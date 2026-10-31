// Environments as values (V98_ENV_PLAN.md E2).
//
// The IR builder's `EnvRegMap` keeps one integer per register and scans the
// instructions in order, so a register it last saw as one environment keeps
// that level after a load overwrites it, and a scope a generator body builds
// after the states that use it is not known when they are scanned. This pass
// tracks what each register holds as a value, over the control-flow graph to
// a fixpoint, and reports facts instead of levels: the environments a function
// creates and what they were built on, the environments it parks in slots, the
// environment each closure is created with, and the one each load and store
// goes through. It does not change the IR; the whole-program pass (E3) turns
// these facts into scopes.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::jump_analysis::{explicit_targets, find_block_starts_with_handlers};
use crate::file::ExceptionHandler;
use crate::opcode::OperandType;
use crate::{BytecodeFile, BytecodeFormat, Instruction};

// What a register holds, as an environment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Env {
    // The environment this function runs in, the first one it creates.
    Own,
    // Its closure environment `n` levels up: 1 is the one it was created with
    // (`GetParentEnvironment 0`), 2 that one's parent, and so on.
    Closure(u32),
    // An environment this function creates past its own, at instruction
    // offset `site`, on `parent` (None: no parent, a top-level environment).
    Created { site: u32, parent: Option<Box<Env>> },
    // `n` levels up from an environment this pass cannot walk any further.
    Up(Box<Env>, u32),
    // Whatever was in slot `slot` of an environment: an environment parked
    // there, if the register is used as one.
    InSlot(Box<Env>, u32),
    // A closure this function just created, and the environment it was given.
    // Not an environment: what `GetClosureEnvironment` reads one from.
    Function { child: u32, env: Box<Env> },
    // The environment of the closure in a register, loaded out of a slot
    // (`GetClosureEnvironment`, where hermesc inlined a call to that closure).
    OfClosure(Box<Env>),
    // `undefined` or the empty value: the initial value of a slot that later
    // holds a closure, which says nothing about which closure.
    NoValue,
    // Paths that disagree, a closure this pass cannot follow, or nesting past
    // the bound.
    Unknown,
}

// Deeper than any real nesting; a loop that walks up a register each time
// round would otherwise never reach a fixpoint.
const MAX_DEPTH: u32 = 32;

impl Env {
    // `self`, `n` levels up.
    fn up(self, n: u32) -> Env {
        if n == 0 {
            return self;
        }
        match self {
            Env::Own => Env::Closure(n),
            Env::Closure(l) if l + n <= MAX_DEPTH => Env::Closure(l + n),
            Env::Created {
                parent: Some(parent),
                ..
            } => parent.up(n - 1),
            Env::Up(inner, m) if m + n <= MAX_DEPTH => Env::Up(inner, m + n),
            Env::InSlot(..) => Env::Up(Box::new(self), n),
            _ => Env::Unknown,
        }
    }

    fn depth(&self) -> u32 {
        match self {
            Env::Own | Env::Closure(_) | Env::NoValue | Env::Unknown => 0,
            Env::Function { env, .. } => 1 + env.depth(),
            Env::OfClosure(inner) => 1 + inner.depth(),
            Env::Created { parent, .. } => parent.as_ref().map_or(0, |p| 1 + p.depth()),
            Env::Up(inner, _) | Env::InSlot(inner, _) => 1 + inner.depth(),
        }
    }

    fn in_slot(self, slot: u32) -> Env {
        if self.depth() >= 4 {
            return Env::Unknown;
        }
        Env::InSlot(Box::new(self), slot)
    }

    // The class of an access, for the census.
    pub fn kind(&self) -> &'static str {
        match self {
            Env::Own => "own",
            Env::Closure(_) => "closure",
            Env::Created { .. } => "created",
            Env::Up(..) => "up from a slot",
            Env::InSlot(..) => "slot-held",
            Env::OfClosure(_) => "a closure's environment",
            Env::Function { .. } | Env::NoValue => "not an environment",
            Env::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Env::Own => write!(f, "own"),
            Env::Closure(n) => write!(f, "closure{n}"),
            Env::Created {
                site,
                parent: Some(p),
            } => write!(f, "created@{site}({p})"),
            Env::Created { site, parent: None } => write!(f, "created@{site}(root)"),
            Env::Up(inner, n) => write!(f, "up{n}({inner})"),
            Env::InSlot(inner, slot) => write!(f, "{inner}[{slot}]"),
            Env::Function { child, env } => write!(f, "F{child}({env})"),
            Env::OfClosure(inner) => write!(f, "env({inner})"),
            Env::NoValue => write!(f, "none"),
            Env::Unknown => write!(f, "?"),
        }
    }
}

// What one function does with environments.
#[derive(Debug, Default, Clone)]
pub struct EnvFacts {
    // Environments created past the function's own: (site, parent).
    pub created: Vec<(u32, Option<Env>)>,
    // An environment value stored into a slot: (through, slot, value).
    pub parked: Vec<(Env, u32, Env)>,
    // The offset of each `parked` store, at the same index.
    pub parked_at: Vec<u32>,
    // (child function, environment it was created with).
    pub closures: Vec<(u32, Env)>,
    // Every store into a slot, for what `GetClosureEnvironment` reads back:
    // (through, slot, the environment of the closure stored), None for a value
    // that is not a closure created here. `undefined` and empty are left out.
    pub slot_closures: Vec<(Env, u32, Option<Env>)>,
    // Loads and stores: (offset, through, slot, is_store).
    pub accesses: Vec<(u32, Env, u32, bool)>,
}

type State = BTreeMap<u32, Env>;

fn join(into: &mut State, from: &State) -> bool {
    let mut changed = false;
    for (reg, env) in from {
        match into.get(reg) {
            None => {
                into.insert(*reg, env.clone());
                changed = true;
            }
            Some(e) if e != env && *e != Env::Unknown => {
                into.insert(*reg, Env::Unknown);
                changed = true;
            }
            _ => {}
        }
    }
    // A register one path does not hold as an environment is not one after
    // the merge; keeping the other path's value is what a linear scan did.
    let gone: Vec<u32> = into
        .keys()
        .filter(|r| !from.contains_key(r))
        .copied()
        .collect();
    for reg in gone {
        if into.get(&reg) != Some(&Env::Unknown) {
            into.insert(reg, Env::Unknown);
            changed = true;
        }
    }
    changed
}

fn reg(inst: &Instruction, i: usize) -> Option<u32> {
    let op = inst.operands.get(i)?;
    matches!(op.ty, OperandType::Reg8 | OperandType::Reg32)
        .then(|| op.value.as_u32())
        .flatten()
}

fn imm(inst: &Instruction, i: usize) -> Option<u32> {
    inst.operands.get(i)?.value.as_u32()
}

// The registers an instruction writes, by Hermes' convention: a leading
// register operand is the destination, except for the families that only
// read it.
fn written(name: &str, inst: &Instruction) -> Vec<u32> {
    let reads_first = name.starts_with("Put")
        || name.starts_with("Store")
        || name.starts_with("Define")
        || name.starts_with("FastArrayPush")
        || name.starts_with("FastArrayStore")
        || name.starts_with("FastArrayAppend")
        || matches!(
            name,
            "Ret"
                | "Throw"
                | "ThrowIfUndefined"
                | "ThrowIfThisInitialized"
                | "IteratorClose"
                | "CacheNewObject"
                | "AddOwnPrivateBySym"
                | "SwitchImm"
                | "UIntSwitchImm"
                | "StringSwitchImm"
        );
    let mut out = Vec::new();
    if !reads_first {
        out.extend(reg(inst, 0));
    }
    // These also advance their second register.
    if matches!(name, "IteratorBegin" | "IteratorNext") {
        out.extend(reg(inst, 1));
    }
    out
}

struct Step<'a> {
    format: &'a BytecodeFormat,
    own_seen: bool,
}

impl Step<'_> {
    fn env_of(state: &State, r: Option<u32>) -> Env {
        r.and_then(|r| state.get(&r).cloned())
            .unwrap_or(Env::Unknown)
    }

    // Apply `inst` to `state`, recording facts when `facts` is given.
    fn apply(&mut self, inst: &Instruction, state: &mut State, facts: Option<&mut EnvFacts>) {
        let Some(def) = self.format.definitions.get(inst.opcode as usize) else {
            return;
        };
        let name = def.name.as_str();
        let mut facts = facts;
        let mut created_closure = None;
        if let Some((child, env_reg)) =
            super::ir_builder::created_function_and_env(self.format, inst)
        {
            let env = Self::env_of(state, Some(env_reg));
            if let Some(f) = facts.as_deref_mut() {
                f.closures.push((child, env.clone()));
            }
            created_closure = Some(Env::Function {
                child,
                env: Box::new(env),
            });
        }
        let value = match name {
            "CreateFunctionEnvironment" | "CreateEnvironment" if inst.operands.len() < 3 => {
                // The first is the function's own (v96's one-operand
                // CreateEnvironment is always that); a later
                // CreateFunctionEnvironment builds a separate scope on the
                // closure environment.
                if !self.own_seen || name == "CreateEnvironment" {
                    self.own_seen = true;
                    Some(Env::Own)
                } else {
                    let parent = Env::Closure(1);
                    if let Some(f) = facts.as_deref_mut() {
                        f.created.push((inst.offset, Some(parent.clone())));
                    }
                    Some(Env::Created {
                        site: inst.offset,
                        parent: Some(Box::new(parent)),
                    })
                }
            }
            "CreateEnvironment" | "CreateInnerEnvironment" => {
                let parent = Self::env_of(state, reg(inst, 1));
                if let Some(f) = facts.as_deref_mut() {
                    f.created.push((inst.offset, Some(parent.clone())));
                }
                Some(Env::Created {
                    site: inst.offset,
                    parent: Some(Box::new(parent)),
                })
            }
            "CreateTopLevelEnvironment" => {
                if let Some(f) = facts.as_deref_mut() {
                    f.created.push((inst.offset, None));
                }
                Some(Env::Created {
                    site: inst.offset,
                    parent: None,
                })
            }
            "GetParentEnvironment" => Some(Env::Closure(imm(inst, 1).unwrap_or(0) + 1)),
            "GetEnvironment" if inst.operands.len() >= 3 => {
                let start = Self::env_of(state, reg(inst, 1));
                Some(start.up(imm(inst, 2).unwrap_or(0)))
            }
            "GetEnvironment" => Some(Env::Closure(imm(inst, 1).unwrap_or(0) + 1)),
            "GetClosureEnvironment" => match Self::env_of(state, reg(inst, 1)) {
                Env::Function { env, .. } => Some(*env),
                held @ Env::InSlot(..) => Some(Env::OfClosure(Box::new(held))),
                _ => Some(Env::Unknown),
            },
            "LoadConstUndefined" | "LoadConstEmpty" => Some(Env::NoValue),
            "LoadFromEnvironment" | "LoadFromEnvironmentL" => {
                let through = Self::env_of(state, reg(inst, 1));
                let slot = imm(inst, 2).unwrap_or(0);
                if let Some(f) = facts.as_deref_mut() {
                    f.accesses.push((inst.offset, through.clone(), slot, false));
                }
                Some(through.in_slot(slot))
            }
            "StoreToEnvironment"
            | "StoreToEnvironmentL"
            | "StoreNPToEnvironment"
            | "StoreNPToEnvironmentL" => {
                let through = Self::env_of(state, reg(inst, 0));
                let slot = imm(inst, 1).unwrap_or(0);
                if let Some(f) = facts {
                    f.accesses.push((inst.offset, through.clone(), slot, true));
                    let held = reg(inst, 2).and_then(|r| state.get(&r));
                    match held {
                        Some(Env::NoValue) => {}
                        Some(Env::Function { env, .. }) => {
                            f.slot_closures
                                .push((through.clone(), slot, Some((**env).clone())));
                        }
                        _ => f.slot_closures.push((through.clone(), slot, None)),
                    }
                    if let Some(v) = held {
                        if !matches!(
                            v,
                            Env::InSlot(..)
                                | Env::Unknown
                                | Env::Function { .. }
                                | Env::OfClosure(_)
                                | Env::NoValue
                        ) {
                            f.parked.push((through, slot, v.clone()));
                            f.parked_at.push(inst.offset);
                        }
                    }
                }
                None
            }
            "Mov" | "MovLong" => {
                let v = reg(inst, 1).and_then(|r| state.get(&r).cloned());
                if let Some(dst) = reg(inst, 0) {
                    match v {
                        Some(v) => {
                            state.insert(dst, v);
                        }
                        None => {
                            state.remove(&dst);
                        }
                    }
                }
                return;
            }
            _ => created_closure,
        };
        let writes = written(name, inst);
        for r in &writes {
            state.remove(r);
        }
        if let (Some(v), Some(&dst)) = (value, writes.first()) {
            state.insert(dst, v);
        }
    }
}

// The facts for one function's instructions.
pub fn analyze(
    insts: &[Instruction],
    format: &BytecodeFormat,
    file: &BytecodeFile,
    handlers: &[ExceptionHandler],
    func_bytecode_offset: u32,
) -> EnvFacts {
    let mut facts = EnvFacts::default();
    if insts.is_empty() {
        return facts;
    }
    // The builder's block starts, plus the instruction after every jump or
    // switch: the builder's list leaves out the fall-through of some
    // conditional jumps (`JStrictEqualLong`), which it splits while emitting.
    let mut starts =
        find_block_starts_with_handlers(insts, format, file, handlers, func_bytecode_offset);
    for (i, inst) in insts.iter().enumerate() {
        if i + 1 < insts.len()
            && !explicit_targets(inst, format, file, func_bytecode_offset).is_empty()
        {
            starts.insert(insts[i + 1].offset);
        }
    }
    let starts: Vec<u32> = starts.into_iter().collect();
    let index_of: BTreeMap<u32, usize> = insts
        .iter()
        .enumerate()
        .map(|(i, x)| (x.offset, i))
        .collect();
    // Blocks as [first, last] instruction indices.
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    for (k, &start) in starts.iter().enumerate() {
        let Some(&first) = index_of.get(&start) else {
            continue;
        };
        let last = starts
            .get(k + 1)
            .and_then(|next| index_of.get(next))
            .map_or(insts.len() - 1, |&n| n.saturating_sub(1));
        blocks.push((first, last));
    }
    let block_at: BTreeMap<usize, usize> = blocks
        .iter()
        .enumerate()
        .map(|(b, &(first, _))| (first, b))
        .collect();
    let block_of_offset = |off: u32| index_of.get(&off).and_then(|i| block_at.get(i)).copied();

    let mut succs: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); blocks.len()];
    for (b, &(_, last)) in blocks.iter().enumerate() {
        let inst = &insts[last];
        let name = format
            .definitions
            .get(inst.opcode as usize)
            .map_or("", |d| d.name.as_str());
        for t in explicit_targets(inst, format, file, func_bytecode_offset) {
            succs[b].extend(block_of_offset(t));
        }
        let falls = !matches!(
            name,
            "Ret" | "Throw" | "Jmp" | "JmpLong" | "SwitchImm" | "UIntSwitchImm" | "StringSwitchImm"
        );
        if falls && last + 1 < insts.len() {
            succs[b].extend(block_at.get(&(last + 1)).copied());
        }
        // Into a handler from anywhere in its try range.
        for h in handlers {
            let off = insts[blocks[b].0].offset;
            if off >= h.start && off < h.end {
                succs[b].extend(block_of_offset(h.target));
            }
        }
    }

    // Which CreateFunctionEnvironment is the function's own is a property of
    // the instruction, not of the path: the first in layout order, as the
    // builder decides.
    let own_site = insts.iter().find_map(|i| {
        let name = format.definitions.get(i.opcode as usize)?.name.as_str();
        (name == "CreateFunctionEnvironment"
            || (name == "CreateEnvironment" && i.operands.len() < 3))
            .then_some(i.offset)
    });
    let mut step = Step {
        format,
        own_seen: false,
    };
    let mut run = |b: usize, state: &mut State, facts: Option<&mut EnvFacts>| {
        let (first, last) = blocks[b];
        let mut facts = facts;
        for inst in &insts[first..=last] {
            step.own_seen = Some(inst.offset) != own_site;
            step.apply(inst, state, facts.as_deref_mut());
        }
    };

    let mut input: Vec<Option<State>> = vec![None; blocks.len()];
    if !blocks.is_empty() {
        input[0] = Some(State::new());
    }
    let mut work: VecDeque<usize> = VecDeque::from([0]);
    let mut queued: BTreeSet<usize> = BTreeSet::from([0]);
    while let Some(b) = work.pop_front() {
        queued.remove(&b);
        let mut state = input[b].clone().unwrap_or_default();
        let entry = state.clone();
        run(b, &mut state, None);
        for &s in &succs[b] {
            // A handler is entered from the middle of the block too.
            let from = if handlers
                .iter()
                .any(|h| block_of_offset(h.target) == Some(s))
            {
                let mut both = entry.clone();
                join(&mut both, &state);
                both
            } else {
                state.clone()
            };
            let changed = match &mut input[s] {
                Some(existing) => join(existing, &from),
                slot @ None => {
                    *slot = Some(from);
                    true
                }
            };
            if changed && queued.insert(s) {
                work.push_back(s);
            }
        }
    }
    for (b, entry) in input.into_iter().enumerate() {
        if let Some(mut state) = entry {
            run(b, &mut state, Some(&mut facts));
        }
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walking_up_normalises() {
        assert_eq!(Env::Own.up(2), Env::Closure(2));
        assert_eq!(Env::Closure(1).up(1), Env::Closure(2));
        let created = Env::Created {
            site: 4,
            parent: Some(Box::new(Env::Closure(2))),
        };
        assert_eq!(created.clone().up(1), Env::Closure(2));
        assert_eq!(created.clone().up(2), Env::Closure(3));
        let held = Env::Closure(1).in_slot(1);
        assert_eq!(held.clone().up(1), Env::Up(Box::new(held.clone()), 1));
        assert_eq!(held.clone().up(1).up(2), Env::Up(Box::new(held), 3));
        assert_eq!(Env::Closure(MAX_DEPTH).up(1), Env::Unknown);
    }

    #[test]
    fn paths_that_disagree_merge_to_unknown() {
        let mut a = State::from([(1, Env::Own), (2, Env::Closure(1))]);
        let b = State::from([(1, Env::Own), (2, Env::Closure(2)), (3, Env::Own)]);
        assert!(join(&mut a, &b));
        assert_eq!(a[&1], Env::Own);
        assert_eq!(a[&2], Env::Unknown);
        assert_eq!(a[&3], Env::Own);
        assert!(!join(&mut a, &b));
    }
}

#[cfg(test)]
mod fixture_tests {
    use super::Env;

    // tests/fixtures/env/generator_states.js at v98. The generator body (3)
    // builds its scope after the states that use it, on its level 2, parks it
    // in its wrapper's slot 1 (and the level 2 environment in slot 2), and
    // creates both closures with whatever slot 1 holds. All of that is in the
    // facts, whatever the layout order.
    #[test]
    fn a_generator_body_parks_its_scope_and_creates_closures_with_it() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/env/generator_states.v98.hbc"
        ))
        .unwrap();
        let file = crate::BytecodeFile::parse_auto(&bytes).unwrap();
        let format = crate::BytecodeFormat::for_version(file.header.version).unwrap();
        let insts = file.decode_function_instructions(&format, 3).unwrap();
        let off = file.function_headers[3].offset() - file.instruction_offset;
        let facts = super::analyze(&insts, &format, &file, &[], off);

        let held = Env::Closure(1).in_slot(1);
        let mut children: Vec<u32> = facts
            .closures
            .iter()
            .filter(|(_, env)| *env == held)
            .map(|(child, _)| *child)
            .collect();
        children.sort();
        assert_eq!(children, [4, 5], "{:?}", facts.closures);

        let [(site, Some(Env::Closure(2)))] = facts.created[..] else {
            panic!("one scope built on level 2: {:?}", facts.created);
        };
        let scope = Env::Created {
            site,
            parent: Some(Box::new(Env::Closure(2))),
        };
        assert!(
            facts.parked.contains(&(Env::Closure(1), 1, scope)),
            "{:?}",
            facts.parked
        );
        assert!(facts
            .parked
            .contains(&(Env::Closure(1), 2, Env::Closure(2))));
    }
}

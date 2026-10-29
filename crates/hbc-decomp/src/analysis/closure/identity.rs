// The slot identity checker (K4.3): the invariants a stamped IR makes checkable.
//
// - I1: every reference stamped with one `SlotRef` has one name, in every body.
// - I2: within one body, one name never means two things: two slots, or a slot
//   an ancestor owns and a binding of the body's own (a local, a parameter). Nor
//   may a scope between a capture and the slot's owner bind the capture's name,
//   which would shadow it.
// - I3: a function declares the slots it owns that its body uses, never a slot
//   an ancestor owns, and nothing nobody uses (a bare `let x;` no reference in
//   the body or below reads).
//
// It runs on bodies as codegen renders them -- parameters named, declarations
// inserted, every statement where it will print -- because I2 is a property of
// rendered scopes: a check on earlier IR would report clean while the output
// collides. Each violation is a C9/C10-class defect in the output, counted
// instead of guessed.
//
// An own slot spelled like the owner's local or parameter is counted apart, as
// an alias, not as I2: the slot is named after the value stored into it, and
// the two print as one variable that holds one value. Whether that stays true
// after the owner reassigns its parameter is exactly C10's `resolveThenable`,
// which shows up as an I2 shadowing violation in the reader instead.

use super::stamps::{own_scopes, var_refs};
use super::ClosureContext;
use crate::ir::{Constant, Expression, SlotRef, Statement};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

// One function's body as it renders: its parameter names and its statements.
pub struct RenderedBody<'a> {
    pub params: &'a [String],
    pub stmts: &'a [Statement],
    // A module factory, rendered as the module's top level. Codegen binds what
    // the module writes and never declares (an import for `x = require(n)`, a
    // `let` for the rest), so a slot the module or its functions write is
    // declared whatever the IR says.
    pub module: bool,
    // A copy of a shared body made for one creator (shared_bodies.rs): the
    // function it copies, whose scopes it has, and the body it is created in.
    pub copy_of: Option<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I1 {
    pub slot: SlotRef,
    // Each name the slot carries, with the functions that use it.
    pub names: BTreeMap<String, BTreeSet<u32>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum I2Kind {
    // Two slots under one name in one body.
    TwoSlots,
    // An ancestor's slot and the body's own local under one name.
    CaptureAndLocal,
    // An ancestor's slot and one of the body's parameters.
    CaptureAndParam,
    // A scope between the body and the slot's owner binds the name.
    Shadowed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I2 {
    pub func: u32,
    pub name: String,
    pub kind: I2Kind,
    pub slot: SlotRef,
    // For `TwoSlots` the other slot; for `Shadowed` the scope that binds it.
    pub other: Option<SlotRef>,
    pub at: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum I3Kind {
    // An own slot the body uses that nothing in the body declares.
    Undeclared,
    // A bare `let x;` that neither the body nor any function below reads.
    Unused,
    // A `let` of a slot an ancestor owns: the write lands in a fresh local and
    // never reaches the owner or its other readers.
    DeclaresCapture,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I3 {
    pub func: u32,
    pub name: String,
    pub kind: I3Kind,
}

#[derive(Debug, Default, Clone)]
pub struct Violations {
    pub i1: Vec<I1>,
    pub i2: Vec<I2>,
    pub i3: Vec<I3>,
    // Own slots spelled like a local or parameter of the owner (not violations).
    pub aliases: usize,
}

impl Violations {
    pub fn counts(&self) -> (usize, usize, usize) {
        (self.i1.len(), self.i2.len(), self.i3.len())
    }
}

// What a body binds by name: its parameters, its declarations, and the
// functions declared in it.
fn bindings(ctx: &ClosureContext, func: u32, body: &RenderedBody) -> HashMap<String, Binds> {
    let mut out: HashMap<String, Binds> = HashMap::new();
    for p in body.params {
        out.entry(p.clone()).or_default().param = true;
    }
    for s in body.stmts {
        collect_lets(s, &mut |name, slot| {
            let b = out.entry(name.to_string()).or_default();
            match slot {
                Some(slot) => {
                    b.slots.insert(slot);
                }
                None => b.local = true,
            }
        });
    }
    let func = body.copy_of.map_or(func, |(origin, _)| origin);
    let own = own_scopes(ctx, func);
    let expressions = function_expression_names(body.stmts, &own);
    for name in ctx.declared_function_names(func) {
        // Only a declaration binds a function's name in the enclosing body;
        // an expression's name is visible inside it alone. Babel's
        // self-replacing helpers assign named expressions to the module's
        // variable (`_getRequireWildcardCache = function
        // _getRequireWildcardCache(...)`) or to a local of another name
        // (`_isNativeReflectConstruct2 = function _isNativeReflectConstruct`),
        // which counted as a local beside the capture (IDENTITY_PLAN.md A0).
        if expressions.contains(&name) {
            continue;
        }
        out.entry(name).or_default().local = true;
    }
    out
}

// Names of functions `stmts` creates only as expressions: the value of an
// assignment or binding to a variable of another name, or to an enclosing
// scope's slot of the same name. A function bound to a variable of its own
// name in this body is the declaration shape and is left out.
fn function_expression_names(stmts: &[Statement], own: &HashSet<u32>) -> HashSet<String> {
    use crate::ir::{AssignTarget, Binding, Visitor};
    struct V<'a> {
        own: &'a HashSet<u32>,
        expr: HashSet<String>,
        decl: HashSet<String>,
    }
    impl V<'_> {
        fn note(&mut self, target: &crate::ir::Var, f: &str) {
            let outer = target.slot.is_some_and(|s| !self.own.contains(&s.owner));
            if target.name == f && !outer {
                self.decl.insert(f.to_string());
            } else {
                self.expr.insert(f.to_string());
            }
        }
    }
    impl<'a> Visitor<'a> for V<'_> {
        fn visit_statement(&mut self, s: &'a Statement) {
            match s {
                Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable(v)),
                    value: Expression::Function { name: Some(f), .. },
                } => self.note(v, f),
                Statement::Let {
                    name: v,
                    value: Expression::Function { name: Some(f), .. },
                    ..
                } => self.note(v, f),
                _ => {}
            }
            self.walk_statement(s);
        }
    }
    let mut v = V {
        own,
        expr: HashSet::new(),
        decl: HashSet::new(),
    };
    for s in stmts {
        v.visit_statement(s);
    }
    v.expr.retain(|n| !v.decl.contains(n));
    v.expr
}

#[derive(Debug, Default)]
struct Binds {
    param: bool,
    local: bool,
    slots: HashSet<SlotRef>,
}

// Every `let` in a body, nested blocks included, with its slot.
fn collect_lets<'a>(s: &'a Statement, f: &mut impl FnMut(&'a str, Option<SlotRef>)) {
    use crate::ir::Visitor;
    struct Lets<'f, F>(&'f mut F);
    impl<'a, F: FnMut(&'a str, Option<SlotRef>)> Visitor<'a> for Lets<'_, F> {
        fn visit_statement(&mut self, s: &'a Statement) {
            if let Statement::Let { name, .. } = s {
                (self.0)(&name.name, name.slot);
            }
            self.walk_statement(s);
        }
    }
    Lets(f).visit_statement(s);
}

// Whether `stmts` assign `name` anywhere.
fn writes(stmts: &[Statement], name: &str) -> bool {
    use crate::ir::{AssignTarget, Binding, Visitor};
    struct W<'n>(&'n str, bool);
    impl<'a> Visitor<'a> for W<'_> {
        fn visit_assign_target(&mut self, t: &'a AssignTarget) {
            if let AssignTarget::Binding(Binding::Variable(v)) = t {
                self.1 |= v.name == self.0;
            }
            self.walk_assign_target(t);
        }
    }
    let mut w = W(name, false);
    for s in stmts {
        w.visit_statement(s);
        if w.1 {
            return true;
        }
    }
    false
}

fn is_bare_let(s: &Statement) -> Option<&str> {
    match s {
        Statement::Let {
            name,
            value: Expression::Value(crate::ir::Value::Constant(Constant::Undefined)),
            ..
        } => Some(&name.name),
        _ => None,
    }
}

pub fn check_slot_identity(
    bodies: &BTreeMap<u32, RenderedBody>,
    ctx: &ClosureContext,
) -> Violations {
    let mut v = Violations::default();
    // A copy has the scopes of the function it copies, and sits in the body
    // that creates it.
    let origin = |f: u32| bodies.get(&f).and_then(|b| b.copy_of).map_or(f, |(o, _)| o);
    let parent_of = |f: u32| -> Option<u32> {
        match bodies.get(&f).and_then(|b| b.copy_of) {
            Some((_, p)) => Some(p),
            None => ctx.parent_function.get(&f).copied(),
        }
    };

    // Per body: every name referenced, with the slots it is stamped with and
    // whether it is also referenced unstamped.
    struct Uses {
        slots: BTreeSet<SlotRef>,
        unstamped: bool,
    }
    let mut uses: BTreeMap<u32, BTreeMap<String, Uses>> = BTreeMap::new();
    let mut slot_names: BTreeMap<SlotRef, BTreeMap<String, BTreeSet<u32>>> = BTreeMap::new();
    for (&func, body) in bodies {
        let mut per: BTreeMap<String, Uses> = BTreeMap::new();
        for s in body.stmts {
            for (name, slot) in var_refs(std::slice::from_ref(s)) {
                let u = per.entry(name.to_string()).or_insert(Uses {
                    slots: BTreeSet::new(),
                    unstamped: false,
                });
                match slot {
                    Some(slot) => {
                        u.slots.insert(slot);
                        slot_names
                            .entry(slot)
                            .or_default()
                            .entry(name.to_string())
                            .or_default()
                            .insert(func);
                    }
                    None => u.unstamped = true,
                }
            }
        }
        uses.insert(func, per);
    }

    // I1.
    for (slot, names) in slot_names {
        if names.len() > 1 {
            v.i1.push(I1 { slot, names });
        }
    }

    let binds: BTreeMap<u32, HashMap<String, Binds>> = bodies
        .iter()
        .map(|(&f, b)| (f, bindings(ctx, f, b)))
        .collect();

    // I2.
    for (&func, per) in &uses {
        let own = own_scopes(ctx, origin(func));
        let my = &binds[&func];
        for (name, u) in per {
            let mut slots = u.slots.iter();
            let Some(&first) = slots.next() else { continue };
            for &other in slots {
                v.i2.push(I2 {
                    func,
                    name: name.clone(),
                    kind: I2Kind::TwoSlots,
                    slot: first,
                    other: Some(other),
                    at: None,
                });
            }
            for &slot in &u.slots {
                if own.contains(&slot.owner) {
                    let b = my.get(name.as_str());
                    if u.unstamped || b.is_some_and(|b| b.param) {
                        v.aliases += 1;
                    }
                    continue;
                }
                let b = my.get(name.as_str());
                if b.is_some_and(|b| b.param) {
                    v.i2.push(I2 {
                        func,
                        name: name.clone(),
                        kind: I2Kind::CaptureAndParam,
                        slot,
                        other: None,
                        at: None,
                    });
                } else if u.unstamped || b.is_some_and(|b| b.local) {
                    v.i2.push(I2 {
                        func,
                        name: name.clone(),
                        kind: I2Kind::CaptureAndLocal,
                        slot,
                        other: None,
                        at: None,
                    });
                }
                // The scopes between the body and the owner.
                let mut cur = parent_of(func);
                let mut seen = HashSet::from([func]);
                while let Some(scope) = cur {
                    // A block's slot is bound in the function that builds
                    // the block: the walk ends there, not past it.
                    if scope == slot.owner
                        || scope == ctx.scope_function(slot.owner)
                        || !seen.insert(scope)
                    {
                        break;
                    }
                    let shadows = binds
                        .get(&scope)
                        .and_then(|b| b.get(name.as_str()))
                        .is_some_and(|b| b.param || b.local || b.slots.iter().any(|s| *s != slot));
                    if shadows {
                        v.i2.push(I2 {
                            func,
                            name: name.clone(),
                            kind: I2Kind::Shadowed,
                            slot,
                            other: None,
                            at: Some(scope),
                        });
                        break;
                    }
                    cur = parent_of(scope);
                }
            }
        }
    }

    // I3.
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&child, &parent) in &ctx.parent_function {
        children
            .entry(ctx.scope_function(parent))
            .or_default()
            .push(child);
    }
    // Every creator of a shared function renders it (or its copy) below.
    for (&f, creators) in &ctx.shared_creations {
        for &g in creators.keys() {
            children.entry(g).or_default().push(f);
        }
    }
    for (&f, b) in bodies {
        if let Some((_, p)) = b.copy_of {
            children.entry(p).or_default().push(f);
            // A nested function the copy did not need to copy keeps its id,
            // and its parent link still names the original.
            children.entry(f).or_default().extend(created_ids(b.stmts));
        }
    }
    // Whether a function below `func` reads `name` as `func`'s binding: a
    // reference stamped with another scope's slot is a different variable that
    // merely shares the spelling, so it does not keep the declaration alive.
    let read_below = |func: u32, name: &str| -> bool {
        let own = own_scopes(ctx, origin(func));
        let mut stack: Vec<u32> = children.get(&func).cloned().unwrap_or_default();
        let mut seen = HashSet::new();
        while let Some(f) = stack.pop() {
            if !seen.insert(f) {
                continue;
            }
            let reads = uses
                .get(&f)
                .and_then(|per| per.get(name))
                .is_some_and(|u| u.unstamped || u.slots.iter().any(|s| own.contains(&s.owner)));
            if reads {
                return true;
            }
            if let Some(c) = children.get(&f) {
                stack.extend(c.iter().copied());
            }
        }
        false
    };
    // Whether the module rooted at `func` writes `name`, in its body or below.
    let module_writes = |func: u32, name: &str| -> bool {
        let mut stack = vec![func];
        let mut seen = HashSet::new();
        while let Some(f) = stack.pop() {
            if !seen.insert(f) {
                continue;
            }
            if bodies.get(&f).is_some_and(|b| writes(b.stmts, name)) {
                return true;
            }
            if let Some(c) = children.get(&f) {
                stack.extend(c.iter().copied());
            }
        }
        false
    };
    for (&func, body) in bodies {
        let own = own_scopes(ctx, origin(func));
        let my = &binds[&func];
        let per = &uses[&func];
        for (name, u) in per {
            let uses_own = u.slots.iter().any(|s| own.contains(&s.owner));
            if uses_own
                && my.get(name.as_str()).is_none()
                && !(body.module && module_writes(func, name))
            {
                v.i3.push(I3 {
                    func,
                    name: name.clone(),
                    kind: I3Kind::Undeclared,
                });
            }
        }
        // Only an ancestor's slot is a capture: statements inlined from another
        // function keep its stamps, and its slot is a local here.
        let mut ancestors: HashSet<u32> = HashSet::new();
        let mut cur = parent_of(func);
        while let Some(p) = cur {
            if !ancestors.insert(origin(p)) {
                break;
            }
            cur = parent_of(p);
        }
        for s in body.stmts {
            collect_lets(s, &mut |name, slot| {
                if slot.is_some_and(|s| !own.contains(&s.owner) && ancestors.contains(&s.owner)) {
                    v.i3.push(I3 {
                        func,
                        name: name.to_string(),
                        kind: I3Kind::DeclaresCapture,
                    });
                }
            });
        }
        let mut refs: Option<HashMap<&str, usize>> = None;
        for s in body.stmts {
            if let Some(name) = is_bare_let(s) {
                let refs = refs.get_or_insert_with(|| {
                    let mut m = HashMap::new();
                    for (n, _) in var_refs(body.stmts) {
                        *m.entry(n).or_insert(0) += 1;
                    }
                    m
                });
                // The declaration itself is the one reference a bare `let` has.
                let used_here = refs.get(name).copied().unwrap_or(0) > 1;
                if !used_here && !read_below(func, name) {
                    v.i3.push(I3 {
                        func,
                        name: name.to_string(),
                        kind: I3Kind::Unused,
                    });
                }
            }
        }
    }
    v
}

// The function ids `stmts` creates.
fn created_ids(stmts: &[Statement]) -> Vec<u32> {
    use crate::ir::Visitor;
    struct Ids(Vec<u32>);
    impl<'a> Visitor<'a> for Ids {
        fn visit_expression(&mut self, e: &'a Expression) {
            if let Expression::Function { id, .. } = e {
                self.0.push(id.0);
            }
            self.walk_expression(e);
        }
    }
    let mut ids = Ids(Vec::new());
    for s in stmts {
        ids.visit_statement(s);
    }
    ids.0
}

// A one-line summary and, at trace level, the first violations of each kind
// with the function names, for the log.
pub fn log_violations(stage: &str, v: &Violations, ctx: &ClosureContext) {
    use super::stamps::TARGET;
    let mut i2 = BTreeMap::new();
    for x in &v.i2 {
        *i2.entry(x.kind).or_insert(0usize) += 1;
    }
    let mut i3 = BTreeMap::new();
    for x in &v.i3 {
        *i3.entry(x.kind).or_insert(0usize) += 1;
    }
    log::debug!(
        target: TARGET,
        "identity {stage}: I1 {}  I2 {} {i2:?}  I3 {} {i3:?}  aliases {}  slot-held env accesses {}",
        v.i1.len(),
        v.i2.len(),
        v.i3.len(),
        v.aliases,
        ctx.slot_held_accesses
    );
    log::debug!(target: TARGET, "env accesses by kind: {:?}", ctx.env_access_kinds);
    log::debug!(target: TARGET, "scope graph: {:?}", ctx.scope_graph_stats);
    if let Ok(path) = std::env::var("HD_SLOTS_DUMP") {
        let path = format!("{path}.{}.tsv", stage.replace(' ', "_"));
        if let Err(e) = std::fs::write(&path, v.to_tsv(ctx)) {
            log::warn!(target: TARGET, "cannot write {path}: {e}");
        }
    }
    if !log::log_enabled!(target: TARGET, log::Level::Trace) {
        return;
    }
    let fname = |f: u32| ctx.get_function_name(f).unwrap_or("?").to_string();
    for x in v.i1.iter().take(10) {
        log::trace!(target: TARGET, "  I1 {:?} {:?}", x.slot, x.names);
    }
    let mut shown: BTreeMap<I2Kind, usize> = BTreeMap::new();
    for x in &v.i2 {
        let n = shown.entry(x.kind).or_default();
        *n += 1;
        if *n > 8 {
            continue;
        }
        log::trace!(
            target: TARGET,
            "  I2 {:?} F{} {} `{}` slot {:?} other {:?} at {:?}",
            x.kind,
            x.func,
            fname(x.func),
            x.name,
            x.slot,
            x.other,
            x.at.map(|a| format!("F{a} {}", fname(a)))
        );
    }
    let mut shown: BTreeMap<I3Kind, usize> = BTreeMap::new();
    for x in &v.i3 {
        let n = shown.entry(x.kind).or_default();
        *n += 1;
        if *n > 8 {
            continue;
        }
        log::trace!(target: TARGET, "  I3 {:?} F{} {} `{}`", x.kind, x.func, fname(x.func), x.name);
    }
}

// The check at a pipeline stage before codegen, over the IR as it stands. No
// parameter is named and no declaration inserted yet, so I3 means nothing here
// and is not logged; I1 and I2 say how much naming left for later stages.
pub fn report_ir(
    stage: &str,
    all_ir: &BTreeMap<u32, Vec<Statement>>,
    ctx: Option<&ClosureContext>,
) {
    if !super::stamps::enabled() {
        return;
    }
    let Some(ctx) = ctx else { return };
    let bodies = all_ir
        .iter()
        .map(|(&f, stmts)| {
            (
                f,
                RenderedBody {
                    params: &[],
                    stmts,
                    module: false,
                    copy_of: None,
                },
            )
        })
        .collect();
    let mut v = check_slot_identity(&bodies, ctx);
    v.i3.clear();
    log_violations(stage, &v, ctx);
}

impl Violations {
    // Every violation, one per line, tab separated: kind, function id, function
    // name, variable name, slot, and what it collides with.
    pub fn to_tsv(&self, ctx: &ClosureContext) -> String {
        use std::fmt::Write;
        let fname = |f: u32| ctx.get_function_name(f).unwrap_or("?").to_string();
        let slot = |s: &SlotRef| format!("{}:{}", s.owner, s.slot);
        let mut out = String::new();
        for x in &self.i1 {
            for (name, funcs) in &x.names {
                for &f in funcs {
                    let _ = writeln!(out, "I1	{f}	{}	{name}	{}	", fname(f), slot(&x.slot));
                }
            }
        }
        for x in &self.i2 {
            let with = match (x.other, x.at) {
                (Some(o), _) => slot(&o),
                (None, Some(a)) => format!("F{a} {}", fname(a)),
                _ => String::new(),
            };
            let _ = writeln!(
                out,
                "I2 {:?}	{}	{}	{}	{}	{with}",
                x.kind,
                x.func,
                fname(x.func),
                x.name,
                slot(&x.slot)
            );
        }
        for x in &self.i3 {
            let _ = writeln!(
                out,
                "I3 {:?}	{}	{}	{}		",
                x.kind,
                x.func,
                fname(x.func),
                x.name
            );
        }
        out
    }
}

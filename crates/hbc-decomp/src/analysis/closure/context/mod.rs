// Global closure context for cross-function resolution.
// Tracks parent-child relationships and environment slot assignments across all functions.

mod analyze;
mod async_prop;
mod helpers;
mod merge;
mod walk;

use super::info::{ClosureInfo, ClosureSlotValue, NameSource};
use std::collections::{BTreeMap, BTreeSet, HashSet};

// The site of a block no creation made: the environment of a closure loaded out
// of a slot (`GetClosureEnvironment`), at `NESTED_ENV_LEVEL_BASE + slot`. Kept
// as a block of its own, keyed by this bit and the level, until A2c3 places a
// closure's environment as the scope it was created in. No offset reaches it.
pub const CLOSURE_ENV_SITE: u32 = 1 << 31;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ClosureContext {
    pub parent_function: BTreeMap<u32, u32>,
    pub function_closures: BTreeMap<u32, ClosureInfo>,
    pub function_names: BTreeMap<u32, String>,
    // Set of function IDs that are async (created with CreateAsyncClosure)
    pub async_functions: HashSet<u32>,
    // Set of function IDs that are generators (created with CreateGeneratorClosure)
    pub generator_functions: HashSet<u32>,
    // A block environment a function built on top of its own (a class
    // binding's scope) is a scope of its own between that function and the
    // closures created inside the block. It is given a synthetic id so it
    // takes part in the parent chain like a function would. Keyed by
    // (creating function, creation site), the key the scope graph uses
    // (`Scope::Created`); value: the synthetic id (IDENTITY_PLAN.md A2c).
    // Built on first use: a store, a creation, or a recorded access; numbered
    // again by every rescan (`reanalyze_all`).
    #[serde(default)]
    pub block_scopes: BTreeMap<(u32, u32), u32>,
    // synthetic id -> the builder level its slots are addressed with inside
    // the creating function.
    #[serde(default)]
    pub block_level: BTreeMap<u32, u32>,
    // synthetic id -> its key in `block_scopes`.
    #[serde(default)]
    pub block_key: BTreeMap<u32, (u32, u32)>,
    // (function, builder level) -> the block scope that body addresses at
    // that level: the blocks it built, and those of a generator step whose
    // body it took over (`rehome_block_scopes`). The level is only the
    // builder's handle; the block is the scope.
    #[serde(default)]
    pub block_index: BTreeMap<(u32, u32), u32>,
    // (creating function, creation site) -> its builder level, for every site
    // the builder saw.
    #[serde(default)]
    pub site_levels: BTreeMap<(u32, u32), u32>,
    // synthetic id -> the function that built the block environment. Its
    // parent is usually that function, but not always: a block built on an
    // outer environment (V98_ENV_PLAN.md S2, a second
    // `CreateFunctionEnvironment`) has that outer scope as its parent.
    #[serde(default)]
    pub block_function: BTreeMap<u32, u32>,
    // function -> (baked capture name -> (level, slot) it came from). A
    // capture whose ancestor slot had no name when it was baked prints as
    // `closure_N`, which says nothing about the level; the late inherit pass
    // resolves it from this record once the owner has named the slot.
    #[serde(default)]
    pub baked_captures: BTreeMap<u32, BTreeMap<String, (u32, u32)>>,
    // function -> (baked name -> the scope W6 stamped it with), IDENTITY_PLAN.md
    // A1b3: the owner, recorded, where `slot_owner` would walk the chain from
    // the level again.
    #[serde(default)]
    pub baked_owners: BTreeMap<u32, BTreeMap<String, u32>>,
    // child -> (creating function, builder level of the block environment it
    // was created with, and its creation site when the builder knew it). Such
    // a child's parent is that block scope: by site, since past the level cap
    // many blocks share one level.
    #[serde(default)]
    pub creation_scopes: BTreeMap<u32, (u32, u32, Option<u32>)>,
    // function -> how many hops past its creator the environment it was created
    // with sits; absent means 0, the creator's own. Its IR levels are already
    // shifted by this (`ir::shift_env_levels`), so level L is always L hops up
    // the parent chain; kept so a function built later is shifted the same way.
    #[serde(default)]
    pub env_offsets: BTreeMap<u32, u32>,
    // Loads and stores, over the whole bundle, that went through an
    // environment loaded out of a slot: identified by slot number alone, so
    // not resolved to a scope (V98_ENV_PLAN.md S3). 0 on v96.
    #[serde(default)]
    pub slot_held_accesses: u32,
    // Loads and stores by the kind of environment they go through, as the
    // environment-value pass sees them (`ir::builder::env_values::Env::kind`).
    #[serde(default)]
    pub env_access_kinds: BTreeMap<String, u32>,
    // Loads and stores by what the scope graph resolves them to, against the
    // level model (V98_ENV_PLAN.md E3).
    #[serde(default)]
    pub scope_graph_stats: BTreeMap<String, u32>,
    // The scope graph's levels, per function whose IR they change
    // (V98_ENV_PLAN.md E4); such a function is built with its plan and not
    // shifted, so it has no `env_offsets` entry.
    #[serde(default)]
    pub env_plans: BTreeMap<u32, crate::ir::EnvPlan>,
    // The scope the graph says a function was created with: (function, None)
    // for a function's own environment, (function, Some(creation site)) for a
    // block environment it built. Where present it is the parent, whatever
    // the creating closure's environment register said. The graph's single
    // answers only (IDENTITY_PLAN.md A2d).
    #[serde(default)]
    pub scope_parents: BTreeMap<u32, (u32, Option<u32>)>,
    // A shared function's place in the tree: its first creation scope. The
    // graph has no single answer for it, and no access depends on this one
    // (each is placed by the scope it records), but declaration placement and
    // the shadow checks need one parent per function: without it, the parent
    // was whichever creator the walk saw last, often without its block
    // (Discord's I2 Shadowed 1,059 -> 2,150). A function that creates itself
    // has none (ts-pattern's `optional`: one parent moved every creator's
    // declaration to one scope). Goes when declarations are placed by
    // identity (A4d).
    #[serde(default)]
    pub shared_parents: BTreeMap<u32, (u32, Option<u32>)>,
    // The same for a block environment: (creating function, creation site) ->
    // the scope it was built on.
    #[serde(default)]
    pub block_parents: BTreeMap<(u32, u32), (u32, Option<u32>)>,
    // Functions created on a block environment at some site: each site has
    // its own environment, live at once (copies of an inlined function, a
    // `let` per iteration), so the closure reads that block's bindings and
    // cannot be declared once outside it (`hoist_repeated_closures`).
    #[serde(default)]
    pub block_created: BTreeSet<u32>,
    // A function the graph gives several creation scopes, by the function
    // whose body creates it and the scopes it is created on there (S3.3).
    // Its one body is rendered at each creation site and must read that
    // scope's names for the slots (`PipelineContext::shared_body_variants`).
    #[serde(default)]
    pub shared_creations: BTreeMap<u32, BTreeMap<u32, BTreeSet<ScopeRef>>>,
    // S3.2b: block scopes a raw machine builds on its first step and parks in
    // its wrapper (`keep_raw_machines`). They live as long as the generator,
    // so they are the wrapper's (`block_function`), and their slots are its
    // variables: declared there, captures everywhere below it, never declared
    // where the block is built (in the step, they restarted every call).
    #[serde(default)]
    pub parked_scopes: BTreeSet<u32>,
    // A1a1 (IDENTITY_PLAN.md): the scope each access the scope graph places
    // reads or writes, by function and bytecode offset. A `ScopeRef`, not an
    // id: block-scope ids are handed out again on every reanalysis.
    #[serde(default)]
    pub access_scopes: BTreeMap<u32, BTreeMap<u32, ScopeRef>>,
}

// Owners `baked_owner` gave, over the run: [walked (nothing recorded),
// recorded agreeing with the walk, recorded disagreeing].
static BAKED_OWNERS: [std::sync::atomic::AtomicU32; 3] = [
    std::sync::atomic::AtomicU32::new(0),
    std::sync::atomic::AtomicU32::new(0),
    std::sync::atomic::AtomicU32::new(0),
];
use std::sync::atomic::Ordering::Relaxed;

pub fn baked_owner_counts() -> [u32; 3] {
    [0, 1, 2].map(|i| BAKED_OWNERS[i].load(Relaxed))
}

// A scope as the context names it: (function, None) for its own environment,
// (function, Some(creation site)) for a block environment it built.
pub type ScopeRef = (u32, Option<u32>);

// Synthetic ids count down from the top of the range; a bundle has far fewer
// than two billion functions, so they never meet a real id.
const FIRST_BLOCK_SCOPE_ID: u32 = u32::MAX - 1;

impl ClosureContext {
    pub fn new() -> Self {
        Self::default()
    }

    // `from`'s body now lives in `to` (a generator wrapper took over the
    // machine it created), so the block environments `from` built are `to`'s:
    // the generator body's own environment, created once when the machine
    // starts, holds what are now `to`'s locals. Left with `from`, which no
    // longer has a body, its slots read as an ancestor's from `to` (1 178
    // CaptureAndLocal rows on Discord once its generators lifted). The parent
    // chain stays as it is: levels count hops along it. The blocks keep their
    // identity; `to` addresses them at the same levels, and `from` still does.
    pub fn rehome_block_scopes(&mut self, from: u32, to: u32) -> Vec<u32> {
        let moved: Vec<(u32, u32)> = self
            .block_index
            .iter()
            .filter(|((f, _), _)| *f == from)
            .map(|(&(_, level), &id)| (level, id))
            .collect();
        let mut out = Vec::new();
        for (level, id) in moved {
            if self.block_index.contains_key(&(to, level)) {
                continue;
            }
            self.block_index.insert((to, level), id);
            self.block_function.insert(id, to);
            out.push(id);
        }
        out
    }

    // Every block environment the builder saw, built once, in key order.
    pub fn build_block_scopes(&mut self) {
        let sites: Vec<(u32, u32)> = self.site_levels.keys().copied().collect();
        for (creator, site) in sites {
            self.block_scope_at(creator, site);
        }
    }

    // The synthetic scope for the block environment `creator` built at
    // `site`. Its parent is the scope the graph says it was built on, else the
    // creator.
    pub fn block_scope_at(&mut self, creator: u32, site: u32) -> u32 {
        if let Some(&id) = self.block_scopes.get(&(creator, site)) {
            return id;
        }
        let level = match self.site_levels.get(&(creator, site)) {
            Some(&level) => level,
            None if site & CLOSURE_ENV_SITE != 0 => site & !CLOSURE_ENV_SITE,
            None => {
                log::trace!(target: "slots", "block scope: F{creator}@{site} has no level");
                return creator;
            }
        };
        let id = FIRST_BLOCK_SCOPE_ID - self.block_scopes.len() as u32;
        self.block_scopes.insert((creator, site), id);
        self.block_key.insert(id, (creator, site));
        self.block_level.insert(id, level);
        self.block_function.insert(id, creator);
        self.block_index.entry((creator, level)).or_insert(id);
        // Inserted first, so a parent cycle among block scopes ends here.
        let built_on = match self.block_parents.get(&(creator, site)).copied() {
            Some(r) => self.scope_id(r),
            None => creator,
        };
        self.parent_function
            .insert(id, if built_on == id { creator } else { built_on });
        id
    }

    // The block scope `function`'s body addresses at builder `level`. A level
    // no created environment claims is a closure environment's
    // (`NESTED_ENV_LEVEL_BASE + slot`, `GetClosureEnvironment` on a closure
    // loaded out of that slot), a block of its own until A2c3 places it. A
    // slot-held environment has no such level since A2c2.
    pub fn block_at_level(&mut self, function: u32, level: u32) -> u32 {
        if let Some(&id) = self.block_index.get(&(function, level)) {
            return id;
        }
        // The site the builder gave that level; past the cap several share
        // it, and the first stands for them (a placed access names its own).
        let site = self
            .site_levels
            .range((function, 0)..=(function, u32::MAX))
            .find(|(_, &l)| l == level)
            .map(|(&(_, site), _)| site);
        self.block_scope_at(function, site.unwrap_or(CLOSURE_ENV_SITE | level))
    }

    // The id a scope reference stands for: the function, or its block scope.
    pub fn scope_id(&mut self, (function, site): (u32, Option<u32>)) -> u32 {
        match site {
            Some(site) => self.block_scope_at(function, site),
            None => function,
        }
    }

    // `scope_id` for a scope that already has an id.
    pub fn known_scope_id(&self, (function, site): (u32, Option<u32>)) -> Option<u32> {
        match site {
            Some(site) => self.block_scopes.get(&(function, site)).copied(),
            None => Some(function),
        }
    }

    // The reverse of `scope_id`.
    pub fn scope_ref(&self, id: u32) -> (u32, Option<u32>) {
        match self.block_key.get(&id) {
            Some(&(creator, site)) => (creator, Some(site)),
            None => (id, None),
        }
    }

    // The scope that owns slot writes `func` makes at `level`: `func` itself
    // at level 0, the block environment it built at a builder level past
    // `NESTED_ENV_LEVEL_BASE`, otherwise the ancestor that many hops up.
    pub fn slot_owner(&self, func: u32, level: u32) -> Option<u32> {
        if level == 0 {
            return Some(func);
        }
        if level >= crate::ir::NESTED_ENV_LEVEL_BASE {
            return self.block_index.get(&(func, level)).copied();
        }
        self.ancestor_at(func, level)
    }

    // The (owner, slot) a name in `func`'s body stands for, from the bake
    // record: the only source that knows, once names are allocated per owner
    // (`crate::ir::placeholder`).
    pub fn resolve_baked(&self, func: u32, name: &str) -> Option<(u32, u32)> {
        let &(level, slot) = self.baked_captures.get(&func)?.get(name)?;
        Some((self.baked_owner(func, name, (level, slot))?, slot))
    }

    // The scope `name`, read at (level, slot) in `func`'s body, stands for:
    // the one W6 stamped it with when the bake recorded the name at that
    // place, else the walk (IDENTITY_PLAN.md A2b). Counted
    // (`baked_owner_counts`).
    pub fn baked_owner(&self, func: u32, name: &str, place: (u32, u32)) -> Option<u32> {
        let walked = self.slot_owner(func, place.0);
        let recorded = self
            .baked_captures
            .get(&func)
            .and_then(|m| m.get(name))
            .filter(|&&p| p == place)
            .and_then(|_| self.baked_owners.get(&func)?.get(name).copied());
        let Some(owner) = recorded else {
            BAKED_OWNERS[0].fetch_add(1, Relaxed);
            return walked;
        };
        let agrees = walked == Some(owner);
        BAKED_OWNERS[if agrees { 1 } else { 2 }].fetch_add(1, Relaxed);
        if !agrees {
            // Counted, not followed: the walk is what moved after W6.
            log::trace!(target: "slots", "baked owner: F{func} `{name}` at {place:?}: recorded {owner}, the walk {walked:?}");
        }
        Some(owner)
    }

    pub fn is_block_scope(&self, id: u32) -> bool {
        self.block_level.contains_key(&id)
    }

    // The function whose body a scope id belongs to: the function itself, or
    // the function that built a block scope.
    pub fn scope_function(&self, id: u32) -> u32 {
        self.block_function.get(&id).copied().unwrap_or(id)
    }

    // Names of the functions declared directly in the body of `scope` (block
    // scopes included): the identifiers a `class X` or `function X` binds at
    // that level, which a variable declared there cannot share.
    pub fn declared_function_names(&self, scope: u32) -> HashSet<String> {
        let owner = self.scope_function(scope);
        self.parent_function
            .iter()
            .filter(|(child, parent)| {
                !self.is_block_scope(**child) && self.scope_function(**parent) == owner
            })
            .filter_map(|(child, _)| self.function_names.get(child).cloned())
            .collect()
    }

    pub fn add_child(&mut self, parent: u32, child: u32) {
        // hermesc can create a closure of a function inside that very
        // function (a chainable API whose methods rebuild the chain). A
        // function is not its own parent; recording it as such made every
        // walk up the chain stop there and left the function without a module.
        if parent == child {
            return;
        }
        if let Some(&r) = self
            .scope_parents
            .get(&child)
            .or_else(|| self.shared_parents.get(&child))
        {
            let scope = self.scope_id(r);
            if scope != child {
                self.parent_function.insert(child, scope);
            }
            return;
        }
        // Created with a block environment of `parent`: the block scope is the
        // parent, its slots are what the child reads at level 1.
        if let Some(&(creator, level, site)) = self.creation_scopes.get(&child) {
            if creator == parent {
                let scope = match site {
                    Some(site) if self.site_levels.contains_key(&(parent, site)) => {
                        self.block_scope_at(parent, site)
                    }
                    _ => self.block_at_level(parent, level),
                };
                self.parent_function.insert(child, scope);
                return;
            }
        }
        self.parent_function.insert(child, parent);
    }

    // Decide slot names so that no function sees two variables under one name.
    //
    // A slot is rendered by its owner's name for it in every function that
    // reads it, so names are decided per owner, outermost scope first: a slot
    // whose derived name an enclosing scope's slot already uses -- or another of
    // the owner's own slots does -- gets a distinct one. A placeholder becomes
    // `closure_N$K`, kept out of the `closure_L_S` shape that names a level; a
    // recovered name takes a numeric suffix. Decided names are recorded on the
    // owner's `ClosureInfo::names` and copied with the slots into every
    // function's combined view.
    pub fn allocate_slot_names(&mut self) {
        let mut depth: BTreeMap<u32, usize> = BTreeMap::new();
        for &scope in self.function_closures.keys() {
            depth.insert(scope, self.ancestors_of(scope).len());
        }
        let mut order: Vec<u32> = depth.keys().copied().collect();
        order.sort_by_key(|s| (depth[s], *s));

        let mut decided: BTreeMap<u32, BTreeMap<u32, String>> = BTreeMap::new();
        for scope in order {
            let Some(info) = self.function_closures.get_mut(&scope) else {
                continue;
            };
            let previous = std::mem::take(&mut info.names);
            let mut visible: HashSet<String> = HashSet::new();
            for anc in self.ancestors_of(scope) {
                if let Some(names) = decided.get(&anc) {
                    visible.extend(names.values().cloned());
                }
            }
            let info = self.function_closures.get_mut(&scope).unwrap();
            let mut mine = BTreeMap::new();
            let slots: Vec<u32> = info
                .slots
                .keys()
                .copied()
                .filter(|s| s >> 24 == 0)
                .collect();
            for slot in slots {
                let derived = info.derived_slot_name(slot);
                let base = derived.text;
                // A placeholder already decided and baked into the IR stays: a
                // later pass may have renamed the clashing ancestor away, but
                // the body still says `closure_N$K`, and codegen declares the
                // slot by the name decided here.
                let kept = previous
                    .get(&slot)
                    .filter(|p| {
                        p.strip_prefix(base.as_str())
                            .is_some_and(|k| k.starts_with('$'))
                    })
                    .filter(|p| !visible.contains(*p));
                let name = if let Some(p) = kept {
                    p.clone()
                } else if visible.contains(&base) {
                    let mut k = 2u32;
                    loop {
                        let candidate = if derived.source == NameSource::Placeholder {
                            format!("{base}${k}")
                        } else {
                            format!("{base}{k}")
                        };
                        if !visible.contains(&candidate) {
                            break candidate;
                        }
                        k += 1;
                    }
                } else {
                    base.clone()
                };
                if name != base {
                    info.names.insert(slot, name.clone());
                }
                visible.insert(name.clone());
                mine.insert(slot, name);
            }
            decided.insert(scope, mine);
        }
    }

    // `scope`'s ancestors, nearest first, stopping at a cycle.
    pub fn ancestors_of(&self, scope: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut seen = HashSet::from([scope]);
        let mut cur = scope;
        while let Some(&p) = self.parent_function.get(&cur) {
            if !seen.insert(p) {
                break;
            }
            out.push(p);
            cur = p;
        }
        out
    }

    pub fn env_offset(&self, function: u32) -> u32 {
        self.env_offsets.get(&function).copied().unwrap_or(0)
    }

    // Each function's environment offset, from where it was created: a closure
    // given its creator's own environment (level 0) reads one hop up at level 1
    // and needs none; one given the creator's level-N environment sits N hops
    // further out -- plus however far out the creator's own levels already sat.
    // `created` is (creator, child, level of the environment given).
    pub fn set_env_offsets(&mut self, created: &[(u32, u32, u32)]) {
        let by_child: BTreeMap<u32, (u32, u32)> = created
            .iter()
            .rev()
            .map(|&(creator, child, level)| (child, (creator, level)))
            .collect();
        fn offset(
            f: u32,
            by_child: &BTreeMap<u32, (u32, u32)>,
            memo: &mut BTreeMap<u32, u32>,
            seen: &mut HashSet<u32>,
        ) -> u32 {
            if let Some(&o) = memo.get(&f) {
                return o;
            }
            let o = match by_child.get(&f) {
                Some(&(creator, level)) if level > 0 && creator != f && seen.insert(f) => {
                    level + offset(creator, by_child, memo, seen)
                }
                _ => 0,
            };
            memo.insert(f, o);
            o
        }
        let mut memo = BTreeMap::new();
        for &child in by_child.keys() {
            offset(child, &by_child, &mut memo, &mut HashSet::new());
        }
        self.env_offsets = memo.into_iter().filter(|&(_, o)| o > 0).collect();
    }

    pub fn set_creation_scope(&mut self, child: u32, creator: u32, level: u32, site: Option<u32>) {
        self.creation_scopes.insert(child, (creator, level, site));
    }

    pub fn add_closure_info(&mut self, function_id: u32, info: ClosureInfo) {
        self.function_closures.insert(function_id, info);
    }

    pub fn add_function_name(&mut self, function_id: u32, name: String) {
        self.function_names.insert(function_id, name);
    }

    pub fn update_slot_variable(&mut self, function_id: u32, slot: u32, name: String) {
        if let Some(info) = self.function_closures.get_mut(&function_id) {
            if log::log_enabled!(target: "slotname", log::Level::Trace) {
                log::trace!(
                    target: "slotname",
                    "{function_id}:{slot} {:?} -> {name}",
                    info.slots.get(&slot)
                );
            }
            info.slots.insert(slot, ClosureSlotValue::Variable(name));
            // A decided name was decided against the old value; the next
            // `allocate_slot_names` decides again against this one.
            info.names.remove(&slot);
        }
    }

    pub fn mark_async(&mut self, function_id: u32) {
        self.async_functions.insert(function_id);
    }

    pub fn mark_generator(&mut self, function_id: u32) {
        self.generator_functions.insert(function_id);
    }

    pub fn is_async(&self, function_id: u32) -> bool {
        self.async_functions.contains(&function_id)
    }

    pub fn is_generator(&self, function_id: u32) -> bool {
        self.generator_functions.contains(&function_id)
    }

    // Propagate async flag from outer wrapper to inner generator.
    // In Hermes (via Babel), async functions compile as:
    //   1. An outer wrapper created via CreateGeneratorClosure (marked as generator)
    //   2. An inner generator (CreateGenerator) containing the actual body with yields
    //
    // Heuristic: iteratively mark generators as async if their parent is NOT a generator
    // OR if their parent is already marked as async. This handles the two-level chain:
    //   Metro factory → CreateGeneratorClosure(719) → CreateGenerator(720)
    //   719 gets async (parent is non-generator), then 720 gets async (parent 719 is async).
    //
    // Async is detected explicitly elsewhere: modern bytecode marks it via the
    // `CreateAsyncClosure` opcode, and the legacy Babel `_asyncToGenerator(
    // function*(){})` pattern is recognised by `detect_async_generator_wrappers`.
    // Here we only PROPAGATE that flag from an async wrapper to the inner
    // generator body it drives. We must NOT guess "async" from the parent merely
    // not being a generator, a real `function*` also has a non-generator parent,
}

#[cfg(test)]
mod block_scope_tests {
    use super::ClosureContext;
    use crate::analysis::closure::info::{encode_level_slot, ClosureSlotValue};
    use crate::ir::{
        AssignTarget, Binding, Expression, FunctionId, Statement, Value, NESTED_ENV_LEVEL_BASE,
    };

    #[test]
    fn a_closure_stored_in_a_block_scope_reads_that_scope_at_level_one() {
        let block = NESTED_ENV_LEVEL_BASE + 1;
        // Factory 1: slot 0 of its own env holds `global`; a block env holds the class.
        let factory = vec![
            Statement::Assign {
                target: AssignTarget::Binding(Binding::closure_var(0, 0)),
                value: Expression::Value(Value::Binding(Binding::Variable("global".into()))),
            },
            Statement::Assign {
                target: AssignTarget::Binding(Binding::closure_var(block, 0)),
                value: Expression::Function {
                    id: FunctionId(7),
                    name: Some("MessageQueue".into()),
                    is_arrow: false,
                    is_async: false,
                    is_generator: false,
                    env: None,
                },
            },
        ];
        let mut ctx = ClosureContext::new();
        ctx.site_levels.insert((1, 40), block);
        ctx.build_block_scopes();
        ctx.analyze_function(1, &factory);
        ctx.analyze_function(7, &[]);

        // The constructor's parent is the block, whose parent is the factory.
        let scope = ctx.parent_function[&7];
        assert!(ctx.is_block_scope(scope));
        assert_eq!(ctx.parent_function[&scope], 1);

        // Inside the constructor, level 1 slot 0 is the class, level 2 slot 0 is `global`.
        let info = ctx.get_closure_info_for(7);
        assert!(matches!(
            info.slots.get(&encode_level_slot(1, 0)),
            Some(ClosureSlotValue::Function { id: 7, .. })
        ));
        assert!(matches!(
            info.slots.get(&encode_level_slot(2, 0)),
            Some(ClosureSlotValue::Variable(v)) if v == "global"
        ));

        // Inside the factory, the block store resolves under the builder level.
        let own = ctx.get_closure_info_for(1);
        assert_eq!(
            own.get_slot_name(encode_level_slot(block, 0)),
            "MessageQueue"
        );
        assert_eq!(own.get_slot_name(0), "global");
    }

    #[test]
    fn a_rehomed_block_scope_still_owns_the_reads_made_at_its_level() {
        // Step 2 built a block env at its builder level; the generator collapse
        // moves it to wrapper 1. A name baked at that level in 2 still means it.
        let block = NESTED_ENV_LEVEL_BASE + 1;
        let mut ctx = ClosureContext::new();
        ctx.add_child(1, 2);
        ctx.site_levels.insert((2, 40), block);
        let scope = ctx.block_scope_at(2, 40);
        assert_eq!(ctx.rehome_block_scopes(2, 1), vec![scope]);
        assert_eq!(ctx.slot_owner(1, block), Some(scope));
        assert_eq!(ctx.slot_owner(2, block), Some(scope));
    }

    #[test]
    fn a_child_the_rescan_does_not_see_keeps_the_block_it_had() {
        // 9 was created in 5's block env; the rescan no longer sees 5's body,
        // while 3's body builds a block first and takes the id 5's block had.
        let block = NESTED_ENV_LEVEL_BASE + 1;
        let mut ctx = ClosureContext::new();
        ctx.site_levels.insert((5, 10), block);
        ctx.site_levels.insert((3, 20), block);
        ctx.build_block_scopes();
        let old = ctx.block_scopes[&(5, 10)];
        ctx.parent_function.insert(9, old);
        let body = vec![Statement::Assign {
            target: AssignTarget::Binding(Binding::closure_var(block, 0)),
            value: Expression::Function {
                id: FunctionId(8),
                name: None,
                is_arrow: true,
                is_async: false,
                is_generator: false,
                env: None,
            },
        }];
        ctx.reanalyze_all(&std::collections::BTreeMap::from([(3, body)]));
        assert_eq!(ctx.scope_ref(ctx.parent_function[&8]), (3, Some(20)));
        assert_eq!(ctx.scope_ref(ctx.parent_function[&9]), (5, Some(10)));
    }

    // jQuery's `inspectPrefiltersOrTransports` (Expensify F71548): `inspect`
    // compiled inline twice, each copy in its own block. Nothing stores into
    // the second block from the creator's body, so built on first store it
    // was never built and its read printed as the undeclared `closure_130_0`.
    #[test]
    fn every_block_a_creator_builds_is_a_scope_before_any_store() {
        let mut ctx = ClosureContext::new();
        ctx.site_levels.insert((4, 30), NESTED_ENV_LEVEL_BASE + 1);
        ctx.site_levels.insert((4, 90), NESTED_ENV_LEVEL_BASE + 2);
        ctx.build_block_scopes();
        for site in [30, 90] {
            let id = ctx.known_scope_id((4, Some(site))).expect("built");
            assert_eq!(ctx.scope_ref(id), (4, Some(site)));
            assert_eq!(ctx.parent_function[&id], 4);
        }
        assert_eq!(
            ctx.slot_owner(4, NESTED_ENV_LEVEL_BASE + 2),
            ctx.known_scope_id((4, Some(90)))
        );
    }
}

#[cfg(test)]
mod constant_name_tests {
    use super::ClosureContext;
    use crate::analysis::closure::info::{ClosureInfo, ClosureSlotValue};

    #[test]
    fn a_string_slot_does_not_take_the_name_of_a_function() {
        let mut ctx = ClosureContext::new();
        ctx.add_function_name(7, "GuildSelector".into());
        let mut info = ClosureInfo::new();
        info.store_slot(0, ClosureSlotValue::Constant("\"GuildSelector\"".into()));
        info.store_slot(1, ClosureSlotValue::Constant("\"ADMINISTRATOR\"".into()));
        ctx.add_closure_info(1, info);
        ctx.enrich_function_slot_names();
        let info = ctx.get_closure_info_for(1);
        assert_eq!(info.get_slot_name(0), "GuildSelector_str");
        assert_eq!(info.get_slot_name(1), "ADMINISTRATOR");
    }
}

#[cfg(test)]
mod slot_name_tests {
    use super::ClosureContext;
    use crate::analysis::closure::info::{encode_level_slot, ClosureInfo, ClosureSlotValue};

    fn unknown_slots(n: u32) -> ClosureInfo {
        let mut info = ClosureInfo::new();
        for s in 0..n {
            info.store_slot(s, ClosureSlotValue::Unknown);
        }
        info
    }

    // outer's slot 0 and mid's slot 0 are different variables; inner reads
    // both, and they must not print as one name.
    #[test]
    fn a_slot_does_not_reuse_an_enclosing_slots_name() {
        let mut ctx = ClosureContext::new();
        ctx.add_child(1, 2);
        ctx.add_child(2, 3);
        ctx.add_closure_info(1, unknown_slots(1));
        ctx.add_closure_info(2, unknown_slots(2));
        ctx.allocate_slot_names();
        let inner = ctx.get_closure_info_for(3);
        assert_eq!(inner.get_slot_name(encode_level_slot(2, 0)), "closure_0");
        assert_eq!(inner.get_slot_name(encode_level_slot(1, 0)), "closure_0$2");
        assert_eq!(inner.get_slot_name(encode_level_slot(1, 1)), "closure_1");
        assert_eq!(ctx.get_closure_info_for(2).get_slot_name(0), "closure_0$2");
    }

    // A decided placeholder is baked into the IR; a later pass renaming the
    // ancestor away must not undo it, or codegen declares a name the body
    // no longer uses.
    #[test]
    fn a_baked_placeholder_survives_reallocation() {
        let mut ctx = ClosureContext::new();
        ctx.add_child(1, 2);
        ctx.add_closure_info(1, unknown_slots(1));
        ctx.add_closure_info(2, unknown_slots(1));
        ctx.allocate_slot_names();
        ctx.update_slot_variable(1, 0, "bridge".into());
        ctx.allocate_slot_names();
        assert_eq!(ctx.get_closure_info_for(1).get_slot_name(0), "bridge");
        assert_eq!(ctx.get_closure_info_for(2).get_slot_name(0), "closure_0$2");
    }
}

// Tracks which environment-nesting *level* each register currently holds.
//
// Hermes bytecode:
//   CreateEnvironment r0          → r0 = current function env (level 0)
//   GetEnvironment r0, N          → r0 = env N levels up (0 = current)
//   LoadFromEnvironment rD, rE, S → load slot S from the env in rE
//   StoreToEnvironment rE, S, rV  → store into slot S of the env in rE
//
// We lower Load/Store to `ClosureVar { level, slot }` so closure resolution can
// distinguish parent captures from local env slots that share the same index.

use std::collections::{BTreeMap, HashSet};

// Levels from this value up denote a nested environment, one that was loaded out
// of a slot rather than reached by walking up the parent chain. The level encodes
// which slot it came from, so two different nested environments never share one.
// `encode_level_slot` keeps 8 bits of level and real nesting is a handful deep,
// so the upper half of the range is free.
pub const NESTED_ENV_LEVEL_BASE: u32 = 128;
const MAX_LEVEL: u32 = 255;

// The level of an access the scope graph cannot answer and the installed chain
// gives no single answer for (S3_PARKED_ENV_PLAN.md S3.4). No chain is this
// deep and it stays below `NESTED_ENV_LEVEL_BASE`, so no walk resolves it: the
// access keeps a placeholder name, `closure_unknown_{slot}`, undeclared and
// counted, instead of whatever scope the level model's walk would reach.
pub const UNKNOWN_ENV_LEVEL: u32 = 100;

// The levels the whole-program scope graph gives a function's loads and stores
// (V98_ENV_PLAN.md E4), by instruction offset. An access it leaves out keeps
// the builder's own level, moved out by `shift` as `ir::shift_env_levels`
// would: a function built with a plan is not shifted afterwards.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnvPlan {
    pub levels: BTreeMap<u32, u32>,
    pub shift: u32,
    // Stores that park an environment in a slot, by offset, whose every read
    // the scope graph resolved (S3_PARKED_ENV_PLAN.md S3.1): the reads are
    // addressed by level, so the store links nothing, and an environment has
    // no JavaScript form (it printed as `closure_0$2 = tmp`).
    #[serde(default)]
    pub drop_stores: std::collections::BTreeSet<u32>,
}

impl EnvPlan {
    // The level an access the builder gave `raw` ends up with without a plan.
    pub fn shifted(shift: u32, raw: u32) -> u32 {
        if (1..NESTED_ENV_LEVEL_BASE).contains(&raw) {
            raw + shift
        } else {
            raw
        }
    }

    pub fn drops_store(&self, at: u32) -> bool {
        self.drop_stores.contains(&at)
    }

    fn level(&self, at: u32, raw: u32) -> u32 {
        self.levels
            .get(&at)
            .copied()
            .unwrap_or_else(|| Self::shifted(self.shift, raw))
    }
}

#[derive(Debug, Clone, Default)]
pub struct EnvRegMap {
    /// register → environment nesting level (0 = current function)
    reg_level: BTreeMap<u32, u32>,
    /// registers loaded out of a slot of an environment this function reaches
    /// by its chain, with the slot: used as an environment, one parked there
    /// (slot-held); holding a closure, the closure whose environment
    /// `GetClosureEnvironment` takes
    slot_held: BTreeMap<u32, u32>,
    /// registers holding the environment of a closure loaded out of a slot,
    /// with that slot: addressed at `NESTED_ENV_LEVEL_BASE + slot` until the
    /// scope graph places them (IDENTITY_PLAN.md A2c3)
    closure_envs: BTreeMap<u32, u32>,
    /// registers holding an environment this function just created
    created_envs: HashSet<u32>,
    /// created environment register -> the offset that created it: its
    /// identity, where the level is only a handle (capped at `MAX_LEVEL`)
    created_site: BTreeMap<u32, u32>,
    /// Whether the environment this function runs in has already been created.
    /// A second `CreateFunctionEnvironment` in the same function does not rebuild
    /// that environment, it builds a separate small one for a closure being made.
    own_env_created: bool,
    /// How many separate environments this function has built past its own, used
    /// to give each one a distinct level so their slots stay apart.
    extra_env_count: u32,
    /// loads and stores that went through an environment loaded out of a slot,
    /// which the model identifies by slot number alone (V98_ENV_PLAN.md S3)
    pub slot_held_accesses: u32,
    /// offset of the instruction being lowered
    pub at: u32,
    /// the scope graph's levels for this function, when it has any
    pub plan: Option<EnvPlan>,
    /// the scope id of each access the scope graph placed, by offset
    /// (`ClosureContext::access_scopes`, IDENTITY_PLAN.md A1a2)
    pub scopes: Option<BTreeMap<u32, (u32, Option<u32>)>>,
    /// (offset, level) of every environment claimed past the function's own
    pub sites: Vec<(u32, u32)>,
    /// (offset, level) of every load and store, as the builder alone sees it
    pub accesses: Vec<(u32, u32)>,
}

impl EnvRegMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// The IR level a closure-relative environment load denotes: v98+'s
    /// `GetParentEnvironment N` and v96's two-operand `GetEnvironment N`. Both
    /// count from "the environment from the closure" (BytecodeList.def) -- the
    /// one the function was created with -- never from the environment the
    /// function itself creates, which it reaches only through the register
    /// `Create*Environment` returned. The IR contract is `level 0 = this
    /// function's own environment, 1 = the next one out, ...`, so the closure's
    /// environment is level 1 and `N` is level `N + 1`, whether or not the
    /// function creates one of its own.
    ///
    /// This used to add the 1 only for a function that creates no environment.
    /// In one that does, `GetParentEnvironment 0` then landed on level 0, the
    /// function's own: a function whose slot 0 and whose parent's slot 0 held
    /// different variables printed both as `closure_0`, and wrote one where the
    /// source wrote the other. How far out the closure's environment itself
    /// sits -- a closure need not be created with its creator's own -- is a
    /// whole-program fact, applied afterwards (`shift_env_levels`).
    pub fn parent_env_level(&self, operand_level: u32) -> u32 {
        operand_level + 1
    }

    /// Register `reg` now holds the environment at nesting `level`. An
    /// environment reached by walking the parent chain is not one loaded out of a
    /// slot, so any earlier provenance for this register is dropped.
    pub fn set_level(&mut self, reg: u32, level: u32) {
        self.reg_level.insert(reg, level);
        self.slot_held.remove(&reg);
        self.closure_envs.remove(&reg);
        self.created_envs.remove(&reg);
    }

    /// `reg` holds an environment this function just created.
    /// Claim `reg` as the environment this function runs in, or, when that has
    /// already been claimed, as a separate environment built for a closure.
    /// Returns the level to give it: 0 for the running environment, a distinct
    /// level past `NESTED_ENV_LEVEL_BASE` for each separate one.
    ///
    /// Hermes emits one `CreateFunctionEnvironment` for the function itself and
    /// one more per closure needing its own scope, all writing slot 0, slot 1 and
    /// so on. Reading them at a single level merges scopes that share nothing: a
    /// factory whose own slots start at 3 was given a slot 0 by a two slot child
    /// environment, so captures of slot 0 inherited that unrelated value's name.
    pub fn claim_function_env(&mut self, reg: u32) -> u32 {
        if !self.own_env_created {
            self.own_env_created = true;
            self.set_level(reg, 0);
            return 0;
        }
        self.extra_env_count += 1;
        let level = (NESTED_ENV_LEVEL_BASE + self.extra_env_count).min(MAX_LEVEL);
        self.set_level(reg, level);
        self.created_envs.insert(reg);
        self.created_site.insert(reg, self.at);
        self.sites.push((self.at, level));
        level
    }

    /// `reg` holds a block environment this function just built on top of the
    /// one it runs in (a class binding's scope, a `let` inside a loop). Its
    /// slots are not the running environment's slots: giving it level 0 wrote
    /// a class constructor over the factory's `global` parameter under one
    /// name. It gets a level of its own, past `NESTED_ENV_LEVEL_BASE`; a later
    /// store of it into a parent slot re-identifies it by that slot.
    pub fn claim_created_env(&mut self, reg: u32) -> u32 {
        self.extra_env_count += 1;
        let level = (NESTED_ENV_LEVEL_BASE + self.extra_env_count).min(MAX_LEVEL);
        self.set_level(reg, level);
        self.created_envs.insert(reg);
        self.created_site.insert(reg, self.at);
        self.sites.push((self.at, level));
        level
    }

    /// The offset that created the environment `reg` holds, if it holds one
    /// this function created.
    pub fn created_site_of(&self, reg: u32) -> Option<u32> {
        if !self.created_envs.contains(&reg) {
            return None;
        }
        self.created_site.get(&reg).copied()
    }

    /// Level for an env register, defaulting to 0 (current) when unknown.
    /// Unknown is common for Mov/phi-like paths; level 0 is the conservative
    /// historical behaviour.
    /// The level the builder recorded for `reg`, without the default.
    pub fn known_level(&self, reg: u32) -> Option<u32> {
        self.reg_level.get(&reg).copied()
    }

    pub fn level_of(&self, reg: u32) -> u32 {
        self.reg_level.get(&reg).copied().unwrap_or(0)
    }

    /// Record that `reg` received the value of a slot of the environment at
    /// `level`. If that value turns out to be an environment itself, it is one
    /// parked there, which only the scope graph can place.
    pub fn set_source_slot(&mut self, reg: u32, level: u32, slot: u32) {
        self.closure_envs.remove(&reg);
        if level < NESTED_ENV_LEVEL_BASE {
            self.slot_held.insert(reg, slot);
        } else {
            self.slot_held.remove(&reg);
        }
    }

    /// `dst` = the environment of the closure in `closure`. One loaded out of
    /// a slot is told apart from another by that slot, a provisional identity
    /// until the scope graph places a closure's environment as the scope it
    /// was created in (IDENTITY_PLAN.md A2c3). Before A2c2 it had the
    /// slot-held identity by accident, the register keeping the closure's
    /// provenance.
    pub fn set_closure_env(&mut self, dst: u32, closure: u32) {
        if let Some(&slot) = self.slot_held.get(&closure) {
            self.slot_held.remove(&dst);
            self.closure_envs.insert(dst, slot);
        }
    }

    /// The level to address `reg` with when it is used as an environment.
    ///
    /// A register loaded out of a slot holds a nested environment: Hermes parks an
    /// inner scope there and indexes it directly. Falling back to level 0 made its
    /// slots share a name with the current environment's slots, so a login token
    /// written to the inner slot 1 was read back under the name of the outer slot
    /// 1 and the output claimed `setJwt(password)`.
    /// `env_level_of` for a load or store through `reg`, counting one that
    /// goes through a slot-held environment.
    pub fn access_level_of(&mut self, reg: u32) -> u32 {
        if self.slot_held.contains_key(&reg) {
            self.slot_held_accesses += 1;
        }
        let raw = self.env_level_of(reg);
        self.accesses.push((self.at, raw));
        match &self.plan {
            Some(plan) => plan.level(self.at, raw),
            None => raw,
        }
    }

    // The scope the access being lowered was placed in, if it was.
    pub fn access_scope(&self) -> crate::ir::ScopeTag {
        crate::ir::ScopeTag(self.scopes.as_ref().and_then(|s| s.get(&self.at).copied()))
    }

    // An environment loaded out of a slot has no level of its own: the scope
    // parked there is the scope graph's to name (the plan's level, or its
    // explicit unknown). It used to be `NESTED_ENV_LEVEL_BASE + slot`, a fake
    // block scope keyed by the slot number alone, which every environment
    // parked in a slot of that number shared and which a created block of the
    // same level number collided with (IDENTITY_PLAN.md A2c2).
    pub fn env_level_of(&self, reg: u32) -> u32 {
        if let Some(&slot) = self.closure_envs.get(&reg) {
            return (NESTED_ENV_LEVEL_BASE + slot).min(MAX_LEVEL);
        }
        if self.slot_held.contains_key(&reg) {
            return UNKNOWN_ENV_LEVEL;
        }
        self.level_of(reg)
    }

    /// When `dst = src` (Mov), propagate env-level knowledge if `src` is known.
    pub fn copy_reg(&mut self, dst: u32, src: u32) {
        if let Some(&lvl) = self.reg_level.get(&src) {
            self.reg_level.insert(dst, lvl);
        } else {
            self.reg_level.remove(&dst);
        }
        match self.slot_held.get(&src).copied() {
            Some(slot) => {
                self.slot_held.insert(dst, slot);
            }
            None => {
                self.slot_held.remove(&dst);
            }
        }
        match self.closure_envs.get(&src).copied() {
            Some(slot) => {
                self.closure_envs.insert(dst, slot);
            }
            None => {
                self.closure_envs.remove(&dst);
            }
        }
        if self.created_envs.contains(&src) {
            self.created_envs.insert(dst);
            if let Some(&site) = self.created_site.get(&src) {
                self.created_site.insert(dst, site);
            }
        } else {
            self.created_envs.remove(&dst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_get_levels() {
        let mut m = EnvRegMap::new();
        m.set_level(0, 0); // CreateEnvironment r0
        m.set_level(1, 2); // GetEnvironment r1, 2
        assert_eq!(m.level_of(0), 0);
        assert_eq!(m.level_of(1), 2);
        assert_eq!(m.level_of(99), 0); // unknown → current
    }

    // GetParentEnvironment N counts from the closure's environment, which is
    // level 1 whether or not the function also creates one of its own (level
    // 0). This is also the v98 generator body's case: GetParentEnvironment 1
    // is the grandparent, where the factory keeps its constants.
    #[test]
    fn closure_relative_loads_start_at_level_one() {
        let m = EnvRegMap::new();
        assert_eq!(m.parent_env_level(0), 1);
        assert_eq!(m.parent_env_level(1), 2);
    }

    // An environment loaded out of a slot is the scope graph's to place: the
    // builder gives it no level of its own, never one keyed by the slot number.
    #[test]
    fn a_slot_held_environment_has_no_level_of_its_own() {
        let mut m = EnvRegMap::new();
        m.set_source_slot(7, 0, 3);
        assert_eq!(m.env_level_of(7), UNKNOWN_ENV_LEVEL);
        // Loaded out of a block's slot: not a parked environment.
        m.set_source_slot(8, NESTED_ENV_LEVEL_BASE + 1, 3);
        assert_eq!(m.env_level_of(8), 0);
    }

    // Reaching an environment through the parent chain is not reaching one out of
    // a slot: the provenance must not survive.
    #[test]
    fn walking_the_parent_chain_clears_slot_provenance() {
        let mut m = EnvRegMap::new();
        m.set_source_slot(4, 0, 2);
        assert_eq!(m.env_level_of(4), UNKNOWN_ENV_LEVEL);
        m.set_level(4, 1);
        assert_eq!(m.env_level_of(4), 1);
    }

    // The environment of a closure loaded out of a slot keeps that slot's
    // provisional identity; it is not a parked environment.
    #[test]
    fn a_closure_environment_is_told_apart_by_its_slot() {
        let mut m = EnvRegMap::new();
        m.set_source_slot(3, 0, 26);
        m.set_closure_env(3, 3);
        assert_eq!(m.env_level_of(3), NESTED_ENV_LEVEL_BASE + 26);
        m.set_source_slot(5, 0, 27);
        m.set_closure_env(5, 5);
        assert_eq!(m.env_level_of(5), NESTED_ENV_LEVEL_BASE + 27);
    }

    #[test]
    fn copy_propagates_level() {
        let mut m = EnvRegMap::new();
        m.set_level(3, 1);
        m.copy_reg(5, 3);
        assert_eq!(m.level_of(5), 1);
        m.copy_reg(5, 7); // src unknown → clear
        assert_eq!(m.level_of(5), 0);
        assert!(!m.reg_level.contains_key(&5));

        // A Mov of a created environment carries that fact to the destination.
        m.claim_created_env(3);
        m.copy_reg(9, 3);
        assert!(m.created_site_of(9).is_some());
        m.copy_reg(9, 7); // src is not a created env → clear
        assert!(m.created_site_of(9).is_none());
    }
}

// Merge ancestor slot maps, reanalyze whole-program, IPA/Metro name hooks.
use super::super::info::{encode_level_slot, ClosureInfo, ClosureSlotValue};
use super::walk::is_parameter_placeholder;
use super::ClosureContext;
use crate::ir::Statement;
use std::collections::{BTreeMap, HashSet};

impl ClosureContext {
    pub fn get_closure_info_for(&self, function_id: u32) -> ClosureInfo {
        let mut combined = ClosureInfo::new();

        // Build a list of all ancestors (parent, grandparent, etc.)
        // Use visited set to break cycles in parent_function map.
        let mut ancestors = Vec::new();
        let mut visited = std::collections::HashSet::new();
        visited.insert(function_id);
        let mut current = function_id;
        while let Some(&parent) = self.parent_function.get(&current) {
            if !visited.insert(parent) {
                break;
            }
            ancestors.push(parent);
            current = parent;
        }

        // The block environments this function built are addressed, inside
        // it, by the builder level the synthetic scope remembers.
        for (&(owner, level), &scope) in &self.block_scopes {
            if owner != function_id {
                continue;
            }
            if let Some(info) = self.function_closures.get(&scope) {
                for (&slot, value) in &info.slots {
                    combined
                        .slots
                        .entry(encode_level_slot(level, slot))
                        .or_insert_with(|| value.clone());
                }
                for (&slot, name) in &info.names {
                    combined
                        .names
                        .entry(encode_level_slot(level, slot))
                        .or_insert_with(|| name.clone());
                }
            }
        }

        // IR contract (see ir/builder/env_state.rs):
        //   ClosureVar.level 0 = this function's environment
        //   ClosureVar.level 1 = direct parent, 2 = grandparent, …
        // Ancestor depth d maps to IR level d+1. Keys never collide with local
        // level-0 slots that share the same slot *index*.
        for (depth, &ancestor) in ancestors.iter().enumerate() {
            if let Some(ancestor_info) = self.function_closures.get(&ancestor) {
                let ir_level = (depth as u32) + 1;
                for (&slot, value) in &ancestor_info.slots {
                    // A block scope of the ancestor is not one of its slots.
                    if slot >> 24 != 0 {
                        continue;
                    }
                    let key = encode_level_slot(ir_level, slot);
                    // Closer ancestors win if a deeper one already filled the key
                    // (should not happen, each level is unique).
                    combined.slots.entry(key).or_insert_with(|| value.clone());
                    if let Some(name) = ancestor_info.names.get(&slot) {
                        combined.names.entry(key).or_insert_with(|| name.clone());
                    }
                }
            }
        }

        // Local env (IR level 0) is this function's own environment and nothing
        // else: every closure-relative load is level 1 or more
        // (`EnvRegMap::parent_env_level`). Level 0 used to be read as "often the
        // captured parent environment", and an ancestor's slot was merged in at
        // level 0 when the local one was missing or looked temporary; once the
        // levels were right, that put a parent's variable under the name of the
        // function's own slot of the same index.
        if let Some(local_info) = self.function_closures.get(&function_id) {
            for (slot, value) in &local_info.slots {
                combined.slots.insert(*slot, value.clone());
            }
            for (slot, name) in &local_info.names {
                combined.names.insert(*slot, name.clone());
            }
        }

        combined
    }

    pub fn resolve_closure_var(
        &self,
        function_id: u32,
        level: u32,
        slot: u32,
    ) -> Option<ClosureSlotValue> {
        // Walk up the parent chain to the appropriate level.
        // Break on cycles to avoid infinite loops.
        let mut current = function_id;
        let mut visited = std::collections::HashSet::new();
        visited.insert(current);
        for _ in 0..=level {
            let parent = *self.parent_function.get(&current)?;
            if !visited.insert(parent) {
                return None;
            }
            current = parent;
        }

        self.function_closures
            .get(&current)?
            .slots
            .get(&slot)
            .cloned()
    }

    // For each function, if its closure slots store generic `argN` names,
    // replace them with the IPA-inferred names from the same function.
    pub fn update_with_ipa_names(&mut self, param_names: &BTreeMap<u32, Vec<Option<String>>>) {
        for (&func_id, info) in self.function_closures.iter_mut() {
            if let Some(names) = param_names.get(&func_id) {
                info.update_with_param_names(names);
            }
        }
    }

    /// Apply Metro factory param role names (`arg1`→`require`, …) only to
    /// functions that are actual Metro factories (`is_factory`).
    ///
    /// Must not be applied to arbitrary functions: their `argN` are normal
    /// parameters, not Metro roles (see Babel helpers mislabeled as `require`).
    pub fn apply_metro_factory_param_roles(
        &mut self,
        roles_for: impl Fn(u32) -> Option<crate::analysis::metro::FactoryRoles>,
    ) {
        for (&func_id, info) in self.function_closures.iter_mut() {
            if let Some(roles) = roles_for(func_id) {
                info.apply_metro_param_roles(&roles);
            }
        }
    }

    pub fn get_function_name(&self, function_id: u32) -> Option<&str> {
        self.function_names.get(&function_id).map(|s| s.as_str())
    }

    /// Walk `level` hops up the parent chain from `from_fn`.
    /// level 1 → direct parent, level 2 → grandparent, …
    pub fn ancestor_at(&self, from_fn: u32, level: u32) -> Option<u32> {
        if level == 0 {
            return Some(from_fn);
        }
        let mut current = from_fn;
        let mut visited = HashSet::new();
        visited.insert(current);
        for _ in 0..level {
            let parent = *self.parent_function.get(&current)?;
            if !visited.insert(parent) {
                return None;
            }
            current = parent;
        }
        Some(current)
    }

    /// Re-scan all function IR to refresh slot maps and parent edges.
    ///
    /// Call after semantic naming / IPA so stores capture meaningful variable
    /// names, not only pre-naming `argN`/`tmp*`. Rebuilds `function_closures`
    /// while merging parent edges. Applies deferred level≥1 stores onto the
    /// correct ancestor (nested body writing into a parent env).
    pub fn reanalyze_all(&mut self, all_ir: &BTreeMap<u32, Vec<Statement>>) {
        self.function_closures.clear();
        for scope in self.block_level.keys() {
            self.parent_function.remove(scope);
        }
        self.block_scopes.clear();
        self.block_level.clear();
        self.block_function.clear();
        let mut deferred: Vec<(u32, u32, u32, ClosureSlotValue)> = Vec::new();
        let mut keys: Vec<u32> = all_ir.keys().copied().collect();
        keys.sort();
        for fid in keys {
            let Some(stmts) = all_ir.get(&fid) else {
                continue;
            };
            self.analyze_function_collecting(fid, stmts, &mut deferred);
        }
        // Second pass: ancestor links and function names are complete.
        for (from_fn, level, slot, val) in deferred {
            // A nested body storing one of its own parameters (`logFn = fn`
            // inside `setLogFn(fn)`) says nothing about the slot's name: the
            // index is the nested function's, and read as the owner's it
            // named the slot after a factory role (`global`).
            if matches!(&val, ClosureSlotValue::Variable(v) if is_parameter_placeholder(v)) {
                continue;
            }
            if let Some(target) = self.ancestor_at(from_fn, level) {
                super::walk::trace_store(target, slot, &val);
                self.function_closures
                    .entry(target)
                    .or_default()
                    .store_slot(slot, val);
            }
        }
        self.enrich_function_slot_names();
    }

    /// Fill `Function { id, name: None }` slots with resolved `function_names`.
    pub fn enrich_function_slot_names(&mut self) {
        let names = self.function_names.clone();
        let taken: HashSet<&str> = names.values().map(String::as_str).collect();
        for info in self.function_closures.values_mut() {
            for value in info.slots.values_mut() {
                match value {
                    ClosureSlotValue::Function { id, name } => {
                        if name.is_none() {
                            if let Some(n) = names.get(id) {
                                *name = Some(n.clone());
                            }
                        }
                    }
                    // A slot named after the text of the string it holds
                    // (`"GuildSelector"`) must not take the name of a function
                    // of the bundle: `const GuildSelector = "GuildSelector"`
                    // next to `function GuildSelector` binds the name twice.
                    ClosureSlotValue::Constant(text) => {
                        if let Some(derived) = super::super::info::name_from_constant_text(text) {
                            if taken.contains(derived.as_str()) {
                                *value = ClosureSlotValue::Variable(format!("{derived}_str"));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

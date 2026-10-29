// Resolve baked `closure_{level}_{slot}` ancestor captures by inheriting the
// resolved name of the ancestor's slot.
//
// `resolve_closures` bakes a raw `closure_{level}_{slot}` variable when the ancestor
// slot was still `Unknown` at resolution time (the ancestor is named later by module
// propagation / semantic naming). By the end of the naming pipeline the ancestor slot
// often has a real name (a module name, a stable capture), so a descendant capture can
// inherit it. This turns noise like `closure_1_9.default.logEvents(closure_1_5.EVENT)`
// into `logger.default.logEvents(constants.EVENT)`.
//
// Accuracy guard: a name is inherited only when it is unambiguous. If two distinct
// ancestor slots resolve to the same name in one function, or the target name is
// already a live local there, the capture is left as `closure_{level}_{slot}` rather
// than aliased. An honest generic name is preferred over two different values sharing
// one identifier.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::analysis::naming::rename_variables_in_stmts;
use crate::analysis::{ClosureContext, ClosureInfo};
use crate::ir::{Expression, Statement, Value, Visitor};

pub fn inherit_ancestor_closure_names(
    all_ir: &mut BTreeMap<u32, Vec<Statement>>,
    closure_ctx: &ClosureContext,
    param_names: &BTreeMap<u32, Vec<Option<String>>>,
) -> usize {
    let fids: Vec<u32> = all_ir.keys().copied().collect();
    // `get_closure_info_for` rebuilds and clones the whole ancestor-slot map, so
    // memoize it: a given ancestor id is resolved once and reused across every
    // descendant capture in every function.
    let mut info_cache: HashMap<u32, ClosureInfo> = HashMap::new();
    let mut total = 0;

    // Reused across functions (cleared each iteration) to avoid per-function allocation.
    let mut all_names: HashSet<String> = HashSet::new();
    let mut closure_names: HashSet<String> = HashSet::new();
    // (scope, slot, name) for every slot a reader took its name from.
    let mut inherited: BTreeSet<(u32, u32, String)> = BTreeSet::new();

    for fid in fids {
        all_names.clear();
        closure_names.clear();
        if let Some(stmts) = all_ir.get(&fid) {
            collect_names(stmts, &mut all_names, &mut closure_names);
        }
        if closure_names.is_empty() {
            continue;
        }

        // Resolve each capture to a candidate name, and count how many distinct
        // captures land on each target so ambiguous targets can be dropped.
        // A `closure_N` this function reads but never writes is a capture of
        // its parent's slot N: an own slot is always written by its owner. It
        // was printed without a level because the slot had no name when the
        // read was baked (`get_slot_name`), and the owner was named later:
        // 1 059 reads of moment's `thresholds` stayed `closure_105` while the
        // owner bound the slot as `dependencyMap3`.
        let written = all_ir
            .get(&fid)
            .map(|stmts| written_names(stmts))
            .unwrap_or_default();
        let mut candidates: Vec<(String, String, u32, u32)> = Vec::new();
        let mut target_counts: HashMap<String, u32> = HashMap::new();
        for name in &closure_names {
            // A single-number name carries no level: the exact origin the
            // bake recorded is the only safe source (guessing level 1 renamed
            // moment's `locales` object after the thresholds' slot).
            let recorded = closure_ctx
                .baked_captures
                .get(&fid)
                .and_then(|m| m.get(name))
                .copied();
            let (level, slot) = match crate::ir::placeholder::parse(name) {
                // A disambiguated `closure_N$K` is not let through: see `crate::ir::placeholder::parse`.
                Some(p) if p.variant.is_some() => continue,
                Some(p) if p.level > 0 => (p.level, p.slot),
                Some(_) if !written.contains(name) => match recorded {
                    Some(origin) => origin,
                    None => continue,
                },
                _ => continue,
            };
            if level == 0 {
                continue;
            }
            let Some(ancestor) = closure_ctx.baked_owner(fid, name, (level, slot)) else {
                continue;
            };
            let info = info_cache
                .entry(ancestor)
                .or_insert_with(|| closure_ctx.get_closure_info_for(ancestor));
            let ancestor_name = info.get_slot_name(slot);
            if is_inheritable(&ancestor_name) && ancestor_name != *name {
                *target_counts.entry(ancestor_name.clone()).or_insert(0) += 1;
                candidates.push((name.clone(), ancestor_name, ancestor, slot));
            }
        }

        let mut renames: BTreeMap<String, String> = BTreeMap::new();
        let mut used: Option<HashSet<String>> = None;
        for (old, target, ancestor, slot) in candidates {
            // Ambiguous: two different captures want the same name in this scope.
            if target_counts.get(&target).copied().unwrap_or(0) > 1 {
                continue;
            }
            // Collision: the name is already a live variable in this function.
            // Another capture keeps it. A plain local of this body gives it up
            // and takes a fresh name when the owner really binds the slot under
            // that name: nothing outside the body can name the local, whereas
            // the capture left as its placeholder named a variable no one
            // declares (`setState(closure_10)` beside the owner's `state`, once
            // the body read `const state = self.state`). Where the owner binds
            // the slot as something else, the slot's recorded name is only a
            // hint (often this very local's), and the capture stays as it is.
            if all_names.contains(&target) {
                let stmts = &all_ir[&fid];
                if renames.contains_key(&target)
                    || !is_plain_local(stmts, &target)
                    || is_own_slot_name(closure_ctx, fid, &target)
                    || !owner_binds(all_ir, closure_ctx, ancestor, slot, &target)
                {
                    continue;
                }
                let used = used.get_or_insert_with(|| {
                    let mut u = written.clone();
                    u.extend(all_names.iter().cloned());
                    u.extend(target_counts.keys().cloned());
                    u
                });
                let fresh = super::closure_definitions::make_unique_name(&target, used);
                renames.insert(target.clone(), fresh);
            }
            inherited.insert((ancestor, slot, target.clone()));
            renames.insert(old, target);
        }

        if !renames.is_empty() {
            if let Some(stmts) = all_ir.get_mut(&fid) {
                rename_variables_in_stmts(stmts, &renames);
                total += renames.len();
            }
        }
    }
    total + rename_owner_bindings(all_ir, closure_ctx, param_names, &inherited)
}

// The owner's own binding of a slot its readers now call `name`. The slot's
// recorded name is often the parameter stored into it, so the readers said
// `filterBy` while the owner kept `let closure_0 = filterBy`: one variable
// under two names (I1), 1 355 of them on Equinox. The owner follows only
// when every other reference to the slot already says `name` and `name`
// means nothing else in the owner's body; the parameter whose value the slot
// holds is the one exception, and its store then prints as `filterBy =
// filterBy`, which codegen drops.
fn rename_owner_bindings(
    all_ir: &mut BTreeMap<u32, Vec<Statement>>,
    closure_ctx: &ClosureContext,
    param_names: &BTreeMap<u32, Vec<Option<String>>>,
    inherited: &BTreeSet<(u32, u32, String)>,
) -> usize {
    use super::closure_definitions::{owner_slot_names, SlotFollow};
    if inherited.is_empty() {
        return 0;
    }
    let follow = SlotFollow::new(all_ir);
    let owner_names = owner_slot_names(all_ir, closure_ctx);
    let no_renames = BTreeMap::new();
    let mut names_of: HashMap<u32, HashSet<String>> = HashMap::new();
    let mut per_owner: BTreeMap<u32, BTreeMap<String, String>> = BTreeMap::new();
    for (scope, slot, name) in inherited {
        let Some(var) = follow.owner_binding_if_follows(
            closure_ctx,
            &owner_names,
            (*scope, *slot),
            name,
            &no_renames,
        ) else {
            continue;
        };
        let owner = closure_ctx.scope_function(*scope);
        let Some(stmts) = all_ir.get(&owner) else {
            continue;
        };
        let names = names_of
            .entry(owner)
            .or_insert_with(|| super::capture_sync::names_in(stmts));
        if names.contains(name) || closure_ctx.declared_function_names(owner).contains(name) {
            continue;
        }
        let param = param_names
            .get(&owner)
            .and_then(|ps| ps.iter().position(|p| p.as_deref() == Some(name.as_str())));
        let key = crate::ir::SlotRef {
            owner: *scope,
            slot: *slot,
        };
        if param.is_some_and(|i| !stores_param(stmts, key, i as u32)) {
            continue;
        }
        per_owner
            .entry(owner)
            .or_default()
            .insert(var, name.clone());
    }
    let mut total = 0;
    for (owner, mut renames) in per_owner {
        // Two slots of one owner that would take one name: neither does.
        let mut counts: HashMap<String, usize> = HashMap::new();
        for n in renames.values() {
            *counts.entry(n.clone()).or_default() += 1;
        }
        renames.retain(|_, n| counts[n] == 1);
        if let Some(stmts) = all_ir.get_mut(&owner) {
            total += renames.len();
            rename_variables_in_stmts(stmts, &renames);
        }
    }
    total
}

// The body stores parameter `index` into `slot` (`closure_0 = arg0`).
fn stores_param(stmts: &[Statement], slot: crate::ir::SlotRef, index: u32) -> bool {
    use crate::ir::{AssignTarget, Binding};
    struct S {
        slot: crate::ir::SlotRef,
        index: u32,
        found: bool,
    }
    impl S {
        fn check(&mut self, v: &crate::ir::Var, value: &Expression) {
            self.found |= v.slot == Some(self.slot)
                && matches!(value, Expression::Value(Value::Parameter(i)) if *i == self.index);
        }
    }
    impl<'a> Visitor<'a> for S {
        fn visit_statement(&mut self, s: &'a Statement) {
            match s {
                Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable(v)),
                    value,
                } => self.check(v, value),
                Statement::Let { name, value, .. } => self.check(name, value),
                _ => {}
            }
            self.walk_statement(s);
        }
    }
    let mut s = S {
        slot,
        index,
        found: false,
    };
    for st in stmts {
        s.visit_statement(st);
    }
    s.found
}

// Every name the statements assign or declare, nested blocks included.
fn written_names(stmts: &[Statement]) -> HashSet<String> {
    use crate::ir::AssignTarget;
    struct W(HashSet<String>);
    impl<'a> Visitor<'a> for W {
        fn visit_assign_target(&mut self, t: &'a AssignTarget) {
            if let AssignTarget::Binding(crate::ir::Binding::Variable(n)) = t {
                self.0.insert(n.name.clone());
            }
            self.walk_assign_target(t);
        }
        fn visit_binding_def(&mut self, name: &'a str) {
            self.0.insert(name.to_string());
        }
    }
    let mut w = W(HashSet::new());
    for s in stmts {
        w.visit_statement(s);
    }
    w.0
}

// A slot name worth inheriting: not one of the generic fallbacks
// (`closure_N`, `outerN`, `argN`, `rN`, `tmp*`, `cN`, `reN`, `fN`).
fn is_inheritable(name: &str) -> bool {
    if name.len() < 2 {
        return false;
    }
    // A name that is not a valid identifier was never a binding name: it is an
    // export key read verbatim, such as the `get ActivityIndicator` accessor a
    // module defines on its exports. Inheriting it puts a getter's name on the
    // module object, so `react-native` was printed as `get_ActivityIndicator`
    // once the space was sanitised away.
    if !crate::util::is_valid_identifier(name) {
        return false;
    }
    if crate::ir::placeholder::has_prefix(name) || name.starts_with("outer") {
        return false;
    }
    if name.starts_with("tmp") {
        return false;
    }
    // prefix + all-digits fallbacks: argN, rN, cN, fN, reN
    for pre in ["arg", "re", "r", "c", "f"] {
        if let Some(rest) = name.strip_prefix(pre) {
            if !rest.is_empty() && rest.chars().all(|ch| ch.is_ascii_digit()) {
                return false;
            }
        }
    }
    true
}

// Collect every referenced variable name (for collision detection) and the subset
// that are `closure_*` captures (the rename candidates).
// `name` is the name of one of `func`'s own environment slots (its function
// scope's or a block scope's). The owner's own references to such a slot can
// be unstamped, so it would pass for a plain local: superagent's `Request`
// constructor lost the `let` of the `self` its callbacks read.
fn is_own_slot_name(closure_ctx: &ClosureContext, func: u32, name: &str) -> bool {
    closure_ctx
        .function_closures
        .iter()
        .filter(|(&scope, _)| scope == func || closure_ctx.scope_function(scope) == func)
        .any(|(_, info)| {
            info.slots
                .keys()
                .filter(|k| *k >> 24 == 0)
                .any(|&k| info.get_slot_name(k) == name)
        })
}

// The owner's own body refers to slot `slot` of scope `scope` as `name`.
fn owner_binds(
    all_ir: &BTreeMap<u32, Vec<Statement>>,
    closure_ctx: &ClosureContext,
    scope: u32,
    slot: u32,
    name: &str,
) -> bool {
    struct B<'n> {
        want: crate::ir::SlotRef,
        name: &'n str,
        found: bool,
    }
    impl B<'_> {
        fn check(&mut self, v: &crate::ir::Var) {
            self.found |= v.name == self.name && v.slot == Some(self.want);
        }
    }
    impl<'a> Visitor<'a> for B<'_> {
        fn visit_statement(&mut self, s: &'a Statement) {
            if let Statement::Let { name, .. } = s {
                self.check(name);
            }
            self.walk_statement(s);
        }
        fn visit_expression(&mut self, e: &'a Expression) {
            if let Expression::Value(Value::Binding(crate::ir::Binding::Variable(v))) = e {
                self.check(v);
            }
            self.walk_expression(e);
        }
        fn visit_assign_target(&mut self, t: &'a crate::ir::AssignTarget) {
            if let crate::ir::AssignTarget::Binding(crate::ir::Binding::Variable(v)) = t {
                self.check(v);
            }
            self.walk_assign_target(t);
        }
    }
    let Some(stmts) = all_ir.get(&closure_ctx.scope_function(scope)) else {
        return false;
    };
    let mut b = B {
        want: crate::ir::SlotRef { owner: scope, slot },
        name,
        found: false,
    };
    for s in stmts {
        b.visit_statement(s);
        if b.found {
            return true;
        }
    }
    false
}

// `name` is a local of this body: assigned or declared here and never used for
// an environment slot (parameters are still `Parameter` values at this stage),
// so renaming it inside the body changes nothing outside it.
fn is_plain_local(stmts: &[Statement], name: &str) -> bool {
    struct P<'n> {
        name: &'n str,
        declared: bool,
        slotted: bool,
    }
    impl<'a> Visitor<'a> for P<'_> {
        fn visit_statement(&mut self, s: &'a Statement) {
            if let Statement::Let { name, .. } = s {
                if name.name == self.name {
                    self.declared = true;
                    self.slotted |= name.slot.is_some();
                }
            }
            self.walk_statement(s);
        }
        fn visit_expression(&mut self, e: &'a Expression) {
            if let Expression::Value(Value::Binding(crate::ir::Binding::Variable(n))) = e {
                if n.name == self.name {
                    self.slotted |= n.slot.is_some();
                }
            }
            self.walk_expression(e);
        }
        fn visit_assign_target(&mut self, t: &'a crate::ir::AssignTarget) {
            if let crate::ir::AssignTarget::Binding(crate::ir::Binding::Variable(n)) = t {
                if n.name == self.name {
                    self.declared = true;
                    self.slotted |= n.slot.is_some();
                }
            }
            self.walk_assign_target(t);
        }
    }
    let mut p = P {
        name,
        declared: false,
        slotted: false,
    };
    for s in stmts {
        p.visit_statement(s);
    }
    p.declared && !p.slotted
}

fn collect_names(stmts: &[Statement], all: &mut HashSet<String>, closures: &mut HashSet<String>) {
    struct C<'a>(&'a mut HashSet<String>, &'a mut HashSet<String>);
    impl<'b> Visitor<'b> for C<'_> {
        fn visit_expression(&mut self, e: &'b Expression) {
            if let Expression::Value(Value::Binding(crate::ir::Binding::Variable(n))) = e {
                self.0.insert(n.name.clone());
                if crate::ir::placeholder::has_prefix(n) {
                    self.1.insert(n.name.clone());
                }
            }
            self.walk_expression(e);
        }
    }
    let mut c = C(all, closures);
    for s in stmts {
        c.visit_statement(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{AssignTarget, Binding, SlotRef, Var};

    fn slot_var(name: &str, owner: u32, slot: u32) -> Var {
        Var {
            name: name.into(),
            slot: Some(SlotRef { owner, slot }),
        }
    }

    // Function 1 stores its parameter in slot 0; function 2 reads the slot
    // by the parameter's name. `local` is a further statement for the owner.
    fn owner_and_reader(local: Option<Statement>) -> BTreeMap<u32, Vec<Statement>> {
        let mut owner = vec![Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(slot_var("closure_0", 1, 0))),
            value: Expression::Value(Value::Parameter(0)),
        }];
        owner.extend(local);
        let reader = vec![Statement::Expr(Expression::Value(Value::Binding(
            Binding::Variable(slot_var("filterBy", 1, 0)),
        )))];
        BTreeMap::from([(1, owner), (2, reader)])
    }

    fn rename(all_ir: &mut BTreeMap<u32, Vec<Statement>>) -> usize {
        let mut ctx = ClosureContext::default();
        ctx.parent_function.insert(2, 1);
        let params = BTreeMap::from([(1, vec![Some("filterBy".to_string())])]);
        let inherited = BTreeSet::from([(1, 0, "filterBy".to_string())]);
        rename_owner_bindings(all_ir, &ctx, &params, &inherited)
    }

    #[test]
    fn the_owner_takes_the_name_its_readers_inherited() {
        let mut all_ir = owner_and_reader(None);
        assert_eq!(rename(&mut all_ir), 1);
        assert_eq!(all_ir[&1][0].to_string(), "filterBy = arg0;");
    }

    #[test]
    fn the_owner_keeps_its_binding_when_the_name_is_taken() {
        let local = Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable("filterBy".into())),
            value: Expression::Value(Value::Constant(crate::ir::Constant::Null)),
        };
        let mut all_ir = owner_and_reader(Some(local));
        assert_eq!(rename(&mut all_ir), 0);
        assert_eq!(all_ir[&1][0].to_string(), "closure_0 = arg0;");
    }

    #[test]
    fn accessor_keys_are_not_inheritable() {
        // An exports accessor key reaches a slot verbatim. It is a property name,
        // never the name of the binding holding the module, so inheriting it
        // renamed `react-native` into `get_ActivityIndicator`.
        assert!(!is_inheritable("get ActivityIndicator"));
        assert!(!is_inheritable("set value"));
        assert!(!is_inheritable("app-platform"));
        assert!(!is_inheritable("2fa"));
    }

    #[test]
    fn real_binding_names_still_inherit() {
        assert!(is_inheritable("SecureStore"));
        assert!(is_inheritable("_reactNative"));
        assert!(is_inheritable("$schema"));
    }

    #[test]
    fn generic_names_stay_rejected() {
        for name in ["r12", "arg3", "c0", "re7", "f2"] {
            assert!(!is_inheritable(name), "{name} should not be inherited");
        }
    }
}

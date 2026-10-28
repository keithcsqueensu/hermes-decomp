// One body, several creators (S3_PARKED_ENV_PLAN.md S3.3).
//
// Hermes compiles a function once per source location, and hermesc inlines a
// small factory into each caller: one compiled inner function is then created
// by several functions, each on its own environment. Every creator renders
// the same body inline, but the body was named against one of them, so in the
// others it reads names that creator does not have (`sum` where the creator
// calls the slot `closure_1`), or worse, names that mean something else there.
//
// The fix is per creator, not per slot: each creator other than the one the
// body was named against gets a copy of the body (and of the nested functions
// that read the same scope) with the stamped slot references renamed to that
// creator's names, under a synthetic function id, and its own creation site
// points at the copy. Each creator keeps its own names and declarations, so
// creators that run at once never share a binding.
//
// A creator is left with the shared body, and counted, when the copy cannot
// be made faithfully: the creator never names a slot the body reads, names it
// twice, or the new name is already a different variable in the body.

use super::PipelineContext;
use crate::ir::{
    AssignTarget, Binding, Expression, FunctionId, MutVisitor, SlotRef, Statement, Value, Visitor,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

// Synthetic function ids for the copies. Real ids are far below; block-scope
// ids count down from the top of the range.
pub(super) const FIRST_VARIANT_ID: u32 = 0x4000_0000;

type Body = (Vec<String>, Vec<Statement>);

#[derive(Default)]
pub(super) struct Variants {
    // copy -> the function it copies.
    pub origin: BTreeMap<u32, u32>,
    // creator -> (shared function -> its copy for that creator).
    pub by_holder: BTreeMap<u32, BTreeMap<u32, u32>>,
}

impl PipelineContext {
    // Makes the copies, adds them to `prepared`, and points each creator in
    // `prepared` at its copy. Module factories and orphan roots are prepared
    // elsewhere; `codegen_statements` points them through `by_holder`.
    pub(super) fn shared_body_variants(&self, prepared: &mut BTreeMap<u32, Body>) -> Variants {
        let mut out = Variants::default();
        let Some(ctx) = self.closure_ctx.as_ref() else {
            return out;
        };
        let mut census: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut factory_bodies: HashMap<u32, Vec<Statement>> = HashMap::new();
        let mut others_of: HashMap<(u32, u32), HashSet<String>> = HashMap::new();
        let mut next = FIRST_VARIANT_ID;
        for (&f, creators) in &ctx.shared_creations {
            if !prepared.contains_key(&f) {
                *census.entry("no body").or_default() += 1;
                continue;
            }
            let owner_of = |(g, level): (u32, Option<u32>)| match level {
                None => Some(g),
                Some(l) => ctx.block_scopes.get(&(g, l)).copied(),
            };
            let owners: BTreeMap<u32, u32> = creators
                .iter()
                .filter_map(|(&g, &r)| Some((g, owner_of(r)?)))
                .collect();
            if owners.len() != creators.len() {
                *census.entry("a creation scope is gone").or_default() += 1;
                continue;
            }
            let scopes: HashSet<u32> = owners.values().copied().collect();
            let family = descendants(f, prepared, &self.registry.function_to_module);
            // The slots of a creation scope the family reads, and whose.
            let mut read: BTreeMap<u32, BTreeMap<u32, String>> = BTreeMap::new();
            for id in &family {
                for (name, slot) in slot_refs(&prepared[id].1) {
                    if let Some(s) = slot.filter(|s| scopes.contains(&s.owner)) {
                        read.entry(s.owner)
                            .or_default()
                            .insert(s.slot, name.to_string());
                    }
                }
            }
            let rep = match read.len() {
                0 => {
                    *census.entry("reads no creation scope").or_default() += 1;
                    continue;
                }
                1 => *read.keys().next().unwrap(),
                _ => {
                    *census.entry("reads several creation scopes").or_default() += 1;
                    continue;
                }
            };
            let wanted = &read[&rep];
            // Every name the family binds or reads other than the shared
            // scope's: a rename onto one of them would capture it.
            let mut taken: HashSet<String> = HashSet::new();
            for id in &family {
                let (params, stmts) = &prepared[id];
                taken.extend(params.iter().cloned());
                for (name, slot) in slot_refs(stmts) {
                    if slot.is_none_or(|s| s.owner != rep) {
                        taken.insert(name.to_string());
                    }
                }
                taken.extend(bound_names(stmts));
            }
            for (&g, &owner) in &owners {
                if owner == rep {
                    continue;
                }
                let creator: &[Statement] = match prepared.get(&g) {
                    Some((_, stmts)) => stmts,
                    None => factory_bodies.entry(g).or_insert_with(|| {
                        self.all_ir
                            .get(&g)
                            .map(|s| self.codegen_statements(g, s))
                            .unwrap_or_default()
                    }),
                };
                let names = owner_names(creator, owner);
                let others = others_of
                    .entry((g, owner))
                    .or_insert_with(|| other_names(creator, owner));
                let mut rename: BTreeMap<u32, String> = BTreeMap::new();
                let mut why = None;
                for (slot, old) in wanted {
                    match names.get(slot) {
                        Some(set) if set.len() == 1 => {
                            let new = set.iter().next().unwrap();
                            if new != old && taken.contains(new) {
                                why = Some("the creator's name is another variable in the body");
                            }
                            rename.insert(*slot, new.clone());
                        }
                        Some(_) => why = Some("the creator names a slot twice"),
                        // The creator never mentions the slot (only closures
                        // write it, or nothing does): the body's name stays,
                        // unless it is a different variable in the creator.
                        None if others.contains(old.as_str()) => {
                            why = Some("the body's name for a slot the creator never names is another variable there")
                        }
                        None => {
                            rename.insert(*slot, old.clone());
                        }
                    }
                }
                if let Some(why) = why {
                    log::trace!(
                        target: "slots",
                        "shared body F{f} in F{g}: {why}; reads {:?} of {rep}, creator has {:?} of {owner}",
                        wanted,
                        names
                    );
                    *census.entry(why).or_default() += 1;
                    continue;
                }
                if rename.iter().all(|(slot, new)| &wanted[slot] == new) {
                    *census.entry("creator: same names").or_default() += 1;
                    continue;
                }
                let mut memo: HashMap<u32, Option<u32>> = HashMap::new();
                let mut made: Vec<(u32, u32, Body)> = Vec::new();
                let v = copy_renamed(
                    f, rep, &rename, prepared, &family, &mut memo, &mut next, &mut made,
                );
                let Some(v) = v else {
                    *census.entry("creator: nothing to rename").or_default() += 1;
                    continue;
                };
                for (id, of, body) in made {
                    out.origin.insert(id, of);
                    prepared.insert(id, body);
                }
                out.by_holder.entry(g).or_default().insert(f, v);
                *census.entry("creator: body copied").or_default() += 1;
            }
        }
        // Each creator in `prepared` creates its copy.
        for (g, map) in &out.by_holder {
            if let Some((_, stmts)) = prepared.get_mut(g) {
                let n = point_at_copies(stmts, map);
                if n == 0 {
                    *census
                        .entry("creator: no creation site in its body")
                        .or_default() += 1;
                }
            }
        }
        log::debug!(target: "slots", "shared bodies: {census:?}");
        out
    }
}

// `f` and every function reachable from its body by `Function { id }`, never
// a module factory (rendered as its module, not inline).
fn descendants(
    f: u32,
    prepared: &BTreeMap<u32, Body>,
    factories: &BTreeMap<u32, u32>,
) -> BTreeSet<u32> {
    let mut seen = BTreeSet::from([f]);
    let mut work = vec![f];
    while let Some(id) = work.pop() {
        let Some((_, stmts)) = prepared.get(&id) else {
            continue;
        };
        for c in created(stmts) {
            if prepared.contains_key(&c) && !factories.contains_key(&c) && seen.insert(c) {
                work.push(c);
            }
        }
    }
    seen
}

fn created(stmts: &[Statement]) -> Vec<u32> {
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

fn slot_refs(stmts: &[Statement]) -> Vec<(&str, Option<SlotRef>)> {
    crate::analysis::closure::stamps::var_refs(stmts)
}

fn bound_names(stmts: &[Statement]) -> Vec<String> {
    struct Bound(Vec<String>);
    impl<'a> Visitor<'a> for Bound {
        fn visit_binding_def(&mut self, name: &'a str) {
            self.0.push(name.to_string());
        }
    }
    let mut b = Bound(Vec::new());
    for s in stmts {
        b.visit_statement(s);
    }
    b.0
}

// Every name `stmts` reads, writes or binds other than for a slot of `owner`.
fn other_names(stmts: &[Statement], owner: u32) -> HashSet<String> {
    let mut out: HashSet<String> = slot_refs(stmts)
        .into_iter()
        .filter(|(_, s)| s.is_none_or(|s| s.owner != owner))
        .map(|(n, _)| n.to_string())
        .collect();
    out.extend(bound_names(stmts));
    out
}

// What `stmts` calls each slot of `owner`.
fn owner_names(stmts: &[Statement], owner: u32) -> BTreeMap<u32, BTreeSet<String>> {
    let mut out: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
    for (name, slot) in slot_refs(stmts) {
        if let Some(s) = slot.filter(|s| s.owner == owner) {
            out.entry(s.slot).or_default().insert(name.to_string());
        }
    }
    out
}

// A copy of `id`'s body with `rep`'s slots renamed, and of every family
// member it creates that needs one. None when nothing in it changed.
#[allow(clippy::too_many_arguments)]
fn copy_renamed(
    id: u32,
    rep: u32,
    rename: &BTreeMap<u32, String>,
    prepared: &BTreeMap<u32, Body>,
    family: &BTreeSet<u32>,
    memo: &mut HashMap<u32, Option<u32>>,
    next: &mut u32,
    made: &mut Vec<(u32, u32, Body)>,
) -> Option<u32> {
    if let Some(done) = memo.get(&id) {
        return *done;
    }
    // A cycle back to `id` sees it unchanged.
    memo.insert(id, None);
    let (params, stmts) = &prepared[&id];
    let mut stmts = stmts.clone();
    let mut r = RenameSlots {
        rep,
        rename,
        changed: false,
    };
    r.visit_statement_list(&mut stmts);
    let mut changed = r.changed;
    let mut children: BTreeMap<u32, u32> = BTreeMap::new();
    for c in created(&stmts) {
        if family.contains(&c) && !children.contains_key(&c) {
            if let Some(v) = copy_renamed(c, rep, rename, prepared, family, memo, next, made) {
                children.insert(c, v);
            }
        }
    }
    if !children.is_empty() {
        point_at_copies(&mut stmts, &children);
        changed = true;
    }
    if !changed {
        return None;
    }
    let v = *next;
    *next += 1;
    made.push((v, id, (params.clone(), stmts)));
    memo.insert(id, Some(v));
    Some(v)
}

struct RenameSlots<'a> {
    rep: u32,
    rename: &'a BTreeMap<u32, String>,
    changed: bool,
}

impl RenameSlots<'_> {
    fn var(&mut self, v: &mut crate::ir::Var) {
        if let Some(s) = v.slot.filter(|s| s.owner == self.rep) {
            if let Some(new) = self.rename.get(&s.slot) {
                if v.name != *new {
                    v.name = new.clone();
                    self.changed = true;
                }
            }
        }
    }
}

impl MutVisitor for RenameSlots<'_> {
    fn visit_statement(&mut self, s: &mut Statement) {
        if let Statement::Let { name, .. } = s {
            self.var(name);
        }
        self.walk_statement(s);
    }
    fn visit_expression(&mut self, e: &mut Expression) {
        if let Expression::Value(Value::Binding(Binding::Variable(v))) = e {
            self.var(v);
        }
        self.walk_expression(e);
    }
    fn visit_assign_target(&mut self, t: &mut AssignTarget) {
        if let AssignTarget::Binding(Binding::Variable(v)) = t {
            self.var(v);
        }
        self.walk_assign_target(t);
    }
}

// Points each `Function { id }` in `map` at its copy; returns how many.
pub(super) fn point_at_copies(stmts: &mut Vec<Statement>, map: &BTreeMap<u32, u32>) -> usize {
    struct Point<'a>(&'a BTreeMap<u32, u32>, usize);
    impl MutVisitor for Point<'_> {
        fn visit_expression(&mut self, e: &mut Expression) {
            if let Expression::Function { id, .. } = e {
                if let Some(&v) = self.0.get(&id.0) {
                    *id = FunctionId(v);
                    self.1 += 1;
                }
            }
            self.walk_expression(e);
        }
    }
    let mut p = Point(map, 0);
    p.visit_statement_list(stmts);
    p.1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Var;

    fn read(name: &str, owner: u32, slot: u32) -> Expression {
        Expression::Value(Value::Binding(Binding::Variable(Var {
            name: name.into(),
            slot: Some(SlotRef { owner, slot }),
        })))
    }

    fn func(id: u32) -> Expression {
        Expression::Function {
            id: FunctionId(id),
            name: None,
            is_arrow: true,
            is_async: false,
            is_generator: false,
        }
    }

    // F3 reads slot 1 of F2 (the creator it was named against) and creates
    // F4, which reads it too; F5 reads nothing of F2. The copy for another
    // creator renames both reads and copies F4, not F5.
    #[test]
    fn a_copy_renames_the_shared_scope_through_nested_functions() {
        let mut prepared: BTreeMap<u32, Body> = BTreeMap::new();
        prepared.insert(
            3,
            (
                vec![],
                vec![
                    Statement::Expr(read("sum", 2, 1)),
                    Statement::Expr(func(4)),
                    Statement::Return(Some(func(5))),
                ],
            ),
        );
        prepared.insert(
            4,
            (vec![], vec![Statement::Return(Some(read("sum", 2, 1)))]),
        );
        prepared.insert(5, (vec![], vec![Statement::Return(Some(read("x", 9, 0)))]));
        let family = descendants(3, &prepared, &BTreeMap::new());
        assert_eq!(family, BTreeSet::from([3, 4, 5]));
        let rename = BTreeMap::from([(1, "closure_1".to_string())]);
        let mut memo = HashMap::new();
        let mut next = FIRST_VARIANT_ID;
        let mut made = Vec::new();
        let v = copy_renamed(
            3, 2, &rename, &prepared, &family, &mut memo, &mut next, &mut made,
        );
        assert!(v.is_some());
        let copied: BTreeMap<u32, u32> = made.iter().map(|(id, of, _)| (*of, *id)).collect();
        assert_eq!(copied.keys().copied().collect::<Vec<_>>(), vec![3, 4]);
        let body3 = &made.iter().find(|(_, of, _)| *of == 3).unwrap().2 .1;
        let names: Vec<&str> = slot_refs(body3).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["closure_1"]);
        assert_eq!(created(body3), vec![copied[&4], 5]);
    }

    #[test]
    fn nothing_to_rename_makes_no_copy() {
        let mut prepared: BTreeMap<u32, Body> = BTreeMap::new();
        prepared.insert(
            3,
            (vec![], vec![Statement::Return(Some(read("sum", 2, 1)))]),
        );
        let family = descendants(3, &prepared, &BTreeMap::new());
        let rename = BTreeMap::from([(1, "sum".to_string())]);
        let (mut memo, mut next, mut made) = (HashMap::new(), FIRST_VARIANT_ID, Vec::new());
        let v = copy_renamed(
            3, 2, &rename, &prepared, &family, &mut memo, &mut next, &mut made,
        );
        assert_eq!(v, None);
        assert!(made.is_empty());
    }
}

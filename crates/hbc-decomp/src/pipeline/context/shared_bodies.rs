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
// One creator can also create it on several scopes: hermesc inlined the
// factory more than once into one function, and each inlined copy builds its
// own environment. Those scopes are live at once, so they first get names of
// their own (`separate_copy_names`, before anything reads the names), and
// each creation site, told apart by the environment it was created on
// (`Expression::Function::env`), gets the copy for its scope.
//
// A scope is left with the shared body, and counted, when the copy cannot be
// made faithfully: the creator names a slot twice, or a rename would land on a
// different variable.

use super::PipelineContext;
use crate::analysis::ClosureContext;
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
    // creator -> ((shared function, creation scope) -> its copy there).
    pub by_holder: BTreeMap<u32, BTreeMap<(u32, u32), u32>>,
}

// The scope id a creation scope reference stands for, when it still exists.
fn owner_of(ctx: &ClosureContext, (g, level): (u32, Option<u32>)) -> Option<u32> {
    match level {
        None => Some(g),
        Some(l) => ctx.block_scopes.get(&(g, l)).copied(),
    }
}

// The scope a creation site in `g` creates `f` on: its only one there, or the
// one its environment tag names.
fn site_owner(ctx: &ClosureContext, f: u32, g: u32, env: Option<u32>) -> Option<u32> {
    let scopes = ctx.shared_creations.get(&f)?.get(&g)?;
    let owners: BTreeSet<u32> = scopes.iter().filter_map(|r| owner_of(ctx, *r)).collect();
    if owners.len() == 1 {
        return owners.into_iter().next();
    }
    let owner = match env? {
        0 => g,
        l if l >= crate::ir::NESTED_ENV_LEVEL_BASE => *ctx.block_scopes.get(&(g, l))?,
        _ => return None,
    };
    owners.contains(&owner).then_some(owner)
}

impl PipelineContext {
    // Gives the creation scopes of a shared function names a copy of its body
    // can take: apart from each other within one creator (copies of an
    // inlined factory are live at once, and one name is one binding), and
    // apart from the body's own locals (a copy reading `warn` for the slot
    // would read the body's `let warn` instead). The scope the body was named
    // against keeps its names. Renames the stamped references in the IR and
    // the context's decided names, before anything downstream reads either.
    // Returns how many slots it renamed.
    pub(super) fn separate_copy_names(&mut self) -> usize {
        let Some(ctx) = self.closure_ctx.as_ref() else {
            return 0;
        };
        let mut renames: BTreeMap<SlotRef, String> = BTreeMap::new();
        let mut in_use_of: HashMap<u32, HashSet<String>> = HashMap::new();
        let factories = &self.registry.function_to_module;
        for (&f, creators) in &ctx.shared_creations {
            let Some(all) = creators
                .iter()
                .flat_map(|(&g, scopes)| scopes.iter().map(move |r| (g, *r)))
                .map(|(g, r)| Some((g, owner_of(ctx, r)?)))
                .collect::<Option<BTreeSet<(u32, u32)>>>()
            else {
                continue;
            };
            let scopes: HashSet<u32> = all.iter().map(|(_, o)| *o).collect();
            // The body and the nested functions it creates: what every
            // creation site renders.
            let mut family = vec![f];
            let mut seen = HashSet::from([f]);
            let mut i = 0;
            while i < family.len() {
                if let Some(b) = self.all_ir.get(&family[i]) {
                    for c in created(b) {
                        if !factories.contains_key(&c)
                            && self.all_ir.contains_key(&c)
                            && seen.insert(c)
                        {
                            family.push(c);
                        }
                    }
                }
                i += 1;
            }
            // The scope the body was named against keeps its names; every
            // other name the family uses is one a creation scope must not
            // have, or the copy for it would capture that variable.
            let mut named: BTreeSet<u32> = BTreeSet::new();
            let mut locals: HashSet<String> = HashSet::new();
            for id in &family {
                let body = &self.all_ir[id];
                for (name, slot) in slot_refs(body) {
                    match slot.filter(|s| scopes.contains(&s.owner)) {
                        Some(s) => {
                            named.insert(s.owner);
                        }
                        None => {
                            locals.insert(name.to_string());
                        }
                    }
                }
                locals.extend(bound_names(body));
            }
            let rep = (named.len() == 1).then(|| *named.iter().next().unwrap());
            let mut by_creator: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
            for &(g, owner) in &all {
                by_creator.entry(g).or_default().push(owner);
            }
            for (g, mut owners) in by_creator {
                owners.sort_by_key(|o| (Some(*o) != rep, *o));
                let Some(body) = self.all_ir.get(&g) else {
                    continue;
                };
                let in_use = in_use_of.entry(g).or_insert_with(|| {
                    let mut set: HashSet<String> = slot_refs(body)
                        .into_iter()
                        .map(|(n, _)| n.to_string())
                        .collect();
                    set.extend(bound_names(body));
                    set
                });
                let mut claimed: HashSet<String> = HashSet::new();
                for owner in owners {
                    for (slot, names) in owner_names(body, owner) {
                        let key = SlotRef { owner, slot };
                        let name = match renames.get(&key) {
                            Some(n) => n.clone(),
                            None if names.len() == 1 => names.into_iter().next().unwrap(),
                            None => continue,
                        };
                        if Some(owner) == rep
                            || (!locals.contains(&name) && claimed.insert(name.clone()))
                        {
                            claimed.insert(name);
                            continue;
                        }
                        let mut avoid: HashSet<String> = in_use.clone();
                        avoid.extend(locals.iter().cloned());
                        let fresh = fresh_name(&name, &avoid);
                        in_use.insert(fresh.clone());
                        claimed.insert(fresh.clone());
                        renames.insert(key, fresh);
                    }
                }
            }
        }
        if renames.is_empty() {
            return 0;
        }
        let n = renames.len();
        let mut r = RenameStamped { renames: &renames };
        for stmts in self.all_ir.values_mut() {
            r.visit_statement_list(stmts);
        }
        if let Some(ctx) = self.closure_ctx.as_mut() {
            for (key, name) in &renames {
                if let Some(info) = ctx.function_closures.get_mut(&key.owner) {
                    info.names.insert(key.slot, name.clone());
                }
            }
        }
        log::debug!(target: "slots", "shared bodies: {n} creation-scope slots renamed apart");
        n
    }

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
        let mut stamps_of: HashMap<u32, HashMap<String, BTreeSet<SlotRef>>> = HashMap::new();
        let mut next = FIRST_VARIANT_ID;
        for (&f, creators) in &ctx.shared_creations {
            if !prepared.contains_key(&f) {
                *census.entry("no body").or_default() += 1;
                continue;
            }
            let wanted_count: usize = creators.values().map(BTreeSet::len).sum();
            let owners: BTreeSet<(u32, u32)> = creators
                .iter()
                .flat_map(|(&g, scopes)| scopes.iter().map(move |r| (g, *r)))
                .filter_map(|(g, r)| Some((g, owner_of(ctx, r)?)))
                .collect();
            if owners.len() != wanted_count {
                *census.entry("a creation scope is gone").or_default() += 1;
                continue;
            }
            let scopes: HashSet<u32> = owners.iter().map(|(_, o)| *o).collect();
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
            for &(g, owner) in &owners {
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
                let stamps = stamps_of.entry(g).or_insert_with(|| name_stamps(creator));
                // A slot the copy cannot read under a name of the creator's
                // gets a name nothing has: undeclared, which fails loudly,
                // where the shared body's name would silently read another
                // scope's binding (or another variable of the creator).
                let mut rename: BTreeMap<u32, String> = BTreeMap::new();
                let mut avoid: Option<HashSet<String>> = None;
                let mut unreadable =
                    |old: &str, why: &'static str, census: &mut BTreeMap<&'static str, usize>| {
                        *census.entry(why).or_default() += 1;
                        log::trace!(target: "slots", "shared body F{f} in F{g} (scope {owner}): `{old}` unreadable: {why}");
                        let avoid = avoid.get_or_insert_with(|| {
                            let mut a = taken.clone();
                            a.extend(others.iter().cloned());
                            a
                        });
                        let fresh = fresh_name(old, avoid);
                        avoid.insert(fresh.clone());
                        fresh
                    };
                for (slot, old) in wanted {
                    let new = match names.get(slot) {
                        Some(set) if set.len() == 1 => {
                            let new = set.iter().next().unwrap();
                            if new != old && taken.contains(new) {
                                unreadable(old, "slot: the creator's name is another variable in the body", &mut census)
                            } else {
                                new.clone()
                            }
                        }
                        Some(_) => unreadable(old, "slot: the creator names it twice", &mut census),
                        // The creator never mentions the slot by its stamp (only
                        // closures write it, nothing does, or a pass dropped the
                        // stamp of its own reference): the body's name stays,
                        // unless the creator stamps that name as another slot.
                        // An unstamped use of the name is no evidence either
                        // way, and is most often the slot itself (a creator's
                        // `const fn = ...` whose stamp went with a rewrite).
                        None if stamps.get(old.as_str()).is_some_and(|set| {
                            set.iter().any(|s| !(s.owner == owner && s.slot == *slot))
                        }) =>
                        {
                            unreadable(
                                old,
                                "slot: never named by the creator, which stamps the body's name as another slot",
                                &mut census,
                            )
                        }
                        None => old.clone(),
                    };
                    rename.insert(*slot, new);
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
                out.by_holder.entry(g).or_default().insert((f, owner), v);
                *census.entry("creator: body copied").or_default() += 1;
            }
        }
        // Each creation site in `prepared` creates its scope's copy.
        for (g, map) in &out.by_holder {
            if let Some((_, stmts)) = prepared.get_mut(g) {
                let n = point_at_copies(ctx, *g, stmts, map);
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

// Every slot `stmts` stamps each name with.
fn name_stamps(stmts: &[Statement]) -> HashMap<String, BTreeSet<SlotRef>> {
    let mut out: HashMap<String, BTreeSet<SlotRef>> = HashMap::new();
    for (name, slot) in slot_refs(stmts) {
        if let Some(s) = slot {
            out.entry(name.to_string()).or_default().insert(s);
        }
    }
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
        point_children(&mut stmts, &children);
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

// Points each creation site in creator `g` at the copy for the scope it
// creates the function on; returns how many.
pub(super) fn point_at_copies(
    ctx: &ClosureContext,
    g: u32,
    stmts: &mut Vec<Statement>,
    map: &BTreeMap<(u32, u32), u32>,
) -> usize {
    struct Point<'a> {
        ctx: &'a ClosureContext,
        g: u32,
        map: &'a BTreeMap<(u32, u32), u32>,
        n: usize,
    }
    impl MutVisitor for Point<'_> {
        fn visit_expression(&mut self, e: &mut Expression) {
            if let Expression::Function { id, env, .. } = e {
                let v = site_owner(self.ctx, id.0, self.g, *env)
                    .and_then(|owner| self.map.get(&(id.0, owner)));
                if let Some(&v) = v {
                    *id = FunctionId(v);
                    self.n += 1;
                }
            }
            self.walk_expression(e);
        }
    }
    let mut p = Point { ctx, g, map, n: 0 };
    p.visit_statement_list(stmts);
    p.n
}

// Points each `Function { id }` in `map` at its copy, whatever its scope:
// inside a copy, a nested family member has one reading only.
fn point_children(stmts: &mut Vec<Statement>, map: &BTreeMap<u32, u32>) {
    struct Point<'a>(&'a BTreeMap<u32, u32>);
    impl MutVisitor for Point<'_> {
        fn visit_expression(&mut self, e: &mut Expression) {
            if let Expression::Function { id, .. } = e {
                if let Some(&v) = self.0.get(&id.0) {
                    *id = FunctionId(v);
                }
            }
            self.walk_expression(e);
        }
    }
    Point(map).visit_statement_list(stmts);
}

// `name` made distinct from everything in `in_use`.
fn fresh_name(name: &str, in_use: &HashSet<String>) -> String {
    let sep = if name.ends_with(|c: char| c.is_ascii_digit()) {
        "_"
    } else {
        ""
    };
    (2u32..)
        .map(|k| format!("{name}{sep}{k}"))
        .find(|c| !in_use.contains(c))
        .unwrap()
}

// Renames every stamped reference to a slot in `renames`.
struct RenameStamped<'a> {
    renames: &'a BTreeMap<SlotRef, String>,
}

impl RenameStamped<'_> {
    fn var(&mut self, v: &mut crate::ir::Var) {
        if let Some(new) = v.slot.and_then(|s| self.renames.get(&s)) {
            v.name = new.clone();
        }
    }
}

impl MutVisitor for RenameStamped<'_> {
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
            env: None,
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

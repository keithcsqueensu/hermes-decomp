// The whole-program scope graph (V98_ENV_PLAN.md E3).
//
// `ir::env_values` says, per function, which environment each closure is
// created with, what each created environment is built on, and which
// environments are parked in which slots, all relative to that function. This
// resolves those to scopes: a function's own environment, or one a function
// created past its own. A function's chain is the scope it was created with,
// that scope's parent, and so on; a slot-held environment is whatever the
// functions running on that scope parked in that slot. The facts depend on
// each other (a closure created with a slot-held environment needs the parked
// value, which needs the parker's chain), so the graph is built to a fixpoint.
//
// An environment that resolves to nothing, or to different scopes along
// different facts, stays unresolved and is counted: never guessed.

use std::collections::{BTreeMap, BTreeSet};

use crate::ir::env_values::{Env, EnvFacts};

// Deeper than any real chain; a malformed parent cycle stops here.
const MAX_HOPS: u32 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    // A function's own environment.
    Fn(u32),
    // An environment function `.0` created past its own, at offset `.1`.
    Created(u32, u32),
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Scope::Fn(id) => write!(f, "F{id}"),
            Scope::Created(id, site) => write!(f, "F{id}@{site}"),
        }
    }
}

// A fact's value: the scopes the facts give it, or too many to follow.
// Hermes shares one compiled function between creators, so a function can be
// created with several scopes; a level they all reach the same way is still
// one answer (`ScopeGraph::chain`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Known {
    Set(BTreeSet<Scope>),
    TooMany,
}

// Past this many scopes a set is not followed. Hermes inlines a small
// function into every caller, so one compiled closure can have hundreds of
// creators (Discord's `dispatchOrResetSocket` callback: 187); at 16 such a
// function fell to the level model and read one creator's names in all.
const MAX_SET: usize = 1024;

#[derive(Debug, Default)]
pub struct ScopeGraph {
    // Function -> the scopes it was created with.
    creation: BTreeMap<u32, Known>,
    // Created scope -> what it was built on, relative to its creator (None:
    // a top-level environment, no parent).
    built_on: BTreeMap<(u32, u32), Option<Env>>,
    // (scope, slot) -> the scopes parked there.
    parked: BTreeMap<(Scope, u32), Known>,
    // How deep `resolve` is in itself. A created scope's parent is resolved
    // through its creator's chain, which can lead back to that scope through
    // facts that disagree; past the bound the answer is unresolved.
    depth: std::cell::Cell<u32>,
}

const MAX_RESOLVE_DEPTH: u32 = 128;

fn learn<K: Ord>(map: &mut BTreeMap<K, Known>, key: K, value: Scope) -> bool {
    match map
        .entry(key)
        .or_insert_with(|| Known::Set(BTreeSet::new()))
    {
        Known::TooMany => false,
        known @ Known::Set(_) => {
            let Known::Set(set) = known else {
                unreachable!()
            };
            if !set.insert(value) {
                return false;
            }
            if set.len() > MAX_SET {
                *known = Known::TooMany;
            }
            true
        }
    }
}

// The one scope in `known`, if that is what it is.
fn single(known: Option<&Known>) -> Option<Scope> {
    match known? {
        Known::Set(set) if set.len() == 1 => set.iter().next().copied(),
        _ => None,
    }
}

impl ScopeGraph {
    pub fn build<'a>(facts: impl Iterator<Item = (u32, &'a EnvFacts)> + Clone) -> Self {
        let mut graph = ScopeGraph::default();
        for (f, fx) in facts.clone() {
            for (site, parent) in &fx.created {
                graph.built_on.insert((f, *site), parent.clone());
            }
        }
        // Each pass can only add or contradict; the bound is for safety.
        for _ in 0..32 {
            let mut changed = false;
            for (f, fx) in facts.clone() {
                for (child, env) in &fx.closures {
                    if *child == f {
                        continue;
                    }
                    if let Some(s) = graph.resolve(f, env) {
                        changed |= learn(&mut graph.creation, *child, s);
                    }
                }
                for (through, slot, value) in &fx.parked {
                    if let (Some(at), Some(v)) =
                        (graph.resolve(f, through), graph.resolve(f, value))
                    {
                        changed |= learn(&mut graph.parked, (at, *slot), v);
                    }
                }
            }
            if !changed {
                break;
            }
        }
        graph
    }

    // The scope `f` was created with, its level 1, when there is one.
    pub fn creation(&self, f: u32) -> Option<Scope> {
        single(self.creation.get(&f))
    }

    // Every scope `f` was created with, when the facts give a set.
    pub fn creations(&self, f: u32) -> Option<&BTreeSet<Scope>> {
        match self.creation.get(&f)? {
            Known::Set(set) => Some(set),
            Known::TooMany => None,
        }
    }

    // The scopes `scope`'s environment can be built on.
    fn parents(&self, scope: Scope) -> Option<BTreeSet<Scope>> {
        match scope {
            Scope::Fn(f) => match self.creation.get(&f)? {
                Known::Set(set) => Some(set.clone()),
                Known::TooMany => None,
            },
            Scope::Created(f, site) => {
                let on = self.built_on.get(&(f, site))?.as_ref()?;
                Some(BTreeSet::from([self.resolve(f, on)?]))
            }
        }
    }

    // `n` levels up from every scope in `from`; one answer only if every way
    // up agrees.
    fn up_all(&self, mut from: BTreeSet<Scope>, n: u32) -> Option<Scope> {
        if n > MAX_HOPS {
            return None;
        }
        for _ in 0..n {
            let mut next = BTreeSet::new();
            for s in &from {
                next.extend(self.parents(*s)?);
            }
            if next.is_empty() || next.len() > MAX_SET {
                return None;
            }
            from = next;
        }
        (from.len() == 1).then(|| *from.iter().next().unwrap())
    }

    fn up(&self, scope: Scope, n: u32) -> Option<Scope> {
        self.up_all(BTreeSet::from([scope]), n)
    }

    // The one scope `scope` is built on, when there is one.
    pub fn parent(&self, scope: Scope) -> Option<Scope> {
        self.up(scope, 1)
    }

    // Function `f`'s closure level `n` (1: the scope it was created with).
    pub fn chain(&self, f: u32, n: u32) -> Option<Scope> {
        if n == 0 {
            return Some(Scope::Fn(f));
        }
        self.up_all(BTreeSet::from([Scope::Fn(f)]), n)
    }

    // The scope `env`, as function `f` sees it, is.
    pub fn resolve(&self, f: u32, env: &Env) -> Option<Scope> {
        if self.depth.get() >= MAX_RESOLVE_DEPTH {
            return None;
        }
        self.depth.set(self.depth.get() + 1);
        let scope = self.resolve_inner(f, env);
        self.depth.set(self.depth.get() - 1);
        scope
    }

    fn resolve_inner(&self, f: u32, env: &Env) -> Option<Scope> {
        match env {
            Env::Own => Some(Scope::Fn(f)),
            Env::Closure(n) => self.chain(f, *n),
            Env::Created { site, .. } => Some(Scope::Created(f, *site)),
            Env::Up(inner, n) => self.up(self.resolve(f, inner)?, *n),
            Env::InSlot(inner, slot) => {
                let at = self.resolve(f, inner)?;
                single(self.parked.get(&(at, *slot)))
            }
            Env::Unknown => None,
        }
    }

    // (functions created with more than one scope, slots parked with more
    // than one scope). The first are often harmless: a level every creator
    // reaches the same way still resolves.
    pub fn conflicts(&self) -> (usize, usize) {
        fn many<K>(m: &BTreeMap<K, Known>) -> usize {
            m.values()
                .filter(|k| !matches!(k, Known::Set(s) if s.len() <= 1))
                .count()
        }
        (many(&self.creation), many(&self.parked))
    }

    // Why `chain(f, n)` is unresolved: the first link that is missing.
    pub fn why_unresolved(&self, f: u32, n: u32) -> &'static str {
        let mut from = BTreeSet::from([Scope::Fn(f)]);
        for _ in 0..n.min(MAX_HOPS) {
            let mut next = BTreeSet::new();
            for s in &from {
                match self.parents(*s) {
                    Some(p) => next.extend(p),
                    None => {
                        return match s {
                            Scope::Fn(g) => match self.creation.get(g) {
                                None => "a function no fact creates",
                                Some(_) => "a function created with too many scopes",
                            },
                            Scope::Created(g, site) => match self.built_on.get(&(*g, *site)) {
                                Some(None) => "past a top-level environment",
                                _ => "a created scope whose parent is unresolved",
                            },
                        }
                    }
                }
            }
            from = next;
        }
        "creators that reach different scopes"
    }

    pub fn functions_created(&self) -> BTreeSet<u32> {
        self.creation.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Vec<(u32, EnvFacts)> {
        // tests/fixtures/env/generator_states at v98, by hand: global 0
        // creates make 1, make creates the wrapper 2 on its own environment,
        // the wrapper creates the body 3 on its own; the body builds a scope
        // on its level 2 at @466, parks it in the wrapper's slot 1, and
        // creates the closures 4 and 5 with whatever slot 1 holds.
        let held = Env::InSlot(Box::new(Env::Closure(1)), 1);
        let scope = Env::Created {
            site: 466,
            parent: Some(Box::new(Env::Closure(2))),
        };
        vec![
            (
                0,
                EnvFacts {
                    closures: vec![(1, Env::Unknown)],
                    ..Default::default()
                },
            ),
            (
                1,
                EnvFacts {
                    closures: vec![(2, Env::Own)],
                    ..Default::default()
                },
            ),
            (
                2,
                EnvFacts {
                    closures: vec![(3, Env::Own)],
                    ..Default::default()
                },
            ),
            // The closures come first, as the generator's layout has them.
            (
                3,
                EnvFacts {
                    closures: vec![(5, held.clone()), (4, held)],
                    created: vec![(466, Some(Env::Closure(2)))],
                    parked: vec![
                        (Env::Closure(1), 1, scope),
                        (Env::Closure(1), 2, Env::Closure(2)),
                    ],
                    ..Default::default()
                },
            ),
        ]
    }

    #[test]
    fn a_closure_created_with_a_parked_scope_reaches_through_it() {
        let facts = facts();
        let graph = ScopeGraph::build(facts.iter().map(|(f, x)| (*f, x)));
        assert_eq!(graph.creation(3), Some(Scope::Fn(2)));
        assert_eq!(graph.creation(5), Some(Scope::Created(3, 466)));
        // The scope's parent is the body's level 2: make's environment, not
        // the wrapper's.
        assert_eq!(graph.chain(5, 2), Some(Scope::Fn(1)));
        assert_eq!(
            graph.resolve(3, &Env::InSlot(Box::new(Env::Closure(1)), 2)),
            Some(Scope::Fn(1))
        );
        // Global's own creation is unknown, so make's chain stops at level 1.
        assert_eq!(graph.chain(5, 3), None);
        assert_eq!(graph.conflicts(), (0, 0));
    }

    #[test]
    fn facts_that_disagree_leave_a_slot_unresolved() {
        let mut facts = facts();
        facts[3].1.parked.push((Env::Closure(1), 1, Env::Own));
        let graph = ScopeGraph::build(facts.iter().map(|(f, x)| (*f, x)));
        assert_eq!(graph.creation(5), None);
        assert_eq!(graph.conflicts().1, 1);
        assert_eq!(graph.chain(5, 1), None);
    }

    // One compiled function created by two siblings: its level 1 differs per
    // creator, its level 2 (their shared parent) does not.
    #[test]
    fn a_function_two_creators_share_resolves_where_they_agree() {
        let facts = [
            (
                1,
                EnvFacts {
                    closures: vec![(2, Env::Own), (3, Env::Own)],
                    ..Default::default()
                },
            ),
            (
                2,
                EnvFacts {
                    closures: vec![(9, Env::Own)],
                    ..Default::default()
                },
            ),
            (
                3,
                EnvFacts {
                    closures: vec![(9, Env::Own)],
                    ..Default::default()
                },
            ),
        ];
        let graph = ScopeGraph::build(facts.iter().map(|(f, x)| (*f, x)));
        assert_eq!(graph.creation(9), None);
        assert_eq!(graph.chain(9, 1), None);
        assert_eq!(graph.chain(9, 2), Some(Scope::Fn(1)));
        assert_eq!(graph.conflicts(), (1, 0));
        assert_eq!(
            graph.why_unresolved(9, 1),
            "creators that reach different scopes"
        );
    }
}

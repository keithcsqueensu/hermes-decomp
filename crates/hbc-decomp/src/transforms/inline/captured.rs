// The variable names each function's nested functions read or write.
//
// After closure resolution a captured slot is a plain name in every body that
// touches it, so a pass looking at one body alone sees a store nobody reads
// and drops it. The map answers, for a function, which names its descendants
// still need bound.

use std::collections::{BTreeMap, HashSet};

use crate::ir::{AssignTarget, Binding, Expression, Statement, Value, Visitor};

pub fn names_used_by_descendants(
    all_ir: &BTreeMap<u32, Vec<Statement>>,
    parent_of: &BTreeMap<u32, u32>,
) -> BTreeMap<u32, HashSet<String>> {
    let mut out: BTreeMap<u32, HashSet<String>> = BTreeMap::new();
    for (&fid, stmts) in all_ir {
        let mut refs: HashSet<String> = HashSet::new();
        {
            let mut r = Refs(&mut refs);
            for s in stmts {
                r.visit_statement(s);
            }
        }
        if refs.is_empty() {
            continue;
        }
        let mut cur = fid;
        let mut hops = 0;
        while let Some(&parent) = parent_of.get(&cur) {
            if parent == cur || hops > 64 {
                break;
            }
            out.entry(parent).or_default().extend(refs.iter().cloned());
            cur = parent;
            hops += 1;
        }
    }
    out
}

// The names each function's nested functions use for a variable of an
// enclosing scope, the subset of `names_used_by_descendants` a declaration
// must not shadow: a name stamped with a slot owned above the function, or an
// unstamped name the nested function reads but never assigns. A name a nested
// function assigns unstamped is its own local (`const self = this` in a
// callback), and counting it kept superagent's `Request` constructor from
// declaring its own `self`, which then wrote the module's `root` slot.
pub fn outer_names_used_by_descendants(
    all_ir: &BTreeMap<u32, Vec<Statement>>,
    parent_of: &BTreeMap<u32, u32>,
    scope_function: impl Fn(u32) -> u32,
) -> BTreeMap<u32, HashSet<String>> {
    let mut out: BTreeMap<u32, HashSet<String>> = BTreeMap::new();
    for (&fid, stmts) in all_ir {
        let mut uses = Uses::default();
        for s in stmts {
            uses.visit_statement(s);
        }
        // (name, the function owning the slot it is stamped with)
        let stamped: Vec<(String, u32)> = uses
            .stamped
            .into_iter()
            .map(|(n, owner)| (n, scope_function(owner)))
            .collect();
        let free: Vec<String> = uses
            .unstamped_reads
            .iter()
            .filter(|n| !uses.unstamped_writes.contains(*n))
            .cloned()
            .collect();
        if stamped.is_empty() && free.is_empty() {
            continue;
        }
        let mut cur = fid;
        let mut hops = 0;
        while let Some(&parent) = parent_of.get(&cur) {
            if parent == cur || hops > 64 {
                break;
            }
            let set = out.entry(parent).or_default();
            set.extend(free.iter().cloned());
            // A slot of `parent` itself, or of a function below it, is not an
            // enclosing variable for `parent`.
            set.extend(
                stamped
                    .iter()
                    .filter(|(_, owner)| *owner != parent && !is_below(*owner, parent, parent_of))
                    .map(|(n, _)| n.clone()),
            );
            cur = parent;
            hops += 1;
        }
    }
    out
}

// `f` is nested, at any depth, inside `ancestor`.
fn is_below(f: u32, ancestor: u32, parent_of: &BTreeMap<u32, u32>) -> bool {
    let mut cur = f;
    let mut hops = 0;
    while let Some(&p) = parent_of.get(&cur) {
        if p == ancestor {
            return true;
        }
        if p == cur || hops > 64 {
            return false;
        }
        cur = p;
        hops += 1;
    }
    false
}

#[derive(Default)]
struct Uses {
    stamped: HashSet<(String, u32)>,
    unstamped_reads: HashSet<String>,
    unstamped_writes: HashSet<String>,
}

impl Uses {
    fn note(&mut self, v: &crate::ir::Var, write: bool) {
        match v.slot {
            Some(s) => {
                self.stamped.insert((v.name.clone(), s.owner));
            }
            None if write => {
                self.unstamped_writes.insert(v.name.clone());
            }
            None => {
                self.unstamped_reads.insert(v.name.clone());
            }
        }
    }
}

impl<'b> Visitor<'b> for Uses {
    fn visit_assign_target(&mut self, t: &'b AssignTarget) {
        if let AssignTarget::Binding(Binding::Variable(n)) = t {
            self.note(n, true);
        }
        self.walk_assign_target(t);
    }
    fn visit_expression(&mut self, e: &'b Expression) {
        if let Expression::Value(Value::Binding(Binding::Variable(n))) = e {
            self.note(n, false);
        }
        self.walk_expression(e);
    }
    fn visit_statement(&mut self, s: &'b Statement) {
        if let Statement::Let { name, .. } = s {
            self.note(name, true);
        }
        self.walk_statement(s);
    }
}

struct Refs<'a>(&'a mut HashSet<String>);

impl<'b> Visitor<'b> for Refs<'_> {
    fn visit_assign_target(&mut self, t: &'b AssignTarget) {
        if let AssignTarget::Binding(Binding::Variable(n)) = t {
            self.0.insert(n.name.clone());
        }
        self.walk_assign_target(t);
    }
    fn visit_expression(&mut self, e: &'b Expression) {
        if let Expression::Value(Value::Binding(Binding::Variable(n))) = e {
            self.0.insert(n.name.clone());
        }
        self.walk_expression(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nested_functions_own_local_is_not_an_outer_name() {
        let var = |n: &str, slot: Option<(u32, u32)>| crate::ir::Var {
            name: n.into(),
            slot: slot.map(|(owner, slot)| crate::ir::SlotRef { owner, slot }),
        };
        let read = |v: crate::ir::Var| {
            Statement::Expr(Expression::Value(Value::Binding(Binding::Variable(v))))
        };
        let mut all_ir = BTreeMap::new();
        all_ir.insert(1, vec![]);
        all_ir.insert(2, vec![]);
        all_ir.insert(
            3,
            vec![
                // its own `self = this`
                Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable(var("self", None))),
                    value: Expression::Value(Value::This),
                },
                read(var("self", None)),
                // a slot of the module, function 1
                read(var("root", Some((1, 3)))),
                // a slot of its parent, function 2
                read(var("req", Some((2, 0)))),
                // a free read of something above
                read(var("config", None)),
            ],
        );
        let mut parent_of = BTreeMap::new();
        parent_of.insert(2, 1);
        parent_of.insert(3, 2);
        let outer = outer_names_used_by_descendants(&all_ir, &parent_of, |s| s);
        let two = &outer[&2];
        assert!(!two.contains("self"));
        assert!(two.contains("root"));
        assert!(
            !two.contains("req"),
            "function 2's own slot is not outer to it"
        );
        assert!(two.contains("config"));
        assert!(!outer[&1].contains("root"), "function 1's own slot");
    }

    #[test]
    fn a_name_read_by_a_grandchild_reaches_both_ancestors() {
        let mut all_ir = BTreeMap::new();
        all_ir.insert(1, vec![]);
        all_ir.insert(2, vec![]);
        all_ir.insert(
            3,
            vec![Statement::Expr(Expression::Value(Value::Binding(
                Binding::Variable("set".into()),
            )))],
        );
        let mut parent_of = BTreeMap::new();
        parent_of.insert(2, 1);
        parent_of.insert(3, 2);
        let used = names_used_by_descendants(&all_ir, &parent_of);
        assert!(used[&1].contains("set"));
        assert!(used[&2].contains("set"));
        assert!(!used.contains_key(&3));
    }
}

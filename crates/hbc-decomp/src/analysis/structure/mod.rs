mod conversion;
mod exceptions;
mod loops;
mod recovery;

use crate::analysis::loops::LoopInfo;
use crate::ir::{BlockId, CfgExceptionHandler, Expression, Statement};
use std::collections::{BTreeMap, HashSet};

// Context passed through recursive structure recovery, avoiding 8-parameter functions.
pub(crate) struct RecoveryCtx<'a> {
    pub cfg: &'a crate::ir::CFG,
    pub loops: &'a [LoopInfo],
    pub visited: &'a mut HashSet<BlockId>,
    // Blocks already emitted somewhere, as opposed to merely marked visited
    // (a merge point or loop exit held back to be emitted after its region).
    pub emitted: &'a mut HashSet<BlockId>,
    pub try_starts: &'a BTreeMap<BlockId, &'a CfgExceptionHandler>,
}

#[derive(Debug, Clone)]
pub enum Structure {
    Block(BlockId, Vec<Statement>),
    Sequence(Vec<Structure>),
    If {
        condition: Expression,
        then_: Box<Structure>,
        else_: Box<Structure>,
    },
    While {
        condition: Expression,
        body: Box<Structure>,
    },
    DoWhile {
        body: Box<Structure>,
        condition: Expression,
    },
    For {
        init: Box<Structure>,
        condition: Expression,
        update: Box<Structure>,
        body: Box<Structure>,
    },
    Switch {
        discriminant: Expression,
        cases: Vec<(Expression, Structure)>,
        default: Box<Structure>,
    },
    TryCatch {
        try_body: Box<Structure>,
        catch_param: Option<String>,
        catch_body: Box<Structure>,
    },
    Return(Option<Expression>),
    Break(Option<String>),
    Continue(Option<String>),
    Label(String, Box<Structure>),
}

pub struct StructureAnalysis {
    pub root: Structure,
    pub loops: Vec<LoopInfo>,
}

impl StructureAnalysis {
    pub fn analyze(cfg: &crate::ir::CFG) -> Self {
        let (root, loops) = recovery::analyze(cfg);
        StructureAnalysis { root, loops }
    }
}

#[cfg(test)]
mod switch_tests {
    use super::StructureAnalysis;
    use crate::ir::{
        AssignTarget, BinaryOp, Binding, Constant, Expression, Statement, Terminator, Value, CFG,
    };

    fn var(n: &str) -> Expression {
        Expression::Value(Value::Binding(Binding::Variable(n.into())))
    }

    fn set(n: &str, value: Expression) -> Statement {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(n.into())),
            value,
        }
    }

    fn text(stmts: &[Statement]) -> String {
        format!("{stmts:?}")
    }

    // `for (const t of tokens) switch (t) { case "a": ...; case "b": ...;
    // default: throw }`: the loop's header is the switch. It was dropped with
    // everything only the switch reaches, and the cases' shared tail (the
    // counter increment and the loop test) went to the first case alone.
    #[test]
    fn a_loop_whose_header_is_a_switch_keeps_its_cases_and_its_test() {
        let mut cfg = CFG::new();
        let header = cfg.create_block();
        let case_a = cfg.create_block();
        let case_b = cfg.create_block();
        let bad = cfg.create_block();
        let latch = cfg.create_block();
        let done = cfg.create_block();
        let entry = cfg.entry;
        cfg.get_mut(entry)
            .unwrap()
            .push(set("i", Expression::constant(Constant::Integer(0))));
        cfg.get_mut(entry)
            .unwrap()
            .set_terminator(Terminator::Jump(header));
        cfg.get_mut(header)
            .unwrap()
            .set_terminator(Terminator::Switch {
                value: var("t"),
                cases: vec![
                    (Expression::constant(Constant::String("a".into())), case_a),
                    (Expression::constant(Constant::String("b".into())), case_b),
                ],
                default: bad,
            });
        for (b, v) in [(case_a, "A"), (case_b, "B")] {
            cfg.get_mut(b)
                .unwrap()
                .push(set("s", Expression::constant(Constant::String(v.into()))));
            cfg.get_mut(b)
                .unwrap()
                .set_terminator(Terminator::Jump(latch));
        }
        cfg.get_mut(bad)
            .unwrap()
            .set_terminator(Terminator::Throw(Expression::constant(Constant::String(
                "bad".into(),
            ))));
        cfg.get_mut(latch).unwrap().push(set(
            "i",
            Expression::binary(
                BinaryOp::Add,
                var("i"),
                Expression::constant(Constant::Integer(1)),
            ),
        ));
        cfg.get_mut(latch)
            .unwrap()
            .set_terminator(Terminator::Branch {
                condition: Expression::binary(BinaryOp::Lt, var("i"), var("n")),
                true_target: header,
                false_target: done,
            });
        cfg.get_mut(done)
            .unwrap()
            .set_terminator(Terminator::Return(Some(var("s"))));

        let analysis = StructureAnalysis::analyze(&cfg);
        let stmts = analysis.root.to_statements(&cfg);
        let all = text(&stmts);
        assert!(
            all.contains("String(\"A\")") && all.contains("String(\"B\")"),
            "{all}"
        );
        assert!(all.contains("Throw"), "the default case is gone: {all}");
        // One increment, after the switch, not inside its first case.
        assert_eq!(all.matches("op: Add").count(), 1, "{all}");
        let Some(Statement::While { body, .. }) =
            stmts.iter().find(|s| matches!(s, Statement::While { .. }))
        else {
            panic!("no loop: {all}");
        };
        let switch_at = body
            .iter()
            .position(|s| matches!(s, Statement::Switch { .. }));
        let add_at = body
            .iter()
            .position(|s| text(std::slice::from_ref(s)).contains("op: Add"));
        assert!(
            switch_at < add_at,
            "the increment is not after the switch: {body:?}"
        );
        // The loop's test leaves the loop, not just the switch: by name, or
        // by returning.
        let Some(Statement::If { else_body, .. }) = body.last() else {
            panic!("the loop test is not last: {body:?}");
        };
        assert!(
            matches!(
                else_body.as_slice(),
                [Statement::Break(Some(_))] | [.., Statement::Return(_)]
            ),
            "the exit does not leave the loop: {else_body:?}"
        );
    }
}

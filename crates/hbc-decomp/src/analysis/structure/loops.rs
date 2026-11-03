use super::recovery::recover_structure;
use super::{RecoveryCtx, Structure};
use crate::analysis::loops::LoopInfo;
use crate::ir::{BlockId, Expression, Terminator};

// A test against `undefined` is the done check of for-of and for-in, which have
// recognisers of their own that read the hoisted form, a `return` inside or not.
fn tests_undefined(condition: &Expression) -> bool {
    let is_undefined = |e: &Expression| {
        matches!(
            e,
            Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::Undefined))
        )
    };
    match condition {
        Expression::Unary { operand, .. } => tests_undefined(operand),
        Expression::Binary { left, right, .. } => is_undefined(left) || is_undefined(right),
        _ => false,
    }
}

// Recovers a loop's body with its exit taken. Every path out of the body is then a `break`,
// and the exit prints after the loop. Left open, the first path to reach it printed it
// inline and the others broke to nothing: `break outer` lost the `return` after the loops.
// A raw machine keeps the as-compiled shape, which the statement lifter reads.
fn reserving_exit(
    ctx: &mut RecoveryCtx<'_>,
    loop_info: &LoopInfo,
    body: impl FnOnce(&mut RecoveryCtx<'_>) -> Structure,
) -> Structure {
    let exit = loop_info
        .exit
        .filter(|e| !ctx.raw_machine && !ctx.visited.contains(e));
    if let Some(e) = exit {
        ctx.visited.insert(e);
    }
    let out = body(ctx);
    if let Some(e) = exit {
        ctx.visited.remove(&e);
    }
    out
}

// What follows a loop: its exit. Printed already on another path, it still has to run on
// this one, so it comes as a copy or as a `break` of the enclosing loop; left out, the inner
// loop's `break` fell into the next pass of the outer one without its `j++`.
fn after_loop(
    ctx: &mut RecoveryCtx<'_>,
    loop_info: &LoopInfo,
    loop_stack: &[&LoopInfo],
) -> Option<Structure> {
    let exit = loop_info.exit?;
    (!ctx.visited.contains(&exit) || !ctx.raw_machine)
        .then(|| recover_structure(ctx, exit, loop_stack))
}

pub(super) fn recover_loop(
    ctx: &mut RecoveryCtx<'_>,
    loop_info: &LoopInfo,
    loop_stack: &[&LoopInfo],
) -> Structure {
    ctx.visited.insert(loop_info.header);

    let header = match ctx.cfg.get(loop_info.header) {
        Some(b) => b,
        None => return Structure::Block(loop_info.header, vec![]),
    };

    let header_stmts = header.statements.clone();

    let update_block = if loop_info.back_edges.len() == 1 {
        Some(loop_info.back_edges[0].0)
    } else {
        None
    };

    let make_loop = |body: Structure, condition: Expression| -> Structure {
        let loop_struct = if let Some(update_id) = update_block {
            if let Some((new_body, update_struct)) = split_update_from_body(body.clone(), update_id)
            {
                Structure::For {
                    init: Box::new(Structure::Block(BlockId(0), vec![])),
                    condition,
                    update: Box::new(update_struct),
                    body: Box::new(new_body),
                }
            } else {
                Structure::While {
                    condition,
                    body: Box::new(body),
                }
            }
        } else {
            Structure::While {
                condition,
                body: Box::new(body),
            }
        };

        let label_name = format!("label{}", loop_stack.len());
        Structure::Label(label_name, Box::new(loop_struct))
    };

    let mut new_stack = loop_stack.to_vec();
    new_stack.push(loop_info);

    match &header.terminator {
        Terminator::Branch {
            condition,
            true_target,
            false_target,
        } => {
            let condition = condition.clone();
            let true_target = *true_target;
            let false_target = *false_target;
            let true_in_loop = loop_info.body.contains(&true_target);
            let false_in_loop = loop_info.body.contains(&false_target);

            // Single-block do-while: the header branches back to itself, so its
            // own statements ARE the loop body. Emit `do { header } while(cond)`
            // instead of hoisting the body out and leaving the loop empty (which
            // silently dropped counting loops).
            if true_target == loop_info.header || false_target == loop_info.header {
                let loops_on_true = true_target == loop_info.header;
                let exit_target = if loops_on_true {
                    false_target
                } else {
                    true_target
                };
                let cond = if loops_on_true {
                    condition
                } else {
                    Expression::unary(crate::ir::UnaryOp::Not, condition)
                };
                let mut parts = vec![Structure::DoWhile {
                    body: Box::new(Structure::Block(loop_info.header, header_stmts)),
                    condition: cond,
                }];
                if !ctx.visited.contains(&exit_target) {
                    parts.push(recover_structure(ctx, exit_target, loop_stack));
                }
                return Structure::Sequence(parts);
            }

            // A header with statements. The back edges come to it, so they run on every
            // pass, and `stmts; while (cond)` runs them once: `for (;;) { push(x); if
            // (done) break; x = next(); }` pushed one item. The last arm keeps them in
            // the loop. With a later block taking a second exit (a `for` with a `return`
            // inside), the header's exit prints inline and the other is what `break`
            // means. A test of `undefined` stays hoisted: the for-of and for-in
            // recognisers read that form.
            let header_exit = match (true_in_loop, false_in_loop) {
                (true, false) => Some(false_target),
                (false, true) => Some(true_target),
                _ => None,
            };
            let mut others: Vec<BlockId> = Vec::new();
            for b in &loop_info.body {
                for t in ctx.cfg.get(*b).map(|x| x.successors()).unwrap_or_default() {
                    if !loop_info.body.contains(&t)
                        && Some(t) != header_exit
                        && !others.contains(&t)
                    {
                        others.push(t);
                    }
                }
            }
            let statements_inside = header_exit.is_some()
                && !header_stmts.is_empty()
                && others.len() <= 1
                && !tests_undefined(&condition)
                && super::recovery::stays_within(ctx.cfg, &[loop_info.header], |b| {
                    loop_info.body.contains(&b)
                });
            // Not when that way out belongs to an enclosing loop: it is `break outer`, and
            // taken for this loop's own exit it printed a plain `break`, which left only
            // this loop and ran the outer one on.
            let enclosing = |b: BlockId| {
                loop_stack
                    .iter()
                    .any(|l| l.header == b || l.exit == Some(b))
            };
            if let Some(&other) = others
                .first()
                .filter(|o| statements_inside && !enclosing(**o))
            {
                if loop_info.exit != Some(other) {
                    let mut retargeted = loop_info.clone();
                    retargeted.exit = Some(other);
                    return recover_loop(ctx, &retargeted, loop_stack);
                }
            }
            // The header's way out leaves the parent loop too, and another stays in it: the
            // other is this loop's own, and the header's test a jump further out. Taken for
            // the exit, `j++` after the inner loop had nowhere to print.
            let parent = loop_stack.iter().rev().find(|l| !l.body.is_empty());
            if let Some(&own) = others
                .iter()
                .find(|o| parent.is_some_and(|p| p.body.contains(*o)))
                .filter(|_| {
                    !ctx.raw_machine
                        && loop_info.exit == header_exit
                        && header_exit.is_some_and(|e| parent.is_some_and(|p| !p.body.contains(&e)))
                })
            {
                let mut retargeted = loop_info.clone();
                retargeted.exit = Some(own);
                return recover_loop(ctx, &retargeted, loop_stack);
            }
            let statements_inside = statements_inside
                || (!ctx.raw_machine && header_exit.is_some() && loop_info.exit != header_exit);

            if true_in_loop && !false_in_loop && !statements_inside {
                let body = reserving_exit(ctx, loop_info, |ctx| {
                    recover_structure(ctx, true_target, &new_stack)
                });
                let mut parts = vec![Structure::Block(loop_info.header, header_stmts)];
                parts.push(make_loop(body, condition));

                parts.extend(after_loop(ctx, loop_info, loop_stack));
                Structure::Sequence(parts)
            } else if false_in_loop && !true_in_loop && !statements_inside {
                let body = reserving_exit(ctx, loop_info, |ctx| {
                    recover_structure(ctx, false_target, &new_stack)
                });
                let mut parts = vec![Structure::Block(loop_info.header, header_stmts)];
                parts.push(make_loop(
                    body,
                    Expression::unary(crate::ir::UnaryOp::Not, condition),
                ));

                parts.extend(after_loop(ctx, loop_info, loop_stack));
                Structure::Sequence(parts)
            } else {
                // The header's branch stays inside the loop on both sides, so
                // the header does NOT decide the loop exit: it is a conditional
                // *within* the loop body (the exit test lives at a back-edge
                // block). Structure the header's diamond the same way
                // `recover_structure` does, recover each arm only up to the
                // merge (post-dominator), emit the merge AFTER the `if`, so it
                // isn't absorbed into one arm, then wrap the whole body in a
                // loop so the exit `break` emitted deeper down is valid.
                let header_block = Structure::Block(loop_info.header, header_stmts);
                let merge = super::recovery::find_merge_point(
                    ctx.cfg,
                    loop_info.header,
                    true_target,
                    false_target,
                )
                .and_then(|m| {
                    super::recovery::past_rejoin(ctx, m, [true_target, false_target], &new_stack)
                })
                // A merge outside the loop is where the arms leave it, not something
                // that runs after this `if` on each pass.
                .filter(|m| ctx.raw_machine || loop_info.body.contains(m));
                let body = reserving_exit(ctx, loop_info, |ctx| {
                    if let Some(merge) = merge {
                        let merge_was_visited = ctx.visited.contains(&merge);
                        ctx.visited.insert(merge);
                        ctx.falls_to.push(Some(merge));
                        let then_ = recover_structure(ctx, true_target, &new_stack);
                        let else_ = recover_structure(ctx, false_target, &new_stack);
                        ctx.falls_to.pop();
                        if !merge_was_visited {
                            ctx.visited.remove(&merge);
                        }
                        Structure::Sequence(vec![
                            header_block,
                            Structure::If {
                                condition,
                                then_: Box::new(then_),
                                else_: Box::new(else_),
                            },
                            recover_structure(ctx, merge, &new_stack),
                        ])
                    } else {
                        let then_ = recover_structure(ctx, true_target, &new_stack);
                        let else_ = recover_structure(ctx, false_target, &new_stack);
                        Structure::Sequence(vec![
                            header_block,
                            Structure::If {
                                condition,
                                then_: Box::new(then_),
                                else_: Box::new(else_),
                            },
                        ])
                    }
                });
                let mut parts = vec![make_loop(
                    body,
                    Expression::constant(crate::ir::Constant::Bool(true)),
                )];
                parts.extend(after_loop(ctx, loop_info, loop_stack));
                Structure::Sequence(parts)
            }
        }
        Terminator::Jump(target) => {
            let target = *target;
            if loop_info.body.contains(&target) {
                // The back edges come to the header, so its statements run on
                // every pass: they open the body, not precede the loop. The
                // exit follows the loop as in the arms above. Hoisted and with
                // no exit, two nested counted loops (`hermesc -O`) lost the
                // inner counter's reset and everything after them (Discord's
                // ConjureDitherField module lost its exports).
                let body = reserving_exit(ctx, loop_info, |ctx| {
                    recover_structure(ctx, target, &new_stack)
                });
                let mut parts = vec![make_loop(
                    Structure::Sequence(vec![
                        Structure::Block(loop_info.header, header_stmts),
                        body,
                    ]),
                    Expression::constant(crate::ir::Constant::Bool(true)),
                )];
                parts.extend(after_loop(ctx, loop_info, loop_stack));
                Structure::Sequence(parts)
            } else {
                let next = recover_structure(ctx, target, loop_stack);
                Structure::Sequence(vec![Structure::Block(loop_info.header, header_stmts), next])
            }
        }
        // A loop whose header dispatches on a switch (a `for` over tokens whose
        // body is a `switch` on each one, v98's `StringSwitchImm`): the header
        // and its switch are the body of `while (true)`. Everything reachable
        // only through the switch used to be dropped with it (graphql's
        // `parseNumberSkeleton` kept two statements of 464 instructions).
        // Inside the switch an unlabeled `break` leaves the switch, not the
        // loop, so the cases are recovered under one more stack frame that
        // matches no block: every exit from the loop then names its label.
        Terminator::Switch {
            value,
            cases,
            default,
        } => {
            let marker = LoopInfo {
                header: BlockId(u32::MAX),
                body: Default::default(),
                exit: None,
                back_edges: Vec::new(),
                is_do_while: false,
            };
            let mut inner = new_stack.clone();
            inner.push(&marker);
            let switch = super::recovery::recover_switch(
                ctx,
                loop_info.header,
                value.clone(),
                cases,
                *default,
                &inner,
            );
            let body = Structure::Sequence(vec![
                Structure::Block(loop_info.header, header_stmts),
                switch,
            ]);
            let mut parts = vec![make_loop(
                body,
                Expression::constant(crate::ir::Constant::Bool(true)),
            )];
            parts.extend(after_loop(ctx, loop_info, loop_stack));
            Structure::Sequence(parts)
        }
        _ => Structure::Block(loop_info.header, header_stmts),
    }
}

pub(super) fn split_update_from_body(
    body: Structure,
    update_id: BlockId,
) -> Option<(Structure, Structure)> {
    match body {
        Structure::Sequence(mut parts) => {
            if parts.is_empty() {
                return None;
            }
            let last = parts.pop().unwrap();

            if let Structure::Block(id, ref _stmts) = last {
                if id == update_id {
                    let new_body = if parts.len() == 1 {
                        parts.into_iter().next().unwrap()
                    } else {
                        Structure::Sequence(parts)
                    };
                    return Some((new_body, last));
                }
            }

            if let Some((new_last, update)) = split_update_from_body(last.clone(), update_id) {
                parts.push(new_last);
                let new_body = Structure::Sequence(parts);
                return Some((new_body, update));
            }

            parts.push(last);
            None
        }
        Structure::Block(id, _) => {
            if id == update_id {
                return Some((Structure::Block(BlockId(0), vec![]), body));
            }
            None
        }
        _ => None,
    }
}

use super::exceptions::recover_catch_body;
use super::loops::recover_loop;
use super::{RecoveryCtx, Structure};
use crate::analysis::loops::{detect_loops, LoopInfo};
use crate::ir::{BlockId, Expression, Statement, Terminator, CFG};
use std::collections::{BTreeMap, HashSet};

// Recovers high-level structures from CFG.
//
// This is the "Structurization" phase.
// The CFG (Control Flow Graph) is a graph of basic blocks with jumps.
// We need to convert this back into nested structures like `if`, `while`, `for`.
//
// Strategy:
// 1. Detect loops (headers, bodies, exits) using graph analysis.
// 2. Traverse the graph starting from entry.
// 3. Recursively match patterns:
//    - If a node dominates 2 children that merge back, it's an `if`.
//    - If a node is a loop header, recurse into the loop body.
//    - Handle `break` and `continue` by checking loop stacks.
pub fn analyze(cfg: &CFG) -> (Structure, Vec<LoopInfo>) {
    let loops = detect_loops(cfg);
    let mut visited = HashSet::new();
    let loop_stack = Vec::new();

    // Build a map: try_block_start → exception handler info.
    //
    // A single source-level `try` can compile to several handler ranges that all
    // target the *same* catch block, the compiler splits the protected region
    // around an inner branch (e.g. the `throw` arm gets its own range). Keying
    // naively by start would then emit one nested try/catch per range. Collapse
    // them: register a single try per catch block, at its earliest start, so the
    // body recovery folds the remaining ranges in as ordinary control flow.
    let mut earliest_for_catch: BTreeMap<BlockId, &crate::ir::CfgExceptionHandler> =
        BTreeMap::new();
    for handler in &cfg.exception_handlers {
        earliest_for_catch
            .entry(handler.catch_block)
            .and_modify(|h| {
                if handler.try_block_start < h.try_block_start {
                    *h = handler;
                }
            })
            .or_insert(handler);
    }
    // Two catch blocks can protect ranges that start at one block: a `try`
    // with both `catch` and `finally` protects the body with the catch and
    // the body and the catch with the finally. Keyed one per start, the
    // finally replaced the catch and the catch body was never emitted
    // (Sentry's `sentryWrapped` lost `withScope(...); throw ex`, and the
    // closure it creates its parent). Both are kept, outermost (the larger
    // range) first, and nest.
    let mut try_starts: BTreeMap<BlockId, Vec<&crate::ir::CfgExceptionHandler>> = BTreeMap::new();
    for handler in earliest_for_catch.values() {
        try_starts
            .entry(handler.try_block_start)
            .or_default()
            .push(*handler);
    }
    for handlers in try_starts.values_mut() {
        handlers.sort_by_key(|h| {
            (
                std::cmp::Reverse(cfg.exception_edge_sources(h.catch_block).len()),
                h.catch_block,
            )
        });
    }

    let mut emitted = HashSet::new();
    let mut ctx = RecoveryCtx {
        cfg,
        loops: &loops,
        visited: &mut visited,
        emitted: &mut emitted,
        try_starts: &try_starts,
        open_tries: HashSet::new(),
    };

    let root = recover_structure(&mut ctx, cfg.entry, &loop_stack);
    (root, loops)
}

// A `return` or `throw` tail (a block, or a straight run of jumps ending in
// one) reached again after it was emitted on another path: structured code
// cannot jump to it, so it is emitted again. Left
// empty, the branch that led here lost its condition with it: a v98
// generator's resume test `i < n ? yield : return "end"` had both targets
// emitted already and printed as nothing at all. A block only marked visited
// (a merge point or loop exit held back to follow its region) is not.
fn reemit_exit(ctx: &RecoveryCtx<'_>, block_id: BlockId) -> Option<Structure> {
    if !ctx.emitted.contains(&block_id) {
        return None;
    }
    let mut all = Vec::new();
    let mut at = block_id;
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(at) || seen.len() > 16 {
            return None;
        }
        let block = ctx.cfg.get(at)?;
        all.extend(block.statements.iter().cloned());
        match &block.terminator {
            Terminator::Return(e) => all.push(Statement::Return(e.clone())),
            Terminator::Throw(e) => all.push(Statement::Throw(e.clone())),
            Terminator::Jump(next) => {
                at = *next;
                continue;
            }
            _ => return None,
        }
        return Some(Structure::Block(block_id, all));
    }
}

pub(super) fn recover_structure(
    ctx: &mut RecoveryCtx<'_>,
    block_id: BlockId,
    loop_stack: &[&LoopInfo],
) -> Structure {
    if ctx.visited.contains(&block_id) {
        // If we're jumping to a loop header we're in, it's a continue
        for (i, loop_info) in loop_stack.iter().enumerate().rev() {
            if block_id == loop_info.header {
                let label = if i < loop_stack.len() - 1 {
                    Some(format!("label{i}"))
                } else {
                    None
                };
                return Structure::Continue(label);
            }
            if loop_info.exit == Some(block_id) {
                let label = if i < loop_stack.len() - 1 {
                    Some(format!("label{i}"))
                } else {
                    None
                };
                return Structure::Break(label);
            }
        }
        return reemit_exit(ctx, block_id).unwrap_or(Structure::Block(block_id, vec![]));
    }

    let next_try = ctx.try_starts.get(&block_id).and_then(|hs| {
        hs.iter()
            .copied()
            .find(|h| !ctx.open_tries.contains(&h.catch_block))
    });
    if let Some(handler) = next_try {
        let catch_block_id = handler.catch_block;

        // The protected range has an end. The block the range falls through
        // to is where the try and the catch reconverge, and it belongs after
        // the statement, not inside the try body. Without this fence the body
        // walk kept going and swallowed everything up to the return, and the
        // catch came out empty. The fence is only placed when the exit is
        // unambiguous and is not a loop boundary the walk already handles.
        let region: HashSet<BlockId> = ctx
            .cfg
            .exception_edge_sources(catch_block_id)
            .into_iter()
            .collect();
        let exit = try_region_exit(ctx.cfg, &region, catch_block_id).filter(|e| {
            !ctx.visited.contains(e)
                && !loop_stack
                    .iter()
                    .any(|l| l.header == *e || l.exit == Some(*e))
        });
        if let Some(exit) = exit {
            ctx.visited.insert(exit);
        }

        // Mark catch block as visited to prevent it from being included in try body traversal
        let catch_was_visited = ctx.visited.contains(&catch_block_id);
        ctx.visited.insert(catch_block_id);

        // Recover the try body (catch block excluded via visited set). A
        // handler inside this one that starts at the same block opens there.
        ctx.open_tries.insert(catch_block_id);
        let try_body = recover_structure(ctx, block_id, loop_stack);
        ctx.open_tries.remove(&catch_block_id);

        // Now recover the catch body, temporarily un-visit the catch block
        if !catch_was_visited {
            ctx.visited.remove(&catch_block_id);
        }
        let (catch_param, catch_body) = recover_catch_body(ctx, catch_block_id, loop_stack);

        let try_catch = Structure::TryCatch {
            try_body: Box::new(try_body),
            catch_param,
            catch_body: Box::new(catch_body),
        };
        return match exit {
            Some(exit) => {
                ctx.visited.remove(&exit);
                let after = recover_structure(ctx, exit, loop_stack);
                Structure::Sequence(vec![try_catch, after])
            }
            None => try_catch,
        };
    }

    recover_structure_inner(ctx, block_id, loop_stack)
}

pub(super) fn recover_structure_inner(
    ctx: &mut RecoveryCtx<'_>,
    block_id: BlockId,
    loop_stack: &[&LoopInfo],
) -> Structure {
    if ctx.visited.contains(&block_id) {
        for (i, loop_info) in loop_stack.iter().enumerate().rev() {
            if block_id == loop_info.header {
                let label = if i < loop_stack.len() - 1 {
                    Some(format!("label{i}"))
                } else {
                    None
                };
                return Structure::Continue(label);
            }
            if loop_info.exit == Some(block_id) {
                let label = if i < loop_stack.len() - 1 {
                    Some(format!("label{i}"))
                } else {
                    None
                };
                return Structure::Break(label);
            }
        }
        return reemit_exit(ctx, block_id).unwrap_or(Structure::Block(block_id, vec![]));
    }

    // Check if this is a loop header
    if let Some(loop_info) = ctx.loops.iter().find(|l| l.header == block_id) {
        if !ctx.visited.contains(&block_id) {
            let loop_info = loop_info.clone();
            return recover_loop(ctx, &loop_info, loop_stack);
        }
    }

    ctx.visited.insert(block_id);
    ctx.emitted.insert(block_id);

    let block = match ctx.cfg.get(block_id) {
        Some(b) => b,
        None => {
            log::warn!(
                "Block {} not found in CFG during structure recovery",
                block_id.0
            );
            return Structure::Block(block_id, vec![]);
        }
    };

    let stmts = block.statements.clone();

    match &block.terminator {
        Terminator::Return(e) => {
            let mut all = stmts;
            all.push(Statement::Return(e.clone()));
            Structure::Block(block_id, all)
        }
        Terminator::Throw(e) => {
            let mut all = stmts;
            all.push(Statement::Throw(e.clone()));
            Structure::Block(block_id, all)
        }
        Terminator::Jump(target) => {
            let target = *target;
            // Check for loop continue/break, emit real Break/Continue statements
            // (not Comment markers). Comment("break")/Comment("continue") previously
            // leaked into codegen as `// break` / `// continue` instead of structured
            // control flow (HBC96 reference bundle: 1268 such comments).
            for (i, loop_info) in loop_stack.iter().enumerate().rev() {
                if target == loop_info.header && ctx.visited.contains(&target) {
                    let label = if i < loop_stack.len() - 1 {
                        Some(format!("label{i}"))
                    } else {
                        None
                    };
                    let mut stmts = stmts;
                    stmts.push(Statement::Continue(label));
                    return Structure::Block(block_id, stmts);
                }
                if loop_info.exit == Some(target) {
                    let label = if i < loop_stack.len() - 1 {
                        Some(format!("label{i}"))
                    } else {
                        None
                    };
                    let mut stmts = stmts;
                    stmts.push(Statement::Break(label));
                    return Structure::Block(block_id, stmts);
                }
            }

            let block_struct = Structure::Block(block_id, stmts);
            let next = recover_structure(ctx, target, loop_stack);
            Structure::Sequence(vec![block_struct, next])
        }
        Terminator::Branch {
            condition,
            true_target,
            false_target,
        } => {
            let condition = condition.clone();
            let true_target = *true_target;
            let false_target = *false_target;

            // Check for loop patterns
            if let Some(loop_info) = loop_stack.last() {
                let true_exits = loop_info.exit == Some(true_target);
                let false_exits = loop_info.exit == Some(false_target);

                if true_exits && !false_exits {
                    let mut parts = vec![Structure::Block(block_id, stmts.clone())];
                    let else_ = recover_structure(ctx, false_target, loop_stack);
                    parts.push(Structure::If {
                        condition: condition.clone(),
                        then_: Box::new(Structure::Break(None)),
                        else_: Box::new(else_),
                    });
                    return Structure::Sequence(parts);
                }

                if false_exits && !true_exits {
                    let mut parts = vec![Structure::Block(block_id, stmts.clone())];
                    let then_ = recover_structure(ctx, true_target, loop_stack);
                    parts.push(Structure::If {
                        condition: Expression::unary(crate::ir::UnaryOp::Not, condition.clone()),
                        then_: Box::new(Structure::Break(None)),
                        else_: Box::new(then_),
                    });
                    return Structure::Sequence(parts);
                }
            }

            // Diamond: if the two branches reconverge at a merge block, recover
            // each branch only UP TO that merge, then emit the merge AFTER the
            // if. Otherwise the merge (the common tail, e.g. a shared `return`)
            // is wrongly absorbed into the `then` branch and the `else` becomes
            // empty.
            if let Some(merge) = find_merge_point(ctx.cfg, block_id, true_target, false_target) {
                if merge != true_target || merge != false_target {
                    let merge_was_visited = ctx.visited.contains(&merge);
                    ctx.visited.insert(merge);
                    let then_ = recover_structure(ctx, true_target, loop_stack);
                    let else_ = recover_structure(ctx, false_target, loop_stack);
                    if !merge_was_visited {
                        ctx.visited.remove(&merge);
                    }
                    let mut parts = vec![
                        Structure::Block(block_id, stmts),
                        Structure::If {
                            condition,
                            then_: Box::new(then_),
                            else_: Box::new(else_),
                        },
                    ];
                    parts.push(recover_structure(ctx, merge, loop_stack));
                    return Structure::Sequence(parts);
                }
            }

            let then_ = recover_structure(ctx, true_target, loop_stack);
            let else_ = recover_structure(ctx, false_target, loop_stack);
            let mut parts = vec![Structure::Block(block_id, stmts)];
            parts.push(Structure::If {
                condition,
                then_: Box::new(then_),
                else_: Box::new(else_),
            });
            Structure::Sequence(parts)
        }
        Terminator::Switch {
            value,
            cases,
            default,
        } => {
            let value = value.clone();
            let cases = cases.clone();
            let default = *default;
            Structure::Sequence(vec![
                Structure::Block(block_id, stmts),
                recover_switch(ctx, block_id, value, &cases, default, loop_stack),
            ])
        }
        Terminator::None => {
            log::debug!(
                "Block {} has Terminator::None, treating as simple block",
                block_id.0
            );
            Structure::Block(block_id, stmts)
        }
    }
}

// A switch's cases, each recovered once (cases sharing a target share its
// body), and its default, followed by the block the cases converge on. Held
// out while the cases are recovered, that block was absorbed by whichever
// case reached it first and every later case ended without it (a token loop's
// counter increment in its first case only, the others looping forever).
pub(super) fn recover_switch(
    ctx: &mut RecoveryCtx<'_>,
    header: BlockId,
    value: Expression,
    cases: &[(Expression, BlockId)],
    default: BlockId,
    loop_stack: &[&LoopInfo],
) -> Structure {
    let mut targets: Vec<BlockId> = Vec::new();
    for t in cases
        .iter()
        .map(|(_, t)| *t)
        .chain(std::iter::once(default))
    {
        if !targets.contains(&t) {
            targets.push(t);
        }
    }
    let merge = find_switch_merge(ctx.cfg, header, &targets)
        .filter(|m| !ctx.visited.contains(m) && !targets.contains(m));
    if let Some(m) = merge {
        ctx.visited.insert(m);
    }
    let switch = recover_switch_cases(ctx, value, cases, default, loop_stack);
    match merge {
        Some(m) => {
            ctx.visited.remove(&m);
            Structure::Sequence(vec![switch, recover_structure(ctx, m, loop_stack)])
        }
        None => switch,
    }
}

// The block most of a switch's targets reach (two at least) without passing
// back through the switch, the nearest to its first target on a tie: where the
// cases rejoin. Targets that end the function reach no such block and do not
// count against it.
fn find_switch_merge(cfg: &CFG, header: BlockId, targets: &[BlockId]) -> Option<BlockId> {
    use std::collections::{HashMap, VecDeque};
    let reach = |from: BlockId| -> HashMap<BlockId, usize> {
        let mut dist: HashMap<BlockId, usize> = HashMap::new();
        let mut queue = VecDeque::from([(from, 0usize)]);
        while let Some((n, d)) = queue.pop_front() {
            if n == header || dist.contains_key(&n) {
                continue;
            }
            dist.insert(n, d);
            if let Some(b) = cfg.get(n) {
                for s in b.successors() {
                    queue.push_back((s, d + 1));
                }
            }
        }
        dist
    };
    let sets: Vec<HashMap<BlockId, usize>> = targets.iter().map(|&t| reach(t)).collect();
    let mut count: BTreeMap<BlockId, (usize, usize)> = BTreeMap::new();
    for (i, set) in sets.iter().enumerate() {
        for (&b, &d) in set {
            let e = count.entry(b).or_insert((0, usize::MAX));
            e.0 += 1;
            if i == 0 || e.1 == usize::MAX {
                e.1 = e.1.min(d);
            }
        }
    }
    count
        .into_iter()
        .filter(|(_, (n, _))| *n >= 2)
        .max_by(|a, b| {
            a.1 .0
                .cmp(&b.1 .0)
                .then(b.1 .1.cmp(&a.1 .1))
                .then(b.0.cmp(&a.0))
        })
        .map(|(b, _)| b)
}

fn recover_switch_cases(
    ctx: &mut RecoveryCtx<'_>,
    value: Expression,
    cases: &[(Expression, BlockId)],
    default: BlockId,
    loop_stack: &[&LoopInfo],
) -> Structure {
    let mut recovered: BTreeMap<BlockId, Structure> = BTreeMap::new();
    for (_case_val, target) in cases {
        if !recovered.contains_key(target) {
            let body = recover_structure(ctx, *target, loop_stack);
            recovered.insert(*target, body);
        }
    }
    let switch_cases = cases
        .iter()
        .map(|(case_val, target)| (case_val.clone(), recovered[target].clone()))
        .collect();
    let default_body = match recovered.get(&default) {
        Some(cached) => cached.clone(),
        None => recover_structure(ctx, default, loop_stack),
    };
    Structure::Switch {
        discriminant: value,
        cases: switch_cases,
        default: Box::new(default_body),
    }
}

// Find the merge (join) point of a branch: the nearest block reachable from
// BOTH the true and false targets. This is the block where the two arms of an
// `if`/`else` diamond reconverge (its immediate post-dominator for simple
// diamonds). Returns None when the branches never reconverge (e.g. both return).
//
// `header` is the branch block itself. Both reachability walks stop *before*
// re-entering `header`: a genuine forward join is reached without re-executing
// the branch. Without this guard, a loop back-edge that flows back through the
// branch would make the branch's own arms mutually reachable, and the walk would
// pick a spurious "merge" one arm earlier than the real post-dominator (which
// then gets wrongly absorbed into an `if` branch, producing infinite loops).
// The block where the try body and the catch body reconverge: the single
// block control falls into when it leaves the protected range by a normal
// edge, provided the catch reaches it too. A block only the try flows into
// (a `return` the compiler left just outside the range) is the tail of the
// try body and stays there. More than one exit (a `break` out of the try
// next to its fallthrough, say) is ambiguous and yields nothing.
fn try_region_exit(cfg: &CFG, region: &HashSet<BlockId>, catch: BlockId) -> Option<BlockId> {
    let mut exits: Vec<BlockId> = Vec::new();
    for &b in region {
        let Some(block) = cfg.get(b) else { continue };
        for succ in block.successors() {
            if succ != catch && !region.contains(&succ) && !exits.contains(&succ) {
                exits.push(succ);
            }
        }
    }
    let exit = match exits.as_slice() {
        [only] => *only,
        _ => return None,
    };
    let mut seen: HashSet<BlockId> = HashSet::new();
    let mut stack = vec![catch];
    while let Some(b) = stack.pop() {
        if b == exit {
            return Some(exit);
        }
        if !seen.insert(b) {
            continue;
        }
        if let Some(block) = cfg.get(b) {
            stack.extend(block.successors());
        }
    }
    None
}

pub(super) fn find_merge_point(
    cfg: &CFG,
    header: BlockId,
    a: BlockId,
    b: BlockId,
) -> Option<BlockId> {
    use std::collections::VecDeque;
    // All blocks reachable from `a` without passing back through `header`.
    let mut reach_a: HashSet<BlockId> = HashSet::new();
    let mut stack = vec![a];
    while let Some(n) = stack.pop() {
        if n == header || !reach_a.insert(n) {
            continue;
        }
        if let Some(blk) = cfg.get(n) {
            for s in blk.successors() {
                stack.push(s);
            }
        }
    }
    // Breadth-first from `b` for the NEAREST block also reachable from `a`,
    // likewise never crossing back through `header`.
    let mut seen: HashSet<BlockId> = HashSet::new();
    let mut queue: VecDeque<BlockId> = VecDeque::new();
    queue.push_back(b);
    while let Some(n) = queue.pop_front() {
        if n == header || !seen.insert(n) {
            continue;
        }
        if reach_a.contains(&n) {
            return Some(n);
        }
        if let Some(blk) = cfg.get(n) {
            for s in blk.successors() {
                queue.push_back(s);
            }
        }
    }
    None
}

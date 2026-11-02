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
// The first block of a `try` in a loop can be the loop's header too. The `try` opened
// there first and the loop was never seen: `for (...) { try { ... } catch { } }` ran
// its body once. An empty block in front takes the back edges and is the header, so
// the loop opens and the `try` opens inside it. Not for `try { for (...) { } }`, whose
// back edges are inside the `try`: that loop belongs inside as it stands.
fn split_try_headers(cfg: &CFG) -> Option<CFG> {
    let loops = detect_loops(cfg);
    let headers: std::collections::BTreeSet<BlockId> = loops
        .iter()
        .filter(|l| {
            cfg.exception_handlers.iter().any(|h| {
                h.try_block_start == l.header
                    && l.back_edges
                        .iter()
                        .all(|(from, _)| !h.try_blocks.contains(from))
            })
        })
        .map(|l| l.header)
        .collect();
    if headers.is_empty() {
        return None;
    }
    let mut out = cfg.clone();
    for header in headers {
        let front = out.create_block();
        let retarget = |t: &mut BlockId| {
            if *t == header {
                *t = front;
            }
        };
        for b in out.blocks_mut() {
            match &mut b.terminator {
                Terminator::Jump(t) => retarget(t),
                Terminator::Branch {
                    true_target,
                    false_target,
                    ..
                } => {
                    retarget(true_target);
                    retarget(false_target);
                }
                Terminator::Switch { cases, default, .. } => {
                    cases.iter_mut().for_each(|(_, t)| retarget(t));
                    retarget(default);
                }
                _ => {}
            }
        }
        if let Some(b) = out.get_mut(front) {
            b.terminator = Terminator::Jump(header);
        }
        if out.entry == header {
            out.entry = front;
        }
    }
    Some(out)
}

pub fn analyze(cfg: &CFG) -> (Structure, Vec<LoopInfo>) {
    match split_try_headers(cfg) {
        Some(split) => analyze_split(&split),
        None => analyze_split(cfg),
    }
}

fn analyze_split(cfg: &CFG) -> (Structure, Vec<LoopInfo>) {
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
        falls_to: Vec::new(),
        fences: Vec::new(),
        next_reg: std::cell::Cell::new(None),
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
        // The merge that follows runs anyway: copied too, it ran twice in print and
        // its registers had two definitions, which blocked inlining them.
        if at != block_id && ctx.falls_to.last().copied().flatten() == Some(at) {
            return Some(Structure::Block(block_id, all));
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

// A tail that ends in a branch, reached again after it was emitted: the second
// test of `a && b || c` jumps to the merge and to the third test, both already
// placed, and left empty the test was lost. It is copied as an `if`, as long as
// every way out ends the function or falls into the merge that follows. Anything
// else has no structured form here, and None keeps the old empty result.
fn reemit_branches(
    ctx: &RecoveryCtx<'_>,
    block_id: BlockId,
    loop_stack: &[&LoopInfo],
    budget: &mut u32,
    copied: &mut HashSet<BlockId>,
) -> Option<Structure> {
    let merge = ctx.falls_to.last().copied().flatten();
    if merge == Some(block_id) {
        return Some(Structure::Block(block_id, vec![]));
    }
    // A loop's header or exit: the copy continues or breaks, as the block did. Only
    // the innermost loop's: a pass can rewrite an outer loop and drop its label, and
    // `continue label0` with no such label does not parse.
    if let Some(l) = loop_stack.last() {
        if block_id == l.header {
            return Some(Structure::Continue(None));
        }
        if l.exit == Some(block_id) {
            return Some(Structure::Break(None));
        }
    }
    if loop_stack
        .iter()
        .any(|l| block_id == l.header || l.exit == Some(block_id))
    {
        return None;
    }
    if !ctx.emitted.contains(&block_id) || *budget == 0 {
        return None;
    }
    *budget -= 1;
    copied.insert(block_id);
    let block = ctx.cfg.get(block_id)?;
    let mut stmts = block.statements.clone();
    match &block.terminator {
        Terminator::Return(e) => {
            stmts.push(Statement::Return(e.clone()));
            Some(Structure::Block(block_id, stmts))
        }
        Terminator::Throw(e) => {
            stmts.push(Statement::Throw(e.clone()));
            Some(Structure::Block(block_id, stmts))
        }
        Terminator::Jump(next) => {
            let next = reemit_branches(ctx, *next, loop_stack, budget, copied)?;
            Some(Structure::Sequence(vec![
                Structure::Block(block_id, stmts),
                next,
            ]))
        }
        Terminator::Branch {
            condition,
            true_target,
            false_target,
        } => {
            let then_ = reemit_branches(ctx, *true_target, loop_stack, budget, copied)?;
            let else_ = reemit_branches(ctx, *false_target, loop_stack, budget, copied)?;
            Some(Structure::Sequence(vec![
                Structure::Block(block_id, stmts),
                Structure::If {
                    condition: condition.clone(),
                    then_: Box::new(then_),
                    else_: Box::new(else_),
                },
            ]))
        }
        _ => None,
    }
}

// Registers a block assigns and registers it reads.
#[derive(Default)]
struct BlockRegs {
    read: HashSet<u32>,
    assigned: HashSet<u32>,
}

impl<'a> crate::ir::Visitor<'a> for BlockRegs {
    fn visit_expression(&mut self, e: &'a Expression) {
        if let Expression::Value(crate::ir::Value::Binding(crate::ir::Binding::Register(r))) = e {
            self.read.insert(*r);
        }
        self.walk_expression(e);
    }
    fn visit_assign_target(&mut self, t: &'a crate::ir::AssignTarget) {
        if let crate::ir::AssignTarget::Binding(crate::ir::Binding::Register(r)) = t {
            self.assigned.insert(*r);
        }
        self.walk_assign_target(t);
    }
}

fn block_regs(block: &crate::ir::BasicBlock) -> BlockRegs {
    use crate::ir::Visitor;
    let mut regs = BlockRegs::default();
    for s in &block.statements {
        regs.visit_statement(s);
    }
    match &block.terminator {
        Terminator::Branch { condition: e, .. }
        | Terminator::Throw(e)
        | Terminator::Return(Some(e))
        | Terminator::Switch { value: e, .. } => regs.visit_expression(e),
        _ => {}
    }
    regs
}

// Whether what the `from` blocks assign is read only inside `within`. A value
// read past a copy is a join of two assignments, and declared in one arm it is
// undeclared in the other. Read past a loop, it was declared in the loop's body.
// Either way the printed name would crash.
pub(super) fn stays_within(cfg: &CFG, from: &[BlockId], within: impl Fn(BlockId) -> bool) -> bool {
    let mut assigned = HashSet::new();
    for id in from {
        if let Some(b) = cfg.get(*id) {
            assigned.extend(block_regs(b).assigned);
        }
    }
    cfg.blocks_with_ids()
        .filter(|(id, _)| !within(*id))
        .all(|(_, b)| block_regs(b).read.is_disjoint(&assigned))
}

// What the copied blocks assign and something past them reads. In
// `if (a && b || c) r = 1; else r = 2; return r` that is `r`: the code after the
// copy reads it on every path, so the copy has to assign that very register. Blocks
// assigning it already make it one variable declared above them all. A register
// assigned in one block is read by later passes as one value, so a second
// assignment there printed names that crash: None, and the copy stays empty.
fn read_past(cfg: &CFG, from: &[BlockId], copied: &HashSet<BlockId>) -> Option<HashSet<u32>> {
    let mut assigned = HashSet::new();
    for id in from {
        if let Some(b) = cfg.get(*id) {
            assigned.extend(block_regs(b).assigned);
        }
    }
    let mut past = HashSet::new();
    for (id, b) in cfg.blocks_with_ids() {
        if !copied.contains(&id) {
            past.extend(block_regs(b).read.intersection(&assigned).copied());
        }
    }
    let several = |r: &u32| {
        cfg.blocks()
            .filter(|b| block_regs(b).assigned.contains(r))
            .nth(1)
            .is_some()
    };
    past.iter().all(several).then_some(past)
}

// Later passes read a register assigned once as one value, an import above all:
// assigned again by a copy, `<tmpResult.CompatView>` lost its declaration. The
// copy gets registers of its own, except those `read_past` says are shared.
// One the copy reads before assigning holds a value from above and keeps its name.
// A tail `reemit_exit` copies keeps all of its names: renamed, it lost strings.
fn fresh_registers(ctx: &RecoveryCtx<'_>, tree: &mut Structure, shared: &HashSet<u32>) {
    use crate::ir::MutVisitor;
    let cfg = ctx.cfg;
    struct Rename(std::collections::HashMap<u32, u32>);
    impl MutVisitor for Rename {
        fn visit_expression(&mut self, e: &mut Expression) {
            if let Expression::Value(crate::ir::Value::Binding(crate::ir::Binding::Register(r))) = e
            {
                if let Some(n) = self.0.get(r) {
                    *r = *n;
                }
            }
            self.walk_expression(e);
        }
        fn visit_assign_target(&mut self, t: &mut crate::ir::AssignTarget) {
            if let crate::ir::AssignTarget::Binding(crate::ir::Binding::Register(r)) = t {
                if let Some(n) = self.0.get(r) {
                    *r = *n;
                }
            }
            self.walk_assign_target(t);
        }
    }
    fn walk(tree: &mut Structure, rename: &mut Rename) {
        match tree {
            Structure::Block(_, stmts) => stmts.iter_mut().for_each(|s| rename.visit_statement(s)),
            Structure::Sequence(parts) => parts.iter_mut().for_each(|p| walk(p, rename)),
            Structure::If {
                condition,
                then_,
                else_,
            } => {
                rename.visit_expression(condition);
                walk(then_, rename);
                walk(else_, rename);
            }
            _ => {}
        }
    }
    // Copies in one function must not share registers either.
    let mut next = ctx.next_reg.get().unwrap_or_else(|| {
        cfg.blocks_with_ids()
            .flat_map(|(_, b)| {
                let r = block_regs(b);
                r.read.into_iter().chain(r.assigned).collect::<Vec<_>>()
            })
            .max()
            .map_or(0, |m| m + 1)
    });
    let mut map = std::collections::HashMap::new();
    for r in renamable(tree).into_iter().filter(|r| !shared.contains(r)) {
        map.insert(r, next);
        next += 1;
    }
    ctx.next_reg.set(Some(next));
    walk(tree, &mut Rename(map));
}

// What a copied tree assigns before any read of it, sorted so that output is stable.
fn renamable(tree: &Structure) -> Vec<u32> {
    use crate::ir::Visitor;
    #[derive(Default)]
    struct Seen {
        assigned: HashSet<u32>,
        from_above: HashSet<u32>,
    }
    fn reads(regs: &BlockRegs, defined: &HashSet<u32>, seen: &mut Seen) {
        seen.from_above
            .extend(regs.read.iter().filter(|r| !defined.contains(r)));
    }
    fn walk(tree: &Structure, defined: &mut HashSet<u32>, seen: &mut Seen) {
        match tree {
            Structure::Block(_, stmts) => {
                for stmt in stmts {
                    let mut regs = BlockRegs::default();
                    regs.visit_statement(stmt);
                    reads(&regs, defined, seen);
                    seen.assigned.extend(regs.assigned.iter().copied());
                    defined.extend(regs.assigned);
                }
            }
            Structure::Sequence(parts) => parts.iter().for_each(|p| walk(p, defined, seen)),
            Structure::If {
                condition,
                then_,
                else_,
            } => {
                let mut regs = BlockRegs::default();
                regs.visit_expression(condition);
                reads(&regs, defined, seen);
                let mut after_then = defined.clone();
                walk(then_, &mut after_then, seen);
                let mut after_else = defined.clone();
                walk(else_, &mut after_else, seen);
                *defined = after_then.intersection(&after_else).copied().collect();
            }
            _ => {}
        }
    }
    let mut seen = Seen::default();
    walk(tree, &mut HashSet::new(), &mut seen);
    let mut out: Vec<u32> = seen
        .assigned
        .difference(&seen.from_above)
        .copied()
        .collect();
    out.sort_unstable();
    out
}

fn reemit(ctx: &RecoveryCtx<'_>, block_id: BlockId, loop_stack: &[&LoopInfo]) -> Structure {
    reemit_exit(ctx, block_id)
        .or_else(|| {
            let mut copied = HashSet::new();
            let tree = reemit_branches(ctx, block_id, loop_stack, &mut 16, &mut copied)?;
            let from: Vec<BlockId> = copied.iter().copied().collect();
            let shared = read_past(ctx.cfg, &from, &copied)?;
            let mut tree = tree;
            fresh_registers(ctx, &mut tree, &shared);
            Some(tree)
        })
        .unwrap_or(Structure::Block(block_id, vec![]))
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
        return reemit(ctx, block_id, loop_stack);
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
        let exit =
            try_region_exit(ctx.cfg, &region, catch_block_id, handler.exact_range).filter(|e| {
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
        ctx.falls_to.push(None);
        ctx.fences.extend(exit);
        let try_body = recover_structure(ctx, block_id, loop_stack);
        if exit.is_some() {
            ctx.fences.pop();
        }
        ctx.open_tries.remove(&catch_block_id);

        // Now recover the catch body, temporarily un-visit the catch block
        if !catch_was_visited {
            ctx.visited.remove(&catch_block_id);
        }
        let (catch_param, catch_body) = recover_catch_body(ctx, catch_block_id, loop_stack);
        ctx.falls_to.pop();

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
        return reemit(ctx, block_id, loop_stack);
    }

    // Check if this is a loop header
    if let Some(loop_info) = ctx.loops.iter().find(|l| l.header == block_id) {
        if !ctx.visited.contains(&block_id) {
            let loop_info = loop_info.clone();
            ctx.falls_to.push(None);
            let recovered = recover_loop(ctx, &loop_info, loop_stack);
            ctx.falls_to.pop();
            return recovered;
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
            // A merge past the open `try`'s fence is no merge inside it: an arm leaving
            // the `try` falls out, and the other must not fall through that code.
            let past_fence = |ctx: &RecoveryCtx<'_>, merge: BlockId| {
                ctx.fences
                    .last()
                    .is_some_and(|f| *f != merge && reaches(ctx.cfg, *f, merge))
            };
            if let Some(merge) = find_merge_point(ctx.cfg, block_id, true_target, false_target)
                .filter(|m| !past_fence(ctx, *m))
                .filter(|m| !bypassed_tail(ctx, *m, [true_target, false_target], loop_stack))
            {
                if merge != true_target || merge != false_target {
                    let merge_was_visited = ctx.visited.contains(&merge);
                    ctx.visited.insert(merge);
                    ctx.falls_to.push(Some(merge));
                    let then_ = recover_structure(ctx, true_target, loop_stack);
                    let else_ = recover_structure(ctx, false_target, loop_stack);
                    ctx.falls_to.pop();
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
fn try_region_exit(
    cfg: &CFG,
    region: &HashSet<BlockId>,
    catch: BlockId,
    exact_range: bool,
) -> Option<BlockId> {
    let mut exits: Vec<BlockId> = Vec::new();
    for &b in region {
        let Some(block) = cfg.get(b) else { continue };
        for succ in block.successors() {
            if succ != catch && !region.contains(&succ) && !exits.contains(&succ) {
                exits.push(succ);
            }
        }
    }
    // A `try` can leave through an empty block where its catch jumps straight to what
    // follows. Fenced at the empty block, the catch took that code for its own, and
    // the `try` never ran it: in a loop, the increment. The fence goes past it.
    let past_empty = |mut at: BlockId| {
        for _ in 0..16 {
            match cfg.get(at) {
                Some(b) if b.statements.is_empty() => match b.terminator {
                    Terminator::Jump(next) if next != at && !region.contains(&next) => at = next,
                    _ => break,
                },
                _ => break,
            }
        }
        at
    };
    let mut resolved: Vec<BlockId> = exits.iter().map(|e| past_empty(*e)).collect();
    resolved.sort();
    resolved.dedup();
    // With a `return` inside, the code after the `try` is the exit its catch reaches
    // without passing through another: the rest are the `try`'s own. Unfenced, that
    // code printed inside and a throw skipped it, so a loop's increment never ran.
    if resolved.len() > 1 && !exact_range {
        let mut reached = Vec::new();
        let mut seen: HashSet<BlockId> = HashSet::new();
        let mut stack = vec![catch];
        while let Some(b) = stack.pop() {
            if !seen.insert(b) || region.contains(&b) {
                continue;
            }
            let at = past_empty(b);
            if resolved.contains(&at) {
                if !reached.contains(&at) {
                    reached.push(at);
                }
                continue;
            }
            if let Some(block) = cfg.get(at) {
                stack.extend(block.successors());
            }
        }
        return match reached.as_slice() {
            [only] => Some(*only),
            _ => None,
        };
    }
    let exit = match resolved.as_slice() {
        [only] => *only,
        _ => return None,
    };
    if exact_range {
        return Some(exit);
    }
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

fn reaches(cfg: &CFG, from: BlockId, to: BlockId) -> bool {
    let mut seen = HashSet::new();
    let mut stack = vec![from];
    while let Some(b) = stack.pop() {
        if b == to {
            return true;
        }
        if seen.insert(b) {
            stack.extend(cfg.get(b).map(|x| x.successors()).unwrap_or_default());
        }
    }
    false
}

// A `return` or `throw` tail both arms can reach is no merge when one can also get
// around it, to the merge that follows or the loop's next step. Held back and printed
// after the `if`, the tail ran on every path: `case "0": if (x) break; default: throw`
// lost its tests and always threw. Left as an arm, the second path copies the tail.
fn bypassed_tail(
    ctx: &RecoveryCtx<'_>,
    merge: BlockId,
    arms: [BlockId; 2],
    loop_stack: &[&LoopInfo],
) -> bool {
    let mut at = merge;
    let mut tail = HashSet::new();
    loop {
        if !tail.insert(at) || tail.len() > 16 {
            return false;
        }
        match ctx.cfg.get(at).map(|b| &b.terminator) {
            Some(Terminator::Return(_) | Terminator::Throw(_)) => break,
            Some(Terminator::Jump(next)) => at = *next,
            _ => return false,
        }
    }
    let header = loop_stack.last().map(|l| l.header);
    // Going on to the tail from there is still every path through it, so a true merge.
    // Around the loop does not count: that is the tail's next run, not this one.
    let leads_to_tail = |from: BlockId| {
        let mut seen = HashSet::new();
        let mut stack = vec![from];
        while let Some(b) = stack.pop() {
            if b == merge {
                return true;
            }
            if Some(b) == header || !seen.insert(b) {
                continue;
            }
            if let Some(block) = ctx.cfg.get(b) {
                stack.extend(block.successors());
            }
        }
        false
    };
    let beyond = |b: BlockId| {
        ctx.falls_to.last().copied().flatten() == Some(b)
            || loop_stack
                .last()
                .is_some_and(|l| l.header == b || l.exit == Some(b))
    };
    let mut seen = HashSet::new();
    let mut stack = arms.to_vec();
    while let Some(b) = stack.pop() {
        if b == merge || !seen.insert(b) {
            continue;
        }
        if beyond(b) {
            if !leads_to_tail(b) {
                return true;
            }
            continue;
        }
        if let Some(block) = ctx.cfg.get(b) {
            stack.extend(block.successors());
        }
    }
    false
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

#[cfg(test)]
mod copy_tests {
    use super::*;
    use crate::ir::{Binding, Value};

    fn reg(r: u32) -> Expression {
        Expression::Value(Value::Binding(Binding::Register(r)))
    }

    fn set(target: u32, value: Expression) -> Statement {
        Statement::assign_reg(target, value)
    }

    // r2 is read before the copy assigns it, so that read is a value from above the
    // copy. Renamed, it would read a register nothing ever wrote.
    #[test]
    fn a_register_read_before_it_is_assigned_keeps_its_name() {
        let tree = Structure::Block(BlockId(0), vec![set(1, reg(2)), set(2, reg(1))]);
        assert_eq!(renamable(&tree), vec![1]);
    }

    // Assigned on one arm only, r3 still holds the value from above on the other.
    #[test]
    fn a_register_assigned_on_one_arm_is_from_above_when_read_after() {
        let tree = Structure::Sequence(vec![
            Structure::If {
                condition: reg(9),
                then_: Box::new(Structure::Block(BlockId(0), vec![set(3, reg(9))])),
                else_: Box::new(Structure::Block(BlockId(0), vec![])),
            },
            Structure::Block(BlockId(0), vec![set(4, reg(3))]),
        ]);
        assert_eq!(renamable(&tree), vec![4]);
    }
}

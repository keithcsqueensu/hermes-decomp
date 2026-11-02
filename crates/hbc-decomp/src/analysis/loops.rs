use crate::ir::{BlockId, Terminator, CFG};
use std::collections::{BTreeMap, BTreeSet, HashSet};

// Information about a detected loop.
#[derive(Debug, Clone)]
pub struct LoopInfo {
    pub header: BlockId,
    pub body: BTreeSet<BlockId>,
    pub exit: Option<BlockId>,
    pub back_edges: Vec<(BlockId, BlockId)>,
    pub is_do_while: bool,
}

// Detect all loops in the CFG using back-edge analysis.
//
// Strategy:
// 1. Compute Dominators for the CFG.
// 2. Find Back-Edges: edges `A -> B` where `B` dominates `A`. `B` is the loop header.
// 3. Construct Loop Body: Start from `A` and walk predecessors backwards until reaching `B`.
// 4. Find Loop Exit: Successors of body nodes that are outside the body.
pub fn detect_loops(cfg: &CFG) -> Vec<LoopInfo> {
    let dominators = compute_dominators(cfg);
    let mut loops = Vec::new();
    let mut back_edges = Vec::new();

    // Find back edges (edge to a dominator)
    for block in cfg.blocks() {
        for succ in block.successors() {
            if dominators
                .get(&block.id)
                .map(|d| d.contains(&succ))
                .unwrap_or(false)
            {
                back_edges.push((block.id, succ));
            }
        }
    }

    let mut headers: BTreeMap<BlockId, Vec<(BlockId, BlockId)>> = BTreeMap::new();
    for edge in back_edges {
        headers.entry(edge.1).or_default().push(edge);
    }

    for (header, edges) in headers {
        let mut body = BTreeSet::new();
        body.insert(header);

        for (from, _) in &edges {
            collect_loop_body(cfg, *from, header, &mut body);
        }

        // Find loop exit (successor of header not in body, or branch target out of body)
        let exit = find_loop_exit(cfg, header, &body, &edges);

        // Determine if this is a do-while (condition at end)
        let is_do_while = edges.iter().any(|(from, to)| {
            *to == header
                && cfg
                    .get(*from)
                    .map(|b| matches!(b.terminator, Terminator::Branch { .. }))
                    .unwrap_or(false)
        });

        loops.push(LoopInfo {
            header,
            body,
            exit,
            back_edges: edges,
            is_do_while,
        });
    }

    loops
}

fn collect_loop_body(cfg: &CFG, node: BlockId, header: BlockId, body: &mut BTreeSet<BlockId>) {
    if body.contains(&node) {
        return;
    }
    body.insert(node);
    for pred in cfg.predecessors(node) {
        if pred != header {
            collect_loop_body(cfg, pred, header, body);
        }
    }
}

fn find_loop_exit(
    cfg: &CFG,
    header: BlockId,
    body: &BTreeSet<BlockId>,
    back_edges: &[(BlockId, BlockId)],
) -> Option<BlockId> {
    // A header that dispatches on a switch leaves the loop only through the
    // cases that end the function (a `throw` for an unknown token); the loop's
    // own test sits at the back edge, so its way out is the exit.
    if matches!(
        cfg.get(header).map(|b| &b.terminator),
        Some(Terminator::Switch { .. })
    ) {
        for (from, _) in back_edges {
            if let Some(block) = cfg.get(*from) {
                if let Some(succ) = block.successors().into_iter().find(|s| !body.contains(s)) {
                    return Some(succ);
                }
            }
        }
    }
    if let Some(block) = cfg.get(header) {
        for succ in block.successors() {
            if !body.contains(&succ) {
                return Some(succ);
            }
        }
    }

    // The test at the back edge decides the loop. Another way out of the body is a
    // `return` or a `break` inside it, and taken for the exit, `break` went there.
    for (from, _) in back_edges {
        if let Some(block) = cfg.get(*from) {
            if let Some(succ) = block.successors().into_iter().find(|s| !body.contains(s)) {
                return Some(succ);
            }
        }
    }

    for &block_id in body {
        if let Some(block) = cfg.get(block_id) {
            for succ in block.successors() {
                if !body.contains(&succ) {
                    return Some(succ);
                }
            }
        }
    }

    None
}

pub fn compute_dominators(cfg: &CFG) -> BTreeMap<BlockId, HashSet<BlockId>> {
    let all_blocks: HashSet<_> = cfg.block_ids().collect();
    let mut dom: BTreeMap<BlockId, HashSet<BlockId>> = BTreeMap::new();

    // Exception catch blocks are entered from outside normal control flow (the
    // exception edge is not a CFG successor), so they have no normal predecessors.
    // They are entered from the blocks of their protected range, which is where
    // their dominators come from. As roots of their own, the block a catch rejoins
    // was dominated by nothing above the `try`: in `for (...) { try { } catch { } }`
    // the header did not dominate the latch, the back edge went unseen, and the loop
    // was lost. Left with the degenerate "all blocks" set, a catch's outgoing edge
    // (`catch -> return`) would look like a back edge. One with no range is a root.
    let mut catch_sources: BTreeMap<BlockId, Vec<BlockId>> = BTreeMap::new();
    for h in &cfg.exception_handlers {
        catch_sources
            .entry(h.catch_block)
            .or_insert_with(|| cfg.exception_edge_sources(h.catch_block));
    }
    let roots: HashSet<BlockId> = std::iter::once(cfg.entry)
        .chain(
            catch_sources
                .iter()
                .filter(|(_, sources)| sources.is_empty())
                .map(|(id, _)| *id),
        )
        .collect();

    for id in cfg.block_ids() {
        if roots.contains(&id) {
            let mut s = HashSet::new();
            s.insert(id);
            dom.insert(id, s);
        } else {
            dom.insert(id, all_blocks.clone());
        }
    }

    // Fixed-point iteration
    let mut rpo = cfg.reverse_postorder();
    for id in catch_sources.keys() {
        if !rpo.contains(id) {
            rpo.push(*id);
        }
    }
    let mut changed = true;

    while changed {
        changed = false;
        for &block_id in &rpo {
            if roots.contains(&block_id) {
                continue;
            }

            let mut preds = cfg.predecessors(block_id);
            if let Some(sources) = catch_sources.get(&block_id) {
                preds.extend(sources.iter().copied());
            }
            if preds.is_empty() {
                continue;
            }

            let mut new_dom = dom.get(&preds[0]).cloned().unwrap_or_default();
            for &pred in &preds[1..] {
                if let Some(pred_dom) = dom.get(&pred) {
                    new_dom = new_dom.intersection(pred_dom).copied().collect();
                }
            }
            new_dom.insert(block_id);

            if new_dom != *dom.get(&block_id).unwrap() {
                changed = true;
                dom.insert(block_id, new_dom);
            }
        }
    }

    dom
}

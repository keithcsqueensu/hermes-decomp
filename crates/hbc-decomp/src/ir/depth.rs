//! A recursion bound for the IR tree walkers.
//!
//! The IR mirrors the shape of the bytecode, and nothing in the bytecode bounds
//! how deeply an expression may nest: register propagation turns a chain of
//! `a = b + c; d = a + e; …` into one tree as deep as the chain is long. Every
//! renderer walks that tree recursively.
//!
//! **[measured]** Nesting `Expression::Binary` and calling `Display` on a 2 MiB
//! thread (the default for a `std::thread` worker, and for a tokio one):
//!
//! ```text
//! depth 1000: rendered ok
//! depth 5000: STATUS_STACK_OVERFLOW
//! ```
//!
//! ≈400 bytes of stack per level. A stack overflow is an **abort**, not a panic:
//! it cannot be caught, and it takes the process with it — which for the MCP
//! server means the client loses the session with no message.
//!
//! **[measured]** The real headroom, over all 62,018 IR functions of a shipped
//! React Native bundle: **max expression depth 79**, with 61,794 functions under
//! 10 and none above 200. So this is not a live problem for compiled JavaScript
//! and is trivially reachable on a crafted or generated input — which, for a tool
//! pointed at unknown APKs, is the job description rather than an edge case.
//!
//! `MAX_RENDER_DEPTH` sits between the two: comfortably above anything a real
//! bundle produces, and comfortably below the stack ceiling.
//!
//! ## What this does not cover
//!
//! Dropping a deeply nested `Box<Expression>` chain is itself recursive, and no
//! guard in a renderer can help with that — the tree has to be built before it can
//! be rendered. The second half of the mitigation is giving the threads that do
//! this work a real stack; see `crate::configure_thread_pool` and `hermes-mcp`'s
//! `main`.

use std::cell::Cell;

/// Maximum nesting a renderer will descend before emitting a marker instead.
///
/// 512 is ~6× the deepest expression observed in a real bundle (79) and ~10×
/// under the measured 2 MiB stack ceiling (≈5,000).
pub const MAX_RENDER_DEPTH: usize = 512;

/// What a renderer emits in place of a subtree it refused to descend into.
///
/// Deliberately a JS comment: it is syntactically inert wherever an expression or
/// a statement was expected, and it is greppable. Silence here would be the same
/// mistake as the rest of the read path's silent degradations.
pub const TOO_DEEP: &str = "/* hbc-decomp: nesting exceeds MAX_RENDER_DEPTH */";

thread_local! {
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// RAII depth counter. Holding one means "I have descended a level"; dropping it
/// gives the level back, including when the walk unwinds.
pub struct DepthGuard(());

impl DepthGuard {
    /// Descend one level, or return `None` if that would exceed the bound.
    ///
    /// The counter is thread-local, so parallel renders (the bulk decompile fans
    /// out across Rayon) each get their own budget rather than sharing one.
    pub fn enter() -> Option<Self> {
        DEPTH.with(|d| {
            let cur = d.get();
            if cur >= MAX_RENDER_DEPTH {
                None
            } else {
                d.set(cur + 1);
                Some(DepthGuard(()))
            }
        })
    }

    /// Current depth, for tests.
    #[cfg(test)]
    pub fn current() -> usize {
        DEPTH.with(|d| d.get())
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_returns_the_level_on_drop() {
        assert_eq!(DepthGuard::current(), 0);
        {
            let _a = DepthGuard::enter().expect("first level");
            assert_eq!(DepthGuard::current(), 1);
            {
                let _b = DepthGuard::enter().expect("second level");
                assert_eq!(DepthGuard::current(), 2);
            }
            assert_eq!(DepthGuard::current(), 1);
        }
        assert_eq!(DepthGuard::current(), 0);
    }

    // The regression this whole module exists for: rendering a pathologically
    // deep expression must terminate with a marker rather than abort the process.
    // Measured before the bound: `Display` died at ~5,000 levels on a 2 MiB stack
    // with STATUS_STACK_OVERFLOW, which is an abort and cannot be caught.
    #[test]
    fn deep_expression_renders_instead_of_overflowing_the_stack() {
        use crate::ir::{BinaryOp, Expression, Value};
        let handle = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024) // the default worker stack this used to die on
            .spawn(|| {
                let mut e = Expression::Value(Value::Register(0));
                for _ in 0..50_000 {
                    e = Expression::Binary {
                        op: BinaryOp::Add,
                        left: Box::new(e),
                        right: Box::new(Expression::Value(Value::Register(1))),
                    };
                }
                let rendered = format!("{e}");
                // Dropping a 50,000-deep Box chain is itself recursive and would
                // overflow this same small stack -- the guard cannot help with
                // that, so leak it rather than assert on an unrelated failure.
                std::mem::forget(e);
                rendered.contains(TOO_DEEP)
            })
            .expect("spawn");
        assert!(
            handle.join().expect("rendering must not abort the process"),
            "the bound must announce itself in the output, not truncate silently"
        );
    }

    #[test]
    fn guard_refuses_past_the_bound_and_recovers() {
        let mut held = Vec::new();
        for _ in 0..MAX_RENDER_DEPTH {
            held.push(DepthGuard::enter().expect("within bound"));
        }
        assert_eq!(DepthGuard::current(), MAX_RENDER_DEPTH);
        assert!(
            DepthGuard::enter().is_none(),
            "the bound must refuse rather than descend"
        );
        drop(held);
        assert_eq!(DepthGuard::current(), 0);
        assert!(DepthGuard::enter().is_some(), "budget is restored after unwind");
    }
}

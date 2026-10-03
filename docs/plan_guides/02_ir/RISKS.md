# IR — risk register

> **Ownership.** *Owns* the risk that the IR layer degrades silently. Today that is one
> finding, **F9** (unbounded render recursion), split here from the read-path hardening
> review because the hazard lives in the IR tree-walk (`ir/depth.rs`, `ir/visitor.rs`), not
> in parsing. *Delegates* the IR's *description* to `../../arch_guides/02_IR.md`, and the
> upstream framing of the F-series (the headline, the "record what happened" fix shape, the
> corruption-sweep harness) to `../01_read/RISKS.md`, which owns the pass these findings came
> from. The finding numbers (F1–F14) are shared across the read/ir/pipeline/frontends
> registers and indexed in `../README.md`; F9 keeps its number.

Status: ✅ fixed. Evidence tag **[measured]** means reproduced against the committed fixtures
or the shipped Equinox v96 bundle (see `../01_read/RISKS.md` for the bundle identity).

---

## F9 — recursion is bounded by stack size, not by a depth check

> **Fixed.** New `ir::depth` module: a thread-local RAII `DepthGuard` with
> `MAX_RENDER_DEPTH = 512`, applied at the three recursive renderers — `format_expr`
> (every other formatter in that file routes through it), `Codegen::generate_expr`, and
> `Codegen::generate_statements` for block nesting. Past the bound they emit
> `/* hbc-decomp: nesting exceeds MAX_RENDER_DEPTH */` rather than descending:
> greppable, syntactically inert, and not silent. 512 is ~6x the deepest expression
> measured in a real bundle (79) and ~10x under the 2 MiB stack ceiling (~5,000).
> `hermes-mcp`'s `main` now calls `configure_thread_pool()` at startup, so the 64 MB
> pool is configured before anything can initialise Rayon lazily — closing the
> cache-hit hole. Held by `deep_expression_renders_instead_of_overflowing_the_stack`,
> which renders a 50,000-deep tree on a deliberately 2 MiB thread. The `Drop`-recursion
> caveat is unchanged and documented in the module: a guard in a renderer cannot help
> with it, which is the other half of why the stack matters.


`lib.rs:38-52` configures a 64 MB Rayon stack with a comment saying the default 2 MB
"overflows and aborts the process on large real-world bundles". That is the mitigation: a
bigger stack, applied to one thread pool.

**[measured]** — nesting `Expression::Binary` and calling `Display`, on a 2 MiB thread:

```
depth   1000: rendered ok (5002 chars)
depth   5000: STATUS_STACK_OVERFLOW   (exit code 0xc00000fd)
```

≈400 bytes of stack per level. A stack overflow is an **abort**, not a panic: no
`catch_unwind`, no error, the process dies — which for the MCP server means the client loses
the session with no message.

**[measured]** — the real headroom, over all 62,018 IR functions of the Equinox bundle:

```
max expression depth: 79 (function 61510)
histogram <10 / <50 / <200 / <1000 / <5000 / >=5000:
          61794 / 222 / 2 / 0 / 0 / 0
```

So: **not a live problem for real React Native bundles** (60× headroom), and trivially
reachable on a crafted or generated one. This is an RE tool pointed at unknown APKs, so
"crafted input" is the job description, not an edge case.

Two related notes. `configure_thread_pool()` is called from `main.rs:48` (CLI) and from
`build_with_options` (`context/mod.rs:56`) — but **not** from `hermes-mcp`'s `main`, and
`build_cached` returns early on a cache hit *before* reaching it. Today nothing breaks,
because the rayon work in `rendering.rs:102,157` is only reached via paths that also build the
pipeline. It is an invariant with no assertion and no test, one refactor away from being false.
And `Drop` of a deeply nested `Box<Expression>` chain is itself recursive, so a depth guard in
`Display` alone is not sufficient.

**Fix.** A depth counter in the expression/statement walkers that emits
`/* expression too deeply nested */` past a limit; explicitly configure a large stack in
`hermes-mcp`'s `main`, or spawn tool work onto a thread with one.


---

## Appendix — harness

### A3 — expression depth ceiling (F9)

Nest `Expression::Binary` n deep, render it with `Display` on a thread created with
`.stack_size(2 * 1024 * 1024)`, and walk n upward until the process dies. Pair it with a pass
over `PipelineContext::all_ir` on a real bundle to measure actual headroom — the ceiling alone
is alarming; the ceiling next to "max observed 79" is a decision.


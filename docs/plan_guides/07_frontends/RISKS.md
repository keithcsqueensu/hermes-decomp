# Frontends — risk register

> **Ownership.** *Owns* the risk that the CLI/MCP surfaces mislead their consumer — a human
> patching by hand or an agent reading tool output as ground truth. Two findings, **F3** (the
> MCP surface had no output bound — `decompile_all` returned 41 MB) and **F4** (one panic
> poisoned the mutex and bricked the server for its lifetime), split here from the read-path
> hardening review because both live in `hbc-decomp-mcp`. *Delegates* the frontends'
> *description* — command surface, TUI, MCP tool list — to `../../arch_guides/07_FRONTENDS.md`,
> and the upstream framing of the F-series to `../01_read/RISKS.md`. Finding numbers are shared
> across the stage registers and indexed in `../README.md`; F3 and F4 keep theirs.

Status: ✅ fixed. Evidence tag **[measured]** means reproduced against the shipped Equinox v96
bundle over a real stdio MCP session (see `../01_read/RISKS.md` for the bundle identity).

---

## F3 — the MCP surface has no output bound

> **Fixed.** Three layers. Every tool response goes through `text_result`, capped at
> `MAX_RESPONSE_BYTES` (256 KiB) with an explicit tail naming the real size and telling
> the caller to narrow the request — truncation is never silent. `dump` and
> `xref_search` take `limit`/`offset` and state the window they returned.
> `decompile_all` *refuses* above 2,000 functions rather than truncating, pointing at
> `list_modules` + `decompile_module`: capping 41 MB at 256 KiB would return 0.6% of the
> answer while looking like it worked. Verified end-to-end over stdio against the real
> bundle — `dump kind=strings` came back capped with `this response was 5382496 bytes`,
> and `decompile_all` refused with the pointer. Held by four `cap_text` tests including
> the multibyte-boundary case.


**[measured]** on the Equinox bundle:

| tool | output |
|---|---|
| `decompile_all` | **41,447,553 bytes** (17 s) |
| `dump --kind strings` | 5,839,550 bytes |
| `dump --kind functions` | 3,717,753 bytes |
| `callgraph` (no root) | 303,412 bytes (14 s — cost is in `analyze_module`, not the string) |

Of the 21 tools in `tools_analyze.rs`, exactly **one** (`list_modules`) takes a `limit`, and
one (`dead_code`) hardcodes `take(200)`. `decompile_all`, `dump`, `dump_table`, `xref_search`,
`disassemble` and `callgraph` are all unbounded, and each returns a single
`ContentBlock::text`.

41 MB into an agent's context is not a degraded result, it is a failed call — and an expensive
one. `render_call_graph` with `root: None` also builds the whole edge list into one `String`
before anything can truncate it.

**Fix.** `limit`/`offset` on every listing tool, a default cap (a few hundred KB) with an
explicit `"… truncated, N of M shown, pass offset=N"` tail, and a hard refusal on
`decompile_all` for bundles above some function count, pointing at `decompile_module`.


## F4 — one panic bricks the MCP server permanently

> **Fixed.** `HermesService::lock` recovers from poisoning via `into_inner()` (the
> data behind the lock is a parsed file plus a memoised context — a panic mid-read
> cannot leave it half-updated), and every tool body runs inside `catch_tool_panic`,
> which turns a panic into one failed call carrying the panic message and a note that
> the session survived. Held by `a_panic_does_not_brick_the_service`, which genuinely
> poisons the mutex and then asserts a normal tool call still returns its normal
> error.


`server/mod.rs:29` — `loaded: Mutex<Option<LoadedFile>>`, and every tool goes through
`with_file` / `with_file_mut`, which map a lock failure to an error. `std::sync::Mutex`
**poisons** on a panic while held. So any panic inside any tool body — F7's overflow (now in
`../01_read/RISKS.md`), an
unforeseen index, a future regression — does not merely fail that call: it makes
`self.loaded.lock()` return `Err` for the rest of the process, and every subsequent tool
returns `lock: poisoned`. The server stays up, answers nothing, and gives no hint that a
restart is the fix.

There is no `catch_unwind` anywhere in either binary (the sole one in the tree is in the
TUI's git-diff view, `tui/gitdiff.rs:249`).

**Fix.** Recover from poisoning (`.unwrap_or_else(|e| e.into_inner())`) — the invariant being
protected is "a parsed file", which a panic mid-read does not corrupt — and wrap tool bodies
in `catch_unwind` so a panic becomes one failed call with a diagnosable message.

A related note: `pipeline_ctx.as_ref().unwrap()` at `tools_analyze.rs:115, 228, 258, 560, 586`
is locally sound (each is preceded by `ensure_pipeline()?`), but it is five unwraps standing
on a call-order convention. A `let … else { return Err(…) }` costs nothing.


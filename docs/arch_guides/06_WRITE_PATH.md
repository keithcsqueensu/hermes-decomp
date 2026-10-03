# 06 — The write path: edit, assemble, serialize

> **Ownership.** *Owns* the structural map of the outbound path — the `write/` module: encode
> instructions, assemble HASM, patch an existing image, serialize a full `.hbc`. It
> deliberately **delegates the depth** — invariants, hazards, the op inventory, the risk
> register, and all open work — to `../plan_guides/`, which mirror this spine stage-for-stage
> (the write stage's register is `../plan_guides/06_write/RISKS.md`) and are indexed in
> `../plan_guides/README.md`. This guide is a pointer with a table, not a second copy (the
> plan-guides' one-home-per-fact rule applies across folders too).

Files: `write/` — `encode.rs`, `serialize.rs`, `create.rs`, `footer.rs`, `header_write.rs`,
`reloc.rs`, `hasm/*`, `patch/*`.

---

## What it does — and what it does not

The write path takes a parsed (or hand-authored) bytecode model and emits bytes: it can
re-encode instructions, assemble textual HASM back into a function body, surgically patch an
existing bundle (string tables, operands, whole function bodies, injected stubs), and
serialize a complete `.hbc` image. It does **not** recompile decompiled JavaScript — there is
no JS → bytecode compiler here; that is hermesc's job. The read path (guides 01–05) and the
write path meet only at the `BytecodeFile` model.

Two properties define it, and both are the reason the plan_guides exist:

- **Identity and payload are separate derivations.** A patch is located by structure, then
  its bytes are re-derived from the instruction being replaced — registers and string-ids are
  never carried forward. (This is the same doctrine the decompiler applies to naming; see the
  cross-cutting principles in [`README.md`](README.md).)
- **Refuse rather than approximate.** An edit that would relay out the whole image, or that
  hits an unmodeled region, errors rather than shipping a wrong address.

## Structure

| Concern | File(s) | Notes |
|---|---|---|
| **Encode** | `encode.rs` | decoded instructions → raw bytecode bytes (`encode_instruction`, `encode_function_body`) |
| **Serialize** | `serialize.rs` | full `.hbc` image; primary path is identity re-emit from `raw_bytes` (preserves unmodeled regions) |
| **Create** | `create.rs` | minimal valid `.hbc` from scratch (`create_minimal`, `CreateOptions`) |
| **Footer** | `footer.rs` | SHA-1 over all preceding bytes — refreshed after any edit |
| **Header write** | `header_write.rs` | binary writers for HBC headers (legacy layout) |
| **Relocation** | `reloc.rs` | helpers after size-changing edits; most reloc currently lives in `patch::patch_function_bytes` — see `../plan_guides/06_write/relocation/PLAN.md` |
| **HASM** | `hasm/{mod,parse,emit}.rs` | our disasm dialect: `emit` text ← bytecode, `parse` text → instructions, assemble into a patched image |
| **Patch** | `patch/mod.rs` + submodules | edit an existing image, split by concern (below) |

`patch/` submodules:

| File | Edit |
|---|---|
| `patch/strings.rs` | string-table entries: same-length in place, or grow/shrink with a full table + storage rebuild and tail relocation |
| `patch/operands.rs` | a single string-id operand in one instruction — no body rebuild; validates shape, read-back verifies |
| `patch/functions.rs` | whole function bodies: same-size in place; different-size splices the code section, shifts later offsets, fixes the debug-info offset |
| `patch/inject.rs` | inject a stub into a body: a runtime no-op pad, or a `print(<name>)` entry-logging prologue |
| `patch/debug_reloc.rs` | keep a function's debug line table correct across an *insertion* (the R24 relocation) |

## Where the depth lives — `../plan_guides/`

This guide stops at "what the files are." Everything about *why* and *what's unfinished* is in
the plan guides, which have their own index (`../plan_guides/README.md`):

| Question | Guide |
|---|---|
| Write-path invariants, hazards, op inventory, risk register | `../plan_guides/06_write/RISKS.md` |
| The absolute-offset surface; splice-and-shift; should `RelocPlan` exist | `../plan_guides/06_write/relocation/PLAN.md` |
| Per-region read/interpret/emit status; how each unmodeled region serializes | `../plan_guides/01_read/unmodeled_regions/PLAN.md` |
| How string storage is laid out and could be repacked (no dedup today) | `../plan_guides/06_write/string_packing/PLAN.md` |
| The decompiler's closure/env-slot model (read-side, but chained here) | `../plan_guides/03_analysis/closure_model/PLAN.md` |
| Read-path robustness findings, incl. F10 (silent debug-info failure) | `../plan_guides/01_read/RISKS.md` |

Do not restate a plan-guide fact here; add a row and a pointer.

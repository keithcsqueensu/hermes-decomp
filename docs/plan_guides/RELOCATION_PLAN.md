# Impl plan — relocation

`WRITE_PATH_GUIDE.md` lists, under design limits:

> **`apply_reloc` on structured headers is intentionally unimplemented** — it errors and points
> callers at `patch_function_bytes`/`finalize_raw_image` (`reloc.rs:23`). `RelocPlan` is a
> placeholder type for a future structured-rebuild path.

That is true, and it is the right refusal. What the bullet does not say is the more useful half:
the thing the stub names — relocation over *structured headers* — is not what the write path
needs, and the thing it does need is **written out three times by hand**, in three files, with
three slightly different contracts. This plan is about closing that gap, and about deciding
whether the placeholder should exist at all.

Read alongside `UNMODELED_REGIONS_PLAN.md` (which owns the *contents* of the debug section)
and `STRING_PACKING_PLAN.md` (which owns how the string region is rebuilt before it is spliced).
Neither overlaps this one: both stop at "and then everything after the region shifts", which is
exactly where this begins.

---

## What is actually true today

### The stub

`write/reloc.rs` is 31 lines. `RelocPlan { code_delta, string_storage_delta, resized_functions }`
plus `identity()`, and:

```rust
pub fn apply_reloc(_file: &mut BytecodeFile, plan: &RelocPlan) -> Result<()> {
    if plan.code_delta == 0 && plan.string_storage_delta == 0 { return Ok(()); }
    Err(Error::Write("apply_reloc on structured headers: use patch_function_bytes / ...".into()))
}
```

Note `_file`: it cannot do anything with the file even in principle. Nothing in the crate
constructs a `RelocPlan` — the only mention outside its own module is the re-export at
`write/mod.rs:26` — so this is an exported type with no producer and a function with no caller.

The field names also describe a design the write path does not have. `code_delta` and
`string_storage_delta` are independent scalars, as if two regions could move independently;
every shipped op splices **exactly one** region and shifts everything after it by one delta.

### Relocation is implemented three times

| Site | Region spliced | Which offsets shift | How it writes headers |
|---|---|---|---|
| `patch/functions.rs:168` (`patch_function_bytes`) | one function body | those `>= end of the patched body` | legacy small: **re-encoded from the model**; modern + overflowed: shifted in place |
| `patch/strings.rs:461` (`patch_string_resize`) | the whole string region | all of them (every body is past it) | shifted in place |
| `patch/strings.rs:756` (`add_string`) | the whole string region | all of them | shifted in place |

The two string loops are byte-identical to each other. The overflow case is duplicated a second
time underneath them: `functions.rs:285 resize_overflowed_function` and
`strings.rs:501 relocate_overflowed_header` do the same job — shift the small header's pointer to
the out-of-line large header, then the large header's own body offset and (legacy) info offset —
with the former adding a threshold test and an optional size write.

All three then do the same two tail steps by hand: shift `debug_info_offset` in the header bytes,
and hand the buffer to `commit_image` (`serialize.rs:114`), which finalizes length + footer and
re-derives the model.

So the shared primitive already exists conceptually. It exists in the codebase as three copies
and one stub, and the stub is the only one that does not work.

### RE1 — the copies do not agree on their source of truth *(registered as R26)*

The legacy non-overflowed branch of `patch_function_bytes` does not shift bits; it calls
`write_function_header_legacy_small(...)` (`header_write.rs:10`) with the *decoded* fields and
overwrites all 16 bytes. The string paths call `shift_legacy_small_header_offsets`
(`header_write.rs:131`) and touch only the two offset bitfields.

The difference matters for blast radius, not for today's output. Re-encoding means every field
the decoder normalizes gets silently rewritten, for **every function in the table**, on any body
resize — so a decode bug in one field stops being a wrong value in one place and becomes a
corrupted header table.

Whether it is lossless today is measurable, so it was measured, on the 11.39.0 Equinox bundle
(v96, `Legacy16`, 62,909 functions) **[measured]**:

```
checked 62894, overflowed(skipped) 15, mismatched 0
```

Byte-identical for every non-overflowed legacy header. So this is not a live defect — it is an
unnecessary second contract, currently correct and pinned by nothing.

### What actually refuses today

The demand for a structured path, in full:

- **Size change on a function that declares an exception handler** — refused (`functions.rs:43`,
  the Q4 guard). Handler entries are *body-relative*, so this is not a file relocation at all;
  Q3 in the guide owns it.
- **`create` cannot emit overflow string entries** (`serialize.rs:153`, `:292`). A serializer
  limit, not a relocation one.
- **No op inserts or removes a function.** This is the only shipped-adjacent case a structured
  rebuild would genuinely serve, and nothing asks for it yet.

Nothing that ships today needs `apply_reloc` on structured headers. That is why the refusal has
survived: it is correct, and its cost has been zero.

---

## Derived facts — the relocation surface **[source]**

### There is no section table to fix up

The HBC header stores **counts and sizes**, not section offsets (`format.rs:53`). Sections are
implied by sequential layout with 4-byte alignment, so growing a region relocates everything
after it without any table needing an update. The whole absolute-offset surface of the format is:

| # | Field | Where | Who owns it today |
|---|---|---|---|
| 1 | function body offset | small header (legacy 25-bit / modern 25-bit) or large header's first `u32` | all three loops |
| 2 | `info_offset` (FunctionInfo: handler table + debug offsets) | legacy small header; legacy/modern large header | all three loops |
| 3 | large-header pointer | small header of an overflowed function | the two overflow relocators |
| 4 | `debug_info_offset` | file header (byte 108 modern; `legacy_debug_info_offset_pos` otherwise) | all three loops, separately |
| 5 | `file_length` | file header bytes 32..36 | `finalize_raw_image` (`serialize.rs:66`) |
| 6 | SHA-1 footer | last 20 bytes | `finalize_raw_image` |

Five and six are already centralized. One through four are not.

### `cjs_module_offset` is **not** a file offset

It is only present below `LEGACY_SEGMENT_ID_MIN_VERSION`, where it occupies the slot later taken
by `segment_id` (`file/parser/header.rs:59`). Upstream **[source]**:

```c
uint32_t cjsModuleOffset; // The starting module ID in this segment.
```

A module ID base. Shifting it by a byte delta would corrupt module resolution on exactly the old
versions nobody tests. It has never been shifted wrongly only because no generic pass exists —
which is the argument against writing one that walks the header looking for things named
`*_offset`.

### What must not move, and why

| Structure | Addressing | Consequence |
|---|---|---|
| Exception handler tables | body-relative (`start`/`end`/`target`) | a whole-region shift moves table and code together and stays valid — which is why `add_string` on a handler-bearing bundle is safe while a body resize is not (Q3/Q4) |
| RegExp storage | storage-relative | shifting the section cannot invalidate it |
| Array / object buffers | index-based | untouched by any delta |
| CJS module table | function **indices** | insertion/removal would invalidate it; a size delta cannot |
| `global_code_index` | function index | same |
| `DebugOffsets`, location streams | section-relative, then body-relative deltas | the section moving is fine; the internals are R24's problem and a relocation must never touch them |

### The 4-alignment rule (I5)

`patch_function_bytes` pads the body so the delta is a multiple of 4 (`functions.rs:59`);
`patch_string_resize` pads the rebuilt region to 4 (`strings.rs:419`). Same rule, two
enforcement points: a non-4-aligned delta misaligns every downstream large header and the
FunctionInfo region. One primitive should assert it once, with the reason attached.

---

## Plan

### P0 — Make the promise honest *(30 minutes)*

Delete `apply_reloc` and `RelocPlan`, or replace them with P1's real signature. Do not leave the
current pair in place: an exported type with no producer, whose field names imply a design the
crate does not use, is R20's shape — a reference that reads as a capability and is not one. The
guide's limitation bullet already points here; after P0 it describes a decision taken rather
than a placeholder left.

`RelocPlan` is `pub use`d, so removing it is a breaking change to the public API. The crate is
0.x and the type is inert, so this is a version-number question, not a design one.

### P1 — One relocation primitive *(the actual work, ~1 day)*

```rust
/// One spliced region, and the size change it caused.
pub struct Reloc {
    pub splice_at: usize,          // file offset where the region was replaced
    pub delta: i64,                // signed size change; must be 4-aligned (I5)
    pub resized: Option<(u32, u32)>, // (function_id, new body size) when a body changed
}

/// Shift every absolute offset in `buf` that points at or past `splice_at`.
///
/// Reads offsets from the **bytes**, never from `file`; `file` supplies only the
/// layout (version, header layout, section starts). The model is not updated --
/// `commit_image` re-derives it, which is invariant I1.
pub fn relocate(file: &BytecodeFile, buf: &mut [u8], r: &Reloc) -> Result<()>;
```

Behaviour is the union of what the three loops already do, with the differences resolved:

- assert `delta % 4 == 0` and say why in the message (I5);
- per function: shift the body offset, the legacy small header's `info_offset`, and for an
  overflowed function the large-header pointer plus the large header's own body and info offsets
  — each only when its current value is `>= splice_at`;
- shift `debug_info_offset` when non-zero;
- write the new body size for `resized`;
- touch nothing else. `overflow_string_count`, `string_storage_size`, identifier hashes and the
  string model stay with their callers — they are not relocation.

The string paths pass `splice_at = start of the string region`, which makes their "shift
everything" and the function path's threshold the same rule rather than two.

**Tests.** Unit coverage per layout arm (legacy small, legacy overflowed, modern small, modern
overflowed) is the floor. The one that earns its keep is a **differential over the corpus**:
`add_string` and a body grow of the same 4-aligned delta, applied to the same bundle, must
produce the same shift for every header that is past both splice points. That is the assertion
the three-copy structure cannot currently make, and it is the one that would catch a fix landing
in one copy and not the others. The existing `vm_verify` suite is the backstop: the primitive is
a refactor, so every op must still run on a real engine with unchanged output.

### P2 — Retire the legacy re-encode *(folded into P1)*

`patch_function_bytes` should shift the legacy small header in place like everything else, which
deletes RE1 rather than documenting it. If the re-encode is kept for some reason not visible
here, pin it instead: the probe above is three lines of assertion over a corpus bundle, and
without it the property is only known to hold on one bundle on one day.

### P3 — The structured rebuild, *if* an op ever demands it

This is what the stub was reaching for, and it is a different order of work: emitting a complete
image from the model rather than splicing the one the parser read. It becomes necessary only when
an op cannot be expressed as "splice one region, shift what follows" — realistically:

- inserting or removing a function (the function-header table itself resizes, and the CJS table
  and `global_code_index` become index-invalid);
- repacking the string table wholesale (`STRING_PACKING_PLAN.md` P2/P3).

What it requires, none of which exists: a total serializer covering debug info
(`UNMODELED_REGIONS_PLAN.md`), RegExp, CJS modules, the function source table, and the object
shape table — every region currently preserved only because it is copied through verbatim.

**The gate, if it is ever built.** Byte-identical re-emit of the 11.39.0 bundle
(`serialize(parse(b)) == b`, 5 MB, no exceptions), *plus* the `hbcdump` differential, *plus* a VM
run. Anything less ships a rebuild that reparses and is wrong somewhere in the middle — the
failure mode this codebase keeps finding.

**And it must not become a second source of truth.** The write path deliberately makes the model
*derived* (I1/R5, `commit_image`). A structured rebuild inverts that for the duration of one op,
so it needs the inverse invariant written down and tested — `parse(serialize(m)) == m` — rather
than left implicit.

---

## Non-goals

- **A generic "shift every u32 that looks like an offset" pass.** `cjs_module_offset` is the
  counterexample, and it is in the header, on the versions with the least test coverage.
- **Relocating debug-info internals or handler-table entries.** Both are body-relative, both have
  owners (R24, Q3), and a file relocation that touched them would be wrong.
- **Changing section order or layout.** Relocation preserves layout by definition; anything that
  reorders sections is P3's problem, not this one's.
- **Making `create` a general emitter.** It builds minimal images; P3 is where a general emitter
  would come from, if ever.

---

## Ordering

1. **P0 + P1 + P2 as one change.** Roughly a day. It removes two duplicate implementations and
   one dead promise, and it is the prerequisite for anything that touches offsets afterwards
   (including `STRING_PACKING_PLAN.md` P1, which splices a differently-sized string region and
   would otherwise be a fourth copy).
2. **P3 only on a named trigger** — the first op that inserts a function or repacks the table.
   Until then the honest state is "the write path splices one region and shifts what follows",
   which is a real design, not a limitation to apologize for.

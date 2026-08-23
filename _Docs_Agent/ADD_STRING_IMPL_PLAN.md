# Implementation Plan — `add-string`

> Status: **implemented** on branch `feat/add-string`. All algorithm steps, CLI wiring,
> exports and tests (9 total, including 3 added during plan audit) are complete. The
> optional shared `rebuild_string_region` refactor is deferred.
> File-path/line-number citations below are accurate as of the commit this doc was
> written against; re-grep before relying on an exact line.

## Goal & scope

Append a **new** entry to the string table and return its id (`= old string_count`), so it
can be referenced by subsequent patches (property names, string literals). Appending at the
end keeps every existing id stable, so **no bytecode operand or debug-info reference needs
remapping** — the single biggest simplification.

**In scope:** plain strings and identifiers; UTF-8/UTF-16 by content; legacy *and* modern
(v97+) layouts.
**Out of scope:** deduplication semantics beyond an optional warning; wiring the string into
any instruction (that is a separate patch).

## Why modern (v97+) is not a blocker

The length-altering write path is already modern-aware: `patch_string_resize` branches on a
`modern` flag throughout (12- vs 16-byte headers, flag byte 11 vs 15, debug offset at byte
108, modern large-header pointer relocation — `write/patch/strings.rs:237,374,384,424-430`).
The only thing still modern-limited is building a file *from scratch* (`create`), which
`add-string` does not touch. `add-string` inherits modern support by reusing the resize
path's downstream-shift code.

## Design decisions

1. **Append at the highest id.** New string → id `N` (old count). Its identifier hash (if
   any) and kind run both go at the *end* of their tables, since both are ordered by
   string-id. Clean and remap-free.
2. **`--identifier` flag.** Caller declares whether the new string is an Identifier
   (symbol/property name, needs a Jenkins hash slot) or a plain String (literal). These are
   different string *kinds* and cannot be inferred from the text.
3. **Encoding by content.** ASCII → 1-byte; any non-ASCII → UTF-16. Reuse the existing rule
   at `strings.rs:277-306`.
4. **Duplicate handling.** Default: append unconditionally (predictable id). Emit a `note:`
   if the value already exists and print the existing id too, so the caller can choose to
   reuse it. No silent dedup — the file has none elsewhere.

## The core structural change vs. `patch_string_resize`

`patch_string_resize` splices at `small_off` and treats `string_kinds` + `identifier_hashes`
as a **fixed-size prefix** (`strings.rs:344`; in-place-only `update_identifier_hash` at
`:188`). Appending must *grow* those two pre-`small_off` tables. So the one real change is:
**move the rebuild origin earlier, from `small_off` to `kinds_off`**, and rebuild the whole
contiguous block `[kinds_off, array_off)` with the new entry included. Everything downstream
of that block (buffers, bytecode, FunctionInfo, debug) is copied shifted by a single `delta`,
exactly as resize already does.

On-disk section order (from `parser/parsing.rs:152-166`, restated in `serialize.rs:220-222`):

```
function_headers -> string_kinds -> identifier_hashes -> small_string_table
  -> overflow_string_table -> string_storage -> array/literal buffers -> ... -> debug
```

## Library function

Add to `crates/hbc-decomp/src/write/patch/strings.rs`:

```rust
pub fn add_string(
    file: &mut BytecodeFile,
    format: &BytecodeFormat,
    value: &str,
    is_identifier: bool,
    opts: &PatchOptions,
) -> Result<(Vec<u8>, u32 /* new id */)>
```

### Algorithm

1. **Derive the six boundaries** from header counts + section order: `kinds_off`, `ids_off`,
   `small_off`, `overflow_off`, `storage_off`, `array_off`. Sizes: kinds `= string_kind_count*4`,
   id_hashes `= identifier_count*4`, small `= string_count*4`, overflow `= overflow_string_count*8`.
   (resize already computes `small_off`/`array_off`; extend to the two earlier ones.)
2. **Read existing locs** via `read_all_string_locs` (`strings.rs:108`) — reuse verbatim.
3. **Append the new entry** to the working list: `is_utf16` by content; push `value`.
4. **Rebuild storage + small table + overflow table** over `N+1` entries — reuse the existing
   loop (`strings.rs:273-338`), iterating one more. Overflow entries are re-derived for *all*
   strings, so adding one may flip others into overflow (offset ≥ `0x800000` or len ≥ `0xff`);
   do **not** assume the overflow delta is 0 or 8.
5. **Rebuild `string_kinds`**: copy existing runs; if the last run's kind matches the new
   string's kind, `count += 1`; else append a new run (`u32`, bit31 = identifier). A new run
   ⇒ `string_kind_count += 1`.
6. **Rebuild `identifier_hashes`**: copy existing; if identifier, append
   `hermes_identifier_hash(value)` (`strings.rs:162`, Jenkins one-at-a-time over UTF-16 units,
   already hermesc-verified) at the end ⇒ `identifier_count += 1`.
7. **Assemble block** = `new_kinds ++ new_idhashes ++ new_small ++ new_overflow ++ new_storage`.
   `delta = block.len() - (array_off - kinds_off)`.
8. **Splice**: `out = raw[..kinds_off] ++ block ++ raw[array_off..]`.
9. **Update header counts** in `out` (little-endian u32, fixed offsets shared by both layouts):

   | field | bytes | change |
   |---|---|---|
   | `string_kind_count` | 44..48 | +1 iff new run |
   | `identifier_count` | 48..52 | +1 iff identifier |
   | `string_count` | 52..56 | **+1 always** |
   | `overflow_string_count` | 56..60 | = rebuilt count |
   | `string_storage_size` | 60..64 | = rebuilt size |

   → 44/48/52 are the **new** writes; resize already handles 56/60.
10. **Shift downstream offsets by `delta`** — reuse resize's block verbatim
    (`strings.rs:352-397`): `debug_info_offset` (byte 108 modern / `legacy_debug_info_offset_pos`),
    every function-header body/info offset, and overflowed large-header pointers via
    `relocate_overflowed_header` (`:417`). Already `modern`-aware.
11. **`finalize_raw_image`** (`serialize.rs:66`) — rewrites `file_length` (bytes 32..36) and
    the SHA-1 footer.
12. **Sync the in-memory model** (`file.strings.push`, `file.string_kinds`,
    `file.identifier_hashes`, `file.header.*_count`, `file.raw_bytes = out.clone()`) so a
    follow-up op in the same process sees a consistent file. Return `(out, N)`.

### Optional refactor (recommended, can defer)

Extract a shared `rebuild_string_region(entries, kinds, id_hashes) -> (bytes, counts)` that
both `patch_string_resize` and `add_string` call, so the two paths cannot drift. Nice-to-have;
a first cut can duplicate the loop and consolidate later.

## CLI wiring (3 files, mirror `patch-string`)

1. **`crates/hbc-decomp-cli/src/cli_args.rs`** — new `Command::AddString { input,
   #[arg(short,long)] output, value: String, #[arg(long)] identifier: bool, format_version,
   layout, function_layout }` (mirror `PatchString` at `:422`).
2. **`crates/hbc-decomp-cli/src/main.rs`** — match arm → `commands::write_cmd::run_add_string(...)`
   (mirror `:508`).
3. **`crates/hbc-decomp-cli/src/commands/write_cmd.rs`** — `run_add_string`: `load_file` →
   `load_format` → `warn_modern_write` → `add_string(...)` → `std::fs::write(output, out)` →
   `println!("added string id {new_id}")` (mirror `run_patch_string` at `:150`).
4. **Exports** — add `add_string` to the `pub use` in `write/patch/mod.rs:12` and `lib.rs:114`.

## Tests (`strings.rs` `#[cfg(test)]`, mirror existing resize/inject tests)

- **Legacy append + reparse**: plain ASCII → reparses, `string_count+1`, value at new id.
- **UTF-16**: append non-ASCII → utf16 flag set, decodes back.
- **Identifier**: `identifier_count+1`; hash equals `hermes_identifier_hash` (model on
  `identifier_hash_matches_hermes` at `:613`).
- **Kind-run growth**: append matching last kind (count bumps, `string_kind_count` unchanged)
  vs. differing kind (new run, `string_kind_count+1`).
- **Modern v98**: append + reparse against `examples/react-native/v98/.../bytecode.hbc`
  (mirror `inject_stub_modern_v98_grows_and_reparses` at `inject.rs:291`).
- **Overflow threshold**: force storage past `0x800000` → overflow entry created, reparses,
  other entries relocated correctly.
- **Roundtrip integrity**: after append, all first-N strings unchanged and a string-referencing
  function still disassembles (downstream offsets intact).

## Risks to validate

1. **Identifier ordering (the one behavioral unknown).** Hermes must accept a new *trailing*
   Identifier run. The in-place hash rewrite already works (table is string-id-ordered, not
   hash-sorted), which is strong evidence, but confirm with the reparse + real-engine check.
2. **Overflow cascade.** One append can flip *other* strings into overflow; the full
   re-derivation in step 4 handles it, but the overflow test must exercise it deliberately.
3. **"Verified on a real engine."** The resize path is reparse-verified and comment-notes a
   one-time v98 VM check, but the named verifier script
   `scripts/build/build_hermes_v98_toolchain.sh` is **not** in the repo. Modern validation
   rests on in-repo reparse tests; treat real-engine acceptance of a newly appended
   *identifier* as unproven until run.

## Sequencing & effort

1. `add_string` library fn + exports — **~0.5–1 day** (reuses resize helpers; new work is
   items 5, 6, 9).
2. Tests (legacy, modern, identifier, kind-run, overflow) — **~0.5 day**.
3. CLI wiring — **~1–2 hrs**.
4. Manual validation on a real Equinox bundle: append an identifier, reparse, `xref` it.

**Total: ~1.5–2 days**, low risk except the identifier-ordering unknown, which the modern
reparse test surfaces early.

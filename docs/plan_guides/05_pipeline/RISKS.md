# Pipeline — risk register

> **Ownership.** *Owns* the risk that the orchestration/cache layer returns a stale or wrong
> result that looks valid. Two findings, **F8** (the cache `options_key` was hand-synced to
> two fields) and **F13** (cache temp-file race, and the 134 MB unauthenticated cache),
> split here from the read-path hardening review because both live in `pipeline/cache.rs`;
> plus two unnumbered items found later (the `cascade` option vs. the cache, and
> `stages.rs` drift), both now fixed.
> *Delegates* the pipeline's *description* — the F/W stage spine, `PipelineContext`, the cache
> key design — to `../../arch_guides/05_PIPELINE.md`, and the upstream framing of the F-series
> to `../01_read/RISKS.md`. Finding numbers are shared across the stage registers and indexed
> in `../README.md`; F8 and F13 keep theirs.

Status: ✅ fixed (F8; F13's race). F13's size/trust notes are documentation items, recorded
not changed. Two items found since, unnumbered (the F-series belongs to the read pass), both
at the end and both ✅ fixed and test-guarded: the `cascade` option vs. the cache, and stage
labels drifting from `stages.rs`. Evidence tag **[measured]** means reproduced against the shipped Equinox v96
bundle (see `../01_read/RISKS.md` for its identity).

---

## F8 — the cache key is hand-synced

> **Fixed.** `options_key` now hashes the whole `DecompileOptionsV2` (which gained
> `Hash`), so a new field cannot desync the key from what `build_with_options` reads.
> Held by `every_option_field_changes_the_cache_key`, which flips each field in turn
> and asserts the key moves — and destructures the struct exhaustively, so adding a
> field without adding it to the test stops compiling.


The pre-fix code, as found (`pipeline/cache.rs:66` then; the hashed `options_key` is at
`cache.rs:81` now):

```rust
fn options_key(options: &DecompileOptionsV2) -> u32 {
    (options.assembly_mode as u32) | ((options.include_offsets as u32) << 1)
}
```

This was correct **then**: `build_with_options` (`pipeline/context/mod.rs:60-61` then) read exactly
those two fields and forced the rest to `optimized()`. Nothing enforced it. (It has since
happened three times over: `deep`, `stable` and `cascade` were added and are all read by
`build_with_options`, `context/mod.rs:69-76` now — after the fix, so the key moved with
them.) Add a seventh field
to `DecompileOptionsV2`, consume it in `build_with_options`, and every cache hit silently
returns a context built with the old value — with the file hash and binary fingerprint both
matching, so the cache looks perfectly valid.

The rest of the cache design is careful (SHA-256 of the bytes, a build.rs fingerprint that
auto-invalidates on any rebuild, temp-file-then-rename). This one field is the exception, and
it is the same "partly-stale model, hand-synced" shape that `../06_write/RISKS.md`'s `commit_image`
harness found in **every** write op.

**Fix.** Derive the key from the whole struct — `#[derive(Hash)]` plus a `DefaultHasher`, or
serialize it. Over-invalidation costs one rebuild; under-invalidation costs a wrong answer
that looks right.


## F13 — cache hygiene

> **Fixed** (the race). The temp file is now `...hdcache.<pid>.tmp`, so concurrent
> processes cannot interleave into one another's write. The 134 MB size and the
> unauthenticated-cache note are documentation items rather than defects — recorded
> here rather than changed.


- **Temp-file race.** `cache.rs:306` (then; the pid-tagged name is at `cache.rs:281` now) —
  `path.with_extension("hdcache.tmp")` was a fixed name.
  Two processes analysing the same bundle write the same temp file concurrently; the rename is
  atomic but the *content* is interleaved. It degrades to a cache miss (`try_load`'s
  `rmp_serde…ok()?`), never to a wrong answer, but it leaves a corrupt file in place until
  something rewrites it. A PID/random suffix fixes it.
- **Size.** **[measured]** the `.hdcache` for the 16,837,408-byte Equinox bundle is
  **134,208,814 bytes** — 8× the input, written silently next to it, with no eviction and no
  mention in the docs. Worth stating in `USAGE.md` at minimum. (Still open: `USAGE.md`
  § Analysis cache now describes the cache and its keys, but not its size.)
- **Trust.** The cache is unauthenticated MessagePack whose header check requires only the
  file hash and the build fingerprint — both derivable by anyone who can write next to the
  input. It deserializes into plain data (no code), so the ceiling is falsified analysis
  output, not execution. Low risk for a local tool; worth one sentence in the doc rather than
  a fix.


## The `cascade` option vs. the cache — ✅ fixed

> **Fixed by option (a) below.** `PipelineContext::build_cached` never reads or writes the cache
> when `cascade` is set, so the guard lives in the library and every caller gets it. Pinned by
> `cache.rs::tests::a_cascade_build_bypasses_the_cache` (no entry written; an existing entry
> neither served nor touched), which fails with the bypass removed. Found re-auditing after
> `DecompileOptionsV2` gained `cascade: Option<PathBuf>`.

Two gaps, both in `pipeline/cache.rs`, both the F8 shape again — a cache hit that looks valid
but was built from different input:

- **The key hashes the artifact's path, not its contents.** `options_key` hashes the whole
  struct (F8's fix), so `cascade` participates — but as a `PathBuf`. Edit the proposal file in
  place and rerun: same bytecode, same build, same path, so the stale context is served.
- **`cascade_names` is not in the snapshot.** `PipelineSnapshot` stores six fields;
  `from_snapshot` sets `cascade_names` empty. The names applied to `all_ir` at W4b survive the
  round-trip, but the rendered *header* names (`context/codegen.rs:generate_function_code`,
  which prefers `cascade_names`) fall back to the string table — `fN` for an anonymous
  function — on every hit.

Before the fix only the CLI sidestepped both, by forcing `no_cache` whenever `--cascade` is
given (`hbc-decomp-cli/src/main.rs:246`, still there, now redundant); a library caller of
`PipelineContext::build_cached` or `decompile_*_cached` with `cascade: Some(..)` had no guard,
and the comments in `pipeline/mod.rs` and `cache.rs:from_snapshot` said the cache "does not key
on" the artifact when it keyed on the path. Those comments now describe the bypass.

**The options were:** (a) refuse — `build_cached` bypasses load *and* save when `cascade` is set;
or (b) key on the artifact bytes *and* add `cascade_names` to the snapshot, bumping
`CACHE_VERSION`. (a) shipped: a few lines, and it matches "the cache is an optimization". A
proposal changes between runs by design, so (b) would rarely hit anyway.

## Stage labels drifting from `stages.rs` — ✅ fixed

> **Fixed.** `stages.rs` lists every stage in run order again, and
> `stages.rs::tests::stage_markers_are_listed_and_unique` fails if a `// STAGE <id>:` marker in
> `ir_gen.rs` or `context/{mod,naming,transforms_phase}.rs` is missing from it or used twice.
> Verified to fail on both: a reused label and an unlisted one.

**What it was.** `stages.rs` had not changed since the W/F numbering was written, while the
executed order grew ~15 passes it did not list, and the inline markers reused IDs with different
meanings: `W4b` named both the cascade apply and source-file module naming; `naming.rs` labelled
its tail `W12`–`W15`, which in `stages.rs` are strip-this / inlining / async / unwrap; `W16e` was
both the third JSX pass and import hoisting; `ir_gen.rs` labelled both the loop folds and closure
resolution F26. The ordering contract `../04_transforms/RISKS.md` asks every new pass to respect
was stated in a file that no longer described the order.

**How it was fixed, without renumbering.** Every ID `stages.rs` already defined keeps its
meaning. The collisions and unlisted passes took sub-labels where they run: cascade apply
**W4a**, ground-truth module naming **W4b**/**W4c**, the naming tail **W11a**–**W11e**,
`hoist_repeated_closures` **W16g**, import hoisting **W16h**, loop/iterator recovery **F5a**,
loop folds **F25b**. The pre-existing sub-labels (`W7b`, `W7c`, `W16a2`–`W16f`) are now listed.


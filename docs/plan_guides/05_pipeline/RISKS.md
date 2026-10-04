# Pipeline — risk register

> **Ownership.** *Owns* the risk that the orchestration/cache layer returns a stale or wrong
> result that looks valid. Two findings, **F8** (the cache `options_key` was hand-synced to
> two fields) and **F13** (cache temp-file race, and the 134 MB unauthenticated cache),
> split here from the read-path hardening review because both live in `pipeline/cache.rs`;
> plus two unnumbered open items found later (the `cascade` option vs. the cache, and
> `stages.rs` drift).
> *Delegates* the pipeline's *description* — the F/W stage spine, `PipelineContext`, the cache
> key design — to `../../arch_guides/05_PIPELINE.md`, and the upstream framing of the F-series
> to `../01_read/RISKS.md`. Finding numbers are shared across the stage registers and indexed
> in `../README.md`; F8 and F13 keep theirs.

Status: ✅ fixed (F8; F13's race). F13's size/trust notes are documentation items, recorded
not changed. Two **open** items found since, unnumbered (the F-series belongs to the read
pass): the `cascade` option vs. the cache, and stage labels drifting from `stages.rs` — both
at the end. Evidence tag **[measured]** means reproduced against the shipped Equinox v96
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


## Open — the `cascade` option vs. the cache

> **Open; mitigated in the CLI only.** Found re-auditing after `DecompileOptionsV2` gained
> `cascade: Option<PathBuf>`.

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

The CLI sidesteps both by forcing `no_cache` whenever `--cascade` is given
(`hbc-decomp-cli/src/main.rs:246`). A library caller of `PipelineContext::build_cached` or
`decompile_*_cached` with `cascade: Some(..)` has no such guard. The code comments in
`pipeline/mod.rs` (on the `cascade` field) and `cache.rs:from_snapshot` say the cache "does
not key on" the artifact; it keys on the path, which is the worse half-truth.

**Fix, either:** (a) refuse — `build_cached` bypasses load *and* save when `cascade` is set, so
the guard lives in the library rather than one frontend; or (b) key on the artifact bytes
(hash the file into the header) *and* add `cascade_names` to the snapshot, bumping
`CACHE_VERSION`. (a) is a few lines and matches "the cache is an optimization".

## Open — stage labels drifting from `stages.rs`

> **Open.** `stages.rs` exists to pin the order; it is now the part that drifted.

`pipeline/stages.rs` is unchanged since the W/F numbering was written, while the executable
order (`context/{mod,naming,transforms_phase}.rs`, `ir_gen.rs`) grew ~15 passes it does not
list, and the inline `// STAGE` comments reuse IDs with different meanings: `W4b` names both
the cascade apply (`context/mod.rs`) and source-file module naming (`naming.rs`); `naming.rs`
labels its tail `W12`–`W15` (dependencyMap rewrite, ancestor inherit, deep loop, stable
names), which in `stages.rs` and `transforms_phase.rs` are strip-this / inlining / async /
unwrap; `W16e` is both the third JSX pass (`transforms_phase.rs`) and import hoisting
(`context/mod.rs`); `ir_gen.rs` labels both the final loop folds and closure resolution F26.
The ordering contract that `../04_transforms/RISKS.md` asks every new pass to respect is
therefore stated in a file that no longer describes the order, and a grep for a stage ID can
land on the wrong pass.

**Fix.** Bring `stages.rs` up to the executed order without renumbering the existing W/F IDs:
give the unlisted passes sub-IDs under the stage they follow (`W7b`, `W16a2`, … as the code
already does) and rename the colliding inline labels (`naming.rs`'s tail, the second `W16e`,
`W4b`, the loop-fold "F26") to unique ones. `../../arch_guides/05_PIPELINE.md` § The stage
spine describes the executed order meanwhile and qualifies colliding labels by file.


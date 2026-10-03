# Pipeline — risk register

> **Ownership.** *Owns* the risk that the orchestration/cache layer returns a stale or wrong
> result that looks valid. Two findings, **F8** (the cache `options_key` was hand-synced to
> two fields) and **F13** (cache temp-file race, and the 134 MB unauthenticated cache),
> split here from the read-path hardening review because both live in `pipeline/cache.rs`.
> *Delegates* the pipeline's *description* — the F/W stage spine, `PipelineContext`, the cache
> key design — to `../../arch_guides/05_PIPELINE.md`, and the upstream framing of the F-series
> to `../01_read/RISKS.md`. Finding numbers are shared across the stage registers and indexed
> in `../README.md`; F8 and F13 keep theirs.

Status: ✅ fixed (F8; F13's race). F13's size/trust notes are documentation items, recorded
not changed. Evidence tag **[measured]** means reproduced against the shipped Equinox v96
bundle (see `../01_read/RISKS.md` for its identity).

---

## F8 — the cache key is hand-synced

> **Fixed.** `options_key` now hashes the whole `DecompileOptionsV2` (which gained
> `Hash`), so a new field cannot desync the key from what `build_with_options` reads.
> Held by `every_option_field_changes_the_cache_key`, which flips each field in turn
> and asserts the key moves — and destructures the struct exhaustively, so adding a
> field without adding it to the test stops compiling.


`pipeline/cache.rs:66`:

```rust
fn options_key(options: &DecompileOptionsV2) -> u32 {
    (options.assembly_mode as u32) | ((options.include_offsets as u32) << 1)
}
```

This is correct **today**: `build_with_options` (`pipeline/context/mod.rs:60-61`) reads exactly
those two fields and forces the rest to `optimized()`. Nothing enforces it. Add a seventh field
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


- **Temp-file race.** `cache.rs:306` — `path.with_extension("hdcache.tmp")` is a fixed name.
  Two processes analysing the same bundle write the same temp file concurrently; the rename is
  atomic but the *content* is interleaved. It degrades to a cache miss (`try_load`'s
  `rmp_serde…ok()?`), never to a wrong answer, but it leaves a corrupt file in place until
  something rewrites it. A PID/random suffix fixes it.
- **Size.** **[measured]** the `.hdcache` for the 16,837,408-byte Equinox bundle is
  **134,208,814 bytes** — 8× the input, written silently next to it, with no eviction and no
  mention in the docs. Worth stating in `USAGE.md` at minimum.
- **Trust.** The cache is unauthenticated MessagePack whose header check requires only the
  file hash and the build fingerprint — both derivable by anyone who can write next to the
  input. It deserializes into plain data (no code), so the ceiling is falsified analysis
  output, not execution. Low risk for a local tool; worth one sentence in the doc rather than
  a fix.


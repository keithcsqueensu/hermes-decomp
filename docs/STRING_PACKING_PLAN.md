# Impl plan — string packing

Research and plan for the *"No string dedup/merge"* limitation: what `hermesc` actually does
when it lays out string storage, how much it buys, whether we can mimic it, and what it would
cost us in safety.

**[source]** = read out of the Hermes checkouts wired up for `tests/upstream_pin.rs`.
**[measured]** = computed against
`com.equinoxfitness.equinox_11.39.0/hermes_bundle/assets/index.android.bundle.backup`
(v96, 16,837,408 B, 98,917 strings). **[code]** = file:line in this tree at time of writing.

---

## What upstream actually does

`hermesc` has **two** packers, and picks between them with an `optimize` flag
(`ConsecutiveStringStorage.cpp`, `StringTableBuilder::packIntoStorage`) **[source]**:

```cpp
if (optimize) { …optimizingPackStrings(asciiStrings_); … }
else          { …fastPackStrings(asciiStrings_);       … }
```

`fastPackStrings` is nine lines and does nothing at all:

```cpp
for (StringEntry &str : strings) {
  str.offsetInStorage_ = result.size();
  result.insert(result.end(), str.chars_.begin(), str.chars_.end());
}
```

**That is byte-for-byte what `strings.rs` does today.** So the limitation is not "we are missing
an optimisation Hermes always applies" — we implement upstream's non-optimising path exactly,
and the gap is only against `-O` builds. Shipped React Native bundles are `-O` builds, hence
the difference.

`optimizingPackStrings` is a four-stage pipeline **[source]**:

| stage | what it does |
|---|---|
| `buildPrefixTrigramSet` | set of every 3-char prefix, used to prune the suffix array to suffixes that could ever match some string's head |
| `buildSuffixArray` | generalized suffix array over all strings — incrementally hashed suffixes (`HashedSuffix` keeps the previous suffix's hash so building is not O(n²)), ordered by a three-way `radixQuicksort` |
| `computeOverlapsAndParents` | per entry, either **containment** (`parent_` + `offsetInParent_`: this string sits fully inside another at some offset) or a weighted list of **overlaps** (our tail == their head) |
| `planLayout` | greedy: walk overlaps heaviest-first, link `src → dst` when both ends are free and no cycle results. Upstream's own comment: *"equivalent to computing Hamiltonian path in the graph of strings while attempting to maximize the weight of its edges"* |

So it is a greedy shortest-common-superstring approximation with substring containment layered
on top. Two structural details matter for us:

- **ASCII and UTF-16 are packed into separate storages** and concatenated, with the UTF-16
  offsets shifted by `u16OffsetAdjust`. A packer must respect that split; a UTF-16 string can
  never share bytes with an ASCII one.
- Upstream validates its own work: `validateStringPacking` re-reads every entry out of the
  finished blob and compares — but only `#ifndef NDEBUG`. We should run that check *always*
  (see P0).

## What it buys, on a real bundle

The shipped bundle's `string_storage` section is **4,250,212 B — 25% of the whole file**. An
unpacked rebuild of the same table is **4,681,691 B [measured]**, so packing saves **431,479 B
(10.2% of storage, 2.56% of the file)**.

Decomposed, with the three parts summing to the known total — which is the cross-check that
the decomposition is right:

| scheme | storage | saving | note |
|---|---|---|---|
| unpacked (what we emit) | 4,681,691 B | — | == upstream `fastPackStrings` |
| + exact dedup | 4,681,685 B | **6 B** | strings are already uniqued at the *table* level upstream, so there is essentially nothing here |
| + suffix merge | 4,580,060 B | ~100 KB | cheap to implement |
| + containment (any offset) | — | ~210 KB | sampled every-30th, extrapolated; its suffix component reproduces the exact suffix number to within 0.3%, which validates the sampling |
| + overlap chaining | — | ~120 KB | the residue, i.e. upstream's greedy Hamiltonian path |
| **`hermesc` actual** | **4,250,212 B** | **431,479 B** | ground truth |

**Exact dedup is worth nothing** — a surprise worth recording, and it is because
`UniquingStringLiteralTable` already guarantees no two ids hold the same text. "Dedup" in the
limitation's name is a misnomer; the win is entirely *substring sharing*.

### It survives compression

The bundle ships inside an APK, so the number that reaches a device is post-deflate
**[measured]**:

| | raw | deflate -9 |
|---|---|---|
| packed (shipped) | 4,250,212 | 1,314,983 |
| unpacked (our rebuild) | 4,681,691 | 1,436,632 |
| **delta** | **+431,479 (+10.2%)** | **+121,649 (+9.3%)** |

Deflate recovers only ~72% of the difference. It cannot do better: its window is 32 KB, while
the sharing upstream exploits is spread across a 4 MB storage. So the win is real on device,
not just on disk — but it is ~122 KB on an APK, which is the honest scale of the prize.

## The catch: unpacked storage is load-bearing today

This is the part that makes packing a design change rather than an optimisation.

`patch_string_by_id` (`strings.rs:799`) patches a same-length string **in place**, but first
scans every other entry for a range intersection **[code]**:

```rust
let overlaps = abs_off < o_end && o_off < our_end;
if overlaps {
    // Storage is shared with another entry, so an in place overwrite
    // would corrupt it. Rebuild the string table unpacked instead, which
    // gives this entry its own storage.
    return patch_string_resize(file, id, new_value);
}
```

The unpacked rebuild is the **escape hatch**: it is what guarantees the entry you just patched
owns its bytes. A packer that re-shares that entry on the way out would defeat the fallback it
was called from — the next same-length patch would bounce to a full rebuild again, forever.

So "pack the rebuild" is not a drop-in change. The resolution is small, though:

> **Pin set.** Packing takes a set of ids that must receive *private, unshared* storage.
> The entry a patch just rewrote goes in it. Everything else packs normally.

That preserves today's safety property exactly, costs a handful of bytes per pinned entry, and
turns the overlap guard from "usually true after a rebuild" into "never true for pinned ids".

## Constraints a packer must respect **[source]**

- `SmallStringTableEntry` is `isUTF16:1, offset:23, length:8`. So **offsets must fit in 23 bits
  (8,388,608)** and lengths in 8, with `255` reserved as the overflow marker.
- The bundle's 1,449 overflow entries are all **length**-driven today, since the whole storage
  (4.25 MB) is under the 8 MB offset ceiling. This is worth watching: an unpacked rebuild pushes
  every later offset *up* by ~431 KB, i.e. toward that ceiling, while packing pushes them down.
  A bundle near 8 MB of storage could be tipped into needing new overflow entries by a rebuild —
  and per the guide's own limitation list, **`create` cannot emit overflow string entries**.
  Packing is therefore mildly *de*-risking, not just smaller.
- ASCII and UTF-16 storages are packed separately and concatenated; no cross-encoding sharing.
- Packing changes **offsets only**. String ids, `string_kinds` and `identifier_hashes` are
  index-parallel to the table and must come out untouched — that is the invariant to assert.

## Plan

### P0 — Always-on packing validation *(prerequisite, useful on its own)*

**Goal.** Before changing any layout, be able to prove a layout is correct.

**Do.** Port upstream's `validateStringPacking`: after any string-table write, re-read every
entry's `(offset, length, isUTF16)` out of the finished blob and assert it yields the original
text. Upstream runs this only in debug builds; run it unconditionally — it is one pass over the
table and it is the only thing standing between a packing bug and silently corrupt strings.

**Acceptance.** Deliberately corrupt one entry's offset in a test and watch it fire. Then run
it over the existing *unpacked* rebuild path and confirm it passes — that is the baseline.

### P1 — Suffix merge with a pin set

**Goal.** ~100 KB of the ~431 KB, with the cheapest possible algorithm and the safety property
intact.

**Do.** Sort unique strings by reversed text **descending** — so a string always follows a
string it is a suffix of — and absorb each entry into the previous one when
`prev.ends_with(s)`, except for pinned ids, which always get fresh bytes. Pack ASCII and UTF-16
separately.

> The sort direction is the whole algorithm and it is easy to get backwards. Sorting reversed
> *ascending* puts each suffix *before* its container, absorbs nothing, and yields a packer that
> silently does nothing — which is exactly what happened in the first draft of the measurement
> above. Assert a known saving on a fixture, or this bug ships as "packing enabled, 0 B saved".

**Acceptance.** On the production bundle, storage drops to ~4,580,060 B **[measured target]**;
P0's validator passes; every pinned id has a byte range intersecting no other entry; the
reparsed file's string table is value-identical to the input's.

### P2 — Substring containment

**Goal.** The largest single component, ~210 KB.

**Do.** Build a generalized suffix automaton (or suffix array) over the kept strings and, for
each candidate, find any occurrence inside another string; record `parent` + `offset_in_parent`
and emit nothing for it. Upstream prunes with a prefix-trigram set before building — do the
same or the build dominates runtime.

**Ordering constraint.** A container must be laid out before anything it contains, and a
contained string must never itself be absorbed into a string that was absorbed — upstream
handles this by making layout recursive (`layoutIfNeeded` lays out a parent, then prev/next).
Mirror that rather than inventing an ordering.

**Acceptance.** ~4,370,000 B on the production bundle; P0 validator passes; runtime for the
whole table stays inside a second or two — if it does not, the trigram prune is missing.

### P3 — Overlap chaining *(optional; measure before building)*

**Goal.** The final ~120 KB, and the only part that needs the greedy Hamiltonian-path machinery
with cycle avoidance.

This is where most of upstream's 838 lines live and where all the subtlety is
(`potentialCycles_`, weighted edge ordering, chain merging). It buys the smallest share of the
win. **Do not build it until P1+P2 have shipped and the remaining gap has been re-measured on a
current bundle** — the numbers above say it is ~28% of the prize for ~70% of the complexity.

### P4 — Expose the choice

**Do.** A `--pack-strings <none|suffix|full>` flag defaulting to whatever P1/P2 reached, with
`none` preserving today's behaviour exactly. Keep `none` working and tested: it is the fallback
when a packing bug is suspected, and it is the mode whose safety properties are already proven.

## Recommendation

Worth doing, with the scale stated plainly: **431 KB on disk, ~122 KB in the APK**, on a
16.8 MB bundle. That is a modest prize, so the case rests on the two secondary reasons rather
than the primary one:

1. **Headroom.** Our rebuild moves storage *toward* the 23-bit offset ceiling; packing moves it
   away. Combined with `create`'s inability to emit overflow entries, that is a real if distant
   correctness cliff.
2. **Fidelity.** Patched output that is structurally closer to `hermesc` output is easier to
   diff against a fresh build, which is how most drift in this project gets caught.

Ship **P0 + P1** (validator + suffix merge with the pin set) as one piece of work — that is
~100 KB, a day or two, and it establishes the pin-set contract that everything after depends
on. Decide P2 on the re-measured number, and treat P3 as unlikely to be worth it.

Do **not** ship any of it without P0. A string packer that is 99.9% correct produces a bundle
that loads, runs, and shows one wrong word somewhere — the worst failure mode in this codebase's
catalogue, and the exact shape of the v99 and v97 opcode drifts.

## Non-goals

- **Matching `hermesc` byte-for-byte.** The greedy heuristic's output depends on tie-breaking and
  iteration order; equal *size* is the goal, not equal bytes.
- **Repacking on same-length in-place patches.** Those do not rebuild storage at all and must
  stay that way — repacking would turn an O(1) write into a full rebuild.
- **Touching string ids, kinds, or identifier hashes.** Packing is an offset-only transform.
- **Delta/incremental packing** (upstream's "delta optimizing mode", where only new strings are
  packed and appended). Only relevant to `add-string`, and only worth it if `add-string` ever
  becomes hot.

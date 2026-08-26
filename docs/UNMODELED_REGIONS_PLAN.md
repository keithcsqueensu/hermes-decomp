# Impl plan — the regions we carry but do not model

Every part of an `.hbc` file this crate reproduces **only by copying it through**, and what it
would take to model each one. Formerly `DEBUG_INFO_AND_REGEXP_PLAN.md`; renamed because those
two were never the whole list, and the rest of the list is what blocks a total serializer.

The gap has two independent halves, and conflating them is why the old title undersold it:

- **Read.** A region we parse but do not *interpret* is a feature we cannot offer (real
  local-variable names, regex sources, which of two meanings a table has) and a drift we cannot
  detect. Most sections are past this line already; four are not.
- **Write.** A region we cannot *emit* is a region that survives only because the raw image is
  spliced rather than rebuilt. That is the whole of `RELOCATION_PLAN.md` P3's blocker, and it
  is true of every section listed below including the ones we read perfectly well.

Written so an impl agent can execute without re-deriving the formats. Everything marked
**[source]** was read out of the Hermes checkouts wired up for `tests/upstream_pin.rs`
(`HERMES_SRC_V96`/`_V97`/`_V98`/`_V99`); everything marked **[code]** is a file:line in this
tree at the time of writing — re-check both, and prefer re-deriving to trusting the tables
below, which is the whole lesson of R8/R19.

**[compiler]** marks a claim checked against the *serializer and generator* —
`lib/BCGen/HBC/BytecodeStream.cpp`, `BytecodeGenerator.cpp`, `DebugInfo.cpp`,
`lib/BCGen/LiteralBufferBuilder.cpp` — and not merely against `BytecodeFileFormat.h` and the
reader in `BytecodeDataProvider.cpp`. That distinction earned its own tag on the second pass:
this is a plan about *emitting* these regions, and the writer states things no reader can. It
is where the ordering invariants, the padding rule, and the two corrections below came from.
Where a claim is tagged both ways, reader and writer agree.

**[measured]** marks a claim confirmed by compiling a bundle that actually contains the region
and parsing it back with this crate — see What compiling actually showed. Static analysis got
the formats right; compiling corrected two claims that reading could not have, and one region
could not be produced at all.

Companion to `WRITE_PATH_GUIDE.md` § Pending impl plans, and to `RELOCATION_PLAN.md` (which
owns *moving* these regions) and `STRING_PACKING_PLAN.md` (which owns rebuilding one of them).
Same conventions: derive from upstream, pin what you derive, refuse rather than approximate.

---

## The inventory

Two axes, because they fail differently. "Interpreted" means the bytes are turned into
something with meaning attached, not merely into a `Vec`. "Emit" means `create` /
`serialize_file` can produce the region from the model rather than copying it.

| Section | Parsed | Interpreted | Emit | Gap |
|---|---|---|---|---|
| function headers, exception handlers | ✅ | ✅ | ✅ | resize of a handler-bearing function is refused (Q3/Q4) |
| string table, storage, kinds, identifier hashes | ✅ | ✅ | ✅ | packing — `STRING_PACKING_PLAN.md` |
| array / literal-value / object key + value buffers | ✅ | ✅ decoded to `LiteralValue` (`parser/buffer.rs`) | ❌ empty only | write side only |
| bigint table + storage | ✅ | ✅ resolved by id (`parser/helpers.rs:8`) | ❌ empty only | write side only |
| object shape table | ✅ `ShapeTableEntry` | ✅ shape lookup (`parser/mod.rs:40`) | ❌ empty only | write side only; **v98+ only** |
| function source table | ✅ pairs | ✅ dumped and resolved (`inspect.rs:248`) | ❌ empty only | write side only |
| **CJS module table** | ✅ pairs | ⚠️ **one of two meanings, and we cannot tell which** | ❌ empty only | OB2 |
| **`options` byte** | ✅ as a `u8` | ❌ **never decoded, anywhere** | carried | OB1 |
| **RegExp table + storage** | table ✅, storage ❌ raw | ❌ | ❌ empty only | P3 |
| **debug info** | partly, at one version | partly | ❌ empty only | DI1 / DI2 / DI3 |

The four bolded rows are the read-side gaps; everything else on the list is a write-side gap
only, and none of them can be *emitted* today. `create` writes a zero count for every one
(`serialize.rs:246-254`), which is honest for a minimal image and is exactly why `create` is a
smoke-test emitter rather than a serializer.

---

## What compiling actually showed **[measured]**

`hermesc` is built for v96/v98/v99 beside each source worktree (v97 is source-only, as ever), so
every claim below is a bundle that was compiled, then parsed by `BytecodeFile::parse_auto`. Four
inputs, compiled with and without `-g3`: an object/regex/arith file, a `'show source'` +
`'hide source'` file, an `async function` file, and a two-module CommonJS directory. This is the
first time this crate has been pointed at a file where these regions are non-empty.

**Confirmations.**

| Claim | Evidence |
|---|---|
| DI1 — `source_locations` is always empty | Every file, every version, including `-g3` v96 builds whose `debug_info_offset` is non-zero and *all* of whose functions carry `FLAG_HAS_DEBUG_INFO`: `source_locations` has **0** entries. The feature is unreachable in practice, not just on paper |
| CJS unresolved pair = (filename string id, function id) | `cjs1.v96`: `(3, 1)` where string 3 is `"helper.js"`. `cjsdir.v96`: `[(5,1) → "index.js", (2,2) → "helper.js"]` |
| `hasAsync` exists at v96 and is gone by v98 | The *same* async source: `options = 0b00000100` at v96 (bit 2 set), `0b00000000` at v98 **and** v99. R27's drift, observed rather than inferred |
| Object shape table is v98+ only | `shapes=0` at v96, `shapes=1` at v98/v99 for the same input |
| Shapes are deduped | `plain.js` has two distinct object literals with the same key set; the v98/v99 table holds **one** entry, `(keyBufferOffset 0, numProps 3)` — the `LiteralBufferBuilder` dedup, measured |
| RegExp table is populated per literal | Two regex literals → `regexp=2`, every version |

**Correction 1 — the function source table is not only about source-visibility directives.**
Upstream's comment says these entries exist "only ... when functions are declared with source
visibility directives", and the directives do behave as documented: `'show source'` yields an
entry pointing at a string holding the function's *actual text*
(`(1, 1, "function visible(a) {\n  'show source';\n  return a * 2;\n}")`), and `'hide source'`
yields an entry pointing at **string id 0, the empty string** — a tombstone rather than an
omission. But `asyncy.js`, which contains one `async function` and no directive at all, also
produces an entry — `(3, 0, "")`, on the inner function the async lowering generates — at every
version. So "has a source-visibility directive" is sufficient, not necessary, and an emitter
cannot derive this table from directives alone.

**Correction 2 — DI3's degradation is real, and now visible.** With `-g3`, the same source
parses to genuinely different debug info depending on version:

| | v96 | v98 / v99 |
|---|---|---|
| `scope_descriptors` | **5** | 0 |
| debug `string_table` | **8** | 0 |

At v98/v99 the reader consumes a 28-byte header where the header is 16, and every interior
offset it then computes is wrong — so it returns an empty `DebugInfo` on a file that demonstrably
has debug info. Silent, exactly as predicted. (A v96 file built *without* `-g3` shows
`scope_descriptors=1`: the one always-present empty entry `DebugInfoGenerator`'s constructor
writes. A useful sanity check that the v96 path is really reading the structure and not
inventing it.)

**What could not be produced.** No `cjsModuleTableStatic` bundle. `-commonjs -fstatic-require`
over a directory, with `moduleIDs` supplied in `metadata.json` and `-Wunresolved-static-require`
silent, still emitted the **unresolved** table: `options` bit 1 clear, pairs resolving to
filenames. So the statically-resolved half of OB2 remains static-analysis-only, and P5's
acceptance test cannot currently be written against a real artifact — it will have to assert the
decoder on a synthesised byte pattern until someone finds the invocation that produces one.
Separately, `hermesc` at **v98 and v99 crashes** on `-commonjs` with a single-file input where
v96 succeeds, so the CJS path is only exercisable at v96 on these builds.

**Fixture recipe, now verified.** P0's missing debug-info fixture is
`hermesc -emit-binary -g3 -out <out>.hbc <in>.js`, and `-g3` does populate the section at all
three versions (`debug_info_offset` non-zero, every function flagged). That is the flag P0 asks
someone to confirm; it is confirmed.

---

## Debug info, precisely

The one-line limitation in the guide — *"Debug info & RegExp are opaque `u8` buffers, not
parsed into typed structs"* — is half stale and half understated:

| | today | file |
|---|---|---|
| `DebugInfoHeader` | **parsed as 7 × `u32`** — the v96 shape, unconditionally (see DI3) | `debug.rs:148` |
| filename table / file regions | skipped (sized from the header, contents unread) | `debug.rs:101` |
| scope descriptors | **parsed** into `ScopeDescriptor` — v96-only by construction | `debug.rs:132` |
| textified callees | **parsed** — v96-only by construction | `debug.rs:141` |
| debug string table | **parsed** — v96-only by construction | `debug.rs:124` |
| **source-location streams** | **not parsed at all**, at any version | — |
| **per-function `DebugOffsets`** | **never read**, at any version | — |
| RegExp table + storage | kept raw; `dump --kind regexp` prints offset/length/bytes | `inspect.rs:95` |

So debug info is not opaque — it is *partly* parsed, at *one* version, with the region that
carries line numbers and the index into it both missing. Three consequences follow; two are
live defects.

### DI1 — debug-driven variable naming is dead code

`DebugInfo::source_locations` is declared (`debug.rs:29`) and read by two call sites —
`pipeline/ir_gen.rs:289` and `pipeline/mod.rs:127` — both of which do the same thing:

```rust
let scope_offset = debug_info.source_locations
    .get(&function_id)
    .and_then(|locs| locs.iter().find_map(|l| l.scope_offset));
debug_info.build_variable_map(scope_offset)
```

`parse()` never assigns `source_locations`. It is therefore always empty, `scope_offset` is
always `None`, and `build_variable_map(None)` returns an empty map unconditionally
(`debug.rs:257-275`). The scope descriptors are parsed, carry real variable names, and are
unreachable through this path. Recovering real local-variable names from a debug-built bundle
is a feature that looks implemented and has never once produced a name.

### DI2 — a size-changing edit silently invalidates debug info *(proposed R24)*

This is the one that matters. A location stream stores **bytecode addresses within a
function**, accumulated as deltas (`current_.address += addressDelta` **[source]**). On a
resize, `patch/functions.rs:231-243` shifts `debug_info_offset` — the *section's* position —
and stops there:

```rust
// The debug info section sits after the code, so its header offset shifts too.
if file.header.debug_info_offset != 0 { … shifted = debug_info_offset + delta … }
```

Nothing rewrites the addresses inside the stream. After `patch-function`, `asm` or
`inject-stub` changes a body's length, every location past the edit point in that function
maps to the wrong instruction, and every location in *other* functions is still fine (their
streams are keyed on their own function-relative addresses). There is no error and no warning.

Compare the exception-handler case, which is the same defect and was handled properly:
`functions.rs:35` refuses to resize a function that declares a handler table, pending Q3's
full relocation (R9). **Nothing anywhere in `write/` references `has_debug_info`** — grep it.
So the handler hazard is guarded and the debug hazard is not, purely because one was noticed.

That asymmetry is the first thing this plan fixes, and it is cheap.

---

## Derived formats

### Section layout **[source]** — the header is version-keyed, *not* stable

`DebugInfoHeader` loses fields at v97 and again at v98. Its size is therefore
**28 / 20 / 16 bytes**, and since the debug data begins immediately after the filename table
and file regions, getting the size wrong misplaces everything downstream:

| version | `DebugInfoHeader` fields | size |
|---|---|---|
| v96 | `filenameCount`, `filenameStorageSize`, `fileRegionCount`, `scopeDescDataOffset`, `textifiedCalleeOffset`, `stringTableOffset`, `debugDataSize` | **28 B** |
| v97 | `filenameCount`, `filenameStorageSize`, `fileRegionCount`, `lexicalDataOffset`, `debugDataSize` | **20 B** |
| v98, v99 | `filenameCount`, `filenameStorageSize`, `fileRegionCount`, `debugDataSize` | **16 B** |

At v96 the debug data is four sub-regions delimited by the three interior offsets; at v97 it
is source locations plus one lexical-data region; at v98+ the header delimits nothing beyond
the total size, so the data is source-location streams and nothing else. The whole
scope-descriptor / textified-callee / debug-string-table apparatus is a **v96-era feature that
upstream removed**, which is why the crate's parsing of it is v96-only whether or not that was
intended.

```
v96:
debug_info_offset →
  [DebugInfoHeader                    28 B = 7 × u32]
  [filename table   filenameCount × 8 B  +  filenameStorageSize B]
  [file regions     fileRegionCount × 12 B]      DebugFileRegion = 3 × u32
  [debug data       debugDataSize B]
       [0                     .. scopeDescDataOffset)    source-location streams
       [scopeDescDataOffset   .. textifiedCalleeOffset)  scope descriptors
       [textifiedCalleeOffset .. stringTableOffset)      textified callees
       [stringTableOffset     .. debugDataSize)          debug string table

v98 / v99:
  [DebugInfoHeader                    16 B = 4 × u32]
  [filename table] [file regions]
  [debug data       debugDataSize B]  = source-location streams only
```

The interior offsets are relative to the **start of the debug data**, not to the section.
`debug.rs:100-107` computes `data_start` that way and gets it right — for v96.

### DI3 — the header parser is version-blind

`DebugInfo::parse` takes `(bytes, debug_info_offset)` and **no version** (`debug.rs:88`), and
`parse_header` reads seven `u32`s unconditionally (`debug.rs:148-158`). On a v98 or v99
debug-built file it consumes 28 bytes where the header is 16: `scope_desc_offset` picks up the
real `debugDataSize`, the next three fields are filename-table bytes reinterpreted as offsets,
and `data_start` is computed from `28 +` when it should be `16 +`.

The bounds checks (`slice_in_bounds`, `slice_range`) then usually make this degrade to
`Ok(DebugInfo::default())` — silently "no debug info" — rather than crash, which is why it has
never been noticed. It has also never been exercised: **every committed fixture is built
without debug info**, so no test reaches this path at any version.

Confirmed on real bundles **[measured]**: one source compiled `-g3` at three versions parses to
5 scope descriptors and an 8-entry debug string table at v96, and to **zeros** at v98 and v99.
See What compiling actually showed.

This is the R8 pattern exactly — a structure hand-transcribed from one upstream vintage, used
for all of them, with nothing re-deriving it. It should get a register row of its own (R25),
separate from R24, because one is a read defect and the other a write defect.

### Per-function entry point: `DebugOffsets` **[source]**

Lives in the function's info area after the exception-handler table, present iff
`flags & FLAG_HAS_DEBUG_INFO` (`0x10`, `format.rs:35`). Its size is **version-dependent**:

| version | fields | size |
|---|---|---|
| v96 | `sourceLocations`, `scopeDescData`, `textifiedCallees` | 12 B |
| v97 | `sourceLocations`, `lexicalData` | 8 B |
| v98, v99 | `sourceLocations` | 4 B |

`sourceLocations` is a byte offset into the debug **data** region, and is `NO_OFFSET`
(`u32::MAX`) when absent. This is the missing index: it is what turns a `function_id` into a
stream position, and it is why DI1 cannot be fixed by parsing the stream region alone.

`modern_layout.rs:154` already documents how to walk past the large header to the info area
for modern layouts (upstream's `getExceptionTableAndDebugOffsets`); reuse that, do not
re-derive it.

### Location stream — **three incompatible encodings** **[source]**

Every stream begins with three SLEB128s: `functionIndex`, absolute `line`, absolute `column`.
Then entries repeat until `addressDelta == -1`. The entry body is where the versions diverge:

| | v96 | v97 | v98 / v99 |
|---|---|---|---|
| `lineDelta` bit 0 | statement present | **location present** | **location present** |
| `lineDelta` bit 1 | — | statement present | statement present |
| `lineDelta` bit 2 | — | — | envIdx present |
| shift | `>>= 1` | `>>= 2` | `>>= 3` |
| always-present fields | `colΔ`, `scopeAddress`, `envReg` | `colΔ` | `colΔ` |
| conditional fields | `[stmtΔ]` | `[stmtΔ]` | `[stmtΔ]`, `[envIdxΔ]` |
| bit 0 clear means | n/a | skip this entry, **stream continues** | same |

Three details that will bite anyone who skims this:

1. **`scopeAddress` exists only at v96.** It is the field DI1's consumers want. At v97+ the
   per-location scope link is gone from this stream — v97 moved it to `lexicalData`, v98+
   dropped that too. So DI1's fix is v96-shaped, which is fine: the Equinox bundles are v96.
   Do not write code that expects a scope offset at v98.
2. **At v97+, `addressDelta` is applied before the bit-0 early return** — an entry with no
   location still advances the address. Returning early without accumulating desynchronises
   the whole rest of the stream.
3. **`-1` terminates, and `-1` is a legal SLEB128 value, not a sentinel byte.** Decode, then
   compare; do not scan for `0x7f`.
4. **At v97+ there are *two* "previous" cursors, not one** **[compiler]**. The writer advances
   `previousAddress` on every entry, but advances `previous` — the base for the line, column,
   statement and envIdx deltas — **only on entries that carry a location**
   (`DebugInfo.cpp:246-280` at v99: `previousAddress = next.address;` sits before the branch,
   `previous = &next;` sits inside it). A decoder that keeps a single cursor and updates it on
   a no-location entry produces line numbers that are silently wrong from the first such entry
   onward, and no length check catches it because the stream still terminates correctly. The
   no-location entry itself is a literal `0` ldelta — bit 0 clear — written after the address
   delta. At v96 the question does not arise: there is no no-location form, and `previous`
   advances every time.

Since `readSignedLEB128` is signed LEB128 and deltas are genuinely negative in real streams,
an unsigned reader will appear to work on small files and corrupt large ones.

### RegExp **[source]**

Simpler in every way, and notably **not** version-keyed:

- `RegExpTableEntry { u32 offset; u32 length }` — 8 B. `offset` is relative to the start of
  regexp storage, which is why the current opacity is *safe*: shifting the whole section
  cannot invalidate it, unlike debug info's intra-function addresses.
- Each entry's storage begins with `RegexBytecodeHeader` — `u16 markedCount`,
  `u16 loopCount`, `u8 syntaxFlags`, `u8 constraints` (`MatchConstraintSet = uint8_t`) = **6 B
  packed** — followed by an instruction stream.
- The instruction set is `include/hermes/Regex/RegexOpcodes.def`, 29 opcodes. That file is
  **byte-identical v96 → v99** (verified by `diff`), so one decoder covers every supported
  version and no `ModernLayout`-style keying is needed.

### OB1 — the `options` byte is never decoded **[source]**

`BytecodeHeader::options` is a bare `u8` (`format.rs:80`) and **nothing in the crate reads it** —
grep `header.options` and every hit is some unrelated `Options` struct. Upstream it is a
bitfield, and it lost a bit inside the range we support:

| version | bits |
|---|---|
| v96 | `staticBuiltins`, `cjsModulesStaticallyResolved`, `hasAsync` |
| v98, v99 | `StaticBuiltins`, `CjsModulesStaticallyResolved` — `hasAsync` **removed** |

That is R8's shape once more: a version-keyed structure carried as an integer. It is harmless
only for as long as nothing reads it, because bit 2 means `hasAsync` on one supported version
and nothing on another. And something *should* read it today — bit 1 decides what the next
table means.

### OB2 — the CJS module table has two meanings **[source]** **[compiler]**

`Array<std::pair<uint32_t, uint32_t>>`, `cjsModuleCount` entries, 4-aligned like every section.
The reader chooses between two tables on `options.cjsModulesStaticallyResolved`
(`BytecodeDataProvider.cpp:300`), and the generator confirms what each pair holds — note that
the *argument* order of `addCJSModule` is the reverse of the *stored* order, which is exactly
the kind of thing reading only the reader would miss (`BytecodeGenerator.cpp:354-368`):

| bit | table | stored pair | built by |
|---|---|---|---|
| clear | `cjsModuleTable` | `{nameID, functionID}` — **filename string ID → function ID** | `addCJSModule(functionID, nameID)` |
| set | `cjsModuleTableStatic` | `{moduleID, functionID}` — **module index → function ID** | `addCJSModuleStatic(moduleID, functionID)` |

So `.second` is the function ID in **both** forms. That narrows the defect considerably from
where the first pass left it: `inspect.rs:89` labels the pair `(symbol_id, function_id)`, and
the second half is right either way. What is wrong is the first half on a statically-resolved
bundle, where the value is a module index and the label invites resolving it as a string id —
which would print an unrelated string, not an obvious error. The crate cannot currently tell
the two apart, because the deciding bit lives in the byte OB1 never decodes. Fixing OB1 fixes
this; there is no separate format work.

**The invariant an emitter must uphold** **[compiler]**. The serializer writes *both* arrays,
back to back, into the one section (`BytecodeStream.cpp:129-138`):

```cpp
for (const auto &it : BM.getCJSModuleTable()) { writeBinary(it.first); writeBinary(it.second); }
writeBinaryArray(BM.getCJSModuleTableStatic());
```

while the header counts only one of them — `cjsModulesStaticallyResolved ? static.size() :
table.size()` (`BytecodeStream.cpp:16-20`). The format is therefore only well-formed because
**exactly one of the two is ever non-empty**, which the generator asserts on both entry points
(`assert(cjsModulesStatic_.empty())` in `addCJSModule`, and the converse in
`addCJSModuleStatic`). Nothing in the *file* records which one you are looking at except the
options bit, and nothing in the file would reveal a violation: both tables non-empty would
produce a section longer than its count, and every later section would still parse, having
started at the wrong offset. P6's emitter has to preserve this; a reader could cheaply
sanity-check it once OB1 lands.

### Function source table **[source]**

`Array<std::pair<uint32_t, uint32_t>>`, `functionSourceCount` entries, present from **v84**
(`header.rs:9`, `LEGACY_FUNCTION_SOURCE_MIN_VERSION`). Upstream:

> Mapping function ids to the string table offsets that store their non-default source code
> representation that would be used by `toString`. These are only available when functions are
> declared with source visibility directives such as 'show source', 'hide source', etc.

So: function ID → string ID, and normally empty. Parsed and resolved already (`inspect.rs:248`,
`dump --kind function-sources`); the only gap is emission, and the only thing that could
invalidate it is inserting or removing a function — a `RELOCATION_PLAN.md` P3 concern, not a
size-delta one, because it stores **indices, not offsets**.

⚠️ That upstream comment is not the whole rule **[measured]**: an `async function` with no
directive at all also gets an entry, pointing at the empty string. `'hide source'` gets one too,
also pointing at string 0. See What compiling actually showed — an emitter cannot reconstruct
this table from directives.

### Object shape table **[source]**

`ShapeTableEntry { uint32_t keyBufferOffset; uint32_t numProps; }`
(`include/hermes/BCGen/ShapeTableEntry.h`), `objShapeTableCount` entries. **Modern only** — the
field does not exist in the v96 header, and our parser correctly makes it `Option`
(`header.rs:129`). A shape names the key sequence of an object literal: where its keys begin in
the literal key buffer, and how many there are.

Read-side this is done and used (`parser/mod.rs:40` resolves a shape id). It is listed here for
one reason: **it is the region that makes "just copy the buffers through" stop working.**
`keyBufferOffset` is an offset into the object key buffer, so any future op that rewrites or
repacks that buffer must rewrite the shape table with it — the same coupling the string table
has with its storage, and the reason a serializer cannot treat the two as independent blobs.

Upstream does exactly that rewrite, which is the confirmation rather than an analogy
**[compiler]**: merging a module rebases every entry with
`entry.keyBufferOffset += objKeyBufferOffset;` and `entry.shapeTableIdx += objShapeTableOffset;`
(`BytecodeGenerator.cpp:257-269`). The builder also dedupes shapes on
`<keyBufferOffset, numProps, allocKind>` (`LiteralBufferBuilder.cpp:205`) — note `allocKind` is
a *builder-side* discriminator with no on-disk field, so two shapes that differ only in it
collapse to one entry when written. An emitter that re-derives the table from decoded literals
must reproduce that dedup or it will emit more entries than upstream would, changing every
downstream shape id.

---

## Plan

Phases are ordered by value per unit of risk. **P0 is worth shipping on its own** and should
not wait for the rest.

### P0 — Guard the resize (fixes DI2)

**Goal.** Stop silently emitting wrong debug info. Mirror the exception-handler guard exactly;
this is a five-line change with a large correctness payoff.

**Do.** In the size-changing path (`write/patch/functions.rs`, beside the handler check at
`:35`), refuse when the target function has `FLAG_HAS_DEBUG_INFO` and `delta != 0`. Error text
should name the reason and point here, matching the handler guard's wording.

**Escape hatch.** Add an explicit opt-out — `--allow-stale-debug-info` on the CLI ops, or a
field on the patch options — because for most patching work the line table is worthless and
refusing outright would regress a working flow. The default must be refuse; the opt-out must
say in one line what it is discarding.

**Acceptance.** A size-changing `patch-function` on a debug-built fixture is refused with a
message naming debug info; the same edit with the opt-out succeeds; a same-length edit is
unaffected in both modes; a function with no debug info is unaffected.

**Tests.** Needs a debug-info fixture, which does not exist today — every fixture is built
with plain `-emit-binary`. Add one: `hermesc -g -emit-binary` (confirm the flag that emits
debug info at each version) and wire it into `scripts/build_hermes_vm.ps1 -Fixtures`. Assert
`has_debug_info()` is actually true on it, or the test proves nothing.

### P1 — Read the location streams (fixes DI1)

**Goal.** Populate `DebugInfo::source_locations`, so the two existing consumers start working.

**Do.**
0. Fix DI3 first: thread the bytecode version into `DebugInfo::parse` and key the header shape
   (28 / 20 / 16 B) off it. Nothing else in this phase is trustworthy until the header is read
   at the right size, and the fix is a precondition for even *locating* the streams at v98+.
1. Read per-function `DebugOffsets` (version-keyed size table above) during function parsing;
   store `source_locations_offset: Option<u32>` on the function.
2. Add a version-keyed stream decoder — one function per encoding, selected the way
   `ModernLayout::for_version` selects, **including its refusal habit**: an unknown version is
   an error, not a best guess.
3. Populate `source_locations` keyed by `function_id`, with `scope_offset` filled from
   `scopeAddress` at v96 and left `None` at v97+.

**Acceptance.** On a v96 debug fixture, `decompile` emits real local-variable names from the
debug scope chain where today it emits synthesised ones. That is the observable end-to-end
signal, and it is currently unreachable, so it doubles as proof P1 landed.

**Pin it.** Add both version-keyed quantities to `tests/upstream_pin.rs` the way opcodes and
function-header fields already are: derive `sizeof(DebugInfoHeader)` and `sizeof(DebugOffsets)`
by counting `uint32_t` members in each checkout, and parse the shift width and flag-bit
meanings out of each checkout's `FunctionDebugInfoDeserializer`, asserting all of it matches
what the decoder implements. The v96 →
v97 → v98 drift documented above is exactly the shape R19 exists to catch, and it will happen
again.

### P2 — Relocate addresses on resize (removes P0's guard)

**Goal.** Rewrite a function's location stream so a size-changing edit keeps debug info
correct, making P0's refusal unnecessary.

**Do.** Given the edit's offset and delta, re-emit that function's stream with every
`address >= edit_offset` shifted by `delta`. Because addresses are delta-encoded in SLEB128,
the re-emitted stream can change length, so the debug data region must be rebuilt and every
`DebugOffsets.sourceLocations` past the rewrite point re-pointed — the same
rebuild-and-repoint shape as the string-table rebuild in `strings.rs`, not an in-place patch.

**Do not** attempt this before P1: relocation without a reader is unfalsifiable.

**Acceptance.** Round-trip on a real fixture: parse → resize a function → re-parse → every
location in the edited function maps to the same *instruction* it did before (not the same
address), and locations in other functions are byte-identical. Then drop the P0 guard.

### P3 — Disassemble RegExp

**Goal.** Turn `dump --kind regexp` from a byte dump into a listing, and give
`CreateRegExp`'s operands something to resolve to.

**Do.** Decode `RegexBytecodeHeader` + the 29-opcode stream from `RegexOpcodes.def`. One
decoder, no version keying. Emit alongside the existing raw bytes rather than replacing them.

**Acceptance.** Every regex in a production bundle decodes to a complete instruction stream
that consumes exactly `length` bytes with nothing left over — a total-consumption check across
the whole corpus is the oracle here, and `tests/corpus.rs` already has the harness shape for
it. Cross-check a handful against the source pattern text recovered from the string table.

### P4 — Write-side RegExp *(only if a need appears)*

Nothing needs this today, and the section-relative offsets mean nothing breaks without it.
Listed only so the boundary is explicit. Do not build it speculatively.

### P5 — Decode the options bitfield (fixes OB1, and OB2 with it)

**Goal.** Stop carrying a version-keyed structure as an integer, and let the CJS table be
labelled correctly.

**Do.** Add a `BytecodeOptions` newtype over the byte with version-keyed accessors —
`static_builtins()`, `cjs_modules_statically_resolved()`, and `has_async()` returning
`Option<bool>`, `None` above v96 because the bit does not exist there rather than because it is
clear. Keep the raw byte on the header: the write path round-trips it verbatim and must go on
doing so. Then key `dump --kind cjs`'s labels on bit 1, and say which form it is showing.

**Pin it.** This is a two-line addition to `tests/upstream_pin.rs` and it is the point of the
phase: parse the bitfield members out of `BytecodeFileFormat.h` in each checkout and assert the
bit order and the *set* of bits match. The v96 → v98 loss of `hasAsync` is precisely the drift
that pin exists to catch, and it already happened once unnoticed.

**Acceptance.** An unresolved bundle dumps as filename string IDs, labelled as such —
`cjsdir.v96` is a real artifact for this, and its first fields resolve to `index.js` and
`helper.js` **[measured]**. `upstream_pin` fails if a checkout adds, removes or reorders a bit;
the `hasAsync`-at-v96-only case is a real bundle too. The statically-resolved arm has **no
artifact**: it could not be produced with these compilers (see What compiling actually showed),
so assert that decoder against a synthesised byte pattern and say so in the test name rather
than pretending a fixture exists.

**Cost.** Hours, not days. It is the cheapest item in this document and the only one that fixes
a wrong output rather than a missing one.

### P6 — Emission: what a total serializer owes each region

**Goal.** The write-side half of the inventory, and the thing `RELOCATION_PLAN.md` P3 is
blocked on. Not a phase to start speculatively — it exists so that when an op finally demands a
rebuild, the per-region contract is already written down.

Per region, what "emit" actually means:

Two facts the writer supplies that the format headers do not, both load-bearing for an
emitter **[compiler]**:

- **Every section is padded to `BYTECODE_ALIGNMENT` before it is written**, by the serializer
  itself (`pad(BYTECODE_ALIGNMENT)` opens each `visit*`, `BytecodeStream.cpp:255-341`). Our
  parser's align-*after* in `track_section` (`parsing.rs:51`) is the same rule seen from the
  other side, which is why section walking has never desynchronised. Function info uses a
  different constant, `INFO_ALIGNMENT`.
- **`DebugOffsets` is written iff the function's `hasDebugInfo` flag is set**, and stripping
  debug info *clears the flag* rather than leaving it set with an empty region
  (`BytecodeStream.cpp:171-177`, and `:83`). So the flag is authoritative on the write side
  too, not merely a hint the reader may check.

| Region | To emit it you must | Coupled to |
|---|---|---|
| array / literal-value / object key + value buffers | re-encode `LiteralValue`s with the tag encoding `parser/buffer.rs` already decodes — the decoder is the spec, so this is a mirror, not a derivation | object shape table (key buffer offsets) |
| object shape table | recompute `keyBufferOffset` / `numProps` as the key buffer is laid out | the key buffer, tightly |
| bigint table + storage | offset/length pairs over a byte blob; the same shape as the string table minus packing | nothing |
| RegExp table + storage | copy storage verbatim and re-point offsets, or P3/P4 if the bytecode must change | nothing (storage-relative) |
| CJS + function source tables | pairs of **indices**; trivial to write, but both are invalidated by inserting or removing a function | function ids |
| debug info | P1's reader run backwards, plus P2's relocation | function offsets, `DebugOffsets` |

**The gate is the same one P3 of the relocation plan states**: byte-identical re-emit of a real
bundle, then `hbcdump` differential, then a VM run. A serializer that reparses is not a
serializer.

**Do not** build P6 region-by-region as a side project. Each region emitted without the others
produces a file that no test can hold to account, because the only meaningful assertion is over
the whole image.

---

## Non-goals

- **Emitting debug info for functions that lack it.** Out of scope at every phase.
- **Source-map generation.** Downstream of P1; a separate plan if wanted.
- **v97 support anywhere in this plan.** v97 is refused by `ModernLayout` on measured grounds
  (see the guide's modern-layout bullet) and never shipped. Its stream encoding is documented
  above so the *shape of the drift* is on record, not because it needs implementing.
- **Making debug info survive a string-table rebuild.** Separate concern from DI2; the debug
  string table is its own table and is not affected by `strings.rs`.
- **Interpreting the buffers, bigints, shape table or function source table any further.** They
  are already read correctly and used; their only gap is emission, which is P6's problem and
  nobody's until an op needs it.
- **Speculative emission.** Writing one region's emitter "while we are in here" produces
  untestable code — see P6.

## Ordering

```
P0  ──────────────► ship immediately, independent
P1  ──► P2         relocation needs a reader first
P3  ──► P4         disassemble before you assemble; P4 only on demand
P5  ──────────────► independent, hours, fixes a wrong output
P6                 blocked on P1+P2 for debug info, and on a demand that does not exist yet
```

P0 before P1 is deliberate: the guard removes a live correctness hole in a day, while P1 is
the larger piece of work. Shipping P1 first would leave DI2 open for the duration.

P5 is unordered with respect to everything else and is the smallest item here; it is separated
out only because it fixes an output that is *wrong* rather than one that is *missing*, which
makes it worth more than its size suggests.

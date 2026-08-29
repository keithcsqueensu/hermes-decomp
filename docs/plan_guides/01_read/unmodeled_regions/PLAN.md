# Impl plan — the regions we carry but do not model

Every part of an `.hbc` file this crate reproduces **only by copying it through**, and what it
would take to model each one. Formerly `DEBUG_INFO_AND_REGEXP_PLAN.md`; renamed because those
two were never the whole list, and the rest of the list is what blocks a total serializer.

The gap has two independent halves, and conflating them is why the old title undersold it:

- **Read.** A region we parse but do not *interpret* is a feature we cannot offer (real
  local-variable names, regex sources, which of two meanings a table has) and a drift we cannot
  detect. Most sections are past this line already; four are not.
- **Write.** A region we cannot *emit* is a region that survives only because the raw image is
  spliced rather than rebuilt. That is the whole of `../../06_write/relocation/PLAN.md` P3's blocker, and it
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

> **Ownership.** Split out of `../../06_write/relocation/PLAN.md` P3, whose structured rebuild is blocked on
> a total serializer this document inventories. *Owns* each region's read / interpret / emit
> status and its derived format. *Delegates* **moving** a region to `../../06_write/relocation/PLAN.md`,
> rebuilding the string region to `../../06_write/string_packing/PLAN.md`, the read-path *symptoms* of a
> silent debug-info failure to `../RISKS.md` F10, and — since P1b — the decompiler's
> closure/env-slot model to `../../03_analysis/closure_model/PLAN.md`.

Same conventions as its siblings: derive from upstream, pin what you derive, refuse rather than
approximate.

---

## The inventory

Two axes, because they fail differently. "Interpreted" means the bytes are turned into
something with meaning attached, not merely into a `Vec`. "Emit" means `create` /
`serialize_file` can produce the region from the model rather than copying it.

| Section | Parsed | Interpreted | Emit | Gap |
|---|---|---|---|---|
| function headers, exception handlers | ✅ | ✅ | ✅ | resize of a handler-bearing function is refused (Q3/Q4) |
| string table, storage, kinds, identifier hashes | ✅ | ✅ | ✅ | packing — `../../06_write/string_packing/PLAN.md` |
| array / literal-value / object key + value buffers | ✅ | ✅ decoded to `LiteralValue` (`parser/buffer.rs`) | ❌ empty only | write side only |
| bigint table + storage | ✅ | ✅ resolved by id (`parser/helpers.rs:8`) | ❌ empty only | write side only |
| object shape table | ✅ `ShapeTableEntry` | ✅ shape lookup (`parser/mod.rs:40`) | ❌ empty only | write side only; **v98+ only** |
| function source table | ✅ pairs | ✅ dumped and resolved (`inspect.rs:248`) | ❌ empty only | write side only |
| CJS module table | ✅ pairs | ✅ labelled by `options` bit 1 (`inspect.rs`) | ❌ empty only | write side only; OB2 closed by P5 |
| `options` byte | ✅ as a `u8` | ✅ `BytecodeOptions`, version-keyed (`format.rs`) | carried verbatim | OB1 closed by P5 |
| **RegExp table + storage** | table ✅, storage ❌ raw | ❌ | ❌ empty only | P3 |
| **debug info** | ✅ version-keyed (96/98/99) | ✅ locations + scopes; **not** the lexical/envIdx data | ❌ empty only | DI2 guarded (P0), DI1 read (P1), decompiler wiring is P1b (behind ../../03_analysis/closure_model/PLAN.md) |

The two bolded rows are the read-side gaps that remain; everything else on the list is a
write-side gap
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
acceptance test could not be written against a real artifact — it asserts the decoder on a
synthesised byte pattern, and will until someone finds the invocation that produces one.
Separately, `hermesc` at **v98 and v99 crashes** on `-commonjs` with a single-file input where
v96 succeeds, so the CJS path is only exercisable at v96 on these builds.

**Fixture recipe, now verified.** P0's missing debug-info fixture is
`hermesc -emit-binary -g3 -out <out>.hbc <in>.js`, and `-g3` does populate the section at all
three versions (`debug_info_offset` non-zero, every function flagged). That is the flag P0 asks
someone to confirm; it is confirmed.

P5's two fixtures came out of the same bench and are now committed: `hermesc -emit-binary
-out asyncy.v<N>.hbc asyncy.js` at each version, and `hermesc -emit-binary -commonjs -out
cjsdir.v96.hbc cjsdir/` — the latter needing a `metadata.json` in the directory (a `segments`
object listing the files), which the note above did not record and which is the one thing that
stops `-commonjs` before it starts.

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

### DI3 — the header parser is version-blind — ✅ **fixed**

> **Fixed since this was written.** `DebugInfo::parse` takes a version and
> `parse_header` branches on a version-keyed `DebugLayout` (`debug.rs`), so the
> 28/20/16-byte split is modelled and an unmodelled version (v97, and anything
> below v96 or above v99) returns nothing *deliberately* rather than reading the
> wrong shape. The remaining gap was that it returned nothing *silently* — R25's
> "wrongness in the return value of a call nothing checks" — and that is closed
> too: `parse_with_status` now reports a `DebugInfoStatus`, surfaced on
> `BytecodeFile::debug_info_status` and warned about by the CLI and MCP. See
> `../RISKS.md` F10. The narrative below is kept as written.


`DebugInfo::parse` takes `(bytes, debug_info_offset)` and **no version** (`debug.rs:88`), and
`parse_header` reads seven `u32`s unconditionally (`debug.rs:148-158`). On a v98 or v99
debug-built file it consumes 28 bytes where the header is 16: `scope_desc_offset` picks up the
real `debugDataSize`, the next three fields are filename-table bytes reinterpreted as offsets,
and `data_start` is computed from `28 +` when it should be `16 +`.

The bounds checks (`slice_in_bounds`, `slice_range`) then usually make this degrade to
`Ok(DebugInfo::default())` — silently "no debug info" — rather than crash, which is why it has
never been noticed. ~~It has also never been exercised: every committed fixture is built without
debug info.~~ **Wrong, and measured wrong** while building P0: every committed fixture carries
debug info on every function, so this path runs on *every* fixture parse at every version. It is
not unexercised — it is unasserted, which is worse, because the wrongness is in the return value
of a call nothing checks.

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
  version and no `ModernLayout`-style keying is needed. **[measured]** The stronger form of that
  claim also holds: the same pattern compiled by the v96, v98 and v99 `hermesc` builds produces
  the *same bytes*, not merely a stream in the same dialect — `/^nope-(\d+)$/i` is
  `010001000106010b054e4f50452d…` at all three. That is what makes P4a's donor trick work
  across versions.
- `syntaxFlags` **[source]** is `SyntaxFlags::toByte` (`Regex/RegexTypes.h:213`), bits
  `i g m u s y d` from bit 0 up, bit 7 unused; `constraints` is `MatchConstraintFlags`
  (`:180`) — `NonASCII`, `AnchoredAtStart`, `NonEmpty` in bits 0-2. **[measured]** on the
  11.39.0 bundle's 909 entries: no entry sets flag bit 7 and none sets a constraint above bit
  2, which corroborates the 6-byte header shape on real data rather than on the struct alone.
  The observed flag bytes are only six distinct values (`0`×507, `g`×239, `i`×125, `gi`×29,
  `gm`×8, `gim`×1).
- **[measured]** Storage is packed end to end with no gaps and no inter-entry padding: the 909
  entries tile `[0, 92533)` exactly, and every entry is at least a header long. The header's
  `regExpStorageSize` records that **unpadded** 92,533; the trailing pad to
  `BYTECODE_ALIGNMENT` (92,536 here) is layout, and is what our section walker reports.
  `regExpCount` and `regExpStorageSize` sit at header bytes **72 and 76 in both layouts** for
  v96 through v99, because the modern header always carries the bigint fields the legacy one
  gained at v87.

### OB1 — the `options` byte is never decoded **[source]** — ✅ **fixed by P5**

`BytecodeHeader::options` was a bare `u8` (`format.rs:80`) that **nothing in the crate read** —
grep `header.options` and every hit was some unrelated `Options` struct. Upstream it is a
bitfield, and it lost a bit inside the range we support:

| version | bits |
|---|---|
| v96 | `staticBuiltins`, `cjsModulesStaticallyResolved`, `hasAsync` |
| v98, v99 | `StaticBuiltins`, `CjsModulesStaticallyResolved` — `hasAsync` **removed** |

That is R8's shape once more: a version-keyed structure carried as an integer. It was harmless
only for as long as nothing read it, because bit 2 means `hasAsync` on one supported version
and nothing on another. And something *should* have been reading it — bit 1 decides what the
next table means.

`BytecodeOptions` now holds the version alongside the byte, so `has_async()` returns `None`
above v97 — *the bit does not exist*, which is a different claim from `Some(false)`. One more
wrinkle the table above does not show, and the reason the boundary is a named constant
(`OPTION_HAS_ASYNC_MAX_VERSION`) with the two commits written beside it: upstream neither
*added* the bit (2021-01-25, tree at v81) nor *removed* it (2025-02-25, tree at v98) with a
`BYTECODE_VERSION` bump. v98 is therefore a version whose meaning changed under it, and a v98
image built before that commit can still carry bit 2 — which surfaces through
`unknown_bits()` rather than being read as a flag that no longer exists.

### OB2 — the CJS module table has two meanings **[source]** **[compiler]** — ✅ **fixed by P5**

`Array<std::pair<uint32_t, uint32_t>>`, `cjsModuleCount` entries, 4-aligned like every section.
The reader chooses between two tables on `options.cjsModulesStaticallyResolved`
(`BytecodeDataProvider.cpp:300`), and the generator confirms what each pair holds — note that
the *argument* order of `addCJSModule` is the reverse of the *stored* order, which is exactly
the kind of thing reading only the reader would miss (`BytecodeGenerator.cpp:354-368`):

| bit | table | stored pair | built by |
|---|---|---|---|
| clear | `cjsModuleTable` | `{nameID, functionID}` — **filename string ID → function ID** | `addCJSModule(functionID, nameID)` |
| set | `cjsModuleTableStatic` | `{moduleID, functionID}` — **module index → function ID** | `addCJSModuleStatic(moduleID, functionID)` |

So `.second` is the function ID in **both** forms. That narrowed the defect considerably from
where the first pass left it: `inspect.rs:89` labelled the pair `(symbol_id, function_id)`, and
the second half was right either way. What was wrong is the first half on a statically-resolved
bundle, where the value is a module index and the label invites resolving it as a string id —
which would print an unrelated string, not an obvious error. The crate could not tell the two
apart, because the deciding bit lived in the byte OB1 never decoded. Fixing OB1 fixed this;
there was no separate format work. `CjsModuleForm` now names the two, and both the text and
JSON dumps say which one they are showing.

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
started at the wrong offset. P6's emitter has to preserve this. **P5 deliberately did not add
the reader-side check** the first pass imagined: our section extents are derived from where
the reader lands, not recorded independently, so there is nothing to compare a too-long
section against. Detecting a violation needs an independent length — which only an emitter,
or a parser that bounds sections some other way, could supply. It stays P6's.

### Function source table **[source]**

`Array<std::pair<uint32_t, uint32_t>>`, `functionSourceCount` entries, present from **v84**
(`header.rs:9`, `LEGACY_FUNCTION_SOURCE_MIN_VERSION`). Upstream:

> Mapping function ids to the string table offsets that store their non-default source code
> representation that would be used by `toString`. These are only available when functions are
> declared with source visibility directives such as 'show source', 'hide source', etc.

So: function ID → string ID, and normally empty. Parsed and resolved already (`inspect.rs:248`,
`dump --kind function-sources`); the only gap is emission, and the only thing that could
invalidate it is inserting or removing a function — a `../../06_write/relocation/PLAN.md` P3 concern, not a
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

### P0 — Guard the resize (fixes DI2) — ✅ **shipped**

**Goal.** Stop silently emitting wrong debug info. Mirror the exception-handler guard; refuse
rather than emit a function whose line table now points at the wrong instructions.

**Shipped as** a second guard in `patch_function_body` (`write/patch/functions.rs`), directly
after the handler check and inside the same `delta != 0` arm, plus
`PatchOptions::allow_stale_debug_info` and `--allow-stale-debug-info` on `asm`,
`patch-function` and `inject-stub`. The handler check stays first, so a function carrying both
still reports the handler reason — that one breaks execution, this one breaks only debugging.

**The guard is keyed on two things, not one**, and the second was not in the original plan:
`FLAG_HAS_DEBUG_INFO` on the function **and** `header.debug_info_offset != 0`. A file with no
debug section has nothing to invalidate, and that case is not hypothetical — `create` emits no
debug info at all but sets flags `0x12` on its legacy global function, which *includes*
`FLAG_HAS_DEBUG_INFO`. On the flag alone the guard refused edits to created images over debug
info they do not contain, and five unit tests said so immediately. The modern `create` path
emits `0x22` and does not claim it, so the two paths disagree; keying on the section means the
guard follows the data rather than a flag that is sometimes wrong. **`create`'s bogus legacy
flag is left as it is and recorded here** — changing what `create` emits is R14's territory, not
this guard's.

**Acceptance, as tested** (`tests/debug_info_guard.rs`, five cases): the fixture really does
carry debug info (checked first, or the rest asserts nothing); a size-changing `inject-stub` on
a debug-bearing function is refused with a message naming both debug info and the opt-out; the
same edit with the opt-out succeeds and reparses; an identical-body (same-size) edit is
unaffected; and a file with no debug section is unaffected. All five run at v96, v98 and v99.

**Two corrections to this plan's own premises**, both measured while building the guard:

- *"Needs a debug-info fixture, which does not exist today — every fixture is built with plain
  `-emit-binary`."* **Backwards.** Every committed fixture already carries
  `FLAG_HAS_DEBUG_INFO` on every function, `plain` included: `hermesc` emits per-function debug
  info without being asked. What no fixture had was *full* debug info, so
  `locations.debug.js` is committed and built with `-g3` — the `.debug.js` suffix is what tells
  `scripts/build_hermes_vm.ps1 -Fixtures` to pass the flag. It is the fixture P1 will need.
- *Refusing by default might regress a working flow.* It does not, and the number is the reason
  the default is safe: **0 of the Equinox bundle's 62,909 functions** carry
  `FLAG_HAS_DEBUG_INFO` [measured]. React Native ships bundles with per-function debug info
  stripped, so the guard cannot fire on the workflow this crate exists for. The escape hatch is
  still there, and `vm_verify`'s resize sweep uses it — which gives the opt-out the only
  coverage that matters: what comes out still runs on a real VM.

**What P0 does not do.** It refuses; it does not relocate. P2 is still the fix.

### P1 — Read the location streams (fixes DI1) — ✅ **shipped**

**Goal.** Populate `DebugInfo::source_locations`, so the two existing consumers start working.

**Shipped as** `DebugLayout::for_version` + `StreamEncoding` in `debug.rs` (an allow-list of
96 / 98 / 99, refusing the rest the way `ModernLayout` does), `parse_debug_offsets` in the
parser, and `DebugInfo::variable_map_for_function`. Pinned by
`debug_info_shapes_match_upstream` in `tests/upstream_pin.rs`, which derives the header size,
the `DebugOffsets` field count, the stream prologue length and the line-delta shift from each
checkout — and was checked by breaking each of them and watching it fail. Asserted by
`tests/debug_locations.rs` (8 cases), whose ground truth is the fixture's own line numbers.

**Three things this phase got wrong on paper, and the measurements that corrected them:**

1. *"`scope_offset` filled from `scopeAddress` at v96"* would have produced a working reader
   that still recovered no names. The stream's `scopeAddress` is the innermost scope live at
   one instruction, and upstream defaults it to the shared empty descriptor at offset 0 — on a
   five-function fixture, four report 0 while their real scopes sit at 3, 6, 9 and 13. **The
   function → scope link is `DebugOffsets.scopeDescData`**, the second field, now read as
   `DebugInfo::function_scopes`. Pinned by `the_scope_link_does_not_come_from_the_stream`.
2. **A name inside a scope descriptor is a byte offset into the debug string table, not an
   index** — upstream's `appendString` writes `stringTable_.size()` and `decodeString` seeks
   there for a LEB128 length. This crate read it as an index, which resolves the one string at
   offset 0 and yields empty for every other. On three captured variables that printed
   `["alpha", "", ""]`, which reads as *Hermes named one of them* rather than as a decode bug.
   Fixed; pinned by `every_captured_name_resolves_not_just_the_first`, which needs three names
   because a test with one would have passed throughout. This was a pre-existing defect, not a
   P1 regression — registered as R28.
3. *"`decompile` emits real local-variable names"* as the acceptance signal was written before
   anyone knew **Hermes only names captured variables**. A `Variable` exists for a captured
   binding; a plain local lives in a register and is never named at any optimization level. So
   the recovered names key on **environment slots**, which the decompiler renders as
   `closure_N`, while the renamer that consumes `debug_names` is register-indexed. See P1b.

**What works end to end now:** `hermes-decomp debug <file> --vars` prints the recovered names,
and `variable_map_for_function` returns them keyed by slot. Both consumers (`ir_gen`,
`pipeline`) were switched off the stream-scanning lookup, so they resolve the right scope.

**The spec this phase was executed from, kept as the record.** It was written before the
three corrections above were known — step 3 in particular is the `scopeAddress` mistake,
left standing so the corrections have something to be corrections *to*.

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

**Acceptance, as it turned out.** The end-to-end signal was specified as `decompile` emitting
real names; that is P1b, below, for the reason given there. What P1 itself delivers, and what the tests
assert: streams decode at v96/v98/v99; the decoded lines *are* the fixture's lines (8, 10, 12,
15 for `classify`; 18–21 for `total`); the two encodings agree about the same program, which is
the check that catches a missed prologue field or a collapsed address cursor; and
`debug --vars` prints four captured variable names that no version of this crate had ever
recovered.

**Pin it.** Add both version-keyed quantities to `tests/upstream_pin.rs` the way opcodes and
function-header fields already are: derive `sizeof(DebugInfoHeader)` and `sizeof(DebugOffsets)`
by counting `uint32_t` members in each checkout, and parse the shift width and flag-bit
meanings out of each checkout's `FunctionDebugInfoDeserializer`, asserting all of it matches
what the decoder implements. The v96 →
v97 → v98 drift documented above is exactly the shape R19 exists to catch, and it will happen
again.

### P1b — Put the recovered names in the decompiler — **specified and measured, not shipped**

**Goal.** `closure_0` → `count` in decompiled output.

**The rule, measured rather than assumed.** For an environment reference at IR level `L`,
slot `S`, in function `F`: walk `L + h` parent links up the debug scope chain from `F`'s own
scope (`DebugOffsets.scopeDescData`) and take `names[S]`, where `h` is 0 if `F` creates its own
environment and 1 if it does not. The discriminator is whether `F`'s body contains
`CreateEnvironment`, and both halves were checked on a four-function fixture:

| function | own env | IR level | hops | scope | name |
|---|---|---|---|---|---|
| `makeCounter` | yes | 0 | 0 | 9 | `count` |
| `bump` | no | 0 | 1 | 13 → 9 | `count` |
| `threeCaptures` | yes | 0 | 0 | 16 | `first`/`second`/`third` |
| `readsAll` | no | 0 | 1 | 22 → 16 | same |

**Why it is not shipped, which is a real blocker rather than an effort estimate.** The
rendered name `closure_N` is *load-bearing for other analyses*: seventeen sites across ten
files test or parse that spelling, including `analysis/metro/propagation`, which recovers a
slot id by `strip_prefix("closure_")`. Substituting a debug name before those run would break
Metro module analysis.

The blocker is **not** that there is nowhere to put the name — it is that the thing to attach
it to has already been destroyed. `resolve_closures` (`analysis/closure/mod.rs:139,217`,
reached from `pipeline/ir_gen.rs:244` and `pipeline/context/closures.rs:40`) lowers
`ClosureVar { level, slot }` to `Variable(String)` at stage W6, and `ClosureInfo::get_slot_name`
is what renders that string — it is the incumbent namer, not an unused hook. After W6 a debug
name and a placeholder are both just strings, distinguishable only by spelling. So print-time
substitution cannot work as originally sketched here: `Codegen` does carry injected analysis
context (`import_map`, `dep_names`, `dep_ids`, `inline_bodies`, with a `with_*` builder
pattern at `transforms/codegen/mod.rs:193`), and a `with_variable_names` would be routine —
but by then there is no `ClosureVar` left to key it on. Likewise `var_naming` already renames
captures beyond registers (`rename_closure_variables_cross_function`,
`rename_closures_from_definitions`); it does so **by string**, which is the same problem one
layer up.

`ClosureSlotValue` also has no rung for a better source of truth: it carries a value, never a
provenance, so a name Hermes itself recorded has no way to outrank one inferred from a store.
**See `../../03_analysis/closure_model/PLAN.md`** — that is where this belongs, it is justified without debug
info (94,453 rendered placeholders per Equinox run), and P1b becomes a short consequence of
its K1/K3 rather than a project. Two of its findings are P1b's own, arrived at early: the `h`
above is a bit the IR builder already observes and discards, and the slot→name map this phase
would produce is currently consumed as a *register*→name map.

**And the payoff is small**: Hermes names only captured variables, and only in files built with
debug info — of which the Equinox bundle has none (0 of 62,909 functions, re-measured). This is
decompiler cosmetics for debug builds. Worth doing once the closure model keeps its structure
to print time; not worth threading a name through a lossy lowering on a four-function sample,
where a wrong name would be worse than `closure_0`.


### P2 — Relocate addresses on resize — ✅ **shipped, for the edits where it means anything**

**Goal.** Keep a function's line table correct across a size-changing edit, instead of
refusing the edit.

**The finding that shaped it: only an insertion can be relocated, and that is not a
limitation of the implementation.** `inject-stub` adds instructions at one known point and
leaves the rest of the body alone, so old address `A` maps to `A` or `A + delta` and the line
table can follow. A wholesale body replacement — `asm`, `patch-function` — has no such
mapping: the new body is *different code*, and there is no answer to "which new address
corresponds to old address `A`". Relocating one would mean inventing a correspondence, so
those keep P0's refusal, with a message that now says which case it is and why. The plan's
"then drop the P0 guard" was written before that distinction existed.

**Shipped as** `write/patch/debug_reloc.rs` (`relocate_locations_for_insertion`), called by
`inject_stub` after the body edit. What keeps it small: **addresses are deltas**, so shifting
every address at or past the insertion point means adding `delta` to exactly *one* delta — the
first entry that crosses the point — because every later entry is relative to it. Nothing else
in the stream is read or rewritten, so statement deltas, `envReg`, `envIdx` and the conditional
fields survive without this code understanding them. When the re-encoded SLEB128 changes
length, the debug data region changes size, so the header's `debugDataSize`, its interior
region offsets (v96) and every later function's `DebugOffsets.sourceLocations` are adjusted;
the debug section is the last thing before the footer, so nothing beyond it needs to move.

**Acceptance, as tested** (`tests/debug_relocation.rs`, four cases at v96/v98/v99): an
insertion no longer needs the opt-out; every pre-edit location exists after the edit at the
address the insertion moved it to, carrying the same line; no other function's table moves; and
a wholesale replacement is still refused.

⚠️ **The first version of that test was vacuous and passed with relocation disabled** — it
skipped when no location existed at the mapped address instead of failing there. It now asserts
the mapping (`A` → `A` or `A + delta`) and requires at least one location to have actually
moved. Re-checked by disabling the splice and confirming the failure, which is the only way to
know a relocation test tests anything: relocation moves addresses and leaves lines alone, so
"the lines are unchanged" is true whether or not it ran.

`vm_verify`'s resize sweep runs through this path on debug-bearing fixtures and the results
still execute on real v96/v98/v99 engines, which is the check that the rewritten debug section
is not merely reparseable.

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

Nothing needs a *regex compiler* today, and the section-relative offsets mean nothing breaks
without one. Listed only so the boundary is explicit. Do not build it speculatively.

"Write-side RegExp" as originally written meant *authoring a regex bytecode stream from a
pattern string* — an assembler, which is why P4 was ordered behind P3's disassembler. That
piece is still speculative and still nobody's. What follows is not that.

### P4a — Put a chosen RegExp into a real bundle *(the need appeared)*

**Why this is a separate phase.** A concrete ask arrived — a regex to be added to the Equinox
bundle — and it turns out **not to need P3, and not to need an assembler at all**. The regex
bytecode stream is self-contained and its offsets are storage-relative, so a stream *compiled by
`hermesc`* transplants verbatim. Do not write a regex compiler to do what the reference compiler
will hand you. That inverts P4's ordering note for this case only: you do not have to
disassemble before you assemble, because you are not assembling.

**Get the payload.** Compile a throwaway one-liner and lift the entry out of it:

```bash
echo 'var re = /^nope-(\d+)$/i; print(re.test("nope-7"));' > donor.js
hermesc -emit-binary -out donor.v96.hbc donor.js
hermes-decomp dump donor.v96.hbc --kind regexp --json     # entry 0 is the payload
```

Any of the three compilers will do (see the byte-identity note above). Read the first six bytes
back before using them: `markedCount`, `loopCount`, `syntaxFlags`, `constraints`. They are the
contract with the calling code, not decoration — see the four traps below.

**Then pick the cheapest archetype that fits.** In cost order, which is also in
blast-radius order:

| | Archetype | Costs | Needs |
|---|---|---|---|
| **A** | **Repoint** an existing `CreateRegExp` at an entry the bundle already has | one instruction, in place | nothing new |
| **B** | **Overwrite** an entry's storage with donor bytes that fit its slot | storage write + the entry's `length` + SHA1 | nothing new |
| **C** | **Append** a new entry | table +8 B, storage +N, **tail relocation** | `add_string`-class shift |

`CreateRegExp` is `(Reg8 dst, UInt32S patternStrId, UInt32S flagsStrId, UInt32 regexpIndex)` at
v96, v98 and v99 alike **[source]** — 14 bytes, three of the four operands being ids. A is
therefore a pure operand edit of the kind `write/patch/operands.rs` already does, and it is the
right answer whenever the pattern you want is already among the bundle's regexes (909 of them at
11.39.0). Finding *which* index that is does not need P3 either: the pattern text is a string in
the table, so `xref --query <pattern>` reaches the `CreateRegExp` site and the site names the
index in its fourth operand. Which also means the search is over patterns the bundle's *own*
code uses — a table entry with no live `CreateRegExp` is invisible to that route.

**B is measured, end to end, on a real engine.** Compile a host with `/^equinox-(\d+)$/i` and a
donor with `/^nope-(\d+)$/i` (49 and 46 bytes); write the donor's bytes over the host's storage,
set the entry's `length` to 46, refresh the trailing SHA1; the file is byte-for-byte the same
size. The host then reports `equinox-42 -> false` and `nope-7 -> true` on a real engine at
**all three versions**. Three things that proves at once: the stream really is
position-independent, an entry may be *shrunk* by editing only its `length` (the slack tail is
never read), and the VM accepts a stream it did not compile for that file.

`scripts/regexp_transplant_demo.py --hermes <dir with hermesc and hvm>` is that run, kept
runnable so the claim can be re-checked rather than believed. It is a demo, not a test: it
builds its own throwaway bundles and touches nothing in the crate. The phase should land the
same sequence as a `vm_verify` case.

**C is the only one that needs code, and less than it looks.** Appending is *easier* than
`add_string`: no packing, no identifier hashes, no string kinds, no overflow entries, and no
interior fixups at all because storage offsets are storage-relative. It is two header counters
(bytes 72 and 76 — write `regExpStorageSize` **unpadded**), an 8-byte table entry, N bytes of
storage, and then the identical downstream shift: `debug_info_offset`, plus every function
header's body offset and, when overflowed, its info offset. On 11.39.0 that shift moves
10,302,900 bytes of bytecode and touches 62,909 function headers — the regexp section sits at
0x6213AC, well upstream of the instruction stream at 0x63B56C.

⚠️ **Do not copy the shift a fourth time.** It exists three times already and that is R26.
Factor `add_string`'s tail relocation into something both callers use *before* adding the regexp
caller, or the phase makes a known problem worse for a section that needed the least of it.

**Four traps, two of them measured.**

- **`.source` and `.flags` do not follow the bytecode.** They are `CreateRegExp`'s two string
  operands, not the stream. **[measured]** the transplanted host above still reports
  `source: ^equinox-(\d+)$` while matching `nope-`. Anything that logs, serialises or
  re-compiles from `.source` sees the old pattern. Fix it with `add_string` + an operand edit,
  or accept the divergence deliberately — but never by accident.
- **`markedCount` is a contract with the calling code.** It is the capture-group count; code
  doing `m[2]` on a pattern that now has one group gets `undefined`, silently and far from the
  edit. Compare the donor's `markedCount` against the old entry's before writing.
- **Literals are case-folded into the stream when `i` is set.** **[measured]** `/^equinox-…/i`
  stores `455155494e4f582d` — `EQUINOX-`, upper case. Eyeballing a literal in a hex dump and
  editing it in place is a trap; recompile a donor instead.
- **`syntaxFlags` in the donor header must agree with the flags *string*.** They are two
  independent encodings of the same thing, and only the header one affects matching. A donor
  compiled with different flags than the site's `flagsStrId` names will match one way and report
  another.

**Acceptance.** A `regexp-set` op that takes a donor `.hbc` (or a pattern it compiles by
shelling out to a configured `hermesc`) and an entry index, does B when the payload fits and C
when it does not, and refuses rather than guessing when `markedCount` drops below the old
entry's. Tested at v96, v98 and v99: the patched image reparses, `dump --kind regexp` shows the
new bytes, and `vm_verify` runs it on a real engine and observes the *new* matching behaviour —
the last being the only assertion that distinguishes "reparses" from "works", exactly as it was
for the debug relocation in P2. Then the C arm once against the 11.39.0 corpus bundle, because a
10 MB tail shift over 62,909 headers is not a thing to first attempt in production.

**Cost.** A is hours and needs nothing. B is a day with the test. C is the real work, and most
of it is the R26 refactor that should happen anyway.

**Not in scope.** Compiling a pattern string to bytecode ourselves (that is P4, still
speculative), and *inserting* a `CreateRegExp` where none exists — that is an instruction
insertion, so it is `inject_stub` plus P2's debug relocation, and it is only worth doing if the
site genuinely has no regex to repoint.

### P5 — Decode the options bitfield (fixes OB1, and OB2 with it) — ✅ **shipped**

**Goal.** Stop carrying a version-keyed structure as an integer, and let the CJS table be
labelled correctly.

**Done.** `BytecodeOptions` (`format.rs`) is a newtype over `(byte, version)` with
`static_builtins()`, `cjs_modules_statically_resolved()`, and `has_async() -> Option<bool>`,
`None` above **v97** — the plan said v96, and the checkouts say the bit survives at v97 and dies
at v98. Plus `unknown_bits()`, which reports set bits this version does not define: upstream
removed `hasAsync` without a version bump, so a v98 image built before that commit is a real
byte this must not silently mis-read. The raw byte stays on the header, renamed `options_raw`
to state at the declaration that it is carried verbatim, and read through
`BytecodeHeader::options()`; the write path still never touches it. `dump --kind cjs-modules`
keys both its text and JSON labels on bit 1 and names the form it is showing (`CjsModuleForm`),
and `info` prints the decoded byte.

**Pinned.** `upstream_pin.rs::bytecode_options_bits_match_upstream` parses the bitfield
members out of `BytecodeFileFormat.h` in each checkout and checks the bit order and the *set* of
bits. It is more than the two lines this predicted, for two reasons. First, there are **two
declaration shapes** inside the supported range — a `union` over a `bool : 1` chain at v96/v97,
and upstream's `HERMES_FIRST_BITFIELD` / `HERMES_NEXT_BITFIELD` macros from v98 — and the commit
that changed the shape is the same commit that dropped `hasAsync`, so a parser handling one form
would report the drop as "cannot parse" rather than as a missing bit. Second, nothing in the
test is transcribed: every expectation is re-derived from `BytecodeOptions` itself, which makes
an **added** bit (an unmodelled name panics), a **removed** bit (the defined mask stops matching
upstream's count) and a **reordered** bit (each bit is lit alone and must be seen by the
accessor upstream names for that position) three distinct failures rather than one silent pass.
Widths are checked too — a field widened to two bits would shift everything above it while the
bits underneath kept passing. Mutation-checked: moving `OPTION_HAS_ASYNC_MAX_VERSION` from 97 to
98 fails at v98 with the mask mismatch.

**Acceptance**, in `tests/bytecode_options.rs`, against two fixtures compiled for it and
committed beside the others. `cjsdir.v96.hbc` (from `tests/fixtures/cjsdir/`, which carries the
`metadata.json` that `-commonjs` requires of a directory input) dumps as filename string IDs,
labelled as such, its two entries resolving to `index.js` and `helper.js` **[measured]**.
`asyncy.v{96,98,99}.hbc` is the same async source at three versions: `options = 0b100` at v96,
`0b000` at v98 and v99 — R27's drift as an artifact, not an inference. The statically-resolved
arm has **no artifact**: it could not be produced with these compilers (see What compiling
actually showed), so its decoder is asserted against a synthesised byte, and the test that does
it says so in its name. That test earns its keep anyway — it flips bit 1 on the *real*
unresolved table and requires the labels to change and the filenames to stop being resolved,
which is OB2's failure mode exactly: those string ids are valid indices, so mislabelling them
printed a plausible wrong answer rather than an error.

**Cost.** Hours, as predicted — the cheapest item in this document, and the only one that fixed
a wrong output rather than a missing one.

### P6 — Emission: what a total serializer owes each region

**Goal.** The write-side half of the inventory, and the thing `../../06_write/relocation/PLAN.md` P3 is
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
P0  ✅ ──────────► shipped: the guard
P1  ✅ ──► P2 ✅    shipped: the reader, then relocation for insertions
P1b                specified and measured; blocked on the closure model — ../../03_analysis/closure_model/PLAN.md K1/K3
P3  ──► P4         disassemble before you assemble; P4 only on demand
P4a ──────────────► independent of both: the donor comes from hermesc, not from us
P5  ✅ ────────────► shipped: the bitfield, and the CJS labels with it
P6                 blocked on P1+P2 for debug info, and on a demand that does not exist yet
```

P0 before P1 is deliberate: the guard removes a live correctness hole in a day, while P1 is
the larger piece of work. Shipping P1 first would leave DI2 open for the duration.

P5 was unordered with respect to everything else and the smallest item here; it was separated
out only because it fixed an output that was *wrong* rather than one that was *missing*, which
made it worth more than its size suggested.

P4a is likewise unordered, and for a reason worth stating plainly: it was filed under P4 —
behind P3, behind a regex assembler — until someone asked for the thing rather than for the
capability, at which point most of the phase turned out not to exist. The payload comes from
`hermesc`. Two of its three archetypes need no new code at all.

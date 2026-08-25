# Impl plan — debug info and RegExp

Scoped plan for the last two sections the write path treats as opaque bytes. Written so an
impl agent can execute without re-deriving the formats. Everything marked **[source]** was
read out of the Hermes checkouts wired up for `tests/upstream_pin.rs`
(`HERMES_SRC_V96`/`_V97`/`_V98`/`_V99`); everything marked **[code]** is a file:line in this
tree at the time of writing — re-check both, and prefer re-deriving to trusting the tables
below, which is the whole lesson of R8/R19.

Companion to `WRITE_PATH_GUIDE.md` § Pending impl plans. Same conventions: derive from
upstream, pin what you derive, refuse rather than approximate.

---

## What is actually true today

The one-line limitation in the guide — *"Debug info & RegExp are opaque `u8` buffers, not
parsed into typed structs"* — is half stale and half understated. Precisely:

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

---

## Non-goals

- **Emitting debug info for functions that lack it.** Out of scope at every phase.
- **Source-map generation.** Downstream of P1; a separate plan if wanted.
- **v97 support anywhere in this plan.** v97 is refused by `ModernLayout` on measured grounds
  (see the guide's modern-layout bullet) and never shipped. Its stream encoding is documented
  above so the *shape of the drift* is on record, not because it needs implementing.
- **Making debug info survive a string-table rebuild.** Separate concern from DI2; the debug
  string table is its own table and is not affected by `strings.rs`.

## Ordering

```
P0  ──────────────► ship immediately, independent
P1  ──► P2         relocation needs a reader first
P3                 independent of all of the above
```

P0 before P1 is deliberate: the guard removes a live correctness hole in a day, while P1 is
the larger piece of work. Shipping P1 first would leave DI2 open for the duration.

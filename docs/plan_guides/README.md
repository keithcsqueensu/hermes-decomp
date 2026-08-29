# Plan guides — what lives where

Six documents, and they are not siblings: five of them were **split out of another one**, each
time because a limitation bullet or a phase grew past the point where its parent could carry it.
That lineage is the thing worth writing down, because it is also the failure mode — see
§ Splitting.

Every document states its own **Ownership** near the top: what it owns, and what it delegates.
That block is the contract. This file is the index over those blocks, not a second copy of them.

---

## The two roots

The crate has two paths and one guide each. Everything else hangs off them.

| | Root | Owns |
|---|---|---|
| **read** | `READ_PATH_GUIDE.md` | `parse_auto` → disasm → IR → analysis → CLI/MCP. Robustness findings F1–F14, and what degrades silently |
| **write** | `WRITE_PATH_GUIDE.md` | `write/` + `write_cmd.rs`. Invariants, hazards, the op inventory, the risk register |

## The lineage

```
WRITE_PATH_GUIDE.md ──── "No string dedup/merge" ─────────► STRING_PACKING_PLAN.md
        │
        └────────────── "apply_reloc … unimplemented" ────► RELOCATION_PLAN.md
                                                                    │
                                          P3 needs a total serializer│
                                                                    ▼
READ_PATH_GUIDE.md ◄── F10 is the symptom ───────► UNMODELED_REGIONS_PLAN.md
                                                                    │
                                       P1b needs somewhere to put a name
                                                                    ▼
                                                          CLOSURE_MODEL_PLAN.md
```

| Document | Split from | Owns | Status |
|---|---|---|---|
| `STRING_PACKING_PLAN.md` | write guide, design limits | how string storage is laid out and could be repacked | researched; P0 worth doing regardless |
| `RELOCATION_PLAN.md` | write guide, design limits | the absolute-offset surface; splice-and-shift | P0–P2 specified, ~1 day; P3 on a named trigger |
| `UNMODELED_REGIONS_PLAN.md` | relocation P3 | per-region read / interpret / emit status + formats | P0, P1, P2, P5 shipped; P1b, P3, P4, P4a, P6 open |
| `CLOSURE_MODEL_PLAN.md` | unmodeled-regions P1b | the decompiler's closure / env-slot model | K1–K4 specified, none shipped |

Two edges in that diagram are worth reading twice, because they are the ones people trip on:

- **The chain crosses sides.** `RELOCATION_PLAN` P3 (write) is blocked on
  `UNMODELED_REGIONS_PLAN` (mostly read), which in turn hands `CLOSURE_MODEL_PLAN` a decompiler
  problem. So "this is write-path work" stops being true two links down. The write guide's
  index says so explicitly: nothing in the write path waits on the closure model.
- **`READ_PATH_GUIDE` F10 and `UNMODELED_REGIONS_PLAN` DI3 are the same bug from two ends.**
  F10 owns "every debug-info failure looks like *no debug info*"; the plan owns why the header
  was read at the wrong size. Neither is a duplicate of the other, and neither should absorb
  the other.

---

## Splitting

**The rule: one home per fact. Everywhere else is a pointer, not a paraphrase.**

Every split in the lineage above did the same thing — moved the *work* into a child, and left a
*summary of the child's argument* in the parent. A pointer stays true forever. A summary has to
be re-verified every time the child changes, and nothing makes it fail loudly when it isn't.

That is not hypothetical. `UNMODELED_REGIONS_PLAN` P1b carried three claims about the
decompiler that the code contradicted; the write guide's Open work index carried a paraphrase of
those claims; so correcting P1b left a wrong sentence one document upstream, in a file whose
author had no reason to look. The chain is exactly as long as the number of places a fact got
restated.

When splitting a section out of a document:

1. **Move the content; do not copy it.** The parent keeps the limitation or the phase heading,
   one line saying why it matters *to that document's reader*, and a pointer. If the parent's
   bullet still contains file:line references, measurements or an argument, it is a summary and
   it will drift — cut it down.
2. **Write the child's Ownership block first**, naming what it took and what it left behind.
   If you cannot draw the boundary in two sentences, the split is in the wrong place.
3. **Say what the parent now delegates**, in the parent's own Ownership block.
4. **Leave the parent's numbering alone.** P1b stays P1b even after its substance moves; the
   number is how three other documents refer to it.
5. **Check upstream, not just downstream.** Grep the whole folder for the phase number or the
   claim you just changed — that is what catches the paraphrase two documents up.

## Conventions shared by all six

- **[source]** — read out of a pinned Hermes checkout (`tests/upstream_pin.rs`,
  `HERMES_SRC_V96`/`_V97`/`_V98`/`_V99`).
- **[compiler]** — checked against upstream's *serializer/generator*, not just the format
  header and reader. The writer states things no reader can.
- **[measured]** — reproduced by running this tree, usually against the shipped Equinox v96
  bundle (`com.equinoxfitness.equinox_11.39.0`, 16,837,408 B, 62,909 functions, 98,917 strings)
  or a `hermesc`-built fixture under a real VM.
- **[code]** — `file:line` in this tree at time of writing. **Re-derive before trusting.** Every
  document says this; it is the standing rule, and R8/R19 are why.
- Refuse rather than approximate. An unknown version is an error, not a best guess —
  `ModernLayout::for_version` and `DebugLayout::for_version` are the pattern.
- A phase that is shipped says so in its heading, and says what pins it.

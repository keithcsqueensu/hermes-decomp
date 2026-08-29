# Plan guides — risk registers and mitigation plans, on the pipeline spine

These are the **risk + prescriptive** counterpart to `../arch_guides/` (which is descriptive).
Both folders now share one axis — the pipeline spine — so the mapping is 1:1 by stage number:

| Stage | Describes it (arch) | Risks + plans (here) |
|---|---|---|
| read | `../arch_guides/01_READ_LAYER.md` | [`01_read/RISKS.md`](01_read/RISKS.md) + `01_read/unmodeled_regions/` |
| ir | `../arch_guides/02_IR.md` | [`02_ir/RISKS.md`](02_ir/RISKS.md) |
| analysis | `../arch_guides/03_ANALYSIS.md` | [`03_analysis/RISKS.md`](03_analysis/RISKS.md) + `03_analysis/closure_model/` |
| transforms | `../arch_guides/04_TRANSFORMS_CODEGEN.md` | [`04_transforms/RISKS.md`](04_transforms/RISKS.md) |
| pipeline | `../arch_guides/05_PIPELINE.md` | [`05_pipeline/RISKS.md`](05_pipeline/RISKS.md) |
| write | `../arch_guides/06_WRITE_PATH.md` | [`06_write/RISKS.md`](06_write/RISKS.md) + `06_write/reference/`, `relocation/`, `string_packing/` |
| frontends | `../arch_guides/07_FRONTENDS.md` | [`07_frontends/RISKS.md`](07_frontends/RISKS.md) |

Each stage has a **risk register** (`RISKS.md`) as its entry point. A short one is a valid
vertebra — the spine needs all of them, and "no open risks, audited" is a real state. **Deep
mitigation plans tree underneath the stage that owns them.** Every register and plan states its
own **Ownership** near the top: what it owns, what it delegates. That block is the contract;
this file is the index over those blocks, not a second copy.

---

## The tree

```
plan_guides/
  01_read/
    RISKS.md                  read-path robustness — F1,F2,F5,F6,F7,F10,F11,F12,F14
    unmodeled_regions/PLAN.md the regions carried but not modelled (debug info, RegExp, …)
  02_ir/
    RISKS.md                  F9 — unbounded render recursion
  03_analysis/
    RISKS.md                  register (points to the closure model)
    closure_model/PLAN.md     the decompiler's closure / env-slot model
  04_transforms/
    RISKS.md                  register (audited; no open risks today)
  05_pipeline/
    RISKS.md                  F8, F13 — the cache
  06_write/
    RISKS.md                  invariants, design limits, risk register (R1–R28), open Qs, open work
    reference/
      VERSION_LAYOUTS.md      reference VMs, the v99 delta, the v99 opcode drift, v97's two tables, the legacy/modern audit
      HARNESSES_AND_HISTORY.md  the test-harness catalogue and the git-history findings
    relocation/PLAN.md        the absolute-offset surface; splice-and-shift (R26)
    string_packing/PLAN.md    how string storage is laid out and could be repacked
  07_frontends/
    RISKS.md                  F3, F4 — the MCP surface
```

## Two dissolved roots

This folder used to have **two roots** — `READ_PATH_GUIDE.md` and `WRITE_PATH_GUIDE.md` — and
everything else branched off them. Both are now dissolved onto the spine:

- **`READ_PATH_GUIDE.md`** was a hardening review of the whole read *side*, so it genuinely
  spanned stages. It is now `01_read/RISKS.md`, and five of its fourteen findings moved to the
  stage where the hazard actually lives (see the finding index below). Its framing — the
  headline, the shape-of-fix, the order the pass was applied, the harness appendix — stays in
  the read register as the record of that one pass.
- **`WRITE_PATH_GUIDE.md`** was entirely *one* stage, so nothing scattered. It is now
  `06_write/RISKS.md`, with its background/derivation matter pulled into `06_write/reference/`.

## The finding index

The read hardening pass numbered its findings **F1–F14**; those numbers are shared across the
read/ir/pipeline/frontends registers. The write path numbers its durable hazards **R1–R28**,
its resolved design decisions **Q1–Q9**, and its invariants **I1–I13** — all in
`06_write/RISKS.md`. Where each lives:

| ID | Stage register | ID | Stage register |
|---|---|---|---|
| F1, F2, F5, F6, F7, F10, F11, F12, F14 | `01_read/RISKS.md` | F9 | `02_ir/RISKS.md` |
| F8, F13 | `05_pipeline/RISKS.md` | F3, F4 | `07_frontends/RISKS.md` |
| R1–R28, Q1–Q9, I1–I13 | `06_write/RISKS.md` | | |

## The lineage — what was split from what

The mitigation plans are not siblings; each was **split out of another document** when a
limitation bullet or a phase grew past the point where its parent could carry it. That lineage
is worth keeping, because it is also the failure mode — see § Splitting.

```
06_write/RISKS.md ──── "No string dedup/merge" ─────────► string_packing/PLAN.md
    │  (was WRITE_PATH_GUIDE.md)
    └────────────────── "apply_reloc … unimplemented" ──► relocation/PLAN.md
                                                                    │
                                          P3 needs a total serializer│
                                                                    ▼
01_read/RISKS.md ◄── F10 is the symptom ─► 01_read/unmodeled_regions/PLAN.md
    │  (was READ_PATH_GUIDE.md)                                     │
                                       P1b needs somewhere to put a name
                                                                    ▼
                                              03_analysis/closure_model/PLAN.md
```

| Plan | Split from | Owns | Status |
|---|---|---|---|
| `06_write/string_packing/PLAN.md` | write register, design limits | how string storage is laid out and could be repacked | researched; P0 worth doing regardless |
| `06_write/relocation/PLAN.md` | write register, design limits | the absolute-offset surface; splice-and-shift | P0–P2 specified, ~1 day; P3 on a named trigger |
| `01_read/unmodeled_regions/PLAN.md` | relocation P3 | per-region read / interpret / emit status + formats | P0, P1, P2, P5 shipped; P1b, P3, P4, P4a, P6 open |
| `03_analysis/closure_model/PLAN.md` | unmodeled-regions P1b | the decompiler's closure / env-slot model | K1–K4 specified, none shipped |

Two edges cross stages and are the ones people trip on:

- **The chain crosses sides.** `relocation/PLAN.md` P3 (write) is blocked on
  `unmodeled_regions/PLAN.md` (mostly read), which hands `closure_model/PLAN.md` a decompiler
  problem. So "this is write-path work" stops being true two links down — the write register's
  Open work index says so: nothing in the write path waits on the closure model.
- **`01_read/RISKS.md` F10 and `unmodeled_regions/PLAN.md` DI3 are the same bug from two ends.**
  F10 owns "every debug-info failure looks like *no debug info*"; the plan owns why the header
  was read at the wrong size. Neither is a duplicate of the other, and neither should absorb it.

---

## Splitting

**The rule: one home per fact. Everywhere else is a pointer, not a paraphrase.**

Every split above did the same thing — moved the *work* into a child, and left a *pointer* to
the child's argument in the parent. A pointer stays true forever. A summary has to be
re-verified every time the child changes, and nothing makes it fail loudly when it isn't.

That is not hypothetical. `unmodeled_regions/PLAN.md` P1b once carried three claims about the
decompiler that the code contradicted; the write register's Open work index carried a
paraphrase of those claims; so correcting P1b left a wrong sentence one document upstream, in a
file whose author had no reason to look. The chain is exactly as long as the number of places a
fact got restated.

When splitting a section out of a document:

1. **Move the content; do not copy it.** The parent keeps the limitation or phase heading, one
   line saying why it matters *to that document's reader*, and a pointer. If the parent's bullet
   still contains file:line references, measurements or an argument, it is a summary and it will
   drift — cut it down.
2. **Write the child's Ownership block first**, naming what it took and what it left behind. If
   you cannot draw the boundary in two sentences, the split is in the wrong place.
3. **Say what the parent now delegates**, in the parent's own Ownership block.
4. **Leave the parent's numbering alone.** P1b stays P1b even after its substance moves; the
   number is how three other documents refer to it. Likewise F9 stays F9 in the ir register.
5. **Check upstream, not just downstream.** Grep the whole folder for the phase/finding number
   or the claim you just changed — that is what catches the paraphrase two documents up.

## Conventions shared by every register and plan

- **[source]** — read out of a pinned Hermes checkout (`tests/upstream_pin.rs`,
  `HERMES_SRC_V96`/`_V97`/`_V98`/`_V99`).
- **[compiler]** — checked against upstream's *serializer/generator*, not just the format
  header and reader. The writer states things no reader can.
- **[measured]** — reproduced by running this tree, usually against the shipped Equinox v96
  bundle (`com.equinoxfitness.equinox_11.39.0`, 16,837,408 B, 62,909 functions, 98,917 strings)
  or a `hermesc`-built fixture under a real VM.
- **[code]** — `file:line` in this tree at time of writing. **Re-derive before trusting.**
- Refuse rather than approximate. An unknown version is an error, not a best guess —
  `ModernLayout::for_version` and `DebugLayout::for_version` are the pattern.
- A phase that is shipped says so in its heading, and says what pins it.

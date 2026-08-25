# CLI reference

Binary: **`hermes-decomp`**. Input: a `.hbc` or React Native `.bundle`.

## Common flags

| Flag | Description |
|---|---|
| `--layout <auto\|legacy\|modern>` | File header layout (default: `auto`) |
| `--function-layout <auto\|legacy16\|modern12>` | Per-function header layout (default: `auto`) |
| `--format-version <N>` | Override detected HBC bytecode version |

## Commands

### Info / versions

```bash
hermes-decomp info app.hbc
hermes-decomp versions          # HBC opcode tables 40-99
```

### Disasm

```bash
hermes-decomp disasm app.hbc --function 5 --output disasm.txt
# --show-offsets  --no-labels  --no-strings  --info
```

### Decompile

```bash
hermes-decomp decompile app.hbc -o decompiled.js
hermes-decomp decompile app.hbc --function 5
hermes-decomp decompile app.hbc --modules 100-150,200
hermes-decomp decompile app.hbc --module-name "Login*,Auth*"
hermes-decomp decompile app.hbc --exclude-module-name "react*,lodash*"
hermes-decomp decompile app.hbc --from-module 42 --module-depth 3
hermes-decomp decompile app.hbc --function 5 --json
```

Useful options: `--resolve-closures`, `--expand` / `--expand-depth N`,
`--show-offsets`, `--no-strings`, `--no-propagate`, `--no-simplify`,
`--no-structure`, `--check-dead-code`, `--assembly`, `--json`,
`--modules`, `--module-name`, `--exclude-module-name`,
`--from-module`, `--module-depth`, `--no-cache`.

Full-bundle `decompile` and runs with `-o` print **progress on stderr**.

**Analysis cache:** first run writes `<input>.hdcache` next to the file
(~0.2s reloads later). Keyed by SHA-256(bytecode) **and** SHA-256(binary).
Delete the `.hdcache` or pass `--no-cache` to force a rebuild.

### Explore / analyze

```bash
hermes-decomp tui app.hbc
hermes-decomp tui app.hbc --input2 app_v2.hbc   # split-view diff
# --diff-code

hermes-decomp bin-diff v1.hbc v2.hbc            # --diff-code
hermes-decomp xref app.hbc --query "loginWithToken"
hermes-decomp xref app.hbc --query 42 --kind function

hermes-decomp graphviz app.hbc --function 5 --open
hermes-decomp callgraph app.hbc
hermes-decomp callgraph app.hbc --function 42 --depth 3 --dot > calls.dot

hermes-decomp extract app.hbc -o modules/
hermes-decomp modules app.hbc --limit 50
hermes-decomp deps app.hbc --module 0 --depth 3

hermes-decomp dump app.hbc --kind strings
hermes-decomp dump app.hbc --kind obj-shapes --json
# kinds: strings, functions, cjs-modules, regexp, obj-shapes,
#   function-sources, string-kinds, sections, big-int, array-buffer

hermes-decomp closures app.hbc --function 5
hermes-decomp debug app.hbc --vars    # also --scopes, --callees
```

### Secrets / Frida

```bash
hermes-decomp secrets app.hbc
hermes-decomp secrets app.hbc --json --show-full

hermes-decomp frida-hooks app.hbc --module 42 -o ./hooks
hermes-decomp frida-hooks app.hbc --module 42 --export "login,logout" -o ./hooks
# writes before.js / after.js / agent.js / run.sh
```

### Bytecode write path (not JS recompilation)

HASM = our disasm dialect. Patches the binary. Does **not** recompile decompiled JS.

```bash
hermes-decomp emit-hasm app.hbc --function 5 -o f5.hasm
hermes-decomp asm app.hbc f5.hasm --function 5 -o app_patched.hbc
hermes-decomp asm-check app.hbc --function 5

hermes-decomp add-string app.hbc --value "myNewString" -o app2.hbc
hermes-decomp add-string app.hbc --value "myProp" --identifier -o app2.hbc
hermes-decomp retarget-string app.hbc --from "H:mm" --to "HH:mm" -o app2.hbc
hermes-decomp retarget-string app.hbc --from-id 5 --to-id 42 -o app2.hbc
hermes-decomp patch-operand app.hbc --at 0xD83E27 --string "black" -o app2.hbc
hermes-decomp patch-operand app.hbc --function 42 --insn-offset 0x1A --string-id 72 -o app2.hbc
hermes-decomp patch-string app.hbc --old "done" --new "fini" -o app2.hbc
hermes-decomp patch-string app.hbc --id 42 --new "hello" -o app2.hbc
hermes-decomp patch-function app.hbc --function 5 --hasm f5.hasm -o app2.hbc
hermes-decomp inject-stub app.hbc --function 5 --kind log -o app2.hbc
hermes-decomp create --version 96 -o tiny.hbc
```

Legacy files (HBC 96 and below) are fully supported and verified against the real
Hermes VM. `patch-string` handles both same length edits, done in place, and
length changes, where it rebuilds the string table and relocates the tail. It
refuses to patch Hermes packed strings whose storage overlaps another entry.

Modern files (HBC 97 and above, with 12 byte headers) are supported for string
patches (same length and length changing), `add-string`, function body resize,
and `inject-stub` resize, including relocation of the out of line large function
headers. All of these are verified on a real v98 Hermes engine. `create` builds a
minimal file from scratch, legacy layout for v96 and lower and modern layout for
v97 and newer. The CLI prints a note when a write command targets a modern file.

`patch-operand` rewrites a single string-id operand inside one instruction
without rebuilding the function body. Addresses by absolute file offset (`--at`)
or function-relative (`--function` + `--insn-offset`). Resolves the new value
by string text (`--string`, must already exist in the table) or numeric id
(`--string-id`). Validates the instruction shape, checks that the new id fits
the operand width (UInt8S/UInt16S/UInt32S), and read-back verifies after the
write. For opcodes with multiple string operands (e.g. `CreateRegExp`), use
`--operand-index`.

`retarget-string` makes one string entry resolve to the same value as another by
copying its 4-byte `SmallStringTableEntry`. Metadata-only: no table rebuild, no
storage growth, no code change. Every instruction that references the source id
now gets the target's value. Accepts `--from-id`/`--to-id` (numeric) or
`--from`/`--to` (by value, first match). Refuses overflow entries; warns when
crossing string/identifier boundaries. If the source is an identifier, its hash
is updated to match the target's value.

`add-string` appends a new entry to the string table and prints its id to stdout.
Every existing string id stays stable. Pass `--identifier` for property or symbol
names (adds a Jenkins hash slot). Encoding is chosen by content: pure ASCII uses
one byte per character; anything with a non-ASCII character uses UTF-16. If the
value already exists, a note is emitted to stderr but the string is still appended
(no silent dedup).

#### Verifying patched output on a real Hermes VM

A patched `.hbc` that reparses is not the same as one that runs. Every defect
found in the write path's modern branch produced an image that reparsed perfectly
and was mis-executed or rejected by the real engine, so reparsing is the weaker
check by a wide margin. Verify by running the output.

`hvm` is a standalone command-line Hermes VM driver: give it a `.hbc` path and it
executes it, printing the program's output and exiting non-zero on an uncaught
error. It is an ordinary subprocess, so the crate stays fully Rust with no C++,
no FFI and no C++ in `build.rs` — nothing needs to link `hermesvm`.

> An earlier version of this section claimed modern output could only be verified
> from C++, on macOS, via a helper script. That was wrong on all three counts, and
> the script it named never existed in this repo. The reasoning ("`hermesvm`
> exports only mangled C++/JSI symbols with no C ABI") is correct but irrelevant:
> you do not need to *link* the VM, only to run it.

**One binary per bytecode version.** An `hvm` refuses anything but its own version:

```
$ hvm file-v96.hbc
Wrong bytecode version. Expected 99 but got 96
```

so there is no single VM that covers everything. Build the ones you need:

```powershell
# Builds hvm + hermesc for that version into a git worktree beside your clone,
# applies the MSVC/CMake portability patches, and smoke-tests the result.
./scripts/build_hermes_vm.ps1 -Version 96 -HermesRepo C:\src\hermes-src -Fixtures
```

Supported versions are 96 (the layout the Equinox bundles use), 98 and 99. The
script prints the environment variable to set when it finishes.

`-HermesRepo` is a plain clone with full history; each version is built in its own
`git worktree` beside it, so the clone is never touched. Keep the clone's directory
name *out* of the `hermes-v<N>` pattern — the script refuses to run if the worktree
path it derives turns out to be the clone itself.

```powershell
96, 98, 99 | ForEach-Object {
    ./scripts/build_hermes_vm.ps1 -Version $_ -HermesRepo C:\src\hermes-src -Fixtures
}
```

⚠️ **v99 means the React Native release branch**, `origin/260318099.0.0-stable`,
not `static_h`. Both declare `BYTECODE_VERSION = 99` and their
`BytecodeFileFormat.h` is byte-identical, so nothing about the header layout can
tell them apart — but `static_h` carries a later `NewFastArray` that takes a third
operand, making the instruction 5 bytes where a shipped v99 bundle has 4. RN ships
from the release branch, so that is the dialect this crate encodes.

**Running the checks:**

```powershell
$env:HERMES_VM_V96 = 'C:\src\hermes-v96\build\bin\Release\hvm.exe'
$env:HERMES_VM_V99 = 'C:\src\hermes-v99\build\bin\Release\hvm.exe'
cargo test --test vm_verify
```

`crates/hbc-decomp/tests/vm_verify.rs` runs each write op against committed
fixtures and asserts on the VM's stdout and exit code. With no `HERMES_VM_V*` set
the tests still run and still assert everything that does not need a VM; only the
"and it runs" step is skipped, with a printed note. CI without a Hermes build
therefore degrades to reparse-only coverage rather than failing.

Three further suites work the same way, each checking against a different external
source of truth. All are opt-in, so a checkout without these artifacts still builds
and tests — see **Requiring the oracles** below for how to make a run refuse to skip:

| Suite | Checks against | Env |
|---|---|---|
| `tests/vm_verify.rs` | a real Hermes VM: does the patched image run | `HERMES_VM_V96` / `_V98` / `_V99` |
| `tests/upstream_pin.rs` | the Hermes sources: does our format model still match `FUNC_HEADER_FIELDS` and `BytecodeList.def` | `HERMES_SRC_V96` / `_V97` / `_V98` / `_V99` |
| `tests/corpus.rs` | a production bundle, plus `hbcdump` as a second disassembler | `HBC_CORPUS_BUNDLE`, `HBC_CORPUS_LIMIT`, `HERMES_HBCDUMP_V96` |
| `hbc-decomp-cli/tests/stdout_contract.rs` | the process boundary: stdout, stderr, exit codes | none |

```powershell
$env:HERMES_SRC_V99     = 'C:\src\hermes-v99'
$env:HERMES_HBCDUMP_V96 = 'C:\src\hermes-v96\build\bin\Release\hbcdump.exe'
$env:HBC_CORPUS_BUNDLE  = 'C:\path\to\index.android.bundle'
$env:HBC_CORPUS_LIMIT   = '0'   # sweep every function (~9s); default 2000
cargo test
```

`upstream_pin` is the one worth running after any Hermes bump: it re-derives the
modern header layout and the whole opcode table from a checkout and fails if either
disagrees with what this crate ships. Upstream has changed both **without bumping
the bytecode version**, so the version number alone is not a safe signal.

It needs source only, no build, so there is a cheaper way to get its four checkouts
than building VMs — one that also guarantees they are the exact commits the tables
record:

```powershell
python scripts/fetch_pinned_hermes.py C:\src\pins
# v96: 2afc7b09f -> C:\src\pins\hermes-v96 (fetched)   ... and 97, 98, 99
```

Each is a blobless, sparse checkout at that version's `GitCommitHash` — about 4 MB
and a few seconds for all four, against ~1.5 GB for a full clone.

**Requiring the oracles.** An unset variable means "I do not have this oracle" and
the suite skips with a printed note. That is what keeps an unconfigured checkout
testable, but it also means a run can be green while asserting almost nothing.
`HBC_REQUIRE_ORACLES` names the oracles a run refuses to do without:

```powershell
$env:HBC_REQUIRE_ORACLES = 'src'          # every HERMES_SRC_V<N>
$env:HBC_REQUIRE_ORACLES = 'src,vm'       # ...and an hvm per fixture version
$env:HBC_REQUIRE_ORACLES = 'all'          # src, vm, hbcdump, corpus
```

An absent oracle then fails with the variable to set rather than skipping. Two
things are errors regardless of this setting: a variable that is *set* but does not
point at what it claims (a stale path silently degrading to a no-op is the failure
this exists to remove), and an unknown token in the list itself.

CI runs `cargo test --workspace` unconfigured, then fetches the four pinned
checkouts and re-runs `upstream_pin` under `HBC_REQUIRE_ORACLES=src`, so the
bundled format tables cannot drift from the upstream commits they record without
the build going red. `vm_verify` and `corpus` need a Hermes build and a third-party
bundle respectively, so they stay opt-in on a public runner.

`corpus` is the one worth running before trusting a change against a real bundle: it
sweeps every function for encode/decode symmetry and diffs the disassembly against
`hbcdump`. The fixtures contain no overflowed string entries at all; a production
bundle has ~1,400.

Two other binaries from the same build are useful as read-side oracles:
`hbcdump -mode=objdump` (reference disassembly plus a string table with kinds,
byte ranges and identifier hashes) and `hermesc` (minting known-good fixtures).

### Self-update

```bash
hermes-decomp update --check
hermes-decomp update --install
hermes-decomp update --version v0.1.7
```

Optional: `HERMES_DECOMP_UPDATE_CHECK=1` for a one-line notice when a newer release exists.

# 07 — The frontends: CLI and MCP

> **Ownership.** *Owns* the two thin frontend crates that wrap the `hbc-decomp` library:
> `hbc-decomp-cli` (the `hermes-decomp` binary + TUI) and `hbc-decomp-mcp` (the `hermes-mcp`
> MCP server). *Delegates* every library entry point they call to guides
> [`01`](01_READ_LAYER.md)–[`06`](06_WRITE_PATH.md). For end-user command syntax and flags see
> `../USAGE.md` (CLI) and `../MCP.md` (tools); this guide is the structural map of how the
> frontends dispatch into the library. The risks of these surfaces (MCP output bound and panic
> recovery, the CLI stdout/stderr contract) live in `../plan_guides/07_frontends/RISKS.md`.

Files: `crates/hbc-decomp-cli/src/*` (+ `tests/*`), `crates/hbc-decomp-mcp/src/*`.

---

Both crates are thin: they parse input, load a `BytecodeFile`, call a library function, and
format the result. Neither holds decompiler logic.

## CLI — `hermes-decomp` (`hbc-decomp-cli`)

### Command surface (`cli_args.rs::Command`, clap derive)
Every file-reading subcommand flattens one shared `FormatArgs` (`--format-version`,
`--layout`, `--function-layout`; both layouts default to `auto`). `cascade` is the exception: its
two actions take only `--format-version` and always auto-detect the layout. A global `--log
<spec>` (env_logger filter, overrides `RUST_LOG`) turns on the library's `log` tracing.

**Read / analysis** → library:
- `info` → `debug_cmd::print_info` (header banner)
- `versions` → `opcode::available_versions`
- `tui` → `tui::run_tui` (optional second input opens the diff view)
- `disasm` → `disassemble_function` / `disassemble_all` (+ `function_info_banner`)
- `decompile` → `decompile_function_v2[_with_context]`, `decompile_all_v2_with_closures[_cached]`,
  `decompile_filtered_v2[_cached]`, `PipelineContext::build_cached` for `--json` IR, and
  `analyze_module` for the dead-code report (many flags: `--expand`, `--resolve-closures`,
  `--json`, `--assembly`, module filters, `--no-cache`)
- `closures` → `decompile_cmd::print_closure_info`
- `deps` / `modules` → `extract_cmd::print_module_deps` / `print_modules`
- `debug` → `debug_cmd::print_debug_info`
- `extract` → `extract_cmd::run_extract` (one file per Metro module)
- `graphviz` → `graphviz_cmd::run_graphviz` (`IRBuilder` + `propagate` + `ir::generate_dot`)
- `xref` → `xref_cmd::run_xref` (`analysis::find_string_xrefs` / `find_function_refs`, `--json`)
- `bindiff` → `bindiff_cmd::run_bindiff`. `--json` prints a serializable, name-sorted
  `DiffReport`; functions are paired by name, and within a name group identical bodies claim each
  other before the positional fallback (`pair_group`), so one inserted function does not mis-pair
  the tail of the ~28k unnamed functions
- `dump` → `dump_cmd::run_dump` (strings/functions/cjs/regexp/shapes/sections/bigint/array…)
- `cascade extract` / `cascade verify` → `cascade_cmd::run_extract` / `run_verify` (offline
  name-proposal chain over `hbc_decomp::cascade`: extract candidate functions as JSON, then check
  a proposal artifact against the bytecode; never on the decompile path)
- `callgraph` → `callgraph_cmd::run_callgraph`
- `secrets` / `frida-hooks` → `write_cmd::run_secrets` / `run_frida_hooks`

**Write path** (all in `write_cmd`, → guide 06): `emit-hasm`, `asm`/`patch-function` and
`inject-stub` (`--allow-stale-debug-info` guard, R24), `patch-operand`, `retarget-string`,
`add-string`, `patch-string`, `create`, `asm-check`. Plus `update` → `update_cmd::run`.

### Dispatch (`main.rs`)
`run()` is one large `match cli.command` routing each variant to a `commands::*` function.
`main()` itself only spawns a **64 MiB-stack worker thread** (`CLI_STACK_SIZE`) because the
giant match overflows Windows' 1 MiB main stack in debug builds; on `Err` the worker prints
`Error: {e:?}` and exits 1, and a panic exits 101. `run()` then calls `init_logging`,
`configure_thread_pool()` and `update_cmd::auto_check_on_startup()` before dispatching.
`commands/mod.rs` declares eleven modules (`bindiff/callgraph/cascade/debug/decompile/dump/
extract/graphviz/update/write_cmd/xref`); `helpers.rs` provides shared `load_file[_with_bytes]`
(which runs `warn_layout_mismatch` — a stderr warning when a forced `--layout` /
`--function-layout` contradicts the file's declared version, or `--function-layout` is ignored
under `--layout auto`), `load_format`, `write_output`, `warn_diagnostics`, `parse_id_ranges`,
`parse_globs`.

Integration tests live in `tests/`: `stdout_contract.rs` (the stdout/stderr and exit-code
contract, → `../plan_guides/07_frontends/RISKS.md`) and `e2e_corpus.rs` (the built binary
decompiles every generated corpus file in its own process within a time budget, and the output
must parse under node when available).

### TUI (`tui/`, ratatui + crossterm)
`run_tui` sets raw mode / alternate screen and runs `events::run_loop`. Components: `app.rs`
(`App` state + `ViewMode`: Disasm, Decompile, Info, Modules, Cfg, Diff, with tab cycling and
xref state), `events.rs` (key/mouse loop), `ui.rs` (rendering/layout), `content.rs`
(per-function content generation), `modules.rs` (Metro module browser), `diff.rs` +
`gitdiff.rs` (side-by-side bindiff and full-program diff), `background.rs` (drains
diff-worker / pipeline-build channels for async work), `formatting.rs`. A file-only
`debug_log` writes to a temp log (never stdout, which would corrupt the TUI);
`decompile_or_log` / `disasm_or_log` surface errors as visible comments.

### Self-update (`update_cmd.rs`)
Synchronous stack (ureq, no tokio). Queries the GitHub releases API for
`SymbioticSec/hermes-decomp`, picks the platform asset, downloads (256 MiB cap), verifies
SHA-256 against the release `SHA256SUMS`, extracts (tar.gz unix / zip windows), and swaps
atomically via `self_replace` (staging file opened O_EXCL). `auto_check_on_startup` is opt-in
via the `HERMES_DECOMP_UPDATE_CHECK` env var.

## MCP — `hermes-mcp` (`hbc-decomp-mcp`)

### Purpose & transports (`main.rs`)
Exposes the decompiler to AI assistants over MCP via `rmcp`. Two transports: **stdio**
(default — one session on process stdin/stdout, for Claude Desktop/Code, Cursor) and **http**
(Streamable HTTP via axum, `--host`/`--port 8744`/`--path /mcp`, one `HermesService` per
client session). Calls `configure_thread_pool()` before anything touches Rayon.

### Tools (`server/tools_analyze.rs`, `server/tools_write.rs`)
Two `#[tool_router]` groups merged in `HermesService::new` (`analyze_router() +
write_router()`). State is a `Mutex<Option<LoadedFile>>`. Every tool body that goes through
`with_file` / `with_file_mut` runs `catch_tool_panic` inside `run_scoped_with_large_stack` (a
scoped 64 MiB thread — tokio workers' ~2 MB overflows on real bundles); `lock()` recovers from
poisoning; read tools answer through `text_result`, which applies `cap_text` at
`MAX_RESPONSE_BYTES` (256 KiB), and so do the write tools. `load_file` parses outside
`with_file`, on its own `hbc_decomp::run_with_large_stack` thread under `catch_tool_panic`, and
reports any `resolve_format` opcode-table substitution and integrity warnings in its response.
`decompile_all` is the one tool not under `cap_text`: it is filtered (`modules` id ranges,
`module_name` / `exclude_module_name` globs, `from_module` + `module_depth`) and bounded by
`max_chars` (default `MAX_RESPONSE_BYTES`; a caller may ask for more) through
`server/bounds.rs::truncate_at_line`, returning a second JSON block with the truncation summary.

- **Read (21 tools):** `load_file`, `file_info`, `decompile_function`,
  `decompile_function_full` / `decompile_module` / `decompile_all` (full pipeline via
  `ensure_pipeline`), `get_ir_json`, `closures`, `disassemble`, `xref_search`, `list_modules`,
  `module_deps`, `module_exports`, `dump` / `dump_table`, `list_versions`, `dead_code`,
  `debug_info`, `graphviz`, `callgraph`, `function_info` — each calls the matching library
  function named in guides 01–05.
- **Write / RE (7 tools):** `secrets`, `emit_hasm`, `patch_string`, `inject_stub`,
  `patch_function` (`parse_hasm_with_context` + `patch_function_body`), `create_hbc`,
  `frida_hooks` — all → guide 06. These return `CallToolResult` directly, outside `cap_text`.
  The CLI's `patch-operand`, `retarget-string` and `add-string` have no MCP counterpart.

### Server structure (`server/`)
`server/mod.rs` holds `HermesService`, `LoadedFile` (file + format + path + bytes + memoized
`pipeline_ctx`, with `pipeline_deep` recording which mode it was built in), the
lock/panic/stack/cap helpers, and the `ServerHandler` impl (`get_info` sets instructions + tool
capabilities). `params.rs` holds all `Parameters<…>` structs; `bounds.rs` holds the
`decompile_all` filter parsers and `truncate_at_line`; `tools_analyze.rs` / `tools_write.rs`
each define one router; `main.rs` wires the transports.

## File map

| CLI | Role | MCP | Role |
|---|---|---|---|
| `main.rs` | worker-thread bootstrap + dispatch match | `main.rs` | transport (stdio/http) setup |
| `cli_args.rs` | clap `Command` surface + `FormatArgs` | `server/mod.rs` | `HermesService`, `LoadedFile`, hardening |
| `helpers.rs` | load/format/output helpers, layout warnings | `server/params.rs` | tool param structs |
| `commands/*_cmd.rs` | per-command logic | `server/bounds.rs` | `decompile_all` filters + `max_chars` cut |
| `tui/*` | ratatui interactive UI | `server/tools_analyze.rs` | 21 read tools |
| `tests/*.rs` | stdout contract, corpus e2e | `server/tools_write.rs` | 7 write/RE tools |

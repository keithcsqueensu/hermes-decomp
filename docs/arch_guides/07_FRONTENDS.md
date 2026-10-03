# 07 — The frontends: CLI and MCP

> **Ownership.** *Owns* the two thin frontend crates that wrap the `hbc-decomp` library:
> `hbc-decomp-cli` (the `hermes-decomp` binary + TUI) and `hbc-decomp-mcp` (the `hermes-mcp`
> MCP server). *Delegates* every library entry point they call to guides
> [`01`](01_READ_LAYER.md)–[`06`](06_WRITE_PATH.md). For end-user command syntax and flags see
> `../USAGE.md`; this guide is the structural map of how the frontends dispatch into the
> library.

Files: `crates/hbc-decomp-cli/src/*`, `crates/hbc-decomp-mcp/src/*`.

---

Both crates are thin: they parse input, load a `BytecodeFile`, call a library function, and
format the result. Neither holds decompiler logic.

## CLI — `hermes-decomp` (`hbc-decomp-cli`)

### Command surface (`cli_args.rs::Command`, clap derive)
All read commands share `--layout` / `--function-layout` (auto-detected header layout) and
most take `--format-version`.

**Read / analysis** → library:
- `info` → `debug_cmd::print_info` (header banner)
- `versions` → `opcode::available_versions`
- `disasm` → `disassemble_function` / `disassemble_all` (+ `function_info_banner`)
- `decompile` → `decompile_function_v2`, `decompile_all_v2_with_closures[_cached]`,
  `decompile_filtered_v2[_cached]`, `analyze_module` (many flags: `--expand`,
  `--resolve-closures`, `--json`, `--assembly`, module filters, `--no-cache`)
- `closures` → `decompile_cmd::print_closure_info`
- `deps` / `modules` → `extract_cmd::print_module_deps` / `print_modules`
- `debug` → `debug_cmd::print_debug_info`
- `extract` → `extract_cmd::run_extract` (one file per Metro module)
- `graphviz` → `IRBuilder` + `ir::generate_dot`
- `xref` → `analysis::find_string_xrefs` / `find_function_refs`
- `bindiff` → `bindiff_cmd::run_bindiff`
- `dump` → `dump_cmd::run_dump` (strings/functions/cjs/regexp/shapes/sections/bigint/array…)
- `callgraph` → `callgraph_cmd::run_callgraph`
- `secrets` / `frida-hooks` → `write_cmd::run_secrets` / `run_frida_hooks`

**Write path** (all in `write_cmd`, → guide 06): `emit-hasm`, `asm`/`patch-function`
(`--allow-stale-debug-info` guard, R24), `patch-operand`, `retarget-string`, `add-string`,
`patch-string`, `inject-stub`, `create`, `asm-check`. Plus `update` → `update_cmd::run`.

### Dispatch (`main.rs`)
`run()` is one large `match cli.command` routing each variant to a `commands::*` function.
`main()` itself spawns a **64 MiB-stack worker thread** (`CLI_STACK_SIZE`) because the giant
match overflows Windows' 1 MiB main stack in debug builds; it also calls
`configure_thread_pool()` and `update_cmd::auto_check_on_startup()`. `commands/mod.rs`
declares eight modules (`bindiff/callgraph/debug/decompile/dump/extract/update/write_cmd`);
`helpers.rs` provides shared `load_file`, `load_format`, `write_output`, `warn_diagnostics`,
`parse_id_ranges`, `parse_globs`.

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
write_router()`). State is a `Mutex<Option<LoadedFile>>`; a poison-recovering `lock()`, a
`catch_tool_panic` wrapper, and a 256 KiB `cap_text` response cap harden the read path.

- **Read (~21 tools):** `load_file`, `file_info`, `decompile_function`,
  `decompile_function_full` / `decompile_module` / `decompile_all` (full pipeline via
  `ensure_pipeline`), `get_ir_json`, `closures`, `disassemble`, `xref_search`, `list_modules`,
  `module_deps`, `module_exports`, `dump` / `dump_table`, `list_versions`, `dead_code`,
  `debug_info`, `graphviz`, `callgraph`, `function_info` — each calls the matching library
  function named in guides 01–05.
- **Write / RE (7 tools):** `secrets`, `emit_hasm`, `patch_string`, `inject_stub`,
  `patch_function` (`parse_hasm_with_context` + `patch_function_body`), `create_hbc`,
  `frida_hooks` — all → guide 06.

### Server structure (`server/`)
`server/mod.rs` holds `HermesService`, `LoadedFile` (file + format + bytes + memoized
`pipeline_ctx`), the lock/panic/cap helpers, and the `ServerHandler` impl (`get_info` sets
instructions + tool capabilities). `params.rs` holds all `Parameters<…>` structs;
`tools_analyze.rs` / `tools_write.rs` each define one router; `main.rs` wires the transports.

## File map

| CLI | Role | MCP | Role |
|---|---|---|---|
| `main.rs` | worker-thread bootstrap + dispatch match | `main.rs` | transport (stdio/http) setup |
| `cli_args.rs` | clap `Command` surface | `server/mod.rs` | `HermesService`, `LoadedFile`, hardening |
| `helpers.rs` | load/format/output helpers | `server/params.rs` | tool param structs |
| `commands/*_cmd.rs` | per-command logic | `server/tools_analyze.rs` | ~21 read tools |
| `tui/*` | ratatui interactive UI | `server/tools_write.rs` | 7 write/RE tools |

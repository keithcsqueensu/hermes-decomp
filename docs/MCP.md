# MCP server

`hermes-mcp` exposes decompiler features as MCP tools for AI assistants
(Claude, Cursor, GPT, etc.).

## Build

```bash
cargo build --release -p hbc-decomp-mcp
# binary: target/release/hermes-mcp
```

## Config (stdio)

Template: [`mcp-config.example.json`](../mcp-config.example.json)

```json
{
  "mcpServers": {
    "hermes-decompiler": {
      "command": "/path/to/target/release/hermes-mcp"
    }
  }
}
```

## HTTP transport

```bash
hermes-mcp                                  # stdio (default)
hermes-mcp --transport http                 # 127.0.0.1:8744/mcp
hermes-mcp --transport http --host 0.0.0.0 --port 9000 --path /mcp
```

```json
{ "mcpServers": { "hermes-decompiler": { "url": "http://127.0.0.1:8744/mcp" } } }
```

Each HTTP connection gets its own isolated session.

## Tools

### Read / analyze

| Tool | Description |
|------|-------------|
| `load_file` | Load a `.hbc` file (must be called first). A failed load leaves the previous file loaded |
| `file_info` | File header info (version, counts) |
| `decompile_function` | Decompile one function (fast, light mode) |
| `decompile_function_full` | Full pipeline (IPA, closures, ESM); `deep` for deep naming |
| `decompile_all` | Whole bundle, grouped by Metro module; filtered and capped (see below) |
| `decompile_module` | One Metro module as ESM |
| `get_ir_json` | Structured JSON IR |
| `disassemble` | Raw bytecode disassembly |
| `xref_search` | Cross-references to a string or function id; pages with `limit` / `offset` |
| `list_modules` | Metro modules (names + export counts) |
| `module_deps` | Module dependency tree |
| `module_exports` | Export names → function IDs |
| `callgraph` | Call graph (text or DOT), optionally from one function to a depth |
| `function_info` | Per-function metadata banner |
| `closures` | Closure slot mappings |
| `debug_info` | Variable names, scopes, callees |
| `dead_code` | Unreachable from Metro roots |
| `graphviz` | CFG of a function (DOT) |
| `dump` | Strings, function headers or identifiers; pages with `limit` / `offset` |
| `dump_table` | Structural tables: `cjs-modules`, `regexp`, `obj-shapes`, `function-sources`, `string-kinds`, `sections`, `big-int`, `array-buffer`; `json` for machine output |
| `list_versions` | Supported HBC versions (40-99) |

### Secrets, hooks and the write path

These write a new file to `output_path` / `output_dir`; the loaded file is never modified.
HASM is the disasm dialect described in [USAGE.md](USAGE.md#bytecode-write-path-not-js-recompilation).

| Tool | Description |
|------|-------------|
| `secrets` | Scan the string table for likely secrets (AWS keys, JWTs, tokens, URLs, private keys); `show_full` to unmask |
| `emit_hasm` | HASM for one function; round-trips with `patch_function` |
| `patch_string` | Patch a string entry by `id` or `old_value`, any length. In place only when the entry has exclusive storage and the same byte length; otherwise the table is rebuilt (Hermes packs overlapping strings). Legacy and modern (v97+) |
| `inject_stub` | `kind = "nop"` or `"log"` (prints the function name on entry) at a function's entry |
| `patch_function` | Assemble a body from HASM. Same size patches in place; growing or shrinking works on legacy and modern files |
| `create_hbc` | Minimal valid `.hbc` from scratch (legacy layout ≤ v96, modern ≥ v97) |
| `frida_hooks` | Frida hooks for a Metro module's exports → `before.js` / `after.js` / `agent.js` / `run.sh` |

The CLI's `add-string`, `retarget-string`, `patch-operand` and `cascade` have no MCP tool.

## Response limits and failures

- **Every tool's text is capped at 256 KiB.** Past that it is cut at a line boundary and
  ends with a `TRUNCATED` note giving the full size and the lines shown — never silently.
  Narrow the request (one function or module, or `limit` / `offset` where the tool takes
  them) rather than treating a capped answer as complete. The write tools answer through
  the same cap.
- **`decompile_all` is filtered and bounded.** Select modules with `modules` (id ranges,
  e.g. `"100-150,200"`), `module_name` / `exclude_module_name` (comma-separated globs) or
  `from_module` + `module_depth` (a dependency subtree). `max_chars` defaults to the same
  256 KiB cap; a caller can ask for more explicitly. Output above it is cut at a line
  boundary and a second block reports `truncated`, `total_chars` and `kept_chars`. Without
  a filter, orphan functions are included.
- **A panic inside a tool fails that one call**, not the server: every tool body, and
  `load_file`'s parse, runs under a panic guard on a large stack.

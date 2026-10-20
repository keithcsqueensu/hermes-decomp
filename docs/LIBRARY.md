# Library API (`hbc-decomp`)

## Cargo

```toml
[dependencies]
hbc-decomp = { git = "https://github.com/SymbioticSec/hermes-decomp" }
```

## Example

```rust
use hbc_decomp::{Decompiler, DecompileOptionsV2};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read("app.hbc")?;
    let mut decompiler = Decompiler::new(&bytes)?;
    decompiler.build_closure_context()?;

    let options = DecompileOptionsV2::optimized();
    let code = decompiler.decompile_all(&options)?;
    println!("{code}");

    let ir = decompiler.decompile_to_ir(0, &options)?;
    let _ = ir;
    Ok(())
}
```

## `DecompileOptionsV2`

| Option | `optimized()` | Description |
|--------|---------|-------------|
| `resolve_strings` | `true` | Replace string IDs with literals |
| `include_offsets` | `false` | Bytecode offset comments |
| `propagate` | `true` | Constant / copy propagation |
| `simplify` | `true` | Drop intermediate temporaries |
| `recover_structures` | `true` | Reconstruct `if` / `while` / `for` |
| `assembly_mode` | `false` | Absolute binary offsets on each line |
| `deep` | `false` | Iterate naming to a fixed point, grounded in the bytecode data flow. Slower; recovers more real names |
| `stable` | `false` | Name otherwise-unnamed modules from a hash of their content, not the volatile Metro id, so names survive across builds |
| `cascade` | `None` | Path to a proposal artifact; names the bytecode confirms are applied (see `hbc_decomp::cascade`) |

Presets: `optimized()` above; `debug()` turns offsets on and propagation / simplification
off. `Default::default()` is all-`false`, which is **not** `optimized()`.

## Analysis cache

`PipelineContext::build_cached(file, format, options, bytes, cache_path)` builds the
whole-program context through an on-disk snapshot (`default_cache_path(input)` gives
`<input>.hdcache`), keyed by the bytecode, the binary and every option. Any cache failure
falls back to a normal build. When `options.cascade` is set it never reads or writes the
cache — the key could only see the artifact's path.

## Pipeline (overview)

1. **Parse** HBC headers, strings, bytecode
2. **Disassemble** → opcodes
3. **IR** + CFG (registers → variables, jumps → graph)
4. **Analysis**, data-flow, structure recovery, Metro/ESM, naming/IPA
5. **Codegen** → JavaScript

The stage-by-stage contract (W/F stage IDs, in run order) is
`crates/hbc-decomp/src/pipeline/stages.rs`; the guides under
[`arch_guides/`](arch_guides/README.md) describe each layer.

## Other modules

- **`write`** — encode / HASM / patch / serialize: `add_string`, `retarget_string`,
  `patch_string_operand`, `patch_string_by_id` / `patch_string_replace`,
  `patch_function_body`, `assemble_function_hasm`, `inject_stub`, `create_minimal`,
  `set_regexp` / `regexp_payload` / `repoint_regexp`, `relocate`, `serialize_file`, `verify_footer`. **Bytecode only**, not JS → hermesc.
- **`cascade`** — offline name proposals: `candidates::extract` picks functions worth
  naming, `verify::verify` checks a proposal artifact against the bytecode, `apply`
  renames the confirmed ones. Artifacts are bound to one bundle by a fingerprint.
- **`inspect`** — `dump_table` / `dump_table_json`, `function_info_banner`,
  `render_call_graph`.
- **`secrets`**, **`frida_hooks`** — the same scans and hook generation the CLI exposes.

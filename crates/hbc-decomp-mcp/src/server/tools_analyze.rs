// Read and analysis MCP tools (decompile, disasm, xref, modules, dump, ...).

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData as McpError};

use hbc_decomp::{
    BytecodeFile, ClosureInfo, DecompileOptionsV2, IRBuilder, IRBuilderOptions,
};

use super::params::*;
use super::{text_result, HermesService, LoadedFile};

#[tool_router(router = analyze_router, vis = "pub(crate)")]
impl HermesService {
    #[tool(
        description = "Load a Hermes bytecode (.hbc) file for analysis. Must be called before any other tool."
    )]
    fn load_file(
        &self,
        Parameters(params): Parameters<LoadFileParams>,
    ) -> Result<CallToolResult, McpError> {
        let bytes = std::fs::read(&params.path)
            .map_err(|e| McpError::internal_error(format!("Failed to read file: {e}"), None))?;
        let mut file = BytecodeFile::parse_auto(&bytes)
            .map_err(|e| McpError::internal_error(format!("Failed to parse HBC: {e}"), None))?;
        // `resolve_format` records a diagnostic when a *different* version's
        // opcode table is substituted. This used to be `let (format, _)`, so an
        // agent reading this response had no way to know its decode came from the
        // wrong table -- which does not fail, it just yields correct-looking
        // JavaScript with the wrong instructions in it.
        let format = file
            .resolve_format()
            .map_err(|e| McpError::internal_error(format!("Unsupported version: {e}"), None))?;

        let mut info = format!(
            "Loaded: {}\nVersion: {}\nFunctions: {}\nStrings: {}\nDebug info: {}",
            params.path,
            file.header.version,
            file.header.function_count,
            file.header.string_count,
            file.debug_info_status.describe(),
        );
        // Integrity and degradation, up front. Silence here is what let a
        // hand-patched bundle with a stale SHA-1 footer, or a file decoded at the
        // wrong stride, read as a clean load.
        let warnings = file.warnings();
        if warnings.is_empty() {
            info.push_str("\nIntegrity: OK (footer and file_length both match)");
        } else {
            info.push_str("\n\n!! This read is degraded:");
            for w in &warnings {
                info.push_str(&format!("\n  - {w}"));
            }
        }

        let mut guard = self.lock();
        *guard = Some(LoadedFile {
            file,
            format,
            path: params.path,
            bytes,
            pipeline_ctx: None,
        });
        text_result(info)
    }

    #[tool(description = "Get file header info: version, function count, string count, file path.")]
    fn file_info(&self) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let file = &loaded.file;
            let info = format!(
                "Path: {}\nVersion: {}\nFunctions: {}\nStrings: {}\nGlobal code: function 0",
                loaded.path,
                file.header.version,
                file.header.function_count,
                file.header.string_count,
            );
            text_result(info)
        })
    }

    #[tool(
        description = "Decompile a function to JavaScript in light mode, fast and for one function. For full quality with IPA naming, closures and ESM, use decompile_function_full."
    )]
    fn decompile_function(
        &self,
        Parameters(params): Parameters<DecompileFunctionParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let opts = DecompileOptionsV2 {
                resolve_strings: true,
                include_offsets: params.show_offsets || params.assembly,
                propagate: params.propagate,
                simplify: params.simplify,
                recover_structures: params.recover_structures,
                assembly_mode: params.assembly,
            };
            let code = if params.resolve_closures {
                let closure_ctx =
                    hbc_decomp::build_closure_context(&loaded.file, &loaded.format)
                        .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
                hbc_decomp::decompile_function_v2_with_context(
                    &loaded.file,
                    &loaded.format,
                    params.function_id,
                    &opts,
                    Some(&closure_ctx),
                )
                .map_err(|e| McpError::internal_error(format!("{e}"), None))?
            } else {
                hbc_decomp::decompile_function_v2(
                    &loaded.file,
                    &loaded.format,
                    params.function_id,
                    &opts,
                )
                .map_err(|e| McpError::internal_error(format!("{e}"), None))?
            };
            text_result(code)
        })
    }

    #[tool(
        description = "Decompile a function with the full pipeline (IPA naming, closures, ESM imports/exports, async detection). Higher quality but builds the full pipeline on first use."
    )]
    fn decompile_function_full(
        &self,
        Parameters(params): Parameters<FunctionIdParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file_mut(|loaded| {
            loaded.ensure_pipeline()?;
            let pipeline = loaded.pipeline_ctx.as_ref().unwrap();
            let code = pipeline.generate_function_code(&loaded.file, params.function_id);
            text_result(code)
        })
    }

    #[tool(
        description = "Decompile all functions with full pipeline (IPA, closures, ESM). Groups output by Metro module. May take several seconds for large bundles."
    )]
    fn decompile_all(&self) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            // Refuse rather than truncate. On a real bundle this produces ~41 MB
            // of JavaScript from 62,909 functions; capping that at 256 KiB would
            // hand back 0.6% of the answer while looking like it worked. Point at
            // the tools that can actually answer the question instead.
            const MAX_FUNCTIONS_FOR_WHOLE_BUNDLE: u32 = 2_000;
            let n = loaded.file.header.function_count;
            if n > MAX_FUNCTIONS_FOR_WHOLE_BUNDLE {
                return Err(McpError::invalid_params(
                    format!(
                        "this bundle has {n} functions; decompiling all of them produces tens \
                         of megabytes of output, far beyond what one response can carry. Use \
                         `list_modules` to find the module you want, then `decompile_module`, \
                         or `decompile_function_full` for a single function."
                    ),
                    None,
                ));
            }
            let opts = DecompileOptionsV2::optimized();
            let code = hbc_decomp::decompile_all_v2_with_closures(
                &loaded.file,
                &loaded.format,
                &opts,
            )
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
            text_result(code)
        })
    }

    #[tool(description = "Get structured JSON IR of a function. Useful for programmatic analysis.")]
    fn get_ir_json(
        &self,
        Parameters(params): Parameters<FunctionIdParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let opts = DecompileOptionsV2::optimized();
            let ir = hbc_decomp::generate_ir(
                &loaded.file,
                &loaded.format,
                params.function_id,
                &opts,
                None,
                true,
            )
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
            let json = serde_json::to_string_pretty(&ir)
                .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
            text_result(json)
        })
    }

    #[tool(description = "Disassemble a function to raw Hermes bytecode instructions.")]
    fn disassemble(
        &self,
        Parameters(params): Parameters<DisasmParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let options = hbc_decomp::DisasmOptions {
                show_offsets: params.show_offsets,
                show_labels: true,
                resolve_strings: true,
                enable_color: false,
            };
            let asm = hbc_decomp::disassemble_function(
                &loaded.file,
                &loaded.format,
                params.function_id,
                &options,
            )
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
            text_result(asm)
        })
    }

    #[tool(description = "Search for cross references to a string or function ID in the bytecode.")]
    fn xref_search(
        &self,
        Parameters(params): Parameters<XrefParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let file = &loaded.file;
            let format = &loaded.format;
            let results = if params.kind == "function" {
                let fid = params
                    .query
                    .parse::<u32>()
                    .map_err(|_| McpError::invalid_params("Invalid function ID", None))?;
                hbc_decomp::analysis::find_function_refs(file, format, fid)
            } else {
                hbc_decomp::analysis::find_string_xrefs(file, format, &params.query)
            };

            // A hot string in a large bundle can have thousands of xrefs; take a
            // window and state it, rather than returning however many there are.
            let start = params.offset.min(results.len());
            let end = params
                .limit
                .map(|l| start.saturating_add(l).min(results.len()))
                .unwrap_or(results.len());
            let mut output = format!(
                "Found {} cross-references for '{}' (showing {start}..{end}):\n",
                results.len(),
                params.query
            );
            for xref in &results[start..end] {
                // A corrupt or mis-parsed header table can carry a function id past
                // the end of it; index defensively rather than panicking inside a
                // long-lived server.
                let name = file
                    .function_headers
                    .get(xref.function_id as usize)
                    .and_then(|h| file.string_at(h.function_name()))
                    .map(|e| e.value.as_str())
                    .unwrap_or("<anonymous>");
                output.push_str(&format!(
                    "  Function {} ({}) at offset {:04x}: {}\n",
                    xref.function_id, name, xref.offset, xref.opcode
                ));
            }
            text_result(output)
        })
    }

    #[tool(description = "List all Metro modules in the React Native bundle.")]
    fn list_modules(
        &self,
        Parameters(params): Parameters<ListModulesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file_mut(|loaded| {
            // Use the full pipeline so module names and exports are populated
            // (the lightweight registry only runs detection, no naming/exports).
            loaded.ensure_pipeline()?;
            let registry = &loaded.pipeline_ctx.as_ref().unwrap().registry;
            let mut modules: Vec<_> = registry.modules.values().collect();
            modules.sort_by_key(|m| m.module_id);
            let limit = params.limit.unwrap_or(modules.len()).min(modules.len());

            let mut output = format!("Found {} Metro modules:\n", modules.len());
            for m in modules.iter().take(limit) {
                let name_str = m
                    .name
                    .as_deref()
                    .map(|n| format!(" - {n}"))
                    .unwrap_or_default();
                let export_count = m.exports.len();
                output.push_str(&format!(
                    "  Module {} (F{}){} deps: {:?} exports: {}\n",
                    m.module_id, m.function_id, name_str, m.dependencies, export_count
                ));
            }
            text_result(output)
        })
    }

    #[tool(description = "Show dependency tree for a Metro module.")]
    fn module_deps(
        &self,
        Parameters(params): Parameters<ModuleDepsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file_mut(|loaded| {
            // Full pipeline so the tree carries real module names.
            loaded.ensure_pipeline()?;
            let registry = &loaded.pipeline_ctx.as_ref().unwrap().registry;
            let tree = registry.get_dependency_tree(params.module_id, params.depth);
            text_result(tree.format(0))
        })
    }

    #[tool(
        description = "Dump strings, function headers, or identifiers from the HBC file. Useful for finding API keys, endpoints, secrets."
    )]
    fn dump(&self, Parameters(params): Parameters<DumpParams>) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let file = &loaded.file;
            let mut output = String::new();

            // Paging. `dump` walks whole tables -- 98,917 strings and 62,909
            // function headers on a real bundle, 5.8 MB and 3.7 MB of text -- so
            // it takes a window and says what the window was.
            let window = |total: usize, out: &mut String, label: &str| -> (usize, usize) {
                let start = params.offset.min(total);
                let end = params
                    .limit
                    .map(|l| start.saturating_add(l).min(total))
                    .unwrap_or(total);
                out.push_str(&format!(
                    "{label}: showing {}..{} of {total}
",
                    start, end
                ));
                (start, end)
            };

            let fn_line = |i: usize, out: &mut String| {
                let fh = &file.function_headers[i];
                let name = file
                    .string_at(fh.function_name())
                    .map(|e| e.value.clone())
                    .unwrap_or_default();
                out.push_str(&format!(
                    "Function {}: name=\"{}\" params={} regs={} size={}
",
                    i,
                    name,
                    fh.param_count(),
                    fh.frame_size(),
                    fh.bytecode_size_in_bytes()
                ));
            };

            let string_line = |i: usize, out: &mut String| {
                if let Some(s) = file.string_at(i as u32) {
                    out.push_str(&format!("{}: {}
", i, s.value));
                }
            };

            match params.kind.as_str() {
                "functions" => {
                    let (a, b) = window(file.function_headers.len(), &mut output, "functions");
                    for i in a..b {
                        fn_line(i, &mut output);
                    }
                }
                "identifiers" => {
                    let (a, b) = window(file.identifier_hashes.len(), &mut output, "identifiers");
                    for i in a..b {
                        output.push_str(&format!(
                            "Identifier {i}: hash=0x{:08x}
",
                            file.identifier_hashes[i]
                        ));
                    }
                    if file.identifier_hashes.is_empty() {
                        output.push_str("No identifier hash table found.
");
                    }
                }
                "all" => {
                    output.push_str("=== strings ===
");
                    let (a, b) = window(file.header.string_count as usize, &mut output, "strings");
                    for i in a..b {
                        string_line(i, &mut output);
                    }
                    output.push_str("
=== functions ===
");
                    let (a, b) = window(file.function_headers.len(), &mut output, "functions");
                    for i in a..b {
                        fn_line(i, &mut output);
                    }
                }
                _ => {
                    // Default: strings
                    let (a, b) = window(file.header.string_count as usize, &mut output, "strings");
                    for i in a..b {
                        string_line(i, &mut output);
                    }
                }
            }
            text_result(output)
        })
    }

    #[tool(description = "List supported Hermes bytecode versions (HBC 40 to 99).")]
    fn list_versions(&self) -> Result<CallToolResult, McpError> {
        let versions = hbc_decomp::opcode::available_versions();
        let list = versions
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        text_result(format!(
            "Supported Hermes bytecode versions: HBC 40-99.\nAvailable opcode tables: {list}"
        ))
    }

    // --- New tools ---

    #[tool(
        description = "Analyze closure variable slots for a function. Shows what parent variables are captured via the environment chain."
    )]
    fn closures(
        &self,
        Parameters(params): Parameters<FunctionIdParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let opts = DecompileOptionsV2::optimized();
            let stmts = hbc_decomp::generate_ir(
                &loaded.file,
                &loaded.format,
                params.function_id,
                &opts,
                None,
                true,
            )
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;

            let info = ClosureInfo::analyze(&stmts);
            if info.slots.is_empty() {
                return text_result(format!(
                    "Function {} has no closure variable slots.",
                    params.function_id
                ));
            }

            let mut output = format!(
                "Function {} closure slots ({} total):\n",
                params.function_id,
                info.slots.len()
            );
            for (slot, value) in &info.slots {
                let desc = match value {
                    hbc_decomp::ClosureSlotValue::Function { id, name } => {
                        let name_str = name
                            .as_deref()
                            .map(|n| format!(" name=\"{n}\""))
                            .unwrap_or_default();
                        format!("Function(id={id}{name_str})")
                    }
                    hbc_decomp::ClosureSlotValue::Constant(s) => format!("Constant(\"{s}\")"),
                    hbc_decomp::ClosureSlotValue::RegExp => "RegExp".to_string(),
                    hbc_decomp::ClosureSlotValue::Variable(s) => format!("Variable(\"{s}\")"),
                    hbc_decomp::ClosureSlotValue::Unknown => "Unknown".to_string(),
                };
                output.push_str(&format!("  slot {slot}: {desc}\n"));
            }
            text_result(output)
        })
    }

    #[tool(
        description = "Analyze dead code in the bundle. Returns unreachable functions not called from any Metro module entry point."
    )]
    fn dead_code(&self) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let analysis = hbc_decomp::analyze_module(&loaded.file, &loaded.format)
                .map_err(|e| McpError::internal_error(format!("{e}"), None))?;

            let total = loaded.file.header.function_count;
            let dead_count = analysis.dead_code.len();
            let pct = if total > 0 {
                (dead_count as f64 / total as f64) * 100.0
            } else {
                0.0
            };

            let mut output = format!(
                "Dead code analysis: {dead_count} dead functions out of {total} total ({pct:.1}%)\n\n"
            );

            if dead_count > 0 {
                let mut dead_ids: Vec<_> = analysis.dead_code.iter().copied().collect();
                dead_ids.sort();
                for &fid in dead_ids.iter().take(200) {
                    let name = loaded
                        .file
                        .function_headers
                        .get(fid as usize)
                        .and_then(|h| loaded.file.string_at(h.function_name()))
                        .map(|e| e.value.as_str())
                        .unwrap_or("<anonymous>");
                    output.push_str(&format!("  Function {fid} ({name})\n"));
                }
                if dead_count > 200 {
                    output.push_str(&format!("  ... and {} more\n", dead_count - 200));
                }
            }
            text_result(output)
        })
    }

    #[tool(
        description = "Show debug info (source locations, variable names, scope chain) for a function, if available in the bytecode."
    )]
    fn debug_info(
        &self,
        Parameters(params): Parameters<FunctionIdParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            // Say *which* kind of "no debug info" this is. The old form tested
            // only `offset == 0 || offset == NO_OFFSET` and reported everything
            // else as absence -- so "this file was built without -g", "this
            // bytecode version has no modelled debug layout" (v97, and anything
            // below v96) and "the section points past EOF" were one answer.
            let status = loaded.file.debug_info_status;
            if status != hbc_decomp::DebugInfoStatus::Present {
                return text_result(format!(
                    "No debug info available: {}.",
                    status.describe()
                ));
            }

            // The file's own parse, which carries the per-function DebugOffsets
            // index the location streams are addressed through.
            let debug = loaded.file.debug_info.clone().ok_or_else(|| {
                McpError::internal_error("Debug parse error: no debug info", None)
            })?;

            let mut output = String::new();

            // Source locations for this function
            if let Some(locations) = debug.source_locations.get(&params.function_id) {
                output.push_str(&format!(
                    "Source locations for function {} ({} entries):\n",
                    params.function_id,
                    locations.len()
                ));
                for loc in locations {
                    output.push_str(&format!(
                        "  offset {:04x}: line {} col {}",
                        loc.bytecode_offset, loc.line, loc.column
                    ));
                    if let Some(scope) = loc.scope_offset {
                        output.push_str(&format!(" scope={scope}"));
                    }
                    output.push('\n');
                }
            } else {
                output.push_str(&format!(
                    "No source locations for function {}.\n",
                    params.function_id
                ));
            }

            // Scope descriptors
            if !debug.scope_descriptors.is_empty() {
                output.push_str(&format!(
                    "\nScope descriptors ({} total):\n",
                    debug.scope_descriptors.len()
                ));
                for scope in &debug.scope_descriptors {
                    output.push_str(&format!("  offset {}: ", scope.offset));
                    if let Some(parent) = scope.parent_offset {
                        output.push_str(&format!("parent={parent} "));
                    }
                    if !scope.names.is_empty() {
                        output.push_str(&format!("vars=[{}]", scope.names.join(", ")));
                    }
                    output.push('\n');
                }
            }

            if output.is_empty() {
                output.push_str("Debug info section exists but contains no data for this function.");
            }

            text_result(output)
        })
    }

    #[tool(
        description = "Generate Graphviz DOT representation of a function's control flow graph. Paste output into graphviz.org to visualize."
    )]
    fn graphviz(
        &self,
        Parameters(params): Parameters<FunctionIdParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let options = IRBuilderOptions {
                resolve_strings: true,
                include_offsets: false,
                ..Default::default()
            };
            let mut builder = IRBuilder::new(&loaded.file, &loaded.format, options);
            let cfg = builder
                .build_function(params.function_id)
                .map_err(|e| McpError::internal_error(format!("IR build error: {e}"), None))?;

            let function_name = loaded
                .file
                .function_headers
                .get(params.function_id as usize)
                .and_then(|h| loaded.file.string_at(h.function_name()))
                .map(|e| e.value.clone())
                .unwrap_or_else(|| format!("f{}", params.function_id));

            let dot = hbc_decomp::ir::generate_dot(&cfg, &function_name);
            text_result(dot)
        })
    }

    #[tool(
        description = "Decompile a Metro module with full analysis (IPA, closures, ESM imports/exports). Builds the full pipeline on first use."
    )]
    fn decompile_module(
        &self,
        Parameters(params): Parameters<DecompileModuleParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file_mut(|loaded| {
            loaded.ensure_pipeline()?;
            let pipeline = loaded.pipeline_ctx.as_ref().unwrap();
            let module = pipeline
                .registry
                .modules
                .get(&params.module_id)
                .ok_or_else(|| {
                    McpError::invalid_params(
                        format!("Module {} not found", params.module_id),
                        None,
                    )
                })?;
            let function_id = module.function_id;
            let code = pipeline.generate_function_code(&loaded.file, function_id);
            text_result(code)
        })
    }

    #[tool(description = "List exports of a Metro module (exported names and their function IDs).")]
    fn module_exports(
        &self,
        Parameters(params): Parameters<ModuleExportsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file_mut(|loaded| {
            // Use the full pipeline for exports (basic registry may have empty exports
            // since export analysis needs full IR)
            loaded.ensure_pipeline()?;
            let pipeline = loaded.pipeline_ctx.as_ref().unwrap();
            let module = pipeline
                .registry
                .modules
                .get(&params.module_id)
                .ok_or_else(|| {
                    McpError::invalid_params(
                        format!("Module {} not found", params.module_id),
                        None,
                    )
                })?;

            let name_str = module
                .name
                .as_deref()
                .map(|n| format!(" ({n})"))
                .unwrap_or_default();

            if module.exports.is_empty() {
                return text_result(format!(
                    "Module {}{} has no detected exports.",
                    params.module_id, name_str
                ));
            }

            let mut output = format!(
                "Module {}{} exports ({} total):\n",
                params.module_id,
                name_str,
                module.exports.len()
            );
            let mut exports: Vec<_> = module.exports.iter().collect();
            exports.sort_by_key(|(name, _)| (*name).clone());
            for (name, &func_id) in &exports {
                let func_name = loaded
                    .file
                    .function_headers
                    .get(func_id as usize)
                    .and_then(|h| loaded.file.string_at(h.function_name()))
                    .map(|e| e.value.as_str())
                    .unwrap_or("<anonymous>");
                output.push_str(&format!(
                    "  export \"{name}\" -> function {func_id} ({func_name})\n"
                ));
            }
            text_result(output)
        })
    }

    #[tool(
        description = "Dump a structural table from the HBC file. Table kinds are cjs-modules, regexp, obj-shapes, function-sources, string-kinds, sections, big-int, array-buffer. Set json=true for machine readable output."
    )]
    fn dump_table(
        &self,
        Parameters(params): Parameters<DumpTableParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let kind = hbc_decomp::TableKind::parse(&params.kind).ok_or_else(|| {
                McpError::invalid_params(
                    format!(
                        "Unknown table kind '{}'. Valid: cjs-modules, regexp, obj-shapes, function-sources, string-kinds, sections, big-int, array-buffer",
                        params.kind
                    ),
                    None,
                )
            })?;
            let text = if params.json {
                let value = hbc_decomp::dump_table_json(&loaded.file, kind);
                serde_json::to_string_pretty(&value)
                    .map_err(|e| McpError::internal_error(format!("{e}"), None))?
            } else {
                hbc_decomp::dump_table(&loaded.file, kind)
            };
            text_result(text)
        })
    }

    #[tool(
        description = "Build the bundle call graph as caller to callee edges. Optionally restrict to the subgraph reachable from a function up to a depth, or emit Graphviz DOT."
    )]
    fn callgraph(
        &self,
        Parameters(params): Parameters<CallgraphParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let output = hbc_decomp::render_call_graph(
                &loaded.file,
                &loaded.format,
                params.function_id,
                params.depth,
                params.dot,
            )
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
            text_result(output)
        })
    }

    #[tool(
        description = "Get a one line metadata banner for a function. It reports id, name, param count, frame size, register counts, bytecode size, offset, flags and exception handler count."
    )]
    fn function_info(
        &self,
        Parameters(params): Parameters<FunctionIdParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_file(|loaded| {
            let banner = hbc_decomp::function_info_banner(&loaded.file, params.function_id)
                .ok_or_else(|| {
                    McpError::invalid_params(
                        format!("Function {} not found", params.function_id),
                        None,
                    )
                })?;
            text_result(banner)
        })
    }
}

// Pipeline Stage Documentation
//
// This module documents the ordering and dependencies of all pipeline stages.
// It is not executable code, only documentation to prevent silent reordering bugs.
//
// Stages are listed in the order they run. A stage added between two existing ones
// takes a sub-label (W4a, W11b, F5a, F25b) rather than renumbering: the numbers are
// how code comments and the docs refer to a stage. The code marks each stage with a
// `// STAGE <id>:` comment, and the test at the bottom of this file fails if one is
// missing here or used twice, which is how this list went stale once before.
//
// ============================================================================
// WHOLE-PROGRAM STAGES (PipelineContext::build_with_options)
// ============================================================================
//
// STAGE W1: Closure Context Build
//   - Builds parent/child function relationships from raw IR.
//   - Marks async/generator functions from CreateAsyncClosure/CreateGeneratorClosure.
//   - REQUIRES: nothing (first stage)
//   - OUTPUT: ClosureContext
//
// STAGE W2: Metro Detection
//   - Scans global function for __d() calls to populate MetroRegistry.
//   - Uses raw (un-optimized) IR to avoid pattern destruction.
//   - REQUIRES: nothing (independent of W1)
//   - OUTPUT: MetroRegistry
//
// STAGE W3: Optimized IR Generation (parallel)
//   - Builds per-function IR with all transforms (SSA, propagation, structure, etc.).
//   - Applies register naming and semantic variable naming.
//   - REQUIRES: W1 (closure context for cross-function resolution)
//   - OUTPUT: BTreeMap<u32, Vec<Statement>> (all_ir)
//
// STAGE W4: Closure Analyze + Insert
//   - Analyzes optimized IR to update closure context with new definitions.
//   - Propagates async flags to generators.
//   - REQUIRES: W3 (optimized IR)
//   - OUTPUT: updated ClosureContext
//
// STAGE W4a: Cascade Apply (only with DecompileOptionsV2::cascade)
//   - Applies the names a proposal artifact carries, for the ones the bytecode confirms.
//   - Runs before naming so a confirmed name reaches call resolution like any other.
//   - REQUIRES: W3 (all_ir)
//   - OUTPUT: renamed all_ir, PipelineContext::cascade_names
//
// The naming pipeline (context/naming.rs) runs W4b through W11e.
//
// STAGE W4b: Module Names from fileFinishedImporting("…/Foo.tsx")
//   - A recorded source path is ground truth and overrides heuristic names.
//   - REQUIRES: W2 (MetroRegistry), W3 (all_ir)
//   - OUTPUT: MetroRegistry names (marked ground truth)
//
// STAGE W4c: Module Names from the Source File in Function Names
//   - REQUIRES: W4b (ground-truth names win)
//   - OUTPUT: MetroRegistry names
//
// STAGE W5: Module Name Propagation
//   - Sub-phases:
//     W5a: Reverse require naming (varName = require(depId) -> name dep module)
//     W5b: Infer names for anonymous modules from exports/body analysis
//     W5c: Re-export propagation (thin wrappers inherit dep name)
//     W5d: Dependency-chain naming (single-dep wrappers)
//     W5e: Propagate module names to closure slots
//     W5f: Propagate module names to require variables
//   - REQUIRES: W3 (all_ir), W2 (MetroRegistry), W4 (ClosureContext)
//   - OUTPUT: MetroRegistry with module names, renamed variables in all_ir
//
// STAGE W6: Closure Resolution (first pass)
//   - Replaces ClosureVar{slot} with named variables using closure context.
//   - REQUIRES: W5 (propagated names in closure context)
//   - OUTPUT: updated all_ir with resolved closure variables
//
// STAGE W7: Metro Export Analysis
//   - Populates MetroModule.exports maps by analyzing factory bodies.
//   - Detects: direct assignment, bulk assignment, Object.defineProperty patterns.
//   - REQUIRES: W6 (resolved closure variables for better analysis)
//   - OUTPUT: MetroRegistry with export maps
//
// STAGE W7b: Single-Export Module Naming
//   - Names still-unnamed modules from exactly one meaningful non-default export.
//   - REQUIRES: W7 (export maps)
//   - OUTPUT: MetroRegistry names
//
// STAGE W7c: Finalize Module Specifiers
//   - Drops placeholder specifiers and uniquifies collisions between Metro ids.
//   - REQUIRES: every module-naming pass above; must precede W8 and codegen
//   - OUTPUT: MetroRegistry specifiers
//
// STAGE W8: Inter-Procedural Analysis (IPA)
//   - 6-phase parameter name inference across function boundaries.
//   - Uses MetroRegistry exports for callee resolution.
//   - REQUIRES: W7 (export maps for resolution), W3 (all_ir for call site collection)
//   - OUTPUT: GlobalAnalysis (param_names, param_links, call graph, dead code)
//
// STAGE W9: IPA Closure Re-resolve
//   - Updates closure slots with IPA-inferred parameter names.
//   - Re-runs closure resolution with updated names.
//   - REQUIRES: W8 (IPA results)
//   - OUTPUT: updated ClosureContext and all_ir
//
// STAGE W10: Closure Property Naming
//   - Renames closure_N variables based on cross-function property access patterns.
//   - REQUIRES: W9 (re-resolved closures)
//   - OUTPUT: updated all_ir with renamed closure variables
//
// STAGE W11: Closure Definition Naming
//   - Renames closures from their definition sites (function assigned to variable).
//   - REQUIRES: W10 (after property naming to avoid conflicts)
//   - OUTPUT: updated all_ir
//
// STAGE W11a: dependencyMap[N] -> Absolute Module IDs
//   - REQUIRES: W10 (captures are renamed to `dependencyMap` only there)
//   - OUTPUT: rewritten all_ir
//
// STAGE W11b: Ancestor Closure Name Inheritance
//   - Baked `closure_{level}_{slot}` captures inherit the now-named ancestor slot.
//   - REQUIRES: W11 (ancestor slots named)
//   - OUTPUT: renamed all_ir
//
// STAGE W11c: Capture Name Sync
//   - Every capture takes the name its owner binds now.
//   - REQUIRES: W11b
//   - OUTPUT: renamed all_ir
//
// STAGE W11d: Deep Naming Fixed Point (only with DecompileOptionsV2::deep)
//   - Re-runs IPA and closure naming over the better-named bodies until nothing changes.
//   - REQUIRES: W11c
//   - OUTPUT: GlobalAnalysis param names, renamed all_ir
//
// STAGE W11e: Stable Module Naming (only with DecompileOptionsV2::stable)
//   - Names still-unnamed modules from a content hash instead of the Metro id.
//   - REQUIRES: every other naming stage
//   - OUTPUT: MetroRegistry names, renamed `module_{id}` variables
//
// The transform pipeline (context/transforms_phase.rs) runs W12 through W16g.
//
// STAGE W12: Strip Hermes This
//   - Removes meaningless `this` arguments from Call expressions.
//   - REQUIRES: W11 (all naming complete)
//   - OUTPUT: updated all_ir
//
// STAGE W13: Variable Inlining
//   - Eliminates dead/single-use temporaries (tmp*, closure_*, rN).
//   - REQUIRES: W12 (all naming and cleanup complete)
//   - OUTPUT: cleaner all_ir
//
// STAGE W14: Async Detection + Yield-to-Await
//   - Detects Babel async-to-generator patterns.
//   - Converts Yield expressions to Await in async function bodies.
//   - REQUIRES: W13 (inlined IR for cleaner pattern detection)
//   - OUTPUT: updated all_ir, updated ClosureContext async flags
//
// STAGE W15: Async Wrapper Unwrap
//   - Inlines Babel _asyncToGenerator wrapper bodies into the outer function.
//   - REQUIRES: W14 (async detection complete)
//   - OUTPUT: simplified all_ir
//
// STAGE W16: Post-IPA Transforms
//   - Reserved word renaming, object/array literal folding, arguments simplification.
//   - REQUIRES: W15 (all async transforms done)
//   - OUTPUT: all_ir
//
// STAGE W16a2: while(true) + trailing break -> do/while, guarded-loop folding
//   - REQUIRES: W13 (inlining cleans the latch)
//
// STAGE W16a3: Second JSX Pass (props are often still variables before inlining)
//   - REQUIRES: W13
//
// STAGE W16b: Collapse Generator Wrappers
//   - Inlines the inner generator body into its `function*` wrapper. Runs W16c-W16e.
//   - REQUIRES: W14 (async flags), W16
//
// STAGE W16c: HBC >= 97 Generator State Machines -> flat `yield` bodies
//   - Conservative: returns the body unchanged on any shape mismatch.
//
// STAGE W16d: HBC >= 97 / Babel Array Destructuring, second short-circuit fold
//
// STAGE W16e: JSX Reconstruction on the fully assembled, named IR
//   - Catches calls whose props object is materialized after F10.
//
// STAGE W16f: Object-Key Closure Naming
//   - Re-runs closure naming now that slot fills and W16c expose `{ key: closure }`.
//   - REQUIRES: W16b-W16e
//
// STAGE W16g: Hoist Repeated Closures
//   - One declaration per non-escaping function hermesc recreates at each call site.
//   - REQUIRES: W16f
//   - OUTPUT: final all_ir for the transform pipeline
//
// STAGE W16h: Import Hoisting
//   - Hoists repeated importDefault(N)/require(N) loads into one module-level binding.
//   - REQUIRES: W16g; must precede W17 so the rewritten bodies are the ones rendered
//   - OUTPUT: final all_ir
//
// STAGE W17: Inline Body Rendering
//   - Multi-pass pre-rendering of nested function bodies for codegen.
//   - Renders leaves first, then parents that reference them.
//   - REQUIRES: W16h (final all_ir)
//   - OUTPUT: inline_bodies map
//
// ============================================================================
// PER-FUNCTION STAGES (generate_ir in ir_gen.rs)
// ============================================================================
//
// STAGE F1:  IR Build (bytecode -> CFG)
// STAGE F2:  SSA / Live Range Splitting
// STAGE F3:  Copy/Constant Propagation
// STAGE F4:  Expression Simplification
// STAGE F5:  Structure Recovery (CFG -> if/while/for/switch/try), builtin-guard folding
// STAGE F5a: Loop/Iterator Recovery (for-of modern + legacy, for-in, iterator destructuring)
// STAGE F6:  Statement Optimization (if inversion, ternary detection, dead assign)
// STAGE F7:  Expression Inlining (single-use register elimination)
// STAGE F8:  Logic Transformation
// STAGE F9:  Concatenation Propagation
// STAGE F10: Pattern Detection (string concat, nullish, optional chaining, short-circuit)
// STAGE F11: Class Pattern Detection (ES6 class reconstruction)
// STAGE F12: Object/Array Literal Reconstruction
// STAGE F13: Default Parameter Detection
// STAGE F14: Spread/Rest Operators
// STAGE F15: Destructuring Detection
// STAGE F16: Generator/Async Pattern Detection
// STAGE F17: Yield-to-Await Conversion (async functions)
// STAGE F18: Cleanup (basic + advanced)
// STAGE F19: Chain Access Optimization
// STAGE F20: Ternary Return Optimization
// STAGE F21: Logic Simplification (advanced)
// STAGE F22: CommonJS Export Inference + Name Inference
// STAGE F23: Register Naming (analyze + debug info merge + rename)
// STAGE F24: Semantic Variable Naming
// STAGE F25: Final Simplification
// STAGE F25b: Loop Folding (while(true) -> do/while, guarded do-while -> for/while)
// STAGE F26: Closure Resolution (if context provided)

#[cfg(test)]
mod tests {
    // Every `// STAGE <id>:` marker in the pipeline must name a stage listed above,
    // and no two markers may share an id. A range marker (`// STAGE W12-W16:`) points
    // at stages documented elsewhere, so only its endpoints are checked.
    #[test]
    fn stage_markers_are_listed_and_unique() {
        let doc = include_str!("stages.rs");
        let sources = [
            ("ir_gen.rs", include_str!("ir_gen.rs")),
            ("context/mod.rs", include_str!("context/mod.rs")),
            ("context/naming.rs", include_str!("context/naming.rs")),
            (
                "context/transforms_phase.rs",
                include_str!("context/transforms_phase.rs"),
            ),
        ];
        let stage_id = |line: &str| -> Option<String> {
            let rest = line.trim_start().strip_prefix("// STAGE ")?;
            Some(
                rest.split([':', ' '])
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            )
        };
        let listed: std::collections::HashSet<String> = doc.lines().filter_map(stage_id).collect();
        let mut seen: std::collections::HashMap<String, &str> = Default::default();
        let mut ranged: std::collections::HashSet<String> = Default::default();
        for (file, src) in sources {
            for id in src.lines().filter_map(stage_id) {
                if let Some((from, to)) = id.split_once('-') {
                    for end in [from, to] {
                        assert!(
                            listed.contains(end),
                            "{file}: range end {end} not in stages.rs"
                        );
                        ranged.insert(end.to_string());
                    }
                    continue;
                }
                assert!(
                    listed.contains(&id),
                    "{file}: STAGE {id} is not in stages.rs"
                );
                if let Some(prev) = seen.insert(id.clone(), file) {
                    panic!("STAGE {id} is marked twice: {prev} and {file}");
                }
            }
        }
        for id in &listed {
            assert!(
                seen.contains_key(id) || ranged.contains(id),
                "stages.rs lists {id}, but nothing marks it"
            );
        }
    }
}

// IR generation: transform bytecode into IR statements.
// Contains generate_ir() and build_closure_context_from_file().

use crate::analysis::{
    rename_registers, resolve_closures_recording, ClosureContext, StructureAnalysis,
};
use crate::error::Result;
use crate::file::BytecodeFile;
use crate::ir::{BinaryOp, Expression, IRBuilder, IRBuilderOptions, Statement, Terminator};
use crate::opcode::BytecodeFormat;
use crate::transforms::{
    self, cleanup_statements, detect_class_patterns, detect_patterns, inline_expressions,
    optimize_statements, propagate, PropagationConfig,
};
use crate::util::is_valid_identifier;
use std::collections::BTreeMap;

use super::DecompileOptionsV2;

// Generate IR for a function (Analysis + Transform phases).

pub fn generate_ir(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    options: &DecompileOptionsV2,
    closure_ctx: Option<&ClosureContext>,
    perform_resolve: bool,
) -> Result<Vec<Statement>> {
    // STAGE F1: IR Build (bytecode -> CFG)
    let builder_options = IRBuilderOptions {
        resolve_strings: options.resolve_strings,
        include_offsets: options.include_offsets || options.assembly_mode,
        absolute_offsets: options.assembly_mode,
    };
    let mut builder = IRBuilder::new(file, format, builder_options);
    let mut cfg = builder.build_function(function_id)?;
    // The same level shift the closure context applied when it was built, so a
    // level means the same scope here as there.
    if let Some(ctx) = closure_ctx {
        crate::ir::shift_env_levels_cfg(&mut cfg, ctx.env_offset(function_id));
    }

    // STAGE F2: SSA / Live Range Splitting.
    // First resolve `globalThis` reads to the invariant value so the register
    // that held it is freed, the HBC >=97 allocator reuses it for unrelated
    // values, and an un-resolved read would force the SSA merge-freeze to
    // collapse the two live ranges under one name.
    transforms::resolve_global_reads(&mut cfg);
    transforms::transform_to_ssa(&mut cfg);

    // STAGE F3: Copy/Constant Propagation
    if options.propagate {
        // Cross-block copy propagation first: resolves register-to-register
        // copies whose source and use straddle a basic-block boundary (e.g. a
        // loop latch `Mov r0, i; ...; Inc i, r0`), which the intra-block pass
        // below cannot reach.
        transforms::propagate_copies(&mut cfg);
        propagate(&mut cfg, &PropagationConfig::default());
    }

    // STAGE F4: Expression Simplification
    if options.simplify {
        for block in cfg.blocks_mut() {
            crate::transforms::simplify_statements(&mut block.statements);
        }
    }

    // STAGE F5: Structure Recovery (CFG -> if/while/for/switch/try)
    let statements = if options.recover_structures {
        let analysis = StructureAnalysis::analyze(&cfg);
        // The `f.call === functionPrototypeCall` diamonds hermesc emits are
        // still plain if/else here; fold them before anything reshapes them.
        transforms::fold_builtin_guards(analysis.root.to_statements(&cfg))
    } else {
        // Flatten blocks without structure recovery
        let mut stmts = Vec::new();
        for (id, block) in cfg.blocks_with_ids() {
            stmts.push(Statement::Comment(format!("{id}:")));
            stmts.extend(block.statements.clone());
            match &block.terminator {
                Terminator::Return(v) => stmts.push(Statement::Return(v.clone())),
                Terminator::Throw(e) => stmts.push(Statement::Throw(e.clone())),
                Terminator::Jump(t) => stmts.push(Statement::Goto(*t)),
                Terminator::Branch {
                    condition,
                    true_target,
                    false_target,
                } => {
                    stmts.push(Statement::CondGoto {
                        condition: condition.clone(),
                        target: *true_target,
                        fallthrough: *false_target,
                    });
                }
                Terminator::Switch {
                    value,
                    cases,
                    default,
                } => {
                    stmts.push(Statement::Comment("Switch dispatch".to_string()));
                    for (case_val, target) in cases {
                        let condition =
                            Expression::binary(BinaryOp::StrictEq, value.clone(), case_val.clone());
                        stmts.push(Statement::CondGoto {
                            condition,
                            target: *target,
                            fallthrough: *default,
                        });
                    }
                    stmts.push(Statement::Goto(*default));
                }
                _ => {}
            }
        }
        stmts
    };

    // STAGE F5a: Reconstruct for-of / for-in loops from the iterator protocol right after
    // structure recovery, BEFORE inlining folds the iterator registers away
    // (later detect_patterns can no longer see `iter = src[Symbol.iterator]()`).
    let statements = if options.recover_structures {
        // Modern protocol (HBC >= 74: IteratorBegin/IteratorNext/IteratorClose).
        let s = transforms::detect_for_of_loops(statements);
        // Legacy protocol (HBC 59-71: full spec {value,done} iterator).
        let s = transforms::detect_legacy_for_of(s);
        let s = transforms::detect_for_in_loops(s);
        // Array destructuring shares the iterator protocol; match it here, before
        // inlining folds the iterator registers away.
        transforms::detect_iterator_destructuring(s)
    } else {
        statements
    };

    // Check if this function contains generator patterns
    let has_generator = transforms::has_generator_patterns(&statements);
    // Determine if this is an async function from the closure context
    let is_async_function = closure_ctx
        .map(|ctx| ctx.is_async(function_id))
        .unwrap_or(false);

    // Apply high-level optimizations
    let mut statements = if options.simplify {
        // STAGE F6: Statement Optimization (if inversion, ternary, dead assign)
        let statements = optimize_statements(statements);
        // STAGE F7: Expression Inlining (single-use register elimination)
        let statements = inline_expressions(statements);

        // STAGE F8: Logic Transformation
        let mut statements = statements;
        transforms::transform_logic(&mut statements);
        let statements = statements;

        // STAGE F9: Concatenation Propagation
        let statements = transforms::data_flow::propagate_concatenation(statements);
        // STAGE F10: Pattern Detection (string concat, nullish, optional chaining, short-circuit)
        let statements = detect_patterns(statements);

        // STAGE F11: Class Pattern Detection (ES6 class reconstruction)
        let statements = detect_class_patterns(statements, file, format, options, closure_ctx);

        let mut statements = statements;

        trace_supers("before F12", &statements);
        // STAGE F12: Object/Array Literal Reconstruction
        transforms::transform_object_literals(&mut statements);
        transforms::arrays::transform_array_literals(&mut statements);
        trace_supers("before F13", &statements);
        // STAGE F13: Default Parameter Detection
        transforms::transform_default_params(&mut statements);

        trace_supers("before F14", &statements);
        // STAGE F14: Spread/Rest Operators
        transforms::transform_spread_rest(&mut statements);
        let statements = statements;

        trace_supers("before F15", &statements);
        // STAGE F15: Destructuring Detection
        let statements = transforms::detect_destructuring(statements);

        trace_supers("before F16", &statements);
        // STAGE F16: Generator/Async Pattern Detection
        let statements = if has_generator {
            let statements = transforms::detect_generator_patterns(statements, is_async_function);
            let statements = transforms::simplify_state_machine(statements);
            transforms::cleanup_generator_comments(statements)
        } else {
            statements
        };

        trace_supers("before F17", &statements);
        // STAGE F17: Yield-to-Await Conversion (async functions)
        let statements = if is_async_function {
            convert_yields_to_awaits(statements)
        } else {
            statements
        };

        trace_supers("before F18", &statements);
        // STAGE F18: Cleanup (basic + advanced)
        let statements = cleanup_statements(statements);
        let statements = transforms::cleanup_advanced(statements);

        trace_supers("before F19", &statements);
        // STAGE F19: Chain Access Optimization
        let statements = transforms::optimize_chain_access(statements);

        trace_supers("before F20", &statements);
        // STAGE F20: Ternary Return Optimization
        let statements = transforms::optimize_ternary_returns(statements);

        trace_supers("before F21", &statements);
        // STAGE F21: Logic Simplification (advanced)
        let statements = transforms::simplify_logic_advanced(statements);

        let mut statements = statements;

        trace_supers("before F22", &statements);
        // STAGE F22: CommonJS Export Inference + Name Inference
        let param_count = file
            .function_headers
            .get(function_id as usize)
            .map(|h| h.param_count())
            .unwrap_or(0);

        if let Some(names) = transforms::exports::infer_commonjs_names(&mut statements, param_count)
        {
            transforms::exports::rename_param_registers(&mut statements, &names);
        }

        transforms::infer_names(&mut statements);
        let statements = statements;

        trace_supers("before F23", &statements);
        // STAGE F23: Register Naming (analyze + debug info merge + rename)
        let statements = super::apply_register_naming(statements, file, function_id);
        trace_supers("after F23", &statements);

        trace_supers("before F24", &statements);
        // STAGE F24: Semantic Variable Naming
        let mut statements = transforms::infer_variable_names(statements);
        trace_supers("after F24", &statements);

        // Shape-table slot fills (`obj[N] = val`) only fold while the object is
        // still a Register. After naming they are Variable/Let — fold again.
        transforms::fold_slot_index_fills(&mut statements);

        trace_supers("before F25", &statements);
        // STAGE F25: Final Simplification
        crate::transforms::simplify_statements(&mut statements);

        trace_supers("before F25b", &statements);
        // STAGE F25b: Bottom-tested `while (true) { …; if (EXIT) break; }` -> `do…while`,
        // then fold Hermes' guarded do-while shape back into a natural `for`/`while`.
        // Runs last, on fully-named statements, once cleanup has produced the clean
        // trailing `if (EXIT) break;` shape.
        let statements = transforms::convert_while_true_loops(statements);
        transforms::fold_guarded_loops(statements)
    } else {
        statements
    };

    trace_supers("before F26", &statements);
    // STAGE F26: Closure Resolution (if context provided)
    if perform_resolve {
        if let Some(ctx) = closure_ctx {
            let closure_info = ctx.get_closure_info_for(function_id);
            if !closure_info.slots.is_empty() {
                statements =
                    resolve_closures_recording(statements, &closure_info, Some((ctx, function_id)))
                        .0;
            }
        }
    }

    Ok(statements)
}

// Build a closure context by analyzing all functions.
// This enables cross-function closure resolution.
pub fn build_closure_context_from_file(
    file: &BytecodeFile,
    format: &BytecodeFormat,
) -> Result<ClosureContext> {
    use rayon::prelude::*;

    let builder_options = IRBuilderOptions {
        resolve_strings: true,
        include_offsets: false,
        absolute_offsets: false,
    };

    // Parallel: compute per-function data (name + analyzed statements).
    // rayon preserves input order in the output Vec.
    type FunctionIr = (
        u32,
        Option<String>,
        Vec<Statement>,
        Vec<(u32, u32)>,
        Vec<(u32, u32)>,
        u32,
        crate::ir::env_values::EnvFacts,
    );
    let results: Vec<Option<FunctionIr>> = (0..file.function_headers.len())
        .into_par_iter()
        .map(|i| {
            let function_id = i as u32;
            let header = &file.function_headers[i];

            let func_name = file
                .string_at(header.function_name())
                .filter(|e| !e.value.is_empty() && is_valid_identifier(&e.value))
                .map(|e| e.value.clone());

            let mut builder = IRBuilder::new(file, format, builder_options.clone());
            builder.collect_env_facts = true;
            let Ok(mut cfg) = builder.build_function(function_id) else {
                return None;
            };
            let env_facts = std::mem::take(&mut builder.env_facts);
            let created_in_block = std::mem::take(&mut builder.created_in_block);
            let created_with_env = std::mem::take(&mut builder.created_with_env);
            let slot_held_accesses = builder.slot_held_accesses;
            propagate(&mut cfg, &PropagationConfig::default());

            let analysis = StructureAnalysis::analyze(&cfg);
            let statements = analysis.root.to_statements(&cfg);

            let debug_names = if let Some(debug_info) = &file.debug_info {
                // Via `function_scopes`, not via the location stream's
                // `scopeAddress`: that field is 0 on real output, so this used to
                // resolve to the empty scope even once the streams were readable.
                debug_info.variable_map_for_function(function_id)
            } else {
                BTreeMap::new()
            };

            let statements = if !debug_names.is_empty() {
                rename_registers(statements, &debug_names)
            } else {
                statements
            };

            Some((
                function_id,
                func_name,
                statements,
                created_in_block,
                created_with_env,
                slot_held_accesses,
                env_facts,
            ))
        })
        .collect();

    // Sequential: build ClosureContext from computed results (order is preserved).
    // Creation scopes first, so every parent link the walk records sees them.
    let mut ctx = ClosureContext::new();
    let mut created = Vec::new();
    let mut results = results;
    let env_facts: Vec<(u32, crate::ir::env_values::EnvFacts)> = results
        .iter_mut()
        .flatten()
        .map(|r| (r.0, std::mem::take(&mut r.6)))
        .collect();
    for item in results.iter().flatten() {
        for &(child, level) in &item.3 {
            ctx.set_creation_scope(child, item.0, level);
        }
        created.extend(item.4.iter().map(|&(child, level)| (item.0, child, level)));
        ctx.slot_held_accesses += item.5;
    }
    for (_, facts) in &env_facts {
        for (_, through, _, _) in &facts.accesses {
            *ctx.env_access_kinds
                .entry(through.kind().to_string())
                .or_default() += 1;
        }
    }
    let scope_graph = crate::analysis::closure::scope_graph::ScopeGraph::build(
        env_facts.iter().map(|(f, x)| (*f, x)),
    );
    // Each function's levels count from the environment it was created with;
    // shift them so level L is L hops up the parent chain before anything
    // keys a slot by level.
    ctx.set_env_offsets(&created);
    for item in results.into_iter().flatten() {
        let (function_id, func_name, mut statements, _, _, _, _) = item;
        if let Some(name) = func_name {
            ctx.add_function_name(function_id, name);
        }
        crate::ir::shift_env_levels(&mut statements, ctx.env_offset(function_id));
        ctx.analyze_function(function_id, &statements);
    }
    ctx.scope_graph_stats = scope_graph_census(&ctx, &scope_graph, &env_facts);
    if let Ok(path) = std::env::var("HD_ENV_DUMP") {
        dump_env_facts(
            &path,
            &ctx,
            &scope_graph,
            env_facts.iter().map(|(f, x)| (*f, x)),
        );
    }

    // Propagate async flag from outer wrappers to inner generators
    ctx.propagate_async_to_generators();

    Ok(ctx)
}

// Convert all `Expression::Yield` to `Expression::Await` in a statement list.
// Used for async functions where the CFG-level generator transform emitted Yield
// but the function is actually async.
// Rewrite every `yield` into `await` in an async function body.
//
// Hermes compiles `async` to a generator, so the reconstructed body suspends
// with `yield`; the async form of the same suspension is `await`. This walks the
// whole tree through MutVisitor rather than a hand written match, because the
// previous hand written walk skipped object and array literals, `new`, spreads,
// switches and the for-in/for-of/do-while bodies, which left a bare `yield`
// inside a function rendered as `async`.
pub(crate) fn convert_yields_to_awaits(stmts: Vec<Statement>) -> Vec<Statement> {
    use crate::ir::MutVisitor;

    struct YieldToAwait;
    impl MutVisitor for YieldToAwait {
        fn visit_expression(&mut self, expr: &mut Expression) {
            self.walk_expression(expr);
            if let Expression::Yield { value, .. } = expr {
                let inner =
                    std::mem::replace(value.as_mut(), Expression::Value(crate::ir::Value::This));
                *expr = Expression::Await(Box::new(inner));
            }
        }
    }

    let mut stmts = stmts;
    YieldToAwait.visit_statement_list(&mut stmts);
    stmts
}

// Trace target `supers`: the `extends` expression of every class in the
// statements, tagged with the pipeline stage, to find the pass that rewrites it.
pub(crate) fn trace_supers(tag: &str, statements: &[crate::ir::Statement]) {
    if !log::log_enabled!(target: "supers", log::Level::Trace) {
        return;
    }
    for s in statements {
        if let crate::ir::Statement::Class {
            name, super_class, ..
        } = s
        {
            if let Some(sc) = super_class {
                log::trace!(target: "supers", "{tag}: class {name} extends {sc}");
                let needle = sc.to_string();
                for other in statements {
                    let text = other.to_string();
                    if !matches!(other, crate::ir::Statement::Class { .. })
                        && text.contains(&needle)
                    {
                        let line = text.lines().next().unwrap_or("");
                        log::trace!(target: "supers", "{tag}:   uses {needle}: {line}");
                    }
                }
            }
        }
    }
}

// One line per fact (`HD_ENV_DUMP=<path>`): the environment-value pass's view
// of every function, for reading a case by hand and for diffing two builds.
fn dump_env_facts<'a>(
    path: &str,
    ctx: &ClosureContext,
    graph: &crate::analysis::closure::scope_graph::ScopeGraph,
    facts: impl Iterator<Item = (u32, &'a crate::ir::env_values::EnvFacts)>,
) {
    use std::fmt::Write as _;
    let mut out = String::new();
    let scope = |f: u32, env: &crate::ir::env_values::Env| {
        graph
            .resolve(f, env)
            .map_or("?".to_string(), |s| s.to_string())
    };
    for (f, facts) in facts {
        if let Some(s) = graph.creation(f) {
            let _ = writeln!(out, "{f}	created-with	{s}");
        }
        for (site, parent) in &facts.created {
            let parent = parent
                .as_ref()
                .map_or("root".to_string(), |p| p.to_string());
            let _ = writeln!(out, "{f}	create	@{site}	{parent}");
        }
        for (through, slot, value) in &facts.parked {
            let _ = writeln!(out, "{f}	park	{through}[{slot}]	{value}");
        }
        for (child, env) in &facts.closures {
            let _ = writeln!(out, "{f}	closure	{child}	{env}	{}", scope(f, env));
        }
        for (off, through, slot, store) in &facts.accesses {
            let op = if *store { "store" } else { "load" };
            // Where the level model disagrees, what it says instead.
            let level_model = match through {
                crate::ir::env_values::Env::Closure(n) => {
                    let cur = ctx.ancestor_at(f, n + ctx.env_offset(f));
                    let same = matches!(
                        (graph.resolve(f, through), cur),
                        (Some(crate::analysis::closure::scope_graph::Scope::Fn(g)), Some(c)) if g == c
                    );
                    match cur {
                        Some(c) if !same => format!("	level model: F{c}"),
                        None => "	level model: ?".to_string(),
                        _ => String::new(),
                    }
                }
                _ => String::new(),
            };
            let _ = writeln!(
                out,
                "{f}	{op}	@{off}	{through}[{slot}]	{}{level_model}",
                scope(f, through)
            );
        }
    }
    if let Err(e) = std::fs::write(path, out) {
        log::warn!("cannot write {path}: {e}");
    }
}

// What the scope graph says each load and store reaches, set against the level
// model the IR is built with (V98_ENV_PLAN.md E3): a closure-level access
// either names the same function both ways, another function, or a scope a
// function created past its own, which the level model has no owner for.
fn scope_graph_census(
    ctx: &ClosureContext,
    graph: &crate::analysis::closure::scope_graph::ScopeGraph,
    facts: &[(u32, crate::ir::env_values::EnvFacts)],
) -> BTreeMap<String, u32> {
    use crate::analysis::closure::scope_graph::Scope;
    use crate::ir::env_values::Env;
    let mut census: BTreeMap<String, u32> = BTreeMap::new();
    for (f, facts) in facts {
        for (_, through, _, _) in &facts.accesses {
            let resolved = graph.resolve(*f, through);
            let class: String = match (through, resolved) {
                (Env::Closure(n), Some(Scope::Fn(g))) => {
                    if ctx.ancestor_at(*f, n + ctx.env_offset(*f)) == Some(g) {
                        "closure: same function".into()
                    } else {
                        "closure: another function".into()
                    }
                }
                // The level model gives a block environment a synthetic scope
                // of the function that built it; the same function is the
                // same answer under another name.
                (Env::Closure(n), Some(Scope::Created(h, _))) => {
                    match ctx.ancestor_at(*f, n + ctx.env_offset(*f)) {
                        Some(cur) if ctx.is_block_scope(cur) && ctx.scope_function(cur) == h => {
                            "closure: created scope, a block of the same function".into()
                        }
                        _ => "closure: created scope, elsewhere".into(),
                    }
                }
                (Env::Closure(n), None) => {
                    format!("closure unresolved: {}", graph.why_unresolved(*f, *n))
                }
                (Env::Own, _) => "own".into(),
                (Env::Created { .. }, _) => "created".into(),
                (Env::InSlot(..), Some(_)) => "slot-held: resolved".into(),
                (Env::InSlot(..), None) => "slot-held: unresolved".into(),
                (Env::Up(..), Some(_)) => "up from a slot: resolved".into(),
                (Env::Up(..), None) => "up from a slot: unresolved".into(),
                (Env::Unknown, _) => "unknown".into(),
            };
            *census.entry(class).or_default() += 1;
        }
    }
    let (creation, parked) = graph.conflicts();
    census.insert("conflicts: functions".to_string(), creation as u32);
    census.insert("conflicts: slots".to_string(), parked as u32);
    census
}

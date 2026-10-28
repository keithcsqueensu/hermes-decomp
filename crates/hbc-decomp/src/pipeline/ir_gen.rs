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
    // The scope graph's levels, as the closure context built this function
    // with them; such a function has no shift.
    builder.env_plan = closure_ctx.and_then(|c| c.env_plans.get(&function_id).cloned());
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
        let statements = transforms::fold_guarded_loops(statements);
        // A base-class constructor builds and returns its own receiver; on
        // fully named statements, give it `this` back.
        transforms::restore_constructor_this(statements)
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

// One function's IR as the closure context reads it, with what the builder
// recorded on the way.
struct FunctionIr {
    id: u32,
    name: Option<String>,
    statements: Vec<Statement>,
    created_in_block: Vec<(u32, u32)>,
    created_with_env: Vec<(u32, u32)>,
    slot_held_accesses: u32,
    env: FunctionEnv,
}

// What the environment-value pass and the builder say about one function's
// environments, by instruction offset.
#[derive(Default)]
struct FunctionEnv {
    facts: crate::ir::env_values::EnvFacts,
    // (offset, builder level) of each environment claimed past its own.
    sites: Vec<(u32, u32)>,
    // (offset, builder level) of each load and store, before any shift.
    access_levels: Vec<(u32, u32)>,
}

fn build_for_context(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    plan: Option<crate::ir::EnvPlan>,
) -> Option<FunctionIr> {
    let builder_options = IRBuilderOptions {
        resolve_strings: true,
        include_offsets: false,
        absolute_offsets: false,
    };
    let header = &file.function_headers[function_id as usize];
    let name = file
        .string_at(header.function_name())
        .filter(|e| !e.value.is_empty() && is_valid_identifier(&e.value))
        .map(|e| e.value.clone());

    let mut builder = IRBuilder::new(file, format, builder_options);
    builder.collect_env_facts = plan.is_none();
    builder.env_plan = plan;
    let mut cfg = builder.build_function(function_id).ok()?;
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

    Some(FunctionIr {
        id: function_id,
        name,
        statements,
        created_in_block: std::mem::take(&mut builder.created_in_block),
        created_with_env: std::mem::take(&mut builder.created_with_env),
        slot_held_accesses: builder.slot_held_accesses,
        env: FunctionEnv {
            facts: std::mem::take(&mut builder.env_facts),
            sites: std::mem::take(&mut builder.env_sites),
            access_levels: std::mem::take(&mut builder.env_access_levels),
        },
    })
}

// Build a closure context by analyzing all functions.
// This enables cross-function closure resolution.
pub fn build_closure_context_from_file(
    file: &BytecodeFile,
    format: &BytecodeFormat,
) -> Result<ClosureContext> {
    use rayon::prelude::*;

    // Parallel: compute per-function data (name + analyzed statements).
    // rayon preserves input order in the output Vec.
    let mut results: Vec<Option<FunctionIr>> = (0..file.function_headers.len() as u32)
        .into_par_iter()
        .map(|f| build_for_context(file, format, f, None))
        .collect();

    // Sequential: build ClosureContext from computed results (order is preserved).
    // Creation scopes first, so every parent link the walk records sees them.
    let mut ctx = ClosureContext::new();
    let mut created = Vec::new();
    let env: Vec<(u32, FunctionEnv)> = results
        .iter_mut()
        .flatten()
        .map(|r| (r.id, std::mem::take(&mut r.env)))
        .collect();
    for item in results.iter().flatten() {
        for &(child, level) in &item.created_in_block {
            ctx.set_creation_scope(child, item.id, level);
        }
        created.extend(
            item.created_with_env
                .iter()
                .map(|&(child, level)| (item.id, child, level)),
        );
        ctx.slot_held_accesses += item.slot_held_accesses;
    }
    for (_, e) in &env {
        for (_, through, _, _) in &e.facts.accesses {
            *ctx.env_access_kinds
                .entry(through.kind().to_string())
                .or_default() += 1;
        }
    }
    let scope_graph = crate::analysis::closure::scope_graph::ScopeGraph::build(
        env.iter().map(|(f, e)| (*f, &e.facts)),
    );
    // Each function's levels count from the environment it was created with;
    // shift them so level L is L hops up the parent chain before anything
    // keys a slot by level.
    ctx.set_env_offsets(&created);
    for (f, e) in &env {
        for (child, through) in &e.facts.closures {
            if matches!(
                scope_graph.resolve(*f, through),
                Some(crate::analysis::closure::scope_graph::Scope::Created(..))
            ) || matches!(through, crate::ir::env_values::Env::Created { .. })
            {
                ctx.block_created.insert(*child);
            }
        }
    }
    // Then the scope graph's word wherever it has one (V98_ENV_PLAN.md E4):
    // the parents it resolves, and the levels of the accesses whose scope it
    // places differently. A function whose levels change is built again with
    // them, and that build is final.
    let (mut census, expected) = plan_env_levels(&mut ctx, &scope_graph, &env);
    let rebuilt: Vec<Option<FunctionIr>> = ctx
        .env_plans
        .iter()
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|(&f, plan)| build_for_context(file, format, f, Some(plan.clone())))
        .collect();
    for item in rebuilt.into_iter().flatten() {
        let id = item.id as usize;
        if let Some(Some(slot)) = results.get_mut(id) {
            slot.statements = item.statements;
        }
    }
    for item in results.into_iter().flatten() {
        if let Some(name) = item.name {
            ctx.add_function_name(item.id, name);
        }
        let mut statements = item.statements;
        crate::ir::shift_env_levels(&mut statements, ctx.env_offset(item.id));
        ctx.analyze_function(item.id, &statements);
    }
    verify_env_plans(&ctx, &expected, &mut census);
    ctx.scope_graph_stats = census;
    if let Ok(path) = std::env::var("HD_ENV_DUMP") {
        dump_env_facts(
            &path,
            &ctx,
            &scope_graph,
            env.iter().map(|(f, e)| (*f, &e.facts)),
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
        let plan = ctx.env_plans.get(&f);
        for (off, through, slot, store) in &facts.accesses {
            let op = if *store { "store" } else { "load" };
            // Where the graph moved the access, the level it moved it to.
            let planned = plan
                .and_then(|p| p.levels.get(off))
                .map_or(String::new(), |l| format!("	level {l}"));
            let _ = writeln!(
                out,
                "{f}	{op}	@{off}	{through}[{slot}]	{}{planned}",
                scope(f, through)
            );
        }
    }
    if let Err(e) = std::fs::write(path, out) {
        log::warn!("cannot write {path}: {e}");
    }
}

// A scope as the closure context names it: (function, None) for a function's
// own environment, (function, Some(builder level)) for a block environment it
// built.
type ScopeRef = (u32, Option<u32>);

// Where the scope graph's word goes into the closure context (V98_ENV_PLAN.md
// E4): the parents it resolves, for functions and block environments, and the
// level of every load and store whose scope it places somewhere the builder's
// level does not reach. Returns the census, and the scope each planned access
// should reach once the parents are in (`verify_env_plans`).
fn plan_env_levels(
    ctx: &mut ClosureContext,
    graph: &crate::analysis::closure::scope_graph::ScopeGraph,
    env: &[(u32, FunctionEnv)],
) -> (BTreeMap<String, u32>, BTreeMap<(u32, u32), ScopeRef>) {
    use crate::analysis::closure::scope_graph::Scope;
    use std::collections::HashMap;
    let site_level: HashMap<(u32, u32), u32> = env
        .iter()
        .flat_map(|(f, e)| e.sites.iter().map(move |&(site, l)| ((*f, site), l)))
        .collect();
    let to_ref = |s: Scope| -> Option<ScopeRef> {
        match s {
            Scope::Fn(g) => Some((g, None)),
            Scope::Created(g, site) => site_level.get(&(g, site)).map(|&l| (g, Some(l))),
        }
    };
    let mut census: BTreeMap<String, u32> = BTreeMap::new();
    let mut count = |k: &str| *census.entry(k.to_string()).or_default() += 1;

    for f in graph.functions_created() {
        if let Some(r) = graph.creation(f).and_then(to_ref) {
            if r != (f, None) {
                ctx.scope_parents.insert(f, r);
                count("parents: functions");
            }
        }
    }
    for (&(g, site), &level) in &site_level {
        if let Some(r) = graph.parent(Scope::Created(g, site)).and_then(to_ref) {
            if r != (g, Some(level)) {
                ctx.block_parents.insert((g, level), r);
                count("parents: blocks");
            }
        }
    }

    // S3.3: a function created with several scopes, by the function whose
    // body creates it and every scope it creates it on there. Each creator
    // renders the one body, so each scope needs its own reading of the body's
    // slots: several creators each on one scope, or one creator on several
    // (copies of an inlined factory).
    let mut creations_of: HashMap<u32, Vec<(u32, &crate::ir::env_values::Env)>> = HashMap::new();
    for (g, e) in env {
        for (child, through) in &e.facts.closures {
            if graph.creations(*child).is_some_and(|set| set.len() > 1) {
                creations_of.entry(*child).or_default().push((*g, through));
            }
        }
    }
    let mut installed: Vec<u32> = Vec::new();
    for (f, sites) in creations_of {
        let mut by_creator: BTreeMap<u32, std::collections::BTreeSet<ScopeRef>> = BTreeMap::new();
        let mut unresolved = false;
        for (g, through) in sites {
            match graph.resolve(g, through).and_then(to_ref) {
                Some(r) => {
                    by_creator.entry(g).or_default().insert(r);
                }
                None => unresolved = true,
            }
        }
        let scopes: usize = by_creator.values().map(|s| s.len()).sum();
        if unresolved || scopes < 2 {
            count("shared: a creation scope unresolved");
            continue;
        }
        if by_creator.values().any(|s| s.len() > 1) {
            count("shared: functions with copies in one creator");
        }
        // S3.4: the chain needs one parent for the function; take its first
        // creation scope. The body is named against that scope, and every
        // other creation scope reads a copy renamed onto its own slots, so a
        // read two levels down through the function lands on a slot the copy
        // renames, not past the function into whatever `add_child` linked.
        if let Some(&rep) = by_creator.values().next().and_then(|s| s.iter().next()) {
            if let std::collections::btree_map::Entry::Vacant(e) = ctx.scope_parents.entry(f) {
                e.insert(rep);
                installed.push(f);
            }
        }
        ctx.shared_creations.insert(f, by_creator);
        count("shared: functions");
    }

    // The scope `n` hops up the installed chain from `from`.
    let walk = |from: ScopeRef, n: u32| -> Option<ScopeRef> {
        let mut cur = from;
        for _ in 0..n {
            cur = match cur {
                (g, None) => *ctx.scope_parents.get(&g)?,
                (g, Some(level)) => *ctx.block_parents.get(&(g, level))?,
            };
        }
        Some(cur)
    };
    // A shared function one of whose creators is itself or sits below it
    // (ts-pattern's `optional`, whose inlined helper creates it again): it is
    // declared once per creator, not copied, so a single parent would read one
    // creator's scope from every declaration.
    let creates_itself =
        |g: u32, creators: &BTreeMap<u32, std::collections::BTreeSet<ScopeRef>>| {
            creators.keys().any(|&c| {
                let mut cur: ScopeRef = (c, None);
                for _ in 0..64 {
                    if cur == (g, None) {
                        return true;
                    }
                    match walk(cur, 1) {
                        Some(next) => cur = next,
                        None => return false,
                    }
                }
                false
            })
        };
    // Such a function keeps no parent, and its reads the level model's answer.
    let selfish: Vec<u32> = installed
        .iter()
        .copied()
        .filter(|g| creates_itself(*g, &ctx.shared_creations[g]))
        .collect();
    for g in &selfish {
        ctx.scope_parents.remove(g);
    }
    for _ in &selfish {
        count("shared: functions that create themselves");
    }
    // The level that reaches `s` from `f`: its own scopes directly, anything
    // else as the hops up its chain -- the chain of parents installed above,
    // which is the one `slot_owner` will walk, not the graph's own: a level
    // every creator of a shared function reaches the same way is one answer
    // to the graph and none to a single parent link.
    let level_for = |f: u32, s: Scope| -> Option<u32> {
        match s {
            Scope::Fn(g) if g == f => return Some(0),
            Scope::Created(g, site) if g == f => return site_level.get(&(f, site)).copied(),
            _ => {}
        }
        let want = to_ref(s)?;
        let mut cur: ScopeRef = (f, None);
        for k in 1..=64u32 {
            cur = match cur {
                (g, None) => *ctx.scope_parents.get(&g)?,
                (g, Some(level)) => match ctx.block_parents.get(&(g, level)) {
                    Some(&r) => r,
                    None => return None,
                },
            };
            if cur == want {
                return Some(k);
            }
        }
        None
    };
    // The same walk on the chain as it now stands.
    let walk = |from: ScopeRef, n: u32| -> Option<ScopeRef> {
        let mut cur = from;
        for _ in 0..n {
            cur = match cur {
                (g, None) => *ctx.scope_parents.get(&g)?,
                (g, Some(level)) => *ctx.block_parents.get(&(g, level))?,
            };
        }
        Some(cur)
    };
    // S3.4: what a closure access the graph left unresolved reads on the
    // installed chain, when that reading holds for every creator: through a
    // shared function, the creation scope itself is renamed per copy, and a
    // scope past it must be the same from every creation scope.
    // `Err(true)`: the creation scopes disagree, so there is no single answer
    // and the access is an explicit unknown. `Err(false)`: the chain breaks
    // (a link the graph does not have), and the level model's walk up the
    // lexical parents stays, being right more often than not.
    let chain_answer = |f: u32, n: u32| -> std::result::Result<ScopeRef, bool> {
        let mut cur: ScopeRef = (f, None);
        for k in 0..n {
            if let (g, None) = cur {
                if let Some(creators) = ctx.shared_creations.get(&g) {
                    let rest = n - k - 1;
                    if rest > 0 {
                        let mut above = creators.values().flatten().map(|&s| walk(s, rest));
                        let first = above.next().flatten().ok_or(false)?;
                        if above.any(|a| a != Some(first)) {
                            return Err(true);
                        }
                    }
                }
            }
            cur = walk(cur, 1).ok_or(false)?;
        }
        Ok(cur)
    };
    // S3 (S3_PARKED_ENV_PLAN.md S3.0): for each parked (scope, slot), how the
    // accesses through an environment loaded out of it resolved. A parking
    // store is safe to drop only when every read through it resolved.
    let mut parked_reads: HashMap<(Scope, u32), (u32, u32)> = HashMap::new();
    let mut expected = BTreeMap::new();
    for (f, e) in env {
        let raw: HashMap<u32, u32> = e.access_levels.iter().copied().collect();
        let shift = ctx.env_offset(*f);
        let mut levels = BTreeMap::new();
        for (off, through, _, _) in &e.facts.accesses {
            let Some(&raw_level) = raw.get(off) else {
                count("access: the builder made none");
                continue;
            };
            let old = crate::ir::EnvPlan::shifted(shift, raw_level);
            let parked_key = parked_base(through)
                .and_then(|(inner, slot)| graph.resolve(*f, inner).map(|s| (s, slot)));
            let mut tally = |ok: bool| {
                if let Some(key) = parked_key {
                    let e = parked_reads.entry(key).or_default();
                    if ok {
                        e.0 += 1;
                    } else {
                        e.1 += 1;
                    }
                }
            };
            let Some(s) = graph.resolve(*f, through) else {
                count(&format!("access: unresolved, {}", through.kind()));
                // Why, for a closure access: the first link up its chain
                // the graph does not have (S3.3).
                if let crate::ir::env_values::Env::Closure(n) = through {
                    let why = graph.why_unresolved(*f, *n);
                    count(&format!("unresolved closure: {why}"));
                    // Whether the scope read is a creation scope of a shared
                    // function the access sits in (itself at level 1, an
                    // enclosing function higher up): each of those creators
                    // reads its own copy of the body, so the graph's missing
                    // single answer is served at render (shared_bodies.rs).
                    let mut cur = Some(*f);
                    let mut shared = false;
                    for _ in 1..=*n {
                        let Some(g) = cur else { break };
                        if ctx.shared_creations.contains_key(&g) {
                            shared = true;
                            break;
                        }
                        cur = ctx.scope_parents.get(&g).map(|&(p, _)| p);
                    }
                    let place = if shared { "a shared body" } else { "elsewhere" };
                    count(&format!("unresolved closure in {place}: {why}"));
                    // S3.4: the chain's answer where it holds for every
                    // creator, else an explicit unknown rather than whatever
                    // the level model's walk reaches.
                    match chain_answer(*f, *n) {
                        Ok(r) => {
                            let l = *n;
                            if l != old {
                                levels.insert(*off, l);
                            }
                            expected.insert((*f, *off), r);
                            count("unresolved closure: read on the chain");
                        }
                        Err(true) => {
                            levels.insert(*off, crate::ir::UNKNOWN_ENV_LEVEL);
                            count("unresolved closure: unknown, creation scopes disagree");
                        }
                        Err(false) => count("unresolved closure: chain broken, level model"),
                    }
                }
                tally(false);
                continue;
            };
            match (level_for(*f, s), to_ref(s)) {
                (Some(l), Some(r)) if l != old => {
                    levels.insert(*off, l);
                    expected.insert((*f, *off), r);
                    count("access: level changed");
                    count(&format!("access: resolved, {}", through.kind()));
                    tally(true);
                }
                (Some(_), Some(_)) => {
                    count("access: level kept");
                    count(&format!("access: resolved, {}", through.kind()));
                    tally(true);
                }
                _ => {
                    count("access: resolved, not on the chain");
                    tally(false);
                }
            }
        }
        if !levels.is_empty() {
            ctx.env_plans.insert(
                *f,
                crate::ir::EnvPlan {
                    levels,
                    shift,
                    drop_stores: Default::default(),
                },
            );
        }
    }
    // Each parking store, by whether the reads through what it parks
    // resolved. One whose reads all resolved, or that nobody reads, is
    // dropped from the output (S3.1); the parked scope must be the graph's
    // one answer for that slot, or another creator's reads could need it.
    let mut drops: BTreeMap<u32, std::collections::BTreeSet<u32>> = BTreeMap::new();
    for (f, e) in env {
        for ((through, slot, _), at) in e.facts.parked.iter().zip(&e.facts.parked_at) {
            let scope = graph.resolve(*f, through);
            let class = match scope.and_then(|s| parked_reads.get(&(s, *slot))) {
                None if scope.is_some() => "parking: store, never read",
                None => "parking: store, slot unresolved",
                Some(&(_, 0)) => "parking: store, every read resolved",
                Some(_) => "parking: store, a read unresolved",
            };
            count(class);
            if matches!(
                class,
                "parking: store, never read" | "parking: store, every read resolved"
            ) {
                drops.entry(*f).or_default().insert(*at);
            }
        }
    }
    // A plan that only drops stores moves no level: the function is rebuilt
    // unshifted (shift 0) and keeps its environment offset, so the shift
    // after the build (`shift_env_levels`) applies to it exactly as to a
    // function built without a plan, and dropping the stores is the only
    // difference between the two builds.
    let mut drop_only: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for (f, set) in drops {
        let plan = ctx.env_plans.entry(f).or_insert_with(|| {
            drop_only.insert(f);
            crate::ir::EnvPlan {
                levels: BTreeMap::new(),
                shift: 0,
                drop_stores: Default::default(),
            }
        });
        plan.drop_stores = set;
    }
    let planned: Vec<u32> = ctx.env_plans.keys().copied().collect();
    for f in planned.iter().filter(|f| !drop_only.contains(f)) {
        ctx.env_offsets.remove(f);
    }
    census.insert("functions planned".to_string(), planned.len() as u32);
    let (creation, parked) = graph.conflicts();
    census.insert("conflicts: functions".to_string(), creation as u32);
    census.insert("conflicts: slots".to_string(), parked as u32);
    (census, expected)
}

// The parked slot an access goes through: `InSlot(E, s)` directly, or under
// a walk up from it (`Up(InSlot(E, s), n)`).
fn parked_base(env: &crate::ir::env_values::Env) -> Option<(&crate::ir::env_values::Env, u32)> {
    match env {
        crate::ir::env_values::Env::InSlot(inner, slot) => Some((inner, *slot)),
        crate::ir::env_values::Env::Up(inner, _) => parked_base(inner),
        _ => None,
    }
}

// Whether each planned level, read the way everything downstream reads it
// (`slot_owner` over the parent chain), reaches the scope the graph gave it.
fn verify_env_plans(
    ctx: &ClosureContext,
    expected: &BTreeMap<(u32, u32), ScopeRef>,
    census: &mut BTreeMap<String, u32>,
) {
    for (&(f, off), &want) in expected {
        let Some(&level) = ctx.env_plans.get(&f).and_then(|p| p.levels.get(&off)) else {
            continue;
        };
        let class = match ctx.slot_owner(f, level) {
            Some(id) if ctx.scope_ref(id) == want => "verify: reaches its scope",
            Some(id) => {
                log::trace!(target: "slots", "env plan: F{f} @{off} level {level} reaches {:?}, not {want:?}", ctx.scope_ref(id));
                "verify: reaches another scope"
            }
            None => {
                log::trace!(target: "slots", "env plan: F{f} @{off} level {level} reaches nothing, not {want:?}");
                "verify: reaches nothing"
            }
        };
        *census.entry(class.to_string()).or_default() += 1;
    }
}

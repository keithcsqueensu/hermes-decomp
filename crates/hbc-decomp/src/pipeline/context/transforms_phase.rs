// Transform pipeline stages (inline, async, generator collapse, folding).
use super::super::ir_gen::convert_yields_to_awaits;
use super::async_detection;
use super::generator_wrapper::generator_wrapper_target;
use super::PipelineContext;
use crate::analysis::ClosureContext;
use crate::file::BytecodeFile;
use crate::ir::Statement;
use crate::transforms;
use std::collections::BTreeMap;

// Slot numbers of a wrapper standing for its machine's environment
// (`keep_raw_machines`): above any slot hermesc allocates, below the
// block-level bits.
const STEP_ENV_SLOTS: u32 = 0x00E0_0000;

impl PipelineContext {
    pub(super) fn run_transform_pipeline(
        all_ir: &mut BTreeMap<u32, Vec<Statement>>,
        closure_ctx: &mut Option<crate::analysis::ClosureContext>,
        global_analysis: &mut crate::analysis::GlobalAnalysis,
        file: &BytecodeFile,
        factories: &BTreeMap<u32, u32>,
    ) {
        // STAGE W12: Strip meaningless Hermes `this` from Call expressions
        for stmts in all_ir.values_mut() {
            transforms::strip_hermes_this(stmts);
        }

        crate::analysis::closure::stamps::report("after W12", all_ir, closure_ctx.as_ref());

        // STAGE W13: Inline single-use temporaries (tmp*, closure_*, rN), parallel.
        // A name a nested function reads stays bound here whatever this body
        // does with it.
        let t = std::time::Instant::now();
        let empty_parents = BTreeMap::new();
        let captured = transforms::names_used_by_descendants(
            all_ir,
            closure_ctx
                .as_ref()
                .map(|c| &c.parent_function)
                .unwrap_or(&empty_parents),
        );
        {
            use rayon::prelude::*;
            let keys: Vec<u32> = all_ir.keys().copied().collect();
            let mut entries: Vec<(u32, Vec<Statement>)> = keys
                .into_iter()
                .filter_map(|id| all_ir.remove(&id).map(|s| (id, s)))
                .collect();
            let none = std::collections::HashSet::new();
            entries.par_iter_mut().for_each(|(id, stmts)| {
                let old = std::mem::take(stmts);
                let keep = captured.get(id).unwrap_or(&none);
                *stmts = transforms::inline_named_variables_keeping(old, keep);
            });
            for (id, stmts) in entries {
                all_ir.insert(id, stmts);
            }
        }
        log::debug!("[pipeline] variable inlining: {:.2?}", t.elapsed());

        crate::analysis::closure::stamps::report("after W13", all_ir, closure_ctx.as_ref());

        // STAGE W14: Detect async generator patterns (yield → await)
        if let Some(ctx) = closure_ctx.as_mut() {
            let async_gen_ids = async_detection::detect_async_generator_wrappers(all_ir);
            for func_id in &async_gen_ids {
                ctx.mark_async(*func_id);
            }
            if !async_gen_ids.is_empty() {
                for func_id in &async_gen_ids {
                    if let Some(stmts) = all_ir.get_mut(func_id) {
                        let old = std::mem::take(stmts);
                        *stmts = convert_yields_to_awaits(old);
                    }
                }
                log::debug!(
                    "[pipeline] async detection: {} functions converted yield→await",
                    async_gen_ids.len()
                );
            }
        }

        crate::analysis::closure::stamps::report("after W14", all_ir, closure_ctx.as_ref());

        // STAGE W15: Unwrap Babel async wrappers
        let mut async_originals = async_detection::Unwrapped::new();
        if let Some(ctx) = closure_ctx.as_mut() {
            let unwrapped = async_detection::unwrap_async_wrappers(
                all_ir,
                ctx,
                &mut global_analysis.param_names,
                file,
                &mut async_originals,
            );
            if unwrapped > 0 {
                log::debug!("[pipeline] async wrapper unwrap: {unwrapped} functions unwrapped");
            }
        }

        crate::analysis::closure::stamps::report("after W15", all_ir, closure_ctx.as_ref());

        // STAGE W16: Post-IPA transforms (reserved words, object/array folding, arguments simplification)
        Self::apply_post_ipa_transforms(all_ir);

        // NOTE: do NOT run promote_const_bindings here. Env slots are shared
        // across closures; a binding that looks unreassigned in one body is often
        // mutated in a sibling. Promotion caused const-reassign / TDZ parse fails.

        crate::analysis::closure::stamps::report("after W16", all_ir, closure_ctx.as_ref());

        // STAGE W16a2: while(true)+trailing break → do/while (after inlining cleans latch)
        {
            use rayon::prelude::*;
            let keys: Vec<u32> = all_ir.keys().copied().collect();
            let mut entries: Vec<(u32, Vec<Statement>)> = keys
                .into_iter()
                .filter_map(|id| all_ir.remove(&id).map(|s| (id, s)))
                .collect();
            entries.par_iter_mut().for_each(|(_, stmts)| {
                let old = std::mem::take(stmts);
                *stmts = transforms::convert_while_true_loops(old);
                let old = std::mem::take(stmts);
                *stmts = transforms::fold_guarded_loops(old);
            });
            for (id, stmts) in entries {
                all_ir.insert(id, stmts);
            }
        }

        crate::analysis::closure::stamps::report("after W16a2", all_ir, closure_ctx.as_ref());

        // STAGE W16a3: second JSX pass after inlining (props often still variables before)
        {
            use rayon::prelude::*;
            let keys: Vec<u32> = all_ir.keys().copied().collect();
            let mut entries: Vec<(u32, Vec<Statement>)> = keys
                .into_iter()
                .filter_map(|id| all_ir.remove(&id).map(|s| (id, s)))
                .collect();
            entries.par_iter_mut().for_each(|(_, stmts)| {
                let old = std::mem::take(stmts);
                *stmts = transforms::reconstruct_jsx(old);
            });
            for (id, stmts) in entries {
                all_ir.insert(id, stmts);
            }
        }

        crate::analysis::closure::stamps::report("after W16a3", all_ir, closure_ctx.as_ref());

        // STAGE W16b: Collapse generator wrappers. A `function* gen()` compiles to
        // a thin wrapper that does `CreateGenerator(body); return it`, with the
        // actual state machine (the yields) in a separate inner function. Inline
        // the inner body into the wrapper so we emit `function* gen() { yield ... }`
        // instead of `function* gen() { return function*() { yield ... } }`.
        if let Some(ctx) = closure_ctx.as_mut() {
            Self::collapse_generator_wrappers(
                all_ir,
                ctx,
                &mut global_analysis.param_names,
                &async_originals,
            );
        }

        crate::analysis::closure::stamps::report("after W16b", all_ir, closure_ctx.as_ref());

        // STAGE W16f: object-key names (`{ login: closure_1_0 }`) are often only
        // visible after slot-fill folding and v98 generator reconstruct. Re-run
        // closure naming so a captured param stored under a property key is named
        // from that key (ground truth), including the parent parameter.
        if let Some(ctx) = closure_ctx.as_mut() {
            let renamed = transforms::rename_closure_variables_cross_function(
                all_ir,
                ctx,
                &mut global_analysis.param_names,
            );
            let inherited = transforms::inherit_ancestor_closure_names(
                all_ir,
                ctx,
                &global_analysis.param_names,
            );
            if renamed > 0 || inherited > 0 {
                log::debug!(
                    "[pipeline] post-reconstruct object-key naming: {renamed} closures, {inherited} inherited"
                );
            }
        }

        crate::analysis::closure::stamps::report("after W16f", all_ir, closure_ctx.as_ref());

        // STAGE W16g: hermesc creates a non-escaping function declaration afresh at every
        // call site. Give each such function one declaration in the scope the
        // sites share, so the text of a module grows with its source and not
        // with its call sites.
        {
            let empty = BTreeMap::new();
            let parent_of = closure_ctx
                .as_ref()
                .map(|c| &c.parent_function)
                .unwrap_or(&empty);
            let hoisted = transforms::hoist_repeated_closures(all_ir, parent_of, factories);
            log::debug!("[pipeline] hoisted {hoisted} repeated closures");
        }
        crate::analysis::closure::stamps::report("after W16g", all_ir, closure_ctx.as_ref());

        Self::separate_step_env_names(all_ir, &global_analysis.param_names);
    }

    // The wrapper statements that must survive its body being replaced: the
    // stores that hand a parameter, or the receiver, to an environment slot
    // (an async method's `self = this`). Everything else a
    // wrapper does is bookkeeping (zeroing the status and label slots) or the
    // return of the generator object itself, both of which the replacement
    // supersedes.
    fn parameter_captures(body: &[Statement]) -> Vec<Statement> {
        use crate::ir::{AssignTarget, Expression, Value};
        body.iter()
            .filter(|stmt| {
                matches!(
                    stmt,
                    Statement::Assign {
                        target: AssignTarget::Binding(_),
                        value: Expression::Value(Value::Parameter(_) | Value::This),
                    }
                )
            })
            .cloned()
            .collect()
    }

    // Whether `body` ever reads `binding`. A capture the replacement body never
    // looks at is not a link between the two, it is a store to a name the output
    // does not otherwise mention, so keeping it would add the very kind of line
    // this change exists to remove.
    fn body_reads_binding(body: &[Statement], binding: &crate::ir::Binding) -> bool {
        use crate::ir::{Expression, Value, Visitor};
        struct Find<'a> {
            want: &'a crate::ir::Binding,
            found: bool,
        }
        impl<'a, 'b> Visitor<'b> for Find<'a> {
            fn visit_expression(&mut self, expr: &'b Expression) {
                if let Expression::Value(Value::Binding(b)) = expr {
                    if b == self.want {
                        self.found = true;
                    }
                }
                self.walk_expression(expr);
            }
        }
        let mut find = Find {
            want: binding,
            found: false,
        };
        for stmt in body {
            find.visit_statement(stmt);
        }
        find.found
    }

    // Drop a store whose source is a name this body never writes and never
    // mentions again.
    //
    // A generator carries its scope in an environment, and capturing one reads
    // `StoreToEnvironment parent, slot, thisEnv`. The environment creation itself
    // has no JavaScript form and emits nothing, so the register naming it is never
    // defined and the store came out as `closure_0 = tmp2`, which stops the module
    // at that line. Suppressing the store in the builder was tried and removed 64
    // string literals and 629 property accesses from the reference bundle: later
    // passes read it. Removing it here, once the body is rebuilt and self
    // contained, is the narrow form.
    //
    // The condition is structural rather than by name, and it has to hold at both
    // ends. The source is never assigned in this body and is mentioned nowhere
    // else in it, and the target is never read in it either. Testing only the
    // source deleted the resume value of an await, because a parameter is written
    // by the call and not by a statement: `closure_130_3 = value` looked like a
    // read of nothing and was in fact the blob id the next line returns.
    fn drop_stores_from_undefined_sources(body: Vec<Statement>) -> Vec<Statement> {
        use crate::ir::{AssignTarget, Binding, Expression, Value, Visitor};
        use std::collections::{HashMap, HashSet};

        struct Scan {
            assigned: HashSet<Binding>,
            reads: HashMap<Binding, usize>,
        }
        impl<'b> Visitor<'b> for Scan {
            fn visit_assign_target(&mut self, t: &'b AssignTarget) {
                if let AssignTarget::Binding(b) = t {
                    self.assigned.insert(b.clone());
                }
                self.walk_assign_target(t);
            }
            fn visit_statement(&mut self, st: &'b Statement) {
                if let Statement::Let { name, .. } = st {
                    self.assigned.insert(Binding::Variable(name.clone()));
                }
                self.walk_statement(st);
            }
            fn visit_expression(&mut self, e: &'b Expression) {
                if let Expression::Value(Value::Binding(b)) = e {
                    *self.reads.entry(b.clone()).or_insert(0) += 1;
                }
                self.walk_expression(e);
            }
        }

        let mut scan = Scan {
            assigned: HashSet::new(),
            reads: HashMap::new(),
        };
        for st in &body {
            scan.visit_statement(st);
        }

        let doomed = |st: &Statement| -> bool {
            let Statement::Assign {
                target: AssignTarget::Binding(dst),
                value: Expression::Value(Value::Binding(src)),
            } = st
            else {
                return false;
            };
            let source_is_undefined =
                !scan.assigned.contains(src) && scan.reads.get(src).copied().unwrap_or(0) == 1;
            let target_is_unread = scan.reads.get(dst).copied().unwrap_or(0) == 0;
            source_is_undefined && target_is_unread
        };

        fn strip(body: Vec<Statement>, doomed: &impl Fn(&Statement) -> bool) -> Vec<Statement> {
            body.into_iter()
                .filter(|st| !doomed(st))
                .map(|st| crate::ir::map_nested_bodies(st, |inner| strip(inner, doomed)))
                .collect()
        }
        strip(body, &doomed)
    }

    // A machine that does not lift is printed as what the bytecode does: its
    // wrapper hands the step function to `CreateGenerator`, whose `next`,
    // `throw` and `return` call it with 0, 1 and 2 and return its `{ value,
    // done }` as is. So the creation reads `HermesBuiltin.createGenerator(step)`
    // with `step` a plain function, the way an unlowered async body reads
    // `HermesBuiltin.spawnAsync(...)`; as `(function* step() {...})()` it was a
    // real generator that returned the step's result object once. A function
    // W15 unwrapped into the wrapper's body gets its own back and is not
    // `async`: its `spawnAsync` call is what drives the machine.
    fn keep_raw_machines(
        all_ir: &mut BTreeMap<u32, Vec<Statement>>,
        closure_ctx: &mut ClosureContext,
        param_names: &mut BTreeMap<u32, Vec<Option<String>>>,
        async_originals: &async_detection::Unwrapped,
        raw: &[(u32, u32)],
    ) {
        use crate::ir::{AssignTarget, Binding, Expression, MutVisitor, SlotRef, Value};
        struct Restamp<'m>(&'m BTreeMap<SlotRef, SlotRef>);
        impl Restamp<'_> {
            fn var(&self, v: &mut crate::ir::Var) {
                if let Some(to) = v.slot.and_then(|s| self.0.get(&s)) {
                    v.slot = Some(*to);
                }
            }
        }
        impl MutVisitor for Restamp<'_> {
            fn visit_statement(&mut self, s: &mut Statement) {
                if let Statement::Let { name, .. } = s {
                    self.var(name);
                }
                self.walk_statement(s);
            }
            fn visit_expression(&mut self, e: &mut Expression) {
                if let Expression::Value(Value::Binding(Binding::Variable(v))) = e {
                    self.var(v);
                }
                self.walk_expression(e);
            }
            fn visit_assign_target(&mut self, t: &mut AssignTarget) {
                if let AssignTarget::Binding(Binding::Variable(v)) = t {
                    self.var(v);
                }
                self.walk_assign_target(t);
            }
        }
        struct Create(u32);
        impl MutVisitor for Create {
            fn visit_expression(&mut self, e: &mut Expression) {
                if let Expression::Call { callee, arguments } = e {
                    let plain_args = arguments.iter().all(|a| {
                        matches!(
                            a,
                            Expression::Value(
                                Value::Constant(crate::ir::Constant::Undefined) | Value::This
                            )
                        )
                    });
                    if let Expression::Function {
                        id,
                        is_generator: true,
                        ..
                    } = callee.as_ref()
                    {
                        if id.0 == self.0 && plain_args {
                            let mut step = callee.as_ref().clone();
                            if let Expression::Function {
                                is_generator,
                                is_async,
                                ..
                            } = &mut step
                            {
                                *is_generator = false;
                                *is_async = false;
                            }
                            *e = Expression::Call {
                                callee: Box::new(Expression::member(
                                    Expression::Value(Value::Binding(
                                        crate::ir::Binding::Variable("HermesBuiltin".into()),
                                    )),
                                    "createGenerator",
                                )),
                                arguments: vec![step],
                            };
                            return;
                        }
                    }
                }
                self.walk_expression(e);
            }
        }
        for &(fid, inner) in raw {
            // The environment the machine builds on its first step is kept in
            // the wrapper's and lives as long as the generator: its slots (a
            // loop counter across yields) are the wrapper's variables, which
            // the step reads; declared in the step they restarted each call.
            // Its later steps reach that environment through the wrapper's
            // slot, a load the stamps cannot follow, so those references are
            // plain names, and the scope hangs below the step, never above
            // it. Each slot the step uses becomes a variable of the wrapper,
            // the step's parent: its references are stamped with a slot of
            // the wrapper's past any real one, which the wrapper binds, so the
            // step takes the name for the wrapper's and does not declare it.
            let moved = closure_ctx.rehome_block_scopes(inner, fid);
            let mut bound: BTreeMap<crate::ir::SlotRef, String> = BTreeMap::new();
            let mut restamp: BTreeMap<crate::ir::SlotRef, crate::ir::SlotRef> = BTreeMap::new();
            if let Some(step) = all_ir.get(&inner) {
                for (name, slot) in crate::analysis::closure::stamps::var_refs(step) {
                    if let Some(slot) = slot.filter(|s| moved.contains(&s.owner)) {
                        let scope = moved.iter().position(|m| *m == slot.owner).unwrap_or(0) as u32;
                        let held = crate::ir::SlotRef {
                            owner: fid,
                            slot: STEP_ENV_SLOTS | scope << 16 | (slot.slot & 0xFFFF),
                        };
                        restamp.insert(slot, held);
                        bound.entry(held).or_insert_with(|| name.to_string());
                    }
                }
            }
            if let Some(step) = all_ir.get_mut(&inner) {
                Restamp(&restamp).visit_statement_list(step);
            }
            if let Some(body) = all_ir.get_mut(&fid) {
                let lets = bound.into_iter().map(|(slot, name)| Statement::Let {
                    name: crate::ir::Var {
                        name,
                        slot: Some(slot),
                    },
                    value: Expression::Value(Value::Constant(crate::ir::Constant::Undefined)),
                    kind: crate::ir::VarKind::Let,
                });
                body.splice(0..0, lets);
            }
            closure_ctx.generator_functions.remove(&inner);
            closure_ctx.async_functions.remove(&inner);
            closure_ctx.async_functions.remove(&fid);
            if let Some((own, params)) = async_originals.get(&fid) {
                all_ir.insert(fid, own.clone());
                match params {
                    Some(p) => param_names.insert(fid, p.clone()),
                    None => param_names.remove(&fid),
                };
                continue;
            }
            if let Some(body) = all_ir.get_mut(&fid) {
                Create(inner).visit_statement_list(body);
            }
        }
    }

    // A raw machine's environment variable (`keep_raw_machines`) is named by
    // the passes after it like any slot, and the name can be its wrapper's
    // parameter's: the machine copies the parameter in, and both take the
    // name the code gives the value (`captureDetails`), or a slot of the
    // wrapper's own or another machine slot can share it. Declared in the
    // wrapper as well, it is a second `let` of the name, a syntax error (all
    // but one of Discord's 49 unparsable modules). Such a variable takes a
    // fresh name, everywhere its stamp is.
    fn separate_step_env_names(
        all_ir: &mut BTreeMap<u32, Vec<Statement>>,
        param_names: &BTreeMap<u32, Vec<Option<String>>>,
    ) {
        use crate::ir::{AssignTarget, Binding, Expression, MutVisitor, SlotRef, Value, Var};
        let mut renames: BTreeMap<SlotRef, String> = BTreeMap::new();
        let none = Vec::new();
        for (&fid, body) in all_ir.iter() {
            let is_step_env = |slot: Option<SlotRef>| {
                slot.filter(|s| s.owner == fid && s.slot & STEP_ENV_SLOTS == STEP_ENV_SLOTS)
            };
            if !body.iter().any(
                |st| matches!(st, Statement::Let { name, .. } if is_step_env(name.slot).is_some()),
            ) {
                continue;
            }
            let params: std::collections::HashSet<&str> = param_names
                .get(&fid)
                .unwrap_or(&none)
                .iter()
                .flatten()
                .map(String::as_str)
                .collect();
            let refs = crate::analysis::closure::stamps::var_refs(body);
            let mut used: std::collections::HashSet<String> =
                refs.iter().map(|(n, _)| n.to_string()).collect();
            used.extend(params.iter().map(|p| p.to_string()));
            let mut kept: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for stmt in body {
                let Statement::Let { name, .. } = stmt else {
                    continue;
                };
                let Some(slot) = is_step_env(name.slot) else {
                    continue;
                };
                // Another variable of the wrapper under this name: a
                // parameter, a slot of its own (`userId = 0`, declared by
                // codegen later), or another machine slot already named so.
                let clash = params.contains(name.name.as_str())
                    || kept.contains(name.name.as_str())
                    || refs
                        .iter()
                        .any(|(n, s)| *n == name.name.as_str() && *s != Some(slot));
                if !clash {
                    kept.insert(name.name.as_str());
                    continue;
                }
                let mut n = 2;
                let fresh = loop {
                    let candidate = format!("{}{n}", name.name);
                    if !used.contains(&candidate) {
                        break candidate;
                    }
                    n += 1;
                };
                used.insert(fresh.clone());
                renames.insert(slot, fresh);
            }
        }
        if renames.is_empty() {
            return;
        }
        struct Rename<'r>(&'r BTreeMap<SlotRef, String>);
        impl Rename<'_> {
            fn var(&self, v: &mut Var) {
                if let Some(n) = v.slot.and_then(|s| self.0.get(&s)) {
                    v.name = n.clone();
                }
            }
        }
        impl MutVisitor for Rename<'_> {
            fn visit_statement(&mut self, s: &mut Statement) {
                if let Statement::Let { name, .. } = s {
                    self.var(name);
                }
                self.walk_statement(s);
            }
            fn visit_expression(&mut self, e: &mut Expression) {
                if let Expression::Value(Value::Binding(Binding::Variable(v))) = e {
                    self.var(v);
                }
                self.walk_expression(e);
            }
            fn visit_assign_target(&mut self, t: &mut AssignTarget) {
                if let AssignTarget::Binding(Binding::Variable(v)) = t {
                    self.var(v);
                }
                self.walk_assign_target(t);
            }
        }
        let mut r = Rename(&renames);
        for body in all_ir.values_mut() {
            r.visit_statement_list(body);
        }
    }

    // See STAGE W16b. Replace each generator wrapper's body with the inner
    // generator body it merely creates and returns.
    pub(super) fn collapse_generator_wrappers(
        all_ir: &mut BTreeMap<u32, Vec<Statement>>,
        closure_ctx: &mut ClosureContext,
        param_names: &mut BTreeMap<u32, Vec<Option<String>>>,
        async_originals: &async_detection::Unwrapped,
    ) {
        // Names each function's nested functions still read: never dropped as
        // dead by the cleanups below.
        let captured = transforms::names_used_by_descendants(all_ir, &closure_ctx.parent_function);
        // A wrapper is any function whose body merely returns a generator object
        // created via CreateGenerator (`return (function*(){...})()` or bare
        // `return function*(){...}`, after env-slot init). The wrapper itself is
        // often a plain CreateClosure (not CreateGeneratorClosure); detecting by
        // shape, not by the is_generator flag on the wrapper, is required.
        // `generator_wrapper_target` only matches Function{is_generator:true}, so
        // the inner is a generator even if analysis missed marking it earlier.
        let mut replacements: Vec<(u32, u32, Vec<Statement>)> = Vec::new();
        let mut raw: Vec<(u32, u32)> = Vec::new();
        for (&fid, body) in all_ir.iter() {
            if let Some(inner) = generator_wrapper_target(body) {
                // A v98 machine that will not lift stays apart from its
                // wrapper. Its status and label live in the wrapper's
                // environment, kept from one call of the machine to the next;
                // moved into the wrapper they became locals the machine
                // declared afresh on every call (`let c0; if (c0 === 2)`),
                // 1 750 of them on Discord.
                let stays_raw = all_ir.get(&inner).is_some_and(|b| {
                    transforms::is_generator_state_machine_v98(b)
                        && transforms::try_reconstruct_generator_v98(b).is_none()
                });
                if inner != fid && stays_raw {
                    raw.push((fid, inner));
                } else if inner != fid && all_ir.contains_key(&inner) {
                    replacements.push((fid, inner, Self::parameter_captures(body)));
                }
            }
        }
        // A factory that returns a generator *function* (`function make() {
        // return gen; }` with `function* gen`) has the same shape as a wrapper
        // that returns a generator object, but its inner is itself a wrapper.
        // Collapsing both moved `gen`'s wrapper body into `make` and then
        // removed `gen`, so the state machine was lost. Only the innermost link
        // of a chain is a real wrapper.
        let wrappers: std::collections::HashSet<u32> =
            replacements.iter().map(|(fid, _, _)| *fid).collect();
        replacements.retain(|(_, inner, _)| !wrappers.contains(inner));
        Self::keep_raw_machines(all_ir, closure_ctx, param_names, async_originals, &raw);
        // Parameter captures held back from each collapsed wrapper, re-attached
        // once the state machine has been lifted.
        let mut wrapper_captures: BTreeMap<u32, Vec<Statement>> = BTreeMap::new();
        for (fid, inner, captures) in replacements {
            if let Some(inner_body) = all_ir.get(&inner).cloned() {
                // The wrapper's body goes away, but the stores that put its
                // parameters into the environment the generator reads are the only
                // link between the two. Dropping them left the body awaiting a slot
                // nothing ever wrote (`Promise.resolve(closure_0)` for a parameter
                // named `x`), which is a lost argument, not just a lost name.
                // Keep only what the replacement body actually reads. The
                // reconstruction below rebuilds the body from the state machine
                // dispatch, so these are re-attached there rather than here.
                let captures: Vec<Statement> = captures
                    .into_iter()
                    .filter(|stmt| match stmt {
                        Statement::Assign {
                            target: crate::ir::AssignTarget::Binding(b),
                            ..
                        } => Self::body_reads_binding(&inner_body, b),
                        _ => false,
                    })
                    .collect();
                if !captures.is_empty() {
                    wrapper_captures.insert(fid, captures);
                }
                all_ir.insert(fid, inner_body);
                // Both ends are generators: wrapper becomes the callable function*,
                // inner was the CreateGenerator body (state machine / yields).
                closure_ctx.mark_generator(fid);
                closure_ctx.mark_generator(inner);
                // The inner body now lives in the wrapper; drop the standalone copy
                // so it is not also emitted as an orphan function.
                all_ir.remove(&inner);
                closure_ctx.rehome_block_scopes(inner, fid);
            }
        }
        // The wrapper collapse above just marked new generators, and W14 marked the
        // wrappers async. Propagate now so a generator whose parent is async is
        // known to be async BEFORE the reconstruction loop below, which is what
        // decides whether its `yield`s become `await`s.
        closure_ctx.propagate_async_to_generators();

        // STAGE W16c: Reconstruct HBC >=97 generator state machines into flat
        // `yield` bodies. v97 removed the generator opcodes; `function*` is now a
        // desugared switch over status/label env slots. The recognizer is
        // conservative, it returns the body unchanged on any shape mismatch.
        let gen_ids: Vec<u32> = all_ir
            .keys()
            .copied()
            .filter(|fid| closure_ctx.is_generator(*fid))
            .collect();
        for fid in gen_ids {
            if let Some(body) = all_ir.remove(&fid) {
                // A wrapper that was collapsed into this body held the stores
                // that hand its parameters to the environment the machine reads.
                // They go back on the front here, after the lift, because the
                // lift rebuilds the body from the dispatch and would drop
                // anything sitting in front of it.
                let captures = wrapper_captures.remove(&fid).unwrap_or_default();
                let prepend = |body: Vec<Statement>| -> Vec<Statement> {
                    if captures.is_empty() {
                        return body;
                    }
                    let mut merged = captures.clone();
                    merged.extend(body);
                    merged
                };
                let Some(lifted) = transforms::try_reconstruct_generator_v98(&body) else {
                    // The machine did not lift, so it stays exactly as decoded.
                    // The cleanup below reads data flow to decide what is dead,
                    // and in a raw resume machine the flow runs through the label
                    // and status slots, which those passes cannot follow: they
                    // then delete live code. That is how the header build in
                    // `piloteAuthHeaders`, `Bearer ` and `x-refresh-token`
                    // included, disappeared from the output while still being
                    // present in the per function decompile of the same body.
                    all_ir.insert(fid, prepend(body));
                    continue;
                };
                let mut body = prepend(lifted);
                // Reconstruct runs after the W14 yield→await pass, so a v98
                // machine that just became `yield` still needs the async rewrite.
                if closure_ctx.is_async(fid) {
                    body = convert_yields_to_awaits(body);
                }
                // Flat body: inline `obj2 = {login}; obj1.body = obj2` then
                // fold placeholder members into the literal, then drop the
                // leftover state-machine temps (`c1 = tmp3`, `dependencyMap = 0`).
                let none = std::collections::HashSet::new();
                let keep = captured.get(&fid).unwrap_or(&none);
                body = transforms::inline_named_variables_keeping(body, keep);
                transforms::fold_slot_index_fills(&mut body);
                body = transforms::inline_named_variables_keeping(body, keep);
                transforms::fold_slot_index_fills(&mut body);
                body = transforms::remove_dead_temp_bindings_keeping(body, keep);
                body = transforms::eliminate_dead_stores(body);
                body = drop_unread_bookkeeping(body);
                body = Self::drop_stores_from_undefined_sources(body);
                body = drop_duplicate_bare_requires(body);
                all_ir.insert(fid, body);
            }
        }

        // W14 ran before reconstruct, so v98 machines had no `yield` yet and
        // detect may have missed `asyncGeneratorStep(undefined, function*(){})`.
        // Re-scan and convert now that bodies are flat.
        let async_ids = async_detection::detect_async_generator_wrappers(all_ir);
        for func_id in &async_ids {
            closure_ctx.mark_async(*func_id);
        }
        // Every async function renders as `async function`, never `async
        // function*` (Hermes has no async generators; Babel lowers them), so a
        // `yield` left in one does not parse. Bodies reach an async function by
        // several routes: W14's detection, the wrapper unwrap, the generator
        // wrapper collapse above, each converting only what it recognised. A
        // v96 wrapper around a helper imported without a name
        // (`_mod47.default(function*(){})`) slipped through all three.
        let async_fids: Vec<u32> = all_ir
            .keys()
            .copied()
            .filter(|&f| closure_ctx.is_async(f))
            .collect();
        for func_id in async_fids {
            if let Some(body) = all_ir.remove(&func_id) {
                all_ir.insert(func_id, convert_yields_to_awaits(body));
            }
        }
        async_detection::strip_redundant_async_helpers(all_ir, closure_ctx);

        // STAGE W16d: Reconstruct HBC >=97 array destructuring from the flat
        // iterator protocol (after the cleanup-handler skip un-nests it). The
        // matcher is conservative, it only rewrites a recognized `iter =
        // src[Symbol.iterator](); ...advances/binds...; iter.return()` block.
        let fids: Vec<u32> = all_ir.keys().copied().collect();
        for fid in &fids {
            if let Some(body) = all_ir.remove(fid) {
                // Two lowerings reach here. Hermes emits the flat iterator protocol
                // for source that still had `[a, b] = src`, while Babel had already
                // rewritten its own inputs into a runtime helper call. A bundle
                // built through Babel carries both.
                let body = transforms::reconstruct_v98_array_destructuring(body);
                let body = transforms::reconstruct_babel_array_destructuring(body);
                // Short circuit folding also runs before naming, where the pattern
                // is still spelled in registers. Naming and the later transforms
                // introduce it again on named bindings, so it runs a second time
                // here, once every body is in its final form.
                all_ir.insert(*fid, transforms::detect_short_circuit_logic(body));
            }
        }

        // STAGE W16e: JSX reconstruction on the fully-assembled, named IR. The
        // in-pipeline pass (F10) runs before object-literal reconstruction, so it
        // misses calls whose props object is materialized later; rerun here where
        // `jsx(Tag, {props, children})` is complete.
        for fid in &fids {
            if let Some(body) = all_ir.remove(fid) {
                all_ir.insert(*fid, transforms::reconstruct_jsx(body));
            }
        }

        // Single-use `tmp = fn()` / `tmp = x.prop` created by the passes above
        // are still inlinable. Same rule as the earlier pass: one definition,
        // one use. No name is invented.
        {
            use rayon::prelude::*;
            let keys: Vec<u32> = all_ir.keys().copied().collect();
            let mut entries: Vec<(u32, Vec<Statement>)> = keys
                .into_iter()
                .filter_map(|id| all_ir.remove(&id).map(|s| (id, s)))
                .collect();
            let none = std::collections::HashSet::new();
            entries.par_iter_mut().for_each(|(id, stmts)| {
                let old = std::mem::take(stmts);
                let keep = captured.get(id).unwrap_or(&none);
                *stmts = transforms::inline_named_variables_keeping(old, keep);
            });
            for (id, stmts) in entries {
                all_ir.insert(id, stmts);
            }
        }

        // After inlining, a discriminant copy can land on the scrutinee
        // (`kind = kind.kind`) and the later `kind.voiceState` reads the string.
        for stmts in all_ir.values_mut() {
            let old = std::mem::take(stmts);
            *stmts = transforms::repair_switch_clobbers(old);
        }
    }

    pub(super) fn apply_post_ipa_transforms(all_ir: &mut BTreeMap<u32, Vec<Statement>>) {
        // Rename reserved JS keywords used as variable names (default → _default)
        for stmts in all_ir.values_mut() {
            transforms::rename_reserved_words(stmts);
        }

        // Fold incremental object/array construction into literals
        for stmts in all_ir.values_mut() {
            transforms::fold_slot_index_fills(stmts);
            let old = std::mem::take(stmts);
            *stmts = transforms::fold_object_literals(old);
            let old = std::mem::take(stmts);
            *stmts = transforms::fold_array_literals(old);
        }

        // Simplify Babel arguments-to-array copy pattern
        for stmts in all_ir.values_mut() {
            let old = std::mem::take(stmts);
            *stmts = transforms::simplify_arguments_copy(old);
        }
    }
}

// Drop leftover state-machine bookkeeping that reconstruct no longer reads
// (`c1 = tmp3`, `dependencyMap = 0`). Only unread trivial copies/scalars.
fn drop_unread_bookkeeping(stmts: Vec<crate::ir::Statement>) -> Vec<crate::ir::Statement> {
    use crate::ir::{AssignTarget, Expression, Statement, Value, Visitor};
    use std::collections::HashMap;

    struct Reads<'a>(&'a mut HashMap<String, u32>);
    impl Visitor<'_> for Reads<'_> {
        fn visit_expression(&mut self, e: &Expression) {
            if let Expression::Value(Value::Binding(crate::ir::Binding::Variable(n))) = e {
                *self.0.entry(n.name.clone()).or_insert(0) += 1;
            }
            self.walk_expression(e);
        }
    }
    let mut reads = HashMap::new();
    {
        let mut c = Reads(&mut reads);
        for s in &stmts {
            c.visit_statement(s);
        }
    }
    stmts
        .into_iter()
        .filter(|stmt| match stmt {
            Statement::Let { name, value, .. }
            | Statement::Assign {
                target: AssignTarget::Binding(crate::ir::Binding::Variable(name)),
                value,
            } => {
                if !is_bookkeeping_name(name) {
                    return true;
                }
                if reads.get(name.as_str()).copied().unwrap_or(0) != 0 {
                    return true;
                }
                !is_trivial_bookkeeping_value(value)
            }
            _ => true,
        })
        .collect()
}

fn is_bookkeeping_name(name: &str) -> bool {
    if name == "dependencyMap" || name == "_dependencyMap" {
        return true;
    }
    if name == "tmp"
        || name
            .strip_prefix("tmp")
            .is_some_and(|r| r.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    let mut ch = name.chars();
    matches!(
        (ch.next(), ch.as_str()),
        (Some('c' | 'v'), rest) if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
    )
}

fn drop_duplicate_bare_requires(stmts: Vec<crate::ir::Statement>) -> Vec<crate::ir::Statement> {
    use crate::ir::{Expression, Statement};
    fn require_key(e: &Expression) -> Option<String> {
        let Expression::Call { arguments, .. } = e else {
            return None;
        };
        let args = if arguments.len() >= 2
            && matches!(
                &arguments[0],
                Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::Undefined))
            ) {
            &arguments[1..]
        } else {
            arguments.as_slice()
        };
        match args.first() {
            Some(Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::Integer(
                id,
            )))) => Some(format!("i{id}")),
            Some(Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::String(s)))) => {
                Some(format!("s{s}"))
            }
            _ => None,
        }
    }
    use crate::ir::Visitor;
    struct Keys<'a>(&'a mut std::collections::HashSet<String>, bool);
    impl Visitor<'_> for Keys<'_> {
        fn visit_expression(&mut self, e: &Expression) {
            if let Some(k) = require_key(e) {
                if self.1 {
                    self.0.insert(k);
                }
            }
            self.walk_expression(e);
        }
    }
    let mut used = std::collections::HashSet::new();
    {
        let mut c = Keys(&mut used, true);
        for s in &stmts {
            if !matches!(s, Statement::Expr(e) if require_key(e).is_some()) {
                c.visit_statement(s);
            }
        }
    }
    stmts
        .into_iter()
        .filter(|s| match s {
            Statement::Expr(e) => require_key(e).is_none_or(|k| !used.contains(&k)),
            _ => true,
        })
        .collect()
}

fn is_trivial_bookkeeping_value(e: &crate::ir::Expression) -> bool {
    use crate::ir::{Constant, Expression, Value};
    matches!(
        e,
        Expression::Value(Value::Binding(crate::ir::Binding::Variable(_)))
            | Expression::Value(Value::Binding(crate::ir::Binding::Register(_)))
            | Expression::Value(Value::Parameter(_))
            | Expression::Value(Value::Constant(
                Constant::Integer(_) | Constant::Null | Constant::Undefined | Constant::Bool(_)
            ))
    )
}

#[cfg(test)]
mod generator_wrapper_tests {
    use super::PipelineContext;
    use crate::ir::{AssignTarget, Binding, Constant, Expression, Statement, Value};

    fn assign(target: &str, value: Expression) -> Statement {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(target.into())),
            value,
        }
    }

    fn read(name: &str) -> Expression {
        Expression::Value(Value::Binding(Binding::Variable(name.into())))
    }

    fn drop_stores(body: Vec<Statement>) -> Vec<Statement> {
        PipelineContext::drop_stores_from_undefined_sources(body)
    }

    // A generator captures its scope with a store of a freshly created
    // environment. The creation has no JavaScript form and emits nothing, so the
    // register naming it is never defined and the store reads a name that does not
    // exist. Nothing else in the body mentions either side.
    #[test]
    fn a_store_from_a_name_nothing_defines_is_dropped() {
        let body = vec![
            assign("closure_0", read("tmp2")),
            assign("closure_1", Expression::constant(Constant::Integer(1))),
            Statement::Return(Some(read("closure_1"))),
        ];
        let out = drop_stores(body);
        assert_eq!(out.len(), 2, "only the undefined read goes: {out:?}");
        assert!(
            matches!(&out[0], Statement::Assign { target: AssignTarget::Binding(Binding::Variable(n)), .. } if n == "closure_1")
        );
    }

    // The resume value of an await arrives as a parameter, which no statement
    // assigns. Testing only the source made that look like a read of nothing, and
    // deleting it lost the value the next line returns.
    #[test]
    fn a_store_whose_target_is_read_is_kept() {
        let body = vec![
            assign("closure_3", read("value")),
            Statement::Return(Some(read("closure_3"))),
        ];
        let out = drop_stores(body.clone());
        assert_eq!(out, body, "the target is read, so the store carries data");
    }

    #[test]
    fn a_source_mentioned_more_than_once_is_kept() {
        let body = vec![
            assign("closure_0", read("shared")),
            assign("closure_1", read("shared")),
        ];
        let out = drop_stores(body.clone());
        assert_eq!(out, body, "a name used twice is not a one off artefact");
    }

    #[test]
    fn a_store_from_a_name_the_body_assigns_is_kept() {
        let body = vec![
            assign("real", Expression::constant(Constant::Integer(7))),
            assign("closure_0", read("real")),
        ];
        let out = drop_stores(body.clone());
        assert_eq!(out, body, "the source is defined right above");
    }

    #[test]
    fn the_rule_reaches_into_nested_bodies() {
        let body = vec![Statement::If {
            condition: Expression::constant(Constant::Bool(true)),
            then_body: vec![assign("closure_0", read("tmp2"))],
            else_body: vec![],
        }];
        let out = drop_stores(body);
        match &out[0] {
            Statement::If { then_body, .. } => assert!(then_body.is_empty(), "{out:?}"),
            other => panic!("unexpected {other:?}"),
        }
    }

    // What the wrapper hands to the generator is the store of its parameter into
    // the environment the machine reads. It is the only link between the two.
    #[test]
    fn a_parameter_capture_is_picked_out_of_a_wrapper_body() {
        let body = vec![
            assign("closure_0", Expression::Value(Value::Parameter(0))),
            assign("c2", Expression::constant(Constant::Integer(0))),
            Statement::Return(None),
        ];
        let caps = PipelineContext::parameter_captures(&body);
        assert_eq!(caps.len(), 1, "only the parameter store: {caps:?}");
        assert_eq!(caps[0], body[0]);
    }

    #[test]
    fn a_wrapper_with_no_parameter_stores_yields_nothing() {
        let body = vec![
            assign("c2", Expression::constant(Constant::Integer(0))),
            Statement::Return(None),
        ];
        assert!(PipelineContext::parameter_captures(&body).is_empty());
    }

    #[test]
    fn a_capture_the_replacement_body_never_reads_is_not_a_link() {
        let inner = vec![Statement::Return(Some(read("closure_9")))];
        assert!(!PipelineContext::body_reads_binding(
            &inner,
            &Binding::Variable("closure_0".into())
        ));
        assert!(PipelineContext::body_reads_binding(
            &inner,
            &Binding::Variable("closure_9".into())
        ));
    }
}

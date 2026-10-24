// Rendering: inline body building and this-usage detection helpers.

mod missing;

// Each pass peels one level of nesting. Five was short of the chains in a
// real bundle. Past the point where `{ ... }` holes stop decreasing, another
// pass only re-embeds a cycle (a function whose body refers to itself through
// another function) and the strings grow without filling anything.
const MAX_INLINE_BODY_PASSES: usize = 64;
// Upper bound on the total rendered text held in memory across all bodies.
// A whole-bundle output is about 130 MB; ten times that is already far past
// what any nesting depth needs, and the machine that runs this has 16 GB.
const MAX_INLINE_BODY_BYTES: usize = 1_200_000_000;
// The largest text one inline body may reach through embedding. A real
// function is far smaller; anything past this is nesting that repeats.
const MAX_INLINE_BODY_CHARS: usize = 32_000_000;

use crate::file::BytecodeFile;
use crate::ir::Statement;
use crate::transforms::{self, Codegen, CodegenOptions};
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use super::super::get_function_params;
use super::PipelineContext;

impl PipelineContext {
    // Build pre-rendered inline function bodies for ALL functions.
    // Multi-pass approach for multi-level nesting support.
    /// The slot identity check (closure model K4.3) over every body as codegen
    /// renders it: nested bodies as `build_all_inline_bodies` prepares them and
    /// module factories as `generate_function_code` does. `None` without a
    /// closure context.
    pub fn check_slot_identity(
        &self,
        file: &BytecodeFile,
    ) -> Option<crate::analysis::closure::identity::Violations> {
        use crate::analysis::closure::identity::{check_slot_identity, RenderedBody};
        let ctx = self.closure_ctx.as_ref()?;
        let prepared = self.prepare_render_bodies(file);
        let factories: BTreeMap<u32, (Vec<String>, Vec<Statement>)> = self
            .registry
            .function_to_module
            .keys()
            .filter_map(|&f| self.prepare_factory_body(file, f).map(|b| (f, b)))
            .collect();
        let bodies = prepared
            .iter()
            .map(|b| (b, false))
            .chain(factories.iter().map(|b| (b, true)))
            .map(|((&f, (params, stmts)), module)| {
                (
                    f,
                    RenderedBody {
                        params,
                        stmts,
                        module,
                    },
                )
            })
            .collect();
        Some(check_slot_identity(&bodies, ctx))
    }

    pub(super) fn build_all_inline_bodies(&mut self, file: &BytecodeFile) {
        // Precompute IPA-renamed + cleaned statements once to avoid cloning per rendering pass
        let prepared = self.prepare_render_bodies(file);
        for (&func, (_, stmts)) in &prepared {
            crate::analysis::closure::stamps::record_render(self.closure_ctx.as_ref(), func, stmts);
        }

        // Reverse dependency map: child function id -> parents that reference it by
        // id (`Expression::Function { id }`). A parent's rendered string only
        // changes between passes when one of the inline bodies it references
        // changed, so later passes re-render only the affected parents instead of
        // every function. This turns the multi-pass cost from passes x all
        // functions into all functions plus a quickly shrinking dirty set.
        let mut referenced_by: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for (&func_id, (_, body_stmts)) in &prepared {
            let mut refs = Vec::new();
            collect_function_refs(body_stmts, &mut refs);
            refs.sort_unstable();
            refs.dedup();
            for child in refs {
                referenced_by.entry(child).or_default().push(func_id);
            }
        }

        // Pass 1: render every function against an empty inline map. Everything
        // produced counts as changed relative to that empty starting point.
        let mut current =
            self.render_prepared_bodies(file, &prepared, &Arc::new(BTreeMap::new()), None);
        let mut changed: Vec<u32> = current.keys().copied().collect();
        let mut holes = count_body_holes(&current);
        log::debug!(
            "[pipeline] inline bodies: {} prepared of {} ir, {} holes after pass 1",
            prepared.len(),
            self.all_ir.len(),
            holes
        );

        for _ in 1..MAX_INLINE_BODY_PASSES {
            // Dirty set: parents that reference a body which changed last pass.
            let mut dirty: BTreeSet<u32> = BTreeSet::new();
            for c in &changed {
                if let Some(parents) = referenced_by.get(c) {
                    dirty.extend(parents.iter().copied());
                }
            }
            if dirty.is_empty() {
                break;
            }

            let shared = Arc::new(std::mem::take(&mut current));
            let updated = self.render_prepared_bodies(file, &prepared, &shared, Some(&dirty));
            current = Arc::try_unwrap(shared).unwrap_or_else(|a| (*a).clone());

            changed.clear();
            let mut capped = 0usize;
            for (id, body) in updated {
                // A body that grew past the cap keeps its previous rendering:
                // a function of that size only exists through embedding, and
                // holding it would let one pass take the whole run with it.
                if body.len() > MAX_INLINE_BODY_CHARS && current.contains_key(&id) {
                    capped += 1;
                    log::debug!(
                        "[pipeline] inline bodies: F{id} kept at its previous size, {} chars over cap",
                        body.len()
                    );
                    continue;
                }
                if current.get(&id).is_none_or(|prev| prev != &body) {
                    changed.push(id);
                }
                current.insert(id, body);
            }
            if capped > 0 {
                log::debug!(
                    "[pipeline] inline bodies: {capped} bodies over the size cap this pass"
                );
            }
            if changed.is_empty() {
                break;
            }
            // Every function referenced by id is embedded as text in its
            // parent, so the size of the map grows with each level of nesting
            // and, where one function is referenced from many sites, with the
            // number of sites. Past a fixed budget the run is no longer
            // rendering, it is copying: stop here and leave the rest as holes.
            let bytes: usize = current.values().map(String::len).sum();
            if bytes > MAX_INLINE_BODY_BYTES {
                log::debug!(
                    "[pipeline] inline bodies: {bytes} bytes after pass, over budget, stopping"
                );
                break;
            }
            holes = count_body_holes(&current);
        }
        log::debug!("[pipeline] inline bodies: {holes} holes after passes");

        // Ids referenced from a rendered body but absent from the map (IR gen
        // skipped them, or they were dropped) stay as `{ ... }`. Build those
        // from bytecode once, then re-render only the parents that name them.
        // A module factory is referenced by the registration in the entry
        // function and is rendered as its module, never inline, so it is not
        // missing. Counting the 17k of them here used up the rebuild budget
        // before a single real gap was looked at.
        let missing = missing::ids_referenced_but_missing(
            &prepared,
            &current,
            &self.registry.function_to_module,
        );
        log::debug!(
            "[pipeline] inline bodies: {} referenced ids have no body",
            missing.len()
        );
        if !missing.is_empty() {
            let filled = self.render_missing_from_bytecode(file, &missing, &current);
            let mut dirty: BTreeSet<u32> = BTreeSet::new();
            for id in filled.keys() {
                if let Some(parents) = referenced_by.get(id) {
                    dirty.extend(parents.iter().copied());
                }
            }
            for (id, body) in filled {
                current.insert(id, body);
            }
            if !dirty.is_empty() {
                let shared = Arc::new(std::mem::take(&mut current));
                let updated = self.render_prepared_bodies(file, &prepared, &shared, Some(&dirty));
                current = Arc::try_unwrap(shared).unwrap_or_else(|a| (*a).clone());
                for (id, body) in updated {
                    // Only keep a re-render that removes holes. A pass that
                    // copies a still-empty body into every caller multiplies them.
                    let old = current
                        .get(&id)
                        .map(|s| s.matches(crate::transforms::BODY_HOLE).count())
                        .unwrap_or(0);
                    if body.matches(crate::transforms::BODY_HOLE).count() <= old {
                        current.insert(id, body);
                    }
                }
            }
        }

        self.inline_bodies = Arc::new(current);
    }

    // The recovered worklet source for `func_id`, looked up by the function's
    // bytecode name (the join key, both the name and the source come from the
    // binary). Returns the source already shaped as a function expression.
    pub(super) fn worklet_source_for(&self, file: &BytecodeFile, func_id: u32) -> Option<String> {
        if self.worklet_sources.is_empty() {
            return None;
        }
        let name = file
            .function_headers
            .get(func_id as usize)
            .and_then(|h| file.string_at(h.function_name()))
            .map(|e| e.value.clone())?;
        let src = self.worklet_sources.get(&name)?;
        // The recovered text is the source of one function; an `export`
        // keyword on it belongs to the module it came from, not to the
        // expression position it is rendered in.
        let src = src
            .trim_start()
            .strip_prefix("export default ")
            .or_else(|| src.trim_start().strip_prefix("export "))
            .unwrap_or(src);
        Some(format!("/* worklet (recovered source) */ {src}"))
    }

    // Precompute IPA-renamed, cleaned, and declaration-inserted statements for all
    // non-factory functions. Each function is prepared independently from read only
    // shared state, so fan the work out across cores.
    // The names of a function's own environment slots, as the closure
    // context resolved them: what its closures read them as.
    pub(super) fn own_env_slot_names(&self, func_id: u32) -> HashSet<String> {
        let Some(ctx) = self.closure_ctx.as_ref() else {
            return HashSet::new();
        };
        let Some(info) = ctx.function_closures.get(&func_id) else {
            return HashSet::new();
        };
        info.slots
            .keys()
            .filter(|k| *k >> 24 == 0)
            .map(|&k| info.get_slot_name(k))
            .collect()
    }

    // Declare a nested (non-factory) body's variables, as codegen renders it:
    // ancestors' env slots stay assignments (only slots: an ancestor's plain
    // local `obj` must not stop a child's own `let obj`), and its own slots are
    // declared by the names its context gives them and, from the stamps, by
    // the names the body uses for them. A name the body uses, by stamp, for an
    // ancestor's slot stays an assignment whatever the context calls the slot.
    pub(super) fn insert_body_declarations(
        &self,
        func_id: u32,
        stmts: &mut Vec<Statement>,
        params: &[String],
    ) {
        let extra = self.extra_writes_for_function(func_id);
        // The names this body stores into an ancestor's slot under, by stamp,
        // where the owner binds them; and the ancestors' slot names (as the
        // context has them) that a function nested here uses, so a local
        // declared here cannot hide the ancestor's variable from it. Any other
        // name an ancestor's slot merely shares is this body's local (each of a
        // module's functions with its own `hasOwnProperty` temporary). Without
        // a closure context, the ancestors' slot names as the context has them.
        let ancestors = self.ancestor_env_slots.get(&func_id);
        let outer: HashSet<String> = match self.closure_ctx.as_ref() {
            Some(ctx) => {
                let mut outer: HashSet<String> =
                    crate::analysis::closure::stamps::captured_slot_names(ctx, func_id, stmts)
                        .into_iter()
                        .filter(|(name, slots)| {
                            slots.iter().all(|s| self.owner_binds(ctx, *s, name))
                        })
                        .map(|(name, _)| name)
                        .collect();
                if let (Some(ancestors), Some(below)) =
                    (ancestors, self.outer_used_by_descendants.get(&func_id))
                {
                    outer.extend(ancestors.intersection(below).cloned());
                }
                outer
            }
            None => ancestors.cloned().unwrap_or_default(),
        };
        let mut own_slots = self.own_env_slot_names(func_id);
        let slot_names = self.slot_names_of(func_id);
        own_slots.extend(slot_names.iter().cloned());
        self.retain_read_slots(func_id, stmts, &mut own_slots);
        // The names the body uses for its own slots: by stamp, and by the name
        // the other bodies give the slot when this body's own reference lost
        // its stamp (moment's `keys = Object.keys`, read by `hooks.locales()`).
        let mut own_used = crate::analysis::closure::stamps::own_slot_names(func_id, stmts);
        if !slot_names.is_empty() {
            let here: HashSet<&str> = crate::analysis::closure::stamps::var_refs(stmts)
                .into_iter()
                .map(|(n, _)| n)
                .collect();
            own_used.extend(
                slot_names
                    .iter()
                    .filter(|n| here.contains(n.as_str()) && !outer.contains(*n))
                    .cloned(),
            );
        }
        let read_by = self.own_slots_read_below(func_id, stmts, &own_used);
        transforms::insert_declarations_with_slots(
            stmts, params, &extra, &outer, &own_slots, &own_used, &read_by,
        );
    }

    // Declare the variables of every class method body in `stmts`, each as the
    // function it is. The class pass rebuilds those bodies from IR and keeps
    // them inside `Statement::Class`, where no declaration pass reached: a
    // constructor's locals (`tmp`, `obj`, destructured options) were bound
    // only by codegen's module-level `let`, which vanishes as soon as any
    // other function in the module declares the same name for itself.
    pub(super) fn declare_class_methods(&self, stmts: &mut Vec<Statement>) {
        use crate::ir::{Expression, MutVisitor};
        struct Methods<'c>(&'c PipelineContext);
        impl MutVisitor for Methods<'_> {
            fn visit_statement(&mut self, s: &mut Statement) {
                if let Statement::Class { methods, .. } = s {
                    for m in methods.iter_mut() {
                        let (Expression::Function { id, .. }, Some(body)) = (&m.value, &mut m.body)
                        else {
                            continue;
                        };
                        self.0.insert_body_declarations(id.0, body, &m.params);
                        self.visit_statement_list(body);
                    }
                }
                self.walk_statement(s);
            }
        }
        Methods(self).visit_statement_list(stmts);
    }

    // Whether the scope that owns `slot` binds it as `name`. A function owner
    // declares every name any body gives its slot (`slot_names_of`), so a store
    // under any of them reaches the owner's scope; while the names disagree
    // (I1) each is its own variable there, shared by the bodies that spell it
    // the same, until `rename_slot` gives the slot one name. (A module factory
    // gets them from ESM codegen, at module level: `with_module_slot_names`.)
    // A block scope binds only what the context calls the slot or its
    // builder's body does.
    // An owner nothing creates any more renders nowhere and binds nothing: the
    // async wrapper that took its generator's body leaves the generator's
    // environment-creating function behind, and the body that now reads its
    // slots declares them itself.
    fn owner_binds(
        &self,
        ctx: &crate::analysis::closure::ClosureContext,
        slot: crate::ir::SlotRef,
        name: &str,
    ) -> bool {
        let owner = ctx.scope_function(slot.owner);
        let renders = self.created_functions.contains(&owner)
            || self.registry.function_to_module.contains_key(&owner)
            || !ctx.parent_function.contains_key(&owner);
        if !renders {
            return false;
        }
        if ctx
            .function_closures
            .get(&slot.owner)
            .is_some_and(|info| info.get_slot_name(slot.slot) == name)
        {
            return true;
        }
        self.slot_names.get(&slot).is_some_and(|n| {
            n.in_owner.contains(name)
                || (owner == slot.owner
                    && n.all.contains(name)
                    && crate::util::is_valid_identifier(name))
        })
    }

    // Every name any body gives one of the function's own slots. The owner
    // declares them all: a slot only its closures fill
    // (`defineLazyObjectProperty`'s value cache, set by `setValue` and read by
    // `getValue`) is bound by no statement of the owner, and a slot its
    // readers spell differently from the owner (React's current fiber, `c100`
    // here and `_null2` there) left the other spelling to codegen's
    // module-level `let`, which disappears as soon as any function in the
    // module declares that name for itself.
    pub(super) fn slot_names_of(&self, func_id: u32) -> HashSet<String> {
        // Slots order by owner first, so a function's slots are one range. The
        // context's slot list is not used: it misses a slot only closures touch.
        let from = crate::ir::SlotRef {
            owner: func_id,
            slot: 0,
        };
        let to = crate::ir::SlotRef {
            owner: func_id,
            slot: u32::MAX,
        };
        self.slot_names
            .range(from..=to)
            .flat_map(|(_, n)| n.all.iter().cloned())
            // A name that is no identifier (`react-native`, a slot whose
            // `require` was lost) is codegen's to bind or not.
            .filter(|n| crate::util::is_valid_identifier(n))
            .collect()
    }

    // Declare a module factory's variables: codegen binds what the module writes
    // and never declares (an import, or a module-level `let`). Its class
    // methods are declared like any function.
    pub(super) fn insert_factory_declarations(
        &self,
        func_id: u32,
        stmts: &mut Vec<Statement>,
        params: &[String],
    ) {
        self.declare_class_methods(stmts);
        let extra = self.extra_writes_for_function(func_id);
        transforms::insert_declarations_with_extra_writes(stmts, params, &extra);
    }

    // Keep the own-slot names something reads as this function's variable:
    // the body itself, or a function below it, by a reference stamped with
    // one of this function's slots or unstamped. Any other name is one the
    // context or another body once gave the slot, and declaring it binds
    // nothing anyone uses (the global function's `let React10;`, a stale
    // `let value2;` beside the `value` its closures read). A reference below
    // stamped with an ancestor's slot is the ancestor's variable when the
    // ancestor binds the name (`owner_binds`), and this declaration would
    // only hide it. Any other stamped read keeps it: a body cloned from a
    // sibling keeps the sibling's stamps (d3's `link` factories share one
    // child, whose `linkSource2` names a slot of the last factory), and a
    // read under a name the owner never binds (I1) has this declaration as
    // its only binding, dead as it may be. Dropping it would turn an
    // `undefined` into a ReferenceError. K4.6 names it.
    fn retain_read_slots(&self, func_id: u32, stmts: &[Statement], names: &mut HashSet<String>) {
        let Some(ctx) = self.closure_ctx.as_ref() else {
            return;
        };
        let ancestors: HashSet<u32> = ctx.ancestors_of(func_id).into_iter().collect();
        retain_read_slots(
            func_id,
            stmts,
            names,
            &self.all_ir,
            &self.child_functions,
            |slot, name| match slot {
                None => true,
                // ESM codegen declares no module slot spelled like a browser
                // global (`location`), whatever `owner_binds` says.
                Some(_) if crate::ir::expr::display::is_builtin_global(name) => true,
                Some(s) => {
                    let owner = ctx.scope_function(s.owner);
                    owner == func_id
                        || !(ancestors.contains(&owner) && self.owner_binds(ctx, s, name))
                }
            },
        );
    }

    // For each closure `stmts` create, the slots of `func_id` among `names`
    // that it or a function below it reads, by stamp.
    fn own_slots_read_below(
        &self,
        func_id: u32,
        stmts: &[Statement],
        names: &HashSet<String>,
    ) -> HashMap<u32, HashSet<String>> {
        let mut out = HashMap::new();
        if names.is_empty() {
            return out;
        }
        for id in closures_created(stmts) {
            let mut read = HashSet::new();
            let mut stack = vec![id];
            let mut seen = HashSet::new();
            while let Some(f) = stack.pop() {
                if !seen.insert(f) {
                    continue;
                }
                if let Some(body) = self.all_ir.get(&f) {
                    for (name, slot) in crate::analysis::closure::stamps::var_refs(body) {
                        if slot.is_some_and(|s| s.owner == func_id) && names.contains(name) {
                            read.insert(name.to_string());
                        }
                    }
                }
                if let Some(children) = self.child_functions.get(&f) {
                    stack.extend(children.iter().copied());
                }
            }
            if !read.is_empty() {
                out.insert(id, read);
            }
        }
        out
    }

    // Names each function's nested functions read or write (see
    // `names_used_by_descendants`).
    pub(crate) fn compute_captured_by_descendants(&self) -> BTreeMap<u32, HashSet<String>> {
        let empty = BTreeMap::new();
        transforms::names_used_by_descendants(
            &self.all_ir,
            self.closure_ctx
                .as_ref()
                .map(|c| &c.parent_function)
                .unwrap_or(&empty),
        )
    }

    // Names each function's nested functions use for an enclosing scope's
    // variable (see `outer_names_used_by_descendants`).
    pub(crate) fn compute_outer_used_by_descendants(&self) -> BTreeMap<u32, HashSet<String>> {
        let empty = BTreeMap::new();
        let parent_of = self
            .closure_ctx
            .as_ref()
            .map(|c| &c.parent_function)
            .unwrap_or(&empty);
        transforms::outer_names_used_by_descendants(&self.all_ir, parent_of, |scope| {
            self.closure_ctx
                .as_ref()
                .map_or(scope, |c| c.scope_function(scope))
        })
    }

    fn prepare_render_bodies(
        &self,
        file: &BytecodeFile,
    ) -> BTreeMap<u32, (Vec<String>, Vec<Statement>)> {
        // Env-slot names only (not every ancestor local), precomputed once on the
        // context so each function is O(slots), not O(ancestor-body-size).
        let captured = &self.captured_by_descendants;
        self.all_ir
            .par_iter()
            .filter(|(func_id, _)| !self.registry.function_to_module.contains_key(func_id))
            .map(|(&func_id, stmts)| {
                let params: Vec<String> =
                    if let Some(names) = self.global_analysis.param_names.get(&func_id) {
                        // Two distinct names can collapse into one once
                        // sanitised, so distinctness is checked after.
                        crate::pipeline::make_params_distinct(
                            crate::pipeline::params_from_names(file, func_id, names, stmts)
                                .iter()
                                .map(|p| crate::util::sanitize_identifier(p)),
                        )
                    } else {
                        crate::pipeline::make_params_distinct(
                            get_function_params(file, func_id, stmts)
                                .into_iter()
                                .map(|p| crate::util::sanitize_identifier(&p)),
                        )
                    };

                let mut body_stmts = stmts.clone();
                if let Some(param_names) = self.global_analysis.param_names.get(&func_id) {
                    transforms::exports::rename_param_registers(&mut body_stmts, param_names);
                }
                body_stmts = transforms::cleanup_noise(body_stmts);
                // Drop dead stores of a reused slot (`nativePerformanceNowResult =
                // __d(...)` repeated for every Metro module): keep the side-effecting
                // call, discard the useless assignment target. Runs here, on the
                // final named form, where these stores are consecutive.
                body_stmts = transforms::eliminate_dead_stores(body_stmts);
                // Drop dead argument-setup copies and other unread pure temps,
                // never a name a nested function still reads.
                let none = HashSet::new();
                let keep = captured.get(&func_id).unwrap_or(&none);
                body_stmts = transforms::remove_dead_temp_bindings_keeping(body_stmts, keep);
                transforms::rename_reserved_words(&mut body_stmts);
                transforms::make_sanitized_names_distinct(&mut body_stmts);
                // After the parameter rename, a copy named for the property
                // `kind` lands on the parameter: `kind = kind.kind`. Split it
                // before declarations so the fresh name is declared and the
                // later `kind.voiceState` reads stay on the object.
                body_stmts = transforms::repair_switch_clobbers(body_stmts);
                self.insert_body_declarations(func_id, &mut body_stmts, &params);
                self.declare_class_methods(&mut body_stmts);

                (func_id, (params, body_stmts))
            })
            .collect()
    }

    // Render pre-prepared function bodies, using `existing_inline` for nested
    // function references. When `only` is `Some`, render just that subset (the
    // parents whose referenced bodies changed in the previous pass); otherwise
    // render every function.
    fn render_prepared_bodies(
        &self,
        file: &BytecodeFile,
        prepared: &BTreeMap<u32, (Vec<String>, Vec<Statement>)>,
        existing_inline: &Arc<BTreeMap<u32, String>>,
        only: Option<&BTreeSet<u32>>,
    ) -> BTreeMap<u32, String> {
        // Each function body renders independently from the shared `existing_inline`
        // map (read only) into its own entry, so a pass over the functions is
        // embarrassingly parallel. On a large bundle this is the dominant cost, so
        // fan it out across cores.
        prepared
            .par_iter()
            .filter(|(func_id, _)| only.is_none_or(|s| s.contains(func_id)))
            .map(|(&func_id, (params, body_stmts))| {
                (
                    func_id,
                    self.render_one_body(file, func_id, params, body_stmts, existing_inline),
                )
            })
            .collect()
    }

    // Render a single function body to its final source string, using
    // `existing_inline` for any nested function references.
    fn render_one_body(
        &self,
        file: &BytecodeFile,
        func_id: u32,
        params: &[String],
        body_stmts: &[Statement],
        existing_inline: &Arc<BTreeMap<u32, String>>,
    ) -> String {
        {
            // If this is a Reanimated worklet, emit its original source recovered
            // from the embedded `__initData.code` string instead of decompiling
            // (the compiled form often mis-renders, e.g. as a `class`).
            if let Some(src) = self.worklet_source_for(file, func_id) {
                return src;
            }
            // Render the body with existing inline bodies for nested functions
            let mut inner_codegen = Codegen::new(CodegenOptions::default())
                .with_inline_bodies(Arc::clone(existing_inline));
            inner_codegen = self.with_module_esm(inner_codegen, func_id);
            let body = inner_codegen.generate_statements(body_stmts);
            // Indent body by one level (2 spaces) for proper nesting inside function { }
            let body_trimmed: String = body
                .trim_end()
                .lines()
                .map(|line| {
                    if line.is_empty() {
                        String::new()
                    } else {
                        format!("  {line}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");

            // Get function properties (is_arrow, etc.) from closure context and bytecode header
            let is_async = self
                .closure_ctx
                .as_ref()
                .is_some_and(|c| c.is_async(func_id));
            let is_generator = self
                .closure_ctx
                .as_ref()
                .is_some_and(|c| c.is_generator(func_id));
            // Async generators (Babel pattern) should render as async, not function*
            let is_generator = is_generator && !is_async;
            // Arrow heuristic: anonymous, not generator, doesn't use `this`
            // or `arguments` (an arrow has neither of its own).
            let uses_this = stmts_use_this(body_stmts) || stmts_use_arguments(body_stmts);
            // `super` is only legal in a method or in an arrow nested in one:
            // Babel's `_superprop_getX` helpers are arrows in the source, and a
            // `function` declaration reading `super.x` does not parse.
            let uses_super = stmts_use_super(body_stmts);
            let is_arrow = !is_generator && (!uses_this || uses_super);
            let func_name = file
                .function_headers
                .get(func_id as usize)
                .and_then(|h| file.string_at(h.function_name()))
                .filter(|e| {
                    !e.value.is_empty()
                        && crate::util::is_valid_identifier(&e.value)
                        && !crate::constants::is_reserved_word(&e.value)
                })
                .map(|e| e.value.clone());

            let async_prefix = if is_async { "async " } else { "" };
            let gen_star = if is_generator { "*" } else { "" };
            let params_str = params.join(", ");

            let rendered = if is_arrow && (func_name.is_none() || uses_super) {
                // Arrow function rendering
                if body_stmts.len() == 1 {
                    if let Statement::Return(Some(expr)) = &body_stmts[0] {
                        // Concise arrow: (params) => expr
                        let expr_str =
                            inner_codegen.generate_statements(&[Statement::Expr(expr.clone())]);
                        let expr_trimmed = expr_str.trim().trim_end_matches(';');
                        // Wrap in parens if it starts with { (object literal ambiguity)
                        if expr_trimmed.starts_with('{') {
                            format!("{async_prefix}({params_str}) => ({expr_trimmed})")
                        } else {
                            format!("{async_prefix}({params_str}) => {expr_trimmed}")
                        }
                    } else {
                        // Single non-return statement: block arrow
                        format!("{async_prefix}({params_str}) => {{\n{body_trimmed}\n}}")
                    }
                } else {
                    // Block arrow
                    format!("{async_prefix}({params_str}) => {{\n{body_trimmed}\n}}")
                }
            } else {
                // Regular function rendering
                match &func_name {
                    Some(n) => format!(
                        "{async_prefix}function{gen_star} {n}({params_str}) {{\n{body_trimmed}\n}}"
                    ),
                    None => format!(
                        "{async_prefix}function{gen_star}({params_str}) {{\n{body_trimmed}\n}}"
                    ),
                }
            };

            rendered
        }
    }

    // What the IR calls each stamped slot: every name any body gives it, and the
    // names its owner's own body gives it.
    pub(in crate::pipeline) fn precompute_slot_names(
        &self,
    ) -> BTreeMap<crate::ir::SlotRef, super::SlotNames> {
        let mut out: BTreeMap<crate::ir::SlotRef, super::SlotNames> = BTreeMap::new();
        let Some(ctx) = self.closure_ctx.as_ref() else {
            return out;
        };
        for (&func, stmts) in &self.all_ir {
            for (name, slot) in crate::analysis::closure::stamps::var_refs(stmts) {
                let Some(slot) = slot else { continue };
                let names = out.entry(slot).or_default();
                if !names.all.contains(name) {
                    names.all.insert(name.to_string());
                }
                if ctx.scope_function(slot.owner) == func && !names.in_owner.contains(name) {
                    names.in_owner.insert(name.to_string());
                }
            }
        }
        out
    }

    pub(in crate::pipeline) fn precompute_created_functions(&self) -> HashSet<u32> {
        use crate::ir::{Expression, Visitor};
        struct Ids(HashSet<u32>);
        impl<'a> Visitor<'a> for Ids {
            fn visit_expression(&mut self, e: &'a Expression) {
                if let Expression::Function { id, .. } = e {
                    self.0.insert(id.0);
                }
                self.walk_expression(e);
            }
            // A class creates its methods; the walker does not visit them.
            fn visit_statement(&mut self, s: &'a Statement) {
                if let Statement::Class { methods, .. } = s {
                    for m in methods {
                        self.visit_expression(&m.value);
                        if let Some(body) = &m.body {
                            for st in body {
                                self.visit_statement(st);
                            }
                        }
                    }
                }
                self.walk_statement(s);
            }
        }
        let mut ids = Ids(HashSet::new());
        for stmts in self.all_ir.values() {
            for s in stmts {
                ids.visit_statement(s);
            }
        }
        ids.0
    }

    // Top-down: names(fn) = names(parent) ∪ slot names of parent. One pass, O(n).
    pub(in crate::pipeline) fn precompute_ancestor_env_slot_names(
        &self,
    ) -> BTreeMap<u32, HashSet<String>> {
        let Some(ctx) = self.closure_ctx.as_ref() else {
            return BTreeMap::new();
        };

        // Each scope's own slots by the context's names, less a name that went
        // stale (K4.4a): a slot whose stamped references all spell it another
        // way no longer answers to the context's name, and keeping the name kept
        // a child's unrelated local undeclared (lodash's `cloneRegExp`
        // destructuring into `source`). Only drops names; what the IR does call
        // the slot is K4.4b's to add.
        let mut own: BTreeMap<u32, HashSet<String>> = BTreeMap::new();
        for (&fid, info) in &ctx.function_closures {
            let mut names = HashSet::new();
            for &key in info.slots.keys() {
                let name = info.get_slot_name(key);
                let slot = crate::ir::SlotRef {
                    owner: fid,
                    slot: key,
                };
                let stale = key >> 24 == 0
                    && self
                        .slot_names
                        .get(&slot)
                        .is_some_and(|used| !used.all.contains(&name));
                if !stale {
                    names.insert(name);
                }
            }
            own.insert(fid, names);
        }

        let mut memo: BTreeMap<u32, HashSet<String>> = BTreeMap::new();
        let mut visiting = HashSet::new();
        for &id in self.all_ir.keys() {
            fill_ancestor_env_slots(id, &ctx.parent_function, &own, &mut memo, &mut visiting);
        }
        memo
    }
}

// The ids of the closures `stmts` create, nested ones included. A class's
// methods are closures too, but the walker does not visit them (moment's
// `hooks.locales()` reads the `keys` slot).
fn closures_created(stmts: &[Statement]) -> Vec<u32> {
    use crate::ir::{Expression, Visitor};
    struct Ids(Vec<u32>);
    impl<'a> Visitor<'a> for Ids {
        fn visit_expression(&mut self, e: &'a Expression) {
            if let Expression::Function { id, .. } = e {
                self.0.push(id.0);
            }
            self.walk_expression(e);
        }
        fn visit_statement(&mut self, s: &'a Statement) {
            if let Statement::Class { methods, .. } = s {
                for m in methods {
                    self.visit_expression(&m.value);
                }
            }
            self.walk_statement(s);
        }
    }
    let mut ids = Ids(Vec::new());
    for s in stmts {
        ids.visit_statement(s);
    }
    ids.0
}

// `PipelineContext::retain_read_slots`, over the IR it is given: `children`
// lists each function's nested functions, and `needs_it` says whether a
// reference below, by its stamp and name, needs this function's declaration.
fn retain_read_slots(
    func_id: u32,
    stmts: &[Statement],
    names: &mut HashSet<String>,
    all_ir: &BTreeMap<u32, Vec<Statement>>,
    children: &BTreeMap<u32, Vec<u32>>,
    needs_it: impl Fn(Option<crate::ir::SlotRef>, &str) -> bool,
) {
    use crate::analysis::closure::stamps::var_refs;
    if names.is_empty() {
        return;
    }
    let here: HashSet<&str> = var_refs(stmts).into_iter().map(|(n, _)| n).collect();
    let mut unread: HashSet<String> = names
        .iter()
        .map(|n| crate::util::sanitize_identifier(n))
        .filter(|n| !here.contains(n.as_str()))
        .collect();
    // The functions below: the context's children, and the closures the IR
    // creates, which include bodies cloned after the context was built.
    let mut stack: Vec<u32> = children.get(&func_id).cloned().unwrap_or_default();
    stack.extend(closures_created(stmts));
    let mut seen = HashSet::new();
    while let Some(f) = stack.pop() {
        if unread.is_empty() {
            break;
        }
        if !seen.insert(f) {
            continue;
        }
        if let Some(body) = all_ir.get(&f) {
            for (name, slot) in var_refs(body) {
                if needs_it(slot, name) {
                    unread.remove(name);
                }
            }
            stack.extend(closures_created(body));
        }
        if let Some(c) = children.get(&f) {
            stack.extend(c.iter().copied());
        }
    }
    names.retain(|n| !unread.contains(&crate::util::sanitize_identifier(n)));
}

fn fill_ancestor_env_slots(
    id: u32,
    parent_of: &BTreeMap<u32, u32>,
    own: &BTreeMap<u32, HashSet<String>>,
    memo: &mut BTreeMap<u32, HashSet<String>>,
    visiting: &mut HashSet<u32>,
) {
    if memo.contains_key(&id) {
        return;
    }
    if !visiting.insert(id) {
        memo.insert(id, HashSet::new());
        return;
    }
    if let Some(&parent) = parent_of.get(&id) {
        fill_ancestor_env_slots(parent, parent_of, own, memo, visiting);
        let mut set = memo.get(&parent).cloned().unwrap_or_default();
        if let Some(parent_own) = own.get(&parent) {
            set.extend(parent_own.iter().cloned());
        }
        memo.insert(id, set);
    } else {
        memo.insert(id, HashSet::new());
    }
    visiting.remove(&id);
}

// Collect the ids of every function referenced by id (`Expression::Function { id }`)
// anywhere in `stmts`. These are the direct inline-body dependencies of the owning
// function: it re-renders when one of their rendered bodies changes.
fn count_body_holes(bodies: &BTreeMap<u32, String>) -> usize {
    bodies
        .values()
        .map(|s| s.matches(crate::transforms::BODY_HOLE).count())
        .sum()
}

fn collect_function_refs(stmts: &[crate::ir::Statement], out: &mut Vec<u32>) {
    use crate::ir::{Expression, Visitor};
    struct C<'a>(&'a mut Vec<u32>);
    impl<'a, 'b> Visitor<'b> for C<'a> {
        fn visit_expression(&mut self, e: &'b Expression) {
            if let Expression::Function { id, .. } = e {
                self.0.push(id.0);
            }
            self.walk_expression(e);
        }
        // The visitor leaves method bodies alone, but a class renders its
        // methods' functions and whatever they create inline. Unrecorded, the
        // parent was never re-rendered once those bodies landed, and kept
        // `/* body not rendered */` for an arrow made in a constructor.
        fn visit_statement(&mut self, s: &'b crate::ir::Statement) {
            if let crate::ir::Statement::Class { methods, .. } = s {
                for method in methods {
                    self.visit_expression(&method.value);
                    for st in method.body.iter().flatten() {
                        self.visit_statement(st);
                    }
                }
            }
            self.walk_statement(s);
        }
    }
    let mut c = C(out);
    for s in stmts {
        c.visit_statement(s);
    }
}

// Whether the body reads `arguments`. Function bodies are separate IR, so the
// walk never enters a nested function.
// Whether these statements read `super` (`super.x`, `super[x]`, `super.m()`).
// Nested function bodies are separate functions and decide for themselves.
fn stmts_use_super(stmts: &[crate::ir::Statement]) -> bool {
    use crate::ir::{Expression, Value, Visitor};
    struct S(bool);
    impl<'a> Visitor<'a> for S {
        fn visit_expression(&mut self, e: &'a Expression) {
            if matches!(e, Expression::Value(Value::Super)) {
                self.0 = true;
            }
            self.walk_expression(e);
        }
    }
    let mut s = S(false);
    for st in stmts {
        s.visit_statement(st);
    }
    s.0
}

pub(super) fn stmts_use_arguments(stmts: &[crate::ir::Statement]) -> bool {
    use crate::ir::{Expression, Value, Visitor};
    struct C(bool);
    impl<'a> Visitor<'a> for C {
        fn visit_expression(&mut self, e: &'a Expression) {
            if matches!(e, Expression::Value(Value::Arguments)) {
                self.0 = true;
            }
            self.walk_expression(e);
        }
    }
    let mut c = C(false);
    for s in stmts {
        c.visit_statement(s);
    }
    c.0
}

// Check if any statement in the list references `this` (non-recursing into nested functions).
pub(super) fn stmts_use_this(stmts: &[crate::ir::Statement]) -> bool {
    for stmt in stmts {
        if stmt_uses_this(stmt) {
            return true;
        }
    }
    false
}

fn stmt_uses_this(stmt: &crate::ir::Statement) -> bool {
    use crate::ir::Statement;
    match stmt {
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => expr_uses_this(e),
        Statement::Assign { target, value } => target_uses_this(target) || expr_uses_this(value),
        Statement::Let { value, .. } => expr_uses_this(value),
        Statement::If {
            condition,
            then_body,
            else_body,
        } => expr_uses_this(condition) || stmts_use_this(then_body) || stmts_use_this(else_body),
        Statement::While { condition, body } | Statement::DoWhile { body, condition } => {
            expr_uses_this(condition) || stmts_use_this(body)
        }
        Statement::For {
            init,
            condition,
            update,
            body,
        } => {
            init.as_ref().is_some_and(|s| stmt_uses_this(s))
                || condition.as_ref().is_some_and(expr_uses_this)
                || update.as_ref().is_some_and(|s| stmt_uses_this(s))
                || stmts_use_this(body)
        }
        Statement::ForIn { object, body, .. } => expr_uses_this(object) || stmts_use_this(body),
        Statement::ForOf { iterable, body, .. } => expr_uses_this(iterable) || stmts_use_this(body),
        Statement::Block(inner) => stmts_use_this(inner),
        Statement::TryCatch {
            try_body,
            catch_body,
            finally_body,
            ..
        } => stmts_use_this(try_body) || stmts_use_this(catch_body) || stmts_use_this(finally_body),
        Statement::Switch {
            discriminant,
            cases,
            default,
        } => {
            expr_uses_this(discriminant)
                || cases
                    .iter()
                    .any(|(e, body)| expr_uses_this(e) || stmts_use_this(body))
                || default.as_ref().is_some_and(|d| stmts_use_this(d))
        }
        _ => false,
    }
}

fn expr_uses_this(expr: &crate::ir::Expression) -> bool {
    use crate::ir::{Expression, Value};
    match expr {
        // `new.target` has no meaning in an arrow either.
        Expression::Value(Value::This) | Expression::Value(Value::NewTarget) => true,
        Expression::Binary { left, right, .. } => expr_uses_this(left) || expr_uses_this(right),
        Expression::Unary { operand, .. } => expr_uses_this(operand),
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            expr_uses_this(callee) || arguments.iter().any(expr_uses_this)
        }
        Expression::Member { object, .. } => expr_uses_this(object),
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => expr_uses_this(condition) || expr_uses_this(then_expr) || expr_uses_this(else_expr),
        Expression::Array { elements } => elements.iter().flatten().any(expr_uses_this),
        Expression::Object { properties } => properties.iter().any(|p| expr_uses_this(&p.value)),
        Expression::Assignment { target, value } => {
            let mut hit = false;
            crate::ir::for_each_target_expression(target, &mut |e| hit |= expr_uses_this(e));
            hit || expr_uses_this(value)
        }
        Expression::Spread(inner) | Expression::Await(inner) => expr_uses_this(inner),
        Expression::Yield { value, .. } => expr_uses_this(value),
        Expression::TemplateLiteral { expressions, .. } => expressions.iter().any(expr_uses_this),
        Expression::Function { .. } => false, // Don't recurse into nested functions
        _ => false,
    }
}

fn target_uses_this(target: &crate::ir::AssignTarget) -> bool {
    use crate::ir::AssignTarget;
    match target {
        AssignTarget::Member { object, .. } => expr_uses_this(object),
        AssignTarget::Index { object, key } => expr_uses_this(object) || expr_uses_this(key),
        _ => false,
    }
}

#[cfg(test)]
mod retain_read_slots_tests {
    use super::retain_read_slots;
    use crate::ir::{AssignTarget, Binding, Expression, SlotRef, Statement, Value, Var};
    use std::collections::{BTreeMap, HashSet};

    fn var(name: &str, slot: Option<SlotRef>) -> Expression {
        Expression::Value(Value::Binding(Binding::Variable(Var {
            name: name.into(),
            slot,
        })))
    }

    fn read(name: &str, slot: Option<SlotRef>) -> Statement {
        Statement::Return(Some(var(name, slot)))
    }

    fn write(name: &str, slot: Option<SlotRef>) -> Statement {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(Var {
                name: name.into(),
                slot,
            })),
            value: Expression::constant(crate::ir::Constant::Null),
        }
    }

    fn names(ns: &[&str]) -> HashSet<String> {
        ns.iter().map(|n| n.to_string()).collect()
    }

    // F1 owns slot 0, which its child F2 reads as `value`; F3, nested in F2,
    // reads F0's slot under the name `value2`; nobody reads `stale`.
    #[test]
    fn keeps_only_names_read_as_this_function_slot() {
        let own = |slot| Some(SlotRef { owner: 1, slot });
        let other = Some(SlotRef { owner: 0, slot: 0 });
        let all_ir = BTreeMap::from([
            (2, vec![read("value", own(0))]),
            (3, vec![read("value2", other), write("loose", None)]),
        ]);
        let children = BTreeMap::from([(1, vec![2]), (2, vec![3])]);
        let mut ns = names(&["value", "value2", "stale", "loose", "here"]);
        let needs_it = |slot: Option<SlotRef>, _: &str| slot.is_none_or(|s| s.owner == 1);
        retain_read_slots(
            1,
            &[read("here", None)],
            &mut ns,
            &all_ir,
            &children,
            needs_it,
        );
        assert_eq!(ns, names(&["value", "loose", "here"]));
    }

    // A read stamped with a slot whose owner does not bind the name keeps
    // the declaration: it is the only binding the read has.
    #[test]
    fn an_unbound_foreign_read_keeps_it() {
        let all_ir = BTreeMap::from([(2, vec![read("x", Some(SlotRef { owner: 0, slot: 3 }))])]);
        let children = BTreeMap::from([(1, vec![2])]);
        let mut ns = names(&["x"]);
        retain_read_slots(1, &[], &mut ns, &all_ir, &children, |_, _| true);
        assert_eq!(ns, names(&["x"]));
        retain_read_slots(1, &[], &mut ns, &all_ir, &children, |_, _| false);
        assert!(ns.is_empty());
    }
}

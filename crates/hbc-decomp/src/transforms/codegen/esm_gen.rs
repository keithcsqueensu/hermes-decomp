use super::{Codegen, DescriptorInfo, EsmClassification, sanitize_import_name, replace_whole_word};
use super::esm_imports::{consolidate_imports, fold_redundant_imports};
use crate::ir::Statement;

impl Codegen {
    // Generate ESM-style module output from IR statements.
    // Classifies statements into imports, body, and exports at the IR level,
    // replacing regex-based text rewriting.
    pub fn generate_esm_module(
        &mut self,
        statements: &[Statement],
        module_id: u32,
        module_name: Option<&str>,
    ) -> String {
        use std::collections::{HashMap, HashSet};

        // Pre-pass 0: Detect re-export modules (Object.keys(source).forEach pattern)
        // These modules just re-export everything from another module.
        if let Some(reexport) = self.detect_reexport_module(statements) {
            let mut output = String::new();
            if let Some(name) = module_name {
                output.push_str(&format!("// Module {module_id} ({name})\n"));
            } else {
                output.push_str(&format!("// Module {module_id}\n"));
            }
            output.push_str(&reexport);
            output.push('\n');
            return output;
        }

        // Pre-pass: rename a generic import binding (`closure_3 = require(id)`) to
        // its module's name, so it renders as `import _slicedToArray from
        // "_slicedToArray"` instead of `import closure_3 from "_slicedToArray"`, and
        // several captures of one module collapse to a single binding (which import
        // consolidation then folds into one line).
        let import_renames = self.import_binding_renames(statements);
        let renamed_owned: Vec<Statement>;
        let statements: &[Statement] = if import_renames.is_empty() {
            statements
        } else {
            let mut s = statements.to_vec();
            crate::analysis::naming::rename_variables_in_stmts(&mut s, &import_renames);
            renamed_owned = s;
            &renamed_owned
        };

        // Pre-pass: names the module writes to more than once at top level. The
        // first write may look like a module load and become an import, but an ESM
        // import binding is immutable, so any later write to the same name has to
        // land on a local instead.
        // Counting reaches nested blocks and bodies, not just the top level, because
        // a write buried in a branch invalidates the import just as surely. Counting
        // one name too many only costs a local alias that is still correct JS, while
        // missing one emits an assignment to an import, which is not.
        let rebound: HashSet<String> = {
            use crate::ir::{AssignTarget, Visitor};
            struct W<'a>(&'a mut HashMap<String, u32>);
            impl<'b> Visitor<'b> for W<'_> {
                fn visit_statement(&mut self, s: &'b Statement) {
                    if let Statement::Let { name, .. } = s {
                        *self.0.entry(name.clone()).or_insert(0) += 1;
                    }
                    self.walk_statement(s);
                }
                fn visit_assign_target(&mut self, t: &'b AssignTarget) {
                    if let AssignTarget::Variable(name) = t {
                        *self.0.entry(name.clone()).or_insert(0) += 1;
                    }
                    self.walk_assign_target(t);
                }
            }
            let mut writes: HashMap<String, u32> = HashMap::new();
            {
                let mut w = W(&mut writes);
                for stmt in statements {
                    w.visit_statement(stmt);
                }
            }
            writes.into_iter().filter(|(_, c)| *c > 1).map(|(n, _)| n).collect()
        };

        // Pre-pass: collect descriptor variables (objects with get/value used in defineProperty)
        let mut descriptor_vars: HashMap<String, DescriptorInfo> = HashMap::new();
        let mut consumed_descriptors: HashSet<String> = HashSet::new();

        // Pass 1: Find all Let/Assign that define descriptor-like objects
        for stmt in statements {
            match stmt {
                Statement::Let { name, value, .. } => {
                    if let Some(info) = self.extract_descriptor_info(value) {
                        descriptor_vars.insert(name.clone(), info);
                    }
                }
                Statement::Assign { target: crate::ir::AssignTarget::Variable(name), value } => {
                    if let Some(info) = self.extract_descriptor_info(value) {
                        descriptor_vars.insert(name.clone(), info);
                    }
                }
                _ => {}
            }
        }

        // Pass 2: Find defineProperty calls that reference descriptor vars, mark them consumed
        for stmt in statements {
            if let Statement::Expr(expr) = stmt {
                if let Some(var_name) = self.get_define_property_descriptor_var(expr) {
                    if descriptor_vars.contains_key(&var_name) {
                        consumed_descriptors.insert(var_name);
                    }
                }
            }
        }

        // Pre-pass 3: Detect `Object.keys(X) + X.forEach(...)` re-export pairs
        // Pattern: X = Object.keys(X); let _ = X.forEach(cb) → export * from "modName"
        // Build a map of import variable → module name from the statements
        let mut import_var_to_module: HashMap<String, String> = HashMap::new();
        for stmt in statements {
            match stmt {
                Statement::Let { name, value, .. } | Statement::Assign { target: crate::ir::AssignTarget::Variable(name), value } => {
                    if let Some(mod_name) = self.resolve_require_module(value) {
                        import_var_to_module.insert(name.clone(), mod_name);
                    }
                    // Also check wrapper(require(N))
                    if let crate::ir::Expression::Call { arguments, .. } = value {
                        for arg in Self::effective_args(arguments) {
                            if let Some(mod_name) = self.resolve_require_module(arg) {
                                import_var_to_module.insert(name.clone(), mod_name);
                                break;
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Find Object.keys(X) assignments followed by X.forEach(...) calls
        let mut reexport_vars: HashSet<String> = HashSet::new();  // vars that are re-exported via forEach
        let mut reexport_skip_stmts: HashSet<usize> = HashSet::new(); // indices to skip
        let mut reexport_exports: Vec<(usize, String)> = Vec::new(); // (insert_at_index, export_line)

        for (i, stmt) in statements.iter().enumerate() {
            // Detect: X = Object.keys(X) (Assign where value is keys() call)
            // Also extract the source variable from Object.keys(SRC)
            let keys_info = match stmt {
                Statement::Assign { target: crate::ir::AssignTarget::Variable(name), value } => {
                    if self.is_object_keys_call(value) {
                        let src = self.extract_object_keys_source(value)
                            .unwrap_or_else(|| name.clone());
                        Some((name.clone(), src))
                    } else { None }
                }
                Statement::Let { name, value, .. } => {
                    if self.is_object_keys_call(value) {
                        let src = self.extract_object_keys_source(value)
                            .unwrap_or_else(|| name.clone());
                        Some((name.clone(), src))
                    } else { None }
                }
                _ => None,
            };
            if let Some((target_var, source_var)) = keys_info {
                // Look for the next statement: _ = X.forEach(callback) or let _ = X.forEach(callback)
                if i + 1 < statements.len() {
                    let next = &statements[i + 1];
                    let is_foreach = match next {
                        Statement::Let { value, .. } => self.is_foreach_on_var(value, &target_var),
                        Statement::Expr(value) => self.is_foreach_on_var(value, &target_var),
                        Statement::Assign { value, .. } => self.is_foreach_on_var(value, &target_var),
                        _ => false,
                    };
                    if is_foreach {
                        // Try source var first (Object.keys(source)), then target var
                        let mod_name = import_var_to_module.get(&source_var)
                            .or_else(|| import_var_to_module.get(&target_var));
                        if let Some(mod_name) = mod_name {
                            reexport_skip_stmts.insert(i);
                            reexport_skip_stmts.insert(i + 1);
                            reexport_exports.push((i, format!("export * from \"{mod_name}\";")));
                            reexport_vars.insert(source_var);
                        }
                    }
                }
            }
        }

        let mut imports = Vec::new();
        let mut body_stmts = Vec::new();
        let mut exports = Vec::new();

        for (i, stmt) in statements.iter().enumerate() {
            // Skip statements consumed by re-export pattern
            if reexport_skip_stmts.contains(&i) {
                // If this index has a re-export line, emit it
                for (idx, line) in &reexport_exports {
                    if *idx == i {
                        exports.push(line.clone());
                    }
                }
                continue;
            }

            // Skip Let/Assign that define consumed descriptor variables
            let skip_descriptor = match stmt {
                Statement::Let { name, .. } => consumed_descriptors.contains(name),
                Statement::Assign { target: crate::ir::AssignTarget::Variable(name), .. } => {
                    consumed_descriptors.contains(name)
                }
                _ => false,
            };
            if skip_descriptor {
                continue;
            }

            // Skip import statements for variables that became export * re-exports
            // (the import is subsumed by the export * from)
            let is_reexport_import = match stmt {
                Statement::Let { name, .. } | Statement::Assign { target: crate::ir::AssignTarget::Variable(name), .. } => {
                    reexport_vars.contains(name)
                }
                _ => false,
            };

            match self.classify_esm_stmt_with_descriptors(stmt, &descriptor_vars, &rebound) {
                EsmClassification::Import(line) => {
                    // Skip import for re-exported modules
                    if is_reexport_import {
                        continue;
                    }
                    imports.push(line);
                }
                EsmClassification::Export(line) => exports.push(line),
                EsmClassification::ImportAndExport(imp, exp) => {
                    imports.push(imp);
                    exports.push(exp);
                }
                EsmClassification::ImportAndBody(imp, line) => {
                    if !is_reexport_import {
                        imports.push(imp);
                    }
                    body_stmts.push(line);
                }
                EsmClassification::Skip => {}
                EsmClassification::Body => body_stmts.push(self.generate_stmt(stmt)),
            }
        }

        // Post-pass: rename closure_N imports to meaningful names
        // e.g. `import closure_0 from "_typeof"` → `import _typeof from "_typeof"`
        let mut closure_renames: HashMap<String, String> = HashMap::new();
        let mut used_import_names: HashSet<String> = HashSet::new();
        // Collect names already used by non-closure imports
        for imp in &imports {
            // Extract import name from patterns like `import X from` or `import { X }` or `import { Y as X }`
            if let Some(rest) = imp.strip_prefix("import ") {
                if let Some(name) = rest.split_whitespace().next() {
                    if !name.starts_with('{') && !name.starts_with('*') {
                        used_import_names.insert(name.to_string());
                    }
                }
            }
        }
        for imp in &imports {
            // Match: import closure_N from "modName";
            if let Some(rest) = imp.strip_prefix("import ") {
                let parts: Vec<&str> = rest.splitn(3, ' ').collect();
                if parts.len() >= 3 && parts[0].starts_with("closure_") && parts[1] == "from" {
                    let mod_name = parts[2].trim_matches(|c| c == '"' || c == ';');
                    let sanitized = sanitize_import_name(mod_name);
                    if !sanitized.is_empty() && sanitized != parts[0] && !used_import_names.contains(&sanitized) {
                        used_import_names.insert(sanitized.clone());
                        closure_renames.insert(parts[0].to_string(), sanitized);
                    }
                }
            }
            // Match: import { default as closure_N } from "modName";
            if imp.contains("default as closure_") {
                if let Some(start) = imp.find("default as closure_") {
                    let after = &imp[start + "default as ".len()..];
                    if let Some(end) = after.find([' ', '}']) {
                        let closure_name = &after[..end];
                        if closure_name.starts_with("closure_") {
                            if let Some(from_idx) = imp.find("from \"") {
                                let mod_part = &imp[from_idx + 6..];
                                if let Some(end_quote) = mod_part.find('"') {
                                    let mod_name = &mod_part[..end_quote];
                                    let sanitized = sanitize_import_name(mod_name);
                                    if !sanitized.is_empty() && sanitized != closure_name && !used_import_names.contains(&sanitized) {
                                        used_import_names.insert(sanitized.clone());
                                        closure_renames.insert(closure_name.to_string(), sanitized);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        // Apply renames to imports and body (using whole-word replacement to avoid partial matches)
        // Sort renames by key for deterministic output
        if !closure_renames.is_empty() {
            let mut sorted_renames: Vec<_> = closure_renames.iter().collect();
            sorted_renames.sort_by(|(a, _), (b, _)| a.cmp(b));
            for imp in imports.iter_mut() {
                for (old, new_name) in &sorted_renames {
                    *imp = replace_whole_word(imp, old, new_name);
                }
            }
            for body in body_stmts.iter_mut() {
                for (old, new_name) in &sorted_renames {
                    *body = replace_whole_word(body, old, new_name);
                }
            }
            for exp in exports.iter_mut() {
                for (old, new_name) in &sorted_renames {
                    *exp = replace_whole_word(exp, old, new_name);
                }
            }
        }

        // Consolidate imports: drop the repeated identical lines a module emits when
        // it requires the same dependency from many functions (e.g. `import _curry2
        // from "_curry2";` x65), and merge distinct named imports of the same module
        // into one `import { a, b } from "M";`.
        let imports = consolidate_imports(imports);
        let (mut imports, mut extra_consts) =
            fold_redundant_imports(imports, &mut body_stmts, &mut exports);
        // Consolidation and folding collapse *one* module's imports, but two
        // *different* modules can still contribute the same binding name; those
        // have to be made unique or the module does not parse.
        dedupe_import_bindings(&mut imports);

        // Deduplicate exports (e.g. multiple export * from same module)
        {
            let mut seen = HashSet::new();
            exports.retain(|e| seen.insert(e.clone()));
        }

        // `function name(){…}` + `export const name = …` → `export function name`
        dedupe_function_export_collisions(&mut body_stmts, &mut exports);
        // An import binding and a top-level declaration cannot share a name
        // either; rewrite longhand re-exports to `export … from` clauses and
        // demote the remaining collisions to side-effect imports.
        resolve_import_declaration_collisions(&mut imports, &body_stmts, &mut exports);

        // A module that still calls `require` needs it bound. Metro keeps some
        // dependencies lazy, most visibly in the React Native index where every
        // export is a getter closing over `require(...)`, and turning those into
        // static imports would load eagerly and change what the module does. The
        // call is therefore kept as written, and the binding it needs is declared
        // here: Metro publishes its loader on the global as `__r`.
        if body_calls_require(&body_stmts) || body_calls_require(&exports) {
            extra_consts.insert(0, "const require = globalThis.__r;".to_string());
        }

        // Build output
        let mut output = String::new();

        // Module header
        if let Some(name) = module_name {
            output.push_str(&format!("// Module {module_id} ({name})\n"));
        } else {
            output.push_str(&format!("// Module {module_id}\n"));
        }

        // Imports
        if !imports.is_empty() {
            for imp in &imports {
                output.push_str(imp);
                output.push('\n');
            }
            if extra_consts.is_empty() {
                output.push('\n');
            }
        }
        if !extra_consts.is_empty() {
            if !imports.is_empty() {
                output.push('\n');
            }
            for c in &extra_consts {
                output.push_str(c);
                output.push('\n');
            }
            output.push('\n');
        }

        // Body (skip leading/trailing empty lines)
        let body_text: String = body_stmts.concat();
        let trimmed = body_text.trim();
        if !trimmed.is_empty() {
            output.push_str(trimmed);
            output.push('\n');
        }

        // Exports
        if !exports.is_empty() {
            output.push('\n');
            for exp in &exports {
                output.push_str(exp);
                output.push('\n');
            }
        }

        output
    }
}

/// When body already has `function name(…)` and exports have `export const name = …`,
/// promote the declaration to `export function name` and drop the export const.
fn dedupe_function_export_collisions(body_stmts: &mut [String], exports: &mut Vec<String>) {
    use std::collections::HashSet;

    let mut fn_names: HashSet<String> = HashSet::new();
    for body in body_stmts.iter() {
        for line in body.lines() {
            let t = line.trim_start();
            if t.starts_with("export ") {
                continue;
            }
            let rest = if let Some(r) = t.strip_prefix("async function ") {
                r.trim_start_matches('*').trim_start()
            } else if let Some(r) = t.strip_prefix("function ") {
                r.trim_start_matches('*').trim_start()
            } else {
                continue;
            };
            if let Some(name) = rest.split(|c: char| c == '(' || c.is_whitespace()).next() {
                if !name.is_empty() && crate::util::is_valid_identifier(name) {
                    fn_names.insert(name.to_string());
                }
            }
        }
    }
    if fn_names.is_empty() {
        return;
    }

    let mut promote: HashSet<String> = HashSet::new();
    exports.retain(|exp| {
        let Some(rest) = exp.strip_prefix("export const ") else {
            return true;
        };
        let Some((name, _)) = rest.split_once(" = ") else {
            return true;
        };
        let name = name.trim();
        if fn_names.contains(name) {
            promote.insert(name.to_string());
            false
        } else {
            true
        }
    });
    if promote.is_empty() {
        return;
    }

    for body in body_stmts.iter_mut() {
        for name in &promote {
            *body = body.replace(
                &format!("async function {name}("),
                &format!("export async function {name}("),
            );
            *body = body.replace(
                &format!("function {name}("),
                &format!("export function {name}("),
            );
            *body = body.replace(
                &format!("async function* {name}("),
                &format!("export async function* {name}("),
            );
            *body = body.replace(
                &format!("function* {name}("),
                &format!("export function* {name}("),
            );
            *body = body.replace("export export ", "export ");
        }
    }
}

fn parse_import_line(line: &str) -> (Option<&str>, Option<&str>) {
    let module = line
        .rfind("from \"")
        .map(|i| i + "from \"".len())
        .or_else(|| line.starts_with("import \"").then_some("import \"".len()))
        .and_then(|start| {
            let rest = &line[start..];
            rest.find('"').map(|end| &rest[..end])
        });

    let Some(rest) = line.strip_prefix("import ") else {
        return (None, module);
    };
    // `import { x } from …`, `import { x as y } from …`, `import * as y from …`
    let binding = if let Some(inner) = rest.strip_prefix('{') {
        inner
            .split_once('}')
            .map(|(names, _)| names)
            .and_then(|names| names.rsplit(" as ").next())
            .map(str::trim)
    } else if let Some(after) = rest.strip_prefix("* as ") {
        after.split_whitespace().next()
    } else {
        rest.split_whitespace().next().filter(|b| *b != "from")
    };

    (
        binding.filter(|b| !b.is_empty() && crate::util::is_valid_identifier(b)),
        module,
    )
}

/// Make import bindings unique, which the module needs in order to parse.
///
/// Two distinct IR variables can carry the same name (upstream naming assigns
/// one name to both), and the same require can be classified twice, so the
/// naive emission produces `import x from "a"; import x from "b";`. An exact
/// repeat is dropped. A real conflict keeps one binding — preferring the import
/// whose module the name came from — and demotes the rest to side-effect
/// imports, which keeps the dependency visible without asserting a binding the
/// body cannot honour.
fn dedupe_import_bindings(imports: &mut Vec<String>) {
    use std::collections::{HashMap, HashSet};

    let mut seen_lines: HashSet<&str> = HashSet::new();
    let mut exact_dupes: Vec<usize> = Vec::new();
    for (i, line) in imports.iter().enumerate() {
        if !seen_lines.insert(line.as_str()) {
            exact_dupes.push(i);
        }
    }
    for i in exact_dupes.into_iter().rev() {
        imports.remove(i);
    }

    // binding → indices of the import lines that introduce it
    let mut by_binding: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, line) in imports.iter().enumerate() {
        if let (Some(binding), _) = parse_import_line(line) {
            by_binding.entry(binding.to_string()).or_default().push(i);
        }
    }

    let mut demote: Vec<(usize, String)> = Vec::new();
    for (binding, indices) in by_binding {
        if indices.len() < 2 {
            continue;
        }
        // The binding belongs to the import it was named after, when there is
        // one (`import infoLog from "infoLog"`), else to the first.
        let keeper = indices
            .iter()
            .copied()
            .find(|&i| {
                parse_import_line(&imports[i])
                    .1
                    .is_some_and(|m| sanitize_import_name(m) == binding)
            })
            .unwrap_or(indices[0]);
        for i in indices {
            if i == keeper {
                continue;
            }
            if let Some(module) = parse_import_line(&imports[i]).1 {
                demote.push((i, format!("import \"{module}\";")));
            }
        }
    }

    for (i, line) in demote {
        imports[i] = line;
    }

    // Demotion can turn two different lines into the same side-effect import.
    let mut seen = HashSet::new();
    imports.retain(|line| seen.insert(line.clone()));
}

/// The name a top-level declaration line introduces (`export const x = …`,
/// `function x(…)`, `class x …`), if any.
fn declared_name(line: &str) -> Option<&str> {
    // Indented lines are nested inside another declaration, not top level.
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = line.strip_prefix("export ").unwrap_or(line);
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    let rest = ["const", "let", "var", "function", "class"]
        .iter()
        .find_map(|kw| rest.strip_prefix(kw))
        // The keyword has to end there: `constant = 1` declares nothing.
        .filter(|rest| rest.starts_with(char::is_whitespace) || rest.starts_with('*'))?;
    let name = rest
        .trim_start_matches('*')
        .trim_start()
        .split(|c: char| c == '(' || c == '=' || c == ';' || c.is_whitespace())
        .next()?;
    crate::util::is_valid_identifier(name).then_some(name)
}

/// `export { prop as name } from "module";`, spelled the short way when the
/// exported name already matches the property.
fn format_reexport(prop: &str, name: &str, module: &str) -> String {
    if prop == name {
        format!("export {{ {name} }} from \"{module}\";")
    } else {
        format!("export {{ {prop} as {name} }} from \"{module}\";")
    }
}

/// Resolve a name shared by an import binding and a declaration, which is as
/// unparseable as two identical import bindings.
///
/// Most of these are re-exports the classifier spelled out longhand — a barrel
/// binds a dependency under the dependency's own name and then re-exports it:
/// `import DrawerActions from "DrawerActions"; export const DrawerActions =
/// DrawerActions.default;`. Rewriting that to a real `export … from` clause
/// removes the local declaration, so the collision goes with it, and the line
/// says what it means. Where the initializer cannot be expressed as a re-export
/// the declaration keeps the name and the import is demoted to a side-effect
/// import, the same policy `dedupe_import_bindings` uses.
fn resolve_import_declaration_collisions(
    imports: &mut [String],
    body_stmts: &[String],
    exports: &mut [String],
) {
    use std::collections::HashMap;

    // binding → (index in `imports`, module it comes from)
    let mut bindings: HashMap<String, (usize, String)> = HashMap::new();
    for (i, line) in imports.iter().enumerate() {
        if let (Some(binding), Some(module)) = parse_import_line(line) {
            bindings.insert(binding.to_string(), (i, module.to_string()));
        }
    }
    if bindings.is_empty() {
        return;
    }

    let mut demote: Vec<usize> = Vec::new();

    for exp in exports.iter_mut() {
        let Some(name) = declared_name(exp) else {
            continue;
        };
        let Some(&(import_idx, _)) = bindings.get(name) else {
            continue;
        };

        // `export const name = <initializer>;`
        let initializer = exp
            .strip_prefix("export const ")
            .and_then(|rest| rest.split_once(" = "))
            .filter(|(declared, _)| *declared == name)
            .map(|(_, init)| init.trim().trim_end_matches(';'));

        let rewritten = initializer.and_then(|init| {
            let (root, prop) = match init.split_once('.') {
                Some((root, prop)) => (root, prop),
                // `export const x = dep;` re-exports that module's default.
                None => (init, "default"),
            };
            if !crate::util::is_valid_identifier(root) || !crate::util::is_valid_identifier(prop) {
                return None;
            }
            match bindings.get(root) {
                Some((_, module)) => Some(format_reexport(prop, name, module)),
                // A local value can still be exported without redeclaring it,
                // but only when the whole initializer is that one name.
                None if init == root => Some(format!("export {{ {root} as {name} }};")),
                None => None,
            }
        });

        match rewritten {
            Some(line) => *exp = line,
            None => demote.push(import_idx),
        }
    }

    // Declarations in the body cannot be turned into re-exports at all.
    for body in body_stmts {
        for line in body.lines() {
            if let Some(name) = declared_name(line) {
                if let Some(&(import_idx, _)) = bindings.get(name) {
                    demote.push(import_idx);
                }
            }
        }
    }

    for i in demote {
        if let Some(module) = parse_import_line(&imports[i]).1 {
            imports[i] = format!("import \"{module}\";");
        }
    }
}

impl Codegen {
    // Map each generic import binding (`closure_3 = require(id)`) to its module's
    // name so it renders as `import <module> from "<module>"`. Multiple captures of
    // the same module map to the same name (they hold the same value, so merging is
    // correct). A target that collides with an unrelated existing binding, or a
    // module whose inferred name is generic, is skipped.
    fn import_binding_renames(&self, statements: &[Statement]) -> std::collections::BTreeMap<String, String> {
        use crate::ir::{AssignTarget, Expression, Value, Visitor};
        use std::collections::{BTreeMap, HashSet};

        // Every variable name referenced in the body (collision guard).
        let mut body_vars: HashSet<String> = HashSet::new();
        struct V<'a>(&'a mut HashSet<String>);
        impl<'a> Visitor<'a> for V<'_> {
            fn visit_expression(&mut self, e: &'a Expression) {
                if let Expression::Value(Value::Variable(n)) = e {
                    self.0.insert(n.clone());
                }
                self.walk_expression(e);
            }
        }
        {
            let mut v = V(&mut body_vars);
            for s in statements {
                v.visit_statement(s);
            }
        }

        let mut renames: BTreeMap<String, String> = BTreeMap::new();
        // One binding per distinct module id, so two captures of the SAME module
        // merge but two DIFFERENT modules that inferred the same name never collapse.
        let mut id_to_binding: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        let mut used_targets: HashSet<String> = HashSet::new();

        for stmt in statements {
            let (name, value) = match stmt {
                Statement::Let { name, value, .. } => (name, value),
                Statement::Assign { target: AssignTarget::Variable(name), value } => (name, value),
                _ => continue,
            };
            if !is_generic_import_binding(name) {
                continue;
            }
            // Resolve (module name, absolute id) from `require(id)` or
            // `wrapper(require(id))`.
            let resolved = self.resolve_require_module_id(value).or_else(|| {
                if let Expression::Call { arguments, .. } = value {
                    Self::effective_args(arguments)
                        .iter()
                        .find_map(|a| self.resolve_require_module_id(a))
                } else {
                    None
                }
            });
            let Some((mod_name, id)) = resolved else { continue };
            let base = super::sanitize_import_name(&mod_name);
            if !crate::util::is_valid_identifier(&base) || is_bad_module_binding(&base) {
                continue;
            }

            let target = if let Some(existing) = id_to_binding.get(&id) {
                existing.clone()
            } else {
                // Uniquify against other assigned bindings and any unrelated body
                // variable of the same name (sources are `closure_N`, never a module
                // name, so a base colliding with body_vars is a genuine other var).
                let mut cand = base.clone();
                let mut i = 2u32;
                while used_targets.contains(&cand) || body_vars.contains(&cand) {
                    cand = format!("{base}{i}");
                    i += 1;
                }
                used_targets.insert(cand.clone());
                id_to_binding.insert(id, cand.clone());
                cand
            };
            renames.insert(name.clone(), target);
        }
        renames
    }
}

// A binding whose name is a decompiler placeholder that should take its module's
// name when it is an import.
fn is_generic_import_binding(name: &str) -> bool {
    if name.starts_with("closure_") {
        return true;
    }
    let result_stem = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if result_stem.ends_with("Result") {
        return true;
    }
    let digit_suffix = |prefix: &str| {
        name.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.chars().all(|c| c.is_ascii_digit()))
    };
    digit_suffix("tmp")
        || (name.starts_with('r') && name.len() > 1 && name[1..].chars().all(|c| c.is_ascii_digit()))
        || digit_suffix("obj")
        || digit_suffix("arr")
}

// Module names too generic to become a binding (would not read better than the
// placeholder).
fn is_bad_module_binding(name: &str) -> bool {
    crate::analysis::metro::is_generic_module_specifier(name)
        || matches!(name, "module" | "exports" | "default" | "require")
}

// Whether the rendered lines call `require` without binding it first. A property
// access (`x.require(...)`) or a longer identifier ending in `require` is not a
// call to the module loader.
fn body_calls_require(lines: &[String]) -> bool {
    let mut calls = false;
    for line in lines {
        for (idx, _) in line.match_indices("require") {
            let before = line[..idx].chars().next_back();
            if matches!(before, Some(c) if c == '.' || c == '_' || c.is_alphanumeric()) {
                continue;
            }
            let after = line[idx + "require".len()..].trim_start();
            if after.starts_with('(') {
                calls = true;
            }
            // `const require = ...`, `let require = ...`, `require = ...` or a
            // parameter list entry all bind the name, so nothing is dangling.
            if after.starts_with('=') && !after.starts_with("==") {
                return false;
            }
        }
    }
    calls
}

#[cfg(test)]
mod tests {
    use super::{
        declared_name, dedupe_import_bindings, parse_import_line,
        resolve_import_declaration_collisions,
    };

    #[test]
    fn parses_every_emitted_import_form() {
        let cases = [
            ("import foo from \"mod\";", (Some("foo"), Some("mod"))),
            ("import { foo } from \"mod\";", (Some("foo"), Some("mod"))),
            ("import { bar as foo } from \"mod\";", (Some("foo"), Some("mod"))),
            ("import { default as foo } from \"mod\";", (Some("foo"), Some("mod"))),
            ("import * as foo from \"mod\";", (Some("foo"), Some("mod"))),
            ("import \"mod\";", (None, Some("mod"))),
        ];
        for (line, want) in cases {
            assert_eq!(parse_import_line(line), want, "parsing {line}");
        }
    }

    #[test]
    fn identical_imports_collapse() {
        let mut imports = vec![
            "import polyfillGlobal from \"polyfillGlobal\";".to_string(),
            "import polyfillGlobal from \"polyfillGlobal\";".to_string(),
        ];
        dedupe_import_bindings(&mut imports);
        assert_eq!(imports, ["import polyfillGlobal from \"polyfillGlobal\";"]);
    }

    #[test]
    fn conflicting_binding_is_demoted_to_a_side_effect_import() {
        // Two IR variables named `toPropertyKey`, importing different modules.
        let mut imports = vec![
            "import toPropertyKey from \"toPropertyKey\";".to_string(),
            "import toPropertyKey from \"infoLog\";".to_string(),
        ];
        dedupe_import_bindings(&mut imports);
        assert_eq!(
            imports,
            [
                "import toPropertyKey from \"toPropertyKey\";",
                "import \"infoLog\";",
            ]
        );
    }

    #[test]
    fn the_module_the_name_came_from_keeps_the_binding() {
        // The conflicting import comes first here, so order must not decide it.
        let mut imports = vec![
            "import toPropertyKey from \"module_503\";".to_string(),
            "import toPropertyKey from \"toPropertyKey\";".to_string(),
        ];
        dedupe_import_bindings(&mut imports);
        assert_eq!(
            imports,
            [
                "import \"module_503\";",
                "import toPropertyKey from \"toPropertyKey\";",
            ]
        );
    }

    #[test]
    fn distinct_bindings_are_left_alone() {
        let imports = vec![
            "import a from \"one\";".to_string(),
            "import { x as b } from \"two\";".to_string(),
            "import \"three\";".to_string(),
        ];
        let mut deduped = imports.clone();
        dedupe_import_bindings(&mut deduped);
        assert_eq!(deduped, imports);
    }

    #[test]
    fn declared_names_are_read_from_top_level_only() {
        assert_eq!(declared_name("export const Foo = x;"), Some("Foo"));
        assert_eq!(declared_name("function Foo(a) {"), Some("Foo"));
        assert_eq!(declared_name("export async function* Foo(a) {"), Some("Foo"));
        assert_eq!(declared_name("class Foo extends Bar {"), Some("Foo"));
        assert_eq!(declared_name("let Foo;"), Some("Foo"));
        // Nested in another declaration, so it introduces no top-level name.
        assert_eq!(declared_name("  const Foo = x;"), None);
        assert_eq!(declared_name("export { Foo } from \"m\";"), None);
        assert_eq!(declared_name("export default x;"), None);
    }

    // A barrel binds a dependency under its own name and re-exports it, which
    // the classifier spells out longhand as a colliding `export const`.
    #[test]
    fn longhand_reexport_becomes_a_real_reexport() {
        let mut imports = vec!["import DrawerActions from \"DrawerActions\";".to_string()];
        let mut exports = vec!["export const DrawerActions = DrawerActions.default;".to_string()];
        resolve_import_declaration_collisions(&mut imports, &[], &mut exports);
        assert_eq!(exports, ["export { default as DrawerActions } from \"DrawerActions\";"]);
        // The import stays: the body may still use the binding.
        assert_eq!(imports, ["import DrawerActions from \"DrawerActions\";"]);
    }

    #[test]
    fn reexport_keeps_the_property_name() {
        let mut imports = vec!["import StackActions from \"StackActions\";".to_string()];
        let mut exports = vec!["export const StackActions = StackActions.StackActions;".to_string()];
        resolve_import_declaration_collisions(&mut imports, &[], &mut exports);
        assert_eq!(exports, ["export { StackActions } from \"StackActions\";"]);
    }

    #[test]
    fn initializer_may_come_from_a_different_import() {
        let mut imports = vec![
            "import checksum from \"checksum\";".to_string(),
            "import EAN2 from \"EAN2\";".to_string(),
        ];
        let mut exports = vec!["export const EAN2 = checksum.default;".to_string()];
        resolve_import_declaration_collisions(&mut imports, &[], &mut exports);
        assert_eq!(exports, ["export { default as EAN2 } from \"checksum\";"]);
    }

    #[test]
    fn a_local_value_is_exported_without_redeclaring_it() {
        let mut imports = vec!["import Constants from \"Constants\";".to_string()];
        let mut exports = vec!["export const Constants = obj;".to_string()];
        resolve_import_declaration_collisions(&mut imports, &[], &mut exports);
        assert_eq!(exports, ["export { obj as Constants };"]);
    }

    #[test]
    fn an_inexpressible_initializer_demotes_the_import() {
        let mut imports = vec!["import Button from \"Facebook\";".to_string()];
        let mut exports = vec!["export const Button = require(3003).ShareAsset;".to_string()];
        resolve_import_declaration_collisions(&mut imports, &[], &mut exports);
        // The declaration keeps the name, the dependency stays visible.
        assert_eq!(exports, ["export const Button = require(3003).ShareAsset;"]);
        assert_eq!(imports, ["import \"Facebook\";"]);
    }

    #[test]
    fn a_body_declaration_takes_the_name_from_the_import() {
        let mut imports = vec!["import getIteratorFn from \"getIteratorFn\";".to_string()];
        let body = vec!["function getIteratorFn(iterable) {
  return null;
}
".to_string()];
        let mut exports: Vec<String> = vec![];
        resolve_import_declaration_collisions(&mut imports, &body, &mut exports);
        assert_eq!(imports, ["import \"getIteratorFn\";"]);
    }

    #[test]
    fn non_colliding_declarations_are_left_alone() {
        let mut imports = vec!["import dep from \"dep\";".to_string()];
        let body = vec!["function helper() {}
".to_string()];
        let mut exports = vec!["export const value = dep.default;".to_string()];
        let (i0, e0) = (imports.clone(), exports.clone());
        resolve_import_declaration_collisions(&mut imports, &body, &mut exports);
        assert_eq!((imports, exports), (i0, e0));
    }
}

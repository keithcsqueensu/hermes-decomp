//! The environment model against ground truth: every fixture in
//! `tests/fixtures/env` is decompiled, run, and must print what its bytecode
//! prints (V98_ENV_PLAN.md E1).
//!
//! Each fixture isolates one way a closure reaches a variable (an async body,
//! a generator state, a block, a class, a parameter scope, deep nesting) and
//! prints a value from every closure, so a capture resolved to the wrong scope
//! prints a different value or throws. `<name>.vN.expected` is what hvm prints
//! for `<name>.vN.hbc` (`scripts/build/build_env_fixtures.sh` rebuilds both),
//! so the oracle is the bytecode's behaviour, not the source's.
//!
//! The decompiled program runs under node, wrapped with shims (`SHIMS`) and a
//! call to the `global` function the decompiler prints the top level as. No
//! node on PATH skips the run with a note.
//!
//! `KNOWN` lists the runs that fail today and why. It is a ratchet: a listed run
//! that starts passing fails the test until it is taken off the list, and an
//! unlisted run that fails is a regression.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use hbc_decomp::{BytecodeFile, BytecodeFormat, DecompileOptionsV2};

// (fixture, version, why it fails). Keep the reason to the observed cause;
// `env:` marks a cause in the environment model (V98_ENV_PLAN.md), `other:` one
// outside it that the fixture happens to reach first.
const KNOWN: &[(&str, u32, &str)] = &[
    ("lang_async_iteration", 98, "other: `for await` reads its iterator through a helper the output calls as a free `makeAsyncIterator`"),
    ("lang_async_iteration", 99, "other: `HermesBuiltin.setFunctionName`, a v99 builtin, is printed unfolded"),
    ("lang_class_fields", 98, "other: the getter and setter of one accessor are defined separately; the second throws `Cannot redefine property: upper`"),
    ("lang_class_fields", 99, "other: `HermesBuiltin.setFunctionName`, a v99 builtin, is printed unfolded"),
    ("lang_class_private", 98, "other: a static private member's brand is checked outside the class (`#Counter` must be declared in an enclosing class)"),
    ("lang_class_private", 99, "other: a static private member's brand is checked outside the class (`#Counter` must be declared in an enclosing class)"),
    ("lang_control_flow", 96, "other: `continue outer` out of a nested loop loses iterations (prints only `00` of the six pairs)"),
    ("lang_control_flow", 98, "other: `continue outer` out of a nested loop loses iterations (prints only `00` of the six pairs)"),
    ("lang_control_flow", 99, "other: `continue outer` out of a nested loop loses iterations (prints only `00` of the six pairs)"),
    ("lang_destructuring", 96, "other: the array pattern's iterator protocol is mis-structured; the rest of the program, `print` included, sits in a branch that never runs"),
    ("lang_destructuring", 98, "other: the array pattern's iterator protocol is mis-structured; the rest of the program, `print` included, sits in a branch that never runs"),
    ("lang_destructuring", 99, "other: the array pattern's iterator protocol is mis-structured; the rest of the program, `print` included, sits in a branch that never runs"),
    ("lang_iterators", 96, "other: the run never ends and exhausts memory (for-of with an early break over an iterable with `return`)"),
    ("lang_iterators", 98, "other: the run never ends and exhausts memory (for-of with an early break over an iterable with `return`)"),
    ("lang_iterators", 99, "other: `HermesBuiltin.setFunctionName`, a v99 builtin, is printed unfolded"),
    ("lang_object_literals", 98, "other: `super.greet()` of an object literal method is printed in a plain function expression, a syntax error"),
    ("lang_object_literals", 99, "other: `super.greet()` of an object literal method is printed in a plain function expression, a syntax error"),
    ("lang_optional_nullish", 96, "other: `out.push(...)` loses its receiver when the method is read through one binding and the receiver passed through another"),
    ("lang_optional_nullish", 98, "other: `out.push(...)` loses its receiver when the method is read through one binding and the receiver passed through another"),
    ("lang_optional_nullish", 99, "other: `out.push(...)` loses its receiver when the method is read through one binding and the receiver passed through another"),
    ("lang_regexp", 96, "other: `HermesBuiltin.initRegexNamedGroups` (named capture groups) is printed unfolded"),
    ("lang_regexp", 98, "other: `HermesBuiltin.initRegexNamedGroups` (named capture groups) is printed unfolded"),
    ("lang_regexp", 99, "other: `HermesBuiltin.initRegexNamedGroups` (named capture groups) is printed unfolded"),
    ("lang_templates", 96, "other: `HermesBuiltin.getTemplateObject` (tagged templates) is printed unfolded"),
    ("lang_templates", 98, "other: `HermesBuiltin.getTemplateObject` (tagged templates) is printed unfolded"),
    ("lang_templates", 99, "other: `HermesBuiltin.getTemplateObject` (tagged templates) is printed unfolded"),
    ("inlined_copies", 98, "env: an inlined factory's three copies each hold their own `tag`, but the copies' slots are named apart (`closure_0`) or together (`closure_0$2` for B and C, one binding), the shared inner function reads one of them (S3.3: one name per slot across a shared function's scopes, one binding per copy), and the object literal is folded above the stores that fill them"),
    ("inlined_copies", 99, "env: an inlined factory's three copies each hold their own `tag`, but the copies' slots are named apart (`closure_0`) or together (`closure_0$2` for B and C, one binding), the shared inner function reads one of them (S3.3: one name per slot across a shared function's scopes, one binding per copy), and the object literal is folded above the stores that fill them"),
    ("class_super", 98, "other: hermesc inlines the constructor at `new Own(4)`, so its private-field installs print outside the class, a syntax error; never seen in the four bundles (privateTwice 0). class_super_call covers the explicit `super(a, 1)` itself"),
    ("class_super", 99, "other: hermesc inlines the constructor at `new Own(4)`, so its private-field installs print outside the class, a syntax error; never seen in the four bundles (privateTwice 0). class_super_call covers the explicit `super(a, 1)` itself"),
];

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("env")
}

// What the CLI's `decompile` runs with by default.
fn options() -> DecompileOptionsV2 {
    DecompileOptionsV2 {
        resolve_strings: true,
        propagate: true,
        simplify: true,
        recover_structures: true,
        ..DecompileOptionsV2::default()
    }
}

fn decompile(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let file = BytecodeFile::parse_auto(&bytes).map_err(|e| format!("parse: {e}"))?;
    let format = BytecodeFormat::for_version(file.header.version)
        .map_err(|e| format!("no opcode table for v{}: {e}", file.header.version))?;
    hbc_decomp::decompile_all_v2_with_closures(&file, &format, &options())
        .map_err(|e| format!("decompile: {e}"))
}

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

// A generator machine the decompiler could not lift prints as
// `HermesBuiltin.createGenerator(step)`, the `CreateGenerator` instruction,
// which node lacks. The shim gives it the instruction's contract (`next`,
// `throw` and `return` call the step with 0, 1 and 2 and hand back its
// `{ value, done }`), so the run tests the machine itself. An async function
// whose machine does not lift keeps Hermes' `HermesBuiltin.spawnAsync(body,
// this, arguments)`, which drives the generator `body` returns as the runtime
// does.
const SHIMS: &str = "globalThis.print = (...a) => console.log(...a);
globalThis.HermesBuiltin = { createGenerator: (step) => ({ next: (v) => step(0, v), \
throw: (e) => step(1, e), return: (v) => step(2, v), [Symbol.iterator]() { return this; } }), \
spawnAsync: (body, self, args) => new Promise((resolve, reject) => { const it = body.apply(self, args); \
const go = (k, v) => { let r; try { r = it[k](v); } catch (e) { return reject(e); } \
if (r.done) resolve(r.value); else Promise.resolve(r.value).then((x) => go('next', x), (e) => go('throw', e)); }; \
go('next'); }) };
";

// Run `program` under node; stdout on a clean exit, the first error line
// otherwise. A decompiled state machine can loop, so the run is time-boxed.
fn run_node(program: &str, scratch: &Path) -> Result<String, String> {
    let script = scratch.with_extension("run.js");
    let wrapped = format!("{SHIMS}{program}\n;global();\n");
    std::fs::write(&script, wrapped).map_err(|e| e.to_string())?;
    let mut child = Command::new("node")
        .arg(&script)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    while child.try_wait().map_err(|e| e.to_string())?.is_none() {
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            return Err("timed out".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&script);
    let stdout = String::from_utf8_lossy(&out.stdout).replace('\r', "");
    if out.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let error = stderr
            .lines()
            .find(|l| l.contains("Error"))
            .unwrap_or("non-zero exit")
            .trim()
            .to_string();
        Err(error)
    }
}

#[test]
fn decompiled_fixtures_print_what_their_bytecode_prints() {
    if !node_available() {
        println!("  env_model: node not on PATH, skipping the runs");
        return;
    }
    let dir = fixture_dir();
    let scratch_dir = std::env::temp_dir().join(format!("env_model_{}", std::process::id()));
    std::fs::create_dir_all(&scratch_dir).unwrap();

    let mut hbcs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "hbc"))
        .collect();
    hbcs.sort();
    assert!(!hbcs.is_empty(), "no fixtures in {}", dir.display());

    let mut problems = Vec::new();
    let mut table = Vec::new();
    for hbc in &hbcs {
        // <name>.v<N>.hbc
        let stem = hbc.file_stem().unwrap().to_string_lossy().to_string();
        let (name, version) = stem.rsplit_once(".v").expect("<name>.v<N>.hbc");
        let version: u32 = version.parse().expect("version number");
        let expected = std::fs::read_to_string(hbc.with_extension("expected"))
            .unwrap_or_else(|e| panic!("{stem}: missing .expected: {e}"))
            .replace('\r', "");
        let outcome = decompile(hbc).and_then(|js| {
            let got = run_node(&js, &scratch_dir.join(&stem))?;
            if got == expected {
                Ok(())
            } else {
                Err(format!(
                    "printed {:?}, expected {:?}",
                    got.trim(),
                    expected.trim()
                ))
            }
        });
        let known = KNOWN
            .iter()
            .find(|(n, v, _)| *n == name && *v == version)
            .map(|(_, _, why)| *why);
        let status = match (&outcome, known) {
            (Ok(()), None) => "pass".to_string(),
            (Err(e), Some(why)) => format!("known: {why} ({e})"),
            (Ok(()), Some(why)) => {
                problems.push(format!("{stem} passes now: take it off KNOWN (was: {why})"));
                "PASSES (listed as known)".to_string()
            }
            (Err(e), None) => {
                problems.push(format!("{stem} fails: {e}"));
                format!("FAIL: {e}")
            }
        };
        table.push(format!("  {stem:<24} {status}"));
    }
    let _ = std::fs::remove_dir_all(&scratch_dir);
    println!("{}", table.join("\n"));
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

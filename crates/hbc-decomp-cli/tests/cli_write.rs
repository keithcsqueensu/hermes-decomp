//! The write commands, run as the binary is run (R17).
//!
//! The library's tests call the write ops directly, which skips the part a user
//! actually touches: argument parsing and resolution -- which flag addresses the
//! instruction, how a string value becomes an id -- and the command handler that
//! wires them to the op. Two kinds of test here:
//!
//!   * **Argument resolution**, no VM needed. Each way of naming the same thing
//!     must produce the same bytes; two ways at once, or none, must be refused at
//!     parse time with nothing written. Conflicting addressing used to be resolved
//!     silently (`--at` beat `--function`, `--string-id` beat `--string`), so a
//!     mistyped command patched something other than what it asked for.
//!   * **End to end**: each write command through the binary at v96/v98/v99, and
//!     the output run on a real engine when `HERMES_VM_V<N>` is set (required with
//!     `HBC_REQUIRE_ORACLES=vm`), asserting the program's exact output.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "../../hbc-decomp/tests/common/mod.rs"]
mod common;
use common::Oracle;

use hbc_decomp::{BytecodeFile, BytecodeFormat};

const VERSIONS: [u32; 3] = [96, 98, 99];
const PLAIN_OUTPUT: &str = "hi bob alpha\n10";
const HANDLERS_OUTPUT: &str = "no-throw: 3\nthrow: -1";

fn exe() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join(format!("hermes-decomp{}", std::env::consts::EXE_SUFFIX))
}

struct Run {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Run {
    let out = Command::new(exe())
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("running {args:?}: {e}"));
    Run {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        stderr: String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
    }
}

fn fixture(name: &str, version: u32) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../hbc-decomp/tests/fixtures")
        .join(format!("{name}.v{version}.hbc"))
}

fn out_path(label: &str) -> PathBuf {
    let d = std::env::temp_dir().join("hbc-decomp-cli-write");
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join(format!("{label}.hbc"));
    let _ = std::fs::remove_file(&p);
    p
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Run a write command that must succeed, and return what it wrote.
fn write(label: &str, args: &[&str]) -> Vec<u8> {
    let out = out_path(label);
    let mut argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    argv.extend(["-o".to_string(), s(&out)]);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let r = run(&argv);
    assert!(r.ok, "{label}: {args:?} failed:\n{}", r.stderr);
    std::fs::read(&out).unwrap_or_else(|e| panic!("{label}: no output: {e}"))
}

/// Run a write command that must be refused, and return its stderr. Nothing may
/// be written.
fn refused(label: &str, args: &[&str]) -> String {
    let out = out_path(label);
    let mut argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    argv.extend(["-o".to_string(), s(&out)]);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let r = run(&argv);
    assert!(!r.ok, "{label}: {args:?} should have been refused");
    assert!(
        r.stdout.is_empty(),
        "{label}: a refusal writes nothing to stdout"
    );
    assert!(
        !out.exists(),
        "{label}: a refused command must not write its output"
    );
    r.stderr
}

/// Run `image` on the engine for `version` and require exactly `expected` -- or
/// skip, when no engine is configured and none is required.
fn runs(version: u32, image: &[u8], expected: &str, label: &str) {
    let Some(vm) = common::oracle_path(
        Oracle::Vm,
        Some(version),
        |p| p.is_file(),
        "an existing file",
    ) else {
        common::skip_or_fail(Oracle::Vm, Some(version), &format!("not running {label}"));
        return;
    };
    let path = out_path(&format!("{label}-run"));
    std::fs::write(&path, image).unwrap();
    let out = Command::new(&vm).arg(&path).output().expect("run hvm");
    let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    assert!(
        out.status.success(),
        "{label}: v{version} engine exited {:?}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout.trim_end(), expected, "{label}: v{version} output");
}

/// Absolute offset, and offset within its function, of the first `opcode`
/// instruction in function `id`.
fn find(file: &BytecodeFile, id: u32, opcode: &str) -> (u32, u32) {
    let format = BytecodeFormat::for_version(file.header.version).unwrap();
    let base = file.function_headers[id as usize].offset();
    let insn = file
        .decode_function_instructions(&format, id)
        .unwrap()
        .into_iter()
        .find(|i| format.definitions[i.opcode as usize].name == opcode)
        .unwrap_or_else(|| panic!("no {opcode} in fn#{id}"));
    (base + insn.offset, insn.offset)
}

fn string_id(file: &BytecodeFile, value: &str) -> u32 {
    file.strings.iter().position(|s| s.value == value).unwrap() as u32
}

// ---------------------------------------------------------------------------
// Argument resolution
// ---------------------------------------------------------------------------

/// Every way of naming the same instruction or string writes the same bytes.
#[test]
fn equivalent_arguments_write_identical_output() {
    let input = fixture("plain", 96);
    let i = s(&input);
    let file = BytecodeFile::parse_auto(&std::fs::read(&input).unwrap()).unwrap();
    let (abs, rel) = find(&file, 0, "LoadConstString");
    let twice = string_id(&file, "twice").to_string();
    let (abs_hex, abs_dec, rel_hex) = (format!("{abs:#x}"), abs.to_string(), format!("{rel:#x}"));

    let by_at = write(
        "op-at",
        &["patch-operand", &i, "--at", &abs_hex, "--string", "twice"],
    );
    for (label, args) in [
        ("op-at-dec", vec!["--at", &abs_dec, "--string", "twice"]),
        (
            "op-rel",
            vec![
                "--function",
                "0",
                "--insn-offset",
                &rel_hex,
                "--string",
                "twice",
            ],
        ),
        ("op-id", vec!["--at", &abs_hex, "--string-id", &twice]),
    ] {
        let mut argv = vec!["patch-operand", i.as_str()];
        argv.extend(args);
        assert_eq!(
            write(label, &argv),
            by_at,
            "{label} differs from --at {abs_hex} --string twice"
        );
    }

    let alpha = string_id(&file, "alpha").to_string();
    let by_value = write(
        "rt-value",
        &["retarget-string", &i, "--from", "alpha", "--to", "twice"],
    );
    let by_id = write(
        "rt-id",
        &[
            "retarget-string",
            &i,
            "--from-id",
            &alpha,
            "--to-id",
            &twice,
        ],
    );
    assert_eq!(by_value, by_id, "retarget by value and by id");

    let by_old = write(
        "ps-old",
        &["patch-string", &i, "--old", "alpha", "--new", "BRAVO"],
    );
    let by_id = write(
        "ps-id",
        &["patch-string", &i, "--id", &alpha, "--new", "BRAVO"],
    );
    assert_eq!(by_old, by_id, "patch-string by value and by id");
}

/// Two ways at once, or none, are refused by the parser and write nothing.
#[test]
fn conflicting_or_missing_arguments_are_refused() {
    let i = s(&fixture("plain", 96));
    let cases: [(&str, Vec<&str>, &str); 9] = [
        (
            "at+function",
            vec![
                "patch-operand",
                &i,
                "--at",
                "0x10",
                "--function",
                "0",
                "--insn-offset",
                "0",
                "--string",
                "twice",
            ],
            "cannot be used with",
        ),
        (
            "function-alone",
            vec!["patch-operand", &i, "--function", "0", "--string", "twice"],
            "--insn-offset",
        ),
        (
            "offset-alone",
            vec![
                "patch-operand",
                &i,
                "--insn-offset",
                "0",
                "--string",
                "twice",
            ],
            "--function",
        ),
        (
            "no-target",
            vec!["patch-operand", &i, "--string", "twice"],
            "--at",
        ),
        (
            "string+id",
            vec![
                "patch-operand",
                &i,
                "--at",
                "0x10",
                "--string",
                "twice",
                "--string-id",
                "1",
            ],
            "cannot be used with",
        ),
        (
            "no-value",
            vec!["patch-operand", &i, "--at", "0x10"],
            "--string",
        ),
        (
            "from+from-id",
            vec![
                "retarget-string",
                &i,
                "--from",
                "alpha",
                "--from-id",
                "1",
                "--to",
                "twice",
            ],
            "cannot be used with",
        ),
        (
            "no-to",
            vec!["retarget-string", &i, "--from", "alpha"],
            "--to",
        ),
        (
            "id+old",
            vec![
                "patch-string",
                &i,
                "--id",
                "1",
                "--old",
                "alpha",
                "--new",
                "x",
            ],
            "cannot be used with",
        ),
    ];
    for (label, args, says) in cases {
        let err = refused(label, &args);
        assert!(err.contains(says), "{label}: expected {says:?} in:\n{err}");
    }
}

/// Values that name nothing are refused by the command, with the reason.
#[test]
fn values_that_name_nothing_are_refused() {
    let input = fixture("plain", 96);
    let i = s(&input);
    let file = BytecodeFile::parse_auto(&std::fs::read(&input).unwrap()).unwrap();
    let at = format!("{:#x}", find(&file, 0, "LoadConstString").0);
    let past = file.strings.len().to_string();
    for (label, args, says) in [
        (
            "op-unknown-string",
            vec![
                "patch-operand",
                &i,
                "--at",
                &at,
                "--string",
                "no such string",
            ],
            "add-string",
        ),
        (
            "op-id-range",
            vec!["patch-operand", &i, "--at", &at, "--string-id", &past],
            "out of range",
        ),
        (
            "rt-unknown",
            vec![
                "retarget-string",
                &i,
                "--from",
                "no such string",
                "--to",
                "twice",
            ],
            "not found",
        ),
        (
            "ps-unknown",
            vec!["patch-string", &i, "--old", "no such string", "--new", "x"],
            "not found",
        ),
        (
            "op-not-string",
            vec![
                "patch-operand",
                &i,
                "--at",
                &format!("{:#x}", find(&file, 0, "Ret").0),
                "--string",
                "twice",
            ],
            "no string operand",
        ),
    ] {
        let err = refused(label, &args);
        assert!(err.contains(says), "{label}: expected {says:?} in:\n{err}");
    }
}

// ---------------------------------------------------------------------------
// End to end, on a real engine
// ---------------------------------------------------------------------------

/// The string commands, as typed, on every fixture version.
#[test]
fn string_commands_run_end_to_end() {
    for v in VERSIONS {
        let i = s(&fixture("plain", v));
        let label = |what: &str| format!("{what}-v{v}");
        let out = write(
            &label("ps-same"),
            &["patch-string", &i, "--old", "alpha", "--new", "BRAVO"],
        );
        runs(v, &out, "hi bob BRAVO\n10", &label("ps-same"));
        let out = write(
            &label("ps-grow"),
            &[
                "patch-string",
                &i,
                "--old",
                "alpha",
                "--new",
                "alphabetical",
            ],
        );
        runs(v, &out, "hi bob alphabetical\n10", &label("ps-grow"));
        let out = write(
            &label("ps-utf16"),
            &["patch-string", &i, "--old", "alpha", "--new", "élan"],
        );
        runs(v, &out, "hi bob élan\n10", &label("ps-utf16"));
        let out = write(
            &label("rt"),
            &["retarget-string", &i, "--from", "alpha", "--to", "twice"],
        );
        runs(v, &out, "hi bob twice\n10", &label("rt"));

        // add-string, then point the global's `tag` load at the new string.
        let added = out_path(&label("add"));
        let r = run(&["add-string", &i, "--value", "omega", "-o", &s(&added)]);
        assert!(r.ok, "add-string: {}", r.stderr);
        let id = r.stdout.trim().to_string();
        let file = BytecodeFile::parse_auto(&std::fs::read(&added).unwrap()).unwrap();
        let (at, _) = find(&file, 0, "LoadConstString");
        assert_eq!(
            file.strings[string_id(&file, "omega") as usize].value,
            "omega"
        );
        let out = write(
            &label("op"),
            &[
                "patch-operand",
                &s(&added),
                "--at",
                &format!("{at:#x}"),
                "--string-id",
                &id,
            ],
        );
        runs(v, &out, "hi bob omega\n10", &label("op"));
    }
}

/// The function commands, as typed: a stub, a HASM edit inside a try region,
/// and a created image.
#[test]
fn function_commands_run_end_to_end() {
    for v in VERSIONS {
        let label = |what: &str| format!("{what}-v{v}");
        let plain = s(&fixture("plain", v));
        let out = write(
            &label("log"),
            &["inject-stub", &plain, "--function", "1", "--kind", "log"],
        );
        runs(v, &out, "greet\nhi bob alpha\n10", &label("log"));
        let out = write(
            &label("nop"),
            &[
                "inject-stub",
                &plain,
                "--function",
                "2",
                "--kind",
                "nop",
                "--allow-stale-debug-info",
            ],
        );
        runs(v, &out, PLAIN_OUTPUT, &label("nop"));

        // emit-hasm, three no-ops at the first handler's start, asm.
        let handlers = s(&fixture("handlers", v));
        let r = run(&["emit-hasm", &handlers, "--function", "1"]);
        assert!(r.ok, "emit-hasm: {}", r.stderr);
        let start = r
            .stdout
            .lines()
            .find_map(|l| l.strip_prefix(".handler "))
            .and_then(|l| l.split(',').next())
            .expect("emit-hasm writes the handlers")
            .trim()
            .to_string();
        let anchor = format!("{start}:\n");
        let edited = r.stdout.replacen(
            &anchor,
            &format!("{anchor}AsyncBreakCheck\nAsyncBreakCheck\nAsyncBreakCheck\n"),
            1,
        );
        let hasm = out_path(&label("hasm")).with_extension("hasm");
        std::fs::write(&hasm, edited).unwrap();
        let out = write(
            &label("asm"),
            &[
                "asm",
                &handlers,
                &s(&hasm),
                "--function",
                "1",
                "--allow-stale-debug-info",
            ],
        );
        runs(v, &out, HANDLERS_OUTPUT, &label("asm"));

        let out = write(&label("create"), &["create", "--version", &v.to_string()]);
        runs(v, &out, "", &label("create"));
    }
}

/// The RegExp commands, as typed: replace an entry with a donor's, and point a
/// site at another entry.
#[test]
fn regexp_commands_run_end_to_end() {
    for v in VERSIONS {
        let label = |what: &str| format!("{what}-v{v}");
        let host = s(&fixture("regexp", v));
        let donor = s(&fixture("regexp_donor", v));
        let out = write(
            &label("rx-set"),
            &["regexp-set", &host, "--index", "0", "--donor", &donor],
        );
        runs(
            v,
            &out,
            "a false true\nb true false false\nb.source x /",
            &label("rx-set"),
        );

        let file = BytecodeFile::parse_auto(&std::fs::read(fixture("regexp", v)).unwrap()).unwrap();
        let format = BytecodeFormat::for_version(v).unwrap();
        let base = file.function_headers[0].offset();
        let site = file
            .decode_function_instructions(&format, 0)
            .unwrap()
            .into_iter()
            .filter(|i| format.definitions[i.opcode as usize].name == "CreateRegExp")
            .nth(1)
            .map(|i| base + i.offset)
            .expect("second CreateRegExp");
        let out = write(
            &label("rx-repoint"),
            &[
                "regexp-repoint",
                &host,
                "--at",
                &format!("{site:#x}"),
                "--index",
                "0",
            ],
        );
        runs(
            v,
            &out,
            "a true false\nb false false true\nb.source ^equinox-(\\d+)$ /i",
            &label("rx-repoint"),
        );
    }
}

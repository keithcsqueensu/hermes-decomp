//! The stdout/stderr split, asserted.
//!
//! The contract (docs/WRITE_PATH_GUIDE.md, "Stdout/stderr discipline"):
//!
//!   * **stdout is the requested output data, and nothing else** — the
//!     machine-consumable result the invocation was *for*. A command that only
//!     transforms a file into `-o` writes nothing to stdout at all.
//!   * **stderr is the diagnostics channel.** Redirecting or discarding it must
//!     never change the captured data.
//!
//! This is load-bearing for scripting: `id=$(hermes-decomp add-string …)` has to
//! capture the id and only the id. It is also not a designed-in convention — it
//! arrived as a bug fix (`316741f`, finding F3), when `add-string` was found to be
//! printing `"added string id {id}"` to stdout and defeating exactly that. So a
//! new command does not inherit the rule by default, and until now nothing checked
//! it. Prose ages; these assertions do not.
//!
//! Needs no Hermes VM: every fixture is built by `create` or read from the
//! committed test fixtures.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn exe() -> PathBuf {
    // The integration-test binary lives next to the CLI binary under target/.
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop(); // deps/
    if p.ends_with("deps") {
        p.pop();
    }
    p.join(format!("hermes-decomp{}", std::env::consts::EXE_SUFFIX))
}

struct Run {
    stdout: String,
    stderr: String,
    ok: bool,
}

fn run(args: &[&str]) -> Run {
    let out: Output = Command::new(exe())
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("running {:?} {args:?}: {e}", exe()));
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        stderr: String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
        ok: out.status.success(),
    }
}

fn tmp_dir() -> PathBuf {
    let d = std::env::temp_dir().join("hbc-decomp-stdout-contract");
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../hbc-decomp/tests/fixtures")
        .join(name)
}

/// Build a scratch .hbc to mutate, so tests never write to the fixtures.
fn scratch(label: &str) -> PathBuf {
    let src = fixture("plain.v96.hbc");
    let dst = tmp_dir().join(format!("{label}.hbc"));
    std::fs::copy(&src, &dst).unwrap_or_else(|e| panic!("copying {}: {e}", src.display()));
    dst
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// stdout carries data, and only data
// ---------------------------------------------------------------------------

/// F3's exact bug: `add-string` must put the bare numeric id on stdout so
/// `id=$(…)` works. Human text belongs on stderr.
#[test]
fn add_string_emits_a_bare_id_on_stdout() {
    let input = scratch("add");
    let out = tmp_dir().join("add-out.hbc");
    let r = run(&[
        "add-string",
        "--value",
        "CONTRACT",
        "-o",
        &s(&out),
        &s(&input),
    ]);
    assert!(r.ok, "add-string failed: {}", r.stderr);

    let captured = r.stdout.trim();
    let id: u32 = captured.parse().unwrap_or_else(|_| {
        panic!(
            "stdout must be a bare id and nothing else, so `id=$(...)` captures a \
             number. Got {captured:?}\nstderr was: {}",
            r.stderr
        )
    });
    assert!(id > 0, "id should be the appended index");
    // The human-readable confirmation still has to exist -- just not on stdout.
    assert!(
        r.stderr.contains("CONTRACT"),
        "the status line should name the added string on stderr, got: {}",
        r.stderr
    );
}

/// Commands whose only product is a file must keep stdout completely empty --
/// otherwise piping them into anything that parses stdout breaks.
#[test]
fn file_transforming_commands_write_nothing_to_stdout() {
    let cases: Vec<(&str, Vec<String>)> = vec![
        (
            "create",
            vec![
                "create".into(),
                "--version".into(),
                "96".into(),
                "-o".into(),
                s(&tmp_dir().join("c.hbc")),
            ],
        ),
        (
            "patch-string",
            vec![
                "patch-string".into(),
                "--old".into(),
                "alpha".into(),
                "--new".into(),
                "BRAVO".into(),
                "-o".into(),
                s(&tmp_dir().join("ps.hbc")),
                s(&scratch("ps-in")),
            ],
        ),
        (
            "retarget-string",
            vec![
                "retarget-string".into(),
                "--from".into(),
                "alpha".into(),
                "--to".into(),
                "twice".into(),
                "-o".into(),
                s(&tmp_dir().join("rt.hbc")),
                s(&scratch("rt-in")),
            ],
        ),
        (
            "patch-operand",
            vec![
                "patch-operand".into(),
                "--function".into(),
                "1".into(),
                "--insn-offset".into(),
                "0".into(),
                "--string".into(),
                "twice".into(),
                "-o".into(),
                s(&tmp_dir().join("po.hbc")),
                s(&scratch("po-in")),
            ],
        ),
    ];

    for (name, args) in cases {
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let r = run(&argv);
        assert!(r.ok, "{name} failed: {}", r.stderr);
        assert!(
            r.stdout.is_empty(),
            "{name} produces a file, so stdout must stay empty. Got:\n{}",
            r.stdout
        );
        assert!(
            !r.stderr.trim().is_empty(),
            "{name} should still report what it did, on stderr"
        );
    }
}

/// `emit-hasm` without `-o` is a data-producing command: the HASM text *is* the
/// result, so it belongs on stdout.
#[test]
fn emit_hasm_without_output_puts_hasm_on_stdout() {
    let r = run(&[
        "emit-hasm",
        "--function",
        "1",
        &s(&fixture("plain.v96.hbc")),
    ]);
    assert!(r.ok, "emit-hasm failed: {}", r.stderr);
    assert!(
        r.stdout.contains("Ret") || r.stdout.contains("LoadParam"),
        "HASM text should be on stdout, got:\n{}\nstderr:\n{}",
        r.stdout,
        r.stderr
    );
}

/// Discarding stderr must not change what stdout carries. If it does, data and
/// diagnostics are entangled and the split is not real.
#[test]
fn discarding_stderr_does_not_change_stdout() {
    let input = scratch("stderr-indep");
    let out = tmp_dir().join("stderr-indep-out.hbc");
    let args = ["add-string", "--value", "INDEP", "-o", &s(&out), &s(&input)];

    let with = run(&args);
    let without = Command::new(exe())
        .args(args)
        .stderr(std::process::Stdio::null())
        .output()
        .expect("run with stderr discarded");
    assert_eq!(
        with.stdout,
        String::from_utf8_lossy(&without.stdout).replace("\r\n", "\n"),
        "stdout changed when stderr was discarded"
    );
}

// ---------------------------------------------------------------------------
// failures go to the exit code, not to stdout
// ---------------------------------------------------------------------------

/// Errors bubble as `Result` to `main`, which sets a non-zero exit code. A
/// consumer parses stdout for data and reads the exit code for success -- so a
/// failing command must not leave half-written data on stdout to be misread as a
/// result.
#[test]
fn errors_use_the_exit_code_and_keep_stdout_clean() {
    let cases: Vec<(&str, Vec<String>)> = vec![
        (
            "missing input file",
            vec!["info".into(), s(&tmp_dir().join("definitely-not-here.hbc"))],
        ),
        (
            "string not in table",
            vec![
                "patch-string".into(),
                "--old".into(),
                "NOT_IN_THE_TABLE".into(),
                "--new".into(),
                "x".into(),
                "-o".into(),
                s(&tmp_dir().join("err.hbc")),
                s(&scratch("err-in")),
            ],
        ),
        (
            "unknown modern layout",
            vec![
                "create".into(),
                "--version".into(),
                "100".into(),
                "-o".into(),
                s(&tmp_dir().join("err100.hbc")),
            ],
        ),
    ];

    for (what, args) in cases {
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let r = run(&argv);
        assert!(!r.ok, "{what}: expected a non-zero exit");
        assert!(
            r.stdout.trim().is_empty(),
            "{what}: a failing command must not write to stdout. Got:\n{}",
            r.stdout
        );
        assert!(
            !r.stderr.trim().is_empty(),
            "{what}: the failure should be explained on stderr"
        );
    }
}

/// The modern-write note is the single most frequently emitted line the tool
/// produces, and it spent a long time pointing users at a build script that has
/// never existed in this repo (R20). Assert the paths it names are real, since
/// nothing else does.
#[test]
fn modern_write_note_points_at_paths_that_exist() {
    let r = run(&[
        "create",
        "--version",
        "99",
        "-o",
        &s(&tmp_dir().join("note.hbc")),
    ]);
    assert!(r.ok, "create v99 failed: {}", r.stderr);
    assert!(
        r.stderr.contains("note: modern HBC"),
        "expected the modern-write note, got: {}",
        r.stderr
    );

    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for word in r.stderr.split_whitespace() {
        let candidate = word.trim_matches(|c: char| !c.is_ascii_graphic() || c == ',' || c == '.');
        // Only check things that look like repo-relative paths we point at.
        if candidate.contains('/') && (candidate.ends_with(".ps1") || candidate.ends_with(".rs")) {
            assert!(
                repo.join(candidate).is_file(),
                "the note points at {candidate}, which does not exist in the repo. \
                 That is exactly how the dead build_hermes_v98_toolchain.sh reference \
                 survived; update the note or add the file."
            );
        }
    }
}

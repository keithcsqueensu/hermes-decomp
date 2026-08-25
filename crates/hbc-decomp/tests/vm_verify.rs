//! VM-backed verification of the write path.
//!
//! Every other test in this crate asserts that patched output *reparses*. That is
//! a much weaker claim than it looks: all three defects recorded as R8/R9/R15 in
//! docs/WRITE_PATH_GUIDE.md produced images that reparsed perfectly and were
//! rejected or mis-executed by the real engine. These tests close that gap by
//! running the output on a real `hvm` binary and asserting on its stdout and exit
//! code.
//!
//! ## Running them
//!
//! Point one env var per bytecode version at an `hvm` executable:
//!
//! ```text
//! HERMES_VM_V96=C:\src\hermes-v96\build\bin\Release\hvm.exe
//! HERMES_VM_V98=C:\src\hermes-v98\build\bin\Release\hvm.exe
//! HERMES_VM_V99=C:\src\hermes\build\bin\Release\hvm.exe
//! ```
//!
//! `scripts/build_hermes_vm.ps1` builds these and prints the exact lines to set.
//!
//! An `hvm` only accepts its own bytecode version ("Wrong bytecode version.
//! Expected 99 but got 96"), so there is no single binary that covers everything
//! and the per-version split is inherent, not a convenience.
//!
//! ## When no VM is configured
//!
//! The tests still run and still assert everything that does not need a VM (the
//! handler guard firing correctly, ops succeeding, output reparsing). Only the
//! "and it runs" assertion is skipped, with a printed note. This is deliberate:
//! CI without a Hermes build degrades to today's coverage rather than failing.

use std::path::PathBuf;
use std::process::Command;

use hbc_decomp::write::patch::PatchOptions;
use hbc_decomp::{
    add_string, create_minimal, inject_stub, patch_string_replace, retarget_string, BytecodeFile,
    BytecodeFormat, CreateOptions, InjectStubKind,
};

// Versions with committed fixtures, one per distinct header layout the write
// path has to handle:
//   96 -- Legacy16, and the version the Equinox bundles actually use
//   98 -- Modern12 with the 37-byte large header (NumCacheNewObject present)
//   99 -- Modern12 with the 36-byte large header (NumCacheNewObject removed)
// v98 and v99 are the two arms of ModernLayout, so both must be exercised or the
// descriptor is only half tested.
const FIXTURE_VERSIONS: [u32; 3] = [96, 98, 99];

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn fixture(name: &str, version: u32) -> Vec<u8> {
    let path = fixture_dir().join(format!("{name}.v{version}.hbc"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("missing fixture {}: {e}", path.display()))
}

fn format_for(file: &BytecodeFile) -> BytecodeFormat {
    BytecodeFormat::for_version(file.header.version)
        .expect("bundled opcode table for fixture version")
}

/// Path to an `hvm` for `version`, or `None` if the env var is unset or does not
/// point at a file that exists.
fn vm_for(version: u32) -> Option<PathBuf> {
    let raw = std::env::var(format!("HERMES_VM_V{version}")).ok()?;
    let path = PathBuf::from(raw);
    path.is_file().then_some(path)
}

struct VmRun {
    status: i32,
    stdout: String,
    stderr: String,
}

fn run_on_vm(vm: &PathBuf, image: &[u8], label: &str) -> VmRun {
    // `hvm` takes a path, so the image has to hit disk. Keep the temp file next
    // to the target dir rather than in the source tree.
    let dir = std::env::temp_dir().join("hbc-decomp-vm-verify");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{label}.hbc"));
    std::fs::write(&path, image).expect("write image");

    let out = Command::new(vm)
        .arg(&path)
        .output()
        .unwrap_or_else(|e| panic!("failed to run {}: {e}", vm.display()));
    let _ = std::fs::remove_file(&path);

    VmRun {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        stderr: String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
    }
}

/// Assert `image` runs cleanly and prints exactly `expected`.
///
/// Returns `false` (having asserted nothing) when no VM is configured for the
/// version, so callers can report the skip.
fn assert_runs(version: u32, image: &[u8], expected: &str, label: &str) -> bool {
    let Some(vm) = vm_for(version) else {
        println!("  [skip] no HERMES_VM_V{version}; not running {label}");
        return false;
    };
    let run = run_on_vm(&vm, image, label);
    assert_eq!(
        run.status, 0,
        "{label}: v{version} VM exited {} \nstdout: {}\nstderr: {}",
        run.status, run.stdout, run.stderr
    );
    assert_eq!(
        run.stdout.trim_end(),
        expected.trim_end(),
        "{label}: v{version} VM output differs\nstderr: {}",
        run.stderr
    );
    true
}

const PLAIN_OUTPUT: &str = "hi bob alpha\n10";
const HANDLERS_OUTPUT: &str = "no-throw: 3\nthrow: -1";

// ---------------------------------------------------------------------------
// Baseline: the fixtures themselves, and the VM wiring
// ---------------------------------------------------------------------------

#[test]
fn fixtures_run_unmodified() {
    for version in FIXTURE_VERSIONS {
        assert_runs(
            version,
            &fixture("plain", version),
            PLAIN_OUTPUT,
            &format!("plain-baseline-v{version}"),
        );
        assert_runs(
            version,
            &fixture("handlers", version),
            HANDLERS_OUTPUT,
            &format!("handlers-baseline-v{version}"),
        );
    }
}

// ---------------------------------------------------------------------------
// R9 -- the exception-handler guard, in both directions
// ---------------------------------------------------------------------------

/// Find the functions that really declare an exception-handler table.
fn functions_with_handlers(file: &BytecodeFile) -> Vec<u32> {
    (0..file.header.function_count)
        .filter(|id| {
            file.function_headers
                .get(*id as usize)
                .is_some_and(|h| h.has_exception_handler())
        })
        .collect()
}

/// The false-negative half of R9: a size-changing edit on a function that really
/// has handlers must be refused. Before the ModernLayout fix this passed on v96
/// and silently accepted the edit on v99, producing an image whose `catch` no
/// longer caught.
#[test]
fn size_change_on_real_handler_table_is_refused() {
    for version in FIXTURE_VERSIONS {
        let bytes = fixture("handlers", version);
        let file = BytecodeFile::parse_auto(&bytes).expect("parse handlers fixture");
        let with_handlers = functions_with_handlers(&file);
        assert!(
            !with_handlers.is_empty(),
            "v{version}: fixture should have at least one function with a handler \
             table, found none -- the header layout is being misread"
        );

        for id in with_handlers {
            let mut file = BytecodeFile::parse_auto(&bytes).expect("reparse");
            let format = format_for(&file);
            let err = inject_stub(
                &mut file,
                &format,
                id,
                InjectStubKind::NopPad,
                &PatchOptions::default(),
            )
            .expect_err(&format!(
                "v{version} fn#{id} declares handlers; a size-changing edit must be refused"
            ));
            assert!(
                err.to_string().contains("exception-handler table"),
                "v{version} fn#{id}: unexpected error {err}"
            );
        }
    }
}

/// The false-positive half of R9: functions with no handler table must not be
/// blocked. Before the fix, two of the three functions in the v99 `plain` fixture
/// were refused on the strength of a byte read past the end of the large header.
#[test]
fn handler_free_functions_accept_size_change_and_still_run() {
    for version in FIXTURE_VERSIONS {
        let bytes = fixture("plain", version);
        let probe = BytecodeFile::parse_auto(&bytes).expect("parse plain fixture");
        assert!(
            functions_with_handlers(&probe).is_empty(),
            "v{version}: the plain fixture must have no handler tables"
        );

        for id in 0..probe.header.function_count {
            let mut file = BytecodeFile::parse_auto(&bytes).expect("reparse");
            let format = format_for(&file);
            let out = inject_stub(
                &mut file,
                &format,
                id,
                InjectStubKind::NopPad,
                &PatchOptions::default(),
            )
            .unwrap_or_else(|e| panic!("v{version} fn#{id} has no handlers but was refused: {e}"));

            BytecodeFile::parse_auto(&out).expect("patched image reparses");
            assert_runs(
                version,
                &out,
                PLAIN_OUTPUT,
                &format!("nop-v{version}-fn{id}"),
            );
        }
    }
}

/// The corruption the guard exists to prevent, asserted end to end: injecting a
/// prologue at body offset 0 shifts every body-relative handler offset, so if the
/// guard ever stops firing the `catch` stops catching. This test would have
/// caught the v99 regression on its own.
#[test]
fn handler_bearing_function_is_never_silently_corrupted() {
    for version in FIXTURE_VERSIONS {
        let bytes = fixture("handlers", version);
        let probe = BytecodeFile::parse_auto(&bytes).expect("parse");
        for id in functions_with_handlers(&probe) {
            let mut file = BytecodeFile::parse_auto(&bytes).expect("reparse");
            let format = format_for(&file);
            match inject_stub(
                &mut file,
                &format,
                id,
                InjectStubKind::LogEntry,
                &PatchOptions::default(),
            ) {
                // Refused: correct, that is the guard doing its job.
                Err(_) => {}
                // Allowed: then handler relocation must have landed (Q3), and the
                // program must still take its catch path correctly. Anything else
                // is the silent corruption this whole test exists for.
                Ok(out) => {
                    assert_runs(
                        version,
                        &out,
                        HANDLERS_OUTPUT,
                        &format!("log-v{version}-fn{id}"),
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// R15 -- `create` must emit an image the engine will actually execute
// ---------------------------------------------------------------------------

/// A created image previously parsed fine and threw `TypeError: Class constructor
/// invoked without new` at entry on v99, because the flags byte was written at
/// the v98 offset. Parsing cannot see this; only running can.
#[test]
fn create_minimal_runs_on_vm() {
    for version in FIXTURE_VERSIONS {
        let image = create_minimal(&CreateOptions {
            version,
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("create v{version}: {e}"));

        BytecodeFile::parse_auto(&image).expect("created image reparses");
        // A minimal image runs its trivial global and prints nothing.
        assert_runs(version, &image, "", &format!("create-v{version}"));
    }
}

/// The layout allow-list must refuse rather than guess. A future modern version
/// silently reusing the newest known shape is precisely how R8 shipped.
#[test]
fn create_refuses_unknown_modern_version() {
    let err = create_minimal(&CreateOptions {
        version: 100,
        ..Default::default()
    })
    .expect_err("v100 layout is unknown and must not be extrapolated");
    let msg = err.to_string();
    assert!(
        msg.contains("100") && msg.contains("not known"),
        "unhelpful error: {msg}"
    );
}

// ---------------------------------------------------------------------------
// String write ops -- the paths that were already correct, now pinned
// ---------------------------------------------------------------------------

#[test]
fn string_write_ops_preserve_program_behaviour() {
    for version in FIXTURE_VERSIONS {
        let bytes = fixture("plain", version);

        // Same length.
        let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
        let format = format_for(&file);
        let out = patch_string_replace(&mut file, &format, "alpha", "BRAVO", &Default::default())
            .expect("same-length patch");
        assert_runs(
            version,
            &out,
            "hi bob BRAVO\n10",
            &format!("str-same-v{version}"),
        );

        // Grow -- shifts every function offset downstream.
        let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
        let out = patch_string_replace(
            &mut file,
            &format,
            "alpha",
            "alphaXXXXXX",
            &Default::default(),
        )
        .expect("grow patch");
        assert_runs(
            version,
            &out,
            "hi bob alphaXXXXXX\n10",
            &format!("str-grow-v{version}"),
        );

        // Shrink -- listed as a test gap in WRITE_PATH_GUIDE until now.
        let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
        let out = patch_string_replace(&mut file, &format, "alpha", "ab", &Default::default())
            .expect("shrink patch");
        assert_runs(
            version,
            &out,
            "hi bob ab\n10",
            &format!("str-shrink-v{version}"),
        );

        // ASCII -> UTF-16 (I7): content, not the old flag, picks the encoding.
        let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
        let out = patch_string_replace(&mut file, &format, "alpha", "élan", &Default::default())
            .expect("utf16 patch");
        assert_runs(
            version,
            &out,
            "hi bob élan\n10",
            &format!("str-utf16-v{version}"),
        );

        // Appending a string must not disturb anything that already ran.
        let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
        let (out, id) = add_string(&mut file, &format, "ZZTOP", false, &Default::default())
            .expect("add_string");
        assert_eq!(id, bytes_string_count(&bytes), "new id must be appended");
        assert_runs(version, &out, PLAIN_OUTPUT, &format!("add-str-v{version}"));

        // Retarget is metadata-only: one entry resolves to another's value.
        let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
        let from = string_id(&file, "alpha");
        let to = string_id(&file, "twice");
        let out =
            retarget_string(&mut file, &format, from, to, &Default::default()).expect("retarget");
        assert_runs(
            version,
            &out,
            "hi bob twice\n10",
            &format!("retarget-v{version}"),
        );
    }
}

fn bytes_string_count(bytes: &[u8]) -> u32 {
    BytecodeFile::parse_auto(bytes).unwrap().header.string_count
}

fn string_id(file: &BytecodeFile, value: &str) -> u32 {
    file.strings
        .iter()
        .position(|s| s.value == value)
        .unwrap_or_else(|| panic!("fixture should contain the string {value:?}")) as u32
}

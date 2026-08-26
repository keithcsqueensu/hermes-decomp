//! P5: the `options` byte is decoded, and the CJS table is labelled by it.
//!
//! These are the acceptance criteria from `docs/UNMODELED_REGIONS_PLAN.md` P5,
//! which closes OB1 (the byte was a bare `u8` nothing read) and OB2 with it (the
//! CJS module table has two meanings and the deciding bit lives in that byte).
//!
//! ## The fixtures, and the one that does not exist
//!
//! `asyncy.js` and `cjsdir/` are compiled artifacts like every other fixture
//! here:
//!
//! ```text
//! hermesc -emit-binary -out asyncy.v<N>.hbc asyncy.js
//! hermesc -emit-binary -commonjs -out cjsdir.v96.hbc cjsdir/
//! ```
//!
//! `cjsdir/metadata.json` is what `-commonjs` requires of a directory input; the
//! CJS path is only exercisable at v96, because `hermesc` at v98 and v99 crashes
//! on `-commonjs` with these builds.
//!
//! The statically-resolved arm of OB2 has **no artifact**. `-commonjs
//! -fstatic-require` over a directory, with `moduleIDs` supplied and
//! `-Wunresolved-static-require` silent, still emits the *unresolved* table, so
//! `cjsModuleTableStatic` could not be produced with any compiler on hand. Its
//! decoder is therefore asserted against a synthesised byte, and the test that
//! does so says so in its name rather than implying a bundle backs it.

use std::path::PathBuf;

use hbc_decomp::format::{OPTION_CJS_MODULES_STATICALLY_RESOLVED, OPTION_HAS_ASYNC};
use hbc_decomp::inspect::{dump_table, dump_table_json, TableKind};
use hbc_decomp::{BytecodeFile, BytecodeOptions, CjsModuleForm};

const VERSIONS: [u32; 3] = [96, 98, 99];

fn fixture(name: &str) -> BytecodeFile {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    BytecodeFile::parse_auto(&bytes).expect("parse fixture")
}

// ---------------------------------------------------------------------------
// OB1 -- the bitfield
// ---------------------------------------------------------------------------

/// The bit that R27 watched drift, on real bundles: the *same* source, compiled
/// at three versions, sets bit 2 at v96 and at no version above it.
///
/// The two halves are different claims. At v96 `has_async()` is `Some(true)`
/// because the bundle contains an `async function`. At v98 and v99 it is `None`
/// because the bit does not exist there -- not `Some(false)`, which would assert
/// something about a flag upstream deleted.
#[test]
fn has_async_is_set_at_v96_and_undefined_above_it() {
    for version in VERSIONS {
        let file = fixture(&format!("asyncy.v{version}.hbc"));
        let options = file.header.options();
        assert_eq!(
            options.version(),
            version,
            "fixture v{version} parsed as v{}",
            options.version()
        );

        if version <= 97 {
            assert_eq!(
                options.raw() & OPTION_HAS_ASYNC,
                OPTION_HAS_ASYNC,
                "v{version}: asyncy.js contains an async function, so hermesc should set bit 2; \
                 options is {options}"
            );
            assert_eq!(options.has_async(), Some(true), "v{version}: {options}");
        } else {
            assert_eq!(
                options.raw() & OPTION_HAS_ASYNC,
                0,
                "v{version}: bit 2 is not hasAsync at this version and hermesc should leave it \
                 clear; options is {options}"
            );
            assert_eq!(
                options.has_async(),
                None,
                "v{version}: hasAsync was removed at v98. Some(false) would be a claim about a \
                 bit that no longer exists."
            );
        }

        // Whatever the version, nothing unmodelled is set -- an unknown bit on a
        // hermesc-built file would mean upstream grew one.
        assert_eq!(
            options.unknown_bits(),
            0,
            "v{version}: {options} carries a bit BytecodeOptions does not model"
        );
    }
}

/// The byte is a view, not a rewrite: it must still be on the header, verbatim,
/// because the write path round-trips it untouched.
#[test]
fn the_raw_byte_is_still_carried_verbatim() {
    for version in VERSIONS {
        let file = fixture(&format!("asyncy.v{version}.hbc"));
        assert_eq!(
            file.header.options().raw(),
            file.header.options_raw,
            "v{version}: the decoded view disagrees with the byte it decodes"
        );
    }
}

/// Decoding at the wrong version is the defect this type exists to make
/// impossible to write by accident, so pin that the *same byte* reads
/// differently at v96 and v99 -- which is what "version-keyed" means, and what a
/// bare `u8` could not express.
#[test]
fn the_same_byte_decodes_differently_per_version() {
    let at96 = BytecodeOptions::new(OPTION_HAS_ASYNC, 96);
    let at99 = BytecodeOptions::new(OPTION_HAS_ASYNC, 99);

    assert_eq!(at96.has_async(), Some(true));
    assert_eq!(at96.unknown_bits(), 0);

    assert_eq!(at99.has_async(), None);
    assert_eq!(
        at99.unknown_bits(),
        OPTION_HAS_ASYNC,
        "at v99 bit 2 is undeclared, so it must surface as unknown rather than as hasAsync. A \
         v98 image built before upstream's BitField rewrite is exactly this byte."
    );
}

// ---------------------------------------------------------------------------
// OB2 -- the CJS module table's two meanings
// ---------------------------------------------------------------------------

/// The plan's acceptance criterion, on the real artifact: an unresolved bundle
/// dumps as filename string ids, labelled as such, and they resolve.
#[test]
fn an_unresolved_cjs_table_dumps_as_filenames() {
    let file = fixture("cjsdir.v96.hbc");
    let options = file.header.options();
    assert!(
        !options.cjs_modules_statically_resolved(),
        "cjsdir.v96 should be the unresolved form; options is {options}"
    );
    assert_eq!(options.cjs_module_form(), CjsModuleForm::Filenames);
    assert_eq!(
        file.cjs_module_table.len(),
        2,
        "cjsdir/ has two modules: index.js and helper.js"
    );

    // The pairs resolve to the two source filenames, which is the whole claim:
    // `.first` is a string id in this form.
    let names: Vec<String> = file
        .cjs_module_table
        .iter()
        .map(|(first, _)| {
            file.string_at(*first)
                .map(|e| e.value.clone())
                .unwrap_or_else(|| panic!("string id {first} does not resolve"))
        })
        .collect();
    assert_eq!(names, vec!["index.js".to_string(), "helper.js".to_string()]);

    let text = dump_table(&file, TableKind::CjsModules);
    assert!(
        text.contains("filename string ids, options bit 1 clear"),
        "the dump must say which of the two tables it is showing:\n{text}"
    );
    assert!(
        text.contains("filename_string_id=") && text.contains("\"index.js\""),
        "the dump must label the first half and resolve it:\n{text}"
    );
    assert!(
        !text.contains("symbol_id"),
        "`symbol_id` was the label OB2 is about; it is right in neither form:\n{text}"
    );

    let json = dump_table_json(&file, TableKind::CjsModules);
    let first = &json.as_array().expect("array")[0];
    assert_eq!(first["form"], "filenames");
    assert_eq!(first["filename"], "index.js");
    assert!(
        first.get("module_id").is_none(),
        "an unresolved table has no module ids: {first}"
    );
}

/// The statically-resolved arm, against a **synthesised** byte -- no compiler on
/// hand emits `cjsModuleTableStatic`, so there is no fixture and this says so.
///
/// What it can still assert is the thing OB2 is about: the *same table bytes*
/// must be labelled differently depending on bit 1, and the module-index form
/// must not be resolved against the string table. So it takes the real
/// unresolved artifact, flips the bit in the parsed header, and requires the
/// output to change meaning -- the string ids in this fixture happen to be valid
/// indices, which is precisely why mislabelling them printed a plausible wrong
/// answer instead of an error.
#[test]
fn a_synthesised_statically_resolved_bit_selects_the_module_id_form() {
    assert_eq!(
        BytecodeOptions::new(OPTION_CJS_MODULES_STATICALLY_RESOLVED, 96).cjs_module_form(),
        CjsModuleForm::StaticallyResolved
    );
    assert_eq!(
        BytecodeOptions::new(0, 96).cjs_module_form(),
        CjsModuleForm::Filenames
    );

    let mut file = fixture("cjsdir.v96.hbc");
    let before = dump_table(&file, TableKind::CjsModules);
    assert!(before.contains("\"index.js\""));

    file.header.options_raw |= OPTION_CJS_MODULES_STATICALLY_RESOLVED;
    let after = dump_table(&file, TableKind::CjsModules);

    assert!(
        after.contains("statically resolved module ids, options bit 1 set"),
        "flipping bit 1 must change what the table is said to be:\n{after}"
    );
    assert!(
        after.contains("module_id=4") && after.contains("module_id=1"),
        "the first half is a module index in this form:\n{after}"
    );
    assert!(
        !after.contains("index.js") && !after.contains("helper.js"),
        "a module index must not be resolved against the string table -- it would print an \
         unrelated string, which is OB2 exactly:\n{after}"
    );

    let json = dump_table_json(&file, TableKind::CjsModules);
    let first = &json.as_array().expect("array")[0];
    assert_eq!(first["form"], "statically-resolved");
    assert_eq!(first["module_id"], 4);
    assert!(
        first.get("filename").is_none() && first.get("filename_string_id").is_none(),
        "no filename may be reported for a statically-resolved table: {first}"
    );

    // `.second` is the function id in *both* forms, so it must survive the flip.
    let ids: Vec<u64> = json
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["function_id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, vec![1, 2], "the function id half does not depend on the bit");
}

/// A bundle with no CJS modules must not acquire a form it cannot have -- the
/// label is keyed on the bit, and the bit is clear, so it reads as the
/// unresolved form over an empty table. Cheap, but it is the case every
/// non-CommonJS bundle takes.
#[test]
fn a_bundle_without_cjs_modules_dumps_an_empty_table() {
    for version in VERSIONS {
        let file = fixture(&format!("plain.v{version}.hbc"));
        assert_eq!(file.cjs_module_table.len(), 0, "v{version}");
        let text = dump_table(&file, TableKind::CjsModules);
        assert!(
            text.contains("(0 entries,"),
            "v{version}: {text}"
        );
    }
}

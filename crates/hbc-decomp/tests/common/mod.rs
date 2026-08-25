//! Oracle gating shared by the three env-gated suites — the "fail when they are
//! missing" half of R21.
//!
//! Every external oracle these tests use (a Hermes source checkout, a built `hvm`,
//! `hbcdump`, the production bundle) is opt-in, because a checkout without them has
//! to stay testable. The cost is that an unconfigured run is green while asserting
//! almost nothing — the same failure shape as a suite that asserts the wrong thing,
//! and the reason R21 outlived the harnesses it is about.
//!
//! `HBC_REQUIRE_ORACLES` closes that. It names the oracles that *must* be present;
//! an absent one is then a hard failure with the variable to set, instead of a
//! printed `[skip]`:
//!
//! ```text
//! HBC_REQUIRE_ORACLES=src        # every HERMES_SRC_V<N> the pin wants
//! HBC_REQUIRE_ORACLES=src,vm     # ...and an hvm per fixture version
//! HBC_REQUIRE_ORACLES=all        # src, vm, hbcdump, corpus
//! ```
//!
//! Two deliberate sharp edges:
//!
//!   * An unknown token is itself a hard failure. `HBC_REQUIRE_ORACLES=source` that
//!     quietly enforced nothing would reproduce the exact defect this exists to
//!     remove, in the one place nobody would look.
//!   * A variable that is *set* but does not point at what it claims is always an
//!     error, strict mode or not. Unset means "I do not have this oracle"; set-but-
//!     wrong means "I think I have it", and silently skipping is how a stale path
//!     turns a real run into a green no-op.

#![allow(dead_code)] // each test binary uses a different subset

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The external oracles, by the token `HBC_REQUIRE_ORACLES` uses for them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Oracle {
    /// A Hermes source checkout — `HERMES_SRC_V<N>`.
    Src,
    /// A built VM — `HERMES_VM_V<N>`.
    Vm,
    /// A built disassembler — `HERMES_HBCDUMP_V<N>`.
    HbcDump,
    /// The production bundle — `HBC_CORPUS_BUNDLE`.
    Corpus,
}

impl Oracle {
    pub fn token(self) -> &'static str {
        match self {
            Oracle::Src => "src",
            Oracle::Vm => "vm",
            Oracle::HbcDump => "hbcdump",
            Oracle::Corpus => "corpus",
        }
    }

    /// The variable that provides this oracle, for `version` where it is per-version.
    pub fn env(self, version: Option<u32>) -> String {
        let v = version.map(|v| v.to_string()).unwrap_or_else(|| "<N>".into());
        match self {
            Oracle::Src => format!("HERMES_SRC_V{v}"),
            Oracle::Vm => format!("HERMES_VM_V{v}"),
            Oracle::HbcDump => format!("HERMES_HBCDUMP_V{v}"),
            Oracle::Corpus => "HBC_CORPUS_BUNDLE".to_string(),
        }
    }
}

const TOKENS: [Oracle; 4] = [Oracle::Src, Oracle::Vm, Oracle::HbcDump, Oracle::Corpus];

/// Which oracles the run has declared mandatory.
fn required() -> BTreeSet<&'static str> {
    let mut out = BTreeSet::new();
    let Ok(raw) = std::env::var("HBC_REQUIRE_ORACLES") else {
        return out;
    };
    for token in raw.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        match token.to_ascii_lowercase().as_str() {
            // `=1` is what everyone tries first; take it to mean everything rather
            // than rejecting it, but reject anything that is merely close.
            "all" | "1" | "true" | "yes" => out.extend(TOKENS.iter().map(|o| o.token())),
            other => match TOKENS.iter().find(|o| o.token() == other) {
                Some(o) => {
                    out.insert(o.token());
                }
                None => panic!(
                    "HBC_REQUIRE_ORACLES={raw:?} contains unknown oracle {token:?}. \
                     Valid tokens: all, {}. A typo here would silently enforce nothing, \
                     which is the failure this variable exists to remove.",
                    TOKENS
                        .iter()
                        .map(|o| o.token())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            },
        }
    }
    out
}

/// Is `oracle` mandatory for this run?
pub fn is_required(oracle: Oracle) -> bool {
    required().contains(oracle.token())
}

/// Report an oracle that is not configured: a printed `[skip]` normally, a hard
/// failure when `HBC_REQUIRE_ORACLES` names it.
///
/// `what` is the skip note as it was before strict mode existed — what will not be
/// asserted, in the caller's own words.
pub fn skip_or_fail(oracle: Oracle, version: Option<u32>, what: &str) {
    if is_required(oracle) {
        panic!(
            "{} is not configured, but HBC_REQUIRE_ORACLES names {:?}: {what}.\n\
             Set {} to provide it, or drop {:?} from HBC_REQUIRE_ORACLES if this runner \
             genuinely cannot have it. See scripts/fetch_pinned_hermes.py for the source \
             checkouts and scripts/build_hermes_vm.ps1 for the builds.",
            oracle.env(version),
            oracle.token(),
            oracle.env(version),
            oracle.token(),
        );
    }
    println!("  [skip] {what}");
}

/// Read an oracle path from the environment.
///
/// `None` means unset (or set to nothing), which is a legitimate "I do not have
/// this". Set-but-wrong is never legitimate, so `check` failing is a panic in every
/// mode — see the module note.
pub fn oracle_path(
    oracle: Oracle,
    version: Option<u32>,
    check: impl Fn(&Path) -> bool,
    expected: &str,
) -> Option<PathBuf> {
    let var = oracle.env(version);
    let raw = std::env::var(&var).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    let path = PathBuf::from(raw.trim());
    assert!(
        check(&path),
        "{var} is set to {} but that is not {expected}. Unset it to skip this oracle; \
         a set-but-wrong path would otherwise degrade to a silent no-op.",
        path.display()
    );
    Some(path)
}

// The identity guard (IDENTITY_PLAN.md A3.0): how many `Var`s each pass makes
// without an identity.
//
// A `Var`'s identity today is its `slot`, set only on an environment slot's
// variable; a local has none until A3 gives it one where registers become
// variables (F23). Every `Var` built from a string (`Var::new`, the `From`
// impls, `Var::with_slot(.., None)`) is one more variable that the passes after
// it can tell apart only by spelling. Each step of A3 converts one pass family
// and must bring that family's count here to 0. Clones and renames through
// `name` keep what the `Var` had, so they are not counted.
//
// A count is charged to the stage running on the constructing thread: a
// per-function stage (`F1`..`F26`, set by `generate_ir`) when one is, and
// otherwise to the whole-program stages since the last checkpoint, which
// `take` hands to the census in `analysis::closure::stamps` (`after W13`
// covers what ran after `after W12`). Most `Var`s come from `.into()`, whose
// caller a `#[track_caller]` cannot see through the blanket `Into` impl, so the
// stage is the finest attribution there is.
//
// On only under `RUST_LOG=slots=debug`, like the census; otherwise each
// construction costs one load of a cached flag. A cached function's IR is
// not rebuilt, so measure with `--no-cache`.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

type Counts = HashMap<&'static str, u64>;

// The key for a `Var` made outside any per-function stage.
const PROGRAM: &str = "";

static ON: OnceLock<bool> = OnceLock::new();

// Every thread's counts, so a checkpoint on the main thread sees the work of
// rayon's workers.
static ALL: Mutex<Vec<Arc<Mutex<Counts>>>> = Mutex::new(Vec::new());

thread_local! {
    static STAGE: Cell<Option<&'static str>> = const { Cell::new(None) };
    static LOCAL: Arc<Mutex<Counts>> = {
        let counts = Arc::new(Mutex::new(Counts::new()));
        if let Ok(mut all) = ALL.lock() {
            all.push(counts.clone());
        }
        counts
    };
}

fn on() -> bool {
    *ON.get_or_init(|| log::log_enabled!(target: "slots", log::Level::Debug))
}

// A `Var` was made without an identity.
#[inline]
pub(crate) fn made() {
    if !on() {
        return;
    }
    let stage = STAGE.with(Cell::get).unwrap_or(PROGRAM);
    LOCAL.with(|c| {
        if let Ok(mut c) = c.lock() {
            *c.entry(stage).or_default() += 1;
        }
    });
}

// The per-function stage now running on this thread.
pub fn stage(name: &'static str) {
    if on() {
        STAGE.with(|s| s.set(Some(name)));
    }
}

// Restores the stage label a per-function run found, on drop: `generate_ir`
// can run inside a whole-program stage, or inside another function's run.
pub struct FunctionRun(Option<&'static str>);

pub fn function_run() -> FunctionRun {
    FunctionRun(STAGE.with(Cell::get))
}

impl Drop for FunctionRun {
    fn drop(&mut self) {
        STAGE.with(|s| s.set(self.0));
    }
}

// The counts since the last call: those made outside any per-function stage,
// and those of each per-function stage.
pub fn take() -> (u64, BTreeMap<&'static str, u64>) {
    let mut program = 0;
    let mut stages: BTreeMap<&'static str, u64> = BTreeMap::new();
    if !on() {
        return (program, stages);
    }
    let Ok(all) = ALL.lock() else {
        return (program, stages);
    };
    for counts in all.iter() {
        let Ok(mut counts) = counts.lock() else {
            continue;
        };
        for (stage, n) in counts.drain() {
            if stage == PROGRAM {
                program += n;
            } else {
                *stages.entry(stage).or_default() += n;
            }
        }
    }
    (program, stages)
}

// Stage labels in pipeline order (`F5a` after `F5`, `F25b` after `F25`).
pub fn stage_order(name: &str) -> (u32, String) {
    let digits: String = name
        .trim_start_matches('F')
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (digits.parse().unwrap_or(u32::MAX), name.to_string())
}

#[cfg(test)]
mod tests {
    use super::stage_order;

    #[test]
    fn stages_sort_in_pipeline_order() {
        let mut stages = vec!["F23", "F5a", "G2 rewrite", "F5", "F25b", "F1", "F25"];
        stages.sort_by_key(|s| stage_order(s));
        assert_eq!(
            stages,
            ["F1", "F5", "F5a", "F23", "F25", "F25b", "G2 rewrite"]
        );
    }
}

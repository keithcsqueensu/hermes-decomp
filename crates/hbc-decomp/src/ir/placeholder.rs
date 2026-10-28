// The decompiler's placeholder spellings, in one place.
//
// An environment slot nothing has named prints as a placeholder:
//
//   closure_{slot}          a slot named by its owner, which printed it first
//   closure_{slot}${k}      the same, made distinct from an enclosing scope's
//                           `closure_{slot}` (`ClosureContext::allocate_slot_names`)
//   closure_{level}_{slot}  a slot `level` scopes out that has no entry at all
//   closure_unknown_{slot}  a slot of a scope the graph and the chain cannot
//                           name (`UNKNOWN_ENV_LEVEL`, S3.4)
//   c{slot}                 a slot initialised with a constant (a counter)
//
// The spelling says a name *is* a placeholder. It does not say whose slot it is:
// since names are allocated per owner, a bare `closure_{slot}` in a body may be
// the body's own slot or any ancestor's. That answer is the bake record's
// (`ClosureContext::resolve_baked`); `parse` is the fallback for a name with no
// record, and its `level` is only the spelling's claim.

pub const PREFIX: &str = "closure_";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placeholder {
    pub level: u32,
    pub slot: u32,
    // `k` of `closure_{slot}${k}`.
    pub variant: Option<u32>,
}

fn digits(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

// Parse a `closure_*` placeholder. `c{slot}` is not parsed: it is a counter's
// name, not a slot reference (see `is_counter_slot`).
//
// The renaming stages that take a placeholder and give it a better name
// (W10/W11 usage and definition naming, ancestor inherit, the declaration
// pass's `let` for env slots) still refuse the `$` form: they key by text, and
// letting them through produced collisions of their own -- in React Native's
// `useAnimatedPropsLifecycleWithPrevNodeRef`, a capture renamed `current` beside
// a local `current`. Opening them is a separate change, not a respelling.
pub fn parse(name: &str) -> Option<Placeholder> {
    let rest = name.strip_prefix(PREFIX)?;
    if let Some((slot, k)) = rest.split_once('$') {
        return Some(Placeholder {
            level: 0,
            slot: digits(slot)?,
            variant: Some(digits(k)?),
        });
    }
    match rest.split_once('_') {
        None => Some(Placeholder {
            level: 0,
            slot: digits(rest)?,
            variant: None,
        }),
        Some(("unknown", slot)) => Some(Placeholder {
            level: crate::ir::UNKNOWN_ENV_LEVEL,
            slot: digits(slot)?,
            variant: None,
        }),
        Some((level, slot)) => Some(Placeholder {
            level: digits(level)?,
            slot: digits(slot)?,
            variant: None,
        }),
    }
}

// The placeholder family by its prefix alone. Nothing but a placeholder starts
// with it; use `parse` where the slot matters.
pub fn has_prefix(name: &str) -> bool {
    name.starts_with(PREFIX)
}

// `c{digits}`: a constant-initialised slot.
pub fn is_counter_slot(name: &str) -> bool {
    name.strip_prefix('c').and_then(digits).is_some()
}

// A placeholder that names a slot: `closure_*` in any well-formed spelling, or
// `c{slot}`.
pub fn is_slot_placeholder(name: &str) -> bool {
    parse(name).is_some() || is_counter_slot(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_parses() {
        let p = |level, slot, variant| {
            Some(Placeholder {
                level,
                slot,
                variant,
            })
        };
        assert_eq!(parse("closure_5"), p(0, 5, None));
        assert_eq!(parse("closure_5$2"), p(0, 5, Some(2)));
        assert_eq!(parse("closure_2_14"), p(2, 14, None));
        assert_eq!(parse("closure_0_3"), p(0, 3, None));
        for bad in [
            "closure_",
            "closure_x",
            "closure_1_",
            "closure_1_2_3",
            "closure_1$",
            "c3",
            "login",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
        assert!(is_counter_slot("c12") && !is_counter_slot("c") && !is_counter_slot("cx1"));
        assert!(is_slot_placeholder("closure_3$2") && is_slot_placeholder("c0"));
    }
}

use std::collections::BTreeMap;

// Encode level and slot into a single u32 key for HashMap storage.
// Uses high 8 bits for level, low 24 bits for slot.
pub fn encode_level_slot(level: u32, slot: u32) -> u32 {
    ((level & 0xFF) << 24) | (slot & 0xFFFFFF)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ClosureSlotValue {
    Function {
        id: u32,
        name: Option<String>,
    },
    Constant(String),
    /// Slot exclusively holds a RegExp literal (no non-regex stores observed).
    /// Only this variant is named `re{N}`, string constants starting with `/`
    /// and reused env slots must not look like regexes.
    RegExp,
    Variable(String),
    Unknown,
}

/// Which rung of `ClosureInfo::slot_name`'s ladder a slot's name came from,
/// best evidence first. A consumer asks this instead of reading the spelling:
/// `closure_3` and a source identifier that happens to be spelled so are
/// different facts. Debug-info names (P1b) would go above `Function`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NameSource {
    /// The closure created into the slot has a name.
    Function,
    /// The slot holds a constant whose text is an identifier.
    Constant,
    /// The slot holds a variable with a meaningful name (IPA, Metro, register
    /// naming).
    Variable,
    /// Built from what the slot holds but not from a name: `f{id}`, `re{N}`,
    /// `c{N}`, `args`.
    Synthesised,
    /// Nothing known: `closure_{N}` (`crate::ir::placeholder`).
    Placeholder,
}

/// A slot's name and the rung it came from. A name decided by
/// `ClosureContext::allocate_slot_names` keeps the rung of the name it was
/// derived from: the decision changes the text, not the evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotName {
    pub text: String,
    pub source: NameSource,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClosureInfo {
    pub slots: BTreeMap<u32, ClosureSlotValue>,
    /// A name decided for a slot, which `get_slot_name` renders ahead of
    /// anything derived from the slot's value. Set by
    /// `ClosureContext::allocate_slot_names` where the derived name would
    /// repeat one an enclosing scope already uses: two variables under one name
    /// in a function body change what the code means.
    #[serde(default)]
    pub names: BTreeMap<u32, String>,
}

impl Default for ClosureInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl ClosureInfo {
    pub fn new() -> Self {
        Self {
            slots: BTreeMap::new(),
            names: BTreeMap::new(),
        }
    }

    /// Record a store into an env slot with reuse-aware merge.
    ///
    /// Hermes reuses environment slots aggressively. A slot that once held a
    /// regex and later holds `Math.random` must not keep the `re{N}` name for
    /// every use (flow-insensitive naming would otherwise mislabel).
    ///
    /// Mutable captured bindings are initialised once (often with a Constant)
    /// then updated with temps (`sum = c0 + 1; c0 = sum`). Flow-insensitive
    /// last-write would rename the *slot* to `sum` and turn the update into
    /// `sum = sum + 1` (TDZ). Keep the first stable name for the slot.
    pub fn store_slot(&mut self, slot: u32, val: ClosureSlotValue) {
        let next = match self.slots.get(&slot) {
            None => val,
            Some(ClosureSlotValue::RegExp) => match val {
                ClosureSlotValue::RegExp => ClosureSlotValue::RegExp,
                other => other,
            },
            Some(prev) => match &val {
                // Non-regex then regex ⇒ slot reuse; drop RegExp so we never
                // emit `re{N}` for mixed slots.
                ClosureSlotValue::RegExp => ClosureSlotValue::Unknown,
                // Temp / intermediate Variable must not rename a stable slot.
                ClosureSlotValue::Variable(v) if is_ephemeral_slot_name(v) => {
                    // Prefer an existing Constant/Function/stable Variable name.
                    if slot_name_is_stable(prev) {
                        prev.clone()
                    } else {
                        val
                    }
                }
                other => other.clone(),
            },
        };
        self.slots.insert(slot, next);
    }
}

/// Names that are intermediate SSA-like temps, not the identity of a captured binding.
pub(super) fn is_ephemeral_slot_name(name: &str) -> bool {
    if name == "tmp"
        || name
            .strip_prefix("tmp")
            .is_some_and(|s| s.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    if name
        .strip_prefix('r')
        .is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    // Common names inferred from binary ops / short-lived results.
    matches!(
        name,
        "sum"
            | "diff"
            | "product"
            | "quotient"
            | "text"
            | "result"
            | "value"
            | "ret"
            | "tmpResult"
            | "callResult"
    ) || name.ends_with("Result")
        || name.ends_with("Return")
}

pub(super) fn slot_name_is_stable(val: &ClosureSlotValue) -> bool {
    match val {
        ClosureSlotValue::Constant(_) | ClosureSlotValue::Function { .. } => true,
        ClosureSlotValue::Variable(v) => !is_ephemeral_slot_name(v),
        ClosureSlotValue::RegExp | ClosureSlotValue::Unknown => false,
    }
}

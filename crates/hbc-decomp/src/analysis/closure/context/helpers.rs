// Shared predicates for env-slot naming / stability.
pub(super) fn binding_name_is_slot_worthy(name: &str) -> bool {
    if name.len() < 2 {
        return false;
    }
    if name.starts_with('r') && name[1..].chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if name.starts_with("arg") && name[3..].chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if name.starts_with("tmp") || name.starts_with("closure_") || name.starts_with("outer") {
        return false;
    }
    // Single-letter or numeric-ish temps
    if name.len() <= 2 && name.chars().all(|c| c.is_ascii_alphanumeric()) {
        // allow common short names like `id`, `fs` — reject pure `c0`/`r1` above
        if name.chars().next().is_some_and(|c| c == 'c')
            && name[1..].chars().all(|c| c.is_ascii_digit())
        {
            return false;
        }
    }
    true
}

// Stamp survival: how many variable references still carry the slot
// `resolve_closures` stamped on them (K4.2).
//
// A pass that rebuilds a `Var` from a string drops its `SlotRef`, and nothing
// else would say so: the output is the same until a later stage reads the slot.
// Spelling cannot find the drops after the fact, because locals have no
// declaration in the IR before codegen: a child's own `items` and an ancestor's
// slot `items` look alike. What a drop does leave is a transition, so the
// census keeps each function's per-name counts from the previous checkpoint and
// counts as `dropped` every reference that went from stamped to unstamped under
// the same name between the two. That is an upper bound, not a count: a pass
// that removes a stamped reference (a self-store `require = require`) and in
// the same run copies an unrelated unstamped one of that name (an inlined
// `require(98)`) shows up too, so a function it names is where to look, not
// proof. It also counts `mixed`: unstamped references to a name the same body
// uses, stamped, for an ancestor's slot, which one JS scope cannot mean as two
// things.
//
// Logged under `RUST_LOG=slots=debug` (`trace` adds the functions that drop
// most); free otherwise.

use super::ClosureContext;
use crate::ir::{AssignTarget, Binding, Expression, SlotRef, Statement, Value, Visitor};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

pub const TARGET: &str = "slots";

pub fn enabled() -> bool {
    log::log_enabled!(target: TARGET, log::Level::Debug)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Census {
    // Stamped references to a slot an ancestor owns.
    pub captures: usize,
    // Stamped references to the function's own slots.
    pub own: usize,
    // Unstamped references to a name the same body uses, stamped, for an
    // ancestor's slot.
    pub mixed: usize,
    // References that were stamped at the previous checkpoint and are not now,
    // under the same name in the same function.
    pub dropped: usize,
}

impl Census {
    pub fn add(&mut self, o: Census) {
        self.captures += o.captures;
        self.own += o.own;
        self.mixed += o.mixed;
        self.dropped += o.dropped;
    }
}

// Every variable reference in a body, with its slot when it has one.
pub fn var_refs(stmts: &[Statement]) -> Vec<(&str, Option<SlotRef>)> {
    struct Refs<'a>(Vec<(&'a str, Option<SlotRef>)>);
    impl<'a> Visitor<'a> for Refs<'a> {
        // A declaration that took over a slot's first write keeps its slot.
        fn visit_statement(&mut self, s: &'a Statement) {
            if let Statement::Let { name, .. } = s {
                self.0.push((&name.name, name.slot));
            }
            self.walk_statement(s);
        }
        fn visit_expression(&mut self, e: &'a Expression) {
            if let Expression::Value(Value::Binding(Binding::Variable(v))) = e {
                self.0.push((&v.name, v.slot));
            }
            self.walk_expression(e);
        }
        fn visit_assign_target(&mut self, t: &'a AssignTarget) {
            if let AssignTarget::Binding(Binding::Variable(v)) = t {
                self.0.push((&v.name, v.slot));
            }
            self.walk_assign_target(t);
        }
    }
    let mut refs = Refs(Vec::new());
    for s in stmts {
        refs.visit_statement(s);
    }
    refs.0
}

// The scopes whose slots a function's body owns: itself and the block scopes it
// builds.
pub fn own_scopes(ctx: &ClosureContext, func: u32) -> HashSet<u32> {
    let mut out = HashSet::from([func]);
    for (&(builder, _), &block) in &ctx.block_scopes {
        if builder == func {
            out.insert(block);
        }
    }
    out
}

// The names a body uses for slots the function itself owns, as the stamps say
// (K4.4a): what its declarations must bind, whatever an ancestor's slot is
// called. Block-scope slots are left out (their binding is per block, not per
// function), and so is a name the body also uses for an ancestor's slot: one
// scope cannot bind it as both, and declaring it would cut the capture.
pub fn own_slot_names(func: u32, stmts: &[Statement]) -> HashSet<String> {
    let mut own = HashSet::new();
    let mut captured = HashSet::new();
    for (name, slot) in var_refs(stmts) {
        match slot {
            Some(s) if s.owner == func => {
                own.insert(name);
            }
            Some(_) => {
                captured.insert(name);
            }
            None => {}
        }
    }
    own.into_iter()
        .filter(|n| !captured.contains(n))
        .map(str::to_string)
        .collect()
}

// The names a body uses for slots another scope owns, as the stamps say
// (K4.4b), each with the slots it stands for: a write to one is a store into
// the owner's variable, never a declaration of the body's own. Read per body,
// so a name some other body gives an ancestor's slot does not stop this body
// declaring a local of it. A name the body also uses for a slot of its own is
// left out (I2 TwoSlots), and so is a slot whose owner is not an ancestor:
// statements inlined from another function keep that function's stamps, and
// its slot is a local of this body now.
pub fn captured_slot_names(
    ctx: &ClosureContext,
    func: u32,
    stmts: &[Statement],
) -> HashMap<String, HashSet<SlotRef>> {
    let own_scopes = own_scopes(ctx, func);
    let ancestors: HashSet<u32> = ctx.ancestors_of(func).into_iter().collect();
    let mut captured: HashMap<&str, HashSet<SlotRef>> = HashMap::new();
    let mut own = HashSet::new();
    for (name, slot) in var_refs(stmts) {
        match slot {
            Some(s) if own_scopes.contains(&s.owner) => {
                own.insert(name);
            }
            Some(s) if ancestors.contains(&s.owner) => {
                captured.entry(name).or_default().insert(s);
            }
            _ => {}
        }
    }
    captured
        .into_iter()
        .filter(|(n, _)| !own.contains(n))
        .map(|(n, s)| (n.to_string(), s))
        .collect()
}

// Per name: (stamped, unstamped) references in one body.
type NameCounts = HashMap<String, (usize, usize)>;

// One body's census, and its per-name counts for the next checkpoint.
fn count(
    ctx: &ClosureContext,
    func: u32,
    stmts: &[Statement],
    before: Option<&NameCounts>,
) -> (Census, NameCounts) {
    let own = own_scopes(ctx, func);
    let refs = var_refs(stmts);
    let mut c = Census::default();
    let mut names: NameCounts = HashMap::new();
    let mut captured: HashSet<&str> = HashSet::new();
    for &(name, slot) in &refs {
        let e = names.entry(name.to_string()).or_default();
        match slot {
            Some(s) if own.contains(&s.owner) => {
                c.own += 1;
                e.0 += 1;
            }
            Some(_) => {
                c.captures += 1;
                e.0 += 1;
                captured.insert(name);
            }
            None => e.1 += 1,
        }
    }
    for &(name, slot) in &refs {
        if slot.is_none() && captured.contains(name) {
            c.mixed += 1;
        }
    }
    if let Some(before) = before {
        for (name, &(stamped, unstamped)) in &names {
            if let Some(&(was_stamped, was_unstamped)) = before.get(name) {
                let lost = was_stamped.saturating_sub(stamped);
                let gained = unstamped.saturating_sub(was_unstamped);
                c.dropped += lost.min(gained);
            }
        }
    }
    (c, names)
}

// Each function's per-name counts at the last checkpoint, and the census
// summed over the run so far.
static LAST: Mutex<Option<HashMap<u32, NameCounts>>> = Mutex::new(None);

// The census over every body at a named checkpoint, logged.
pub fn report(stage: &str, all_ir: &BTreeMap<u32, Vec<Statement>>, ctx: Option<&ClosureContext>) {
    if !enabled() {
        return;
    }
    let Some(ctx) = ctx else { return };
    let Ok(mut last) = LAST.lock() else { return };
    let before = last.take().unwrap_or_default();
    let mut now = HashMap::with_capacity(all_ir.len());
    let mut total = Census::default();
    let mut worst = Vec::new();
    for (&func, stmts) in all_ir {
        let (c, names) = count(ctx, func, stmts, before.get(&func));
        if c.dropped > 0 {
            worst.push((func, c));
        }
        total.add(c);
        now.insert(func, names);
    }
    log_census(stage, total);
    worst.sort_by(|a, b| b.1.dropped.cmp(&a.1.dropped).then(a.0.cmp(&b.0)));
    for (func, c) in worst.into_iter().take(5) {
        let was = &before[&func];
        let names: Vec<&String> = now[&func]
            .iter()
            .filter(|(n, &(s, u))| was.get(*n).is_some_and(|&(ws, wu)| ws > s && u > wu))
            .map(|(n, _)| n)
            .collect();
        log::trace!(target: TARGET, "    F{func}: dropped {} {names:?}", c.dropped);
    }
    *last = Some(now);
}

pub fn log_census(stage: &str, c: Census) {
    log::debug!(
        target: TARGET,
        "{stage:<14} captures {:>7}  own {:>6}  mixed {:>5}  dropped {:>6}",
        c.captures,
        c.own,
        c.mixed,
        c.dropped,
    );
}

// The render checkpoint is split: nested bodies are prepared together, module
// factories one at a time as they are emitted. Each compares against the last
// checkpoint and adds here, and the batch logs the sum once the last module is out.
static RENDERED: Mutex<Census> = Mutex::new(Census {
    captures: 0,
    own: 0,
    mixed: 0,
    dropped: 0,
});

pub fn record_render(ctx: Option<&ClosureContext>, func: u32, stmts: &[Statement]) {
    if !enabled() {
        return;
    }
    let Some(ctx) = ctx else { return };
    let before = LAST
        .lock()
        .ok()
        .and_then(|last| last.as_ref().and_then(|m| m.get(&func).cloned()));
    let (c, _) = count(ctx, func, stmts, before.as_ref());
    if c.dropped > 0 {
        log::trace!(target: TARGET, "    render F{func}: dropped {}", c.dropped);
    }
    if let Ok(mut total) = RENDERED.lock() {
        total.add(c);
    }
}

pub fn report_render() {
    if !enabled() {
        return;
    }
    if let Ok(total) = RENDERED.lock() {
        log_census("at codegen", *total);
    }
}

// A v98 generator machine read off the step function's CFG, before structure
// recovery (GENERATOR_LIFT_PLAN.md G1). There a loop around a suspend is still a
// graph: a suspend is an edge from its block to the resume entry of the label it
// stores. `read` and the census change nothing; `rewrite` (G2) makes each such
// edge a `yield`.
//
// `hermesc` lowers the step to: a guard on the status slot (2 running, 3 done);
// `status = 2`; a chain of tests on the label slot, the last label being the
// chain's default; at each label the resume protocol on parameter 0 (1 throw, 2
// return, otherwise run); and each suspend `label = N; status = 1; return
// { value, done: false }`. Constants and copies are propagated by this point, so
// a register read is resolved through its single definition.

use crate::ir::{
    AssignTarget, BinaryOp, Binding, BlockId, Constant, Expression, MutVisitor, PropertyKey,
    Statement, Terminator, Value, Visitor, CFG,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Mutex;

const ACTION: u32 = 0;
const RESUME: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Slot {
    level: u32,
    slot: u32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Src {
    Slot(Slot),
    Param(u32),
    Int(i32),
}

#[derive(Debug, Clone)]
pub struct Suspend {
    // The block holding the `return { value, done: false }`.
    pub returns: BlockId,
    // The block storing the label: `returns`, or a block jumping to it.
    pub stores: BlockId,
    pub label: i32,
    pub value: Expression,
}

#[derive(Debug)]
pub struct Machine {
    pub status: Slot,
    pub label: Slot,
    // The source's `try`s (G3): the try-state slot, the one catch block, and
    // (state, catch label). Empty without a `try`.
    pub try_slot: Option<Slot>,
    pub catch_block: Option<BlockId>,
    pub catches: Vec<(i32, i32)>,
    // Label -> the block its resume protocol runs on to; a catch label's entry.
    pub resumes: BTreeMap<i32, BlockId>,
    pub suspends: Vec<Suspend>,
    // Blocks returning `{ value, done: true }` from the body.
    pub completes: Vec<BlockId>,
    // The blocks reached from the resume entries.
    pub body: BTreeSet<BlockId>,
}

// Registers with one definition, from a slot, a parameter or an integer.
fn aliases(cfg: &CFG) -> HashMap<u32, Src> {
    let mut out = HashMap::new();
    let mut seen = HashSet::new();
    for b in cfg.blocks() {
        for s in &b.statements {
            let Statement::Assign {
                target: AssignTarget::Binding(Binding::Register(r)),
                value,
            } = s
            else {
                continue;
            };
            if !seen.insert(*r) {
                out.remove(r);
                continue;
            }
            if let Some(src) = direct(value) {
                out.insert(*r, src);
            }
        }
    }
    // A register defined again after its removal stays out: a third definition
    // finds it in `seen` as well.
    out
}

fn direct(e: &Expression) -> Option<Src> {
    match e {
        Expression::Value(Value::Binding(Binding::ClosureVar { level, slot, .. })) => {
            Some(Src::Slot(Slot {
                level: *level,
                slot: *slot,
            }))
        }
        Expression::Value(Value::Parameter(n)) => Some(Src::Param(*n)),
        Expression::Value(Value::Constant(Constant::Integer(n))) => Some(Src::Int(*n)),
        _ => None,
    }
}

// The try-state slot, its tested (state, catch label) arms, and the default arm's label.
type CatchChain = (Slot, Vec<(i32, i32)>, i32);

struct Reader<'a> {
    cfg: &'a CFG,
    alias: HashMap<u32, Src>,
    // The slot holding which `try` the machine is in, 0 for none (G3).
    try_slot: Option<Slot>,
}

impl Reader<'_> {
    fn src(&self, e: &Expression) -> Option<Src> {
        if let Expression::Value(Value::Binding(Binding::Register(r))) = e {
            return self.alias.get(r).copied();
        }
        direct(e)
    }

    // `src ===/!== k`: the source, k, and the targets when equal and when not.
    fn test(&self, id: BlockId) -> Option<(Src, i32, BlockId, BlockId)> {
        let Terminator::Branch {
            condition:
                Expression::Binary {
                    op: op @ (BinaryOp::StrictEq | BinaryOp::StrictNeq),
                    left,
                    right,
                },
            true_target,
            false_target,
        } = &self.cfg.get(id)?.terminator
        else {
            return None;
        };
        let (src, k) = match (self.src(left)?, self.src(right)?) {
            (Src::Int(_), Src::Int(_)) => return None,
            (Src::Int(k), s) | (s, Src::Int(k)) => (s, k),
            _ => return None,
        };
        Some(if matches!(op, BinaryOp::StrictEq) {
            (src, k, *true_target, *false_target)
        } else {
            (src, k, *false_target, *true_target)
        })
    }

    // A statement with no effect of its own on the machine's path: a register
    // copy of a constant, a parameter or a slot, or an integer store to the status.
    fn trivial(&self, s: &Statement, status: Slot) -> bool {
        match s {
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Register(_)),
                value,
            } => self.src(value).is_some(),
            Statement::Comment(_) => true,
            _ => {
                matches!(self.slot_of(s), Some((sl, Some(Src::Int(_))))
                    if sl == status || Some(sl) == self.try_slot)
            }
        }
    }

    fn stores_try(&self, id: BlockId) -> bool {
        self.cfg.get(id).is_some_and(|b| {
            b.statements
                .iter()
                .any(|s| matches!(self.slot_of(s), Some((sl, _)) if Some(sl) == self.try_slot))
        })
    }

    // A jump through trivial blocks, stopping at a store of the try state: that
    // store is the program's, where the protocol's sides only mark it finished.
    fn skip_to_protocol(&self, mut id: BlockId, status: Slot) -> BlockId {
        for _ in 0..8 {
            let Some(b) = self.cfg.get(id) else { return id };
            match &b.terminator {
                Terminator::Jump(t)
                    if !self.stores_try(id)
                        && b.statements.iter().all(|s| self.trivial(s, status)) =>
                {
                    id = *t
                }
                _ => return id,
            }
        }
        id
    }

    fn skip(&self, mut id: BlockId, status: Slot) -> BlockId {
        for _ in 0..8 {
            let Some(b) = self.cfg.get(id) else { return id };
            match &b.terminator {
                Terminator::Jump(t) if b.statements.iter().all(|s| self.trivial(s, status)) => {
                    id = *t
                }
                _ => return id,
            }
        }
        id
    }

    // `{ value: v, done: d }` as `e` holds it at the end of block `id`.
    fn result(&self, id: BlockId, e: &Expression) -> Option<(Expression, bool)> {
        let stmts = &self.cfg.get(id)?.statements;
        let object = match e {
            Expression::Object { .. } => e,
            Expression::Value(Value::Binding(Binding::Register(r))) => {
                stmts.iter().rev().find_map(|s| match s {
                    Statement::Assign {
                        target: AssignTarget::Binding(Binding::Register(t)),
                        value,
                    } if t == r => Some(value),
                    _ => None,
                })?
            }
            _ => return None,
        };
        let Expression::Object { properties } = object else {
            return None;
        };
        if properties.len() != 2 {
            return None;
        }
        let prop = |name: &str| {
            properties
                .iter()
                .find(|p| matches!(&p.key, PropertyKey::Ident(k) if k.as_str() == name))
                .map(|p| &p.value)
        };
        let Expression::Value(Value::Constant(Constant::Bool(done))) = prop("done")? else {
            return None;
        };
        let mut value = prop("value")?.clone();
        // The value filled in by slot, once the object exists.
        if let Expression::Value(Value::Binding(Binding::Register(r))) = e {
            for s in stmts {
                if let Statement::Assign {
                    target: AssignTarget::Index { object, key },
                    value: v,
                } = s
                {
                    let same = matches!(
                        object,
                        Expression::Value(Value::Binding(Binding::Register(o))) if *o == *r
                    );
                    let first = matches!(
                        key,
                        Expression::Value(Value::Constant(Constant::Integer(0)))
                    );
                    if same && first {
                        value = v.clone();
                    }
                }
            }
        }
        Some((value, *done))
    }

    fn throw_side(&self, id: BlockId, status: Slot) -> bool {
        let id = self.skip(id, status);
        let Some(b) = self.cfg.get(id) else {
            return false;
        };
        b.statements.iter().all(|s| self.trivial(s, status))
            && matches!(&b.terminator, Terminator::Throw(e) if self.src(e) == Some(Src::Param(RESUME)))
    }

    fn return_side(&self, id: BlockId, status: Slot) -> bool {
        let id = self.skip(id, status);
        let Some(b) = self.cfg.get(id) else {
            return false;
        };
        let Terminator::Return(Some(e)) = &b.terminator else {
            return false;
        };
        matches!(self.result(id, e), Some((v, true)) if self.src(&v) == Some(Src::Param(RESUME)))
    }

    // The block a label's resume protocol runs on to.
    fn protocol(&self, entry: BlockId, status: Slot) -> Result<BlockId, &'static str> {
        let mut n = self.skip_to_protocol(entry, status);
        let (mut threw, mut returned) = (false, false);
        while !(threw && returned) {
            let b = self.cfg.get(n).ok_or("protocol: no block")?;
            if self.stores_try(n) || !b.statements.iter().all(|s| self.trivial(s, status)) {
                return Err("protocol: code in front");
            }
            let Some((Src::Param(ACTION), k, on, off)) = self.test(n) else {
                return Err("protocol: not a test of the action");
            };
            match k {
                1 if !threw && self.throw_side(on, status) => threw = true,
                2 if !returned && self.return_side(on, status) => returned = true,
                _ => return Err("protocol: unexpected branch"),
            }
            n = if threw && returned {
                off
            } else {
                self.skip_to_protocol(off, status)
            };
        }
        Ok(n)
    }

    // The block a catch's arm stores a label in and rejoins the dispatch from.
    fn catch_arm(&self, id: BlockId, label: Slot, dispatch: BlockId) -> Option<i32> {
        let b = self.cfg.get(id)?;
        matches!(b.terminator, Terminator::Jump(t) if t == dispatch)
            .then(|| self.label_store(id, label))
            .flatten()
    }

    // The source's `try`s, all through one catch: it keeps the exception in a
    // slot, then tests the try state: 0 rethrows, k stores k's catch label and
    // rejoins the dispatch. The last arm is the chain's default, its state known
    // only from the stores in the body.
    fn read_catch(
        &self,
        catch: BlockId,
        status: Slot,
        label: Slot,
        dispatch: BlockId,
    ) -> Result<CatchChain, &'static str> {
        const NOT: &str = "a try in the source: not one catch chain";
        let Some((Src::Slot(slot), 0, on, mut n)) = self.test(catch) else {
            return Err(NOT);
        };
        if slot == status || slot == label || !self.own_catch(on, status) {
            return Err(NOT);
        }
        let mut arms = Vec::new();
        while let Some((Src::Slot(s), k, on, off)) = self.test(n) {
            if s != slot {
                return Err(NOT);
            }
            arms.push((k, self.catch_arm(on, label, dispatch).ok_or(NOT)?));
            n = off;
        }
        let default = self.catch_arm(n, label, dispatch).ok_or(NOT)?;
        Ok((slot, arms, default))
    }

    fn slot_of(&self, s: &Statement) -> Option<(Slot, Option<Src>)> {
        match s {
            Statement::Assign {
                target: AssignTarget::Binding(Binding::ClosureVar { level, slot, .. }),
                value,
            } => Some((
                Slot {
                    level: *level,
                    slot: *slot,
                },
                self.src(value),
            )),
            _ => None,
        }
    }

    fn label_store(&self, id: BlockId, label: Slot) -> Option<i32> {
        self.cfg
            .get(id)?
            .statements
            .iter()
            .rev()
            .find_map(|s| match self.slot_of(s) {
                Some((sl, Some(Src::Int(k)))) if sl == label => Some(k),
                _ => None,
            })
    }

    // A catch that only marks the machine done and rethrows: the machine's own.
    fn own_catch(&self, id: BlockId, status: Slot) -> bool {
        let id = self.skip(id, status);
        let Some(b) = self.cfg.get(id) else {
            return false;
        };
        matches!(b.terminator, Terminator::Throw(_))
            && b.statements.iter().all(|s| {
                self.trivial(s, status)
                    || matches!(
                        s,
                        Statement::Assign {
                            target: AssignTarget::Binding(Binding::Register(_)),
                            ..
                        }
                    )
            })
    }
}

// Reads of the action or of the machine's own slots, and stores into the slots.
struct Scan<'r> {
    reader: &'r Reader<'r>,
    status: Slot,
    label: Slot,
    try_slot: Option<Slot>,
    action: bool,
    slots_read: bool,
    label_stores: usize,
    bad_status: bool,
}

impl<'a> Visitor<'a> for Scan<'_> {
    fn visit_statement(&mut self, s: &'a Statement) {
        if let Some((sl, v)) = self.reader.slot_of(s) {
            if sl == self.label {
                self.label_stores += 1;
                return;
            }
            if sl == self.status {
                self.bad_status |= !matches!(v, Some(Src::Int(1 | 3)));
                return;
            }
            if Some(sl) == self.try_slot {
                self.bad_status |= !matches!(v, Some(Src::Int(_)));
                return;
            }
        }
        // A register's copy of the machine's values is no read until it is used.
        if let Statement::Assign {
            target: AssignTarget::Binding(Binding::Register(_)),
            value,
        } = s
        {
            if self.reader.src(value).is_some() {
                return;
            }
        }
        self.walk_statement(s);
    }
    fn visit_expression(&mut self, e: &'a Expression) {
        match self.reader.src(e) {
            Some(Src::Param(ACTION)) => self.action = true,
            Some(Src::Slot(s))
                if s == self.status || s == self.label || Some(s) == self.try_slot =>
            {
                self.slots_read = true
            }
            _ => {}
        }
        self.walk_expression(e);
    }
}

pub fn read(cfg: &CFG) -> Result<Machine, &'static str> {
    let mut r = Reader {
        cfg,
        alias: aliases(cfg),
        try_slot: None,
    };
    // The status guard.
    let mut n = cfg.entry;
    let mut status = None;
    while let Some((Src::Slot(s), 2 | 3, _, off)) = r.test(n) {
        if status.is_some_and(|st| st != s) {
            break;
        }
        status = Some(s);
        n = off;
    }
    let status = status.ok_or("not a machine: no status guard")?;
    // `status = 2`, then the chain of label tests.
    n = r.skip(n, status);
    let running = cfg.get(n).is_some_and(|b| {
        b.statements
            .iter()
            .any(|s| matches!(r.slot_of(s), Some((sl, Some(Src::Int(2)))) if sl == status))
    });
    if !running {
        return Err("no running store after the guard");
    }
    let dispatch = n;
    let mut entries: BTreeMap<i32, BlockId> = BTreeMap::new();
    let mut label = None;
    while let Some((Src::Slot(s), k, on, off)) = r.test(n) {
        if s == status || label.is_some_and(|l| l != s) {
            break;
        }
        label = Some(s);
        entries.insert(k, on);
        n = r.skip(off, status);
    }
    let label = label.ok_or("no label test")?;
    // A `try` in the source: its catch stores a label and rejoins the dispatch,
    // and that label has no resume protocol. The machine's own catch only marks
    // it done and rethrows.
    let mut catch_blocks: BTreeSet<BlockId> = BTreeSet::new();
    for h in &cfg.exception_handlers {
        if !r.own_catch(h.catch_block, status) {
            catch_blocks.insert(h.catch_block);
        }
    }
    if catch_blocks.len() > 1 {
        return Err("a try in the source: several catch blocks");
    }
    let catch_block = catch_blocks.into_iter().next();
    let mut arms = Vec::new();
    let mut default_arm = None;
    if let Some(c) = catch_block {
        let (slot, a, d) = r.read_catch(c, status, label, dispatch)?;
        r.try_slot = Some(slot);
        arms = a;
        default_arm = Some(d);
    }
    let catch_labels: BTreeSet<i32> = arms.iter().map(|&(_, k)| k).chain(default_arm).collect();
    let mut resumes = BTreeMap::new();
    for (&k, &e) in &entries {
        if catch_labels.contains(&k) {
            resumes.insert(k, e);
        } else {
            resumes.insert(k, r.protocol(e, status)?);
        }
    }
    // A catch label the chain does not test is its default.
    let mut default_used = false;
    for &k in &catch_labels {
        if !entries.contains_key(&k) {
            if default_used {
                return Err("several labels the chain does not test");
            }
            default_used = true;
            resumes.insert(k, n);
        }
    }
    // The body, from the resume entries.
    let default_resume = if default_used {
        Err("the default label is a catch's")
    } else {
        r.protocol(n, status)
    };
    let mut body = BTreeSet::new();
    let mut stack: Vec<BlockId> = resumes.values().copied().collect();
    stack.extend(default_resume.iter().copied());
    while let Some(id) = stack.pop() {
        if body.insert(id) {
            if let Some(b) = cfg.get(id) {
                stack.extend(b.successors());
            }
        }
    }
    let mut suspends = Vec::new();
    let mut completes = Vec::new();
    for &id in &body {
        let Some(b) = cfg.get(id) else { continue };
        let Terminator::Return(ret) = &b.terminator else {
            continue;
        };
        let (value, done) = ret
            .as_ref()
            .and_then(|e| r.result(id, e))
            .ok_or("a return that is no result object")?;
        if done {
            completes.push(id);
            continue;
        }
        if let Some(k) = r.label_store(id, label) {
            suspends.push(Suspend {
                returns: id,
                stores: id,
                label: k,
                value,
            });
            continue;
        }
        let preds = cfg.predecessors(id);
        if preds.is_empty() {
            return Err("a suspend with no way in");
        }
        for p in preds {
            let jumps = cfg
                .get(p)
                .is_some_and(|b| matches!(b.terminator, Terminator::Jump(t) if t == id));
            let k = r
                .label_store(p, label)
                .filter(|_| jumps)
                .ok_or("no label store beside a suspend")?;
            suspends.push(Suspend {
                returns: id,
                stores: p,
                label: k,
                value: value.clone(),
            });
        }
    }
    // A stored label the chain does not test is its default.
    let missing: BTreeSet<i32> = suspends
        .iter()
        .map(|s| s.label)
        .filter(|k| !resumes.contains_key(k))
        .collect();
    match (missing.iter().next(), missing.len(), default_resume) {
        (None, _, _) => {}
        (Some(&k), 1, Ok(d)) => {
            resumes.insert(k, d);
        }
        (Some(_), 1, Err(why)) => return Err(why),
        _ => return Err("several labels the chain does not test"),
    }
    if !resumes.contains_key(&0) {
        return Err("no entry label");
    }
    // The states the body stores: the one no arm tests is the default arm's.
    let mut catches = arms;
    if let (Some(slot), Some(d)) = (r.try_slot, default_arm) {
        let mut states = BTreeSet::new();
        for &id in &body {
            let Some(b) = cfg.get(id) else { continue };
            for s in &b.statements {
                if let Some((sl, Some(Src::Int(k)))) = r.slot_of(s) {
                    if sl == slot && k != 0 {
                        states.insert(k);
                    }
                }
            }
        }
        let mut untested = states
            .into_iter()
            .filter(|k| !catches.iter().any(|&(s, _)| s == *k));
        if let Some(k) = untested.next() {
            if untested.next().is_some() {
                return Err("a try in the source: several states no arm tests");
            }
            catches.push((k, d));
        }
    }
    let mut scan = Scan {
        reader: &r,
        status,
        label,
        try_slot: r.try_slot,
        action: false,
        slots_read: false,
        label_stores: 0,
        bad_status: false,
    };
    for &id in &body {
        let Some(b) = cfg.get(id) else { continue };
        for s in &b.statements {
            scan.visit_statement(s);
        }
        match &b.terminator {
            Terminator::Branch { condition, .. } => scan.visit_expression(condition),
            Terminator::Throw(e) | Terminator::Return(Some(e)) => scan.visit_expression(e),
            Terminator::Switch { value, .. } => scan.visit_expression(value),
            _ => {}
        }
    }
    if scan.action {
        return Err("the body reads the action");
    }
    if scan.slots_read {
        return Err("the body reads the status or label");
    }
    if scan.bad_status {
        return Err("an unexpected status store");
    }
    let stores: BTreeSet<BlockId> = suspends.iter().map(|s| s.stores).collect();
    if scan.label_stores != stores.len() {
        return Err("a label store away from a suspend");
    }
    let try_slot = r.try_slot;
    Ok(Machine {
        status,
        label,
        try_slot,
        catch_block,
        catches,
        resumes,
        suspends,
        completes,
        body,
    })
}

// --- the rewrite (G2) ---

// Reads of the resume value, and registers read against those defined.
#[derive(Default)]
struct Regs {
    read: BTreeSet<u32>,
    defined: BTreeSet<u32>,
    resume_read: bool,
    max: u32,
}

impl<'a> Visitor<'a> for Regs {
    fn visit_expression(&mut self, e: &'a Expression) {
        match e {
            Expression::Value(Value::Binding(Binding::Register(r))) => {
                self.read.insert(*r);
                self.max = self.max.max(*r);
            }
            Expression::Value(Value::Parameter(RESUME)) => self.resume_read = true,
            _ => {}
        }
        self.walk_expression(e);
    }
    fn visit_assign_target(&mut self, t: &'a AssignTarget) {
        if let AssignTarget::Binding(Binding::Register(r)) = t {
            self.defined.insert(*r);
            self.max = self.max.max(*r);
        }
        self.walk_assign_target(t);
    }
}

// A register from the dropped dispatch becomes what it held, and the resume
// value becomes the register each `yield` assigns.
struct Subst<'s> {
    alias: &'s HashMap<u32, Src>,
    defined: &'s BTreeSet<u32>,
    resume: u32,
}

impl MutVisitor for Subst<'_> {
    fn visit_expression(&mut self, e: &mut Expression) {
        let held = match e {
            Expression::Value(Value::Binding(Binding::Register(r)))
                if !self.defined.contains(r) =>
            {
                self.alias.get(r).copied()
            }
            Expression::Value(Value::Parameter(RESUME)) => Some(Src::Param(RESUME)),
            _ => None,
        };
        match held {
            Some(Src::Int(k)) => *e = Expression::constant(Constant::Integer(k)),
            Some(Src::Param(RESUME)) => {
                *e = Expression::Value(Value::Binding(Binding::Register(self.resume)))
            }
            _ => self.walk_expression(e),
        }
    }
}

fn terminator_expressions(t: &mut Terminator) -> Vec<&mut Expression> {
    match t {
        Terminator::Branch { condition, .. } => vec![condition],
        Terminator::Throw(e) | Terminator::Return(Some(e)) => vec![e],
        Terminator::Switch { value, cases, .. } => {
            let mut out = vec![value];
            out.extend(cases.iter_mut().map(|(c, _)| c));
            out
        }
        _ => vec![],
    }
}

// Turn a machine read by `read` into an ordinary function's CFG: each suspend a
// `yield` and a jump to the resume entry of the label it stores, each completion
// a plain return, the dispatch and the protocols gone (`.throw` and `.return` are
// what `yield` itself does). The structurer then runs on it as on any function.
pub fn rewrite(cfg: &mut CFG, m: &Machine) -> Result<(), &'static str> {
    if m.suspends.is_empty() {
        return Err("no suspend");
    }
    if m.body.contains(&cfg.entry) {
        return Err("the entry is in the body");
    }
    let alias = aliases(cfg);
    // The fresh register is past every register of the function.
    let mut all = Regs::default();
    for b in cfg.blocks() {
        for s in &b.statements {
            all.visit_statement(s);
        }
    }
    let mut regs = Regs {
        max: all.max,
        ..Regs::default()
    };
    for &id in &m.body {
        let Some(b) = cfg.get(id) else { continue };
        for s in &b.statements {
            regs.visit_statement(s);
        }
        match &b.terminator {
            Terminator::Branch { condition: e, .. }
            | Terminator::Throw(e)
            | Terminator::Return(Some(e))
            | Terminator::Switch { value: e, .. } => regs.visit_expression(e),
            _ => {}
        }
    }
    // The catch block's statements move into each catch's head.
    if let Some(b) = m.catch_block.and_then(|c| cfg.get(c)) {
        let mut head = Regs::default();
        for s in &b.statements {
            head.visit_statement(s);
        }
        regs.defined.extend(head.defined);
    }
    for &r in regs.read.difference(&regs.defined) {
        match alias.get(&r) {
            Some(Src::Int(_)) | Some(Src::Param(RESUME)) => {}
            held => {
                log::trace!(target: "genlift", "register {r} from the dispatch, held {held:?}");
                return Err("a register from the dispatch reaches the body");
            }
        }
    }
    let defined = regs.defined.clone();
    // Each resume entry has a register for its resume value, read in the blocks
    // it reaches before the next suspend. One register for the whole machine
    // would be declared in the first branch that assigned it, out of scope of
    // the rest. A block read from two entries has no single answer: it stays raw.
    let reads_resume = |id: BlockId| {
        let mut one = Regs::default();
        if let Some(b) = cfg.get(id) {
            for s in &b.statements {
                one.visit_statement(s);
            }
            match &b.terminator {
                Terminator::Branch { condition: e, .. }
                | Terminator::Throw(e)
                | Terminator::Return(Some(e))
                | Terminator::Switch { value: e, .. } => one.visit_expression(e),
                _ => {}
            }
        }
        one.resume_read
            || one
                .read
                .iter()
                .any(|r| !defined.contains(r) && alias.get(r) == Some(&Src::Param(RESUME)))
    };
    let stores: BTreeSet<BlockId> = m.suspends.iter().map(|s| s.stores).collect();
    let returns_of: BTreeMap<BlockId, BlockId> =
        m.suspends.iter().map(|s| (s.stores, s.returns)).collect();
    let entries: BTreeSet<BlockId> = m.resumes.values().copied().collect();
    let mut owner: BTreeMap<BlockId, BlockId> = BTreeMap::new();
    let mut resume_of: BTreeMap<BlockId, u32> = BTreeMap::new();
    let mut next_reg = regs.max + 1;
    let catch_entries: BTreeSet<BlockId> = m
        .catches
        .iter()
        .filter_map(|(_, k)| m.resumes.get(k).copied())
        .collect();
    for &entry in &entries {
        let mut seen = BTreeSet::new();
        let mut stack = vec![entry];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if reads_resume(id) {
                if owner.insert(id, entry).is_some_and(|o| o != entry) {
                    return Err("a block reads the resume value of two entries");
                }
                resume_of.entry(entry).or_insert_with(|| {
                    next_reg += 1;
                    next_reg - 1
                });
            }
            if stores.contains(&id) {
                // The suspend's own block, if apart, is folded into this one.
                if let Some(&r) = returns_of.get(&id).filter(|&&r| r != id) {
                    if reads_resume(r) {
                        if owner.insert(r, entry).is_some_and(|o| o != entry) {
                            return Err("a block reads the resume value of two entries");
                        }
                        resume_of.entry(entry).or_insert_with(|| {
                            next_reg += 1;
                            next_reg - 1
                        });
                    }
                }
                continue;
            }
            if let Some(b) = cfg.get(id) {
                stack.extend(b.successors());
            }
        }
    }
    let new_blocks = {
        let reader = Reader {
            cfg,
            alias: alias.clone(),
            try_slot: m.try_slot,
        };
        let is_machine_store = |s: &Statement| matches!(reader.slot_of(s), Some((sl, _)) if sl == m.status || sl == m.label);
        // What a block holds once the machine's stores and, where it returns a
        // result, the result object are gone.
        let kept = |id: BlockId| -> Vec<Statement> {
            let Some(b) = cfg.get(id) else { return vec![] };
            let object = match &b.terminator {
                Terminator::Return(Some(Expression::Value(Value::Binding(Binding::Register(
                    r,
                )))))
                    if m.completes.contains(&id) || m.suspends.iter().any(|s| s.returns == id) =>
                {
                    Some(*r)
                }
                _ => None,
            };
            b.statements
                .iter()
                .filter(|s| !is_machine_store(s))
                .filter(|s| match (s, object) {
                    (
                        Statement::Assign {
                            target: AssignTarget::Binding(Binding::Register(t)),
                            ..
                        },
                        Some(o),
                    ) => *t != o,
                    (
                        Statement::Assign {
                            target: AssignTarget::Index { object: t, .. },
                            ..
                        },
                        Some(o),
                    ) => {
                        !matches!(t, Expression::Value(Value::Binding(Binding::Register(r))) if *r == o)
                    }
                    _ => true,
                })
                .cloned()
                .collect()
        };
        let mut new_blocks: Vec<(BlockId, Vec<Statement>, Terminator)> = Vec::new();
        let suspending: BTreeSet<BlockId> = m.suspends.iter().map(|s| s.stores).collect();
        for s in &m.suspends {
            let mut stmts = kept(s.stores);
            if s.returns != s.stores {
                stmts.extend(kept(s.returns));
            }
            let suspend = Expression::Yield {
                value: Box::new(s.value.clone()),
                delegate: false,
            };
            let to = *m.resumes.get(&s.label).ok_or("a suspend to no label")?;
            stmts.push(match resume_of.get(&to) {
                Some(&reg) => Statement::Assign {
                    target: AssignTarget::Binding(Binding::Register(reg)),
                    value: suspend,
                },
                None => Statement::Expr(suspend),
            });
            new_blocks.push((s.stores, stmts, Terminator::Jump(to)));
        }
        for &id in &m.body {
            if suspending.contains(&id) {
                continue;
            }
            let Some(b) = cfg.get(id) else { continue };
            let terminator = if m.completes.contains(&id) {
                let Terminator::Return(Some(e)) = &b.terminator else {
                    return Err("a completion that does not return");
                };
                let (v, _) = reader.result(id, e).ok_or("a completion with no result")?;
                let undefined =
                    matches!(v, Expression::Value(Value::Constant(Constant::Undefined)));
                Terminator::Return((!undefined).then_some(v))
            } else {
                b.terminator.clone()
            };
            new_blocks.push((id, kept(id), terminator));
        }
        new_blocks
    };
    if catch_entries.iter().any(|e| resume_of.contains_key(e)) {
        return Err("a catch reads the resume value");
    }
    let entry_to = *m.resumes.get(&0).ok_or("no entry label")?;
    let catch_head: Vec<Statement> = m
        .catch_block
        .and_then(|c| cfg.get(c))
        .map(|b| {
            let reader = Reader {
                cfg,
                alias: alias.clone(),
                try_slot: m.try_slot,
            };
            b.statements
                .iter()
                .filter(|s| {
                    !matches!(
                        s,
                        Statement::Assign {
                            target: AssignTarget::Binding(Binding::Register(_)),
                            value,
                        } if matches!(reader.src(value), Some(Src::Slot(sl)) if Some(sl) == m.try_slot)
                    )
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    // Past here a refusal leaves `cfg` half rewritten: the caller drops it.
    for (id, mut stmts, mut terminator) in new_blocks {
        // The block's entry; a block reading no resume value never needs it. A
        // suspend's folded block reads through the block storing the label.
        let folded = returns_of.get(&id).and_then(|r| owner.get(r));
        let resume = owner
            .get(&id)
            .or(folded)
            .and_then(|e| resume_of.get(e))
            .copied()
            .unwrap_or(next_reg);
        let mut subst = Subst {
            alias: &alias,
            defined: &defined,
            resume,
        };
        for s in &mut stmts {
            subst.visit_statement(s);
        }
        for e in terminator_expressions(&mut terminator) {
            subst.visit_expression(e);
        }
        if let Some(b) = cfg.get_mut(id) {
            b.statements = stmts;
            b.terminator = terminator;
        }
    }
    let entry = cfg.entry;
    let first_resume = resume_of.get(&entry_to).copied();
    if let Some(b) = cfg.get_mut(entry) {
        // Before the first suspend the resume value is what `next()` was given,
        // which a generator cannot see.
        b.statements = first_resume
            .map(|reg| Statement::Assign {
                target: AssignTarget::Binding(Binding::Register(reg)),
                value: Expression::constant(Constant::Undefined),
            })
            .into_iter()
            .collect();
        b.terminator = Terminator::Jump(entry_to);
    }
    cfg.exception_handlers.clear();
    if let Some(slot) = m.try_slot {
        recover_tries(cfg, m, slot, &alias, entry_to, catch_head)?;
    }
    cfg.retain_reachable();
    Ok(())
}

// Hand the structurer the source's `try`s as ordinary handlers (G3). The try state
// says which `try` each statement is in, so a block is split at each store of it,
// the store goes, and each state's blocks are that `try`'s protected range, with
// the ranges of the `try`s entered from inside it.
fn recover_tries(
    cfg: &mut CFG,
    m: &Machine,
    slot: Slot,
    alias: &HashMap<u32, Src>,
    entry_to: BlockId,
    catch_head: Vec<Statement>,
) -> Result<(), &'static str> {
    const NESTING: &str = "a try in the source: regions do not nest";
    let state_of = |s: &Statement| -> Option<i32> {
        let Statement::Assign {
            target: AssignTarget::Binding(Binding::ClosureVar { level, slot: n, .. }),
            value,
        } = s
        else {
            return None;
        };
        if *level != slot.level || *n != slot.slot {
            return None;
        }
        let held = match value {
            Expression::Value(Value::Binding(Binding::Register(r))) => alias.get(r).copied(),
            e => direct(e),
        };
        match held {
            Some(Src::Int(k)) => Some(k),
            _ => None,
        }
    };
    // Split at each store; `forced` is the state a split block starts in.
    let mut forced: BTreeMap<BlockId, i32> = BTreeMap::new();
    let ids: Vec<BlockId> = cfg.block_ids().collect();
    for id in ids {
        let Some(b) = cfg.get_mut(id) else { continue };
        if !b.statements.iter().any(|s| state_of(s).is_some()) {
            continue;
        }
        let stmts = std::mem::take(&mut b.statements);
        let terminator = std::mem::replace(&mut b.terminator, Terminator::None);
        let mut cur = id;
        let mut seg: Vec<Statement> = Vec::new();
        for s in stmts {
            if let Some(k) = state_of(&s) {
                let next = cfg.create_block();
                if let Some(b) = cfg.get_mut(cur) {
                    b.statements = std::mem::take(&mut seg);
                    b.terminator = Terminator::Jump(next);
                }
                forced.insert(next, k);
                cur = next;
            } else {
                seg.push(s);
            }
        }
        if let Some(b) = cfg.get_mut(cur) {
            b.statements = seg;
            b.terminator = terminator;
        }
    }
    // Each catch: a head holding the exception, then its label's entry, in the
    // state the entry restores first.
    let mut head_of: BTreeMap<i32, BlockId> = BTreeMap::new();
    for &(state, label) in &m.catches {
        let entry = *m.resumes.get(&label).ok_or(NESTING)?;
        let mut restored = None;
        let mut n = entry;
        for _ in 0..4 {
            if let Some(&k) = forced.get(&n) {
                restored = Some(k);
                break;
            }
            match cfg.get(n).map(|b| &b.terminator) {
                Some(Terminator::Jump(t)) => n = *t,
                _ => break,
            }
        }
        let restored = restored.ok_or("a try in the source: a catch keeps its state")?;
        let head = cfg.create_block();
        if let Some(b) = cfg.get_mut(head) {
            b.statements = catch_head.clone();
            b.terminator = Terminator::Jump(entry);
        }
        forced.insert(head, restored);
        head_of.insert(state, head);
    }
    // The state of every block, from the entry and the catch heads.
    let mut state: BTreeMap<BlockId, i32> = BTreeMap::new();
    let mut stack: Vec<(BlockId, i32)> = vec![(cfg.entry, 0), (entry_to, 0)];
    stack.extend(head_of.values().map(|h| (*h, forced[h])));
    while let Some((id, inherited)) = stack.pop() {
        let st = forced.get(&id).copied().unwrap_or(inherited);
        if let Some(&was) = state.get(&id) {
            if was != st {
                return Err("a try in the source: a block in two states");
            }
            continue;
        }
        state.insert(id, st);
        if let Some(b) = cfg.get(id) {
            stack.extend(b.successors().into_iter().map(|t| (t, st)));
        }
    }
    // A split leaves empty blocks between a store and what follows. Jump past
    // those in one state, so a `try` and its catch rejoin at the same block, which
    // is what fences the code after the `try` out of it.
    let empty: BTreeMap<BlockId, BlockId> = state
        .keys()
        .filter_map(|&id| {
            let b = cfg.get(id)?;
            match &b.terminator {
                Terminator::Jump(t) if b.statements.is_empty() && *t != id => Some((id, *t)),
                _ => None,
            }
        })
        .filter(|(id, t)| state.get(id) == state.get(t))
        .collect();
    let past = |mut id: BlockId| {
        for _ in 0..16 {
            match empty.get(&id) {
                Some(&t) => id = t,
                None => break,
            }
        }
        id
    };
    let live: Vec<BlockId> = state.keys().copied().collect();
    for id in live {
        if let Some(b) = cfg.get_mut(id) {
            match &mut b.terminator {
                Terminator::Jump(t) => *t = past(*t),
                Terminator::Branch {
                    true_target,
                    false_target,
                    ..
                } => {
                    *true_target = past(*true_target);
                    *false_target = past(*false_target);
                }
                _ => {}
            }
        }
    }
    // Nesting: a state's parent is the state it is first entered from.
    let mut parent: BTreeMap<i32, i32> = BTreeMap::new();
    let mut order = vec![0];
    let mut i = 0;
    while i < order.len() {
        let from = order[i];
        i += 1;
        for (&id, &st) in &state {
            if st != from {
                continue;
            }
            for t in cfg.get(id).map(|b| b.successors()).unwrap_or_default() {
                let to = state.get(&t).copied().unwrap_or(st);
                if to != 0 && !parent.contains_key(&to) && !order.contains(&to) {
                    parent.insert(to, from);
                    order.push(to);
                }
            }
        }
    }
    let within = |mut s: i32, k: i32| {
        while s != k {
            match parent.get(&s) {
                Some(&p) => s = p,
                None => return false,
            }
        }
        true
    };
    // Outermost first.
    for &k in order.iter().skip(1) {
        let head = *head_of
            .get(&k)
            .ok_or("a try in the source: a state with no arm")?;
        let mut try_blocks: Vec<BlockId> = state
            .iter()
            .filter(|(_, &s)| within(s, k))
            .map(|(&b, _)| b)
            .collect();
        // A `return` of a value already computed cannot throw: it may print
        // inside, and then it is no way out.
        let quiet: Vec<BlockId> = try_blocks
            .iter()
            .flat_map(|b| cfg.get(*b).map(|x| x.successors()).unwrap_or_default())
            .filter(|t| !try_blocks.contains(t) && *t != head)
            .filter(|t| {
                cfg.get(*t).is_some_and(|x| {
                    x.statements.is_empty()
                        && matches!(
                            &x.terminator,
                            Terminator::Return(None)
                                | Terminator::Return(Some(Expression::Value(_)))
                        )
                })
            })
            .collect();
        try_blocks.extend(quiet);
        try_blocks.sort();
        try_blocks.dedup();
        let inside: BTreeSet<BlockId> = try_blocks.iter().copied().collect();
        // Dropped blocks (a protocol, the dispatch) still jump to resume entries,
        // and have no state.
        let mut starts = try_blocks.iter().copied().filter(|&b| {
            cfg.predecessors(b)
                .iter()
                .any(|p| state.contains_key(p) && !inside.contains(p))
        });
        let start = starts.next().ok_or(NESTING)?;
        if starts.next().is_some() {
            return Err("a try in the source: a region with two ways in");
        }
        // One way out at most, or the code after the `try` cannot be told from
        // it and would print inside, where its throws would be caught.
        let mut exits = BTreeSet::new();
        for &b in &try_blocks {
            for t in cfg.get(b).map(|x| x.successors()).unwrap_or_default() {
                if !inside.contains(&t) && t != head {
                    exits.insert(t);
                }
            }
        }
        if exits.len() > 1 {
            return Err("a try in the source: several ways out");
        }
        cfg.exception_handlers.push(crate::ir::CfgExceptionHandler {
            try_block_start: start,
            catch_block: head,
            try_blocks,
            exact_range: true,
        });
    }
    Ok(())
}

// --- the census: this reading against the lifter's (G1's exit) ---

static VERDICTS: Mutex<BTreeMap<u32, String>> = Mutex::new(BTreeMap::new());

fn count_yields(stmts: &[Statement]) -> usize {
    struct Count(usize);
    impl<'b> Visitor<'b> for Count {
        fn visit_expression(&mut self, e: &'b Expression) {
            if matches!(e, Expression::Yield { .. }) {
                self.0 += 1;
            }
            self.walk_expression(e);
        }
    }
    let mut c = Count(0);
    for s in stmts {
        c.visit_statement(s);
    }
    c.0
}

// One machine's verdict, by function; `generate_ir` runs more than once per
// function, and the last reading stands.
pub fn record(function: u32, reading: &Result<Machine, &'static str>, stmts: &[Statement]) {
    if !super::is_generator_state_machine_v98(stmts) && reading.is_err() {
        return;
    }
    let lift = super::lift_generator_v98(stmts);
    let verdict = match (reading, &lift) {
        (Ok(m), Ok(l)) if count_yields(l) == m.suspends.len() => "agree".to_string(),
        (Ok(m), Ok(l)) => {
            log::trace!(
                target: "genlift",
                "F{function}: lifter {} yields, CFG {} suspends",
                count_yields(l),
                m.suspends.len()
            );
            "lifted, suspends differ".to_string()
        }
        (Ok(_), Err(_)) => "read, lifter refuses".to_string(),
        (Err(why), Ok(_)) => format!("lifted, unread: {why}"),
        (Err(why), Err(_)) => format!("neither: {why}"),
    };
    if let Ok(mut v) = VERDICTS.lock() {
        v.insert(function, verdict);
    }
}

pub fn report() {
    let Ok(v) = VERDICTS.lock() else { return };
    if v.is_empty() {
        return;
    }
    let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
    for s in v.values() {
        *counts.entry(s.as_str()).or_default() += 1;
    }
    log::debug!(target: "genlift", "CFG reading: {counts:?}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(n: u32) -> Expression {
        Expression::Value(Value::Binding(Binding::ClosureVar {
            level: 1,
            slot: n,
            scope: Default::default(),
        }))
    }
    fn int(n: i32) -> Expression {
        Expression::constant(Constant::Integer(n))
    }
    fn param(n: u32) -> Expression {
        Expression::Value(Value::Parameter(n))
    }
    fn store(n: u32, k: i32) -> Statement {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::ClosureVar {
                level: 1,
                slot: n,
                scope: Default::default(),
            }),
            value: int(k),
        }
    }
    fn result(v: Expression, done: bool) -> Terminator {
        Terminator::Return(Some(Expression::Object {
            properties: vec![
                crate::ir::ObjectProperty {
                    key: PropertyKey::Ident("value".into()),
                    value: v,
                },
                crate::ir::ObjectProperty {
                    key: PropertyKey::Ident("done".into()),
                    value: Expression::constant(Constant::Bool(done)),
                },
            ],
        }))
    }
    fn branch(l: Expression, k: i32, t: BlockId, f: BlockId) -> Terminator {
        Terminator::Branch {
            condition: Expression::binary(BinaryOp::StrictEq, l, int(k)),
            true_target: t,
            false_target: f,
        }
    }

    // A protocol at a fresh block, running on to `run`.
    fn protocol(c: &mut CFG, run: BlockId) -> BlockId {
        let (a, b, th, re) = (
            c.create_block(),
            c.create_block(),
            c.create_block(),
            c.create_block(),
        );
        c.get_mut(th).unwrap().terminator = Terminator::Throw(param(RESUME));
        c.get_mut(re).unwrap().terminator = result(param(RESUME), true);
        c.get_mut(a).unwrap().terminator = branch(param(ACTION), 1, th, b);
        c.get_mut(b).unwrap().terminator = branch(param(ACTION), 2, re, run);
        a
    }

    // `function* () { if (slot 5) yield 1; else yield 2; return 3 }`, with label
    // 2 the chain's default.
    fn machine() -> (CFG, BlockId) {
        let mut c = CFG::new();
        let (run, test2) = (c.create_block(), c.create_block());
        let (start, y1, y2, finish) = (
            c.create_block(),
            c.create_block(),
            c.create_block(),
            c.create_block(),
        );
        let (guard3, done, busy) = (c.create_block(), c.create_block(), c.create_block());
        let (p0, p1, p2) = (
            protocol(&mut c, start),
            protocol(&mut c, finish),
            protocol(&mut c, finish),
        );
        let entry = c.entry;
        c.get_mut(entry).unwrap().terminator = branch(slot(1), 2, busy, guard3);
        c.get_mut(busy).unwrap().terminator = Terminator::Throw(int(0));
        c.get_mut(guard3).unwrap().terminator = branch(slot(1), 3, done, run);
        c.get_mut(done).unwrap().terminator = result(param(RESUME), true);
        let b = c.get_mut(run).unwrap();
        b.statements.push(store(1, 2));
        b.terminator = branch(slot(2), 0, p0, test2);
        c.get_mut(test2).unwrap().terminator = branch(slot(2), 1, p1, p2);
        c.get_mut(start).unwrap().terminator = Terminator::Branch {
            condition: slot(5),
            true_target: y1,
            false_target: y2,
        };
        for (y, k, v) in [(y1, 1, 1), (y2, 2, 2)] {
            let b = c.get_mut(y).unwrap();
            b.statements.push(store(2, k));
            b.statements.push(store(1, 1));
            b.terminator = result(int(v), false);
        }
        let b = c.get_mut(finish).unwrap();
        b.statements.push(store(1, 3));
        b.terminator = result(int(3), true);
        (c, finish)
    }

    #[test]
    fn reads_a_machine_with_suspends_under_a_branch() {
        let (c, _) = machine();
        let m = read(&c).unwrap();
        assert_eq!(m.suspends.len(), 2);
        assert_eq!(m.completes.len(), 1);
        assert_eq!(m.resumes.len(), 3, "the chain's default is label 2");
    }

    #[test]
    fn a_body_reading_the_action_is_refused() {
        let (mut c, finish) = machine();
        c.get_mut(finish)
            .unwrap()
            .statements
            .push(Statement::Expr(param(ACTION)));
        assert_eq!(read(&c).unwrap_err(), "the body reads the action");
    }

    #[test]
    fn a_rewritten_machine_has_yields_and_no_machine_left() {
        let (mut c, _) = machine();
        let m = read(&c).unwrap();
        rewrite(&mut c, &m).unwrap();
        let text = format!("{:?}", c.blocks().collect::<Vec<_>>());
        assert_eq!(text.matches("Yield").count(), 2, "{text}");
        assert!(!text.contains("done"), "{text}");
        assert!(!text.contains("Parameter"), "{text}");
        assert!(!text.contains("slot: 1,"), "status left: {text}");
        assert!(!text.contains("slot: 2,"), "label left: {text}");
        assert!(text.contains("slot: 5,"), "{text}");
    }

    #[test]
    fn an_ordinary_function_is_no_machine() {
        let mut c = CFG::new();
        let entry = c.entry;
        c.get_mut(entry).unwrap().terminator = Terminator::Return(None);
        assert!(read(&c).is_err());
    }
}

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
    // Label -> the block its resume protocol runs on to.
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

struct Reader<'a> {
    cfg: &'a CFG,
    alias: HashMap<u32, Src>,
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
            _ => matches!(self.slot_of(s), Some((sl, Some(Src::Int(_)))) if sl == status),
        }
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
        let mut n = self.skip(entry, status);
        let (mut threw, mut returned) = (false, false);
        while !(threw && returned) {
            let b = self.cfg.get(n).ok_or("protocol: no block")?;
            if !b.statements.iter().all(|s| self.trivial(s, status)) {
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
            n = self.skip(off, status);
        }
        Ok(n)
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
            Some(Src::Slot(s)) if s == self.status || s == self.label => self.slots_read = true,
            _ => {}
        }
        self.walk_expression(e);
    }
}

pub fn read(cfg: &CFG) -> Result<Machine, &'static str> {
    let r = Reader {
        cfg,
        alias: aliases(cfg),
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
    // A `try` in the source: its catch stores a label and rejoins the dispatch,
    // and that label has no resume protocol. G3 reads those; the machine's own
    // catch only marks it done and rethrows.
    if cfg
        .exception_handlers
        .iter()
        .any(|h| !r.own_catch(h.catch_block, status))
    {
        return Err("a try in the source");
    }
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
    let mut resumes = BTreeMap::new();
    for (&k, &e) in &entries {
        resumes.insert(k, r.protocol(e, status)?);
    }
    // The body, from the resume entries.
    let default_resume = r.protocol(n, status);
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
    let mut scan = Scan {
        reader: &r,
        status,
        label,
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
    Ok(Machine {
        status,
        label,
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
    for &r in regs.read.difference(&regs.defined) {
        match alias.get(&r) {
            Some(Src::Int(_)) | Some(Src::Param(RESUME)) => {}
            _ => return Err("a register from the dispatch reaches the body"),
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
    let entry_to = *m.resumes.get(&0).ok_or("no entry label")?;
    // The rewrite is sure of itself from here on.
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
    cfg.retain_reachable();
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

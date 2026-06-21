//! Optimization passes over MIR.
//!
//! These are the classic data-flow optimizations that the CFG form makes
//! natural, run to a fixpoint:
//!
//! * [`const_fold`] - fold operators on constant operands, and rewrite a branch
//!   with a constant condition into an unconditional `goto`.
//! * [`algebraic_simplify`] - apply identity and absorbing-element laws
//!   (`x + 0`, `x * 1`, `x * 0`, `x - x`, …) that hold even when one side is not
//!   constant.
//! * [`copy_propagation`] - when a register is just a copy of another operand
//!   (`r = x`), replace every use of `r` with `x`. Combined with folding this
//!   also achieves constant propagation.
//! * [`local_cse`] - within a block, reuse the result of an identical earlier
//!   pure computation instead of recomputing it.
//! * [`dead_store`] - drop a store to a local that a later store in the same
//!   block overwrites before any read.
//! * [`dead_code`] - delete pure instructions whose result is never used.
//! * [`simplify_cfg`] - drop blocks unreachable from the entry, collapse a
//!   branch whose arms coincide, and thread empty `goto`-only blocks.
//!
//! Every pass preserves observable behaviour: instructions with side effects
//! (`Call`, `Store`, `SetIndex`) are never removed, only pure computations.

use std::collections::{HashMap, HashSet};

use crate::hir::{BinOp, UnOp};
use crate::mir::*;

/// Statistics from an optimization run.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MirStats {
    pub folded: usize,
    pub simplified: usize,
    pub propagated: usize,
    pub cse: usize,
    pub dead_stores: usize,
    pub removed: usize,
    pub blocks_removed: usize,
}

impl MirStats {
    pub fn total(&self) -> usize {
        self.folded
            + self.simplified
            + self.propagated
            + self.cse
            + self.dead_stores
            + self.removed
            + self.blocks_removed
    }
}

/// Optimizes every function in `program` to a fixpoint.
#[tracing::instrument(level = "debug", skip_all)]
pub fn optimize(program: &mut Program) -> MirStats {
    let mut stats = MirStats::default();
    for func in &mut program.functions {
        for _ in 0..8 {
            let folded = const_fold(func);
            let simplified = algebraic_simplify(func);
            let propagated = copy_propagation(func);
            let cse = local_cse(func);
            let dead_stores = dead_store(func);
            let removed = dead_code(func);
            let blocks_removed = simplify_cfg(func);
            stats.folded += folded;
            stats.simplified += simplified;
            stats.propagated += propagated;
            stats.cse += cse;
            stats.dead_stores += dead_stores;
            stats.removed += removed;
            stats.blocks_removed += blocks_removed;
            if folded + simplified + propagated + cse + dead_stores + removed + blocks_removed == 0
            {
                break;
            }
        }
    }
    tracing::debug!(?stats, "MIR optimization complete");
    stats
}

// ---------------------------------------------------------------------------
// Constant folding
// ---------------------------------------------------------------------------

/// Folds constant computations and constant branches. Returns the change count.
pub fn const_fold(func: &mut Function) -> usize {
    let mut count = 0;
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            if let Inst::Assign { rvalue, .. } = inst
                && let Some(folded) = fold_rvalue(rvalue)
            {
                *rvalue = Rvalue::Use(Operand::Const(folded));
                count += 1;
            }
        }
        // A branch on a constant becomes an unconditional jump.
        if let Terminator::Branch {
            cond: Operand::Const(Const::Bool(b)),
            then_bb,
            else_bb,
        } = &block.term
        {
            let target = if *b { *then_bb } else { *else_bb };
            block.term = Terminator::Goto(target);
            count += 1;
        }
    }
    count
}

fn fold_rvalue(rvalue: &Rvalue) -> Option<Const> {
    match rvalue {
        Rvalue::Unary(op, Operand::Const(c)) => fold_unary(*op, c),
        Rvalue::Binary(op, Operand::Const(a), Operand::Const(b)) => fold_binary(*op, a, b),
        Rvalue::Concat(Operand::Const(Const::Str(a)), Operand::Const(Const::Str(b))) => {
            Some(Const::Str(format!("{a}{b}").into()))
        }
        _ => None,
    }
}

fn fold_unary(op: UnOp, c: &Const) -> Option<Const> {
    match (op, c) {
        (UnOp::Neg, Const::Int(v)) => Some(Const::Int(v.wrapping_neg())),
        (UnOp::Neg, Const::Float(v)) => Some(Const::Float(-v)),
        (UnOp::Not, Const::Bool(v)) => Some(Const::Bool(!v)),
        _ => None,
    }
}

fn fold_binary(op: BinOp, a: &Const, b: &Const) -> Option<Const> {
    use Const::{Bool, Float, Int};
    Some(match (a, b) {
        (Int(x), Int(y)) => match op {
            BinOp::Add => Int(x.wrapping_add(*y)),
            BinOp::Sub => Int(x.wrapping_sub(*y)),
            BinOp::Mul => Int(x.wrapping_mul(*y)),
            BinOp::Div => Int(x.checked_div(*y)?),
            BinOp::Rem => Int(x.checked_rem(*y)?),
            BinOp::Eq => Bool(x == y),
            BinOp::Ne => Bool(x != y),
            BinOp::Lt => Bool(x < y),
            BinOp::Le => Bool(x <= y),
            BinOp::Gt => Bool(x > y),
            BinOp::Ge => Bool(x >= y),
            BinOp::And | BinOp::Or => return None,
        },
        (Float(x), Float(y)) => match op {
            BinOp::Add => Float(x + y),
            BinOp::Sub => Float(x - y),
            BinOp::Mul => Float(x * y),
            BinOp::Div => Float(x / y),
            BinOp::Rem => Float(x % y),
            BinOp::Eq => Bool(x == y),
            BinOp::Ne => Bool(x != y),
            BinOp::Lt => Bool(x < y),
            BinOp::Le => Bool(x <= y),
            BinOp::Gt => Bool(x > y),
            BinOp::Ge => Bool(x >= y),
            BinOp::And | BinOp::Or => return None,
        },
        (Bool(x), Bool(y)) => match op {
            BinOp::Eq => Bool(x == y),
            BinOp::Ne => Bool(x != y),
            _ => return None,
        },
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Algebraic simplification
// ---------------------------------------------------------------------------

/// Applies integer identity and absorbing-element laws to `Binary` rvalues,
/// turning them into a copy of an operand or a constant. These hold regardless
/// of whether the other side is a constant, so they catch cases folding cannot.
///
/// Only integer rules are applied: float `x + 0.0` would mishandle `-0.0`/`NaN`,
/// and `&&`/`||` are lowered to control flow, never to a `Binary` here.
pub fn algebraic_simplify(func: &mut Function) -> usize {
    let mut count = 0;
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            if let Inst::Assign { rvalue, .. } = inst
                && let Rvalue::Binary(op, a, b) = rvalue
                && let Some(simpler) = simplify_binary(*op, a, b)
            {
                *rvalue = simpler;
                count += 1;
            }
        }
    }
    count
}

fn is_int(op: &Operand, want: i64) -> bool {
    matches!(op, Operand::Const(Const::Int(v)) if *v == want)
}

fn same_reg(a: &Operand, b: &Operand) -> bool {
    matches!((a, b), (Operand::Reg(x), Operand::Reg(y)) if x == y)
}

/// The simplified rvalue for `a op b`, if an integer law applies.
fn simplify_binary(op: BinOp, a: &Operand, b: &Operand) -> Option<Rvalue> {
    let use_op = |o: &Operand| Rvalue::Use(o.clone());
    let zero = || Rvalue::Use(Operand::Const(Const::Int(0)));
    match op {
        // x + 0 = 0 + x = x
        BinOp::Add if is_int(b, 0) => Some(use_op(a)),
        BinOp::Add if is_int(a, 0) => Some(use_op(b)),
        // x - 0 = x; x - x = 0
        BinOp::Sub if is_int(b, 0) => Some(use_op(a)),
        BinOp::Sub if same_reg(a, b) => Some(zero()),
        // x * 1 = 1 * x = x; x * 0 = 0 * x = 0
        BinOp::Mul if is_int(b, 1) => Some(use_op(a)),
        BinOp::Mul if is_int(a, 1) => Some(use_op(b)),
        BinOp::Mul if is_int(a, 0) || is_int(b, 0) => Some(zero()),
        // x / 1 = x; x % 1 = 0
        BinOp::Div if is_int(b, 1) => Some(use_op(a)),
        BinOp::Rem if is_int(b, 1) => Some(zero()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Copy / constant propagation
// ---------------------------------------------------------------------------

/// Replaces uses of registers that are simple copies (`r = x`) with `x`.
pub fn copy_propagation(func: &mut Function) -> usize {
    // Map each register that is defined exactly by `Use(operand)` to that
    // operand, resolving chains so `a = b; b = 1` propagates `1` for `a`.
    let mut copies: HashMap<Reg, Operand> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Inst::Assign {
                dst,
                rvalue: Rvalue::Use(src),
            } = inst
            {
                let resolved = resolve(src, &copies);
                copies.insert(*dst, resolved);
            }
        }
    }
    if copies.is_empty() {
        return 0;
    }

    let mut count = 0;
    map_operands(func, &mut |op| {
        if let Operand::Reg(r) = op
            && let Some(repl) = copies.get(r)
        {
            *op = repl.clone();
            count += 1;
        }
    });
    count
}

/// Follows a copy chain to the ultimate operand.
fn resolve(op: &Operand, copies: &HashMap<Reg, Operand>) -> Operand {
    let mut cur = op.clone();
    let mut guard = 0;
    while let Operand::Reg(r) = cur {
        match copies.get(&r) {
            Some(next) if guard < 1000 => {
                cur = next.clone();
                guard += 1;
            }
            _ => break,
        }
    }
    cur
}

// ---------------------------------------------------------------------------
// Dead-code elimination
// ---------------------------------------------------------------------------

/// Removes pure `Assign` instructions whose destination register is never read.
pub fn dead_code(func: &mut Function) -> usize {
    let used = used_regs(func);
    let mut count = 0;
    for block in &mut func.blocks {
        let before = block.insts.len();
        block.insts.retain(|inst| match inst {
            Inst::Assign { dst, rvalue } => used.contains(dst) || !rvalue_is_pure(rvalue),
            // Stores, set-index, and calls have effects and are always kept.
            _ => true,
        });
        count += before - block.insts.len();
    }
    count
}

/// The set of registers read anywhere in the function.
fn used_regs(func: &Function) -> HashSet<Reg> {
    let mut used = HashSet::new();
    let mut record = |op: &Operand| {
        if let Some(r) = op.reg() {
            used.insert(r);
        }
    };
    for block in &func.blocks {
        for inst in &block.insts {
            match inst {
                Inst::Assign { rvalue, .. } => rvalue_operands(rvalue, &mut record),
                Inst::Store { src, .. } => record(src),
                Inst::SetIndex { base, index, value } => {
                    record(base);
                    record(index);
                    record(value);
                }
                Inst::Call { args, .. } => args.iter().for_each(&mut record),
            }
        }
        match &block.term {
            Terminator::Branch { cond, .. } => record(cond),
            Terminator::Return(o) => record(o),
            Terminator::Goto(_) | Terminator::Unreachable => {}
        }
    }
    used
}

/// Whether an rvalue has no side effects (all current rvalues are pure; only
/// `Call`/`Store`/`SetIndex` instructions effect the world).
fn rvalue_is_pure(_rvalue: &Rvalue) -> bool {
    true
}

// ---------------------------------------------------------------------------
// Dead-store elimination
// ---------------------------------------------------------------------------

/// Removes a `Store` to a local that a later store in the *same block*
/// overwrites before any intervening read of that local.
///
/// Working within a single block keeps this sound without a cross-block
/// liveness analysis: a store that is overwritten before the block ends cannot
/// have been observed by any successor, because the local's value never escaped
/// the block. Only `Load` reads a local, so it is the sole thing that keeps an
/// earlier store alive.
pub fn dead_store(func: &mut Function) -> usize {
    let mut count = 0;
    for block in &mut func.blocks {
        // local -> index of its latest store that has not yet been read.
        let mut pending: HashMap<LocalId, usize> = HashMap::new();
        let mut dead: HashSet<usize> = HashSet::new();
        for (i, inst) in block.insts.iter().enumerate() {
            match inst {
                // A read keeps the local's pending store alive.
                Inst::Assign {
                    rvalue: Rvalue::Load(local),
                    ..
                } => {
                    pending.remove(local);
                }
                // A new store kills the previous unread store to the same local.
                Inst::Store { local, .. } => {
                    if let Some(prev) = pending.insert(*local, i) {
                        dead.insert(prev);
                    }
                }
                _ => {}
            }
        }
        if dead.is_empty() {
            continue;
        }
        let before = block.insts.len();
        let mut i = 0;
        block.insts.retain(|_| {
            let keep = !dead.contains(&i);
            i += 1;
            keep
        });
        count += before - block.insts.len();
    }
    count
}

// ---------------------------------------------------------------------------
// CFG simplification
// ---------------------------------------------------------------------------

/// Collapses degenerate branches, threads empty blocks, and removes blocks
/// unreachable from the entry. Returns the number of blocks removed.
pub fn simplify_cfg(func: &mut Function) -> usize {
    // 1. Collapse a branch whose arms are identical into a goto.
    for block in &mut func.blocks {
        if let Terminator::Branch {
            then_bb, else_bb, ..
        } = &block.term
            && then_bb == else_bb
        {
            block.term = Terminator::Goto(*then_bb);
        }
    }

    // 2. Thread jumps through empty `goto`-only blocks (those with no
    //    instructions and a `Goto` terminator).
    let forward: HashMap<BlockId, BlockId> = func
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(i, b)| match (&b.term, b.insts.is_empty()) {
            (Terminator::Goto(t), true) if BlockId(i as u32) != *t => Some((BlockId(i as u32), *t)),
            _ => None,
        })
        .collect();
    if !forward.is_empty() {
        for block in &mut func.blocks {
            redirect(&mut block.term, &forward);
        }
    }

    // 3. Remove blocks unreachable from the entry.
    let reachable = reachable_blocks(func);
    if reachable.len() == func.blocks.len() {
        return 0;
    }
    remove_unreachable(func, &reachable)
}

/// Rewrites a terminator's targets through the forwarding table (one hop, since
/// the pass runs to a fixpoint).
fn redirect(term: &mut Terminator, forward: &HashMap<BlockId, BlockId>) {
    let hop = |b: BlockId| forward.get(&b).copied().unwrap_or(b);
    match term {
        Terminator::Goto(t) => *t = hop(*t),
        Terminator::Branch {
            then_bb, else_bb, ..
        } => {
            *then_bb = hop(*then_bb);
            *else_bb = hop(*else_bb);
        }
        Terminator::Return(_) | Terminator::Unreachable => {}
    }
}

fn reachable_blocks(func: &Function) -> HashSet<BlockId> {
    let mut seen = HashSet::new();
    let mut stack = vec![func.entry];
    while let Some(b) = stack.pop() {
        if seen.insert(b) {
            stack.extend(func.block(b).term.successors());
        }
    }
    seen
}

/// Drops unreachable blocks and renumbers the survivors, fixing up all targets.
fn remove_unreachable(func: &mut Function, reachable: &HashSet<BlockId>) -> usize {
    let removed = func.blocks.len() - reachable.len();

    // Old BlockId -> new dense index.
    let mut remap: HashMap<BlockId, BlockId> = HashMap::new();
    let mut next = 0u32;
    for i in 0..func.blocks.len() as u32 {
        if reachable.contains(&BlockId(i)) {
            remap.insert(BlockId(i), BlockId(next));
            next += 1;
        }
    }

    let old = std::mem::take(&mut func.blocks);
    for (i, mut block) in old.into_iter().enumerate() {
        if !reachable.contains(&BlockId(i as u32)) {
            continue;
        }
        renumber(&mut block.term, &remap);
        func.blocks.push(block);
    }
    func.entry = remap[&func.entry];
    removed
}

fn renumber(term: &mut Terminator, remap: &HashMap<BlockId, BlockId>) {
    let to = |b: BlockId| remap.get(&b).copied().unwrap_or(b);
    match term {
        Terminator::Goto(t) => *t = to(*t),
        Terminator::Branch {
            then_bb, else_bb, ..
        } => {
            *then_bb = to(*then_bb);
            *else_bb = to(*else_bb);
        }
        Terminator::Return(_) | Terminator::Unreachable => {}
    }
}

// ---------------------------------------------------------------------------
// Operand traversal helpers
// ---------------------------------------------------------------------------

/// Applies `f` to every operand *read* in the function (not definitions).
fn map_operands(func: &mut Function, f: &mut impl FnMut(&mut Operand)) {
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            match inst {
                Inst::Assign { rvalue, .. } => rvalue_operands_mut(rvalue, f),
                Inst::Store { src, .. } => f(src),
                Inst::SetIndex { base, index, value } => {
                    f(base);
                    f(index);
                    f(value);
                }
                Inst::Call { args, .. } => args.iter_mut().for_each(&mut *f),
            }
        }
        match &mut block.term {
            Terminator::Branch { cond, .. } => f(cond),
            Terminator::Return(o) => f(o),
            Terminator::Goto(_) | Terminator::Unreachable => {}
        }
    }
}

fn rvalue_operands(rvalue: &Rvalue, f: &mut impl FnMut(&Operand)) {
    match rvalue {
        Rvalue::Use(o) | Rvalue::Unary(_, o) => f(o),
        Rvalue::Binary(_, a, b) | Rvalue::Concat(a, b) | Rvalue::Index(a, b) => {
            f(a);
            f(b);
        }
        Rvalue::MakeArray(elems) => elems.iter().for_each(f),
        Rvalue::Load(_) => {}
    }
}

// ---------------------------------------------------------------------------
// Local common-subexpression elimination
// ---------------------------------------------------------------------------

/// Within each basic block, replaces a repeated pure computation with a copy of
/// the register that first produced it. Because MIR registers are
/// single-assignment, a value computed from the same operands earlier in the
/// block is guaranteed still valid, so `b = x + y` following `a = x + y`
/// becomes `b = a` (which copy propagation then forwards).
///
/// Eligible rvalues are the side-effect-free ones: `Unary`, `Binary`, `Concat`,
/// and `Load`. A repeated `Load(l)` is forwarded too (redundant-load
/// elimination), which is what lets arithmetic over the *same* variable
/// deduplicate after copy propagation reunites the operand registers. To keep
/// load forwarding sound, every cached `Load(l)` is dropped when a `Store(l)`
/// could have changed `l`. `Index` and `MakeArray` are never reused: an array's
/// contents are mutable and each `MakeArray` yields a distinct value.
pub fn local_cse(func: &mut Function) -> usize {
    let mut count = 0;
    for block in &mut func.blocks {
        // First occurrences of each eligible rvalue, with the register holding
        // the result. Reset per block (block-local analysis only).
        let mut seen: Vec<(Rvalue, Reg)> = Vec::new();
        for inst in &mut block.insts {
            match inst {
                Inst::Assign { dst, rvalue } if cse_eligible(rvalue) => {
                    match seen
                        .iter()
                        .find_map(|(rv, r)| cse_eq(rv, rvalue).then_some(*r))
                    {
                        Some(prev) => {
                            *rvalue = Rvalue::Use(Operand::Reg(prev));
                            count += 1;
                        }
                        None => seen.push((rvalue.clone(), *dst)),
                    }
                }
                // A store to `local` invalidates any cached load of it.
                Inst::Store { local, .. } => {
                    seen.retain(|(rv, _)| !matches!(rv, Rvalue::Load(l) if l == local));
                }
                _ => {}
            }
        }
    }
    count
}

/// Whether an rvalue is a pure value computation safe to deduplicate.
fn cse_eligible(rvalue: &Rvalue) -> bool {
    matches!(
        rvalue,
        Rvalue::Unary(..) | Rvalue::Binary(..) | Rvalue::Concat(..) | Rvalue::Load(_)
    )
}

/// Structural equality for the CSE-eligible rvalues. Operands compare by value,
/// so identical constants and identical (single-assignment) registers match.
fn cse_eq(a: &Rvalue, b: &Rvalue) -> bool {
    match (a, b) {
        (Rvalue::Unary(o1, x1), Rvalue::Unary(o2, x2)) => o1 == o2 && x1 == x2,
        (Rvalue::Binary(o1, a1, b1), Rvalue::Binary(o2, a2, b2)) => {
            o1 == o2 && a1 == a2 && b1 == b2
        }
        (Rvalue::Concat(a1, b1), Rvalue::Concat(a2, b2)) => a1 == a2 && b1 == b2,
        (Rvalue::Load(l1), Rvalue::Load(l2)) => l1 == l2,
        _ => false,
    }
}

fn rvalue_operands_mut(rvalue: &mut Rvalue, f: &mut impl FnMut(&mut Operand)) {
    match rvalue {
        Rvalue::Use(o) | Rvalue::Unary(_, o) => f(o),
        Rvalue::Binary(_, a, b) | Rvalue::Concat(a, b) | Rvalue::Index(a, b) => {
            f(a);
            f(b);
        }
        Rvalue::MakeArray(elems) => elems.iter_mut().for_each(f),
        Rvalue::Load(_) => {}
    }
}

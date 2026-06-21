//! Dead-code elimination.
//!
//! Removes code that cannot affect the program's result, in three sound forms:
//!
//! * **Unreachable after `return`**  statements (and the tail) following a
//!   `return` in a block are deleted.
//! * **`while false`**  a loop whose condition is the literal `false` never
//!   runs and is removed.
//! * **Unused pure `let`**  a binding whose slot is never read and whose
//!   initialiser is side-effect-free is removed. Reads are counted across the
//!   whole function, so this is conservative and correct.
//!
//! Nested blocks (branches, loop bodies, block expressions) are processed
//! recursively. The pass never removes anything with side effects; that
//! judgement is delegated entirely to [`is_pure`](crate::opt::is_pure).

use std::collections::HashSet;

use crate::hir::{Block, Expr, ExprKind, Function, Hir, LocalId, Stmt};
use crate::opt::is_pure;

/// Runs the DCE pass over the whole program; returns the number of removals.
pub fn run(hir: &mut Hir) -> usize {
    let mut count = 0;
    for func in &mut hir.functions {
        dce_function(func, &mut count);
    }
    count
}

fn dce_function(func: &mut Function, count: &mut usize) {
    let reads = collect_reads(func);
    dce_block(&mut func.body, &reads, count);
}

fn dce_block(block: &mut Block, reads: &HashSet<u32>, count: &mut usize) {
    // Recurse into nested blocks first.
    for stmt in &mut block.stmts {
        dce_stmt(stmt, reads, count);
    }
    if let Some(tail) = &mut block.tail {
        dce_expr(tail, reads, count);
    }

    // Drop everything after the first `return`: it is unreachable.
    if let Some(pos) = block
        .stmts
        .iter()
        .position(|s| matches!(s, Stmt::Return(_)))
    {
        let removed = block.stmts.len() - (pos + 1) + usize::from(block.tail.is_some());
        if removed > 0 {
            block.stmts.truncate(pos + 1);
            block.tail = None;
            *count += removed;
        }
    }

    // Remove `while false { … }` and unused pure `let`s.
    let before = block.stmts.len();
    block.stmts.retain(|stmt| keep_stmt(stmt, reads));
    *count += before - block.stmts.len();
}

/// Whether a statement should be kept (`false` ⇒ it is dead and removable).
fn keep_stmt(stmt: &Stmt, reads: &HashSet<u32>) -> bool {
    match stmt {
        Stmt::While { cond, .. } => !matches!(cond.kind, ExprKind::Bool(false)),
        Stmt::Let { local, value } => reads.contains(&local.0) || !is_pure(value),
        _ => true,
    }
}

fn dce_stmt(stmt: &mut Stmt, reads: &HashSet<u32>, count: &mut usize) {
    match stmt {
        Stmt::Let { value, .. } => dce_expr(value, reads, count),
        Stmt::Expr(e) => dce_expr(e, reads, count),
        Stmt::Return(Some(e)) => dce_expr(e, reads, count),
        Stmt::Return(None) => {}
        Stmt::While { cond, body } => {
            dce_expr(cond, reads, count);
            dce_block(body, reads, count);
        }
        Stmt::For {
            start, end, body, ..
        } => {
            dce_expr(start, reads, count);
            dce_expr(end, reads, count);
            dce_block(body, reads, count);
        }
        Stmt::Break | Stmt::Continue => {}
    }
}

fn dce_expr(expr: &mut Expr, reads: &HashSet<u32>, count: &mut usize) {
    match &mut expr.kind {
        ExprKind::Unary { rhs, .. } => dce_expr(rhs, reads, count),
        ExprKind::Binary { lhs, rhs, .. } => {
            dce_expr(lhs, reads, count);
            dce_expr(rhs, reads, count);
        }
        ExprKind::Call { args, .. } => args.iter_mut().for_each(|a| dce_expr(a, reads, count)),
        ExprKind::Assign { value, .. } => dce_expr(value, reads, count),
        ExprKind::ArrayLit(elems) => elems.iter_mut().for_each(|e| dce_expr(e, reads, count)),
        ExprKind::Index { base, index } => {
            dce_expr(base, reads, count);
            dce_expr(index, reads, count);
        }
        ExprKind::SetIndex { base, index, value } => {
            dce_expr(base, reads, count);
            dce_expr(index, reads, count);
            dce_expr(value, reads, count);
        }
        ExprKind::StructLit(fields) => fields.iter_mut().for_each(|e| dce_expr(e, reads, count)),
        ExprKind::GetField { base, .. } => dce_expr(base, reads, count),
        ExprKind::SetField { base, value, .. } => {
            dce_expr(base, reads, count);
            dce_expr(value, reads, count);
        }
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            dce_expr(cond, reads, count);
            dce_block(then_branch, reads, count);
            if let Some(e) = else_branch {
                dce_expr(e, reads, count);
            }
        }
        ExprKind::Block(block) => dce_block(block, reads, count),
        _ => {}
    }
}

/// Collects the set of locals that are *read* anywhere in the function.
///
/// Only `Local` reads count; an assignment's target is a write, not a read, so
/// a variable that is only ever assigned (never observed) is correctly seen as
/// unused.
fn collect_reads(func: &Function) -> HashSet<u32> {
    let mut reads = HashSet::new();
    collect_block(&func.body, &mut reads);
    reads
}

fn collect_block(block: &Block, reads: &mut HashSet<u32>) {
    for stmt in &block.stmts {
        collect_stmt(stmt, reads);
    }
    if let Some(tail) = &block.tail {
        collect_expr(tail, reads);
    }
}

fn collect_stmt(stmt: &Stmt, reads: &mut HashSet<u32>) {
    match stmt {
        Stmt::Let { value, .. } => collect_expr(value, reads),
        Stmt::Expr(e) => collect_expr(e, reads),
        Stmt::Return(Some(e)) => collect_expr(e, reads),
        Stmt::Return(None) => {}
        Stmt::While { cond, body } => {
            collect_expr(cond, reads);
            collect_block(body, reads);
        }
        Stmt::For {
            var,
            end_var,
            start,
            end,
            body,
        } => {
            // The loop variable and cached bound are implicitly read by the
            // generated counter, so mark them live.
            reads.insert(var.0);
            reads.insert(end_var.0);
            collect_expr(start, reads);
            collect_expr(end, reads);
            collect_block(body, reads);
        }
        Stmt::Break | Stmt::Continue => {}
    }
}

fn collect_expr(expr: &Expr, reads: &mut HashSet<u32>) {
    match &expr.kind {
        ExprKind::Local(LocalId(id)) => {
            reads.insert(*id);
        }
        ExprKind::Unary { rhs, .. } => collect_expr(rhs, reads),
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_expr(lhs, reads);
            collect_expr(rhs, reads);
        }
        ExprKind::Call { args, .. } => args.iter().for_each(|a| collect_expr(a, reads)),
        // The assignment target is a write; only its value is read.
        ExprKind::Assign { value, .. } => collect_expr(value, reads),
        ExprKind::ArrayLit(elems) => elems.iter().for_each(|e| collect_expr(e, reads)),
        ExprKind::Index { base, index } => {
            collect_expr(base, reads);
            collect_expr(index, reads);
        }
        ExprKind::SetIndex { base, index, value } => {
            collect_expr(base, reads);
            collect_expr(index, reads);
            collect_expr(value, reads);
        }
        ExprKind::StructLit(fields) => fields.iter().for_each(|e| collect_expr(e, reads)),
        ExprKind::GetField { base, .. } => collect_expr(base, reads),
        ExprKind::SetField { base, value, .. } => {
            collect_expr(base, reads);
            collect_expr(value, reads);
        }
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_expr(cond, reads);
            collect_block(then_branch, reads);
            if let Some(e) = else_branch {
                collect_expr(e, reads);
            }
        }
        ExprKind::Block(block) => collect_block(block, reads),
        _ => {}
    }
}

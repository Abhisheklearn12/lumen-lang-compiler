//! Function inlining.
//!
//! Replaces a call to a small, pure, non-recursive *expression function* with a
//! copy of its body, so the optimizer's other passes (constant folding, dead
//! code) can then see across the former call boundary.
//!
//! To stay obviously correct, only the simplest functions are inlined: those
//! whose body is a single expression that reads only its parameters and
//! performs no statements, loops, assignments, or mutation. For such a callee,
//! `f(a, b)` becomes a block `{ let p0 = a; let p1 = b; <body> }`, with the
//! callee's parameter slots remapped to fresh locals in the caller. Side
//! effects in the arguments still run exactly once, in order, because each
//! argument is bound with a `let`.

use std::collections::HashMap;

use crate::hir::{Block, Callee, Expr, ExprKind, FnId, Hir, LocalDecl, LocalId, Stmt};
use crate::sema::types::Type;

/// A function eligible for inlining: its parameter count and a clonable body.
struct Candidate {
    param_count: usize,
    body: Expr,
}

/// Inlines eligible calls throughout the program. Returns how many call sites
/// were inlined.
pub fn run(hir: &mut Hir) -> usize {
    let candidates = collect_candidates(hir);
    if candidates.is_empty() {
        return 0;
    }
    let mut count = 0;
    for (i, func) in hir.functions.iter_mut().enumerate() {
        let self_id = FnId(i as u32);
        // Split the borrow so the body can be walked while locals grow.
        let crate::hir::Function { locals, body, .. } = func;
        inline_block(body, &candidates, self_id, locals, &mut count);
    }
    count
}

/// Gathers the functions that are safe to inline.
fn collect_candidates(hir: &Hir) -> HashMap<FnId, Candidate> {
    let mut out = HashMap::new();
    for (i, func) in hir.functions.iter().enumerate() {
        let id = FnId(i as u32);
        if id == hir.main {
            continue;
        }
        if !func.body.stmts.is_empty() {
            continue;
        }
        let Some(tail) = &func.body.tail else {
            continue;
        };
        if is_simple(tail, func.param_count) && !calls(tail, id) {
            out.insert(
                id,
                Candidate {
                    param_count: func.param_count,
                    body: (**tail).clone(),
                },
            );
        }
    }
    out
}

/// Whether `expr` is a pure expression that reads only the first `params`
/// locals and contains no statements, loops, assignments, or mutation.
fn is_simple(expr: &Expr, params: usize) -> bool {
    match &expr.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_) => true,
        ExprKind::Local(id) => (id.0 as usize) < params,
        ExprKind::Unary { rhs, .. } => is_simple(rhs, params),
        ExprKind::Binary { lhs, rhs, .. } => is_simple(lhs, params) && is_simple(rhs, params),
        ExprKind::Call { args, .. } => args.iter().all(|a| is_simple(a, params)),
        ExprKind::ArrayLit(es) | ExprKind::StructLit(es) => es.iter().all(|e| is_simple(e, params)),
        ExprKind::Index { base, index } => is_simple(base, params) && is_simple(index, params),
        ExprKind::GetField { base, .. } => is_simple(base, params),
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            is_simple(cond, params)
                && simple_block(then_branch, params)
                && else_branch.as_ref().is_none_or(|e| is_simple(e, params))
        }
        ExprKind::Block(b) => simple_block(b, params),
        // Assignments, stores, and statement-bearing forms disqualify a body.
        ExprKind::Assign { .. } | ExprKind::SetIndex { .. } | ExprKind::SetField { .. } => false,
    }
}

/// A block is simple when it has no statements and a simple tail.
fn simple_block(block: &Block, params: usize) -> bool {
    block.stmts.is_empty() && block.tail.as_deref().is_none_or(|t| is_simple(t, params))
}

/// Whether `expr` contains a call to function `id` (recursion guard).
fn calls(expr: &Expr, id: FnId) -> bool {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            matches!(callee, Callee::Fn(f) if *f == id) || args.iter().any(|a| calls(a, id))
        }
        ExprKind::Unary { rhs, .. } => calls(rhs, id),
        ExprKind::Binary { lhs, rhs, .. } => calls(lhs, id) || calls(rhs, id),
        ExprKind::ArrayLit(es) | ExprKind::StructLit(es) => es.iter().any(|e| calls(e, id)),
        ExprKind::Index { base, index } => calls(base, id) || calls(index, id),
        ExprKind::GetField { base, .. } => calls(base, id),
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            calls(cond, id)
                || then_branch.tail.as_deref().is_some_and(|t| calls(t, id))
                || else_branch.as_deref().is_some_and(|e| calls(e, id))
        }
        ExprKind::Block(b) => b.tail.as_deref().is_some_and(|t| calls(t, id)),
        _ => false,
    }
}

// ---- inlining walk ----

fn inline_block(
    block: &mut Block,
    cands: &HashMap<FnId, Candidate>,
    self_id: FnId,
    locals: &mut Vec<LocalDecl>,
    count: &mut usize,
) {
    for stmt in &mut block.stmts {
        inline_stmt(stmt, cands, self_id, locals, count);
    }
    if let Some(tail) = &mut block.tail {
        inline_expr(tail, cands, self_id, locals, count);
    }
}

fn inline_stmt(
    stmt: &mut Stmt,
    cands: &HashMap<FnId, Candidate>,
    self_id: FnId,
    locals: &mut Vec<LocalDecl>,
    count: &mut usize,
) {
    match stmt {
        Stmt::Let { value, .. } => inline_expr(value, cands, self_id, locals, count),
        Stmt::Expr(e) => inline_expr(e, cands, self_id, locals, count),
        Stmt::Return(Some(e)) => inline_expr(e, cands, self_id, locals, count),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        Stmt::While { cond, body } => {
            inline_expr(cond, cands, self_id, locals, count);
            inline_block(body, cands, self_id, locals, count);
        }
        Stmt::For {
            start, end, body, ..
        } => {
            inline_expr(start, cands, self_id, locals, count);
            inline_expr(end, cands, self_id, locals, count);
            inline_block(body, cands, self_id, locals, count);
        }
    }
}

fn inline_expr(
    expr: &mut Expr,
    cands: &HashMap<FnId, Candidate>,
    self_id: FnId,
    locals: &mut Vec<LocalDecl>,
    count: &mut usize,
) {
    // Inline children first so arguments are already simplified.
    match &mut expr.kind {
        ExprKind::Unary { rhs, .. } => inline_expr(rhs, cands, self_id, locals, count),
        ExprKind::Binary { lhs, rhs, .. } => {
            inline_expr(lhs, cands, self_id, locals, count);
            inline_expr(rhs, cands, self_id, locals, count);
        }
        ExprKind::Call { args, .. } => {
            for a in args.iter_mut() {
                inline_expr(a, cands, self_id, locals, count);
            }
        }
        ExprKind::Assign { value, .. } => inline_expr(value, cands, self_id, locals, count),
        ExprKind::ArrayLit(es) | ExprKind::StructLit(es) => {
            for e in es.iter_mut() {
                inline_expr(e, cands, self_id, locals, count);
            }
        }
        ExprKind::Index { base, index } => {
            inline_expr(base, cands, self_id, locals, count);
            inline_expr(index, cands, self_id, locals, count);
        }
        ExprKind::GetField { base, .. } => inline_expr(base, cands, self_id, locals, count),
        ExprKind::SetIndex { base, index, value } => {
            inline_expr(base, cands, self_id, locals, count);
            inline_expr(index, cands, self_id, locals, count);
            inline_expr(value, cands, self_id, locals, count);
        }
        ExprKind::SetField { base, value, .. } => {
            inline_expr(base, cands, self_id, locals, count);
            inline_expr(value, cands, self_id, locals, count);
        }
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            inline_expr(cond, cands, self_id, locals, count);
            inline_block(then_branch, cands, self_id, locals, count);
            if let Some(e) = else_branch {
                inline_expr(e, cands, self_id, locals, count);
            }
        }
        ExprKind::Block(b) => inline_block(b, cands, self_id, locals, count),
        _ => {}
    }

    // Then inline this node if it is an eligible call.
    let should_inline = matches!(
        &expr.kind,
        ExprKind::Call { callee: Callee::Fn(id), .. }
            if *id != self_id && cands.contains_key(id)
    );
    if should_inline {
        let ExprKind::Call {
            callee: Callee::Fn(id),
            args,
        } = std::mem::replace(&mut expr.kind, ExprKind::Int(0))
        else {
            unreachable!()
        };
        let cand = &cands[&id];
        expr.kind = build_inlined(cand, args, expr.ty, locals);
        *count += 1;
    }
}

/// Builds the block that replaces an inlined call.
fn build_inlined(
    cand: &Candidate,
    args: Vec<Expr>,
    result_ty: Type,
    locals: &mut Vec<LocalDecl>,
) -> ExprKind {
    // Allocate a fresh caller local for each parameter and bind the argument.
    let mut map = HashMap::new();
    let mut stmts = Vec::with_capacity(cand.param_count);
    for (i, arg) in args.into_iter().enumerate().take(cand.param_count) {
        let new_local = LocalId(locals.len() as u32);
        locals.push(LocalDecl {
            name: format!("<inl{}>", new_local.0),
            ty: arg.ty,
        });
        map.insert(i as u32, new_local);
        stmts.push(Stmt::Let {
            local: new_local,
            value: arg,
        });
    }
    // Clone the callee body and remap its parameter reads to the new locals.
    let mut body = cand.body.clone();
    remap_locals(&mut body, &map);
    ExprKind::Block(Block {
        stmts,
        tail: Some(Box::new(body)),
        ty: result_ty,
    })
}

/// Remaps parameter local reads in a cloned callee body to the caller's locals.
fn remap_locals(expr: &mut Expr, map: &HashMap<u32, LocalId>) {
    match &mut expr.kind {
        ExprKind::Local(id) => {
            if let Some(new) = map.get(&id.0) {
                *id = *new;
            }
        }
        ExprKind::Unary { rhs, .. } => remap_locals(rhs, map),
        ExprKind::Binary { lhs, rhs, .. } => {
            remap_locals(lhs, map);
            remap_locals(rhs, map);
        }
        ExprKind::Call { args, .. } => args.iter_mut().for_each(|a| remap_locals(a, map)),
        ExprKind::ArrayLit(es) | ExprKind::StructLit(es) => {
            es.iter_mut().for_each(|e| remap_locals(e, map))
        }
        ExprKind::Index { base, index } => {
            remap_locals(base, map);
            remap_locals(index, map);
        }
        ExprKind::GetField { base, .. } => remap_locals(base, map),
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            remap_locals(cond, map);
            if let Some(t) = &mut then_branch.tail {
                remap_locals(t, map);
            }
            if let Some(e) = else_branch {
                remap_locals(e, map);
            }
        }
        ExprKind::Block(b) => {
            if let Some(t) = &mut b.tail {
                remap_locals(t, map);
            }
        }
        // Simple bodies contain none of the remaining forms.
        _ => {}
    }
}

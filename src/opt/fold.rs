//! Constant folding, algebraic simplification, and constant-condition collapse.
//!
//! Runs a single post-order walk: children are simplified before their parent,
//! so a parent sees already-folded operands. Each rewrite is value-preserving:
//!
//! * **Constant folding**  operators on literal operands become a literal
//!   (`1 + 2` → `3`, `1 < 2` → `true`). Integer arithmetic uses wrapping
//!   semantics that match the VM exactly; division or remainder by zero is left
//!   un-folded so the runtime error is preserved.
//! * **Algebraic identities**  `x + 0`, `x * 1`, `x - 0` → `x`; `x * 0` → `0`
//!   only when `x` is pure (otherwise its side effects must run).
//! * **Short-circuit logic**  `true || x` → `true`, `false && x` → `false`,
//!   etc., which is sound because `||`/`&&` would not evaluate `x` anyway.
//! * **Constant conditions**  `if true { a } else { b }` → `a`.

use crate::hir::{BinOp, Block, Expr, ExprKind, Function, Hir, Stmt, UnOp};
use crate::opt::{is_pure, unit_expr};
use crate::sema::types::Type;

/// Runs the fold pass over the whole program; returns the number of rewrites.
pub fn run(hir: &mut Hir) -> usize {
    let mut count = 0;
    for func in &mut hir.functions {
        fold_function(func, &mut count);
    }
    count
}

fn fold_function(func: &mut Function, count: &mut usize) {
    fold_block(&mut func.body, count);
}

fn fold_block(block: &mut Block, count: &mut usize) {
    for stmt in &mut block.stmts {
        fold_stmt(stmt, count);
    }
    if let Some(tail) = &mut block.tail {
        fold_expr(tail, count);
        block.ty = tail.ty;
    }
}

fn fold_stmt(stmt: &mut Stmt, count: &mut usize) {
    match stmt {
        Stmt::Let { value, .. } => fold_expr(value, count),
        Stmt::Expr(e) => fold_expr(e, count),
        Stmt::Return(e) => {
            if let Some(e) = e {
                fold_expr(e, count);
            }
        }
        Stmt::While { cond, body } => {
            fold_expr(cond, count);
            fold_block(body, count);
        }
        Stmt::For {
            start, end, body, ..
        } => {
            fold_expr(start, count);
            fold_expr(end, count);
            fold_block(body, count);
        }
        Stmt::Break | Stmt::Continue => {}
    }
}

fn fold_expr(expr: &mut Expr, count: &mut usize) {
    // Post-order: fold children first so parents see folded operands.
    match &mut expr.kind {
        ExprKind::Unary { rhs, .. } => fold_expr(rhs, count),
        ExprKind::Binary { lhs, rhs, .. } => {
            fold_expr(lhs, count);
            fold_expr(rhs, count);
        }
        ExprKind::Call { args, .. } => args.iter_mut().for_each(|a| fold_expr(a, count)),
        ExprKind::Assign { value, .. } => fold_expr(value, count),
        ExprKind::ArrayLit(elems) => elems.iter_mut().for_each(|e| fold_expr(e, count)),
        ExprKind::Index { base, index } => {
            fold_expr(base, count);
            fold_expr(index, count);
        }
        ExprKind::SetIndex { base, index, value } => {
            fold_expr(base, count);
            fold_expr(index, count);
            fold_expr(value, count);
        }
        ExprKind::StructLit(fields) => fields.iter_mut().for_each(|e| fold_expr(e, count)),
        ExprKind::GetField { base, .. } => fold_expr(base, count),
        ExprKind::SetField { base, value, .. } => {
            fold_expr(base, count);
            fold_expr(value, count);
        }
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            fold_expr(cond, count);
            fold_block(then_branch, count);
            if let Some(e) = else_branch {
                fold_expr(e, count);
            }
        }
        ExprKind::Block(block) => fold_block(block, count),
        _ => {}
    }

    // Now attempt to rewrite this node. Take ownership of the kind so children
    // can be moved freely, then either install a replacement or restore it.
    let kind = std::mem::replace(&mut expr.kind, ExprKind::Bool(false));
    match rewrite(kind, expr.ty, expr.span) {
        Ok(replacement) => {
            *expr = replacement;
            *count += 1;
        }
        Err(kind) => expr.kind = kind,
    }
}

/// Attempts to rewrite a single node. `Ok` is the replacement; `Err` returns the
/// original kind unchanged.
fn rewrite(kind: ExprKind, ty: Type, span: crate::span::Span) -> Result<Expr, ExprKind> {
    match kind {
        ExprKind::Unary { op, rhs } => fold_unary(op, rhs, ty, span),
        ExprKind::Binary { op, lhs, rhs } => fold_binary(op, lhs, rhs, ty, span),
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => fold_if(*cond, then_branch, else_branch, ty, span),
        other => Err(other),
    }
}

fn fold_unary(
    op: UnOp,
    rhs: Box<Expr>,
    ty: Type,
    span: crate::span::Span,
) -> Result<Expr, ExprKind> {
    let folded = match (op, &rhs.kind) {
        (UnOp::Neg, ExprKind::Int(v)) => Some(ExprKind::Int(v.wrapping_neg())),
        (UnOp::Neg, ExprKind::Float(v)) => Some(ExprKind::Float(-v)),
        (UnOp::Not, ExprKind::Bool(v)) => Some(ExprKind::Bool(!v)),
        _ => None,
    };
    match folded {
        Some(kind) => Ok(Expr::new(kind, ty, span)),
        None => Err(ExprKind::Unary { op, rhs }),
    }
}

fn fold_binary(
    op: BinOp,
    lhs: Box<Expr>,
    rhs: Box<Expr>,
    ty: Type,
    span: crate::span::Span,
) -> Result<Expr, ExprKind> {
    // 1. Both operands constant: compute the result literal.
    if let Some(kind) = const_binary(op, &lhs.kind, &rhs.kind) {
        return Ok(Expr::new(kind, ty, span));
    }

    // 2. Algebraic identities. Compute predicates up front (each borrow is
    //    released immediately) so the operands can then be moved freely.
    let l_zero = is_zero(&lhs.kind);
    let r_zero = is_zero(&rhs.kind);
    let l_one = is_one(&lhs.kind);
    let r_one = is_one(&rhs.kind);
    let l_bool = as_bool(&lhs.kind);

    match op {
        BinOp::Add => {
            if r_zero {
                return Ok(*lhs);
            }
            if l_zero {
                return Ok(*rhs);
            }
        }
        BinOp::Sub => {
            if r_zero {
                return Ok(*lhs);
            }
        }
        BinOp::Mul => {
            if r_one {
                return Ok(*lhs);
            }
            if l_one {
                return Ok(*rhs);
            }
            // `x * 0` → 0 only if `x` cannot have side effects.
            if r_zero && is_pure(&lhs) {
                return Ok(*rhs);
            }
            if l_zero && is_pure(&rhs) {
                return Ok(*lhs);
            }
        }
        BinOp::And => {
            // `true && x` → x ; `false && x` → false (lhs is a pure literal).
            match l_bool {
                Some(true) => return Ok(*rhs),
                Some(false) => return Ok(*lhs),
                None => {}
            }
        }
        BinOp::Or => {
            // `true || x` → true ; `false || x` → x.
            match l_bool {
                Some(true) => return Ok(*lhs),
                Some(false) => return Ok(*rhs),
                None => {}
            }
        }
        _ => {}
    }
    Err(ExprKind::Binary { op, lhs, rhs })
}

/// Collapses an `if` whose condition folded to a constant.
fn fold_if(
    cond: Expr,
    then_branch: Block,
    else_branch: Option<Box<Expr>>,
    ty: Type,
    span: crate::span::Span,
) -> Result<Expr, ExprKind> {
    let cond_span = cond.span;
    match cond.kind {
        ExprKind::Bool(true) => Ok(Expr::new(ExprKind::Block(then_branch), ty, span)),
        ExprKind::Bool(false) => match else_branch {
            Some(else_expr) => Ok(*else_expr),
            // `if false { … }` with no else is just unit.
            None => Ok(unit_expr(span)),
        },
        // Condition is not constant: rebuild unchanged.
        other => {
            let cond = Box::new(Expr::new(other, Type::Bool, cond_span));
            Err(ExprKind::If {
                cond,
                then_branch,
                else_branch,
            })
        }
    }
}

// ---- constant evaluation ----

/// Folds a binary operator applied to two literal operands, if possible.
fn const_binary(op: BinOp, lhs: &ExprKind, rhs: &ExprKind) -> Option<ExprKind> {
    use ExprKind::{Bool, Float, Int, Str};
    match (lhs, rhs) {
        (Int(a), Int(b)) => const_int(op, *a, *b),
        (Float(a), Float(b)) => const_float(op, *a, *b),
        (Bool(a), Bool(b)) => const_bool(op, *a, *b),
        (Str(a), Str(b)) => match op {
            BinOp::Eq => Some(Bool(a == b)),
            BinOp::Ne => Some(Bool(a != b)),
            _ => None,
        },
        _ => None,
    }
}

fn const_int(op: BinOp, a: i64, b: i64) -> Option<ExprKind> {
    use ExprKind::{Bool, Int};
    Some(match op {
        BinOp::Add => Int(a.wrapping_add(b)),
        BinOp::Sub => Int(a.wrapping_sub(b)),
        BinOp::Mul => Int(a.wrapping_mul(b)),
        // Preserve the runtime error: do not fold division/remainder by zero,
        // nor the single overflowing case `i64::MIN / -1`.
        BinOp::Div => Int(a.checked_div(b)?),
        BinOp::Rem => Int(a.checked_rem(b)?),
        BinOp::Eq => Bool(a == b),
        BinOp::Ne => Bool(a != b),
        BinOp::Lt => Bool(a < b),
        BinOp::Le => Bool(a <= b),
        BinOp::Gt => Bool(a > b),
        BinOp::Ge => Bool(a >= b),
        BinOp::And | BinOp::Or => return None,
    })
}

fn const_float(op: BinOp, a: f64, b: f64) -> Option<ExprKind> {
    use ExprKind::{Bool, Float};
    Some(match op {
        BinOp::Add => Float(a + b),
        BinOp::Sub => Float(a - b),
        BinOp::Mul => Float(a * b),
        BinOp::Div => Float(a / b),
        BinOp::Rem => Float(a % b),
        BinOp::Eq => Bool(a == b),
        BinOp::Ne => Bool(a != b),
        BinOp::Lt => Bool(a < b),
        BinOp::Le => Bool(a <= b),
        BinOp::Gt => Bool(a > b),
        BinOp::Ge => Bool(a >= b),
        BinOp::And | BinOp::Or => return None,
    })
}

fn const_bool(op: BinOp, a: bool, b: bool) -> Option<ExprKind> {
    use ExprKind::Bool;
    Some(match op {
        BinOp::Eq => Bool(a == b),
        BinOp::Ne => Bool(a != b),
        BinOp::And => Bool(a && b),
        BinOp::Or => Bool(a || b),
        _ => return None,
    })
}

// ---- literal predicates ----

fn is_zero(kind: &ExprKind) -> bool {
    matches!(kind, ExprKind::Int(0)) || matches!(kind, ExprKind::Float(f) if *f == 0.0)
}

fn is_one(kind: &ExprKind) -> bool {
    matches!(kind, ExprKind::Int(1)) || matches!(kind, ExprKind::Float(f) if *f == 1.0)
}

fn as_bool(kind: &ExprKind) -> Option<bool> {
    match kind {
        ExprKind::Bool(b) => Some(*b),
        _ => None,
    }
}

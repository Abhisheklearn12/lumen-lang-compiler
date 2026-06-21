//! The optimizer: a small pass manager over the [`Hir`].
//!
//! # Design
//!
//! The optimizer rewrites HIR in place through a fixed sequence of passes,
//! repeated to a fixpoint (bounded by [`OptOptions::max_iterations`]) so that
//! one pass can expose opportunities for another  e.g. constant folding turns
//! a condition into `true`, which dead-code elimination then uses to drop a
//! branch.
//!
//! Three passes are implemented, each justified by measurable value on real
//! programs and each preserving observable behaviour:
//!
//! * [`inline`]  inlines small, pure, non-recursive expression functions so
//!   the other passes can see across the former call boundary.
//! * [`fold`]  constant folding, algebraic simplification, and collapsing
//!   `if`/`while` with a constant condition.
//! * [`dce`]  dead-code elimination: unreachable code after `return`,
//!   never-taken `while false`, and unused pure `let` bindings.
//!
//! Every transformation is guarded for **soundness**: code with side effects
//! (calls, assignments) is never dropped. The shared [`is_pure`] predicate is
//! the single place that decides what is safe to remove.
//!
//! All passes are deterministic, so optimized output is reproducible and
//! snapshot-testable.

pub mod dce;
pub mod fold;
pub mod inline;

use crate::hir::{Block, Expr, ExprKind, Hir, Stmt};
use crate::sema::types::Type;
use crate::span::Span;

/// Tuning knobs for the optimizer.
#[derive(Debug, Clone, Copy)]
pub struct OptOptions {
    /// Whether to run any passes at all (`-O0` turns this off).
    pub enabled: bool,
    /// Maximum fixpoint iterations before stopping.
    pub max_iterations: usize,
}

impl Default for OptOptions {
    fn default() -> OptOptions {
        OptOptions {
            enabled: true,
            max_iterations: 8,
        }
    }
}

/// Per-run statistics, surfaced through tracing and the driver's `--stats`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct OptStats {
    /// Constant folds / algebraic simplifications applied.
    pub folded: usize,
    /// Dead statements or branches removed.
    pub eliminated: usize,
    /// Call sites replaced by an inlined function body.
    pub inlined: usize,
    /// Fixpoint iterations performed.
    pub iterations: usize,
}

impl OptStats {
    /// Total rewrites across all passes.
    pub fn total(&self) -> usize {
        self.folded + self.eliminated + self.inlined
    }
}

/// Optimizes `hir` in place, returning what was done.
#[tracing::instrument(level = "debug", skip_all)]
pub fn optimize(hir: &mut Hir, options: OptOptions) -> OptStats {
    let mut stats = OptStats::default();
    if !options.enabled {
        return stats;
    }
    for iteration in 0..options.max_iterations {
        // Inline first so folding and DCE can see across the former call.
        let inlined = inline::run(hir);
        let folded = fold::run(hir);
        let eliminated = dce::run(hir);
        stats.inlined += inlined;
        stats.folded += folded;
        stats.eliminated += eliminated;
        stats.iterations = iteration + 1;
        tracing::debug!(
            iteration,
            inlined,
            folded,
            eliminated,
            "optimizer iteration"
        );
        if inlined + folded + eliminated == 0 {
            // Reached a fixpoint: nothing changed this round.
            break;
        }
    }
    tracing::debug!(?stats, "optimization complete");
    stats
}

// ---- shared helpers used by more than one pass ----

/// An expression that evaluates to the unit value with no side effects, used to
/// replace removed unit-typed code.
pub(crate) fn unit_expr(span: Span) -> Expr {
    Expr::new(
        ExprKind::Block(Block {
            stmts: Vec::new(),
            tail: None,
            ty: Type::Unit,
        }),
        Type::Unit,
        span,
    )
}

/// Whether evaluating `expr` has no observable side effect and so may be
/// duplicated or removed.
///
/// Conservative by construction: anything that could call a function, assign a
/// variable, or otherwise affect the world is impure. This is the single
/// soundness gate for every removal the optimizer performs.
pub(crate) fn is_pure(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Str(_)
        | ExprKind::Local(_) => true,
        ExprKind::Unary { rhs, .. } => is_pure(rhs),
        ExprKind::Binary { lhs, rhs, .. } => is_pure(lhs) && is_pure(rhs),
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            is_pure(cond)
                && block_is_pure(then_branch)
                && else_branch.as_ref().is_none_or(|e| is_pure(e))
        }
        ExprKind::Block(block) => block_is_pure(block),
        // Constructing an array/struct and reading an element/field are
        // side-effect-free.
        ExprKind::ArrayLit(elems) => elems.iter().all(is_pure),
        ExprKind::StructLit(fields) => fields.iter().all(is_pure),
        ExprKind::Index { base, index } => is_pure(base) && is_pure(index),
        ExprKind::GetField { base, .. } => is_pure(base),
        // Calls may perform I/O; assignments and stores mutate state.
        ExprKind::Call { .. }
        | ExprKind::Assign { .. }
        | ExprKind::SetIndex { .. }
        | ExprKind::SetField { .. } => false,
    }
}

/// Whether a block can be removed wholesale: every statement and its tail are
/// pure. A `return` makes a block impure (it transfers control).
pub(crate) fn block_is_pure(block: &Block) -> bool {
    block.stmts.iter().all(stmt_is_pure) && block.tail.as_ref().is_none_or(|t| is_pure(t))
}

fn stmt_is_pure(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Let { value, .. } => is_pure(value),
        Stmt::Expr(e) => is_pure(e),
        // Control-flow effects: conservatively impure.
        Stmt::Return(_) | Stmt::While { .. } | Stmt::For { .. } | Stmt::Break | Stmt::Continue => {
            false
        }
    }
}

#[cfg(test)]
mod tests;

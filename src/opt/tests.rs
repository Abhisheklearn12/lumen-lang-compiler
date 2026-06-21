//! Unit tests for the optimizer.

use super::*;
use crate::hir::{ExprKind, Stmt, lower, print_hir};
use crate::lexer::tokenize;
use crate::parser::parse;
use crate::sema::{check, resolve};
use crate::source::SourceFile;

/// Lowers `src` (asserting it is error-free) without optimizing.
fn lower_src(src: &str) -> Hir {
    let file = SourceFile::new("test.lm", src);
    let mut diags = crate::diagnostics::Diagnostics::new();
    let tokens = tokenize(&file, &mut diags);
    let ast = parse(tokens, &mut diags);
    let res = resolve(&ast, &mut diags);
    let tc = check(&ast, &res, &mut diags);
    assert!(
        !diags.has_errors(),
        "unexpected errors:\n{}",
        diags.render_all(&file)
    );
    lower(&ast, &res, &tc)
}

/// Lowers and fully optimizes `src`, returning the HIR and the stats.
fn optimized(src: &str) -> (Hir, OptStats) {
    let mut hir = lower_src(src);
    let stats = optimize(&mut hir, OptOptions::default());
    (hir, stats)
}

/// The function named `name` from a program.
fn func<'a>(hir: &'a Hir, name: &str) -> &'a crate::hir::Function {
    hir.functions.iter().find(|f| f.name == name).unwrap()
}

#[test]
fn folds_constant_arithmetic() {
    let (hir, stats) = optimized("fn main() { let x = 1 + 2 * 3; print_int(x); }");
    let main = func(&hir, "main");
    // `1 + 2 * 3` → `7`.
    let Stmt::Let { value, .. } = &main.body.stmts[0] else {
        panic!()
    };
    assert!(
        matches!(value.kind, ExprKind::Int(7)),
        "got {:?}",
        value.kind
    );
    assert!(stats.folded >= 1);
}

#[test]
fn folds_comparisons_and_logic() {
    let (hir, _) = optimized("fn main() { let b = (1 < 2) && (3 == 3); print_bool(b); }");
    let Stmt::Let { value, .. } = &func(&hir, "main").body.stmts[0] else {
        panic!()
    };
    assert!(matches!(value.kind, ExprKind::Bool(true)));
}

#[test]
fn does_not_fold_division_by_zero() {
    // Must be preserved so the VM raises the runtime error.
    let (hir, _) = optimized("fn main() { let x = 1 / 0; print_int(x); }");
    let Stmt::Let { value, .. } = &func(&hir, "main").body.stmts[0] else {
        panic!()
    };
    assert!(
        matches!(value.kind, ExprKind::Binary { .. }),
        "division by zero was folded"
    );
}

#[test]
fn algebraic_identities() {
    let (hir, _) = optimized("fn f(x: i64) -> i64 { x + 0 } fn main() {}");
    let tail = func(&hir, "f").body.tail.as_ref().unwrap();
    // `x + 0` → `x` (a local read).
    assert!(matches!(tail.kind, ExprKind::Local(_)));
}

#[test]
fn multiply_by_zero_only_when_pure() {
    // Pure operand: `x * 0` → `0`.
    let (hir, _) = optimized("fn f(x: i64) -> i64 { x * 0 } fn main() {}");
    assert!(matches!(
        func(&hir, "f").body.tail.as_ref().unwrap().kind,
        ExprKind::Int(0)
    ));

    // Impure operand (a call): must NOT collapse, the call has to run.
    let (hir, _) = optimized(
        "fn side() -> i64 { print_int(1); 5 } fn main() { let y = side() * 0; print_int(y); }",
    );
    let Stmt::Let { value, .. } = &func(&hir, "main").body.stmts[0] else {
        panic!()
    };
    assert!(
        matches!(value.kind, ExprKind::Binary { .. }),
        "impure multiply was collapsed"
    );
}

#[test]
fn collapses_constant_if() {
    let (hir, _) = optimized("fn f() -> i64 { if true { 1 } else { 2 } } fn main() {}");
    // The whole `if` collapses to the then-block, whose value is `1`.
    let tail = func(&hir, "f").body.tail.as_ref().unwrap();
    match &tail.kind {
        ExprKind::Block(b) => assert!(matches!(b.tail.as_ref().unwrap().kind, ExprKind::Int(1))),
        ExprKind::Int(1) => {}
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn removes_code_after_return() {
    let (hir, stats) =
        optimized("fn f() -> i64 { return 1; let dead = 2; print_int(dead); 3 } fn main() {}");
    let body = &func(&hir, "f").body;
    // Only the `return` statement remains.
    assert_eq!(body.stmts.len(), 1);
    assert!(matches!(body.stmts[0], Stmt::Return(_)));
    assert!(body.tail.is_none());
    assert!(stats.eliminated >= 1);
}

#[test]
fn removes_while_false() {
    let (hir, _) = optimized("fn main() { while false { print_int(1); } }");
    assert!(func(&hir, "main").body.stmts.is_empty());
}

#[test]
fn removes_unused_pure_let() {
    let (hir, _) = optimized("fn main() { let unused = 1 + 2; }");
    // The binding is never read and its initialiser is pure: gone.
    assert!(func(&hir, "main").body.stmts.is_empty());
}

#[test]
fn keeps_unused_impure_let() {
    // The initialiser calls a builtin; even though `x` is unused, the call must
    // remain.
    let (hir, _) = optimized("fn main() { let x = { print_int(1); 0 }; }");
    assert_eq!(func(&hir, "main").body.stmts.len(), 1);
}

#[test]
fn disabled_optimizer_changes_nothing() {
    let mut hir = lower_src("fn main() { let x = 1 + 2; print_int(x); }");
    let before = print_hir(&hir);
    let stats = optimize(
        &mut hir,
        OptOptions {
            enabled: false,
            max_iterations: 8,
        },
    );
    assert_eq!(stats, OptStats::default());
    assert_eq!(print_hir(&hir), before);
}

#[test]
fn reaches_a_fixpoint() {
    // `if (1 < 2) { ... }` requires folding the condition first, then the if.
    let (_hir, stats) = optimized("fn f() -> i64 { if 1 < 2 { 10 + 5 } else { 0 } } fn main() {}");
    // Converged in fewer than the max iterations.
    assert!(stats.iterations < OptOptions::default().max_iterations);
    assert!(stats.total() >= 2);
}

#[test]
fn purity_predicate() {
    let hir = lower_src("fn main() { let x = 1; print_int(x); }");
    let main = func(&hir, "main");
    // `let x = 1` initialiser is pure.
    let Stmt::Let { value, .. } = &main.body.stmts[0] else {
        panic!()
    };
    assert!(is_pure(value));
    // `print_int(x)` is impure.
    let Stmt::Expr(call) = &main.body.stmts[1] else {
        panic!()
    };
    assert!(!is_pure(call));
}

// ---- inlining ----

/// Whether any expression in the program calls a user function (not a builtin).
fn has_user_call(hir: &Hir) -> bool {
    use crate::hir::Callee;
    fn in_expr(e: &crate::hir::Expr) -> bool {
        match &e.kind {
            ExprKind::Call { callee, args } => {
                matches!(callee, Callee::Fn(_)) || args.iter().any(in_expr)
            }
            ExprKind::Unary { rhs, .. } => in_expr(rhs),
            ExprKind::Binary { lhs, rhs, .. } => in_expr(lhs) || in_expr(rhs),
            ExprKind::Assign { value, .. } => in_expr(value),
            ExprKind::ArrayLit(es) | ExprKind::StructLit(es) => es.iter().any(in_expr),
            ExprKind::Index { base, index } => in_expr(base) || in_expr(index),
            ExprKind::GetField { base, .. } => in_expr(base),
            ExprKind::SetIndex { base, index, value } => {
                in_expr(base) || in_expr(index) || in_expr(value)
            }
            ExprKind::SetField { base, value, .. } => in_expr(base) || in_expr(value),
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                in_expr(cond)
                    || in_block(then_branch)
                    || else_branch.as_deref().is_some_and(in_expr)
            }
            ExprKind::Block(b) => in_block(b),
            _ => false,
        }
    }
    fn in_block(b: &crate::hir::Block) -> bool {
        b.stmts.iter().any(in_stmt) || b.tail.as_deref().is_some_and(in_expr)
    }
    fn in_stmt(s: &Stmt) -> bool {
        match s {
            Stmt::Let { value, .. } => in_expr(value),
            Stmt::Expr(e) => in_expr(e),
            Stmt::Return(Some(e)) => in_expr(e),
            Stmt::While { cond, body } => in_expr(cond) || in_block(body),
            Stmt::For {
                start, end, body, ..
            } => in_expr(start) || in_expr(end) || in_block(body),
            _ => false,
        }
    }
    hir.functions.iter().any(|f| in_block(&f.body))
}

/// Output of running `src` with optimization on or off; the two must agree.
fn run_output(src: &str, optimize_on: bool) -> String {
    let mut hir = lower_src(src);
    optimize(
        &mut hir,
        OptOptions {
            enabled: optimize_on,
            ..OptOptions::default()
        },
    );
    let program = crate::backend::generate(&hir);
    crate::backend::execute(&program).unwrap().stdout
}

#[test]
fn inlines_a_simple_expression_function() {
    let (hir, stats) = optimized("fn sq(x: i64) -> i64 { x * x } fn main() { print_int(sq(7)); }");
    assert!(stats.inlined >= 1, "expected an inline, stats: {stats:?}");
    // The only remaining call is the `print_int` builtin; `sq` is gone.
    assert!(
        !has_user_call(&hir),
        "a user call survived inlining:\n{}",
        print_hir(&hir)
    );
}

#[test]
fn inlined_program_behaves_identically() {
    let src = "fn add(a: i64, b: i64) -> i64 { a + b }\n\
               fn max(a: i64, b: i64) -> i64 { if a > b { a } else { b } }\n\
               fn main() { print_int(add(max(3, 9), 100)); }";
    assert_eq!(run_output(src, false), run_output(src, true));
    assert_eq!(run_output(src, true).trim(), "109");
}

#[test]
fn does_not_inline_recursive_functions() {
    let src = "fn fib(n: i64) -> i64 { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }\n\
               fn main() { print_int(fib(8)); }";
    let (_hir, stats) = optimized(src);
    assert_eq!(stats.inlined, 0, "recursive function must not be inlined");
    assert_eq!(run_output(src, false), run_output(src, true));
}

#[test]
fn arguments_with_effects_run_once_after_inlining() {
    // `bump` mutates the array; if inlining duplicated the argument it would run
    // twice and the totals would differ from the unoptimized run.
    let src = "fn twice(x: i64) -> i64 { x + x }\n\
               fn main() {\n\
                 let mut a = [0];\n\
                 a[0] = a[0] + 1;\n\
                 print_int(twice(a[0]));\n\
               }";
    assert_eq!(run_output(src, false), run_output(src, true));
}

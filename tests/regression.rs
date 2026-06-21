//! Regression tests.
//!
//! Each test pins down a specific bug found while building the compiler, so it
//! can never silently come back. The comment on each names the original defect.

mod common;

use common::{compile_errors, stdout};

/// A function whose body is a bare `return` was reported *twice*: once for the
/// explicit return and once for a non-existent fall-through. Divergence
/// analysis must suppress the fall-through check.
#[test]
fn explicit_return_reports_a_single_error() {
    assert_eq!(
        compile_errors("fn f() -> i64 { return true; } fn main() {}"),
        vec!["E0303"]
    );
}

/// An unresolved name inside an arithmetic expression used to also trigger a
/// spurious "invalid operands" error. The error type must absorb follow-on
/// checks so only the root cause is reported.
#[test]
fn error_type_does_not_cascade() {
    assert_eq!(
        compile_errors("fn main() { let x = missing + 1; }"),
        vec!["E0200"]
    );
}

/// `let n = n;` with no outer `n` must fail to resolve the initialiser; an early
/// version brought the binding into scope too soon and accepted it.
#[test]
fn let_initialiser_cannot_reference_itself() {
    assert_eq!(compile_errors("fn main() { let n = n; }"), vec!["E0200"]);
}

/// Right-associative exponent-style nesting and left-associative subtraction
/// must evaluate with the correct grouping. `1 - 2 - 3` is `(1 - 2) - 3 = -4`,
/// not `1 - (2 - 3) = 2`.
#[test]
fn subtraction_is_left_associative_at_runtime() {
    assert_eq!(stdout("fn main() { print_int(1 - 2 - 3); }"), "-4\n");
}

/// `x * 0` must not be folded to `0` when `x` has side effects: the call has to
/// run. Regression for an over-eager algebraic simplification.
#[test]
fn multiply_by_zero_keeps_side_effects() {
    let out = stdout(
        "fn noisy() -> i64 { print_int(42); 7 }\n\
         fn main() { let z = noisy() * 0; print_int(z); }",
    );
    // 42 from the call, then 0 from the result.
    assert_eq!(out, "42\n0\n");
}

/// Constant folding must preserve a division-by-zero so the runtime error still
/// occurs, rather than folding it away at compile time.
#[test]
fn constant_division_by_zero_is_not_folded_away() {
    use common::{Outcome, run};
    match run("fn main() { print_int(1 / 0); }") {
        Outcome::RuntimeError(err) => assert_eq!(err.to_string(), "division by zero"),
        other => panic!("expected a runtime error, got {other:?}"),
    }
}

/// `if` without `else` yields unit, and the then-branch value is discarded; a
/// `while` body's value is likewise discarded. Both must leave the operand
/// stack balanced so later statements execute correctly.
#[test]
fn statement_position_values_are_discarded() {
    let out = stdout(
        "fn main() {\n\
        \x20   let mut i = 0;\n\
        \x20   while i < 3 { i = i + 1; }\n\
        \x20   if true { print_int(i); }\n\
        \x20   print_int(99);\n\
         }",
    );
    assert_eq!(out, "3\n99\n");
}

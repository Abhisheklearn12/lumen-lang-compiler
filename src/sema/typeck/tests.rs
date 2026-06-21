//! Unit tests for the type checker.

use super::*;
use crate::lexer::tokenize;
use crate::parser::parse;
use crate::sema::resolve::resolve;
use crate::source::SourceFile;

/// Runs the full front-end over `src` and returns the type tables and the codes
/// of any diagnostics produced (from resolution and type checking).
fn check_src(src: &str) -> (Typeck, Vec<DiagCode>) {
    let file = SourceFile::new("test.lm", src);
    let mut diags = Diagnostics::new();
    let tokens = tokenize(&file, &mut diags);
    let ast = parse(tokens, &mut diags);
    assert!(
        !diags.has_errors(),
        "unexpected parse errors:\n{}",
        diags.render_all(&file)
    );
    let res = resolve(&ast, &mut diags);
    let tc = check(&ast, &res, &mut diags);
    let codes = diags.items().iter().map(|d| d.code).collect();
    (tc, codes)
}

/// Asserts `src` type-checks with no diagnostics at all.
fn assert_ok(src: &str) {
    let (_tc, codes) = check_src(src);
    assert!(codes.is_empty(), "expected no errors, got {codes:?}");
}

/// Asserts `src` produces exactly the given diagnostic codes (in any order).
fn assert_codes(src: &str, expected: &[DiagCode]) {
    let (_tc, mut codes) = check_src(src);
    let mut want = expected.to_vec();
    codes.sort_by_key(|c| c.as_str());
    want.sort_by_key(|c| c.as_str());
    assert_eq!(codes, want);
}

#[test]
fn well_typed_program() {
    assert_ok(
        "fn add(a: i64, b: i64) -> i64 { a + b }\n\
         fn main() { let x = add(1, 2); print_int(x); }",
    );
}

#[test]
fn let_inference_records_type() {
    let (tc, codes) = check_src("fn main() { let x = 1 + 2; print_int(x); }");
    assert!(codes.is_empty());
    // The initialiser `1 + 2` is typed Int.
    assert!(tc.expr_types.values().any(|&t| t == Type::Int));
}

#[test]
fn let_annotation_mismatch() {
    assert_codes(
        "fn main() { let x: i64 = true; }",
        &[DiagCode::TypeMismatch],
    );
}

#[test]
fn return_type_mismatch() {
    assert_codes(
        "fn f() -> i64 { return true; } fn main() {}",
        &[DiagCode::ReturnTypeMismatch],
    );
}

#[test]
fn body_tail_must_match_return_type() {
    assert_codes(
        "fn f() -> i64 { true } fn main() {}",
        &[DiagCode::ReturnTypeMismatch],
    );
}

#[test]
fn implicit_unit_return_is_ok() {
    assert_ok("fn f() { let x = 1; } fn main() { f(); }");
}

#[test]
fn falling_off_a_value_returning_function() {
    assert_codes(
        "fn f() -> i64 { let x = 1; } fn main() {}",
        &[DiagCode::MissingReturn],
    );
}

#[test]
fn all_paths_return_is_ok() {
    // No trailing expression, but both arms of the `if` return: well-typed.
    assert_ok(
        "fn f(n: i64) -> i64 { if n < 0 { return 0; } else { return n; } }\n\
         fn main() { print_int(f(3)); }",
    );
}

#[test]
fn explicit_return_only_reports_one_error() {
    // The `return true` is the single error; the fall-through check must not
    // fire a second time because the body diverges.
    assert_codes(
        "fn f() -> i64 { return true; } fn main() {}",
        &[DiagCode::ReturnTypeMismatch],
    );
}

#[test]
fn arity_mismatch() {
    assert_codes(
        "fn g(a: i64) {} fn main() { g(1, 2); }",
        &[DiagCode::ArityMismatch],
    );
}

#[test]
fn argument_type_mismatch() {
    assert_codes(
        "fn g(a: i64) {} fn main() { g(true); }",
        &[DiagCode::TypeMismatch],
    );
}

#[test]
fn non_bool_if_condition() {
    assert_codes("fn main() { if 1 { } }", &[DiagCode::NonBoolCondition]);
}

#[test]
fn non_bool_while_condition() {
    assert_codes("fn main() { while 1 { } }", &[DiagCode::NonBoolCondition]);
}

#[test]
fn mixed_numeric_operands_rejected() {
    // i64 + f64 is not allowed (no implicit conversion).
    assert_codes(
        "fn main() { let x = 1 + 1.0; }",
        &[DiagCode::InvalidOperands],
    );
}

#[test]
fn arithmetic_on_bool_rejected() {
    assert_codes(
        "fn main() { let x = true + false; }",
        &[DiagCode::InvalidOperands],
    );
}

#[test]
fn comparison_yields_bool() {
    assert_ok("fn main() { let b: bool = 1 < 2; }");
}

#[test]
fn equality_on_strings() {
    assert_ok(r#"fn main() { let b: bool = "a" == "b"; }"#);
}

#[test]
fn logical_requires_bool() {
    assert_codes(
        "fn main() { let x = 1 && true; }",
        &[DiagCode::InvalidOperands],
    );
}

#[test]
fn assign_to_immutable() {
    assert_codes(
        "fn main() { let x = 1; x = 2; }",
        &[DiagCode::AssignToImmutable],
    );
}

#[test]
fn assign_respects_mutability_and_type() {
    assert_ok("fn main() { let mut x = 1; x = 2; }");
    assert_codes(
        "fn main() { let mut x = 1; x = true; }",
        &[DiagCode::TypeMismatch],
    );
}

#[test]
fn if_branches_must_agree() {
    assert_codes(
        "fn main() { let x = if true { 1 } else { false }; }",
        &[DiagCode::IfBranchMismatch],
    );
}

#[test]
fn if_branches_agreeing_is_ok() {
    assert_ok("fn main() { let x: i64 = if true { 1 } else { 2 }; print_int(x); }");
}

#[test]
fn if_without_else_must_be_unit() {
    assert_codes(
        "fn main() { let x = if true { 1 }; }",
        &[DiagCode::IfBranchMismatch],
    );
}

#[test]
fn unknown_type_name() {
    assert_codes("fn f(x: i32) {} fn main() {}", &[DiagCode::UnknownType]);
}

#[test]
fn function_used_as_value() {
    assert_codes(
        "fn helper() {} fn main() { let x = helper; }",
        &[DiagCode::TypeMismatch],
    );
}

#[test]
fn calling_a_local_is_not_callable() {
    assert_codes("fn main() { let x = 1; x(); }", &[DiagCode::NotCallable]);
}

#[test]
fn missing_main() {
    assert_codes("fn f() {}", &[DiagCode::BadMain]);
}

#[test]
fn main_with_params_is_invalid() {
    assert_codes("fn main(x: i64) {}", &[DiagCode::BadMain]);
}

#[test]
fn recursion_type_checks() {
    assert_ok(
        "fn fib(n: i64) -> i64 { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }\n\
         fn main() { print_int(fib(10)); }",
    );
}

#[test]
fn error_type_suppresses_cascades() {
    // `unknown` is undefined: one resolution error, and the surrounding
    // arithmetic should NOT add a spurious operand error.
    let (_tc, codes) = check_src("fn main() { let x = unknown + 1; }");
    assert_eq!(codes, vec![DiagCode::UnresolvedName]);
}

#[test]
fn for_loop_is_well_typed() {
    assert_ok("fn main() { let mut s = 0; for i in 0..10 { s += i; } print_int(s); }");
}

#[test]
fn for_range_bounds_must_be_int() {
    assert_codes(
        "fn main() { for i in 0..true { } }",
        &[DiagCode::TypeMismatch],
    );
}

#[test]
fn break_outside_loop_is_rejected() {
    assert_codes("fn main() { break; }", &[DiagCode::BreakOutsideLoop]);
}

#[test]
fn continue_outside_loop_is_rejected() {
    assert_codes("fn main() { continue; }", &[DiagCode::BreakOutsideLoop]);
}

#[test]
fn break_inside_loop_is_ok() {
    assert_ok("fn main() { while true { break; } }");
    assert_ok("fn main() { for i in 0..5 { if i == 2 { continue; } } }");
}

#[test]
fn compound_assignment_requires_mutable() {
    assert_codes(
        "fn main() { let x = 1; x += 1; }",
        &[DiagCode::AssignToImmutable],
    );
    assert_ok("fn main() { let mut x = 1; x += 1; print_int(x); }");
}

#[test]
fn compound_assignment_type_checks_operands() {
    assert_codes(
        "fn main() { let mut x = 1; x += true; }",
        &[DiagCode::InvalidOperands],
    );
}

#[test]
fn constants_are_well_typed() {
    assert_ok("const N: i64 = 5; fn main() { print_int(N); }");
    assert_ok("const A: i64 = 2; const B: i64 = A * 3; fn main() { print_int(B); }");
}

#[test]
fn const_type_mismatch_is_reported() {
    assert_codes(
        "const N: i64 = true; fn main() {}",
        &[DiagCode::TypeMismatch],
    );
}

#[test]
fn non_constant_initialiser_is_reported() {
    assert_codes(
        "fn f() -> i64 { 1 } const N: i64 = f(); fn main() {}",
        &[DiagCode::NotConstant],
    );
}

#[test]
fn duplicate_global_names_are_reported() {
    assert_codes(
        "const X: i64 = 1; fn X() {} fn main() {}",
        &[DiagCode::DuplicateDefinition],
    );
}

#[test]
fn constant_used_as_call_target_is_not_callable() {
    assert_codes(
        "const N: i64 = 1; fn main() { N(); }",
        &[DiagCode::NotCallable],
    );
}

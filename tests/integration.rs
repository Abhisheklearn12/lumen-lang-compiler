//! Integration tests: complete programs compiled and executed through the
//! public API, covering success cases, failure cases, and edge cases.

mod common;

use common::{Outcome, compile_errors, run, stdout};

// ---------------------------------------------------------------------------
// Success cases
// ---------------------------------------------------------------------------

#[test]
fn hello_world() {
    assert_eq!(
        stdout(r#"fn main() { print_str("Hello, world!"); }"#),
        "Hello, world!\n"
    );
}

#[test]
fn arithmetic_and_precedence() {
    assert_eq!(stdout("fn main() { print_int(2 + 3 * 4 - 1); }"), "13\n");
    assert_eq!(stdout("fn main() { print_int((2 + 3) * 4); }"), "20\n");
    assert_eq!(stdout("fn main() { print_int(17 % 5); }"), "2\n");
}

#[test]
fn factorial_with_recursion() {
    let out = stdout(
        "fn fact(n: i64) -> i64 { if n <= 1 { 1 } else { n * fact(n - 1) } }\n\
         fn main() { print_int(fact(5)); }",
    );
    assert_eq!(out, "120\n");
}

#[test]
fn iterative_sum_with_while() {
    let out = stdout(
        "fn main() {\n\
        \x20   let mut total = 0;\n\
        \x20   let mut n = 1;\n\
        \x20   while n <= 100 { total = total + n; n = n + 1; }\n\
        \x20   print_int(total);\n\
         }",
    );
    assert_eq!(out, "5050\n");
}

#[test]
fn mutual_recursion_even_odd() {
    let out = stdout(
        "fn is_even(n: i64) -> bool { if n == 0 { true } else { is_odd(n - 1) } }\n\
         fn is_odd(n: i64) -> bool { if n == 0 { false } else { is_even(n - 1) } }\n\
         fn main() { print_bool(is_even(10)); print_bool(is_odd(7)); }",
    );
    assert_eq!(out, "true\ntrue\n");
}

#[test]
fn nested_blocks_and_shadowing() {
    let out = stdout(
        "fn main() {\n\
        \x20   let x = 1;\n\
        \x20   let y = { let x = 10; x + 5 };\n\
        \x20   print_int(x);\n\
        \x20   print_int(y);\n\
         }",
    );
    assert_eq!(out, "1\n15\n");
}

#[test]
fn float_and_bool_values() {
    let out = stdout(
        "fn main() {\n\
        \x20   print_float(3.0 / 2.0);\n\
        \x20   print_bool(1.5 < 2.5);\n\
        \x20   print_bool(!false);\n\
         }",
    );
    assert_eq!(out, "1.5\ntrue\ntrue\n");
}

#[test]
fn early_return() {
    let out = stdout(
        "fn first_positive(a: i64, b: i64) -> i64 {\n\
        \x20   if a > 0 { return a; }\n\
        \x20   b\n\
         }\n\
         fn main() { print_int(first_positive(0 - 3, 7)); print_int(first_positive(5, 7)); }",
    );
    assert_eq!(out, "7\n5\n");
}

#[test]
fn empty_main_produces_no_output() {
    assert_eq!(stdout("fn main() {}"), "");
}

#[test]
fn optimization_preserves_behaviour() {
    // The same program with and without optimization must print the same thing.
    let src = "fn main() {\n\
               \x20   let a = 2 * 3 + 4;\n\
               \x20   let mut s = 0;\n\
               \x20   while false { s = s + 1; }\n\
               \x20   print_int(a + s);\n\
               }";
    let opt = match common::run_with(src, true) {
        Outcome::Ok(o) => o,
        other => panic!("{other:?}"),
    };
    let unopt = match common::run_with(src, false) {
        Outcome::Ok(o) => o,
        other => panic!("{other:?}"),
    };
    assert_eq!(opt, "10\n");
    assert_eq!(opt, unopt);
}

// ---------------------------------------------------------------------------
// Failure cases (compile-time)
// ---------------------------------------------------------------------------

#[test]
fn type_error_is_reported() {
    assert_eq!(
        compile_errors("fn main() { let x: bool = 1; }"),
        vec!["E0300"]
    );
}

#[test]
fn undefined_variable_is_reported() {
    assert_eq!(
        compile_errors("fn main() { print_int(nope); }"),
        vec!["E0200"]
    );
}

#[test]
fn missing_main_is_reported() {
    assert_eq!(compile_errors("fn helper() {}"), vec!["E0308"]);
}

#[test]
fn arity_error_is_reported() {
    assert_eq!(
        compile_errors("fn f(a: i64) {} fn main() { f(1, 2, 3); }"),
        vec!["E0301"],
    );
}

#[test]
fn multiple_independent_errors_are_all_reported() {
    // A syntax error in one function must not hide a type error in another.
    let codes = compile_errors(
        "fn broken( { }\n\
         fn main() { let x: i64 = true; }",
    );
    assert!(
        codes.contains(&"E0100".to_string()),
        "missing parse error: {codes:?}"
    );
    assert!(
        codes.contains(&"E0300".to_string()),
        "missing type error: {codes:?}"
    );
}

// ---------------------------------------------------------------------------
// Failure cases (runtime)
// ---------------------------------------------------------------------------

#[test]
fn runtime_division_by_zero() {
    match run("fn main() { let d = 0; print_int(10 / d); }") {
        Outcome::RuntimeError(err) => assert_eq!(err.to_string(), "division by zero"),
        other => panic!("expected runtime error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn deeply_nested_arithmetic_does_not_overflow_the_compiler() {
    // Build `1 + 1 + 1 + ... ` with 200 terms; should compile and evaluate.
    let mut src = String::from("fn main() { print_int(1");
    for _ in 0..199 {
        src.push_str(" + 1");
    }
    src.push_str("); }");
    assert_eq!(stdout(&src), "200\n");
}

#[test]
fn integer_wrapping_matches_runtime_and_const_folding() {
    // i64::MAX + 1 wraps to i64::MIN under both the folder and the VM.
    let src = "fn main() { print_int(9223372036854775807 + 1); }";
    assert_eq!(stdout(src), "-9223372036854775808\n");
}

#[test]
fn unit_returning_functions_compose() {
    let out = stdout(
        "fn greet() { print_str(\"hi\"); }\n\
         fn main() { greet(); greet(); }",
    );
    assert_eq!(out, "hi\nhi\n");
}

//! End-to-end tests for code generation and the VM: compile real programs and
//! check what they print (and what errors they raise).

use super::*;
use crate::diagnostics::Diagnostics;
use crate::hir::lower;
use crate::lexer::tokenize;
use crate::opt::{OptOptions, optimize};
use crate::parser::parse;
use crate::sema::{check, resolve};
use crate::source::SourceFile;

/// Compiles `src` to bytecode. `opt` selects whether the optimizer runs.
fn compile(src: &str, opt: bool) -> Program {
    let file = SourceFile::new("test.lm", src);
    let mut diags = Diagnostics::new();
    let tokens = tokenize(&file, &mut diags);
    let ast = parse(tokens, &mut diags);
    let res = resolve(&ast, &mut diags);
    let tc = check(&ast, &res, &mut diags);
    assert!(
        !diags.has_errors(),
        "unexpected compile errors:\n{}",
        diags.render_all(&file)
    );
    let mut hir = lower(&ast, &res, &tc);
    optimize(
        &mut hir,
        OptOptions {
            enabled: opt,
            max_iterations: 8,
        },
    );
    generate(&hir)
}

/// Compiles and runs `src`, returning captured stdout. Asserts success.
fn run(src: &str) -> String {
    let program = compile(src, true);
    execute(&program)
        .expect("program should run without error")
        .stdout
}

/// Compiles and runs `src` expecting a runtime error.
fn run_err(src: &str) -> VmError {
    let program = compile(src, true);
    execute(&program).expect_err("program should fail")
}

#[test]
fn prints_integer_arithmetic() {
    assert_eq!(run("fn main() { print_int(1 + 2 * 3); }"), "7\n");
}

#[test]
fn variables_and_mutation() {
    let out = run("fn main() { let mut x = 10; x = x + 5; print_int(x); }");
    assert_eq!(out, "15\n");
}

#[test]
fn while_loop_sums() {
    let out = run("fn main() {\n\
        \x20   let mut i = 1;\n\
        \x20   let mut sum = 0;\n\
        \x20   while i <= 5 { sum = sum + i; i = i + 1; }\n\
        \x20   print_int(sum);\n\
         }");
    assert_eq!(out, "15\n"); // 1+2+3+4+5
}

#[test]
fn recursion_fibonacci() {
    let out = run(
        "fn fib(n: i64) -> i64 { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }\n\
         fn main() { print_int(fib(10)); }",
    );
    assert_eq!(out, "55\n");
}

#[test]
fn functions_with_several_arguments() {
    let out = run("fn add3(a: i64, b: i64, c: i64) -> i64 { a + b + c }\n\
         fn main() { print_int(add3(1, 20, 300)); }");
    assert_eq!(out, "321\n");
}

#[test]
fn if_expression_value() {
    let out = run(
        "fn classify(n: i64) -> i64 { if n < 0 { 0 - 1 } else if n == 0 { 0 } else { 1 } }\n\
         fn main() { print_int(classify(0 - 5)); print_int(classify(0)); print_int(classify(9)); }",
    );
    assert_eq!(out, "-1\n0\n1\n");
}

#[test]
fn strings_and_equality() {
    let out =
        run(r#"fn main() { print_str("hello"); print_bool("a" == "a"); print_bool("a" == "b"); }"#);
    assert_eq!(out, "hello\ntrue\nfalse\n");
}

#[test]
fn float_arithmetic() {
    let out = run("fn main() { print_float(1.5 + 2.25); }");
    assert_eq!(out, "3.75\n");
}

#[test]
fn boolean_logic_short_circuits() {
    // `side` prints 999 and returns true. Short-circuit must skip it.
    let src = "fn side() -> bool { print_int(999); true }\n\
               fn main() {\n\
               \x20   print_bool(false && side());\n\
               \x20   print_bool(true || side());\n\
               }";
    // No 999 should appear: both `side()` calls are short-circuited away.
    assert_eq!(run(src), "false\ntrue\n");
}

#[test]
fn boolean_logic_evaluates_when_needed() {
    let src = "fn side() -> bool { print_int(999); true }\n\
               fn main() { print_bool(true && side()); }";
    // Here the rhs must run, printing 999, then the result true.
    assert_eq!(run(src), "999\ntrue\n");
}

#[test]
fn division_by_zero_is_a_runtime_error() {
    assert_eq!(
        run_err("fn main() { let z = 0; print_int(1 / z); }"),
        VmError::DivisionByZero
    );
}

#[test]
fn remainder_by_zero_is_a_runtime_error() {
    assert_eq!(
        run_err("fn main() { let z = 0; print_int(1 % z); }"),
        VmError::DivisionByZero
    );
}

#[test]
fn integer_min_div_negative_one_overflows() {
    // -9223372036854775808 / -1 overflows i64.
    let src = "fn main() { let a = 0 - 9223372036854775807 - 1; let b = 0 - 1; print_int(a / b); }";
    assert_eq!(run_err(src), VmError::IntegerOverflow);
}

#[test]
fn step_limit_stops_infinite_loops() {
    let program = compile("fn main() { while true { } }", true);
    let err = execute_with_limit(&program, 10_000).unwrap_err();
    assert!(matches!(err, VmError::StepLimitExceeded(_)));
}

#[test]
fn optimized_and_unoptimized_agree() {
    let src = "fn main() { let x = (1 + 2) * 3 + 0; print_int(x); }";
    let unopt = execute(&compile(src, false)).unwrap().stdout;
    let opt = execute(&compile(src, true)).unwrap().stdout;
    assert_eq!(unopt, "9\n");
    assert_eq!(opt, unopt);
}

#[test]
fn disassembly_snapshot() {
    // Unoptimized so the arithmetic instructions are visible.
    let program = compile("fn main() { print_int(1 + 2); }", false);
    let asm = disassemble(&program);
    let expected = "\
fn#0 main (params=0, locals=0) [entry]
     0  push_int 1
     1  push_int 2
     2  add.i
     3  call_builtin print_int argc=1
     4  pop
     5  push_unit
     6  return
";
    assert_eq!(asm, expected);
}

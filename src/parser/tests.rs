//! Unit and property tests for the parser.

use super::ast::{ItemKind, TypeExprKind};
use super::print::print_ast;
use super::*;
use crate::diagnostics::Diagnostics;
use crate::lexer::tokenize;
use crate::source::SourceFile;

/// Parses `src`, asserting it is error-free, and returns the pretty-printed AST.
fn ast_of(src: &str) -> String {
    let file = SourceFile::new("test.lm", src);
    let mut diags = Diagnostics::new();
    let tokens = tokenize(&file, &mut diags);
    let ast = parse(tokens, &mut diags);
    assert!(
        !diags.has_errors(),
        "unexpected parse errors:\n{}",
        diags.render_all(&file)
    );
    print_ast(&ast)
}

/// Parses `src` expecting errors; returns the diagnostics sink.
fn parse_err(src: &str) -> Diagnostics {
    let file = SourceFile::new("test.lm", src);
    let mut diags = Diagnostics::new();
    let tokens = tokenize(&file, &mut diags);
    let _ = parse(tokens, &mut diags);
    assert!(diags.has_errors(), "expected parse errors for {src:?}");
    diags
}

#[test]
fn empty_function() {
    let out = ast_of("fn main() {}");
    assert_eq!(out, "fn main() -> unit\n  block\n");
}

#[test]
fn function_with_params_and_return() {
    let out = ast_of("fn add(a: i64, b: i64) -> i64 { a + b }");
    assert!(out.starts_with("fn add(a: i64, b: i64) -> i64\n"));
    assert!(out.contains("binary +"));
}

#[test]
fn precedence_mul_over_add() {
    // 1 + 2 * 3  ==>  (+ 1 (* 2 3))
    let out = ast_of("fn f() -> i64 { 1 + 2 * 3 }");
    let expected = "\
fn f() -> i64
  block
    tail
      binary +
        int 1
        binary *
          int 2
          int 3
";
    assert_eq!(out, expected);
}

#[test]
fn left_associativity_of_subtraction() {
    // 1 - 2 - 3  ==>  (- (- 1 2) 3)
    let out = ast_of("fn f() -> i64 { 1 - 2 - 3 }");
    let expected = "\
fn f() -> i64
  block
    tail
      binary -
        binary -
          int 1
          int 2
        int 3
";
    assert_eq!(out, expected);
}

#[test]
fn comparison_below_arithmetic() {
    // 1 + 2 < 3 * 4  ==>  (< (+ 1 2) (* 3 4))
    let out = ast_of("fn f() -> bool { 1 + 2 < 3 * 4 }");
    assert!(out.contains("binary <"));
    let lt = out.find("binary <").unwrap();
    let plus = out.find("binary +").unwrap();
    let star = out.find("binary *").unwrap();
    assert!(lt < plus && plus < star, "operator nesting wrong:\n{out}");
}

#[test]
fn logical_operators_bind_loosest() {
    // a && b || c  ==>  (|| (&& a b) c)
    let out = ast_of("fn f(a: bool, b: bool, c: bool) -> bool { a && b || c }");
    let expected = "\
fn f(a: bool, b: bool, c: bool) -> bool
  block
    tail
      binary ||
        binary &&
          name a
          name b
        name c
";
    assert_eq!(out, expected);
}

#[test]
fn unary_negation_and_not() {
    let out = ast_of("fn f() -> i64 { -(!true) }");
    assert!(out.contains("unary -"));
    assert!(out.contains("unary !"));
}

#[test]
fn assignment_is_right_associative() {
    let out = ast_of("fn f() { let mut a = 0; let mut b = 0; a = b = 1; }");
    // assign(a, assign(b, 1))
    let outer = out.find("assign").unwrap();
    let inner = out[outer + 6..].find("assign").unwrap();
    assert!(inner > 0, "assignment not nested right:\n{out}");
}

#[test]
fn call_with_arguments() {
    let out = ast_of("fn f() { g(1, 2, 3); }");
    assert!(out.contains("call"));
    assert_eq!(out.matches("arg").count(), 3);
}

#[test]
fn if_else_chain() {
    let out = ast_of("fn f(x: i64) -> i64 { if x { 1 } else if x { 2 } else { 3 } }");
    assert_eq!(out.matches("if").count(), 2);
    assert_eq!(out.matches("else").count(), 2);
}

#[test]
fn while_loop_and_statements() {
    let out = ast_of("fn f() { let mut i = 0; while i < 10 { i = i + 1; } }");
    assert!(out.contains("while"));
    assert!(out.contains("let mut i"));
}

#[test]
fn let_with_type_annotation() {
    let out = ast_of("fn f() { let x: i64 = 1; }");
    assert!(out.contains("let x: i64"));
}

#[test]
fn block_as_expression_value() {
    let out = ast_of("fn f() -> i64 { let x = { 1 + 2 }; x }");
    assert!(out.contains("let x"));
    // Inner block has a tail.
    assert_eq!(out.matches("tail").count(), 2);
}

#[test]
fn parenthesized_overrides_precedence() {
    // (1 + 2) * 3
    let out = ast_of("fn f() -> i64 { (1 + 2) * 3 }");
    let star = out.find("binary *").unwrap();
    let plus = out.find("binary +").unwrap();
    assert!(star < plus, "parentheses ignored:\n{out}");
}

// ---- error recovery ----

#[test]
fn reports_missing_semicolon() {
    let diags = parse_err("fn f() { let x = 1 let y = 2; }");
    assert!(
        diags
            .items()
            .iter()
            .any(|d| d.code == DiagCode::UnexpectedToken)
    );
}

#[test]
fn recovers_and_finds_later_errors() {
    // Two broken functions: the parser must report problems in both, not bail
    // after the first.
    let diags = parse_err("fn a( { } fn b( { }");
    assert!(
        diags.error_count() >= 2,
        "expected multiple errors, got {}",
        diags.error_count()
    );
}

#[test]
fn recovers_to_next_function() {
    // The first function body is broken; the second is fine and should parse.
    let file = SourceFile::new("t.lm", "fn bad() { @@@ } fn good() { }");
    let mut diags = Diagnostics::new();
    let tokens = tokenize(&file, &mut diags);
    let ast = parse(tokens, &mut diags);
    assert!(
        ast.items
            .iter()
            .any(|i| matches!(&i.kind, ItemKind::Fn(f) if f.name.name == "good"))
    );
}

#[test]
fn missing_type_yields_placeholder() {
    let file = SourceFile::new("t.lm", "fn f(x: ) {}");
    let mut diags = Diagnostics::new();
    let tokens = tokenize(&file, &mut diags);
    let ast = parse(tokens, &mut diags);
    assert!(
        diags
            .items()
            .iter()
            .any(|d| d.code == DiagCode::ExpectedType)
    );
    // Recovery still produced the function with one parameter.
    let ItemKind::Fn(decl) = &ast.items[0].kind else {
        panic!("expected a function")
    };
    assert_eq!(decl.params.len(), 1);
    assert_eq!(decl.params[0].ty.kind, TypeExprKind::Error);
}

mod property {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Parsing arbitrary tokens must never panic and must always terminate.
        #[test]
        fn never_panics(src in "[a-z0-9 (){}+\\-*/;=<>!&|,:]{0,120}") {
            let file = SourceFile::new("fuzz.lm", &src);
            let mut diags = Diagnostics::new();
            let tokens = tokenize(&file, &mut diags);
            let _ = parse(tokens, &mut diags);
            // Reaching here without panicking or hanging is the property.
        }
    }
}

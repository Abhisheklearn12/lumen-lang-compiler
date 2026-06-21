//! Tests for the source formatter: specific output, idempotence, round-trip.

use crate::diagnostics::Diagnostics;
use crate::format::format_source;
use crate::lexer::tokenize;
use crate::parser::parse;
use crate::source::SourceFile;

/// Parses `src` (asserting no errors) and formats it.
fn fmt(src: &str) -> String {
    let file = SourceFile::new("t.lm", src);
    let mut diags = Diagnostics::new();
    let tokens = tokenize(&file, &mut diags);
    let ast = parse(tokens, &mut diags);
    assert!(
        !diags.has_errors(),
        "parse errors:\n{}",
        diags.render_all(&file)
    );
    format_source(&ast)
}

#[test]
fn formats_a_simple_function() {
    let out = fmt("fn   main( ){let x=1+2;print_int(x);}");
    let expected = "\
fn main() {
    let x = 1 + 2;
    print_int(x);
}
";
    assert_eq!(out, expected);
}

#[test]
fn preserves_precedence_with_minimal_parentheses() {
    // No redundant parens, but `(1 + 2) * 3` keeps the needed ones.
    assert_eq!(
        fmt("fn f() -> i64 { 1 + 2 * 3 }").trim(),
        "fn f() -> i64 {\n    1 + 2 * 3\n}"
    );
    assert!(fmt("fn f() -> i64 { (1 + 2) * 3 }").contains("(1 + 2) * 3"));
}

#[test]
fn formats_control_flow() {
    let out = fmt("fn f(n:i64){let mut s=0;for i in 0..n{if i%2==0{continue;}s+=i;}}");
    assert!(out.contains("for i in 0..n {"));
    assert!(out.contains("if i % 2 == 0 {"));
    assert!(out.contains("continue;"));
    assert!(out.contains("s += i;"));
}

#[test]
fn formats_arrays_and_indexing() {
    let out = fmt("fn main(){let a=[1,2,3];a[0]=a[1];print_int(len(a));}");
    assert!(out.contains("let a = [1, 2, 3];"));
    assert!(out.contains("a[0] = a[1];"));
    assert!(out.contains("len(a)"));
}

#[test]
fn formats_const_and_strings() {
    let out = fmt(r#"const G:str="hi";fn main(){print_str(G+"!");}"#);
    assert!(out.contains(r#"const G: str = "hi";"#));
    assert!(out.contains(r#"G + "!""#));
}

#[test]
fn whole_numbers_in_float_position_keep_a_decimal() {
    // `3.0` must not be emitted as `3` (which would re-lex as i64).
    let out = fmt("fn main() { print_float(3.0); }");
    assert!(out.contains("3.0"), "got: {out}");
}

#[test]
fn formatting_is_idempotent() {
    let src = "fn fib(n:i64)->i64{if n<2{n}else{fib(n-1)+fib(n-2)}}fn main(){print_int(fib(10));}";
    let once = fmt(src);
    let twice = fmt(&once);
    assert_eq!(once, twice, "formatter is not idempotent");
}

#[test]
fn round_trips_a_realistic_program() {
    let src = "\
const LIMIT: i64 = 100;

fn is_even(n: i64) -> bool {
    n % 2 == 0
}

fn main() {
    let mut total = 0;
    for i in 0..LIMIT {
        if is_even(i) {
            total += i;
        }
    }
    print_int(total);
}
";
    // Formatting an already-canonical program returns it unchanged.
    assert_eq!(fmt(src), src);
}

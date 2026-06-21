//! Integration tests for `match` expressions.

mod common;

use common::{compile_errors, run_with, stdout};

#[test]
fn matches_integer_literals() {
    let src = "fn label(n: i64) -> str {\n\
                  match n { 1 => \"one\", 2 => \"two\", _ => \"many\" }\n\
               }\n\
               fn main() { print_str(label(1)); print_str(label(2)); print_str(label(9)); }";
    assert_eq!(stdout(src), "one\ntwo\nmany\n");
}

#[test]
fn matches_negative_literals() {
    let src = "fn sign(n: i64) -> i64 { match n { -1 => 0, 0 => 1, _ => 2 } }\n\
               fn main() { print_int(sign(-1)); print_int(sign(0)); print_int(sign(5)); }";
    assert_eq!(stdout(src), "0\n1\n2\n");
}

#[test]
fn matches_bool_without_wildcard() {
    // Both `true` and `false` are covered, so this is exhaustive.
    let src = "fn main() { let b = true; print_str(match b { true => \"t\", false => \"f\" }); }";
    assert_eq!(stdout(src), "t\n");
}

#[test]
fn scrutinee_is_evaluated_once() {
    let src = "fn tick() -> i64 { print_int(0); 1 }\n\
               fn main() { print_int(match tick() { 1 => 10, _ => 20 }); }";
    // `0` from the single call, then the chosen arm value `10`.
    assert_eq!(stdout(src), "0\n10\n");
}

#[test]
fn arms_can_be_blocks() {
    let src = "fn main() {\n\
                 let n = 2;\n\
                 let r = match n {\n\
                   1 => { let a = 5; a + 1 }\n\
                   _ => { let b = 9; b * 2 }\n\
                 };\n\
                 print_int(r);\n\
               }";
    assert_eq!(stdout(src), "18\n");
}

#[test]
fn optimized_and_unoptimized_agree() {
    let src = "fn f(n: i64) -> i64 { match n { 0 => 0, 1 => 100, 2 => 200, _ => n } }\n\
               fn main() { for i in 0..5 { print_int(f(i)); } }";
    let opt = match run_with(src, true) {
        common::Outcome::Ok(s) => s,
        other => panic!("opt failed: {other:?}"),
    };
    let unopt = match run_with(src, false) {
        common::Outcome::Ok(s) => s,
        other => panic!("unopt failed: {other:?}"),
    };
    assert_eq!(opt, unopt);
    assert_eq!(opt, "0\n100\n200\n3\n4\n");
}

#[test]
fn non_exhaustive_match_is_rejected() {
    let codes = compile_errors("fn main() { let x = 5; print_int(match x { 1 => 1, 2 => 2 }); }");
    assert!(codes.contains(&"E0318".to_string()), "got {codes:?}");
}

#[test]
fn mismatched_arm_types_are_rejected() {
    let codes = compile_errors("fn main() { let x = 1; let _y = match x { 1 => 1, _ => true }; }");
    assert!(codes.contains(&"E0309".to_string()), "got {codes:?}");
}

#[test]
fn pattern_type_must_match_scrutinee() {
    let codes =
        compile_errors("fn main() { let b = true; print_int(match b { 1 => 1, _ => 0 }); }");
    assert!(codes.contains(&"E0300".to_string()), "got {codes:?}");
}

//! Integration tests for string concatenation and the string builtins.

mod common;

use common::{compile_errors, stdout};

#[test]
fn concatenation_with_plus() {
    assert_eq!(
        stdout(r#"fn main() { print_str("foo" + "bar" + "baz"); }"#),
        "foobarbaz\n",
    );
}

#[test]
fn concatenation_of_variables() {
    let out = stdout(r#"fn main() { let a = "x"; let b = "y"; print_str(a + b + a); }"#);
    assert_eq!(out, "xyx\n");
}

#[test]
fn str_len_counts_bytes() {
    assert_eq!(
        stdout(r#"fn main() { print_int(str_len("hello")); }"#),
        "5\n"
    );
    assert_eq!(stdout(r#"fn main() { print_int(str_len("")); }"#), "0\n");
}

#[test]
fn value_to_string_builtins() {
    let out = stdout(
        "fn main() {\n\
        \x20   print_str(int_to_str(123));\n\
        \x20   print_str(bool_to_str(false));\n\
        \x20   print_str(float_to_str(2.5));\n\
         }",
    );
    assert_eq!(out, "123\nfalse\n2.5\n");
}

#[test]
fn building_a_message() {
    let out = stdout(
        "fn main() {\n\
        \x20   let n = 7;\n\
        \x20   print_str(\"n = \" + int_to_str(n));\n\
         }",
    );
    assert_eq!(out, "n = 7\n");
}

#[test]
fn arithmetic_plus_does_not_apply_to_mixed_types() {
    // `str + i64` is not allowed; only `str + str`.
    assert_eq!(
        compile_errors(r#"fn main() { let x = "a" + 1; }"#),
        vec!["E0305"],
    );
}

#[test]
fn subtraction_is_not_defined_for_strings() {
    assert_eq!(
        compile_errors(r#"fn main() { let x = "a" - "b"; }"#),
        vec!["E0305"],
    );
}

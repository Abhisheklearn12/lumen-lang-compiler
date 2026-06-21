//! Integration tests for tuples.

mod common;

use common::{compile_errors, stdout};

#[test]
fn construct_and_index() {
    assert_eq!(
        stdout("fn main() { let p = (3, 4); print_int(p.0); print_int(p.1); }"),
        "3\n4\n"
    );
}

#[test]
fn heterogeneous_elements() {
    let out = stdout(
        r#"fn main() { let t = (true, "hi", 42); print_bool(t.0); print_str(t.1); print_int(t.2); }"#,
    );
    assert_eq!(out, "true\nhi\n42\n");
}

#[test]
fn returned_from_a_function() {
    let out = stdout(
        "fn divmod(a: i64, b: i64) -> (i64, i64) { (a / b, a % b) }\n\
         fn main() { let r = divmod(17, 5); print_int(r.0); print_int(r.1); }",
    );
    assert_eq!(out, "3\n2\n");
}

#[test]
fn element_mutation() {
    assert_eq!(
        stdout("fn main() { let mut t = (1, 2); t.0 = 100; print_int(t.0); print_int(t.1); }"),
        "100\n2\n",
    );
}

#[test]
fn structural_equality() {
    let out = stdout("fn main() { print_bool((1, 2) == (1, 2)); print_bool((1, 2) == (1, 3)); }");
    assert_eq!(out, "true\nfalse\n");
}

#[test]
fn nested_tuples() {
    let out = stdout("fn main() { let t = ((1, 2), 3); print_int(t.0 .0); print_int(t.1); }");
    assert_eq!(out, "1\n3\n");
}

#[test]
fn tuple_parameter() {
    let out = stdout(
        "fn fst(p: (i64, i64)) -> i64 { p.0 }\n\
         fn main() { print_int(fst((9, 8))); }",
    );
    assert_eq!(out, "9\n");
}

#[test]
fn single_element_parens_are_grouping_not_tuple() {
    // `(1 + 2) * 3` is grouping; the result is an i64, not a tuple.
    assert_eq!(stdout("fn main() { print_int((1 + 2) * 3); }"), "9\n");
}

// ---- compile-time checks ----

#[test]
fn out_of_range_index_is_rejected() {
    assert_eq!(
        compile_errors("fn main() { let t = (1, 2); print_int(t.5); }"),
        vec!["E0316"]
    );
}

#[test]
fn indexing_a_non_tuple_is_rejected() {
    assert_eq!(
        compile_errors("fn main() { let x = 1; print_int(x.0); }"),
        vec!["E0316"]
    );
}

#[test]
fn tuple_type_mismatch_is_rejected() {
    assert_eq!(
        compile_errors("fn f() -> (i64, i64) { (1, true) } fn main() {}"),
        vec!["E0303"],
    );
}

#[test]
fn distinct_arities_are_distinct_types() {
    // A 2-tuple is not assignable where a 3-tuple is expected.
    assert_eq!(
        compile_errors("fn main() { let t: (i64, i64, i64) = (1, 2); }"),
        vec!["E0300"],
    );
}

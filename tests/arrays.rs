//! Integration tests for arrays: literals, indexing, mutation, `len`, and
//! bounds checking.

mod common;

use common::{Outcome, compile_errors, run, stdout};

#[test]
fn literal_indexing() {
    assert_eq!(
        stdout("fn main() { let a = [10, 20, 30]; print_int(a[1]); }"),
        "20\n"
    );
}

#[test]
fn length() {
    assert_eq!(
        stdout("fn main() { let a = [1, 2, 3, 4]; print_int(len(a)); }"),
        "4\n"
    );
}

#[test]
fn element_assignment() {
    let out =
        stdout("fn main() { let mut a = [1, 2, 3]; a[0] = 99; print_int(a[0]); print_int(a[2]); }");
    assert_eq!(out, "99\n3\n");
}

#[test]
fn iterate_and_sum() {
    let out = stdout(
        "fn main() {\n\
        \x20   let a = [5, 10, 15];\n\
        \x20   let mut total = 0;\n\
        \x20   for i in 0..len(a) { total += a[i]; }\n\
        \x20   print_int(total);\n\
         }",
    );
    assert_eq!(out, "30\n");
}

#[test]
fn arrays_pass_by_reference() {
    // Mutating an array inside a function is visible to the caller.
    let out = stdout(
        "fn set_first(a: [i64]) { a[0] = 100; }\n\
         fn main() { let mut a = [1, 2]; set_first(a); print_int(a[0]); }",
    );
    assert_eq!(out, "100\n");
}

#[test]
fn string_arrays() {
    let out = stdout(r#"fn main() { let s = ["a", "b", "c"]; print_str(s[2]); }"#);
    assert_eq!(out, "c\n");
}

#[test]
fn array_equality() {
    let out = stdout(
        "fn main() {\n\
        \x20   print_bool([1, 2, 3] == [1, 2, 3]);\n\
        \x20   print_bool([1, 2] == [1, 3]);\n\
         }",
    );
    assert_eq!(out, "true\nfalse\n");
}

#[test]
fn nested_array_type_annotation() {
    assert_eq!(
        stdout("fn main() { let a: [i64] = [7]; print_int(a[0]); }"),
        "7\n"
    );
}

#[test]
fn out_of_bounds_is_a_runtime_error() {
    match run("fn main() { let a = [1, 2]; print_int(a[2]); }") {
        Outcome::RuntimeError(e) => {
            assert!(e.to_string().contains("out of bounds"), "got {e}")
        }
        other => panic!("expected a runtime error, got {other:?}"),
    }
}

#[test]
fn negative_index_is_a_runtime_error() {
    match run("fn main() { let a = [1, 2]; let i = 0 - 1; print_int(a[i]); }") {
        Outcome::RuntimeError(e) => assert!(e.to_string().contains("out of bounds")),
        other => panic!("expected a runtime error, got {other:?}"),
    }
}

// ---- compile-time checks ----

#[test]
fn indexing_non_array_is_rejected() {
    assert_eq!(
        compile_errors("fn main() { let x = 1; print_int(x[0]); }"),
        vec!["E0315"]
    );
}

#[test]
fn non_integer_index_is_rejected() {
    assert_eq!(
        compile_errors("fn main() { let a = [1]; print_int(a[true]); }"),
        vec!["E0300"],
    );
}

#[test]
fn mismatched_element_types_are_rejected() {
    assert_eq!(
        compile_errors("fn main() { let a = [1, true]; }"),
        vec!["E0300"]
    );
}

#[test]
fn element_mutation_does_not_require_mut_binding() {
    // Arrays are reference types: mutating an element mutates the referent, so
    // the binding need not be `mut` (only rebinding the variable would).
    assert_eq!(
        stdout("fn main() { let a = [1, 2]; a[0] = 5; print_int(a[0]); }"),
        "5\n",
    );
}

#[test]
fn empty_array_literal_is_rejected() {
    assert_eq!(compile_errors("fn main() { let a = []; }"), vec!["E0314"]);
}

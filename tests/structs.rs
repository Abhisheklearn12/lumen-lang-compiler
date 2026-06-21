//! Integration tests for structs: declaration, construction, field access,
//! mutation, and reference semantics.

mod common;

use common::{compile_errors, stdout};

#[test]
fn construct_and_read_fields() {
    let out = stdout(
        "struct Point { x: i64, y: i64 }\n\
         fn main() { let p = Point { x: 3, y: 7 }; print_int(p.x); print_int(p.y); }",
    );
    assert_eq!(out, "3\n7\n");
}

#[test]
fn field_order_in_literal_is_irrelevant() {
    let out = stdout(
        "struct P { a: i64, b: i64 }\n\
         fn main() { let p = P { b: 2, a: 1 }; print_int(p.a); print_int(p.b); }",
    );
    assert_eq!(out, "1\n2\n");
}

#[test]
fn field_mutation() {
    let out = stdout(
        "struct C { n: i64 }\n\
         fn main() { let c = C { n: 0 }; c.n = 42; print_int(c.n); }",
    );
    assert_eq!(out, "42\n");
}

#[test]
fn structs_pass_by_reference() {
    let out = stdout(
        "struct Box { v: i64 }\n\
         fn bump(b: Box) { b.v = b.v + 1; }\n\
         fn main() { let b = Box { v: 10 }; bump(b); print_int(b.v); }",
    );
    assert_eq!(out, "11\n");
}

#[test]
fn mixed_field_types() {
    let out = stdout(
        "struct Person { name: str, age: i64 }\n\
         fn main() { let p = Person { name: \"Ada\", age: 36 }; print_str(p.name); print_int(p.age); }",
    );
    assert_eq!(out, "Ada\n36\n");
}

#[test]
fn struct_equality_is_structural() {
    let out = stdout(
        "struct V { x: i64 }\n\
         fn main() { print_bool(V { x: 1 } == V { x: 1 }); print_bool(V { x: 1 } == V { x: 2 }); }",
    );
    assert_eq!(out, "true\nfalse\n");
}

#[test]
fn forward_referenced_struct_type() {
    // A function may name a struct declared later in the file.
    let out = stdout(
        "fn first(p: Pair) -> i64 { p.a }\n\
         struct Pair { a: i64, b: i64 }\n\
         fn main() { print_int(first(Pair { a: 9, b: 0 })); }",
    );
    assert_eq!(out, "9\n");
}

#[test]
fn no_struct_literal_ambiguity_in_conditions() {
    // `if p` where `p` is a bool, followed by a block - must not be read as a
    // struct literal `p { ... }`.
    let out = stdout("fn main() { let p = true; if p { print_int(1); } else { print_int(2); } }");
    assert_eq!(out, "1\n");
}

// ---- compile-time checks ----

#[test]
fn unknown_field_is_rejected() {
    assert_eq!(
        compile_errors("struct P { x: i64 } fn main() { let p = P { x: 1 }; print_int(p.z); }"),
        vec!["E0316"],
    );
}

#[test]
fn missing_field_is_rejected() {
    assert_eq!(
        compile_errors("struct P { x: i64, y: i64 } fn main() { let p = P { x: 1 }; }"),
        vec!["E0317"],
    );
}

#[test]
fn wrong_field_type_is_rejected() {
    assert_eq!(
        compile_errors("struct P { x: i64 } fn main() { let p = P { x: true }; }"),
        vec!["E0300"],
    );
}

#[test]
fn field_access_on_non_struct_is_rejected() {
    assert_eq!(
        compile_errors("fn main() { let x = 1; print_int(x.field); }"),
        vec!["E0316"],
    );
}

#[test]
fn struct_name_conflicts_with_function() {
    assert_eq!(
        compile_errors("struct Foo { x: i64 } fn Foo() {} fn main() {}"),
        vec!["E0201"],
    );
}

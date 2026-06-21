//! Integration tests for `for v in array` element iteration.

mod common;

use common::{compile_errors, run_with, stdout};

#[test]
fn iterates_array_elements() {
    assert_eq!(
        stdout("fn main() { for x in [10, 20, 30] { print_int(x); } }"),
        "10\n20\n30\n"
    );
}

#[test]
fn sums_over_a_named_array() {
    let out = stdout(
        "fn main() {\n\
            let xs = [1, 2, 3, 4];\n\
            let mut s = 0;\n\
            for x in xs { s += x; }\n\
            print_int(s);\n\
        }",
    );
    assert_eq!(out, "10\n");
}

#[test]
fn continue_skips_to_the_next_element() {
    let out =
        stdout("fn main() { for x in [1, 2, 3, 4] { if x == 2 { continue; } print_int(x); } }");
    assert_eq!(out, "1\n3\n4\n");
}

#[test]
fn break_exits_early() {
    let out = stdout("fn main() { for x in [5, 6, 7, 8] { if x == 7 { break; } print_int(x); } }");
    assert_eq!(out, "5\n6\n");
}

#[test]
fn binds_the_element_type() {
    // Each element is an `f64`, so float arithmetic must type-check.
    let out = stdout("fn main() { for x in [1.5, 2.5] { print_float(x + 0.5); } }");
    assert_eq!(out, "2\n3\n");
}

#[test]
fn array_expression_is_evaluated_once() {
    // `build()` prints a marker each call; iterating its result must call it once.
    let out = stdout(
        "fn build() -> [i64] { print_int(0); [7, 8] }\n\
         fn main() { for x in build() { print_int(x); } }",
    );
    assert_eq!(out, "0\n7\n8\n");
}

#[test]
fn optimized_and_unoptimized_agree() {
    let src = "fn main() {\n\
        let xs = [3, 1, 4, 1, 5];\n\
        let mut hi = 0;\n\
        for x in xs { if x > hi { hi = x; } }\n\
        print_int(hi);\n\
    }";
    let opt = match run_with(src, true) {
        common::Outcome::Ok(s) => s,
        other => panic!("opt run failed: {other:?}"),
    };
    let unopt = match run_with(src, false) {
        common::Outcome::Ok(s) => s,
        other => panic!("unopt run failed: {other:?}"),
    };
    assert_eq!(opt, unopt);
    assert_eq!(opt, "5\n");
}

#[test]
fn iterating_a_non_array_is_an_error() {
    // E0315 is the "not indexable / not iterable" diagnostic.
    let codes = compile_errors("fn main() { for x in 42 { print_int(x); } }");
    assert!(codes.contains(&"E0315".to_string()), "got {codes:?}");
}

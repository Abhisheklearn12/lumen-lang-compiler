//! Integration tests for the math and string standard-library builtins.

mod common;

use common::{compile_errors, stdout};

#[test]
fn integer_math() {
    assert_eq!(stdout("fn main() { print_int(abs(0 - 7)); }"), "7\n");
    assert_eq!(
        stdout("fn main() { print_int(min(3, 9)); print_int(max(3, 9)); }"),
        "3\n9\n"
    );
    assert_eq!(stdout("fn main() { print_int(pow_int(2, 10)); }"), "1024\n");
}

#[test]
fn float_math() {
    assert_eq!(stdout("fn main() { print_float(sqrt(16.0)); }"), "4\n");
    assert_eq!(
        stdout("fn main() { print_float(abs_float(0.0 - 2.5)); }"),
        "2.5\n"
    );
    assert_eq!(stdout("fn main() { print_int(floor(3.9)); }"), "3\n");
    assert_eq!(
        stdout("fn main() { print_float(pow_float(2.0, 3.0)); }"),
        "8\n"
    );
}

#[test]
fn string_ops() {
    assert_eq!(
        stdout(r#"fn main() { print_str(substring("hello world", 6, 11)); }"#),
        "world\n"
    );
    assert_eq!(
        stdout(r#"fn main() { print_int(char_at("ABC", 0)); }"#),
        "65\n"
    );
    assert_eq!(
        stdout(r#"fn main() { print_str(str_repeat("xy", 3)); }"#),
        "xyxyxy\n"
    );
}

#[test]
fn substring_clamps_out_of_range() {
    // Out-of-range indices clamp rather than crashing.
    assert_eq!(
        stdout(r#"fn main() { print_str(substring("hi", 0, 99)); }"#),
        "hi\n"
    );
    assert_eq!(
        stdout(r#"fn main() { print_str(substring("hi", 5, 1)); }"#),
        "\n"
    );
}

#[test]
fn builtins_are_type_checked() {
    // `min` takes two i64s; passing a bool is a type error.
    assert_eq!(
        compile_errors("fn main() { print_int(min(1, true)); }"),
        vec!["E0300"]
    );
    // `sqrt` takes an f64, not an i64.
    assert_eq!(
        compile_errors("fn main() { print_float(sqrt(4)); }"),
        vec!["E0300"]
    );
    // Wrong arity.
    assert_eq!(
        compile_errors("fn main() { print_int(max(1)); }"),
        vec!["E0301"]
    );
}

#[test]
fn composes_with_the_language() {
    let out = stdout(
        "fn main() {\n\
        \x20   let mut best = 0;\n\
        \x20   for i in 0..10 { best = max(best, i * (10 - i)); }\n\
        \x20   print_int(best);\n\
         }",
    );
    // max of i*(10-i) for i in 0..10 is 25 (i = 5).
    assert_eq!(out, "25\n");
}

#[test]
fn integer_gcd_sign_and_clamp() {
    assert_eq!(stdout("fn main() { print_int(gcd(48, 36)); }"), "12\n");
    assert_eq!(stdout("fn main() { print_int(gcd(0, 0)); }"), "0\n");
    assert_eq!(
        stdout("fn main() { print_int(sign(-7)); print_int(sign(0)); print_int(sign(7)); }"),
        "-1\n0\n1\n"
    );
    assert_eq!(
        stdout(
            "fn main() { print_int(clamp(15, 0, 10)); print_int(clamp(-3, 0, 10)); print_int(clamp(5, 0, 10)); }"
        ),
        "10\n0\n5\n"
    );
}

#[test]
fn float_min_and_max() {
    assert_eq!(
        stdout("fn main() { print_float(min_float(2.5, 1.5)); print_float(max_float(2.5, 1.5)); }"),
        "1.5\n2.5\n"
    );
}

#[test]
fn string_predicates_and_search() {
    assert_eq!(
        stdout(r#"fn main() { print_bool(starts_with("hello", "he")); }"#),
        "true\n"
    );
    assert_eq!(
        stdout(r#"fn main() { print_bool(ends_with("hello", "xo")); }"#),
        "false\n"
    );
    assert_eq!(
        stdout(r#"fn main() { print_bool(contains("hello", "ell")); }"#),
        "true\n"
    );
    assert_eq!(
        stdout(r#"fn main() { print_int(index_of("hello", "l")); }"#),
        "2\n"
    );
    assert_eq!(
        stdout(r#"fn main() { print_int(index_of("hello", "z")); }"#),
        "-1\n"
    );
}

#[test]
fn numeric_conversions() {
    assert_eq!(stdout("fn main() { print_float(to_float(7)); }"), "7\n");
    assert_eq!(stdout("fn main() { print_int(to_int(3.9)); }"), "3\n");
    assert_eq!(
        stdout("fn main() { print_int(ceil(2.1)); print_int(round(2.5)); }"),
        "3\n3\n"
    );
}

#[test]
fn parse_and_char_helpers() {
    assert_eq!(
        stdout(r#"fn main() { print_int(parse_int("  42 ")); }"#),
        "42\n"
    );
    assert_eq!(
        stdout(r#"fn main() { print_int(parse_int("nope")); }"#),
        "0\n"
    );
    assert_eq!(stdout("fn main() { print_str(char_to_str(65)); }"), "A\n");
}

#[test]
fn conversions_round_trip_through_arithmetic() {
    // Average two integers using float division, then truncate back.
    let out = stdout(
        "fn main() {\n\
            let a = 3;\n\
            let b = 8;\n\
            print_int(to_int((to_float(a) + to_float(b)) / 2.0));\n\
         }",
    );
    assert_eq!(out, "5\n");
}

#[test]
fn string_case_and_trim() {
    assert_eq!(
        stdout(r#"fn main() { print_str(to_upper("Hello, World")); }"#),
        "HELLO, WORLD\n"
    );
    assert_eq!(
        stdout(r#"fn main() { print_str(to_lower("Hello, World")); }"#),
        "hello, world\n"
    );
    assert_eq!(
        stdout(r#"fn main() { print_str(trim("   spaced   ")); }"#),
        "spaced\n"
    );
}

#[test]
fn least_common_multiple() {
    assert_eq!(stdout("fn main() { print_int(lcm(4, 6)); }"), "12\n");
    assert_eq!(stdout("fn main() { print_int(lcm(0, 5)); }"), "0\n");
    assert_eq!(stdout("fn main() { print_int(lcm(21, 6)); }"), "42\n");
}

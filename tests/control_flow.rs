//! Integration tests for loops, `break`/`continue`, and compound assignment.

mod common;

use common::stdout;

#[test]
fn for_range_sums() {
    assert_eq!(
        stdout("fn main() { let mut s = 0; for i in 0..5 { s += i; } print_int(s); }"),
        "10\n"
    );
}

#[test]
fn for_range_is_half_open() {
    // 0..3 visits 0, 1, 2 (not 3).
    assert_eq!(
        stdout("fn main() { for i in 0..3 { print_int(i); } }"),
        "0\n1\n2\n"
    );
}

#[test]
fn break_exits_the_loop() {
    let out = stdout("fn main() { for i in 0..100 { if i == 4 { break; } print_int(i); } }");
    assert_eq!(out, "0\n1\n2\n3\n");
}

#[test]
fn continue_skips_to_next_iteration() {
    let out = stdout("fn main() { for i in 0..6 { if i % 2 == 0 { continue; } print_int(i); } }");
    assert_eq!(out, "1\n3\n5\n");
}

#[test]
fn break_only_exits_the_innermost_loop() {
    let out = stdout(
        "fn main() {\n\
        \x20   for i in 0..3 {\n\
        \x20       for j in 0..3 {\n\
        \x20           if j == 1 { break; }\n\
        \x20           print_int(i * 10 + j);\n\
        \x20       }\n\
        \x20   }\n\
         }",
    );
    // Each outer iteration prints only j==0: 0, 10, 20.
    assert_eq!(out, "0\n10\n20\n");
}

#[test]
fn for_end_bound_is_evaluated_once() {
    // `side()` returns 3 and prints 999; if the bound were re-evaluated each
    // iteration, 999 would appear more than once.
    let out = stdout(
        "fn side() -> i64 { print_int(999); 3 }\n\
         fn main() { for i in 0..side() { print_int(i); } }",
    );
    assert_eq!(out, "999\n0\n1\n2\n");
}

#[test]
fn compound_assignment_operators() {
    let out = stdout(
        "fn main() {\n\
        \x20   let mut x = 10;\n\
        \x20   x += 5; print_int(x);\n\
        \x20   x -= 3; print_int(x);\n\
        \x20   x *= 2; print_int(x);\n\
        \x20   x /= 4; print_int(x);\n\
        \x20   x %= 4; print_int(x);\n\
         }",
    );
    // 10 +5=15, -3=12, *2=24, /4=6, %4=2
    assert_eq!(out, "15\n12\n24\n6\n2\n");
}

#[test]
fn while_with_break_and_continue() {
    let out = stdout(
        "fn main() {\n\
        \x20   let mut i = 0;\n\
        \x20   while true {\n\
        \x20       i += 1;\n\
        \x20       if i == 2 { continue; }\n\
        \x20       if i > 4 { break; }\n\
        \x20       print_int(i);\n\
        \x20   }\n\
         }",
    );
    // prints 1, (skip 2), 3, 4, then i=5 breaks
    assert_eq!(out, "1\n3\n4\n");
}

//! Tests for the bytecode peephole optimizer.

use super::*;
use crate::backend::bytecode::Op;
use crate::backend::{execute, generate};
use crate::diagnostics::Diagnostics;
use crate::hir::lower;
use crate::lexer::tokenize;
use crate::opt::{OptOptions, optimize as optimize_hir};
use crate::parser::parse;
use crate::sema::{check, resolve};
use crate::source::SourceFile;

/// Compiles `src` to (peephole-optimized, raw) bytecode programs.
fn programs(src: &str) -> (Program, Program) {
    let file = SourceFile::new("t.lm", src);
    let mut diags = Diagnostics::new();
    let tokens = tokenize(&file, &mut diags);
    let ast = parse(tokens, &mut diags);
    let res = resolve(&ast, &mut diags);
    let tc = check(&ast, &res, &mut diags);
    assert!(!diags.has_errors(), "errors:\n{}", diags.render_all(&file));
    let mut hir = lower(&ast, &res, &tc);
    optimize_hir(&mut hir, OptOptions::default());
    let raw = generate(&hir);
    let mut opt = generate(&hir);
    optimize(&mut opt);
    (opt, raw)
}

#[test]
fn push_pop_pair_is_removed() {
    let mut chunk = Chunk {
        name: "t".into(),
        n_locals: 0,
        n_params: 0,
        consts: Vec::new(),
        code: vec![Op::PushInt(1), Op::Pop, Op::PushUnit, Op::Return],
    };
    let removed = eliminate_push_pop(&mut chunk);
    assert_eq!(removed, 2);
    assert_eq!(chunk.code.len(), 2);
    assert!(matches!(chunk.code[0], Op::PushUnit));
    assert!(matches!(chunk.code[1], Op::Return));
}

#[test]
fn jump_targets_are_remapped_after_removal() {
    // bb: push;pop (removable), then a jump that targets index 2 (PushUnit).
    let mut chunk = Chunk {
        name: "t".into(),
        n_locals: 0,
        n_params: 0,
        consts: Vec::new(),
        code: vec![
            Op::PushInt(1),
            Op::Pop,
            Op::PushUnit,
            Op::Jump(2),
            Op::Return,
        ],
    };
    eliminate_push_pop(&mut chunk);
    // After removing the first two ops, PushUnit is index 0 and the jump must
    // now target 0.
    let jump = chunk.code.iter().find_map(|op| match op {
        Op::Jump(t) => Some(*t),
        _ => None,
    });
    assert_eq!(jump, Some(0));
}

#[test]
fn jump_threading_skips_trampolines() {
    let mut chunk = Chunk {
        name: "t".into(),
        n_locals: 0,
        n_params: 0,
        consts: Vec::new(),
        // index 0 jumps to 1, which jumps to 2.
        code: vec![Op::Jump(1), Op::Jump(2), Op::Return],
    };
    thread_jumps(&mut chunk);
    assert!(matches!(chunk.code[0], Op::Jump(2)));
}

#[test]
fn peephole_shrinks_or_preserves_code() {
    let (opt, raw) = programs(
        "fn main() {\n\
        \x20   let mut i = 0;\n\
        \x20   while i < 5 { print_int(i); i = i + 1; }\n\
         }",
    );
    let raw_len: usize = raw.functions.iter().map(|c| c.code.len()).sum();
    let opt_len: usize = opt.functions.iter().map(|c| c.code.len()).sum();
    assert!(opt_len <= raw_len, "peephole grew the code");
}

#[test]
fn peephole_preserves_output() {
    // For a range of programs, optimized and raw bytecode must run identically.
    let sources = [
        "fn main() { let mut s = 0; for i in 0..10 { s += i; } print_int(s); }",
        "fn fib(n: i64) -> i64 { if n < 2 { n } else { fib(n-1) + fib(n-2) } }\n\
         fn main() { print_int(fib(12)); }",
        "fn main() { let mut x = 1; while x < 100 { x = x * 2; } print_int(x); }",
        "fn main() { print_bool(true && false); print_bool(false || true); }",
    ];
    for src in sources {
        let (opt, raw) = programs(src);
        let opt_out = execute(&opt).unwrap().stdout;
        let raw_out = execute(&raw).unwrap().stdout;
        assert_eq!(opt_out, raw_out, "peephole changed output for:\n{src}");
    }
}

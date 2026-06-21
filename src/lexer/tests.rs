//! Unit and property tests for the lexer.

use super::*;
use crate::source::SourceFile;

/// Lexes `src` and returns the token kinds (dropping the trailing `Eof`),
/// asserting that no diagnostics were produced.
fn kinds(src: &str) -> Vec<TokenKind> {
    let file = SourceFile::new("test.lm", src);
    let mut diags = Diagnostics::new();
    let mut toks = tokenize(&file, &mut diags);
    assert!(!diags.has_errors(), "unexpected lex errors for {src:?}");
    assert_eq!(toks.last().map(|t| &t.kind), Some(&TokenKind::Eof));
    toks.pop();
    toks.into_iter().map(|t| t.kind).collect()
}

/// Lexes `src` expecting at least one error; returns the diagnostics sink.
fn lex_err(src: &str) -> Diagnostics {
    let file = SourceFile::new("test.lm", src);
    let mut diags = Diagnostics::new();
    let _ = tokenize(&file, &mut diags);
    assert!(diags.has_errors(), "expected lex errors for {src:?}");
    diags
}

#[test]
fn empty_input_yields_only_eof() {
    let file = SourceFile::new("t.lm", "   \n\t ");
    let mut diags = Diagnostics::new();
    let toks = tokenize(&file, &mut diags);
    assert_eq!(toks.len(), 1);
    assert_eq!(toks[0].kind, TokenKind::Eof);
}

#[test]
fn operators_take_longest_match() {
    use TokenKind::*;
    assert_eq!(
        kinds("== = != ! <= < >= > -> - && ||"),
        vec![
            EqEq, Eq, BangEq, Bang, LtEq, Lt, GtEq, Gt, Arrow, Minus, AmpAmp, PipePipe
        ],
    );
}

#[test]
fn keywords_vs_identifiers() {
    use TokenKind::*;
    assert_eq!(
        kinds("fn let mut return if else while true false foo i64"),
        vec![
            Fn,
            Let,
            Mut,
            Return,
            If,
            Else,
            While,
            True,
            False,
            Ident("foo".into()),
            // `i64` is a plain identifier at the lexical level.
            Ident("i64".into()),
        ],
    );
}

#[test]
fn integer_and_float_literals() {
    use TokenKind::*;
    assert_eq!(kinds("0 42 1000"), vec![Int(0), Int(42), Int(1000)]);
    assert_eq!(kinds("1.25 0.5"), vec![Float(1.25), Float(0.5)]);
}

#[test]
fn trailing_dot_is_not_a_float() {
    use TokenKind::*;
    // `2.` is an integer followed by a `.` (field-access dot), not a float:
    // a float needs a fractional digit after the dot.
    assert_eq!(kinds("2."), vec![Int(2), Dot]);
    // `2.5` is a float; `2..5` is `2`, `..`, `5`.
    assert_eq!(kinds("2.5"), vec![Float(2.5)]);
    assert_eq!(kinds("2..5"), vec![Int(2), DotDot, Int(5)]);
}

#[test]
fn string_with_escapes() {
    use TokenKind::*;
    assert_eq!(kinds(r#""hello\nworld""#), vec![Str("hello\nworld".into())]);
    assert_eq!(kinds(r#""tab\there""#), vec![Str("tab\there".into())]);
    assert_eq!(kinds(r#""q\"q""#), vec![Str("q\"q".into())]);
}

#[test]
fn comments_are_skipped() {
    use TokenKind::*;
    assert_eq!(kinds("1 // line\n+ 2"), vec![Int(1), Plus, Int(2)]);
    assert_eq!(kinds("1 /* block */ + 2"), vec![Int(1), Plus, Int(2)]);
    assert_eq!(
        kinds("1 /* a /* nested */ b */ + 2"),
        vec![Int(1), Plus, Int(2)]
    );
}

#[test]
fn spans_point_at_the_right_bytes() {
    let file = SourceFile::new("t.lm", "let x = 42;");
    let mut diags = Diagnostics::new();
    let toks = tokenize(&file, &mut diags);
    // Token 3 is `42` at bytes 8..10.
    assert_eq!(toks[3].kind, TokenKind::Int(42));
    assert_eq!(file.snippet(toks[3].span), "42");
    assert_eq!(toks[0].span.lo, 0);
    assert_eq!(file.snippet(toks[0].span), "let");
}

#[test]
fn unterminated_string_recovers() {
    let diags = lex_err("\"oops");
    assert_eq!(diags.items()[0].code, DiagCode::UnterminatedString);
}

#[test]
fn unterminated_block_comment_recovers() {
    let diags = lex_err("/* never closed");
    assert_eq!(diags.items()[0].code, DiagCode::UnterminatedComment);
}

#[test]
fn invalid_escape_is_reported_but_recoverable() {
    let file = SourceFile::new("t.lm", r#""bad\q""#);
    let mut diags = Diagnostics::new();
    let toks = tokenize(&file, &mut diags);
    assert_eq!(diags.items()[0].code, DiagCode::InvalidEscape);
    // The string token is still produced.
    assert!(matches!(toks[0].kind, TokenKind::Str(_)));
}

#[test]
fn integer_overflow_is_reported() {
    let diags = lex_err("99999999999999999999999999");
    assert_eq!(diags.items()[0].code, DiagCode::InvalidNumber);
}

#[test]
fn lexing_always_ends_in_eof_even_on_error() {
    let file = SourceFile::new("t.lm", "@ # $ valid");
    let mut diags = Diagnostics::new();
    let toks = tokenize(&file, &mut diags);
    assert!(diags.has_errors());
    assert_eq!(toks.last().unwrap().kind, TokenKind::Eof);
    // The valid identifier still came through after the garbage.
    assert!(
        toks.iter()
            .any(|t| t.kind == TokenKind::Ident("valid".into()))
    );
}

mod property {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Lexing arbitrary input must never panic and must always terminate in
        /// a single trailing `Eof` token.
        #[test]
        fn never_panics_and_ends_in_eof(src in ".{0,200}") {
            let file = SourceFile::new("fuzz.lm", src);
            let mut diags = Diagnostics::new();
            let toks = tokenize(&file, &mut diags);
            prop_assert_eq!(toks.last().map(|t| &t.kind), Some(&TokenKind::Eof));
            // Exactly one Eof, and it is last.
            let eofs = toks.iter().filter(|t| t.kind == TokenKind::Eof).count();
            prop_assert_eq!(eofs, 1);
        }

        /// Every non-EOF token has a non-empty, in-bounds span.
        #[test]
        fn spans_are_well_formed(src in "[a-zA-Z0-9 +\\-*/();=]{0,200}") {
            let file = SourceFile::new("fuzz.lm", &src);
            let mut diags = Diagnostics::new();
            let toks = tokenize(&file, &mut diags);
            for tok in &toks {
                prop_assert!(tok.span.lo <= tok.span.hi);
                prop_assert!(tok.span.hi as usize <= src.len());
                if tok.kind != TokenKind::Eof {
                    prop_assert!(tok.span.hi > tok.span.lo);
                }
            }
        }
    }
}

//! Token kinds produced by the [lexer](super) and consumed by the
//! [parser](crate::parser).
//!
//! Literal tokens carry their *parsed* value (`Int(i64)`, `Float(f64)`,
//! `Str(String)`). Doing the conversion in the lexer keeps a single source of
//! truth for literal syntax and lets the parser stay free of numeric parsing.
//! The trade-off  that `TokenKind` is not `Copy` and not `Eq` (because of the
//! `f64`)  is handled by [`TokenKind::same_kind`], which compares variants
//! without comparing payloads.

use crate::span::Span;
use std::mem;

/// The lexical category of a token, plus any literal payload.
#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    // ---- Literals ----
    /// Integer literal, already range-checked to fit `i64`.
    Int(i64),
    /// Floating-point literal.
    Float(f64),
    /// String literal with escapes already resolved.
    Str(String),
    /// Identifier (also covers type names like `i64`, resolved later).
    Ident(String),

    // ---- Keywords ----
    Fn,
    Struct,
    Const,
    Let,
    Mut,
    Return,
    If,
    Else,
    While,
    For,
    In,
    Match,
    Break,
    Continue,
    True,
    False,

    // ---- Operators & punctuation ----
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    BangEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Eq,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    AmpAmp,
    PipePipe,
    Bang,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Colon,
    Arrow,
    FatArrow,
    Dot,
    DotDot,

    /// End-of-input sentinel. Always the final token in a stream.
    Eof,
}

impl TokenKind {
    /// Maps an identifier string to its keyword kind, if it is one.
    ///
    /// Primitive type names (`i64`, `bool`, …) are deliberately *not* keywords:
    /// they are ordinary identifiers recognised contextually by the parser,
    /// which keeps the type grammar open to extension without touching the lexer.
    pub fn keyword(ident: &str) -> Option<TokenKind> {
        Some(match ident {
            "fn" => TokenKind::Fn,
            "struct" => TokenKind::Struct,
            "const" => TokenKind::Const,
            "let" => TokenKind::Let,
            "mut" => TokenKind::Mut,
            "return" => TokenKind::Return,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "match" => TokenKind::Match,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            _ => return None,
        })
    }

    /// Whether two kinds are the same variant, ignoring any payload.
    ///
    /// The parser uses this to match against expected punctuation/keywords
    /// without having to construct a payload for literal/ident variants.
    pub fn same_kind(&self, other: &TokenKind) -> bool {
        mem::discriminant(self) == mem::discriminant(other)
    }

    /// A short, human-readable description for diagnostics, e.g. `` `+` `` or
    /// `keyword `fn``. Used to render "expected X, found Y" messages.
    pub fn describe(&self) -> String {
        use TokenKind::*;
        match self {
            Int(_) => "integer literal".to_string(),
            Float(_) => "float literal".to_string(),
            Str(_) => "string literal".to_string(),
            Ident(name) => format!("identifier `{name}`"),
            Eof => "end of file".to_string(),
            other => format!("`{}`", other.symbol()),
        }
    }

    /// The canonical spelling of a fixed token (keyword or punctuation).
    ///
    /// For literals and identifiers  which have no fixed spelling  this
    /// returns a placeholder; callers use [`describe`](Self::describe) for those.
    pub fn symbol(&self) -> &'static str {
        use TokenKind::*;
        match self {
            Fn => "fn",
            Struct => "struct",
            Const => "const",
            Let => "let",
            Mut => "mut",
            Return => "return",
            If => "if",
            Else => "else",
            While => "while",
            For => "for",
            In => "in",
            Match => "match",
            Break => "break",
            Continue => "continue",
            True => "true",
            False => "false",
            Plus => "+",
            Minus => "-",
            Star => "*",
            Slash => "/",
            Percent => "%",
            EqEq => "==",
            BangEq => "!=",
            Lt => "<",
            LtEq => "<=",
            Gt => ">",
            GtEq => ">=",
            Eq => "=",
            PlusEq => "+=",
            MinusEq => "-=",
            StarEq => "*=",
            SlashEq => "/=",
            PercentEq => "%=",
            AmpAmp => "&&",
            PipePipe => "||",
            Bang => "!",
            LParen => "(",
            RParen => ")",
            LBrace => "{",
            RBrace => "}",
            LBracket => "[",
            RBracket => "]",
            Comma => ",",
            Semi => ";",
            Colon => ":",
            Arrow => "->",
            FatArrow => "=>",
            Dot => ".",
            DotDot => "..",
            Int(_) | Float(_) | Str(_) | Ident(_) | Eof => "<value>",
        }
    }
}

/// A token: its kind and the source span it covers.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Token {
        Token { kind, span }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_lookup() {
        assert_eq!(TokenKind::keyword("fn"), Some(TokenKind::Fn));
        assert_eq!(TokenKind::keyword("while"), Some(TokenKind::While));
        assert_eq!(TokenKind::keyword("i64"), None);
        assert_eq!(TokenKind::keyword("foo"), None);
    }

    #[test]
    fn same_kind_ignores_payload() {
        assert!(TokenKind::Int(1).same_kind(&TokenKind::Int(999)));
        assert!(TokenKind::Ident("a".into()).same_kind(&TokenKind::Ident("b".into())));
        assert!(!TokenKind::Int(1).same_kind(&TokenKind::Float(1.0)));
    }

    #[test]
    fn describe_is_readable() {
        assert_eq!(TokenKind::Plus.describe(), "`+`");
        assert_eq!(TokenKind::Arrow.describe(), "`->`");
        assert_eq!(TokenKind::Fn.describe(), "`fn`");
        assert_eq!(TokenKind::Int(0).describe(), "integer literal");
    }
}

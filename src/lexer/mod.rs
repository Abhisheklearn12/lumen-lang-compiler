//! The lexer: turns source text into a flat [`Token`] stream.
//!
//! # Design
//!
//! The scanner is a hand-written, single-pass cursor over the source string. It
//! is deliberately not table- or regex-driven: the token grammar is small, and
//! explicit `match` arms are easier to read, debug, and attribute spans to than
//! a generated automaton.
//!
//! # Error recovery
//!
//! Lexing never aborts. On a malformed token (stray character, unterminated
//! string, bad number) it emits a [`Diagnostic`] and resynchronises  skipping
//! the offending character or taking a best-effort value  so that a single
//! typo does not hide every later token. The returned stream is therefore
//! always well-formed and always ends in [`TokenKind::Eof`], which frees the
//! parser from having to handle lexical errors.
//!
//! # Complexity
//!
//! `O(n)` in the length of the source; each byte is visited a constant number
//! of times. No backtracking.

mod token;

pub use token::{Token, TokenKind};

use crate::diagnostics::{Diagnostic, Diagnostics};
use crate::errors::DiagCode;
use crate::source::SourceFile;
use crate::span::Span;

/// Tokenises `file`, reporting any lexical errors into `diags`.
///
/// The returned vector always ends with an [`TokenKind::Eof`] token whose span
/// is the empty range at end-of-input, giving the parser a stable sentinel.
#[tracing::instrument(level = "debug", skip_all, fields(file = file.name()))]
pub fn tokenize(file: &SourceFile, diags: &mut Diagnostics) -> Vec<Token> {
    let tokens = Lexer::new(file.text(), diags).run();
    tracing::debug!(token_count = tokens.len(), "lexing complete");
    tokens
}

/// Internal scanning state. Not exposed; callers use [`tokenize`].
struct Lexer<'a> {
    src: &'a str,
    /// Current byte offset into `src`. Always on a UTF-8 char boundary.
    pos: usize,
    diags: &'a mut Diagnostics,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str, diags: &'a mut Diagnostics) -> Lexer<'a> {
        Lexer { src, pos: 0, diags }
    }

    fn run(mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        loop {
            self.skip_trivia();
            let start = self.pos;
            let Some(ch) = self.peek() else {
                let eof = Span::new(self.src.len() as u32, self.src.len() as u32);
                tokens.push(Token::new(TokenKind::Eof, eof));
                return tokens;
            };
            if let Some(token) = self.scan_token(ch, start) {
                tokens.push(token);
            }
        }
    }

    // ---- cursor primitives ----

    /// The current character without advancing.
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    /// The character one position ahead of the cursor.
    fn peek2(&self) -> Option<char> {
        let mut chars = self.src[self.pos..].chars();
        chars.next();
        chars.next()
    }

    /// Advances past and returns the current character.
    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    /// Advances if the current character equals `expected`.
    fn eat(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.pos += expected.len_utf8();
            true
        } else {
            false
        }
    }

    fn span_from(&self, start: usize) -> Span {
        Span::new(start as u32, self.pos as u32)
    }

    // ---- trivia ----

    /// Skips whitespace and both comment forms, looping until real input.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('/') if self.peek2() == Some('/') => self.skip_line_comment(),
                Some('/') if self.peek2() == Some('*') => self.skip_block_comment(),
                _ => return,
            }
        }
    }

    fn skip_line_comment(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            self.bump();
        }
    }

    /// Skips a `/* ... */` comment, supporting nesting. Reports an unterminated
    /// comment if EOF is reached before the nesting returns to zero.
    fn skip_block_comment(&mut self) {
        let start = self.pos;
        self.bump(); // '/'
        self.bump(); // '*'
        let mut depth = 1u32;
        while depth > 0 {
            match self.bump() {
                Some('/') if self.peek() == Some('*') => {
                    self.bump();
                    depth += 1;
                }
                Some('*') if self.peek() == Some('/') => {
                    self.bump();
                    depth -= 1;
                }
                Some(_) => {}
                None => {
                    self.diags.emit(
                        Diagnostic::error(
                            DiagCode::UnterminatedComment,
                            "unterminated block comment",
                        )
                        .with_primary(self.span_from(start), "comment starts here")
                        .with_help("add a closing `*/`"),
                    );
                    return;
                }
            }
        }
    }

    // ---- token dispatch ----

    /// Scans one token starting at `start` whose first char is `ch`.
    ///
    /// Returns `None` only when the input was an unrecoverable single character
    /// (already reported) that produces no token.
    fn scan_token(&mut self, ch: char, start: usize) -> Option<Token> {
        if ch.is_ascii_digit() {
            return Some(self.scan_number(start));
        }
        if is_ident_start(ch) {
            return Some(self.scan_ident(start));
        }
        if ch == '"' {
            return Some(self.scan_string(start));
        }
        self.scan_symbol(ch, start)
    }

    /// Scans punctuation/operators, choosing the longest match (`==` over `=`).
    fn scan_symbol(&mut self, ch: char, start: usize) -> Option<Token> {
        self.bump();
        let kind = match ch {
            '+' if self.eat('=') => TokenKind::PlusEq,
            '+' => TokenKind::Plus,
            '-' if self.eat('>') => TokenKind::Arrow,
            '-' if self.eat('=') => TokenKind::MinusEq,
            '-' => TokenKind::Minus,
            '*' if self.eat('=') => TokenKind::StarEq,
            '*' => TokenKind::Star,
            '/' if self.eat('=') => TokenKind::SlashEq,
            '/' => TokenKind::Slash,
            '%' if self.eat('=') => TokenKind::PercentEq,
            '%' => TokenKind::Percent,
            '=' if self.eat('=') => TokenKind::EqEq,
            '=' if self.eat('>') => TokenKind::FatArrow,
            '=' => TokenKind::Eq,
            '!' if self.eat('=') => TokenKind::BangEq,
            '!' => TokenKind::Bang,
            '<' if self.eat('=') => TokenKind::LtEq,
            '<' => TokenKind::Lt,
            '>' if self.eat('=') => TokenKind::GtEq,
            '>' => TokenKind::Gt,
            '&' if self.eat('&') => TokenKind::AmpAmp,
            '|' if self.eat('|') => TokenKind::PipePipe,
            '.' if self.eat('.') => TokenKind::DotDot,
            '.' => TokenKind::Dot,
            '(' => TokenKind::LParen,
            ')' => TokenKind::RParen,
            '{' => TokenKind::LBrace,
            '}' => TokenKind::RBrace,
            '[' => TokenKind::LBracket,
            ']' => TokenKind::RBracket,
            ',' => TokenKind::Comma,
            ';' => TokenKind::Semi,
            ':' => TokenKind::Colon,
            _ => {
                self.diags.emit(
                    Diagnostic::error(
                        DiagCode::UnexpectedChar,
                        format!("unexpected character `{ch}`"),
                    )
                    .with_primary(self.span_from(start), "not part of any token"),
                );
                return None;
            }
        };
        Some(Token::new(kind, self.span_from(start)))
    }

    /// Scans an integer or float literal beginning with an ASCII digit.
    fn scan_number(&mut self, start: usize) -> Token {
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
        }
        // A float requires a `.` *followed by* a digit, so `1.method` (none yet,
        // but future-proof) and `1..2` would not be misread as floats.
        let is_float = self.peek() == Some('.') && self.peek2().is_some_and(|c| c.is_ascii_digit());
        if is_float {
            self.bump(); // '.'
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.bump();
            }
        }
        let span = self.span_from(start);
        let text = &self.src[start..self.pos];
        let kind = if is_float {
            match text.parse::<f64>() {
                Ok(v) => TokenKind::Float(v),
                Err(_) => self.bad_number(text, span),
            }
        } else {
            match text.parse::<i64>() {
                Ok(v) => TokenKind::Int(v),
                Err(_) => self.bad_number(text, span),
            }
        };
        Token::new(kind, span)
    }

    /// Reports an unparseable numeric literal and substitutes a zero so that
    /// later phases can proceed.
    fn bad_number(&mut self, text: &str, span: Span) -> TokenKind {
        self.diags.emit(
            Diagnostic::error(
                DiagCode::InvalidNumber,
                format!("invalid numeric literal `{text}`"),
            )
            .with_primary(span, "does not fit in a 64-bit number")
            .with_note("integer literals must be in the range of a signed 64-bit integer"),
        );
        TokenKind::Int(0)
    }

    /// Scans an identifier or keyword.
    fn scan_ident(&mut self, start: usize) -> Token {
        while self.peek().is_some_and(is_ident_continue) {
            self.bump();
        }
        let span = self.span_from(start);
        let text = &self.src[start..self.pos];
        let kind = TokenKind::keyword(text).unwrap_or_else(|| TokenKind::Ident(text.to_string()));
        Token::new(kind, span)
    }

    /// Scans a double-quoted string literal, resolving escape sequences.
    ///
    /// On an unterminated string the partial value is returned with a
    /// diagnostic; on an unknown escape the backslash sequence is kept verbatim
    /// and a diagnostic is emitted. Either way a usable token is produced.
    fn scan_string(&mut self, start: usize) -> Token {
        self.bump(); // opening quote
        let mut value = String::new();
        loop {
            match self.bump() {
                Some('"') => break,
                Some('\\') => self.scan_escape(&mut value, start),
                Some(c) => value.push(c),
                None => {
                    self.diags.emit(
                        Diagnostic::error(
                            DiagCode::UnterminatedString,
                            "unterminated string literal",
                        )
                        .with_primary(self.span_from(start), "string starts here")
                        .with_help("add a closing `\"`"),
                    );
                    break;
                }
            }
        }
        Token::new(TokenKind::Str(value), self.span_from(start))
    }

    /// Handles the character following a backslash inside a string.
    fn scan_escape(&mut self, value: &mut String, str_start: usize) {
        let esc_start = self.pos - 1;
        match self.bump() {
            Some('n') => value.push('\n'),
            Some('t') => value.push('\t'),
            Some('r') => value.push('\r'),
            Some('0') => value.push('\0'),
            Some('\\') => value.push('\\'),
            Some('"') => value.push('"'),
            Some(other) => {
                value.push(other);
                self.diags.emit(
                    Diagnostic::error(
                        DiagCode::InvalidEscape,
                        format!("unknown escape `\\{other}`"),
                    )
                    .with_primary(self.span_from(esc_start), "not a valid escape")
                    .with_help("valid escapes are \\n \\t \\r \\0 \\\\ \\\""),
                );
            }
            None => {
                // Backslash immediately before EOF: the unterminated-string path
                // in `scan_string` will report the missing quote.
                let _ = str_start;
            }
        }
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests;

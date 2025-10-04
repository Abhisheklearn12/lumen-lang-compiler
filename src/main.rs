/*
 * MIT License
 *
 * Copyright (c) 2025 Abhishek Kumar
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */


// Arithmetic expression parser + evaluator in a single file.
// Features:
//  - Lexer with identifiers, keywords, numbers, operators, comments, and line/column spans
//  - Pratt parser with correct precedence and right-associative exponent
//  - Statements: let, assignment, expression-stmt, blocks
//  - Built-in functions: sin, cos, tan, sqrt, pow, log, exp, abs
//  - Variables with lexical scopes, shadowing, and mutability
//  - Pretty AST printer
//  - REPL and unit tests, to see everything is working fine

#![allow(dead_code)]

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Write};
use std::str::FromStr;

// ------------------------- Position and Span -------------------------

/// Byte offset 
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pos {
    pub offset: usize, // byte offset from start
    pub line: usize,   // 1-based
    pub col: usize,    // 1-based (column in characters)
}

impl Pos {
    pub fn new(offset: usize, line: usize, col: usize) -> Self {
        Pos { offset, line, col }
    }
}

/// A span between two positions (start inclusive, end exclusive), useful for error slices.
#[derive(Clone, Copy, Debug)]
pub struct Span {
    pub start: Pos,
    pub end: Pos,
}

impl Span {
    pub fn new(start: Pos, end: Pos) -> Self {
        Span { start, end }
    }
}

// ------------------------- Tokens & Lexer -------------------------

/// Token kinds produced by the lexer. Each token keeps its source span for diagnostics.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Single-character tokens
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret, // ^ exponent
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Semicolon,
    Assign, // =

    // Literals
    Number(f64),
    Identifier(String),

    // Keywords
    Let,

    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn simple(kind: TokenKind, span: Span) -> Self {
        Token { kind, span }
    }
}

/// LexError carries a span and a message.
#[derive(Debug)]
pub struct LexError {
    pub span: Span,
    pub msg: String,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LexError at {}:{}: {}", self.span.start.line, self.span.start.col, self.msg)
    }
}

/// Lexer implementation: produces tokens from source char-by-char, tracking line/col.
pub struct Lexer<'a> {
    src: &'a str,
    chars: Vec<char>,
    idx: usize, // index into chars vector
    byte_offsets: Vec<usize>, // maps char index -> byte offset in original string
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        let mut chars = Vec::new();
        let mut byte_offsets = Vec::new();
        for (byte_idx, ch) in src.char_indices() {
            chars.push(ch);
            byte_offsets.push(byte_idx);
        }
        // push sentinel offset for end (byte length)
        byte_offsets.push(src.len());
        Lexer { src, chars, idx: 0, byte_offsets }
    }

    fn is_at_end(&self) -> bool {
        self.idx >= self.chars.len()
    }

    fn current_char(&self) -> Option<char> {
        self.chars.get(self.idx).copied()
    }

    fn peek_char(&self) -> Option<char> {
        self.chars.get(self.idx + 1).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.current_char();
        if ch.is_some() { self.idx += 1; }
        ch
    }

    fn make_pos(&self, char_index: usize) -> Pos {
        // compute line and col by scanning from start; O(n) but fine for educational example.
        let mut line = 1usize;
        let mut col = 1usize;
        for i in 0..char_index {
            if self.chars[i] == '\n' {
 line += 1; 
 col = 1; 
} 
else { 
  col += 1; 
}
        }
        let offset = self.byte_offsets.get(char_index).copied().unwrap_or(self.src.len());
        Pos::new(offset, line, col)
    }

    fn make_span_from_char_indices(&self, start: usize, end: usize) -> Span {
        let s = self.make_pos(start);
        let e = self.make_pos(end);
        Span::new(s, e)
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            // whitespace
            while let Some(c) = self.current_char() {
                if c.is_whitespace() { self.bump(); } else { break; }
            }
            // check for comment: `//` until end of line or `/* ... */` multi-line
            if self.current_char() == Some('/') && self.peek_char() == Some('/') {
                // single-line
                while let Some(c) = self.current_char() {
                    self.bump();
                    if c == '\n'
 { break; }
                }
                continue; // continue outer loop to trim more whitespace
            } else if self.current_char() == Some('/') && self.peek_char() == Some('*') {
                // multiline comment
                self.bump(); // /
                self.bump(); // *
                let mut closed = false;
                while !self.is_at_end() {
                    if self.current_char() == Some('*') && self.peek_char() == Some('/') {
                        self.bump();
                        self.bump();
                        closed = true;
                        break;
                    } else {
                        self.bump();
                    }
                }
                if !closed { break; } // unterminated, let tokenization handle it as EOF later
                continue;
            }
            break;
        }
    }

    pub fn tokenize(&mut self) -> Result<Vec<Token>, LexError> {
        let mut toks = Vec::new();
        while !self.is_at_end() {
            self.skip_whitespace_and_comments();
            if self.is_at_end() { break; }
            let start_idx = self.idx;
            let ch = self.current_char().unwrap();
            match ch {
                '+' | '-' | '*' | '/' | '%' | '^' | '(' | ')' | '{' | '}' | ',' | ';' | '=' => {
                    // single-character tokens
                    let kind = match ch {
                        '+' => TokenKind::Plus,
                        '-' => TokenKind::Minus,
                        '*' => TokenKind::Star,
                        '/' => TokenKind::Slash,
                        '%' => TokenKind::Percent,
                        '^' => TokenKind::Caret,
                        '(' => TokenKind::LParen,
                        ')' => TokenKind::RParen,
                        '{' => TokenKind::LBrace,
                        '}' => TokenKind::RBrace,
                        ',' => TokenKind::Comma,
                        ';' => TokenKind::Semicolon,
                        '=' => TokenKind::Assign,
                        _ => unreachable!(),
                    };
                    self.bump();
                    let span = self.make_span_from_char_indices(start_idx, self.idx);
                    toks.push(Token::simple(kind, span));
                }
                c if c.is_ascii_digit() || (c == '.' && self.peek_char().map_or(false, |p| p.is_ascii_digit())) => {
                    // number literal — support floats with optional leading dot
                    let mut end = start_idx;
                    let mut seen_dot = false;
                    while let Some(cc) = self.current_char() {
                        if cc.is_ascii_digit() {
                            self.bump(); end = self.idx;
                        } else if cc == '.' && !seen_dot {
                            seen_dot = true; self.bump(); end = self.idx;
                        } else { break; }
                    }
                    let span = self.make_span_from_char_indices(start_idx, end);
                    let slice = &self.src[span.start.offset..span.end.offset];
                    match f64::from_str(slice) {
                        Ok(n) => toks.push(Token::simple(TokenKind::Number(n), span)),
                        Err(_) => return Err(LexError { span, msg: format!("invalid number literal '{}'", slice) }),
                    }
                }
                c if is_identifier_start(c) => {
                    // identifier or keyword
                    let mut end = start_idx;
                    while let Some(cc) = self.current_char() {
                        if is_identifier_continue(cc) { self.bump(); end = self.idx; } else { break; }
                    }
                    let span = self.make_span_from_char_indices(start_idx, end);
                    let slice = &self.src[span.start.offset..span.end.offset];
                    let tok = match slice {
                        "let" => TokenKind::Let,
                        _ => TokenKind::Identifier(slice.to_string()),
                    };
                    toks.push(Token::simple(tok, span));
                }
                _ => {
                    let span = self.make_span_from_char_indices(start_idx, start_idx + 1);
                    return Err(LexError { span, msg: format!("unexpected character '{}'", ch) });
                }
            }
        }
        // EOF token span at end
        let eof_pos = self.make_pos(self.chars.len());
        let eof_span = Span::new(eof_pos, eof_pos);
        toks.push(Token::simple(TokenKind::Eof, eof_span));
        Ok(toks)
    }
}

fn is_identifier_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_identifier_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

// ------------------------- AST -------------------------

/// Binary operator with explicit precedence and associativity rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow, // ^ exponent
}

impl BinaryOp {
    /// precedence: higher value -> binds tighter
    pub fn precedence(&self) -> u8 {
        match self {
            BinaryOp::Add | BinaryOp::Sub => 10,
            BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => 20,
            BinaryOp::Pow => 30,
        }
    }

    /// true if operator is right-associative (like exponentiation)
    pub fn is_right_associative(&self) -> bool {
        matches!(self, BinaryOp::Pow)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Plus,
    Minus,
}

/// Expression AST nodes.
#[derive(Debug, Clone)]
pub enum Expr {
    Number(f64, Span),
    Ident(String, Span),
    Unary { op: UnaryOp, expr: Box<Expr>, span: Span },
    Binary { left: Box<Expr>, op: BinaryOp, right: Box<Expr>, span: Span },
    Call { callee: Box<Expr>, args: Vec<Expr>, span: Span },
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Number(_, s) => *s,
            Expr::Ident(_, s) => *s,
            Expr::Unary { span, .. } => *span,
            Expr::Binary { span, .. } => *span,
            Expr::Call { span, .. } => *span,
        }
    }
}

/// Statements for a small scripting surface.
#[derive(Debug, Clone)]
pub enum Stmt {
    Let { name: String, expr: Expr, span: Span },
    Assign { name: String, expr: Expr, span: Span },
    ExprStmt { expr: Expr, span: Span },
    Block { stmts: Vec<Stmt>, span: Span },
}

impl Stmt {
    pub fn span(&self) -> Span {
        match self {
            Stmt::Let { span, .. } => *span,
            Stmt::Assign { span, .. } => *span,
            Stmt::ExprStmt { span, .. } => *span,
            Stmt::Block { span, .. } => *span,
        }
    }
}

// ------------------------- Parser -------------------------

#[derive(Debug)]
pub struct ParseError {
    pub span: Span,
    pub msg: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ParseError at {}:{}: {}", self.span.start.line, self.span.start.col, self.msg)
    }
}

pub struct Parser<'a> {
    tokens: &'a [Token],
    idx: usize,
}

impl<'a> Parser<'a> {
    pub fn new(tokens: &'a [Token]) -> Self {
        Parser { tokens, idx: 0 }
    }

    fn current(&self) -> &Token {
        &self.tokens[self.idx]
    }

    fn advance(&mut self) {
        if !matches!(self.current().kind, TokenKind::Eof) { self.idx += 1; }
    }

    fn consume_if(&mut self, kind_pred: impl Fn(&TokenKind) -> bool) -> Option<Token> {
        if kind_pred(&self.current().kind) {
            let t = self.current().clone();
            self.advance();
            Some(t)
        } else { None }
    }

    fn expect(&mut self, expected: TokenKind) -> Result<Token, ParseError> {
        if std::mem::discriminant(&self.current().kind) == std::mem::discriminant(&expected) {
            let t = self.current().clone();
            self.advance();
            Ok(t)
        } else {
            Err(ParseError { span: self.current().span, msg: format!("expected {:?}, found {:?}", expected, self.current().kind) })
        }
    }
  
    // ------------------ Top-level: parse a sequence of statements ------------------
    pub fn parse_program(&mut self) -> Result<Vec<Stmt>, ParseError> {
        let mut stmts = Vec::new();
        while !matches!(self.current().kind, TokenKind::Eof) {
            let s = self.parse_stmt()?;
            stmts.push(s);
        }
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        match &self.current().kind {
            TokenKind::Let => self.parse_let(),
            TokenKind::LBrace => self.parse_block(),
            _ => self.parse_expr_stmt()
        }
    }

    fn parse_let(&mut self) -> Result<Stmt, ParseError> {
        let let_tok = self.expect(TokenKind::Let)?;
        // expect identifier
        let name = match &self.current().kind {
            TokenKind::Identifier(n) => { let s = n.clone(); self.advance(); s }
            _ => return Err(ParseError { span: self.current().span, msg: "expected identifier after 'let'".into() }),
        };
        self.expect(TokenKind::Assign)?; // =
        let expr = self.parse_expr()?;
        // semicolon optional: allow both `let x = 1;` and `let x = 1`
        let end_span = if matches!(self.current().kind, TokenKind::Semicolon) { let t = self.current().span; self.advance(); t } else { expr.span() };
        Ok(Stmt::Let { name, expr, span: Span::new(let_tok.span.start, end_span.end) })
    }

    fn parse_block(&mut self) -> Result<Stmt, ParseError> {
        let lbrace = self.expect(TokenKind::LBrace)?;
        let mut stmts = Vec::new();
        while !matches!(self.current().kind, TokenKind::RBrace) && !matches!(self.current().kind, TokenKind::Eof) {
            let s = self.parse_stmt()?;
            stmts.push(s);
        }
        let rbrace = self.expect(TokenKind::RBrace)?;
        Ok(Stmt::Block { stmts, span: Span::new(lbrace.span.start, rbrace.span.end) })
    }

    fn parse_expr_stmt(&mut self) -> Result<Stmt, ParseError> {
        // parse an expression; later we inspect whether this was an assignment by looking ahead
        let expr = self.parse_expr()?;
        // optional semicolon
        let end_span = if matches!(self.current().kind, TokenKind::Semicolon) { let t = self.current().span; self.advance(); t } else { expr.span() };

        // If the expression is a single identifier and the next token was `=` before we consumed it,
        // assignments are handled by parsing `ident = expr` as a statement. However, because parse_expr
        // would have consumed tokens, the only way to get here with an identifier is if the expression
        // is exactly an identifier and the next token (current) is Assign.
 if let Expr::Ident(name, ident_span) = &expr {
    if matches!(self.current().kind, TokenKind::Assign) {
        // consume '=' and parse rhs
        let _assign_span = self.current().span;
        self.advance();
    
        let rhs = self.parse_expr()?;
        let end_span2 = if matches!(self.current().kind, TokenKind::Semicolon) {
            let t = self.current().span;
            self.advance();
            t
        } 
        else {
            rhs.span()
        };
      
        let full_span = Span::new(ident_span.start, end_span2.end);
        return Ok(Stmt::Assign {
            name: name.clone(),
            expr: rhs,
            span: full_span,
        });
    }
}

// capture expr.span() before moving expr
let expr_span = expr.span();
Ok(Stmt::ExprStmt {
    expr,
    span: Span::new(expr_span.start, end_span.end),
})
}

    // ------------ Expressions: Pratt parser for binary ops with precedence & associativity ------------

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_prec(0)
    }

    fn parse_prec(&mut self, min_prec: u8) -> Result<Expr, ParseError> {
        // parse lhs: prefix expressions
        let mut lhs = self.parse_prefix()?;

        loop {
            // identify operator without borrowing token across advance
            let kind = self.current().kind.clone();
            let op = match kind {
                TokenKind::Plus => Some(BinaryOp::Add),
                TokenKind::Minus => Some(BinaryOp::Sub),
                TokenKind::Star => Some(BinaryOp::Mul),
                TokenKind::Slash => Some(BinaryOp::Div),
                TokenKind::Percent => Some(BinaryOp::Mod),
                TokenKind::Caret => Some(BinaryOp::Pow),
                _ => None,
            };
            if let Some(binop) = op {
                let prec = binop.precedence();
                let assoc_right = binop.is_right_associative();
                // for left-associative ops require prec > min_prec
                let cond = if assoc_right { prec >= min_prec } else { prec > min_prec };
                if !cond { break; }
                // capture span ends for later
                let left_start = lhs.span().start;
                // consume operator
                let _op_span_end = self.current().span.end; // copy span
                self.advance();
                // determine next minimal precedence
                let next_min = if assoc_right { prec } else { prec + 1 };
                let rhs = self.parse_prec(next_min)?;
                let span = Span::new(left_start, rhs.span().end);
                lhs = Expr::Binary { left: Box::new(lhs), op: binop, right: Box::new(rhs), span };
                continue;
            }
            break;
        }
        Ok(lhs)
    }

    fn parse_prefix(&mut self) -> Result<Expr, ParseError> {
        match &self.current().kind {
            TokenKind::Plus => {
                let t = self.current().clone(); self.advance();
                let expr = self.parse_prefix()?;
                let span = Span::new(t.span.start, expr.span().end);
                Ok(Expr::Unary { op: UnaryOp::Plus, expr: Box::new(expr), span })
            }
            TokenKind::Minus => {
                let t = self.current().clone(); self.advance();
                let expr = self.parse_prefix()?;
                let span = Span::new(t.span.start, expr.span().end);
                Ok(Expr::Unary { op: UnaryOp::Minus, expr: Box::new(expr), span })
            }
            TokenKind::Number(_) => {
                let t = self.current().clone(); self.advance();
                if let TokenKind::Number(v) = t.kind { Ok(Expr::Number(v, t.span)) } else { unreachable!() }
            }
            TokenKind::Identifier(_) => {
                let t = self.current().clone(); self.advance();
                let base = Expr::Ident(match &t.kind { TokenKind::Identifier(n) => n.clone(), _ => unreachable!() }, t.span);
                self.parse_postfix(base)
            }
            TokenKind::LParen => {
                let _l = self.current().clone(); self.advance();
                let e = self.parse_expr()?;
                self.expect(TokenKind::RParen)?;
                Ok(e)
            }
            other => Err(ParseError { span: other_span(other), msg: format!("unexpected token in expression: {:?}", other) }),
        }
    }

    fn parse_postfix(&mut self, mut expr: Expr) -> Result<Expr, ParseError> {
        loop {
            match &self.current().kind {
                TokenKind::LParen => {
                    let _lparen = self.current().clone(); self.advance();
                    let mut args = Vec::new();
                    if !matches!(self.current().kind, TokenKind::RParen) {
                        loop {
                            let a = self.parse_expr()?;
                            args.push(a);
                            if matches!(self.current().kind, TokenKind::Comma) { self.advance(); continue; }
                            break;
                        }
                    }
                    let rparen = self.expect(TokenKind::RParen)?;
                    let span = Span::new(expr.span().start, rparen.span.end);
                    expr = Expr::Call { callee: Box::new(expr), args, span };
                }
                _ => break,
            }
        }
        Ok(expr)
    }
}

fn other_span(_kind: &TokenKind) -> Span {
    let p = Pos::new(0, 1, 1);
    Span::new(p, p)
}

// ------------------------- Pretty AST printer -------------------------

pub fn pretty_print_stmt(stmt: &Stmt, f: &mut fmt::Formatter<'_>, indent: usize) -> fmt::Result {
    let pad = "  ".repeat(indent);
    match stmt {
        Stmt::Let { name, expr, .. } => {
            writeln!(f, "{}Let {} =", pad, name)?;
            pretty_print_expr(expr, f, indent + 1)?;
        }
        Stmt::Assign { name, expr, .. } => {
            writeln!(f, "{}Assign {} =", pad, name)?;
            pretty_print_expr(expr, f, indent + 1)?;
        }
        Stmt::ExprStmt { expr, .. } => {
            writeln!(f, "{}ExprStmt:", pad)?;
            pretty_print_expr(expr, f, indent + 1)?;
        }
        Stmt::Block { stmts, .. } => {
            writeln!(f, "{}Block {{", pad)?;
            for s in stmts { pretty_print_stmt(s, f, indent + 1)?; }
            writeln!(f, "{}}}", pad)?;
        }
    }
    Ok(())
}

pub fn pretty_print_expr(expr: &Expr, f: &mut fmt::Formatter<'_>, indent: usize) -> fmt::Result {
    let pad = "  ".repeat(indent);
    match expr {
        Expr::Number(n, _) => writeln!(f, "{}Number({})", pad, n),
        Expr::Ident(name, _) => writeln!(f, "{}Ident({})", pad, name),
        Expr::Unary { op, expr, .. } => {
            writeln!(f, "{}Unary({:?})", pad, op)?;
            pretty_print_expr(expr, f, indent + 1)
        }
        Expr::Binary { left, op, right, .. } => {
            writeln!(f, "{}Binary({:?})", pad, op)?;
            pretty_print_expr(left, f, indent + 1)?;
            pretty_print_expr(right, f, indent + 1)
        }
        Expr::Call { callee, args, .. } => {
            writeln!(f, "{}Call:", pad)?;
            pretty_print_expr(callee, f, indent + 1)?;
            for a in args { pretty_print_expr(a, f, indent + 1)?; }
            Ok(())
        }
    }
}

impl fmt::Display for Stmt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        pretty_print_stmt(self, f, 0)
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        pretty_print_expr(self, f, 0)
    }
}

// ------------------------- Evaluator -------------------------

/// Runtime errors: evaluation or name resolution errors.
#[derive(Debug)]
pub enum EvalError {
    DivByZero(Span),
    NameError(String, Span),
    ArityError(String, usize, Span),
    Other(String, Span),
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvalError::DivByZero(span) => write!(f, "Division by zero at {}:{}", span.start.line, span.start.col),
            EvalError::NameError(name, span) => write!(f, "Unknown name '{}' at {}:{}", name, span.start.line, span.start.col),
            EvalError::ArityError(name, got, span) => write!(f, "Function '{}' expected different arity (got {}) at {}:{}", name, got, span.start.line, span.start.col),
            EvalError::Other(msg, span) => write!(f, "{} at {}:{}", msg, span.start.line, span.start.col),
        }
    }
}

/// Environment: a stack of scopes for lexical variables. Each variable can be mutable or immutable.
#[derive(Clone)]
pub struct VarInfo { pub value: f64, pub mutable: bool }

#[derive(Clone)]
pub struct Env {
    scopes: Vec<HashMap<String, VarInfo>>,
}

impl Env {
    pub fn new() -> Self {
        Env { scopes: vec![HashMap::new()] }
    }

    pub fn push_scope(&mut self) { self.scopes.push(HashMap::new()); }
    pub fn pop_scope(&mut self) { self.scopes.pop(); }

    pub fn define(&mut self, name: &str, value: f64, mutable: bool) {
        if let Some(scope) = self.scopes.last_mut() { scope.insert(name.to_string(), VarInfo { value, mutable }); }
    }

    pub fn assign(&mut self, name: &str, value: f64) -> Result<(), ()> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(v) = scope.get_mut(name) {
                if v.mutable { v.value = value; return Ok(()) } else { return Err(()) }
            }
        }
        Err(())
    }

    pub fn get(&self, name: &str) -> Option<f64> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) { return Some(v.value); }
        }
        None
    }
}

/// Built-in functions: name -> (arity, implementation)
type BuiltinFn = fn(&[f64]) -> Result<f64, String>;

fn builtin_sin(args: &[f64]) -> Result<f64, String> { if args.len() == 1 { Ok(args[0].sin()) } else { Err("sin expects 1 arg".into()) } }
fn builtin_cos(args: &[f64]) -> Result<f64, String> { if args.len() == 1 { Ok(args[0].cos()) } else { Err("cos expects 1 arg".into()) } }
fn builtin_tan(args: &[f64]) -> Result<f64, String> { if args.len() == 1 { Ok(args[0].tan()) } else { Err("tan expects 1 arg".into()) } }
fn builtin_sqrt(args: &[f64]) -> Result<f64, String> { if args.len() == 1 { Ok(args[0].sqrt()) } else { Err("sqrt expects 1 arg".into()) } }
fn builtin_pow(args: &[f64]) -> Result<f64, String> { if args.len() == 2 { Ok(args[0].powf(args[1])) } else { Err("pow expects 2 args".into()) } }
fn builtin_log(args: &[f64]) -> Result<f64, String> { if args.len() == 1 { Ok(args[0].ln()) } else { Err("log expects 1 arg".into()) } }
fn builtin_exp(args: &[f64]) -> Result<f64, String> { if args.len() == 1 { Ok(args[0].exp()) } else { Err("exp expects 1 arg".into()) } }
fn builtin_abs(args: &[f64]) -> Result<f64, String> { if args.len() == 1 { Ok(args[0].abs()) } else { Err("abs expects 1 arg".into()) } }

fn builtin_list() -> HashMap<String, (usize, BuiltinFn)> {
    let mut m = HashMap::new();
    m.insert("sin".to_string(), (1usize, builtin_sin as BuiltinFn));
    m.insert("cos".to_string(), (1usize, builtin_cos));
    m.insert("tan".to_string(), (1usize, builtin_tan));
    m.insert("sqrt".to_string(), (1usize, builtin_sqrt));
    m.insert("pow".to_string(), (2usize, builtin_pow));
    m.insert("log".to_string(), (1usize, builtin_log));
    m.insert("exp".to_string(), (1usize, builtin_exp));
    m.insert("abs".to_string(), (1usize, builtin_abs));
    m
}

pub struct Evaluator {
    env: Env,
    builtins: HashMap<String, (usize, BuiltinFn)>,
}

impl Evaluator {
    pub fn new() -> Self {
        Evaluator { env: Env::new(), builtins: builtin_list() }
    }

    pub fn eval_program(&mut self, stmts: &[Stmt]) -> Result<Option<f64>, EvalError> {
        let mut last: Option<f64> = None;
        for s in stmts {
            last = Some(self.eval_stmt(s)?);
        }
        Ok(last)
    }

    fn eval_stmt(&mut self, stmt: &Stmt) -> Result<f64, EvalError> {
        match stmt {
            Stmt::Let { name, expr, span: _span } => {
                let v = self.eval_expr(expr)?;
                // by default let creates mutable binding
                self.env.define(name, v, true);
                Ok(v)
            }
            Stmt::Assign { name, expr, span } => {
                let v = self.eval_expr(expr)?;
                self.env.assign(name, v).map_err(|_| EvalError::NameError(name.clone(), *span))?;
                Ok(v)
            }
            Stmt::ExprStmt { expr, .. } => self.eval_expr(expr),
            Stmt::Block { stmts, .. } => {
                self.env.push_scope();
                let mut last = 0.0;
                for s in stmts { last = self.eval_stmt(s)?; }
                self.env.pop_scope();
                Ok(last)
            }
        }
    }

    fn eval_expr(&mut self, expr: &Expr) -> Result<f64, EvalError> {
        match expr {
            Expr::Number(n, _) => Ok(*n),
            Expr::Ident(name, span) => {
                self.env.get(name).ok_or_else(|| EvalError::NameError(name.clone(), *span))
            }
            Expr::Unary { op, expr, span: _ } => {
                let v = self.eval_expr(expr)?;
                Ok(match op { UnaryOp::Plus => v, UnaryOp::Minus => -v })
            }
            Expr::Binary { left, op, right, span } => {
                let a = self.eval_expr(left)?;
                let b = self.eval_expr(right)?;
                match op {
                    BinaryOp::Add => Ok(a + b),
                    BinaryOp::Sub => Ok(a - b),
                    BinaryOp::Mul => Ok(a * b),
                    BinaryOp::Div => {
                        if b == 0.0 { Err(EvalError::DivByZero(*span)) } else { Ok(a / b) }
                    }
                    BinaryOp::Mod => Ok(a % b),
                    BinaryOp::Pow => Ok(a.powf(b)),
                }
            }
            Expr::Call { callee, args, span } => {
                match &**callee {
                    Expr::Ident(name, _) => {
                        if let Some((arity, func)) = self.builtins.get(name).cloned() {
                            if arity != args.len() { 
                              return Err(EvalError::ArityError(name.clone(), args.len(), *span)); }
                            let mut eval_args = Vec::new();
                            for a in args { eval_args.push(self.eval_expr(a)?); }
                            match func(&eval_args) { Ok(r) => Ok(r), Err(msg) => Err(EvalError::Other(msg, *span)),
                             }
                        } else { Err(EvalError::NameError(name.clone(), *span)) }
                    }
                    _ => Err(EvalError::Other("call target must be identifier (builtin)".into(), *span)),
                }
            }
        }
    }
}

// ------------------------- Utilities: error pretty-printing -------------------------

pub fn render_error_with_source(src: &str, span: Span, message: &str) -> String {
    // show the line with a caret pointing to column position
    let lines: Vec<&str> = src.lines().collect();
    let line_idx = span.start.line.saturating_sub(1);
    if line_idx >= lines.len() { return format!("{} at {}:{}", message, span.start.line, span.start.col); }
    let line = lines[line_idx];
    let mut out = String::new();
    out.push_str(&format!("{} at {}:{}
", message, span.start.line, span.start.col));
    out.push_str(line);
    out.push('\n');
    for _ in 0..(span.start.col.saturating_sub(1)) { out.push(' '); }
    out.push('^');
    out
}

// ------------------------- REPL and Main -------------------------

fn repl() {
    println!("Extended expression language REPL. Type 'quit' or empty line to exit.");
    println!("Supports: numbers, + - * / % ^, parentheses, let, assignments, builtins (sin, cos, sqrt, pow, ...)");

    let mut src = String::new();
    let mut evaluator = Evaluator::new();
    loop {
        print!("> "); io::stdout().flush().unwrap();
        src.clear();
        if io::stdin().read_line(&mut src).is_err() { break; }
        let line = src.trim();
        if line.is_empty() || line == "quit" { break; }
        match run_source_with_evaluator(line, &mut evaluator) {
            Ok(Some(v)) => println!("=> {}", v),
            Ok(None) => println!("=> (no result)"),
            Err(e) => println!("error: {}", e),
        }
    }
}

fn run_source(src: &str) -> Result<Option<f64>, String> {
    let mut evaluator = Evaluator::new();
    run_source_with_evaluator(src, &mut evaluator)
}

fn run_source_with_evaluator(src: &str, evaluator: &mut Evaluator) -> Result<Option<f64>, String> {
    // lex
    let mut lexer = Lexer::new(src);
    let toks = lexer.tokenize().map_err(|e| format!("lexer error: {}", e))?;
    // parse
    let mut parser = Parser::new(&toks);
    let prog = parser.parse_program().map_err(|e| format!("parser error: {}", e))?;
    // eval
    let res = evaluator.eval_program(&prog).map_err(|e| format!("runtime error: {}", e))?;
    Ok(res)
}

fn main() {
    // If desired, you could parse args and run a file. For now REPL.
    repl();
}

// ------------------------- Unit tests -------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_arithmetic() {
        assert_eq!(run_source("1+2").unwrap(), Some(3.0));
        assert_eq!(run_source("2*3+1").unwrap(), Some(7.0));
        assert_eq!(run_source("2*(3+1)").unwrap(), Some(8.0));
    }

    #[test]
    fn test_precedence_pow_right_assoc() {
        // 2 ^ 3 ^ 2 == 2 ^ (3 ^ 2) == 2^9 == 512
        assert_eq!(run_source("2^3^2").unwrap(), Some(512.0));
    }

    #[test]
    fn test_unary_and_calls() {
        assert_eq!(run_source("-3").unwrap(), Some(-3.0));
        assert!((run_source("sin(0)").unwrap().unwrap() - 0.0).abs() < 1e-12);
        assert!((run_source("pow(2,3)").unwrap().unwrap() - 8.0).abs() < 1e-12);
    }

    #[test]
    fn test_variables() {
        // let x = 3
        assert_eq!(run_source("let x = 3; x").unwrap(), Some(3.0));
    }

    #[test]
    fn test_div_by_zero() {
        let res = run_source("1 / (2 - 2)");
        assert!(res.is_err());
    }
}

// This is the implementation of Parser through my learnings and experimentation "Fuck around and Find out".
// All these are going to be for my "Compiler" Project.

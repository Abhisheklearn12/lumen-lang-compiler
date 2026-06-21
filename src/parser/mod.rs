//! The parser: a hand-written recursive-descent parser with a Pratt
//! (precedence-climbing) core for expressions.
//!
//! # Design
//!
//! Recursive descent mirrors the grammar one function per production, which
//! keeps the parser readable and the call stack a direct trace of the parse.
//! Expressions use a single precedence-climbing routine ([`Parser::parse_bp`])
//! driven by a [`binding_power`] table, avoiding a separate function per
//! precedence level.
//!
//! # Error recovery
//!
//! The parser reports many errors per run. On a syntax error it emits a
//! diagnostic, then *resynchronises* to a stable boundary  the next item at
//! top level, or the next `;`/`}` inside a block  and continues. A progress
//! guard guarantees forward motion so malformed input can never loop forever.
//! The resulting (possibly partial) [`Ast`] lets later phases still run and
//! surface their own diagnostics.
//!
//! # Grammar (informal EBNF)
//!
//! ```text
//! program  := item*
//! item     := "fn" ident "(" params? ")" ("->" type)? block
//! params   := param ("," param)* ","?
//! param    := ident ":" type
//! type     := ident
//! block    := "{" stmt* expr? "}"
//! stmt     := "let" "mut"? ident (":" type)? "=" expr ";"
//!           | "return" expr? ";"
//!           | "while" expr block
//!           | expr ";"
//! expr     := assign
//! assign   := or ("=" assign)?
//! or       := and ("||" and)*
//! and      := cmp ("&&" cmp)*
//! cmp      := add (("=="|"!="|"<"|"<="|">"|">=") add)*
//! add      := mul (("+"|"-") mul)*
//! mul      := unary (("*"|"/"|"%") unary)*
//! unary    := ("-"|"!") unary | call
//! call     := primary ("(" args? ")")*
//! primary  := int | float | string | "true" | "false" | ident
//!           | "(" expr ")" | block | if
//! if       := "if" expr block ("else" (if | block))?
//! ```

pub mod ast;
pub mod print;

use ast::*;

use crate::diagnostics::{Diagnostic, Diagnostics};
use crate::errors::DiagCode;
use crate::lexer::{Token, TokenKind};
use crate::span::Span;

/// Parses a token stream into an [`Ast`], reporting syntax errors into `diags`.
///
/// `tokens` must end with [`TokenKind::Eof`] (as produced by
/// [`tokenize`](crate::lexer::tokenize)).
#[tracing::instrument(level = "debug", skip_all)]
pub fn parse(tokens: Vec<Token>, diags: &mut Diagnostics) -> Ast {
    let mut parser = Parser::new(tokens, diags);
    let ast = parser.parse_program();
    tracing::debug!(item_count = ast.items.len(), "parsing complete");
    ast
}

struct Parser<'a> {
    tokens: Vec<Token>,
    pos: usize,
    ids: NodeIdGen,
    diags: &'a mut Diagnostics,
    /// Whether a `Name { … }` may currently be parsed as a struct literal.
    /// Disabled while parsing `if`/`while` conditions and `for` bounds, where a
    /// trailing `{` instead begins the loop/branch body (the classic ambiguity).
    struct_ok: bool,
}

/// One element parsed from inside a block: either a statement, the block's
/// trailing value expression, or a recovered error.
enum BlockElem {
    Stmt(Stmt),
    Tail(Expr),
    Error,
}

impl<'a> Parser<'a> {
    fn new(tokens: Vec<Token>, diags: &'a mut Diagnostics) -> Parser<'a> {
        Parser {
            tokens,
            pos: 0,
            ids: NodeIdGen::new(),
            diags,
            struct_ok: true,
        }
    }

    /// Runs `f` with struct-literal parsing toggled, restoring the previous
    /// setting afterward.
    fn with_struct_ok<T>(&mut self, allowed: bool, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = self.struct_ok;
        self.struct_ok = allowed;
        let result = f(self);
        self.struct_ok = saved;
        result
    }

    /// The kind of the token `n` positions ahead of the cursor.
    fn nth_kind(&self, n: usize) -> &TokenKind {
        let idx = (self.pos + n).min(self.tokens.len() - 1);
        &self.tokens[idx].kind
    }

    // ---- token cursor ----

    fn cur(&self) -> &Token {
        // The stream always ends in Eof, so the final token is a valid sentinel
        // and indexing the last position can never go out of bounds.
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn kind(&self) -> &TokenKind {
        &self.cur().kind
    }

    fn span(&self) -> Span {
        self.cur().span
    }

    fn at_eof(&self) -> bool {
        matches!(self.kind(), TokenKind::Eof)
    }

    /// Whether the current token is the same variant as `k`.
    fn at(&self, k: &TokenKind) -> bool {
        self.kind().same_kind(k)
    }

    /// Consumes and returns the current token, stopping at `Eof`.
    fn bump(&mut self) -> Token {
        let tok = self.cur().clone();
        if !self.at_eof() {
            self.pos += 1;
        }
        tok
    }

    /// Consumes the current token if it matches `k`.
    fn eat(&mut self, k: &TokenKind) -> Option<Token> {
        if self.at(k) { Some(self.bump()) } else { None }
    }

    /// Consumes a token of kind `k`, or reports "expected … found …".
    fn expect(&mut self, k: &TokenKind) -> Option<Token> {
        if self.at(k) {
            Some(self.bump())
        } else {
            self.error_expected(k.describe());
            None
        }
    }

    fn fresh_id(&mut self) -> NodeId {
        self.ids.fresh()
    }

    fn mk_expr(&mut self, kind: ExprKind, span: Span) -> Expr {
        Expr {
            id: self.fresh_id(),
            kind,
            span,
        }
    }

    // ---- diagnostics ----

    fn error_expected(&mut self, what: impl Into<String>) {
        let what = what.into();
        let (code, headline) = if self.at_eof() {
            (
                DiagCode::UnexpectedEof,
                format!("expected {what}, found end of file"),
            )
        } else {
            (
                DiagCode::UnexpectedToken,
                format!("expected {what}, found {}", self.kind().describe()),
            )
        };
        self.diags.emit(
            Diagnostic::error(code, headline).with_primary(self.span(), format!("expected {what}")),
        );
    }

    // ---- program / items ----

    fn parse_program(&mut self) -> Ast {
        let mut items = Vec::new();
        while !self.at_eof() {
            let before = self.pos;
            match self.parse_item() {
                Some(item) => items.push(item),
                None => {
                    self.recover_to_item();
                    // Guarantee progress even if recovery found nothing to skip.
                    if self.pos == before && !self.at_eof() {
                        self.bump();
                    }
                }
            }
        }
        Ast { items }
    }

    fn parse_item(&mut self) -> Option<Item> {
        match self.kind() {
            TokenKind::Fn => self.parse_fn(),
            TokenKind::Const => self.parse_const_item(),
            TokenKind::Struct => self.parse_struct_item(),
            _ => {
                self.error_expected("an item (`fn`, `const`, or `struct`)");
                None
            }
        }
    }

    fn parse_struct_item(&mut self) -> Option<Item> {
        let start = self.span();
        self.bump(); // `struct`
        let name = self.parse_ident()?;
        self.expect(&TokenKind::LBrace)?;
        let mut fields = Vec::new();
        while !self.at(&TokenKind::RBrace) && !self.at_eof() {
            let field_name = self.parse_ident()?;
            self.expect(&TokenKind::Colon)?;
            let ty = self.parse_type();
            let span = field_name.span.to(ty.span);
            fields.push(FieldDef {
                name: field_name,
                ty,
                span,
            });
            if self.eat(&TokenKind::Comma).is_none() {
                break;
            }
        }
        let close = self.expect(&TokenKind::RBrace)?;
        let span = start.to(close.span);
        Some(Item {
            id: self.fresh_id(),
            kind: ItemKind::Struct(StructDecl {
                id: self.fresh_id(),
                name,
                fields,
            }),
            span,
        })
    }

    fn parse_const_item(&mut self) -> Option<Item> {
        let start = self.span();
        self.bump(); // `const`
        let name = self.parse_ident()?;
        self.expect(&TokenKind::Colon)?;
        let ty = self.parse_type();
        self.expect(&TokenKind::Eq)?;
        let value = self.parse_expr()?;
        let semi = self.expect(&TokenKind::Semi)?;
        let span = start.to(semi.span);
        Some(Item {
            id: self.fresh_id(),
            kind: ItemKind::Const(ConstDecl {
                id: self.fresh_id(),
                name,
                ty,
                value,
            }),
            span,
        })
    }

    fn parse_fn(&mut self) -> Option<Item> {
        let start = self.span();
        self.bump(); // `fn`
        let name = self.parse_ident()?;
        self.expect(&TokenKind::LParen)?;
        let params = self.parse_params()?;
        self.expect(&TokenKind::RParen)?;
        let ret = if self.eat(&TokenKind::Arrow).is_some() {
            Some(self.parse_type())
        } else {
            None
        };
        let body = self.parse_block()?;
        let span = start.to(body.span);
        Some(Item {
            id: self.fresh_id(),
            kind: ItemKind::Fn(FnDecl {
                name,
                params,
                ret,
                body,
            }),
            span,
        })
    }

    fn parse_params(&mut self) -> Option<Vec<Param>> {
        let mut params = Vec::new();
        while !self.at(&TokenKind::RParen) && !self.at_eof() {
            let name = self.parse_ident()?;
            self.expect(&TokenKind::Colon)?;
            let ty = self.parse_type();
            let span = name.span.to(ty.span);
            params.push(Param {
                id: self.fresh_id(),
                name,
                ty,
                span,
            });
            if self.eat(&TokenKind::Comma).is_none() {
                break;
            }
        }
        Some(params)
    }

    fn parse_ident(&mut self) -> Option<Ident> {
        if let TokenKind::Ident(name) = self.kind() {
            let name = name.clone();
            let span = self.span();
            self.bump();
            Some(Ident { name, span })
        } else {
            self.error_expected("an identifier");
            None
        }
    }

    /// Parses a type annotation. Never returns `None`: on a malformed type it
    /// reports an error and yields a [`TypeExprKind::Error`] placeholder so the
    /// surrounding construct can keep parsing.
    fn parse_type(&mut self) -> TypeExpr {
        if self.at(&TokenKind::LBracket) {
            let start = self.span();
            self.bump(); // `[`
            let elem = self.parse_type();
            let close = self
                .expect(&TokenKind::RBracket)
                .map(|t| t.span)
                .unwrap_or(elem.span);
            return TypeExpr {
                kind: TypeExprKind::Array(Box::new(elem)),
                span: start.to(close),
            };
        }
        if self.at(&TokenKind::LParen) {
            let start = self.span();
            self.bump(); // `(`
            let mut elems = Vec::new();
            while !self.at(&TokenKind::RParen) && !self.at_eof() {
                elems.push(self.parse_type());
                if self.eat(&TokenKind::Comma).is_none() {
                    break;
                }
            }
            let close = self
                .expect(&TokenKind::RParen)
                .map(|t| t.span)
                .unwrap_or(start);
            // `(T)` is just `T`; two or more elements form a tuple type.
            return if elems.len() == 1 {
                elems.into_iter().next().unwrap()
            } else {
                TypeExpr {
                    kind: TypeExprKind::Tuple(elems),
                    span: start.to(close),
                }
            };
        }
        if let TokenKind::Ident(name) = self.kind() {
            let name = name.clone();
            let span = self.span();
            self.bump();
            TypeExpr {
                kind: TypeExprKind::Named(name),
                span,
            }
        } else {
            let span = self.span();
            self.diags.emit(
                Diagnostic::error(
                    DiagCode::ExpectedType,
                    format!("expected a type, found {}", self.kind().describe()),
                )
                .with_primary(span, "expected a type name here"),
            );
            TypeExpr {
                kind: TypeExprKind::Error,
                span,
            }
        }
    }

    // ---- blocks & statements ----

    fn parse_block(&mut self) -> Option<Block> {
        let open = self.expect(&TokenKind::LBrace)?;
        // Inside a block, struct literals are unambiguous again (any `{` here
        // would belong to a nested expression, not this block).
        let saved_struct_ok = self.struct_ok;
        self.struct_ok = true;
        let mut stmts = Vec::new();
        let mut tail = None;
        while !self.at(&TokenKind::RBrace) && !self.at_eof() {
            let before = self.pos;
            match self.parse_block_elem() {
                BlockElem::Stmt(s) => stmts.push(s),
                BlockElem::Tail(e) => {
                    tail = Some(Box::new(e));
                    break;
                }
                BlockElem::Error => {
                    self.recover_in_block();
                    if self.pos == before && !self.at_eof() {
                        self.bump();
                    }
                }
            }
        }
        let close = self
            .expect(&TokenKind::RBrace)
            .unwrap_or_else(|| self.cur().clone());
        self.struct_ok = saved_struct_ok;
        Some(Block {
            stmts,
            tail,
            span: open.span.to(close.span),
        })
    }

    fn parse_block_elem(&mut self) -> BlockElem {
        match self.kind() {
            TokenKind::Let => self.parse_let().map_or(BlockElem::Error, BlockElem::Stmt),
            TokenKind::Return => self
                .parse_return()
                .map_or(BlockElem::Error, BlockElem::Stmt),
            TokenKind::While => self.parse_while().map_or(BlockElem::Error, BlockElem::Stmt),
            TokenKind::For => self.parse_for().map_or(BlockElem::Error, BlockElem::Stmt),
            TokenKind::Break => self
                .parse_loop_jump(TokenKind::Break, StmtKind::Break)
                .map_or(BlockElem::Error, BlockElem::Stmt),
            TokenKind::Continue => self
                .parse_loop_jump(TokenKind::Continue, StmtKind::Continue)
                .map_or(BlockElem::Error, BlockElem::Stmt),
            _ => self.parse_expr_stmt_or_tail(),
        }
    }

    fn parse_let(&mut self) -> Option<Stmt> {
        let start = self.span();
        self.bump(); // `let`
        let mutable = self.eat(&TokenKind::Mut).is_some();
        let name = self.parse_ident()?;
        let ty = if self.eat(&TokenKind::Colon).is_some() {
            Some(self.parse_type())
        } else {
            None
        };
        self.expect(&TokenKind::Eq)?;
        let init = self.parse_expr()?;
        let semi = self.expect(&TokenKind::Semi)?;
        let span = start.to(semi.span);
        Some(Stmt {
            kind: StmtKind::Let(LetStmt {
                id: self.fresh_id(),
                name,
                mutable,
                ty,
                init,
            }),
            span,
        })
    }

    fn parse_return(&mut self) -> Option<Stmt> {
        let start = self.span();
        self.bump(); // `return`
        // `return;` returns unit; otherwise an expression precedes the `;`.
        let value = if self.at(&TokenKind::Semi) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        let semi = self.expect(&TokenKind::Semi)?;
        Some(Stmt {
            kind: StmtKind::Return(value),
            span: start.to(semi.span),
        })
    }

    fn parse_while(&mut self) -> Option<Stmt> {
        let start = self.span();
        self.bump(); // `while`
        let cond = self.with_struct_ok(false, |p| p.parse_expr())?;
        let body = self.parse_block()?;
        let span = start.to(body.span);
        Some(Stmt {
            kind: StmtKind::While(WhileStmt { cond, body }),
            span,
        })
    }

    fn parse_for(&mut self) -> Option<Stmt> {
        let start = self.span();
        self.bump(); // `for`
        let var = self.parse_ident()?;
        self.expect(&TokenKind::In)?;
        let first = self.with_struct_ok(false, |p| p.parse_expr())?;
        // `for v in start..end` is a range loop; `for v in expr` iterates the
        // elements of an array. The `..` after the first expression decides.
        if self.eat(&TokenKind::DotDot).is_some() {
            let range_end = self.with_struct_ok(false, |p| p.parse_expr())?;
            let body = self.parse_block()?;
            let span = start.to(body.span);
            return Some(Stmt {
                kind: StmtKind::For(ForStmt {
                    id: self.fresh_id(),
                    var,
                    start: first,
                    end: range_end,
                    body,
                }),
                span,
            });
        }
        let body = self.parse_block()?;
        let span = start.to(body.span);
        Some(Stmt {
            kind: StmtKind::ForEach(ForEachStmt {
                id: self.fresh_id(),
                var,
                iterable: first,
                body,
            }),
            span,
        })
    }

    /// Parses a `break;` or `continue;` statement.
    fn parse_loop_jump(&mut self, keyword: TokenKind, kind: StmtKind) -> Option<Stmt> {
        let start = self.span();
        self.bump(); // `break` / `continue`
        let semi = self.expect(&TokenKind::Semi)?;
        debug_assert!(matches!(keyword, TokenKind::Break | TokenKind::Continue));
        Some(Stmt {
            kind,
            span: start.to(semi.span),
        })
    }

    /// Parses an expression in statement position and decides whether it is a
    /// statement (`expr;` or a block-like expression) or the block's tail value.
    fn parse_expr_stmt_or_tail(&mut self) -> BlockElem {
        let Some(expr) = self.parse_expr() else {
            return BlockElem::Error;
        };
        if let Some(semi) = self.eat(&TokenKind::Semi) {
            let span = expr.span.to(semi.span);
            BlockElem::Stmt(Stmt {
                kind: StmtKind::Expr(expr),
                span,
            })
        } else if self.at(&TokenKind::RBrace) {
            BlockElem::Tail(expr)
        } else if expr.is_block_like() {
            // `if …` / `{ … }` may stand alone as a statement, like in Rust.
            let span = expr.span;
            BlockElem::Stmt(Stmt {
                kind: StmtKind::Expr(expr),
                span,
            })
        } else {
            self.error_expected("`;` or `}`");
            BlockElem::Error
        }
    }

    // ---- expressions (Pratt) ----

    fn parse_expr(&mut self) -> Option<Expr> {
        self.parse_assign()
    }

    /// Assignment is the lowest-precedence, right-associative expression form.
    /// Plain `=` and compound `+=`/`-=`/`*=`/`/=`/`%=` are handled here.
    fn parse_assign(&mut self) -> Option<Expr> {
        let lhs = self.parse_bp(0)?;
        if self.at(&TokenKind::Eq) {
            self.bump(); // `=`
            let value = self.parse_assign()?;
            let span = lhs.span.to(value.span);
            return Some(self.mk_expr(
                ExprKind::Assign {
                    target: Box::new(lhs),
                    value: Box::new(value),
                },
                span,
            ));
        }
        if let Some(op) = compound_assign_op(self.kind()) {
            self.bump(); // `+=` etc.
            let value = self.parse_assign()?;
            let span = lhs.span.to(value.span);
            return Some(self.mk_expr(
                ExprKind::AssignOp {
                    target: Box::new(lhs),
                    op,
                    value: Box::new(value),
                },
                span,
            ));
        }
        Some(lhs)
    }

    /// Precedence-climbing core: parses binary operators whose left binding
    /// power is at least `min_bp`.
    fn parse_bp(&mut self, min_bp: u8) -> Option<Expr> {
        let mut lhs = self.parse_unary()?;
        while let Some(op) = peek_binop(self.kind()) {
            let (lbp, rbp) = binding_power(op);
            if lbp < min_bp {
                break;
            }
            self.bump(); // operator
            let rhs = self.parse_bp(rbp)?;
            let span = lhs.span.to(rhs.span);
            lhs = self.mk_expr(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            );
        }
        Some(lhs)
    }

    fn parse_unary(&mut self) -> Option<Expr> {
        let op = match self.kind() {
            TokenKind::Minus => UnOp::Neg,
            TokenKind::Bang => UnOp::Not,
            _ => return self.parse_call(),
        };
        let start = self.span();
        self.bump();
        let rhs = self.parse_unary()?;
        let span = start.to(rhs.span);
        Some(self.mk_expr(
            ExprKind::Unary {
                op,
                rhs: Box::new(rhs),
            },
            span,
        ))
    }

    /// Parses postfix calls `f(...)`, indexing `a[i]`, and field access `a.b`,
    /// which chain freely.
    fn parse_call(&mut self) -> Option<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.at(&TokenKind::LParen) {
                self.bump(); // `(`
                let mut args = Vec::new();
                while !self.at(&TokenKind::RParen) && !self.at_eof() {
                    args.push(self.with_struct_ok(true, |p| p.parse_expr())?);
                    if self.eat(&TokenKind::Comma).is_none() {
                        break;
                    }
                }
                let close = self.expect(&TokenKind::RParen)?;
                let span = expr.span.to(close.span);
                expr = self.mk_expr(
                    ExprKind::Call {
                        callee: Box::new(expr),
                        args,
                    },
                    span,
                );
            } else if self.at(&TokenKind::LBracket) {
                self.bump(); // `[`
                let index = self.with_struct_ok(true, |p| p.parse_expr())?;
                let close = self.expect(&TokenKind::RBracket)?;
                let span = expr.span.to(close.span);
                expr = self.mk_expr(
                    ExprKind::Index {
                        base: Box::new(expr),
                        index: Box::new(index),
                    },
                    span,
                );
            } else if self.at(&TokenKind::Dot) {
                self.bump(); // `.`
                // `.0` is a tuple index; `.name` is a struct field access.
                if let TokenKind::Int(n) = self.kind() {
                    let index = (*n).max(0) as usize;
                    let index_span = self.span();
                    self.bump();
                    let span = expr.span.to(index_span);
                    expr = self.mk_expr(
                        ExprKind::TupleIndex {
                            base: Box::new(expr),
                            index,
                            index_span,
                        },
                        span,
                    );
                } else {
                    let field = self.parse_ident()?;
                    let span = expr.span.to(field.span);
                    expr = self.mk_expr(
                        ExprKind::Field {
                            base: Box::new(expr),
                            field,
                        },
                        span,
                    );
                }
            } else {
                break;
            }
        }
        Some(expr)
    }

    fn parse_primary(&mut self) -> Option<Expr> {
        let span = self.span();
        let kind = match self.kind() {
            TokenKind::Int(v) => ExprKind::Int(*v),
            TokenKind::Float(v) => ExprKind::Float(*v),
            TokenKind::Str(s) => ExprKind::Str(s.clone()),
            TokenKind::True => ExprKind::Bool(true),
            TokenKind::False => ExprKind::Bool(false),
            // `Name { ... }` is a struct literal, but only where allowed (not in
            // a condition position, where `{` starts a block).
            TokenKind::Ident(_)
                if self.struct_ok && matches!(self.nth_kind(1), TokenKind::LBrace) =>
            {
                return self.parse_struct_lit();
            }
            TokenKind::Ident(name) => ExprKind::Name(name.clone()),
            TokenKind::LParen => return self.parse_grouping(),
            TokenKind::LBrace => {
                let block = self.parse_block()?;
                let span = block.span;
                return Some(self.mk_expr(ExprKind::Block(block), span));
            }
            TokenKind::If => return self.parse_if(),
            TokenKind::Match => return self.parse_match(),
            TokenKind::LBracket => return self.parse_array_lit(),
            _ => {
                self.error_expected("an expression");
                return None;
            }
        };
        self.bump();
        Some(self.mk_expr(kind, span))
    }

    fn parse_array_lit(&mut self) -> Option<Expr> {
        let start = self.span();
        self.bump(); // `[`
        let mut elems = Vec::new();
        while !self.at(&TokenKind::RBracket) && !self.at_eof() {
            elems.push(self.with_struct_ok(true, |p| p.parse_expr())?);
            if self.eat(&TokenKind::Comma).is_none() {
                break;
            }
        }
        let close = self.expect(&TokenKind::RBracket)?;
        let span = start.to(close.span);
        Some(self.mk_expr(ExprKind::ArrayLit(elems), span))
    }

    /// Parses `Name { field: value, ... }`.
    fn parse_struct_lit(&mut self) -> Option<Expr> {
        let name = self.parse_ident()?;
        self.expect(&TokenKind::LBrace)?;
        let mut fields = Vec::new();
        while !self.at(&TokenKind::RBrace) && !self.at_eof() {
            let field_name = self.parse_ident()?;
            self.expect(&TokenKind::Colon)?;
            let value = self.with_struct_ok(true, |p| p.parse_expr())?;
            fields.push(FieldInit {
                name: field_name,
                value,
            });
            if self.eat(&TokenKind::Comma).is_none() {
                break;
            }
        }
        let close = self.expect(&TokenKind::RBrace)?;
        let span = name.span.to(close.span);
        Some(self.mk_expr(ExprKind::StructLit { name, fields }, span))
    }

    /// Parses `( ... )`: either a parenthesised expression (one element) or a
    /// tuple literal (two or more).
    fn parse_grouping(&mut self) -> Option<Expr> {
        let start = self.span();
        self.bump(); // `(`
        let mut elems = Vec::new();
        let mut saw_comma = false;
        while !self.at(&TokenKind::RParen) && !self.at_eof() {
            elems.push(self.with_struct_ok(true, |p| p.parse_expr())?);
            if self.eat(&TokenKind::Comma).is_some() {
                saw_comma = true;
            } else {
                break;
            }
        }
        let close = self.expect(&TokenKind::RParen)?;
        if elems.len() == 1 && !saw_comma {
            // `(e)` is just a grouped expression.
            return Some(elems.into_iter().next().unwrap());
        }
        let span = start.to(close.span);
        Some(self.mk_expr(ExprKind::TupleLit(elems), span))
    }

    fn parse_if(&mut self) -> Option<Expr> {
        let start = self.span();
        self.bump(); // `if`
        let cond = self.with_struct_ok(false, |p| p.parse_expr())?;
        let then_branch = self.parse_block()?;
        let mut end = then_branch.span;
        let else_branch = if self.eat(&TokenKind::Else).is_some() {
            let e = if self.at(&TokenKind::If) {
                self.parse_if()?
            } else {
                let block = self.parse_block()?;
                let span = block.span;
                self.mk_expr(ExprKind::Block(block), span)
            };
            end = e.span;
            Some(Box::new(e))
        } else {
            None
        };
        let span = start.to(end);
        Some(self.mk_expr(
            ExprKind::If(IfExpr {
                cond: Box::new(cond),
                then_branch,
                else_branch,
            }),
            span,
        ))
    }

    /// Parses `match scrutinee { pat => body, ... }`.
    fn parse_match(&mut self) -> Option<Expr> {
        let start = self.span();
        self.bump(); // `match`
        let scrutinee = self.with_struct_ok(false, |p| p.parse_expr())?;
        self.expect(&TokenKind::LBrace)?;
        let mut arms = Vec::new();
        while !self.at(&TokenKind::RBrace) && !self.at_eof() {
            let arm_start = self.span();
            let pattern = self.parse_pattern()?;
            self.expect(&TokenKind::FatArrow)?;
            let body = self.with_struct_ok(true, |p| p.parse_expr())?;
            let span = arm_start.to(body.span);
            let block_like = matches!(
                body.kind,
                ExprKind::Block(_) | ExprKind::If(_) | ExprKind::Match(_)
            );
            arms.push(MatchArm {
                pattern,
                body,
                span,
            });
            // A comma separates arms. It is optional after a block-like body
            // (`{ ... }`, `if`, `match`), matching Rust, and after the final arm.
            if self.eat(&TokenKind::Comma).is_none() && !block_like {
                break;
            }
        }
        let close = self.expect(&TokenKind::RBrace)?;
        let span = start.to(close.span);
        Some(self.mk_expr(
            ExprKind::Match(MatchExpr {
                scrutinee: Box::new(scrutinee),
                arms,
            }),
            span,
        ))
    }

    /// Parses a single `match` arm pattern: a scalar literal or `_`.
    fn parse_pattern(&mut self) -> Option<Pattern> {
        match self.kind() {
            TokenKind::Int(v) => {
                let v = *v;
                self.bump();
                Some(Pattern::Int(v))
            }
            TokenKind::Minus => {
                self.bump();
                match self.kind() {
                    TokenKind::Int(v) => {
                        let v = *v;
                        self.bump();
                        Some(Pattern::Int(-v))
                    }
                    _ => {
                        self.error_expected("an integer after `-`");
                        None
                    }
                }
            }
            TokenKind::True => {
                self.bump();
                Some(Pattern::Bool(true))
            }
            TokenKind::False => {
                self.bump();
                Some(Pattern::Bool(false))
            }
            TokenKind::Ident(name) if name == "_" => {
                self.bump();
                Some(Pattern::Wild)
            }
            _ => {
                self.error_expected("a pattern (a literal or `_`)");
                None
            }
        }
    }

    // ---- recovery ----

    /// Skips tokens until the next top-level item boundary (`fn`, `const`, EOF).
    fn recover_to_item(&mut self) {
        while !self.at_eof() && !self.at(&TokenKind::Fn) && !self.at(&TokenKind::Const) {
            self.bump();
        }
    }

    /// Skips to the end of the current statement: past the next `;`, or up to
    /// the closing `}` of the block.
    fn recover_in_block(&mut self) {
        while !self.at_eof() && !self.at(&TokenKind::RBrace) {
            let was_semi = self.at(&TokenKind::Semi);
            self.bump();
            if was_semi {
                break;
            }
        }
    }
}

/// Maps a compound-assignment token (`+=` …) to its underlying binary operator.
fn compound_assign_op(kind: &TokenKind) -> Option<BinOp> {
    Some(match kind {
        TokenKind::PlusEq => BinOp::Add,
        TokenKind::MinusEq => BinOp::Sub,
        TokenKind::StarEq => BinOp::Mul,
        TokenKind::SlashEq => BinOp::Div,
        TokenKind::PercentEq => BinOp::Rem,
        _ => return None,
    })
}

/// Maps a token to the binary operator it introduces, if any.
fn peek_binop(kind: &TokenKind) -> Option<BinOp> {
    Some(match kind {
        TokenKind::PipePipe => BinOp::Or,
        TokenKind::AmpAmp => BinOp::And,
        TokenKind::EqEq => BinOp::Eq,
        TokenKind::BangEq => BinOp::Ne,
        TokenKind::Lt => BinOp::Lt,
        TokenKind::LtEq => BinOp::Le,
        TokenKind::Gt => BinOp::Gt,
        TokenKind::GtEq => BinOp::Ge,
        TokenKind::Plus => BinOp::Add,
        TokenKind::Minus => BinOp::Sub,
        TokenKind::Star => BinOp::Mul,
        TokenKind::Slash => BinOp::Div,
        TokenKind::Percent => BinOp::Rem,
        _ => return None,
    })
}

/// The (left, right) binding powers of a binary operator. All operators here
/// are left-associative, encoded as `left < right`. Higher binds tighter.
fn binding_power(op: BinOp) -> (u8, u8) {
    use BinOp::*;
    let level: u8 = match op {
        Or => 1,
        And => 2,
        Eq | Ne => 3,
        Lt | Le | Gt | Ge => 4,
        Add | Sub => 5,
        Mul | Div | Rem => 6,
    };
    (level * 2, level * 2 + 1)
}

#[cfg(test)]
mod tests;

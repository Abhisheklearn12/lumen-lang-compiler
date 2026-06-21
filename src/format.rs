//! The source formatter: turns an [`Ast`] back into canonical Lumen source.
//!
//! This powers `lumenc fmt`. Unlike the AST *debug* printer
//! ([`parser::print`](crate::parser::print)), which emits a tree for inspection,
//! the formatter emits **valid, re-parseable Lumen code** in a single canonical
//! style: four-space indentation, one statement per line, spaces around binary
//! operators, and minimal parentheses driven by operator precedence.
//!
//! Formatting is idempotent - running it on already-formatted output is a
//! no-op - and round-trips: formatting, parsing, and formatting again yields the
//! same text. Both properties are covered by tests.

use std::fmt::Write as _;

use crate::parser::ast::*;

/// Formats a whole program to canonical source text.
pub fn format_source(ast: &Ast) -> String {
    let mut f = Formatter {
        out: String::new(),
        depth: 0,
    };
    for (i, item) in ast.items.iter().enumerate() {
        if i > 0 {
            f.out.push('\n');
        }
        f.item(item);
    }
    f.out
}

struct Formatter {
    out: String,
    depth: usize,
}

/// The precedence level of a binary operator, used to parenthesise minimally.
/// Higher binds tighter; mirrors the parser's `binding_power`.
fn precedence(op: BinOp) -> u8 {
    use BinOp::*;
    match op {
        Or => 1,
        And => 2,
        Eq | Ne => 3,
        Lt | Le | Gt | Ge => 4,
        Add | Sub => 5,
        Mul | Div | Rem => 6,
    }
}

impl Formatter {
    fn indent(&mut self) {
        for _ in 0..self.depth {
            self.out.push_str("    ");
        }
    }

    fn line(&mut self, text: &str) {
        self.indent();
        self.out.push_str(text);
        self.out.push('\n');
    }

    // ---- items ----

    fn item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Fn(decl) => self.function(decl),
            ItemKind::Const(decl) => {
                let value = self.expr_to_string(&decl.value, 0);
                self.line(&format!(
                    "const {}: {} = {value};",
                    decl.name.name,
                    type_str(&decl.ty)
                ));
            }
            ItemKind::Struct(decl) => {
                self.line(&format!("struct {} {{", decl.name.name));
                self.depth += 1;
                for field in &decl.fields {
                    self.line(&format!("{}: {},", field.name.name, type_str(&field.ty)));
                }
                self.depth -= 1;
                self.line("}");
            }
        }
    }

    fn function(&mut self, decl: &FnDecl) {
        let params = decl
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name.name, type_str(&p.ty)))
            .collect::<Vec<_>>()
            .join(", ");
        let ret = match &decl.ret {
            Some(ty) => format!(" -> {}", type_str(ty)),
            None => String::new(),
        };
        self.indent();
        let _ = write!(self.out, "fn {}({params}){ret} ", decl.name.name);
        self.block(&decl.body);
        self.out.push('\n');
    }

    // ---- blocks & statements ----

    /// Writes a block starting at the current cursor (after `… `) and ending
    /// with the closing brace on its own line.
    fn block(&mut self, block: &Block) {
        if block.stmts.is_empty() && block.tail.is_none() {
            self.out.push_str("{}");
            return;
        }
        self.out.push_str("{\n");
        self.depth += 1;
        for stmt in &block.stmts {
            self.stmt(stmt);
        }
        if let Some(tail) = &block.tail {
            let text = self.expr_to_string(tail, 0);
            self.line(&text);
        }
        self.depth -= 1;
        self.indent();
        self.out.push('}');
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let(l) => {
                let kw = if l.mutable { "let mut" } else { "let" };
                let ty =
                    l.ty.as_ref()
                        .map(|t| format!(": {}", type_str(t)))
                        .unwrap_or_default();
                let value = self.expr_to_string(&l.init, 0);
                self.line(&format!("{kw} {}{ty} = {value};", l.name.name));
            }
            StmtKind::Expr(e) => {
                let text = self.expr_to_string(e, 0);
                self.line(&format!("{text};"));
            }
            StmtKind::Return(Some(e)) => {
                let text = self.expr_to_string(e, 0);
                self.line(&format!("return {text};"));
            }
            StmtKind::Return(None) => self.line("return;"),
            StmtKind::While(w) => {
                let cond = self.expr_to_string(&w.cond, 0);
                self.indent();
                let _ = write!(self.out, "while {cond} ");
                self.block(&w.body);
                self.out.push('\n');
            }
            StmtKind::For(fr) => {
                let start = self.expr_to_string(&fr.start, 0);
                let end = self.expr_to_string(&fr.end, 0);
                self.indent();
                let _ = write!(self.out, "for {} in {start}..{end} ", fr.var.name);
                self.block(&fr.body);
                self.out.push('\n');
            }
            StmtKind::ForEach(fe) => {
                let iter = self.expr_to_string(&fe.iterable, 0);
                self.indent();
                let _ = write!(self.out, "for {} in {iter} ", fe.var.name);
                self.block(&fe.body);
                self.out.push('\n');
            }
            StmtKind::Break => self.line("break;"),
            StmtKind::Continue => self.line("continue;"),
        }
    }

    // ---- expressions ----

    /// Renders a block expression (used inside `if`/block tails) at the current
    /// indentation, returning the text.
    fn block_to_string(&mut self, block: &Block) -> String {
        let saved = std::mem::take(&mut self.out);
        self.block(block);
        std::mem::replace(&mut self.out, saved)
    }

    /// Formats an expression to a string. `parent_prec` is the precedence of the
    /// enclosing binary operator (0 at the top), used to add parentheses only
    /// when needed to preserve the parse.
    fn expr_to_string(&mut self, expr: &Expr, parent_prec: u8) -> String {
        match &expr.kind {
            ExprKind::Int(v) => v.to_string(),
            ExprKind::Float(v) => format_float(*v),
            ExprKind::Bool(v) => v.to_string(),
            ExprKind::Str(s) => format!("{s:?}"),
            ExprKind::Name(n) => n.clone(),
            ExprKind::Unary { op, rhs } => {
                format!("{}{}", op.symbol(), self.expr_to_string(rhs, 7))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let prec = precedence(*op);
                let l = self.expr_to_string(lhs, prec);
                let r = self.expr_to_string(rhs, prec + 1);
                let text = format!("{l} {} {r}", op.symbol());
                if prec < parent_prec {
                    format!("({text})")
                } else {
                    text
                }
            }
            ExprKind::Call { callee, args } => {
                let callee = self.expr_to_string(callee, 8);
                let args = args
                    .iter()
                    .map(|a| self.expr_to_string(a, 0))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{callee}({args})")
            }
            ExprKind::Index { base, index } => {
                let base = self.expr_to_string(base, 8);
                let index = self.expr_to_string(index, 0);
                format!("{base}[{index}]")
            }
            ExprKind::ArrayLit(elems) => {
                let items = elems
                    .iter()
                    .map(|e| self.expr_to_string(e, 0))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[{items}]")
            }
            ExprKind::Assign { target, value } => {
                format!(
                    "{} = {}",
                    self.expr_to_string(target, 0),
                    self.expr_to_string(value, 0)
                )
            }
            ExprKind::AssignOp { target, op, value } => {
                format!(
                    "{} {}= {}",
                    self.expr_to_string(target, 0),
                    op.symbol(),
                    self.expr_to_string(value, 0)
                )
            }
            ExprKind::StructLit { name, fields } => {
                let parts = fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name.name, self.expr_to_string(&f.value, 0)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} {{ {parts} }}", name.name)
            }
            ExprKind::Field { base, field } => {
                format!("{}.{}", self.expr_to_string(base, 8), field.name)
            }
            ExprKind::TupleLit(elems) => {
                let parts = elems
                    .iter()
                    .map(|e| self.expr_to_string(e, 0))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({parts})")
            }
            ExprKind::TupleIndex { base, index, .. } => {
                format!("{}.{index}", self.expr_to_string(base, 8))
            }
            ExprKind::If(if_expr) => self.if_to_string(if_expr),
            ExprKind::Match(m) => self.match_to_string(m),
            ExprKind::Block(block) => self.block_to_string(block),
        }
    }

    fn match_to_string(&mut self, m: &MatchExpr) -> String {
        let scrut = self.expr_to_string(&m.scrutinee, 0);
        let mut text = format!("match {scrut} {{\n");
        self.depth += 1;
        for arm in &m.arms {
            let pat = pattern_str(&arm.pattern);
            let body = self.expr_to_string(&arm.body, 0);
            for _ in 0..self.depth {
                text.push_str("    ");
            }
            let _ = writeln!(text, "{pat} => {body},");
        }
        self.depth -= 1;
        for _ in 0..self.depth {
            text.push_str("    ");
        }
        text.push('}');
        text
    }

    fn if_to_string(&mut self, if_expr: &IfExpr) -> String {
        let cond = self.expr_to_string(&if_expr.cond, 0);
        let then = self.block_to_string(&if_expr.then_branch);
        let mut text = format!("if {cond} {then}");
        if let Some(else_branch) = &if_expr.else_branch {
            let else_text = self.expr_to_string(else_branch, 0);
            let _ = write!(text, " else {else_text}");
        }
        text
    }
}

/// Renders a `match` arm pattern to source form.
fn pattern_str(pattern: &Pattern) -> String {
    match pattern {
        Pattern::Int(v) => v.to_string(),
        Pattern::Bool(b) => b.to_string(),
        Pattern::Wild => "_".to_string(),
    }
}

/// Renders a syntactic type to source form.
fn type_str(ty: &TypeExpr) -> String {
    match &ty.kind {
        TypeExprKind::Named(name) => name.clone(),
        TypeExprKind::Array(inner) => format!("[{}]", type_str(inner)),
        TypeExprKind::Tuple(elems) => {
            format!(
                "({})",
                elems.iter().map(type_str).collect::<Vec<_>>().join(", ")
            )
        }
        TypeExprKind::Error => "?".to_string(),
    }
}

/// Formats a float so it always round-trips and always reads as a float (a
/// trailing `.0` is added to whole numbers, which would otherwise lex as `int`).
fn format_float(v: f64) -> String {
    let s = v.to_string();
    if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") {
        s
    } else {
        format!("{s}.0")
    }
}

#[cfg(test)]
mod tests;

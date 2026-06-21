//! Type checking: assigns a [`Type`] to every expression and binding, and
//! verifies the program is well-typed.
//!
//! # Design
//!
//! The checker runs after [resolution](super::resolve), so every name already
//! points at a definition. It is a straightforward bidirectional-ish walk:
//! expressions are *synthesised* bottom-up, and a handful of positions
//! (`let` annotations, `return`, call arguments, conditions) *check* a
//! synthesised type against an expected one.
//!
//! Lumen has no implicit conversions: `i64` and `f64` never mix, and operators
//! require matching operand types. This keeps the rules  and their
//! diagnostics  simple and predictable.
//!
//! Like every phase, it reports as much as it can: a type error yields a
//! diagnostic and a [`Type::Error`] result, which is compatible with everything
//! and so suppresses cascading errors.
//!
//! # Output
//!
//! A [`Typeck`] of side tables: `expr_types` (per expression), `local_types`
//! (per `let`/parameter definition), and `signatures` (per [`FnId`]). HIR
//! lowering reads these to build its fully-typed tree.

use std::collections::HashMap;

use crate::diagnostics::{Diagnostic, Diagnostics};
use crate::errors::DiagCode;
use crate::parser::ast::*;
use crate::sema::resolve::{ConstId, FnId, Res, Resolution, StructId};
use crate::sema::types::{Builtin, Elem, Type};
use crate::span::Span;

/// The resolved layout of a declared struct: its name and ordered fields.
#[derive(Clone, Debug)]
pub struct StructInfo {
    pub name: String,
    /// Fields in declaration order, paired with their resolved types.
    pub fields: Vec<(String, Type)>,
}

impl StructInfo {
    /// The index and type of a field by name, if it exists.
    pub fn field(&self, name: &str) -> Option<(usize, Type)> {
        self.fields
            .iter()
            .position(|(n, _)| n == name)
            .map(|i| (i, self.fields[i].1))
    }
}

/// The resolved signature of a user function.
#[derive(Clone, Debug)]
pub struct FnSig {
    pub name: String,
    pub params: Vec<Type>,
    pub ret: Type,
}

/// The compile-time value of a top-level constant, inlined at each use.
#[derive(Clone, Debug, PartialEq)]
pub enum ConstValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

impl ConstValue {
    /// The type of this constant value.
    pub fn ty(&self) -> Type {
        match self {
            ConstValue::Int(_) => Type::Int,
            ConstValue::Float(_) => Type::Float,
            ConstValue::Bool(_) => Type::Bool,
            ConstValue::Str(_) => Type::Str,
        }
    }
}

/// The product of type checking: types for every node, plus signatures.
#[derive(Debug, Default)]
pub struct Typeck {
    /// Type of each expression, keyed by its [`NodeId`].
    pub expr_types: HashMap<NodeId, Type>,
    /// Type of each binding, keyed by its definition's [`NodeId`].
    pub local_types: HashMap<NodeId, Type>,
    /// Signatures indexed by [`FnId`].
    pub signatures: Vec<FnSig>,
    /// Declared type of each constant, indexed by [`ConstId`].
    pub const_types: Vec<Type>,
    /// Evaluated value of each constant, indexed by [`ConstId`].
    pub const_values: Vec<ConstValue>,
    /// Layout of each struct, indexed by [`StructId`].
    pub structs: Vec<StructInfo>,
    /// Element types of each interned tuple type, indexed by the `u32` carried
    /// in [`Type::Tuple`]. Tuple types are structural, so identical element
    /// lists share one index.
    pub tuple_types: Vec<Vec<Type>>,
    /// The `main` entry point, if the program has a valid one.
    pub main: Option<FnId>,
}

impl Typeck {
    /// The type assigned to an expression. Defaults to [`Type::Error`] if the
    /// expression was never typed (only possible on malformed input).
    pub fn type_of(&self, id: NodeId) -> Type {
        self.expr_types.get(&id).copied().unwrap_or(Type::Error)
    }

    /// The type of a binding (`let`/parameter) by its definition id.
    pub fn local_type(&self, id: NodeId) -> Type {
        self.local_types.get(&id).copied().unwrap_or(Type::Error)
    }

    /// The signature of a function.
    pub fn signature(&self, id: FnId) -> &FnSig {
        &self.signatures[id.0 as usize]
    }

    /// The declared type of a constant.
    pub fn const_type(&self, id: ConstId) -> Type {
        self.const_types
            .get(id.0 as usize)
            .copied()
            .unwrap_or(Type::Error)
    }

    /// The evaluated value of a constant.
    pub fn const_value(&self, id: ConstId) -> &ConstValue {
        &self.const_values[id.0 as usize]
    }

    /// The layout of a struct.
    pub fn struct_info(&self, id: StructId) -> &StructInfo {
        &self.structs[id.0 as usize]
    }

    /// The element types of an interned tuple type.
    pub fn tuple_elems(&self, id: u32) -> &[Type] {
        &self.tuple_types[id as usize]
    }

    /// Interns a tuple type, returning the index identifying it. Structurally
    /// identical tuples share an index, so [`Type::Tuple`] comparison is correct.
    fn intern_tuple(&mut self, elems: Vec<Type>) -> u32 {
        if let Some(pos) = self.tuple_types.iter().position(|t| *t == elems) {
            return pos as u32;
        }
        self.tuple_types.push(elems);
        (self.tuple_types.len() - 1) as u32
    }
}

/// Type-checks `ast` using the [`Resolution`], reporting type errors to `diags`.
#[tracing::instrument(level = "debug", skip_all)]
pub fn check(ast: &Ast, res: &Resolution, diags: &mut Diagnostics) -> Typeck {
    let mut checker = Checker {
        ast,
        res,
        diags,
        tc: Typeck::default(),
        cur_ret: Type::Unit,
        loop_depth: 0,
    };
    checker.collect_structs();
    checker.collect_signatures();
    checker.check_consts();
    checker.check_main();
    for (fn_id, item_idx) in res.functions.iter() {
        if let ItemKind::Fn(decl) = &ast.items[item_idx].kind {
            checker.check_fn(fn_id, decl);
        }
    }
    tracing::debug!(
        typed_exprs = checker.tc.expr_types.len(),
        "type checking complete"
    );
    checker.tc
}

struct Checker<'a> {
    ast: &'a Ast,
    res: &'a Resolution,
    diags: &'a mut Diagnostics,
    tc: Typeck,
    /// Return type of the function currently being checked.
    cur_ret: Type,
    /// Nesting depth of enclosing loops, for validating `break`/`continue`.
    loop_depth: u32,
}

impl Checker<'_> {
    // ---- signatures ----

    /// Resolves every function's parameter and return types up front, so that
    /// calls can be checked regardless of declaration order.
    fn collect_signatures(&mut self) {
        for (_fn_id, item_idx) in self.res.functions.iter() {
            let ItemKind::Fn(decl) = &self.ast.items[item_idx].kind else {
                continue;
            };
            let params: Vec<Type> = decl
                .params
                .iter()
                .map(|p| {
                    let ty = self.resolve_type(&p.ty);
                    self.tc.local_types.insert(p.id, ty);
                    ty
                })
                .collect();
            let ret = decl
                .ret
                .as_ref()
                .map(|t| self.resolve_type(t))
                .unwrap_or(Type::Unit);
            self.tc.signatures.push(FnSig {
                name: decl.name.name.clone(),
                params,
                ret,
            });
        }
    }

    /// Resolves the field types of every struct up front, so struct types can
    /// be named anywhere (including before the struct's own declaration).
    fn collect_structs(&mut self) {
        for (_id, item_idx) in self.res.structs.iter() {
            let ItemKind::Struct(decl) = &self.ast.items[item_idx].kind else {
                continue;
            };
            let mut fields = Vec::with_capacity(decl.fields.len());
            let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for field in &decl.fields {
                if !seen.insert(&field.name.name) {
                    self.diags.emit(
                        Diagnostic::error(
                            DiagCode::BadStructLiteral,
                            format!("field `{}` is declared more than once", field.name.name),
                        )
                        .with_primary(field.name.span, "duplicate field"),
                    );
                }
                let ty = self.resolve_type(&field.ty);
                fields.push((field.name.name.clone(), ty));
            }
            self.tc.structs.push(StructInfo {
                name: decl.name.name.clone(),
                fields,
            });
        }
    }

    /// Type-checks and evaluates every top-level constant, in declaration order.
    /// A constant may reference only constants declared before it.
    fn check_consts(&mut self) {
        for (_const_id, item_idx) in self.res.consts.iter() {
            let ItemKind::Const(decl) = &self.ast.items[item_idx].kind else {
                continue;
            };
            let declared = self.resolve_type(&decl.ty);
            let value_ty = self.check_expr(&decl.value);
            if !value_ty.compatible(declared) {
                self.diags.emit(
                    Diagnostic::error(
                        DiagCode::TypeMismatch,
                        format!("mismatched types in `const {}`", decl.name.name),
                    )
                    .with_primary(
                        decl.value.span,
                        format!("expected `{declared}`, found `{value_ty}`"),
                    )
                    .with_label(decl.ty.span, "expected due to this annotation"),
                );
            }
            let value = self.eval_const(&decl.value).unwrap_or_else(|| {
                // Only report a separate "not constant" error when the value was
                // otherwise well-typed (avoids piling onto an existing error).
                if !value_ty.is_error() {
                    self.diags.emit(
                        Diagnostic::error(
                            DiagCode::NotConstant,
                            format!("`const {}` is not a constant expression", decl.name.name),
                        )
                        .with_primary(decl.value.span, "must be evaluable at compile time")
                        .with_note("constants may use literals, operators, and earlier constants"),
                    );
                }
                default_const_value(declared)
            });
            self.tc.const_types.push(declared);
            self.tc.const_values.push(value);
        }
    }

    /// Evaluates a constant expression to a [`ConstValue`], or `None` if it is
    /// not a compile-time constant.
    fn eval_const(&self, expr: &Expr) -> Option<ConstValue> {
        match &expr.kind {
            ExprKind::Int(v) => Some(ConstValue::Int(*v)),
            ExprKind::Float(v) => Some(ConstValue::Float(*v)),
            ExprKind::Bool(v) => Some(ConstValue::Bool(*v)),
            ExprKind::Str(s) => Some(ConstValue::Str(s.clone())),
            ExprKind::Name(_) => match self.res.use_of(expr.id) {
                // A reference to an already-evaluated constant.
                Some(Res::Const(id)) if (id.0 as usize) < self.tc.const_values.len() => {
                    Some(self.tc.const_values[id.0 as usize].clone())
                }
                _ => None,
            },
            ExprKind::Unary { op, rhs } => eval_const_unary(*op, self.eval_const(rhs)?),
            ExprKind::Binary { op, lhs, rhs } => {
                eval_const_binary(*op, self.eval_const(lhs)?, self.eval_const(rhs)?)
            }
            _ => None,
        }
    }

    /// Translates a syntactic [`TypeExpr`] to a semantic [`Type`], reporting
    /// unknown type names.
    fn resolve_type(&mut self, ty: &TypeExpr) -> Type {
        match &ty.kind {
            TypeExprKind::Named(name) => {
                if let Some(ty) = Type::from_name(name) {
                    return ty;
                }
                if let Some(id) = self.res.structs.lookup(name) {
                    return Type::Struct(id.0);
                }
                let mut diag =
                    Diagnostic::error(DiagCode::UnknownType, format!("unknown type `{name}`"))
                        .with_primary(ty.span, "not a known type")
                        .with_help("the built-in types are i64, f64, bool, str, unit");
                const PRIMS: [&str; 5] = ["i64", "f64", "bool", "str", "unit"];
                if let Some(hint) = crate::suggest::closest(name, PRIMS) {
                    diag = diag.with_help(format!("did you mean `{hint}`?"));
                }
                self.diags.emit(diag);
                Type::Error
            }
            TypeExprKind::Array(inner) => {
                let elem_ty = self.resolve_type(inner);
                if elem_ty.is_error() {
                    return Type::Error;
                }
                match Elem::of(elem_ty) {
                    Some(elem) => Type::Array(elem),
                    None => {
                        self.diags.emit(
                            Diagnostic::error(
                                DiagCode::BadArrayType,
                                format!("`{elem_ty}` is not a valid array element type"),
                            )
                            .with_primary(
                                inner.span,
                                "array elements must be i64, f64, bool, or str",
                            ),
                        );
                        Type::Error
                    }
                }
            }
            TypeExprKind::Tuple(elem_exprs) => {
                let elems: Vec<Type> = elem_exprs.iter().map(|e| self.resolve_type(e)).collect();
                if elems.iter().any(|t| t.is_error()) {
                    return Type::Error;
                }
                Type::Tuple(self.tc.intern_tuple(elems))
            }
            // Already reported by the parser; stay silent.
            TypeExprKind::Error => Type::Error,
        }
    }

    /// Verifies the program has a `main` with signature `fn main()`.
    fn check_main(&mut self) {
        match self.res.functions.lookup("main") {
            Some(id) => {
                let sig = &self.tc.signatures[id.0 as usize];
                let ok = sig.params.is_empty() && matches!(sig.ret, Type::Unit);
                if ok {
                    self.tc.main = Some(id);
                } else {
                    let item = &self.ast.items[self.res.functions.item_index(id)];
                    self.diags.emit(
                        Diagnostic::error(DiagCode::BadMain, "`main` has an invalid signature")
                            .with_primary(
                                item.span,
                                "must be `fn main()` taking no arguments and returning unit",
                            ),
                    );
                }
            }
            None => {
                self.diags.emit(
                    Diagnostic::error(DiagCode::BadMain, "program has no `main` function")
                        .with_help("add an entry point: `fn main() { ... }`"),
                );
            }
        }
    }

    // ---- functions / blocks / statements ----

    fn check_fn(&mut self, fn_id: FnId, decl: &FnDecl) {
        self.cur_ret = self.tc.signatures[fn_id.0 as usize].ret;
        let body_ty = self.check_block(&decl.body);

        // If every path through the body ends in an explicit `return`, those
        // returns were checked individually and there is no fall-through value
        // to verify here.
        if block_diverges(&decl.body) {
            return;
        }

        // Otherwise control can fall off the end with the body's tail value
        // (or `unit` if there is no tail), which must match the return type.
        if body_ty.compatible(self.cur_ret) {
            return;
        }
        if matches!(body_ty, Type::Unit) && !matches!(self.cur_ret, Type::Unit) {
            // Fell off the end of a value-returning function.
            self.diags.emit(
                Diagnostic::error(
                    DiagCode::MissingReturn,
                    format!(
                        "function `{}` may reach its end without returning `{}`",
                        decl.name.name, self.cur_ret
                    ),
                )
                .with_primary(decl.body.span, format!("expected `{}` value", self.cur_ret))
                .with_help("add a trailing expression or a `return`"),
            );
        } else {
            let tail_span = decl
                .body
                .tail
                .as_ref()
                .map(|t| t.span)
                .unwrap_or(decl.body.span);
            self.diags.emit(
                Diagnostic::error(
                    DiagCode::ReturnTypeMismatch,
                    format!(
                        "function `{}` should return `{}` but its body has type `{}`",
                        decl.name.name, self.cur_ret, body_ty
                    ),
                )
                .with_primary(
                    tail_span,
                    format!("expected `{}`, found `{}`", self.cur_ret, body_ty),
                ),
            );
        }
    }

    /// Checks a block and returns its type (the tail expression's type, or
    /// `unit` if there is none).
    fn check_block(&mut self, block: &Block) -> Type {
        for stmt in &block.stmts {
            self.check_stmt(stmt);
        }
        match &block.tail {
            Some(tail) => self.check_expr(tail),
            None => Type::Unit,
        }
    }

    fn check_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let(l) => self.check_let(l),
            StmtKind::Expr(e) => {
                self.check_expr(e);
            }
            StmtKind::Return(value) => self.check_return(value.as_ref(), stmt.span),
            StmtKind::While(w) => {
                let cond = self.check_expr(&w.cond);
                self.expect(
                    cond,
                    Type::Bool,
                    w.cond.span,
                    DiagCode::NonBoolCondition,
                    "`while` condition",
                );
                self.loop_depth += 1;
                self.check_block(&w.body);
                self.loop_depth -= 1;
            }
            StmtKind::For(f) => self.check_for(f),
            StmtKind::ForEach(f) => self.check_for_each(f),
            StmtKind::Break => self.check_loop_jump("break", stmt.span),
            StmtKind::Continue => self.check_loop_jump("continue", stmt.span),
        }
    }

    /// Checks `for v in array { ... }`: the iterable must be an array, and the
    /// loop variable takes the array's element type.
    fn check_for_each(&mut self, f: &ForEachStmt) {
        let iter_ty = self.check_expr(&f.iterable);
        let elem = match iter_ty {
            Type::Array(elem) => elem.ty(),
            Type::Error => Type::Error,
            other => {
                self.diags.emit(
                    Diagnostic::error(DiagCode::NotIndexable, format!("`{other}` is not iterable"))
                        .with_primary(f.iterable.span, "`for ... in` needs an array"),
                );
                Type::Error
            }
        };
        self.tc.local_types.insert(f.id, elem);
        self.loop_depth += 1;
        self.check_block(&f.body);
        self.loop_depth -= 1;
    }

    fn check_for(&mut self, f: &ForStmt) {
        // Both bounds must be `i64`; the loop variable is then an `i64`.
        let start = self.check_expr(&f.start);
        self.expect(
            start,
            Type::Int,
            f.start.span,
            DiagCode::TypeMismatch,
            "range start",
        );
        let end = self.check_expr(&f.end);
        self.expect(
            end,
            Type::Int,
            f.end.span,
            DiagCode::TypeMismatch,
            "range end",
        );
        self.tc.local_types.insert(f.id, Type::Int);
        self.loop_depth += 1;
        self.check_block(&f.body);
        self.loop_depth -= 1;
    }

    /// Reports `break`/`continue` used outside any loop.
    fn check_loop_jump(&mut self, kw: &str, span: Span) {
        if self.loop_depth == 0 {
            self.diags.emit(
                Diagnostic::error(
                    DiagCode::BreakOutsideLoop,
                    format!("`{kw}` outside of a loop"),
                )
                .with_primary(span, format!("`{kw}` can only be used inside a loop")),
            );
        }
    }

    fn check_let(&mut self, l: &LetStmt) {
        let init = self.check_expr(&l.init);
        let ty = match &l.ty {
            Some(annot) => {
                let expected = self.resolve_type(annot);
                if !init.compatible(expected) {
                    self.diags.emit(
                        Diagnostic::error(
                            DiagCode::TypeMismatch,
                            format!("mismatched types in `let {}`", l.name.name),
                        )
                        .with_primary(
                            l.init.span,
                            format!("expected `{expected}`, found `{init}`"),
                        )
                        .with_label(annot.span, "expected due to this annotation"),
                    );
                }
                expected
            }
            // No annotation: infer from the initialiser.
            None => init,
        };
        self.tc.local_types.insert(l.id, ty);
    }

    fn check_return(&mut self, value: Option<&Expr>, span: Span) {
        let actual = match value {
            Some(e) => self.check_expr(e),
            None => Type::Unit,
        };
        if !actual.compatible(self.cur_ret) {
            let span = value.map(|e| e.span).unwrap_or(span);
            self.diags.emit(
                Diagnostic::error(
                    DiagCode::ReturnTypeMismatch,
                    format!(
                        "returning `{actual}` from a function declared to return `{}`",
                        self.cur_ret
                    ),
                )
                .with_primary(
                    span,
                    format!("expected `{}`, found `{actual}`", self.cur_ret),
                ),
            );
        }
    }

    // ---- expressions ----

    /// Synthesises and records the type of an expression.
    fn check_expr(&mut self, expr: &Expr) -> Type {
        let ty = match &expr.kind {
            ExprKind::Int(_) => Type::Int,
            ExprKind::Float(_) => Type::Float,
            ExprKind::Bool(_) => Type::Bool,
            ExprKind::Str(_) => Type::Str,
            ExprKind::Name(name) => self.check_name(expr.id, name, expr.span),
            ExprKind::Unary { op, rhs } => self.check_unary(*op, rhs),
            ExprKind::Binary { op, lhs, rhs } => self.check_binary(*op, lhs, rhs),
            ExprKind::Call { callee, args } => self.check_call(callee, args, expr.span),
            ExprKind::Assign { target, value } => self.check_assign(target, value),
            ExprKind::AssignOp { target, op, value } => self.check_assign_op(*op, target, value),
            ExprKind::ArrayLit(elems) => self.check_array_lit(elems, expr.span),
            ExprKind::Index { base, index } => self.check_index(base, index),
            ExprKind::StructLit { name, fields } => self.check_struct_lit(name, fields, expr.span),
            ExprKind::Field { base, field } => self.check_field(base, field),
            ExprKind::TupleLit(elems) => self.check_tuple_lit(elems),
            ExprKind::TupleIndex {
                base,
                index,
                index_span,
            } => self.check_tuple_index(base, *index, *index_span),
            ExprKind::If(if_expr) => self.check_if(if_expr, expr.span),
            ExprKind::Match(m) => self.check_match(m, expr.span),
            ExprKind::Block(block) => self.check_block(block),
        };
        self.tc.expr_types.insert(expr.id, ty);
        ty
    }

    fn check_name(&mut self, id: NodeId, name: &str, span: Span) -> Type {
        match self.res.use_of(id) {
            Some(Res::Local(def)) => self.tc.local_type(def),
            Some(Res::Const(cid)) => self.tc.const_type(cid),
            // A function or builtin name only has meaning as a call target.
            Some(Res::Fn(_)) | Some(Res::Builtin(_)) => {
                self.diags.emit(
                    Diagnostic::error(
                        DiagCode::TypeMismatch,
                        format!("`{name}` is a function and cannot be used as a value"),
                    )
                    .with_primary(span, "functions are not first-class values in Lumen")
                    .with_help("call it with `()` instead"),
                );
                Type::Error
            }
            // Unresolved: resolution already reported it.
            None => Type::Error,
        }
    }

    fn check_unary(&mut self, op: UnOp, rhs: &Expr) -> Type {
        let t = self.check_expr(rhs);
        match op {
            UnOp::Neg if t.is_numeric() || t.is_error() => t,
            UnOp::Not if matches!(t, Type::Bool) || t.is_error() => Type::Bool,
            _ => {
                self.diags.emit(
                    Diagnostic::error(
                        DiagCode::InvalidOperands,
                        format!("cannot apply unary `{}` to `{t}`", op.symbol()),
                    )
                    .with_primary(rhs.span, format!("operand has type `{t}`")),
                );
                Type::Error
            }
        }
    }

    fn check_binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> Type {
        let lt = self.check_expr(lhs);
        let rt = self.check_expr(rhs);
        if lt.is_error() || rt.is_error() {
            // Result type still follows the operator category to limit cascades.
            return if op.is_comparison() || op.is_logical() {
                Type::Bool
            } else {
                Type::Error
            };
        }
        let span = lhs.span.to(rhs.span);
        use BinOp::*;
        match op {
            // `+` doubles as string concatenation when both sides are `str`.
            Add if lt == Type::Str && rt == Type::Str => Type::Str,
            Add | Sub | Mul | Div | Rem => {
                if lt == rt && lt.is_numeric() {
                    lt
                } else {
                    self.operand_error(op, lt, rt, span);
                    Type::Error
                }
            }
            Lt | Le | Gt | Ge => {
                if lt == rt && lt.is_numeric() {
                    Type::Bool
                } else {
                    self.operand_error(op, lt, rt, span);
                    Type::Bool
                }
            }
            Eq | Ne => {
                // Equality is defined for any two equal, comparable types.
                if lt == rt && !matches!(lt, Type::Unit) {
                    Type::Bool
                } else {
                    self.operand_error(op, lt, rt, span);
                    Type::Bool
                }
            }
            And | Or => {
                if matches!(lt, Type::Bool) && matches!(rt, Type::Bool) {
                    Type::Bool
                } else {
                    self.operand_error(op, lt, rt, span);
                    Type::Bool
                }
            }
        }
    }

    fn operand_error(&mut self, op: BinOp, lt: Type, rt: Type, span: Span) {
        self.diags.emit(
            Diagnostic::error(
                DiagCode::InvalidOperands,
                format!("cannot apply `{}` to `{lt}` and `{rt}`", op.symbol()),
            )
            .with_primary(span, format!("`{lt}` {} `{rt}`", op.symbol())),
        );
    }

    fn check_call(&mut self, callee: &Expr, args: &[Expr], span: Span) -> Type {
        // Calls are only valid on a function/builtin name (no function values).
        if let ExprKind::Name(name) = &callee.kind {
            match self.res.use_of(callee.id) {
                Some(Res::Fn(id)) => {
                    let sig = self.tc.signatures[id.0 as usize].clone();
                    self.check_args(&sig.params, args, &sig.name, span);
                    return sig.ret;
                }
                Some(Res::Builtin(b)) if b.is_generic() => {
                    return self.check_generic_builtin(b, args, span);
                }
                Some(Res::Builtin(b)) => {
                    self.check_args(b.params(), args, b.name(), span);
                    return b.ret();
                }
                Some(Res::Local(_)) | Some(Res::Const(_)) => {
                    self.diags.emit(
                        Diagnostic::error(
                            DiagCode::NotCallable,
                            format!("`{name}` is a value, not a function"),
                        )
                        .with_primary(callee.span, "cannot be called"),
                    );
                }
                None => {} // unresolved; already reported
            }
        } else {
            let t = self.check_expr(callee);
            if !t.is_error() {
                self.diags.emit(
                    Diagnostic::error(DiagCode::NotCallable, format!("type `{t}` is not callable"))
                        .with_primary(callee.span, "cannot be called"),
                );
            }
        }
        // Still type the arguments so their own errors surface.
        for arg in args {
            self.check_expr(arg);
        }
        Type::Error
    }

    /// Checks call arity and per-argument types against a parameter list.
    fn check_args(&mut self, params: &[Type], args: &[Expr], callee: &str, span: Span) {
        if params.len() != args.len() {
            self.diags.emit(
                Diagnostic::error(
                    DiagCode::ArityMismatch,
                    format!(
                        "`{callee}` expects {} argument(s) but {} were supplied",
                        params.len(),
                        args.len()
                    ),
                )
                .with_primary(span, format!("expected {} argument(s)", params.len())),
            );
        }
        for (arg, &expected) in args.iter().zip(params) {
            let actual = self.check_expr(arg);
            if !actual.compatible(expected) {
                self.diags.emit(
                    Diagnostic::error(
                        DiagCode::TypeMismatch,
                        format!("argument to `{callee}` has the wrong type"),
                    )
                    .with_primary(arg.span, format!("expected `{expected}`, found `{actual}`")),
                );
            }
        }
        // Type any surplus arguments so their internal errors are still found.
        for arg in args.iter().skip(params.len()) {
            self.check_expr(arg);
        }
    }

    /// Checks a call to a builtin whose signature is generic (currently only
    /// `len`, which accepts any array).
    fn check_generic_builtin(&mut self, builtin: Builtin, args: &[Expr], span: Span) -> Type {
        debug_assert!(matches!(builtin, Builtin::Len));
        if args.len() != 1 {
            self.diags.emit(
                Diagnostic::error(
                    DiagCode::ArityMismatch,
                    format!(
                        "`{}` expects 1 argument but {} were supplied",
                        builtin.name(),
                        args.len()
                    ),
                )
                .with_primary(span, "expected 1 argument"),
            );
            for arg in args {
                self.check_expr(arg);
            }
            return builtin.ret();
        }
        let arg_ty = self.check_expr(&args[0]);
        if !arg_ty.is_array() && !arg_ty.is_error() {
            self.diags.emit(
                Diagnostic::error(
                    DiagCode::TypeMismatch,
                    format!("`len` expects an array, found `{arg_ty}`"),
                )
                .with_primary(args[0].span, "not an array"),
            );
        }
        builtin.ret()
    }

    fn check_assign(&mut self, target: &Expr, value: &Expr) -> Type {
        // Indexed assignment `a[i] = v` is a separate form of place expression.
        if let ExprKind::Index { base, index } = &target.kind {
            return self.check_index_assign(base, index, value);
        }
        // Field assignment `s.f = v` is another place form.
        if let ExprKind::Field { base, field } = &target.kind {
            return self.check_field_assign(base, field, value);
        }
        // Tuple element assignment `t.0 = v`.
        if let ExprKind::TupleIndex {
            base,
            index,
            index_span,
        } = &target.kind
        {
            let elem_ty = self.check_tuple_index(base, *index, *index_span);
            let value_ty = self.check_expr(value);
            if !value_ty.compatible(elem_ty) && !elem_ty.is_error() {
                self.diags.emit(
                    Diagnostic::error(
                        DiagCode::TypeMismatch,
                        "mismatched types in tuple assignment",
                    )
                    .with_primary(
                        value.span,
                        format!("expected `{elem_ty}`, found `{value_ty}`"),
                    ),
                );
            }
            return Type::Unit;
        }
        let value_ty = self.check_expr(value);
        // The only valid assignment target is a mutable local.
        if let ExprKind::Name(name) = &target.kind
            && let Some(Res::Local(def)) = self.res.use_of(target.id)
        {
            let target_ty = self.tc.local_type(def);
            self.tc.expr_types.insert(target.id, target_ty);
            let mutable = self.res.local(def).map(|l| l.mutable).unwrap_or(false);
            if !mutable {
                self.diags.emit(
                    Diagnostic::error(
                        DiagCode::AssignToImmutable,
                        format!("cannot assign to immutable binding `{name}`"),
                    )
                    .with_primary(target.span, "cannot assign twice to an immutable binding")
                    .with_help(format!(
                        "declare it with `let mut {name}` to allow assignment"
                    )),
                );
            } else if !value_ty.compatible(target_ty) {
                self.diags.emit(
                    Diagnostic::error(
                        DiagCode::TypeMismatch,
                        format!("mismatched types assigning to `{name}`"),
                    )
                    .with_primary(
                        value.span,
                        format!("expected `{target_ty}`, found `{value_ty}`"),
                    ),
                );
            }
            return Type::Unit;
        }
        // Not a place expression.
        if !self.check_expr(target).is_error() {
            self.diags.emit(
                Diagnostic::error(DiagCode::InvalidAssignTarget, "invalid assignment target")
                    .with_primary(target.span, "cannot assign to this expression"),
            );
        }
        Type::Unit
    }

    /// Checks `base[index] = value`.
    ///
    /// Arrays have reference semantics, so mutating an *element* mutates the
    /// shared referent rather than the binding. Element assignment is therefore
    /// allowed regardless of the binding's `mut`-ness (only *rebinding* the
    /// variable itself requires `mut`), which is what makes mutation through an
    /// array parameter work.
    fn check_index_assign(&mut self, base: &Expr, index: &Expr, value: &Expr) -> Type {
        let base_ty = self.check_expr(base);
        let index_ty = self.check_expr(index);
        let value_ty = self.check_expr(value);
        self.expect(
            index_ty,
            Type::Int,
            index.span,
            DiagCode::TypeMismatch,
            "array index",
        );

        match base_ty {
            Type::Array(elem) => {
                if !value_ty.compatible(elem.ty()) {
                    self.diags.emit(
                        Diagnostic::error(
                            DiagCode::TypeMismatch,
                            "mismatched types in element assignment",
                        )
                        .with_primary(
                            value.span,
                            format!("expected `{}`, found `{value_ty}`", elem.ty()),
                        ),
                    );
                }
            }
            Type::Error => {}
            other => {
                self.diags.emit(
                    Diagnostic::error(
                        DiagCode::NotIndexable,
                        format!("`{other}` cannot be indexed"),
                    )
                    .with_primary(base.span, "not an array"),
                );
            }
        }
        Type::Unit
    }

    /// Checks `base.field = value`. Like array elements, struct fields have
    /// reference semantics, so the binding need not be `mut`.
    fn check_field_assign(&mut self, base: &Expr, field: &Ident, value: &Expr) -> Type {
        let field_ty = self.check_field(base, field);
        let value_ty = self.check_expr(value);
        if !value_ty.compatible(field_ty) && !field_ty.is_error() {
            self.diags.emit(
                Diagnostic::error(
                    DiagCode::TypeMismatch,
                    "mismatched types in field assignment",
                )
                .with_primary(
                    value.span,
                    format!("expected `{field_ty}`, found `{value_ty}`"),
                ),
            );
        }
        Type::Unit
    }

    /// Checks a compound assignment `target op= value`. The target must be a
    /// mutable local, and `target op value` must itself be well-typed and yield
    /// the target's type.
    fn check_assign_op(&mut self, op: BinOp, target: &Expr, value: &Expr) -> Type {
        let target_ty = self.check_expr(target);
        let value_ty = self.check_expr(value);

        let mutable = match (&target.kind, self.res.use_of(target.id)) {
            (ExprKind::Name(_), Some(Res::Local(def))) => {
                self.res.local(def).map(|l| l.mutable).unwrap_or(false)
            }
            _ => {
                if !target_ty.is_error() {
                    self.diags.emit(
                        Diagnostic::error(
                            DiagCode::InvalidAssignTarget,
                            "invalid compound-assignment target",
                        )
                        .with_primary(target.span, "cannot assign to this expression"),
                    );
                }
                return Type::Unit;
            }
        };
        if !mutable
            && !target_ty.is_error()
            && let ExprKind::Name(name) = &target.kind
        {
            self.diags.emit(
                Diagnostic::error(
                    DiagCode::AssignToImmutable,
                    format!("cannot assign to immutable binding `{name}`"),
                )
                .with_primary(target.span, "binding is not mutable")
                .with_help(format!("declare it with `let mut {name}`")),
            );
        }
        // The operator must be applicable: `target op value` must type-check and
        // produce the target's type (compound assignment cannot change the type).
        let operands_ok = target_ty.is_error()
            || value_ty.is_error()
            || (target_ty == value_ty && target_ty.is_numeric());
        if !operands_ok {
            self.operand_error(op, target_ty, value_ty, target.span.to(value.span));
        }
        Type::Unit
    }

    /// Checks an array literal. All elements must share one primitive type.
    fn check_array_lit(&mut self, elems: &[Expr], span: Span) -> Type {
        if elems.is_empty() {
            self.diags.emit(
                Diagnostic::error(
                    DiagCode::BadArrayType,
                    "cannot infer the type of an empty array literal",
                )
                .with_primary(span, "give it at least one element")
                .with_help("empty arrays are not yet supported"),
            );
            return Type::Error;
        }
        let first = self.check_expr(&elems[0]);
        for e in &elems[1..] {
            let t = self.check_expr(e);
            if !t.compatible(first) {
                self.diags.emit(
                    Diagnostic::error(
                        DiagCode::TypeMismatch,
                        "array elements have differing types",
                    )
                    .with_primary(e.span, format!("expected `{first}`, found `{t}`")),
                );
            }
        }
        if first.is_error() {
            return Type::Error;
        }
        match Elem::of(first) {
            Some(elem) => Type::Array(elem),
            None => {
                self.diags.emit(
                    Diagnostic::error(
                        DiagCode::BadArrayType,
                        format!("`{first}` is not a valid array element type"),
                    )
                    .with_primary(elems[0].span, "elements must be i64, f64, bool, or str"),
                );
                Type::Error
            }
        }
    }

    /// Checks an index expression `base[index]`.
    fn check_index(&mut self, base: &Expr, index: &Expr) -> Type {
        let base_ty = self.check_expr(base);
        let index_ty = self.check_expr(index);
        self.expect(
            index_ty,
            Type::Int,
            index.span,
            DiagCode::TypeMismatch,
            "array index",
        );
        match base_ty {
            Type::Array(elem) => elem.ty(),
            Type::Error => Type::Error,
            other => {
                self.diags.emit(
                    Diagnostic::error(
                        DiagCode::NotIndexable,
                        format!("`{other}` cannot be indexed"),
                    )
                    .with_primary(base.span, "not an array"),
                );
                Type::Error
            }
        }
    }

    /// Checks a struct literal `Name { field: value, ... }`.
    fn check_struct_lit(&mut self, name: &Ident, fields: &[FieldInit], span: Span) -> Type {
        let Some(id) = self.res.structs.lookup(&name.name) else {
            // Type the field values anyway so their own errors surface.
            for f in fields {
                self.check_expr(&f.value);
            }
            self.diags.emit(
                Diagnostic::error(
                    DiagCode::BadStructLiteral,
                    format!("`{}` is not a struct", name.name),
                )
                .with_primary(name.span, "unknown struct"),
            );
            return Type::Error;
        };
        let info = self.tc.struct_info(id).clone();

        let mut provided: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for f in fields {
            let actual = self.check_expr(&f.value);
            match info.field(&f.name.name) {
                Some((_, expected)) => {
                    if !provided.insert(&f.name.name) {
                        self.diags.emit(
                            Diagnostic::error(
                                DiagCode::BadStructLiteral,
                                format!("field `{}` specified more than once", f.name.name),
                            )
                            .with_primary(f.name.span, "duplicate field"),
                        );
                    }
                    if !actual.compatible(expected) {
                        self.diags.emit(
                            Diagnostic::error(
                                DiagCode::TypeMismatch,
                                format!("field `{}` has the wrong type", f.name.name),
                            )
                            .with_primary(
                                f.value.span,
                                format!("expected `{expected}`, found `{actual}`"),
                            ),
                        );
                    }
                }
                None => {
                    self.diags.emit(
                        Diagnostic::error(
                            DiagCode::UnknownField,
                            format!("struct `{}` has no field `{}`", info.name, f.name.name),
                        )
                        .with_primary(f.name.span, "no such field"),
                    );
                }
            }
        }
        // Every declared field must be supplied.
        let missing: Vec<&str> = info
            .fields
            .iter()
            .map(|(n, _)| n.as_str())
            .filter(|n| !provided.contains(n))
            .collect();
        if !missing.is_empty() {
            self.diags.emit(
                Diagnostic::error(
                    DiagCode::BadStructLiteral,
                    format!(
                        "missing field(s) {} in `{}` literal",
                        missing.join(", "),
                        info.name
                    ),
                )
                .with_primary(span, "all fields must be provided"),
            );
        }
        Type::Struct(id.0)
    }

    /// Checks field access `base.field`.
    fn check_field(&mut self, base: &Expr, field: &Ident) -> Type {
        let base_ty = self.check_expr(base);
        match base_ty {
            Type::Struct(sid) => {
                let info = self.tc.struct_info(StructId(sid));
                match info.field(&field.name) {
                    Some((_, ty)) => ty,
                    None => {
                        let name = info.name.clone();
                        self.diags.emit(
                            Diagnostic::error(
                                DiagCode::UnknownField,
                                format!("struct `{name}` has no field `{}`", field.name),
                            )
                            .with_primary(field.span, "no such field"),
                        );
                        Type::Error
                    }
                }
            }
            Type::Error => Type::Error,
            other => {
                self.diags.emit(
                    Diagnostic::error(
                        DiagCode::UnknownField,
                        format!("type `{other}` has no fields"),
                    )
                    .with_primary(base.span, "not a struct"),
                );
                Type::Error
            }
        }
    }

    /// Checks a tuple literal, interning its element types into a tuple type.
    fn check_tuple_lit(&mut self, elems: &[Expr]) -> Type {
        let tys: Vec<Type> = elems.iter().map(|e| self.check_expr(e)).collect();
        if tys.iter().any(|t| t.is_error()) {
            return Type::Error;
        }
        Type::Tuple(self.tc.intern_tuple(tys))
    }

    /// Checks tuple element access `base.index`.
    fn check_tuple_index(&mut self, base: &Expr, index: usize, index_span: Span) -> Type {
        let base_ty = self.check_expr(base);
        match base_ty {
            Type::Tuple(id) => {
                let elems = self.tc.tuple_elems(id);
                match elems.get(index) {
                    Some(&ty) => ty,
                    None => {
                        let arity = elems.len();
                        self.diags.emit(
                            Diagnostic::error(
                                DiagCode::UnknownField,
                                format!(
                                    "tuple has {arity} element(s); index {index} is out of range"
                                ),
                            )
                            .with_primary(index_span, "no such tuple element"),
                        );
                        Type::Error
                    }
                }
            }
            Type::Error => Type::Error,
            other => {
                self.diags.emit(
                    Diagnostic::error(
                        DiagCode::UnknownField,
                        format!("type `{other}` is not a tuple"),
                    )
                    .with_primary(base.span, "not a tuple"),
                );
                Type::Error
            }
        }
    }

    fn check_if(&mut self, if_expr: &IfExpr, span: Span) -> Type {
        let cond = self.check_expr(&if_expr.cond);
        self.expect(
            cond,
            Type::Bool,
            if_expr.cond.span,
            DiagCode::NonBoolCondition,
            "`if` condition",
        );
        let then_ty = self.check_block(&if_expr.then_branch);
        match &if_expr.else_branch {
            Some(else_branch) => {
                let else_ty = self.check_expr(else_branch);
                if then_ty.compatible(else_ty) {
                    // Prefer the concrete type if one side is the error type.
                    if then_ty.is_error() { else_ty } else { then_ty }
                } else {
                    self.diags.emit(
                        Diagnostic::error(
                            DiagCode::IfBranchMismatch,
                            "`if` and `else` branches have incompatible types",
                        )
                        .with_primary(span, format!("`{then_ty}` vs `{else_ty}`"))
                        .with_help("both branches of an `if` expression must have the same type"),
                    );
                    Type::Error
                }
            }
            // Without `else`, the `if` yields unit, so the then-branch must too.
            None => {
                if !then_ty.compatible(Type::Unit) {
                    self.diags.emit(
                        Diagnostic::error(
                            DiagCode::IfBranchMismatch,
                            "`if` without `else` must have type `unit`",
                        )
                        .with_primary(
                            if_expr.then_branch.span,
                            format!("this block has type `{then_ty}`"),
                        )
                        .with_help("add an `else` branch that yields the same type"),
                    );
                }
                Type::Unit
            }
        }
    }

    /// Checks a `match` expression: the scrutinee must be a scalar (`i64` or
    /// `bool`), every pattern must match the scrutinee's type, all arm bodies
    /// must share a type, and the arms together must be exhaustive.
    fn check_match(&mut self, m: &MatchExpr, span: Span) -> Type {
        let scrut = self.check_expr(&m.scrutinee);
        let scalar = matches!(scrut, Type::Int | Type::Bool);
        if !scrut.is_error() && !scalar {
            self.diags.emit(
                Diagnostic::error(
                    DiagCode::TypeMismatch,
                    format!("cannot `match` on a value of type `{scrut}`"),
                )
                .with_primary(m.scrutinee.span, "only `i64` and `bool` can be matched")
                .with_help("`match` compares the scrutinee against scalar literals"),
            );
        }

        let mut result: Option<Type> = None;
        let mut has_wild = false;
        let mut saw_true = false;
        let mut saw_false = false;
        for arm in &m.arms {
            self.check_arm_pattern(&arm.pattern, scrut, arm.span);
            match &arm.pattern {
                Pattern::Wild => has_wild = true,
                Pattern::Bool(true) => saw_true = true,
                Pattern::Bool(false) => saw_false = true,
                Pattern::Int(_) => {}
            }
            let body_ty = self.check_expr(&arm.body);
            result = Some(match result {
                None => body_ty,
                Some(prev) if prev.compatible(body_ty) => {
                    if prev.is_error() {
                        body_ty
                    } else {
                        prev
                    }
                }
                Some(prev) => {
                    self.diags.emit(
                        Diagnostic::error(
                            DiagCode::IfBranchMismatch,
                            "`match` arms have incompatible types",
                        )
                        .with_primary(arm.span, format!("`{prev}` vs `{body_ty}`"))
                        .with_help("every arm of a `match` must yield the same type"),
                    );
                    Type::Error
                }
            });
        }

        // A `bool` match is exhaustive once both values appear; any match with a
        // wildcard is exhaustive. Otherwise (notably `i64`) it is not.
        let exhaustive = has_wild || (scrut == Type::Bool && saw_true && saw_false);
        if !scrut.is_error() && scalar && !exhaustive {
            self.diags.emit(
                Diagnostic::error(
                    DiagCode::NonExhaustiveMatch,
                    "`match` does not cover every possible value",
                )
                .with_primary(span, "add the missing patterns or a `_` arm"),
            );
        }
        result.unwrap_or(Type::Unit)
    }

    /// Checks one arm pattern against the scrutinee type.
    fn check_arm_pattern(&mut self, pattern: &Pattern, scrut: Type, span: Span) {
        let pat_ty = match pattern {
            Pattern::Int(_) => Type::Int,
            Pattern::Bool(_) => Type::Bool,
            Pattern::Wild => return,
        };
        if !scrut.is_error() && scrut != pat_ty {
            self.diags.emit(
                Diagnostic::error(
                    DiagCode::TypeMismatch,
                    format!("pattern of type `{pat_ty}` cannot match `{scrut}`"),
                )
                .with_primary(span, format!("expected a `{scrut}` pattern")),
            );
        }
    }

    /// Reports a mismatch if `actual` is not compatible with `expected`.
    fn expect(&mut self, actual: Type, expected: Type, span: Span, code: DiagCode, what: &str) {
        if !actual.compatible(expected) {
            self.diags.emit(
                Diagnostic::error(
                    code,
                    format!("{what} must be `{expected}`, found `{actual}`"),
                )
                .with_primary(span, format!("expected `{expected}`")),
            );
        }
    }
}

/// A placeholder constant value of the given declared type, used after an error
/// so that later phases (which only run on error-free programs) still see a
/// consistently-typed value.
fn default_const_value(ty: Type) -> ConstValue {
    match ty {
        Type::Float => ConstValue::Float(0.0),
        Type::Bool => ConstValue::Bool(false),
        Type::Str => ConstValue::Str(String::new()),
        // `Int`, `Unit`, and `Error` all fall back to an integer zero.
        _ => ConstValue::Int(0),
    }
}

/// Evaluates a unary operator over a constant value.
fn eval_const_unary(op: UnOp, v: ConstValue) -> Option<ConstValue> {
    match (op, v) {
        (UnOp::Neg, ConstValue::Int(n)) => Some(ConstValue::Int(n.wrapping_neg())),
        (UnOp::Neg, ConstValue::Float(n)) => Some(ConstValue::Float(-n)),
        (UnOp::Not, ConstValue::Bool(b)) => Some(ConstValue::Bool(!b)),
        _ => None,
    }
}

/// Evaluates a binary operator over two constant values.
fn eval_const_binary(op: BinOp, a: ConstValue, b: ConstValue) -> Option<ConstValue> {
    use ConstValue::{Bool, Float, Int, Str};
    Some(match (a, b) {
        (Int(x), Int(y)) => match op {
            BinOp::Add => Int(x.wrapping_add(y)),
            BinOp::Sub => Int(x.wrapping_sub(y)),
            BinOp::Mul => Int(x.wrapping_mul(y)),
            BinOp::Div => Int(x.checked_div(y)?),
            BinOp::Rem => Int(x.checked_rem(y)?),
            BinOp::Eq => Bool(x == y),
            BinOp::Ne => Bool(x != y),
            BinOp::Lt => Bool(x < y),
            BinOp::Le => Bool(x <= y),
            BinOp::Gt => Bool(x > y),
            BinOp::Ge => Bool(x >= y),
            BinOp::And | BinOp::Or => return None,
        },
        (Float(x), Float(y)) => match op {
            BinOp::Add => Float(x + y),
            BinOp::Sub => Float(x - y),
            BinOp::Mul => Float(x * y),
            BinOp::Div => Float(x / y),
            BinOp::Rem => Float(x % y),
            BinOp::Eq => Bool(x == y),
            BinOp::Ne => Bool(x != y),
            BinOp::Lt => Bool(x < y),
            BinOp::Le => Bool(x <= y),
            BinOp::Gt => Bool(x > y),
            BinOp::Ge => Bool(x >= y),
            BinOp::And | BinOp::Or => return None,
        },
        (Bool(x), Bool(y)) => match op {
            BinOp::Eq => Bool(x == y),
            BinOp::Ne => Bool(x != y),
            BinOp::And => Bool(x && y),
            BinOp::Or => Bool(x || y),
            _ => return None,
        },
        (Str(x), Str(y)) => match op {
            BinOp::Eq => Bool(x == y),
            BinOp::Ne => Bool(x != y),
            _ => return None,
        },
        _ => return None,
    })
}

/// Whether control is guaranteed to leave `block` via `return` rather than
/// falling off its end.
///
/// This is a deliberately simple, conservative analysis (no constant folding of
/// conditions): a block diverges if any statement diverges, or if its tail
/// expression diverges. It is used only to decide whether to check the
/// fall-through value, so being conservative merely means occasionally checking
/// a fall-through that cannot happen  never the reverse.
fn block_diverges(block: &Block) -> bool {
    block.stmts.iter().any(stmt_diverges) || block.tail.as_deref().is_some_and(expr_diverges)
}

fn stmt_diverges(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Return(_) => true,
        StmtKind::Expr(e) => expr_diverges(e),
        // A `let` diverges only if evaluating its initialiser does.
        StmtKind::Let(l) => expr_diverges(&l.init),
        // Loops may execute zero times; `break`/`continue` transfer control
        // within a loop, not out of the function. None guarantee divergence.
        StmtKind::While(_)
        | StmtKind::For(_)
        | StmtKind::ForEach(_)
        | StmtKind::Break
        | StmtKind::Continue => false,
    }
}

fn expr_diverges(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Block(block) => block_diverges(block),
        // An `if` diverges only when it has an `else` and both arms diverge.
        ExprKind::If(if_expr) => {
            let then_div = block_diverges(&if_expr.then_branch);
            let else_div = if_expr.else_branch.as_deref().is_some_and(expr_diverges);
            then_div && else_div
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests;

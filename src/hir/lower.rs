//! Lowering: AST + [`Resolution`] + [`Typeck`] → [`Hir`].
//!
//! # Precondition
//!
//! Lowering assumes the program is **well-typed**: the driver only calls it
//! after resolution and type checking have produced no errors. It therefore
//! emits no diagnostics. Where a malformed program could in principle reach a
//! branch (e.g. a name that did not resolve to a local), lowering substitutes a
//! harmless placeholder rather than panicking, keeping the rule that the
//! compiler never crashes on bad input.
//!
//! # What it does
//!
//! For each function it allocates a dense [`LocalId`] for every parameter and
//! `let` binding (parameters first), recording the mapping from the AST's
//! definition [`NodeId`]s. It then walks the body, replacing names with local
//! reads, call targets with [`Callee`]s, and attaching the type computed by the
//! checker to every node.

use std::collections::HashMap;

use crate::hir::*;
use crate::parser::ast;
use crate::parser::ast::NodeId;
use crate::sema::resolve::{Res, Resolution, StructId};
use crate::sema::typeck::{ConstValue, Typeck};
use crate::sema::types::Type;

/// Builds the HIR literal kind for an inlined constant value.
fn const_value_kind(value: &ConstValue) -> ExprKind {
    match value {
        ConstValue::Int(v) => ExprKind::Int(*v),
        ConstValue::Float(v) => ExprKind::Float(*v),
        ConstValue::Bool(v) => ExprKind::Bool(*v),
        ConstValue::Str(s) => ExprKind::Str(s.clone()),
    }
}

/// Lowers a type-checked program to [`Hir`].
#[tracing::instrument(level = "debug", skip_all)]
pub fn lower(ast: &ast::Ast, res: &Resolution, tc: &Typeck) -> Hir {
    let mut lowerer = Lowerer {
        res,
        tc,
        locals: Vec::new(),
        local_map: HashMap::new(),
    };
    let mut functions = Vec::with_capacity(res.functions.len());
    for (fn_id, item_idx) in res.functions.iter() {
        let ast::ItemKind::Fn(decl) = &ast.items[item_idx].kind else {
            continue;
        };
        functions.push(lowerer.lower_fn(fn_id, decl));
    }
    // `main` is guaranteed present in a well-typed program; fall back to the
    // first function only if the precondition was violated.
    let main = tc.main.unwrap_or(FnId(0));
    tracing::debug!(functions = functions.len(), "lowering complete");
    Hir { functions, main }
}

struct Lowerer<'a> {
    res: &'a Resolution,
    tc: &'a Typeck,
    /// Locals of the function currently being lowered.
    locals: Vec<LocalDecl>,
    /// Definition `NodeId` → its allocated [`LocalId`], for the current function.
    local_map: HashMap<NodeId, LocalId>,
}

impl Lowerer<'_> {
    fn lower_fn(&mut self, fn_id: FnId, decl: &ast::FnDecl) -> Function {
        self.locals.clear();
        self.local_map.clear();

        for param in &decl.params {
            self.alloc_local(param.id, &param.name.name, self.tc.local_type(param.id));
        }
        let param_count = self.locals.len();
        let ret = self.tc.signature(fn_id).ret;
        let body = self.lower_block(&decl.body);

        Function {
            name: decl.name.name.clone(),
            locals: std::mem::take(&mut self.locals),
            param_count,
            ret,
            body,
        }
    }

    fn lower_block(&mut self, block: &ast::Block) -> Block {
        let stmts = block.stmts.iter().map(|s| self.lower_stmt(s)).collect();
        let tail = block.tail.as_ref().map(|t| Box::new(self.lower_expr(t)));
        let ty = tail.as_ref().map(|t| t.ty).unwrap_or(Type::Unit);
        Block { stmts, tail, ty }
    }

    fn lower_stmt(&mut self, stmt: &ast::Stmt) -> Stmt {
        match &stmt.kind {
            ast::StmtKind::Let(l) => {
                // Lower the initialiser first; the binding is not in scope for it.
                let value = self.lower_expr(&l.init);
                let local = self.alloc_local(l.id, &l.name.name, self.tc.local_type(l.id));
                Stmt::Let { local, value }
            }
            ast::StmtKind::Expr(e) => Stmt::Expr(self.lower_expr(e)),
            ast::StmtKind::Return(e) => Stmt::Return(e.as_ref().map(|e| self.lower_expr(e))),
            ast::StmtKind::While(w) => Stmt::While {
                cond: self.lower_expr(&w.cond),
                body: self.lower_block(&w.body),
            },
            ast::StmtKind::For(f) => {
                // Bounds are evaluated in the outer scope; then the loop variable
                // and a hidden slot caching the upper bound get their own slots.
                let start = self.lower_expr(&f.start);
                let end = self.lower_expr(&f.end);
                let var = self.alloc_local(f.id, &f.var.name, Type::Int);
                let end_var = self.alloc_synthetic("<for-end>", Type::Int);
                let body = self.lower_block(&f.body);
                Stmt::For {
                    var,
                    end_var,
                    start,
                    end,
                    body,
                }
            }
            ast::StmtKind::ForEach(f) => self.lower_for_each(f, stmt.span),
            ast::StmtKind::Break => Stmt::Break,
            ast::StmtKind::Continue => Stmt::Continue,
        }
    }

    /// Desugars `for v in arr { body }` into an index-based loop:
    ///
    /// ```text
    /// {
    ///     let <arr> = arr;          // evaluate the array exactly once
    ///     for <i> in 0..len(<arr>) {
    ///         let v = <arr>[<i>];   // bind the element at the loop top
    ///         body
    ///     }
    /// }
    /// ```
    ///
    /// Reusing the [`Stmt::For`] node (rather than a `while`) keeps `continue`
    /// routed to the index increment, so the loop cannot spin forever.
    fn lower_for_each(&mut self, f: &ast::ForEachStmt, span: Span) -> Stmt {
        let iterable = self.lower_expr(&f.iterable);
        let elem_ty = match iterable.ty {
            Type::Array(elem) => elem.ty(),
            _ => Type::Error,
        };
        // Hidden slots: the cached array, the index, and the cached length.
        let arr_local = self.alloc_synthetic("<foreach-arr>", iterable.ty);
        let idx = self.alloc_synthetic("<foreach-idx>", Type::Int);
        let end_var = self.alloc_synthetic("<for-end>", Type::Int);
        // The user-visible element binding must exist before lowering the body.
        let var = self.alloc_local(f.id, &f.var.name, elem_ty);

        let mut body = self.lower_block(&f.body);
        // Prepend `let v = <arr>[<i>];` to the loop body.
        let read = Stmt::Let {
            local: var,
            value: Expr::new(
                ExprKind::Index {
                    base: Box::new(Expr::new(ExprKind::Local(arr_local), iterable.ty, span)),
                    index: Box::new(Expr::new(ExprKind::Local(idx), Type::Int, span)),
                },
                elem_ty,
                span,
            ),
        };
        body.stmts.insert(0, read);

        let len_call = Expr::new(
            ExprKind::Call {
                callee: Callee::Builtin(Builtin::Len),
                args: vec![Expr::new(ExprKind::Local(arr_local), iterable.ty, span)],
            },
            Type::Int,
            span,
        );
        let for_stmt = Stmt::For {
            var: idx,
            end_var,
            start: Expr::new(ExprKind::Int(0), Type::Int, span),
            end: len_call,
            body,
        };
        let let_arr = Stmt::Let {
            local: arr_local,
            value: iterable,
        };
        // Wrap the array binding and the loop in a unit-typed block statement.
        Stmt::Expr(Expr::new(
            ExprKind::Block(Block {
                stmts: vec![let_arr, for_stmt],
                tail: None,
                ty: Type::Unit,
            }),
            Type::Unit,
            span,
        ))
    }

    fn lower_expr(&mut self, expr: &ast::Expr) -> Expr {
        let ty = self.tc.type_of(expr.id);
        let kind = match &expr.kind {
            ast::ExprKind::Int(v) => ExprKind::Int(*v),
            ast::ExprKind::Float(v) => ExprKind::Float(*v),
            ast::ExprKind::Bool(v) => ExprKind::Bool(*v),
            ast::ExprKind::Str(s) => ExprKind::Str(s.clone()),
            ast::ExprKind::Name(_) => self.lower_name(expr.id),
            ast::ExprKind::Unary { op, rhs } => ExprKind::Unary {
                op: *op,
                rhs: Box::new(self.lower_expr(rhs)),
            },
            ast::ExprKind::Binary { op, lhs, rhs } => ExprKind::Binary {
                op: *op,
                lhs: Box::new(self.lower_expr(lhs)),
                rhs: Box::new(self.lower_expr(rhs)),
            },
            ast::ExprKind::Call { callee, args } => self.lower_call(callee, args),
            ast::ExprKind::ArrayLit(elems) => {
                ExprKind::ArrayLit(elems.iter().map(|e| self.lower_expr(e)).collect())
            }
            ast::ExprKind::Index { base, index } => ExprKind::Index {
                base: Box::new(self.lower_expr(base)),
                index: Box::new(self.lower_expr(index)),
            },
            ast::ExprKind::StructLit { name, fields } => self.lower_struct_lit(name, fields),
            ast::ExprKind::Field { base, field } => {
                let idx = self.field_index(base, &field.name);
                ExprKind::GetField {
                    base: Box::new(self.lower_expr(base)),
                    idx,
                }
            }
            // Tuples share the struct/array runtime representation.
            ast::ExprKind::TupleLit(elems) => {
                ExprKind::StructLit(elems.iter().map(|e| self.lower_expr(e)).collect())
            }
            ast::ExprKind::TupleIndex { base, index, .. } => ExprKind::GetField {
                base: Box::new(self.lower_expr(base)),
                idx: *index as u32,
            },
            ast::ExprKind::Assign { target, value } => self.lower_assign(target, value),
            ast::ExprKind::AssignOp { target, op, value } => {
                self.lower_assign_op(*op, target, value)
            }
            ast::ExprKind::If(if_expr) => ExprKind::If {
                cond: Box::new(self.lower_expr(&if_expr.cond)),
                then_branch: self.lower_block(&if_expr.then_branch),
                else_branch: if_expr
                    .else_branch
                    .as_ref()
                    .map(|e| Box::new(self.lower_expr(e))),
            },
            ast::ExprKind::Match(m) => self.lower_match(m, ty, expr.span),
            ast::ExprKind::Block(block) => ExprKind::Block(self.lower_block(block)),
        };
        Expr::new(kind, ty, expr.span)
    }

    /// Desugars `match scrutinee { pat => body, ... }` into a binding for the
    /// scrutinee followed by a chain of `if`/`else`:
    ///
    /// ```text
    /// { let <m> = scrutinee;
    ///   if <m> == p0 { b0 } else if <m> == p1 { b1 } else { default } }
    /// ```
    ///
    /// The default is the wildcard arm's body, or, for an exhaustive match with
    /// no wildcard (a `bool` covering both values), the final arm unconditionally
    /// (its condition is then redundant, so it is dropped). Type checking has
    /// already proven the match exhaustive, so a missing default is unreachable.
    fn lower_match(&mut self, m: &ast::MatchExpr, ty: Type, span: Span) -> ExprKind {
        let scrut = self.lower_expr(&m.scrutinee);
        let scrut_ty = scrut.ty;
        let tmp = self.alloc_synthetic("<match>", scrut_ty);

        // Split the arms into conditional ones and a single default body.
        let wild = m
            .arms
            .iter()
            .position(|a| matches!(a.pattern, ast::Pattern::Wild));
        let (cond_arms, default): (&[ast::MatchArm], Option<&ast::Expr>) = match wild {
            Some(i) => (&m.arms[..i], Some(&m.arms[i].body)),
            None => match m.arms.split_last() {
                Some((last, rest)) => (rest, Some(&last.body)),
                None => (&[], None),
            },
        };

        // Build the chain from the back so each `if` nests inside the previous
        // arm's `else`.
        let mut else_expr: Option<Box<Expr>> = default.map(|b| Box::new(self.lower_expr(b)));
        for arm in cond_arms.iter().rev() {
            let body = self.lower_expr(&arm.body);
            let body_ty = body.ty;
            let then_branch = Block {
                stmts: Vec::new(),
                tail: Some(Box::new(body)),
                ty: body_ty,
            };
            let cond = Expr::new(
                ExprKind::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(Expr::new(ExprKind::Local(tmp), scrut_ty, arm.span)),
                    rhs: Box::new(self.lower_pattern_literal(&arm.pattern, scrut_ty, arm.span)),
                },
                Type::Bool,
                arm.span,
            );
            let if_expr = Expr::new(
                ExprKind::If {
                    cond: Box::new(cond),
                    then_branch,
                    else_branch: else_expr.take(),
                },
                ty,
                arm.span,
            );
            else_expr = Some(Box::new(if_expr));
        }

        // The scrutinee is bound first so it is evaluated exactly once. An empty
        // match (no arms) is rejected by the checker; lower a unit placeholder.
        let tail = else_expr.unwrap_or_else(|| {
            Box::new(Expr::new(
                ExprKind::Block(Block {
                    stmts: Vec::new(),
                    tail: None,
                    ty: Type::Unit,
                }),
                Type::Unit,
                span,
            ))
        });
        ExprKind::Block(Block {
            stmts: vec![Stmt::Let {
                local: tmp,
                value: scrut,
            }],
            tail: Some(tail),
            ty,
        })
    }

    /// Lowers a match arm's literal pattern to the constant it compares against.
    fn lower_pattern_literal(&self, pattern: &ast::Pattern, ty: Type, span: Span) -> Expr {
        let kind = match pattern {
            ast::Pattern::Int(v) => ExprKind::Int(*v),
            ast::Pattern::Bool(b) => ExprKind::Bool(*b),
            // A wildcard never reaches here: it becomes the default, not a test.
            ast::Pattern::Wild => ExprKind::Bool(true),
        };
        Expr::new(kind, ty, span)
    }

    /// Lowers a name in value position: a local read, or an inlined constant.
    fn lower_name(&self, use_id: NodeId) -> ExprKind {
        match self.res.use_of(use_id) {
            Some(Res::Local(def)) => ExprKind::Local(self.local_of(def)),
            // Constants are inlined: each use becomes the evaluated literal.
            Some(Res::Const(id)) => const_value_kind(self.tc.const_value(id)),
            // Unreachable for well-typed input (functions are not values).
            _ => ExprKind::Int(0),
        }
    }

    fn lower_call(&mut self, callee: &ast::Expr, args: &[ast::Expr]) -> ExprKind {
        let target = match self.res.use_of(callee.id) {
            Some(Res::Fn(id)) => Callee::Fn(id),
            Some(Res::Builtin(b)) => Callee::Builtin(b),
            // Unreachable for well-typed input (no indirect calls).
            _ => Callee::Fn(FnId(0)),
        };
        let args = args.iter().map(|a| self.lower_expr(a)).collect();
        ExprKind::Call {
            callee: target,
            args,
        }
    }

    /// Builds a struct literal with field values placed in declaration order.
    fn lower_struct_lit(&mut self, name: &ast::Ident, fields: &[ast::FieldInit]) -> ExprKind {
        let order: Vec<String> = match self.res.structs.lookup(&name.name) {
            Some(id) => self
                .tc
                .struct_info(id)
                .fields
                .iter()
                .map(|(n, _)| n.clone())
                .collect(),
            None => return ExprKind::Int(0), // unreachable for well-typed input
        };
        let values = order
            .iter()
            .map(
                |field_name| match fields.iter().find(|f| &f.name.name == field_name) {
                    Some(fi) => self.lower_expr(&fi.value),
                    None => Expr::new(ExprKind::Int(0), Type::Error, name.span),
                },
            )
            .collect();
        ExprKind::StructLit(values)
    }

    /// The declaration index of `field` on the struct that `base` evaluates to.
    fn field_index(&self, base: &ast::Expr, field: &str) -> u32 {
        match self.tc.type_of(base.id) {
            Type::Struct(sid) => self
                .tc
                .struct_info(StructId(sid))
                .field(field)
                .map(|(i, _)| i as u32)
                .unwrap_or(0),
            _ => 0,
        }
    }

    fn lower_assign(&mut self, target: &ast::Expr, value: &ast::Expr) -> ExprKind {
        // `a[i] = v` lowers to a SetIndex rather than a local store.
        if let ast::ExprKind::Index { base, index } = &target.kind {
            return ExprKind::SetIndex {
                base: Box::new(self.lower_expr(base)),
                index: Box::new(self.lower_expr(index)),
                value: Box::new(self.lower_expr(value)),
            };
        }
        // `s.f = v` lowers to a SetField.
        if let ast::ExprKind::Field { base, field } = &target.kind {
            let idx = self.field_index(base, &field.name);
            return ExprKind::SetField {
                base: Box::new(self.lower_expr(base)),
                idx,
                value: Box::new(self.lower_expr(value)),
            };
        }
        // `t.0 = v` also lowers to a SetField (tuples reuse the struct runtime).
        if let ast::ExprKind::TupleIndex { base, index, .. } = &target.kind {
            return ExprKind::SetField {
                base: Box::new(self.lower_expr(base)),
                idx: *index as u32,
                value: Box::new(self.lower_expr(value)),
            };
        }
        let local = match self.res.use_of(target.id) {
            Some(Res::Local(def)) => self.local_of(def),
            _ => LocalId(0),
        };
        ExprKind::Assign {
            local,
            value: Box::new(self.lower_expr(value)),
        }
    }

    /// Desugars `target op= value` into `target = target op value`.
    fn lower_assign_op(
        &mut self,
        op: ast::BinOp,
        target: &ast::Expr,
        value: &ast::Expr,
    ) -> ExprKind {
        let local = match self.res.use_of(target.id) {
            Some(Res::Local(def)) => self.local_of(def),
            _ => LocalId(0),
        };
        let ty = self.tc.type_of(target.id);
        let lhs = Expr::new(ExprKind::Local(local), ty, target.span);
        let rhs = self.lower_expr(value);
        let combined = Expr::new(
            ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            ty,
            target.span.to(value.span),
        );
        ExprKind::Assign {
            local,
            value: Box::new(combined),
        }
    }

    // ---- local slot allocation ----

    /// Allocates the next local slot for a definition node and records it.
    fn alloc_local(&mut self, def: NodeId, name: &str, ty: Type) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(LocalDecl {
            name: name.to_string(),
            ty,
        });
        self.local_map.insert(def, id);
        id
    }

    /// Allocates a compiler-internal local slot not tied to any source binding
    /// (e.g. a `for` loop's cached upper bound).
    fn alloc_synthetic(&mut self, name: &str, ty: Type) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(LocalDecl {
            name: name.to_string(),
            ty,
        });
        id
    }

    /// The local slot for a definition node, or slot 0 if (impossibly) absent.
    fn local_of(&self, def: NodeId) -> LocalId {
        self.local_map.get(&def).copied().unwrap_or(LocalId(0))
    }
}

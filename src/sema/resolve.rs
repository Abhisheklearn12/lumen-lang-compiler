//! Name resolution: binds every identifier *use* to the *definition* it refers
//! to, before type checking runs.
//!
//! # Design
//!
//! The resolver walks the AST with a stack of lexical scopes. It does not
//! mutate the tree; instead it produces a [`Resolution`] of side tables keyed by
//! [`NodeId`]:
//!
//! * `uses` maps each `Name` expression to a [`Res`] (local, user function, or
//!   builtin).
//! * `locals` maps each binding's definition node to its [`LocalInfo`]
//!   (mutability/name/span), which type checking consults for assignment checks.
//! * `functions` is the global [`FunctionTable`].
//!
//! Functions are collected in a first pass so calls may refer to functions
//! declared later and to themselves (recursion). Within a function, parameters
//! and `let` bindings live in nested scopes; a `let` becomes visible only
//! *after* its initialiser, so `let x = x;` sees the outer `x`. Re-binding a
//! name in the same scope is permitted (shadowing); duplicate *parameters* are
//! not.

use std::collections::HashMap;

use crate::diagnostics::{Diagnostic, Diagnostics};
use crate::errors::DiagCode;
use crate::parser::ast::*;
use crate::sema::types::Builtin;
use crate::span::Span;

/// Identifies a user-defined function by dense index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FnId(pub u32);

/// Identifies a top-level constant by dense index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConstId(pub u32);

/// Identifies a declared struct by dense index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StructId(pub u32);

/// What a name refers to once resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Res {
    /// A parameter or `let` binding, identified by its definition's [`NodeId`].
    Local(NodeId),
    /// A user-declared function.
    Fn(FnId),
    /// A top-level constant.
    Const(ConstId),
    /// A compiler builtin.
    Builtin(Builtin),
}

/// Mutability and provenance of a local binding, recorded at its definition.
#[derive(Clone, Debug)]
pub struct LocalInfo {
    pub name: String,
    pub mutable: bool,
    pub span: Span,
}

/// The global table of user functions.
#[derive(Debug, Default)]
pub struct FunctionTable {
    by_name: HashMap<String, FnId>,
    /// `FnId` index → position of the item in [`Ast::items`].
    item_index: Vec<usize>,
}

impl FunctionTable {
    /// The [`FnId`] a name binds to, if any.
    pub fn lookup(&self, name: &str) -> Option<FnId> {
        self.by_name.get(name).copied()
    }

    /// The `Ast::items` index of a function.
    pub fn item_index(&self, id: FnId) -> usize {
        self.item_index[id.0 as usize]
    }

    /// Number of registered functions.
    pub fn len(&self) -> usize {
        self.item_index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.item_index.is_empty()
    }

    /// Iterates `(FnId, item index)` pairs in declaration order.
    pub fn iter(&self) -> impl Iterator<Item = (FnId, usize)> + '_ {
        self.item_index
            .iter()
            .enumerate()
            .map(|(i, &idx)| (FnId(i as u32), idx))
    }
}

/// The global table of top-level constants.
#[derive(Debug, Default)]
pub struct ConstTable {
    by_name: HashMap<String, ConstId>,
    /// `ConstId` index → position of the item in [`Ast::items`].
    item_index: Vec<usize>,
}

impl ConstTable {
    /// The [`ConstId`] a name binds to, if any.
    pub fn lookup(&self, name: &str) -> Option<ConstId> {
        self.by_name.get(name).copied()
    }

    /// The `Ast::items` index of a constant.
    pub fn item_index(&self, id: ConstId) -> usize {
        self.item_index[id.0 as usize]
    }

    /// Number of registered constants.
    pub fn len(&self) -> usize {
        self.item_index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.item_index.is_empty()
    }

    /// Iterates `(ConstId, item index)` pairs in declaration order.
    pub fn iter(&self) -> impl Iterator<Item = (ConstId, usize)> + '_ {
        self.item_index
            .iter()
            .enumerate()
            .map(|(i, &idx)| (ConstId(i as u32), idx))
    }
}

/// The global table of declared structs.
#[derive(Debug, Default)]
pub struct StructTable {
    by_name: HashMap<String, StructId>,
    /// `StructId` index → position of the item in [`Ast::items`].
    item_index: Vec<usize>,
}

impl StructTable {
    /// The [`StructId`] a name binds to, if any.
    pub fn lookup(&self, name: &str) -> Option<StructId> {
        self.by_name.get(name).copied()
    }

    /// The `Ast::items` index of a struct.
    pub fn item_index(&self, id: StructId) -> usize {
        self.item_index[id.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.item_index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.item_index.is_empty()
    }

    /// Iterates `(StructId, item index)` pairs in declaration order.
    pub fn iter(&self) -> impl Iterator<Item = (StructId, usize)> + '_ {
        self.item_index
            .iter()
            .enumerate()
            .map(|(i, &idx)| (StructId(i as u32), idx))
    }
}

/// The product of resolution: the side tables later phases consume.
#[derive(Debug, Default)]
pub struct Resolution {
    pub uses: HashMap<NodeId, Res>,
    pub locals: HashMap<NodeId, LocalInfo>,
    pub functions: FunctionTable,
    pub consts: ConstTable,
    pub structs: StructTable,
}

impl Resolution {
    /// The resolution of a `Name` expression, by its node id.
    pub fn use_of(&self, id: NodeId) -> Option<Res> {
        self.uses.get(&id).copied()
    }

    /// The binding info for a definition node (`let`/param), by its node id.
    pub fn local(&self, id: NodeId) -> Option<&LocalInfo> {
        self.locals.get(&id)
    }
}

/// Resolves all names in `ast`, reporting unresolved/duplicate names to `diags`.
#[tracing::instrument(level = "debug", skip_all)]
pub fn resolve(ast: &Ast, diags: &mut Diagnostics) -> Resolution {
    let mut resolver = Resolver {
        res: Resolution::default(),
        scopes: Vec::new(),
        diags,
    };
    resolver.collect_globals(ast);
    for item in &ast.items {
        match &item.kind {
            ItemKind::Fn(decl) => resolver.resolve_fn(decl),
            ItemKind::Const(decl) => resolver.resolve_const(decl),
            // Struct field types are resolved during type checking; nothing in a
            // struct declaration refers to a value name.
            ItemKind::Struct(_) => {}
        }
    }
    tracing::debug!(
        functions = resolver.res.functions.len(),
        consts = resolver.res.consts.len(),
        uses = resolver.res.uses.len(),
        "resolution complete"
    );
    resolver.res
}

/// A single lexical scope: local name → its definition node.
type Scope = HashMap<String, NodeId>;

struct Resolver<'a> {
    res: Resolution,
    scopes: Vec<Scope>,
    diags: &'a mut Diagnostics,
}

impl Resolver<'_> {
    /// First pass: register every top-level name (functions and constants) so
    /// later items can refer to earlier and later ones. Functions and constants
    /// share one namespace; any duplicate keeps the first and reports the rest.
    fn collect_globals(&mut self, ast: &Ast) {
        // Name → span of its first definition, across both kinds.
        let mut seen: HashMap<String, Span> = HashMap::new();
        for (idx, item) in ast.items.iter().enumerate() {
            let (name, name_span) = match &item.kind {
                ItemKind::Fn(decl) => (decl.name.name.clone(), decl.name.span),
                ItemKind::Const(decl) => (decl.name.name.clone(), decl.name.span),
                ItemKind::Struct(decl) => (decl.name.name.clone(), decl.name.span),
            };
            if let Some(&first) = seen.get(&name) {
                self.diags.emit(
                    Diagnostic::error(
                        DiagCode::DuplicateDefinition,
                        format!("`{name}` is defined multiple times"),
                    )
                    .with_primary(name_span, "redefined here")
                    .with_label(first, "first defined here"),
                );
                continue;
            }
            seen.insert(name.clone(), name_span);
            match &item.kind {
                ItemKind::Fn(_) => {
                    let id = FnId(self.res.functions.item_index.len() as u32);
                    self.res.functions.item_index.push(idx);
                    self.res.functions.by_name.insert(name, id);
                }
                ItemKind::Const(_) => {
                    let id = ConstId(self.res.consts.item_index.len() as u32);
                    self.res.consts.item_index.push(idx);
                    self.res.consts.by_name.insert(name, id);
                }
                ItemKind::Struct(_) => {
                    let id = StructId(self.res.structs.item_index.len() as u32);
                    self.res.structs.item_index.push(idx);
                    self.res.structs.by_name.insert(name, id);
                }
            }
        }
    }

    /// Resolves a constant's initialiser. Constants live in the global scope, so
    /// the initialiser sees no locals - only other globals.
    fn resolve_const(&mut self, decl: &ConstDecl) {
        self.resolve_expr(&decl.value);
    }

    fn resolve_fn(&mut self, decl: &FnDecl) {
        self.push_scope();
        // Parameters share one scope; duplicates are an error (not shadowing).
        let mut seen: HashMap<&str, Span> = HashMap::new();
        for param in &decl.params {
            if let Some(&first) = seen.get(param.name.name.as_str()) {
                self.diags.emit(
                    Diagnostic::error(
                        DiagCode::DuplicateParameter,
                        format!("duplicate parameter `{}`", param.name.name),
                    )
                    .with_primary(param.name.span, "redefined here")
                    .with_label(first, "first defined here"),
                );
            } else {
                seen.insert(&param.name.name, param.name.span);
            }
            self.declare(param.id, &param.name.name, false, param.name.span);
        }
        // The body's own block scope nests inside the parameter scope.
        self.resolve_block(&decl.body);
        self.pop_scope();
    }

    fn resolve_block(&mut self, block: &Block) {
        self.push_scope();
        for stmt in &block.stmts {
            self.resolve_stmt(stmt);
        }
        if let Some(tail) = &block.tail {
            self.resolve_expr(tail);
        }
        self.pop_scope();
    }

    fn resolve_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let(l) => {
                // Resolve the initialiser first, then bring the name into scope,
                // so the initialiser cannot see the binding it defines.
                self.resolve_expr(&l.init);
                self.declare(l.id, &l.name.name, l.mutable, l.name.span);
            }
            StmtKind::Expr(e) => self.resolve_expr(e),
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    self.resolve_expr(e);
                }
            }
            StmtKind::While(w) => {
                self.resolve_expr(&w.cond);
                self.resolve_block(&w.body);
            }
            StmtKind::For(f) => {
                // The range bounds are resolved in the outer scope; the loop
                // variable is then introduced for the body only.
                self.resolve_expr(&f.start);
                self.resolve_expr(&f.end);
                self.push_scope();
                self.declare(f.id, &f.var.name, false, f.var.span);
                self.resolve_block(&f.body);
                self.pop_scope();
            }
            StmtKind::ForEach(f) => {
                // The array is resolved in the outer scope; the element variable
                // is then introduced for the body only.
                self.resolve_expr(&f.iterable);
                self.push_scope();
                self.declare(f.id, &f.var.name, false, f.var.span);
                self.resolve_block(&f.body);
                self.pop_scope();
            }
            StmtKind::Break | StmtKind::Continue => {}
        }
    }

    fn resolve_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_) => {}
            ExprKind::Name(name) => self.resolve_name(expr.id, name, expr.span),
            ExprKind::Unary { rhs, .. } => self.resolve_expr(rhs),
            ExprKind::Binary { lhs, rhs, .. } => {
                self.resolve_expr(lhs);
                self.resolve_expr(rhs);
            }
            ExprKind::Call { callee, args } => {
                self.resolve_expr(callee);
                for arg in args {
                    self.resolve_expr(arg);
                }
            }
            ExprKind::Assign { target, value } => {
                self.resolve_expr(target);
                self.resolve_expr(value);
            }
            ExprKind::AssignOp { target, value, .. } => {
                self.resolve_expr(target);
                self.resolve_expr(value);
            }
            ExprKind::ArrayLit(elems) => {
                for e in elems {
                    self.resolve_expr(e);
                }
            }
            ExprKind::Index { base, index } => {
                self.resolve_expr(base);
                self.resolve_expr(index);
            }
            ExprKind::StructLit { fields, .. } => {
                // The struct name is checked against the struct table during
                // type checking; here we resolve the field value expressions.
                for field in fields {
                    self.resolve_expr(&field.value);
                }
            }
            ExprKind::Field { base, .. } => {
                // The field name is resolved against the struct's declaration
                // during type checking.
                self.resolve_expr(base);
            }
            ExprKind::TupleLit(elems) => {
                for e in elems {
                    self.resolve_expr(e);
                }
            }
            ExprKind::TupleIndex { base, .. } => self.resolve_expr(base),
            ExprKind::If(if_expr) => {
                self.resolve_expr(&if_expr.cond);
                self.resolve_block(&if_expr.then_branch);
                if let Some(else_branch) = &if_expr.else_branch {
                    self.resolve_expr(else_branch);
                }
            }
            ExprKind::Match(m) => {
                self.resolve_expr(&m.scrutinee);
                // Patterns are literals, so they bind nothing; resolve bodies.
                for arm in &m.arms {
                    self.resolve_expr(&arm.body);
                }
            }
            ExprKind::Block(block) => self.resolve_block(block),
        }
    }

    /// Resolves a single name use against locals, then functions, then builtins.
    fn resolve_name(&mut self, use_id: NodeId, name: &str, span: Span) {
        if let Some(def) = self.lookup_local(name) {
            self.res.uses.insert(use_id, Res::Local(def));
        } else if let Some(fn_id) = self.res.functions.lookup(name) {
            self.res.uses.insert(use_id, Res::Fn(fn_id));
        } else if let Some(const_id) = self.res.consts.lookup(name) {
            self.res.uses.insert(use_id, Res::Const(const_id));
        } else if let Some(builtin) = Builtin::from_name(name) {
            self.res.uses.insert(use_id, Res::Builtin(builtin));
        } else {
            let mut diag = Diagnostic::error(
                DiagCode::UnresolvedName,
                format!("cannot find `{name}` in this scope"),
            )
            .with_primary(span, "not found in this scope");
            if let Some(hint) = self.suggest_name(name) {
                diag = diag.with_help(format!("did you mean `{hint}`?"));
            }
            self.diags.emit(diag);
        }
    }

    /// Suggests the in-scope name closest to `name` (locals, functions,
    /// constants, builtins), for a "did you mean …?" help line.
    fn suggest_name(&self, name: &str) -> Option<String> {
        let mut candidates: Vec<&str> = Vec::new();
        for scope in &self.scopes {
            candidates.extend(scope.keys().map(String::as_str));
        }
        candidates.extend(self.res.functions.by_name.keys().map(String::as_str));
        candidates.extend(self.res.consts.by_name.keys().map(String::as_str));
        candidates.extend(Builtin::ALL.iter().map(|b| b.name()));
        candidates.sort_unstable();
        candidates.dedup();
        crate::suggest::closest(name, candidates).map(str::to_string)
    }

    // ---- scope management ----

    fn push_scope(&mut self) {
        self.scopes.push(Scope::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Introduces a binding in the innermost scope and records its info.
    fn declare(&mut self, def: NodeId, name: &str, mutable: bool, span: Span) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), def);
        }
        self.res.locals.insert(
            def,
            LocalInfo {
                name: name.to_string(),
                mutable,
                span,
            },
        );
    }

    /// Finds the nearest enclosing definition of `name`.
    fn lookup_local(&self, name: &str) -> Option<NodeId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }
}

#[cfg(test)]
mod tests;

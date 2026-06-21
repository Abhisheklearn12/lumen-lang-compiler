//! The High-level Intermediate Representation.
//!
//! HIR is the AST after name resolution and type checking have been "baked in".
//! It is the representation the optimizer and code generator work on, and it is
//! deliberately **self-contained**: unlike the AST, it carries no [`NodeId`]s
//! and depends on no external side tables. Every expression records its
//! [`Type`]; every variable reference is a dense [`LocalId`]; every call names
//! its target directly as a [`Callee`].
//!
//! # What lowering desugars
//!
//! * Names → [`LocalId`] (locals/parameters) or a [`Callee`] (functions).
//! * Parameters and `let` bindings are unified into a single per-function array
//!   of [`LocalDecl`]s, so the backend can assign stack slots by index.
//! * Types annotations and inference results become concrete [`Type`]s on nodes.
//!
//! Operators ([`UnOp`], [`BinOp`]) are reused from the AST: they are language
//! constants, not syntax-phase state, so re-defining them would be needless
//! duplication.

pub mod lower;
pub mod print;

pub use lower::lower;
pub use print::print_hir;

#[cfg(test)]
mod tests;

use crate::sema::types::{Builtin, Type};
use crate::span::Span;

// Operators are pure language data; share the AST's definitions.
pub use crate::parser::ast::{BinOp, UnOp};

/// Identifies a function by dense index, shared with name resolution so a
/// [`Callee::Fn`] indexes directly into [`Hir::functions`].
pub use crate::sema::resolve::FnId;

/// A local slot within a function: parameters first, then `let` bindings, in
/// declaration order. Indexes into [`Function::locals`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

/// A fully-lowered program.
#[derive(Debug)]
pub struct Hir {
    /// Functions indexed by [`FnId`].
    pub functions: Vec<Function>,
    /// The entry point.
    pub main: FnId,
}

/// A lowered function.
#[derive(Debug)]
pub struct Function {
    pub name: String,
    /// All locals; the first `param_count` are the parameters.
    pub locals: Vec<LocalDecl>,
    pub param_count: usize,
    pub ret: Type,
    pub body: Block,
}

impl Function {
    /// The parameter locals.
    pub fn params(&self) -> &[LocalDecl] {
        &self.locals[..self.param_count]
    }
}

/// Declaration of a single local slot.
#[derive(Debug, Clone)]
pub struct LocalDecl {
    pub name: String,
    pub ty: Type,
}

/// A lowered block: statements followed by an optional value-producing tail.
#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Box<Expr>>,
    /// The block's value type (the tail's type, or `unit`).
    pub ty: Type,
}

/// A lowered statement.
#[derive(Debug, Clone)]
pub enum Stmt {
    /// Initialise a local slot.
    Let { local: LocalId, value: Expr },
    /// Evaluate an expression for effect, discarding its value.
    Expr(Expr),
    /// Return from the enclosing function.
    Return(Option<Expr>),
    /// Loop while `cond` holds.
    While { cond: Expr, body: Block },
    /// Count `var` over the half-open range `[start, end)`.
    ///
    /// Kept as a distinct node (rather than desugared to `while`) so the code
    /// generator can route `continue` to the increment rather than the
    /// condition, preserving correct loop semantics. `end_var` is a hidden slot
    /// that caches the upper bound, so `end` is evaluated exactly once even if
    /// it has side effects.
    For {
        var: LocalId,
        end_var: LocalId,
        start: Expr,
        end: Expr,
        body: Block,
    },
    /// Exit the innermost loop.
    Break,
    /// Jump to the next iteration of the innermost loop.
    Continue,
}

/// A lowered, fully-typed expression.
#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Type,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    /// Read a local/parameter slot.
    Local(LocalId),
    Unary {
        op: UnOp,
        rhs: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// Call a function or builtin (Lumen has no indirect calls).
    Call {
        callee: Callee,
        args: Vec<Expr>,
    },
    /// Assign to a local slot; evaluates to `unit`.
    Assign {
        local: LocalId,
        value: Box<Expr>,
    },
    /// An array literal; evaluates to a fresh array.
    ArrayLit(Vec<Expr>),
    /// Read `base[index]`.
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    /// Store `base[index] = value`; evaluates to `unit`.
    SetIndex {
        base: Box<Expr>,
        index: Box<Expr>,
        value: Box<Expr>,
    },
    /// A struct value, with field values in declaration order.
    StructLit(Vec<Expr>),
    /// Read field `idx` of a struct.
    GetField {
        base: Box<Expr>,
        idx: u32,
    },
    /// Store field `idx` of a struct; evaluates to `unit`.
    SetField {
        base: Box<Expr>,
        idx: u32,
        value: Box<Expr>,
    },
    If {
        cond: Box<Expr>,
        then_branch: Block,
        else_branch: Option<Box<Expr>>,
    },
    Block(Block),
}

/// The target of a [`ExprKind::Call`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callee {
    Fn(FnId),
    Builtin(Builtin),
}

impl Expr {
    /// Constructs an expression node.
    pub fn new(kind: ExprKind, ty: Type, span: Span) -> Expr {
        Expr { kind, ty, span }
    }
}

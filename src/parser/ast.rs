//! The abstract syntax tree produced by the [parser](super).
//!
//! The AST is a faithful, lightly-structured mirror of the source: it keeps
//! enough shape to type-check and lower, but performs no desugaring (that
//! happens in [HIR lowering](crate::hir)). Every node carries a [`Span`] so any
//! later phase can point diagnostics back at the source.
//!
//! # Node identities
//!
//! Selected nodes carry a [`NodeId`], a small dense integer assigned by the
//! parser. Later phases attach information to nodes through side tables keyed by
//! `NodeId` (name resolution → [`Res`](crate::sema::Res); type checking →
//! types) rather than mutating the tree. This keeps the AST immutable and the
//! per-phase data cleanly separated, exactly as the architecture requires.

use crate::span::Span;

/// A dense, unique identifier for an AST node.
///
/// Assigned by the parser via [`NodeIdGen`]. Used as the key for the side
/// tables that resolution and type checking produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

/// Monotonic allocator of [`NodeId`]s, owned by the parser.
#[derive(Debug, Default)]
pub struct NodeIdGen {
    next: u32,
}

impl NodeIdGen {
    pub fn new() -> NodeIdGen {
        NodeIdGen::default()
    }

    /// Returns a fresh id, never previously returned by this generator.
    pub fn fresh(&mut self) -> NodeId {
        let id = NodeId(self.next);
        self.next += 1;
        id
    }

    /// The number of ids handed out so far, i.e. an upper bound on any `NodeId`.
    pub fn count(&self) -> usize {
        self.next as usize
    }
}

/// An identifier occurrence: its text plus the span it was written at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

/// The root of a parsed program: a flat list of top-level items.
#[derive(Debug, Clone)]
pub struct Ast {
    pub items: Vec<Item>,
}

/// A top-level item: a function, a constant, or a struct declaration.
#[derive(Debug, Clone)]
pub struct Item {
    pub id: NodeId,
    pub kind: ItemKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ItemKind {
    Fn(FnDecl),
    Const(ConstDecl),
    Struct(StructDecl),
}

/// A struct declaration: `struct Name { field: T, ... }`.
#[derive(Debug, Clone)]
pub struct StructDecl {
    pub id: NodeId,
    pub name: Ident,
    pub fields: Vec<FieldDef>,
}

/// A single declared field.
#[derive(Debug, Clone)]
pub struct FieldDef {
    pub name: Ident,
    pub ty: TypeExpr,
    pub span: Span,
}

/// A top-level constant: `const NAME: T = value;`. The value must be a
/// compile-time constant expression; it is inlined at each use during lowering.
#[derive(Debug, Clone)]
pub struct ConstDecl {
    pub id: NodeId,
    pub name: Ident,
    pub ty: TypeExpr,
    pub value: Expr,
}

/// A function declaration: signature plus body block.
#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: Ident,
    pub params: Vec<Param>,
    /// The written return type, or `None` for the implicit `unit` return.
    pub ret: Option<TypeExpr>,
    pub body: Block,
}

/// A single function parameter.
#[derive(Debug, Clone)]
pub struct Param {
    pub id: NodeId,
    pub name: Ident,
    pub ty: TypeExpr,
    pub span: Span,
}

/// A syntactic type annotation, e.g. `i64`. Resolved to a semantic
/// [`Type`](crate::sema::Type) during type checking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeExprKind {
    /// A named type such as `i64`, `bool`, or `str`.
    Named(String),
    /// An array type `[T]`.
    Array(Box<TypeExpr>),
    /// A tuple type `(T1, T2, ...)` with two or more elements.
    Tuple(Vec<TypeExpr>),
    /// A placeholder inserted by the parser after a malformed annotation, so
    /// recovery can continue. Type checking treats it as the error type and
    /// emits no further diagnostic.
    Error,
}

/// A braced block: a sequence of statements with an optional trailing
/// expression whose value becomes the block's value (`unit` if absent).
#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Box<Expr>>,
    pub span: Span,
}

/// A statement.
#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    /// `let [mut] name [: ty] = init;`
    Let(LetStmt),
    /// An expression evaluated for its side effects (`expr;`).
    Expr(Expr),
    /// `return [expr];`
    Return(Option<Expr>),
    /// `while cond { body }`
    While(WhileStmt),
    /// `for var in start..end { body }`
    For(ForStmt),
    /// `for var in array { body }`
    ForEach(ForEachStmt),
    /// `break;` - exit the innermost enclosing loop.
    Break,
    /// `continue;` - skip to the next iteration of the innermost loop.
    Continue,
}

#[derive(Debug, Clone)]
pub struct LetStmt {
    pub id: NodeId,
    pub name: Ident,
    pub mutable: bool,
    pub ty: Option<TypeExpr>,
    pub init: Expr,
}

#[derive(Debug, Clone)]
pub struct WhileStmt {
    pub cond: Expr,
    pub body: Block,
}

/// `for var in start..end { body }`. The loop variable is an `i64` that takes
/// each value in the half-open range `[start, end)`.
#[derive(Debug, Clone)]
pub struct ForStmt {
    /// Definition id of the loop variable, for resolution/lowering.
    pub id: NodeId,
    pub var: Ident,
    pub start: Expr,
    pub end: Expr,
    pub body: Block,
}

/// `for var in array { body }`. The loop variable binds each element of the
/// array in turn, so its type is the array's element type.
#[derive(Debug, Clone)]
pub struct ForEachStmt {
    /// Definition id of the loop variable, for resolution/lowering.
    pub id: NodeId,
    pub var: Ident,
    pub iterable: Expr,
    pub body: Block,
}

/// An expression.
#[derive(Debug, Clone)]
pub struct Expr {
    pub id: NodeId,
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    /// A reference to a name (variable, parameter, or function).
    Name(String),
    Unary {
        op: UnOp,
        rhs: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    /// `target = value`. The target's validity as an lvalue is checked in
    /// type checking, not the parser.
    Assign {
        target: Box<Expr>,
        value: Box<Expr>,
    },
    /// `target op= value`, e.g. `x += 1`. Desugared to a plain assignment
    /// during lowering.
    AssignOp {
        target: Box<Expr>,
        op: BinOp,
        value: Box<Expr>,
    },
    /// An array literal `[e0, e1, ...]`.
    ArrayLit(Vec<Expr>),
    /// An indexing expression `base[index]`.
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    /// A struct literal `Name { field: value, ... }`.
    StructLit {
        name: Ident,
        fields: Vec<FieldInit>,
    },
    /// Field access `base.field`.
    Field {
        base: Box<Expr>,
        field: Ident,
    },
    /// A tuple literal `(e0, e1, ...)` with two or more elements.
    TupleLit(Vec<Expr>),
    /// Tuple element access `base.0`, `base.1`, ...
    TupleIndex {
        base: Box<Expr>,
        index: usize,
        /// Span of the numeric index, for diagnostics.
        index_span: Span,
    },
    If(IfExpr),
    /// `match scrutinee { pat => body, ... }`.
    Match(MatchExpr),
    Block(Block),
}

/// `match scrutinee { arm, arm, ... }`. The scrutinee is an integer or boolean;
/// each arm matches a literal value or the wildcard `_`. Desugared to a chain
/// of `if`/`else` during lowering, so it adds no node to the HIR.
#[derive(Debug, Clone)]
pub struct MatchExpr {
    pub scrutinee: Box<Expr>,
    pub arms: Vec<MatchArm>,
}

/// One `pattern => body` arm of a [`MatchExpr`].
#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Expr,
    pub span: Span,
}

/// A `match` arm pattern: a scalar literal or the catch-all wildcard.
#[derive(Debug, Clone)]
pub enum Pattern {
    Int(i64),
    Bool(bool),
    /// The wildcard `_`, matching any value.
    Wild,
}

/// A `field: value` pair in a struct literal.
#[derive(Debug, Clone)]
pub struct FieldInit {
    pub name: Ident,
    pub value: Expr,
}

/// `if cond { then } [else (block | if)]`. An `if` is an expression; its value
/// is that of the taken branch (or `unit` when there is no `else`).
#[derive(Debug, Clone)]
pub struct IfExpr {
    pub cond: Box<Expr>,
    pub then_branch: Block,
    /// Always a `Block` or another `If` expression when present.
    pub else_branch: Option<Box<Expr>>,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// Arithmetic negation `-`.
    Neg,
    /// Logical negation `!`.
    Not,
}

impl UnOp {
    pub fn symbol(self) -> &'static str {
        match self {
            UnOp::Neg => "-",
            UnOp::Not => "!",
        }
    }
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        use BinOp::*;
        match self {
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Rem => "%",
            Eq => "==",
            Ne => "!=",
            Lt => "<",
            Le => "<=",
            Gt => ">",
            Ge => ">=",
            And => "&&",
            Or => "||",
        }
    }

    /// Whether this is a comparison/equality operator (result is always `bool`).
    pub fn is_comparison(self) -> bool {
        use BinOp::*;
        matches!(self, Eq | Ne | Lt | Le | Gt | Ge)
    }

    /// Whether this is a short-circuiting logical operator.
    pub fn is_logical(self) -> bool {
        matches!(self, BinOp::And | BinOp::Or)
    }
}

impl Expr {
    /// Whether this expression form may stand as a statement without a trailing
    /// semicolon (block-like: `{ … }` and `if …`). Used by the parser's
    /// statement loop to mirror Rust's ergonomics.
    pub fn is_block_like(&self) -> bool {
        matches!(self.kind, ExprKind::Block(_) | ExprKind::If(_))
    }
}

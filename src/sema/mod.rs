//! Semantic analysis: the phases that run between parsing and lowering.
//!
//! * [`types`]  the shared [`Type`] representation and [`Builtin`] signatures.
//! * [`resolve`]  name resolution, binding identifier uses to definitions.
//! * [`typeck`]  type checking, assigning and verifying types across the AST.
//!
//! These phases consume the immutable [`Ast`](crate::parser::ast) and produce
//! side tables (e.g. [`Resolution`]) keyed by
//! [`NodeId`](crate::parser::ast::NodeId), which later phases read to build a
//! fully-typed tree.

pub mod resolve;
pub mod typeck;
pub mod types;

pub use resolve::{ConstId, FnId, Res, Resolution, StructId, resolve};
pub use typeck::{ConstValue, FnSig, StructInfo, Typeck, check};
pub use types::{Builtin, Elem, Type};

//! The Mid-level Intermediate Representation: a control-flow graph in
//! three-address form.
//!
//! # Why a second IR?
//!
//! [`Hir`](crate::hir) is a typed *tree* - convenient for type-directed code
//! generation, but awkward for the classic data-flow optimizations, which want
//! an explicit control-flow graph and instructions with named results. MIR
//! provides exactly that: each [`Function`] is a list of [`Block`]s, each block
//! a straight-line sequence of [`Inst`]s ending in a [`Terminator`]. Nested
//! expressions are flattened into temporaries ([`Reg`]), and all control flow -
//! `if`, `while`, `for`, and short-circuit `&&`/`||` - becomes explicit branches.
//!
//! On this form the [optimizer](opt) runs constant folding, constant and copy
//! propagation, dead-code elimination, and CFG simplification, none of which are
//! natural on a tree.
//!
//! MIR is built from optimized HIR and is inspectable via `lumenc dump mir`; it
//! is an analysis and optimization layer that complements the HIR→bytecode code
//! generator.

pub mod build;
pub mod dot;
pub mod interp;
pub mod opt;
pub mod print;

pub use build::build;
pub use dot::to_dot;
pub use interp::interpret;
pub use opt::{MirStats, optimize};
pub use print::print_mir;

use crate::hir::{BinOp, Callee, LocalDecl, UnOp};
use crate::sema::types::Type;
use std::rc::Rc;

/// A basic-block identifier, dense within a function.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

/// A virtual register (SSA-like temporary), dense within a function.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Reg(pub u32);

/// A local-variable slot, shared with HIR's numbering.
pub use crate::hir::LocalId;

/// A whole program in MIR form.
#[derive(Debug)]
pub struct Program {
    pub functions: Vec<Function>,
    /// Index of the entry function (`main`) in [`Program::functions`].
    pub main: usize,
}

/// A function: its locals, register count, and control-flow graph.
#[derive(Debug)]
pub struct Function {
    pub name: String,
    pub param_count: usize,
    pub locals: Vec<LocalDecl>,
    /// Number of virtual registers allocated.
    pub reg_count: usize,
    pub blocks: Vec<Block>,
    pub entry: BlockId,
}

impl Function {
    pub fn block(&self, id: BlockId) -> &Block {
        &self.blocks[id.0 as usize]
    }

    pub fn block_mut(&mut self, id: BlockId) -> &mut Block {
        &mut self.blocks[id.0 as usize]
    }
}

/// A basic block: straight-line instructions then a terminator.
#[derive(Debug)]
pub struct Block {
    pub insts: Vec<Inst>,
    pub term: Terminator,
}

/// A compile-time constant operand.
#[derive(Debug, Clone, PartialEq)]
pub enum Const {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(Rc<str>),
    Unit,
}

/// An instruction operand: a constant or a register.
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    Const(Const),
    Reg(Reg),
}

impl Operand {
    /// The register this operand reads, if any.
    pub fn reg(&self) -> Option<Reg> {
        match self {
            Operand::Reg(r) => Some(*r),
            Operand::Const(_) => None,
        }
    }
}

/// A value-producing computation assigned to a register.
#[derive(Debug, Clone)]
pub enum Rvalue {
    /// Copy an operand.
    Use(Operand),
    /// Read a local variable.
    Load(LocalId),
    Unary(UnOp, Operand),
    Binary(BinOp, Operand, Operand),
    /// `a ++ b` string concatenation.
    Concat(Operand, Operand),
    /// Build an array from element operands.
    MakeArray(Vec<Operand>),
    /// `base[index]`.
    Index(Operand, Operand),
}

/// A single MIR instruction. All have an explicit result register except the
/// effecting stores.
#[derive(Debug)]
pub enum Inst {
    /// `dst = rvalue`
    Assign { dst: Reg, rvalue: Rvalue },
    /// `local = src`
    Store { local: LocalId, src: Operand },
    /// `base[index] = value`
    SetIndex {
        base: Operand,
        index: Operand,
        value: Operand,
    },
    /// `dst = callee(args)` (may have side effects).
    Call {
        dst: Reg,
        callee: Callee,
        args: Vec<Operand>,
        ret: Type,
    },
}

/// How a block transfers control.
#[derive(Debug, Clone)]
pub enum Terminator {
    /// Jump unconditionally.
    Goto(BlockId),
    /// Branch on a boolean operand.
    Branch {
        cond: Operand,
        then_bb: BlockId,
        else_bb: BlockId,
    },
    /// Return a value from the function.
    Return(Operand),
    /// Placeholder used transiently while building; never present in finished
    /// MIR.
    Unreachable,
}

impl Terminator {
    /// The blocks this terminator may transfer control to.
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Terminator::Goto(b) => vec![*b],
            Terminator::Branch {
                then_bb, else_bb, ..
            } => vec![*then_bb, *else_bb],
            Terminator::Return(_) | Terminator::Unreachable => vec![],
        }
    }
}

#[cfg(test)]
mod tests;

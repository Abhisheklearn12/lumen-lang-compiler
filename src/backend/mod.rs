//! The backend: bytecode definition, code generation, the VM, and a
//! disassembler.
//!
//! * [`bytecode`]  [`Value`](bytecode::Value), the [`Op`](bytecode::Op)
//!   instruction set, and the [`Program`](bytecode::Program) container.
//! * [`codegen`]  lowers [`Hir`](crate::hir) to a [`Program`].
//! * [`vm`]  executes a [`Program`], capturing output.
//! * [`disasm`]  renders bytecode for inspection and snapshot tests.
//!
//! The backend depends only on [`Hir`](crate::hir) (and the shared
//! [`types`](crate::sema::types)); it knows nothing of the AST, tokens, or
//! diagnostics, keeping the phase boundary clean.

pub mod builtins;
pub mod bytecode;
pub mod c;
pub mod codegen;
pub mod disasm;
pub mod object;
pub mod peephole;
pub mod verify;
pub mod vm;

pub use bytecode::{Op, Program, Value};
pub use c::{CError, emit_c};
pub use codegen::generate;
pub use disasm::disassemble;
pub use verify::{VerifyError, verify};
pub use vm::{Execution, VmError, execute, execute_with_limit};

#[cfg(test)]
mod tests;

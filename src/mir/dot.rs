//! Graphviz DOT export of the MIR control-flow graph.
//!
//! `lumenc dump cfg <file>` emits a `digraph` per function that can be rendered
//! with Graphviz (`dot -Tpng`). Each basic block is a node listing its
//! instructions and terminator; edges follow the terminator's successors, with
//! the two arms of a conditional branch labelled `T` (true) and `F` (false).
//!
//! The rendering is deterministic, so it is also snapshot-testable.

use std::fmt::Write as _;

use crate::hir::Callee;
use crate::mir::*;
use crate::sema::types::Builtin;

/// Renders the whole program's CFG as one or more DOT digraphs.
pub fn to_dot(program: &Program) -> String {
    let mut out = String::new();
    for func in &program.functions {
        function_dot(&mut out, func);
    }
    out
}

fn function_dot(out: &mut String, func: &Function) {
    let _ = writeln!(out, "digraph \"{}\" {{", func.name);
    out.push_str("  node [shape=box, fontname=\"monospace\"];\n");
    for (i, block) in func.blocks.iter().enumerate() {
        let mut label = format!("bb{i}");
        if BlockId(i as u32) == func.entry {
            label.push_str(" (entry)");
        }
        for inst in &block.insts {
            let _ = write!(label, "\\l{}", escape(&inst_str(inst)));
        }
        let _ = write!(label, "\\l{}", escape(&term_str(&block.term)));
        let _ = writeln!(out, "  bb{i} [label=\"{label}\\l\"];");
    }
    for (i, block) in func.blocks.iter().enumerate() {
        match &block.term {
            Terminator::Goto(t) => {
                let _ = writeln!(out, "  bb{i} -> bb{};", t.0);
            }
            Terminator::Branch {
                then_bb, else_bb, ..
            } => {
                let _ = writeln!(out, "  bb{i} -> bb{} [label=\"T\"];", then_bb.0);
                let _ = writeln!(out, "  bb{i} -> bb{} [label=\"F\"];", else_bb.0);
            }
            Terminator::Return(_) | Terminator::Unreachable => {}
        }
    }
    out.push_str("}\n");
}

/// Escapes characters that are special inside a DOT label string.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn inst_str(inst: &Inst) -> String {
    match inst {
        Inst::Assign { dst, rvalue } => format!("%{} = {}", dst.0, rvalue_str(rvalue)),
        Inst::Store { local, src } => format!("_{} <- {}", local.0, operand(src)),
        Inst::SetIndex { base, index, value } => {
            format!(
                "{}[{}] <- {}",
                operand(base),
                operand(index),
                operand(value)
            )
        }
        Inst::Call {
            dst, callee, args, ..
        } => {
            let args = args.iter().map(operand).collect::<Vec<_>>().join(", ");
            format!("%{} = call {}({})", dst.0, callee_str(*callee), args)
        }
    }
}

fn rvalue_str(rvalue: &Rvalue) -> String {
    match rvalue {
        Rvalue::Use(o) => operand(o),
        Rvalue::Load(l) => format!("load _{}", l.0),
        Rvalue::Unary(op, o) => format!("{}{}", op.symbol(), operand(o)),
        Rvalue::Binary(op, a, b) => format!("{} {} {}", operand(a), op.symbol(), operand(b)),
        Rvalue::Concat(a, b) => format!("{} ++ {}", operand(a), operand(b)),
        Rvalue::MakeArray(elems) => {
            format!(
                "[{}]",
                elems.iter().map(operand).collect::<Vec<_>>().join(", ")
            )
        }
        Rvalue::Index(b, i) => format!("{}[{}]", operand(b), operand(i)),
    }
}

fn term_str(term: &Terminator) -> String {
    match term {
        Terminator::Goto(b) => format!("goto bb{}", b.0),
        Terminator::Branch { cond, .. } => format!("branch {}", operand(cond)),
        Terminator::Return(o) => format!("return {}", operand(o)),
        Terminator::Unreachable => "unreachable".to_string(),
    }
}

fn operand(o: &Operand) -> String {
    match o {
        Operand::Const(c) => const_str(c),
        Operand::Reg(r) => format!("%{}", r.0),
    }
}

fn const_str(c: &Const) -> String {
    match c {
        Const::Int(v) => v.to_string(),
        Const::Float(v) => v.to_string(),
        Const::Bool(v) => v.to_string(),
        Const::Str(v) => format!("{v:?}"),
        Const::Unit => "unit".to_string(),
    }
}

fn callee_str(callee: Callee) -> String {
    match callee {
        Callee::Fn(id) => format!("fn#{}", id.0),
        Callee::Builtin(b) => format!("@{}", Builtin::name(b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Diagnostics;
    use crate::hir::lower;
    use crate::lexer::tokenize;
    use crate::mir::build;
    use crate::parser::parse;
    use crate::sema::{check, resolve};
    use crate::source::SourceFile;

    fn dot_of(src: &str) -> String {
        let file = SourceFile::new("t.lm", src);
        let mut diags = Diagnostics::new();
        let tokens = tokenize(&file, &mut diags);
        let ast = parse(tokens, &mut diags);
        let res = resolve(&ast, &mut diags);
        let tc = check(&ast, &res, &mut diags);
        assert!(!diags.has_errors());
        let hir = lower(&ast, &res, &tc);
        to_dot(&build(&hir))
    }

    #[test]
    fn emits_a_digraph_with_edges() {
        let dot = dot_of("fn main() { let mut i = 0; while i < 3 { i = i + 1; } }");
        assert!(dot.contains("digraph \"main\""));
        assert!(dot.contains("->"), "no edges in CFG");
        assert!(dot.contains("[label=\"T\"]"), "branch arms not labelled");
    }

    #[test]
    fn is_deterministic() {
        let a = dot_of("fn main() { print_int(1); }");
        let b = dot_of("fn main() { print_int(1); }");
        assert_eq!(a, b);
    }
}

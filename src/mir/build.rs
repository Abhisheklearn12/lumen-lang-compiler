//! Lowering from [`Hir`](crate::hir) to [`Program`](super::Program) MIR.
//!
//! Expressions are flattened into a sequence of three-address instructions
//! writing to fresh [`Reg`]s, and every control-flow construct is turned into
//! explicit basic blocks and branches:
//!
//! * `if` evaluates each arm into a synthesised result local, then both arms
//!   jump to a common merge block that loads it.
//! * `while`/`for` become header/body/after blocks; `break`/`continue` jump to
//!   the after/continuation block of the innermost loop (tracked on a stack).
//! * `&&`/`||` short-circuit through a temporary local, mirroring their runtime
//!   semantics.
//!
//! The result is a CFG where the only values that cross block boundaries do so
//! through locals, which keeps the builder simple while still giving the
//! optimizer an explicit graph to work on.

use crate::hir::{self, ExprKind, Hir, Stmt};
use crate::mir::*;
use crate::sema::types::Type;

/// Lowers a whole HIR program to MIR.
#[tracing::instrument(level = "debug", skip_all)]
pub fn build(hir: &Hir) -> Program {
    let functions = hir.functions.iter().map(build_function).collect();
    Program {
        functions,
        main: hir.main.0 as usize,
    }
}

fn build_function(func: &hir::Function) -> Function {
    let mut b = Builder {
        locals: func.locals.clone(),
        reg_count: 0,
        blocks: Vec::new(),
        cur: BlockId(0),
        loops: Vec::new(),
    };
    let entry = b.new_block();
    b.cur = entry;
    let value = b.lower_block(&func.body);
    b.terminate(Terminator::Return(value));
    Function {
        name: func.name.clone(),
        param_count: func.param_count,
        locals: b.locals,
        reg_count: b.reg_count,
        blocks: b.blocks,
        entry,
    }
}

/// The continuation targets of an enclosing loop.
struct Loop {
    continue_bb: BlockId,
    break_bb: BlockId,
}

struct Builder {
    locals: Vec<LocalDecl>,
    reg_count: usize,
    blocks: Vec<Block>,
    /// The block instructions are currently appended to.
    cur: BlockId,
    loops: Vec<Loop>,
}

impl Builder {
    fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(Block {
            insts: Vec::new(),
            term: Terminator::Unreachable,
        });
        id
    }

    fn fresh_reg(&mut self) -> Reg {
        let r = Reg(self.reg_count as u32);
        self.reg_count += 1;
        r
    }

    /// Allocates a fresh, compiler-internal local of the given type.
    fn fresh_local(&mut self, ty: Type) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(LocalDecl {
            name: format!("<t{}>", id.0),
            ty,
        });
        id
    }

    fn emit(&mut self, inst: Inst) {
        self.blocks[self.cur.0 as usize].insts.push(inst);
    }

    /// Sets the current block's terminator (only if still unset) and is a no-op
    /// on an already-terminated block.
    fn terminate(&mut self, term: Terminator) {
        let block = &mut self.blocks[self.cur.0 as usize];
        if matches!(block.term, Terminator::Unreachable) {
            block.term = term;
        }
    }

    /// Materialises an rvalue into a fresh register and returns it as an operand.
    fn push_value(&mut self, rvalue: Rvalue) -> Operand {
        let dst = self.fresh_reg();
        self.emit(Inst::Assign { dst, rvalue });
        Operand::Reg(dst)
    }

    // ---- blocks & statements ----

    fn lower_block(&mut self, block: &hir::Block) -> Operand {
        for stmt in &block.stmts {
            self.lower_stmt(stmt);
        }
        match &block.tail {
            Some(tail) => self.lower_expr(tail),
            None => Operand::Const(Const::Unit),
        }
    }

    fn lower_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let { local, value } => {
                let src = self.lower_expr(value);
                self.emit(Inst::Store { local: *local, src });
            }
            Stmt::Expr(e) => {
                self.lower_expr(e);
            }
            Stmt::Return(value) => {
                let op = match value {
                    Some(e) => self.lower_expr(e),
                    None => Operand::Const(Const::Unit),
                };
                self.terminate(Terminator::Return(op));
                // Subsequent statements are unreachable; continue in a dead block.
                self.cur = self.new_block();
            }
            Stmt::While { cond, body } => self.lower_while(cond, body),
            Stmt::For {
                var,
                end_var,
                start,
                end,
                body,
            } => self.lower_for(*var, *end_var, start, end, body),
            Stmt::Break => self.lower_jump(true),
            Stmt::Continue => self.lower_jump(false),
        }
    }

    fn lower_while(&mut self, cond: &hir::Expr, body: &hir::Block) {
        let header = self.new_block();
        let body_bb = self.new_block();
        let after = self.new_block();
        self.terminate(Terminator::Goto(header));

        self.cur = header;
        let cond_op = self.lower_expr(cond);
        self.terminate(Terminator::Branch {
            cond: cond_op,
            then_bb: body_bb,
            else_bb: after,
        });

        self.cur = body_bb;
        self.loops.push(Loop {
            continue_bb: header,
            break_bb: after,
        });
        self.lower_block(body);
        self.loops.pop();
        self.terminate(Terminator::Goto(header));

        self.cur = after;
    }

    fn lower_for(
        &mut self,
        var: LocalId,
        end_var: LocalId,
        start: &hir::Expr,
        end: &hir::Expr,
        body: &hir::Block,
    ) {
        let start_op = self.lower_expr(start);
        self.emit(Inst::Store {
            local: var,
            src: start_op,
        });
        let end_op = self.lower_expr(end);
        self.emit(Inst::Store {
            local: end_var,
            src: end_op,
        });

        let header = self.new_block();
        let body_bb = self.new_block();
        let incr = self.new_block();
        let after = self.new_block();
        self.terminate(Terminator::Goto(header));

        // header: var < end_var ?
        self.cur = header;
        let v = self.push_value(Rvalue::Load(var));
        let e = self.push_value(Rvalue::Load(end_var));
        let cond = self.push_value(Rvalue::Binary(hir::BinOp::Lt, v, e));
        self.terminate(Terminator::Branch {
            cond,
            then_bb: body_bb,
            else_bb: after,
        });

        // body
        self.cur = body_bb;
        self.loops.push(Loop {
            continue_bb: incr,
            break_bb: after,
        });
        self.lower_block(body);
        self.loops.pop();
        self.terminate(Terminator::Goto(incr));

        // incr: var = var + 1
        self.cur = incr;
        let v = self.push_value(Rvalue::Load(var));
        let next = self.push_value(Rvalue::Binary(
            hir::BinOp::Add,
            v,
            Operand::Const(Const::Int(1)),
        ));
        self.emit(Inst::Store {
            local: var,
            src: next,
        });
        self.terminate(Terminator::Goto(header));

        self.cur = after;
    }

    fn lower_jump(&mut self, is_break: bool) {
        if let Some(loop_ctx) = self.loops.last() {
            let target = if is_break {
                loop_ctx.break_bb
            } else {
                loop_ctx.continue_bb
            };
            self.terminate(Terminator::Goto(target));
        }
        // Continue lowering into a fresh (dead) block.
        self.cur = self.new_block();
    }

    // ---- expressions ----

    fn lower_expr(&mut self, expr: &hir::Expr) -> Operand {
        match &expr.kind {
            ExprKind::Int(v) => Operand::Const(Const::Int(*v)),
            ExprKind::Float(v) => Operand::Const(Const::Float(*v)),
            ExprKind::Bool(v) => Operand::Const(Const::Bool(*v)),
            ExprKind::Str(s) => Operand::Const(Const::Str(s.as_str().into())),
            ExprKind::Local(id) => self.push_value(Rvalue::Load(*id)),
            ExprKind::Unary { op, rhs } => {
                let o = self.lower_expr(rhs);
                self.push_value(Rvalue::Unary(*op, o))
            }
            ExprKind::Binary { op, lhs, rhs } => self.lower_binary(*op, lhs, rhs),
            ExprKind::Call { callee, args } => {
                let arg_ops = args.iter().map(|a| self.lower_expr(a)).collect();
                let dst = self.fresh_reg();
                self.emit(Inst::Call {
                    dst,
                    callee: *callee,
                    args: arg_ops,
                    ret: expr.ty,
                });
                Operand::Reg(dst)
            }
            ExprKind::Assign { local, value } => {
                let src = self.lower_expr(value);
                self.emit(Inst::Store { local: *local, src });
                Operand::Const(Const::Unit)
            }
            ExprKind::ArrayLit(elems) => {
                let ops = elems.iter().map(|e| self.lower_expr(e)).collect();
                self.push_value(Rvalue::MakeArray(ops))
            }
            ExprKind::Index { base, index } => {
                let b = self.lower_expr(base);
                let i = self.lower_expr(index);
                self.push_value(Rvalue::Index(b, i))
            }
            ExprKind::SetIndex { base, index, value } => {
                let b = self.lower_expr(base);
                let i = self.lower_expr(index);
                let v = self.lower_expr(value);
                self.emit(Inst::SetIndex {
                    base: b,
                    index: i,
                    value: v,
                });
                Operand::Const(Const::Unit)
            }
            // Structs share the array representation (fixed-length records).
            ExprKind::StructLit(fields) => {
                let ops = fields.iter().map(|e| self.lower_expr(e)).collect();
                self.push_value(Rvalue::MakeArray(ops))
            }
            ExprKind::GetField { base, idx } => {
                let b = self.lower_expr(base);
                self.push_value(Rvalue::Index(b, Operand::Const(Const::Int(*idx as i64))))
            }
            ExprKind::SetField { base, idx, value } => {
                let b = self.lower_expr(base);
                let v = self.lower_expr(value);
                self.emit(Inst::SetIndex {
                    base: b,
                    index: Operand::Const(Const::Int(*idx as i64)),
                    value: v,
                });
                Operand::Const(Const::Unit)
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => self.lower_if(cond, then_branch, else_branch.as_deref(), expr.ty),
            ExprKind::Block(block) => self.lower_block(block),
        }
    }

    fn lower_binary(&mut self, op: hir::BinOp, lhs: &hir::Expr, rhs: &hir::Expr) -> Operand {
        match op {
            hir::BinOp::And => self.lower_logical(lhs, rhs, false),
            hir::BinOp::Or => self.lower_logical(lhs, rhs, true),
            _ => {
                let l = self.lower_expr(lhs);
                let r = self.lower_expr(rhs);
                if op == hir::BinOp::Add && lhs.ty == Type::Str {
                    self.push_value(Rvalue::Concat(l, r))
                } else {
                    self.push_value(Rvalue::Binary(op, l, r))
                }
            }
        }
    }

    /// Lowers `a && b` (`or_else = false`) or `a || b` (`or_else = true`) with
    /// short-circuit evaluation through a result local.
    fn lower_logical(&mut self, lhs: &hir::Expr, rhs: &hir::Expr, or_else: bool) -> Operand {
        let result = self.fresh_local(Type::Bool);
        let l = self.lower_expr(lhs);

        let eval_rhs = self.new_block();
        let short = self.new_block();
        let merge = self.new_block();
        // For `&&`: if lhs is false, short-circuit; for `||`: if lhs is true.
        let (then_bb, else_bb) = if or_else {
            (short, eval_rhs)
        } else {
            (eval_rhs, short)
        };
        self.terminate(Terminator::Branch {
            cond: l,
            then_bb,
            else_bb,
        });

        self.cur = eval_rhs;
        let r = self.lower_expr(rhs);
        self.emit(Inst::Store {
            local: result,
            src: r,
        });
        self.terminate(Terminator::Goto(merge));

        self.cur = short;
        self.emit(Inst::Store {
            local: result,
            src: Operand::Const(Const::Bool(or_else)),
        });
        self.terminate(Terminator::Goto(merge));

        self.cur = merge;
        self.push_value(Rvalue::Load(result))
    }

    fn lower_if(
        &mut self,
        cond: &hir::Expr,
        then_branch: &hir::Block,
        else_branch: Option<&hir::Expr>,
        ty: Type,
    ) -> Operand {
        let result = self.fresh_local(ty);
        let cond_op = self.lower_expr(cond);
        let then_bb = self.new_block();
        let else_bb = self.new_block();
        let merge = self.new_block();
        self.terminate(Terminator::Branch {
            cond: cond_op,
            then_bb,
            else_bb,
        });

        self.cur = then_bb;
        let then_val = self.lower_block(then_branch);
        self.emit(Inst::Store {
            local: result,
            src: then_val,
        });
        self.terminate(Terminator::Goto(merge));

        self.cur = else_bb;
        let else_val = match else_branch {
            Some(e) => self.lower_expr(e),
            None => Operand::Const(Const::Unit),
        };
        self.emit(Inst::Store {
            local: result,
            src: else_val,
        });
        self.terminate(Terminator::Goto(merge));

        self.cur = merge;
        self.push_value(Rvalue::Load(result))
    }
}

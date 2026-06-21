//! A direct interpreter for MIR.
//!
//! This is a second execution engine: rather than generating bytecode and
//! running it on the stack [`vm`](crate::backend::vm), it walks the MIR
//! control-flow graph directly. Each call evaluates instructions into a flat
//! register file, follows terminators between basic blocks, and recurses for
//! function calls. Builtins are evaluated through the
//! [shared module](crate::backend::builtins), so both engines produce identical
//! output.
//!
//! Its main purpose is validation: a test runs a suite of programs through both
//! engines and asserts they agree, which exercises the whole MIR pipeline
//! (construction, optimization) against the trusted bytecode path. Because MIR
//! instructions are untyped, arithmetic dispatches on the runtime [`Value`].

use std::cell::RefCell;
use std::rc::Rc;

use crate::backend::builtins;
use crate::backend::bytecode::Value;
use crate::backend::vm::{Execution, VmError, index_in_bounds};
use crate::hir::{BinOp, Callee, UnOp};
use crate::mir::*;
use crate::sema::types::Builtin;

/// A generous step budget mirroring the stack VM's, so runaway MIR also stops.
const STEP_LIMIT: u64 = 50_000_000;

/// Interprets a MIR program from its entry point, returning the result and any
/// captured output.
#[tracing::instrument(level = "debug", skip_all)]
pub fn interpret(program: &Program) -> Result<Execution, VmError> {
    let mut interp = Interp {
        program,
        stdout: String::new(),
        steps: 0,
    };
    let value = interp.call(program.main, Vec::new())?;
    Ok(Execution {
        value,
        stdout: interp.stdout,
    })
}

struct Interp<'a> {
    program: &'a Program,
    stdout: String,
    steps: u64,
}

impl Interp<'_> {
    /// Runs the function at `idx` with `args`, returning its result value.
    fn call(&mut self, idx: usize, args: Vec<Value>) -> Result<Value, VmError> {
        let func = self
            .program
            .functions
            .get(idx)
            .ok_or(VmError::Internal("bad function"))?;
        let mut regs: Vec<Value> = vec![Value::Unit; func.reg_count];
        let mut locals: Vec<Value> = vec![Value::Unit; func.locals.len()];
        for (slot, arg) in args.into_iter().enumerate() {
            if let Some(local) = locals.get_mut(slot) {
                *local = arg;
            }
        }

        let mut block = func.entry;
        loop {
            self.steps += 1;
            if self.steps > STEP_LIMIT {
                return Err(VmError::StepLimitExceeded(STEP_LIMIT));
            }
            let bb = func.block(block);
            for inst in &bb.insts {
                self.exec(inst, &mut regs, &mut locals)?;
            }
            match &bb.term {
                Terminator::Goto(t) => block = *t,
                Terminator::Branch {
                    cond,
                    then_bb,
                    else_bb,
                } => {
                    let taken = matches!(operand(cond, &regs)?, Value::Bool(true));
                    block = if taken { *then_bb } else { *else_bb };
                }
                Terminator::Return(o) => return operand(o, &regs),
                Terminator::Unreachable => {
                    return Err(VmError::Internal("reached unreachable MIR block"));
                }
            }
        }
    }

    /// Executes a single instruction, updating the register file and locals.
    fn exec(
        &mut self,
        inst: &Inst,
        regs: &mut [Value],
        locals: &mut [Value],
    ) -> Result<(), VmError> {
        match inst {
            Inst::Assign { dst, rvalue } => {
                let v = self.eval_rvalue(rvalue, regs, locals)?;
                set_reg(regs, *dst, v)?;
            }
            Inst::Store { local, src } => {
                let v = operand(src, regs)?;
                *locals
                    .get_mut(local.0 as usize)
                    .ok_or(VmError::Internal("bad local"))? = v;
            }
            Inst::SetIndex { base, index, value } => {
                let arr = as_array(&operand(base, regs)?)?;
                let i = as_int(&operand(index, regs)?)?;
                let v = operand(value, regs)?;
                let mut borrow = arr.borrow_mut();
                let len = borrow.len();
                let idx =
                    index_in_bounds(i, len).ok_or(VmError::IndexOutOfBounds { index: i, len })?;
                borrow[idx] = v;
            }
            Inst::Call {
                dst, callee, args, ..
            } => {
                let arg_vals: Vec<Value> = args
                    .iter()
                    .map(|a| operand(a, regs))
                    .collect::<Result<_, _>>()?;
                let result = self.call_callee(*callee, arg_vals)?;
                set_reg(regs, *dst, result)?;
            }
        }
        Ok(())
    }

    fn call_callee(&mut self, callee: Callee, args: Vec<Value>) -> Result<Value, VmError> {
        match callee {
            Callee::Fn(id) => self.call(id.0 as usize, args),
            // `len` is a dedicated opcode in bytecode; here it is a normal call.
            Callee::Builtin(Builtin::Len) => {
                let arr = as_array(args.first().ok_or(VmError::Internal("len needs an arg"))?)?;
                let len = arr.borrow().len() as i64;
                Ok(Value::Int(len))
            }
            Callee::Builtin(b) => builtins::eval(b, &args, &mut self.stdout),
        }
    }

    fn eval_rvalue(
        &mut self,
        rvalue: &Rvalue,
        regs: &[Value],
        locals: &[Value],
    ) -> Result<Value, VmError> {
        Ok(match rvalue {
            Rvalue::Use(o) => operand(o, regs)?,
            Rvalue::Load(l) => locals
                .get(l.0 as usize)
                .cloned()
                .ok_or(VmError::Internal("bad local"))?,
            Rvalue::Unary(op, o) => eval_unary(*op, operand(o, regs)?)?,
            Rvalue::Binary(op, a, b) => eval_binary(*op, operand(a, regs)?, operand(b, regs)?)?,
            Rvalue::Concat(a, b) => {
                let x = as_str(&operand(a, regs)?)?;
                let y = as_str(&operand(b, regs)?)?;
                Value::Str(format!("{x}{y}").into())
            }
            Rvalue::MakeArray(elems) => {
                let items: Vec<Value> = elems
                    .iter()
                    .map(|o| operand(o, regs))
                    .collect::<Result<_, _>>()?;
                Value::Array(Rc::new(RefCell::new(items)))
            }
            Rvalue::Index(base, idx) => {
                let arr = as_array(&operand(base, regs)?)?;
                let i = as_int(&operand(idx, regs)?)?;
                let borrow = arr.borrow();
                let n = borrow.len();
                let at =
                    index_in_bounds(i, n).ok_or(VmError::IndexOutOfBounds { index: i, len: n })?;
                borrow[at].clone()
            }
        })
    }
}

// ---- operand and value helpers ----

fn operand(o: &Operand, regs: &[Value]) -> Result<Value, VmError> {
    Ok(match o {
        Operand::Const(c) => const_value(c),
        Operand::Reg(r) => regs
            .get(r.0 as usize)
            .cloned()
            .ok_or(VmError::Internal("bad reg"))?,
    })
}

fn set_reg(regs: &mut [Value], r: Reg, v: Value) -> Result<(), VmError> {
    *regs
        .get_mut(r.0 as usize)
        .ok_or(VmError::Internal("bad reg"))? = v;
    Ok(())
}

fn const_value(c: &Const) -> Value {
    match c {
        Const::Int(v) => Value::Int(*v),
        Const::Float(v) => Value::Float(*v),
        Const::Bool(v) => Value::Bool(*v),
        Const::Str(v) => Value::Str(v.clone()),
        Const::Unit => Value::Unit,
    }
}

fn eval_unary(op: UnOp, v: Value) -> Result<Value, VmError> {
    Ok(match (op, v) {
        (UnOp::Neg, Value::Int(n)) => Value::Int(n.wrapping_neg()),
        (UnOp::Neg, Value::Float(n)) => Value::Float(-n),
        (UnOp::Not, Value::Bool(b)) => Value::Bool(!b),
        _ => return Err(VmError::Internal("bad unary operand")),
    })
}

fn eval_binary(op: BinOp, a: Value, b: Value) -> Result<Value, VmError> {
    use Value::{Bool, Float, Int};
    Ok(match (a, b) {
        (Int(x), Int(y)) => match op {
            BinOp::Add => Int(x.wrapping_add(y)),
            BinOp::Sub => Int(x.wrapping_sub(y)),
            BinOp::Mul => Int(x.wrapping_mul(y)),
            BinOp::Div => Int(checked(x, y, |a, b| a.checked_div(b))?),
            BinOp::Rem => Int(checked(x, y, |a, b| a.checked_rem(b))?),
            BinOp::Eq => Bool(x == y),
            BinOp::Ne => Bool(x != y),
            BinOp::Lt => Bool(x < y),
            BinOp::Le => Bool(x <= y),
            BinOp::Gt => Bool(x > y),
            BinOp::Ge => Bool(x >= y),
            BinOp::And | BinOp::Or => return Err(VmError::Internal("logical op on ints")),
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
            BinOp::And | BinOp::Or => return Err(VmError::Internal("logical op on floats")),
        },
        (a, b) => match op {
            BinOp::Eq => Bool(a.value_eq(&b)),
            BinOp::Ne => Bool(!a.value_eq(&b)),
            _ => return Err(VmError::Internal("bad binary operands")),
        },
    })
}

/// Applies a checked integer operation, mapping the failure cases to the same
/// runtime errors the stack VM produces.
fn checked(a: i64, b: i64, f: impl Fn(i64, i64) -> Option<i64>) -> Result<i64, VmError> {
    if b == 0 {
        return Err(VmError::DivisionByZero);
    }
    f(a, b).ok_or(VmError::IntegerOverflow)
}

fn as_int(v: &Value) -> Result<i64, VmError> {
    match v {
        Value::Int(n) => Ok(*n),
        _ => Err(VmError::Internal("expected int")),
    }
}

fn as_str(v: &Value) -> Result<Rc<str>, VmError> {
    match v {
        Value::Str(s) => Ok(s.clone()),
        _ => Err(VmError::Internal("expected str")),
    }
}

fn as_array(v: &Value) -> Result<crate::backend::bytecode::Array, VmError> {
    match v {
        Value::Array(a) => Ok(a.clone()),
        _ => Err(VmError::Internal("expected array")),
    }
}

#[cfg(test)]
mod tests;

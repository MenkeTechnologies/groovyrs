//! Lower the Groovy AST to a `fusevm::Chunk`.
//!
//! There is no bespoke VM or JVM here: statements and expressions emit fusevm
//! ops (`LoadInt`, `Add`, `GetVar`, `JumpIfFalse`, …) into a `ChunkBuilder`, and
//! fusevm runs the chunk on its three-tier Cranelift JIT. Groovy values ride the
//! fusevm value model; the strict numeric hook in `crate::host` supplies string
//! `+` concatenation, and Groovy's `BigDecimal`-promoting `/` lowers to the
//! `GDIV` builtin (integer `7/2` is `3.5`, not `3`).
//!
//! Locals are addressed by name through `GetVar`/`SetVar` (slice 1 has a single
//! script frame with no lexical scopes), so this stays a direct, readable
//! lowering. `break`/`continue` are backpatched through a loop-context stack.

use crate::ast::*;
use fusevm::{Chunk, ChunkBuilder, Op, Value};

/// One enclosing loop's backpatch targets.
struct Loop {
    /// `continue` jump op indices, patched to the loop's step/re-test label
    /// once it is known (the label is emitted *after* the loop body, so these
    /// cannot be resolved at the time the `continue` is compiled).
    continue_ops: Vec<usize>,
    /// `break` jump op indices, patched to the loop exit once known.
    break_ops: Vec<usize>,
}

struct Compiler {
    b: ChunkBuilder,
    loops: Vec<Loop>,
    /// A top-level `break`/`return` (no enclosing loop) jumps to script end.
    exit_ops: Vec<usize>,
    /// The source line of the statement currently being lowered — attached to
    /// every emitted op so `--disasm` and `--dap` carry real line numbers.
    cur_line: u32,
    /// When true, emit a `DBG_LINE` marker before each statement (for `--dap`).
    /// Off for ordinary runs, which carry zero extra ops.
    debug: bool,
}

/// Compile a parsed [`Program`]'s body to a runnable fusevm chunk.
pub fn compile(prog: &Program) -> Result<Chunk, String> {
    compile_with(prog, false)
}

/// Compile with per-statement `DBG_LINE` markers for the debug adapter
/// (`groovy --dap`). Identical bytecode to [`compile`] except for the markers.
pub fn compile_debug(prog: &Program) -> Result<Chunk, String> {
    compile_with(prog, true)
}

fn compile_with(prog: &Program, debug: bool) -> Result<Chunk, String> {
    let mut c = Compiler {
        b: ChunkBuilder::new(),
        loops: Vec::new(),
        exit_ops: Vec::new(),
        cur_line: 0,
        debug,
    };
    for stmt in &prog.body {
        c.stmt(stmt)?;
    }
    // Patch any script-level `break`/`return` to the final position.
    let end = c.b.current_pos();
    let exit_ops = std::mem::take(&mut c.exit_ops);
    for op in exit_ops {
        c.b.patch_jump(op, end);
    }
    Ok(c.b.build())
}

impl Compiler {
    fn stmt(&mut self, s: &Stmt) -> Result<(), String> {
        self.cur_line = s.line;
        // In debug mode, a `DBG_LINE` marker precedes each statement so the
        // debug adapter can stop on this line. `CallBuiltin` pushes the
        // builtin's `Undef` return, discarded by the trailing `Pop`.
        if self.debug {
            self.b
                .emit(Op::CallBuiltin(crate::host::DBG_LINE, 0), s.line);
            self.b.emit(Op::Pop, s.line);
        }
        match &s.kind {
            StmtKind::Local { name, init, .. } => {
                if let Some(e) = init {
                    self.expr(e)?;
                    let idx = self.b.add_name(name);
                    self.b.emit(Op::SetVar(idx), self.cur_line);
                }
                // An uninitialized local stays unbound (Groovy defaults it to
                // `null`; a read before assignment yields `null`).
                Ok(())
            }
            StmtKind::Assign { name, op, value } => {
                let idx = self.b.add_name(name);
                match op {
                    AssignOp::Assign => {
                        self.expr(value)?;
                    }
                    AssignOp::Div => {
                        // `x /= e` → x = x / e, through the Groovy division builtin.
                        self.b.emit(Op::GetVar(idx), self.cur_line);
                        self.expr(value)?;
                        self.b.emit(Op::CallBuiltin(crate::host::GDIV, 2), self.cur_line);
                    }
                    _ => {
                        // `x <op>= e` → x = x <op> e
                        self.b.emit(Op::GetVar(idx), self.cur_line);
                        self.expr(value)?;
                        self.b.emit(compound_op(*op), self.cur_line);
                    }
                }
                self.b.emit(Op::SetVar(idx), self.cur_line);
                Ok(())
            }
            StmtKind::Expr(Expr::Println { newline, arg }) => {
                // The print builtin returns `null`; discard it in statement
                // position.
                self.println(*newline, arg.as_deref())?;
                self.b.emit(Op::Pop, self.cur_line);
                Ok(())
            }
            StmtKind::Expr(Expr::PostIncDec { name, inc }) => {
                self.post_inc_dec(name, *inc);
                Ok(())
            }
            StmtKind::Expr(e) => {
                self.expr(e)?;
                self.b.emit(Op::Pop, self.cur_line);
                Ok(())
            }
            StmtKind::If { cond, then, els } => self.if_stmt(cond, then, els),
            StmtKind::While { cond, body } => self.while_stmt(cond, body),
            StmtKind::For {
                init,
                cond,
                update,
                body,
            } => self.for_stmt(init, cond, update, body),
            StmtKind::Break => {
                let op = self.b.emit(Op::Jump(0), self.cur_line);
                match self.loops.last_mut() {
                    Some(l) => l.break_ops.push(op),
                    None => self.exit_ops.push(op),
                }
                Ok(())
            }
            StmtKind::Continue => {
                let op = self.b.emit(Op::Jump(0), self.cur_line);
                self.loops
                    .last_mut()
                    .ok_or_else(|| "groovyrs: `continue` outside a loop".to_string())?
                    .continue_ops
                    .push(op);
                Ok(())
            }
        }
    }

    fn if_stmt(&mut self, cond: &Expr, then: &[Stmt], els: &[Stmt]) -> Result<(), String> {
        self.expr(cond)?;
        let jf = self.b.emit(Op::JumpIfFalse(0), self.cur_line);
        for s in then {
            self.stmt(s)?;
        }
        if els.is_empty() {
            let end = self.b.current_pos();
            self.b.patch_jump(jf, end);
        } else {
            let jend = self.b.emit(Op::Jump(0), self.cur_line);
            let else_start = self.b.current_pos();
            self.b.patch_jump(jf, else_start);
            for s in els {
                self.stmt(s)?;
            }
            let end = self.b.current_pos();
            self.b.patch_jump(jend, end);
        }
        Ok(())
    }

    fn while_stmt(&mut self, cond: &Expr, body: &[Stmt]) -> Result<(), String> {
        let top = self.b.current_pos();
        self.expr(cond)?;
        let jf = self.b.emit(Op::JumpIfFalse(0), self.cur_line);
        self.loops.push(Loop {
            continue_ops: Vec::new(),
            break_ops: Vec::new(),
        });
        for s in body {
            self.stmt(s)?;
        }
        // `continue` in a `while` re-tests the condition: patch to the top.
        let l = self.loops.pop().unwrap();
        for op in &l.continue_ops {
            self.b.patch_jump(*op, top);
        }
        self.b.emit(Op::Jump(top), self.cur_line);
        let end = self.b.current_pos();
        self.b.patch_jump(jf, end);
        for op in l.break_ops {
            self.b.patch_jump(op, end);
        }
        Ok(())
    }

    fn for_stmt(
        &mut self,
        init: &Option<Box<Stmt>>,
        cond: &Option<Expr>,
        update: &Option<Box<Stmt>>,
        body: &[Stmt],
    ) -> Result<(), String> {
        if let Some(init) = init {
            self.stmt(init)?;
        }
        let top = self.b.current_pos();
        let jf = match cond {
            Some(c) => {
                self.expr(c)?;
                Some(self.b.emit(Op::JumpIfFalse(0), self.cur_line))
            }
            None => None,
        };
        // `continue` runs the update clause, then re-tests — target it at the
        // step label emitted after the body.
        self.loops.push(Loop {
            continue_ops: Vec::new(),
            break_ops: Vec::new(),
        });
        for s in body {
            self.stmt(s)?;
        }
        // step label: patch this loop's `continue` jumps to here, so they run
        // the update clause and re-test rather than skipping it.
        let step = self.b.current_pos();
        let l = self.loops.pop().unwrap();
        for op in &l.continue_ops {
            self.b.patch_jump(*op, step);
        }
        if let Some(update) = update {
            self.stmt(update)?;
        }
        self.b.emit(Op::Jump(top), self.cur_line);
        let end = self.b.current_pos();
        if let Some(jf) = jf {
            self.b.patch_jump(jf, end);
        }
        for op in l.break_ops {
            self.b.patch_jump(op, end);
        }
        Ok(())
    }

    fn post_inc_dec(&mut self, name: &str, inc: bool) {
        let idx = self.b.add_name(name);
        self.b.emit(Op::GetVar(idx), self.cur_line);
        self.b.emit(Op::LoadInt(1), self.cur_line);
        self.b.emit(if inc { Op::Add } else { Op::Sub }, self.cur_line);
        self.b.emit(Op::SetVar(idx), self.cur_line);
    }

    /// Lower `println(arg)` / `print(arg)` to the Groovy-formatting print
    /// builtin. Leaves the builtin's `null` return value on the stack.
    fn println(&mut self, newline: bool, arg: Option<&Expr>) -> Result<(), String> {
        let n = match arg {
            Some(e) => {
                self.expr(e)?;
                1
            }
            None => 0,
        };
        let id = if newline {
            crate::host::GPRINTLN
        } else {
            crate::host::GPRINT
        };
        self.b.emit(Op::CallBuiltin(id, n), self.cur_line);
        Ok(())
    }

    fn expr(&mut self, e: &Expr) -> Result<(), String> {
        match e {
            Expr::Int(n) => {
                self.b.emit(Op::LoadInt(*n), self.cur_line);
            }
            Expr::Float(f) => {
                let c = self.b.add_constant(Value::float(*f));
                self.b.emit(Op::LoadConst(c), self.cur_line);
            }
            Expr::Str(s) => {
                let c = self.b.add_constant(Value::str(s.clone()));
                self.b.emit(Op::LoadConst(c), self.cur_line);
            }
            Expr::Bool(b) => {
                self.b
                    .emit(if *b { Op::LoadTrue } else { Op::LoadFalse }, self.cur_line);
            }
            Expr::Null => {
                // Groovy `null` — fusevm has no Null variant, so it rides as Undef.
                self.b.emit(Op::LoadUndef, self.cur_line);
            }
            Expr::Var(name) => {
                let idx = self.b.add_name(name);
                self.b.emit(Op::GetVar(idx), self.cur_line);
            }
            Expr::Unary { op, rhs } => {
                self.expr(rhs)?;
                match op {
                    UnOp::Neg => {
                        self.b.emit(Op::Negate, self.cur_line);
                    }
                    UnOp::Not => {
                        self.b.emit(Op::LogNot, self.cur_line);
                    }
                }
            }
            Expr::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs)?,
            // Println/PostIncDec in value position: the print builtin leaves its
            // `null` return value on the stack.
            Expr::Println { newline, arg } => {
                self.println(*newline, arg.as_deref())?;
            }
            Expr::PostIncDec { name, inc } => {
                let idx = self.b.add_name(name);
                self.b.emit(Op::GetVar(idx), self.cur_line);
                self.post_inc_dec(name, *inc);
            }
        }
        Ok(())
    }

    fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> Result<(), String> {
        // `&&` / `||` short-circuit: keep the deciding operand as the result.
        match op {
            BinOp::And => {
                self.expr(lhs)?;
                let jf = self.b.emit(Op::JumpIfFalseKeep(0), self.cur_line);
                self.b.emit(Op::Pop, self.cur_line);
                self.expr(rhs)?;
                let end = self.b.current_pos();
                self.b.patch_jump(jf, end);
                return Ok(());
            }
            BinOp::Or => {
                self.expr(lhs)?;
                let jt = self.b.emit(Op::JumpIfTrueKeep(0), self.cur_line);
                self.b.emit(Op::Pop, self.cur_line);
                self.expr(rhs)?;
                let end = self.b.current_pos();
                self.b.patch_jump(jt, end);
                return Ok(());
            }
            _ => {}
        }
        self.expr(lhs)?;
        self.expr(rhs)?;
        // Groovy `/` is not a native op — it lowers to the GDIV builtin so
        // integer division promotes to a decimal (`7/2 → 3.5`).
        if let BinOp::Div = op {
            self.b.emit(Op::CallBuiltin(crate::host::GDIV, 2), self.cur_line);
            return Ok(());
        }
        let vop = match op {
            BinOp::Add => Op::Add,
            BinOp::Sub => Op::Sub,
            BinOp::Mul => Op::Mul,
            BinOp::Mod => Op::Mod,
            BinOp::Eq => Op::NumEq,
            BinOp::Ne => Op::NumNe,
            BinOp::Lt => Op::NumLt,
            BinOp::Gt => Op::NumGt,
            BinOp::Le => Op::NumLe,
            BinOp::Ge => Op::NumGe,
            BinOp::Div => unreachable!("handled above"),
            BinOp::And | BinOp::Or => unreachable!("handled above"),
        };
        self.b.emit(vop, self.cur_line);
        Ok(())
    }
}

fn compound_op(op: AssignOp) -> Op {
    match op {
        AssignOp::Add => Op::Add,
        AssignOp::Sub => Op::Sub,
        AssignOp::Mul => Op::Mul,
        AssignOp::Mod => Op::Mod,
        AssignOp::Div => unreachable!("Div lowers through the GDIV builtin, not compound_op"),
        AssignOp::Assign => unreachable!("plain assign never lowers through compound_op"),
    }
}

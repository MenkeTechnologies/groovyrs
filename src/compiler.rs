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
use std::collections::{HashMap, HashSet, VecDeque};

/// The desugar target a `rust { ... }` block lowers to (see [`crate::rust_ffi`]).
const RUST_COMPILE: &str = "__rust_compile";

/// Lexical state while lowering a user-function body: the parameter/local names
/// bound to frame slots. Inside a function every declared name (parameters and
/// `def`/typed locals) addresses a frame-local slot via `GetSlot`/`SetSlot`, so
/// recursion is sound (each call frame has its own slots). A name not bound here
/// falls back to a global (`GetVar`/`SetVar`) — the script binding, matching
/// Groovy's method-vs-binding scoping.
struct FnScope {
    vars: HashMap<String, u16>,
    next_slot: u16,
}

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
    /// True when the program contains a `rust { ... }` FFI block (a
    /// `__rust_compile` call). Only then does an unresolved call name lower to a
    /// runtime FFI dispatch instead of a compile error, so non-FFI programs keep
    /// their exact unresolved-reference compile-time diagnostic.
    has_ffi: bool,
    /// Names of the program's user-defined functions, collected up front so a
    /// call can resolve to a forward-declared function (Groovy lets a script call
    /// a function defined later in the file).
    fn_names: HashSet<String>,
    /// The active function scope while lowering a function body; `None` at script
    /// top level (where names are globals).
    scope: Option<FnScope>,
    /// Closure bodies discovered while lowering, awaiting emission as subroutine
    /// regions after the main body and the user functions (see
    /// [`Compiler::emit_closure`]). A queue because emitting one closure may
    /// enqueue further nested closures.
    pending_closures: VecDeque<PendingClosure>,
    /// Monotonic id for synthetic closure names (`$closure_0`, `$closure_1`, …).
    closures_seen: u32,
}

/// A closure body queued for emission as a subroutine region. `params` already
/// has the implicit `it` injected when the literal had no explicit parameters.
struct PendingClosure {
    name_idx: u16,
    params: Vec<String>,
    body: Vec<Stmt>,
    line: u32,
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
    let has_ffi = body_has_ffi(&prog.body);
    // Collect user-function names up front so calls can resolve forward references.
    let mut fn_names = HashSet::new();
    for stmt in &prog.body {
        if let StmtKind::Function { name, .. } = &stmt.kind {
            fn_names.insert(name.clone());
        }
    }
    let mut c = Compiler {
        b: ChunkBuilder::new(),
        loops: Vec::new(),
        exit_ops: Vec::new(),
        cur_line: 0,
        debug,
        has_ffi,
        fn_names,
        scope: None,
        pending_closures: VecDeque::new(),
        closures_seen: 0,
    };
    // Emit the script body (function definitions are hoisted out and emitted as
    // subroutine regions below).
    for stmt in &prog.body {
        if matches!(stmt.kind, StmtKind::Function { .. }) {
            continue;
        }
        c.stmt(stmt)?;
    }
    // Jump past the function bodies so top-level fall-through halts instead of
    // running into a function body (which is only reachable via `Op::Call`).
    let skip = c.b.emit(Op::Jump(0), 0);
    for stmt in &prog.body {
        if let StmtKind::Function { name, params, body } = &stmt.kind {
            c.function(stmt.line, name, params, body)?;
        }
    }
    // Emit queued closure bodies as subroutine regions. Draining may enqueue
    // further closures (a closure nested inside another closure), so loop until
    // the queue is empty.
    while let Some(pc) = c.pending_closures.pop_front() {
        c.emit_closure(pc)?;
    }
    let end = c.b.current_pos();
    c.b.patch_jump(skip, end);
    // Patch any script-level `break`/`return` to the final position.
    let exit_ops = std::mem::take(&mut c.exit_ops);
    for op in exit_ops {
        c.b.patch_jump(op, end);
    }
    Ok(c.b.build())
}

impl Compiler {
    /// The op that reads `name`: a frame slot inside a function body, else a
    /// global (the script binding).
    fn load_op_for(&mut self, name: &str) -> Op {
        match self.scope.as_ref().and_then(|s| s.vars.get(name).copied()) {
            Some(slot) => Op::GetSlot(slot),
            None => Op::GetVar(self.b.add_name(name)),
        }
    }

    /// The op that writes `name` for an *assignment*: a known local's slot, else
    /// a global. Unlike a declaration this never introduces a new slot — an
    /// assignment to an undeclared name is a script binding (global).
    fn store_op_for(&mut self, name: &str) -> Op {
        match self.scope.as_ref().and_then(|s| s.vars.get(name).copied()) {
            Some(slot) => Op::SetSlot(slot),
            None => Op::SetVar(self.b.add_name(name)),
        }
    }

    /// Register a fresh frame slot for a local `name` inside a function (reusing
    /// an existing mapping if the name was already declared). Returns the slot, or
    /// `None` at script top level (where a declaration is a global).
    fn declare_slot(&mut self, name: &str) -> Option<u16> {
        let scope = self.scope.as_mut()?;
        if let Some(&s) = scope.vars.get(name) {
            return Some(s);
        }
        let s = scope.next_slot;
        scope.next_slot += 1;
        scope.vars.insert(name.to_string(), s);
        Some(s)
    }

    /// The op that writes `name` for a *declaration* (`def`/typed local): a newly
    /// allocated frame slot inside a function, else a global.
    fn store_op_for_decl(&mut self, name: &str) -> Op {
        match self.declare_slot(name) {
            Some(slot) => Op::SetSlot(slot),
            None => Op::SetVar(self.b.add_name(name)),
        }
    }

    /// Lower a user function into a subroutine region: register its entry, bind
    /// its parameters from the value stack into frame slots, lower the body with
    /// an implicit last-expression return, and end with a `null` fall-through
    /// return. See the `Op::Call` frame ABI in fusevm.
    fn function(
        &mut self,
        line: u32,
        name: &str,
        params: &[String],
        body: &[Stmt],
    ) -> Result<(), String> {
        let entry = self.b.current_pos();
        let nidx = self.b.add_name(name);
        self.b.add_sub_entry(nidx, entry);

        let mut vars = HashMap::new();
        for (i, p) in params.iter().enumerate() {
            vars.insert(p.clone(), i as u16);
        }
        let prev = self.scope.replace(FnScope {
            vars,
            next_slot: params.len() as u16,
        });
        self.cur_line = line;

        // Prologue: the caller pushed args left-to-right (param0 deepest,
        // paramN-1 on top). Pop them top-down into their slots.
        for i in (0..params.len()).rev() {
            self.b.emit(Op::SetSlot(i as u16), line);
        }

        self.fn_body(body)?;

        // Fall-through: a function that does not hit an explicit `return` (or
        // whose last statement is not a value expression) returns `null`.
        self.b.emit(Op::LoadUndef, self.cur_line);
        self.b.emit(Op::ReturnValue, self.cur_line);

        self.scope = prev;
        Ok(())
    }

    /// Emit a queued closure body as a subroutine region, using the same frame
    /// ABI as [`Compiler::function`]: register the entry, bind parameters from
    /// the value stack into frame slots, lower the body with an implicit
    /// last-expression return, and end with a `null` fall-through return. A
    /// closure's non-parameter names resolve to globals (the enclosing script
    /// bindings), so it captures the script scope it was defined in.
    fn emit_closure(&mut self, pc: PendingClosure) -> Result<(), String> {
        let entry = self.b.current_pos();
        self.b.add_sub_entry(pc.name_idx, entry);

        let mut vars = HashMap::new();
        for (i, p) in pc.params.iter().enumerate() {
            vars.insert(p.clone(), i as u16);
        }
        let prev = self.scope.replace(FnScope {
            vars,
            next_slot: pc.params.len() as u16,
        });
        let saved_line = self.cur_line;
        self.cur_line = pc.line;

        // Prologue: pop the pushed args top-down into their parameter slots.
        for i in (0..pc.params.len()).rev() {
            self.b.emit(Op::SetSlot(i as u16), pc.line);
        }

        self.fn_body(&pc.body)?;

        // Fall-through: a closure with no trailing value expression returns null.
        self.b.emit(Op::LoadUndef, self.cur_line);
        self.b.emit(Op::ReturnValue, self.cur_line);

        self.scope = prev;
        self.cur_line = saved_line;
        Ok(())
    }

    /// Lower a function body. Groovy returns the value of the last evaluated
    /// statement; when that statement is a bare value expression it becomes the
    /// return value (`Op::ReturnValue`). Other trailing statements fall through to
    /// the `null` return emitted by [`Compiler::function`]; use an explicit
    /// `return` to carry a value out of a control-flow-terminated body.
    fn fn_body(&mut self, body: &[Stmt]) -> Result<(), String> {
        let Some((last, init)) = body.split_last() else {
            return Ok(());
        };
        for s in init {
            self.stmt(s)?;
        }
        match &last.kind {
            // A value expression as the final statement is the implicit return.
            // `println`/`print` are void, so they fall through to the null return.
            StmtKind::Expr(Expr::Println { .. }) => self.stmt(last)?,
            StmtKind::Expr(e) => {
                self.cur_line = last.line;
                self.expr(e)?;
                self.b.emit(Op::ReturnValue, last.line);
            }
            _ => self.stmt(last)?,
        }
        Ok(())
    }

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
                    let store = self.store_op_for_decl(name);
                    self.b.emit(store, self.cur_line);
                } else {
                    // An uninitialized local stays unbound (Groovy defaults it to
                    // `null`; a read before assignment yields `null`). Inside a
                    // function still register the slot so later reads/writes of the
                    // name resolve to the local, not a same-named global.
                    self.declare_slot(name);
                }
                Ok(())
            }
            StmtKind::Assign { name, op, value } => {
                match op {
                    AssignOp::Assign => {
                        self.expr(value)?;
                    }
                    AssignOp::Div => {
                        // `x /= e` → x = x / e, through the Groovy division builtin.
                        let get = self.load_op_for(name);
                        self.b.emit(get, self.cur_line);
                        self.expr(value)?;
                        self.b
                            .emit(Op::CallBuiltin(crate::host::GDIV, 2), self.cur_line);
                    }
                    _ => {
                        // `x <op>= e` → x = x <op> e
                        let get = self.load_op_for(name);
                        self.b.emit(get, self.cur_line);
                        self.expr(value)?;
                        self.b.emit(compound_op(*op), self.cur_line);
                    }
                }
                let store = self.store_op_for(name);
                self.b.emit(store, self.cur_line);
                Ok(())
            }
            StmtKind::Expr(Expr::Println { newline, arg }) => {
                // The print builtin returns `null`; discard it in statement
                // position.
                self.println(*newline, arg.as_deref())?;
                self.b.emit(Op::Pop, self.cur_line);
                Ok(())
            }
            StmtKind::Expr(Expr::PostIncDec { name, inc })
            | StmtKind::Expr(Expr::PreIncDec { name, inc }) => {
                // In statement position pre and post are identical: the result is
                // discarded, so only the in-place update matters.
                self.inc_dec_update(name, *inc);
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
            StmtKind::Return { value } => {
                if self.scope.is_some() {
                    // In a function: carry the value out (a bare `return` → null).
                    match value {
                        Some(e) => self.expr(e)?,
                        None => {
                            self.b.emit(Op::LoadUndef, self.cur_line);
                        }
                    }
                    self.b.emit(Op::ReturnValue, self.cur_line);
                } else {
                    // Top-level: the value (if any) becomes the script result;
                    // end the script by jumping past the remaining statements.
                    if let Some(e) = value {
                        self.expr(e)?;
                    }
                    let op = self.b.emit(Op::Jump(0), self.cur_line);
                    self.exit_ops.push(op);
                }
                Ok(())
            }
            // Function definitions are hoisted and emitted as subroutine regions
            // by `compile_with`; they produce no code in statement position.
            StmtKind::Function { .. } => Ok(()),
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

    /// Emit the in-place update `name = name ± 1` (leaving nothing on the stack).
    /// Used by both `++`/`--` in statement position and as the update step of the
    /// value-position pre/post forms.
    fn inc_dec_update(&mut self, name: &str, inc: bool) {
        let get = self.load_op_for(name);
        self.b.emit(get, self.cur_line);
        self.b.emit(Op::LoadInt(1), self.cur_line);
        self.b
            .emit(if inc { Op::Add } else { Op::Sub }, self.cur_line);
        let store = self.store_op_for(name);
        self.b.emit(store, self.cur_line);
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
                let get = self.load_op_for(name);
                self.b.emit(get, self.cur_line);
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
                // Post: yield the value before the update, then update.
                let get = self.load_op_for(name);
                self.b.emit(get, self.cur_line);
                self.inc_dec_update(name, *inc);
            }
            Expr::PreIncDec { name, inc } => {
                // Pre: update, then yield the new value.
                self.inc_dec_update(name, *inc);
                let get = self.load_op_for(name);
                self.b.emit(get, self.cur_line);
            }
            Expr::Call { name, args, line } => self.call(name, args, *line)?,
            Expr::List(elems) => {
                for e in elems {
                    self.expr(e)?;
                }
                self.b
                    .emit(Op::MakeArray(elems.len() as u16), self.cur_line);
            }
            Expr::Map(entries) => {
                // MakeHash pops interleaved key/value pairs (key pushed first).
                for (k, v) in entries {
                    self.expr(k)?;
                    self.expr(v)?;
                }
                self.b
                    .emit(Op::MakeHash((entries.len() * 2) as u16), self.cur_line);
            }
            Expr::MethodCall {
                recv,
                method,
                args,
                line,
                safe,
            } => {
                // Stack: [recv, arg0..argN-1, methodname]; the GDK dispatch
                // builtin pops the name, the N args, then the receiver. The
                // safe-navigation form routes through GMETHOD_SAFE, which returns
                // `null` without dispatching when the receiver is `null`.
                self.expr(recv)?;
                for a in args {
                    self.expr(a)?;
                }
                let midx = self.b.add_constant(Value::str(method.clone()));
                self.b.emit(Op::LoadConst(midx), *line);
                let id = if *safe {
                    crate::host::GMETHOD_SAFE
                } else {
                    crate::host::GMETHOD
                };
                self.b.emit(Op::CallBuiltin(id, args.len() as u8), *line);
            }
            Expr::Property {
                recv,
                name,
                line,
                safe,
            } => {
                // Stack: [recv, propname]; the property builtin pops both.
                self.expr(recv)?;
                let nidx = self.b.add_constant(Value::str(name.clone()));
                self.b.emit(Op::LoadConst(nidx), *line);
                let id = if *safe {
                    crate::host::GPROP_SAFE
                } else {
                    crate::host::GPROP
                };
                self.b.emit(Op::CallBuiltin(id, 0), *line);
            }
            Expr::Closure { params, body } => self.closure(params, body)?,
            Expr::Range {
                start,
                end,
                inclusive,
            } => {
                // Materialise to a Groovy list of the enumerated integers. The
                // native `Op::Range` is inclusive (`from..=to`); a half-open
                // `a..<b` lowers to the inclusive range `a..(b-1)`.
                self.expr(start)?;
                self.expr(end)?;
                if !*inclusive {
                    self.b.emit(Op::LoadInt(1), self.cur_line);
                    self.b.emit(Op::Sub, self.cur_line);
                }
                self.b.emit(Op::Range, self.cur_line);
            }
            Expr::Ternary { cond, then, els } => {
                // `cond ? then : els` — branch on Groovy truthiness.
                self.expr(cond)?;
                let jf = self.b.emit(Op::JumpIfFalse(0), self.cur_line);
                self.expr(then)?;
                let jend = self.b.emit(Op::Jump(0), self.cur_line);
                let else_start = self.b.current_pos();
                self.b.patch_jump(jf, else_start);
                self.expr(els)?;
                let end = self.b.current_pos();
                self.b.patch_jump(jend, end);
            }
            Expr::Elvis { lhs, rhs } => {
                // `lhs ?: rhs` — keep `lhs` when Groovy-truthy, else evaluate
                // `rhs`. `JumpIfTrueKeep` leaves the deciding `lhs` on the stack.
                self.expr(lhs)?;
                let jt = self.b.emit(Op::JumpIfTrueKeep(0), self.cur_line);
                self.b.emit(Op::Pop, self.cur_line);
                self.expr(rhs)?;
                let end = self.b.current_pos();
                self.b.patch_jump(jt, end);
            }
        }
        Ok(())
    }

    /// Lower a closure literal: queue its body for emission as a subroutine
    /// region and, at the literal site, build the runtime closure handle. The
    /// handle carries the synthetic name-pool index (which resolves to the body
    /// entry via `Chunk::find_sub` at call time) and the parameter count. An
    /// implicit-`it` closure (no explicit parameters) has one parameter, `it`.
    fn closure(&mut self, params: &[String], body: &[Stmt]) -> Result<(), String> {
        let effective: Vec<String> = if params.is_empty() {
            vec!["it".to_string()]
        } else {
            params.to_vec()
        };
        let id = self.closures_seen;
        self.closures_seen += 1;
        let name_idx = self.b.add_name(&format!("$closure_{id}"));
        self.pending_closures.push_back(PendingClosure {
            name_idx,
            params: effective.clone(),
            body: body.to_vec(),
            line: self.cur_line,
        });
        // Build the closure value: push its name index and parameter count, then
        // register through the make-closure builtin (returns a `Value::Obj`).
        self.b.emit(Op::LoadInt(name_idx as i64), self.cur_line);
        self.b
            .emit(Op::LoadInt(effective.len() as i64), self.cur_line);
        self.b.emit(
            Op::CallBuiltin(crate::host::GMAKE_CLOSURE, 2),
            self.cur_line,
        );
        Ok(())
    }

    /// Lower a call expression. Slice 1 has no user methods, so only the inline-
    /// Rust FFI calls resolve: `__rust_compile("<b64>", line)` compiles + registers
    /// the block, and an unknown callee dispatches by name through the FFI runtime
    /// when the program contains a `rust { ... }` block. Every lowering leaves
    /// exactly one value on the stack (the `CallBuiltin` result the VM pushes).
    fn call(&mut self, name: &str, args: &[Expr], line: u32) -> Result<(), String> {
        // `__rust_compile("<base64>", line)` — the desugar target of a
        // `rust { ... }` block. Compile the base64 body string and hand it to the
        // FFI-compile builtin; the second (line) argument is metadata only.
        if name == RUST_COMPILE {
            match args.first() {
                Some(body) => {
                    self.expr(body)?;
                    self.b
                        .emit(Op::CallBuiltin(crate::host::GFFI_COMPILE, 1), line);
                }
                None => {
                    self.b.emit(Op::LoadUndef, line);
                }
            }
            return Ok(());
        }
        // A user-defined function: push the args (left-to-right) and call through
        // the fusevm frame ABI; `Op::Call` leaves the return value on the stack.
        if self.fn_names.contains(name) {
            for a in args {
                self.expr(a)?;
            }
            let nidx = self.b.add_name(name);
            self.b.emit(Op::Call(nidx, args.len() as u8), line);
            return Ok(());
        }
        // Unknown callee. With a `rust { ... }` block present it may be an FFI
        // export registered at runtime, so lower to a by-name FFI dispatch: push
        // the args (deepest first), then the name, then call.
        if self.has_ffi {
            for a in args {
                self.expr(a)?;
            }
            let nidx = self.b.add_constant(Value::str(name.to_string()));
            self.b.emit(Op::LoadConst(nidx), line);
            self.b.emit(
                Op::CallBuiltin(crate::host::GFFI_CALL, args.len() as u8),
                line,
            );
            return Ok(());
        }
        // Otherwise `name(args)` is a call through a variable — a closure invoked
        // directly, `def f = { it * 2 }; f(21)`. Load the value, push the args,
        // and dispatch through the closure-call builtin, which faults with
        // `unresolved reference: name` if the value is not a closure.
        let get = self.load_op_for(name);
        self.b.emit(get, line);
        for a in args {
            self.expr(a)?;
        }
        let nidx = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(nidx), line);
        self.b.emit(
            Op::CallBuiltin(crate::host::GCLOSURE_CALL, args.len() as u8),
            line,
        );
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
            self.b
                .emit(Op::CallBuiltin(crate::host::GDIV, 2), self.cur_line);
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

// ── FFI detection (does the program contain a `rust { ... }` block?) ────────

/// True if any statement in `body` (recursively) evaluates a `__rust_compile`
/// call — the desugar target of a `rust { ... }` block.
fn body_has_ffi(body: &[Stmt]) -> bool {
    body.iter().any(|s| match &s.kind {
        StmtKind::Local { init, .. } => init.as_ref().is_some_and(expr_has_ffi),
        StmtKind::Assign { value, .. } => expr_has_ffi(value),
        StmtKind::Expr(e) => expr_has_ffi(e),
        StmtKind::If { cond, then, els } => {
            expr_has_ffi(cond) || body_has_ffi(then) || body_has_ffi(els)
        }
        StmtKind::While { cond, body } => expr_has_ffi(cond) || body_has_ffi(body),
        StmtKind::For {
            init,
            cond,
            update,
            body,
        } => {
            init.as_deref()
                .is_some_and(|s| body_has_ffi(std::slice::from_ref(s)))
                || cond.as_ref().is_some_and(expr_has_ffi)
                || update
                    .as_deref()
                    .is_some_and(|s| body_has_ffi(std::slice::from_ref(s)))
                || body_has_ffi(body)
        }
        StmtKind::Return { value } => value.as_ref().is_some_and(expr_has_ffi),
        StmtKind::Function { body, .. } => body_has_ffi(body),
        StmtKind::Break | StmtKind::Continue => false,
    })
}

fn expr_has_ffi(e: &Expr) -> bool {
    match e {
        Expr::Call { name, args, .. } => name == RUST_COMPILE || args.iter().any(expr_has_ffi),
        Expr::Unary { rhs, .. } => expr_has_ffi(rhs),
        Expr::Binary { lhs, rhs, .. } => expr_has_ffi(lhs) || expr_has_ffi(rhs),
        Expr::Println { arg, .. } => arg.as_deref().is_some_and(expr_has_ffi),
        Expr::List(elems) => elems.iter().any(expr_has_ffi),
        Expr::Map(entries) => entries
            .iter()
            .any(|(k, v)| expr_has_ffi(k) || expr_has_ffi(v)),
        Expr::MethodCall { recv, args, .. } => expr_has_ffi(recv) || args.iter().any(expr_has_ffi),
        Expr::Property { recv, .. } => expr_has_ffi(recv),
        Expr::Closure { body, .. } => body_has_ffi(body),
        Expr::Range { start, end, .. } => expr_has_ffi(start) || expr_has_ffi(end),
        Expr::Ternary { cond, then, els } => {
            expr_has_ffi(cond) || expr_has_ffi(then) || expr_has_ffi(els)
        }
        Expr::Elvis { lhs, rhs } => expr_has_ffi(lhs) || expr_has_ffi(rhs),
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Null
        | Expr::Var(_)
        | Expr::PostIncDec { .. }
        | Expr::PreIncDec { .. } => false,
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

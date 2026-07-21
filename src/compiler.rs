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
    /// The field names of the class whose method/constructor body is currently
    /// being lowered; `None` outside a class member. A bare name that is a field
    /// (and not shadowed by a parameter/local) resolves to `this.field`.
    cur_class_fields: Option<HashSet<String>>,
    /// The method names of the class whose member body is currently being
    /// lowered. A bare call to one of these (not shadowed by a local) is an
    /// implicit `this.method(args)`.
    cur_class_methods: Option<HashSet<String>>,
    /// The name of the class whose member body is currently being lowered; `None`
    /// outside a class member. Used to resolve `super.m()` / `super(...)` to the
    /// class's declared superclass at compile time.
    cur_class_super: Option<String>,
    /// Every declared class's superclass and own field/method names, so a class
    /// body can resolve *inherited* bare names (`name`, `speak()`) to `this` by
    /// walking the superclass chain at compile time. Keyed by class name.
    class_index: HashMap<String, ClassInfo>,
    /// Closure bodies discovered while lowering, awaiting emission as subroutine
    /// regions after the main body and the user functions (see
    /// [`Compiler::emit_closure`]). A queue because emitting one closure may
    /// enqueue further nested closures.
    pending_closures: VecDeque<PendingClosure>,
    /// Monotonic id for synthetic closure names (`$closure_0`, `$closure_1`, …).
    closures_seen: u32,
}

/// A declared class's inheritance-relevant shape: its direct superclass and the
/// names of its own fields and methods. Used to compute the transitive (inherited)
/// field/method sets that drive bare-name resolution inside a class body.
struct ClassInfo {
    superclass: Option<String>,
    fields: Vec<String>,
    methods: Vec<String>,
}

/// A closure body queued for emission as a subroutine region. `params` already
/// has the implicit `it` injected when the literal had no explicit parameters.
/// `captures` are the enclosing-frame locals the closure reads as upvalues; they
/// occupy the frame slots immediately after the parameters (see
/// [`Compiler::emit_closure`]) and are supplied at call time from the closure
/// handle. `class_fields` carries a class context down into a closure defined
/// inside a method so a bare field name still resolves to `this.field`.
struct PendingClosure {
    name_idx: u16,
    params: Vec<String>,
    captures: Vec<String>,
    body: Vec<Stmt>,
    line: u32,
    class_fields: Option<HashSet<String>>,
    class_methods: Option<HashSet<String>>,
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
    // Index every class's inheritance shape up front so a subclass body can
    // resolve inherited bare names to `this` (a subclass may appear before its
    // superclass in source, like function forward references).
    let mut class_index = HashMap::new();
    for stmt in &prog.body {
        if let StmtKind::Class {
            name,
            superclass,
            fields,
            methods,
            ..
        } = &stmt.kind
        {
            class_index.insert(
                name.clone(),
                ClassInfo {
                    superclass: superclass.clone(),
                    fields: fields.iter().map(|f| f.name.clone()).collect(),
                    methods: methods.iter().map(|m| m.name.clone()).collect(),
                },
            );
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
        cur_class_fields: None,
        cur_class_methods: None,
        cur_class_super: None,
        class_index,
        pending_closures: VecDeque::new(),
        closures_seen: 0,
    };
    // Register every class before running the body so `new C()` and method
    // dispatch resolve regardless of source order (like function forward refs).
    for stmt in &prog.body {
        if let StmtKind::Class {
            name,
            superclass,
            fields,
            ctors,
            methods,
        } = &stmt.kind
        {
            c.register_class(name, superclass.as_deref(), fields, ctors, methods);
        }
    }
    // Emit the script body (function and class definitions are hoisted out and
    // emitted as subroutine regions below).
    for stmt in &prog.body {
        if matches!(
            stmt.kind,
            StmtKind::Function { .. } | StmtKind::Class { .. }
        ) {
            continue;
        }
        c.stmt(stmt)?;
    }
    // Jump past the function/method bodies so top-level fall-through halts
    // instead of running into a body (only reachable via `Op::Call`/dispatch).
    let skip = c.b.emit(Op::Jump(0), 0);
    for stmt in &prog.body {
        if let StmtKind::Function { name, params, body } = &stmt.kind {
            c.function(stmt.line, name, params, body)?;
        }
    }
    // Emit each class's field-initializer, constructor, and method subroutines.
    for stmt in &prog.body {
        if let StmtKind::Class {
            name,
            superclass,
            fields,
            ctors,
            methods,
        } = &stmt.kind
        {
            c.class_bodies(
                stmt.line,
                name,
                superclass.as_deref(),
                fields,
                ctors,
                methods,
            )?;
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

        // Slots: parameters first (0..n), then captured upvalues (n..n+k). At
        // call time `invoke_closure` pushes the params then the captures in this
        // order, so the prologue pops all of them into their slots.
        let mut vars = HashMap::new();
        for (i, p) in pc.params.iter().enumerate() {
            vars.insert(p.clone(), i as u16);
        }
        for (j, cap) in pc.captures.iter().enumerate() {
            vars.insert(cap.clone(), (pc.params.len() + j) as u16);
        }
        let total = pc.params.len() + pc.captures.len();
        let prev = self.scope.replace(FnScope {
            vars,
            next_slot: total as u16,
        });
        let prev_fields = std::mem::replace(&mut self.cur_class_fields, pc.class_fields);
        let prev_methods = std::mem::replace(&mut self.cur_class_methods, pc.class_methods);
        let saved_line = self.cur_line;
        self.cur_line = pc.line;

        // Prologue: pop the pushed params + captures top-down into their slots.
        for i in (0..total).rev() {
            self.b.emit(Op::SetSlot(i as u16), pc.line);
        }

        self.fn_body(&pc.body)?;

        // Fall-through: a closure with no trailing value expression returns null.
        self.b.emit(Op::LoadUndef, self.cur_line);
        self.b.emit(Op::ReturnValue, self.cur_line);

        self.scope = prev;
        self.cur_class_fields = prev_fields;
        self.cur_class_methods = prev_methods;
        self.cur_line = saved_line;
        Ok(())
    }

    // ── Classes ─────────────────────────────────────────────────────────────

    /// Synthetic sub name for a class method body.
    fn method_sub_name(class: &str, method: &str) -> String {
        format!("$cls_{class}_m_{method}")
    }
    /// Synthetic sub name for a class constructor of the given arity.
    fn ctor_sub_name(class: &str, arity: usize) -> String {
        format!("$cls_{class}_ctor_{arity}")
    }
    /// Synthetic sub name for a class field's initializer thunk.
    fn init_sub_name(class: &str, field: &str) -> String {
        format!("$cls_{class}_init_{field}")
    }

    /// Emit the runtime registration of a class: push its name, field-name list,
    /// method table, field-initializer table, and constructor table, then call
    /// the class-register builtin. Runs once at script start (hoisted), so the
    /// class is resolvable before any `new`.
    fn register_class(
        &mut self,
        name: &str,
        superclass: Option<&str>,
        fields: &[Field],
        ctors: &[Ctor],
        methods: &[Method],
    ) {
        let line = 0;
        // superclass name (empty string ⇒ no superclass), pushed first so the
        // register builtin pops it last.
        let sidx = self
            .b
            .add_constant(Value::str(superclass.unwrap_or("").to_string()));
        self.b.emit(Op::LoadConst(sidx), line);
        // class name
        let nidx = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(nidx), line);
        // field-name array (declaration order)
        for f in fields {
            let c = self.b.add_constant(Value::str(f.name.clone()));
            self.b.emit(Op::LoadConst(c), line);
        }
        self.b.emit(Op::MakeArray(fields.len() as u16), line);
        // method table: name -> sub name-pool index
        for m in methods {
            let k = self.b.add_constant(Value::str(m.name.clone()));
            self.b.emit(Op::LoadConst(k), line);
            let sub = self.b.add_name(&Self::method_sub_name(name, &m.name));
            self.b.emit(Op::LoadInt(sub as i64), line);
        }
        self.b.emit(Op::MakeHash((methods.len() * 2) as u16), line);
        // field-initializer table: field -> init-thunk sub name-pool index
        let mut init_count = 0;
        for f in fields {
            if f.init.is_some() {
                let k = self.b.add_constant(Value::str(f.name.clone()));
                self.b.emit(Op::LoadConst(k), line);
                let sub = self.b.add_name(&Self::init_sub_name(name, &f.name));
                self.b.emit(Op::LoadInt(sub as i64), line);
                init_count += 1;
            }
        }
        self.b.emit(Op::MakeHash((init_count * 2) as u16), line);
        // constructor table: arity -> ctor sub name-pool index
        for ctor in ctors {
            let k = self
                .b
                .add_constant(Value::str(ctor.params.len().to_string()));
            self.b.emit(Op::LoadConst(k), line);
            let sub = self
                .b
                .add_name(&Self::ctor_sub_name(name, ctor.params.len()));
            self.b.emit(Op::LoadInt(sub as i64), line);
        }
        self.b.emit(Op::MakeHash((ctors.len() * 2) as u16), line);
        self.b.emit(Op::CallBuiltin(crate::host::GCLASS, 0), line);
        self.b.emit(Op::Pop, line);
    }

    /// Emit every subroutine body a class needs: field-initializer thunks,
    /// constructors, and methods. Constructors and methods carry an implicit
    /// `this` in slot 0 and resolve bare field names to `this.field`.
    fn class_bodies(
        &mut self,
        line: u32,
        name: &str,
        superclass: Option<&str>,
        fields: &[Field],
        ctors: &[Ctor],
        methods: &[Method],
    ) -> Result<(), String> {
        // Include inherited fields/methods so a bare inherited name inside this
        // class's bodies resolves to `this.field` / `this.method(...)`.
        let field_set = self.inherited_names(name, |i| &i.fields);
        let method_set = self.inherited_names(name, |i| &i.methods);
        // Publish the superclass so `super.m()` / `super(...)` in this class's
        // bodies resolve to it; restored after emitting the members.
        let prev_super =
            std::mem::replace(&mut self.cur_class_super, superclass.map(str::to_string));
        // Field-initializer thunks (0-arg subs that compute the initial value).
        for f in fields {
            if let Some(init) = &f.init {
                self.emit_field_init(line, name, &f.name, init)?;
            }
        }
        for ctor in ctors {
            let sub = Self::ctor_sub_name(name, ctor.params.len());
            self.emit_member(
                line,
                &sub,
                &ctor.params,
                &ctor.body,
                &field_set,
                &method_set,
            )?;
        }
        for m in methods {
            let sub = Self::method_sub_name(name, &m.name);
            self.emit_member(line, &sub, &m.params, &m.body, &field_set, &method_set)?;
        }
        self.cur_class_super = prev_super;
        Ok(())
    }

    /// The transitive set of field (or method) names for `class`, unioned across
    /// its superclass chain via [`Compiler::class_index`]. `select` picks the
    /// field or method list from each ancestor's [`ClassInfo`].
    fn inherited_names(
        &self,
        class: &str,
        select: impl Fn(&ClassInfo) -> &Vec<String>,
    ) -> HashSet<String> {
        let mut set = HashSet::new();
        let mut cur = Some(class.to_string());
        while let Some(name) = cur {
            let Some(info) = self.class_index.get(&name) else {
                break;
            };
            for n in select(info) {
                set.insert(n.clone());
            }
            cur = info.superclass.clone();
        }
        set
    }

    /// Emit a class member (method or constructor) as a subroutine: `this` in
    /// slot 0, parameters in slots 1..n+1, bare field names resolving to
    /// `this.field`. Uses the implicit last-expression return like a function.
    fn emit_member(
        &mut self,
        line: u32,
        sub_name: &str,
        params: &[String],
        body: &[Stmt],
        field_set: &HashSet<String>,
        method_set: &HashSet<String>,
    ) -> Result<(), String> {
        let entry = self.b.current_pos();
        let nidx = self.b.add_name(sub_name);
        self.b.add_sub_entry(nidx, entry);

        // `this` occupies slot 0 (a named slot so a nested closure can capture
        // it); parameters follow in slots 1..n+1.
        let mut vars = HashMap::new();
        vars.insert("this".to_string(), 0);
        for (i, p) in params.iter().enumerate() {
            vars.insert(p.clone(), (i + 1) as u16);
        }
        let prev = self.scope.replace(FnScope {
            vars,
            next_slot: (params.len() + 1) as u16,
        });
        let prev_fields = self.cur_class_fields.replace(field_set.clone());
        let prev_methods = self.cur_class_methods.replace(method_set.clone());
        let saved_line = self.cur_line;
        self.cur_line = line;

        // Prologue: pop `this` + args top-down into slots 0..n+1.
        for i in (0..params.len() + 1).rev() {
            self.b.emit(Op::SetSlot(i as u16), line);
        }
        self.fn_body(body)?;
        self.b.emit(Op::LoadUndef, self.cur_line);
        self.b.emit(Op::ReturnValue, self.cur_line);

        self.scope = prev;
        self.cur_class_fields = prev_fields;
        self.cur_class_methods = prev_methods;
        self.cur_line = saved_line;
        Ok(())
    }

    /// Emit a field-initializer thunk: a 0-arg subroutine that evaluates the
    /// initializer and returns it. No `this` is bound (initializers see script
    /// globals, not other fields).
    fn emit_field_init(
        &mut self,
        line: u32,
        class: &str,
        field: &str,
        init: &Expr,
    ) -> Result<(), String> {
        let entry = self.b.current_pos();
        let nidx = self.b.add_name(&Self::init_sub_name(class, field));
        self.b.add_sub_entry(nidx, entry);
        let prev = self.scope.replace(FnScope {
            vars: HashMap::new(),
            next_slot: 0,
        });
        let prev_fields = self.cur_class_fields.take();
        let prev_methods = self.cur_class_methods.take();
        let saved_line = self.cur_line;
        self.cur_line = line;
        self.expr(init)?;
        self.b.emit(Op::ReturnValue, self.cur_line);
        self.scope = prev;
        self.cur_class_fields = prev_fields;
        self.cur_class_methods = prev_methods;
        self.cur_line = saved_line;
        Ok(())
    }

    /// True when `name` is a field of the class currently being lowered and is
    /// not shadowed by a parameter/local — so it resolves to `this.field`.
    fn is_field(&self, name: &str) -> bool {
        self.cur_class_fields
            .as_ref()
            .is_some_and(|f| f.contains(name))
            && !self.is_local(name)
    }

    /// True when `name` is bound to a slot in the current function/method scope
    /// (a parameter or local), so it must not be reinterpreted as a field/method.
    fn is_local(&self, name: &str) -> bool {
        self.scope
            .as_ref()
            .is_some_and(|s| s.vars.contains_key(name))
    }

    /// True when `name` is a method of the class currently being lowered and is
    /// not shadowed by a local — so a bare call resolves to `this.method(args)`.
    fn is_method(&self, name: &str) -> bool {
        self.cur_class_methods
            .as_ref()
            .is_some_and(|m| m.contains(name))
            && !self.is_local(name)
    }

    /// True when `name` is a field of the class currently being lowered
    /// (membership only, ignoring local shadowing) — used to decide whether a
    /// nested closure must capture `this`.
    fn field_of_class(&self, name: &str) -> bool {
        self.cur_class_fields
            .as_ref()
            .is_some_and(|f| f.contains(name))
    }

    /// True when `name` is a method of the class currently being lowered.
    fn method_of_class(&self, name: &str) -> bool {
        self.cur_class_methods
            .as_ref()
            .is_some_and(|m| m.contains(name))
    }

    /// Push the current instance (`this`). In a method it is frame slot 0; in a
    /// closure nested inside a method it is a captured upvalue slot — both are
    /// reached by resolving the name `this` through the scope.
    fn emit_this(&mut self) {
        if self.is_local("this") {
            let get = self.load_op_for("this");
            self.b.emit(get, self.cur_line);
        } else {
            self.b.emit(Op::GetSlot(0), self.cur_line);
        }
    }

    /// Emit a read of the current instance's field `name` (`this.field`):
    /// `this` through the property builtin.
    fn emit_field_get(&mut self, name: &str) {
        self.emit_this();
        let c = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(c), self.cur_line);
        self.b
            .emit(Op::CallBuiltin(crate::host::GPROP, 0), self.cur_line);
    }

    /// Lower an assignment to a bare field inside a method: `field <op>= value`
    /// becomes `this.field = this.field <op> value` through the property-set
    /// builtin. Stack for the builtin is `this` (deepest), the new value, then
    /// the field name.
    fn assign_field(&mut self, name: &str, op: AssignOp, value: &Expr) -> Result<(), String> {
        self.emit_this(); // receiver for the set
        match op {
            AssignOp::Assign => {
                self.expr(value)?;
            }
            AssignOp::Div => {
                self.emit_field_get(name);
                self.expr(value)?;
                self.b
                    .emit(Op::CallBuiltin(crate::host::GDIV, 2), self.cur_line);
            }
            _ => {
                self.emit_field_get(name);
                self.expr(value)?;
                self.b.emit(compound_op(op), self.cur_line);
            }
        }
        let c = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(c), self.cur_line);
        self.b
            .emit(Op::CallBuiltin(crate::host::GSETPROP, 0), self.cur_line);
        self.b.emit(Op::Pop, self.cur_line);
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
                // A bare field name inside a method/constructor is `this.field`.
                if self.is_field(name) {
                    return self.assign_field(name, *op, value);
                }
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
            StmtKind::SetProperty { recv, name, value } => {
                // `recv.name = value` — stack: recv (deepest), value, name.
                self.expr(recv)?;
                self.expr(value)?;
                let c = self.b.add_constant(Value::str(name.clone()));
                self.b.emit(Op::LoadConst(c), self.cur_line);
                self.b
                    .emit(Op::CallBuiltin(crate::host::GSETPROP, 0), self.cur_line);
                self.b.emit(Op::Pop, self.cur_line);
                Ok(())
            }
            // Classes are hoisted and emitted as subroutine regions by
            // `compile_with`; they produce no code in statement position.
            StmtKind::Class { .. } => Ok(()),
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
                // A bare field name inside a method/constructor is `this.field`.
                if self.is_field(name) {
                    self.emit_field_get(name);
                } else {
                    let get = self.load_op_for(name);
                    self.b.emit(get, self.cur_line);
                }
            }
            Expr::This => {
                self.emit_this();
            }
            // A bare `super` outside a `super.method(...)` call resolves to the
            // current instance (Groovy has no standalone `super` value).
            Expr::Super => {
                self.emit_this();
            }
            Expr::SuperCtor { args, line } => {
                // `super(args)`: run the superclass's arity-matched constructor on
                // the current instance (stack: [this, args, superclassname]).
                self.emit_this();
                for a in args {
                    self.expr(a)?;
                }
                let sname = self.cur_class_super.clone().unwrap_or_default();
                let sidx = self.b.add_constant(Value::str(sname));
                self.b.emit(Op::LoadConst(sidx), *line);
                self.b.emit(
                    Op::CallBuiltin(crate::host::GSUPER_CTOR, args.len() as u8),
                    *line,
                );
            }
            Expr::InstanceOf { value, class } => {
                // `value instanceof Class` — stack: [value, classname].
                self.expr(value)?;
                let cidx = self.b.add_constant(Value::str(class.clone()));
                self.b.emit(Op::LoadConst(cidx), self.cur_line);
                self.b
                    .emit(Op::CallBuiltin(crate::host::GINSTANCEOF, 0), self.cur_line);
            }
            Expr::New { class, args, line } => {
                // Push the constructor args, then the class name on top.
                for a in args {
                    self.expr(a)?;
                }
                let c = self.b.add_constant(Value::str(class.clone()));
                self.b.emit(Op::LoadConst(c), *line);
                self.b
                    .emit(Op::CallBuiltin(crate::host::GNEW, args.len() as u8), *line);
            }
            Expr::Index { recv, index, line } => {
                // `recv[index]` — stack: recv (deepest), index.
                self.expr(recv)?;
                self.expr(index)?;
                self.b.emit(Op::CallBuiltin(crate::host::GINDEX, 0), *line);
            }
            Expr::CallValue { callee, args, line } => {
                // Invoke the value of `callee` with `args` — the postfix
                // call-application that makes `f(a)(b)` work. Reuses the
                // closure-call builtin with a synthetic name for diagnostics.
                self.expr(callee)?;
                for a in args {
                    self.expr(a)?;
                }
                let nidx = self.b.add_constant(Value::str("<closure>".to_string()));
                self.b.emit(Op::LoadConst(nidx), *line);
                self.b.emit(
                    Op::CallBuiltin(crate::host::GCLOSURE_CALL, args.len() as u8),
                    *line,
                );
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
                // Groovy maps preserve insertion order, so a map literal builds a
                // host-heap ordered map (not the unordered fusevm `Hash`). Push
                // the interleaved key/value pairs (key first), then the entry
                // count, and register through the make-map builtin.
                for (k, v) in entries {
                    self.expr(k)?;
                    self.expr(v)?;
                }
                self.b
                    .emit(Op::LoadInt(entries.len() as i64), self.cur_line);
                self.b
                    .emit(Op::CallBuiltin(crate::host::GMAKE_MAP, 0), self.cur_line);
            }
            Expr::MethodCall {
                recv,
                method,
                args,
                line,
                safe,
            } => {
                // `super.method(args)` statically dispatches at the superclass,
                // skipping the current class's override (stack: [this, args,
                // methodname, superclassname]).
                if matches!(**recv, Expr::Super) {
                    self.emit_this();
                    for a in args {
                        self.expr(a)?;
                    }
                    let midx = self.b.add_constant(Value::str(method.clone()));
                    self.b.emit(Op::LoadConst(midx), *line);
                    let sname = self.cur_class_super.clone().unwrap_or_default();
                    let sidx = self.b.add_constant(Value::str(sname));
                    self.b.emit(Op::LoadConst(sidx), *line);
                    self.b.emit(
                        Op::CallBuiltin(crate::host::GSUPER_METHOD, args.len() as u8),
                        *line,
                    );
                } else {
                    // Stack: [recv, arg0..argN-1, methodname]; the GDK dispatch
                    // builtin pops the name, the N args, then the receiver. The
                    // safe-navigation form routes through GMETHOD_SAFE, which
                    // returns `null` without dispatching when the receiver is null.
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
        // Upvalues: the closure's free names that resolve to a slot in the
        // enclosing function/closure frame. At script top level there is no such
        // frame (`scope` is `None`), so a closure captures nothing and its free
        // names stay script-binding globals, exactly as before.
        let mut captures: Vec<String> = match self.scope.as_ref() {
            Some(scope) => free_vars(&effective, body)
                .into_iter()
                .filter(|n| scope.vars.contains_key(n))
                .collect(),
            None => Vec::new(),
        };
        // A closure that reads a field or calls a sibling method needs the
        // enclosing `this` even though the bare name is the field/method, not
        // `this`. Capture `this` (the enclosing slot 0) so `this.field` inside
        // the closure resolves to the instance, not the closure's own slot 0.
        if self.is_local("this") && !captures.iter().any(|c| c == "this") {
            let uses_this = free_vars(&effective, body)
                .iter()
                .any(|n| self.field_of_class(n) || self.method_of_class(n));
            if uses_this {
                captures.push("this".to_string());
            }
        }
        // Push each captured value (read from the enclosing frame) so the
        // make-closure builtin can store it in the handle.
        for cap in &captures {
            let get = self.load_op_for(cap);
            self.b.emit(get, self.cur_line);
        }
        let id = self.closures_seen;
        self.closures_seen += 1;
        let name_idx = self.b.add_name(&format!("$closure_{id}"));
        self.pending_closures.push_back(PendingClosure {
            name_idx,
            params: effective.clone(),
            captures: captures.clone(),
            body: body.to_vec(),
            line: self.cur_line,
            class_fields: self.cur_class_fields.clone(),
            class_methods: self.cur_class_methods.clone(),
        });
        // Build the closure value: push its name index, parameter count, and
        // capture count, then register through the make-closure builtin (returns
        // a `Value::Obj`).
        self.b.emit(Op::LoadInt(name_idx as i64), self.cur_line);
        self.b
            .emit(Op::LoadInt(effective.len() as i64), self.cur_line);
        self.b
            .emit(Op::LoadInt(captures.len() as i64), self.cur_line);
        self.b.emit(
            Op::CallBuiltin(crate::host::GMAKE_CLOSURE, 0),
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
        // A bare call to a sibling method inside a class body is an implicit
        // `this.method(args)` (a local variable of the same name would shadow it).
        if self.is_method(name) && !self.is_local(name) {
            self.emit_this(); // this (receiver)
            for a in args {
                self.expr(a)?;
            }
            let midx = self.b.add_constant(Value::str(name.to_string()));
            self.b.emit(Op::LoadConst(midx), line);
            self.b.emit(
                Op::CallBuiltin(crate::host::GMETHOD, args.len() as u8),
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
        // Groovy `<=>` is not a native op — it lowers to the GCMP builtin, which
        // dispatches a user `compareTo` on an instance operand or yields the
        // primitive sign (`-1`/`0`/`1`).
        if let BinOp::Cmp = op {
            self.b
                .emit(Op::CallBuiltin(crate::host::GCMP, 2), self.cur_line);
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
            BinOp::Div | BinOp::Cmp => unreachable!("handled above"),
            BinOp::And | BinOp::Or => unreachable!("handled above"),
        };
        self.b.emit(vop, self.cur_line);
        Ok(())
    }
}

// ── Free-variable analysis for closure upvalue capture ──────────────────────

/// The free variables of a closure body: names referenced but not bound by the
/// closure's own parameters or locals. The caller intersects this with the
/// enclosing frame's slots to decide which values to capture as upvalues. The
/// walk descends into nested closures (extending the bound set with their
/// parameters/locals) so an inner closure's free name propagates outward and is
/// captured at each intervening level. First-seen order, deduplicated.
fn free_vars(params: &[String], body: &[Stmt]) -> Vec<String> {
    let mut bound: HashSet<String> = params.iter().cloned().collect();
    collect_bound_stmts(body, &mut bound);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for s in body {
        free_in_stmt(s, &bound, &mut out, &mut seen);
    }
    out
}

/// Add every name declared as a local at this closure level (including inside
/// control-flow blocks, but *not* inside nested closures) to `bound`.
fn collect_bound_stmts(body: &[Stmt], bound: &mut HashSet<String>) {
    for s in body {
        match &s.kind {
            StmtKind::Local { name, .. } => {
                bound.insert(name.clone());
            }
            StmtKind::If { then, els, .. } => {
                collect_bound_stmts(then, bound);
                collect_bound_stmts(els, bound);
            }
            StmtKind::While { body, .. } => collect_bound_stmts(body, bound),
            StmtKind::For {
                init, update, body, ..
            } => {
                if let Some(i) = init {
                    collect_bound_stmts(std::slice::from_ref(i), bound);
                }
                if let Some(u) = update {
                    collect_bound_stmts(std::slice::from_ref(u), bound);
                }
                collect_bound_stmts(body, bound);
            }
            _ => {}
        }
    }
}

/// Record `name` as free if it is not bound in the current scope (deduped).
fn note_free(
    name: &str,
    bound: &HashSet<String>,
    out: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    if !bound.contains(name) && seen.insert(name.to_string()) {
        out.push(name.to_string());
    }
}

fn free_in_stmt(
    s: &Stmt,
    bound: &HashSet<String>,
    out: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    match &s.kind {
        StmtKind::Local { init, .. } => {
            if let Some(e) = init {
                free_in_expr(e, bound, out, seen);
            }
        }
        StmtKind::Assign { name, value, .. } => {
            note_free(name, bound, out, seen);
            free_in_expr(value, bound, out, seen);
        }
        StmtKind::Expr(e) => free_in_expr(e, bound, out, seen),
        StmtKind::If { cond, then, els } => {
            free_in_expr(cond, bound, out, seen);
            for s in then {
                free_in_stmt(s, bound, out, seen);
            }
            for s in els {
                free_in_stmt(s, bound, out, seen);
            }
        }
        StmtKind::While { cond, body } => {
            free_in_expr(cond, bound, out, seen);
            for s in body {
                free_in_stmt(s, bound, out, seen);
            }
        }
        StmtKind::For {
            init,
            cond,
            update,
            body,
        } => {
            if let Some(i) = init {
                free_in_stmt(i, bound, out, seen);
            }
            if let Some(c) = cond {
                free_in_expr(c, bound, out, seen);
            }
            if let Some(u) = update {
                free_in_stmt(u, bound, out, seen);
            }
            for s in body {
                free_in_stmt(s, bound, out, seen);
            }
        }
        StmtKind::Return { value } => {
            if let Some(e) = value {
                free_in_expr(e, bound, out, seen);
            }
        }
        StmtKind::SetProperty { recv, value, .. } => {
            free_in_expr(recv, bound, out, seen);
            free_in_expr(value, bound, out, seen);
        }
        StmtKind::Break
        | StmtKind::Continue
        | StmtKind::Function { .. }
        | StmtKind::Class { .. } => {}
    }
}

fn free_in_expr(
    e: &Expr,
    bound: &HashSet<String>,
    out: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    match e {
        Expr::Var(n) => note_free(n, bound, out, seen),
        Expr::PostIncDec { name, .. } | Expr::PreIncDec { name, .. } => {
            note_free(name, bound, out, seen)
        }
        Expr::Unary { rhs, .. } => free_in_expr(rhs, bound, out, seen),
        Expr::Binary { lhs, rhs, .. } => {
            free_in_expr(lhs, bound, out, seen);
            free_in_expr(rhs, bound, out, seen);
        }
        Expr::Println { arg, .. } => {
            if let Some(a) = arg {
                free_in_expr(a, bound, out, seen);
            }
        }
        Expr::Call { name, args, .. } => {
            // The callee may be an enclosing-scope closure variable, so it is a
            // free reference too (a global function name simply won't intersect
            // the enclosing frame's slots and so is never captured).
            note_free(name, bound, out, seen);
            for a in args {
                free_in_expr(a, bound, out, seen);
            }
        }
        Expr::CallValue { callee, args, .. } => {
            free_in_expr(callee, bound, out, seen);
            for a in args {
                free_in_expr(a, bound, out, seen);
            }
        }
        Expr::List(elems) => {
            for e in elems {
                free_in_expr(e, bound, out, seen);
            }
        }
        Expr::Map(entries) => {
            for (k, v) in entries {
                free_in_expr(k, bound, out, seen);
                free_in_expr(v, bound, out, seen);
            }
        }
        Expr::MethodCall { recv, args, .. } => {
            free_in_expr(recv, bound, out, seen);
            for a in args {
                free_in_expr(a, bound, out, seen);
            }
        }
        Expr::Property { recv, .. } => free_in_expr(recv, bound, out, seen),
        Expr::Index { recv, index, .. } => {
            free_in_expr(recv, bound, out, seen);
            free_in_expr(index, bound, out, seen);
        }
        Expr::New { args, .. } => {
            for a in args {
                free_in_expr(a, bound, out, seen);
            }
        }
        Expr::Closure { params, body } => {
            // Descend with the nested closure's own bindings added, so a name
            // free in the inner closure but not bound here still surfaces.
            let mut inner = bound.clone();
            if params.is_empty() {
                inner.insert("it".to_string());
            }
            for p in params {
                inner.insert(p.clone());
            }
            collect_bound_stmts(body, &mut inner);
            for s in body {
                free_in_stmt(s, &inner, out, seen);
            }
        }
        Expr::Range { start, end, .. } => {
            free_in_expr(start, bound, out, seen);
            free_in_expr(end, bound, out, seen);
        }
        Expr::Ternary { cond, then, els } => {
            free_in_expr(cond, bound, out, seen);
            free_in_expr(then, bound, out, seen);
            free_in_expr(els, bound, out, seen);
        }
        Expr::Elvis { lhs, rhs } => {
            free_in_expr(lhs, bound, out, seen);
            free_in_expr(rhs, bound, out, seen);
        }
        // `this`/`super` are captured upvalues when a closure inside a method
        // uses them (both resolve to the receiver instance in slot 0).
        Expr::This | Expr::Super => note_free("this", bound, out, seen),
        Expr::SuperCtor { args, .. } => {
            note_free("this", bound, out, seen);
            for a in args {
                free_in_expr(a, bound, out, seen);
            }
        }
        Expr::InstanceOf { value, .. } => free_in_expr(value, bound, out, seen),
        Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Null => {}
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
        StmtKind::SetProperty { recv, value, .. } => expr_has_ffi(recv) || expr_has_ffi(value),
        StmtKind::Class {
            fields,
            ctors,
            methods,
            ..
        } => {
            fields
                .iter()
                .any(|f| f.init.as_ref().is_some_and(expr_has_ffi))
                || ctors.iter().any(|c| body_has_ffi(&c.body))
                || methods.iter().any(|m| body_has_ffi(&m.body))
        }
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
        Expr::CallValue { callee, args, .. } => {
            expr_has_ffi(callee) || args.iter().any(expr_has_ffi)
        }
        Expr::New { args, .. } => args.iter().any(expr_has_ffi),
        Expr::Index { recv, index, .. } => expr_has_ffi(recv) || expr_has_ffi(index),
        Expr::SuperCtor { args, .. } => args.iter().any(expr_has_ffi),
        Expr::InstanceOf { value, .. } => expr_has_ffi(value),
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Null
        | Expr::This
        | Expr::Super
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

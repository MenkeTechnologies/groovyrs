//! Lower the Groovy AST to a `fusevm::Chunk`.
//!
//! There is no bespoke VM or JVM here: statements and expressions emit fusevm
//! ops (`LoadInt`, `Add`, `GetVar`, `JumpIfFalse`, …) into a `ChunkBuilder`, and
//! fusevm runs the chunk on its three-tier Cranelift JIT. Groovy values ride the
//! fusevm value model; the strict numeric hook in `crate::host` supplies string
//! `+` concatenation, and Groovy's `BigDecimal`-promoting `/` lowers to the
//! `GDIV` builtin (integer `7/2` is `3.5`, not `3`).
//!
//! Locals are addressed by name through `GetVar`/`SetVar` at script level (a
//! single frame with no lexical scopes) and by frame slot inside a function,
//! closure, or method, so this stays a direct, readable lowering.
//! `break`/`continue` are backpatched through a loop-context stack, and an
//! exception unwind through a parallel `try`-context stack.
//!
//! Two lowerings are deliberately *conditional*, so a program that does not use
//! the feature keeps exactly the bytecode it had:
//!
//! * the Groovy-truthiness call is emitted only where a condition's static shape
//!   could be a value fusevm's own truth test reads differently (see
//!   `needs_truth`) — a comparison-shaped loop guard stays on the native op
//!   the JIT traces;
//! * every exception op is gated on the program containing `try`/`throw` (see
//!   `body_uses_exceptions`).

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

/// One enclosing `try` block's pending unwind jumps. A `throw` (or a post-call
/// pending-exception check) inside the block emits a `Jump` and parks its index
/// here; the jumps are backpatched to the handler once its position is known.
/// The scope is popped before the `catch` arms are lowered, so a `throw` from a
/// handler targets the *enclosing* `try` — Groovy's rule.
struct TryScope {
    unwind_ops: Vec<usize>,
}

/// One enclosing `try`'s `finally` body, kept so an early exit out of the block
/// (`return`, `break`, `continue`, or an unwind from a `catch` arm) can emit the
/// cleanup inline before it leaves. `javac` duplicates a `finally` body per exit
/// path for exactly this reason, and so does groovyrs.
struct FinallyFrame {
    body: Vec<Stmt>,
    /// `Compiler::loops.len()` at `try` entry — a `break`/`continue` runs the
    /// frames at or above the depth of the loop it targets.
    loop_depth: usize,
    /// `Compiler::tries.len()` at `try` entry, *after* its own `TryScope` was
    /// pushed. While the `try` body is being lowered this equals the current
    /// depth (so an unwind goes to the handler, which runs the `finally`
    /// itself); once the scope is popped for the `catch` arms it exceeds the
    /// current depth, which is how an unwind out of a handler knows to run the
    /// cleanup on its way out.
    try_depth: usize,
}

/// One enclosing loop's (or `switch`'s) backpatch targets.
struct Loop {
    /// `continue` jump op indices, patched to the loop's step/re-test label
    /// once it is known (the label is emitted *after* the loop body, so these
    /// cannot be resolved at the time the `continue` is compiled).
    continue_ops: Vec<usize>,
    /// `break` jump op indices, patched to the loop exit once known.
    break_ops: Vec<usize>,
    /// The `label:` this frame was introduced with, if any — what a
    /// `break label` / `continue label` names.
    label: Option<String>,
    /// True for a `switch` frame. A `switch` is a `break` target but *not* a
    /// `continue` target: a `continue` inside one continues the enclosing loop.
    is_switch: bool,
}

impl Loop {
    /// A fresh frame carrying `label`.
    fn new(label: Option<String>, is_switch: bool) -> Self {
        Loop {
            continue_ops: Vec::new(),
            break_ops: Vec::new(),
            label,
            is_switch,
        }
    }
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
    /// The subset of [`Compiler::fn_names`] that can reach itself through the
    /// static call graph. Each of these gets a [`crate::host::GDEPTH`] check in
    /// its prologue, so runaway native recursion raises a catchable
    /// `StackOverflowError` instead of growing `vm.frames` until the process is
    /// killed. See [`recursive_fns`].
    recursive_fns: HashSet<String>,
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
    /// Nesting depth of the closure body being lowered; 0 outside one. A bare
    /// name only reaches a `with`/`tap` delegate from inside a closure, so this
    /// is what keeps script and function bodies on their native variable ops.
    closure_depth: u32,
    /// Every name the *script* binds at its own level, collected up front by
    /// [`collect_script_vars`]. A closure's free name that appears here is an
    /// ordinary script binding and keeps its native `GetVar`/`SetVar` — only a
    /// name nothing in the program declares can be a delegate's property.
    script_vars: HashSet<String>,
    /// True when the program contains `try` or `throw`. Only then is a call site
    /// followed by a pending-exception check, so an exception-free program's
    /// bytecode is exactly what it was before exceptions landed.
    has_exceptions: bool,
    /// True when the program uses exceptions *and* declares a class — the only
    /// combination in which a native arithmetic op can re-enter the VM (an
    /// operator-overload method on a user instance, dispatched by the strict
    /// numeric hook) and so can leave an exception in flight. Gating on it keeps
    /// arithmetic native for every other program.
    exc_after_arith: bool,
    /// The enclosing `try` blocks of the frame being lowered.
    tries: Vec<TryScope>,
    /// The enclosing `finally` bodies of the frame being lowered.
    finallys: Vec<FinallyFrame>,
    /// Monotonic id for compiler-minted temporaries (`$exc_…`).
    temps_seen: u32,
    /// The `label:` a `StmtKind::Labeled` just introduced, consumed by the loop
    /// or `switch` it wraps when that pushes its [`Loop`] frame.
    pending_label: Option<String>,
    /// Variables whose value is a `Long` rather than an `Integer` — declared
    /// `long`/`Long`/`BigInteger`, or `def`-declared from a `Long` initializer.
    /// See [`Compiler::is_wide`] for why the compiler tracks this at all.
    ///
    /// The set is *flow-sensitive*: a `def` re-declaration or a plain assignment
    /// re-binds the name to its new initializer's width, narrow included, so
    /// `def a = 5L; …; a = 5; a * 1000000000` wraps at 32 bits the way Groovy's
    /// runtime does. It is also *scoped*: [`Compiler::function`] and
    /// [`Compiler::emit_closure`] save and restore it around a body, so a
    /// `def a = 5L` inside one closure cannot make a sibling closure's own
    /// `def a = 2000000000` wide. Both were previously one flat set that only
    /// ever grew, which made the width of a name depend on every *other* place
    /// in the file that happened to spell it.
    wide_vars: HashSet<String>,
    /// Callables whose result is statically a `Long`: a user function or a
    /// closure-bound variable every one of whose returned expressions is wide.
    /// Without this, `def f = { -> 5L }; def a = f(); a * 1000000000` would wrap
    /// at 32 bits — the value is inside `Integer` range, so the host's magnitude
    /// rule cannot see the `Long` either. Tracked and scoped alongside
    /// [`Compiler::wide_vars`].
    wide_returns: HashSet<String>,
    /// Names whose `Long` width is fixed by an explicit `long`/`Long`/
    /// `BigInteger` declaration rather than inferred from an initializer. Java's
    /// static type wins over the assigned value — `long t = 0; t = 5;
    /// t * 2000000000` is still 64-bit arithmetic — so these are exempt from the
    /// narrowing above until the name is re-declared.
    pinned_wide: HashSet<String>,
    /// Names the compiler can see are bound to a host-heap object whose `>>` is
    /// *not* a bit shift: a closure (`f >> g` is `Closure.andThen`, forward
    /// composition) or a user-class instance (a `rightShift` overload). Only
    /// these route `>>` through [`crate::host::GSHR`]; everything else keeps the
    /// native lowering, so a shifting loop keeps its JIT trace. Tracked and
    /// scoped alongside [`Compiler::wide_vars`].
    obj_vars: HashSet<String>,
    /// The op indices of arithmetic whose operands are statically `Long`, handed
    /// to the host as [`crate::host::set_wide_sites`]. See
    /// [`Compiler::is_wide`].
    wide_sites: HashSet<usize>,
    /// The names in the *current* scope that live in a boxed binding — a
    /// [`crate::host::GCELL_NEW`] cell — because some closure written in that
    /// scope captures them. Every read of one goes through
    /// [`crate::host::GCELL_GET`] and every write through
    /// [`crate::host::GCELL_SET`], so the closure and the enclosing code share
    /// the variable rather than a snapshot of its value, and a declaration
    /// inside a loop body makes a new variable per iteration.
    ///
    /// Scoped like [`Compiler::wide_vars`]: [`Compiler::function`] and
    /// [`Compiler::emit_closure`] save and restore it around a body. Computed by
    /// [`boxed_names`], so a scope with no closure in it boxes nothing and its
    /// loops keep their plain slot reads.
    cells: HashSet<String>,
    /// Set by [`Compiler::emit_compound_value`] just before it calls
    /// [`Compiler::binary`]: the left operand's value is **already on the stack**
    /// (a compound assignment pushed it), so `binary` must analyse `lhs` without
    /// emitting it.
    ///
    /// A flag rather than a parameter so `binary`'s five separate left-operand
    /// emit sites stay one decision, and consumed with `mem::take` at the top of
    /// `binary` so a nested operand's own `binary` call never inherits it.
    preloaded_lhs: bool,
}

/// The Groovy/Java type names a `case <Name>:` label can check against without
/// the script declaring the class — the same set `host::value_is_a` resolves.
/// Modeled throwables are recognised separately, through `throwable::is_builtin`.
const BUILTIN_TYPE_NAMES: &[&str] = &[
    "Object",
    "GroovyObject",
    "String",
    "CharSequence",
    "GString",
    "Integer",
    "Long",
    "Short",
    "Byte",
    "BigDecimal",
    "Double",
    "Float",
    "BigInteger",
    "Number",
    "Boolean",
    "List",
    "ArrayList",
    "Collection",
    "Iterable",
    "Map",
    "LinkedHashMap",
    "HashMap",
];

/// A declared class's inheritance-relevant shape: its direct superclass and the
/// names of its own fields and methods. Used to compute the transitive (inherited)
/// field/method sets that drive bare-name resolution inside a class body.
struct ClassInfo {
    superclass: Option<String>,
    /// The `implements A, B` names (or an interface's own `extends` list). A
    /// bare call inside a member reaches an interface's `default` methods
    /// through this, exactly as it reaches a superclass's.
    interfaces: Vec<String>,
    fields: Vec<String>,
    /// Method names declared here — including an interface's *abstract*
    /// declarations, which bind no body but still make a bare call inside a
    /// sibling `default` method mean `this.m()`.
    methods: Vec<String>,
}

/// The per-name static shape state — integer width, and whether the name holds
/// a closure/instance — a function or closure body is entered with, restored
/// when it ends so its locals cannot leak either outward. See
/// [`Compiler::wide_vars`] and [`Compiler::obj_vars`].
struct WidthScope {
    vars: HashSet<String>,
    returns: HashSet<String>,
    pinned: HashSet<String>,
    objs: HashSet<String>,
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
    /// The subset of `captures` that arrive as boxed bindings (cells). The body
    /// must read and write those through [`crate::host::GCELL_GET`] /
    /// [`crate::host::GCELL_SET`] — copying the *handle* into a slot is exactly
    /// what makes the closure share the enclosing variable.
    cell_captures: HashSet<String>,
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

/// Collect every name the *script* binds at its own level — a `def`/typed
/// declaration or a bare assignment, including inside the script's own control
/// flow. A function or class body is a different scope, and a closure body is
/// precisely the case the set exists to exclude (a name bound only there may be
/// a `with`/`tap` delegate's property), so neither is descended into.
fn collect_script_vars(body: &[Stmt], out: &mut HashSet<String>) {
    for s in body {
        match &s.kind {
            StmtKind::Local { name, .. } | StmtKind::Assign { name, .. } => {
                out.insert(name.clone());
            }
            StmtKind::If { then, els, .. } => {
                collect_script_vars(then, out);
                collect_script_vars(els, out);
            }
            StmtKind::While { body, .. } | StmtKind::DoWhile { body, .. } => {
                collect_script_vars(body, out)
            }
            StmtKind::For {
                init, update, body, ..
            } => {
                if let Some(i) = init {
                    collect_script_vars(std::slice::from_ref(&**i), out);
                }
                if let Some(u) = update {
                    collect_script_vars(std::slice::from_ref(&**u), out);
                }
                collect_script_vars(body, out);
            }
            StmtKind::Labeled { stmt, .. } => {
                collect_script_vars(std::slice::from_ref(&**stmt), out)
            }
            StmtKind::Switch { cases, .. } => {
                for c in cases {
                    collect_script_vars(&c.body, out);
                }
            }
            StmtKind::Try {
                body,
                catches,
                finally_body,
            } => {
                collect_script_vars(body, out);
                for c in catches {
                    collect_script_vars(&c.body, out);
                }
                collect_script_vars(finally_body, out);
            }
            _ => {}
        }
    }
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
            interfaces,
            fields,
            methods,
            abstract_methods,
            ..
        } = &stmt.kind
        {
            class_index.insert(
                name.clone(),
                ClassInfo {
                    superclass: superclass.clone(),
                    interfaces: interfaces.clone(),
                    fields: fields.iter().map(|f| f.name.clone()).collect(),
                    methods: methods
                        .iter()
                        .map(|m| m.name.clone())
                        .chain(abstract_methods.iter().cloned())
                        .collect(),
                },
            );
        }
    }
    let recursive = recursive_fns(prog, &fn_names);
    let mut c = Compiler {
        b: ChunkBuilder::new(),
        loops: Vec::new(),
        exit_ops: Vec::new(),
        cur_line: 0,
        debug,
        has_ffi,
        fn_names,
        recursive_fns: recursive,
        scope: None,
        cur_class_fields: None,
        cur_class_methods: None,
        cur_class_super: None,
        class_index,
        pending_closures: VecDeque::new(),
        closures_seen: 0,
        closure_depth: 0,
        script_vars: {
            let mut v = HashSet::new();
            // Groovy puts `args` in every script's binding — an empty
            // `String[]` when the launcher was given none — so it is bound
            // whether or not the script assigns it, and reading it never raises.
            v.insert("args".to_string());
            collect_script_vars(&prog.body, &mut v);
            v
        },
        has_exceptions: body_uses_exceptions(&prog.body),
        exc_after_arith: body_uses_exceptions(&prog.body)
            && prog
                .body
                .iter()
                .any(|s| matches!(s.kind, StmtKind::Class { .. })),
        tries: Vec::new(),
        finallys: Vec::new(),
        temps_seen: 0,
        pending_label: None,
        wide_vars: HashSet::new(),
        wide_returns: HashSet::new(),
        pinned_wide: HashSet::new(),
        obj_vars: HashSet::new(),
        wide_sites: HashSet::new(),
        cells: boxed_names(&[], &prog.body),
        preloaded_lhs: false,
    };
    // Arm the host's exception machinery for this run. Emitted only by a program
    // that uses `try`/`throw`, and it is what lets a runtime `Throwable` (a zero
    // divisor) become catchable instead of aborting.
    if c.has_exceptions {
        c.b.emit(Op::CallBuiltin(crate::host::GEXC_ARM, 0), 0);
        c.b.emit(Op::Pop, 0);
    }
    // Register every class before running the body so `new C()` and method
    // dispatch resolve regardless of source order (like function forward refs).
    for stmt in &prog.body {
        if let StmtKind::Class {
            name,
            superclass,
            interfaces,
            is_interface,
            is_trait,
            generated,
            fields,
            ctors,
            methods,
            ..
        } = &stmt.kind
        {
            c.register_class(
                name,
                superclass.as_deref(),
                interfaces,
                *is_interface,
                *is_trait,
                *generated,
                fields,
                ctors,
                methods,
            );
        }
    }
    // A call to a user function is a `Long` when every `return` in its body is,
    // which a forward reference needs known before the call is lowered.
    for stmt in &prog.body {
        if let StmtKind::Function { name, body, .. } = &stmt.kind {
            if c.body_returns_wide(body) {
                c.wide_returns.insert(name.clone());
            }
        }
    }
    // Install a `null` cell for every boxed script binding before the body runs,
    // so a read that reaches one before its declaration finds a cell rather than
    // an unset global. A declaration then replaces it with a fresh cell, which is
    // what makes a top-level `def` inside a loop a new variable per iteration.
    let mut script_cells: Vec<String> = c.cells.iter().cloned().collect();
    script_cells.sort();
    for n in &script_cells {
        c.emit_cell_init(n, 0);
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
            interfaces,
            is_trait,
            fields,
            ctors,
            methods,
            ..
        } = &stmt.kind
        {
            // A trait's `extends` names land in `interfaces` (the declaration is
            // a list, as an interface's is), so its `super.m()` has to resolve
            // through there — a trait has no superclass slot to publish.
            let parent = match (is_trait, superclass.as_deref()) {
                (true, None) => interfaces.first().map(String::as_str),
                (_, s) => s,
            };
            c.class_bodies(stmt.line, name, parent, fields, ctors, methods)?;
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
    // An exception no handler claimed reaches here: report it Groovy-style and
    // exit non-zero. Both the fall-through jump and every script-level exit land
    // on this check, so an unwind out of the top level always finds it.
    if c.has_exceptions {
        c.b.emit(Op::CallBuiltin(crate::host::GEXC_PENDING, 0), 0);
        let jf = c.b.emit(Op::JumpIfFalse(0), 0);
        c.b.emit(Op::CallBuiltin(crate::host::GEXC_ABORT, 0), 0);
        c.b.emit(Op::Pop, 0);
        let after = c.b.current_pos();
        c.b.patch_jump(jf, after);
    }
    crate::host::set_wide_sites(std::mem::take(&mut c.wide_sites));
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

    /// True when a bare `name` has to go through the delegate-aware builtins:
    /// we are lowering a closure body and nothing the compiler can see binds the
    /// name. Groovy resolves such a name against the closure's `with`/`tap`
    /// delegate (its `OWNER_FIRST` strategy, the owner having failed), which is
    /// what those builtins do. Anything the compiler *can* bind — a slot, a
    /// field, a class, a function, a JDK class, a script-level declaration —
    /// keeps its native `GetVar`/`SetVar`, so an ordinary variable costs nothing
    /// and a hot closure loop keeps its JIT trace eligibility.
    fn needs_delegate(&self, name: &str) -> bool {
        self.closure_depth > 0 && self.nothing_binds(name)
    }

    /// True when no construct the compiler can see binds `name` — not a slot,
    /// field, method, function, class, JDK class, or script-level declaration.
    /// `this` and the `$`-prefixed synthetic names are compiler-owned and never
    /// resolve this way.
    fn nothing_binds(&self, name: &str) -> bool {
        if name == "this" || name.starts_with('$') {
            return false;
        }
        if let Some(scope) = self.scope.as_ref() {
            if scope.vars.contains_key(name) {
                return false;
            }
        }
        !self.is_field(name)
            && !self.method_of_class(name)
            && !self.fn_names.contains(name)
            && !self.class_index.contains_key(name)
            && !self.is_static_class_ref(name)
            && !self.script_vars.contains(name)
    }

    /// Emit the read of a bare `name` — the native slot/global read, or the
    /// checked builtin for a name the compiler cannot bind.
    ///
    /// The builtin covers two cases with one op. Inside a closure the name may
    /// be a `with`/`tap` delegate's property, which is what it is asked for.
    /// Anywhere, a name nothing binds at *run* time is Groovy's
    /// `MissingPropertyException`, not `null` — reading an undeclared name is an
    /// error in Groovy, and answering `null` turned every typo into a silent
    /// `null` that surfaced far from its cause. A name the compiler *can* bind
    /// keeps its native `GetVar`/`GetSlot`, so an ordinary variable costs
    /// nothing and a hot loop keeps its JIT trace eligibility.
    fn emit_name_load(&mut self, name: &str, line: u32) -> Result<(), String> {
        if !self.nothing_binds(name) {
            self.emit_var_read(name, line);
            return Ok(());
        }
        self.emit_name_site(name, line);
        self.emit_call_builtin(crate::host::GNAME_GET, 0, line)
    }

    /// Emit the write of a bare `name`, its value already on the stack.
    fn emit_name_store(&mut self, name: &str, line: u32) -> Result<(), String> {
        if self.cells.contains(name) {
            // The value is already on the stack; push the cell over it and write
            // through the handle, so the closures holding it see the new value.
            let get = self.load_op_for(name);
            self.b.emit(get, line);
            self.b
                .emit(Op::CallBuiltin(crate::host::GCELL_SET, 0), line);
            self.b.emit(Op::Pop, line);
            return Ok(());
        }
        if !self.needs_delegate(name) {
            let store = self.store_op_for(name);
            self.b.emit(store, line);
            return Ok(());
        }
        self.emit_name_site(name, line);
        self.emit_call_builtin(crate::host::GNAME_SET, 0, line)?;
        self.b.emit(Op::Pop, line);
        Ok(())
    }

    /// Push the `(global-index, name)` pair both bare-name builtins read: the
    /// index answers the owner-first question, the name is what the delegate is
    /// asked for.
    fn emit_name_site(&mut self, name: &str, line: u32) {
        let g = self.b.add_name(name);
        self.b.emit(Op::LoadInt(g as i64), line);
        let n = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(n), line);
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

    /// Emit a read of `name` the compiler can bind: the slot/global load, plus
    /// the cell deref when the name is boxed.
    fn emit_var_read(&mut self, name: &str, line: u32) {
        let get = self.load_op_for(name);
        self.b.emit(get, line);
        if self.cells.contains(name) {
            self.b
                .emit(Op::CallBuiltin(crate::host::GCELL_GET, 0), line);
        }
    }

    /// Emit the store for a *declaration* of `name`, its value already on the
    /// stack. A boxed name gets a **fresh** cell here — that is what makes a
    /// `def` inside a loop body a new variable on every iteration, so the
    /// closures built in two iterations do not share one binding.
    ///
    /// "Fresh" is about identity, not about allocation: the cell the target
    /// already holds is reused when no closure captured it, which is the common
    /// case for a loop that builds a closure only sometimes (or never calls one
    /// that outlives the iteration). Pushing the target's current handle after
    /// the initializer is what lets [`crate::host::GCELL_RENEW`] decide — and
    /// taking it from the target itself is what keeps a recursive call from
    /// reusing its caller's cell.
    fn emit_decl_store(&mut self, name: &str, line: u32) {
        if self.cells.contains(name) {
            let get = self.load_op_for(name);
            self.b.emit(get, line);
            self.b
                .emit(Op::CallBuiltin(crate::host::GCELL_RENEW, 0), line);
        }
        let store = self.store_op_for_decl(name);
        self.b.emit(store, line);
    }

    /// Install a fresh `null` cell for a boxed `name` in the current scope.
    /// Emitted for an uninitialized declaration (`def v` with no initializer),
    /// and once per scope entry so that a read reaching the name before its
    /// declaration runs finds a cell rather than an unset slot.
    fn emit_cell_init(&mut self, name: &str, line: u32) {
        if !self.cells.contains(name) {
            return;
        }
        self.b.emit(Op::LoadUndef, line);
        self.b
            .emit(Op::CallBuiltin(crate::host::GCELL_NEW, 0), line);
        let store = self.store_op_for_decl(name);
        self.b.emit(store, line);
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
        // A `try` in the caller must never catch a throw from this body: the
        // unwind leaves the frame and the caller's post-call check re-raises it.
        let prev_tries = std::mem::take(&mut self.tries);
        let prev_finallys = std::mem::take(&mut self.finallys);
        // The body's locals are its own: what it declares must not leak a width
        // onto a same-named variable outside it (or in a sibling body). The
        // parameters arrive with no width the compiler can see, so they start
        // narrow whatever the enclosing scope calls that name.
        let prev_widths = self.enter_width_scope(params);
        self.cur_line = line;

        // Prologue: the caller pushed args left-to-right (param0 deepest,
        // paramN-1 on top). Pop them top-down into their slots.
        for i in (0..params.len()).rev() {
            self.b.emit(Op::SetSlot(i as u16), line);
        }

        // A function that can reach itself checks the call depth here. fusevm's
        // `Op::Call` costs no Rust stack, so unbounded native recursion does not
        // overflow the process — it grows `vm.frames` until the process is
        // killed, which no `catch` can see. Emitted only for the functions
        // `recursive_fns` found, so every other prologue is unchanged.
        if self.recursive_fns.contains(name) {
            self.b.emit(Op::CallBuiltin(crate::host::GDEPTH, 0), line);
            self.b.emit(Op::Pop, line);
            self.emit_exc_check(line)?;
        }

        let prev_cells = self.enter_cell_scope(params, body, &HashSet::new(), line);
        self.fn_body(body)?;
        self.exit_cell_scope(prev_cells);

        // Fall-through: a function that does not hit an explicit `return` (or
        // whose last statement is not a value expression) returns `null`.
        self.b.emit(Op::LoadUndef, self.cur_line);
        self.b.emit(Op::ReturnValue, self.cur_line);

        self.scope = prev;
        self.tries = prev_tries;
        self.finallys = prev_finallys;
        self.exit_width_scope(prev_widths);
        Ok(())
    }

    /// Enter a body's boxed-binding scope: compute which of its own names some
    /// closure inside it captures, install their cells, and hand back the
    /// previous set for [`Compiler::exit_cell_scope`].
    ///
    /// `pre_boxed` are names that arrive **already** boxed — a closure's cell
    /// captures, whose slots hold the enclosing scope's handles and must not be
    /// re-boxed. Parameters that are boxed here are re-wrapped from their slot,
    /// so each call gets its own cell; every other boxed local starts as a `null`
    /// cell so a read that runs before the declaration finds one.
    fn enter_cell_scope(
        &mut self,
        params: &[String],
        body: &[Stmt],
        pre_boxed: &HashSet<String>,
        line: u32,
    ) -> HashSet<String> {
        let saved = std::mem::take(&mut self.cells);
        let mut own = boxed_names(params, body);
        // A capture already lives in a cell; boxing it again would give the
        // closure a private copy, which is the bug this whole mechanism fixes.
        for n in pre_boxed {
            own.remove(n);
        }
        self.cells = own;
        self.cells.extend(pre_boxed.iter().cloned());
        // A boxed parameter is re-wrapped from its slot, so each call of the body
        // gets its own cell: a closure built on the second call cannot see the
        // first call's argument.
        for p in params {
            if !self.cells.contains(p) || pre_boxed.contains(p) {
                continue;
            }
            self.emit_var_read_raw(p, line);
            self.b
                .emit(Op::CallBuiltin(crate::host::GCELL_NEW, 0), line);
            let store = self.store_op_for_decl(p);
            self.b.emit(store, line);
        }
        // Sorted: this walk allocates the frame slots, and a `HashSet`'s order
        // would make the same source compile to different slot numbers run to
        // run.
        let mut locals: Vec<String> = self
            .cells
            .iter()
            .filter(|n| !params.contains(n) && !pre_boxed.contains(*n))
            .cloned()
            .collect();
        locals.sort();
        for n in locals {
            self.emit_cell_init(&n, line);
        }
        saved
    }

    /// Restore the boxed-binding set a body was entered from.
    fn exit_cell_scope(&mut self, saved: HashSet<String>) {
        self.cells = saved;
    }

    /// Push `name`'s slot/global value without the cell deref — the raw handle.
    fn emit_var_read_raw(&mut self, name: &str, line: u32) {
        let get = self.load_op_for(name);
        self.b.emit(get, line);
    }

    /// Save the width state around a function/closure body and start the body
    /// with its parameters narrow. Returns what [`Compiler::exit_width_scope`]
    /// restores.
    fn enter_width_scope(&mut self, params: &[String]) -> WidthScope {
        let saved = WidthScope {
            vars: self.wide_vars.clone(),
            returns: self.wide_returns.clone(),
            pinned: self.pinned_wide.clone(),
            objs: self.obj_vars.clone(),
        };
        for p in params {
            self.wide_vars.remove(p);
            self.wide_returns.remove(p);
            self.pinned_wide.remove(p);
            self.obj_vars.remove(p);
        }
        saved
    }

    /// Restore the width state a body was entered with.
    fn exit_width_scope(&mut self, saved: WidthScope) {
        self.wide_vars = saved.vars;
        self.wide_returns = saved.returns;
        self.pinned_wide = saved.pinned;
        self.obj_vars = saved.objs;
    }

    /// Does every value this body hands back have a statically-`Long` width?
    /// A body with no returned expression at all answers `false` (its result is
    /// `null`). Used to give `def a = f()` the width `f`'s body produces.
    fn body_returns_wide(&self, body: &[Stmt]) -> bool {
        let mut saw_one = false;
        let mut all_wide = true;
        for s in body {
            self.scan_returns(s, &mut saw_one, &mut all_wide);
        }
        // The trailing expression is Groovy's implicit return.
        if let Some(Stmt {
            kind: StmtKind::Expr(e),
            ..
        }) = body.last()
        {
            saw_one = true;
            all_wide &= self.is_wide(e);
        }
        saw_one && all_wide
    }

    /// Fold every `return <expr>` reachable in `s` into the "all returns are
    /// wide" answer [`Compiler::body_returns_wide`] builds. A nested function or
    /// closure body belongs to *that* callable, so it is not descended into.
    fn scan_returns(&self, s: &Stmt, saw_one: &mut bool, all_wide: &mut bool) {
        let nested = |c: &Self, body: &[Stmt], saw: &mut bool, all: &mut bool| {
            for st in body {
                c.scan_returns(st, saw, all);
            }
        };
        match &s.kind {
            StmtKind::Return { value } => {
                *saw_one = true;
                *all_wide &= value.as_ref().is_some_and(|e| self.is_wide(e));
            }
            StmtKind::If { then, els, .. } => {
                nested(self, then, saw_one, all_wide);
                nested(self, els, saw_one, all_wide);
            }
            StmtKind::While { body, .. }
            | StmtKind::DoWhile { body, .. }
            | StmtKind::For { body, .. } => nested(self, body, saw_one, all_wide),
            StmtKind::Switch { cases, .. } => {
                for c in cases {
                    nested(self, &c.body, saw_one, all_wide);
                }
            }
            StmtKind::Try {
                body,
                catches,
                finally_body,
            } => {
                nested(self, body, saw_one, all_wide);
                for c in catches {
                    nested(self, &c.body, saw_one, all_wide);
                }
                nested(self, finally_body, saw_one, all_wide);
            }
            StmtKind::Labeled { stmt, .. } => self.scan_returns(stmt, saw_one, all_wide),
            _ => {}
        }
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
        let prev_tries = std::mem::take(&mut self.tries);
        let prev_finallys = std::mem::take(&mut self.finallys);
        // A closure's captures keep the width they had where it was written, so
        // only its own parameters reset; its locals are restored on the way out.
        let prev_widths = self.enter_width_scope(&pc.params);
        let saved_line = self.cur_line;
        self.cur_line = pc.line;
        // Inside a closure body a free name the compiler cannot bind may be a
        // `with`/`tap` delegate's property; outside one it never is.
        self.closure_depth += 1;

        // Prologue: pop the pushed params + captures top-down into their slots.
        for i in (0..total).rev() {
            self.b.emit(Op::SetSlot(i as u16), pc.line);
        }

        let prev_cells = self.enter_cell_scope(&pc.params, &pc.body, &pc.cell_captures, pc.line);
        self.fn_body(&pc.body)?;
        self.exit_cell_scope(prev_cells);
        self.closure_depth -= 1;

        // Fall-through: a closure with no trailing value expression returns null.
        self.b.emit(Op::LoadUndef, self.cur_line);
        self.b.emit(Op::ReturnValue, self.cur_line);

        self.scope = prev;
        self.cur_class_fields = prev_fields;
        self.cur_class_methods = prev_methods;
        self.tries = prev_tries;
        self.finallys = prev_finallys;
        self.exit_width_scope(prev_widths);
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
    #[allow(clippy::too_many_arguments)]
    fn register_class(
        &mut self,
        name: &str,
        superclass: Option<&str>,
        interfaces: &[String],
        is_interface: bool,
        is_trait: bool,
        generated: Generated,
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
        // `interface` flag, the `trait` flag, then the implemented-interface
        // name array. Pushed in that order, so the register builtin pops the
        // trait flag before the interface one.
        let iidx = self.b.add_constant(Value::bool(is_interface));
        self.b.emit(Op::LoadConst(iidx), line);
        let tidx = self.b.add_constant(Value::bool(is_trait));
        self.b.emit(Op::LoadConst(tidx), line);
        // What the class's annotations asked to be generated, as one bit set —
        // the register builtin pops it next.
        let gbits = (generated.to_string as i64)
            | ((generated.include_names as i64) << 1)
            | ((generated.equals_hash as i64) << 2)
            | ((generated.tuple_ctor as i64) << 3);
        self.b.emit(Op::LoadInt(gbits), line);
        for i in interfaces {
            let c = self.b.add_constant(Value::str(i.clone()));
            self.b.emit(Op::LoadConst(c), line);
        }
        self.b.emit(Op::MakeArray(interfaces.len() as u16), line);
        // class name
        let nidx = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(nidx), line);
        // field-name array (declaration order). Each name carries its declared
        // type after a `:` — the one thing the type decides at run time is an
        // uninitialised *primitive* field's zero, and packing it here avoids a
        // second parallel array through the register builtin.
        for f in fields {
            let c = self
                .b
                .add_constant(Value::str(format!("{}:{}", f.name, f.ty)));
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
        // method-arity table: name -> declared parameter count. Methods are keyed
        // by name alone (see the overloading entry in BUGS.md), so this does not
        // separate overloads — what it answers is how many parameters the method
        // that *did* register under a name takes, which is what
        // `propertyMissing` needs to tell its reader form from its writer form.
        for m in methods {
            let k = self.b.add_constant(Value::str(m.name.clone()));
            self.b.emit(Op::LoadConst(k), line);
            self.b.emit(Op::LoadInt(m.params.len() as i64), line);
        }
        self.b.emit(Op::MakeHash((methods.len() * 2) as u16), line);
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
        // Breadth-first over the superclass chain and every implemented
        // interface (transitively); `seen` terminates a cyclic `extends`.
        let mut queue = vec![class.to_string()];
        let mut seen: HashSet<String> = HashSet::new();
        let mut i = 0;
        while i < queue.len() {
            let name = queue[i].clone();
            i += 1;
            if !seen.insert(name.clone()) {
                continue;
            }
            let Some(info) = self.class_index.get(&name) else {
                continue;
            };
            for n in select(info) {
                set.insert(n.clone());
            }
            queue.extend(info.superclass.iter().cloned());
            queue.extend(info.interfaces.iter().cloned());
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
        let prev_tries = std::mem::take(&mut self.tries);
        let prev_finallys = std::mem::take(&mut self.finallys);
        let saved_line = self.cur_line;
        self.cur_line = line;

        // Prologue: pop `this` + args top-down into slots 0..n+1.
        for i in (0..params.len() + 1).rev() {
            self.b.emit(Op::SetSlot(i as u16), line);
        }
        let prev_cells = self.enter_cell_scope(params, body, &HashSet::new(), line);
        self.fn_body(body)?;
        self.exit_cell_scope(prev_cells);
        self.b.emit(Op::LoadUndef, self.cur_line);
        self.b.emit(Op::ReturnValue, self.cur_line);

        self.scope = prev;
        self.cur_class_fields = prev_fields;
        self.cur_class_methods = prev_methods;
        self.tries = prev_tries;
        self.finallys = prev_finallys;
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
        let prev_tries = std::mem::take(&mut self.tries);
        let prev_finallys = std::mem::take(&mut self.finallys);
        let prev_cells = std::mem::take(&mut self.cells);
        let saved_line = self.cur_line;
        self.cur_line = line;
        self.expr(init)?;
        self.b.emit(Op::ReturnValue, self.cur_line);
        self.cells = prev_cells;
        self.scope = prev;
        self.cur_class_fields = prev_fields;
        self.cur_class_methods = prev_methods;
        self.tries = prev_tries;
        self.finallys = prev_finallys;
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

    /// True when `name` is a statically named JDK class (`Math`, `Integer`, …)
    /// rather than a variable — so it lowers to a `java.lang.Class` reference.
    /// A script-declared class of the same name, a local, or a field all win,
    /// which is Groovy's own resolution order.
    fn is_static_class_ref(&self, name: &str) -> bool {
        crate::host::jdk_class_package(name).is_some()
            && !self.is_local(name)
            && !self.class_index.contains_key(name)
    }

    /// The dotted package a receiver expression spells, if it is nothing but a
    /// chain of lowercase-initial identifiers (`java`, `java.math`) rooted at a
    /// name that is not bound to anything. That is what makes
    /// `java.math.RoundingMode.HALF_UP` reachable: the parser sees an ordinary
    /// property chain, and only the package shape tells it apart from a real
    /// receiver. A binding of the same name wins, as it does in Groovy.
    fn package_prefix(&self, e: &Expr) -> Option<String> {
        match e {
            Expr::Var(name) => {
                let first = name.chars().next()?;
                if !first.is_lowercase()
                    || self.is_local(name)
                    || self.is_field(name)
                    || self.class_index.contains_key(name)
                {
                    return None;
                }
                Some(name.clone())
            }
            Expr::Property {
                recv,
                name,
                safe: false,
                ..
            } if name.chars().next().is_some_and(char::is_lowercase) => {
                Some(format!("{}.{name}", self.package_prefix(recv)?))
            }
            _ => None,
        }
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
    fn emit_field_get(&mut self, name: &str) -> Result<(), String> {
        self.emit_this();
        let c = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(c), self.cur_line);
        self.emit_call_builtin(crate::host::GPROP, 0, self.cur_line)
    }

    /// Emit `binary`'s left operand, unless a compound assignment already
    /// pushed its value.
    fn emit_binary_lhs(&mut self, preloaded: bool, lhs: &Expr) -> Result<(), String> {
        if preloaded {
            return Ok(());
        }
        self.expr(lhs)
    }

    /// Lower the right-hand side of an assignment to an expression target,
    /// combining it with the old value the caller has already pushed.
    ///
    /// A plain `=` just lowers the value; a compound op expects the old value on
    /// top of the stack and leaves the new one in its place. `/=` and `%=` go
    /// through their own builtins for the same reason the bare-name form does:
    /// Groovy's `/` promotes to `BigDecimal` and its `%` has a zero-divisor
    /// guard, neither of which the raw arithmetic op carries.
    fn emit_compound_value(
        &mut self,
        op: AssignOp,
        target: &Expr,
        value: &Expr,
    ) -> Result<(), String> {
        match op {
            AssignOp::Assign => self.expr(value),
            AssignOp::Div => {
                self.expr(value)?;
                self.emit_call_builtin(crate::host::GDIV, 2, self.cur_line)
            }
            AssignOp::Mod => {
                self.expr(value)?;
                self.emit_mod(value, self.cur_line)
            }
            // `<<=`, `>>=`, `>>>=`, `&=`, `|=`, `^=`, `**=`. Lowered by the
            // binary-operator path itself rather than by a switch of their own:
            // those operators route on the *operands* (a `BigInteger` takes a
            // builtin where an `int` keeps a native op, a `>>` masks its count
            // to the left operand's Java width), and a second implementation
            // would drift from `x = x <op> n`. `target` is the left operand —
            // its value is already on the stack, so `preloaded_lhs` tells
            // `binary` to analyse it without emitting it again, which is what
            // keeps `m[key()] <<= 5` calling `key` once.
            AssignOp::Bin(bin) => {
                self.preloaded_lhs = true;
                let r = self.binary(bin, target, value);
                self.preloaded_lhs = false;
                r
            }
            _ => {
                self.expr(value)?;
                self.b.emit(compound_op(op), self.cur_line);
                Ok(())
            }
        }
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
                self.emit_field_get(name)?;
                self.expr(value)?;
                self.emit_call_builtin(crate::host::GDIV, 2, self.cur_line)?;
            }
            // `field %= e` shares `%`'s zero-divisor guard.
            AssignOp::Mod => {
                self.emit_field_get(name)?;
                self.expr(value)?;
                self.emit_mod(value, self.cur_line)?;
            }
            // As for a bare name: the bitwise/exponent forms take the binary
            // path. The target is `this.<name>`, which is what `emit_field_get`
            // just pushed.
            AssignOp::Bin(bin) => {
                self.emit_field_get(name)?;
                let target = Expr::Property {
                    recv: Box::new(Expr::This),
                    name: name.to_string(),
                    line: self.cur_line,
                    safe: false,
                };
                self.preloaded_lhs = true;
                let r = self.binary(bin, &target, value);
                self.preloaded_lhs = false;
                r?;
            }
            _ => {
                self.emit_field_get(name)?;
                self.expr(value)?;
                self.b.emit(compound_op(op), self.cur_line);
            }
        }
        let c = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(c), self.cur_line);
        self.emit_call_builtin(crate::host::GSETPROP, 0, self.cur_line)?;
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
            // Groovy's implicit return reaches through a trailing `if` or `try`:
            // the value is the last expression of whichever branch runs. Rewrite
            // that expression into an explicit `return` so the ordinary return
            // path — including the `finally` bodies it must run first — applies.
            StmtKind::If { .. } | StmtKind::Try { .. } => {
                match tail_return(std::slice::from_ref(last)) {
                    Some(rewritten) => {
                        for s in &rewritten {
                            self.stmt(s)?;
                        }
                    }
                    None => self.stmt(last)?,
                }
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
            StmtKind::Local { ty, name, init } => {
                // The initializer is lowered first: it may read an *outer*
                // variable this declaration shadows, which still has its own
                // width until the new binding takes effect.
                if let Some(e) = init {
                    self.expr(e)?;
                    self.note_var_width(ty, name, init.as_ref());
                    self.emit_decl_store(name, self.cur_line);
                    // `def y = null - 1` raises from the numeric hook, which has
                    // no check of its own. See the expression-statement arm.
                    self.emit_exc_check(self.cur_line)?;
                } else {
                    self.note_var_width(ty, name, None);
                    // An uninitialized local stays unbound (Groovy defaults it to
                    // `null`; a read before assignment yields `null`). Inside a
                    // function still register the slot so later reads/writes of the
                    // name resolve to the local, not a same-named global. A boxed
                    // one still needs its own cell here: the declaration is what
                    // makes the variable, so a `def v` inside a loop body is a
                    // fresh binding each iteration whether or not it is initialized.
                    self.declare_slot(name);
                    self.emit_cell_init(name, self.cur_line);
                }
                Ok(())
            }
            StmtKind::Assign { name, op, value } => {
                // A bare field name inside a method/constructor is `this.field`.
                if self.is_field(name) {
                    return self.assign_field(name, *op, value);
                }
                // An undeclared `x = 1L` is a script binding, and it is a `Long`
                // for the same reason a declared one is. A plain `=` re-binds
                // the width in both directions (`a = 5` after `def a = 5L` is an
                // `Integer` again); a compound `x += e` combines with what `x`
                // already holds, so it can only widen. The new width is applied
                // *after* the value is lowered, because the value may read the
                // variable itself (`a = a * 2`) at its old width.
                let new_wide = if matches!(op, AssignOp::Assign) {
                    self.is_wide(value)
                } else {
                    self.wide_vars.contains(name) || self.is_wide(value)
                };
                match op {
                    AssignOp::Assign => {
                        self.expr(value)?;
                    }
                    AssignOp::Div => {
                        // `x /= e` → x = x / e, through the Groovy division builtin.
                        self.emit_name_load(name, self.cur_line)?;
                        self.expr(value)?;
                        self.emit_call_builtin(crate::host::GDIV, 2, self.cur_line)?;
                    }
                    // `x %= e` shares `%`'s zero-divisor guard.
                    AssignOp::Mod => {
                        self.emit_name_load(name, self.cur_line)?;
                        self.expr(value)?;
                        self.emit_mod(value, self.cur_line)?;
                    }
                    // `x <<= e` and the other bitwise/exponent forms lower
                    // through the binary path, so they inherit its operand
                    // routing rather than repeating it. The name's value is
                    // pushed first and `preloaded_lhs` stops `binary` pushing it
                    // again.
                    AssignOp::Bin(bin) => {
                        self.emit_name_load(name, self.cur_line)?;
                        self.preloaded_lhs = true;
                        let r = self.binary(*bin, &Expr::Var(name.clone()), value);
                        self.preloaded_lhs = false;
                        r?;
                    }
                    _ => {
                        // `x <op>= e` → x = x <op> e
                        self.emit_name_load(name, self.cur_line)?;
                        self.expr(value)?;
                        let pos = self.b.emit(compound_op(*op), self.cur_line);
                        // `long t = 0; t += 2000000000; t += 2000000000` is
                        // `4000000000`: the target's own width decides, not the
                        // running value's, which is still inside `Integer`
                        // range when the second `+=` overflows it.
                        self.mark_wide_site(pos, &Expr::Var(name.clone()), value);
                    }
                }
                self.emit_name_store(name, self.cur_line)?;
                // As for a declaration: `y = null - 1` raises from the numeric
                // hook, whose throw nothing downstream would notice.
                self.emit_exc_check(self.cur_line)?;
                self.set_var_width(name, new_wide);
                // `f = { it }` binds a closure to an undeclared name as surely as
                // `def f = { it }` does; a compound `f += …` cannot produce one.
                if matches!(op, AssignOp::Assign) {
                    self.set_var_obj("", name, Some(value));
                } else {
                    self.obj_vars.remove(name);
                }
                Ok(())
            }
            StmtKind::SetProperty {
                recv,
                name,
                op,
                value,
            } => {
                // `recv.name = value` — stack: recv (deepest), value, name.
                self.expr(recv)?;
                if !matches!(op, AssignOp::Assign) {
                    // `recv.name <op>= value`. The receiver is already on the
                    // stack and is duplicated to read the old value through, so
                    // the receiver expression runs once — `f().x += 1` calls `f`
                    // a single time, as Groovy's does.
                    self.b.emit(Op::Dup, self.cur_line);
                    let c = self.b.add_constant(Value::str(name.clone()));
                    self.b.emit(Op::LoadConst(c), self.cur_line);
                    self.emit_call_builtin(crate::host::GPROP, 0, self.cur_line)?;
                }
                // The target expression, for the operand analyses `binary` runs
                // (its Java width, whether it may be a `BigInteger`). It is
                // never emitted — its value is the one on the stack.
                let target = Expr::Property {
                    recv: Box::new(recv.clone()),
                    name: name.clone(),
                    line: self.cur_line,
                    safe: false,
                };
                self.emit_compound_value(*op, &target, value)?;
                let c = self.b.add_constant(Value::str(name.clone()));
                self.b.emit(Op::LoadConst(c), self.cur_line);
                self.emit_call_builtin(crate::host::GSETPROP, 0, self.cur_line)?;
                self.b.emit(Op::Pop, self.cur_line);
                Ok(())
            }
            StmtKind::SetIndex {
                recv,
                index,
                op,
                value,
            } => {
                // `recv[index] = value` — stack: recv (deepest), index, value.
                // The builtin answers the receiver's new contents, which a
                // variable receiver stores back (a fusevm list is a value, so a
                // list element cannot be written through the handle).
                self.expr(recv)?;
                self.expr(index)?;
                if !matches!(op, AssignOp::Assign) {
                    // `recv[index] <op>= value`. Both are already on the stack
                    // and are duplicated to read the old element through, so
                    // each runs once — `m[key()] += 5` calls `key` a single
                    // time, as Groovy's does.
                    self.b.emit(Op::Dup2, self.cur_line);
                    self.emit_call_builtin(crate::host::GINDEX, 0, self.cur_line)?;
                }
                let target = Expr::Index {
                    recv: Box::new(recv.clone()),
                    index: Box::new(index.clone()),
                    line: self.cur_line,
                };
                self.emit_compound_value(*op, &target, value)?;
                self.emit_call_builtin(crate::host::GSETINDEX, 0, self.cur_line)?;
                match recv {
                    Expr::Var(name) if !self.is_field(name) => {
                        self.emit_name_store(name, self.cur_line)?;
                    }
                    _ => {
                        self.b.emit(Op::Pop, self.cur_line);
                    }
                }
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
                self.inc_dec_update(name, *inc)
            }
            StmtKind::Expr(e) => {
                self.expr(e)?;
                self.b.emit(Op::Pop, self.cur_line);
                // A throw raised by fusevm's numeric hook — `null % 3`, `null + 1`
                // — has no check of its own: the hook is called from inside the
                // dispatch loop for a *native* arithmetic op, and only a builtin
                // call is followed by `emit_exc_check`. In a discarded expression
                // statement there may be no builtin call after it at all, and the
                // exception then escapes the enclosing `try` entirely
                // (`try { def y = println(-7); y % 3 } catch (…)` never entered
                // the handler). Where a later call did happen to notice it, the
                // statements in between had already run and printed. Checking at
                // the end of the statement puts the throw where Groovy raises it.
                self.emit_exc_check(self.cur_line)?;
                Ok(())
            }
            StmtKind::If { cond, then, els } => self.branch_stmt(|c| c.if_stmt(cond, then, els)),
            StmtKind::While { cond, body } => self.branch_stmt(|c| c.while_stmt(cond, body)),
            StmtKind::DoWhile { body, cond } => self.branch_stmt(|c| c.do_while_stmt(body, cond)),
            StmtKind::Switch { subject, cases } => {
                self.branch_stmt(|c| c.switch_stmt(subject, cases))
            }
            StmtKind::Labeled { label, stmt } => {
                self.pending_label = Some(label.clone());
                let r = self.stmt(stmt);
                // A label on something that is not a loop/switch never bound;
                // drop it so it cannot leak onto the next one.
                self.pending_label = None;
                r
            }
            StmtKind::For {
                init,
                cond,
                update,
                body,
            } => self.branch_stmt(|c| c.for_stmt(init, cond, update, body)),
            StmtKind::Try {
                body,
                catches,
                finally_body,
            } => self.branch_stmt(|c| c.try_stmt(body, catches, finally_body, s.line)),
            StmtKind::Throw(e) => self.throw_stmt(e, s.line),
            StmtKind::Assert {
                cond,
                message,
                text,
                ast_text,
                value_names,
            } => self.assert_stmt(cond, message.as_ref(), text, ast_text, value_names, s.line),
            StmtKind::Break(label) => self.break_stmt(label.as_deref()),
            StmtKind::Continue(label) => self.continue_stmt(label.as_deref()),
            StmtKind::Return { value } => {
                // Evaluate the returned expression *before* the cleanup, so a
                // `finally` that mutates a variable cannot change the value
                // already computed — Java/Groovy's rule.
                if let Some(e) = value {
                    self.expr(e)?;
                }
                self.emit_finallys(|_| true)?;
                if self.scope.is_some() {
                    if value.is_none() {
                        self.b.emit(Op::LoadUndef, self.cur_line);
                    }
                    self.b.emit(Op::ReturnValue, self.cur_line);
                } else {
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

    /// Lower a condition: the expression, followed by the Groovy-truthiness
    /// builtin **only when the expression's static shape could be a value
    /// fusevm's own truth test gets wrong** (a heap handle — `BigDecimal`,
    /// ordered map, closure, instance — or a `String`, which fusevm reads
    /// shell-style so `"0"` is false).
    ///
    /// This is what keeps the fix free for the loops that matter: a
    /// comparison-shaped guard (`i < n`, `x != 0`, `!done`) is statically a
    /// `Boolean`, so it emits exactly the ops it did before — a native
    /// `NumLt`/`JumpIfFalse` pair the JIT still traces. Only a condition whose
    /// type is not statically known (`while (x)`, `if (m)`) pays one builtin
    /// call, and that is precisely the case where the answer was wrong before.
    fn cond_expr(&mut self, cond: &Expr) -> Result<(), String> {
        self.expr(cond)?;
        if needs_truth(cond) {
            // The truth builtin's own check covers the condition.
            return self.emit_call_builtin(crate::host::GTRUTH, 0, self.cur_line);
        }
        // A statically-boolean condition emits no builtin, so a throw from
        // inside it — `if (null % 3 == 0)` — would otherwise reach the jump and
        // run a branch chosen from a value that was never computed.
        self.emit_exc_check(self.cur_line)
    }

    /// Lower a condition that must *keep* its operand as the expression's value
    /// — the Elvis operator, whose result is the deciding operand itself
    /// (`0 ?: "x"` is `"x"`, `5 ?: "x"` is `5`). Leaves the operand on the stack
    /// with its truth value pushed above it, so a `JumpIf*` consumes only the
    /// truth value. When the operand is statically fusevm-truth-compatible
    /// nothing extra is emitted and the caller uses the `…Keep` jump instead.
    ///
    /// Returns `true` when the truth value was pushed (so the caller must use the
    /// plain `JumpIfTrue`/`JumpIfFalse` form).
    fn cond_expr_keep(&mut self, cond: &Expr) -> Result<bool, String> {
        self.expr(cond)?;
        if needs_truth(cond) {
            self.emit_call_builtin(crate::host::GTRUTH_KEEP, 0, self.cur_line)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Lower an expression to a strict `Boolean`, for `&&`/`||` — whose *value*
    /// in Groovy is a `Boolean`, not the deciding operand (`5 && 3` is `true`,
    /// not `3`). An operand that is already statically a `Boolean` (a comparison,
    /// `!x`, `instanceof`, a nested `&&`/`||`) emits nothing extra, so
    /// `i < n && j < m` keeps its native ops.
    fn bool_expr(&mut self, e: &Expr) -> Result<(), String> {
        self.expr(e)?;
        if !is_static_bool(e) {
            self.emit_call_builtin(crate::host::GTRUTH, 0, self.cur_line)?;
        }
        Ok(())
    }

    // ── exceptions (`throw` / `try` / `catch` / `finally`) ──────────────────
    //
    // fusevm has no unwind opcode, so an in-flight exception is a host-side
    // pending value ([`crate::host::GTHROW`]) plus two compiler-side pieces:
    //
    //   * inside a frame, an unwind is a `Jump` to the innermost enclosing
    //     handler, backpatched through [`Compiler::tries`];
    //   * across a frame boundary it is `LoadUndef; ReturnValue`, and every call
    //     site is followed by a pending-exception check that repeats the unwind
    //     in the caller. `Op::ReturnValue` truncates the value stack to the
    //     frame base, so the abandoned operands of the callee cost nothing.
    //
    // Only the second piece has a runtime cost, and only in a program that uses
    // exceptions at all ([`Compiler::has_exceptions`]).

    /// A fresh compiler-minted temporary name (a frame slot inside a function, a
    /// script binding at top level). The `$` prefix keeps it unnameable in
    /// Groovy source.
    fn fresh_temp(&mut self, tag: &str) -> String {
        let n = self.temps_seen;
        self.temps_seen += 1;
        format!("$exc_{tag}_{n}")
    }

    fn emit_temp_set(&mut self, name: &str) {
        let op = self.store_op_for_decl(name);
        self.b.emit(op, self.cur_line);
    }

    fn emit_temp_get(&mut self, name: &str) {
        let op = self.load_op_for(name);
        self.b.emit(op, self.cur_line);
    }

    /// Emit a builtin call followed by the pending-exception check. Used for
    /// every builtin that can re-enter the VM to run user code (method and
    /// closure dispatch, `new`, property get/set, subscripting, `/`, `<=>`,
    /// `println`'s `toString`, the truthiness `asBoolean`). A no-op beyond the
    /// call itself in a program without exceptions.
    fn emit_call_builtin(&mut self, id: u16, argc: u8, line: u32) -> Result<(), String> {
        self.b.emit(Op::CallBuiltin(id, argc), line);
        self.emit_exc_check(line)
    }

    /// Emit the post-call check: if the call left an exception in flight, unwind.
    /// Must follow *every* call that can re-enter the VM — a path that skips it
    /// swallows the exception and resumes with a placeholder value.
    fn emit_exc_check(&mut self, line: u32) -> Result<(), String> {
        if !self.has_exceptions {
            return Ok(());
        }
        self.b
            .emit(Op::CallBuiltin(crate::host::GEXC_PENDING, 0), line);
        let jf = self.b.emit(Op::JumpIfFalse(0), line);
        self.emit_unwind(line)?;
        let after = self.b.current_pos();
        self.b.patch_jump(jf, after);
        Ok(())
    }

    /// Abandon the current computation: run any `finally` bodies this exit
    /// skips, then jump to the innermost handler in this frame — or leave the
    /// frame so the caller's post-call check picks the exception up. At script
    /// top level it jumps to the program exit, where the uncaught report runs.
    fn emit_unwind(&mut self, line: u32) -> Result<(), String> {
        // A `finally` whose `try` scope has already been popped (we are inside
        // its `catch` arms) is skipped by the jump below, so run it here.
        let depth = self.tries.len();
        self.emit_finallys(|f| f.try_depth > depth)?;
        if !self.tries.is_empty() {
            let op = self.b.emit(Op::Jump(0), line);
            self.tries.last_mut().unwrap().unwind_ops.push(op);
        } else if self.scope.is_some() {
            // A function/method/closure frame: return a placeholder so the frame
            // is popped and the stack rebalanced. The caller acts on the pending
            // exception, not on this value.
            self.b.emit(Op::LoadUndef, line);
            self.b.emit(Op::ReturnValue, line);
        } else {
            let op = self.b.emit(Op::Jump(0), line);
            self.exit_ops.push(op);
        }
        Ok(())
    }

    /// Emit, innermost first, the `finally` bodies of the enclosing frames that
    /// `keep` selects — the cleanup an early exit would otherwise skip. Each
    /// body is lowered with the frame stack emptied, so a `return` inside a
    /// `finally` cannot re-emit its own cleanup forever.
    fn emit_finallys(&mut self, keep: impl Fn(&FinallyFrame) -> bool) -> Result<(), String> {
        let selected: Vec<usize> = (0..self.finallys.len())
            .rev()
            .filter(|&i| keep(&self.finallys[i]))
            .collect();
        if selected.is_empty() {
            return Ok(());
        }
        let saved = std::mem::take(&mut self.finallys);
        let mut result = Ok(());
        for &i in &selected {
            for s in &saved[i].body {
                result = result.and(self.stmt(s));
            }
        }
        self.finallys = saved;
        result
    }

    /// Lower `throw <expr>`: evaluate the throwable, park it as the pending
    /// exception, then unwind.
    fn throw_stmt(&mut self, e: &Expr, line: u32) -> Result<(), String> {
        self.expr(e)?;
        self.b.emit(Op::CallBuiltin(crate::host::GTHROW, 1), line);
        self.b.emit(Op::Pop, line);
        self.emit_unwind(line)?;
        Ok(())
    }

    /// Lower `assert cond [: message]`.
    ///
    /// ```text
    ///   GASSERT_START               ; clear the value recorder
    ///   <cond>                      ; recorded sub-expressions call GASSERT_REC
    ///   GTRUTH; JumpIfTrue ok
    ///   <message or null>
    ///   GASSERT_FAIL(text, names)   ; renders and parks the AssertionError
    ///   <unwind>
    /// ok:
    /// ```
    ///
    /// The recorder is host-side because the rendering needs every value's
    /// source column *and* its position in the line layout, which only the host
    /// can compute once all of them are known.
    fn assert_stmt(
        &mut self,
        cond: &Expr,
        message: Option<&Expr>,
        text: &str,
        ast_text: &str,
        value_names: &[(String, u32)],
        line: u32,
    ) -> Result<(), String> {
        self.b
            .emit(Op::CallBuiltin(crate::host::GASSERT_START, 0), line);
        self.expr(cond)?;
        self.b.emit(Op::CallBuiltin(crate::host::GTRUTH, 1), line);
        let ok = self.b.emit(Op::JumpIfTrue(0), line);
        match message {
            Some(m) => self.expr(m)?,
            None => {
                self.b.emit(Op::LoadUndef, line);
            }
        }
        let t = self.b.add_constant(Value::str(text.to_string()));
        self.b.emit(Op::LoadConst(t), line);
        let a = self.b.add_constant(Value::str(ast_text.to_string()));
        self.b.emit(Op::LoadConst(a), line);
        // The `Values:` clause names travel as one comma-joined constant, and
        // their values as a parallel list read *here*, not from the power-assert
        // recorder: Groovy reports a variable's current value even when `&&`
        // short-circuited past the operand it sits in.
        let joined = value_names
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>()
            .join(",");
        let n = self.b.add_constant(Value::str(joined));
        self.b.emit(Op::LoadConst(n), line);
        for (name, _) in value_names {
            self.emit_var_read(name, line);
        }
        self.b.emit(Op::MakeArray(value_names.len() as u16), line);
        self.b
            .emit(Op::CallBuiltin(crate::host::GASSERT_FAIL, 0), line);
        self.b.emit(Op::Pop, line);
        self.emit_unwind(line)?;
        let after = self.b.current_pos();
        self.b.patch_jump(ok, after);
        Ok(())
    }

    /// Lower `try { … } catch (T e) { … }* [finally { … }]`.
    ///
    /// ```text
    ///   depth = GEXC_DEPTH          ; so the handler can drop abandoned operands
    ///   <try body>                  ; unwinds inside it jump to `handler`
    ///   Jump normal
    /// handler:
    ///   GEXC_CUT(depth); exc = GEXC_TAKE
    ///   if (exc instanceof E1) { e1 = exc; <catch 1>; Jump normal }
    ///   …
    ///   <finally>                   ; unmatched: the cleanup still runs …
    ///   GTHROW(exc); <unwind>       ; … then the exception continues outward
    /// normal:
    ///   <finally>
    /// ```
    ///
    /// The `finally` body is emitted once per exit path rather than shared
    /// through a subroutine, because a shared copy would need a return address
    /// and fusevm's frames are for calls, not local jumps — the same duplication
    /// `javac` performs.
    fn try_stmt(
        &mut self,
        body: &[Stmt],
        catches: &[CatchArm],
        finally_body: &[Stmt],
        line: u32,
    ) -> Result<(), String> {
        // Record the stack depth so the handler can discard whatever the
        // abandoned expression had already pushed.
        let depth_t = self.fresh_temp("depth");
        self.b
            .emit(Op::CallBuiltin(crate::host::GEXC_DEPTH, 0), line);
        self.emit_temp_set(&depth_t);

        self.tries.push(TryScope {
            unwind_ops: Vec::new(),
        });
        let has_finally = !finally_body.is_empty();
        if has_finally {
            self.finallys.push(FinallyFrame {
                body: finally_body.to_vec(),
                loop_depth: self.loops.len(),
                try_depth: self.tries.len(),
            });
        }
        for s in body {
            self.stmt(s)?;
        }
        let scope = self.tries.pop().unwrap();
        let to_normal = self.b.emit(Op::Jump(0), line);

        // ── handler ──
        let handler = self.b.current_pos();
        for op in scope.unwind_ops {
            self.b.patch_jump(op, handler);
        }
        self.emit_temp_get(&depth_t);
        self.b.emit(Op::CallBuiltin(crate::host::GEXC_CUT, 1), line);
        self.b.emit(Op::Pop, line);
        let exc_t = self.fresh_temp("exc");
        self.b
            .emit(Op::CallBuiltin(crate::host::GEXC_TAKE, 0), line);
        self.emit_temp_set(&exc_t);

        // `catch` arms in source order — the first type match wins.
        let mut matched_jumps = Vec::new();
        for arm in catches {
            // `exc instanceof T` for each caught type, OR-ed for a multi-catch.
            let mut type_hits = Vec::new();
            for (i, ty) in arm.types.iter().enumerate() {
                if i > 0 {
                    self.b.emit(Op::Pop, line);
                }
                self.emit_temp_get(&exc_t);
                let c = self.b.add_constant(Value::str(ty.clone()));
                self.b.emit(Op::LoadConst(c), line);
                self.b
                    .emit(Op::CallBuiltin(crate::host::GINSTANCEOF, 0), line);
                if i + 1 < arm.types.len() {
                    type_hits.push(self.b.emit(Op::JumpIfTrueKeep(0), line));
                }
            }
            let tested = self.b.current_pos();
            for op in type_hits {
                self.b.patch_jump(op, tested);
            }
            let jf = self.b.emit(Op::JumpIfFalse(0), line);
            self.emit_temp_get(&exc_t);
            self.emit_temp_set(&arm.name);
            for s in &arm.body {
                self.stmt(s)?;
            }
            matched_jumps.push(self.b.emit(Op::Jump(0), line));
            let next = self.b.current_pos();
            self.b.patch_jump(jf, next);
        }
        // Past the arms the `finally` is no longer this block's responsibility —
        // both remaining paths emit it inline.
        if has_finally {
            self.finallys.pop();
        }
        // No arm matched: run `finally`, then let the exception continue outward.
        for s in finally_body {
            self.stmt(s)?;
        }
        self.emit_temp_get(&exc_t);
        self.b.emit(Op::CallBuiltin(crate::host::GTHROW, 1), line);
        self.b.emit(Op::Pop, line);
        self.emit_unwind(line)?;

        // ── normal completion (the body fell through, or an arm finished) ──
        let normal = self.b.current_pos();
        self.b.patch_jump(to_normal, normal);
        for op in matched_jumps {
            self.b.patch_jump(op, normal);
        }
        for s in finally_body {
            self.stmt(s)?;
        }
        Ok(())
    }

    fn if_stmt(&mut self, cond: &Expr, then: &[Stmt], els: &[Stmt]) -> Result<(), String> {
        self.cond_expr(cond)?;
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

    /// Lower `while (cond) { … }` **rotated**: the test is emitted once as an
    /// entry guard and once at the bottom, so the loop closes with a
    /// *conditional* backward branch instead of an unconditional `Jump` back to
    /// a test at the top.
    ///
    /// That shape is what fusevm's tracing JIT needs — it only closes a trace
    /// on a conditional backward branch. Emitted the other way, `--tiers`
    /// reported `trace-eligible=true traced=false` and `reaches native code
    /// false` for every `for` and `while` groovyrs produced, and the loop
    /// stayed in the interpreter however hot it got; the same arithmetic
    /// written as `do { … } while (…)` — the one loop form that already ended
    /// in a conditional branch — reported `traced=true` and ran three orders of
    /// magnitude faster (3 000 000 iterations of `s += i % 7`: 3.88 s
    /// against 0.01 s, measured).
    ///
    /// Rotation costs one copy of the condition's code and saves one jump per
    /// iteration. Evaluation order and count are unchanged: a top-test loop
    /// runs the test `n + 1` times for `n` iterations, and so does this — one
    /// entry test, plus one after each body run.
    fn while_stmt(&mut self, cond: &Expr, body: &[Stmt]) -> Result<(), String> {
        let label = self.pending_label.take();
        let cond_line = self.cur_line;
        self.cond_expr(cond)?;
        let jf = self.b.emit(Op::JumpIfFalse(0), self.cur_line);
        let top = self.b.current_pos();
        self.loops.push(Loop::new(label, false));
        for s in body {
            self.stmt(s)?;
        }
        // `continue` in a `while` re-tests the condition, which is now the
        // bottom copy of it.
        let test = self.b.current_pos();
        let l = self.loops.pop().unwrap();
        for op in &l.continue_ops {
            self.b.patch_jump(*op, test);
        }
        self.cur_line = cond_line;
        self.cond_expr(cond)?;
        self.b.emit(Op::JumpIfTrue(top), self.cur_line);
        let end = self.b.current_pos();
        self.b.patch_jump(jf, end);
        for op in l.break_ops {
            self.b.patch_jump(op, end);
        }
        Ok(())
    }

    /// Lower the C-style `for (init; cond; update)`, rotated for the reason
    /// [`Compiler::while_stmt`] gives. `continue` targets the update, which now
    /// sits immediately before the bottom copy of the test.
    fn for_stmt(
        &mut self,
        init: &Option<Box<Stmt>>,
        cond: &Option<Expr>,
        update: &Option<Box<Stmt>>,
        body: &[Stmt],
    ) -> Result<(), String> {
        let label = self.pending_label.take();
        if let Some(init) = init {
            self.stmt(init)?;
        }
        let cond_line = self.cur_line;
        let jf = match cond {
            Some(c) => {
                self.cond_expr(c)?;
                Some(self.b.emit(Op::JumpIfFalse(0), self.cur_line))
            }
            None => None,
        };
        let top = self.b.current_pos();
        // `continue` runs the update clause, then re-tests — target it at the
        // step label emitted after the body.
        self.loops.push(Loop::new(label, false));
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
        self.cur_line = cond_line;
        match cond {
            Some(c) => {
                self.cond_expr(c)?;
                self.b.emit(Op::JumpIfTrue(top), self.cur_line);
            }
            // `for (;;)` has no test to branch on, so its back edge stays
            // unconditional — and so, necessarily, does its JIT eligibility.
            None => {
                self.b.emit(Op::Jump(top), self.cur_line);
            }
        }
        let end = self.b.current_pos();
        if let Some(jf) = jf {
            self.b.patch_jump(jf, end);
        }
        for op in l.break_ops {
            self.b.patch_jump(op, end);
        }
        Ok(())
    }

    /// Lower `do { … } while (cond)`. The body precedes the test, so it always
    /// runs at least once; `continue` targets the test, not the top.
    fn do_while_stmt(&mut self, body: &[Stmt], cond: &Expr) -> Result<(), String> {
        let label = self.pending_label.take();
        let top = self.b.current_pos();
        self.loops.push(Loop::new(label, false));
        for s in body {
            self.stmt(s)?;
        }
        // `continue` in a `do`/`while` skips the rest of the body and re-tests.
        let test = self.b.current_pos();
        let l = self.loops.pop().unwrap();
        for op in &l.continue_ops {
            self.b.patch_jump(*op, test);
        }
        self.cond_expr(cond)?;
        self.b.emit(Op::JumpIfTrue(top), self.cur_line);
        let end = self.b.current_pos();
        for op in l.break_ops {
            self.b.patch_jump(op, end);
        }
        Ok(())
    }

    /// The index in [`Compiler::loops`] a `break`/`continue` targets: the frame
    /// carrying `label`, or the innermost frame when unlabeled. A `continue`
    /// additionally skips `switch` frames, which it passes straight through.
    fn loop_target(&self, label: Option<&str>, for_continue: bool) -> Option<usize> {
        self.loops.iter().enumerate().rev().find_map(|(i, l)| {
            let usable = !(for_continue && l.is_switch);
            let named = match label {
                Some(want) => l.label.as_deref() == Some(want),
                None => true,
            };
            (usable && named).then_some(i)
        })
    }

    /// Lower `break` / `break label`: run the `finally` bodies the exit skips,
    /// then jump to the target frame's exit. A `break` with no enclosing frame
    /// at all leaves the script, which is what Groovy's top-level `break` does.
    fn break_stmt(&mut self, label: Option<&str>) -> Result<(), String> {
        let target = self.loop_target(label, false);
        if label.is_some() && target.is_none() {
            return Err(format!(
                "groovyrs: no enclosing loop labeled `{}` on line {}",
                label.unwrap_or_default(),
                self.cur_line
            ));
        }
        // Any `finally` entered inside the frame being left runs first.
        let depth = target.map_or(0, |i| i + 1);
        self.emit_finallys(|f| f.loop_depth >= depth)?;
        let op = self.b.emit(Op::Jump(0), self.cur_line);
        match target {
            Some(i) => self.loops[i].break_ops.push(op),
            None => self.exit_ops.push(op),
        }
        Ok(())
    }

    /// Lower `continue` / `continue label`.
    fn continue_stmt(&mut self, label: Option<&str>) -> Result<(), String> {
        let target = self.loop_target(label, true).ok_or_else(|| match label {
            Some(l) => format!(
                "groovyrs: no enclosing loop labeled `{l}` on line {}",
                self.cur_line
            ),
            None => "groovyrs: `continue` outside a loop".to_string(),
        })?;
        // Every `finally` entered *inside* the target frame (depth above it).
        self.emit_finallys(|f| f.loop_depth > target)?;
        let op = self.b.emit(Op::Jump(0), self.cur_line);
        self.loops[target].continue_ops.push(op);
        Ok(())
    }

    /// Lower `switch (subject) { case L: … default: … }`.
    ///
    /// ```text
    ///   subject -> $switch_N
    ///   $switch_N; <L0>; GIS_CASE; JumpIfTrue body0    ; dispatch chain, in
    ///   $switch_N; <L1>; GIS_CASE; JumpIfTrue body1    ; source order
    ///   Jump default_body (or end)
    /// body0: <stmts>                                   ; falls through …
    /// body1: <stmts>                                   ; … into the next body
    /// end:
    /// ```
    ///
    /// The dispatch chain runs first so a label expression is evaluated at most
    /// once and only until one matches, and the bodies are laid out contiguously
    /// so fall-through costs nothing. `break` targets `end` through a `switch`
    /// [`Loop`] frame; `continue` passes through it to the enclosing loop.
    fn switch_stmt(&mut self, subject: &Expr, cases: &[SwitchCase]) -> Result<(), String> {
        let label = self.pending_label.take();
        let subject_tmp = self.fresh_temp("switch");
        self.expr(subject)?;
        self.emit_temp_set(&subject_tmp);

        let mut entry_jumps: Vec<(usize, usize)> = Vec::new(); // (case index, op)
        for (i, case) in cases.iter().enumerate() {
            let Some(test) = &case.label else { continue };
            self.emit_temp_get(&subject_tmp);
            let builtin = self.case_label(test)?;
            self.emit_call_builtin(builtin, 0, self.cur_line)?;
            entry_jumps.push((i, self.b.emit(Op::JumpIfTrue(0), self.cur_line)));
        }
        // No label matched: enter `default` if the switch has one, else leave.
        let no_match = self.b.emit(Op::Jump(0), self.cur_line);

        self.loops.push(Loop::new(label, true));
        let mut starts: Vec<usize> = Vec::with_capacity(cases.len());
        for case in cases {
            starts.push(self.b.current_pos());
            for s in &case.body {
                self.stmt(s)?;
            }
        }
        let end = self.b.current_pos();
        let l = self.loops.pop().unwrap();

        for (i, op) in entry_jumps {
            self.b.patch_jump(op, starts[i]);
        }
        let default_start = cases
            .iter()
            .position(|c| c.label.is_none())
            .map_or(end, |i| starts[i]);
        self.b.patch_jump(no_match, default_start);
        for op in l.break_ops {
            self.b.patch_jump(op, end);
        }
        // A `continue` inside a switch belongs to the enclosing loop, which the
        // frame let through — nothing can be parked here.
        debug_assert!(l.continue_ops.is_empty());
        Ok(())
    }

    /// Lower one `case` label onto the stack and name the `isCase` builtin that
    /// decides it. A bare identifier naming a type is Groovy's `case String:` /
    /// `case MyClass:` class check, which has to be recognised here because a
    /// class name is not a value groovyrs can load; it pushes the *name* and
    /// routes to the type-check builtin. Every other label is an ordinary
    /// expression whose `isCase` the host decides from its runtime shape.
    fn case_label(&mut self, label: &Expr) -> Result<u16, String> {
        if let Expr::Var(name) = label {
            if self.names_a_type(name) {
                let idx = self.b.add_constant(Value::str(name.clone()));
                self.b.emit(Op::LoadConst(idx), self.cur_line);
                return Ok(crate::host::GIS_CASE_TYPE);
            }
        }
        self.expr(label)?;
        Ok(crate::host::GIS_CASE)
    }

    /// Does the bare name `name` denote a type rather than a variable? True for
    /// a class the script declares, a modeled built-in throwable, and the
    /// Groovy/Java type names `instanceof` already understands — unless a local
    /// of that name shadows it in the current frame.
    fn names_a_type(&self, name: &str) -> bool {
        if self
            .scope
            .as_ref()
            .is_some_and(|s| s.vars.contains_key(name))
        {
            return false;
        }
        self.class_index.contains_key(name)
            || crate::throwable::is_builtin(name)
            || BUILTIN_TYPE_NAMES.contains(&name)
    }

    /// Emit the in-place update `name = name ± 1` (leaving nothing on the stack).
    /// Used by both `++`/`--` in statement position and as the update step of the
    /// value-position pre/post forms.
    fn inc_dec_update(&mut self, name: &str, inc: bool) -> Result<(), String> {
        self.emit_name_load(name, self.cur_line)?;
        self.b.emit(Op::LoadInt(1), self.cur_line);
        self.b
            .emit(if inc { Op::Add } else { Op::Sub }, self.cur_line);
        self.emit_name_store(name, self.cur_line)
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
        self.emit_call_builtin(id, n, self.cur_line)?;
        Ok(())
    }

    fn expr(&mut self, e: &Expr) -> Result<(), String> {
        match e {
            Expr::Int(n, _) => {
                self.b.emit(Op::LoadInt(*n), self.cur_line);
            }
            Expr::Float(f) => {
                let c = self.b.add_constant(Value::float(*f));
                self.b.emit(Op::LoadConst(c), self.cur_line);
            }
            // The sequence a `for-in` walks: the host materialises the value's
            // iteration elements once, before the loop.
            Expr::Iterable(inner) => {
                self.expr(inner)?;
                self.emit_call_builtin(crate::host::GITER, 1, self.cur_line)?;
            }
            // A spread is only meaningful in an argument list, and every call
            // shape takes its arguments apart before lowering them — so reaching
            // here means one was written somewhere a call cannot expand it.
            Expr::SpreadArg(_) => {
                return Err(format!(
                    "groovyrs: `*` spread is only valid in an argument list on line {}",
                    self.cur_line
                ))
            }
            // A `~/…/` literal compiles at run time, through the host, because a
            // compiled pattern is a heap object with no fusevm representation.
            // An `assert` sub-expression whose value the power-assert renderer
            // shows: evaluate it, hand a copy to the recorder, keep the value.
            Expr::Recorded { col, inner } => {
                self.expr(inner)?;
                self.b.emit(Op::LoadInt(*col as i64), self.cur_line);
                self.b
                    .emit(Op::CallBuiltin(crate::host::GASSERT_REC, 0), self.cur_line);
            }
            Expr::Regex(src) => {
                let c = self.b.add_constant(Value::str(src.clone()));
                self.b.emit(Op::LoadConst(c), self.cur_line);
                self.b
                    .emit(Op::CallBuiltin(crate::host::GREGEX, 1), self.cur_line);
            }
            // A `BigDecimal` literal has no fusevm `Value` representation, so it
            // is built at run time: push the literal's text and let the `GDEC`
            // builtin intern it on the host heap (repeated evaluations of the
            // same literal share one handle).
            Expr::Dec(text) => {
                let c = self.b.add_constant(Value::str(text.clone()));
                self.b.emit(Op::LoadConst(c), self.cur_line);
                self.b
                    .emit(Op::CallBuiltin(crate::host::GDEC, 1), self.cur_line);
            }
            // A `java.math.BigInteger` literal is built the same way, from its
            // digits — the value has no machine-integer form to load.
            Expr::BigInt(text) => {
                let c = self.b.add_constant(Value::str(text.clone()));
                self.b.emit(Op::LoadConst(c), self.cur_line);
                self.b
                    .emit(Op::CallBuiltin(crate::host::GBIGINT, 1), self.cur_line);
            }
            Expr::Str(s) => {
                let c = self.b.add_constant(Value::str(s.clone()));
                self.b.emit(Op::LoadConst(c), self.cur_line);
            }
            // A `GString` pushes each rendered part and joins them through the
            // host builtin, which renders an object through its `toString()` —
            // lowering to `+` instead would miss that dispatch.
            Expr::GString(parts) => {
                for part in parts {
                    match part {
                        GStringPart::Text(t) => {
                            let c = self.b.add_constant(Value::str(t.clone()));
                            self.b.emit(Op::LoadConst(c), self.cur_line);
                        }
                        GStringPart::Expr(e) => self.expr(e)?,
                    }
                }
                self.b.emit(Op::LoadInt(parts.len() as i64), self.cur_line);
                self.emit_call_builtin(crate::host::GSTRING, 0, self.cur_line)?;
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
                    self.emit_field_get(name)?;
                } else if self.is_static_class_ref(name) {
                    // `Math`, `Integer`, … resolve to the JDK class, so that
                    // `Math.max(1, 2)` has a receiver to dispatch on.
                    let nidx = self.b.add_constant(Value::str(name.clone()));
                    self.b.emit(Op::LoadConst(nidx), self.cur_line);
                    self.emit_call_builtin(crate::host::GCLASSREF, 0, self.cur_line)?;
                } else {
                    self.emit_name_load(name, self.cur_line)?;
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
                let argc = self.emit_args(args, *line)?;
                let sname = self.cur_class_super.clone().unwrap_or_default();
                let sidx = self.b.add_constant(Value::str(sname));
                self.b.emit(Op::LoadConst(sidx), *line);
                self.emit_call_builtin(crate::host::GSUPER_CTOR, argc, *line)?;
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
                let argc = self.emit_args(args, *line)?;
                let c = self.b.add_constant(Value::str(class.clone()));
                self.b.emit(Op::LoadConst(c), *line);
                self.emit_call_builtin(crate::host::GNEW, argc, *line)?;
            }
            Expr::Index { recv, index, line } => {
                // `recv[index]` — stack: recv (deepest), index.
                self.expr(recv)?;
                self.expr(index)?;
                self.emit_call_builtin(crate::host::GINDEX, 0, *line)?;
            }
            Expr::CallValue { callee, args, line } => {
                // Invoke the value of `callee` with `args` — the postfix
                // call-application that makes `f(a)(b)` work. Reuses the
                // closure-call builtin with a synthetic name for diagnostics.
                self.expr(callee)?;
                let argc = self.emit_args(args, *line)?;
                let nidx = self.b.add_constant(Value::str("<closure>".to_string()));
                self.b.emit(Op::LoadConst(nidx), *line);
                self.emit_call_builtin(crate::host::GCLOSURE_CALL, argc, *line)?;
            }
            Expr::Unary { op, rhs } => match op {
                UnOp::Neg => {
                    self.expr(rhs)?;
                    self.b.emit(Op::Negate, self.cur_line);
                    if self.exc_after_arith {
                        self.emit_exc_check(self.cur_line)?;
                    }
                }
                // `!x` negates Groovy truth, so its operand goes through the
                // same condition lowering `if`/`while` use — otherwise `!0.0`
                // and `!"0"` would read the raw fusevm truth of the operand.
                UnOp::Not => {
                    self.cond_expr(rhs)?;
                    self.b.emit(Op::LogNot, self.cur_line);
                }
                // `~x` is the bitwise complement — a plain native op on an
                // integer, and the [`crate::host::GBITNOT`] builtin on an
                // operand that may ride a handle (`~7G` is `-8`, where the
                // native op read the handle as `0` and answered `-1`).
                UnOp::BitNot => {
                    self.expr(rhs)?;
                    if self.bit_operand_is_object(rhs) {
                        self.emit_call_builtin(crate::host::GBITNOT, 1, self.cur_line)?;
                    } else {
                        self.b.emit(Op::BitNot, self.cur_line);
                    }
                }
            },
            Expr::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs)?,
            // `value as Type`: evaluate the value, push the type name, coerce.
            Expr::Cast { value, ty } => {
                self.expr(value)?;
                let tidx = self.b.add_constant(Value::str(ty.clone()));
                self.b.emit(Op::LoadConst(tidx), self.cur_line);
                self.emit_call_builtin(crate::host::GCAST, 0, self.cur_line)?;
            }
            // Println/PostIncDec in value position: the print builtin leaves its
            // `null` return value on the stack.
            Expr::Println { newline, arg } => {
                self.println(*newline, arg.as_deref())?;
            }
            Expr::PostIncDec { name, inc } => {
                // Post: yield the value before the update, then update.
                self.emit_name_load(name, self.cur_line)?;
                self.inc_dec_update(name, *inc)?;
            }
            Expr::PreIncDec { name, inc } => {
                // Pre: update, then yield the new value.
                self.inc_dec_update(name, *inc)?;
                self.emit_name_load(name, self.cur_line)?;
            }
            Expr::Call { name, args, line } => self.call(name, args, *line)?,
            Expr::List(elems) => {
                // Groovy lists are references — `def b = a` names one
                // `ArrayList` twice, and `b.add(4)` is visible through `a` — so
                // a literal registers a host-heap handle rather than leaving a
                // fusevm array (a *value*) on the stack. `Op::MakeArray` still
                // gathers the elements; the builtin only wraps the result.
                for e in elems {
                    self.expr(e)?;
                }
                self.b
                    .emit(Op::MakeArray(elems.len() as u16), self.cur_line);
                self.emit_call_builtin(crate::host::GMAKE_LIST, 1, self.cur_line)?;
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
                // `getClass()` on a receiver the compiler knows is a `Long` has
                // to be told so: `1L` and `1` are the same runtime value, and
                // only the static width separates `java.lang.Long` from
                // `java.lang.Integer`.
                if method == "getClass" && args.is_empty() && self.is_wide(recv) {
                    self.expr(recv)?;
                    return self.emit_call_builtin(crate::host::GCLASS_LONG, 1, *line);
                }
                if matches!(**recv, Expr::Super) {
                    self.emit_this();
                    let argc = self.emit_args(args, *line)?;
                    let midx = self.b.add_constant(Value::str(method.clone()));
                    self.b.emit(Op::LoadConst(midx), *line);
                    let sname = self.cur_class_super.clone().unwrap_or_default();
                    let sidx = self.b.add_constant(Value::str(sname));
                    self.b.emit(Op::LoadConst(sidx), *line);
                    self.emit_call_builtin(crate::host::GSUPER_METHOD, argc, *line)?;
                } else {
                    // Stack: [recv, arg0..argN-1, methodname]; the GDK dispatch
                    // builtin pops the name, the N args, then the receiver. The
                    // safe-navigation form routes through GMETHOD_SAFE, which
                    // returns `null` without dispatching when the receiver is null.
                    // Java picks an overload by declared parameter width, and
                    // `16` and `16L` are the one `Value::Int`, so a call whose
                    // receiver or arguments the compiler can see are `Long`s
                    // carries their widths beneath the receiver and dispatches
                    // through `GMETHOD_WIDE`. A call with nothing wide at it —
                    // nearly every call — is emitted exactly as before.
                    let widths = if *safe {
                        0
                    } else {
                        self.call_width_mask(recv, args)
                    };
                    if widths != 0 {
                        self.b.emit(Op::LoadInt(i64::from(widths)), *line);
                    }
                    self.expr(recv)?;
                    let argc = self.emit_args(args, *line)?;
                    let midx = self.b.add_constant(Value::str(method.clone()));
                    self.b.emit(Op::LoadConst(midx), *line);
                    let id = match (*safe, widths) {
                        (true, _) => crate::host::GMETHOD_SAFE,
                        (false, 0) => crate::host::GMETHOD,
                        (false, _) => crate::host::GMETHOD_WIDE,
                    };
                    self.emit_call_builtin(id, argc, *line)?;
                    self.emit_receiver_writeback(recv, method, args)?;
                }
            }
            Expr::Property {
                recv,
                name,
                line,
                safe,
            } => {
                // A package-qualified class name (`java.math.RoundingMode`) is a
                // property chain in the tree and a *class* in the language.
                // Fold it here so the member read that follows dispatches on a
                // `java.lang.Class` rather than faulting on an unbound `java`.
                if let Some(pkg) = self.package_prefix(recv) {
                    if let Some(qualified) = crate::host::jdk_qualified_class(&pkg, name) {
                        let nidx = self.b.add_constant(Value::str(qualified));
                        self.b.emit(Op::LoadConst(nidx), *line);
                        return self.emit_call_builtin(crate::host::GCLASSREF, 0, *line);
                    }
                }
                // `.class` on a statically-`Long` receiver, for the reason
                // `getClass()` gives above.
                if name == "class" && self.is_wide(recv) {
                    self.expr(recv)?;
                    return self.emit_call_builtin(crate::host::GCLASS_LONG, 1, *line);
                }
                // Stack: [recv, propname]; the property builtin pops both.
                self.expr(recv)?;
                let nidx = self.b.add_constant(Value::str(name.clone()));
                self.b.emit(Op::LoadConst(nidx), *line);
                let id = if *safe {
                    crate::host::GPROP_SAFE
                } else {
                    crate::host::GPROP
                };
                self.emit_call_builtin(id, 0, *line)?;
            }
            Expr::Closure {
                params,
                body,
                explicit_params,
                varargs,
            } => self.closure(params, body, *explicit_params, *varargs)?,
            Expr::Range {
                start,
                end,
                inclusive,
            } => {
                // Materialise to a Groovy list of the enumerated values. This
                // goes through the host rather than the native `Op::Range`
                // because Groovy counts *down* when the start exceeds the end
                // and enumerates characters for string endpoints, neither of
                // which the native op does. The hot shape — `for (x in a..b)` —
                // never reaches here: the parser desugars it into a counted
                // `for` loop with native comparisons.
                self.expr(start)?;
                self.expr(end)?;
                self.b.emit(
                    if *inclusive {
                        Op::LoadTrue
                    } else {
                        Op::LoadFalse
                    },
                    self.cur_line,
                );
                self.emit_call_builtin(crate::host::GRANGE, 0, self.cur_line)?;
            }
            Expr::Ternary { cond, then, els } => {
                // `cond ? then : els` — branch on Groovy truthiness.
                self.cond_expr(cond)?;
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
                // `rhs`. With a statically-`Boolean` `lhs`, `JumpIfTrueKeep`
                // leaves the deciding value itself; otherwise the truth builtin
                // pushes the decision above it and a plain `JumpIfTrue` consumes
                // only that, again leaving `lhs`.
                let pushed = self.cond_expr_keep(lhs)?;
                let jt = self.b.emit(
                    if pushed {
                        Op::JumpIfTrue(0)
                    } else {
                        Op::JumpIfTrueKeep(0)
                    },
                    self.cur_line,
                );
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
    fn closure(
        &mut self,
        params: &[String],
        body: &[Stmt],
        explicit_params: bool,
        varargs: bool,
    ) -> Result<(), String> {
        // No parameter list at all means Groovy's single implicit `it`; an
        // explicit list — even the empty `{ -> … }` — is taken as written.
        let effective: Vec<String> = if params.is_empty() && !explicit_params {
            vec!["it".to_string()]
        } else {
            params.to_vec()
        };
        // Upvalues: the closure's free names that resolve to a slot in the
        // enclosing function/closure frame. At script top level there is no such
        // frame (`scope` is `None`), so a closure captures nothing and its free
        // names stay script-binding globals, exactly as before.
        //
        // At script top level there is no frame, but a **boxed** script binding
        // is still captured: the closure has to hold the cell that existed when
        // it was created, not whatever cell the name points at later. Without
        // that, a `def` inside a top-level loop would hand every closure the last
        // iteration's binding.
        let mut captures: Vec<String> = match self.scope.as_ref() {
            Some(scope) => free_vars(&effective, body)
                .into_iter()
                .filter(|n| scope.vars.contains_key(n) || self.cells.contains(n))
                .collect(),
            None => free_vars(&effective, body)
                .into_iter()
                .filter(|n| self.cells.contains(n))
                .collect(),
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
        let cell_captures: HashSet<String> = captures
            .iter()
            .filter(|n| self.cells.contains(*n))
            .cloned()
            .collect();
        self.pending_closures.push_back(PendingClosure {
            name_idx,
            params: effective.clone(),
            captures: captures.clone(),
            cell_captures,
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
        // Whether the last parameter collects the call's remaining arguments.
        // The body's prologue is the same either way — one slot per parameter —
        // so this only reaches `invoke_closure`, which decides what to bind.
        self.b.emit(Op::LoadInt(i64::from(varargs)), self.cur_line);
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
        // `printf(format, args…)` / `sprintf(format, args…)` — the script-scope
        // formatting methods, unless the script declared its own.
        if matches!(name, "printf" | "sprintf")
            && !self.fn_names.contains(name)
            && !self.is_local(name)
        {
            for a in args {
                self.expr(a)?;
            }
            let id = if name == "printf" {
                crate::host::GPRINTF
            } else {
                crate::host::GSPRINTF
            };
            self.emit_call_builtin(id, args.len() as u8, line)?;
            return Ok(());
        }
        // `use (Cat, …) { … }` — Groovy's category block. Lowered here rather
        // than as an ordinary call because its arguments are *class names*, and
        // a bare capitalised name is only a class reference in this position.
        if name == "use" && args.len() >= 2 && !self.fn_names.contains(name) {
            let (cats, body) = args.split_at(args.len() - 1);
            for a in cats {
                // A bare name here is the *class*, not a variable — a category
                // is named, never held. Lowering it as a variable read would
                // find an unbound global.
                match a {
                    Expr::Var(n) => {
                        let c = self.b.add_constant(Value::str(n.clone()));
                        self.b.emit(Op::LoadConst(c), line);
                        self.emit_call_builtin(crate::host::GCLASSREF, 0, line)?;
                    }
                    other => self.expr(other)?,
                }
            }
            self.expr(&body[0])?;
            self.emit_call_builtin(crate::host::GUSE, args.len() as u8, line)?;
            return Ok(());
        }
        // A user-defined function: push the args (left-to-right) and call through
        // the fusevm frame ABI; `Op::Call` leaves the return value on the stack.
        if self.fn_names.contains(name) {
            let nidx = self.b.add_name(name);
            // `Op::Call` binds a fixed number of stack values into the callee's
            // frame, so a spread — whose count is a run-time value — enters the
            // same subroutine through the host instead.
            if Self::has_spread(args) {
                self.emit_spread_args(args, line)?;
                self.b.emit(Op::LoadInt(nidx as i64), line);
                self.emit_call_builtin(crate::host::GCALL_SPREAD, 0, line)?;
                self.emit_exc_check(line)?;
                return Ok(());
            }
            for a in args {
                self.expr(a)?;
            }
            self.b.emit(Op::Call(nidx, args.len() as u8), line);
            self.emit_exc_check(line)?;
            return Ok(());
        }
        // Unknown callee. With a `rust { ... }` block present it may be an FFI
        // export registered at runtime, so lower to a by-name FFI dispatch: push
        // the args (deepest first), then the name, then call.
        if self.has_ffi {
            let argc = self.emit_args(args, line)?;
            let nidx = self.b.add_constant(Value::str(name.to_string()));
            self.b.emit(Op::LoadConst(nidx), line);
            self.b
                .emit(Op::CallBuiltin(crate::host::GFFI_CALL, argc), line);
            return Ok(());
        }
        // A bare call to a sibling method inside a class body is an implicit
        // `this.method(args)` (a local variable of the same name would shadow it).
        if self.is_method(name) && !self.is_local(name) {
            self.emit_this(); // this (receiver)
            let argc = self.emit_args(args, line)?;
            let midx = self.b.add_constant(Value::str(name.to_string()));
            self.b.emit(Op::LoadConst(midx), line);
            self.emit_call_builtin(crate::host::GMETHOD, argc, line)?;
            return Ok(());
        }
        // Otherwise `name(args)` is a call through a variable — a closure invoked
        // directly, `def f = { it * 2 }; f(21)`. Load the value, push the args,
        // and dispatch through the closure-call builtin, which faults with
        // `unresolved reference: name` if the value is not a closure.
        self.emit_var_read(name, line);
        let argc = self.emit_args(args, line)?;
        let nidx = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(nidx), line);
        self.emit_call_builtin(crate::host::GCLOSURE_CALL, argc, line)?;
        Ok(())
    }

    /// Lower a call's arguments and answer the argument count the call opcode
    /// should carry.
    ///
    /// Without a spread that is the arguments pushed and their count. With one,
    /// the whole list is built and parked instead and the count is zero — the
    /// builtin takes the parked list. Every call shape goes through here, so a
    /// spread reaches all of them.
    fn emit_args(&mut self, args: &[Expr], line: u32) -> Result<u8, String> {
        if Self::has_spread(args) {
            self.emit_spread_args(args, line)?;
            return Ok(0);
        }
        for a in args {
            self.expr(a)?;
        }
        Ok(args.len() as u8)
    }

    /// Does this argument list contain a `*` spread? A call that does cannot use
    /// a fixed-argument-count opcode — see [`Compiler::emit_spread_args`].
    fn has_spread(args: &[Expr]) -> bool {
        args.iter().any(|a| matches!(a, Expr::SpreadArg(_)))
    }

    /// Build a spread call's whole argument list on the stack and park it for
    /// the call opcode that follows.
    ///
    /// The list is assembled the same way a list literal with a spread is —
    /// concatenation, with `Expr::Iterable` turning each spread operand into its
    /// elements — so a range, a set and an array spread as readily as a list.
    /// The call is then emitted with an argument count of zero, and the builtin
    /// takes the parked list instead of popping the stack.
    fn emit_spread_args(&mut self, args: &[Expr], line: u32) -> Result<(), String> {
        let mut acc: Option<Expr> = None;
        let mut plain: Vec<Expr> = Vec::new();
        let flush = |acc: &mut Option<Expr>, plain: &mut Vec<Expr>| {
            if plain.is_empty() {
                return;
            }
            let chunk = Expr::List(std::mem::take(plain));
            *acc = Some(match acc.take() {
                None => chunk,
                Some(a) => concat_expr(a, chunk),
            });
        };
        for a in args {
            match a {
                Expr::SpreadArg(inner) => {
                    flush(&mut acc, &mut plain);
                    let spread = Expr::Iterable(inner.clone());
                    acc = Some(match acc.take() {
                        None => concat_expr(Expr::List(Vec::new()), spread),
                        Some(prev) => concat_expr(prev, spread),
                    });
                }
                other => plain.push(other.clone()),
            }
        }
        flush(&mut acc, &mut plain);
        let list = acc.unwrap_or_else(|| Expr::List(Vec::new()));
        self.expr(&list)?;
        self.b
            .emit(Op::CallBuiltin(crate::host::GSPREAD_ARGS, 0), line);
        self.b.emit(Op::Pop, line);
        Ok(())
    }

    /// True when `e`'s value is statically a `Long` rather than an `Integer`.
    ///
    /// Groovy's integer width is a property of the *value*, not of a declared
    /// type: `Integer op Integer` wraps at 32 bits and anything involving a
    /// `Long` wraps at 64, so `1000000 * 1000000` is `-727379968` while
    /// `1000000L * 1000000` is `1000000000000`. The host applies that rule at
    /// run time from the operands' magnitudes (see
    /// [`crate::host::sited_numeric_hook`]), which is exact for every `Long`
    /// too large to be an `Integer` — and blind to the one that is not, because
    /// `2000000000L` and `2000000000` are the same `Value::Int`.
    ///
    /// This is what closes that hole. The compiler marks the arithmetic whose
    /// operands it can *see* are `Long`, and the host trusts the mark over the
    /// magnitudes. It is what makes the `long` accumulator work — `long t = 0;
    /// t += 2000000000; t += 2000000000` is `4000000000`, where the same code
    /// on a `def` is the `Integer` `-294967296`.
    ///
    /// Unknown is narrow, which is Groovy's own default: an unsuffixed literal
    /// is an `Integer`, so a value the compiler cannot see into is far more
    /// likely to be one than a `Long`. Being wrong here costs only the wrap on
    /// an overflow the magnitude rule then catches anyway.
    fn is_wide(&self, e: &Expr) -> bool {
        match e {
            Expr::Int(_, w) => *w == IntWidth::Long,
            Expr::Var(name) => self.wide_vars.contains(name),
            Expr::Recorded { inner, .. } => self.is_wide(inner),
            // A shift takes its width from the value being shifted; the count is
            // an `Integer` either way (`1L << 3` is a `Long`, `1 << 3L` is not).
            Expr::Binary {
                op: BinOp::Shl | BinOp::Shr | BinOp::UShr,
                lhs,
                ..
            } => self.is_wide(lhs),
            Expr::Binary { lhs, rhs, .. } => self.is_wide(lhs) || self.is_wide(rhs),
            // `-2147483648` is `Integer.MIN_VALUE`, an `Integer`, even though
            // `2147483648` on its own is a `Long`: the minimum of a
            // two's-complement type is only writable as a negated literal, so
            // Java and Groovy read the sign as part of it. That is why
            // `-2147483648 - 1` is `2147483647`.
            Expr::Unary { op: UnOp::Neg, rhs } => match &**rhs {
                // Only the literal that is a `Long` *by magnitude alone* and
                // negates back into `Integer` range — `2147483648` — reads this
                // way. An `L` suffix says `Long` outright, so `-8L` stays one.
                Expr::Int(n, w) => {
                    *w == IntWidth::Long
                        && !(i32::try_from(*n).is_err() && i32::try_from(n.wrapping_neg()).is_ok())
                }
                other => self.is_wide(other),
            },
            Expr::Unary { rhs, .. } => self.is_wide(rhs),
            Expr::Ternary { then, els, .. } => self.is_wide(then) || self.is_wide(els),
            Expr::Elvis { lhs, rhs } => self.is_wide(lhs) || self.is_wide(rhs),
            // `Long.MAX_VALUE` / `Long.MIN_VALUE`, and the casts and conversions
            // that name the type outright.
            Expr::Property { recv, name, .. } => {
                matches!(&**recv, Expr::Var(v) if v == "Long")
                    && matches!(name.as_str(), "MAX_VALUE" | "MIN_VALUE")
            }
            // `Long.valueOf(5)` / `Long.parseLong("5")` name the type outright,
            // so their result is a `Long` however small it is.
            Expr::MethodCall { recv, method, .. } => {
                matches!(
                    method.as_str(),
                    "longValue" | "toLong" | "currentTimeMillis"
                ) || (matches!(&**recv, Expr::Var(v) if v == "Long")
                    && matches!(method.as_str(), "valueOf" | "parseLong"))
            }
            // A call to a callable whose every return is statically a `Long`.
            Expr::Call { name, .. } => self.wide_returns.contains(name),
            Expr::CallValue { callee, .. } => {
                matches!(&**callee, Expr::Var(f) if self.wide_returns.contains(f))
            }
            Expr::Cast { ty, .. } => matches!(ty.as_str(), "long" | "Long" | "BigInteger"),
            _ => false,
        }
    }

    /// Sign-extend the low 32 bits of the value on top of the stack — the
    /// `Integer` an `int`-width operation answers with. `Op::Shr` is an
    /// arithmetic shift in the interpreter and in the Cranelift backend
    /// (`sshr`), so this is two native ops the tracing JIT records rather than a
    /// builtin call, which would abort the trace.
    fn emit_wrap32(&mut self, line: u32) {
        self.b.emit(Op::LoadInt(32), line);
        self.b.emit(Op::Shl, line);
        self.b.emit(Op::LoadInt(32), line);
        self.b.emit(Op::Shr, line);
    }

    /// Which positions of a `recv.method(args)` call the compiler can see are
    /// statically `Long`: bit 0 the receiver, bit `k+1` argument `k`.
    ///
    /// Zero means "no `Long` visible here", not "everything is an `Integer`" —
    /// [`Compiler::is_wide`] is a conservative static reading, and the host
    /// treats a clear bit the same cautious way. Positions past the seventh
    /// argument do not fit the mask byte and stay unmarked; no Java overload
    /// groovyrs models discriminates that far out.
    fn call_width_mask(&self, recv: &Expr, args: &[Expr]) -> u8 {
        let mut mask = u8::from(self.is_wide(recv));
        for (i, a) in args.iter().enumerate().take(7) {
            if self.is_wide(a) {
                mask |= 1 << (i + 1);
            }
        }
        mask
    }

    /// Record that the op just emitted at `pos` is `Long` arithmetic, so the
    /// host wraps its overflow at 64 bits rather than 32. See
    /// [`Compiler::is_wide`].
    fn mark_wide_site(&mut self, pos: usize, lhs: &Expr, rhs: &Expr) {
        if self.is_wide(lhs) || self.is_wide(rhs) {
            self.wide_sites.insert(pos);
        }
    }

    /// Note the width of a declaration's variable. A `long`/`Long` declaration
    /// says so outright and *pins* the width, which is how `long t = 0`
    /// accumulates at 64 bits even after a plain `t = 5`; a `def` takes the
    /// width of its initializer, and re-binds the name either way — a
    /// declaration is a fresh variable, so `def a = 2000000000` after an earlier
    /// `def a = 5L` is an `Integer` again.
    fn note_var_width(&mut self, ty: &str, name: &str, init: Option<&Expr>) {
        // A declaration re-binds the name's `>>` receiver shape too, and does so
        // whatever its width turns out to be.
        self.set_var_obj(ty, name, init);
        if matches!(ty, "long" | "Long" | "BigInteger") {
            self.wide_vars.insert(name.to_string());
            self.pinned_wide.insert(name.to_string());
            return;
        }
        self.pinned_wide.remove(name);
        self.set_var_width(name, init.is_some_and(|e| self.is_wide(e)));
        // `def f = { -> 5L }` binds a callable, not a number: record what
        // *calling* it yields so `f()` has a width at the call site.
        self.wide_returns.remove(name);
        if let Some(Expr::Closure { body, .. }) = init {
            if self.body_returns_wide(body) {
                self.wide_returns.insert(name.to_string());
            }
        }
    }

    /// Record whether `name` now holds a closure or a class instance — the two
    /// receivers whose `>>` is not a bit shift. A declared `Closure c` says so
    /// outright; otherwise the initializer's static shape decides, and a
    /// re-binding to anything else clears the name again (`def f = { it }; f = 8`
    /// leaves `f >> 1` on the native shift). See [`Compiler::obj_vars`].
    fn set_var_obj(&mut self, ty: &str, name: &str, init: Option<&Expr>) {
        // `bit_operand_is_object`, not `shr_receiver_is_object`: a name bound to
        // a decimal (`def a = 1G`) is also a handle, and `a & 3G` / `a >> 1`
        // read it as `0` on the native lowering.
        let is_obj = matches!(ty, "Closure") || init.is_some_and(|e| self.bit_operand_is_object(e));
        if is_obj {
            self.obj_vars.insert(name.to_string());
        } else {
            self.obj_vars.remove(name);
        }
    }

    /// Can the compiler see that this expression's value is a closure or a class
    /// instance — a receiver whose `>>` is `Closure.andThen` or a `rightShift`
    /// overload rather than a bit shift?
    ///
    /// Conservative in the safe direction: `false` only means "lower `>>` to the
    /// native shift", and [`crate::host::b_shr`] still performs that same shift
    /// when a `true` turns out to be wrong. So a miss costs an answer, a false
    /// positive costs only the trace on that one site.
    fn shr_receiver_is_object(&self, e: &Expr) -> bool {
        match e {
            Expr::Closure { .. } | Expr::New { .. } => true,
            Expr::Var(name) => self.obj_vars.contains(name),
            Expr::Recorded { inner, .. } => self.shr_receiver_is_object(inner),
            // A composition is itself a closure, so `f >> g >> h` chains.
            Expr::Binary {
                op: BinOp::Shr | BinOp::Shl,
                lhs,
                ..
            } => self.shr_receiver_is_object(lhs),
            Expr::Ternary { then, els, .. } => {
                self.shr_receiver_is_object(then) || self.shr_receiver_is_object(els)
            }
            Expr::Elvis { lhs, rhs } => {
                self.shr_receiver_is_object(lhs) || self.shr_receiver_is_object(rhs)
            }
            _ => false,
        }
    }

    /// Can the compiler see that this expression's value may ride a heap handle
    /// rather than being a plain `Value::Int` — the operands fusevm's native
    /// bitwise ops read as `0`?
    ///
    /// Conservative in the same direction as [`Compiler::shr_receiver_is_object`]
    /// and for the same reason: a `true` still gets the right answer (the
    /// builtin performs the ordinary integer operation when the guess was
    /// wrong), it only costs that one site its native lowering. A `false` on a
    /// decimal that the compiler could not see — `def a = f(); def b = g();
    /// a & b`, where neither side names a decimal — keeps the native op and
    /// still answers `0`; BUGS.md carries that residue.
    fn bit_operand_is_object(&self, e: &Expr) -> bool {
        match e {
            // Every `G` literal and every unsuffixed decimal is a handle.
            Expr::Dec(_) | Expr::BigInt(_) => true,
            Expr::Var(name) => self.obj_vars.contains(name),
            Expr::Recorded { inner, .. } => self.bit_operand_is_object(inner),
            Expr::Unary { rhs, .. } => self.bit_operand_is_object(rhs),
            Expr::Binary { lhs, rhs, .. } => {
                self.bit_operand_is_object(lhs) || self.bit_operand_is_object(rhs)
            }
            Expr::Ternary { then, els, .. } => {
                self.bit_operand_is_object(then) || self.bit_operand_is_object(els)
            }
            Expr::Elvis { lhs, rhs } => {
                self.bit_operand_is_object(lhs) || self.bit_operand_is_object(rhs)
            }
            other => self.shr_receiver_is_object(other),
        }
    }

    /// Record `name`'s width at this point in the flow. A pinned declaration
    /// (`long x`) keeps its declared width whatever is assigned to it.
    fn set_var_width(&mut self, name: &str, wide: bool) {
        if self.pinned_wide.contains(name) {
            return;
        }
        if wide {
            self.wide_vars.insert(name.to_string());
        } else {
            self.wide_vars.remove(name);
        }
    }

    /// Lower a statement that encloses a nested body (`if`, the loops, `switch`,
    /// `try`), merging the widths its branches produced.
    ///
    /// A name widened on *any* path is wide afterwards, and a name narrowed on
    /// one path only is not — the compiler cannot know which branch ran, and
    /// `Integer` is the default that a magnitude check still catches when the
    /// guess is wrong (see [`Compiler::is_wide`]). Union is therefore the merge,
    /// which also keeps `def a = 5; if (c) { a = 5L }; a * b` at 64 bits.
    fn branch_stmt(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> Result<(), String> {
        let before = self.wide_vars.clone();
        let r = f(self);
        self.wide_vars.extend(before);
        r
    }

    fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> Result<(), String> {
        // A compound assignment (`x <<= n`) has already pushed the left
        // operand's value. Taken here rather than read below, so the flag cannot
        // reach a nested operand's own `binary` call.
        let pre = std::mem::take(&mut self.preloaded_lhs);
        // `&&` / `||` short-circuit and evaluate to a `Boolean` (Groovy's
        // logical operators are boolean-valued: `5 && 3` is `true`, `0 || 7` is
        // `true`). Both operands lower through `bool_expr`, so the kept deciding
        // value is already the `Boolean` result.
        match op {
            BinOp::And => {
                self.bool_expr(lhs)?;
                let jf = self.b.emit(Op::JumpIfFalseKeep(0), self.cur_line);
                self.b.emit(Op::Pop, self.cur_line);
                self.bool_expr(rhs)?;
                let end = self.b.current_pos();
                self.b.patch_jump(jf, end);
                return Ok(());
            }
            BinOp::Or => {
                self.bool_expr(lhs)?;
                let jt = self.b.emit(Op::JumpIfTrueKeep(0), self.cur_line);
                self.b.emit(Op::Pop, self.cur_line);
                self.bool_expr(rhs)?;
                let end = self.b.current_pos();
                self.b.patch_jump(jt, end);
                return Ok(());
            }
            _ => {}
        }
        // `>>` on an `Integer` is a 32-bit arithmetic shift whose count is
        // masked to 5 bits, where fusevm's `Op::Shr` is 64-bit with the count
        // masked to 6: `1 >> 32` is `1` in Groovy and `0` natively, and
        // `256 >> 33` is `128`. Sign-extending the value to 32 bits and masking
        // the count to 5 restores Groovy's answer in native ops the tracing JIT
        // still records — a builtin here would cost every shifting loop its
        // trace. A `Long` shift is already exactly what `Op::Shr` does.
        // …but `>>` is only a shift on a *number*. On a closure it is
        // `Closure.andThen` (forward composition) and on a class instance a
        // `rightShift` overload, neither of which the native ops can express —
        // and a `Value::Obj` handle silently coerces to an integer there, so
        // `f >> g` answered a number instead of a composed closure. Those two
        // receivers are exactly what `shr_receiver_is_object` spots, so they go
        // to the builtin and every other `>>` keeps its native lowering.
        if matches!(op, BinOp::Shr) && self.shr_receiver_is_object(lhs) {
            self.emit_binary_lhs(pre, lhs)?;
            self.expr(rhs)?;
            // The width rides along as `GSHL`/`GUSHR` take it, for the numeric
            // fallback a mis-typed receiver lands on.
            let wide = self.is_wide(lhs);
            self.b.emit(Op::LoadInt(i64::from(wide)), self.cur_line);
            self.emit_call_builtin(crate::host::GSHR, 3, self.cur_line)?;
            return Ok(());
        }
        // `&`/`|`/`^` where an operand may be a `BigInteger`. fusevm's native
        // `Op::BitAnd` reads its operands with `Value::to_int`, which answers
        // `0` for the `Value::Obj` a decimal rides — so `1G & 3G` evaluated to
        // `0`, silently. `NumOp` has no bitwise member for the numeric hook to
        // carry, so those operands take the [`crate::host::GBITOP`] builtin the
        // same way an object-receiver `>>` takes `GSHR`; `int & int` keeps its
        // native lowering and its JIT trace.
        if matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor)
            && (self.bit_operand_is_object(lhs) || self.bit_operand_is_object(rhs))
        {
            self.emit_binary_lhs(pre, lhs)?;
            self.expr(rhs)?;
            let name = match op {
                BinOp::BitAnd => "and",
                BinOp::BitOr => "or",
                _ => "xor",
            };
            let idx = self.b.add_constant(Value::str(name.to_string()));
            self.b.emit(Op::LoadConst(idx), self.cur_line);
            self.emit_call_builtin(crate::host::GBITOP, 3, self.cur_line)?;
            return Ok(());
        }
        // A `>>` whose left operand may be a `BigInteger` needs the builtin for
        // the same reason: the native shift reads the handle as `0`.
        if matches!(op, BinOp::Shr) && self.bit_operand_is_object(lhs) {
            self.emit_binary_lhs(pre, lhs)?;
            self.expr(rhs)?;
            self.b
                .emit(Op::LoadInt(i64::from(self.is_wide(lhs))), self.cur_line);
            self.emit_call_builtin(crate::host::GSHR, 3, self.cur_line)?;
            return Ok(());
        }
        if matches!(op, BinOp::Shr) && !self.is_wide(lhs) {
            self.emit_binary_lhs(pre, lhs)?;
            self.emit_wrap32(self.cur_line);
            self.expr(rhs)?;
            self.b.emit(Op::LoadInt(31), self.cur_line);
            self.b.emit(Op::BitAnd, self.cur_line);
            self.b.emit(Op::Shr, self.cur_line);
            return Ok(());
        }
        self.emit_binary_lhs(pre, lhs)?;
        self.expr(rhs)?;
        // Groovy `/` is not a native op — it lowers to the GDIV builtin so
        // integer division promotes to a decimal (`7/2 → 3.5`).
        if let BinOp::Div = op {
            self.emit_call_builtin(crate::host::GDIV, 2, self.cur_line)?;
            return Ok(());
        }
        // Groovy `<=>` is not a native op — it lowers to the GCMP builtin, which
        // dispatches a user `compareTo` on an instance operand or yields the
        // primitive sign (`-1`/`0`/`1`).
        if let BinOp::Cmp = op {
            self.emit_call_builtin(crate::host::GCMP, 2, self.cur_line)?;
            return Ok(());
        }
        // Groovy `%` raises `ArithmeticException` on a zero divisor, which
        // fusevm's native `Op::Mod` answers with `0`. The guard branches to the
        // `GMOD` builtin only when the divisor really is zero.
        if let BinOp::Mod = op {
            return self.emit_mod(rhs, self.cur_line);
        }
        // The operators whose result type is a *runtime* question route through
        // a builtin: `**` follows the numeric tower, `<<` is `leftShift` (a
        // shift, an append or a concatenation), `>>>` fills to the operand's
        // Java width, and `in` is the collection's membership test.
        let builtin = match op {
            BinOp::Power => Some(crate::host::GPOWER),
            BinOp::Shl => Some(crate::host::GSHL),
            BinOp::UShr => Some(crate::host::GUSHR),
            BinOp::In => Some(crate::host::GIN),
            // `=~` builds a `Matcher`, `==~` answers a `Boolean`.
            BinOp::Match => Some(crate::host::GMATCH),
            BinOp::MatchFull => Some(crate::host::GMATCH_FULL),
            _ => None,
        };
        if let Some(id) = builtin {
            // `<<` and `>>>` shift at the *left* operand's Java width, and the
            // count is masked to that width's bit index — `1 << 32` is `1`, and
            // `1L << 32` is `4294967296`. `**` narrows its result to that same
            // width (`2 ** 40` is a `BigInteger`, `2L ** 40` a `Long`). The host
            // reads the width from the operand's magnitude, which cannot tell
            // `1L` from `1`, so the statically-known width rides along as a
            // third argument.
            if matches!(op, BinOp::Shl | BinOp::UShr | BinOp::Power) {
                let wide = self.is_wide(lhs);
                self.b.emit(Op::LoadInt(i64::from(wide)), self.cur_line);
                self.emit_call_builtin(id, 3, self.cur_line)?;
            } else {
                self.emit_call_builtin(id, 2, self.cur_line)?;
            }
            // `list << x` appends in place, so it writes back like `list.add`.
            // Not for `list <<= x`: the assignment stores the result into the
            // target itself, and writing back here as well would append twice.
            if matches!(op, BinOp::Shl) && !pre {
                self.emit_receiver_writeback(lhs, "leftShift", &[])?;
            }
            return Ok(());
        }
        let vop = match op {
            BinOp::Add => Op::Add,
            BinOp::Sub => Op::Sub,
            BinOp::Mul => Op::Mul,
            BinOp::Mod => unreachable!("handled above"),
            BinOp::Eq => Op::NumEq,
            BinOp::Ne => Op::NumNe,
            BinOp::Lt => Op::NumLt,
            BinOp::Gt => Op::NumGt,
            BinOp::Le => Op::NumLe,
            BinOp::Ge => Op::NumGe,
            BinOp::BitAnd => Op::BitAnd,
            BinOp::BitOr => Op::BitOr,
            BinOp::BitXor => Op::BitXor,
            BinOp::Shr => Op::Shr,
            BinOp::Div | BinOp::Cmp => unreachable!("handled above"),
            BinOp::And | BinOp::Or => unreachable!("handled above"),
            BinOp::Power
            | BinOp::Shl
            | BinOp::UShr
            | BinOp::In
            | BinOp::Match
            | BinOp::MatchFull => {
                unreachable!("routed to a builtin above")
            }
        };
        let pos = self.b.emit(vop, self.cur_line);
        // `+`/`-`/`*` are the ops that overflow, and the host decides the width
        // their overflow wraps at. Tell it where the `Long` ones are.
        if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) {
            self.mark_wide_site(pos, lhs, rhs);
        }
        // A user-class operand routes this op through the strict numeric hook,
        // which re-enters the VM to run an operator-overload method — the one
        // native op that can leave an exception in flight. Only a program that
        // both uses exceptions and declares a class pays the check.
        if self.exc_after_arith {
            self.emit_exc_check(self.cur_line)?;
        }
        Ok(())
    }

    /// Write a self-mutating GDK result back to a variable receiver.
    ///
    /// Groovy's `List.sort()` and `List.unique()` sort/dedupe the receiver **in
    /// place** and return it, so `l.sort(); println(l)` prints the sorted list.
    /// A fusevm `Value::Array` is a value, not a reference, so the host can only
    /// return a new list — this stores that result back over a bare-variable
    /// receiver, which reproduces Groovy for the shape scripts actually write.
    /// The call's own value stays on the stack, so the expression is unchanged.
    ///
    /// Whether the receiver is a list is a *runtime* question (`Map.sort()`
    /// returns a new map and does **not** mutate), so the receiver is re-read
    /// and [`crate::host::GWRITEBACK`] picks between the result and the
    /// unchanged receiver:
    ///
    /// ```text
    ///   <call>            ; [result]
    ///   Dup               ; [result, result]
    ///   <load recv>       ; [result, result, receiver]
    ///   GWRITEBACK        ; [result, value-to-store]
    ///   <store recv>      ; [result]
    /// ```
    ///
    /// `sort(false)` explicitly asks for a copy, so only the no-argument, the
    /// `sort(true)`, and the closure-argument forms write back at all.
    ///
    /// The `List` mutators (`add`, `remove`, `<<`, …) travel the same path, but
    /// their *result* is not the new list (`add` answers `true`), so they park
    /// the new contents in the host's `MUTATED` slot and `GWRITEBACK` prefers
    /// those over the result.
    fn emit_receiver_writeback(
        &mut self,
        recv: &Expr,
        method: &str,
        args: &[Expr],
    ) -> Result<(), String> {
        let mutating = (matches!(method, "sort" | "unique")
            && args
                .iter()
                .all(|a| matches!(a, Expr::Closure { .. } | Expr::Bool(true))))
            // `reverse(true)` reverses the receiver in place; the no-argument
            // form copies, so unlike `sort` only the explicit `true` writes back.
            || (method == "reverse" && matches!(args, [Expr::Bool(true)]))
            || matches!(
                method,
                "add"
                    | "addAll"
                    | "remove"
                    | "removeAt"
                    | "removeElement"
                    | "removeAll"
                    | "retainAll"
                    | "set"
                    | "putAt"
                    | "clear"
                    | "push"
                    | "pop"
                    | "removeLast"
                    | "swap"
                    | "leftShift"
            );
        let Expr::Var(name) = recv else {
            return Ok(());
        };
        // A bare field name inside a method is `this.field`, not a variable —
        // leave that to the (unsupported) property path rather than inventing a
        // local of the same name.
        if !mutating || self.is_field(name) {
            return Ok(());
        }
        self.b.emit(Op::Dup, self.cur_line);
        // The receiver was *read* through the delegate when only a delegate
        // binds the name, so the new contents have to go back the same way —
        // writing them to a script binding instead would leave the delegate
        // holding the pre-mutation list.
        self.emit_name_load(name, self.cur_line)?;
        self.b
            .emit(Op::CallBuiltin(crate::host::GWRITEBACK, 2), self.cur_line);
        self.emit_name_store(name, self.cur_line)
    }

    /// Lower Groovy `%` for two operands already on the stack.
    ///
    /// Java's `%` throws `ArithmeticException` on a zero divisor where fusevm's
    /// native `Op::Mod` yields `0` — a silent wrong answer. Rather than route
    /// every `%` through a builtin (which would cost the native op and its JIT
    /// trace, the way `/` pays for `GDIV`), the divisor is tested against zero
    /// with native ops and only the zero branch calls [`crate::host::GMOD`]:
    ///
    /// ```text
    ///   Dup; LoadInt(0); NumEq; JumpIfFalse native
    ///   GMOD(a, b)                 ; raises, or answers NaN for a double
    ///   Jump end
    /// native:
    ///   Op::Mod
    /// end:
    /// ```
    ///
    /// A **literal non-zero divisor** is proved safe at compile time and emits
    /// the bare `Op::Mod`, so `i % 2` in a loop is byte-identical to before.
    fn emit_mod(&mut self, divisor: &Expr, line: u32) -> Result<(), String> {
        if is_nonzero_literal(divisor) {
            self.b.emit(Op::Mod, line);
            if self.exc_after_arith {
                self.emit_exc_check(line)?;
            }
            return Ok(());
        }
        self.b.emit(Op::Dup, line);
        self.b.emit(Op::LoadInt(0), line);
        self.b.emit(Op::NumEq, line);
        let to_native = self.b.emit(Op::JumpIfFalse(0), line);
        self.emit_call_builtin(crate::host::GMOD, 2, line)?;
        let to_end = self.b.emit(Op::Jump(0), line);
        let native = self.b.current_pos();
        self.b.patch_jump(to_native, native);
        self.b.emit(Op::Mod, line);
        if self.exc_after_arith {
            self.emit_exc_check(line)?;
        }
        let end = self.b.current_pos();
        self.b.patch_jump(to_end, end);
        Ok(())
    }
}

/// Is this expression a numeric literal the compiler can prove is not zero? Used
/// to elide the `%` zero-divisor guard for the constant-divisor shapes (`i % 2`,
/// `n % -3`) that make up every hot loop.
fn is_nonzero_literal(e: &Expr) -> bool {
    match e {
        Expr::Int(n, _) => *n != 0,
        Expr::Float(f) => *f != 0.0,
        Expr::Unary { op: UnOp::Neg, rhs } => is_nonzero_literal(rhs),
        _ => false,
    }
}

// ── Static condition typing (Groovy truthiness) ─────────────────────────────

/// Does a condition of this shape need the Groovy-truthiness builtin?
///
/// `false` means the expression's runtime value is guaranteed to be one of the
/// fusevm variants whose native `is_truthy` already matches Groovy — `Int`,
/// `Float`, `Bool`, `Undef` (`null`), `Array` (a list), `Hash`. Those conditions
/// emit no builtin at all, so `while (i < n)` / `for (…; i < n; …)` keep the
/// native `NumLt`+`JumpIfFalse` pair and stay JIT-traceable.
///
/// `true` (the conservative default) covers the two families fusevm reads
/// differently from Groovy:
///
/// * a `Value::Obj` heap handle — a `BigDecimal` (so `if (0.0)` is false),
///   an ordered map (`if ([:])`), a closure, a class instance (`asBoolean()`);
/// * a `String`, which fusevm reads shell-style, making `"0"` false where
///   Groovy makes every non-empty string true.
fn needs_truth(e: &Expr) -> bool {
    match e {
        // Statically a number / boolean / null.
        Expr::Int(..) | Expr::Float(_) | Expr::Bool(_) | Expr::Null => false,
        // A list literal is a host handle now that lists alias, and every
        // `Value::Obj` is true to fusevm — so `if ([])` needs the builtin to
        // report the empty list as false. (It was a `Value::Array`, whose truth
        // fusevm already read the way Groovy does, when this answered `false`.)
        Expr::List(_) => true,
        // Comparisons and `instanceof` yield a `Boolean`; `<=>` an `Integer`;
        // `&&`/`||` are boolean-valued in Groovy (see `Compiler::binary`).
        Expr::Binary {
            op:
                BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Gt
                | BinOp::Le
                | BinOp::Ge
                | BinOp::Cmp
                | BinOp::And
                | BinOp::Or,
            ..
        } => false,
        // `!x` is a `Boolean`; unary `-` can be a decimal.
        Expr::Unary { op: UnOp::Not, .. } => false,
        Expr::InstanceOf { .. } => false,
        // A conditional yields one of its arms.
        Expr::Ternary { then, els, .. } => needs_truth(then) || needs_truth(els),
        Expr::Elvis { lhs, rhs } => needs_truth(lhs) || needs_truth(rhs),
        // Everything else — a variable, call, property, arithmetic, string,
        // decimal literal, map/closure literal, `new` — could be a handle.
        _ => true,
    }
}

/// Is this expression's value statically a `Boolean`? Used by `&&`/`||`, whose
/// Groovy result is a `Boolean` rather than the deciding operand — an operand
/// that already is one needs no conversion, so `i < n && j < m` compiles to the
/// same native ops it did before.
fn is_static_bool(e: &Expr) -> bool {
    match e {
        Expr::Bool(_) => true,
        Expr::Unary { op: UnOp::Not, .. } => true,
        Expr::InstanceOf { .. } => true,
        Expr::Binary { op, .. } => matches!(
            op,
            BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Gt
                | BinOp::Le
                | BinOp::Ge
                | BinOp::And
                | BinOp::Or
        ),
        Expr::Ternary { then, els, .. } => is_static_bool(then) && is_static_bool(els),
        Expr::Elvis { lhs, rhs } => is_static_bool(lhs) && is_static_bool(rhs),
        _ => false,
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
    walk_free(params, body).out
}

/// The walk's accumulators. `out`/`seen` build the free-variable list;
/// `depth`/`closure_bound`/`captured` answer the *other* question the same walk
/// can settle — which of this scope's own names a nested closure captures, which
/// is the set that has to be boxed (see [`crate::host::GCELL_NEW`]).
#[derive(Default)]
struct FreeCtx {
    out: Vec<String>,
    seen: HashSet<String>,
    /// Closure-literal nesting depth at the current reference.
    depth: usize,
    /// One entry per enclosing closure literal: the names it binds itself.
    closure_bound: Vec<HashSet<String>>,
    /// Every name referenced inside some closure that the closure does not bind.
    captured: HashSet<String>,
}

/// Run the free-variable walk over a scope's body and hand back both answers.
fn walk_free(params: &[String], body: &[Stmt]) -> FreeCtx {
    let mut bound: HashSet<String> = params.iter().cloned().collect();
    collect_bound_stmts(body, &mut bound);
    let mut cx = FreeCtx::default();
    for s in body {
        free_in_stmt(s, &bound, &mut cx);
    }
    cx
}

/// The names a scope *owns* — its parameters and the locals its body declares —
/// that some closure written inside it captures.
///
/// These are the ones the compiler boxes. Groovy compiles exactly this set into
/// `groovy.lang.Reference` cells, and that is what gives a closure the variable
/// rather than a copy of its value, and what makes a declaration inside a loop
/// body a fresh variable per iteration. Boxing is confined to this set so a loop
/// with no closure in it keeps its plain slot reads and its JIT trace.
fn boxed_names(params: &[String], body: &[Stmt]) -> HashSet<String> {
    let cx = walk_free(params, body);
    let mut own: HashSet<String> = params.iter().cloned().collect();
    collect_bound_stmts(body, &mut own);
    // `this` is the receiver in slot 0, never a variable a cell could hold.
    own.remove("this");
    cx.captured
        .into_iter()
        .filter(|n| own.contains(n))
        .collect()
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
            StmtKind::While { body, .. } | StmtKind::DoWhile { body, .. } => {
                collect_bound_stmts(body, bound)
            }
            StmtKind::Switch { cases, .. } => {
                for c in cases {
                    collect_bound_stmts(&c.body, bound);
                }
            }
            StmtKind::Labeled { stmt, .. } => {
                collect_bound_stmts(std::slice::from_ref(stmt), bound)
            }
            StmtKind::Try {
                body,
                catches,
                finally_body,
            } => {
                collect_bound_stmts(body, bound);
                collect_bound_stmts(finally_body, bound);
                for arm in catches {
                    bound.insert(arm.name.clone());
                    collect_bound_stmts(&arm.body, bound);
                }
            }
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
fn note_free(name: &str, bound: &HashSet<String>, cx: &mut FreeCtx) {
    // A reference made *inside* a closure to a name that closure does not bind
    // itself is a capture: the enclosing scope owns the variable and the closure
    // reads it through the handle. That is the set the compiler has to box.
    if cx.depth > 0 && !cx.closure_bound.iter().any(|b| b.contains(name)) {
        cx.captured.insert(name.to_string());
    }
    if !bound.contains(name) && cx.seen.insert(name.to_string()) {
        cx.out.push(name.to_string());
    }
}

fn free_in_stmt(s: &Stmt, bound: &HashSet<String>, cx: &mut FreeCtx) {
    match &s.kind {
        StmtKind::Local { init, .. } => {
            if let Some(e) = init {
                free_in_expr(e, bound, cx);
            }
        }
        StmtKind::Assign { name, value, .. } => {
            note_free(name, bound, cx);
            free_in_expr(value, bound, cx);
        }
        StmtKind::Expr(e) => free_in_expr(e, bound, cx),
        StmtKind::If { cond, then, els } => {
            free_in_expr(cond, bound, cx);
            for s in then {
                free_in_stmt(s, bound, cx);
            }
            for s in els {
                free_in_stmt(s, bound, cx);
            }
        }
        StmtKind::While { cond, body } | StmtKind::DoWhile { body, cond } => {
            free_in_expr(cond, bound, cx);
            for s in body {
                free_in_stmt(s, bound, cx);
            }
        }
        StmtKind::Switch { subject, cases } => {
            free_in_expr(subject, bound, cx);
            for c in cases {
                if let Some(l) = &c.label {
                    free_in_expr(l, bound, cx);
                }
                for s in &c.body {
                    free_in_stmt(s, bound, cx);
                }
            }
        }
        StmtKind::Labeled { stmt, .. } => free_in_stmt(stmt, bound, cx),
        StmtKind::For {
            init,
            cond,
            update,
            body,
        } => {
            if let Some(i) = init {
                free_in_stmt(i, bound, cx);
            }
            if let Some(c) = cond {
                free_in_expr(c, bound, cx);
            }
            if let Some(u) = update {
                free_in_stmt(u, bound, cx);
            }
            for s in body {
                free_in_stmt(s, bound, cx);
            }
        }
        StmtKind::Return { value } => {
            if let Some(e) = value {
                free_in_expr(e, bound, cx);
            }
        }
        StmtKind::SetProperty { recv, value, .. } => {
            free_in_expr(recv, bound, cx);
            free_in_expr(value, bound, cx);
        }
        StmtKind::SetIndex {
            recv, index, value, ..
        } => {
            free_in_expr(recv, bound, cx);
            free_in_expr(index, bound, cx);
            free_in_expr(value, bound, cx);
        }
        StmtKind::Try {
            body,
            catches,
            finally_body,
        } => {
            for s in body.iter().chain(finally_body) {
                free_in_stmt(s, bound, cx);
            }
            for arm in catches {
                // The caught binding is local to its arm.
                let mut inner = bound.clone();
                inner.insert(arm.name.clone());
                collect_bound_stmts(&arm.body, &mut inner);
                for s in &arm.body {
                    free_in_stmt(s, &inner, cx);
                }
            }
        }
        StmtKind::Throw(e) => free_in_expr(e, bound, cx),
        StmtKind::Assert { cond, message, .. } => {
            free_in_expr(cond, bound, cx);
            if let Some(m) = message {
                free_in_expr(m, bound, cx);
            }
        }
        StmtKind::Break(_)
        | StmtKind::Continue(_)
        | StmtKind::Function { .. }
        | StmtKind::Class { .. } => {}
    }
}

/// `lhs + rhs` — list concatenation, used to assemble a spread call's argument
/// list. The same operator the parser desugars a literal's spread to.
fn concat_expr(lhs: Expr, rhs: Expr) -> Expr {
    Expr::Binary {
        op: BinOp::Add,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    }
}

fn free_in_expr(e: &Expr, bound: &HashSet<String>, cx: &mut FreeCtx) {
    match e {
        Expr::Regex(_) => {}
        Expr::Iterable(inner) | Expr::SpreadArg(inner) => free_in_expr(inner, bound, cx),
        Expr::Recorded { inner, .. } => free_in_expr(inner, bound, cx),
        Expr::Var(n) => note_free(n, bound, cx),
        Expr::Cast { value, .. } => free_in_expr(value, bound, cx),
        Expr::PostIncDec { name, .. } | Expr::PreIncDec { name, .. } => note_free(name, bound, cx),
        Expr::Unary { rhs, .. } => free_in_expr(rhs, bound, cx),
        Expr::Binary { lhs, rhs, .. } => {
            free_in_expr(lhs, bound, cx);
            free_in_expr(rhs, bound, cx);
        }
        Expr::Println { arg, .. } => {
            if let Some(a) = arg {
                free_in_expr(a, bound, cx);
            }
        }
        Expr::Call { name, args, .. } => {
            // The callee may be an enclosing-scope closure variable, so it is a
            // free reference too (a global function name simply won't intersect
            // the enclosing frame's slots and so is never captured).
            note_free(name, bound, cx);
            for a in args {
                free_in_expr(a, bound, cx);
            }
        }
        Expr::CallValue { callee, args, .. } => {
            free_in_expr(callee, bound, cx);
            for a in args {
                free_in_expr(a, bound, cx);
            }
        }
        Expr::List(elems) => {
            for e in elems {
                free_in_expr(e, bound, cx);
            }
        }
        Expr::Map(entries) => {
            for (k, v) in entries {
                free_in_expr(k, bound, cx);
                free_in_expr(v, bound, cx);
            }
        }
        Expr::MethodCall { recv, args, .. } => {
            free_in_expr(recv, bound, cx);
            for a in args {
                free_in_expr(a, bound, cx);
            }
        }
        Expr::Property { recv, .. } => free_in_expr(recv, bound, cx),
        Expr::Index { recv, index, .. } => {
            free_in_expr(recv, bound, cx);
            free_in_expr(index, bound, cx);
        }
        Expr::New { args, .. } => {
            for a in args {
                free_in_expr(a, bound, cx);
            }
        }
        Expr::Closure {
            params,
            body,
            explicit_params,
            ..
        } => {
            // Descend with the nested closure's own bindings added, so a name
            // free in the inner closure but not bound here still surfaces.
            let mut inner = bound.clone();
            let mut own: HashSet<String> = HashSet::new();
            if params.is_empty() && !*explicit_params {
                own.insert("it".to_string());
            }
            for p in params {
                own.insert(p.clone());
            }
            collect_bound_stmts(body, &mut own);
            inner.extend(own.iter().cloned());
            cx.closure_bound.push(own);
            cx.depth += 1;
            for s in body {
                free_in_stmt(s, &inner, cx);
            }
            cx.depth -= 1;
            cx.closure_bound.pop();
        }
        Expr::Range { start, end, .. } => {
            free_in_expr(start, bound, cx);
            free_in_expr(end, bound, cx);
        }
        Expr::GString(parts) => {
            for p in parts {
                if let GStringPart::Expr(e) = p {
                    free_in_expr(e, bound, cx);
                }
            }
        }
        Expr::Ternary { cond, then, els } => {
            free_in_expr(cond, bound, cx);
            free_in_expr(then, bound, cx);
            free_in_expr(els, bound, cx);
        }
        Expr::Elvis { lhs, rhs } => {
            free_in_expr(lhs, bound, cx);
            free_in_expr(rhs, bound, cx);
        }
        // `this`/`super` are captured upvalues when a closure inside a method
        // uses them (both resolve to the receiver instance in slot 0).
        Expr::This | Expr::Super => note_free("this", bound, cx),
        Expr::SuperCtor { args, .. } => {
            note_free("this", bound, cx);
            for a in args {
                free_in_expr(a, bound, cx);
            }
        }
        Expr::InstanceOf { value, .. } => free_in_expr(value, bound, cx),
        Expr::Int(..)
        | Expr::Float(_)
        | Expr::Dec(_)
        | Expr::BigInt(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Null => {}
    }
}

// ── Implicit return through a trailing `if` / `try` ─────────────────────────

/// Rewrite a body's trailing value expression into an explicit `return`,
/// descending into a trailing `if` (both branches) and a trailing `try` (the
/// block and every `catch` arm — never the `finally`, whose value Groovy
/// discards). Returns `None` when the body has no trailing value expression to
/// carry out, in which case the caller keeps its original lowering.
fn tail_return(body: &[Stmt]) -> Option<Vec<Stmt>> {
    let (last, init) = body.split_last()?;
    let new_last = match &last.kind {
        // `println`/`print` are void — no implicit return value.
        StmtKind::Expr(Expr::Println { .. }) => return None,
        StmtKind::Expr(e) => Stmt::new(
            last.line,
            StmtKind::Return {
                value: Some(e.clone()),
            },
        ),
        StmtKind::If { cond, then, els } => Stmt::new(
            last.line,
            StmtKind::If {
                cond: cond.clone(),
                then: tail_return(then).unwrap_or_else(|| then.to_vec()),
                els: tail_return(els).unwrap_or_else(|| els.to_vec()),
            },
        ),
        StmtKind::Try {
            body: tbody,
            catches,
            finally_body,
        } => Stmt::new(
            last.line,
            StmtKind::Try {
                body: tail_return(tbody).unwrap_or_else(|| tbody.to_vec()),
                catches: catches
                    .iter()
                    .map(|a| CatchArm {
                        types: a.types.clone(),
                        name: a.name.clone(),
                        body: tail_return(&a.body).unwrap_or_else(|| a.body.clone()),
                    })
                    .collect(),
                finally_body: finally_body.clone(),
            },
        ),
        _ => return None,
    };
    let mut out = init.to_vec();
    out.push(new_last);
    Some(out)
}

// ── Exception detection (does the program use `try`/`throw`?) ──────────────

/// True when any statement in `body` (recursively, including class members and
/// closure bodies) is a `try` or a `throw`. Gates every exception-related op the
/// compiler emits, so an exception-free program's bytecode is unchanged.
fn body_uses_exceptions(body: &[Stmt]) -> bool {
    body.iter().any(|s| match &s.kind {
        StmtKind::Try { .. } | StmtKind::Throw(_) => true,
        StmtKind::Local { init, .. } => init.as_ref().is_some_and(expr_uses_exceptions),
        StmtKind::Assign { value, .. } => expr_uses_exceptions(value),
        StmtKind::Expr(e) => expr_uses_exceptions(e),
        StmtKind::If { cond, then, els } => {
            expr_uses_exceptions(cond) || body_uses_exceptions(then) || body_uses_exceptions(els)
        }
        StmtKind::While { cond, body } => expr_uses_exceptions(cond) || body_uses_exceptions(body),
        StmtKind::For {
            init,
            cond,
            update,
            body,
        } => {
            init.as_deref()
                .is_some_and(|s| body_uses_exceptions(std::slice::from_ref(s)))
                || cond.as_ref().is_some_and(expr_uses_exceptions)
                || update
                    .as_deref()
                    .is_some_and(|s| body_uses_exceptions(std::slice::from_ref(s)))
                || body_uses_exceptions(body)
        }
        StmtKind::Return { value } => value.as_ref().is_some_and(expr_uses_exceptions),
        StmtKind::Function { body, .. } => body_uses_exceptions(body),
        StmtKind::SetProperty { recv, value, .. } => {
            expr_uses_exceptions(recv) || expr_uses_exceptions(value)
        }
        StmtKind::SetIndex {
            recv, index, value, ..
        } => {
            expr_uses_exceptions(recv) || expr_uses_exceptions(index) || expr_uses_exceptions(value)
        }
        StmtKind::Class {
            fields,
            ctors,
            methods,
            ..
        } => {
            fields
                .iter()
                .any(|f| f.init.as_ref().is_some_and(expr_uses_exceptions))
                || ctors.iter().any(|c| body_uses_exceptions(&c.body))
                || methods.iter().any(|m| body_uses_exceptions(&m.body))
        }
        StmtKind::DoWhile { body, cond } => {
            expr_uses_exceptions(cond) || body_uses_exceptions(body)
        }
        StmtKind::Switch { subject, cases } => {
            expr_uses_exceptions(subject)
                || cases.iter().any(|c| {
                    c.label.as_ref().is_some_and(expr_uses_exceptions)
                        || body_uses_exceptions(&c.body)
                })
        }
        StmtKind::Labeled { stmt, .. } => body_uses_exceptions(std::slice::from_ref(stmt)),
        // An `assert` raises an `AssertionError`, so a program containing one
        // needs the pending-exception checks even without a `throw`.
        StmtKind::Assert { .. } => true,
        StmtKind::Break(_) | StmtKind::Continue(_) => false,
    })
}

fn expr_uses_exceptions(e: &Expr) -> bool {
    match e {
        Expr::Recorded { inner, .. } => expr_uses_exceptions(inner),
        Expr::Closure { body, .. } => body_uses_exceptions(body),
        Expr::Unary { rhs, .. } => expr_uses_exceptions(rhs),
        Expr::Binary { lhs, rhs, .. } => expr_uses_exceptions(lhs) || expr_uses_exceptions(rhs),
        Expr::Println { arg, .. } => arg.as_deref().is_some_and(expr_uses_exceptions),
        Expr::Call { args, .. } | Expr::New { args, .. } | Expr::SuperCtor { args, .. } => {
            args.iter().any(expr_uses_exceptions)
        }
        Expr::CallValue { callee, args, .. } => {
            expr_uses_exceptions(callee) || args.iter().any(expr_uses_exceptions)
        }
        Expr::List(elems) => elems.iter().any(expr_uses_exceptions),
        Expr::Map(entries) => entries
            .iter()
            .any(|(k, v)| expr_uses_exceptions(k) || expr_uses_exceptions(v)),
        Expr::MethodCall { recv, args, .. } => {
            expr_uses_exceptions(recv) || args.iter().any(expr_uses_exceptions)
        }
        Expr::Property { recv, .. } | Expr::InstanceOf { value: recv, .. } => {
            expr_uses_exceptions(recv)
        }
        Expr::Index { recv, index, .. } => {
            expr_uses_exceptions(recv) || expr_uses_exceptions(index)
        }
        Expr::Range { start, end, .. } => expr_uses_exceptions(start) || expr_uses_exceptions(end),
        Expr::GString(parts) => parts.iter().any(|p| match p {
            GStringPart::Expr(e) => expr_uses_exceptions(e),
            GStringPart::Text(_) => false,
        }),
        Expr::Ternary { cond, then, els } => {
            expr_uses_exceptions(cond) || expr_uses_exceptions(then) || expr_uses_exceptions(els)
        }
        Expr::Elvis { lhs, rhs } => expr_uses_exceptions(lhs) || expr_uses_exceptions(rhs),
        _ => false,
    }
}

// ── Generic AST walk ────────────────────────────────────────────────────────

/// Walk every expression in `body` (and in every nested statement, closure,
/// class member and function it contains) and answer whether `f` is true of any
/// of them.
///
/// Short-circuits like `||`. A predicate that always answers `false` therefore
/// visits the whole tree, which is how a *collector* uses this — see
/// [`recursive_fns`]. `f` is `&mut dyn` rather than a generic: the walk is
/// mutually recursive with [`expr_any`], and a generic closure parameter would
/// instantiate infinitely.
fn body_any(body: &[Stmt], f: &mut dyn FnMut(&Expr) -> bool) -> bool {
    body.iter().any(|s| match &s.kind {
        StmtKind::Local { init, .. } => init.as_ref().is_some_and(|e| expr_any(e, f)),
        StmtKind::Assign { value, .. } => expr_any(value, f),
        StmtKind::Expr(e) => expr_any(e, f),
        StmtKind::If { cond, then, els } => {
            expr_any(cond, f) || body_any(then, f) || body_any(els, f)
        }
        StmtKind::While { cond, body } => expr_any(cond, f) || body_any(body, f),
        StmtKind::For {
            init,
            cond,
            update,
            body,
        } => {
            init.as_deref()
                .is_some_and(|s| body_any(std::slice::from_ref(s), f))
                || cond.as_ref().is_some_and(|e| expr_any(e, f))
                || update
                    .as_deref()
                    .is_some_and(|s| body_any(std::slice::from_ref(s), f))
                || body_any(body, f)
        }
        StmtKind::Return { value } => value.as_ref().is_some_and(|e| expr_any(e, f)),
        StmtKind::Function { body, .. } => body_any(body, f),
        StmtKind::SetProperty { recv, value, .. } => expr_any(recv, f) || expr_any(value, f),
        StmtKind::SetIndex {
            recv, index, value, ..
        } => expr_any(recv, f) || expr_any(index, f) || expr_any(value, f),
        StmtKind::Class {
            fields,
            ctors,
            methods,
            ..
        } => {
            fields
                .iter()
                .any(|fl| fl.init.as_ref().is_some_and(|e| expr_any(e, f)))
                || ctors.iter().any(|c| body_any(&c.body, f))
                || methods.iter().any(|m| body_any(&m.body, f))
        }
        StmtKind::Try {
            body,
            catches,
            finally_body,
        } => {
            body_any(body, f)
                || body_any(finally_body, f)
                || catches.iter().any(|c| body_any(&c.body, f))
        }
        StmtKind::Throw(e) => expr_any(e, f),
        StmtKind::DoWhile { body, cond } => expr_any(cond, f) || body_any(body, f),
        StmtKind::Switch { subject, cases } => {
            expr_any(subject, f)
                || cases.iter().any(|c| {
                    c.label.as_ref().is_some_and(|e| expr_any(e, f)) || body_any(&c.body, f)
                })
        }
        StmtKind::Labeled { stmt, .. } => body_any(std::slice::from_ref(stmt), f),
        StmtKind::Assert { cond, message, .. } => {
            expr_any(cond, f) || message.as_ref().is_some_and(|e| expr_any(e, f))
        }
        StmtKind::Break(_) | StmtKind::Continue(_) => false,
    })
}

/// [`body_any`] for a single expression: `f` is applied to `e` itself and to
/// every expression reachable from it.
fn expr_any(e: &Expr, f: &mut dyn FnMut(&Expr) -> bool) -> bool {
    if f(e) {
        return true;
    }
    match e {
        Expr::Iterable(inner) | Expr::SpreadArg(inner) => expr_any(inner, f),
        Expr::Recorded { inner, .. } => expr_any(inner, f),
        Expr::Call { args, .. } => args.iter().any(|a| expr_any(a, f)),
        Expr::Unary { rhs, .. } => expr_any(rhs, f),
        Expr::Cast { value, .. } => expr_any(value, f),
        Expr::Binary { lhs, rhs, .. } => expr_any(lhs, f) || expr_any(rhs, f),
        Expr::Println { arg, .. } => arg.as_deref().is_some_and(|a| expr_any(a, f)),
        Expr::List(elems) => elems.iter().any(|a| expr_any(a, f)),
        Expr::Map(entries) => entries
            .iter()
            .any(|(k, v)| expr_any(k, f) || expr_any(v, f)),
        Expr::MethodCall { recv, args, .. } => {
            expr_any(recv, f) || args.iter().any(|a| expr_any(a, f))
        }
        Expr::Property { recv, .. } => expr_any(recv, f),
        Expr::Closure { body, .. } => body_any(body, f),
        Expr::Range { start, end, .. } => expr_any(start, f) || expr_any(end, f),
        Expr::GString(parts) => parts.iter().any(|p| match p {
            GStringPart::Expr(e) => expr_any(e, f),
            GStringPart::Text(_) => false,
        }),
        Expr::Ternary { cond, then, els } => {
            expr_any(cond, f) || expr_any(then, f) || expr_any(els, f)
        }
        Expr::Elvis { lhs, rhs } => expr_any(lhs, f) || expr_any(rhs, f),
        Expr::CallValue { callee, args, .. } => {
            expr_any(callee, f) || args.iter().any(|a| expr_any(a, f))
        }
        Expr::New { args, .. } => args.iter().any(|a| expr_any(a, f)),
        Expr::Index { recv, index, .. } => expr_any(recv, f) || expr_any(index, f),
        Expr::SuperCtor { args, .. } => args.iter().any(|a| expr_any(a, f)),
        Expr::InstanceOf { value, .. } => expr_any(value, f),
        Expr::Regex(_)
        | Expr::Int(..)
        | Expr::Float(_)
        | Expr::Dec(_)
        | Expr::BigInt(_)
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

// ── FFI detection (does the program contain a `rust { ... }` block?) ────────

/// True if any statement in `body` (recursively) evaluates a `__rust_compile`
/// call — the desugar target of a `rust { ... }` block.
fn body_has_ffi(body: &[Stmt]) -> bool {
    body_any(
        body,
        &mut |e| matches!(e, Expr::Call { name, .. } if name == RUST_COMPILE),
    )
}

// ── Recursion detection (which functions need a call-depth guard?) ──────────

/// The top-level functions that can reach themselves through the static call
/// graph — directly (`def f(n) { f(n + 1) }`) or through a cycle
/// (`def a() { b() }; def b() { a() }`).
///
/// Only these carry the [`crate::host::GDEPTH`] prologue check, because only a
/// native `Op::Call` can recurse without the host seeing it: a closure, a
/// method, a constructor and an operator overload all run through
/// `host::run_sub`, which enforces [`crate::host::MAX_CALL_DEPTH`] itself. A
/// function that cannot reach itself cannot recurse, so it keeps a prologue of
/// nothing but `SetSlot`s — which matters, because a `CallBuiltin` anywhere in a
/// region a tracing-JIT recording walks aborts the trace.
///
/// The graph counts a call written anywhere in the body, including inside a
/// nested closure. That over-approximates (the closure may never be invoked),
/// and over-approximating costs a guard that never fires rather than a recursion
/// that never raises.
fn recursive_fns(prog: &Program, fn_names: &HashSet<String>) -> HashSet<String> {
    let mut callees: HashMap<&str, HashSet<&str>> = HashMap::new();
    for stmt in &prog.body {
        let StmtKind::Function { name, body, .. } = &stmt.kind else {
            continue;
        };
        let edges = callees.entry(name.as_str()).or_default();
        body_any(body, &mut |e| {
            if let Expr::Call { name: callee, .. } = e {
                if let Some(known) = fn_names.get(callee) {
                    edges.insert(known.as_str());
                }
            }
            false
        });
    }
    // Transitive closure by repeated relaxation: the graph has one node per
    // top-level function, so this converges in at most that many rounds.
    let mut changed = true;
    while changed {
        changed = false;
        for name in callees.keys().copied().collect::<Vec<_>>() {
            let reached: HashSet<&str> = callees[name]
                .iter()
                .flat_map(|c| callees.get(c).into_iter().flatten().copied())
                .collect();
            let edges = callees.get_mut(name).expect("key just enumerated");
            let before = edges.len();
            edges.extend(reached);
            changed |= edges.len() != before;
        }
    }
    callees
        .iter()
        .filter(|(name, reach)| reach.contains(*name))
        .map(|(name, _)| (*name).to_string())
        .collect()
}

fn compound_op(op: AssignOp) -> Op {
    match op {
        AssignOp::Add => Op::Add,
        AssignOp::Sub => Op::Sub,
        AssignOp::Mul => Op::Mul,
        AssignOp::Mod => unreachable!("Mod lowers through Compiler::emit_mod, not compound_op"),
        AssignOp::Div => unreachable!("Div lowers through the GDIV builtin, not compound_op"),
        AssignOp::Assign => unreachable!("plain assign never lowers through compound_op"),
        AssignOp::Bin(_) => {
            unreachable!("the bitwise forms lower through Compiler::binary, not compound_op")
        }
    }
}

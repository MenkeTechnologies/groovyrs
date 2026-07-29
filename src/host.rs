//! The groovyrs host: builtin registration, Groovy value formatting, the Groovy
//! `/` division builtin, and the strict numeric hook.
//!
//! groovyrs owns a host-side object heap keyed by fusevm's opaque
//! `Value::Obj(u32)` handle: closures (with their captured upvalues), class
//! instances, and insertion-ordered maps all live there, with a class registry
//! alongside. fusevm carries only the handle. Beyond that heap, three places
//! need Groovy semantics that fusevm's default awk/shell flavour does not
//! provide:
//!
//! 1. **Printing.** fusevm's native `PrintLn` renders values shell-style
//!    (`true`→`1`, `3.0`→`3`). `println`/`print` instead lower to a registered
//!    builtin ([`GPRINTLN`]/[`GPRINT`]) that formats through [`groovy_str`] —
//!    `true`/`false`, `3.0`, `null` — matching Groovy.
//! 2. **`/` division.** Groovy divides two integers as `BigDecimal`, so `7/2`
//!    is `3.5`, not `3`. `/` lowers to [`GDIV`], which returns an integer only
//!    when the division is exact and a decimal otherwise.
//! 3. **`+` overloading.** Groovy's `+` dispatches on its left operand: a list
//!    concatenates/appends, a map merges, and a `String` (or other scalar)
//!    concatenates. fusevm runs *strict* once a numeric hook is installed,
//!    delegating any operation with a non-numeric operand to [`numeric_hook`],
//!    where `+` routes through `groovy_add`.

use crate::decimal;
use bigdecimal::BigDecimal;
use fusevm::{Frame, NumOp, VMResult, Value, VM};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// Builtin id for `println` (one Groovy-formatted arg + newline).
pub const GPRINTLN: u16 = 700;
/// Builtin id for `print` (one Groovy-formatted arg, no newline).
pub const GPRINT: u16 = 701;
/// Builtin id for Groovy `/` division (BigDecimal-style promotion).
pub const GDIV: u16 = 702;
/// Builtin id for compiling + registering an inline `rust { ... }` FFI block.
/// Pops the base64 block body (a `String`) and hands it to
/// `fusevm::ffi::compile_and_register`; the desugar target `__rust_compile`
/// lowers to this (see [`crate::rust_ffi`]).
pub const GFFI_COMPILE: u16 = 703;
/// Builtin id for calling an FFI-exported function by name. The `argc` payload
/// is the argument count; the stack holds the args (deepest first) with the
/// function name (a `String`) on top. Dispatches through `fusevm::ffi::try_call`
/// and returns the result.
pub const GFFI_CALL: u16 = 704;
/// Builtin id for a Groovy method call `recv.method(args...)`. The stack holds
/// the receiver (deepest), the `argc` args, and the method name (a `String`) on
/// top. Dispatches a faithful GDK subset (see `dispatch_method`).
pub const GMETHOD: u16 = 705;
/// Builtin id for a Groovy property read `recv.name` (e.g. `list.size`,
/// `str.length`). The stack holds the receiver then the property name on top.
pub const GPROP: u16 = 706;
/// Builtin id for building a closure value. The stack holds the closure's
/// synthetic name-pool index and its parameter count (two integers); the builtin
/// registers them and returns a `Value::Obj` handle (see `invoke_closure`).
pub const GMAKE_CLOSURE: u16 = 707;
/// Builtin id for invoking a closure directly, `f(args)`. The stack holds the
/// closure (deepest), the `argc` args, and the callee name (a `String`) on top;
/// faults `unresolved reference: name` when the value is not a closure.
pub const GCLOSURE_CALL: u16 = 708;
/// Builtin id for the safe-navigation method call `recv?.method(args)`. Same
/// stack layout as [`GMETHOD`]; returns `null` without dispatching when the
/// receiver is `null`.
pub const GMETHOD_SAFE: u16 = 709;
/// Builtin id for the safe-navigation property read `recv?.name`. Same stack
/// layout as [`GPROP`]; returns `null` when the receiver is `null`.
pub const GPROP_SAFE: u16 = 710;
/// Builtin id for registering a class declaration. The stack holds (deepest
/// first) the class name (`String`), the field-name list (`Array`), the method
/// table (`Hash` name→name-pool-index), the field-initializer table (`Hash`),
/// and the constructor table (`Hash` arity→name-pool-index) on top. Builds a
/// `ClassMeta` in the registry; returns `null`.
pub const GCLASS: u16 = 711;
/// Builtin id for `new C(args)`. The stack holds the `argc` constructor args
/// (deepest first) with the class name (`String`) on top; allocates a heap
/// instance, runs field initializers and the arity-matched constructor, and
/// returns the instance handle (`Value::Obj`).
pub const GNEW: u16 = 712;
/// Builtin id for a property assignment `recv.name = value`. The stack holds the
/// receiver (deepest), the value, then the property name (`String`) on top.
/// Honours a user `set<Name>` setter, else writes the instance field (or map
/// entry); returns the assigned value.
pub const GSETPROP: u16 = 713;
/// Builtin id for an index read `recv[index]`. The stack holds the receiver
/// (deepest) then the index on top. Dispatches to a user `getAt(index)` overload
/// on an instance, else a list/map/string element.
pub const GINDEX: u16 = 714;
/// Builtin id for Groovy `<=>` (three-way compare). Pops two operands; on a
/// user-class instance left operand it dispatches `compareTo` (re-entering the
/// VM), otherwise it yields the primitive sign (`-1`/`0`/`1`). See `b_cmp`.
pub const GCMP: u16 = 716;
/// Builtin id for a `super.method(args)` call. The stack holds `this` (deepest),
/// the `argc` args, the method name (`String`), and the superclass name
/// (`String`) on top. Resolves the method from the superclass upward (skipping
/// the current class's override) and invokes it on `this`. See `b_super_method`.
pub const GSUPER_METHOD: u16 = 717;
/// Builtin id for a `super(args)` constructor call. The stack holds `this`
/// (deepest), the `argc` args, and the superclass name (`String`) on top. Runs
/// the superclass's arity-matched constructor on `this`. See `b_super_ctor`.
pub const GSUPER_CTOR: u16 = 718;
/// Builtin id for `value instanceof Class`. The stack holds the value (deepest)
/// then the class name (`String`) on top. Returns a `Boolean`. See
/// `b_instanceof`.
pub const GINSTANCEOF: u16 = 719;
/// Builtin id for building a Groovy map literal `[k: v, …]`. The stack holds the
/// interleaved key/value pairs (key pushed first) with the entry count on top;
/// returns an insertion-ordered map handle (`Value::Obj`). Groovy maps preserve
/// insertion order (a `LinkedHashMap`), which fusevm's unordered `Hash` cannot,
/// so the map lives in the host heap instead.
pub const GMAKE_MAP: u16 = 715;
/// Builtin id for materializing a `BigDecimal` literal. The stack holds the
/// literal's source text (a `String`); the builtin parses it into an exact
/// (unscaled value, scale) decimal on the host heap and returns its handle.
/// Literals are interned by text, so re-evaluating one in a loop reuses the same
/// handle instead of growing the heap. See [`crate::decimal`].
pub const GDEC: u16 = 720;
/// Builtin id for Groovy truthiness. Pops one value and pushes the `Boolean`
/// [`groovy_truthy`] computes for it. Emitted only in front of a condition whose
/// static shape could be a value fusevm's own `is_truthy` gets wrong — a heap
/// handle (`BigDecimal`, ordered map, closure, instance) or a `String`. A
/// comparison-shaped condition (`i < n`) is statically a `Boolean` and stays on
/// the native/JIT path with no builtin at all (see `compiler::needs_truth`).
pub const GTRUTH: u16 = 721;
/// Builtin id for Groovy truthiness that *keeps* its operand: peeks the top of
/// the stack and pushes the `Boolean` above it. Used by `&&`/`||`/Elvis, which
/// must yield the deciding operand itself rather than its truth value.
pub const GTRUTH_KEEP: u16 = 722;
/// Builtin id for building a `GString`. The stack holds the rendered parts
/// (literal text and evaluated placeholder values, in source order) with the
/// part count on top; the builtin renders each with the `println` rules — so an
/// embedded object goes through its `toString()` — and returns the joined
/// `String`.
pub const GSTRING: u16 = 723;

// ── Exceptions (`throw` / `try` / `catch` / `finally`) ──────────────────────
//
// fusevm has no unwind opcode. groovyrs models the in-flight exception the way
// the sibling frontends do: a host-side pending value plus two compiler-side
// pieces — inside a frame an unwind is a `Jump` to the innermost handler, and
// across a frame boundary it is a return followed by a pending-exception check
// at the call site. Every one of these builtins is emitted only by a program
// that actually uses `try`/`throw`, so an exception-free program's bytecode is
// byte-for-byte what it was.

/// Arm exception handling for this run. `argc == 0`, emitted once at the top of
/// a program that uses `try`/`throw`. While armed, a runtime error that Groovy
/// raises as a `Throwable` (a zero divisor) parks a catchable exception instead
/// of aborting the run.
pub const GEXC_ARM: u16 = 730;
/// `throw e` — stack `[throwable]`, `argc == 1`. Parks the value as the pending
/// exception; the compiler emits the jump to the handler right after.
pub const GTHROW: u16 = 731;
/// Is an exception in flight? `argc == 0`; pushes a `Bool`. Emitted after every
/// call site that can re-enter the VM.
pub const GEXC_PENDING: u16 = 732;
/// Take the pending exception, clearing it. `argc == 0`; pushes the throwable.
/// Emitted at the top of a handler.
pub const GEXC_TAKE: u16 = 733;
/// The current value-stack depth. `argc == 0`; pushes an `Int`. Recorded on
/// entry to a `try` so the handler can discard the operands of the expression
/// the throw abandoned.
pub const GEXC_DEPTH: u16 = 734;
/// Truncate the value stack to a depth recorded by [`GEXC_DEPTH`]. Stack
/// `[depth]`, `argc == 1`.
pub const GEXC_CUT: u16 = 735;
/// Report an uncaught exception and halt. `argc == 0`. Formats Groovy's
/// `Caught: <qualified class>: <message>` line and faults, so the process exits
/// non-zero exactly as `groovy` does.
pub const GEXC_ABORT: u16 = 736;

/// Builtin id for the `--dap` per-statement line marker. Emitted only by the
/// debug compiler (`compiler::compile_debug`); an ordinary run never registers a
/// handler for it, so it costs nothing. The debug run path registers a handler
/// that calls [`crate::dap::on_debug_line`].
pub const DBG_LINE: u16 = 799;

/// Install groovyrs builtins on a VM: the Groovy-formatting print builtins, the
/// division builtin, and the inline-Rust FFI compile/call dispatch. This is the
/// single install choke point later waves (methods, `String`/list objects, the
/// GDK) grow into.
pub fn install(vm: &mut VM) {
    vm.register_builtin(GPRINTLN, b_println);
    vm.register_builtin(GPRINT, b_print);
    vm.register_builtin(GDIV, b_div);
    vm.register_builtin(GFFI_COMPILE, b_ffi_compile);
    vm.register_builtin(GFFI_CALL, b_ffi_call);
    vm.register_builtin(GMETHOD, b_method);
    vm.register_builtin(GPROP, b_prop);
    vm.register_builtin(GMAKE_CLOSURE, b_make_closure);
    vm.register_builtin(GCLOSURE_CALL, b_closure_call);
    vm.register_builtin(GMETHOD_SAFE, b_method_safe);
    vm.register_builtin(GPROP_SAFE, b_prop_safe);
    vm.register_builtin(GCLASS, b_class);
    vm.register_builtin(GNEW, b_new);
    vm.register_builtin(GSETPROP, b_setprop);
    vm.register_builtin(GINDEX, b_index);
    vm.register_builtin(GMAKE_MAP, b_make_map);
    vm.register_builtin(GCMP, b_cmp);
    vm.register_builtin(GSUPER_METHOD, b_super_method);
    vm.register_builtin(GSUPER_CTOR, b_super_ctor);
    vm.register_builtin(GINSTANCEOF, b_instanceof);
    vm.register_builtin(GDEC, b_dec);
    vm.register_builtin(GTRUTH, b_truth);
    vm.register_builtin(GTRUTH_KEEP, b_truth_keep);
    vm.register_builtin(GSTRING, b_gstring);
    vm.register_builtin(GEXC_ARM, b_exc_arm);
    vm.register_builtin(GTHROW, b_throw);
    vm.register_builtin(GEXC_PENDING, b_exc_pending);
    vm.register_builtin(GEXC_TAKE, b_exc_take);
    vm.register_builtin(GEXC_DEPTH, b_exc_depth);
    vm.register_builtin(GEXC_CUT, b_exc_cut);
    vm.register_builtin(GEXC_ABORT, b_exc_abort);
    // A fresh VM install starts with an empty object heap: `Value::Obj` handles
    // are chunk-relative (a closure carries a name-pool index, an instance a
    // class id), so a handle from a prior run must never survive into a new
    // chunk. The class registry is likewise rebuilt per program.
    reset_heap();
}

// ── Host object heap (keyed by `Value::Obj(u32)`) ───────────────────────────
//
// fusevm's `Value::Obj(u32)` is an opaque handle into a *frontend-owned* object
// heap; fusevm only carries the handle. groovyrs owns the pointed-to objects
// here. Both closures and class instances live in the one `HEAP` vector, indexed
// by the handle, so identity (`is`) is the handle and no fusevm change is
// needed. The class table (`CLASSES`) maps a class id to its metadata.

thread_local! {
    /// The object heap. A `Value::Obj(id)` indexes this vector. Cleared on each
    /// [`install`] because handles are only meaningful for the chunk that made
    /// them (closures carry a chunk name-pool index).
    static HEAP: RefCell<Vec<HeapObj>> = const { RefCell::new(Vec::new()) };
    /// The class registry, keyed by class id. Populated by the class-register
    /// builtin as the program's `class` declarations execute.
    static CLASSES: RefCell<Vec<ClassMeta>> = const { RefCell::new(Vec::new()) };
    /// The VM currently executing, published for the strict numeric hook.
    ///
    /// fusevm calls the [`NumericHook`](fusevm::NumericHook) with `(op, &a, &b)`
    /// and *no* VM handle, so operator overloading — which must re-enter the VM to
    /// run a user `plus`/`minus`/`compareTo`/… method — has nothing to dispatch
    /// through. groovyrs publishes the running VM here around `crate::run_chunk`'s
    /// `VM::run` so [`numeric_hook`] can reach it. The pointer is the very VM that
    /// is executing (fusevm calls the hook synchronously from inside its dispatch
    /// loop), so it is always live while the hook runs; builtins already receive
    /// `&mut VM` and never consult it.
    static VM_PTR: Cell<*mut VM> = const { Cell::new(std::ptr::null_mut()) };
    /// Decimal literals already materialized on the heap, keyed by source text.
    /// A literal inside a loop is pushed once per iteration; interning keeps
    /// that from appending a heap entry every time. Cleared with the heap.
    static DEC_LITERALS: RefCell<HashMap<String, u32>> = RefCell::new(HashMap::new());
    /// The exception in flight, if any. Set by [`GTHROW`], cleared by
    /// [`GEXC_TAKE`] when a handler claims it. It lives here rather than on the
    /// value stack because it has to survive the returns that unwind every frame
    /// between the `throw` and its handler.
    static PENDING: RefCell<Option<Value>> = const { RefCell::new(None) };
    /// True once [`GEXC_ARM`] has run — i.e. the program uses `try`/`throw`, so
    /// the compiler emitted the pending-exception checks that make a parked
    /// exception observable. A runtime error Groovy models as a `Throwable`
    /// (a zero divisor) is only *thrown* while armed; unarmed it stays the hard
    /// fault it has always been, because nothing would ever look at it.
    static EXC_ARMED: Cell<bool> = const { Cell::new(false) };
}

/// Is an exception in flight? Host-side loops that drive user code (GDK
/// iteration, constructor field initializers) must stop as soon as one is.
fn pending_exc() -> bool {
    PENDING.with(|p| p.borrow().is_some())
}

/// Park `exc` as the pending exception. The compiler emits the jump to the
/// handler (or the frame exit) immediately after the `throw` site; a host-raised
/// throwable relies on the caller's post-call pending check instead.
fn set_pending(exc: Value) {
    PENDING.with(|p| *p.borrow_mut() = Some(exc));
}

/// Raise a Groovy `Throwable` from inside a builtin. While exception handling is
/// armed this parks a catchable exception; otherwise it degrades to the hard
/// `groovyrs:` fault the same condition has always produced, so an
/// exception-free program's behaviour is unchanged.
fn raise(vm: &mut VM, class: &str, message: &str) {
    if EXC_ARMED.with(|a| a.get()) {
        set_pending(new_throwable(class, message));
    } else {
        fault(vm, format!("groovyrs: {message}"));
    }
}

/// Allocate a built-in throwable instance with `message` on the host heap.
fn new_throwable(class: &str, message: &str) -> Value {
    let cid = find_class(class).unwrap_or(0);
    let mut fields = std::collections::HashMap::new();
    fields.insert("message".to_string(), Value::str(message.to_string()));
    heap_push(HeapObj::Instance(Instance { class: cid, fields }))
}

/// Register the built-in throwable hierarchy (`Exception`, `IOException`, …) as
/// ordinary classes, so `new`, `instanceof`, `catch` matching, and a user
/// `class MyEx extends Exception` all run through the one class registry. The
/// root carries the `message` field, which [`class_chain`] then materialises on
/// every descendant.
fn register_throwables() {
    CLASSES.with(|c| {
        let mut c = c.borrow_mut();
        for (name, superclass, _) in crate::throwable::all() {
            c.push(ClassMeta {
                name: name.to_string(),
                superclass: superclass.map(str::to_string),
                field_names: if superclass.is_none() {
                    vec!["message".to_string()]
                } else {
                    Vec::new()
                },
                field_inits: Vec::new(),
                methods: std::collections::HashMap::new(),
                ctors: std::collections::HashMap::new(),
            });
        }
    });
}

/// True when `class` (a registry id) descends from the built-in `Throwable`.
fn is_throwable_class(class: u32) -> bool {
    class_chain(class)
        .first()
        .and_then(|id| class_meta(*id))
        .is_some_and(|m| m.name == "Throwable")
}

/// Render a throwable the way `Throwable.toString()` does: the qualified class
/// name (bare for a script-declared subclass), plus `": " + message` when a
/// message was supplied. Verified against Apache Groovy 5.0.7.
fn throwable_str(v: &Value) -> String {
    let Some(inst) = as_instance(v) else {
        return groovy_str(v);
    };
    let Some(meta) = class_meta(inst.class) else {
        return groovy_str(v);
    };
    let name = crate::throwable::qualified(&meta.name);
    match inst.fields.get("message") {
        Some(m) if !matches!(m, Value::Undef) => format!("{name}: {}", groovy_str(m)),
        _ => name,
    }
}

/// `GEXC_ARM`: arm exception handling for this run (see [`EXC_ARMED`]).
fn b_exc_arm(vm: &mut VM, argc: u8) -> Value {
    pop_args(vm, argc);
    EXC_ARMED.with(|a| a.set(true));
    Value::Undef
}

/// `GTHROW`: park the throwable as the pending exception.
fn b_throw(vm: &mut VM, argc: u8) -> Value {
    let exc = pop_args(vm, argc)
        .into_iter()
        .next()
        .unwrap_or(Value::Undef);
    set_pending(exc);
    Value::Undef
}

/// `GEXC_PENDING`: true while an exception is in flight (the post-call check).
fn b_exc_pending(vm: &mut VM, argc: u8) -> Value {
    pop_args(vm, argc);
    Value::bool(pending_exc())
}

/// `GEXC_TAKE`: claim the pending exception for a handler, clearing it.
fn b_exc_take(vm: &mut VM, argc: u8) -> Value {
    pop_args(vm, argc);
    PENDING
        .with(|p| p.borrow_mut().take())
        .unwrap_or(Value::Undef)
}

/// `GEXC_DEPTH`: the value-stack depth at `try` entry.
fn b_exc_depth(vm: &mut VM, argc: u8) -> Value {
    pop_args(vm, argc);
    Value::int(vm.stack.len() as i64)
}

/// `GEXC_CUT`: discard everything the abandoned expression left on the value
/// stack, back to the depth [`GEXC_DEPTH`] recorded at `try` entry. Frames
/// between the throw and the handler clean themselves (a return truncates to the
/// frame base), but the operands of the half-evaluated expression *inside* the
/// handler's own frame would otherwise pile up — once per throw, forever, in a
/// loop.
fn b_exc_cut(vm: &mut VM, argc: u8) -> Value {
    let depth = pop_args(vm, argc)
        .first()
        .map(|v| v.to_int())
        .unwrap_or(0)
        .max(0) as usize;
    if depth <= vm.stack.len() {
        vm.stack.truncate(depth);
    }
    Value::Undef
}

/// `GEXC_ABORT`: an exception that reached the end of the script. Reports it the
/// way `groovy` does (`Caught: java.lang.Foo: message`) and faults, so the
/// process exits non-zero.
fn b_exc_abort(vm: &mut VM, argc: u8) -> Value {
    pop_args(vm, argc);
    let exc = PENDING
        .with(|p| p.borrow_mut().take())
        .unwrap_or(Value::Undef);
    let msg = format!("Caught: {}", throwable_str(&exc));
    fault(vm, msg);
    Value::Undef
}

/// Pop `argc` arguments off the value stack in source order.
fn pop_args(vm: &mut VM, argc: u8) -> Vec<Value> {
    let mut args = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        args.push(vm.stack.pop().unwrap_or(Value::Undef));
    }
    args.reverse();
    args
}

/// Publish the running VM so the numeric hook can re-enter it for operator
/// overloading (see `VM_PTR`). Called around `VM::run` in `crate::run_chunk`
/// and the debug runner; paired with [`clear_vm_ptr`].
pub fn set_vm_ptr(vm: &mut VM) {
    VM_PTR.with(|p| p.set(vm as *mut VM));
}

/// Clear the published VM pointer once a run returns (see `VM_PTR`).
pub fn clear_vm_ptr() {
    VM_PTR.with(|p| p.set(std::ptr::null_mut()));
}

/// Re-enter the published VM to run `f`. Returns `None` when no VM is published
/// (the hook fired outside a run — never in practice).
///
/// SAFETY: fusevm invokes the numeric hook synchronously from inside `VM::run`,
/// so the pointer published by [`set_vm_ptr`] is the exact VM executing and stays
/// valid for the hook's duration. Every operator path clones its operands before
/// calling this, so no borrow of fusevm's operand stack is read after the nested
/// run mutates (and possibly reallocates) it.
fn with_vm<R>(f: impl FnOnce(&mut VM) -> R) -> Option<R> {
    VM_PTR.with(|p| {
        let ptr = p.get();
        (!ptr.is_null()).then(|| f(unsafe { &mut *ptr }))
    })
}

/// A heap object behind a `Value::Obj` handle: a closure, a class instance, or
/// an insertion-ordered map.
enum HeapObj {
    Closure(ClosureMeta),
    Instance(Instance),
    /// A Groovy map: insertion-ordered key/value pairs (a `LinkedHashMap`
    /// equivalent). Lives on the heap so `println` order is Groovy's and
    /// `m.k = v` mutates in place through the shared handle.
    OrderedMap(Vec<(String, Value)>),
    /// A `java.math.BigDecimal` — every unsuffixed Groovy decimal. fusevm's
    /// `Value::Float` is an `f64` and cannot carry a decimal's scale or its
    /// unbounded magnitude, so decimals ride a handle like any other host
    /// object. Being non-numeric to fusevm, they route every arithmetic and
    /// comparison op through [`numeric_hook`], where [`crate::decimal`] applies
    /// Groovy's exact scale rules.
    Dec(BigDecimal),
}

/// A registered closure: the body's name-pool index, its parameter count, and
/// the values captured from the enclosing frame at creation time (its upvalues).
/// Captures are stored by value, so a curried `{ x -> { y -> x + y } }` sees the
/// outer `x` after the outer frame has returned.
#[derive(Clone)]
struct ClosureMeta {
    name_idx: u16,
    params: u8,
    captures: Vec<Value>,
}

/// A class instance: its class id (into `CLASSES`) and its field values keyed by
/// field name.
#[derive(Clone)]
struct Instance {
    class: u32,
    fields: std::collections::HashMap<String, Value>,
}

/// Compiled metadata for one class: its name, field names (in declaration order,
/// for default construction and iteration), and the method/constructor
/// name-pool indices resolved to subroutine entries at call time.
#[derive(Clone)]
struct ClassMeta {
    name: String,
    /// The direct superclass's name (`class C extends B`), or `None` for a root
    /// class. Resolved to an id lazily (via [`find_class`]) so declaration order
    /// does not matter. Drives method/field inheritance and virtual dispatch.
    superclass: Option<String>,
    field_names: Vec<String>,
    /// Field initializer thunks: name-pool index of a synthetic 0-arg subroutine
    /// that computes the initial value, per field that has an initializer.
    field_inits: Vec<(String, u16)>,
    /// method name → subroutine name-pool index.
    methods: std::collections::HashMap<String, u16>,
    /// constructor subroutine name-pool indices keyed by arity.
    ctors: std::collections::HashMap<u8, u16>,
}

/// Clear the object heap, class registry, and decimal-literal intern table
/// (called from [`install`]).
fn reset_heap() {
    HEAP.with(|h| h.borrow_mut().clear());
    CLASSES.with(|c| c.borrow_mut().clear());
    DEC_LITERALS.with(|d| d.borrow_mut().clear());
    PENDING.with(|p| *p.borrow_mut() = None);
    EXC_ARMED.with(|a| a.set(false));
    // The built-in throwables occupy the first registry ids of every run; a
    // program's own `class` declarations append after them.
    register_throwables();
}

/// Push an object onto the heap and return its `Value::Obj` handle.
fn heap_push(obj: HeapObj) -> Value {
    let id = HEAP.with(|h| {
        let mut h = h.borrow_mut();
        let id = h.len() as u32;
        h.push(obj);
        id
    });
    Value::Obj(id)
}

/// Look up a closure handle's metadata, if `v` is a closure value.
fn closure_meta(v: &Value) -> Option<ClosureMeta> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::Closure(c)) => Some(c.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// Clone the `BigDecimal` behind a handle, if `v` is a decimal.
fn as_dec(v: &Value) -> Option<BigDecimal> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::Dec(d)) => Some(d.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// The `BigDecimal` view of any Groovy number: a decimal as itself, an
/// `Integer`/`Boolean` at scale 0. `None` for a `double` (which must stay on the
/// IEEE path) and for non-numbers.
fn as_exact_dec(v: &Value) -> Option<BigDecimal> {
    match v {
        Value::Int(n) => Some(decimal::from_i64(*n)),
        Value::Bool(b) => Some(decimal::from_i64(*b as i64)),
        _ => as_dec(v),
    }
}

/// Put a `BigDecimal` on the heap and return its handle.
fn dec_value(d: BigDecimal) -> Value {
    heap_push(HeapObj::Dec(d))
}

/// `GDEC`: pop a decimal literal's source text and return the handle of its
/// `BigDecimal`, interning by text so a literal in a loop allocates once.
fn b_dec(vm: &mut VM, _argc: u8) -> Value {
    let text = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    if let Some(id) = DEC_LITERALS.with(|d| d.borrow().get(&text).copied()) {
        return Value::Obj(id);
    }
    // The lexer already rejected malformed literals, so this parse cannot fail
    // for compiler-emitted text; a zero keeps a hypothetical bad call total.
    let value = dec_value(decimal::parse(&text).unwrap_or_else(|| decimal::from_i64(0)));
    if let Value::Obj(id) = value {
        DEC_LITERALS.with(|d| d.borrow_mut().insert(text, id));
    }
    value
}

// ── Groovy truthiness ───────────────────────────────────────────────────────
//
// fusevm's `Value::is_truthy` is shell/awk-flavoured and differs from Groovy in
// two places: every `Value::Obj` handle is true (so a zero `BigDecimal` and an
// empty ordered map are both true), and the string `"0"` is false. Groovy's rule
// is: `null` false; `Boolean` itself; a number non-zero; a `String`/`GString`
// non-empty; a `Collection`/`Map` non-empty; any other object true unless its
// class defines `asBoolean()`.
//
// The compiler emits [`GTRUTH`] only where the condition's static type could be
// one of the values fusevm gets wrong, so a comparison-shaped loop guard keeps
// the native op and the JIT trace (see `compiler::needs_truth`).

/// Groovy truthiness of `v`. Needs the VM because a class instance may define
/// `asBoolean()`, which is a user method and so re-enters the VM.
fn groovy_truthy(vm: &mut VM, v: &Value) -> bool {
    match v {
        // Groovy: a String is truthy when non-empty. fusevm additionally treats
        // "0" as false (a shell convention), which Groovy does not.
        Value::Str(s) => !s.is_empty(),
        Value::Obj(_) => {
            if let Some(d) = as_dec(v) {
                return !decimal::cmp(&d, &decimal::from_i64(0)).is_eq();
            }
            if let Some(entries) = as_omap(v) {
                return !entries.is_empty();
            }
            // A closure handle is an object: always true.
            if closure_meta(v).is_some() {
                return true;
            }
            // A class instance: `asBoolean()` decides when the class defines it,
            // else an object is true.
            match call_user_method(vm, v, "asBoolean", &[]) {
                Some(Ok(r)) => r.is_truthy(),
                Some(Err(e)) => {
                    fault(vm, e);
                    false
                }
                None => true,
            }
        }
        _ => v.is_truthy(),
    }
}

/// `GTRUTH`: pop a value, push its Groovy truth.
fn b_truth(vm: &mut VM, _argc: u8) -> Value {
    let v = vm.stack.pop().unwrap_or(Value::Undef);
    Value::bool(groovy_truthy(vm, &v))
}

/// `GSTRING`: pop the part count and that many rendered parts, then join them.
/// Rendering goes through [`render_value`], so an embedded class instance uses
/// its `toString()` — which is what a Groovy `GString` does and what plain `+`
/// concatenation of the pieces would not.
fn b_gstring(vm: &mut VM, _argc: u8) -> Value {
    let n = vm.stack.pop().unwrap_or(Value::Undef).to_int().max(0) as usize;
    let mut vals = Vec::with_capacity(n);
    for _ in 0..n {
        vals.push(vm.stack.pop().unwrap_or(Value::Undef));
    }
    vals.reverse();
    let mut out = String::new();
    for v in &vals {
        out.push_str(&render_value(vm, v));
    }
    Value::str(out)
}

/// `GTRUTH_KEEP`: push the Groovy truth of the top of the stack *above* it,
/// leaving the operand in place for `&&`/`||`/Elvis to yield.
fn b_truth_keep(vm: &mut VM, _argc: u8) -> Value {
    let v = vm.stack.last().cloned().unwrap_or(Value::Undef);
    Value::bool(groovy_truthy(vm, &v))
}

/// Clone the entries of an ordered-map handle, if `v` is one.
fn as_omap(v: &Value) -> Option<Vec<(String, Value)>> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::OrderedMap(m)) => Some(m.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// Set `key` on an ordered-map handle in place, preserving insertion order
/// (updating an existing key keeps its position; a new key appends). Returns
/// `false` if `v` is not an ordered map.
fn omap_set(v: &Value, key: String, val: Value) -> bool {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow_mut().get_mut(*id as usize) {
            Some(HeapObj::OrderedMap(m)) => {
                match m.iter_mut().find(|(k, _)| *k == key) {
                    Some(slot) => slot.1 = val,
                    None => m.push((key, val)),
                }
                true
            }
            _ => false,
        }),
        _ => false,
    }
}

/// `GMAKE_MAP`: pop the entry count and the interleaved key/value pairs, then
/// build an insertion-ordered map on the heap. A duplicate key keeps its first
/// position with the last value (Groovy's `LinkedHashMap` semantics).
fn b_make_map(vm: &mut VM, _argc: u8) -> Value {
    let n = vm.stack.pop().unwrap_or(Value::Undef).to_int() as usize;
    // Pop 2n values: they come off as v(n-1), k(n-1), …, v0, k0.
    let mut flat = Vec::with_capacity(n * 2);
    for _ in 0..(n * 2) {
        flat.push(vm.stack.pop().unwrap_or(Value::Undef));
    }
    flat.reverse();
    let mut entries: Vec<(String, Value)> = Vec::with_capacity(n);
    let mut i = 0;
    while i + 1 < flat.len() {
        // Groovy-format the key so a decimal key reads `1.50`, not a raw handle.
        let key = groovy_str(&flat[i]);
        let val = flat[i + 1].clone();
        match entries.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = val,
            None => entries.push((key, val)),
        }
        i += 2;
    }
    heap_push(HeapObj::OrderedMap(entries))
}

/// `GMAKE_CLOSURE`: pop the capture count, parameter count, and name index, then
/// the captured upvalue values (deepest-first), register the closure, and return
/// its `Value::Obj` handle.
fn b_make_closure(vm: &mut VM, _argc: u8) -> Value {
    let ncap = vm.stack.pop().unwrap_or(Value::Undef).to_int() as usize;
    let params = vm.stack.pop().unwrap_or(Value::Undef).to_int() as u8;
    let name_idx = vm.stack.pop().unwrap_or(Value::Undef).to_int() as u16;
    let mut captures = Vec::with_capacity(ncap);
    for _ in 0..ncap {
        captures.push(vm.stack.pop().unwrap_or(Value::Undef));
    }
    captures.reverse();
    heap_push(HeapObj::Closure(ClosureMeta {
        name_idx,
        params,
        captures,
    }))
}

/// Invoke a closure `clo` with `args`, running its body through the fusevm frame
/// ABI. Drives a nested `VM::run`: a call frame is pushed whose `return_ip` is
/// past the end of the chunk, so the nested run halts exactly when the closure's
/// `ReturnValue` pops that frame. The interpreter's IP is saved and restored so
/// the enclosing dispatch loop resumes where it left off.
fn invoke_closure(vm: &mut VM, clo: &Value, args: &[Value]) -> Result<Value, String> {
    let meta = closure_meta(clo).ok_or_else(|| "groovyrs: value is not a closure".to_string())?;
    let entry = vm
        .chunk
        .find_sub(meta.name_idx)
        .ok_or_else(|| "groovyrs: closure body not found".to_string())?;
    // Push exactly the parameter count the body's prologue expects: pad missing
    // arguments with `null`, drop extras. Then push the captured upvalues, in
    // declaration order, so the prologue pops them into the slots immediately
    // after the parameters (see `compiler::emit_closure`).
    let want = meta.params as usize;
    let stack_base = vm.stack.len();
    for i in 0..want {
        vm.stack.push(args.get(i).cloned().unwrap_or(Value::Undef));
    }
    for cap in &meta.captures {
        vm.stack.push(cap.clone());
    }
    run_sub(vm, entry, stack_base)
}

/// Run a subroutine body already positioned on the value stack (its prologue
/// values pushed above `stack_base`). Drives a nested `VM::run` with a call frame
/// whose `return_ip` is past the chunk end, so the nested run halts exactly when
/// the body's `ReturnValue` pops that frame; the interpreter IP is saved and
/// restored so the enclosing dispatch loop resumes cleanly. Shared by closure,
/// method, constructor, and field-initializer invocation.
fn run_sub(vm: &mut VM, entry: usize, stack_base: usize) -> Result<Value, String> {
    let return_ip = vm.chunk.ops.len();
    vm.frames.push(Frame {
        return_ip,
        stack_base,
        slots: Vec::new(),
    });
    let saved_ip = vm.ip;
    vm.ip = entry;
    let result = vm.run();
    vm.ip = saved_ip;
    match result {
        VMResult::Ok(v) => Ok(v),
        VMResult::Halted => Ok(vm.stack.pop().unwrap_or(Value::Undef)),
        VMResult::Error(e) => Err(e),
    }
}

/// Invoke a subroutine by its name-pool index, pushing `pushes` (in order) as its
/// prologue values. Used for methods (`[this, args…]`), constructors, and 0-arg
/// field-initializer thunks.
fn invoke_sub(vm: &mut VM, name_idx: u16, pushes: &[Value]) -> Result<Value, String> {
    let entry = vm
        .chunk
        .find_sub(name_idx)
        .ok_or_else(|| "groovyrs: subroutine body not found".to_string())?;
    let stack_base = vm.stack.len();
    for v in pushes {
        vm.stack.push(v.clone());
    }
    run_sub(vm, entry, stack_base)
}

// ── Classes and instances ───────────────────────────────────────────────────

/// Find a registered class id by name.
fn find_class(name: &str) -> Option<u32> {
    CLASSES.with(|c| {
        c.borrow()
            .iter()
            .position(|m| m.name == name)
            .map(|i| i as u32)
    })
}

/// Read a copy of a class's metadata by id.
fn class_meta(id: u32) -> Option<ClassMeta> {
    CLASSES.with(|c| c.borrow().get(id as usize).cloned())
}

/// Resolve a method name to its subroutine index, walking the superclass chain
/// so an inherited (or overriding) method is found. A subclass entry shadows its
/// super's, giving virtual dispatch (the most-derived definition wins).
fn lookup_method(class: u32, method: &str) -> Option<u16> {
    let mut cur = Some(class);
    while let Some(id) = cur {
        let meta = class_meta(id)?;
        if let Some(idx) = meta.methods.get(method) {
            return Some(*idx);
        }
        cur = meta.superclass.as_deref().and_then(find_class);
    }
    None
}

/// The class-id chain from a root ancestor down to `class` (inclusive). Used to
/// materialise inherited fields and run inherited field initializers in the
/// correct (superclass-first) order.
fn class_chain(class: u32) -> Vec<u32> {
    let mut chain = Vec::new();
    let mut cur = Some(class);
    while let Some(id) = cur {
        chain.push(id);
        cur = class_meta(id).and_then(|m| m.superclass.as_deref().and_then(find_class));
    }
    chain.reverse(); // root ancestor first
    chain
}

/// Invoke a user method `method` on instance `recv` (implicit `this`), resolving
/// it through the superclass chain. Returns `None` when `recv` is not an instance
/// or its class defines no such method (so the caller can fall back), `Some(Err)`
/// when the body faults.
fn call_user_method(
    vm: &mut VM,
    recv: &Value,
    method: &str,
    args: &[Value],
) -> Option<Result<Value, String>> {
    let inst = as_instance(recv)?;
    let idx = lookup_method(inst.class, method)?;
    let mut pushes = Vec::with_capacity(args.len() + 1);
    pushes.push(recv.clone());
    pushes.extend_from_slice(args);
    Some(invoke_sub(vm, idx, &pushes))
}

/// If `v` is a heap instance, return a clone of it (class id + fields).
fn as_instance(v: &Value) -> Option<Instance> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::Instance(inst)) => Some(inst.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// Write a field on a heap instance in place (mutating the heap object so the
/// change is visible through every handle to it — Groovy objects are references).
fn set_instance_field(v: &Value, field: &str, val: Value) -> bool {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow_mut().get_mut(*id as usize) {
            Some(HeapObj::Instance(inst)) => {
                inst.fields.insert(field.to_string(), val);
                true
            }
            _ => false,
        }),
        _ => false,
    }
}

/// Uppercase the first character (`x` → `X`) for Groovy's getter/setter naming
/// (`getX`/`setX`).
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// `GCLASS`: register a class declaration. Stack (deepest first): name, the
/// field-name array, the method table, the field-initializer table, and the
/// constructor table on top.
fn b_class(vm: &mut VM, _argc: u8) -> Value {
    let ctors_h = vm.stack.pop().unwrap_or(Value::Undef);
    let inits_h = vm.stack.pop().unwrap_or(Value::Undef);
    let methods_h = vm.stack.pop().unwrap_or(Value::Undef);
    let fields_a = vm.stack.pop().unwrap_or(Value::Undef);
    let name = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    // The superclass name (empty string ⇒ root class), pushed first by
    // `register_class`, so it is popped last.
    let super_name = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let superclass = (!super_name.is_empty()).then_some(super_name);

    let field_names: Vec<String> = match fields_a {
        Value::Array(a) => a.iter().map(|v| v.as_str_cow().into_owned()).collect(),
        _ => Vec::new(),
    };
    let methods: std::collections::HashMap<String, u16> = match methods_h {
        Value::Hash(h) => h.into_iter().map(|(k, v)| (k, v.to_int() as u16)).collect(),
        _ => std::collections::HashMap::new(),
    };
    // Preserve declaration order of initialized fields by walking field_names.
    let init_map: std::collections::HashMap<String, u16> = match inits_h {
        Value::Hash(h) => h.into_iter().map(|(k, v)| (k, v.to_int() as u16)).collect(),
        _ => std::collections::HashMap::new(),
    };
    let field_inits: Vec<(String, u16)> = field_names
        .iter()
        .filter_map(|f| init_map.get(f).map(|idx| (f.clone(), *idx)))
        .collect();
    let ctors: std::collections::HashMap<u8, u16> = match ctors_h {
        Value::Hash(h) => h
            .into_iter()
            .filter_map(|(k, v)| k.parse::<u8>().ok().map(|a| (a, v.to_int() as u16)))
            .collect(),
        _ => std::collections::HashMap::new(),
    };
    CLASSES.with(|c| {
        c.borrow_mut().push(ClassMeta {
            name,
            superclass,
            field_names,
            field_inits,
            methods,
            ctors,
        })
    });
    Value::Undef
}

/// `GNEW`: construct `new C(args)`. Stack: `argc` constructor args (deepest),
/// class name on top.
fn b_new(vm: &mut VM, argc: u8) -> Value {
    let name = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let n = argc as usize;
    let mut args = Vec::with_capacity(n);
    for _ in 0..n {
        args.push(vm.stack.pop().unwrap_or(Value::Undef));
    }
    args.reverse();
    let Some(cid) = find_class(&name) else {
        fault(vm, format!("unable to resolve class {name}"));
        return Value::Undef;
    };
    // Materialise every field across the superclass chain (root → leaf), each
    // defaulting to null — an inherited field is a real field of the instance.
    let chain = class_chain(cid);
    let mut fields = std::collections::HashMap::new();
    for id in &chain {
        if let Some(m) = class_meta(*id) {
            for f in &m.field_names {
                fields.insert(f.clone(), Value::Undef);
            }
        }
    }
    let handle = heap_push(HeapObj::Instance(Instance { class: cid, fields }));
    // Run field initializers superclass-first so a subclass initializer can rely
    // on inherited state.
    for id in &chain {
        let Some(m) = class_meta(*id) else { continue };
        for (fname, init_idx) in &m.field_inits {
            match invoke_sub(vm, *init_idx, &[]) {
                Ok(v) => {
                    if pending_exc() {
                        return Value::Undef;
                    }
                    set_instance_field(&handle, fname, v);
                }
                Err(e) => {
                    fault(vm, e);
                    return Value::Undef;
                }
            }
        }
    }
    // Constructor dispatch. The most-derived (leaf) class owns construction: a
    // matching-arity ctor runs (and may itself invoke `super(...)`). Constructors
    // are not inherited, so a subclass with its own ctors but none of this arity
    // is an error; a subclass with no ctors at all gets Groovy's implicit default
    // constructor, which chains to the superclass's no-arg ctor.
    let meta = class_meta(cid).unwrap();
    // A throwable with no matching script-declared constructor uses the modeled
    // JDK pair `T()` / `T(String message)`, which is what a Groovy script means
    // by `new Exception("boom")` or `class Plain extends RuntimeException {}`.
    if !meta.ctors.contains_key(&argc) && is_throwable_class(cid) && argc <= 1 {
        if let Some(m) = args.first() {
            set_instance_field(&handle, "message", Value::str(groovy_str(m)));
        }
        return handle;
    }
    if let Some(ctor_idx) = meta.ctors.get(&argc) {
        let mut pushes = Vec::with_capacity(n + 1);
        pushes.push(handle.clone());
        pushes.extend(args);
        if let Err(e) = invoke_sub(vm, *ctor_idx, &pushes) {
            fault(vm, e);
            return Value::Undef;
        }
    } else if !meta.ctors.is_empty() {
        fault(
            vm,
            format!("groovyrs: no constructor for {name} taking {argc} argument(s)"),
        );
        return Value::Undef;
    } else if argc == 0 {
        // Implicit default constructor: run the nearest ancestor's no-arg ctor.
        if let Err(e) = run_implicit_super_ctor(vm, &handle, cid) {
            fault(vm, e);
            return Value::Undef;
        }
    } else {
        fault(
            vm,
            format!("groovyrs: no constructor for {name} taking {argc} argument(s)"),
        );
        return Value::Undef;
    }
    handle
}

/// Run the implicit superclass constructor for a class with no declared ctors:
/// walk up to the nearest ancestor that declares a no-arg constructor and run it
/// on `handle` (that ctor may itself chain further via `super(...)`). An ancestor
/// that has constructors but no no-arg one is an error (Groovy cannot supply the
/// missing arguments).
fn run_implicit_super_ctor(vm: &mut VM, handle: &Value, class: u32) -> Result<(), String> {
    let mut cur = class_meta(class).and_then(|m| m.superclass.as_deref().and_then(find_class));
    while let Some(id) = cur {
        let m = class_meta(id).ok_or("groovyrs: broken class chain")?;
        if let Some(idx) = m.ctors.get(&0) {
            invoke_sub(vm, *idx, std::slice::from_ref(handle))?;
            return Ok(());
        }
        if !m.ctors.is_empty() {
            return Err(format!(
                "groovyrs: superclass {} has no no-argument constructor",
                m.name
            ));
        }
        cur = m.superclass.as_deref().and_then(find_class);
    }
    Ok(())
}

/// `GSUPER_METHOD`: `super.method(args)`. Stack: `this` (deepest), `argc` args,
/// method name, superclass name (top). Resolves `method` from the superclass
/// upward — skipping the current class's override, which is what `super` means —
/// and invokes it against `this`.
fn b_super_method(vm: &mut VM, argc: u8) -> Value {
    let super_name = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let method = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let n = argc as usize;
    let mut args = Vec::with_capacity(n);
    for _ in 0..n {
        args.push(vm.stack.pop().unwrap_or(Value::Undef));
    }
    args.reverse();
    let this = vm.stack.pop().unwrap_or(Value::Undef);
    let Some(super_id) = find_class(&super_name) else {
        fault(
            vm,
            format!("groovyrs: unable to resolve superclass {super_name}"),
        );
        return Value::Undef;
    };
    let Some(idx) = lookup_method(super_id, &method) else {
        fault(
            vm,
            format!("groovyrs: no such method `{method}` on {super_name}"),
        );
        return Value::Undef;
    };
    let mut pushes = Vec::with_capacity(n + 1);
    pushes.push(this);
    pushes.extend(args);
    match invoke_sub(vm, idx, &pushes) {
        Ok(v) => v,
        Err(e) => {
            fault(vm, e);
            Value::Undef
        }
    }
}

/// `GSUPER_CTOR`: `super(args)`. Stack: `this` (deepest), `argc` args, superclass
/// name (top). Runs the superclass's arity-matched constructor on `this` (which
/// may itself chain further via `super(...)`).
fn b_super_ctor(vm: &mut VM, argc: u8) -> Value {
    let super_name = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let n = argc as usize;
    let mut args = Vec::with_capacity(n);
    for _ in 0..n {
        args.push(vm.stack.pop().unwrap_or(Value::Undef));
    }
    args.reverse();
    let this = vm.stack.pop().unwrap_or(Value::Undef);
    let Some(super_id) = find_class(&super_name) else {
        fault(
            vm,
            format!("groovyrs: unable to resolve superclass {super_name}"),
        );
        return Value::Undef;
    };
    let Some(idx) = class_meta(super_id).and_then(|m| m.ctors.get(&argc).copied()) else {
        // `super(message)` into the built-in throwable chain — the modeled
        // `Throwable(String)` constructor, which a user exception class calls.
        if is_throwable_class(super_id) && argc <= 1 {
            if let Some(m) = args.first() {
                set_instance_field(&this, "message", Value::str(groovy_str(m)));
            }
            return Value::Undef;
        }
        fault(
            vm,
            format!("groovyrs: no constructor for {super_name} taking {argc} argument(s)"),
        );
        return Value::Undef;
    };
    let mut pushes = Vec::with_capacity(n + 1);
    pushes.push(this);
    pushes.extend(args);
    match invoke_sub(vm, idx, &pushes) {
        Ok(_) => Value::Undef,
        Err(e) => {
            fault(vm, e);
            Value::Undef
        }
    }
}

/// `GINSTANCEOF`: `value instanceof Class`. Stack: value (deepest), class name
/// (top). True when `value` is a user instance whose class chain contains the
/// named class, or when the named class is a built-in type the value matches.
/// `null instanceof X` is always false (Groovy).
fn b_instanceof(vm: &mut VM, _argc: u8) -> Value {
    let class = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let value = vm.stack.pop().unwrap_or(Value::Undef);
    Value::bool(value_is_a(&value, &class))
}

/// Whether `value` is an instance of the (user or built-in) type `class`.
fn value_is_a(value: &Value, class: &str) -> bool {
    // `null` is never an instance of anything.
    if matches!(value, Value::Undef) {
        return false;
    }
    // A user class instance: the named class must appear in its superclass chain.
    if let Some(inst) = as_instance(value) {
        if let Some(target) = find_class(class) {
            return class_chain(inst.class).contains(&target);
        }
        // Named type is not a user class — fall through to built-in checks (an
        // instance is still an `Object`/`GroovyObject`).
    }
    // Built-in Groovy/Java types (short or common fully-qualified names).
    let short = class.rsplit('.').next().unwrap_or(class);
    match short {
        "Object" | "GroovyObject" => true,
        "String" | "CharSequence" | "GString" => matches!(value, Value::Str(_)),
        "Integer" | "Int" | "Long" | "Short" | "Byte" => matches!(value, Value::Int(_)),
        "BigDecimal" | "Double" | "Float" | "BigInteger" => matches!(value, Value::Float(_)),
        "Number" => matches!(value, Value::Int(_) | Value::Float(_)),
        "Boolean" => matches!(value, Value::Bool(_)),
        "List" | "ArrayList" | "Collection" | "Iterable" => matches!(value, Value::Array(_)),
        "Map" | "LinkedHashMap" | "HashMap" => {
            matches!(value, Value::Hash(_)) || as_omap(value).is_some()
        }
        _ => false,
    }
}

/// Dispatch a method call on a class instance: a user method (implicit `this`),
/// else Groovy's auto getter/setter over a field. Returns `None` when `recv` is
/// not an instance (so the caller falls through to closure/GDK dispatch).
fn dispatch_instance_method(
    vm: &mut VM,
    recv: &Value,
    method: &str,
    args: &[Value],
) -> Option<Result<Value, String>> {
    let inst = as_instance(recv)?;
    let meta = class_meta(inst.class)?;
    // Virtual dispatch: resolve the method most-derived-first through the chain.
    if let Some(idx) = lookup_method(inst.class, method) {
        let mut pushes = Vec::with_capacity(args.len() + 1);
        pushes.push(recv.clone());
        pushes.extend_from_slice(args);
        return Some(invoke_sub(vm, idx, &pushes));
    }
    // Auto getter `getX()` / setter `setX(v)` over a field.
    if let Some(field) = method.strip_prefix("get") {
        let key = lower_first(field);
        if inst.fields.contains_key(&key) {
            return Some(Ok(inst.fields.get(&key).cloned().unwrap_or(Value::Undef)));
        }
    }
    if let Some(field) = method.strip_prefix("set") {
        let key = lower_first(field);
        if inst.fields.contains_key(&key) {
            let v = args.first().cloned().unwrap_or(Value::Undef);
            set_instance_field(recv, &key, v);
            return Some(Ok(Value::Undef));
        }
    }
    // The `Throwable` methods a script actually calls, for the modeled built-in
    // hierarchy (a user override was already found by `lookup_method` above).
    if is_throwable_class(inst.class) {
        match method {
            "toString" => return Some(Ok(Value::str(throwable_str(recv)))),
            "getLocalizedMessage" => {
                return Some(Ok(inst
                    .fields
                    .get("message")
                    .cloned()
                    .unwrap_or(Value::Undef)))
            }
            _ => {}
        }
    }
    Some(Err(format!(
        "groovyrs: no such method `{method}` on {}",
        meta.name
    )))
}

/// Lowercase the first character (`X` → `x`) — the inverse of [`capitalize`],
/// used to map a `getX`/`setX` accessor back to its field name.
fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Read a property on a class instance: a user `getX()` getter if defined, else
/// the field, else fault. `None` when `recv` is not an instance.
fn dispatch_instance_prop_get(
    vm: &mut VM,
    recv: &Value,
    name: &str,
) -> Option<Result<Value, String>> {
    let inst = as_instance(recv)?;
    let meta = class_meta(inst.class)?;
    let getter = format!("get{}", capitalize(name));
    if let Some(idx) = lookup_method(inst.class, &getter) {
        return Some(invoke_sub(vm, idx, std::slice::from_ref(recv)));
    }
    if inst.fields.contains_key(name) {
        return Some(Ok(inst.fields.get(name).cloned().unwrap_or(Value::Undef)));
    }
    Some(Err(format!(
        "groovyrs: no such property `{name}` on {}",
        meta.name
    )))
}

/// `GSETPROP`: assign `recv.name = value`. Stack: receiver (deepest), value,
/// property name on top. Honours a user `setX` setter, else writes the field.
fn b_setprop(vm: &mut VM, _argc: u8) -> Value {
    let name = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let value = vm.stack.pop().unwrap_or(Value::Undef);
    let recv = vm.stack.pop().unwrap_or(Value::Undef);
    if let Some(inst) = as_instance(&recv) {
        {
            let setter = format!("set{}", capitalize(&name));
            if let Some(idx) = lookup_method(inst.class, &setter) {
                return match invoke_sub(vm, idx, &[recv.clone(), value]) {
                    Ok(_) => Value::Undef,
                    Err(e) => {
                        fault(vm, e);
                        Value::Undef
                    }
                };
            }
        }
        set_instance_field(&recv, &name, value.clone());
        return value;
    }
    // `map.k = v` mutates the ordered map in place (through its shared handle).
    if omap_set(&recv, name.clone(), value.clone()) {
        return value;
    }
    fault(
        vm,
        format!(
            "groovyrs: cannot set property `{name}` on {}",
            type_name(&recv)
        ),
    );
    Value::Undef
}

/// `GINDEX`: read `recv[index]`. Dispatches a user `getAt(index)` on an instance,
/// else a list/map/string element (Groovy allows a negative list index).
fn b_index(vm: &mut VM, _argc: u8) -> Value {
    let index = vm.stack.pop().unwrap_or(Value::Undef);
    let recv = vm.stack.pop().unwrap_or(Value::Undef);
    if as_instance(&recv).is_some() {
        return match dispatch_instance_method(vm, &recv, "getAt", &[index]) {
            Some(Ok(v)) => v,
            Some(Err(e)) => {
                fault(vm, e);
                Value::Undef
            }
            None => Value::Undef,
        };
    }
    if let Some(entries) = as_omap(&recv) {
        let k = index.as_str_cow().into_owned();
        return entries
            .iter()
            .find(|(ek, _)| *ek == k)
            .map(|(_, v)| v.clone())
            .unwrap_or(Value::Undef);
    }
    match &recv {
        Value::Array(a) => {
            let i = index.to_int();
            let idx = if i < 0 { a.len() as i64 + i } else { i };
            if idx < 0 {
                Value::Undef
            } else {
                a.get(idx as usize).cloned().unwrap_or(Value::Undef)
            }
        }
        Value::Hash(h) => h
            .get(&index.as_str_cow().into_owned())
            .cloned()
            .unwrap_or(Value::Undef),
        Value::Str(s) => {
            let i = index.to_int();
            let chars: Vec<char> = s.chars().collect();
            let idx = if i < 0 { chars.len() as i64 + i } else { i };
            if idx < 0 {
                Value::Undef
            } else {
                chars
                    .get(idx as usize)
                    .map(|c| Value::str(c.to_string()))
                    .unwrap_or(Value::Undef)
            }
        }
        _ => {
            fault(vm, format!("groovyrs: cannot index {}", type_name(&recv)));
            Value::Undef
        }
    }
}

/// `GCLOSURE_CALL`: invoke a closure directly (`f(args)`). Stack: the closure
/// (deepest), `argc` args, then the callee name on top.
fn b_closure_call(vm: &mut VM, argc: u8) -> Value {
    let name = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let n = argc as usize;
    let mut args = Vec::with_capacity(n);
    for _ in 0..n {
        args.push(vm.stack.pop().unwrap_or(Value::Undef));
    }
    args.reverse();
    let clo = vm.stack.pop().unwrap_or(Value::Undef);
    if closure_meta(&clo).is_none() {
        // Faithful to the compile-time diagnostic the non-closure path replaced.
        fault(vm, format!("unresolved reference: {name}"));
        return Value::Undef;
    }
    match invoke_closure(vm, &clo, &args) {
        Ok(v) => v,
        Err(e) => {
            fault(vm, e);
            Value::Undef
        }
    }
}

/// `GMETHOD_SAFE`: the safe-navigation method call `recv?.method(args)`. Returns
/// `null` without dispatching when the receiver is `null`; otherwise identical to
/// [`b_method`].
fn b_method_safe(vm: &mut VM, argc: u8) -> Value {
    let name = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let n = argc as usize;
    let mut args = Vec::with_capacity(n);
    for _ in 0..n {
        args.push(vm.stack.pop().unwrap_or(Value::Undef));
    }
    args.reverse();
    let recv = vm.stack.pop().unwrap_or(Value::Undef);
    if matches!(recv, Value::Undef) {
        return Value::Undef;
    }
    dispatch_call(vm, recv, &name, args)
}

/// `GPROP_SAFE`: the safe-navigation property read `recv?.name`. Returns `null`
/// when the receiver is `null`; otherwise identical to [`b_prop`].
fn b_prop_safe(vm: &mut VM, _argc: u8) -> Value {
    let name = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let recv = vm.stack.pop().unwrap_or(Value::Undef);
    if matches!(recv, Value::Undef) {
        return Value::Undef;
    }
    if let Some(res) = dispatch_instance_prop_get(vm, &recv, &name) {
        return match res {
            Ok(v) => v,
            Err(e) => {
                fault(vm, e);
                Value::Undef
            }
        };
    }
    match dispatch_property(&recv, &name) {
        Ok(v) => v,
        Err(e) => {
            fault(vm, e);
            Value::Undef
        }
    }
}

thread_local! {
    /// Parks a runtime fault raised inside a builtin (a `rust { ... }` block that
    /// fails to compile, or a call to an unregistered FFI export) so the CLI can
    /// surface it as `groovyrs: <reason>` after `VM::run` returns. A builtin
    /// cannot return a `Result`, so it halts the VM and leaves the message here.
    static G_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Take and clear any pending runtime-fault message (see `G_ERROR`).
pub fn take_error() -> Option<String> {
    G_ERROR.with(|e| e.borrow_mut().take())
}

/// Record a fault message and halt the VM; the runtime reports it once
/// [`VM::run`] returns.
fn fault(vm: &mut VM, msg: impl Into<String>) {
    G_ERROR.with(|e| *e.borrow_mut() = Some(msg.into()));
    vm.request_halt();
}

/// `__rust_compile` builtin: pop the base64-encoded `rust { ... }` block body and
/// compile + register its exported functions through `fusevm::ffi`. Returns
/// `null` (the desugared call is evaluated for its side effect).
fn b_ffi_compile(vm: &mut VM, _argc: u8) -> Value {
    let body = vm.stack.pop().unwrap_or(Value::Undef);
    let b64 = body.as_str_cow().into_owned();
    if let Err(e) = fusevm::ffi::compile_and_register(&b64) {
        fault(vm, format!("rust {{}} block: {e}"));
    }
    Value::Undef
}

/// FFI-call builtin: the stack holds the args (deepest first) with the callee
/// name (a `String`) on top; `argc` is the argument count. Dispatches through
/// `fusevm::ffi::try_call` and returns the exported function's result.
fn b_ffi_call(vm: &mut VM, argc: u8) -> Value {
    let name = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let n = argc as usize;
    let mut args = Vec::with_capacity(n);
    for _ in 0..n {
        args.push(vm.stack.pop().unwrap_or(Value::Undef));
    }
    args.reverse();
    match fusevm::ffi::try_call(&name, &args) {
        Some(Ok(v)) => v,
        Some(Err(e)) => {
            fault(vm, format!("rust FFI call {name}: {e}"));
            Value::Undef
        }
        None => {
            fault(vm, format!("unresolved reference: {name}"));
            Value::Undef
        }
    }
}

/// Groovy method-call builtin: the stack holds the receiver (deepest), `argc`
/// args, and the method name (a `String`) on top. Dispatches a faithful GDK
/// subset via `dispatch_method`.
fn b_method(vm: &mut VM, argc: u8) -> Value {
    let name = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let n = argc as usize;
    let mut args = Vec::with_capacity(n);
    for _ in 0..n {
        args.push(vm.stack.pop().unwrap_or(Value::Undef));
    }
    args.reverse();
    let recv = vm.stack.pop().unwrap_or(Value::Undef);
    dispatch_call(vm, recv, &name, args)
}

/// Dispatch `recv.method(args)`, trying the closure-consuming operations first
/// (they re-enter the VM to run a closure body) and falling back to the pure GDK
/// dispatch. Shared by [`b_method`] and [`b_method_safe`].
fn dispatch_call(vm: &mut VM, recv: Value, method: &str, args: Vec<Value>) -> Value {
    // A method on a class instance: a user method (implicit `this`) or Groovy's
    // auto getter/setter over a field. Checked first — an instance handle is a
    // `Value::Obj`, the same tag closures use.
    if let Some(res) = dispatch_instance_method(vm, &recv, method, &args) {
        return match res {
            Ok(v) => v,
            Err(e) => {
                fault(vm, e);
                Value::Undef
            }
        };
    }
    // `clo.call(args)` — invoke the receiver closure.
    if method == "call" && closure_meta(&recv).is_some() {
        return match invoke_closure(vm, &recv, &args) {
            Ok(v) => v,
            Err(e) => {
                fault(vm, e);
                Value::Undef
            }
        };
    }
    // `x.toString()` on a value with no user `toString` renders it the way
    // `println` does (`[1, 2, 3]`, `[a:1]`, `1.50`, `java.lang.Exception: boom`).
    if method == "toString" && args.is_empty() {
        return Value::str(render_value(vm, &recv));
    }
    // Closure-consuming list/range iteration (`each`/`collect`/`findAll`/…).
    if let Value::Array(items) = &recv {
        if let Some(res) = dispatch_iteration(vm, items, method, &args) {
            return match res {
                Ok(v) => v,
                Err(e) => {
                    fault(vm, e);
                    Value::Undef
                }
            };
        }
    }
    // Pure GDK dispatch — no closure, no VM re-entrancy.
    match dispatch_method(&recv, method, &args) {
        Ok(v) => v,
        Err(e) => {
            fault(vm, e);
            Value::Undef
        }
    }
}

/// The closure-driven GDK collection methods over a list (or a materialised
/// range): `each`, `collect`, `findAll`, `find`, `inject`, `sum`. Returns `None`
/// when `method` is not one of these (so the caller falls back to the pure GDK
/// dispatch), else the faithful result (or a fault message).
fn dispatch_iteration(
    vm: &mut VM,
    items: &[Value],
    method: &str,
    args: &[Value],
) -> Option<Result<Value, String>> {
    match method {
        // `list.each { it -> ... }` — run the closure for its side effects on
        // each element; the list itself is returned.
        "each" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            for it in items {
                if let Err(e) = invoke_closure(vm, clo, std::slice::from_ref(it)) {
                    return Some(Err(e));
                }
                // A `throw` inside the closure unwound out of its body; stop
                // iterating so the exception reaches the caller's handler.
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
            }
            Some(Ok(Value::array(items.to_vec())))
        }
        // `list.eachWithIndex { it, i -> ... }` — element and 0-based index.
        "eachWithIndex" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            for (i, it) in items.iter().enumerate() {
                let call_args = [it.clone(), Value::int(i as i64)];
                if let Err(e) = invoke_closure(vm, clo, &call_args) {
                    return Some(Err(e));
                }
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
            }
            Some(Ok(Value::array(items.to_vec())))
        }
        // `list.collect { it -> ... }` — map to a new list of closure results.
        "collect" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                match invoke_closure(vm, clo, std::slice::from_ref(it)) {
                    Ok(v) => out.push(v),
                    Err(e) => return Some(Err(e)),
                }
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
            }
            Some(Ok(Value::array(out)))
        }
        // `list.findAll { it -> pred }` — keep the elements the closure accepts.
        "findAll" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            let mut out = Vec::new();
            for it in items {
                match invoke_closure(vm, clo, std::slice::from_ref(it)) {
                    Ok(v) => {
                        if pending_exc() {
                            return Some(Ok(Value::Undef));
                        }
                        if groovy_truthy(vm, &v) {
                            out.push(it.clone());
                        }
                    }
                    Err(e) => return Some(Err(e)),
                }
            }
            Some(Ok(Value::array(out)))
        }
        // `list.find { it -> pred }` — first accepted element, else `null`.
        "find" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            for it in items {
                match invoke_closure(vm, clo, std::slice::from_ref(it)) {
                    Ok(v) => {
                        if pending_exc() {
                            return Some(Ok(Value::Undef));
                        }
                        if groovy_truthy(vm, &v) {
                            return Some(Ok(it.clone()));
                        }
                    }
                    Err(e) => return Some(Err(e)),
                }
            }
            Some(Ok(Value::Undef))
        }
        // `list.inject(initial) { acc, val -> ... }` folds left. The one-arg
        // form `inject { acc, val -> ... }` seeds with the first element.
        "inject" => {
            let (clo, mut acc, start) = match args {
                [seed, clo] if closure_meta(clo).is_some() => (clo, seed.clone(), 0),
                [clo] if closure_meta(clo).is_some() => {
                    match items.first() {
                        Some(first) => (clo, first.clone(), 1),
                        // Groovy: `[].inject(clo)` yields null.
                        None => return Some(Ok(Value::Undef)),
                    }
                }
                _ => return None,
            };
            for it in &items[start..] {
                let call_args = [acc, it.clone()];
                match invoke_closure(vm, clo, &call_args) {
                    Ok(v) => acc = v,
                    Err(e) => return Some(Err(e)),
                }
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
            }
            Some(Ok(acc))
        }
        // `list.sum()` adds the elements; `list.sum { it -> ... }` sums the
        // closure results. An empty list sums to `null` (Groovy).
        "sum" => {
            let clo = args.last().filter(|a| closure_meta(a).is_some());
            let mut acc: Option<Value> = None;
            for it in items {
                let v = match clo {
                    Some(c) => match invoke_closure(vm, c, std::slice::from_ref(it)) {
                        Ok(v) => {
                            if pending_exc() {
                                return Some(Ok(Value::Undef));
                            }
                            v
                        }
                        Err(e) => return Some(Err(e)),
                    },
                    None => it.clone(),
                };
                acc = Some(match acc {
                    Some(a) => groovy_sum_add(&a, &v),
                    None => v,
                });
            }
            Some(Ok(acc.unwrap_or(Value::Undef)))
        }
        _ => None,
    }
}

/// Add two values for `sum`: integer addition stays integral, a decimal operand
/// keeps `BigDecimal` scale, and a `double` operand promotes to a double
/// (Groovy's numeric-tower `+`).
fn groovy_sum_add(a: &Value, b: &Value) -> Value {
    if let (Some(x), Some(y)) = (as_i64(a), as_i64(b)) {
        return Value::int(x + y);
    }
    if let Some(Ok(v)) = decimal_operator(NumOp::Add, a, b) {
        return v;
    }
    Value::float(as_f64(a) + as_f64(b))
}

/// Groovy property-read builtin: the stack holds the receiver then the property
/// name on top. Dispatches via [`dispatch_property`].
fn b_prop(vm: &mut VM, _argc: u8) -> Value {
    let name = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let recv = vm.stack.pop().unwrap_or(Value::Undef);
    if let Some(res) = dispatch_instance_prop_get(vm, &recv, &name) {
        return match res {
            Ok(v) => v,
            Err(e) => {
                fault(vm, e);
                Value::Undef
            }
        };
    }
    match dispatch_property(&recv, &name) {
        Ok(v) => v,
        Err(e) => {
            fault(vm, e);
            Value::Undef
        }
    }
}

/// The element/character count of a Groovy value: characters for a `String`,
/// element count for a list, entry count for a map.
fn value_size(v: &Value) -> i64 {
    match v {
        Value::Str(s) => s.chars().count() as i64,
        Value::Array(a) => a.len() as i64,
        Value::Hash(h) => h.len() as i64,
        _ => as_omap(v).map(|m| m.len() as i64).unwrap_or(0),
    }
}

/// Dispatch a faithful subset of the Groovy GDK for `recv.method(args)`. Unknown
/// combinations raise a `groovyrs: ...` runtime fault rather than mis-running.
fn dispatch_method(recv: &Value, method: &str, args: &[Value]) -> Result<Value, String> {
    match (recv, method) {
        // Universal size query (String chars / list elements / map entries).
        (_, "size") => Ok(Value::int(value_size(recv))),

        // ── String ──
        (Value::Str(s), "length") => Ok(Value::int(s.chars().count() as i64)),
        (Value::Str(s), "toUpperCase") => Ok(Value::str(s.to_uppercase())),
        (Value::Str(s), "toLowerCase") => Ok(Value::str(s.to_lowercase())),
        (Value::Str(s), "trim") => Ok(Value::str(s.trim().to_string())),
        (Value::Str(s), "reverse") => Ok(Value::str(s.chars().rev().collect::<String>())),
        (Value::Str(s), "isEmpty") => Ok(Value::bool(s.is_empty())),
        (Value::Str(s), "contains") => {
            let needle = args.first().map(groovy_str).unwrap_or_default();
            Ok(Value::bool(s.contains(&needle)))
        }

        // ── List ──
        (Value::Array(a), "isEmpty") => Ok(Value::bool(a.is_empty())),
        (Value::Array(a), "contains") => {
            let want = args.first().cloned().unwrap_or(Value::Undef);
            Ok(Value::bool(
                a.iter().any(|v| groovy_str(v) == groovy_str(&want)),
            ))
        }
        (Value::Array(a), "get") => {
            let i = args.first().and_then(as_i64).unwrap_or(0);
            Ok(a.get(i.max(0) as usize).cloned().unwrap_or(Value::Undef))
        }
        (Value::Array(a), "reverse") => {
            let mut r = a.clone();
            r.reverse();
            Ok(Value::array(r))
        }

        // ── Map ──
        (Value::Hash(h), "isEmpty") => Ok(Value::bool(h.is_empty())),
        (Value::Hash(h), "containsKey") => {
            let k = args.first().map(groovy_str).unwrap_or_default();
            Ok(Value::bool(h.contains_key(&k)))
        }

        // ── BigDecimal (host heap) ──
        _ if as_dec(recv).is_some() => {
            let d = as_dec(recv).unwrap();
            match method {
                "toString" => Ok(Value::str(decimal::to_groovy_string(&d))),
                "abs" => Ok(dec_value(decimal::abs(&d))),
                "negate" => Ok(dec_value(decimal::neg(&d))),
                "toBigDecimal" => Ok(recv.clone()),
                // Truncating conversions; `round` goes to the nearest integer.
                "intValue" | "longValue" | "toInteger" | "toLong" => {
                    Ok(Value::int(decimal::truncate_to_i64(&d)))
                }
                "round" => Ok(Value::int(decimal::round_to_i64(&d))),
                "doubleValue" | "toDouble" | "floatValue" | "toFloat" => {
                    Ok(Value::float(decimal::to_f64(&d)))
                }
                _ => Err(format!("groovyrs: no such method `{method}` on BigDecimal")),
            }
        }

        // ── Ordered map (host heap) ──
        _ if as_omap(recv).is_some() => {
            let entries = as_omap(recv).unwrap();
            match method {
                "isEmpty" => Ok(Value::bool(entries.is_empty())),
                "containsKey" => {
                    let k = args.first().map(groovy_str).unwrap_or_default();
                    Ok(Value::bool(entries.iter().any(|(ek, _)| *ek == k)))
                }
                "get" => {
                    let k = args.first().map(groovy_str).unwrap_or_default();
                    Ok(entries
                        .iter()
                        .find(|(ek, _)| *ek == k)
                        .map(|(_, v)| v.clone())
                        .unwrap_or(Value::Undef))
                }
                "keySet" | "keys" => Ok(Value::array(
                    entries.iter().map(|(k, _)| Value::str(k.clone())).collect(),
                )),
                "values" => Ok(Value::array(entries.into_iter().map(|(_, v)| v).collect())),
                _ => Err(format!("groovyrs: no such method `{method}` on Map")),
            }
        }

        _ => Err(format!(
            "groovyrs: no such method `{method}` on {}",
            type_name(recv)
        )),
    }
}

/// Dispatch a Groovy property read `recv.name`. Supports the `size`/`length`
/// count properties on `String`/list/map; a map's `k` also reads entry `k`.
fn dispatch_property(recv: &Value, name: &str) -> Result<Value, String> {
    match (recv, name) {
        (_, "size") | (_, "length") => Ok(Value::int(value_size(recv))),
        // Groovy map property access reads the entry of that key (`m.k` == `m['k']`).
        (Value::Hash(h), key) => Ok(h.get(key).cloned().unwrap_or(Value::Undef)),
        _ => {
            // An ordered-map handle: `m.k` reads entry `k` (null if absent).
            if let Some(entries) = as_omap(recv) {
                return Ok(entries
                    .iter()
                    .find(|(ek, _)| ek == name)
                    .map(|(_, v)| v.clone())
                    .unwrap_or(Value::Undef));
            }
            Err(format!(
                "groovyrs: no such property `{name}` on {}",
                type_name(recv)
            ))
        }
    }
}

/// A short Groovy-ish type name for diagnostics.
fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Str(_) => "String",
        Value::Array(_) => "List",
        Value::Hash(_) => "Map",
        Value::Obj(_) if as_dec(v).is_some() => "BigDecimal",
        Value::Obj(_) if as_omap(v).is_some() => "Map",
        Value::Obj(_) if as_instance(v).is_some() => "Object",
        Value::Int(_) => "Integer",
        // Only a `d`/`f`-suffixed literal is an IEEE double; an unsuffixed
        // decimal is a `BigDecimal` on the heap (above).
        Value::Float(_) => "Double",
        Value::Bool(_) => "Boolean",
        Value::Undef => "null",
        _ => "Object",
    }
}

/// `println` builtin: pop `argc` values (0 or 1 in slice 1), print them
/// Groovy-formatted followed by a newline, and return `null`.
fn b_println(vm: &mut VM, argc: u8) -> Value {
    print_args(vm, argc, true)
}

/// `print` builtin: as [`b_println`] but with no trailing newline.
fn b_print(vm: &mut VM, argc: u8) -> Value {
    print_args(vm, argc, false)
}

fn print_args(vm: &mut VM, argc: u8, newline: bool) -> Value {
    use std::io::Write;
    // Pop the args (pushed left-to-right, so the last is on top) and restore
    // source order.
    let mut vals = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        vals.push(vm.stack.pop().unwrap_or(Value::Undef));
    }
    vals.reverse();
    // Render each value; a class instance prints through its `toString()` when
    // the class defines one (Groovy's `println` calls `toString`).
    // A `toString()` that threw leaves an exception in flight and a placeholder
    // rendering; printing that would emit a spurious `null` before the handler
    // runs, so the write is skipped and the caller's post-call check picks the
    // exception up. An exception that was *already* in flight means this
    // `println` is inside a `finally` running on the unwind path — that output
    // is real and must still appear.
    let already_unwinding = pending_exc();
    let rendered: Vec<String> = vals.iter().map(|v| render_value(vm, v)).collect();
    if !already_unwinding && pending_exc() {
        return Value::Undef;
    }
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    for s in &rendered {
        let _ = write!(lock, "{s}");
    }
    if newline {
        let _ = writeln!(lock);
    }
    // `println`/`print` are `void`; the CallBuiltin result is discarded by a
    // trailing Pop in statement position.
    Value::Undef
}

/// Groovy `/` division builtin. Pops two operands (`a / b`) and applies Groovy's
/// `BigDecimal`-promoting semantics: two integers divide exactly to an integer
/// when there is no remainder (`4/2 → 2`) and to a decimal otherwise
/// (`7/2 → 3.5`); any decimal operand forces decimal division (`10.0/4 → 2.5`).
fn b_div(vm: &mut VM, _argc: u8) -> Value {
    let b = vm.stack.pop().unwrap_or(Value::Undef);
    let a = vm.stack.pop().unwrap_or(Value::Undef);
    // User-class `/` overload: Groovy dispatches `a / b` as `a.div(b)`. `/` lowers
    // to this builtin (not the numeric hook), so a class `div` method is resolved
    // here, with the `&mut VM` this builtin already holds. A non-instance `a` (or
    // a class without `div`) falls through to native decimal division below.
    if let Some(res) = call_user_method(vm, &a, "div", std::slice::from_ref(&b)) {
        return match res {
            Ok(v) => v,
            Err(e) => {
                fault(vm, e);
                Value::Undef
            }
        };
    }
    // Groovy divides two integers exactly when it can (`4/2` is the Integer 2)
    // and promotes to `BigDecimal` otherwise (`7/2` is 3.5, `1/3` is
    // 0.3333333333) — never to a double.
    if let (Some(x), Some(y)) = (as_i64(&a), as_i64(&b)) {
        if y != 0 && x % y == 0 {
            return Value::int(x / y);
        }
    }
    // A `double` operand keeps the IEEE path, where `5.0d / 0.0d` is Infinity.
    if matches!(a, Value::Float(_)) || matches!(b, Value::Float(_)) {
        return Value::float(as_f64(&a) / as_f64(&b));
    }
    match (as_exact_dec(&a), as_exact_dec(&b)) {
        (Some(x), Some(y)) => match decimal::divide(&x, &y) {
            Some(q) => dec_value(q),
            None => {
                // Groovy raises `java.lang.ArithmeticException: Division by
                // zero`, which a script can catch; unarmed this degrades to the
                // hard fault it has always been (see [`raise`]).
                raise(vm, "ArithmeticException", DIVISION_BY_ZERO);
                Value::Undef
            }
        },
        // A non-numeric operand: no Groovy meaning for `/`.
        _ => {
            fault(
                vm,
                format!(
                    "groovyrs: operator `/` is not defined for operands `{}` and `{}`",
                    groovy_str(&a),
                    groovy_str(&b)
                ),
            );
            Value::Undef
        }
    }
}

/// `GCMP`: Groovy `<=>`. Pops `a <=> b`. A user-class instance left operand
/// dispatches `compareTo` (Groovy returns its raw `int`); otherwise a numeric
/// pair compares numerically and any other pair by Groovy string ordering, both
/// yielding the sign `-1`/`0`/`1`. Byte-verified against Apache Groovy 5.0.7.
fn b_cmp(vm: &mut VM, _argc: u8) -> Value {
    let b = vm.stack.pop().unwrap_or(Value::Undef);
    let a = vm.stack.pop().unwrap_or(Value::Undef);
    if let Some(res) = call_user_method(vm, &a, "compareTo", std::slice::from_ref(&b)) {
        return match res {
            Ok(v) => v,
            Err(e) => {
                fault(vm, e);
                Value::Undef
            }
        };
    }
    // A decimal operand compares exactly (scale-insensitively); other numbers
    // compare as doubles; anything else by Groovy string ordering.
    let ord = match (as_dec(&a).is_some() || as_dec(&b).is_some())
        .then(|| (as_exact_dec(&a), as_exact_dec(&b)))
    {
        Some((Some(x), Some(y))) => Some(decimal::cmp(&x, &y)),
        _ => match (as_num(&a), as_num(&b)) {
            (Some(x), Some(y)) => x.partial_cmp(&y),
            _ => Some(groovy_str(&a).cmp(&groovy_str(&b))),
        },
    };
    match ord {
        Some(std::cmp::Ordering::Less) => Value::int(-1),
        Some(std::cmp::Ordering::Greater) => Value::int(1),
        _ => Value::int(0),
    }
}

/// A numeric view of a value (`Int`/`Float`/`Bool`/`BigDecimal`), or `None` for
/// a non-number.
fn as_num(v: &Value) -> Option<f64> {
    match v {
        Value::Int(n) => Some(*n as f64),
        Value::Float(f) => Some(*f),
        Value::Bool(b) => Some(*b as i64 as f64),
        _ => as_dec(v).map(|d| decimal::to_f64(&d)),
    }
}

/// An integer view of a value, or `None` if it is a float/non-number.
fn as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Int(n) => Some(*n),
        Value::Bool(b) => Some(*b as i64),
        _ => None,
    }
}

/// A float view of a value, for the paths that must run on IEEE doubles.
fn as_f64(v: &Value) -> f64 {
    match v {
        Value::Int(n) => *n as f64,
        Value::Float(f) => *f,
        Value::Bool(b) => *b as i64 as f64,
        _ => as_dec(v).map(|d| decimal::to_f64(&d)).unwrap_or(f64::NAN),
    }
}

/// Render a value for output, invoking a class instance's `toString()` (Groovy's
/// `println` prints an object through `toString`). Collections render their
/// elements the same way. Everything else defers to [`groovy_str`] (which has no
/// VM and so cannot dispatch a method). `default_instance_str` covers an instance
/// whose class defines no `toString`.
fn render_value(vm: &mut VM, v: &Value) -> String {
    if let Some(inst) = as_instance(v) {
        return instance_to_string(vm, v).unwrap_or_else(|| instance_default_str(v, &inst));
    }
    if let Some(entries) = as_omap(v) {
        if entries.is_empty() {
            return "[:]".to_string();
        }
        let items: Vec<String> = entries
            .iter()
            .map(|(k, val)| format!("{k}:{}", render_value(vm, val)))
            .collect();
        return format!("[{}]", items.join(", "));
    }
    match v {
        Value::Array(a) => {
            let items: Vec<String> = a.iter().map(|e| render_value(vm, e)).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Hash(h) if !h.is_empty() => {
            let items: Vec<String> = h
                .iter()
                .map(|(k, val)| format!("{k}:{}", render_value(vm, val)))
                .collect();
            format!("[{}]", items.join(", "))
        }
        _ => groovy_str(v),
    }
}

/// Invoke a class instance's `toString()` and return its rendered value, if the
/// class defines one.
fn instance_to_string(vm: &mut VM, recv: &Value) -> Option<String> {
    let inst = as_instance(recv)?;
    let idx = lookup_method(inst.class, "toString")?;
    match invoke_sub(vm, idx, std::slice::from_ref(recv)) {
        Ok(v) => Some(groovy_str(&v)),
        Err(e) => {
            fault(vm, e);
            Some(String::new())
        }
    }
}

/// The fallback rendering for an instance whose class defines no `toString`:
/// the class name (Groovy's default is `Class@hexhash`, but the hash is not
/// reproducible, so groovyrs prints the class name deterministically).
fn default_instance_str(inst: &Instance) -> String {
    class_meta(inst.class)
        .map(|m| m.name)
        .unwrap_or_else(|| "Object".to_string())
}

/// Render an instance that defines no `toString`: a throwable prints
/// `java.lang.Exception: boom` (Groovy's `Throwable.toString`), any other object
/// its class name.
fn instance_default_str(v: &Value, inst: &Instance) -> String {
    if is_throwable_class(inst.class) {
        return throwable_str(v);
    }
    default_instance_str(inst)
}

/// Render a value with Groovy's `println`/`toString` rules (as opposed to
/// fusevm's shell-flavoured `as_str_cow`): booleans as `true`/`false`, whole
/// decimals with a trailing `.0`, `Undef`/`null` as `null`.
pub fn groovy_str(v: &Value) -> String {
    // A decimal handle renders through `BigDecimal.toString` — trailing zeros
    // kept, `E+n` form outside the plain-notation window.
    if let Some(d) = as_dec(v) {
        return decimal::to_groovy_string(&d);
    }
    // An ordered-map handle renders `[k:v, …]` in insertion order (`[:]` empty).
    if let Some(entries) = as_omap(v) {
        if entries.is_empty() {
            return "[:]".to_string();
        }
        let items: Vec<String> = entries
            .iter()
            .map(|(k, val)| format!("{k}:{}", groovy_str(val)))
            .collect();
        return format!("[{}]", items.join(", "));
    }
    match v {
        Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Value::Float(f) => decimal::format_double(*f),
        Value::Undef => "null".to_string(),
        // Groovy renders a list as `[a, b, c]` and a map as `[k:v, ...]` (the
        // empty map as `[:]`); collection elements print with the same rules
        // (strings appear unquoted). NOTE: `Value::Hash` is an unordered
        // `HashMap`, so a multi-entry map's print order is not Groovy's
        // insertion order — single-entry maps render faithfully.
        Value::Array(a) => {
            let items: Vec<String> = a.iter().map(groovy_str).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Hash(h) => {
            if h.is_empty() {
                return "[:]".to_string();
            }
            let items: Vec<String> = h
                .iter()
                .map(|(k, v)| format!("{k}:{}", groovy_str(v)))
                .collect();
            format!("[{}]", items.join(", "))
        }
        other => other.as_str_cow().into_owned(),
    }
}

/// The fault Groovy raises for a zero divisor (`java.lang.ArithmeticException:
/// Division by zero`), which aborts the script exactly as an uncaught exception
/// does.
const DIVISION_BY_ZERO: &str = "Division by zero";

/// Groovy arithmetic and comparison when a `BigDecimal` is involved. Returns
/// `None` when the operation is not decimal arithmetic — no decimal operand, or
/// a `String` operand, where `+` concatenates and comparisons compare printed
/// forms through the hook's default paths.
///
/// Mixing a decimal with a `double` widens to `double` (Groovy: `1.0 + 1.0d` is
/// a `Double`); every other numeric mix stays exact, with `Integer` read at
/// scale 0.
fn decimal_operator(op: NumOp, a: &Value, b: &Value) -> Option<Result<Value, String>> {
    if matches!(op, NumOp::Neg) {
        return Some(Ok(dec_value(decimal::neg(&as_dec(a)?))));
    }
    if as_dec(a).is_none() && as_dec(b).is_none() {
        return None;
    }
    if matches!(a, Value::Str(_)) || matches!(b, Value::Str(_)) {
        return None;
    }
    // Groovy keeps exactly one mixed case exact: a `BigDecimal % Double` reads
    // the double as its full binary expansion and stays a `BigDecimal`
    // (`1.5 % 0.555d` is `0.38999999999999990230…0781250`). The mirrored
    // `Double % BigDecimal`, and every other mixed operation, widens to double.
    if matches!(op, NumOp::Mod) {
        if let (Some(x), Value::Float(f)) = (as_dec(a), b) {
            if let Some(y) = decimal::from_f64_exact(*f) {
                return Some(match decimal::remainder(&x, &y) {
                    Some(r) => Ok(dec_value(r)),
                    None => Err(DIVISION_BY_ZERO.to_string()),
                });
            }
        }
    }
    if matches!(a, Value::Float(_)) || matches!(b, Value::Float(_)) {
        return Some(Ok(double_operator(op, as_f64(a), as_f64(b))));
    }
    let (x, y) = (as_exact_dec(a)?, as_exact_dec(b)?);
    let ordering = || decimal::cmp(&x, &y);
    let result = match op {
        NumOp::Add => dec_value(decimal::add(&x, &y)),
        NumOp::Sub => dec_value(decimal::sub(&x, &y)),
        NumOp::Mul => dec_value(decimal::mul(&x, &y)),
        // `/` lowers to the `GDIV` builtin, but the hook still handles it for
        // completeness (an operand pair fusevm delegates directly).
        NumOp::Div => match decimal::divide(&x, &y) {
            Some(q) => dec_value(q),
            None => return Some(Err(DIVISION_BY_ZERO.to_string())),
        },
        NumOp::Mod => match decimal::remainder(&x, &y) {
            Some(r) => dec_value(r),
            None => return Some(Err(DIVISION_BY_ZERO.to_string())),
        },
        // Groovy raises a decimal to an integer power exactly; a negative or
        // fractional exponent falls back to `double`.
        NumOp::Pow => match decimal::to_i64(&y).and_then(|e| decimal::pow(&x, e)) {
            Some(p) => dec_value(p),
            None => Value::float(decimal::to_f64(&x).powf(decimal::to_f64(&y))),
        },
        NumOp::Eq => Value::bool(ordering().is_eq()),
        NumOp::Ne => Value::bool(ordering().is_ne()),
        NumOp::Lt => Value::bool(ordering().is_lt()),
        NumOp::Gt => Value::bool(ordering().is_gt()),
        NumOp::Le => Value::bool(ordering().is_le()),
        NumOp::Ge => Value::bool(ordering().is_ge()),
        NumOp::Neg => unreachable!("unary negation returns above"),
    };
    Some(Ok(result))
}

/// The same operator set on two IEEE doubles, for a decimal/`double` mix.
fn double_operator(op: NumOp, x: f64, y: f64) -> Value {
    match op {
        NumOp::Add => Value::float(x + y),
        NumOp::Sub => Value::float(x - y),
        NumOp::Mul => Value::float(x * y),
        NumOp::Div => Value::float(x / y),
        NumOp::Mod => Value::float(x % y),
        NumOp::Pow => Value::float(x.powf(y)),
        NumOp::Eq => Value::bool(x == y),
        NumOp::Ne => Value::bool(x != y),
        NumOp::Lt => Value::bool(x < y),
        NumOp::Gt => Value::bool(x > y),
        NumOp::Le => Value::bool(x <= y),
        NumOp::Ge => Value::bool(x >= y),
        NumOp::Neg => Value::float(-x),
    }
}

/// Groovy `+` on a non-numeric left operand, dispatched on the left value
/// (Groovy dispatches `+` as `left.plus(right)`): a list concatenates another
/// list or appends a scalar; an ordered map merges another map (right wins on a
/// duplicate key, insertion order preserved); anything else concatenates as a
/// string.
fn groovy_add(a: &Value, b: &Value) -> Value {
    if let Value::Array(xs) = a {
        let mut out = xs.clone();
        match b {
            Value::Array(ys) => out.extend(ys.iter().cloned()),
            other => out.push(other.clone()),
        }
        return Value::array(out);
    }
    if let Some(mut entries) = as_omap(a) {
        if let Some(rhs) = as_omap(b) {
            for (k, v) in rhs {
                match entries.iter_mut().find(|(ek, _)| *ek == k) {
                    Some(slot) => slot.1 = v,
                    None => entries.push((k, v)),
                }
            }
        }
        return heap_push(HeapObj::OrderedMap(entries));
    }
    // String concatenation renders each side the way `println` does, so a class
    // instance operand goes through its `toString()`. Both operands are cloned
    // before the VM re-entry that dispatch needs (see [`with_vm`]).
    let (x, y) = (a.clone(), b.clone());
    let joined = with_vm(|vm| format!("{}{}", render_value(vm, &x), render_value(vm, &y)))
        .unwrap_or_else(|| format!("{}{}", groovy_str(a), groovy_str(b)));
    Value::str(joined)
}

/// The Groovy method a binary/unary arithmetic operator dispatches to on a
/// user-class instance (byte-verified against Apache Groovy 5.0.7: `%` maps to
/// `remainder`, `**` to `power`, unary `-` to `negative`). `/` is handled in
/// [`b_div`], comparisons/equality in [`instance_operator`], so they are absent.
fn arith_method(op: NumOp) -> Option<&'static str> {
    match op {
        NumOp::Add => Some("plus"),
        NumOp::Sub => Some("minus"),
        NumOp::Mul => Some("multiply"),
        NumOp::Mod => Some("remainder"),
        NumOp::Pow => Some("power"),
        NumOp::Neg => Some("negative"),
        _ => None,
    }
}

/// Dispatch a Groovy operator on a user-class instance left operand. Arithmetic
/// (`+`/`-`/`*`/`%`/`**`/unary `-`) calls the mapped method strictly (a missing
/// method faults, as Groovy raises `MissingMethodException`); `==`/`!=` go through
/// [`instance_equals`]; ordered comparisons through `compareTo`. Returns `None`
/// when the operator has no instance meaning here — an ordered comparison on a
/// class without `compareTo` — so the hook's default (string comparison) applies.
fn instance_operator(op: NumOp, a: &Value, b: &Value) -> Option<Result<Value, String>> {
    // Clone the operands before any VM re-entry (see [`with_vm`] SAFETY note).
    let recv = a.clone();
    let rhs = b.clone();
    match op {
        NumOp::Add | NumOp::Sub | NumOp::Mul | NumOp::Mod | NumOp::Pow => {
            let m = arith_method(op)?;
            Some(dispatch_operator_method(&recv, m, &[rhs]))
        }
        NumOp::Neg => Some(dispatch_operator_method(&recv, "negative", &[])),
        NumOp::Eq | NumOp::Ne => Some(
            instance_equals(&recv, &rhs)
                .map(|eq| Value::bool(if matches!(op, NumOp::Eq) { eq } else { !eq })),
        ),
        NumOp::Lt | NumOp::Gt | NumOp::Le | NumOp::Ge => match instance_compare(&recv, &rhs) {
            Some(Ok(c)) => {
                let r = match op {
                    NumOp::Lt => c < 0,
                    NumOp::Gt => c > 0,
                    NumOp::Le => c <= 0,
                    NumOp::Ge => c >= 0,
                    _ => unreachable!(),
                };
                Some(Ok(Value::bool(r)))
            }
            Some(Err(e)) => Some(Err(e)),
            // No `compareTo` — defer to the hook's default string comparison.
            None => None,
        },
        // `/` lowers to the GDIV builtin and never reaches the hook.
        NumOp::Div => None,
    }
}

/// Invoke an operator overload method on `recv`, re-entering the VM. A missing
/// method faults with the same `no such method` diagnostic the GDK dispatch uses
/// (Groovy signals `MissingMethodException` for an undefined operator method).
fn dispatch_operator_method(recv: &Value, method: &str, args: &[Value]) -> Result<Value, String> {
    match with_vm(|vm| call_user_method(vm, recv, method, args)) {
        Some(Some(res)) => res,
        // The class does not define the operator method.
        Some(None) => {
            let name = as_instance(recv)
                .and_then(|i| class_meta(i.class))
                .map(|m| m.name)
                .unwrap_or_else(|| "Object".to_string());
            Err(format!("groovyrs: no such method `{method}` on {name}"))
        }
        // No VM published — only possible if the hook fired outside a run.
        None => Err("groovyrs: operator overload dispatched with no active VM".to_string()),
    }
}

/// Groovy `==`/`!=` on a user-class instance. Null-safe (an instance is never
/// `== null`); a class implementing `Comparable` (modeled here as defining
/// `compareTo`) compares equal when `compareTo` is `0`; otherwise a user `equals`
/// decides; with neither, equality is object identity (the shared heap handle).
/// Byte-verified against Apache Groovy 5.0.7.
fn instance_equals(a: &Value, b: &Value) -> Result<bool, String> {
    // Groovy `==` is null-safe: a non-null instance never equals null.
    if matches!(b, Value::Undef) {
        return Ok(false);
    }
    let Some(inst) = as_instance(a) else {
        return Ok(false);
    };
    // Comparable → equality is `compareTo(...) == 0`.
    if lookup_method(inst.class, "compareTo").is_some() {
        return match instance_compare(a, b) {
            Some(res) => res.map(|c| c == 0),
            None => Ok(false),
        };
    }
    // A user `equals(Object)` decides.
    if let Some(res) =
        with_vm(|vm| call_user_method(vm, a, "equals", std::slice::from_ref(b))).flatten()
    {
        return res.map(|v| v.is_truthy());
    }
    // No `compareTo`/`equals`: default `Object` identity — the same heap handle.
    Ok(matches!((a, b), (Value::Obj(x), Value::Obj(y)) if x == y))
}

/// Invoke a user `compareTo` and return its `int` result, for `<`/`>`/`<=`/`>=`
/// and Comparable-based `==`. `None` when the class defines no `compareTo` (so
/// an ordered comparison falls back to the hook's default).
fn instance_compare(a: &Value, b: &Value) -> Option<Result<i64, String>> {
    let inst = as_instance(a)?;
    lookup_method(inst.class, "compareTo")?;
    let res =
        with_vm(|vm| call_user_method(vm, a, "compareTo", std::slice::from_ref(b))).flatten()?;
    Some(res.map(|v| v.to_int()))
}

/// Strict numeric hook: fusevm calls this only for an operation with a
/// non-numeric operand — Groovy's `+` overload (list concat / map merge / string
/// concatenation) and value comparisons against strings. All-numeric arithmetic
/// never reaches here (it stays on the native fast path and the JIT). `/` never
/// reaches here — it lowers to the [`GDIV`] builtin instead.
pub fn numeric_hook(op: NumOp, a: &Value, b: &Value) -> Result<Value, String> {
    // User-class operator overloading. Groovy dispatches an operator on its LEFT
    // operand as a method call (`a + b` == `a.plus(b)`, `a > b` == `a.compareTo(b)
    // > 0`, `a == b` via `equals`/`compareTo`). Only a class-instance left operand
    // routes here; primitive `Int`/`Float`/`String` arithmetic stays on the native
    // and JIT fast paths and never reaches this hook. `/` is absent — it lowers to
    // the [`GDIV`] builtin, where the `div` overload is dispatched instead.
    if as_instance(a).is_some() {
        if let Some(res) = instance_operator(op, a, b) {
            return res;
        }
    }
    // Decimal arithmetic. A `BigDecimal` is a host-heap handle, so fusevm sees a
    // non-numeric operand and delegates every `+`/`-`/`*`/`%`/`**`/comparison on
    // one to this hook — which is exactly where Groovy's scale rules belong.
    if let Some(res) = decimal_operator(op, a, b) {
        return res;
    }
    match op {
        // Groovy `+` dispatches on the left operand: list concatenation/append,
        // map merge, else string concatenation.
        NumOp::Add => Ok(groovy_add(a, b)),
        // Groovy `==`/`!=` are value equality (`.equals`), not reference
        // identity — comparing string/boolean operands by value is faithful.
        NumOp::Eq => Ok(Value::bool(groovy_str(a) == groovy_str(b))),
        NumOp::Ne => Ok(Value::bool(groovy_str(a) != groovy_str(b))),
        NumOp::Lt => Ok(Value::bool(groovy_str(a) < groovy_str(b))),
        NumOp::Gt => Ok(Value::bool(groovy_str(a) > groovy_str(b))),
        NumOp::Le => Ok(Value::bool(groovy_str(a) <= groovy_str(b))),
        NumOp::Ge => Ok(Value::bool(groovy_str(a) >= groovy_str(b))),
        // Arithmetic other than `+` on a non-numeric operand has no slice-1
        // meaning (`String.minus`/`multiply` GDK overloads are not modeled yet).
        NumOp::Sub | NumOp::Mul | NumOp::Div | NumOp::Mod | NumOp::Pow => Err(format!(
            "groovyrs: operator `{op:?}` is not defined for operands `{}` and `{}`",
            groovy_str(a),
            groovy_str(b)
        )),
        NumOp::Neg => Err(format!(
            "groovyrs: unary `-` is not defined for `{}`",
            groovy_str(a)
        )),
    }
}

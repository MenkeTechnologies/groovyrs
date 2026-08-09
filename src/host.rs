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
use bigdecimal::{BigDecimal, Zero};
use fusevm::{Frame, NumOp, VMResult, Value, VM};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

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
/// `groovy_truthy` computes for it. Emitted only in front of a condition whose
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

/// `switch` case matching — Groovy's `isCase`, which is *not* `==`: a `Range` or
/// list label contains, a closure label is called, a `Pattern` label matches the
/// whole subject, and anything else compares equal. Stack: subject (deepest),
/// then the label; pushes a `Boolean`.
pub const GIS_CASE: u16 = 737;

/// `switch` case matching against a *type* label (`case String:`). Stack:
/// subject (deepest), then the type name as a `String`; pushes a `Boolean`. A
/// separate builtin because a class name is not a loadable value — the compiler
/// resolves the name statically (`compiler::names_a_type`).
pub const GIS_CASE_TYPE: u16 = 738;

/// Build a `java.util.regex.Pattern` from a `~/…/` literal's source. Stack: the
/// pattern text; pushes the compiled pattern handle.
pub const GREGEX: u16 = 739;

/// Builtin id for materialising a `java.math.BigInteger` literal. The stack
/// holds the literal's digits (a `String`); returns the handle of its value.
pub const GBIGINT: u16 = 759;

/// Groovy's `=~` find operator. Stack: the subject (deepest), then the pattern;
/// pushes a `java.util.regex.Matcher` positioned before the first match.
pub const GMATCH: u16 = 757;

/// Groovy's `==~` match operator. Stack: the subject (deepest), then the
/// pattern; pushes a `Boolean` — whether the pattern matches the whole subject.
pub const GMATCH_FULL: u16 = 758;

/// Clear the power-assert value recorder, at the top of an `assert`.
pub const GASSERT_START: u16 = 740;

/// Record one `assert` sub-expression's value. Stack: the value (deepest), then
/// its 1-based source column; the value is pushed back so it flows on.
pub const GASSERT_REC: u16 = 741;

/// Raise the `AssertionError` for a failed `assert`. Stack: the `: message`
/// operand (`null` when absent), the condition's source text, then the
/// comma-joined `Values:` variable names.
pub const GASSERT_FAIL: u16 = 742;

/// Builtin id for Groovy `%` on a divisor the compiler's guard found to be zero.
/// Stack: dividend, divisor. Emitted *only* behind that guard, so `%` keeps the
/// native `Op::Mod` (and its JIT trace) on every non-zero divisor.
pub const GMOD: u16 = 743;

/// Builtin id for materialising `for (x in <expr>)`'s sequence. Stack: the
/// value; pushes the list of elements Groovy iterates over.
pub const GITER: u16 = 744;

/// Builtin id for choosing what a self-mutating GDK call writes back to its
/// variable receiver. Stack: the call's result, then the receiver's *current*
/// value; pushes the result when the receiver is a list (Groovy's `List.sort` /
/// `List.unique` mutate in place) and the receiver unchanged otherwise (a map's
/// `sort` returns a new map and leaves the receiver alone).
pub const GWRITEBACK: u16 = 745;

/// Builtin id for Groovy's `**` power operator. Stack: base, then exponent.
/// Not a native op because the result type follows Groovy's numeric tower
/// (`2 ** 10` is an `Integer`, `2 ** -1` and `2.0 ** 3` are `BigDecimal`s).
pub const GPOWER: u16 = 746;

/// Builtin id for `<<`, which Groovy defines as `leftShift`: a bit shift on a
/// number, an append on a list, a concatenation on a string.
pub const GSHL: u16 = 747;

/// Builtin id for `>>>`, Java's zero-filling right shift. It has no native op
/// because the fill width follows the operand's Java type (32 bits for an
/// `Integer`, 64 for a `Long`).
pub const GUSHR: u16 = 748;

/// Builtin id for `x in coll` — Groovy's membership test, which is the
/// collection's `isCase`/`contains`.
pub const GIN: u16 = 749;

/// Builtin id for `value as Type` — Groovy's `asType` coercion. Stack: the
/// value, then the type name.
pub const GCAST: u16 = 750;

/// Builtin id for materialising a JDK class reference from its name, so that
/// `Math.max(…)` and `Integer.MAX_VALUE` have a receiver to dispatch on. Stack:
/// the simple class name; pushes a `java.lang.Class` handle.
pub const GCLASSREF: u16 = 751;

/// Builtin id for a subscript assignment `recv[index] = value` — Groovy's
/// `putAt`. Stack: receiver, index, value; pushes the receiver's new contents
/// (a list is a fusevm value, so the caller stores them back).
pub const GSETINDEX: u16 = 752;

/// Builtin ids for the script-scope `printf(format, args…)` (prints) and
/// `sprintf(format, args…)` (answers the string). Stack: the format string,
/// then the arguments.
pub const GPRINTF: u16 = 753;
pub const GSPRINTF: u16 = 754;

/// Builtin id for building a range literal. Stack: start, end, and the
/// inclusive flag. Groovy counts down when the start exceeds the end and
/// enumerates characters for single-character string endpoints.
pub const GRANGE: u16 = 755;

/// Builtin id for `getClass()` / `.class` on a receiver the compiler knows is a
/// `Long`. Stack: the value. See `b_class_long`.
pub const GCLASS_LONG: u16 = 756;

/// Builtin ids for reading and writing a bare name inside a closure body that
/// the *owner* could not resolve — the name form of what [`b_closure_call`]
/// already does for a bare call. Emitted only for a name the compiler could not
/// tie to a slot, a field, a class, a function, or a script-level declaration,
/// so an ordinary variable keeps its native `GetVar`/`SetVar` (and with it its
/// JIT trace eligibility). Stack for the read: the global's name-pool index,
/// then the name. For the write: the value, the index, then the name.
/// See [`b_name_get`] / [`b_name_set`].
pub const GNAME_GET: u16 = 760;
pub const GNAME_SET: u16 = 761;

/// Builtin id for a list literal: takes the `Value::Array` that `Op::MakeArray`
/// just built and registers it as a `java.util.ArrayList` handle.
///
/// A Groovy list is a *reference* — `def b = a` names one `ArrayList` twice — so
/// a literal has to allocate a handle rather than leave a fusevm array on the
/// stack. `Op::MakeArray` still does the element gathering; this only wraps it.
/// See [`b_make_list`].
pub const GMAKE_LIST: u16 = 762;

/// Builtin id for `>>` on a receiver whose `rightShift` is *not* a bit shift —
/// a closure (forward composition, `f >> g` is `x -> g(f(x))`) or a user-class
/// instance with a `rightShift` overload. Stack: the two operands, then the
/// statically-known `Long` width of the left one (as [`GSHL`] takes it), which
/// only the numeric fallback consults.
///
/// Unlike `<<`, `>>` is *not* routed here unconditionally: an `Integer` `>>` is
/// six native ops the tracing JIT records (see `Compiler::binary`), and sending
/// every shift through a builtin would cost each shifting loop its trace. The
/// compiler emits this only where it can see the left operand is a closure or an
/// instance, so an ordinary shift is untouched. See [`b_shr`].
pub const GSHR: u16 = 763;

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
    // Groovy's default integer is a 32-bit `Integer`, so arithmetic that leaves
    // that range is the interesting case and has to reach the host — see the
    // `Integer` width commentary further down. Arithmetic that stays inside it
    // (all of it, in the ordinary program) never consults this.
    vm.set_fixnum_range(i64::from(i32::MIN), i64::from(i32::MAX));
    vm.register_builtin(GPRINTLN, b_println);
    vm.register_builtin(GPRINT, b_print);
    vm.register_builtin(GDIV, b_div);
    vm.register_builtin(GMOD, b_mod);
    vm.register_builtin(GITER, b_iter);
    vm.register_builtin(GWRITEBACK, b_writeback);
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
    vm.register_builtin(GMAKE_LIST, b_make_list);
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
    vm.register_builtin(GIS_CASE, b_is_case);
    vm.register_builtin(GIS_CASE_TYPE, b_is_case_type);
    vm.register_builtin(GREGEX, b_regex);
    vm.register_builtin(GBIGINT, b_bigint);
    vm.register_builtin(GMATCH, b_match);
    vm.register_builtin(GMATCH_FULL, b_match_full);
    vm.register_builtin(GASSERT_START, b_assert_start);
    vm.register_builtin(GASSERT_REC, b_assert_rec);
    vm.register_builtin(GASSERT_FAIL, b_assert_fail);
    vm.register_builtin(GPOWER, b_power);
    vm.register_builtin(GSHL, b_shl);
    vm.register_builtin(GSHR, b_shr);
    vm.register_builtin(GUSHR, b_ushr);
    vm.register_builtin(GIN, b_in);
    vm.register_builtin(GCAST, b_cast);
    vm.register_builtin(GCLASSREF, b_classref);
    vm.register_builtin(GSETINDEX, b_setindex);
    vm.register_builtin(GRANGE, b_range);
    vm.register_builtin(GCLASS_LONG, b_class_long);
    vm.register_builtin(GPRINTF, b_printf);
    vm.register_builtin(GSPRINTF, b_sprintf);
    vm.register_builtin(GNAME_GET, b_name_get);
    vm.register_builtin(GNAME_SET, b_name_set);
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
    /// The `(source column, value)` pairs recorded while evaluating the current
    /// `assert` condition — what Groovy's power-assert rendering lays out under
    /// the condition's source text. Cleared at each `assert`.
    static ASSERT_VALUES: RefCell<Vec<(u32, Value)>> = const { RefCell::new(Vec::new()) };
    /// The new contents a self-mutating GDK list call produced, parked for the
    /// compiler-emitted writeback that follows it (see [`b_writeback`]).
    ///
    /// fusevm's `Value::Array` is a value, not a handle, so `list.add(4)` cannot
    /// mutate its receiver through the argument it was given. The call's *result*
    /// is not the new list either (`add` answers `true`), so the new contents ride
    /// this slot instead: the GDK arm stores them, and the writeback op that the
    /// compiler emits for a variable receiver takes them back out.
    static MUTATED: RefCell<Option<Value>> = const { RefCell::new(None) };
    /// Results already computed by a `clo.memoize()` closure, keyed by that
    /// closure's heap id and the Groovy rendering of its arguments. Cleared with
    /// the heap.
    static MEMO: RefCell<HashMap<(u32, String), Value>> = RefCell::new(HashMap::new());
    /// The default-value closure a map got from `map.withDefault { … }`, keyed by
    /// the map's heap id. A missing-key read then answers that closure's result
    /// and stores it, which is what `groovy.lang.MapWithDefault` does.
    static MAP_DEFAULTS: RefCell<HashMap<u32, Value>> = RefCell::new(HashMap::new());
    /// The `with`/`tap` receivers whose closures are currently running, innermost
    /// last — Groovy's closure *delegate* chain. A bare call inside the closure
    /// that the owner (the script) cannot resolve is dispatched against the
    /// innermost delegate that answers it, which is Groovy's `OWNER_FIRST`
    /// resolve strategy: the compiler has already tried the owner by the time a
    /// name reaches [`b_closure_call`] as a non-closure.
    ///
    /// A list is a fusevm *value*, so a mutator called on the delegate
    /// (`[1, 2].tap { add(3) }`) parks its new contents in [`MUTATED`]; the
    /// dispatch writes them back into the slot here, which is what `tap` then
    /// answers.
    static DELEGATES: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
}

// ── Integer width (`Integer` vs `Long`) ────────────────────────────────────
//
// Groovy's integer arithmetic wraps at the width of its operands: `Integer op
// Integer` is 32-bit and anything involving a `Long` is 64-bit, so
// `1000000 * 1000000` is `-727379968` while `1000000L * 1000000` is
// `1000000000000`. fusevm has one integer type, so the width has to come from
// somewhere else. It comes from two sources, in this order:
//
//   1. The compiler, which marks the op indices whose operands it can see are
//      `Long` (`crate::compiler::Compiler::is_wide`). This is the only thing
//      that can tell `2000000000L` from `2000000000` — the same `Value::Int` —
//      and so the only thing that makes a `long` accumulator accumulate.
//   2. The operands' magnitudes, for everything the compiler cannot see into: a
//      value outside `Integer` range *is* a `Long`, whatever produced it. This
//      is what makes the arithmetic inside a closure wrap like Groovy's.
//
// `VM::set_fixnum_range` is what routes the question here at all: with the range
// set to 32 bits, every `+`/`-`/`*`/negation whose result leaves `Integer` range
// delegates to [`sited_numeric_hook`], and everything inside it stays on the
// native and JIT'd fast paths at no cost.

thread_local! {
    /// The op indices of statically-`Long` arithmetic in the running chunk, from
    /// the compiler. Replaced on each compile.
    static WIDE_SITES: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
}

/// Publish the compiler's statically-`Long` arithmetic sites for this chunk.
pub fn set_wide_sites(sites: HashSet<usize>) {
    WIDE_SITES.with(|s| *s.borrow_mut() = sites);
}

/// Whether the op at `ip` was compiled as `Long` arithmetic.
fn wide_site(ip: usize) -> bool {
    WIDE_SITES.with(|s| s.borrow().contains(&ip))
}

/// Whether `a op b` runs at 64 bits: either the compiler saw a `Long` operand
/// at this site, or one of the values is outside `Integer` range and so is a
/// `Long` no matter what produced it.
fn arith_is_wide(ip: usize, a: &Value, b: &Value) -> bool {
    let out_of_int_range = |v: &Value| matches!(v, Value::Int(n) if i32::try_from(*n).is_err());
    wide_site(ip) || out_of_int_range(a) || out_of_int_range(b)
}

/// Wrap `result` at the width `operand`'s magnitude says it has — 32 bits for
/// an `Integer`, 64 for a `Long`. For the host paths that have an operand but
/// no compiler site to consult.
fn wrap_to_width_of(operand: i64, result: i64) -> i64 {
    if i32::try_from(operand).is_ok() {
        i64::from(result as i32)
    } else {
        result
    }
}

/// `GCLASS_LONG`: `getClass()` / `.class` where the compiler saw a `Long`.
///
/// The magnitude rule in [`java_class_name`] cannot see a `Long` small enough
/// to be an `Integer` — `1L` and `1` are the same `Value::Int` — so `1L.class`
/// asks here instead, where the compiler's static width answers it. Any other
/// value takes the ordinary reading, so a receiver whose width the compiler
/// misjudged still names its real class.
fn b_class_long(vm: &mut VM, _argc: u8) -> Value {
    let v = vm.stack.pop().unwrap_or(Value::Undef);
    if matches!(v, Value::Int(_)) {
        return heap_push(HeapObj::ClassRef("java.lang.Long".to_string()));
    }
    class_ref_of(&v)
}

/// `Math.abs` at the value's own width. The minimum of a two's-complement type
/// has no positive counterpart, so `Math.abs(Integer.MIN_VALUE)` is
/// `Integer.MIN_VALUE` and `Math.abs(Long.MIN_VALUE)` is `Long.MIN_VALUE` —
/// Java returns the wrapped value rather than throwing.
fn abs_at_width(n: i64) -> i64 {
    if n == i64::from(i32::MIN) || n == i64::MIN {
        return n;
    }
    n.abs()
}

/// Java's narrowing conversion to an integral type: keep the target's low bits
/// and sign-extend. `2147483648L as int` is `-2147483648`, `300 as byte` is
/// `44`. A `long`/`Long` target is 64 bits, so it keeps the value whole.
fn narrow_to(ty: &str, n: i64) -> i64 {
    match simple_name_of(ty).as_str() {
        "int" | "Integer" => i64::from(n as i32),
        "short" | "Short" => i64::from(n as i16),
        "byte" | "Byte" => i64::from(n as i8),
        _ => n,
    }
}

/// Pop the static width flag the compiler pushes as a shift's third argument.
///
/// A shift takes its width from the value being shifted, and unlike `+`/`-`/`*`
/// it has no overflow for the magnitude rule to catch: `1L << 32` and `1 << 32`
/// see the identical operands and answer `4294967296` and `1`. So the width
/// here is the compiler's alone.
fn shift_is_wide(vm: &mut VM) -> bool {
    matches!(vm.stack.pop(), Some(Value::Int(1)))
}

/// The Groovy `Integer`/`Long` result of arithmetic that left 32-bit range.
///
/// `wide` is [`arith_is_wide`]. A `Long` keeps the full 64-bit two's-complement
/// result (`Long.MAX_VALUE + 1` is `Long.MIN_VALUE`); an `Integer` narrows to
/// 32 (`Integer.MAX_VALUE + 1` is `Integer.MIN_VALUE`).
fn wrap_to_width(value: i64, wide: bool) -> Value {
    if wide {
        Value::int(value)
    } else {
        Value::int(i64::from(value as i32))
    }
}

/// Groovy's `+`/`-`/`*`/`%`/negation on two integers, wrapped at the operands'
/// own width. Answers `None` for an op or an operand this rule does not cover,
/// which then takes [`numeric_hook`]'s ordinary path.
fn int_arith(op: NumOp, a: &Value, b: &Value, ip: usize) -> Option<Value> {
    let wide = arith_is_wide(ip, a, b);
    if let (NumOp::Neg, Value::Int(x)) = (op, a) {
        return Some(wrap_to_width(x.wrapping_neg(), wide));
    }
    let (Value::Int(x), Value::Int(y)) = (a, b) else {
        return None;
    };
    let (x, y) = (*x, *y);
    let raw = match op {
        NumOp::Add => x.wrapping_add(y),
        NumOp::Sub => x.wrapping_sub(y),
        NumOp::Mul => x.wrapping_mul(y),
        // Only a zero divisor reaches here for `%` (the exact result of a
        // non-zero one is always in range), and that is a `Throwable`, not a
        // width question — leave it to `numeric_hook`.
        _ => return None,
    };
    Some(wrap_to_width(raw, wide))
}

/// The strict numeric hook, told which op index delegated to it.
///
/// The site is what separates an `Integer` overflow from a `Long` one — see the
/// `Integer` width commentary above. Everything else is [`numeric_hook`].
pub fn sited_numeric_hook(call: fusevm::NumericCall<'_>) -> Result<Value, String> {
    if let Some(v) = int_arith(call.op, call.a, call.b, call.ip) {
        return Ok(v);
    }
    numeric_hook(call.op, call.a, call.b)
}

/// Park the new contents of a mutated list receiver for the writeback op.
fn set_mutated(v: Value) {
    MUTATED.with(|m| *m.borrow_mut() = Some(v));
}

/// Take (and clear) any parked mutated-receiver contents.
fn take_mutated() -> Option<Value> {
    MUTATED.with(|m| m.borrow_mut().take())
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
    raise_opt(vm, class, Some(message));
}

/// [`raise`] for a throwable whose message may be *absent*. `java.math`'s
/// `BigDecimal` parser throws a message-less `NumberFormatException` for a few
/// inputs, and `e.getMessage()` then answers `null` — a difference a script can
/// print, so it is modeled rather than flattened to an empty string.
fn raise_opt(vm: &mut VM, class: &str, message: Option<&str>) {
    if EXC_ARMED.with(|a| a.get()) {
        set_pending(new_throwable_opt(class, message));
    } else {
        let name = crate::throwable::qualified(class);
        fault(
            vm,
            match message {
                Some(m) => format!("{name}: {m}"),
                None => name,
            },
        );
    }
}

/// Raise `groovy.lang.MissingMethodException` for an unresolved `recv.method(…)`,
/// with the message Groovy builds: the method name, the receiver's Java class,
/// and the argument types and values. Returns the placeholder the faulting
/// builtin hands back — the compiler's post-call check unwinds before it is read.
fn raise_missing_method(vm: &mut VM, recv: &Value, method: &str, args: &[Value]) -> Value {
    let types = args
        .iter()
        .map(simple_class_name)
        .collect::<Vec<_>>()
        .join(", ");
    let values = args.iter().map(groovy_str).collect::<Vec<_>>().join(", ");
    raise(
        vm,
        "MissingMethodException",
        &format!(
            "No signature of method: {method} for class: {} \
             is applicable for argument types: ({types}) values: [{values}]",
            java_class_name(recv)
        ),
    );
    Value::Undef
}

/// Raise `groovy.lang.MissingPropertyException` for an unresolved `recv.name`.
fn raise_missing_property(vm: &mut VM, recv: &Value, name: &str) -> Value {
    raise(
        vm,
        "MissingPropertyException",
        &format!(
            "No such property: {name} for class: {}",
            java_class_name(recv)
        ),
    );
    Value::Undef
}

/// The fully-qualified Java class name Groovy names in a `MissingMethod` /
/// `MissingProperty` message. A script-declared class prints bare, the way
/// Groovy prints a class with no package.
fn java_class_name(v: &Value) -> String {
    if let Some(inst) = as_instance(v) {
        if let Some(meta) = class_meta(inst.class) {
            return crate::throwable::qualified(&meta.name);
        }
    }
    // An integer outside 32-bit range is a `Long`; inside it, Groovy's default
    // `Integer`. The one case this misreads is a `Long` small enough to be an
    // `Integer` (`1L`), which no longer exists once it is a `Value::Int`.
    if let Value::Int(n) = v {
        return if i32::try_from(*n).is_ok() {
            "java.lang.Integer".to_string()
        } else {
            "java.lang.Long".to_string()
        };
    }
    if let Some(r) = as_range(v) {
        return range_class(&r).to_string();
    }
    if let Some((class, _)) = as_buffer(v) {
        return class.to_string();
    }
    if let Some((_, kind)) = as_set(v) {
        return set_class(kind).to_string();
    }
    if regex_source(v).is_some() {
        return "java.util.regex.Pattern".to_string();
    }
    if as_matcher(v).is_some() {
        return "java.util.regex.Matcher".to_string();
    }
    match v {
        Value::Str(_) => "java.lang.String",
        Value::Float(_) => "java.lang.Double",
        Value::Bool(_) => "java.lang.Boolean",
        Value::Array(_) => "java.util.ArrayList",
        Value::Hash(_) => "java.util.LinkedHashMap",
        Value::Undef => "null",
        _ if as_list(v).is_some() => "java.util.ArrayList",
        _ if as_bigint(v).is_some() => "java.math.BigInteger",
        _ if as_dec(v).is_some() => "java.math.BigDecimal",
        _ if as_omap(v).is_some() => "java.util.LinkedHashMap",
        _ if closure_meta(v).is_some() => "groovy.lang.Closure",
        _ if as_iter(v).is_some() => return as_iter(v).unwrap().0.to_string(),
        _ => "java.lang.Object",
    }
    .to_string()
}

/// The simple class name Groovy lists in a `MissingMethodException`'s
/// `argument types: (…)` — the qualified name's last segment, and `null` for a
/// null argument.
fn simple_class_name(v: &Value) -> String {
    simple_name_of(&java_class_name(v))
}

/// The last segment of a qualified class name (`java.lang.String` → `String`).
fn simple_name_of(qualified: &str) -> String {
    qualified
        .rsplit('.')
        .next()
        .unwrap_or(qualified)
        .to_string()
}

/// Allocate a built-in throwable instance on the host heap. `None` gives it the
/// `null` message a message-less `NumberFormatException` carries.
fn new_throwable_opt(class: &str, message: Option<&str>) -> Value {
    let cid = find_class(class).unwrap_or(0);
    let mut fields = std::collections::HashMap::new();
    fields.insert(
        "message".to_string(),
        message.map_or(Value::Undef, |m| Value::str(m.to_string())),
    );
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
                interfaces: Vec::new(),
                is_interface: false,
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
    // `PowerAssertionError` overrides `toString` to lead with a banner and
    // surround the rendered layout with blank lines, rather than name itself.
    if meta.name == "PowerAssertionError" {
        let body = inst
            .fields
            .get("message")
            .map(groovy_str)
            .unwrap_or_default();
        return format!("Assertion failed: \n\n{body}\n");
    }
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
    /// A `java.math.BigInteger` — an integer too large for a `Long`, a `G`
    /// literal, or the result of an integer `**` that overflowed. It carries a
    /// scale-0 [`BigDecimal`], so every exact-arithmetic path groovyrs already
    /// has applies to it unchanged; what the separate variant buys is the
    /// *type*, which decides `getClass()`, `instanceof`, and whether an
    /// arithmetic result stays a `BigInteger` or widens to a `BigDecimal`.
    BigInt(BigDecimal),
    /// A `~/…/` pattern — Groovy's `java.util.regex.Pattern`. Held on the heap
    /// because it is a value a `switch` label (and a variable) can carry, and
    /// because its *source* is what `toString` prints. The compiled form lives
    /// in [`crate::regex`]'s cache, keyed by that source.
    Regex(String),
    /// A live `java.util.regex.Matcher` — what `text =~ pattern` yields. Stateful
    /// like Java's: `find()` moves its cursor and `group(n)` reads whatever the
    /// last one landed on, so it must be shared by handle rather than copied.
    Matcher(MatcherVal),
    /// A `java.lang.Class` — what `getClass()` / the `.class` property answers.
    /// Holds the fully-qualified class name (a script-declared class has no
    /// package, so its qualified and simple names coincide).
    ClassRef(String),
    /// One `Map.Entry` — what the single-parameter closure form of a map's
    /// `each` / `collect` / `find` receives. Holds the entry's key and value; it
    /// prints as `k=v` and answers `key`/`value` (and `getKey`/`getValue`).
    Entry(String, Value),
    /// A `groovy.lang.Range` — `1..5` / `1..<5` / `'a'..'e'`. Groovy's ranges
    /// are objects, not lists: `println(1..5)` prints `1..5`, `getClass()` names
    /// `groovy.lang.IntRange`, and `from`/`to`/`step`/`reverse` are its own
    /// members. Materialising the literal to a list (which is what groovyrs did)
    /// gets every one of those wrong, so the endpoints are kept and the elements
    /// are enumerated on demand by [`range_elements`].
    Range(RangeVal),
    /// A mutable character buffer — `java.lang.StringBuilder`, its synchronised
    /// twin `java.lang.StringBuffer`, and `java.io.StringWriter`. All three are
    /// the same object here: a growable `String` behind a shared handle, so
    /// `sb.append("a")` mutates the buffer the caller still holds and answers
    /// it, which is what makes `sb.append("a").append(1)` chain. The class name
    /// is kept so `getClass()` names the one that was constructed.
    Buffer {
        class: &'static str,
        text: String,
    },
    /// A `java.util.Iterator` over an already-enumerated sequence — what
    /// `list.iterator()` / `map.iterator()` / `range.iterator()` answer. Stateful
    /// like Java's (`next()` advances a cursor the caller keeps seeing), so it
    /// lives behind a shared handle; `class` is the iterator implementation
    /// `getClass()` names for the receiver it came from.
    Iter {
        class: &'static str,
        items: Vec<Value>,
        pos: usize,
    },
    /// A `java.util.Set`. Elements are *stored* in insertion order whatever the
    /// implementation; [`SetKind`] decides the order they are iterated and
    /// printed in.
    ///
    /// A set is a heap object rather than a de-duplicated `Value::Array` because
    /// its type is observable in four ways a list cannot carry: `getClass()`
    /// names it, `==` ignores order (`([1, 2] as Set) == ([2, 1] as Set)`) while
    /// a list's does not, the set operators re-de-duplicate their result
    /// (`([1, 2] as Set) + ([2, 3] as Set)` is `[1, 2, 3]`, not `[1, 2, 2, 3]`),
    /// and `add` answers `false` for an element already present. Riding a handle
    /// also makes `s.add(x)` mutate the set the caller still holds, which a
    /// fusevm `Value::Array` cannot do (see `MUTATED`).
    SetVal {
        items: Vec<Value>,
        kind: SetKind,
    },
    /// A `java.util.ArrayList` — every Groovy `List` a script can name.
    ///
    /// A list rides a handle for the same reason a set and a map do: Groovy's
    /// lists are *references*. `def b = a` gives the one `ArrayList` a second
    /// name, and `b.add(4)` is visible through `a`; the same holds for a list
    /// reached through a map value, an element of another list, a closure
    /// parameter, or a capture. A fusevm `Value::Array` is a value, so none of
    /// that could work while lists were one — a mutator could only write back
    /// through the *variable* it was called on (see `MUTATED`), which made
    /// `[1, 2, 3]` behave like a copy at every other reference.
    ///
    /// `Value::Array` survives as the *transient* element-vector representation:
    /// [`deref_list`] hands one to the GDK read arms, which pattern-match it, and
    /// internal sequences (a range's enumeration, a spread's intermediate) never
    /// allocate a handle. The invariant is that a list which **escapes to user
    /// code** is built by [`glist`] and nothing else.
    ListVal(Vec<Value>),
}

/// Which `java.util.Set` implementation a set handle is, which is exactly the
/// question of what order it presents its elements in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SetKind {
    /// `java.util.LinkedHashSet` — insertion order. What `as Set` builds.
    Linked,
    /// `java.util.HashSet` — the JDK's bucket order (see [`hash_order`]).
    /// `req` is the initial capacity the constructor that built it asked for;
    /// the table size follows from that and the element count, and the two
    /// differ between construction paths — see [`hash_req_for_collection`].
    Hash { req: usize },
    /// `java.util.TreeSet` — ascending natural order.
    Tree,
}

/// A Groovy range: its endpoints as written, and whether the upper one is
/// included (`1..5`) or not (`1..<5`). The endpoints are kept as `Value`s rather
/// than as an enumerated list so `toString`, `getClass`, `from`, `to`, and
/// `size()` all answer without walking anything.
#[derive(Clone)]
pub struct RangeVal {
    from: Value,
    to: Value,
    inclusive: bool,
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
    /// Set when this closure has no body region of its own but wraps another
    /// callable — what `curry`, `>>`/`<<`, and `memoize` return. `name_idx` and
    /// `captures` are unused then; `params` still reports the arity the derived
    /// closure accepts, which the arity-sensitive GDK methods read.
    derived: Option<Box<Derived>>,
}

/// How a derived closure computes its result from the closure it wraps.
#[derive(Clone)]
enum Derived {
    /// `clo.curry(a…)` / `clo.rcurry(a…)` / `clo.ncurry(n, a…)`: the bound
    /// values are spliced into the call's argument list, at index `at` from the
    /// left or — when `from_right` — at `at` counted back from the end.
    Curried {
        base: Value,
        at: usize,
        from_right: bool,
        bound: Vec<Value>,
    },
    /// `a >> b` (`a.andThen(b)`) and `a << b`: call `first` with the arguments,
    /// then `second` with that result.
    Composed { first: Value, second: Value },
    /// `clo.memoize()`: call `base` once per distinct argument list, keyed by the
    /// Groovy rendering of the arguments. The cache lives in [`MEMO`], keyed by
    /// this closure's own heap id, so it is shared by every holder of the handle.
    Memoized { base: Value },
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
    /// The `implements A, B` names (an interface's own `extends` list lands here
    /// too). They make `instanceof` answer and contribute `default` methods.
    interfaces: Vec<String>,
    /// True for an `interface` declaration — it cannot be instantiated.
    is_interface: bool,
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
    MEMO.with(|m| m.borrow_mut().clear());
    MAP_DEFAULTS.with(|m| m.borrow_mut().clear());
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

/// Allocate a Groovy `List`. **The** way a list that escapes to user code is
/// built — every GDK method answering a `List`, every list literal, and every
/// coercion to one goes through here, so each of them is aliasable.
fn glist(items: Vec<Value>) -> Value {
    heap_push(HeapObj::ListVal(items))
}

/// The elements behind a `java.util.List` handle, if `v` is one.
fn as_list(v: &Value) -> Option<Vec<Value>> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::ListVal(items)) => Some(items.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// Is `v` a `java.util.List` — either a list handle or the transient
/// `Value::Array` form? The shape test every "did I get a list?" site uses, so
/// none of them has to know which of the two representations it is holding.
fn is_list(v: &Value) -> bool {
    matches!(v, Value::Array(_)) || as_list(v).is_some()
}

/// The heap id of a list handle, if `v` is one — what an in-place mutator writes
/// through and what `is()` compares.
fn list_id(v: &Value) -> Option<u32> {
    match v {
        Value::Obj(id) if as_list(v).is_some() => Some(*id),
        _ => None,
    }
}

/// Replace a list handle's contents in place. This is what makes a mutation
/// visible through every other name for the same list.
fn list_store(id: u32, items: Vec<Value>) {
    HEAP.with(|h| {
        if let Some(HeapObj::ListVal(slot)) = h.borrow_mut().get_mut(id as usize) {
            *slot = items;
        }
    });
}

/// Normalise a list handle to the transient `Value::Array` that the GDK read
/// arms pattern-match on; any other value passes through untouched.
///
/// This is the single adapter between the two representations. Applying it to a
/// receiver (and to the operands of an operator) is what let every existing
/// `(Value::Array(a), "method")` arm keep working unchanged when lists became
/// handles — the alternative was rewriting seventy match arms.
fn deref_list(v: &Value) -> Value {
    match as_list(v) {
        Some(items) => Value::array(items),
        None => v.clone(),
    }
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

/// The fully-qualified class name behind a `java.lang.Class` handle, if `v` is
/// one.
fn as_class_ref(v: &Value) -> Option<String> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::ClassRef(n)) => Some(n.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// The `(class, remaining-element-count)` of an iterator handle, if `v` is one.
fn as_iter(v: &Value) -> Option<(&'static str, usize)> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::Iter { class, items, pos }) => Some((*class, items.len() - *pos)),
            _ => None,
        }),
        _ => None,
    }
}

/// Advance an iterator handle, answering the element it was sitting on. `None`
/// once it is exhausted (Java's `NoSuchElementException` case).
fn iter_next(v: &Value) -> Option<Value> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow_mut().get_mut(*id as usize) {
            Some(HeapObj::Iter { items, pos, .. }) if *pos < items.len() => {
                *pos += 1;
                Some(items[*pos - 1].clone())
            }
            _ => None,
        }),
        _ => None,
    }
}

/// The `(key, value)` behind a `Map.Entry` handle, if `v` is one.
fn as_entry(v: &Value) -> Option<(String, Value)> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::Entry(k, val)) => Some((k.clone(), val.clone())),
            _ => None,
        }),
        _ => None,
    }
}

/// Clone the `BigDecimal` behind a handle, if `v` is a decimal.
fn as_dec(v: &Value) -> Option<BigDecimal> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            // A `BigInteger` answers here too: it is a scale-0 `BigDecimal`, so
            // every exact-arithmetic and rendering path applies to it as it
            // stands. Only the *type* differs, which [`as_bigint`] asks about.
            Some(HeapObj::Dec(d)) | Some(HeapObj::BigInt(d)) => Some(d.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// Clone the value behind a `java.math.BigInteger` handle, if `v` is one.
fn as_bigint(v: &Value) -> Option<BigDecimal> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::BigInt(d)) => Some(d.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// Allocate a `java.math.BigInteger`, truncating any fractional part the way
/// Java's `BigDecimal.toBigInteger` does.
fn bigint_value(d: BigDecimal) -> Value {
    heap_push(HeapObj::BigInt(decimal::to_big_integer(&d)))
}

/// Is `v` an integer type — `Integer`, `Long`, or `BigInteger`? This is what
/// decides whether an exact arithmetic result stays a `BigInteger`: Groovy
/// widens to `BigDecimal` only when a real decimal takes part.
fn is_integral(v: &Value) -> bool {
    matches!(v, Value::Int(_) | Value::Bool(_)) || as_bigint(v).is_some()
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

/// `GBIGINT`: pop a `java.math.BigInteger` literal's digits and return its
/// handle. Interned by text like [`b_dec`], so a literal inside a loop allocates
/// once rather than on every evaluation.
fn b_bigint(vm: &mut VM, _argc: u8) -> Value {
    let text = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let key = format!("{text}G");
    if let Some(id) = DEC_LITERALS.with(|d| d.borrow().get(&key).copied()) {
        return Value::Obj(id);
    }
    // The lexer already rejected malformed literals, so this parse cannot fail
    // for compiler-emitted text.
    let value = bigint_value(decimal::parse(&text).unwrap_or_else(|| decimal::from_i64(0)));
    if let Value::Obj(id) = value {
        DEC_LITERALS.with(|d| d.borrow_mut().insert(key, id));
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
            // A list is a collection: true when it holds anything. This has to
            // come before the generic "any handle is true" fallback, or an empty
            // list would be truthy.
            if let Some(items) = as_list(v) {
                return !items.is_empty();
            }
            if let Some(entries) = as_omap(v) {
                return !entries.is_empty();
            }
            // A set is a collection: true when it holds anything.
            if let Some((items, _)) = as_set(v) {
                return !items.is_empty();
            }
            // A range is a collection: true when it enumerates anything.
            if let Some(r) = as_range(v) {
                return !range_elements(&r).is_empty();
            }
            // A `StringBuilder` is a `CharSequence`, whose truth is non-empty.
            if let Some((_, text)) = as_buffer(v) {
                return !text.is_empty();
            }
            // Groovy's `asBoolean(Matcher)` *is* `matcher.find()` — it advances
            // the cursor, which is what makes `while (m) { … }` walk the
            // matches and `(s =~ /x/) ? …` ask "does one exist".
            if let Some(m) = as_matcher(v) {
                let handle = v.clone();
                return matcher_find(vm, &handle, &m);
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
        // A `GString` placeholder goes through `InvokerHelper.write`, which
        // writes any `Collection` as its elements — so an embedded range renders
        // `[1, 2, 3]` where `println` and `+` (which take `toString`) render
        // `1..3`. Verified against Apache Groovy 5.0.8: `"x${1..3}y"` is
        // `x[1, 2, 3]y`.
        out.push_str(&render_value(vm, &range_as_list(v)));
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
/// `GMAKE_LIST` — wrap the array `Op::MakeArray` just built into a list handle,
/// so the literal is a reference every other name for it can observe.
fn b_make_list(vm: &mut VM, _argc: u8) -> Value {
    let built = vm.stack.pop().unwrap_or(Value::Undef);
    match built {
        Value::Array(items) => glist(items),
        // Not an array: nothing to wrap (an empty literal still arrives as an
        // array, so this is only reachable if the emitter changes).
        other => other,
    }
}

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
        derived: None,
    }))
}

/// Build a derived-closure handle (see [`Derived`]) reporting `params` arity.
fn derived_closure(params: u8, d: Derived) -> Value {
    heap_push(HeapObj::Closure(ClosureMeta {
        name_idx: u16::MAX,
        params,
        captures: Vec::new(),
        derived: Some(Box::new(d)),
    }))
}

/// Run a derived closure: splice in curried arguments, chain a composition, or
/// answer a memoized call from [`MEMO`].
fn invoke_derived(vm: &mut VM, this: &Value, d: &Derived, args: &[Value]) -> Result<Value, String> {
    match d {
        Derived::Curried {
            base,
            at,
            from_right,
            bound,
        } => {
            let mut call: Vec<Value> = args.to_vec();
            // `rcurry` counts its insertion point back from the end of the
            // *supplied* arguments, so `{a,b -> a-b}.rcurry(1)(5)` is `5 - 1`.
            let idx = if *from_right {
                call.len().saturating_sub(*at)
            } else {
                (*at).min(call.len())
            };
            for (i, v) in bound.iter().enumerate() {
                call.insert(idx + i, v.clone());
            }
            invoke_closure(vm, base, &call)
        }
        Derived::Composed { first, second } => {
            let mid = invoke_closure(vm, first, args)?;
            invoke_closure(vm, second, std::slice::from_ref(&mid))
        }
        Derived::Memoized { base } => {
            let id = match this {
                Value::Obj(id) => *id,
                _ => return invoke_closure(vm, base, args),
            };
            let key: Vec<String> = args.iter().map(groovy_str).collect();
            let key = key.join("\u{1}");
            if let Some(hit) = MEMO.with(|m| m.borrow().get(&(id, key.clone())).cloned()) {
                return Ok(hit);
            }
            let out = invoke_closure(vm, base, args)?;
            MEMO.with(|m| m.borrow_mut().insert((id, key), out.clone()));
            Ok(out)
        }
    }
}

/// `clo.curry(…)` / `rcurry` / `ncurry` / `memoize` / `>>` / `<<` / `andThen` /
/// `clone` — the `groovy.lang.Closure` combinators, which all answer another
/// closure. Returns `None` when `method` is not one of them.
fn closure_combinator(
    recv: &Value,
    meta: &ClosureMeta,
    method: &str,
    args: &[Value],
) -> Option<Value> {
    let remaining = |taken: usize| meta.params.saturating_sub(taken as u8);
    Some(match method {
        "curry" => derived_closure(
            remaining(args.len()),
            Derived::Curried {
                base: recv.clone(),
                at: 0,
                from_right: false,
                bound: args.to_vec(),
            },
        ),
        "rcurry" => derived_closure(
            remaining(args.len()),
            Derived::Curried {
                base: recv.clone(),
                at: 0,
                from_right: true,
                bound: args.to_vec(),
            },
        ),
        "ncurry" => {
            let n = as_i64(args.first()?)?.max(0) as usize;
            let bound = args[1..].to_vec();
            derived_closure(
                remaining(bound.len()),
                Derived::Curried {
                    base: recv.clone(),
                    at: n,
                    from_right: false,
                    bound,
                },
            )
        }
        "memoize" => derived_closure(meta.params, Derived::Memoized { base: recv.clone() }),
        // `a >> b` and `a.andThen(b)` run the receiver first; `a << b` runs the
        // argument first. Both answer a closure of the *first* one's arity.
        "rightShift" | "andThen" => {
            let other = args.first()?.clone();
            closure_meta(&other)?;
            derived_closure(
                meta.params,
                Derived::Composed {
                    first: recv.clone(),
                    second: other,
                },
            )
        }
        "leftShift" | "compose" => {
            let other = args.first()?.clone();
            let om = closure_meta(&other)?;
            derived_closure(
                om.params,
                Derived::Composed {
                    first: other,
                    second: recv.clone(),
                },
            )
        }
        // A closure clone is a fresh handle over the same body and captures.
        "clone" => heap_push(HeapObj::Closure(meta.clone())),
        _ => return None,
    })
}

/// Invoke a closure `clo` with `args`, running its body through the fusevm frame
/// ABI. Drives a nested `VM::run`: a call frame is pushed whose `return_ip` is
/// past the end of the chunk, so the nested run halts exactly when the closure's
/// `ReturnValue` pops that frame. The interpreter's IP is saved and restored so
/// the enclosing dispatch loop resumes where it left off.
fn invoke_closure(vm: &mut VM, clo: &Value, args: &[Value]) -> Result<Value, String> {
    let meta = closure_meta(clo).ok_or_else(|| "groovyrs: value is not a closure".to_string())?;
    // A curried / composed / memoized closure has no body region of its own.
    if let Some(d) = &meta.derived {
        return invoke_derived(vm, clo, d, args);
    }
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
        // Same identity `Op::Call` records: this frame enters the subroutine
        // at `entry`, so `Chunk::sub_slot_names` is reachable from it.
        entry_ip: Some(entry),
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
    // No class in the chain defines it: an implemented interface may, as a Java 8
    // `default` method. Interfaces are searched after the whole class chain, so a
    // class definition always wins over a default.
    for id in interface_closure(class) {
        if let Some(idx) = class_meta(id).and_then(|m| m.methods.get(method).copied()) {
            return Some(idx);
        }
    }
    None
}

/// Every interface `class` implements, transitively — through its superclass
/// chain and through interfaces that themselves `extend` others. Breadth-first
/// from the most-derived class, so a nearer `default` method shadows a farther
/// one. Cycles (`interface A extends B`, `B extends A`) terminate on the seen
/// set.
fn interface_closure(class: u32) -> Vec<u32> {
    let mut queue: Vec<u32> = class_chain(class);
    queue.reverse(); // most-derived class first
    let mut seen: Vec<u32> = queue.clone();
    let mut out = Vec::new();
    let mut i = 0;
    while i < queue.len() {
        let Some(meta) = class_meta(queue[i]) else {
            i += 1;
            continue;
        };
        for name in &meta.interfaces {
            if let Some(id) = find_class(name) {
                if !seen.contains(&id) {
                    seen.push(id);
                    queue.push(id);
                    out.push(id);
                }
            }
        }
        i += 1;
    }
    out
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
    // The interface-name array, the `interface` flag, and the superclass name
    // (empty string ⇒ root class) are pushed first by `register_class`, so they
    // pop last and in that order.
    let interfaces_a = vm.stack.pop().unwrap_or(Value::Undef);
    let is_interface = matches!(vm.stack.pop(), Some(Value::Bool(true)));
    let super_name = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let superclass = (!super_name.is_empty()).then_some(super_name);
    let interfaces: Vec<String> = match interfaces_a {
        Value::Array(a) => a.iter().map(|v| v.as_str_cow().into_owned()).collect(),
        _ => Vec::new(),
    };

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
            interfaces,
            is_interface,
            field_names,
            field_inits,
            methods,
            ctors,
        })
    });
    Value::Undef
}

/// `new C(args)` for the JDK classes a Groovy script instantiates directly.
/// `None` when `C` is not one of them, so the caller falls through to the
/// script's own class registry (and then to the unresolved-class fault, which
/// stays an honest error for a class groovyrs does not model).
///
/// The collection classes construct the value groovyrs already uses for that
/// shape — a `Value::Array` for a `List`, a set handle for a `Set`, an ordered-map handle for a
/// `Map` — so every GDK method on them already applies. The box types construct
/// the primitive, which is what Groovy's auto-unboxing makes them anyway.
fn new_jdk(vm: &mut VM, class: &str, args: &[Value]) -> Option<Value> {
    let first = || args.first().cloned().unwrap_or(Value::Undef);
    let text = || args.first().map(groovy_str).unwrap_or_default();
    Some(match simple_name_of(class).as_str() {
        "StringBuilder" | "StringBuffer" | "StringWriter" => {
            let qualified = match simple_name_of(class).as_str() {
                "StringBuilder" => "java.lang.StringBuilder",
                "StringBuffer" => "java.lang.StringBuffer",
                _ => "java.io.StringWriter",
            };
            // `new StringBuilder(16)` sizes the buffer; only a `String`
            // argument is initial *content*.
            let text = match args.first() {
                Some(Value::Str(s)) => s.to_string(),
                _ => String::new(),
            };
            heap_push(HeapObj::Buffer {
                class: qualified,
                text,
            })
        }
        "ArrayList" | "LinkedList" | "Vector" => Value::array(match args.first() {
            Some(v) => iteration_elements(v),
            None => Vec::new(),
        }),
        "HashSet" | "LinkedHashSet" | "TreeSet" => {
            let seed = args.first().map(iteration_elements).unwrap_or_default();
            let kind = match simple_name_of(class).as_str() {
                "LinkedHashSet" => SetKind::Linked,
                "TreeSet" => SetKind::Tree,
                // `new HashSet(Collection)` sizes its table from the argument;
                // `new HashSet()` takes the default 16.
                _ => SetKind::Hash {
                    req: if args.is_empty() {
                        16
                    } else {
                        hash_req_for_collection(seed.len())
                    },
                },
            };
            make_set(seed, kind)
        }
        "HashMap" | "LinkedHashMap" | "TreeMap" => {
            let entries = args.first().and_then(as_omap).unwrap_or_default();
            heap_push(HeapObj::OrderedMap(entries))
        }
        "Object" => heap_push(HeapObj::Instance(Instance {
            class: u32::MAX,
            fields: std::collections::HashMap::new(),
        })),
        "Integer" | "Long" | "Short" | "Byte" => match as_i64(&first()) {
            Some(n) => Value::int(n),
            None => match text().trim().parse::<i64>() {
                Ok(n) => Value::int(n),
                Err(_) => raise_number_format(vm, text().trim()),
            },
        },
        "Double" | "Float" => match parse_java_double(&text()) {
            Some(f) => Value::float(f),
            None => raise_number_format(vm, text().trim()),
        },
        "Boolean" => Value::bool(matches!(&first(), Value::Bool(true)) || text() == "true"),
        "String" => Value::str(text()),
        // `new BigDecimal("1.5")` / `new BigInteger("12")` parse with Java's own
        // character-level diagnostics.
        "BigDecimal" | "BigInteger" => {
            let big = simple_name_of(class) == "BigInteger";
            let carry = |d: BigDecimal| if big { bigint_value(d) } else { dec_value(d) };
            match &first() {
                Value::Int(n) => carry(decimal::from_i64(*n)),
                other => match decimal::parse_java(groovy_str(other).trim()) {
                    Ok(d) => carry(d),
                    Err(msg) => {
                        raise_opt(vm, "NumberFormatException", msg.as_deref());
                        Value::Undef
                    }
                },
            }
        }
        _ => return None,
    })
}

/// The text behind a character-buffer handle, if `v` is one.
fn as_buffer(v: &Value) -> Option<(&'static str, String)> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::Buffer { class, text }) => Some((*class, text.clone())),
            _ => None,
        }),
        _ => None,
    }
}

/// Replace a character buffer's contents in place. Java's `StringBuilder`
/// mutates through the reference, so every holder of the handle sees the change.
/// Returns `false` if `v` is not a buffer.
fn buffer_set(v: &Value, text: String) -> bool {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow_mut().get_mut(*id as usize) {
            Some(HeapObj::Buffer { text: slot, .. }) => {
                *slot = text;
                true
            }
            _ => false,
        }),
        _ => false,
    }
}

/// The `java.lang.StringBuilder` methods a script calls, all of them mutating
/// the buffer in place and answering the builder itself where Java does (which
/// is what lets `sb.append("a").append(1)` chain). `None` for a method that is
/// not a buffer's, so the caller reports it missing rather than guessing.
///
/// Verified against Apache Groovy 5.0.8: `insert`/`deleteCharAt`/`setLength`/
/// `replace`/`reverse` all answer the receiver, `charAt` a one-character
/// `String`, and `indexOf` a character index.
fn dispatch_buffer_method(recv: &Value, text: &str, method: &str, args: &[Value]) -> Option<Value> {
    let chars: Vec<char> = text.chars().collect();
    let idx = |i: usize| args.get(i).and_then(as_i64).unwrap_or(0).max(0) as usize;
    let arg_str = |i: usize| args.get(i).map(groovy_str).unwrap_or_default();
    // Every mutator rebuilds the whole text, which is what a `String`-backed
    // buffer can do without a rope.
    let mutate = |next: String| {
        buffer_set(recv, next);
        recv.clone()
    };
    Some(match method {
        "append" | "leftShift" | "write" | "print" => mutate(format!("{text}{}", arg_str(0))),
        "toString" | "getText" => Value::str(text.to_string()),
        "length" | "size" => Value::int(chars.len() as i64),
        "isEmpty" => Value::bool(chars.is_empty()),
        "charAt" => Value::str(chars.get(idx(0)).map(|c| c.to_string()).unwrap_or_default()),
        "indexOf" => Value::int(
            text.find(&arg_str(0))
                .map(|b| text[..b].chars().count() as i64)
                .unwrap_or(-1),
        ),
        "reverse" => mutate(chars.iter().rev().collect()),
        "insert" => {
            let at = idx(0).min(chars.len());
            let (head, tail): (String, String) = (
                chars[..at].iter().collect(),
                chars[at..].iter().collect::<String>(),
            );
            mutate(format!("{head}{}{tail}", arg_str(1)))
        }
        "deleteCharAt" => {
            let at = idx(0);
            mutate(
                chars
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != at)
                    .map(|(_, c)| *c)
                    .collect(),
            )
        }
        "setLength" => {
            let n = idx(0);
            mutate(chars.iter().take(n).collect())
        }
        // `replace(start, end, text)` swaps the half-open character span.
        "replace" => {
            let (start, end) = (idx(0).min(chars.len()), idx(1).min(chars.len()));
            let head: String = chars[..start].iter().collect();
            let tail: String = chars[end.max(start)..].iter().collect();
            mutate(format!("{head}{}{tail}", arg_str(2)))
        }
        "delete" => {
            let (start, end) = (idx(0).min(chars.len()), idx(1).min(chars.len()));
            let head: String = chars[..start].iter().collect();
            let tail: String = chars[end.max(start)..].iter().collect();
            mutate(format!("{head}{tail}"))
        }
        _ => return None,
    })
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
    // A script-declared class shadows a JDK one of the same name, so the
    // registry is consulted first.
    if find_class(&name).is_none() {
        if let Some(v) = new_jdk(vm, &name, &args) {
            return v;
        }
    }
    let Some(cid) = find_class(&name) else {
        fault(vm, format!("unable to resolve class {name}"));
        return Value::Undef;
    };
    if class_meta(cid).is_some_and(|m| m.is_interface) {
        fault(vm, format!("groovyrs: {name} is an interface"));
        return Value::Undef;
    }
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

/// `GREGEX`: compile a `~/…/` literal's source into a `java.util.regex.Pattern`
/// handle. An unsupported construct faults at the literal, which is where the
/// mistake is — see [`crate::regex`] for what "unsupported" means and why the
/// alternative (handing it to the engine anyway) answers a different question.
fn b_regex(vm: &mut VM, argc: u8) -> Value {
    let source = pop_args(vm, argc)
        .into_iter()
        .next()
        .map(|v| v.as_str_cow().into_owned())
        .unwrap_or_default();
    match &*crate::regex::compile(&source) {
        Ok(_) => heap_push(HeapObj::Regex(source)),
        Err(e) => {
            fault(vm, format!("groovyrs: bad regex `{source}`: {e}"));
            Value::Undef
        }
    }
}

/// If `v` is a heap pattern handle, its source text — how Groovy renders a
/// `Pattern`, and what every match on it recompiles from (the compile is cached
/// by source in [`crate::regex`], so a pattern in a loop compiles once).
fn regex_source(v: &Value) -> Option<String> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::Regex(p)) => Some(p.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// If `v` is a heap pattern handle, does `text` match it in full? Groovy's
/// `case ~/…/` is `Pattern.matcher(s).matches()`, an all-or-nothing match.
fn regex_matches(v: &Value, text: &str) -> Option<bool> {
    let source = regex_source(v)?;
    match &*crate::regex::compile_whole(&source) {
        Ok(p) => Some(p.matches_whole(text).unwrap_or(false)),
        Err(_) => Some(false),
    }
}

/// The pattern source a `=~`/`==~` right operand names: a `Pattern` handle
/// (`~/…/`) contributes its own source, and any other value its string form —
/// which is how a slashy string (`/a./`, a plain `String` in Groovy) and a
/// double-quoted one both work as patterns.
fn pattern_source_of(v: &Value) -> String {
    regex_source(v).unwrap_or_else(|| groovy_str(v))
}

/// `GMATCH`: Groovy's `text =~ pattern` — a `java.util.regex.Matcher` over the
/// subject, positioned before the first match. Stack: subject (deepest), then
/// the pattern.
fn b_match(vm: &mut VM, _argc: u8) -> Value {
    let pattern = vm.stack.pop().unwrap_or(Value::Undef);
    let text = groovy_str(&vm.stack.pop().unwrap_or(Value::Undef));
    let source = pattern_source_of(&pattern);
    if let Err(e) = &*crate::regex::compile(&source) {
        raise(vm, "PatternSyntaxException", e);
        return Value::Undef;
    }
    heap_push(HeapObj::Matcher(MatcherVal {
        source,
        text,
        pos: 0,
        last: None,
    }))
}

/// `GMATCH_FULL`: Groovy's `text ==~ pattern` — whether the pattern matches the
/// **whole** subject. Stack: subject (deepest), then the pattern.
fn b_match_full(vm: &mut VM, _argc: u8) -> Value {
    let pattern = vm.stack.pop().unwrap_or(Value::Undef);
    let text = groovy_str(&vm.stack.pop().unwrap_or(Value::Undef));
    let source = pattern_source_of(&pattern);
    match &*crate::regex::compile_whole(&source) {
        Ok(p) => Value::bool(p.matches_whole(&text).unwrap_or(false)),
        Err(e) => {
            raise(vm, "PatternSyntaxException", e);
            Value::Undef
        }
    }
}

/// Clone the matcher behind a handle, if `v` is one.
fn as_matcher(v: &Value) -> Option<MatcherVal> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::Matcher(m)) => Some(m.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// Write a matcher's cursor and last match back through its handle. Java's
/// `Matcher` is mutable, so `m.find()` has to be visible to the next call.
fn matcher_advance(v: &Value, pos: usize, last: Option<crate::regex::Match>) {
    if let Value::Obj(id) = v {
        HEAP.with(|h| {
            if let Some(HeapObj::Matcher(m)) = h.borrow_mut().get_mut(*id as usize) {
                m.pos = pos;
                m.last = last;
            }
        });
    }
}

/// `Matcher.find()` — advance the cursor to the next match, answering whether
/// there was one. A zero-width match still advances, so the walk terminates.
fn matcher_find(vm: &mut VM, handle: &Value, m: &MatcherVal) -> bool {
    let compiled = crate::regex::compile(&m.source);
    let Ok(p) = &*compiled else { return false };
    match p.find(&m.text, m.pos) {
        Ok(Some(hit)) => {
            let next = if hit.end == hit.start {
                hit.end + 1
            } else {
                hit.end
            };
            matcher_advance(handle, next, Some(hit));
            true
        }
        Ok(None) => {
            matcher_advance(handle, m.text.len() + 1, None);
            false
        }
        Err(e) => {
            fault(vm, format!("groovyrs: {e}"));
            false
        }
    }
}

/// Every match in a matcher's subject, which is what `size()`, `count`, `[i]`
/// and iterating one all run over. Independent of the cursor, exactly as
/// Groovy's own `DefaultGroovyMethods` re-walks the matcher from the start.
fn matcher_all(m: &MatcherVal) -> Vec<crate::regex::Match> {
    match &*crate::regex::compile(&m.source) {
        Ok(p) => p.find_all(&m.text).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// One match as the value Groovy's `Matcher[i]` and the `each`/`collect` closure
/// receive: the matched text when the pattern has no groups, and the list
/// `[whole, g1, g2, …]` when it has. A group that did not participate is `null`.
fn match_value(hit: &crate::regex::Match) -> Value {
    if hit.groups.len() <= 1 {
        return Value::str(hit.groups[0].clone().unwrap_or_default());
    }
    Value::array(
        hit.groups
            .iter()
            .map(|g| match g {
                Some(t) => Value::str(t.clone()),
                None => Value::Undef,
            })
            .collect(),
    )
}

/// The `java.util.regex.Matcher` methods a Groovy script calls. `None` for a
/// method that is not a matcher's, so the caller reports it missing.
///
/// `group`/`start`/`end` read the *last* `find`, which is why the matcher is
/// mutable; `size`/`count`/`getAt` re-walk the subject instead, which is what
/// Groovy's collection view of a matcher does.
fn dispatch_matcher_method(
    vm: &mut VM,
    handle: &Value,
    m: &MatcherVal,
    method: &str,
    args: &[Value],
) -> Option<Value> {
    let group = |n: usize| match m.last.as_ref().and_then(|h| h.groups.get(n)) {
        Some(Some(t)) => Value::str(t.clone()),
        Some(None) => Value::Undef,
        // Java raises `IllegalStateException` before the first `find`; groovyrs
        // reports the same shape through the fault path.
        None => Value::Undef,
    };
    Some(match method {
        // Answered here rather than falling through to the match list, which
        // would name `java.util.ArrayList`.
        "getClass" => heap_push(HeapObj::ClassRef("java.util.regex.Matcher".to_string())),
        "toString" => Value::str(matcher_str(m)),
        "find" => Value::bool(matcher_find(vm, handle, m)),
        // `group` before any successful `find` is Java's `IllegalStateException`,
        // not a null — a script that reads it has a real bug.
        "group" if m.last.is_none() => {
            raise(vm, "IllegalStateException", "No match found");
            Value::Undef
        }
        "group" => group(args.first().and_then(as_i64).unwrap_or(0).max(0) as usize),
        "groupCount" => Value::int(match &*crate::regex::compile(&m.source) {
            Ok(p) => p.group_count() as i64,
            Err(_) => 0,
        }),
        "start" => Value::int(m.last.as_ref().map(|h| h.start as i64).unwrap_or(-1)),
        "end" => Value::int(m.last.as_ref().map(|h| h.end as i64).unwrap_or(-1)),
        // `matches()` asks whether the pattern covers the whole subject, which
        // is a different question from `find()` and does not move the cursor.
        "matches" => match &*crate::regex::compile_whole(&m.source) {
            Ok(p) => Value::bool(p.matches_whole(&m.text).unwrap_or(false)),
            Err(_) => Value::bool(false),
        },
        "size" | "count" | "getCount" => Value::int(matcher_all(m).len() as i64),
        "pattern" | "getPattern" => heap_push(HeapObj::Regex(m.source.clone())),
        "reset" => {
            matcher_advance(handle, 0, None);
            handle.clone()
        }
        "getAt" => {
            let all = matcher_all(m);
            let i = args.first().and_then(as_i64).unwrap_or(0);
            let idx = if i < 0 { all.len() as i64 + i } else { i };
            match usize::try_from(idx).ok().and_then(|u| all.get(u)) {
                Some(hit) => match_value(hit),
                None => Value::Undef,
            }
        }
        _ => return None,
    })
}

/// `Matcher.toString()` — Java's own shape, `java.util.regex.Matcher[pattern=P
/// region=0,N lastmatch=M]`, where `lastmatch` is empty until a `find` succeeds.
fn matcher_str(m: &MatcherVal) -> String {
    let last = m
        .last
        .as_ref()
        .and_then(|h| h.groups.first().cloned().flatten())
        .unwrap_or_default();
    format!(
        "java.util.regex.Matcher[pattern={} region=0,{} lastmatch={last}]",
        m.source,
        m.text.chars().count()
    )
}

/// A live `java.util.regex.Matcher`: the pattern source, the subject, the byte
/// offset the next `find()` searches from, and the match the last one landed on
/// (which `group(n)`, `start()` and `end()` read).
#[derive(Clone)]
pub struct MatcherVal {
    source: String,
    text: String,
    pos: usize,
    last: Option<crate::regex::Match>,
}

/// `GIS_CASE_TYPE`: Groovy's `case SomeType:` — `subject instanceof SomeType`.
/// Stack: subject (deepest), then the type name.
fn b_is_case_type(vm: &mut VM, _argc: u8) -> Value {
    let class = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let subject = vm.stack.pop().unwrap_or(Value::Undef);
    Value::bool(value_is_a(&subject, &class))
}

/// `GIS_CASE`: Groovy's `switch` case matching, which dispatches on the *label*,
/// not the subject — `Object.isCase(Object)` and its overrides. Stack: subject
/// (deepest), then the label.
///
/// Verified against Apache Groovy 5.0.7: a list (and therefore a range) label
/// matches when it contains the subject, a closure label matches
/// when calling it with the subject yields a Groovy-true value, a `Pattern`
/// label matches when the subject's string form matches it *entirely*
/// (`Matcher.matches`, not `find`), and every other label matches on `equals`.
fn b_is_case(vm: &mut VM, _argc: u8) -> Value {
    let label = vm.stack.pop().unwrap_or(Value::Undef);
    let subject = vm.stack.pop().unwrap_or(Value::Undef);
    // A `Range` label contains — `case 1..5:` and `x in 1..5` both ask that.
    let label = range_as_list(&label);
    // A null subject never matches a pattern; Groovy matches the rest against
    // their `toString`, which is exactly what `println` renders.
    if let Some(hit) = regex_matches(&label, &groovy_str(&subject)) {
        return Value::bool(hit && !matches!(subject, Value::Undef));
    }
    if closure_meta(&label).is_some() {
        return match invoke_closure(vm, &label, std::slice::from_ref(&subject)) {
            Ok(v) => Value::bool(groovy_truthy(vm, &v)),
            Err(e) => {
                fault(vm, e);
                Value::Undef
            }
        };
    }
    // `case [a, b]:` is a membership test. The label reaches here in *both*
    // representations — a list literal is a handle, and a `Range` label was just
    // rewritten to the transient array form above — so the shape test has to
    // admit either.
    if is_list(&label) {
        let want = groovy_str(&subject);
        return Value::bool(
            iteration_elements(&label)
                .iter()
                .any(|v| groovy_str(v) == want),
        );
    }
    // `case null:` matches only null; otherwise Groovy's `equals`.
    if matches!(label, Value::Undef) || matches!(subject, Value::Undef) {
        return Value::bool(matches!(label, Value::Undef) && matches!(subject, Value::Undef));
    }
    Value::bool(values_equal(&label, &subject))
}

// ---------------------------------------------------------------------------
// Power assert
// ---------------------------------------------------------------------------

/// `GASSERT_START`: begin an `assert`, discarding any values a previous one
/// recorded.
fn b_assert_start(vm: &mut VM, argc: u8) -> Value {
    pop_args(vm, argc);
    ASSERT_VALUES.with(|v| v.borrow_mut().clear());
    Value::Undef
}

/// `GASSERT_REC`: record one sub-expression's value under its source column and
/// push the value back, so recording is transparent to the surrounding
/// expression. Stack: value (deepest), then the column.
fn b_assert_rec(vm: &mut VM, _argc: u8) -> Value {
    let col = vm.stack.pop().unwrap_or(Value::Undef).to_int() as u32;
    let value = vm.stack.pop().unwrap_or(Value::Undef);
    ASSERT_VALUES.with(|v| v.borrow_mut().push((col, value.clone())));
    value
}

/// `GASSERT_FAIL`: raise the throwable a failed `assert` raises. With a `:`
/// message Groovy throws a plain `java.lang.AssertionError` carrying that
/// message plus the condition's AST text; without one it throws a
/// `PowerAssertionError` whose message is the rendered value layout over the
/// condition's *source* text.
fn b_assert_fail(vm: &mut VM, _argc: u8) -> Value {
    let pop_str = |vm: &mut VM| {
        vm.stack
            .pop()
            .unwrap_or(Value::Undef)
            .as_str_cow()
            .into_owned()
    };
    let named_values = vm.stack.pop().unwrap_or(Value::Undef);
    let names = pop_str(vm);
    let ast_text = pop_str(vm);
    let text = pop_str(vm);
    let message = vm.stack.pop().unwrap_or(Value::Undef);
    let values = ASSERT_VALUES.with(|v| std::mem::take(&mut *v.borrow_mut()));
    let (class, message) = match message {
        // `text` is the statement's verbatim source, which the recorded columns
        // are 1-based over.
        Value::Undef => ("PowerAssertionError", render_assertion(&text, values)),
        m => (
            "AssertionError",
            plain_assertion(&groovy_str(&m), &ast_text, &names, &named_values),
        ),
    };
    raise(vm, class, &message);
    Value::Undef
}

/// The `: message` form's text: `<message>. Expression: (<text>)`, plus a
/// `Values:` clause naming the condition's bare-variable operands when it has
/// any. Verified against Apache Groovy 5.0.7.
fn plain_assertion(message: &str, text: &str, names: &str, values: &Value) -> String {
    // No parentheses here: `Expression.getText()` already brackets a binary, and
    // a `!x` condition prints unbracketed.
    let mut out = format!("{message}. Expression: {text}");
    let empty = Vec::new();
    let values = match values {
        Value::Array(a) => a,
        _ => &empty,
    };
    let named: Vec<String> = names
        .split(',')
        .filter(|n| !n.is_empty())
        .zip(values)
        .map(|(name, v)| format!("{name} = {}", groovy_str(v)))
        .collect();
    if !named.is_empty() {
        out.push_str(&format!(". Values: {}", named.join(", ")));
    }
    out
}

/// Render a failed `assert` the way Groovy's power assert does: the condition's
/// source text, then each recorded value placed under the column it came from,
/// with `|` markers on the lines between.
///
/// This is a port of `org.codehaus.groovy.runtime.powerassert.AssertionRenderer`
/// (Apache Groovy 5.0.7). The layout rule is subtle enough that paraphrasing it
/// would not reproduce Groovy's output: values are placed right to left, each
/// onto the first line whose existing content starts *after* the value would
/// end, and every line it passes over on the way down gets a `|` marker.
fn render_assertion(text: &str, mut values: Vec<(u32, Value)>) -> String {
    // Right to left, and *stably*: where two expressions share a column the rule
    // below keeps the last recorded, which is only well defined for a stable sort.
    values.sort_by_key(|(col, _)| std::cmp::Reverse(*col));

    let mut lines: Vec<Vec<char>> = vec![text.chars().collect(), Vec::new()];
    // `starts[i]` is the first non-empty column of `lines[i]`; line 0 (the source
    // text) and the empty marker line both start at 0, so no value can be placed
    // on either and every value falls through to a line of its own or below.
    let mut starts: Vec<usize> = vec![0, 0];

    for i in 0..values.len() {
        let (col, value) = &values[i];
        let start_column = *col as usize;
        if start_column < 1 {
            continue;
        }
        // Where several expressions share a column, only the last recorded (the
        // outermost) is shown — Groovy's GROOVY-4344 rule.
        if values
            .get(i + 1)
            .is_some_and(|(c, _)| *c as usize == start_column)
        {
            continue;
        }
        let rendered = inspect_value(value);
        let rows: Vec<&str> = rendered.split('\n').collect();
        // A multi-line value never shares a line, so it always starts a new one.
        let end_column = if rows.len() == 1 {
            start_column + rendered.chars().count()
        } else {
            usize::MAX
        };

        let mut placed = false;
        for j in 1..lines.len() {
            if end_column < starts[j] {
                place(&mut lines[j], &rendered, start_column);
                starts[j] = start_column;
                placed = true;
                break;
            }
            place(&mut lines[j], "|", start_column);
            // Line 1 is the marker line and must stay claimable by nothing, so
            // its start column is left at 0.
            if j > 1 {
                starts[j] = start_column + 1;
            }
        }
        if !placed {
            for row in rows {
                let mut line = Vec::new();
                place(&mut line, row, start_column);
                lines.push(line);
                starts.push(start_column);
            }
        }
    }

    lines
        .into_iter()
        .map(|l| l.into_iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Write `s` into `line` starting at 1-based `column`, padding with spaces.
fn place(line: &mut Vec<char>, s: &str, column: usize) {
    while line.len() < column - 1 {
        line.push(' ');
    }
    for (k, ch) in s.chars().enumerate() {
        let at = column - 1 + k;
        match line.get_mut(at) {
            Some(slot) => *slot = ch,
            None => line.push(ch),
        }
    }
}

/// Groovy's *verbose* value rendering (`FormatHelper.format(v, true)`), which
/// the power-assert layout uses and `println` does not: a `String` is quoted,
/// and a collection's elements — including a map's keys — are rendered the same
/// way recursively. Everything else prints as it always does.
fn inspect_value(v: &Value) -> String {
    if let Some(entries) = as_omap(v) {
        if entries.is_empty() {
            return "[:]".to_string();
        }
        let items: Vec<String> = entries
            .iter()
            .map(|(k, val)| format!("'{k}':{}", inspect_value(val)))
            .collect();
        return format!("[{}]", items.join(", "));
    }
    // A list renders its elements with the same quoting rules, recursively —
    // through the handle form as well as the transient array form.
    if let Some(items) = as_list(v) {
        let shown: Vec<String> = items.iter().map(inspect_value).collect();
        return format!("[{}]", shown.join(", "));
    }
    match v {
        Value::Str(s) => format!("'{s}'"),
        Value::Array(a) => {
            let items: Vec<String> = a.iter().map(inspect_value).collect();
            format!("[{}]", items.join(", "))
        }
        other => groovy_str(other),
    }
}

/// Groovy's value equality for a `switch` label: numeric operands compare
/// numerically across types (`case 1:` matches the subject `1.00`), and
/// everything else compares by its rendered form — the same rule `==` uses.
fn values_equal(a: &Value, b: &Value) -> bool {
    // A list rides a heap handle, but `List.equals` is by *elements* — two
    // separately built lists holding the same values are equal, and the same
    // list under two names is not equal by virtue of being one handle. Deref to
    // the transient array form so every comparison below reads the elements.
    let (da, db) = (deref_list(a), deref_list(b));
    let (a, b) = (&da, &db);
    // `Set.equals` ignores order and is only ever true against another set:
    // `([1, 2] as Set) == ([2, 1] as Set)` but `([1, 2] as Set) == [1, 2]` is
    // false, because a `Set` and a `List` are never equal in Java whatever they
    // contain. Both halves need deciding here — the fallback below compares
    // rendered forms, which would answer the opposite on both counts.
    let (sa, sb) = (as_set(a), as_set(b));
    if sa.is_some() || sb.is_some() {
        return match (sa, sb) {
            (Some((x, _)), Some((y, _))) => {
                x.len() == y.len() && x.iter().all(|v| y.iter().any(|w| values_equal(v, w)))
            }
            _ => false,
        };
    }
    // A range compares as the list it enumerates, so `(1..3) == [1, 2, 3]` is
    // true in both directions the way Groovy's `AbstractList.equals` makes it.
    if as_range(a).is_some() || as_range(b).is_some() {
        let (x, y) = (range_as_list(a), range_as_list(b));
        return match (&x, &y) {
            (Value::Array(p), Value::Array(q)) => {
                p.len() == q.len() && p.iter().zip(q).all(|(i, j)| values_equal(i, j))
            }
            _ => false,
        };
    }
    if let Some(Ok(v)) = decimal_operator(NumOp::Eq, a, b) {
        return matches!(v, Value::Bool(true));
    }
    if let (Some(x), Some(y)) = (as_i64(a), as_i64(b)) {
        return x == y;
    }
    groovy_str(a) == groovy_str(b)
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
            return class_chain(inst.class).contains(&target)
                || interface_closure(inst.class).contains(&target);
        }
        // Named type is not a user class — fall through to built-in checks (an
        // instance is still an `Object`/`GroovyObject`).
    }
    // Built-in Groovy/Java types (short or common fully-qualified names).
    let short = class.rsplit('.').next().unwrap_or(class);
    match short {
        "Object" | "GroovyObject" => true,
        "String" | "GString" => matches!(value, Value::Str(_)),
        // A `StringBuilder`/`StringBuffer`/`StringWriter` is a `CharSequence`
        // too, which is what `sb instanceof CharSequence` asks.
        "CharSequence" => matches!(value, Value::Str(_)) || as_buffer(value).is_some(),
        "StringBuilder" | "StringBuffer" | "StringWriter" | "Appendable" => as_buffer(value)
            .is_some_and(|(c, _)| simple_name_of(c) == short || short == "Appendable"),
        "Integer" | "Int" | "Long" | "Short" | "Byte" => matches!(value, Value::Int(_)),
        // An unsuffixed Groovy decimal is a `BigDecimal` (a host-heap handle);
        // only a `d`/`f`-suffixed literal is an IEEE `Double`. A `BigInteger`
        // is a separate type and satisfies neither the other's test.
        "BigDecimal" => as_dec(value).is_some() && as_bigint(value).is_none(),
        "BigInteger" => as_bigint(value).is_some(),
        "Double" | "Float" => matches!(value, Value::Float(_)),
        "Number" => matches!(value, Value::Int(_) | Value::Float(_)) || as_dec(value).is_some(),
        "Boolean" => matches!(value, Value::Bool(_)),
        // A Groovy `Range` *is* a `java.util.List`, so it answers both.
        "List" | "ArrayList" | "Collection" | "Iterable" => {
            matches!(value, Value::Array(_))
                || as_list(value).is_some()
                || as_range(value).is_some()
        }
        "Range" | "IntRange" | "ObjectRange" | "NumberRange" => as_range(value).is_some(),
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
    // A handle whose class is not in the registry is not an instance call.
    class_meta(inst.class)?;
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
    // `getClass()` answers on every object (a user override was already found
    // by `lookup_method` above).
    if method == "getClass" && args.is_empty() {
        return Some(Ok(class_ref_of(recv)));
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
    Some(Ok(raise_missing_method(vm, recv, method, args)))
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
    // A handle whose class is not in the registry is not an instance read.
    class_meta(inst.class)?;
    let getter = format!("get{}", capitalize(name));
    if let Some(idx) = lookup_method(inst.class, &getter) {
        return Some(invoke_sub(vm, idx, std::slice::from_ref(recv)));
    }
    if inst.fields.contains_key(name) {
        return Some(Ok(inst.fields.get(name).cloned().unwrap_or(Value::Undef)));
    }
    // `obj.class` is the `getClass()` property, on a user instance too — but a
    // declared field named `class` (read above) still wins.
    if name == "class" {
        return Some(Ok(class_ref_of(recv)));
    }
    Some(Ok(raise_missing_property(vm, recv, name)))
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
        // A field the class chain never declared: Groovy raises rather than
        // growing the object (fields are materialised at construction).
        if !inst.fields.contains_key(&name) {
            return raise_missing_property(vm, &recv, &name);
        }
        set_instance_field(&recv, &name, value.clone());
        return value;
    }
    // `map.k = v` mutates the ordered map in place (through its shared handle).
    if omap_set(&recv, name.clone(), value.clone()) {
        return value;
    }
    // Groovy's property write on `null` surfaces the JDK's helpful
    // `NullPointerException` from the receiver's own `getClass()` call.
    if matches!(recv, Value::Undef) {
        raise(
            vm,
            "NullPointerException",
            "Cannot invoke \"Object.getClass()\" because \"obj\" is null",
        );
        return Value::Undef;
    }
    raise_missing_property(vm, &recv, &name)
}

/// `GINDEX`: read `recv[index]`. Dispatches a user `getAt(index)` on an instance,
/// else a list/map/string element (Groovy allows a negative list index).
fn b_index(vm: &mut VM, _argc: u8) -> Value {
    let index = vm.stack.pop().unwrap_or(Value::Undef);
    let recv = vm.stack.pop().unwrap_or(Value::Undef);
    index_read(vm, recv, index)
}

/// A missing-key read on a map built by `withDefault { … }`: run the stored
/// closure with the key, store the result under it, and answer it. `null` for
/// an ordinary map, which is what a missing key reads as.
fn map_default(vm: &mut VM, map: &Value, key: &str) -> Value {
    let Value::Obj(id) = map else {
        return Value::Undef;
    };
    let Some(clo) = MAP_DEFAULTS.with(|m| m.borrow().get(id).cloned()) else {
        return Value::Undef;
    };
    match invoke_closure(vm, &clo, &[Value::str(key.to_string())]) {
        Ok(v) => {
            omap_set(map, key.to_string(), v.clone());
            v
        }
        Err(e) => {
            fault(vm, e);
            Value::Undef
        }
    }
}

/// `recv[index]`, shared by the `[…]` subscript builtin and by the `getAt(…)`
/// method — Groovy defines the subscript *as* `getAt`, so both are one path.
fn index_read(vm: &mut VM, recv: Value, index: Value) -> Value {
    // A range subscripts as the list it enumerates, on either side: `(1..5)[0]`
    // is `1`, and `list[1..2]` is the sublist at those positions. A list handle
    // reads through the same transient array form the arms below match on.
    let recv = deref_list(&range_as_list(&recv));
    let index = deref_list(&range_as_list(&index));
    // `m[0]` is `Matcher.getAt(0)` — the i-th match, as the matched text or as
    // `[whole, g1, …]` when the pattern has groups.
    if as_matcher(&recv).is_some() {
        return dispatch_call(vm, recv, "getAt", vec![index]);
    }
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
        return match entries.iter().find(|(ek, _)| *ek == k) {
            Some((_, v)) => v.clone(),
            None => map_default(vm, &recv, &k),
        };
    }
    // Subscripting by a *collection* of indices — which is what a range
    // subscript is (it is rewritten to its element list above). `list[0..1]` is the
    // sublist and `"abc"[0..1]` the substring at those positions.
    if let Value::Array(idxs) = &index {
        let pick = |len: usize| -> Vec<i64> {
            idxs.iter()
                .filter_map(as_i64)
                .map(|i| if i < 0 { len as i64 + i } else { i })
                .collect()
        };
        match &recv {
            Value::Array(a) => {
                return Value::array(
                    pick(a.len())
                        .into_iter()
                        .filter_map(|i| usize::try_from(i).ok().and_then(|u| a.get(u)).cloned())
                        .collect(),
                )
            }
            Value::Str(s) => {
                let chars: Vec<char> = s.chars().collect();
                return Value::str(
                    pick(chars.len())
                        .into_iter()
                        .filter_map(|i| usize::try_from(i).ok().and_then(|u| chars.get(u)))
                        .collect::<String>(),
                );
            }
            _ => {}
        }
    }
    match &recv {
        // A list index past the end yields `null`; only a negative index that
        // stays negative after wrapping is an error, and Groovy reports it with
        // the array-subscript message its `getAt` uses.
        Value::Array(a) => {
            let i = index.to_int();
            let idx = if i < 0 { a.len() as i64 + i } else { i };
            if idx < 0 {
                raise_negative_index(vm, i, a.len())
            } else {
                a.get(idx as usize).cloned().unwrap_or(Value::Undef)
            }
        }
        Value::Hash(h) => h
            .get(&index.as_str_cow().into_owned())
            .cloned()
            .unwrap_or(Value::Undef),
        // A String subscript is a one-character substring, so an index past the
        // end is a `StringIndexOutOfBoundsException` naming the `[i, i+1)` range.
        Value::Str(s) => {
            let i = index.to_int();
            let chars: Vec<char> = s.chars().collect();
            let idx = if i < 0 { chars.len() as i64 + i } else { i };
            if idx < 0 {
                raise_negative_index(vm, i, chars.len())
            } else {
                match chars.get(idx as usize) {
                    Some(c) => Value::str(c.to_string()),
                    None => {
                        raise(
                            vm,
                            "StringIndexOutOfBoundsException",
                            &format!(
                                "Range [{idx}, {}) out of bounds for length {}",
                                idx + 1,
                                chars.len()
                            ),
                        );
                        Value::Undef
                    }
                }
            }
        }
        // Groovy has no `getAt` for this receiver, so the subscript is reported
        // as the missing `getAt` method it desugars to.
        _ => raise_missing_method(vm, &recv, "getAt", std::slice::from_ref(&index)),
    }
}

/// `GSETINDEX`: `recv[index] = value`, Groovy's `putAt`. A map writes through
/// its handle; a list is a fusevm *value*, so the new contents are pushed for
/// the caller to store back over a variable receiver. Assigning past the end of
/// a list grows it with nulls, exactly as Groovy's `putAt` does.
fn b_setindex(vm: &mut VM, _argc: u8) -> Value {
    let value = vm.stack.pop().unwrap_or(Value::Undef);
    let index = vm.stack.pop().unwrap_or(Value::Undef);
    let recv = vm.stack.pop().unwrap_or(Value::Undef);
    if as_instance(&recv).is_some() {
        if let Some(Err(e)) = dispatch_instance_method(vm, &recv, "putAt", &[index, value]) {
            fault(vm, e);
        }
        return recv;
    }
    if as_omap(&recv).is_some() {
        omap_set(&recv, groovy_str(&index), value);
        return recv;
    }
    // A list rides a handle, so the write goes *through* it and every other name
    // for the list observes it. The element math is [`list_put`], shared with the
    // transient-array arm below so the two can never drift.
    if let Some(id) = list_id(&recv) {
        return match list_put(as_list(&recv).unwrap_or_default(), &index, value) {
            Ok(next) => {
                list_store(id, next);
                recv
            }
            Err((i, len)) => raise_negative_index(vm, i, len),
        };
    }
    match &recv {
        Value::Array(a) => match list_put(a.clone(), &index, value) {
            Ok(next) => Value::array(next),
            Err((i, len)) => raise_negative_index(vm, i, len),
        },
        _ => raise_missing_method(vm, &recv, "putAt", &[index, value]),
    }
}

/// `list[i] = v`. A negative index counts from the end; an index past the end
/// grows the list, padding with `null`, the way Groovy's `List.putAt` does.
/// `Err((index, len))` is the negative-index-too-large case.
fn list_put(
    mut items: Vec<Value>,
    index: &Value,
    value: Value,
) -> Result<Vec<Value>, (i64, usize)> {
    let i = index.to_int();
    let idx = if i < 0 { items.len() as i64 + i } else { i };
    if idx < 0 {
        return Err((i, items.len()));
    }
    let idx = idx as usize;
    if idx >= items.len() {
        items.resize(idx + 1, Value::Undef);
    }
    items[idx] = value;
    Ok(items)
}

/// Raise the `ArrayIndexOutOfBoundsException` Groovy reports for a negative
/// subscript whose magnitude exceeds the receiver's length.
fn raise_negative_index(vm: &mut VM, index: i64, len: usize) -> Value {
    raise(
        vm,
        "ArrayIndexOutOfBoundsException",
        &format!("Negative array index [{index}] too large for array size {len}"),
    );
    Value::Undef
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
        // The owner (the script) has no such closure, so a `with`/`tap` delegate
        // is next in Groovy's `OWNER_FIRST` chain — innermost first.
        if let Some(v) = dispatch_on_delegate(vm, &name, &args) {
            return v;
        }
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

/// Dispatch a bare `name(args)` call the owner could not resolve against the
/// innermost `with`/`tap` delegate. `None` when no `with`/`tap` is running, so
/// the caller reports the name as unresolved as before. A mutator's new
/// contents are written back into the delegate slot, so `tap` sees them.
fn dispatch_on_delegate(vm: &mut VM, name: &str, args: &[Value]) -> Option<Value> {
    let recv = DELEGATES.with(|d| d.borrow().last().cloned())?;
    let out = dispatch_call(vm, recv, name, args.to_vec());
    if let Some(next) = take_mutated() {
        DELEGATES.with(|d| {
            if let Some(slot) = d.borrow_mut().last_mut() {
                *slot = next;
            }
        });
    }
    Some(out)
}

/// Pop the `(global-index, name)` pair both bare-name builtins are handed.
fn pop_name_site(vm: &mut VM) -> (usize, String) {
    let name = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let gidx = match vm.stack.pop() {
        Some(Value::Int(i)) if i >= 0 => i as usize,
        _ => usize::MAX,
    };
    (gidx, name)
}

/// True when the script binding `gidx` has been written. Globals start `Undef`,
/// so this is what separates "the owner has this name" from "nobody does" —
/// Groovy's `OWNER_FIRST` question, asked of the only owner a script closure has.
fn owner_bound(vm: &VM, gidx: usize) -> bool {
    vm.globals
        .get(gidx)
        .is_some_and(|v| !matches!(v, Value::Undef))
}

/// `GNAME_GET`: read a bare name inside a closure the owner could not resolve.
/// The owner answers first when it has the binding; otherwise the innermost
/// `with`/`tap` delegate is asked for the *property* of that name, which is what
/// Groovy's `OWNER_FIRST` resolve strategy does — and asking it as a property
/// means a delegate answers a bare name exactly as it answers `delegate.name`.
/// With no delegate running the answer is `null`, the unbound-global read this
/// replaced.
fn b_name_get(vm: &mut VM, _argc: u8) -> Value {
    let (gidx, name) = pop_name_site(vm);
    if owner_bound(vm, gidx) {
        return vm.globals[gidx].clone();
    }
    let Some(recv) = DELEGATES.with(|d| d.borrow().last().cloned()) else {
        return Value::Undef;
    };
    if let Some(res) = dispatch_instance_prop_get(vm, &recv, &name) {
        return match res {
            Ok(v) => v,
            Err(e) => {
                fault(vm, e);
                Value::Undef
            }
        };
    }
    dispatch_property(vm, &recv, &name)
}

/// `GNAME_SET`: write a bare name inside a closure the owner could not resolve.
/// A bound script binding takes the write; otherwise the innermost delegate does
/// if it can hold the name — a map takes any key (`[a: 1].with { b = 7 }` adds
/// `b`), an instance takes a field it declared. A delegate that can hold neither
/// does *not* raise: Groovy accepts `[1, 2].with { zork = 1 }` silently, so the
/// write falls through to the script binding, which is where it used to go
/// unconditionally.
fn b_name_set(vm: &mut VM, _argc: u8) -> Value {
    let (gidx, name) = pop_name_site(vm);
    let value = vm.stack.pop().unwrap_or(Value::Undef);
    if !owner_bound(vm, gidx) {
        if let Some(recv) = DELEGATES.with(|d| d.borrow().last().cloned()) {
            // A map mutates in place through its shared handle, so the delegate
            // slot needs no write-back the way a mutated list would.
            if omap_set(&recv, name.clone(), value.clone()) {
                return Value::Undef;
            }
            if as_instance(&recv).is_some_and(|i| i.fields.contains_key(&name)) {
                set_instance_field(&recv, &name, value);
                return Value::Undef;
            }
        }
    }
    if let Some(slot) = vm.globals.get_mut(gidx) {
        *slot = value;
    }
    Value::Undef
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
    dispatch_property(vm, &recv, &name)
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
    // Drop any mutated-receiver contents a previous call parked but whose
    // writeback never ran (the receiver was not a plain variable).
    take_mutated();
    // Groovy routes a call on `null` through `NullObject`, which answers
    // `toString()` and `equals()` and raises a `NullPointerException` for
    // everything else. The safe-navigation form never reaches here.
    if matches!(recv, Value::Undef) {
        return match method {
            "toString" => Value::str("null".to_string()),
            "equals" => Value::bool(matches!(args.first(), None | Some(Value::Undef))),
            // Groovy routes `null.getClass()` to `NullObject`, which answers.
            "getClass" => class_ref_of(&recv),
            _ => {
                raise(
                    vm,
                    "NullPointerException",
                    &format!("Cannot invoke method {method}() on null object"),
                );
                Value::Undef
            }
        };
    }
    // A `List` rides a handle so two names can see one list. Every list method
    // is already written against the transient `Value::Array` form, so the call
    // runs against that and its effect is reconciled back through the handle
    // here — one place rather than seventy match arms.
    // `Object.is(other)` — reference identity, which is exactly handle identity.
    // Defined here rather than per-type so every reference value answers it with
    // one rule. A non-handle receiver falls through to the ordinary dispatch (and
    // its `MissingMethodException`); see BUGS.md for why the boxed `Integer` /
    // interned `String` answers are not modeled.
    if method == "is" && args.len() == 1 {
        if let Value::Obj(id) = recv {
            return Value::bool(matches!(args[0], Value::Obj(other) if other == id));
        }
    }
    // `Object.equals(other)` on a collection. Groovy's `==` *is* `equals`, so the
    // two have to agree; these are the values with no per-type table entry for it
    // (`String`/`Number` have their own, and a user class's own `equals` is
    // dispatched below, where an instance receiver reaches it first).
    //
    // This runs *above* the per-type branches deliberately: the `Set` branch
    // hands anything it does not define to the list of its elements, which would
    // turn `setA.equals(setB)` into a list-vs-set comparison — false, where
    // Groovy answers true. Verified against Groovy 5.0.8, including the two
    // cross-type answers `==` already gets right: a list never equals a `Set`,
    // and a list *does* equal the `Range` enumerating the same elements.
    if method == "equals"
        && args.len() == 1
        && (is_list(&recv)
            || as_omap(&recv).is_some()
            || as_set(&recv).is_some()
            || as_range(&recv).is_some())
    {
        return Value::bool(values_equal(&recv, &args[0]));
    }
    // `with`/`tap` install the receiver as the closure's *delegate*, and a bare
    // mutator call inside the body (`[1, 2].tap { add(3) }`) has to reach this
    // list — so they run against the handle, further down, not against the
    // detached element copy this branch would hand them.
    let delegating =
        matches!(method, "with" | "tap") && args.len() == 1 && closure_meta(&args[0]).is_some();
    if let Some(id) = list_id(&recv).filter(|_| !delegating) {
        // `sort`/`unique` do not use the `MUTATED` slot — their *result* is the
        // new list (which is why the compiler writes the result back). Same rule
        // as `compiler::Compiler::emit_receiver_writeback`: only the no-argument,
        // `sort(true)` and closure forms mutate; `sort(false)` asks for a copy.
        let result_mutates = matches!(method, "sort" | "unique")
            && args
                .iter()
                .all(|a| closure_meta(a).is_some() || matches!(a, Value::Bool(true)));
        let items = as_list(&recv).unwrap_or_default();
        let answer = dispatch_call(vm, Value::array(items), method, args);
        // An in-place mutator parks its new contents for the compiler's
        // writeback. Store them through the handle instead: that is what every
        // *other* name for this list observes. Re-park the handle itself so a
        // writeback that does run stores a list back into the variable rather
        // than replacing it with a detached array.
        let mut mutated = take_mutated().is_some_and(|next| {
            list_store(id, iteration_elements(&next));
            true
        });
        if result_mutates {
            if let Value::Array(next) = &answer {
                list_store(id, next.clone());
                mutated = true;
            }
        }
        if mutated {
            set_mutated(recv.clone());
        }
        // The methods whose Groovy answer *is* the receiver, so that
        // `a.is(a.sort())` is true and a chained `a << 1 << 2` keeps writing
        // into the one list. Verified against Groovy 5.0.8; `reverse`,
        // `sort(false)` and `collect` answer a new list and are absent.
        let answers_receiver = matches!(method, "each" | "eachWithIndex" | "reverseEach")
            || (mutated && matches!(method, "sort" | "unique" | "leftShift" | "swap"));
        if answers_receiver && matches!(answer, Value::Array(_)) {
            return recv;
        }
        return answer;
    }
    // A `Range` answers its own members (`from`, `step`, `size`, `toString`)
    // and hands every other call to the list it enumerates, which is where the
    // closure-driven GDK (`each`, `collect`, `find`, `sum`) already lives. That
    // is faithful because Groovy's `Range` is a `java.util.List`; only the
    // `each` family answers the receiver itself rather than the list.
    // A `Set` answers its own members (the operators, the mutators, `getClass`)
    // and hands everything else to the list it enumerates — which is faithful,
    // because those methods answer an `ArrayList` in Groovy too.
    if let Some((items, kind)) = as_set(&recv) {
        if let Some(v) = dispatch_set_method(vm, &recv, &items, kind, method, &args) {
            return v;
        }
        return dispatch_call(vm, Value::array(set_elements(&items, kind)), method, args);
    }
    if let Some(r) = as_range(&recv) {
        if let Some(v) = dispatch_range_method(vm, &r, method, &args) {
            return v;
        }
        let listed = dispatch_call(vm, Value::array(range_elements(&r)), method, args);
        return if matches!(method, "each" | "eachWithIndex" | "reverseEach") {
            recv
        } else {
            listed
        };
    }
    // A `StringBuilder`/`StringBuffer`/`StringWriter` mutates through its
    // handle, so its methods run before the value-shaped dispatch below.
    if let Some((_, text)) = as_buffer(&recv) {
        if let Some(v) = dispatch_buffer_method(&recv, &text, method, &args) {
            return v;
        }
    }
    // A `Matcher` is mutable too — `find()` moves its cursor — so it is answered
    // through its handle. Anything it does not define runs over its matches,
    // which is Groovy's collection view of a matcher (`each`, `collect`, …).
    if let Some(m) = as_matcher(&recv) {
        if let Some(v) = dispatch_matcher_method(vm, &recv, &m, method, &args) {
            return v;
        }
        let hits: Vec<Value> = matcher_all(&m).iter().map(match_value).collect();
        return dispatch_call(vm, Value::array(hits), method, args);
    }
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
    // The `groovy.lang.Closure` combinators, which answer another closure.
    if let Some(meta) = closure_meta(&recv) {
        if let Some(v) = closure_combinator(&recv, &meta, method, &args) {
            return v;
        }
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
    // Groovy's `Object.with { … }` / `Object.tap { … }`: run the closure with
    // the receiver as `it`. `with` answers the closure's result, `tap` the
    // receiver. Defined on every value, so it precedes the per-type tables.
    if matches!(method, "with" | "tap") && args.len() == 1 && closure_meta(&args[0]).is_some() {
        DELEGATES.with(|d| d.borrow_mut().push(recv.clone()));
        let result = invoke_closure(vm, &args[0], std::slice::from_ref(&recv));
        // A mutator run against the delegate replaced its contents, so `tap`
        // answers the *mutated* receiver, not the one it was handed.
        let recv = DELEGATES
            .with(|d| d.borrow_mut().pop())
            .unwrap_or_else(|| recv.clone());
        return match result {
            Ok(v) => {
                if method == "tap" {
                    recv
                } else {
                    v
                }
            }
            Err(e) => {
                fault(vm, e);
                Value::Undef
            }
        };
    }
    // The closure-driven `Number` loops (`3.times { … }`, `1.upto(3) { … }`).
    if let Some(res) = dispatch_number_iteration(vm, &recv, method, &args) {
        return match res {
            Ok(v) => v,
            Err(e) => {
                fault(vm, e);
                Value::Undef
            }
        };
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
    // `String.replaceAll`/`replaceFirst`, whose replacement is either Java's
    // `$n`/`${name}` grammar or — Groovy's addition — a closure called with each
    // match. The closure form re-enters the VM, which is why this sits here
    // rather than in the pure GDK table.
    if let (Value::Str(s), "replaceAll" | "replaceFirst") = (&recv, method) {
        let first_only = method == "replaceFirst";
        let pattern = args.first().map(pattern_source_of).unwrap_or_default();
        let compiled = crate::regex::compile(&pattern);
        let p = match &*compiled {
            Ok(p) => p,
            Err(e) => {
                raise(vm, "PatternSyntaxException", e);
                return Value::Undef;
            }
        };
        let replacement = args.get(1).cloned().unwrap_or(Value::Undef);
        let result = if closure_meta(&replacement).is_some() {
            // The closure receives the whole match when the pattern has no
            // groups, and `(whole, g1, …)` when it has — so
            // `"a1b2".replaceAll(/(\d)/) { all, d -> "<$d>" }` is `a<1>b<2>`.
            p.replace_with(s, first_only, |hit| {
                let args: Vec<Value> = if hit.groups.len() <= 1 {
                    vec![match_value(hit)]
                } else {
                    hit.groups
                        .iter()
                        .map(|g| match g {
                            Some(t) => Value::str(t.clone()),
                            None => Value::Undef,
                        })
                        .collect()
                };
                invoke_closure(vm, &replacement, &args).map(|v| groovy_str(&v))
            })
        } else {
            p.replace(s, &groovy_str(&replacement), first_only)
        };
        return match result {
            Ok(text) => Value::str(text),
            Err(e) => {
                fault(vm, format!("groovyrs: {e}"));
                Value::Undef
            }
        };
    }
    // `s.eachMatch(pattern) { … }` runs the closure over every match — the whole
    // match for a group-less pattern, `[whole, g1, …]` when it has groups — and
    // answers the receiver.
    if let (Value::Str(s), "eachMatch") = (&recv, method) {
        let Some(clo) = args.last().filter(|a| closure_meta(a).is_some()) else {
            return raise_missing_method(vm, &recv, method, &args);
        };
        let pattern = args.first().map(pattern_source_of).unwrap_or_default();
        let compiled = crate::regex::compile(&pattern);
        let hits = match &*compiled {
            Ok(p) => p.find_all(s).unwrap_or_default(),
            Err(e) => {
                raise(vm, "PatternSyntaxException", e);
                return Value::Undef;
            }
        };
        for hit in &hits {
            if let Err(e) = invoke_closure(vm, clo, &[match_value(hit)]) {
                fault(vm, e);
                return Value::Undef;
            }
            if pending_exc() {
                return Value::Undef;
            }
        }
        return recv;
    }
    // A `String` iterates over its characters, so the same closure-driven GDK
    // applies. The `each` family answers the receiver itself, not a list.
    if let Value::Str(s) = &recv {
        let chars: Vec<Value> = s.chars().map(|c| Value::str(c.to_string())).collect();
        if let Some(res) = dispatch_iteration(vm, &chars, method, &args) {
            return match res {
                Ok(v) => {
                    if matches!(method, "each" | "eachWithIndex" | "reverseEach") {
                        recv
                    } else {
                        v
                    }
                }
                Err(e) => {
                    fault(vm, e);
                    Value::Undef
                }
            };
        }
    }
    // The same operations over a map, which passes `(key, value)` (or one
    // `Map.Entry`) to the closure and rebuilds a map where Groovy does.
    if let Some(entries) = as_omap(&recv) {
        if let Some(res) = dispatch_map_iteration(vm, &entries, method, &args) {
            return match res {
                Ok(v) => v,
                Err(e) => {
                    fault(vm, e);
                    Value::Undef
                }
            };
        }
    }
    // `x.getAt(i)` is the `x[i]` subscript spelled out — same operation, same
    // range/list/negative-index rules, same out-of-bounds diagnostics.
    if method == "getAt"
        && args.len() == 1
        && (!matches!(recv, Value::Obj(_)) || as_omap(&recv).is_some())
    {
        return index_read(vm, recv, args[0].clone());
    }
    // Pure GDK dispatch — no closure, no VM re-entrancy.
    dispatch_method(vm, &recv, method, &args)
}

/// The closure-driven GDK collection methods over a list (or the elements a
/// range enumerates): `each`, `collect`, `findAll`, `find`, `inject`, `sum`. Returns `None`
/// when `method` is not one of these (so the caller falls back to the pure GDK
/// dispatch), else the faithful result (or a fault message).
fn dispatch_iteration(
    vm: &mut VM,
    items: &[Value],
    method: &str,
    args: &[Value],
) -> Option<Result<Value, String>> {
    match method {
        // `list.removeAll { it > 1 }` / `retainAll { … }` — the *predicate*
        // spellings, which drop (or keep) every element the closure accepts and
        // answer whether the list changed. The collection-argument spellings
        // (`removeAll([2, 3])`) are the pure-GDK arm; only the closure form
        // re-enters the VM, so only it belongs here. Verified against Groovy
        // 5.0.8: `[1, 2, 3].removeAll { it > 1 }` answers `true` and leaves `[1]`.
        "removeAll" | "retainAll" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            let mut kept: Vec<Value> = Vec::new();
            for it in items {
                let hit = match invoke_closure(vm, clo, &item_args(clo, it)) {
                    Ok(v) => v.is_truthy(),
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                if hit == (method == "retainAll") {
                    kept.push(it.clone());
                }
            }
            let changed = kept.len() != items.len();
            set_mutated(Value::array(kept));
            Some(Ok(Value::bool(changed)))
        }
        // `list.each { it -> ... }` — run the closure for its side effects on
        // each element; the list itself is returned.
        "each" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            for it in items {
                if let Err(e) = invoke_closure(vm, clo, &item_args(clo, it)) {
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
                match invoke_closure(vm, clo, &item_args(clo, it)) {
                    Ok(v) => out.push(v),
                    Err(e) => return Some(Err(e)),
                }
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
            }
            Some(Ok(Value::array(out)))
        }
        // `list.permutations { it -> … }` is Groovy's `collect(permutations(self),
        // closure)` — the permutation *set* walked in its own bucket order, with
        // the closure's results in an `ArrayList`.
        "permutations" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            let perms = set_elements(
                &dedup_values(
                    permutations_of(items)
                        .into_iter()
                        .map(Value::array)
                        .collect(),
                ),
                SetKind::Hash {
                    req: DEFAULT_HASH_REQ,
                },
            );
            let mut out = Vec::with_capacity(perms.len());
            for p in &perms {
                match invoke_closure(vm, clo, &item_args(clo, p)) {
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
                match invoke_closure(vm, clo, &item_args(clo, it)) {
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
                match invoke_closure(vm, clo, &item_args(clo, it)) {
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
        // closure results; `list.sum(seed)` starts from `seed`. An empty list
        // with no seed sums to `null` (Groovy).
        "sum" => {
            let clo = args.last().filter(|a| closure_meta(a).is_some());
            // A leading non-closure argument is the seed.
            let mut acc: Option<Value> =
                args.first().filter(|a| closure_meta(a).is_none()).cloned();
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
        // `list.sort()` / `sort { it.key }` / `sort { a, b -> … }`. Groovy sorts
        // the receiver *in place* and returns it; the compiler writes the result
        // back to a variable receiver (see `compiler::Compiler::emit_receiver_writeback`).
        // `sort(false)` asks for a copy, which is what this always produces.
        "sort" => {
            let order = OrderBy::of(args);
            Some(sort_values(vm, items, &order).map(Value::array))
        }
        // `list.unique()` / `unique { key }` — drop later duplicates, keeping
        // source order. Mutates the receiver in Groovy, like `sort`.
        "unique" => {
            let order = OrderBy::of(args);
            let mut out: Vec<Value> = Vec::new();
            for it in items {
                let mut dup = false;
                for kept in &out {
                    match order.apply(vm, it, kept) {
                        Ok(o) => dup |= o.is_eq(),
                        Err(e) => return Some(Err(e)),
                    }
                }
                if !dup {
                    out.push(it.clone());
                }
            }
            Some(Ok(Value::array(out)))
        }
        // `list.max()` / `max { … }`, `list.min()` / `min { … }`.
        "max" | "min" => {
            let order = OrderBy::of(args);
            let want = if method == "max" {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Less
            };
            Some(extreme_value(vm, items, &order, want))
        }
        // `list.groupBy { … }` — a map from the closure's value to the sublist
        // of elements that produced it, keys in first-seen order.
        "groupBy" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            let mut groups: Vec<(String, Vec<Value>)> = Vec::new();
            for it in items {
                let key = match invoke_closure(vm, clo, std::slice::from_ref(it)) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                let key = groovy_str(&key);
                match groups.iter_mut().find(|(k, _)| *k == key) {
                    Some(slot) => slot.1.push(it.clone()),
                    None => groups.push((key, vec![it.clone()])),
                }
            }
            Some(Ok(heap_push(HeapObj::OrderedMap(
                groups
                    .into_iter()
                    .map(|(k, v)| (k, Value::array(v)))
                    .collect(),
            ))))
        }
        // `list.countBy { … }` — a map from the closure's value to how many
        // elements produced it, keys in first-seen order.
        "countBy" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            let mut counts: Vec<(String, i64)> = Vec::new();
            for it in items {
                let key = match invoke_closure(vm, clo, &item_args(clo, it)) {
                    Ok(v) => groovy_str(&v),
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                match counts.iter_mut().find(|(k, _)| *k == key) {
                    Some(slot) => slot.1 += 1,
                    None => counts.push((key, 1)),
                }
            }
            Some(Ok(heap_push(HeapObj::OrderedMap(
                counts
                    .into_iter()
                    .map(|(k, n)| (k, Value::int(n)))
                    .collect(),
            ))))
        }
        // `list.findIndexValues { … }` — *every* accepted index, where
        // `findIndexOf` answers only the first.
        "findIndexValues" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            // The optional leading argument is the index to start looking from.
            let from = args
                .first()
                .filter(|a| closure_meta(a).is_none())
                .and_then(as_i64)
                .unwrap_or(0)
                .max(0) as usize;
            let mut out: Vec<Value> = Vec::new();
            for (i, it) in items.iter().enumerate().skip(from) {
                let v = match invoke_closure(vm, clo, &item_args(clo, it)) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                if groovy_truthy(vm, &v) {
                    out.push(Value::int(i as i64));
                }
            }
            Some(Ok(Value::array(out)))
        }
        // `list.any { … }` / `list.every { … }` — short-circuiting predicates.
        // `any` stops at the first accepted element, `every` at the first
        // rejected one, matching Groovy's evaluation count exactly.
        "any" | "every" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            let want_all = method == "every";
            for it in items {
                let v = match invoke_closure(vm, clo, &item_args(clo, it)) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                if groovy_truthy(vm, &v) != want_all {
                    return Some(Ok(Value::bool(!want_all)));
                }
            }
            Some(Ok(Value::bool(want_all)))
        }
        // `list.count { … }` counts accepted elements; `list.count(value)`
        // counts elements equal to `value`.
        "count" => {
            let arg = args.last()?;
            if closure_meta(arg).is_none() {
                let want = groovy_str(arg);
                let n = items.iter().filter(|v| groovy_str(v) == want).count();
                return Some(Ok(Value::int(n as i64)));
            }
            let mut n = 0i64;
            for it in items {
                let v = match invoke_closure(vm, arg, &item_args(arg, it)) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                n += groovy_truthy(vm, &v) as i64;
            }
            Some(Ok(Value::int(n)))
        }
        // `list.collectEntries { … }` — build a map from each closure result,
        // which is either a two-element `[key, value]` list or a whole map.
        "collectEntries" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            let mut out: Vec<(String, Value)> = Vec::new();
            for it in items {
                let v = match invoke_closure(vm, clo, &item_args(clo, it)) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                for (k, val) in entry_pairs(&v) {
                    match out.iter_mut().find(|(ek, _)| *ek == k) {
                        Some(slot) => slot.1 = val,
                        None => out.push((k, val)),
                    }
                }
            }
            Some(Ok(heap_push(HeapObj::OrderedMap(out))))
        }
        // `list.collectMany { … }` — collect, then concatenate the sublists.
        "collectMany" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            let mut out: Vec<Value> = Vec::new();
            for it in items {
                let v = match invoke_closure(vm, clo, &item_args(clo, it)) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                out.extend(iteration_elements(&v));
            }
            Some(Ok(Value::array(out)))
        }
        // `list.findResult { … }` — the first non-null closure result, else null.
        "findResult" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            for it in items {
                let v = match invoke_closure(vm, clo, &item_args(clo, it)) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                if !matches!(v, Value::Undef) {
                    return Some(Ok(v));
                }
            }
            Some(Ok(Value::Undef))
        }
        // `list.findIndexOf { … }` / `findLastIndexOf { … }` — the index of the
        // first (last) accepted element, or `-1`.
        "findIndexOf" | "findLastIndexOf" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            let mut found = -1i64;
            for (i, it) in items.iter().enumerate() {
                let v = match invoke_closure(vm, clo, &item_args(clo, it)) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                if groovy_truthy(vm, &v) {
                    found = i as i64;
                    if method == "findIndexOf" {
                        break;
                    }
                }
            }
            Some(Ok(Value::int(found)))
        }
        // `list.takeWhile { … }` / `dropWhile { … }` — split at the first
        // element the closure rejects.
        "takeWhile" | "dropWhile" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            let mut cut = items.len();
            for (i, it) in items.iter().enumerate() {
                let v = match invoke_closure(vm, clo, &item_args(clo, it)) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                if !groovy_truthy(vm, &v) {
                    cut = i;
                    break;
                }
            }
            let kept = if method == "takeWhile" {
                items[..cut].to_vec()
            } else {
                items[cut..].to_vec()
            };
            Some(Ok(Value::array(kept)))
        }
        // `list.reverseEach { … }` — `each` from the last element backwards.
        "reverseEach" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            for it in items.iter().rev() {
                if let Err(e) = invoke_closure(vm, clo, &item_args(clo, it)) {
                    return Some(Err(e));
                }
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
            }
            Some(Ok(Value::array(items.to_vec())))
        }
        // `list.split { … }` — `[accepted, rejected]`, both in source order.
        "split" => {
            let clo = args.last()?;
            closure_meta(clo)?;
            let (mut yes, mut no) = (Vec::new(), Vec::new());
            for it in items {
                let v = match invoke_closure(vm, clo, &item_args(clo, it)) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                if groovy_truthy(vm, &v) {
                    yes.push(it.clone());
                } else {
                    no.push(it.clone());
                }
            }
            Some(Ok(Value::array(vec![Value::array(yes), Value::array(no)])))
        }
        // `list.toSorted(…)` is `sort` that never mutates the receiver — which
        // is what this always produces, so the two share an implementation.
        "toSorted" => {
            let order = OrderBy::of(args);
            Some(sort_values(vm, items, &order).map(Value::array))
        }
        // `list.toUnique(…)` is the non-mutating `unique`.
        "toUnique" => {
            let order = OrderBy::of(args);
            let mut out: Vec<Value> = Vec::new();
            for it in items {
                let mut dup = false;
                for kept in &out {
                    match order.apply(vm, it, kept) {
                        Ok(o) => dup |= o.is_eq(),
                        Err(e) => return Some(Err(e)),
                    }
                }
                if !dup {
                    out.push(it.clone());
                }
            }
            Some(Ok(Value::array(out)))
        }
        _ => None,
    }
}

/// The arguments Groovy passes a closure for one collection element: a closure
/// declaring more than one parameter receives a **list** element *spread* across
/// its parameters (`[[1, 2]].collect { a, b -> a + b }` yields `[3]`), and every
/// other case receives the element itself.
fn item_args(clo: &Value, item: &Value) -> Vec<Value> {
    let spread = closure_meta(clo).map(|m| m.params).unwrap_or(1) >= 2;
    // The element is a list handle; deref to see the elements to spread.
    match deref_list(item) {
        Value::Array(a) if spread => a,
        _ => vec![item.clone()],
    }
}

/// The closure-driven `Number` loops: `n.times { i -> }` (0 .. n-1),
/// `a.upto(b) { }` / `a.downto(b) { }` (both inclusive), and
/// `a.step(to, by) { }` (exclusive of `to`, like a `for`). All four answer
/// `null`, as Groovy's do. Returns `None` when `method` is not one of these.
fn dispatch_number_iteration(
    vm: &mut VM,
    recv: &Value,
    method: &str,
    args: &[Value],
) -> Option<Result<Value, String>> {
    if !matches!(method, "times" | "upto" | "downto" | "step") {
        return None;
    }
    let from = as_i64(recv)?;
    let clo = args.last()?;
    closure_meta(clo)?;
    // `times` counts 0 .. n-1; the others run from the receiver to the bound.
    let (mut i, to, by) = match method {
        "times" => (0, from - 1, 1),
        "upto" => (from, as_i64(args.first()?)?, 1),
        "downto" => (from, as_i64(args.first()?)?, -1),
        // `step` excludes its bound, so pull the limit in by one step.
        _ => {
            let to = as_i64(args.first()?)?;
            let by = as_i64(args.get(1)?)?;
            if by == 0 {
                return Some(Err("groovyrs: step size cannot be zero".to_string()));
            }
            (from, to - by.signum(), by)
        }
    };
    while if by > 0 { i <= to } else { i >= to } {
        if let Err(e) = invoke_closure(vm, clo, &[Value::int(i)]) {
            return Some(Err(e));
        }
        if pending_exc() {
            return Some(Ok(Value::Undef));
        }
        i += by;
    }
    Some(Ok(Value::Undef))
}

/// The `(key, value)` pairs a `collectEntries` closure result contributes: a
/// map contributes all of its entries, a two-element list one entry.
fn entry_pairs(v: &Value) -> Vec<(String, Value)> {
    if let Some(entries) = as_omap(v) {
        return entries;
    }
    if let Some((k, val)) = as_entry(v) {
        return vec![(k, val)];
    }
    // The `[key, value]` pair is a list handle; deref to read its two elements.
    match deref_list(v) {
        Value::Array(a) if a.len() == 2 => vec![(groovy_str(&a[0]), a[1].clone())],
        _ => Vec::new(),
    }
}

/// The closure-driven GDK over a **map**: `each`, `collect`, `findAll`, `find`,
/// `any`, `every`, `groupBy`, `inject`, `sort`, `max`, `min`. Returns `None`
/// when `method` is not one of these, so the caller falls back to the pure map
/// dispatch (`size`, `containsKey`, …).
///
/// Groovy hands a map's closure either `(key, value)` (a two-parameter closure)
/// or one `Map.Entry` (a one-parameter closure); [`entry_args`] picks between
/// them from the closure's declared parameter count.
fn dispatch_map_iteration(
    vm: &mut VM,
    entries: &[(String, Value)],
    method: &str,
    args: &[Value],
) -> Option<Result<Value, String>> {
    // Every operation here consumes a trailing closure except `sort`, which also
    // has a no-argument (sort-by-key) form.
    let clo = args.last().filter(|a| closure_meta(a).is_some());
    match method {
        "each" | "eachWithIndex" => {
            let clo = clo?;
            for (i, (k, v)) in entries.iter().enumerate() {
                let mut call = entry_args(clo, k, v);
                if method == "eachWithIndex" {
                    call.push(Value::int(i as i64));
                }
                if let Err(e) = invoke_closure(vm, clo, &call) {
                    return Some(Err(e));
                }
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
            }
            Some(Ok(heap_push(HeapObj::OrderedMap(entries.to_vec()))))
        }
        // `map.collectEntries { k, v -> … }` rebuilds a map from each closure
        // result (a `[key, value]` pair or a whole map).
        "collectEntries" => {
            let clo = clo?;
            let mut out: Vec<(String, Value)> = Vec::new();
            for (k, v) in entries {
                let r = match invoke_closure(vm, clo, &entry_args(clo, k, v)) {
                    Ok(r) => r,
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                for (ek, ev) in entry_pairs(&r) {
                    match out.iter_mut().find(|(k2, _)| *k2 == ek) {
                        Some(slot) => slot.1 = ev,
                        None => out.push((ek, ev)),
                    }
                }
            }
            Some(Ok(heap_push(HeapObj::OrderedMap(out))))
        }
        // `map.collect { k, v -> … }` yields a *list* of the closure's results.
        "collect" => {
            let clo = clo?;
            let mut out = Vec::with_capacity(entries.len());
            for (k, v) in entries {
                match invoke_closure(vm, clo, &entry_args(clo, k, v)) {
                    Ok(r) => out.push(r),
                    Err(e) => return Some(Err(e)),
                }
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
            }
            Some(Ok(Value::array(out)))
        }
        // `map.findAll { … }` yields a *map* of the accepted entries; `find`
        // yields the first accepted entry (a `Map.Entry`), else `null`.
        "findAll" | "find" | "any" | "every" => {
            let clo = clo?;
            let mut kept: Vec<(String, Value)> = Vec::new();
            for (k, v) in entries {
                let r = match invoke_closure(vm, clo, &entry_args(clo, k, v)) {
                    Ok(r) => r,
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                let ok = groovy_truthy(vm, &r);
                match method {
                    "find" if ok => {
                        return Some(Ok(heap_push(HeapObj::Entry(k.clone(), v.clone()))))
                    }
                    "any" if ok => return Some(Ok(Value::bool(true))),
                    "every" if !ok => return Some(Ok(Value::bool(false))),
                    "findAll" if ok => kept.push((k.clone(), v.clone())),
                    _ => {}
                }
            }
            Some(Ok(match method {
                "findAll" => heap_push(HeapObj::OrderedMap(kept)),
                "any" => Value::bool(false),
                "every" => Value::bool(true),
                _ => Value::Undef,
            }))
        }
        // `map.groupBy { … }` — a map from the closure's value to the *sub-map*
        // of entries that produced it.
        "groupBy" => {
            let clo = clo?;
            let mut groups: Vec<(String, Vec<(String, Value)>)> = Vec::new();
            for (k, v) in entries {
                let key = match invoke_closure(vm, clo, &entry_args(clo, k, v)) {
                    Ok(r) => groovy_str(&r),
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                match groups.iter_mut().find(|(gk, _)| *gk == key) {
                    Some(slot) => slot.1.push((k.clone(), v.clone())),
                    None => groups.push((key, vec![(k.clone(), v.clone())])),
                }
            }
            Some(Ok(heap_push(HeapObj::OrderedMap(
                groups
                    .into_iter()
                    .map(|(k, v)| (k, heap_push(HeapObj::OrderedMap(v))))
                    .collect(),
            ))))
        }
        // `map.inject(seed) { acc, entry -> … }` folds over the entries.
        "inject" => {
            let clo = clo?;
            let mut acc = match args {
                [seed, _] => seed.clone(),
                _ => return None,
            };
            for (k, v) in entries {
                let entry = heap_push(HeapObj::Entry(k.clone(), v.clone()));
                match invoke_closure(vm, clo, &[acc, entry]) {
                    Ok(r) => acc = r,
                    Err(e) => return Some(Err(e)),
                }
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
            }
            Some(Ok(acc))
        }
        // `map.sort()` orders by key and yields a *new* map (Groovy does not
        // mutate the receiver here, unlike `List.sort`).
        "sort" => {
            let handles: Vec<Value> = entries
                .iter()
                .map(|(k, v)| heap_push(HeapObj::Entry(k.clone(), v.clone())))
                .collect();
            let order = match clo {
                Some(c) => OrderBy::Key(c),
                None => OrderBy::Natural,
            };
            // With no closure Groovy orders by key; the entry handles themselves
            // have no natural order, so sort the keys and rebuild.
            if clo.is_none() {
                let mut sorted = entries.to_vec();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                return Some(Ok(heap_push(HeapObj::OrderedMap(sorted))));
            }
            Some(sort_values(vm, &handles, &order).map(|sorted| {
                heap_push(HeapObj::OrderedMap(
                    sorted.iter().filter_map(as_entry).collect(),
                ))
            }))
        }
        // `map.count { k, v -> … }` — how many entries the closure accepts.
        "count" => {
            let clo = clo?;
            let mut n = 0i64;
            for (k, v) in entries {
                let r = match invoke_closure(vm, clo, &entry_args(clo, k, v)) {
                    Ok(r) => r,
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                n += groovy_truthy(vm, &r) as i64;
            }
            Some(Ok(Value::int(n)))
        }
        // `map.countBy { k, v -> … }` — a map from the closure's value to how
        // many entries produced it, in first-seen order.
        "countBy" => {
            let clo = clo?;
            let mut counts: Vec<(String, i64)> = Vec::new();
            for (k, v) in entries {
                let key = match invoke_closure(vm, clo, &entry_args(clo, k, v)) {
                    Ok(r) => groovy_str(&r),
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                match counts.iter_mut().find(|(ck, _)| *ck == key) {
                    Some(slot) => slot.1 += 1,
                    None => counts.push((key, 1)),
                }
            }
            Some(Ok(heap_push(HeapObj::OrderedMap(
                counts
                    .into_iter()
                    .map(|(k, n)| (k, Value::int(n)))
                    .collect(),
            ))))
        }
        // `map.withDefault { key -> … }` — a copy whose missing-key reads run the
        // closure and *store* its result, the way `groovy.lang.MapWithDefault`
        // does. The closure is remembered in `MAP_DEFAULTS` against the copy.
        "withDefault" => {
            let clo = clo?;
            let copy = heap_push(HeapObj::OrderedMap(entries.to_vec()));
            if let Value::Obj(id) = copy {
                MAP_DEFAULTS.with(|m| m.borrow_mut().insert(id, clo.clone()));
            }
            Some(Ok(copy))
        }
        // `map.max { it.value }` / `min` yield the extreme *entry*.
        "max" | "min" => {
            let clo = clo?;
            let handles: Vec<Value> = entries
                .iter()
                .map(|(k, v)| heap_push(HeapObj::Entry(k.clone(), v.clone())))
                .collect();
            let want = if method == "max" {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Less
            };
            Some(extreme_value(vm, &handles, &OrderBy::Key(clo), want))
        }
        _ => None,
    }
}

/// The argument list a map's GDK closure receives for one entry: `(key, value)`
/// for a two-parameter closure, a single `Map.Entry` for a one-parameter one —
/// which is how Groovy decides too.
fn entry_args(clo: &Value, key: &str, value: &Value) -> Vec<Value> {
    if closure_meta(clo).map(|m| m.params).unwrap_or(1) >= 2 {
        vec![Value::str(key.to_string()), value.clone()]
    } else {
        vec![heap_push(HeapObj::Entry(key.to_string(), value.clone()))]
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
    if matches!(a, Value::Float(_)) || matches!(b, Value::Float(_)) {
        return Value::float(as_f64(a) + as_f64(b));
    }
    // A non-numeric element sums with Groovy's `plus` — strings concatenate,
    // lists append — exactly as the `+` operator does.
    groovy_add(a, b)
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
    dispatch_property(vm, &recv, &name)
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

/// Dispatch a faithful subset of the Groovy GDK for `recv.method(args)`. A
/// combination outside the subset raises `groovy.lang.MissingMethodException`
/// rather than mis-running, and a modeled method that Groovy fails on (an
/// out-of-range `list.get`, an unparsable `String.toInteger`) raises the same
/// throwable Groovy does.
fn dispatch_method(vm: &mut VM, recv: &Value, method: &str, args: &[Value]) -> Value {
    // `getClass()` answers on every value, so it precedes the per-type table.
    if method == "getClass" && args.is_empty() {
        return class_ref_of(recv);
    }
    // A `Range` answers its own members and hands everything else to the list it
    // enumerates — which is faithful, because Groovy's `Range` is a `List`.
    if let Some((items, kind)) = as_set(recv) {
        if let Some(v) = dispatch_set_method(vm, recv, &items, kind, method, args) {
            return v;
        }
        return dispatch_method(vm, &Value::array(set_elements(&items, kind)), method, args);
    }
    if let Some(r) = as_range(recv) {
        if let Some(v) = dispatch_range_method(vm, &r, method, args) {
            return v;
        }
        return dispatch_method(vm, &Value::array(range_elements(&r)), method, args);
    }
    // `next`/`previous` are Groovy's successor/predecessor — what a `Range`
    // walks with, and what `for (x in a..b)` steps by. They answer on every
    // ordered type, so they precede the per-type table.
    if args.is_empty() && matches!(method, "next" | "previous") {
        if let Some(v) = successor(recv, method == "next") {
            return v;
        }
    }
    match (recv, method) {
        // Universal size query (String chars / list elements / map entries).
        (_, "size") => Value::int(value_size(recv)),

        // ── String ──
        (Value::Str(s), "length") => Value::int(s.chars().count() as i64),
        (Value::Str(s), "toUpperCase") => Value::str(s.to_uppercase()),
        (Value::Str(s), "toLowerCase") => Value::str(s.to_lowercase()),
        (Value::Str(s), "trim") => Value::str(s.trim().to_string()),
        (Value::Str(s), "reverse") => Value::str(s.chars().rev().collect::<String>()),
        (Value::Str(s), "isEmpty") => Value::bool(s.is_empty()),
        (Value::Str(s), "contains") => {
            let needle = args.first().map(groovy_str).unwrap_or_default();
            Value::bool(s.contains(&needle))
        }
        // Groovy's numeric conversions parse the *trimmed* text and raise
        // `NumberFormatException` naming it when the parse fails.
        (Value::Str(s), "toInteger" | "toLong") => {
            let t = s.trim();
            let parsed = t
                .parse::<i64>()
                .ok()
                .filter(|n| method == "toLong" || (i32::MIN as i64..=i32::MAX as i64).contains(n));
            match parsed {
                Some(n) => Value::int(n),
                None => raise_number_format(vm, t),
            }
        }
        (Value::Str(s), "toDouble" | "toFloat") => match parse_java_double(s) {
            Some(f) => Value::float(f),
            None => raise_number_format(vm, s.trim()),
        },
        // `String.toBigDecimal()` is `new BigDecimal(text.trim())`, whose
        // `NumberFormatException` carries `BigDecimal`'s own character-level
        // diagnostics — including the message-less form for an empty string.
        (Value::Str(s), "toBigDecimal") => match decimal::parse_java(s.trim()) {
            Ok(d) => dec_value(d),
            Err(msg) => {
                raise_opt(vm, "NumberFormatException", msg.as_deref());
                Value::Undef
            }
        },
        // Index queries run over *characters*, matching `String.length()`.
        (Value::Str(s), "indexOf" | "lastIndexOf") => {
            let needle: String = args.first().map(groovy_str).unwrap_or_default();
            let byte_pos = if method == "indexOf" {
                s.find(&needle)
            } else {
                s.rfind(&needle)
            };
            Value::int(
                byte_pos
                    .map(|b| s[..b].chars().count() as i64)
                    .unwrap_or(-1),
            )
        }
        (Value::Str(s), "startsWith") => {
            Value::bool(s.starts_with(&args.first().map(groovy_str).unwrap_or_default()))
        }
        (Value::Str(s), "endsWith") => {
            Value::bool(s.ends_with(&args.first().map(groovy_str).unwrap_or_default()))
        }
        (Value::Str(s), "replace") => {
            let (from, to) = (
                args.first().map(groovy_str).unwrap_or_default(),
                args.get(1).map(groovy_str).unwrap_or_default(),
            );
            Value::str(s.replace(&from, &to))
        }
        (Value::Str(s), "concat") => Value::str(format!(
            "{s}{}",
            args.first().map(groovy_str).unwrap_or_default()
        )),
        (Value::Str(s), "equals") => {
            Value::bool(matches!(args.first(), Some(Value::Str(o)) if o == s))
        }
        (Value::Str(s), "equalsIgnoreCase") => Value::bool(
            args.first()
                .map(|o| groovy_str(o).to_lowercase() == s.to_lowercase())
                .unwrap_or(false),
        ),
        (Value::Str(s), "compareTo") => {
            let other = args.first().map(groovy_str).unwrap_or_default();
            Value::int(match s.as_ref().as_str().cmp(other.as_str()) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            })
        }
        (Value::Str(s), "charAt") => {
            let i = args.first().and_then(as_i64).unwrap_or(0);
            match usize::try_from(i).ok().and_then(|u| s.chars().nth(u)) {
                Some(c) => Value::str(c.to_string()),
                None => {
                    raise(
                        vm,
                        "StringIndexOutOfBoundsException",
                        &format!("Index {i} out of bounds for length {}", s.chars().count()),
                    );
                    Value::Undef
                }
            }
        }
        (Value::Str(s), "substring") => {
            let chars: Vec<char> = s.chars().collect();
            let from = args.first().and_then(as_i64).unwrap_or(0).max(0) as usize;
            let to = args
                .get(1)
                .and_then(as_i64)
                .map(|n| n.max(0) as usize)
                .unwrap_or(chars.len());
            if from > chars.len() || to > chars.len() || from > to {
                raise(
                    vm,
                    "StringIndexOutOfBoundsException",
                    &format!("begin {from}, end {to}, length {}", chars.len()),
                );
                return Value::Undef;
            }
            Value::str(chars[from..to].iter().collect::<String>())
        }
        // `String.multiply(n)` is the `"x" * n` operator.
        (Value::Str(s), "multiply") => {
            let n = args.first().and_then(as_i64).unwrap_or(0).max(0) as usize;
            Value::str(s.repeat(n))
        }
        // Groovy's padding/centring DGM. The pad text repeats and is cut to
        // length; `center` puts the odd character on the right.
        (Value::Str(s), "padLeft" | "padRight" | "center") => {
            let width = args.first().and_then(as_i64).unwrap_or(0).max(0) as usize;
            let pad = args.get(1).map(groovy_str).unwrap_or_else(|| " ".into());
            let len = s.chars().count();
            if width <= len || pad.is_empty() {
                return Value::str(s.to_string());
            }
            let need = width - len;
            let (left, right) = match method {
                "padLeft" => (need, 0),
                "padRight" => (0, need),
                _ => (need / 2, need - need / 2),
            };
            Value::str(format!(
                "{}{s}{}",
                pad_text(&pad, left),
                pad_text(&pad, right)
            ))
        }
        // `split` takes a regex, `tokenize` a set of delimiter characters
        // (whitespace when omitted) and drops the empty tokens.
        //
        // `String.split`'s specified rules are not a bare engine split: a
        // zero-width match at index 0 contributes no leading empty field
        // (`"abc".split("")` is `[a, b, c]`), and the default limit drops
        // *trailing* empties but keeps interior ones (`"a,b,,".split(",")` is
        // `[a, b]`). Both live in `crate::regex`.
        (Value::Str(s), "split") => {
            let pattern = args.first().map(pattern_source_of).unwrap_or_default();
            let limit = args.get(1).and_then(as_i64).unwrap_or(0);
            match &*crate::regex::compile(&pattern) {
                Ok(p) => match p.split(s, limit) {
                    Ok(parts) => Value::array(parts.into_iter().map(Value::str).collect()),
                    Err(e) => {
                        raise(vm, "IllegalArgumentException", &e);
                        Value::Undef
                    }
                },
                Err(e) => {
                    raise(vm, "PatternSyntaxException", e);
                    Value::Undef
                }
            }
        }
        // `String.matches(regex)` anchors to the whole input.
        (Value::Str(s), "matches") => {
            let pattern = args.first().map(pattern_source_of).unwrap_or_default();
            match &*crate::regex::compile_whole(&pattern) {
                Ok(p) => Value::bool(p.matches_whole(s).unwrap_or(false)),
                Err(e) => {
                    raise(vm, "PatternSyntaxException", e);
                    Value::Undef
                }
            }
        }
        // `findAll` answers every match, `find` the first (or `null`).
        (Value::Str(s), "findAll" | "find") if !args.is_empty() => {
            let pattern = args.first().map(pattern_source_of).unwrap_or_default();
            match &*crate::regex::compile(&pattern) {
                Ok(p) => {
                    let hits = p.find_all(s).unwrap_or_default();
                    if method == "find" {
                        return match hits.first() {
                            Some(h) => Value::str(h.groups[0].clone().unwrap_or_default()),
                            None => Value::Undef,
                        };
                    }
                    Value::array(
                        hits.iter()
                            .map(|h| Value::str(h.groups[0].clone().unwrap_or_default()))
                            .collect(),
                    )
                }
                Err(e) => {
                    raise(vm, "PatternSyntaxException", e);
                    Value::Undef
                }
            }
        }
        (Value::Str(s), "tokenize") => {
            let delims = args.first().map(groovy_str);
            let parts: Vec<Value> = match &delims {
                Some(d) => s
                    .split(|c| d.contains(c))
                    .filter(|p| !p.is_empty())
                    .map(|p| Value::str(p.to_string()))
                    .collect(),
                None => s
                    .split_whitespace()
                    .map(|p| Value::str(p.to_string()))
                    .collect(),
            };
            Value::array(parts)
        }
        (Value::Str(s), "toList" | "toCharArray" | "chars") => {
            Value::array(s.chars().map(|c| Value::str(c.to_string())).collect())
        }
        // `s.tr(from, to)` — `tr(1)`-style character translation. Both sides
        // expand `a-c` ranges (reversed ones too); a `from` character past the
        // end of `to` maps to `to`'s last character, and an unlisted character
        // passes through.
        (Value::Str(s), "tr") => {
            let from = expand_hyphen(&args.first().map(groovy_str).unwrap_or_default());
            let to = expand_hyphen(&args.get(1).map(groovy_str).unwrap_or_default());
            Value::str(
                s.chars()
                    .map(|c| match from.iter().position(|f| *f == c) {
                        Some(i) if !to.is_empty() => to[i.min(to.len() - 1)],
                        _ => c,
                    })
                    .collect::<String>(),
            )
        }
        // `s.stripIndent()` is Java's `String.stripIndent`: strip the common
        // indent and every line's trailing whitespace — except that a string
        // *ending* in a line terminator opts out of the outdent entirely.
        (Value::Str(s), "stripIndent") if args.is_empty() => Value::str(strip_indent(s)),
        // `s.stripIndent(n)` drops exactly `n` leading characters per line (a
        // shorter line becomes empty) and joins the lines with `\n`.
        (Value::Str(s), "stripIndent") => {
            let n = args.first().and_then(as_i64).unwrap_or(0);
            if s.is_empty() || n <= 0 {
                return Value::str(s.to_string());
            }
            let n = n as usize;
            Value::str(
                read_lines(s)
                    .iter()
                    .map(|l| l.chars().skip(n).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
        // `s.stripMargin([c])` — drop each line's leading whitespace up to and
        // including the first margin character (`|` by default). A line without
        // one is left alone.
        (Value::Str(s), "stripMargin") => {
            let margin = args
                .first()
                .map(groovy_str)
                .and_then(|m| m.chars().next())
                .unwrap_or('|');
            Value::str(
                read_lines(s)
                    .iter()
                    .map(|l| {
                        let body = l.trim_start_matches(|c: char| c <= ' ');
                        match body.strip_prefix(margin) {
                            Some(rest) => rest.to_string(),
                            None => l.clone(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
        // `s.expand([tabStop])` — replace each tab with spaces up to the next
        // tab stop, counting columns from the start of that line.
        (Value::Str(s), "expand") => {
            let stop = args.first().and_then(as_i64).unwrap_or(8).max(1) as usize;
            let expanded: Vec<String> =
                read_lines(s).iter().map(|l| expand_tabs(l, stop)).collect();
            Value::str(expanded.join("\n"))
        }
        // `normalize` folds CRLF and lone CR to LF; `denormalize` puts the
        // platform's line separator back (which is LF here).
        (Value::Str(s), "normalize" | "denormalize") => {
            Value::str(s.replace("\r\n", "\n").replace('\r', "\n"))
        }
        // `s.minus(x)` drops the *first* occurrence — of a literal string, or of
        // whatever a `Pattern` argument first matches.
        (Value::Str(s), "minus") => match args.first() {
            Some(p) if regex_source(p).is_some() => {
                let compiled = crate::regex::compile(&regex_source(p).unwrap());
                match &*compiled {
                    Ok(re) => match re.replace(s, "", true) {
                        Ok(t) => Value::str(t),
                        Err(e) => {
                            fault(vm, format!("groovyrs: {e}"));
                            Value::Undef
                        }
                    },
                    Err(e) => {
                        raise(vm, "PatternSyntaxException", e);
                        Value::Undef
                    }
                }
            }
            _ => groovy_sub(recv, args.first().unwrap_or(&Value::Undef)),
        },
        // `s.formatted(args…)` is `String.format(s, args…)` with the receiver as
        // the format string.
        (Value::Str(s), "formatted") => Value::str(java_format(vm, s, args)),
        // The `String.isX()` conversion predicates — each asks whether the
        // trimmed text parses as that type, which is how Groovy's are defined.
        (
            Value::Str(s),
            "isInteger" | "isLong" | "isDouble" | "isFloat" | "isBigDecimal" | "isBigInteger"
            | "isNumber",
        ) => {
            let t = s.trim();
            Value::bool(match method {
                "isInteger" => t.parse::<i32>().is_ok(),
                "isLong" => t.parse::<i64>().is_ok(),
                "isDouble" | "isFloat" => t.parse::<f64>().is_ok(),
                // `new BigInteger(t)` takes only an integer literal; the rest
                // ask `new BigDecimal(t)`, which is Java's decimal grammar.
                "isBigInteger" => {
                    let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
                    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
                }
                _ => crate::decimal::parse_java(t).is_ok(),
            })
        }
        (Value::Str(s), "capitalize" | "uncapitalize") => {
            let mut cs = s.chars();
            match cs.next() {
                Some(c) if method == "capitalize" => {
                    Value::str(c.to_uppercase().collect::<String>() + cs.as_str())
                }
                Some(c) => Value::str(c.to_lowercase().collect::<String>() + cs.as_str()),
                None => Value::str(String::new()),
            }
        }
        (Value::Str(s), "take" | "drop") => {
            let n = args.first().and_then(as_i64).unwrap_or(0).max(0) as usize;
            let cs: Vec<char> = s.chars().collect();
            let cut = n.min(cs.len());
            let kept = if method == "take" {
                &cs[..cut]
            } else {
                &cs[cut..]
            };
            Value::str(kept.iter().collect::<String>())
        }
        (Value::Str(s), "toBoolean") => Value::bool(matches!(
            s.trim().to_lowercase().as_str(),
            "true" | "y" | "1"
        )),

        // ── List ──
        (Value::Array(a), "isEmpty") => Value::bool(a.is_empty()),
        (Value::Array(a), "contains") => {
            let want = args.first().cloned().unwrap_or(Value::Undef);
            Value::bool(a.iter().any(|v| groovy_str(v) == groovy_str(&want)))
        }
        // Unlike the `[i]` subscript (which yields `null` past the end),
        // `List.get` is the raw JDK call and raises on any out-of-range index.
        (Value::Array(a), "get") => {
            let i = args.first().and_then(as_i64).unwrap_or(0);
            match usize::try_from(i).ok().and_then(|u| a.get(u)) {
                Some(v) => v.clone(),
                None => {
                    raise(
                        vm,
                        "IndexOutOfBoundsException",
                        &format!("Index {i} out of bounds for length {}", a.len()),
                    );
                    Value::Undef
                }
            }
        }
        // `list.subList(from, to)` — the half-open window `[from, to)`.
        //
        // The three bounds outcomes are the JDK's own, and they are distinct: a
        // negative `fromIndex` and a `toIndex` past the end are both
        // `IndexOutOfBoundsException` but name *different* indices, while a
        // reversed range is an `IllegalArgumentException` instead. Checking
        // `from` before `to` before the ordering is the order
        // `ArrayList.subListRangeCheck` uses, so a call that is wrong in two
        // ways reports the same one the JDK does.
        //
        // The result is a **copy**, not the aliasing `ArrayList$SubList` view
        // Java answers, and `getClass()` on it names `java.util.ArrayList`
        // rather than `java.util.ArrayList$SubList`. That is not a choice made
        // here: a fusevm `Value::Array` is a value rather than a handle (see
        // `MUTATED`), so groovyrs has no list aliasing at all — plain
        // `def b = a; b.add(4)` already leaves `a` unchanged. A view would have
        // nothing to point at until lists become heap handles, and a *copy* is
        // exactly as aliased as every other list groovyrs hands out.
        (Value::Array(a), "subList") => {
            let from = args.first().and_then(as_i64).unwrap_or(0);
            let to = args.get(1).and_then(as_i64).unwrap_or(0);
            if from < 0 {
                raise(
                    vm,
                    "IndexOutOfBoundsException",
                    &format!("fromIndex = {from}"),
                );
                Value::Undef
            } else if to > a.len() as i64 {
                raise(vm, "IndexOutOfBoundsException", &format!("toIndex = {to}"));
                Value::Undef
            } else if from > to {
                raise(
                    vm,
                    "IllegalArgumentException",
                    &format!("fromIndex({from}) > toIndex({to})"),
                );
                Value::Undef
            } else {
                Value::array(a[from as usize..to as usize].to_vec())
            }
        }
        (Value::Array(a), "reverse") => {
            let mut r = a.clone();
            r.reverse();
            Value::array(r)
        }
        // `list.join([separator])` renders each element the way `println` does
        // and joins with the separator (the empty string when omitted).
        (Value::Array(a), "join") => {
            let sep = args.first().map(groovy_str).unwrap_or_default();
            let parts: Vec<String> = a.iter().map(|v| render_value(vm, v)).collect();
            Value::str(parts.join(&sep))
        }
        // Prefix/suffix slices. A count past the end is clamped, as Groovy's are.
        (Value::Array(a), "take" | "drop") => {
            let n = (args.first().and_then(as_i64).unwrap_or(0).max(0) as usize).min(a.len());
            Value::array(if method == "take" {
                a[..n].to_vec()
            } else {
                a[n..].to_vec()
            })
        }
        // `head`/`first` raise on an empty list; `tail`/`init` are the
        // complementary slices and raise for the same reason. Each carries the
        // wording its own GDK method uses.
        (Value::Array(a), "first" | "head" | "last" | "tail" | "init") => {
            if a.is_empty() {
                raise(
                    vm,
                    "NoSuchElementException",
                    match method {
                        "last" => "Cannot access last() element from an empty List",
                        "tail" => "Cannot access tail() for an empty iterable",
                        "init" => "Cannot access init() for an empty Iterable",
                        _ => "Cannot access first() element from an empty List",
                    },
                );
                return Value::Undef;
            }
            match method {
                "first" | "head" => a[0].clone(),
                "last" => a[a.len() - 1].clone(),
                "tail" => Value::array(a[1..].to_vec()),
                _ => Value::array(a[..a.len() - 1].to_vec()),
            }
        }
        (Value::Array(a), "indexOf" | "lastIndexOf") => {
            let want = args.first().map(groovy_str).unwrap_or_default();
            let hit = if method == "indexOf" {
                a.iter().position(|v| groovy_str(v) == want)
            } else {
                a.iter().rposition(|v| groovy_str(v) == want)
            };
            Value::int(hit.map(|i| i as i64).unwrap_or(-1))
        }
        (Value::Array(a), "flatten") => Value::array(flatten_values(a)),
        (Value::Array(a), "toList" | "asImmutable" | "asSynchronized" | "clone") => {
            Value::array(a.clone())
        }
        // `toSet()` is Groovy's `new HashSet<>(self.size())` + `addAll`, so it
        // asks for a table sized to the *element count* — a smaller one than
        // `new HashSet(collection)` asks for, and that difference is visible as
        // a different iteration order for the same elements.
        (Value::Array(a), "toSet") => make_set(
            a.clone(),
            SetKind::Hash {
                req: table_size_for(a.len()),
            },
        ),
        // `toUnique` answers a de-duplicated **List**, not a set.
        (Value::Array(a), "toUnique") => {
            let mut out: Vec<Value> = Vec::new();
            for v in a {
                if !out.iter().any(|k| values_equal(k, v)) {
                    out.push(v.clone());
                }
            }
            Value::array(out)
        }
        (Value::Array(a), "containsAll") => {
            let other = args.first().map(iteration_elements).unwrap_or_default();
            Value::bool(other.iter().all(|w| a.iter().any(|v| values_equal(v, w))))
        }
        (Value::Array(a), "disjoint") => {
            let other = args.first().map(iteration_elements).unwrap_or_default();
            Value::bool(!other.iter().any(|w| a.iter().any(|v| values_equal(v, w))))
        }
        // Set-flavoured combinators, all keeping the receiver's order. The
        // two-argument `plus(index, other)` is a splice and is handled below.
        (Value::Array(a), "intersect" | "minus" | "plus") if args.len() < 2 => {
            let other = args.first().map(iteration_elements).unwrap_or_default();
            Value::array(match method {
                "intersect" => a
                    .iter()
                    .filter(|v| other.iter().any(|w| values_equal(v, w)))
                    .cloned()
                    .collect(),
                "minus" => a
                    .iter()
                    .filter(|v| !other.iter().any(|w| values_equal(v, w)))
                    .cloned()
                    .collect(),
                _ => a.iter().cloned().chain(other).collect(),
            })
        }
        // `list * n` — the receiver repeated `n` times.
        (Value::Array(a), "multiply") => {
            let n = args.first().and_then(as_i64).unwrap_or(0).max(0) as usize;
            Value::array(std::iter::repeat(a.clone()).take(n).flatten().collect())
        }
        // `[[1, 2], [3, 4]].transpose()` == `[[1, 3], [2, 4]]`; the result is as
        // long as the *shortest* row, which is what Groovy's does.
        (Value::Array(a), "transpose") => {
            let rows: Vec<Vec<Value>> = a.iter().map(iteration_elements).collect();
            let cols = rows.iter().map(Vec::len).min().unwrap_or(0);
            Value::array(
                (0..cols)
                    .map(|c| Value::array(rows.iter().map(|r| r[c].clone()).collect()))
                    .collect(),
            )
        }
        // `list.collate(n[, step])` — fixed-size windows, keeping the short
        // trailing one unless `keepRemainder` is false.
        (Value::Array(a), "collate") => {
            let size = args.first().and_then(as_i64).unwrap_or(1).max(1) as usize;
            let step = args
                .get(1)
                .and_then(as_i64)
                .filter(|_| args.len() > 2 || !matches!(args.get(1), Some(Value::Bool(_))))
                .map(|n| n.max(1) as usize)
                .unwrap_or(size);
            let keep = !matches!(args.last(), Some(Value::Bool(false)));
            let mut out = Vec::new();
            let mut i = 0;
            while i < a.len() {
                let end = (i + size).min(a.len());
                if end - i == size || keep {
                    out.push(Value::array(a[i..end].to_vec()));
                }
                i += step;
            }
            Value::array(out)
        }
        // The cartesian product of the receiver's sub-collections. A non-list
        // element counts as a one-element collection, so `[1, 2, 3]` has exactly
        // one combination.
        // The *first* sub-collection varies fastest, which is the order
        // `GroovyCollections.combinations` produces: `[[1, 2], [3, 4]]` gives
        // `[[1, 3], [2, 3], [1, 4], [2, 4]]`.
        (Value::Array(a), "combinations") => {
            let mut out: Vec<Vec<Value>> = vec![Vec::new()];
            for e in a {
                let choices = if is_list(e) {
                    iteration_elements(e)
                } else {
                    vec![e.clone()]
                };
                out = choices
                    .iter()
                    .flat_map(|c| {
                        out.iter().map(move |prefix| {
                            let mut p = prefix.clone();
                            p.push(c.clone());
                            p
                        })
                    })
                    .collect();
            }
            Value::array(out.into_iter().map(Value::array).collect())
        }
        // `permutations` and `subsequences` answer a `java.util.HashSet<List>`,
        // not a list — so they de-duplicate (`[1, 1].permutations()` is one
        // entry) and print in the JDK's bucket order rather than generation
        // order. Both feed a bare `new HashSet<>()`, whose table starts at 16.
        (Value::Array(a), "permutations") if args.is_empty() => make_set(
            permutations_of(a)
                .into_iter()
                .map(Value::array)
                .collect::<Vec<_>>(),
            SetKind::Hash {
                req: DEFAULT_HASH_REQ,
            },
        ),
        (Value::Array(a), "subsequences") => make_set(
            subsequences_of(a),
            SetKind::Hash {
                req: DEFAULT_HASH_REQ,
            },
        ),
        // `list.withIndex([offset])` pairs each element with its position…
        (Value::Array(a), "withIndex") => {
            let base = args.first().and_then(as_i64).unwrap_or(0);
            Value::array(
                a.iter()
                    .enumerate()
                    .map(|(i, v)| Value::array(vec![v.clone(), Value::int(base + i as i64)]))
                    .collect(),
            )
        }
        // …while `indexed([offset])` answers a *map* from position to element.
        (Value::Array(a), "indexed") => {
            let base = args.first().and_then(as_i64).unwrap_or(0);
            heap_push(HeapObj::OrderedMap(
                a.iter()
                    .enumerate()
                    .map(|(i, v)| ((base + i as i64).to_string(), v.clone()))
                    .collect(),
            ))
        }
        // `list.iterator()` — a live cursor over the elements. Stateful, so it
        // rides a handle (see `HeapObj::Iter`).
        (Value::Array(a), "iterator" | "listIterator") => heap_push(HeapObj::Iter {
            class: "java.util.ArrayList$Itr",
            items: a.clone(),
            pos: 0,
        }),
        (Value::Str(s), "iterator") => heap_push(HeapObj::Iter {
            class: "java.util.Iterator",
            items: s.chars().map(|c| Value::str(c.to_string())).collect(),
            pos: 0,
        }),
        // `list.takeRight(n)` / `dropRight(n)` — `take`/`drop` from the end.
        (Value::Array(a), "takeRight" | "dropRight") => {
            let n = (args.first().and_then(as_i64).unwrap_or(0).max(0) as usize).min(a.len());
            Value::array(if method == "takeRight" {
                a[a.len() - n..].to_vec()
            } else {
                a[..a.len() - n].to_vec()
            })
        }
        // `list.plus(index, other)` splices `other` in at `index`.
        (Value::Array(a), "plus") if args.len() == 2 => {
            let at = (args[0].to_int().max(0) as usize).min(a.len());
            let mut out = a[..at].to_vec();
            out.extend(iteration_elements(&args[1]));
            out.extend_from_slice(&a[at..]);
            Value::array(out)
        }
        // `list.swap(i, j)` exchanges two elements in place and answers the list.
        (Value::Array(a), "swap") => {
            let len = a.len();
            let i = args.first().and_then(as_i64).unwrap_or(0);
            let j = args.get(1).and_then(as_i64).unwrap_or(0);
            let slot = |n: i64| usize::try_from(n).ok().filter(|u| *u < len);
            match (slot(i), slot(j)) {
                (Some(x), Some(y)) => {
                    let mut next = a.clone();
                    next.swap(x, y);
                    let out = Value::array(next);
                    set_mutated(out.clone());
                    out
                }
                _ => {
                    let bad = if slot(i).is_none() { i } else { j };
                    raise(
                        vm,
                        "IndexOutOfBoundsException",
                        &format!("Index {bad} out of bounds for length {len}"),
                    );
                    Value::Undef
                }
            }
        }
        // Groovy's `pop` takes the *first* element and `removeLast` the last;
        // both raise on an empty list rather than answering `null`.
        (Value::Array(a), "pop" | "removeLast") => {
            if a.is_empty() {
                raise(
                    vm,
                    "NoSuchElementException",
                    &format!("Cannot {method}() an empty List"),
                );
                return Value::Undef;
            }
            let mut next = a.clone();
            let gone = if method == "pop" {
                next.remove(0)
            } else {
                next.pop().unwrap()
            };
            set_mutated(Value::array(next));
            gone
        }
        // ── List mutators ────────────────────────────────────────────────────
        // Each parks the new contents for the compiler-emitted writeback (see
        // `MUTATED`) and answers what the JDK/GDK call answers.
        (Value::Array(a), "add" | "leftShift" | "push" | "addAll") => {
            let mut next = a.clone();
            // `add(index, element)` inserts; every other form appends.
            match (method, args.len()) {
                ("add", 2) => {
                    let i = (as_i64(&args[0]).unwrap_or(0).max(0) as usize).min(next.len());
                    next.insert(i, args[1].clone());
                }
                ("addAll", _) => {
                    next.extend(args.first().map(iteration_elements).unwrap_or_default())
                }
                // `push` is Groovy's *stack* spelling, and its stack grows at the
                // front — the end `pop` takes from. Verified against Groovy
                // 5.0.8: `[1, 2].push(3)` leaves `[3, 1, 2]`, and `pop()` on that
                // answers `3`.
                ("push", _) => next.insert(0, args.first().cloned().unwrap_or(Value::Undef)),
                _ => next.push(args.first().cloned().unwrap_or(Value::Undef)),
            }
            let answer = match method {
                // `<<` answers the list itself so calls chain. `push` is `add`,
                // whose answer is `true` (verified against Groovy 5.0.8).
                "leftShift" => Value::array(next.clone()),
                "add" if args.len() == 2 => Value::Undef,
                _ => Value::bool(true),
            };
            set_mutated(Value::array(next));
            answer
        }
        (Value::Array(a), "remove" | "removeAt") => {
            let i = args.first().and_then(as_i64).unwrap_or(0);
            match usize::try_from(i).ok().filter(|u| *u < a.len()) {
                Some(u) => {
                    let mut next = a.clone();
                    let gone = next.remove(u);
                    set_mutated(Value::array(next));
                    gone
                }
                None => {
                    raise(
                        vm,
                        "IndexOutOfBoundsException",
                        &format!("Index {i} out of bounds for length {}", a.len()),
                    );
                    Value::Undef
                }
            }
        }
        (Value::Array(a), "removeElement" | "removeAll" | "retainAll") => {
            let drop_set = match method {
                "removeElement" => vec![args.first().cloned().unwrap_or(Value::Undef)],
                _ => args.first().map(iteration_elements).unwrap_or_default(),
            };
            let keep = |v: &Value| {
                let hit = drop_set.iter().any(|w| values_equal(v, w));
                if method == "retainAll" {
                    hit
                } else {
                    !hit
                }
            };
            let next: Vec<Value> = if method == "removeElement" {
                // Only the *first* occurrence goes, unlike `removeAll`.
                let mut seen = false;
                a.iter()
                    .filter(|v| {
                        let gone = !seen && drop_set.iter().any(|w| values_equal(v, w));
                        seen |= gone;
                        !gone
                    })
                    .cloned()
                    .collect()
            } else {
                a.iter().filter(|v| keep(v)).cloned().collect()
            };
            let changed = next.len() != a.len();
            set_mutated(Value::array(next));
            Value::bool(changed)
        }
        (Value::Array(a), "set") => {
            let i = args.first().and_then(as_i64).unwrap_or(0);
            match usize::try_from(i).ok().filter(|u| *u < a.len()) {
                Some(u) => {
                    let mut next = a.clone();
                    let old = std::mem::replace(&mut next[u], args[1].clone());
                    set_mutated(Value::array(next));
                    old
                }
                None => {
                    raise(
                        vm,
                        "IndexOutOfBoundsException",
                        &format!("Index {i} out of bounds for length {}", a.len()),
                    );
                    Value::Undef
                }
            }
        }
        (Value::Array(_), "clear") => {
            set_mutated(Value::array(Vec::new()));
            Value::Undef
        }

        // ── Map ──
        (Value::Hash(h), "isEmpty") => Value::bool(h.is_empty()),
        (Value::Hash(h), "containsKey") => {
            let k = args.first().map(groovy_str).unwrap_or_default();
            Value::bool(h.contains_key(&k))
        }

        // ── Integer / Long ──
        // `intdiv` is Groovy's *integer* division (`/` yields a BigDecimal), and
        // it truncates toward zero like Java's `/`.
        (Value::Int(n), "intdiv") => match args.first().and_then(as_i64) {
            Some(0) | None => {
                raise(vm, "ArithmeticException", "Division by zero");
                Value::Undef
            }
            // `Integer.MIN_VALUE.intdiv(-1)` overflows an `Integer` and wraps
            // back to `Integer.MIN_VALUE`, exactly as Java's `/` does.
            Some(d) => Value::int(wrap_to_width_of(*n, n.wrapping_div(d))),
        },
        // `Math.abs`/`.abs()` on `Integer.MIN_VALUE` is `Integer.MIN_VALUE`:
        // the positive counterpart is not an `Integer`, so the wrap stands.
        (Value::Int(n), "abs") => Value::int(abs_at_width(*n)),
        // `n.power(e)` is the `**` operator spelled out.
        (Value::Int(_), "power") => {
            power_of(vm, recv, args.first().unwrap_or(&Value::Undef), false)
        }
        // `255.toString(16)` is `16`, not `ff`: `Integer` has no *instance*
        // `toString(int)`, so Java's overload resolution reaches the static
        // `Integer.toString(int)` and renders the argument, discarding the
        // receiver. The two-argument form is the radix one — `255.toString(16, 2)`
        // is `Integer.toString(16, 2)`, `10000`. `Integer.toHexString(255)` is
        // the spelling that converts the receiver.
        (Value::Int(_), "toString") => match dispatch_static(vm, "Integer", "toString", args) {
            Some(v) => v,
            None => raise_missing_method(vm, recv, method, args),
        },
        (Value::Int(n), "toLong" | "longValue") => Value::int(*n),
        // `intValue()` is Java's narrowing conversion, so `3000000000L.intValue()`
        // is `-1294967296`.
        (Value::Int(n), "toInteger" | "intValue") => Value::int(i64::from(*n as i32)),
        (Value::Int(n), "toDouble" | "doubleValue" | "toFloat" | "floatValue") => {
            Value::float(*n as f64)
        }
        (Value::Int(n), "toBigDecimal") => dec_value(BigDecimal::from(*n)),
        (Value::Int(n), "toBigInteger") => bigint_value(BigDecimal::from(*n)),
        (Value::Int(n), "equals") => Value::bool(args.first().and_then(as_i64) == Some(*n)),
        (Value::Int(n), "compareTo") => {
            let other = args.first().and_then(as_i64).unwrap_or(0);
            Value::int((*n > other) as i64 - (*n < other) as i64)
        }
        // ── Double ──
        (Value::Float(f), "abs") => Value::float(f.abs()),
        (Value::Float(f), "round") if args.is_empty() => Value::int(java_round(*f)),
        // `d.round(n)` keeps `n` decimal places and stays a `double`; `trunc(n)`
        // is the same cut without the rounding.
        (Value::Float(f), "round" | "trunc") => {
            let n = args.first().and_then(as_i64).unwrap_or(0).max(0) as u32;
            let scale = 10f64.powi(n as i32);
            let scaled = f * scale;
            Value::float(if method == "round" {
                java_round(scaled) as f64 / scale
            } else {
                scaled.trunc() / scale
            })
        }
        (Value::Float(f), "toInteger" | "toLong" | "intValue" | "longValue") => {
            Value::int(*f as i64)
        }
        (Value::Float(f), "toDouble" | "doubleValue" | "toFloat" | "floatValue") => {
            Value::float(*f)
        }

        // ── BigDecimal (host heap) ──
        _ if as_dec(recv).is_some() => {
            let d = as_dec(recv).unwrap();
            match method {
                // `BigInteger` really does have an instance `toString(int radix)`
                // — unlike `Integer`, whose one-argument form is the static —
                // so `255G.toString(16)` is `ff`. `BigDecimal` has no such
                // overload, and neither takes two arguments.
                "toString" if args.len() == 1 && as_bigint(recv).is_some() => Value::str(
                    decimal::to_radix_string(&d, args.first().and_then(as_i64).unwrap_or(10)),
                ),
                "toString" if !args.is_empty() => raise_missing_method(vm, recv, method, args),
                "toString" => Value::str(decimal::to_groovy_string(&d)),
                // `BigDecimal.equals` compares *scale as well as value*, so
                // `1.00.equals(1.0)` is false where `1.00 == 1.0` is true.
                "equals" => Value::bool(
                    as_dec(args.first().unwrap_or(&Value::Undef))
                        .map(|o| decimal::to_groovy_string(&o) == decimal::to_groovy_string(&d))
                        .unwrap_or(false),
                ),
                // A `BigInteger` receiver keeps its type through these, which is
                // what Java's own `BigInteger.abs`/`negate` do.
                "abs" if as_bigint(recv).is_some() => bigint_value(decimal::abs(&d)),
                "negate" if as_bigint(recv).is_some() => bigint_value(decimal::neg(&d)),
                "abs" => dec_value(decimal::abs(&d)),
                "negate" => dec_value(decimal::neg(&d)),
                "toBigDecimal" => dec_value(d),
                "toBigInteger" => bigint_value(d),
                // `intdiv` is Groovy's integer division: exact, truncating, and
                // (unlike `/`) not promoted to a `BigDecimal`.
                "intdiv" => match args.first().and_then(as_exact_dec) {
                    Some(y) => match decimal::divide(&d, &y) {
                        Some(q) => bigint_value(q),
                        None => {
                            raise(vm, "ArithmeticException", "BigInteger divide by zero");
                            Value::Undef
                        }
                    },
                    None => raise_missing_method(vm, recv, method, args),
                },
                // Truncating conversions; `round` goes to the nearest integer.
                "intValue" | "longValue" | "toInteger" | "toLong" => {
                    Value::int(decimal::truncate_to_i64(&d))
                }
                "round" if args.is_empty() => Value::int(decimal::round_to_i64(&d)),
                // `round(n)` keeps `n` decimal places (half-up, Groovy's mode);
                // `trunc(n)` cuts them off. Both stay `BigDecimal`s, except that
                // the no-argument `trunc` answers a `BigInteger`.
                "round" => dec_value(decimal::round_half_up(
                    &d,
                    args.first().and_then(as_i64).unwrap_or(0),
                )),
                "trunc" if args.is_empty() => bigint_value(decimal::to_big_integer(&d)),
                "trunc" => dec_value(decimal::truncate_to_scale(
                    &d,
                    args.first().and_then(as_i64).unwrap_or(0),
                )),
                "power" => power_of(vm, recv, args.first().unwrap_or(&Value::Undef), false),
                "doubleValue" | "toDouble" | "floatValue" | "toFloat" => {
                    Value::float(decimal::to_f64(&d))
                }
                _ => raise_missing_method(vm, recv, method, args),
            }
        }

        // ── java.lang.Class (host heap) ──
        _ if as_class_ref(recv).is_some() => {
            let qualified = as_class_ref(recv).unwrap();
            match method {
                "getName" | "getTypeName" | "getCanonicalName" => Value::str(qualified),
                "getSimpleName" => Value::str(simple_name_of(&qualified)),
                _ => match dispatch_static(vm, &simple_name_of(&qualified), method, args) {
                    Some(v) => v,
                    None => raise_missing_method(vm, recv, method, args),
                },
            }
        }

        // ── Map.Entry (host heap) ──
        _ if as_entry(recv).is_some() => {
            let (k, v) = as_entry(recv).unwrap();
            match method {
                "getKey" => Value::str(k),
                "getValue" => v,
                _ => raise_missing_method(vm, recv, method, args),
            }
        }

        // ── java.util.Iterator (host heap) ──
        _ if as_iter(recv).is_some() => {
            let (_, left) = as_iter(recv).unwrap();
            match method {
                "hasNext" => Value::bool(left > 0),
                "next" => match iter_next(recv) {
                    Some(v) => v,
                    None => {
                        raise_opt(vm, "NoSuchElementException", None);
                        Value::Undef
                    }
                },
                _ => raise_missing_method(vm, recv, method, args),
            }
        }

        // ── Ordered map (host heap) ──
        _ if as_omap(recv).is_some() => {
            let entries = as_omap(recv).unwrap();
            match method {
                "isEmpty" => Value::bool(entries.is_empty()),
                "containsKey" => {
                    let k = args.first().map(groovy_str).unwrap_or_default();
                    Value::bool(entries.iter().any(|(ek, _)| *ek == k))
                }
                "get" => {
                    let k = args.first().map(groovy_str).unwrap_or_default();
                    entries
                        .iter()
                        .find(|(ek, _)| *ek == k)
                        .map(|(_, v)| v.clone())
                        // `get(k, default)` answers (and stores) the default.
                        .unwrap_or_else(|| match args.get(1) {
                            Some(d) => {
                                omap_set(recv, k, d.clone());
                                d.clone()
                            }
                            None => Value::Undef,
                        })
                }
                "keySet" | "keys" => {
                    Value::array(entries.iter().map(|(k, _)| Value::str(k.clone())).collect())
                }
                // `map.subMap(keys)` — the entries for the listed keys, in the
                // *receiver's* order; a key the map does not hold is dropped.
                // Both the collection and the varargs spelling are accepted.
                "subMap" => {
                    let wanted: Vec<String> = match args {
                        [one] if is_list(one) || as_range(one).is_some() => {
                            iteration_elements(one).iter().map(groovy_str).collect()
                        }
                        rest => rest.iter().map(groovy_str).collect(),
                    };
                    heap_push(HeapObj::OrderedMap(
                        entries
                            .into_iter()
                            .filter(|(k, _)| wanted.iter().any(|w| w == k))
                            .collect(),
                    ))
                }
                // `map.spread()` is a shallow copy of the map.
                "spread" | "clone" | "asImmutable" | "asSynchronized" => {
                    heap_push(HeapObj::OrderedMap(entries))
                }
                // `map - other` / `map.minus(other)` drops the entries the other
                // map holds *identically* (same key and same value).
                "minus" => {
                    let drop: Vec<(String, Value)> =
                        args.first().map(entry_pairs).unwrap_or_default();
                    heap_push(HeapObj::OrderedMap(
                        entries
                            .into_iter()
                            .filter(|(k, v)| {
                                !drop.iter().any(|(dk, dv)| dk == k && values_equal(dv, v))
                            })
                            .collect(),
                    ))
                }
                // …and `intersect` keeps exactly those.
                "intersect" => {
                    let keep: Vec<(String, Value)> =
                        args.first().map(entry_pairs).unwrap_or_default();
                    heap_push(HeapObj::OrderedMap(
                        entries
                            .into_iter()
                            .filter(|(k, v)| {
                                keep.iter().any(|(dk, dv)| dk == k && values_equal(dv, v))
                            })
                            .collect(),
                    ))
                }
                // A map iterates over its entries.
                "iterator" => heap_push(HeapObj::Iter {
                    class: "java.util.LinkedHashMap$LinkedEntryIterator",
                    items: entries
                        .into_iter()
                        .map(|(k, v)| heap_push(HeapObj::Entry(k, v)))
                        .collect(),
                    pos: 0,
                }),
                "values" => Value::array(entries.into_iter().map(|(_, v)| v).collect()),
                // `getOrDefault(k, d)` and the two-argument `get(k, d)` — the
                // latter also *stores* the default, which is what Groovy's does.
                "getOrDefault" => {
                    let k = args.first().map(groovy_str).unwrap_or_default();
                    entries
                        .iter()
                        .find(|(ek, _)| *ek == k)
                        .map(|(_, v)| v.clone())
                        .unwrap_or_else(|| args.get(1).cloned().unwrap_or(Value::Undef))
                }
                "containsValue" => {
                    let want = args.first().cloned().unwrap_or(Value::Undef);
                    Value::bool(entries.iter().any(|(_, v)| values_equal(v, &want)))
                }
                // A map is a heap handle, so its mutators write through it and
                // need no compiler-emitted writeback.
                "put" => {
                    let k = args.first().map(groovy_str).unwrap_or_default();
                    let old = entries
                        .iter()
                        .find(|(ek, _)| *ek == k)
                        .map(|(_, v)| v.clone())
                        .unwrap_or(Value::Undef);
                    omap_set(recv, k, args.get(1).cloned().unwrap_or(Value::Undef));
                    old
                }
                "putAll" => {
                    for (k, v) in args.first().map(entry_pairs).unwrap_or_default() {
                        omap_set(recv, k, v);
                    }
                    Value::Undef
                }
                "remove" => {
                    let k = args.first().map(groovy_str).unwrap_or_default();
                    let old = entries
                        .iter()
                        .find(|(ek, _)| *ek == k)
                        .map(|(_, v)| v.clone())
                        .unwrap_or(Value::Undef);
                    omap_retain(recv, |ek| ek != k);
                    old
                }
                "clear" => {
                    omap_retain(recv, |_| false);
                    Value::Undef
                }
                "entrySet" => Value::array(
                    entries
                        .into_iter()
                        .map(|(k, v)| heap_push(HeapObj::Entry(k, v)))
                        .collect(),
                ),
                _ => raise_missing_method(vm, recv, method, args),
            }
        }

        _ => raise_missing_method(vm, recv, method, args),
    }
}

/// `GPOWER`: Groovy's `**`. The result follows the numeric tower — two integers
/// with a non-negative exponent stay integral, a negative exponent or a decimal
/// base yields a `BigDecimal` (whose scale is the base's, times the exponent),
/// and a `double` base stays IEEE.
fn b_power(vm: &mut VM, _argc: u8) -> Value {
    // The compiler's static width of the base, pushed above the operands the
    // same way `<<` and `>>>` receive it — the magnitude of a `Long` small
    // enough to be an `Integer` cannot say which it is, and the two narrow
    // their `**` result to different types.
    let wide = shift_is_wide(vm);
    let exp = vm.stack.pop().unwrap_or(Value::Undef);
    let base = vm.stack.pop().unwrap_or(Value::Undef);
    power_of(vm, &base, &exp, wide)
}

/// `base ** exp`, shared by the `**` builtin and the `power(exp)` method Groovy
/// defines as the same operation. `wide` is the base's statically-known `Long`
/// width, which decides whether an integer result narrows to `Integer`.
fn power_of(vm: &mut VM, base: &Value, exp: &Value, wide: bool) -> Value {
    let (base, exp) = (base.clone(), exp.clone());
    let e = match as_i64(&exp) {
        Some(e) => e,
        // A fractional exponent has no exact form, so Groovy runs it as a double.
        None => return Value::float(as_f64(&base).powf(as_f64(&exp))),
    };
    if let Some(d) = as_dec(&base) {
        return match decimal::pow(&d, e) {
            Some(r) => dec_value(r),
            None => Value::float(as_f64(&base).powi(e as i32)),
        };
    }
    if let Value::Float(f) = base {
        return Value::float(f.powf(e as f64));
    }
    let Some(b) = as_i64(&base) else {
        raise(
            vm,
            "MissingMethodException",
            &format!("No signature of method: power() for {}", groovy_str(&base)),
        );
        return Value::Undef;
    };
    // A negative exponent leaves the integers entirely: Groovy runs
    // `Math.pow` and answers a `Double` (`2 ** -1` is the `Double` `0.5`, not a
    // `BigDecimal`). Verified against Apache Groovy 5.0.8.
    if e < 0 {
        return Value::float((b as f64).powf(e as f64));
    }
    // Groovy computes an integer power in `BigInteger` and then narrows to the
    // *base's* type if it fits: an `Integer` base answers an `Integer` while the
    // result is in 32-bit range and a `BigInteger` past it, so `2 ** 10` is the
    // `Integer` 1024 and `2 ** 40` is a `BigInteger` even though 1099511627776
    // is a perfectly good `Long`. A `Long` base narrows to `Long` instead, which
    // is why `2L ** 40` *is* a `Long`. Verified against Apache Groovy 5.0.8.
    let Some(exact) = decimal::pow(&decimal::from_i64(b), e) else {
        return Value::float((b as f64).powi(e.min(i32::MAX as i64) as i32));
    };
    let fits = match decimal::to_i64(&exact) {
        Some(n) if wide => Some(n),
        Some(n) if i32::try_from(n).is_ok() => Some(n),
        _ => None,
    };
    match fits {
        Some(n) => Value::int(n),
        None => bigint_value(exact),
    }
}

/// `GSHL`: Groovy's `<<`, which is `leftShift` — a bit shift on a number, an
/// append on a list, a concatenation on a string. The list form parks its new
/// contents for the compiler-emitted writeback, exactly like `list.add`.
fn b_shl(vm: &mut VM, _argc: u8) -> Value {
    let wide = shift_is_wide(vm);
    let rhs = vm.stack.pop().unwrap_or(Value::Undef);
    let lhs = vm.stack.pop().unwrap_or(Value::Undef);
    take_mutated();
    // `list << x` is `List.leftShift`, an in-place append that answers the list.
    // Route it through the ordinary method dispatch rather than repeating the
    // append here, so the handle write-through has exactly one implementation.
    if list_id(&lhs).is_some() {
        return dispatch_call(vm, lhs, "leftShift", vec![rhs]);
    }
    // `sb << "a"` is `StringBuilder.append`, which mutates through the handle
    // and answers the builder — so it chains, and needs no writeback.
    if let Some((_, text)) = as_buffer(&lhs) {
        buffer_set(&lhs, format!("{text}{}", groovy_str(&rhs)));
        return lhs;
    }
    // `a << b` on two closures is `Closure.compose`: `b` runs first.
    if let (Some(la), Some(rb)) = (closure_meta(&lhs), closure_meta(&rhs)) {
        let _ = la;
        return derived_closure(
            rb.params,
            Derived::Composed {
                first: rhs,
                second: lhs,
            },
        );
    }
    // `<<` on a user-class instance is its `leftShift` overload, the same way
    // `+` is `plus`. Arithmetic reaches an overload through the numeric hook,
    // but `NumOp` has no shift member, so the dispatch happens here.
    if let Some(v) = shift_overload(vm, &lhs, &rhs, "leftShift") {
        return v;
    }
    match &lhs {
        Value::Array(a) => {
            let mut next = a.clone();
            next.push(rhs);
            let out = Value::array(next);
            set_mutated(out.clone());
            out
        }
        Value::Str(s) => Value::str(format!("{s}{}", groovy_str(&rhs))),
        _ => match (as_i64(&lhs), as_i64(&rhs)) {
            // An `Integer` shifts at 32 bits with the count masked to 5, so
            // `1 << 31` is `-2147483648` and `1 << 32` is `1` again; a `Long`
            // shifts at 64 with the count masked to 6.
            (Some(a), Some(b)) => {
                if wide {
                    Value::int(a.wrapping_shl(b as u32 & 63))
                } else {
                    Value::int(i64::from((a as i32).wrapping_shl(b as u32 & 31)))
                }
            }
            _ => {
                raise(
                    vm,
                    "MissingMethodException",
                    &format!(
                        "No signature of method: leftShift() for {}",
                        groovy_str(&lhs)
                    ),
                );
                Value::Undef
            }
        },
    }
}

/// Dispatch a shift operator to a user-class `leftShift`/`rightShift` overload.
/// `Some` once the left operand is an instance — either the overload's result,
/// or `Undef` after faulting when the class does not define it (Groovy raises
/// `MissingMethodException`). `None` when the left operand is not an instance,
/// so the caller's own shift semantics apply.
///
/// `+`/`-`/`*`/`%`/`**` reach their overload through the strict numeric hook
/// ([`instance_operator`]), but fusevm's `NumOp` has no shift member for the
/// hook to carry, so the shift builtins dispatch here instead.
fn shift_overload(vm: &mut VM, lhs: &Value, rhs: &Value, method: &str) -> Option<Value> {
    as_instance(lhs)?;
    match call_user_method(vm, lhs, method, std::slice::from_ref(rhs)) {
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            fault(vm, e);
            Some(Value::Undef)
        }
        None => {
            raise(
                vm,
                "MissingMethodException",
                &format!(
                    "No signature of method: {}.{method}() is applicable for argument types: ({}) values: [{}]",
                    java_class_name(lhs),
                    java_class_name(rhs),
                    groovy_str(rhs)
                ),
            );
            Some(Value::Undef)
        }
    }
}

/// `GSHR`: Groovy's `>>` where the left operand is not a number.
///
/// Two forms reach here, both silently mis-answered before: `f >> g` on two
/// closures is `Closure.andThen` — forward composition, so `(f >> g)(x)` is
/// `g(f(x))` — and an instance left operand dispatches its `rightShift`
/// overload. Anything else falls back to the very shift the native lowering
/// performs, so a receiver the compiler mistook for an object (a name that held
/// a closure earlier in the flow, say) still answers correctly, just slower.
///
/// An ordinary `int >> n` never reaches this builtin: `Compiler::binary` emits
/// it only for a statically-object left operand and keeps the native ops
/// otherwise, so a shifting loop keeps its JIT trace.
fn b_shr(vm: &mut VM, _argc: u8) -> Value {
    let wide = shift_is_wide(vm);
    let rhs = vm.stack.pop().unwrap_or(Value::Undef);
    let lhs = vm.stack.pop().unwrap_or(Value::Undef);
    // `a >> b` on two closures is `Closure.andThen`: `a` runs first, and the
    // result takes `a`'s arity.
    if let (Some(la), Some(_)) = (closure_meta(&lhs), closure_meta(&rhs)) {
        return derived_closure(
            la.params,
            Derived::Composed {
                first: lhs,
                second: rhs,
            },
        );
    }
    if let Some(v) = shift_overload(vm, &lhs, &rhs, "rightShift") {
        return v;
    }
    match (as_i64(&lhs), as_i64(&rhs)) {
        // Same widths the native lowering uses: an `Integer` shifts its
        // sign-extended low 32 bits with the count masked to 5, a `Long` all 64
        // with the count masked to 6.
        (Some(a), Some(b)) => {
            if wide {
                Value::int(a >> (b as u32 & 63))
            } else {
                Value::int(i64::from((a as i32) >> (b as u32 & 31)))
            }
        }
        _ => {
            raise(
                vm,
                "MissingMethodException",
                &format!(
                    "No signature of method: {}.rightShift() is applicable for argument types: ({}) values: [{}]",
                    java_class_name(&lhs),
                    java_class_name(&rhs),
                    groovy_str(&rhs)
                ),
            );
            Value::Undef
        }
    }
}

/// `GUSHR`: Java's `>>>`. The fill width is the left operand's Java type — 32
/// bits for an `Integer`, 64 for a `Long` — so `-1 >>> 28` is `15` and
/// `-1L >>> 60` is `15` too. The count is masked to that width's bit index.
///
/// Stack: left, right, and the compiler's static width flag (see
/// [`shift_is_wide`]).
fn b_ushr(vm: &mut VM, _argc: u8) -> Value {
    let wide = shift_is_wide(vm);
    let rhs = vm.stack.pop().unwrap_or(Value::Undef);
    let lhs = vm.stack.pop().unwrap_or(Value::Undef);
    match (as_i64(&lhs), as_i64(&rhs)) {
        (Some(a), Some(b)) => {
            if wide {
                Value::int(((a as u64) >> (b as u32 & 63)) as i64)
            } else {
                // The result of an `int >>> n` is an `int`, so it carries the
                // sign of its low 32 bits: `Integer.MIN_VALUE >>> 0` is
                // `Integer.MIN_VALUE`, not `2147483648`.
                Value::int(i64::from(((a as i32 as u32) >> (b as u32 & 31)) as i32))
            }
        }
        _ => {
            raise(
                vm,
                "IllegalArgumentException",
                "`>>>` needs integer operands",
            );
            Value::Undef
        }
    }
}

/// `GIN`: Groovy's `x in coll`. A collection answers `contains`, a range its
/// bounds, a map key membership, a string substring containment.
fn b_in(vm: &mut VM, _argc: u8) -> Value {
    // `x in 1..5` asks the range's `contains`, which is its element list's. A
    // list handle reads through its transient array form.
    let coll = deref_list(&range_as_list(&vm.stack.pop().unwrap_or(Value::Undef)));
    let needle = vm.stack.pop().unwrap_or(Value::Undef);
    let _ = vm;
    if let Some(entries) = as_omap(&coll) {
        let k = groovy_str(&needle);
        return Value::bool(entries.iter().any(|(ek, _)| *ek == k));
    }
    Value::bool(match &coll {
        Value::Array(a) => a.iter().any(|v| values_equal(v, &needle)),
        Value::Str(s) => s.contains(&groovy_str(&needle)),
        Value::Undef => false,
        other => values_equal(other, &needle),
    })
}

/// `GCAST`: Groovy's `value as Type`. Covers the coercions a script actually
/// writes; an unmodeled target leaves the value alone rather than inventing a
/// conversion.
fn b_cast(vm: &mut VM, _argc: u8) -> Value {
    let ty = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    // A list handle casts through its transient array form (`as Set`, `as List`).
    let v = deref_list(&vm.stack.pop().unwrap_or(Value::Undef));
    let ty_simple = simple_name_of(&ty);
    match ty_simple.as_str() {
        // Integral targets truncate toward zero, as Java's narrowing casts do.
        "int" | "Integer" | "long" | "Long" | "short" | "Short" | "byte" | "Byte" => {
            let n = match &v {
                Value::Str(s) => match s.trim().parse::<i64>() {
                    Ok(n) => n,
                    Err(_) => return raise_number_format(vm, s.trim()),
                },
                Value::Float(f) => *f as i64,
                _ => match as_dec(&v) {
                    Some(d) => decimal::truncate_to_i64(&d),
                    None => as_i64(&v).unwrap_or(0),
                },
            };
            // A narrowing cast keeps the target's low bits, so `2147483648L as
            // int` is `-2147483648` and `300 as byte` is `44`.
            Value::int(narrow_to(&ty, n))
        }
        "double" | "Double" | "float" | "Float" => match &v {
            Value::Str(s) => match parse_java_double(s) {
                Some(f) => Value::float(f),
                None => raise_number_format(vm, s.trim()),
            },
            _ => Value::float(as_f64(&v)),
        },
        "BigDecimal" | "BigInteger" => {
            // `as BigInteger` truncates any fraction, which is Java's
            // `BigDecimal.toBigInteger`.
            let carry = |d: BigDecimal| {
                if ty_simple == "BigInteger" {
                    bigint_value(d)
                } else {
                    dec_value(d)
                }
            };
            match &v {
                Value::Str(s) => match decimal::parse_java(s.trim()) {
                    Ok(d) => carry(d),
                    Err(msg) => {
                        raise_opt(vm, "NumberFormatException", msg.as_deref());
                        Value::Undef
                    }
                },
                Value::Int(n) => carry(decimal::from_i64(*n)),
                _ => match as_dec(&v).or_else(|| decimal::from_f64_exact(as_f64(&v))) {
                    Some(d) => carry(d),
                    None => v,
                },
            }
        }
        "String" => Value::str(render_value(vm, &v)),
        "boolean" | "Boolean" => Value::bool(groovy_truthy(vm, &v)),
        // A `char` cast takes the code point (`255 as char` is `ÿ`).
        "char" | "Character" => match as_i64(&v) {
            Some(n) => Value::str(
                char::from_u32(n as u32)
                    .map(String::from)
                    .unwrap_or_default(),
            ),
            None => Value::str(
                groovy_str(&v)
                    .chars()
                    .next()
                    .map(String::from)
                    .unwrap_or_default(),
            ),
        },
        // A Groovy `Range` already *is* a `java.util.List`, so the cast is
        // identity: `(1..3) as List` is still the `IntRange` `1..3`.
        "List" | "ArrayList" | "Collection" | "Iterable" => match &v {
            Value::Array(_) => v,
            _ if as_range(&v).is_some() => v,
            other => Value::array(iteration_elements(other)),
        },
        // `x as Set` builds a `LinkedHashSet`, so it keeps the source's order —
        // `[10, 3, 7, 1] as Set` prints `[10, 3, 7, 1]`, not the `HashSet`
        // ordering `toSet()` would give it.
        "Set" | "LinkedHashSet" | "SortedSet" | "TreeSet" | "HashSet" => {
            let seed = iteration_elements(&v);
            make_set(
                seed.clone(),
                match ty_simple.as_str() {
                    "TreeSet" | "SortedSet" => SetKind::Tree,
                    "HashSet" => SetKind::Hash {
                        req: hash_req_for_collection(seed.len()),
                    },
                    _ => SetKind::Linked,
                },
            )
        }
        _ => v,
    }
}

/// The JDK classes a script names statically (`Math.max`, `Integer.parseInt`),
/// mapped to the package they live in. The compiler consults this to decide
/// whether a bare capitalised identifier is a class reference rather than an
/// undeclared variable.
pub fn jdk_class_package(name: &str) -> Option<&'static str> {
    Some(match name {
        "Math" | "Integer" | "Long" | "Double" | "Float" | "Short" | "Byte" | "Boolean"
        | "Character" | "String" | "StringBuilder" | "System" | "Object" | "Number" | "Thread"
        | "Runtime" => "java.lang",
        "BigDecimal" | "BigInteger" => "java.math",
        "Collections" | "Arrays" | "List" | "Map" | "Set" | "Random" | "UUID" => "java.util",
        _ => return None,
    })
}

/// `GCLASSREF`: build a `java.lang.Class` handle for a statically named class.
fn b_classref(vm: &mut VM, _argc: u8) -> Value {
    let name = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let qualified = match jdk_class_package(&name) {
        Some(pkg) => format!("{pkg}.{name}"),
        None => name,
    };
    heap_push(HeapObj::ClassRef(qualified))
}

/// `Integer.toString(int, int radix)` / `Long.toString(long, int radix)`: the
/// value in `radix`, sign-prefixed, lowercase digits. Java falls back to base 10
/// for a radix outside `Character.MIN_RADIX`..`Character.MAX_RADIX` (2..36)
/// instead of raising — `Integer.toString(255, 1)` and `(255, 37)` are both
/// `255`.
fn java_radix_string(n: i64, radix: i64) -> String {
    let radix = if (2..=36).contains(&radix) { radix } else { 10 } as u64;
    let negative = n < 0;
    // Read the magnitude unsigned so `Long.MIN_VALUE` (whose positive
    // counterpart is not an `i64`) still renders.
    let mut mag = n.unsigned_abs();
    let mut digits = Vec::new();
    loop {
        digits.push(std::char::from_digit((mag % radix) as u32, radix as u32).unwrap());
        mag /= radix;
        if mag == 0 {
            break;
        }
    }
    if negative {
        digits.push('-');
    }
    digits.iter().rev().collect()
}

/// `Integer.toHexString` and friends, which render the value's *unsigned*
/// two's-complement bit pattern at its own Java width: `Integer.toHexString(-1)`
/// is `ffffffff`, `Long.toHexString(-1L)` all sixteen. Rendering an `Integer` at
/// 64 bits — what a bare `format!("{n:x}")` does — is why `-1` printed sixteen
/// `f`s here before.
fn java_unsigned_radix_string(n: i64, radix: u32, wide: bool) -> String {
    let bits: u64 = if wide { n as u64 } else { u64::from(n as u32) };
    match radix {
        2 => format!("{bits:b}"),
        8 => format!("{bits:o}"),
        _ => format!("{bits:x}"),
    }
}

/// `Integer.parseInt(String, int radix)`: a signed parse in `radix`, digits
/// case-insensitive. `None` on any text Java rejects, which the caller turns into
/// `NumberFormatException`.
fn java_parse_radix(text: &str, radix: i64) -> Option<i64> {
    if !(2..=36).contains(&radix) {
        return None;
    }
    let t = text.trim();
    let (negative, body) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if body.is_empty() {
        return None;
    }
    let mut acc: i64 = 0;
    for c in body.chars() {
        let d = c.to_digit(radix as u32)?;
        acc = acc.checked_mul(radix)?.checked_add(i64::from(d))?;
    }
    Some(if negative { -acc } else { acc })
}

/// The static methods of the JDK classes a Groovy script actually calls.
/// Returns `None` when `class`/`method` is not one of them, so the caller falls
/// through to `MissingMethodException`.
fn dispatch_static(vm: &mut VM, class: &str, method: &str, args: &[Value]) -> Option<Value> {
    let arg0 = args.first().cloned().unwrap_or(Value::Undef);
    let f0 = as_f64(&arg0);
    Some(match (class, method) {
        // `Math.round` is half-up on a double and answers a `long`; every other
        // `Math` entry point is IEEE and answers a `double`.
        ("Math", "round") => Value::int(java_round(f0)),
        ("Math", "abs") => match as_i64(&arg0) {
            Some(n) => Value::int(abs_at_width(n)),
            None => Value::float(f0.abs()),
        },
        ("Math", "max" | "min") => {
            let f1 = as_f64(args.get(1).unwrap_or(&Value::Undef));
            let pick_max = method == "max";
            match (as_i64(&arg0), args.get(1).and_then(as_i64)) {
                (Some(a), Some(b)) => Value::int(if pick_max { a.max(b) } else { a.min(b) }),
                _ => Value::float(if pick_max { f0.max(f1) } else { f0.min(f1) }),
            }
        }
        ("Math", "sqrt") => Value::float(f0.sqrt()),
        ("Math", "cbrt") => Value::float(f0.cbrt()),
        ("Math", "floor") => Value::float(f0.floor()),
        ("Math", "ceil") => Value::float(f0.ceil()),
        ("Math", "rint") => Value::float(f0.round_ties_even()),
        ("Math", "signum") => Value::float(f0.signum()),
        ("Math", "exp") => Value::float(f0.exp()),
        ("Math", "log") => Value::float(f0.ln()),
        ("Math", "log10") => Value::float(f0.log10()),
        ("Math", "sin") => Value::float(f0.sin()),
        ("Math", "cos") => Value::float(f0.cos()),
        ("Math", "tan") => Value::float(f0.tan()),
        ("Math", "atan2") => Value::float(f0.atan2(as_f64(args.get(1)?))),
        ("Math", "hypot") => Value::float(f0.hypot(as_f64(args.get(1)?))),
        ("Math", "toRadians") => Value::float(f0.to_radians()),
        ("Math", "toDegrees") => Value::float(f0.to_degrees()),
        ("Math", "pow") => Value::float(f0.powf(as_f64(args.get(1)?))),
        ("Math", "random") => return None,

        // `parseInt`/`parseLong`/`valueOf` take an optional radix, so
        // `Integer.parseInt("ff", 16)` is `255` — the second argument used to be
        // dropped, which turned every non-decimal parse into a
        // `NumberFormatException`.
        ("Integer" | "Long" | "Short" | "Byte", "parseInt" | "parseLong" | "valueOf") => {
            let text = groovy_str(&arg0);
            let parsed = match args.get(1).and_then(as_i64) {
                Some(radix) => java_parse_radix(&text, radix),
                None => text.trim().parse::<i64>().ok(),
            };
            match parsed {
                Some(n) => Value::int(n),
                None => raise_number_format(vm, text.trim()),
            }
        }
        // Java's overload resolution admits `255.toString(16)` as the *static*
        // `Integer.toString(int)`, which renders its argument in base 10 and
        // ignores the receiver — so Groovy prints `16`, not `ff`. The two-arg
        // form is the one that takes a radix.
        // Both parameters are `int`, so a non-integer argument matches no
        // overload and raises — `255.toString('x')` is a MissingMethodException,
        // not a base-10 zero.
        ("Integer" | "Long", "toString")
            if !args.is_empty() && args.iter().all(|a| matches!(a, Value::Int(_))) =>
        {
            let n = as_i64(&arg0).unwrap_or(0);
            match args.get(1).and_then(as_i64) {
                Some(radix) => Value::str(java_radix_string(n, radix)),
                None => Value::str(n.to_string()),
            }
        }
        ("Double" | "Float", "parseDouble" | "parseFloat" | "valueOf") => {
            let text = groovy_str(&arg0);
            match parse_java_double(&text) {
                Some(f) => Value::float(f),
                None => raise_number_format(vm, text.trim()),
            }
        }
        // The unsigned renderings fill to the *named class's* width, so
        // `Integer.toHexString(-1)` is `ffffffff` and `Long.toHexString(-1L)` is
        // sixteen `f`s.
        ("Integer" | "Long", "toBinaryString" | "toHexString" | "toOctalString") => {
            let radix = match method {
                "toBinaryString" => 2,
                "toOctalString" => 8,
                _ => 16,
            };
            Value::str(java_unsigned_radix_string(
                as_i64(&arg0).unwrap_or(0),
                radix,
                class == "Long",
            ))
        }
        ("Boolean", "parseBoolean" | "valueOf") => {
            Value::bool(groovy_str(&arg0).eq_ignore_ascii_case("true"))
        }
        ("String", "valueOf") => Value::str(render_value(vm, &arg0)),
        ("String", "format") => Value::str(java_format(vm, &groovy_str(&arg0), &args[1..])),
        _ => return None,
    })
}

/// The static fields a script reads off a JDK class (`Integer.MAX_VALUE`).
fn static_field(class: &str, name: &str) -> Option<Value> {
    Some(match (class, name) {
        ("Integer", "MAX_VALUE") => Value::int(i32::MAX as i64),
        ("Integer", "MIN_VALUE") => Value::int(i32::MIN as i64),
        ("Long", "MAX_VALUE") => Value::int(i64::MAX),
        ("Long", "MIN_VALUE") => Value::int(i64::MIN),
        ("Short", "MAX_VALUE") => Value::int(i16::MAX as i64),
        ("Short", "MIN_VALUE") => Value::int(i16::MIN as i64),
        ("Byte", "MAX_VALUE") => Value::int(i8::MAX as i64),
        ("Byte", "MIN_VALUE") => Value::int(i8::MIN as i64),
        ("Double", "MAX_VALUE") => Value::float(f64::MAX),
        ("Double", "MIN_VALUE") => Value::float(f64::MIN_POSITIVE),
        ("Double", "POSITIVE_INFINITY") => Value::float(f64::INFINITY),
        ("Double", "NEGATIVE_INFINITY") => Value::float(f64::NEG_INFINITY),
        ("Double", "NaN") => Value::float(f64::NAN),
        ("Math", "PI") => Value::float(std::f64::consts::PI),
        ("Math", "E") => Value::float(std::f64::consts::E),
        _ => return None,
    })
}

/// A faithful-enough `java.util.Formatter`: the conversions a Groovy script
/// writes (`%s`, `%d`, `%f`/`%.Nf`, `%x`, `%o`, `%b`, `%%`, `%n`), with width
/// and left-justification. An unmodeled conversion is copied through verbatim
/// rather than guessed at.
fn java_format(vm: &mut VM, spec: &str, args: &[Value]) -> String {
    let mut out = String::new();
    let mut next = 0usize;
    let mut it = spec.chars().peekable();
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        // Collect flags, width and precision up to the conversion character.
        let mut flags = String::new();
        while matches!(it.peek(), Some('-' | '+' | '0' | ' ' | ',' | '#')) {
            flags.push(it.next().unwrap());
        }
        let mut width = String::new();
        while matches!(it.peek(), Some(d) if d.is_ascii_digit()) {
            width.push(it.next().unwrap());
        }
        let mut precision = String::new();
        if matches!(it.peek(), Some('.')) {
            it.next();
            while matches!(it.peek(), Some(d) if d.is_ascii_digit()) {
                precision.push(it.next().unwrap());
            }
        }
        let Some(conv) = it.next() else { break };
        if conv == '%' {
            out.push('%');
            continue;
        }
        if conv == 'n' {
            out.push('\n');
            continue;
        }
        let arg = args.get(next).cloned().unwrap_or(Value::Undef);
        next += 1;
        let body = match conv {
            'd' => as_i64(&arg)
                .map(|n| n.to_string())
                .unwrap_or_else(|| groovy_str(&arg)),
            'f' | 'e' | 'g' => {
                let digits: usize = precision.parse().unwrap_or(6);
                // Java rounds HALF_UP on the argument's *exact* value, so a
                // `BigDecimal` argument (every unsuffixed Groovy literal) rounds
                // off the decimal — `"%.2f"` of `1.005` is `1.01`, not the
                // `1.00` the nearest double would give.
                match as_dec(&arg) {
                    Some(d) => {
                        decimal::to_groovy_string(&decimal::round_half_up(&d, digits as i64))
                    }
                    None => format!("{:.*}", digits, as_f64(&arg)),
                }
            }
            'x' => format!("{:x}", as_i64(&arg).unwrap_or(0)),
            'X' => format!("{:X}", as_i64(&arg).unwrap_or(0)),
            'o' => format!("{:o}", as_i64(&arg).unwrap_or(0)),
            'b' => groovy_truthy(vm, &arg).to_string(),
            's' | 'S' => {
                let s = render_value(vm, &arg);
                if conv == 'S' {
                    s.to_uppercase()
                } else {
                    s
                }
            }
            _ => groovy_str(&arg),
        };
        let w: usize = width.parse().unwrap_or(0);
        if body.chars().count() >= w {
            out.push_str(&body);
        } else if flags.contains('-') {
            out.push_str(&body);
            out.push_str(&" ".repeat(w - body.chars().count()));
        } else {
            let fill = if flags.contains('0') && conv != 's' {
                "0"
            } else {
                " "
            };
            out.push_str(&fill.repeat(w - body.chars().count()));
            out.push_str(&body);
        }
    }
    out
}

/// `GRANGE`: build a range literal's `groovy.lang.Range` object.
fn b_range(vm: &mut VM, _argc: u8) -> Value {
    let inclusive = matches!(vm.stack.pop(), Some(Value::Bool(true)));
    let to = vm.stack.pop().unwrap_or(Value::Undef);
    let from = vm.stack.pop().unwrap_or(Value::Undef);
    heap_push(HeapObj::Range(RangeVal {
        from,
        to,
        inclusive,
    }))
}

/// Clone the range behind a handle, if `v` is one.
fn as_range(v: &Value) -> Option<RangeVal> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::Range(r)) => Some(r.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// A range's endpoints read as code points, when both are single-character
/// strings — Groovy's `ObjectRange` over characters (`'a'..'e'`).
fn range_char_ends(r: &RangeVal) -> Option<(i64, i64)> {
    match (&r.from, &r.to) {
        (Value::Str(a), Value::Str(b)) if a.chars().count() == 1 && b.chars().count() == 1 => {
            Some((
                a.chars().next().unwrap() as i64,
                b.chars().next().unwrap() as i64,
            ))
        }
        _ => None,
    }
}

/// A range's endpoints as the integers its walk counts between. A decimal
/// endpoint truncates to its integer part, as Groovy's `NumberRange` iteration
/// does for whole steps.
fn range_bounds(r: &RangeVal) -> (i64, i64) {
    if let Some(pair) = range_char_ends(r) {
        return pair;
    }
    match (as_i64(&r.from), as_i64(&r.to)) {
        (Some(a), Some(b)) => (a, b),
        _ => (
            as_dec(&r.from)
                .map(|d| decimal::truncate_to_i64(&d))
                .unwrap_or(0),
            as_dec(&r.to)
                .map(|d| decimal::truncate_to_i64(&d))
                .unwrap_or(0),
        ),
    }
}

/// Whether a range counts down — `5..1` and `'e'..'a'` do.
fn range_is_reverse(r: &RangeVal) -> bool {
    let (from, to) = range_bounds(r);
    from > to
}

/// What `isReverse()` answers, which is the walk direction everywhere except an
/// `ObjectRange` that collapses to a single value: `('f'..<'e')` walks just `f`,
/// and Groovy calls that forward. The numeric ranges keep the direction they
/// were written with, so `(4..<3)` and `(1.0..<0.0)` both stay reverse.
fn range_reported_reverse(r: &RangeVal) -> bool {
    if range_char_ends(r).is_some() && range_size(r) == 1 {
        return false;
    }
    range_is_reverse(r)
}

/// Order two range endpoints. Integers and `BigDecimal`s compare numerically,
/// single-character strings by code point. `None` when the pair has no ordering,
/// which stops a walk rather than looping.
fn range_cmp(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        _ => match (as_dec(a), as_dec(b)) {
            (Some(x), Some(y)) => Some(decimal::cmp(&x, &y)),
            _ => None,
        },
    }
}

/// How many values a range enumerates, by Groovy's own arithmetic:
/// `floor(|to - from|)` plus one when the range is inclusive.
///
/// This is *not* always `range_elements(r).len()`, and the difference is
/// Groovy's, not ours: `(1.5..<4.0).size()` answers 2 while `toList()` walks
/// three values, because `size` divides the span and the walk steps by one.
fn range_size(r: &RangeVal) -> i64 {
    let span = match (as_dec(&r.from), as_dec(&r.to)) {
        (Some(a), Some(b)) => decimal::truncate_to_i64(&decimal::abs(&decimal::sub(&b, &a))),
        _ => {
            let (a, b) = range_bounds(r);
            (b - a).abs()
        }
    };
    span + i64::from(r.inclusive)
}

/// Whether a range enumerates nothing — only an exclusive range with equal
/// endpoints does, and Groovy gives that one its own class.
fn range_is_empty(r: &RangeVal) -> bool {
    !r.inclusive && range_cmp(&r.from, &r.to) == Some(std::cmp::Ordering::Equal)
}

/// A range's lower bound — its `from` property. Groovy normalises this to the
/// *smaller* end of what the range actually enumerates, so `(4..0).from` is 0
/// and `(4..<0).from` is 1. A `NumberRange` is the exception: it keeps the
/// endpoints as written and records exclusivity separately, so `(1.5..<4.0).from`
/// stays 1.5.
fn range_lower(r: &RangeVal) -> Value {
    range_bound(r, false)
}

/// A range's upper bound — its `to` property. The mirror of [`range_lower`]:
/// `(0..<4).to` is 3, and `(1.5..<4.0).to` stays 4.0.
fn range_upper(r: &RangeVal) -> Value {
    range_bound(r, true)
}

fn range_bound(r: &RangeVal, upper: bool) -> Value {
    let desc = range_is_reverse(r);
    let (lo, hi) = if desc {
        (&r.to, &r.from)
    } else {
        (&r.from, &r.to)
    };
    let end = if upper { hi } else { lo };
    // The end an exclusive walk stops short of is always the *second* endpoint
    // written, whichever side of the order that puts it on. An inclusive range,
    // an empty one, and a `NumberRange` all report their endpoint unadjusted.
    let excluded = !r.inclusive
        && !range_is_empty(r)
        && range_class(r) != "groovy.lang.NumberRange"
        && upper != desc;
    if excluded {
        successor(end, desc).unwrap_or_else(|| end.clone())
    } else {
        end.clone()
    }
}

/// Groovy's `next()` / `previous()`: the successor or predecessor of an ordered
/// value. Integers and `BigDecimal`s move by one (a `BigInteger` keeps its
/// type); a `String` moves its *last* character by one code point, so
/// `'a'.next()` is `'b'` and `'z'.next()` is `'{'`. `None` for a value with no
/// ordering, which falls through to the per-type table and then to
/// `MissingMethodException`.
fn successor(v: &Value, forward: bool) -> Option<Value> {
    let delta: i64 = if forward { 1 } else { -1 };
    match v {
        Value::Int(n) => Some(Value::int(n.wrapping_add(delta))),
        Value::Str(s) => {
            let mut chars: Vec<char> = s.chars().collect();
            let last = chars.pop()?;
            chars.push(char::from_u32((last as i64 + delta) as u32)?);
            Some(Value::str(chars.into_iter().collect::<String>()))
        }
        _ => {
            let d = as_dec(v)?;
            let stepped = decimal::add(&d, &decimal::from_i64(delta));
            Some(if as_bigint(v).is_some() {
                bigint_value(stepped)
            } else {
                dec_value(stepped)
            })
        }
    }
}

/// The values a range enumerates — what iterating it yields and what every
/// list-shaped GDK method on it runs over. `5..1` counts down, `'a'..'e'` walks
/// characters, and the exclusive form drops the endpoint from whichever end the
/// walk finishes on.
fn range_elements(r: &RangeVal) -> Vec<Value> {
    use std::cmp::Ordering;
    let desc = range_is_reverse(r);
    let mut out = Vec::new();
    let mut cur = r.from.clone();
    // Walk from the endpoint written first toward the other one, stepping with
    // `next`/`previous`. Stepping rather than renumbering is what keeps the
    // element *type* — `1.5..4.0` walks `1.5, 2.5, 3.5`, not `1, 2, 3`.
    while let Some(ord) = range_cmp(&cur, &r.to) {
        let past = match (desc, ord) {
            (false, Ordering::Greater) | (true, Ordering::Less) => true,
            (_, Ordering::Equal) => !r.inclusive,
            _ => false,
        };
        if past {
            break;
        }
        out.push(cur.clone());
        match successor(&cur, !desc) {
            Some(next) => cur = next,
            None => break,
        }
    }
    out
}

/// The methods a `groovy.lang.Range` answers *as a range* rather than as the
/// list it enumerates. `None` hands the call on to the list, which is where
/// `collect`, `each`, `sum`, `join`, `head`, and the rest are already modeled.
///
/// `step` and `reverse` answer a `java.util.ArrayList`, not another range, which
/// is what Groovy's own `Range.step` / `DefaultGroovyMethods.reverse` return.
fn dispatch_range_method(vm: &mut VM, r: &RangeVal, method: &str, args: &[Value]) -> Option<Value> {
    let elems = || range_elements(r);
    Some(match method {
        // `Range.subList` answers another **range**, not the list a range
        // usually delegates to: `(1..5).subList(1, 3)` is `2..3`, and the empty
        // window is the `EmptyRange` `1..<1` — built from the range's *lower*
        // bound, not from the window's position.
        "subList" => return range_sublist(vm, r, args),
        // Answered here rather than falling through, because the list a range
        // delegates to is an `ArrayList` and would name the wrong class.
        "getClass" => heap_push(HeapObj::ClassRef(range_class(r).to_string())),
        // `from`/`to` are the range's *bounds*, not the endpoints as written:
        // `(4..0).getFrom()` is 0 and `getTo()` is 4, because Groovy's
        // `IntRange` normalises them and records the direction separately.
        "getFrom" => range_lower(r),
        "getTo" => range_upper(r),
        // `isReverse` asks whether the range counts *down*, which is a property
        // of the endpoints, not of the `..<` form: `(1..<5).isReverse()` is
        // false.
        "isReverse" => Value::bool(range_reported_reverse(r)),
        "toString" | "inspect" => Value::str(range_str(r)),
        "size" | "getSize" => Value::int(range_size(r)),
        "step" => {
            let n = args.first().and_then(as_i64).unwrap_or(1);
            Value::array(range_step(r, n))
        }
        "contains" => {
            let needle = args.first().cloned().unwrap_or(Value::Undef);
            Value::bool(elems().iter().any(|e| values_equal(e, &needle)))
        }
        _ => return None,
    })
}

/// `Range.subList(from, to)` — the half-open window, answered as another
/// `Range`.
///
/// The window is taken over the range's **ascending** elements even when the
/// range counts down, which is Groovy's own indexing here and is *not* the
/// indexing its subscript uses: `(5..1)[1]` is `4`, while `(5..1).subList(1, 2)`
/// is `2..2`. Groovy's `IntRange.subList` builds `IntRange(from + fromIndex,
/// from + toIndex - 1, reverse)` from the normalised lower bound and re-applies
/// the direction afterwards, so the index counts up from the low end regardless.
/// Reading the ascending elements rather than doing the arithmetic gets the same
/// answer for every endpoint type groovyrs models — including `1.5..4.5`, whose
/// elements step by one from a decimal — without a second numeric path.
///
/// An empty window is Groovy's `EmptyRange`, whose sole endpoint is the range's
/// lower bound rather than anything about where the window sat: both
/// `(1..5).subList(2, 2)` and `(5..1).subList(2, 2)` are `1..<1`.
///
/// Bounds are the JDK's, checked in the JDK's order — see the list `subList`.
fn range_sublist(vm: &mut VM, r: &RangeVal, args: &[Value]) -> Option<Value> {
    let from = args.first().and_then(as_i64).unwrap_or(0);
    let to = args.get(1).and_then(as_i64).unwrap_or(0);
    let size = range_size(r);
    if from < 0 {
        raise(
            vm,
            "IndexOutOfBoundsException",
            &format!("fromIndex = {from}"),
        );
        return Some(Value::Undef);
    }
    if to > size {
        raise(vm, "IndexOutOfBoundsException", &format!("toIndex = {to}"));
        return Some(Value::Undef);
    }
    if from > to {
        raise(
            vm,
            "IllegalArgumentException",
            &format!("fromIndex({from}) > toIndex({to})"),
        );
        return Some(Value::Undef);
    }
    let lower = range_lower(r);
    if from == to {
        return Some(heap_push(HeapObj::Range(RangeVal {
            from: lower.clone(),
            to: lower,
            inclusive: false,
        })));
    }
    let mut asc = range_elements(r);
    if range_is_reverse(r) {
        asc.reverse();
    }
    // `from < to <= size` holds here, and `asc` is `size` long, so both indices
    // are in range.
    let (lo, hi) = (asc[from as usize].clone(), asc[to as usize - 1].clone());
    let (a, b) = if range_is_reverse(r) {
        (hi, lo)
    } else {
        (lo, hi)
    };
    Some(heap_push(HeapObj::Range(RangeVal {
        from: a,
        to: b,
        inclusive: true,
    })))
}

/// `Range.step(n)` — every `n`-th element, as a `java.util.ArrayList`. A
/// negative step walks the range from its far end back, so `(1..5).step(-2)` is
/// `[5, 3, 1]`.
fn range_step(r: &RangeVal, step: i64) -> Vec<Value> {
    if step == 0 {
        return Vec::new();
    }
    let mut seq = range_elements(r);
    if step < 0 {
        seq.reverse();
    }
    seq.into_iter()
        .step_by(step.unsigned_abs() as usize)
        .collect()
}

// ── `java.util.Set` ─────────────────────────────────────────────────────────
//
// A set stores its elements in insertion order and presents them in the order
// its [`SetKind`] dictates. Everything below is verified against Apache Groovy
// 5.0.8; the `HashSet` ordering in particular is a model of the JDK's table
// layout rather than a guess, and the probes in `parity-scripts/probes.txt`
// pin it.

/// The set behind a handle: its elements in insertion order and its kind.
fn as_set(v: &Value) -> Option<(Vec<Value>, SetKind)> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::SetVal { items, kind }) => Some((items.clone(), *kind)),
            _ => None,
        }),
        _ => None,
    }
}

/// Java's `hashCode` for the element kinds a set can order by. `None` for a
/// value whose hash is the JVM's identity hash — not reproducible here, and not
/// reproducible across two JVM runs either — which keeps insertion order.
fn java_hash(v: &Value) -> Option<i32> {
    match v {
        // `Integer.hashCode` is the value; `Long.hashCode` folds the halves.
        Value::Int(n) => Some(if (i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(n) {
            *n as i32
        } else {
            (*n ^ ((*n as u64) >> 32) as i64) as i32
        }),
        Value::Bool(b) => Some(if *b { 1231 } else { 1237 }),
        // `null` hashes to 0 both as a `HashMap` key and as a `List` element
        // (`Objects.hashCode(null)`), though a bare `null.hashCode()` throws.
        Value::Undef => Some(0),
        // `List.hashCode` — `31 * acc + element.hashCode()` from 1. This is what
        // puts a `HashSet<List>` (`permutations()`, `subsequences()`) in the
        // order Groovy prints. An element with no reproducible hash makes the
        // whole list's unreproducible too.
        Value::Array(items) => items.iter().try_fold(1i32, |h, e| {
            Some(h.wrapping_mul(31).wrapping_add(java_hash(e)?))
        }),
        // `String.hashCode` — `s[0]*31^(n-1) + …`, wrapping at 32 bits.
        Value::Str(s) => Some(
            s.encode_utf16()
                .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c))),
        ),
        _ => None,
    }
}

/// The JDK's `HashMap.tableSizeFor` — the least power of two at or above `n`.
fn table_size_for(n: usize) -> usize {
    let mut cap = 1usize;
    while cap < n {
        cap <<= 1;
    }
    cap.max(1)
}

/// The initial capacity a bare `new HashSet<>()` asks for — the JDK's
/// `DEFAULT_INITIAL_CAPACITY`. The GDK methods that answer a set built that way
/// (`permutations`, `subsequences`) iterate against this table size.
const DEFAULT_HASH_REQ: usize = 16;

/// The initial capacity `new HashSet(Collection c)` asks for:
/// `Math.max((int) (c.size() / .75f) + 1, 16)`. `toSet()` does *not* go through
/// this — it asks for the collection's size — and the two really do iterate
/// differently as a result: `[17,5,33,2,20,9].toSet()` is
/// `[17, 33, 9, 2, 20, 5]` while `new HashSet([17,5,33,2,20,9])` is
/// `[17, 33, 2, 20, 5, 9]`, because the first lands in an 8-slot table and the
/// second in a 16-slot one.
fn hash_req_for_collection(n: usize) -> usize {
    (((n as f32) / 0.75) as usize + 1).max(16)
}

/// The order a `HashSet` iterates `items` in, as indices into `items`.
///
/// The JDK lays entries out in a power-of-two table indexed by
/// `(capacity - 1) & (h ^ (h >>> 16))`, appends within a bucket, and preserves
/// relative order across a resize; iteration then walks bucket 0 upward. So the
/// order is exactly a **stable sort of the insertion sequence by bucket index**.
/// The table starts at `table_size_for(req)` and doubles while the element count
/// exceeds three quarters of it, which is the resize rule.
///
/// Not modeled, and not reproducible in Java either: a bucket that treeifies
/// (8 collisions with a table of 64+), and an element whose hash is the JVM
/// identity hash — [`java_hash`] answers `None` there and the element keeps its
/// insertion position.
fn hash_order(items: &[Value], req: usize) -> Vec<usize> {
    let n = items.len();
    let mut cap = table_size_for(req);
    while n > cap * 3 / 4 {
        cap *= 2;
    }
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by_key(|&i| {
        let h = java_hash(&items[i]).unwrap_or(0) as u32;
        ((cap as u32 - 1) & (h ^ (h >> 16))) as usize
    });
    idx
}

/// A set's elements in the order it presents them — what iterating it yields
/// and what `toString` lays out.
fn set_elements(items: &[Value], kind: SetKind) -> Vec<Value> {
    match kind {
        SetKind::Linked => items.to_vec(),
        SetKind::Hash { req } => hash_order(items, req)
            .into_iter()
            .map(|i| items[i].clone())
            .collect(),
        SetKind::Tree => {
            let mut out = items.to_vec();
            out.sort_by(natural_order);
            out
        }
    }
}

/// Build a set handle from `items`, dropping later duplicates — which is what
/// makes `[1, 2, 2, 3] as Set` three elements and, applied to every operator
/// result, what makes the set operators re-de-duplicate.
fn make_set(items: Vec<Value>, kind: SetKind) -> Value {
    heap_push(HeapObj::SetVal {
        items: dedup_values(items),
        kind,
    })
}

/// `items` with every later duplicate dropped — `Set.add`'s answer for an
/// element already present is `false`, and it leaves the first one in place.
fn dedup_values(items: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for v in items {
        if !out.iter().any(|k| values_equal(k, &v)) {
            out.push(v);
        }
    }
    out
}

/// Replace a set handle's elements in place, keeping its kind. This is how a
/// mutator (`add`, `remove`, `clear`) is visible to every holder of the handle,
/// the way `java.util.Set`'s are.
fn set_store(v: &Value, items: Vec<Value>) {
    let Value::Obj(id) = v else { return };
    HEAP.with(|h| {
        if let Some(HeapObj::SetVal { items: dst, .. }) = h.borrow_mut().get_mut(*id as usize) {
            *dst = items;
        }
    });
}

/// The methods a `Set` answers **as a set** rather than as the list it
/// enumerates. `None` hands the call on to that list, which is where `collect`,
/// `sort`, `sum`, `join`, `find`, `max`, and the rest already live — and which
/// is faithful, because those all answer a `java.util.ArrayList` in Groovy too
/// (`([3, 1] as Set).collect { it }` is an `ArrayList`, not a set).
///
/// What has to be answered here is everything whose result is *itself* a set,
/// plus the mutators, which write through the handle.
fn dispatch_set_method(
    vm: &mut VM,
    recv: &Value,
    items: &[Value],
    kind: SetKind,
    method: &str,
    args: &[Value],
) -> Option<Value> {
    let ordered = || set_elements(items, kind);
    let other = || args.first().map(iteration_elements).unwrap_or_default();
    Some(match method {
        "getClass" => heap_push(HeapObj::ClassRef(set_class(kind).to_string())),
        "toString" | "inspect" => Value::str(groovy_str(recv)),
        "size" | "getSize" => Value::int(items.len() as i64),
        "isEmpty" => Value::bool(items.is_empty()),
        "contains" => Value::bool(items.iter().any(|v| values_equal(v, &first_arg(args)))),
        // The set operators. Each answers a set of the receiver's kind with the
        // duplicates dropped, which is the whole point of the type: a list's
        // `+` concatenates, a set's unions.
        "plus" | "minus" | "intersect" | "unique" | "toSet" | "asImmutable" | "asSynchronized"
        | "clone" => {
            let o = other();
            make_set(
                match method {
                    "plus" => ordered().into_iter().chain(o).collect(),
                    "minus" => ordered()
                        .into_iter()
                        .filter(|v| !o.iter().any(|w| values_equal(v, w)))
                        .collect(),
                    "intersect" => ordered()
                        .into_iter()
                        .filter(|v| o.iter().any(|w| values_equal(v, w)))
                        .collect(),
                    _ => ordered(),
                },
                kind,
            )
        }
        // `findAll`/`grep` keep the receiver's type; `collect` does not, so it
        // falls through to the list. These take a closure, so they route through
        // `dispatch_call` — the closure-driven GDK lives there, not in
        // `dispatch_method`.
        "findAll" | "grep" => {
            let listed = dispatch_call(vm, Value::array(ordered()), method, args.to_vec());
            make_set(iteration_elements(&listed), kind)
        }
        // `add` answers whether the set changed, and mutates through the handle.
        "add" | "leftShift" => {
            let v = first_arg(args);
            let present = items.iter().any(|k| values_equal(k, &v));
            if !present {
                let mut next = items.to_vec();
                next.push(v);
                set_store(recv, next);
            }
            if method == "leftShift" {
                recv.clone()
            } else {
                Value::bool(!present)
            }
        }
        "remove" => {
            let v = first_arg(args);
            let next: Vec<Value> = items
                .iter()
                .filter(|k| !values_equal(k, &v))
                .cloned()
                .collect();
            let changed = next.len() != items.len();
            set_store(recv, next);
            Value::bool(changed)
        }
        "addAll" | "removeAll" | "retainAll" => {
            let o = other();
            let next: Vec<Value> = match method {
                "addAll" => items.iter().cloned().chain(o).collect(),
                "removeAll" => items
                    .iter()
                    .filter(|k| !o.iter().any(|w| values_equal(k, w)))
                    .cloned()
                    .collect(),
                _ => items
                    .iter()
                    .filter(|k| o.iter().any(|w| values_equal(k, w)))
                    .cloned()
                    .collect(),
            };
            let mut deduped: Vec<Value> = Vec::new();
            for v in next {
                if !deduped.iter().any(|k| values_equal(k, &v)) {
                    deduped.push(v);
                }
            }
            let changed = deduped.len() != items.len();
            set_store(recv, deduped);
            Value::bool(changed)
        }
        "clear" => {
            set_store(recv, Vec::new());
            Value::Undef
        }
        // `each` answers the receiver, not the list it delegated to.
        "each" | "eachWithIndex" | "reverseEach" => {
            dispatch_call(vm, Value::array(ordered()), method, args.to_vec());
            recv.clone()
        }
        _ => return None,
    })
}

/// The first argument, or `null` when there is none.
fn first_arg(args: &[Value]) -> Value {
    args.first().cloned().unwrap_or(Value::Undef)
}

/// The qualified class name a set kind reports.
fn set_class(kind: SetKind) -> &'static str {
    match kind {
        SetKind::Linked => "java.util.LinkedHashSet",
        SetKind::Hash { .. } => "java.util.HashSet",
        SetKind::Tree => "java.util.TreeSet",
    }
}

/// A range as the list it enumerates, or the value unchanged when it is not one.
/// This is what lets every list operation groovyrs already models — `+`, `==`,
/// `collect`, subscripting, `instanceof List` — apply to a range for free, which
/// is faithful because Groovy's `Range` *is* a `java.util.List`.
fn range_as_list(v: &Value) -> Value {
    match as_range(v) {
        Some(r) => Value::array(range_elements(&r)),
        None => v.clone(),
    }
}

/// `Range.toString()` — the source form (`1..5`, `1..<5`, `5..1`, `a..e`), which
/// is also how `println` and `String +` render one.
fn range_str(r: &RangeVal) -> String {
    // An `ObjectRange` renders the values it actually walks — `('a'..<'e')`
    // prints `a..d` — where the numeric ranges keep the form they were written
    // in. An empty range keeps its `..<` whatever its class.
    if range_class(r) == "groovy.lang.ObjectRange" {
        let last = if r.inclusive {
            r.to.clone()
        } else {
            successor(&r.to, range_is_reverse(r)).unwrap_or_else(|| r.to.clone())
        };
        return format!("{}..{}", groovy_str(&r.from), groovy_str(&last));
    }
    format!(
        "{}{}{}",
        groovy_str(&r.from),
        if r.inclusive { ".." } else { "..<" },
        groovy_str(&r.to)
    )
}

/// The `groovy.lang.Range` subclass a range's endpoints put it in: integer
/// endpoints make an `IntRange`, single-character ones an `ObjectRange`, and a
/// decimal one a `NumberRange`.
fn range_class(r: &RangeVal) -> &'static str {
    // A range that enumerates nothing — an exclusive one with equal endpoints —
    // is its own class in Groovy, whatever the endpoints' type.
    if range_is_empty(r) {
        return "groovy.lang.EmptyRange";
    }
    if range_char_ends(r).is_some() {
        return "groovy.lang.ObjectRange";
    }
    match (&r.from, &r.to) {
        (Value::Int(_), Value::Int(_)) => "groovy.lang.IntRange",
        _ => "groovy.lang.NumberRange",
    }
}

/// `GPRINTF` / `GSPRINTF`: Groovy's script-scope `printf`/`sprintf`. The
/// argument list is either spread (`printf("%d %s", 1, "a")`) or a single list
/// (`printf("%d %s", [1, "a"])`), both of which Groovy accepts.
fn b_printf(vm: &mut VM, argc: u8) -> Value {
    let text = formatted(vm, argc);
    print!("{text}");
    use std::io::Write;
    let _ = std::io::stdout().flush();
    Value::Undef
}

fn b_sprintf(vm: &mut VM, argc: u8) -> Value {
    let text = formatted(vm, argc);
    Value::str(text)
}

/// Pop a `printf`-style call's arguments and render them.
fn formatted(vm: &mut VM, argc: u8) -> String {
    let args = pop_args(vm, argc);
    let Some((spec, rest)) = args.split_first() else {
        return String::new();
    };
    // A lone list argument is the argument *vector*, not one `%s` operand.
    let rest: Vec<Value> = match rest {
        [Value::Array(a)] => a.clone(),
        other => other.to_vec(),
    };
    java_format(vm, &groovy_str(spec), &rest)
}

/// `pad` repeated and cut to exactly `n` characters (Groovy's `padLeft` and
/// friends cycle a multi-character pad rather than repeating it whole).
fn pad_text(pad: &str, n: usize) -> String {
    pad.chars().cycle().take(n).collect()
}

/// `Math.round`'s half-up rule: ties go toward positive infinity, so
/// `round(-1.5)` is `-1`, not `-2`.
fn java_round(f: f64) -> i64 {
    (f + 0.5).floor() as i64
}

/// Every nested list flattened away, depth-first — Groovy's `List.flatten()`.
fn flatten_values(items: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for v in items {
        // A nested list is a handle; deref so nesting flattens through both the
        // handle form and the transient array form.
        match deref_list(v) {
            Value::Array(inner) => out.extend(flatten_values(&inner)),
            other => out.push(other),
        }
    }
    out
}

/// Every permutation of `items`, in the order Groovy's `permutations()` yields
/// for a list (it answers a `Set`, whose iteration order is the insertion order
/// of this generation).
fn permutations_of(items: &[Value]) -> Vec<Vec<Value>> {
    if items.is_empty() {
        return vec![Vec::new()];
    }
    let mut out = Vec::new();
    for (i, head) in items.iter().enumerate() {
        let mut rest = items.to_vec();
        rest.remove(i);
        for mut tail in permutations_of(&rest) {
            tail.insert(0, head.clone());
            out.push(tail);
        }
    }
    out
}

/// `list.subsequences()` — every non-empty ordered subset, in the *insertion*
/// order `groovy.util.GroovyCollections.subsequences` adds them to its
/// `HashSet`. A faithful port of that method, which grows the answer one element
/// at a time:
///
/// ```text
/// Set<List<T>> ans = new HashSet<>();
/// for (T h : items) {
///     Set<List<T>> next = new HashSet<>();
///     for (List<T> it : ans) { List<T> t = new ArrayList<>(it); t.add(h); next.add(t); }
///     next.addAll(ans);
///     List<T> l = new ArrayList<>(); l.add(h); next.add(l);
///     ans = next;
/// }
/// ```
///
/// Both inner loops walk `ans` in *its* `HashSet` order, and that order decides
/// where each subsequence lands in `next` — so reproducing Groovy's printed
/// answer means reproducing the bucket walk at every intermediate step, not only
/// the last. [`set_elements`] is what supplies it.
fn subsequences_of(items: &[Value]) -> Vec<Value> {
    // `ans` is kept in insertion order; `set_elements` reads it back in the
    // order a `HashSet` would iterate it.
    let mut ans: Vec<Value> = Vec::new();
    for h in items {
        let seen = set_elements(
            &ans,
            SetKind::Hash {
                req: DEFAULT_HASH_REQ,
            },
        );
        let mut next: Vec<Value> = Vec::new();
        let add = |v: Value, next: &mut Vec<Value>| {
            if !next.iter().any(|k| values_equal(k, &v)) {
                next.push(v);
            }
        };
        for it in &seen {
            let mut extended = match it {
                Value::Array(a) => a.clone(),
                _ => continue,
            };
            extended.push(h.clone());
            add(Value::array(extended), &mut next);
        }
        for it in &seen {
            add(it.clone(), &mut next);
        }
        add(Value::array(vec![h.clone()]), &mut next);
        ans = next;
    }
    ans
}

/// Drop the entries of an ordered-map handle whose key `keep` rejects, mutating
/// through the handle (a map is shared, unlike a list).
fn omap_retain(v: &Value, keep: impl Fn(&str) -> bool) -> bool {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow_mut().get_mut(*id as usize) {
            Some(HeapObj::OrderedMap(m)) => {
                m.retain(|(k, _)| keep(k));
                true
            }
            _ => false,
        }),
        _ => false,
    }
}

/// Raise `java.lang.NumberFormatException` the way `Integer`/`Long`/`Double`
/// parsing does, naming the text that failed to parse.
fn raise_number_format(vm: &mut VM, text: &str) -> Value {
    raise(
        vm,
        "NumberFormatException",
        &format!("For input string: \"{text}\""),
    );
    Value::Undef
}

/// Parse `s` the way `Double.parseDouble` does: leading/trailing whitespace and
/// an optional `d`/`f` type suffix are allowed, and the spelled-out `Infinity` /
/// `NaN` forms — but not Rust's extra `inf`/`nan` spellings or a hex literal.
fn parse_java_double(s: &str) -> Option<f64> {
    let t = s.trim();
    let core = t.strip_suffix(['d', 'D', 'f', 'F']).unwrap_or(t);
    match core {
        "Infinity" | "+Infinity" => return Some(f64::INFINITY),
        "-Infinity" => return Some(f64::NEG_INFINITY),
        "NaN" => return Some(f64::NAN),
        _ => {}
    }
    // Any letter other than an exponent marker means a spelling Java rejects.
    if core
        .bytes()
        .any(|b| b.is_ascii_alphabetic() && b != b'e' && b != b'E')
    {
        return None;
    }
    core.parse().ok()
}

/// Dispatch a Groovy property read `recv.name`. Supports the `size`/`length`
/// count properties on `String`/list/map; a map's `k` also reads entry `k`. An
/// unmodeled property raises `groovy.lang.MissingPropertyException`.
fn dispatch_property(vm: &mut VM, recv: &Value, name: &str) -> Value {
    // A map's property access is *only* a key read (`m.k` == `m['k']`), and an
    // absent key is `null` — including the names that are properties on every
    // other value: `[a:1].size`, `[a:1].class`, and `[a:1].length` are all
    // `null` in Groovy, while `[size: 9].size` is `9`. Checked first for that
    // reason.
    if let Some(entries) = as_omap(recv) {
        return entries
            .iter()
            .find(|(ek, _)| ek == name)
            .map(|(_, v)| v.clone())
            .unwrap_or(Value::Undef);
    }
    if let Value::Hash(h) = recv {
        return h.get(name).cloned().unwrap_or(Value::Undef);
    }
    // Groovy's `.class` is the `getClass()` property, on every value but `null`
    // (where it answers `NullObject`'s class rather than raising).
    if name == "class" {
        return class_ref_of(recv);
    }
    // A `Matcher`'s properties are its getters, Groovy's property-for-getter
    // rule: `m.count` is `getCount()` and `m.pattern` is `pattern()`.
    if let Some(m) = as_matcher(recv) {
        if let Some(v) = dispatch_matcher_method(vm, recv, &m, name, &[]) {
            return v;
        }
    }
    // A `Range`'s own properties are its endpoints; anything else reads off the
    // list it enumerates, exactly as its methods do.
    if let Some(r) = as_range(recv) {
        return match name {
            "from" => range_lower(&r),
            "to" => range_upper(&r),
            _ => dispatch_property(vm, &Value::array(range_elements(&r)), name),
        };
    }
    // A `java.lang.Class` exposes its accessors as properties (`c.name`,
    // `c.simpleName`) — Groovy's getter-to-property rule.
    if let Some(qualified) = as_class_ref(recv) {
        return match name {
            "name" | "typeName" | "canonicalName" => Value::str(qualified),
            "simpleName" => Value::str(simple_name_of(&qualified)),
            // A statically named JDK class also carries its constants
            // (`Integer.MAX_VALUE`, `Math.PI`).
            _ => match static_field(&simple_name_of(&qualified), name) {
                Some(v) => v,
                None => raise_missing_property(vm, recv, name),
            },
        };
    }
    if let Some((k, v)) = as_entry(recv) {
        return match name {
            "key" => Value::str(k),
            "value" => v,
            _ => raise_missing_property(vm, recv, name),
        };
    }
    // A closure reports its declared arity (Groovy's `Closure` getters, which
    // the parameter-count-sensitive GDK methods read).
    if let Some(meta) = closure_meta(recv) {
        return match name {
            "maximumNumberOfParameters" => Value::int(meta.params as i64),
            _ => raise_missing_property(vm, recv, name),
        };
    }
    match (recv, name) {
        // Every property read on `null` raises, including `size`/`length`.
        (Value::Undef, _) => {
            raise(
                vm,
                "NullPointerException",
                &format!("Cannot get property '{name}' on null object"),
            );
            Value::Undef
        }
        // `size`/`length` are *methods* on a String and a list, not properties:
        // Groovy raises `MissingPropertyException` for `[1, 2].size` and
        // `"abc".length` alike (a map's `m.size` is the key read handled above).
        _ => raise_missing_property(vm, recv, name),
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
    // `/` on `null` dispatches `null.div(…)`, which is a NullPointerException.
    if matches!(a, Value::Undef) {
        raise(
            vm,
            "NullPointerException",
            "Cannot invoke method div() on null object",
        );
        return Value::Undef;
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
                // Groovy raises `java.lang.ArithmeticException`, which a script
                // can catch; unarmed this degrades to the hard fault it has
                // always been (see [`raise`]).
                raise(vm, "ArithmeticException", zero_divisor_message(&x));
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

/// The `java.lang.Class` handle for a value. `null` answers Groovy's
/// `NullObject` class rather than raising — `null.getClass()` and `null.class`
/// both do in Groovy, because a `null` receiver is routed to `NullObject`.
fn class_ref_of(v: &Value) -> Value {
    if matches!(v, Value::Undef) {
        return heap_push(HeapObj::ClassRef(
            "org.codehaus.groovy.runtime.NullObject".to_string(),
        ));
    }
    heap_push(HeapObj::ClassRef(java_class_name(v)))
}

/// `GITER`: materialise what `for (x in v)` iterates over. Pops the value and
/// pushes the element list, following Groovy's `DefaultTypeTransformation`:
/// a list yields its elements, a map its `Map.Entry`s, a `String` its
/// characters, `null` nothing at all, and any other value exactly itself (so
/// `for (x in 5)` runs once).
fn b_iter(vm: &mut VM, _argc: u8) -> Value {
    let v = vm.stack.pop().unwrap_or(Value::Undef);
    Value::array(iteration_elements(&v))
}

/// `GWRITEBACK`: pick the value a self-mutating GDK call stores back over its
/// variable receiver. Only a **list** receiver is mutated in place by Groovy —
/// `Map.sort()` returns a new map and leaves the receiver alone — so a
/// non-list receiver writes itself back unchanged.
fn b_writeback(vm: &mut VM, _argc: u8) -> Value {
    let receiver = vm.stack.pop().unwrap_or(Value::Undef);
    let result = vm.stack.pop().unwrap_or(Value::Undef);
    // A call whose *result* is not the new list (`add` answers `true`) parks the
    // new contents instead; prefer them when they are there.
    if let Some(new) = take_mutated() {
        return new;
    }
    if matches!(receiver, Value::Array(_)) {
        result
    } else {
        receiver
    }
}

/// The elements [`b_iter`] enumerates for a value.
fn iteration_elements(v: &Value) -> Vec<Value> {
    if let Some(items) = as_list(v) {
        return items;
    }
    if let Some(r) = as_range(v) {
        return range_elements(&r);
    }
    if let Some((items, kind)) = as_set(v) {
        return set_elements(&items, kind);
    }
    if let Some(entries) = as_omap(v) {
        return entries
            .into_iter()
            .map(|(k, val)| heap_push(HeapObj::Entry(k, val)))
            .collect();
    }
    match v {
        Value::Undef => Vec::new(),
        Value::Array(a) => a.clone(),
        Value::Str(s) => s.chars().map(|c| Value::str(c.to_string())).collect(),
        other => vec![other.clone()],
    }
}

/// `GMOD`: Groovy `%` for the zero-divisor case. The compiler emits this only
/// behind a native `divisor == 0` guard (see `compiler::Compiler::emit_mod`), so a
/// non-zero divisor never pays for it and `%` keeps `Op::Mod` and its JIT trace.
///
/// Groovy's `%` splits three ways on a zero divisor, and this reproduces all
/// three (verified against Apache Groovy 5.0.7):
///
/// | operands                | `7 % 0`                              |
/// |-------------------------|--------------------------------------|
/// | `Integer % Integer`     | `ArithmeticException: / by zero`     |
/// | either a `BigDecimal`   | `ArithmeticException: Division by zero` (`Division undefined` when the dividend is zero too) |
/// | a `Double`, no decimal  | `NaN` (IEEE, no exception)           |
fn b_mod(vm: &mut VM, _argc: u8) -> Value {
    let b = vm.stack.pop().unwrap_or(Value::Undef);
    let a = vm.stack.pop().unwrap_or(Value::Undef);
    // A user-class left operand dispatches Groovy's `remainder` overload, the
    // same method the numeric hook would reach for a non-zero divisor.
    if as_instance(&a).is_some() {
        if let Some(res) = instance_operator(NumOp::Mod, &a, &b) {
            return match res {
                Ok(v) => v,
                Err(e) => {
                    fault(vm, e);
                    Value::Undef
                }
            };
        }
    }
    // Two `Integer`s: `Integer.remainder`, whose zero-divisor message is the
    // JDK's `/ by zero` (not `BigDecimal`'s wording).
    if let (Some(x), Some(y)) = (as_i64(&a), as_i64(&b)) {
        if y == 0 {
            raise(vm, "ArithmeticException", "/ by zero");
            return Value::Undef;
        }
        return Value::int(x.wrapping_rem(y));
    }
    // A `BigDecimal` operand: `BigDecimal.remainder`, whose zero divisor carries
    // `Division by zero` / `Division undefined`.
    match decimal_operator(NumOp::Mod, &a, &b) {
        Some(Ok(v)) => return v,
        Some(Err(msg)) => {
            raise(vm, "ArithmeticException", &msg);
            return Value::Undef;
        }
        None => {}
    }
    // A `double` with no decimal operand: IEEE `%`, where `x % 0.0` is NaN.
    if matches!(a, Value::Float(_)) || matches!(b, Value::Float(_)) {
        return Value::float(as_f64(&a) % as_f64(&b));
    }
    match numeric_hook(NumOp::Mod, &a, &b) {
        Ok(v) => v,
        Err(e) => {
            fault(vm, e);
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
    // `<=>` is `DefaultTypeTransformation.compareTo`, which needs a `Comparable`
    // left operand once `null` is out of the way. A list, a map, a set, a range
    // and a user class without `compareTo` are not `Comparable`, so Groovy
    // raises rather than inventing an order — even for two equal lists.
    // `null` orders before everything, whichever side it is on.
    match (matches!(a, Value::Undef), matches!(b, Value::Undef)) {
        (true, true) => return Value::int(0),
        (true, false) => return Value::int(-1),
        (false, true) => return Value::int(1),
        _ => {}
    }
    if is_incomparable(&a) || is_incomparable(&b) {
        let (sa, sb) = (java_to_string(vm, &a), java_to_string(vm, &b));
        let message = format!(
            "Cannot compare {} with value '{sa}' and {} with value '{sb}'",
            java_class_name(&a),
            java_class_name(&b),
        );
        raise(vm, "IllegalArgumentException", &message);
        return Value::Undef;
    }
    match natural_order(&a, &b) {
        std::cmp::Ordering::Less => Value::int(-1),
        std::cmp::Ordering::Greater => Value::int(1),
        std::cmp::Ordering::Equal => Value::int(0),
    }
}

/// True for a value Java would not accept as `Comparable`: a collection, a map,
/// a range, or a class instance with no `compareTo`.
fn is_incomparable(v: &Value) -> bool {
    if let Some(inst) = as_instance(v) {
        return lookup_method(inst.class, "compareTo").is_none();
    }
    matches!(v, Value::Array(_) | Value::Hash(_))
        || as_list(v).is_some()
        || as_omap(v).is_some()
        || as_range(v).is_some()
}

/// A value rendered the way *Java's* `toString` renders it, which is what the
/// `Cannot compare …` diagnostic quotes: a map prints `{k=v, …}` where Groovy's
/// own rendering prints `[k:v, …]`.
fn java_to_string(vm: &mut VM, v: &Value) -> String {
    match as_omap(v) {
        Some(entries) => {
            let items: Vec<String> = entries
                .iter()
                .map(|(k, val)| format!("{k}={}", groovy_str(val)))
                .collect();
            format!("{{{}}}", items.join(", "))
        }
        None => render_value(vm, v),
    }
}

/// Groovy's natural ordering for two values with no user `compareTo`: a decimal
/// operand compares exactly (scale-insensitively), other numbers compare as
/// doubles, and anything else compares by its rendered form (Groovy's `String`
/// ordering). An incomparable pair (a NaN) reports `Equal`, which keeps a sort
/// stable rather than panicking.
fn natural_order(a: &Value, b: &Value) -> std::cmp::Ordering {
    let ord = match (as_dec(a).is_some() || as_dec(b).is_some())
        .then(|| (as_exact_dec(a), as_exact_dec(b)))
    {
        Some((Some(x), Some(y))) => Some(decimal::cmp(&x, &y)),
        _ => match (as_num(a), as_num(b)) {
            (Some(x), Some(y)) => x.partial_cmp(&y),
            _ => Some(groovy_str(a).cmp(&groovy_str(b))),
        },
    };
    ord.unwrap_or(std::cmp::Ordering::Equal)
}

/// Compare two values the way a GDK `sort`/`max`/`min` with no closure does: a
/// user-class operand dispatches its `compareTo`, everything else falls to
/// [`natural_order`].
fn compare_values(vm: &mut VM, a: &Value, b: &Value) -> Result<std::cmp::Ordering, String> {
    if let Some(res) = call_user_method(vm, a, "compareTo", std::slice::from_ref(b)) {
        return res.map(|v| v.to_int().cmp(&0));
    }
    Ok(natural_order(a, b))
}

/// How a GDK closure argument orders two elements.
enum OrderBy<'a> {
    /// No closure: natural ordering (a user `compareTo` when the element is an
    /// instance).
    Natural,
    /// A one-parameter closure: a *key extractor*; elements order by the values
    /// it returns (`list.sort { it.length() }`).
    Key(&'a Value),
    /// A two-parameter closure: a *comparator* returning a negative / zero /
    /// positive `int` (`list.sort { a, b -> b <=> a }`).
    Comparator(&'a Value),
}

impl<'a> OrderBy<'a> {
    /// Read the trailing closure argument (if any) as an ordering rule. Groovy
    /// picks key-extractor vs comparator from the closure's parameter count.
    fn of(args: &'a [Value]) -> Self {
        match args.last().filter(|a| closure_meta(a).is_some()) {
            Some(clo) if closure_meta(clo).map(|m| m.params).unwrap_or(1) >= 2 => {
                OrderBy::Comparator(clo)
            }
            Some(clo) => OrderBy::Key(clo),
            None => OrderBy::Natural,
        }
    }

    fn apply(&self, vm: &mut VM, a: &Value, b: &Value) -> Result<std::cmp::Ordering, String> {
        match self {
            OrderBy::Natural => compare_values(vm, a, b),
            OrderBy::Key(clo) => {
                let ka = invoke_closure(vm, clo, std::slice::from_ref(a))?;
                let kb = invoke_closure(vm, clo, std::slice::from_ref(b))?;
                compare_values(vm, &ka, &kb)
            }
            OrderBy::Comparator(clo) => {
                let r = invoke_closure(vm, clo, &[a.clone(), b.clone()])?;
                Ok(r.to_int().cmp(&0))
            }
        }
    }
}

/// Stable merge sort over `items` under `order`. Groovy's `List.sort` is
/// `Collections.sort`, which is stable — a hand-rolled merge keeps that while
/// letting a comparator closure that faults abort the sort (Rust's `sort_by`
/// cannot return an error from its comparator).
fn sort_values(vm: &mut VM, items: &[Value], order: &OrderBy) -> Result<Vec<Value>, String> {
    if items.len() < 2 {
        return Ok(items.to_vec());
    }
    let mid = items.len() / 2;
    let left = sort_values(vm, &items[..mid], order)?;
    let right = sort_values(vm, &items[mid..], order)?;
    let mut out = Vec::with_capacity(items.len());
    let (mut i, mut j) = (0, 0);
    while i < left.len() && j < right.len() {
        // `<=` keeps equal elements in their original order (stability).
        if order.apply(vm, &right[j], &left[i])?.is_lt() {
            out.push(right[j].clone());
            j += 1;
        } else {
            out.push(left[i].clone());
            i += 1;
        }
    }
    out.extend_from_slice(&left[i..]);
    out.extend_from_slice(&right[j..]);
    Ok(out)
}

/// The extreme element under `order` — `max` when `want` is `Greater`, `min`
/// when it is `Less`. Groovy keeps the *first* element on a tie, and answers
/// `null` for an empty collection.
fn extreme_value(
    vm: &mut VM,
    items: &[Value],
    order: &OrderBy,
    want: std::cmp::Ordering,
) -> Result<Value, String> {
    let mut best: Option<Value> = None;
    for it in items {
        best = Some(match best {
            None => it.clone(),
            Some(b) => {
                if order.apply(vm, it, &b)? == want {
                    it.clone()
                } else {
                    b
                }
            }
        });
    }
    Ok(best.unwrap_or(Value::Undef))
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
    // A `Pattern` renders as its source text, the way `Pattern.toString` does.
    if let Some(src) = regex_source(v) {
        return src;
    }
    // `Range.toString` is the source form (`1..5`), *not* the list it
    // enumerates — `println(1..5)` prints `1..5` and `"x" + (1..5)` is `x1..5`.
    if let Some(r) = as_range(v) {
        return range_str(&r);
    }
    // A character buffer renders as its contents, the way `StringBuilder`'s own
    // `toString` does.
    if let Some((_, text)) = as_buffer(v) {
        return text;
    }
    if let Some(m) = as_matcher(v) {
        return matcher_str(&m);
    }
    // `java.lang.Class.toString` prefixes the qualified name with `class `.
    if let Some(name) = as_class_ref(v) {
        return format!("class {name}");
    }
    // `Map.Entry.toString` is `key=value`.
    if let Some((k, val)) = as_entry(v) {
        return format!("{k}={}", groovy_str(&val));
    }
    // A list handle renders `[a, b, c]` (`[]` when empty), exactly as the
    // transient `Value::Array` form below does.
    if let Some(items) = as_list(v) {
        let shown: Vec<String> = items.iter().map(groovy_str).collect();
        return format!("[{}]", shown.join(", "));
    }
    // A set renders like a list — `[a, b]`, `[]` when empty — but in the order
    // its implementation presents, which for a `HashSet`/`TreeSet` is not the
    // order elements went in.
    if let Some((items, kind)) = as_set(v) {
        let shown: Vec<String> = set_elements(&items, kind).iter().map(groovy_str).collect();
        return format!("[{}]", shown.join(", "));
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

/// The `java.lang.ArithmeticException` message Java's `BigDecimal.divide` uses
/// for a zero divisor: `0 / 0` is *undefined* (no quotient is more right than
/// any other), while `n / 0` for a non-zero `n` is division by zero. Groovy
/// promotes integer division to `BigDecimal`, so `0 / 0` reports the same way.
fn zero_divisor_message(dividend: &BigDecimal) -> &'static str {
    if dividend.is_zero() {
        "Division undefined"
    } else {
        "Division by zero"
    }
}

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
        let negated = decimal::neg(&as_dec(a)?);
        // A `BigInteger` stays one through unary `-`, exactly as it does through
        // `+`/`-`/`*` and `negate()` — `(-255G).getClass()` is
        // `java.math.BigInteger`. Answering a `BigDecimal` here made `-255G`
        // quietly change type, and with it lose `toString(radix)`.
        return Some(Ok(if as_bigint(a).is_some() {
            bigint_value(negated)
        } else {
            dec_value(negated)
        }));
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
                    None => Err(zero_divisor_message(&x).to_string()),
                });
            }
        }
    }
    if matches!(a, Value::Float(_)) || matches!(b, Value::Float(_)) {
        return Some(Ok(double_operator(op, as_f64(a), as_f64(b))));
    }
    let (x, y) = (as_exact_dec(a)?, as_exact_dec(b)?);
    let ordering = || decimal::cmp(&x, &y);
    // `BigInteger op Integer/Long/BigInteger` stays a `BigInteger`; a real
    // decimal operand widens the result to `BigDecimal`, and `/` always does
    // (`100G / 3` is `33.3333333333`). So the exact result is built once and
    // tagged with whichever of the two types it belongs to.
    let integral =
        (as_bigint(a).is_some() || as_bigint(b).is_some()) && is_integral(a) && is_integral(b);
    let exact = |d: BigDecimal| {
        if integral {
            bigint_value(d)
        } else {
            dec_value(d)
        }
    };
    let result = match op {
        NumOp::Add => exact(decimal::add(&x, &y)),
        NumOp::Sub => exact(decimal::sub(&x, &y)),
        NumOp::Mul => exact(decimal::mul(&x, &y)),
        // `/` lowers to the `GDIV` builtin, but the hook still handles it for
        // completeness (an operand pair fusevm delegates directly).
        NumOp::Div => match decimal::divide(&x, &y) {
            Some(q) => dec_value(q),
            None => return Some(Err(zero_divisor_message(&x).to_string())),
        },
        NumOp::Mod => match decimal::remainder(&x, &y) {
            Some(r) => exact(r),
            None => return Some(Err(zero_divisor_message(&x).to_string())),
        },
        // Groovy raises a decimal to an integer power exactly; a negative or
        // fractional exponent falls back to `double`.
        NumOp::Pow => match decimal::to_i64(&y).and_then(|e| decimal::pow(&x, e)) {
            Some(p) => exact(p),
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

/// A string's lines with the terminators removed — Groovy's `readLines`, which
/// (like Java's `String.lines`) yields no trailing empty line for a string that
/// ends in a terminator.
fn read_lines(s: &str) -> Vec<String> {
    let body = s.strip_suffix('\n').unwrap_or(s);
    let body = body.strip_suffix('\r').unwrap_or(body);
    if body.is_empty() && s.is_empty() {
        return Vec::new();
    }
    body.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect()
}

/// Java's `String.stripIndent`: remove the common indent and each line's
/// trailing whitespace. A string ending in a line terminator opts *out* of the
/// outdent (the indent stays) but still loses its trailing whitespace, and the
/// terminator is put back.
fn strip_indent(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let ends_with_terminator = s.ends_with('\n') || s.ends_with('\r');
    let lines = read_lines(s);
    let indent_of = |l: &String| l.chars().take_while(|c| c.is_whitespace()).count();
    // Blank lines do not constrain the indent — except the last one, which does.
    let outdent = if ends_with_terminator {
        0
    } else {
        lines
            .iter()
            .enumerate()
            .filter(|(i, l)| *i + 1 == lines.len() || !l.trim().is_empty())
            .map(|(_, l)| indent_of(l))
            .min()
            .unwrap_or(0)
    };
    let mut out: String = lines
        .iter()
        .map(|l| {
            let body: String = l.chars().skip(outdent).collect();
            body.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    if ends_with_terminator {
        out.push('\n');
    }
    out
}

/// Replace each tab in one line with spaces up to the next multiple of `stop`,
/// counting columns from the start of the line — Groovy's `expandLine`.
fn expand_tabs(line: &str, stop: usize) -> String {
    let mut out = String::new();
    let mut col = 0usize;
    for c in line.chars() {
        if c == '\t' {
            let pad = stop - col % stop;
            out.extend(std::iter::repeat(' ').take(pad));
            col += pad;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

/// Expand a `tr` character set: `a-c` becomes `abc` and a reversed `c-a` becomes
/// `cba`; a hyphen with no character on one side of it stays literal.
fn expand_hyphen(spec: &str) -> Vec<char> {
    let cs: Vec<char> = spec.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if i + 2 < cs.len() && cs[i + 1] == '-' {
            let (a, b) = (cs[i] as u32, cs[i + 2] as u32);
            if a <= b {
                out.extend((a..=b).filter_map(char::from_u32));
            } else {
                out.extend((b..=a).rev().filter_map(char::from_u32));
            }
            i += 3;
        } else {
            out.push(cs[i]);
            i += 1;
        }
    }
    out
}

/// Groovy `+` on a non-numeric left operand, dispatched on the left value
/// (Groovy dispatches `+` as `left.plus(right)`): a list concatenates another
/// list or appends a scalar; an ordered map merges another map (right wins on a
/// duplicate key, insertion order preserved); anything else concatenates as a
/// string.
fn groovy_add(a: &Value, b: &Value) -> Value {
    // A set unions rather than concatenating, and the result is another set of
    // the same kind — so `([1, 2] as Set) + ([2, 3] as Set)` is `[1, 2, 3]`.
    // Only the *left* operand decides, exactly as `left.plus(right)` implies:
    // `[1, 2] + ([2, 3] as Set)` is the four-element list `[1, 2, 2, 3]`.
    if let Some((items, kind)) = as_set(a) {
        return make_set(
            set_elements(&items, kind)
                .into_iter()
                .chain(iteration_elements(b))
                .collect(),
            kind,
        );
    }
    if let Value::Array(xs) = a {
        let mut out = xs.clone();
        match b {
            Value::Array(ys) => out.extend(ys.iter().cloned()),
            // `List.plus` has a `Collection` overload and an `Object` one, and
            // Java picks the `Collection` one for a set or a range: `[1, 2] +
            // ([2, 3] as Set)` is the four-element `[1, 2, 2, 3]` — the *list*
            // does not de-duplicate — rather than a list with a set inside it.
            _ if as_set(b).is_some() || as_range(b).is_some() => out.extend(iteration_elements(b)),
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

/// Groovy's `-` on a `String` or a list: a string drops the **first** occurrence
/// of the right operand's text, a list drops **every** element the right operand
/// contains (or the right operand itself when it is not a collection).
fn groovy_sub(a: &Value, b: &Value) -> Value {
    if let Value::Array(xs) = a {
        let drop: Vec<Value> = match b {
            Value::Array(_) => iteration_elements(b),
            other => vec![other.clone()],
        };
        return Value::array(
            xs.iter()
                .filter(|v| !drop.iter().any(|w| values_equal(v, w)))
                .cloned()
                .collect(),
        );
    }
    let (s, needle) = (groovy_str(a), groovy_str(b));
    match s.find(&needle) {
        Some(at) => Value::str(format!("{}{}", &s[..at], &s[at + needle.len()..])),
        None => Value::str(s),
    }
}

/// Groovy's `*` on a `String` or a list: the receiver repeated `n` times.
fn groovy_mul(a: &Value, b: &Value) -> Value {
    let n = as_i64(b).unwrap_or(0).max(0) as usize;
    match a {
        Value::Array(xs) => Value::array(std::iter::repeat(xs.clone()).take(n).flatten().collect()),
        other => Value::str(groovy_str(other).repeat(n)),
    }
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

/// Strict numeric hook: fusevm calls this for an operation it will not answer
/// itself. Three things route here, and only the first is non-numeric:
///
///  1. A non-numeric operand — Groovy's `+` overload (list concat / map merge /
///     string concatenation) and value comparisons against strings.
///  2. An integer result outside the fixnum range set by
///     [`VM::set_fixnum_range`], i.e. Groovy's `Integer` overflow. That one is
///     answered in `int_arith` before it gets this far, so it never reaches
///     the operator match below — but it does *not* "stay on the native fast
///     path": it is delegated like any other.
///  3. A mixed integral/`double` pair whose integer is past 2^53, where reading
///     the integer as an `f64` would land on a neighbouring value and only the
///     host knows whether that matters. It does not: Groovy promotes to
///     `double` first, so the rounded answer is the right one — see the
///     promotion gate below.
///
/// `/` never reaches here — it lowers to the [`GDIV`] builtin instead.
pub fn numeric_hook(op: NumOp, a: &Value, b: &Value) -> Result<Value, String> {
    // A range takes part in an operator as the list it enumerates: `(1..3) + [9]`
    // concatenates and `(1..3) == [1, 2, 3]` is true, because Groovy's `Range`
    // is a `java.util.List`. Rewriting the operands here is what gives a range
    // every list operator at once.
    //
    // The one exception is `String + Range`, which dispatches on its *left*
    // operand: `String.plus` appends the range's `toString`, so `"x" + (1..3)`
    // is `x1..3` and not `x[1, 2, 3]`. (`(1..3) + "x"` dispatches the other way
    // and does append the element, giving `[1, 2, 3, x]`.)
    // A list rides a handle, and every operator below is written against the
    // transient array form. Rewriting the operands once here is the same trick
    // the range branch uses, and keeps `groovy_add`/`groovy_sub`/`groovy_mul`
    // and the comparisons unaware that lists moved to the heap.
    if as_list(a).is_some() || as_list(b).is_some() {
        return numeric_hook(op, &deref_list(a), &deref_list(b));
    }
    let string_plus = matches!(op, NumOp::Add) && matches!(a, Value::Str(_));
    if !string_plus && (as_range(a).is_some() || as_range(b).is_some()) {
        return numeric_hook(op, &range_as_list(a), &range_as_list(b));
    }
    // User-class operator overloading. Groovy dispatches an operator on its LEFT
    // operand as a method call (`a + b` == `a.plus(b)`, `a > b` == `a.compareTo(b)
    // > 0`, `a == b` via `equals`/`compareTo`). Only a class-instance left operand
    // routes here; primitive `Int`/`Float`/`String` arithmetic stays on the native
    // and JIT fast paths for every pair fusevm can answer itself. `/` is absent —
    // it lowers to the [`GDIV`] builtin, where the `div` overload is dispatched
    // instead.
    if as_instance(a).is_some() {
        if let Some(res) = instance_operator(op, a, b) {
            return res;
        }
    }
    // Arithmetic whose LEFT operand is `null` is a `NullPointerException` in
    // Groovy, not a hard fault: the operator dispatches as a method call on
    // `null`. `+` gets its own wording (verified against Apache Groovy 5.0.8),
    // and the comparisons stay total (`null > 1` is false, not a throw).
    if matches!(a, Value::Undef) {
        let message = match op {
            NumOp::Add => Some(format!("Cannot execute null+{}", groovy_str(b))),
            NumOp::Sub => Some("Cannot invoke method minus() on null object".to_string()),
            NumOp::Mul => Some("Cannot invoke method multiply() on null object".to_string()),
            NumOp::Div => Some("Cannot invoke method div() on null object".to_string()),
            NumOp::Mod => Some("Cannot invoke method remainder() on null object".to_string()),
            NumOp::Pow => Some("Cannot invoke method power() on null object".to_string()),
            NumOp::Neg => Some("Cannot invoke method negative() on null object".to_string()),
            _ => None,
        };
        if let Some(message) = message {
            let raised = with_vm(|vm| {
                raise(vm, "NullPointerException", &message);
            });
            if raised.is_some() {
                return Ok(Value::Undef);
            }
        }
    }
    // Groovy's `==` between a `String` and a number is **false** — it never
    // coerces (`"1" == 1` is false, unlike `1 == 1.0`). Checked before the
    // decimal path so `"1" == 1.0` answers the same way.
    if matches!(op, NumOp::Eq | NumOp::Ne) {
        let numeric = |v: &Value| {
            matches!(v, Value::Int(_) | Value::Float(_) | Value::Bool(_)) || as_dec(v).is_some()
        };
        let mismatched = (matches!(a, Value::Str(_)) && numeric(b))
            || (matches!(b, Value::Str(_)) && numeric(a));
        if mismatched {
            return Ok(Value::bool(matches!(op, NumOp::Ne)));
        }
    }
    // Decimal arithmetic. A `BigDecimal` is a host-heap handle, so fusevm sees a
    // non-numeric operand and delegates every `+`/`-`/`*`/`%`/`**`/comparison on
    // one to this hook — which is exactly where Groovy's scale rules belong.
    if let Some(res) = decimal_operator(op, a, b) {
        return res;
    }
    // Binary numeric promotion. A primitive pair with a `double` in it is
    // answered on IEEE doubles, exactly as fusevm's native path answers it —
    // measured against Apache Groovy 5.0.8: `16677181699666569L + 2.0d` is
    // `1.667718169966657E16`, and `16677181699666569L == 1.6677181699666568E16d`
    // is *true*, because the `long` is widened to `double` (landing on a
    // neighbouring value) before the operator runs. So the rounding is the
    // answer, not a loss to route around.
    //
    // fusevm delegates such a pair once the integer is past 2^53, where reading
    // it as an `f64` is inexact and only the host can say whether that matters.
    // The gate is on operand *shape* and sits ahead of the operator match below,
    // because that match ends in a catch-all concatenating `+` and "not defined"
    // arms that would silently absorb any numeric case it did not cover — which
    // is what used to happen: `16677181699666569L + 2.0d` answered the *string*
    // `"166771816996665692.0"`, and `-`/`*`/`%`/`**` answered an error.
    //
    // `Bool` is deliberately not a number here (Groovy has no `Boolean.plus`),
    // and a two-integer pair is not promoted either: `Integer` overflow belongs
    // to `int_arith`, `/` divides as a `BigDecimal` in [`GDIV`], and `%` by zero
    // is an `ArithmeticException` — none of which is double arithmetic.
    let is_groovy_number = |v: &Value| matches!(v, Value::Int(_) | Value::Float(_));
    let has_double = matches!(a, Value::Float(_)) || matches!(b, Value::Float(_));
    if has_double && is_groovy_number(a) && is_groovy_number(b) {
        return Ok(double_operator(op, as_f64(a), as_f64(b)));
    }
    match op {
        // Groovy `+` dispatches on the left operand: list concatenation/append,
        // map merge, else string concatenation.
        NumOp::Add => Ok(groovy_add(a, b)),
        // A set on either side compares as a set: order-insensitive, and never
        // equal to a list. The rendered-form comparison below would get both
        // halves backwards, so it has to be decided before it.
        NumOp::Eq | NumOp::Ne if as_set(a).is_some() || as_set(b).is_some() => {
            let eq = values_equal(a, b);
            Ok(Value::bool(if matches!(op, NumOp::Eq) { eq } else { !eq }))
        }
        // Groovy `==`/`!=` are value equality (`.equals`), not reference
        // identity — comparing string/boolean operands by value is faithful.
        NumOp::Eq => Ok(Value::bool(groovy_str(a) == groovy_str(b))),
        NumOp::Ne => Ok(Value::bool(groovy_str(a) != groovy_str(b))),
        NumOp::Lt => Ok(Value::bool(groovy_str(a) < groovy_str(b))),
        NumOp::Gt => Ok(Value::bool(groovy_str(a) > groovy_str(b))),
        NumOp::Le => Ok(Value::bool(groovy_str(a) <= groovy_str(b))),
        NumOp::Ge => Ok(Value::bool(groovy_str(a) >= groovy_str(b))),
        // Groovy defines `-` and `*` on strings and lists too: `"abc" - "b"`
        // removes the first occurrence, `"abc" * 3` repeats, `list - other`
        // subtracts every match, `list * n` repeats the list.
        NumOp::Sub if matches!(a, Value::Str(_) | Value::Array(_)) => Ok(groovy_sub(a, b)),
        // `set - other` answers another set of the same kind with every element
        // the right side holds removed — the `Set.minus` overload, dispatched on
        // the left operand like every other Groovy operator.
        NumOp::Sub if as_set(a).is_some() => {
            let (x, y) = (a.clone(), b.clone());
            with_vm(|vm| dispatch_method(vm, &x, "minus", std::slice::from_ref(&y)))
                .ok_or_else(|| "groovyrs: set `-` dispatched with no active VM".to_string())
        }
        // `map - other` drops the entries the other map holds identically —
        // the `Map.minus` GDK overload, dispatched on the left operand.
        NumOp::Sub if as_omap(a).is_some() => {
            let (x, y) = (a.clone(), b.clone());
            with_vm(|vm| dispatch_method(vm, &x, "minus", std::slice::from_ref(&y)))
                .ok_or_else(|| "groovyrs: map `-` dispatched with no active VM".to_string())
        }
        NumOp::Mul if matches!(a, Value::Str(_) | Value::Array(_)) => Ok(groovy_mul(a, b)),
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

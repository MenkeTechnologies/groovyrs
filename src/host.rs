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
use bigdecimal::{BigDecimal, Signed, Zero};
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
/// the *owner* could not resolve — the name form of what `b_closure_call`
/// already does for a bare call. Emitted only for a name the compiler could not
/// tie to a slot, a field, a class, a function, or a script-level declaration,
/// so an ordinary variable keeps its native `GetVar`/`SetVar` (and with it its
/// JIT trace eligibility). Stack for the read: the global's name-pool index,
/// then the name. For the write: the value, the index, then the name.
/// See `b_name_get` / `b_name_set`.
pub const GNAME_GET: u16 = 760;
pub const GNAME_SET: u16 = 761;

/// Builtin id for a list literal: takes the `Value::Array` that `Op::MakeArray`
/// just built and registers it as a `java.util.ArrayList` handle.
///
/// A Groovy list is a *reference* — `def b = a` names one `ArrayList` twice — so
/// a literal has to allocate a handle rather than leave a fusevm array on the
/// stack. `Op::MakeArray` still does the element gathering; this only wraps it.
/// See `b_make_list`.
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
/// instance, so an ordinary shift is untouched. See `b_shr`.
pub const GSHR: u16 = 763;

/// Builtin id for `recv.method(args)` at a call site where the compiler saw a
/// statically-`Long` receiver or argument — [`GMETHOD`] with the widths attached.
///
/// Java resolves an overload on the *declared* parameter width, and `16` and
/// `16L` reach the host as the one `Value::Int`: `255.toString(16)` renders `16`
/// through the static `Integer.toString(int)`, while `255.toString(16L)` matches
/// no overload at all and is a `MissingMethodException`. The magnitude rule that
/// serves `java_class_name` cannot separate them, so the widths travel with the
/// call, exactly as [`GCLASS_LONG`] carries the width of a `getClass()` receiver.
///
/// Stack: the width mask (bit 0 the receiver, bit `k+1` argument `k`), then the
/// receiver, the arguments, and the method name — so the handler pops the plain
/// [`GMETHOD`] shape and finds the mask beneath it. The compiler emits this only
/// where the mask is non-zero, which leaves every ordinary call on [`GMETHOD`].
pub const GMETHOD_WIDE: u16 = 764;

/// Builtin id for the call-depth check a statically recursive function carries
/// in its prologue. Pushes nothing and pops nothing; it reads `vm.frames.len()`
/// and raises `java.lang.StackOverflowError` past [`MAX_CALL_DEPTH`]. See
/// `b_depth`, and `Compiler::recursive_fns` for which functions get one.
pub const GDEPTH: u16 = 765;

/// Builtin id for `&`/`|`/`^` where an operand is not a plain integer. Stack:
/// left, right, and the operator's Groovy method name (`and`/`or`/`xor`).
///
/// fusevm's `Op::BitAnd` and friends read their operands with `Value::to_int`,
/// which answers `0` for a `Value::Obj` — so `1G & 3G` (two `BigInteger`
/// handles) evaluated to `0` on the native lowering, silently and at every
/// width. `NumOp` has no bitwise member for the strict numeric hook to carry,
/// so the same escape the shifts use applies: `Compiler::bit_operand_is_object`
/// spots the operands that need arbitrary precision and routes only those here.
pub const GBITOP: u16 = 766;

/// Builtin id for `~` where the operand is not a plain integer — the unary
/// sibling of [`GBITOP`], and `0` on the native lowering for the same reason.
/// Stack: the operand.
pub const GBITNOT: u16 = 767;

/// The call depth at which groovyrs raises `java.lang.StackOverflowError`.
///
/// Groovy's depth is the JVM's: whatever fits in the thread's `-Xss`. Measured
/// on Apache Groovy 5.0.8 / JVM 21.0.12 with `def r; r = { -> d++; r() }`, a
/// self-recursive closure reached **1650** frames before `StackOverflowError`.
/// This sits above that, so every recursion the reference can complete completes
/// here too, and a runaway one raises the same catchable throwable rather than
/// growing `vm.frames` until the process is killed.
///
/// The measure is `vm.frames.len()`, the one depth both recursion paths share:
/// fusevm's `Op::Call` pushes a frame, and so does `run_sub`, the host's
/// nested-`VM::run` entry for closures, methods, constructors and field
/// initializers. `crate::INTERPRETER_STACK_BYTES` is sized to hold this many
/// levels of the second kind, which are the ones that cost Rust stack.
pub const MAX_CALL_DEPTH: usize = 2000;

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
    vm.register_builtin(GMETHOD_WIDE, b_method_wide);
    vm.register_builtin(GDEPTH, b_depth);
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
    vm.register_builtin(GBITOP, b_bitop);
    vm.register_builtin(GBITNOT, b_bitnot);
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
    raise_with(vm, class, message, Vec::new);
}

/// [`raise_opt`], carrying the diagnostic payload the throwable's class exposes:
/// `MissingMethodException.getMethod()`/`getType()`/`getArguments()` and
/// `MissingPropertyException.getProperty()`/`getType()`. Groovy's dynamic
/// dispatch makes those two throwables ordinary control flow, and a handler that
/// reads the payload is the reason they carry one.
///
/// The payload is built only when exception handling is armed — hence the
/// thunk. An exception-free program still degrades to the hard `groovyrs:`
/// fault, so it allocates nothing, which matters for `type`: a `Class` is a heap
/// object and the heap has no collector.
fn raise_with(
    vm: &mut VM,
    class: &str,
    message: Option<&str>,
    payload: impl FnOnce() -> Vec<(&'static str, Value)>,
) {
    if EXC_ARMED.with(|a| a.get()) {
        set_pending(new_throwable_with(class, message, payload()));
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

/// Raise the message-less `java.lang.StackOverflowError` runaway recursion
/// raises. The JVM's carries no message (`new StackOverflowError().getMessage()`
/// and the one a real overflow throws are both `null`, verified on Apache Groovy
/// 5.0.8 / JVM 21.0.12), so this goes through [`raise_opt`] rather than
/// [`raise`]. Called from [`run_sub`] and [`b_depth`], the two places
/// [`MAX_CALL_DEPTH`] is enforced.
fn raise_stack_overflow(vm: &mut VM) {
    raise_opt(vm, "StackOverflowError", None);
}

/// [`GDEPTH`]: the call-depth check a statically recursive function runs in its
/// prologue. fusevm's `Op::Call` costs no Rust stack — it pushes onto the
/// heap-allocated `vm.frames` — so unbounded native recursion does not overflow
/// the process stack; it grows memory until the process is killed (measured
/// ~250 MB per second on `def r(n) { return r(n + 1) }`). Neither outcome is
/// catchable, and Groovy's is.
fn b_depth(vm: &mut VM, _argc: u8) -> Value {
    if vm.frames.len() >= MAX_CALL_DEPTH {
        raise_stack_overflow(vm);
    }
    Value::Undef
}

/// Raise `groovy.lang.MissingMethodException` for an unresolved `recv.method(…)`,
/// with the message Groovy builds: the method name, the receiver's Java class,
/// and the argument types and values. Returns the placeholder the faulting
/// builtin hands back — the compiler's post-call check unwinds before it is read.
fn raise_missing_method(vm: &mut VM, recv: &Value, method: &str, args: &[Value]) -> Value {
    raise_missing_method_wide(vm, recv, method, args, 0)
}

/// [`raise_missing_method`], told which of the receiver and arguments the
/// compiler saw as a `Long` (the [`GMETHOD_WIDE`] mask).
///
/// The message names each participant's class, and the magnitude rule cannot
/// name a small `Long`: `255.toString(16L)` must report `class: java.lang.Integer`
/// and `argument types: (Long)`, which are two different readings of two
/// `Value::Int`s that are numerically 255 and 16. Only the call site knows.
fn raise_missing_method_wide(
    vm: &mut VM,
    recv: &Value,
    method: &str,
    args: &[Value],
    widths: u8,
) -> Value {
    let wide_at = |bit: u32| widths & (1 << bit) != 0;
    let types = args
        .iter()
        .enumerate()
        .map(|(i, a)| match a {
            Value::Int(_) if wide_at(i as u32 + 1) => "Long".to_string(),
            _ => simple_class_name(a),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let values = args.iter().map(groovy_str).collect::<Vec<_>>().join(", ");
    let class = match recv {
        Value::Int(_) if wide_at(0) => "java.lang.Long".to_string(),
        _ => java_class_name(recv),
    };
    let payload_class = class.clone();
    let payload_args = args.to_vec();
    raise_with(
        vm,
        "MissingMethodException",
        Some(&format!(
            "No signature of method: {method} for class: {class} \
             is applicable for argument types: ({types}) values: [{values}]"
        )),
        // Groovy's `MissingMethodException` carries the three things a handler
        // needs to recover: the name that missed, the receiver's class, and the
        // arguments it was called with. `getArguments()` is an `Object[]`, which
        // groovyrs models as the transient list form.
        || {
            vec![
                ("method", Value::str(method.to_string())),
                ("type", heap_push(HeapObj::ClassRef(payload_class))),
                ("arguments", Value::array(payload_args)),
            ]
        },
    );
    Value::Undef
}

/// Is `v` one of Groovy's `Number` types — the receivers a bitwise, shift or
/// `**` operator is *defined* on? A `Boolean` is deliberately excluded: it
/// carries its own `and`/`or`/`xor` (so `true & false` is `false`) but is not a
/// `Number`, and `true & 1` raises `MissingMethodException` naming
/// `java.lang.Boolean`.
fn is_number(v: &Value) -> bool {
    matches!(v, Value::Int(_) | Value::Float(_)) || as_dec(v).is_some()
}

/// Is `v` an `Integer`/`Long`/`BigInteger` — a number whose bits Java will
/// actually shift or mask? [`is_integral`] answers the *arithmetic tower*
/// question and counts a `Boolean`; this one answers the *operator* question
/// and does not.
fn is_integral_number(v: &Value) -> bool {
    matches!(v, Value::Int(_)) || as_bigint(v).is_some()
}

/// Raise what Groovy raises when a bitwise, shift or `**` operator is applied to
/// operands it is not defined on. `op` is the Groovy *method* name the operator
/// desugars to — `leftShift`, `rightShift`, `rightShiftUnsigned`, `and`, `or`,
/// `xor`, `power`.
///
/// Four different throwables, keyed on which operand is wrong and how. All four
/// verified against Apache Groovy 5.0.8 on JDK 21 (`parity-scripts/messages.txt`
/// pins each one):
///
/// | operands | throwable and message |
/// |---|---|
/// | `null >> 2` | `NullPointerException: Cannot invoke method rightShift() on null object` |
/// | `"x" >> 2`, `1 >> "x"`, `box >> 2` | `MissingMethodException: No signature of method: rightShift for class: java.lang.String is applicable for argument types: (Integer) values: [2]` |
/// | `1 >> 2.0` | `UnsupportedOperationException: Shift distance must be an integral type, but 2.0 (java.math.BigDecimal) was supplied` |
/// | `1.0 >> 2`, `1 & 2.0`, `1G >>> 2` | `UnsupportedOperationException: Cannot use rightShift() on this number type: java.math.BigDecimal with value: 1.0` |
///
/// Two orderings in that table are load-bearing and were read off the oracle
/// rather than reasoned out. A non-`Number` right operand outranks everything:
/// `1G >>> "x"` is the `MissingMethodException`, not the "`BigInteger` has no
/// `>>>`" `UnsupportedOperationException`. And the mask operators name the
/// **left** operand even when the right one is what is wrong — `1 & 2.0` reports
/// `java.lang.Integer with value: 1`, saying nothing about the `2.0`.
fn raise_operator_operand(vm: &mut VM, op: &str, lhs: &Value, rhs: &Value) -> Value {
    // A null receiver never gets as far as the operator: it is the same NPE any
    // method call on `null` raises, named after the method the operator is.
    if matches!(lhs, Value::Undef) {
        raise(
            vm,
            "NullPointerException",
            &format!("Cannot invoke method {op}() on null object"),
        );
        return Value::Undef;
    }
    // A receiver that is not a `Number` has no such method at all, and neither
    // does a `Number` handed an argument no overload accepts. `**` stops here
    // for every remaining case: it is defined on *any* pair of `Number`s
    // (`2G ** 2.0` is `4`), so a `Number` receiver and a `Number` argument
    // cannot reach the operand-shape errors below.
    if !is_number(lhs) || !is_number(rhs) || op == "power" {
        return raise_missing_method(vm, lhs, op, std::slice::from_ref(rhs));
    }
    let unsupported_on = |vm: &mut VM, v: &Value| {
        raise(
            vm,
            "UnsupportedOperationException",
            &format!(
                "Cannot use {op}() on this number type: {} with value: {}",
                java_class_name(v),
                groovy_str(v)
            ),
        );
    };
    // `BigInteger` is two's-complement but unbounded, so it has no fill width
    // for `>>>` and Groovy declines the operator outright — while `1G >> 2` and
    // `1G & 3G` both answer.
    let no_unsigned_fill = op == "rightShiftUnsigned" && as_bigint(lhs).is_some();
    if !is_integral_number(lhs) || no_unsigned_fill {
        unsupported_on(vm, lhs);
        return Value::Undef;
    }
    // An integral receiver and a fractional argument: the shifts blame the
    // distance, the mask operators blame the receiver.
    if matches!(op, "leftShift" | "rightShift" | "rightShiftUnsigned") {
        raise(
            vm,
            "UnsupportedOperationException",
            &format!(
                "Shift distance must be an integral type, but {} ({}) was supplied",
                groovy_str(rhs),
                java_class_name(rhs)
            ),
        );
    } else {
        unsupported_on(vm, lhs);
    }
    Value::Undef
}

/// The class a `GroovyCastException` names for an `as` target. A *primitive*
/// target reports its **wrapper** — `[1, 2] as int` blames `java.lang.Integer`,
/// not `int` — and the numeric wrappers and `java.math` pair carry their
/// packages. Anything unrecognised is a script-declared class, which prints
/// bare. Verified against Apache Groovy 5.0.8.
fn cast_target_class(ty_simple: &str) -> String {
    match ty_simple {
        "int" | "Integer" => "java.lang.Integer",
        "long" | "Long" => "java.lang.Long",
        "short" | "Short" => "java.lang.Short",
        "byte" | "Byte" => "java.lang.Byte",
        "double" | "Double" => "java.lang.Double",
        "float" | "Float" => "java.lang.Float",
        "char" | "Character" => "java.lang.Character",
        "boolean" | "Boolean" => "java.lang.Boolean",
        "String" => "java.lang.String",
        "BigDecimal" => "java.math.BigDecimal",
        "BigInteger" => "java.math.BigInteger",
        other => other,
    }
    .to_string()
}

/// Raise the `GroovyCastException` a `value as Type` with no such coercion
/// raises. A **map** source has its own wording — Groovy tries to build the
/// target from the map's keys first and reports why it could not — while every
/// other source reports the value and both class names. Both verified against
/// Apache Groovy 5.0.8:
///
/// ```text
/// [1, 2] as Integer  →  Cannot cast object '[1, 2]' with class 'java.util.ArrayList' to class 'java.lang.Integer'
/// [a: 1] as Integer  →  Cannot coerce a map to class java.lang.Integer because it is a final class
/// ```
fn raise_cast(vm: &mut VM, v: &Value, ty_simple: &str) -> Value {
    let target = cast_target_class(ty_simple);
    let message = if as_omap(v).is_some() || matches!(v, Value::Hash(_)) {
        format!("Cannot coerce a map to class {target} because it is a final class")
    } else {
        format!(
            "Cannot cast object '{}' with class '{}' to class '{target}'",
            groovy_str(v),
            java_class_name(v)
        )
    };
    raise(vm, "GroovyCastException", &message);
    Value::Undef
}

/// Raise `groovy.lang.MissingPropertyException` for an unresolved `recv.name`.
fn raise_missing_property(vm: &mut VM, recv: &Value, name: &str) -> Value {
    let class = java_class_name(recv);
    let payload_class = class.clone();
    raise_with(
        vm,
        "MissingPropertyException",
        Some(&format!("No such property: {name} for class: {class}")),
        // `getProperty()` and `getType()` — what Groovy's own
        // `MissingPropertyException` exposes to a handler.
        || {
            vec![
                ("property", Value::str(name.to_string())),
                ("type", heap_push(HeapObj::ClassRef(payload_class))),
            ]
        },
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
        // A window names the JDK's nested class. Read through the *raw*
        // accessor: `getClass()` is one of the two calls Groovy still answers on
        // a stale window (`is()` is the other), so it must not comodification-check.
        _ if is_sublist(v) => "java.util.ArrayList$SubList",
        _ if as_list_raw(v).is_some() => "java.util.ArrayList",
        _ if as_bigint(v).is_some() => "java.math.BigInteger",
        _ if as_dec(v).is_some() => "java.math.BigDecimal",
        _ if omap_kind(v).is_some() => map_class(omap_kind(v).unwrap()),
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
/// A **nested** class is separated by `$` rather than `.`, and `getSimpleName`
/// drops that too — `java.util.ArrayList$SubList` is `SubList`, which is also the
/// name a `MissingMethodException` lists in its `argument types: (…)`. Both
/// verified against Apache Groovy 5.0.8.
fn simple_name_of(qualified: &str) -> String {
    qualified
        .rsplit(['.', '$'])
        .next()
        .unwrap_or(qualified)
        .to_string()
}

/// Allocate a built-in throwable instance on the host heap. A `None` message is
/// the `null` a message-less `NumberFormatException` carries.
///
/// `payload` is the diagnostic state the throwable's own class
/// declares — `method`/`type`/`arguments` on a `MissingMethodException`,
/// `property`/`type` on a `MissingPropertyException`. Groovy exposes each
/// through both a getter and a property (`e.getMethod()` and `e.method`), and
/// both read the field this puts on the instance.
fn new_throwable_with(
    class: &str,
    message: Option<&str>,
    payload: Vec<(&'static str, Value)>,
) -> Value {
    let cid = find_class(class).unwrap_or(0);
    let mut fields = std::collections::HashMap::new();
    fields.insert(
        "message".to_string(),
        message.map_or(Value::Undef, |m| Value::str(m.to_string())),
    );
    for (k, v) in payload {
        fields.insert(k.to_string(), v);
    }
    heap_push(HeapObj::Instance(Instance { class: cid, fields }))
}

/// Apply the JDK's `Throwable` constructors to a freshly built built-in
/// throwable: `T()`, `T(String message)`, `T(String message, Throwable cause)`
/// and `T(Throwable cause)` — the last taking its message from the cause, so
/// `new RuntimeException(new IOException("b")).getMessage()` is
/// `java.io.IOException: b`. Verified against Apache Groovy 5.0.8 / JVM 21.0.12.
///
/// Answers `false` for an arity the JDK has no constructor for, which the caller
/// reports. Shared by `new T(…)` ([`b_new`]) and a user subclass's `super(…)`
/// ([`b_super_ctor`]).
fn init_builtin_throwable(handle: &Value, args: &[Value]) -> bool {
    match args {
        [] => true,
        // `T(Throwable cause)`: the message is the cause's `toString()`.
        [only] if is_throwable_value(only) => {
            set_instance_field(handle, "message", Value::str(throwable_str(only)));
            set_instance_field(handle, "cause", only.clone());
            true
        }
        [message] => {
            set_instance_field(handle, "message", Value::str(groovy_str(message)));
            true
        }
        [message, cause] => {
            set_instance_field(handle, "message", Value::str(groovy_str(message)));
            set_instance_field(handle, "cause", cause.clone());
            true
        }
        _ => false,
    }
}

/// Is `v` an instance of a class descending from the built-in `Throwable`?
fn is_throwable_value(v: &Value) -> bool {
    as_instance(v).is_some_and(|i| is_throwable_class(i.class))
}

/// The `Throwable` members Groovy answers that are not a plain field read:
/// `getCause`/`initCause` and `getSuppressed`/`addSuppressed`, plus the
/// `toString`/`getLocalizedMessage` pair.
///
/// `None` means "not one of these", so the caller falls through to its own
/// dispatch. Reached from a method call (`e.getCause()`) and from a property
/// read (`e.cause`), which Groovy maps to the same getter.
fn throwable_member(recv: &Value, method: &str, args: &[Value]) -> Option<Value> {
    let inst = as_instance(recv)?;
    let field = |name: &str| inst.fields.get(name).cloned().unwrap_or(Value::Undef);
    Some(match method {
        "toString" => Value::str(throwable_str(recv)),
        "getLocalizedMessage" => field("message"),
        // `null` until something sets one — never absent, so a script printing
        // `e.getCause()` on an ordinary throwable prints `null`, as Groovy does.
        "getCause" => field("cause"),
        "initCause" => {
            set_instance_field(recv, "cause", args.first().cloned().unwrap_or(Value::Undef));
            // `initCause` answers the receiver, so `e.initCause(c).getMessage()`
            // chains. (The JDK also refuses a second call with an
            // `IllegalStateException`; groovyrs takes the second one.)
            recv.clone()
        }
        // A `Throwable[]`, which groovyrs models as the transient list form —
        // the same simplification `args` carries (see the BUGS.md `args` entry).
        // Empty until something is suppressed, and no heap slot either way.
        "getSuppressed" => match field("suppressed") {
            v @ Value::Array(_) => v,
            _ => Value::array(Vec::new()),
        },
        "addSuppressed" => {
            let mut list = match field("suppressed") {
                Value::Array(items) => items.to_vec(),
                _ => Vec::new(),
            };
            list.push(args.first().cloned().unwrap_or(Value::Undef));
            set_instance_field(recv, "suppressed", Value::array(list));
            Value::Undef
        }
        _ => return None,
    })
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
    /// A Groovy map. Entries are *stored* in insertion order whatever the
    /// implementation; [`MapKind`] decides the order they are iterated and
    /// printed in, exactly as [`SetKind`] does for a set.
    ///
    /// Lives on the heap so `println` order is Groovy's and `m.k = v` mutates in
    /// place through the shared handle.
    ///
    /// Keeping storage in insertion order and deriving presentation from `kind`
    /// is what makes one accessor ([`as_omap`]) fix every read at once: `println`,
    /// `each`, `collect`, `keySet`, `values`, `entrySet`, `iterator`, `inject`
    /// and `groupBy` all go through it, so a `TreeMap` sorts in all of them
    /// without a per-method sort. It also keeps `omap_set` a plain append —
    /// re-sorting on every write would make `m.c = 3` O(n log n).
    OrderedMap {
        entries: Vec<(String, Value)>,
        /// Key → its position in `entries`. A Groovy map is a `HashMap` under
        /// every implementation name, so a lookup and an overwrite must not
        /// scan: with `entries` alone, filling one took time quadratic in its
        /// size (16 000 `m["k$i"] = i` writes took 9 s against Apache Groovy's
        /// 1.1 s, and doubling the count quadrupled it).
        ///
        /// Rebuilt wholesale by [`omap_retain`], which is the only mutator that
        /// moves an entry; every other write appends or overwrites in place, so
        /// a position, once handed out, stays valid.
        index: HashMap<String, usize>,
        kind: MapKind,
    },
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
    /// allocate a handle.
    ///
    /// This used to claim the invariant is that a list which **escapes to user
    /// code** is built by [`glist`] and nothing else. That is false, and the
    /// falsehood is load-bearing: a *literal* gets a handle, but a list a GDK
    /// method **returns** escapes as a bare `Value::Array`, so `collect`,
    /// `transpose`, `collate`, `withIndex`, `combinations`, `permutations`,
    /// `subsequences`, `groupBy` and `split` all answer something a second name
    /// cannot observe a mutation through. BUGS.md carries the entry ("Lists a
    /// GDK method returns are not references"); reading this comment as the
    /// invariant it announced would hide it. Allocating a handle per GDK result
    /// is what would make the claim true, and it needs heap reclamation the host
    /// heap does not have — a loop calling `collect` would grow it without
    /// bound.
    ///
    /// `mod_count` is `java.util.ArrayList`'s own field: a counter bumped by
    /// every *structural* modification (one that changes the length — plus the
    /// two the JDK bumps anyway, `ArrayList.sort` and `addAll` with an empty
    /// argument). Nothing reads it except a [`HeapObj::SubList`] taken onto this
    /// list, which is the only construct in Groovy that can observe it.
    ListVal {
        items: Vec<Value>,
        mod_count: u64,
    },
    /// A `java.util.ArrayList$SubList` — a **live window** onto a `ListVal`,
    /// what `list.subList(from, to)` answers.
    ///
    /// Not a copy: a write through the window reaches the backing list and a
    /// write to the backing list shows through the window, because both names
    /// resolve to the same element storage. `root` is the backing `ListVal`'s
    /// handle (a window onto a window still points at the *root* list, as the
    /// JDK's does), `offset` is absolute in the root's storage, and `len` is the
    /// window's own size.
    ///
    /// `parent` is the handle this window was taken from — the root itself for a
    /// first-level window, another `SubList` for a nested one. It exists for
    /// exactly one job, the JDK's `updateSizeAndModCount`: a structural change
    /// made *through* this window has to resize this window and every window it
    /// was taken through, walking up. It deliberately does not walk *down*, so a
    /// window taken **from** this one is invalidated by the change — which is
    /// what Groovy 5.0.8 does.
    ///
    /// `exp_mod` is the root's `mod_count` as of the last operation this window
    /// took part in. Any other reference structurally modifying the root moves
    /// the root's counter past it, and every subsequent read or write through
    /// this window is then a `ConcurrentModificationException` — permanently,
    /// since the counter never comes back down. See [`check_comodification`].
    SubList {
        root: u32,
        parent: u32,
        offset: usize,
        len: usize,
        exp_mod: u64,
    },
}

/// Which `java.util.Map` implementation a map handle is, which is exactly the
/// question of what order it presents its entries in. The set-side twin of
/// [`SetKind`], and it reuses that side's ordering machinery — a map's keys are
/// bucketed by [`hash_order`] and sorted by [`natural_order`] the same way a
/// set's elements are.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MapKind {
    /// `java.util.LinkedHashMap` — insertion order. Every map literal, and what
    /// a GDK method that rebuilds a map answers.
    Linked,
    /// `java.util.HashMap` — the JDK's bucket order (see [`hash_order`]). `req`
    /// is the initial capacity the construction path asked for; a bare
    /// `new HashMap()` takes the default 16 and `new HashMap(Map)` pre-sizes
    /// from the argument (see [`hash_req_for_map`]).
    Hash { req: usize },
    /// `java.util.TreeMap` — ascending key order.
    Tree,
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

/// Allocate a Groovy `List` behind a heap handle, which is what makes two names
/// see one list.
///
/// A **literal** is built here (via `GMAKE_LIST`), and so is the script binding's
/// `args`. A GDK method's *result* is not: it comes back as the transient
/// `Value::Array` the match arms produce, and the compiler's receiver write-back
/// is what makes `a.sort()` show through `a`. So `def s = [1, 2].collect { it }`
/// names an array, not a shared list, and a second name for it does not alias —
/// see the BUGS.md entry. Allocating here for every GDK call instead would push
/// a heap entry per iteration of any loop that calls one, on a heap that is only
/// cleared per run.
fn glist(items: Vec<Value>) -> Value {
    heap_push(HeapObj::ListVal {
        items,
        mod_count: 0,
    })
}

/// Where a list handle's elements actually live: the backing `ListVal`'s heap
/// id, and the `[offset, offset + len)` window of it this handle names. A root
/// list is the whole of itself; a `SubList` is a window onto its root.
fn list_slot(v: &Value) -> Option<(u32, usize, usize)> {
    let Value::Obj(id) = v else { return None };
    HEAP.with(|h| match h.borrow().get(*id as usize) {
        Some(HeapObj::ListVal { items, .. }) => Some((*id, 0, items.len())),
        Some(HeapObj::SubList {
            root, offset, len, ..
        }) => Some((*root, *offset, *len)),
        _ => None,
    })
}

/// The elements behind a `java.util.List` handle **without** checking a window's
/// comodification — the shape/identity/class questions, which Groovy answers on
/// a stale window too (`s.getClass()` and `s.is(s)` both work after the backing
/// list has moved on). Every site that reads a list's *contents* wants
/// [`as_list`] instead.
fn as_list_raw(v: &Value) -> Option<Vec<Value>> {
    let Value::Obj(id) = v else { return None };
    HEAP.with(|h| {
        let h = h.borrow();
        match h.get(*id as usize) {
            Some(HeapObj::ListVal { items, .. }) => Some(items.clone()),
            Some(HeapObj::SubList {
                root, offset, len, ..
            }) => match h.get(*root as usize) {
                Some(HeapObj::ListVal { items, .. }) => {
                    items.get(*offset..*offset + *len).map(<[Value]>::to_vec)
                }
                _ => None,
            },
            _ => None,
        }
    })
}

/// The elements behind a `java.util.List` handle, if `v` is one.
///
/// Reading a `SubList` whose backing list has been structurally modified through
/// any other reference raises `ConcurrentModificationException` first — this is
/// the read half of Java's fail-fast rule, and putting it here is what puts it
/// on every path that consumes a list's contents at once (rendering, iteration,
/// subscripting, the operators, and the whole GDK) rather than on a list of
/// call sites that could drift apart.
fn as_list(v: &Value) -> Option<Vec<Value>> {
    check_comodification(v);
    as_list_raw(v)
}

/// The root list's structural-modification counter.
fn list_mod_count(root: u32) -> u64 {
    HEAP.with(|h| match h.borrow().get(root as usize) {
        Some(HeapObj::ListVal { mod_count, .. }) => *mod_count,
        _ => 0,
    })
}

/// Java's `ArrayList.SubList.checkForComodification`: a window is stale once the
/// root list's `mod_count` has moved past the value the window last synced to.
/// Raises the message-less `java.util.ConcurrentModificationException` Groovy
/// 5.0.8 raises and answers `false`; `true` means the value is usable (which
/// every non-window value trivially is).
fn check_comodification(v: &Value) -> bool {
    let Value::Obj(id) = v else { return true };
    let stale = HEAP.with(|h| {
        let h = h.borrow();
        match h.get(*id as usize) {
            Some(HeapObj::SubList { root, exp_mod, .. }) => match h.get(*root as usize) {
                Some(HeapObj::ListVal { mod_count, .. }) => *mod_count != *exp_mod,
                _ => false,
            },
            _ => false,
        }
    });
    if stale {
        // `raise` needs a VM only for the un-armed hard-fault path; the armed
        // path parks a catchable throwable and never touches it. Reading a stale
        // window happens under `deref_list`/`groovy_str`, several of which have
        // no VM in scope, so the ambient one is borrowed here.
        with_vm(|vm| raise_opt(vm, "ConcurrentModificationException", None));
    }
    !stale
}

/// Is `v` a `java.util.List` — either a list handle or the transient
/// `Value::Array` form? The shape test every "did I get a list?" site uses, so
/// none of them has to know which of the two representations it is holding.
fn is_list(v: &Value) -> bool {
    matches!(v, Value::Array(_)) || as_list_raw(v).is_some()
}

/// The heap id of a list handle, if `v` is one — what an in-place mutator writes
/// through and what `is()` compares. A window answers **its own** id, not its
/// root's, so `a.is(a.subList(0, a.size()))` is false as Groovy's is.
fn list_id(v: &Value) -> Option<u32> {
    match v {
        Value::Obj(id) if as_list_raw(v).is_some() => Some(*id),
        _ => None,
    }
}

/// Is this handle a live window rather than a whole list?
fn is_sublist(v: &Value) -> bool {
    matches!(v, Value::Obj(id) if HEAP.with(|h| matches!(h.borrow().get(*id as usize), Some(HeapObj::SubList { .. }))))
}

/// Replace a list handle's contents in place. This is what makes a mutation
/// visible through every other name for the same list.
///
/// Through a **window** the new contents are spliced into the backing list at
/// the window's offset, so the write reaches every other reference to that list.
/// `structural` says whether the operation is one the JDK counts (see
/// [`bumps_mod_count`]); a structural one bumps the root's counter — invalidating
/// every *other* window onto it — and then re-syncs this window and each window
/// it was taken through, which is the JDK's `updateSizeAndModCount`. A length
/// change is structural whatever the caller claims, since the window's size
/// moved.
fn list_store(id: u32, items: Vec<Value>, structural: bool) {
    let Some((root, offset, len)) = list_slot(&Value::Obj(id)) else {
        return;
    };
    let structural = structural || items.len() != len;
    let new_len = items.len();
    HEAP.with(|h| {
        if let Some(HeapObj::ListVal {
            items: slot,
            mod_count,
        }) = h.borrow_mut().get_mut(root as usize)
        {
            let end = (offset + len).min(slot.len());
            slot.splice(offset.min(end)..end, items);
            if structural {
                *mod_count += 1;
            }
        }
    });
    if !structural || root == id {
        return;
    }
    let synced = list_mod_count(root);
    let delta = new_len as isize - len as isize;
    // `updateSizeAndModCount`: this window and every window it was taken
    // through resize and re-sync. Never downward — a window taken *from* this
    // one keeps its old count and is invalidated, which is Groovy's behaviour.
    let mut cur = id;
    loop {
        let next = HEAP.with(|h| match h.borrow_mut().get_mut(cur as usize) {
            Some(HeapObj::SubList {
                parent,
                len,
                exp_mod,
                ..
            }) => {
                *len = len.saturating_add_signed(delta);
                *exp_mod = synced;
                Some(*parent)
            }
            _ => None,
        });
        match next {
            Some(parent) if parent != cur => cur = parent,
            _ => return,
        }
    }
}

/// Normalise a list handle to the transient `Value::Array` that the GDK read
/// arms pattern-match on; any other value passes through untouched.
///
/// This is the single adapter between the two representations. Applying it to a
/// receiver (and to the operands of an operator) is what let every existing
/// `(Value::Array(a), "method")` arm keep working unchanged when lists became
/// handles — the alternative was rewriting seventy match arms. A window derefs
/// to the elements it spans, so those same arms read a window without knowing
/// one exists.
fn deref_list(v: &Value) -> Value {
    match as_list(v) {
        Some(items) => Value::array(items),
        None => v.clone(),
    }
}

/// Does `method` count as a *structural* modification — one the JDK bumps
/// `modCount` for, and so one that invalidates every window taken before it?
///
/// Only the operations that bump **unconditionally** are named here. The ones
/// that bump only when they changed something (`removeAll`, `retainAll`,
/// `removeElement`, and a `remove` that found nothing) are left to
/// [`list_store`]'s length rule, which is the same question asked after the fact.
///
/// Two of them differ between a whole list and a window, because the JDK reaches
/// them through different code. Measured against Apache Groovy 5.0.8 (JVM
/// 26.0.2), taking a window and then running the operation on the named
/// receiver:
///
/// | operation                | on the list  | on a window  |
/// |--------------------------|--------------|--------------|
/// | `sort()`, any size       | invalidates  | does not     |
/// | `addAll([])`             | invalidates  | does not     |
/// | `clear()`, even on empty | invalidates  | invalidates  |
/// | `unique()`, size > 1     | invalidates  | invalidates  |
/// | `unique()`, size 0 or 1  | does not     | does not     |
/// | `unique { … }`, any size | invalidates  | invalidates  |
/// | `set`/`swap`             | does not     | does not     |
/// | `sort(false)`            | does not     | does not     |
///
/// `ArrayList.sort` and `ArrayList.addAll` bump the counter before looking at
/// whether they changed anything; `List.sort`'s default reorders through `set`
/// alone, and `SubList.addAll` returns before the counter when its argument is
/// empty. `unique` is Groovy's own and ends in `clear()` + `addAll(…)` whatever
/// it found — except that the argument-less and `unique(boolean)` forms return
/// early on a collection that cannot hold a duplicate, which the closure form
/// does not.
fn bumps_mod_count(method: &str, args: &[Value], is_view: bool, len: usize) -> bool {
    // The `sort`/`unique` forms that mutate the receiver at all: the same rule
    // `compiler::Compiler::emit_receiver_writeback` uses. `sort(false)` asks for
    // a copy and leaves the receiver — and its counter — alone.
    let mutating = args
        .iter()
        .all(|a| closure_meta(a).is_some() || matches!(a, Value::Bool(true)));
    let has_closure = args.iter().any(|a| closure_meta(a).is_some());
    match method {
        "add" | "leftShift" | "push" | "remove" | "removeAt" | "pop" | "removeLast" | "clear" => {
            true
        }
        "unique" => mutating && (len > 1 || has_closure),
        "sort" => mutating && !is_view,
        "addAll" => !is_view,
        _ => false,
    }
}

/// `list.subList(from, to)` — a live `java.util.ArrayList$SubList` window.
///
/// The three bounds outcomes are the JDK's own and they are distinct: a negative
/// `fromIndex` and a `toIndex` past the end are both `IndexOutOfBoundsException`
/// but name *different* indices, while a reversed range is an
/// `IllegalArgumentException` instead. Checking `from`, then `to`, then the
/// ordering is `ArrayList.subListRangeCheck`'s order, so a call that is wrong in
/// two ways reports the same one the JDK reports.
///
/// The bounds are checked against the **receiver's** length, and the window's
/// offset is relative to the receiver, so a window taken from a window spans the
/// right elements while still pointing at the root list (which is where the JDK
/// points it too).
fn make_sublist(vm: &mut VM, recv: &Value, args: &[Value]) -> Value {
    let (Some((root, offset, len)), Value::Obj(parent)) = (list_slot(recv), recv) else {
        return Value::Undef;
    };
    let from = args.first().and_then(as_i64).unwrap_or(0);
    let to = args.get(1).and_then(as_i64).unwrap_or(0);
    if from < 0 {
        raise(
            vm,
            "IndexOutOfBoundsException",
            &format!("fromIndex = {from}"),
        );
        return Value::Undef;
    }
    if to > len as i64 {
        raise(vm, "IndexOutOfBoundsException", &format!("toIndex = {to}"));
        return Value::Undef;
    }
    if from > to {
        raise(
            vm,
            "IllegalArgumentException",
            &format!("fromIndex({from}) > toIndex({to})"),
        );
        return Value::Undef;
    }
    heap_push(HeapObj::SubList {
        root,
        parent: *parent,
        offset: offset + from as usize,
        len: (to - from) as usize,
        exp_mod: list_mod_count(root),
    })
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

/// The entries of an ordered-map handle **in the order it presents them** — what
/// iterating it yields and what `toString` lays out. The map-side twin of
/// [`set_elements`], and the single choke point that makes a `TreeMap` sort
/// everywhere: every read arm in the GDK reaches its entries through here.
///
/// A caller that needs *storage* order (only the mutators, which append) works
/// against the heap directly.
fn as_omap(v: &Value) -> Option<Vec<(String, Value)>> {
    let (entries, kind) = as_omap_kind(v)?;
    Some(map_order(entries, kind))
}

/// The entries of an ordered-map handle in **storage** (insertion) order, with
/// the kind that decides how they are presented.
fn as_omap_kind(v: &Value) -> Option<(Vec<(String, Value)>, MapKind)> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::OrderedMap { entries, kind, .. }) => Some((entries.clone(), *kind)),
            _ => None,
        }),
        _ => None,
    }
}

/// Just the implementation kind of an ordered-map handle.
fn omap_kind(v: &Value) -> Option<MapKind> {
    as_omap_kind(v).map(|(_, k)| k)
}

/// Reorder insertion-ordered entries into the order `kind` presents them in.
/// A `HashMap` buckets its **keys** through the very same [`hash_order`] a
/// `HashSet` buckets its elements with, and a `TreeMap` sorts them through
/// [`natural_order`] — which, because a map key is stored as its rendered form,
/// is `String.compareTo` (see the `TreeMap` note in BUGS.md for the numeric-key
/// consequence).
fn map_order(entries: Vec<(String, Value)>, kind: MapKind) -> Vec<(String, Value)> {
    match kind {
        MapKind::Linked => entries,
        MapKind::Hash { req } => {
            let keys: Vec<Value> = entries.iter().map(|(k, _)| Value::str(k.clone())).collect();
            hash_order(&keys, req)
                .into_iter()
                .map(|i| entries[i].clone())
                .collect()
        }
        MapKind::Tree => {
            let mut out = entries;
            out.sort_by(|a, b| natural_order(&Value::str(a.0.clone()), &Value::str(b.0.clone())));
            out
        }
    }
}

/// The initial capacity `new HashMap(Map m)` asks for: the JDK's
/// `putMapEntries` pre-size, `Math.ceil(size / 0.75)`.
///
/// This is **not** [`hash_req_for_collection`]: `HashSet(Collection)` floors its
/// request at 16, `HashMap(Map)` does not, and the two really do iterate
/// differently as a result. Five entries land in an 8-slot table here and a
/// 16-slot one there.
fn hash_req_for_map(n: usize) -> usize {
    (n as f64 / 0.75).ceil() as usize
}

/// Build a `LinkedHashMap` handle — every map literal and every GDK method that
/// rebuilds a map from scratch.
fn gmap(entries: Vec<(String, Value)>) -> Value {
    gmap_kind(entries, MapKind::Linked)
}

/// The key → position index for a freshly built entry vector.
fn omap_index(entries: &[(String, Value)]) -> HashMap<String, usize> {
    entries
        .iter()
        .enumerate()
        .map(|(i, (k, _))| (k.clone(), i))
        .collect()
}

/// Build a map handle of a given implementation kind. `entries` is in insertion
/// order; the kind decides presentation.
fn gmap_kind(entries: Vec<(String, Value)>, kind: MapKind) -> Value {
    let index = omap_index(&entries);
    heap_push(HeapObj::OrderedMap {
        entries,
        index,
        kind,
    })
}

/// One entry of an ordered-map handle, without copying the rest of it.
///
/// The outer `None` means `v` is not a map; the inner one means the key is
/// absent. [`as_omap`] answers the same question by cloning every entry (and
/// re-ordering them, for a `HashMap`/`TreeMap`), which turns a single key read
/// into work proportional to the whole map — 100 000 reads of a 3 000-entry map
/// took 17.6 s that way against Apache Groovy's 1.4 s. Presentation order does
/// not matter to a keyed read, so this skips it entirely.
fn omap_get(v: &Value, key: &str) -> Option<Option<Value>> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::OrderedMap { entries, index, .. }) => {
                Some(index.get(key).map(|i| entries[*i].1.clone()))
            }
            _ => None,
        }),
        _ => None,
    }
}

/// Is `v` an ordered-map handle? The cheap question — [`as_omap`] answers it
/// too, but only by building the copy the caller may not want.
fn is_omap(v: &Value) -> bool {
    match v {
        Value::Obj(id) => HEAP.with(|h| {
            matches!(
                h.borrow().get(*id as usize),
                Some(HeapObj::OrderedMap { .. })
            )
        }),
        _ => false,
    }
}

/// The entry count of an ordered-map handle, without copying it.
fn omap_len(v: &Value) -> Option<usize> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::OrderedMap { entries, .. }) => Some(entries.len()),
            _ => None,
        }),
        _ => None,
    }
}

/// `java.util.NavigableMap`'s own methods — the ones a `TreeMap` answers and no
/// other map does. Returns `None` for a method that is not one of them, so the
/// caller falls through to the shared `Map` arms.
///
/// `entries` arrives sorted (a `Tree` map presents in key order), so a *range*
/// method is a filter and a *neighbour* method is a scan. The whole family is
/// keyed off [`utf16_cmp`] rather than `natural_order` for the same reason
/// [`map_order`] is: a map key is stored as its rendered form, so `TreeMap`'s
/// comparator here is `String.compareTo`, and the two must agree or a `headMap`
/// would cut a sequence it was not sorted by.
///
/// `comparator()` answers `null` because a `TreeMap` built without one uses the
/// keys' natural ordering, and no construction path here can supply one.
fn dispatch_navigable_map(
    vm: &mut VM,
    entries: &[(String, Value)],
    method: &str,
    args: &[Value],
) -> Option<Value> {
    let key_arg = |i: usize| args.get(i).map(groovy_str).unwrap_or_default();
    let entry_at = |i: Option<usize>| match i {
        Some(i) => {
            let (k, v) = &entries[i];
            heap_push(HeapObj::Entry(k.clone(), v.clone()))
        }
        None => Value::Undef,
    };
    let key_at = |i: Option<usize>| match i {
        Some(i) => Value::str(entries[i].0.clone()),
        None => Value::Undef,
    };
    // The four neighbour searches, as indices into the sorted `entries`.
    let strictly_below = |k: &str| {
        (0..entries.len())
            .rev()
            .find(|&i| entries[i].0.as_str() < k)
    };
    let at_or_below = |k: &str| {
        (0..entries.len())
            .rev()
            .find(|&i| entries[i].0.as_str() <= k)
    };
    let at_or_above = |k: &str| (0..entries.len()).find(|&i| entries[i].0.as_str() >= k);
    let strictly_above = |k: &str| (0..entries.len()).find(|&i| entries[i].0.as_str() > k);
    // A range view is materialised as a plain `TreeMap` of the selected entries.
    // The views' own class names (`TreeMap$AscendingSubMap`, `$DescendingSubMap`)
    // are not modeled — see the `TreeMap` entry in BUGS.md.
    let range = |keep: &dyn Fn(&str) -> bool| {
        gmap_kind(
            entries
                .iter()
                .filter(|(k, _)| keep(k))
                .cloned()
                .collect::<Vec<_>>(),
            MapKind::Tree,
        )
    };
    // `inclusive` defaults differ per method: `headMap(k)` excludes `k`,
    // `tailMap(k)` includes it.
    let flag = |i: usize, dflt: bool| match args.get(i) {
        Some(Value::Bool(b)) => *b,
        _ => dflt,
    };
    Some(match method {
        // `firstKey`/`lastKey` raise on an empty map where `firstEntry`/
        // `lastEntry` answer null — a JDK asymmetry, not an oversight.
        "firstKey" | "lastKey" => {
            if entries.is_empty() {
                raise(vm, "NoSuchElementException", "");
                return Some(Value::Undef);
            }
            key_at(Some(if method == "firstKey" {
                0
            } else {
                entries.len() - 1
            }))
        }
        "lowerKey" => key_at(strictly_below(&key_arg(0))),
        "floorKey" => key_at(at_or_below(&key_arg(0))),
        "ceilingKey" => key_at(at_or_above(&key_arg(0))),
        "higherKey" => key_at(strictly_above(&key_arg(0))),
        "lowerEntry" => entry_at(strictly_below(&key_arg(0))),
        "floorEntry" => entry_at(at_or_below(&key_arg(0))),
        "ceilingEntry" => entry_at(at_or_above(&key_arg(0))),
        "higherEntry" => entry_at(strictly_above(&key_arg(0))),
        "headMap" => {
            let (k, inc) = (key_arg(0), flag(1, false));
            range(&|x: &str| if inc { x <= &k } else { x < &k })
        }
        "tailMap" => {
            let (k, inc) = (key_arg(0), flag(1, true));
            range(&|x: &str| if inc { x >= &k } else { x > &k })
        }
        // `subMap` is the one name that collides: on a `TreeMap` two or four
        // arguments are the `NavigableMap` *range*, but the single-argument
        // collection form is still the GDK's key selection, so it falls through.
        "subMap" if args.len() == 2 && !is_list(&args[0]) => {
            let (lo, hi) = (key_arg(0), key_arg(1));
            range(&|x: &str| x >= &lo && x < &hi)
        }
        "subMap" if args.len() == 4 => {
            let (lo, hi) = (key_arg(0), key_arg(2));
            let (li, hi_inc) = (flag(1, true), flag(3, false));
            range(&|x: &str| {
                (if li { x >= &lo } else { x > &lo }) && (if hi_inc { x <= &hi } else { x < &hi })
            })
        }
        "descendingMap" => {
            let mut rev = entries.to_vec();
            rev.reverse();
            // A descending view is not a `TreeMap` — presenting it as one would
            // re-sort it ascending — so it is handed back as the insertion-
            // ordered map that holds the reversed sequence.
            gmap(rev)
        }
        "descendingKeySet" => Value::array(
            entries
                .iter()
                .rev()
                .map(|(k, _)| Value::str(k.clone()))
                .collect(),
        ),
        "navigableKeySet" => {
            Value::array(entries.iter().map(|(k, _)| Value::str(k.clone())).collect())
        }
        "comparator" => Value::Undef,
        _ => return None,
    })
}

/// Groovy's `DefaultGroovyMethods.createSimilarMap`: the kind of the map a GDK
/// method builds to hold a *subset* of a receiver's entries. It preserves only
/// sortedness — a `SortedMap` gets another `TreeMap`, and every other receiver
/// (including a `HashMap`) gets a `LinkedHashMap`, because the JDK offers no way
/// to clone a hash table's capacity. `findAll` is the method this governs;
/// `each` and `clone`, which answer the receiver itself or a true copy, keep the
/// exact kind instead.
fn similar_map_kind(kind: MapKind) -> MapKind {
    match kind {
        MapKind::Tree => MapKind::Tree,
        _ => MapKind::Linked,
    }
}

/// Set `key` on an ordered-map handle in place, preserving insertion order
/// (updating an existing key keeps its position; a new key appends). Returns
/// `false` if `v` is not an ordered map.
fn omap_set(v: &Value, key: String, val: Value) -> bool {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow_mut().get_mut(*id as usize) {
            Some(HeapObj::OrderedMap { entries, index, .. }) => {
                match index.get(&key) {
                    Some(i) => entries[*i].1 = val,
                    None => {
                        index.insert(key.clone(), entries.len());
                        entries.push((key, val));
                    }
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
        Value::Array(items) => glist(items.to_vec()),
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
    gmap(entries)
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
    // Every level here is a nested `VM::run` on the *Rust* stack, so unbounded
    // recursion through a closure, a method, an operator overload or a
    // constructor overflows the process stack — which aborts, and no `catch`
    // can see it. Groovy raises a `StackOverflowError` a script can catch.
    if vm.frames.len() >= MAX_CALL_DEPTH {
        raise_stack_overflow(vm);
        return Ok(Value::Undef);
    }
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
            let kind = match simple_name_of(class).as_str() {
                "TreeMap" => MapKind::Tree,
                "LinkedHashMap" => MapKind::Linked,
                // `new HashMap(Map)` pre-sizes its table from the argument;
                // `new HashMap()` takes the JDK's default 16.
                _ => MapKind::Hash {
                    req: if args.is_empty() {
                        DEFAULT_HASH_REQ
                    } else {
                        hash_req_for_map(entries.len())
                    },
                },
            };
            gmap_kind(entries, kind)
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
        //
        // `new BigDecimal(double)` is the *exact* binary expansion —
        // `new BigDecimal(0.555d)` is
        // `0.55500000000000004884981308350688777863979339599609375`, and the
        // gap between it and `0.555d as BigDecimal` (which is
        // `BigDecimal.valueOf`, the shortest round-tripping decimal) is the
        // classic Java surprise the constructor's Javadoc warns about. The two
        // were the wrong way round here: the constructor rendered the double
        // and re-parsed it, and the `as` coercion took the exact expansion.
        "BigDecimal" | "BigInteger" => {
            let big = simple_name_of(class) == "BigInteger";
            let carry = |d: BigDecimal| if big { bigint_value(d) } else { dec_value(d) };
            match &first() {
                Value::Int(n) => carry(decimal::from_i64(*n)),
                Value::Float(f) => match decimal::from_f64_exact(*f) {
                    Some(d) => carry(d),
                    None => {
                        raise(vm, "NumberFormatException", "Infinite or NaN");
                        Value::Undef
                    }
                },
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
    // registry is consulted first. A *qualified* name (`new
    // java.io.IOException(…)`) resolves through `resolve_class_name`, which
    // insists the package be the one groovyrs models for that class.
    if resolve_class_name(&name).is_none() {
        if let Some(v) = new_jdk(vm, &name, &args) {
            return v;
        }
    }
    let Some(cid) = resolve_class_name(&name) else {
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
    // JDK set `T()` / `T(String)` / `T(String, Throwable)` / `T(Throwable)`,
    // which is what a Groovy script means by `new Exception("boom")`,
    // `new RuntimeException("outer", e)`, or `class Plain extends
    // RuntimeException {}`.
    if !meta.ctors.contains_key(&argc)
        && is_throwable_class(cid)
        && init_builtin_throwable(&handle, &args)
    {
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
        // `super(message)` / `super(message, cause)` into the built-in throwable
        // chain — the modeled JDK constructors, which a user exception class
        // calls.
        if is_throwable_class(super_id) && init_builtin_throwable(&this, &args) {
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
        // A group index the pattern has no group for is an
        // `IndexOutOfBoundsException`, not a null: `Matcher.group(int)` range-
        // checks before it reads. Clamping the index to `0` instead turned
        // `m.group(9)` into the whole match and `m.group(-1)` into it as well.
        // The bound is the captured-group vector's length — slot 0 is the whole
        // match, so `("a" =~ /a/)` has exactly one and `group(1)` is already out.
        // `m.group("name")` is `Matcher.group(String)`, a *different* overload
        // from the index one: it reads the group `(?<name>…)` declared, and a
        // name the pattern never declared is an `IllegalArgumentException`.
        // Falling through to the index arm read the argument as `as_i64`, which
        // answers `None` for text and defaulted to `0` — so every named read
        // silently returned the whole match.
        "group" if matches!(args.first(), Some(Value::Str(_))) => {
            let name = groovy_str(&args[0]);
            let found = match &*crate::regex::compile(&m.source) {
                Ok(p) => p.group_index(&name),
                Err(_) => None,
            };
            match found {
                Some(i) => group(i),
                None => {
                    raise(
                        vm,
                        "IllegalArgumentException",
                        &format!("No group with name <{name}>"),
                    );
                    Value::Undef
                }
            }
        }
        "group" => {
            let n = args.first().and_then(as_i64).unwrap_or(0);
            let groups = m.last.as_ref().map_or(0, |h| h.groups.len()) as i64;
            if n < 0 || n >= groups {
                raise(vm, "IndexOutOfBoundsException", &format!("No group {n}"));
                Value::Undef
            } else {
                group(n as usize)
            }
        }
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
    is_case(vm, &label, &subject)
}

/// `Object.isCase(Object)` as a plain call rather than a stack builtin, so the
/// GDK methods that are *specified* in terms of it — `grep`, and the `switch`
/// lowering above — share one implementation. A `java.lang.Class` label is an
/// `isInstance` test (`[1, 'a'].grep(Integer)` keeps the integers); the
/// `switch` lowering never reaches that arm because the compiler resolves a
/// bare type name to [`GIS_CASE_TYPE`] before emitting, but a `grep` argument
/// is an ordinary expression and arrives here as a `ClassRef`.
fn is_case(vm: &mut VM, label: &Value, subject: &Value) -> Value {
    if let Some(class) = as_class_ref(label) {
        return Value::bool(value_is_a(subject, &class));
    }
    // A `Range` label contains — `case 1..5:` and `x in 1..5` both ask that.
    let label = range_as_list(label);
    let subject = subject.clone();
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
    // `Map.equals` is by *entry set* — it ignores both order and implementation,
    // so `[b: 2, a: 1] == [a: 1, b: 2]` and a `TreeMap` equals the
    // `LinkedHashMap` of the same entries. Decided here because the fallback
    // below compares rendered forms, which answers `false` for both: two maps
    // holding the same entries in different orders print differently.
    //
    // Only ever true against another map, the way the set arm above is: a map
    // and a list are never equal in Java whatever they contain.
    let (ma, mb) = (as_omap(a), as_omap(b));
    if ma.is_some() || mb.is_some() {
        return match (ma, mb) {
            (Some(x), Some(y)) => {
                x.len() == y.len()
                    && x.iter()
                        .all(|(k, v)| y.iter().any(|(k2, v2)| k == k2 && values_equal(v, v2)))
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
                p.len() == q.len() && p.iter().zip(q.iter()).all(|(i, j)| values_equal(i, j))
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

/// `java.lang.String.hashCode()`: `s[0]*31^(n-1) + … + s[n-1]` in wrapping
/// 32-bit arithmetic, over **UTF-16 code units** — an astral character counts as
/// its two surrogates, which is why `"a😀b".length()` is 4 in Java and the fold
/// has to run over `encode_utf16` rather than over Rust `char`s.
fn string_hash(s: &str) -> i32 {
    s.encode_utf16()
        .fold(0i32, |h, u| h.wrapping_mul(31).wrapping_add(u as i32))
}

/// `java.lang.Double.hashCode()`: `(int)(bits ^ (bits >>> 32))` over
/// `doubleToLongBits`, which collapses every NaN payload to one canonical bit
/// pattern before folding.
fn double_hash(f: f64) -> i32 {
    let bits = if f.is_nan() {
        0x7ff8_0000_0000_0000u64
    } else {
        f.to_bits()
    };
    (bits ^ (bits >> 32)) as u32 as i32
}

/// `Integer.hashCode()` (the value) or `Long.hashCode()`
/// (`(int)(v ^ (v >>> 32))`), chosen by the same rule [`java_class_name`] uses:
/// a `Value::Int` inside 32-bit range is an `Integer`, outside it a `Long`. The
/// case that rule misreads is the same one — a `Long` small enough to be an
/// `Integer` (`-1L` hashes to 0 as a `Long`, to -1 as an `Integer`) — because
/// the width is not carried on the value. See BUGS.md.
fn integer_hash(n: i64) -> i32 {
    match i32::try_from(n) {
        Ok(i) => i,
        Err(_) => ((n as u64) ^ ((n as u64) >> 32)) as u32 as i32,
    }
}

/// `java.util.AbstractList.hashCode()`: `hash = 31 * hash + e.hashCode()` from a
/// seed of 1, with a null element contributing 0.
fn list_hash(items: &[Value]) -> i32 {
    items.iter().fold(1i32, |h, e| {
        h.wrapping_mul(31).wrapping_add(object_hash_code(e))
    })
}

/// `groovy.lang.Range.hashCode()`. Only `IntRange` defines its own — the Cantor
/// pairing of its **normalised inclusive** bounds, `(from + to + 1) * (from + to)
/// / 2 + to` in `int` arithmetic (read off `IntRange.hashCode`'s bytecode in
/// groovy-5.0.8.jar). `NumberRange`, `ObjectRange` and `EmptyRange` declare no
/// `hashCode` at all, so they inherit `AbstractList`'s over the elements they
/// enumerate.
fn range_hash(r: &RangeVal) -> i32 {
    if range_class(r) == "groovy.lang.IntRange" {
        if let (Some(lo), Some(hi)) = (as_i64(&range_lower(r)), as_i64(&range_upper(r))) {
            let (lo, hi) = (lo as i32, hi as i32);
            let sum = lo.wrapping_add(hi);
            return (sum.wrapping_add(1).wrapping_mul(sum) / 2).wrapping_add(hi);
        }
    }
    list_hash(&range_elements(r))
}

/// `Object.hashCode()` for every value whose Java class specifies one — which is
/// every value a Groovy script can write a literal for. A value with no
/// specified contract (a closure, a `StringBuilder`, a `Pattern`, a user
/// instance that does not override it) gets Java's *identity* hash, and the heap
/// handle is groovyrs's identity: stable for the life of the object, equal
/// exactly when the references are equal. It is not the number a JVM would
/// print, but a JVM's own identity hash varies run to run, so no value could be.
fn object_hash_code(v: &Value) -> i32 {
    // The heap-backed shapes first: they all wear a `Value::Obj` tag, so the
    // variant alone cannot tell them apart.
    if let Some(items) = as_list_raw(v) {
        return list_hash(&items);
    }
    if let Some(entries) = as_omap(v) {
        // `AbstractMap`: the sum over entries of `keyHash ^ valueHash`. Every
        // groovyrs map key is a `String`, so a non-string key hashes as its
        // rendering rather than as itself — see BUGS.md.
        return entries.iter().fold(0i32, |h, (k, val)| {
            h.wrapping_add(string_hash(k) ^ object_hash_code(val))
        });
    }
    if let Some((k, val)) = as_entry(v) {
        // `Map.Entry.hashCode()` is that same `keyHash ^ valueHash`.
        return string_hash(&k) ^ object_hash_code(&val);
    }
    if let Some((items, kind)) = as_set(v) {
        // `AbstractSet`: the *sum* of the elements', so it does not depend on
        // iteration order and a `LinkedHashSet` matches the `TreeSet` of the
        // same elements.
        return set_elements(&items, kind)
            .iter()
            .fold(0i32, |h, e| h.wrapping_add(object_hash_code(e)));
    }
    if let Some(r) = as_range(v) {
        return range_hash(&r);
    }
    // A `BigInteger` answers `as_dec` too (it is a scale-0 `BigDecimal`), so it
    // has to be asked about first — the two hash by different rules.
    if let Some(n) = as_bigint(v) {
        return decimal::big_integer_hash(&n.as_bigint_and_exponent().0);
    }
    if let Some(d) = as_dec(v) {
        return decimal::big_decimal_hash(&d);
    }
    match v {
        Value::Str(s) => string_hash(s),
        Value::Int(n) => integer_hash(*n),
        Value::Float(f) => double_hash(*f),
        Value::Bool(b) => {
            if *b {
                1231
            } else {
                1237
            }
        }
        Value::Array(a) => list_hash(a),
        // Only reachable as an *element*: `null.hashCode()` is an NPE, which the
        // null receiver branch in `dispatch_call` raises before this is asked.
        // `AbstractList`/`AbstractMap` score a null member 0.
        Value::Undef => 0,
        Value::Obj(id) => *id as i32,
        other => string_hash(&groovy_str(other)),
    }
}

/// Whether `recv` is a class instance whose own `hashCode` overrides the
/// built-in one — the guard that keeps the universal hook from shadowing a user
/// method, the way `getClass` is guarded inside `dispatch_instance_method`.
fn has_user_hash_code(recv: &Value) -> bool {
    as_instance(recv).is_some_and(|inst| lookup_method(inst.class, "hashCode").is_some())
}

/// Whether `value` is an instance of the (user or built-in) type `class`.
/// Resolve a type name written in a `catch` clause or an `instanceof` to a class
/// registry id. A simple name is looked up as written; a **qualified** one
/// (`groovy.lang.MissingMethodException`, `java.io.IOException`) resolves to the
/// same class only when its package is the one groovyrs models for that name, so
/// a same-named type from another package still does not match.
fn resolve_class_name(name: &str) -> Option<u32> {
    if let Some(id) = find_class(name) {
        return Some(id);
    }
    let (_, short) = name.rsplit_once('.')?;
    let id = find_class(short)?;
    (crate::throwable::qualified(short) == name).then_some(id)
}

fn value_is_a(value: &Value, class: &str) -> bool {
    // `null` is never an instance of anything.
    if matches!(value, Value::Undef) {
        return false;
    }
    // A user class instance: the named class must appear in its superclass chain.
    if let Some(inst) = as_instance(value) {
        if let Some(target) = resolve_class_name(class) {
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
        // `instanceof` is a type test, not a read: a stale window is still a
        // `List`, so this asks the raw shape.
        "List" | "ArrayList" | "Collection" | "Iterable" => {
            matches!(value, Value::Array(_))
                || as_list_raw(value).is_some()
                || as_range(value).is_some()
        }
        "Range" | "IntRange" | "ObjectRange" | "NumberRange" => as_range(value).is_some(),
        // Every map answers `Map`. The concrete names follow the JDK's hierarchy
        // rather than aliasing each other: `LinkedHashMap extends HashMap`, so a
        // `LinkedHashMap` is a `HashMap` but **not** the other way round, and
        // `SortedMap`/`NavigableMap` are the `TreeMap`'s alone.
        "Map" | "AbstractMap" => matches!(value, Value::Hash(_)) || as_omap(value).is_some(),
        "LinkedHashMap" => {
            matches!(value, Value::Hash(_)) || omap_kind(value) == Some(MapKind::Linked)
        }
        "HashMap" => {
            matches!(value, Value::Hash(_))
                || matches!(
                    omap_kind(value),
                    Some(MapKind::Linked | MapKind::Hash { .. })
                )
        }
        "TreeMap" | "SortedMap" | "NavigableMap" => omap_kind(value) == Some(MapKind::Tree),
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
        if let Some(v) = throwable_member(recv, method, args) {
            return Some(Ok(v));
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
    // Groovy reads `e.cause` / `e.suppressed` through `getCause()` /
    // `getSuppressed()`, so a throwable's non-field members answer as properties
    // too. The field reads above already served `e.message`.
    if is_throwable_class(inst.class) {
        let getter = format!("get{}", capitalize(name));
        if let Some(v) = throwable_member(recv, &getter, &[]) {
            return Some(Ok(v));
        }
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
    if is_omap(&recv) {
        let k = index.as_str_cow().into_owned();
        return match omap_get(&recv, &k).flatten() {
            Some(v) => v,
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
            let len = utf16_len(s);
            let idx = if i < 0 { len as i64 + i } else { i };
            if idx < 0 {
                raise_negative_index(vm, i, len)
            } else if idx < len as i64 {
                Value::str(utf16_slice(s, idx as usize, idx as usize + 1))
            } else {
                raise(
                    vm,
                    "StringIndexOutOfBoundsException",
                    &format!("Range [{idx}, {}) out of bounds for length {len}", idx + 1),
                );
                Value::Undef
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
    if is_omap(&recv) {
        omap_set(&recv, groovy_str(&index), value);
        return recv;
    }
    // A list rides a handle, so the write goes *through* it and every other name
    // for the list observes it. The element math is [`list_put`], shared with the
    // transient-array arm below so the two can never drift.
    if let Some(id) = list_id(&recv) {
        if !check_comodification(&recv) {
            return Value::Undef;
        }
        return match list_put(as_list_raw(&recv).unwrap_or_default(), &index, value) {
            Ok(next) => {
                // `putAt(int)` replaces an element, which is not a structural
                // modification — unless the index was past the end, where Groovy
                // grows the list and the length rule inside `list_store` catches it.
                list_store(id, next, false);
                recv
            }
            Err((i, len)) => raise_negative_index(vm, i, len),
        };
    }
    match &recv {
        Value::Array(a) => match list_put(a.to_vec(), &index, value) {
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
/// With no delegate running there is nothing left that could bind the name, so
/// the read is Groovy's `MissingPropertyException` — `println zork` on an
/// undeclared `zork` raises rather than printing `null`. A *delegate* that does
/// not hold the name is a different question and keeps its own answer: a map
/// answers `null` for a key it lacks, because a bare name inside `with` is asked
/// of the delegate as a property and `[a: 1].zork` is `null`.
fn b_name_get(vm: &mut VM, _argc: u8) -> Value {
    let (gidx, name) = pop_name_site(vm);
    if owner_bound(vm, gidx) {
        return vm.globals[gidx].clone();
    }
    let Some(recv) = DELEGATES.with(|d| d.borrow().last().cloned()) else {
        // The bare-name miss names the *script* class, so `e.getType()` answers
        // it too — Groovy's `e.type` here is `class p7` for `p7.groovy`.
        raise_with(
            vm,
            "MissingPropertyException",
            Some(&format!(
                "No such property: {name} for class: {}",
                script_class()
            )),
            || {
                vec![
                    ("property", Value::str(name.clone())),
                    ("type", heap_push(HeapObj::ClassRef(script_class()))),
                ]
            },
        );
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

    /// The name Groovy gives the class it compiles the running script into —
    /// what a `MissingPropertyException` on a bare name names. It is
    /// *entry-point dependent*: `groovy Foo.groovy` compiles to class `Foo`,
    /// while `groovy -e '…'` compiles to `script_from_command_line`, which is
    /// the default here. [`set_script_class`] installs the file's stem.
    static SCRIPT_CLASS: RefCell<String> =
        const { RefCell::new(String::new()) };
}

/// Name the class the running script compiles into — the file's stem, for the
/// `groovy <file>` entry point. Leave it unset for `groovy -e`.
pub fn set_script_class(name: &str) {
    SCRIPT_CLASS.with(|s| *s.borrow_mut() = name.to_string());
}

/// Seed the script binding's `args` — the launcher arguments after the script,
/// which Groovy puts in every script's binding as a `String[]` (empty, not
/// absent, when there are none). Call after [`install`], which clears the heap
/// the list is allocated on. `names` is the chunk's name pool; a script that
/// never mentions `args` has no entry there and nothing is seeded.
pub fn bind_script_args(vm: &mut VM, names: &[String], argv: &[String]) {
    let Some(idx) = names.iter().position(|n| n == "args") else {
        return;
    };
    let list = glist(argv.iter().map(|a| Value::str(a.clone())).collect());
    if let Some(slot) = vm.globals.get_mut(idx) {
        *slot = list;
    }
}

/// The script class name for a diagnostic, defaulting to what `groovy -e` uses.
fn script_class() -> String {
    SCRIPT_CLASS.with(|s| {
        let s = s.borrow();
        if s.is_empty() {
            "script_from_command_line".to_string()
        } else {
            s.clone()
        }
    })
}

/// Take and clear any pending runtime-fault message (see `G_ERROR`).
pub fn take_error() -> Option<String> {
    G_ERROR.with(|e| e.borrow_mut().take())
}

/// Has a hard runtime fault been recorded? The un-armed twin of [`pending_exc`]:
/// a program with no `try` in it degrades a raise to a `groovyrs:` fault, and a
/// host loop that would otherwise keep producing output has to stop for that too.
fn faulted() -> bool {
    G_ERROR.with(|e| e.borrow().is_some())
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

thread_local! {
    /// The width mask of the method call being dispatched: bit 0 the receiver,
    /// bit `k+1` argument `k`, set when the compiler saw that position as a
    /// statically-`Long`. Zero for every call the compiler saw no `Long` at —
    /// which is nearly all of them — so a reader must treat a clear bit as "not
    /// known to be wide", never as "known to be an `Integer`". See
    /// [`GMETHOD_WIDE`].
    static CALL_WIDTHS: Cell<u8> = const { Cell::new(0) };
}

/// The width mask of the call currently dispatching.
fn call_widths() -> u8 {
    CALL_WIDTHS.with(|w| w.get())
}

/// Pop the plain [`GMETHOD`] stack shape: the method name, `argc` arguments, and
/// the receiver beneath them.
fn pop_call(vm: &mut VM, argc: u8) -> (String, Vec<Value>, Value) {
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
    (name, args, recv)
}

/// Groovy method-call builtin: the stack holds the receiver (deepest), `argc`
/// args, and the method name (a `String`) on top. Dispatches a faithful GDK
/// subset via `dispatch_method`.
fn b_method(vm: &mut VM, argc: u8) -> Value {
    let (name, args, recv) = pop_call(vm, argc);
    // No mask was pushed for this call, and the previous call's must not be read
    // as this one's.
    CALL_WIDTHS.with(|w| w.set(0));
    dispatch_call(vm, recv, &name, args)
}

/// [`GMETHOD_WIDE`]: `b_method` with the compiler's width mask pushed beneath
/// the receiver. The mask is published for the dispatch and cleared after it, so
/// a nested call the dispatch makes never inherits it.
fn b_method_wide(vm: &mut VM, argc: u8) -> Value {
    let (name, args, recv) = pop_call(vm, argc);
    let mask = vm.stack.pop().map(|m| m.to_int() as u8).unwrap_or(0);
    CALL_WIDTHS.with(|w| w.set(mask));
    let out = dispatch_call(vm, recv, &name, args);
    CALL_WIDTHS.with(|w| w.set(0));
    out
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
            // `inspect()` is a GDK method on `Object`, and `NullObject` answers
            // it rather than raising: `null.inspect()` is the text `null`.
            "inspect" => Value::str(inspect_value(&recv)),
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
    // `Object.hashCode()`. Above the per-type branches for the same reason
    // `equals` is: the `Set` and `Range` branches hand a method they do not
    // define to the *list* of their elements, and `AbstractSet` (a sum) and
    // `IntRange` (a Cantor pairing of its bounds) do not hash the way
    // `AbstractList` does — delegating would answer a list's hash for both. A
    // user class's own `hashCode` still wins, because `has_user_hash_code`
    // excludes an instance that declares one and the instance branch below
    // dispatches it.
    if method == "hashCode" && args.is_empty() && !has_user_hash_code(&recv) {
        return Value::int(object_hash_code(&recv) as i64);
    }
    // `with`/`tap` install the receiver as the closure's *delegate*, and a bare
    // mutator call inside the body (`[1, 2].tap { add(3) }`) has to reach this
    // list — so they run against the handle, further down, not against the
    // detached element copy this branch would hand them.
    let delegating =
        matches!(method, "with" | "tap") && args.len() == 1 && closure_meta(&args[0]).is_some();
    if let Some(id) = list_id(&recv).filter(|_| !delegating) {
        // `getClass()` reads the reference, not the elements, so it answers on a
        // stale window too — and it has to answer *here*, because the call below
        // runs against a detached `Value::Array` that would name the wrong class
        // (`java.util.ArrayList` for what is an `ArrayList$SubList`).
        if method == "getClass" && args.is_empty() {
            return class_ref_of(&recv);
        }
        // Every other call through a window is a fail-fast read: if the backing
        // list moved on, it throws before doing anything.
        if !check_comodification(&recv) {
            return Value::Undef;
        }
        // `subList` answers a live **window** onto this list, so it too is
        // decided on the handle rather than on the detached elements.
        if method == "subList" && args.len() == 2 {
            return make_sublist(vm, &recv, &args);
        }
        // `sort`/`unique` do not use the `MUTATED` slot — their *result* is the
        // new list (which is why the compiler writes the result back). Same rule
        // as `compiler::Compiler::emit_receiver_writeback`: only the no-argument,
        // `sort(true)` and closure forms mutate; `sort(false)` asks for a copy.
        let result_mutates = (matches!(method, "sort" | "unique")
            && args
                .iter()
                .all(|a| closure_meta(a).is_some() || matches!(a, Value::Bool(true))))
            // `reverse(true)` is the mutating spelling — it reverses the
            // receiver and answers it, so `a.is(a.reverse(true))`. Unlike
            // `sort`, the *no-argument* form copies, so only the explicit
            // `true` counts. Verified against Apache Groovy 5.0.8.
            || (method == "reverse" && matches!(args.as_slice(), [Value::Bool(true)]));
        let items = as_list_raw(&recv).unwrap_or_default();
        let structural = bumps_mod_count(method, &args, is_sublist(&recv), items.len());
        let answer = dispatch_call(vm, Value::array(items), method, args);
        // An in-place mutator parks its new contents for the compiler's
        // writeback. Store them through the handle instead: that is what every
        // *other* name for this list observes. Re-park the handle itself so a
        // writeback that does run stores a list back into the variable rather
        // than replacing it with a detached array.
        let mut mutated = take_mutated().is_some_and(|next| {
            list_store(id, iteration_elements(&next), structural);
            true
        });
        if result_mutates {
            if let Value::Array(next) = &answer {
                list_store(id, next.to_vec(), structural);
                mutated = true;
            }
        }
        if mutated {
            set_mutated(recv.clone());
        }
        // The methods whose Groovy answer *is* the receiver, so that
        // `a.is(a.sort())` is true and a chained `a << 1 << 2` keeps writing
        // into the one list. Verified against Groovy 5.0.8; `sort(false)`,
        // `reverse()`/`reverse(false)` and `collect` answer a new list, and are
        // absent — `reverse` reaches the list below only in its mutating
        // `reverse(true)` spelling, which `mutated` already gates.
        let answers_receiver = matches!(method, "each" | "eachWithIndex" | "reverseEach")
            || (mutated && matches!(method, "sort" | "unique" | "leftShift" | "swap" | "reverse"));
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
    // `grep` is defined on `Object`, not just on collections: Groovy iterates
    // the receiver with `InvokerHelper.asIterator`, which yields a *single*
    // element for anything that is not one. So `5.grep { it > 1 }` is `[5]`, and
    // `5.grep { it > 9 }` is `[]`. The collection receivers were handled above.
    if method == "grep"
        && args.len() <= 1
        && !matches!(recv, Value::Array(_) | Value::Str(_))
        && !is_omap(&recv)
        && as_set(&recv).is_none()
        && as_range(&recv).is_none()
    {
        if let Some(res) = dispatch_iteration(vm, std::slice::from_ref(&recv), method, &args) {
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
                    } else if matches!(method, "takeWhile" | "dropWhile") {
                        // `StringGroovyMethods.takeWhile`/`dropWhile` answer a
                        // **String**, not the character list every other
                        // closure-driven String method answers — `"abcdef"
                        // .takeWhile { it < 'd' }` is `abc`, and `[a, b, c]` was
                        // this branch handing back the raw element vector.
                        Value::str(
                            iteration_elements(&v)
                                .iter()
                                .map(groovy_str)
                                .collect::<String>(),
                        )
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
    //
    // Every arm of `dispatch_map_iteration` but `sort` and `grep` starts by
    // demanding a closure argument, so asking for one first decides nothing on
    // its own — but it decides it *without copying the map*, and reaching this
    // point with a copy in hand is what made every ordinary `m.put(k, v)` cost
    // a clone (twice: `as_omap` and `omap_kind` each built one) plus, for a
    // `HashMap`/`TreeMap`, a re-ordering of every entry.
    let map_iteration =
        matches!(method, "sort" | "grep") || args.iter().any(|a| closure_meta(a).is_some());
    if map_iteration && is_omap(&recv) {
        let (entries, kind) = as_omap_kind(&recv).unwrap_or_else(|| (Vec::new(), MapKind::Linked));
        let entries = map_order(entries, kind);
        if let Some(res) = dispatch_map_iteration(vm, &entries, kind, method, &args) {
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
    if method == "getAt" && args.len() == 1 && (!matches!(recv, Value::Obj(_)) || is_omap(&recv)) {
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
        // `list.grep(filter)` — keep the elements the **filter** accepts, where
        // "accepts" is `filter.isCase(element)`, not `element == filter`. So a
        // closure filter calls the closure, a `Class` filter is an `isInstance`
        // test, a `Pattern` filter is a whole-string match, and a collection or
        // range filter is a membership test — the same five rules a `switch`
        // label follows, which is why this shares [`is_case`] with it.
        //
        // The no-argument `grep()` is Groovy's `grep(Closure.IDENTITY)`: keep
        // the elements that are Groovy-true, so `[1, 'a', null, 0].grep()` is
        // `[1, 'a']`.
        //
        // It lives among the closure-driven arms rather than in the pure GDK
        // table because the closure form re-enters the VM; the other four forms
        // do not, but splitting one method across two dispatch tables by the
        // *shape of its argument* is how a filter ends up meaning two different
        // things.
        "grep" => {
            if args.len() > 1 {
                return None;
            }
            let mut out = Vec::new();
            for it in items {
                let keep = match args.first() {
                    Some(filter) => {
                        let hit = is_case(vm, filter, it);
                        if pending_exc() {
                            return Some(Ok(Value::Undef));
                        }
                        groovy_truthy(vm, &hit)
                    }
                    None => groovy_truthy(vm, it),
                };
                if keep {
                    out.push(it.clone());
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
            Some(Ok(gmap(
                groups
                    .into_iter()
                    .map(|(k, v)| (k, Value::array(v)))
                    .collect(),
            )))
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
            Some(Ok(gmap(
                counts
                    .into_iter()
                    .map(|(k, n)| (k, Value::int(n)))
                    .collect(),
            )))
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
            Some(Ok(gmap(out)))
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
        Value::Array(a) if spread => a.to_vec(),
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
/// `entries` arrives in the receiver's *presentation* order, so every closure
/// here already runs in the order the map iterates. `kind` is the receiver's
/// implementation, carried so the methods that answer a map **of the same
/// class** — `each` (which returns the receiver), `findAll`, `sort` — rebuild
/// one; the rest (`collectEntries`, `groupBy`, `countBy`) answer a
/// `LinkedHashMap` whatever they were called on, as Groovy's do.
fn dispatch_map_iteration(
    vm: &mut VM,
    entries: &[(String, Value)],
    kind: MapKind,
    method: &str,
    args: &[Value],
) -> Option<Result<Value, String>> {
    // Every operation here consumes a trailing closure except `sort`, which also
    // has a no-argument (sort-by-key) form.
    let clo = args.last().filter(|a| closure_meta(a).is_some());
    match method {
        // `reverseEach` walks the map's own order backwards and, like `each`,
        // answers the receiver.
        "each" | "eachWithIndex" | "reverseEach" => {
            let clo = clo?;
            let walked: Vec<&(String, Value)> = match method {
                "reverseEach" => entries.iter().rev().collect(),
                _ => entries.iter().collect(),
            };
            for (i, (k, v)) in walked.into_iter().enumerate() {
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
            Some(Ok(gmap_kind(entries.to_vec(), kind)))
        }
        // `map.grep(filter)` has no `Map` overload, so Groovy reaches the
        // `Object` one: it iterates the map as its entry set and builds an
        // `ArrayList`. `[a:1, b:0].grep { it.value }` is therefore the **list**
        // `[a=1]`, not the map `[a:1]` that `findAll` answers — and the closure
        // is called with the whole `Map.Entry`, never with `(key, value)`, since
        // that spread is `callClosureForMapEntry`'s and only the methods with a
        // real `Map` overload go through it.
        "grep" => {
            let listed: Vec<Value> = entries
                .iter()
                .map(|(k, v)| heap_push(HeapObj::Entry(k.clone(), v.clone())))
                .collect();
            dispatch_iteration(vm, &listed, method, args)
        }
        // `map.collectMany { k, v -> … }` *does* have a `Map` overload, so it
        // takes the `(key, value)` spread and flattens each result into one
        // list: `[a:1, b:2].collectMany { k, v -> [k, v] }` is `[a, 1, b, 2]`.
        // (`map.entrySet().collectMany { k, v -> … }` is a
        // `MissingMethodException` in Groovy for exactly the opposite reason —
        // an entry set is a plain collection, so the two-parameter closure never
        // matches its single `Map.Entry` argument.)
        "collectMany" => {
            let clo = clo?;
            let mut out: Vec<Value> = Vec::new();
            for (k, v) in entries {
                let r = match invoke_closure(vm, clo, &entry_args(clo, k, v)) {
                    Ok(r) => r,
                    Err(e) => return Some(Err(e)),
                };
                if pending_exc() {
                    return Some(Ok(Value::Undef));
                }
                out.extend(iteration_elements(&r));
            }
            Some(Ok(Value::array(out)))
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
            // `collectEntries` accumulates into `createSimilarMap`'s result, so
            // `new TreeMap(…).collectEntries { … }` is a `TreeMap` and re-sorts
            // whatever keys the closure produced, while a `HashMap` receiver
            // answers a `LinkedHashMap`.
            Some(Ok(gmap_kind(out, similar_map_kind(kind))))
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
                "findAll" => gmap_kind(kept, similar_map_kind(kind)),
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
            Some(Ok(gmap(
                groups.into_iter().map(|(k, v)| (k, gmap(v))).collect(),
            )))
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
            // A two-parameter closure sorts a *map* the way it sorts a list: as
            // a comparator over the entries (`sort { a, b -> a.value <=> b.value }`),
            // not as a `(key, value)` pair the way `each`/`collect` spread one.
            // Treating it as a key extractor called the closure with one entry
            // and a null second argument.
            let order = match clo {
                Some(c) if closure_meta(c).map(|m| m.params).unwrap_or(1) >= 2 => {
                    OrderBy::Comparator(c)
                }
                Some(c) => OrderBy::Key(c),
                None => OrderBy::Natural,
            };
            // With no closure Groovy orders by key; the entry handles themselves
            // have no natural order, so sort the keys and rebuild.
            if clo.is_none() {
                let mut sorted = entries.to_vec();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                // `sort(Map)` answers a `TreeMap` whatever it was called on —
                // sorting by key *is* building one — while the closure form
                // below answers a `LinkedHashMap` holding the closure's order,
                // which a `TreeMap` could not represent.
                return Some(Ok(gmap_kind(sorted, MapKind::Tree)));
            }
            Some(
                sort_values(vm, &handles, &order)
                    .map(|sorted| gmap(sorted.iter().filter_map(as_entry).collect())),
            )
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
            Some(Ok(gmap(
                counts
                    .into_iter()
                    .map(|(k, n)| (k, Value::int(n)))
                    .collect(),
            )))
        }
        // `map.withDefault { key -> … }` — a copy whose missing-key reads run the
        // closure and *store* its result, the way `groovy.lang.MapWithDefault`
        // does. The closure is remembered in `MAP_DEFAULTS` against the copy.
        "withDefault" => {
            let clo = clo?;
            let copy = gmap(entries.to_vec());
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

/// `Character.isWhitespace` — which `String.strip()` and `isBlank()` use, and
/// which is **not** Unicode `White_Space`.
///
/// Java excludes the three non-breaking space characters (U+00A0, U+2007,
/// U+202F) that the Unicode property includes, and includes the file/group/
/// record/unit separators (U+001C..U+001F) that it does not. Rust's
/// `char::is_whitespace` is the Unicode property, so `"\u{a0}x".strip()` would
/// lose the NBSP that Java keeps.
fn java_is_whitespace(c: char) -> bool {
    match c {
        '\u{a0}' | '\u{2007}' | '\u{202f}' => false,
        '\u{1c}'..='\u{1f}' => true,
        _ => c.is_whitespace(),
    }
}

/// Java's `String.length()`: the count of UTF-16 **code units**, so an astral
/// character counts as its surrogate pair and `"a😀b".length()` is 4.
///
/// Rust's `chars().count()` counts code *points* and answers 3 for the same
/// string. The two names look interchangeable and type-check either way, which
/// is why every index this file hands to or takes from a Groovy script goes
/// through this function rather than `chars()`. [`string_hash`] already folded
/// over `encode_utf16` for exactly this reason; the index family did not, and
/// the two contradicted each other on any string outside the BMP.
fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `s` sliced by UTF-16 code units, `[from, to)`, decoded back to a `String`.
///
/// A slice that lands inside a surrogate pair decodes that half to the
/// replacement character: groovyrs has no `java.lang.Character` type that could
/// hold a lone surrogate, and a Rust `char` cannot represent one. Java keeps the
/// unpaired half; this is the one place the UTF-16 index model is lossy, and it
/// is only reachable by slicing an astral character in two.
fn utf16_slice(s: &str, from: usize, to: usize) -> String {
    let units: Vec<u16> = s.encode_utf16().collect();
    let lo = from.min(units.len());
    let hi = to.clamp(lo, units.len());
    String::from_utf16_lossy(&units[lo..hi])
}

/// The UTF-16 index of the byte offset `b` in `s`.
fn utf16_index_of_byte(s: &str, b: usize) -> usize {
    s[..b].encode_utf16().count()
}

/// The byte offset at UTF-16 index `i`, or `None` past the end. An index inside
/// a surrogate pair answers the offset of the character containing it, so a
/// search bounded by it still starts at a character boundary.
fn byte_at_utf16_index(s: &str, i: usize) -> Option<usize> {
    if i == 0 {
        return Some(0);
    }
    let mut units = 0usize;
    for (b, c) in s.char_indices() {
        if units >= i {
            return Some(b);
        }
        units += c.len_utf16();
    }
    (units >= i).then_some(s.len())
}

/// The element/character count of a Groovy value: characters for a `String`,
/// element count for a list, entry count for a map.
fn value_size(v: &Value) -> i64 {
    match v {
        Value::Str(s) => utf16_len(s) as i64,
        Value::Array(a) => a.len() as i64,
        Value::Hash(h) => h.len() as i64,
        _ => omap_len(v).map(|n| n as i64).unwrap_or(0),
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
    // `inspect()` is `InvokerHelper.inspect`, which is the *verbose* rendering
    // [`inspect_value`] already produces for the power-assert layout: a String
    // is quoted and a collection's elements are rendered the same way
    // recursively, so `[1, 'a'].inspect()` is `[1, 'a']` where `toString()` is
    // `[1, a]`. It answers on every value — a `Range` overrides it below,
    // because `groovy.lang.Range` declares its own `inspect` and answers
    // `1..5` / `'a'..'c'` rather than the elements it enumerates.
    if method == "inspect" && args.is_empty() && as_range(recv).is_none() {
        return Value::str(inspect_value(recv));
    }
    // `toListString()` / `toMapString()` are *not* `inspect`: they are
    // `FormatHelper.toString(coll, false)`, the same rendering `println` uses,
    // under a receiver-specific name — `[1, 'a'].toListString()` is `[1, a]`
    // where `inspect()` is `[1, 'a']`.
    if args.is_empty()
        && ((method == "toListString" && (is_list(recv) || as_set(recv).is_some()))
            || (method == "toMapString" && as_omap(recv).is_some()))
    {
        return Value::str(groovy_str(recv));
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

        // `value.asType(Type)` is the method spelling of `value as Type`, so it
        // runs the one coercion rather than a second, drifting copy of it. The
        // argument is a `java.lang.Class`; `as` takes the name.
        (_, "asType") if args.len() == 1 => {
            let ty = as_class_ref(&args[0]).unwrap_or_else(|| groovy_str(&args[0]));
            vm.stack.push(recv.clone());
            vm.stack.push(Value::str(ty));
            b_cast(vm, 0)
        }

        // ── String ──
        (Value::Str(s), "length") => Value::int(utf16_len(s) as i64),
        (Value::Str(s), "toUpperCase") => Value::str(s.to_uppercase()),
        (Value::Str(s), "toLowerCase") => Value::str(s.to_lowercase()),
        // `String.trim()` strips code points `<= U+0020` — *not* Unicode
        // whitespace. Rust's `str::trim` strips the `White_Space` property, and
        // the two disagree in both directions: it strips NBSP (U+00A0), which
        // Java keeps, and keeps NUL (U+0000), which Java strips. `strip()` is
        // the Java method whose rule really is Unicode whitespace.
        (Value::Str(s), "trim") => Value::str(s.trim_matches(|c| c <= ' ').to_string()),
        (Value::Str(s), "strip") => Value::str(s.trim_matches(java_is_whitespace).to_string()),
        (Value::Str(s), "stripLeading") => {
            Value::str(s.trim_start_matches(java_is_whitespace).to_string())
        }
        (Value::Str(s), "stripTrailing") => {
            Value::str(s.trim_end_matches(java_is_whitespace).to_string())
        }
        (Value::Str(s), "isBlank") => Value::bool(s.chars().all(java_is_whitespace)),
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
        // `String.toBigInteger()` is `new BigInteger(text.trim())`, which — unlike
        // `toBigDecimal` — accepts *only* an integer: `"1.5".toBigInteger()`
        // raises rather than truncating. Its two failure messages are
        // `BigInteger`'s own, and the empty string has its own wording rather
        // than the `For input string:` form. Verified against Apache Groovy
        // 5.0.8 / JDK 21: `"".toBigInteger()` is `Zero length BigInteger` and
        // `"1.5".toBigInteger()` is `For input string: "1.5"`.
        (Value::Str(s), "toBigInteger") => {
            let t = s.trim();
            if t.is_empty() {
                raise(vm, "NumberFormatException", "Zero length BigInteger");
                return Value::Undef;
            }
            match decimal::parse_java(t).ok().filter(|d| d.is_integer()) {
                Some(d) => bigint_value(d),
                None => raise_number_format(vm, t),
            }
        }
        // Index queries answer a UTF-16 index, matching `String.length()`.
        //
        // Three Java behaviours the previous one-liner dropped: the `fromIndex`
        // overload (`"abc".indexOf("b", 2)` is `-1`, not `1`); the `int ch`
        // overload, where the argument is a code point rather than text
        // (`"abc".indexOf(97)` is `0`, not the index of the literal `"97"`); and
        // that `lastIndexOf`'s `fromIndex` is an upper bound, not a lower one.
        (Value::Str(s), "indexOf" | "lastIndexOf") => {
            // `indexOf(int)` searches for the code point. Only an `Int`
            // argument selects it — a one-character `Str` is still text.
            let needle: String = match args.first() {
                Some(Value::Int(n)) => u32::try_from(*n)
                    .ok()
                    .and_then(char::from_u32)
                    .map(String::from)
                    .unwrap_or_default(),
                other => other.map(groovy_str).unwrap_or_default(),
            };
            let len = utf16_len(s);
            let from = args.get(1).and_then(as_i64);
            let byte_pos = if method == "indexOf" {
                // A negative `fromIndex` is treated as 0; one past the end
                // finds only the empty needle at the end.
                let start = from.unwrap_or(0).clamp(0, len as i64) as usize;
                byte_at_utf16_index(s, start).and_then(|b| s[b..].find(&needle).map(|p| b + p))
            } else {
                // `lastIndexOf(str, fromIndex)` searches at or before
                // `fromIndex`, so the window ends `needle.len()` units past it.
                let end = match from {
                    Some(f) if f < 0 => return Value::int(-1),
                    Some(f) => (f as usize).saturating_add(utf16_len(&needle)).min(len),
                    None => len,
                };
                byte_at_utf16_index(s, end).and_then(|b| s[..b].rfind(&needle))
            };
            Value::int(
                byte_pos
                    .map(|b| utf16_index_of_byte(s, b) as i64)
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
        // `String.compareTo` answers the *difference* of the first differing
        // UTF-16 code units, or the length difference — not a normalised sign.
        // `"a".compareTo("c")` is `-2`. Rust's `str::cmp` answers an `Ordering`,
        // and normalising it to `-1/0/1` (which is what `<=>` wants) is the
        // wrong answer for the method a script calls directly.
        (Value::Str(s), "compareTo") => {
            let other = args.first().map(groovy_str).unwrap_or_default();
            let (a, b): (Vec<u16>, Vec<u16>) =
                (s.encode_utf16().collect(), other.encode_utf16().collect());
            let diff = a
                .iter()
                .zip(b.iter())
                .find(|(x, y)| x != y)
                .map(|(x, y)| i64::from(*x) - i64::from(*y))
                .unwrap_or(a.len() as i64 - b.len() as i64);
            Value::int(diff)
        }
        (Value::Str(s), "charAt") => {
            let i = args.first().and_then(as_i64).unwrap_or(0);
            let len = utf16_len(s);
            let in_range = (0..len as i64).contains(&i);
            if in_range {
                Value::str(utf16_slice(s, i as usize, i as usize + 1))
            } else {
                raise(
                    vm,
                    "StringIndexOutOfBoundsException",
                    &format!("Index {i} out of bounds for length {len}"),
                );
                Value::Undef
            }
        }
        // `substring` indexes UTF-16 code units, and its bounds check is
        // `checkBoundsBeginEnd`: `begin < 0 || end > length || begin > end`,
        // reported as `Range [begin, end) out of bounds for length n` — with the
        // raw indices, so a negative `begin` shows as written.
        //
        // The message used to read `begin {from}, end {to}, length {n}`, which
        // is the JDK *17* wording. JDK 19 rewrote it, and the harness gate added
        // for `Double.toString` does not see a string frozen inside the
        // implementation. The sibling subscript path (`"ab"[9]`) already emitted
        // the current form, so the two contradicted each other.
        (Value::Str(s), "substring") => {
            let len = utf16_len(s) as i64;
            let from = args.first().and_then(as_i64).unwrap_or(0);
            let to = args.get(1).and_then(as_i64).unwrap_or(len);
            if from < 0 || to > len || from > to {
                raise(
                    vm,
                    "StringIndexOutOfBoundsException",
                    &format!("Range [{from}, {to}) out of bounds for length {len}"),
                );
                return Value::Undef;
            }
            Value::str(utf16_slice(s, from as usize, to as usize))
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
        // Groovy's no-argument `split()` is not `split("")`: it is
        // `StringTokenizer`'s whitespace tokenizing, which drops every empty
        // field including the leading and trailing ones, so `" a b ".split()`
        // is `[a, b]` and `"".split()` is `[]`.
        (Value::Str(s), "split") if args.is_empty() => Value::array(
            s.split_whitespace()
                .map(|w| Value::str(w.to_string()))
                .collect(),
        ),
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
                // The no-argument form is `StringTokenizer`'s default delimiter
                // set — exactly `" \t\n\r\f"`. Rust's `split_whitespace` splits
                // on the Unicode `White_Space` property, which additionally
                // breaks on NBSP and the vertical tab.
                None => s
                    .split(|c| " \t\n\r\u{c}".contains(c))
                    .filter(|p| !p.is_empty())
                    .map(|p| Value::str(p.to_string()))
                    .collect(),
            };
            Value::array(parts)
        }
        (Value::Str(s), "toList" | "toCharArray" | "chars") => {
            Value::array(s.chars().map(|c| Value::str(c.to_string())).collect())
        }
        // `s.bytes` / `s.getBytes()` — the UTF-8 encoding as *signed* bytes, so
        // a non-ASCII character's units print negative the way a Java `byte[]`
        // does. Modeled as a list, like every other array here.
        (Value::Str(s), "getBytes") => Value::array(
            s.as_bytes()
                .iter()
                .map(|b| Value::int(*b as i8 as i64))
                .collect(),
        ),
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
        // `readLines()` is `StringGroovyMethods.readLines` — the same line split
        // `eachLine` and `stripIndent` already use here, answered as a list.
        (Value::Str(s), "readLines") => {
            Value::array(read_lines(s).into_iter().map(Value::str).collect())
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
                // `Double.valueOf`'s grammar, not Rust's: `"inf"`, `"infinity"`
                // and `"+nan"` all parse as an `f64` and none of them is a Java
                // double literal. [`parse_java_double`] is the same parser
                // `toDouble()` uses, so the predicate and the conversion agree.
                "isDouble" | "isFloat" => parse_java_double(t).is_some(),
                // `new BigInteger(t)` takes only an integer literal; the rest
                // ask `new BigDecimal(t)`, which is Java's decimal grammar.
                "isBigInteger" => {
                    let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
                    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
                }
                _ => crate::decimal::parse_java(t).is_ok(),
            })
        }
        // Groovy's `capitalize` calls `Character.toUpperCase(char)`, a
        // *single-character* mapping that leaves anything with no single-char
        // uppercase alone — `'ß'` stays `'ß'`. Rust's `char::to_uppercase` is the
        // full Unicode mapping and expands it to `SS`.
        (Value::Str(s), "capitalize" | "uncapitalize") => {
            let mut cs = s.chars();
            let one = |c: char, up: bool| {
                let mapped: Vec<char> = if up {
                    c.to_uppercase().collect()
                } else {
                    c.to_lowercase().collect()
                };
                match mapped[..] {
                    [m] => m,
                    _ => c,
                }
            };
            match cs.next() {
                Some(c) => {
                    let head = one(c, method == "capitalize");
                    Value::str(head.to_string() + cs.as_str())
                }
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
        // `list.subList(from, to)` on a **transient** element vector — an
        // internal sequence that never became a handle, so there is no backing
        // list for a window to point at. A `subList` a script can name is
        // answered by [`make_sublist`] on the handle, above this dispatch, and is
        // a live `java.util.ArrayList$SubList`; this arm is the copy fallback for
        // the transient form, and repeats the same JDK bounds order.
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
            let mut r = a.to_vec();
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
            Value::array(a.to_vec())
        }
        // `toSet()` is Groovy's `new HashSet<>(self.size())` + `addAll`, so it
        // asks for a table sized to the *element count* — a smaller one than
        // `new HashSet(collection)` asks for, and that difference is visible as
        // a different iteration order for the same elements.
        (Value::Array(a), "toSet") => make_set(
            a.to_vec(),
            SetKind::Hash {
                req: table_size_for(a.len()),
            },
        ),
        // `toUnique` answers a de-duplicated **List**, not a set.
        (Value::Array(a), "toUnique") => {
            let mut out: Vec<Value> = Vec::new();
            for v in a.iter() {
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
            Value::array(std::iter::repeat(a.to_vec()).take(n).flatten().collect())
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
            for e in a.iter() {
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
            gmap(
                a.iter()
                    .enumerate()
                    .map(|(i, v)| ((base + i as i64).to_string(), v.clone()))
                    .collect(),
            )
        }
        // `list.iterator()` — a live cursor over the elements. Stateful, so it
        // rides a handle (see `HeapObj::Iter`).
        // `list.iterator()` and `list.listIterator()` are two *different*
        // `ArrayList` inner classes, and `getClass()` tells them apart:
        // `ArrayList$Itr` for the plain cursor, `ArrayList$ListItr` for the
        // bidirectional one. Only the forward half is modeled, so they share an
        // implementation, but they must not share a name.
        (Value::Array(a), "iterator" | "listIterator") => heap_push(HeapObj::Iter {
            class: if method == "listIterator" {
                "java.util.ArrayList$ListItr"
            } else {
                "java.util.ArrayList$Itr"
            },
            items: a.to_vec(),
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
                    let mut next = a.to_vec();
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
                // The two are not symmetric. `pop` explains itself; `removeLast`
                // throws a bare `new NoSuchElementException()`, whose message is
                // `null`. Interpolating the method name into one sentence read
                // as the tidier design and invented a message for `removeLast`
                // that Groovy has never printed.
                raise_opt(
                    vm,
                    "NoSuchElementException",
                    (method == "pop").then_some("Cannot pop() an empty List"),
                );
                return Value::Undef;
            }
            let mut next = a.to_vec();
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
            let mut next = a.to_vec();
            // How many elements the call actually added — what `addAll` answers.
            let mut added = 1usize;
            // `add(index, element)` and `addAll(index, collection)` insert at the
            // index; every other form appends.
            // The two-argument (positional) forms range-check their index the
            // way `ArrayList.rangeCheckForAdd` does — and its message is
            // `Index: 9, Size: 3`, a different spelling from the
            // `Index 9 out of bounds for length 3` that `get`/`set` carry.
            // Insertion *at* the end is legal, so the bound is `0..=len`, one
            // wider than a read's. Clamping instead (`max(0).min(len)`) silently
            // appended for `add(9, x)` and prepended for `add(-1, x)`.
            let insert_at = |vm: &mut VM, len: usize| -> Option<usize> {
                let i = as_i64(&args[0]).unwrap_or(0);
                if i < 0 || i > len as i64 {
                    raise(
                        vm,
                        "IndexOutOfBoundsException",
                        &format!("Index: {i}, Size: {len}"),
                    );
                    return None;
                }
                Some(i as usize)
            };
            match (method, args.len()) {
                ("add", 2) => {
                    let Some(i) = insert_at(vm, next.len()) else {
                        return Value::Undef;
                    };
                    next.insert(i, args[1].clone());
                }
                ("addAll", 2) => {
                    let Some(at) = insert_at(vm, next.len()) else {
                        return Value::Undef;
                    };
                    let extra = iteration_elements(&args[1]);
                    added = extra.len();
                    next.splice(at..at, extra);
                }
                ("addAll", _) => {
                    let extra = args.first().map(iteration_elements).unwrap_or_default();
                    added = extra.len();
                    next.extend(extra);
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
                // `Collection.addAll` answers whether the collection *changed*,
                // so an empty argument is `false` — `add`/`push` always add their
                // one element and answer `true`. Verified against Groovy 5.0.8:
                // `[1, 2].addAll([])` is `false` and `[1, 2].addAll([3])` is `true`.
                "addAll" => Value::bool(added > 0),
                _ => Value::bool(true),
            };
            set_mutated(Value::array(next));
            answer
        }
        (Value::Array(a), "remove" | "removeAt") => {
            let i = args.first().and_then(as_i64).unwrap_or(0);
            match usize::try_from(i).ok().filter(|u| *u < a.len()) {
                Some(u) => {
                    let mut next = a.to_vec();
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
                    let mut next = a.to_vec();
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
        // `list.putAt(i, v)` is `list[i] = v` spelled out — same negative-index
        // and grow-past-the-end rules, shared with the subscript through
        // [`list_put`]. Unlike `set`, whose `List.set` contract answers the
        // element it displaced, the GDK's `putAt` is `void`, so it answers null.
        (Value::Array(a), "putAt") if args.len() == 2 => {
            match list_put(a.to_vec(), &args[0], args[1].clone()) {
                Ok(next) => {
                    set_mutated(Value::array(next));
                    Value::Undef
                }
                Err((i, len)) => raise_negative_index(vm, i, len),
            }
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
        // `intdiv` is defined on the integral types only. A decimal on *either*
        // side takes the whole call out of `NumberMath`'s integer path, and the
        // message names the RECEIVER whichever side was the decimal:
        // `7.intdiv(2.0)` reports `java.lang.Integer with value: 7`.
        (_, "intdiv") if !args.is_empty() && (!is_integral(recv) || !is_integral(&args[0])) => {
            raise(
                vm,
                "UnsupportedOperationException",
                &format!(
                    "Cannot use intdiv() on this number type: {} with value: {}",
                    java_class_name(recv),
                    groovy_str(recv)
                ),
            );
            Value::Undef
        }
        (Value::Int(n), "intdiv") => match args.first().and_then(as_i64) {
            Some(0) | None => {
                // Three different wordings reach an `ArithmeticException` for a
                // zero divisor, and which one depends on the *type* that did the
                // dividing, not on the operator. An `Integer`/`Long` `intdiv`
                // divides natively, so the message is the JVM's own `/ by zero`
                // — `Division by zero` is `BigDecimal.divide`'s, which `1 / 0`
                // reaches because `/` promotes, and `BigInteger divide by zero`
                // is a third. Verified on Apache Groovy 5.0.8 / JDK 21:
                // `1.intdiv(0)` and `1L.intdiv(0)` give `/ by zero`,
                // `1G.intdiv(0G)` gives `BigInteger divide by zero`, and
                // `1G / 0G` gives `Division by zero`.
                raise(vm, "ArithmeticException", "/ by zero");
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
        //
        // Which overloads exist depends on the receiver's own width, and both
        // widths are invisible in the values: `255.toString(16L)` and
        // `255L.toString(16)` arrive as the same two `Value::Int`s. The four
        // signatures are `Integer.toString(int)`, `Integer.toString(int, int)`,
        // `Long.toString(long)` and `Long.toString(long, int)`, so a `Long` is
        // admissible in exactly one place — the first argument of the `Long`
        // pair. Anywhere else it matches nothing and Groovy raises, where
        // groovyrs used to render base 10 and answer `16`. See [`GMETHOD_WIDE`]
        // for where the widths come from.
        (Value::Int(_), "toString") => {
            let widths = call_widths();
            let recv_is_long = widths & 1 != 0;
            let long_arg_at = |i: usize| widths & (1 << (i + 1)) != 0;
            let unmatched = args.len() > 2
                || (0..args.len()).any(|i| long_arg_at(i) && !(i == 0 && recv_is_long));
            let class = if recv_is_long { "Long" } else { "Integer" };
            let rendered = if unmatched {
                None
            } else {
                dispatch_static(vm, class, "toString", args)
            };
            match rendered {
                Some(v) => v,
                None => raise_missing_method_wide(vm, recv, method, args, widths),
            }
        }
        // The shift *methods* — `5.leftShift(2)` is what `5 << 2` desugars to,
        // and answers the same 20. The width the fill and the count mask use is
        // the receiver's Java type, which the values do not carry; the compiler
        // marks it on the call (see [`GMETHOD_WIDE`]), which is how
        // `(-1).rightShiftUnsigned(28)` is `15` while `(-1L)`'s of 60 is too.
        (Value::Int(n), "leftShift" | "rightShift" | "rightShiftUnsigned") => {
            match args.first().and_then(as_i64).filter(|_| args.len() == 1) {
                Some(b) => java_shift(method, call_widths() & 1 != 0, *n, b),
                None => {
                    raise_operator_operand(vm, method, recv, args.first().unwrap_or(&Value::Undef))
                }
            }
        }
        // The mask *methods* — `5.and(3)` is `5 & 3`. Unlike the shifts these
        // need no width: `&`, `|` and `^` of two sign-extended 32-bit values
        // give the same 64-bit pattern either way, and so does `~`.
        (Value::Int(n), "and" | "or" | "xor") => {
            match args.first().and_then(as_i64).filter(|_| args.len() == 1) {
                Some(b) => Value::int(match method {
                    "and" => n & b,
                    "or" => n | b,
                    _ => n ^ b,
                }),
                None => {
                    raise_operator_operand(vm, method, recv, args.first().unwrap_or(&Value::Undef))
                }
            }
        }
        (Value::Int(n), "bitwiseNegate") if args.is_empty() => Value::int(!*n),
        (Value::Int(n), "toLong" | "longValue") => Value::int(*n),
        // `intValue()` is Java's narrowing conversion, so `3000000000L.intValue()`
        // is `-1294967296`.
        (Value::Int(n), "toInteger" | "intValue") => Value::int(i64::from(*n as i32)),
        (Value::Int(n), "toDouble" | "doubleValue" | "toFloat" | "floatValue") => {
            Value::float(*n as f64)
        }
        (Value::Int(n), "toBigDecimal") => dec_value(BigDecimal::from(*n)),
        (Value::Int(n), "toBigInteger") => bigint_value(BigDecimal::from(*n)),
        // `Integer.equals(Object)` is `false` for anything that is not an
        // `Integer` of the same value. `as_i64` reads a `Boolean` as 0/1, which
        // made `1.equals(true)` answer `true`.
        (Value::Int(n), "equals") => {
            Value::bool(matches!(args.first(), Some(Value::Int(o)) if o == n))
        }
        // `compareTo` compares the numeric values, so a non-integral argument
        // has to be read as a `double` rather than dropped: `as_i64` answered
        // `None` for `2.5` and the `unwrap_or(0)` then compared `1` against `0`,
        // making `1.compareTo(2.5)` answer `1` where Groovy answers `-1`.
        (Value::Int(n), "compareTo") => {
            let arg = args.first().unwrap_or(&Value::Undef);
            match as_i64(arg) {
                Some(other) => Value::int((*n > other) as i64 - (*n < other) as i64),
                None => {
                    let (a, b) = (*n as f64, as_f64(arg));
                    Value::int((a > b) as i64 - (a < b) as i64)
                }
            }
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
        // `intValue()`/`toInteger()` narrow to 32 bits, `longValue()` to 64. Both
        // casts *saturate* in Java, so `(1e10).intValue()` is `Integer.MAX_VALUE`
        // — reading both through `as i64` answered `10000000000`.
        (Value::Float(f), "toInteger" | "intValue") => Value::int(java_double_to_int(*f)),
        (Value::Float(f), "toLong" | "longValue") => Value::int(*f as i64),
        (Value::Float(f), "toDouble" | "doubleValue" | "toFloat" | "floatValue") => {
            Value::float(*f)
        }
        // `Double.compareTo` is `Double.compare`, so NaN is greater than
        // everything and `-0.0` below `+0.0`.
        (Value::Float(f), "compareTo") => Value::int(
            match java_compare_f64(*f, as_f64(args.first().unwrap_or(&Value::Undef))) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            },
        ),
        // `Double.equals(Object)` compares `doubleToLongBits`, not `==`, so it
        // disagrees with `==` at exactly the two values IEEE treats specially:
        // `Double.NaN.equals(Double.NaN)` is **true** (every NaN payload folds to
        // one canonical pattern, the same rule [`double_hash`] applies, which is
        // what keeps `equals`/`hashCode` consistent) and `(0.0d).equals(-0.0d)`
        // is **false**. It is also typed: only another `Double` can be equal, so
        // `(1.5d).equals(1.5)` is false against the `BigDecimal` `1.5`, and
        // `(1.0d).equals(1)` is false against the `Integer`. All four verified
        // against Apache Groovy 5.0.8 on JDK 21.
        (Value::Float(f), "equals") => Value::bool(match args.first() {
            Some(Value::Float(o)) => {
                (f.is_nan() && o.is_nan()) || (!f.is_nan() && f.to_bits() == o.to_bits())
            }
            _ => false,
        }),
        (Value::Float(f), "isNaN") => Value::bool(f.is_nan()),
        (Value::Float(f), "isInfinite") => Value::bool(f.is_infinite()),

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
                // A `BigDecimal` is an (unscaled value, scale) pair, and these
                // three read it directly — `2.5.scale()` is `1`,
                // `2.5.precision()` is `2`, `2.5.unscaledValue()` is `25`.
                "scale" if args.is_empty() => Value::int(decimal::scale_of(&d)),
                "precision" if args.is_empty() => Value::int(decimal::precision_of(&d)),
                "unscaledValue" if args.is_empty() => bigint_value(decimal::unscaled_value(&d)),
                // `setScale(n)` rounds with `RoundingMode.UNNECESSARY`, so it
                // raises rather than dropping digits: `1.5.setScale(0)` is an
                // `ArithmeticException`, while `1.5.setScale(3)` pads to `1.500`.
                "setScale" if args.len() == 1 => match args[0].to_int() {
                    n if decimal::fits_at_scale(&d, n) => dec_value(decimal::round_half_up(&d, n)),
                    _ => {
                        raise(vm, "ArithmeticException", "Rounding necessary");
                        Value::Undef
                    }
                },
                // `setScale(n, RoundingMode.X)`. The mode arrives as its own
                // name (see [`static_field`]); an unknown one is a dispatch miss
                // rather than a silent HALF_UP.
                "setScale" if args.len() == 2 => {
                    let scale = args[0].to_int();
                    match decimal::with_scale(&d, scale, &groovy_str(&args[1])) {
                        Some(v) => dec_value(v),
                        None if decimal::rounding_mode_exists(&groovy_str(&args[1])) => {
                            raise(vm, "ArithmeticException", "Rounding necessary");
                            Value::Undef
                        }
                        None => raise_missing_method(vm, recv, method, args),
                    }
                }
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
                //
                // `intValue()` keeps the **low 32 bits** of the truncated value
                // — it does not saturate the way a `double`'s `(int)` cast does.
                // `(1e30G).intValue()` is `1073741824`, and reading it through
                // an `i64` that saturates at the *long* bounds answered `0`.
                "longValue" | "toLong" => Value::int(decimal::low_i64(&d)),
                "intValue" | "toInteger" => Value::int(i64::from(decimal::low_i32(&d))),
                // `compareTo` is scale-insensitive (`1.0G.compareTo(1.00G)` is
                // `0`) and answers a sign, not a difference.
                "compareTo" => {
                    let other = args.first().and_then(as_exact_dec);
                    match other {
                        Some(o) => Value::int(match decimal::cmp(&d, &o) {
                            std::cmp::Ordering::Less => -1,
                            std::cmp::Ordering::Equal => 0,
                            std::cmp::Ordering::Greater => 1,
                        }),
                        None => raise_missing_method(vm, recv, method, args),
                    }
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
                // The mask and shift *methods*: `7G.and(3G)` is `7G & 3G`, and
                // `1G.shiftLeft(3)` is Java's own name for `1G << 3`. All of
                // them go through the same arbitrary-precision paths the
                // operators do.
                "and" | "or" | "xor" => {
                    bit_op_values(vm, method, recv, args.first().unwrap_or(&Value::Undef))
                }
                "bitwiseNegate" | "not" if args.is_empty() => {
                    match decimal::bit_not(&d).filter(|_| as_bigint(recv).is_some()) {
                        Some(r) => bigint_value(r),
                        None => raise_operator_operand(vm, "bitwiseNegate", recv, recv),
                    }
                }
                "leftShift" | "rightShift" | "shiftLeft" | "shiftRight" => {
                    let left = matches!(method, "leftShift" | "shiftLeft");
                    match bigint_shift(left, recv, args.first().unwrap_or(&Value::Undef)) {
                        Some(v) => v,
                        None => raise_operator_operand(
                            vm,
                            if left { "leftShift" } else { "rightShift" },
                            recv,
                            args.first().unwrap_or(&Value::Undef),
                        ),
                    }
                }
                // Java's own arithmetic method names, which a script reaches
                // past the operators. `pow` is `BigInteger`/`BigDecimal.pow`;
                // Groovy's `**` is `power`, already above.
                "add" | "subtract" | "multiply" | "pow" => {
                    let Some(y) = args
                        .first()
                        .and_then(as_exact_dec)
                        .filter(|_| args.len() == 1)
                    else {
                        return raise_missing_method(vm, recv, method, args);
                    };
                    let big = as_bigint(recv).is_some() && is_integral(&args[0]);
                    let r = match method {
                        "add" => decimal::add(&d, &y),
                        "subtract" => decimal::sub(&d, &y),
                        "multiply" => decimal::mul(&d, &y),
                        _ => match args
                            .first()
                            .and_then(as_i64)
                            .and_then(|e| decimal::pow(&d, e))
                        {
                            Some(p) => p,
                            None => return raise_missing_method(vm, recv, method, args),
                        },
                    };
                    if big {
                        bigint_value(r)
                    } else {
                        dec_value(r)
                    }
                }
                // `divide`/`remainder`/`mod` are the *Java* methods, not the
                // Groovy operators, and the two disagree on the receiver's type:
                //
                // - a `BigInteger` divides toward zero, so `7G.divide(3G)` is
                //   `2` where `7G / 3G` is `2.3333333333`;
                // - a `BigDecimal` demands an **exact** quotient, so
                //   `1.0G.divide(3.0G)` raises `ArithmeticException` where
                //   `1.0G / 3.0G` is the ten-digit approximation
                //   [`decimal::divide`] produces for the operator.
                // - `mod` is `BigInteger`'s alone and is never negative:
                //   `(-7G).mod(3G)` is `2` while `(-7G).remainder(3G)` is `-1`.
                // `divide(divisor, scale, RoundingMode)` — the three-argument
                // form that *always* terminates, unlike the one-argument one
                // that raises on a non-terminating expansion.
                "divide" if args.len() == 3 && as_bigint(recv).is_none() => {
                    let Some(y) = args.first().and_then(as_exact_dec) else {
                        return raise_missing_method(vm, recv, method, args);
                    };
                    if y.is_zero() {
                        // The three-argument form's zero divisor reports the
                        // JVM's bare `/ by zero`, not the one-argument form's
                        // `BigDecimal divide by zero`. Verified on 5.1.0.
                        raise(vm, "ArithmeticException", "/ by zero");
                        return Value::Undef;
                    }
                    let scale = args[1].to_int();
                    let mode = groovy_str(&args[2]);
                    // Divide to a few digits past the target scale and round
                    // there: the quotient itself may not terminate.
                    let wide = decimal::divide_to_scale(&d, &y, scale + 4);
                    match decimal::with_scale(&wide, scale, &mode) {
                        Some(v) => dec_value(v),
                        None if decimal::rounding_mode_exists(&mode) => {
                            raise(vm, "ArithmeticException", "Rounding necessary");
                            Value::Undef
                        }
                        None => raise_missing_method(vm, recv, method, args),
                    }
                }
                "divide" | "remainder" | "mod" => {
                    let big = as_bigint(recv).is_some();
                    let Some(y) = args
                        .first()
                        .and_then(as_exact_dec)
                        .filter(|_| args.len() == 1 && (big || method != "mod"))
                    else {
                        return raise_missing_method(vm, recv, method, args);
                    };
                    if y.is_zero() {
                        let what = if big { "BigInteger" } else { "BigDecimal" };
                        raise(vm, "ArithmeticException", &format!("{what} divide by zero"));
                        return Value::Undef;
                    }
                    match (method, big) {
                        ("divide", true) => bigint_value(decimal::divide(&d, &y).unwrap_or(d)),
                        ("divide", false) => match decimal::exact_divide(&d, &y) {
                            Some(q) => dec_value(q),
                            None => {
                                raise(
                                    vm,
                                    "ArithmeticException",
                                    "Non-terminating decimal expansion; \
                                     no exact representable decimal result.",
                                );
                                Value::Undef
                            }
                        },
                        // `BigInteger.mod` takes the sign of the *modulus*,
                        // which is always positive here; `remainder` takes the
                        // dividend's.
                        ("mod", _) => {
                            let r = decimal::remainder(&d, &y).unwrap_or_else(|| d.clone());
                            let negative =
                                decimal::cmp(&r, &decimal::from_i64(0)) == std::cmp::Ordering::Less;
                            bigint_value(if negative {
                                decimal::add(&r, &decimal::abs(&y))
                            } else {
                                r
                            })
                        }
                        (_, big) => {
                            let r = decimal::remainder(&d, &y).unwrap_or_else(|| d.clone());
                            if big {
                                bigint_value(r)
                            } else {
                                dec_value(r)
                            }
                        }
                    }
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
        // The keyed operations first, because they need no copy of the map.
        // Falling through to the general arm below makes a single `put` or
        // `get` cost a clone (and, for a `HashMap`/`TreeMap`, a re-order) of
        // every entry — filling a map with 16 000 `put`s took 42 s that way.
        (_, "put" | "putAt") if args.len() == 2 && is_omap(recv) => {
            let key = groovy_str(&args[0]);
            let previous = omap_get(recv, &key).flatten();
            omap_set(recv, key, args[1].clone());
            // `put` answers the value it displaced; `putAt` (the `m[k] = v`
            // spelling) answers nothing.
            match method {
                "put" => previous.unwrap_or(Value::Undef),
                _ => Value::Undef,
            }
        }
        (_, "get" | "getAt") if args.len() == 1 && is_omap(recv) => {
            omap_get(recv, &groovy_str(&args[0]))
                .flatten()
                .unwrap_or(Value::Undef)
        }
        (_, "containsKey") if args.len() == 1 && is_omap(recv) => {
            Value::bool(omap_get(recv, &groovy_str(&args[0])).flatten().is_some())
        }
        (_, "isEmpty") if args.is_empty() && is_omap(recv) => {
            Value::bool(omap_len(recv) == Some(0))
        }
        _ if is_omap(recv) => {
            let entries = as_omap(recv).unwrap();
            let kind = omap_kind(recv).unwrap_or(MapKind::Linked);
            // `java.util.NavigableMap` is the `TreeMap`'s interface alone: on any
            // other map every one of these is a `MissingMethodException`, which
            // is why they are gated on the kind rather than offered to all maps.
            // `entries` is already in key order for a `Tree` map, so each of them
            // is a scan of a sorted sequence.
            if kind == MapKind::Tree {
                if let Some(v) = dispatch_navigable_map(vm, &entries, method, args) {
                    return v;
                }
            }
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
                // The entry-at-an-end four are the GDK's, defined on *every* map —
                // unlike `firstKey`/`lastKey`, which are `NavigableMap`'s and
                // reach a `TreeMap` alone. They answer null on an empty map where
                // the key pair raises, and the two `poll` spellings also remove
                // the entry they answer, mutating through the handle.
                "firstEntry" | "lastEntry" | "pollFirstEntry" | "pollLastEntry" => {
                    match entries.len() {
                        0 => Value::Undef,
                        n => {
                            let i = match method {
                                "firstEntry" | "pollFirstEntry" => 0,
                                _ => n - 1,
                            };
                            let (k, v) = &entries[i];
                            let taken = heap_push(HeapObj::Entry(k.clone(), v.clone()));
                            if method.starts_with("poll") {
                                let gone = k.clone();
                                omap_retain(recv, |x| x != gone);
                            }
                            taken
                        }
                    }
                }
                // `map.take(n)` / `drop(n)` — the first (or all but the first)
                // `n` entries in the map's own order, as a map of the same kind.
                "take" | "drop" => {
                    let n = args
                        .first()
                        .map(|v| v.to_int().max(0) as usize)
                        .unwrap_or(0);
                    let kept: Vec<(String, Value)> = match method {
                        "take" => entries.into_iter().take(n).collect(),
                        _ => entries.into_iter().skip(n).collect(),
                    };
                    gmap_kind(kept, similar_map_kind(kind))
                }
                // `map.toSorted()` orders by key like `sort()` but — unlike it —
                // answers a `LinkedHashMap` *holding* that order rather than a
                // `TreeMap`. The two spellings really do differ in class.
                "toSorted" if args.is_empty() => {
                    let mut sorted = entries;
                    sorted.sort_by(|a, b| utf16_cmp(&a.0, &b.0));
                    gmap(sorted)
                }
                // `map.subMap(keys)` — the entries for the listed keys, in the
                // ***argument's*** order; a key the map does not hold is dropped.
                // Both the collection and the varargs spelling are accepted.
                //
                // The GDK walks the requested keys and puts each into a fresh
                // `LinkedHashMap`, so the result follows them rather than the
                // receiver: `[b: 2, a: 1].subMap('a', 'b')` is `[a:1, b:2]`.
                // Filtering the receiver's entries instead reads identically
                // whenever the two orders agree, which is why a sorted receiver
                // hid it.
                "subMap" => {
                    let wanted: Vec<String> = match args {
                        [one] if is_list(one) || as_range(one).is_some() => {
                            iteration_elements(one).iter().map(groovy_str).collect()
                        }
                        rest => rest.iter().map(groovy_str).collect(),
                    };
                    gmap(
                        wanted
                            .into_iter()
                            .filter_map(|w| {
                                entries
                                    .iter()
                                    .find(|(k, _)| *k == w)
                                    .map(|(k, v)| (k.clone(), v.clone()))
                            })
                            .collect(),
                    )
                }
                // `map.spread()` is a shallow copy of the map. A copy is the same
                // implementation as its source, so the kind rides along —
                // `new TreeMap(…).clone()` is a `TreeMap` and still sorts.
                "spread" | "clone" | "asImmutable" | "asSynchronized" => gmap_kind(entries, kind),
                // `map - other` / `map.minus(other)` drops the entries the other
                // map holds *identically* (same key and same value).
                "minus" => {
                    let drop: Vec<(String, Value)> =
                        args.first().map(entry_pairs).unwrap_or_default();
                    gmap_kind(
                        entries
                            .into_iter()
                            .filter(|(k, v)| {
                                !drop.iter().any(|(dk, dv)| dk == k && values_equal(dv, v))
                            })
                            .collect(),
                        similar_map_kind(kind),
                    )
                }
                // …and `intersect` keeps exactly those.
                "intersect" => {
                    let keep: Vec<(String, Value)> =
                        args.first().map(entry_pairs).unwrap_or_default();
                    gmap_kind(
                        entries
                            .into_iter()
                            .filter(|(k, v)| {
                                keep.iter().any(|(dk, dv)| dk == k && values_equal(dv, v))
                            })
                            .collect(),
                        similar_map_kind(kind),
                    )
                }
                // A map iterates over its entries.
                "iterator" => heap_push(HeapObj::Iter {
                    class: match kind {
                        MapKind::Linked => "java.util.LinkedHashMap$LinkedEntryIterator",
                        MapKind::Hash { .. } => "java.util.HashMap$EntryIterator",
                        MapKind::Tree => "java.util.TreeMap$EntryIterator",
                    },
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
                // `m.putAt(k, v)` is `m[k] = v` spelled out. It is `Map.put`'s
                // `void` sibling, so it answers null where `put` answers the
                // value it displaced.
                "putAt" => {
                    let k = args.first().map(groovy_str).unwrap_or_default();
                    omap_set(recv, k, args.get(1).cloned().unwrap_or(Value::Undef));
                    Value::Undef
                }
                "putAll" => {
                    for (k, v) in args.first().map(entry_pairs).unwrap_or_default() {
                        omap_set(recv, k, v);
                    }
                    Value::Undef
                }
                // `map << other` is `putAll` that answers the receiver, so it
                // chains. Only a `Map` or a single `Map.Entry` is accepted —
                // `[a:1] << ['b', 2]` is a `MissingMethodException` in Groovy,
                // and admitting the pair here would silently accept it.
                "leftShift"
                    if args.len() == 1
                        && (as_omap(&args[0]).is_some() || as_entry(&args[0]).is_some()) =>
                {
                    for (k, v) in entry_pairs(&args[0]) {
                        omap_set(recv, k, v);
                    }
                    recv.clone()
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
    // `**` is `Number.power(Number)`, so a non-`Number` on either side has no
    // overload at all. Asked before the exponent is read as a number: `as_f64`
    // answers `NaN` for a `String`, so `2 ** "x"` used to *print* `NaN` where
    // Groovy raises, and a `String` base reached a hand-written message
    // (`No signature of method: power() for x`) in neither Groovy's shape nor
    // its wording.
    if !is_number(&base) || !is_number(&exp) {
        return raise_operator_operand(vm, "power", &base, &exp);
    }
    let e = match as_i64(&exp) {
        Some(e) => e,
        // A fractional exponent has no exact form, so Groovy runs it as a double.
        None => return Value::float(as_f64(&base).powf(as_f64(&exp))),
    };
    // A `BigInteger` base keeps its type — `BigInteger.pow` answers a
    // `BigInteger`, so `2G ** 70` and `(2G).power(10)` are both `BigInteger`
    // while `1.5G ** 2` is a `BigDecimal`. Asked before `as_dec`, which answers
    // for a `BigInteger` too (it is a scale-0 `BigDecimal`) and would widen it.
    // A negative exponent leaves the integers in either case: `decimal::pow`
    // declines it and Groovy answers the `Double` `0.5` for `2G ** -1`.
    // Verified against Apache Groovy 5.0.8.
    if let Some(d) = as_bigint(&base) {
        return match decimal::pow(&d, e) {
            Some(r) => bigint_value(r),
            None => Value::float(as_f64(&base).powf(e as f64)),
        };
    }
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
    // `map << other` is `Map.leftShift` — a `putAll` that answers the receiver,
    // so it chains. Same reason as the list above: one implementation, reached
    // through the ordinary dispatch.
    if as_omap(&lhs).is_some() {
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
            let mut next = a.to_vec();
            next.push(rhs);
            let out = Value::array(next);
            set_mutated(out.clone());
            out
        }
        Value::Str(s) => Value::str(format!("{s}{}", groovy_str(&rhs))),
        _ => match bigint_shift(true, &lhs, &rhs) {
            Some(v) => v,
            None => match (as_i64(&lhs), as_i64(&rhs)) {
                (Some(a), Some(b)) => java_shift("leftShift", wide, a, b),
                _ => raise_operator_operand(vm, "leftShift", &lhs, &rhs),
            },
        },
    }
}

/// `GBITOP`: `&`/`|`/`^` on operands the native ops cannot read.
///
/// Two integers take Java's own answer, which is width-independent for the mask
/// operators — `&`, `|` and `^` of two sign-extended 32-bit values give the same
/// 64-bit pattern either way. A `BigInteger` operand takes the arbitrary-
/// precision two's-complement answer ([`decimal::bit_op`]), so `(-1G) & 255G` is
/// `255`. Anything else — a `BigDecimal`, a String, a null — raises what Groovy
/// raises for that operand shape ([`raise_operator_operand`]).
fn b_bitop(vm: &mut VM, _argc: u8) -> Value {
    let op = vm
        .stack
        .pop()
        .unwrap_or(Value::Undef)
        .as_str_cow()
        .into_owned();
    let rhs = vm.stack.pop().unwrap_or(Value::Undef);
    let lhs = vm.stack.pop().unwrap_or(Value::Undef);
    bit_op_values(vm, &op, &lhs, &rhs)
}

/// The body of [`b_bitop`], shared with the `a.and(b)` method spellings.
fn bit_op_values(vm: &mut VM, op: &str, lhs: &Value, rhs: &Value) -> Value {
    if let (Some(a), Some(b)) = (plain_int(lhs), plain_int(rhs)) {
        return Value::int(match op {
            "and" => a & b,
            "or" => a | b,
            _ => a ^ b,
        });
    }
    // A `BigInteger` on either side widens the whole operation; an `Integer`
    // partner converts exactly.
    if as_bigint(lhs).is_some() || as_bigint(rhs).is_some() {
        if let (Some(a), Some(b)) = (as_exact_dec(lhs), as_exact_dec(rhs)) {
            if let Some(r) = decimal::bit_op(op, &a, &b) {
                return bigint_value(r);
            }
        }
    }
    raise_operator_operand(vm, op, lhs, rhs)
}

/// `GBITNOT`: `~x` where `x` is not a plain integer — a `BigInteger`'s
/// two's-complement `not`, which is `-x - 1` at any width.
fn b_bitnot(vm: &mut VM, _argc: u8) -> Value {
    let v = vm.stack.pop().unwrap_or(Value::Undef);
    if let Some(n) = plain_int(&v) {
        return Value::int(!n);
    }
    if as_bigint(&v).is_some() {
        if let Some(r) = as_exact_dec(&v).and_then(|d| decimal::bit_not(&d)) {
            return bigint_value(r);
        }
    }
    // Groovy's `~` desugars to `bitwiseNegate()`, and reports the operand under
    // that name. It is unary, so the "argument" it blames is the operand itself.
    raise_operator_operand(vm, "bitwiseNegate", &v, &v)
}

/// The `i64` behind a value that really is an `Integer`/`Long` — *not* what
/// [`as_i64`] answers, which also reads a `Boolean` and a decimal handle. The
/// bitwise builtins need the narrow question: a decimal must take the
/// arbitrary-precision path rather than a lossy 64-bit one.
fn plain_int(v: &Value) -> Option<i64> {
    match v {
        Value::Int(n) => Some(*n),
        _ => None,
    }
}

/// Java's three integer shifts at the left operand's own width: 32 bits for an
/// `Integer`, 64 for a `Long`, with the count masked to that width's bit index.
/// So `1 << 32` is `1` again, `-1 >>> 28` is `15`, and `-1L >>> 60` is `15` too.
///
/// Shared by the operator builtins ([`b_shl`], [`b_shr`], [`b_ushr`]) and by the
/// method spellings `a.leftShift(b)` / `rightShift` / `rightShiftUnsigned`,
/// which are the same operation under the name the operator desugars to.
fn java_shift(op: &str, wide: bool, a: i64, b: i64) -> Value {
    let count = b as u32 & if wide { 63 } else { 31 };
    match (op, wide) {
        ("leftShift", true) => Value::int(a.wrapping_shl(count)),
        ("leftShift", false) => Value::int(i64::from((a as i32).wrapping_shl(count))),
        ("rightShift", true) => Value::int(a >> count),
        ("rightShift", false) => Value::int(i64::from((a as i32) >> count)),
        (_, true) => Value::int(((a as u64) >> count) as i64),
        // The result of an `int >>> n` is an `int`, so it carries the sign of its
        // low 32 bits: `Integer.MIN_VALUE >>> 0` is `Integer.MIN_VALUE`, not
        // `2147483648`.
        (_, false) => Value::int(i64::from(((a as i32 as u32) >> count) as i32)),
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
        // An instance whose class declares no such overload raises exactly the
        // `MissingMethodException` a missing *method* raises — same wording,
        // same simple-name argument list. This used to build its own
        // `Klass.rightShift() is applicable for…` variant, which named the
        // method in the wrong position and printed the argument's *qualified*
        // class where Groovy prints the simple one.
        None => Some(raise_missing_method(
            vm,
            lhs,
            method,
            std::slice::from_ref(rhs),
        )),
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
    if let Some(v) = bigint_shift(false, &lhs, &rhs) {
        return v;
    }
    match (as_i64(&lhs), as_i64(&rhs)) {
        (Some(a), Some(b)) => java_shift("rightShift", wide, a, b),
        _ => raise_operator_operand(vm, "rightShift", &lhs, &rhs),
    }
}

/// `bigInteger << n` / `>> n` — the arbitrary-precision shifts, which have no
/// width to mask the count against and lose no bits: `1G << 100` is the full
/// 31-digit power of two and `12345678901234567890G << 2` keeps all 20 digits.
/// `None` when the left operand is not a `BigInteger` (so the caller's own
/// `Integer`/`Long` rules apply) or when the count is not an integer.
fn bigint_shift(left: bool, lhs: &Value, rhs: &Value) -> Option<Value> {
    as_bigint(lhs)?;
    let n = plain_int(rhs)?;
    decimal::shift(left, &as_exact_dec(lhs)?, n).map(bigint_value)
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
        (Some(a), Some(b)) => java_shift("rightShiftUnsigned", wide, a, b),
        // Not two integers. Groovy has four different answers for that, none of
        // them this one — an `IllegalArgumentException` carrying a sentence
        // written here rather than by Groovy, which no version has ever printed.
        _ => raise_operator_operand(vm, "rightShiftUnsigned", &lhs, &rhs),
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
    if let Some(found) = omap_get(&coll, &groovy_str(&needle)) {
        return Value::bool(found.is_some());
    }
    Value::bool(match &coll {
        Value::Array(a) => a.iter().any(|v| values_equal(v, &needle)),
        // `x in str` is `str.isCase(x)`, and `String.isCase` is *equality*, not
        // containment: `'a' in 'abc'` is `false` in Groovy. (`'a' in 'abc'`
        // reading as a substring test is the natural guess, and the wrong one —
        // `switch ('abc') { case 'a': }` does not match either.)
        Value::Str(s) => **s == groovy_str(&needle),
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
    // `null as T` is decided by whether `T` is a *primitive*: Groovy casts the
    // null to the wrapper and then unboxes it, so `null as int` is the JVM's
    // unboxing `NullPointerException` while `null as Integer` is just `null`.
    // groovyrs used to coerce the null instead and answer `0`/`NaN`/`[]`.
    // `boolean` is the exception — Groovy truth-tests rather than unboxes, and
    // `null as boolean` is `false`.
    if matches!(v, Value::Undef) && ty_simple != "boolean" {
        // The wrapper and accessor the JVM names in the message. The trailing
        // `because "` is where Groovy 5.0.8's message ends: the helpful-NPE text
        // names the local it read, and the local the cast reads is synthetic and
        // unnamed. Verified against the oracle, which prints exactly this.
        let unbox = match ty_simple.as_str() {
            "int" => Some("Integer.intValue"),
            "long" => Some("Long.longValue"),
            "short" => Some("Short.shortValue"),
            "byte" => Some("Byte.byteValue"),
            "double" => Some("Double.doubleValue"),
            "float" => Some("Float.floatValue"),
            "char" => Some("Character.charValue"),
            _ => None,
        };
        return match unbox {
            Some(m) => {
                raise(
                    vm,
                    "NullPointerException",
                    &format!("Cannot invoke \"java.lang.{m}()\" because \""),
                );
                Value::Undef
            }
            // Every reference target — the wrappers, `String`, the collections,
            // `BigDecimal`, `Object` — keeps the null.
            None => Value::Undef,
        };
    }
    match ty_simple.as_str() {
        // Integral targets truncate toward zero, as Java's narrowing casts do.
        "int" | "Integer" | "long" | "Long" | "short" | "Short" | "byte" | "Byte" => {
            let n = match &v {
                Value::Str(s) => match s.trim().parse::<i64>() {
                    Ok(n) => n,
                    Err(_) => return raise_number_format(vm, s.trim()),
                },
                Value::Float(f) => *f as i64,
                Value::Int(n) => *n,
                _ => match as_dec(&v) {
                    Some(d) => decimal::truncate_to_i64(&d),
                    // Not a number and not a parseable `String`, so there is no
                    // narrowing to perform. `as_i64(&v).unwrap_or(0)` answered
                    // `0` for a list, a map and a closure alike, and `1` for
                    // `true` — inventing a conversion Groovy declines.
                    None => return raise_cast(vm, &v, &ty_simple),
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
            Value::Int(_) | Value::Float(_) => Value::float(as_f64(&v)),
            // `as_f64` answers `NaN` for everything it cannot read, so a list or
            // a `Boolean` used to cast to `NaN` rather than raising.
            _ if as_dec(&v).is_some() => Value::float(as_f64(&v)),
            _ => raise_cast(vm, &v, &ty_simple),
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
                // `as BigDecimal` on a double is `BigDecimal.valueOf`, which
                // goes through `Double.toString` — `0.555d as BigDecimal` is
                // `0.555`, *not* the exact binary expansion the `new
                // BigDecimal(double)` constructor gives.
                // A non-finite double renders as `Infinity`/`NaN`, which
                // `BigDecimal`'s parser rejects character by character — the
                // same `NumberFormatException` Groovy raises.
                Value::Float(f) if as_dec(&v).is_none() => {
                    match decimal::parse_java(&decimal::format_double(*f)) {
                        Ok(d) => carry(d),
                        Err(msg) => {
                            raise_opt(vm, "NumberFormatException", msg.as_deref());
                            Value::Undef
                        }
                    }
                }
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
        // `map as TreeMap` re-homes the same entries into another implementation,
        // which changes the order they present in.
        //
        // `as HashMap` converts a `TreeMap` but leaves the other two **alone**,
        // because `asType` hands back an operand that is already an instance of
        // the target and `LinkedHashMap extends HashMap`: `[a: 1] as HashMap` is
        // still a `LinkedHashMap`. Casting the kind unconditionally got that
        // wrong in the direction that is hardest to notice — the entries are the
        // same, only the print order moves.
        "TreeMap" | "HashMap" | "LinkedHashMap" => match (as_omap(&v), omap_kind(&v)) {
            (Some(entries), Some(from)) => match ty_simple.as_str() {
                "TreeMap" => gmap_kind(entries, MapKind::Tree),
                "LinkedHashMap" => gmap_kind(entries, MapKind::Linked),
                _ if from == MapKind::Tree => {
                    let req = hash_req_for_map(entries.len());
                    gmap_kind(entries, MapKind::Hash { req })
                }
                _ => v,
            },
            _ => v,
        },
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

/// The fully-qualified name for a class written out with its package
/// (`java.math.RoundingMode`), or `None` when that package holds no such
/// modeled class.
///
/// Two sets answer here. Everything [`jdk_class_package`] knows is reachable
/// both bare and qualified, because Groovy default-imports those packages. The
/// second set is reachable *only* qualified — Groovy 5 does not import
/// `RoundingMode`, so a bare `RoundingMode.HALF_UP` raises
/// `MissingPropertyException` there and must raise here too.
pub fn jdk_qualified_class(package: &str, name: &str) -> Option<String> {
    let known = jdk_class_package(name) == Some(package)
        || matches!(
            (package, name),
            ("java.math", "RoundingMode") | ("java.math", "MathContext")
        );
    known.then(|| format!("{package}.{name}"))
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
                _ => Value::float(java_extreme_f64(f0, f1, pick_max)),
            }
        }
        ("Math", "sqrt") => Value::float(f0.sqrt()),
        ("Math", "cbrt") => Value::float(f0.cbrt()),
        ("Math", "floor") => Value::float(f0.floor()),
        ("Math", "ceil") => Value::float(f0.ceil()),
        ("Math", "rint") => Value::float(f0.round_ties_even()),
        ("Math", "signum") => Value::float(java_signum(f0)),
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
        // `asin`/`acos`/`atan`/`sinh`/`cosh`/`tanh`/`log1p`/`expm1` are
        // deliberately absent. Each is one `f64` method call away, and each was
        // measured against the oracle disagreeing in the last bit on ordinary
        // inputs (`Math.asin(0.5)` is `0.5235987755982989` on the JVM and
        // `…88` through the platform's libm). Answering within an ulp is a wrong
        // answer; `MissingMethodException` is not. See BUGS.md for the same
        // divergence in the transcendentals that *are* modeled.
        ("Math", "ulp") => Value::float(java_ulp(f0)),
        ("Math", "copySign") => Value::float(f0.copysign(as_f64(args.get(1)?))),
        ("Math", "nextUp") => Value::float(next_after(f0, f64::INFINITY)),
        ("Math", "nextDown") => Value::float(next_after(f0, f64::NEG_INFINITY)),
        ("Math", "nextAfter") => Value::float(next_after(f0, as_f64(args.get(1)?))),
        ("Math", "getExponent") => Value::int(i64::from(java_get_exponent(f0))),
        ("Math", "IEEEremainder") => {
            let y = as_f64(args.get(1)?);
            Value::float(f0 - y * (f0 / y).round_ties_even())
        }
        // `floorDiv`/`floorMod` round the quotient toward negative infinity,
        // where `/` and `%` truncate: `Math.floorDiv(-7, 2)` is `-4` and
        // `Math.floorMod(-7, 2)` is `1`. Rust's `div_euclid`/`rem_euclid` are
        // *Euclidean*, not floored — they differ from Java's whenever the
        // divisor is negative (`floorMod(7, -2)` is `-1`, `7.rem_euclid(-2)` is
        // `1`), so the sign correction is written out.
        ("Math", "floorDiv" | "floorMod") => {
            let (a, b) = (as_i64(&arg0)?, args.get(1).and_then(as_i64)?);
            if b == 0 {
                raise(vm, "ArithmeticException", "/ by zero");
                return Some(Value::Undef);
            }
            let (q, r) = (a.wrapping_div(b), a.wrapping_rem(b));
            let adjust = r != 0 && (r < 0) != (b < 0);
            Value::int(if method == "floorDiv" {
                if adjust {
                    q - 1
                } else {
                    q
                }
            } else if adjust {
                r + b
            } else {
                r
            })
        }
        // The `…Exact` family throws on overflow rather than wrapping. The width
        // is the class's, not the value's, so `Math.addExact` is the `int` form
        // and `toIntExact` narrows a `long`.
        ("Math", "addExact" | "subtractExact" | "multiplyExact") => {
            let (a, b) = (as_i64(&arg0)?, args.get(1).and_then(as_i64)?);
            let wide = match method {
                "addExact" => a.checked_add(b),
                "subtractExact" => a.checked_sub(b),
                _ => a.checked_mul(b),
            };
            match wide.filter(|n| i32::try_from(*n).is_ok()) {
                Some(n) => Value::int(n),
                None => {
                    raise(vm, "ArithmeticException", "integer overflow");
                    return Some(Value::Undef);
                }
            }
        }
        ("Math", "toIntExact") => match as_i64(&arg0).filter(|n| i32::try_from(*n).is_ok()) {
            Some(n) => Value::int(n),
            None => {
                raise(vm, "ArithmeticException", "integer overflow");
                return Some(Value::Undef);
            }
        },

        // `Double.compare` is not `<`/`>`: NaN is greater than everything and
        // `-0.0` is below `+0.0`. See [`java_compare_f64`].
        ("Double" | "Float", "compare") => {
            Value::int(match java_compare_f64(f0, as_f64(args.get(1)?)) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            })
        }
        ("Double" | "Float", "isNaN") => Value::bool(f0.is_nan()),
        ("Double" | "Float", "isInfinite") => Value::bool(f0.is_infinite()),
        ("Double" | "Float", "isFinite") => Value::bool(f0.is_finite()),
        ("Double", "sum") => Value::float(f0 + as_f64(args.get(1)?)),
        ("Double", "max" | "min") => {
            Value::float(java_extreme_f64(f0, as_f64(args.get(1)?), method == "max"))
        }
        // `doubleToLongBits` collapses every NaN payload to one canonical
        // pattern; `doubleToRawLongBits` does not.
        ("Double", "doubleToLongBits") => Value::int(if f0.is_nan() {
            0x7ff8_0000_0000_0000u64 as i64
        } else {
            f0.to_bits() as i64
        }),
        ("Double", "doubleToRawLongBits") => Value::int(f0.to_bits() as i64),
        ("Double", "longBitsToDouble") => Value::float(f64::from_bits(as_i64(&arg0)? as u64)),

        // The bit-twiddling statics take their width from the class name, which
        // is the one place a width is not invisible in a `Value::Int`.
        ("Integer" | "Long", "compare" | "compareUnsigned") => {
            let (a, b) = (as_i64(&arg0)?, args.get(1).and_then(as_i64)?);
            let (a, b) = if method == "compareUnsigned" {
                (a as u64 as i64 ^ i64::MIN, b as u64 as i64 ^ i64::MIN)
            } else {
                (a, b)
            };
            Value::int((a > b) as i64 - (a < b) as i64)
        }
        ("Integer" | "Long", "sum") => {
            let (a, b) = (as_i64(&arg0)?, args.get(1).and_then(as_i64)?);
            let sum = a.wrapping_add(b);
            Value::int(if class == "Integer" {
                i64::from(sum as i32)
            } else {
                sum
            })
        }
        ("Integer" | "Long", "max" | "min") => {
            let (a, b) = (as_i64(&arg0)?, args.get(1).and_then(as_i64)?);
            Value::int(if method == "max" { a.max(b) } else { a.min(b) })
        }
        ("Integer" | "Long", "signum") => Value::int(as_i64(&arg0)?.signum()),
        // Split across two arms so neither pattern needs a continuation line
        // starting with a string literal — `dispatch_names_are_unique` reads
        // such a line as a second arm and then reports every name on it twice.
        ("Integer" | "Long", "bitCount" | "numberOfLeadingZeros" | "numberOfTrailingZeros") => {
            java_bit_static(class, method, as_i64(&arg0)?)
        }
        ("Integer" | "Long", "highestOneBit" | "lowestOneBit" | "reverse" | "reverseBytes") => {
            java_bit_static(class, method, as_i64(&arg0)?)
        }

        // `parseInt`/`parseLong`/`valueOf` take an optional radix, so
        // `Integer.parseInt("ff", 16)` is `255` — the second argument used to be
        // dropped, which turned every non-decimal parse into a
        // `NumberFormatException`.
        // The parse is *not* lenient: `Integer.parseInt(" 5 ")` throws in Java
        // (`Long.parseLong` too), so the text goes in untrimmed. And the result
        // has to fit the named class — `Integer.parseInt("3000000000")` is a
        // `NumberFormatException`, not the `long` the digits spell.
        ("Integer" | "Long" | "Short" | "Byte", "parseInt" | "parseLong" | "valueOf") => {
            let text = groovy_str(&arg0);
            let parsed = match args.get(1).and_then(as_i64) {
                Some(radix) => java_parse_radix(&text, radix),
                None => text.parse::<i64>().ok(),
            };
            let fits = |n: i64| match class {
                "Integer" => i64::from(i32::MIN) <= n && n <= i64::from(i32::MAX),
                "Short" => i64::from(i16::MIN) <= n && n <= i64::from(i16::MAX),
                "Byte" => i64::from(i8::MIN) <= n && n <= i64::from(i8::MAX),
                _ => true,
            };
            match parsed.filter(|n| fits(*n)) {
                Some(n) => Value::int(n),
                None => raise_number_format(vm, &text),
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

        // `java.lang.Character`'s classification statics. A Groovy `char` is a
        // one-character `String` here, and the `int` overloads take a code
        // point — `Character.isDigit(53)` is `true`, the same as `'5'`.
        //
        // Only the overloads Groovy's own dispatch admits are modeled.
        // `Character.isAlphabetic(c)`, `digit(c, radix)` and `forDigit(d, radix)`
        // are deliberately absent: all three are `MissingMethodException` under
        // Apache Groovy 5.1.0, so answering them would be a divergence in the
        // other direction.
        ("Character", "isDigit" | "isLetter" | "isLetterOrDigit" | "isWhitespace") => {
            let c = java_char_arg(&arg0)?;
            Value::bool(match method {
                "isDigit" => c.is_numeric(),
                "isLetter" => c.is_alphabetic(),
                "isLetterOrDigit" => c.is_alphanumeric(),
                _ => java_is_whitespace(c),
            })
        }
        ("Character", "isUpperCase" | "isLowerCase") => {
            let c = java_char_arg(&arg0)?;
            Value::bool(if method == "isUpperCase" {
                c.is_uppercase()
            } else {
                c.is_lowercase()
            })
        }
        // These answer a `char`, which prints as the bare character.
        ("Character", "toUpperCase" | "toLowerCase" | "toString") => {
            let c = java_char_arg(&arg0)?;
            Value::str(match method {
                "toUpperCase" => c.to_uppercase().to_string(),
                "toLowerCase" => c.to_lowercase().to_string(),
                _ => c.to_string(),
            })
        }
        ("Character", "compare") => {
            let (a, b) = (java_char_arg(&arg0)?, java_char_arg(args.get(1)?)?);
            Value::int(a as i64 - b as i64)
        }
        // `getNumericValue` answers the digit a character spells — `-1` for one
        // that spells none.
        ("Character", "getNumericValue") => {
            let c = java_char_arg(&arg0)?;
            Value::int(c.to_digit(36).map(i64::from).unwrap_or(-1))
        }

        // `java.util.Collections`. The three mutators write **through the
        // handle**, which is what makes `Collections.sort(l)` reorder the caller's
        // list, and answer `void` — `null` — rather than the list.
        ("Collections", "emptyList" | "emptySet") => Value::array(Vec::new()),
        ("Collections", "emptyMap") => gmap(Vec::new()),
        ("Collections", "singletonList" | "singleton") => Value::array(vec![arg0]),
        ("Collections", "nCopies") => {
            let n = as_i64(&arg0)?.max(0) as usize;
            Value::array(vec![args.get(1)?.clone(); n])
        }
        // `unmodifiableList`/`unmodifiableMap` answer the same elements; the
        // wrapper *type* is not modeled, so a write through the result is not
        // rejected. See BUGS.md.
        ("Collections", "unmodifiableList" | "unmodifiableCollection") => {
            Value::array(iteration_elements(&arg0))
        }
        ("Collections", "sort" | "reverse") => {
            let id = list_id(&arg0)?;
            let mut items = iteration_elements(&arg0);
            if method == "sort" {
                items = match sort_values(vm, &items, &OrderBy::Natural) {
                    Ok(sorted) => sorted,
                    Err(e) => {
                        fault(vm, e);
                        return Some(Value::Undef);
                    }
                };
            } else {
                items.reverse();
            }
            list_store(id, items, false);
            Value::Undef
        }
        // `Collections.max`/`min` order by the natural comparator — not the
        // primitive `>`/`<` scan `DefaultGroovyMethods.max` uses (see
        // [`prefers`]), so they are the sort's endpoints rather than that scan's.
        ("Collections", "max" | "min") => {
            let items = iteration_elements(&arg0);
            let sorted = match sort_values(vm, &items, &OrderBy::Natural) {
                Ok(sorted) => sorted,
                Err(e) => {
                    fault(vm, e);
                    return Some(Value::Undef);
                }
            };
            if method == "max" {
                sorted.last()?.clone()
            } else {
                sorted.first()?.clone()
            }
        }
        ("Collections", "frequency") => {
            let want = args.get(1)?;
            Value::int(
                iteration_elements(&arg0)
                    .iter()
                    .filter(|v| values_equal(v, want))
                    .count() as i64,
            )
        }
        ("Collections", "disjoint") => {
            let (a, b) = (iteration_elements(&arg0), iteration_elements(args.get(1)?));
            Value::bool(!a.iter().any(|x| b.iter().any(|y| values_equal(x, y))))
        }
        // `Arrays.asList(a, b, c)` — the varargs form, the only one a Groovy
        // script without real arrays can write.
        ("Arrays", "asList") => Value::array(args.to_vec()),

        // `java.lang.System`'s property readers. `getProperty(name)` answers
        // null for a name the JVM does not carry, and the two-argument form
        // answers the supplied default instead.
        ("System", "lineSeparator") => Value::str("\n".to_string()),
        ("System", "getProperty") => match java_system_property(&groovy_str(&arg0)) {
            Some(v) => Value::str(v.to_string()),
            None => args.get(1).cloned().unwrap_or(Value::Undef),
        },
        ("System", "getenv") => match std::env::var(groovy_str(&arg0)) {
            Ok(v) => Value::str(v),
            Err(_) => Value::Undef,
        },
        _ => return None,
    })
}

/// The `char` an argument to a `java.lang.Character` static denotes. A Groovy
/// `char` is a one-character `String` here; an `int` argument selects the
/// code-point overload, so `Character.isDigit(53)` asks about `'5'`.
fn java_char_arg(v: &Value) -> Option<char> {
    match v {
        Value::Int(n) => char::from_u32(u32::try_from(*n).ok()?),
        Value::Str(s) => s.chars().next(),
        _ => None,
    }
}

/// The `System.getProperty` names groovyrs answers. Deliberately a short list
/// rather than a passthrough to the process environment: the values a script can
/// depend on are the ones whose answer is a property of the platform, and
/// inventing an answer for `java.version` (or leaking the host's) would be worse
/// than the `null` Java gives for an unset name.
fn java_system_property(name: &str) -> Option<&'static str> {
    Some(match name {
        "line.separator" => "\n",
        "file.separator" => "/",
        "path.separator" => ":",
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
        // `Float`'s constants are stored as the `double` nearest the text a
        // `Float` *prints*, not as `f32::MAX as f64`. groovyrs has no
        // `java.lang.Float` (an `f`-suffixed literal is a `double`, as
        // `1.0f / 3.0f` being `0.3333333333333333` on both sides shows), so a
        // widened `f32::MAX` would render `3.4028234663852886E38` where Groovy
        // renders `3.4028235E38`. The two agree to within `f32` precision —
        // they round to the same `float` — and only the printed form differs,
        // so the printed form is what is kept.
        ("Float", "MAX_VALUE") => Value::float(3.4028235e38),
        ("Float", "MIN_VALUE") => Value::float(1.4e-45),
        ("Float", "MIN_NORMAL") => Value::float(1.1754944e-38),
        ("Float", "POSITIVE_INFINITY") => Value::float(f64::INFINITY),
        ("Float", "NEGATIVE_INFINITY") => Value::float(f64::NEG_INFINITY),
        ("Float", "NaN") => Value::float(f64::NAN),
        ("Float", "SIZE") => Value::int(32),
        ("Float", "BYTES") => Value::int(4),
        ("Float", "MAX_EXPONENT") => Value::int(127),
        ("Float", "MIN_EXPONENT") => Value::int(-126),
        // A `char` is a one-character `String` here (BUGS.md, *Java `char`*), so
        // `Character`'s bounds are the code points `' '` and `'￿'`.
        // `java.math.RoundingMode`'s constants. Each is its own name as a
        // `String`: that is what an enum constant *prints*, what
        // `BigDecimal.setScale` reads back here, and what comparing two of them
        // answers. Only `getClass()` tells the difference (BUGS.md).
        ("RoundingMode", _)
            if matches!(
                name,
                "UP" | "DOWN"
                    | "CEILING"
                    | "FLOOR"
                    | "HALF_UP"
                    | "HALF_DOWN"
                    | "HALF_EVEN"
                    | "UNNECESSARY"
            ) =>
        {
            Value::str(name.to_string())
        }
        ("Character", "MIN_VALUE") => Value::str("\u{0}".to_string()),
        ("Character", "MAX_VALUE") => Value::str("\u{ffff}".to_string()),
        ("Character", "SIZE") => Value::int(16),
        ("Character", "BYTES") => Value::int(2),
        ("Double", "MAX_VALUE") => Value::float(f64::MAX),
        // Java's `Double.MIN_VALUE` is the smallest *subnormal* (4.9E-324);
        // Rust's `f64::MIN_POSITIVE` is the smallest *normal*, which is Java's
        // `MIN_NORMAL`. The two names look interchangeable and are not — reading
        // `MIN_POSITIVE` here answered `2.2250738585072014E-308`.
        ("Double", "MIN_VALUE") => Value::float(f64::from_bits(1)),
        ("Double", "MIN_NORMAL") => Value::float(f64::MIN_POSITIVE),
        ("Double", "POSITIVE_INFINITY") => Value::float(f64::INFINITY),
        ("Double", "NEGATIVE_INFINITY") => Value::float(f64::NEG_INFINITY),
        ("Double", "NaN") => Value::float(f64::NAN),
        ("Double", "MAX_EXPONENT") => Value::int(1023),
        ("Double", "MIN_EXPONENT") => Value::int(-1022),
        // `SIZE`/`BYTES` are the width of the *primitive*, and are `int`s.
        ("Integer", "SIZE") => Value::int(32),
        ("Integer", "BYTES") => Value::int(4),
        ("Long", "SIZE") => Value::int(64),
        ("Long", "BYTES") => Value::int(8),
        ("Double", "SIZE") => Value::int(64),
        ("Double", "BYTES") => Value::int(8),
        ("Short", "SIZE") => Value::int(16),
        ("Short", "BYTES") => Value::int(2),
        ("Byte", "SIZE") => Value::int(8),
        ("Byte", "BYTES") => Value::int(1),
        ("Math", "PI") => Value::float(std::f64::consts::PI),
        ("Math", "E") => Value::float(std::f64::consts::E),
        // The radix bounds `Integer.parseInt`/`toString` clamp to (see
        // [`java_radix_string`]), and `int`s rather than `char`s.
        ("Character", "MIN_RADIX") => Value::int(2),
        ("Character", "MAX_RADIX") => Value::int(36),
        // `BigDecimal`'s constants keep scale 0, and `BigInteger`'s stay
        // `BigInteger`s — the two are distinct types in every later operation.
        ("BigDecimal", "ZERO") => dec_value(decimal::from_i64(0)),
        ("BigDecimal", "ONE") => dec_value(decimal::from_i64(1)),
        ("BigDecimal", "TWO") => dec_value(decimal::from_i64(2)),
        ("BigDecimal", "TEN") => dec_value(decimal::from_i64(10)),
        ("BigInteger", "ZERO") => bigint_value(decimal::from_i64(0)),
        ("BigInteger", "ONE") => bigint_value(decimal::from_i64(1)),
        ("BigInteger", "TWO") => bigint_value(decimal::from_i64(2)),
        ("BigInteger", "TEN") => bigint_value(decimal::from_i64(10)),
        _ => return None,
    })
}

/// `java.util.Formatter`, driven by [`crate::format`]'s specifier parser.
///
/// The JDK's `Formatter` is strict in three ways a naive printf is not, and a
/// Groovy script sees all three: a conversion accepts a fixed flag set and
/// throws on anything else, an integral conversion refuses a `BigDecimal`
/// argument (`"%d"` of the literal `1.5` is an `IllegalFormatConversionException`,
/// not `1.5`), and a `double` is laid out from its shortest round-trip
/// representation rather than its exact binary value.
///
/// The one deliberate departure is `%c`: groovyrs has no `java.lang.Character`
/// (BUGS.md, *Java `char`*), modeling a `char` as a one-character `String`, so
/// `%c` accepts a one-character `String` where the JDK would refuse a `String`
/// outright. A longer `String` still raises, which is what catches the real
/// mistake.
fn java_format(vm: &mut VM, spec: &str, args: &[Value]) -> String {
    let pieces = match crate::format::parse(spec) {
        Ok(p) => p,
        Err(e) => {
            raise(vm, e.class(), &e.message());
            return String::new();
        }
    };
    let mut out = String::new();
    // Ordinary conversions walk the argument list; an explicit `%3$s` index
    // reads without advancing that cursor, and `%<s` re-reads the last one.
    let mut next = 0usize;
    let mut last = 0usize;
    for piece in pieces {
        let spec = match piece {
            crate::format::Piece::Literal(text) => {
                out.push_str(&text);
                continue;
            }
            crate::format::Piece::Conv(s) => s,
        };
        match spec.conv {
            '%' => {
                out.push_str(&pad_conversion(&spec, "%", ""));
                continue;
            }
            'n' => {
                out.push('\n');
                continue;
            }
            _ => {}
        }
        let idx = if spec.prev {
            last
        } else if let Some(i) = spec.index {
            i.saturating_sub(1)
        } else {
            next += 1;
            next - 1
        };
        last = idx;
        let Some(arg) = args.get(idx).cloned() else {
            raise(
                vm,
                "MissingFormatArgumentException",
                &format!("Format specifier '{}'", spec.text),
            );
            return String::new();
        };
        match format_one(vm, &spec, &arg) {
            Some(body) => out.push_str(&body),
            // `format_one` has already parked the throwable.
            None => return String::new(),
        }
    }
    out
}

/// Render one conversion, padded. `None` means a throwable was raised.
fn format_one(vm: &mut VM, spec: &crate::format::Spec, arg: &Value) -> Option<String> {
    let null = matches!(arg, Value::Undef);
    let refuse = |vm: &mut VM| -> Option<String> {
        raise(
            vm,
            "IllegalFormatConversionException",
            &format!("{} != {}", spec.conv, java_class_name(arg)),
        );
        None
    };
    // A null argument prints `null` under every conversion but `%b`, which is
    // the one conversion defined on the *absence* of a value.
    if null && !matches!(spec.conv, 'b' | 'B') {
        return Some(pad_conversion(spec, &truncated("null", spec.precision), ""));
    }
    match spec.conv {
        'b' | 'B' => {
            let text = match arg {
                Value::Undef => "false".to_string(),
                Value::Bool(b) => b.to_string(),
                _ => "true".to_string(),
            };
            Some(pad_conversion(
                spec,
                &cased(&text, spec.conv, spec.precision),
                "",
            ))
        }
        'h' | 'H' => {
            let text = format!("{:x}", object_hash_code(arg) as u32);
            Some(pad_conversion(
                spec,
                &cased(&text, spec.conv, spec.precision),
                "",
            ))
        }
        's' | 'S' => {
            let text = render_value(vm, arg);
            Some(pad_conversion(
                spec,
                &cased(&text, spec.conv, spec.precision),
                "",
            ))
        }
        'c' | 'C' => {
            let text = match arg {
                // A code point, the way the JDK's `%c` takes an `int`.
                Value::Int(n) => match u32::try_from(*n).ok().and_then(char::from_u32) {
                    Some(c) => c.to_string(),
                    None => {
                        raise(vm, "IllegalFormatCodePointException", &format!("{n:#x}"));
                        return None;
                    }
                },
                // groovyrs's stand-in for `java.lang.Character`.
                Value::Str(s) if s.chars().count() == 1 => s.to_string(),
                _ => return refuse(vm),
            };
            Some(pad_conversion(spec, &cased(&text, spec.conv, None), ""))
        }
        'd' => {
            let digits = if let Some(b) = as_bigint(arg) {
                decimal::to_groovy_string(&decimal::abs(&b))
            } else if let Value::Int(n) = arg {
                n.unsigned_abs().to_string()
            } else {
                return refuse(vm);
            };
            let negative = as_bigint(arg)
                .map(|b| b.is_negative())
                .unwrap_or(matches!(arg, Value::Int(n) if *n < 0));
            let digits = if spec.has(',') {
                group_integer_digits(&digits)
            } else {
                digits
            };
            Some(signed_number(spec, &digits, negative, ""))
        }
        'o' | 'x' | 'X' => {
            let radix: u32 = if spec.conv == 'o' { 8 } else { 16 };
            let prefix = match (spec.has('#'), spec.conv) {
                (true, 'o') => "0",
                (true, 'x') => "0x",
                (true, 'X') => "0X",
                _ => "",
            };
            if let Some(b) = as_bigint(arg) {
                // A `BigInteger` is not a two's-complement word, so the JDK
                // signs it — and only then are `+`, ` ` and `(` legal.
                let digits = decimal::to_radix_string(&decimal::abs(&b), radix as i64);
                let digits = cased(&digits, spec.conv, None);
                return Some(signed_number(spec, &digits, b.is_negative(), prefix));
            }
            let Value::Int(n) = arg else {
                return refuse(vm);
            };
            for f in "+ (".chars() {
                if spec.has(f) {
                    let e = crate::format::Error::FlagMismatch(spec.conv, f);
                    raise(vm, e.class(), &e.message());
                    return None;
                }
            }
            // Two's complement at the argument's own width: an `Integer` wraps
            // at 32 bits (`%x` of `-1` is `ffffffff`), a `Long` at 64.
            let text = match i32::try_from(*n) {
                Ok(small) => radix_text(small as u32 as u64, radix),
                Err(_) => radix_text(*n as u64, radix),
            };
            let text = cased(&text, spec.conv, None);
            Some(pad_conversion(spec, &text, prefix))
        }
        'e' | 'E' | 'f' | 'g' | 'G' | 'a' | 'A' => {
            let prec = spec.precision.unwrap_or(6);
            // A `double` keeps the IEEE path; every other Groovy decimal is an
            // exact `BigDecimal`. An integral type is refused outright.
            let (value, is_double) = match arg {
                Value::Float(f) => {
                    if f.is_nan() || f.is_infinite() {
                        let body = if f.is_nan() { "NaN" } else { "Infinity" };
                        return Some(signed_word(spec, body, f.is_sign_negative() && !f.is_nan()));
                    }
                    match decimal::parse(&decimal::format_double(*f)) {
                        Some(d) => (d, true),
                        None => return refuse(vm),
                    }
                }
                _ if as_bigint(arg).is_some() => return refuse(vm),
                _ => match as_dec(arg) {
                    Some(d) => (d, false),
                    None => return refuse(vm),
                },
            };
            let negative = value.is_negative();
            let magnitude = decimal::abs(&value);
            // A zero `double` has no scale to take an exponent from, so `%e` of
            // `0.0d` is `e+00` where the `BigDecimal` `0.0` is `e-01`.
            let zero_exp = (is_double && magnitude.is_zero()).then_some(0);
            let digits = match spec.conv {
                'f' => {
                    let body = crate::format::fixed(&magnitude, prec);
                    if spec.has(',') {
                        group_integer_digits(&body)
                    } else {
                        body
                    }
                }
                'e' | 'E' => cased(
                    &crate::format::scientific(&magnitude, prec, zero_exp),
                    spec.conv,
                    None,
                ),
                'g' | 'G' => {
                    let prec = if spec.precision == Some(0) { 1 } else { prec };
                    let decimals = if is_double {
                        general_double_digits(&magnitude, prec)
                    } else {
                        crate::format::general_decimal_digits(&magnitude, prec)
                    };
                    let body = match decimals {
                        Some(d) => {
                            let body = crate::format::fixed(&magnitude, d);
                            if spec.has(',') {
                                group_integer_digits(&body)
                            } else {
                                body
                            }
                        }
                        None => crate::format::scientific(&magnitude, prec - 1, zero_exp),
                    };
                    cased(&body, spec.conv, None)
                }
                _ => {
                    if !is_double {
                        return refuse(vm);
                    }
                    cased(&hex_double(as_f64(arg).abs()), spec.conv, None)
                }
            };
            Some(signed_number(spec, &digits, negative, ""))
        }
        _ => Some(String::new()),
    }
}

/// `Integer.toHexString`/`toOctalString` of an already-widened word.
fn radix_text(bits: u64, radix: u32) -> String {
    if radix == 8 {
        format!("{bits:o}")
    } else {
        format!("{bits:x}")
    }
}

/// Apply `%S`/`%B`/`%X`-style upper-casing and a `%s`-style precision cut.
fn cased(text: &str, conv: char, precision: Option<usize>) -> String {
    let text = truncated(text, precision);
    if conv.is_uppercase() {
        text.to_uppercase()
    } else {
        text
    }
}

/// A precision on `%s`/`%b`/`%h` cuts the rendering to that many characters.
fn truncated(text: &str, precision: Option<usize>) -> String {
    match precision {
        Some(p) if text.chars().count() > p => text.chars().take(p).collect(),
        _ => text.to_string(),
    }
}

/// A non-numeric float body (`NaN`, `Infinity`): the sign flags apply, the
/// zero-fill does not — the JDK pads these with spaces whatever `0` says.
fn signed_word(spec: &crate::format::Spec, body: &str, negative: bool) -> String {
    let sign = if negative {
        "-"
    } else if spec.has('+') {
        "+"
    } else if spec.has(' ') {
        " "
    } else {
        ""
    };
    pad_text_body(spec, &format!("{sign}{body}"), false)
}

/// Attach the sign (or the `(` flag's parentheses) to an unsigned numeric body
/// and pad it. Zero-fill goes *between* the sign and the digits, which is why
/// this cannot go through the plain text padder.
fn signed_number(
    spec: &crate::format::Spec,
    digits: &str,
    negative: bool,
    radix_prefix: &str,
) -> String {
    let (open, close) = if negative && spec.has('(') {
        ("(", ")")
    } else if negative {
        ("-", "")
    } else if spec.has('+') {
        ("+", "")
    } else if spec.has(' ') {
        (" ", "")
    } else {
        ("", "")
    };
    let width = spec.width.unwrap_or(0);
    let fixed = open.chars().count()
        + close.chars().count()
        + radix_prefix.chars().count()
        + digits.chars().count();
    if spec.has('0') && width > fixed && !spec.has('-') {
        return format!(
            "{open}{radix_prefix}{}{digits}{close}",
            "0".repeat(width - fixed)
        );
    }
    pad_text_body(spec, &format!("{open}{radix_prefix}{digits}{close}"), false)
}

/// Width padding for a conversion whose body carries no sign.
fn pad_conversion(spec: &crate::format::Spec, body: &str, radix_prefix: &str) -> String {
    pad_text_body(spec, &format!("{radix_prefix}{body}"), spec.has('0'))
}

/// Pad `body` to the specifier's width, left- or right-justified.
fn pad_text_body(spec: &crate::format::Spec, body: &str, zero_fill: bool) -> String {
    let width = spec.width.unwrap_or(0);
    let len = body.chars().count();
    if len >= width {
        return body.to_string();
    }
    let fill = if zero_fill { "0" } else { " " };
    if spec.has('-') {
        format!("{body}{}", " ".repeat(width - len))
    } else {
        format!("{}{body}", fill.repeat(width - len))
    }
}

/// `%g`'s branch for a `double` argument. Unlike the `BigDecimal` rule (which
/// compares the value itself — see [`crate::format::general_decimal_digits`]),
/// the JDK decides a `double` on the exponent the value has *after* rounding to
/// `prec` significant digits, so a zero takes the decimal branch.
fn general_double_digits(magnitude: &BigDecimal, prec: usize) -> Option<usize> {
    if magnitude.is_zero() {
        return Some(prec - 1);
    }
    // The rounded scientific rendering carries the exponent the branch needs.
    let sci = crate::format::scientific(magnitude, prec.saturating_sub(1), None);
    let exp: i64 = sci
        .rsplit('e')
        .next()
        .and_then(|e| e.parse().ok())
        .unwrap_or(0);
    if (-4..prec as i64).contains(&exp) {
        Some((prec as i64 - exp - 1).max(0) as usize)
    } else {
        None
    }
}

/// `Double.toHexString`, which is what `%a` prints: a normalized value as
/// `0x1.<hex mantissa>p<exponent>`, a subnormal as `0x0.…p-1022`, and a zero as
/// `0x0.0p0`. The value is non-negative; the caller attaches the sign.
fn hex_double(f: f64) -> String {
    if f == 0.0 {
        return "0x0.0p0".to_string();
    }
    let bits = f.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i64;
    let mantissa = bits & 0x000f_ffff_ffff_ffff;
    let (lead, unbiased) = if exponent == 0 {
        ("0x0.", -1022)
    } else {
        ("0x1.", exponent - 1023)
    };
    let mut hex = format!("{mantissa:013x}");
    while hex.len() > 1 && hex.ends_with('0') {
        hex.pop();
    }
    format!("{lead}{hex}p{unbiased}")
}

/// Insert `,` every three digits of `text`'s integer part, leaving a sign, a
/// fraction and an exponent alone: `1234567` becomes `1,234,567` and
/// `-1234.5678` becomes `-1,234.5678`. This is `%,d` / `%,f`'s en-US grouping.
fn group_integer_digits(text: &str) -> String {
    let (sign, rest) = match text.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", text),
    };
    let cut = rest.find(['.', 'e', 'E']).unwrap_or(rest.len());
    let (int_part, tail) = rest.split_at(cut);
    if !int_part.chars().all(|c| c.is_ascii_digit()) {
        return text.to_string();
    }
    let mut grouped = String::with_capacity(int_part.len() + int_part.len() / 3);
    for (i, c) in int_part.chars().enumerate() {
        if i > 0 && (int_part.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{sign}{grouped}{tail}")
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
        "toString" => Value::str(range_str(r)),
        "inspect" => Value::str(range_inspect(r)),
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

/// Java's `hashCode` for the element kinds a set can order **by bucket**. The
/// rules themselves are [`object_hash_code`]'s; what differs is the `None`,
/// which reports a value whose hash is the JVM's identity hash — not
/// reproducible here, and not reproducible across two JVM runs either — so
/// [`hash_order`] leaves it in insertion position rather than bucketing it on a
/// number Groovy would not have produced. `object_hash_code` has to answer
/// *something* for those (a script calling `.hashCode()` gets a number), which
/// is why the two are separate and this one stays narrower.
fn java_hash(v: &Value) -> Option<i32> {
    match v {
        Value::Int(_) | Value::Bool(_) | Value::Str(_) => Some(object_hash_code(v)),
        // `null` hashes to 0 both as a `HashMap` key and as a `List` element
        // (`Objects.hashCode(null)`), though a bare `null.hashCode()` throws.
        Value::Undef => Some(0),
        // `List.hashCode` — `31 * acc + element.hashCode()` from 1. This is what
        // puts a `HashSet<List>` (`permutations()`, `subsequences()`) in the
        // order Groovy prints. An element with no reproducible hash makes the
        // whole list's unreproducible too, which is why this cannot just call
        // `list_hash`.
        Value::Array(items) => items.iter().try_fold(1i32, |h, e| {
            Some(h.wrapping_mul(31).wrapping_add(java_hash(e)?))
        }),
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
        // `inspect()` is deliberately absent here: a set's is the quoted
        // rendering `([1, 'a'] as Set).inspect()` == `[1, 'a']`, which
        // `dispatch_method` answers for every value ahead of this table. It used
        // to share this arm with `toString`, which prints `[1, a]`.
        "toString" => Value::str(groovy_str(recv)),
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
/// The class a map handle of this kind names.
fn map_class(kind: MapKind) -> &'static str {
    match kind {
        MapKind::Linked => "java.util.LinkedHashMap",
        MapKind::Hash { .. } => "java.util.HashMap",
        MapKind::Tree => "java.util.TreeMap",
    }
}

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
    range_rendered(r, groovy_str)
}

/// `Range.inspect()`. Same layout as [`range_str`] with the endpoints rendered
/// *verbosely*, so a `String`-bounded range quotes them: `('a'..'c').inspect()`
/// is `'a'..'c'` where its `toString()` is `a..c`. The numeric ranges render
/// identically either way.
fn range_inspect(r: &RangeVal) -> String {
    range_rendered(r, inspect_value)
}

/// The shared `from..to` layout, with `render` deciding how an endpoint prints.
fn range_rendered(r: &RangeVal, render: impl Fn(&Value) -> String) -> String {
    // An `ObjectRange` renders the values it actually walks — `('a'..<'e')`
    // prints `a..d` — where the numeric ranges keep the form they were written
    // in. An empty range keeps its `..<` whatever its class.
    if range_class(r) == "groovy.lang.ObjectRange" {
        let last = if r.inclusive {
            r.to.clone()
        } else {
            successor(&r.to, range_is_reverse(r)).unwrap_or_else(|| r.to.clone())
        };
        return format!("{}..{}", render(&r.from), render(&last));
    }
    format!(
        "{}{}{}",
        render(&r.from),
        if r.inclusive { ".." } else { "..<" },
        render(&r.to)
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
    // A lone list argument is the argument *vector*, not one `%s` operand —
    // Groovy's `sprintf(String, Object)` spreads an array or a `List`. A list
    // literal is a heap handle rather than a `Value::Array`, so both shapes have
    // to unwrap or `sprintf("%h", [1, 2])` hashes the *list* where Groovy
    // hashes its first element.
    let rest: Vec<Value> = match rest {
        [Value::Array(a)] => a.to_vec(),
        [only] => as_list(only).unwrap_or_else(|| vec![only.clone()]),
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
///
/// Not `(f + 0.5).floor()`. That was `Math.round`'s implementation before Java
/// 7, and it is wrong for the doubles where `f + 0.5` rounds *up* to the next
/// representable value: `Math.round(0.49999999999999994)` is `0` in Java and `1`
/// under the old formula (JDK-8010430). Java 7 replaced it with a bit-level
/// truncate-then-adjust, which this reproduces as `floor(f)` plus one when the
/// fraction is at least a half — computed without ever forming `f + 0.5`.
fn java_round(f: f64) -> i64 {
    if f.is_nan() {
        return 0;
    }
    let floor = f.floor();
    let rounded = if f - floor >= 0.5 { floor + 1.0 } else { floor };
    // `as` saturates at the i64 bounds, which is what Java's `(long)` cast and
    // `Math.round`'s own clamp both do for an infinity or a huge magnitude.
    rounded as i64
}

/// `Math.signum`: the sign as a `double`, with the **zero returned unchanged**
/// so `signum(-0.0)` is `-0.0` and `signum(0.0)` is `0.0`.
///
/// Rust's `f64::signum` answers `±1.0` for `±0.0` — same name, different
/// function. NaN agrees (both answer NaN).
fn java_signum(f: f64) -> f64 {
    if f == 0.0 || f.is_nan() {
        f
    } else {
        f.signum()
    }
}

/// `Math.max`/`Math.min` on doubles, which are **not** `f64::max`/`f64::min`.
///
/// Rust's are IEEE `fmax`/`fmin`: they *ignore* a NaN operand and answer the
/// other one, and they do not distinguish `-0.0` from `+0.0`. Java's are
/// specified in terms of `Double.compare`: a NaN operand makes the answer NaN,
/// `max(-0.0, +0.0)` is `+0.0`, and `min(-0.0, +0.0)` is `-0.0`.
fn java_extreme_f64(a: f64, b: f64, want_max: bool) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if a == 0.0 && b == 0.0 {
        // Both zeros: the sign bit decides, which `==` cannot see.
        let a_neg = a.is_sign_negative();
        return if want_max == a_neg { b } else { a };
    }
    if want_max {
        if a > b {
            a
        } else {
            b
        }
    } else if a < b {
        a
    } else {
        b
    }
}

/// The `Integer`/`Long` bit-twiddling statics. The width comes from the class
/// name — the one place in this file where a width is *not* invisible in a
/// `Value::Int` — so `Integer.reverse(1)` answers `-2147483648` where
/// `Long.reverse(1)` answers `-9223372036854775808`.
fn java_bit_static(class: &str, method: &str, n: i64) -> Value {
    if class == "Integer" {
        let u = n as i32 as u32;
        let highest = if u == 0 {
            0
        } else {
            1u32 << (31 - u.leading_zeros())
        };
        Value::int(match method {
            "bitCount" => i64::from(u.count_ones()),
            "numberOfLeadingZeros" => i64::from(u.leading_zeros()),
            "numberOfTrailingZeros" => i64::from(u.trailing_zeros()),
            "highestOneBit" => i64::from(highest as i32),
            "lowestOneBit" => i64::from((u & u.wrapping_neg()) as i32),
            "reverse" => i64::from(u.reverse_bits() as i32),
            _ => i64::from(u.swap_bytes() as i32),
        })
    } else {
        let u = n as u64;
        let highest = if u == 0 {
            0
        } else {
            1u64 << (63 - u.leading_zeros())
        };
        Value::int(match method {
            "bitCount" => i64::from(u.count_ones()),
            "numberOfLeadingZeros" => i64::from(u.leading_zeros()),
            "numberOfTrailingZeros" => i64::from(u.trailing_zeros()),
            "highestOneBit" => highest as i64,
            "lowestOneBit" => (u & u.wrapping_neg()) as i64,
            "reverse" => u.reverse_bits() as i64,
            _ => u.swap_bytes() as i64,
        })
    }
}

/// `Math.ulp`: the distance from `f` to the next representable double away from
/// zero.
fn java_ulp(f: f64) -> f64 {
    if f.is_nan() {
        return f64::NAN;
    }
    if f.is_infinite() {
        return f64::INFINITY;
    }
    let a = f.abs();
    next_after(a, f64::INFINITY) - a
}

/// `Math.nextAfter`: the adjacent double in the direction of `toward`, walked
/// through the bit pattern the way the JDK does.
fn next_after(f: f64, toward: f64) -> f64 {
    if f.is_nan() || toward.is_nan() {
        return f64::NAN;
    }
    if f == toward {
        return toward;
    }
    if f == 0.0 {
        return if toward > 0.0 {
            f64::from_bits(1)
        } else {
            -f64::from_bits(1)
        };
    }
    let bits = f.to_bits();
    let up = (toward > f) == (f > 0.0);
    f64::from_bits(if up { bits + 1 } else { bits - 1 })
}

/// `Math.getExponent`: the unbiased exponent, with the JDK's answers for the
/// special cases (`MAX_EXPONENT + 1` for a NaN or infinity, `MIN_EXPONENT - 1`
/// for zero and the subnormals).
fn java_get_exponent(f: f64) -> i32 {
    if f.is_nan() || f.is_infinite() {
        return 1024;
    }
    let raw = ((f.to_bits() >> 52) & 0x7ff) as i32;
    if raw == 0 {
        -1023
    } else {
        raw - 1023
    }
}

/// Java's `(int)` cast applied to a `double` — what `Double.intValue()` does.
///
/// The cast saturates at the `int` bounds (it does **not** wrap), and NaN
/// becomes 0. Rust's `as i32` on an `f64` has the same saturating semantics, but
/// going through `as i64` first (which saturates at the *long* bounds) and
/// stopping there answers `10000000000` for `(1e10).intValue()` where Java
/// answers `2147483647`.
fn java_double_to_int(f: f64) -> i64 {
    i64::from(f as i32)
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
                Value::Array(a) => a.to_vec(),
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
            Some(HeapObj::OrderedMap {
                entries: m, index, ..
            }) => {
                m.retain(|(k, _)| keep(k));
                // Removing an entry shifts every later position, so the index
                // is rebuilt rather than patched. This is the one mutator that
                // has to, and it was already linear in the map's size.
                *index = m
                    .iter()
                    .enumerate()
                    .map(|(i, (k, _))| (k.clone(), i))
                    .collect();
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
    if let Some(found) = omap_get(recv, name) {
        return found.unwrap_or(Value::Undef);
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
        // Groovy's property-for-getter rule: `s.bytes` is `s.getBytes()`.
        (Value::Str(_), "bytes") => dispatch_method(vm, recv, "getBytes", &[]),
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
    //
    // Rendering can raise on its own account — printing a `subList` window whose
    // backing list moved on is a `ConcurrentModificationException` — and in a
    // program with no `try` in it that raise degrades to a hard fault instead of
    // a pending exception. A fault is an unconditional halt with no unwind path
    // to print on, so it suppresses the write outright rather than joining the
    // `already_unwinding` exemption above.
    let already_unwinding = pending_exc();
    let rendered: Vec<String> = vals.iter().map(|v| render_value(vm, v)).collect();
    if faulted() || (!already_unwinding && pending_exc()) {
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
        Value::Array(a) => a.to_vec(),
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
        || as_list_raw(v).is_some()
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
/// operand compares exactly (scale-insensitively), other numbers compare the way
/// `Double.compare` does, and anything else compares by its rendered form.
///
/// Two Rust names look right here and are not. `f64::partial_cmp` answers `None`
/// for a NaN — folding it to `Equal` left `[1.0d, NaN, 0.5d].sort()` unsorted,
/// where `Double.compare` makes NaN greater than everything and Groovy answers
/// `[0.5, 1.0, NaN]`. And `partial_cmp` reports `-0.0 == 0.0`, where
/// `Double.compare(-0.0, 0.0)` is `-1`. `f64::total_cmp` is the Rust spelling of
/// `Double.compare`, and it agrees on both.
///
/// `String::cmp` is UTF-8 **byte** order; Java's `String.compareTo` is UTF-16
/// **code-unit** order. They invert for an astral character against
/// `U+E000..U+FFFF`, so the fallback encodes before comparing.
fn natural_order(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (as_dec(a).is_some() || as_dec(b).is_some()).then(|| (as_exact_dec(a), as_exact_dec(b))) {
        Some((Some(x), Some(y))) => decimal::cmp(&x, &y),
        _ => match (as_num(a), as_num(b)) {
            (Some(x), Some(y)) => java_compare_f64(x, y),
            _ => utf16_cmp(&groovy_str(a), &groovy_str(b)),
        },
    }
}

/// `Double.compare`: NaN is greater than everything (including `+Infinity`) and
/// equal to itself whatever its sign bit, and `-0.0` sorts below `+0.0`.
///
/// `total_cmp` alone would order a *negative* NaN below `-Infinity`, and the
/// sign of the NaN `0.0d/0.0d` produces is platform-dependent.
fn java_compare_f64(x: f64, y: f64) -> std::cmp::Ordering {
    match (x.is_nan(), y.is_nan()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => x.total_cmp(&y),
    }
}

/// `String.compareTo`'s ordering: by UTF-16 code unit, then by length.
fn utf16_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
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

/// Whether `candidate` displaces `best` in a `max`/`min` scan.
///
/// With no closure Groovy's `max`/`min` over a collection do **not** use the
/// comparator its `sort` uses: `DefaultGroovyMethods.max`/`min` scan with the
/// primitive `>`/`<`, where every comparison against a NaN is `false`. So
/// `[Double.NaN, 1.0d].max()` and `.min()` both answer `NaN` — the first element
/// is never displaced — while `[1.0d, NaN, 0.5d].sort()` puts NaN last, because
/// sorting goes through `NumberAwareComparator` and `Double.compare`. The two
/// disagree in Groovy itself; reproducing one with the other gets `min` wrong.
fn prefers(
    vm: &mut VM,
    order: &OrderBy,
    candidate: &Value,
    best: &Value,
    want: std::cmp::Ordering,
) -> Result<bool, String> {
    // Only for a plain `Int`/`Float` pair: a `BigDecimal` operand still compares
    // exactly through [`natural_order`], and neither can be a NaN anyway.
    let plain = |v: &Value| matches!(v, Value::Int(_) | Value::Float(_));
    if matches!(order, OrderBy::Natural) && plain(candidate) && plain(best) {
        let (c, b) = (as_f64(candidate), as_f64(best));
        if c.is_nan() || b.is_nan() {
            return Ok(false);
        }
        return Ok(if want == std::cmp::Ordering::Greater {
            c > b
        } else {
            c < b
        });
    }
    Ok(order.apply(vm, candidate, best)? == want)
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
                if prefers(vm, order, it, &b, want)? {
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
    // A list reaches here in either representation — a literal rides a handle,
    // a GDK result is a transient `Value::Array` — and `List.plus` is the same
    // operation for both. Reading only the transient form left the handle form
    // to the string fallback below, which is why `[[1, 2], [3]].sum([])`
    // rendered `[][1, 2][3]` instead of concatenating to `[1, 2, 3]`.
    let (da, db) = (deref_list(a), deref_list(b));
    let (a, b) = (&da, &db);
    if let Value::Array(xs) = a {
        let mut out = xs.to_vec();
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
        // `Map.plus` clones the *left* operand and puts the right into it, so
        // the result is the left's implementation: `new TreeMap(…) + [d: 4]`
        // is a `TreeMap`, and `d` sorts into place rather than landing last.
        return gmap_kind(entries, omap_kind(a).unwrap_or(MapKind::Linked));
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
        Value::Array(xs) => {
            Value::array(std::iter::repeat(xs.to_vec()).take(n).flatten().collect())
        }
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
        // A set or a map on either side compares by *contents*: order-insensitive,
        // and never equal to a list. The rendered-form comparison below would get
        // both halves backwards, so it has to be decided before it.
        //
        // The map half is why `[b: 2, a: 1] == [a: 1, b: 2]` is true. Rendering
        // the two maps gives `[b:2, a:1]` and `[a:1, b:2]`, so the fallback
        // answered `false` — a silent wrong answer on plain map literals, and a
        // second one now that a `TreeMap` and a `LinkedHashMap` holding the same
        // entries render in different orders by design.
        NumOp::Eq | NumOp::Ne
            if as_set(a).is_some()
                || as_set(b).is_some()
                || as_omap(a).is_some()
                || as_omap(b).is_some() =>
        {
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

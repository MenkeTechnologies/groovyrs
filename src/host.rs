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
//!    where `+` routes through [`groovy_add`].

use fusevm::{Frame, NumOp, VMResult, Value, VM};
use std::cell::RefCell;

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
/// top. Dispatches a faithful GDK subset (see [`dispatch_method`]).
pub const GMETHOD: u16 = 705;
/// Builtin id for a Groovy property read `recv.name` (e.g. `list.size`,
/// `str.length`). The stack holds the receiver then the property name on top.
pub const GPROP: u16 = 706;
/// Builtin id for building a closure value. The stack holds the closure's
/// synthetic name-pool index and its parameter count (two integers); the builtin
/// registers them and returns a `Value::Obj` handle (see [`invoke_closure`]).
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
/// Builtin id for building a Groovy map literal `[k: v, …]`. The stack holds the
/// interleaved key/value pairs (key pushed first) with the entry count on top;
/// returns an insertion-ordered map handle (`Value::Obj`). Groovy maps preserve
/// insertion order (a `LinkedHashMap`), which fusevm's unordered `Hash` cannot,
/// so the map lives in the host heap instead.
pub const GMAKE_MAP: u16 = 715;
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
    field_names: Vec<String>,
    /// Field initializer thunks: name-pool index of a synthetic 0-arg subroutine
    /// that computes the initial value, per field that has an initializer.
    field_inits: Vec<(String, u16)>,
    /// method name → subroutine name-pool index.
    methods: std::collections::HashMap<String, u16>,
    /// constructor subroutine name-pool indices keyed by arity.
    ctors: std::collections::HashMap<u8, u16>,
}

/// Clear the object heap and class registry (called from [`install`]).
fn reset_heap() {
    HEAP.with(|h| h.borrow_mut().clear());
    CLASSES.with(|c| c.borrow_mut().clear());
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
        let key = flat[i].as_str_cow().into_owned();
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
    let meta = class_meta(cid).unwrap();
    // Allocate the instance (fields default to null), then run initializers.
    let mut fields = std::collections::HashMap::new();
    for f in &meta.field_names {
        fields.insert(f.clone(), Value::Undef);
    }
    let handle = heap_push(HeapObj::Instance(Instance { class: cid, fields }));
    for (fname, init_idx) in &meta.field_inits {
        match invoke_sub(vm, *init_idx, &[]) {
            Ok(v) => {
                set_instance_field(&handle, fname, v);
            }
            Err(e) => {
                fault(vm, e);
                return Value::Undef;
            }
        }
    }
    // Run the arity-matched constructor, if any (implicit `this` in slot 0).
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
    }
    handle
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
    if let Some(idx) = meta.methods.get(method) {
        let mut pushes = Vec::with_capacity(args.len() + 1);
        pushes.push(recv.clone());
        pushes.extend_from_slice(args);
        return Some(invoke_sub(vm, *idx, &pushes));
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
    if let Some(idx) = meta.methods.get(&getter) {
        return Some(invoke_sub(vm, *idx, std::slice::from_ref(recv)));
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
        if let Some(meta) = class_meta(inst.class) {
            let setter = format!("set{}", capitalize(&name));
            if let Some(idx) = meta.methods.get(&setter) {
                return match invoke_sub(vm, *idx, &[recv.clone(), value]) {
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
/// subset via [`dispatch_method`].
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
                    Ok(v) if v.is_truthy() => out.push(it.clone()),
                    Ok(_) => {}
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
                    Ok(v) if v.is_truthy() => return Some(Ok(it.clone())),
                    Ok(_) => {}
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
                        Ok(v) => v,
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

/// Add two values for `sum`: integer addition stays integral, any float operand
/// promotes to a float (Groovy's numeric-tower `+`).
fn groovy_sum_add(a: &Value, b: &Value) -> Value {
    match (as_i64(a), as_i64(b)) {
        (Some(x), Some(y)) => Value::int(x + y),
        _ => Value::float(as_f64(a) + as_f64(b)),
    }
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
        Value::Obj(_) if as_omap(v).is_some() => "Map",
        Value::Obj(_) if as_instance(v).is_some() => "Object",
        Value::Int(_) => "Integer",
        Value::Float(_) => "BigDecimal",
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
    let rendered: Vec<String> = vals.iter().map(|v| render_value(vm, v)).collect();
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
    match (as_i64(&a), as_i64(&b)) {
        // Both integers: exact → integer, else decimal.
        (Some(x), Some(y)) => {
            if y != 0 && x % y == 0 {
                Value::int(x / y)
            } else {
                Value::float(x as f64 / y as f64)
            }
        }
        // A decimal operand (or a non-integer numeric): decimal division.
        _ => {
            let x = as_f64(&a);
            let y = as_f64(&b);
            Value::float(x / y)
        }
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

/// A float view of a value for decimal arithmetic.
fn as_f64(v: &Value) -> f64 {
    match v {
        Value::Int(n) => *n as f64,
        Value::Float(f) => *f,
        Value::Bool(b) => *b as i64 as f64,
        _ => f64::NAN,
    }
}

/// Render a value for output, invoking a class instance's `toString()` (Groovy's
/// `println` prints an object through `toString`). Collections render their
/// elements the same way. Everything else defers to [`groovy_str`] (which has no
/// VM and so cannot dispatch a method). `default_instance_str` covers an instance
/// whose class defines no `toString`.
fn render_value(vm: &mut VM, v: &Value) -> String {
    if let Some(inst) = as_instance(v) {
        return instance_to_string(vm, v).unwrap_or_else(|| default_instance_str(&inst));
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
    let meta = class_meta(inst.class)?;
    let idx = *meta.methods.get("toString")?;
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

/// Render a value with Groovy's `println`/`toString` rules (as opposed to
/// fusevm's shell-flavoured `as_str_cow`): booleans as `true`/`false`, whole
/// decimals with a trailing `.0`, `Undef`/`null` as `null`.
pub fn groovy_str(v: &Value) -> String {
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
        Value::Float(f) => format_decimal(*f),
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

/// Groovy prints a whole `BigDecimal`/`double` with a trailing `.0` (`3.0`, not
/// `3`) and keeps a decimal point; non-finite `double`s print as
/// `Infinity`/`-Infinity`/`NaN`.
fn format_decimal(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_string()
    } else if f.is_infinite() {
        if f < 0.0 { "-Infinity" } else { "Infinity" }.to_string()
    } else if f.fract() == 0.0 && f.abs() < 1e16 {
        format!("{f}.0")
    } else {
        format!("{f}")
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
    Value::str(format!("{}{}", groovy_str(a), groovy_str(b)))
}

/// Strict numeric hook: fusevm calls this only for an operation with a
/// non-numeric operand — Groovy's `+` overload (list concat / map merge / string
/// concatenation) and value comparisons against strings. All-numeric arithmetic
/// never reaches here (it stays on the native fast path and the JIT). `/` never
/// reaches here — it lowers to the [`GDIV`] builtin instead.
pub fn numeric_hook(op: NumOp, a: &Value, b: &Value) -> Result<Value, String> {
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

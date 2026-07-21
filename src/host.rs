//! The groovyrs host: builtin registration, Groovy value formatting, the Groovy
//! `/` division builtin, and the strict numeric hook.
//!
//! groovyrs keeps no object heap of its own yet (slice 1 runs on the fusevm
//! value model directly). Three places need Groovy semantics that fusevm's
//! default awk/shell flavour does not provide:
//!
//! 1. **Printing.** fusevm's native `PrintLn` renders values shell-style
//!    (`true`→`1`, `3.0`→`3`). `println`/`print` instead lower to a registered
//!    builtin ([`GPRINTLN`]/[`GPRINT`]) that formats through [`groovy_str`] —
//!    `true`/`false`, `3.0`, `null` — matching Groovy.
//! 2. **`/` division.** Groovy divides two integers as `BigDecimal`, so `7/2`
//!    is `3.5`, not `3`. `/` lowers to [`GDIV`], which returns an integer only
//!    when the division is exact and a decimal otherwise.
//! 3. **`+` overloading.** Groovy's `+` is string concatenation when either
//!    operand is a `String`. fusevm runs *strict* once a numeric hook is
//!    installed, delegating any operation with a non-numeric operand to
//!    [`numeric_hook`], where `+` concatenates via the same [`groovy_str`].

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
    // A fresh VM install starts with an empty closure registry: handles are
    // chunk-relative (they carry a name-pool index), so a handle from a prior
    // run must never survive into a new chunk.
    reset_closures();
}

thread_local! {
    /// Registry of closure values created by [`b_make_closure`]. A `Value::Obj`
    /// handle indexes this vector; each entry is the closure body's synthetic
    /// name-pool index (resolved to the entry ip via `Chunk::find_sub` at call
    /// time) and its parameter count. Cleared on each [`install`] because the
    /// name index is only meaningful for the chunk that created it.
    static CLOSURES: RefCell<Vec<ClosureMeta>> = const { RefCell::new(Vec::new()) };
}

/// A registered closure: the body's name-pool index and its parameter count.
#[derive(Clone, Copy)]
struct ClosureMeta {
    name_idx: u16,
    params: u8,
}

/// Clear the closure registry (called from [`install`] on a fresh VM).
fn reset_closures() {
    CLOSURES.with(|c| c.borrow_mut().clear());
}

/// Look up a closure handle's metadata, if `v` is a closure value.
fn closure_meta(v: &Value) -> Option<ClosureMeta> {
    match v {
        Value::Obj(id) => CLOSURES.with(|c| c.borrow().get(*id as usize).copied()),
        _ => None,
    }
}

/// `GMAKE_CLOSURE`: pop the parameter count and name index, register the closure,
/// and return its `Value::Obj` handle.
fn b_make_closure(vm: &mut VM, _argc: u8) -> Value {
    let params = vm.stack.pop().unwrap_or(Value::Undef).to_int() as u8;
    let name_idx = vm.stack.pop().unwrap_or(Value::Undef).to_int() as u16;
    let id = CLOSURES.with(|c| {
        let mut c = c.borrow_mut();
        let id = c.len() as u32;
        c.push(ClosureMeta { name_idx, params });
        id
    });
    Value::Obj(id)
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
    // arguments with `null`, drop extras.
    let want = meta.params as usize;
    let stack_base = vm.stack.len();
    for i in 0..want {
        vm.stack.push(args.get(i).cloned().unwrap_or(Value::Undef));
    }
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
        _ => 0,
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
        _ => Err(format!(
            "groovyrs: no such property `{name}` on {}",
            type_name(recv)
        )),
    }
}

/// A short Groovy-ish type name for diagnostics.
fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Str(_) => "String",
        Value::Array(_) => "List",
        Value::Hash(_) => "Map",
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
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    for v in &vals {
        let _ = write!(lock, "{}", groovy_str(v));
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

/// Render a value with Groovy's `println`/`toString` rules (as opposed to
/// fusevm's shell-flavoured `as_str_cow`): booleans as `true`/`false`, whole
/// decimals with a trailing `.0`, `Undef`/`null` as `null`.
pub fn groovy_str(v: &Value) -> String {
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

/// Strict numeric hook: fusevm calls this only for an operation with a
/// non-numeric operand. In slice 1 that is Groovy's `String` `+` overload plus
/// value comparisons against strings; all-numeric arithmetic never reaches here
/// (it stays on the native fast path and the JIT). `/` never reaches here — it
/// lowers to the [`GDIV`] builtin instead.
pub fn numeric_hook(op: NumOp, a: &Value, b: &Value) -> Result<Value, String> {
    match op {
        // Groovy `+`: if either side is non-numeric (a String), concatenate
        // using Groovy's value-to-string rules.
        NumOp::Add => Ok(Value::str(format!("{}{}", groovy_str(a), groovy_str(b)))),
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

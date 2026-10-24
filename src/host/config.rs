//! `groovy.util.ConfigSlurper` and the `ConfigObject`s it builds.
//!
//! A config script is Groovy source run with a `ConfigObject` as the target
//! of every assignment: `a.b = 1` stores through nested objects, `name { … }`
//! runs the block against the nested object `name`, and an
//! `environments { env { … } }` block is applied only when the slurper was
//! built for that environment. groovyrs has no run-time compiler, so the
//! script is parsed and walked here; its values are the literal forms a
//! config writes — numbers, strings (interpolating earlier settings), lists,
//! maps, ranges, arithmetic and method calls on those — and any other
//! expression faults.
//!
//! A `ConfigObject` is a `LinkedHashMap` handle flavored with the class name.
//! Reading a key it lacks creates and stores an empty nested one, as
//! `ConfigObject.getProperty` does.

use super::*;
use crate::ast::{BinOp, Expr, GStringPart, StmtKind, UnOp};

pub(super) const CONFIG_OBJECT: &str = "groovy.util.ConfigObject";

/// A new, empty `ConfigObject`.
pub(super) fn new_config() -> Value {
    let map = gmap_kind(Vec::new(), MapKind::Linked);
    seq::set_flavor(&map, CONFIG_OBJECT, seq::Writes::Open);
    map
}

/// Whether `v` is a `ConfigObject`.
pub(super) fn is_config(v: &Value) -> bool {
    seq::flavor_class(v) == Some(CONFIG_OBJECT)
}

/// `config.key`: the entry, or a new empty `ConfigObject` stored under it.
pub(super) fn get_or_create(config: &Value, key: &str) -> Value {
    match omap_get(config, key) {
        Some(Some(v)) => return v,
        Some(None) => {}
        None => return Value::Undef,
    }
    let nested = new_config();
    omap_set(config, key.to_string(), nested.clone());
    nested
}

/// `new ConfigSlurper([environment])`.
pub(super) fn new_slurper(args: &[Value]) -> Option<Value> {
    let env = match args {
        [] => None,
        [e] => Some(groovy_str(e)),
        _ => return None,
    };
    Some(heap_push(HeapObj::ConfigSlurper(env)))
}

/// The environment a slurper handle was built for, if `v` is one.
pub(super) fn slurper_env(v: &Value) -> Option<Option<String>> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::ConfigSlurper(env)) => Some(env.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// One script run: the slurper's environment and the locals `def x = …`
/// declared.
struct Run {
    env: Option<String>,
    locals: HashMap<String, Value>,
}

/// `slurper.parse(text)`.
pub(super) fn parse(vm: &mut VM, env: Option<String>, text: &str) -> Value {
    let program = match crate::parser::parse(text) {
        Ok(p) => p,
        Err(e) => {
            fault(vm, e);
            return Value::Undef;
        }
    };
    let root = new_config();
    let mut run = Run {
        env,
        locals: HashMap::new(),
    };
    if let Err(e) = block(vm, &mut run, &program.body, std::slice::from_ref(&root)) {
        fault(vm, e);
        return Value::Undef;
    }
    root
}

/// Run `body` against the innermost of `scopes` (outer ones last-resort for
/// reads).
fn block(
    vm: &mut VM,
    run: &mut Run,
    body: &[crate::ast::Stmt],
    scopes: &[Value],
) -> Result<(), String> {
    let target = scopes.last().cloned().unwrap_or(Value::Undef);
    for s in body {
        match &s.kind {
            StmtKind::Local { name, init, .. } => {
                let v = match init {
                    Some(e) => eval(vm, run, e, scopes)?,
                    None => Value::Undef,
                };
                run.locals.insert(name.clone(), v);
            }
            StmtKind::Assign { name, value, .. } => {
                let v = eval(vm, run, value, scopes)?;
                if run.locals.contains_key(name) {
                    run.locals.insert(name.clone(), v);
                } else {
                    omap_set(&target, name.clone(), v);
                }
            }
            StmtKind::SetProperty {
                recv, name, value, ..
            } => {
                let owner = path(vm, run, recv, scopes)?;
                let v = eval(vm, run, value, scopes)?;
                omap_set(&owner, name.clone(), v);
            }
            StmtKind::Expr(Expr::Call { name, args, .. }) => match args.as_slice() {
                [Expr::Closure { body, .. }] if name == "environments" => {
                    let Some(env) = run.env.clone() else { continue };
                    for inner in body {
                        if let StmtKind::Expr(Expr::Call { name, args, .. }) = &inner.kind {
                            if *name == env {
                                if let [Expr::Closure { body, .. }] = args.as_slice() {
                                    block(vm, run, body, scopes)?;
                                }
                            }
                        }
                    }
                }
                [Expr::Closure { body, .. }] => {
                    let nested = get_or_create(&target, name);
                    let mut inner = scopes.to_vec();
                    inner.push(nested);
                    block(vm, run, body, &inner)?;
                }
                _ => {
                    eval(
                        vm,
                        run,
                        &Expr::Call {
                            name: name.clone(),
                            args: args.clone(),
                            line: s.line,
                        },
                        scopes,
                    )?;
                }
            },
            // `a.b { … }`: a block on a nested object.
            StmtKind::Expr(Expr::MethodCall {
                recv, method, args, ..
            }) if matches!(args.as_slice(), [Expr::Closure { .. }]) => {
                let owner = path(vm, run, recv, scopes)?;
                let nested = get_or_create(&owner, method);
                if let [Expr::Closure { body, .. }] = args.as_slice() {
                    let mut inner = scopes.to_vec();
                    inner.push(nested);
                    block(vm, run, body, &inner)?;
                }
            }
            StmtKind::Expr(e) => {
                eval(vm, run, e, scopes)?;
            }
            _ => {
                return Err(format!(
                    "groovyrs: ConfigSlurper does not run this statement on line {}",
                    s.line
                ))
            }
        }
    }
    Ok(())
}

/// The `ConfigObject` a dotted name designates, created as needed:
/// `app` in `app.name = …`, `a.b` in `a.b.c = …`.
fn path(vm: &mut VM, run: &mut Run, e: &Expr, scopes: &[Value]) -> Result<Value, String> {
    let target = scopes.last().cloned().unwrap_or(Value::Undef);
    match e {
        Expr::Var(n) if !run.locals.contains_key(n) => Ok(get_or_create(&target, n)),
        Expr::Property { recv, name, .. } => {
            let owner = path(vm, run, recv, scopes)?;
            Ok(get_or_create(&owner, name))
        }
        Expr::Recorded { inner, .. } => path(vm, run, inner, scopes),
        other => eval(vm, run, other, scopes),
    }
}

/// A variable read: a local, else the nearest scope holding the key, else a
/// new empty object in the current one.
fn read_var(run: &Run, name: &str, scopes: &[Value]) -> Value {
    if let Some(v) = run.locals.get(name) {
        return v.clone();
    }
    for s in scopes.iter().rev() {
        if let Some(Some(v)) = omap_get(s, name) {
            return v;
        }
    }
    get_or_create(scopes.last().unwrap_or(&Value::Undef), name)
}

fn eval(vm: &mut VM, run: &mut Run, e: &Expr, scopes: &[Value]) -> Result<Value, String> {
    Ok(match e {
        Expr::Int(n, _) => Value::int(*n),
        Expr::Float(f) => Value::float(*f),
        Expr::Single(f) => float_value(*f),
        Expr::Dec(t) => dec_value(crate::decimal::parse(t).ok_or("bad decimal literal")?),
        Expr::BigInt(t) => bigint_value(crate::decimal::parse(t).ok_or("bad integer literal")?),
        Expr::Str(s) => Value::str(s.clone()),
        Expr::Bool(b) => Value::bool(*b),
        Expr::Null => Value::Undef,
        Expr::Recorded { inner, .. } => eval(vm, run, inner, scopes)?,
        Expr::GString(parts) => {
            let mut out = String::new();
            for p in parts {
                match p {
                    GStringPart::Text(t) => out.push_str(t),
                    GStringPart::Expr(x) => {
                        let v = eval(vm, run, x, scopes)?;
                        out.push_str(&render_value(vm, &v));
                    }
                }
            }
            Value::str(out)
        }
        Expr::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for i in items {
                out.push(eval(vm, run, i, scopes)?);
            }
            glist(out)
        }
        Expr::Map(entries) => {
            let mut out = Vec::with_capacity(entries.len());
            for (k, v) in entries {
                let key = eval(vm, run, k, scopes)?;
                out.push((mapkey::encode(&key), eval(vm, run, v, scopes)?));
            }
            gmap_kind(out, MapKind::Linked)
        }
        Expr::Range {
            start,
            end,
            inclusive,
            exclusive_left,
        } => {
            let from = eval(vm, run, start, scopes)?;
            let to = eval(vm, run, end, scopes)?;
            let r = RangeVal {
                from,
                to,
                inclusive: *inclusive,
                long: false,
                excl_left: false,
                step: None,
            };
            let r = if *exclusive_left {
                exclusive_left_range(r.from, r.to, r.inclusive, false)
            } else {
                r
            };
            heap_push(HeapObj::Range(r))
        }
        Expr::Var(n) => read_var(run, n, scopes),
        Expr::Property { recv, name, .. } => {
            let r = eval(vm, run, recv, scopes)?;
            if is_config(&r) {
                get_or_create(&r, name)
            } else {
                get_property(vm, &r, name)
            }
        }
        Expr::Unary { op: UnOp::Neg, rhs } => {
            let v = eval(vm, run, rhs, scopes)?;
            match v {
                Value::Int(n) => Value::int(n.wrapping_neg()),
                Value::Float(f) => Value::float(-f),
                other => numeric_hook(NumOp::Neg, &other, &Value::Undef)?,
            }
        }
        Expr::Unary { op: UnOp::Not, rhs } => {
            let v = eval(vm, run, rhs, scopes)?;
            Value::bool(!groovy_truthy(vm, &v))
        }
        Expr::Binary { op, lhs, rhs } => {
            let a = eval(vm, run, lhs, scopes)?;
            match op {
                BinOp::And if !groovy_truthy(vm, &a) => return Ok(Value::bool(false)),
                BinOp::Or if groovy_truthy(vm, &a) => return Ok(Value::bool(true)),
                _ => {}
            }
            let b = eval(vm, run, rhs, scopes)?;
            let num = match op {
                BinOp::Add => NumOp::Add,
                BinOp::Sub => NumOp::Sub,
                BinOp::Mul => NumOp::Mul,
                BinOp::Div => return Ok(divide_values(vm, a, b)),
                BinOp::Shl if !matches!(a, Value::Int(_)) => {
                    return Ok(dispatch_call(vm, a, "leftShift", vec![b]));
                }
                BinOp::Mod => NumOp::Mod,
                BinOp::Power => NumOp::Pow,
                BinOp::Eq => NumOp::Eq,
                BinOp::Ne => NumOp::Ne,
                BinOp::Lt => NumOp::Lt,
                BinOp::Gt => NumOp::Gt,
                BinOp::Le => NumOp::Le,
                BinOp::Ge => NumOp::Ge,
                BinOp::And | BinOp::Or => return Ok(Value::bool(groovy_truthy(vm, &b))),
                _ => return Err("groovyrs: ConfigSlurper does not evaluate this operator".into()),
            };
            // Two integers are the VM's own arithmetic; the hook answers
            // the rest (decimals, strings, division).
            if let (Value::Int(x), Value::Int(y)) = (&a, &b) {
                let (x, y) = (*x, *y);
                let native = match num {
                    NumOp::Add => Some(Value::int(x.wrapping_add(y))),
                    NumOp::Sub => Some(Value::int(x.wrapping_sub(y))),
                    NumOp::Mul => Some(Value::int(x.wrapping_mul(y))),
                    NumOp::Mod if y != 0 => Some(Value::int(x % y)),
                    NumOp::Eq => Some(Value::bool(x == y)),
                    NumOp::Ne => Some(Value::bool(x != y)),
                    NumOp::Lt => Some(Value::bool(x < y)),
                    NumOp::Gt => Some(Value::bool(x > y)),
                    NumOp::Le => Some(Value::bool(x <= y)),
                    NumOp::Ge => Some(Value::bool(x >= y)),
                    _ => None,
                };
                if let Some(v) = native {
                    return Ok(v);
                }
            }
            numeric_hook(num, &a, &b)?
        }
        Expr::Elvis { lhs, rhs } => {
            let v = eval(vm, run, lhs, scopes)?;
            if groovy_truthy(vm, &v) {
                v
            } else {
                eval(vm, run, rhs, scopes)?
            }
        }
        Expr::Ternary { cond, then, els } => {
            let c = eval(vm, run, cond, scopes)?;
            if groovy_truthy(vm, &c) {
                eval(vm, run, then, scopes)?
            } else {
                eval(vm, run, els, scopes)?
            }
        }
        Expr::MethodCall {
            recv, method, args, ..
        } => {
            let r = eval(vm, run, recv, scopes)?;
            let mut vals = Vec::with_capacity(args.len());
            for a in args {
                vals.push(eval(vm, run, a, scopes)?);
            }
            dispatch_call(vm, r, method, vals)
        }
        _ => return Err("groovyrs: ConfigSlurper does not evaluate this expression".into()),
    })
}

/// `ConfigObject.flatten()`: every leaf under its dotted path, depth first;
/// an empty nested object contributes nothing.
fn flatten_into(config: &Value, prefix: &str, out: &mut Vec<(String, Value)>) {
    for (k, v) in as_omap(config).unwrap_or_default() {
        let key = mapkey::text(&k);
        let path = if prefix.is_empty() {
            key
        } else {
            format!("{prefix}.{key}")
        };
        if is_config(&v) {
            flatten_into(&v, &path, out);
        } else {
            out.push((path, v));
        }
    }
}

/// Merge `other` into `into`, nested objects recursively.
fn merge(into: &Value, other: &Value) {
    for (k, v) in as_omap(other).unwrap_or_default() {
        let key = mapkey::text(&k);
        match omap_get(into, &key).flatten() {
            Some(mine) if is_config(&mine) && is_config(&v) => merge(&mine, &v),
            _ => {
                omap_set(into, key, v);
            }
        }
    }
}

/// The `ConfigSlurper` and `ConfigObject` methods.
pub(super) fn dispatch(vm: &mut VM, recv: &Value, method: &str, args: &[Value]) -> Option<Value> {
    if let Some(env) = slurper_env(recv) {
        return match (method, args) {
            ("parse", [src]) => {
                let text = match file::file_path(src) {
                    Some(path) => std::fs::read_to_string(&path).ok()?,
                    None => groovy_str(src),
                };
                Some(parse(vm, env, &text))
            }
            ("getEnvironment", []) => Some(env.map(Value::str).unwrap_or(Value::Undef)),
            _ => None,
        };
    }
    if !is_config(recv) {
        return None;
    }
    Some(match (method, args) {
        ("flatten", []) => {
            let mut out = Vec::new();
            flatten_into(recv, "", &mut out);
            let map = gmap_kind(out, MapKind::Linked);
            seq::set_flavor(&map, CONFIG_OBJECT, seq::Writes::Open);
            map
        }
        // A `java.util.Properties` of the flattened leaves as strings, in its
        // hash table's order.
        ("toProperties", []) => {
            let mut out = Vec::new();
            flatten_into(recv, "", &mut out);
            let entries: Vec<(String, Value)> = out
                .into_iter()
                .map(|(k, v)| {
                    let text = render_value(vm, &v);
                    (k, Value::str(text))
                })
                .collect();
            let map = gmap_kind(
                entries,
                MapKind::Hash {
                    req: DEFAULT_HASH_REQ,
                },
            );
            seq::set_flavor(&map, "java.util.Properties", seq::Writes::Open);
            map
        }
        ("isSet", [name]) => {
            let found = omap_get(recv, &groovy_str(name));
            Value::bool(match found {
                Some(Some(v)) if is_config(&v) => omap_len(&v).unwrap_or(0) > 0,
                Some(_) => true,
                None => false,
            })
        }
        ("merge", [other]) => {
            merge(recv, other);
            recv.clone()
        }
        ("getProperty", [name]) => get_or_create(recv, &groovy_str(name)),
        _ => return None,
    })
}

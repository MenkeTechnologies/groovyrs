//! `groovy.util.ConfigSlurper` and the `ConfigObject`s it builds.
//!
//! A config script is Groovy source run with a `ConfigObject` as the target
//! of every assignment: `a.b = 1` stores through nested objects, `name { … }`
//! runs the block against the nested object `name`, and an
//! `environments { env { … } }` block is applied only when the slurper was
//! built for that environment. groovyrs has no run-time compiler, so the
//! script is parsed and walked here; its values are the literal forms a
//! config writes — numbers, strings (interpolating earlier settings), lists,
//! maps, ranges, arithmetic and method calls on those — and closures, whose
//! bodies are walked the same way when called; any other expression faults.
//!
//! A `ConfigObject` is a `LinkedHashMap` handle flavored with the class name.
//! Reading a key it lacks creates and stores an empty nested one, as
//! `ConfigObject.getProperty` does.

use super::*;
use crate::ast::{BinOp, Expr, GStringPart, Stmt, StmtKind, UnOp};

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

/// One script run: the slurper's environment, the locals `def x = …`
/// declared, and the class name each closure literal of the script has.
struct Run {
    env: Option<String>,
    locals: HashMap<String, Value>,
    names: std::rc::Rc<ClosureNames>,
    /// The settings the active environment's block made: Groovy runs it
    /// against a config object of its own and merges that into the result
    /// after the script.
    env_settings: Option<Value>,
    /// The script object (`Script_<md5>`): every closure's `thisObject`, and
    /// the `owner` of one written at the top level.
    script: Value,
    /// The closure the statements being walked are written in — a block's
    /// closure or a called closure — which owns a closure written there.
    owner: Option<Value>,
}

/// A closure literal's generated class name, by the address of its body.
type ClosureNames = HashMap<usize, String>;

/// A closure literal written in a config script. groovyrs has no run-time
/// compiler, so its body is walked here when it is called, against the
/// locals and the config objects in scope where it was written.
pub(super) struct ConfigClosure {
    class: String,
    params: Vec<String>,
    implicit_it: bool,
    body: &'static [Stmt],
    env: Option<String>,
    locals: HashMap<String, Value>,
    scopes: Vec<Value>,
    names: std::rc::Rc<ClosureNames>,
    script: Value,
}

impl ConfigClosure {
    /// The parameter count a call binds: the implicit `it` counts as one.
    pub(super) fn arity(&self) -> usize {
        if self.implicit_it {
            1
        } else {
            self.params.len()
        }
    }

    /// The fewest arguments a direct call may pass.
    pub(super) fn min_arity(&self) -> usize {
        if self.implicit_it {
            0
        } else {
            self.params.len()
        }
    }

    /// The class Groovy generates for the literal.
    pub(super) fn class_name(&self) -> &str {
        &self.class
    }
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
    // A closure the script stores outlives this call and walks its body when
    // called, so the program stays alive for the rest of the run.
    let program: &'static crate::ast::Program = Box::leak(Box::new(program));
    let root = new_config();
    let names = closure_names(text, &program.body);
    let mut run = Run {
        env,
        locals: HashMap::new(),
        names: std::rc::Rc::new(names),
        env_settings: None,
        script: heap_push(HeapObj::ConfigScript(format!(
            "Script_{}",
            script_hash(text)
        ))),
        owner: None,
    };
    if let Err(e) = block(vm, &mut run, &program.body, std::slice::from_ref(&root)) {
        fault(vm, e);
        return Value::Undef;
    }
    if let Some(env) = &run.env_settings {
        merge(&root, env);
    }
    root
}

/// The class name of every closure literal in a config script, as Groovy's
/// class generation assigns them: `ConfigSlurper.parse(String)` compiles the
/// text as `Script_<md5 of the text>`, the closures written directly in its
/// body are `$_run_closure1`, `2`, … in source order, and each closure class
/// then numbers the closures written directly in its own body
/// (`$_run_closure2$_closure4`), one global count, a class's children before
/// its grandchildren.
/// The md5 of a config script's text, in hex: what its class is named after.
fn script_hash(text: &str) -> String {
    digest::md5(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn closure_names(text: &str, body: &'static [Stmt]) -> ClosureNames {
    let hex = script_hash(text);
    let mut names = ClosureNames::new();
    let mut count = 0;
    let mut name_level = |parent: &str, kids: Vec<&'static [Stmt]>, names: &mut ClosureNames| {
        kids.into_iter()
            .map(|kid| {
                count += 1;
                let name = if parent.is_empty() {
                    format!("Script_{hex}$_run_closure{count}")
                } else {
                    format!("{parent}$_closure{count}")
                };
                names.insert(kid.as_ptr() as usize, name.clone());
                (name, kid)
            })
            .collect::<Vec<_>>()
    };
    let mut stack = name_level("", direct_closures(body), &mut names);
    stack.reverse();
    while let Some((name, kid)) = stack.pop() {
        let mut level = name_level(&name, direct_closures(kid), &mut names);
        level.reverse();
        stack.extend(level);
    }
    names
}

/// The bodies of the closure literals written directly in `body` (not inside
/// a nested closure), in source order.
fn direct_closures(body: &'static [Stmt]) -> Vec<&'static [Stmt]> {
    fn stmt(s: &'static Stmt, out: &mut Vec<&'static [Stmt]>) {
        match &s.kind {
            StmtKind::Local { init: Some(e), .. }
            | StmtKind::Expr(e)
            | StmtKind::Return { value: Some(e) } => expr(e, out),
            StmtKind::Assign { value, .. } => expr(value, out),
            StmtKind::SetProperty { recv, value, .. } => {
                expr(recv, out);
                expr(value, out);
            }
            StmtKind::If { cond, then, els } => {
                expr(cond, out);
                then.iter().chain(els).for_each(|s| stmt(s, out));
            }
            _ => {}
        }
    }
    fn expr(e: &'static Expr, out: &mut Vec<&'static [Stmt]>) {
        match e {
            Expr::Closure { body, .. } => out.push(body),
            Expr::Recorded { inner, .. } => expr(inner, out),
            Expr::GString(parts) => parts.iter().for_each(|p| {
                if let GStringPart::Expr(x) = p {
                    expr(x, out);
                }
            }),
            Expr::List(items) => items.iter().for_each(|i| expr(i, out)),
            Expr::Map(entries) => entries.iter().for_each(|(k, v)| {
                expr(k, out);
                expr(v, out);
            }),
            Expr::Range { start, end, .. } => {
                expr(start, out);
                expr(end, out);
            }
            Expr::Property { recv, .. } | Expr::Unary { rhs: recv, .. } => expr(recv, out),
            Expr::Binary { lhs, rhs, .. } | Expr::Elvis { lhs, rhs } => {
                expr(lhs, out);
                expr(rhs, out);
            }
            Expr::Ternary { cond, then, els } => {
                expr(cond, out);
                expr(then, out);
                expr(els, out);
            }
            Expr::Index { recv, index, .. } => {
                expr(recv, out);
                expr(index, out);
            }
            Expr::MethodCall { recv, args, .. } => {
                expr(recv, out);
                args.iter().for_each(|a| expr(a, out));
            }
            Expr::Call { args, .. } => args.iter().for_each(|a| expr(a, out)),
            Expr::CallValue { callee, args, .. } => {
                expr(callee, out);
                args.iter().for_each(|a| expr(a, out));
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    body.iter().for_each(|s| stmt(s, &mut out));
    out
}

/// Call a config-script closure: its parameters (or `it`) bound over the
/// locals it was written among, its body walked statement by statement, the
/// last expression statement's value — or a `return`'s — its result.
pub(super) fn call_closure(
    vm: &mut VM,
    this: &Value,
    clo: &ConfigClosure,
    args: &[Value],
) -> Result<Value, String> {
    let mut locals = clo.locals.clone();
    if clo.implicit_it {
        locals.insert(
            "it".to_string(),
            args.first().cloned().unwrap_or(Value::Undef),
        );
    } else {
        for (i, p) in clo.params.iter().enumerate() {
            locals.insert(p.clone(), args.get(i).cloned().unwrap_or(Value::Undef));
        }
    }
    let mut run = Run {
        env: clo.env.clone(),
        locals,
        names: clo.names.clone(),
        env_settings: None,
        script: clo.script.clone(),
        owner: Some(this.clone()),
    };
    Ok(match body_value(vm, &mut run, clo.body, &clo.scopes)? {
        Flow::Next(v) | Flow::Return(v) => v,
    })
}

/// How a closure body's statements end.
enum Flow {
    /// It ran to its end; the value of its last expression statement.
    Next(Value),
    /// A `return` left it.
    Return(Value),
}

/// Walk a closure body (or one branch of an `if` in it).
fn body_value(
    vm: &mut VM,
    run: &mut Run,
    body: &'static [Stmt],
    scopes: &[Value],
) -> Result<Flow, String> {
    let mut last = Value::Undef;
    for s in body {
        last = Value::Undef;
        match &s.kind {
            StmtKind::Return { value } => {
                let v = match value {
                    Some(e) => eval(vm, run, e, scopes)?,
                    None => Value::Undef,
                };
                return Ok(Flow::Return(v));
            }
            StmtKind::If { cond, then, els } => {
                let c = eval(vm, run, cond, scopes)?;
                let branch = if groovy_truthy(vm, &c) { then } else { els };
                match body_value(vm, run, branch, scopes)? {
                    Flow::Next(v) => last = v,
                    ret => return Ok(ret),
                }
            }
            StmtKind::Expr(Expr::MethodCall {
                recv, method, args, ..
            }) if matches!(args.as_slice(), [Expr::Closure { .. }]) => {
                last = closure_arg_call(vm, run, recv, method, &args[0], scopes)?;
            }
            StmtKind::Expr(e) if !is_block_call(e) => last = eval(vm, run, e, scopes)?,
            _ => block(vm, run, std::slice::from_ref(s), scopes)?,
        }
    }
    Ok(Flow::Next(last))
}

/// `name { … }`: a statement that runs a block on a nested config object
/// rather than computing a value.
fn is_block_call(e: &Expr) -> bool {
    matches!(e, Expr::Call { args, .. } if matches!(args.as_slice(), [Expr::Closure { .. }]))
}

/// Run `body` against the innermost of `scopes` (outer ones last-resort for
/// reads).
fn block(
    vm: &mut VM,
    run: &mut Run,
    body: &'static [Stmt],
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
            StmtKind::Expr(call @ Expr::Call { name, args, .. }) => match args.as_slice() {
                [envs @ Expr::Closure { body, .. }] if name == "environments" => {
                    let Some(env) = run.env.clone() else { continue };
                    let envs_closure = eval(vm, run, envs, scopes)?;
                    let prev = run.owner.replace(envs_closure);
                    for inner in body {
                        if let StmtKind::Expr(Expr::Call { name, args, .. }) = &inner.kind {
                            if *name == env {
                                if let [lit @ Expr::Closure { body, .. }] = args.as_slice() {
                                    let settings =
                                        run.env_settings.get_or_insert_with(new_config).clone();
                                    let mut inner = scopes.to_vec();
                                    inner.push(settings);
                                    run_block(vm, run, lit, body, &inner, scopes)?;
                                }
                            }
                        }
                    }
                    run.owner = prev;
                }
                [lit @ Expr::Closure { body, .. }] => {
                    let nested = get_or_create(&target, name);
                    let mut inner = scopes.to_vec();
                    inner.push(nested);
                    run_block(vm, run, lit, body, &inner, scopes)?;
                }
                _ => {
                    eval(vm, run, call, scopes)?;
                }
            },
            StmtKind::Expr(Expr::MethodCall {
                recv, method, args, ..
            }) if matches!(args.as_slice(), [Expr::Closure { .. }]) => {
                closure_arg_call(vm, run, recv, method, &args[0], scopes)?;
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
fn path(vm: &mut VM, run: &mut Run, e: &'static Expr, scopes: &[Value]) -> Result<Value, String> {
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

fn eval(vm: &mut VM, run: &mut Run, e: &'static Expr, scopes: &[Value]) -> Result<Value, String> {
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
            // The operators compiled code runs as a builtin take the same
            // builtin here, its operands on the stack.
            let builtin: Option<(Builtin, bool)> = match op {
                BinOp::Cmp => Some((b_cmp, false)),
                BinOp::In => Some((b_in, false)),
                BinOp::Match => Some((b_match, false)),
                BinOp::MatchFull => Some((b_match_full, false)),
                BinOp::Power => Some((b_power, true)),
                _ => None,
            };
            if let Some((f, width)) = builtin {
                vm.stack.push(a);
                vm.stack.push(b);
                if width {
                    // Neither operand is statically a `Long` in a config value.
                    vm.stack.push(Value::int(0));
                }
                return Ok(f(vm, 2 + u8::from(width)));
            }
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
        Expr::Closure {
            params,
            body,
            explicit_params,
            ..
        } => {
            let class = run
                .names
                .get(&(body.as_ptr() as usize))
                .cloned()
                .unwrap_or_default();
            let clo = ConfigClosure {
                class,
                params: params.clone(),
                implicit_it: params.is_empty() && !explicit_params,
                body,
                env: run.env.clone(),
                locals: run.locals.clone(),
                scopes: scopes.to_vec(),
                names: run.names.clone(),
                script: run.script.clone(),
            };
            let value = config_closure_value(clo);
            // A closure written at the top level is the script's; one written
            // in a block or a closure belongs to that closure.
            let owner = run.owner.clone().unwrap_or_else(|| run.script.clone());
            set_closure_owner(&value, owner, run.script.clone());
            value
        }
        Expr::Index { recv, index, .. } => {
            let r = eval(vm, run, recv, scopes)?;
            let i = eval(vm, run, index, scopes)?;
            dispatch_call(vm, r, "getAt", vec![i])
        }
        // `f(args)` for a closure the script holds in a local or a setting.
        Expr::Call { name, args, .. } => {
            let f = run
                .locals
                .get(name)
                .cloned()
                .or_else(|| {
                    scopes
                        .iter()
                        .rev()
                        .find_map(|s| omap_get(s, name).flatten())
                })
                .unwrap_or(Value::Undef);
            if closure_meta(&f).is_none() {
                return Err("groovyrs: ConfigSlurper does not evaluate this expression".into());
            }
            let mut vals = Vec::with_capacity(args.len());
            for a in args {
                vals.push(eval(vm, run, a, scopes)?);
            }
            dispatch_call(vm, f, "call", vals)
        }
        _ => return Err("groovyrs: ConfigSlurper does not evaluate this expression".into()),
    })
}

/// `ConfigObject.flatten()`: every leaf under its dotted path, depth first,
/// through nested config objects and plain maps alike.
fn flatten_into(config: &Value, prefix: &str, out: &mut Vec<(String, Value)>) {
    for (k, v) in as_omap(config).unwrap_or_default() {
        let key = mapkey::text(&k);
        let path = if prefix.is_empty() {
            key
        } else {
            format!("{prefix}.{key}")
        };
        // `ConfigObject.flatten` descends into every map, and an empty one
        // contributes nothing.
        match as_omap(&v) {
            Some(entries) if entries.is_empty() => {}
            Some(_) => flatten_into(&v, &path, out),
            None => out.push((path, v)),
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

/// A host builtin as the VM registers it.
type Builtin = fn(&mut VM, u8) -> Value;

/// A closure value over a config-script literal.
fn config_closure_value(clo: ConfigClosure) -> Value {
    let params = clo.arity() as u8;
    derived_closure(params, false, Derived::Config(std::rc::Rc::new(clo)))
}

/// `a.b { … }` as a statement: a block on the nested object `b` when `a` is a
/// config object, else the method `b` called on `a` with the closure. The
/// call's value, or `null` for a block.
fn closure_arg_call(
    vm: &mut VM,
    run: &mut Run,
    recv: &'static Expr,
    method: &str,
    arg: &'static Expr,
    scopes: &[Value],
) -> Result<Value, String> {
    let owner = path(vm, run, recv, scopes)?;
    if !is_config(&owner) {
        let clo = eval(vm, run, arg, scopes)?;
        return Ok(dispatch_call(vm, owner, method, vec![clo]));
    }
    let nested = get_or_create(&owner, method);
    if let Expr::Closure { body, .. } = arg {
        let mut inner = scopes.to_vec();
        inner.push(nested);
        run_block(vm, run, arg, body, &inner, scopes)?;
    }
    Ok(Value::Undef)
}

/// Walk a block's body (`name { … }`) with its closure as the owner of the
/// closures written in it.
fn run_block(
    vm: &mut VM,
    run: &mut Run,
    lit: &'static Expr,
    body: &'static [Stmt],
    inner: &[Value],
    scopes: &[Value],
) -> Result<(), String> {
    let closure = eval(vm, run, lit, scopes)?;
    let prev = run.owner.replace(closure);
    let out = block(vm, run, body, inner);
    run.owner = prev;
    out
}

/// Set a closure's `owner` and `thisObject` (its `delegate` defaults to the
/// owner).
fn set_closure_owner(clo: &Value, owner: Value, this: Value) {
    if let Value::Obj(id) = clo {
        HEAP.with(|h| {
            if let Some(HeapObj::Closure(meta)) = h.borrow_mut().get_mut(*id as usize) {
                meta.owner = Some(owner);
                meta.this_obj = Some(this);
            }
        });
    }
}

/// The class of a config script object, if `v` is one.
pub(super) fn script_class_of(v: &Value) -> Option<String> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::ConfigScript(class)) => Some(class.clone()),
            _ => None,
        }),
        _ => None,
    }
}

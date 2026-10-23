//! `groovy.xml.MarkupBuilder`: every call on the builder (or, inside a
//! builder closure, every bare call) writes an element to the builder's
//! writer as it is made — `name(attrs)`, `name(text)`, `name { children }` —
//! indented two spaces per level, attributes single-quoted, the way Apache
//! Groovy 6.0.0's `IndentPrinter` lays it out.

use super::*;

/// The builder's writer, nesting depth and whether anything was written.
fn state(v: &Value) -> Option<(Value, usize, bool)> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::MarkupBuilder {
                writer,
                depth,
                started,
            }) => Some(((**writer).clone(), *depth, *started)),
            _ => None,
        }),
        _ => None,
    }
}

fn set_state(v: &Value, new_depth: usize, new_started: bool) {
    if let Value::Obj(id) = v {
        HEAP.with(|h| {
            if let Some(HeapObj::MarkupBuilder { depth, started, .. }) =
                h.borrow_mut().get_mut(*id as usize)
            {
                *depth = new_depth;
                *started = new_started;
            }
        });
    }
}

/// Whether `v` is a `MarkupBuilder`.
pub(super) fn is_builder(v: &Value) -> bool {
    state(v).is_some()
}

/// `new MarkupBuilder()` (to standard output) / `new MarkupBuilder(writer)`.
pub(super) fn new_builder(args: &[Value]) -> Option<Value> {
    let writer = match args {
        [] => heap_push(HeapObj::PrintStream { err: false }),
        [w] => w.clone(),
        _ => return None,
    };
    Some(heap_push(HeapObj::MarkupBuilder {
        writer: Box::new(writer),
        depth: 0,
        started: false,
    }))
}

fn write(vm: &mut VM, writer: &Value, text: &str) {
    let method = if as_print_stream(writer).is_some() {
        "print"
    } else {
        "write"
    };
    dispatch_call(
        vm,
        writer.clone(),
        method,
        vec![Value::str(text.to_string())],
    );
}

/// Text content, escaped: `&`, `<`, `>`.
fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// An attribute value, escaped for its single quotes as well.
fn escape_attr(s: &str) -> String {
    escape_text(s).replace('\'', "&apos;")
}

/// One builder call: the element `name` written with what its arguments say
/// — a map is its attributes, a closure its children (run with the builder
/// as delegate), anything else its text.
pub(super) fn element(vm: &mut VM, builder: &Value, name: &str, args: &[Value]) -> Value {
    let Some((writer, depth, started)) = state(builder) else {
        return Value::Undef;
    };
    let mut attrs = String::new();
    let mut text: Option<String> = None;
    let mut body: Option<Value> = None;
    for a in args {
        if let Some(entries) = as_omap(a) {
            for (k, v) in entries {
                let value = render_value(vm, &v);
                attrs.push_str(&format!(" {}='{}'", mapkey::text(&k), escape_attr(&value)));
            }
        } else if closure_meta(a).is_some() {
            body = Some(a.clone());
        } else {
            text = Some(render_value(vm, a));
        }
    }
    let indent = "  ".repeat(depth);
    let lead = if started { "\n" } else { "" };
    match (body, text) {
        (Some(clo), text) => {
            write(vm, &writer, &format!("{lead}{indent}<{name}{attrs}>"));
            if let Some(t) = text {
                write(vm, &writer, &escape_text(&t));
            }
            set_state(builder, depth + 1, true);
            DELEGATES.with(|d| d.borrow_mut().push(builder.clone()));
            let out = invoke_closure(vm, &clo, &[]);
            DELEGATES.with(|d| d.borrow_mut().pop());
            set_state(builder, depth, true);
            if let Err(e) = out {
                fault(vm, e);
            }
            write(vm, &writer, &format!("\n{indent}</{name}>"));
        }
        (None, Some(t)) => {
            write(
                vm,
                &writer,
                &format!("{lead}{indent}<{name}{attrs}>{}</{name}>", escape_text(&t)),
            );
            set_state(builder, depth, true);
        }
        (None, None) => {
            write(vm, &writer, &format!("{lead}{indent}<{name}{attrs} />"));
            set_state(builder, depth, true);
        }
    }
    Value::Undef
}

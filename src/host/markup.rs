//! `groovy.xml.MarkupBuilder`: every call on the builder (or, inside a
//! builder closure, every bare call) is an element — `name(attrs)`,
//! `name(text)`, `name { children }` — laid out the way Apache Groovy 6.0.0's
//! `IndentPrinter` lays it out: two spaces per level, attributes
//! single-quoted, `<x />` for an element with no content, a closing tag on its
//! own line only after a child element. `mkp.yield` / `yieldUnescaped` /
//! `comment` write inline content, and the `doubleQuotes`,
//! `escapeAttributes` and `omitNullAttributes` options are honoured.
//!
//! A top-level element is built in memory and written to the writer when it
//! closes, which is what lets an empty closure still become `<x />`.

use super::*;

/// One open element: whether its start tag still lacks its `>` (nothing has
/// been written inside it yet), and whether a child element was written.
#[derive(Clone, Default)]
struct Frame {
    pending: bool,
    child_element: bool,
}

/// A builder's writer, options and the element being built.
#[derive(Clone)]
pub(super) struct MarkupState {
    writer: Value,
    out: String,
    frames: Vec<Frame>,
    started: bool,
    /// A processing instruction just ended its own line, so the next element
    /// starts there unindented (Groovy's `state = 2` after `pi`).
    after_pi: bool,
    double_quotes: bool,
    escape_attributes: bool,
    omit_null_attributes: bool,
}

fn with_state<R>(v: &Value, f: impl FnOnce(&mut MarkupState) -> R) -> Option<R> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow_mut().get_mut(*id as usize) {
            Some(HeapObj::MarkupBuilder(state)) => Some(f(state)),
            _ => None,
        }),
        _ => None,
    }
}

/// Whether `v` is a `MarkupBuilder`.
pub(super) fn is_builder(v: &Value) -> bool {
    with_state(v, |_| ()).is_some()
}

/// The builder an `mkp` helper belongs to.
pub(super) fn helper_builder(v: &Value) -> Option<Value> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::MarkupHelper(b)) => Some((**b).clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// `new MarkupBuilder()` (to standard output) / `new MarkupBuilder(writer)`.
pub(super) fn new_builder(args: &[Value]) -> Option<Value> {
    let writer = match args {
        [] => heap_push(HeapObj::PrintStream { err: false }),
        [w] => w.clone(),
        _ => return None,
    };
    Some(heap_push(HeapObj::MarkupBuilder(Box::new(MarkupState {
        writer,
        out: String::new(),
        frames: Vec::new(),
        started: false,
        after_pi: false,
        double_quotes: false,
        escape_attributes: true,
        omit_null_attributes: false,
    }))))
}

/// `builder.mkp` (the `MarkupBuilderHelper`) and the option properties.
pub(super) fn property(builder: &Value, name: &str) -> Option<Value> {
    match name {
        "mkp" => Some(heap_push(HeapObj::MarkupHelper(Box::new(builder.clone())))),
        "doubleQuotes" => with_state(builder, |s| Value::bool(s.double_quotes)),
        "escapeAttributes" => with_state(builder, |s| Value::bool(s.escape_attributes)),
        "omitNullAttributes" => with_state(builder, |s| Value::bool(s.omit_null_attributes)),
        _ => None,
    }
}

/// `builder.doubleQuotes = …` and the other option writes; `false` for any
/// other property.
pub(super) fn set_option(builder: &Value, name: &str, value: &Value) -> bool {
    let on = value.is_truthy();
    with_state(builder, |s| match name {
        "doubleQuotes" => {
            s.double_quotes = on;
            true
        }
        "escapeAttributes" => {
            s.escape_attributes = on;
            true
        }
        "omitNullAttributes" => {
            s.omit_null_attributes = on;
            true
        }
        _ => false,
    })
    .unwrap_or(false)
}

/// Text content, escaped: `&`, `<`, `>`.
fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Close the innermost open start tag, if it still lacks its `>`.
fn open_parent(s: &mut MarkupState) {
    if let Some(top) = s.frames.last_mut() {
        if top.pending {
            s.out.push('>');
            top.pending = false;
        }
    }
}

/// Hand a finished top-level element to the writer.
fn flush(vm: &mut VM, builder: &Value) {
    let Some((writer, text)) = with_state(builder, |s| {
        if !s.frames.is_empty() {
            return None;
        }
        Some((s.writer.clone(), std::mem::take(&mut s.out)))
    })
    .flatten() else {
        return;
    };
    let method = if as_print_stream(&writer).is_some() {
        "print"
    } else {
        "write"
    };
    dispatch_call(vm, writer, method, vec![Value::str(text)]);
}

/// One builder call: the element `name` with what its arguments say — a map
/// is its attributes, a closure its children (run with the builder as
/// delegate), anything else its text.
pub(super) fn element(vm: &mut VM, builder: &Value, name: &str, args: &[Value]) -> Value {
    let mut attrs: Vec<(String, Value)> = Vec::new();
    let mut text: Option<String> = None;
    let mut body: Option<Value> = None;
    for a in args {
        if let Some(entries) = as_omap(a) {
            attrs.extend(entries.into_iter().map(|(k, v)| (mapkey::text(&k), v)));
        } else if closure_meta(a).is_some() {
            body = Some(a.clone());
        } else {
            text = Some(render_value(vm, a));
        }
    }
    let rendered = render_attrs(vm, attrs);
    with_state(builder, |s| {
        start_tag(s, name, &rendered);
        s.frames.push(Frame {
            pending: true,
            child_element: false,
        });
        if let Some(t) = &text {
            open_parent(s);
            s.out.push_str(&escape_text(t));
        }
    });
    if let Some(clo) = body {
        DELEGATES.with(|d| d.borrow_mut().push(builder.clone()));
        let out = invoke_closure(vm, &clo, &[]);
        DELEGATES.with(|d| d.borrow_mut().pop());
        if let Err(e) = out {
            fault(vm, e);
        }
    }
    with_state(builder, |s| {
        let frame = s.frames.pop().unwrap_or_default();
        if frame.pending {
            s.out.push_str(" />");
        } else if frame.child_element {
            s.out.push('\n');
            s.out.push_str(&"  ".repeat(s.frames.len()));
            s.out.push_str(&format!("</{name}>"));
        } else {
            s.out.push_str(&format!("</{name}>"));
        }
    });
    flush(vm, builder);
    Value::Undef
}

/// Attribute values as text; `None` for a `null` value.
fn render_attrs(vm: &mut VM, attrs: Vec<(String, Value)>) -> Vec<(String, Option<String>)> {
    attrs
        .into_iter()
        .map(|(k, v)| {
            let shown = (!matches!(v, Value::Undef)).then(|| render_value(vm, &v));
            (k, shown)
        })
        .collect()
}

/// Write `<name attrs` on its own line — or straight after a processing
/// instruction's — leaving the tag open.
fn start_tag(s: &mut MarkupState, name: &str, attrs: &[(String, Option<String>)]) {
    open_parent(s);
    if let Some(top) = s.frames.last_mut() {
        top.child_element = true;
    }
    if std::mem::take(&mut s.after_pi) {
        // `pi` already ended the line.
    } else {
        if s.started {
            s.out.push('\n');
        }
        s.out.push_str(&"  ".repeat(s.frames.len()));
    }
    s.started = true;
    s.out.push('<');
    s.out.push_str(name);
    let quote = if s.double_quotes { '"' } else { '\'' };
    for (k, v) in attrs {
        if v.is_none() && s.omit_null_attributes {
            continue;
        }
        let raw = v.clone().unwrap_or_default();
        let value = if !s.escape_attributes {
            raw
        } else if s.double_quotes {
            escape_text(&raw).replace('"', "&quot;")
        } else {
            escape_text(&raw).replace('\'', "&apos;")
        };
        s.out.push_str(&format!(" {k}={quote}{value}{quote}"));
    }
}

/// `mkp.pi(target: [attrs])`: `<?target attrs?>` and a line break, written
/// the way an element's start tag is.
fn processing_instruction(vm: &mut VM, builder: &Value, args: &[Value]) -> Option<Value> {
    let entries = args.first().and_then(as_omap)?;
    let (target, attrs) = entries.into_iter().next()?;
    let attrs = as_omap(&attrs)
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| (mapkey::text(&k), v))
        .collect();
    let rendered = render_attrs(vm, attrs);
    with_state(builder, |s| {
        start_tag(s, &format!("?{}", mapkey::text(&target)), &rendered);
        s.out.push_str("?>\n");
        s.after_pi = true;
    });
    flush(vm, builder);
    Some(Value::Undef)
}

/// `mkp.yield(text)`, `mkp.yieldUnescaped(text)`, `mkp.comment(text)`:
/// content written inline in the open element.
pub(super) fn helper_call(
    vm: &mut VM,
    helper: &Value,
    method: &str,
    args: &[Value],
) -> Option<Value> {
    let builder = helper_builder(helper)?;
    if method == "pi" {
        return processing_instruction(vm, &builder, args);
    }
    let text = args
        .first()
        .map(|a| render_value(vm, a))
        .unwrap_or_default();
    let piece = match method {
        "yield" => escape_text(&text),
        "yieldUnescaped" => text,
        "comment" => format!("<!-- {text} -->"),
        _ => return None,
    };
    with_state(&builder, |s| {
        open_parent(s);
        s.out.push_str(&piece);
        s.started = true;
    });
    flush(vm, &builder);
    Some(Value::Undef)
}

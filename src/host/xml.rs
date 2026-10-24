//! `groovy.xml.XmlSlurper` / `groovy.xml.XmlParser` and the GPath views over
//! what they parse.
//!
//! Both parse into one arena of element nodes. The slurper hands back
//! `GPathResult`s — a `NodeChild` (one element), `NodeChildren` (a selection,
//! possibly empty) and `Attributes` — whose text is their descendants' text
//! run together and whose property reads select child elements (`r.book`),
//! attributes (`r.'@id'`) or every descendant (`r.'**'`). The parser hands
//! back `groovy.util.Node`s and `NodeList`s, which render as
//! `name[attributes={…}; value=[…]]`.
//!
//! As with both JDK-backed parsers' defaults, comments, processing
//! instructions and the doctype are skipped, and whitespace-only text is
//! dropped.

use super::*;

/// One parsed element.
struct XNode {
    name: String,
    attrs: Vec<(String, String)>,
    kids: Vec<XKid>,
    parent: Option<u32>,
}

enum XKid {
    El(u32),
    Text(String),
}

thread_local! {
    /// Every element any parse produced; a handle is its index.
    static NODES: RefCell<Vec<XNode>> = const { RefCell::new(Vec::new()) };
}

/// A parser or a view over parsed elements.
#[derive(Clone)]
pub(super) enum XmlVal {
    Slurper,
    Parser,
    /// A slurper `NodeChild`.
    Child(u32),
    /// A slurper `NodeChildren` selection.
    Children(Vec<u32>),
    /// A slurper `Attributes` selection: the attribute values, in order.
    Attrs(Vec<String>),
    /// A parser `groovy.util.Node`.
    Node(u32),
    /// A parser `groovy.util.NodeList`.
    NodeList(Vec<u32>),
}

pub(super) fn as_xml(v: &Value) -> Option<XmlVal> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::Xml(x)) => Some((**x).clone()),
            _ => None,
        }),
        _ => None,
    }
}

fn wrap(x: XmlVal) -> Value {
    heap_push(HeapObj::Xml(Box::new(x)))
}

/// `new XmlSlurper()` / `new XmlParser()`.
pub(super) fn construct(class: &str, args: &[Value]) -> Option<Value> {
    if !args.is_empty() {
        return None;
    }
    Some(wrap(match class {
        "XmlSlurper" => XmlVal::Slurper,
        "XmlParser" => XmlVal::Parser,
        _ => return None,
    }))
}

pub(super) fn class_name(v: &Value) -> Option<&'static str> {
    Some(match as_xml(v)? {
        XmlVal::Slurper => "groovy.xml.XmlSlurper",
        XmlVal::Parser => "groovy.xml.XmlParser",
        XmlVal::Child(_) => "groovy.xml.slurpersupport.NodeChild",
        XmlVal::Children(_) => "groovy.xml.slurpersupport.NodeChildren",
        XmlVal::Attrs(_) => "groovy.xml.slurpersupport.Attributes",
        XmlVal::Node(_) => "groovy.util.Node",
        XmlVal::NodeList(_) => "groovy.util.NodeList",
    })
}

// ---- parsing ---------------------------------------------------------------

struct Reader<'a> {
    s: &'a str,
    pos: usize,
}

impl Reader<'_> {
    fn rest(&self) -> &str {
        &self.s[self.pos..]
    }

    fn skip_ws(&mut self) {
        let trimmed = self.rest().trim_start();
        self.pos = self.s.len() - trimmed.len();
    }

    /// Skip past the next `end`, or fail at the end of input.
    fn skip_past(&mut self, end: &str) -> Result<(), String> {
        match self.rest().find(end) {
            Some(i) => {
                self.pos += i + end.len();
                Ok(())
            }
            None => {
                Err("XML document structures must start and end within the same entity.".into())
            }
        }
    }

    fn name(&mut self) -> Result<String, String> {
        let len = self
            .rest()
            .find(|c: char| c.is_whitespace() || matches!(c, '>' | '/' | '=' | '<'))
            .unwrap_or(self.rest().len());
        if len == 0 {
            return Err(
                "The markup in the document preceding the root element must be well-formed.".into(),
            );
        }
        let n = self.rest()[..len].to_string();
        self.pos += len;
        Ok(n)
    }

    /// Skip comments, processing instructions and a doctype.
    fn skip_misc(&mut self) -> Result<(), String> {
        loop {
            self.skip_ws();
            let r = self.rest();
            if r.starts_with("<!--") {
                self.skip_past("-->")?;
            } else if r.starts_with("<?") {
                self.skip_past("?>")?;
            } else if r.starts_with("<!DOCTYPE") {
                self.skip_past(">")?;
            } else {
                return Ok(());
            }
        }
    }
}

/// Replace the predefined and numeric character references.
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';') else { break };
        let ent = &rest[1..end];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .map(|h| u32::from_str_radix(h, 16))
                .or_else(|| ent.strip_prefix('#').map(str::parse::<u32>))
                .and_then(Result::ok)
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn parse_element(
    r: &mut Reader,
    nodes: &mut Vec<XNode>,
    parent: Option<u32>,
) -> Result<u32, String> {
    r.pos += 1; // `<`
    let name = r.name()?;
    let mut attrs = Vec::new();
    loop {
        r.skip_ws();
        let rest = r.rest();
        if rest.starts_with("/>") {
            r.pos += 2;
            let id = nodes.len() as u32;
            nodes.push(XNode {
                name,
                attrs,
                kids: Vec::new(),
                parent,
            });
            return Ok(id);
        }
        if rest.starts_with('>') {
            r.pos += 1;
            break;
        }
        let key = r.name()?;
        r.skip_ws();
        if !r.rest().starts_with('=') {
            return Err(format!(
                "Attribute name \"{key}\" associated with an element type \"{name}\" must be followed by the ' = ' character."
            ));
        }
        r.pos += 1;
        r.skip_ws();
        let quote = r.rest().chars().next().filter(|c| matches!(c, '"' | '\''));
        let Some(q) = quote else {
            return Err(format!(
                "Open quote is expected for attribute \"{key}\" associated with an  element type  \"{name}\"."
            ));
        };
        r.pos += 1;
        let end = r
            .rest()
            .find(q)
            .ok_or("XML document structures must start and end within the same entity.")?;
        attrs.push((key, unescape(&r.rest()[..end])));
        r.pos += end + 1;
    }
    let id = nodes.len() as u32;
    nodes.push(XNode {
        name: name.clone(),
        attrs,
        kids: Vec::new(),
        parent,
    });
    let mut text = String::new();
    let flush = |text: &mut String, nodes: &mut Vec<XNode>| {
        if !text.trim().is_empty() {
            nodes[id as usize]
                .kids
                .push(XKid::Text(std::mem::take(text)));
        }
        text.clear();
    };
    loop {
        let rest = r.rest();
        if rest.is_empty() {
            return Err(
                "XML document structures must start and end within the same entity.".to_string(),
            );
        }
        if let Some(after) = rest.strip_prefix("</") {
            flush(&mut text, nodes);
            let len = after
                .find('>')
                .ok_or("XML document structures must start and end within the same entity.")?;
            let close = after[..len].trim();
            if close != name {
                r.pos += 2 + len;
                return Err(format!(
                    "The element type \"{name}\" must be terminated by the matching end-tag \"</{name}>\"."
                ));
            }
            r.pos += 2 + len + 1;
            return Ok(id);
        }
        if rest.starts_with("<!--") {
            r.skip_past("-->")?;
        } else if let Some(cdata) = rest.strip_prefix("<![CDATA[") {
            let end = cdata
                .find("]]>")
                .ok_or("XML document structures must start and end within the same entity.")?;
            text.push_str(&cdata[..end]);
            r.pos += 9 + end + 3;
        } else if rest.starts_with("<?") {
            r.skip_past("?>")?;
        } else if rest.starts_with('<') {
            flush(&mut text, nodes);
            let child = parse_element(r, nodes, Some(id))?;
            nodes[id as usize].kids.push(XKid::El(child));
        } else {
            let len = rest.find('<').unwrap_or(rest.len());
            text.push_str(&unescape(&rest[..len]));
            r.pos += len;
        }
    }
}

/// Parse a whole document; its root element's handle, or the error and the
/// `line:column` Xerces reports it at.
fn parse_document(text: &str) -> Result<u32, (String, String)> {
    let mut r = Reader { s: text, pos: 0 };
    let out = NODES.with(|n| {
        r.skip_misc()?;
        if !r.rest().starts_with('<') {
            return Err("Content is not allowed in prolog.".to_string());
        }
        let mut nodes = n.borrow_mut();
        let root = parse_element(&mut r, &mut nodes, None)?;
        r.skip_misc()?;
        if !r.rest().is_empty() {
            return Err("Content is not allowed in trailing section.".into());
        }
        Ok(root)
    });
    out.map_err(|msg| {
        let before = &text[..r.pos.min(text.len())];
        let line = before.matches('\n').count() + 1;
        let col = before
            .rsplit('\n')
            .next()
            .map_or(0, |l| l.chars().count())
            .max(1);
        (msg, format!("{line}:{col}"))
    })
}

// ---- tree queries ----------------------------------------------------------

fn with_node<R>(id: u32, f: impl FnOnce(&XNode) -> R) -> R {
    NODES.with(|n| f(&n.borrow()[id as usize]))
}

fn node_name(id: u32) -> String {
    with_node(id, |n| n.name.clone())
}

/// The child elements of `id`, optionally only those named `name`.
fn child_elements(id: u32, name: Option<&str>) -> Vec<u32> {
    NODES.with(|n| {
        let nodes = n.borrow();
        nodes[id as usize]
            .kids
            .iter()
            .filter_map(|k| match k {
                XKid::El(c) if name.is_none_or_eq(&nodes[*c as usize].name) => Some(*c),
                _ => None,
            })
            .collect()
    })
}

trait NameFilter {
    fn is_none_or_eq(&self, name: &str) -> bool;
}

impl NameFilter for Option<&str> {
    fn is_none_or_eq(&self, name: &str) -> bool {
        self.map_or(true, |want| want == "*" || want == name)
    }
}

/// `id` and its descendants, depth-first.
fn depth_first(id: u32, out: &mut Vec<u32>) {
    out.push(id);
    for c in child_elements(id, None) {
        depth_first(c, out);
    }
}

/// `id` and its descendants, level by level.
fn breadth_first(id: u32) -> Vec<u32> {
    let mut out = vec![id];
    let mut i = 0;
    while i < out.len() {
        let kids = child_elements(out[i], None);
        out.extend(kids);
        i += 1;
    }
    out
}

/// Every descendant text run together.
fn node_text(id: u32) -> String {
    NODES.with(|n| {
        let nodes = n.borrow();
        let mut out = String::new();
        // Walk in document order: a stack of pending kids, reversed.
        let mut pending: Vec<&XKid> = nodes[id as usize].kids.iter().rev().collect();
        while let Some(k) = pending.pop() {
            match k {
                XKid::Text(t) => out.push_str(t),
                XKid::El(c) => pending.extend(nodes[*c as usize].kids.iter().rev()),
            }
        }
        out
    })
}

fn attr(id: u32, name: &str) -> Option<String> {
    with_node(id, |n| {
        n.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    })
}

fn attrs_map(id: u32, linked: bool) -> Value {
    let entries: Vec<(String, Value)> = with_node(id, |n| {
        n.attrs
            .iter()
            .map(|(k, v)| (k.clone(), Value::str(v.clone())))
            .collect()
    });
    let kind = if linked {
        MapKind::Linked
    } else {
        MapKind::Hash {
            req: DEFAULT_HASH_REQ,
        }
    };
    gmap_kind(entries, kind)
}

/// `Node.toString`: `name[attributes={k=v}; value=[…]]`.
fn node_string(id: u32) -> String {
    let (name, attrs, kids) = with_node(id, |n| {
        let attrs: Vec<String> = n.attrs.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let kids: Vec<Result<u32, String>> = n
            .kids
            .iter()
            .map(|k| match k {
                XKid::El(c) => Ok(*c),
                XKid::Text(t) => Err(t.clone()),
            })
            .collect();
        (n.name.clone(), attrs.join(", "), kids)
    });
    let value: Vec<String> = kids
        .into_iter()
        .map(|k| match k {
            Ok(c) => node_string(c),
            Err(t) => t,
        })
        .collect();
    format!(
        "{name}[attributes={{{attrs}}}; value=[{}]]",
        value.join(", ")
    )
}

/// A parser node's children: elements as `Ok`, text as `Err`.
fn direct_children(id: u32) -> Vec<Result<u32, String>> {
    with_node(id, |n| {
        n.kids
            .iter()
            .map(|k| match k {
                XKid::El(c) => Ok(*c),
                XKid::Text(t) => Err(t.clone()),
            })
            .collect()
    })
}

/// Whether `Node` traversal descends into these children: a lone text child
/// is the node's value, not a member of the walk.
fn walked(children: &[Result<u32, String>]) -> bool {
    children.len() > 1 || matches!(children.first(), Some(Ok(_)))
}

/// `Node.depthFirstRest(true)`: the preorder walk below `id`, text included
/// where it sits beside other children.
fn node_depth_rest(id: u32) -> Vec<Result<u32, String>> {
    let mut out = Vec::new();
    for child in direct_children(id) {
        match child {
            Ok(c) => {
                let below = node_depth_rest(c);
                out.push(Ok(c));
                if walked(&below) {
                    out.extend(below);
                }
            }
            text => out.push(text),
        }
    }
    out
}

/// `Node.breadthFirst()`: `id`, then each level's children in turn.
fn node_breadth(id: u32) -> Vec<Result<u32, String>> {
    let mut out = vec![Ok(id)];
    let mut level = direct_children(id);
    while !level.is_empty() {
        let mut next = Vec::new();
        for child in level {
            if let Ok(c) = child {
                let kids = direct_children(c);
                if walked(&kids) {
                    next.extend(kids);
                }
            }
            out.push(child);
        }
        level = next;
    }
    out
}

/// A traversal as a list of nodes and strings.
fn node_items(items: Vec<Result<u32, String>>) -> Value {
    glist(
        items
            .into_iter()
            .map(|i| match i {
                Ok(c) => wrap(XmlVal::Node(c)),
                Err(t) => Value::str(t),
            })
            .collect(),
    )
}

/// A parser `Node.value()`: its children, elements as nodes and text as
/// strings.
fn node_value(id: u32) -> Value {
    let kids: Vec<Value> = NODES.with(|n| {
        n.borrow()[id as usize]
            .kids
            .iter()
            .map(|k| match k {
                XKid::El(c) => Value::Int(i64::from(*c)),
                XKid::Text(t) => Value::str(t.clone()),
            })
            .collect()
    });
    let items = kids
        .into_iter()
        .map(|k| match k {
            Value::Int(c) => wrap(XmlVal::Node(c as u32)),
            other => other,
        })
        .collect();
    glist(items)
}

// ---- views -----------------------------------------------------------------

/// The text a view renders as.
pub(super) fn text(x: &XmlVal) -> Option<String> {
    Some(match x {
        XmlVal::Child(id) => node_text(*id),
        XmlVal::Children(ids) => ids.iter().map(|i| node_text(*i)).collect(),
        XmlVal::Attrs(vals) => vals.concat(),
        XmlVal::Node(id) => node_string(*id),
        XmlVal::NodeList(ids) => format!(
            "[{}]",
            ids.iter()
                .map(|i| node_string(*i))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        XmlVal::Slurper | XmlVal::Parser => return None,
    })
}

/// The text of a slurper view, for the `GPathResult` equality and
/// conversions.
pub(super) fn gpath_text(v: &Value) -> Option<String> {
    match as_xml(v)? {
        x @ (XmlVal::Child(_) | XmlVal::Children(_) | XmlVal::Attrs(_)) => text(&x),
        _ => None,
    }
}

/// The selected elements of a slurper view.
fn selected(x: &XmlVal) -> Option<Vec<u32>> {
    match x {
        XmlVal::Child(id) => Some(vec![*id]),
        XmlVal::Children(ids) => Some(ids.clone()),
        _ => None,
    }
}

/// What `for (x in v)` and the closure methods walk: a selection's
/// `NodeChild`ren, a node list's `Node`s.
pub(super) fn elements(v: &Value) -> Option<Vec<Value>> {
    Some(match as_xml(v)? {
        XmlVal::Child(id) => vec![wrap(XmlVal::Child(id))],
        XmlVal::Children(ids) => ids.into_iter().map(|i| wrap(XmlVal::Child(i))).collect(),
        XmlVal::NodeList(ids) => ids.into_iter().map(|i| wrap(XmlVal::Node(i))).collect(),
        XmlVal::Attrs(vals) => vals.into_iter().map(Value::str).collect(),
        _ => return None,
    })
}

/// `view.name` / `view.'@name'` / `view.'**'`.
pub(super) fn property(v: &Value, name: &str) -> Option<Value> {
    let x = as_xml(v)?;
    if let Some(ids) = selected(&x) {
        if let Some(a) = name.strip_prefix('@') {
            return Some(wrap(XmlVal::Attrs(
                ids.iter().filter_map(|i| attr(*i, a)).collect(),
            )));
        }
        if name == "**" {
            let mut out = Vec::new();
            for i in ids {
                depth_first(i, &mut out);
            }
            return Some(wrap(XmlVal::Children(out)));
        }
        let picked = ids
            .iter()
            .flat_map(|i| child_elements(*i, Some(name)))
            .collect();
        return Some(wrap(XmlVal::Children(picked)));
    }
    let ids = match x {
        XmlVal::Node(id) => {
            if let Some(a) = name.strip_prefix('@') {
                return Some(attr(id, a).map(Value::str).unwrap_or(Value::Undef));
            }
            vec![id]
        }
        XmlVal::NodeList(ids) => ids,
        _ => return None,
    };
    if name == "**" {
        let mut out = Vec::new();
        for i in ids {
            depth_first(i, &mut out);
        }
        return Some(wrap(XmlVal::NodeList(out)));
    }
    let picked = ids
        .iter()
        .flat_map(|i| child_elements(*i, Some(name)))
        .collect();
    Some(wrap(XmlVal::NodeList(picked)))
}

/// `view.@name`.
pub(super) fn attribute(v: &Value, name: &str) -> Option<Value> {
    property(v, &format!("@{name}"))
}

fn index(len: usize, i: i64) -> Option<usize> {
    let i = if i < 0 { i + len as i64 } else { i };
    (0..len as i64).contains(&i).then_some(i as usize)
}

/// The methods of the parsers and the views. `None` hands the call to the
/// view's elements as a list.
pub(super) fn dispatch(vm: &mut VM, recv: &Value, method: &str, args: &[Value]) -> Option<Value> {
    let x = as_xml(recv)?;
    match (&x, method, args) {
        (XmlVal::Slurper | XmlVal::Parser, "parseText", [t]) => {
            return Some(parse_result(vm, &x, &groovy_str(t)));
        }
        (XmlVal::Slurper | XmlVal::Parser, "parse", [src]) => {
            let text = match file::file_path(src) {
                Some(path) => match std::fs::read_to_string(&path) {
                    Ok(t) => t,
                    Err(_) => {
                        raise(
                            vm,
                            "FileNotFoundException",
                            &format!("{path} (No such file or directory)"),
                        );
                        return Some(Value::Undef);
                    }
                },
                None => {
                    let t = dispatch_call(vm, src.clone(), "getText", Vec::new());
                    render_value(vm, &t)
                }
            };
            return Some(parse_result(vm, &x, &text));
        }
        (XmlVal::Slurper | XmlVal::Parser, ..) => return None,
        _ => {}
    }
    if let Some(ids) = selected(&x) {
        return gpath_method(vm, &x, ids, method, args);
    }
    if let XmlVal::Attrs(vals) = &x {
        return Some(match (method, args) {
            ("text" | "toString", []) => Value::str(vals.concat()),
            ("size", []) => Value::int(vals.len() as i64),
            _ => text_conversion(vm, &vals.concat(), method, args)?,
        });
    }
    if let XmlVal::Node(id) = x {
        return Some(match (method, args) {
            ("name", []) => Value::str(node_name(id)),
            ("attributes", []) => attrs_map(id, true),
            ("attribute", [n]) => attr(id, &groovy_str(n))
                .map(Value::str)
                .unwrap_or(Value::Undef),
            ("text", []) => Value::str(node_text(id)),
            ("value" | "children", []) => node_value(id),
            ("parent", []) => with_node(id, |n| n.parent)
                .map(|p| wrap(XmlVal::Node(p)))
                .unwrap_or(Value::Undef),
            ("depthFirst", []) => {
                let mut out = vec![Ok(id)];
                out.extend(node_depth_rest(id));
                node_items(out)
            }
            ("breadthFirst", []) => node_items(node_breadth(id)),
            ("toString", []) => Value::str(node_string(id)),
            ("get" | "getAt", [n]) if matches!(n, Value::Str(_)) => property(recv, &groovy_str(n))?,
            _ => return None,
        });
    }
    let XmlVal::NodeList(ids) = x else {
        return None;
    };
    Some(match (method, args) {
        ("getAt", [Value::Int(i)]) => match index(ids.len(), *i) {
            Some(at) => wrap(XmlVal::Node(ids[at])),
            None => Value::Undef,
        },
        ("getAt" | "get", [n]) if matches!(n, Value::Str(_)) => property(recv, &groovy_str(n))?,
        ("text", []) => Value::str(ids.iter().map(|i| node_text(*i)).collect::<String>()),
        ("toString", []) => Value::str(text(&XmlVal::NodeList(ids))?),
        _ => return None,
    })
}

fn parse_result(vm: &mut VM, x: &XmlVal, text: &str) -> Value {
    match parse_document(text) {
        Ok(root) => wrap(match x {
            XmlVal::Slurper => XmlVal::Child(root),
            _ => XmlVal::Node(root),
        }),
        Err((msg, at)) => {
            // The JDK parser's default error handler reports a fatal error on
            // standard error before the exception propagates.
            eprintln!("[Fatal Error] :{at}: {msg}");
            raise(vm, "SAXParseException", &msg);
            Value::Undef
        }
    }
}

/// `GPathResult.toInteger()` and the other text conversions.
fn text_conversion(vm: &mut VM, text: &str, method: &str, args: &[Value]) -> Option<Value> {
    if !args.is_empty() {
        return None;
    }
    let target = match method {
        "toInteger" => "toInteger",
        "toLong" => "toLong",
        "toDouble" => "toDouble",
        "toFloat" => "toFloat",
        "toBigDecimal" => "toBigDecimal",
        "toBigInteger" => "toBigInteger",
        "toBoolean" => "toBoolean",
        _ => return None,
    };
    Some(dispatch_call(
        vm,
        Value::str(text.to_string()),
        target,
        Vec::new(),
    ))
}

/// A slurper `NodeChild` / `NodeChildren` method.
fn gpath_method(
    vm: &mut VM,
    x: &XmlVal,
    ids: Vec<u32>,
    method: &str,
    args: &[Value],
) -> Option<Value> {
    let children = |v: Vec<u32>| wrap(XmlVal::Children(v));
    Some(match (method, args) {
        ("name", []) => match ids.first() {
            Some(id) => Value::str(node_name(*id)),
            None => Value::str(String::new()),
        },
        ("text" | "toString", []) => Value::str(text(x)?),
        ("size", []) => Value::int(ids.len() as i64),
        ("isEmpty", []) => Value::bool(ids.is_empty()),
        ("attributes", []) => match ids.first() {
            Some(id) => attrs_map(*id, false),
            None => gmap_kind(Vec::new(), MapKind::Linked),
        },
        ("children", []) => children(ids.iter().flat_map(|i| child_elements(*i, None)).collect()),
        ("parent", []) => children(
            ids.first()
                .and_then(|i| with_node(*i, |n| n.parent))
                .into_iter()
                .collect(),
        ),
        ("depthFirst", []) => {
            let mut out = Vec::new();
            for i in ids {
                depth_first(i, &mut out);
            }
            glist(out.into_iter().map(|i| wrap(XmlVal::Child(i))).collect())
        }
        ("breadthFirst", []) => glist(
            ids.iter()
                .flat_map(|i| breadth_first(*i))
                .map(|i| wrap(XmlVal::Child(i)))
                .collect(),
        ),
        ("list", []) => glist(ids.into_iter().map(|i| wrap(XmlVal::Child(i))).collect()),
        ("getAt", [Value::Int(i)]) => match index(ids.len(), *i) {
            Some(at) => wrap(XmlVal::Child(ids[at])),
            None => children(Vec::new()),
        },
        ("getAt" | "getProperty", [n]) if matches!(n, Value::Str(_)) => {
            property(&wrap(x.clone()), &groovy_str(n))?
        }
        ("find" | "findAll", [clo]) if closure_meta(clo).is_some() => {
            let mut hits = Vec::new();
            for id in ids {
                let out = invoke_closure(vm, clo, &[wrap(XmlVal::Child(id))]);
                match out {
                    Ok(v) if v.is_truthy() => {
                        hits.push(id);
                        if method == "find" {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        fault(vm, e);
                        return Some(Value::Undef);
                    }
                }
            }
            if method == "find" && hits.len() == 1 {
                wrap(XmlVal::Child(hits[0]))
            } else {
                children(hits)
            }
        }
        _ => {
            let joined = text(x)?;
            text_conversion(vm, &joined, method, args)?
        }
    })
}

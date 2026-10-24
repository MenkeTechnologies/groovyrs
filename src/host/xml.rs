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

/// One parsed element. `name` is the qualified name as written; a
/// namespace-aware parse also resolves its `local` part, `prefix` and `uri`
/// and moves its `xmlns` declarations to `decls`.
struct XNode {
    name: String,
    local: String,
    prefix: String,
    uri: String,
    attrs: Vec<XAttr>,
    decls: Vec<(String, String)>,
    kids: Vec<XKid>,
    parent: Option<u32>,
    aware: bool,
}

/// One attribute: its qualified name, local part, namespace and value.
struct XAttr {
    qname: String,
    local: String,
    uri: String,
    value: String,
}

impl XAttr {
    /// The key it has in an `attributes()` map: its local name, or
    /// `{uri}local` for a namespaced one (a `QName` there).
    fn key(&self) -> String {
        if self.uri.is_empty() {
            self.local.clone()
        } else {
            format!("{{{}}}{}", self.uri, self.local)
        }
    }
}

enum XKid {
    El(u32),
    Text(String),
}

thread_local! {
    /// Every element any parse produced; a handle is its index.
    static NODES: RefCell<Vec<XNode>> = const { RefCell::new(Vec::new()) };
    /// The prefixes `declareNamespace` bound, by document root: what a
    /// prefixed GPath name (`r.'a:x'`) is resolved through.
    static HINTS: RefCell<HashMap<u32, Vec<(String, String)>>> = RefCell::new(HashMap::new());
}

/// A parser or a view over parsed elements.
#[derive(Clone)]
pub(super) enum XmlVal {
    /// A parser, and whether it is namespace-aware.
    Slurper(bool),
    Parser(bool),
    /// A parser node's `groovy.namespace.QName` name.
    QName {
        uri: String,
        local: String,
        prefix: String,
    },
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

/// `new XmlSlurper()` / `new XmlParser()`, and the
/// `(validating, namespaceAware)` forms; `new QName(uri, local[, prefix])`.
pub(super) fn construct(class: &str, args: &[Value]) -> Option<Value> {
    if class == "QName" {
        let text = |i: usize| args.get(i).map(groovy_str).unwrap_or_default();
        return match args.len() {
            1 => Some(wrap(XmlVal::QName {
                uri: String::new(),
                local: text(0),
                prefix: String::new(),
            })),
            2 | 3 => Some(wrap(XmlVal::QName {
                uri: text(0),
                local: text(1),
                prefix: text(2),
            })),
            _ => None,
        };
    }
    let aware = match args {
        [] => true,
        [_, aware] => aware.is_truthy(),
        _ => return None,
    };
    Some(wrap(match class {
        "XmlSlurper" => XmlVal::Slurper(aware),
        "XmlParser" => XmlVal::Parser(aware),
        _ => return None,
    }))
}

pub(super) fn class_name(v: &Value) -> Option<&'static str> {
    Some(match as_xml(v)? {
        XmlVal::Slurper(_) => "groovy.xml.XmlSlurper",
        XmlVal::Parser(_) => "groovy.xml.XmlParser",
        XmlVal::QName { .. } => "groovy.namespace.QName",
        XmlVal::Child(_) => "groovy.xml.slurpersupport.NodeChild",
        XmlVal::Children(_) => "groovy.xml.slurpersupport.NodeChildren",
        XmlVal::Attrs(_) => "groovy.xml.slurpersupport.Attributes",
        XmlVal::Node(_) => "groovy.util.Node",
        XmlVal::NodeList(_) => "groovy.util.NodeList",
    })
}

// ---- parsing ---------------------------------------------------------------
//
// Errors carry the byte offset Xerces reports them at: the character that
// gave the error away (a mismatched end tag's last name character, a missing
// quote, the first character after the root element), or the end of input.

const EOF_IN_ENTITY: &str = "XML document structures must start and end within the same entity.";

type ParseError = (String, usize);

struct Reader<'a> {
    s: &'a str,
    pos: usize,
}

fn is_name_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == ':'
}

fn is_name_char(c: char) -> bool {
    is_name_start(c) || c.is_ascii_digit() || matches!(c, '-' | '.') || c.is_alphanumeric()
}

impl Reader<'_> {
    fn rest(&self) -> &str {
        &self.s[self.pos..]
    }

    fn eof(&self) -> ParseError {
        (EOF_IN_ENTITY.to_string(), self.s.len())
    }

    fn skip_ws(&mut self) {
        let trimmed = self.rest().trim_start();
        self.pos = self.s.len() - trimmed.len();
    }

    /// Skip past the next `end`, or fail at the end of input.
    fn skip_past(&mut self, end: &str) -> Result<(), ParseError> {
        match self.rest().find(end) {
            Some(i) => {
                self.pos += i + end.len();
                Ok(())
            }
            None => Err(self.eof()),
        }
    }

    /// An XML name; `bad` is the message for a character that cannot start
    /// one, reported at that character.
    fn name(&mut self, bad: &str) -> Result<String, ParseError> {
        match self.rest().chars().next() {
            None => return Err(self.eof()),
            Some(c) if !is_name_start(c) => return Err((bad.to_string(), self.pos)),
            _ => {}
        }
        let len = self
            .rest()
            .find(|c: char| !is_name_char(c))
            .unwrap_or(self.rest().len());
        let n = self.rest()[..len].to_string();
        self.pos += len;
        Ok(n)
    }

    /// Skip comments, processing instructions and a doctype.
    fn skip_misc(&mut self) -> Result<(), ParseError> {
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

/// Replace the predefined and numeric character references in `s`, which
/// starts at byte `base` of the document.
fn unescape(s: &str, base: usize) -> Result<String, ParseError> {
    if !s.contains('&') {
        return Ok(s.to_string());
    }
    let mut out = String::new();
    let mut at = 0;
    while let Some(i) = s[at..].find('&') {
        out.push_str(&s[at..at + i]);
        let start = at + i;
        let Some(len) = s[start..].find(';') else {
            out.push_str(&s[start..]);
            return Ok(out);
        };
        let ent = &s[start + 1..start + len];
        let after = base + start + len + 1;
        let ch = match ent {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                if let Some(hex) = ent.strip_prefix("#x") {
                    let bad = hex.find(|c: char| !c.is_ascii_hexdigit());
                    match (bad, u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)) {
                        (None, Some(c)) => c,
                        (b, _) => {
                            return Err((
                                "A hexadecimal representation must immediately follow the \"&#x\" in a character reference.".to_string(),
                                base + start + 3 + b.unwrap_or(0),
                            ))
                        }
                    }
                } else if let Some(dec) = ent.strip_prefix('#') {
                    let bad = dec.find(|c: char| !c.is_ascii_digit());
                    match (bad, dec.parse::<u32>().ok().and_then(char::from_u32)) {
                        (None, Some(c)) => c,
                        (b, _) => {
                            return Err((
                                "A decimal representation must immediately follow the \"&#\" in a character reference.".to_string(),
                                base + start + 2 + b.unwrap_or(0),
                            ))
                        }
                    }
                } else {
                    return Err((
                        format!("The entity \"{ent}\" was referenced, but not declared."),
                        after,
                    ));
                }
            }
        };
        out.push(ch);
        at = start + len + 1;
    }
    out.push_str(&s[at..]);
    Ok(out)
}

/// A start tag's raw attributes, in order.
type RawAttrs = Vec<(String, String)>;

fn parse_element(
    r: &mut Reader,
    nodes: &mut Vec<XNode>,
    parent: Option<u32>,
    aware: bool,
    bad_name: &str,
) -> Result<u32, ParseError> {
    r.pos += 1; // `<`
    let name = r.name(bad_name)?;
    let mut attrs: RawAttrs = Vec::new();
    let empty;
    loop {
        let before_ws = r.pos;
        r.skip_ws();
        let rest = r.rest();
        if rest.is_empty() {
            return Err(r.eof());
        }
        if rest.starts_with("/>") {
            r.pos += 2;
            empty = true;
            break;
        }
        if rest.starts_with('>') {
            r.pos += 1;
            empty = false;
            break;
        }
        if r.pos == before_ws {
            return Err((
                format!("Element type \"{name}\" must be followed by either attribute specifications, \">\" or \"/>\"."),
                r.pos,
            ));
        }
        let key = r.name(&format!(
            "Element type \"{name}\" must be followed by either attribute specifications, \">\" or \"/>\"."
        ))?;
        r.skip_ws();
        if !r.rest().starts_with('=') {
            return Err((
                format!("Attribute name \"{key}\" associated with an element type \"{name}\" must be followed by the ' = ' character."),
                r.pos,
            ));
        }
        r.pos += 1;
        r.skip_ws();
        let quote = r.rest().chars().next().filter(|c| matches!(c, '"' | '\''));
        let Some(q) = quote else {
            return Err((
                format!("Open quote is expected for attribute \"{key}\" associated with an  element type  \"{name}\"."),
                r.pos,
            ));
        };
        r.pos += 1;
        // The value is scanned up to its closing quote; a `<` met first is
        // the error, wherever the quote is.
        let stop = r.rest().find([q, '<']);
        let end = match stop {
            Some(i) if r.rest()[i..].starts_with('<') => {
                return Err((
                    format!("The value of attribute \"{key}\" associated with an element type \"{name}\" must not contain the '<' character."),
                    r.pos + i,
                ));
            }
            Some(i) => i,
            None => return Err(r.eof()),
        };
        let raw = &r.rest()[..end];
        let value = unescape(raw, r.pos)?;
        r.pos += end + 1;
        attrs.push((key, value));
    }
    // A repeated attribute is noticed once the tag is complete.
    for (i, (k, _)) in attrs.iter().enumerate() {
        if attrs[..i].iter().any(|(p, _)| p == k) {
            return Err((
                format!("Attribute \"{k}\" was already specified for element \"{name}\"."),
                r.pos,
            ));
        }
    }
    let id = nodes.len() as u32;
    let node = new_node(nodes, parent, &name, attrs, aware);
    nodes.push(node);
    if empty {
        return Ok(id);
    }
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
            return Err(r.eof());
        }
        if let Some(after) = rest.strip_prefix("</") {
            flush(&mut text, nodes);
            let start = r.pos + 2;
            let len = after
                .find(|c: char| !is_name_char(c))
                .unwrap_or(after.len());
            let close = &after[..len];
            if close != name {
                // Reported at the end tag name's last character.
                let last = close.char_indices().last().map_or(0, |(i, _)| i);
                return Err((
                    format!("The element type \"{name}\" must be terminated by the matching end-tag \"</{name}>\"."),
                    start + last,
                ));
            }
            r.pos = start + len;
            r.skip_ws();
            if !r.rest().starts_with('>') {
                return Err(if r.rest().is_empty() {
                    r.eof()
                } else {
                    (
                        format!("The end-tag for element type \"{name}\" must end with a '>' delimiter."),
                        r.pos,
                    )
                });
            }
            r.pos += 1;
            return Ok(id);
        }
        if rest.starts_with("<!--") {
            r.skip_past("-->")?;
        } else if let Some(cdata) = rest.strip_prefix("<![CDATA[") {
            let end = cdata.find("]]>").ok_or_else(|| r.eof())?;
            text.push_str(&cdata[..end]);
            r.pos += 9 + end + 3;
        } else if rest.starts_with("<?") {
            r.skip_past("?>")?;
        } else if rest.starts_with('<') {
            flush(&mut text, nodes);
            let child = parse_element(
                r,
                nodes,
                Some(id),
                aware,
                "The content of elements must consist of well-formed character data or markup.",
            )?;
            nodes[id as usize].kids.push(XKid::El(child));
        } else {
            let len = rest.find('<').unwrap_or(rest.len());
            text.push_str(&unescape(&rest[..len], r.pos)?);
            r.pos += len;
        }
    }
}

/// The namespace bound to `prefix` (`""` for the default) at an element whose
/// parent is `parent`, given the element's own declarations.
fn resolve_prefix(
    nodes: &[XNode],
    parent: Option<u32>,
    own: &[(String, String)],
    prefix: &str,
) -> String {
    if let Some((_, uri)) = own.iter().find(|(p, _)| p == prefix) {
        return uri.clone();
    }
    let mut cur = parent;
    while let Some(id) = cur {
        let n = &nodes[id as usize];
        if let Some((_, uri)) = n.decls.iter().find(|(p, _)| p == prefix) {
            return uri.clone();
        }
        cur = n.parent;
    }
    String::new()
}

/// An element as the parser records it: in a namespace-aware parse its
/// `xmlns` declarations are split off and its name and attributes resolved.
fn new_node(nodes: &[XNode], parent: Option<u32>, name: &str, raw: RawAttrs, aware: bool) -> XNode {
    let split = |q: &str| match q.split_once(':') {
        Some((p, l)) => (p.to_string(), l.to_string()),
        None => (String::new(), q.to_string()),
    };
    if !aware {
        return XNode {
            name: name.to_string(),
            local: name.to_string(),
            prefix: String::new(),
            uri: String::new(),
            attrs: raw
                .into_iter()
                .map(|(q, value)| XAttr {
                    local: q.clone(),
                    qname: q,
                    uri: String::new(),
                    value,
                })
                .collect(),
            decls: Vec::new(),
            kids: Vec::new(),
            parent,
            aware,
        };
    }
    let mut decls = Vec::new();
    let mut plain = Vec::new();
    for (q, v) in raw {
        if q == "xmlns" {
            decls.push((String::new(), v));
        } else if let Some(p) = q.strip_prefix("xmlns:") {
            decls.push((p.to_string(), v));
        } else {
            plain.push((q, v));
        }
    }
    let (prefix, local) = split(name);
    let uri = resolve_prefix(nodes, parent, &decls, &prefix);
    let attrs = plain
        .into_iter()
        .map(|(q, value)| {
            let (p, l) = split(&q);
            // An unprefixed attribute is in no namespace.
            let uri = if p.is_empty() {
                String::new()
            } else {
                resolve_prefix(nodes, parent, &decls, &p)
            };
            XAttr {
                qname: q,
                local: l,
                uri,
                value,
            }
        })
        .collect();
    XNode {
        name: name.to_string(),
        local,
        prefix,
        uri,
        attrs,
        decls,
        kids: Vec::new(),
        parent,
        aware,
    }
}

/// Parse a whole document; its root element's handle, or the error and the
/// `line:column` Xerces reports it at.
fn parse_document(text: &str, aware: bool) -> Result<u32, (String, String)> {
    let mut r = Reader { s: text, pos: 0 };
    let out = NODES.with(|n| {
        r.skip_misc()?;
        if r.rest().is_empty() {
            return Err(("Premature end of file.".to_string(), r.pos));
        }
        if !r.rest().starts_with('<') {
            return Err(("Content is not allowed in prolog.".to_string(), r.pos));
        }
        let mut nodes = n.borrow_mut();
        let root = parse_element(
            &mut r,
            &mut nodes,
            None,
            aware,
            "The markup in the document preceding the root element must be well-formed.",
        )?;
        r.skip_misc()?;
        if r.rest() == "<" {
            return Err(r.eof());
        }
        if r.rest().starts_with('<') {
            return Err((
                "The markup in the document following the root element must be well-formed."
                    .to_string(),
                r.pos + 1,
            ));
        }
        if !r.rest().is_empty() {
            return Err((
                "Content is not allowed in trailing section.".to_string(),
                r.pos,
            ));
        }
        Ok(root)
    });
    out.map_err(|(msg, at)| {
        let at = at.min(text.len());
        let before = &text[..at];
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        (msg, format!("{line}:{col}"))
    })
}

// ---- tree queries ----------------------------------------------------------

fn with_node<R>(id: u32, f: impl FnOnce(&XNode) -> R) -> R {
    NODES.with(|n| f(&n.borrow()[id as usize]))
}

/// A slurper element's `name()`: its local name in a namespace-aware parse.
fn node_name(id: u32) -> String {
    with_node(id, |n| n.local.clone())
}

/// A parser node's `name()`: a `QName` for an element in a namespace, the
/// local name for one in none, and the name as written when the parse is not
/// namespace-aware (`XmlParser.getElementName`).
fn parser_name(id: u32) -> Value {
    with_node(id, |n| {
        if n.aware && n.uri.is_empty() {
            Value::str(n.local.clone())
        } else if n.aware {
            wrap(XmlVal::QName {
                uri: n.uri.clone(),
                local: n.local.clone(),
                prefix: n.prefix.clone(),
            })
        } else {
            Value::str(n.name.clone())
        }
    })
}

/// What a parser node's name renders as: `{uri}local`, or the local name in
/// no namespace.
fn qname_text(n: &XNode) -> String {
    if n.uri.is_empty() {
        n.local.clone()
    } else {
        format!("{{{}}}{}", n.uri, n.local)
    }
}

/// The root element of the document `id` is in.
fn root_of(id: u32) -> u32 {
    NODES.with(|n| {
        let nodes = n.borrow();
        let mut cur = id;
        while let Some(p) = nodes[cur as usize].parent {
            cur = p;
        }
        cur
    })
}

/// How a GPath name selects: a slurper matches the local name, or a declared
/// `prefix:local`; a parser matches what `QName.equals(String)` accepts.
#[derive(Clone, Copy)]
enum Select<'a> {
    Any,
    Slurp(&'a str),
    Parse(&'a str),
}

fn selects(n: &XNode, sel: Select, hints: &[(String, String)]) -> bool {
    match sel {
        Select::Any => true,
        Select::Slurp("*") | Select::Parse("*") => true,
        _ if !n.aware => matches!(sel, Select::Slurp(w) | Select::Parse(w) if w == n.name),
        Select::Slurp(want) => match want.split_once(':') {
            Some((p, l)) => hints
                .iter()
                .find(|(hp, _)| hp == p)
                .is_some_and(|(_, uri)| *uri == n.uri && l == n.local),
            None => want == n.local,
        },
        Select::Parse(want) => {
            if n.prefix.is_empty() {
                want == n.local
            } else {
                want == n.name
            }
        }
    }
}

/// The child elements of `id` that `sel` selects.
fn child_elements(id: u32, sel: Select) -> Vec<u32> {
    let hints = match sel {
        Select::Slurp(w) if w.contains(':') => {
            let root = root_of(id);
            HINTS.with(|h| h.borrow().get(&root).cloned().unwrap_or_default())
        }
        _ => Vec::new(),
    };
    NODES.with(|n| {
        let nodes = n.borrow();
        nodes[id as usize]
            .kids
            .iter()
            .filter_map(|k| match k {
                XKid::El(c) if selects(&nodes[*c as usize], sel, &hints) => Some(*c),
                _ => None,
            })
            .collect()
    })
}

/// `id` and its descendants, depth-first.
fn depth_first(id: u32, out: &mut Vec<u32>) {
    out.push(id);
    for c in child_elements(id, Select::Any) {
        depth_first(c, out);
    }
}

/// `id` and its descendants, level by level.
fn breadth_first(id: u32) -> Vec<u32> {
    let mut out = vec![id];
    let mut i = 0;
    while i < out.len() {
        let kids = child_elements(out[i], Select::Any);
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

/// A slurper attribute by its qualified name as written (`@a:k`).
fn attr(id: u32, name: &str) -> Option<String> {
    with_node(id, |n| {
        n.attrs
            .iter()
            .find(|a| a.qname == name)
            .map(|a| a.value.clone())
    })
}

/// A parser attribute by its `attributes()` key.
fn parser_attr(id: u32, name: &str) -> Option<String> {
    with_node(id, |n| {
        n.attrs
            .iter()
            .find(|a| a.key() == name)
            .map(|a| a.value.clone())
    })
}

/// `attributes()`: a parser node's `LinkedHashMap`, or a slurper element's
/// `NamespaceAwareHashMap`.
fn attrs_map(id: u32, linked: bool) -> Value {
    let entries: Vec<(String, Value)> = with_node(id, |n| {
        n.attrs
            .iter()
            .map(|a| (a.key(), Value::str(a.value.clone())))
            .collect()
    });
    if linked {
        // `XmlParser` keys a namespaced attribute by its `QName`.
        let keys: Vec<String> = with_node(id, |n| {
            n.attrs
                .iter()
                .map(|a| {
                    if a.uri.is_empty() {
                        return a.key();
                    }
                    let prefix = a.qname.split_once(':').map_or("", |(p, _)| p);
                    mapkey::encode(&wrap(XmlVal::QName {
                        uri: a.uri.clone(),
                        local: a.local.clone(),
                        prefix: prefix.to_string(),
                    }))
                })
                .collect()
        });
        let entries = keys
            .into_iter()
            .zip(entries)
            .map(|(k, (_, v))| (k, v))
            .collect();
        return gmap_kind(entries, MapKind::Linked);
    }
    // `XmlSlurper.startElement` adds the attributes last to first, which
    // decides the order of keys sharing a hash bucket.
    let entries: Vec<(String, Value)> = entries.into_iter().rev().collect();
    let map = gmap_kind(
        entries,
        MapKind::Hash {
            req: DEFAULT_HASH_REQ,
        },
    );
    seq::set_flavor(
        &map,
        "groovy.xml.slurpersupport.NamespaceAwareHashMap",
        seq::Writes::Open,
    );
    map
}

/// `Node.toString`: `name[attributes={k=v}; value=[…]]`.
fn node_string(id: u32) -> String {
    let (name, attrs, kids) = with_node(id, |n| {
        let attrs: Vec<String> = n
            .attrs
            .iter()
            .map(|a| format!("{}={}", a.key(), a.value))
            .collect();
        let kids: Vec<Result<u32, String>> = n
            .kids
            .iter()
            .map(|k| match k {
                XKid::El(c) => Ok(*c),
                XKid::Text(t) => Err(t.clone()),
            })
            .collect();
        (qname_text(n), attrs.join(", "), kids)
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
        XmlVal::QName { uri, local, .. } => {
            if uri.is_empty() {
                local.clone()
            } else {
                format!("{{{uri}}}{local}")
            }
        }
        XmlVal::Slurper(_) | XmlVal::Parser(_) => return None,
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
    // A `QName`'s properties are its getters.
    if let XmlVal::QName { uri, local, prefix } = &x {
        return Some(Value::str(match name {
            "localPart" => local.clone(),
            "namespaceURI" => uri.clone(),
            "prefix" => prefix.clone(),
            "qualifiedName" => qualified_name(prefix, local),
            _ => return None,
        }));
    }
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
            .flat_map(|i| child_elements(*i, Select::Slurp(name)))
            .collect();
        return Some(wrap(XmlVal::Children(picked)));
    }
    let ids = match x {
        XmlVal::Node(id) => {
            if let Some(a) = name.strip_prefix('@') {
                return Some(parser_attr(id, a).map(Value::str).unwrap_or(Value::Undef));
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
        .flat_map(|i| child_elements(*i, Select::Parse(name)))
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
        (XmlVal::Slurper(_) | XmlVal::Parser(_), "parseText", [t]) => {
            return Some(parse_result(vm, &x, &groovy_str(t)));
        }
        (XmlVal::Slurper(_) | XmlVal::Parser(_), "parse", [src]) => {
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
        (XmlVal::Slurper(_) | XmlVal::Parser(_), ..) => return None,
        (XmlVal::QName { .. }, "equals", [other]) => {
            return Some(Value::bool(qname_equal(recv, other).unwrap_or(false)));
        }
        (XmlVal::QName { .. }, "matches", [other]) => {
            return Some(Value::bool(qname_matches(recv, other).unwrap_or(false)));
        }
        (XmlVal::QName { uri, local, prefix }, _, []) => {
            return Some(match method {
                "getLocalPart" => Value::str(local.clone()),
                "getNamespaceURI" => Value::str(uri.clone()),
                "getPrefix" => Value::str(prefix.clone()),
                "getQualifiedName" => Value::str(qualified_name(prefix, local)),
                "toString" => Value::str(text(&x)?),
                _ => return None,
            })
        }
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
            ("name", []) => parser_name(id),
            ("attributes", []) => attrs_map(id, true),
            // `attributes().get(key)`: a namespaced attribute answers its `QName`.
            ("attribute", [n]) => omap_get(&attrs_map(id, true), &mapkey::encode(n))
                .flatten()
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
            // `node[qname]`: the children with that namespace and local name.
            ("getAt", [q]) => {
                let XmlVal::QName { uri, local, .. } = as_xml(q)? else {
                    return None;
                };
                let picked = child_elements(id, Select::Any)
                    .into_iter()
                    .filter(|c| with_node(*c, |n| n.uri == uri && n.local == local))
                    .collect();
                wrap(XmlVal::NodeList(picked))
            }
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
    match parse_document(
        text,
        matches!(x, XmlVal::Slurper(true) | XmlVal::Parser(true)),
    ) {
        Ok(root) => wrap(match x {
            XmlVal::Slurper(_) => XmlVal::Child(root),
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
    // The `NodeChild`-only members are missing on a selection.
    if matches!(x, XmlVal::Children(_)) && matches!(method, "namespaceURI" | "lookupNamespace") {
        return Some(raise_missing_method(vm, &wrap(x.clone()), method, args));
    }
    if let XmlVal::Child(id) = x {
        match (method, args) {
            ("namespaceURI", []) => return Some(Value::str(with_node(*id, |n| n.uri.clone()))),
            ("lookupNamespace", [p]) => {
                let prefix = groovy_str(p);
                let uri = NODES.with(|n| {
                    let nodes = n.borrow();
                    let node = &nodes[*id as usize];
                    resolve_prefix(&nodes, node.parent, &node.decls, &prefix)
                });
                return Some(if uri.is_empty() {
                    Value::Undef
                } else {
                    Value::str(uri)
                });
            }
            _ => {}
        }
    }
    Some(match (method, args) {
        // `declareNamespace(prefix: uri, …)` binds prefixes for every GPath
        // over the document, and answers the receiver.
        ("declareNamespace", [m]) => {
            let entries = as_omap(m)?;
            if let Some(first) = ids.first() {
                let root = root_of(*first);
                HINTS.with(|h| {
                    let mut h = h.borrow_mut();
                    let hints = h.entry(root).or_default();
                    for (k, v) in entries {
                        let p = mapkey::text(&k);
                        hints.retain(|(hp, _)| *hp != p);
                        hints.push((p, groovy_str(&v)));
                    }
                });
            }
            wrap(x.clone())
        }
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
        ("children", []) => children(
            ids.iter()
                .flat_map(|i| child_elements(*i, Select::Any))
                .collect(),
        ),
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

/// `QName.equals`: another `QName` with the same namespace and local part, or
/// a `prefix:local` string whose prefix is this name's prefix or namespace.
/// A string without a `:` (or ending in one) is never equal. `None` when `a`
/// is no `QName`.
pub(super) fn qname_equal(a: &Value, b: &Value) -> Option<bool> {
    let XmlVal::QName { uri, local, prefix } = as_xml(a)? else {
        return None;
    };
    if let Some(XmlVal::QName {
        uri: u2, local: l2, ..
    }) = as_xml(b)
    {
        return Some(uri == u2 && local == l2);
    }
    let Value::Str(s) = b else {
        return Some(false);
    };
    Some(match split_qualified(s) {
        Some((p, l)) => (p == prefix || p == uri) && l == local,
        None => false,
    })
}

/// `QName.matches`: [`qname_equal`] with `*` as a wildcard namespace,
/// prefix or local part, and a bare local name matching an unprefixed name.
fn qname_matches(a: &Value, b: &Value) -> Option<bool> {
    let XmlVal::QName { uri, local, prefix } = as_xml(a)? else {
        return None;
    };
    let wild = |x: &str, y: &str| x == y || x == "*" || y == "*";
    if let Some(XmlVal::QName {
        uri: u2, local: l2, ..
    }) = as_xml(b)
    {
        return Some(wild(&uri, &u2) && wild(&local, &l2));
    }
    let Value::Str(s) = b else {
        return Some(false);
    };
    if s.is_empty() {
        return Some(false);
    }
    if !s.contains(':') && prefix.is_empty() {
        return Some(**s == local);
    }
    Some(match split_qualified(s) {
        Some((p, l)) => (p == prefix || p == uri || p == "*") && (l == local || l == "*"),
        None => false,
    })
}

/// A `prefix:local` string split at its last `:`; `None` without one, or
/// with nothing after it.
fn split_qualified(s: &str) -> Option<(&str, &str)> {
    let (p, l) = s.rsplit_once(':')?;
    (!l.is_empty()).then_some((p, l))
}

/// `QName.valueOf`: `{uri}local`, or a bare local name.
fn qname_value_of(text: &str) -> Result<Value, &'static str> {
    const INVALID: &str = "invalid QName literal";
    if text.is_empty() {
        return Err(INVALID);
    }
    let Some(rest) = text.strip_prefix('{') else {
        return Ok(wrap(XmlVal::QName {
            uri: String::new(),
            local: text.to_string(),
            prefix: String::new(),
        }));
    };
    match rest.find('}') {
        Some(end) if end + 1 < rest.len() => Ok(wrap(XmlVal::QName {
            uri: rest[..end].to_string(),
            local: rest[end + 1..].to_string(),
            prefix: String::new(),
        })),
        _ => Err(INVALID),
    }
}

/// `QName.getQualifiedName`: `prefix:local`, or the local part alone.
fn qualified_name(prefix: &str, local: &str) -> String {
    if prefix.is_empty() {
        local.to_string()
    } else {
        format!("{prefix}:{local}")
    }
}

/// `QName`'s static methods.
pub(super) fn static_call(vm: &mut VM, method: &str, args: &[Value]) -> Option<Value> {
    match (method, args) {
        ("valueOf", [text]) => Some(match qname_value_of(&groovy_str(text)) {
            Ok(v) => v,
            Err(message) => {
                raise(vm, "IllegalArgumentException", message);
                Value::Undef
            }
        }),
        _ => None,
    }
}

/// `QName.hashCode`: `29 * namespaceURI.hashCode() + localPart.hashCode()`.
pub(super) fn qname_hash(v: &Value) -> Option<i32> {
    let XmlVal::QName { uri, local, .. } = as_xml(v)? else {
        return None;
    };
    Some(
        string_hash(&uri)
            .wrapping_mul(29)
            .wrapping_add(string_hash(&local)),
    )
}

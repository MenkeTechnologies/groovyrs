//! `groovy.json`: `JsonOutput.toJson`, `JsonOutput.prettyPrint` and
//! `JsonSlurper.parseText`, with the renderings and value types Apache Groovy
//! 6.0.0 answers (measured on JVM 21.0.12).

use super::*;

/// `JsonOutput.toJson(value)`.
///
/// `null`, booleans and numbers print as their Java text (`1.0E20` for a
/// `double`, `2.50` for a `BigDecimal`); a string is quoted with `"`, `\`, the
/// control characters and every character past `~` escaped (`é`); a map
/// is an object keyed by its keys' text, any other collection, range or array
/// an array; a script class instance is the object of its properties, in
/// `getProperties()` order without `class`.
pub(super) fn to_json(vm: &mut VM, v: &Value) -> String {
    let mut out = String::new();
    write_json(vm, v, &mut out);
    out
}

fn write_json(vm: &mut VM, v: &Value, out: &mut String) {
    if matches!(v, Value::Undef) {
        out.push_str("null");
        return;
    }
    if let Value::Bool(b) = v {
        out.push_str(if *b { "true" } else { "false" });
        return;
    }
    if matches!(v, Value::Int(_) | Value::Float(_))
        || boxed::as_box(v).is_some()
        || is_dec_handle(v)
        || as_float_handle(v).is_some()
    {
        out.push_str(&groovy_str(v));
        return;
    }
    if let Some(entries) = as_omap(v) {
        out.push('{');
        for (i, (k, val)) in entries.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            write_string(&mapkey::text(k), out);
            out.push(':');
            write_json(vm, val, out);
        }
        out.push('}');
        return;
    }
    if let Some(inst) = as_instance(v) {
        out.push('{');
        let names: Vec<String> = class_properties(inst.class)
            .into_iter()
            .map(|p| p.name)
            .filter(|n| n != "class")
            .collect();
        for (i, name) in names.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            write_string(name, out);
            out.push(':');
            let val = get_property(vm, v, name);
            write_json(vm, &val, out);
        }
        out.push('}');
        return;
    }
    let is_text = matches!(v, Value::Str(_)) || as_gstring(v).is_some();
    if !is_text && (as_list(v).is_some() || as_set(v).is_some() || as_range(v).is_some()) {
        out.push('[');
        for (i, item) in iteration_elements(v).iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            write_json(vm, item, out);
        }
        out.push(']');
        return;
    }
    write_string(&render_value(vm, v), out);
}

/// A JSON string literal of `s`, escaped as `JsonOutput` escapes it.
fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for unit in s.encode_utf16() {
        match unit {
            0x22 => out.push_str("\\\""),
            0x5c => out.push_str("\\\\"),
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0x20..=0x7e => out.push(char::from(unit as u8)),
            _ => out.push_str(&format!("\\u{unit:04x}")),
        }
    }
    out.push('"');
}

/// `JsonOutput.prettyPrint(json)`: the text re-laid out over its tokens, four
/// spaces per level, `": "` after a key and a line per element. An empty
/// object or array still opens a line, as Groovy's does (`{\n        \n    }`).
pub(super) fn pretty_print(json: &str) -> String {
    let mut out = String::new();
    let mut indent = 0usize;
    let pad = |n: usize| " ".repeat(n * 4);
    let mut chars = json.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                out.push('"');
                while let Some(d) = chars.next() {
                    out.push(d);
                    match d {
                        '\\' => {
                            if let Some(e) = chars.next() {
                                out.push(e);
                            }
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '{' | '[' => {
                indent += 1;
                out.push(c);
                out.push('\n');
                out.push_str(&pad(indent));
            }
            '}' | ']' => {
                indent = indent.saturating_sub(1);
                out.push('\n');
                out.push_str(&pad(indent));
                out.push(c);
            }
            ',' => {
                out.push_str(",\n");
                out.push_str(&pad(indent));
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    out
}

/// `new JsonSlurper().parseText(text)`: objects as `LazyMap`s (insertion
/// ordered), arrays as `ArrayList`s, an integer as the narrowest of
/// `Integer`/`Long`/`BigInteger`, any number with a fraction or exponent as a
/// `BigDecimal`. `Err` is the `JsonException` message.
pub(super) fn parse_text(text: &str) -> Result<Value, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut p = Reader { chars, pos: 0 };
    p.skip_ws();
    let v = p.value()?;
    p.skip_ws();
    if p.pos < p.chars.len() {
        return Err(format!(
            "A JSON payload should start with an object or array. Unexpected character at index {}",
            p.pos
        ));
    }
    Ok(v)
}

struct Reader {
    chars: Vec<char>,
    pos: usize,
}

impl Reader {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    fn fail<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at index {}", self.pos))
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            self.fail(&format!("expected '{c}'"))
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"') => Ok(Value::str(self.string()?)),
            Some('t') => self.word("true", Value::bool(true)),
            Some('f') => self.word("false", Value::bool(false)),
            Some('n') => self.word("null", Value::Undef),
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            _ => self.fail("unexpected character"),
        }
    }

    fn word(&mut self, w: &str, v: Value) -> Result<Value, String> {
        let end = self.pos + w.chars().count();
        if self
            .chars
            .get(self.pos..end)
            .is_some_and(|s| s.iter().copied().eq(w.chars()))
        {
            self.pos = end;
            Ok(v)
        } else {
            self.fail("unexpected character")
        }
    }

    fn object(&mut self) -> Result<Value, String> {
        self.expect('{')?;
        let mut entries: Vec<(String, Value)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
        } else {
            loop {
                self.skip_ws();
                if self.peek() != Some('"') {
                    return self.fail("expected a string key");
                }
                let key = self.string()?;
                self.skip_ws();
                self.expect(':')?;
                self.skip_ws();
                let v = self.value()?;
                let key = mapkey::encode(&Value::str(key));
                match entries.iter_mut().find(|(k, _)| *k == key) {
                    Some(slot) => slot.1 = v,
                    None => entries.push((key, v)),
                }
                self.skip_ws();
                match self.peek() {
                    Some(',') => self.pos += 1,
                    Some('}') => {
                        self.pos += 1;
                        break;
                    }
                    _ => return self.fail("expected ',' or '}'"),
                }
            }
        }
        let map = gmap(entries);
        seq::set_flavor(
            &map,
            "org.apache.groovy.json.internal.LazyMap",
            seq::Writes::Open,
        );
        Ok(map)
    }

    fn array(&mut self) -> Result<Value, String> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(glist(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some(']') => {
                    self.pos += 1;
                    return Ok(glist(items));
                }
                _ => return self.fail("expected ',' or ']'"),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut units: Vec<u16> = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return self.fail("unterminated string");
            };
            self.pos += 1;
            match c {
                '"' => break,
                '\\' => {
                    let Some(e) = self.peek() else {
                        return self.fail("unterminated string");
                    };
                    self.pos += 1;
                    let unit = match e {
                        'n' => 0x0a,
                        't' => 0x09,
                        'r' => 0x0d,
                        'b' => 0x08,
                        'f' => 0x0c,
                        '/' => 0x2f,
                        '\\' => 0x5c,
                        '"' => 0x22,
                        'u' => {
                            let hex: String = self
                                .chars
                                .get(self.pos..self.pos + 4)
                                .map(|s| s.iter().collect())
                                .unwrap_or_default();
                            let Ok(u) = u16::from_str_radix(&hex, 16) else {
                                return self.fail("bad unicode escape");
                            };
                            self.pos += 4;
                            u
                        }
                        _ => return self.fail("bad escape"),
                    };
                    units.push(unit);
                }
                c => {
                    let mut buf = [0u16; 2];
                    units.extend_from_slice(c.encode_utf16(&mut buf));
                }
            }
        }
        Ok(String::from_utf16_lossy(&units))
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        let mut fraction = false;
        while let Some(c) = self.peek() {
            match c {
                '0'..='9' | '-' | '+' => {}
                '.' | 'e' | 'E' => fraction = true,
                _ => break,
            }
            self.pos += 1;
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        if fraction {
            return match decimal::parse(&text) {
                Some(d) => Ok(dec_value(d)),
                None => self.fail("bad number"),
            };
        }
        if let Ok(n) = text.parse::<i64>() {
            return Ok(Value::int(n));
        }
        match decimal::parse(&text) {
            Some(d) => Ok(bigint_value(d)),
            None => self.fail("bad number"),
        }
    }
}

/// `JsonBuilder`'s DSL: run `body` with a collector as its delegate, so each
/// bare call `name value` (`name { … }` nesting, `name(k: v)` a map,
/// `name a, b` a list) becomes an entry of the object it answers.
pub(super) fn build_object(vm: &mut VM, body: &Value) -> Value {
    let map = gmap(Vec::new());
    let collector = heap_push(HeapObj::JsonCollector(Box::new(map.clone())));
    DELEGATES.with(|d| d.borrow_mut().push(collector.clone()));
    let out = invoke_closure(vm, body, &[]);
    DELEGATES.with(|d| d.borrow_mut().pop());
    if let Err(e) = out {
        fault(vm, e);
    }
    map
}

/// The value one builder call's arguments make: a closure an object, one
/// argument itself, several a list, none an empty list; a map with a trailing
/// closure merges the closure's entries after the map's.
pub(super) fn builder_value(vm: &mut VM, args: &[Value]) -> Value {
    match args {
        [] => glist(Vec::new()),
        [c] if closure_meta(c).is_some() => build_object(vm, c),
        [m, c] if as_omap(m).is_some() && closure_meta(c).is_some() => {
            let mut entries = as_omap(m).unwrap_or_default();
            let built = build_object(vm, c);
            entries.extend(as_omap(&built).unwrap_or_default());
            gmap(entries)
        }
        [v] => v.clone(),
        many => glist(many.to_vec()),
    }
}

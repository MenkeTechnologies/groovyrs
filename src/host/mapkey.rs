//! Typed map keys.
//!
//! A map stores its entries under a `String` key, which is what its index,
//! its ordering and every GDK method on entries read. A `String` key is stored
//! as itself; any other key (`1`, `1.5`, `true`, `null`, a list, an object) is
//! stored under an *encoded* string that begins with [`MARK`] and names the
//! key's type, so `[1: 'a']` and `['1': 'a']` are two different maps, a key
//! comes back as the `Integer` it was, and `TreeMap` / `HashMap` order by the
//! key's own value.
//!
//! The encoding only exists at the map's boundary: [`encode`] turns a key value
//! into the stored string where a key goes in, and [`decode`] / [`text`] turn it
//! back where a key comes out (a closure argument, `keySet()`, a rendering).
//! Everything between — copying, merging, filtering, grouping — moves the string
//! untouched, which is why the type survives it.

use super::*;

/// First character of an encoded (non-`String`) key.
const MARK: char = '\u{1}';

thread_local! {
    /// The object behind an encoded key that has to come back as itself — a
    /// list, a set, a map, a range, an instance — by encoded string.
    static OBJECTS: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

/// Forget every registered key object (the heap was cleared).
pub(super) fn reset() {
    OBJECTS.with(|o| o.borrow_mut().clear());
}

/// The stored key for the key value `v`.
pub(super) fn encode(v: &Value) -> String {
    match v {
        Value::Str(s) => s.to_string(),
        Value::Int(n) => format!("{MARK}i{n}"),
        Value::Float(f) => format!("{MARK}f{:016x}", f.to_bits()),
        Value::Bool(b) => format!("{MARK}b{}", u8::from(*b)),
        Value::Undef => format!("{MARK}n"),
        _ => {
            if as_gstring(v).is_some() {
                return groovy_str(v);
            }
            if let Some(f) = as_float_handle(v) {
                return format!("{MARK}F{:08x}", f.to_bits());
            }
            if as_bigint(v).is_some() {
                return format!("{MARK}g{}", groovy_str(v));
            }
            if as_dec(v).is_some() {
                return format!("{MARK}d{}", groovy_str(v));
            }
            if let Some((_, text)) = as_buffer(v) {
                return text;
            }
            // A list, set, map or range is a key by *content*; an instance, a
            // closure or anything else by identity.
            let by_content = as_list_raw(v).is_some()
                || as_set(v).is_some()
                || as_omap(v).is_some()
                || as_range(v).is_some()
                || matches!(v, Value::Array(_));
            let enc = match (by_content, v) {
                (true, _) => format!("{MARK}l{}", inspect_value(v)),
                (false, Value::Obj(id)) => format!("{MARK}h{id}"),
                _ => format!("{MARK}o{}", groovy_str(v)),
            };
            OBJECTS.with(|o| {
                o.borrow_mut()
                    .entry(enc.clone())
                    .or_insert_with(|| v.clone());
            });
            enc
        }
    }
}

/// The key value a stored key stands for.
pub(super) fn decode(k: &str) -> Value {
    let mut chars = k.chars();
    if chars.next() != Some(MARK) {
        return Value::str(k.to_string());
    }
    let tag = chars.next();
    let rest = chars.as_str();
    match tag {
        Some('i') => rest.parse::<i64>().map_or(Value::Undef, Value::int),
        Some('f') => u64::from_str_radix(rest, 16)
            .map_or(Value::Undef, |b| Value::float(f64::from_bits(b))),
        Some('b') => Value::bool(rest == "1"),
        Some('n') => Value::Undef,
        Some('F') => u32::from_str_radix(rest, 16)
            .map_or(Value::Undef, |b| float_value(f32::from_bits(b))),
        Some('g') => decimal::parse_java(rest).map_or(Value::Undef, bigint_value),
        Some('d') => decimal::parse_java(rest).map_or(Value::Undef, dec_value),
        Some('h') => rest.parse::<u32>().map_or(Value::Undef, Value::Obj),
        _ => OBJECTS
            .with(|o| o.borrow().get(k).cloned())
            .unwrap_or_else(|| Value::str(rest.to_string())),
    }
}

/// How a stored key renders: a `String` as itself, anything else the way its
/// value prints.
pub(super) fn text(k: &str) -> String {
    if k.starts_with(MARK) {
        groovy_str(&decode(k))
    } else {
        k.to_string()
    }
}

/// How `inspect()` renders a stored key: a `String` quoted, anything else bare.
pub(super) fn inspect(k: &str) -> String {
    if k.starts_with(MARK) {
        inspect_value(&decode(k))
    } else {
        format!("'{}'", inspect_escape(k, '\''))
    }
}

/// Order two stored keys by their values (`TreeMap`, `sort()`, `firstKey()`).
pub(super) fn cmp(a: &str, b: &str) -> std::cmp::Ordering {
    if !a.starts_with(MARK) && !b.starts_with(MARK) {
        return utf16_cmp(a, b);
    }
    natural_order(&decode(a), &decode(b))
}

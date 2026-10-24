//! `java.lang.Long`, `Short` and `Byte` values small enough to be `Integer`s.
//!
//! fusevm has one integer type, so a `Value::Int` inside 32-bit range is an
//! `Integer` and one outside it a `Long` (the magnitude rule). A `Long`,
//! `Short` or `Byte` whose value an `Integer` could hold needs a value of its
//! own wherever its class can be observed — `getClass()`, `equals` (Java's
//! `Long.equals(Integer)` is `false`, so `2L in [1, 2]` is `false`),
//! `hashCode`, `instanceof`, a map key. Such a value is a *box*:
//!
//! - a `Long` is `Value::Status(n)`: fusevm reads its number through
//!   `to_int` / `to_float` / `to_str` as it reads a `Value::Int`'s, and
//!   delegates any arithmetic on it to the numeric hook, which keeps the kind;
//! - a `Short` or a `Byte` is `Value::Ref` over `[tag, n]`.
//!
//! Boxes are made where the kind is known — a cast, a conversion method, an
//! `L` literal or statically-`Long` expression where it leaves compiled code,
//! arithmetic on a `Long` box — and read back to a plain `Value::Int`
//! ([`unbox`]) wherever only the number matters.

use super::*;

/// A Java integer class.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum IntKind {
    Integer,
    Long,
    Short,
    Byte,
}

impl IntKind {
    /// The qualified class name.
    pub(super) fn class_name(self) -> &'static str {
        match self {
            IntKind::Integer => "java.lang.Integer",
            IntKind::Long => "java.lang.Long",
            IntKind::Short => "java.lang.Short",
            IntKind::Byte => "java.lang.Byte",
        }
    }
}

const SHORT_TAG: i64 = 16;
const BYTE_TAG: i64 = 8;

/// The kind and value of a box; `None` for anything else, a plain
/// `Value::Int` included.
pub(super) fn as_box(v: &Value) -> Option<(IntKind, i64)> {
    match v {
        Value::Status(n) => Some((IntKind::Long, i64::from(*n))),
        Value::Ref(inner) => match &**inner {
            Value::Array(a) => match a.as_slice() {
                [Value::Int(SHORT_TAG), Value::Int(n)] => Some((IntKind::Short, *n)),
                [Value::Int(BYTE_TAG), Value::Int(n)] => Some((IntKind::Byte, *n)),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// The kind and value of any Java integer value: a box, or a `Value::Int`
/// (an `Integer`, or a `Long` by magnitude).
pub(super) fn int_view(v: &Value) -> Option<(IntKind, i64)> {
    match v {
        Value::Int(n) => Some((
            if i32::try_from(*n).is_ok() {
                IntKind::Integer
            } else {
                IntKind::Long
            },
            *n,
        )),
        _ => as_box(v),
    }
}

/// `v` with a box read back to its plain `Value::Int`.
pub(super) fn unbox(v: Value) -> Value {
    match as_box(&v) {
        Some((_, n)) => Value::Int(n),
        None => v,
    }
}

/// Whether `v` is a `Long` box.
pub(super) fn is_long_box(v: &Value) -> bool {
    matches!(v, Value::Status(_))
}

/// A `java.lang.Long`: a box when an `Integer` could hold it, else the plain
/// value the magnitude rule already reads as a `Long`.
pub(super) fn long_value(n: i64) -> Value {
    match i32::try_from(n) {
        Ok(small) => Value::Status(small),
        Err(_) => Value::Int(n),
    }
}

/// A value of `kind`, narrowed to its width (`300` as a `Byte` is `44`).
pub(super) fn of_kind(kind: IntKind, n: i64) -> Value {
    let tagged =
        |tag: i64, n: i64| Value::Ref(Box::new(Value::array(vec![Value::Int(tag), Value::Int(n)])));
    match kind {
        IntKind::Integer => Value::Int(i64::from(n as i32)),
        IntKind::Long => long_value(n),
        IntKind::Short => tagged(SHORT_TAG, i64::from(n as i16)),
        IntKind::Byte => tagged(BYTE_TAG, i64::from(n as i8)),
    }
}

/// The integer kind a type name designates (`long`, `Long`,
/// `java.lang.Long`, …), if it names one.
pub(super) fn kind_named(ty: &str) -> Option<IntKind> {
    Some(match ty.rsplit('.').next().unwrap_or(ty) {
        "int" | "Integer" => IntKind::Integer,
        "long" | "Long" => IntKind::Long,
        "short" | "Short" => IntKind::Short,
        "byte" | "Byte" => IntKind::Byte,
        _ => return None,
    })
}

/// `hashCode()` of a Java integer: a `Long` folds its high half in.
pub(super) fn int_hash(kind: IntKind, n: i64) -> i32 {
    match kind {
        IntKind::Long => (n ^ ((n as u64) >> 32) as i64) as i32,
        _ => n as i32,
    }
}

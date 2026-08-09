//! The strict numeric hook, called directly with the operand pairs fusevm
//! delegates to it.
//!
//! The pair that matters is a `long` past 2^53 against a `double`: reading the
//! integer as an `f64` is inexact, so fusevm hands the question to the host
//! rather than answering from a rounded value. Groovy's answer *is* the rounded
//! one — binary numeric promotion widens the integer to `double` before the
//! operator runs — and every expectation below is the literal output of Apache
//! Groovy 5.0.8 (`groovy examples/NumericPromotion.groovy`), not a value
//! derived from groovyrs.
//!
//! `examples/NumericPromotion.groovy` covers the same expressions end to end;
//! this file pins the hook itself, which the currently pinned fusevm reaches
//! only for a non-numeric operand.

use fusevm::{NumOp, Value};
use groovyrs::host::numeric_hook;

/// `3**34` — the smallest power of three past 2^53, so its `f64` image
/// (`1.6677181699666568E16`) is a *neighbouring* integer, not itself.
const L: i64 = 16_677_181_699_666_569;

/// `L`'s `f64` image: a different integer that Groovy still compares equal to
/// `L`, because the comparison happens after promotion.
const L_AS_DOUBLE: f64 = 1.6677181699666568E16;

/// The hook's `f64` answer for `a op b`, or a panic naming what came back.
fn f(op: NumOp, a: Value, b: Value) -> f64 {
    match numeric_hook(op, &a, &b) {
        Ok(Value::Float(x)) => x,
        other => panic!("expected a Double from {op:?}, got {other:?}"),
    }
}

/// The hook's `bool` answer for `a op b`, or a panic naming what came back.
fn b(op: NumOp, a: Value, b: Value) -> bool {
    match numeric_hook(op, &a, &b) {
        Ok(Value::Bool(x)) => x,
        other => panic!("expected a Boolean from {op:?}, got {other:?}"),
    }
}

#[test]
fn large_long_with_double_promotes_to_double_arithmetic() {
    let (l, y) = (Value::int(L), Value::float(2.0));
    // Groovy: 1.667718169966657E16 both ways.
    assert_eq!(f(NumOp::Add, l.clone(), y.clone()), 1.667718169966657E16);
    assert_eq!(f(NumOp::Add, y.clone(), l.clone()), 1.667718169966657E16);
    assert_eq!(f(NumOp::Sub, l.clone(), y.clone()), 1.6677181699666566E16);
    assert_eq!(f(NumOp::Sub, y.clone(), l.clone()), -1.6677181699666566E16);
    assert_eq!(f(NumOp::Mul, l.clone(), y.clone()), 3.3354363399333136E16);
    assert_eq!(f(NumOp::Mul, y.clone(), l.clone()), 3.3354363399333136E16);
    // `/` and `**` lower to builtins, so the hook is not on their runtime path;
    // it still has to agree with them, and both promote the same way.
    assert_eq!(f(NumOp::Div, l.clone(), y.clone()), 8.338590849833284E15);
    assert_eq!(f(NumOp::Div, y.clone(), l.clone()), 1.19924339496762E-16);
    assert_eq!(f(NumOp::Mod, l.clone(), y.clone()), 0.0);
    assert_eq!(f(NumOp::Mod, y.clone(), l.clone()), 2.0);
    assert_eq!(f(NumOp::Pow, l.clone(), y.clone()), 2.781283894436935E32);
    assert_eq!(f(NumOp::Pow, y.clone(), l.clone()), f64::INFINITY);
}

#[test]
fn large_long_with_double_compares_as_double() {
    let (l, y) = (Value::int(L), Value::float(2.0));
    // Groovy orders these numerically. Rendering them as text and comparing the
    // strings — the hook's fallback for a non-numeric operand — would order
    // "16677181699666569" before "2.0" and get every one of these backwards.
    assert!(!b(NumOp::Eq, l.clone(), y.clone()));
    assert!(!b(NumOp::Eq, y.clone(), l.clone()));
    assert!(b(NumOp::Ne, l.clone(), y.clone()));
    assert!(b(NumOp::Ne, y.clone(), l.clone()));
    assert!(!b(NumOp::Lt, l.clone(), y.clone()));
    assert!(b(NumOp::Lt, y.clone(), l.clone()));
    assert!(b(NumOp::Gt, l.clone(), y.clone()));
    assert!(!b(NumOp::Gt, y.clone(), l.clone()));
    assert!(!b(NumOp::Le, l.clone(), y.clone()));
    assert!(b(NumOp::Le, y.clone(), l.clone()));
    assert!(b(NumOp::Ge, l.clone(), y.clone()));
    assert!(!b(NumOp::Ge, y.clone(), l.clone()));
}

#[test]
fn a_long_equals_its_own_rounded_double_image() {
    let (l, n) = (Value::int(L), Value::float(L_AS_DOUBLE));
    // The two are different integers — this is the rounding fusevm refuses to
    // decide on its own — and Groovy still calls them equal, because `L` is
    // widened to `n`'s exact value before `==` runs.
    assert_ne!(L, L_AS_DOUBLE as i64);
    assert!(b(NumOp::Eq, l.clone(), n.clone()));
    assert!(b(NumOp::Eq, n.clone(), l.clone()));
    assert!(!b(NumOp::Lt, l.clone(), n.clone()));
    assert!(!b(NumOp::Gt, l.clone(), n.clone()));
    assert_eq!(f(NumOp::Add, l, n), 3.3354363399333136E16);
}

#[test]
fn small_and_all_double_pairs_take_the_same_path() {
    // Nothing about the fix is conditional on magnitude: the gate is operand
    // shape, so a pair fusevm answers natively today gets the identical answer
    // if it is ever delegated.
    assert_eq!(f(NumOp::Add, Value::int(3), Value::float(2.5)), 5.5);
    assert_eq!(f(NumOp::Add, Value::float(1.5), Value::float(2.5)), 4.0);
    assert_eq!(f(NumOp::Sub, Value::float(2.5), Value::int(3)), -0.5);
    assert!(!b(NumOp::Lt, Value::int(3), Value::float(2.5)));
}

#[test]
fn non_numeric_operands_keep_their_groovy_operators() {
    // The promotion gate must not swallow `+`'s String overload, and a
    // `Boolean` is not one of its numbers: real Groovy 5.0.8 answers
    // `true + 2.0d` with a `MissingMethodException`, so a `Boolean` must not
    // start doing double arithmetic here. groovyrs concatenates it instead —
    // a pre-existing gap this test pins rather than endorses.
    let cat = |a: Value, bb: Value| match numeric_hook(NumOp::Add, &a, &bb) {
        Ok(Value::Str(s)) => s.to_string(),
        other => panic!("expected concatenation, got {other:?}"),
    };
    assert_eq!(
        cat(Value::str("x".to_string()), Value::int(L)),
        "x16677181699666569"
    );
    assert_eq!(cat(Value::str("x".to_string()), Value::float(2.0)), "x2.0");
    assert_eq!(cat(Value::float(2.0), Value::str("x".to_string())), "2.0x");
    assert_eq!(cat(Value::bool(true), Value::float(2.0)), "true2.0");
    // Groovy's `==` between a String and a number is false, never a coercion.
    assert!(!b(
        NumOp::Eq,
        Value::str("2.0".to_string()),
        Value::float(2.0)
    ));
}

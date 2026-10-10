//! The `java.math` surface beyond the operators: `BigInteger`'s number-theory
//! and bit methods, `BigDecimal`'s `MathContext` forms and decimal-point
//! movers, the legacy `BigDecimal.ROUND_*` constants, and the `RoundingMode`
//! statics.

use super::*;
use bigdecimal::num_bigint::{BigInt, Sign};
use bigdecimal::ToPrimitive;
use std::num::NonZeroU64;

/// `java.math.RoundingMode`'s constants in ordinal order.
const MODES: [&str; 8] = [
    "UP",
    "DOWN",
    "CEILING",
    "FLOOR",
    "HALF_UP",
    "HALF_DOWN",
    "HALF_EVEN",
    "UNNECESSARY",
];

/// The `RoundingMode` name a mode argument spells: the constant itself, or the
/// legacy `BigDecimal.ROUND_*` integer (`4` is `HALF_UP`).
pub(super) fn rounding_name(v: &Value) -> String {
    match v {
        Value::Int(n) if (0..8).contains(n) => MODES[*n as usize].to_string(),
        other => groovy_str(other),
    }
}

/// The `BigDecimal.ROUND_*` int constants.
pub(super) fn legacy_constant(name: &str) -> Option<Value> {
    let mode = name.strip_prefix("ROUND_")?;
    MODES
        .iter()
        .position(|m| *m == mode)
        .map(|i| Value::int(i as i64))
}

/// `RoundingMode.values()` / `valueOf(name)`.
pub(super) fn rounding_mode_static(vm: &mut VM, method: &str, args: &[Value]) -> Option<Value> {
    match (method, args.len()) {
        ("values", 0) => Some(Value::array(
            MODES.iter().map(|m| Value::str(m.to_string())).collect(),
        )),
        ("valueOf", 1) => {
            let name = rounding_name(&args[0]);
            if MODES.contains(&name.as_str()) {
                return Some(Value::str(name));
            }
            raise(
                vm,
                "IllegalArgumentException",
                &format!("No enum constant java.math.RoundingMode.{name}"),
            );
            Some(Value::Undef)
        }
        _ => None,
    }
}

// ── MathContext ─────────────────────────────────────────────────────────────

/// A `java.math.MathContext`: a significant-digit budget (0 is unlimited) and
/// the rounding mode that applies when the budget is exceeded.
#[derive(Clone)]
pub(super) struct MathCtx {
    pub(super) precision: u64,
    pub(super) mode: String,
}

pub(super) fn math_ctx(v: &Value) -> Option<MathCtx> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::MathCtx(c)) => Some(c.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// `new MathContext(precision [, mode])`.
pub(super) fn new_math_ctx(vm: &mut VM, args: &[Value]) -> Value {
    let precision = match args.first().and_then(as_i64) {
        Some(p) if p >= 0 => p as u64,
        Some(_) => {
            raise(vm, "IllegalArgumentException", "Digits < 0");
            return Value::Undef;
        }
        None => 0,
    };
    let mode = args
        .get(1)
        .map_or_else(|| "HALF_UP".to_string(), rounding_name);
    heap_push(HeapObj::MathCtx(MathCtx { precision, mode }))
}

/// `MathContext.DECIMAL32` and friends.
pub(super) fn math_ctx_constant(name: &str) -> Option<Value> {
    let (precision, mode) = match name {
        "UNLIMITED" => (0, "HALF_UP"),
        "DECIMAL32" => (7, "HALF_EVEN"),
        "DECIMAL64" => (16, "HALF_EVEN"),
        "DECIMAL128" => (34, "HALF_EVEN"),
        _ => return None,
    };
    Some(heap_push(HeapObj::MathCtx(MathCtx {
        precision,
        mode: mode.to_string(),
    })))
}

pub(super) fn math_ctx_str(c: &MathCtx) -> String {
    format!("precision={} roundingMode={}", c.precision, c.mode)
}

pub(super) fn math_ctx_method(c: &MathCtx, method: &str, args: &[Value]) -> Option<Value> {
    match (method, args.len()) {
        ("getPrecision", 0) => Some(Value::int(c.precision as i64)),
        ("getRoundingMode", 0) => Some(Value::str(c.mode.clone())),
        ("toString", 0) => Some(Value::str(math_ctx_str(c))),
        _ => None,
    }
}

/// `d` rounded to the context's digit budget.
fn round_to(d: &BigDecimal, c: &MathCtx) -> Option<BigDecimal> {
    let Some(digits) = NonZeroU64::new(c.precision) else {
        return Some(d.clone());
    };
    if d.digits() <= c.precision {
        return Some(d.clone());
    }
    let mode = match c.mode.as_str() {
        "UP" => bigdecimal::RoundingMode::Up,
        "DOWN" => bigdecimal::RoundingMode::Down,
        "CEILING" => bigdecimal::RoundingMode::Ceiling,
        "FLOOR" => bigdecimal::RoundingMode::Floor,
        "HALF_UP" => bigdecimal::RoundingMode::HalfUp,
        "HALF_DOWN" => bigdecimal::RoundingMode::HalfDown,
        "HALF_EVEN" => bigdecimal::RoundingMode::HalfEven,
        _ => return None,
    };
    let drop = d.digits() - digits.get();
    let rounded = d.with_scale_round(decimal::scale_of(d) - drop as i64, mode);
    // A carry out of the top digit (999.5 to 1000) costs one more.
    Some(if rounded.digits() > digits.get() {
        rounded.with_scale_round(decimal::scale_of(&rounded) - 1, mode)
    } else {
        rounded
    })
}

// ── BigInteger number theory ────────────────────────────────────────────────

fn big_of(d: &BigDecimal) -> BigInt {
    d.as_bigint_and_exponent().0
}

fn gcd(a: &BigInt, b: &BigInt) -> BigInt {
    let (mut a, mut b) = (a.abs(), b.abs());
    while !b.is_zero() {
        let r = &a % &b;
        a = b;
        b = r;
    }
    a
}

/// Miller–Rabin over the first twelve primes: exact below 3.3e24, and
/// overwhelmingly reliable above it.
fn is_probable_prime(n: &BigInt) -> bool {
    const BASES: [u32; 12] = [2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37];
    let n = n.abs();
    if n < BigInt::from(2) {
        return false;
    }
    for p in BASES {
        let p = BigInt::from(p);
        if n == p {
            return true;
        }
        if (&n % &p).is_zero() {
            return false;
        }
    }
    let one = BigInt::from(1);
    let n_1 = &n - &one;
    let mut d = n_1.clone();
    let mut r = 0u32;
    while (&d % 2u32).is_zero() {
        d /= 2u32;
        r += 1;
    }
    'bases: for a in BASES {
        let mut x = BigInt::from(a).modpow(&d, &n);
        if x == one || x == n_1 {
            continue;
        }
        for _ in 1..r {
            x = (&x * &x) % &n;
            if x == n_1 {
                continue 'bases;
            }
        }
        return false;
    }
    true
}

/// `a⁻¹ mod m` by the extended Euclidean algorithm, `None` when not coprime.
fn mod_inverse(a: &BigInt, m: &BigInt) -> Option<BigInt> {
    let (mut old_r, mut r) = (a.modpow(&BigInt::from(1), m), m.clone());
    let (mut old_s, mut s) = (BigInt::from(1), BigInt::from(0));
    while !r.is_zero() {
        let q = &old_r / &r;
        let next_r = &old_r - &q * &r;
        old_r = std::mem::replace(&mut r, next_r);
        let next_s = &old_s - &q * &s;
        old_s = std::mem::replace(&mut s, next_s);
    }
    if old_r != BigInt::from(1) {
        return None;
    }
    Some(((old_s % m) + m) % m)
}

/// `BigInteger.bitLength()`: the minimal two's-complement width, sign excluded.
fn bit_length(n: &BigInt) -> i64 {
    if n.sign() == Sign::Minus {
        (-n - 1u32).bits() as i64
    } else {
        n.bits() as i64
    }
}

/// `BigInteger.bitCount()`: set bits for a non-negative value, bits differing
/// from the sign bit for a negative one.
fn bit_count(n: &BigInt) -> i64 {
    let magnitude = if n.sign() == Sign::Minus {
        -n - 1u32
    } else {
        n.clone()
    };
    magnitude.to_biguint().map_or(0, |u| {
        u.to_radix_le(2).iter().filter(|b| **b == 1).count() as i64
    })
}

/// The `BigInteger` methods past the operators. `None` is a dispatch miss.
fn integer_method(vm: &mut VM, d: &BigDecimal, method: &str, args: &[Value]) -> Option<Value> {
    let n = big_of(d);
    let arg_int = |i: usize| args.get(i).and_then(as_exact_dec).map(|a| big_of(&a));
    let result = |n: BigInt| bigint_value(BigDecimal::from_bigint(n, 0));
    Some(match (method, args.len()) {
        ("signum", 0) => Value::int(match n.sign() {
            Sign::Minus => -1,
            Sign::NoSign => 0,
            Sign::Plus => 1,
        }),
        ("gcd", 1) => result(gcd(&n, &arg_int(0)?)),
        ("sqrt", 0) => {
            if n.sign() == Sign::Minus {
                raise(vm, "ArithmeticException", "Negative BigInteger");
                return Some(Value::Undef);
            }
            result(n.sqrt())
        }
        ("bitLength", 0) => Value::int(bit_length(&n)),
        ("bitCount", 0) => Value::int(bit_count(&n)),
        ("testBit", 1) => {
            let i = args[0].to_int();
            if i < 0 {
                raise(vm, "ArithmeticException", "Negative bit address");
                return Some(Value::Undef);
            }
            Value::bool(!((&n >> i as usize) & BigInt::from(1)).is_zero())
        }
        ("setBit" | "clearBit" | "flipBit", 1) => {
            let i = args[0].to_int();
            if i < 0 {
                raise(vm, "ArithmeticException", "Negative bit address");
                return Some(Value::Undef);
            }
            let mask = BigInt::from(1) << i as usize;
            let set = !((&n >> i as usize) & BigInt::from(1)).is_zero();
            result(match method {
                "setBit" if set => n,
                "setBit" => n + mask,
                "clearBit" if set => n - mask,
                "clearBit" => n,
                _ if set => n - mask,
                _ => n + mask,
            })
        }
        ("getLowestSetBit", 0) => Value::int(if n.is_zero() {
            -1
        } else {
            let mut i = 0usize;
            while ((&n >> i) & BigInt::from(1)).is_zero() {
                i += 1;
            }
            i as i64
        }),
        ("isProbablePrime", 1) => Value::bool(is_probable_prime(&n)),
        ("nextProbablePrime", 0) => {
            if n.sign() == Sign::Minus {
                raise(
                    vm,
                    "ArithmeticException",
                    &format!("start < 0: {}", decimal::to_groovy_string(d)),
                );
                return Some(Value::Undef);
            }
            let mut c = n + 1u32;
            while !is_probable_prime(&c) {
                c += 1u32;
            }
            result(c)
        }
        ("modPow", 2) => {
            let (e, m) = (arg_int(0)?, arg_int(1)?);
            if m.sign() != Sign::Plus {
                raise(
                    vm,
                    "ArithmeticException",
                    "BigInteger: modulus not positive",
                );
                return Some(Value::Undef);
            }
            let (base, e) = if e.sign() == Sign::Minus {
                match mod_inverse(&n, &m) {
                    Some(inv) => (inv, -e),
                    None => {
                        raise(vm, "ArithmeticException", "BigInteger not invertible.");
                        return Some(Value::Undef);
                    }
                }
            } else {
                (n, e)
            };
            let base = ((base % &m) + &m) % &m;
            result(base.modpow(&e, &m))
        }
        ("modInverse", 1) => {
            let m = arg_int(0)?;
            if m.sign() != Sign::Plus {
                raise(
                    vm,
                    "ArithmeticException",
                    "BigInteger: modulus not positive",
                );
                return Some(Value::Undef);
            }
            match mod_inverse(&n, &m) {
                Some(inv) => result(inv),
                None => {
                    raise(vm, "ArithmeticException", "BigInteger not invertible.");
                    Value::Undef
                }
            }
        }
        ("andNot", 1) => result(&n & !arg_int(0)?),
        ("intValueExact" | "longValueExact", 0) => {
            let (lo, hi, what) = if method == "intValueExact" {
                (i64::from(i32::MIN), i64::from(i32::MAX), "int")
            } else {
                (i64::MIN, i64::MAX, "long")
            };
            match n.to_i64().filter(|v| (lo..=hi).contains(v)) {
                Some(v) => Value::int(v),
                None => {
                    raise(
                        vm,
                        "ArithmeticException",
                        &format!("BigInteger out of {what} range"),
                    );
                    Value::Undef
                }
            }
        }
        _ => return None,
    })
}

// ── BigDecimal ──────────────────────────────────────────────────────────────

/// `BigDecimal.toEngineeringString`: the exponent is a multiple of three.
fn engineering_string(d: &BigDecimal) -> String {
    let (unscaled, scale) = d.as_bigint_and_exponent();
    let digits = unscaled.abs().to_string();
    let sign = if unscaled.sign() == Sign::Minus {
        "-"
    } else {
        ""
    };
    let adjusted = digits.len() as i64 - 1 - scale;
    // Plain notation, exactly as `toString` decides it.
    if scale >= 0 && adjusted >= -6 {
        return decimal::to_groovy_string(d);
    }
    let sig = adjusted.rem_euclid(3);
    let exp = adjusted - sig;
    let mut int_digits = digits.clone();
    let mut frac = String::new();
    if unscaled.is_zero() {
        return decimal::to_groovy_string(d);
    }
    let need = (sig + 1) as usize;
    while int_digits.len() < need {
        int_digits.push('0');
    }
    if int_digits.len() > need {
        frac = int_digits.split_off(need);
    }
    let mut out = format!("{sign}{int_digits}");
    if !frac.is_empty() {
        out.push('.');
        out.push_str(&frac);
    }
    if exp != 0 {
        out.push_str(&format!("E{}{exp}", if exp > 0 { "+" } else { "" }));
    }
    out
}

fn decimal_method(
    vm: &mut VM,
    recv: &Value,
    d: &BigDecimal,
    method: &str,
    args: &[Value],
) -> Option<Value> {
    let ctx = args.last().and_then(math_ctx);
    let other = args.first().and_then(as_exact_dec);
    Some(match (method, args.len()) {
        ("signum", 0) => Value::int(if d.is_zero() {
            0
        } else if d.is_negative() {
            -1
        } else {
            1
        }),
        ("max" | "min", 1) => {
            let o = other?;
            let ge = decimal::cmp(d, &o) != std::cmp::Ordering::Less;
            let le = decimal::cmp(d, &o) != std::cmp::Ordering::Greater;
            let keep_self = if method == "max" { ge } else { le };
            if keep_self {
                recv.clone()
            } else {
                args[0].clone()
            }
        }
        ("toEngineeringString", 0) => Value::str(engineering_string(d)),
        ("movePointLeft" | "movePointRight", 1) => {
            let n = args[0].to_int();
            let n = if method == "movePointLeft" { n } else { -n };
            let moved = BigDecimal::from_bigint(big_of(d), decimal::scale_of(d) + n);
            // A negative result scale is clamped back to zero, as Java's does.
            dec_value(if decimal::scale_of(&moved) < 0 {
                moved.with_scale(0)
            } else {
                moved
            })
        }
        ("scaleByPowerOfTen", 1) => dec_value(BigDecimal::from_bigint(
            big_of(d),
            decimal::scale_of(d) - args[0].to_int(),
        )),
        ("ulp", 0) => dec_value(BigDecimal::from_bigint(
            BigInt::from(1),
            decimal::scale_of(d),
        )),
        ("divideToIntegralValue", 1) => {
            let y = other?;
            if y.is_zero() {
                raise(vm, "ArithmeticException", "Division by zero");
                return Some(Value::Undef);
            }
            let q = decimal::divide_to_integral(d, &y);
            // Java prefers the scale `this.scale - divisor.scale`, floored at 0.
            let preferred = (decimal::scale_of(d) - decimal::scale_of(&y)).max(0);
            dec_value(q.with_scale(preferred))
        }
        ("round" | "plus", 1) => dec_value(round_to(d, &ctx?)?),
        ("add" | "subtract" | "multiply", 2) => {
            let y = other?;
            let exact = match method {
                "add" => decimal::add(d, &y),
                "subtract" => decimal::sub(d, &y),
                _ => decimal::mul(d, &y),
            };
            dec_value(round_to(&exact, &ctx?)?)
        }
        ("divide", 2) if ctx.is_some() => {
            let y = other?;
            let c = ctx?;
            if y.is_zero() {
                raise(vm, "ArithmeticException", "Division by zero");
                return Some(Value::Undef);
            }
            if let Some(exact) = decimal::exact_divide(d, &y) {
                return Some(dec_value(round_to(&exact, &c)?));
            }
            if c.precision == 0 {
                raise(
                    vm,
                    "ArithmeticException",
                    "Non-terminating decimal expansion; no exact representable decimal result.",
                );
                return Some(Value::Undef);
            }
            // Thirty guard digits stand in for the exact tail the rounding
            // mode would inspect.
            let wide = decimal::divide_to_precision(d, &y, c.precision + 30);
            dec_value(round_to(&wide, &c)?)
        }
        ("sqrt", 1) => {
            let c = ctx?;
            if d.is_negative() {
                raise(
                    vm,
                    "ArithmeticException",
                    "Attempted square root of negative BigDecimal",
                );
                return Some(Value::Undef);
            }
            let digits = if c.precision == 0 { 34 } else { c.precision };
            let root = d.sqrt()?;
            if decimal::mul(&root, &root) == *d {
                // An exact root keeps the preferred scale `ceil(scale / 2)`,
                // after its trailing zeros are stripped.
                let preferred = (decimal::scale_of(d) + 1).div_euclid(2);
                let stripped = decimal::strip_trailing_zeros(&root);
                let scale = decimal::scale_of(&stripped).max(preferred);
                return Some(dec_value(stripped.with_scale(scale)));
            }
            dec_value(round_to(
                &root,
                &MathCtx {
                    precision: digits,
                    mode: c.mode,
                },
            )?)
        }
        ("intValueExact" | "longValueExact", 0) => {
            if decimal::scale_of(d) > 0 && decimal::truncate_to_scale(d, 0) != *d {
                raise(vm, "ArithmeticException", "Rounding necessary");
                return Some(Value::Undef);
            }
            let whole = decimal::to_big_integer(d);
            let (lo, hi) = if method == "intValueExact" {
                (i64::from(i32::MIN), i64::from(i32::MAX))
            } else {
                (i64::MIN, i64::MAX)
            };
            match whole.to_i64().filter(|v| (lo..=hi).contains(v)) {
                Some(v) => Value::int(v),
                None => {
                    raise(vm, "ArithmeticException", "Overflow");
                    Value::Undef
                }
            }
        }
        ("toBigIntegerExact", 0) => {
            if decimal::scale_of(d) <= 0 || decimal::truncate_to_scale(d, 0) == *d {
                bigint_value(decimal::to_big_integer(d))
            } else {
                raise(vm, "ArithmeticException", "Rounding necessary");
                Value::Undef
            }
        }
        _ => return None,
    })
}

/// The extra instance methods of a `BigInteger` (`big`) or `BigDecimal`.
pub(super) fn extra_method(
    vm: &mut VM,
    recv: &Value,
    d: &BigDecimal,
    big: bool,
    method: &str,
    args: &[Value],
) -> Option<Value> {
    if big {
        integer_method(vm, d, method, args).or_else(|| match (method, args.len()) {
            ("signum" | "max" | "min", _) => decimal_method(vm, recv, d, method, args),
            _ => None,
        })
    } else {
        decimal_method(vm, recv, d, method, args)
    }
}

/// `BigInteger.valueOf(long)` and `BigDecimal.valueOf(long, scale)`.
pub(super) fn value_of(class: &str, args: &[Value]) -> Option<Value> {
    match (class, args.len()) {
        ("BigInteger", 1) => Some(bigint_value(decimal::from_i64(as_i64(&args[0])?))),
        ("BigDecimal", 2) => Some(dec_value(BigDecimal::from_bigint(
            BigInt::from(as_i64(&args[0])?),
            as_i64(&args[1])?,
        ))),
        _ => None,
    }
}

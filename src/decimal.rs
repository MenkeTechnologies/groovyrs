//! Groovy's decimal value model: `java.math.BigDecimal`.
//!
//! An unsuffixed Groovy decimal literal (`1.50`, `2.5e7`) is a `BigDecimal`, not
//! a `double`. That is not a formatting detail — a `BigDecimal` is an exact
//! (unscaled value, scale) pair, so it prints through `BigDecimal.toString`
//! (which switches to `E+n` form outside a narrow plain-notation window) and its
//! arithmetic *accumulates scale*: `1.25 * 0` is `0.00`, `2.5e7 + 1` is
//! `25000001` (scale 0, no trailing `.0`), and `1.5e300 * 1.5e300` is `2.25E+600`
//! rather than an `f64` infinity. Only a `d`/`f`-suffixed literal is an IEEE
//! double, which keeps its own `Double.toString` rules ([`format_double`]).
//!
//! This module is the pure value model: parsing, rendering, and the five
//! arithmetic operations with Groovy's exact scale rules. [`crate::host`] wires
//! it into the fusevm object heap (a decimal rides a `Value::Obj` handle), the
//! strict numeric hook, and the `GDIV` division builtin.
//!
//! Every rule below is byte-verified against Apache Groovy 5.0.7 / JVM 26; the
//! unit tests at the bottom carry the observed reference output.

use bigdecimal::num_bigint::{BigInt, Sign};
use bigdecimal::{BigDecimal, FromPrimitive, RoundingMode, Signed, ToPrimitive, Zero};
use std::str::FromStr;

/// Extra significant digits Groovy asks for when a quotient does not terminate
/// (`BigDecimalMath.DIVISION_EXTRA_PRECISION`): the `MathContext` precision is
/// `max(precision(a), precision(b)) + 10`.
const DIVISION_EXTRA_PRECISION: u64 = 10;

/// The floor Groovy puts under a non-terminating quotient's scale
/// (`BigDecimalMath.DIVISION_MIN_SCALE`): the result is rounded to at most
/// `max(scale(a), scale(b), 10)` fraction digits. This is why `1.0 / 3.0` is
/// `0.3333333333` (10 digits) and not the full `MathContext` precision.
const DIVISION_MIN_SCALE: i64 = 10;

/// The largest `**` exponent computed exactly. An exact decimal power multiplies
/// the scale by the exponent, so beyond this the result is measured in megabytes
/// of digits; [`pow`] declines instead, and the caller falls back to `double`.
const MAX_EXACT_EXPONENT: i64 = 10_000;

/// The plain-notation window of `BigDecimal.toString`: a value whose adjusted
/// exponent is below this (and any value with a negative scale) prints in
/// scientific `E+n` form.
const PLAIN_NOTATION_MIN_EXPONENT: i64 = -6;

/// Parse a Groovy decimal literal (`1.50`, `2.5e7`, `1E+3`) into a `BigDecimal`,
/// preserving the literal's exact scale. Returns `None` for text that is not a
/// decimal literal.
pub fn parse(text: &str) -> Option<BigDecimal> {
    BigDecimal::from_str(text).ok()
}

/// The most exponent digits `BigDecimal`'s parser will look at; beyond this it
/// gives up with `Too many nonzero exponent digits.` rather than overflowing.
/// Leading zeros are skipped before the count, so `1e0000000000000005` parses.
const MAX_EXPONENT_DIGITS: usize = 10;

/// Parse `text` the way `new java.math.BigDecimal(String)` does, reporting the
/// same `NumberFormatException` messages on failure.
///
/// `Err(Some(msg))` carries the JDK's message; `Err(None)` is the *message-less*
/// `NumberFormatException` the JDK throws for an empty string and for an
/// exponent mark with no digits after it (`""`, `"1e"`, `"1e+"`) — Groovy
/// surfaces that as a `null` message, so the distinction is observable.
///
/// The port covers the ASCII grammar; the JDK additionally accepts Unicode
/// decimal digits (`Character.digit`), which this rejects as ordinary
/// unexpected characters.
pub fn parse_java(text: &str) -> Result<BigDecimal, Option<String>> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Err(None);
    }
    // An optional leading sign, then digits with at most one decimal point,
    // ending either at the end of the text or at an exponent mark.
    let mut i = usize::from(matches!(chars[0], '+' | '-'));
    let mut digits = 0usize;
    let mut seen_dot = false;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() {
            digits += 1;
        } else if c == '.' {
            if seen_dot {
                return Err(Some(
                    "Character array contains more than one decimal point.".to_string(),
                ));
            }
            seen_dot = true;
        } else if c == 'e' || c == 'E' {
            parse_java_exponent(&chars[i + 1..])?;
            break;
        } else {
            return Err(Some(format!(
                "Character {c} is neither a decimal digit number, decimal point, \
                 nor \"e\" notation exponential mark."
            )));
        }
        i += 1;
    }
    if digits == 0 {
        return Err(Some("No digits found.".to_string()));
    }
    BigDecimal::from_str(text).map_err(|_| None)
}

/// Validate the exponent part (everything after the `e`/`E`) of a `BigDecimal`
/// literal, reproducing `BigDecimal.parseExp`'s diagnostics.
fn parse_java_exponent(rest: &[char]) -> Result<(), Option<String>> {
    let signed = rest.first().is_some_and(|c| matches!(c, '+' | '-'));
    let negative = rest.first() == Some(&'-');
    let mut digits = &rest[usize::from(signed)..];
    // No digits at all after the mark is the message-less failure.
    if digits.is_empty() {
        return Err(None);
    }
    while digits.len() > MAX_EXPONENT_DIGITS && digits[0] == '0' {
        digits = &digits[1..];
    }
    if digits.len() > MAX_EXPONENT_DIGITS {
        return Err(Some("Too many nonzero exponent digits.".to_string()));
    }
    let mut exp: i64 = 0;
    for c in digits {
        let Some(d) = c.to_digit(10) else {
            return Err(Some("Not a digit.".to_string()));
        };
        exp = exp * 10 + i64::from(d);
    }
    if negative {
        exp = -exp;
    }
    // The JDK stores the *scale*, which is the negated exponent, in an `int` —
    // so the range is asymmetric: `1e2147483648` parses and `1e-2147483648`
    // does not.
    if i32::try_from(-exp).is_err() {
        return Err(Some("Exponent overflow.".to_string()));
    }
    Ok(())
}

/// The *exact* decimal value of a double — every digit of its binary expansion,
/// as `new java.math.BigDecimal(double)` gives (`0.555d` becomes
/// `0.55500000000000004884981308350688777863979339599609375`). Groovy needs this
/// for `BigDecimal % Double`, the one mixed operation it keeps exact. `None` for
/// a non-finite double.
pub fn from_f64_exact(f: f64) -> Option<BigDecimal> {
    f.is_finite().then(|| BigDecimal::from_f64(f)).flatten()
}

/// The `BigDecimal` view of an `i64` (scale 0), for mixed `1 + 1.5` arithmetic.
pub fn from_i64(n: i64) -> BigDecimal {
    BigDecimal::from(n)
}

/// The `f64` view of a decimal, for the paths that must fall back to double
/// arithmetic (a `d`-suffixed operand, a fractional exponent) — Java's
/// `BigDecimal.doubleValue()`, which is *correctly rounded*.
///
/// `BigDecimal::to_f64` is not: it scales the unscaled value by a power of ten
/// in `f64`, so the error compounds with the exponent and the result overflows
/// early — `1.0e300 as double` came back `1.0000000000000006E300` and
/// `1.7976931348623157e308 as double` (an exactly representable `Double.MAX_VALUE`)
/// came back `Infinity`. Rust's `str::parse::<f64>` is correctly rounded and
/// saturates to infinity only where Java does, so the decimal's own scientific
/// rendering is round-tripped through it. Scientific notation keeps the string
/// bounded for a large scale either way.
pub fn to_f64(d: &BigDecimal) -> f64 {
    d.to_scientific_notation()
        .parse::<f64>()
        .ok()
        .or_else(|| d.to_f64())
        .unwrap_or(f64::NAN)
}

/// The exact `i64` value of a decimal, or `None` when it has a fraction part or
/// does not fit. Used for `**` (whose exponent must be an integer).
pub fn to_i64(d: &BigDecimal) -> Option<i64> {
    d.is_integer().then(|| d.to_i64()).flatten()
}

/// Render a decimal exactly as `java.math.BigDecimal.toString`.
///
/// Plain notation is used when the scale is non-negative *and* the adjusted
/// exponent (`digits - 1 - scale`) is at least -6; everything else — including
/// every negative scale, which is what an exponent literal like `2.5e7` produces
/// — uses scientific notation with an explicit exponent sign (`2.5E+7`, `1E-7`).
/// A single-digit coefficient keeps no fraction part (`1E+3`, not `1.0E+3`).
pub fn to_groovy_string(d: &BigDecimal) -> String {
    let (unscaled, scale) = d.as_bigint_and_exponent();
    let text = unscaled.to_string();
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", text.as_str()),
    };
    let n = digits.len() as i64;
    // The exponent the value would carry in scientific notation.
    let adjusted = n - 1 - scale;
    let body = if scale == 0 {
        digits.to_string()
    } else if scale > 0 && adjusted >= PLAIN_NOTATION_MIN_EXPONENT {
        if n > scale {
            let split = (n - scale) as usize;
            format!("{}.{}", &digits[..split], &digits[split..])
        } else {
            format!("0.{}{}", "0".repeat((scale - n) as usize), digits)
        }
    } else {
        let mut out = String::with_capacity(digits.len() + 6);
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('E');
        out.push(if adjusted >= 0 { '+' } else { '-' });
        out.push_str(&adjusted.abs().to_string());
        out
    };
    format!("{sign}{body}")
}

// Addition, subtraction, and multiplication run on the unscaled integers rather
// than through `bigdecimal`'s operators, which short-circuit a zero operand
// (`x * 0`, `x - 0`) to a scale-0 zero. Java keeps the scale in those cases —
// `1.25 * 0` is `0.00`, and a zero remainder is `0.000` — and the scale is the
// whole point of this module.

/// `a + b` — scale is `max(scale(a), scale(b))` (`1.10 + 2.20` is `3.30`).
pub fn add(a: &BigDecimal, b: &BigDecimal) -> BigDecimal {
    let (x, y, scale) = aligned(a, b);
    BigDecimal::from_bigint(x + y, scale)
}

/// `a - b` — scale is `max(scale(a), scale(b))`.
pub fn sub(a: &BigDecimal, b: &BigDecimal) -> BigDecimal {
    let (x, y, scale) = aligned(a, b);
    BigDecimal::from_bigint(x - y, scale)
}

/// `a * b` — scale is `scale(a) + scale(b)` (`1.25 * 0` is `0.00`).
pub fn mul(a: &BigDecimal, b: &BigDecimal) -> BigDecimal {
    let (ua, sa) = a.as_bigint_and_exponent();
    let (ub, sb) = b.as_bigint_and_exponent();
    BigDecimal::from_bigint(ua * ub, sa + sb)
}

/// Unary `-a`, keeping the scale.
pub fn neg(a: &BigDecimal) -> BigDecimal {
    let (u, scale) = a.as_bigint_and_exponent();
    BigDecimal::from_bigint(-u, scale)
}

/// Both operands' unscaled values raised to their common (larger) scale.
fn aligned(a: &BigDecimal, b: &BigDecimal) -> (BigInt, BigInt, i64) {
    let (ua, sa) = a.as_bigint_and_exponent();
    let (ub, sb) = b.as_bigint_and_exponent();
    let scale = sa.max(sb);
    (
        ua * pow10((scale - sa) as u64),
        ub * pow10((scale - sb) as u64),
        scale,
    )
}

/// `a % b` — Java's `BigDecimal.remainder`: `a - divideToIntegralValue(a, b) * b`,
/// so the result takes the sign of `a` and the scale of that subtraction
/// (`1.5 % 0.4` is `0.3`, `657.87e+3 % 1.50` is `0.0`). `None` when `b` is zero
/// (Groovy raises `ArithmeticException`).
pub fn remainder(a: &BigDecimal, b: &BigDecimal) -> Option<BigDecimal> {
    if b.is_zero() {
        return None;
    }
    Some(sub(a, &mul(&divide_to_integral(a, b), b)))
}

/// Java's `BigDecimal.divideToIntegralValue`: the quotient truncated toward
/// zero, carried at the preferred scale `scale(a) - scale(b)` — padded up to it,
/// or with trailing zeros stripped down to it, never past. The scale is not
/// cosmetic here: [`remainder`] subtracts `q * b` from `a`, so this scale is
/// what decides the remainder's own.
fn divide_to_integral(a: &BigDecimal, b: &BigDecimal) -> BigDecimal {
    let preferred = a.fractional_digit_count() - b.fractional_digit_count();
    // Java short-circuits a quotient of zero straight to the preferred scale.
    if a.abs() < b.abs() {
        return BigDecimal::from_bigint(BigInt::zero(), preferred);
    }
    at_preferred_scale(integral_quotient(a, b), 0, preferred)
}

/// `a ** e` for a non-negative integer exponent — exact, with scale
/// `scale(a) * e` (`10.0 ** 20` keeps 20 fraction digits). Returns `None` for a
/// negative exponent (which Groovy computes as a `double`) and for an exponent
/// so large that the exact result would not fit memory.
pub fn pow(a: &BigDecimal, e: i64) -> Option<BigDecimal> {
    (0..=MAX_EXACT_EXPONENT).contains(&e).then(|| {
        // Repeated [`mul`], not the crate's operator: `0.0 ** 3` is `0.000`, and
        // a zero operand there would collapse the scale.
        let mut acc = BigDecimal::from(1);
        for _ in 0..e {
            acc = mul(&acc, a);
        }
        acc
    })
}

/// `|a|`, keeping the scale (`(-1.50).abs()` is `1.50`).
pub fn abs(a: &BigDecimal) -> BigDecimal {
    a.abs()
}

/// The three `java.math.BigInteger` mask operators — `and`, `or`, `xor`, which
/// Groovy spells `&`, `|`, `^` — at arbitrary precision and in two's complement,
/// so `(-1G) & 255G` is `255` and `12345678901234567890G & 255G` is `210`.
///
/// The receivers are `BigInteger`s carried as scale-0 [`BigDecimal`]s; `None`
/// for anything with a fraction part, which is Groovy's
/// `UnsupportedOperationException` (a `BigDecimal` has no bits) and is the
/// caller's to raise.
pub fn bit_op(op: &str, a: &BigDecimal, b: &BigDecimal) -> Option<BigDecimal> {
    let (x, y) = (integer_part(a)?, integer_part(b)?);
    Some(BigDecimal::from(match op {
        "and" => x & y,
        "or" => x | y,
        "xor" => x ^ y,
        _ => return None,
    }))
}

/// `~a` — `BigInteger.not`, which in two's complement is `-a - 1`: `~7G` is
/// `-8`. `None` for a value with a fraction part, as [`bit_op`].
pub fn bit_not(a: &BigDecimal) -> Option<BigDecimal> {
    Some(BigDecimal::from(!integer_part(a)?))
}

/// `a << n` / `a >> n` on a `BigInteger`, which — unlike the `Integer` and
/// `Long` shifts — has no width to mask the count against and no bits to lose:
/// `1G << 100` is the full 31-digit power of two. A negative count shifts the
/// other way, as `BigInteger.shiftLeft` does. `None` for a fraction part.
pub fn shift(left: bool, a: &BigDecimal, n: i64) -> Option<BigDecimal> {
    let x = integer_part(a)?;
    let left = if n < 0 { !left } else { left };
    let n = n.unsigned_abs();
    // A shift wider than the value is zero (or -1 for a negative), and building
    // the intermediate for an absurd count would exhaust memory first.
    if left && n > MAX_SHIFT_BITS {
        return None;
    }
    Some(BigDecimal::from(if left { x << n } else { x >> n }))
}

/// The exact integer behind a scale-0 `BigDecimal`, or `None` when the value
/// carries a fraction — the case Groovy declines outright rather than truncating.
fn integer_part(d: &BigDecimal) -> Option<BigInt> {
    d.is_integer()
        .then(|| d.with_scale(0).into_bigint_and_exponent().0)
}

/// The largest left-shift distance computed exactly. Past this the result is
/// measured in gigabytes of digits; [`shift`] declines, exactly as [`pow`] does.
const MAX_SHIFT_BITS: u64 = 1 << 24;

/// `BigDecimal.toBigInteger()`: the value with its fraction part dropped, still
/// carried as a scale-0 [`BigDecimal`] so it keeps its unbounded magnitude
/// (unlike [`truncate_to_i64`], which saturates). This is what a
/// `java.math.BigInteger` holds.
pub fn to_big_integer(d: &BigDecimal) -> BigDecimal {
    d.with_scale_round(0, RoundingMode::Down)
}

/// `BigInteger.toString(int radix)`: the value in `radix`, sign-prefixed, with
/// lowercase digits. Java falls back to base 10 for a radix outside
/// `Character.MIN_RADIX`..`Character.MAX_RADIX` (2..36) rather than raising, so
/// `255G.toString(1)` and `255G.toString(37)` are both `255`.
pub fn to_radix_string(d: &BigDecimal, radix: i64) -> String {
    let radix = if (2..=36).contains(&radix) { radix } else { 10 };
    let (unscaled, scale) = to_big_integer(d).into_bigint_and_exponent();
    debug_assert_eq!(scale, 0, "to_big_integer leaves scale 0");
    unscaled.to_str_radix(radix as u32)
}

/// `intValue()` / `toInteger()`: truncate toward zero.
/// `BigDecimal.intValue()`: the fraction dropped, then the **low 32 bits** of
/// the resulting `BigInteger` — Java discards the high bits rather than
/// saturating, so `(1e30G).intValue()` is `1073741824`, not `Integer.MAX_VALUE`.
/// (`Double.intValue()` *does* saturate; the two conversions differ.)
pub fn low_i32(d: &BigDecimal) -> i32 {
    low_bits(d, 32) as i32
}

/// `BigDecimal.longValue()`: the same rule at 64 bits, so `(1e30G).longValue()`
/// is `5076944270305263616` rather than a saturated `Long.MAX_VALUE`.
pub fn low_i64(d: &BigDecimal) -> i64 {
    low_bits(d, 64)
}

/// The low `width` bits of `d`'s integral part, as a sign-correct two's
/// complement value. The magnitude is taken from the `BigInt`'s little-endian
/// 32-bit digits and negated afterwards, which is what discarding the high bits
/// of a negated two's complement value comes to.
fn low_bits(d: &BigDecimal, width: u32) -> i64 {
    let (unscaled, _) = to_big_integer(d).into_bigint_and_exponent();
    let (sign, digits) = unscaled.to_u32_digits();
    let lo = u64::from(digits.first().copied().unwrap_or(0));
    let hi = u64::from(digits.get(1).copied().unwrap_or(0));
    let magnitude = (lo | (hi << 32)) as i64;
    let value = if sign == Sign::Minus {
        magnitude.wrapping_neg()
    } else {
        magnitude
    };
    if width == 32 {
        i64::from(value as i32)
    } else {
        value
    }
}

pub fn truncate_to_i64(d: &BigDecimal) -> i64 {
    d.with_scale_round(0, RoundingMode::Down)
        .to_i64()
        .unwrap_or(0)
}

/// The GDK's `round()`: to the nearest integer, halves away from zero
/// (`1.75.round()` is `2`, `(-1.5).round()` is `-2`).
pub fn round_to_i64(d: &BigDecimal) -> i64 {
    d.with_scale_round(0, RoundingMode::HalfUp)
        .to_i64()
        .unwrap_or(0)
}

/// `d` rounded to `scale` fraction digits, halves away from zero — Java's
/// `RoundingMode.HALF_UP`, which is what `java.util.Formatter`'s `%f` applies to
/// a `BigDecimal` argument.
pub fn round_half_up(d: &BigDecimal, scale: i64) -> BigDecimal {
    d.with_scale_round(scale, RoundingMode::HalfUp)
}

/// `BigDecimal.scale()`: the number of digits to the right of the point. A
/// value written in `E+n` form has a *negative* scale (`2.5e7` is scale -6).
pub fn scale_of(d: &BigDecimal) -> i64 {
    d.as_bigint_and_exponent().1
}

/// `BigDecimal.precision()`: how many digits the unscaled value has. Zero has
/// precision 1, matching Java.
pub fn precision_of(d: &BigDecimal) -> i64 {
    let digits = d.as_bigint_and_exponent().0.abs().to_str_radix(10);
    digits.len() as i64
}

/// `BigDecimal.unscaledValue()`, as the `BigInteger` it is.
pub fn unscaled_value(d: &BigDecimal) -> BigDecimal {
    BigDecimal::from_bigint(d.as_bigint_and_exponent().0, 0)
}

/// `a / b` carried out to at least `scale` fraction digits, so a caller that
/// wants a *fixed* scale (`BigDecimal.divide(y, scale, mode)`) has enough
/// digits left to round at. `b` must be non-zero.
pub fn divide_to_scale(a: &BigDecimal, b: &BigDecimal, scale: i64) -> BigDecimal {
    if let Some(exact) = exact_divide(a, b) {
        return exact;
    }
    let precision = (a.digits() + scale.max(0) as u64 + DIVISION_EXTRA_PRECISION).max(1);
    divide_to_precision(a, b, precision)
}

/// Is `name` one of `java.math.RoundingMode`'s constants?
pub fn rounding_mode_exists(name: &str) -> bool {
    rounding_mode(name).is_some()
}

/// A `java.math.RoundingMode` constant by name. `UNNECESSARY` has no rounding
/// rule of its own — it is the assertion that none is needed — so it maps to
/// `None` and the caller checks [`fits_at_scale`] instead.
fn rounding_mode(name: &str) -> Option<Option<RoundingMode>> {
    Some(match name {
        "UP" => Some(RoundingMode::Up),
        "DOWN" => Some(RoundingMode::Down),
        "CEILING" => Some(RoundingMode::Ceiling),
        "FLOOR" => Some(RoundingMode::Floor),
        "HALF_UP" => Some(RoundingMode::HalfUp),
        "HALF_DOWN" => Some(RoundingMode::HalfDown),
        "HALF_EVEN" => Some(RoundingMode::HalfEven),
        "UNNECESSARY" => None,
        _ => return None,
    })
}

/// `BigDecimal.setScale(scale, mode)`. `None` for an unknown mode name, and for
/// `UNNECESSARY` when a digit would be lost — the caller tells the two apart
/// with [`rounding_mode_exists`].
pub fn with_scale(d: &BigDecimal, scale: i64, mode: &str) -> Option<BigDecimal> {
    match rounding_mode(mode)? {
        Some(m) => Some(d.with_scale_round(scale, m)),
        None => fits_at_scale(d, scale).then(|| d.with_scale_round(scale, RoundingMode::Down)),
    }
}

/// Can `d` be written at `scale` fraction digits with no digit lost? This is
/// what `BigDecimal.setScale(int)`'s implicit `RoundingMode.UNNECESSARY` asks
/// before it raises `ArithmeticException("Rounding necessary")`.
pub fn fits_at_scale(d: &BigDecimal, scale: i64) -> bool {
    scale >= scale_of(d) || d.with_scale_round(scale, RoundingMode::Down) == *d
}

/// `d` cut to `scale` fraction digits, dropping the rest — Java's
/// `RoundingMode.DOWN`, which is what Groovy's `BigDecimal.trunc` applies.
pub fn truncate_to_scale(d: &BigDecimal, scale: i64) -> BigDecimal {
    d.with_scale_round(scale, RoundingMode::Down)
}

/// Compare two decimals by value, ignoring scale (`1.5 <=> 1.50` is `0`), which
/// is what Groovy's `==`/`<`/`<=>` use on a `BigDecimal`.
pub fn cmp(a: &BigDecimal, b: &BigDecimal) -> std::cmp::Ordering {
    a.cmp(b)
}

/// Groovy's `/` on decimals, ported from
/// `org.codehaus.groovy.runtime.typehandling.BigDecimalMath.divideImpl`: take
/// Java's exact `BigDecimal.divide` when the quotient terminates, and otherwise
/// redo the division with `MathContext(max(precision) + 10)` before clamping the
/// scale to `max(scale(a), scale(b), 10)` with HALF_UP rounding.
///
/// `None` signals a zero divisor (Groovy raises `ArithmeticException`).
pub fn divide(a: &BigDecimal, b: &BigDecimal) -> Option<BigDecimal> {
    if b.is_zero() {
        return None;
    }
    if let Some(exact) = exact_divide(a, b) {
        return Some(exact);
    }
    let precision = a.digits().max(b.digits()) + DIVISION_EXTRA_PRECISION;
    let q = divide_to_precision(a, b, precision);
    let cap = a
        .fractional_digit_count()
        .max(b.fractional_digit_count())
        .max(DIVISION_MIN_SCALE);
    Some(if q.fractional_digit_count() > cap {
        q.with_scale_round(cap, RoundingMode::HalfUp)
    } else {
        q
    })
}

/// Java's exact `BigDecimal.divide(divisor)`: the quotient when it has a
/// terminating decimal expansion, at the *preferred* scale
/// (`scale(a) - scale(b)`) when that can hold it and at the smallest exact scale
/// otherwise — `1.000 / 4` is `0.250` (padded to the preferred scale 3) while
/// `2.5e7 / 1000` is `2.5E+4` (scale -3, because the preferred -6 cannot
/// represent 25000). `None` when the expansion does not terminate.
///
/// Public because it is also the whole of the *method* `BigDecimal.divide(y)`,
/// which — unlike Groovy's `/` operator, which is [`divide`] — raises
/// `ArithmeticException` rather than approximating when the expansion runs on.
pub fn exact_divide(a: &BigDecimal, b: &BigDecimal) -> Option<BigDecimal> {
    let (ua, sa) = a.as_bigint_and_exponent();
    let (ub, sb) = b.as_bigint_and_exponent();
    let preferred = sa - sb;
    // A zero dividend takes the preferred scale as-is, negative included:
    // `0 / -0.51251` prints `0E+5`, not `0`.
    if ua.is_zero() {
        return Some(BigDecimal::from_bigint(BigInt::zero(), preferred));
    }
    // Write |b|'s unscaled value as 2^twos · 5^fives · rest. The quotient
    // terminates exactly when `rest` (which shares no factor with 10) divides
    // a's unscaled value.
    let (rest, twos, fives) = strip_factors_of_ten(ub.abs());
    if !(&ua % &rest).is_zero() {
        return None;
    }
    let k = twos.max(fives);
    let numerator = (&ua / &rest) * pow10(k);
    let denominator = BigInt::from(2).pow(twos as u32) * BigInt::from(5).pow(fives as u32);
    let mut unscaled = numerator / denominator;
    if ub.sign() == Sign::Minus {
        unscaled = -unscaled;
    }
    Some(at_preferred_scale(unscaled, k as i64 + sa - sb, preferred))
}

/// Strip a positive integer's factors of 2 and 5, returning
/// `(rest, twos, fives)` with `rest` coprime to 10.
fn strip_factors_of_ten(mut n: BigInt) -> (BigInt, u64, u64) {
    let (mut twos, mut fives) = (0, 0);
    while (&n % 2u32).is_zero() {
        n /= 2u32;
        twos += 1;
    }
    while (&n % 5u32).is_zero() {
        n /= 5u32;
        fives += 1;
    }
    (n, twos, fives)
}

/// Rescale an exact quotient to Java's preferred scale: drop trailing zeros
/// down to `preferred` (never below it), or pad with zeros up to it.
fn at_preferred_scale(mut unscaled: BigInt, mut scale: i64, preferred: i64) -> BigDecimal {
    while scale > preferred && !unscaled.is_zero() && (&unscaled % 10u32).is_zero() {
        unscaled /= 10u32;
        scale -= 1;
    }
    if scale < preferred {
        unscaled *= pow10((preferred - scale) as u64);
        scale = preferred;
    }
    BigDecimal::from_bigint(unscaled, scale)
}

/// Divide to exactly `precision` significant digits, rounded HALF_UP — Java's
/// `a.divide(b, new MathContext(precision))` (whose default rounding is
/// HALF_UP). Done on the unscaled integers so the result is the correctly
/// rounded quotient, never a doubly-rounded one.
fn divide_to_precision(a: &BigDecimal, b: &BigDecimal, precision: u64) -> BigDecimal {
    let (ua, sa) = a.as_bigint_and_exponent();
    let (ub, sb) = b.as_bigint_and_exponent();
    let negative = (ua.sign() == Sign::Minus) != (ub.sign() == Sign::Minus);
    let (ua, ub) = (ua.abs(), ub.abs());
    // Scale the numerator so the integer quotient carries at least one digit
    // more than the requested precision — the extra digit decides the rounding.
    let shift = (precision as i64 + 2 + digit_count(&ub) as i64 - digit_count(&ua) as i64).max(0);
    let q = (ua * pow10(shift as u64)) / &ub;
    // Drop the surplus digits, rounding the truncated tail HALF_UP.
    let mut drop = digit_count(&q) as i64 - precision as i64;
    let mut rounded = &q / pow10(drop as u64);
    if (&q % pow10(drop as u64)) * 2u32 >= pow10(drop as u64) {
        rounded += 1;
    }
    // A carry out of the top digit (999… → 1000…) costs one more digit.
    if digit_count(&rounded) as u64 > precision {
        rounded /= 10u32;
        drop += 1;
    }
    BigDecimal::from_bigint(
        if negative { -rounded } else { rounded },
        shift - drop + sa - sb,
    )
}

/// `truncate(a / b)` toward zero as an integer, for [`divide_to_integral`].
fn integral_quotient(a: &BigDecimal, b: &BigDecimal) -> BigInt {
    let (ua, sa) = a.as_bigint_and_exponent();
    let (ub, sb) = b.as_bigint_and_exponent();
    // a / b == (ua / ub) · 10^(sb - sa): move the power of ten onto whichever
    // side keeps both operands integral.
    let shift = sb - sa;
    if shift >= 0 {
        (ua * pow10(shift as u64)) / ub
    } else {
        ua / (ub * pow10((-shift) as u64))
    }
}

/// `10^n` as a `BigInt`.
fn pow10(n: u64) -> BigInt {
    BigInt::from(10).pow(n as u32)
}

/// The number of decimal digits in an integer's magnitude (`0` has one digit).
fn digit_count(n: &BigInt) -> usize {
    let text = n.magnitude().to_string();
    text.len()
}

/// Render an IEEE double exactly as `java.lang.Double.toString` — the rules a
/// `d`/`f`-suffixed Groovy literal prints by, which are *not* the `BigDecimal`
/// rules: plain notation only in `[1e-3, 1e7)`, scientific notation elsewhere
/// with no `+` on a positive exponent (`2.5E7`, `1.0E-7`), and always at least
/// one fraction digit (`1000.0`).
pub fn format_double(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-Infinity" } else { "Infinity" }.to_string();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    // Rust's `{:e}` yields the shortest round-tripping digits plus a decimal
    // exponent — the same digit selection JDK 19+ `Double.toString` makes.
    let sci = format!("{f:e}");
    let (mantissa, exponent) = sci
        .split_once('e')
        .expect("`{:e}` always emits an exponent");
    let exponent: i64 = exponent.parse().unwrap_or(0);
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    // `Double.toString` wants at least *two* significant digits and, among the
    // decimals of that minimal length that round-trip, the one nearest the
    // value. Rust stops at the shortest round-tripping form, which can be a
    // single digit, and padding it with a `0` is not always the nearest
    // two-digit decimal: `4.9406564584124654E-324`'s shortest form is `5e-324`,
    // where Java prints the nearer `4.9E-324`. Only the extreme denormals — one
    // ulp spanning a large fraction of the value — can differ.
    let (digits, exponent) = match digits.len() {
        1 => nearest_two_digits(f).unwrap_or((digits, exponent)),
        _ => (digits, exponent),
    };
    let body = if (-3..7).contains(&exponent) {
        if exponent >= 0 {
            let split = exponent as usize + 1;
            let integral = if digits.len() >= split {
                digits[..split].to_string()
            } else {
                format!("{digits}{}", "0".repeat(split - digits.len()))
            };
            let fraction = digits.get(split..).filter(|s| !s.is_empty()).unwrap_or("0");
            format!("{integral}.{fraction}")
        } else {
            format!("0.{}{digits}", "0".repeat((-exponent - 1) as usize))
        }
    } else {
        let fraction = digits.get(1..).filter(|s| !s.is_empty()).unwrap_or("0");
        format!("{}.{fraction}E{exponent}", &digits[..1])
    };
    format!("{sign}{body}")
}

/// The two-significant-digit decimal nearest `f`, as `(digits, exponent)` in the
/// same shape `{:e}` produces — `Some(("49", -324))` for the smallest positive
/// denormal. `None` when the rounded decimal does not read back as `f`, in which
/// case the caller keeps the shortest form it already has.
///
/// `f`'s exact value is a finite decimal (a binary fraction always is), so
/// `BigDecimal::from_f64` loses nothing and `with_prec(2)` rounds it exactly.
fn nearest_two_digits(f: f64) -> Option<(String, i64)> {
    let rounded = BigDecimal::from_f64(f)?.with_prec(2);
    if to_f64(&rounded) != f {
        return None;
    }
    let (unscaled, scale) = rounded.as_bigint_and_exponent();
    let digits = unscaled.abs().to_string();
    // `{:e}`'s exponent is that of the leading digit: `digits.len() - 1 - scale`.
    // It does not move when trailing zeros are dropped below.
    let exponent = digits.len() as i64 - 1 - scale;
    let digits = digits.trim_end_matches('0');
    // Rounding landed back on the single digit the caller already has (`0.1`
    // rounds to `0.10`), so its padded form is the nearest two-digit decimal and
    // substituting would print the padding twice.
    (digits.len() > 1).then(|| (digits.to_string(), exponent))
}

/// `java.math.BigInteger.hashCode()`: fold the magnitude's 32-bit words
/// big-endian with `hash = 31 * hash + word` in wrapping `int` arithmetic, then
/// multiply by the signum. Zero has an empty magnitude and signum 0, so it
/// hashes to 0, and `-n` hashes to `-(n.hashCode())`.
pub fn big_integer_hash(n: &BigInt) -> i32 {
    let (sign, magnitude) = n.clone().into_parts();
    // `to_u32_digits` is little-endian; Java's `mag[]` is big-endian.
    let hash = magnitude
        .to_u32_digits()
        .iter()
        .rev()
        .fold(0i32, |h, w| h.wrapping_mul(31).wrapping_add(*w as i32));
    match sign {
        Sign::Minus => hash.wrapping_neg(),
        _ => hash,
    }
}

/// `java.math.BigDecimal.hashCode()`: `31 * unscaledValue().hashCode() + scale()`.
/// The scale is part of it, so `1.5` and `1.50` — equal in value, different in
/// scale — hash differently, exactly as they compare unequal under `equals`.
pub fn big_decimal_hash(d: &BigDecimal) -> i32 {
    let (unscaled, scale) = d.as_bigint_and_exponent();
    big_integer_hash(&unscaled)
        .wrapping_mul(31)
        .wrapping_add(scale as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a literal the way the lexer hands it over.
    fn d(text: &str) -> BigDecimal {
        parse(text).expect("decimal literal parses")
    }

    /// Render `text` as Groovy would print the literal.
    fn show(text: &str) -> String {
        to_groovy_string(&d(text))
    }

    // Every expectation below is the observed stdout of Apache Groovy 5.0.7 on
    // the same expression — the reason this module exists is that an f64 cannot
    // produce these strings.

    #[test]
    fn literal_rendering_matches_bigdecimal_tostring() {
        // Exponent literals keep a negative scale, so they print in E+n form.
        assert_eq!(show("2.5e7"), "2.5E+7");
        assert_eq!(show("1e3"), "1E+3");
        assert_eq!(show("-2.5e7"), "-2.5E+7");
        // Trailing zeros are part of the value, not noise.
        assert_eq!(show("100.00"), "100.00");
        assert_eq!(show("1.0"), "1.0");
        // The plain-notation window ends at an adjusted exponent of -6.
        assert_eq!(show("1.5E-5"), "0.000015");
        assert_eq!(show("1.0e-6"), "0.0000010");
        assert_eq!(show("1e-7"), "1E-7");
        assert_eq!(show("9.99e-4"), "0.000999");
        assert_eq!(show("123.456e2"), "12345.6");
        assert_eq!(show("0.0"), "0.0");
    }

    #[test]
    fn addition_and_subtraction_take_the_larger_scale() {
        // The headline case: adding an integer to an exponent literal drops to
        // scale 0, so Groovy prints no `.0` at all.
        assert_eq!(
            to_groovy_string(&add(&d("2.5e7"), &from_i64(1))),
            "25000001"
        );
        assert_eq!(
            to_groovy_string(&sub(&d("2.5e7"), &from_i64(1))),
            "24999999"
        );
        assert_eq!(to_groovy_string(&add(&d("1.10"), &d("2.20"))), "3.30");
        // Exact decimal addition, where an f64 would show 0.30000000000000004.
        assert_eq!(to_groovy_string(&add(&d("0.1"), &d("0.2"))), "0.3");
        assert_eq!(to_groovy_string(&sub(&d("0.30"), &d("0.10"))), "0.20");
        assert_eq!(to_groovy_string(&add(&d("12.34"), &d("0.1"))), "12.44");
    }

    #[test]
    fn multiplication_sums_the_scales() {
        assert_eq!(to_groovy_string(&mul(&d("1.1"), &d("1.1"))), "1.21");
        assert_eq!(to_groovy_string(&mul(&d("2.50"), &from_i64(4))), "10.00");
        assert_eq!(to_groovy_string(&mul(&d("1.25"), &from_i64(0))), "0.00");
        assert_eq!(to_groovy_string(&mul(&d("2.5e7"), &from_i64(2))), "5.0E+7");
        // Beyond an f64's range entirely (which would be Infinity).
        assert_eq!(
            to_groovy_string(&mul(&d("1.5e300"), &d("1.5e300"))),
            "2.25E+600"
        );
    }

    #[test]
    fn terminating_division_uses_javas_preferred_scale() {
        let div = |x: &str, y: &str| to_groovy_string(&divide(&d(x), &d(y)).expect("nonzero"));
        assert_eq!(div("7.0", "2.0"), "3.5");
        assert_eq!(div("1.0", "8"), "0.125");
        assert_eq!(div("10", "4"), "2.5");
        // Padded up to the preferred scale (3), not left at the minimal 2.
        assert_eq!(div("1.000", "4"), "0.250");
        assert_eq!(div("1.00", "4"), "0.25");
        // The preferred scale (-6) cannot hold 25000, so the minimal one wins.
        assert_eq!(div("2.5e7", "1000"), "2.5E+4");
        assert_eq!(div("0.3", "0.1"), "3");
        // A zero dividend keeps the preferred scale, negative or not.
        assert_eq!(div("0.00", "5"), "0.00");
        assert_eq!(div("0", "-0.51251"), "0E+5");
        assert_eq!(div("0.0", "0.674128e-1"), "0E+6");
    }

    #[test]
    fn nonterminating_division_rounds_to_groovys_scale_floor() {
        let div = |x: &str, y: &str| to_groovy_string(&divide(&d(x), &d(y)).expect("nonzero"));
        // Ten fraction digits, not an f64's seventeen.
        assert_eq!(div("1.0", "3.0"), "0.3333333333");
        assert_eq!(div("1", "3"), "0.3333333333");
        assert_eq!(div("1.00000", "3"), "0.3333333333");
        // HALF_UP at the tenth digit.
        assert_eq!(div("2", "3"), "0.6666666667");
        // Precision, not scale, bounds this one: 11 significant digits.
        assert_eq!(div("1e3", "3"), "333.33333333");
        assert_eq!(div("-1.0", "3.0"), "-0.3333333333");
    }

    #[test]
    fn division_by_zero_is_rejected() {
        assert!(divide(&d("1.0"), &from_i64(0)).is_none());
        assert!(remainder(&d("1.0"), &from_i64(0)).is_none());
    }

    #[test]
    fn remainder_and_power_keep_bigdecimal_scale() {
        assert_eq!(
            to_groovy_string(&remainder(&d("1.5"), &d("0.4")).unwrap()),
            "0.3"
        );
        assert_eq!(
            to_groovy_string(&remainder(&from_i64(7), &d("2.5")).unwrap()),
            "2.0"
        );
        // The remainder's scale comes from `divideToIntegralValue`, whose own
        // scale is the preferred `scale(a) - scale(b)` — these six all print a
        // different number of fraction digits than a naive max-scale rule gives.
        let rem = |x: &str, y: &str| to_groovy_string(&remainder(&d(x), &d(y)).unwrap());
        assert_eq!(rem("99.72559", "44.562e-5"), "0.0002902");
        assert_eq!(rem("657.87e+3", "1.50"), "0.0");
        assert_eq!(rem("-0.000", "0.041"), "0.000");
        assert_eq!(rem("71.59514", "8e-9"), "0E-7");
        assert_eq!(rem("11.535", "0.16859e-3"), "0.0000722");
        assert_eq!(rem("-91", "0.0100"), "0.00");
        assert_eq!(rem("1.5", "4.25"), "1.5");
        assert_eq!(rem("123.456", "1"), "0.456");
        assert_eq!(rem("1e3", "1e-3"), "0E+3");
        assert_eq!(rem("2.5e7", "3"), "1");
        assert_eq!(to_groovy_string(&pow(&d("2.5"), 3).unwrap()), "15.625");
        assert_eq!(to_groovy_string(&pow(&d("0.0"), 3).unwrap()), "0.000");
        assert_eq!(to_groovy_string(&pow(&d("2.50"), 2).unwrap()), "6.2500");
        assert_eq!(to_groovy_string(&pow(&d("1.5"), 0).unwrap()), "1");
        // 10.0 ** 20 keeps one fraction digit per multiplication.
        assert_eq!(
            to_groovy_string(&pow(&d("10.0"), 20).unwrap()),
            "100000000000000000000.00000000000000000000"
        );
        assert!(pow(&d("2.5"), -1).is_none());
        // `BigDecimal % Double` reads the double exactly, so the remainder
        // carries the double's full binary expansion.
        assert_eq!(
            to_groovy_string(&remainder(&d("1.5"), &from_f64_exact(0.555).unwrap()).unwrap()),
            "0.38999999999999990230037383298622444272041320800781250"
        );
    }

    #[test]
    fn comparison_ignores_scale() {
        assert_eq!(cmp(&d("1.5"), &d("1.50")), std::cmp::Ordering::Equal);
        assert_eq!(cmp(&d("1.5"), &d("1.4")), std::cmp::Ordering::Greater);
        assert_eq!(cmp(&d("2.5e7"), &d("25000000")), std::cmp::Ordering::Equal);
    }

    #[test]
    fn arbitrary_precision_survives_the_f64_boundary() {
        // 30 significant digits: an f64 would round the trailing .5 away.
        assert_eq!(
            to_groovy_string(&add(&d("123456789012345678901234567890.5"), &from_i64(1))),
            "123456789012345678901234567891.5"
        );
    }

    #[test]
    fn double_rendering_matches_java_double_tostring() {
        // A `d`-suffixed literal is an IEEE double and prints by Java's other
        // rule set: no `+` on the exponent, and a plain window of [1e-3, 1e7).
        assert_eq!(format_double(2.5e7), "2.5E7");
        assert_eq!(format_double(1e3), "1000.0");
        assert_eq!(format_double(1e-7), "1.0E-7");
        assert_eq!(format_double(1234567.0), "1234567.0");
        assert_eq!(format_double(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(format_double(1.0 / 3.0), "0.3333333333333333");
        assert_eq!(format_double(f64::INFINITY), "Infinity");
        assert_eq!(format_double(f64::NEG_INFINITY), "-Infinity");
        assert_eq!(format_double(f64::NAN), "NaN");
        assert_eq!(format_double(0.0), "0.0");
        assert_eq!(format_double(-2.5), "-2.5");
        assert_eq!(format_double(0.001), "0.001");
        assert_eq!(format_double(0.0001), "1.0E-4");
    }

    #[test]
    fn double_rendering_picks_javas_nearest_two_digit_denormal() {
        // Java wants at least two significant digits and, among the decimals of
        // that minimal length that round-trip, the nearest one. Rust's `{:e}`
        // stops at the shortest round-tripping form, which for these is a single
        // digit whose padded form is *not* the nearest. Each expectation is the
        // stdout of Apache Groovy 5.0.8 on JVM 21.0.12 — which matters, because
        // JVM 17 still renders doubles by the pre-JDK-19 algorithm.
        assert_eq!(format_double(f64::from_bits(1)), "4.9E-324");
        assert_eq!(format_double(f64::from_bits(2)), "9.9E-324");
        assert_eq!(format_double(f64::from_bits(10)), "4.9E-323");
        assert_eq!(format_double(f64::from_bits(12)), "5.9E-323");
        assert_eq!(format_double(-f64::from_bits(1)), "-4.9E-324");
        // The two-digit search must not fire where the padded single digit
        // already *is* the nearest — these all round to a trailing zero.
        assert_eq!(format_double(0.1), "0.1");
        assert_eq!(format_double(1.0e23), "1.0E23");
        assert_eq!(format_double(f64::from_bits(202)), "1.0E-321");
        assert_eq!(format_double(1.0e-310), "1.0E-310");
    }

    #[test]
    fn to_f64_is_correctly_rounded_like_bigdecimal_doublevalue() {
        // `BigDecimal::to_f64` scales by a power of ten in f64, so its error
        // compounds with the exponent: `1.0e300` came back
        // `1.0000000000000006E300` and `Double.MAX_VALUE` — exactly
        // representable — came back `Infinity`. Java's `doubleValue()` is
        // correctly rounded; these are the values `(<literal>) as double` prints
        // under Apache Groovy 5.0.8 / JVM 21.0.12.
        assert_eq!(format_double(to_f64(&d("1.0e308"))), "1.0E308");
        assert_eq!(format_double(to_f64(&d("1.0e300"))), "1.0E300");
        assert_eq!(
            to_f64(&d("1.7976931348623157e308")),
            f64::MAX,
            "Double.MAX_VALUE is exactly representable and must not overflow"
        );
        assert_eq!(format_double(to_f64(&d("0.1"))), "0.1");
        assert_eq!(
            format_double(to_f64(&d("123456789012345678901234567890"))),
            "1.2345678901234568E29"
        );
        // Past the top of the range Java does saturate.
        assert!(to_f64(&d("1e309")).is_infinite());
    }

    #[test]
    fn biginteger_hash_folds_the_magnitude_words_big_endian() {
        // Every expectation is the stdout of `println (<literal>).hashCode()`
        // under Apache Groovy 5.0.8 / JVM 17.0.4.1. The multi-word cases are the
        // ones that pin the word order: `2**70` is `[0x40, 0, 0]` big-endian, so
        // only a big-endian fold reaches 61504, and the 20-digit literal needs
        // both words *and* the `int` wrap.
        let bi = |t: &str| big_integer_hash(&d(t).as_bigint_and_exponent().0);
        assert_eq!(bi("0"), 0);
        assert_eq!(bi("1"), 1);
        assert_eq!(bi("-1"), -1);
        assert_eq!(bi("100"), 100);
        assert_eq!(bi("1180591620717411303424"), 61504); // 2**70
        assert_eq!(bi("12345678901234567890"), -1436577082);
        assert_eq!(bi("-12345678901234567890"), 1436577082);
    }

    #[test]
    fn bigdecimal_hash_carries_the_scale() {
        // Also Groovy 5.0.8's. `1.5` and `1.50` are the pair that shows the
        // scale is hashed, not just the numeric value.
        let bd = |t: &str| big_decimal_hash(&d(t));
        assert_eq!(bd("1.5"), 466);
        assert_eq!(bd("1.50"), 4652);
        assert_eq!(bd("1.500"), 46503);
        assert_eq!(bd("0.1"), 32);
        assert_eq!(bd("-3.14"), -9732);
        assert_eq!(bd("1.5e10"), 456);
    }
}

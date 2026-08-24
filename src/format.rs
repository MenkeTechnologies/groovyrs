//! `java.util.Formatter`'s specifier grammar and numeric layout.
//!
//! `printf` / `sprintf` / `String.format` / `CharSequence.formatted` all go
//! through the JDK's `Formatter`, whose behaviour is far more specific than
//! "printf with a `%`": the flag set a conversion accepts is fixed per
//! conversion and a wrong one *throws*, an integral conversion refuses a
//! `BigDecimal` argument, `%e`/`%g` lay a value out from its *unscaled digits*
//! rather than from an IEEE double, and a `double` is rendered from its
//! shortest round-trip representation (so `"%.20f"` of `0.1d` is
//! `0.10000000000000000000`, not the exact binary expansion).
//!
//! This module is the pure half — parsing a format string into pieces and
//! laying an unsigned `BigDecimal` out in the three float forms. The half that
//! needs the VM (rendering `%s`, hashing `%h`, classifying an argument's Java
//! type, raising) lives in `java_format`.
//!
//! Every rule and message below is byte-verified against Apache Groovy 5.1.0 /
//! JVM 26; `tests/format.rs` carries the observed reference output.

use bigdecimal::num_bigint::BigInt;
use bigdecimal::{BigDecimal, Signed, Zero};

/// One parsed `%…` specifier.
#[derive(Debug, Clone)]
pub struct Spec {
    /// The specifier exactly as written. The JDK quotes it in
    /// `MissingFormatArgumentException` and `MissingFormatWidthException`.
    pub text: String,
    /// `%3$s` → `Some(3)`. A one-based *explicit* argument index.
    pub index: Option<usize>,
    /// `%<s` — reuse the argument the previous specifier consumed.
    pub prev: bool,
    /// Flags in source order (`-`, `#`, `+`, ` `, `0`, `,`, `(`).
    pub flags: String,
    pub width: Option<usize>,
    pub precision: Option<usize>,
    pub conv: char,
}

impl Spec {
    pub fn has(&self, flag: char) -> bool {
        self.flags.contains(flag)
    }
}

/// A format string is a sequence of literal runs and conversions.
#[derive(Debug, Clone)]
pub enum Piece {
    Literal(String),
    Conv(Spec),
}

/// The JDK throwables a malformed or mismatched specifier raises. Each variant
/// carries exactly the `getMessage()` text the JDK produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// `UnknownFormatConversionException: Conversion = 'q'`
    UnknownConversion(char),
    /// `DuplicateFormatFlagsException: Flags = '-'`
    DuplicateFlag(char),
    /// `MissingFormatWidthException: %-s` — `-` or `0` with no width.
    MissingWidth(String),
    /// `FormatFlagsConversionMismatchException: Conversion = s, Flags = #`
    FlagMismatch(char, char),
    /// `IllegalFormatPrecisionException: 2` — a precision on `%d`/`%c`/`%x`/…
    IllegalPrecision(usize),
}

impl Error {
    /// The throwable's simple class name, as [`crate::throwable`] registers it.
    pub fn class(&self) -> &'static str {
        match self {
            Error::UnknownConversion(_) => "UnknownFormatConversionException",
            Error::DuplicateFlag(_) => "DuplicateFormatFlagsException",
            Error::MissingWidth(_) => "MissingFormatWidthException",
            Error::FlagMismatch(..) => "FormatFlagsConversionMismatchException",
            Error::IllegalPrecision(_) => "IllegalFormatPrecisionException",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Error::UnknownConversion(c) => format!("Conversion = '{c}'"),
            Error::DuplicateFlag(f) => format!("Flags = '{f}'"),
            Error::MissingWidth(t) => t.clone(),
            Error::FlagMismatch(conv, flag) => format!("Conversion = {conv}, Flags = {flag}"),
            Error::IllegalPrecision(p) => p.to_string(),
        }
    }
}

/// The flags each conversion accepts. `-` (left justify) is universal except on
/// `%n`; everything else is per-conversion, and a flag outside the set is a
/// `FormatFlagsConversionMismatchException` rather than a silent no-op.
///
/// `x`/`o` accept `+`, ` ` and `(` only for a `BigInteger` argument (the JDK
/// can render a sign for one because it is not a two's-complement word), which
/// is why those three are not listed here and are admitted by the caller.
fn allowed_flags(conv: char) -> &'static str {
    match conv {
        'd' => "-+ 0,(",
        'o' | 'x' | 'X' => "-#0",
        'e' | 'E' => "-#+ 0(",
        'f' => "-#+ 0,(",
        'g' | 'G' => "-+ 0,(",
        'a' | 'A' => "-#0",
        's' | 'S' => "-",
        'c' | 'C' | 'b' | 'B' | 'h' | 'H' => "-",
        '%' => "-",
        _ => "",
    }
}

/// Does `conv` take a precision? `%s`/`%b`/`%h` truncate to it, the float
/// conversions count digits with it, and everything else rejects it.
fn takes_precision(conv: char) -> bool {
    matches!(
        conv,
        's' | 'S' | 'b' | 'B' | 'h' | 'H' | 'e' | 'E' | 'f' | 'g' | 'G' | 'a' | 'A'
    )
}

/// Split a format string into literal runs and conversions, validating
/// everything that does not depend on the argument: the conversion character,
/// duplicate flags, a flag the conversion refuses, `-`/`0` without a width, and
/// a precision on a conversion that takes none.
pub fn parse(spec: &str) -> Result<Vec<Piece>, Error> {
    let mut out = Vec::new();
    let mut literal = String::new();
    let mut it = spec.chars().peekable();
    while let Some(c) = it.next() {
        if c != '%' {
            literal.push(c);
            continue;
        }
        if !literal.is_empty() {
            out.push(Piece::Literal(std::mem::take(&mut literal)));
        }
        let mut text = String::from("%");

        // An explicit argument index (`3$`) is digits followed by `$`; the same
        // digits with no `$` are a width, so this needs a lookahead over the
        // whole run. `<` is the "reuse the previous argument" flag.
        let mut digits = String::new();
        while matches!(it.peek(), Some(d) if d.is_ascii_digit()) {
            digits.push(it.next().unwrap());
        }
        let mut index = None;
        let mut width_digits = String::new();
        if matches!(it.peek(), Some('$')) && !digits.is_empty() {
            it.next();
            text.push_str(&digits);
            text.push('$');
            index = digits.parse::<usize>().ok();
        } else {
            // Not an index. A leading `0` is the zero-pad flag, the rest is the
            // width — `%05d` is flag `0` plus width `5`.
            width_digits = digits;
        }
        let mut prev = false;
        if width_digits.is_empty() && matches!(it.peek(), Some('<')) {
            it.next();
            text.push('<');
            prev = true;
        }

        let mut flags = String::new();
        let push_flag = |f: char, flags: &mut String| -> Result<(), Error> {
            if flags.contains(f) {
                return Err(Error::DuplicateFlag(f));
            }
            flags.push(f);
            Ok(())
        };
        // Zero-pad digits already consumed above as part of the width run.
        while !width_digits.is_empty() && width_digits.starts_with('0') {
            push_flag('0', &mut flags)?;
            text.push('0');
            width_digits.remove(0);
        }
        while matches!(it.peek(), Some('-' | '+' | ' ' | ',' | '#' | '(' | '0')) {
            let f = it.next().unwrap();
            push_flag(f, &mut flags)?;
            text.push(f);
        }
        while matches!(it.peek(), Some(d) if d.is_ascii_digit()) {
            width_digits.push(it.next().unwrap());
        }
        text.push_str(&width_digits);
        let width = if width_digits.is_empty() {
            None
        } else {
            width_digits.parse::<usize>().ok()
        };

        let mut precision = None;
        if matches!(it.peek(), Some('.')) {
            it.next();
            text.push('.');
            let mut p = String::new();
            while matches!(it.peek(), Some(d) if d.is_ascii_digit()) {
                p.push(it.next().unwrap());
            }
            text.push_str(&p);
            precision = Some(p.parse::<usize>().unwrap_or(0));
        }

        let Some(conv) = it.next() else {
            return Err(Error::UnknownConversion(' '));
        };
        text.push(conv);
        if !matches!(
            conv,
            'b' | 'B'
                | 'h'
                | 'H'
                | 's'
                | 'S'
                | 'c'
                | 'C'
                | 'd'
                | 'o'
                | 'x'
                | 'X'
                | 'e'
                | 'E'
                | 'f'
                | 'g'
                | 'G'
                | 'a'
                | 'A'
                | 'n'
                | '%'
        ) {
            return Err(Error::UnknownConversion(conv));
        }
        let spec = Spec {
            text,
            index,
            prev,
            flags,
            width,
            precision,
            conv,
        };
        validate(&spec)?;
        out.push(Piece::Conv(spec));
    }
    if !literal.is_empty() {
        out.push(Piece::Literal(literal));
    }
    Ok(out)
}

/// The argument-independent half of the JDK's `checkGeneral`/`checkNumeric`.
/// `x`/`o` deliberately let `+`, ` ` and `(` through: whether they are legal
/// depends on the argument being a `BigInteger`, which only the caller knows.
fn validate(spec: &Spec) -> Result<(), Error> {
    let allowed = allowed_flags(spec.conv);
    let bigint_only = matches!(spec.conv, 'o' | 'x' | 'X');
    for f in spec.flags.chars() {
        if allowed.contains(f) || (bigint_only && "+ (".contains(f)) {
            continue;
        }
        return Err(Error::FlagMismatch(spec.conv, f));
    }
    if (spec.has('-') || spec.has('0')) && spec.width.is_none() {
        return Err(Error::MissingWidth(spec.text.clone()));
    }
    if let Some(p) = spec.precision {
        if !takes_precision(spec.conv) {
            return Err(Error::IllegalPrecision(p));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Numeric layout. Every function here takes a NON-NEGATIVE value: the JDK
// prints `value.abs()` and emits the sign itself, and `%g`'s decimal-vs-
// scientific choice is made on the magnitude for the same reason.
// ---------------------------------------------------------------------------

/// A non-negative `BigDecimal` as (unsigned digit string of the unscaled value,
/// scale). The digit string never carries a leading zero except for the value
/// zero, which is the invariant `BigInteger.toString` gives and which the
/// exponent arithmetic below relies on.
fn parts(d: &BigDecimal) -> (String, i64) {
    let (unscaled, scale) = d.as_bigint_and_exponent();
    (unscaled.abs().to_str_radix(10), scale)
}

/// The JDK's `- value.scale() + (value.unscaledValue().toString().length() - 1)`:
/// the power of ten the leading digit sits on. Zero keeps its scale's exponent,
/// which is why `%e` of the `BigDecimal` `0.0` is `0.000000e-01`.
fn adjusted_exponent(d: &BigDecimal) -> i64 {
    let (digits, scale) = parts(d);
    -scale + (digits.len() as i64 - 1)
}

/// Round a digit string to `n` significant digits, HALF_UP. Answers the rounded
/// digits (always exactly `n` long) and whether the carry grew the number by a
/// power of ten (`999` → `100`, exponent + 1).
fn round_digits(digits: &str, n: usize) -> (String, bool) {
    if digits.len() <= n {
        let mut d = digits.to_string();
        d.push_str(&"0".repeat(n - digits.len()));
        return (d, false);
    }
    let mut kept: Vec<u8> = digits.as_bytes()[..n].to_vec();
    if digits.as_bytes()[n] >= b'5' {
        let mut i = n;
        loop {
            if i == 0 {
                // Every digit was a 9: the result is 1 followed by zeros, one
                // decimal place further left than the input.
                let mut d = String::from("1");
                d.push_str(&"0".repeat(n.saturating_sub(1)));
                return (d, true);
            }
            i -= 1;
            if kept[i] == b'9' {
                kept[i] = b'0';
            } else {
                kept[i] += 1;
                break;
            }
        }
    }
    (String::from_utf8(kept).unwrap_or_default(), false)
}

/// Render a non-negative (digits, scale) pair in plain notation — never the
/// `E+n` form `BigDecimal.toString` switches to, because a `Formatter`
/// conversion has already decided which form it wants.
fn plain(digits: &str, scale: i64) -> String {
    if scale <= 0 {
        let mut s = digits.to_string();
        s.push_str(&"0".repeat((-scale) as usize));
        return s;
    }
    let scale = scale as usize;
    if digits.len() > scale {
        let cut = digits.len() - scale;
        format!("{}.{}", &digits[..cut], &digits[cut..])
    } else {
        format!("0.{}{}", "0".repeat(scale - digits.len()), digits)
    }
}

/// `%f`: `prec` digits after the point, HALF_UP, no exponent. The value is
/// non-negative.
pub fn fixed(d: &BigDecimal, prec: usize) -> String {
    let rounded = crate::decimal::round_half_up(d, prec as i64);
    let (digits, scale) = parts(&rounded);
    let body = plain(&digits, scale);
    // `round_half_up` keeps the value's own scale when it is already shorter
    // than `prec` (`1.5` at prec 3 stays scale 1), so pad the fraction out.
    let have = body.find('.').map_or(0, |i| body.len() - i - 1);
    if prec == 0 {
        return body.split('.').next().unwrap_or("0").to_string();
    }
    if have < prec {
        let mut s = body;
        if !s.contains('.') {
            s.push('.');
        }
        s.push_str(&"0".repeat(prec - have));
        s
    } else {
        body
    }
}

/// `%e`: `d.ddd…e±NN` with `prec` digits after the point, HALF_UP. The value is
/// non-negative; `exp_override` is the adjusted exponent a `double` argument
/// carries (a zero `double` is `e+00` where the `BigDecimal` `0.0` is `e-01`).
pub fn scientific(d: &BigDecimal, prec: usize, exp_override: Option<i64>) -> String {
    let (digits, _) = parts(d);
    let adj = exp_override.unwrap_or_else(|| adjusted_exponent(d));
    let (rounded, carried) = round_digits(&digits, prec + 1);
    let exp = if carried { adj + 1 } else { adj };
    let mut out = String::from(&rounded[..1]);
    if prec > 0 {
        out.push('.');
        out.push_str(&rounded[1..]);
    }
    out.push('e');
    out.push(if exp < 0 { '-' } else { '+' });
    out.push_str(&format!("{:02}", exp.abs()));
    out
}

/// `%g`'s decimal-or-scientific choice and the digit count that follows from
/// it, for a `BigDecimal` argument. The JDK compares the value itself against
/// `1e-4` and `10^prec` — not its rounded exponent — and treats a value that
/// `equals(BigDecimal.ZERO)` (scale 0 as well as unscaled 0) as in range, which
/// is why `0` is `0.00000` and `0.0` is `0.00000e-01`.
pub fn general_decimal_digits(d: &BigDecimal, prec: usize) -> Option<usize> {
    let ten_to_neg_four = BigDecimal::new(BigInt::from(1), 4);
    let ten_to_prec = BigDecimal::new(BigInt::from(1), -(prec as i64));
    let is_java_zero = d.is_zero() && d.as_bigint_and_exponent().1 == 0;
    if is_java_zero || (d >= &ten_to_neg_four && d < &ten_to_prec) {
        let e = adjusted_exponent(d);
        Some((prec as i64 - e - 1).max(0) as usize)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(s: &str) -> BigDecimal {
        BigDecimal::from_str(s).unwrap()
    }

    /// The three flag rejections a `%s` gets wrong most often, and the two
    /// width/precision rules — each verified against Groovy 5.1.0 / JVM 26.
    #[test]
    fn specifier_validation_matches_the_jdk() {
        assert_eq!(parse("%#s").unwrap_err(), Error::FlagMismatch('s', '#'));
        assert_eq!(parse("%,x").unwrap_err(), Error::FlagMismatch('x', ','));
        assert_eq!(parse("%#d").unwrap_err(), Error::FlagMismatch('d', '#'));
        assert_eq!(
            parse("%-s").unwrap_err(),
            Error::MissingWidth("%-s".to_string())
        );
        assert_eq!(
            parse("%0d").unwrap_err(),
            Error::MissingWidth("%0d".to_string())
        );
        assert_eq!(parse("%.2d").unwrap_err(), Error::IllegalPrecision(2));
        assert_eq!(parse("%--s").unwrap_err(), Error::DuplicateFlag('-'));
        assert_eq!(parse("%q").unwrap_err(), Error::UnknownConversion('q'));
        // `+`/` `/`(` on `x` are argument-dependent (legal for a `BigInteger`),
        // so the parser must let them through for the caller to judge.
        assert!(parse("%+x").is_ok());
    }

    /// `%05d` is the zero-pad FLAG plus width 5, but `%5d` is width 5 with no
    /// flag, and `%1$05d` is an index, then the flag, then the width. Getting
    /// this run wrong silently turns a width into an index.
    #[test]
    fn index_flag_and_width_run_apart() {
        let one = |s: &str| match parse(s).unwrap().remove(0) {
            Piece::Conv(c) => c,
            _ => panic!("not a conversion"),
        };
        let z = one("%05d");
        assert_eq!((z.flags.as_str(), z.width, z.index), ("0", Some(5), None));
        let w = one("%5d");
        assert_eq!((w.flags.as_str(), w.width, w.index), ("", Some(5), None));
        let i = one("%1$05d");
        assert_eq!(
            (i.flags.as_str(), i.width, i.index),
            ("0", Some(5), Some(1))
        );
        assert_eq!(i.text, "%1$05d");
        let p = one("%<s");
        assert!(p.prev && p.index.is_none());
    }

    #[test]
    fn scientific_layout_matches_the_jdk() {
        assert_eq!(scientific(&dec("1234.5"), 6, None), "1.234500e+03");
        assert_eq!(scientific(&dec("0.000012"), 6, None), "1.200000e-05");
        // Rounding that carries past the leading digit moves the exponent.
        assert_eq!(scientific(&dec("9.9999999"), 6, None), "1.000000e+01");
        assert_eq!(scientific(&dec("9.999"), 2, None), "1.00e+01");
        assert_eq!(scientific(&dec("1234.5"), 0, None), "1e+03");
        // A `BigDecimal` zero keeps its scale's exponent; a `double` zero does
        // not, which is the whole reason for `exp_override`.
        assert_eq!(scientific(&dec("0.0"), 6, None), "0.000000e-01");
        assert_eq!(scientific(&dec("0.0"), 6, Some(0)), "0.000000e+00");
        assert_eq!(scientific(&dec("1E+300"), 6, None), "1.000000e+300");
    }

    #[test]
    fn fixed_layout_rounds_half_up_and_pads() {
        assert_eq!(fixed(&dec("1.005"), 2), "1.01");
        assert_eq!(fixed(&dec("1.5"), 3), "1.500");
        assert_eq!(fixed(&dec("2.5"), 0), "3");
        assert_eq!(fixed(&dec("0.1"), 20), "0.10000000000000000000");
        assert_eq!(fixed(&dec("1E+3"), 2), "1000.00");
    }

    /// `%g`'s branch, including the two zeros that do not agree: `0` (scale 0)
    /// is `BigDecimal.ZERO` and takes the decimal branch, `0.0` does not.
    #[test]
    fn general_branch_matches_the_jdk() {
        assert_eq!(general_decimal_digits(&dec("1234.5"), 6), Some(2));
        assert_eq!(general_decimal_digits(&dec("0.0001"), 6), Some(9));
        assert_eq!(general_decimal_digits(&dec("999999.0"), 6), Some(0));
        assert_eq!(general_decimal_digits(&dec("1000000.0"), 6), None);
        assert_eq!(general_decimal_digits(&dec("0.00001"), 6), None);
        assert_eq!(general_decimal_digits(&dec("0"), 6), Some(5));
        assert_eq!(general_decimal_digits(&dec("0.0"), 6), None);
    }
}

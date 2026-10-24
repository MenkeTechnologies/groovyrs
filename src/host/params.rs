//! Declared parameter types at a call: which declaration an argument list fits
//! (Groovy's distance-based method selection, for methods, constructors and
//! script functions alike), and the conversion a numeric parameter applies to
//! the value it receives.
//!
//! Measured against Apache Groovy 6.0.0 / JVM 21.0.12 by passing every value
//! kind to a constructor of every parameter type: a `double` parameter takes
//! any number and makes it a `Double` (`3` arrives as `3.0`), a `float` one
//! takes every number but a `Double`, a `long` one only an integer, an `int`
//! one an `Integer` or a `BigInteger`, a `BigDecimal` one any number, a
//! `String` one a `String` or a `GString`, and `null` any reference type but
//! no primitive.

use super::*;

/// The outcome of weighing a call's arguments against the declarations
/// sharing its name and arity.
pub(super) enum Fit {
    /// The closest declaration: its sub index.
    Best(u16),
    /// There are typed declarations, and the arguments fit none of them.
    Mismatch,
    /// Nothing declared this name and arity with types to weigh.
    NoCandidates,
}

/// The closest of `candidates` for `args`: an exact class over a widening or
/// a supertype over `Object`/`def`, the first listed on a tie (callers list
/// the most-derived and the first-declared first).
pub(super) fn best_fit<'a>(
    candidates: impl IntoIterator<Item = &'a (Vec<String>, u16)>,
    args: &[Value],
) -> Fit {
    let mut best: Option<(u32, u16)> = None;
    let mut any = false;
    for (types, idx) in candidates {
        any = true;
        let distance = types
            .iter()
            .zip(args)
            .try_fold(0, |acc, (ty, v)| Some(acc + param_fit(ty, v)?));
        if let Some(d) = distance {
            if best.map_or(true, |(b, _)| d < b) {
                best = Some((d, *idx));
            }
        }
    }
    match (best, any) {
        (Some((_, idx)), _) => Fit::Best(idx),
        (None, true) => Fit::Mismatch,
        (None, false) => Fit::NoCandidates,
    }
}

/// A declared type as the fit tables name it: no generic arguments, no
/// `java.lang.` / `java.util.` / `java.math.` package.
fn bare_type(ty: &str) -> &str {
    let ty = ty.split('<').next().unwrap_or(ty).trim();
    ["java.lang.", "java.util.", "java.math."]
        .iter()
        .find_map(|p| ty.strip_prefix(p))
        .unwrap_or(ty)
}

fn is_primitive(ty: &str) -> bool {
    matches!(
        ty,
        "int" | "long" | "short" | "byte" | "double" | "float" | "boolean" | "char"
    )
}

/// How far `v` is from a parameter declared `ty`, `None` when it does not fit:
/// `0` for the argument's own type, `1` for a widening or a supertype, `2` for
/// `Object`/`def`. A type the tables do not model fits anything at `3`, so an
/// unmodeled JDK parameter type never turns a working call into a miss.
pub(super) fn param_fit(ty: &str, v: &Value) -> Option<u32> {
    let ty = bare_type(ty);
    if matches!(ty, "def" | "Object") {
        return Some(if matches!(v, Value::Undef) { 0 } else { 2 });
    }
    if matches!(v, Value::Undef) {
        return (!is_primitive(ty)).then_some(1);
    }
    // An array parameter takes an array of a fitting element type, never a
    // bare value — which is what lets a variadic one be told apart.
    if ty.ends_with("[]") {
        return (array_elem(v).is_some() && value_is_a(v, ty)).then_some(0);
    }
    // The integer kind: an `Integer`, a `Long` (by magnitude or a box), or a
    // `Short`/`Byte` box, which widens to the larger integer parameters.
    let kind = boxed::int_view(v).map(|(k, _)| k);
    let int32 = kind == Some(boxed::IntKind::Integer);
    let is_int = kind.is_some();
    let short = kind == Some(boxed::IntKind::Short);
    let byte = kind == Some(boxed::IntKind::Byte);
    let is_double = matches!(v, Value::Float(_));
    let is_float = as_float_handle(v).is_some();
    let is_big = as_bigint(v).is_some();
    let is_dec = is_dec_handle(v) && !is_big;
    let is_number = is_int || is_double || is_float || is_big || is_dec;
    let fit = |exact: bool, widens: bool| match (exact, widens) {
        (true, _) => Some(0),
        (false, true) => Some(1),
        _ => None,
    };
    match ty {
        "int" | "Integer" => fit(int32, is_big || short || byte),
        "short" | "Short" => fit(int32 || short, is_big || byte),
        "byte" | "Byte" => fit(int32 || byte, is_big),
        "long" | "Long" => fit(kind == Some(boxed::IntKind::Long), is_int),
        "double" | "Double" => fit(is_double, is_number),
        "float" | "Float" => fit(is_float, is_number && !is_double),
        "BigDecimal" => fit(is_dec, is_number),
        "BigInteger" => fit(is_big, is_int),
        "Number" => fit(false, is_number),
        "boolean" | "Boolean" => fit(matches!(v, Value::Bool(_)), false),
        // groovyrs has no `Character`: a one-character `String` stands in for
        // one, and a `String` parameter still wins it over a `char` one.
        "char" | "Character" => fit(false, matches!(v, Value::Str(s) if s.chars().count() == 1)),
        "String" => fit(matches!(v, Value::Str(_)) || as_gstring(v).is_some(), false),
        "CharSequence" => fit(false, matches!(v, Value::Str(_)) || as_gstring(v).is_some()),
        "Closure" => fit(closure_meta(v).is_some(), false),
        _ => {
            if let Some(cid) = resolve_class_name(ty) {
                // A script type: the value is one, or a closure the parameter
                // coerces — an interface or abstract class with one abstract
                // method. Two or more and Groovy refuses the closure.
                if value_is_a(v, ty) {
                    let own = as_instance(v).is_some_and(|i| i.class == cid);
                    return Some(if own { 0 } else { 1 });
                }
                return (is_sam_type(ty) && closure_meta(v).is_some()).then_some(2);
            }
            if matches!(
                ty,
                "List" | "ArrayList" | "Collection" | "Map" | "LinkedHashMap" | "HashMap" | "Set"
            ) {
                return value_is_a(v, ty).then_some(1);
            }
            Some(3)
        }
    }
}

/// The value a parameter declared `ty` receives for `v`: a number widened to
/// a `double`/`float`/`BigDecimal`/`BigInteger` parameter takes that type, so
/// `def d(double x)` called with `3` sees `3.0`. Anything else is unchanged.
pub(super) fn convert_param(vm: &mut VM, ty: &str, v: Value) -> Value {
    let target = match bare_type(ty) {
        "double" | "Double" => "Double",
        "float" | "Float" => "Float",
        "BigDecimal" => "BigDecimal",
        "BigInteger" => "BigInteger",
        _ => return v,
    };
    if param_fit(target, &v) != Some(1) {
        return v;
    }
    as_type(vm, boxed::unbox(v), target)
}

/// What a call's arguments select among every declaration of its name, of
/// every arity.
pub(super) enum Selection {
    /// The declaration to run, and the arguments it receives — repacked
    /// when the call reached a variadic parameter or Groovy's one-`null` rule.
    Found(u16, Vec<Value>),
    /// The name is declared, and the call fits no declaration of it.
    NoFit,
    /// A call with no arguments that two or more single-parameter
    /// declarations could take with a `null`: Groovy's
    /// `MethodSelectionException`, listing them (their subs, in order).
    Ambiguous(Vec<u16>),
    /// Nothing declared the name with types to weigh.
    NoCandidates,
}

/// Groovy's choice among `decls` (each `(parameter types, sub)`, most-derived
/// and first-declared first) for `args`, in its order of preference:
///
/// 1. a declaration of exactly the call's arity that the arguments fit
///    ([`best_fit`]);
/// 2. a variadic one (`T... rest` / `T[] rest` last): its fixed parameters
///    fit, and every trailing argument fits `T`, which are packed into the
///    `T[]` it receives — unless the call already passes an array (or
///    `null`) in that position, which is the exact-arity case above;
/// 3. a call with no arguments, against the one declaration taking a single
///    reference-typed parameter, which receives `null`.
pub(super) fn select_call(vm: &mut VM, decls: &[(Vec<String>, u16)], args: &[Value]) -> Selection {
    if decls.is_empty() {
        return Selection::NoCandidates;
    }
    let exact: Vec<(Vec<String>, u16)> = decls
        .iter()
        .filter(|(types, _)| types.len() == args.len())
        .cloned()
        .collect();
    if let Fit::Best(idx) = best_fit(&exact, args) {
        return Selection::Found(idx, args.to_vec());
    }
    let mut best: Option<(u32, u16, usize, &str)> = None;
    for (types, idx) in decls {
        let Some((last, fixed)) = types.split_last() else {
            continue;
        };
        let Some(component) = last.strip_suffix("[]") else {
            continue;
        };
        if args.len() < fixed.len() {
            continue;
        }
        let (head, tail) = args.split_at(fixed.len());
        let fixed_fit = fixed
            .iter()
            .zip(head)
            .try_fold(0, |acc, (ty, v)| Some(acc + param_fit(ty, v)?));
        let tail_fit = tail
            .iter()
            .try_fold(0, |acc, v| Some(acc + param_fit(component, v)?));
        let distance = fixed_fit.zip(tail_fit).map(|(a, b)| a + b);
        if let Some(d) = distance {
            if best.map_or(true, |(b, ..)| d < b) {
                best = Some((d, *idx, fixed.len(), last.as_str()));
            }
        }
    }
    if let Some((_, idx, fixed, last)) = best {
        let mut packed = args[..fixed].to_vec();
        let rest = glist(args[fixed..].to_vec());
        packed.push(as_type(vm, rest, last));
        return Selection::Found(idx, packed);
    }
    if args.is_empty() {
        // One entry per signature: an override hides what it overrides.
        let mut single: Vec<&(Vec<String>, u16)> = Vec::new();
        for decl in decls {
            let (types, _) = decl;
            if types.len() == 1
                && !types[0].ends_with("[]")
                && !single.iter().any(|(seen, _)| seen == types)
            {
                single.push(decl);
            }
        }
        if let [(types, idx)] = single.as_slice() {
            if !is_primitive(bare_type(&types[0])) {
                return Selection::Found(*idx, vec![Value::Undef]);
            }
        }
        if single.len() > 1 {
            return Selection::Ambiguous(single.iter().map(|(_, idx)| *idx).collect());
        }
    }
    Selection::NoFit
}

/// `MethodRankHelper.delDistance`, ported: a Damerau–Levenshtein distance in
/// tenths, where inserting, deleting or substituting a character costs 10, a
/// substitution that only changes case 5, and an adjacent transposition 5
/// (plus 5 for each case change it makes). Over UTF-16 units, as Java's
/// `CharSequence` is.
pub(super) fn del_distance(s: &str, t: &str) -> i32 {
    let s: Vec<u16> = s.encode_utf16().collect();
    let t: Vec<u16> = t.encode_utf16().collect();
    let (n, m) = (s.len(), t.len());
    if n == 0 {
        return m as i32;
    }
    if m == 0 {
        return n as i32;
    }
    let lower = |c: u16| char::from_u32(u32::from(c)).is_some_and(char::is_lowercase);
    let caseless = |a: u16, b: u16| {
        let fold = |c: u16| {
            char::from_u32(u32::from(c))
                .map(|ch| ch.to_lowercase().next().unwrap_or(ch))
                .unwrap_or('\0')
        };
        fold(a) == fold(b)
    };
    let mut vals = vec![vec![0i32; n + 1]; 3];
    for (i, v) in vals[1].iter_mut().enumerate() {
        *v = i as i32 * 10;
    }
    for j in 1..=m {
        let tj = t[j - 1];
        vals[0][0] = j as i32 * 10;
        for i in 1..=n {
            let si = s[i - 1];
            let cost = if lower(si) != lower(tj) {
                if caseless(si, tj) {
                    5
                } else {
                    10
                }
            } else if si == tj {
                0
            } else {
                10
            };
            vals[0][i] = (vals[0][i - 1] + 10)
                .min(vals[1][i] + 10)
                .min(vals[1][i - 1] + cost);
            if i > 1 && j > 1 {
                let mut swap = if lower(si) != lower(t[j - 2]) { 5 } else { 0 };
                if lower(s[i - 2]) != lower(tj) {
                    swap += 5;
                }
                if caseless(si, t[j - 2]) && caseless(s[i - 2], tj) {
                    vals[0][i] = vals[0][i].min(vals[2][i - 2] + 5 + swap);
                }
            }
        }
        vals.rotate_right(1);
    }
    vals[1][n]
}

/// `MethodRankHelper.getPropertySuggestionString`: the candidates within
/// distance 1–30 of `name`, closest first (the given order on a tie), at most
/// five, as `\nPossible solutions: a, b` — or nothing.
pub(super) fn property_suggestions(name: &str, candidates: &[String]) -> String {
    let mut ranked: Vec<(i32, &String)> = candidates
        .iter()
        .map(|c| (del_distance(name, c), c))
        .filter(|(d, _)| (1..=30).contains(d))
        .collect();
    ranked.sort_by_key(|(d, _)| *d);
    let names: Vec<&str> = ranked.iter().take(5).map(|(_, c)| c.as_str()).collect();
    if names.is_empty() {
        String::new()
    } else {
        format!("\nPossible solutions: {}", names.join(", "))
    }
}

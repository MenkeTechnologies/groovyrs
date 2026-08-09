//! Differential parity fuzzer: `groovy -e <s>` vs our `groovy -e <s>`.
//!
//! Generates thousands of grammar-driven, deterministic-output Groovy snippets,
//! runs each through both the reference `groovy` (the oracle) and our `groovy`,
//! and reports every case where stdout OR success/failure diverge. Each case is
//! produced from a per-index seed so any divergence replays exactly:
//! `parity-fuzz --seed <N> --once`.
//!
//! **Scope invariant.** The generator only emits constructs groovyrs actually
//! implements: arithmetic, comparisons, `&&`/`||`, string `+` concatenation,
//! `if`/`while`/`for`-`in` over ranges and collections, `break`/`continue`,
//! `println`/`print`, Groovy truthiness over every value shape, closures and the
//! closure-driven GDK over lists and maps, the spread operator, `GString`
//! interpolation, `try`/`catch`/`finally`/`throw`, classes and interfaces,
//! `getClass()`, and `String.toBigDecimal()`. It stays out of constructs
//! groovyrs rejects — a mutual error teaches nothing.
//!
//! **Determinism invariant.** Every case has output that is identical on any
//! correct runtime. The generator stays clear of the two *documented*
//! simplifications (see BUGS.md) so a reported divergence is a real parity gap:
//!
//! * integer arithmetic only (`+ - * %`), with small operands so no result
//!   overflows `int` (Groovy wraps an overflowing `Integer` at 32 bits, groovyrs
//!   at 64);
//! * every `/` in the arithmetic modes keeps a non-zero right operand, since a
//!   zero divisor aborts an unarmed program in both runtimes and a mutual abort
//!   teaches nothing. `%` by zero has its own dedicated mode (`modzero`), which
//!   pins the three different answers Groovy gives (`/ by zero` for two
//!   Integers, `Division by zero` for a BigDecimal operand, `NaN` for a double)
//!   both caught and uncaught.
//!
//! Decimals are *not* restricted: since the `BigDecimal` value model landed
//! (src/decimal.rs) they carry exact scale through `+ - * / %`, so arbitrary
//! literals, scales, and exponent forms are all in scope — `10 * 1.25` (`12.50`),
//! `1/3` (`0.3333333333`), `2.5e7 + 1` (`25000001`). Narrowing that generator
//! again would hide exactly the regressions it exists to catch.
//!
//! Within that surface the fuzzer is a parity/regression prover: any divergence
//! is a groovyrs bug (the kind the `continue`-codegen fix in slice 1 was).
//!
//! Subprocess-only: this binary never links the groovyrs library — it compares
//! two `groovy` processes, exactly as a user would observe them.
//!
//! Build:  cargo build --bin parity-fuzz
//! Run:    ./target/debug/parity-fuzz --count 2000 --mode control

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Deterministic PRNG (splitmix64) — no `rand` dependency.
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % n
        }
    }
    fn range_i(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo + 1) as u64) as i64
    }
    fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }
}

fn pick<'a, T>(rng: &mut Rng, xs: &'a [T]) -> &'a T {
    &xs[rng.below(xs.len() as u64) as usize]
}

// ---------------------------------------------------------------------------
// Modes
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Arith,
    Logic,
    Strings,
    Control,
    Format,
    Truth,
    Closures,
    Gstring,
    Exceptions,
    Faults,
    Switch,
    Asserts,
    ModZero,
    Gdk,
    Conversions,
    Classes,
    Ranges,
    Aliasing,
    Mixed,
}

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Arith => "arith",
        Mode::Logic => "logic",
        Mode::Strings => "strings",
        Mode::Control => "control",
        Mode::Format => "format",
        Mode::Truth => "truth",
        Mode::Closures => "closures",
        Mode::Gstring => "gstring",
        Mode::Exceptions => "exceptions",
        Mode::Faults => "faults",
        Mode::Switch => "switch",
        Mode::Asserts => "asserts",
        Mode::ModZero => "modzero",
        Mode::Gdk => "gdk",
        Mode::Conversions => "conversions",
        Mode::Classes => "classes",
        Mode::Ranges => "ranges",
        Mode::Aliasing => "aliasing",
        Mode::Mixed => "mixed",
    }
}

fn mode_from(s: &str) -> Option<Mode> {
    Some(match s {
        "arith" => Mode::Arith,
        "logic" => Mode::Logic,
        "strings" => Mode::Strings,
        "control" => Mode::Control,
        "format" => Mode::Format,
        "truth" => Mode::Truth,
        "closures" => Mode::Closures,
        "gstring" => Mode::Gstring,
        "exceptions" => Mode::Exceptions,
        "faults" => Mode::Faults,
        "switch" => Mode::Switch,
        "asserts" => Mode::Asserts,
        "modzero" => Mode::ModZero,
        "gdk" => Mode::Gdk,
        "conversions" => Mode::Conversions,
        "classes" => Mode::Classes,
        "ranges" => Mode::Ranges,
        "aliasing" => Mode::Aliasing,
        "mixed" => Mode::Mixed,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Expression generators — each stays inside the deterministic surface.
// ---------------------------------------------------------------------------

/// A small integer arithmetic expression (`+ - * %`, unary `-`, grouping). `%`
/// keeps a positive right operand so the sign convention never matters. Operands
/// stay small so no result overflows.
fn gen_int(rng: &mut Rng, depth: u32) -> String {
    if depth == 0 || rng.chance(1, 3) {
        return rng.range_i(0, 40).to_string();
    }
    match rng.below(6) {
        0 => format!(
            "({} + {})",
            gen_int(rng, depth - 1),
            gen_int(rng, depth - 1)
        ),
        1 => format!(
            "({} - {})",
            gen_int(rng, depth - 1),
            gen_int(rng, depth - 1)
        ),
        2 => format!(
            "({} * {})",
            gen_int(rng, depth - 1),
            gen_int(rng, depth - 1)
        ),
        3 => format!("({} % {})", gen_int(rng, depth - 1), rng.range_i(1, 12)),
        4 => format!("(-{})", gen_int(rng, depth - 1)),
        _ => rng.range_i(0, 40).to_string(),
    }
}

/// A decimal literal with an arbitrary scale: plain (`0.750`), exponent form
/// (`2.5e-3`, whose scale is negative for a positive exponent), or `d`-suffixed
/// (an IEEE double, which prints and divides by different rules). Sometimes
/// negative. Every shape is in scope for the `BigDecimal` value model.
fn gen_decimal(rng: &mut Rng) -> String {
    let digits = rng.range_i(1, 7) as usize;
    let mut mantissa = String::new();
    for _ in 0..digits {
        mantissa.push(char::from(b'0' + rng.below(10) as u8));
    }
    // Place the point anywhere inside the digits (scale 0 keeps it integral).
    let scale = rng.below(digits as u64 + 2) as usize;
    let mut text = if scale == 0 {
        format!("{mantissa}.0")
    } else if scale < mantissa.len() {
        let split = mantissa.len() - scale;
        format!("{}.{}", &mantissa[..split], &mantissa[split..])
    } else {
        format!("0.{}{mantissa}", "0".repeat(scale - mantissa.len()))
    };
    match rng.below(6) {
        0 => text.push_str(&format!("e{}", rng.range_i(-8, 8))),
        1 => text.push('d'),
        _ => {}
    }
    if rng.chance(1, 4) {
        text.insert(0, '-');
    }
    text
}

/// A decimal literal guaranteed non-zero, for the right operand of `/` and `%`
/// (a zero divisor aborts both runtimes).
fn gen_nonzero_decimal(rng: &mut Rng) -> String {
    let text = gen_decimal(rng);
    let magnitude = text.trim_start_matches('-').trim_end_matches('d');
    if magnitude.parse::<f64>().map_or(true, |v| v == 0.0) {
        return "3.7".to_string();
    }
    text
}

/// A decimal arithmetic expression: `+ - * / %` over decimal literals and small
/// integers, exercising scale accumulation, the `BigDecimal` division policy, and
/// the `BigDecimal`/`double` mixes.
fn gen_dec_arith(rng: &mut Rng, depth: u32) -> String {
    if depth == 0 || rng.chance(1, 3) {
        return if rng.chance(1, 4) {
            rng.range_i(0, 40).to_string()
        } else {
            gen_decimal(rng)
        };
    }
    match rng.below(5) {
        0 => format!(
            "({} + {})",
            gen_dec_arith(rng, depth - 1),
            gen_dec_arith(rng, depth - 1)
        ),
        1 => format!(
            "({} - {})",
            gen_dec_arith(rng, depth - 1),
            gen_dec_arith(rng, depth - 1)
        ),
        2 => format!(
            "({} * {})",
            gen_dec_arith(rng, depth - 1),
            gen_dec_arith(rng, depth - 1)
        ),
        3 => format!(
            "({} / {})",
            gen_dec_arith(rng, depth - 1),
            gen_nonzero_decimal(rng)
        ),
        _ => format!(
            "({} % {})",
            gen_dec_arith(rng, depth - 1),
            gen_nonzero_decimal(rng)
        ),
    }
}

/// A single division with a non-zero divisor. Any divisor is fair game now that
/// quotients are exact `BigDecimal`s — `1/3` prints `0.3333333333` on both sides.
fn gen_div(rng: &mut Rng) -> String {
    let d = rng.range_i(1, 50);
    let n = rng.range_i(0, 200);
    format!("{n} / {d}")
}

/// A boolean expression: comparisons of integer expressions joined by
/// short-circuit `&&`/`||`, with optional `!`.
fn gen_bool(rng: &mut Rng, depth: u32) -> String {
    if depth == 0 || rng.chance(1, 2) {
        let op = *pick(rng, &["==", "!=", "<", ">", "<=", ">="]);
        return format!("({} {} {})", gen_int(rng, 2), op, gen_int(rng, 2));
    }
    match rng.below(3) {
        0 => format!(
            "({} && {})",
            gen_bool(rng, depth - 1),
            gen_bool(rng, depth - 1)
        ),
        1 => format!(
            "({} || {})",
            gen_bool(rng, depth - 1),
            gen_bool(rng, depth - 1)
        ),
        _ => format!("(!{})", gen_bool(rng, depth - 1)),
    }
}

/// A string-valued expression: a quoted literal, or a `+` concatenation mixing
/// strings with an int / boolean / decimal / `null` operand (Groovy's `+`
/// overload — the strict numeric hook path).
fn gen_string(rng: &mut Rng, depth: u32) -> String {
    const WORDS: &[&str] = &["x", "val=", "-", " ", "n:", "ok", "café", "a1"];
    if depth == 0 || rng.chance(1, 3) {
        let quote = if rng.chance(1, 2) { '"' } else { '\'' };
        return format!("{quote}{}{quote}", pick(rng, WORDS));
    }
    let rhs = match rng.below(5) {
        0 => gen_int(rng, 2),
        1 => gen_decimal(rng),
        2 => (*pick(rng, &["true", "false"])).to_string(),
        3 => "null".to_string(),
        _ => gen_string(rng, depth - 1),
    };
    format!("({} + {})", gen_string(rng, depth - 1), rhs)
}

/// A value to print in the `format` mode — stresses `groovy_str`/`format_decimal`
/// across booleans, `null`, negatives, dyadic decimals, and terminating divisions.
fn gen_format_value(rng: &mut Rng) -> String {
    match rng.below(7) {
        0 => (*pick(rng, &["true", "false"])).to_string(),
        1 => "null".to_string(),
        2 => format!("-{}", rng.range_i(1, 999)),
        3 => gen_decimal(rng),
        4 => gen_dec_arith(rng, 3),
        5 => gen_string(rng, 2),
        _ => gen_int(rng, 2),
    }
}

/// A value of *any* Groovy shape, chosen for its truthiness behaviour: numbers
/// (including a zero `BigDecimal`, which fusevm's own truth test gets wrong),
/// strings (including `""` and `"0"`, which fusevm reads shell-style), `null`,
/// booleans, and empty / non-empty lists and maps.
///
/// Ranges are deliberately absent: groovyrs materialises a range value to a list
/// (a *documented* simplification — see BUGS.md), so `println(0..2)` prints
/// `[0, 1, 2]` where Groovy prints `0..2`. Emitting one here would report that
/// known gap instead of a truthiness one.
fn gen_truthy_value(rng: &mut Rng) -> String {
    const VALUES: &[&str] = &[
        "0", "1", "-3", "0.0", "0.00", "1.50", "-2.5", "0.0d", "2.5d", "0e0", "\"\"", "\"0\"",
        "\"x\"", "null", "true", "false", "[]", "[1, 2]", "[:]", "[a: 1]",
    ];
    (*pick(rng, VALUES)).to_string()
}

/// A truthiness program. Every case is a **self-contained one-liner**: the
/// binding and the observation share a statement, so the shrinker cannot delete
/// a declaration and leave an unbound reference behind (which would report the
/// unrelated undefined-variable gap instead of a truthiness one).
///
/// This is the mode that pins `if (0.0)` and `if ("0")`, the boolean-valued
/// `&&`/`||`, and the operand-valued Elvis — and, through the counted loop, that
/// a comparison-shaped guard still behaves after the truthiness change.
fn gen_truth(rng: &mut Rng) -> Vec<String> {
    let mut out = Vec::new();
    let n = rng.range_i(1, 3) as usize;
    for k in 0..n {
        let v = gen_truthy_value(rng);
        // Half the cases test the value through a variable (which is where the
        // Groovy-truthiness builtin is emitted) and half inline (where the
        // compiler may know the type statically).
        let (bind, name) = if rng.chance(1, 2) {
            (format!("def v{k} = {v}; "), format!("v{k}"))
        } else {
            (String::new(), v)
        };
        out.push(match rng.below(6) {
            0 => format!("{bind}if ({name}) println(\"T{k}\") else println(\"F{k}\")"),
            1 => format!("{bind}println({name} ? \"t{k}\" : \"f{k}\")"),
            2 => format!("{bind}println({name} ?: \"elvis{k}\")"),
            3 => format!("{bind}println({name} && true)"),
            4 => format!("{bind}println({name} || false)"),
            _ => format!("{bind}println(!{name})"),
        });
    }
    // A counted loop over a comparison-shaped guard — the native/JIT condition
    // path the truthiness fix deliberately leaves untouched. `for`-`in` binds its
    // own variable, so this line stands alone too.
    let hi = rng.range_i(0, 3);
    out.push(format!("for (c in 0..<{hi}) {{ println(\"w\" + c) }}"));
    out
}

/// A closure program: a list run through the closure-driven GDK, plus the
/// first-class forms (direct call, `.call`, currying, a factory closure, and the
/// explicit zero-parameter `{ -> … }`).
fn gen_closures(rng: &mut Rng) -> Vec<String> {
    let mut out = Vec::new();
    let len = rng.range_i(0, 4) as usize;
    let items: Vec<String> = (0..len).map(|_| rng.range_i(-5, 9).to_string()).collect();
    out.push(format!("def xs = [{}]", items.join(", ")));
    let k = rng.range_i(1, 4);
    match rng.below(8) {
        0 => out.push(format!("println(xs.collect {{ it * {k} }})")),
        1 => out.push(format!("println(xs.findAll {{ it % {k} == 0 }})")),
        2 => out.push(format!("println(xs.find {{ it > {k} }})")),
        3 => out.push("println(xs.inject(0) { a, v -> a + v })".to_string()),
        4 => out.push(format!("xs.each {{ println(\"e\" + (it + {k})) }}")),
        5 => out.push("xs.eachWithIndex { v, i -> println(i + \":\" + v) }".to_string()),
        6 => out.push("println(xs.sum())".to_string()),
        _ => out.push(format!("println(xs.collect {{ it }}.size() + {k})")),
    }
    match rng.below(5) {
        0 => {
            out.push(format!("def f = {{ it + {k} }}"));
            out.push("println(f(10))".to_string());
            out.push("println(f.call(1))".to_string());
        }
        1 => {
            out.push("def g = { a, b -> a * b }".to_string());
            out.push(format!("println(g({k}, 6))"));
        }
        2 => {
            out.push("def curry = { x -> { y -> x + y } }".to_string());
            out.push(format!("println(curry({k})(9))"));
        }
        3 => {
            out.push("def make(n) { return { it + n } }".to_string());
            out.push(format!("println(make({k})(4))"));
        }
        _ => {
            out.push(format!("def z = {{ -> {k} * 3 }}"));
            out.push("println(z())".to_string());
        }
    }
    out
}

/// A `GString` program: literal text interleaved with `$name` paths and
/// `${ expr }` placeholders over bound values of several shapes.
fn gen_gstring(rng: &mut Rng) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!("def a = {}", gen_truthy_value(rng)));
    out.push(format!("def n = {}", rng.range_i(-9, 20)));
    out.push("def m = [b: 7, c: 3]".to_string());
    let n = rng.range_i(1, 4) as usize;
    for k in 0..n {
        let body = match rng.below(8) {
            0 => "$a".to_string(),
            1 => "${a}".to_string(),
            2 => format!("${{n + {}}}", rng.range_i(1, 9)),
            3 => "$m.b".to_string(),
            4 => "${m}".to_string(),
            5 => "$a$n".to_string(),
            6 => format!("${{ n > {} ? \"big\" : \"small\" }}", rng.range_i(0, 10)),
            _ => "${ [1, 2].collect { it * 2 } }".to_string(),
        };
        out.push(format!("println(\"p{k} {body} q\")"));
    }
    // A `$` that is escaped, and a single-quoted string, must stay inert.
    out.push("println('lit $a')".to_string());
    out.push("println(\"esc \\$a\")".to_string());
    out
}

/// The throwable types the exception generator throws and catches. Pairing a
/// thrown type with a *non*-matching catch type is deliberate: the exception
/// then escapes, which exercises the uncaught path (stdout up to the throw, plus
/// a non-zero exit) as well as the caught one.
const EXC_TYPES: &[&str] = &[
    "Exception",
    "RuntimeException",
    "IllegalStateException",
    "IllegalArgumentException",
    "NumberFormatException",
    "ArithmeticException",
    "IOException",
];

/// An exception program: a function whose body throws under a data-dependent
/// guard, wrapped in `try`/`catch`/`finally`, driven over a range. Covers the
/// caught path, the `finally` on both exits, an early `return` out of a `try`
/// with a `finally`, a rethrow from a handler, and the uncaught escape.
fn gen_exceptions(rng: &mut Rng) -> Vec<String> {
    let mut out = Vec::new();
    let hi = rng.range_i(1, 3);
    let trip = rng.range_i(0, hi);
    let thrown = *pick(rng, EXC_TYPES);
    // A catch of the thrown type, of a supertype, or of an unrelated type.
    let caught = match rng.below(4) {
        0 => thrown,
        1 | 2 => "Exception",
        _ => *pick(rng, EXC_TYPES),
    };
    let with_finally = rng.chance(3, 5);
    match rng.below(4) {
        // A throwing function called from a guarded loop.
        0 | 1 => {
            out.push("def f(i) {".to_string());
            out.push("  try {".to_string());
            out.push(format!(
                "    if (i == {trip}) throw new {thrown}(\"boom\" + i)"
            ));
            out.push("    return \"ok\" + i".to_string());
            out.push(format!("  }} catch ({caught} e) {{"));
            out.push("    return \"c:\" + e.message".to_string());
            out.push("  }".to_string());
            if with_finally {
                out.push("  finally { println(\"fin\" + i) }".to_string());
            }
            out.push("}".to_string());
            out.push(format!("for (i in 0..{hi}) println(f(i))"));
        }
        // A top-level try whose handler rethrows.
        2 => {
            out.push("try {".to_string());
            out.push("  try {".to_string());
            out.push(format!("    throw new {thrown}(\"inner\")"));
            out.push(format!("  }} catch ({caught} e) {{"));
            out.push("    throw new IllegalStateException(\"re:\" + e.message)".to_string());
            out.push("  }".to_string());
            if with_finally {
                out.push("  finally { println(\"f1\") }".to_string());
            }
            out.push("} catch (Exception e) { println(\"outer \" + e.message) }".to_string());
        }
        // A zero divisor, which Groovy raises as a catchable ArithmeticException.
        _ => {
            let d = if rng.chance(1, 2) {
                0
            } else {
                rng.range_i(1, 5)
            };
            out.push(format!("def q = {}", rng.range_i(0, 40)));
            out.push("try {".to_string());
            out.push(format!("  println(q / {d})"));
            out.push(format!("}} catch ({caught} e) {{"));
            out.push("  println(\"div \" + e.message)".to_string());
            out.push("}".to_string());
            if with_finally {
                out.push("println(\"done\")".to_string());
            }
        }
    }
    out
}

/// One runtime-fault probe: a Groovy expression, the throwable it raises, and
/// whether that throwable's `getMessage()` is reproducible byte for byte.
///
/// `msg_ok` is `false` exactly where Groovy appends its fuzzy
/// `Possible solutions: …` suggestion list, which enumerates the receiver's real
/// GDK signatures and so cannot be reproduced without the JDK's method tables
/// (see BUGS.md). Those probes still assert the throwable *type* and the control
/// flow around it — only the message text is out of scope.
const FAULTS: &[(&str, &str, bool)] = &[
    ("\"hi\".nope()", "MissingMethodException", false),
    ("[1, 2, 3].nope()", "MissingMethodException", false),
    ("[a: 1].nope()", "MissingMethodException", false),
    ("5.nope()", "MissingMethodException", false),
    ("2.5.nope()", "MissingMethodException", false),
    ("true.nope()", "MissingMethodException", false),
    ("5[0]", "MissingMethodException", false),
    ("\"hi\".zork", "MissingPropertyException", true),
    ("5.zork", "MissingPropertyException", true),
    ("true.zork", "MissingPropertyException", true),
    ("nil.length()", "NullPointerException", true),
    ("nil.charAt(0)", "NullPointerException", true),
    ("nil.zork", "NullPointerException", true),
    ("[1, 2, 3].get(9)", "IndexOutOfBoundsException", true),
    ("[1, 2, 3].get(-1)", "IndexOutOfBoundsException", true),
    ("\"abc\"[9]", "StringIndexOutOfBoundsException", true),
    ("\"abc\"[-9]", "ArrayIndexOutOfBoundsException", true),
    ("[1, 2, 3][-9]", "ArrayIndexOutOfBoundsException", true),
    ("\"abc\".toInteger()", "NumberFormatException", true),
    ("\"abc\".toLong()", "NumberFormatException", true),
    ("\"1x\".toDouble()", "NumberFormatException", true),
    ("1 / 0", "ArithmeticException", true),
];

/// Supertypes a probe's throwable can also be caught as, so a generated `catch`
/// can name an ancestor and still match. Each list is rooted at the probe's own
/// type and walks up Groovy's hierarchy.
fn fault_supertypes(class: &str) -> &'static [&'static str] {
    match class {
        "MissingMethodException" | "MissingPropertyException" => {
            &["GroovyRuntimeException", "RuntimeException", "Exception"]
        }
        "NumberFormatException" => &["IllegalArgumentException", "RuntimeException", "Exception"],
        "StringIndexOutOfBoundsException" | "ArrayIndexOutOfBoundsException" => {
            &["IndexOutOfBoundsException", "RuntimeException", "Exception"]
        }
        _ => &["RuntimeException", "Exception"],
    }
}

/// A runtime-fault program: a probe from [`FAULTS`] placed either under a
/// matching `catch` (the handler runs), under a deliberately *non*-matching
/// `catch` (the throwable escapes — a non-zero exit and truncated stdout), or
/// bare (the uncaught path). `finally` and a surrounding function frame are
/// mixed in, since a fault has to unwind the same way an explicit `throw` does.
fn gen_faults(rng: &mut Rng) -> Vec<String> {
    let (expr, class, msg_ok) = *pick(rng, FAULTS);
    // A script *binding* (no `def`), so the probe still resolves `nil` from
    // inside a generated function — a top-level `def` would be local to `run`.
    let mut out = vec!["nil = null".to_string()];
    out.push("println(\"before\")".to_string());
    // Catch the exact type, an ancestor, or something unrelated (which escapes).
    let caught = match rng.below(4) {
        0 | 1 => class,
        2 => *pick(rng, fault_supertypes(class)),
        _ => "IllegalStateException",
    };
    // Only print the message where Groovy's text is reproducible; otherwise
    // print a fixed marker so the case still pins the type and the control flow.
    let report = if msg_ok && rng.chance(1, 2) {
        "println(\"caught \" + e.message)"
    } else {
        "println(\"caught\")"
    };
    let with_finally = rng.chance(1, 2);
    match rng.below(3) {
        // The probe inline, under a handler.
        0 => {
            out.push("try {".to_string());
            out.push(format!("  println({expr})"));
            out.push(format!("}} catch ({caught} e) {{"));
            out.push(format!("  {report}"));
            out.push("}".to_string());
            if with_finally {
                out.pop();
                out.push("} finally { println(\"fin\") }".to_string());
            }
        }
        // The probe inside a function, so the throwable crosses a frame.
        1 => {
            out.push("def f() {".to_string());
            out.push(format!("  return {expr}"));
            out.push("}".to_string());
            out.push("try {".to_string());
            out.push("  println(f())".to_string());
            out.push(format!("}} catch ({caught} e) {{ {report} }}"));
        }
        // Uncaught: stdout stops at the fault and the exit status is non-zero.
        _ => out.push(format!("println({expr})")),
    }
    out.push("println(\"after\")".to_string());
    out
}

/// `case` labels the switch generator emits, paired with the subjects that make
/// them interesting. Each exercises a different `isCase` rule: a constant is
/// `equals`, a range and a list are `contains`, a type is `instanceof`, a
/// pattern is a whole-string match, and a closure is called with the subject.
const CASE_LABELS: &[&str] = &[
    "1",
    "2",
    "\"s\"",
    "3..5",
    "[7, 8]",
    "String",
    "Integer",
    "~/a+b/",
    "{ it instanceof Integer && it > 100 }",
    "null",
];

/// Subjects the switch generator dispatches, chosen so every label above both
/// matches and misses across the corpus.
const CASE_SUBJECTS: &[&str] = &[
    "0", "1", "2", "4", "7", "101", "\"s\"", "\"aab\"", "\"zz\"", "null",
];

/// A `switch` / `do`-`while` / labeled-jump program. Fall-through is deliberate
/// (a section only sometimes ends in `break`), and the labeled jumps target both
/// the inner and the outer loop, since binding a label to the wrong frame is the
/// bug class this mode exists to catch.
fn gen_switch(rng: &mut Rng) -> Vec<String> {
    let mut out = Vec::new();
    match rng.below(3) {
        // A switch over several subjects, with fall-through and a default.
        0 => {
            let n = rng.range_i(2, 4) as usize;
            let mut labels: Vec<&str> = Vec::new();
            while labels.len() < n {
                let l = *pick(rng, CASE_LABELS);
                if !labels.contains(&l) {
                    labels.push(l);
                }
            }
            out.push("def f(x) {".to_string());
            out.push("  def r = \"\"".to_string());
            out.push("  switch (x) {".to_string());
            for (i, l) in labels.iter().enumerate() {
                out.push(format!("    case {l}: r = r + \"{i}\""));
                // Omitting `break` is the fall-through case.
                if rng.chance(2, 3) {
                    out.push("      break".to_string());
                }
            }
            if rng.chance(3, 4) {
                out.push("    default: r = r + \"d\"".to_string());
            }
            out.push("  }".to_string());
            out.push("  return r".to_string());
            out.push("}".to_string());
            let subjects = rng.range_i(3, 6) as usize;
            let list: Vec<String> = (0..subjects)
                .map(|_| pick(rng, CASE_SUBJECTS).to_string())
                .collect();
            out.push(format!("def xs = [{}]", list.join(", ")));
            out.push("for (i in 0..<xs.size()) println(\"\" + xs[i] + \" -> \" + f(xs[i]))".into());
        }
        // A `do`/`while`, which must run its body before the first test.
        1 => {
            let hi = rng.range_i(0, 4);
            out.push("def i = 0".to_string());
            out.push("do {".to_string());
            out.push("  i++".to_string());
            if rng.chance(1, 3) {
                out.push(format!("  if (i == {}) continue", rng.range_i(1, 4)));
            }
            if rng.chance(1, 4) {
                out.push(format!("  if (i == {}) break", rng.range_i(1, 5)));
            }
            out.push("  println(\"i \" + i)".to_string());
            out.push(format!("}} while (i < {hi})"));
            out.push("println(\"end \" + i)".to_string());
        }
        // Nested labeled loops: the jump must bind to the named frame, and a
        // `continue` inside a `switch` must pass through to the loop around it.
        _ => {
            let trip = rng.range_i(0, 2);
            let outer_jump = rng.chance(1, 2);
            out.push("outer:".to_string());
            out.push("for (a in 0..2) {".to_string());
            out.push("  inner:".to_string());
            out.push("  for (b in 0..2) {".to_string());
            out.push(format!("    if (b == {trip}) {{"));
            let (kw, label) = (
                if rng.chance(1, 2) {
                    "break"
                } else {
                    "continue"
                },
                if outer_jump { "outer" } else { "inner" },
            );
            out.push(format!("      {kw} {label}"));
            out.push("    }".to_string());
            if rng.chance(1, 2) {
                out.push("    switch (b) {".to_string());
                out.push("      case 1: continue".to_string());
                out.push("      default: break".to_string());
                out.push("    }".to_string());
            }
            out.push("    println(\"\" + a + b)".to_string());
            out.push("  }".to_string());
            out.push("}".to_string());
        }
    }
    out
}

/// Condition shapes the `assert` generator renders, each exercising a different
/// corner of the power-assert layout: a bare variable, a nested binary (two
/// recorded columns on one line), a method call (whose receiver is pushed down a
/// line because its value is too wide to share one), a subscript (recorded under
/// the `[`), a unary operator, an `instanceof`, and a short-circuit chain.
const ASSERT_CONDITIONS: &[&str] = &[
    "x == LIT",
    "x + 1 == LIT",
    "x * 2 - 1 == LIT",
    "s.length() == LIT",
    "s.toUpperCase().length() == LIT",
    "l[0] == LIT",
    "l[x - 2] == LIT",
    "!x",
    "-x == LIT",
    "x > 1 && x > LIT",
    "x > LIT || x > 8",
    "l.isEmpty()",
    "s.contains(\"z\")",
    "l == [LIT]",
    "m.a == LIT",
    "x instanceof String",
    "l.size() + s.length() == LIT",
    "(x + 1) == LIT",
];

/// An `assert` program. Half the cases are written to pass (nothing printed,
/// exit 0) and half to fail, which is where the power-assert layout — the
/// verbatim source line, the `|` markers, and every recorded value under its own
/// source column — has to match Groovy byte for byte. Both the bare form
/// (a `PowerAssertionError` carrying the layout) and the `: message` form (a
/// plain `AssertionError` with the `Expression:` / `Values:` clauses) are
/// generated, caught and printed as well as left to escape.
fn gen_asserts(rng: &mut Rng) -> Vec<String> {
    let x = rng.range_i(0, 4);
    let cond = pick(rng, ASSERT_CONDITIONS)
        // A literal that usually misses, so most cases exercise the failure
        // rendering; sometimes it hits and the assert is silent.
        .replace("LIT", &rng.range_i(0, 6).to_string());
    let mut out = vec![
        format!("x = {x}"),
        "s = \"hi\"".to_string(),
        "l = [1, 2]".to_string(),
        "m = [a: 1]".to_string(),
        "println(\"before\")".to_string(),
    ];
    // A `: message` condition must stay a plain binary, since that is the only
    // shape Groovy's `Values:` clause reports on.
    let with_message = rng.chance(1, 3) && cond.contains("==");
    let stmt = if with_message {
        format!("assert {cond} : \"boom\"")
    } else {
        format!("assert {cond}")
    };
    match rng.below(3) {
        // Caught, with the rendered message printed.
        0 | 1 => {
            out.push("try {".to_string());
            out.push(format!("  {stmt}"));
            out.push("  println(\"passed\")".to_string());
            out.push("} catch (AssertionError e) { println(e.getMessage()) }".to_string());
        }
        // Uncaught: stdout stops at the assert and the exit status is non-zero.
        _ => out.push(stmt),
    }
    out.push("println(\"after\")".to_string());
    out
}

// ---------------------------------------------------------------------------
// Statement / program generators
// ---------------------------------------------------------------------------

/// A `println(<expr>)` probe for a value-producing mode.
fn println_of(expr: String) -> String {
    format!("println({expr})")
}

/// Distinct loop variables per nesting level (so an inner loop never shadows an
/// outer counter).
const LOOP_VARS: &[&str] = &["i", "j", "k", "m", "p"];

/// A control-flow program: `for`-`in` range / `while` loops, up to three levels
/// deep, with `if`/`else` bodies that may `break`/`continue` (binding to the
/// innermost loop) and print integers. This is the mode that exercises the
/// compiler's loop-context stack and jump backpatching hardest — the slice-1
/// `continue`-codegen bug lived exactly here, and nested loops stress the stack
/// that a single loop never touches.
fn gen_control(rng: &mut Rng) -> Vec<String> {
    let mut out = Vec::new();
    let max_level = rng.range_i(0, 2) as usize; // 0, 1, or 2 nested levels
    gen_loop(rng, &mut out, 0, max_level);
    out
}

/// Emit one loop at nesting `level`, recursing for a nested loop up to
/// `max_level`. `while` loops advance their counter before every `continue` and
/// once at the end, so termination is guaranteed regardless of the guards taken.
fn gen_loop(rng: &mut Rng, out: &mut Vec<String>, level: usize, max_level: usize) {
    let var = LOOP_VARS[level.min(LOOP_VARS.len() - 1)];
    let ind = "  ".repeat(level);
    let bind = "  ".repeat(level + 1);
    let lo = rng.range_i(0, 3);
    // span 0 admits boundary ranges: `lo..<lo` is empty, `lo..lo` is one iter.
    let hi = lo + rng.range_i(0, 4);
    let is_while = rng.chance(2, 5);

    if is_while {
        out.push(format!("{ind}def {var} = {lo}"));
        out.push(format!("{ind}while ({var} <= {hi}) {{"));
    } else {
        let op = if rng.chance(1, 2) { ".." } else { "..<" };
        out.push(format!("{ind}for ({var} in {lo}{op}{hi}) {{"));
    }

    // continue guard (a `while` must advance before continuing or it spins)
    if rng.chance(2, 5) {
        let g = rng.range_i(lo, hi);
        out.push(format!("{bind}if ({var} == {g}) {{"));
        if is_while {
            out.push(format!("{bind}  {var}++"));
        }
        out.push(format!("{bind}  continue"));
        out.push(format!("{bind}}}"));
    }
    // break guard
    if rng.chance(1, 3) {
        let g = rng.range_i(lo, hi);
        out.push(format!("{bind}if ({var} == {g}) break"));
    }
    // a conditional print keeps output varied but deterministic
    if rng.chance(1, 2) {
        out.push(format!(
            "{bind}if ({var} % 2 == 0) println({var}) else println(\"odd \" + {var})"
        ));
    } else {
        out.push(format!("{bind}println({var} * {var})"));
    }
    // a nested loop — the inner break/continue must bind to the inner loop only
    if level < max_level && rng.chance(3, 5) {
        gen_loop(rng, out, level + 1, max_level);
    }
    if is_while {
        out.push(format!("{bind}{var}++"));
    }
    out.push(format!("{ind}}}"));
}

/// Zero-divisor `%` probes: an `Integer % 0` raises `/ by zero`, a `BigDecimal`
/// operand raises `Division by zero` / `Division undefined`, and a `double`
/// operand answers `NaN` without raising. The divisor comes from a *variable* in
/// half the cases so the compile-time literal elision is exercised both ways.
const MOD_ZERO_OPERANDS: &[(&str, &str)] = &[
    ("7", "0"),
    ("-7", "0"),
    ("0", "0"),
    ("7.5", "0"),
    ("0.0", "0"),
    ("7", "0.0"),
    ("7.50", "0.00"),
    ("7.0d", "0"),
    ("7", "0.0d"),
    ("7.5", "0.0d"),
    ("7.0d", "0.0d"),
];

/// A `%`-by-zero program. Half the cases catch, half let the throwable escape
/// (which pins the exit status and the truncated stdout too).
fn gen_mod_zero(rng: &mut Rng) -> Vec<String> {
    let (a, b) = *pick(rng, MOD_ZERO_OPERANDS);
    let mut out = vec!["println(\"before\")".to_string()];
    // A variable divisor defeats the compiler's literal-zero elision; a literal
    // one exercises the other branch of `Compiler::emit_mod`.
    let divisor = if rng.chance(1, 2) {
        out.push(format!("def z = {b}"));
        "z".to_string()
    } else {
        b.to_string()
    };
    // A non-zero `%` in the same program keeps the native path under test.
    let (n, d) = (rng.range_i(-20, 20), rng.range_i(1, 7));
    out.push(format!("println({n} % {d})"));
    match rng.below(3) {
        0 => out.push(format!("println({a} % {divisor})")),
        1 => {
            out.push(format!("try {{ println({a} % {divisor}) }}"));
            out.push("catch (ArithmeticException e) { println(\"caught \" + e.message) }".into());
        }
        _ => {
            out.push(format!("def x = {a}"));
            out.push(format!("try {{ x %= {divisor}; println(x) }}"));
            out.push("catch (ArithmeticException e) { println(\"c \" + e.message) }".into());
        }
    }
    out.push("println(\"after\")".to_string());
    out
}

/// List and map values the GDK generator dispatches over. Each is chosen so
/// every modeled method has a defined, deterministic answer on it.
const GDK_LISTS: &[&str] = &[
    "[]",
    "[1]",
    "[3, 1, 2]",
    "[3, 1, 2, 3, 1]",
    "[-2, 5, 0, 5]",
    "[\"b\", \"a\", \"cc\"]",
    "[1.50, 0.5, 2]",
];

/// GDK list calls, paired with the list above. Every one is closed under the
/// documented value model (no `Float`, no 32-bit overflow).
const GDK_LIST_CALLS: &[&str] = &[
    "sort()",
    "sort(false)",
    "sort { a, b -> b <=> a }",
    "unique()",
    "reverse()",
    "max()",
    "min()",
    "sum()",
    "sum(100)",
    "join(\"-\")",
    "join()",
    "groupBy { it }",
    "collect { it }",
    "findAll { it != null }",
    "inject(0) { a, b -> a }",
    "size()",
    "collect { it }.join(\",\")",
];

const GDK_MAPS: &[&str] = &["[:]", "[a: 1]", "[b: 2, a: 1, c: 3]", "[x: 0, y: -1]"];

const GDK_MAP_CALLS: &[&str] = &[
    "each { k, v -> println(k + \"=\" + v) }",
    "each { e -> println(e) }",
    "collect { k, v -> k + v }",
    "collect { e -> e.key }",
    "findAll { k, v -> v > 0 }",
    "find { k, v -> v > 0 }",
    "any { k, v -> v > 1 }",
    "every { k, v -> v > 0 }",
    "groupBy { k, v -> v > 1 }",
    "inject(0) { a, e -> a + e.value }",
    "sort()",
    "max { it.value }",
    "min { it.value }",
    "keySet()",
    "values()",
    "size()",
];

/// Non-empty lists the aliasing generator mutates. Every one has at least one
/// element so an index-taking mutator (`remove(0)`, `set(0, …)`, `pop()`) has a
/// defined answer rather than a bounds throw, and they stay numeric so an
/// order-sensitive mutator (`sort`, `unique`) is comparable.
const ALIAS_LISTS: &[&str] = &["[1, 2, 3]", "[3, 1, 2]", "[1, 1, 2]", "[5]", "[2, 4, 6, 8]"];

/// In-place `java.util.List` mutators. Each writes through the receiver in
/// Groovy, so every other name for that same list observes the change — which is
/// the property these programs exist to compare.
const ALIAS_MUTATORS: &[&str] = &[
    "add(9)",
    "add(0, 9)",
    "remove(0)",
    "clear()",
    "sort()",
    "unique()",
    "set(0, 9)",
    "addAll([7, 8])",
    "leftShift(9)",
    "removeAll([1])",
    "retainAll([1, 2])",
    "removeLast()",
    "pop()",
];

/// An aliasing program: build one list, take a **second reference** to it, mutate
/// through one of the two, and print both.
///
/// This is the surface the rest of the generator never reaches. `gen_gdk` builds
/// a list and calls a method on the single name that built it, so a receiver
/// write-back through that one variable is indistinguishable from real aliasing;
/// nothing anywhere else in this file emits a `.add(`, a second name for a list,
/// or a list reached through a map/element/parameter. Each arm below is one way
/// Groovy hands out that second reference.
fn gen_aliasing(rng: &mut Rng) -> Vec<String> {
    let mut out = Vec::new();
    let base = pick(rng, ALIAS_LISTS);
    let m = pick(rng, ALIAS_MUTATORS);
    out.push(format!("def a = {base}"));
    match rng.below(6) {
        // A plain second name.
        0 => {
            out.push("def b = a".to_string());
            out.push(format!("b.{m}"));
            out.push("println b".to_string());
            out.push("println a.is(b)".to_string());
        }
        // Reached through a map value.
        1 => {
            out.push("def m = [k: a]".to_string());
            out.push(format!("m.k.{m}"));
            out.push("println m".to_string());
        }
        // Reached as an element of another list (twice, so both windows move).
        2 => {
            out.push("def outer = [a, a]".to_string());
            out.push(format!("outer[0].{m}"));
            out.push("println outer".to_string());
        }
        // Handed to a closure as a parameter.
        3 => {
            out.push(format!("def f = {{ l -> l.{m} }}"));
            out.push("f(a)".to_string());
        }
        // Captured by a closure.
        4 => {
            out.push(format!("def c = {{ a.{m} }}"));
            out.push("c()".to_string());
        }
        // A *copy* is not an alias — the negative case, so a fix that aliased
        // everything unconditionally diverges here instead of passing.
        _ => {
            out.push("def b = a.collect { it }".to_string());
            out.push(format!("b.{m}"));
            out.push("println b".to_string());
            out.push("println a.is(b)".to_string());
        }
    }
    out.push("println a".to_string());
    out
}

/// A GDK / spread program: one list or map call, plus a spread expression and a
/// `for-in` walk over the same value, so the three list surfaces agree.
fn gen_gdk(rng: &mut Rng) -> Vec<String> {
    let mut out = Vec::new();
    if rng.chance(1, 2) {
        let lst = pick(rng, GDK_LISTS);
        out.push(format!("def xs = {lst}"));
        out.push(format!("println(xs.{})", pick(rng, GDK_LIST_CALLS)));
        // `sort`/`unique` mutate the receiver in Groovy — print it again so a
        // missing write-back diverges.
        out.push("println(xs)".to_string());
        match rng.below(3) {
            0 => out.push("println(xs*.toString())".to_string()),
            1 => out.push("for (x in xs) { println(\"e\" + x) }".to_string()),
            _ => out.push("println(xs*.getClass().size())".to_string()),
        }
    } else {
        let m = pick(rng, GDK_MAPS);
        out.push(format!("def m = {m}"));
        out.push(format!("println(m.{})", pick(rng, GDK_MAP_CALLS)));
        out.push("println(m)".to_string());
        if rng.chance(1, 2) {
            out.push("for (e in m) { println(\"e\" + e) }".to_string());
        }
    }
    out
}

/// Values whose `getClass()` naming is stable across runs (a closure's synthetic
/// class name is not, so closures stay out).
const CLASS_OF: &[&str] = &[
    "1", "-7", "\"s\"", "\"\"", "1.5", "1.50", "2.5e7", "1.5d", "true", "false", "[1, 2]", "[]",
    "[a: 1]", "[:]", "null",
];

/// Strings the `toBigDecimal` generator parses — half valid, half each of
/// `BigDecimal`'s distinct parse diagnostics (including the two inputs whose
/// `NumberFormatException` carries a `null` message).
const BIG_DECIMAL_TEXTS: &[&str] = &[
    "1.5",
    "100.00",
    "0",
    "-3",
    "+4",
    " 7 ",
    "1e5",
    "1E5",
    "1e-7",
    "2.5e7",
    ".5",
    "1.",
    "007",
    "",
    "  ",
    "x",
    "abc",
    "12a",
    "1_0",
    "0x10",
    "1.2.3",
    "..",
    "1e",
    "1e+",
    "1ex",
    "1e5.5",
    "--1",
    "1-",
    "+",
    "-",
    ".",
    "1,5",
    "1.5d",
    "NaN",
    "Infinity",
    "1e999999999999",
    "1e2147483648",
    "1e-2147483648",
];

/// A `getClass()` / `toBigDecimal()` program.
fn gen_conversions(rng: &mut Rng) -> Vec<String> {
    let mut out = Vec::new();
    if rng.chance(1, 2) {
        let v = pick(rng, CLASS_OF);
        out.push(format!("def v = {v}"));
        match rng.below(4) {
            0 => out.push("println(v.getClass())".to_string()),
            1 => out.push("println(v.class)".to_string()),
            2 => out.push("println(v.getClass().getName())".to_string()),
            _ => out.push("println(v.class.simpleName)".to_string()),
        }
    } else {
        let text = pick(rng, BIG_DECIMAL_TEXTS);
        out.push(format!("def s = \"{text}\""));
        out.push("try { println(s.toBigDecimal()) }".to_string());
        out.push("catch (NumberFormatException e) { println(\"nfe \" + e.message) }".to_string());
    }
    out
}

/// A class / interface program: an interface with an abstract method and a
/// `default` one, an implementing class, and the `instanceof` answers for both.
fn gen_classes(rng: &mut Rng) -> Vec<String> {
    let k = rng.range_i(1, 9);
    let mut out = vec![
        "interface Named { def label(); default def shout() { return label() + \"!\" } }".into(),
        "interface Tagged extends Named { def tag() }".into(),
    ];
    let (decl, extra): (String, Vec<String>) = if rng.chance(1, 2) {
        (
            "class Thing implements Tagged {".into(),
            vec![
                format!("  def label() {{ return \"t{k}\" }}"),
                "  def tag() { return \"g\" }".to_string(),
            ],
        )
    } else {
        (
            "class Thing implements Named {".into(),
            vec![format!("  def label() {{ return \"t{k}\" }}")],
        )
    };
    out.push(decl);
    out.extend(extra);
    // A `shout` override in half the cases, so the interface default both wins
    // and loses across the corpus.
    if rng.chance(1, 3) {
        out.push("  def shout() { return \"over\" }".to_string());
    }
    out.push("}".to_string());
    out.push("def t = new Thing()".to_string());
    out.push("println(t.label())".to_string());
    out.push("println(t.shout())".to_string());
    out.push("println(t instanceof Named)".to_string());
    out.push("println(t instanceof Tagged)".to_string());
    out.push("println(t instanceof Thing)".to_string());
    out.push(format!("println([t]*.label() == [\"t{k}\"])"));
    out
}

/// The two endpoints of a generated range, as source text. The pair is drawn so
/// that **either order** is reachable — the `control` mode only ever builds
/// `lo..hi` with `lo <= hi`, which is why it never observed that a descending
/// `for (i in 5..1)` iterated zero times under groovyrs and five times under
/// Groovy. Characters are in the pool for the same reason: `for (c in 'a'..'e')`
/// walks letters, and a naive `c++` counting loop never terminates.
fn gen_range_ends(rng: &mut Rng) -> (String, String) {
    match rng.below(6) {
        // Integer literals — the parser can fold the direction from these.
        0 | 1 => (
            rng.range_i(-3, 5).to_string(),
            rng.range_i(-3, 5).to_string(),
        ),
        // One endpoint behind a variable, so the direction is only known at run
        // time and the folded and unfolded lowerings are both exercised.
        2 => (
            rng.range_i(-3, 5).to_string(),
            format!("({})", rng.range_i(-3, 5)),
        ),
        // Character endpoints, either order.
        3 | 4 => {
            let a = (b'a' + rng.below(6) as u8) as char;
            let b = (b'a' + rng.below(6) as u8) as char;
            (format!("'{a}'"), format!("'{b}'"))
        }
        // Decimal endpoints — a `BigDecimal` range steps by one and keeps scale.
        _ => (
            format!("{}.0", rng.range_i(-2, 3)),
            format!("{}.0", rng.range_i(-2, 3)),
        ),
    }
}

/// A range program: build a range either way round, walk it with `for-in`, and
/// read the members Groovy defines on `Range` itself.
///
/// Four bug classes this mode exists to catch, each of which the `control` mode
/// structurally cannot reach:
///
/// * a descending range iterating zero times,
/// * a character range never terminating,
/// * `..<` dropping the wrong endpoint when the range counts down,
/// * a body that assigns the loop variable changing the iteration.
///
/// Every statement is self-contained — the range literal is repeated rather than
/// bound to a name, and the whole walk is one multi-line entry — so the shrinker
/// cannot delete a binding and leave a dangling reference behind (which would
/// report an unbound-name gap instead of a range one).
fn gen_ranges(rng: &mut Rng) -> Vec<String> {
    let (a, b) = gen_range_ends(rng);
    let op = if rng.chance(1, 2) { ".." } else { "..<" };
    let r = format!("({a}{op}{b})");

    // The `Range` members, which are answered off the *bounds* rather than the
    // endpoints as written: `(4..0).from` is 0.
    let mut out = vec![
        format!("println({r}.size())"),
        format!("println({r}.from)"),
        format!("println({r}.to)"),
        format!("println({r}.isReverse())"),
        format!("println({r}.toList())"),
    ];
    if rng.chance(1, 3) {
        out.push(format!("println({r}.reverse())"));
        out.push(format!("println({r}.step({}))", rng.range_i(1, 3)));
    }

    // The walk itself, as one atomic statement. `acc` accumulates every element,
    // so a wrong direction, a wrong endpoint, or a skipped iteration all show up
    // as a single diff.
    let mut walk = format!("def acc = []\nfor (x in {a}{op}{b}) {{\n");
    if rng.chance(1, 3) {
        walk.push_str("  if (acc.size() == 1) continue\n");
    }
    if rng.chance(1, 4) {
        walk.push_str("  if (acc.size() == 3) break\n");
    }
    walk.push_str("  acc << x\n");
    // Assigning the loop variable must not steer the loop: Groovy iterates a
    // snapshot, so the walk carries on from where it was.
    if rng.chance(1, 4) {
        walk.push_str(&format!("  x = {a}\n"));
    }
    walk.push_str("}\nprintln(acc)");
    out.push(walk);

    // `next`/`previous` — the successor operations a range walks with, read
    // directly so a wrong one is reported at its source rather than as a wrong
    // element three lines later.
    if rng.chance(1, 2) {
        out.push(format!("println(({a}).next())"));
        out.push(format!("println(({b}).previous())"));
    }
    out
}

/// Generate one case (a list of statements) for a mode and seed.
fn gen_case(seed: u64, mode: Mode) -> Vec<String> {
    let mut rng = Rng::new(seed);
    let mode = if mode == Mode::Mixed {
        *pick(
            &mut rng,
            &[
                Mode::Arith,
                Mode::Logic,
                Mode::Strings,
                Mode::Control,
                Mode::Format,
                Mode::Truth,
                Mode::Closures,
                Mode::Gstring,
                Mode::Exceptions,
                Mode::Faults,
                Mode::Switch,
                Mode::Asserts,
                Mode::ModZero,
                Mode::Gdk,
                Mode::Conversions,
                Mode::Classes,
                Mode::Ranges,
                Mode::Aliasing,
            ],
        )
    } else {
        mode
    };
    match mode {
        Mode::Control => gen_control(&mut rng),
        Mode::Truth => gen_truth(&mut rng),
        Mode::Closures => gen_closures(&mut rng),
        Mode::Gstring => gen_gstring(&mut rng),
        Mode::Exceptions => gen_exceptions(&mut rng),
        Mode::Faults => gen_faults(&mut rng),
        Mode::Switch => gen_switch(&mut rng),
        Mode::Asserts => gen_asserts(&mut rng),
        Mode::ModZero => gen_mod_zero(&mut rng),
        Mode::Gdk => gen_gdk(&mut rng),
        Mode::Conversions => gen_conversions(&mut rng),
        Mode::Classes => gen_classes(&mut rng),
        Mode::Ranges => gen_ranges(&mut rng),
        Mode::Aliasing => gen_aliasing(&mut rng),
        _ => {
            let n = rng.range_i(1, 5) as usize;
            (0..n)
                .map(|_| {
                    let expr = match mode {
                        Mode::Arith => match rng.below(5) {
                            0 | 1 => gen_dec_arith(&mut rng, 4),
                            2 => gen_div(&mut rng),
                            _ => gen_int(&mut rng, 4),
                        },
                        Mode::Logic => gen_bool(&mut rng, 4),
                        Mode::Strings => gen_string(&mut rng, 4),
                        Mode::Format => gen_format_value(&mut rng),
                        Mode::Control
                        | Mode::Truth
                        | Mode::Closures
                        | Mode::Gstring
                        | Mode::Exceptions
                        | Mode::Faults
                        | Mode::Switch
                        | Mode::Asserts
                        | Mode::ModZero
                        | Mode::Gdk
                        | Mode::Conversions
                        | Mode::Classes
                        | Mode::Ranges
                        | Mode::Aliasing
                        | Mode::Mixed => unreachable!(),
                    };
                    println_of(expr)
                })
                .collect()
        }
    }
}

fn build_program(stmts: &[String]) -> String {
    stmts.join("\n")
}

// ---------------------------------------------------------------------------
// Binary resolution / invocation
// ---------------------------------------------------------------------------

/// Our `groovy` binary — the sibling of this harness binary.
fn ours_bin() -> PathBuf {
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_groovy") {
        return PathBuf::from(p);
    }
    if let Some(d) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    {
        let cand = d.join("groovy");
        if cand.exists() {
            return cand;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("debug")
        .join("groovy")
}

/// The ORACLE — reference Apache Groovy. Every divergence is "groovyrs disagrees
/// with THIS runtime", so which runtime it is matters. `GROOVYRS_FUZZ_GROOVY`
/// names the oracle explicitly; if set but unusable this is a HARD ERROR.
fn resolve_oracle() -> String {
    if let Ok(p) = std::env::var("GROOVYRS_FUZZ_GROOVY") {
        if version_of(&p).is_none() {
            eprintln!("parity-fuzz: GROOVYRS_FUZZ_GROOVY={p}: not a usable groovy");
            std::process::exit(2);
        }
        return p;
    }
    for p in [
        "groovy",
        "/opt/homebrew/bin/groovy",
        "/usr/local/bin/groovy",
        "/usr/bin/groovy",
    ] {
        if version_of(p).is_some() {
            return p.to_string();
        }
    }
    eprintln!("parity-fuzz: no reference groovy found; set GROOVYRS_FUZZ_GROOVY");
    std::process::exit(2);
}

fn version_of(prog: &str) -> Option<String> {
    let o = Command::new(prog).arg("--version").output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    if s.is_empty() {
        return None;
    }
    Some(s)
}

struct RunOut {
    stdout: Vec<u8>,
    exit: i32,
    timed_out: bool,
}

/// Run `<prog> -e <src>` with a timeout, capturing stdout.
fn run_prog(prog: &Path, src: &str, timeout: Duration) -> RunOut {
    let mut cmd = Command::new(prog);
    cmd.arg("-e")
        .arg(src)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => {
            return RunOut {
                stdout: Vec::new(),
                exit: -1,
                timed_out: false,
            }
        }
    };
    let out_h = child.stdout.take().map(|mut o| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = o.read_to_end(&mut b);
            b
        })
    });

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let exit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit = status.code().unwrap_or(-1);
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let s = child.wait().ok();
                    exit = s.and_then(|s| s.code()).unwrap_or(-1);
                    timed_out = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(_) => {
                exit = -1;
                break;
            }
        }
    }
    let stdout = out_h.and_then(|h| h.join().ok()).unwrap_or_default();
    RunOut {
        stdout,
        exit,
        timed_out,
    }
}

/// stdout mismatch, or success/failure disagreement, is a divergence.
fn differs(oracle: &RunOut, ours: &RunOut) -> bool {
    // A run of ours that had to be killed is a divergence whatever the oracle
    // did. Without this a hanging loop and an oracle that also failed compare
    // equal — two failures reading as agreement.
    if ours.timed_out {
        return true;
    }
    if (oracle.exit == 0) != (ours.exit == 0) {
        return true;
    }
    oracle.stdout != ours.stdout
}

/// Whether the oracle actually *ran* the generated program, which is what makes
/// the case a comparison at all.
///
/// A case where the reference itself never produced anything — it timed out, or
/// it rejected the program before executing a line of it — measures nothing:
/// our side failing too would read as agreement. Those cases are counted and
/// reported as skipped rather than folded into the pass count. A program that
/// prints and *then* throws still counts: the printed prefix and the exit status
/// are both real observations, and several modes are built on exactly that.
fn oracle_ran(o: &RunOut) -> bool {
    !o.timed_out && (o.exit == 0 || !o.stdout.is_empty())
}

fn diverges(script: &str, bin: &Path, oracle: &str, timeout: Duration) -> bool {
    let o = run_prog(Path::new(oracle), script, timeout);
    if !oracle_ran(&o) {
        return false;
    }
    let r = run_prog(bin, script, timeout);
    differs(&o, &r)
}

fn render(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches('\n')
        .to_string()
}

/// Shrink a diverging case to the smallest statement subset that still diverges.
fn minimize(stmts: Vec<String>, bin: &Path, oracle: &str, timeout: Duration) -> Vec<String> {
    let mut cur = stmts;
    let mut changed = true;
    while changed && cur.len() > 1 {
        changed = false;
        for i in 0..cur.len() {
            let mut cand = cur.clone();
            cand.remove(i);
            if cand.is_empty() {
                continue;
            }
            if diverges(&build_program(&cand), bin, oracle, timeout) {
                cur = cand;
                changed = true;
                break;
            }
        }
    }
    cur
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

struct Args {
    count: u64,
    base_seed: u64,
    once: bool,
    timeout_ms: u64,
    out_path: PathBuf,
    max_report: usize,
    jobs: usize,
    mode: Mode,
}

fn parse_args() -> Args {
    let mut count = 1000u64;
    let mut base_seed = 1u64;
    let mut once = false;
    let mut timeout_ms = 15000u64;
    let mut max_report = 100usize;
    let mut mode = Mode::Mixed;
    let mut jobs = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let mut out_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("parity-fuzz")
        .join("divergences.txt");

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--count" | "-c" => {
                i += 1;
                count = argv.get(i).and_then(|s| s.parse().ok()).unwrap_or(count);
            }
            "--seed" | "-s" => {
                i += 1;
                base_seed = argv
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(base_seed);
            }
            "--once" => once = true,
            "--timeout-ms" => {
                i += 1;
                timeout_ms = argv
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(timeout_ms);
            }
            "--out" | "-o" => {
                i += 1;
                if let Some(p) = argv.get(i) {
                    out_path = PathBuf::from(p);
                }
            }
            "--max-report" => {
                i += 1;
                max_report = argv
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(max_report);
            }
            "--jobs" | "-j" => {
                i += 1;
                jobs = argv
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .filter(|&j| j >= 1)
                    .unwrap_or(jobs);
            }
            "--mode" | "-m" => {
                i += 1;
                if let Some(m) = argv.get(i).and_then(|s| mode_from(s)) {
                    mode = m;
                } else {
                    eprintln!(
                        "parity-fuzz: unknown --mode (arith|logic|strings|control|format|truth|closures|gstring|exceptions|faults|switch|asserts|modzero|gdk|conversions|classes|ranges|aliasing|mixed)"
                    );
                    std::process::exit(2);
                }
            }
            "--help" | "-h" => {
                println!(
                    "parity-fuzz — differential Groovy fuzzer (groovy -e vs groovyrs -e)\n\n\
                     options:\n  \
                     -c, --count N        cases to run (default 1000)\n  \
                     -s, --seed N         base seed (default 1)\n  \
                     -m, --mode M         arith|logic|strings|control|format|truth|closures|\n                       gstring|exceptions|faults|switch|asserts|modzero|gdk|\n                       conversions|classes|ranges|aliasing|mixed (default mixed)\n  \
                     -j, --jobs N         parallel workers (default = cores)\n  \
                     --once               replay a single --seed, minimize, dump both sides\n  \
                     --timeout-ms N       per-run timeout (default 15000; groovy boots the JVM)\n  \
                     -o, --out FILE       write divergence report here\n  \
                     --max-report N       stop after N divergences (default 100)\n\n\
                     The oracle is `groovy` on PATH (override with GROOVYRS_FUZZ_GROOVY)."
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("parity-fuzz: unknown argument `{other}` (try --help)");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    Args {
        count,
        base_seed,
        once,
        timeout_ms,
        out_path,
        max_report,
        jobs,
        mode,
    }
}

fn main() {
    let args = parse_args();
    let bin = ours_bin();
    let oracle = resolve_oracle();
    let timeout = Duration::from_millis(args.timeout_ms);

    if !bin.exists() {
        eprintln!(
            "groovyrs `groovy` binary not found at {}; run `cargo build` first",
            bin.display()
        );
        std::process::exit(2);
    }

    // --once: replay a single seed, minimize if it diverges, dump both sides.
    if args.once {
        let stmts = gen_case(args.base_seed, args.mode);
        let script = build_program(&stmts);
        let o = run_prog(Path::new(&oracle), &script, timeout);
        let r = run_prog(&bin, &script, timeout);
        let diverged = !o.timed_out && differs(&o, &r);
        println!("seed   : {}", args.base_seed);
        println!("mode   : {}", mode_name(args.mode));
        let (show, o, r) = if diverged && stmts.len() > 1 {
            let m = minimize(stmts, &bin, &oracle, timeout);
            let ms = build_program(&m);
            let mo = run_prog(Path::new(&oracle), &ms, timeout);
            let mr = run_prog(&bin, &ms, timeout);
            (ms, mo, mr)
        } else {
            (script, o, r)
        };
        println!("program:\n  {}", show.replace('\n', "\n  "));
        println!("--- groovy   exit={} timeout={} ---", o.exit, o.timed_out);
        println!("{}", render(&o.stdout));
        println!("--- groovyrs exit={} timeout={} ---", r.exit, r.timed_out);
        println!("{}", render(&r.stdout));
        println!("--- {} ---", if diverged { "DIVERGE" } else { "match" });
        std::process::exit(if diverged { 1 } else { 0 });
    }

    let next = AtomicU64::new(0);
    let checked = AtomicU64::new(0);
    let timeouts = AtomicU64::new(0);
    let skipped = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let divergences: Mutex<Vec<(u64, String)>> = Mutex::new(Vec::new());
    let start = Instant::now();

    eprintln!("oracle: {}", oracle);
    eprintln!("ours  : {}", bin.display());
    eprintln!(
        "fuzzing {} cases ({}) across {} workers…",
        args.count,
        mode_name(args.mode),
        args.jobs
    );

    std::thread::scope(|scope| {
        for _ in 0..args.jobs {
            scope.spawn(|| loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let idx = next.fetch_add(1, Ordering::Relaxed);
                if idx >= args.count {
                    break;
                }
                let seed = args.base_seed.wrapping_add(idx);
                let stmts = gen_case(seed, args.mode);
                let script = build_program(&stmts);
                let o = run_prog(Path::new(&oracle), &script, timeout);
                let r = run_prog(&bin, &script, timeout);
                checked.fetch_add(1, Ordering::Relaxed);
                if o.timed_out || r.timed_out {
                    timeouts.fetch_add(1, Ordering::Relaxed);
                }
                // A case the oracle never ran is not evidence either way; count
                // it as skipped so a run that measured nothing cannot report a
                // clean pass.
                if !oracle_ran(&o) {
                    skipped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                if differs(&o, &r) {
                    // Re-verify a real gap reproduces before reporting.
                    if !diverges(&script, &bin, &oracle, timeout) {
                        continue;
                    }
                    let minimal = minimize(stmts, &bin, &oracle, timeout);
                    let ms = build_program(&minimal);
                    let mo = run_prog(Path::new(&oracle), &ms, timeout);
                    let mr = run_prog(&bin, &ms, timeout);
                    let rec = format!(
                        "==== seed {seed} ====\n\
                         program:\n  {}\n\
                         groovy   : exit={} timeout={}\n{}\n\
                         groovyrs : exit={} timeout={}\n{}\n",
                        ms.replace('\n', "\n  "),
                        mo.exit,
                        mo.timed_out,
                        render(&mo.stdout),
                        mr.exit,
                        mr.timed_out,
                        render(&mr.stdout),
                    );
                    let mut d = divergences.lock().unwrap();
                    d.push((seed, rec));
                    if d.len() >= args.max_report {
                        stop.store(true, Ordering::Relaxed);
                    }
                }
            });
        }
    });

    let elapsed = start.elapsed();
    let mut divs = divergences.into_inner().unwrap();
    divs.sort_by_key(|(s, _)| *s);
    let done = checked.load(Ordering::Relaxed);
    let to = timeouts.load(Ordering::Relaxed);
    let sk = skipped.load(Ordering::Relaxed);

    println!(
        "\n════════════════════════════════════════════\n\
         checked {done} cases in {:.1}s  ({} timeouts, {} skipped)\n\
         compared  {} cases\n\
         divergences: {}\n\
         ════════════════════════════════════════════",
        elapsed.as_secs_f64(),
        to,
        sk,
        done - sk,
        divs.len()
    );

    if !divs.is_empty() {
        if let Some(parent) = args.out_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let body: String = divs.iter().map(|(_, r)| r.clone()).collect();
        let _ =
            std::fs::File::create(&args.out_path).and_then(|mut f| f.write_all(body.as_bytes()));
        println!(
            "first divergences (full report → {}):\n",
            args.out_path.display()
        );
        for (_, rec) in divs.iter().take(10) {
            println!("{rec}");
        }
        std::process::exit(1);
    }
}

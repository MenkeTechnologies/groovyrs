//! Differential parity fuzzer: `groovy -e <s>` vs our `groovy -e <s>`.
//!
//! Generates thousands of grammar-driven, deterministic-output Groovy snippets,
//! runs each through both the reference `groovy` (the oracle) and our `groovy`,
//! and reports every case where stdout OR success/failure diverge. Each case is
//! produced from a per-index seed so any divergence replays exactly:
//! `parity-fuzz --seed <N> --once`.
//!
//! **Scope invariant.** The generator only emits constructs groovyrs actually
//! implements (arithmetic, comparisons, `&&`/`||`, string `+` concatenation,
//! `if`/`while`/`for`-`in` ranges, `break`/`continue`, `println`/`print`). It
//! never emits methods, closures, GStrings, or collections — groovyrs rejects
//! those, and a mutual error teaches nothing.
//!
//! **Determinism invariant.** Every case has output that is identical on any
//! correct runtime. In particular the generator stays clear of the *documented*
//! f64-vs-BigDecimal divergences (see BUGS.md) so a reported divergence is a real
//! parity gap, never a known simplification:
//!
//! * integer arithmetic only (`+ - * %`), with small operands so no result
//!   overflows `i64`/`int`;
//! * division only by terminating divisors (factors 2 and 5), so the exact
//!   rational result prints identically under f64 shortest-round-trip and
//!   Groovy's stripped `BigDecimal` quotient (`7/2`, `3/8`, `9/20` — never `1/3`);
//! * decimals appear only as standalone literals / concatenation operands, never
//!   inside `+`/`-`/`*` — Groovy's `BigDecimal` *accumulates scale* through those
//!   (`10 * 1.25 → 12.50`), which an f64 cannot reproduce.
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
    Mixed,
}

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Arith => "arith",
        Mode::Logic => "logic",
        Mode::Strings => "strings",
        Mode::Control => "control",
        Mode::Format => "format",
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

/// A dyadic decimal literal — a multiple of 1/8, exactly representable in f64, so
/// every `+`/`-`/`*` over these values is exact on both sides.
fn gen_dyadic(rng: &mut Rng) -> String {
    const VALS: &[&str] = &[
        "0.5", "0.25", "0.75", "0.125", "0.625", "1.5", "2.25", "3.75", "0.375", "1.25",
    ];
    pick(rng, VALS).to_string()
}

/// A single division whose exact result terminates (divisor factors are only 2
/// and 5), so f64 shortest-round-trip and BigDecimal print the same text.
fn gen_terminating_div(rng: &mut Rng) -> String {
    let divs = [2, 4, 5, 8, 10, 16, 20, 25, 40, 50];
    let d = *pick(rng, &divs);
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
        1 => gen_dyadic(rng),
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
        3 => gen_dyadic(rng),
        4 => gen_terminating_div(rng),
        5 => gen_string(rng, 2),
        _ => gen_int(rng, 2),
    }
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
            ],
        )
    } else {
        mode
    };
    match mode {
        Mode::Control => gen_control(&mut rng),
        _ => {
            let n = rng.range_i(1, 5) as usize;
            (0..n)
                .map(|_| {
                    let expr = match mode {
                        Mode::Arith => {
                            if rng.chance(2, 5) {
                                gen_terminating_div(&mut rng)
                            } else {
                                gen_int(&mut rng, 4)
                            }
                        }
                        Mode::Logic => gen_bool(&mut rng, 4),
                        Mode::Strings => gen_string(&mut rng, 4),
                        Mode::Format => gen_format_value(&mut rng),
                        Mode::Control | Mode::Mixed => unreachable!(),
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
    if (oracle.exit == 0) != (ours.exit == 0) {
        return true;
    }
    oracle.stdout != ours.stdout
}

fn diverges(script: &str, bin: &Path, oracle: &str, timeout: Duration) -> bool {
    let o = run_prog(Path::new(oracle), script, timeout);
    if o.timed_out {
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
                        "parity-fuzz: unknown --mode (arith|logic|strings|control|format|mixed)"
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
                     -m, --mode M         arith|logic|strings|control|format|mixed (default mixed)\n  \
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
                // Oracle-side timeout ⇒ pathological case; not a parity gap.
                if !o.timed_out && differs(&o, &r) {
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

    println!(
        "\n════════════════════════════════════════════\n\
         checked {done} cases in {:.1}s  ({} timeouts)\n\
         divergences: {}\n\
         ════════════════════════════════════════════",
        elapsed.as_secs_f64(),
        to,
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

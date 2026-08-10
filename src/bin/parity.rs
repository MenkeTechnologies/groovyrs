//! Differential parity harness (development tool): run the example corpus
//! through groovyrs and the reference `groovy`, diffing stdout. Needs `groovy`
//! on PATH, so CI never runs it. Frozen outputs live in
//! tests/data/parity_expected.txt for the no-`groovy` replay in tests/parity.rs.

use std::path::Path;
use std::process::Command;

/// Refuse an oracle whose JVM or default locale would make its answers the wrong
/// reference — the gate `parity-scripts/oracle-jvm.sh` applies to the shell
/// harnesses, which this binary did not have either.
///
/// A pre-JDK-19 JVM renders every double by the old `Double.toString`
/// algorithm, and a non-en-US locale changes case mapping — and
/// `examples/GStrings.groovy` prints a `toUpperCase()` whose answer really does
/// move with the locale. Both would report divergences on correct output.
fn gate_oracle() {
    const PROBE: &str = concat!(
        "println 1.0e23d\n",
        "println Double.MIN_VALUE\n",
        "println String.format('%,.2f', 1234.5)\n",
        "println 'hi'.toUpperCase()\n",
    );
    let out = match Command::new("groovy").arg("-e").arg(PROBE).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(e) => {
            eprintln!("parity: no reference `groovy` on PATH: {e}");
            std::process::exit(2);
        }
    };
    for (needle, why) in [
        (
            "1.0E23",
            "a JVM predating the JDK 19 Double.toString rewrite",
        ),
        (
            "4.9E-324",
            "a JVM predating the JDK 19 Double.toString rewrite",
        ),
        (
            "1,234.50",
            "a default locale that is not en-US (number format)",
        ),
        ("HI", "a default locale that is not en-US (case mapping)"),
    ] {
        if !out.lines().any(|l| l == needle) {
            eprintln!("parity: REFUSING this oracle — {why}.");
            eprintln!(
                "parity:   JAVA_HOME={}",
                std::env::var("JAVA_HOME").unwrap_or_else(|_| "<unset>".into())
            );
            eprintln!("parity:   expected a line {needle:?}; got {out:?}");
            std::process::exit(2);
        }
    }
}

fn main() {
    let dir = Path::new("examples");
    if !dir.exists() {
        eprintln!("parity: no examples/ directory (run from the crate root)");
        std::process::exit(2);
    }
    gate_oracle();
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "groovy").unwrap_or(false))
        .collect();
    files.sort();

    // Our `groovy` binary is a sibling of this harness binary.
    let ours_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("groovy")))
        .unwrap_or_else(|| Path::new("groovy").to_path_buf());

    let mut pass = 0;
    let mut fail = 0;
    for f in &files {
        let ours = Command::new(&ours_bin)
            .arg(f)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        let theirs = Command::new("groovy")
            .arg(f)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        match (ours, theirs) {
            (Some(a), Some(b)) if a == b => {
                pass += 1;
                println!("ok   {}", f.display());
            }
            (Some(a), Some(b)) => {
                fail += 1;
                println!("DIFF {}\n  ours  : {a:?}\n  groovy: {b:?}", f.display());
            }
            (None, _) => {
                fail += 1;
                println!("ERR  {} (groovyrs failed to run)", f.display());
            }
            (_, None) => {
                println!("skip {} (no groovy)", f.display());
            }
        }
    }
    println!("\nparity: {pass} passed, {fail} failed");
    // A run that compared nothing is not a pass. Both shell harnesses were
    // hardened against this ("measuring nothing is an error, not a pass") and
    // this binary was not: it always exited 0, so an empty corpus, or a `groovy`
    // that would not run, read as a clean sweep.
    if pass + fail == 0 {
        eprintln!("parity: 0 examples compared — nothing was measured");
        std::process::exit(2);
    }
    if fail > 0 {
        std::process::exit(1);
    }
}

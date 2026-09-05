//! Frozen differential parity for `java.util.Formatter` — `printf`, `sprintf`,
//! `String.format` and `CharSequence.formatted`.
//!
//! `tests/data/format_expected.txt` is a curated corpus of one-line Groovy
//! programs and the exact stdout each produces, captured from Apache Groovy
//! 5.1.0 / JVM 26. Every program is self-describing: it wraps its expression in
//! a `try`/`catch` that prints `EXC <class>: <message>`, so a record pins the
//! *throwable* a bad specifier raises as tightly as it pins a good one's text.
//! That matters here more than almost anywhere else in the frontend, because
//! the JDK's `Formatter` fails on flag/conversion combinations a hand-written
//! printf happily accepts (`%#s`, `%,x`, `%0d`, `%d` of a `BigDecimal`), and
//! *accepting* them silently is the divergence.
//!
//! Two tests share the corpus:
//!
//! * [`frozen_corpus_matches_reference_groovy`] replays it against the built
//!   `groovy` binary alone. It needs no JVM, so this is the one CI runs.
//! * [`live_reference_still_matches_the_frozen_corpus`] re-derives the
//!   expectations from a real `groovy` when one is on PATH, which is what
//!   catches a snapshot that has gone stale. It packs the whole corpus into ONE
//!   script so the JVM starts once rather than 124 times, and skips silently
//!   where no launcher exists.
//!
//! Editing the data file by hand to match a wrong groovyrs output would defeat
//! both; it is only ever regenerated from a real `groovy`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A corpus record: the program source, and the stdout it must produce.
struct Record {
    program: String,
    expected: String,
}

fn corpus() -> Vec<Record> {
    include_str!("data/format_expected.txt")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            let (program, expected) = line
                .split_once('\t')
                .unwrap_or_else(|| panic!("record {}: want `program<TAB>stdout`", i + 1));
            Record {
                program: program.to_string(),
                expected: expected.replace("\\n", "\n"),
            }
        })
        .collect()
}

/// Every program is wrapped so a throwable becomes ordinary stdout — that is
/// what lets one record describe both a value and a failure.
fn wrapped(program: &str) -> String {
    format!(
        "try {{\n{program}\n}} catch (Throwable t) \
         {{ println('EXC ' + t.getClass().getName() + ': ' + t.getMessage()) }}\n"
    )
}

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

#[test]
fn frozen_corpus_matches_reference_groovy() {
    let records = corpus();
    assert!(
        records.len() >= 100,
        "only {} format records — the corpus is truncated, so this test is no \
         longer comparing anything",
        records.len()
    );
    // A corpus with no failing records would silently stop covering the JDK's
    // whole exception surface, which is the half most likely to be wrong.
    let failing = records
        .iter()
        .filter(|r| r.expected.starts_with("EXC "))
        .count();
    assert!(
        failing >= 8,
        "the expected-THROW half of the corpus has thinned out: {failing} records"
    );

    let bin = PathBuf::from(env!("CARGO_BIN_EXE_groovy"));
    let path = scratch("groovyrs_format_one.groovy");
    let mut failures = Vec::new();
    for (i, rec) in records.iter().enumerate() {
        std::fs::write(&path, wrapped(&rec.program)).expect("write probe");
        let out = Command::new(&bin)
            .arg(&path)
            .output()
            .unwrap_or_else(|e| panic!("failed to run {}: {e}", bin.display()));
        let got = String::from_utf8_lossy(&out.stdout).into_owned();
        if got != rec.expected {
            failures.push(format!(
                "record {}: {}\n  groovy  : {:?}\n  groovyrs: {:?}{}",
                i + 1,
                rec.program,
                rec.expected,
                got,
                match String::from_utf8_lossy(&out.stderr).trim() {
                    "" => String::new(),
                    e => format!("\n  stderr  : {e}"),
                }
            ));
        }
    }
    let _ = std::fs::remove_file(&path);
    assert!(
        failures.is_empty(),
        "groovyrs diverged from frozen groovy `Formatter` output:\n{}",
        failures.join("\n")
    );
}

/// Re-derive the corpus from a live `groovy`, in one JVM start. Skips where no
/// launcher is installed, which is every CI runner that has not provisioned a
/// JDK — the frozen test above is the one that must always run.
#[test]
fn live_reference_still_matches_the_frozen_corpus() {
    let Some((launcher, java_home)) = pinned_reference() else {
        eprintln!("no `groovy` launcher on PATH — skipping the live cross-check");
        return;
    };
    let records = corpus();
    // One script, one JVM: a marker line delimits each program's output. The
    // marker starts with a newline of its own so a program that ended with a
    // bare `print` still leaves it at the start of a line.
    let mut script = String::new();
    for (i, rec) in records.iter().enumerate() {
        script.push_str(&format!("print('\\n@@@{}\\n')\n", i + 1));
        script.push_str(&wrapped(&rec.program));
    }
    script.push_str("print('\\n@@@END\\n')\n");
    let path = scratch("groovyrs_format_packed.groovy");
    std::fs::write(&path, &script).expect("write packed script");
    let mut cmd = Command::new(&launcher);
    cmd.arg(&path);
    // Run under the JVM `pinned_reference` selected, not whatever the ambient
    // environment points at.
    match &java_home {
        Some(h) => {
            cmd.env("JAVA_HOME", h);
        }
        None => {
            cmd.env_remove("JAVA_HOME");
        }
    }
    let out = cmd.output().expect("run reference groovy");
    let _ = std::fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        stdout.contains("@@@END"),
        "the reference run did not reach the end of the packed corpus — it \
         failed to compile, so nothing was compared:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut current: Option<usize> = None;
    let mut buckets: Vec<String> = vec![String::new(); records.len()];
    for line in stdout.split('\n') {
        if let Some(tag) = line.strip_prefix("@@@") {
            current = tag.parse::<usize>().ok().map(|n| n - 1);
            continue;
        }
        if let Some(i) = current.filter(|i| *i < buckets.len()) {
            buckets[i].push_str(line);
            buckets[i].push('\n');
        }
    }
    let mut stale = Vec::new();
    for (i, rec) in records.iter().enumerate() {
        // The next marker's leading newline lands in this program's bucket.
        let observed = buckets[i].strip_suffix('\n').unwrap_or(&buckets[i]);
        if observed != rec.expected {
            stale.push(format!(
                "record {}: {}\n  frozen : {:?}\n  live   : {:?}",
                i + 1,
                rec.program,
                rec.expected,
                observed
            ));
        }
    }
    assert!(
        stale.is_empty(),
        "the frozen snapshot no longer describes this `groovy` — regenerate it:\n{}",
        stale.join("\n")
    );
}

/// A `groovy` launcher plus the `JAVA_HOME` that makes it a valid reference.
///
/// The launcher is a shell script that `exec`s `$JAVA_HOME/bin/java`, so which
/// JVM answers is decided by this process's environment, not by which `groovy`
/// is on PATH. That matters because `Double.toString` was reimplemented in
/// JDK 19 (JDK-4511638) to emit the shortest round-tripping decimal: a JDK 17
/// oracle prints `9.999999999999999E22` where every JDK 19+ one prints
/// `1.0E23`, and groovyrs implements the JDK 19+ rule. An interactive shell on
/// a jenv-managed machine exports a JDK 17 home, so inheriting the ambient
/// `JAVA_HOME` is how a cross-check silently starts measuring the wrong JVM.
///
/// The corpus in `tests/data/format_expected.txt` happens to route every double
/// through `java.util.Formatter` at a fixed precision, which is version-stable
/// — but "no record depends on it today" is not a property this file enforces,
/// and one `sprintf('%s', 1.0e23d)` record would make it depend on it silently.
/// So the launcher is pinned the same way `parity-scripts/oracle-jvm.sh` pins
/// it: probe candidate homes, take the first that renders the JDK 19+ way under
/// an en-US locale, and report which one answered.
///
/// Returns `None` only when no `groovy` runs at all (an unprovisioned CI
/// runner). A `groovy` that runs but has no conforming JVM is a hard failure —
/// silently comparing against a stale renderer is the outcome this prevents.
fn pinned_reference() -> Option<(PathBuf, Option<String>)> {
    let launcher = {
        let named = std::env::var("GROOVY_REFERENCE").ok();
        let p = PathBuf::from(named.unwrap_or_else(|| "groovy".into()));
        Command::new(&p)
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        p
    };

    // Candidate homes, in preference order. An explicit
    // GROOVYRS_PARITY_JAVA_HOME is honoured ALONE: naming a JVM means measure
    // that one or refuse, never substitute another.
    let mut cands: Vec<Option<String>> = Vec::new();
    if let Ok(pinned) = std::env::var("GROOVYRS_PARITY_JAVA_HOME") {
        cands.push(Some(pinned));
    } else {
        if let Ok(ambient) = std::env::var("JAVA_HOME") {
            cands.push(Some(ambient));
        }
        cands.push(None);
        for h in [
            "/opt/homebrew/opt/openjdk",
            "/opt/homebrew/opt/openjdk@25",
            "/opt/homebrew/opt/openjdk@21",
            "/usr/local/opt/openjdk",
            "/usr/lib/jvm/default-java",
        ] {
            if Path::new(h).is_dir() {
                cands.push(Some(h.to_string()));
            }
        }
    }

    // Two JVM probes and two locale probes. A JVM that answers one by accident
    // still has to answer the other; the locale pair catches the second
    // contamination axis a version string cannot show (`%,.2f` is `1.234,50`
    // under de-DE, `"hi".toUpperCase()` is `Hİ` under tr-TR).
    const PROBE: &str = "println 1.0e23d\nprintln Double.MIN_VALUE\n\
         println String.format('%,.2f', 1234.5)\nprintln 'hi'.toUpperCase()\n";
    let probe_path = scratch("groovyrs_format_jvmprobe.groovy");
    std::fs::write(&probe_path, PROBE).expect("write jvm probe");

    let mut tried = Vec::new();
    for cand in cands {
        let mut cmd = Command::new(&launcher);
        cmd.arg(&probe_path);
        match &cand {
            Some(h) => {
                cmd.env("JAVA_HOME", h);
            }
            None => {
                cmd.env_remove("JAVA_HOME");
            }
        }
        let out = cmd.output().expect("run jvm probe");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let ok = text.contains("1.0E23")
            && text.contains("4.9E-324")
            && text.contains("1,234.50")
            && text.contains("HI");
        if ok {
            let mut ver = Command::new(&launcher);
            ver.arg("--version");
            match &cand {
                Some(h) => {
                    ver.env("JAVA_HOME", h);
                }
                None => {
                    ver.env_remove("JAVA_HOME");
                }
            }
            let banner = ver
                .output()
                .ok()
                .map(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .next()
                        .unwrap_or("")
                        .to_string()
                })
                .unwrap_or_default();
            eprintln!(
                "format parity oracle: {} | {banner} | JAVA_HOME={} (pinned)",
                launcher.display(),
                cand.as_deref().unwrap_or("<unset>")
            );
            let _ = std::fs::remove_file(&probe_path);
            return Some((launcher, cand));
        }
        tried.push(format!(
            "  JAVA_HOME={} → {}",
            cand.as_deref().unwrap_or("<unset>"),
            text.replace('\n', " | ")
        ));
    }
    let _ = std::fs::remove_file(&probe_path);
    panic!(
        "`{}` runs but no candidate JVM renders doubles the JDK 19+ way under an \
         en-US locale, so the live cross-check would compare against the wrong \
         reference (JDK 17 prints 9.999999999999999E22 for 1.0e23d). Candidates \
         tried:\n{}\nSet GROOVYRS_PARITY_JAVA_HOME to a JDK 19+ home.",
        launcher.display(),
        tried.join("\n")
    );
}

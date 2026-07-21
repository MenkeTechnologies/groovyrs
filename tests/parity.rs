//! CI-safe parity replay: run groovyrs over every `examples/*.groovy` and assert
//! its stdout matches the FROZEN reference output captured from Apache Groovy.
//!
//! Unlike the `parity` binary (which shells out to a live `groovy`), this test
//! needs no Groovy or JVM installed — it compares against
//! tests/data/parity_expected.txt, a snapshot regenerated only by running system
//! `groovy` over the corpus. Editing that file by hand to match a wrong groovyrs
//! output would defeat its purpose; it must always be regenerated from real
//! `groovy`.
//!
//! Snapshot format: for each example (sorted by filename), a header line
//! `==== <basename> ====` followed by that program's exact stdout bytes.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The compiled `groovy` binary under test (`CARGO_BIN_EXE_groovy` is set by
/// cargo for integration tests of a crate that declares the bin).
fn groovy_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_groovy"))
}

/// Sorted list of `examples/*.groovy`.
fn example_files() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("examples/ dir exists")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "groovy").unwrap_or(false))
        .collect();
    files.sort();
    files
}

/// Parse the frozen snapshot into (basename, stdout) records.
fn parse_expected(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("==== ") {
            let name = rest.trim_end_matches(" ====").to_string();
            out.push((name, String::new()));
        } else if let Some(last) = out.last_mut() {
            last.1.push_str(line);
            last.1.push('\n');
        }
    }
    out
}

#[test]
fn examples_match_frozen_groovy_output() {
    let expected_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/parity_expected.txt");
    let expected_text = std::fs::read_to_string(&expected_path).unwrap_or_else(|e| {
        panic!(
            "missing frozen snapshot {}: {e}\n\
             regenerate with system `groovy` over examples/*.groovy",
            expected_path.display()
        )
    });
    let expected = parse_expected(&expected_text);
    let bin = groovy_bin();
    let files = example_files();

    assert_eq!(
        files.len(),
        expected.len(),
        "example count ({}) != frozen record count ({}); regenerate the snapshot",
        files.len(),
        expected.len()
    );

    let mut failures = Vec::new();
    for (f, (exp_name, exp_out)) in files.iter().zip(expected.iter()) {
        let base = f.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(&base, exp_name, "snapshot ordering mismatch");

        let out = Command::new(&bin)
            .arg(f)
            .output()
            .unwrap_or_else(|e| panic!("failed to run {}: {e}", bin.display()));
        let got = String::from_utf8_lossy(&out.stdout).into_owned();
        if &got != exp_out {
            failures.push(format!(
                "DIFF {base}\n  frozen groovy: {exp_out:?}\n  groovyrs     : {got:?}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "groovyrs diverged from frozen groovy output:\n{}",
        failures.join("\n")
    );
}

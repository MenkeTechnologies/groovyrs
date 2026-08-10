//! Integration tests for inline Rust FFI: a `rust { ... }` block compiles to a
//! cached cdylib whose exported `pub extern "C" fn`s become callable by name
//! from Groovy. Each test runs a real end-to-end compile+call through the built
//! `groovy` binary (the first run invokes `rustc`; a per-body hash caches the
//! dylib under a private, throwaway `FUSEVM_FFI_DIR` so the suite is hermetic).
//!
//! Requires a `rustc`. These tests used to `return` early when they could not
//! find one — reporting PASS having executed no assertion at all, so an FFI
//! bridge that stopped working entirely would still show green on any machine
//! whose PATH had drifted. A test that cannot run is not a test that passed:
//! [`rustc_command`] now fails loudly instead, and it resolves the compiler the
//! way cargo does (`$RUSTC` first, then PATH), so the only way to reach that
//! failure is a genuinely absent toolchain — which cannot be the case in a
//! process `cargo test` started.

use std::process::Command;

/// Run a Groovy source string through the `groovy` binary and return
/// (stdout, stderr, ok). Points the FFI dylib cache at a fresh temp dir so a run
/// never reuses (or pollutes) the developer's `~/.cache/fusevm/ffi`.
fn run(src: &str, cache: &std::path::Path) -> (String, String, bool) {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("groovyrs_ffi_{}.groovy", fasthash(src)));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_groovy"))
        .arg(&path)
        .env("FUSEVM_FFI_DIR", cache)
        .output()
        .expect("spawn groovy");
    let _ = std::fs::remove_file(&path);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

fn fasthash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// A unique cache dir per test so parallel tests never share a lock/dylib.
fn cache_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("groovyrs_ffi_cache_{tag}_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

/// The `rustc` a `rust { … }` block will compile with, resolved the way cargo
/// resolves it: `$RUSTC` when set (a rustup shim need not be on PATH), else
/// `rustc` from PATH. Panics rather than answering `None`, because a test that
/// skips itself here reports a pass having measured nothing.
fn rustc_command() -> String {
    let cmd = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let ok = Command::new(&cmd)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(
        ok,
        "no working rustc at `{cmd}` — a `rust {{ … }}` block cannot compile, so \
         this test can measure nothing. It used to return here and report a \
         pass. Set RUSTC, or put rustc on PATH."
    );
    cmd
}

#[test]
fn rust_block_export_is_callable_and_returns_the_right_value() {
    rustc_command();
    let cache = cache_dir("triple");
    let src = "\
rust {
    pub extern \"C\" fn g_triple(x: i64) -> i64 { x * 3 }
}
println(g_triple(14))
";
    let (out, err, ok) = run(src, &cache);
    let _ = std::fs::remove_dir_all(&cache);
    assert!(ok, "run failed: stderr={err}");
    assert_eq!(out, "42\n", "stderr={err}");
}

#[test]
fn multiple_exports_with_multiple_args() {
    rustc_command();
    let cache = cache_dir("add");
    let src = "\
rust {
    pub extern \"C\" fn g_add(a: i64, b: i64) -> i64 { a + b }
    pub extern \"C\" fn g_sq(x: i64) -> i64 { x * x }
}
println(g_add(40, 2))
println(g_sq(9))
";
    let (out, err, ok) = run(src, &cache);
    let _ = std::fs::remove_dir_all(&cache);
    assert!(ok, "run failed: stderr={err}");
    assert_eq!(out, "42\n81\n", "stderr={err}");
}

#[test]
fn unknown_call_without_a_rust_block_is_an_unresolved_reference() {
    // Gating check: with NO `rust { ... }` block present, an unknown callee must
    // stay a compile-time unresolved-reference error, not silently route to FFI.
    let cache = cache_dir("noffi");
    let (out, err, ok) = run("println(nope(1))\n", &cache);
    let _ = std::fs::remove_dir_all(&cache);
    assert!(!ok, "expected failure, got stdout={out:?}");
    assert!(
        err.contains("unresolved reference: nope"),
        "unexpected stderr: {err}"
    );
}

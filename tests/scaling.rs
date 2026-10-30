//! Operations that must cost O(1) in the size of the collection they touch.
//!
//! A per-operation copy of a whole collection does not change any answer, so
//! no output-comparing test can see it; it only turns filling a collection
//! quadratic. Each test here fills one far enough that the quadratic form takes
//! minutes on a debug build while the linear form takes a few seconds, and
//! bounds the run well above the latter.

use std::process::Command;
use std::time::{Duration, Instant};

/// Run `src` through the `groovy` binary; answer its stdout and wall time.
fn timed(name: &str, src: &str) -> (String, Duration) {
    let path = std::env::temp_dir().join(format!(
        "groovyrs_scaling_{name}_{}.groovy",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let start = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_groovy"))
        .arg(&path)
        .output()
        .expect("spawn groovy");
    let took = start.elapsed();
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "program failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (String::from_utf8_lossy(&out.stdout).into_owned(), took)
}

/// The bound every program here must finish inside. The linear form of each
/// takes a few seconds on a debug build; the quadratic one took minutes.
const BOUND: Duration = Duration::from_secs(20);

/// Filling a map through every write and keyed-read spelling, for each map
/// implementation. `m[k] = v` asked whether the receiver was a map by copying
/// its entries (and, for a `HashMap`, re-bucketing them), and every map method
/// call did the same before dispatching, so 20 000 `m[i] = i` took 11 s of
/// debug-build CPU and doubling the count quadrupled it.
#[test]
fn filling_a_map_is_linear_in_its_size() {
    let src = r#"
def total = 0
for (m in [[:], new HashMap(), new TreeMap()]) {
    for (int i = 0; i < 40000; i++) {
        m[i] = i
        m.put("k" + i, i)
        if (m.get(i) != i || !m.containsKey("k" + i) || m[i] != i) { println "lost $i" }
    }
    total += m.size()
}
println total
"#;
    let (out, took) = timed("map_fill", src);
    assert_eq!(out, "240000\n");
    assert!(took < BOUND, "filling took {took:?}");
}

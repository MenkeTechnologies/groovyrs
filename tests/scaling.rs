//! Operations that must cost O(1) in the size of the collection they touch.
//!
//! A per-operation copy of a whole collection does not change any answer, so
//! no output-comparing test can see it; it only turns filling a collection
//! quadratic. Each test here fills one far enough that the quadratic form takes
//! minutes on a debug build while the linear form takes a few seconds, and
//! bounds the run well above the latter. The bound is on the child's CPU time,
//! not wall time, so a loaded machine running other jobs does not fail it.

use std::process::Command;
use std::time::Duration;

/// User plus system CPU time consumed so far by this process's waited-for
/// children.
fn children_cpu() -> Duration {
    // SAFETY: `getrusage` only writes the `rusage` it is handed.
    let usage = unsafe {
        let mut u: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_CHILDREN, &mut u);
        u
    };
    let tv = |t: libc::timeval| {
        Duration::from_secs(t.tv_sec as u64) + Duration::from_micros(t.tv_usec as u64)
    };
    tv(usage.ru_utime) + tv(usage.ru_stime)
}

/// Run `src` through the `groovy` binary; answer its stdout and CPU time.
fn timed(name: &str, src: &str) -> (String, Duration) {
    let path = std::env::temp_dir().join(format!(
        "groovyrs_scaling_{name}_{}.groovy",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let start = children_cpu();
    let out = Command::new(env!("CARGO_BIN_EXE_groovy"))
        .arg(&path)
        .output()
        .expect("spawn groovy");
    let took = children_cpu() - start;
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "program failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (String::from_utf8_lossy(&out.stdout).into_owned(), took)
}

/// The CPU time every program here must finish inside. The linear form of each
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

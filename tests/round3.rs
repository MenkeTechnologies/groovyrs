//! Round-3 parity regressions: each program's expected stdout was captured from
//! Apache Groovy 6.0.0 and frozen here, so the suite needs no JVM. A divergence
//! in one of these is a groovyrs bug; regenerate an expectation only by running
//! the program under a real `groovy`.

use std::process::Command;

/// Run a Groovy source string through the `groovy` binary and return
/// (stdout, ok).
fn run(src: &str) -> (String, bool) {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("groovyrs_round3_{}.groovy", fasthash(src)));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_groovy"))
        .arg(&path)
        .output()
        .expect("spawn groovy");
    let _ = std::fs::remove_file(&path);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

fn fasthash(s: &str) -> u64 {
    // FNV-1a, so concurrent tests use distinct temp files.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Assert that `src` runs and prints exactly `expected`.
fn check(src: &str, expected: &str) {
    let (out, ok) = run(src);
    assert!(ok, "program failed; stdout so far:\n{out}");
    assert_eq!(out, expected);
}

#[test]
fn string_subsequence_and_map_tosorted_closure() {
    let src = r####"println('abc'.subSequence(1, 3))
println('hello'.subSequence(0, 2).class.simpleName)
def m = [b: 1, a: 2, c: 0]
println(m.toSorted { it.key })
println(m.toSorted { x, y -> x.value <=> y.value })
println(m.toSorted { it.value }.getClass().name)
println(m.toSorted())
println([a: 3, b: 1.5, c: 2].toSorted())
println([z: 'b', y: 'a'].toSorted())
"####;
    let expected = r####"bc
String
[a:2, b:1, c:0]
[c:0, b:1, a:2]
java.util.LinkedHashMap
[c:0, b:1, a:2]
[b:1.5, c:2, a:3]
[y:a, z:b]
"####;
    check(src, expected);
}

#[test]
fn default_arguments_combined_with_variadic_parameter() {
    let src = r####"def f(int a, String b = 'x', Object... rest) { "$a $b $rest" }
println(f(1))
println(f(1, 'y'))
println(f(1, 'y', 3, 4))
def g(String... s) { s.length }
println g()
println g('a')
def h(a = 5, int... r) { "$a $r" }
println h()
println h(1)
println h(1, 2, 3)
"####;
    let expected = r####"1 x []
1 y []
1 y [3, 4]
0
1
5 []
1 []
1 [2, 3]
"####;
    check(src, expected);
}

#[test]
fn generic_arguments_after_new_are_erased() {
    let src = r####"def s = new ArrayList<>(); s << 1; println s
def m = new HashMap<String, List<Integer>>(); m.a = [1]; println m
class Box<T> { List<T> items = []; void push(T t) { items << t }; T pop() { items.remove(items.size() - 1) } }
def k = new Box<Integer>()
k.push(4); println(k.pop())
def t = new TreeMap<String, Map<String, Integer>>(); t.b = [x: 1]; println t
println(new ArrayList<Integer>([3, 1]).sort())
"####;
    let expected = r####"[1]
[a:[1]]
4
[b:[x:1]]
[1, 3]
"####;
    check(src, expected);
}

#[test]
fn closure_body_starting_with_assignment_is_not_a_default_parameter() {
    let src = r####"def m = [x: 1]
m.with { x = 5 }
println(m)
def v = 0
def c = { v = 9 }
c()
println(v)
def d = { a = 3 -> a * 2 }
println(d())
println(d(4))
def e = { a, b = [1, 2] -> a + b }
println(e([0]))
"####;
    let expected = r####"[x:5]
9
6
8
[0, 1, 2]
"####;
    check(src, expected);
}

//! Round-11 parity regressions: each program's expected stdout was captured from
//! Apache Groovy 6.0.0 and frozen here, so the suite needs no JVM. A divergence
//! in one of these is a groovyrs bug; regenerate an expectation only by running
//! the program under a real `groovy`.

use std::process::Command;

/// Run a Groovy source string through the `groovy` binary and return
/// (stdout, ok).
fn run(src: &str) -> (String, bool) {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("groovyrs_round11_{}.groovy", fasthash(src)));
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
fn untyped_parameter_keeps_long_kind_under_bitwise_ops() {
    let src = r####"def f0(x) { [x & 1, x | 2, x ^ 3, x << 1, x >> 1, x >>> 1, ~x].collect { it.getClass().simpleName } }
println f0(5L)
println f0(5)
def g(x) { def y = x & 0xFF; y.getClass().simpleName }
println g(300L)
def h(x, y) { (x & y).getClass().simpleName + (x << y).getClass().simpleName }
println h(6L, 2)
println h(6, 2L)
def k(x) { x & 1 }
println k(3L).getClass().name
def r(x) { x = x | 1; x.getClass().name }
println r(4L)
class C { def m(x) { [x & 1, x | 2, x ^ 3, x >> 1].collect { it.getClass().simpleName } }
  static sm(x) { (x & 3).getClass().simpleName }
  C(x) { println((x ^ 1).getClass().simpleName) } }
println new C(7L).m(5L)
println C.sm(9L)
def f(Long x) { (x & 1).getClass().simpleName }
println f(4L)
def gn(Number x) { (x & 1).getClass().simpleName }
println gn(4L)
def ho(Object x) { (x & 1).getClass().simpleName }
println ho(4L)
def b(x) { def s = 0; for (i in 0..<3) { s = s | (x >> i) }; s.getClass().simpleName }
println b(7L)
def sh(Short x) { (x & 1).getClass().simpleName }
println sh(4 as Short)
def u(x) { (x & 1).getClass().simpleName }
println u(4 as Short)
println u(4 as Byte)
println u(4G)
"####;
    let expected = r####"[Long, Long, Long, Long, Long, Long, Long]
[Integer, Integer, Integer, Integer, Integer, Integer, Integer]
Long
LongLong
LongInteger
java.lang.Long
java.lang.Long
Long
[Long, Long, Long, Long]
Long
Long
Long
Long
Long
Integer
Integer
Integer
BigInteger
"####;
    check(src, expected);
}

#[test]
fn long_kind_through_function_method_and_closure_returns() {
    let src = r####"long f(long y) { y + 1 }
println f(4).getClass().simpleName
println([f(4)][0].getClass().simpleName)
def g(long y) { y * 2 }
println g(3).getClass().simpleName
def h(long y) { return y }
println h(3).getClass().simpleName
class C { def m(long y) { y * 2 }; long n(int a) { a } ; static long s(x) { x } }
println new C().m(3).getClass().simpleName
println new C().n(3).getClass().simpleName
println C.s(3).getClass().simpleName
def c = { long y -> y * 2 }
println c(3).getClass().simpleName
def d = { long y -> def t = y; t }
println d(3).getClass().simpleName
long fib(long n) { n < 2 ? n : fib(n - 1) + fib(n - 2) }
println fib(20)
println fib(20).getClass().simpleName
def k() { long z = 5; z }
println k().getClass().simpleName
def w() { long z = 5; z = z + 1 }
println w().getClass().simpleName
int i2(long q) { q }
println i2(3).getClass().simpleName
Long boxedRet() { 7 }
println boxedRet().getClass().simpleName
"####;
    let expected = r####"Long
Long
Long
Long
Long
Long
Long
Long
Long
6765
Long
Long
Long
Integer
Long
"####;
    check(src, expected);
}

#[test]
fn switch_on_jdk_class_matches_jdk_interface_labels() {
    let src = r####"def t0(c) {
  switch (c) {
    case CharSequence: return "cs"
    case Comparable: return "cmp"
    case Number: return "num"
    case Collection: return "coll"
    case Map: return "map"
    case Iterable: return "iter"
    case Serializable: return "ser"
    default: return "other"
  }
}
[String, Integer, Long, Object, Number, StringBuilder, List, Set, BigDecimal, Boolean, Date, Map, Character, Math].each { println "${it.simpleName} ${t0(it)}" }
println String.isCase(String)
println CharSequence.isCase(String)
println CharSequence.isAssignableFrom(String)
println Comparable.isAssignableFrom(Integer)
println Number.isAssignableFrom(Integer)
println List.isAssignableFrom(ArrayList)
println Object.isAssignableFrom(List)
println Iterable.isAssignableFrom(Set)
println(String in CharSequence)
println(switch (Integer) { case Comparable -> 'c'; default -> 'd' })
class D implements Comparator { int compare(a, b) { 0 } }
class E extends D {}
interface Shape {}
class Sq implements Shape, Runnable { void run() {} }
def k(c) { switch (c) { case Comparator: return 'cmp'; case Runnable: return 'run'; case Shape: return 'shape'; default: return 'x' } }
[D, E, Sq, String].each { println "${it.simpleName} ${k(it)}" }
println Comparator.isAssignableFrom(E)
println Runnable.isAssignableFrom(Sq)
println Comparable.isAssignableFrom(Sq)
println CharSequence
println Comparable.name
println Iterable.isInterface()
println AbstractList.isInterface()
println(ArrayList in List)
println(List in ArrayList)
println(HashMap in Map)
println(Integer in Comparable)
println(Integer in CharSequence)
def v = 'str'
println(switch (v.getClass()) { case Comparable -> 'yes'; default -> 'no' })
println([String, Integer, ArrayList].findAll { Comparable.isAssignableFrom(it) }*.simpleName)
println Number.isAssignableFrom(Comparable)
println Object.isAssignableFrom(Comparable)
"####;
    let expected = r####"String cs
Integer cmp
Long cmp
Object other
Number num
StringBuilder cs
List coll
Set coll
BigDecimal cmp
Boolean cmp
Date cmp
Map map
Character cmp
Math other
true
true
true
true
true
true
true
true
true
c
D cmp
E cmp
Sq run
String x
true
true
false
interface java.lang.CharSequence
java.lang.Comparable
true
false
true
false
true
true
false
yes
[String, Integer]
false
true
"####;
    check(src, expected);
}

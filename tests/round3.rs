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

#[test]
fn java_array_subscript_past_the_end_raises() {
    let src = r####"def a = new int[3]
a[0] = 1; a[2] += 5; a[1]++
println(a)
try { a[3] } catch (e) { println e.class.simpleName + ' ' + e.message }
try { a[3] = 1 } catch (e) { println e.message }
try { a[3]++ } catch (e) { println e.message }
try { a.getAt(7) } catch (e) { println e.message }
println(a[-1])
def s = 'a b'.split()
try { s[2] } catch (e) { println e.message }
def g = new int[2][2]
g[1][1] = 4
try { g[2][0] } catch (e) { println e.message }
def l = [1, 2]
println(l[5])
l[4] = 9
println(l)
"####;
    let expected = r####"[1, 1, 5]
ArrayIndexOutOfBoundsException Index 3 out of bounds for length 3
Index 3 out of bounds for length 3
Index 3 out of bounds for length 3
Index 7 out of bounds for length 3
5
Index 2 out of bounds for length 2
Index 2 out of bounds for length 2
null
[1, 2, null, null, 9]
"####;
    check(src, expected);
}

#[test]
fn assignment_in_value_position_yields_the_stored_value() {
    let src = r####"def x; (x = 7); println x
println((x = 8))
def y = (x = 4) + 1; println y
def z = null; z ?: (z = 7); println z
if ((x = 3) > 2) println x
def a, b; a = b = 4; println a + b
def i = 0; while ((i += 1) < 3) {}; println i
def l = []; def n; while ((n = l.size()) < 2) l << n; println l
def it2 = [1, 2, 3].iterator(); def e
while (it2.hasNext() && (e = it2.next()) != 3) print e
println()
def m = [:]
println(m.k = 5)
println(m)
class K { int v; def set(n) { return (v = n) * 2 }; def bump() { (v += 1) } }
def k = new K(); println k.set(4); println k.v; println k.bump()
def arr = [0, 0]
println(arr[1] = 9)
println(arr)
String line
def lines = ['p', 'q'].iterator()
while ((line = lines.hasNext() ? lines.next() : null) != null) println line
if (x = 0) println 'no' else println 'falsy'
"####;
    let expected = r####"7
8
5
7
3
8
3
[0, 1]
12
5
[k:5]
8
4
5
9
[0, 9]
p
q
falsy
"####;
    check(src, expected);
}

#[test]
fn bitwise_operators_on_booleans_answer_booleans() {
    let src = r####"def t = true, f = false
println(t ^ f)
println(t & f)
println(t | f)
println((t & t).class.name)
boolean b = true
b &= false
println b
b |= true
println b
b ^= true
println b
println(true.and(false))
println(true.or(false))
println(true.xor(true))
println(!true ^ true)
println(5 > 3 & 2 > 1)
println(5 > 3 | 2 > 9)
println(5 > 3 ^ 2 > 1)
println(6 & 3)
println(6 | 3)
println(6 ^ 3)
"####;
    let expected = r####"true
false
true
java.lang.Boolean
false
true
false
false
true
false
true
true
true
false
2
7
5
"####;
    check(src, expected);
}

#[test]
fn system_out_print_stream_and_receiver_command_calls() {
    let src = r####"System.out.println([1, 2])
System.out.println(null)
System.out.println("a${1}")
System.out.print(1.5)
System.out.println()
System.out.format("%05d|%s%n", 42, [1])
System.out.printf("%d%n", 7)
println(System.out.getClass().name)
def out = System.out
out.println 'cmd'
System.out.append('x').append('y')
System.out.println()
System.out << 'a' << 1 << "\n"
def sb = new StringBuilder()
sb.append 'x'
sb.append 1
println sb
def l = []
l.add 3
l.add 'q'
println l
def m = [:]
m.put 'k', 'v'
println m
l.each { out.println it }
"####;
    let expected = r####"[1, 2]
null
a1
1.5
00042|[1]
7
java.io.PrintStream
cmd
xy
a1
x1
[3, q]
[k:v]
3
q
"####;
    check(src, expected);
}

#[test]
fn destructuring_a_short_array_pads_with_null() {
    let src = r####"def c, d, e
(c, d, e) = 'p q'.split(' ')
println "$c $d $e"
def (x, y) = [1]
println "$x $y"
def (p, q) = 'ab'
println "$p $q"
(x, y) = [y, x]
println "$x $y"
"####;
    let expected = r####"p q null
1 null
a b
null 1
"####;
    check(src, expected);
}

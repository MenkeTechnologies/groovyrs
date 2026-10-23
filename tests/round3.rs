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

#[test]
fn ast_transform_immutable_singleton_delegate_and_member_selection() {
    let src = r####"import groovy.transform.*
@Immutable class V { int x; String s }
def v = new V(1, 'a')
println v
println v == new V(x: 1, s: 'a')
try { v.x = 5 } catch (ReadOnlyPropertyException e) { println e.message }
println v.x
@Singleton class Sg { def n = 3 }
println Sg.instance.n
println Sg.instance.is(Sg.instance)
@EqualsAndHashCode(includes='a') class EH { int a; int b }
println new EH(a: 1, b: 2) == new EH(a: 1, b: 3)
println new EH(a: 1, b: 2).hashCode() == new EH(a: 1, b: 9).hashCode()
@EqualsAndHashCode(excludes=['b']) class EX { int a; int b }
println new EX(a: 1, b: 2) == new EX(a: 1, b: 3)
@ToString(excludes='b') class TS { int a; int b; int c }
println new TS(a: 1, b: 2, c: 3)
@ToString(includes='b,c') class TI { int a; int b; int c }
println new TI(a: 1, b: 2, c: 3)
class Z { @Delegate List l = [1, 2] }
println new Z().size()
println new Z().contains(2)
"####;
    let expected = r####"V(1, a)
true
Cannot set read-only property: x for class: V
1
3
true
true
true
true
TS(1, 3)
TI(2, 3)
2
true
"####;
    check(src, expected);
}

#[test]
fn bare_command_expressions_with_a_literal_argument() {
    let src = r####"def f(a, b = 'z') { println "$a$b" }
f 1, 2
f 'x'
def c = { x -> println "c$x" }
c 5
printf "%05d|%s\n", 42, 'q'
def g(Map m, x) { println "${m.k}$x" }
g 7, k: 1
class K {
    static void s(x) { println "s$x" }
    def run() { s 3 }
}
new K().run()
[1].each { f 'e', it }
def sb = new StringBuilder()
sb.append 'a'
println sb
"####;
    let expected = r####"12
xz
c5
00042|q
17
s3
e1
a
"####;
    check(src, expected);
}

#[test]
fn bytes_decode_and_base64_hex_encodings() {
    let src = r####"println new String([104, 105] as byte[])
println new String('héllo'.getBytes('UTF-8'), 'UTF-8')
println new String([104, -23] as byte[], 'ISO-8859-1')
println new String(['a', 'b'] as char[])
println new String('aGVsbG8='.decodeBase64())
println new String('aGVs bG8'.decodeBase64())
println 'aGVsbG8gd29ybGQ=\n'.decodeBase64().length
println new String('aGVsbA'.decodeBase64())
println '[' + ''.bytes.encodeBase64() + ']'
println 'a'.bytes.encodeBase64()
println 'ab'.bytes.encodeBase64()
println 'abc'.bytes.encodeBase64()
println 'héllo wörld!'.bytes.encodeBase64().toString()
println 'hi'.bytes.encodeHex()
println(([-1, 0, 16] as byte[]).encodeHex().toString())
try { 'a$b'.decodeBase64() } catch (RuntimeException e) { println e.message }
"####;
    let expected = r####"hi
héllo
hé
ab
hello
hello
11
hell
[]
YQ==
YWI=
YWJj
aMOpbGxvIHfDtnJsZCE=
6869
ff0010
bad character in base64 value
"####;
    check(src, expected);
}

#[test]
fn closure_declared_as_a_script_interface_is_coerced() {
    let src = r####"interface I { int calc(int x) }
I b = { x -> x * 5 }
println b.calc(4)
println(b instanceof I)
abstract class AC {
    abstract String nm()
    String hi() { "hi " + nm() }
}
AC ac = { -> 'zed' }
println ac.hi()
def plain = { 3 }
println plain()
"####;
    let expected = r####"20
true
hi zed
3
"####;
    check(src, expected);
}

#[test]
fn increment_and_decrement_call_next_and_previous() {
    let src = r####"def s = 'Az'
s++
println s
def t = s--
println t + s
def z = 'a'
println(++z)
class R {
    int lo
    def next() { new R(lo: lo + 1) }
    def previous() { new R(lo: lo - 1) }
    String toString() { "R$lo" }
}
def r = new R(lo: 1)
r++
println r
--r
println r
def q = r++
println "$q $r"
class C {
    def s = 'a'
    def bump() { s++; s }
}
println new C().bump()
def c = new C()
c.s++
println c.s
def l = ['x']
l[0]++
println l
def m = [k: 'p']
m.k--
println m
class N { int v }
def n = new N(v: 1)
try { n++ } catch (MissingMethodException e) { println e.message.readLines()[0] }
println n.v
def u = null
try { u++ } catch (NullPointerException e) { println e.message }
def b = 2.5G
b++
println b
int i = 0
for (int k = 0; k < 5; k++) i++
println i
"####;
    let expected = r####"A{
A{Az
b
R2
R1
R1 R2
b
b
[y]
[k:o]
No signature of method: next for class: N is applicable for argument types: () values: []
1
Cannot invoke method next() on null object
3.5
5
"####;
    check(src, expected);
}

#[test]
fn methods_overloaded_by_parameter_type() {
    let src = r####"class A {
    def m(int x) { 'int' }
    def m(String s) { 'str' }
    def m(x) { 'obj' }
    def n(Number x) { 'num' }
    def n(Integer x) { 'integer' }
    def k(List l) { 'list' }
    def k(Map m) { 'map' }
    def two(String a, int b) { 'si' }
    def two(int a, String b) { 'is' }
    static s(String x) { 'sstr' }
    static s(Integer x) { 'sint' }
    def self() { [m(1), m('a'), m(null)] }
}
def a = new A()
println a.m(1); println a.m('s'); println a.m(1.5); println a.m([1]); println a.m("g${1}")
println a.n(1); println a.n(1.5)
println a.k([1]); println a.k([a: 1])
println a.two('x', 1); println a.two(1, 'x')
println A.s('q'); println A.s(3)
println a.self()
class B extends A { def m(String s) { 'B.str' } }
println new B().m('z'); println new B().m(2)
"####;
    let expected = r####"int
str
obj
obj
str
integer
num
list
map
si
is
sstr
sint
[int, str, obj]
B.str
int
"####;
    check(src, expected);
}

#[test]
fn switch_as_the_last_statement_is_the_return_value() {
    let src = r####"def g1(s) { switch (s) { case 1: 'one'; break; case 2: 'two'; break; default: 'other' } }
println g1(1); println g1(2); println g1(3)
def g2(s) { switch (s) { case 1: 'one' } }
println g2(1); println g2(5)
def g4(s) { switch (s) { case 1: 'a'; case 2: 'b'; break; default: 'c' } }
println g4(1)
def c = { s -> switch (s) { case 1: 'one'; break; default: 'dflt' } }
println c(1); println c(9)
def g6(s) { switch (s) { case 1: def x = 5; x + 1; break; default: 0 } }
println g6(1)
def g8(s) { switch (s) { case 1: println 'side'; break } }
println g8(1)
def a2(s) { if (s > 0) { switch (s) { case 1: 'one'; break; default: 'pos' } } else { 'neg' } }
println a2(1); println a2(4); println a2(-1)
def a3(s) { switch (s) { case 1: case 2: 'low'; break; default: if (s > 5) { 'hi' } else { 'mid' } } }
println a3(2); println a3(7); println a3(4)
def a4(s) { switch (s) { case 1: 'x'; break; case 2: 'y' } }
println a4(2); println a4(3)
def a1(s) { switch (s) { case 1 -> 'one'; default -> 'd' } }
println a1(1); println a1(2)
"####;
    let expected = r####"one
two
other
one
null
b
one
dflt
6
side
null
one
pos
neg
low
hi
mid
y
null
one
d
"####;
    check(src, expected);
}

#[test]
fn user_method_mutating_a_field_list_keeps_the_instance() {
    let src = r####"class P {
    def items = []
    def add(x) { items << x; 7 }
    def push(x) { items.add(x) }
    def clear() { items.clear() }
    def leftShift(x) { items << x; this }
}
def p = new P()
p.add(1)
println p.items
p.push(2)
println p.items
p << 3 << 4
println p.items
p.clear()
println p.items.size()
println p.getClass().name
"####;
    let expected = r####"[1]
[1, 2]
[1, 2, 3, 4]
0
P
"####;
    check(src, expected);
}

#[test]
fn switch_default_before_a_case_is_a_compile_error() {
    // Groovy rejects the program before running any of it: "A default branch
    // must appear as the last branch of a switch".
    let (out, ok) =
        run("println 'before'\nswitch (1) { default: println 'd'; case 1: println 'one' }\n");
    assert!(!ok);
    assert_eq!(out, "");
}

#[test]
fn incdec_on_property_and_subscript_targets_in_expression_position() {
    // `p.x++` yields the value before the update, `++p.x` the one after, on a
    // property, a direct field, a dynamic name and a subscript; a `String`
    // operand steps through `next()`.
    let src = r####"class A { int x = 1 }
def a = new A()
def y = a.x++
println "$y ${a.x}"
println(--a.x)
println(a.x++ + 10)
println((a.x--) * 3)
println a.x
def fld = a.@x++
println "$fld ${a.@x}"
def p = 'x'
def dyn = a."$p"--
println "$dyn ${a.x}"
def l = [1, 2, 3]
def e = l[1]++
println "$e $l"
println(++l[2])
def m = [k: 5, s: 'a']
println "${m.k++} ${m.k} ${m.s++} ${m.s}"
println(--m.k)
println m
def counts = [:].withDefault { 0 }
['a', 'b', 'a'].each { counts[it]++ }
println counts
assert a.x++ == 1
println a.x
"####;
    let expected = r####"1 2
1
11
6
1
1 2
2 1
2 [1, 3, 3]
4
5 6 a b
5
[k:5, s:b]
[a:2, b:1]
2
"####;
    check(src, expected);
}

#[test]
fn incdec_on_a_subscript_or_dynamic_name_evaluates_each_part_as_groovy_does() {
    // The receiver and index of `g()[idx()]++` run once; an interpolated name
    // runs once per read and per write, so `key` is called twice.
    let src = r####"def l = [1, 2]
int k = 0, j = 0
def g = { k++; l }
def idx = { j++; 0 }
def r = g()[idx()]++
println "$r $k $j $l"
r = --g()[idx()]
println "$r $k $j $l"
def m = [k: 1]
int c = 0
def key = { c++; 'k' }
r = m."${key()}"++
println "$r $c $m"
"####;
    let expected = r####"1 1 1 [2, 2]
1 2 2 [1, 2]
1 2 [k:2]
"####;
    check(src, expected);
}

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

#[test]
fn uuid_empty_field_message() {
    let src = r####"['1--3-4-5', '-1-2-3-4', '1-2-3-4-'].each { s ->
    try { UUID.fromString(s) } catch (NumberFormatException e) { println "[${e.message}]" }
}
try { UUID.fromString('1-2-3-4-+') } catch (NumberFormatException e) { println e.message }
"####;
    let expected = r####"[]
[]
[]
Error at index 1 in: "+"
"####;
    check(src, expected);
}

#[test]
fn bigdecimal_sqrt_mathcontext() {
    let src = r####"import java.math.MathContext
import java.math.RoundingMode
def t(n, Closure c) { try { println "$n: " + c() } catch (ArithmeticException e) { println "$n: ${e.message}" } }
t(1) { 4.0G.sqrt(new MathContext(10)) }
t(2) { 16.00G.sqrt(new MathContext(10)) }
t(3) { 1.5129G.sqrt(new MathContext(2)) }
t(4) { 4.0000G.sqrt(new MathContext(2)) }
t(5) { 0.0400G.sqrt(new MathContext(5)) }
t(6) { 1E+4G.sqrt(new MathContext(5)) }
t(7) { 1.000G.sqrt(new MathContext(5)) }
t(8) { 2.0G.sqrt(MathContext.UNLIMITED) }
t(9) { 6.25G.sqrt(MathContext.UNLIMITED) }
t(10) { 0.000G.sqrt(new MathContext(5)) }
t(11) { 2.25000G.sqrt(new MathContext(3)) }
t(12) { 9.000G.sqrt(new MathContext(3, RoundingMode.UNNECESSARY)) }
t(13) { 2.0G.sqrt(new MathContext(3, RoundingMode.UNNECESSARY)) }
t(14) { 1.5129G.sqrt(new MathContext(2, RoundingMode.UNNECESSARY)) }
t(15) { 2.0G.sqrt(new MathContext(10)) }
"####;
    let expected = r####"1: 2
2: 4.0
3: 1.2
4: 2.0
5: 0.20
6: 1E+2
7: 1.0
8: Computed square root not exact.
9: 2.5
10: 0.0
11: 1.50
12: 3.0
13: Computed square root not exact.
14: Computed square root not exact.
15: 1.414213562
"####;
    check(src, expected);
}

#[test]
fn missing_method_message_format() {
    let src = r####"import java.math.MathContext
class Foo { String toString() { 'FOO' } }
def t(n, Closure c) { try { c() } catch (MissingMethodException e) { println "$n: ${e.message.readLines()[0]}" } }
t(1) { 5.zork(new MathContext(3), new Foo(), 1..3, 2.5f) }
t(2) { 5.zork(new MathContext(3), new MathContext(4), new MathContext(5), new MathContext(6)) }
t(3) { 5.zork(*(1..40).toList()) }
t(4) { 5.zork((1..40).toList()) }
t(5) { 5.zork('q' * 90, 'r') }
t(6) { 5.zork(~/x/, [1, 2] as int[]) }
"####;
    let expected = r####"1: No signature of method: zork for class: java.lang.Integer is applicable for argument types: (java.math.MathContext, Foo, IntRange, Float) values: [precision=3 roundingMode=HALF_UP, FOO, 1..3, 2.5]
2: No signature of method: zork for class: java.lang.Integer is applicable for argument types: (j.m.MathContext, j.m.MathContext, j.m.MathContext, j.m.MathContext) values: [precision=3 roundingMode=HALF_UP, precision=4 roundingMode=HALF_UP, precision=5 roundingMode=HALF_UP, ...]
3: No signature of method: zork for class: java.lang.Integer is applicable for argument types: (Integer, Integer, Integer, Integer, Integer, Integer, Integer, Integer, Integer, Integer...) values: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, ...]
4: No signature of method: zork for class: java.lang.Integer is applicable for argument types: (ArrayList) values: [[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, ...]]
5: No signature of method: zork for class: java.lang.Integer is applicable for argument types: (String, String) values: [qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq, ...]
6: No signature of method: zork for class: java.lang.Integer is applicable for argument types: (java.util.regex.Pattern, [I) values: [x, [1, 2]]
"####;
    check(src, expected);
}

#[test]
fn closure_argument_sam_coercion() {
    let src = r####"interface Calc { int calc(int x) }
abstract class Ab { abstract int f(int x); int g() { f(1) + 100 } }
def take(Calc c) { c.calc(2) }
println take { it + 1 }
def take2(Calc c, int y) { c.calc(y) }
println take2({ it * 10 }, 4)
class Holder { def run(Calc c) { c.calc(5) }; Holder() {}; Holder(Calc c) { println "ctor ${c.calc(1)}" } }
println new Holder().run { it - 1 }
new Holder({ it + 41 })
def g = { Calc c -> c.calc(7) }
println g { it * 3 }
def ab(Ab a) { a.g() }
println ab { it + 5 }
interface One { def go(x) }
def one(One o) { [o instanceof One, o instanceof Closure, o.go(4)] }
println one { it * 3 }
class Impl implements Calc { int calc(int x) { -x } }
println take(new Impl())
"####;
    let expected = r####"3
40
4
ctor 42
21
106
[true, false, 12]
-2
"####;
    check(src, expected);
}

#[test]
fn constructor_overload_by_type() {
    let src = r####"class P { def v; P(int x) { v = "int $x" }; P(String s) { v = "str $s" }; P(List l) { v = "list $l" } }
println new P(3).v
println new P('x').v
println new P([1]).v
class B { def tag; B(Integer i) { tag = "B-int $i" }; B(String s) { tag = "B-str $s" } }
class D extends B { D(String s) { super(s) }; D(int n, int m) { super(n + m) } }
println new D('q').tag
println new D(2, 3).tag
class E { def v; E(String s, int n = 1) { v = "s$s$n" }; E(int a, int b = 2) { v = "i${a + b}" } }
println new E('x').v
println new E(5).v
println new E('y', 7).v
class F { def v; F(Object o) { v = 'obj' }; F(Number n) { v = 'num' }; F(Integer i) { v = 'int' } }
println new F(3).v
println new F(3.5).v
println new F('s').v
println new F(null).v
"####;
    let expected = r####"int 3
str x
list [1]
B-str q
B-int 5
sx1
i7
sy7
int
num
obj
obj
"####;
    check(src, expected);
}

#[test]
fn final_fields_are_read_only() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
class A { final int v = 3; final String s; A(String s) { this.s = s } }
def a = new A('q')
t(1) { a.v = 4 }
t(2) { a.s = 'z' }
t(3) { a.s }
t(4) { a.setV(5) }
t(5) { a.v += 1 }
t(6) { a.v++ }
t(7) { a.v }
class P { private final int x = 1; public final int y = 2 }
t(8) { new P().y = 5 }
class S { static final int K = 1; static int M = 1 }
t(9) { S.K = 2 }
t(10) { S.M = 2; S.M }
class W { final List l = [] }
t(11) { def w = new W(); w.l << 1; w.l }
class F { final int k; def w; String toString() { "F($k,$w)" } }
t(12) { new F(k: 3, w: 1) }
"####;
    let expected = r####"1: groovy.lang.ReadOnlyPropertyException Cannot set read-only property: v for class: A
2: groovy.lang.ReadOnlyPropertyException Cannot set read-only property: s for class: A
3: q
4: groovy.lang.MissingMethodException No signature of method: setV for class: A is applicable for argument types: (Integer) values: [5]
5: groovy.lang.ReadOnlyPropertyException Cannot set read-only property: v for class: A
6: groovy.lang.ReadOnlyPropertyException Cannot set read-only property: v for class: A
7: 3
8: groovy.lang.ReadOnlyPropertyException Cannot set read-only property: y for class: P
9: groovy.lang.ReadOnlyPropertyException Cannot set read-only property: K for class: S
10: 2
11: [1]
12: groovy.lang.ReadOnlyPropertyException Cannot set read-only property: k for class: F
"####;
    check(src, expected);
}

#[test]
fn interface_constants_through_implementor() {
    let src = r####"interface K { static final int A = 7; String B = 'bee' }
class X implements K { def get() { A + 1 }; def b() { B } }
class Y extends X {}
println X.A
println new X().get()
println K.A
println new X().b()
println X.B
println Y.A
println new Y().get()
try { K.B = 'x' } catch (ReadOnlyPropertyException e) { println e.message }
trait T { int n = 4 }
class U implements T {}
def u = new U(); u.n = 5; println u.n
"####;
    let expected = r####"7
8
7
bee
bee
7
8
Cannot set read-only property: B for class: K
5
"####;
    check(src, expected);
}

#[test]
fn trailing_assignment_is_the_return_value() {
    let src = r####"def x
def f1() { y = 5 }
println f1()
def f2() { def l = [1]; l[0] = 9 }
println f2()
def f3() { def m = [:]; m.k = 4 }
println f3()
def f4() { def i = 1; i += 2 }
println f4()
class C { def v; def set(n) { v = n }; def set2(n) { this.v = n * 2 }; def inc() { v += 1 }
  String name; def setName(String s) { name = s.toUpperCase() }; def rename(s) { this.name = s } }
def c = new C()
println c.set(3)
println c.set2(3)
println c.inc()
println c.rename('al')
println c.name
def g = { x = 11 }
println g()
println([1, 2].collect { x = it * 3 })
def h = { a, b -> a = b }
println h(1, 2)
def k = { it.v = 8 }
println k(c)
def f6() { def s = 'a'; s *= 2 }
println f6()
int f7() { def d = 2.9; d = d * 2 }
println f7()
"####;
    let expected = r####"5
9
4
3
3
6
7
al
al
11
[3, 6]
2
8
aa
5
"####;
    check(src, expected);
}

#[test]
fn property_by_name_on_instances() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
class A { def x = 1; String name = 'n'; def setName(String s) { name = s.toUpperCase() }
  def propertyMissing(String n) { "pm:$n" } }
class B { def y = 2 }
def a = new A()
t(1) { a['x'] }
t(2) { a['x'] = 5; a.x }
t(3) { a.setProperty('x', 6); a.x }
t(4) { a.getProperty('x') }
t(5) { a['name'] = 'bob'; a.name }
t(6) { a.setProperty('name', 'al'); a.name }
t(7) { a['zz'] }
t(8) { new B()['zz'] }
t(9) { new B()['zz'] = 1 }
t(10) { new B().getProperty('zz') }
t(11) { new B().setProperty('zz', 1) }
t(12) { def b = new B(); b.setProperty('y', 7); b['y'] }
"####;
    let expected = r####"1: 1
2: 5
3: 6
4: 6
5: BOB
6: AL
7: pm:zz
8: groovy.lang.MissingPropertyException No such property: zz for class: B
9: groovy.lang.MissingPropertyException No such property: zz for class: B
10: groovy.lang.MissingPropertyException No such property: zz for class: B
11: groovy.lang.MissingPropertyException No such property: zz for class: B
12: 7
"####;
    check(src, expected);
}

#[test]
fn method_missing_and_invoke_method_hooks() {
    let src = r####"class M { def methodMissing(String name, args) { "mm:$name:${args.getClass().name}:${args.length}:${args as List}" } }
println new M().zap(1)
println new M().zap(1, 'b')
println new M().zap()
class O { def invokeMethod(String name, args) { "om:$name:${args.getClass().simpleName}:${args.toList()}" }; def real() { 'real' } }
println new O().q(5, 6)
println new O().real()
class Q { def invokeMethod(String name, args) { "q:$name" }; def methodMissing(String name, args) { "m:$name" } }
println new Q().zz()
class R { def invokeMethod(String name, args) { "r:$name" } }
println new R().with { 5 }
"####;
    let expected = r####"mm:zap:[Ljava.lang.Object;:1:[1]
mm:zap:[Ljava.lang.Object;:2:[1, b]
mm:zap:[Ljava.lang.Object;:0:[]
om:q:Object[]:[5, 6]
real
m:zz
5
"####;
    check(src, expected);
}

#[test]
fn object_level_iteration_on_plain_instances() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
class Z { String toString() { 'Z!' } }
def z = new Z()
t(1) { z.collect { it.toString() } }
t(2) { def r = []; z.each { r << it }; r }
t(3) { z.each { }.is(z) }
t(4) { z.find { true } }
t(5) { z.findAll { true } }
t(6) { z.any { it instanceof Z } }
t(7) { z.every { false } }
t(8) { z.inject('a') { acc, v -> acc + v } }
t(9) { z.findIndexOf { true } }
t(10) { z.findResult { 'fr' } }
t(11) { def r = []; z.eachWithIndex { v, i -> r << i }; r }
t(12) { z.toList() }
"####;
    let expected = r####"1: [Z!]
2: [Z!]
3: true
4: Z!
5: [Z!]
6: true
7: false
8: aZ!
9: 0
10: fr
11: [0]
12: groovy.lang.MissingMethodException No signature of method: toList for class: Z is applicable for argument types: () values: []
"####;
    check(src, expected);
}

#[test]
fn list_and_map_literal_to_class_typed_local() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
class Pt { int x, y; Pt(int x, int y) { this.x = x; this.y = y }; Pt() {}; String toString() { "Pt($x,$y)" } }
t(1) { Pt p = [3, 4]; p }
t(2) { Pt p = [3, 4]; p.getClass().name }
t(3) { Pt p = [x: 5, y: 6]; p }
t(4) { Pt p = []; p }
class V { def a; String toString() { "V($a)" } }
t(5) { V v = [a: 9]; v }
"####;
    let expected = r####"1: Pt(3,4)
2: Pt
3: Pt(5,6)
4: Pt(0,0)
5: V(9)
"####;
    check(src, expected);
}

#[test]
fn map_constructor_writes_properties() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
class Pt { int x, y; String toString() { "Pt($x,$y)" } }
t(1) { new Pt(x: 1, z: 2) }
t(2) { new Pt(x: 1, y: 2) }
class Q { def a; def propertyMissing(String n, v) { println "pm $n=$v" }; String toString() { "Q($a)" } }
t(3) { new Q(a: 1, b: 2) }
class S { def v; void setV(x) { v = x * 2 }; String toString() { "S($v)" } }
t(4) { new S(v: 4) }
"####;
    let expected = r####"1: groovy.lang.MissingPropertyException No such property: z for class: Pt
2: Pt(1,2)
pm b=2
3: Q(1)
4: S(8)
"####;
    check(src, expected);
}

#[test]
fn md5_and_sha256() {
    let src = r####"println 'hello'.md5()
println ''.md5()
println 'hello'.sha256()
println ''.sha256()
println 'abc'.bytes.md5()
println 'abc'.bytes.sha256()
println(('x' * 1000).md5())
println(('y' * 55).sha256())
println(('y' * 56).sha256())
println(('y' * 64).md5())
println 'héllo ✓'.md5()
println "a${1}b".sha256()
"####;
    let expected = r####"5d41402abc4b2a76b9719d911017c592
d41d8cd98f00b204e9800998ecf8427e
2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
900150983cd24fb0d6963f7d28e17f72
ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
398533d48111e9f664b1f64cb10c4b63
fb66d40c3bfff05b0d5af8612d0abfbfacc6f5f26c330bc7ad634f1f44bc20ad
4877e564e5e36e367c7c8d59670774becd3350610b6df4c399c9fa9b66da5813
b0c4864f4e28d2d76929c38d87db7ed5
21b1ae5bc147bb564254200a4731e337
acee240f9a281b012353cca27eb1c4068a15cc67138da3dbc39cc4464d128448
"####;
    check(src, expected);
}

#[test]
fn url_encoder_and_decoder() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
t(1) { URLEncoder.encode('a b&c=d/é~*', 'UTF-8') }
t(2) { URLDecoder.decode('a+b%26c%3Dd%2F%C3%A9', 'UTF-8') }
t(3) { URLEncoder.encode('x y') + URLEncoder.encode('ü') }
t(4) { URLEncoder.encode('ü€', 'ISO-8859-1') }
t(5) { URLDecoder.decode('%FC', 'latin1') }
t(6) { URLEncoder.encode('x', 'nope') }
t(7) { URLDecoder.decode('%zz', 'UTF-8') }
t(8) { URLDecoder.decode('%-1', 'UTF-8') }
t(9) { URLDecoder.decode('abc%2', 'UTF-8') }
t(10) { URLDecoder.decode('%C3', 'UTF-8') }
t(11) { URLDecoder.decode('%e2%82%ac%41', 'utf8') }
t(12) { URLDecoder.decode('%1z') }
t(13) { URLEncoder.encode('😀 a.b-c_d') }
t(14) { java.net.URLEncoder.encode('q r') }
t(15) { URLDecoder.decode('%%41') }
"####;
    let expected = r####"1: a+b%26c%3Dd%2F%C3%A9%7E*
2: a b&c=d/é
3: x+y%C3%BC
4: %FC%3F
5: ü
6: java.io.UnsupportedEncodingException nope
7: java.lang.IllegalArgumentException URLDecoder: Illegal hex characters in escape (%) pattern - Error at index 0 in: "zz"
8: java.lang.IllegalArgumentException URLDecoder: Illegal hex characters in escape (%) pattern - negative value
9: java.lang.IllegalArgumentException URLDecoder: Incomplete trailing escape (%) pattern
10: �
11: €A
12: java.lang.IllegalArgumentException URLDecoder: Illegal hex characters in escape (%) pattern - Error at index 1 in: "1z"
13: %F0%9F%98%80+a.b-c_d
14: q+r
15: java.lang.IllegalArgumentException URLDecoder: Illegal hex characters in escape (%) pattern - Error at index 0 in: "%4"
"####;
    check(src, expected);
}

#[test]
fn case_insensitive_order_comparator() {
    let src = r####"def l = ['b', 'A', 'c', 'a', 'B']
println l.sort(String.CASE_INSENSITIVE_ORDER)
println l
println String.CASE_INSENSITIVE_ORDER.compare('a', 'B')
println String.CASE_INSENSITIVE_ORDER.compare('abc', 'ABC')
println(['b', 'A', 'c'].toSorted(String.CASE_INSENSITIVE_ORDER))
println(['bb', 'A', 'C'].max(String.CASE_INSENSITIVE_ORDER))
println(['x', 'Y', 'z'].sort(false, String.CASE_INSENSITIVE_ORDER))
println(String.CASE_INSENSITIVE_ORDER instanceof Comparator)
def c = { a, b -> a <=> b }
println(c instanceof Comparator)
Comparator d = { a, b -> b <=> a }
println(d instanceof Comparator)
"####;
    let expected = r####"null
[A, a, b, B, c]
-1
0
[A, b, c]
C
[x, Y, z]
true
false
true
"####;
    check(src, expected);
}

#[test]
fn uuid_name_uuid_from_bytes() {
    let src = r####"def u = UUID.nameUUIDFromBytes('hello'.bytes)
println u
println u.version()
println u.variant()
println UUID.nameUUIDFromBytes(new byte[0])
println UUID.nameUUIDFromBytes('héllo ✓'.getBytes('UTF-8'))
println UUID.nameUUIDFromBytes('x'.bytes) == UUID.nameUUIDFromBytes('x'.bytes)
println UUID.nameUUIDFromBytes('abc'.bytes).mostSignificantBits
"####;
    let expected = r####"5d41402a-bc4b-3a76-b971-9d911017c592
3
2
d41d8cd9-8f00-3204-a980-0998ecf8427e
21b1ae5b-c147-3b56-8254-200a4731e337
true
-8070080442485555280
"####;
    check(src, expected);
}

#[test]
fn script_function_overloads_by_parameter_type() {
    let src = r####"SN = this.getClass().name
def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)?.replace(SN, "Script")}" } }
def f(String s) { "s" }
def f(int i) { "i" }
def f(List l) { "l" }
t(1) { f(1) }
t(2) { f('x') }
t(3) { f([1]) }
t(4) { f("g${1}") }
def g(Integer i) { 'I' }
def g(Object o) { 'O' }
t(5) { g(1) }
t(6) { g('a') }
t(7) { g(null) }
def h(int a, String b) { 'is' }
def h(String a, int b) { 'si' }
t(8) { h(1, 'x') }
t(9) { h('x', 1) }
t(10) { h(1, 2) }
def k(String s) { 'ks' }
def k(Map m) { 'km' }
t(11) { k(['z']) }
t(12) { k(5) }
def args = [2]
t(13) { f(*args) }
"####;
    let expected = r####"1: i
2: s
3: l
4: s
5: I
6: O
7: O
8: is
9: si
10: groovy.lang.MissingMethodException No signature of method: h for class: Script is applicable for argument types: (Integer, Integer) values: [1, 2]
11: ks
12: groovy.lang.MissingMethodException No signature of method: k for class: Script is applicable for argument types: (Integer) values: [5]
13: i
"####;
    check(src, expected);
}

#[test]
fn method_selection_by_argument_type() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
interface Calc { int calc(int x) }
class A { def m(Calc c) { c.calc(1) }; def m(String s) { 's' }
          def n(Closure c) { c(2) }; def n(int i) { 'i' }
          def g(String s) { s }; def g(int i) { 'gi' }
          def o(Object o) { 'o' }; def o(int i) { 'oi' }
          def d(double d) { "d$d" }; def d(String s) { 'ds' }
          def b(BigDecimal b) { "b$b" }; def b(String s) { 'bs' }
          def ch(Character c) { "C$c" }; def ch(int i) { 'ci' }
          def s(String x) { 'ss' }; def one(int a) { "one$a" }; def bo(boolean x) { "b$x" } }
def a = new A()
t(1) { a.m { it + 10 } }
t(2) { a.m('x') }
t(3) { a.n { it * 3 } }
t(4) { a.n(4) }
t(5) { a.g("x${1}") }
t(6) { a.g(3) }
t(7) { a.o(null) }
t(8) { a.o(3) }
t(9) { a.o('x') }
t(10) { a.d(3) }
t(11) { a.d(2.5) }
t(12) { a.b(3) }
t(13) { a.b(2.5d) }
t(14) { a.ch('q' as Character) }
t(15) { a.s(5) }
t(16) { a.one('x') }
t(17) { a.one(2.5) }
t(18) { a.one([7]) }
t(19) { a.one([1, 2]) }
t(20) { a.bo(1) }
t(21) { a.one(null) }
"####;
    let expected = r####"1: 11
2: s
3: 6
4: i
5: x1
6: gi
7: o
8: oi
9: o
10: d3.0
11: d2.5
12: b3
13: b2.5
14: Cq
15: groovy.lang.MissingMethodException No signature of method: s for class: A is applicable for argument types: (Integer) values: [5]
16: groovy.lang.MissingMethodException No signature of method: one for class: A is applicable for argument types: (String) values: [x]
17: groovy.lang.MissingMethodException No signature of method: one for class: A is applicable for argument types: (BigDecimal) values: [2.5]
18: one7
19: groovy.lang.MissingMethodException No signature of method: one for class: A is applicable for argument types: (ArrayList) values: [[1, 2]]
20: groovy.lang.MissingMethodException No signature of method: bo for class: A is applicable for argument types: (Integer) values: [1]
21: groovy.lang.MissingMethodException No signature of method: one for class: A is applicable for argument types: (null) values: [null]
"####;
    check(src, expected);
}

#[test]
fn numeric_parameters_convert_their_argument() {
    let src = r####"class P { def v; P(double d) { v = "double ${d} ${d.getClass().simpleName}" } }
class R { def v; R(BigDecimal d) { v = "BD ${d} ${d.getClass().simpleName}" } }
class X { def v; X(float d) { v = "f ${d} ${d.getClass().simpleName}" } }
class Y { def v; Y(BigInteger d) { v = "BI ${d} ${d.getClass().simpleName}" } }
def ok = { c -> try { c() } catch (e) { 'X' } }
println([3, 3L, 2.5, 2.5d, 'str', 2.5f, 5G].collect { a -> ok { new P(a).v } })
println([3, 3L, 2.5, 2.5d, 'str', 2.5f, 5G].collect { a -> ok { new R(a).v } })
println([3, 3L, 2.5, 2.5d, 'str', 2.5f, 5G].collect { a -> ok { new X(a).v } })
println([3, 3L, 2.5, 2.5d, 'str', 2.5f, 5G].collect { a -> ok { new Y(a).v } })
def half(double x) { x / 2 }
println half(3)
class M { def twice(double x) { x * 2 } }
println new M().twice(4)
def big = { BigDecimal b -> b.scale() }
println big(7)
"####;
    let expected = r####"[double 3.0 Double, double 3.0 Double, double 2.5 Double, double 2.5 Double, X, double 2.5 Double, double 5.0 Double]
[BD 3 BigDecimal, BD 3 BigDecimal, BD 2.5 BigDecimal, BD 2.5 BigDecimal, X, BD 2.5 BigDecimal, BD 5 BigDecimal]
[f 3.0 Float, f 3.0 Float, f 2.5 Float, X, X, f 2.5 Float, f 5.0 Float]
[BI 3 BigInteger, BI 3 BigInteger, X, X, X, X, BI 5 BigInteger]
1.5
8.0
0
"####;
    check(src, expected);
}

#[test]
fn constructor_without_a_matching_signature_raises() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.join(' / ')}" } }
class P { P(int a) {}; P(int a, String b) {} ; String toString() { 'P' } }
class Q { Q(String s) {}; String toString() { 'Q' } }
t(1) { new P('x') }
t(2) { new P('x', 1) }
t(3) { new P(null, null) }
t(4) { new P([1], 2.5) }
t(5) { new Q(1) }
t(6) { new Q(new java.math.MathContext(2)) }
t(7) { new P(1) }
t(8) { new Q(null) }
t(9) { new P(1, 'a') }
t(10) { new P(1, 2, 3) }
t(11) { new Q() }
class E1 extends Exception { E1(String m) { super(m) } }
t(12) { new E1(5).message }
t(13) { new E1('m').message }
"####;
    let expected = r####"1: groovy.lang.GroovyRuntimeException Could not find matching constructor for: P(String)
2: groovy.lang.GroovyRuntimeException Could not find matching constructor for: P(String, Integer)
3: groovy.lang.GroovyRuntimeException Could not find matching constructor for: P(null, null)
4: groovy.lang.GroovyRuntimeException Could not find matching constructor for: P(ArrayList, BigDecimal)
5: groovy.lang.GroovyRuntimeException Could not find matching constructor for: Q(Integer)
6: groovy.lang.GroovyRuntimeException Could not find matching constructor for: Q(java.math.MathContext)
7: P
8: Q
9: P
10: groovy.lang.GroovyRuntimeException Could not find matching constructor for: P(Integer, Integer, Integer)
11: Q
12: groovy.lang.GroovyRuntimeException Could not find matching constructor for: E1(Integer)
13: m
"####;
    check(src, expected);
}

#[test]
fn closure_for_a_multi_method_interface_is_refused() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)?.replaceAll(/\(.*/, "(…")}" } }
interface Two { int a(int x); int b(int x) }
interface One { int a(int x) }
class H { def two(Two t) { t.a(3) }; def one(One o) { o.a(3) }; H() {}; H(Two t) {}; String toString() { 'H' } }
t(1) { new H().two { it * 2 } }
t(2) { new H().one { it * 2 } }
t(3) { new H({ it }) }
"####;
    let expected = r####"1: groovy.lang.MissingMethodException No signature of method: two for class: H is applicable for argument types: (…
2: 6
3: groovy.lang.GroovyRuntimeException Could not find matching constructor for: H(…
"####;
    check(src, expected);
}

#[test]
fn closure_as_a_concrete_class() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
class C { def a() { 'a' }; def b(x) { "b$x" }; def f = 3 }
def d = { 'k' } as C
t(1) { d.a() }
t(2) { d.b(1) }
t(3) { d instanceof C }
t(4) { d.zzz() }
t(5) { d.f }
t(6) { d.getClass().name }
t(7) { [d].collect { it.a() } }
def e = { x -> "e$x" } as C
t(8) { e.b(5) }
t(9) { e.a() }
"####;
    let expected = r####"1: k
2: k
3: true
4: groovy.lang.MissingMethodException No signature of method: zzz for class: C1_groovyProxy is applicable for argument types: () values: []
5: k
6: C1_groovyProxy
7: [k]
8: e5
9: enull
"####;
    check(src, expected);
}

#[test]
fn direct_write_of_a_final_field() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
class A { final int v = 3; final String s; int w; A(String s) { this.s = s; this.@w = 1 }; A(int x) { s = 'i'; this.@v2 = 0 }; int v2 }
def a = new A('q')
t(1) { a.@v = 11; a.v }
t(2) { a.@w = 5; a.w }
t(3) { a.s }
t(4) { new A(2).s }
t(5) { a.@s = 'z' }
"####;
    let expected = r####"1: groovy.lang.GroovyRuntimeException Cannot set the property 'v' because the backing field is final.
2: 5
3: q
4: i
5: groovy.lang.GroovyRuntimeException Cannot set the property 's' because the backing field is final.
"####;
    check(src, expected);
}

#[test]
fn store_into_a_class_typed_variable_is_cast_to_type() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
class Pt { int x, y; Pt(int x, int y) { this.x = x; this.y = y }; Pt() {}; String toString() { "Pt($x,$y)" } }
class V { def a; String toString() { "V($a)" } }
t(1) { def l = [3, 4]; Pt p = l; p }
t(2) { def m = [x: 5, y: 6]; Pt p = m; p }
t(3) { Pt p = [x: 1, z: 2]; p }
t(4) { [x: 1, z: 2] as Pt }
t(5) { Pt p = [1]; p }
t(6) { def l = [1]; Pt p = l; p }
t(7) { [a: 1, b: 2] as V }
t(8) { V v = [a: 1, b: 2]; v }
t(9) { Pt p = 'str'; p }
t(10) { def s = 'str'; Pt p = s; p }
t(11) { Pt p = null; p }
t(12) { def q = new Pt(2, 2); Pt p = q; p }
Pt mk() { [7, 8] }
t(13) { mk() }
"####;
    let expected = r####"1: Pt(3,4)
2: Pt(5,6)
3: org.codehaus.groovy.runtime.typehandling.GroovyCastException Cannot cast object '{x=1, z=2}' with class 'java.util.LinkedHashMap' to class 'Pt' due to: org.codehaus.groovy.runtime.metaclass.MissingPropertyExceptionNoStack: No such property: z for class: Pt
4: Pt(0,0)
5: org.codehaus.groovy.runtime.typehandling.GroovyCastException Cannot cast object '[1]' with class 'java.util.ArrayList' to class 'Pt' due to: groovy.lang.GroovyRuntimeException: Could not find matching constructor for: Pt(Integer)
6: org.codehaus.groovy.runtime.typehandling.GroovyCastException Cannot cast object '[1]' with class 'java.util.ArrayList' to class 'Pt' due to: groovy.lang.GroovyRuntimeException: Could not find matching constructor for: Pt(Integer)
7: V(null)
8: org.codehaus.groovy.runtime.typehandling.GroovyCastException Cannot cast object '{a=1, b=2}' with class 'java.util.LinkedHashMap' to class 'V' due to: org.codehaus.groovy.runtime.metaclass.MissingPropertyExceptionNoStack: No such property: b for class: V
9: org.codehaus.groovy.runtime.typehandling.GroovyCastException Cannot cast object 'str' with class 'java.lang.String' to class 'Pt'
10: org.codehaus.groovy.runtime.typehandling.GroovyCastException Cannot cast object 'str' with class 'java.lang.String' to class 'Pt'
11: null
12: Pt(2,2)
13: Pt(7,8)
"####;
    check(src, expected);
}

#[test]
fn record_declarations() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
record Point(int x, int y) {
  def sum() { x + y }
}
record Named(String name, List tags) {}
def p = new Point(1, 2)
t(1) { p }
t(2) { p.x() }
t(3) { p.x }
t(4) { p.sum() }
t(5) { p == new Point(1, 2) }
t(6) { p == new Point(2, 1) }
t(7) { p.hashCode() }
t(8) { p.x = 5 }
t(9) { new Named('a', [1]) }
t(10) { new Point(x: 3, y: 4) }
t(11) { p.getClass().name }
t(12) { new Point(1) }
t(13) { [new Point(2, 2), new Point(1, 1)].sort { it.x } }
t(14) { p.getX() }
t(15) { p.toList() }
t(16) { p.size() }
t(17) { p[1] }
t(18) { p.toMap() }
t(19) { new Named('b', null).hashCode() }
t(20) { p instanceof Record }
"####;
    let expected = r####"1: Point[x=1, y=2]
2: 1
3: 1
4: 3
5: true
6: false
7: 33
8: groovy.lang.ReadOnlyPropertyException Cannot set read-only property: x for class: Point
9: Named[name=a, tags=[1]]
10: Point[x=3, y=4]
11: Point
12: groovy.lang.GroovyRuntimeException Could not find matching constructor for: Point(Integer)
13: [Point[x=1, y=1], Point[x=2, y=2]]
14: groovy.lang.MissingMethodException No signature of method: getX for class: Point is applicable for argument types: () values: []
15: [1, 2]
16: 2
17: 2
18: [x:1, y:2]
19: 3038
20: true
"####;
    check(src, expected);
}

#[test]
fn integer_equals_long_is_false() {
    let src = r####"println 5.equals(5L)
println 5L.equals(5L)
println 5L.equals(5)
println 5.equals(5)
def x = 5L
println 5.equals(x)
println x.equals(5L)
long y = 7
println y.equals(7)
println y.equals(7L)
println 5.equals(6)
println 5 == 5L
"####;
    let expected = r####"false
true
false
true
false
true
false
true
false
true
"####;
    check(src, expected);
}

#[test]
fn println_before_a_sign_is_a_binary_operator() {
    let src = r####"SN = this.getClass().name
def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)?.replace(SN, "Script")}" } }
t(1) { println -7 % 3 }
t(2) { print +5 }
t(3) { println(-7 % 3) }
t(4) { def x = 3; println -x }
println 'end'
"####;
    let expected = r####"1: groovy.lang.MissingPropertyException No such property: println for class: Script
2: groovy.lang.MissingPropertyException No such property: print for class: Script
-1
3: null
4: groovy.lang.MissingPropertyException No such property: println for class: Script
end
"####;
    check(src, expected);
}

#[test]
fn encodings_answer_a_writable() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
def w = 'abc'.bytes.encodeBase64()
def h = 'abc'.bytes.encodeHex()
t(1) { w }
t(2) { w.getClass().name }
t(3) { h.getClass().name }
t(4) { w == 'YWJj' }
t(5) { w.toString() == 'YWJj' }
t(6) { w instanceof Writable }
t(7) { w.length() }
t(8) { "x${w}y" }
t(9) { def sw = new StringWriter(); w.writeTo(sw); sw.toString() }
t(10) { w as String }
t(11) { [w].join(',') }
t(12) { h.toString() }
t(13) { ('x' * 60).bytes.encodeBase64(true).toString().inspect() }
t(14) { ('y' * 57).bytes.encodeBase64(true).toString().inspect() }
t(15) { 'ab'.bytes.encodeBase64(false).toString() }
"####;
    let expected = r####"1: YWJj
2: org.codehaus.groovy.runtime.EncodingGroovyMethods$1
3: org.codehaus.groovy.runtime.EncodingGroovyMethods$2
4: false
5: true
6: true
7: groovy.lang.MissingMethodException No signature of method: length for class: org.codehaus.groovy.runtime.EncodingGroovyMethods$1 is applicable for argument types: () values: []
8: xYWJjy
9: YWJj
10: YWJj
11: YWJj
12: 616263
13: 'eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4\r\neHh4'
14: 'eXl5eXl5eXl5eXl5eXl5eXl5eXl5eXl5eXl5eXl5eXl5eXl5eXl5eXl5eXl5eXl5eXl5eXl5eXl5\r\n'
15: YWI=
"####;
    check(src, expected);
}

#[test]
fn list_with_default() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
def l = [].withDefault { it * 2 }
t(1) { [l[3], l] }
t(2) { [l[1], l] }
t(3) { l.getClass().name }
t(4) { l.size() }
t(5) { l.get(5); l }
t(6) { def e = [1].withEagerDefault { it * 10 }; [e[3], e] }
t(7) { def z = [1, null, 3].withDefault { -1 }; [z[1], z] }
t(8) { def z = [1, null, 3].withEagerDefault { -1 }; [z[1], z] }
t(9) { def z = [].withLazyDefault { 'x' }; [z[0], z] }
t(10) { def z = [5].withDefault { 0 }; z << 6; z }
t(11) { def z = [].withDefault { 0 }; z[-1] }
t(12) { def z = [1,2].withDefault { 0 }; z.collect { it + 1 } }
"####;
    let expected = r####"1: [6, [null, null, null, 6]]
2: [2, [null, 2, null, 6]]
3: groovy.lang.ListWithDefault
4: 4
5: [null, 2, null, 6, null, 10]
6: [30, [1, 10, 20, 30]]
7: [-1, [1, -1, 3]]
8: [null, [1, null, 3]]
9: [x, [x]]
10: [5, 6]
11: java.lang.IndexOutOfBoundsException Negative index [-1] too large for list size 0
12: [2, 3]
"####;
    check(src, expected);
}

#[test]
fn tuples() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
def tp = new Tuple2(1, 'a')
t(1) { tp }
t(2) { [tp.v1, tp.v2, tp.getV1(), tp[1], tp.size()] }
t(3) { tp.getClass().name }
t(4) { tp == [1, 'a'] }
t(5) { tp[0] = 5 }
t(6) { tp.add(3) }
t(7) { Tuple.tuple(1, 2, 3).getClass().name }
t(8) { Tuple.tuple(1, 2, 3).v3 }
t(9) { new Tuple(1, 2).getClass().name }
t(10) { tp.collect { it.toString() * 2 } }
t(11) { def (a, b) = tp; "$a$b" }
t(12) { tp.v3 }
t(13) { new Tuple2(1, 2) == new Tuple2(1, 2) }
t(14) { Tuple.tuple().getClass().name }
t(15) { new Tuple3(1, 2, 3).v3 }
t(16) { tp instanceof List }
t(17) { tp.first }
t(18) { tp.second }
"####;
    let expected = r####"1: [1, a]
2: [1, a, 1, a, 2]
3: groovy.lang.Tuple2
4: true
5: java.lang.UnsupportedOperationException null
6: java.lang.UnsupportedOperationException null
7: groovy.lang.Tuple3
8: 3
9: groovy.lang.Tuple
10: [11, aa]
11: 1a
12: groovy.lang.MissingPropertyException Exception evaluating property 'v3' for groovy.lang.Tuple2, Reason: groovy.lang.MissingPropertyException: No such property: v3 for class: java.lang.Integer
13: true
14: groovy.lang.Tuple0
15: 3
16: true
17: 1
18: a
"####;
    check(src, expected);
}

#[test]
fn gpath_property_miss_is_wrapped() {
    let src = r####"try { [1, 'a'].v3 } catch (MissingPropertyException e) { println e.message }
try { [new Object()].zz } catch (MissingPropertyException e) { println e.message }
println(['ab', 'c'].bytes.size())
"####;
    let expected = r####"Exception evaluating property 'v3' for java.util.ArrayList, Reason: groovy.lang.MissingPropertyException: No such property: v3 for class: java.lang.Integer
Exception evaluating property 'zz' for java.util.ArrayList, Reason: groovy.lang.MissingPropertyException: No such property: zz for class: java.lang.Object
2
"####;
    check(src, expected);
}

#[test]
fn zero_arguments_and_variadic_parameters() {
    let src = r####"SN = this.getClass().name
def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)?.replace(SN, "Script")}" } }
class A { def m(x) { "m$x" }; def p(int x) { "p$x" }; def s(String x) { "s$x" }
  def v(String... xs) { xs.length }; def w(int a, Object... rest) { "$a ${rest.length}" } }
class Q { def v; Q(String s) { v = "q$s" } }
class R { def v; R(int s) { v = "r$s" } }
class V { def n; V(String... xs) { n = xs.length } }
def f(x) { "f$x" }
def g(int x) { x }
def h(a, b) { a }
def a = new A()
t(1) { a.m() }
t(2) { a.p() }
t(3) { a.s() }
t(4) { new Q().v }
t(5) { new R().v }
t(6) { f() }
t(7) { g() }
t(8) { h(1) }
t(9) { f(1, 2) }
t(10) { a.v('a', 'b') }
t(11) { a.v() }
t(12) { a.v('x') }
t(13) { a.w(1, 2, 3) }
t(14) { a.w(1) }
t(15) { new V('a', 'b', 'c').n }
t(16) { new V().n }
t(17) { a.v(1) }
"####;
    let expected = r####"1: mnull
2: groovy.lang.MissingMethodException No signature of method: p for class: A is applicable for argument types: () values: []
3: snull
4: qnull
5: groovy.lang.GroovyRuntimeException Could not find matching constructor for: R()
6: fnull
7: groovy.lang.MissingMethodException No signature of method: g for class: Script is applicable for argument types: () values: []
8: groovy.lang.MissingMethodException No signature of method: h for class: Script is applicable for argument types: (Integer) values: [1]
9: groovy.lang.MissingMethodException No signature of method: f for class: Script is applicable for argument types: (Integer, Integer) values: [1, 2]
10: 2
11: 0
12: 1
13: 1 2
14: 1 0
15: 3
16: 0
17: groovy.lang.MissingMethodException No signature of method: v for class: A is applicable for argument types: (Integer) values: [1]
"####;
    check(src, expected);
}

#[test]
fn enum_constructor_arguments() {
    let src = r####"enum Op {
  PLUS("+"), MINUS("-")
  final String sym
  Op(String sym) { this.sym = sym }
  def apply(a, b) { this == PLUS ? a + b : a - b }
}
println Op.MINUS.sym
println Op.PLUS.apply(2, 3)
enum Level { LOW(1), HIGH(10)
  final int n
  Level(int n) { this.n = n }
}
println Level.values().collect { "$it:${it.n}:${it.ordinal()}" }
"####;
    let expected = r####"-
5
[LOW:1:0, HIGH:10:1]
"####;
    check(src, expected);
}

#[test]
fn script_level_this() {
    let src = r####"println this.is(this)
def f() { this.getClass().name == getClass().name }
println f()
def c = { this.is(owner) && this.is(thisObject) }
println c()
println(this instanceof Script)
"####;
    let expected = r####"true
true
true
true
"####;
    check(src, expected);
}

#[test]
fn method_assigning_a_final_field_is_a_compile_error() {
    // Groovy rejects the program before running any of it: "cannot modify
    // final field 'v' outside of constructor."
    for src in [
        "println 'x'\nclass B { final int v = 1; def bump() { v = 2 } }\n",
        "println 'x'\nclass B { final int v = 1; def bump() { this.v += 2 } }\n",
    ] {
        let (out, ok) = run(src);
        assert!(!ok);
        assert_eq!(out, "");
    }
}

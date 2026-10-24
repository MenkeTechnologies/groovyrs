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

#[test]
fn single_declaration_function_is_type_checked() {
    let src = r####"SN = this.getClass().name
def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)?.replace(SN, 'Script')}" } }
def k(String s) { "k$s" }
def i(int n) { n * 2 }
def d(double x) { x }
def l(List x) { x.size() }
def two(String a, int b) { a * b }
t(1) { k(5) }
t(2) { k('a') }
t(3) { def v = 5; k(v) }
t(4) { i('x') }
t(5) { i(3) }
t(6) { def v = 'q'; i(v) }
t(7) { d(2) }
t(8) { l([1, 2]) }
t(9) { l('s') }
t(10) { two('ab', 2) }
t(11) { two(2, 'ab') }
t(12) { k(null) }
t(13) { i(null) }
t(14) { k("g${1}") }
int fib(int n) { n < 2 ? n : fib(n - 1) + fib(n - 2) }
t(15) { fib(20) }
t(16) { def s = 'x'; [k(s)] }
"####;
    let expected = r####"1: groovy.lang.MissingMethodException No signature of method: k for class: Script is applicable for argument types: (Integer) values: [5]
2: ka
3: groovy.lang.MissingMethodException No signature of method: k for class: Script is applicable for argument types: (Integer) values: [5]
4: groovy.lang.MissingMethodException No signature of method: i for class: Script is applicable for argument types: (String) values: [x]
5: 6
6: groovy.lang.MissingMethodException No signature of method: i for class: Script is applicable for argument types: (String) values: [q]
7: 2.0
8: 2
9: groovy.lang.MissingMethodException No signature of method: l for class: Script is applicable for argument types: (String) values: [s]
10: abab
11: groovy.lang.MissingMethodException No signature of method: two for class: Script is applicable for argument types: (Integer, String) values: [2, ab]
12: knull
13: groovy.lang.MissingMethodException No signature of method: i for class: Script is applicable for argument types: (null) values: [null]
14: kg1
15: 6765
16: [kx]
"####;
    check(src, expected);
}

#[test]
fn ambiguous_no_argument_call() {
    let src = r####"class S { def v; S(String s) { v = 1 }; S(Integer i) { v = 2 } }
class T { def m(String s) { 1 }; def m(List l) { 2 } }
class U { def v; U(String s) { v = 'u' } }
def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message.readLines()[0]} (${e.message.readLines().size()} lines)" } }
t(1) { new S() }
t(2) { new T().m() }
t(3) { new U().v }
class B { def m(String s) { 'b' } }
class C extends B { def m(String s) { 'c' } }
t(4) { new C().m() }
"####;
    let expected = r####"1: org.codehaus.groovy.runtime.metaclass.MethodSelectionException Could not find which method <init>() to invoke from this list: (3 lines)
2: org.codehaus.groovy.runtime.metaclass.MethodSelectionException Could not find which method m() to invoke from this list: (3 lines)
3: u
4: c
"####;
    check(src, expected);
}

#[test]
fn tree_set_ordered_by_a_comparator() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
Comparator c = { a, b -> b <=> a }
def s = new TreeSet(c); s << 1 << 3 << 2
t(1) { s }
t(2) { s.first() }
t(3) { s.contains(2) }
t(4) { s.remove(3); s }
t(5) { def ci = new TreeSet(String.CASE_INSENSITIVE_ORDER); ci << 'b' << 'A' << 'a' << 'C'; ci }
t(6) { def ci = new TreeSet(String.CASE_INSENSITIVE_ORDER); ci.addAll(['x', 'X', 'y']); [ci, ci.size()] }
t(7) { s.getClass().name }
t(8) { s.collect { it * 10 } }
t(9) { def ci = new TreeSet(String.CASE_INSENSITIVE_ORDER); ci.add('q'); [ci.add('Q'), ci.contains('Q')] }
t(10) { s.size() }
t(11) { s.toList() }
t(12) { (s as List).join('-') }
"####;
    let expected = r####"1: [3, 2, 1]
2: 3
3: true
4: [2, 1]
5: [A, b, C]
6: [[x, y], 2]
7: java.util.TreeSet
8: [20, 10]
9: [false, true]
10: 2
11: [2, 1]
12: 2-1
"####;
    check(src, expected);
}

#[test]
fn closure_class_names() {
    let src = r####"SN = this.getClass().name
def a = { 1 }
println((a.getClass().name).toString().replace(SN, "S"))
def b = { -> { -> { -> 3 } } }
println((b.getClass().name).toString().replace(SN, "S"))
println((b().getClass().name).toString().replace(SN, "S"))
println((b()().getClass().name).toString().replace(SN, "S"))
def c = { [{ 1 }, { 2 }] }
println((c().collect { it.getClass().name }).toString().replace(SN, "S"))
def f() { { -> 5 } }
println((f().getClass().name).toString().replace(SN, "S"))
def g() { [{ 5 }, { -> { -> 6 } }] }
println((g()*.getClass()*.name).toString().replace(SN, "S"))
println((g()[1]().getClass().name).toString().replace(SN, "S"))
class K { def m() { { -> 7 } }; def n = { 8 }; static s() { { -> 9 } }; def w() { { -> { -> 1 } } } }
println((new K().m().getClass().name).toString().replace(SN, "S"))
println((new K().n.getClass().name).toString().replace(SN, "S"))
println((K.s().getClass().name).toString().replace(SN, "S"))
println((new K().w()().getClass().name).toString().replace(SN, "S"))
println([1].collect { it }.getClass().name)
def d = { 4 }
println((d.getClass().name).toString().replace(SN, "S"))
println(d.getClass().simpleName)
def t2(n, Closure c) { try { println "$n: " + c().toString().replace(SN, "S") } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)?.replaceAll(/@[0-9a-f]+/, "@H")?.replace(SN, "S")}" } }
def up = 'abc'.&toUpperCase
t2(1) { up.getClass().name }
t2(2) { String.CASE_INSENSITIVE_ORDER.getClass().name }
t2(3) { 5.zork({ 1 }) }
t2(4) { { -> 2 }.getClass().simpleName }
t2(5) { [1, 2]*.toString().getClass().name }
t2(6) { def x; def f = (x = { 3 }); f.getClass().name }
t2(7) { this.&t.getClass().name }
"####;
    let expected = r####"S$_run_closure1
S$_run_closure2
S$_run_closure2$_closure17
S$_run_closure2$_closure17$_closure18
[S$_run_closure3$_closure19, S$_run_closure3$_closure20]
S$_f_closure14
[S$_g_closure15, S$_g_closure16]
S$_g_closure16$_closure24
K$_m_closure2
K$_closure1
K$_s_closure3
K$_w_closure4$_closure5
java.util.ArrayList
S$_run_closure6
_run_closure6
1: org.codehaus.groovy.runtime.MethodClosure
2: java.lang.String$CaseInsensitiveComparator
3: groovy.lang.MissingMethodException No signature of method: zork for class: java.lang.Integer is applicable for argument types: (S$_run_closure9$_closure21) values: [S$_run_closure9$_closure21@H]
4: _closure22
5: java.util.ArrayList
6: S$_run_closure12$_closure23
7: org.codehaus.groovy.runtime.MethodClosure
"####;
    check(src, expected);
}

#[test]
fn missing_property_possible_solutions() {
    let src = r####"class Pt { int zeta, alpha, beta; def getGamma() { 1 } }
class Sub extends Pt { def own }
def t(n, Closure c) { try { c() } catch (e) { println "$n: ${e.message.replace('\n', ' / ')}" } }
t(1) { Pt p = [q: 1]; p }
t(2) { Pt p = [zetaa: 1]; p }
t(3) { Pt p = [Alpha: 1]; p }
t(4) { Pt p = [clas: 1]; p }
t(5) { Sub s = [ow: 1]; s }
t(6) { Sub s = [qqqqqq: 1]; s }
t(7) { new Pt().zzz }
t(8) { new Pt().zet }
t(9) { new Pt(gama: 1) }
t(10) { Pt p = [eta: 1]; p }
class R { def xbcd; def abcd }
try { new R().bacd } catch (e) { println e.message.replace("\n", " / ") }
"####;
    let expected = r####"1: Cannot cast object '{q=1}' with class 'java.util.LinkedHashMap' to class 'Pt' due to: org.codehaus.groovy.runtime.metaclass.MissingPropertyExceptionNoStack: No such property: q for class: Pt
2: Cannot cast object '{zetaa=1}' with class 'java.util.LinkedHashMap' to class 'Pt' due to: org.codehaus.groovy.runtime.metaclass.MissingPropertyExceptionNoStack: No such property: zetaa for class: Pt / Possible solutions: zeta, beta
3: Cannot cast object '{Alpha=1}' with class 'java.util.LinkedHashMap' to class 'Pt' due to: org.codehaus.groovy.runtime.metaclass.MissingPropertyExceptionNoStack: No such property: Alpha for class: Pt / Possible solutions: alpha
4: Cannot cast object '{clas=1}' with class 'java.util.LinkedHashMap' to class 'Pt' due to: org.codehaus.groovy.runtime.metaclass.MissingPropertyExceptionNoStack: No such property: clas for class: Pt / Possible solutions: class
5: Cannot cast object '{ow=1}' with class 'java.util.LinkedHashMap' to class 'Sub' due to: org.codehaus.groovy.runtime.metaclass.MissingPropertyExceptionNoStack: No such property: ow for class: Sub / Possible solutions: own
6: Cannot cast object '{qqqqqq=1}' with class 'java.util.LinkedHashMap' to class 'Sub' due to: org.codehaus.groovy.runtime.metaclass.MissingPropertyExceptionNoStack: No such property: qqqqqq for class: Sub
7: No such property: zzz for class: Pt / Possible solutions: zeta
8: No such property: zet for class: Pt / Possible solutions: zeta, beta
9: No such property: gama for class: Pt / Possible solutions: gamma, zeta, beta
10: Cannot cast object '{eta=1}' with class 'java.util.LinkedHashMap' to class 'Pt' due to: org.codehaus.groovy.runtime.metaclass.MissingPropertyExceptionNoStack: No such property: eta for class: Pt / Possible solutions: zeta, beta
No such property: bacd for class: R / Possible solutions: abcd, xbcd
"####;
    check(src, expected);
}

#[test]
fn map_as_class_proxy_class() {
    let src = r####"class Pt { int x; String toString() { "Pt($x)" } }
def p = [x: 1, z: 2] as Pt
println p
println p.getClass().name
println p instanceof Pt
println(([x: 3] as Pt).getClass().name)
"####;
    let expected = r####"Pt(0)
Pt1_groovyProxy
true
Pt
"####;
    check(src, expected);
}

#[test]
fn record_compact_constructor_and_this_call() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
record P(int x, String s) {
  P {
    if (x < 0) throw new IllegalArgumentException('neg')
    s = s.toUpperCase()
  }
}
t(1) { new P(1, 'a') }
t(2) { new P(-1, 'a') }
t(3) { new P(1, 'b').properties }
t(4) { P.serialVersionUID }
record Q(int a) { Q(String s) { this(s.size()) } }
t(5) { new Q('abc') }
record Q2(int a) { Q2(String s) { this(s.size()) } }
println new Q2(5)
class A { def v; A() { this(3) }; A(x) { v = x }; String toString() { "A$v" } }
println new A()
"####;
    let expected = r####"1: P[x=1, s=A]
2: java.lang.IllegalArgumentException neg
3: [x:1, s:B, serialVersionUID:0, class:class P]
4: 0
5: Q[a=3]
Q2[a=5]
A3
"####;
    check(src, expected);
}

#[test]
fn array_typed_closure_parameter_is_variadic() {
    let src = r####"def a = { Object[] xs -> xs.length }
println a(1, 2, 3)
println a()
println a(1)
def b = { String s, int[] ns -> "$s ${ns.toList()}" }
println b('x', 1, 2)
println b('y')
println a.maximumNumberOfParameters
def c = { Object[] xs -> "c${xs?.toList()}" } as Runnable
"####;
    let expected = r####"3
0
1
x [1, 2]
y []
1
"####;
    check(src, expected);
}

#[test]
fn expando_meta_class_injection() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
String.metaClass.shout = { -> delegate.toUpperCase() + '!' }
t(1) { 'hi'.shout() }
t(2) { "g${1}".shout() }
String.metaClass.wrap = { String l, String r -> l + delegate + r }
t(3) { 'x'.wrap('[', ']') }
Integer.metaClass.twice = { -> delegate * 2 }
t(4) { 5.twice() }
class P { def x = 1; def own() { 'own' } }
class Q extends P {}
P.metaClass.greet = { "hi ${delegate.x}" }
t(5) { new P().greet() }
t(6) { new Q().greet() }
def p = new P(); p.metaClass.solo = { -> 'only me' }
t(7) { p.solo() }
t(8) { new P().solo() }
P.metaClass.static.make = { -> new P(x: 9) }
t(9) { P.make().x }
P.metaClass.getY = { -> delegate.x + 41 }
t(10) { new P().y }
P.metaClass.own = { -> 'replaced' }
t(11) { new P().own() }
Object.metaClass.anyone = { -> 'everyone' }
t(12) { 3.anyone() + ' ' + [].anyone() }
t(13) { new P().metaClass.getClass().simpleName }
t(14) { String.metaClass.getClass().simpleName }
String.metaClass.toUpperCase = { -> 'nope' }
t(15) { 'abc'.toUpperCase() }
"####;
    let expected = r####"1: HI!
2: G1!
3: [x]
4: 10
5: hi 1
6: hi 1
7: only me
8: groovy.lang.MissingMethodException No signature of method: solo for class: P is applicable for argument types: () values: []
9: 9
10: 42
11: replaced
12: everyone everyone
13: HandleMetaClass
14: ExpandoMetaClass
15: nope
"####;
    check(src, expected);
}

#[test]
fn annotated_category() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
class StrCat { static String shout(String s) { s.toUpperCase() + '!' }; static int twice(Integer i) { i * 2 } }
t(1) { use(StrCat) { 'a'.shout() + 3.twice() } }
t(2) { use(StrCat) { [1, 2].collect { it.twice() } } }
t(3) { 'a'.shout() }
t(4) { use(StrCat) { use(StrCat) { 'n'.shout() } } }
@Category(String) class Rev { String rev() { this.reverse() } }
t(5) { use(Rev) { 'abc'.rev() } }
t(6) { def sb = new StringBuilder(); sb << 'a' << 1 << [2]; sb.toString() }
t(7) { def w = new StringWriter(); w << 'x'; w.write('y'); w.toString() }
t(8) { 'a b c'.split(' ').collectEntries { [it, it.toUpperCase()] } }
"####;
    let expected = r####"1: A!6
2: [2, 4]
3: groovy.lang.MissingMethodException No signature of method: shout for class: java.lang.String is applicable for argument types: () values: []
4: N!
5: cba
6: a1[2]
7: xy
8: [a:A, b:B, c:C]
"####;
    check(src, expected);
}

#[test]
fn json_output_and_slurper() {
    let src = r####"import groovy.json.*
println JsonOutput.toJson([1, 'a', null, [x: [y: 'z']], 1.5d, 2.50, 10L, 1e20d, 'é\u0001\t/'])
println JsonOutput.toJson('s')
println JsonOutput.toJson(null)
println JsonOutput.toJson([:])
println JsonOutput.toJson([])
class P { int x = 1; String n = 'q'; def getZ() { 3 } }
println JsonOutput.toJson(new P())
println JsonOutput.toJson(['a', 'b'] as Set)
println JsonOutput.toJson(1..3)
println JsonOutput.toJson([k: "g${1}"])
println JsonOutput.prettyPrint('{"a":{},"b":[],"c":[{"d":"x, y"}],"e":"a\\"b"}')
println JsonOutput.toJson(3.0f)
println JsonOutput.toJson(123456789012345678901234567890)
def s = new JsonSlurper()
def r = s.parseText('{"i":1,"l":12345678901,"b":123456789012345678901234567890,"d":1.25,"e":1e3,"n":null,"t":true,"s":"a\\u0041\\n","a":[],"o":{}}')
println r
println r.collect { k, v -> "$k:${v?.getClass()?.simpleName}" }
println r.getClass().name
println s.parseText('[1, [2, [3]]]')
println s.parseText('"str"')
println s.parseText('42')
try { s.parseText('{bad') } catch (e) { println e.getClass().name }
println s.parseText('{"a":-0.5,"b":-3}')
"####;
    let expected = r####"[1,"a",null,{"x":{"y":"z"}},1.5,2.50,10,1.0E20,"\u00e9\u0001\t/"]
"s"
null
{}
[]
{"x":1,"n":"q","z":3}
["a","b"]
[1,2,3]
{"k":"g1"}
{
    "a": {
        
    },
    "b": [
        
    ],
    "c": [
        {
            "d": "x, y"
        }
    ],
    "e": "a\"b"
}
3.0
123456789012345678901234567890
[i:1, l:12345678901, b:123456789012345678901234567890, d:1.25, e:1E+3, n:null, t:true, s:aA
, a:[], o:[:]]
[i:Integer, l:Long, b:BigInteger, d:BigDecimal, e:BigDecimal, n:null, t:Boolean, s:String, a:ArrayList, o:LazyMap]
org.apache.groovy.json.internal.LazyMap
[1, [2, [3]]]
str
42
groovy.json.JsonException
[a:-0.5, b:-3]
"####;
    check(src, expected);
}

#[test]
fn json_builder() {
    let src = r####"import groovy.json.*
def j = new JsonBuilder()
j { name 'x'; n 1 }
println j.toString()
def k = new JsonBuilder()
k.person { name 'Ann'; age 30; tags 'a', 'b'; address { city 'X' }; empty() }
println k
println k.toPrettyString()
def m = new JsonBuilder([1, [a: 2]])
println m.toString()
def q = new JsonBuilder()
q.root(id: 7) { inner true }
println q
println q.content
println j.getClass().name
"####;
    let expected = r####"{"name":"x","n":1}
{"person":{"name":"Ann","age":30,"tags":["a","b"],"address":{"city":"X"},"empty":[]}}
{
    "person": {
        "name": "Ann",
        "age": 30,
        "tags": [
            "a",
            "b"
        ],
        "address": {
            "city": "X"
        },
        "empty": [
            
        ]
    }
}
[1,{"a":2}]
{"root":{"id":7,"inner":true}}
[root:[id:7, inner:true]]
groovy.json.JsonBuilder
"####;
    check(src, expected);
}

#[test]
fn gstring_is_a_runtime_type() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
def x = 1
t(1) { "a${x}".getClass().name }
t(2) { "a${x}" == 'a1' }
t(3) { "a${x}".equals('a1') }
t(4) { 'a1'.equals("a${x}") }
t(5) { "a${x}".hashCode() == 'a1'.hashCode() }
t(6) { def m = [("k${x}"): 1]; m.containsKey('k1') }
t(7) { def m = ['k1': 1]; m["k${x}"] }
t(9) { ["a${x}"].contains('a1') }
t(10) { "a${x}" instanceof String }
t(11) { "a${x}" instanceof GString }
t(12) { "a${x}" instanceof CharSequence }
t(13) { def g = "a${x}"; g.strings.toList() + g.values.toList() }
t(14) { def y = 1; def g = "v=${-> y}"; y = 2; g.toString() }
t(15) { def y = 1; def g = "v=${y}"; y = 2; g.toString() }
t(16) { "a${x}" + 'b' }
t(17) { ("a${x}" + 'b').getClass().name }
t(18) { 'b' + "a${x}" }
t(19) { ('b' + "a${x}").getClass().name }
t(20) { "a${x}".length() }
t(21) { def g = "a${x}"; g.valueCount }
t(22) { "plain".getClass().name }
t(23) { "a${x}".toString().getClass().name }
t(24) { String s = "a${x}"; s.getClass().name }
t(25) { "a${x}" <=> 'a1' }
t(26) { ["a${x}", 'a1'].unique().size() }
t(27) { def m = [:]; m["k${x}"] = 1; m['k1'] = 2; m.size() }
t(28) { "a${x}".is("a${x}") }
t(29) { def l = []; l << "${x}"; l*.getClass()*.simpleName }
t(30) { switch ("a${x}") { case 'a1': return 'matched'; default: return 'no' } }
t(31) { "${x}${x}".bytes.length }
t(32) { "a${x}".toUpperCase().getClass().name }
t(33) { ["a${x}"].contains("a${x}") }
t(34) { ["a${x}"].indexOf("a${x}") }
"####;
    let expected = r####"1: org.codehaus.groovy.runtime.GStringImpl
2: true
3: false
4: false
5: false
6: false
7: 1
9: false
10: false
11: true
12: true
13: [a, , 1]
14: v=2
15: v=1
16: a1b
17: org.codehaus.groovy.runtime.GStringImpl
18: ba1
19: java.lang.String
20: 2
21: 1
22: java.lang.String
23: java.lang.String
24: java.lang.String
25: 0
26: 1
27: 1
28: false
29: [GStringImpl]
30: matched
31: 2
32: java.lang.String
33: true
34: 0
"####;
    check(src, expected);
}

#[test]
fn gstring_concatenation_and_members() {
    let src = r####"def x = 1
println "a${x}".hashCode()
println 'a1'.hashCode()
println(("a${x}" + 1).getClass().name)
println(("a${x}" + "b${x}").getClass().name)
println(("a${x}" + "b${x}").values.toList())
println(("a${x}" + 'z').strings.toList())
println("a${x}".equals("a${x}"))
println("a${x}".compareTo('a1'))
println("${x}".strings.toList())
println("a${x}${x}b".strings.toList())
println((1 + "a${x}").getClass().name)
println(("a${x}" * 2).getClass().name)
println("a${x}".plus('q').getClass().name)
"####;
    let expected = r####"3093
3056
java.lang.String
org.codehaus.groovy.runtime.GStringImpl
[1, 1]
[a, z]
true
0
[, ]
[a, , b]
java.lang.String
java.lang.String
org.codehaus.groovy.runtime.GStringImpl
"####;
    check(src, expected);
}

#[test]
fn gstring_where_a_string_is_expected() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
def x = 7; def e = ''; def sep = ','; def k = 'a'
t(1) { "${x}" as Integer }
t(2) { Integer.parseInt("${x}") }
t(3) { ("a${x}" =~ /a(\d)/)[0][1] }
t(4) { "a${x}" ==~ /a\d/ }
t(5) { 'abc'.contains("b${e}") }
t(6) { [1, 2].join("${sep}") }
t(7) { 'a,b'.split("${sep}").toList() }
t(8) { [a: 1]["${k}"] }
t(9) { 'abc'.indexOf("c${e}") }
t(10) { 'x-y'.replace("-${e}", '+') }
t(11) { "${x}".toInteger() + 1 }
t(12) { "${x}".padLeft(3, '0') }
t(13) { new StringBuilder("s${x}").append("t${x}").toString() }
t(14) { String.format("%s!", "v${x}") }
t(15) { "a${x}".startsWith("a${e}") }
t(16) { ['a7': 1].get("a${x}") }
t(17) { ['a7': 1].containsKey("a${x}") }
t(18) { def l = ['a7']; l.indexOf("a${x}") }
t(19) { def m = [:]; m.put("q${x}", 1); m.keySet()*.getClass()*.simpleName }
t(20) { "a${x}".toList() }
t(21) { "${x}".each { print it }; '' }
t(22) { def s = "a${x}"; s += 'b'; s.getClass().simpleName }
t(23) { def s = 'a'; s += "${x}"; s.getClass().simpleName }
t(24) { "a${x}" in ['a7'] }
t(25) { ['a7'].contains("a${x}".toString()) }
t(26) { "${x}".class.simpleName }
t(27) { "abc".tr("${'a'}", 'z') }
t(28) { "${x}".asBoolean() }
t(29) { "${e}" ? 'y' : 'n' }
t(30) { [("${k}"): 1].a }
t(31) { def m = [("${k}"): 1]; m["a"] }
t(32) { "x${x}".size() }
t(33) { "${[1, 2]}" }
"####;
    let expected = r####"1: 7
2: 7
3: 7
4: true
5: true
6: 1,2
7: [a, b]
8: 1
9: 2
10: x+y
11: 8
12: 007
13: s7t7
14: v7!
15: true
16: null
17: false
18: -1
19: [GStringImpl]
20: [a, 7]
721: 
22: GStringImpl
23: String
24: false
25: true
26: GStringImpl
27: zbc
28: true
29: n
30: null
31: null
32: 2
33: [1, 2]
"####;
    check(src, expected);
}

#[test]
fn comparator_tree_set_operators() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
def mk = { def s = new TreeSet(String.CASE_INSENSITIVE_ORDER); s.addAll(['b', 'A', 'c']); s }
t(1) { mk() + ['a', 'D'] }
t(2) { (mk() + ['a', 'D']).getClass().name }
t(3) { mk() - ['B'] }
t(4) { def s = mk(); s.retainAll(['C', 'a']); s }
t(5) { def s = mk(); s.removeAll(['C']); s }
t(6) { mk().intersect(['B', 'x']) }
t(7) { mk().plus('Z') }
t(8) { mk() == (['a', 'b', 'c'] as Set) }
t(9) { mk().first() + mk().last() }
t(10) { mk().findAll { it < 'c' } }
t(11) { mk().collect { it } }
t(12) { mk().size() }
"####;
    let expected = r####"1: [A, b, c, D]
2: java.util.TreeSet
3: [A, c]
4: []
5: [A, b]
6: []
7: [A, b, c, Z]
8: false
9: Ac
10: [A, b]
11: [A, b, c]
12: 3
"####;
    check(src, expected);
}

#[test]
fn meta_class_responds_to_constructor_and_left_shift() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
class P { def x = 1; def m() { 'm' }; def m(a) { 'm1' }; P() {}; P(int v) { x = v } }
t(1) { new P().respondsTo('m').size() }
t(2) { new P().respondsTo('m', 1).size() }
t(3) { new P().respondsTo('zz').size() }
t(4) { new P().respondsTo('m') ? 'y' : 'n' }
t(5) { P.metaClass.respondsTo(new P(), 'm').size() }
t(6) { new P().hasProperty('x') != null }
t(7) { P.metaClass.hasProperty(new P(), 'x') != null }
t(8) { new P().hasProperty('q') }
P.metaClass.extra << { -> 'ex' }
t(9) { new P().extra() }
t(10) { new P().respondsTo('extra').size() }
P.metaClass.constructor << { String s -> new P(s.size()) }
t(11) { new P('abcd').x }
t(12) { new P(7).x }
P.metaClass.static.st << { -> 'static!' }
t(13) { P.st() }
String.metaClass.twice << { -> delegate * 2 }
t(14) { 'ab'.twice() }
"####;
    let expected = r####"1: 2
2: 1
3: 0
4: y
5: 2
6: true
7: true
8: null
9: ex
10: 1
11: 4
12: 7
13: static!
14: abab
"####;
    check(src, expected);
}

#[test]
fn category_applies_only_to_its_type() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
@Category(String) class Rev { String rev() { this.reverse() } }
@Category(Integer) class Dbl { def dbl() { this * 2 } }
t(1) { use(Rev) { 'abc'.rev() } }
t(2) { use(Rev) { 5.rev() } }
t(3) { use(Rev, Dbl) { 'ab'.rev() + 4.dbl() } }
t(4) { use(Dbl) { 'x'.dbl() } }
"####;
    let expected = r####"1: cba
2: groovy.lang.MissingMethodException No signature of method: rev for class: java.lang.Integer is applicable for argument types: () values: []
3: ba8
4: groovy.lang.MissingMethodException No signature of method: dbl for class: java.lang.String is applicable for argument types: () values: []
"####;
    check(src, expected);
}

#[test]
fn file_text_and_readers() {
    let src = r####"def base = new File(System.getProperty('java.io.tmpdir'), 'groovyrs_file_' + System.nanoTime())
base.mkdirs()
def d = base.path
def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)?.replaceAll(/\/.*groovyrs_file_\d+/, 'D')}" } }
def f = new File(d, 'a.txt')
t(1) { f.exists() }
t(2) { f.text = 'one\ntwo\n'; f.text }
t(3) { f.readLines() }
t(4) { f << 'three'; f.text.size() }
t(5) { def r = []; f.eachLine { r << it }; r }
t(6) { def r = []; f.eachLine { l, n -> r << n }; r }
t(7) { [f.name, f.path.replace(d, 'D'), f.parent.replace(d, 'D'), f.isFile(), f.isDirectory(), f.length()] }
t(8) { new File(d + '/nope.txt').text }
t(9) { f.write('w'); f.text }
t(10) { f.append('x'); f.append('y'); f.text }
t(11) { new File(d).isDirectory() }
t(12) { new File(d, 'sub').mkdirs(); new File(d, 'sub').exists() }
t(13) { new File(d).list().toList().sort() }
t(14) { f.toString().replace(d, 'D') }
t(15) { f.getClass().name }
t(16) { def g = new File(d, 'b.txt'); g.withWriter { w -> w << 'p'; w.write('q\nr') }; g.text }
t(17) { new File(d, 'b.txt').withReader { [it.readLine(), it.readLine(), it.readLine()] } }
t(18) { new File(d, 'b.txt').bytes.length }
t(19) { def g = new File(d, 'c.txt'); g.text = 'a\r\nb'; g.readLines() }
t(20) { new File('rel/x.txt').path + ' ' + new File('rel/x.txt').name + ' ' + new File('rel/x.txt').parent }
t(21) { new groovy.json.JsonSlurper().parse(new File(d, 'j.json').with { it.text = '{"k":[1]}'; it }) }
t(22) { new groovy.json.JsonSlurper().parse(new StringReader('[true]')) }
t(23) { new StringReader('x\ny').readLines() }
['a.txt', 'b.txt', 'c.txt', 'j.json', 'sub'].each { new File(d, it).delete() }
println base.delete()
"####;
    let expected = r####"1: false
2: one
two

3: [one, two]
4: 13
5: [one, two, three]
6: [1, 2, 3]
7: [a.txt, D/a.txt, D, true, false, 13]
8: java.io.FileNotFoundException D/nope.txt (No such file or directory)
9: w
10: wxy
11: true
12: true
13: [a.txt, sub]
14: D/a.txt
15: java.io.File
16: pq
r
17: [pq, r, null]
18: 4
19: [a, b]
20: rel/x.txt x.txt rel
21: [k:[1]]
22: [true]
23: [x, y]
true
"####;
    check(src, expected);
}

#[test]
fn list_spread_into_a_function() {
    let src = r####"SN = this.getClass().name
def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)?.replace(SN, 'S')}" } }
def f(int a) { a * 2 }
def g(String a, int b) { a * b }
def h(int a, int b) { a + b }
t(1) { f([7]) }
t(2) { g(['ab', 2]) }
t(3) { f(['x']) }
t(4) { f([1, 2]) }
t(5) { h([3, 4]) }
t(6) { f(7) }
t(7) { def l = [5]; f(l) }
"####;
    let expected = r####"1: 14
2: abab
3: groovy.lang.MissingMethodException No signature of method: f for class: S is applicable for argument types: (ArrayList) values: [[x]]
4: groovy.lang.MissingMethodException No signature of method: f for class: S is applicable for argument types: (ArrayList) values: [[1, 2]]
5: 7
6: 14
7: 10
"####;
    check(src, expected);
}

#[test]
fn regex_matchers() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
t(1) { def m = 'a1b22c333' =~ /(\d+)/; [m.count, m[0], m[1][1], m.size()] }
t(2) { ('foo=1, bar=2' =~ /(\w+)=(\d+)/).collect { it[1] + ':' + it[2] } }
t(3) { 'x' ==~ /x|y/ }
t(4) { def m = 'ab' =~ /(?<first>a)(?<second>b)/; m.matches(); m.group('second') }
t(5) { 'a-b-c'.replaceAll(/-(\w)/) { all, ch -> ch.toUpperCase() } }
t(6) { 'hello world'.find(/o\w*/) }
t(7) { 'hello world'.findAll(/o./) }
t(8) { 'a1b2'.replaceFirst(/\d/, '#') }
t(9) { def p = ~/(\d)(\d)/; def m = p.matcher('12 34'); def r = []; while (m.find()) r << m.group(2); r }
t(10) { ('2024-01-15' =~ /(\d+)-(\d+)-(\d+)/)[0][1..3] }
t(11) { 'abc' =~ /z/ ? 'y' : 'n' }
t(12) { switch ('abc') { case ~/a.c/: return 'rx'; default: return 'no' } }
t(13) { 'a,b;c'.split(/[,;]/).toList() }
t(14) { def m = 'x=1' =~ /(x)=(\d)/; m[0].size() }
t(15) { ('aaa' =~ /a/).replaceAll('b') }
t(16) { 'Hello'.matches(/H.*/) }
t(17) { (~/a+/).getClass().name }
t(18) { ('a' =~ /a/).getClass().name }
t(19) { 'tel: 555-1234'.find(/(\d{3})-(\d{4})/) { full, a, b -> b + a } }
t(20) { 'one two'.eachMatch(/\w+/) { print it[0].toUpperCase() + ' ' }; '' }
"####;
    let expected = r####"1: [3, [1, 1], 22, 3]
2: [foo:1, bar:2]
3: true
4: b
5: aBC
6: o
7: [o , or]
8: a#b2
9: [2, 4]
10: [2024, 01, 15]
11: n
12: rx
13: [a, b, c]
14: 3
15: bbb
16: true
17: java.util.regex.Pattern
18: java.util.regex.Matcher
19: 1234555
O T 20: 
"####;
    check(src, expected);
}

#[test]
fn closure_combinators() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
def add = { a, b, c -> a + b * c }
t(1) { add.curry(1)(2, 3) }
t(2) { add.rcurry(10)(1, 2) }
t(3) { add.ncurry(1, 5)(1, 2) }
t(4) { def calls = 0; def sq = { calls++; it * it }.memoize(); sq(3); sq(3); sq(4); calls }
t(5) { def fact; fact = { n, acc = 1G -> n <= 1 ? acc : fact.trampoline(n - 1, acc * n) }.trampoline(); fact(20) }
t(6) { def f = { it + 1 } >> { it * 2 }; f(3) }
t(7) { def f = { it + 1 } << { it * 2 }; f(3) }
t(8) { [1, 2].with { size() } }
t(9) { new StringBuilder().tap { append('a'); append('b') }.toString() }
t(10) { add.maximumNumberOfParameters }
t(11) { add.curry(1).maximumNumberOfParameters }
t(12) { def m = { a -> a * 2 }.memoizeAtMost(2); [m(1), m(2), m(1)] }
t(13) { def c = { x -> x }; c.curry(5)() }
t(14) { { String a, int b -> a * b }.parameterTypes*.simpleName }
t(15) { def fib; fib = { n -> n < 2 ? n : fib(n - 1) + fib(n - 2) }.memoize(); fib(30) }
t(16) { def c = { -> delegate.size() }; c.delegate = [1, 2, 3]; c() }
t(17) { def c = { it * 2 }; c.call(4) + c(1) }
t(18) { def c = { a, b -> a + b }; c.curry(1).curry(2)() }
t(19) { def comp = { it * 2 } >> { it + 1 } >> { "r$it" }; comp(5) }
t(20) { (1..3).collect({ it * 2 } >> { it + 1 }) }
"####;
    let expected = r####"1: 7
2: 21
3: 11
4: 2
5: 2432902008176640000
6: 8
7: 7
8: 2
9: ab
10: 3
11: 2
12: [2, 4, 2]
13: 5
14: [String, int]
15: 832040
16: 3
17: 10
18: 3
19: r11
20: [3, 5, 7]
"####;
    check(src, expected);
}

#[test]
fn ast_transform_annotations() {
    let src = r####"import groovy.transform.*
def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
@ToString class A { int x; String s }
@ToString(includeNames = true) class B { int x; String s }
@EqualsAndHashCode class C { int x }
@Canonical class D { int a; List b }
@Immutable class E { int a; String b }
@TupleConstructor class F { int a; int b = 5 }
@ToString(includePackage = false, ignoreNulls = true) class G { String a; String b }
t(1) { new A(x: 1, s: 'q') }
t(2) { new B(x: 1, s: 'q') }
t(3) { new C(x: 1) == new C(x: 1) }
t(4) { new C(x: 1).hashCode() == new C(x: 1).hashCode() }
t(5) { new D(1, [2]) }
t(6) { new D(1, [2]) == new D(1, [2]) }
t(7) { new E(1, 'z') }
t(8) { def e = new E(1, 'z'); e.copyWith(b: 'w') }
t(9) { new F(1) .with { [a, b] } }
t(10) { new G(a: 'x') }
t(11) { new E(a: 2, b: 'q').hashCode() == new E(2, 'q').hashCode() }
t(12) { new D(a: 3) }
t(13) { [new C(x: 1), new C(x: 1)].unique().size() }
t(14) { new E(1, 'z').with { it.a } }
@ToString(excludes = 'b') class H { int a; int b }
t(15) { new H(a: 1, b: 2) }
@Sortable class S { String n; int v }
t(16) { [new S(n: 'b', v: 1), new S(n: 'a', v: 2)].sort()*.n }
@AutoClone class AC { List l = [1] }
t(17) { def a = new AC(); def b = a.clone(); b.l.is(a.l) }
"####;
    let expected = r####"1: A(1, q)
2: B(x:1, s:q)
3: true
4: true
5: D(1, [2])
6: true
7: E(1, z)
8: groovy.lang.MissingMethodException No signature of method: copyWith for class: E is applicable for argument types: (LinkedHashMap) values: [[b:w]]
9: [1, 5]
10: G(x)
11: true
12: D(3, null)
13: 1
14: 1
15: H(1)
16: [a, b]
17: false
"####;
    check(src, expected);
}

#[test]
fn annotations_on_script_members() {
    let src = r####"import groovy.transform.*
@Memoized def slow(int n) { n * 2 }
println slow(3)
@CompileStatic
int add(int a, int b) { a + b }
println add(1, 2)
@Field def counter = 5
println counter
@SuppressWarnings('unused') def x = 3
println x
@Field def c = 5
@Field List seen = []
def f() { seen << c; c + 1 }
println f()
println seen
c = 9
println f()
"####;
    let expected = r####"6
3
5
3
6
[5]
10
"####;
    check(src, expected);
}

#[test]
fn markup_builder() {
    let src = r####"import groovy.xml.MarkupBuilder
def w = new StringWriter()
def b = new MarkupBuilder(w)
b.html {
  head { title('T & <x>') }
  body(class: 'main', id: "b'1\"") {
    p 'one'
    br()
    ul {
      [1, 2].each { n -> li(n) }
    }
    div(a: 1) { span(b: 2) }
    empty(k: 'v')
  }
}
println w.toString()
println '---'
def w2 = new StringWriter()
new MarkupBuilder(w2).item(id: 3, 'text')
println w2
def w3 = new StringWriter()
new MarkupBuilder(w3).a { bb { c("deep") } }
println w3
"####;
    let expected = r####"<html>
  <head>
    <title>T &amp; &lt;x&gt;</title>
  </head>
  <body class='main' id='b&apos;1"'>
    <p>one</p>
    <br />
    <ul>
      <li>1</li>
      <li>2</li>
    </ul>
    <div a='1'>
      <span b='2' />
    </div>
    <empty k='v' />
  </body>
</html>
---
<item id='3'>text</item>
<a>
  <bb>
    <c>deep</c>
  </bb>
</a>
"####;
    check(src, expected);
}

#[test]
fn instance_map_keys_render_through_to_string() {
    // An enum constant or an object with its own `toString` as a map key (and
    // inside `inspect()`) renders through `toString`, not as a heap handle.
    let src = r####"enum S { A, B, C }
def m = [(S.A): 1, (S.B): 2]
println m
println "m=$m"
println String.format("%s", m)
println m.entrySet().toList()[0]
println m.inspect()
println([S.A, S.C].inspect())
class P { int v; String toString() { "P$v" } }
println([(new P(v: 1)): 'x'])
println([new P(v: 2)].inspect())
println([(S.C): [(S.A): 1]])
println new TreeMap([(S.B): 1, (S.A): 2])
"####;
    let expected = r####"[A:1, B:2]
m=[A:1, B:2]
{A=1, B=2}
A=1
[A:1, B:2]
[A, C]
[P1:x]
[P2]
[C:[A:1]]
[A:2, B:1]
"####;
    check(src, expected);
}

#[test]
fn unresolved_bare_call_is_missing_method_exception() {
    // A bare call nothing binds is a catchable `MissingMethodException`: on the
    // script class at the top level and in its closures, on the instance in a
    // class member (so a trait reaches the implementing class and
    // `methodMissing` answers), on a `with` delegate inside `with`.
    let src = r####"try { foo(1) } catch (MissingMethodException e) { println e.message.readLines()[0].replace(getClass().name, "Script"); println e.method; println e.arguments.toList() }
try { bar() } catch (MissingMethodException e) { println e.message.readLines()[0].replace(getClass().name, "Script") }
def c = { qux(2, 'a') }
try { c() } catch (MissingMethodException e) { println e.message.readLines()[0].replace(getClass().name, "Script") }
try { [a: 1].with { yy(1) } } catch (MissingMethodException e) { println e.message.readLines()[0].replace(getClass().name, "Script") }
class K { def m() { nope(3) } }
try { new K().m() } catch (MissingMethodException e) { println e.message.readLines()[0].replace(getClass().name, "Script") }
class W { def m() { [1].collect { inner(it) } } }
try { new W().m() } catch (MissingMethodException e) { println e.message.readLines()[0].replace(getClass().name, "Script") }
trait T { def greet() { "hi " + name() } }
class U implements T { def name() { 'u' } }
println new U().greet()
class M { def methodMissing(String n, args) { "mm:$n$args" }; def go() { zork(1) } }
println new M().go()
println "after"
"####;
    let expected = r####"No signature of method: foo for class: Script is applicable for argument types: (Integer) values: [1]
foo
[1]
No signature of method: bar for class: Script is applicable for argument types: () values: []
No signature of method: qux for class: Script is applicable for argument types: (Integer, String) values: [2, a]
No signature of method: yy for class: java.util.LinkedHashMap is applicable for argument types: (Integer) values: [1]
No signature of method: nope for class: K is applicable for argument types: (Integer) values: [3]
No signature of method: inner for class: W is applicable for argument types: (Integer) values: [1]
hi u
mm:zork[1]
after
"####;
    check(src, expected);
}

/// A GString renders where text is needed (method receiver, `String` local, concatenation) and stays lazy over a mutable value or a `{ -> }` part.
#[test]
fn round6_gstring_text_contexts() {
    let src = r####"def x = 5
def g = "v$x"
println g.toUpperCase()
println g.getClass().name
String s = "a$x"
println s.getClass().name
println "${x}".size()
def l = [1]
def lazy = "$l"
l << 2
println lazy
def cl = "n=${-> x}"
x = 9
println cl
println g == 'v5'
println g.equals('v5')
println g.hashCode() == 'v5'.hashCode()
println((g + '!').getClass().name)
"####;
    let expected = r####"V5
org.codehaus.groovy.runtime.GStringImpl
java.lang.String
1
[1, 2]
n=9
true
false
false
org.codehaus.groovy.runtime.GStringImpl
"####;
    check(src, expected);
}

/// A GString and a String of the same text are different set members: `GStringImpl.equals` is false against a `String`.
#[test]
fn round6_gstring_set_membership() {
    let src = r####"def x = 1
def s = [] as Set; s << "a${x}"
println s.contains('a1')
println s.contains("a${x}")
s << 'a1'
println s.size()
println((['a1', "a${x}"] as Set).size())
def h = new HashSet(); h.add("a${x}"); println h.contains('a1')
println(['a1'].toSet().contains("a${x}"))
println("a${x}" in ['a1'])
"####;
    let expected = r####"false
true
2
2
false
false
false
"####;
    check(src, expected);
}

/// A comparator `TreeSet` answers `retainAll` / `intersect` / `disjoint` / `containsAll` through its comparator.
#[test]
fn round6_comparator_treeset_ops() {
    let src = r####"def mk = { def s = new TreeSet(String.CASE_INSENSITIVE_ORDER); s.addAll(['b', 'A', 'c']); s }
def s = mk(); println s.retainAll(['C', 'a']); println s
def i = mk().intersect(['B', 'x', 'c']); println i; println i.getClass().name
def j = mk().intersect(['B']); j << 'b'; println j
println(mk().disjoint(['Q']))
println(mk().containsAll(['a', 'B']))
println(mk().containsAll(['a', 'z']))
"####;
    let expected = r####"true
[]
[c]
java.util.TreeSet
[b]
true
true
false
"####;
    check(src, expected);
}

/// `eachFile` / `eachDir` / `eachFileRecurse` / `eachDirRecurse`, `listFiles`, `renameTo`, `deleteDir`.
#[test]
fn round6_file_traversal() {
    let src = r####"def base = new File(System.getProperty('java.io.tmpdir'), 'groovyrs_walk_' + System.nanoTime())
new File(base, 'd1/d2').mkdirs()
new File(base, 'a.txt').text = 'a'
new File(base, 'd1/b.txt').text = 'b'
new File(base, 'd1/d2/c.txt').text = 'c'
def rel = { File f -> f.path.substring(base.path.size() + 1) }
def r = []; base.eachFile { r << rel(it) }; println r.sort()
r = []; base.eachDir { r << rel(it) }; println r.sort()
r = []; base.eachFileRecurse { r << rel(it) }; println r.sort()
r = []; base.eachDirRecurse { r << rel(it) }; println r.sort()
println base.listFiles().collect { it.name }.sort()
println new File(base, 'a.txt').renameTo(new File(base, 'z.txt'))
println new File(base, 'z.txt').exists()
println base.deleteDir()
println base.exists()
"####;
    let expected = r####"[a.txt, d1]
[d1]
[a.txt, d1, d1/b.txt, d1/d2, d1/d2/c.txt]
[d1, d1/d2]
[a.txt, d1]
true
true
true
false
"####;
    check(src, expected);
}

/// `MarkupBuilder`: empty closures, `mkp.yield` / `yieldUnescaped` / `comment` / `pi`, the quoting options, quoted (prefixed) element names, and a paren-less `item i` call.
#[test]
fn round6_markup_extras() {
    let src = r####"import groovy.xml.MarkupBuilder
def run = { Closure c -> def w = new StringWriter(); def b = new MarkupBuilder(w); c(b); println w.toString(); println '--' }
run { b -> b.r { e {} } }
run { b -> b.p { mkp.yield 'a<b'; mkp.yieldUnescaped '<i/>'; span 'x' } }
run { b -> b.doubleQuotes = true; b.a(href: "x'y") }
run { b -> b.escapeAttributes = false; b.a(href: '<&>') }
run { b -> b.r { mkp.comment 'note' } }
run { b -> b.x(a: null, b: 1) }
run { b -> b.'my-tag'(1) }
run { b -> b.r { 3.times { i -> item i } } }
run { b -> b.omitNullAttributes = true; b.x(a: null, b: 1) }
run { b -> b.r('t') { c 1 } }
run { b -> b.'x:root'('xmlns:x': 'urn:x') { 'x:item'(id: 1, 'v'); 'y:other'() } }
run { b -> b.root { mkp.pi(xml: [version: '1.0']); a('x') } }
"####;
    let expected = r####"<r>
  <e />
</r>
--
<p>a&lt;b<i/>
  <span>x</span>
</p>
--
<a href="x'y" />
--
<a href='<&>' />
--
<r><!-- note --></r>
--
<x a='' b='1' />
--
<my-tag>1</my-tag>
--
<r>
  <item>0</item>
  <item>1</item>
  <item>2</item>
</r>
--
<x b='1' />
--
<r>t
  <c>1</c>
</r>
--
<x:root xmlns:x='urn:x'>
  <x:item id='1'>v</x:item>
  <y:other />
</x:root>
--
<root>
  <?xml version='1.0'?>
<a>x</a>
</root>
--
"####;
    check(src, expected);
}

/// `java.util.Date`: arithmetic in days, ordering, ranges, `format` in an explicit zone, and `Date.parse`.
#[test]
fn round6_date_basics() {
    let src = r####"def d = new Date(86400000L * 365)
def utc = TimeZone.getTimeZone('UTC')
println d.time
println d.format('yyyy-MM-dd HH:mm:ss', utc)
println((d + 1).format('yyyy-MM-dd', utc))
println((d - 1).format('yyyy-MM-dd', utc))
println((d + 10) - d)
println d.before(d + 1)
println(d < (d + 1))
println(new Date(0) == new Date(0))
println d.getClass().name
println d.format("EEE, d MMM yyyy 'T' hh:mm a", utc)
println d.next().format('dd', utc)
println utc.ID
println d.format('yyyy-MM-dd HH:mm:ss.SSS Z', TimeZone.getTimeZone('GMT+05:30'))
println d.format('yy MMM MMMM EEEE', utc)
println new Date(1500).compareTo(new Date(0))
println(([new Date(1)] as Set).contains(new Date(1)))
println((d..(d + 2)).size())
println((d..(d + 2)).collect { it.format('MM-dd', utc) })
println(((d + 2)..d).collect { it.format('MM-dd', utc) })
println([d + 1, d].sort().collect { it.format('MM-dd', utc) })
println Date.parse('yyyy-MM-dd', '2020-03-04').format('yyyy/MM/dd')
"####;
    let expected = r####"31536000000
1971-01-01 00:00:00
1971-01-02
1970-12-31
10
true
true
true
java.util.Date
Fri, 1 Jan 1971 T 12:00 AM
02
UTC
1971-01-01 05:30:00.000 +0530
71 Jan January Friday
1
true
3
[01-01, 01-02, 01-03]
[01-03, 01-02, 01-01]
[01-01, 01-02]
2020/03/04
"####;
    check(src, expected);
}

/// `groovy.time.TimeCategory` durations: their arithmetic, classes and `BaseDuration.toString`.
#[test]
fn round6_time_category() {
    let src = r####"def utc = TimeZone.getTimeZone('UTC')
use(groovy.time.TimeCategory) {
  println 1.day
  println 1.hour
  println 90.minutes
  println(2.days + 3.hours + 1.minute + 5.seconds + 7.milliseconds)
  println 0.seconds
  println 2.weeks
  println((3.days - 1.day))
  println 1.second.toMilliseconds()
  println 2.days.days
  println((new Date(0) + 1.week).time)
  println((new Date(1000) - new Date(0)).getClass().name)
  println(new Date(90061000L) - new Date(0))
  println 1.month
  println 2.years
  println 1.day.getClass().name
  println(1.day > 1.hour)
  println((new Date(0) + 90.minutes).format('HH:mm', utc))
  println((new Date(0) + 2.days).format('yyyy-MM-dd', utc))
}
println((new Date(86400000L) - new Date(0)))
"####;
    let expected = r####"1 days
1 hours
90 minutes
2 days, 3 hours, 1 minutes, 5.007 seconds
0
14 days
2 days
1000
2
604800000
groovy.time.TimeDuration
1 days, 1 hours, 1 minutes, 1.000 seconds
1 months
2 years
groovy.time.Duration
true
01:30
1970-01-03
1
"####;
    check(src, expected);
}

/// `XmlSlurper` GPath: child / attribute / `**` selection, the `GPathResult` classes, text, conversions, iteration, equality and parse errors.
#[test]
fn round6_xml_slurper() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
def xml = '''<library name="city">
  <book id="1" lang="en"><title>Dune</title><year>1965</year></book>
  <book id="2"><title>Emma &amp; Co</title><year>1815</year></book>
  <!-- note -->
  <mag>Wired</mag>
</library>'''
def r = new groovy.xml.XmlSlurper().parseText(xml)
t(1) { r.name() }
t(2) { r.@name }
t(3) { r.book.size() }
t(4) { r.book[0].title }
t(5) { r.book[1].title.text() }
t(6) { r.book*.@id }
t(7) { r.book.title*.text() }
t(8) { r.book.find { it.@id == '2' }.year }
t(9) { r.book.findAll { it.year.toInteger() > 1900 }.title }
t(10) { r.getClass().name }
t(11) { r.book.getClass().name }
t(12) { r.book[0].@lang.getClass().name }
t(13) { r.'**'.findAll { it.name() == 'title' }*.text() }
t(14) { r.children().size() }
t(15) { r.text() }
t(16) { r.nope.size() }
t(17) { r.book[0].attributes() }
t(18) { r.book.@id }
t(19) { r.mag == 'Wired' }
t(20) { 'Wired' == r.mag }
t(21) { r.book.year.sum { it.toInteger() } }
t(22) { r.book[0].year.toInteger() + 1 }
t(23) { r.depthFirst().collect { it.name() } }
t(24) { r.breadthFirst()*.name() }
t(25) { r.book[0].parent().name() }
t(26) { r.book.'@id' }
t(27) { def out = []; for (b in r.book) out << b.@id.toInteger(); out }
t(28) { r.book[-1].title }
t(29) { r.book[5].size() }
t(30) { "${r.book[0]}" }
t(31) { new groovy.xml.XmlSlurper().parseText('<a x="1 &lt; 2"><![CDATA[<raw>]]><b/></a>').with { [it.@x.text(), it.text(), it.b.size()] } }
t(32) { new groovy.xml.XmlSlurper().parseText('<a><b></a>') }
t(33) { new groovy.xml.XmlSlurper().parseText('junk') }
"####;
    let expected = r####"1: library
2: city
3: 2
4: Dune
5: Emma & Co
6: [1, 2]
7: [Dune, Emma & Co]
8: 1815
9: Dune
10: groovy.xml.slurpersupport.NodeChild
11: groovy.xml.slurpersupport.NodeChildren
12: groovy.xml.slurpersupport.Attributes
13: [Dune, Emma & Co]
14: 3
15: Dune1965Emma & Co1815Wired
16: 0
17: [id:1, lang:en]
18: 12
19: true
20: false
21: 3780
22: 1966
23: [library, book, title, year, book, title, year, mag]
24: [library, book, book, mag, title, year, title, year]
25: library
26: 12
27: [1, 2]
28: Emma & Co
29: 0
30: Dune1965
31: [1 < 2, <raw>, 1]
32: org.xml.sax.SAXParseException The element type "b" must be terminated by the matching end-tag "</b>".
33: org.xml.sax.SAXParseException Content is not allowed in prolog.
"####;
    check(src, expected);
}

/// List wrappers: `asSynchronized` / `asUnmodifiable` / `Collections.unmodifiableList` are live views, `asImmutable` a copy; `subList` and `as` keep the JDK class.
#[test]
fn round6_list_views() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
def x = [1, 2, 3]
def y = x.asSynchronized()
y[0] = 9
x << 4
t(1) { "$x $y ${y.size()} ${y.getClass().name}" }
t(2) { y.subList(1, 3) }
t(3) { y.is(x) }
t(4) { y == x }
def un = x.asUnmodifiable()
t(5) { un.add(1) }
t(6) { un[0] = 1 }
t(7) { x << 5; un }
t(8) { un.getClass().name }
def c = [1]; def cu = Collections.unmodifiableList(c); c << 2
t(9) { cu }
t(10) { def im = [1]; def f = im.asImmutable(); im << 2; f }
t(11) { Collections.unmodifiableList([1, 2]).subList(0, 1) << 3 }
t(12) { Arrays.asList(1, 2, 3).subList(0, 1).getClass().name }
t(13) { List.of(1, 2).subList(0, 1).getClass().name }
t(14) { new LinkedList([1, 2]).subList(0, 1).getClass().name }
t(15) { new Vector([1, 2]).subList(0, 1).getClass().name }
t(16) { def s = Arrays.asList(1, 2, 3).subList(0, 2); s[0] = 9; s }
t(17) { Arrays.asList(1, 2, 3).subList(0, 2) << 9 }
t(18) { ([1, 2] as LinkedList).getClass().name }
t(19) { ([3, 1, 2] as PriorityQueue).getClass().name }
t(20) { LinkedList l = [1, 2]; l.getClass().name }
t(21) { def a = new LinkedList([1]); LinkedList b = a; b << 2; a }
t(22) { new LinkedList([1]).asSynchronized().getClass().name }
t(23) { new TreeSet([1]).asSynchronized().getClass().name }
t(24) { new TreeMap([a: 1]).asUnmodifiable().getClass().name }
t(25) { Collections.unmodifiableCollection([1]).getClass().name }
"####;
    let expected = r####"1: [9, 2, 3, 4] [9, 2, 3, 4] 4 java.util.Collections$SynchronizedRandomAccessList
2: [2, 3]
3: false
4: true
5: java.lang.UnsupportedOperationException null
6: java.lang.UnsupportedOperationException null
7: [9, 2, 3, 4, 5]
8: java.util.Collections$UnmodifiableRandomAccessList
9: [1, 2]
10: [1]
11: java.lang.UnsupportedOperationException null
12: java.util.AbstractList$RandomAccessSubList
13: java.util.ImmutableCollections$SubList
14: java.util.AbstractList$SubList
15: java.util.Collections$SynchronizedRandomAccessList
16: [9, 2]
17: java.lang.UnsupportedOperationException null
18: java.util.LinkedList
19: java.util.PriorityQueue
20: java.util.LinkedList
21: [1, 2]
22: java.util.Collections$SynchronizedList
23: java.util.Collections$SynchronizedNavigableSet
24: java.util.Collections$UnmodifiableNavigableMap
25: java.util.Collections$UnmodifiableCollection
"####;
    check(src, expected);
}

/// `respondsTo` / `hasProperty` on JDK values answer the metaclass `MetaMethod`s and `MetaProperty`s.
#[test]
fn round6_responds_to_jdk() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
t(1) { 'abc'.respondsTo('toUpperCase') as boolean }
t(2) { 'abc'.respondsTo('nope') }
t(3) { 'abc'.respondsTo('toUpperCase').size() }
t(4) { 'abc'.respondsTo('toUpperCase')[0].name }
t(5) { 'abc'.respondsTo('toUpperCase')[0].getClass().name }
t(6) { [1].respondsTo('each') as boolean }
t(7) { 5.respondsTo('plus', 1) as boolean }
t(8) { 'abc'.respondsTo('substring', 1).size() }
t(9) { 'abc'.respondsTo('substring').size() }
t(10) { 'abc'.respondsTo('substring', 1, 2)[0].parameterTypes*.name }
t(11) { 'abc'.metaClass.respondsTo('abc', 'size')[0].name }
t(12) { 'abc'.respondsTo('toUpperCase')[0].returnType.name }
t(13) { 'x'.respondsTo('center', 3).size() }
t(14) { 'abc'.respondsTo('toUpperCase')[0].toString() }
t(15) { 'abc'.respondsTo('size')[0].declaringClass.name }
t(16) { 'abc'.respondsTo('size')[0].isStatic() }
t(17) { new Object().respondsTo('toString').size() }
t(18) { 'abc'.hasProperty('bytes').getClass().name }
t(19) { 'abc'.hasProperty('bytes').type.name }
t(20) { 'abc'.hasProperty('zzz') }
t(21) { [1].hasProperty('empty')?.name }
t(22) { 'abc'.respondsTo('center', 3, 'x')[0].parameterTypes }
"####;
    let expected = r####"1: true
2: []
3: 2
4: toUpperCase
5: org.codehaus.groovy.reflection.CachedMethod
6: true
7: true
8: 1
9: 2
10: [int, int]
11: size
12: java.lang.String
13: 1
14: public java.lang.String java.lang.String.toUpperCase()
15: java.lang.CharSequence
16: false
17: 1
18: groovy.lang.MetaBeanProperty
19: [B
20: null
21: empty
22: [class java.lang.Number, interface java.lang.CharSequence]
"####;
    check(src, expected);
}

/// `XmlParser` nodes: `Node` / `NodeList` rendering, attributes, text, and the mixed-content depth-first walk.
#[test]
fn round6_xml_parser() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
def xml = '''<library name="city">
  <book id="1" lang="en"><title>Dune</title><year>1965</year></book>
  <book id="2"><title>Emma &amp; Co</title><year>1815</year></book>
  <mag>Wired <b>mag</b> tail</mag>
</library>'''
def p = new groovy.xml.XmlParser().parseText(xml)
t(1) { p.name() }
t(2) { p.@name }
t(3) { p.book.size() }
t(4) { p.book[0].title.text() }
t(5) { p.book[0].getClass().name }
t(6) { p.book.getClass().name }
t(7) { p.book[0].attributes() }
t(8) { p.book[0].title }
t(9) { p.book[0] }
t(10) { p.children().size() }
t(11) { p.text() }
t(12) { p.book*.@id }
t(13) { p.book[0].attribute('lang') }
t(14) { p.book.title*.text() }
t(15) { p.mag[0].value() }
t(16) { p.depthFirst()*.name() }
t(17) { p.book[0].parent().name() }
t(18) { p.'**'.title*.text() }
t(19) { p.book[0].attributes().getClass().name }
t(20) { def ids = []; p.book.each { ids << it.@id }; ids }
t(21) { p.depthFirst().collect { it instanceof String ? "S:$it" : it.name() } }
t(22) { p.breadthFirst().collect { it instanceof String ? "S:$it" : it.name() } }
"####;
    let expected = r####"1: library
2: city
3: 2
4: Dune
5: groovy.util.Node
6: groovy.util.NodeList
7: [id:1, lang:en]
8: [title[attributes={}; value=[Dune]]]
9: book[attributes={id=1, lang=en}; value=[title[attributes={}; value=[Dune]], year[attributes={}; value=[1965]]]]
10: 3
11: Dune1965Emma & Co1815Wired mag tail
12: [1, 2]
13: en
14: [Dune, Emma & Co]
15: [Wired , b[attributes={}; value=[mag]],  tail]
16: groovy.lang.MissingMethodException No signature of method: name for class: java.lang.String is applicable for argument types: () values: []
17: library
18: [Dune, Emma & Co]
19: java.util.LinkedHashMap
20: [1, 2]
21: [library, book, title, year, book, title, year, mag, S:Wired , b, S: tail]
22: [library, book, book, mag, title, year, title, year, S:Wired , b, S: tail]
"####;
    check(src, expected);
}

/// The lines of the program's standard error that start with `prefix`, each
/// ending in `\n`.
fn stderr_lines(src: &str, prefix: &str) -> String {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("groovyrs_round3_err_{}.groovy", fasthash(src)));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_groovy"))
        .arg(&path)
        .output()
        .expect("spawn groovy");
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|l| l.starts_with(prefix))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// `asSynchronized` / `asUnmodifiable` / `Collections.unmodifiableSet` & co. on a set or map are live views with the JDK class and write policy; `asImmutable` stays a copy.
#[test]
fn round7_set_map_wrapper_views() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
def s = [1, 2] as Set; def v = s.asSynchronized(); v << 3
t(1) { s }
t(2) { v.getClass().name }
s << 4
t(3) { v }
t(4) { v.contains(4) }
t(5) { v.size() }
def m = [a: 1]; def mv = m.asSynchronized(); mv.b = 2
t(6) { m }
m.c = 3
t(7) { mv }
t(8) { mv.c }
def m2 = [a: 1]; def mu = m2.asUnmodifiable(); m2.c = 3
t(9) { mu }
t(10) { mu.d = 4 }
t(11) { mu.remove('a') }
t(12) { mu.getClass().name }
def su = Collections.unmodifiableSet(s)
t(13) { su << 9 }
s << 5
t(14) { su }
def ts = new TreeSet([3, 1]); def tv = ts.asUnmodifiable(); ts << 2
t(15) { tv }
t(16) { tv.first() }
t(17) { tv.getClass().name }
t(18) { def x = [b: 2]; def y = Collections.synchronizedMap(x); y.put('a', 1); x }
t(19) { mv.each { k, val -> print k }; '' }
t(20) { mv.collect { k, val -> val } }
t(21) { v.collect { it * 2 } }
t(22) { v == s }
t(23) { mu.keySet() }
t(24) { mu.containsKey('c') }
t(25) { def fs = [1] as Set; def fi = fs.asImmutable(); fs << 2; fi }
t(26) { Collections.unmodifiableCollection([1] as Set).getClass().name }
t(27) { v.remove(1); s }
t(28) { mv.clear(); m }
"####;
    let expected = r####"1: [1, 2, 3]
2: java.util.Collections$SynchronizedSet
3: [1, 2, 3, 4]
4: true
5: 4
6: [a:1, b:2]
7: [a:1, b:2, c:3]
8: 3
9: [a:1, c:3]
10: java.lang.UnsupportedOperationException null
11: java.lang.UnsupportedOperationException null
12: java.util.Collections$UnmodifiableMap
13: java.lang.UnsupportedOperationException null
14: [1, 2, 3, 4, 5]
15: [1, 2, 3]
16: 1
17: java.util.Collections$UnmodifiableNavigableSet
18: [b:2, a:1]
abc19: 
20: [1, 2, 3]
21: [2, 4, 6, 8, 10]
22: true
23: [a, c]
24: true
25: [1]
26: java.util.Collections$UnmodifiableCollection
27: [2, 3, 4, 5]
28: [:]
"####;
    check(src, expected);
}

/// Namespace-aware XML: slurper local-name selection, `declareNamespace` prefixes, `NamespaceAwareHashMap` attributes, parser `QName` names, and the non-aware `(false, false)` parsers.
#[test]
fn round7_xml_namespaces() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
def xml = '''<r xmlns="urn:d" xmlns:a="urn:a" a:k="v" plain="p"><a:x id="1">ax</a:x><y>dy</y><a:x>ax2</a:x></r>'''
def r = new groovy.xml.XmlSlurper().parseText(xml)
t(1) { r.name() }
t(2) { r.x.size() }
t(3) { r.'a:x'.size() }
t(4) { r.y.text() }
t(5) { r.attributes() }
t(6) { r.attributes().getClass().name }
t(7) { r.@k }
t(8) { r.'@a:k' }
t(9) { r.x[0].namespaceURI() }
t(10) { r.namespaceURI() }
t(11) { r.y.namespaceURI() }
t(12) { r.lookupNamespace('a') }
t(13) { r.declareNamespace(a: 'urn:a').'a:x'.size() }
t(14) { r.x[0].name() }
t(15) { r.@plain }
def p = new groovy.xml.XmlParser().parseText(xml)
t(16) { p.name() }
t(17) { p.name().getClass().name }
t(18) { p.attributes() }
t(19) { p.x.size() }
t(20) { p.'a:x'.size() }
t(21) { p.children()[0].name() }
t(22) { p }
t(23) { p.y[0].name() }
t(24) { p.name().localPart }
t(25) { p.name().namespaceURI }
t(26) { p.children()[0].name().prefix }
t(27) { p.'**'.size() }
def q = new groovy.xml.XmlSlurper(false, false).parseText(xml)
t(28) { q.name() }
t(29) { q.'a:x'.size() }
t(30) { q.x.size() }
t(31) { q.attributes() }
def u = new groovy.xml.XmlParser(false, false).parseText(xml)
t(32) { u.name() }
t(33) { u.attributes() }
t(34) { u.'a:x'.size() }
t(35) { new groovy.xml.XmlSlurper().parseText('<a><b/></a>').b[0].attributes().getClass().name }
println r.declareNamespace(z: 'urn:a').'z:x'.size()
println r.declareNamespace(z: 'urn:d').'z:y'.size()
println r.declareNamespace(z: 'urn:d').'z:x'.size()
println r.'*'.size()
println r.x.@id
println r.@'a:k'
println p.attribute('plain')
println p.'@plain'
def qn = new groovy.namespace.QName('urn:a', 'x')
println p[qn].size()
println p.children()[0].name() == 'x'
println p.children()[0].name() == 'a:x'
println q.children()[0].name()
println new groovy.xml.XmlSlurper(false, false).parseText('<r xmlns="urn:d" xmlns:a="urn:a" a:k="v" plain="p"/>').attributes().keySet()
"####;
    let expected = r####"1: r
2: 2
3: 0
4: dy
5: [plain:p, {urn:a}k:v]
6: groovy.xml.slurpersupport.NamespaceAwareHashMap
7: 
8: v
9: urn:a
10: urn:d
11: groovy.lang.MissingMethodException No signature of method: namespaceURI for class: groovy.xml.slurpersupport.NodeChildren is applicable for argument types: () values: []
12: urn:a
13: 2
14: x
15: p
16: {urn:d}r
17: groovy.namespace.QName
18: [{urn:a}k:v, plain:p]
19: 0
20: 2
21: {urn:a}x
22: {urn:d}r[attributes={{urn:a}k=v, plain=p}; value=[{urn:a}x[attributes={id=1}; value=[ax]], {urn:d}y[attributes={}; value=[dy]], {urn:a}x[attributes={}; value=[ax2]]]]
23: {urn:d}y
24: r
25: urn:d
26: a
27: 4
28: r
29: 2
30: 0
31: [xmlns:a:urn:a, xmlns:urn:d, a:k:v, plain:p]
32: r
33: [xmlns:urn:d, xmlns:a:urn:a, a:k:v, plain:p]
34: 2
35: groovy.xml.slurpersupport.NamespaceAwareHashMap
2
1
0
3
1
v
p
p
2
false
true
a:x
[xmlns:a, xmlns, a:k, plain]
"####;
    check(src, expected);
}

/// Malformed XML raises the `SAXParseException` messages Xerces raises.
#[test]
fn round7_xml_parse_errors() {
    let src = r####"['<a><b></a>', 'junk', '<a>', '<a></b>', '<a x=1/>', '<a x="1></a>', '<a><b>text</c></a>', '<a/><b/>', '<a/>junk', '  <a>\n<b>\n</a>', '<a b="1" b="2"/>', '<a>&foo;</a>', '<a><!-- x </a>', '<1a/>', '<a>\n  <b></c>\n</a>', '', '<a x="<"/>', '<a>&#xZZ;</a>', '<?xml version="1.0"?><a></a><', '<a></a >', '<a x></a>'].each { s ->
  try { new groovy.xml.XmlSlurper().parseText(s); println "ok" } catch (e) { println "${e.getClass().name}: ${e.message}" }
}
"####;
    let expected = r####"org.xml.sax.SAXParseException: The element type "b" must be terminated by the matching end-tag "</b>".
org.xml.sax.SAXParseException: Content is not allowed in prolog.
org.xml.sax.SAXParseException: XML document structures must start and end within the same entity.
org.xml.sax.SAXParseException: The element type "a" must be terminated by the matching end-tag "</a>".
org.xml.sax.SAXParseException: Open quote is expected for attribute "x" associated with an  element type  "a".
org.xml.sax.SAXParseException: The value of attribute "x" associated with an element type "a" must not contain the '<' character.
org.xml.sax.SAXParseException: The element type "b" must be terminated by the matching end-tag "</b>".
org.xml.sax.SAXParseException: The markup in the document following the root element must be well-formed.
org.xml.sax.SAXParseException: Content is not allowed in trailing section.
org.xml.sax.SAXParseException: The element type "b" must be terminated by the matching end-tag "</b>".
org.xml.sax.SAXParseException: Attribute "b" was already specified for element "a".
org.xml.sax.SAXParseException: The entity "foo" was referenced, but not declared.
org.xml.sax.SAXParseException: XML document structures must start and end within the same entity.
org.xml.sax.SAXParseException: The markup in the document preceding the root element must be well-formed.
org.xml.sax.SAXParseException: The element type "b" must be terminated by the matching end-tag "</b>".
org.xml.sax.SAXParseException: Premature end of file.
org.xml.sax.SAXParseException: The value of attribute "x" associated with an element type "a" must not contain the '<' character.
org.xml.sax.SAXParseException: A hexadecimal representation must immediately follow the "&#x" in a character reference.
org.xml.sax.SAXParseException: XML document structures must start and end within the same entity.
ok
org.xml.sax.SAXParseException: Attribute name "x" associated with an element type "a" must be followed by the ' = ' character.
"####;
    check(src, expected);
    // The JDK parser's `[Fatal Error] :line:col:` lines, on standard error.
    let fatal = r####"[Fatal Error] :1:9: The element type "b" must be terminated by the matching end-tag "</b>".
[Fatal Error] :1:1: Content is not allowed in prolog.
[Fatal Error] :1:4: XML document structures must start and end within the same entity.
[Fatal Error] :1:6: The element type "a" must be terminated by the matching end-tag "</a>".
[Fatal Error] :1:6: Open quote is expected for attribute "x" associated with an  element type  "a".
[Fatal Error] :1:9: The value of attribute "x" associated with an element type "a" must not contain the '<' character.
[Fatal Error] :1:13: The element type "b" must be terminated by the matching end-tag "</b>".
[Fatal Error] :1:6: The markup in the document following the root element must be well-formed.
[Fatal Error] :1:5: Content is not allowed in trailing section.
[Fatal Error] :3:3: The element type "b" must be terminated by the matching end-tag "</b>".
[Fatal Error] :1:17: Attribute "b" was already specified for element "a".
[Fatal Error] :1:9: The entity "foo" was referenced, but not declared.
[Fatal Error] :1:15: XML document structures must start and end within the same entity.
[Fatal Error] :1:2: The markup in the document preceding the root element must be well-formed.
[Fatal Error] :2:8: The element type "b" must be terminated by the matching end-tag "</b>".
[Fatal Error] :1:1: Premature end of file.
[Fatal Error] :1:7: The value of attribute "x" associated with an element type "a" must not contain the '<' character.
[Fatal Error] :1:7: A hexadecimal representation must immediately follow the "&#x" in a character reference.
[Fatal Error] :1:30: XML document structures must start and end within the same entity.
[Fatal Error] :1:5: Attribute name "x" associated with an element type "a" must be followed by the ' = ' character.
"####;
    assert_eq!(stderr_lines(src, "[Fatal Error]"), fatal);
}

/// `respondsTo` on a script class answers `CachedMethod`s: declared return and parameter types, `static`, the declaring class of an override, the reflected `toString`.
#[test]
fn round7_responds_to_script_classes() {
    let src = r####"class P { def a(int x) { x }; String a(String s) { s }; static int st() { 1 }; def b() {} ; int c(Integer i, List l) { 0 } }
class Q extends P { def a(int x) { 2 } }
def p = new P()
def show = { m -> "${m.getClass().name}|${m.name}|${m.parameterTypes*.name}|${m.returnType.name}|${m.declaringClass.name}|${m.isStatic()}|$m" }
p.respondsTo('a').each { println show(it) }
println p.respondsTo('a', 1).size()
println p.respondsTo('a', 'x')*.returnType*.name
p.respondsTo('st').each { println show(it) }
p.respondsTo('c').each { println show(it) }
println p.respondsTo('nope')
new Q().respondsTo('a').each { println show(it) }
println P.respondsTo('st').size()
p.respondsTo('a')[0].parameterTypes.each { println it.getClass().name }
"####;
    let expected = r####"org.codehaus.groovy.reflection.CachedMethod|a|[int]|java.lang.Object|P|false|public java.lang.Object P.a(int)
org.codehaus.groovy.reflection.CachedMethod|a|[java.lang.String]|java.lang.String|P|false|public java.lang.String P.a(java.lang.String)
1
[java.lang.String]
org.codehaus.groovy.reflection.CachedMethod|st|[]|int|P|true|public static int P.st()
org.codehaus.groovy.reflection.CachedMethod|c|[java.lang.Integer, java.util.List]|int|P|false|public int P.c(java.lang.Integer,java.util.List)
[]
org.codehaus.groovy.reflection.CachedMethod|a|[int]|java.lang.Object|Q|false|public java.lang.Object Q.a(int)
org.codehaus.groovy.reflection.CachedMethod|a|[java.lang.String]|java.lang.String|P|false|public java.lang.String P.a(java.lang.String)
1
org.codehaus.groovy.reflection.stdclasses.IntegerCachedClass
"####;
    check(src, expected);
}

/// A script-class instance built before its class's metaclass was first injected into keeps the original metaclass.
#[test]
fn round7_emc_existing_instances() {
    let src = r####"class P {}
class Q extends P {}
def t = { o, n -> try { println "$n " + o.z() } catch (e) { println "$n " + e.getClass().simpleName } }
def p1 = new P()
def q1 = new Q()
def mc = P.metaClass
def p2 = new P()
P.metaClass.z = { -> 7 }
def p3 = new P()
def q2 = new Q()
t(p1, 'p1'); t(p2, 'p2'); t(p3, 'p3'); t(q1, 'q1'); t(q2, 'q2')
def s = 'abc'
String.metaClass.zz = { -> 'zz' }
println s.zz()
class R { def r() { 1 } }
def r1 = new R()
R.metaClass.r = { -> 2 }
println r1.r()
println new R().r()
r1.metaClass.w = { -> 3 }
println r1.w()
"####;
    let expected = r####"p1 MissingMethodException
p2 MissingMethodException
p3 7
q1 7
q2 7
zz
1
2
3
"####;
    check(src, expected);
}

/// `with` / `tap` / `identity` / `inspect` / `respondsTo` on a `GString` see the `GString`, not its text.
#[test]
fn round7_gstring_object_methods() {
    let src = r####"def x = 3
def g = "v$x"
println g.with { it.getClass().name }
println g.tap { }.getClass().name
println g.identity { it.getClass().name }
println g.inspect()
println g.asType(String).getClass().name
println g.respondsTo('getValues').size()
println g.hasProperty('values') != null
println g.compareTo('v3')
println g.toString().getClass().name
println g.intern().getClass().name
println g.getStrings().size()
println g.collect { it }.size()
println g.isCase('v3')
println g.asBoolean()
println g.toList()
"####;
    let expected = r####"org.codehaus.groovy.runtime.GStringImpl
org.codehaus.groovy.runtime.GStringImpl
org.codehaus.groovy.runtime.GStringImpl
"v3"
java.lang.String
1
true
0
java.lang.String
java.lang.String
2
2
true
true
[v, 3]
"####;
    check(src, expected);
}

/// A `def` local read only as text stores an immutable `GString` as its `String` — and keeps a lazy or mutable one a `GString`.
#[test]
fn round7_gstring_text_locals() {
    let src = r####"def x = 3
def a = "v$x"; println a.getClass().name
def b = "v$x"; println b.size()
def c = "v$x"; println c.toUpperCase(); println c.getClass().name
def d = "v$x"; def l = [d]; println l[0].getClass().name
def e = "v$x"; println(e == 'v3')
def f = "v$x"; println f
def g = "v$x"; println g.with { it.getClass().name }
def h = "v$x"; def cl = { h.getClass().name }; println cl()
def i = "v$x"; println "${i}".values[0].getClass().name
def j = "v$x"; println j.equals('v3')
def k = "v$x"; println k.length(); k = 5; println k.getClass().name
def m = "v$x"; def mm = m; println mm.getClass().name
def lst = [1]
def gs = "$lst"
lst << 2
println gs
def num = 1
def ms = "a${-> num}"
num = 2
println ms
"####;
    let expected = r####"org.codehaus.groovy.runtime.GStringImpl
2
V3
org.codehaus.groovy.runtime.GStringImpl
org.codehaus.groovy.runtime.GStringImpl
true
v3
org.codehaus.groovy.runtime.GStringImpl
org.codehaus.groovy.runtime.GStringImpl
org.codehaus.groovy.runtime.GStringImpl
false
2
java.lang.Integer
org.codehaus.groovy.runtime.GStringImpl
[1, 2]
a2
"####;
    check(src, expected);
}

/// `x in c` is `c.isCase(x)` for a class, a pattern, a closure and a set.
#[test]
fn round7_in_operator() {
    let src = r####"def t(n, Closure c) { try { def r = c(); println "$n: $r" } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
t(1) { 2 in [1, 2] }
t(2) { 2 in 1..3 }
t(3) { 'a' in 'abc' }
t(4) { 'ab' in 'xaby' }
t(5) { 'k' in [k: 1] }
t(6) { 'k' in [k: null] }
t(7) { 'k' in [k: 0] }
t(8) { 2.0 in [1, 2] }
t(9) { 2 in [1, 2.0] }
t(10) { null in [null] }
t(11) { 3 !in [1, 2] }
t(12) { 'abc' in ~/a.*/ }
t(13) { 5 in Integer }
t(14) { 'x' in String }
t(15) { [1] in [[1], [2]] }
t(16) { 2 in ([1, 2] as Set) }
t(17) { 2 in (1..3).step(2) }
t(18) { 1.5 in 1..2 }
t(19) { 'b' in 'a'..'c' }
t(21) { 'a' in ['A'] }
t(22) { 'x' in { it == 'x' } }
t(23) { 5 in null }
t(24) { 2 in [1, 2].iterator() }
t(25) { 'ab' in ['ab'] as String[] }
t(26) { 1 in [1: 'a'] }
"####;
    let expected = r####"1: true
2: true
3: true
4: true
5: true
6: true
7: true
8: false
9: false
10: true
11: true
12: true
13: true
14: true
15: true
16: true
17: false
18: false
19: true
21: false
22: true
23: false
24: false
25: true
26: true
"####;
    check(src, expected);
}

/// `as` and `instanceof` bind tighter than the other relational operators.
#[test]
fn round7_cast_precedence() {
    let src = r####"println(1 < 2 as int)
println(3 << 1 as String)
println((3 << 1 as String).getClass().name)
println("5" as int instanceof Integer)
println(2 in [1,2] as Set)
println([1] instanceof List == true)
"####;
    let expected = r####"true
6
java.lang.String
true
true
true
"####;
    check(src, expected);
}

/// Line continuation in triple-quoted strings, `indent`, `stripIndent(boolean)`, a `GString`'s Groovy `stripIndent`, whitespace-only lines, and no `tokenize(char[])`.
#[test]
fn round7_string_corners() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c().inspect() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
def x = 5
t(1) { '''  a
    b
  c'''.stripIndent() }
t(2) { """|a $x
  |b""".stripMargin() }
t(3) { '''#a
  #b'''.stripMargin('#') }
t(4) { '''\
line1
line2''' }
t(5) { """\
  x=$x
""".stripIndent() }
t(6) { '   \n  a\n    b\n'.stripIndent() }
t(7) { 'a\tb'.stripIndent() }
t(8) { 'a,b,,c,'.split(',') as List }
t(9) { 'a,b,,c,'.tokenize(',') }
t(10) { 'a b  c'.split() as List }
t(11) { 'a b  c'.tokenize() }
t(12) { 'a.b.c'.split('.') as List }
t(13) { 'a1b2c3'.split(/\d/) as List }
t(14) { ',a,b'.split(',') as List }
t(15) { 'a;b,c'.tokenize(';,') }
t(16) { 'a;b,c'.tokenize([';', ','] as char[]) }
t(17) { 'aXbXc'.split('X', 2) as List }
t(18) { ''.split(',') as List }
t(19) { ''.tokenize(',') }
t(20) { 'abc'.split('') as List }
t(21) { 'a,b'.split(',').getClass().name }
t(22) { 'a,b'.tokenize(',').getClass().name }
t(23) { 'a  b'.split(' ') as List }
t(24) { 'a\nb\r\nc'.readLines() }
t(25) { 'a\nb\n'.split('\n') as List }
t(26) { '  x  '.stripLeading() + '|' }
t(27) { 'aXbXc'.split('X', -1) as List }
t(28) { 'a,,b,,'.split(',', -1) as List }
t(29) { 'hello'.tokenize('l') }
t(30) { """a
b""".normalize() }
t(31) { 'a\r\nb'.denormalize().size() }
t(32) { 'abc'.indent(2) }
t(33) { '  abc\n  d'.stripIndent(true) }
t(1) { 'a\nb\r\nc\n'.indent(-1) }
t(2) { '  a\n b\n'.indent(-2) }
t(3) { ''.indent(3) }
t(4) { 'x'.indent(0) }
t(5) { '\t\ta\n\tb'.stripIndent(true) }
t(6) { '   \n  a\n    b'.stripIndent(true) }
t(7) { 'a\n  b'.stripIndent(false) }
t(8) { "a\\\nb" }
t(9) { 'p\
q' }
t(10) { "x\
y" }
println '   \n  a\n    b'.stripIndent(2).inspect()
println 'ab\n   \ncd\n'.stripIndent(1).inspect()
"####;
    let expected = r####"1: 'a\n  b\nc'
2: 'a 5\nb'
3: 'a\nb'
4: 'line1\nline2'
5: 'x=5\n'
6: '\n  a\n    b\n'
7: 'a\tb'
8: ['a', 'b', '', 'c']
9: ['a', 'b', 'c']
10: ['a', 'b', 'c']
11: ['a', 'b', 'c']
12: []
13: ['a', 'b', 'c']
14: ['', 'a', 'b']
15: ['a', 'b', 'c']
16: groovy.lang.MissingMethodException No signature of method: tokenize for class: java.lang.String is applicable for argument types: ([C) values: [;,]
17: ['a', 'bXc']
18: ['']
19: []
20: ['a', 'b', 'c']
21: '[Ljava.lang.String;'
22: 'java.util.ArrayList'
23: ['a', '', 'b']
24: ['a', 'b', 'c']
25: ['a', 'b']
26: 'x  |'
27: ['a', 'b', 'c']
28: ['a', '', 'b', '', '']
29: ['he', 'o']
30: 'a\nb'
31: 3
32: '  abc\n'
33: 'abc\nd'
1: 'a\nb\nc\n'
2: 'a\nb\n'
3: ''
4: 'x\n'
5: '\ta\nb'
6: '\na\n  b'
7: 'a\n  b'
8: 'a\\\nb'
9: 'pq'
10: 'xy'
'\na\n  b'
'b\n\nd\n'
"####;
    check(src, expected);
}

/// `Expando`: `properties`, `toString` / `equals` closures and its metaclass answers.
#[test]
fn round7_expando() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
def e = new Expando(name: 'n', age: 3)
t(1) { e.name }
t(2) { e.missing }
t(3) { e.greet = { -> "hi ${name}" }; e.greet() }
t(6) { e.getProperty('age') }
t(7) { e.setProperty('x', 9); e.x }
t(8) { e.add = { a, b -> a + b }; e.add(1, 2) }
t(9) { e.nope() }
t(10) { e == new Expando(name: 'n', age: 3) }
t(11) { e.hashCode() == new Expando().hashCode() }
t(12) { new Expando().toString() }
t(13) { e.class.name }
t(14) { e.properties.keySet() }
t(15) { def x = new Expando(); x.a = 1; x.b = 2; x.properties.collect { k, v -> k + v } }
t(16) { def x = new Expando(); x.toString = { -> 'custom' }; x.toString() }
t(17) { def x = new Expando(); x.toString = { -> 'custom' }; "$x" }
t(18) { def x = new Expando(); x.equals = { o -> true }; x == 5 }
t(19) { e.respondsTo('greet') }
t(20) { e.hasProperty('age') != null }
"####;
    let expected = r####"1: n
2: null
3: hi n
6: 3
7: 9
8: 3
9: groovy.lang.MissingMethodException No signature of method: nope for class: groovy.util.Expando is applicable for argument types: () values: []
10: false
11: false
12: {}
13: groovy.util.Expando
14: [name, age, greet, x, add]
15: [a1, b2]
16: custom
17: custom
18: true
19: []
20: false
"####;
    check(src, expected);
}

/// `ConfigSlurper`: dotted and block assignments, environments, auto-created `ConfigObject`s, `flatten` / `toProperties` / `isSet` / `merge`, and expressions over earlier settings.
#[test]
fn round7_config_slurper() {
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
def text = '''
app.name = 'demo'
app.port = 8080
db {
  url = "jdbc:x"
  pool { max = 5 }
}
list = [1, 2]
flag = true
environments {
  prod { app.port = 80 }
}
'''
def c = new ConfigSlurper().parse(text)
t(1) { c.app.name }
t(2) { c.app.port }
t(3) { c.db.url }
t(4) { c.db.pool.max }
t(5) { c.list }
t(6) { c.flag }
t(7) { c.getClass().name }
t(8) { c.nope }
t(9) { c.nope.getClass().name }
t(10) { c.flatten() }
t(11) { c.toProperties() }
t(12) { c.app }
t(13) { c.keySet() }
t(14) { new ConfigSlurper('prod').parse(text).app.port }
t(15) { c.isSet('app') }
t(16) { c.db.pool }
t(17) { c.containsKey('db') }
t(18) { c.app.port + 1 }
t(19) { def d = new ConfigSlurper().parse('a.b.c = 1'); d }
t(20) { def d = new ConfigSlurper().parse('a = 1\nb = a + 1'); d.b }
t(21) { c.merge(new ConfigSlurper().parse('app.extra = 2')); c.app }
t(22) { c.toString() }

text = '''
app {
  name = 'demo'
  title = "${name}-app"
  url = "http://${host ?: 'localhost'}:${port ?: 80}"
}
servers = [[name: 'a'], [name: 'b']]
size = 2 * 3
neg = -4
ratio = 1 / 4
def local = 7
fromLocal = local + 1
'''
c = new ConfigSlurper().parse(text)
t(1) { c.app.title }
t(2) { c.app.url }
t(3) { c.servers*.name }
t(4) { c.size }
t(5) { c.neg }
t(6) { c.ratio }
t(7) { c.fromLocal }
t(8) { c.local }
t(10) { c instanceof Map }
t(11) { c.app.size() }
t(12) { c.a.b.c.getClass().name }
t(13) { c.keySet() }
t(14) { def m = []; c.app.each { k, v -> m << k }; m }
t(15) { c.app.name = 'z'; c.app.name }
t(16) { c == new ConfigSlurper().parse(text) }
t(17) { new ConfigSlurper().parse('x = 1').x.getClass().name }
t(18) { new ConfigSlurper().parse('s.t = [1, 2]\ns.t << 3').s.t }
"####;
    let expected = r####"1: demo
2: 8080
3: jdbc:x
4: 5
5: [1, 2]
6: true
7: groovy.util.ConfigObject
8: [:]
9: groovy.util.ConfigObject
10: [app.name:demo, app.port:8080, db.url:jdbc:x, db.pool.max:5, list:[1, 2], flag:true]
11: [flag:true, app.port:8080, db.pool.max:5, list:[1, 2], app.name:demo, db.url:jdbc:x]
12: [name:demo, port:8080]
13: [app, db, list, flag, nope]
14: 80
15: true
16: [max:5]
17: true
18: 8081
19: [a:[b:[c:1]]]
20: 2
21: [name:demo, port:8080, extra:2]
22: [app:[name:demo, port:8080, extra:2], db:[url:jdbc:x, pool:[max:5]], list:[1, 2], flag:true, nope:[:]]
1: demo-app
2: http://localhost:80
3: [a, b]
4: 6
5: -4
6: 0.25
7: 8
8: [:]
10: true
11: 5
12: groovy.util.ConfigObject
13: [app, servers, size, neg, ratio, fromLocal, local, a]
14: [name, title, host, port, url]
15: z
16: false
17: java.lang.Integer
18: [1, 2, 3]
"####;
    check(src, expected);
}

/// Region time zones from the tz database with the JDK short names, custom `GMT±h` ids, legacy three-letter ids, `java.sql.Timestamp` and `GregorianCalendar`.
#[test]
fn round7_time_zones_calendar() {
    if !tz_database_present() {
        return;
    }
    let src = r####"def t(n, Closure c) { try { println "$n: " + c() } catch (e) { println "$n: ${e.getClass().name} ${e.message?.readLines()?.getAt(0)}" } }
def f = 'yyyy-MM-dd HH:mm:ss z Z'
def ny = TimeZone.getTimeZone('America/New_York')
def d1 = new Date(1700000000000L)  // Nov 2023
def d2 = new Date(1690000000000L)  // Jul 2023
def d3 = new Date(4102444800000L)  // 2100
def d4 = new Date(-2000000000000L) // 1906
t(1) { d1.format(f, ny) }
t(2) { d2.format(f, ny) }
t(3) { d3.format(f, ny) }
t(4) { d4.format(f, ny) }
t(5) { ny.ID }
t(6) { ny.rawOffset }
t(7) { ny.getOffset(d2.time) }
t(8) { ny.inDaylightTime(d2) }
t(9) { ny.useDaylightTime() }
t(10) { TimeZone.getTimeZone('Bogus/Zone').ID }
t(11) { d1.format(f, TimeZone.getTimeZone('Asia/Kolkata')) }
t(12) { d1.format(f, TimeZone.getTimeZone('Europe/London')) }
t(13) { d2.format(f, TimeZone.getTimeZone('Europe/London')) }
t(14) { d1.format(f, TimeZone.getTimeZone('Australia/Sydney')) }
t(15) { d1.format(f, TimeZone.getTimeZone('Asia/Tokyo')) }
t(16) { d1.format(f, TimeZone.getTimeZone('UTC')) }
t(17) { d1.format(f, TimeZone.getTimeZone('GMT+05:30')) }
t(18) { d1.format(f, TimeZone.getTimeZone('Europe/Berlin')) }
t(19) { d2.format(f, TimeZone.getTimeZone('America/Los_Angeles')) }
t(20) { d1.format(f, TimeZone.getTimeZone('America/Sao_Paulo')) }
t(21) { d1.format(f, TimeZone.getTimeZone('Asia/Dubai')) }
t(22) { d1.format(f, TimeZone.getTimeZone('PST')) }
t(23) { d1.format(f, TimeZone.getTimeZone('EST')) }
t(24) { TimeZone.getTimeZone('GMT+5').ID }
t(25) { TimeZone.getTimeZone('GMT-0800').ID }
t(26) { d1.toTimestamp().getClass().name }
t(27) { d1.toTimestamp().time }
t(28) { def c = Calendar.getInstance(ny); c.setTime(d1); c.get(Calendar.YEAR) + ' ' + c.get(Calendar.MONTH) + ' ' + c.get(Calendar.DAY_OF_MONTH) + ' ' + c.get(Calendar.HOUR_OF_DAY) }
t(29) { def c = Calendar.getInstance(TimeZone.getTimeZone('UTC')); c.setTimeInMillis(0); c.add(Calendar.MONTH, 13); c.time.time }
t(30) { def c = Calendar.getInstance(TimeZone.getTimeZone('UTC')); c.clear(); c.set(2020, 1, 29); c.timeInMillis }
t(31) { Calendar.getInstance().getClass().name }
t(32) { def c = Calendar.getInstance(TimeZone.getTimeZone('UTC')); c.timeInMillis = 0; c.get(Calendar.DAY_OF_WEEK) }
t(33) { def c = Calendar.getInstance(TimeZone.getTimeZone('UTC')); c.timeInMillis = 0; c[Calendar.YEAR] }
t(34) { def c = Calendar.getInstance(TimeZone.getTimeZone('UTC')); c.timeInMillis = 86400000L*40; c.getActualMaximum(Calendar.DAY_OF_MONTH) }
t(35) { def c = Calendar.getInstance(TimeZone.getTimeZone('UTC')); c.timeInMillis = 0; c.format('yyyy') }
t(36) { new java.sql.Timestamp(1500L).toString().size() }
t(37) { d1.toCalendar().getClass().name }
t(38) { def c = Calendar.getInstance(TimeZone.getTimeZone('UTC')); c.clear(); c.set(2021, 0, 31); c.add(Calendar.MONTH, 1); c.format('yyyy-MM-dd') }
t(39) { def c = Calendar.getInstance(TimeZone.getTimeZone('UTC')); c.clear(); c.set(2020, 1, 29); c.add(Calendar.YEAR, 1); c.format('yyyy-MM-dd') }
t(40) { def c = Calendar.getInstance(TimeZone.getTimeZone('America/New_York')); c.clear(); c.set(2023, 2, 12, 2, 30); c.format('yyyy-MM-dd HH:mm z') }
t(41) { def c = Calendar.getInstance(TimeZone.getTimeZone('UTC')); c.clear(); c.set(2020, 0, 1); c.add(Calendar.DAY_OF_MONTH, 60); c.get(Calendar.DAY_OF_YEAR) }
"####;
    let expected = r####"1: 2023-11-14 17:13:20 EST -0500
2: 2023-07-22 00:26:40 EDT -0400
3: 2099-12-31 19:00:00 EST -0500
4: 1906-08-16 15:26:40 EST -0500
5: America/New_York
6: -18000000
7: -14400000
8: true
9: true
10: GMT
11: 2023-11-15 03:43:20 IST +0530
12: 2023-11-14 22:13:20 GMT +0000
13: 2023-07-22 05:26:40 BST +0100
14: 2023-11-15 09:13:20 AEDT +1100
15: 2023-11-15 07:13:20 JST +0900
16: 2023-11-14 22:13:20 UTC +0000
17: 2023-11-15 03:43:20 GMT+05:30 +0530
18: 2023-11-14 23:13:20 CET +0100
19: 2023-07-21 21:26:40 PDT -0700
20: 2023-11-14 19:13:20 BRT -0300
21: 2023-11-15 02:13:20 GST +0400
22: 2023-11-14 14:13:20 PST -0800
23: 2023-11-14 17:13:20 EST -0500
24: GMT+05:00
25: GMT-08:00
26: java.sql.Timestamp
27: 1700000000000
28: 2023 10 14 17
29: 34214400000
30: 1582934400000
31: java.util.GregorianCalendar
32: 5
33: 1970
34: 28
35: 1970
36: 21
37: java.util.GregorianCalendar
38: 2021-02-28
39: 2021-02-28
40: 2023-03-12 03:30 EDT
41: 61
"####;
    check(src, expected);
}

/// Region time zones across history, daylight time and the far future (the zone file's POSIX rule).
#[test]
fn round7_time_zone_sweep() {
    if !tz_database_present() {
        return;
    }
    let src = r####"def f = 'yyyy-MM-dd HH:mm:ss.SSS z Z'
def ids = ['America/New_York', 'Europe/London', 'Australia/Lord_Howe', 'Asia/Kathmandu', 'America/St_Johns', 'Pacific/Chatham', 'Africa/Casablanca', 'America/Santiago', 'Asia/Tehran', 'America/Godthab', 'Antarctica/Troll', 'Etc/GMT+5', 'CST', 'AET', 'EST5EDT', 'Europe/Moscow', 'Pacific/Kiritimati']
def instants = [0L, 1700000000000L, 1690000000000L, 4102444800000L, 7258118400000L, -1000000000000L, 1711846800000L, 1711850400000L, 4118000000000L, 7273800000000L]
ids.each { id -> def z = TimeZone.getTimeZone(id); instants.each { ms -> println "$id $ms ${new Date(ms).format(f, z)} ${z.inDaylightTime(new Date(ms))}" } ; println "$id raw=${z.rawOffset} dst=${z.useDaylightTime()}" }
"####;
    let expected = r####"America/New_York 0 1969-12-31 19:00:00.000 EST -0500 false
America/New_York 1700000000000 2023-11-14 17:13:20.000 EST -0500 false
America/New_York 1690000000000 2023-07-22 00:26:40.000 EDT -0400 true
America/New_York 4102444800000 2099-12-31 19:00:00.000 EST -0500 false
America/New_York 7258118400000 2199-12-31 19:00:00.000 EST -0500 false
America/New_York -1000000000000 1938-04-24 18:13:20.000 EDT -0400 true
America/New_York 1711846800000 2024-03-30 21:00:00.000 EDT -0400 true
America/New_York 1711850400000 2024-03-30 22:00:00.000 EDT -0400 true
America/New_York 4118000000000 2100-06-29 20:53:20.000 EDT -0400 true
America/New_York 7273800000000 2200-07-01 08:00:00.000 EDT -0400 true
America/New_York raw=-18000000 dst=true
Europe/London 0 1970-01-01 01:00:00.000 GMT +0100 false
Europe/London 1700000000000 2023-11-14 22:13:20.000 GMT +0000 false
Europe/London 1690000000000 2023-07-22 05:26:40.000 BST +0100 true
Europe/London 4102444800000 2100-01-01 00:00:00.000 GMT +0000 false
Europe/London 7258118400000 2200-01-01 00:00:00.000 GMT +0000 false
Europe/London -1000000000000 1938-04-24 23:13:20.000 BST +0100 true
Europe/London 1711846800000 2024-03-31 02:00:00.000 BST +0100 true
Europe/London 1711850400000 2024-03-31 03:00:00.000 BST +0100 true
Europe/London 4118000000000 2100-06-30 01:53:20.000 BST +0100 true
Europe/London 7273800000000 2200-07-01 13:00:00.000 BST +0100 true
Europe/London raw=0 dst=true
Australia/Lord_Howe 0 1970-01-01 10:00:00.000 LHST +1000 false
Australia/Lord_Howe 1700000000000 2023-11-15 09:13:20.000 LHDT +1100 true
Australia/Lord_Howe 1690000000000 2023-07-22 14:56:40.000 LHST +1030 false
Australia/Lord_Howe 4102444800000 2100-01-01 11:00:00.000 LHDT +1100 true
Australia/Lord_Howe 7258118400000 2200-01-01 11:00:00.000 LHDT +1100 true
Australia/Lord_Howe -1000000000000 1938-04-25 08:13:20.000 LHST +1000 false
Australia/Lord_Howe 1711846800000 2024-03-31 12:00:00.000 LHDT +1100 true
Australia/Lord_Howe 1711850400000 2024-03-31 13:00:00.000 LHDT +1100 true
Australia/Lord_Howe 4118000000000 2100-06-30 11:23:20.000 LHST +1030 false
Australia/Lord_Howe 7273800000000 2200-07-01 22:30:00.000 LHST +1030 false
Australia/Lord_Howe raw=37800000 dst=true
Asia/Kathmandu 0 1970-01-01 05:30:00.000 NPT +0530 false
Asia/Kathmandu 1700000000000 2023-11-15 03:58:20.000 NPT +0545 false
Asia/Kathmandu 1690000000000 2023-07-22 10:11:40.000 NPT +0545 false
Asia/Kathmandu 4102444800000 2100-01-01 05:45:00.000 NPT +0545 false
Asia/Kathmandu 7258118400000 2200-01-01 05:45:00.000 NPT +0545 false
Asia/Kathmandu -1000000000000 1938-04-25 03:43:20.000 NPT +0530 false
Asia/Kathmandu 1711846800000 2024-03-31 06:45:00.000 NPT +0545 false
Asia/Kathmandu 1711850400000 2024-03-31 07:45:00.000 NPT +0545 false
Asia/Kathmandu 4118000000000 2100-06-30 06:38:20.000 NPT +0545 false
Asia/Kathmandu 7273800000000 2200-07-01 17:45:00.000 NPT +0545 false
Asia/Kathmandu raw=20700000 dst=false
America/St_Johns 0 1969-12-31 20:30:00.000 NST -0330 false
America/St_Johns 1700000000000 2023-11-14 18:43:20.000 NST -0330 false
America/St_Johns 1690000000000 2023-07-22 01:56:40.000 NDT -0230 true
America/St_Johns 4102444800000 2099-12-31 20:30:00.000 NST -0330 false
America/St_Johns 7258118400000 2199-12-31 20:30:00.000 NST -0330 false
America/St_Johns -1000000000000 1938-04-24 18:43:20.000 NST -0330 false
America/St_Johns 1711846800000 2024-03-30 22:30:00.000 NDT -0230 true
America/St_Johns 1711850400000 2024-03-30 23:30:00.000 NDT -0230 true
America/St_Johns 4118000000000 2100-06-29 22:23:20.000 NDT -0230 true
America/St_Johns 7273800000000 2200-07-01 09:30:00.000 NDT -0230 true
America/St_Johns raw=-12600000 dst=true
Pacific/Chatham 0 1970-01-01 12:45:00.000 CHAST +1245 false
Pacific/Chatham 1700000000000 2023-11-15 11:58:20.000 CHADT +1345 true
Pacific/Chatham 1690000000000 2023-07-22 17:11:40.000 CHAST +1245 false
Pacific/Chatham 4102444800000 2100-01-01 13:45:00.000 CHADT +1345 true
Pacific/Chatham 7258118400000 2200-01-01 13:45:00.000 CHADT +1345 true
Pacific/Chatham -1000000000000 1938-04-25 10:28:20.000 CHAST +1215 false
Pacific/Chatham 1711846800000 2024-03-31 14:45:00.000 CHADT +1345 true
Pacific/Chatham 1711850400000 2024-03-31 15:45:00.000 CHADT +1345 true
Pacific/Chatham 4118000000000 2100-06-30 13:38:20.000 CHAST +1245 false
Pacific/Chatham 7273800000000 2200-07-02 00:45:00.000 CHAST +1245 false
Pacific/Chatham raw=45900000 dst=true
Africa/Casablanca 0 1970-01-01 00:00:00.000 WET +0000 false
Africa/Casablanca 1700000000000 2023-11-14 23:13:20.000 WEST +0100 true
Africa/Casablanca 1690000000000 2023-07-22 05:26:40.000 WEST +0100 true
Africa/Casablanca 4102444800000 2100-01-01 00:00:00.000 WET +0000 false
Africa/Casablanca 7258118400000 2200-01-01 00:00:00.000 WET +0000 false
Africa/Casablanca -1000000000000 1938-04-24 22:13:20.000 WET +0000 false
Africa/Casablanca 1711846800000 2024-03-31 01:00:00.000 WET +0000 false
Africa/Casablanca 1711850400000 2024-03-31 02:00:00.000 WET +0000 false
Africa/Casablanca 4118000000000 2100-06-30 00:53:20.000 WET +0000 false
Africa/Casablanca 7273800000000 2200-07-01 12:00:00.000 WET +0000 false
Africa/Casablanca raw=0 dst=false
America/Santiago 0 1969-12-31 21:00:00.000 CLST -0300 true
America/Santiago 1700000000000 2023-11-14 19:13:20.000 CLST -0300 true
America/Santiago 1690000000000 2023-07-22 00:26:40.000 CLT -0400 false
America/Santiago 4102444800000 2099-12-31 21:00:00.000 CLST -0300 true
America/Santiago 7258118400000 2199-12-31 21:00:00.000 CLST -0300 true
America/Santiago -1000000000000 1938-04-24 18:13:20.000 CLT -0400 false
America/Santiago 1711846800000 2024-03-30 22:00:00.000 CLST -0300 true
America/Santiago 1711850400000 2024-03-30 23:00:00.000 CLST -0300 true
America/Santiago 4118000000000 2100-06-29 20:53:20.000 CLT -0400 false
America/Santiago 7273800000000 2200-07-01 08:00:00.000 CLT -0400 false
America/Santiago raw=-14400000 dst=true
Asia/Tehran 0 1970-01-01 03:30:00.000 IRST +0330 false
Asia/Tehran 1700000000000 2023-11-15 01:43:20.000 IRST +0330 false
Asia/Tehran 1690000000000 2023-07-22 07:56:40.000 IRST +0330 false
Asia/Tehran 4102444800000 2100-01-01 03:30:00.000 IRST +0330 false
Asia/Tehran 7258118400000 2200-01-01 03:30:00.000 IRST +0330 false
Asia/Tehran -1000000000000 1938-04-25 01:43:20.000 IRST +0330 false
Asia/Tehran 1711846800000 2024-03-31 04:30:00.000 IRST +0330 false
Asia/Tehran 1711850400000 2024-03-31 05:30:00.000 IRST +0330 false
Asia/Tehran 4118000000000 2100-06-30 04:23:20.000 IRST +0330 false
Asia/Tehran 7273800000000 2200-07-01 15:30:00.000 IRST +0330 false
Asia/Tehran raw=12600000 dst=false
America/Godthab 0 1969-12-31 21:00:00.000 WGT -0300 false
America/Godthab 1700000000000 2023-11-14 20:13:20.000 WGT -0200 false
America/Godthab 1690000000000 2023-07-22 02:26:40.000 WGT -0200 false
America/Godthab 4102444800000 2099-12-31 22:00:00.000 WGT -0200 false
America/Godthab 7258118400000 2199-12-31 22:00:00.000 WGT -0200 false
America/Godthab -1000000000000 1938-04-24 19:13:20.000 WGT -0300 false
America/Godthab 1711846800000 2024-03-31 00:00:00.000 WGST -0100 true
America/Godthab 1711850400000 2024-03-31 01:00:00.000 WGST -0100 true
America/Godthab 4118000000000 2100-06-29 23:53:20.000 WGST -0100 true
America/Godthab 7273800000000 2200-07-01 11:00:00.000 WGST -0100 true
America/Godthab raw=-7200000 dst=true
Antarctica/Troll 0 1970-01-01 00:00:00.000 GMT +0000 false
Antarctica/Troll 1700000000000 2023-11-14 22:13:20.000 GMT +0000 false
Antarctica/Troll 1690000000000 2023-07-22 06:26:40.000 CEST +0200 true
Antarctica/Troll 4102444800000 2100-01-01 00:00:00.000 GMT +0000 false
Antarctica/Troll 7258118400000 2200-01-01 00:00:00.000 GMT +0000 false
Antarctica/Troll -1000000000000 1938-04-24 22:13:20.000 GMT +0000 false
Antarctica/Troll 1711846800000 2024-03-31 03:00:00.000 CEST +0200 true
Antarctica/Troll 1711850400000 2024-03-31 04:00:00.000 CEST +0200 true
Antarctica/Troll 4118000000000 2100-06-30 02:53:20.000 CEST +0200 true
Antarctica/Troll 7273800000000 2200-07-01 14:00:00.000 CEST +0200 true
Antarctica/Troll raw=0 dst=true
Etc/GMT+5 0 1969-12-31 19:00:00.000 GMT-05:00 -0500 false
Etc/GMT+5 1700000000000 2023-11-14 17:13:20.000 GMT-05:00 -0500 false
Etc/GMT+5 1690000000000 2023-07-21 23:26:40.000 GMT-05:00 -0500 false
Etc/GMT+5 4102444800000 2099-12-31 19:00:00.000 GMT-05:00 -0500 false
Etc/GMT+5 7258118400000 2199-12-31 19:00:00.000 GMT-05:00 -0500 false
Etc/GMT+5 -1000000000000 1938-04-24 17:13:20.000 GMT-05:00 -0500 false
Etc/GMT+5 1711846800000 2024-03-30 20:00:00.000 GMT-05:00 -0500 false
Etc/GMT+5 1711850400000 2024-03-30 21:00:00.000 GMT-05:00 -0500 false
Etc/GMT+5 4118000000000 2100-06-29 19:53:20.000 GMT-05:00 -0500 false
Etc/GMT+5 7273800000000 2200-07-01 07:00:00.000 GMT-05:00 -0500 false
Etc/GMT+5 raw=-18000000 dst=false
CST 0 1969-12-31 18:00:00.000 CST -0600 false
CST 1700000000000 2023-11-14 16:13:20.000 CST -0600 false
CST 1690000000000 2023-07-21 23:26:40.000 CDT -0500 true
CST 4102444800000 2099-12-31 18:00:00.000 CST -0600 false
CST 7258118400000 2199-12-31 18:00:00.000 CST -0600 false
CST -1000000000000 1938-04-24 17:13:20.000 CDT -0500 true
CST 1711846800000 2024-03-30 20:00:00.000 CDT -0500 true
CST 1711850400000 2024-03-30 21:00:00.000 CDT -0500 true
CST 4118000000000 2100-06-29 19:53:20.000 CDT -0500 true
CST 7273800000000 2200-07-01 07:00:00.000 CDT -0500 true
CST raw=-21600000 dst=true
AET 0 1970-01-01 10:00:00.000 AEST +1000 false
AET 1700000000000 2023-11-15 09:13:20.000 AEDT +1100 true
AET 1690000000000 2023-07-22 14:26:40.000 AEST +1000 false
AET 4102444800000 2100-01-01 11:00:00.000 AEDT +1100 true
AET 7258118400000 2200-01-01 11:00:00.000 AEDT +1100 true
AET -1000000000000 1938-04-25 08:13:20.000 AEST +1000 false
AET 1711846800000 2024-03-31 12:00:00.000 AEDT +1100 true
AET 1711850400000 2024-03-31 13:00:00.000 AEDT +1100 true
AET 4118000000000 2100-06-30 10:53:20.000 AEST +1000 false
AET 7273800000000 2200-07-01 22:00:00.000 AEST +1000 false
AET raw=36000000 dst=true
EST5EDT 0 1969-12-31 19:00:00.000 EST -0500 false
EST5EDT 1700000000000 2023-11-14 17:13:20.000 EST -0500 false
EST5EDT 1690000000000 2023-07-22 00:26:40.000 EDT -0400 true
EST5EDT 4102444800000 2099-12-31 19:00:00.000 EST -0500 false
EST5EDT 7258118400000 2199-12-31 19:00:00.000 EST -0500 false
EST5EDT -1000000000000 1938-04-24 17:13:20.000 EST -0500 false
EST5EDT 1711846800000 2024-03-30 21:00:00.000 EDT -0400 true
EST5EDT 1711850400000 2024-03-30 22:00:00.000 EDT -0400 true
EST5EDT 4118000000000 2100-06-29 20:53:20.000 EDT -0400 true
EST5EDT 7273800000000 2200-07-01 08:00:00.000 EDT -0400 true
EST5EDT raw=-18000000 dst=true
Europe/Moscow 0 1970-01-01 03:00:00.000 MSK +0300 false
Europe/Moscow 1700000000000 2023-11-15 01:13:20.000 MSK +0300 false
Europe/Moscow 1690000000000 2023-07-22 07:26:40.000 MSK +0300 false
Europe/Moscow 4102444800000 2100-01-01 03:00:00.000 MSK +0300 false
Europe/Moscow 7258118400000 2200-01-01 03:00:00.000 MSK +0300 false
Europe/Moscow -1000000000000 1938-04-25 01:13:20.000 MSK +0300 false
Europe/Moscow 1711846800000 2024-03-31 04:00:00.000 MSK +0300 false
Europe/Moscow 1711850400000 2024-03-31 05:00:00.000 MSK +0300 false
Europe/Moscow 4118000000000 2100-06-30 03:53:20.000 MSK +0300 false
Europe/Moscow 7273800000000 2200-07-01 15:00:00.000 MSK +0300 false
Europe/Moscow raw=10800000 dst=false
Pacific/Kiritimati 0 1969-12-31 13:20:00.000 LINT -1040 false
Pacific/Kiritimati 1700000000000 2023-11-15 12:13:20.000 LINT +1400 false
Pacific/Kiritimati 1690000000000 2023-07-22 18:26:40.000 LINT +1400 false
Pacific/Kiritimati 4102444800000 2100-01-01 14:00:00.000 LINT +1400 false
Pacific/Kiritimati 7258118400000 2200-01-01 14:00:00.000 LINT +1400 false
Pacific/Kiritimati -1000000000000 1938-04-24 11:33:20.000 LINT -1040 false
Pacific/Kiritimati 1711846800000 2024-03-31 15:00:00.000 LINT +1400 false
Pacific/Kiritimati 1711850400000 2024-03-31 16:00:00.000 LINT +1400 false
Pacific/Kiritimati 4118000000000 2100-06-30 14:53:20.000 LINT +1400 false
Pacific/Kiritimati 7273800000000 2200-07-02 02:00:00.000 LINT +1400 false
Pacific/Kiritimati raw=50400000 dst=false
"####;
    check(src, expected);
}

/// A `MetaMethod`'s `parameterTypes` are `CachedClass`es of Groovy's own kinds; `nativeParameterTypes` and arrays.
#[test]
fn round7_cached_class_types() {
    let src = r####"println 'abc'.respondsTo('center', 3, 'x')[0].parameterTypes
println 'abc'.respondsTo('center')[0].parameterTypes[0].getClass().name
println 'abc'.respondsTo('center')[0].declaringClass.getClass().name
println 'abc'.respondsTo('center')[0].parameterTypes*.theClass
println([1].respondsTo('each')[0].parameterTypes[0].getClass().name)
class R { void v(int[] a, P p) {} }
class P {}
println new R().respondsTo('v')[0]
println new R().respondsTo('v')[0].parameterTypes*.getClass()*.name
println new R().respondsTo('v')[0].parameterTypes*.name
"####;
    let expected = r####"[class java.lang.Number, interface java.lang.CharSequence]
org.codehaus.groovy.reflection.stdclasses.NumberCachedClass
org.codehaus.groovy.reflection.CachedClass
[class java.lang.Number]
org.codehaus.groovy.reflection.stdclasses.CachedClosureClass
public void R.v(int[],P)
[org.codehaus.groovy.reflection.stdclasses.ArrayCachedClass, org.codehaus.groovy.reflection.CachedClass]
[[I, P]
"####;
    check(src, expected);
}

/// Whether the system tz database the zone tests read is installed
/// (`$TZDIR`, else `/usr/share/zoneinfo`). Without it those tests skip, saying
/// why, rather than fail on a machine that cannot answer them.
fn tz_database_present() -> bool {
    let dir = std::env::var("TZDIR").unwrap_or_else(|_| "/usr/share/zoneinfo".into());
    let present = std::path::Path::new(&dir).join("America/New_York").exists();
    if !present {
        eprintln!("skipped: no tz database at {dir}");
    }
    present
}

/// Ireland's Irish Standard Time is daylight time to the JDK (the tz source
/// keeps it as standard time with negative winter saving).
#[test]
fn round8_dublin_negative_dst() {
    if !tz_database_present() {
        return;
    }
    let src = r####"['Europe/Dublin', 'Eire', 'Europe/London', 'Europe/Lisbon', 'Africa/Casablanca', 'Europe/Moscow', 'Asia/Kolkata'].each { id ->
  def z = TimeZone.getTimeZone(id)
  [0L, 1700000000000L, 1690000000000L, -1000000000000L, 4118000000000L, 100000000000L].each { println id + ' ' + new Date(it).format("yyyy-MM-dd z Z", z) + " " + z.inDaylightTime(new Date(it)) }
}
"####;
    let expected = r####"Europe/Dublin 1970-01-01 IST +0100 true
Europe/Dublin 2023-11-14 GMT +0000 false
Europe/Dublin 2023-07-22 IST +0100 true
Europe/Dublin 1938-04-24 IST +0100 true
Europe/Dublin 2100-06-30 IST +0100 true
Europe/Dublin 1973-03-03 GMT +0000 false
Eire 1970-01-01 IST +0100 true
Eire 2023-11-14 GMT +0000 false
Eire 2023-07-22 IST +0100 true
Eire 1938-04-24 IST +0100 true
Eire 2100-06-30 IST +0100 true
Eire 1973-03-03 GMT +0000 false
Europe/London 1970-01-01 GMT +0100 false
Europe/London 2023-11-14 GMT +0000 false
Europe/London 2023-07-22 BST +0100 true
Europe/London 1938-04-24 BST +0100 true
Europe/London 2100-06-30 BST +0100 true
Europe/London 1973-03-03 GMT +0000 false
Europe/Lisbon 1970-01-01 WET +0100 false
Europe/Lisbon 2023-11-14 WET +0000 false
Europe/Lisbon 2023-07-22 WEST +0100 true
Europe/Lisbon 1938-04-24 WEST +0100 true
Europe/Lisbon 2100-06-30 WEST +0100 true
Europe/Lisbon 1973-03-03 WET +0100 false
Africa/Casablanca 1970-01-01 WET +0000 false
Africa/Casablanca 2023-11-14 WEST +0100 true
Africa/Casablanca 2023-07-22 WEST +0100 true
Africa/Casablanca 1938-04-24 WET +0000 false
Africa/Casablanca 2100-06-30 WET +0000 false
Africa/Casablanca 1973-03-03 WET +0000 false
Europe/Moscow 1970-01-01 MSK +0300 false
Europe/Moscow 2023-11-15 MSK +0300 false
Europe/Moscow 2023-07-22 MSK +0300 false
Europe/Moscow 1938-04-25 MSK +0300 false
Europe/Moscow 2100-06-30 MSK +0300 false
Europe/Moscow 1973-03-03 MSK +0300 false
Asia/Kolkata 1970-01-01 IST +0530 false
Asia/Kolkata 2023-11-15 IST +0530 false
Asia/Kolkata 2023-07-22 IST +0530 false
Asia/Kolkata 1938-04-25 IST +0530 false
Asia/Kolkata 2100-06-30 IST +0530 false
Asia/Kolkata 1973-03-03 IST +0530 false
"####;
    check(src, expected);
}

/// `respondsTo` on a script class answers the `GroovyObject` methods it has
/// no declaration of.
#[test]
fn round8_responds_to_groovy_object_methods() {
    let src = r####"class P {}
def p = new P()
["getMetaClass", "invokeMethod", "setMetaClass", "toString", "getProperty"].each { n -> p.respondsTo(n).each { m -> println "$n ${m.getClass().name} ${m.declaringClass.name} ${m.parameterTypes*.name} ${m.returnType.name}" } }
println p.respondsTo("invokeMethod")[0]
"####;
    let expected = r####"getMetaClass org.codehaus.groovy.reflection.GeneratedMetaMethod$Proxy groovy.lang.GroovyObject [] groovy.lang.MetaClass
invokeMethod org.codehaus.groovy.reflection.CachedMethod groovy.lang.GroovyObject [java.lang.String, java.lang.Object] java.lang.Object
setMetaClass org.codehaus.groovy.reflection.GeneratedMetaMethod$Proxy groovy.lang.GroovyObject [groovy.lang.MetaClass] void
toString org.codehaus.groovy.reflection.GeneratedMetaMethod$Proxy java.lang.Object [] java.lang.String
getProperty org.codehaus.groovy.reflection.CachedMethod groovy.lang.GroovyObject [java.lang.String] java.lang.Object
public default java.lang.Object groovy.lang.GroovyObject.invokeMethod(java.lang.String,java.lang.Object)
"####;
    check(src, expected);
}

/// Groovy's compile-time declaration errors: a redeclared local (in the same
/// scope, a nested block, a closure or a method), a closure parameter or a
/// catch parameter naming a visible variable, and a duplicate class. Sibling
/// blocks may reuse a name.
#[test]
fn round8_duplicate_declarations_are_compile_errors() {
    let cases = [
        (
            "def x = 1; def x = 2",
            "The current scope already contains a variable of the name x",
        ),
        (
            "def x = 1; if (true) { def x = 2 }",
            "The current scope already contains a variable of the name x",
        ),
        (
            "def x = 1; def c = { def x = 2 }",
            "The current scope already contains a variable of the name x",
        ),
        (
            "def x = 1; def c = { x -> x }",
            "The current parameter list already contains a parameter of the name x",
        ),
        (
            "def f(a) { def a = 1 }",
            "The current scope already contains a variable of the name a",
        ),
        (
            "int n = 1; String n = 'x'",
            "The current scope already contains a variable of the name n",
        ),
        (
            "def x = 1; for (x in 1..2) {}",
            "The current scope already contains a variable of the name x",
        ),
        (
            "def e = 1; try { } catch (e) { }",
            "The current scope already contains a variable of the name e",
        ),
        (
            "class A {}; class A {}",
            "Invalid duplicate class definition of class A",
        ),
    ];
    for (src, message) in cases {
        let (out, ok) = run(src);
        assert!(!ok, "{src} compiled; stdout {out:?}");
        let err = stderr_lines(src, "groovyrs: ");
        assert!(err.contains(message), "{src}: {err:?}");
    }
    let fine = "if (true) { def y = 1 }; def y = 2\nfor (i in 1..2) { def z = i }; for (i in 1..2) { def z = i }\nfor (int k = 0; k < 1; k++) {}; for (int k = 0; k < 1; k++) {}\ntry { def e = 1 } catch (e) { }\nprintln y";
    check(fine, "2\n");
}

/// `java.util.TimeZone` is the JDK's `ZoneInfo`: an instant is daylight time
/// when its offset is not the JDK's standard offset, which folds negative
/// daylight saving into the standard offset of each zone line using it (Dublin
/// 1968-71, Windhoek since 1990, Casablanca since 2018), and the metazone's
/// explicit daylight offset names it in a pattern (Inuvik 1975). Before 1900
/// the raw offset applies.
#[test]
fn round9_zoneinfo_standard_offsets() {
    if !tz_database_present() {
        return;
    }
    let src = r####"def ms = ['1969-06-01': -18446400000L, '1970-01-15': 1252800000L, '1972-01-15': 64324800000L,
  '1995-07-15': 805809600000L, '2020-01-15': 1579089600000L, '2020-07-15': 1594814400000L,
  '1946-07-15': -740491200000L, '1947-01-15': -724593600000L, '1960-01-15': -314366400000L,
  '2024-03-20': 1710936000000L, '2024-07-01': 1719835200000L, '1975-07-01': 173448000000L,
  '1975-12-01': 186667200000L, '1990-07-01': 646833600000L, '1890-06-01': -2511432000000L,
  '1910-06-01': -1880366400000L]
[
  ['Europe/Dublin', ['1969-06-01', '1970-01-15', '1972-01-15']],
  ['Europe/London', ['1969-06-01', '1970-01-15', '1972-01-15']],
  ['Africa/Windhoek', ['1995-07-15', '2020-01-15', '2020-07-15']],
  ['Europe/Prague', ['1946-07-15', '1947-01-15', '1960-01-15']],
  ['Africa/Casablanca', ['2024-03-20', '2024-07-01']],
  ['America/Inuvik', ['1975-07-01', '1975-12-01', '1990-07-01']],
  ['Africa/Abidjan', ['1890-06-01', '1910-06-01']],
  ['Asia/Kolkata', ['1890-06-01']],
].each { id, days ->
  def z = TimeZone.getTimeZone(id)
  println "$id raw=${z.rawOffset} save=${z.DSTSavings} use=${z.useDaylightTime()}"
  days.each { d ->
    def date = new Date(ms[d])
    def c = Calendar.getInstance(z)
    c.time = date
    println "  $d ${date.format('z Z', z)} ${z.inDaylightTime(date)} ${z.getOffset(date.time)} zo=${c.get(Calendar.ZONE_OFFSET)} dst=${c.get(Calendar.DST_OFFSET)}"
  }
}
"####;
    let expected = r####"Europe/Dublin raw=0 save=3600000 use=true
  1969-06-01 IST +0100 true 3600000 zo=0 dst=3600000
  1970-01-15 IST +0100 true 3600000 zo=0 dst=3600000
  1972-01-15 GMT +0000 false 0 zo=0 dst=0
Europe/London raw=0 save=3600000 use=true
  1969-06-01 GMT +0100 false 3600000 zo=3600000 dst=0
  1970-01-15 GMT +0100 false 3600000 zo=3600000 dst=0
  1972-01-15 GMT +0000 false 0 zo=0 dst=0
Africa/Windhoek raw=3600000 save=0 use=false
  1995-07-15 CAT +0100 false 3600000 zo=3600000 dst=0
  2020-01-15 WAT +0200 true 7200000 zo=3600000 dst=3600000
  2020-07-15 WAT +0200 true 7200000 zo=3600000 dst=3600000
Europe/Prague raw=3600000 save=3600000 use=true
  1946-07-15 CEST +0200 true 7200000 zo=3600000 dst=3600000
  1947-01-15 CET +0000 false 0 zo=0 dst=0
  1960-01-15 CET +0100 false 3600000 zo=3600000 dst=0
Africa/Casablanca raw=0 save=0 use=false
  2024-03-20 WET +0000 false 0 zo=0 dst=0
  2024-07-01 WEST +0100 true 3600000 zo=0 dst=3600000
America/Inuvik raw=-25200000 save=0 use=false
  1975-07-01 MST -0700 true -25200000 zo=-28800000 dst=3600000
  1975-12-01 MST -0800 false -28800000 zo=-28800000 dst=0
  1990-07-01 MDT -0600 true -21600000 zo=-25200000 dst=3600000
Africa/Abidjan raw=0 save=0 use=false
  1890-06-01 GMT +0000 false 0 zo=0 dst=0
  1910-06-01 GMT -0016 false -968000 zo=-968000 dst=0
Asia/Kolkata raw=19800000 save=0 use=false
  1890-06-01 IST +0530 false 19800000 zo=19800000 dst=0
"####;
    check(src, expected);
}

/// `XmlParser` keys a namespaced attribute by its `QName`, which equals,
/// matches and hashes as `groovy.namespace.QName` does: by namespace and local
/// part, a `prefix:local` string with the prefix or the namespace, `*`
/// wildcards in `matches` only.
#[test]
fn round9_qname_attribute_keys() {
    let src = r####"import groovy.xml.XmlParser
import groovy.xml.XmlSlurper
import groovy.namespace.QName
def x = new XmlParser().parseText('<r xmlns:a="urn:a"><e a:k="1" b="2"/></r>')
def at = x.e[0].attributes()
println at.getClass().name
println at.keySet()*.getClass()*.name
println at.keySet()*.toString()
println at[new QName('urn:a', 'k')]
println at[new QName('urn:a', 'k', 'zz')]
println at['{urn:a}k']
println at['a:k']
println at.containsKey(new QName('urn:a', 'k'))
at.each { k, v -> println "${k.getClass().simpleName} $k=$v" }
println at.collect { k, v -> k instanceof QName ? k.prefix : '-' }
def k = at.keySet().iterator().next()
println([k.localPart, k.namespaceURI, k.prefix, k.qualifiedName])
println(k == new QName('urn:a', 'k', 'q'))
println(k.equals('a:k'))
println(k.equals('urn:a:k'))
println(k.matches('*:k'))
println(k.hashCode() == new QName('urn:a', 'k').hashCode())
println at
println x.e[0]
println at.inspect()
def y = new XmlParser(false, false).parseText('<e a:k="1"/>')
println y.attributes().keySet()*.getClass()*.simpleName
def s = new XmlSlurper().parseText('<r xmlns:a="urn:a"><e a:k="1" b="2"/></r>')
println s.e[0].attributes()
println s.e[0].attributes().keySet()*.getClass()*.simpleName
def m = [:]; m[new QName('u', 'l')] = 1; m[new QName('u', 'l', 'p')] = 2
println m
println m.size()
def q2 = new QName('k')
println([q2 == 'k', q2.equals('k'), q2.matches('k'), q2.equals(':k'), q2.matches('*:k'), q2.matches(':k')])
def r2 = new QName('urn:a', 'k', 'a')
println([r2 == 'a:k', r2.equals('urn:a:k'), r2.matches('urn:a:k'), r2.matches('*:k'), r2.matches('a:*'), r2.matches(new QName('*', 'k')), r2.equals(new QName('*', 'k')), r2.matches('k'), r2.equals('a:'), r2.matches(new QName('urn:a', '*'))])
println(new QName('', 'k', 'p').matches('k'))
println([q2.hashCode(), r2.hashCode(), new QName('urn:a','k').hashCode()])
println(r2 instanceof QName)
println(r2.qualifiedName)
println(q2.qualifiedName)
println(QName.valueOf('{urn:x}y').namespaceURI)
println(QName.valueOf('y').localPart)
println r2.class.name
def m2 = [(r2): 1]
println m2[new QName('urn:a', 'k')]
println m2.keySet()[0].prefix
println('a:k' == r2)
println x.e[0].attribute('{urn:a}k')
"####;
    let expected = r####"java.util.LinkedHashMap
[groovy.namespace.QName, java.lang.String]
[{urn:a}k, b]
1
1
null
null
true
QName {urn:a}k=1
String b=2
[a, -]
[k, urn:a, a, a:k]
true
true
true
true
true
[{urn:a}k:1, b:2]
e[attributes={{urn:a}k=1, b=2}; value=[]]
[{urn:a}k:'1', 'b':'2']
[String]
[b:2, {urn:a}k:1]
[String, String]
[{u}l:2]
1
[false, false, true, true, true, true]
[true, true, true, true, true, true, false, false, false, true]
false
[107, -1059850845, -1059850845]
true
a:k
k
urn:x
y
groovy.namespace.QName
1
a
false
null
"####;
    check(src, expected);
    // An element in no namespace is named by a `String` (`XmlParser.getElementName`).
    let src = "def x = new groovy.xml.XmlParser().parseText('<r><e/></r>')\nprintln x.name().getClass().name\nprintln(x.name() == 'r')\nprintln(x.e[0].name() == 'e')\n";
    check(src, "java.lang.String\ntrue\ntrue\n");
}

/// `obj.metaClass` / `getMetaClass()` is a `HandleMetaClass` over the value;
/// `X.metaClass` is one too until an injection (or a `.static` view) installs
/// an `ExpandoMetaClass` for `X`. A string iterates through `asCollection`,
/// an `ArrayList$Itr`.
#[test]
fn round9_handle_meta_class_and_string_iterator() {
    let src = r####"def g = "a${1}b"
println g.metaClass.getClass().name
println "x".metaClass.getClass().name
println 5.metaClass.getClass().name
println([1].metaClass.getClass().name)
println([a: 1].metaClass.getClass().name)
println String.metaClass.getClass().name
println Integer.metaClass.getClass().name
class P {}
println new P().metaClass.getClass().name
println P.metaClass.getClass().name
println g.iterator().getClass().name
println "xy".iterator().collect()
println g.iterator().collect()
println g.getMetaClass().getClass().name
println "x".getMetaClass().getClass().name
println(g.metaClass.theClass.name)
println("x".metaClass.theClass.name)
"####;
    let expected = r####"org.codehaus.groovy.runtime.HandleMetaClass
org.codehaus.groovy.runtime.HandleMetaClass
org.codehaus.groovy.runtime.HandleMetaClass
org.codehaus.groovy.runtime.HandleMetaClass
org.codehaus.groovy.runtime.HandleMetaClass
org.codehaus.groovy.runtime.HandleMetaClass
org.codehaus.groovy.runtime.HandleMetaClass
org.codehaus.groovy.runtime.HandleMetaClass
org.codehaus.groovy.runtime.HandleMetaClass
java.util.ArrayList$Itr
[x, y]
[a, 1, b]
org.codehaus.groovy.runtime.HandleMetaClass
org.codehaus.groovy.runtime.HandleMetaClass
org.codehaus.groovy.runtime.GStringImpl
java.lang.String
"####;
    check(src, expected);
    let src = r####"println String.metaClass.getClass().name
String.metaClass.shout = { -> delegate.toUpperCase() }
println String.metaClass.getClass().name
println "x".metaClass.getClass().name
println "x".shout()
def s = "inst"
s.metaClass.whisper = { -> 'w' }
println s.metaClass.getClass().name
println Integer.metaClass.getClass().name
class P {}
P.metaClass.hi = { -> 'hi' }
println P.metaClass.getClass().name
println new P().metaClass.getClass().name
def p = new P()
p.metaClass.yo = { -> 'yo' }
println p.metaClass.getClass().name
println p.yo()
"####;
    let expected = r####"org.codehaus.groovy.runtime.HandleMetaClass
groovy.lang.ExpandoMetaClass
org.codehaus.groovy.runtime.HandleMetaClass
X
org.codehaus.groovy.runtime.HandleMetaClass
org.codehaus.groovy.runtime.HandleMetaClass
groovy.lang.ExpandoMetaClass
org.codehaus.groovy.runtime.HandleMetaClass
org.codehaus.groovy.runtime.HandleMetaClass
yo
"####;
    check(src, expected);
    let src = r####"def s = "inst"
s.metaClass.whisper = { -> "w" }
println String.metaClass.getClass().name
String.metaClass.static.st = { -> 1 }
println String.metaClass.getClass().name
Integer.metaClass.static.st = { -> 1 }
println Integer.metaClass.getClass().name
Long.metaClass.constructor = { -> 1L }
println Long.metaClass.getClass().name
println String.metaClass.static.getClass().name
println Short.metaClass.static.getClass().name
"####;
    let expected = r####"org.codehaus.groovy.runtime.HandleMetaClass
groovy.lang.ExpandoMetaClass
groovy.lang.ExpandoMetaClass
groovy.lang.ExpandoMetaClass
groovy.lang.ExpandoMetaClass$ExpandoMetaProperty
groovy.lang.ExpandoMetaClass$ExpandoMetaProperty
"####;
    check(src, expected);
}

/// A closure literal in a `ConfigSlurper` script: run where a value uses it
/// (`collect`, `sort`, `groupBy`), stored and called later, nested, with
/// `return` and `if`, and named `Script_<md5 of the text>$_run_closureN` in
/// the order Groovy generates its classes.
#[test]
fn round9_config_slurper_closures() {
    let src = r####"def cfg = new ConfigSlurper().parse('''
nums = (1..3).collect { it * 2 }
evens = [1,2,3,4].findAll { it % 2 == 0 }
name = "abc".collect { it.toUpperCase() }.join()
total = [1,2,3].inject(0) { a, b -> a + b }
sorted = [3,1,2].sort { a, b -> b <=> a }
db { url = [h: 'x'].collectEntries { k, v -> [(k): v * 2] } }
fn = { x -> x + 1 }
''')
println cfg.nums
println cfg.evens
println cfg.name
println cfg.total
println cfg.sorted
println cfg.db.url
println cfg.fn(41)
println cfg.fn.getClass().name.replaceAll(/\d+/, 'N')
"####;
    let expected = r####"[2, 4, 6]
[2, 4]
ABC
6
[3, 2, 1]
[h:xx]
42
Script_ebfeNeNaNdNbNcdefabNebdNaN$_run_closureN
"####;
    check(src, expected);
    let src = r####"def text = '''
a = { 1 }
b { c = { 2 }; d = [1].collect { x -> { -> x } } }
e = { -> { -> 3 } }
'''
def cfg = new ConfigSlurper().parse(text)
println cfg.a.getClass().name
println cfg.b.c.getClass().name
println cfg.b.d[0].getClass().name
println cfg.e.getClass().name
println cfg.e().getClass().name
println text.md5()
println cfg.a()
println cfg.e()()
println cfg.b.d[0]()
"####;
    let expected = r####"Script_5d8e9711cf7a2a1057bb2841ffe39f33$_run_closure1
Script_5d8e9711cf7a2a1057bb2841ffe39f33$_run_closure2$_closure4
Script_5d8e9711cf7a2a1057bb2841ffe39f33$_run_closure2$_closure5$_closure6
Script_5d8e9711cf7a2a1057bb2841ffe39f33$_run_closure3
Script_5d8e9711cf7a2a1057bb2841ffe39f33$_run_closure3$_closure7
5d8e9711cf7a2a1057bb2841ffe39f33
1
3
1
"####;
    check(src, expected);
    let src = r####"def cfg = new ConfigSlurper('prod').parse('''
def base = 10
factor = 3
scaled = [1, 2].collect { it * factor + base }
pick = { x -> if (x > 1) { return 'big' }; 'small' }
twice = { f, v -> f(f(v)) }
inc = { it + 1 }
nested = [[1, 2], [3]].collect { row -> row.collect { c -> c * 10 } }
server {
  port = 8000
  urls = ['a', 'b'].collect { "http://$it:$port" }
}
environments {
  prod { mode = ['x'].collect { it.toUpperCase() } }
}
s = [3, 1, 2].sort { a, b -> a <=> b }
any = [1, 2, 3].any { it > 2 }
grp = ['ab', 'c', 'de'].groupBy { it.size() }
idx = [5, 6].findIndexOf { it == 6 }
''')
println cfg.scaled
println cfg.pick(1) + ' ' + cfg.pick(5)
println cfg.twice(cfg.inc, 1)
println cfg.inc(41)
println cfg.nested
println cfg.server.urls
println cfg.mode
println cfg.s
println cfg.any
println cfg.grp
println cfg.idx
println cfg.inc.maximumNumberOfParameters
println cfg.pick.maximumNumberOfParameters
println cfg.twice.parameterTypes.size()
"####;
    let expected = r####"[13, 16]
small big
3
42
[[10, 20], [30]]
[http://a:8000, http://b:8000]
[X]
[1, 2, 3]
true
[2:[ab, de], 1:[c]]
1
1
1
2
"####;
    check(src, expected);
}

/// `ConfigSlurper` runs the active environment's block against a config
/// object of its own and merges it into the result after the script, and
/// `flatten` descends into every non-empty map.
#[test]
fn round9_config_slurper_environment_merge_and_flatten() {
    let src = r####"def c = new ConfigSlurper('dev').parse('''
a = 1
environments { dev { a = 2; b = 3; n { x = 1 } } }
n { y = 2 }
m = [:]
p = [k: [j: 1]]
e {}
z = 9
''')
println c
println c.flatten()
println c.toProperties().sort()
"####;
    let expected = r####"[a:2, n:[y:2, x:1], m:[:], p:[k:[j:1]], e:[:], z:9, b:3]
[a:2, n.y:2, n.x:1, p.k.j:1, z:9, b:3]
[a:2, b:3, n.x:1, n.y:2, p.k.j:1, z:9]
"####;
    check(src, expected);
    let src = r####"def c = new ConfigSlurper('dev').parse('''
a = 1
s { t = 1 }
environments { dev { b = a + 1; s.u = 2; q.r = 3; s { v = t } } }
after = a
''')
println c
println c.flatten()
def d = new ConfigSlurper().parse('''
environments { dev { b = 1 } }
x = [k: [:], j: [1]]
''')
println d
println d.flatten()
"####;
    let expected = r####"[a:1, s:[t:[:], u:2, v:[:]], after:1, b:2, q:[r:3]]
[a:1, s.u:2, after:1, b:2, q.r:3]
[x:[k:[:], j:[1]]]
[x.j:[1]]
"####;
    check(src, expected);
}

/// `@ToString` belongs to the annotated class: a plain subclass inherits it
/// (name and members), an annotated one renders only its own properties unless
/// `includeSuperProperties` / `includeSuper` widen it, plain fields need
/// `includeFields`, getters and trait properties count, and a generated member
/// shadows a superclass's declared one.
#[test]
fn round10_to_string_member_selection_and_inheritance() {
    let src = r####"import groovy.transform.*
@ToString class A { int a = 1; String a2 = 'x' }
class B extends A { int b = 2 }
@ToString class C extends B { int c = 3 }
@ToString(includeSuperProperties = true, includeNames = true) class D extends B { int d = 4 }
@ToString(includeSuper = true) class E extends C { int e = 5 }
println new B()
println new C()
println new D()
println new E()
class F extends A { String toString() { 'F:' + super.toString() } }
println new F()
class U { String toString() { 'U!' } }
@ToString class U2 extends U { int u = 1 }
println new U2()
@ToString(includeSuper = true, includeNames = true) class U3 extends U { int u = 1 }
println new U3()
@ToString class Pv { int a = 1; private int b = 2; public int c = 3; static int s = 4 }
println new Pv()
@ToString(includeFields = true) class Pf { private int z = 9; int a = 1; public int p = 4 }
println new Pf()
@ToString(includes = 'b,a') class Inc { int a = 1; int b = 2; int c = 3 }
println new Inc()
@ToString class G { String n = 'a'; String getN() { 'G' + n }; boolean isOk() { true }; String getUrl() { 'u' } }
println new G()
trait Tr { int t = 7 }
@ToString class W implements Tr { int w = 1 }
println new W()
"####;
    let expected = r####"A(1, x)
C(3)
D(d:4, b:2, a:1, a2:x)
E(5, C(3))
F:A(1, x)
U2(1)
U3(u:1, super:U!)
Pv(1)
Pf(1, 9, 4)
Inc(2, 1)
G(Ga, true, u)
W(1, 7)
"####;
    check(src, expected);
}

/// `@EqualsAndHashCode` / `@TupleConstructor` / `@Canonical` on a class
/// hierarchy: own properties only by default, `callSuper` folds the parent in
/// last, `canEqual` keeps a re-annotated subclass from equalling its parent,
/// getters are read, and a tuple constructor is not inherited.
#[test]
fn round10_equals_tuple_member_selection_and_inheritance() {
    let src = r####"import groovy.transform.*
@EqualsAndHashCode class Q { int q }
class Q2 extends Q { int r }
@EqualsAndHashCode class Q3 extends Q { int r }
@EqualsAndHashCode(callSuper = true) class Q4 extends Q { int r }
println new Q2(q: 1, r: 1) == new Q2(q: 1, r: 2)
println new Q2(q: 1, r: 1).hashCode()
println new Q(q: 1) == new Q2(q: 1, r: 5)
println new Q3(q: 1, r: 1) == new Q3(q: 2, r: 1)
println new Q(q: 1) == new Q3(q: 1, r: 1)
println new Q3(q: 1, r: 1) == new Q(q: 1)
println new Q4(q: 1, r: 1) == new Q4(q: 2, r: 1)
println new Q4(q: 1, r: 1) == new Q4(q: 1, r: 1)
println new Q4(q: 1, r: 1).hashCode()
println([new Q2(q: 1, r: 1), new Q2(q: 1, r: 2)].unique().size())
@EqualsAndHashCode class Pv { int a = 1; private int b = 2 }
println new Pv().hashCode()
@EqualsAndHashCode(includeFields = true) class Pf { private int z = 9; int a = 1 }
println new Pf().hashCode()
@EqualsAndHashCode class Gt { String n = 'a'; String getN() { 'b' } }
println new Gt().hashCode()
println new Gt() == new Gt(n: 'z')
@TupleConstructor class T0 { int a; int b }
@TupleConstructor class T1 extends T0 { int c }
println new T1(5).c
@TupleConstructor(includeSuperProperties = true) class T2 extends T0 { int c }
def t2 = new T2(1, 2, 3)
println "${t2.a}${t2.b}${t2.c}"
@TupleConstructor class T3 { private int z = 9; int a; int b }
println new T3(1, 2).b
@TupleConstructor(includes = 'b,a') class T4 { int a; int b }
println new T4(1, 2).a
@Canonical class P0 { String n }
@Canonical class P1 extends P0 { int k }
println new P1(3)
println new P1(n: 'z', k: 3)
println new P1(n: 'x', k: 1) == new P1(n: 'y', k: 1)
"####;
    let expected = r####"true
7494
true
true
false
false
false
true
449640
1
7494
442155
7591
true
5
123
2
2
P1(3)
P1(3)
true
"####;
    check(src, expected);
}

/// `Long`, `Short` and `Byte` values small enough to be `Integer`s keep their
/// class: `getClass()`, `equals` (`2L in [1, 2]` is `false`), `hashCode`,
/// `instanceof`, map keys and set members, arithmetic (a `Long` stays one,
/// a `Short`/`Byte` widens to `Integer`), bitwise operators and collections.
#[test]
fn round9_boxed_integer_kinds() {
    let src = r####"println("L1: " + ((2L in [1,2])))
println("L2: " + (([1,2].contains(2L))))
println("L3: " + ((((byte)5).getClass().name)))
println("L4: " + ((((short)5).getClass().name)))
println("L5: " + ((((long)5).getClass().name)))
def b = (byte) 7; println b.getClass().name
def s = (short) 7; println s.getClass().name
def l = (long) 7; println l.getClass().name
def xs = [(byte)1, (short)1, 1L, 1]
println("L10: " + (xs*.getClass()*.simpleName))
println("L11: " + (xs.unique().size()))
println("L12: " + (((byte)1 == 1)))
println("L13: " + ((((byte)1).equals(1))))
println("L14: " + ((1.equals(1L))))
println("L15: " + ((1L.equals(1))))
println("L16: " + (([1L] as Set == [1] as Set)))
println("L17: " + ((([1L] as Set).contains(1))))
println("L18: " + (([a:1L].containsValue(1))))
println("L19: " + ((((Long)3).hashCode())))
println("L20: " + (((5 as long).getClass().name)))
println("L21: " + (((5 as byte).getClass().name)))
println("L22: " + (((5 as short).getClass().name)))
println("L23: " + ((Long.valueOf(5).getClass().name)))
def m = [:]; m[1L] = 'L'; m[1] = 'I'; println m.size()
switch (2L) { case 2: println 'int-case'; break; default: println 'no' }
println("L26: " + (([1, 2, 3].indexOf(2L))))
println("L27: " + ((new HashSet([1, 1L, (short)1, (byte)1]).size())))
def ll = 3L; println ll.getClass().name
println("L29: " + (((ll + 1).getClass().name)))
println("L30: " + ((((byte)1 + (byte)1).getClass().name)))
println("L31: " + (([1L, 2L].sum().getClass().name)))
println("L32: " + (Integer.valueOf(4).equals((short)4)))
println("L33: " + ((2L.isCase(2))))
println("L34: " + (([2L].isCase(2))))
println("L35: " + (([1,2,3] - [2L])))
println("L36: " + (([1,2,3].count(2L))))
println("L37: " + (([5L,6L].collect{ it.getClass().simpleName })))
"####;
    let expected = r####"L1: false
L2: false
L3: java.lang.Byte
L4: java.lang.Short
L5: java.lang.Long
java.lang.Byte
java.lang.Short
java.lang.Long
L10: [Byte, Short, Long, Integer]
L11: 1
L12: true
L13: false
L14: false
L15: false
L16: true
L17: false
L18: false
L19: 3
L20: java.lang.Long
L21: java.lang.Byte
L22: java.lang.Short
L23: java.lang.Long
2
int-case
L26: -1
L27: 4
java.lang.Long
L29: java.lang.Long
L30: java.lang.Integer
L31: java.lang.Long
L32: false
L33: true
L34: false
L35: [1, 3]
L36: 1
L37: [Long, Long]
"####;
    check(src, expected);
    let src = r####"def show = { v -> "${v} ${v?.getClass()?.simpleName}" }
println show(2L)
println show((byte) 5)
println show((short) 5)
println show(5 as long)
println show(5 as Byte)
println show(Long.valueOf(7))
println show(Short.valueOf((short) 3))
println show(Byte.valueOf((byte) 3))
println show("12".toLong())
println show(12.longValue())
println show(12.shortValue())
println show(12.byteValue())
println show(Math.round(2.5d))
def l = [1L, 2L]
println l*.getClass()*.simpleName
println l.sum().getClass().simpleName
println((l[0] + 1).getClass().simpleName)
println((l[0] * 2).getClass().simpleName)
println(((byte)1 + (byte)2).getClass().simpleName)
println(((short)1 * 3).getClass().simpleName)
println((-(5L)).getClass().simpleName)
println(2L in [1, 2])
println(2 in [1L, 2L])
println([1, 2].contains(2L))
println([1L, 2L].contains(2L))
println([1, 2].indexOf(2L))
println([1L, 2L].indexOf(2L))
println([1, 2, 3].count(2L))
println([1, 2, 3] - [2L])
println(2L == 2)
println(2L.equals(2))
println(2.equals(2L))
println(2L.equals(2L))
println((short) 2 == 2)
println(((short) 2).equals(2))
println(((byte) 2).equals((byte) 2))
println(2L <=> 3)
println((-1L).hashCode())
println((-1).hashCode())
println(((byte) -1).hashCode())
println(5000000000L.hashCode())
println([(1L): 'a', (1): 'b'].size())
def m = [:]; m[1L] = 'L'; m[1] = 'I'; println m
println m[1L]
println m.keySet()*.getClass()*.simpleName
switch (2L) { case 2: println 'case-int'; break; default: println 'case-none' }
switch (2) { case 2L: println 'case-long'; break; default: println 'case-none2' }
println(0L ? 't' : 'f')
println((byte) 0 ? 't' : 'f')
println "${3L}"
println([3L, 1L, 2L].sort())
println([3L, 1, 2L].max().getClass().simpleName)
println(2L.class.name)
println((long) 3 instanceof Long)
println((long) 3 instanceof Integer)
println(((short) 3) instanceof Short)
println((byte) 200)
println((short) 70000)
println([(byte) 1, (short) 1, 1L, 1].unique().size())
println([1L, 2L].collect { it * 2 }*.getClass()*.simpleName)
println(1L..3L)
println((1L..3L).collect { it.getClass().simpleName })
println([5L].inspect())
println(String.format('%d|%d|%d', 5L, (byte) 6, (short) 7))
long x = 4
println(x.getClass().simpleName)
def y = x
println([y].first().getClass().simpleName)
def f(long p) { p.getClass().simpleName }
println(f(3))
def g(def p) { p.getClass().simpleName }
println(g(3L))
println([a: 1L].a.getClass().simpleName)
println([1L].toSet().first().getClass().simpleName)
println(Long.MAX_VALUE.getClass().simpleName)
"####;
    let expected = r####"2 Long
5 Byte
5 Short
5 Long
5 Byte
7 Long
3 Short
3 Byte
12 Long
12 Long
12 Short
12 Byte
3 Long
[Long, Long]
Long
Long
Long
Integer
Integer
Long
false
false
false
true
-1
1
1
[1, 3]
true
false
false
true
true
false
true
-1
0
-1
-1
705032705
2
[1:L, 1:I]
L
[Long, Integer]
case-int
case-long
f
f
3
[1, 2, 3]
Long
java.lang.Long
true
false
true
-56
4464
1
[Long, Long]
1..3
[Long, Long, Long]
[5]
5|6|7
Long
Long
Long
Long
Long
Long
Long
"####;
    check(src, expected);
    let src = r####"def list = []
for (long i = 0; i < 70000; i++) list << i
println list.sum()
println list.sum().getClass().name
println([1L, 2L].collect { it * 2 }*.getClass()*.simpleName)
println(((short) 3) & 1)
println((1L << 40))
def b = (byte) 5
println(b | 2)
"####;
    let expected = r####"2449965000
java.lang.Long
[Long, Long]
1
1099511627776
7
"####;
    check(src, expected);
}

/// A `Long` read out of a collection, a call or a property stays a `Long`
/// through `&`, `|`, `^`, `>>` and `<<`, and the JDK methods that answer a
/// `long` (`Date.getTime`, `File.length` / `lastModified`, `System.nanoTime`,
/// a `long[]` element) answer a `Long`.
#[test]
fn round10_long_kind_from_runtime_paths() {
    let src = r####"def l = [5L, 6L]
def x = l[0]
println((x & 1).getClass().simpleName)
println((x | 2).getClass().simpleName)
println((x ^ 3).getClass().simpleName)
println((x >> 1).getClass().simpleName)
println((l[1] & 2).getClass().simpleName)
println((l[1] << 1).getClass().simpleName)
println(((byte) 5 & 1).getClass().simpleName)
println(((short) 5 >> 1).getClass().simpleName)
def y = x + 1
println((y & 7).getClass().simpleName)
println((7 & y))
println([1L, 2L].collect { (it & 1).getClass().simpleName })
def s = "12"
def n = s.toLong()
println((n & 4).getClass().simpleName)
def m = 'toLong'
println("3"."$m"().getClass().simpleName)
println(System.currentTimeMillis().getClass().simpleName)
def t = System.nanoTime()
println(t.getClass().simpleName)
println(Long.parseLong('4').getClass().simpleName)
println("abc".length().getClass().simpleName)
println(Math.abs(-3L).getClass().simpleName)
println(Math.max(1L, 2).getClass().simpleName)
println([3L,1L].max().getClass().simpleName)
println(([1L] as long[])[0].getClass().simpleName)
println((1L..3L).sum().getClass().simpleName)
println(Long.MAX_VALUE.intdiv(1L << 40).getClass().simpleName)
println(10L.intdiv(3).getClass().simpleName)
println(10.intdiv(3L).getClass().simpleName)
println((10L % 3).getClass().simpleName)
println((-x).getClass().simpleName)
println((x ** 2).getClass().simpleName)
println(x.abs().getClass().simpleName)
println(x.next().getClass().simpleName)
println(Integer.toBinaryString(5).getClass().simpleName)
println(Long.valueOf('9').getClass().simpleName)
println("5".toLong().getClass().simpleName)
println(5.toLong().getClass().simpleName)
println(new Date(0).getTime().getClass().simpleName)
println new File(".").length().getClass().simpleName
println new File(".").lastModified().getClass().simpleName
println new File("no-such-file-here").lastModified()
"####;
    let expected = r####"Long
Long
Long
Long
Long
Long
Integer
Integer
Long
6
[Long, Long]
Long
Long
Long
Long
Long
Integer
Long
Long
Long
Long
Long
Long
Long
Long
Long
Long
Long
Long
Long
String
Long
Long
Long
Long
Long
Long
0
"####;
    check(src, expected);
}

/// `Date.parse` is `SimpleDateFormat.parse`: `Z` (RFC 822), `X` (ISO 8601),
/// `z` (`GMT±hh:mm` or a zone name from the JDK's zone strings, first match
/// winning), month and day names, quoted text, abutting numeric fields, the
/// two-digit-year window, and a wall time a change repeats or skips read as
/// `GregorianCalendar` reads it.
#[test]
fn round10_date_parse_zones_and_names() {
    if !tz_database_present() {
        return;
    }
    let src = r####"def p = { pat, txt, z = 'UTC' -> try { Date.parse(pat, txt, TimeZone.getTimeZone(z)).time } catch (e) { e.class.simpleName + ': ' + e.message } }
println p('yyyy-MM-dd HH:mm Z', '1969-06-01 12:00 +0000')
println p('yyyy-MM-dd HH:mm Z', '1969-06-01 12:00 -0530')
println p('yyyy-MM-dd HH:mm:ss Z', '2024-03-10 02:30:00 +0100')
println p('yyyy-MM-dd HH:mm z', '2024-03-10 02:30 GMT')
println p('yyyy-MM-dd HH:mm z', '2024-03-10 02:30 PST')
println p('yyyy-MM-dd HH:mm z', '2024-03-10 02:30 GMT+02:00')
println p('yyyy-MM-dd HH:mm X', '2024-03-10 02:30 Z')
println p('yyyy-MM-dd HH:mm XXX', '2024-03-10 02:30 +05:30')
println p("yyyy-MM-dd'T'HH:mm:ssZ", '2024-03-10T02:30:00+0000')
println p('yyyy-MM-dd HH:mm Z', '2024-03-10 02:30 junk')
println p('dd MMM yyyy', '05 Mar 2024')
println p('EEE, dd MMM yyyy HH:mm:ss Z', 'Sun, 10 Mar 2024 02:30:00 +0000')
println p('yyyy-MM-dd HH:mm Z', '2024-03-10 02:30 +01')
['GMT','UTC','PST','PDT','EST','EDT','CST','CDT','CET','CEST','BST','JST','AEST','MSK','HST','AKST','WET','EET','Pacific Standard Time','Central European Summer Time','pst'].each { n ->
  println "$n ${p('yyyy-MM-dd HH:mm z', "2024-07-10 12:00 $n")} ${p('yyyy-MM-dd HH:mm z', "2024-01-10 12:00 $n")}"
}
println p('yyyyMMddHHmmss', '20240310023000')
println p('yyMMdd', '991231')
println p('yy-MM-dd', '24-01-02')
println p('hh:mm a, dd/MM/yyyy', '12:30 AM, 05/03/2024')
println p('hh:mm a, dd/MM/yyyy', '12:30 pm, 05/03/2024')
println p('yyyy-MM-dd HH:mm', '2024-03-10 02:30', 'America/New_York')
println p('yyyy-MM-dd HH:mm', '2024-11-03 01:30', 'America/New_York')
println p('yyyy-MM-dd HH:mm', '2024-03-10 02:30', 'Asia/Tokyo')
println p('yyyy-MM-dd', '2024-13-40')
println p('MMMM d, yyyy', 'March 5, 2024')
println p('d MMM yyyy', '5 SEPT 2024')
println p('yyyy-MM-dd HH:mm:ss.SSS', '2024-03-10 02:30:00.123')
println p('yyyy', '2024abc')
println p('yyyy-MM-dd HH:mm z', '2024-07-10 12:00 GMT-7')
"####;
    let expected = r####"-18446400000
-18426600000
1710034200000
1710037800000
1710066600000
1710030600000
1710037800000
1710018000000
1710037800000
ParseException: Unparseable date: "2024-03-10 02:30 junk"
1709596800000
1710037800000
ParseException: Unparseable date: "2024-03-10 02:30 +01"
GMT 1720612800000 1704888000000
UTC 1720612800000 1704888000000
PST 1720641600000 1704916800000
PDT 1720638000000 1704913200000
EST 1720630800000 1704906000000
EDT 1720627200000 1704902400000
CST 1720634400000 1704909600000
CDT 1720630800000 1704906000000
CET 1720609200000 1704884400000
CEST 1720605600000 1704880800000
BST 1720609200000 1704884400000
JST 1720580400000 1704855600000
AEST 1720576800000 1704852000000
MSK 1720602000000 1704877200000
HST 1720648800000 1704924000000
AKST 1720645200000 1704920400000
WET 1720612800000 1704888000000
EET 1720605600000 1704880800000
Pacific Standard Time 1720641600000 1704916800000
Central European Summer Time 1720605600000 1704880800000
pst 1720641600000 1704916800000
1710037800000
946598400000
1704153600000
1709598600000
1709641800000
1710055800000
1730615400000
1710005400000
1739059200000
1709596800000
ParseException: Unparseable date: "5 SEPT 2024"
1710037800123
1704067200000
ParseException: Unparseable date: "2024-07-10 12:00 GMT-7"
"####;
    check(src, expected);
}

/// A config closure's `owner` is the closure it is written in (a block's
/// closure, an environment's, another closure) or the `Script_<md5>` script
/// object at the top level; `thisObject` is the script object and the
/// `delegate` defaults to the owner.
#[test]
fn round10_config_closure_owner() {
    let src = r####"def text = '''
a = { 1 }
b { c = { 2 }; d = [1].collect { x -> { -> x } } }
e = { -> { -> 3 } }
environments { dev { f = { 4 } } }
'''
def cfg = new ConfigSlurper('dev').parse(text)
def n = { v -> v?.getClass()?.name }
println n(cfg.a.owner)
println n(cfg.a.delegate)
println n(cfg.a.thisObject)
println n(cfg.b.c.owner)
println n(cfg.b.c.delegate)
println n(cfg.b.c.thisObject)
println n(cfg.b.d[0].owner)
println n(cfg.b.d[0].owner.owner)
println n(cfg.e().owner)
println n(cfg.f.owner)
println n(cfg.f.owner.owner)
println(cfg.a.owner.is(cfg.e.owner))
println(cfg.a.owner == cfg.a.thisObject)
"####;
    let expected = r####"Script_2ddc2d1cbaa9043cb4155eb40eadecd4
Script_2ddc2d1cbaa9043cb4155eb40eadecd4
Script_2ddc2d1cbaa9043cb4155eb40eadecd4
Script_2ddc2d1cbaa9043cb4155eb40eadecd4$_run_closure2
Script_2ddc2d1cbaa9043cb4155eb40eadecd4$_run_closure2
Script_2ddc2d1cbaa9043cb4155eb40eadecd4
Script_2ddc2d1cbaa9043cb4155eb40eadecd4$_run_closure2$_closure6
Script_2ddc2d1cbaa9043cb4155eb40eadecd4$_run_closure2
Script_2ddc2d1cbaa9043cb4155eb40eadecd4$_run_closure3
Script_2ddc2d1cbaa9043cb4155eb40eadecd4$_run_closure4$_closure9
Script_2ddc2d1cbaa9043cb4155eb40eadecd4$_run_closure4
true
true
"####;
    check(src, expected);
}

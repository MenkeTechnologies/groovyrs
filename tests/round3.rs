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

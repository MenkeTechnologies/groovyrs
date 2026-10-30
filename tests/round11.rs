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

/// [`check`] for a program whose output names its script class (an anonymous
/// class is `Script$1`): it runs from a file named `<stem>.groovy`, as its
/// expectation was captured.
fn check_as(stem: &str, src: &str, expected: &str) {
    let dir = std::env::temp_dir().join(format!("groovyrs_round11_{}", fasthash(src)));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{stem}.groovy"));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_groovy"))
        .arg(&path)
        .output()
        .expect("spawn groovy");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "program failed; stdout so far:\n{stdout}"
    );
    assert_eq!(stdout, expected);
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

#[test]
fn anonymous_classes_extend_implement_capture_and_are_named_like_groovy() {
    let src = r####"def c = new Comparator() { int compare(a, b) { b <=> a } }
println([3,1,2].sort(c))
println c.getClass().name
int base = 10
def r = new Runnable() { void run() { println "run ${base}" } }
r.run()
println r instanceof Runnable
abstract class Shape { abstract double area(); String describe() { "area=${area()}" } }
def s = new Shape() { double area() { 2.5 } }
println s.describe()
println s.getClass().name
println s.getClass().superclass.name
class Outer { int v = 7; def make() { new Runnable() { void run() { println "outer v=${v}" } } } }
new Outer().make().run()
println new Outer().make().getClass().name
int x = 1
def r2 = new Runnable() { void run() { x++ ; println "in $x" } }
r2.run(); println x
x = 10; r2.run(); println x
def a = new Object() { def f() { 1 } }
def b = new Object() { def g() { new Object() { String toString() { "inner" } } } }
println a.getClass().name + " " + b.getClass().name + " " + b.g().getClass().name
println b.g()
def m() { new Object() { String toString() { "fromfn" } } }
println m().getClass().name
println m()
def c2 = { -> new Object() { String toString() { "incl" } } }
println c2().getClass().name
abstract class Base { int n; Base(int n) { this.n = n }; abstract int k(); int tot() { n + k() } }
def bb = new Base(5) { int k() { 3 } }
println bb.tot()
println bb.getClass().superclass.name + " " + bb.getClass().interfaces
def it2 = new Iterator() { int i = 0; boolean hasNext() { i < 3 }; Object next() { i++ } }
while (it2.hasNext()) print it2.next()
println()
println(new Object() { def hello() { "hi " + this.getClass().name } }.hello())
def comp = new Comparator<String>() { @Override int compare(String p, String q) { p.length() <=> q.length() } }
println(['ccc','a','bb'].toSorted(comp))
"####;
    let expected = r####"null
anon$1
run 10
true
area=2.5
anon$3
Shape
outer v=7
Outer$1
in 2
2
in 11
11
anon$5 anon$6 anon$6$1
inner
anon$7
fromfn
anon$8
8
Base []
012
hi anon$11
[a, bb, ccc]
"####;
    check_as("anon", src, expected);
}

#[test]
fn anonymous_classes_reach_outer_members_closures_and_comparators() {
    let src = r####"interface Greeter { String greet(String n) }
abstract class Animal {
  String name
  Animal(String n) { name = n }
  abstract String sound()
  String speak() { "$name says ${sound()}" }
}
class Zoo {
  int visits = 0
  String prefix = "zoo"
  def bump() { visits++ }
  def make(String suffix) {
    int local = 5
    new Greeter() {
      String greet(String n) { bump(); visits += 10; local++; "$prefix-$n-$suffix-$local" }
    }
  }
  static def smake() { new Greeter() { String greet(String n) { "static " + n } } }
  def viaClosure() { def k = 3; [1, 2].collect { i -> new Greeter() { String greet(String n) { "$n$i$k$prefix" } } } }
}
def z = new Zoo()
def g = z.make("s")
println g.greet("a")
println g.greet("b")
println z.visits
println g.getClass().name
println Zoo.smake().greet("x")
println Zoo.smake().getClass().name
println z.viaClosure()*.greet("q")
println z.viaClosure()*.getClass()*.name
def dog = new Animal("Rex") { String sound() { "woof" } }
println dog.speak()
println dog.name
println dog instanceof Animal
def cat = new Animal("Tom") { String sound() { "meow" }; String speak() { "custom " + super.speak() } }
println cat.speak()
def counter = 0
def inc = new Runnable() { void run() { counter += 2 } }
inc.run(); inc.run()
println counter
def items = ['x', 'y', 'z']
def it3 = new Iterator() { int i = 0; boolean hasNext() { i < items.size() }; Object next() { items[i++] } }
def acc = []
while (it3.hasNext()) acc << it3.next()
println acc
def base = 100
def withField = new Object() { def f = base + 1; def get() { f } }
println withField.get()
def outerAnon = new Object() {
  def mk(int m) { new Object() { String toString() { "inner m=$m base=$base" } } }
}
println outerAnon.mk(7)
println outerAnon.mk(7).getClass().name
def g2 = new Greeter() { String greet(String n) { n.toUpperCase() } }
println g2.greet("hi")
println g2 instanceof Greeter
println([g2].collect { it.greet("yo") })
def gs = (1..3).collect { n -> new Greeter() { String greet(String s) { s * n } } }
println gs*.greet("ab")
println new Object() { String toString() { "anon toString" } }
def eq = new Object() { boolean equals(Object o) { true }; int hashCode() { 1 } }
println eq == 5
def byLen = new Comparator<String>() { int compare(String a, String b) { a.size() <=> b.size() ?: a <=> b } }
def words = ['pear', 'fig', 'banana', 'kiwi']
println words.toSorted(byLen)
println words.max(byLen)
println words.min(byLen)
def ts = new TreeSet(byLen); ts.addAll(words); println ts
println words.sort(false, byLen)
Collections.sort(words, byLen); println words
def mk(int n) { new Object() { int twice() { n * 2 }; String toString() { "obj$n" } } }
def objs = (1..3).collect { mk(it) }
println objs*.twice()
println objs
def rs = []
for (int i = 0; i < 3; i++) { def j = i; rs << new Runnable() { void run() { print j } } }
rs*.run(); println()
def thrower = new Object() { def boom() { throw new IllegalStateException("bang") } }
try { thrower.boom() } catch (IllegalStateException e) { println "caught ${e.message}" }
def sorted = [5, 3, 9].sort(new Comparator() { int compare(a, b) { b <=> a } })
println sorted
def lst = [5, 3, 9]
lst.sort(new Comparator() { int compare(a, b) { b <=> a } })
println lst
def selfRef = new Object() { def me() { this }; def cls() { [1].collect { this.getClass().name } } }
println selfRef.me().is(selfRef)
println selfRef.cls()
println "${new Object() { String toString() { 'in gstring' } }}"
def counter2 = new Object() { private int c = 0; def next() { ++c } }
counter2.next(); counter2.next(); println counter2.next()
def adder = { x -> new Object() { def add(y) { x + y } } }
println adder(10).add(5)
"####;
    let expected = r####"zoo-a-s-6
zoo-b-s-7
22
Zoo$1
static x
Zoo$2
[q13zoo, q23zoo]
[Zoo$3, Zoo$3]
Rex says woof
Rex
true
custom Tom says meow
4
[x, y, z]
101
inner m=7 base=100
anons$6$1
HI
true
[YO]
[ab, abab, ababab]
anon toString
true
[fig, kiwi, pear, banana]
banana
fig
[fig, kiwi, pear, banana]
[fig, kiwi, pear, banana]
[fig, kiwi, pear, banana]
[2, 4, 6]
[obj1, obj2, obj3]
012
caught bang
null
[9, 5, 3]
true
[anons$17]
in gstring
3
15
"####;
    check_as("anons", src, expected);
}

#[test]
fn script_class_comparator_instance_is_a_jdk_comparator() {
    let src = r####"class Desc implements Comparator { int compare(a, b) { b <=> a } }
def c = new Desc()
def l = [3,1,2]
println l.sort(c)
println l
println([3,1,2].toSorted(c))
println([5,4,9].max(c))
def ts = new TreeSet(c); ts.addAll([1,5,3]); println ts
class R implements Runnable { void run() { println "ran" } }
println c instanceof Comparator
println c.getClass().interfaces
int x = 1
def cl = { x++ }
cl(); println x
"####;
    let expected = r####"null
[3, 2, 1]
[3, 2, 1]
4
[5, 3, 1]
true
[interface java.util.Comparator, interface groovy.lang.GroovyObject]
2
"####;
    check(src, expected);
}

/// The script object renders through `Object.toString` — its class name, `@`,
/// and the hex of its `hashCode()` — wherever it is printed. It rendered as the
/// heap's fallback `(obj:0)`.
#[test]
fn script_object_renders_as_class_at_hash() {
    let src = r####"println this.toString() == "scriptobj@" + Integer.toHexString(this.hashCode())
println "${this}" == this.toString()
println([this].toString() == "[" + this + "]")
println({ -> this }().toString().startsWith("scriptobj@"))
"####;
    check_as("scriptobj", src, "true\ntrue\ntrue\ntrue\n");
}

/// A power assert records the values inside a `GString` placeholder at their
/// columns in the script, in every placeholder form and nesting, and records
/// nothing inside a closure body. Placeholders were parsed from their own text,
/// so their columns were unknown and none of their values was shown.
#[test]
fn power_assert_records_inside_gstring_placeholders() {
    let src = r####"def x = 5; def a = [1, 2]
def show(Closure c) { try { c() } catch (AssertionError e) { println e.message } }
show { assert "v=${x + 1} and ${a.size()}" == "no" }
show { assert "o${"i${x * 2}"}" == "no" }
show { assert "${a[0] + x}".size() == 9 }
show { assert "q${ -> x}" == "no" }
show { assert a.any { it == x } }
"####;
    let expected = r####"assert "v=${x + 1} and ${a.size()}" == "no"
            | |          | |        |
            5 6          | 2        false
                         [1, 2]
assert "o${"i${x * 2}"}" == "no"
               | |       |
               5 10      false
assert "${a[0] + x}".size() == 9
          ||   | |   |      |
          |1   6 5   1      false
          [1, 2]
assert "q${ -> x}" == "no"
                   |
                   false
assert a.any { it == x }
       | |
       | false
       [1, 2]
"####;
    check(src, expected);
}

/// `min()`/`max()` with no argument skip `null` elements, as
/// `GroovyCollections.min`/`max` do; the closure and comparator forms see
/// every element. `[1, null, 2].min()` answered `null`.
#[test]
fn natural_min_and_max_skip_nulls() {
    let src = r####"println([1,null,2].min())
println([null, 3, 1].min())
println([null].min())
println([null, 1].max())
println([2, null, 1].max())
println([3, null, 1].min { it })
println([3, null, 1].min { a, b -> a <=> b })
println(([3, null, 1] as Integer[]).min())
println([3, null, 1].toSet().min())
println([].min())
"####;
    let expected = r####"1
1
null
1
2
null
null
1
1
null
"####;
    check(src, expected);
}

/// `instanceof GroovyObject` holds for Groovy-implemented values only (a user
/// instance, a closure, a `GString`, an `Expando`, a `ConfigObject`), never for
/// a JDK value, an array or a Groovy `Range`; and `instanceof GString` never
/// holds for a plain `String`. Both answered `true` for every value.
#[test]
fn groovy_object_and_gstring_type_tests() {
    let src = r####"class A {}
def x = 1
def vals = ["s", 1, 1L, 1.5, 1.5d, 1.5f, 1G, true, [1], [a:1], [1] as Set, 1..2, 'a'..'c', ~/x/, ("ab" =~ /a/), new StringBuilder(), new A(), {it}, "g$x", new Expando(), [a:1].entrySet().first(), new Object(), [1] as int[], new Date(), new ConfigObject(), Optional.empty(), UUID.randomUUID()]
println vals.collect { it instanceof GroovyObject }
println vals.collect { it instanceof GString }
println vals.collect { it instanceof String }
println(("a$x" + "b") instanceof GString)
println(("a" + "b$x") instanceof GString)
"####;
    let expected = r####"[false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, true, true, true, true, false, false, false, false, true, false, false]
[false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, true, false, false, false, false, false, false, false, false]
[true, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false]
true
false
"####;
    check(src, expected);
}

/// `X.class` on a type name is a class literal, the class `X` itself, for a
/// JDK type and a script class alike; `.class` on a class *value* is still
/// `java.lang.Class`. `Integer.class` answered `class java.lang.Class`.
#[test]
fn class_literal_on_a_type_name_is_the_class() {
    let src = r####"class A { static int n = 3 }
println Integer.class
println A.class
println Math.class.name
println String.class == String
println A.class.name
println A.class.simpleName
println Integer.class.class
println A.class.n
println Integer.class.MAX_VALUE
println([1, 2].class)
def f(x) { x.class }
println f(A)
"####;
    let expected = r####"class java.lang.Integer
class A
java.lang.Math
true
A
A
class java.lang.Class
3
2147483647
class java.util.ArrayList
class java.lang.Class
"####;
    check(src, expected);
}

/// A closure parameter default applies only to a call that stops short of the
/// parameter, as Groovy's one-`doCall`-per-arity closures do: an explicit
/// `null` stays `null`. The default answered for an explicit `null` too.
#[test]
fn closure_default_does_not_replace_an_explicit_null() {
    let src = r####"def c = { a, b = 5 -> "$a $b" }
println c(1, null)
println c(1)
println c(1, 2)
def d = { a = 1, b = 2 -> "$a $b" }
println d()
println d(null)
println d(7)
println d(null, null)
println([3].collect { v, w = 'w' -> "$v $w" })
def e = { a, b = a * 2 -> b }
println e(4)
println(c.call(3))
println(c.call(3, null))
"####;
    let expected = r####"1 null
1 5
1 2
1 2
null 2
7 2
null null
[3 w]
8
3 5
3 null
"####;
    check(src, expected);
}

/// `C.newInstance(args)` is the GDK's `Class.newInstance(Object[])`, `new C(args)`,
/// for a script class and a JDK class, unless the class declares its own
/// `static newInstance`. It raised `MissingMethodException`.
#[test]
fn class_new_instance_constructs() {
    let src = r####"class A { int x = 4; String toString() { "A($x)" } }
println A.newInstance()
println A.class.newInstance()
println A.newInstance().x
class B { int y; B(int y) { this.y = y } }
println B.newInstance(7).y
println(Integer.newInstance(5) + 1)
class F { static newInstance() { "mine" } }
println F.newInstance()
try { B.newInstance() } catch (e) { println e.class.name }
"####;
    let expected = r####"A(4)
A(4)
4
7
6
mine
groovy.lang.GroovyRuntimeException
"####;
    check(src, expected);
}

/// `@Singleton` is a port of `SingletonASTTransformation`: a static `getInstance()`,
/// a `lazy` instance made on first use, a `property` name, and a no-argument
/// constructor that raises once the instance exists. `S.x` reads through a
/// declared `static getX()`. `new S()` made a second instance and `getInstance()`
/// was missing.
#[test]
fn singleton_transform_guards_the_constructor() {
    let src = r####"@Singleton class S { int n = 1 }
println S.instance.n
println S.instance.is(S.getInstance())
try { new S(); println "made" } catch (e) { println e.class.name + ": " + e.message }
@Singleton(lazy = true) class L { }
println "before"
println L.instance.getClass().name
println L.instance.is(L.instance)
@Singleton(property = 'only') class Q { }
println Q.only.getClass().name
try { new Q() } catch (e) { println e.message }
try { S.instance = null } catch (e) { println e.class.name }
@Singleton(strict = false) class T { int v; T() { v = 3 } }
println T.instance.v
class K { static int x = 1; static int getX() { 5 } }
println K.x
println K.@x
"####;
    let expected = r####"1
true
java.lang.RuntimeException: Can't instantiate singleton S. Use S.instance
before
L
true
Q
Can't instantiate singleton Q. Use Q.only
groovy.lang.ReadOnlyPropertyException
3
5
1
"####;
    check(src, expected);
}

/// An `@Immutable` class holds unmodifiable copies of its `List`, `Set` and `Map`
/// properties: an in-place write raises `UnsupportedOperationException` and a
/// later change to the passed collection does not show. They stayed mutable.
#[test]
fn immutable_collection_properties_are_frozen_copies() {
    let src = r####"import groovy.transform.Immutable
@Immutable class P { List xs; Map m; String n }
def p = new P(xs: [1], m: [a: 1], n: 'x')
try { p.xs << 2; println p.xs } catch (e) { println e.class.name }
try { p.xs.add(3); println p.xs } catch (e) { println e.class.name }
try { p.m.b = 2; println p.m } catch (e) { println e.class.name }
println p.xs.getClass().name
def src = [9]; def q = new P(xs: src, m: [:], n: 'y'); src << 10; println q.xs
println p
def r = new P([5], [k: 1], 'z'); try { r.xs << 6 } catch (e) { println e.class.name }; println r.xs
println(p == new P(xs: [1], m: [a: 1], n: 'x'))
@Immutable class Q2 { Set s }
def q2 = new Q2(s: [1, 2] as Set); try { q2.s << 3 } catch (e) { println e.class.name }; println q2.s; println q2.s.getClass().name
println p.m.getClass().name
"####;
    let expected = r####"java.lang.UnsupportedOperationException
java.lang.UnsupportedOperationException
java.lang.UnsupportedOperationException
java.util.Collections$UnmodifiableRandomAccessList
[9]
P([1], [a:1], x)
java.lang.UnsupportedOperationException
[5]
true
java.lang.UnsupportedOperationException
[1, 2]
java.util.Collections$UnmodifiableSet
java.util.Collections$UnmodifiableMap
"####;
    check(src, expected);
}

/// Declaring `compareTo` does not make a class `Comparable`: `<=>` and the
/// relational operators raise `IllegalArgumentException` for it, and `==` falls
/// back to identity. A declared `compareTo` stood in for `implements Comparable`.
#[test]
fn compare_to_without_comparable_is_not_comparable() {
    let src = r####"class V { int v; int compareTo(o) { v <=> o.v } }
class W implements Comparable { int v; int compareTo(o) { v <=> o.v } }
class X extends W { }
println(new W(v: 1) <=> new W(v: 2))
println(new W(v: 1) < new W(v: 2))
println(new X(v: 2) == new X(v: 2))
for (op in [{ a, b -> a <=> b }, { a, b -> a < b }, { a, b -> a >= b }]) {
    try { println op(new V(v: 1), new V(v: 2)) } catch (e) { println e.class.name }
}
println(new V(v: 1) == new V(v: 1))
def v = new V(v: 1); println(v == v)
println([new W(v: 3), new W(v: 1)].sort()*.v)
"####;
    let expected = r####"-1
true
true
java.lang.IllegalArgumentException
java.lang.IllegalArgumentException
java.lang.IllegalArgumentException
false
true
[1, 3]
"####;
    check(src, expected);
}

/// Every active category's declarations of a name are weighed together by the
/// receiver's and arguments' types, and the closest fit wins whichever order the
/// categories were applied in. The innermost category answered by arity alone.
#[test]
fn categories_select_by_receiver_type() {
    let src = r####"class SCat { static String hi(String s) { "S:$s" } }
class ICat { static String hi(Integer i) { "I:$i" } }
use(SCat, ICat) { println "a".hi(); println 5.hi() }
use(ICat, SCat) { println "a".hi(); println 5.hi() }
class OCat { static String hi(Object o) { "O:$o" } }
use(OCat, SCat) { println "a".hi(); println 5.hi() }
use(SCat, OCat) { println "a".hi(); println 5.hi() }
class NCat { static String twice(Number n) { "N:" + (n * 2) } }
class LCat { static String twice(Long n) { "L:" + (n * 2) } }
use(LCat, NCat) { println 3.twice(); println 3L.twice(); println 1.5.twice() }
class ACat { static String add(String s, Integer i) { "SI" }; static String add(String s, String t) { "SS" } }
use(ACat) { println "x".add(1); println "x".add("y") }
@Category(String) class Shout { String shout() { toUpperCase() + "!" } }
use(Shout) { println "hey".shout(); try { 5.shout() } catch (e) { println e.class.name } }
"####;
    let expected = r####"S:a
I:5
S:a
I:5
S:a
O:5
S:a
O:5
N:6
L:6
N:3.0
SI
SS
HEY!
groovy.lang.MissingMethodException
"####;
    check(src, expected);
}

/// An `assert` that runs inside another's condition (in a closure or a called
/// function) records and renders its own values; the enclosing one keeps its
/// own. The inner one's values replaced the outer's.
#[test]
fn nested_asserts_keep_their_own_values() {
    let src = r####"def x = 5; def a = [1, 2]
try { assert a.every { assert it > 0; it > x } } catch (AssertionError e) { println e.message }
try { assert a.every { assert it > 1; true } } catch (AssertionError e) { println e.message }
def chk(n) { assert n > 0; n > 3 }
try { assert chk(x - 4) && x > 9 } catch (AssertionError e) { println e.message }
try { assert chk(x) && x > 9 } catch (AssertionError e) { println e.message }
"####;
    let expected = r####"assert a.every { assert it > 0; it > x }
       | |
       | false
       [1, 2]
assert it > 1
       |  |
       1  false
assert chk(x - 4) && x > 9
       |   | |    |
       |   5 1    false
       false
assert chk(x) && x > 9
       |   |  |  | |
       |   5  |  5 false
       true   false
"####;
    check(src, expected);
}

/// A list element is spread across a closure's parameters only when the closure
/// has no one-argument `doCall`: `{ x, y = 9 -> }` binds the whole list to `x`,
/// and `{ x, y, z = 0 -> }` takes it spread. The defaulted form was spread.
#[test]
fn list_element_spread_needs_two_required_params() {
    let src = r####"println([[1], [2, null]].collect { x, y = 9 -> "$x $y" })
println([[1, 2]].collect { x, y -> "$x $y" })
println([[1, 2]].collect { x -> "$x" })
println([[1, 2, 3]].collect { x, y, z = 0 -> "$x $y $z" })
println([[1, 2]].each { x, y = 9 -> println "$x $y" })
def m = [a: 1]; m.each { k, v = 3 -> println "$k $v" }
println([[1, 2]].collect { x, y, z = 0 -> "$x $y $z" })
println([[1, 2], [3, 4]].findAll { a, b = 0 -> a instanceof List })
println([[1, 2]].collect { Object... xs -> xs.length })
"####;
    let expected = r####"[[1] 9, [2, null] 9]
[1 2]
[[1, 2]]
[1 2 3]
[1, 2] 9
[[1, 2]]
a 1
[1 2 0]
[[1, 2], [3, 4]]
[1]
"####;
    check(src, expected);
}

/// A `HashSet` decides membership by Java's `hashCode`/`equals`: `1` and `1.0`,
/// `1.0` and `1.00`, `1` and `1L`, and a `GString` and a `String` are distinct
/// elements, while a `TreeSet` and the GDK's `minus`/`intersect` still coerce.
/// Membership used Groovy's coercing `==`, so `[1, 1.0].toSet()` had one element.
#[test]
fn hash_sets_use_java_equality() {
    let src = r####"println([1, 1.0].toSet().size())
println([1, 1L].toSet().size())
println([1, 1.0d].toSet().size())
def s = new HashSet([1]); println s.contains(1.0); println s.add(1.0); println s
println([1, 1.0] as Set)
println([1, 1.0].unique())
println(new LinkedHashSet([1G, 1]).size())
println(new LinkedHashSet([1.0, 1.00]).size())
println(new LinkedHashSet(["a", "a"]).size())
println(([1, 2] as Set) + [2.0, 3])
println(([1, 2] as Set) - [2.0])
println(([1, 2] as Set).intersect([2.0]))
println(([1.0, 2] as Set).contains(1))
println(([1, 2] as Set) == ([1.0, 2] as Set))
println(([1, 1.0] as Set).size())
def h = new HashSet(); h << 1; h << 1L; h << (short) 1; h << 1.0f; h << 1.0d; h << 1.0; h << 1G; println h.size()
def m = [:] as HashMap; m[1] = 'a'; m[1L] = 'b'; println m.size()
println(([1, 2] as Set).unique().size())
def ls = [1, 1.0] as LinkedHashSet; ls.remove(1.0); println ls
println(([3, 3.0, 'a', "a"] as Set).size())
def x = 2; println((["a$x", "a2"] as Set).size())
println(([[1], [1.0]] as Set).size())
"####;
    let expected = r####"2
2
2
false
true
[1, 1.0]
[1, 1.0]
[1]
2
2
1
[1, 2, 2.0, 3]
[1]
[2]
false
true
2
7
2
2
[1]
3
2
2
"####;
    check(src, expected);
}

/// `java.lang.Character` is its own value: `as char`, `(char)`, a `char` local,
/// field or parameter, `charAt`, `toCharArray` and `Character.valueOf` make one.
/// It prints as its character, adds and compares as its code point, is `==` to
/// its one-character string but not `equals` it, and answers the `Character`
/// methods. It was a one-character `String`.
#[test]
fn character_is_its_own_type() {
    let src = r####"def c = 'c' as char
def t(Closure k) { try { println k() } catch (e) { println e.class.name } }
t { c }
t { c.getClass().name }
t { c instanceof Character }; t { c instanceof String }; t { c instanceof Comparable }; t { c instanceof Number }
t { c == "c" }; t { "c" == c }; t { c == 99 }; t { c.equals("c") }; t { c.equals("c" as char) }
t { c + 1 }; t { (c + 1).getClass().name }; t { c - ("a" as char) }; t { c + "x" }; t { "x" + c }
t { (c * 2) }
t { c < ("d" as char) }; t { c > 50 }
def d = c; d++; t { d }; t { d.getClass().name }
t { "${c}!" }
t { c.inspect() }
t { c.toString().getClass().name }
t { c as int }; t { (int) c }; t { c as String }
t { c.hashCode() }
t { c.isLetter() }; t { c.isDigit() }; t { c.toUpperCase() }; t { c.toUpperCase().getClass().name }
t { "abc".charAt(1) }; t { "abc".charAt(1).getClass().name }
t { "abc".toCharArray()[0].getClass().name }
t { ["c" as char].contains("c") }; t { ["c" as char].contains(c) }
t { ([c, 'd' as char].join('-')) }
t { "abcabc".indexOf(c) }
t { "abc".replace(c, 'z' as char) }
t { def sb = new StringBuilder(); sb.append(c); sb << c; sb }
switch (c) { case 'c': println "case str"; break; default: println "default" }
t { Character.valueOf(c).getClass().name }
t { Character.isLetter(c) }
t { (char) 65 }; t { ((char) 65).getClass().name }
char e = "e"; t { e.getClass().name }; t { e + 1 }
char f = 102; t { f }
t { ([c] as Set).contains(c) }
t { (c <=> ('a' as char)) }
t { c.next() }
t { ([b: c]) }
t { String.valueOf(c) }
t { "c".toCharacter().getClass().name }
t { (c in ['c']) }
t { (c.compareTo('a' as char)) }
t { (c as Character) }
t { (Character.toUpperCase(c)) }
t { ((c as int) + 1) }
t { c.charValue() }
t { "abcabc".indexOf(c) }
t { "abcabc".lastIndexOf(c) }
t { "abc".contains(c) }
t { "abc".split(c) }
t { "a,b".tokenize(',' as char) }
t { "abc".count(c) }
t { "abc" - c }
t { "abc".startsWith(c) }
t { [a: 1][c] }
t { def m = [:]; m[c] = 1; m['c'] = 2; m.size() }
t { def m = [:]; m[c] = 1; m.keySet()[0].getClass().name }
t { ("a" as char)..("e" as char) }
t { (('a' as char)..('c' as char)).collect { it.getClass().name } }
t { c.class }
t { [c, c].unique().size() }
t { [c] == ['c'] }
t { "${c}".length() }
t { c.toString() * 2 }
t { String.valueOf(c) + "!" }
t { new StringBuilder("ab").append(c).toString() }
t { (int) 'c' }
t { ((char) 'c').getClass().name }
t { c.asType(Integer) }
t { Character.isDigit('5' as char) }
t { 'x'.toCharacter() == 'x' }
t { switch (c) { case Character: return "char"; default: return "no" } }
t { switch ('c') { case c: return "hit"; default: return "no" } }
t { [c].sum() }
t { c ? 'y' : 'n' }
t { ('\0' as char) ? 'y' : 'n' }
t { -c }
def fc(char c) { c.getClass().name }
def g(Character c) { c.getClass().name }
t { fc('x') }; t { fc('x' as char) }; t { g('x' as char) }
class P { char c; Character d }
t { def p = new P(c: 'q', d: 'r' as char); p.c.getClass().name + " " + p.d.getClass().name }
char[] arr = ['a', 'b'] as char[]
t { arr[0].getClass().name }; t { arr }; t { new String(arr) }; t { ("abc" as char[]).length }
t { def cl = { char z -> z.getClass().name }; cl('k') }
t { def cl = { char z -> z.getClass().name }; cl('k' as char) }
t { char x = 'x'; x.getClass().name }
t { Character y = 'y'; y.getClass().name }
"####;
    let expected = r####"c
java.lang.Character
true
false
true
false
true
true
true
false
true
100
java.lang.Integer
2
groovy.lang.MissingMethodException
xc
198
true
true
d
java.lang.Character
c!
c
java.lang.String
99
99
c
99
true
false
C
java.lang.Character
b
java.lang.Character
java.lang.Character
false
true
c-d
groovy.lang.MissingMethodException
abz
cc
case str
java.lang.Character
true
A
java.lang.Character
java.lang.Character
102
f
true
1
d
[b:c]
c
java.lang.Character
false
1
c
C
100
c
groovy.lang.MissingMethodException
groovy.lang.MissingMethodException
groovy.lang.MissingMethodException
groovy.lang.MissingMethodException
[a, b]
groovy.lang.MissingMethodException
ab
groovy.lang.MissingMethodException
null
2
java.lang.Character
a..e
[java.lang.Character, java.lang.Character, java.lang.Character]
class java.lang.Character
1
true
1
cc
c!
abc
99
java.lang.Character
99
true
true
char
no
c
y
n
groovy.lang.MissingMethodException
groovy.lang.MissingMethodException
java.lang.Character
java.lang.Character
java.lang.Character java.lang.Character
java.lang.Character
ab
ab
3
groovy.lang.MissingMethodException
java.lang.Character
java.lang.Character
java.lang.Character
"####;
    check(src, expected);
}

/// `flatten()` flattens a nested range or set into its elements, as it does a
/// list: they are `Collection`s. They were kept whole.
#[test]
fn flatten_expands_ranges_and_sets() {
    let src = r####"println([(1..3)].flatten())
println([('a'..'c'), [4, [5]]].flatten())
println([[1, 2] as Set, 3].flatten())
println([(1..<1), 2].flatten())
println([[a: 1], 2].flatten())
println([(1..2).step(1)].flatten())
println([new LinkedHashSet([[6, 7]])].flatten())
"####;
    let expected = r####"[1, 2, 3]
[a, b, c, 4, 5]
[1, 2, 3]
[2]
[[a:1], 2]
[1, 2]
[6, 7]
"####;
    check(src, expected);
}

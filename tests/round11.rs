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

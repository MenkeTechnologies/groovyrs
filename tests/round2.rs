//! Round-2 parity regressions: each program's expected stdout was captured from
//! Apache Groovy 6.0.0 and frozen here, so the suite needs no JVM. A divergence
//! in one of these is a groovyrs bug; regenerate an expectation only by running
//! the program under a real `groovy`.

use std::process::Command;

/// Run a Groovy source string through the `groovy` binary and return
/// (stdout, ok).
fn run(src: &str) -> (String, bool) {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("groovyrs_round2_{}.groovy", fasthash(src)));
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
fn closures_follow_declared_signature_on_direct_calls() {
    // bare call(), owner/thisObject/delegate, direct-call arity, curry limits, composition
    let src = r####"// bare call() recursion, owner/thisObject/delegate, direct-call arity
def fact = { n -> n <= 1 ? 1 : n * call(n - 1) }
println fact(6)
def fibo = { n -> n < 2 ? n : call(n - 1) + call(n - 2) }
println fibo(10)

class Host {
    def n = 'host'
    def mk() { return { -> n } }
    def nested() { return { -> { -> n } } }
}
def h = new Host()
def c = h.mk()
println c()
println c.owner.is(h)
println c.thisObject.is(h)
println c.delegate.is(h)
def inner = h.nested()()
println inner.owner.is(h) == false
println inner.thisObject.is(h)
def top = { -> 1 }
println top.owner.getClass().getName() == top.thisObject.getClass().getName()
top.delegate = [x: 1]
println top.delegate
println top.owner != top.delegate

def two = { a, b -> a + b }
def show = { Closure f, Object... args ->
    try { println f(*args) } catch (e) { println e.getClass().getSimpleName() }
}
show(two, 1, 2)
show(two, 1)
show(two)
show(two, 1, 2, 3)
show(two, [4, 5])
def one = { x -> x }
show(one)
show(one, 1, 2)
def none = { -> 42 }
show(none)
show(none, 1)
def dflt = { a, b = 10 -> a + b }
show(dflt, 1)
show(dflt, 1, 2)
show(dflt, 1, 2, 3)
def typed = { String s, int n -> s * n }
show(typed, 'ab', 2)
show(typed, 5, 2)
show(typed, 'ab', 'x')
def va = { Object... xs -> xs.size() }
show(va)
show(va, 1, 2, 3)
println two.curry(1)(2)
println two.rcurry(1)(2)
try { two.curry(1, 2, 3) } catch (e) { println e.getClass().getSimpleName() }
try { none.curry(1) } catch (e) { println e.getClass().getSimpleName() }
println two.curry(1).maximumNumberOfParameters
def three = { p, q, r -> "$p$q$r" }
println three.ncurry(1, 'M')('L', 'R')
println three.curry('x', 'y')('z')
println three.rcurry('z')('x', 'y')
def comp1 = two >> { it * 10 }
println comp1(1, 2)
def comp2 = { it * 10 } << two
println comp2(1, 2)
println two.memoize()(3, 4)
"####;
    let expected = r####"720
55
host
true
true
true
true
true
true
[x:1]
true
3
MissingMethodException
MissingMethodException
MissingMethodException
9
null
MissingMethodException
42
MissingMethodException
11
3
MissingMethodException
abab
MissingMethodException
MissingMethodException
0
3
3
3
IllegalArgumentException
IllegalArgumentException
1
LMR
xyz
xyz
30
30
7
"####;
    check(src, expected);
}

#[test]
fn trampoline_comparator_runnable_and_interface_proxies() {
    // trampoline recursion, closures coerced to JDK and script interfaces
    let src = r####"def total
total = { n, acc = 0 -> n == 0 ? acc : total.trampoline(n - 1, acc + n) }.trampoline()
println total(10000)
def isEven, isOdd
isEven = { n -> n == 0 ? true : isOdd.trampoline(n - 1) }.trampoline()
isOdd = { n -> n == 0 ? false : isEven.trampoline(n - 1) }.trampoline()
println isEven(1001)
println isOdd(1001)

Runnable r = { println 'ran' }
r.run()
def r2 = { println 'ran2' } as Runnable
r2.run()
println r2 instanceof Runnable
println r2 instanceof Closure
java.util.concurrent.Callable cl = { 7 }
println cl.call()
Comparator cmp = { a, b -> b <=> a }
println([3, 1, 2].sort(cmp))
println cmp.compare(1, 2)

interface Greeter { String greet(String who) }
interface Shouter extends Greeter { String shout(String who) }
def g = { who -> "hi $who" } as Greeter
println g.greet('bob')
println g instanceof Greeter
def m = [greet: { who -> "yo " + who }] as Greeter
println m.greet('al')
def s = [greet: { w -> "g " + w }, shout: { w -> w.toUpperCase() }] as Shouter
println s.shout('q')
println s.greet('z')
println s instanceof Greeter
println s instanceof Shouter
interface Defaulted { String name(); default String hello() { 'hello ' + name() } }
def d = [name: { -> 'dd' }] as Defaulted
println d.hello()
"####;
    let expected = r####"50005000
false
true
ran
ran2
true
true
7
null
1
hi bob
true
yo al
Q
g z
true
true
hello dd
"####;
    check(src, expected);
}

#[test]
fn try_with_resources_closes_in_reverse_before_catch() {
    // try-with-resources ordering with catch, finally, return, null and existing resources
    let src = r####"class Res implements AutoCloseable {
    String name
    boolean failClose
    Res(String name, boolean failClose = false) { this.name = name; this.failClose = failClose; println "open $name" }
    void close() { println "close $name"; if (failClose) throw new IllegalStateException("close $name") }
}
try (def a = new Res('a'); def b = new Res('b')) {
    println 'body'
}
println '--'
try (Res a = new Res('a'); Res b = new Res('b')) {
    println 'throwing'
    throw new RuntimeException('boom')
} catch (RuntimeException e) {
    println 'caught ' + e.message
} finally {
    println 'finally'
}
println '--'
try (Res a = new Res('x')) {
    println 'in'
} finally {
    println 'fin only'
}
println '--'
def f() {
    try (Res a = new Res('r')) {
        return 'returned'
    } finally {
        println 'f-finally'
    }
}
println f()
println '--'
try (Res a = new Res('c', true)) {
    println 'with failing close'
} catch (IllegalStateException e) {
    println 'caught ' + e.message
}
println '--'
def existing = new Res('ex')
try (existing) {
    println 'using existing'
}
println '--'
try (Res n = null) {
    println 'null resource ok'
}
for (i in 1..2) {
    try (Res l = new Res("loop$i")) {
        if (i == 1) continue
        println 'second'
    }
}
"####;
    let expected = r####"open a
open b
body
close b
close a
--
open a
open b
throwing
close b
close a
caught boom
finally
--
open x
in
close x
fin only
--
open r
close r
f-finally
returned
--
open c
with failing close
close c
caught close c
--
open ex
using existing
close ex
--
null resource ok
open loop1
close loop1
open loop2
second
close loop2
"####;
    check(src, expected);
}

#[test]
fn deques_priority_queues_stacks_and_seeded_random() {
    // ArrayDeque, PriorityQueue heap order, Stack, LinkedList, java.util.Random sequences
    let src = r####"def dq = new ArrayDeque()
dq.add(1); dq.addFirst(0); dq.addLast(2); dq.offer(3); dq.push(-1)
println dq
println dq.peekFirst()
println dq.peekLast()
println dq.pollFirst()
println dq.pollLast()
println dq.pop()
println dq
println dq.size()
println dq.contains(1)
println dq.getClass().getName()
println dq instanceof Deque
println dq instanceof Queue
println dq instanceof List
try { new ArrayDeque().removeFirst() } catch (e) { println e.getClass().getName() }
try { new ArrayDeque().element() } catch (e) { println e.getClass().getName() }
println new ArrayDeque().poll()
println new ArrayDeque().peek()
try { new ArrayDeque().add(null) } catch (e) { println e.getClass().getName() }

def pq = new PriorityQueue()
[9, 4, 7, 1, 8, 2, 6].each { pq.add(it) }
println pq
println pq.peek()
pq.remove(7)
println pq
def drained = []
while (!pq.isEmpty()) drained << pq.poll()
println drained
def maxq = new PriorityQueue({ a, b -> b <=> a } as Comparator)
maxq.addAll([3, 9, 1, 5])
println maxq
println maxq.poll()
println new PriorityQueue(['pear', 'fig', 'apple', 'kiwi'])
println new PriorityQueue([5, 3, 8, 1, 9, 2])
try { new PriorityQueue().remove() } catch (e) { println e.getClass().getName() }
println new PriorityQueue().poll()
println pq.getClass().getName()

def st = new Stack()
st.push('a'); st.push('b'); st.push('c')
println st
println st.peek()
println st.pop()
println st.search('a')
println st.empty()
println st.size()
println st.collect { it.toUpperCase() }
println st.getClass().getName()
println st instanceof List
try { new Stack().pop() } catch (e) { println e.getClass().getName() }
try { new Stack().peek() } catch (e) { println e.getClass().getName() }

def ll = new LinkedList([1, 2, 3])
ll.addFirst(0)
ll << 4
println ll
println ll.removeFirst()
println ll.removeLast()
println ll.peek()
println ll.poll()
println ll
println ll.getFirst()
println ll.getLast()
println ll.indexOf(3)
println ll.getClass().getName()
println ll instanceof List
println ll instanceof Deque
println ll instanceof ArrayList

def r = new Random(42)
println([r.nextInt(100), r.nextInt(100), r.nextInt(100)])
println r.nextInt()
println r.nextLong()
println r.nextDouble()
println r.nextBoolean()
println r.nextGaussian()
println r.nextInt(5, 10)
def r2 = new Random(42)
println r2.nextInt(100)
r2.setSeed(42)
println r2.nextInt(100)
def shuffled = (1..10).toList()
Collections.shuffle(shuffled, new Random(7))
println shuffled
println r.getClass().getName()
"####;
    let expected = r####"[-1, 0, 1, 2, 3]
-1
3
-1
3
0
[1, 2]
2
true
java.util.ArrayDeque
true
true
false
java.util.NoSuchElementException
java.util.NoSuchElementException
null
null
java.lang.NullPointerException
[1, 4, 2, 9, 8, 7, 6]
1
[1, 4, 2, 9, 8, 6]
[1, 2, 4, 6, 8, 9]
[9, 5, 1, 3]
9
[apple, fig, pear, kiwi]
[1, 3, 2, 5, 9, 8]
java.util.NoSuchElementException
null
java.util.PriorityQueue
[a, b, c]
c
c
2
false
2
[A, B]
java.util.Stack
true
java.util.EmptyStackException
java.util.EmptyStackException
[0, 1, 2, 3, 4]
0
4
1
1
[2, 3]
2
3
1
java.util.LinkedList
true
true
false
[30, 63, 48]
205897768
5694868678511409995
0.27707849007413665
true
-0.8761154839986063
7
30
30
[1, 2, 10, 4, 8, 5, 9, 6, 3, 7]
java.util.Random
"####;
    check(src, expected);
}

#[test]
fn unmodifiable_immutable_and_fixed_size_collections() {
    // wrapper collections refuse writes with UnsupportedOperationException
    let src = r####"def t = { String label, Closure c -> try { println "$label = ${c()}" } catch (e) { println "$label ! ${e.getClass().getName()}" } }
def src = [3, 1, 2]
def im = src.asImmutable()
src << 9
t('imm list', { im })
t('imm add', { im << 4 })
t('imm set', { im[0] = 1 })
t('imm sort', { im.sort() })
t('imm sorted copy', { im.sort(false) })
t('imm clear', { im.clear() })
t('imm remove', { im.remove(0) })
t('imm class', { im.getClass().getName() })
def view = Collections.unmodifiableList(src)
t('unmod add', { view.add(1) })
t('unmod class', { view.getClass().getName() })
t('unmod read', { view.size() })
def lo = List.of(1, 2, 3)
t('List.of', { lo })
t('List.of add', { lo.add(4) })
t('List.of class', { lo.getClass().getName() })
t('List.of small', { List.of(1).getClass().getName() })
t('List.of null', { List.of(1, null) })
t('List.of contains', { lo.contains(2) })
def al = Arrays.asList(1, 2, 3)
t('asList', { al })
t('asList add', { al.add(4) })
t('asList remove', { al.remove(0) })
al[0] = 99
t('asList set', { al })
t('asList class', { al.getClass().getName() })
def ms = [a: 1].asImmutable()
t('imm map', { ms })
t('imm map put', { ms.put('b', 2) })
t('imm map assign', { ms.b = 2 })
t('imm map class', { ms.getClass().getName() })
t('Map.of', { Map.of('k', 1).get('k') })
t('Map.of put', { Map.of('k', 1).put('j', 2) })
t('Map.of dup', { Map.of('k', 1, 'k', 2) })
def ss = ([1, 2] as Set).asImmutable()
t('imm set', { ss })
t('imm set add', { ss.add(3) })
t('imm set class', { ss.getClass().getName() })
t('Set.of dup', { Set.of(1, 1) })
t('empty list', { Collections.emptyList().add(1) })
t('empty class', { Collections.emptyList().getClass().getName() })
t('singleton list', { Collections.singletonList(1).add(2) })
t('nCopies', { Collections.nCopies(3, 'x') })
t('nCopies add', { Collections.nCopies(3, 'x').add('y') })
t('unmod map', { Collections.unmodifiableMap([a: 1]).put('b', 1) })
t('unmod set', { Collections.unmodifiableSet([1] as Set).add(2) })
t('copy still mutable', { def c = new ArrayList(im); c << 5; c })
"####;
    let expected = r####"imm list = [3, 1, 2]
imm add ! java.lang.UnsupportedOperationException
imm set ! java.lang.UnsupportedOperationException
imm sort ! java.lang.UnsupportedOperationException
imm sorted copy = [1, 2, 3]
imm clear ! java.lang.UnsupportedOperationException
imm remove ! java.lang.UnsupportedOperationException
imm class = java.util.Collections$UnmodifiableRandomAccessList
unmod add ! java.lang.UnsupportedOperationException
unmod class = java.util.Collections$UnmodifiableRandomAccessList
unmod read = 4
List.of = [1, 2, 3]
List.of add ! java.lang.UnsupportedOperationException
List.of class = java.util.ImmutableCollections$ListN
List.of small = java.util.ImmutableCollections$List12
List.of null ! java.lang.NullPointerException
List.of contains = true
asList = [1, 2, 3]
asList add ! java.lang.UnsupportedOperationException
asList remove ! java.lang.UnsupportedOperationException
asList set = [99, 2, 3]
asList class = java.util.Arrays$ArrayList
imm map = [a:1]
imm map put ! java.lang.UnsupportedOperationException
imm map assign ! java.lang.UnsupportedOperationException
imm map class = java.util.Collections$UnmodifiableMap
Map.of = 1
Map.of put ! java.lang.UnsupportedOperationException
Map.of dup ! java.lang.IllegalArgumentException
imm set = [1, 2]
imm set add ! java.lang.UnsupportedOperationException
imm set class = java.util.Collections$UnmodifiableSet
Set.of dup ! java.lang.IllegalArgumentException
empty list ! java.lang.UnsupportedOperationException
empty class = java.util.Collections$EmptyList
singleton list ! java.lang.UnsupportedOperationException
nCopies = [x, x, x]
nCopies add ! java.lang.UnsupportedOperationException
unmod map ! java.lang.UnsupportedOperationException
unmod set ! java.lang.UnsupportedOperationException
copy still mutable = [3, 1, 2, 5]
"####;
    check(src, expected);
}

#[test]
fn gstring_closure_values_evaluate_lazily() {
    // a closure interpolated into a GString runs when the text is read
    let src = r####"def x = 5
def eager = "v=${x}"
def lazy = "v=${-> x}"
x = 9
println eager
println lazy
println lazy.toString()
println lazy.length()
println lazy == 'v=9'
println 'v=9' == lazy
println lazy.toUpperCase()
println lazy + '!'
println lazy.getClass().getName()
println lazy instanceof GString
println lazy instanceof String
println lazy instanceof CharSequence
def n = 1
def multi = "a${-> n}b${-> n * 2}c"
n = 4
println multi
n = 5
println multi
def calls = 0
def counted = "c=${-> ++calls}"
println counted
println counted
println calls
def f = { -> 'from closure' }
println "${f}"
println "pre ${f} post"
def w = "x${ { out -> out << 'written' } }y"
println w
def list = ["a${-> 1}", 'b']
println list
println list*.size()
println list.join(',')
def map = [k: "v${-> 2}"]
println map
println map.k.size()
switch ("q${-> 1}") { case 'q1': println 'matched'; break; default: println 'no' }
println "${-> [1, 2].collect { it * 2 }}"
println "${-> null}"
println "nested ${-> "inner ${-> 'deep'}"}"
def sb = new StringBuilder()
sb << "s${-> 3}"
println sb
println "${-> 1}${-> 2}".length()
println ("a${-> 'b'}" as String).getClass().getName()
println "t${-> 7}".trim()
println "t${-> 7}".split('t').toList()
println "t${-> 7}".bytes.length
"####;
    let expected = r####"v=5
v=9
v=9
3
true
true
V=9
v=9!
org.codehaus.groovy.runtime.GStringImpl
true
false
true
a4b8c
a5b10c
c=1
c=2
2
from closure
pre from closure post
xwritteny
[a1, b]
[2, 1]
a1,b
[k:v2]
2
matched
[2, 4]
null
nested inner deep
s3
2
ab
t7
[, 7]
2
"####;
    check(src, expected);
}

#[test]
fn java_math_context_rounding_and_biginteger_methods() {
    // MathContext, RoundingMode statics, BigInteger number theory
    let src = r####"import java.math.MathContext
import java.math.RoundingMode
def d = 123.456G
println d.round(new MathContext(4))
println d.round(new MathContext(2))
println 999.5G.round(new MathContext(3))
println d.round(new MathContext(2, RoundingMode.DOWN))
println 1.0G.divide(3G, new MathContext(5))
println 10.0G.divide(4G, MathContext.DECIMAL32)
println 1.5G.add(2.25G, new MathContext(2))
println 2.0G.sqrt(new MathContext(10))
println 4.0G.sqrt(new MathContext(10))
println 16.00G.sqrt(new MathContext(10))
println new MathContext(7)
println MathContext.DECIMAL64
println MathContext.DECIMAL128.getPrecision()
println new MathContext(3, RoundingMode.FLOOR).getRoundingMode()
println d.setScale(1, RoundingMode.HALF_UP)
println d.setScale(0, RoundingMode.CEILING)
println d.setScale(2, BigDecimal.ROUND_DOWN)
println BigDecimal.ROUND_HALF_UP
println RoundingMode.valueOf('UP')
println RoundingMode.values().size()
println 12.5G.movePointLeft(3)
println 12.5G.movePointRight(3)
println 5.0G.divideToIntegralValue(2G)
println 1E+3G.toEngineeringString()
println 12345.678G.toEngineeringString()
println 1.5G.max(2.5G)
println((-1.5G).signum())
println 1.5G.scaleByPowerOfTen(3)
println 15G.gcd(10G)
println((-5G).bitLength())
println 255G.bitLength()
println 255G.bitCount()
println 5G.testBit(2)
println 5G.setBit(1)
println 5G.clearBit(0)
println 5G.flipBit(3)
println 97G.isProbablePrime(10)
println 91G.isProbablePrime(10)
println 100G.nextProbablePrime()
println 4G.modPow(13G, 497G)
println 3G.modInverse(7G)
println 100G.sqrt()
println new BigInteger('ff', 16)
println new BigInteger('-1010', 2)
println BigInteger.valueOf(Long.MAX_VALUE) + 1
println BigInteger.valueOf(7).pow(30)
println BigDecimal.valueOf(25, 1)
println BigDecimal.valueOf(2.5)
println 7G.max(9G)
println 7G.signum()
try { 100G.setScale(2) } catch (e) { println e.getClass().getSimpleName() }
try { 100G.precision() } catch (e) { println e.getClass().getSimpleName() }
try { 1.5G.intValueExact() } catch (e) { println e.getClass().getSimpleName() }
try { 5G.modInverse(0G) } catch (e) { println e.getClass().getSimpleName() }
try { 2G.modInverse(4G) } catch (e) { println e.message }
println 1.5G.byteValue()
println 300G.byteValue()
println 70000.7G.shortValue()
println 3.99.byteValue()
println (+d)
println (+3)
"####;
    let expected = r####"123.5
1.2E+2
1.00E+3
1.2E+2
0.33333
2.5
3.8
1.414213562
2.0
4.0
precision=7 roundingMode=HALF_UP
precision=16 roundingMode=HALF_EVEN
34
FLOOR
123.5
124
123.45
4
UP
8
0.0125
12500
2.0
1E+3
12345.678
2.5
-1
1.5E+3
5
3
8
8
true
7
4
13
true
false
101
445
5
10
255
-10
9223372036854775808
22539340290692258087863249
2.5
2.5
9
1
MissingMethodException
MissingMethodException
ArithmeticException
ArithmeticException
BigInteger not invertible.
1
44
4464
3
123.456
3
"####;
    check(src, expected);
}

#[test]
fn stringbuilder_surface_and_bounds() {
    // StringBuilder methods and JDK bounds exceptions
    let src = r####"def sb = new StringBuilder('hello world')
println sb.length()
println sb.charAt(1)
println sb[0]
println sb[-1]
println sb.indexOf('o')
println sb.indexOf('o', 5)
println sb.lastIndexOf('o')
println sb.substring(2)
println sb.substring(1, 4)
println sb.subSequence(0, 5)
sb.setCharAt(0, 'J' as char)
println sb
sb.insert(0, '>> ').append('!').append(1).append(2.5).append(true).append([1, 2])
println sb
println sb.deleteCharAt(0)
println sb.delete(0, 2)
println sb.replace(0, 5, 'HELLO')
sb.setLength(5)
println sb
sb.setLength(7)
println sb.length()
println sb.reverse()
println sb.capacity() >= 7
println sb.toString().size()
def b2 = new StringBuilder()
b2 << 'a' << 1 << 'b'
println b2
println b2.append('xyz', 1, 2)
println b2.appendCodePoint(65)
println b2.contains('b')
println b2.take(2)
println b2.drop(2)
println b2.padLeft(10, '*')
println b2.replaceAll('[0-9]', '#')
println b2.each { }
println b2.isEmpty()
println new StringBuilder().isEmpty()
println b2.compareTo(new StringBuilder('a1'))
println b2 == b2
println b2.equals(new StringBuilder('a1bxA'))
try { b2.charAt(99) } catch (e) { println e.getClass().getSimpleName() }
try { b2.deleteCharAt(99) } catch (e) { println e.getClass().getSimpleName() }
try { b2.insert(99, 'x') } catch (e) { println e.getClass().getSimpleName() }
try { b2.setCharAt(99, 'x' as char) } catch (e) { println e.getClass().getSimpleName() }
try { b2.substring(3, 1) } catch (e) { println e.getClass().getSimpleName() }
try { b2.startsWith('a') } catch (e) { println e.getClass().getSimpleName() }
def buf = new StringBuffer('abc')
buf.append('d').reverse()
println buf
println buf.getClass().getName()
println 'abc'.chars().sum()
println new StringBuilder('xyz').chars().sum()
"####;
    let expected = r####"11
e
h
d
4
7
7
llo world
ell
hello
Jello world
>> Jello world!12.5true[1, 2]
> Jello world!12.5true[1, 2]
Jello world!12.5true[1, 2]
HELLO world!12.5true[1, 2]
HELLO
7
  OLLEH
true
7
a1b
a1by
a1byA
true
a1
byA
*****a1byA
a#byA
a1byA
false
true
3
true
false
StringIndexOutOfBoundsException
StringIndexOutOfBoundsException
StringIndexOutOfBoundsException
StringIndexOutOfBoundsException
StringIndexOutOfBoundsException
MissingMethodException
dcba
java.lang.StringBuffer
294
363
"####;
    check(src, expected);
}

#[test]
fn misc_safe_subscript_trait_super_and_class_members() {
    // safe subscript, Trait.super, Class members, unary plus
    let src = r####"def nul = null
println nul?[0]
println nul?.foo
def lst = [10, 20, 30]
println lst?[1]
println lst?[1, 2]
println lst?[0..1]
def mp = [a: 1]
println mp?['a']
println mp?.a
println 1 + [2]
println 2 * [3]
println 10 - [3]
println 1.5 + [2]
try { 1 + [1, 2] } catch (e) { println e.getClass().getSimpleName() }
println (+5)
println(+lst.size())
class Cls { static boolean isCase(Object o) { o == 'magic' } }
switch ('magic') { case Cls: println 'matched'; break; default: println 'no' }
switch ('other') { case Cls: println 'matched'; break; default: println 'no' }
trait Walker { String move() { 'walk' } }
trait Swimmer { String move() { 'swim' } }
class Duck implements Walker, Swimmer {
    String move() { Walker.super.move() + '+' + Swimmer.super.move() }
}
println new Duck().move()
class Animal { String sound() { 'generic' } }
class Dog extends Animal implements Walker { String sound() { super.sound() + '/bark/' + Walker.super.move() } }
println new Dog().sound()
println Dog.getSuperclass().getName()
println Dog.getSuperclass() == Animal
println Animal.getSuperclass().getName()
println Dog.isInstance(new Dog())
println Animal.isInstance(new Dog())
println Dog.isInstance(new Animal())
println Animal.isAssignableFrom(Dog)
println Dog.isAssignableFrom(Animal)
println Dog.getInterfaces()*.getName()
println 'abc'.getClass().getSuperclass().getName()
println Integer.getSuperclass().getName()
try { new ArrayList(-1) } catch (e) { println e.getClass().getSimpleName() + ': ' + e.message }
try { throw new IllegalStateException('x') } catch (e) { println e.stackTrace.length > 0 }
try { null.foo = 1 } catch (e) { println e.message }
try { def z = null; z[0] = 1 } catch (e) { println e.getClass().getSimpleName() + ': ' + e.message }
try { def z = null; z[0] } catch (e) { println e.getClass().getSimpleName() + ': ' + e.message }
println 'abc'.getAt(1)
println([3, 1, 2].asUnmodifiable().sort(false))
println 5.asType(int).getClass().getName()
println '7'.asType(Integer) + 1
"####;
    let expected = r####"null
null
20
[20, 30]
[10, 20]
1
1
3
6
7
3.5
MissingMethodException
5
3
matched
no
walk+swim
generic/bark/walk
Animal
true
java.lang.Object
true
true
false
true
false
[Walker]
java.lang.Object
java.lang.Number
IllegalArgumentException: Illegal Capacity: -1
true
Cannot set property 'foo' on null object
NullPointerException: Cannot invoke method putAt() on null object
NullPointerException: Cannot invoke method getAt() on null object
b
[1, 2, 3]
java.lang.Integer
8
"####;
    check(src, expected);
}

#[test]
fn map_keys_keep_their_type() {
    // non-String map keys stay Integer, order numerically in TreeMap and sort, keyword keys are Strings
    let src = r####"def m = [1: 'a', 2: 'b', 10: 'c']
m.each { k, v -> println k + 1 }
println m.keySet().collect { it * 2 }
println m.keySet()*.getClass()*.getSimpleName()
println m[1]
println m['1']
println m.containsKey(1)
println m.containsKey('1')
def mixed = [(1): 'x', ('1'): 'y']
println mixed
println mixed.size()
def d = [1.5: 'a', 2.5: 'b']
println d.keySet()*.getClass()*.getSimpleName()
println d[1.5]
println m.sort()
println m.keySet().sort()
println new TreeMap(m)
println new TreeMap(m).firstKey()
println new TreeMap(m).headMap(10)
println new HashMap([10: 'x', 2: 'y', 33: 'z', 4: 'w'])
println([3, 1, 2, 10].groupBy { it % 3 })
println([1, 22, 333].countBy { it.toString().size() })
println([10, 9, 100].collectEntries { [(it): it * 2] }.keySet().sort())
println m.inspect()
def kw = [true: 1, false: 0, null: 'n', if: 2, class: 3, 'a b': 4]
println kw
println kw.keySet()*.getClass()*.getSimpleName()
println kw.true
println kw['true']
println kw[true]
def lk = [[1, 2]: 'p']
println lk[[1, 2]]
println lk.keySet()*.getClass()*.getSimpleName()
"####;
    let expected = r####"2
3
11
[2, 4, 20]
[Integer, Integer, Integer]
a
null
true
false
[1:x, 1:y]
2
[BigDecimal, BigDecimal]
a
[1:a, 2:b, 10:c]
[1, 2, 10]
[1:a, 2:b, 10:c]
1
[1:a, 2:b]
[33:z, 10:x, 2:y, 4:w]
[0:[3], 1:[1, 10], 2:[2]]
[1:1, 2:1, 3:1]
[9, 10, 100]
[1:'a', 2:'b', 10:'c']
[true:1, false:0, null:n, if:2, class:3, a b:4]
[String, String, String, String, String, String]
1
1
null
p
[ArrayList]
"####;
    check(src, expected);
}

#[test]
fn subscript_bounds_comparisons_and_equality_details() {
    // list and string range subscripts, comparison operators on null and Boolean, universal equals
    let src = r####"def t = { l, c -> try { println l + ' = ' + c() } catch (e) { println l + ' ! ' + e.getClass().getName() + ': ' + (e.message ?: "").readLines()[0] } }
t('list[5..6]') { [1, 2, 3][5..6] }
t('list[1..5]') { [1, 2, 3][1..5] }
t('list[-5..-1]') { [1, 2, 3][-5..-1] }
t('list[2..0]') { [1, 2, 3][2..0] }
t('list[-1..-3]') { [1, 2, 3][-1..-3] }
t('str[1..5]') { 'abc'[1..5] }
t('str[-5..-1]') { 'ab'[-5..-1] }
t('str[2..0]') { 'x'[2..0] }
t('rrange[1..2]') { (5..1)[1..2] }
t('rrange[2..0]') { (5..1)[2..0] }
t('crrange[1..2]') { ('e'..'a')[1..2] }
t('inject-empty') { [].inject { x, y -> x + y } }
t('r.asList') { (1..3).asList().getClass().getSimpleName() }
t('r.sort') { (1..3).sort { -it } }
t('r.sort.empty') { (1..<1).sort { -it } }
t('dec.isCase') { (1.0..3.0).isCase(3) }
t('step0') { (1..10).step(0) }
t('null<1') { null < 1 }
t('1<null') { 1 < null }
t('list<list') { [1, 2] < [1, 3] }
t('bool<int') { true < 1 }
t('0.0==false') { 0.0 == false }
t('1G==true') { 1G == true }
t('same list <=>') { def l = [1]; l <=> l }
t('diff list <=>') { [1] <=> [1] }
t('plus1') { 1 + [2] }
t('times1') { 2 * [3] }
t('plus2') { 1 + [2, 3] }
t('equals bool') { true.equals(true) }
t('equals obj') { new Object().equals(null) }
t('iter truth') { [].iterator() ? 'T' : 'F' }
t('iter truth2') { [1].iterator() ? 'T' : 'F' }
t('inspect str') { 'it\'s\n'.inspect() }
t('inspect sb') { new StringBuilder('ab').inspect() }
t('class of class') { Integer.getClass().getName() }
t('null idx') { def n = null; n[0] }
t("null set") { null.foo = 1 }
t('strsum') { 'abc'.sum() }
t('str next empty') { ''.next().size() }
t('str+chars') { 'x' + 'abc'.toCharArray() }
t('matcher oob') { ('abc' =~ /l+/)[0] }
t('lines') { 'a\nb'.lines().toList() }
t('sum noStack') { [[1], [2]].sum(0) }
"####;
    let expected = r####"list[5..6] ! java.lang.IndexOutOfBoundsException: toIndex = 7
list[1..5] ! java.lang.IndexOutOfBoundsException: toIndex = 6
list[-5..-1] ! java.lang.IndexOutOfBoundsException: fromIndex = -2
list[2..0] = [3, 2, 1]
list[-1..-3] = [3, 2, 1]
str[1..5] ! java.lang.StringIndexOutOfBoundsException: Range [1, 6) out of bounds for length 3
str[-5..-1] ! java.lang.StringIndexOutOfBoundsException: Range [-3, 2) out of bounds for length 2
str[2..0] ! java.lang.StringIndexOutOfBoundsException: Range [0, 3) out of bounds for length 1
rrange[1..2] = [3, 2]
rrange[2..0] = [1, 2, 3]
crrange[1..2] = [c]
inject-empty ! java.util.NoSuchElementException: Cannot call inject() on an empty iterable without passing an initial value.
r.asList = IntRange
r.sort ! java.lang.UnsupportedOperationException: null
r.sort.empty = 1..<1
dec.isCase = true
step0 ! groovy.lang.GroovyRuntimeException: Infinite loop detected due to step size of 0
null<1 = true
1<null = false
list<list ! java.lang.IllegalArgumentException: Cannot compare java.util.ArrayList with value '[1, 2]' and java.util.ArrayList with value '[1, 3]'
bool<int ! java.lang.IllegalArgumentException: Cannot compare java.lang.Boolean with value 'true' and java.lang.Integer with value '1'
0.0==false = false
1G==true = false
same list <=> = 0
diff list <=> ! java.lang.IllegalArgumentException: Cannot compare java.util.ArrayList with value '[1]' and java.util.ArrayList with value '[1]'
plus1 = 3
times1 = 6
plus2 ! groovy.lang.MissingMethodException: No signature of method: plus for class: java.lang.Integer is applicable for argument types: (ArrayList) values: [[2, 3]]
equals bool = true
equals obj = false
iter truth = F
iter truth2 = T
inspect str = 'it\'s\n'
inspect sb = "ab"
class of class = java.lang.Class
null idx ! java.lang.NullPointerException: Cannot invoke method getAt() on null object
null set ! java.lang.NullPointerException: Cannot set property 'foo' on null object
strsum ! groovy.lang.MissingMethodException: No signature of method: sum for class: java.lang.String is applicable for argument types: () values: []
str next empty = 1
str+chars = xabc
matcher oob ! java.lang.IndexOutOfBoundsException: index is out of range 0..-1 (index = 0)
lines = [a, b]
sum noStack = 3
"####;
    check(src, expected);
}

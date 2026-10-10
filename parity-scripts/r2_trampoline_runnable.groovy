def total
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

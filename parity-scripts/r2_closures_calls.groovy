// bare call() recursion, owner/thisObject/delegate, direct-call arity
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

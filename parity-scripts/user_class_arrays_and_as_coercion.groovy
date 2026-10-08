class V { int n; V() {}; V(int n) { this.n = n }; String toString() { "V$n" } }
def xs = [new V(1), new V(2)] as V[]
println xs
println xs.getClass().getName()
println xs.getClass().getSimpleName()
println xs.length
println xs[1]
def ys = new V[2]
println ys
println ys.getClass().getName()
V[] zs = [new V(3)]
println zs.getClass().getSimpleName()
println(xs instanceof V[])
try { [1, 2] as V[] } catch (e) { println e.getClass().getName() + ": " + e.message }
try { println(1 as V) } catch (e) { println e.getClass().getName() + ": " + e.message }
class W { int n; String toString() { "W$n" } }
try { println([n: 2] as W) } catch (e) { println e.getClass().getName() + ": " + e.message }
try { println([4] as V) } catch (e) { println e.getClass().getName() + ": " + e.message }
try { println([4, 5] as V) } catch (e) { println e.getClass().getName() + ": " + e.message }
try { println("x" as V) } catch (e) { println e.getClass().getName() + ": " + e.message }
println(null as V)
def v = new V(7)
println((v as V).is(v))
println(xs instanceof Object[])
println((["a"] as String[]) instanceof Object[])
println(([1] as int[]) instanceof Object[])
println(([1] as Integer[]) instanceof Object[])
println(([1] as Object[]) instanceof String[])

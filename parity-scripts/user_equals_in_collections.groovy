class A { int x = 1; String toString() { "A$x" }; boolean equals(o) { o instanceof A && o.x == x }; int hashCode() { x } }
class C implements Comparable { int x; int compareTo(o) { x <=> o.x }; String toString() { "C$x" } }
class D { int x }
def l = [new A(x: 1), new A(x: 1), new A(x: 2)]
println l.unique(false).size()
println l.contains(new A(x: 2))
println l.indexOf(new A(x: 2))
println l.count(new A(x: 1))
println([new A(x: 1)] == [new A(x: 1)])
println(new A(x: 2) in l)
println l.minus([new A(x: 1)])
def cs = [new C(x: 1), new C(x: 1)]
println cs.unique(false).size()
println cs.contains(new C(x: 1))
println([new C(x: 1)] == [new C(x: 1)])
def ds = [new D(x: 1), new D(x: 1)]
println ds.unique(false).size()
println ds.contains(new D(x: 1))
println([new D(x: 1)] == [new D(x: 1)])
println l.toUnique { it.x }.size()
println l.intersect([new A(x: 2)])
println(([new A(x: 1), new A(x: 1)] as Set).size())
println(new HashSet([new A(x: 3), new A(x: 3), new A(x: 4)]).size())

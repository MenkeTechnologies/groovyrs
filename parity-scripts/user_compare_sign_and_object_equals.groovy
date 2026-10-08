class V implements Comparable { int n; String toString() { "V$n" }; int compareTo(o) { n - o.n } }
def a = new V(n: 1), b = new V(n: 5)
println a <=> b
println b <=> a
println a <=> new V(n: 1)
println a.compareTo(b)
println a.equals(a)
println a.equals(b)
println a.equals(null)
println a.equals("x")
class Plain {}
def p = new Plain()
println p.equals(p)
println p.equals(new Plain())
println([p].contains(p))
println([new Plain()].contains(p))
println "x".equals(p)

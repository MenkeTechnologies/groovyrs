// Operator overloading: a user-class instance operand dispatches its operator
// method (plus/minus/multiply/negative/remainder, compareTo for the relational
// and spaceship operators, and equals-or-compareTo for ==).
class Vec implements Comparable<Vec> {
  int x
  Vec(int v) { this.x = v }
  Vec plus(Vec o) { new Vec(x + o.x) }
  Vec minus(Vec o) { new Vec(x - o.x) }
  Vec multiply(int n) { new Vec(x * n) }
  Vec negative() { new Vec(-x) }
  Vec remainder(int n) { new Vec(x % n) }
  int compareTo(Vec o) { x <=> o.x }
  String toString() { "Vec(" + x + ")" }
}

def a = new Vec(10)
def b = new Vec(3)
println a + b
println a - b
println a * 2
println(-a)
println a % 4
println (a <=> b)
println a > b
println a <= b
println a == new Vec(10)
println a == b
println a == null

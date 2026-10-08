class Outer {
  static class Inner { int v = 3; String toString() { "Inner$v" } }
  static class Node { def val; Node next; Node(v) { val = v } }
  def make() { new Inner() }
  def chain() { def n = new Node(1); n.next = new Node(2); n }
  enum Kind { A, B }
  interface Shape { def area() }
  static class Sq implements Shape { def area() { 4 } }
}
println new Outer.Inner().v
println new Outer().make()
println new Outer().make().getClass().getName()
println new Outer().make().getClass().getSimpleName()
println new Outer().chain().next.val
println Outer.Kind.B.ordinal()
println new Outer.Sq().area()
println(new Outer.Sq() instanceof Outer.Shape)
def i = new Outer.Inner()
println(i instanceof Outer.Inner)
println Outer.Kind.values()
println Outer.Kind.A.getClass().getName()
class A {
  static class B {
    static class C { def hi() { "C" } }
    static K = 5
    static twice(x) { x * 2 }
    def mk() { new C() }
  }
  static build() { new B.C() }
}
println new A.B.C().hi()
println A.build().getClass().getName()
println new A.B().mk().getClass().getSimpleName()
println A.B.K
println A.B.twice(21)
println A.B.C.name
println A.B.C.simpleName
class Tree {
  static class Leaf { def v; Leaf(v) { this.v = v }; String toString() { "Leaf($v)" } }
  List<Leaf> leaves = []
  def add(x) { leaves << new Leaf(x); this }
}
println new Tree().add(1).add(2).leaves
def l = new Tree.Leaf(9)
println l
println l.class.name
println(l instanceof Tree.Leaf)
Tree.Leaf typed = new Tree.Leaf(3)
println typed.v

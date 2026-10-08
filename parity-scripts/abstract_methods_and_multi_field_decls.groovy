// An abstract method has no body; several fields can share one declaration.
abstract class Shape {
    abstract area()
    abstract String label();
    String describe() { "${label()}: ${area()}" }
}
class Sq extends Shape {
    def side = 2
    def area() { side * side }
    String label() { 'square' }
}
println new Sq().describe()
class P { def x, y; int a = 1, b = a + 1; static String s = 'S', t }
def p = new P(x: 1, y: 2)
println p.x + p.y
println "${p.a} ${p.b} ${P.s} ${P.t}"
class Q { private int m, n = 5
  def sum() { m + n } }
println new Q().sum()

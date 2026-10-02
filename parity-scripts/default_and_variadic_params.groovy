def g(a, b = 10) { a + b }
println g(1); println g(1, 2)
def h(a = 1, b) { "$a$b" }
println h(5); println h(6, 7)
def k(a, b = a * 2, c = b + 1) { [a, b, c] }
println k(1); println k(1, 5); println k(1, 5, 9)
class P {
  def x
  P(x = 3) { this.x = x }
  def m(a, b = 'd') { "$x$a$b" }
}
println new P().x; println new P(8).x; println new P().m('q'); println new P(1).m('q', 'r')
def v(... args) { args.size() }
println v(1, 2, 3); println v()
def w(String p, Object... rest) { p + rest.toList() + rest.getClass().simpleName }
println w('a', 1, 2); println w('b')
def z(String... s) { s.length }
println z('x', 'y')

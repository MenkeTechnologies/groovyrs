// A field holding a closure is callable by its name, after every method and
// before methodMissing; a field holding anything else is not.
class A { def d = { x -> x * 10 }; def f = 5; def g = null
  def methodMissing(String n, a) { "mm:$n" } }
def a = new A()
println a.d(4)
println a.f()
println a.g()
class B { def d = { -> 'bd' }; def d() { 'method' } }
println new B().d()
class C { def h = { -> 'ch' }; def add = { x, y -> x + y } }
println new C().h()
println new C().add(2, 3)
try { new C().f() } catch (e) { println e.class.name + ': ' + e.message.readLines()[0] }
class F { def h = 5 }
try { new F().h() } catch (e) { println e.class.name + ': ' + e.message.readLines()[0] }
class D { def h = { -> 'dh ' + this.getClass().name } }
println new D().h()

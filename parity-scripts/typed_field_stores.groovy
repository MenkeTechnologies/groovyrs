// A store to a typed field converts the value to the declared type, as a
// typed local's store does: initializer, property write, setter, map and
// tuple constructors, direct `@` access, and static fields alike.
class A { int i; double d; String s; Integer bi; boolean b }
def a = new A()
a.i = 2.7; println a.i
a.d = 3; println a.d
try { a.s = 5; println a.s.class.name } catch (e) { println e.class.name }
try { a.i = '7'; println a.i } catch (e) { println e.class.name }
try { a.i = 'abc'; println a.i } catch (e) { println e.class.name }
a.b = 'x'; println a.b
try { a.bi = 3L; println a.bi.class.name } catch (e) { println e.class.name }
try { a.i = null } catch (e) { println e.class.name }
a.setD(8); println a.d
def b = new A(i: 3.9, d: 1); println "${b.i} ${b.d}"
a.@d = 7; println a.d
class C { double v; def set(x) { v = x }; def setT(x) { this.v = x } }
def c = new C(); c.set(4); println c.v; c.setT(5); println c.v
class D { double v = 1; D() { v += 1 } }; println new D().v
class E { int n = 7.9; static double z = 2; }
println new E().n
println E.z
E.z = 5; println E.z
@groovy.transform.TupleConstructor class F { double x; int y }
def f = new F(1.5d, 2); println "${f.x} ${f.y}"
class Sq { double side = 2; double area() { side * side } }
println new Sq().area()

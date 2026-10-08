// A field initializer is class code: bare field and method names in it resolve
// on the instance being built, in declaration order, across the superclass chain.
class A {
    def a = 1
    def b = a + 1
    def c = twice(b)
    def d = { -> a * 10 }
    def e = this.a + 100
    def twice(x) { x * 2 }
}
def x = new A()
println "${x.a} ${x.b} ${x.c} ${x.d.call()} ${x.e}"
class B extends A { def f = a + c }
println new B().f
class Counter { int n = 3; List seen = (1..n).collect { it * n } }
println new Counter().seen

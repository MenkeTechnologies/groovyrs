def f() { def r = 5 }
println f()
def g() { int y = 3 }
println g()
def c = { def x = 7 }; println c()
def m(b) { if (b) { def q = 'y' } else { 'n' } }
println m(true); println m(false)
class K { def v() { def z = 9 } }
println new K().v()
def f(x) {
  def r = switch (x) {
    case 1:
      yield 'one'
  }
}
println f(1)
try { f(2) } catch (IllegalStateException e) { println e.message }

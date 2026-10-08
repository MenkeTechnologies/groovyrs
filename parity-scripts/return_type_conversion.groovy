// A value a function or method returns is converted to its declared return
// type; a `void` one answers null. Closures inside keep their own value.
void f1() { 5 }
println f1()
class A { void f() { 5 }; double g() { 3 }; static double h() { 4 }; int i(x) { if (x) return 1.9; 2.9 } }
println new A().f()
println new A().g()
println A.h()
println new A().i(true) + ' ' + new A().i(false)
int f2() { 2.9 }
println f2()
String f3() { 5 }
println f3().class.name
double f4(x) { if (x) return 1; 2 }
println f4(true)
println f4(false)
Integer f5() { null }
println f5()
def f6() { 2 }
println f6()
double f7() { try { 1 } finally { } }
println f7()
Object f8() { 3 }
println f8()
double f9() { def c = { return 5 }; c() }
println f9()
List f10() { [1] }
println f10()
double f11() { def r = 7 }
println f11()
boolean f12() { 'x' }
println f12()
def c = { -> 3 }
println c()

// Method pointers (`recv.&name`), calling a non-closure value (`x(args)` is
// `x.call(args)`), `this.f()` on the script, and `boolean isX()` getters.
def sq(y) { y * y }
def add(a, b) { a + b }
def f = this.&sq
println f(3)
println([1, 2, 3].collect(this.&sq))
println this.&add.curry(10)(5)
println this.sq(4)
println Math.&abs(-3)
def mx = Math.&max
println mx(3, 9)
def s = "hello"
def up = s.&toUpperCase
s = "bye"
println up()
class Scale { def n = 2; def times(x) { x * n } }
def t = new Scale().&times
println([1, 2].collect(t))
println f instanceof Closure
def lst = [3, 1, 2]
def adder = lst.&add
adder(9)
println lst

class Doubler { def call(x) { x * 2 }; def call(a, b) { a + b } }
def d = new Doubler()
println d(4)
println d(1, 2)
def n = 5
try { n(3) } catch (e) { println e.class.name + ": " + e.message.readLines()[0] }
def z = null
try { z(3) } catch (e) { println e.class.name + ": " + e.message.readLines()[0] }

class Flags { boolean isOk() { true }; def isPlain() { true }; Boolean isBoxed() { true } }
def fl = new Flags()
println fl.ok
try { println fl.plain } catch (e) { println e.class.name }
try { println fl.boxed } catch (e) { println e.class.name }
class Both { boolean isOk() { false }; boolean getOk() { true } }
println new Both().ok
println new Both().getOk()
class Sub extends Flags {}
println new Sub().ok

[1, 2, 3].eachPermutation { print it }
println()
[1, 1].eachPermutation { print it }
println()
def gen = ['a', 'b'].eachPermutation { a, b -> print b + a }
println()
println gen.getClass().name
println gen.hasNext()
try { [].eachPermutation { } } catch (e) { println e }

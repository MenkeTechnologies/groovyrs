def nul = null
println nul?[0]
println nul?.foo
def lst = [10, 20, 30]
println lst?[1]
println lst?[1, 2]
println lst?[0..1]
def mp = [a: 1]
println mp?['a']
println mp?.a
println 1 + [2]
println 2 * [3]
println 10 - [3]
println 1.5 + [2]
try { 1 + [1, 2] } catch (e) { println e.getClass().getSimpleName() }
println (+5)
println(+lst.size())
class Cls { static boolean isCase(Object o) { o == 'magic' } }
switch ('magic') { case Cls: println 'matched'; break; default: println 'no' }
switch ('other') { case Cls: println 'matched'; break; default: println 'no' }
trait Walker { String move() { 'walk' } }
trait Swimmer { String move() { 'swim' } }
class Duck implements Walker, Swimmer {
    String move() { Walker.super.move() + '+' + Swimmer.super.move() }
}
println new Duck().move()
class Animal { String sound() { 'generic' } }
class Dog extends Animal implements Walker { String sound() { super.sound() + '/bark/' + Walker.super.move() } }
println new Dog().sound()
println Dog.getSuperclass().getName()
println Dog.getSuperclass() == Animal
println Animal.getSuperclass().getName()
println Dog.isInstance(new Dog())
println Animal.isInstance(new Dog())
println Dog.isInstance(new Animal())
println Animal.isAssignableFrom(Dog)
println Dog.isAssignableFrom(Animal)
println Dog.getInterfaces()*.getName()
println 'abc'.getClass().getSuperclass().getName()
println Integer.getSuperclass().getName()
try { new ArrayList(-1) } catch (e) { println e.getClass().getSimpleName() + ': ' + e.message }
try { throw new IllegalStateException('x') } catch (e) { println e.stackTrace.length > 0 }
try { null.foo = 1 } catch (e) { println e.message }
try { def z = null; z[0] = 1 } catch (e) { println e.getClass().getSimpleName() + ': ' + e.message }
try { def z = null; z[0] } catch (e) { println e.getClass().getSimpleName() + ': ' + e.message }
println 'abc'.getAt(1)
println([3, 1, 2].asUnmodifiable().sort(false))
println 5.asType(int).getClass().getName()
println '7'.asType(Integer) + 1

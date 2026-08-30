// `trait`: an interface that carries implementations and state.
//
// A class can implement several, which is what separates a trait from a
// superclass — and when two of them define the same method, the LAST one named
// in the `implements` list wins.

trait Greets {
    String greeting() { "hello" }
    String greet(String who) { "${greeting()}, $who" }
}

class Plain implements Greets {}
println(new Plain().greet("world"))

// An implementor overrides a trait method by simply declaring it; the trait's
// other methods then call the override, because dispatch is on the instance.
class Loud implements Greets {
    String greeting() { "HELLO" }
}
println(new Loud().greet("world"))

// Traits carry state, and each instance gets its own copy.
trait Counter {
    int count = 0
    def bump() { count = count + 1; count }
}

class Clicker implements Counter {}
def c1 = new Clicker()
def c2 = new Clicker()
c1.bump()
c1.bump()
c2.bump()
println("${c1.count} ${c2.count}")

// Multiple traits: the last one in the list supplies a conflicting method.
trait Alpha { String who() { "Alpha" } }
trait Beta { String who() { "Beta" } }

class AB implements Alpha, Beta {}
class BA implements Beta, Alpha {}
println(new AB().who())
println(new BA().who())

// Non-conflicting methods from every trait are all present.
trait Walks { String move() { "walks" } }
trait Swims { String swim() { "swims" } }

class Duck implements Walks, Swims {
    String toString() { "Duck" }
}
def d = new Duck()
println("${d} ${d.move()} and ${d.swim()}")

// A trait can extend another; the implementor gets both levels.
trait Base { String base() { "base" } }
trait Derived extends Base { String derived() { "derived on ${base()}" } }

class Impl implements Derived {}
println(new Impl().derived())

// A trait satisfies `instanceof`, the way an interface does.
def duck = new Duck()
println(duck instanceof Walks)
println(duck instanceof Swims)
println(duck instanceof Greets)

// A class can mix a trait with a real superclass.
class Animal { String kind() { "animal" } }
class Dog extends Animal implements Walks {}
def dog = new Dog()
println("${dog.kind()} that ${dog.move()}")
println(dog instanceof Animal)
println(dog instanceof Walks)

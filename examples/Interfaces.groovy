// Interfaces: `interface` declarations, `implements`, abstract method
// declarations, Java 8 `default` methods, interface inheritance (`extends` with
// several parents), and the `instanceof` answers a type hierarchy gives.
interface Named {
  def name()
  // A `default` method has a body every implementor inherits, and it may call
  // the interface's own abstract methods.
  default def greet() { "hello " + name() }
}

interface Aged {
  def age()
}

// An interface may extend several others; an implementor satisfies them all.
interface Person extends Named, Aged {}

abstract class Being implements Named {
  def describe() { greet() + " (" + kind() + ")" }
  def kind() { "being" }
}

class Human extends Being implements Person {
  def n
  def a
  Human(n, a) { this.n = n; this.a = a }
  def name() { n }
  def age() { a }
  def kind() { "human" }
}

class Robot implements Named {
  def name() { "unit-7" }
  // An implementing class's own method wins over the interface default.
  def greet() { "BEEP " + name() }
}

def h = new Human("ada", 36)
println h.name()
println h.age()
println h.greet()      // inherited from the Named default
println h.describe()   // abstract-class method, virtual kind()

def r = new Robot()
println r.greet()      // the class override, not the default

println h instanceof Named
println h instanceof Aged
println h instanceof Person
println h instanceof Being
println r instanceof Named
println r instanceof Person

// Spread reaches an interface method across a heterogeneous list.
def all = [h, r]
println all*.name()
println all*.greet()

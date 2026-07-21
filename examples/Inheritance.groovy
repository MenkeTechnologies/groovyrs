// Single inheritance: extends, super(...) constructor chaining, method override
// with virtual dispatch, super.method(), inherited fields/methods, and
// instanceof.
class Animal {
  String name
  Animal(String n) { this.name = n }
  String speak() { "..." }
  String describe() { name + " says " + speak() }
}

class Dog extends Animal {
  Dog(String n) { super(n) }
  @Override
  String speak() { "Woof" }
  String fetch() { name + " fetches" }
}

class Puppy extends Dog {
  Puppy(String n) { super(n) }
  String speak() { "Yip (" + super.speak() + ")" }
}

def d = new Dog("Rex")
println d.speak()
println d.describe()   // base method dispatches virtually to Dog.speak
println d.fetch()      // inherited field `name`

def p = new Puppy("Bit")
println p.describe()   // three-level chain, super.speak() reaches Dog

println d instanceof Dog
println d instanceof Animal
println (d instanceof Puppy)
println (p instanceof Animal)

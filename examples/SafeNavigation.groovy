// The operators that make a null-heavy chain readable: `?.`, `?:`, `*.`, and
// `with`. Each has its own rule about what it does with null specifically.

class Address { String city }
class Person { String name; Address address }

def withAddr = new Person(name: "Ada", address: new Address(city: "London"))
def noAddr = new Person(name: "Bob")

// `?.` short-circuits the WHOLE chain to null on the first null receiver —
// it does not merely guard the one call it is written on.
println(withAddr?.address?.city)
println(noAddr?.address?.city)

// A plain `.` on the same null throws; that difference is the whole point.
try {
    println(noAddr.address.city)
} catch (NullPointerException e) {
    println("NPE without ?.")
}

// A null-typed variable stays null all the way down the chain.
Person absent = null
println(absent?.address?.city)

// `?:` (Elvis) falls back on FALSINESS, not only on null — so in Groovy an
// empty string, an empty collection and zero all take the right-hand side.
println(noAddr?.address?.city ?: "unknown")
println("" ?: "empty-is-falsy")
println(0 ?: "zero-is-falsy")
println([] ?: "empty-list-is-falsy")
println("set" ?: "not-used")
println(1 ?: "not-used")

// Elvis assignment keeps the left side when it is already truthy.
def a = null
a ?= "assigned"
def b = "kept"
b ?= "not-assigned"
println("$a $b")

// `*.` spreads a property or a call over a collection, and drops nothing:
// a null element contributes a null entry rather than disappearing.
def people = [withAddr, noAddr]
println(people*.name)
println(people*.address*.city)
println([[1, 2], [3, 4, 5]]*.size())
println([1, 2, 3]*.toString())

// The spread ARGUMENT operator unrolls a list into a call's parameters.
def add = { p, q -> p + q }
println(add(*[3, 4]))

// `with` returns the CLOSURE's value; `tap` returns the receiver, which is
// what makes tap chainable and with a way to compute something from one.
def built = new StringBuilder().with { sb -> sb.append("x"); sb.append("y"); sb.toString() }
println(built)
def tapped = new StringBuilder().tap { it.append("z") }
println(tapped.toString())

// `with` on a map reads the map's keys as if they were properties.
println([name: "Eve", age: 3].with { "$name is $age" })

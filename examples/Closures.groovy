// Nested closures capture the enclosing frame's locals as upvalues, so a
// curried closure and a chained call `f(a)(b)` both work.
def adder = { x -> { y -> x + y } }
def add5 = adder(5)
println "add5(10) = " + add5(10)
println "adder(3)(4) = " + adder(3)(4)

// A closure returned from a function captures that function's local.
def makeMultiplier(factor) { return { it * factor } }
def triple = makeMultiplier(3)
println "triple(7) = " + triple(7)

// Capture composes with the GDK collection methods.
def scale(factor, xs) { xs.collect { it * factor } }
println "scaled = " + scale(10, [1, 2, 3])

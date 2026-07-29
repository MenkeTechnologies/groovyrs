// Groovy truthiness: null and "empty or zero" are false, everything else true.
// A zero BigDecimal and an empty map are the cases a naive handle-is-true rule
// gets wrong; the string "0" is the case a shell-flavoured rule gets wrong.
def show(label, v) {
    if (v) println label + " -> true" else println label + " -> false"
}

show("0", 0)
show("1", 1)
show("0.0", 0.0)
show("0.00", 0.00)
show("1.50", 1.50)
show("0.0d", 0.0d)
show('""', "")
show('"0"', "0")
show('"x"', "x")
show("null", null)
show("[]", [])
show("[1, 2]", [1, 2])
show("[:]", [:])
show("[a:1]", [a: 1])

// The logical operators are boolean-VALUED in Groovy, unlike Elvis, which
// yields the deciding operand itself.
println 5 && 3
println 0 || 7
println 0.0 ?: "fallback"
println 1.50 ?: "fallback"
// Parenthesised: `println [:]` would read the `[` as a subscript on `println`.
println([:] ?: "fallback")
println([1] ?: "fallback")

// A class decides its own truth with asBoolean().
class Tank {
    def level
    Tank(n) { level = n }
    boolean asBoolean() { return level > 0 }
}
show("Tank(0)", new Tank(0))
show("Tank(3)", new Tank(3))

// A comparison-shaped guard is statically a Boolean, so it stays on the native
// op path — behaviour here must be identical to before truthiness was modeled.
def total = 0
for (i in 0..<5) {
    if (i % 2 == 0 && i < 4) total += i
}
println "total = " + total

// GString interpolation. A double-quoted string evaluates `$name`, a dotted
// property path `$a.b`, and a full `${ expression }`; a single-quoted string
// never interpolates, and `\$` is a literal dollar.
def name = "world"
def n = 7
def m = [count: 3, inner: [deep: 9]]
def xs = [1, 2, 3]

println "hello $name"
println "braced ${name}"
println "expr ${n * 6}"
println "path $m.count"
println "deep path $m.inner.deep"
println "adjacent $name$n"
println "escaped \$name"
println 'single $name'
println "nested quotes ${ n > 5 ? "big" : "small" }"
println "closure inside ${ xs.collect { it * 2 } }"
println "collection ${m}"
println "list $xs"

// A GString is a String: it has the usual methods and compares by value.
def greeting = "hi $name"
println greeting.length()
println greeting.toUpperCase()
println("$name" == "world")

// An embedded object renders through its toString(), which plain handle
// formatting would not do.
class Point {
    def x
    def y
    Point(a, b) { x = a; y = b }
    String toString() { return "(" + x + ", " + y + ")" }
}
def p = new Point(2, 5)
println "point $p and ${p}"
println "concat " + p

// Decimals keep their BigDecimal rendering inside a GString.
def price = 19.90
println "price $price total ${price * 3}"

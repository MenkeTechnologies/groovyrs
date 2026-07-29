// `getClass()` / the `.class` property, and `String.toBigDecimal()` with
// BigDecimal's own parse diagnostics.
println 1.getClass()
println "s".getClass()
println 1.5.getClass()
println 1.5d.getClass()
println true.getClass()
println([1, 2].getClass())
println([a: 1].getClass())
println null.getClass()

println 1.class.getName()
println 1.class.simpleName
println "s".class.name

class Widget {}
def w = new Widget()
println w.getClass()
println w.class.name
println w.class.simpleName

try {
  throw new IllegalStateException("bad")
} catch (Exception e) {
  println e.getClass().getName()
  println e.class.simpleName
}

// Valid decimals keep their exact scale and BigDecimal's own rendering.
println "1.5".toBigDecimal()
println "100.00".toBigDecimal()
println " 7 ".toBigDecimal()
println ".5".toBigDecimal()
println "2.5e7".toBigDecimal()
println "1e-7".toBigDecimal()

// Each failure carries BigDecimal's character-level message — and two of them
// carry no message at all, which Groovy reports as `null`.
def bad = ["", "x", "1.2.3", "1e", "+", "1e999999999999", "1ex"]
for (s in bad) {
  try {
    println "[" + s + "] -> " + s.toBigDecimal()
  } catch (NumberFormatException e) {
    println "[" + s + "] !! " + e.message
  }
}

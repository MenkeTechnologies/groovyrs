// The GDK over lists and maps, the spread operator, and `for (x in …)`.
def xs = [3, 1, 2, 3, 1]

// `sort()` and `unique()` mutate the receiver and return it.
println xs.sort()
println xs
println xs.unique()
println xs

println xs.reverse()   // a new list; the receiver is untouched
println xs
println xs.max()
println xs.min()
println xs.sum()
println xs.sum(100)
println xs.join("-")
println xs.collect { it * 2 }
println xs.findAll { it > 1 }
println xs.inject(0) { acc, v -> acc + v }
println xs.groupBy { it % 2 }

// A closure with one parameter is a key extractor; with two, a comparator.
println(["bbb", "a", "cc"].sort { it.size() })
println([3, 1, 2].sort { a, b -> b <=> a })
println(["bbb", "a", "cc"].max { it.size() })

def m = [b: 2, a: 1, c: 3]
m.each { k, v -> println k + "=" + v }
m.each { e -> println e }              // one parameter: a Map.Entry
println m.collect { k, v -> k + v }
println m.findAll { k, v -> v > 1 }
println m.find { k, v -> v == 1 }
println m.groupBy { k, v -> v > 1 }
println m.inject(0) { acc, e -> acc + e.value }
println m.sort()
println m.max { it.value }
println m.every { k, v -> v > 0 }
println m.any { k, v -> v > 2 }

// The spread operator applies a member to every element (null-safe).
class Point {
  def x
  Point(x) { this.x = x }
  def twice() { x * 2 }
  String toString() { "P($x)" }
}
def ps = [new Point(1), new Point(4)]
println ps*.x
println ps*.twice()
println([1, null, 3]*.toString())
println([[1, 2], [3]]*.size())

// `for (x in …)` walks a list's elements, a map's entries, and a String's
// characters; `null` iterates zero times.
for (x in [10, 20]) { println x }
for (e in [k: 1]) { println e }
for (c in "hi") { println c }
for (x in null) { println "never" }

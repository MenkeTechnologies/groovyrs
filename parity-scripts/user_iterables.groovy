// User classes iterated through their own iterator(): for-in, an iterator
// handle in for-in, an `implements Iterator` class, and the GDK on them.
class It { def iterator() { [7, 8].iterator() } }
for (v in new It()) println v
def it2 = [1, 2, 3].iterator()
it2.next()
for (v in it2) println v
println it2.hasNext()
class Count implements Iterator { int i = 0; boolean hasNext() { i < 3 }; def next() { i++ } }
for (v in new Count()) print v
println()
class Bag implements Iterable { Iterator iterator() { new Count() } }
for (v in new Bag()) print v
println()
new Bag().each { print it }
println()
println new Bag().collect { it * 2 }
class NoIt { def x = 1 }
for (v in new NoIt()) println v.getClass().name
def b = new Bag()
println([b.sum(), b.max(), b.join('-'), b.toList(), b.find { it > 1 }, b.inject(0) { a, x -> a + x }, b.any { it > 2 }, b.every { it < 9 }, b.findAll { it % 2 }, b.first(), b.last(), b.size(), b.toSet(), b.count(1)])
println(b.each { } instanceof Bag)
def c = new Count(); println c.collect { it }; println c.hasNext()
class OnlyIt { def iterator() { [5, 6].iterator() } }
println new OnlyIt().collect { it + 1 }
println b.groupBy { it % 2 }
println b.withIndex()

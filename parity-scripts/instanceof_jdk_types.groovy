// `instanceof` against the JDK collection, regex, closure and marker
// interfaces, per value shape. Each row is one value's answers, in `types`
// order.
def show = { String label, v, List bits -> println label.padRight(12) + bits.collect { it ? 1 : 0 }.join('') }
def row = { v -> [
    v instanceof Closure, v instanceof Set, v instanceof HashSet, v instanceof LinkedHashSet,
    v instanceof TreeSet, v instanceof SortedSet, v instanceof NavigableSet, v instanceof Collection,
    v instanceof Iterable, v instanceof List, v instanceof ArrayList, v instanceof AbstractList,
    v instanceof RandomAccess, v instanceof java.util.regex.Pattern, v instanceof java.util.regex.Matcher,
    v instanceof Map.Entry, v instanceof Comparable, v instanceof CharSequence, v instanceof Number,
    v instanceof Serializable, v instanceof Cloneable, v instanceof Map, v instanceof Range] }
show('closure', null, row({ 1 }))
show('curried', null, row({ a, b -> a }.curry(1)))
show('toSet', null, row([1].toSet()))
show('asSet', null, row([1] as Set))
show('TreeSet', null, row(new TreeSet([1])))
show('Pattern', null, row(~/a/))
show('Matcher', null, row("abc" =~ /b/))
show('entry', null, row([a: 1].entrySet().first()))
show('SB', null, row(new StringBuilder()))
show('SW', null, row(new StringWriter()))
show('IntRange', null, row(1..2))
show('ObjRange', null, row('a'..'c'))
show('LongRange', null, row(1L..3L))
show('EmptyRange', null, row(1..<1))
show('int[]', null, row([1] as int[]))
show('Object', null, row(new Object()))
show('Exception', null, row(new Exception('x')))
show('String', null, row('x'))
show('Integer', null, row(1))
show('BigInteger', null, row(1G))
show('BigDecimal', null, row(1.5))
show('Double', null, row(1.5d))
show('Boolean', null, row(true))
show('ArrayList', null, row([1]))
show('SubList', null, row([1, 2, 3].subList(0, 2)))
show('map', null, row([a: 1]))
show('TreeMap', null, row(new TreeMap()))
show('HashMap', null, row(new HashMap()))
show('withDefault', null, row([:].withDefault { 0 }))
class Money implements Comparable { int v; int compareTo(o) { v <=> o.v } }
class Plain {}
show('Comparable', null, row(new Money()))
show('Plain', null, row(new Plain()))

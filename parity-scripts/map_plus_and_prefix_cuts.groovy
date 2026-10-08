// Map.plus as a method, and the closure-driven prefix cuts on a map.
def m = [a:1, b:2, c:1]
println(m + [d:4])
println m.plus([d:4])
println m.plus([b:9])
println m.takeWhile { k, v -> v < 2 }
println m.dropWhile { k, v -> v < 2 }
println m.takeWhile { e -> e.value < 2 }
println m.dropWhile { e -> e.key != 'c' }
println m.takeWhile { k, v -> true }
println m.dropWhile { k, v -> true }
println m.takeWhile { k, v -> true }.getClass().name
def t = new TreeMap([b:2, a:1, c:3])
println t.takeWhile { k, v -> k < 'c' }.getClass().name
println([:].takeWhile { k, v -> true })

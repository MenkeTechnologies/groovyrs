// Arrays.toString renders each element through String.valueOf; the System
// clocks answer positive longs.
println Arrays.toString([1, 2] as int[])
println Arrays.toString([1.5, 2] as double[])
println Arrays.toString(['a', null] as String[])
println Arrays.toString((int[]) null)
println Arrays.toString([1, 2])
println Arrays.toString(new int[0])
println Arrays.toString([[1], [a: 2]] as Object[])
println(System.currentTimeMillis() > 1700000000000)
println System.currentTimeMillis().class.name
def t0 = System.nanoTime()
println(System.nanoTime() >= t0)
println t0.class.name

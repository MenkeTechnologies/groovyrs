// `as T[]` converts each element with `as` to the component type.
println(([2.7, 3] as int[]))
println(([1, 2.5] as double[]))
try { println((['7', 8] as int[])) } catch (e) { println e.class.name }
try { println((['ab'] as int[])) } catch (e) { println e.class.name }
println(([1, 'x'] as String[]))
println(([1, 0] as boolean[]))
println(([1, 2] as Double[]))
println(([1, null] as Integer[]))
try { println(([1, null] as int[])) } catch (e) { println e.class.name }
println(([1, 2] as Object[])[0].class.name)
int[] a = [3.9, 4]; println a
double[] d = [1, 2]; println d
println(([65, 66] as char[]))
println(([1, 2] as float[]))
println(([[1, 2.5], [3]] as int[][]))

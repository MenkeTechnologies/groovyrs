println({ a, b -> a }.parameterTypes)
println({ it }.parameterTypes)
println({ -> 1 }.parameterTypes)
println({ int a, String b -> a }.parameterTypes)
println({ String... xs -> xs }.parameterTypes)
println({ a, b = 1 -> a }.parameterTypes)
println({ Integer x, def y, Object z -> x }.parameterTypes)
println({ a, b -> a }.parameterTypes.length)
println({ a, b -> a }.parameterTypes.getClass().getName())
println({ a, b -> a }.getParameterTypes().size())
println({ a, b -> a }.curry(1).parameterTypes)
println({ double d, boolean f, List l, Map m -> 1 }.parameterTypes)
println({ int[] xs -> xs }.parameterTypes)
println({ a -> a }.getParameterTypes())
println({ a, b -> a }.getMaximumNumberOfParameters())
def m = { def k, v -> k }
println m.parameterTypes
println m(1, 2)
interface Shape { def area() }
println Shape
println Shape.isInterface()
println String.isInterface()
class K {}
println K
println K.isInterface()
println List
println Map
println List.isInterface()
interface I {}
trait T {}
class C implements I {}
println new C().getClass()
println I
println T
println List
println(I.isInterface())
def c = { a, b -> a }
println c.getMaximumNumberOfParameters()

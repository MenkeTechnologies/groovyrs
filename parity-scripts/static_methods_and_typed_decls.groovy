class C {
  static twice(a) { a * 2 }
  static plusOne(a) { twice(a) + 1 }
  static int sq(int x) { x * x }
}
println C.twice(3); println C.plusOne(3); println C.sq(5); println C; println C.simpleName
final x = 1
final int y = 2
static int cube(int n) { n * n * n }
println x + y + cube(2)
Map<String, Integer> m = [a: 1]; List<String> l = ['x']; println m; println l
List<List<Integer>> n = [[1]]; println n
def f(List<String> xs, Map<String, List<Integer>> m2) { xs.size() + m2.size() }
println f(['a'], [:])
for (Map.Entry<String, Integer> e in [a: 1]) println e.key

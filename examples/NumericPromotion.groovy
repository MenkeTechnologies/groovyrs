// Binary numeric promotion: an operator with a `double` on either side runs on
// IEEE doubles, so the other operand is widened first.
//
// The interesting case is a `long` past 2^53, where widening lands on a
// NEIGHBOURING value: 3**34 is 16677181699666569, but the nearest double is
// ...568. Groovy answers from the widened value, so the long compares EQUAL to
// a double that is not its own magnitude.

long L = 16677181699666569L
double y = 2.0d
double n = 1.6677181699666568E16d

println(L)
println(L as double)

println(L + y)
println(y + L)
println(L - y)
println(y - L)
println(L * y)
println(y * L)
println(L / y)
println(y / L)
println(L % y)
println(y % L)
println(L ** y)
println(y ** L)

println(L == y)
println(y == L)
println(L != y)
println(y != L)
println(L < y)
println(y < L)
println(L > y)
println(y > L)
println(L <= y)
println(y <= L)
println(L >= y)
println(y >= L)
println(L <=> y)
println(y <=> L)

// The rounding case: `n` is `L`'s double image, one unit away.
println(L == n)
println(n == L)
println(L < n)
println(L > n)
println(L <=> n)
println(L + n)

// Every promoted result is a Double, including `/` and `**`.
println((L + y).class.name)
println((L / y).class.name)
println((L ** y).class.name)

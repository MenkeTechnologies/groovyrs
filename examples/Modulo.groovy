// `%` by zero. Java splits three ways on the operand types, and Groovy keeps
// each: two Integers raise `/ by zero`, a BigDecimal operand raises
// `Division by zero` (or `Division undefined` when the dividend is zero too),
// and a double operand answers NaN without raising at all.
def z = 0
def dz = 0.0
def ddz = 0.0d

println 17 % 5
println 17.5 % 5
println(-17 % 5)

try { println 7 % z } catch (ArithmeticException e) { println "int:      " + e.message }
try { println 7.5 % z } catch (ArithmeticException e) { println "decimal:  " + e.message }
try { println 0.0 % dz } catch (ArithmeticException e) { println "zero/0:   " + e.message }
try { println 7 % dz } catch (ArithmeticException e) { println "int/dec:  " + e.message }
try { println 7.5 % ddz } catch (ArithmeticException e) { println "dec/dbl:  " + e.message }

// A double operand never raises.
println 7.0d % ddz
println 7 % ddz
println 7.0d % z

// The compound form shares the same check.
def x = 17
x %= 5
println x
try { x %= z } catch (ArithmeticException e) { println "compound: " + e.message }

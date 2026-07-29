// Groovy's unsuffixed decimal literals are java.math.BigDecimal, not doubles:
// the literal's scale is part of the value, and it survives arithmetic.
println 2.5e7                  // => 2.5E+7   (an exponent literal has a negative scale)
println 100.00                 // => 100.00   (trailing zeros are the value, not noise)
println 1e-7                   // => 1E-7     (outside the plain-notation window)

// Scale propagates: + and - take the larger scale, * sums them.
println 2.5e7 + 1              // => 25000001 (scale 0 — no trailing .0 at all)
println 1.10 + 2.20            // => 3.30
println 1.25 * 0               // => 0.00
println 0.1 + 0.2              // => 0.3      (exact; a double gives 0.30000000000000004)

// Division promotes like Groovy's BigDecimalMath: exact when the quotient
// terminates, otherwise cut to ten fraction digits.
println 7 / 2                  // => 3.5
println 1.000 / 4              // => 0.250    (padded to Java's preferred scale)
println 1 / 3                  // => 0.3333333333

// A d-suffixed literal is an IEEE double and keeps double rules.
println 0.1d + 0.2d            // => 0.30000000000000004
println 5.0d / 0.0d            // => Infinity

// Decimals are unbounded, so this overflows no exponent range.
println 1.5e300 * 1.5e300      // => 2.25E+600

// Scale survives printing from a collection and string concatenation.
println([1.50, 2.0])
println("total: " + (0.1 + 0.2))

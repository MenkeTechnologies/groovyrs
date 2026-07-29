// try / catch / finally / throw over the built-in throwable hierarchy.
def classify(n) {
    try {
        if (n < 0) throw new IllegalArgumentException("negative " + n)
        if (n == 0) throw new ArithmeticException("zero")
        return "ok " + (100 / n)
    } catch (IllegalArgumentException e) {
        return "bad-arg: " + e.message
    } catch (Exception e) {
        return "other: " + e.getMessage()
    } finally {
        println "  checked " + n
    }
}
println classify(4)
println classify(0)
println classify(-2)

// A throwable prints as Throwable.toString(): the qualified class name plus the
// detail message.
println new Exception("boom")
println new IOException("disk")
println new RuntimeException("rt").getMessage()
println new Exception().getMessage()

// catch matches on the whole supertype chain.
println new NumberFormatException("n") instanceof IllegalArgumentException
println new IllegalStateException("s") instanceof RuntimeException
println new IOException("i") instanceof RuntimeException

// A script class can extend a built-in throwable and chain to its constructor.
class ParseFailed extends Exception {
    ParseFailed(String m) { super(m) }
}
try {
    throw new ParseFailed("line 7")
} catch (Exception e) {
    println e
    println "caught? " + (e instanceof ParseFailed)
}

// A multi-catch arm, and an untyped `catch (e)`.
try { throw new NumberFormatException("nf") }
catch (IllegalStateException | IllegalArgumentException e) { println "multi: " + e.message }
try { throw new Exception("bare") } catch (e) { println "untyped: " + e.message }

// `finally` runs on every exit path, including an early return out of a loop
// and a rethrow from a handler.
def firstEven(xs) {
    for (i in 0..<xs.size()) {
        try {
            if (xs[i] % 2 == 0) return "found " + xs[i]
        } finally {
            println "  visited " + xs[i]
        }
    }
    return "none"
}
println firstEven([1, 3, 6, 8])

try {
    try { throw new Exception("inner") }
    catch (Exception e) { throw new IllegalStateException("wrapped: " + e.message) }
    finally { println "  cleanup" }
} catch (Exception e) {
    println e.message
}

// Groovy raises a zero divisor as a catchable ArithmeticException.
try { println 1 / 0 } catch (ArithmeticException e) { println "div: " + e.message }

// A throw out of a closure unwinds to the caller's handler.
try {
    [1, 2, 3].each { if (it == 2) throw new IllegalStateException("stop at " + it); println "  saw " + it }
} catch (Exception e) {
    println "escaped: " + e.message
}

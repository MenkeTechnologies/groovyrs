// Runtime faults are catchable Groovy throwables, not aborts: an unknown
// method or property, a call on null, an out-of-range index, and an unparsable
// number all raise the type Groovy raises and unwind to the handler.
nil = null

def caught(label, c) {
    try {
        println(label + " => " + c())
    } catch (NullPointerException e) {
        println(label + " NPE " + e.message)
    } catch (StringIndexOutOfBoundsException e) {
        println(label + " SIOOBE " + e.message)
    } catch (ArrayIndexOutOfBoundsException e) {
        println(label + " AIOOBE " + e.message)
    } catch (IndexOutOfBoundsException e) {
        println(label + " IOOBE " + e.message)
    } catch (NumberFormatException e) {
        println(label + " NFE " + e.message)
    } catch (MissingPropertyException e) {
        println(label + " MPE " + e.message)
    } catch (MissingMethodException e) {
        println(label + " MME")
    }
}

caught("nomethod", { "hi".nope() })
caught("noprop", { "hi".zork })
caught("nullcall", { nil.length() })
caught("nullprop", { nil.zork })
caught("listget", { [1, 2, 3].get(9) })
caught("strsub", { "abc"[9] })
caught("negsub", { [1, 2, 3][-9] })
caught("toint", { "abc".toInteger() })
caught("subscript", { 5[0] })

// The throwable is an ordinary member of the built-in hierarchy.
try {
    "hi".nope()
} catch (Exception e) {
    println(e instanceof MissingMethodException)
    println(e instanceof GroovyRuntimeException)
    println(e instanceof RuntimeException)
    println(e instanceof NullPointerException)
}

// A fault unwinds across a frame boundary and still runs `finally`.
def deep(x) {
    try {
        return x.length()
    } finally {
        println("fin")
    }
}
try {
    println(deep(nil))
} catch (NullPointerException e) {
    println("outer " + e.message)
}

// The reads Groovy does *not* fault on stay non-faulting.
println([1, 2, 3][5])
println([a: 1].missing)
println(nil.toString())
println("42".toInteger() + 1)
println("3.5".toDouble())

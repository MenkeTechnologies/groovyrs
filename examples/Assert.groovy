// A failing `assert` raises a PowerAssertionError whose message is Groovy's
// power-assert layout: the statement's own source, then every sub-expression's
// value placed under the column it came from.
x = 3
s = "hi"
l = [1, 2]
m = [a: 1]

def show(c) {
    try {
        c()
        println("passed")
    } catch (AssertionError e) {
        println(e.getMessage())
        println("--")
    }
}

show({ assert x == 5 })
show({ assert x + 1 == 5 })
show({ assert s.length() == 3 })
show({ assert l[0] == 9 })
show({ assert !x })
show({ assert -x == 3 })
show({ assert x > 1 && x > 9 })
show({ assert l.isEmpty() })
show({ assert m.a == 2 })
show({ assert s.toUpperCase().length() == 9 })
show({ assert 1 == 2 })

// A passing assert is silent.
show({ assert x == 3 })

// The `: message` form raises a plain AssertionError instead, quoting the
// condition's canonical AST text and the variables it names.
show({ assert x == 5 : "custom" })
show({ assert s.length() == 9 : "len bad" })
show({ assert x instanceof String : "type" })

// The throwable is an ordinary member of the hierarchy.
try {
    assert x == 5
} catch (Throwable t) {
    println(t instanceof AssertionError)
    println(t instanceof Error)
    println(t instanceof Exception)
}

// It unwinds like any other throwable: across a frame, through `finally`.
def check(v) {
    try {
        assert v < 2 : "too big"
        return "ok " + v
    } finally {
        println("fin " + v)
    }
}
for (i in 0..2) {
    try {
        println(check(i))
    } catch (AssertionError e) {
        println("caught " + e.getMessage())
    }
}

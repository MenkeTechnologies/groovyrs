// Groovy's `switch` matches with `isCase`, not `==`: a range or list contains,
// a type is an `instanceof`, a `~/…/` pattern matches the whole subject, and a
// closure is called with it. Sections fall through until a `break`.
class Animal {
    def name = "rex"
}

def classify(x) {
    switch (x) {
        case 1: return "one"
        case 2:
        case 3: return "two-or-three"
        case 4..6: return "range"
        case [7, 8]: return "list"
        case String: return "string"
        case ~/a+b/: return "regex"
        case Animal: return "animal"
        case { it instanceof Integer && it > 100 }: return "big"
        case null: return "null"
        default: return "other"
    }
}

def probes = [1, 2, 3, 5, 7, "zz", 101, null, 9.5]
for (i in 0..<probes.size()) {
    println("" + probes[i] + " -> " + classify(probes[i]))
}
// `case String` wins over `case ~/a+b/` because it is written first.
println(classify("aab"))
println(classify(new Animal()))

// Fall-through: a section without `break` runs the next section's body too.
def fall(x) {
    def r = ""
    switch (x) {
        case 1: r = r + "a"
        case 2: r = r + "b"; break
        case 3: r = r + "c"
        default: r = r + "d"
    }
    return r
}
for (i in 1..4) println("fall " + i + " -> " + fall(i))

// The subject is evaluated once, and labels only until one matches.
n = 0
def bump() { n = n + 1; return 2 }
hits = ""
def probe(v) { hits = hits + v; return v }
switch (bump()) {
    case probe(1): println("no"); break
    case probe(2): println("yes"); break
    case probe(3): println("no"); break
}
println("subject evals=" + n + " labels=" + hits)

// A switch with no default and no match runs nothing.
switch (99) { case 1: println("unreachable") }

// `do`/`while` runs its body before the first test.
def i = 0
do {
    println("do " + i)
    i++
} while (i < 3)
do { println("always once") } while (false)

// Labeled `break`/`continue` name the frame they leave.
outer:
for (a in 0..2) {
    for (b in 0..2) {
        if (b == 1) continue outer
        if (a == 2) break outer
        println(a + "-" + b)
    }
}

// A labeled `break` leaves the loop from inside a `switch`; an unlabeled
// `continue` inside one continues the loop around it.
scan:
for (a in 0..3) {
    switch (a) {
        case 1: continue
        case 3: break scan
        default: break
    }
    println("scan " + a)
}
println("done")

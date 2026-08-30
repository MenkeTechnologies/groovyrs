// Groovy's two regex operators and the Matcher they produce.
//
// `==~` is a whole-string match returning a boolean; `=~` builds a Matcher,
// which is truthy when it finds anything and iterable over its matches.

println("hello" ==~ /h.*o/)
println("hello" ==~ /ell/)
println("hello" ==~ /.*ell.*/)

def m = ("a1b22c333" =~ /\d+/)
println(m.find())
println(("a1b22c333" =~ /\d+/).collect { it })
println(("a1b22c333" =~ /\d+/).count)

// A Matcher is truthy exactly when it matches something.
println(("abc" =~ /b/) ? "found" : "missing")
println(("abc" =~ /z/) ? "found" : "missing")

// Groups: [0] is the whole match, [n] the nth group.
def g = ("2026-08-29" =~ /(\d{4})-(\d{2})-(\d{2})/)
g.find()
println(g[0])
println(g.group(1) + "/" + g.group(2) + "/" + g.group(3))
println(g.groupCount())

// Every match, with its groups, via the list form.
def all = ("a=1,b=22" =~ /(\w)=(\d+)/).collect { it }
println(all)

// replaceAll takes a literal or a closure; the closure receives the match.
println("a1b22".replaceAll(/\d+/, "#"))
println("a1b22".replaceAll(/\d+/) { "<${it}>" })
println("a1b22".replaceFirst(/\d+/, "#"))

// A group reference in the replacement string.
println("john smith".replaceAll(/(\w+) (\w+)/, '$2, $1'))

// Pattern-ish helpers on String.
println("a,b,,c".split(/,/) as List)
println("a1b2".findAll(/\d/))
println("hello world".find(/o\s*w/))
println("abc".matches(/[a-c]+/))

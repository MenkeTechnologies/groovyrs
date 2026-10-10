def x = 5
def eager = "v=${x}"
def lazy = "v=${-> x}"
x = 9
println eager
println lazy
println lazy.toString()
println lazy.length()
println lazy == 'v=9'
println 'v=9' == lazy
println lazy.toUpperCase()
println lazy + '!'
println lazy.getClass().getName()
println lazy instanceof GString
println lazy instanceof String
println lazy instanceof CharSequence
def n = 1
def multi = "a${-> n}b${-> n * 2}c"
n = 4
println multi
n = 5
println multi
def calls = 0
def counted = "c=${-> ++calls}"
println counted
println counted
println calls
def f = { -> 'from closure' }
println "${f}"
println "pre ${f} post"
def w = "x${ { out -> out << 'written' } }y"
println w
def list = ["a${-> 1}", 'b']
println list
println list*.size()
println list.join(',')
def map = [k: "v${-> 2}"]
println map
println map.k.size()
switch ("q${-> 1}") { case 'q1': println 'matched'; break; default: println 'no' }
println "${-> [1, 2].collect { it * 2 }}"
println "${-> null}"
println "nested ${-> "inner ${-> 'deep'}"}"
def sb = new StringBuilder()
sb << "s${-> 3}"
println sb
println "${-> 1}${-> 2}".length()
println ("a${-> 'b'}" as String).getClass().getName()
println "t${-> 7}".trim()
println "t${-> 7}".split('t').toList()
println "t${-> 7}".bytes.length

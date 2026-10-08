// `println ++i` parses as `(println++)(i)`: the property read of `println`
// raises before `i` is incremented.
def i = 5
try { println ++i } catch (e) { println e.class.name + ' ' + e.message }
println i
try { print --i } catch (e) { println e.class.name + ' ' + e.message }
println i
println(++i)

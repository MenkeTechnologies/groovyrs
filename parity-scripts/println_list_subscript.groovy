// A paren-less `println [..]` / `print [..]` is the subscript `println[..]`
// of the property `println`, not a call with a list argument.
try { println [1, 2, 3] } catch (e) { println e.class.name + ' ' + e.message }
try { print [1].size() } catch (e) { println e.class.name + ' ' + e.message }
try { println [1, 2, 3].toListString() } catch (e) { println e.class.name }
println([1, 2, 3])
println "${[1, 2]}"

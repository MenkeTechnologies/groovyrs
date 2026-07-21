// Groovy maps preserve insertion order (a LinkedHashMap). A multi-entry literal
// prints in the order written, a new key appends, and subscript / property
// access read entries.
def m = [banana: 3, apple: 5, cherry: 2]
println m
m.date = 7
println m
println "apple = " + m.apple
println "cherry = " + m["cherry"]
println "size = " + m.size()
println "has apple: " + m.containsKey("apple")

// Iterative Fibonacci — compiled to fusevm bytecode, hot loop trace-JITed.
def a = 0
def b = 1
for (i in 0..<10) {
    println a
    def next = a + b
    a = b
    b = next
}

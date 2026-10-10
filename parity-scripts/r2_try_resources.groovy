class Res implements AutoCloseable {
    String name
    boolean failClose
    Res(String name, boolean failClose = false) { this.name = name; this.failClose = failClose; println "open $name" }
    void close() { println "close $name"; if (failClose) throw new IllegalStateException("close $name") }
}
try (def a = new Res('a'); def b = new Res('b')) {
    println 'body'
}
println '--'
try (Res a = new Res('a'); Res b = new Res('b')) {
    println 'throwing'
    throw new RuntimeException('boom')
} catch (RuntimeException e) {
    println 'caught ' + e.message
} finally {
    println 'finally'
}
println '--'
try (Res a = new Res('x')) {
    println 'in'
} finally {
    println 'fin only'
}
println '--'
def f() {
    try (Res a = new Res('r')) {
        return 'returned'
    } finally {
        println 'f-finally'
    }
}
println f()
println '--'
try (Res a = new Res('c', true)) {
    println 'with failing close'
} catch (IllegalStateException e) {
    println 'caught ' + e.message
}
println '--'
def existing = new Res('ex')
try (existing) {
    println 'using existing'
}
println '--'
try (Res n = null) {
    println 'null resource ok'
}
for (i in 1..2) {
    try (Res l = new Res("loop$i")) {
        if (i == 1) continue
        println 'second'
    }
}

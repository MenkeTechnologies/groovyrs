// while with break/continue, and a compound counter.
def i = 0
def evens = 0
while (i < 20) {
    i++
    if (i % 2 != 0) continue
    if (i > 10) break
    evens += i
}
println "even sum up to 10 = " + evens

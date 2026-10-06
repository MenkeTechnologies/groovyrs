// remove((Object) x) selects Collection.remove(Object); findResults,
// collectEntries() with no closure, asList on a list.
def l = [1, 2, 3]; println l.remove((Object) 3); println l
def m = [5, 6, 7]; m.remove(7 as Object); println m
def n = [5, 6, 7]; Object o = 2; println n.remove(o); println n
def p = [1, 2, 3]; println p.remove((Integer) 1); println p
def mp = [a: 1, b: 2]; println mp.remove((Object) 'a'); println mp
def st = [1, 2, 3] as Set; println st.remove((Object) 2); println st
def h = [l: [1, 2, 9]]; h.l.remove((Object) 9); println h
println([1, null, 2].findResults()); println([[a: 1], [b: 2]].collectEntries())
println([1, 2].findResults { it > 1 ? it : null }); println([a: 1, b: 2].findResults { k, v -> v > 1 ? k : null })
println((1..4).findResults { it % 2 ? it : null })
def q = [1]; println q.asList().is(q); println([5, 6].asList())
println(['a', 'b'].withIndex().collectEntries())

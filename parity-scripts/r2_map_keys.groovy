def m = [1: 'a', 2: 'b', 10: 'c']
m.each { k, v -> println k + 1 }
println m.keySet().collect { it * 2 }
println m.keySet()*.getClass()*.getSimpleName()
println m[1]
println m['1']
println m.containsKey(1)
println m.containsKey('1')
def mixed = [(1): 'x', ('1'): 'y']
println mixed
println mixed.size()
def d = [1.5: 'a', 2.5: 'b']
println d.keySet()*.getClass()*.getSimpleName()
println d[1.5]
println m.sort()
println m.keySet().sort()
println new TreeMap(m)
println new TreeMap(m).firstKey()
println new TreeMap(m).headMap(10)
println new HashMap([10: 'x', 2: 'y', 33: 'z', 4: 'w'])
println([3, 1, 2, 10].groupBy { it % 3 })
println([1, 22, 333].countBy { it.toString().size() })
println([10, 9, 100].collectEntries { [(it): it * 2] }.keySet().sort())
println m.inspect()
def kw = [true: 1, false: 0, null: 'n', if: 2, class: 3, 'a b': 4]
println kw
println kw.keySet()*.getClass()*.getSimpleName()
println kw.true
println kw['true']
println kw[true]
def lk = [[1, 2]: 'p']
println lk[[1, 2]]
println lk.keySet()*.getClass()*.getSimpleName()

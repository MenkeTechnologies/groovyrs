def t(String n, Closure c) { try { println "$n: " + c() } catch (Throwable e) { println "$n: EXC " + e.getClass().getName() } }
def e = new Expando()
e.name = "bob"
e.age = 3
e.greet = { -> "hi " + name }
e.add = { a, b -> a + b }
t("prop") { e.name }
t("missing") { e.zzz }
t("call") { e.greet() }
t("call2") { e.add(2, 3) }
t("str") { new Expando(a: 1, b: [2]).toString() }
t("props") { e.getProperties().keySet() }
t("class") { e.getClass().getName() }
t("ctor") { def x = new Expando(a: 1, b: 2); x.a + x.b }
t("ctorStr") { new Expando(a: 1) }
t("nomethod") { e.nope() }
t("eq") { new Expando(a: 1) == new Expando(a: 1) }
t("hasProp") { e.name != null }
t("sub") { e['name'] }
t("set2") { e.age += 1; e.age }
t("self") { def z = new Expando(); z.n = 2; z.twice = { -> n * 2 }; z.twice() }
t("delegate") { def z = new Expando(); z.n = 2; z.inc = { -> n = n + 1 }; z.inc(); z.n }
t("emptystr") { new Expando() }
t("truth") { new Expando() ? 't' : 'f' }
t("hash") { new Expando(a: 1).hashCode() == [a: 1].hashCode() }
t("nonclosure") { def z = new Expando(); z.x = 5; z.x() }
t("getProperty") { e.getProperty("age") }
t("setProperty") { e.setProperty("k", 9); e.k }
t("with") { e.with { age } }
t("cls") { e.class.name }
t("hashkeys") { def h = new Expando(); ['zeta', 'alpha', 'mid', 'b'].each { h.setProperty(it, it.size()) }; h.toString() }
t("toStringClosure") { def z = new Expando(); z.toString = { -> "custom" }; z.toString() }

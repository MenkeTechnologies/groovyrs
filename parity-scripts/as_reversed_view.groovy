def t(String n, Closure c) { try { println "$n: " + c() } catch (Throwable e) { println "$n: EXC " + e.getClass().getName() } }
def l = [1, 2, 3]
def r = l.asReversed()
t("r") { r }
t("live") { l << 4; r }
t("get") { r[0] }
t("size") { r.size() }
t("set") { r[0] = 9; [l, r] }
t("add") { r << 5; [l, r] }
t("remove") { r.remove(0); [l, r] }
t("each") { def out = []; r.each { out << it }; out }
t("collect") { r.collect { it * 2 } }
t("class") { r.getClass().getName() }
t("eq") { r == [4, 3, 2, 1] }
t("arr") { ([1, 2] as int[]).asReversed() }
t("empty") { [].asReversed() }
t("neg") { r[-1] }
t("sub") { r.subList(0, 2) }
t("join") { r.join('-') }
t("plus") { r + [0] }
t("sort") { r.sort() }
t("l") { l }

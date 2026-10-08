def t(String n, Closure c) { try { println "$n: " + c() } catch (Throwable e) { println "$n: EXC " + e.getClass().getName() + " | " + e.message } }
t("of") { Optional.of(3) }
t("empty") { Optional.empty() }
t("ofNullable") { Optional.ofNullable(null) }
t("map") { Optional.of(3).map { it + 1 }.get() }
t("mapnull") { Optional.of(3).map { null } }
t("filter") { Optional.of(3).filter { it > 5 } }
t("orElse") { Optional.empty().orElse(9) }
t("orElseGet") { Optional.empty().orElseGet { 10 } }
t("isPresent") { Optional.of(1).isPresent() }
t("isEmpty") { Optional.empty().isEmpty() }
t("getEmpty") { Optional.empty().get() }
t("ofNull") { Optional.of(null) }
t("flatMap") { Optional.of(2).flatMap { Optional.of(it * 5) } }
t("ifPresent") { def r = []; Optional.of(4).ifPresent { r << it }; r }
t("truth") { Optional.empty() ? "t" : "f" }
t("truth2") { Optional.of(0) ? "t" : "f" }
t("eq") { Optional.of(1) == Optional.of(1) }
t("class") { Optional.of(1).getClass().getName() }
t("orElseThrow") { Optional.empty().orElseThrow() }
t("orElseThrow2") { Optional.empty().orElseThrow { new IllegalStateException("x") } }
t("or") { Optional.empty().or { Optional.of(7) }.get() }
t("str") { "v=${Optional.of('a')}" }
t("hash") { Optional.of(1).hashCode() }
t("hashE") { Optional.empty().hashCode() }
t("orNull") { Optional.empty().orElse(null) }
t("ifPresentOrElse") { def r = []; Optional.empty().ifPresentOrElse({ r << it }, { r << 'none' }); r }
t("eqE") { Optional.empty() == Optional.empty() }
t("neq") { Optional.of(1) == Optional.of(2) }
t("inList") { [Optional.of(1), Optional.empty()] }
t("str2") { Optional.of([1, 2]).toString() }
t("mapStr") { Optional.of([a: 1]) }
t("chain") { Optional.ofNullable("abc").map { it.toUpperCase() }.filter { it.size() == 3 }.orElse("none") }
t("isEq") { Optional.of(5).equals(Optional.of(5)) }
t("ofNullable2") { Optional.ofNullable(8).get() }

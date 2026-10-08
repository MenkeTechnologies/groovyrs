def t(String n, Closure c) { try { println "$n: " + c() } catch (Throwable e) { println "$n: EXC " + e.getClass().getName() + " | " + e.message } }
def r = UUID.randomUUID()
t("len") { r.toString().length() }
t("ver") { r.version() }
t("variant") { r.variant() }
t("cls") { r.getClass().getName() }
t("fmt") { r.toString() ==~ /[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}/ }
t("distinct") { UUID.randomUUID() != UUID.randomUUID() }
def u = UUID.fromString("123e4567-e89b-12d3-a456-426614174000")
t("from") { u }
t("fromVer") { u.version() }
t("eq") { u == UUID.fromString("123E4567-E89B-12D3-A456-426614174000") }
t("msb") { u.getMostSignificantBits() }
t("lsb") { u.leastSignificantBits }
t("hash") { u.hashCode() }
t("cmp") { u.compareTo(UUID.fromString("00000000-0000-0000-0000-000000000001")) }
t("bad") { UUID.fromString("nope") }
t("short") { UUID.fromString("1-2-3-4-5") }
t("ctor") { new UUID(1L, 2L) }
t("str") { "id=${u}" }
t("nil") { new UUID(0, 0).toString() }
t("neg") { new UUID(-1L, -1L) }
t("inSet") { [u, UUID.fromString(u.toString())].toSet().size() }
["1--3-4-5", "g-2-3-4-5", "123e4567-e89b-12d3-a456-42661417400g", "123e4567-e89b-12d3-a456-4266141740001", "1-2-3-4", "-1-2-3-4", "11111111111111111-2-3-4-5", "1-2-3-4-5-6", "+1-2-3-4-5", "-5-2-3-4-5", "8000000000000000-0-0-0-0", "+-1-2-3-4", "-", "#-1-2-3-4", "1-2-3-4-+"].each { s -> t(s) { UUID.fromString(s) } }
t("lt") { UUID.fromString("1-2-3-4-5") < UUID.fromString("1-2-3-4-6") }
t("sort") { [UUID.fromString("0-0-0-0-2"), UUID.fromString("ffffffff-0-0-0-1"), UUID.fromString("0-0-0-0-1")].sort() }
t("negcmp") { new UUID(-1L, 0L).compareTo(new UUID(1L, 0L)) }
t("negvariant") { new UUID(0L, -1L).variant() }
t("v0") { new UUID(0L, 0L).variant() }
t("v1") { new UUID(0L, 0x4000000000000000L).variant() }
t("hashn") { new UUID(-1L, 5L).hashCode() }

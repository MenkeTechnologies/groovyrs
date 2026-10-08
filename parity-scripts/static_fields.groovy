// A static field is one value per class, reached through the class, an
// instance, or a bare name in the class's code, and initialised (superclass
// first) on the class's first use.
class P { static final int K = 3; static String s = "x" + K }
println P.K
println P.s
class Q { static int n = 0; def id; Q() { n++; id = n }; static int count() { n } }
def a = new Q(); def b = new Q()
println Q.n
println a.id + " " + b.id
println Q.count()
println a.n
Q.n = 10
println Q.count()
a.n = 20
println Q.n
Q.n += 5
println Q.n
class R { static List items = []; void add(x) { items << x } }
new R().add(1); new R().add(2)
println R.items
class S extends Q { static int m = n + 100 }
println S.m
println S.n
class T { static int t; static boolean flag; static String str }
println "${T.t} ${T.flag} ${T.str}"
class U { static u = { println 'init U'; 7 }(); static hello() { 'hi' } }
println 'before'
println U.hello()
println U.u

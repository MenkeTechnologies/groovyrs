class P { def a = 1; public pub = 2; private priv = 3; protected prot = 4; static st = 5; String getG() { "g" }; boolean isOk() { true }; static String getSg() { "sg" }; void setW(x) {}; final fin = 6; int n; String getURL() { "u" } }
def p = new P()
for (n in ["a", "pub", "priv", "prot", "st", "g", "ok", "sg", "w", "fin", "class", "n", "URL", "uRL", "zz"]) {
  def mp = p.hasProperty(n)
  println "$n: ${mp?.getClass()?.getName()} ${mp?.name} ${mp?.type} ${mp ? 'yes' : 'no'}"
}
println p.properties
println P.hasProperty("a")?.getClass()?.getName()
println P.hasProperty("q")
class Q { def zeta = 1; def alpha = 2; String getMid() { "m" }; def getAaa() { 0 }; private priv = 3; static st = 9; public pubf = 5 }
println new Q().properties
println new Q().properties.getClass().getName()
class T { }
println new T().properties
class S { def name; S(n) { name = n } }
println new S("x").properties
println new S("y").properties.name
enum E { X, Y }
println E.X.properties
println E.X.hasProperty("declaringClass")?.name

enum Suit { HEARTS, SPADES, CLUBS }
def s = Suit.SPADES
switch (s) {
  case Suit.HEARTS: println "h"; break
  case Suit.SPADES: println "s"; break
}
println Suit.values().collect { it.name().toLowerCase() }
println([Suit.CLUBS, Suit.HEARTS].sort())
println([Suit.CLUBS, Suit.HEARTS].max())
println Suit.CLUBS.ordinal() + Suit.HEARTS.ordinal()
println s == Suit.SPADES
println s != Suit.CLUBS
println s.getClass().getSimpleName()
println Suit.valueOf('HEARTS') == Suit.HEARTS
try { Suit.valueOf(null) } catch (e) { println e.getClass().getName() + ": " + e.message }
println "card: $s"
println s.toString().length()
for (x in Suit.values()) print x.ordinal()
println()
println Suit.values().find { it.name().startsWith("C") }
enum Op {
  PLUS("+"), MINUS("-")
  final String sym
  Op(String sym) { this.sym = sym }
  def apply(a, b) { this == PLUS ? a + b : a - b }
}
println Op.MINUS.sym
println Op.PLUS.apply(2, 3)
println Op.MINUS.apply(2, 3)
enum Level { LOW(1), HIGH(10)
  int v
  Level(int v) { this.v = v }
  String toString() { "L" + v }
}
println Level.values()
println Level.HIGH.name()
println Level.valueOf("LOW")
enum Color { RED, GREEN, BLUE }
println Color.BLUE.next()
println Color.RED.previous()
println Color.MIN_VALUE
println Color.MAX_VALUE
println Color.RED.getClass().getName()
println Color.RED instanceof Enum
println Color.RED instanceof Comparable
try { Color.valueOf("X") } catch (e) { println e.getClass().getName() + ": " + e.message }
println Color.values().getClass().getSimpleName()
println Color.RED.compareTo(Color.BLUE)
println Color.RED <=> Color.BLUE
println Color.values()*.ordinal()
println Color.RED.hashCode() == Color.RED.hashCode()
println "${Color.GREEN}"
println Color.GREEN.toString()
println Color.RED.equals(Color.RED)
println(Color.RED in [Color.RED])
println Color.valueOf("BLUE").name()
switch (Color.BLUE) { case [Color.RED, Color.BLUE]: println "in"; break }
println Color.RED.declaringClass
println Color.values().size()
println Color.values().length
enum Planet { EARTH(5.97), MARS(0.642); final double mass; Planet(double m) { mass = m }; String toString() { "P" + mass } }
println Planet.MARS
println Planet.EARTH.mass
println Planet.values()*.name()

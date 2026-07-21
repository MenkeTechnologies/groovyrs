// A class with fields, a constructor, methods, `this`, an auto getter, and a
// `toString` used by println.
class Rectangle {
  def width
  def height
  Rectangle(w, h) { this.width = w; this.height = h }
  def area() { width * height }
  def getPerimeter() { 2 * (width + height) }
  String toString() { "Rectangle(" + width + "x" + height + ")" }
}

def r = new Rectangle(3, 4)
println r
println "area = " + r.area()
println "perimeter = " + r.perimeter
r.width = 5
println "after resize, area = " + r.area()

// A field initializer and a no-argument construction.
class Counter {
  def count = 0
  def bump() { count += 1; return count }
}
def c = new Counter()
println "bumps: " + c.bump() + ", " + c.bump() + ", " + c.bump()

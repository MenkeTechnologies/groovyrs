def sb = new StringBuilder('hello world')
println sb.length()
println sb.charAt(1)
println sb[0]
println sb[-1]
println sb.indexOf('o')
println sb.indexOf('o', 5)
println sb.lastIndexOf('o')
println sb.substring(2)
println sb.substring(1, 4)
println sb.subSequence(0, 5)
sb.setCharAt(0, 'J' as char)
println sb
sb.insert(0, '>> ').append('!').append(1).append(2.5).append(true).append([1, 2])
println sb
println sb.deleteCharAt(0)
println sb.delete(0, 2)
println sb.replace(0, 5, 'HELLO')
sb.setLength(5)
println sb
sb.setLength(7)
println sb.length()
println sb.reverse()
println sb.capacity() >= 7
println sb.toString().size()
def b2 = new StringBuilder()
b2 << 'a' << 1 << 'b'
println b2
println b2.append('xyz', 1, 2)
println b2.appendCodePoint(65)
println b2.contains('b')
println b2.take(2)
println b2.drop(2)
println b2.padLeft(10, '*')
println b2.replaceAll('[0-9]', '#')
println b2.each { }
println b2.isEmpty()
println new StringBuilder().isEmpty()
println b2.compareTo(new StringBuilder('a1'))
println b2 == b2
println b2.equals(new StringBuilder('a1bxA'))
try { b2.charAt(99) } catch (e) { println e.getClass().getSimpleName() }
try { b2.deleteCharAt(99) } catch (e) { println e.getClass().getSimpleName() }
try { b2.insert(99, 'x') } catch (e) { println e.getClass().getSimpleName() }
try { b2.setCharAt(99, 'x' as char) } catch (e) { println e.getClass().getSimpleName() }
try { b2.substring(3, 1) } catch (e) { println e.getClass().getSimpleName() }
try { b2.startsWith('a') } catch (e) { println e.getClass().getSimpleName() }
def buf = new StringBuffer('abc')
buf.append('d').reverse()
println buf
println buf.getClass().getName()
println 'abc'.chars().sum()
println new StringBuilder('xyz').chars().sum()

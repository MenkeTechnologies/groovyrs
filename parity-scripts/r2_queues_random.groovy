def dq = new ArrayDeque()
dq.add(1); dq.addFirst(0); dq.addLast(2); dq.offer(3); dq.push(-1)
println dq
println dq.peekFirst()
println dq.peekLast()
println dq.pollFirst()
println dq.pollLast()
println dq.pop()
println dq
println dq.size()
println dq.contains(1)
println dq.getClass().getName()
println dq instanceof Deque
println dq instanceof Queue
println dq instanceof List
try { new ArrayDeque().removeFirst() } catch (e) { println e.getClass().getName() }
try { new ArrayDeque().element() } catch (e) { println e.getClass().getName() }
println new ArrayDeque().poll()
println new ArrayDeque().peek()
try { new ArrayDeque().add(null) } catch (e) { println e.getClass().getName() }

def pq = new PriorityQueue()
[9, 4, 7, 1, 8, 2, 6].each { pq.add(it) }
println pq
println pq.peek()
pq.remove(7)
println pq
def drained = []
while (!pq.isEmpty()) drained << pq.poll()
println drained
def maxq = new PriorityQueue({ a, b -> b <=> a } as Comparator)
maxq.addAll([3, 9, 1, 5])
println maxq
println maxq.poll()
println new PriorityQueue(['pear', 'fig', 'apple', 'kiwi'])
println new PriorityQueue([5, 3, 8, 1, 9, 2])
try { new PriorityQueue().remove() } catch (e) { println e.getClass().getName() }
println new PriorityQueue().poll()
println pq.getClass().getName()

def st = new Stack()
st.push('a'); st.push('b'); st.push('c')
println st
println st.peek()
println st.pop()
println st.search('a')
println st.empty()
println st.size()
println st.collect { it.toUpperCase() }
println st.getClass().getName()
println st instanceof List
try { new Stack().pop() } catch (e) { println e.getClass().getName() }
try { new Stack().peek() } catch (e) { println e.getClass().getName() }

def ll = new LinkedList([1, 2, 3])
ll.addFirst(0)
ll << 4
println ll
println ll.removeFirst()
println ll.removeLast()
println ll.peek()
println ll.poll()
println ll
println ll.getFirst()
println ll.getLast()
println ll.indexOf(3)
println ll.getClass().getName()
println ll instanceof List
println ll instanceof Deque
println ll instanceof ArrayList

def r = new Random(42)
println([r.nextInt(100), r.nextInt(100), r.nextInt(100)])
println r.nextInt()
println r.nextLong()
println r.nextDouble()
println r.nextBoolean()
println r.nextGaussian()
println r.nextInt(5, 10)
def r2 = new Random(42)
println r2.nextInt(100)
r2.setSeed(42)
println r2.nextInt(100)
def shuffled = (1..10).toList()
Collections.shuffle(shuffled, new Random(7))
println shuffled
println r.getClass().getName()

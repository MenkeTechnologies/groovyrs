# Known gaps

An honest list of what groovyrs does **not** do yet. The target is the Groovy
*script* model — top-level statements, functions, classes, closures, control
flow, exceptions, and the `println`/`print` commands. Unsupported constructs are
reported as parse or compile errors, never silently mis-run.

## Implemented

- **User-defined functions.** `def f(a, b) { … }` (and typed `Type f(…) { … }`)
  compile to fusevm subroutine regions over the native `Op::Call` frame ABI:
  parameters and locals live in frame slots, so recursion and mutual recursion
  (forward references resolve) are sound. Explicit `return <expr>` carries a
  value out; a function with no explicit `return` returns the value of its last
  statement when that statement is a value expression, else `null`.
- **Method / property dispatch on values.** `s.length()`, `list.size()`,
  `"hi".toUpperCase()`, `map.k`, and chains on literals (`[1,2,3].size()`) route
  through a host GDK dispatch. See the README's *Pure GDK* and *Closure-driven
  GDK* entries for what is modeled. `size`/`length` are **methods, not
  properties**, exactly as in Groovy: `[1, 2].size` and `"abc".length` both raise
  `MissingPropertyException`. A **map**'s property access is only ever a key read
  (`m.k` == `m['k']`, `null` when absent) — including the names that are
  properties on every other value, so `[a:1].size` and `[a:1].class` are `null`
  in Groovy while `[size: 9].size` is `9`. An unknown method/property faults
  rather than mis-running.
- **List and map literals.** `[1, 2, 3]`, `[]`, `[a: 1]`, `[:]` build a host-heap
  `java.util.ArrayList` and an insertion-ordered map, and print Groovy-style
  (`[1, 2, 3]`, `[a:1]`, `[:]`).
- **Lists are references, like Groovy's.** `def b = a` gives one `ArrayList` a
  second name, so `b.add(4)` shows through `a`, and `a.is(b)` is `true` while a
  `collect` copy is `false`. The same holds for a list reached through a map
  value, an element of another list, a closure parameter, or a capture, and for
  every in-place mutator — `add`, `remove`/`removeAt`, `set`, `clear`, `push`,
  `pop`/`removeLast`, `<<`, `addAll`, `removeAll`, `retainAll`, `swap`, `sort`,
  `unique`, and `list[i] = v` — whatever shape the receiver is reached through
  (a bare name, a field, a map entry, a subscript). The methods Groovy answers
  the receiver with (`sort`, `unique`, `<<`, `swap`, `each`, `eachWithIndex`)
  answer the same reference, so `a.is(a.sort())` is `true`; `reverse()`,
  `sort(false)` and `collect` answer a new list, and are `false`. `reverse(true)`
  is the mutating spelling and joins the first group — it reverses the receiver
  through `set`, so a `subList` window over it stays live.
  `removeAll`/`retainAll` accept Groovy's *predicate closure* as well as a
  collection, and `push` inserts at the front — the end `pop` takes from.
- **`Object.is()` / `equals()`.** `is()` is handle identity, answered by every
  reference value (list, map, set, range, matcher, buffer, closure, instance).
  `equals()` on a collection agrees with `==`, including the two cross-type
  answers: a list never equals a `Set`, and a list does equal the `Range`
  enumerating the same elements.
- **`Object.hashCode()`.** Java's specified rule for each type, not an
  approximation: `String` folds UTF-16 code units (so an astral character counts
  as its surrogate pair), `Integer` is the value while `Long` folds its halves,
  `Double` folds `doubleToLongBits` with the canonical NaN, `BigDecimal` is
  `31 * unscaled + scale` (so `1.5` and `1.50` differ, as they do under
  `equals`), `BigInteger` folds its magnitude words big-endian, `AbstractList`
  seeds at 1 and multiplies by 31, `AbstractMap` and `Map.Entry` use
  `key ^ value`, `AbstractSet` *sums* its elements (order-independent, so a
  `LinkedHashSet` and the `TreeSet` of the same elements agree), and `IntRange`
  has its own — the Cantor pairing `(from + to + 1) * (from + to) / 2 + to` of
  its normalised inclusive bounds, read off `IntRange.hashCode`'s bytecode.
  `NumberRange`, `ObjectRange` and `EmptyRange` declare none and inherit
  `AbstractList`'s. A user class's own `hashCode` overrides all of it. See the
  simplification note below for the types groovyrs cannot distinguish.
- **`java.util.Set`.** `as Set`, `toSet()` and
  `new HashSet`/`LinkedHashSet`/`TreeSet` build a real set behind a handle, not a
  de-duplicated list. `getClass()` names the implementation, `==` ignores order
  and is never true against a `List`, `add` answers `false` for an element
  already present and mutates through the handle, and the operators
  re-de-duplicate — `([1, 2] as Set) + ([2, 3] as Set)` is `[1, 2, 3]`. The
  methods whose Groovy result is a `List` rather than a `Set` (`collect`, `sort`,
  `toList`) still answer a list, and `+` dispatches on its left operand, so
  `[1, 2] + ([2, 3] as Set)` is the four-element `[1, 2, 2, 3]`.
- **`permutations()` / `subsequences()`.** Both answer a
  `java.util.HashSet<List>`, so they de-duplicate (`[1, 1].permutations()` is one
  entry) and print in the JDK's bucket order rather than generation order —
  `[1,2,3].subsequences()` is `[[1], [1, 2, 3], [2], [2, 3], [1, 2], [3], [1, 3]]`.
  That needs `List.hashCode` (`31 * acc + element`) through the spread and bucket
  walk below, at *every* intermediate step for `subsequences`, whose answer is
  grown one element at a time and whose insertion order into each round's set is
  the previous round's bucket order. `permutations { … }` is `collect` over that
  same set and answers an `ArrayList`; `combinations()` really is a `List`.
- **`HashSet` iteration order.** A `HashSet` presents its elements in the JDK's
  table order — a stable sort of the insertion sequence by
  `(capacity - 1) & (h ^ (h >>> 16))` — rather than in insertion order, so
  `new HashSet([17, 5, 33, 2, 20, 9])` prints `[17, 33, 2, 20, 5, 9]`. The table
  size is the one the *constructor* asked for, which differs between paths:
  `toSet()` asks for the element count and `new HashSet(collection)` for
  `size / 0.75 + 1` (min 16), so the same six elements come out as
  `[17, 33, 9, 2, 20, 5]` through the first and `[17, 33, 2, 20, 5, 9]` through
  the second. Not modeled: a bucket that treeifies (8 collisions with a table of
  64+), and an element whose hash is the JVM identity hash — that one keeps its
  insertion position, and it is not reproducible across two JVM runs either.
- **`subList`, as a live view.** `list.subList(from, to)` answers a
  `java.util.ArrayList$SubList` — a **window** onto the backing list, not a copy.
  A write through the window reaches the backing list (`s.set(0, 99)`,
  `s[0] = 99`) and a write to the backing list shows through the window; a
  *structural* write through the window (`add`, `add(i, e)`, `remove`, `clear`,
  `addAll`, `pop`, `push`, `removeLast`, `unique`, `removeAll`, `retainAll`)
  splices the backing list at the window's offset and resizes the window with it,
  so `[1,2,3,4].subList(1,3).add(99)` leaves `[1, 2, 3, 99, 4]`. `sort()` orders
  that stretch of the backing list in place and answers the window. A window onto
  a window addresses the root list, and a structural write through it resizes
  every window it was taken through.

  Java's **fail-fast** rule is modeled with the JDK's mechanism, an `ArrayList`
  `modCount` the window syncs to at every operation: a structural change made to
  the backing list through any *other* reference — a second name for it, or a
  sibling window — invalidates the window permanently, and every later read or
  write through it raises the message-less
  `java.util.ConcurrentModificationException`. `getClass()` and `is()` still
  answer, since they read the reference rather than the elements, and a window
  taken *after* the change is fine.

  Which operations count is the JDK's answer, not a length test, and it differs
  between the list and a window taken onto it. Three bump the counter without
  changing any length: `a.sort()` (`ArrayList.sort` bumps unconditionally, while
  `s.sort()` on a *window* runs `List.sort`'s default, which reorders through
  `set` and does not), `a.addAll([])` (`ArrayList.addAll` bumps before it looks
  at the argument, while `SubList.addAll` returns first), and `clear()` on an
  already-empty receiver (both). `unique()` is Groovy's own and ends in
  `clear()` + `addAll(…)` whatever it found, so it bumps at size 2 and up —
  except that the no-argument and `unique(true)` forms return early below that
  while `unique { … }` never does. `set`, `swap`, `sort(false)`, and a
  `removeAll`/`retainAll` that removed nothing leave the window live. Each of
  these is measured against Apache Groovy 5.0.8; the table is in
  `host::bumps_mod_count`.

  Bounds are Java's exact behaviour — `IndexOutOfBoundsException` naming
  `fromIndex` or `toIndex`, `IllegalArgumentException` for a reversed range, and
  the JDK's check *order*, so `[1, 2, 3].subList(9, 5)` reports `toIndex = 5`
  rather than the reversal. `range.subList(from, to)` answers another range
  (`(1..5).subList(1, 3)` is `2..3`, the empty window an `EmptyRange`), which is
  what Groovy's `IntRange` does.
- **`++`/`--` in expression position.** Both postfix (`i++`, value before
  update) and prefix (`++i`, value after update), in addition to the statement
  forms.
- **Closures.** `{ a, b -> … }`, the explicit zero-parameter `{ -> … }`, and the
  implicit `{ it }` single-parameter form
  are first-class callable values: a closure lowers to a fusevm subroutine
  region and a runtime handle, invoked through the native `Op::Call` frame ABI
  via `.call(args)` or direct call (`def f = { it * 2 }; f(21)`). A closure
  captures its enclosing **script** scope by reference (a later mutation of a
  captured binding is visible).
- **Nested-closure upvalue capture.** A closure defined inside a function or
  another closure captures that enclosing frame's locals as upvalues, so a
  curried `{ x -> { y -> x + y } }` works and a factory (`def make(n) { return
  { it + n } }`) keeps `n` after the outer frame returns. Chained calls
  `f(a)(b)` parse (postfix call-application). Capture of a frame local is
  **by value** at closure-creation time (see the simplification note below).
- **Closure combinators.** `curry` / `rcurry` / `ncurry`, `memoize`, `andThen` /
  `compose` and the `>>` / `<<` operators, and `clone`. Each answers a *derived*
  closure — a handle that
  wraps the callable it was built from rather than a body region of its own — so
  it is a closure everywhere a closure is expected (`.call`, a GDK argument,
  another combinator) and reports the arity it still accepts.
- **`with` / `tap` delegate to the receiver.** A bare call inside the closure
  that the script cannot resolve dispatches against the receiver — Groovy's
  `OWNER_FIRST` chain, innermost `with` first — so `[:].with { put('a', 1) }` and
  `'abc'.with { toUpperCase() }` work and a script closure of the same name still
  wins. A list mutator writes through, so `[1, 2].tap { add(3) }` is
  `[1, 2, 3]`.
- **`java.util.Iterator`.** `iterator()` on a list, a map (its entries), a range
  and a `String` answers a live cursor behind a shared handle: `next()` advances
  it for every holder, `hasNext()` reports what is left, and `next()` past the
  end raises `NoSuchElementException`.
- **Classes.** `class C { fields; C(..){..}; def m(){..} }`, `new C(args)`,
  fields (with initializers), constructors (arity-dispatched), methods with an
  implicit `this`, property get/set, and Groovy's auto getter/setter over a
  field (`getX`/`setX`). A bare field name inside a method resolves to
  `this.field`; `toString()` drives `println`. Instances live in the host object
  heap behind a `Value::Obj` handle (reference identity), so a method mutating a
  field is visible through every reference to the object.
- **Subscripting (`recv[i]`).** List (with a negative index counting from the
  end), map (`m[k]`), and String element reads, plus a user `getAt(i)` overload
  on a class instance.
- **Insertion-ordered maps.** A map literal `[k: v, …]` builds a host-side
  ordered map (a `LinkedHashMap` equivalent) behind a `Value::Obj` handle, so a
  multi-entry map prints in insertion order and `m.k = v` mutates it in place
  (the new key appends). `size`, `containsKey`, `get`, `keySet`/`keys`, and
  `values` dispatch over it.
- **Collection `+`.** `+` dispatches on its left operand: a list concatenates
  another list or appends a scalar (`[1, 2] + 3` → `[1, 2, 3]`), a map merges
  another map (right wins on a duplicate key, order preserved), and a `String`
  concatenates. This is the built-in behavior for lists/maps/strings; a
  user-class left operand instead dispatches its `plus` method (see operator
  overloading below).
- **Operator overloading.** A user-class instance operand dispatches the Groovy
  operator method: `+`→`plus`, `-`→`minus`, `*`→`multiply`, `/`→`div`,
  `%`→`remainder`, `**`→`power`, unary `-`→`negative`; `<`/`>`/`<=`/`>=` and
  `<=>` go through `compareTo`; `==`/`!=` use `compareTo` (when the class defines
  it, i.e. is `Comparable`) else `equals`, and are null-safe (an instance is
  never `== null`); `recv[i]` uses `getAt`. Primitive (`Int`/`Float`/`String`)
  operands never route to a method — only a user-class operand does. (They can
  still reach the numeric hook: `Integer`-range overflow and an integral/`double`
  pair whose integer is past 2^53 are delegated, and answered there as Groovy's
  binary numeric promotion answers them.) Mechanism: the strict numeric hook (and the `GDIV`/`GCMP` builtins)
  re-enter the running VM through a published thread-local pointer to invoke the
  method — no fusevm change.
- **Inheritance.** `class C extends B { … }` with single-inheritance method and
  field inheritance, virtual dispatch (the most-derived override wins, and a base
  method calling a virtual method reaches the override), `super.m(args)` (static
  parent dispatch), `super(args)` constructor chaining, inherited field
  initializers, `value instanceof Type` (user chain + built-in type names, with
  `null instanceof X` false), and `@Override` / other annotations parsed and
  ignored. A leading `abstract`/`public`/`final` modifier on the declaration is
  accepted and ignored.
- **Interfaces.** `interface I { … }`, `class C implements A, B`, and an
  interface's own multiple `extends A, B`. A method declared with no body is an
  abstract declaration (it binds nothing, and a sibling `default` method may
  still call it by bare name); a method declared *with* a body is a Java 8
  `default` every implementor inherits, which a class definition overrides.
  `instanceof` walks the whole type closure — superclasses and interfaces,
  transitively — so `impl instanceof I` answers through a superclass that
  implements `I` and through an interface that extends another. An interface
  cannot be instantiated. `trait` is still not implemented.
- **Closure-driven GDK iteration.** Over lists (and the elements a `Range`
  enumerates):
  `each`, `eachWithIndex`, `collect`, `findAll`, `find`, `inject` (both the
  `inject(init){…}` and seedless `inject{…}` forms), `sum` (bare, seeded, and
  closure forms), `sort`, `unique`, `max`, `min`, `groupBy`, `join`, and
  `reverse` — e.g. `[1,2,3].collect { it * 2 }` → `[2, 4, 6]` and
  `[1,2,3,4].findAll { it % 2 == 0 }` → `[2, 4]`. A one-parameter closure
  argument to `sort`/`unique`/`max`/`min` is Groovy's *key extractor*, a
  two-parameter one a *comparator*, and the sort is stable, like
  `Collections.sort`.
  Over maps: `each`, `eachWithIndex`, `collect` (→ a list), `findAll` (→ a map),
  `find` (→ a `Map.Entry`), `any`, `every`, `groupBy` (→ a map of sub-maps),
  `inject`, `sort` (by key), `max`, `min`. A two-parameter closure receives
  `(key, value)` and a one-parameter closure a `Map.Entry` (which prints `k=v`
  and answers `key`/`value`/`getKey()`/`getValue()`) — the same rule Groovy uses.
- **The spread operator `*.`.** `list*.member` and `list*.method(args)` apply the
  member to every element and collect the results. It is exactly
  `list.collect { it?.member }`, including the safe navigation, so a `null`
  element spreads to `null`.
- **`for (x in <collection>)`.** Beyond the range form, the loop walks a list's
  elements, a map's `Map.Entry`s, a `String`'s characters, zero iterations for
  `null`, and any other value exactly once — Groovy's own iteration rules. The
  sequence is materialised once before the loop, so a body that mutates the
  collection still walks the original elements.
- **`getClass()` / the `.class` property.** Both answer a `java.lang.Class`
  value that prints `class java.lang.Integer`, and whose `getName()`/`name`,
  `getSimpleName()`/`simpleName`, and `getCanonicalName()` answer. `null` reports
  `class org.codehaus.groovy.runtime.NullObject`, which is what Groovy's
  `NullObject` gives.
- **`String.toBigDecimal()`.** `new BigDecimal(text.trim())`, with the exact
  scale (`"100.00"` → `100.00`) and with `BigDecimal`'s own character-level
  `NumberFormatException` diagnostics: `Character x is neither a decimal digit
  number, decimal point, nor "e" notation exponential mark.`, `Character array
  contains more than one decimal point.`, `No digits found.`, `Not a digit.`,
  `Too many nonzero exponent digits.`, `Exponent overflow.` — plus the two
  message-*less* forms (an empty string, and an exponent mark with no digits
  after it), where `e.getMessage()` is `null`.
- **First-class ranges.** `0..5` (inclusive), `0..<5` (half-open), descending
  (`5..1`) and character (`'a'..'e'`) — a real `groovy.lang.Range` object, not a
  materialised list: it prints `1..5`, `getClass()` names `IntRange` /
  `ObjectRange` / `NumberRange` from its endpoints, and `from` / `to` / `step(n)`
  / `reverse()` / `size()` / `contains(x)` / `isReverse()` are its own members.
  Because Groovy's `Range` *is* a `java.util.List`, every other call, operator
  and subscript reads the list it enumerates, so `.each`, `.collect`, `+`,
  `== [1, 2, 3]`, `in`, `[1..2]` and `instanceof List` all apply. The
  `for (x in a..b)` loop still lowers to a counted loop and allocates nothing.
  The walk *steps* from one endpoint to the other with `next`/`previous` rather
  than renumbering, so it keeps the element type: `1.5..4.0` enumerates
  `[1.5, 2.5, 3.5]`, not `[1, 2, 3]`. `from`/`to` report the bounds of what is
  actually enumerated — `(4..0).from` is 0 and `(0..<4).to` is 3 — except on a
  `NumberRange`, which keeps its endpoints as written. An exclusive range with
  equal endpoints is a `groovy.lang.EmptyRange`.
- **`next()` / `previous()`** on `Integer`, `BigInteger`, `BigDecimal` and
  `String`: the successor and predecessor a range walks with. A `String` moves
  its *last* character by one code point, so `'a'.next()` is `'b'` and
  `'z'.next()` is `'{'`.
- **Ternary, Elvis, safe navigation.** `c ? t : e`, the Elvis `a ?: b`
  (null/false-coalescing) and its assigning form `a ?= b` (which is `a = a ?: b`,
  so a `0` or an empty list is overwritten too, and a property or a subscript is
  a target as well), and `a?.member` / `a?.method()` (yields `null` on a
  `null` receiver rather than faulting). All branch on Groovy truthiness.
- **`BigDecimal` decimals.** An unsuffixed decimal literal is a
  `java.math.BigDecimal` with the literal's exact scale, not an `f64`: it prints
  through `BigDecimal.toString` (`2.5e7` → `2.5E+7`, `100.00` → `100.00`,
  `1e-7` → `1E-7`) and carries scale through arithmetic — `2.5e7 + 1` is
  `25000001`, `1.25 * 0` is `0.00`, `0.1 + 0.2` is exactly `0.3`. `/` follows
  Groovy's `BigDecimalMath` policy (exact quotient at Java's preferred scale, else
  `max(precision) + 10` significant digits clamped to `max(scale, 10)` fraction
  digits, so `1/3` is `0.3333333333`), `%` follows `BigDecimal.remainder`
  including the `BigDecimal % Double` case Groovy keeps exact, and a zero divisor
  raises Groovy's `ArithmeticException` (catchable in a program that uses
  `try`/`catch`, otherwise a hard fault that aborts the run as Groovy's uncaught
  exception does). Magnitude is unbounded
  (`1.5e300 * 1.5e300` is `2.25E+600`). A `d`/`f`-suffixed literal stays an IEEE
  double with `Double.toString` rules (`2.5e7d` → `2.5E7`, `5.0d/0.0d` →
  `Infinity`). Value model in `src/decimal.rs`.
- **`BigInteger`.** A `G`-suffixed integer literal (`123G`), an unsuffixed one
  past `Long` (`9223372036854775808`), `new BigInteger("12")`, `x as BigInteger`
  and the overflowing integer `**` are all `java.math.BigInteger`, a distinct
  type from `BigDecimal`: `getClass()` and `instanceof` say so, and an operator
  keeps it a `BigInteger` when every operand is integral but widens to
  `BigDecimal` when a real decimal takes part or the operator is `/`. `**`
  narrows to the *base's* type where it fits, which is Groovy's own rule and why
  `2 ** 40` is a `BigInteger` while `2L ** 40` is a `Long`; a negative exponent
  leaves the integers entirely and answers a `Double`. Magnitude is unbounded
  (`2 ** 100` is exact).
- **Groovy truthiness.** `null`, `0`, a zero `BigDecimal` (`0.0`, `0.00`, `0e0`),
  `""`, an empty list, an empty map, and `false` are false; every other value is
  true, and a class decides its own truth with `asBoolean()`. `!x`, `if`,
  `while`, `for`, the ternary, `?:`, `&&`, `||`, and the GDK's `find`/`findAll`
  all use it. `&&`/`||` are boolean-*valued* (`5 && 3` is `true`, not `3`);
  Elvis yields the deciding operand (`0.0 ?: "d"` is `"d"`, `1.50 ?: "d"` is
  `1.50`). **The truth test is emitted only where the condition's static shape
  could be a value fusevm reads differently** — a heap handle or a `String`. A
  comparison-shaped guard (`i < n`, `x != 0`, `!done`, `a < b && c > d`) is
  statically a `Boolean`, so `while`/`for` conditions still compile to the same
  native `NumLt`+`JumpIfFalse` pair and stay JIT-traceable (see
  `compiler::needs_truth` / `is_static_bool`).
- **GStrings.** A double-quoted string interpolates `$name`, the dotted property
  path `$a.b.c` (Groovy stops at the path, so `"$n.toString()"` reads the
  property `n.toString`), and `${ expr }` with nested braces and nested string
  literals (`"${ c ? "a" : "b" }"`). `\$` is a literal dollar and a
  single-quoted string never interpolates. Placeholders are re-parsed with the
  ordinary expression grammar, and each part renders the way `println` does — so
  an embedded object goes through its `toString()`. A double-quoted literal with
  no placeholder stays a plain string and compiles exactly as before.
- **Exceptions.** `throw`, `try` / `catch` / `finally`, multi-catch
  `catch (A | B e)`, and the untyped `catch (e)` (which Groovy reads as
  `Exception`). The built-in throwable hierarchy is pre-registered as ordinary
  classes — `Throwable`, `Exception`, `Error`, `RuntimeException`,
  `IllegalArgumentException`, `NumberFormatException`, `IllegalStateException`,
  `ArithmeticException`, `NullPointerException`, `IndexOutOfBoundsException` (+
  the array/string forms), `UnsupportedOperationException`, `ClassCastException`,
  `InterruptedException`, `CloneNotSupportedException`, `AssertionError`,
  `IOException`, `FileNotFoundException`, `NoSuchElementException`,
  `ConcurrentModificationException`, `GroovyRuntimeException`,
  `VirtualMachineError`, `StackOverflowError` — so `catch`
  matching, `instanceof`, and `class MyEx extends Exception { MyEx(String m) {
  super(m) } }` all run through the one class registry. A type may be written
  **fully qualified** wherever a type is named — `catch
  (groovy.lang.MissingMethodException e)`, a multi-catch arm, `instanceof`, and
  `new` — and the package half is checked, so a same-named type from another
  package does not match. A throwable prints as
  `java.lang.Exception: boom` (bare class name for a script-declared subclass),
  and `getMessage()` / `.message` / `toString()` work — as do the JDK's four
  constructors (`T()`, `T(String)`, `T(String, Throwable)`, and `T(Throwable)`,
  whose message is the cause's `toString()`), `getCause()` / `initCause(c)` /
  `.cause`, and `getSuppressed()` / `addSuppressed(t)` / `.suppressed`. A
  `getCause()` with no cause answers `null` and `getSuppressed()` an empty
  list, never absent; `super(message, cause)` reaches the same constructors from
  a script-declared subclass. `finally` runs on every
  exit path: fall-through, a matched handler, an unmatched rethrow, and an early
  `return` / `break` / `continue` out of the block. Groovy's implicit return
  reaches through a trailing `try` (and a trailing `if`), so a closure whose body
  is a `try` yields the taken branch's value. A zero divisor raises a catchable
  `java.lang.ArithmeticException: Division by zero`; an uncaught exception
  reports on stderr and exits 1, matching `groovy`'s exit status.
  **Mechanism:** fusevm has no unwind opcode, so an in-flight exception is a
  host-side pending value plus compiler-emitted jumps to the innermost handler,
  and a post-call check at every site that can re-enter the VM. **A program with
  no `try`/`throw` emits none of those ops**, so its bytecode is unchanged.
- **Catchable runtime faults.** A groovyrs runtime fault is an ordinary Groovy
  throwable, so `try { m.k.length() } catch (Exception e) { … }` reaches its
  handler. Every fault site allocates the throwable Groovy allocates and parks it
  as the pending exception rather than aborting, and every raising builtin is
  emitted with its post-call pending check, so no site can swallow one. Modeled,
  each with Groovy's own message text: `groovy.lang.MissingMethodException` (an
  unknown method, and the `getAt` a subscript on a non-collection desugars to),
  `groovy.lang.MissingPropertyException` (an unknown property read, a write
  to a field the class chain never declared, and a bare *name* nothing binds —
  `println zork` raises rather than printing `null`, naming the script class
  Groovy names: the file's stem, or `script_from_command_line` under `-e`),
  `java.lang.NullPointerException` (`Cannot invoke method m() on null object` /
  `Cannot get property 'p' on null object` — while `null.toString()` and
  `null.equals(x)` still answer, as Groovy's `NullObject` does),
  `java.lang.IndexOutOfBoundsException` (`list.get(i)` past either end),
  `java.lang.StringIndexOutOfBoundsException` (a `String` subscript past the
  end), `java.lang.ArrayIndexOutOfBoundsException` (a negative subscript larger
  than the receiver), and `java.lang.NumberFormatException` (`toInteger` /
  `toLong` / `toDouble` / `toFloat` on text that does not parse, including an
  `int`-overflowing literal). The reads Groovy does *not* fault on stay
  non-faulting: a list subscript past the end and a missing map key are `null`.
  Each throwable sits in the real hierarchy, so `catch (GroovyRuntimeException
  e)`, `catch (IndexOutOfBoundsException e)`, and `instanceof` all behave.
  A fault in a program that uses no `try`/`throw` still aborts (nothing would
  observe the parked throwable), now reporting `groovyrs: <qualified class>:
  <message>` on stderr with the same non-zero exit.

  The two throwables Groovy's dynamic dispatch raises most often carry the
  payload a handler recovers from, not just the message:
  `MissingMethodException.getMethod()` / `getType()` / `getArguments()` and
  `MissingPropertyException.getProperty()` / `getType()`, each also readable as
  a property (`e.method`, `e.property`). A bare-name miss names the *script*
  class in `getType()`, the same name its message quotes. The payload is built
  only when the program has armed exception handling, so a program with no `try`
  allocates nothing on the fault path.

- **Runaway recursion raises `java.lang.StackOverflowError`.** Groovy's
  recursion depth is the JVM's, and running out of it throws a catchable
  `StackOverflowError`. groovyrs raises the same throwable in the same place in
  the hierarchy (`StackOverflowError` → `VirtualMachineError` → `Error` →
  `Throwable`), so `catch (Exception e)` does not swallow it. All four shapes
  are covered — a self-recursive function, a mutual cycle, a closure, and a
  method — because what is bounded is the *one* depth both recursion paths
  share, `vm.frames.len()`, at `host::MAX_CALL_DEPTH` (2000, above the JVM's
  measured 1650 for a self-recursive closure on JDK 21). Two enforcement points:
  `host::run_sub`, which every closure / method / constructor /
  operator-overload re-entry passes through, and a `GDEPTH` check the compiler
  puts in the prologue of each function `compiler::recursive_fns` finds in a
  call-graph cycle. A function that cannot reach itself gets no check, which is
  what keeps a hot loop calling one trace-eligible — a `CallBuiltin` in a
  recorded region aborts the trace.

  Before this the two paths failed differently and neither was catchable: host
  re-entry overflowed the Rust stack (`fatal runtime error: stack overflow`,
  SIGABRT, at 63 nested levels on the main thread's 8 MiB), and native
  `Op::Call` recursion grew `vm.frames` on the heap at ~250 MB/s until the
  process was killed. The binary now runs on a thread with
  `groovyrs::INTERPRETER_STACK_BYTES` (512 MiB) of stack, which is what makes a
  bound of 2000 servable at the measured ~133 KB of Rust stack per host
  re-entry in a debug build.

- **`switch`.** Groovy's, with its full `isCase` semantics rather than `==`: a
  constant label compares equal (numerically across `Integer`/`BigDecimal`), a
  range or list label *contains* the subject, a bare type name
  (`case String:`, `case MyClass:`, `case IOException:`) is an `instanceof`, a
  `~/…/` pattern label matches the subject's string form *entirely*
  (`Matcher.matches`, not `find`), a closure label is called with the subject and
  read for Groovy truth, and `case null:` matches only `null`. Sections keep
  source order and fall through until a `break`; `default` may sit anywhere and
  is entered only when no label matched. The subject is evaluated once and the
  labels only until one matches. A `switch` is a `break` target but not a
  `continue` target — a `continue` inside one continues the enclosing loop.
- **Regex: `~/…/`, `/…/`, `=~`, `==~`, `Matcher`.** `~/pattern/` is a
  `java.util.regex.Pattern` (it prints as its source and drives a `case` label);
  `/pattern/` is a slashy `String`, whose backslashes are literal and which may
  interpolate and span lines; `s =~ p` is a `java.util.regex.Matcher` and
  `s ==~ p` a whole-input `Boolean`. The `Matcher` is stateful as Java's is —
  `find()` moves its cursor, `group(n)` / `start()` / `end()` read the last
  match, and its own truth is `find()`, so `while (m) { … }` walks — plus
  `matches()`, `groupCount()`, `pattern()`, `reset()`, `size()` / `count`,
  `m[i]`, and iteration over its matches. `String` carries `matches`,
  `replaceAll` / `replaceFirst` (in both the `$n` and closure forms), `findAll`,
  `find`, and a `split` that follows Java's specified rules. The `/`-versus-
  division question is settled positionally: `/` opens a literal wherever an
  expression may begin and divides wherever one has just ended. See
  `src/regex.rs` for which Java *meanings* are reproduced and which constructs
  are refused outright.
- **`do` / `while`.** `do { … } while (cond)` runs its body before the first
  test, so it always executes at least once; `continue` targets the test.
- **Labeled `break` / `continue`.** `outer: for (…) { … break outer }` and
  `continue outer`, on `for`, `while`, `do`/`while`, and `switch`. The label
  binds to the frame it precedes; `break label` naming no enclosing frame is a
  compile error rather than a silent no-op.

- **`assert`, with Groovy's power-assert rendering.** A failing bare `assert`
  raises `org.codehaus.groovy.runtime.powerassert.PowerAssertionError` whose
  message is the statement's own source text followed by every sub-expression's
  value placed under the column it came from, `|` markers filling the lines
  between — a port of Groovy's `AssertionRenderer` layout, including the rule
  that a value too wide to share a line moves down one and leaves a marker
  behind. Values render *verbose* (a `String` is quoted, a map's keys too), which
  is not how `println` renders them. Recorded shapes: variables, binary
  operators (under the operator's column), unary `!`/`-`, `instanceof`, method
  calls and property reads (under the member name's column), subscripts (under
  the `[`), and calls. The `assert cond : message` form instead raises a plain
  `java.lang.AssertionError` reading `<message>. Expression: <canonical text>`,
  plus a `Values:` clause naming the condition's bare-variable operands — where
  the canonical text is Groovy's `Expression.getText()` (fully parenthesised,
  implicit `this` on a bare call, qualified type names), *not* the source. The
  values in that clause are read at failure time, so a variable `&&` short-
  circuited past still reports.

## Aborts a Groovy program survives — open

Round 7 removed the recursion aborts (see the `StackOverflowError` entry above)
and the deep-nesting one. A differential sweep of ~33,700 degenerate programs
then found these, which are **still open**. Each line below was run on both
sides: Apache Groovy 5.0.8 / JVM 21.0.12, and the groovyrs build at
`294aa8b4e9`. A panic or an abort is a parity divergence even where the happy
path agrees, because no `catch` can see one.

**Integer overflow in a host builtin panics** (debug-build overflow checks; a
release build would wrap silently, which is the worse of the two). All seven
sites are `src/host.rs`:

| program | Groovy | groovyrs |
|---|---|---|
| `println Long.MIN_VALUE / -1` | `9223372036854775808` | rc=101, `attempt to calculate the remainder with overflow` |
| `println([Long.MAX_VALUE, 1].sum())` | `-9223372036854775808` | rc=101, `attempt to add with overflow` |
| `Long.MAX_VALUE.upto(Long.MAX_VALUE) { println it }` | prints, then loops | rc=101, `attempt to add with overflow` |
| `Long.MIN_VALUE.times { }` | prints nothing, exits 0 | rc=101, `attempt to subtract with overflow` |
| `println "abc".getAt(9223372036854775807)` | `MissingMethodException` | rc=101, `attempt to add with overflow` |
| `println([1,2].withIndex(9223372036854775807))` | `MissingMethodException` | rc=101, `attempt to add with overflow` |
| `println([1,2].indexed(9223372036854775807))` | `MissingMethodException` | rc=101, `attempt to add with overflow` |

**An unbounded length reaches the allocator.** `println "abc".multiply(Long.MAX_VALUE)`
is `IllegalArgumentException: multiply() should be called with a number ≥ 0` in
Groovy and `capacity overflow` (rc=101) here; `def l=[1,2]; l[Long.MAX_VALUE]=1`
is accepted by Groovy and is `capacity overflow` here; `"abc".padLeft(Long.MAX_VALUE)`
answers `abc` in Groovy and aborts here with
`memory allocation of 9223372036854775804 bytes failed` (rc=134). `padRight`,
`center`, `[1]*2147483647`, `String.format("%2147483647d", 1)` and
`1.0.round(2147483647)` are the same shape and hang rather than abort.

**A self-referential collection has no cycle detection.** Groovy renders the
back-edge as `[(this Collection)]` / `[k:(this Map)]`; every groovyrs path that
walks the elements recurses until the Rust stack is gone:

| program | Groovy | groovyrs |
|---|---|---|
| `def a=[]; a<<a; println a` | `[(this Collection)]` | rc=134, stack overflow |
| `def m=[:]; m.k=m; println m` | `[k:(this Map)]` | rc=134, stack overflow |
| `def a=[]; a<<a; println (a==a)` | `true` | rc=134, stack overflow |
| `def a=[]; a<<a; println a.hashCode()` | `StackOverflowError` (catchable) | rc=134, stack overflow |
| `def a=[]; def b=[a]; a<<b; println a` | `StackOverflowError` (catchable) | rc=134, stack overflow |

37 reproducers dedup to this one cause, across four paths: rendering
(`toString`, `join`, `inspect`, `dump`, a GString), hashing (`hashCode`,
`toSet`, a list used as a map key), equality (`==`, `equals`, `contains`,
`indexOf`), and structural walks (`flatten`, `clone`, `reverse`, `sort`,
`unique`, `sum`, `max`, `groupBy`, `find`). Note the two rows where Groovy
raises: `MAX_CALL_DEPTH` does not bound this one, because the recursion is a
plain Rust function walking the heap rather than a VM frame — it needs its own
visited-set, which is also what produces `(this Collection)`.

**Non-terminating where Groovy terminates.** `class A extends A {}` and a mutual
`extends` cycle hang in class resolution (Groovy: a compile error). A `for (x in
1..1000000000) { break }` materialises the range before the first iteration, so
the `break` is never reached. A `BigDecimal` with an extreme scale
(`new BigDecimal("1E+2147483647") + 1`) expands to its full digit string. A
40,000-entry map literal takes quadratic time to parse (10k entries 2.4 s, 20k
8.3 s, 40k past 20 s).

**One semantic divergence found in the same sweep.** `class A { def x; def
getX() { x } }; println new A().x` prints `null` in Groovy — a bare field name
inside the declaring class reads the field and must not route back through the
getter — where groovyrs recurses and now raises `StackOverflowError`.

**Not a divergence**, recorded so a later sweep does not re-report them:
`Long.MAX_VALUE.times {}` and `(1..Long.MAX_VALUE).each {}` loop forever in
Groovy too, and `while (true) { try { return 1 } finally { continue } }` is an
infinite loop on both sides.

## Not implemented (errors today)

- **`trait`.** `class`, `interface`, `extends` (single inheritance for a class,
  multiple for an interface), `implements`, and interface `default` methods are
  supported; `trait` is not, and a `trait` declaration is a parse error.
- **A *script-declared* class name is not a value.** `Foo.class` and `Foo.name`
  for a class the script declares do not resolve — such a name is only meaningful
  in `new`, `instanceof`, a `catch` clause, and a `switch` `case` label;
  `getClass()` on a *value* is what answers. The **JDK** classes a script names
  statically (`Math`, `Integer`, `Long`, `Double`, `Float`, `Short`, `Byte`,
  `Boolean`, `Character`, `String`, `System`, `BigDecimal`, …) *do* resolve to a
  `java.lang.Class`, so `Math.max(1, 2)`, `Integer.parseInt("42")` and
  `Integer.MAX_VALUE` work. Each of them also resolves **package-qualified**
  (`java.lang.Math.max(3, 4)`, `java.util.Arrays.asList(1, 2)`), and so does
  `java.math.RoundingMode`, which Groovy does *not* default-import — a bare
  `RoundingMode` raises `MissingPropertyException` on both sides. A binding of
  the same name shadows the package: `def java = [math: 5]; java.math` is the
  map read.

  A `RoundingMode` constant is modeled as its own **name**, a `String`. That is
  what an enum constant prints, what `setScale`/`divide` read back, and what
  comparing two of them answers; only `getClass()` tells the difference (it
  names `java.lang.String` where Groovy names `java.math.RoundingMode`, and a
  `MissingMethodException` that lists one among its argument types says
  `String`). Modeling the enum properly means a heap object with an identity,
  which nothing else in the frontend needs yet.
- **An array answers the whole `List` GDK, where Groovy's answers only part of
  it.** Arrays are modeled as a list *kind*, so `([3,1] as int[]).sort()` and
  `([1,2] as int[]) + [3]` work here; real Groovy raises
  `MissingMethodException` for both, because a Java array is not a `Collection`
  and only the GDK methods defined on arrays apply. This is deliberate
  permissiveness of the same sort as `println [1,2].toString()` — every program
  that works in Groovy works here, and tightening it would reject working
  programs to gain nothing but a matching error.
- **`new Random(…)` / `new Date(…)` and the other stateful JDK classes.**
  Reproducing them means reproducing Java's exact LCG and clock, so they fault
  rather than answering a plausible-looking different number. The instantiable
  classes that *are* modeled: `StringBuilder`, `StringBuffer`, `StringWriter`,
  `ArrayList`/`LinkedList`/`Vector`, `HashSet`/`LinkedHashSet`/`TreeSet`,
  `HashMap`/`LinkedHashMap`/`TreeMap`, `Object`, the box types, `BigDecimal`,
  `BigInteger`, and every modeled throwable. The three `Map` names build the
  implementation they name — a `TreeMap` sorts, a `HashMap` buckets — as the
  three `Set` names do. The three `List` names all build the one implementation:
  see **A `List` is always an `ArrayList`** below.
- **`java.util.UUID`.** `UUID.fromString` and `UUID.randomUUID` are not
  modeled: a UUID is its own type (`getClass()` names `java.util.UUID`, and it
  compares and hashes as 128 bits, not as its text), and answering the string
  instead would get all three wrong. The class *name* resolves, so the call
  raises `MissingMethodException` rather than failing to compile.
- **`Closure.parameterTypes`.** A closure's declared parameter *types* are not
  kept — `ClosureMeta` carries the count and nothing else — so the list Groovy
  answers (`[class java.lang.Object]` for an untyped parameter, the declared
  class for a typed one) cannot be built. `maximumNumberOfParameters` is
  answered, since the count is what is kept.
- **`Float.MAX_VALUE` / `Float.MIN_VALUE`.** groovyrs has no `java.lang.Float`,
  so a 32-bit constant could only be answered as the `Double` nearest it —
  `3.4028234663852886E38` where Groovy prints `3.4028235E38`. Answering the wrong
  number is worse than not answering, so the read raises. Every `Double`,
  `Integer`, `Long`, `Short`, `Byte` and `Math` constant *is* answered, including
  `MIN_NORMAL`, `MAX_EXPONENT`/`MIN_EXPONENT` and `SIZE`/`BYTES`.
- **A `GString` is a `String`.** An interpolated literal produces a plain
  `java.lang.String`, so `"$s".getClass()` reports `java.lang.String` where
  Groovy reports `org.codehaus.groovy.runtime.GStringImpl`.
- **A closure's `getClass()` names `groovy.lang.Closure`.** Groovy reports the
  synthetic per-closure class (`Script1$_run_closure1`), whose name depends on
  the enclosing script's name and the closure's position; groovyrs has no such
  class to name.
- **Method overloading, by arity as well as by parameter type.** A class's
  methods (and its operator methods) are keyed by *name alone* — `ClassMeta`
  holds a `HashMap<String, u16>`, and the synthetic subroutine each method
  compiles to is named `$cls_<class>_m_<name>` with no arity in it — so every
  same-named declaration collapses onto one entry and the **first** declared
  body answers every call. `class C { def g() { "zero" }; def g(a) { "one" };
  def g(a, b) { "two" } }` answers `[zero, zero, zero]` where Groovy answers
  `[zero, one, two]`, and the extra arguments are silently discarded rather
  than raising. Top-level script functions collapse the same way (`def f() {
  "f0" }; def f(a) { "f1" }` makes `f(1)` answer `f0`). Constructors are the
  exception and already work: they are keyed by *arity* (`HashMap<u8, u16>`,
  `$cls_<class>_ctor_<arity>`), which is the shape the method table needs.
- **A `BigInteger` argument where the JDK overload wants an `int`.**
  `255.toString(16G)` answers `16` in Groovy — the `BigInteger` coerces to the
  `int` the static `Integer.toString(int)` wants — where groovyrs raises
  `MissingMethodException`. The `Long` spelling of the same call is modeled (see
  `GMETHOD_WIDE`); only the `BigInteger` coercion is missing.
- **`++`/`--` do not call `next`/`previous`.** To keep the JIT fast path for
  integer loop counters (`for (i=0; i<n; i++)`), `++`/`--` lower to native
  `+ 1` / `- 1` rather than routing through a builtin (which would abort trace
  JIT). On a user-class instance they therefore dispatch `plus`/`minus`, not
  Groovy's `next`/`previous`. Call `x.next()` / `x.previous()` explicitly for
  those.
- **`import`/`package`** are tolerated (skipped) but do nothing.
- **Command-argument chains beyond one arg** (`println a, b`, `foo bar baz`).

## Modeled with a documented simplification

- **Recursion depth is a fixed 2000 frames, not the JVM's stack.** Groovy's
  limit is whatever `-Xss` leaves and so varies by JVM, thread and frame size;
  groovyrs's is the constant `host::MAX_CALL_DEPTH`. It sits above the reference's
  measured depth (1650 frames for a self-recursive closure on Apache Groovy
  5.0.8 / JVM 21.0.12), so a recursion the reference completes completes here —
  but the *depth at which* the two raise differs, and a program that prints a
  counter before overflowing prints a different number. The counter is also
  frames, not calls: a recursion that goes through the host (a closure, a
  method) spends two per level where a plain function call spends one.

- **Source nested deeper than 5000 is refused.** The parser is recursive
  descent, the compiler walks the AST recursively, and the tree recurses again
  when it drops, so nesting in the source is Rust stack three times over.
  `parser::MAX_NESTING` bounds it and the program is the compile error
  `groovyrs: expression nesting is deeper than 5000 on line N` — where before it
  was `fatal runtime error: stack overflow`, an abort with no diagnostic.
  Measured against Apache Groovy 5.0.8 / JVM 21.0.12, the reference's own parser
  gives out first (500 nested parentheses compile and 1000 do not; a 1000-term
  `+` chain compiles and a 2000-term one does not), so nothing the reference
  accepts is refused here. The budget counts three things against one limit:
  recursive-descent depth, statement-block depth, and the length of a
  left-folded operator chain, which is AST depth by another spelling.

- **`getSuppressed()` and `getArguments()` answer a `List`, not an array.** Both
  are arrays in Java (`Throwable[]`, `Object[]`), and groovyrs has no array
  type — the same absence recorded for `args` and `String.split` below. They
  print and iterate identically and answer `size()`; what differs is `.length`,
  which raises `MissingPropertyException`, and `getClass()`, which reports
  `java.util.ArrayList` rather than `[Ljava.lang.Throwable;`.

- **`initCause` may be called twice.** The JDK refuses the second call with
  `IllegalStateException: Can't overwrite cause with …`; groovyrs takes it. The
  first call, which is the one programs make, agrees — including its answering
  the receiver so `e.initCause(c)` chains.

- **A qualified type name from a package groovyrs does not model is accepted and
  never matches.** `t instanceof com.example.IOException` is `false` here, where
  Groovy fails the compile with `unable to resolve class`. The `catch` reading is
  the same either way (the arm does not fire); what differs is that Groovy
  refuses the program and groovyrs runs it.

- **`throw 5` — a non-`Throwable` — is a groovyrs fault, not Groovy's
  `VerifyError`.** Groovy compiles the `throw` and the *JVM verifier* rejects the
  resulting bytecode at class load, so the program dies with
  `java.lang.VerifyError`. That throwable is an artifact of JVM bytecode
  verification with nothing to correspond to here; groovyrs reports
  `groovyrs: Caught: 5` and exits non-zero. Both refuse the program; only the
  diagnostic differs.

- **A labeled `break`/`continue` that leaves the loop containing a `try` runs
  its `finally`; Groovy 5.0.7 does not.** groovyrs follows the JLS here. Repro:
  `N: for (i in 0..1) { for (j in 0..2) { try { if (j == 1) continue N;
  println("d"+i+j) } finally { println("df"+i+j) } } }` prints the `df?1`
  cleanup lines under groovyrs and omits them under `groovy`. An *unlabeled*
  `break`/`continue`, and a labeled one naming the loop the `try` is directly
  inside, run the `finally` in both.
- **A closure's default parameter is a null guard.** `{ a, b = 5 -> … }` lowers
  to `if (b == null) b = 5` at the top of the body, where Groovy generates one
  overload per arity. The two agree for every call that *omits* the argument;
  they differ when a caller passes an explicit `null`, which groovyrs replaces
  with the default and Groovy keeps as `null`.
- **A `null` *right* operand of arithmetic on a number.** With `x` null, Groovy
  raises `groovy.lang.GroovyRuntimeException` ("Ambiguous method overloading for
  method java.lang.Integer#plus") for `5 + x`, `5 - x`, `5 * x` and `5 / x`, and
  a `NullPointerException` for `5 % x`. groovyrs concatenates the `+` (`5 + x`
  answers the string `5null`, and so does `z += x`) and hard-faults the rest
  (`groovyrs: operator \`Sub\` is not defined for operands \`5\` and \`null\``).
  A null *left* operand is the modeled direction and agrees with Groovy.
- **`println(<null arithmetic>)` prints a line before it raises.** `x` null,
  `try { println(x - 1) }` prints `null` and *then* raises the
  `NullPointerException`, where Groovy raises first and prints nothing. The
  exception, its class and the exit status are right; the extra line is not.
  Every other position tested raises before printing anything — as a discarded
  statement (`x - 1`), in a declaration (`def q = x - 1`), in an `if`/`while`
  condition, and with a method call on the result (`println((x - 1).toString())`)
  — because those are the places the compiler now checks for a pending throw.
  Only the value handed straight to `println` runs the print first. Same shape
  and cause as the `ConcurrentModificationException` entry below.
- **A power assert does not record inside a `GString`.** A placeholder is lexed
  on its own, so its columns are relative to the placeholder rather than the
  script and recording it would put values under the wrong column. `assert
  "v=${x}" == "no"` therefore shows the `==` result but not `x`, where Groovy
  shows both.
- **A handful of `java.util.regex` constructs are refused, not approximated.**
  The engine underneath is `fancy-regex`, chosen because Java's flavour is
  backtracking (backreferences, lookaround) and the linear-time `regex` crate
  rejects those by construction. `src/regex.rs` rewrites every Java *default*
  that would otherwise silently answer a different question — ASCII-only
  `\d`/`\w`/`\s`/`\b`, ASCII-only `(?i)`, `.` excluding all five line
  terminators, `$` matching before a final terminator, `\Q…\E`, `\h`/`\v`/`\R`,
  `\Z`, the POSIX `\p{Alpha}` names — so those all behave as Java's do. What has
  no faithful translation is refused by name at compile time rather than
  approximated: possessive quantifiers (`a*+`), atomic groups (`(?>…)`),
  conditionals, comment groups, `\G`, `\X`, `\cX`, `\N{…}`, octal escapes,
  Unicode *blocks* (`\p{InGreek}`), `\p{javaLowerCase}`-style `Character`
  predicates, and the `(?m)`/`(?x)`/`(?d)`/`(?u)`/`(?U)` flags. Each raises
  `java.util.regex.PatternSyntaxException` naming the construct.
- **`$/…/$` dollar-slashy strings.** The `/…/` slashy form is implemented (and
  interpolates, and spans lines); the `$/…/$` form, whose only difference is
  that `/` needs no escape and `$$` escapes a dollar, is not lexed.
- **A `MissingMethodException` message omits Groovy's `Possible solutions:`
  line.** Groovy appends a fuzzy suggestion list built from the receiver's real
  JDK/GDK method table (`Possible solutions: grep(), next(), size(), …`), which
  groovyrs has no table to build. Everything before it — `No signature of method:
  <name> for class: <qualified class> is applicable for argument types:
  (<simple types>) values: [<values>]` — is byte-identical to Groovy. The same
  holds for the suggestion line Groovy sometimes appends to a
  `MissingPropertyException` on a class with fields.
- **A `ConcurrentModificationException` raised by a native operator is not
  caught where Groovy catches it.** Reading an invalidated `subList` window
  raises, and every path that consumes a list's elements is a builtin call whose
  post-call check unwinds immediately — except `==`, `+` and the other operators,
  which fusevm answers natively and whose post-op pending check the compiler
  emits only under the gate described in *An exception thrown out of an
  operator-overload method*. Outside that gate the throwable is parked and the
  program runs one more statement before a check finds it, so
  `try { println(s == [2, 3]) }` prints `true` and *then* raises, where Groovy
  raises first and prints nothing. The exception, its class and the exit status
  are right; the extra line is not. This is the same shape as
  *`println(<null arithmetic>)` prints a line before it raises* above, and has
  the same cause — `try { println(null - 1) }` prints `null` first too. A program
  with **no** `try` in it is exact either way: the raise degrades to a hard fault,
  which halts before the next statement.
- **A reverse `ObjectRange`'s `subList` differs**, in the one corner where
  Groovy's own answer is self-inconsistent: `('e'..'a').subList(1, 3)` *prints*
  `c..d` but *iterates* `[c]`, because Groovy builds it through the constructor
  that neither normalises its endpoints nor re-derives them. groovyrs answers
  `c..b`, which prints and iterates consistently. Every numeric range and every
  forward `ObjectRange` is exact, including the `EmptyRange` and the
  count-down indexing quirk (`(5..1)[1]` is `4` while `(5..1).subList(1, 2)` is
  `2..2`).
- **A bare name written inside `with`/`tap` is not readable again afterwards.**
  The *name* forms now reach the delegate the way a bare call always did:
  `[a: 1].with { a }` answers `1`, `m.with { a = 9 }` writes into `m`,
  `m.with { b = 7 }` adds the key, and `+=` / `++` / `--`, a subscript write and
  a mutating method's write-back all go back through the delegate. Groovy's
  `OWNER_FIRST` is preserved: a script binding of the same name still wins, and
  a delegate that can hold neither a key nor a field takes no write and raises
  nothing (`[1, 2].with { zork = 1 }` is accepted, as Groovy accepts it).
  Reading the name *after* the block raises `MissingPropertyException` in both
  now — nothing at script level ever bound it — which is the general behaviour
  of an unbound bare name, not something `with` introduces.
  One difference is positional. Groovy scopes a script variable from its
  declaration onward, while the compiler collects the script's declared names in
  one pass over the whole file, so a delegate key that a *later* line also
  declares as a script variable (`m.with { a }` above a later `def a = 5`)
  resolves to the script binding rather than the delegate.
- **A property read on a list is not a spread read.** Groovy's `list.prop` maps
  the read over the elements, so `[1,2,3].zork` reports the *element* type
  (`No such property: zork for class: java.lang.Integer`, wrapped in an
  `Exception evaluating property` message); groovyrs reports the list itself.
  The explicit spread `list*.prop` **is** modeled.
- **`String.toBigDecimal` accepts only the ASCII grammar.** `java.math`'s parser
  additionally admits Unicode decimal digits (`Character.digit`); groovyrs
  rejects those as ordinary unexpected characters, with the same
  `Character … is neither a decimal digit number …` message Groovy gives for a
  genuinely invalid character.
- **The `NullPointerException` for a property *write* on `null` carries the
  JDK's helpful-NPE text** (`Cannot invoke "Object.getClass()" because "obj" is
  null`) rather than a groovyrs-authored message, since that is what Groovy
  surfaces. The wording is the JDK's, so it can change with the JVM version.

- **`%` with a non-literal divisor costs its loop's trace eligibility.** Java's
  `%` throws `ArithmeticException` on a zero divisor where fusevm's native
  `Op::Mod` answers `0` — a silent wrong answer. The compiler therefore guards
  the op with `Dup; LoadInt(0); NumEq; JumpIfFalse` and calls the `GMOD` builtin
  only on the zero branch. The guard itself is four native ops, but the
  never-taken `CallBuiltin` sits *inside* the loop body, and fusevm's trace
  eligibility is a static scan of the region's opcodes, so the loop is
  disqualified. Measured with `--tiers` on
  `def d = 7; def t = 0; for (i in 0..500000) { t += i % d }; println(t)`:
  `loop @10 trace-eligible=false`; the same program with the literal `7` in
  place of `d` reports `loop @8 trace-eligible=true`.
  A **literal non-zero divisor** (`i % 2`, `n % -3`) is proved safe at compile
  time and emits nothing at all, so every constant-divisor loop is byte-identical
  to before — and that is the shape hot loops actually take. Moving the zero
  branch out of line (past the loop, like a deferred block) would recover
  eligibility for the variable-divisor case; it needs a deferred-emission path
  the compiler does not have yet.
- **A condition whose type is not statically known costs one builtin call.**
  Groovy truthiness is exact everywhere, but where the compiler cannot prove the
  condition is already a number/boolean/list (`while (x)`, `if (m)`), it emits a
  host call that aborts a JIT trace through that condition. Comparison-shaped
  guards — the loop conditions that matter — are unaffected. This is the
  deliberate trade: correctness where the type is unknown, the old native path
  where it is known.
- **An exception thrown out of an operator-overload method needs a class in the
  program.** A user instance operand routes a native arithmetic op through the
  strict numeric hook, which re-enters the VM. groovyrs emits the post-call check
  after arithmetic only when the program *both* uses exceptions and declares a
  class; that gate keeps arithmetic native everywhere else, and it is exactly the
  condition under which the re-entry can happen.
- **A `finally` body is emitted once per exit path.** Fall-through, each handler,
  the rethrow, and every early `return`/`break`/`continue` each get their own
  copy of the block rather than sharing one through a subroutine — fusevm's
  frames are for calls, not local jumps. This is what `javac` does; it costs code
  size, not semantics.
- **`{ -> … }` does not reject extra arguments.** An explicit empty parameter
  list is honoured (`def z = { -> 7 }; z()` works), but a call that passes
  arguments anyway drops them instead of failing the way Groovy's arity check
  does.
- **Implicit return does not reach through a trailing loop or assignment.** It
  reaches through a trailing expression, `if`, and `try`; a body ending in a
  `for`/`while` or a bare assignment still returns `null`.
- **A captured local's cell is reclaimed only when nothing captured it.** A
  boxed binding's cell is reused in place when the target's current one has not
  been captured, so a loop that builds a closure only sometimes costs one cell
  rather than one per iteration (400 000 iterations that create a single
  escaping closure: 59 MB before, 11 MB after). A loop where every iteration's
  closure *does* escape needs every cell and keeps them all, because the host
  heap has no collector — 400 000 escaping closures hold 176 MB for the length
  of the run. That is the same absence as the entry below, not a cell-specific
  one.
- **Every decimal operation allocates a heap slot that is never reclaimed.**
  Decimal literals are interned (a literal inside a loop allocates once), but each
  arithmetic *result* takes a new slot in the host heap, which has no collector.
  A script doing millions of decimal operations in a loop grows memory for the
  length of the run; integer and `double` arithmetic are unaffected (they never
  touch the heap).
- **`float`/`Float` is modeled as a `double`.** An `f`-suffixed literal parses to
  an `f64`, so it prints and computes with `double` precision: `0.1f + 0.2f` is
  `0.30000000000000004` where Groovy's `Float` prints `0.30000000447034836`. The
  same absence makes `floatValue()` a no-op: `(16777217).floatValue()` keeps
  every digit and prints `1.6777217E7` where Java rounds through `f32` and
  prints `1.6777216E7`.
- **The transcendental `Math` functions can differ in the last bit.**
  `Math.sin`, `cos`, `tan`, `exp`, `log`, `log10` and `cbrt` call the platform's
  libm; the JVM's are fdlibm-derived (and intrinsified). Java specifies them
  only to within 1 ulp, so both are conforming and they disagree on some inputs:
  measured against Apache Groovy 5.0.8 on JVM 21.0.12, `Math.sin(1.5)` is
  `0.9974949866040543` there and `…44` here, `Math.exp(1.0)` is
  `2.7182818284590455` there and `…45` here. `sqrt`, `hypot`, `atan2`, `pow`,
  `rint`, `floor`, `ceil`, `signum`, `toRadians`, `toDegrees`, `ulp`, `nextUp`,
  `nextDown`, `getExponent` and `IEEEremainder` were all measured exact over the
  same sweep. `asin`, `acos`, `atan`, `sinh`, `cosh`, `tanh`, `log1p` and
  `expm1` are each one method call away and are deliberately **not** modeled:
  each was measured off by an ulp, and answering within an ulp is a wrong
  answer, so they raise `MissingMethodException` instead. Closing this needs a
  port of the JDK's fdlibm, not a different libm call.
- **`Double.NaN == Double.NaN` is `false`; Groovy answers `true`.** Groovy's `==`
  on two `Number`s goes through `Double.compare`-style equality, so a NaN equals
  itself (while `<`, `>`, `<=`, `>=` against a NaN are all `false`, as they are
  here). Both operands are numeric, so fusevm answers `==` natively and the
  strict numeric hook — which only sees non-numeric operands — never gets the
  chance.

  **This is not a fusevm limitation, and the earlier note that called it one was
  wrong.** The host-side comparator already gets NaN right: `values_equal`
  reaches `decimal_operator`, and `[Double.NaN].contains(Double.NaN)` and
  `[Double.NaN] == [Double.NaN]` both answer `true` today. Every path that runs
  through the host is already correct; the only wrong answer comes from the
  *compiler* choosing fusevm's native `Op::NumEq` for a bare `==`. Nothing about
  fixing it requires a fusevm change — it requires the compiler to route `==` to
  a builtin when an operand may be a `Double`, exactly as `BinOp::Shr` already
  routes to `GSHR` when `shr_receiver_is_object` says the receiver is not a
  number.

  What blocks it is that `Compiler` has no static float typing. It tracks
  `Long`-ness (`is_wide`/`wide_vars`/`pinned_wide`) and object-ness
  (`obj_vars`), and a `may_be_double` analysis would be the third of that same
  shape — `d`/`f`-suffixed literals, `Double.*` static reads, `Math.*` results,
  `as double`, `double`-pinned declarations, and arithmetic over those. The cost
  is also narrower than previously recorded: only `Double`-typed comparisons
  would lose the native op, not "every `==` in the language" — an `Integer`,
  `String` or `BigDecimal` comparison can never be a NaN and keeps `NumEq`, so
  loop counters are untouched. Sized as a compiler dataflow addition, not a
  VM change; still open, but open for a different and smaller reason than
  recorded before.

  `<=>`, `sort`, `max` and `min` all reproduce Groovy's NaN ordering
  (`[1.0d, Double.NaN, 0.5d].sort()` is `[0.5, 1.0, NaN]`, and `max`/`min` both
  answer `NaN`, which is Groovy's own inconsistency — its `sort` uses
  `Double.compare` and its `max`/`min` scan with the primitive `>`/`<`).
- **`Math.abs(-2147483648L)` answers `-2147483648`; Java answers `2147483648`.**
  `Math.abs` is overloaded on width and `Integer.MIN_VALUE` is the one value
  whose `int` and `long` answers differ. The compiler's call-site width mask
  (`GMETHOD_WIDE`) is emitted for *instance* calls only, so a static call
  arrives with both widths erased and the value-based rule picks the `int`
  overload. Same family as the `Long`-versus-`Integer` entry below.
- **`charAt` and `toCharArray` cannot answer half a surrogate pair.** Every
  string index is a UTF-16 code unit, matching Java, but there is no
  `java.lang.Character` type and no Rust `char` can hold a lone surrogate, so
  `"a😀b".charAt(1)` answers the replacement character where Java answers the
  high surrogate `0xD83D`. `toCharArray()`/`chars()` answer a list of
  one-character strings rather than a `char[]` of code units, so their length is
  the code-point count.

  The same missing type makes `%c` more permissive than the JDK's. Java's
  `Formatter` takes a `Character` or an `int` code point and refuses a `String`
  outright; groovyrs takes a *one-character* `String` (which is what `'a' as
  char` evaluates to here) so that `sprintf("%c", 'a' as char)` still prints
  `a`. A longer `String` raises `IllegalFormatConversionException` as Java
  does, which is where the real mistake shows up.
- **`%a` ignores a precision.** `sprintf("%a", 1.5d)` is `0x1.8p0`, matching
  `Double.toHexString`, but `%.3a` prints the same rather than rounding the hex
  mantissa to three digits. The rounding is defined on the *hex* significand, a
  base the value model has no arithmetic for; every other float conversion
  (`%f`, `%e`, `%g`) honours its precision exactly.
- **A compound assignment to an unbound name raises the wrong throwable.**
  `counter += 1` with nothing bound reads `null` and then faults on the
  arithmetic, so it raises `NullPointerException` where Groovy raises
  `MissingPropertyException` from the read. The plain read (`println counter`)
  is right; only the read-modify-write path is not.
- **`this` is not bound at script level.** `this.getClass().getName()` answers
  `org.codehaus.groovy.runtime.NullObject` where Groovy answers the script
  class (`p7` for `p7.groovy`). The script class *name* is modeled — a
  `MissingPropertyException` message quotes it — but there is no `this`
  receiver to hang it on.
- **A `Long` small enough to be an `Integer` is one, except where the compiler
  looked.** Groovy's integer width is a property of the value — `Integer op
  Integer` wraps at 32 bits, anything with a `Long` in it at 64 — and fusevm has
  one integer type. groovyrs recovers the width from two places: the compiler,
  which marks the arithmetic whose operands it can see are `Long` (an `L`
  suffix, a `long`/`Long` declaration, a literal past `Integer` range, and what
  propagates from those), and the operands' magnitudes at run time, since a
  value outside `Integer` range *is* a `Long` whatever produced it. Between them
  every case in the probe corpus is exact, including the `long` accumulator
  (`long t = 0; t += 2000000000; t += 2000000000` is `4000000000`) and
  arithmetic inside a closure (`[1000000, 1000000].inject(1) { a, b -> a * b }`
  is `-727379968`).

  The tracking is scoped and flow-sensitive: a function or closure body cannot
  leak the width of a name it declares to a same-named variable outside it, a
  `def` re-declaration or a plain assignment re-binds the width in *both*
  directions (`def a = 5L; a = 5; a * 1000000000` wraps at 32 bits, as Groovy
  does), an explicit `long`/`Long` declaration pins it against that narrowing
  (`long t = 0; t = 5; t * 2000000000` stays 64-bit), branches merge by union,
  and a call to a callable whose every `return` is statically `Long` carries
  that width to the call site (`def f = { -> 5L }; f() * 1000000000`).

  What is left is a `Long` the compiler could not see *and* whose value fits an
  `Integer`: one stored in a list (`[5L][0] * 1000000000`), passed through a
  closure *parameter* (`{ x -> x * 1000000000 }(5L)`), or narrowed by an
  assignment inside a closure that runs before the use. All three need the width
  to travel with the value at run time, which fusevm's single integer type
  cannot carry. Their arithmetic wraps at 32 bits and `getClass()` answers
  `java.lang.Integer`. Widening the default instead would trade this for the far
  commoner error of never wrapping an `Integer` at all, since Groovy's own
  default for an unsuffixed literal is `Integer`.
- **`(1.5..<4.0).size()` answers 2 while `toList()` walks three values.** The
  inconsistency is Groovy's, and groovyrs reproduces it: `size` divides the span
  (`floor(|to - from|)`, plus one when inclusive) where the walk steps by one and
  stops short of the excluded end. It only shows on a fractional exclusive range;
  every whole-numbered range agrees with its element count.
- **`Object.is()` answers only for a heap-handle receiver.** Reference identity
  needs a reference, so `is()` is defined on the values that have one — a list,
  map, set, range, matcher, buffer, closure or class instance — and raises
  `MissingMethodException` on an `Integer`/`String`/`Boolean` receiver, where
  Groovy answers from the JVM's boxing and interning caches. Those answers are
  the JVM's (`Integer.valueOf` caches -128..127, and literal `String`s are
  interned but computed ones are not), so modeling them means modeling the cache
  boundaries rather than the language.
- **`Map.Entry` is modeled only as far as the GDK needs.** It prints `k=v` and
  answers `key`/`value`/`getKey()`/`getValue()`; `setValue` and the rest of the
  interface are absent, and its key is always the map's `String` key.
- **Types are not checked.** Declared types (`int`, `String`, `def`) are kept
  for diagnostics but do not gate execution — the runtime is dynamically typed on
  the fusevm value model.
- **`==` compares by value.** This matches Groovy (`==` is `.equals`, not
  reference identity) for the string/number/boolean operands modeled here.
  Cross-type comparisons that Groovy would coerce (`"5" == 5 → false`) are not
  yet distinguished — both sides compare by their printed form.
- **A `GString` whose expression is a closure is not deferred.** `"${-> x}"` is
  a *lazy* `GString` in Groovy: the closure is called at render time, so
  `def x = 1; def s = "${-> x}"; x = 2; s.toString()` is `2`. groovyrs renders
  every interpolation where the literal is written, so it has nothing to defer
  to — the same root as the *A `GString` is a `String`* entry above. The literal
  is currently a parse error rather than a wrong answer.
- **A list a GDK method *returns* is not a reference; only a literal is.** A list
  literal is built behind a heap handle, so every claim in the *Lists are
  references* entry above holds for one — including a list reached through a map
  value or as an element of another literal. A GDK method's result is the
  transient array form instead, and the compiler's receiver write-back is what
  makes `a.sort()` show through `a`. Three things follow. A second name for a
  returned list does not alias it: `def r = [1,2].collect { it }; def s = r;
  s << 9` leaves `r` unchanged where Groovy shows `9` through both. `is()` on one
  raises `MissingMethodException` (it needs a handle). And a *nested* list inside
  a returned one cannot be mutated through the outer: `[[1,2],[3,4]].transpose()`
  then `t[0] << 9` is dropped, where Groovy answers `[[1, 3, 9], [2, 4]]` — the
  same for `collate`, `withIndex`, `combinations`, `permutations`,
  `subsequences`, `groupBy` and `split`. Allocating a handle per GDK result would
  fix all three and cost a heap entry per call, on a heap cleared only per run —
  a loop calling `collect` would grow it without bound, so the fix is a heap with
  a reclamation story, not a one-line change at the return site.
- **`[[1, 2], [3, 4]].sum()` concatenates as strings.** Groovy's `sum()` folds
  with `plus`, so a list of lists answers the concatenated list `[1, 2, 3, 4]`;
  groovyrs renders each element and joins, answering the string `[1, 2][3, 4]`.
- **`collectNested` is not implemented** and raises `MissingMethodException`.
- **A name a *closure* assigned `null` reads as unbound.** Reading a name
  nothing binds now raises `MissingPropertyException` the way Groovy does, and
  the test is the runtime one — the global has been written — so a name bound
  only inside a closure body (`[1, 2].each { newVar = it }`) is found by a later
  top-level read. The gap is that a global holds `Undef` both when it was never
  written and when `null` was written to it, so the *one* shape that still
  differs is a closure assigning literally `null` and the script reading the
  name afterwards: `[1].each { z = null }; println z` raises where Groovy prints
  `null`. A script-level `z = null` is unaffected — the compiler sees the
  assignment, so the read keeps its plain global op and never asks.
- **A declared-but-never-executed script variable reads as `null`.** The
  compiler collects the script's declared names in one pass over the whole file,
  so `if (false) { neverRun = 1 }; println neverRun` keeps a plain global read
  and answers `null`, where Groovy raises `MissingPropertyException`. Only a
  name that appears in *no* script-level declaration or assignment gets the
  checked read.
- **`hashCode()` answers Java's contract only for the types groovyrs models.**
  Every specified rule is implemented and byte-verified (`String` over UTF-16
  code units, `Integer`/`Long`, `Double`, `Boolean`, `BigDecimal` including its
  scale, `BigInteger`, `AbstractList`, `AbstractMap`, `AbstractSet`, `Map.Entry`,
  `IntRange`'s Cantor pairing and the other ranges' inherited `AbstractList`
  hash). What differs follows from types groovyrs does not model rather than
  from the hashing: a `Float` literal is a `Double` here, so `(1.0f).hashCode()`
  answers `Double`'s; a `GString` is a `String`; a map key is always a `String`,
  so `[(1): 'x'].hashCode()` hashes `"1"` rather than `1`; and a `Long` small
  enough to be an `Integer` is indistinguishable from one, so `(-1L).hashCode()`
  answers -1 where Java's `Long` folds the halves to 0. A value with no
  specified contract — a closure, a `StringBuilder`, a `Pattern`, a user
  instance that declares no `hashCode` — gets the object's heap handle as its
  identity hash: stable within a run and equal exactly when the references are,
  which is the contract, but not the number a JVM prints. A JVM's own identity
  hash varies run to run, so no value could match it.
- **`args` is a `List`, not a `String[]`.** Every script's binding carries
  `args` — the launcher arguments after the script file, empty when there are
  none — but as the `List` groovyrs models rather than the array Groovy binds.
  So `args.getClass()` reports `java.util.ArrayList` instead of
  `[Ljava.lang.String;`, and an out-of-range `args[0]` answers `null` where
  Groovy raises `ArrayIndexOutOfBoundsException`. Same root as the *Java arrays*
  entry above.
- **The paren-less `println <expr>` command form is more permissive** than
  Groovy's command-expression grammar. groovyrs parses the whole following
  expression as the single argument, so `println -42` prints `-42`. Real Groovy
  reads `println - 42` as a binary `minus` on the `println` method value and
  throws. Wrap the argument — `println(-42)` — for exact parity; the parenthesised
  form is unambiguous on both. (The differential fuzzer only ever emits the
  parenthesised form, so it never reports this.)
- **A `Set` whose elements are not all plain `Integer`s or all plain `String`s
  still scans.** Membership is decided by `values_equal`, which can re-enter the
  VM for a user class's own `equals` and equates values across types, so no hash
  of a value is consistent with it. `SetIndex` is therefore an accelerator, not
  the answer: it exists only while every element fits one of those two key
  kinds — where `values_equal` *is* key equality — a hit is a candidate the real
  `equals` still confirms, and anything else falls back to the `O(n)` scan.
  Mixing a kind in turns the index off for that set's lifetime. `add`/`contains`
  on an indexed set are `O(1)`; on any other they are what they always were.
- **A `List` is always an `ArrayList`.** `HeapObj::ListVal` carries no
  implementation kind the way `HeapObj::SetVal` carries [`SetKind`] and
  `HeapObj::OrderedMap` carries [`MapKind`], so every list names one class
  whatever built it. `new LinkedList([1,2]).getClass()` and
  `new Vector([1,2]).getClass()` both report `java.util.ArrayList`, and
  `Arrays.asList(1,2,3)` reports it too where Groovy reports
  `java.util.Arrays$ArrayList`.

  The `Map` side no longer has this gap — `HeapObj::OrderedMap` carries a
  [`MapKind`], so a `TreeMap` sorts and a `HashMap` buckets — and neither does
  the `Set` side. What remains of it for maps is the three entries below.
- **A `TreeMap` orders non-`String` keys as their rendered text.** A map key is
  stored as `groovy_str` of the key, so the key's *type* is gone by the time
  `MapKind::Tree` sorts. That is exactly `String.compareTo` and so is right for
  every `String`-keyed `TreeMap`, but a numeric-keyed one sorts lexically:
  `new TreeMap([10:'a', 9:'b', 100:'c'])` is `[9:b, 10:a, 100:c]` in Groovy and
  `[10:a, 100:c, 9:b]` here. Fixing it means carrying the key `Value` alongside
  its rendered form through every map construction site, not a change to the
  ordering itself. The same stringification is why `[1:'a']` and `['1':'a']` are
  one map here and two in Groovy.
- **A map's `keySet()`/`entrySet()`/`values()` answer a plain `List`, and a
  `TreeMap`'s range views a plain map.** The *contents* and their order are
  right; the view type is not modeled, so `getClass()` names
  `java.util.ArrayList` where Groovy names `java.util.TreeMap$KeySet`,
  `$EntrySet` or `$Values`, and `headMap`/`tailMap`/`subMap`/`descendingMap`
  name a `java.util.TreeMap` (or, for the descending one, a `LinkedHashMap`)
  where Groovy names `TreeMap$AscendingSubMap`/`$DescendingSubMap`. A real view
  would also be *live* — a write through it reaching the backing map — which is
  the behaviour half of the gap and needs the same machinery
  [`HeapObj::SubList`] has on the list side. `Map.Entry` has the same shape of
  gap: it carries no class, so `getClass()` on one names `java.lang.Object`
  rather than `java.util.LinkedHashMap$Entry`, `TreeMap$Entry`, or the
  `AbstractMap$SimpleImmutableEntry` that `firstEntry`/`pollFirstEntry` answer.
- **`asImmutable()` / `asSynchronized()` / `Collections.unmodifiableList` answer
  a plain copy.** All three answer the same *elements*, so a read through the
  result is right, but the wrapper type is not modeled: `getClass()` names
  `java.util.ArrayList` rather than
  `java.util.Collections$UnmodifiableRandomAccessList` or
  `$SynchronizedRandomAccessList`, and — the observable one — a
  mutation through the result **succeeds** where Groovy raises
  `UnsupportedOperationException`. Same root as the entry above: a list has no
  kind to carry the immutability in. Maps now have a kind but still no
  *wrapper* kind, so the same holds there — `asImmutable()` on a `TreeMap`
  answers a mutable `java.util.TreeMap` rather than
  `Collections$UnmodifiableNavigableMap`. (`withDefault { … }` is no longer one
  of these: it answers a `HeapObj::MapWithDefault` view naming
  `groovy.lang.MapWithDefault`, and writes through to the map it wraps.)
  `Collections.emptyList`, `singletonList`
  and `nCopies` have the same name divergence (`Collections$EmptyList`,
  `$SingletonList`, `$CopiesList`) but no behaviour one, since nothing in the
  corpus mutates them.
- **`"abc".chars()` is not modeled.** It answers an `IntStream`, which groovyrs
  has no value for. (`toArray()`, `toCharArray()` and `String.bytes` no longer
  share this gap — they answer real `Object[]` / `char[]` / `byte[]` arrays.)
- **Only part of each AST-transform annotation's attribute set is read.**
  `@ToString`'s `includeNames` is honoured; its `excludes`, `includes`,
  `includeSuper`, `ignoreNulls` and the rest are accepted and ignored, as are
  `@EqualsAndHashCode`'s and `@TupleConstructor`'s. An annotation outside the
  family (`@Immutable`, `@Sortable`, `@CompileStatic`, …) is consumed and has no
  effect, which is what the member-level annotation skip already did.
- **A `static` method is not callable on the class.** `class Z { static String s() { 'x' } }; Z.s()`
  raises: a class name in expression position is a `java.lang.Class` and the
  static half of a class's method table is not modeled, so nothing answers.
  Calling it on an *instance* works, because that is ordinary dispatch. This is
  not trait-specific — a `static` method in a `trait` is unreachable for the same
  reason.
- **A class defining both `propertyMissing` forms gets only one.** The reader
  (`propertyMissing(String)`) and the writer (`propertyMissing(String, value)`)
  share a name, and methods are keyed by name alone (see the overloading entry
  above), so the second declaration replaces the first and only its half of the
  hook fires. Either form alone works. Same root as the overloading gap, not a
  hook-specific one.
- **A user instance renders as its class name, not `Class@hash`.** `new W()`
  prints `W` where Groovy prints `W@60fa3495`, and `toString()` agrees with it.
  The suffix is the JVM identity hash, which is not reproducible here — the same
  absence as the `HashSet` ordering note above.
- **A closure is called with the arguments it is given, padded with nulls.**
  Groovy resolves a closure call against the declared parameter count and raises
  `MissingMethodException` when nothing matches, so
  `[a:1].entrySet().collect { k, v -> "$k=$v" }` throws — an entry set is a
  plain collection, and its single `Map.Entry` element matches no
  two-parameter signature. groovyrs passes the entry as `k` and `null` as `v`
  and answers `[a=1=null]`. The methods that really do spread — `map.each`,
  `map.collect`, `map.collectMany`, and the rest with a genuine `Map` overload —
  are modeled and answer Groovy's `(key, value)`.
- **The bitwise operators need to *see* a decimal operand.**
  `Compiler::bit_operand_is_object` decides statically whether `&`/`|`/`^`/`~`/
  `>>` route to the host builtin that handles `BigInteger`s; a `G` literal, a
  name bound to one, and any expression containing either are spotted. An
  operand the compiler cannot see — `def a = f(); def b = g(); a & b`, where
  neither side names a decimal — keeps the native lowering, which reads the
  heap handle as `0`. Naming either operand's decimal-ness anywhere in the
  expression is enough to route it correctly.

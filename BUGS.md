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
  through a host GDK dispatch. A faithful subset is modeled: `size` (String
  chars / list / map), String `length`/`toUpperCase`/`toLowerCase`/`trim`/
  `reverse`/`isEmpty`/`contains`, list `isEmpty`/`contains`/`get`/`reverse`,
  map `isEmpty`/`containsKey`; property `.size`/`.length` and map key reads
  (`m.k`). An unknown method/property faults rather than mis-running.
- **List and map literals.** `[1, 2, 3]`, `[]`, `[a: 1]`, `[:]` build a fusevm
  `Array` (list) and a host-heap insertion-ordered map, and print Groovy-style
  (`[1, 2, 3]`, `[a:1]`, `[:]`).
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
  operands stay on the native/JIT fast path — only a user-class operand routes to
  a method. Mechanism: the strict numeric hook (and the `GDIV`/`GCMP` builtins)
  re-enter the running VM through a published thread-local pointer to invoke the
  method — no fusevm change.
- **Inheritance.** `class C extends B { … }` with single-inheritance method and
  field inheritance, virtual dispatch (the most-derived override wins, and a base
  method calling a virtual method reaches the override), `super.m(args)` (static
  parent dispatch), `super(args)` constructor chaining, inherited field
  initializers, `value instanceof Type` (user chain + built-in type names, with
  `null instanceof X` false), and `@Override` / other annotations parsed and
  ignored. `implements` is still ignored (interfaces have no runtime effect).
- **Closure-driven GDK iteration.** `each`, `eachWithIndex`, `collect`,
  `findAll`, `find`, `inject` (both the `inject(init){…}` and seedless
  `inject{…}` forms), and `sum` over lists (and over materialised ranges), e.g.
  `[1,2,3].collect { it * 2 }` → `[2, 4, 6]` and `[1,2,3,4].findAll { it % 2 == 0 }`
  → `[2, 4]`.
- **First-class ranges.** `0..5` (inclusive) and `0..<5` (half-open) build a
  Groovy list of the enumerated integers, so `.size()`, `.contains(x)`, `.each`,
  and `.collect` apply.
- **Ternary, Elvis, safe navigation.** `c ? t : e`, the Elvis `a ?: b`
  (null/false-coalescing), and `a?.member` / `a?.method()` (yields `null` on a
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
  `ConcurrentModificationException`, `GroovyRuntimeException` — so `catch`
  matching, `instanceof`, and `class MyEx extends Exception { MyEx(String m) {
  super(m) } }` all run through the one class registry. A throwable prints as
  `java.lang.Exception: boom` (bare class name for a script-declared subclass),
  and `getMessage()` / `.message` / `toString()` work. `finally` runs on every
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
  `groovy.lang.MissingPropertyException` (an unknown property read, and a write
  to a field the class chain never declared),
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

## Not implemented (errors today)

- **`trait` / `implements` behavior.** `extends` (single inheritance) is
  supported; `trait`s and interface method bodies are not, and `implements`
  clauses are parsed and ignored (only `Comparable`'s `compareTo` matters, and
  that is keyed off the method's presence, not the clause).
- **Method overloading by parameter type.** Methods (and a class's operator
  methods) are keyed by name only, so two same-named methods with different
  parameter types collapse to one (the last declared wins). Constructors *are*
  dispatched, but by arity only, not parameter type.
- **`++`/`--` do not call `next`/`previous`.** To keep the JIT fast path for
  integer loop counters (`for (i=0; i<n; i++)`), `++`/`--` lower to native
  `+ 1` / `- 1` rather than routing through a builtin (which would abort trace
  JIT). On a user-class instance they therefore dispatch `plus`/`minus`, not
  Groovy's `next`/`previous`. Call `x.next()` / `x.previous()` explicitly for
  those.
- **`switch`, `do/while`, labeled break, `assert`.**
- **`getClass()`.** There is no runtime class object, so `e.getClass().getName()`
  raises `MissingMethodException` rather than naming the type. Use `instanceof`.
- **`%` by zero.** `/` by zero raises a catchable `ArithmeticException` with
  Groovy's exact message (`Division by zero`, or `Division undefined` when the
  dividend is zero too), but `%` does not: an integer `x % 0` lowers to fusevm's
  native `Op::Mod` and yields `0` where Groovy raises `ArithmeticException: /
  by zero`, and a decimal `x % 0` raises through the numeric hook, which has no
  pending-exception channel — so the message is right but the run aborts instead
  of reaching a handler. Making either catchable costs `%`'s native/JIT path (the
  way `/` already pays for `GDIV`) or a pending check after every arithmetic op.
- **`String.toBigDecimal()`.** The other numeric conversions are modeled;
  `toBigDecimal` is not, so it raises `MissingMethodException` where Groovy would
  either convert or raise `NumberFormatException` with `BigDecimal`'s own
  character-level message.
- **`import`/`package`** are tolerated (skipped) but do nothing.
- **Command-argument chains beyond one arg** (`println a, b`, `foo bar baz`).

## Modeled with a documented simplification

- **A `MissingMethodException` message omits Groovy's `Possible solutions:`
  line.** Groovy appends a fuzzy suggestion list built from the receiver's real
  JDK/GDK method table (`Possible solutions: grep(), next(), size(), …`), which
  groovyrs has no table to build. Everything before it — `No signature of method:
  <name> for class: <qualified class> is applicable for argument types:
  (<simple types>) values: [<values>]` — is byte-identical to Groovy. The same
  holds for the suggestion line Groovy sometimes appends to a
  `MissingPropertyException` on a class with fields.
- **A property read on a list is not a spread read.** Groovy's `list.prop` maps
  the read over the elements, so `[1,2,3].zork` reports the *element* type
  (`No such property: zork for class: java.lang.Integer`, wrapped in an
  `Exception evaluating property` message); groovyrs reports the list itself.
- **The `NullPointerException` for a property *write* on `null` carries the
  JDK's helpful-NPE text** (`Cannot invoke "Object.getClass()" because "obj" is
  null`) rather than a groovyrs-authored message, since that is what Groovy
  surfaces. The wording is the JDK's, so it can change with the JVM version.

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
- **Every decimal operation allocates a heap slot that is never reclaimed.**
  Decimal literals are interned (a literal inside a loop allocates once), but each
  arithmetic *result* takes a new slot in the host heap, which has no collector.
  A script doing millions of decimal operations in a loop grows memory for the
  length of the run; integer and `double` arithmetic are unaffected (they never
  touch the heap).
- **`float`/`Float` is modeled as a `double`.** An `f`-suffixed literal parses to
  an `f64`, so it prints and computes with `double` precision: `0.1f + 0.2f` is
  `0.30000000000000004` where Groovy's `Float` prints `0.30000000447034836`.
- **Integer arithmetic uses fusevm's 64-bit wrapping.** Groovy's `Integer`
  arithmetic wraps at 32 bits (`2147483647 + 1` is `-2147483648`, `9993973 *
  -490` is `-602079474`); groovyrs computes in `i64` and wraps there instead, so
  a result that overflows an `int` but fits an `i64` prints the mathematically
  correct value rather than Groovy's wrapped one.
- **`for (x in a..b)` iterates ascending only.** A descending literal range
  (`5..1`, which Groovy walks downward) runs zero times. The endpoint is
  evaluated once (a body that mutates it still iterates the original range).
- **Types are not checked.** Declared types (`int`, `String`, `def`) are kept
  for diagnostics but do not gate execution — the runtime is dynamically typed on
  the fusevm value model.
- **`==` compares by value.** This matches Groovy (`==` is `.equals`, not
  reference identity) for the string/number/boolean operands modeled here.
  Cross-type comparisons that Groovy would coerce (`"5" == 5 → false`) are not
  yet distinguished — both sides compare by their printed form.
- **Upvalue capture of a frame local is by value, not by reference.** A closure
  nested in a function/closure captures the enclosing frame's locals at
  closure-creation time (the value is copied into the closure handle). Groovy
  captures the *variable*, so a mutation of the outer local made *after* the
  closure is created is visible to a later call; groovyrs's copy is not. The
  common curry / factory shapes (`{ x -> { y -> x + y } }`, `def make(n) {
  return { it + n } }`) are unaffected because the outer local is not mutated
  after capture. Capture of a **script** binding (a top-level global) stays
  by-reference, matching Groovy. Boxed-cell by-reference capture across live
  frames is a later wave.
- **Range values materialise ascending only.** `0..5` / `0..<5` enumerate to a
  list; a descending literal range (`5..0`) yields an empty list rather than the
  reverse sequence. `println` of a range value therefore shows the list form.
- **An unbound name reads as `null` instead of raising.** A declared-but-
  uninitialized local (`def x` then `println x`) and an entirely undeclared name
  both yield `null`; Groovy defaults the former to `null` too but raises
  `groovy.lang.MissingPropertyException` for the latter.
- **The paren-less `println <expr>` command form is more permissive** than
  Groovy's command-expression grammar. groovyrs parses the whole following
  expression as the single argument, so `println -42` prints `-42`. Real Groovy
  reads `println - 42` as a binary `minus` on the `println` method value and
  throws. Wrap the argument — `println(-42)` — for exact parity; the parenthesised
  form is unambiguous on both. (The differential fuzzer only ever emits the
  parenthesised form, so it never reports this.)

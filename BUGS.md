# Known gaps

An honest list of what groovyrs does **not** do yet. Slice 1 is the Groovy
*script* subset — top-level statements, arithmetic/logic, control flow, and the
`println`/`print` commands. Unsupported constructs are reported as parse or
compile errors, never silently mis-run.

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
- **List and map literals.** `[1, 2, 3]`, `[]`, `[a: 1]`, `[:]` build fusevm
  `Array`/`Hash` values and print Groovy-style (`[1, 2, 3]`, `[a:1]`, `[:]`).
- **`++`/`--` in expression position.** Both postfix (`i++`, value before
  update) and prefix (`++i`, value after update), in addition to the statement
  forms.
- **Closures.** `{ a, b -> … }` and the implicit `{ it }` single-parameter form
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
  faults as Groovy's `ArithmeticException` aborts. Magnitude is unbounded
  (`1.5e300 * 1.5e300` is `2.25E+600`). A `d`/`f`-suffixed literal stays an IEEE
  double with `Double.toString` rules (`2.5e7d` → `2.5E7`, `5.0d/0.0d` →
  `Infinity`). Value model in `src/decimal.rs`.

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
- **GStrings / interpolation.** `"$name"` / `"${expr}"` are lexed as literal
  text — the `${…}` is **not** evaluated. Use `+` concatenation.
- **`switch`, `do/while`, labeled break.**
- **`try`/`catch`/`finally`, exceptions, `throw`, `assert`.**
- **`import`/`package`** are tolerated (skipped) but do nothing.
- **Command-argument chains beyond one arg** (`println a, b`, `foo bar baz`).

## Modeled with a documented simplification

- **A decimal is truthy even when it is zero.** A `BigDecimal` is a host-heap
  value behind fusevm's opaque `Value::Obj` handle, and fusevm treats every
  handle as true, so `if (0.0)` takes the then-branch where Groovy takes the
  else. The same already applies to an empty ordered map (`if ([:])`). Numeric
  conditions written as comparisons (`if (x != 0)`, `while (i < n)`) are exact;
  those also stay on the JIT's native fast path, which is why the conditions are
  not routed through a truthiness builtin.
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
  reference identity) for the string/number/boolean operands slice 1 supports.
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
- **Uninitialized locals are unbound** and read back as `null`.
- **The paren-less `println <expr>` command form is more permissive** than
  Groovy's command-expression grammar. groovyrs parses the whole following
  expression as the single argument, so `println -42` prints `-42`. Real Groovy
  reads `println - 42` as a binary `minus` on the `println` method value and
  throws. Wrap the argument — `println(-42)` — for exact parity; the parenthesised
  form is unambiguous on both. (The differential fuzzer only ever emits the
  parenthesised form, so it never reports this.)

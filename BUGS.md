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

- **Decimal division promotes, but stays `f64`.** Groovy's `/` on two integers
  yields a `BigDecimal` (`7/2 == 3.5`, `4/2 == 2`), which groovyrs reproduces via
  the `GDIV` builtin. The result rides fusevm's `f64`, so a `BigDecimal`'s exact
  scale and non-terminating quotients differ: `1/3` prints the `f64`
  `0.3333333333333333`, not Groovy's `0.3333333333`.
- **Decimal literals are `f64`, not `BigDecimal`.** `3.10` prints `3.1`.
  Groovy's `BigDecimal` also *accumulates scale* through arithmetic — `10 * 1.25`
  is `12.50` and `0.25 + 0.25` is `0.50` (trailing zeros), where groovyrs prints
  `12.5` / `0.5`. Only standalone decimal literals, string concatenation of them,
  and terminating divisions (whose Groovy quotient is scale-stripped) print
  identically. Arbitrary-precision / scale-tracking decimal arithmetic is a later
  wave.
- **Integer arithmetic uses fusevm's 64-bit wrapping.** Groovy auto-promotes an
  overflowing `int`/`long` to `BigInteger`; groovyrs wraps at `i64` instead.
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

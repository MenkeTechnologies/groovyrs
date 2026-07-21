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

- **Classes.** `class`/`trait`, fields, `new`, and `this` are not compiled.
- **GStrings / interpolation.** `"$name"` / `"${expr}"` are lexed as literal
  text — the `${…}` is **not** evaluated. Use `+` concatenation in slice 1.
- **Multi-entry map print order.** `Value::Hash` is an unordered `HashMap`, so a
  map with more than one entry does not print in Groovy's insertion order.
  Single-entry maps render faithfully.
- **`switch`, `do/while`, labeled break, spaceship `<=>`.**
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
- **Closures capture script scope, not enclosing-function/closure locals.** A
  closure's non-parameter names resolve to the script (global) bindings, which is
  faithful for the common script-level case. A closure defined *inside* a function
  or another closure does not capture that enclosing frame's locals as upvalues
  (so a curried `{ x -> { y -> x + y } }` does not see the outer `x`). Real
  lexical upvalue capture is a later wave.
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

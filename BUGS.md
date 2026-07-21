# Known gaps

An honest list of what groovyrs does **not** do yet. Slice 1 is the Groovy
*script* subset — top-level statements, arithmetic/logic, control flow, and the
`println`/`print` commands. Unsupported constructs are reported as parse or
compile errors, never silently mis-run.

## Not implemented (errors today)

- **Methods, closures, classes.** `def f(x) { … }`, `{ it -> … }` closures,
  `class`/`trait`, fields, `new`, and `this` are not compiled. Only the script
  body runs. (Next wave: fusevm's native `Op::Call` frame ABI.)
- **Method / property access on values.** `s.length()`, `list.size()`,
  `obj.field`, `it.toUpperCase()`. Any `.` call after a value is rejected.
- **GStrings / interpolation.** `"$name"` / `"${expr}"` are lexed as literal
  text — the `${…}` is **not** evaluated. Use `+` concatenation in slice 1.
- **Collections & the GDK.** Lists (`[1, 2, 3]`), maps (`[a: 1]`), ranges as
  first-class values, and GDK methods (`each`, `collect`, `find`, `*.`) are not
  modeled. `for (x in a..b)` integer ranges are the only iterable form.
- **`switch`, `do/while`, ternary `?:`, the Elvis `?:`, safe-nav `?.`,
  labeled break, spaceship `<=>`.**
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
- **Uninitialized locals are unbound** and read back as `null`.
- **The paren-less `println <expr>` command form is more permissive** than
  Groovy's command-expression grammar. groovyrs parses the whole following
  expression as the single argument, so `println -42` prints `-42`. Real Groovy
  reads `println - 42` as a binary `minus` on the `println` method value and
  throws. Wrap the argument — `println(-42)` — for exact parity; the parenthesised
  form is unambiguous on both. (The differential fuzzer only ever emits the
  parenthesised form, so it never reports this.)

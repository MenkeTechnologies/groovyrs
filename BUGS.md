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
- **Decimal literals are `f64`, not `BigDecimal`.** `3.10` prints `3.1`;
  arbitrary-precision decimal arithmetic is not modeled.
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

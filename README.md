```
 ██████╗ ██████╗  ██████╗  ██████╗ ██╗   ██╗██╗   ██╗
██╔════╝ ██╔══██╗██╔═══██╗██╔═══██╗██║   ██║╚██╗ ██╔╝
██║  ███╗██████╔╝██║   ██║██║   ██║██║   ██║ ╚████╔╝
██║   ██║██╔══██╗██║   ██║██║   ██║╚██╗ ██╔╝  ╚██╔╝
╚██████╔╝██║  ██║╚██████╔╝╚██████╔╝ ╚████╔╝    ██║
 ╚═════╝ ╚═╝  ╚═╝ ╚═════╝  ╚═════╝   ╚═══╝     ╚═╝
```

[![CI](https://github.com/MenkeTechnologies/groovyrs/actions/workflows/ci.yml/badge.svg)](https://github.com/MenkeTechnologies/groovyrs/actions/workflows/ci.yml)
![Rust](https://img.shields.io/badge/Rust-2021-05d9e8?style=flat-square)
![license](https://img.shields.io/badge/license-MIT-ff2a6d?style=flat-square)
![status](https://img.shields.io/badge/status-active%20%C2%B7%20in%20development-9b5de5?style=flat-square)

### `[GROOVY, COMPILED TO BYTECODE — JIT-COMPILED, NOT WALKED — NO JVM]`

> *"Apache Groovy runs Groovy on the JVM. groovyrs runs Groovy on fusevm."*

**Groovy in Rust** — a Groovy frontend that lexes and parses Groovy script
source, lowers it to [`fusevm`](https://github.com/MenkeTechnologies/fusevm)
bytecode, and runs it on the shared three-tier Cranelift JIT — the same engine
behind `zshrs`, `stryke`, `awkrs`, `elisp`, `ruby`, `python`, `php`, `node`, and
`java`. No bespoke VM. No JVM. No `.class` files.

---

## Table of Contents

- [\[0x00\] Overview](#0x00-overview)
- [\[0x01\] Install](#0x01-install)
- [\[0x02\] Usage](#0x02-usage)
- [\[0x03\] Language Features](#0x03-language-features)
- [\[0x04\] Command-Line Flags](#0x04-command-line-flags)
- [\[0x05\] Architecture](#0x05-architecture)
- [\[0x06\] Status & Roadmap](#0x06-status--roadmap)
- [\[0xFF\] License](#0xff-license)

---

## [0x00] OVERVIEW

Every Groovy runtime in existence targets the JVM: `groovyc` emits `.class`
bytecode, and a JVM interprets and JIT-compiles it. `groovyrs` takes a different
path — it lexes and parses Groovy script source to an AST, lowers that AST
**directly to fusevm bytecode**, and runs it on fusevm's compiled VM with a
Cranelift tracing JIT. groovyrs carries no VM or JIT of its own; it is a pure
frontend over the shared engine. Highlights:

- **Compiled, not tree-walked** — arithmetic, comparisons, and control flow
  lower to native fusevm ops (`LoadInt`, `Add`, `NumLt`, `JumpIfFalse`, …), so
  the tracing JIT compiles hot loops to native code.
- **fusevm-hosted, no JVM** — no local `vm.rs` / `jit.rs`, no `.class` files, no
  `libjvm`. The same three-tier Cranelift engine that hosts zshrs, stryke,
  awkrs, elisp, ruby, python, php, node, and java runs Groovy too.
  `jit-disk-cache` persists native code across runs.
- **The Groovy script model** — a `.groovy` file is a sequence of top-level
  statements (classes optional, no `main`); semicolons optional (newlines
  terminate statements), and `println x` works with or without parentheses.
- **Groovy value semantics** — `println` formats `true`/`false`, `3.0`, and
  `null` the Groovy way; an unsuffixed decimal literal is a real
  `java.math.BigDecimal` (exact scale, `2.5e7` prints `2.5E+7`, `1.25 * 0` is
  `0.00`), and integer `/` promotes to it so `7 / 2` is `3.5` and `1 / 3` is
  `0.3333333333`.
- **Operator overloading** — a strict numeric hook supplies string concatenation
  (`"x=" + x`) for mixed operands, and dispatches a user-class instance's operator
  method (`plus`/`minus`/`multiply`/`compareTo`/`equals`/…) for `+`/`-`/`*`/`<`/
  `==`/… — by re-entering the VM. A primitive operand never routes to a method,
  though the hook does answer the primitive pairs fusevm declines to (`Integer`
  overflow, and an integral/`double` mix past 2^53) with Groovy's own result.

Every program in `examples/` is diffed byte-for-byte against Apache Groovy in
the test suite.

---

## [0x01] INSTALL

```sh
git clone https://github.com/MenkeTechnologies/groovyrs
cd groovyrs
cargo build --release
# the binary is target/release/groovy
```

Requires a stable Rust toolchain. No JVM, no Groovy install.

---

## [0x02] USAGE

```sh
groovy script.groovy          # run a Groovy script
groovy -e 'println 6 * 7'     # run an inline script string
groovy --version              # print the version banner
groovy --dump-tokens f.groovy # inspect the lexer token stream
groovy --dump-ast f.groovy    # inspect the parsed AST
groovy --disasm f.groovy      # inspect the lowered fusevm bytecode
groovy --tiers f.groovy       # run it, then report which fusevm tiers took it
groovy --lsp                  # Language Server Protocol over stdio
groovy --dap                  # Debug Adapter Protocol over stdio
```

```groovy
// hello.groovy
println "Hello from groovyrs — Groovy on fusevm"

for (n in 1..5) {
    if (n % 2 == 0) println n + " even"
    else println n + " odd"
}
```

---

## [0x03] LANGUAGE FEATURES

Implemented and checked against Apache Groovy:

- **Script model** — a file is top-level statements (no `main`); classes may be
  declared. Statements are separated by newlines or `;` (semicolons optional). A
  leading `#!` shebang and `package`/`import` lines are tolerated.
- **Variables** — `def x = …`, typed `int` / `double` / `String` / `boolean`
  declarations, and bare `x = …` script bindings; plain and compound assignment
  (`=`, `+=`, `-=`, `*=`, `/=`, `%=`); increment / decrement in both statement
  and expression position, postfix (`i++`) and prefix (`++i`).
- **Functions** — `def f(a, b) { … }` (and typed `Type f(…) { … }`) compiled to
  fusevm subroutine regions over the native `Op::Call` frame ABI. Parameters and
  locals are frame slots, so recursion and mutual recursion are sound; `return
  <expr>` carries a value out, and a `return`-less body returns its last value
  expression (else `null`).
- **Expressions** — integer (decimal, `0x` hex, `0b` binary and leading-zero
  octal, with `_` group separators and the `L`/`G` suffixes) / decimal / string
  (single-, double- and triple-quoted) / boolean / `null` literals; `+ - * / % **`,
  `== != < > <= >=`, `&& ||` (short-circuiting), the bitwise `& | ^ ~` and the
  shifts `<< >> >>>`, `x in coll`, the `value as Type` coercion, unary `-` and
  `!`, grouping. An unsuffixed decimal literal is a `BigDecimal` whose scale
  propagates (`1.10 + 2.20 == 3.30`), a `d`/`f` suffix makes an IEEE double;
  integer `/` promotes to a decimal (`7 / 2 == 3.5`); `**` follows the numeric
  tower (`2 ** 10` is an `Integer`, `2 ** -1` a `BigDecimal`); `+` concatenates
  when either side is a string, `-` and `*` also work on strings and lists
  (`"abc" * 3`, `[1, 2, 3] - [2]`), `<<` is `leftShift` (a bit shift, a list
  append, or a string concatenation) and `>>` is `rightShift` (a bit shift on a
  number, forward composition on a closure).
- **32-bit `Integer` arithmetic.** Groovy's integer width is a property of the
  value, and groovyrs reproduces it: `Integer.MAX_VALUE + 1` is `-2147483648`,
  `1000000 * 1000000` is `-727379968`, and `2147483647 + 1L` is `2147483648`
  because the `L` makes it a `Long`. The shifts carry their operand's width too,
  so `1 << 32` is `1` and `1L << 32` is `4294967296`. Arithmetic that stays
  inside `Integer` range — all of it, in the ordinary program — never leaves
  fusevm's native and JIT'd fast paths.
- **Collections** — list literals `[1, 2, 3]` / `[]` and insertion-ordered map
  literals `[a: 1, b: 2]` / `[:]`, printed Groovy-style; subscripting `list[i]`
  (negative index counts from the end), `map[k]`, `str[i]`, and by a range
  (`list[0..1]`, `"abcdef"[1..3]`); subscript *assignment* `list[i] = v` (growing
  the list with nulls past the end) and `map[k] = v`. A multi-entry map keeps
  insertion order and `m.k = v` mutates it in place.
- **Lists are references**, as Groovy's are: `def b = a` gives one `ArrayList`
  a second name, so `b.add(4)` is visible through `a`, and `a.is(b)` is `true`
  while a `collect` copy is `false`. The same holds for a list reached through a
  map value, an element of another list, a closure parameter or a capture, and
  for every in-place mutator (`add`, `remove`, `set`, `clear`, `push`, `pop`,
  `<<`, `sort`, `unique`, `removeAll`, `retainAll`, `swap`, `list[i] = v`).
  `removeAll`/`retainAll` take Groovy's *predicate closure* as well as a
  collection, and `push` inserts at the front — the end `pop` takes from.
  `addAll` answers whether the list changed (`[1, 2].addAll([])` is `false`) and
  takes Java's `addAll(index, collection)` insert form.
- **`subList` is a live window**, a `java.util.ArrayList$SubList` and not a copy:
  a write through the window reaches the backing list, a write to the backing
  list shows through the window, and a structural write through the window
  (`add`, `remove`, `clear`, `addAll`, `pop`, `push`, `unique`) splices the
  backing list and resizes the window with it. A window onto a window reaches the
  root list. Java's fail-fast rule is modeled too: a **structural** change made
  to the backing list through any *other* reference invalidates the window
  permanently, and every later read or write through it raises
  `java.util.ConcurrentModificationException` — `getClass()` and `is()` still
  answer, because they read the reference rather than the elements.
- **Declarations** — the multi-declarator `def a = 1, b = 2` and Groovy's
  multiple assignment `def (a, b) = [1, 2]` (the right side is evaluated once;
  a name past its end is `null`).
- **Classes** — `class C { fields; C(..){..}; def m(){..} }`, `new C(args)`,
  fields with initializers, arity-dispatched constructors, methods with an
  implicit `this`, property get/set with Groovy's auto `getX`/`setX`, a bare
  field resolving to `this.field`, and `toString()` driving `println`. Instances
  are heap objects with reference identity. A user `getAt(i)` drives `obj[i]`.
- **Inheritance** — `class C extends B { … }` with single-inheritance field and
  method inheritance, virtual dispatch (most-derived override wins),
  `super.m(args)` and `super(args)` chaining, inherited field initializers,
  `value instanceof Type` (user chain + built-in types), and `@Override` parsed
  and ignored.
- **Interfaces** — `interface I { … }`, `class C implements A, B`, an interface's
  own multiple `extends`, abstract method declarations, and Java 8 `default`
  methods (inherited by every implementor, overridable by a class). `instanceof`
  walks superclasses and interfaces transitively.
- **Operator overloading** — a user-class instance operand dispatches its
  operator method: `+`→`plus`, `-`→`minus`, `*`→`multiply`, `/`→`div`,
  `%`→`remainder`, `**`→`power`, unary `-`→`negative`, `[]`→`getAt`,
  `<<`→`leftShift`, `>>`→`rightShift`;
  `<`/`>`/`<=`/`>=`/`<=>` via `compareTo`; null-safe `==`/`!=` via `compareTo`
  (Comparable) or `equals`. A primitive operand never routes to a method.
- **Method / property dispatch** — `s.length()`, `list.size()`,
  `"hi".toUpperCase()`, `map.k`, chains on literals (`[1,2,3].size()`), over a
  faithful GDK subset routed through a host dispatch. `size`/`length` are
  methods, not properties, so `[1,2].size` raises the way Groovy's does. An
  unknown member raises a catchable `MissingMethodException` /
  `MissingPropertyException`, and so does a bare *name* nothing binds
  (`println zork`), naming the script class Groovy names — the file's stem, or
  `script_from_command_line` under `-e`.
- **`hashCode()`** — Java's specified rule per type, not an approximation:
  `String` over UTF-16 code units, `Integer` versus `Long`, `Double` over
  `doubleToLongBits`, `BigDecimal` carrying its scale, `BigInteger` over its
  magnitude words, `AbstractList` / `AbstractMap` / `AbstractSet`, and
  `IntRange`'s own Cantor pairing. A user `hashCode` overrides it.
- **`args`** — every script's binding carries the launcher arguments after the
  file (empty when there are none), as a `List`.
- **Closures** — `{ a, b -> … }`, defaulted parameters (`{ a, b = 5 -> … }`),
   the explicit zero-parameter `{ -> … }`, and the implicit `{ it }` form as first-class
  callable values, invoked with `.call(args)` or directly (`def f = { it * 2 };
  f(21)`). A closure captures its enclosing script scope, and a closure nested in
  a function/closure captures that frame's locals as upvalues, so a curried
  `{ x -> { y -> x + y } }` and a chained call `f(a)(b)` work.
- **Closure-driven GDK** — over lists, ranges and a `String`'s characters:
  `each`, `eachWithIndex`, `reverseEach`, `collect`, `collectMany`,
  `collectEntries`, `findAll`, `find`, `findResult`, `findIndexOf`, `any`,
  `every`, `count`, `countBy`, `inject`, `sum`, `sort`, `toSorted`, `unique`,
  `toUnique`, `max`, `min`, `groupBy`, `split`, `takeWhile`, `dropWhile`, `grep`,
  `findIndexValues`, `join`, `reverse`
  (`[1,2,3].collect { it * 2 }` → `[2, 4, 6]`), plus `with`/`tap` on any value
  and the `Number` loops `times` / `upto` / `downto` / `step`. A one-parameter
  closure to `sort`/`max`/`min`/`unique` is a key extractor, a two-parameter one
  a comparator, and `sort`/`unique` mutate a variable receiver the way Groovy's
  do; a multi-parameter closure receives a list element *spread* across its
  parameters (`[[1,2],[3,4]].collect { a, b -> a + b }` → `[3, 7]`). Over maps:
  `each`, `collect`, `collectEntries`, `findAll`, `find`, `any`, `every`,
  `groupBy`, `countBy`, `count`, `inject`, `sort`, `max`, `min`, `withDefault`,
  `collectMany` — a two-parameter closure gets `(key, value)`, a one-parameter
  closure a `Map.Entry`.
- **`grep` filters by the filter's `isCase`, not by `==`** — the same five rules
  a `switch` label follows, so the filter's *type* picks the test: a closure
  calls, a `Class` is `isInstance` (`[1, 'a'].grep(Integer)` → `[1, 2]`), a
  `Pattern` matches the whole string, and a collection or range is membership.
  The no-argument `grep()` keeps the Groovy-true elements. It is defined on
  `Object`, so a receiver that is not a collection iterates as a single element
  (`5.grep { it > 1 }` → `[5]`), and on a map it answers the **list** of
  accepted entries rather than the map `findAll` answers.
- **`with` / `tap` delegate the closure to the receiver** — a bare call *and* a
  bare name the script cannot resolve both dispatch against it
  (`[:].with { put('a', 1) }`, `'abc'.with { toUpperCase() }`,
  `[a: 1].with { a }`), innermost `with` first, and a script binding of the same
  name still wins (Groovy's `OWNER_FIRST`). Writes go back through the delegate
  too — `m.with { a = 9 }` updates `m`, `m.with { b = 7 }` adds the key, and
  `+=`, `++`/`--`, a subscript write and a mutating method's write-back all
  follow. A mutator writes through, so `[1, 2].tap { add(3) }` is `[1, 2, 3]`.
- **Closure combinators** — `curry` / `rcurry` / `ncurry`, `memoize`,
  `andThen` / `compose` and the `>>` / `<<` operators, and `clone`, each
  answering another closure that reports the arity it still accepts.
- **Pure GDK** — lists answer `take`, `drop`, `takeRight`, `dropRight`, `first`,
  `head`, `last`, `tail`, `init`, `pop`, `removeLast`, `swap`, `getAt`,
  `indexOf`, `flatten`, `intersect`, `minus`, `plus` (including the
  `plus(index, other)` splice), `disjoint`, `transpose`, `collate`,
  `combinations`, `permutations` and `subsequences` (both answering the
  `java.util.HashSet<List>` Groovy does, in the JDK's bucket order),
  `withIndex`, `indexed`, `iterator`/`listIterator`, `toSet`, `subList`,
  `toList`, `containsAll`, `putAt`, `removeAt`/`removeElement` and the mutators,
  and every value answers `inspect()` (the *verbose* rendering, so
  `[1, 'a'].inspect()` is `[1, 'a']` where `toString()` is `[1, a]`) with
  `toListString`/`toMapString` as the plain-rendering aliases;
  strings answer `indexOf`/`lastIndexOf` (with the
  `fromIndex` and code-point overloads), `replace`, `split`, `tokenize`,
  `charAt`, `substring`, `compareTo`, `padLeft`/`padRight`/`center`,
  `capitalize`, `take`/`drop`, `multiply`, `minus`, `startsWith`/`endsWith`,
  `tr`, `trim`, `strip`/`stripLeading`/`stripTrailing`/`isBlank`, `stripIndent`,
  `stripMargin`, `expand`, `normalize`/`denormalize`, `readLines`, `formatted`,
  `equalsIgnoreCase`, `uncapitalize` and the
  `isInteger`/`isLong`/`isDouble`/`isBigDecimal`/`isBigInteger`/`isNumber`
  predicates. Every index is a **UTF-16 code unit** the way Java's is, so
  `"a😀b".length()` is 4 and `indexOf("b")` is 3; `trim` strips code points
  `<= U+0020` while `strip` strips `Character.isWhitespace`, which are different
  sets. Maps answer `put`, `remove`, `getOrDefault`, `entrySet`, `keySet`,
  `values`, `subMap`, `spread`, `minus`, `intersect`, `iterator`,
  `containsValue`, `putAt`, `putAll`, `clear`. Numbers answer `power`, the scaled
  `round(n)` and `trunc([n])`, `intdiv`, `abs`, the conversions, and the operator
  method names — `and`/`or`/`xor`/`bitwiseNegate` and
  `leftShift`/`rightShift`/`rightShiftUnsigned`, which fill to the receiver's
  Java width. A `BigDecimal`/`BigInteger` additionally answers Java's own
  `add`/`subtract`/`multiply`/`divide`/`remainder`/`mod`/`pow`, which are **not**
  the Groovy operators: `7G.divide(3G)` truncates to `2` where `7G / 3G` is
  `2.3333333333`, and `1.0G.divide(3.0G)` raises `ArithmeticException` where the
  operator approximates.
- **`<=>` needs a `Comparable`** — a list, a map, a set, a range and a user class
  with no `compareTo` are not, so `[1, 2] <=> [1, 3]` raises the
  `IllegalArgumentException` Groovy raises (even for two equal lists) rather than
  inventing an order. `null` still orders before everything.
- **JDK statics** — `Math` (`max`, `min`, `abs`, `round`, `signum`, `sqrt`,
  `floor`, `ceil`, `rint`, `pow`, `hypot`, `atan2`, the trig and log family,
  `floorDiv` / `floorMod`, `IEEEremainder`, `ulp`, `copySign`, `nextUp` /
  `nextDown` / `nextAfter`, `getExponent`, the `addExact` / `subtractExact` /
  `multiplyExact` / `toIntExact` throwing family, `PI`, `E`), `Integer` / `Long`
  (`parseInt` / `parseLong` / `valueOf` with an optional radix and the named
  class's range check, `toString` with one, `toHexString` / `toBinaryString` /
  `toOctalString` filling to the named class's width, `compare`, `signum`,
  `sum`, `max` / `min`, `bitCount`, `numberOfLeadingZeros` /
  `numberOfTrailingZeros`, `highestOneBit` / `lowestOneBit`, `reverse` /
  `reverseBytes`, `MAX_VALUE` / `MIN_VALUE`), `Double` (`compare`, `isNaN` /
  `isInfinite` / `isFinite`, `sum`, `max` / `min`, `doubleToLongBits` /
  `doubleToRawLongBits` / `longBitsToDouble`), `Boolean`, `BigDecimal` /
  `BigInteger` `ZERO` / `ONE` / `TWO` / `TEN`, `Character` (`isDigit` /
  `isLetter` / `isLetterOrDigit` / `isWhitespace` / `isUpperCase` /
  `isLowerCase`, `toUpperCase` / `toLowerCase` / `toString`, `compare`,
  `getNumericValue`, `MIN_RADIX` / `MAX_RADIX` — each with the `int` code-point
  overload), `Collections` (`emptyList` / `emptyMap` / `emptySet`,
  `singletonList`, `nCopies`, `unmodifiableList`, the in-place `sort` / `reverse`,
  `max` / `min`, `frequency`, `disjoint`), `Arrays.asList`, `System`
  (`lineSeparator`, `getProperty` with and without a default, `getenv`), and
  `String.format` / `String.valueOf`, plus the script-scope `printf` / `sprintf`
  over a `java.util.Formatter` subset (`%s %d %f %x %o %b %n %%`, width,
  precision, left-justification and the `,` grouping flag, so `%,d` of `1234567`
  is `1,234,567`). `Math.round`, `Math.signum`, `Math.max` / `Math.min` and
  `Double.compare` follow Java's rules rather than the same-named Rust ones,
  which differ on ties, signed zeros and NaN.
- **Spread `*.`** — `list*.member` / `list*.method(args)` applies the member to
  every element (null-safe, so a `null` element spreads to `null`).
- **`getClass()` / `.class`** — a `java.lang.Class` value that prints
  `class java.lang.Integer` and answers `name`/`simpleName`/`canonicalName`.
- **`String.toBigDecimal()`** — `new BigDecimal(text.trim())`, with the exact
  scale and `BigDecimal`'s own character-level `NumberFormatException` messages.
- **Ranges** — `0..5` / `0..<5`, descending (`5..1`), character (`'a'..'e'`) and
  decimal (`1.5..4.0`), as a real `groovy.lang.Range`: it prints `1..5`,
  `getClass()` names `IntRange`/`ObjectRange`/`NumberRange`/`EmptyRange`, and
  `from`/`to`/`step(n)`/`reverse()`/`size()`/`contains(x)`/`isReverse()` are its
  own members — with `from`/`to` reporting the bounds of what is enumerated, so
  `(4..0).from` is 0 and `(0..<4).to` is 3. The walk steps with `next`/`previous`
  and so keeps the element type (`1.5..4.0` is `[1.5, 2.5, 3.5]`). Being a
  `java.util.List` in Groovy, every `List` method and operator applies too
  (`.each`, `.collect`, `+`, `== [1, 2, 3]`, `in`, `r[1..2]`).
- **Regex** — `~/…/` patterns, `/…/` slashy strings (backslashes literal,
  interpolating, multi-line), the `=~` and `==~` operators, and a stateful
  `java.util.regex.Matcher` (`find`/`group`/`start`/`end`/`matches`/
  `groupCount`/`pattern`/`m[i]`, iteration, and `find()` truth so `while (m)`
  walks). `String` carries `matches`, `replaceAll`/`replaceFirst` in both the
  `$n` and closure forms, `findAll`, `find`, and Java's specified `split` rules.
- **`BigInteger`** — `123G`, integer literals past `Long`, `new BigInteger(…)`,
  `as BigInteger`, and the overflowing integer `**`, as a type distinct from
  `BigDecimal` with unbounded magnitude.
- **Instantiable JDK classes** — `new StringBuilder()` / `StringBuffer` /
  `StringWriter` (mutating through a shared handle, so `sb.append("a").append(1)`
  and `sb << "a" << 1` chain), the collection classes, the box types, and
  `BigDecimal`/`BigInteger`.
- **Ternary / Elvis / safe navigation** — `c ? t : e`, `a ?: b`, and
  `a?.member` / `a?.method()`.
- **Control flow** — `if` / `else if` / `else`, `while`, `do`/`while`, the
  C-style `for (init; cond; update)`, the `for (x in a..b)` / `for (x in a..<b)`
  range loop and the `for (x in <collection>)` loop (a list's elements, a map's
  entries, a `String`'s characters), `break`, `continue`, labeled
  `break`/`continue`, `return`.
- **`assert`** — with Groovy's power-assert rendering: the statement's source
  followed by every sub-expression's value under its own column. The
  `assert cond : message` form raises the plain `AssertionError` Groovy does.
- **`switch`** — Groovy's, with the full `isCase` semantics: constant, range,
  list, type (`case String:`), `~/…/` pattern, closure, and `null` labels,
  source-order fall-through until a `break`, and a `default` anywhere.
- **Output** — `println` / `print` with Groovy value formatting, in both the
  `println(x)` and paren-less `println x` command forms.
- **Comments** — `//` line, `/* … */` block.

- **Catchable runtime faults** — an unknown method or property, a call on
  `null`, an out-of-range index, and an unparsable numeric conversion raise the
  Groovy throwable Groovy raises (`MissingMethodException`,
  `MissingPropertyException`, `NullPointerException`,
  `IndexOutOfBoundsException`, `NumberFormatException`) with Groovy's own message
  text, so `try`/`catch` reaches its handler. Runaway recursion is one of them:
  it raises `java.lang.StackOverflowError`, which sits under
  `VirtualMachineError`, so `catch (Exception e)` does not swallow it.
- **Throwable shape, not just message** — a throwable carries the payload
  Groovy's handlers read: `getCause` / `initCause` and the
  `T(message, cause)` / `T(cause)` constructors, `getSuppressed` /
  `addSuppressed`, and the two throwables Groovy's dynamic dispatch makes
  ordinary control flow —  `MissingMethodException.getMethod()` /
  `getType()` / `getArguments()` and `MissingPropertyException.getProperty()` /
  `getType()`. A type may be named fully qualified wherever a type is named:
  `catch (groovy.lang.MissingMethodException e)`, a multi-catch arm,
  `instanceof`, and `new`.

See [`BUGS.md`](BUGS.md) for the honest known-gaps list (`trait`s, method
overloading by parameter type, script-declared class names as values, Java
arrays, `GString` as a type, `++`/`--` not calling `next`/`previous`,
by-reference upvalue capture).

---

## [0x04] COMMAND-LINE FLAGS

| Flag | Effect |
| --- | --- |
| `FILE [args…]` | Run a `.groovy` script. |
| `-e` / `--eval SCRIPT` | Run an inline script string and exit. |
| `-v` / `-version` / `--version` | Print the version banner and exit. |
| `-h` / `--help` | Print usage and exit. |
| `--dump-tokens FILE` | Print the lexer token stream and exit. |
| `--dump-ast FILE` | Print the parsed AST and exit. |
| `--disasm FILE` | Print the lowered fusevm bytecode (with source line numbers) and exit. |
| `--tiers FILE` | Run it, then report which fusevm execution tier took each of its chunks. |
| `--lsp` | Speak the Language Server Protocol over stdio. |
| `--dap` | Speak the Debug Adapter Protocol over stdio. |

`groovy --version` reports the targeted language level (`Groovy 4.0`) followed by
the real engine (`groovyrs <crate-version>`) and the host triple, so nothing is
misrepresented as Apache Groovy.

### Editor tooling

Both editor servers ship in the same binary and speak their protocol over stdio:

- **`--lsp`** — a Language Server. Diagnostics come from the runtime's own
  parser (a syntax error maps to its reported line); completion and hover draw on
  the language-surface corpus in `src/lsp.rs` — reserved words, literal forms,
  contextual keywords, operators, script commands, every dispatched GDK method
  and property, the class-member hooks, the modeled throwables, and the built-in
  type names, each with a signature, a description, and a runnable example. The
  same corpus generates `docs/reference.html` via `cargo run --bin gen-docs`, so
  the page never drifts from what the server knows.
- **`--dap`** — a Debug Adapter. The script is compiled with per-statement line
  markers and run without the tracing JIT so every marker fires; source-line
  breakpoints, stepping (`next` / `stepIn` / `stepOut` over the single script
  frame), a `stackTrace`, and `variables` (script locals) are supported. Program
  `println` output is forwarded as `output` events so it never corrupts the JSON
  channel.

---

## [0x05] ARCHITECTURE

groovyrs contains no virtual machine or JIT of its own. The execution path
mirrors how `zshrs` hosts zsh, `ruby` hosts Ruby, and `java` hosts Java:

```
Groovy script → lexer → parser (AST) → lower to fusevm bytecode → fusevm VM + Cranelift JIT
                                              │
                                strict numeric hook (Groovy `+` concat + operator overloads)
                                GDIV / GCMP / GDEC builtins (`/`, `<=>`, BigDecimal literals)
                                print builtins (Groovy value formatting)
```

| Piece | How |
| --- | --- |
| **fusevm-hosted** | No local `vm.rs` / `jit.rs`, no JVM. Groovy lowers to fusevm bytecode and runs on the shared three-tier Cranelift JIT; `jit-disk-cache` persists native code across runs. |
| **Native arithmetic** | `+ - * %`, comparisons, and logic lower to native fusevm ops; the JIT traces hot integer loops. `%` additionally carries a four-op zero-divisor guard (Java's `%` throws where fusevm's `Op::Mod` answers `0`), elided entirely when the divisor is a non-zero literal — see BUGS.md for what it costs when it is not. A strict numeric hook supplies Groovy's `+` string concatenation for non-numeric operands, and dispatches a user-class instance's operator method (`plus`/`minus`/`compareTo`/…) by re-entering the VM through a published thread-local pointer — a user-class operand is the only thing that routes to a method. The hook also answers a *primitive* pair whenever fusevm declines to natively (an `Integer`-range overflow, or an integral/`double` mix whose integer is past 2^53 and so cannot be widened exactly), with the identical result the native path gives — Groovy promotes to `double`, so the rounded answer is the correct one. |
| **Groovy division** | `/` lowers to the `GDIV` builtin: two integers divide exactly to an integer and to a `BigDecimal` otherwise (`7/2 → 3.5`, `1/3 → 0.3333333333`), following Groovy's `BigDecimalMath` scale policy; a zero divisor raises Groovy's catchable `ArithmeticException`. |
| **`BigDecimal` value model** | An unsuffixed decimal literal is an exact (unscaled value, scale) pair on the host heap (`src/decimal.rs`), so scale propagates through `+ - * / %` (`1.25 * 0 → 0.00`, `2.5e7 + 1 → 25000001`) and magnitude is unbounded (`1.5e300 * 1.5e300 → 2.25E+600`). Being non-numeric to fusevm, decimals route through the strict numeric hook; `d`/`f`-suffixed literals are IEEE doubles, answered natively except where the other operand is an integer too large to widen exactly, which the hook promotes to `double` the same way. |
| **Groovy print semantics** | `println`/`print` lower to a registered builtin that formats values Groovy-style (`true`/`false`, `3.0`, `null`), rather than the VM's shell-flavoured `PrintLn`. |

---

## [0x06] STATUS & ROADMAP

Groovy scripts — top-level statements, `def`/typed locals, user-defined
functions (recursion over the native `Op::Call` frame ABI), closures
(`{ a, b -> … }` / implicit `{ it }`, `.call` and direct invocation) with the
closure-driven GDK (`each` / `collect` / `findAll` / `find` / `inject` / `sum` /
`sort` / `groupBy` / `countBy` / `max` / `min` / `join`), the closure
combinators (`curry` / `rcurry` / `ncurry` / `memoize` / `andThen` / `compose`),
delegating `with` / `tap`, and the spread operator
and nested-closure upvalue capture (curried `{ x -> { y -> x + y } }`, chained
`f(a)(b)`), classes (fields, constructors, methods, `this`, property get/set with
auto getter/setter, `new`, `toString`, `getAt` subscript) on a host object heap,
single-inheritance `extends` (virtual dispatch, `super`, `instanceof`), operator
overloading (`plus`/`minus`/`multiply`/`div`/`remainder`/`power`/`negative`/
`compareTo`/`equals` driving `+`/`-`/`*`/`/`/`%`/`**`/unary `-`/`<`/`>`/`<=>`/`==`),
insertion-ordered maps, first-class `Range` values, `java.util.regex` patterns /
matchers / the `=~` and `==~` operators, `BigInteger`, arithmetic /
comparison / logic, `BigDecimal`-style division, ternary / Elvis /
safe-navigation, Groovy truthiness, GString interpolation, `try` / `catch` /
`finally` / `throw`, `if` / `while` / `for` / range `for-in` / `break` /
`continue` / `return`, list/map literals, subscripting, method/property dispatch
over a GDK subset, `println`/`print`, string concatenation — verified against
Apache Groovy by the frozen example replay and the differential fuzzer. The editor tooling is
shipped: a bytecode disassembler (`--disasm`), a Language Server (`--lsp`), and a
Debug Adapter (`--dap`).

Next waves, in priority order:

1. **By-reference upvalue capture** — boxed cells so a closure sees a mutation of
   an outer frame local made after capture (capture is by value today).
2. **Method overloading, by arity as well as by parameter type** — today methods
   and operator methods are keyed by name only, so same-named declarations
   collapse onto the first, which then answers calls of every arity.
   Constructors already key by arity and are the shape to follow.
3. **`trait`s** — `interface` and `implements` are modeled (including `default`
   methods); `trait` is not.
4. **Java arrays.** `new int[3]` does not resolve — an array is a distinct type
   from a `List` (`.length` versus `.size()`, class name `[I`), and modeling it
   as a `List` would make `[1, 2, 3].length` answer where Groovy raises.
5. **A `GString` type.** `"$s"` produces a plain `java.lang.String`, so
   `"$s".getClass()` reports `java.lang.String` where Groovy reports
   `org.codehaus.groovy.runtime.GStringImpl`.
6. **A `List`/`Map` implementation kind.** A set carries one, so a `TreeSet`
   sorts; a list and a map do not, so `new LinkedList([1,2]).getClass()` reports
   `java.util.ArrayList` and — the behaviour difference, not just a name —
   `new TreeMap([b:2, a:1])` prints in insertion order rather than key order.
   The same missing kind is why `asImmutable()` takes a write instead of raising
   `UnsupportedOperationException`.
7. **Command-argument chains beyond one argument** — `println a, b` and
   `foo bar baz` do not parse; the parenthesised call always does.

See [`BUGS.md`](BUGS.md) for the honest known-gaps list.

### Differential parity harness

Four harnesses check groovyrs against the reference `groovy`, all comparing two
subprocesses exactly as a user would observe them:

```sh
cargo run --bin parity                 # diff examples/*.groovy vs live `groovy`
cargo run --bin parity-fuzz -- \
    --mode control --count 2000        # fuzz: groovy -e <s> vs groovyrs -e <s>
bash parity-scripts/run.sh -v          # byte-parity over the regression corpus
bash parity-scripts/fuzz.sh            # diff the probe corpus, one JVM start
```

`fuzz.sh` diffs `parity-scripts/probes.txt` — many hundreds of small snippets
covering division and `BigDecimal` scale, the bit and shift operators at every
width (including `BigInteger`'s unbounded ones), number formatting, `GString`
interpolation, the list/map/string GDK, the JDK statics, `Matcher`, closures,
ranges, throwables and control flow. Every probe is wrapped in a `try`, so a
throw is a comparable observation rather than a dead run.

Both sides run each probe **in isolation**. groovyrs starts in milliseconds, so
it runs one process per probe; the oracle would need ~50 minutes to do the same,
so it runs a driver that hands each probe to its own `GroovyShell` from a single
JVM. A fresh shell carries a fresh `Binding`, so an undeclared assignment cannot
leak into the next probe (`counter = 0` then `counter += 1` raises
`MissingPropertyException`, as it does standalone — concatenating the probes into
one script made it print `2`), and parsing under the probe's own filename gives
the script the class name a standalone run would derive from it. What one JVM
still shares is process-wide state: system properties, the default locale, static
initialisers.

`run.sh`, `fuzz.sh`, `parity` and `parity-fuzz` all gate the oracle before
comparing anything, on two axes.

The **JVM**: the `groovy` launcher is a shell script that resolves its JVM from
an ambient `JAVA_HOME`, so the same binary answers from a different JVM depending
on the caller's environment — and `Double.toString` was reimplemented in JDK 19,
so a pre-19 JVM renders every double differently (`1.0e23` prints
`9.999999999999999E22` there and `1.0E23` from JDK 19 on).

The **locale**: the JVM reads `user.language`/`user.country` from the environment,
and two frozen behaviours move with them — number formatting
(`String.format("%,.2f", 1234.5)` is `1,234.50` under en-US and `1.234,50` under
de-DE) and case mapping (`"hi".toUpperCase()` is `HI` under en-US and `Hİ` under
tr-TR). `examples/GStrings.groovy` prints a `toUpperCase()`, so the frozen
snapshot really does depend on it.

Each harness probes the resolved oracle, prints its Groovy and JVM version
alongside the `JAVA_HOME` it saw, and exits 2 naming the axis that failed rather
than reporting the oracle's disagreements as groovyrs divergences. Run them with
`JAVA_HOME` pointing at a JDK 19+ install (or unset) and an en-US default locale.

`parity-fuzz` generates grammar-driven, deterministic-output snippets from a
per-index seed (so any divergence replays with `--seed <N> --once`, then
auto-minimizes). It stays strictly inside groovyrs's implemented surface and away
from the documented simplifications (`/` by zero), so every divergence it reports
is a real parity gap — the class of bug the slice-1 `continue`-codegen fix was.
It compares stdout and success-versus-failure; stderr and the exact exit code are
not compared, so a divergence in a diagnostic's *text* is invisible to it (the
probe corpus covers those by catching and printing the throwable's class).
Decimal literals, scales, and exponent forms are generated without restriction
now that the `BigDecimal` model is exact. Modes:
`arith`, `logic`, `strings`, `control`, `format`, `truth`, `closures`,
`gstring`, `exceptions`, `faults`, `switch`, `asserts`, `modzero`, `gdk`,
`conversions`, `classes`, `ranges`, `aliasing`, `views`, `mixed`.

Both fuzzers report a **skipped** count alongside the divergences. A case only
counts as a comparison when the reference itself ran the program — it neither
timed out nor rejected the program before executing a line of it. Without that
split, a generated program the reference also refuses reads as agreement, and a
run that measured nothing reports a clean pass. A run of *ours* that has to be
killed is always a divergence, never a match.

All four need `groovy` on PATH and never run in CI; the CI-safe replay is the
frozen `tests/parity.rs` (snapshot in `tests/data/parity_expected.txt`,
regenerated only from real `groovy`).

---

## [0xFF] LICENSE

MIT — free and open source. See [`LICENSE`](LICENSE).

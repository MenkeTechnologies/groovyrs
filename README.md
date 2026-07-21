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
  `null` the Groovy way, and integer `/` promotes like `BigDecimal` so `7 / 2`
  is `3.5`, not `3`.
- **Groovy `+` overloading** — a strict numeric hook supplies string
  concatenation (`"x=" + x`) for the mixed operands the VM's native arithmetic
  does not compute.

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
- **Expressions** — integer / decimal / string (single- and double-quoted) /
  boolean / `null` literals; `+ - * / %`, `== != < > <= >=`, `&& ||`
  (short-circuiting), unary `-` and `!`, grouping. Integer `/` promotes to a
  decimal (`7 / 2 == 3.5`); `+` concatenates when either side is a string.
- **Collections** — list literals `[1, 2, 3]` / `[]` and insertion-ordered map
  literals `[a: 1, b: 2]` / `[:]`, printed Groovy-style; subscripting `list[i]`
  (negative index counts from the end), `map[k]`, `str[i]`. A multi-entry map
  keeps insertion order and `m.k = v` mutates it in place.
- **Classes** — `class C { fields; C(..){..}; def m(){..} }`, `new C(args)`,
  fields with initializers, arity-dispatched constructors, methods with an
  implicit `this`, property get/set with Groovy's auto `getX`/`setX`, a bare
  field resolving to `this.field`, and `toString()` driving `println`. Instances
  are heap objects with reference identity. A user `getAt(i)` drives `obj[i]`.
- **Method / property dispatch** — `s.length()`, `list.size()`,
  `"hi".toUpperCase()`, `map.k`, chains on literals (`[1,2,3].size()`), over a
  faithful GDK subset routed through a host dispatch. An unknown member faults.
- **Closures** — `{ a, b -> … }` and the implicit `{ it }` form as first-class
  callable values, invoked with `.call(args)` or directly (`def f = { it * 2 };
  f(21)`). A closure captures its enclosing script scope, and a closure nested in
  a function/closure captures that frame's locals as upvalues, so a curried
  `{ x -> { y -> x + y } }` and a chained call `f(a)(b)` work.
- **Closure-driven GDK** — `each`, `eachWithIndex`, `collect`, `findAll`,
  `find`, `inject`, `sum` over lists and ranges (`[1,2,3].collect { it * 2 }` →
  `[2, 4, 6]`).
- **Ranges** — first-class `0..5` / `0..<5` values with `.size()`,
  `.contains(x)`, `.each`, `.collect`.
- **Ternary / Elvis / safe navigation** — `c ? t : e`, `a ?: b`, and
  `a?.member` / `a?.method()`.
- **Control flow** — `if` / `else if` / `else`, `while`, the C-style
  `for (init; cond; update)`, the `for (x in a..b)` / `for (x in a..<b)` range
  loop, `break`, `continue`, `return`.
- **Output** — `println` / `print` with Groovy value formatting, in both the
  `println(x)` and paren-less `println x` command forms.
- **Comments** — `//` line, `/* … */` block.

See [`BUGS.md`](BUGS.md) for the honest known-gaps list (inheritance/`trait`,
operator overloading through `+`/`-`/`<=>`/`==`, GStrings, by-reference upvalue
capture).

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
| `--lsp` | Speak the Language Server Protocol over stdio. |
| `--dap` | Speak the Debug Adapter Protocol over stdio. |

`groovy --version` reports the targeted language level (`Groovy 4.0`) followed by
the real engine (`groovyrs <crate-version>`) and the host triple, so nothing is
misrepresented as Apache Groovy.

### Editor tooling

Both editor servers ship in the same binary and speak their protocol over stdio:

- **`--lsp`** — a Language Server. Diagnostics come from the runtime's own
  parser (a syntax error maps to its reported line); completion and hover draw on
  the keyword / literal / command corpus in `src/lsp.rs`, which also generates
  `docs/reference.html` via `cargo run --bin gen-docs` (so the page never drifts
  from what the server knows).
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
                                strict numeric hook (Groovy `+` concat)
                                GDIV builtin (BigDecimal-style `/`)
                                print builtins (Groovy value formatting)
```

| Piece | How |
| --- | --- |
| **fusevm-hosted** | No local `vm.rs` / `jit.rs`, no JVM. Groovy lowers to fusevm bytecode and runs on the shared three-tier Cranelift JIT; `jit-disk-cache` persists native code across runs. |
| **Native arithmetic** | `+ - * %`, comparisons, and logic lower to native fusevm ops; the JIT traces hot integer loops. A strict numeric hook supplies Groovy's `+` string concatenation for non-numeric operands. |
| **Groovy division** | `/` lowers to the `GDIV` builtin: two integers divide exactly to an integer and to a decimal otherwise (`7/2 → 3.5`), matching Groovy's `BigDecimal` promotion. |
| **Groovy print semantics** | `println`/`print` lower to a registered builtin that formats values Groovy-style (`true`/`false`, `3.0`, `null`), rather than the VM's shell-flavoured `PrintLn`. |

---

## [0x06] STATUS & ROADMAP

Groovy scripts — top-level statements, `def`/typed locals, user-defined
functions (recursion over the native `Op::Call` frame ABI), closures
(`{ a, b -> … }` / implicit `{ it }`, `.call` and direct invocation) with the
closure-driven GDK (`each` / `collect` / `findAll` / `find` / `inject` / `sum`)
and nested-closure upvalue capture (curried `{ x -> { y -> x + y } }`, chained
`f(a)(b)`), classes (fields, constructors, methods, `this`, property get/set with
auto getter/setter, `new`, `toString`, `getAt` subscript) on a host object heap,
insertion-ordered maps, first-class ranges (`0..5` / `0..<5`), arithmetic /
comparison / logic, `BigDecimal`-style division, ternary / Elvis /
safe-navigation, `if` / `while` / `for` / range `for-in` / `break` / `continue` /
`return`, list/map literals, subscripting, method/property dispatch over a GDK
subset, `println`/`print`, string concatenation — verified against Apache Groovy
by the frozen example replay and the differential fuzzer. The editor tooling is
shipped: a bytecode disassembler (`--disasm`), a Language Server (`--lsp`), and a
Debug Adapter (`--dap`).

Next waves, in priority order:

1. **Operator overloading through the operators** — `plus`/`minus`/`compareTo`/
   `equals` driving `+`/`-`/`<=>`/`==` (a user `getAt` already drives `[]`). This
   needs a VM-re-entrant numeric hook in fusevm; the current hook signature
   cannot run a user method.
2. **Inheritance** — `extends`/`super`/interfaces and superclass method
   resolution (today classes are flat; `extends` is parsed and ignored).
3. **By-reference upvalue capture** — boxed cells so a closure sees a mutation of
   an outer frame local made after capture (capture is by value today).
4. **Interpolation & standard library** — GString `"$name"` / `"${expr}"`;
   `Math`, broader `java.util`/GDK collection methods.
5. **Scale-tracking decimals** — a real `BigDecimal` value so `10 * 1.25` prints
   `12.50`, closing the last documented arithmetic divergence.

See [`BUGS.md`](BUGS.md) for the honest known-gaps list.

### Differential parity harness

Two harnesses check groovyrs against the reference `groovy`, both comparing two
subprocesses exactly as a user would observe them:

```sh
cargo run --bin parity                 # diff examples/*.groovy vs live `groovy`
cargo run --bin parity-fuzz -- \
    --mode control --count 2000        # fuzz: groovy -e <s> vs groovyrs -e <s>
bash parity-scripts/run.sh -v          # byte-parity over the regression corpus
```

`parity-fuzz` generates grammar-driven, deterministic-output snippets from a
per-index seed (so any divergence replays with `--seed <N> --once`, then
auto-minimizes). It stays strictly inside groovyrs's implemented surface and away
from the documented f64-vs-`BigDecimal` divergences, so every divergence it
reports is a real parity gap — the class of bug the slice-1 `continue`-codegen fix
was. Modes: `arith`, `logic`, `strings`, `control`, `format`, `mixed`.

All three need `groovy` on PATH and never run in CI; the CI-safe replay is the
frozen `tests/parity.rs` (snapshot in `tests/data/parity_expected.txt`,
regenerated only from real `groovy`).

---

## [0xFF] LICENSE

MIT — free and open source. See [`LICENSE`](LICENSE).

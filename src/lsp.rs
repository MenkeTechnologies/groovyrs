//! Language Server Protocol over stdio (`groovy --lsp`).
//!
//! Self-contained and read-only: diagnostics come from the same `parser::parse`
//! the runtime uses (a syntax error maps to the reported line); hover and
//! completion draw on the keyword / command / literal corpus below. No output
//! ever reaches the terminal — JSON-RPC on stdio only. Structure follows the
//! sibling `-rs` frontends' `lsp.rs` (see `pythonrs/src/lsp.rs`).
//!
//! The corpus documents what groovyrs *actually* recognizes today, and only
//! that: the lexer's reserved words, the literal forms it lexes, the contextual
//! keywords the parser accepts by position, every operator it lowers, the
//! `println`/`print` script commands, every GDK method `host::dispatch_*`
//! answers, every property `host::dispatch_property` reads, every class-member
//! hook the runtime calls by name, the throwable hierarchy `throwable.rs`
//! pre-registers, and the type names `compiler::BUILTIN_TYPE_NAMES` resolves.
//! It documents no GDK method the runtime does not implement, so completion and
//! hover never advertise a capability the engine lacks; where groovyrs diverges
//! from Apache Groovy the entry says so.

use std::collections::HashMap;

use lsp_server::{Connection, ErrorCode, ExtractError, Message, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{Completion, HoverRequest, Request as _};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, Hover, HoverContents, HoverParams, HoverProviderCapability,
    MarkupContent, MarkupKind, Position, PublishDiagnosticsParams, Range, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions, Uri,
};

/// The language-surface corpus: `(name, chapter, signature, one-line doc,
/// example)`. Single source of truth for LSP completion and hover, and for the
/// generated `docs/reference.html`. Every entry mirrors something the runtime
/// truly recognizes, and the chapter names the source table it came from:
///   * "Reserved Words"            → `lexer::keyword_or_ident`
///   * "Literals and Literal Forms"→ the literal arms of `lexer::lex`
///   * "Contextual Keywords"       → identifiers `parser` accepts by position
///   * "Operators"                 → `lexer::Tok` punctuation + `compiler` lowering
///   * "Script Commands"           → `host::GPRINTLN` / `host::GPRINT`
///   * "GDK — …"                   → `host::dispatch_method`,
///     `host::dispatch_iteration`, `host::dispatch_map_iteration`
///   * "Properties"                → `host::dispatch_property`
///   * "Class Member Hooks"        → the names the runtime calls on an instance
///   * "Throwables"                → `throwable::THROWABLES`
///   * "Type Names"                → `compiler::BUILTIN_TYPE_NAMES`
///   * "Inline Rust"               → the `rust { … }` FFI desugar
const CORPUS: &[(&str, &str, &str, &str, &str)] = &[
    // ── Reserved Words — lexer::keyword_or_ident ──
    ("def", "Reserved Words", "def name [= expr]",
     "Declare a dynamically-typed local variable, or a script binding at top level. An unassigned `def` reads as `null`.",
     "def x = 5\nprintln(x)   // => 5"),
    ("if", "Reserved Words", "if (cond) stmt",
     "Conditional branch. The condition goes through Groovy truth (`host::groovy_truthy`), so an empty string, an empty list, `0`, and `null` are all false.",
     "if (1 < 2) println(\"yes\")   // => yes"),
    ("else", "Reserved Words", "if (cond) stmt else stmt",
     "The fallback branch of an `if`. Binds to the nearest unmatched `if`.",
     "if (false) println(\"a\") else println(\"b\")   // => b"),
    ("while", "Reserved Words", "while (cond) stmt",
     "Loop while the condition is Groovy-truthy; the test runs before the first iteration.",
     "def i = 0\nwhile (i < 3) i++\nprintln(i)   // => 3"),
    ("do", "Reserved Words", "do stmt while (cond)",
     "Post-tested loop: the body runs once before the condition is evaluated.",
     "def i = 0\ndo { i++ } while (i < 3)\nprintln(i)   // => 3"),
    ("for", "Reserved Words", "for (init; cond; update) stmt | for (name in iterable) stmt",
     "C-style three-clause loop, or the `in` form over a range, list, map, or String. The `in` form materialises its subject through `host::GITER` first.",
     "for (i in 0..2) print(i)   // => 012"),
    ("in", "Reserved Words", "for (name in iterable)",
     "The iteration separator of a `for` loop. Walks a range's integers, a list's elements, a map's entries, or a String's characters.",
     "for (n in [1, 2, 3]) print(n)   // => 123"),
    ("switch", "Reserved Words", "switch (subject) { case label: … default: … }",
     "Multi-way branch. Each label is tested with Groovy's `isCase` (`host::GIS_CASE`), so a constant, a range, a list, a type name, a `~/…/` pattern, or a closure all work as labels.",
     "switch (5) { case 4..6: println(\"in\"); break; default: println(\"out\") }   // => in"),
    ("case", "Reserved Words", "case label:",
     "One `switch` label. Sections fall through to the next one until a `break`, exactly as in Java.",
     "switch (1) { case 1: println(\"one\"); break }   // => one"),
    ("default", "Reserved Words", "default: | default ReturnType method(params) { … }",
     "The `switch` section entered when no `case` label matched; a duplicate `default` is a parse error. Inside an `interface` body it instead marks a method that carries an implementation.",
     "switch (9) { case 1: break; default: println(\"d\") }   // => d"),
    ("assert", "Reserved Words", "assert cond [ : message ]",
     "Raise `PowerAssertionError` when the condition is falsy, printing the source text with each recorded sub-expression's value under the column it occupied. The `: message` form instead raises a plain `AssertionError` whose text is `<message>. Expression: <condition>`, with a `Values:` clause naming the condition's bare-variable operands.",
     "assert 1 + 1 == 2   // passes silently"),
    ("return", "Reserved Words", "return [expr]",
     "Return from the enclosing method, closure, or constructor; at script level it ends the script. A method with no `return` yields its last expression.",
     "def f() { return 7 }\nprintln(f())   // => 7"),
    ("break", "Reserved Words", "break [label]",
     "Exit the nearest enclosing loop or `switch`. `break label` exits the loop carrying that label; an unknown label is a compile error.",
     "for (i in 0..9) { if (i == 3) break; print(i) }   // => 012"),
    ("continue", "Reserved Words", "continue [label]",
     "Skip to the next iteration of the nearest enclosing loop, or of the labeled one.",
     "for (i in 0..3) { if (i == 1) continue; print(i) }   // => 023"),
    ("new", "Reserved Words", "new Type([args])",
     "Construct an instance. Fields across the whole superclass chain are materialised to `null`, initialisers run superclass-first, then the most-derived constructor of matching arity runs (`host::GNEW`).",
     "class P { def x; P(v) { x = v } }\nprintln(new P(7).x)   // => 7"),

    // ── Literals and Literal Forms — the literal arms of lexer::lex ──
    ("true", "Literals and Literal Forms", "true",
     "The boolean true value, lexed as a reserved word.",
     "println(true)   // => true"),
    ("false", "Literals and Literal Forms", "false",
     "The boolean false value.",
     "println(1 > 2)   // => false"),
    ("null", "Literals and Literal Forms", "null",
     "The null reference. A method call on `null` answers only `toString`, `equals`, and `getClass`; anything else raises `NullPointerException`.",
     "def x\nprintln(x)   // => null"),
    ("42", "Literals and Literal Forms", "<digits>",
     "An integer literal. It is an `Integer` when it fits in 32 bits and a `Long` when it does not, which is the width its arithmetic wraps at: `1000000 * 1000000` is `-727379968`. A literal past `Long` range is a lex error, not a silent wrap.",
     "println(42 + 1)   // => 43"),
    ("42L", "Literals and Literal Forms", "<digits>L | <digits>l",
     "A long-suffixed integer literal. The suffix makes the literal a `Long` whatever its magnitude, so its arithmetic wraps at 64 bits rather than 32: `2000000000L + 2000000000L` is `4000000000` where the unsuffixed form is `-294967296`.",
     "println(2000000000L + 2000000000L)   // => 4000000000"),
    ("0xFF", "Literals and Literal Forms", "0x<hex> | 0b<binary> | 0<octal>",
     "A radix-prefixed integer literal. The digits are read as a magnitude and the literal takes the smallest type holding it, so `0xff` is an `Integer` and `0xFFFFFFFF` is the `Long` `4294967295`.",
     "println(0xFF)   // => 255"),
    ("1_000_000", "Literals and Literal Forms", "<digits>_<digits>",
     "An integer or decimal literal with `_` group separators. The separators are not part of the value.",
     "println(1_000_000)   // => 1000000"),
    ("1.50", "Literals and Literal Forms", "<digits>.<digits> [g|G]",
     "An unsuffixed decimal literal — a `java.math.BigDecimal`, kept as its exact source text so the literal's own scale survives. `1.50` prints with both fraction digits, which no `f64` could preserve.",
     "println(1.50)   // => 1.50"),
    ("1.5d", "Literals and Literal Forms", "<digits>.<digits> d|D|f|F",
     "A `d`/`f`-suffixed literal — an IEEE double (`Value::Float`). This is the only literal form that is *not* a `BigDecimal`. The `instanceof` table classifies by value shape rather than by literal form, so groovyrs answers `1.5d instanceof BigDecimal` true and `1.5 instanceof BigDecimal` false — Apache Groovy answers the opposite for both.",
     "println(1.5d / 0.0d)   // => Infinity"),
    ("2.5e7", "Literals and Literal Forms", "<digits>[.<digits>]e[+|-]<digits>",
     "An exponent literal. The exponent makes the literal a decimal with or without a fractional part, and the result is a `BigDecimal` unless a `d`/`f` suffix follows.",
     "println(2.5e7)   // => 2.5E+7"),
    ("'text'", "Literals and Literal Forms", "'…'",
     "A single-quoted String — inert text. Backslash escapes are decoded, but `$` never interpolates.",
     "println('a$b')   // => a$b"),
    ("\"text\"", "Literals and Literal Forms", "\"… $name … ${ expr } …\"",
     "A double-quoted String. With no placeholder it lexes as a plain String; with one it becomes a GString whose parts render through `toString()` at runtime (`host::GSTRING`). `$name` takes a dotted *property* path only, so `\"$n.toString()\"` reads the property `n.toString` and leaves `()` as text.",
     "def n = 5\nprintln(\"n=$n and ${n + 1}\")   // => n=5 and 6"),
    ("~/pattern/", "Literals and Literal Forms", "~/…/",
     "A regex literal — a pattern handle built by `host::GREGEX`. Only `\\/` is special inside the delimiters; every other backslash escape passes through to the pattern. Modeled far enough to drive a `switch` label, whose `isCase` is a *full* match.",
     "switch (\"abc\") { case ~/a.c/: println(\"m\"); break }   // => m"),
    ("[a, b]", "Literals and Literal Forms", "[expr, …]",
     "A list literal. Lists are `Value::Array` and print with Groovy's `[1, 2, 3]` rendering.",
     "println([1, 2, 3])   // => [1, 2, 3]"),
    ("[]", "Literals and Literal Forms", "[]",
     "The empty list. Falsy under Groovy truth, so `if ([])` does not run.",
     "if (![]) println(\"empty\")   // => empty"),
    ("[k: v]", "Literals and Literal Forms", "[key: expr, …] | [(expr): expr, …]",
     "A map literal, built by `host::GMAKE_MAP` into an insertion-ordered map. A bare identifier key is the string of its own name; parenthesise the key to compute it.",
     "println([a: 1, b: 2])   // => [a:1, b:2]"),
    ("[:]", "Literals and Literal Forms", "[:]",
     "The empty map — the form that distinguishes an empty map from the empty list `[]`.",
     "println([:].isEmpty())   // => true"),
    ("{ a, b -> … }", "Literals and Literal Forms", "{ param, … -> body }",
     "A closure literal with an explicit parameter list, including the zero-parameter form `{ -> … }`. It lowers to a subroutine region plus a runtime closure handle and captures the enclosing script bindings.",
     "def add = { a, b -> a + b }\nprintln(add(2, 3))   // => 5"),
    ("{ it }", "Literals and Literal Forms", "{ body }",
     "A closure literal with no parameter list: Groovy supplies the single implicit parameter `it`.",
     "println([1, 2, 3].collect { it * 2 })   // => [2, 4, 6]"),

    // ── Contextual Keywords — identifiers the parser accepts by position ──
    ("try", "Contextual Keywords", "try { … } catch (T e) { … } [finally { … }]",
     "Run a block with `catch` handlers and/or a `finally` cleanup. Recognised at statement start when followed by `{`; a `try` with neither a `catch` nor a `finally` is a parse error.",
     "try { throw new Exception(\"x\") } catch (Exception e) { println(e.message) }   // => x"),
    ("catch", "Contextual Keywords", "catch (Type [| Type…] name) { … }",
     "Handle a thrown value whose class chain reaches the named type. The multi-catch form `catch (A | B e)` tests each alternative in turn.",
     "try { 1 / 0 } catch (ArithmeticException e) { println(e.message) }   // => Division by zero"),
    ("finally", "Contextual Keywords", "finally { … }",
     "Cleanup block. Runs on every exit path out of the `try` — normal completion, a caught throwable, an uncaught one, and a `return`.",
     "try { println(\"body\") } finally { println(\"cleanup\") }   // => body then cleanup"),
    ("throw", "Contextual Keywords", "throw expr",
     "Raise a throwable (`host::GTHROW`), unwinding to the innermost `catch` whose type matches. groovyrs has no unwind opcode: the throw parks the value and the compiler-emitted guards cut the stack back.",
     "try { throw new IllegalStateException(\"bad\") } catch (Exception e) { println(e) }   // => java.lang.IllegalStateException: bad"),
    ("class", "Contextual Keywords", "[modifiers] class Name [extends S] [implements I, …] { … }",
     "Declare a class. Registration is a `host::GCLASS` call carrying the superclass, interfaces, field names, initialisers, methods, and constructors.",
     "class P { def x = 1 }\nprintln(new P().x)   // => 1"),
    ("interface", "Contextual Keywords", "[modifiers] interface Name [extends I, …] { … }",
     "Declare an interface. `new` on one faults; its `default` methods are reachable from an implementing class through the interface closure.",
     "interface Greet { default def hi() { \"hi\" } }\nclass A implements Greet {}\nprintln(new A().hi())   // => hi"),
    ("extends", "Contextual Keywords", "class Name extends Superclass",
     "Name the direct superclass in a class header (or the supertype list in an interface header). Method lookup walks this chain most-derived first.",
     "class A { def f() { 1 } }\nclass B extends A {}\nprintln(new B().f())   // => 1"),
    ("implements", "Contextual Keywords", "class Name implements Iface, …",
     "Name the interfaces a class implements. They participate in `instanceof` and in `catch` matching through the interface closure.",
     "interface I {}\nclass A implements I {}\nprintln(new A() instanceof I)   // => true"),
    ("instanceof", "Contextual Keywords", "value instanceof Type",
     "Runtime type test (`host::GINSTANCEOF`) at relational precedence. Answers for declared classes, the 25 modeled throwables, and the 22 built-in type names. `null instanceof X` is always false.",
     "println(\"x\" instanceof String)   // => true"),
    ("package", "Contextual Keywords", "package name.path",
     "A leading package declaration. Parsed and skipped — groovyrs has no package namespace, so the line has no runtime effect.",
     "package demo\nprintln(1)   // => 1"),
    ("import", "Contextual Keywords", "import name.path[.*]",
     "A leading import. Parsed and skipped: the built-in type and throwable names resolve without one, and there is no classpath to import from.",
     "import java.util.List\nprintln([1].size())   // => 1"),
    ("public", "Contextual Keywords", "public …",
     "Access modifier on a type declaration or class member. Accepted and skipped — groovyrs enforces no access control.",
     "class A { public def f() { 1 } }\nprintln(new A().f())   // => 1"),
    ("private", "Contextual Keywords", "private …",
     "Access modifier. Accepted and skipped: a `private` member is reachable from anywhere, which is a deliberate divergence from Groovy.",
     "class A { private def x = 1 }\nprintln(new A().x)   // => 1"),
    ("protected", "Contextual Keywords", "protected …",
     "Access modifier. Accepted and skipped, like `private`.",
     "class A { protected def x = 2 }\nprintln(new A().x)   // => 2"),
    ("static", "Contextual Keywords", "static …",
     "Member modifier. Accepted and skipped — groovyrs has no static storage, so a `static` field is an ordinary per-instance field.",
     "class A { static def f() { 3 } }\nprintln(new A().f())   // => 3"),
    ("final", "Contextual Keywords", "final …",
     "Immutability modifier. Accepted and skipped; groovyrs does not reject a later write.",
     "class A { final def x = 4 }\nprintln(new A().x)   // => 4"),
    ("abstract", "Contextual Keywords", "abstract …",
     "Abstractness modifier. Accepted and skipped: groovyrs does not refuse `new` on an `abstract` class the way it refuses it on an `interface`.",
     "abstract class A { def f() { 5 } }\nprintln(new A().f())   // => 5"),
    ("synchronized", "Contextual Keywords", "synchronized …",
     "Member modifier. Accepted and skipped — a groovyrs script runs on one thread, so there is nothing to synchronise.",
     "class A { synchronized def f() { 6 } }\nprintln(new A().f())   // => 6"),
    ("transient", "Contextual Keywords", "transient …",
     "Serialization modifier. Accepted and skipped; groovyrs has no serialization.",
     "class A { transient def x = 7 }\nprintln(new A().x)   // => 7"),
    ("volatile", "Contextual Keywords", "volatile …",
     "Memory-visibility modifier. Accepted and skipped, like `synchronized`.",
     "class A { volatile def x = 8 }\nprintln(new A().x)   // => 8"),
    ("this", "Contextual Keywords", "this[.member]",
     "The receiver inside a class member. A bare field or method name inside a class body already resolves through `this`, so the explicit form is rarely needed.",
     "class A { def x = 1; def f() { this.x } }\nprintln(new A().f())   // => 1"),
    ("super", "Contextual Keywords", "super(args) | super.method(args)",
     "Invoke the superclass constructor (`host::GSUPER_CTOR`, valid as the first statement of a constructor) or a superclass method non-virtually (`host::GSUPER_METHOD`).",
     "class A { def f() { \"A\" } }\nclass B extends A { def f() { super.f() + \"B\" } }\nprintln(new B().f())   // => AB"),
    ("it", "Contextual Keywords", "it",
     "The implicit single parameter of a closure written without a parameter list. It is an ordinary binding, so a closure with explicit parameters has no `it`.",
     "println([1, 2].collect { it + 1 })   // => [2, 3]"),
    // ── Operators — lexer::Tok punctuation + the compiler's lowering ──
    ("+", "Operators", "a + b",
     "Addition. Two integers stay integral, a decimal operand keeps `BigDecimal` scale, a `double` operand promotes to a double, a String operand concatenates (rendering the other side through `toString()`), a list operand appends, and a map operand merges. On a class instance it dispatches `plus`.",
     "println(1 + 2)\nprintln(\"a\" + 1)   // => 3 then a1"),
    ("-", "Operators", "a - b",
     "Subtraction over the same numeric tower as `+`. On a class instance it dispatches `minus`.",
     "println(5 - 2)   // => 3"),
    ("*", "Operators", "a * b",
     "Multiplication. On a class instance it dispatches `multiply`.",
     "println(3 * 4)   // => 12"),
    ("/", "Operators", "a / b",
     "Division, lowered to the `GDIV` builtin rather than a VM opcode. Two integers that divide exactly yield an integer; anything else yields a `BigDecimal` — never a double, unless an operand already is one. A zero divisor raises `ArithmeticException`. On a class instance it dispatches `div`.",
     "println(7 / 2)\nprintln(4 / 2)   // => 3.5 then 2"),
    ("%", "Operators", "a % b",
     "Remainder. The compiler emits a bare opcode when it can prove the divisor non-zero and a guarded builtin call otherwise. On a class instance it dispatches `remainder`, which is Groovy's mapping — not `mod`.",
     "println(5 % 3)   // => 2"),
    ("- (unary)", "Operators", "-a",
     "Arithmetic negation. On a class instance it dispatches `negative`.",
     "println(-(-3))   // => 3"),
    ("!", "Operators", "!a",
     "Logical negation of the operand's Groovy truth, so `![]`, `!\"\"`, `!0`, and `!null` are all true.",
     "println(!false)   // => true"),
    ("==", "Operators", "a == b",
     "Equality. Groovy's `==` is value equality, not identity, and it is null-safe. On a class instance it uses `compareTo(…) == 0` when the class defines `compareTo`, a user `equals` otherwise, and heap identity with neither.",
     "println([1, 2] == [1, 2])   // => true"),
    ("!=", "Operators", "a != b",
     "The negation of `==`, resolved through the same instance rules.",
     "println(1 != 2)   // => true"),
    ("<", "Operators", "a < b",
     "Less-than. On a class instance it dispatches `compareTo`; a class without one falls back to comparing the rendered strings.",
     "println(1 < 2)   // => true"),
    (">", "Operators", "a > b",
     "Greater-than, resolved like `<`.",
     "println(3 > 2)   // => true"),
    ("<=", "Operators", "a <= b",
     "Less-than-or-equal, resolved like `<`.",
     "println(2 <= 2)   // => true"),
    (">=", "Operators", "a >= b",
     "Greater-than-or-equal, resolved like `<`.",
     "println(2 >= 3)   // => false"),
    ("<=>", "Operators", "a <=> b",
     "The spaceship operator — `compareTo`, lowered to the `GCMP` builtin. Yields a negative number, zero, or a positive number.",
     "println(5 <=> 3)   // => 1"),
    ("&&", "Operators", "a && b",
     "Short-circuiting logical and. Both operands go through Groovy truth; the result is a `Boolean`, not the operand.",
     "println(1 && \"x\")   // => true"),
    ("||", "Operators", "a || b",
     "Short-circuiting logical or, with the same truth rules as `&&`.",
     "println(0 || 3)   // => true"),
    ("?:", "Operators", "a ?: b",
     "The Elvis operator. Evaluates the left side once and yields it when Groovy-truthy, otherwise the right side. Unlike `||` it yields the *operand*, not a `Boolean`.",
     "println(null ?: \"d\")\nprintln(\"\" ?: \"d\")   // => d then d"),
    ("? :", "Operators", "cond ? a : b",
     "The ternary conditional. The condition goes through Groovy truth.",
     "println(true ? 1 : 2)   // => 1"),
    ("?.", "Operators", "recv?.member | recv?.method(args)",
     "Safe navigation. Yields `null` without dispatching when the receiver is `null`, so a chain of `?.` never raises `NullPointerException`.",
     "def x\nprintln(x?.size())   // => null"),
    ("*.", "Operators", "recv*.member | recv*.method(args)",
     "The spread-dot operator. Desugars to `recv.collect { it?.member }` — including the safe navigation, which is why a `null` element spreads to `null` rather than raising.",
     "println([1, 2, 3]*.toString())   // => [1, 2, 3]"),
    ("[ ]", "Operators", "recv[index]",
     "Subscript, lowered to the `GINDEX` builtin. A list index past the end yields `null` and a negative index counts from the end; a String subscript yields a one-character String and raises past the end; a map subscript is a key read. On a class instance it dispatches `getAt`.",
     "println([1, 2, 3][-1])\nprintln(\"hello\"[1])   // => 3 then e"),
    ("..", "Operators", "a..b",
     "An inclusive range — a `groovy.lang.Range` object, so `(0..3).class.simpleName` is `IntRange` and printing one shows `0..3`. Being a `java.util.List` in Groovy, every list method and operator applies to it as well.",
     "println(0..3)   // => [0, 1, 2, 3]"),
    ("..<", "Operators", "a..<b",
     "A half-open range: the endpoint is excluded, so `(0..<3)` enumerates `0, 1, 2` and prints `0..<3`.",
     "println(0..<3)   // => [0, 1, 2]"),
    ("++", "Operators", "x++ | ++x",
     "Increment. Postfix yields the value before the update, prefix the value after. On a class instance it dispatches `plus` — not Groovy's `next`, which is a deliberate divergence.",
     "def i = 0\nprintln(i++)\nprintln(i)   // => 0 then 1"),
    ("--", "Operators", "x-- | --x",
     "Decrement, mirroring `++`; on a class instance it dispatches `minus` rather than Groovy's `previous`.",
     "def i = 2\nprintln(--i)   // => 1"),
    ("=", "Operators", "target = expr",
     "Assignment to a local, a script binding, a field, a property (`GSETPROP`), or a subscript. A property write to a name the class chain never declared raises `MissingPropertyException` rather than growing the object.",
     "def x = 1\nx = 2\nprintln(x)   // => 2"),
    ("+=", "Operators", "target += expr",
     "Compound addition — the `+` lowering with a load before and a store after. Inside a class body a bare field target routes through the property builtins.",
     "def x = 1\nx += 2\nprintln(x)   // => 3"),
    ("-=", "Operators", "target -= expr",
     "Compound subtraction.",
     "def x = 5\nx -= 2\nprintln(x)   // => 3"),
    ("*=", "Operators", "target *= expr",
     "Compound multiplication.",
     "def x = 3\nx *= 4\nprintln(x)   // => 12"),
    ("/=", "Operators", "target /= expr",
     "Compound division. Reuses the `GDIV` builtin, so `7 /= 2` yields a `BigDecimal`.",
     "def x = 7\nx /= 2\nprintln(x)   // => 3.5"),
    ("%=", "Operators", "target %= expr",
     "Compound remainder, reusing the guarded `%` lowering.",
     "def x = 7\nx %= 3\nprintln(x)   // => 1"),
    (".", "Operators", "recv.member | recv.method(args)",
     "Member access. A call routes through the `GMETHOD` builtin and a read through `GPROP`; on a class instance a read prefers a user `getX()` getter over the raw field.",
     "println(\"abc\".size())   // => 3"),
    ("->", "Operators", "{ params -> body }",
     "The closure parameter separator. Its presence — even with an empty list — is what makes the parameter list explicit and suppresses the implicit `it`.",
     "println([1, 2].collect { n -> n * 10 })   // => [10, 20]"),
    ("|", "Operators", "catch (A | B name)",
     "The multi-catch alternative separator. This is its only meaning: groovyrs has no bitwise-or operator.",
     "try { 1 / 0 } catch (IOException | ArithmeticException e) { println(\"caught\") }   // => caught"),
    ("@", "Operators", "@Name",
     "The annotation marker. Annotations are lexed and skipped before a class or member declaration; groovyrs attaches no meaning to any of them.",
     "class A { @Override def toString() { \"A\" } }\nprintln(new A())   // => A"),
    ("label:", "Operators", "name: loop",
     "A statement label. Only a loop is a useful target, and only `break label` / `continue label` read it; naming a label no enclosing loop carries is a compile error.",
     "outer: for (i in 0..3) { if (i == 1) break outer; print(i) }   // => 0"),
    // ── Script Commands — host::GPRINTLN / host::GPRINT ──
    ("println", "Script Commands", "println([value])",
     "Print a Groovy-formatted value and a trailing newline, then yield `null`. Parentheses are optional. A class instance renders through its `toString()`; a list renders as `[1, 2, 3]` and a map as `[a:1]`.",
     "println(\"hi\")\nprintln([a: 1])   // => hi then [a:1]"),
    ("print", "Script Commands", "print([value])",
     "Print a Groovy-formatted value with no trailing newline, then yield `null`.",
     "print(\"a\"); print(\"b\")   // => ab"),

    // ── GDK — Any Receiver — host::dispatch_call / dispatch_method ──
    ("size", "GDK — Any Receiver", "value.size() -> Integer",
     "The element count: characters for a String, elements for a list, entries for a map. groovyrs answers `size()` on *every* receiver and yields `0` for a scalar, where Groovy raises `MissingMethodException` on, say, an `Integer`.",
     "println(\"ab\".size())\nprintln([1, 2, 3].size())   // => 2 then 3"),
    ("getClass", "GDK — Any Receiver", "value.getClass() -> Class",
     "The `java.lang.Class` handle for the receiver, answered before the per-type table so it works on everything. `null.getClass()` answers `NullObject`'s class rather than raising, as in Groovy.",
     "println(1.5.getClass().getName())   // => java.math.BigDecimal"),
    ("toString", "GDK — Any Receiver", "value.toString() -> String",
     "The receiver rendered the way `println` renders it. A class instance uses its own `toString()` when it declares one; a throwable renders as `qualified.Name: message`.",
     "println([1, 2].toString())   // => [1, 2]"),
    ("equals", "GDK — Any Receiver", "value.equals(other) -> Boolean",
     "Value equality. Modeled explicitly only on a `null` receiver (`null.equals(null)` is true); on a class instance a user `equals` is dispatched, and on other receivers it is the `==` comparison.",
     "def x\nprintln(x.equals(null))   // => true"),
    ("call", "GDK — Any Receiver", "closure.call([args]) -> Object",
     "Invoke a closure explicitly. `clo(args)` and `clo.call(args)` reach the same code path; calling `call` on a non-closure falls through to ordinary GDK dispatch.",
     "def f = { a -> a * 2 }\nprintln(f.call(4))   // => 8"),

    // ── GDK — String — host::dispatch_method ──
    ("length", "GDK — String", "string.length() -> Integer",
     "The character count of a String — Unicode characters, not bytes.",
     "println(\"héllo\".length())   // => 5"),
    ("toUpperCase", "GDK — String", "string.toUpperCase() -> String",
     "The receiver uppercased. Uses Rust's Unicode-aware case mapping rather than a JDK locale.",
     "println(\"abc\".toUpperCase())   // => ABC"),
    ("toLowerCase", "GDK — String", "string.toLowerCase() -> String",
     "The receiver lowercased.",
     "println(\"ABC\".toLowerCase())   // => abc"),
    ("trim", "GDK — String", "string.trim() -> String",
     "The receiver with leading and trailing whitespace removed.",
     "println(\"  x  \".trim() + \"!\")   // => x!"),
    ("reverse", "GDK — String", "string.reverse() -> String",
     "The receiver's characters in reverse order.",
     "println(\"abc\".reverse())   // => cba"),
    ("isEmpty", "GDK — String", "string.isEmpty() -> Boolean",
     "True when the String has no characters. This is a length test, not a whitespace test.",
     "println(\"\".isEmpty())   // => true"),
    ("contains", "GDK — String", "string.contains(needle) -> Boolean",
     "True when the rendered argument occurs as a substring. The argument is rendered first, so a non-String argument is compared by its printed form — where Apache Groovy raises `MissingMethodException` for one.",
     "println(\"abc\".contains(\"bc\"))   // => true"),
    ("toInteger", "GDK — String", "string.toInteger() -> Integer",
     "Parse the *trimmed* text as an integer, range-checked to 32 bits. A failed parse — including one that overflows `Integer` — raises `NumberFormatException` naming the text.",
     "println(\"42\".toInteger() + 1)   // => 43"),
    ("toLong", "GDK — String", "string.toLong() -> Long",
     "Parse the trimmed text as a 64-bit integer. Unlike `toInteger` it accepts values outside the 32-bit range.",
     "println(\"2147483648\".toLong())   // => 2147483648"),
    ("toDouble", "GDK — String", "string.toDouble() -> Double",
     "Parse the text the way `Double.parseDouble` does: surrounding whitespace and a trailing `d`/`f` are allowed, as are `Infinity` and `NaN` — but not Rust's extra `inf`/`nan` spellings or a hex literal.",
     "println(\"1.5\".toDouble() * 2)   // => 3.0"),
    ("toFloat", "GDK — String", "string.toFloat() -> Float",
     "Identical to `toDouble` — groovyrs has one IEEE type, so there is no narrowing to 32-bit precision.",
     "println(\"0.25\".toFloat())   // => 0.25"),
    ("toBigDecimal", "GDK — String", "string.toBigDecimal() -> BigDecimal",
     "`new BigDecimal(text.trim())`. The failure carries `BigDecimal`'s own character-level diagnostics, including the message-less `NumberFormatException` an empty string produces.",
     "println(\"1.50\".toBigDecimal())   // => 1.50"),

    // ── GDK — List — host::dispatch_method ──
    ("isEmpty", "GDK — List", "list.isEmpty() -> Boolean",
     "True when the list has no elements.",
     "println([].isEmpty())   // => true"),
    ("contains", "GDK — List", "list.contains(value) -> Boolean",
     "True when some element matches. groovyrs compares the *rendered* forms, so `[1, 2].contains(\"1\")` is true where Groovy's `equals`-based test is false.",
     "println([1, 2, 3].contains(2))   // => true"),
    ("get", "GDK — List", "list.get(index) -> Object",
     "The element at `index`. Unlike the `[i]` subscript this is the raw JDK call: any out-of-range index raises `IndexOutOfBoundsException` instead of yielding `null`, and a negative index does not wrap.",
     "println([10, 20].get(1))   // => 20"),
    ("reverse", "GDK — List", "list.reverse() -> List",
     "A new list with the elements in reverse order. The receiver is not mutated.",
     "println([1, 2, 3].reverse())   // => [3, 2, 1]"),
    ("join", "GDK — List", "list.join([separator]) -> String",
     "Render each element the way `println` does and join them with the separator, which defaults to the empty string.",
     "println([1, 2, 3].join(\"-\"))   // => 1-2-3"),

    // ── GDK — List Closure Methods — host::dispatch_iteration ──
    ("each", "GDK — List Closure Methods", "list.each { it -> … } -> List",
     "Run the closure once per element for its side effects and yield the receiver. A `throw` inside the closure stops the iteration so the exception reaches the caller's handler.",
     "[1, 2].each { print(it) }   // => 12"),
    ("eachWithIndex", "GDK — List Closure Methods", "list.eachWithIndex { it, i -> … } -> List",
     "Like `each`, but the closure also receives the element's 0-based index.",
     "[9, 8].eachWithIndex { v, i -> println(\"$i:$v\") }   // => 0:9 then 1:8"),
    ("collect", "GDK — List Closure Methods", "list.collect { it -> … } -> List",
     "Map each element through the closure into a new list of the same length.",
     "println([1, 2, 3].collect { it * 2 })   // => [2, 4, 6]"),
    ("findAll", "GDK — List Closure Methods", "list.findAll { it -> … } -> List",
     "Keep the elements for which the closure's result is Groovy-truthy.",
     "println([1, 2, 3].findAll { it > 1 })   // => [2, 3]"),
    ("find", "GDK — List Closure Methods", "list.find { it -> … } -> Object",
     "The first element the closure accepts, or `null` when none does.",
     "println([1, 2, 3].find { it > 1 })   // => 2"),
    ("inject", "GDK — List Closure Methods", "list.inject([seed]) { acc, val -> … } -> Object",
     "Fold left. With a seed the fold starts there; without one it seeds with the first element and starts at the second. An empty list with no seed yields `null`.",
     "println([1, 2, 3].inject(0) { a, b -> a + b })   // => 6"),
    ("sum", "GDK — List Closure Methods", "list.sum([seed]) | list.sum { it -> … } -> Object",
     "Add the elements, or the closure's results, using the `+` numeric tower — so a String element concatenates and a list element appends. An empty list with no seed yields `null`.",
     "println([1, 2, 3].sum())\nprintln([1, 2, 3].sum { it * 2 })   // => 6 then 12"),
    ("sort", "GDK — List Closure Methods", "list.sort([true]) | list.sort { it -> key } | list.sort { a, b -> … } -> List",
     "Order the elements naturally, by a one-parameter key closure, or by a two-parameter comparator. The call always builds a new list; when the receiver is a plain variable the compiler writes the result back, which is what makes it look in-place. `sort(false)` asks for a copy and suppresses the write-back.",
     "println([3, 1, 2].sort())   // => [1, 2, 3]"),
    ("unique", "GDK — List Closure Methods", "list.unique([true]) | list.unique { it -> key } -> List",
     "Drop later duplicates, keeping source order. Comparison is the same natural-or-closure ordering `sort` uses, and it writes back to a variable receiver the same way.",
     "println([1, 1, 2].unique())   // => [1, 2]"),
    ("max", "GDK — List Closure Methods", "list.max([{ it -> key }]) -> Object",
     "The greatest element, by natural order or by the closure's key.",
     "println([3, 1, 2].max())   // => 3"),
    ("min", "GDK — List Closure Methods", "list.min([{ it -> key }]) -> Object",
     "The least element, by natural order or by the closure's key.",
     "println([3, 1, 2].min())   // => 1"),
    ("groupBy", "GDK — List Closure Methods", "list.groupBy { it -> key } -> Map",
     "A map from each closure result to the sublist of elements that produced it, with keys in first-seen order. The key is the rendered form of the closure's result.",
     "println([1, 2, 3].groupBy { it % 2 })   // => [1:[1, 3], 0:[2]]"),
    ("countBy", "GDK — List Closure Methods", "list.countBy { it -> key } -> Map",
     "A map from each closure result to how many elements produced it, keys in first-seen order.",
     "println([1, 2, 3, 4].countBy { it % 2 })   // => [1:2, 0:2]"),
    ("findIndexValues", "GDK — List Closure Methods", "list.findIndexValues([from]) { it -> … } -> List",
     "Every index whose element the closure accepts, where `findIndexOf` answers only the first. An optional leading argument is the index to start from.",
     "println([1, 2, 3].findIndexValues { it > 1 })   // => [1, 2]"),

    // ── GDK — Closure — host::closure_combinator ──
    ("curry", "GDK — Closure", "closure.curry(args…) -> Closure",
     "A closure with `args` bound to the *leading* parameters; the rest are supplied at call time.",
     "def add = { a, b -> a + b }\nprintln add.curry(1)(2)   // => 3"),
    ("rcurry", "GDK — Closure", "closure.rcurry(args…) -> Closure",
     "As `curry`, but the bound values go at the *end* of the supplied arguments.",
     "def sub = { a, b -> a - b }\nprintln sub.rcurry(1)(5)   // => 4"),
    ("ncurry", "GDK — Closure", "closure.ncurry(n, args…) -> Closure",
     "As `curry`, but the bound values are spliced in at index `n` of the argument list.",
     "def three = { a, b, c -> \"$a$b$c\" }\nprintln three.ncurry(1, \"X\")(\"a\", \"c\")   // => aXc"),
    ("memoize", "GDK — Closure", "closure.memoize() -> Closure",
     "A closure that runs the body once per distinct argument list and answers the recorded result thereafter. The cache is keyed by the rendered arguments and shared by every holder of the handle.",
     "def n = 0\ndef f = { n++; it * 2 }.memoize()\nf(3); f(3)\nprintln n   // => 1"),
    ("andThen", "GDK — Closure", "closure.andThen(other) -> Closure",
     "Composition running the receiver first and feeding its result to `other` — the same as Groovy's `>>`.",
     "def inc = { it + 1 }\nprintln inc.andThen { it * 2 }(3)   // => 8"),
    ("compose", "GDK — Closure", "closure.compose(other) -> Closure",
     "Composition running `other` first — the same as Groovy's `<<`.",
     "def inc = { it + 1 }\nprintln inc.compose { it * 2 }(3)   // => 7"),

    // ── GDK — Map — host::dispatch_method ──
    ("isEmpty", "GDK — Map", "map.isEmpty() -> Boolean",
     "True when the map has no entries.",
     "println([:].isEmpty())   // => true"),
    ("containsKey", "GDK — Map", "map.containsKey(key) -> Boolean",
     "True when the map holds the rendered key.",
     "println([a: 1].containsKey(\"a\"))   // => true"),
    ("get", "GDK — Map", "map.get(key) -> Object",
     "The value stored under the rendered key, or `null` when absent. Unlike `List.get` this never raises.",
     "println([a: 1].get(\"a\"))   // => 1"),
    ("keySet", "GDK — Map", "map.keySet() -> List",
     "The keys in insertion order. groovyrs yields a plain list rather than a `Set` view, so it is ordered and allows subscripting.",
     "println([a: 1, b: 2].keySet())   // => [a, b]"),
    ("keys", "GDK — Map", "map.keys() -> List",
     "A groovyrs synonym for `keySet`. Apache Groovy has no `Map.keys()` — this is an extension, not a port.",
     "println([a: 1, b: 2].keys())   // => [a, b]"),
    ("values", "GDK — Map", "map.values() -> List",
     "The values in insertion order, as a plain list.",
     "println([a: 1, b: 2].values())   // => [1, 2]"),
    ("subMap", "GDK — Map", "map.subMap(keys) | map.subMap(k1, k2, …) -> Map",
     "The entries for the listed keys, in the *receiver's* order. A key the map does not hold is dropped rather than read as `null`.",
     "println([a: 1, b: 2].subMap([\"a\"]))   // => [a:1]"),
    ("spread", "GDK — Map", "map.spread() -> Map",
     "A shallow copy of the map.",
     "println([a: 1].spread())   // => [a:1]"),
    ("intersect", "GDK — Map", "map.intersect(other) -> Map",
     "The entries `other` holds identically — same key *and* same value. `map.minus(other)` (`map - other`) is the complement.",
     "println([a: 1, b: 2].intersect([a: 1]))   // => [a:1]"),
    ("iterator", "GDK — Map", "map.iterator() -> Iterator",
     "A live cursor over the map's entries. Also defined on a list, a range and a `String`; `next()` advances the shared handle and raises `NoSuchElementException` past the end.",
     "println([a: 1].iterator().next())   // => a=1"),
    // ── GDK — Map Closure Methods — host::dispatch_map_iteration ──
    ("each", "GDK — Map Closure Methods", "map.each { k, v -> … } | map.each { entry -> … } -> Map",
     "Run the closure once per entry and yield the receiver. A two-parameter closure receives `(key, value)`; a one-parameter closure receives one `Map.Entry` — the closure's declared parameter count decides, as in Groovy.",
     "[a: 1, b: 2].each { k, v -> println(\"$k$v\") }   // => a1 then b2"),
    ("eachWithIndex", "GDK — Map Closure Methods", "map.eachWithIndex { k, v, i -> … } -> Map",
     "Like `each`, with the entry's 0-based index appended to the closure's arguments.",
     "[a: 1].eachWithIndex { k, v, i -> print(\"$i:$k\") }   // => 0:a"),
    ("collect", "GDK — Map Closure Methods", "map.collect { k, v -> … } -> List",
     "Map each entry through the closure. The result is a *list* of the closure's results, not a map — matching Groovy.",
     "println([a: 1, b: 2].collect { k, v -> k + v })   // => [a1, b2]"),
    ("findAll", "GDK — Map Closure Methods", "map.findAll { k, v -> … } -> Map",
     "Keep the entries the closure accepts, as a new map in source order.",
     "println([a: 1, b: 2].findAll { k, v -> v > 1 })   // => [b:2]"),
    ("find", "GDK — Map Closure Methods", "map.find { k, v -> … } -> Map.Entry",
     "The first accepted entry as a `Map.Entry` (which prints as `key=value`), or `null` when none matches.",
     "println([a: 1, b: 2].find { k, v -> v > 1 })   // => b=2"),
    ("any", "GDK — Map Closure Methods", "map.any { k, v -> … } -> Boolean",
     "True when the closure accepts at least one entry; stops at the first acceptance.",
     "println([a: 1, b: 2].any { k, v -> v > 1 })   // => true"),
    ("every", "GDK — Map Closure Methods", "map.every { k, v -> … } -> Boolean",
     "True when the closure accepts every entry; stops at the first rejection.",
     "println([a: 1, b: 2].every { k, v -> v > 0 })   // => true"),
    ("count", "GDK — Map Closure Methods", "map.count { k, v -> … } -> Integer",
     "How many entries the closure accepts.",
     "println([a: 1, b: 2].count { k, v -> v > 1 })   // => 1"),
    ("countBy", "GDK — Map Closure Methods", "map.countBy { k, v -> key } -> Map",
     "A map from each closure result to how many entries produced it, keys in first-seen order.",
     "println([a: 1, b: 2].countBy { k, v -> v % 2 })   // => [1:1, 0:1]"),
    ("withDefault", "GDK — Map Closure Methods", "map.withDefault { key -> … } -> Map",
     "A copy whose missing-key read runs the closure with that key, *stores* the result under it, and answers it — Groovy's `MapWithDefault`, which grows as it is read.",
     "def m = [:].withDefault { 0 }\nprintln m[\"z\"]\nprintln m   // => 0 then [z:0]"),
    ("groupBy", "GDK — Map Closure Methods", "map.groupBy { k, v -> key } -> Map",
     "A map from each closure result to the *sub-map* of entries that produced it, keys in first-seen order.",
     "println([a: 1, b: 2].groupBy { k, v -> v % 2 })   // => [1:[a:1], 0:[b:2]]"),
    ("inject", "GDK — Map Closure Methods", "map.inject(seed) { acc, entry -> … } -> Object",
     "Fold over the entries, passing the accumulator and one `Map.Entry`. Unlike the list form the seed is required here.",
     "println([a: 1, b: 2].inject(0) { a, e -> a + e.value })   // => 3"),
    ("sort", "GDK — Map Closure Methods", "map.sort([{ entry -> key }]) -> Map",
     "A new map ordered by key, or by the closure's key over each entry. Unlike `List.sort` this never mutates or writes back to the receiver, which is also Groovy's behaviour.",
     "println([b: 1, a: 2].sort())   // => [a:2, b:1]"),
    ("max", "GDK — Map Closure Methods", "map.max { entry -> key } -> Map.Entry",
     "The entry whose closure key is greatest, as a `Map.Entry`. The closure is required.",
     "println([a: 2, b: 1].max { it.value })   // => a=2"),
    ("min", "GDK — Map Closure Methods", "map.min { entry -> key } -> Map.Entry",
     "The entry whose closure key is least, as a `Map.Entry`.",
     "println([a: 2, b: 1].min { it.value })   // => b=1"),

    // ── GDK — BigDecimal — host::dispatch_method ──
    ("toString", "GDK — BigDecimal", "decimal.toString() -> String",
     "The decimal rendered with its own scale preserved, so `1.50` keeps both fraction digits and `2.5e7` prints as `2.5E+7`.",
     "println(1.50.toString())   // => 1.50"),
    ("abs", "GDK — BigDecimal", "decimal.abs() -> BigDecimal",
     "The magnitude, keeping the receiver's scale.",
     "println((-1.5).abs())   // => 1.5"),
    ("negate", "GDK — BigDecimal", "decimal.negate() -> BigDecimal",
     "The receiver with its sign flipped.",
     "println(1.5.negate())   // => -1.5"),
    ("toBigDecimal", "GDK — BigDecimal", "decimal.toBigDecimal() -> BigDecimal",
     "The receiver itself — the identity conversion, provided so a script can call it uniformly on a String or a decimal.",
     "println(1.50.toBigDecimal())   // => 1.50"),
    ("intValue", "GDK — BigDecimal", "decimal.intValue() -> Integer",
     "Truncate toward zero to an integer. This discards the fraction rather than rounding it.",
     "println(1.99.intValue())   // => 1"),
    ("longValue", "GDK — BigDecimal", "decimal.longValue() -> Long",
     "Truncate toward zero to a 64-bit integer — the same value `intValue` yields, since groovyrs holds one integer width.",
     "println(1.99.longValue())   // => 1"),
    ("toInteger", "GDK — BigDecimal", "decimal.toInteger() -> Integer",
     "A truncating conversion, identical to `intValue`.",
     "println(2.9.toInteger())   // => 2"),
    ("toLong", "GDK — BigDecimal", "decimal.toLong() -> Long",
     "A truncating conversion, identical to `longValue`.",
     "println(2.9.toLong())   // => 2"),
    ("round", "GDK — BigDecimal", "decimal.round() -> Integer",
     "Round to the nearest integer — the one conversion here that does not truncate.",
     "println(1.50.round())   // => 2"),
    ("doubleValue", "GDK — BigDecimal", "decimal.doubleValue() -> Double",
     "Convert to an IEEE double, giving up the exact decimal scale.",
     "println(1.50.doubleValue())   // => 1.5"),
    ("toDouble", "GDK — BigDecimal", "decimal.toDouble() -> Double",
     "Identical to `doubleValue`.",
     "println(0.25.toDouble())   // => 0.25"),
    ("floatValue", "GDK — BigDecimal", "decimal.floatValue() -> Float",
     "Identical to `doubleValue` — groovyrs has one IEEE width, so there is no narrowing to 32-bit precision.",
     "println(1.25.floatValue())   // => 1.25"),
    ("toFloat", "GDK — BigDecimal", "decimal.toFloat() -> Float",
     "Identical to `floatValue`.",
     "println(1.25.toFloat())   // => 1.25"),

    // ── GDK — java.lang.Class — host::dispatch_method ──
    ("getName", "GDK — java.lang.Class", "clazz.getName() -> String",
     "The fully-qualified class name. A script-declared class answers its bare name, as in Groovy's default package.",
     "println(\"x\".getClass().getName())   // => java.lang.String"),
    ("getTypeName", "GDK — java.lang.Class", "clazz.getTypeName() -> String",
     "The qualified name — the same string `getName` yields.",
     "println([1].getClass().getTypeName())   // => java.util.ArrayList"),
    ("getCanonicalName", "GDK — java.lang.Class", "clazz.getCanonicalName() -> String",
     "The qualified name again. groovyrs models no nested or array classes, the two cases where the JDK's canonical name differs from `getName`.",
     "println(1.getClass().getCanonicalName())   // => java.lang.Integer"),
    ("getSimpleName", "GDK — java.lang.Class", "clazz.getSimpleName() -> String",
     "The last dot-separated segment of the qualified name.",
     "println(\"x\".getClass().getSimpleName())   // => String"),

    // ── GDK — Map.Entry — host::dispatch_method ──
    ("getKey", "GDK — Map.Entry", "entry.getKey() -> String",
     "The entry's key. Map keys are held as strings, so this always yields a String.",
     "println([a: 1].find { k, v -> true }.getKey())   // => a"),
    ("getValue", "GDK — Map.Entry", "entry.getValue() -> Object",
     "The entry's value, of whatever type the map holds.",
     "println([a: 1].find { k, v -> true }.getValue())   // => 1"),

    // ── Properties — host::dispatch_property ──
    ("size", "Properties", "value.size -> Integer",
     "The count property on a String, list, or map. On a map this rule is *not* reached: a map's property access is only a key read, so `[a: 1].size` is `null` while `[size: 9].size` is `9`.",
     "println([1, 2].size)\nprintln([a: 1].size)   // => 2 then null"),
    ("length", "Properties", "value.length -> Integer",
     "A synonym of the `size` property, with the same map exception.",
     "println(\"abc\".length)   // => 3"),
    ("class", "Properties", "value.class -> Class",
     "The `getClass()` property, available on every value but a map — where it is a key read like any other name. On a class instance a declared field named `class` still wins.",
     "println(\"a\".class.simpleName)   // => String"),
    ("name", "Properties", "clazz.name -> String",
     "The qualified name of a `java.lang.Class` handle, through Groovy's getter-to-property rule.",
     "println(1.5.class.name)   // => java.math.BigDecimal"),
    ("typeName", "Properties", "clazz.typeName -> String",
     "The qualified name of a class handle — the property form of `getTypeName()`.",
     "println(\"x\".class.typeName)   // => java.lang.String"),
    ("canonicalName", "Properties", "clazz.canonicalName -> String",
     "The qualified name of a class handle — the property form of `getCanonicalName()`.",
     "println([1].class.canonicalName)   // => java.util.ArrayList"),
    ("simpleName", "Properties", "clazz.simpleName -> String",
     "The last segment of a class handle's qualified name.",
     "println([1].class.simpleName)   // => ArrayList"),
    ("key", "Properties", "entry.key -> String",
     "The key of a `Map.Entry`, the property form of `getKey()`.",
     "println([a: 1].find { k, v -> true }.key)   // => a"),
    ("value", "Properties", "entry.value -> Object",
     "The value of a `Map.Entry`, the property form of `getValue()`.",
     "println([a: 1].max { it.value }.value)   // => 1"),

    // ── Class Member Hooks — names the runtime calls on an instance ──
    ("toString", "Class Member Hooks", "def toString() { … }",
     "Declare it and `println`, string concatenation, GString interpolation, and `list.join` all render the instance through it.",
     "class P { def x = 1; String toString() { \"P($x)\" } }\nprintln(new P())   // => P(1)"),
    ("equals", "Class Member Hooks", "def equals(other) { … }",
     "Decides `==` and `!=` for instances of the class — but only when the class does not also define `compareTo`, which takes precedence.",
     "class P { def equals(o) { true } }\nprintln(new P() == new P())   // => true"),
    ("compareTo", "Class Member Hooks", "def compareTo(other) { … }",
     "Models `Comparable`. It drives `<`, `>`, `<=`, `>=`, `<=>`, `sort`, `unique`, `max`, `min` — and equality too, where `==` becomes `compareTo(…) == 0`.",
     "class P { def n = 1; def compareTo(o) { 0 } }\nprintln(new P() == new P())   // => true"),
    ("asBoolean", "Class Member Hooks", "def asBoolean() { … }",
     "Defines the instance's Groovy truth for `if`, `while`, `&&`, `||`, `?:`, and `findAll`. An instance whose class does not declare it is always truthy.",
     "class P { def asBoolean() { false } }\nif (new P()) println(\"t\") else println(\"f\")   // => f"),
    ("getAt", "Class Member Hooks", "def getAt(index) { … }",
     "Defines the `[…]` subscript on the instance. The subscript is reported as a missing `getAt` when the class does not declare one.",
     "class P { def getAt(i) { i * 2 } }\nprintln(new P()[4])   // => 8"),
    ("plus", "Class Member Hooks", "def plus(other) { … }",
     "The `+` operator on the instance. `++` also dispatches here — groovyrs does not model Groovy's `next`.",
     "class P { def plus(o) { 42 } }\nprintln(new P() + 1)   // => 42"),
    ("minus", "Class Member Hooks", "def minus(other) { … }",
     "The `-` operator on the instance, and the operator `--` dispatches to.",
     "class P { def minus(o) { 7 } }\nprintln(new P() - 1)   // => 7"),
    ("multiply", "Class Member Hooks", "def multiply(other) { … }",
     "The `*` operator on the instance.",
     "class P { def multiply(o) { 6 } }\nprintln(new P() * 2)   // => 6"),
    ("div", "Class Member Hooks", "def div(other) { … }",
     "The `/` operator on the instance. It is resolved inside the `GDIV` builtin rather than through the numeric hook the other operators use.",
     "class P { def div(o) { 9 } }\nprintln(new P() / 1)   // => 9"),
    ("remainder", "Class Member Hooks", "def remainder(other) { … }",
     "The `%` operator on the instance. Groovy maps `%` to `remainder`, not to `mod` — verified against Apache Groovy 5.0.7.",
     "class P { def remainder(o) { 3 } }\nprintln(new P() % 2)   // => 3"),
    ("negative", "Class Member Hooks", "def negative() { … }",
     "Unary `-` on the instance.",
     "class P { def negative() { -5 } }\nprintln(-new P())   // => -5"),
    ("power", "Class Member Hooks", "def power(other) { … }",
     "Groovy's `**` operator method. The mapping exists in the runtime, but groovyrs's lexer has no `**` token, so nothing can reach this hook today.",
     "class P { def power(o) { 8 } }\nprintln(new P().power(3))   // => 8"),
    ("get<Name>", "Class Member Hooks", "def getName() { … }",
     "A getter. Reading the property `obj.name` prefers this method over the raw field, and calling `obj.getName()` falls back to the field when no such method exists. Note that a getter which reads its own backing field re-enters itself in the current build, so the body must not name the field it fronts.",
     "class P { def x = 1; def getX() { 99 } }\nprintln(new P().x)   // => 99"),
    ("set<Name>", "Class Member Hooks", "def setName(value) { … }",
     "A setter. Writing `obj.name = v` prefers this method over the raw field write. As with the getter, assigning to the backing field of the same name re-enters the setter in the current build, so write through a differently-named field.",
     "class P { def y = 0; def setX(v) { y = v * 2 } }\ndef p = new P()\np.x = 5\nprintln(p.y)   // => 10"),
    // ── Throwables — throwable::THROWABLES ──
    ("Throwable", "Throwables", "new Throwable([String message]) — java.lang",
     "The root of the modeled hierarchy and the only type that declares the `message` field; every descendant inherits it. `catch (Throwable e)` matches everything a script can throw.",
     "try { throw new Throwable(\"t\") } catch (Throwable e) { println(e) }   // => java.lang.Throwable: t"),
    ("Exception", "Throwables", "new Exception([String message]) — java.lang",
     "The checked-exception root, directly under `Throwable`. It does not match an `Error`, so `catch (Exception e)` will not catch an `AssertionError`.",
     "try { throw new Exception(\"x\") } catch (Exception e) { println(e.message) }   // => x"),
    ("Error", "Throwables", "new Error([String message]) — java.lang",
     "The unrecoverable-condition root, the sibling of `Exception` under `Throwable`. `AssertionError` lives beneath it.",
     "try { throw new Error(\"e\") } catch (Throwable t) { println(t) }   // => java.lang.Error: e"),
    ("RuntimeException", "Throwables", "new RuntimeException([String message]) — java.lang",
     "The unchecked-exception root under `Exception`; most of the throwables the runtime raises itself descend from it.",
     "try { throw new RuntimeException(\"r\") } catch (Exception e) { println(e) }   // => java.lang.RuntimeException: r"),
    ("IllegalArgumentException", "Throwables", "new IllegalArgumentException([String message]) — java.lang",
     "An argument outside a method's contract. Its one modeled subtype is `NumberFormatException`.",
     "try { throw new IllegalArgumentException(\"bad\") } catch (RuntimeException e) { println(e.message) }   // => bad"),
    ("NumberFormatException", "Throwables", "new NumberFormatException([String message]) — java.lang",
     "Raised by `String.toInteger`, `toLong`, `toDouble`, `toFloat`, and `toBigDecimal` when the text does not parse. The `toBigDecimal` path can raise the message-less form, which prints with no `: message` suffix.",
     "try { \"x\".toInteger() } catch (NumberFormatException e) { println(e.message) }   // => For input string: \"x\""),
    ("IllegalStateException", "Throwables", "new IllegalStateException([String message]) — java.lang",
     "An operation attempted in the wrong state. groovyrs never raises it itself; it is here for scripts to throw.",
     "try { throw new IllegalStateException(\"bad\") } catch (Exception e) { println(e) }   // => java.lang.IllegalStateException: bad"),
    ("ArithmeticException", "Throwables", "new ArithmeticException([String message]) — java.lang",
     "Raised by division and remainder with a zero divisor. The message is `Division by zero` for a decimal divisor, `/ by zero` for an integer one, and `Division undefined` for `0/0`.",
     "try { 1 / 0 } catch (ArithmeticException e) { println(e.message) }   // => Division by zero"),
    ("NullPointerException", "Throwables", "new NullPointerException([String message]) — java.lang",
     "Raised by a method call, property read, or property write on `null` — except `toString`, `equals`, and `getClass`, which `null` answers.",
     "try { null.foo() } catch (NullPointerException e) { println(e.message) }   // => Cannot invoke method foo() on null object"),
    ("IndexOutOfBoundsException", "Throwables", "new IndexOutOfBoundsException([String message]) — java.lang",
     "Raised by `List.get` for any index outside the list. The `[i]` subscript does not raise it — a list subscript past the end yields `null`.",
     "try { [1].get(5) } catch (IndexOutOfBoundsException e) { println(e.message) }   // => Index 5 out of bounds for length 1"),
    ("ArrayIndexOutOfBoundsException", "Throwables", "new ArrayIndexOutOfBoundsException([String message]) — java.lang",
     "Raised by a negative subscript whose magnitude exceeds the receiver's length, on a list or a String.",
     "try { [1][-5] } catch (ArrayIndexOutOfBoundsException e) { println(e.message) }   // => Negative array index [-5] too large for array size 1"),
    ("StringIndexOutOfBoundsException", "Throwables", "new StringIndexOutOfBoundsException([String message]) — java.lang",
     "Raised by a String subscript past the end, naming the half-open range it tried to read.",
     "try { \"ab\"[9] } catch (StringIndexOutOfBoundsException e) { println(e.message) }   // => Range [9, 10) out of bounds for length 2"),
    ("UnsupportedOperationException", "Throwables", "new UnsupportedOperationException([String message]) — java.lang",
     "An operation a type does not support. Registered for scripts to throw; the runtime does not raise it.",
     "try { throw new UnsupportedOperationException(\"no\") } catch (RuntimeException e) { println(e.message) }   // => no"),
    ("ClassCastException", "Throwables", "new ClassCastException([String message]) — java.lang",
     "A bad cast. groovyrs has no cast operator, so only a script raises this.",
     "try { throw new ClassCastException(\"c\") } catch (Exception e) { println(e) }   // => java.lang.ClassCastException: c"),
    ("InterruptedException", "Throwables", "new InterruptedException([String message]) — java.lang",
     "Thread interruption, directly under `Exception`. A groovyrs script runs on one thread, so this exists for hierarchy fidelity.",
     "try { throw new InterruptedException(\"i\") } catch (Exception e) { println(e) }   // => java.lang.InterruptedException: i"),
    ("CloneNotSupportedException", "Throwables", "new CloneNotSupportedException([String message]) — java.lang",
     "Registered so the checked-exception branch under `Exception` matches the JDK's shape; groovyrs models no `clone`.",
     "try { throw new CloneNotSupportedException(\"c\") } catch (Exception e) { println(e) }   // => java.lang.CloneNotSupportedException: c"),
    ("AssertionError", "Throwables", "new AssertionError([String message]) — java.lang",
     "Raised by the `assert cond : message` form. It sits under `Error`, so `catch (Exception e)` does not catch it.",
     "try { assert false : \"nope\" } catch (AssertionError e) { println(e.message) }   // => nope. Expression: false"),
    ("IOException", "Throwables", "new IOException([String message]) — java.io",
     "The I/O root. groovyrs performs no file I/O, so only a script raises it.",
     "try { throw new IOException(\"x\") } catch (Exception e) { println(e) }   // => java.io.IOException: x"),
    ("FileNotFoundException", "Throwables", "new FileNotFoundException([String message]) — java.io",
     "A missing file, under `IOException`.",
     "try { throw new FileNotFoundException(\"f\") } catch (IOException e) { println(e) }   // => java.io.FileNotFoundException: f"),
    ("NoSuchElementException", "Throwables", "new NoSuchElementException([String message]) — java.util",
     "An exhausted iterator or an absent element, under `RuntimeException`.",
     "try { throw new NoSuchElementException(\"n\") } catch (RuntimeException e) { println(e) }   // => java.util.NoSuchElementException: n"),
    ("ConcurrentModificationException", "Throwables", "new ConcurrentModificationException([String message]) — java.util",
     "A collection mutated during iteration. groovyrs iterates over a materialised copy and so never raises it itself.",
     "try { throw new ConcurrentModificationException(\"c\") } catch (RuntimeException e) { println(e) }   // => java.util.ConcurrentModificationException: c"),
    ("GroovyRuntimeException", "Throwables", "new GroovyRuntimeException([String message]) — groovy.lang",
     "The root of Groovy's own runtime failures, under `RuntimeException`. Both dispatch failures below descend from it.",
     "try { throw new GroovyRuntimeException(\"g\") } catch (RuntimeException e) { println(e) }   // => groovy.lang.GroovyRuntimeException: g"),
    ("MissingMethodException", "Throwables", "new MissingMethodException([String message]) — groovy.lang",
     "Raised whenever a `recv.method(args)` combination falls outside the modeled GDK, rather than mis-running. The message names the method, the receiver's class, and the argument types.",
     "try { \"\".bar() } catch (MissingMethodException e) { println(e.message) }"),
    ("MissingPropertyException", "Throwables", "new MissingPropertyException([String message]) — groovy.lang",
     "Raised by a read of an unmodeled property, and by a write to a field the class chain never declared — groovyrs does not grow an object on assignment.",
     "try { 5.zz } catch (MissingPropertyException e) { println(e.message) }   // => No such property: zz for class: java.lang.Integer"),
    ("PowerAssertionError", "Throwables", "new PowerAssertionError([String message]) — org.codehaus.groovy.runtime.powerassert",
     "What a bare `assert` raises. It overrides `toString` to print `Assertion failed:` followed by the source text with each recorded sub-expression's value laid out under the column it occupied.",
     "try { assert 1 == 2 } catch (AssertionError e) { println(e.getClass().simpleName) }   // => PowerAssertionError"),

    // ── Type Names — compiler::BUILTIN_TYPE_NAMES ──
    ("Object", "Type Names", "value instanceof Object",
     "True for every non-null value. A qualified name is matched on its last segment, so `java.lang.Object` behaves identically.",
     "println(1 instanceof Object)   // => true"),
    ("GroovyObject", "Type Names", "value instanceof GroovyObject",
     "True for every non-null value — groovyrs treats it as a synonym of `Object`. Apache Groovy restricts it to objects that implement the interface, and answers false for a `String`.",
     "println(\"x\" instanceof GroovyObject)   // => true"),
    ("String", "Type Names", "value instanceof String",
     "True for a String value.",
     "println(\"x\" instanceof String)   // => true"),
    ("CharSequence", "Type Names", "value instanceof CharSequence",
     "True for a String value — the same test `String` performs.",
     "println(\"x\" instanceof CharSequence)   // => true"),
    ("GString", "Type Names", "value instanceof GString",
     "True for a String value. An interpolated literal collapses to an ordinary String at runtime, so groovyrs cannot distinguish a GString from a plain one here.",
     "def n = 1\nprintln(\"$n\" instanceof GString)   // => true"),
    ("Integer", "Type Names", "value instanceof Integer",
     "True for an integer value. groovyrs holds one 64-bit integer type, so this does not range-check to 32 bits.",
     "println(1 instanceof Integer)   // => true"),
    ("Long", "Type Names", "value instanceof Long",
     "True for an integer value — indistinguishable from `Integer` in groovyrs, where Apache Groovy answers false because an integer literal is an `Integer`.",
     "println(1 instanceof Long)   // => true"),
    ("Short", "Type Names", "value instanceof Short",
     "True for an integer value, with no width check; Apache Groovy answers false because an integer literal is an `Integer`.",
     "println(1 instanceof Short)   // => true"),
    ("Byte", "Type Names", "value instanceof Byte",
     "True for an integer value, with no width check; Apache Groovy answers false because an integer literal is an `Integer`.",
     "println(1 instanceof Byte)   // => true"),
    ("BigDecimal", "Type Names", "value instanceof BigDecimal",
     "True for an IEEE double value. This is coarser than the value model: a decimal literal such as `1.5` lives on the host heap and is *not* matched here, while the `d`-suffixed `1.5d` is — the exact inverse of what Apache Groovy answers for the same two literals.",
     "println(1.5d instanceof BigDecimal)\nprintln(1.5 instanceof BigDecimal)   // => true then false"),
    ("Double", "Type Names", "value instanceof Double",
     "True for an IEEE double value.",
     "println(1.5d instanceof Double)   // => true"),
    ("Float", "Type Names", "value instanceof Float",
     "True for an IEEE double value — groovyrs has one IEEE width.",
     "println(1.5d instanceof Float)   // => true"),
    ("BigInteger", "Type Names", "value instanceof BigInteger",
     "True for an IEEE double value. groovyrs models no arbitrary-precision integer, so this name resolves with the other floating types rather than with `Integer` — Apache Groovy answers false for a double.",
     "println(1.5d instanceof BigInteger)   // => true"),
    ("Number", "Type Names", "value instanceof Number",
     "True for an integer or an IEEE double value.",
     "println(1 instanceof Number)   // => true"),
    ("Boolean", "Type Names", "value instanceof Boolean",
     "True for a boolean value.",
     "println(true instanceof Boolean)   // => true"),
    ("List", "Type Names", "value instanceof List",
     "True for a list value — including a range, since a Groovy `Range` is a `java.util.List`.",
     "println([1] instanceof List)   // => true"),
    ("ArrayList", "Type Names", "value instanceof ArrayList",
     "True for a list value. `ArrayList` is also the class name a list reports from `getClass()`.",
     "println((0..2) instanceof ArrayList)   // => true"),
    ("Collection", "Type Names", "value instanceof Collection",
     "True for a list value. groovyrs models no `Set`, so `Collection` and `List` answer identically.",
     "println([1] instanceof Collection)   // => true"),
    ("Iterable", "Type Names", "value instanceof Iterable",
     "True for a list value. Note that a String and a map are both iterable in a `for (x in …)` loop yet do not answer true here.",
     "println([1] instanceof Iterable)   // => true"),
    ("Map", "Type Names", "value instanceof Map",
     "True for a map value, whether it is a fusevm hash or an insertion-ordered host map.",
     "println([a: 1] instanceof Map)   // => true"),
    ("LinkedHashMap", "Type Names", "value instanceof LinkedHashMap",
     "True for a map value — and the accurate one, since every groovyrs map preserves insertion order.",
     "println([a: 1] instanceof LinkedHashMap)   // => true"),
    ("HashMap", "Type Names", "value instanceof HashMap",
     "True for a map value, even though groovyrs never produces an unordered map.",
     "println([a: 1] instanceof HashMap)   // => true"),

    // ── Inline Rust — the rust { … } FFI desugar ──
    ("rust", "Inline Rust", "rust { … }",
     "An inline Rust block. The parser rewrites it to a `__rust_compile(\"<base64>\", line)` call, which hands the body to fusevm's FFI at run time: it is compiled to a cdylib once and cached on disk under `FUSEVM_FFI_DIR`, so a second run of the same block is a cache hit.",
     "rust {\n    #[no_mangle]\n    pub extern \"C\" fn add(a: i64, b: i64) -> i64 { a + b }\n}\nprintln(add(2, 3))   // => 5"),
    ("__rust_compile", "Inline Rust", "__rust_compile(base64Body, line)",
     "The desugar target a `rust { … }` block lowers to — the `GFFI_COMPILE` builtin. It is not meant to be written by hand; a script that does so is calling the FFI compiler directly.",
     "// emitted by the parser for every `rust { … }` block"),
];

/// The corpus, exposed for offline doc generation (`gen-docs`). The tuple is
/// `(name, chapter, signature, doc, example)`.
pub fn corpus() -> &'static [(
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
)] {
    CORPUS
}

/// Open document text keyed by URI, kept current from the sync notifications so
/// hover can look up the identifier under the cursor.
type Docs = HashMap<String, String>;

/// Entry point for `groovy --lsp`.
pub fn run() -> Result<(), String> {
    spawn_orphan_guard();
    let (conn, io_threads) = Connection::stdio();
    let (init_id, _params) = conn
        .initialize_start()
        .map_err(|e| format!("lsp initialize: {e}"))?;
    let init_result = serde_json::json!({
        "capabilities": server_capabilities(),
        "serverInfo": { "name": "groovyrs", "version": env!("CARGO_PKG_VERSION") },
    });
    conn.sender
        .send(Response::new_ok(init_id, init_result).into())
        .map_err(|e| format!("lsp send: {e}"))?;

    let mut docs: Docs = HashMap::new();
    for msg in &conn.receiver {
        match msg {
            Message::Request(req) => {
                if conn
                    .handle_shutdown(&req)
                    .map_err(|e| format!("lsp shutdown: {e}"))?
                {
                    break;
                }
                dispatch_request(&conn, &docs, req);
            }
            Message::Notification(not) => dispatch_notification(&conn, &mut docs, not),
            Message::Response(_) => {}
        }
    }
    drop(conn);
    io_threads.join().map_err(|_| "lsp io join".to_string())?;
    Ok(())
}

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                ..Default::default()
            },
        )),
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(false),
            ..Default::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        ..Default::default()
    }
}

fn handle<P, R>(conn: &Connection, req: Request, f: impl FnOnce(P) -> R)
where
    P: serde::de::DeserializeOwned,
    R: serde::Serialize,
{
    let method = req.method.clone();
    let id = req.id.clone();
    match req.extract::<P>(&method) {
        Ok((id, params)) => {
            let value = serde_json::to_value(f(params)).unwrap_or(serde_json::Value::Null);
            let _ = conn.sender.send(Response::new_ok(id, value).into());
        }
        Err(ExtractError::JsonError { error, .. }) => {
            let _ = conn.sender.send(
                Response::new_err(id, ErrorCode::InvalidParams as i32, error.to_string()).into(),
            );
        }
        Err(ExtractError::MethodMismatch(_)) => unreachable!("method matched before extract"),
    }
}

fn dispatch_request(conn: &Connection, docs: &Docs, req: Request) {
    match req.method.as_str() {
        Completion::METHOD => handle(conn, req, |_p: CompletionParams| completions()),
        HoverRequest::METHOD => handle(conn, req, |p: HoverParams| hover(docs, &p)),
        _ => {
            let _ = conn.sender.send(
                Response::new_err(req.id, ErrorCode::MethodNotFound as i32, "unhandled".into())
                    .into(),
            );
        }
    }
}

fn dispatch_notification(conn: &Connection, docs: &mut Docs, not: lsp_server::Notification) {
    match not.method.as_str() {
        DidOpenTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidOpenTextDocumentParams>(not.params) {
                let uri = p.text_document.uri;
                docs.insert(uri.as_str().to_string(), p.text_document.text.clone());
                publish_diagnostics(conn, &uri, &p.text_document.text);
            }
        }
        DidChangeTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidChangeTextDocumentParams>(not.params) {
                if let Some(change) = p.content_changes.into_iter().last() {
                    let uri = p.text_document.uri;
                    docs.insert(uri.as_str().to_string(), change.text.clone());
                    publish_diagnostics(conn, &uri, &change.text);
                }
            }
        }
        DidCloseTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidCloseTextDocumentParams>(not.params) {
                let uri = p.text_document.uri;
                docs.remove(uri.as_str());
                publish_diagnostics(conn, &uri, "");
            }
        }
        _ => {}
    }
}

fn completions() -> CompletionResponse {
    let items = CORPUS
        .iter()
        .map(|(name, chapter, sig, doc, _example)| CompletionItem {
            label: name.to_string(),
            kind: Some(completion_kind(chapter)),
            detail: Some((*sig).to_string()),
            documentation: Some(lsp_types::Documentation::String((*doc).to_string())),
            ..Default::default()
        })
        .collect();
    CompletionResponse::Array(items)
}

/// The completion-item kind a corpus chapter maps to. Chapters are grouped by
/// prefix so a new "GDK — …" chapter needs no change here.
fn completion_kind(chapter: &str) -> CompletionItemKind {
    match chapter {
        "Reserved Words" | "Contextual Keywords" => CompletionItemKind::KEYWORD,
        "Literals and Literal Forms" => CompletionItemKind::VALUE,
        "Operators" => CompletionItemKind::OPERATOR,
        "Script Commands" | "Inline Rust" => CompletionItemKind::FUNCTION,
        "Properties" => CompletionItemKind::PROPERTY,
        "Class Member Hooks" => CompletionItemKind::METHOD,
        "Throwables" | "Type Names" => CompletionItemKind::CLASS,
        _ => CompletionItemKind::METHOD,
    }
}

/// Hover: look up the identifier under the cursor in the corpus and render its
/// chapter, signature, doc, and example. A name that appears in several
/// chapters (`each` on a list and on a map) renders every match. Falls back to
/// a short banner when the cursor is not on a known name.
fn hover(docs: &Docs, params: &HoverParams) -> Hover {
    let pos = params.text_document_position_params.position;
    let uri = params
        .text_document_position_params
        .text_document
        .uri
        .as_str();
    let word = docs
        .get(uri)
        .and_then(|text| word_at(text, pos))
        .unwrap_or_default();

    let matches: Vec<&(&str, &str, &str, &str, &str)> =
        CORPUS.iter().filter(|(name, ..)| *name == word).collect();

    let body = if matches.is_empty() {
        "**groovyrs** — Groovy on the fusevm bytecode VM + Cranelift JIT.".to_string()
    } else {
        let mut out = String::new();
        for (name, chapter, sig, doc, example) in matches {
            out.push_str(&format!(
                "**`{name}`** — _{chapter}_\n\n```groovy\n{sig}\n```\n\n{doc}\n\n```groovy\n{example}\n```\n\n"
            ));
        }
        out.trim_end().to_string()
    };

    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: body,
        }),
        range: None,
    }
}

/// Extract the identifier (`[A-Za-z0-9_$]+`) spanning the given position, if any.
fn word_at(text: &str, pos: Position) -> Option<String> {
    let line = text.lines().nth(pos.line as usize)?;
    let chars: Vec<char> = line.chars().collect();
    let col = (pos.character as usize).min(chars.len());
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';

    let mut start = col;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    Some(chars[start..end].iter().collect())
}

fn publish_diagnostics(conn: &Connection, uri: &Uri, text: &str) {
    let params = PublishDiagnosticsParams {
        uri: uri.clone(),
        diagnostics: compute_diagnostics(text),
        version: None,
    };
    let not = lsp_server::Notification::new(PublishDiagnostics::METHOD.to_string(), params);
    let _ = conn.sender.send(not.into());
}

/// Parse the whole document with the runtime's own parser; a syntax error maps
/// to a single diagnostic on the line named in its `… on line N` / `… line N`
/// suffix.
fn compute_diagnostics(text: &str) -> Vec<Diagnostic> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    match crate::parser::parse(text) {
        Ok(_) => Vec::new(),
        Err(e) => {
            let line = parse_error_line(&e).saturating_sub(1);
            vec![Diagnostic {
                range: Range {
                    start: Position { line, character: 0 },
                    end: Position {
                        line,
                        character: 200,
                    },
                },
                severity: Some(DiagnosticSeverity::ERROR),
                message: e,
                ..Default::default()
            }]
        }
    }
}

/// Extract the (1-based) line number from a groovyrs lexer/parser error, which
/// embeds it as `… line N`. Defaults to line 1 when no such marker is present.
fn parse_error_line(e: &str) -> u32 {
    e.rsplit_once("line ")
        .and_then(|(_, rest)| rest.split(|c: char| !c.is_ascii_digit()).next())
        .filter(|n| !n.is_empty())
        .and_then(|n| n.parse().ok())
        .unwrap_or(1)
}

/// Exit if reparented to pid 1 (the editor died) so we never leak.
fn spawn_orphan_guard() {
    std::thread::spawn(|| {
        #[cfg(target_os = "linux")]
        // SAFETY: prctl(PR_SET_PDEATHSIG, ...) only registers a signal disposition.
        unsafe {
            libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGKILL as libc::c_ulong,
                0,
                0,
                0,
            );
        }
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            // SAFETY: getppid takes no arguments and never fails.
            if unsafe { libc::getppid() } == 1 {
                std::process::exit(0);
            }
        }
    });
}

//! Integration tests: run `.groovy` scripts through the built `groovy` binary
//! and assert their stdout. Expected outputs are frozen after verifying them
//! byte-for-byte against Apache Groovy 5.0.x, so the suite is self-contained —
//! CI needs no JVM or Groovy install.

use std::process::Command;

/// Run a Groovy source string through the `groovy` binary and return
/// (stdout, ok).
fn run(src: &str) -> (String, bool) {
    let (out, _, ok) = run_full(src);
    (out, ok)
}

/// [`run`], keeping **stderr** as well — which is where a fault's class and
/// message go (`groovyrs: <throwable>: <reason>`).
///
/// A test that asserts only `!ok` cannot tell one failure from another: a
/// dispatch miss, a parse error and a panic all exit non-zero, so the throwable
/// the test is named for is never actually checked. Pinning stderr is what makes
/// those tests measure the thing they claim to.
fn run_full(src: &str) -> (String, String, bool) {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("groovyrs_test_{}.groovy", fasthash(src)));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_groovy"))
        .arg(&path)
        .output()
        .expect("spawn groovy");
    let _ = std::fs::remove_file(&path);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

fn fasthash(s: &str) -> u64 {
    // A tiny FNV-1a so concurrent tests use distinct temp files.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[test]
fn prints_a_string_literal() {
    let (out, ok) = run(r#"println("hello")"#);
    assert!(ok);
    assert_eq!(out, "hello\n");
}

#[test]
fn println_command_form_without_parens() {
    let (out, _) = run(r#"println "no parens""#);
    assert_eq!(out, "no parens\n");
}

#[test]
fn optional_semicolons_newline_terminated() {
    // No semicolons anywhere — newlines terminate statements.
    let (out, _) = run("int a = 3\nint b = 4\nprintln a + b");
    assert_eq!(out, "7\n");
}

#[test]
fn integer_arithmetic_and_precedence() {
    let (out, _) = run("println 2 + 3 * 4 - 1");
    assert_eq!(out, "13\n");
}

#[test]
fn groovy_division_promotes_to_decimal() {
    // Groovy divides two ints as BigDecimal: exact stays integral, else decimal.
    let (out, _) = run("println 7 / 2\nprintln 4 / 2\nprintln 9 / 2");
    assert_eq!(out, "3.5\n2\n4.5\n");
}

#[test]
fn modulo() {
    let (out, _) = run("println 7 % 3");
    assert_eq!(out, "1\n");
}

#[test]
fn string_plus_int_concatenation() {
    let (out, _) = run(r#"def x = 21; println "x=" + x * 2"#);
    assert_eq!(out, "x=42\n");
}

#[test]
fn boolean_prints_groovy_style() {
    let (out, _) = run("println 3 > 2\nprintln 1 == 2");
    assert_eq!(out, "true\nfalse\n");
}

#[test]
fn decimal_prints_with_trailing_point_zero() {
    let (out, _) = run("def d = 3.0\nprintln d");
    assert_eq!(out, "3.0\n");
}

#[test]
fn null_literal_prints_null() {
    let (out, _) = run("println null");
    assert_eq!(out, "null\n");
}

#[test]
fn if_elseif_else_single_line() {
    let (out, _) = run(r#"def n = 5
if (n < 0) println "neg" else if (n == 0) println "zero" else println "pos""#);
    assert_eq!(out, "pos\n");
}

#[test]
fn while_loop_accumulates() {
    let (out, _) = run("int sum = 0\nint i = 1\nwhile (i <= 5) { sum += i; i++ }\nprintln sum");
    assert_eq!(out, "15\n");
}

#[test]
fn c_style_for_counts() {
    let (out, _) = run("for (int i = 0; i < 3; i++) { println i }");
    assert_eq!(out, "0\n1\n2\n");
}

#[test]
fn for_in_inclusive_range() {
    let (out, _) = run("for (i in 1..3) println i");
    assert_eq!(out, "1\n2\n3\n");
}

#[test]
fn for_in_half_open_range() {
    let (out, _) = run("for (i in 0..<3) println i");
    assert_eq!(out, "0\n1\n2\n");
}

#[test]
fn for_in_range_over_a_variable_endpoint() {
    let (out, _) = run("def n = 3\nfor (i in 1..n) println i");
    assert_eq!(out, "1\n2\n3\n");
}

#[test]
fn break_and_continue() {
    let (out, _) = run("for (i in 0..10) { if (i == 2) continue; if (i == 4) break; println i }");
    assert_eq!(out, "0\n1\n3\n");
}

#[test]
fn short_circuit_and_or() {
    let (out, _) = run("int x = 5\nprintln x > 0 && x < 10\nprintln x < 0 || x == 5");
    assert_eq!(out, "true\ntrue\n");
}

#[test]
fn unary_negation_and_not() {
    let (out, _) = run("int x = 3\nprintln(-x)\nprintln(!(x > 5))");
    assert_eq!(out, "-3\ntrue\n");
}

#[test]
fn compound_division_assignment() {
    let (out, _) = run("def x = 10\nx /= 4\nprintln x");
    assert_eq!(out, "2.5\n");
}

#[test]
fn print_without_newline() {
    let (out, _) = run(r#"print "a"; print "b"; println "c""#);
    assert_eq!(out, "abc\n");
}

#[test]
fn fizzbuzz_first_five_with_range() {
    let (out, _) = run(r#"for (i in 1..5) {
  if (i % 15 == 0) println "FizzBuzz"
  else if (i % 3 == 0) println "Fizz"
  else if (i % 5 == 0) println "Buzz"
  else println i
}"#);
    assert_eq!(out, "1\n2\nFizz\n4\nBuzz\n");
}

#[test]
fn utf8_string_literal_survives() {
    let (out, _) = run(r#"println "café — ☕""#);
    assert_eq!(out, "café — ☕\n");
}

#[test]
fn single_quoted_string() {
    let (out, _) = run("println 'plain string'");
    assert_eq!(out, "plain string\n");
}

#[test]
fn user_function_with_params_and_explicit_return() {
    let (out, ok) = run("def add(a, b) { return a + b }\nprintln add(2, 3)");
    assert!(ok);
    assert_eq!(out, "5\n");
}

#[test]
fn user_function_implicit_last_expression_return() {
    // Groovy returns the value of the last evaluated expression with no `return`.
    let (out, _) = run("def sq(x) { x * x }\nprintln sq(7)");
    assert_eq!(out, "49\n");
}

#[test]
fn recursion_is_frame_local() {
    // Recursion is only sound if each call frame has its own `n`; a global would
    // clobber. Factorial exercises the frame-slot ABI.
    let (out, _) =
        run("def fact(n) {\n  if (n <= 1) return 1\n  return n * fact(n - 1)\n}\nprintln fact(5)");
    assert_eq!(out, "120\n");
}

#[test]
fn mutual_recursion_resolves_forward_references() {
    let src = "def isEven(n) { if (n == 0) return true; return isOdd(n - 1) }\n\
               def isOdd(n) { if (n == 0) return false; return isEven(n - 1) }\n\
               println isEven(10)";
    let (out, _) = run(src);
    assert_eq!(out, "true\n");
}

#[test]
fn function_locals_do_not_leak_across_calls() {
    // Each invocation's `total` is a fresh frame slot; a shared global would sum
    // across the two calls.
    let src = "def sumTo(n) {\n  def total = 0\n  for (i in 1..n) total += i\n  return total\n}\n\
               println sumTo(3)\nprintln sumTo(3)";
    let (out, _) = run(src);
    assert_eq!(out, "6\n6\n");
}

#[test]
fn function_reads_script_binding() {
    // A bare (undeclared) assignment is a script binding, visible inside methods.
    let (out, _) = run("x = 10\ndef f() { return x + 5 }\nprintln f()");
    assert_eq!(out, "15\n");
}

#[test]
fn postfix_increment_in_expression_position() {
    // `i++` yields the value before the update.
    let (out, _) = run("int i = 5\nprintln i++\nprintln i");
    assert_eq!(out, "5\n6\n");
}

#[test]
fn prefix_increment_in_expression_position() {
    // `++i` yields the value after the update.
    let (out, _) = run("int i = 5\nprintln ++i\nprintln i");
    assert_eq!(out, "6\n6\n");
}

#[test]
fn list_literal_prints_groovy_style() {
    let (out, ok) = run("println([1, 2, 3])");
    assert!(ok);
    assert_eq!(out, "[1, 2, 3]\n");
}

#[test]
fn empty_list_and_string_elements_unquoted() {
    let (out, _) = run("println([])\nprintln([\"a\", \"b\"])");
    assert_eq!(out, "[]\n[a, b]\n");
}

#[test]
fn nested_list_literal() {
    let (out, _) = run("println([[1, 2], [3, 4]])");
    assert_eq!(out, "[[1, 2], [3, 4]]\n");
}

#[test]
fn single_entry_map_literal() {
    // A single entry avoids HashMap ordering nondeterminism.
    let (out, _) = run("println([name: \"bob\"])");
    assert_eq!(out, "[name:bob]\n");
}

#[test]
fn empty_map_literal() {
    let (out, _) = run("println([:])");
    assert_eq!(out, "[:]\n");
}

#[test]
fn map_property_read() {
    let (out, _) = run("def m = [x: 5]\nprintln m.x");
    assert_eq!(out, "5\n");
}

#[test]
fn string_gdk_methods() {
    let src = "def s = \"Hello\"\nprintln s.length()\nprintln s.toUpperCase()\nprintln s.reverse()";
    let (out, _) = run(src);
    assert_eq!(out, "5\nHELLO\nolleH\n");
}

#[test]
fn size_method_on_string_list_and_map() {
    let (out, _) =
        run("println \"abc\".size()\nprintln [1, 2, 3, 4].size()\nprintln([k: 1].size())");
    assert_eq!(out, "3\n4\n1\n");
}

#[test]
fn list_method_chain_on_literal() {
    let (out, _) = run("println [10, 20, 30].contains(20)");
    assert_eq!(out, "true\n");
}

#[test]
fn unknown_method_is_an_error() {
    // A dispatch miss must fault, not mis-run — and the fault has to be the
    // `MissingMethodException` Groovy raises, naming the method and the
    // receiver class. `!ok` alone passes for a parse error or a panic too.
    let (out, err, ok) = run_full("def s = \"hi\"\nprintln s.frobnicate()");
    assert!(!ok, "unknown method should fault");
    assert_eq!(out, "");
    assert_eq!(
        err,
        "groovyrs: groovy.lang.MissingMethodException: No signature of method: \
         frobnicate for class: java.lang.String is applicable for argument \
         types: () values: []\n"
    );
}

// ── Closures ──────────────────────────────────────────────────────────────

#[test]
fn closure_implicit_it_direct_call() {
    // The canonical unlock: a single-implicit-parameter closure, called directly.
    let (out, ok) = run("def f = { it * 2 }\nprintln f(21)");
    assert!(ok);
    assert_eq!(out, "42\n");
}

#[test]
fn closure_two_params_direct_call() {
    let (out, _) = run("def add = { a, b -> a + b }\nprintln add(2, 3)");
    assert_eq!(out, "5\n");
}

#[test]
fn closure_dot_call_method() {
    // `.call(args)` invokes a closure value, same as calling it directly.
    let (out, _) = run("def inc = { it + 1 }\nprintln inc.call(41)");
    assert_eq!(out, "42\n");
}

#[test]
fn closure_captures_script_binding_by_reference() {
    // A closure sees later mutations of a captured script binding (capture is by
    // reference, not by value at creation time).
    let (out, _) = run("def base = 10\ndef f = { it + base }\nbase = 100\nprintln f(5)");
    assert_eq!(out, "105\n");
}

// ── GDK iteration with closures ─────────────────────────────────────────────

#[test]
fn collect_doubles_each_element() {
    // The canonical `collect` line.
    let (out, ok) = run("println([1, 2, 3].collect { it * 2 })");
    assert!(ok);
    assert_eq!(out, "[2, 4, 6]\n");
}

#[test]
fn find_all_keeps_matching_elements() {
    let (out, _) = run("println([1, 2, 3, 4].findAll { it % 2 == 0 })");
    assert_eq!(out, "[2, 4]\n");
}

#[test]
fn find_returns_first_match_else_null() {
    let (out, _) = run("println([1, 2, 3, 4].find { it > 2 })\nprintln([1, 2].find { it > 9 })");
    assert_eq!(out, "3\nnull\n");
}

#[test]
fn each_runs_closure_per_element() {
    let (out, _) = run("[1, 2, 3].each { println it * 10 }");
    assert_eq!(out, "10\n20\n30\n");
}

#[test]
fn inject_folds_with_initial_and_seedless_forms() {
    // Two-arg (explicit initial) and one-arg (seed = first element) forms.
    let (out, _) = run("println([1, 2, 3, 4].inject(0) { acc, v -> acc + v })\n\
         println([1, 2, 3, 4].inject { acc, v -> acc + v })");
    assert_eq!(out, "10\n10\n");
}

#[test]
fn sum_adds_list_elements() {
    let (out, _) = run("println([1, 2, 3, 4].sum())");
    assert_eq!(out, "10\n");
}

#[test]
fn collect_then_sum_chains() {
    // A closure-driven method feeding another method on its result.
    let (out, _) = run("println([1, 2, 3].collect { it * 2 }.sum())");
    assert_eq!(out, "12\n");
}

// ── First-class ranges ──────────────────────────────────────────────────────

#[test]
fn range_size_and_contains() {
    let (out, _) =
        run("def r = 0..5\nprintln r.size()\nprintln r.contains(3)\nprintln r.contains(9)");
    assert_eq!(out, "6\ntrue\nfalse\n");
}

#[test]
fn half_open_range_excludes_upper_bound() {
    let (out, _) = run("def r = 0..<5\nprintln r.size()\nprintln r.contains(5)");
    assert_eq!(out, "5\nfalse\n");
}

#[test]
fn range_each_and_collect() {
    let (out, _) = run("(0..3).each { print it }\nprintln()\nprintln((1..3).collect { it * it })");
    assert_eq!(out, "0123\n[1, 4, 9]\n");
}

// ── Ternary, Elvis, safe navigation ─────────────────────────────────────────

#[test]
fn ternary_selects_branch_on_truthiness() {
    let (out, _) = run("println(3 > 2 ? \"yes\" : \"no\")\nprintln(1 > 2 ? \"yes\" : \"no\")");
    assert_eq!(out, "yes\nno\n");
}

#[test]
fn elvis_coalesces_falsy_left() {
    // Truthy left kept; null and 0 (Groovy-falsy) fall through to the right.
    let (out, _) = run("def x = \"set\"\nprintln(x ?: \"default\")\n\
         def y = null\nprintln(y ?: \"default\")\n\
         println(0 ?: \"fallback\")");
    assert_eq!(out, "set\ndefault\nfallback\n");
}

#[test]
fn safe_navigation_short_circuits_on_null() {
    // `?.` yields null (no dispatch) on a null receiver, dispatches otherwise.
    let (out, _) = run("def x = null\nprintln(x?.toUpperCase())\n\
         def s = \"hi\"\nprintln(s?.toUpperCase())");
    assert_eq!(out, "null\nHI\n");
}

#[test]
fn unresolved_call_still_faults() {
    // A call through an undefined name (not a closure) remains an error, and
    // the diagnostic names the unresolved name rather than any other failure.
    let (out, err, ok) = run_full("println foo(1)");
    assert!(!ok, "calling an undefined non-closure must fault");
    assert_eq!(out, "");
    assert_eq!(err, "groovyrs: unresolved reference: foo\n");
}

// ── Nested-closure upvalue capture ──────────────────────────────────────────

#[test]
fn nested_closure_captures_outer_param() {
    // The canonical curry: the inner closure captures the outer closure's `x`
    // as an upvalue, so `adder(5)` returns a closure that adds 5.
    let (out, ok) =
        run("def adder = { x -> { y -> x + y } }\ndef add5 = adder(5)\nprintln add5(10)");
    assert!(ok);
    assert_eq!(out, "15\n");
}

#[test]
fn chained_call_applies_to_returned_closure() {
    // `f(a)(b)` must parse: the second argument list applies to the closure the
    // first call returned.
    let (out, ok) = run("def adder = { x -> { y -> x + y } }\nprintln adder(3)(4)");
    assert!(ok);
    assert_eq!(out, "7\n");
}

#[test]
fn three_level_curry() {
    let (out, _) = run("def f = { a -> { b -> { c -> a + b + c } } }\nprintln f(1)(2)(3)");
    assert_eq!(out, "6\n");
}

#[test]
fn closure_captures_enclosing_function_local() {
    // A closure returned from a function captures that function's local as an
    // upvalue, surviving the function's return.
    let src = "def makeCounter(start) {\n  def n = start\n  return { n + 1 }\n}\n\
               def c = makeCounter(10)\nprintln c()";
    let (out, _) = run(src);
    assert_eq!(out, "11\n");
}

#[test]
fn gdk_closure_captures_function_param() {
    // A `collect` closure inside a function captures the function parameter
    // `factor` — capture and GDK iteration compose.
    let (out, _) =
        run("def scale(factor, xs) { xs.collect { it * factor } }\nprintln scale(3, [1, 2, 3])");
    assert_eq!(out, "[3, 6, 9]\n");
}

// ── Classes ─────────────────────────────────────────────────────────────────

#[test]
fn class_fields_constructor_and_method() {
    let src = "class Point {\n  def x\n  def y\n  Point(a, b) { x = a; y = b }\n  \
               def dist() { return x * x + y * y }\n}\n\
               def p = new Point(3, 4)\nprintln p.x\nprintln p.y\nprintln p.dist()";
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "3\n4\n25\n");
}

#[test]
fn class_default_field_init_and_noarg_ctor() {
    // A field initializer runs at construction; a class with no constructor is
    // instantiated with `new C()`.
    let src = "class Counter {\n  def count = 0\n  def inc() { count = count + 1 }\n  \
               def get() { count }\n}\n\
               def c = new Counter()\nc.inc()\nc.inc()\nprintln c.get()\nprintln c.count";
    let (out, _) = run(src);
    assert_eq!(out, "2\n2\n");
}

#[test]
fn compound_assignment_to_field() {
    // `total += n` inside a method resolves `total` to `this.total`.
    let src = "class Acc {\n  def total = 0\n  def add(n) { total += n; return total }\n}\n\
               def a = new Acc()\nprintln a.add(5)\nprintln a.add(10)";
    let (out, _) = run(src);
    assert_eq!(out, "5\n15\n");
}

#[test]
fn this_reference_and_method_chaining() {
    // `this` is the receiver; returning it enables fluent chaining.
    let src = "class Box {\n  def v\n  def set(x) { this.v = x; return this }\n  \
               def show() { println this.v }\n}\n\
               def b = new Box()\nb.set(42).show()";
    let (out, _) = run(src);
    assert_eq!(out, "42\n");
}

#[test]
fn property_auto_getter_and_setter() {
    // Groovy synthesises `getX`/`setX` over a field.
    let src = "class P {\n  def x\n  P(v) { x = v }\n}\n\
               def p = new P(7)\nprintln p.getX()\np.setX(9)\nprintln p.x\np.x = 11\nprintln p.getX()";
    let (out, _) = run(src);
    assert_eq!(out, "7\n9\n11\n");
}

#[test]
fn user_getter_drives_property_read() {
    // A user `getArea()` is invoked by the bare property `.area`.
    let src = "class Sq {\n  def side\n  Sq(s) { side = s }\n  def getArea() { side * side }\n}\n\
               def s = new Sq(4)\nprintln s.area\nprintln s.getArea()";
    let (out, _) = run(src);
    assert_eq!(out, "16\n16\n");
}

#[test]
fn instance_prints_through_tostring() {
    let src = "class Rect {\n  def w\n  def h\n  Rect(w, h) { this.w = w; this.h = h }\n  \
               String toString() { \"Rect \" + w + \"x\" + h }\n}\nprintln new Rect(3, 4)";
    let (out, _) = run(src);
    assert_eq!(out, "Rect 3x4\n");
}

#[test]
fn method_calls_sibling_method_on_implicit_this() {
    // A bare call `dbl()` inside a method is an implicit `this.dbl()`.
    let src = "class Calc {\n  def base\n  Calc(b) { base = b }\n  def dbl() { base * 2 }\n  \
               def quad() { dbl() * 2 }\n}\ndef c = new Calc(5)\nprintln c.quad()";
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "20\n");
}

#[test]
fn closure_inside_method_captures_field_and_param() {
    // A `collect` closure inside a method sees the field `items` and the
    // parameter `f`.
    let src =
        "class Repo {\n  def items = [1, 2, 3]\n  def scaled(f) { items.collect { it * f } }\n}\n\
               def r = new Repo()\nprintln r.scaled(10)";
    let (out, _) = run(src);
    assert_eq!(out, "[10, 20, 30]\n");
}

#[test]
fn closure_in_method_captures_this_for_field_access() {
    // The GDK closure `{ it * factor }` reads the field `factor` — it must
    // capture the method's `this`, not resolve to its own slot 0.
    let src =
        "class Multiplier {\n  def factor = 3\n  def apply(xs) { xs.collect { it * factor } }\n}\n\
               println new Multiplier().apply([1, 2, 3])";
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "[3, 6, 9]\n");
}

#[test]
fn new_of_unknown_class_faults() {
    let (out, err, ok) = run_full("def x = new Nonexistent()");
    assert!(!ok, "constructing an unregistered class must fault");
    assert_eq!(out, "");
    assert_eq!(err, "groovyrs: unable to resolve class Nonexistent\n");
}

#[test]
fn unknown_method_on_instance_faults() {
    let src = "class C { def v = 1 }\ndef c = new C()\nprintln c.nope()";
    let (out, err, ok) = run_full(src);
    assert!(!ok, "an unknown method on an instance must fault");
    assert_eq!(out, "");
    // A script-declared class prints bare — no package — in the message.
    assert_eq!(
        err,
        "groovyrs: groovy.lang.MissingMethodException: No signature of method: \
         nope for class: C is applicable for argument types: () values: []\n"
    );
}

// ── Subscript / getAt ───────────────────────────────────────────────────────

#[test]
fn subscript_on_list_map_and_string() {
    let (out, _) =
        run("println([10, 20, 30][1])\nprintln([a: 1, b: 2][\"b\"])\nprintln(\"hello\"[1])");
    assert_eq!(out, "20\n2\ne\n");
}

#[test]
fn negative_list_index_counts_from_end() {
    let (out, _) = run("println([1, 2, 3][-1])");
    assert_eq!(out, "3\n");
}

#[test]
fn user_get_at_overload_drives_subscript() {
    // A user `getAt(i)` is invoked by `v[i]`.
    let src = "class Vec {\n  def x\n  def y\n  Vec(a, b) { x = a; y = b }\n  \
               def getAt(i) { i == 0 ? x : y }\n}\n\
               def v = new Vec(7, 9)\nprintln v[0]\nprintln v[1]";
    let (out, _) = run(src);
    assert_eq!(out, "7\n9\n");
}

// ── Insertion-ordered maps ──────────────────────────────────────────────────

#[test]
fn multi_entry_map_preserves_insertion_order() {
    // The round-2 gap: a multi-entry map prints in insertion order, not the
    // nondeterministic HashMap order.
    let (out, ok) = run("def m = [b: 1, a: 2, c: 3]\nprintln m");
    assert!(ok);
    assert_eq!(out, "[b:1, a:2, c:3]\n");
}

#[test]
fn map_key_assignment_appends_and_persists() {
    // `m.k = v` mutates the map in place (through its shared heap handle) and
    // appends a new key at the end.
    let (out, _) = run("def m = [b: 1, a: 2]\nm.c = 3\nprintln m\nprintln m.c");
    assert_eq!(out, "[b:1, a:2, c:3]\n3\n");
}

#[test]
fn list_plus_concatenates_and_appends() {
    // Groovy `+` on a list concatenates another list or appends a scalar.
    let (out, _) = run("println([1, 2] + [3, 4])\nprintln([1, 2] + 3)");
    assert_eq!(out, "[1, 2, 3, 4]\n[1, 2, 3]\n");
}

#[test]
fn map_plus_merges_right_wins() {
    // Map `+` merges; a duplicate key takes the right value, order preserved.
    let (out, _) = run("println([a: 1, b: 2] + [b: 9, c: 3])");
    assert_eq!(out, "[a:1, b:9, c:3]\n");
}

#[test]
fn map_size_and_contains_key() {
    let (out, _) =
        run("def m = [x: 1, y: 2, z: 3]\nprintln m.size()\nprintln m.containsKey(\"y\")\nprintln m.containsKey(\"q\")");
    assert_eq!(out, "3\ntrue\nfalse\n");
}

// ── Operator overloading (dispatched to user methods) ───────────────────────

#[test]
fn operator_overloads_arithmetic_on_instances() {
    // `+`/`-`/`*`/unary `-`/`%` dispatch to plus/minus/multiply/negative/
    // remainder on a user-class instance (Groovy 5's operator-method names).
    let src = r#"
class Vec {
    int x
    Vec(int v) { this.x = v }
    Vec plus(Vec o) { new Vec(x + o.x) }
    Vec minus(Vec o) { new Vec(x - o.x) }
    Vec multiply(int n) { new Vec(x * n) }
    Vec negative() { new Vec(-x) }
    Vec remainder(int n) { new Vec(x % n) }
    String toString() { "V(" + x + ")" }
}
def a = new Vec(10)
def b = new Vec(3)
println a + b
println a - b
println a * 2
println(-a)
println a % 3
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "V(13)\nV(7)\nV(20)\nV(-10)\nV(1)\n");
}

#[test]
fn div_operator_dispatches_user_div() {
    // Groovy `/` lowers to the GDIV builtin, which dispatches a user `div`
    // overload before falling back to numeric division.
    let src = r#"
class Scale {
    int v
    Scale(int v) { this.v = v }
    Scale div(int k) { new Scale(v - k) }
    String toString() { "S(" + v + ")" }
}
println new Scale(10) / 3
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "S(7)\n");
}

#[test]
fn comparable_class_drives_relational_operators() {
    // A class defining `compareTo` powers `<`, `>`, `<=`, `>=`.
    let src = r#"
class Vec {
    int x
    Vec(int v) { this.x = v }
    int compareTo(Vec o) { x - o.x }
    String toString() { "V(" + x + ")" }
}
def a = new Vec(10)
def b = new Vec(3)
println a > b
println a < b
println a >= b
println a <= b
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "true\nfalse\ntrue\nfalse\n");
}

#[test]
fn equals_method_drives_equality_and_is_null_safe() {
    // `==` on a class without `compareTo` uses its `equals`; an instance is
    // never `== null`.
    let src = r#"
class P {
    int x
    P(int x) { this.x = x }
    boolean equals(Object o) { o instanceof P && o.x == x }
    String toString() { "P(" + x + ")" }
}
println new P(1) == new P(1)
println new P(1) == new P(2)
def p = new P(5)
println p == p
println p == null
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "true\nfalse\ntrue\nfalse\n");
}

#[test]
fn comparable_drives_equality() {
    // A Comparable class (defines `compareTo`) compares equal via compareTo==0.
    let src = r#"
class Vec {
    int x
    Vec(int v) { this.x = v }
    int compareTo(Vec o) { x - o.x }
}
println new Vec(4) == new Vec(4)
println new Vec(4) == new Vec(9)
println new Vec(4) != new Vec(9)
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "true\nfalse\ntrue\n");
}

#[test]
fn spaceship_dispatches_compare_to_and_primitive_sign() {
    // `<=>` dispatches `compareTo` on an instance and yields the sign on
    // primitives; it also parses inside a compareTo body.
    let src = r#"
class Vec implements Comparable<Vec> {
    int x
    Vec(int v) { this.x = v }
    int compareTo(Vec o) { x <=> o.x }
    String toString() { "V(" + x + ")" }
}
def a = new Vec(10)
def b = new Vec(3)
println (a <=> b)
println (b <=> a)
println (a <=> new Vec(10))
println (1 <=> 2)
println (5 <=> 5)
println ("apple" <=> "banana")
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "1\n-1\n0\n-1\n0\n-1\n");
}

// ── Inheritance (extends / super / virtual dispatch / instanceof) ────────────

#[test]
fn subclass_super_constructor_and_inherited_field() {
    // `super(n)` runs the parent ctor; an inherited field is a real field.
    let src = r#"
class Animal {
    String name
    Animal(String n) { this.name = n }
    String kind() { "Animal" }
}
class Dog extends Animal {
    Dog(String n) { super(n) }
    String fetch() { name + " fetches" }
}
def d = new Dog("Rex")
println d.name
println d.kind()
println d.fetch()
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "Rex\nAnimal\nRex fetches\n");
}

#[test]
fn method_override_virtual_dispatch() {
    // A base-class method calling a virtual method resolves to the subclass
    // override (dynamic dispatch on the runtime class).
    let src = r#"
class Animal {
    String name
    Animal(String n) { this.name = n }
    String speak() { "..." }
    String describe() { name + " says " + speak() }
}
class Dog extends Animal {
    Dog(String n) { super(n) }
    String speak() { "Woof" }
}
def d = new Dog("Rex")
println d.speak()
println d.describe()
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "Woof\nRex says Woof\n");
}

#[test]
fn super_method_call_reaches_parent_implementation() {
    // `super.speak()` reaches the parent's implementation, skipping the override.
    let src = r#"
class Animal {
    String speak() { "..." }
}
class Dog extends Animal {
    String speak() { "Woof" }
}
class Puppy extends Dog {
    String speak() { "Yip (" + super.speak() + ")" }
}
println new Puppy().speak()
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "Yip (Woof)\n");
}

#[test]
fn instanceof_user_and_builtin_types() {
    // `instanceof` on user classes walks the superclass chain; built-in type
    // names are recognised; `null instanceof X` is false.
    let src = r#"
class A {}
class B extends A {}
def b = new B()
println b instanceof B
println b instanceof A
println (new A() instanceof B)
println ("x" instanceof String)
println (5 instanceof Integer)
println ([1, 2] instanceof List)
println (null instanceof A)
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "true\ntrue\nfalse\ntrue\ntrue\ntrue\nfalse\n");
}

#[test]
fn inherited_field_initializer_and_bare_method_call() {
    // Inherited field initializers run; a subclass method calls an inherited
    // method by bare name (resolved to `this` across the chain).
    let src = r#"
class Base {
    int a = 1
    int b = 2
    int sum() { a + b }
}
class Derived extends Base {
    int c = 10
    int total() { sum() + c }
}
def d = new Derived()
println d.a
println d.c
println d.sum()
println d.total()
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "1\n10\n3\n13\n");
}

#[test]
fn override_annotation_is_parsed_and_ignored() {
    // `@Override` (and other annotations) parse without effect.
    let src = r#"
class Animal {
    String speak() { "..." }
}
class Cat extends Animal {
    @Override
    String speak() { "Meow" }
}
println new Cat().speak()
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "Meow\n");
}

#[test]
fn three_level_inheritance_virtual_dispatch() {
    // A three-level chain: the most-derived override wins, and an inherited base
    // method dispatches virtually to it.
    let src = r#"
class Animal {
    String name
    Animal(String n) { this.name = n }
    String speak() { "..." }
    String describe() { name + " says " + speak() }
}
class Dog extends Animal {
    Dog(String n) { super(n) }
    String speak() { "Woof" }
}
class Puppy extends Dog {
    Puppy(String n) { super(n) }
    String speak() { "Yip (" + super.speak() + ")" }
}
def p = new Puppy("Bit")
println p.speak()
println p.describe()
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "Yip (Woof)\nBit says Yip (Woof)\n");
}

#[test]
fn subclass_inherits_tostring_when_not_overridden() {
    // A subclass with no `toString` prints through the inherited one.
    let src = r#"
class Base {
    int v = 7
    String toString() { "Base(" + v + ")" }
}
class Sub extends Base {
}
println new Sub()
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "Base(7)\n");
}

// ── BigDecimal value model ─────────────────────────────────────────────────
//
// An unsuffixed Groovy decimal is a `java.math.BigDecimal`, so its scale is part
// of the value. Each expectation below is the observed stdout of Apache Groovy
// 5.0.7; every one of them differs from what an f64 prints, which is exactly the
// class of regression these pin.

#[test]
fn decimal_literals_print_through_bigdecimal_tostring() {
    // An exponent literal carries a negative scale and prints in E+n form; a
    // literal's trailing zeros are kept; the plain window ends at 1e-7.
    let src = "println 2.5e7\nprintln 1e3\nprintln 100.00\nprintln 1e-7\nprintln 1.5E-5";
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "2.5E+7\n1E+3\n100.00\n1E-7\n0.000015\n");
}

#[test]
fn decimal_arithmetic_accumulates_scale() {
    // `+`/`-` take the larger scale (so adding 1 to an exponent literal lands at
    // scale 0 and prints no `.0` at all), `*` sums the scales.
    let src = "println 2.5e7 + 1\nprintln 1.10 + 2.20\nprintln 1.25 * 0\nprintln 2.50 * 4";
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "25000001\n3.30\n0.00\n10.00\n");
}

#[test]
fn decimal_addition_is_exact_not_binary_floating_point() {
    // The canonical f64 tell: 0.1 + 0.2 is 0.30000000000000004 in binary.
    let (out, _) = run("println 0.1 + 0.2\nprintln 1.1 * 1.1");
    assert_eq!(out, "0.3\n1.21\n");
}

#[test]
fn decimal_division_follows_groovys_scale_policy() {
    // Terminating quotients take Java's preferred scale (`1.000/4` keeps three
    // fraction digits); a non-terminating one is cut to ten.
    let src = "println 1.000 / 4\nprintln 2.5e7 / 1000\nprintln 1 / 3\nprintln 1.0 / 3.0";
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "0.250\n2.5E+4\n0.3333333333\n0.3333333333\n");
}

#[test]
fn decimal_remainder_takes_the_preferred_scale() {
    // `%` is `a - divideToIntegralValue(a, b) * b`, whose scale is neither
    // operand's — 657.87e+3 % 1.50 prints one fraction digit, not two.
    let (out, _) = run("println 1.5 % 0.4\nprintln 657.87e+3 % 1.50\nprintln 7 % 2.5");
    assert_eq!(out, "0.3\n0.0\n2.0\n");
}

#[test]
fn decimals_exceed_the_f64_range_and_precision() {
    // An f64 would return Infinity for the product and drop the trailing .5.
    let src = "println 1.5e300 * 1.5e300\nprintln 123456789012345678901234567890.5 + 1";
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "2.25E+600\n123456789012345678901234567891.5\n");
}

#[test]
fn suffixed_literals_stay_ieee_doubles() {
    // `d`/`f` keep the IEEE path: binary rounding, Infinity on divide-by-zero,
    // and `Double.toString` formatting (no `+` in the exponent).
    let src = "println 0.1d + 0.2d\nprintln 5.0d / 0.0d\nprintln 2.5e7d\nprintln 1e3d";
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "0.30000000000000004\nInfinity\n2.5E7\n1000.0\n");
}

#[test]
fn decimal_and_double_mix_widens_to_double_except_for_remainder() {
    // Groovy widens a BigDecimal/Double mix to Double — except `BigDecimal %
    // Double`, which reads the double's exact binary expansion and stays exact.
    let src = "println 1.0 + 1.0d\nprintln 1.5 % 0.555d";
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "2.0\n0.38999999999999990230037383298622444272041320800781250\n"
    );
}

#[test]
fn decimals_render_inside_collections_and_concatenation() {
    // A decimal keeps its scale wherever it is printed from.
    let src = "println([1.50, 2.0])\nprintln([a: 1.50])\nprintln(\"x\" + 1.50)";
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "[1.50, 2.0]\n[a:1.50]\nx1.50\n");
}

#[test]
fn decimal_comparison_ignores_scale() {
    // `1.5 == 1.50` is true (Groovy compares BigDecimals by value), and `<=>`
    // agrees, while `.toString()` still shows the scale each carries.
    let src =
        "println 1.5 == 1.50\nprintln 1.5 <=> 1.50\nprintln 1.50.toString()\nprintln 1.5 > 1.4";
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "true\n0\n1.50\ntrue\n");
}

#[test]
fn decimal_accumulates_across_a_loop() {
    // The literal is interned and the running total stays exact — an f64 would
    // print 0.30000000000000004 here.
    let src = "def t = 0.0\nfor (i in 1..3) { t = t + 0.1 }\nprintln t";
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "0.3\n");
}

#[test]
fn decimal_division_by_zero_aborts() {
    // Groovy raises ArithmeticException; groovyrs faults, which likewise aborts
    // the script rather than yielding an f64 Infinity.
    let (out, err, ok) = run_full("println 1.0 / 0\nprintln \"unreachable\"");
    assert!(
        !ok,
        "a zero divisor must abort rather than yield an Infinity"
    );
    assert_eq!(out, "");
    // `out == ""` alone is satisfied by any pre-`println` failure, including a
    // parse error on the literal. The class and message are the behaviour under
    // test: `/` promotes to `BigDecimal`, so this is `BigDecimal.divide`s
    // `Division by zero` and not the `/ by zero` an integral divide gives.
    assert_eq!(
        err,
        "groovyrs: java.lang.ArithmeticException: Division by zero\n"
    );
}

// ── Groovy truthiness ───────────────────────────────────────────────────────
//
// fusevm's own truth test reads every heap handle as true and the string "0" as
// false. Each expectation below is byte-verified against Apache Groovy 5.0.7.

#[test]
fn zero_decimal_is_false_but_nonzero_is_true() {
    // The bug this suite exists to pin: `0.0` is a host-heap BigDecimal, and a
    // naive handle-is-true rule takes the then-branch here.
    let src = r#"
if (0.0) println("t0") else println("f0")
if (0.00) println("t1") else println("f1")
if (0e0) println("t2") else println("f2")
if (1.50) println("t3") else println("f3")
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "f0\nf1\nf2\nt3\n");
}

#[test]
fn empty_map_is_false_and_string_zero_is_true() {
    // `[:]` is a host-heap ordered map (handle-is-true would take `t`), and
    // `"0"` is a non-empty string (a shell-flavoured rule would take `f`).
    let src = r#"
if ([:]) println("t0") else println("f0")
if ([a: 1]) println("t1") else println("f1")
if ("0") println("t2") else println("f2")
if ("") println("t3") else println("f3")
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "f0\nt1\nt2\nf3\n");
}

#[test]
fn unary_not_uses_groovy_truth() {
    let (out, ok) = run(r#"println(!0.00); println(!"0"); println(![:]); println(![1])"#);
    assert!(ok);
    assert_eq!(out, "true\nfalse\ntrue\nfalse\n");
}

#[test]
fn logical_operators_are_boolean_valued_but_elvis_is_operand_valued() {
    // Groovy's `&&`/`||` yield a Boolean; only Elvis yields the deciding operand.
    let src = r#"
println(5 && 3)
println(0 || 7)
println(0.0 ?: "fallback")
println(1.50 ?: "fallback")
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "true\ntrue\nfallback\n1.50\n");
}

#[test]
fn a_class_decides_its_own_truth_with_as_boolean() {
    let src = r#"
class Tank { def n; Tank(v) { n = v }; boolean asBoolean() { return n > 0 } }
if (new Tank(0)) println("t0") else println("f0")
if (new Tank(2)) println("t1") else println("f1")
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "f0\nt1\n");
}

#[test]
fn comparison_conditions_emit_no_truthiness_builtin() {
    // The perf contract of the truthiness fix: a comparison-shaped guard is
    // statically a Boolean, so it must still compile to the native `NumLt` +
    // `JumpIfFalse` pair the JIT traces — no host call in the loop condition.
    let disasm = groovyrs::disassemble("def n = 3\ndef i = 0\nwhile (i < n) { i++ }").unwrap();
    assert!(
        disasm.contains("NumLt"),
        "expected a native comparison, got:\n{disasm}"
    );
    assert!(
        !disasm.contains("CallBuiltin"),
        "a comparison-shaped `while` guard must emit no builtin call:\n{disasm}"
    );
}

// ── Exceptions ──────────────────────────────────────────────────────────────

#[test]
fn catch_matches_the_supertype_chain_and_finally_always_runs() {
    let src = r#"
def f(n) {
    try {
        if (n == 0) throw new IllegalStateException("zero")
        return "ok " + n
    } catch (RuntimeException e) {
        return "caught " + e.message
    } finally {
        println("fin " + n)
    }
}
println(f(1))
println(f(0))
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "fin 1\nok 1\nfin 0\ncaught zero\n");
}

#[test]
fn a_throwable_prints_as_its_qualified_name_and_message() {
    let src = r#"
println(new Exception("boom"))
println(new IOException("disk"))
println(new Exception())
println(new NumberFormatException("n") instanceof IllegalArgumentException)
println(new IOException("i") instanceof RuntimeException)
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "java.lang.Exception: boom\njava.io.IOException: disk\njava.lang.Exception\ntrue\nfalse\n"
    );
}

#[test]
fn a_script_class_can_extend_a_builtin_throwable() {
    let src = r#"
class ParseFailed extends Exception { ParseFailed(String m) { super(m) } }
try { throw new ParseFailed("line 7") } catch (Exception e) { println(e); println(e.message) }
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "ParseFailed: line 7\nline 7\n");
}

#[test]
fn finally_runs_before_an_early_return_out_of_a_loop() {
    // The escape path javac duplicates the cleanup block for: the `return` jumps
    // past the try's own normal exit, so the compiler must emit the `finally`
    // inline ahead of it.
    let src = r#"
def first(xs) {
    for (i in 0..<xs.size()) {
        try { if (xs[i] > 1) return "found " + xs[i] } finally { println("visit " + xs[i]) }
    }
    return "none"
}
println(first([1, 2, 3]))
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "visit 1\nvisit 2\nfound 2\n");
}

#[test]
fn a_rethrow_from_a_handler_still_runs_the_finally() {
    let src = r#"
try {
    try { throw new Exception("inner") }
    catch (Exception e) { throw new IllegalStateException("re:" + e.message) }
    finally { println("cleanup") }
} catch (Exception e) { println(e.message) }
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "cleanup\nre:inner\n");
}

#[test]
fn a_throw_out_of_a_closure_reaches_the_callers_handler() {
    // The closure body runs in a nested VM re-entry, so the host iteration loop
    // has to notice the pending exception and stop rather than keep iterating.
    let src = r#"
try {
    [1, 2, 3].each { if (it == 2) throw new IllegalStateException("stop"); println("saw " + it) }
} catch (Exception e) { println("escaped " + e.message) }
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "saw 1\nescaped stop\n");
}

#[test]
fn a_zero_divisor_raises_a_catchable_arithmetic_exception() {
    let (out, ok) = run(
        r#"try { println(1 / 0) } catch (ArithmeticException e) { println("d " + e.message) }"#,
    );
    assert!(ok);
    assert_eq!(out, "d Division by zero\n");
}

#[test]
fn an_unmatched_exception_escapes_with_a_nonzero_exit() {
    let src = r#"
println("before")
try { throw new Exception("x") } catch (IllegalStateException e) { println("no") }
println("unreachable")
"#;
    let (out, err, ok) = run_full(src);
    assert!(!ok, "an uncaught exception must exit non-zero");
    assert_eq!(out, "before\n");
    // Which throwable escaped is the point of the test. A handler that matched
    // the wrong class, or a different exception raised from the same line,
    // stops execution in exactly the same place and prints the same stdout.
    assert_eq!(err, "groovyrs: Caught: java.lang.Exception: x\n");
}

#[test]
fn a_program_without_exceptions_emits_no_exception_ops() {
    // The gate that keeps exception support free: no `try`/`throw` in the source
    // means none of the exception builtins are emitted at all.
    let disasm = groovyrs::disassemble("def s = 0\nfor (i in 1..3) s += i\nprintln s").unwrap();
    for id in ["730", "731", "732", "733", "734", "735", "736"] {
        assert!(
            !disasm.contains(&format!("CallBuiltin({id}")),
            "exception builtin {id} leaked into an exception-free program:\n{disasm}"
        );
    }
}

// ── GStrings ────────────────────────────────────────────────────────────────

#[test]
fn gstring_interpolates_names_paths_and_expressions() {
    let src = r#"
def name = "world"
def n = 7
def m = [b: 3, inner: [deep: 9]]
println("hello $name")
println("braced ${name}")
println("expr ${n * 6}")
println("path $m.b")
println("deep $m.inner.deep")
println("adjacent $name$n")
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "hello world\nbraced world\nexpr 42\npath 3\ndeep 9\nadjacent world7\n"
    );
}

#[test]
fn gstring_placeholders_nest_braces_and_quotes() {
    let src = r#"
def n = 7
println("a ${ n > 5 ? "big" : "small" } b")
println("c ${ [1, 2].collect { it * 2 } } d")
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "a big b\nc [2, 4] d\n");
}

#[test]
fn a_dollar_is_inert_when_escaped_or_single_quoted() {
    let src = "def a = 1\nprintln(\"esc \\$a\")\nprintln('lit $a')";
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "esc $a\nlit $a\n");
}

#[test]
fn an_interpolated_object_renders_through_its_to_string() {
    // Plain handle formatting would print an opaque id here; a GString (and `+`
    // concatenation) must dispatch the class's own toString.
    let src = r#"
class P { def x; P(v) { x = v }; String toString() { return "P<" + x + ">" } }
def p = new P(4)
println("obj $p")
println("cat " + p)
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "obj P<4>\ncat P<4>\n");
}

#[test]
fn a_double_quoted_string_without_a_placeholder_stays_a_plain_constant() {
    // The compile-time gate: no `$` means no GString builtin, so ordinary string
    // literals keep the bytecode they always had.
    let disasm = groovyrs::disassemble(r#"println("plain text")"#).unwrap();
    assert!(
        !disasm.contains("CallBuiltin(723"),
        "a placeholder-free literal must not build a GString:\n{disasm}"
    );
}

// ── Closures ────────────────────────────────────────────────────────────────

#[test]
fn an_explicit_empty_parameter_list_takes_no_implicit_it() {
    let (out, ok) = run("def z = { -> 7 * 3 }\nprintln(z())\nprintln(z.call())");
    assert!(ok);
    assert_eq!(out, "21\n21\n");
}

#[test]
fn a_closure_body_that_is_a_try_returns_the_taken_branch() {
    // Groovy's implicit return reaches through a trailing `try`.
    let src = r#"println([1, 2, 3].collect { try { if (it == 2) throw new Exception("s"); it } catch (Exception e) { 99 } })"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "[1, 99, 3]\n");
}

#[test]
fn an_exception_from_every_re_entrant_dispatch_path_reaches_the_handler() {
    // Each of these runs user code through a nested VM re-entry (a constructor,
    // `toString` during `println`, a property getter, `getAt`, an operator
    // overload, `asBoolean`, a closure call, a GString placeholder). A path that
    // skipped its post-call check would resume with a placeholder value instead
    // of unwinding — and `println` would emit a spurious `null`.
    let src = r#"
class B { def n; B(x) { if (x < 0) throw new IllegalArgumentException("neg"); n = x } }
class C { String toString() { throw new IllegalStateException("ts") } }
class D { def getX() { throw new IllegalStateException("gx") } }
class E { def getAt(i) { throw new IllegalStateException("ga") } }
class F { def n = 1; def plus(o) { throw new IllegalStateException("pl") } }
class G { def asBoolean() { throw new IllegalStateException("ab") } }
try { println(new B(-1).n) } catch (Exception e) { println("ctor " + e.message) }
try { println(new C()) } catch (Exception e) { println("tostr " + e.message) }
try { println(new D().x) } catch (Exception e) { println("get " + e.message) }
try { println(new E()[0]) } catch (Exception e) { println("at " + e.message) }
try { println(new F() + new F()) } catch (Exception e) { println("plus " + e.message) }
try { if (new G()) println("y") else println("n") } catch (Exception e) { println("bool " + e.message) }
def h = { throw new IllegalStateException("clo") }
try { h(1) } catch (Exception e) { println("clo " + e.message) }
try { println("x ${ [1].collect { throw new IllegalStateException('gs') } }") } catch (Exception e) { println("gstr " + e.message) }
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "ctor neg\ntostr ts\nget gx\nat ga\nplus pl\nbool ab\nclo clo\ngstr gs\n"
    );
}

// ── Catchable runtime faults ────────────────────────────────────────────────

#[test]
fn an_unknown_method_raises_a_catchable_missing_method_exception() {
    // Before this landed, an unknown method aborted the run, so the handler was
    // unreachable. Verified against Apache Groovy 5.0.7 (which additionally
    // appends its `Possible solutions:` GDK suggestion list — see BUGS.md).
    let src = r#"
try { println("hi".nope()) }
catch (MissingMethodException e) { println("mme") }
println("after")
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "mme\nafter\n");
}

#[test]
fn an_unknown_property_message_matches_groovy() {
    let src =
        r#"try { println("hi".zork) } catch (MissingPropertyException e) { println(e.message) }"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "No such property: zork for class: java.lang.String\n");
}

#[test]
fn a_call_on_null_raises_a_catchable_null_pointer_exception() {
    let src = r#"
nil = null
try { println(nil.length()) } catch (NullPointerException e) { println(e.message) }
try { println(nil.zork) } catch (NullPointerException e) { println(e.message) }
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "Cannot invoke method length() on null object\n\
         Cannot get property 'zork' on null object\n"
    );
}

#[test]
fn null_still_answers_to_string_and_equals() {
    // Groovy routes a call on null through `NullObject`, which answers these two
    // rather than raising — so the NPE path must not swallow them.
    let (out, ok) = run("nil = null\nprintln(nil.toString())\nprintln(nil.equals(1))");
    assert!(ok);
    assert_eq!(out, "null\nfalse\n");
}

#[test]
fn out_of_range_indexing_raises_the_types_groovy_raises() {
    let src = r#"
try { println([1, 2, 3].get(9)) } catch (IndexOutOfBoundsException e) { println(e.message) }
try { println("abc"[9]) } catch (StringIndexOutOfBoundsException e) { println(e.message) }
try { println([1, 2, 3][-9]) } catch (ArrayIndexOutOfBoundsException e) { println(e.message) }
// A list subscript past the end is null in Groovy, not an error.
println([1, 2, 3][5])
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "Index 9 out of bounds for length 3\n\
         Range [9, 10) out of bounds for length 3\n\
         Negative array index [-9] too large for array size 3\n\
         null\n"
    );
}

#[test]
fn unparsable_string_conversions_raise_number_format_exception() {
    let src = r#"
try { println("abc".toInteger()) } catch (NumberFormatException e) { println(e.message) }
try { println("1x".toDouble()) } catch (NumberFormatException e) { println(e.message) }
// An `int`-overflowing literal is a parse failure in Groovy, not a wrap.
try { println("9999999999".toInteger()) } catch (NumberFormatException e) { println(e.message) }
println("9999999999".toLong())
println(" 42 ".toInteger() + 1)
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "For input string: \"abc\"\n\
         For input string: \"1x\"\n\
         For input string: \"9999999999\"\n\
         9999999999\n\
         43\n"
    );
}

#[test]
fn a_runtime_fault_unwinds_across_a_frame_and_runs_finally() {
    let src = r#"
nil = null
def deep(x) {
    try { return x.length() } finally { println("fin") }
}
try { println(deep(nil)) } catch (Exception e) { println("outer " + e.message) }
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "fin\nouter Cannot invoke method length() on null object\n"
    );
}

#[test]
fn a_runtime_fault_the_handler_does_not_match_still_escapes() {
    let src = r#"
println("before")
try { println("hi".nope()) } catch (IllegalStateException e) { println("no") }
println("unreachable")
"#;
    let (out, err, ok) = run_full(src);
    assert!(!ok, "an unmatched runtime fault must exit non-zero");
    assert_eq!(out, "before\n");
    assert_eq!(
        err,
        "groovyrs: Caught: groovy.lang.MissingMethodException: No signature of \
         method: nope for class: java.lang.String is applicable for argument \
         types: () values: []\n"
    );
}

#[test]
fn the_new_throwables_sit_in_groovys_hierarchy() {
    let src = r#"
try { "hi".nope() } catch (Exception e) {
    println(e instanceof MissingMethodException)
    println(e instanceof GroovyRuntimeException)
    println(e instanceof RuntimeException)
    println(e instanceof NullPointerException)
}
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "true\ntrue\ntrue\nfalse\n");
}

#[test]
fn writing_an_undeclared_field_raises_missing_property_exception() {
    let src = r#"
class Foo { def a = 1 }
def f = new Foo()
try { f.zz = 3 } catch (MissingPropertyException e) { println("mpe") }
f.a = 2
println(f.a)
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "mpe\n2\n");
}

#[test]
fn dividing_zero_by_zero_reports_division_undefined() {
    // Java's `BigDecimal.divide` distinguishes the two zero-divisor cases, and
    // Groovy promotes integer division to `BigDecimal`, so `0 / 0` differs from
    // `7 / 0`. Verified against Apache Groovy 5.0.7.
    let src = r#"
def p(l, c) { try { println(l + " " + c()) } catch (ArithmeticException e) { println(l + " " + e.message) } }
p("a", { 0 / 0 })
p("b", { 7 / 0 })
p("c", { 0.0 / 0 })
p("d", { 0.0d / 0.0d })
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "a Division undefined\nb Division by zero\nc Division undefined\nd NaN\n"
    );
}

// ── switch / do-while / labeled break ───────────────────────────────────────

#[test]
fn switch_matches_with_groovys_is_case_rules() {
    // Not `==`: a range and a list contain, a type is `instanceof`, a pattern
    // matches the *whole* subject, and a closure is called with it. Verified
    // against Apache Groovy 5.0.7.
    let src = r#"
def f(x) {
    switch (x) {
        case 1: return "one"
        case 4..6: return "range"
        case [7, 8]: return "list"
        case String: return "string"
        case ~/a+b/: return "regex"
        case { it instanceof Integer && it > 100 }: return "big"
        case null: return "null"
        default: return "other"
    }
}
def xs = [1, 5, 7, "zz", 101, null, 9.5]
for (i in 0..<xs.size()) println(f(xs[i]))
println(f("aab"))
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "one\nrange\nlist\nstring\nbig\nnull\nother\nstring\n");
}

#[test]
fn a_regex_case_must_match_the_whole_subject() {
    // Groovy's `case ~/…/` is `Matcher.matches`, not `find` — `a+` does not
    // match "aabb" even though it matches a prefix of it.
    let src = r#"
switch ("aabb") {
    case ~/a+/: println("partial"); break
    case ~/a+b+/: println("full"); break
    default: println("none")
}
println(~/a+b/)
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "full\na+b\n");
}

#[test]
fn switch_falls_through_until_a_break() {
    let src = r#"
def g(x) {
    def r = ""
    switch (x) {
        case 1: r = r + "a"
        case 2: r = r + "b"; break
        case 3: r = r + "c"
        default: r = r + "d"
    }
    return r
}
for (i in 1..4) println(g(i))
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "ab\nb\ncd\nd\n");
}

#[test]
fn a_switch_evaluates_its_subject_once_and_labels_lazily() {
    let src = r#"
n = 0
hits = ""
def bump() { n = n + 1; return 2 }
def probe(v) { hits = hits + v; return v }
switch (bump()) {
    case probe(1): println("no"); break
    case probe(2): println("yes"); break
    case probe(3): println("no"); break
}
println(n + " " + hits)
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "yes\n1 12\n");
}

#[test]
fn do_while_runs_its_body_before_the_first_test() {
    let src = r#"
def i = 0
do { println("i " + i); i++ } while (i < 3)
do { println("once") } while (false)
def k = 0
do {
    k++
    if (k == 2) continue
    if (k == 5) break
    println("k " + k)
} while (k < 10)
println("end " + k)
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "i 0\ni 1\ni 2\nonce\nk 1\nk 3\nk 4\nend 5\n");
}

#[test]
fn a_labeled_break_and_continue_bind_to_the_named_loop() {
    let src = r#"
outer:
for (a in 0..2) {
    for (b in 0..2) {
        if (b == 1) continue outer
        if (a == 2) break outer
        println(a + "-" + b)
    }
}
o2: for (a in 0..2) { inner: for (b in 0..2) { if (b == 1) break inner; println("i " + a + b) } }
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "0-0\n1-0\ni 00\ni 10\ni 20\n");
}

#[test]
fn a_switch_is_a_break_target_but_not_a_continue_target() {
    // Groovy/Java rule: a `continue` inside a `switch` continues the enclosing
    // loop, while a `break` leaves only the switch. A labeled `break` leaves the
    // named loop from inside it.
    let src = r#"
scan:
for (a in 0..3) {
    switch (a) {
        case 1: continue
        case 3: break scan
        default: break
    }
    println("scan " + a)
}
println("done")
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "scan 0\nscan 2\ndone\n");
}

#[test]
fn a_break_naming_no_enclosing_loop_is_a_compile_error() {
    let (out, err, ok) = run_full("for (i in 0..2) { break nope }");
    assert!(!ok, "a label with no matching loop must not compile");
    assert_eq!(out, "");
    // A *compile* error, so it names the label and the line — asserting the
    // text is what distinguishes it from the runtime faults above.
    assert_eq!(
        err,
        "groovyrs: no enclosing loop labeled `nope` on line 1\n"
    );
}

// ── assert (power assert) ───────────────────────────────────────────────────

#[test]
fn a_failing_assert_renders_groovys_power_assert_layout() {
    // The statement's own source, then each recorded value under the column it
    // came from, with `|` markers on the lines between. Byte-verified against
    // Apache Groovy 5.0.7.
    let src = r#"
x = 3
try { assert x + 1 == 5 } catch (AssertionError e) { println(e.getMessage()) }
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "assert x + 1 == 5\n       | |   |\n       3 4   false\n"
    );
}

#[test]
fn a_value_too_wide_to_share_a_line_is_pushed_down_one() {
    // `'hi'` would overlap the `2` recorded at the next column, so it moves to a
    // new line and leaves a `|` marker behind — the layout rule that makes the
    // renderer worth porting rather than approximating.
    let src = r#"
s = "hi"
try { assert s.length() == 3 } catch (AssertionError e) { println(e.getMessage()) }
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "assert s.length() == 3\n       | |        |\n       | 2        false\n       'hi'\n"
    );
}

#[test]
fn power_assert_values_use_groovys_verbose_rendering() {
    // Unlike `println`, the layout quotes strings and a map's keys.
    let src = r#"
l = ["a", "b"]
m = [k: "v"]
try { assert l == 1 } catch (AssertionError e) { println(e.getMessage()) }
try { assert m == 1 } catch (AssertionError e) { println(e.getMessage()) }
println(l)
println(m)
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "assert l == 1\n       | |\n       | false\n       ['a', 'b']\n\
         assert m == 1\n       | |\n       | false\n       ['k':'v']\n\
         [a, b]\n[k:v]\n"
    );
}

#[test]
fn a_unary_operator_is_recorded_under_its_own_column() {
    let src = r#"
x = 3
try { assert !x } catch (AssertionError e) { println(e.getMessage()) }
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "assert !x\n       ||\n       |3\n       false\n");
}

#[test]
fn the_message_form_raises_a_plain_assertion_error() {
    // With `: message` Groovy throws `java.lang.AssertionError` and quotes the
    // condition's canonical AST text — fully parenthesised, with qualified type
    // names — not the source text the power form prints.
    let src = r#"
x = 3
s = "hi"
def p(c) { try { c() } catch (AssertionError e) { println(e.getMessage()) } }
p({ assert x == 5 : "custom" })
p({ assert s.length() == 9 : "len bad" })
p({ assert x instanceof String : "type" })
p({ assert (x + 1) * 2 == 9 : "math" })
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "custom. Expression: (x == 5). Values: x = 3\n\
         len bad. Expression: (s.length() == 9)\n\
         type. Expression: (x instanceof java.lang.String). Values: x = 3\n\
         math. Expression: (((x + 1) * 2) == 9). Values: x = 3\n"
    );
}

#[test]
fn the_values_clause_reads_variables_at_failure_time() {
    // Groovy reports a named variable's current value even when `&&`
    // short-circuited past the operand it sits in, so the clause cannot be built
    // from the power-assert recorder alone.
    let src = r#"
x = 3
try { assert x == 1 && x == 2 : "M" } catch (AssertionError e) { println(e.getMessage()) }
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "M. Expression: ((x == 1) && (x == 2)). Values: x = 3, x = 3\n"
    );
}

#[test]
fn a_power_assertion_error_prints_groovys_banner() {
    let src = r#"
x = 3
try { assert x == 5 } catch (Throwable t) { println("[" + t.toString() + "]") }
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[Assertion failed: \n\nassert x == 5\n       | |\n       3 false\n]\n"
    );
}

#[test]
fn an_assert_is_an_ordinary_throwable_that_unwinds() {
    let src = r#"
def check(v) {
    try { assert v < 2 : "too big"; return "ok " + v } finally { println("fin " + v) }
}
for (i in 1..2) {
    try { println(check(i)) } catch (AssertionError e) { println("caught " + e.getMessage()) }
}
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "fin 1\nok 1\nfin 2\ncaught too big. Expression: (v < 2). Values: v = 2\n"
    );
}

#[test]
fn a_passing_assert_is_silent_and_an_uncaught_one_exits_nonzero() {
    let (out, ok) = run("x = 3\nassert x == 3\nprintln(\"after\")");
    assert!(ok);
    assert_eq!(out, "after\n");

    let (out, ok) = run("x = 3\nprintln(\"a\")\nassert x == 5\nprintln(\"b\")");
    assert!(!ok, "an uncaught assert must exit non-zero");
    assert_eq!(out, "a\n");
}

// ── `%` by zero (verified against Apache Groovy 5.0.7) ─────────────────────

#[test]
fn integer_modulo_by_zero_raises_slash_by_zero() {
    // Groovy: `java.lang.ArithmeticException: / by zero`, catchable. A literal
    // divisor and a variable one take the two different compiler paths.
    let src = r#"
def z = 0
try { println(7 % z) } catch (ArithmeticException e) { println("var " + e.message) }
try { println(7 % 0) } catch (ArithmeticException e) { println("lit " + e.message) }
println(7 % 3)
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "var / by zero\nlit / by zero\n1\n");
}

#[test]
fn decimal_modulo_by_zero_reaches_its_handler() {
    // A BigDecimal operand uses BigDecimal.remainder's wording, and `0.0 % 0`
    // is *undefined* rather than division by zero.
    let src = r#"
def z = 0
try { println(7.5 % z) } catch (ArithmeticException e) { println("a " + e.message) }
try { println(0.0 % 0.0) } catch (ArithmeticException e) { println("b " + e.message) }
try { println(7 % 0.0) } catch (ArithmeticException e) { println("c " + e.message) }
println("after")
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "a Division by zero\nb Division undefined\nc Division by zero\nafter\n"
    );
}

#[test]
fn double_modulo_by_zero_is_nan_not_an_exception() {
    let (out, ok) = run("println(7.0d % 0.0d)\nprintln(7 % 0.0d)\nprintln(7.0d % 0)");
    assert!(ok);
    assert_eq!(out, "NaN\nNaN\nNaN\n");
}

#[test]
fn compound_modulo_assign_shares_the_zero_check() {
    let src = r#"
def z = 0
def x = 17
x %= 5
println(x)
try { x %= z } catch (ArithmeticException e) { println(e.message) }
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "2\n/ by zero\n");
}

#[test]
fn an_uncaught_modulo_by_zero_exits_nonzero() {
    let (out, err, ok) = run_full("def z = 0\nprintln(\"a\")\nprintln(7 % z)\nprintln(\"b\")");
    assert!(!ok, "an uncaught ArithmeticException must exit non-zero");
    assert_eq!(out, "a\n");
    // `%` on two integers stays integral, so this is the JVM's own `/ by zero`
    // and not the `Division by zero` that `BigDecimal.divide` carries — the two
    // wordings are what distinguish the promoted path from the native one.
    assert_eq!(err, "groovyrs: java.lang.ArithmeticException: / by zero\n");
}

// ── Interfaces (`interface`, `implements`, `default` methods) ──────────────

#[test]
fn an_implemented_interface_answers_instanceof() {
    let src = r#"
interface Named { def name() }
interface Tagged extends Named { def tag() }
class Thing implements Tagged {
    def name() { "t" }
    def tag() { "g" }
}
def t = new Thing()
println(t.name())
println(t instanceof Named)
println(t instanceof Tagged)
println(t instanceof Thing)
println(t instanceof Comparable)
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "t\ntrue\ntrue\ntrue\nfalse\n");
}

#[test]
fn an_interface_default_method_is_inherited_and_overridable() {
    // The default may call the interface's own abstract method, and a class
    // definition wins over it.
    let src = r#"
interface Named {
    def name()
    default def greet() { "hi " + name() }
}
class A implements Named { def name() { "a" } }
class B implements Named { def name() { "b" }; def greet() { "yo" } }
println(new A().greet())
println(new B().greet())
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "hi a\nyo\n");
}

#[test]
fn an_interface_reached_through_a_superclass_still_answers() {
    let src = r#"
interface Named { def name(); default def greet() { "hi " + name() } }
abstract class Base implements Named { def describe() { greet() + "!" } }
class Leaf extends Base { def name() { "leaf" } }
def l = new Leaf()
println(l.describe())
println(l instanceof Named)
println(l instanceof Base)
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "hi leaf!\ntrue\ntrue\n");
}

// ── GDK collections, spread, and `for (x in …)` ────────────────────────────

#[test]
fn sort_and_unique_mutate_a_variable_receiver() {
    // Groovy's List.sort()/unique() sort the receiver in place and return it.
    let src = r#"
def xs = [3, 1, 2, 3]
println(xs.sort())
println(xs)
println(xs.unique())
println(xs)
def ys = [3, 1, 2]
println(ys.sort(false))
println(ys)
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[1, 2, 3, 3]\n[1, 2, 3, 3]\n[1, 2, 3]\n[1, 2, 3]\n[1, 2, 3]\n[3, 1, 2]\n"
    );
}

#[test]
fn a_one_parameter_sort_closure_is_a_key_extractor() {
    let src = r#"
println(["bbb", "a", "cc"].sort { it.size() })
println([3, 1, 2].sort { a, b -> b <=> a })
println(["bbb", "a", "cc"].max { it.size() })
println([3, 1, 2].min())
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "[a, cc, bbb]\n[3, 2, 1]\nbbb\n1\n");
}

#[test]
fn list_group_by_join_and_sum() {
    let src = r#"
println([1, 2, 3, 4].groupBy { it % 2 })
println([1, 2, 3].join("-"))
println([1, [2, 3]].join("-"))
println([1, 2, 3].sum(100))
println(["a", "b"].sum())
println([].max())
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[1:[1, 3], 0:[2, 4]]\n1-2-3\n1-[2, 3]\n106\nab\nnull\n"
    );
}

#[test]
fn map_gdk_passes_key_value_or_a_single_entry() {
    let src = r#"
def m = [b: 2, a: 1]
m.each { k, v -> println(k + "=" + v) }
m.each { e -> println(e) }
println(m.collect { k, v -> k + v })
println(m.findAll { k, v -> v > 1 })
println(m.find { k, v -> v == 1 })
println(m.groupBy { k, v -> v > 1 })
println(m.inject(0) { acc, e -> acc + e.value })
println(m.sort())
println(m.max { it.value })
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "b=2\na=1\nb=2\na=1\n[b2, a1]\n[b:2]\na=1\n[true:[b:2], false:[a:1]]\n3\n[a:1, b:2]\nb=2\n"
    );
}

#[test]
fn the_spread_operator_maps_a_member_over_the_elements() {
    // `*.` is `collect { it?.member }`, so a null element spreads to null.
    let src = r#"
class P { def x; P(x) { this.x = x }; def twice() { x * 2 } }
def ps = [new P(1), new P(4)]
println(ps*.x)
println(ps*.twice())
println([1, null, 3]*.toString())
println([[1, 2], [3]]*.size())
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "[1, 4]\n[2, 8]\n[1, null, 3]\n[2, 1]\n");
}

#[test]
fn for_in_walks_lists_maps_strings_and_nothing_for_null() {
    let src = r#"
for (x in [10, 20]) { println(x) }
for (e in [k: 1, j: 2]) { println(e) }
for (c in "hi") { println(c) }
for (x in null) { println("never") }
for (x in 5) { println(x) }
def xs = [1, 2, 3]
for (x in xs) { if (x == 2) continue; println("v" + x) }
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "10\n20\nk=1\nj=2\nh\ni\n5\nv1\nv3\n");
}

// ── `getClass()` and `String.toBigDecimal()` ───────────────────────────────

#[test]
fn get_class_names_the_java_type() {
    let src = r#"
println(1.getClass())
println("s".class)
println(1.5.getClass().getName())
println(1.5d.class.simpleName)
println([1].getClass())
println([a: 1].getClass())
println(null.getClass())
class W {}
println(new W().getClass().getName())
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "class java.lang.Integer\nclass java.lang.String\njava.math.BigDecimal\nDouble\n\
         class java.util.ArrayList\nclass java.util.LinkedHashMap\n\
         class org.codehaus.groovy.runtime.NullObject\nW\n"
    );
}

#[test]
fn to_big_decimal_parses_with_exact_scale() {
    let src = r#"
println("1.5".toBigDecimal())
println("100.00".toBigDecimal())
println(" 7 ".toBigDecimal())
println(".5".toBigDecimal())
println("1.".toBigDecimal())
println("2.5e7".toBigDecimal())
println("1e-7".toBigDecimal())
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "1.5\n100.00\n7\n0.5\n1\n2.5E+7\n1E-7\n");
}

#[test]
fn to_big_decimal_reports_big_decimals_own_parse_messages() {
    // Three distinct diagnostics, plus the message-less form the JDK throws for
    // an empty string and for an exponent mark with no digits (`null` message).
    let src = r#"
def bad = ["x", "1.2.3", "+", "1e999999999999", "1ex", "", "1e"]
for (s in bad) {
    try { println(s.toBigDecimal()) }
    catch (NumberFormatException e) { println("[" + s + "] " + e.message) }
}
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[x] Character x is neither a decimal digit number, decimal point, nor \"e\" notation exponential mark.\n\
         [1.2.3] Character array contains more than one decimal point.\n\
         [+] No digits found.\n\
         [1e999999999999] Too many nonzero exponent digits.\n\
         [1ex] Not a digit.\n\
         [] null\n\
         [1e] null\n"
    );
}

#[test]
fn map_sort_does_not_mutate_its_receiver() {
    // Regression: the `List.sort()` write-back must not fire for a map, whose
    // `sort()` returns a new map and leaves the receiver in insertion order.
    // (Caught by the `gdk` fuzz mode against Apache Groovy 5.0.7.)
    let src = r#"
def m = [b: 2, a: 1, c: 3]
println(m.sort())
println(m)
for (e in m) { println("e" + e) }
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "[a:1, b:2, c:3]\n[b:2, a:1, c:3]\neb=2\nea=1\nec=3\n");
}

#[test]
fn a_map_property_read_is_only_ever_a_key_read() {
    // Groovy: `m.size`, `m.class`, and `m.length` are key reads on a Map, so an
    // absent key is null — the count properties do NOT apply. A key that *is*
    // named `size`/`class` reads its own value.
    let src = r#"
def m = [a: 1]
println(m.size)
println(m.class)
println(m.length)
println(m.size())
def m2 = [size: 9, class: "c", a: 1]
println(m2.size)
println(m2.size())
println(m2.class)
println(m2.getClass())
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "null\nnull\nnull\n1\n9\n3\nc\nclass java.util.LinkedHashMap\n"
    );
}

#[test]
fn sort_writes_back_for_the_mutating_forms_only() {
    // Groovy: `sort()`, `sort(true)`, and `sort(closure)` mutate the receiver;
    // `sort(false)` returns a copy and leaves it alone.
    let src = r#"
def a = [3, 1, 2]
println(a.sort(true))
println(a)
def b = [3, 1, 2]
println(b.sort(false) { x, y -> x <=> y })
println(b)
def c = [3, 1, 2]
println(c.sort(true) { x, y -> y <=> x })
println(c)
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[1, 2, 3]\n[1, 2, 3]\n[1, 2, 3]\n[3, 1, 2]\n[3, 2, 1]\n[3, 2, 1]\n"
    );
}

// ── Groovy's 32-bit `Integer` semantics ────────────────────────────────────
//
// Groovy's integer width is a property of the value, not of a declared type:
// `Integer op Integer` wraps at 32 bits and anything involving a `Long` wraps at
// 64. Every expected output below was verified byte-for-byte against Apache
// Groovy 5.0.8.

#[test]
fn integer_arithmetic_wraps_at_32_bits() {
    // The two canonical overflows. `1000000 * 1000000` fits an i64 easily, so
    // getting `-727379968` proves the narrowing is real and not i64 saturation.
    let (out, _) = run("println(Integer.MAX_VALUE + 1)\nprintln(1000000 * 1000000)\nprintln(2000000000 + 2000000000)\nprintln(Integer.MIN_VALUE - 1)");
    assert_eq!(out, "-2147483648\n-727379968\n-294967296\n2147483647\n");
}

#[test]
fn a_long_operand_widens_the_arithmetic_to_64_bits() {
    // The `L` is the only difference between these and the wrapping forms
    // above — the runtime values are identical, so this is what proves the
    // compiler's static width reaches the host.
    let (out, _) = run(
        "println(2147483647 + 1L)\nprintln(1000000L * 1000000)\nprintln(2000000000L + 2000000000L)",
    );
    assert_eq!(out, "2147483648\n1000000000000\n4000000000\n");
}

#[test]
fn a_long_accumulator_accumulates_past_integer_range() {
    // The declared width has to win over the running value's magnitude: `t` is
    // still inside `Integer` range when the second `+=` overflows it.
    let (out, _) =
        run("long t = 0\nt += 2000000000\nt += 2000000000\nprintln(t)\nprintln(t.getClass())");
    assert_eq!(out, "4000000000\nclass java.lang.Long\n");
    // A `def` initialised from an `L` literal is a `Long` the same way.
    let (out, _) = run("def t = 0L\nt += 2000000000\nt += 2000000000\nprintln(t)");
    assert_eq!(out, "4000000000\n");
    // Without either, the same code is `Integer` arithmetic and wraps.
    let (out, _) = run("def t = 0\nt += 2000000000\nt += 2000000000\nprintln(t)");
    assert_eq!(out, "-294967296\n");
}

#[test]
fn arithmetic_wraps_where_the_compiler_cannot_see_the_operands() {
    // A closure parameter has no static width, so these wrap on the operands'
    // magnitudes alone — the runtime half of the width rule.
    let (out, _) = run("def f = { a, b -> a * b }\nprintln(f(1000000, 1000000))\nprintln([1000000, 1000000].inject(1) { a, b -> a * b })");
    assert_eq!(out, "-727379968\n-727379968\n");
}

#[test]
fn integer_and_long_are_told_apart_by_class() {
    let (out, _) = run("println(2147483647.getClass())\nprintln(2147483648.getClass())\nprintln(1L.getClass())\nprintln((1000000 * 1000000).getClass())");
    assert_eq!(
        out,
        "class java.lang.Integer\nclass java.lang.Long\nclass java.lang.Long\nclass java.lang.Integer\n"
    );
}

#[test]
fn long_arithmetic_wraps_at_64_bits() {
    let (out, _) = run("println(Long.MAX_VALUE + 1)\nprintln(9223372036854775807 + 1)");
    assert_eq!(out, "-9223372036854775808\n-9223372036854775808\n");
}

#[test]
fn the_negated_minimum_literal_is_an_integer() {
    // `-2147483648` is `Integer.MIN_VALUE`, even though `2147483648` alone is a
    // `Long` — so subtracting one from it wraps rather than widening.
    let (out, _) = run("println(-2147483648 - 1)\nprintln((-2147483648).getClass())\nprintln(-Integer.MIN_VALUE)\nprintln(Math.abs(Integer.MIN_VALUE))");
    assert_eq!(
        out,
        "2147483647\nclass java.lang.Integer\n-2147483648\n-2147483648\n"
    );
}

#[test]
fn shifts_carry_their_left_operands_width() {
    // The count is masked to the width's bit index, so an `Integer` shift by 32
    // is the identity and a `Long` shift by 32 is not.
    let (out, _) = run("println(1 << 31)\nprintln(1 << 32)\nprintln(1L << 32)\nprintln(1 >> 32)\nprintln(256 >> 33)");
    assert_eq!(out, "-2147483648\n1\n4294967296\n1\n128\n");
}

#[test]
fn unsigned_right_shift_fills_to_the_operands_width() {
    let (out, _) = run("println(-1 >>> 28)\nprintln(-1L >>> 60)\nprintln(-8 >>> 1)\nprintln(Integer.MIN_VALUE >>> 0)");
    assert_eq!(out, "15\n15\n2147483644\n-2147483648\n");
}

#[test]
fn radix_and_separated_integer_literals() {
    let (out, _) = run("println(0xFF)\nprintln(0b1010)\nprintln(011)\nprintln(1_000_000)\nprintln(0xFFFFFFFF)\nprintln(0xFFFFFFFF.getClass())");
    assert_eq!(
        out,
        "255\n10\n9\n1000000\n4294967295\nclass java.lang.Long\n"
    );
}

#[test]
fn narrowing_conversions_keep_the_targets_low_bits() {
    let (out, _) = run("println(2147483648L as int)\nprintln(300 as byte)\nprintln(3.9 as int)\nprintln(3000000000L.intValue())\nprintln(Integer.MIN_VALUE.intdiv(-1))");
    assert_eq!(out, "-2147483648\n44\n3\n-1294967296\n-2147483648\n");
}

#[test]
fn iterator_is_a_live_cursor_over_a_list_a_map_and_a_string() {
    // `next()` advances the shared handle, so a second holder sees the move, and
    // an exhausted iterator raises `NoSuchElementException`.
    let (out, _) = run(
        "def it = [1, 2, 3].iterator()\nprintln it.next()\nprintln it.hasNext()\n\
         println([a: 1, b: 2].entrySet().iterator().next().key\n)\n\
         println([a: 1].iterator().next())\nprintln('ab'.iterator().next())\n\
         println([1, 2].iterator().getClass().getName())\n\
         def e = [1].iterator()\ne.next()\ntry { e.next() } catch (t) { println t.getClass().getName() }",
    );
    assert_eq!(
        out,
        "1\ntrue\na\na=1\na\njava.util.ArrayList$Itr\njava.util.NoSuchElementException\n"
    );
}

#[test]
fn pop_takes_the_first_element_and_remove_last_the_last() {
    // Groovy's `List.pop` is not a stack pop — it removes the *head*. Both it
    // and `removeLast` raise `NoSuchElementException` on an empty list, but they
    // are NOT symmetric about the message: `pop` explains itself and
    // `removeLast` throws a bare `new NoSuchElementException()`, whose message
    // is `null`.
    //
    // This test previously expected `Cannot removeLast() an empty List`, which
    // Groovy has never printed — the implementation interpolated the method name
    // into `pop`'s sentence and the expectation was written from the
    // implementation rather than from the reference. Re-verified against Apache
    // Groovy 5.0.8 on JDK 21, which prints exactly the two lines below.
    //
    // The class is now asserted alongside the message. The old expectation read
    // `getMessage()` only, so it could not have told a `NoSuchElementException`
    // from any other throwable carrying the same text.
    let (out, _) = run("def a = [1, 2, 3]\nprintln a.pop()\nprintln a\n\
         def b = [1, 2, 3]\nprintln b.removeLast()\nprintln b\n\
         try { [].pop() } catch (t) { println t.getClass().getName() + '|' + t.getMessage() }\n\
         try { [].removeLast() } catch (t) { println t.getClass().getName() + '|' + t.getMessage() }");
    assert_eq!(
        out,
        "1\n[2, 3]\n3\n[1, 2]\n\
         java.util.NoSuchElementException|Cannot pop() an empty List\n\
         java.util.NoSuchElementException|null\n"
    );
}

#[test]
fn indexed_answers_a_map_where_with_index_answers_pairs() {
    let (out, _) = run(
        "println([1, 2, 3].indexed())\nprintln([1, 2, 3].indexed(1))\nprintln([1, 2, 3].withIndex())",
    );
    assert_eq!(
        out,
        "[0:1, 1:2, 2:3]\n[1:1, 2:2, 3:3]\n[[1, 0], [2, 1], [3, 2]]\n"
    );
}

#[test]
fn combinations_varies_the_first_sub_collection_fastest() {
    let (out, _) = run("println([[1, 2], [3, 4]].combinations())");
    assert_eq!(out, "[[1, 3], [2, 3], [1, 4], [2, 4]]\n");
}

#[test]
fn spaceship_raises_for_a_receiver_that_is_not_comparable() {
    // A list, a map and a range are not `Comparable`, so `<=>` raises rather
    // than inventing an order — even for two *equal* lists. `null` still orders
    // before everything.
    let (out, _) = run(
        "try { [1, 2] <=> [1, 2] } catch (t) { println t.getMessage() }\n\
         try { [a: 1] <=> 1 } catch (t) { println t.getMessage() }\n\
         println(null <=> 1)\nprintln(1 <=> null)\nprintln('a' <=> 'b')",
    );
    assert_eq!(
        out,
        "Cannot compare java.util.ArrayList with value '[1, 2]' and java.util.ArrayList with value '[1, 2]'\n\
         Cannot compare java.util.LinkedHashMap with value '{a=1}' and java.lang.Integer with value '1'\n\
         -1\n1\n-1\n"
    );
}

#[test]
fn map_with_default_stores_the_computed_value() {
    // `groovy.lang.MapWithDefault` grows: a missing-key read runs the closure
    // *and* records its result under that key.
    let (out, _) = run(
        "def m = [a: 1].withDefault { 0 }\nprintln m['z']\nprintln m\n\
         def n = [:].withDefault { k -> k.size() }\nn['abc']\nprintln n",
    );
    assert_eq!(out, "0\n[a:1, z:0]\n[abc:3]\n");
}

#[test]
fn map_sub_map_minus_and_intersect_compare_whole_entries() {
    let (out, _) = run(
        "println([a: 1, b: 2].subMap(['a']))\nprintln([a: 1, b: 2].subMap('a', 'b'))\n\
         println([a: 1].subMap(['z']))\nprintln([a: 1, b: 2] - [a: 1])\n\
         println([a: 1, b: 2] - [a: 9])\nprintln([a: 1, b: 2].intersect([a: 1]))",
    );
    assert_eq!(out, "[a:1]\n[a:1, b:2]\n[:]\n[b:2]\n[a:1, b:2]\n[a:1]\n");
}

#[test]
fn string_translation_indent_and_margin_stripping() {
    // `tr` expands ranges on both sides (a reversed one too) and repeats the
    // last replacement; `stripIndent` opts out of the outdent when the string
    // ends in a line terminator, as Java's does.
    let (out, _) = run(
        "println('abc'.tr('a-c', 'x-z'))\nprintln('abc'.tr('abc', 'z'))\n\
         println('abc'.tr('c-a', 'x-z'))\n\
         println('  a\\n   b'.stripIndent() + '|')\n\
         println('  a\\n  b\\n'.stripIndent() + '|')\n\
         println('    a\\n    b'.stripIndent(2) + '|')\n\
         println('|a\\n |b'.stripMargin())",
    );
    assert_eq!(out, "xyz\nzzz\nzyx\na\n b|\n  a\n  b\n|\n  a\n  b|\na\nb\n");
}

#[test]
fn string_conversion_predicates_and_tab_expansion() {
    let (out, _) = run(
        "println([' 42'.isInteger(), '4.2'.isInteger(), '42'.isBigInteger(), '4.2'.isBigInteger()])\n\
         println('a\\tb'.expand() + '|')\nprintln('a\\tb'.expand(4) + '|')\n\
         println('abcb'.minus('b'))\nprintln('a1b2'.minus(~/\\d/))\nprintln('%s-%d'.formatted('a', 1))",
    );
    assert_eq!(
        out,
        "[true, false, true, false]\na       b|\na   b|\nacb\nab2\na-1\n"
    );
}

#[test]
fn scaled_rounding_truncation_and_power() {
    // `round(n)`/`trunc(n)` keep the receiver's type; `trunc()` with no scale
    // answers a `BigInteger`.
    let (out, _) = run(
        "println(3.14.round(1))\nprintln(3.15d.round(1))\nprintln(3.7.trunc())\n\
         println((-3.7).trunc())\nprintln(3.789.trunc(2))\nprintln(3.7d.trunc())\n\
         println(10.power(3))\nprintln(2.power(-1))\nprintln(2.0.power(3))",
    );
    assert_eq!(out, "3.1\n3.2\n3\n-3\n3.78\n3.0\n1000\n0.5\n8.000\n");
}

#[test]
fn closure_currying_composition_and_memoization() {
    // `rcurry` counts its insertion point back from the end of the supplied
    // arguments, and `memoize` runs the body once per distinct argument list.
    let (out, _) = run(
        "def add = { a, b -> a + b }\nprintln add.curry(1)(2)\nprintln add.curry(1, 2)()\n\
         def sub = { a, b -> a - b }\nprintln sub.rcurry(1)(5)\n\
         def three = { a, b, c -> \"$a$b$c\" }\nprintln three.ncurry(1, 'X')('a', 'c')\n\
         def n = 0\ndef twice = { n++; it * 2 }.memoize()\nprintln twice(3)\nprintln twice(3)\nprintln n\n\
         def inc = { it + 1 }\ndef dbl = { it * 2 }\nprintln inc.andThen(dbl)(3)\nprintln inc.compose(dbl)(3)\n\
         println((inc << dbl)(3))",
    );
    assert_eq!(out, "3\n3\n4\naXc\n6\n6\n1\n8\n7\n7\n");
}

#[test]
fn with_and_tap_make_the_receiver_the_closures_delegate() {
    // Groovy's `OWNER_FIRST`: the script is tried first (so a same-named
    // closure wins), then the delegate. A mutator writes through, which is what
    // `tap` answers.
    let (out, _) = run("def m = [:]\nm.with { put('a', 1) }\nprintln m\n\
         println([1, 2].with { size() })\nprintln('abc'.with { toUpperCase() })\n\
         println([1, 2].tap { add(3) })\nprintln([1, 2].with { [3].with { size() } })\n\
         def size = { 99 }\nprintln([1, 2].with { size() })\n\
         try { [1, 2].with { zork() } } catch (t) { println t.getClass().getName() }");
    assert_eq!(
        out,
        "[a:1]\n2\nABC\n[1, 2, 3]\n1\n99\ngroovy.lang.MissingMethodException\n"
    );
}

#[test]
fn a_bare_name_inside_with_resolves_against_the_delegate() {
    // The property form of the delegate dispatch the bare-*call* form already
    // did. Groovy's `OWNER_FIRST` asks the owner first, so a script binding
    // still wins; a name nothing in the script binds reaches the delegate and
    // is asked for as a property, which is why a missing map key answers
    // `null` and a list — whose property read is a spread — raises.
    let (out, _) = run("println([a: 1].with { a })\n\
         println([a: 1].tap { a })\n\
         println([a: 1].with { nokey })\n\
         println([a: 1].with { [b: 2].with { \"\" + a + \"/\" + b } })\n\
         def owner = 100\n\
         println([owner: 1].with { owner })\n\
         try { println([1, 2, 3].with { size }) } catch (t) { println t.getClass().getName() }");
    assert_eq!(
        out,
        "1\n[a:1]\nnull\nnull/2\n100\ngroovy.lang.MissingPropertyException\n"
    );
}

#[test]
fn a_bare_write_inside_with_goes_to_the_delegate_not_a_script_binding() {
    // Every site that writes a bare name has to reach the delegate, not just
    // the plain `=`: a compound assignment and `++`/`--` read and write the
    // same name, and a subscript or mutating-method write-back stores the
    // receiver's new contents back through it. Writing any of them to a script
    // binding instead left the delegate holding its original value *and* leaked
    // the name into the script.
    let (out, _) = run("def m1 = [a: 1]\nm1.with { a = 9 }\nprintln m1\n\
         def m2 = [a: 1]\nm2.with { b = 7 }\nprintln m2\n\
         def m3 = [a: 1]\nm3.with { a += 5 }\nprintln m3\n\
         def m4 = [a: 1]\nm4.with { a++ }\nprintln m4\n\
         def m5 = [a: 1]\nm5.with { a-- }\nprintln m5\n\
         def m6 = [q: [9, 9]]\nm6.with { q[0] = 5 }\nprintln m6\n\
         def m7 = [lst: [3, 1, 2]]\nm7.with { lst.sort() }\nprintln m7");
    assert_eq!(
        out,
        "[a:9]\n[a:1, b:7]\n[a:6]\n[a:2]\n[a:0]\n[q:[5, 9]]\n[lst:[1, 2, 3]]\n"
    );
}

#[test]
fn a_delegate_that_cannot_hold_the_name_does_not_raise_on_write() {
    // Groovy accepts `[1, 2].with { zork = 1 }` silently — a list holds no such
    // property, and the write falls through to the script rather than raising.
    let (out, ok) = run("[1, 2].with { zork = 1 }\nprintln 'ok'");
    assert!(ok);
    assert_eq!(out, "ok\n");
}

#[test]
fn an_ordinary_script_variable_inside_a_closure_keeps_its_native_ops() {
    // The delegate-aware builtins are emitted only for a name the compiler
    // cannot bind. A script variable must keep `GetVar`/`SetVar`, because a
    // builtin in the closure body would cost the loop its JIT trace
    // eligibility — the reason the compile-time gate exists at all.
    let dir = std::env::temp_dir();
    let src = "def total = 0\n[1, 2, 3].each { total += it }\nprintln total\n";
    let path = dir.join(format!("groovyrs_test_{}.groovy", fasthash(src)));
    std::fs::write(&path, src).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_groovy"))
        .arg("--disasm")
        .arg(&path)
        .output()
        .expect("spawn groovy");
    let _ = std::fs::remove_file(&path);
    let dis = String::from_utf8_lossy(&out.stdout);
    assert!(
        !dis.contains("CallBuiltin(760") && !dis.contains("CallBuiltin(761"),
        "a script-bound name must not go through the delegate builtins:\n{dis}"
    );
    assert!(dis.contains("GetVar") || dis.contains("SetVar"));
}

#[test]
fn right_shift_composes_two_closures() {
    // `f >> g` is `Closure.andThen`: `f` runs first, so `(f >> g)(3)` is
    // `g(f(3))`. `<<` composes the other way. Before this, `>>` lowered
    // unconditionally to the native shift ops, which coerced both `Value::Obj`
    // handles to integers and answered a number.
    let (out, ok) = run(concat!(
        "def f = { it + 1 }\n",
        "def g = { it * 2 }\n",
        "def h = { it - 3 }\n",
        "println((f >> g)(3))\n",
        "println((f << g)(3))\n",
        "println((f >> g >> h)(3))\n",
        "println(({ it + 1 } >> { it * 2 })(3))\n",
        "Closure p = f\n",
        "println((p >> g)(3))\n",
    ));
    assert!(ok);
    assert_eq!(out, "8\n7\n5\n8\n8\n");
}

#[test]
fn a_composed_closure_keeps_the_first_closures_arity() {
    let (out, _) =
        run("def add = { a, b -> a + b }\ndef show = { \"=$it\" }\nprintln((add >> show)(2, 3))");
    assert_eq!(out, "=5\n");
}

#[test]
fn shift_operators_dispatch_a_user_class_overload() {
    // `+` reaches a `plus` overload through fusevm's numeric hook, but `NumOp`
    // has no shift member, so `<<`/`>>` dispatch `leftShift`/`rightShift` from
    // the shift builtins instead. A class without the method raises, as Groovy
    // does — `>>` used to answer `0` and `<<` to report the wrong receiver.
    let (out, ok) = run(concat!(
        "class Pipe {\n",
        "  String name\n",
        "  Pipe(String n) { name = n }\n",
        "  def rightShift(Pipe o) { new Pipe(name + '|' + o.name) }\n",
        "  def leftShift(o) { 'into:' + o }\n",
        "  String toString() { name }\n",
        "}\n",
        "def a = new Pipe('a')\n",
        "println(a >> new Pipe('b'))\n",
        "println(a << 5)\n",
        "println(new Pipe('x') >> new Pipe('y'))\n",
        "class Bare { def v = 1 }\n",
        "try { println(new Bare() >> 2) } catch (e) { println(e.getClass().getName()) }\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        "a|b\ninto:5\nx|y\ngroovy.lang.MissingMethodException\n"
    );
}

#[test]
fn a_numeric_right_shift_keeps_its_native_ops() {
    // The composition builtin is emitted only where the compiler can see the
    // left operand is a closure or an instance. An ordinary shift must stay on
    // `Shl`/`Shr`/`BitAnd`, because a builtin in a shifting loop would cost it
    // the JIT trace — the reason `>>` is not routed unconditionally the way
    // `<<` is.
    let dir = std::env::temp_dir();
    let src =
        "def x = 1024\ndef n = 0\nfor (int i = 0; i < 4; i++) { n = n + (x >> i) }\nprintln n\n";
    let path = dir.join(format!("groovyrs_test_{}.groovy", fasthash(src)));
    std::fs::write(&path, src).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_groovy"))
        .arg("--disasm")
        .arg(&path)
        .output()
        .expect("spawn groovy");
    let _ = std::fs::remove_file(&path);
    let dis = String::from_utf8_lossy(&out.stdout);
    assert!(
        !dis.contains(&format!("CallBuiltin({}", groovyrs::host::GSHR)),
        "a numeric `>>` must not go through the composition builtin:\n{dis}"
    );
    assert!(dis.contains("Shr"), "{dis}");
    // And a name re-bound from a closure to a number shifts as a number.
    let (out, _) = run("def q = { it }\nq = 64\nprintln(q >> 2)");
    assert_eq!(out, "16\n");
}

#[test]
fn to_string_with_an_argument_resolves_the_static_integer_overload() {
    // Java's overload resolution admits `255.toString(16)` as the *static*
    // `Integer.toString(int)` — `Integer` has no instance `toString(int)` — so
    // the receiver is discarded and the argument is rendered in base 10: `16`,
    // not `ff`. The two-argument form is the radix one. A radix outside 2..36
    // falls back to base 10 instead of raising.
    let (out, ok) = run(concat!(
        "println([255.toString(16), 255.toString(16, 2), 10.toString(16), (-255).toString(16)])\n",
        "println([255.toString(1), 255.toString(37), 255.toString(36), 255.toString(0)])\n",
        "println([255L.toString(16), 255L.toString(16, 2), 255.toString(16).getClass()])\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        "[16, 10000, 16, 16]\n[1, 37, 36, 0]\n[16, 10000, class java.lang.String]\n"
    );
}

#[test]
fn integer_and_long_statics_take_a_radix() {
    let (out, ok) = run(concat!(
        "println([Integer.toString(255, 16), Integer.toString(255), Long.toString(255, 16), Long.toString(255)])\n",
        "println([Integer.toString(255,1), Integer.toString(255,37), Integer.toString(-255,16), Integer.toString(255,2)])\n",
        "println([Integer.parseInt('ff',16), Integer.parseInt('-ff',16), Integer.parseInt('FF',16), Long.parseLong('ff',16), Integer.valueOf('ff',16)])\n",
        "try { println(Integer.parseInt('zz',16)) } catch(e) { println('EXC:'+e.getClass().getSimpleName()) }\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        "[ff, 255, ff, 255]\n[255, 255, -ff, 11111111]\n[255, -255, 255, 255, 255]\nEXC:NumberFormatException\n"
    );
}

#[test]
fn the_unsigned_renderings_fill_to_the_named_classs_width() {
    // `Integer.toHexString(-1)` is `ffffffff` — 32 bits — where the same value
    // through `Long` runs to 64. Rendering both at 64 (a bare `{:x}` over the
    // host `i64`) made every negative `Integer` sixteen digits wide.
    let (out, ok) = run(concat!(
        "println([Integer.toHexString(-1), Long.toHexString(-1L), Integer.toBinaryString(-1), Integer.toOctalString(-1)])\n",
        "println([Long.toHexString(255), Long.toBinaryString(5), Long.toOctalString(8), Integer.toHexString(255)])\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        "[ffffffff, ffffffffffffffff, 11111111111111111111111111111111, 37777777777]\n[ff, 101, 10, ff]\n"
    );
}

#[test]
fn big_integer_converts_its_receiver_and_stays_one_through_unary_minus() {
    // `BigInteger` is the one type here with a real instance `toString(int
    // radix)`, so it is the spelling that answers `ff`. `BigDecimal` has no such
    // overload and neither takes two arguments. Unary `-` must keep a
    // `BigInteger` a `BigInteger` — it used to answer a `BigDecimal`, which
    // silently changed the type and lost the radix overload with it.
    let (out, ok) = run(concat!(
        "println([255G.toString(2), (-255G).toString(16), 255G.toString(36), 255G.toString(1), 255G.toString(16)])\n",
        "println([(-255G).getClass(), (255G + 1G).getClass(), 255G.negate().getClass(), (-3.14G).getClass()])\n",
        "try { println(255G.toString(16, 2)) } catch(e) { println('EXC:'+e.getClass().getSimpleName()) }\n",
        "try { println(3.14.toString(16)) } catch(e) { println('EXC:'+e.getClass().getSimpleName()) }\n",
        "println([255.toString(), 3.14.toString(), [1,2].toString(), 255G.toString()])\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "[11111111, -ff, 73, 255, ff]\n",
            "[class java.math.BigInteger, class java.math.BigInteger, class java.math.BigInteger, class java.math.BigDecimal]\n",
            "EXC:MissingMethodException\n",
            "EXC:MissingMethodException\n",
            "[255, 3.14, [1, 2], 255]\n"
        )
    );
}

#[test]
fn subsequences_answers_a_hash_set_in_the_jdks_bucket_order() {
    // `subsequences()` is a `java.util.HashSet<List>`, so it prints in the JDK's
    // table order — `List.hashCode` (`31 * acc + element`) spread through
    // `(cap - 1) & (h ^ (h >>> 16))` — which is neither generation nor sorted
    // order. The answer is grown one element at a time and each round's set is
    // traversed in *its* bucket order, so the intermediate steps have to be
    // reproduced too, not only the last.
    let (out, ok) = run(concat!(
        "println([1,2,3].subsequences())\n",
        "println([1,2,3].subsequences().getClass())\n",
        "println([1,2].subsequences())\n",
        "println([1,2,3,4].subsequences())\n",
        "println(['a','b','c'].subsequences())\n",
        "println([1,1,2].subsequences())\n",
        "println([].subsequences())\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "[[1], [1, 2, 3], [2], [2, 3], [1, 2], [3], [1, 3]]\n",
            "class java.util.HashSet\n",
            "[[1], [2], [1, 2]]\n",
            "[[1], [1, 3, 4], [1, 2, 3], [2], [2, 3, 4], [1, 2, 4], [3, 4], [2, 3], [1, 2], [3], [2, 4], [1, 3], [4], [1, 4], [1, 2, 3, 4]]\n",
            "[[a, b, c], [a], [b], [b, c], [a, b], [c], [a, c]]\n",
            "[[1], [1, 1, 2], [1, 1], [2], [1, 2]]\n",
            "[]\n"
        )
    );
}

#[test]
fn permutations_answers_a_hash_set_too() {
    // Same story as `subsequences`: `permutations()` is a `HashSet`, so it
    // de-duplicates and its order depends on the *input*, not only on the
    // element set. It used to answer an `ArrayList` in generation order.
    // `combinations()` really is a `List`, and the closure form of
    // `permutations` is `collect` over the set.
    let (out, ok) = run(concat!(
        "println([1,2].permutations())\n",
        "println([1,2].permutations().getClass())\n",
        "println([1,2,3].permutations())\n",
        "println([3,1,2].permutations())\n",
        "println([1,1].permutations())\n",
        "println([1,2,3].permutations { it.join('') })\n",
        "println([1,2,3].permutations { it.join('') }.getClass())\n",
        "println([[1,2],[3,4]].combinations().getClass())\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "[[2, 1], [1, 2]]\n",
            "class java.util.HashSet\n",
            "[[1, 2, 3], [3, 2, 1], [2, 1, 3], [3, 1, 2], [1, 3, 2], [2, 3, 1]]\n",
            "[[3, 2, 1], [1, 2, 3], [3, 1, 2], [2, 1, 3], [1, 3, 2], [2, 3, 1]]\n",
            "[[1, 1]]\n",
            "[123, 321, 213, 312, 132, 231]\n",
            "class java.util.ArrayList\n",
            "class java.util.ArrayList\n"
        )
    );
}

#[test]
fn to_string_rejects_an_argument_that_matches_no_overload() {
    // Both `Integer.toString` parameters are `int`. A `String` argument narrows
    // to neither, so Groovy raises instead of rendering — it used to answer the
    // base-10 `0` that a failed coercion left behind.
    let (out, ok) = run(
        "try { println(255.toString('x')) } catch(e) { println('EXC:'+e.getClass().getSimpleName()) }",
    );
    assert!(ok);
    assert_eq!(out, "EXC:MissingMethodException\n");
}

#[test]
fn sublist_is_a_live_view_of_the_backing_list() {
    // Java's `subList` answers a `java.util.ArrayList$SubList`, not a copy: a
    // write through the window reaches the backing list and a write to the
    // backing list shows through the window. groovyrs answered a detached copy,
    // so both directions were invisible. Frozen from Apache Groovy 5.0.8.
    let (out, ok) = run(concat!(
        "def a = [1, 2, 3, 4]; def s = a.subList(1, 3)\n",
        "println([s.getClass().getName(), s.getClass().getSimpleName(), s, s.size()])\n",
        "s.set(0, 99); println([a, s])\n",
        "a.set(1, 42); println s\n",
        "println([a.is(s), s.is(s), a.subList(1, 3).is(a.subList(1, 3))])\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "[java.util.ArrayList$SubList, SubList, [2, 3], 2]\n",
            "[[1, 99, 3, 4], [99, 3]]\n",
            "[42, 3]\n",
            "[false, true, false]\n"
        )
    );
}

#[test]
fn a_structural_write_through_a_window_resizes_the_backing_list() {
    // Adding and removing through the window splices the backing list at the
    // window's offset, and the window resizes with it rather than going stale —
    // the JDK's `updateSizeAndModCount`. Frozen from Apache Groovy 5.0.8.
    let (out, ok) = run(concat!(
        "def a = [1, 2, 3, 4]; def s = a.subList(1, 3); s.add(99); println([a, s, a.size()])\n",
        "def b = [1, 2, 3, 4]; def t = b.subList(1, 3); t.remove(0); println([b, t])\n",
        "def c = [1, 2, 3, 4]; def u = c.subList(1, 3); u.clear(); println([c, u])\n",
        "def d = [1, 2]; def v = d.subList(2, 2); v.add(9); println([d, v])\n",
        "def e = [5, 4, 3, 2, 1]; def w = e.subList(1, 4); println([w.is(w.sort()), e, w])\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "[[1, 2, 3, 99, 4], [2, 3, 99], 5]\n",
            "[[1, 3, 4], [3]]\n",
            "[[1, 4], []]\n",
            "[[1, 2, 9], [9]]\n",
            "[true, [5, 2, 3, 4, 1], [2, 3, 4]]\n"
        )
    );
}

#[test]
fn a_window_onto_a_window_reaches_the_root_list() {
    // A nested window points at the *root* list with an absolute offset, and a
    // structural write through it resizes every window it was taken through.
    // Frozen from Apache Groovy 5.0.8.
    let (out, ok) = run(concat!(
        "def a = [1, 2, 3, 4, 5]; def s = a.subList(1, 4); def t = s.subList(1, 3)\n",
        "println([t.getClass().getName(), t])\n",
        "t.set(0, 77); println([a, s, t])\n",
        "t.add(9); println([a, s, t])\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "[java.util.ArrayList$SubList, [3, 4]]\n",
            "[[1, 2, 77, 4, 5], [2, 77, 4], [77, 4]]\n",
            "[[1, 2, 77, 4, 9, 5], [2, 77, 4, 9], [77, 4, 9]]\n"
        )
    );
}

#[test]
fn a_structural_change_to_the_backing_list_invalidates_a_window() {
    // Java's fail-fast rule: once the backing list's `modCount` moves past the
    // value the window synced to, every read or write through the window is a
    // `ConcurrentModificationException` — permanently, and with a `null`
    // message. A window taken *after* the change is fine. Frozen from Apache
    // Groovy 5.0.8.
    let (out, ok) = run(concat!(
        "def a = [1, 2, 3, 4]; def s = a.subList(1, 3); a.add(5)\n",
        "try { println s } catch (e) { println([e.getClass().getName(), e.getMessage()]) }\n",
        "try { println s.size() } catch (e) { println e.getClass().getName() }\n",
        "try { for (x in s) println x } catch (e) { println e.getClass().getName() }\n",
        "try { println s[0] } catch (e) { println e.getClass().getName() }\n",
        "try { s[0] = 1 } catch (e) { println e.getClass().getName() }\n",
        "try { if (s) println 'live' } catch (e) { println e.getClass().getName() }\n",
        "try { println(2 in s) } catch (e) { println e.getClass().getName() }\n",
        "try { println([s]) } catch (e) { println e.getClass().getName() }\n",
        "try { println s.subList(0, 1) } catch (e) { println e.getClass().getName() }\n",
        "println a.subList(1, 3)\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        format!(
            "[java.util.ConcurrentModificationException, null]\n{}[2, 3]\n",
            "java.util.ConcurrentModificationException\n".repeat(8)
        )
    );
}

#[test]
fn getclass_and_is_still_answer_on_an_invalidated_window() {
    // The two calls that read the *reference* rather than the elements. Groovy
    // answers both after the backing list has moved on, so the comodification
    // check must not sit in front of them. Frozen from Apache Groovy 5.0.8.
    let (out, ok) = run(concat!(
        "def a = [1, 2, 3, 4]; def s = a.subList(1, 3); a.add(5)\n",
        "println([s.getClass().getName(), s.is(s), s instanceof List])\n",
    ));
    assert!(ok);
    assert_eq!(out, "[java.util.ArrayList$SubList, true, true]\n");
}

#[test]
fn only_a_structural_change_invalidates_a_window() {
    // The boundary the whole rule turns on. `set`, `swap`, a `removeAll` that
    // removed nothing and `sort(false)` leave `modCount` alone; `sort()` on the
    // LIST bumps it while `sort()` on a WINDOW does not (`List.sort`'s default
    // reorders through `set`); `addAll([])` bumps it on the list but not on a
    // window; and `unique()` returns early below two elements while the closure
    // form never does. Each answer is Apache Groovy 5.0.8's, measured.
    let cme = "java.util.ConcurrentModificationException";
    let (out, ok) = run(concat!(
        "def a = [1, 2, 3, 4]; def s = a.subList(1, 3)\n",
        "a[0] = 9; a.swap(0, 3); a.removeAll([99]); a.sort(false); println s\n",
        "def b = [3, 1, 2, 0]; def p = b.subList(0, 3); def q = b.subList(1, 3)\n",
        "p.sort(); println([b, q])\n",
        "def c = [3, 1, 2, 0]; def r = c.subList(1, 3); c.sort()\n",
        "try { println r } catch (e) { println e.getClass().getName() }\n",
        "def d = [1, 2, 3, 4]; def e1 = d.subList(0, 3); def f = d.subList(1, 3)\n",
        "println([e1.addAll([]), f])\n",
        "def g = [1, 2, 3]; def h = g.subList(0, 2); g.addAll([])\n",
        "try { println h } catch (e) { println e.getClass().getName() }\n",
        "def i = [1, 2, 3, 4]; def j = i.subList(0, 1); def k = i.subList(2, 4)\n",
        "j.unique(); j.unique(true); println k\n",
        "def l = [1, 2, 3, 4]; def m = l.subList(0, 1); def n = l.subList(2, 4); m.unique { it }\n",
        "try { println n } catch (e) { println e.getClass().getName() }\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        format!("[2, 3]\n[[1, 2, 3, 0], [2, 3]]\n{cme}\n[false, [2, 3]]\n{cme}\n[3, 4]\n{cme}\n")
    );
}

#[test]
fn add_all_answers_whether_the_list_changed_and_honours_an_index() {
    // `Collection.addAll` answers whether anything was added, so an empty
    // argument is `false` — groovyrs answered `true` unconditionally. The
    // two-argument `addAll(index, collection)` inserts at the index; it used to
    // read its *index* as the collection and append that, so
    // `[1, 2].addAll(1, [8, 9])` left `[1, 2, 1]`. Frozen from Apache Groovy 5.0.8.
    let (out, ok) = run(concat!(
        "println([[1, 2].addAll([]), [1, 2].addAll([3]), [1, 2].addAll(1, [8, 9]), [1, 2].addAll(1, [])])\n",
        "def a = [1, 2]; a.addAll(1, [8, 9]); println a\n",
        "def b = [1, 2]; b.addAll([3]); println b\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "[false, true, true, false]\n",
            "[1, 8, 9, 2]\n",
            "[1, 2, 3]\n"
        )
    );
}

#[test]
fn a_long_argument_matches_no_integer_to_string_overload() {
    // `Integer.toString(int)` renders its argument and discards the receiver, so
    // `255.toString(16)` is `16`. `255.toString(16L)` reaches no overload at all
    // — Java resolves on the declared parameter width, and a `Long` does not
    // narrow to an `int` — where groovyrs used to answer `16` for both, the two
    // arguments being the one `Value::Int`. The widths ride the call site.
    let (out, ok) = run(concat!(
        "println(255.toString(16))\n",
        "try { println(255.toString(16L)) } catch(e) { println('EXC:'+e.getMessage().split('\\n')[0]) }\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "16\n",
            "EXC:No signature of method: toString for class: java.lang.Integer \
             is applicable for argument types: (Long) values: [16]\n"
        )
    );
}

#[test]
fn a_long_receiver_admits_the_long_argument_that_an_integer_receiver_rejects() {
    // The four signatures are `Integer.toString(int)`, `Integer.toString(int,
    // int)`, `Long.toString(long)` and `Long.toString(long, int)`, so a `Long`
    // fits in exactly one position — the first argument of the `Long` pair. The
    // radix parameter is an `int` in both classes, which is why the two-argument
    // `Long` receiver still rejects a `Long` radix.
    let (out, ok) = run(concat!(
        "println(255L.toString(16L))\n",
        "println(255L.toString(16, 2))\n",
        "try { println(255L.toString(16, 2L)) } catch(e) { println('EXC:'+e.getMessage().split('\\n')[0]) }\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "16\n",
            "10000\n",
            "EXC:No signature of method: toString for class: java.lang.Long \
             is applicable for argument types: (Integer, Long) values: [16, 2]\n"
        )
    );
}

#[test]
fn the_width_of_a_to_string_argument_survives_a_variable() {
    // The width is the compiler's static reading of the *expression*, not of the
    // literal, so a `Long` that reaches the call through a declaration is still
    // a `Long` at the call.
    let (out, ok) = run(concat!(
        "def r = 16L\n",
        "def s = 16\n",
        "try { println(255.toString(r)) } catch(e) { println('EXC:'+e.getClass().getSimpleName()) }\n",
        "println(255.toString(s))\n",
    ));
    assert!(ok);
    assert_eq!(out, "EXC:MissingMethodException\n16\n");
}

#[test]
fn casting_null_to_a_primitive_unboxes_and_raises() {
    // Groovy casts the null to the wrapper and then unboxes it, so `null as int`
    // is the JVM's unboxing NullPointerException. groovyrs used to coerce the
    // null instead and answer `0` / `NaN`. The message ends at `because "`: the
    // helpful-NPE text names the local it read, and the local a cast reads is
    // synthetic and unnamed.
    let (out, ok) = run(concat!(
        "def x = null\n",
        "try { println(x as int) } catch(e) { println(e.getClass().getName()+'|'+e.getMessage()) }\n",
        "try { println(x as double) } catch(e) { println(e.getClass().getName()+'|'+e.getMessage()) }\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "java.lang.NullPointerException|Cannot invoke \"java.lang.Integer.intValue()\" because \"\n",
            "java.lang.NullPointerException|Cannot invoke \"java.lang.Double.doubleValue()\" because \"\n"
        )
    );
}

#[test]
fn casting_null_to_a_reference_type_keeps_the_null() {
    // The wrappers, `String`, and the collections take the null unchanged —
    // `null as List` was `[]` and `null as Integer` was `0`. `boolean` is the one
    // primitive that does not unbox: Groovy truth-tests it, so it is `false`.
    let (out, ok) = run(concat!(
        "def x = null\n",
        "println([x as Integer, x as Long, x as String, x as List, x as Set])\n",
        "println(x as boolean)\n",
    ));
    assert!(ok);
    assert_eq!(out, "[null, null, null, null, null]\nfalse\n");
}

#[test]
fn a_throw_from_a_discarded_statement_reaches_the_enclosing_catch() {
    // A throw raised by fusevm's numeric hook has no check of its own — the hook
    // runs inside the dispatch loop for a *native* arithmetic op, and only a
    // builtin call is followed by a pending-exception check. A discarded
    // expression statement may have no builtin call after it, and the throw then
    // left the `try` entirely.
    let (out, ok) = run(concat!(
        "def y = null\n",
        "try { y % 3 } catch(e) { println('caught:'+e.getClass().getSimpleName()) }\n",
    ));
    assert!(ok);
    assert_eq!(out, "caught:NullPointerException\n");
}

#[test]
fn a_discarded_statements_throw_stops_the_statements_after_it() {
    // Where a later call did happen to notice the pending throw, the statements
    // in between had already run and printed. The check belongs at the end of the
    // statement that raised, not at the next call.
    let (out, ok) = run(concat!(
        "def y = null\n",
        "try { y - 1; println('unreachable') } catch(e) { println('caught') }\n",
    ));
    assert!(ok);
    assert_eq!(out, "caught\n");
}

#[test]
fn a_throw_from_a_declaration_or_a_condition_reaches_the_catch() {
    // The same gap in the other two places a native arithmetic result is
    // consumed without a following builtin call: a variable store, and a
    // statically-boolean condition — which would otherwise pick a branch from a
    // value that was never computed.
    let (out, ok) = run(concat!(
        "def y = null\n",
        "try { def z = y * 2; println(z) } catch(e) { println('caught:decl') }\n",
        "try { if (y % 3 == 0) { println('then') } else { println('else') } } catch(e) { println('caught:if') }\n",
        "try { while (y - 1 > 0) { break }; println('after') } catch(e) { println('caught:while') }\n",
    ));
    assert!(ok);
    assert_eq!(out, "caught:decl\ncaught:if\ncaught:while\n");
}

#[test]
fn double_equals_compares_bits_so_nan_equals_itself_and_zero_is_signed() {
    // `Double.equals(Object)` was missing entirely — every call answered
    // `MissingMethodException`, including `Double.NaN.equals(Double.NaN)`.
    //
    // It is not `==`. It compares `doubleToLongBits`, so it disagrees with `==`
    // at exactly the two values IEEE treats specially, in opposite directions:
    // NaN equals itself under `equals` (every payload folds to one canonical
    // pattern) and `-0.0` does NOT equal `+0.0`, which is the reverse of what
    // `==` says about both. It is also typed — only another `Double` can be
    // equal, so the `BigDecimal` `1.5` and the `Integer` `1` are not.
    //
    // The last two lines pin the `equals`/`hashCode` contract that the bit rule
    // exists to keep: two values that are `equals` must hash alike, and the
    // canonical-NaN fold in `double_hash` is the same fold used here.
    //
    // Every line is the stdout of Apache Groovy 5.0.8 on JDK 21.
    let (out, ok) = run(concat!(
        "println([ Double.NaN.equals(Double.NaN), (0.0d).equals(-0.0d), (-0.0d).equals(0.0d) ])\n",
        "println([ (0.0d).equals(0.0d), (1.5d).equals(1.5d), (1.5d).equals(2.0d) ])\n",
        "println([ (1.5d).equals(1.5), (1.0d).equals(1), (1.5d).equals(\"x\"), (1.5d).equals(null) ])\n",
        "println([ Double.POSITIVE_INFINITY.equals(Double.POSITIVE_INFINITY), \
         Double.POSITIVE_INFINITY.equals(Double.NEGATIVE_INFINITY) ])\n",
        "println([ Double.NaN.equals(Double.NaN), Double.NaN.hashCode() == Double.NaN.hashCode() ])\n",
        "println([ (0.0d).equals(-0.0d), (0.0d).hashCode() == (-0.0d).hashCode() ])\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "[true, false, false]\n",
            "[true, true, false]\n",
            "[false, false, false, false]\n",
            "[true, false]\n",
            "[true, true]\n",
            "[false, false]\n",
        )
    );
}

#[test]
fn hashcode_answers_javas_specified_contract_for_every_value_shape() {
    // `hashCode()` was missing entirely — every receiver answered
    // `MissingMethodException`. Each line below is the stdout of Apache Groovy
    // 5.0.8 (JVM 17.0.4.1) on the same expression, so it pins the *rule*, not
    // just a number: `String` folds UTF-16 code units (the astral case needs the
    // surrogate pair, not the scalar), `Long` folds its halves where `Integer`
    // does not, `Double` folds `doubleToLongBits`, `BigDecimal` carries its
    // scale (1.5 and 1.50 differ), `BigInteger` folds magnitude words
    // big-endian, `AbstractList` seeds at 1 and multiplies by 31, `AbstractMap`
    // sums `key ^ value`, `AbstractSet` sums its elements (so a `LinkedHashSet`
    // and the `TreeSet` of the same elements agree), and `IntRange` uses a
    // Cantor pairing of its normalised bounds while every other range shape
    // inherits `AbstractList`'s.
    let (out, ok) = run(concat!(
        "println([ \"\".hashCode(), \"a\".hashCode(), \"hello\".hashCode(), \"café éè 中\".hashCode(), \"a😀b\".hashCode() ])\n",
        "println([ 0.hashCode(), 1.hashCode(), (-1).hashCode(), 2147483647.hashCode(), (-2147483648).hashCode() ])\n",
        "println([ 4294967296L.hashCode(), 9223372036854775807L.hashCode(), (-9223372036854775808L).hashCode() ])\n",
        "println([ true.hashCode(), false.hashCode() ])\n",
        "println([ (1.0d).hashCode(), (-0.0d).hashCode(), (3.14d).hashCode(), Double.NaN.hashCode(), Double.POSITIVE_INFINITY.hashCode(), Double.NEGATIVE_INFINITY.hashCode() ])\n",
        "println([ (1.5G).hashCode(), (1.50G).hashCode(), (0.1G).hashCode(), (-3.14G).hashCode(), (1.5e10G).hashCode() ])\n",
        "println([ (0G).hashCode(), (1G).hashCode(), (-1G).hashCode(), (100G).hashCode(), (12345678901234567890G).hashCode(), ((2G)**70).hashCode() ])\n",
        "println([ [].hashCode(), [1].hashCode(), [1,2,3].hashCode(), [1,[2,3]].hashCode(), [\"a\",\"b\"].hashCode(), [null].hashCode(), [[1,2],[3,4]].hashCode() ])\n",
        "println([ [:].hashCode(), [a:1].hashCode(), [a:1,b:2].hashCode(), [a:[1,2]].hashCode(), [a:[b:1]].hashCode() ])\n",
        "println([ ([] as Set).hashCode(), ([1,2,3] as Set).hashCode(), ([1,2,3] as LinkedHashSet).hashCode(), (new TreeSet([1,2,3])).hashCode(), ([[1,2]] as Set).hashCode() ])\n",
        "println([ (0..0).hashCode(), (1..1).hashCode(), (1..3).hashCode(), (3..1).hashCode(), (1..<3).hashCode(), (0..65535).hashCode(), (46340..46340).hashCode(), (100000..100000).hashCode(), (-5..-1).hashCode() ])\n",
        "println([ (1..<1).hashCode(), ('a'..'c').hashCode(), ('c'..'a').hashCode(), (1.0..3.0).hashCode(), (1.5G..3.5G).hashCode() ])\n",
        "println([ [1,2,3,4].subList(1,3).hashCode(), [(1..3)].hashCode(), [true].hashCode(), [1.0d].hashCode(), [1.5G].hashCode(), [4294967296L].hashCode() ])\n",
        "println([ \"abc\".hashCode() == \"abc\".hashCode(), [1,2] == [1,2], [1,2].hashCode() == [1,2].hashCode(), (1..3).hashCode() == [1,2,3].hashCode() ])\n",
        "println(\"x\".hashCode().getClass().getName())\n",
        "def n = null\n",
        "try { n.hashCode() } catch (e) { println([e.getClass().getName(), e.getMessage()]) }\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "[0, 97, 99162322, 1447970315, 57849694]\n",
            "[0, 1, -1, 2147483647, -2147483648]\n",
            "[1, -2147483648, -2147483648]\n",
            "[1231, 1237]\n",
            "[1072693248, -2147483648, 300063655, 2146959360, 2146435072, -1048576]\n",
            "[466, 4652, 32, -9732, 456]\n",
            "[0, 1, -1, 100, -1436577082, 61504]\n",
            "[1, 32, 30817, 2018, 4066, 31, 32833]\n",
            "[0, 96, 192, 899, 2]\n",
            "[0, 6, 6, 6, 994]\n",
            "[0, 4, 13, 13, 8, 32767, -83416, 672847168, 14]\n",
            "[1, 126145, 128065, 348844, 502759]\n",
            "[1026, 44, 1262, 1072693279, 497, 32]\n",
            "[true, true, true, false]\n",
            "java.lang.Integer\n",
            "[java.lang.NullPointerException, Cannot invoke method hashCode() on null object]\n",
        )
    );
}

#[test]
fn a_user_hashcode_overrides_the_built_in_one() {
    // The universal hook sits above the per-type branches, so the guard that
    // keeps it from shadowing a declared `hashCode` is what this pins. The
    // *identity* answers are not compared to a JVM's — a JVM's own identity hash
    // varies run to run — only their contract: stable, and equal exactly when
    // the references are.
    let (out, ok) = run(concat!(
        "class P { int v; P(int v) { this.v = v }; int hashCode() { 7 * v } }\n",
        "println(new P(3).hashCode())\n",
        "def c = { -> 1 }\n",
        "println([c.hashCode() == c.hashCode(), c.hashCode() == { -> 1 }.hashCode()])\n",
        "def b = new StringBuilder('ab')\n",
        "println([b.hashCode() == b.hashCode(), b.hashCode() == new StringBuilder('ab').hashCode()])\n",
    ));
    assert!(ok);
    assert_eq!(out, "21\n[true, false]\n[true, false]\n");
}

#[test]
fn a_biginteger_power_stays_a_biginteger() {
    // `**`/`power` reached `BigDecimal`'s branch (a `BigInteger` is a scale-0
    // `BigDecimal`, so the `as_dec` test answered for it) and widened the type:
    // `2G ** 70` printed the right digits under the wrong class, which
    // `hashCode` then made visible — `BigDecimal.hashCode` multiplies
    // `BigInteger`'s by 31. Frozen from Apache Groovy 5.0.8.
    let (out, ok) = run(concat!(
        "println([2G ** 70, (2G ** 70).getClass().getName()])\n",
        "println([(2G).power(10), (2G).power(10).getClass().getName()])\n",
        "println([2G ** 0, (2G ** 0).getClass().getName()])\n",
        "println([(-2G) ** 3, ((-2G) ** 3).getClass().getName()])\n",
        "println([2G ** -1, (2G ** -1).getClass().getName()])\n",
        "println([1.5G ** 2, (1.5G ** 2).getClass().getName()])\n",
        "println([1.5G ** 0, (1.5G ** 0).getClass().getName()])\n",
        "println([2 ** 10, (2 ** 10).getClass().getName()])\n",
        "println([2 ** 40, (2 ** 40).getClass().getName()])\n",
        "println([2L ** 40, (2L ** 40).getClass().getName()])\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "[1180591620717411303424, java.math.BigInteger]\n",
            "[1024, java.math.BigInteger]\n",
            "[1, java.math.BigInteger]\n",
            "[-8, java.math.BigInteger]\n",
            "[0.5, java.lang.Double]\n",
            "[2.25, java.math.BigDecimal]\n",
            "[1, java.math.BigDecimal]\n",
            "[1024, java.lang.Integer]\n",
            "[1099511627776, java.math.BigInteger]\n",
            "[1099511627776, java.lang.Long]\n",
        )
    );
}

#[test]
fn reading_a_name_nothing_binds_raises_missing_property() {
    // Reading an undeclared name answered `null`, so every typo became a silent
    // `null` that surfaced far from its cause. Groovy raises. The cases that
    // must *keep* answering are pinned alongside: a `with` delegate is asked for
    // the property, and `[a: 1].zork` is `null` rather than a raise; a name a
    // closure wrote is a script binding afterwards; and `null` really assigned
    // is a value, not an absence. Frozen from Apache Groovy 5.0.8.
    let (out, ok) = run(concat!(
        "try { println zork } catch (e) { println([e.getClass().getName(), e.getMessage().startsWith('No such property: zork for class: ')]) }\n",
        "try { println(zork + 1) } catch (e) { println e.getClass().getName() }\n",
        "try { def y = zork } catch (e) { println e.getClass().getName() }\n",
        "try { [1].each { println nope } } catch (e) { println e.getClass().getName() }\n",
        "try { println it } catch (e) { println e.getClass().getName() }\n",
        "def m = [a: 1]\n",
        "m.with { println zork }\n",
        "m.with { println a }\n",
        "[1].each { bound = 7 }\n",
        "println bound\n",
        "w = null\n",
        "println w\n",
        "def d = [a: 1]\n",
        "d.with { b = 2 }\n",
        "println d\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "[groovy.lang.MissingPropertyException, true]\n",
            "groovy.lang.MissingPropertyException\n",
            "groovy.lang.MissingPropertyException\n",
            "groovy.lang.MissingPropertyException\n",
            "groovy.lang.MissingPropertyException\n",
            "null\n",
            "1\n",
            "7\n",
            "null\n",
            "[a:1, b:2]\n",
        )
    );
}

#[test]
fn args_is_bound_in_every_script_and_carries_the_launcher_arguments() {
    // Groovy puts `args` in every script's binding — empty, not absent, when the
    // launcher was given none — so reading it must not raise now that an unbound
    // name does. The arguments after the file reach it.
    let (out, ok) = run("println args\nprintln args.size()\nprintln(args instanceof List)");
    assert!(ok);
    assert_eq!(out, "[]\n0\ntrue\n");

    let dir = std::env::temp_dir();
    let path = dir.join("groovyrs_test_script_args.groovy");
    std::fs::write(&path, "println args\nprintln args[1]\n").unwrap();
    let got = Command::new(env!("CARGO_BIN_EXE_groovy"))
        .arg(&path)
        .arg("one")
        .arg("two")
        .output()
        .expect("spawn groovy");
    let _ = std::fs::remove_file(&path);
    assert_eq!(String::from_utf8_lossy(&got.stdout), "[one, two]\ntwo\n");
}

#[test]
fn the_script_class_name_follows_the_entry_point() {
    // The class Groovy compiles a script into is named after the *file's stem*,
    // and `groovy -e` — which has no file — uses `script_from_command_line`.
    // That name is what a bare-name `MissingPropertyException` prints, so the
    // two entry points give the same program two different messages. Both
    // measured against Apache Groovy 5.0.8.
    let dir = std::env::temp_dir();
    let path = dir.join("GroovyrsEntryPointProbe.groovy");
    let src = "try { println zork } catch (e) { println e.getMessage() }\n";
    std::fs::write(&path, src).unwrap();
    let from_file = Command::new(env!("CARGO_BIN_EXE_groovy"))
        .arg(&path)
        .output()
        .expect("spawn groovy");
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        String::from_utf8_lossy(&from_file.stdout),
        "No such property: zork for class: GroovyrsEntryPointProbe\n"
    );

    let from_eval = Command::new(env!("CARGO_BIN_EXE_groovy"))
        .arg("-e")
        .arg(src)
        .output()
        .expect("spawn groovy");
    assert_eq!(
        String::from_utf8_lossy(&from_eval.stdout),
        "No such property: zork for class: script_from_command_line\n"
    );
}

#[test]
fn reverse_true_reverses_the_receiver_in_place() {
    // `List.reverse(boolean mutate)` — the mutating spelling reverses the
    // receiver and answers it, so `a.is(a.reverse(true))`. `reverse()` and
    // `reverse(false)` copy, which is why the no-argument form cannot be
    // gated the way `sort`'s is. `Collections.reverse` reorders through `set`,
    // so it leaves `modCount` alone and a `subList` window stays live over it —
    // the last line is what pins that. Frozen from Apache Groovy 5.0.8.
    let (out, ok) = run(concat!(
        "def a = [1,2,3]; def b = a.reverse(true); println([a, b, a.is(b)])\n",
        "def c = [1,2,3]; def d = c.reverse(); println([c, d, c.is(d)])\n",
        "def e = [1,2,3]; def f = e.reverse(false); println([e, f, e.is(f)])\n",
        "def g = [1,2,3]; g.reverse(true); println g\n",
        "def h = [1,2,3,4]; def w = h.subList(1,3); h.reverse(true); println([h, w])\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "[[3, 2, 1], [3, 2, 1], true]\n",
            "[[1, 2, 3], [3, 2, 1], false]\n",
            "[[1, 2, 3], [3, 2, 1], false]\n",
            "[3, 2, 1]\n",
            "[[4, 3, 2, 1], [3, 2]]\n",
        )
    );
}

#[test]
fn a_bigdecimal_converts_to_a_correctly_rounded_double() {
    // `BigDecimal::to_f64` scaled by a power of ten in f64, so its error grew
    // with the exponent and it overflowed early: `1.0e300 as double` printed
    // `1.0000000000000006E300` and `Double.MAX_VALUE` — exactly representable —
    // printed `Infinity`. Java's `doubleValue()` is correctly rounded. Frozen
    // from Apache Groovy 5.0.8 on **JVM 21**: JVM 17 still renders doubles by
    // the pre-JDK-19 algorithm and would answer `9.999999999999999E22` to the
    // last line.
    let (out, ok) = run(concat!(
        "println((1.0e308) as double)\n",
        "println((1.7976931348623157e308) as double)\n",
        "println((1.0e300) as double)\n",
        "println((0.1) as double)\n",
        "println((123456789012345678901234567890) as double)\n",
        "println(1.0e308.toDouble())\n",
        "println(Double.MIN_VALUE)\n",
        "println(1.0e23d)\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "1.0E308\n",
            "1.7976931348623157E308\n",
            "1.0E300\n",
            "0.1\n",
            "1.2345678901234568E29\n",
            "1.0E308\n",
            "4.9E-324\n",
            "1.0E23\n",
        )
    );
}

#[test]
fn the_boxed_type_constants_are_javas() {
    // `Double.MIN_VALUE` read Rust's `f64::MIN_POSITIVE`, which is Java's
    // `MIN_NORMAL` — the smallest *normal*, not the smallest subnormal. The two
    // names look interchangeable, the table type-checked either way, and nothing
    // in the build could see the difference. `SIZE`/`BYTES`/`MAX_EXPONENT` were
    // absent. Frozen from Apache Groovy 5.0.8 / JVM 21.0.12.
    let (out, ok) = run(concat!(
        "println([Double.MIN_VALUE, Double.MIN_NORMAL, Double.MAX_VALUE])\n",
        "println([Double.MAX_EXPONENT, Double.MIN_EXPONENT])\n",
        "println([Integer.SIZE, Integer.BYTES, Long.SIZE, Long.BYTES])\n",
        "println([Short.SIZE, Short.BYTES, Byte.SIZE, Byte.BYTES])\n",
        "println([Double.SIZE, Double.BYTES])\n",
        "println([Integer.MAX_VALUE, Integer.MIN_VALUE, Long.MAX_VALUE, Long.MIN_VALUE])\n",
        "println([Double.MIN_VALUE.getClass().getName(), Double.SIZE.getClass().getName()])\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "[4.9E-324, 2.2250738585072014E-308, 1.7976931348623157E308]\n",
            "[1023, -1022]\n",
            "[32, 4, 64, 8]\n",
            "[16, 2, 8, 1]\n",
            "[64, 8]\n",
            "[2147483647, -2147483648, 9223372036854775807, -9223372036854775808]\n",
            "[java.lang.Double, java.lang.Integer]\n",
        )
    );
}

#[test]
fn the_jdk_lookalikes_answer_javas_rule_not_rusts() {
    // A sweep of every mapping where the Rust name resembles the Java one and
    // means something else. `Math.signum` is `f64::signum` in name only (Rust
    // answers ±1.0 for ±0.0, Java returns the zero unchanged); `Math.round` was
    // the pre-Java-7 `floor(a + 0.5)`, which answers 1 for
    // `0.49999999999999994`; `Math.max`/`min` were `fmax`/`fmin`, which ignore a
    // NaN operand; `sort` compared with `partial_cmp`, which reports NaN as
    // `Equal` and `-0.0` as equal to `+0.0`; `String.length`/`indexOf`/
    // `substring`/`charAt` counted code points where Java counts UTF-16 units;
    // `String.trim` used Rust's Unicode-whitespace trim rather than Java's
    // `<= U+0020`; `String.compareTo` answered a normalised sign rather than the
    // code-unit difference; `Double.intValue` never narrowed to 32 bits;
    // `Integer.parseInt` neither range-checked nor rejected surrounding space;
    // `new BigDecimal(double)` and `as BigDecimal` were the wrong way round.
    //
    // `substring`'s bounds message was the JDK *17* wording (`begin 0, end 9,
    // length 2`); JDK 19 rewrote it to `Range [0, 9) out of bounds for length 2`,
    // which the sibling subscript path already emitted. The harness's JVM gate
    // cannot see a version-stale string frozen inside the implementation.
    //
    // Every line below was captured from Apache Groovy 5.0.8 on JVM 21.0.12 by
    // running this exact source through it; the two outputs are byte-identical.
    let (out, ok) = run(concat!(
        "println([Math.signum(0.0d), Math.signum(-0.0d), Math.signum(-3.5d), Math.signum(Double.NaN)])\n",
        "println([Math.round(0.49999999999999994d), Math.round(2.5d), Math.round(-2.5d), Math.round(-1.5d)])\n",
        "println([Math.max(Double.NaN, 1.0d), Math.min(Double.NaN, 1.0d)])\n",
        "println([Math.max(-0.0d, 0.0d), Math.min(-0.0d, 0.0d)])\n",
        "println([Math.floorDiv(-7, 2), Math.floorMod(-7, 2), Math.floorDiv(7, -2), Math.floorMod(7, -2)])\n",
        "println([Double.compare(Double.NaN, 1.0d), Double.compare(-0.0d, 0.0d), Integer.compare(1, 2)])\n",
        "println([Integer.bitCount(255), Integer.highestOneBit(100), Integer.reverse(1), Long.reverse(1L)])\n",
        "println([(1e10d).intValue(), (-1e10d).intValue(), (1.0d/0).intValue(), (1e10d).longValue()])\n",
        "println([1.compareTo(2.5), 1.compareTo(0.5), 1.equals(true), 1.equals(1)])\n",
        "println([1.5d.compareTo(2.0d), 1.5G.compareTo(2.0G), 1.0G.compareTo(1.00G)])\n",
        "println([(1e30G).intValue(), (1e30G).longValue()])\n",
        "println([[1.0d, Double.NaN, 0.5d].sort(), [Double.NaN, 1.0d].max(), [Double.NaN, 1.0d].min()])\n",
        "println(\"a\\u0041b\" + \"|\" + 'a\\u0041b' + \"|\" + \"\"\"a\\u0041b\"\"\" + \"|\" + /a\\u0041b/)\n",
        "println([\"\\uD83D\\uDE00\".length(), \"\\101\".length(), \"\\101\", \"a\\bb\".length()])\n",
        "println([\"a😀b\".length(), \"a😀b\".indexOf(\"b\"), \"a😀b\".size()])\n",
        "println(\"a😀b\".substring(1, 3) == \"😀\")\n",
        "println([\"abc\".indexOf(\"b\", 2), \"abcb\".indexOf(\"b\", 2), \"abc\".indexOf(97), \"abcb\".lastIndexOf(\"b\", 2)])\n",
        "println([\"a\".compareTo(\"c\"), \"abc\".compareTo(\"abd\"), \"abc\".compareTo(\"abc\")])\n",
        "println([\"\\u00A0x\".trim() + \"|\", \"\\u0000x\".trim() + \"|\", \"\\u00A0x\".strip() + \"|\", \"  \".isBlank()])\n",
        "println([\"a\\u00A0b\".tokenize(), \"a b\\tc\".tokenize()])\n",
        "println([\"ßx\".capitalize(), \"abc\".capitalize(), \"ABC\".uncapitalize()])\n",
        "println([\"inf\".isDouble(), \"NaN\".isDouble(), \"1.5\".isDouble()])\n",
        "try { Integer.parseInt(\"3000000000\") } catch (e) { println(e.getClass().getName() + \": \" + e.getMessage()) }\n",
        "try { Integer.parseInt(\" 5 \") } catch (e) { println(e.getClass().getName() + \": \" + e.getMessage()) }\n",
        "println([Long.parseLong(\"3000000000\"), Integer.parseInt(\"ff\", 16)])\n",
        "try { \"abc\".substring(-1) } catch (e) { println(e.getClass().getName() + \": \" + e.getMessage()) }\n",
        "try { \"ab\".substring(0, 9) } catch (e) { println(e.getClass().getName() + \": \" + e.getMessage()) }\n",
        "try { \"abc\".substring(2, 1) } catch (e) { println(e.getClass().getName() + \": \" + e.getMessage()) }\n",
        "println([new BigDecimal(0.555d).toString(), (0.555d as BigDecimal).toString()])\n",
        "println([BigDecimal.ZERO, BigDecimal.TEN, BigInteger.TWO, BigInteger.TEN])\n",
        "println([Math.ulp(1.0d), Math.copySign(3.0d, -1.0d), Math.nextUp(1.0d), Math.getExponent(1.0d)])\n",
        "println([Math.toIntExact(5L), Math.addExact(1, 2)])\n",
        "try { Math.addExact(Integer.MAX_VALUE, 1) } catch (e) { println(e.getClass().getName() + \": \" + e.getMessage()) }\n",
    ));
    assert!(ok);
    assert_eq!(
        out,
        concat!(
            "[0.0, -0.0, -1.0, NaN]\n",
            "[0, 3, -2, -1]\n",
            "[NaN, NaN]\n",
            "[0.0, -0.0]\n",
            "[-4, 1, -4, -1]\n",
            "[1, -1, -1]\n",
            "[8, 64, -2147483648, -9223372036854775808]\n",
            "[2147483647, -2147483648, 2147483647, 10000000000]\n",
            "[-1, 1, false, true]\n",
            "[-1, -1, 0]\n",
            "[1073741824, 5076944270305263616]\n",
            "[[0.5, 1.0, NaN], NaN, NaN]\n",
            "aAb|aAb|aAb|aAb\n",
            "[2, 1, A, 3]\n",
            "[4, 3, 4]\n",
            "true\n",
            "[-1, 3, 0, 1]\n",
            "[-2, -1, 0]\n",
            "[ x|, x|,  x|, true]\n",
            "[[a b], [a, b, c]]\n",
            "[ßx, Abc, aBC]\n",
            "[false, true, true]\n",
            "java.lang.NumberFormatException: For input string: \"3000000000\"\n",
            "java.lang.NumberFormatException: For input string: \" 5 \"\n",
            "[3000000000, 255]\n",
            "java.lang.StringIndexOutOfBoundsException: Range [-1, 3) out of bounds for length 3\n",
            "java.lang.StringIndexOutOfBoundsException: Range [0, 9) out of bounds for length 2\n",
            "java.lang.StringIndexOutOfBoundsException: Range [2, 1) out of bounds for length 3\n",
            "[0.55500000000000004884981308350688777863979339599609375, 0.555]\n",
            "[0, 10, 2, 10]\n",
            "[2.220446049250313E-16, -3.0, 1.0000000000000002, 0]\n",
            "[5, 3]\n",
            "java.lang.ArithmeticException: integer overflow\n",
        )
    );
}

// ── Runaway recursion is a catchable StackOverflowError ─────────────────────
//
// Groovy's depth is the JVM's, and running out of it throws a
// `java.lang.StackOverflowError` a script can catch. groovyrs has two
// recursions that are not fusevm's `Op::Call` frame vector — `host::run_sub`'s
// nested `VM::run` (the Rust stack) and the parser/compiler's tree walk — and
// before `host::MAX_CALL_DEPTH` both ended in `fatal runtime error: stack
// overflow`, an abort with no diagnostic that no `catch` could see. The native
// `Op::Call` path did not abort at all: it grew `vm.frames` until the process
// was killed. Every expectation below is byte-verified against Apache Groovy
// 5.0.8 on JVM 21.0.12.

#[test]
fn runaway_function_recursion_raises_a_catchable_stack_overflow_error() {
    // The native `Op::Call` path — the one that used to grow memory forever
    // rather than raise. Both the direct and the mutual cycle are guarded,
    // because `compiler::recursive_fns` closes the call graph transitively.
    let (out, ok) = run(
        "def r(n) { return r(n + 1) }\n\
         try { println r(0) } catch (StackOverflowError e) { println \"caught \" + e.getClass().getName() + \" msg=\" + e.getMessage() }\n\
         def a(n) { return b(n + 1) }\n\
         def b(n) { return a(n + 1) }\n\
         try { println a(0) } catch (Throwable t) { println \"mutual \" + t.getClass().getName() }\n",
    );
    assert!(ok);
    assert_eq!(
        out,
        "caught java.lang.StackOverflowError msg=null\n\
         mutual java.lang.StackOverflowError\n"
    );
}

#[test]
fn runaway_closure_recursion_raises_a_stack_overflow_error_in_the_right_place() {
    // The host re-entry path, and the hierarchy that decides what catches it:
    // `StackOverflowError` is a `VirtualMachineError`, so it is an `Error` and
    // is *not* an `Exception`.
    let (out, ok) = run(
        "def q\n\
         q = { n -> q(n + 1) }\n\
         try { println q(0) } catch (Throwable t) { println \"closure \" + t.getClass().getName() + \" isErr=\" + (t instanceof Error) + \" isVME=\" + (t instanceof VirtualMachineError) + \" isExc=\" + (t instanceof Exception) }\n",
    );
    assert!(ok);
    assert_eq!(
        out,
        "closure java.lang.StackOverflowError isErr=true isVME=true isExc=false\n"
    );
}

#[test]
fn runaway_method_recursion_raises_a_stack_overflow_error() {
    let (out, ok) = run(
        "class R { def go(n) { return go(n + 1) } }\n\
         try { println new R().go(0) } catch (Throwable t) { println \"method \" + t.getClass().getName() }\n",
    );
    assert!(ok);
    assert_eq!(out, "method java.lang.StackOverflowError\n");
}

#[test]
fn a_stack_overflow_error_escapes_catch_exception() {
    // The whole point of putting `StackOverflowError` under `VirtualMachineError`
    // rather than raising some convenient `RuntimeException`: a program that
    // catches `Exception` broadly must not swallow it.
    let (out, ok) = run(
        "def r(n) { return r(n + 1) }\n\
         try { try { r(0) } catch (Exception e) { println \"WRONG: an Error was caught as an Exception\" } }\n\
         catch (Throwable t) { println \"escaped \" + t.getClass().getName() }\n",
    );
    assert!(ok);
    assert_eq!(out, "escaped java.lang.StackOverflowError\n");
}

#[test]
fn recursion_below_the_depth_limit_still_completes() {
    // The guard must not turn ordinary recursion into an error. 900 deep is
    // past what a naive Rust-stack recursion survived (63) and inside both
    // `MAX_CALL_DEPTH` and the JVM's own measured 1650.
    let (out, ok) = run("def f(n) { if (n <= 0) return 0; return 1 + f(n - 1) }\n\
         println f(900)\n\
         def fib(n) { n < 2 ? n : fib(n - 1) + fib(n - 2) }\n\
         println fib(20)\n");
    assert!(ok);
    assert_eq!(out, "900\n6765\n");
}

#[test]
fn source_nested_past_the_limit_is_a_compile_error_not_an_abort() {
    // The third recursion: source nesting is Rust stack in the parser, in the
    // compiler's walk, and again when the tree drops. Apache Groovy refuses
    // these too (`CompilationFailedException: parsing failed` at 1000 nested
    // parentheses and at a 2000-term `+` chain, measured on 5.0.8 / JVM
    // 21.0.12); what is pinned here is that groovyrs refuses them the same way
    // — a message and exit 1 — rather than with `fatal runtime error: stack
    // overflow`, which is what it did before `parser::MAX_NESTING`.
    let deep_parens = format!("println {}1{}\n", "(".repeat(20_000), ")".repeat(20_000));
    let (out, err, ok) = run_full(&deep_parens);
    assert!(!ok, "expected a compile error, got stdout={out:?}");
    assert!(
        err.contains("expression nesting is deeper than"),
        "expected the nesting diagnostic, got stderr={err:?}"
    );
    assert!(
        !err.contains("stack overflow"),
        "the process aborted instead of reporting: {err:?}"
    );

    // A left-folded operator chain is the same depth by another spelling, and
    // it is the shape the recursive-descent counter alone does not see.
    let long_chain = format!("println 1{}\n", " + 1".repeat(50_000));
    let (_, err, ok) = run_full(&long_chain);
    assert!(!ok);
    assert!(
        err.contains("expression nesting is deeper than") && !err.contains("stack overflow"),
        "stderr={err:?}"
    );

    // …and just under the limit still compiles and runs, so the guard is a
    // bound and not a ban.
    let (out, ok) = run(&format!(
        "println {}1{}\n",
        "(".repeat(4900),
        ")".repeat(4900)
    ));
    assert!(ok);
    assert_eq!(out, "1\n");
}

// ── Throwable shape: cause, suppressed, and the Groovy payloads ─────────────

#[test]
fn a_throwable_carries_its_cause() {
    // The JDK's four constructors, `getCause`, `initCause`, and the `cause`
    // property Groovy reads through `getCause()`. `T(Throwable)` takes its
    // message from the cause's `toString()`, which is why `wrapped.getMessage()`
    // below is the qualified name and the inner message.
    let (out, ok) = run("println new RuntimeException(\"plain\").getCause()\n\
         def chained = new RuntimeException(\"outer\", new java.io.IOException(\"inner\"))\n\
         println chained.getMessage()\n\
         println chained.getCause().getClass().getName()\n\
         println chained.getCause().getMessage()\n\
         println chained.cause.message\n\
         def wrapped = new RuntimeException(new java.io.IOException(\"only\"))\n\
         println wrapped.getMessage()\n\
         println wrapped.getCause().getClass().getName()\n\
         def ic = new RuntimeException(\"a\")\n\
         ic.initCause(new java.io.IOException(\"b\"))\n\
         println ic.getCause().getClass().getName()\n");
    assert!(ok);
    assert_eq!(
        out,
        "null\n\
         outer\n\
         java.io.IOException\n\
         inner\n\
         inner\n\
         java.io.IOException: only\n\
         java.io.IOException\n\
         java.io.IOException\n"
    );
}

#[test]
fn a_throwable_carries_its_suppressed_list() {
    // `getSuppressed()` is empty rather than absent on an ordinary throwable —
    // a script printing it must see `[]`, not `null`.
    let (out, ok) = run("def e = new Exception(\"x\")\n\
         println e.getSuppressed().size()\n\
         println e.getSuppressed()\n\
         e.addSuppressed(new java.io.IOException(\"s1\"))\n\
         e.addSuppressed(new IllegalStateException(\"s2\"))\n\
         println e.getSuppressed().size()\n\
         println e.getSuppressed()[0].getClass().getName()\n\
         println e.getSuppressed()[1].getMessage()\n\
         println e.suppressed.size()\n");
    assert!(ok);
    assert_eq!(out, "0\n[]\n2\njava.io.IOException\ns2\n2\n");
}

#[test]
fn missing_method_and_missing_property_carry_groovys_payload() {
    // Groovy's dynamic dispatch makes these two throwables ordinary control
    // flow, and a handler recovers from one by reading what missed. Answering
    // the right *message* under a throwable with no `getMethod()` is a
    // divergence a message audit cannot see.
    let (out, ok) = run(
        "try { 5.zork(1, \"a\") } catch (Throwable e) {\n\
        \x20 println e.getMethod()\n\
        \x20 println e.getType().getName()\n\
        \x20 println e.getArguments()\n\
        \x20 println e.method\n\
        \x20 println e.type\n\
         }\n\
         try { [1, 2].nope() } catch (Throwable e) { println e.getType().getName() + \" \" + e.getMethod() + \" \" + e.getArguments().size() }\n\
         try { println 5.zork } catch (Throwable e) {\n\
        \x20 println e.getProperty()\n\
        \x20 println e.getType().getName()\n\
        \x20 println e.property\n\
         }\n",
    );
    assert!(ok);
    assert_eq!(
        out,
        "zork\n\
         java.lang.Integer\n\
         [1, a]\n\
         zork\n\
         class java.lang.Integer\n\
         java.util.ArrayList nope 0\n\
         zork\n\
         java.lang.Integer\n\
         zork\n"
    );
}

#[test]
fn a_bare_name_miss_names_the_script_class_in_its_payload() {
    // `getType()` for a bare-name miss is the *script* class, so the answer
    // depends on the file's stem — the same rule
    // `the_script_class_name_follows_the_entry_point` pins for the message.
    let dir = std::env::temp_dir();
    let path = dir.join("GroovyrsMissingPropertyType.groovy");
    std::fs::write(
        &path,
        "try { println zork } catch (Throwable e) { println e.getProperty() + \" / \" + e.getType().getName() }\n",
    )
    .unwrap();
    let got = Command::new(env!("CARGO_BIN_EXE_groovy"))
        .arg(&path)
        .output()
        .expect("spawn groovy");
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        String::from_utf8_lossy(&got.stdout),
        "zork / GroovyrsMissingPropertyType\n"
    );
}

#[test]
fn a_type_may_be_named_fully_qualified() {
    // `catch (groovy.lang.MissingMethodException e)` is the spelling the Groovy
    // docs use for its own exceptions and the only one available when a script
    // has shadowed the simple name. It was a parse error here, which made every
    // `groovy.lang.*` handler unreachable. `new`, `instanceof` and a multi-catch
    // arm read the same grammar.
    let (out, ok) = run(
        "try { 5.zork() } catch (groovy.lang.MissingMethodException e) { println \"MME\" }\n\
         try { println 5.zork } catch (groovy.lang.MissingPropertyException e) { println \"MPE\" }\n\
         try { 5.zork() } catch (groovy.lang.GroovyRuntimeException e) { println \"GRE\" }\n\
         try { throw new java.io.IOException(\"q\") } catch (java.io.IOException e) { println \"IOE \" + e.getMessage() }\n\
         try { 5.zork() } catch (java.io.IOException | groovy.lang.MissingMethodException e) { println \"arm \" + e.getClass().getName() }\n\
         try { throw new java.io.FileNotFoundException(\"f\") } catch (Throwable t) {\n\
        \x20 println (t instanceof java.io.IOException)\n\
        \x20 println (t instanceof java.lang.Exception)\n\
        \x20 println (t instanceof java.lang.Error)\n\
         }\n",
    );
    assert!(ok);
    assert_eq!(
        out,
        "MME\n\
         MPE\n\
         GRE\n\
         IOE q\n\
         arm groovy.lang.MissingMethodException\n\
         true\n\
         true\n\
         false\n"
    );
}

#[test]
fn a_qualified_name_from_the_wrong_package_does_not_match() {
    // The package half of a qualified name is load-bearing: resolving it by
    // simple name alone would make `catch (com.example.IOException e)` catch a
    // `java.io.IOException`. (Apache Groovy rejects the unresolvable name at
    // compile time; groovyrs accepts it and never matches, which is the same
    // observable for the `catch` and is recorded in BUGS.md.)
    let (out, ok) = run(
        "try { throw new java.io.IOException(\"z\") } catch (Throwable t) {\n\
        \x20 println (t instanceof com.example.IOException)\n\
        \x20 println (t instanceof java.io.IOException)\n\
         }\n",
    );
    assert!(ok);
    assert_eq!(out, "false\ntrue\n");
}

#[test]
fn a_continue_in_a_c_style_for_runs_the_update_clause() {
    // The `continue` in a three-clause `for` targets the **step label** emitted
    // after the body, so the update clause still runs and the loop terminates.
    // Targeting the loop top instead — the `while` rule — skips `i++` and spins
    // forever on the first index the guard accepts; that lowering bug shipped in
    // this frontend's template lineage, and the corpus's `%%`-separated probes
    // cannot catch a hang, only a wrong answer.
    //
    // Each case below would not terminate under it: the first `continue`s on an
    // even index, the second on the very first, the third `continue`s out of an
    // inner three-clause loop.
    let src = r#"
def a = ''
for (int i = 0; i < 6; i++) { if (i % 2 == 0) continue; a += i }
println a
def b = ''
for (int i = 0; i < 4; i++) { if (i == 0) continue; b += i }
println b
def c = ''
for (int i = 0; i < 3; i++) { for (int j = 0; j < 3; j++) { if (j == 1) continue; c += "$i$j," } }
println c
def d = ''
outer: for (int i = 0; i < 3; i++) { for (int j = 0; j < 3; j++) { if (j == 1) continue outer; d += "$i$j," } }
println d
def e = ''
for (int i = 0; i < 4; i++) { try { if (i == 2) continue; e += i } finally { e += 'f' } }
println e
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "135\n123\n00,02,10,12,20,22,\n00,10,20,\n0f1ff3f\n");
}

#[test]
fn grep_filters_by_the_filters_is_case_not_by_equality() {
    // `grep(filter)` is specified as `filter.isCase(element)`, so the filter's
    // *type* picks the test: a closure calls, a `Class` is `isInstance`, a
    // `Pattern` matches the whole string, and a collection or range is
    // membership. `grep()` with no filter is `Closure.IDENTITY` — the
    // Groovy-true elements. A collection filter is why `[[1,2],[3]].grep([1,2])`
    // keeps nothing: `[1,2]` does not *contain* `[1,2]`.
    let src = r#"
println([1,2,3,4].grep { it > 2 })
println([1,2,3,4].grep(2))
println([1,'a',2].grep(Integer))
println(['ab','cd','ax'].grep(~/a./))
println([1,2,3,4,5].grep(2..3))
println([[1,2],[3]].grep([1,2]))
println([1,'a',null,0].grep())
println([null, 1, null].grep(null))
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[3, 4]\n[2]\n[1, 2]\n[ab, ax]\n[2, 3]\n[]\n[1, a]\n[null, null]\n"
    );
}

#[test]
fn grep_takes_the_receivers_shape_from_the_receiver() {
    // A map has no `Map` overload of `grep`, so it reaches the `Object` one and
    // answers a **list of entries** — not the map `findAll` answers. A String
    // greps its characters, a `Set` keeps its type, and a receiver that is not a
    // collection at all iterates as a single element.
    let src = r#"
println([a:1, b:0].grep { it.value })
println([a:1, b:0].findAll { it.value })
println("abcd".grep { it > 'b' })
println(([1,2,3] as Set).grep { it > 1 })
println((1..5).grep { it % 2 == 1 })
println(5.grep { it > 1 })
println(5.grep { it > 9 })
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "[a=1]\n[a:1]\n[c, d]\n[2, 3]\n[1, 3, 5]\n[5]\n[]\n");
}

#[test]
fn inspect_is_the_verbose_rendering_and_a_range_overrides_it() {
    // `inspect()` quotes Strings and recurses into collections, where
    // `toString()` does not. A `Range` declares its own and answers its bounds
    // rather than the elements it enumerates — with the bounds themselves
    // rendered verbosely, so a String-bounded range quotes them.
    let src = r#"
println([1, 'a', [1,2], null, 1.5].inspect())
println([1, 'a'].toString())
println(['a':1, 'b':[1,'x']].inspect())
println(([1,'a'] as Set).inspect())
println("hi".inspect() + '|' + 5.inspect() + '|' + null.inspect())
println([(1..5).inspect(), (1..<5).inspect(), ('a'..'c').inspect(), ('a'..<'e').inspect()])
println([[1,'a'].toListString(), [a:1,b:'x'].toMapString()])
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[1, 'a', [1, 2], null, 1.5]\n\
         [1, a]\n\
         ['a':1, 'b':[1, 'x']]\n\
         [1, 'a']\n\
         'hi'|5|null\n\
         [1..5, 1..<5, 'a'..'c', 'a'..'d']\n\
         [[1, a], [a:1, b:x]]\n"
    );
}

#[test]
fn put_at_is_the_subscript_assignment_and_answers_null() {
    // `putAt` is `[i] =` spelled out — same negative-index and grow-past-the-end
    // rules — and is `void`, where `List.set` and `Map.put` answer what they
    // displaced. The list form writes through the handle, so a second name sees
    // it.
    let src = r#"
def l = [1,2,3]
println([l.putAt(1, 'x'), l])
def alias = l
alias.putAt(0, 'y')
println(l)
l.putAt(-1, 'z')
println(l)
def g = [1,2]
g.putAt(4, 'q')
println(g)
def m = [a:1]
println([m.putAt('b', 2), m])
println([m.put('a', 9), m])
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[null, [1, x, 3]]\n\
         [y, x, 3]\n\
         [y, x, z]\n\
         [1, 2, null, null, q]\n\
         [null, [a:1, b:2]]\n\
         [1, [a:9, b:2]]\n"
    );
}

#[test]
fn the_bit_and_shift_operators_answer_under_their_method_names_too() {
    // `5.and(3)` is `5 & 3` and `5.leftShift(2)` is `5 << 2`. The shifts fill to
    // the receiver's Java type, which the value does not carry — the compiler
    // marks the width on the call, so `(-1).rightShiftUnsigned(28)` is the
    // 32-bit `15` while `(-1L)`'s of 60 is the 64-bit one.
    let src = r#"
println([5.and(3), 5.or(3), 5.xor(3), 5.bitwiseNegate()])
println([5.leftShift(2), 5.rightShift(1), 5.rightShiftUnsigned(1)])
println([(-8).rightShiftUnsigned(1), (-1).rightShiftUnsigned(28), (-1L).rightShiftUnsigned(60)])
println(1L.leftShift(40))
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[1, 7, 6, -6]\n[20, 2, 2]\n[2147483644, 15, 15]\n1099511627776\n"
    );
}

#[test]
fn big_integer_bit_and_shift_operators_run_at_arbitrary_precision() {
    // fusevm's native `Op::BitAnd` reads its operands with `Value::to_int`,
    // which answers `0` for the `Value::Obj` a `BigInteger` rides — so every one
    // of these evaluated to `0`, silently and at every magnitude. They route to
    // the host builtin instead, which is two's-complement and unbounded:
    // `(-1G) & 255G` is `255` and `1G << 100` keeps all 31 digits.
    let src = r#"
println([1G & 3G, 7G | 8G, 7G ^ 3G, ~7G])
println([1G & 3, 1 & 3G, 255G & 15G, (-1G) & 255G])
def a = 1G
println(a & 3G)
println([4G >> 1, (-8G) >> 1, 4G << 1])
println(12345678901234567890G & 255G)
println(12345678901234567890G << 2)
println(1G << 100)
println([7G.and(3G), 7G.or(8G), 7G.xor(3G), 7G.bitwiseNegate(), 1G.shiftLeft(3)])
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[1, 15, 4, -8]\n\
         [1, 1, 15, 255]\n\
         1\n\
         [2, -4, 8]\n\
         210\n\
         49382715604938271560\n\
         1267650600228229401496703205376\n\
         [3, 15, 4, -8, 8]\n"
    );
}

#[test]
fn a_big_decimal_has_no_bits_and_groovy_declines_rather_than_truncating() {
    // The mask operators are defined on the integral types only. A `BigDecimal`
    // operand is an `UnsupportedOperationException` naming the *left* operand,
    // and `>>>` has no fill width for an unbounded `BigInteger` at all.
    let (out, ok) = run(
        "try { println(1.5G & 1G) } catch (Throwable t) { println([t.getClass().getName(), t.getMessage()]) }\n\
         try { println(4G >>> 1) } catch (Throwable t) { println([t.getClass().getName(), t.getMessage()]) }\n",
    );
    assert!(ok);
    assert_eq!(
        out,
        "[java.lang.UnsupportedOperationException, Cannot use and() on this number type: java.math.BigDecimal with value: 1.5]\n\
         [java.lang.UnsupportedOperationException, Cannot use rightShiftUnsigned() on this number type: java.math.BigInteger with value: 4]\n"
    );
}

#[test]
fn the_java_named_arithmetic_methods_are_not_the_groovy_operators() {
    // `7G.divide(3G)` is `BigInteger.divide` — truncating — where `7G / 3G`
    // promotes to `2.3333333333`, and `1.0G.divide(3.0G)` demands an exact
    // quotient where the operator approximates to ten digits. `mod` is
    // `BigInteger`'s alone and is never negative; `remainder` takes the
    // dividend's sign.
    let src = r#"
println([7G.divide(3G), 7G / 3G])
println([(-7G).divide(3G), 7G.mod(3G), (-7G).mod(3G), 7G.remainder(3G), (-7G).remainder(3G)])
try { println(1.0G.divide(3.0G)) } catch (Throwable t) { println([t.getClass().getName(), t.getMessage()]) }
println(1.0G / 3.0G)
println([1.5G.divide(3G), 7.5G.divide(2G), 7.5G.remainder(2G)])
println([7.5G.add(1G), 7.5G.subtract(1G), 7.5G.multiply(2G), 7.5G.pow(2)])
println([7G.add(1G), 7G.pow(2), 7G.add(1G).getClass().getName()])
try { println(1G.divide(0G)) } catch (Throwable t) { println([t.getClass().getName(), t.getMessage()]) }
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[2, 2.3333333333]\n\
         [-2, 1, 2, 1, -1]\n\
         [java.lang.ArithmeticException, Non-terminating decimal expansion; no exact representable decimal result.]\n\
         0.3333333333\n\
         [0.5, 3.75, 1.5]\n\
         [8.5, 6.5, 15.0, 56.25]\n\
         [8, 49, java.math.BigInteger]\n\
         [java.lang.ArithmeticException, BigInteger divide by zero]\n"
    );
}

#[test]
fn matcher_group_by_name_is_a_different_overload_from_group_by_index() {
    // `Matcher.group(String)` reads the group `(?<name>…)` declared. Falling
    // through to the index arm read the argument with `as_i64`, which answers
    // `None` for text and defaulted to `0` — so every named read silently
    // returned the whole match.
    let src = r#"
def m = ("a1b2" =~ /(?<L>[a-z])(?<D>\d)/)
m.find()
println([m.group('L'), m.group('D'), m.group(0), m.group(1)])
def n = ("a1" =~ /(?<L>[a-z])/)
n.find()
try { n.group('Z') } catch (Throwable t) { println([t.getClass().getName(), t.getMessage()]) }
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[a, 1, a1, a]\n[java.lang.IllegalArgumentException, No group with name <Z>]\n"
    );
}

#[test]
fn the_format_grouping_flag_separates_thousands() {
    // `%,d` / `%,f` are `java.util.Formatter`'s locale grouping, which under the
    // en-US locale groovyrs models is a comma every three digits of the integer
    // part only — the sign, the fraction and the exponent are untouched.
    let src = r#"
println(String.format("%,d", 1234567))
println(String.format("%,d", -1234567))
println(String.format("%,d", 123))
println(String.format("%d", 1234567))
println(String.format("%,.2f", 1234.5))
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "1,234,567\n-1,234,567\n123\n1234567\n1,234.50\n");
}

#[test]
fn take_while_and_drop_while_answer_a_string_for_a_string_receiver() {
    // Every other closure-driven String method answers the character list, and
    // these two were handed that raw vector — `"abcdef".takeWhile { it < 'd' }`
    // printed `[a, b, c]` where `StringGroovyMethods` answers `abc`.
    let src = r#"
println("abcdef".takeWhile { it < 'd' })
println("abcdef".dropWhile { it < 'd' })
println("abcdef".findAll { it < 'd' })
println([1,2,3,4].takeWhile { it < 3 })
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "abc\ndef\n[a, b, c]\n[1, 2]\n");
}

#[test]
fn sum_with_a_collection_seed_concatenates_through_the_handle() {
    // `sum(seed)` folds with `plus`, and a list `plus` a list concatenates. A
    // list reaches the fold as a *handle*, which the concatenation arm did not
    // read — so `[[1,2],[3]].sum([])` fell through to the string fallback and
    // rendered `[][1, 2][3]`.
    let src = r#"
println([[1,2],[3]].sum([]))
println([[1,2],[3]].sum())
println([1,2].sum(10))
println(['a','b'].sum(''))
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "[1, 2, 3]\n[1, 2, 3]\n13\nab\n");
}

#[test]
fn character_collections_and_system_statics_answer() {
    // The `int` overloads of the `Character` predicates take a code point, so
    // `Character.isDigit(53)` asks about `'5'`. `Collections`' mutators write
    // through the handle and answer `void`.
    let src = r#"
println([Character.isDigit('5' as char), Character.isLetterOrDigit('_' as char), Character.isDigit(53)])
println([Character.toUpperCase('a' as char), Character.getNumericValue('7' as char), Character.getNumericValue('!' as char)])
println([Character.MIN_RADIX, Character.MAX_RADIX])
println([Collections.emptyList(), Collections.emptyMap(), Collections.singletonList(1), Collections.nCopies(3, 'x')])
def l = [3,1,2]
println([Collections.sort(l), l])
Collections.reverse(l)
println(l)
println([Collections.max([1,5,2]), Collections.min([1,5,2]), Collections.frequency([1,1,2], 1), Collections.disjoint([1],[2])])
println(Arrays.asList(1,2,3))
println([System.lineSeparator() == "\n", System.getProperty("file.separator"), System.getProperty("nope.nope"), System.getProperty("nope.nope", "d")])
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[true, false, true]\n\
         [A, 7, -1]\n\
         [2, 36]\n\
         [[], [:], [1], [x, x, x]]\n\
         [null, [1, 2, 3]]\n\
         [3, 2, 1]\n\
         [5, 1, 2, true]\n\
         [1, 2, 3]\n\
         [true, /, null, d]\n"
    );
}

#[test]
fn iterator_and_list_iterator_are_two_different_inner_classes() {
    // `getClass()` tells `ArrayList$Itr` from `ArrayList$ListItr`; the two used
    // to share a name because they share an implementation here.
    let (out, ok) = run(
        "println([[1,2,3].iterator().getClass().getName(), [1,2,3].listIterator().getClass().getName()])",
    );
    assert!(ok);
    assert_eq!(
        out,
        "[java.util.ArrayList$Itr, java.util.ArrayList$ListItr]\n"
    );
}

#[test]
fn map_collect_many_spreads_the_entry_but_grep_does_not() {
    // `collectMany` has a `Map` overload and goes through
    // `callClosureForMapEntry`, so its closure takes `(key, value)`. `grep` has
    // none, reaches the `Object` overload, and hands the whole `Map.Entry` over.
    let src = r#"
println([a:1, b:2].collectMany { k, v -> [k, v] })
println([[1,2],[3,4]].collectMany { it })
println([a:1, b:0].grep { it.value })
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(out, "[a, 1, b, 2]\n[1, 2, 3, 4]\n[a=1]\n");
}

#[test]
fn string_read_lines_splits_on_the_line_terminators() {
    let (out, ok) = run(
        "println([\"a\\nb\\nc\".readLines(), \"\".readLines(), \"a\\n\".readLines(), \"a\\r\\nb\".readLines()])",
    );
    assert!(ok);
    assert_eq!(out, "[[a, b, c], [], [a], [a, b]]\n");
}

#[test]
fn a_tree_map_presents_its_entries_in_key_order_through_every_read() {
    // `HeapObj::OrderedMap` carries a `MapKind`, and `as_omap` applies it — so
    // one accessor puts a `TreeMap` in key order everywhere at once. This pins
    // the *read paths*, which is where the old gap was a silent wrong answer:
    // the entries were all present, only in the wrong order, so nothing threw.
    //
    // Keys are added in three different ways (constructor, property write,
    // `putAll`) because storage stays insertion-ordered and presentation is
    // derived; a fix that sorted only at construction would pass line 1 alone.
    let src = r#"
def m = new TreeMap([b:2, a:1, c:3])
println m
m.e = 5
m.putAll([d:4])
println m
println([m.keySet(), m.values(), m.entrySet()])
def order = ''
m.each { k, v -> order += k }
for (entry in m) { order += entry.key }
println order
println([m.collect { k, v -> k }, m.inject('') { acc, e -> acc + e.key }])
println([m.firstKey(), m.lastKey(), m.iterator().next()])
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[a:1, b:2, c:3]\n\
         [a:1, b:2, c:3, d:4, e:5]\n\
         [[a, b, c, d, e], [1, 2, 3, 4, 5], [a=1, b=2, c=3, d=4, e=5]]\n\
         abcdeabcde\n\
         [[a, b, c, d, e], abcde]\n\
         [a, e, a=1]\n"
    );
}

#[test]
fn a_tree_map_stays_sorted_across_many_writes_and_through_a_second_name() {
    // A probe corpus only ever exercises a handful of keys in one statement.
    // This drives enough writes that a fix which sorted the *storage* on each
    // one — rather than deriving presentation — would drift, and it reads the
    // result through an alias to prove the kind rides the handle rather than
    // the variable.
    let src = r#"
def m = new TreeMap()
(1..200).each { m["k${(217 * it) % 200}"] = it }
def alias = m
def keys = alias.keySet()
println([keys.size(), keys == keys.toSorted(), alias.firstKey(), alias.lastKey()])
println([alias.getClass().getName(), alias.is(m)])
alias.a = 0
println([m.firstKey(), m.size()])
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[200, true, k0, k99]\n\
         [java.util.TreeMap, true]\n\
         [a, 201]\n"
    );
}

#[test]
fn map_equality_ignores_both_order_and_implementation() {
    // `Map.equals` is by entry set. groovyrs compared the *rendered* forms, so
    // `[b:2, a:1] == [a:1, b:2]` was `false` — a silent wrong answer on plain
    // map literals, independent of any TreeMap. Modeling map kinds made it
    // worse, since a sorted and an insertion-ordered map now render differently
    // by design, which is why this is pinned rather than left to the corpus.
    let src = r#"
println([[b:2, a:1] == [a:1, b:2], [b:2, a:1].equals([a:1, b:2])])
println([new TreeMap([b:2, a:1]) == [a:1, b:2], [a:1, b:2] == new TreeMap([b:2, a:1])])
println([[a:1] == [a:1, b:2], [a:1, b:2] == [a:1, b:3], [a:1] != [a:2]])
println([([a:1] as Set) == [a:1], [a:1] == [1], [a:1] == 'a'])
println([[a:[1, 2]] == [a:[1, 2]], [a:[b:1]] == [a:[b:1]]])
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[true, true]\n\
         [true, true]\n\
         [false, false, true]\n\
         [false, false, false]\n\
         [true, true]\n"
    );
}

#[test]
fn a_hash_map_iterates_in_bucket_order_with_the_maps_own_table_pre_size() {
    // `HashMap` lays its keys out in a power-of-two table, so its iteration
    // order is neither insertion nor sorted. The table size is the JDK's
    // `putMapEntries` pre-size, `Math.ceil(size / 0.75)` — deliberately *not*
    // `HashSet(Collection)`'s `max(size / 0.75 + 1, 16)`. The two differ for
    // five entries (an 8-slot table against a 16-slot one) and would otherwise
    // look interchangeable, so both construction paths are pinned here.
    let src = r#"
println(new HashMap([one:1, two:2, three:3, four:4, five:5]))
def built = new HashMap()
['one','two','three','four','five'].eachWithIndex { k, i -> built.put(k, i + 1) }
println built
println(new HashMap([z:1, y:2, x:3]))
println([new HashMap([:]), new HashMap([x:1])])
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[two:2, three:3, five:5, four:4, one:1]\n\
         [four:4, one:1, two:2, three:3, five:5]\n\
         [x:3, y:2, z:1]\n\
         [[:], [x:1]]\n"
    );
}

#[test]
fn a_derived_map_takes_its_class_from_which_gdk_method_built_it() {
    // Three different rules, which is why guessing one of them gets the others
    // wrong: `each`/`clone`/`plus` answer the receiver's exact implementation;
    // `findAll`/`collectEntries`/`minus`/`take` go through Groovy's
    // `createSimilarMap`, which preserves sortedness only (so a `HashMap`
    // receiver yields a `LinkedHashMap`); and `sort()` always builds a `TreeMap`
    // while `sort(closure)`/`toSorted()` never do.
    let src = r#"
def t = new TreeMap([b:2, a:1, c:3])
def h = new HashMap([b:2, a:1, c:3])
def cls = { it.getClass().getName() }
println([cls(t.clone()), cls(t.each { }), cls(t + [d:4]), cls(h.clone()), cls(h.each { })])
println([cls(t.findAll { k, v -> true }), cls(h.findAll { k, v -> true })])
println([cls(t.collectEntries { k, v -> [k, v] }), cls(h.collectEntries { k, v -> [k, v] })])
println([cls(t.minus([a:1])), cls(h.minus([a:1])), cls(t.take(2)), cls(h.take(2))])
println([cls([b:2, a:1].sort()), cls(h.sort()), cls([b:2, a:1].sort { it.value }), cls([b:2, a:1].toSorted())])
println([cls(t.groupBy { k, v -> 1 }), cls(t.countBy { k, v -> 1 })])
println([t + [d:4], t.collectEntries { k, v -> [k, v * 2] }])
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[java.util.TreeMap, java.util.TreeMap, java.util.TreeMap, java.util.HashMap, java.util.HashMap]\n\
         [java.util.TreeMap, java.util.LinkedHashMap]\n\
         [java.util.TreeMap, java.util.LinkedHashMap]\n\
         [java.util.TreeMap, java.util.LinkedHashMap, java.util.TreeMap, java.util.LinkedHashMap]\n\
         [java.util.TreeMap, java.util.TreeMap, java.util.LinkedHashMap, java.util.LinkedHashMap]\n\
         [java.util.LinkedHashMap, java.util.LinkedHashMap]\n\
         [[a:1, b:2, c:3, d:4], [a:2, b:4, c:6]]\n"
    );
}

#[test]
fn navigable_map_methods_reach_a_tree_map_and_no_other_map() {
    // These are `java.util.NavigableMap`'s, so offering them to every map would
    // turn a `MissingMethodException` Groovy raises into an answer. The negative
    // half is the point of the test: `firstKey` on a `LinkedHashMap` must still
    // throw, and it must throw *that* — which is why stderr is pinned rather
    // than just a non-zero exit.
    let src = r#"
def m = new TreeMap([b:2, a:1, c:3])
println([m.firstKey(), m.lastKey(), m.lowerKey('b'), m.higherKey('b'), m.floorKey('b'), m.ceilingKey('b')])
println([m.lowerKey('a'), m.higherKey('c'), m.floorKey('0'), m.ceilingKey('z')])
println([m.lowerEntry('b'), m.floorEntry('b'), m.ceilingEntry('bb'), m.higherEntry('c')])
println([m.headMap('c'), m.tailMap('b'), m.headMap('c', true), m.tailMap('b', false)])
println([m.subMap('a', 'c'), m.subMap('a', true, 'c', true), m.headMap('a'), m.tailMap('z')])
println([m.descendingMap(), m.descendingKeySet(), m.navigableKeySet(), m.comparator()])
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[a, c, a, c, b, b]\n\
         [null, null, null, null]\n\
         [a=1, b=2, c=3, null]\n\
         [[a:1, b:2], [b:2, c:3], [a:1, b:2, c:3], [c:3]]\n\
         [[a:1, b:2], [a:1, b:2, c:3], [:], [:]]\n\
         [[c:3, b:2, a:1], [c, b, a], [a, b, c], null]\n"
    );

    for (recv, method) in [
        ("[b:2, a:1]", "firstKey()"),
        ("[b:2, a:1]", "headMap('b')"),
        ("new HashMap([b:2, a:1])", "comparator()"),
        ("new HashMap([b:2, a:1])", "descendingMap()"),
    ] {
        let (_, err, ok) = run_full(&format!("println({recv}.{method})"));
        assert!(!ok, "{recv}.{method} should not dispatch");
        assert!(
            err.contains("groovy.lang.MissingMethodException"),
            "{recv}.{method} raised {err:?}"
        );
    }

    // `firstKey` on an *empty* TreeMap raises where `firstEntry` answers null —
    // a JDK asymmetry, not an oversight.
    let (_, err, ok) = run_full("println(new TreeMap([:]).firstKey())");
    assert!(!ok);
    assert!(err.contains("NoSuchElementException"), "{err:?}");
}

#[test]
fn the_gdk_end_entry_methods_reach_every_map_unlike_the_navigable_key_pair() {
    // `firstEntry`/`lastEntry`/`pollFirstEntry`/`pollLastEntry` are the GDK's
    // and are defined on any map — the mirror image of `firstKey`, which is
    // NavigableMap's. Gating these on the kind too would have been the natural
    // mistake. The polls also have to mutate through the handle.
    let src = r#"
println([[b:2, a:1].firstEntry(), [b:2, a:1].lastEntry(), [:].firstEntry(), [:].pollFirstEntry()])
def m = [b:2, a:1, c:3]
println([m.pollFirstEntry(), m])
println([m.pollLastEntry(), m])
def t = new TreeMap([b:2, a:1, c:3])
println([t.pollFirstEntry(), t])
println([[b:2, a:1].take(1), [b:2, a:1].drop(1), new TreeMap([b:2, a:1, c:3]).take(2)])
def seen = ''
def back = [b:2, a:1].reverseEach { k, v -> seen += k }
println([seen, back])
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[b=2, a=1, null, null]\n\
         [b=2, [a:1, c:3]]\n\
         [c=3, [a:1]]\n\
         [a=1, [b:2, c:3]]\n\
         [[b:2], [a:1], [a:1, b:2]]\n\
         [ab, [b:2, a:1]]\n"
    );
}

#[test]
fn a_map_as_cast_re_homes_only_when_it_is_not_already_that_class() {
    // `asType` hands back an operand that already *is* the target, and
    // `LinkedHashMap extends HashMap` — so `[a:1] as HashMap` stays a
    // `LinkedHashMap` and keeps insertion order. Casting the kind
    // unconditionally gets this wrong in the way that is hardest to notice: the
    // entries are identical, only the print order moves.
    let src = r#"
def cls = { it.getClass().getName() }
println([cls([b:2, a:1] as TreeMap), cls([a:1] as HashMap), cls([a:1] as LinkedHashMap), cls([a:1] as Map)])
println([cls(new TreeMap([b:2, a:1]) as HashMap), cls(new TreeMap([b:2, a:1]) as LinkedHashMap)])
println([[b:2, a:1] as TreeMap, new TreeMap([b:2, a:1]) as LinkedHashMap])
def m = new TreeMap([b:2, a:1])
println([m instanceof Map, m instanceof TreeMap, m instanceof SortedMap, m instanceof NavigableMap, m instanceof LinkedHashMap, m instanceof HashMap])
def h = new HashMap([b:2, a:1])
println([h instanceof HashMap, h instanceof LinkedHashMap, h instanceof TreeMap])
println([[b:2, a:1] instanceof LinkedHashMap, [b:2, a:1] instanceof HashMap, [b:2, a:1] instanceof TreeMap])
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[java.util.TreeMap, java.util.LinkedHashMap, java.util.LinkedHashMap, java.util.LinkedHashMap]\n\
         [java.util.HashMap, java.util.LinkedHashMap]\n\
         [[a:1, b:2], [a:1, b:2]]\n\
         [true, true, true, true, false, false]\n\
         [true, false, false]\n\
         [true, true, false]\n"
    );
}

#[test]
fn sub_map_selects_in_the_arguments_order_not_the_receivers() {
    // The GDK walks the requested keys and puts each into a fresh map, so the
    // result follows the argument. Filtering the receiver's entries instead
    // reads identically whenever the two orders agree — which every sorted
    // receiver does, so only an out-of-order request catches it.
    //
    // On a TreeMap the two- and four-argument spellings are NavigableMap's
    // half-open *range* instead, so the same name means two different things
    // depending on the receiver's kind.
    let src = r#"
println([[b:2, a:1, c:3].subMap(['c', 'a']), [b:2, a:1].subMap('a', 'b'), [a:1].subMap(['a', 'zz'])])
println([new TreeMap([b:2, a:1, c:3]).subMap(['c', 'a']), new TreeMap([b:2, a:1, c:3]).subMap('a', 'c')])
println([b:2, a:1, c:3].subMap(['c', 'a']).getClass().getName())
"#;
    let (out, ok) = run(src);
    assert!(ok);
    assert_eq!(
        out,
        "[[c:3, a:1], [a:1, b:2], [a:1]]\n\
         [[c:3, a:1], [a:1, b:2]]\n\
         java.util.LinkedHashMap\n"
    );
}

/// `s.split()` with no argument is `StringTokenizer`, not `split("")` or
/// `split(" ")`: runs of whitespace collapse and every empty field is dropped,
/// leading and trailing ones included. Splitting on a whitespace *regex* keeps
/// the leading empty, so the two must not share an implementation.
#[test]
fn no_argument_split_tokenizes_on_whitespace_runs() {
    let (out, ok) = run(r#"println("a b  c".split().toList())
println(" a b ".split().toList())
println("a\tb\nc".split().toList())
println("".split().toList())
println(" a b ".split("\\s+").toList())"#);
    assert!(ok);
    assert_eq!(
        out, "[a, b, c]\n[a, b]\n[a, b, c]\n[]\n[, a, b]\n",
        "no-argument split is not whitespace tokenizing"
    );
}

/// `x in str` is `str.isCase(x)`, which for a `String` is **equality**. Reading
/// it as containment is the natural guess and the wrong one — the same rule
/// decides whether `switch` takes a `case`.
#[test]
fn in_on_a_string_is_equality_not_containment() {
    let (out, ok) = run(r#"println('a' in 'abc')
println('abc' in 'abc')
switch ('abc') { case 'a': println('sub'); break; case 'abc': println('eq'); break }"#);
    assert!(ok);
    assert_eq!(out, "false\ntrue\neq\n");
}

/// `intdiv` is defined on the integral types only; a decimal on either side
/// raises, and the message names the RECEIVER whichever side it was.
#[test]
fn intdiv_refuses_a_decimal_on_either_side() {
    let (out, ok) = run(
        r#"try { 7.0.intdiv(2) } catch (e) { println(e.getClass().name + '|' + e.message) }
try { 7.intdiv(2.0) } catch (e) { println(e.message) }
println(7.intdiv(2))
println(7G.intdiv(2))"#,
    );
    assert!(ok);
    assert_eq!(
        out,
        "java.lang.UnsupportedOperationException|Cannot use intdiv() on this number type: \
         java.math.BigDecimal with value: 7.0\n\
         Cannot use intdiv() on this number type: java.lang.Integer with value: 7\n3\n3\n"
    );
}

/// A two-parameter closure sorts a map as a *comparator over entries*, the way
/// it sorts a list — not as the `(key, value)` pair that `each` and `collect`
/// spread. Treating it as a key extractor called the closure with one entry and
/// a null second argument, so every such sort raised.
#[test]
fn map_sort_with_two_parameters_is_an_entry_comparator() {
    let (out, ok) = run(r#"println([a:1, b:2].sort { x, y -> y.value <=> x.value })
println([b:2, a:1].sort { it.key })
println([b:2, a:1].sort())"#);
    assert!(ok);
    assert_eq!(out, "[b:2, a:1]\n[a:1, b:2]\n[a:1, b:2]\n");
}

/// `map << map` is `putAll` answering the receiver, so it chains. A `[key,
/// value]` list is NOT accepted — Groovy has only the `Map` and `Map.Entry`
/// overloads — and admitting it would quietly accept a program Groovy rejects.
#[test]
fn map_left_shift_puts_all_and_chains() {
    let (out, ok) = run(r#"def m = [a:1]
m << [b:2] << [c:3]
println(m)
try { [a:1] << ['b', 2] } catch (e) { println(e.getClass().name) }"#);
    assert!(ok);
    assert_eq!(out, "[a:1, b:2, c:3]\ngroovy.lang.MissingMethodException\n");
}

/// `setScale(int)` rounds with `UNNECESSARY`: it pads freely and raises rather
/// than dropping a digit. The two-argument form names the mode.
#[test]
fn set_scale_pads_but_refuses_to_lose_a_digit() {
    let (out, ok) = run(r#"println(1.5.setScale(3))
try { 1.5.setScale(0) } catch (e) { println(e.getClass().name + '|' + e.message) }
println(1.500.setScale(1))
println(2.5.scale())
println(2.5.precision())
println(2.5.unscaledValue())"#);
    assert!(ok);
    assert_eq!(
        out,
        "1.500\njava.lang.ArithmeticException|Rounding necessary\n1.5\n1\n2\n25\n"
    );
}

/// `asType(Type)` is the method spelling of `value as Type` and must run the
/// same coercion, not a second one that has drifted.
#[test]
fn as_type_runs_the_same_coercion_as_the_as_operator() {
    let (out, ok) = run(r#"println([1, 2].asType(Set).size())
println("42".asType(Integer))
println(5.asType(String))
println("abc".asType(List))
println(1.5.asType(Integer))"#);
    assert!(ok);
    assert_eq!(out, "2\n42\n5\n[a, b, c]\n1\n");
}

/// `Float`'s constants print the way a `Float` prints. groovyrs stores them as
/// the `double` nearest that text rather than a widened `f32`, which would
/// render `3.4028234663852886E38`.
#[test]
fn float_constants_print_as_floats() {
    let (out, ok) = run(r#"println(Float.MAX_VALUE)
println(Float.MIN_VALUE)
println(Float.SIZE)
println(Float.MAX_EXPONENT)
println(Character.SIZE)"#);
    assert!(ok);
    assert_eq!(out, "3.4028235E38\n1.4E-45\n32\n127\n16\n");
}

/// `a ?= b` is `a = a ?: b`, so it is Groovy *truth* that decides — a `0`, an
/// empty string and an empty list all take the right-hand side, not just null.
/// A property and a subscript are targets too.
#[test]
fn elvis_assignment_writes_only_over_a_falsy_target() {
    let (out, ok) = run(r#"def q = null; q ?= 5; println(q)
def r = 1; r ?= 5; println(r)
def z = 0; z ?= 5; println(z)
def s = ''; s ?= 'x'; println(s)
def m = [:]; m.a ?= 7; println(m)
def n = [a:1]; n.a ?= 7; println(n)
def k = [:]; k['j'] ?= 3; println(k)
def l = [null]; l[0] ?= 9; println(l)"#);
    assert!(ok);
    assert_eq!(out, "5\n1\n5\nx\n[a:7]\n[a:1]\n[j:3]\n[9]\n");
}

/// A package-qualified class name is an ordinary property chain in the parse
/// tree; only its *shape* says it is a class. A binding of the same name still
/// wins — `def java = [math: 5]` makes `java.math` the map read, as it does in
/// Groovy — and a class Groovy does not default-import (`RoundingMode`) must
/// stay unreachable bare.
#[test]
fn package_qualified_class_names_resolve() {
    let (out, ok) = run(r#"println(java.lang.Integer.MAX_VALUE)
println(java.lang.Math.max(3, 4))
println(java.math.BigDecimal.ONE)
println(java.util.Arrays.asList(1, 2))
def java = [math: 5]
println(java.math)"#);
    assert!(ok);
    assert_eq!(out, "2147483647\n4\n1\n[1, 2]\n5\n");

    let (bare, _, bare_ok) = run_full("println(RoundingMode)");
    assert!(!bare_ok, "Groovy does not default-import RoundingMode");
    assert_eq!(bare, "");
}

/// `setScale(n, mode)` and the three-argument `divide(y, scale, mode)` — the
/// two places a `RoundingMode` reaches the decimal model. `UNNECESSARY` is not
/// a rounding rule but an assertion, so it raises rather than truncating.
#[test]
fn rounding_mode_drives_set_scale_and_divide() {
    let (out, ok) = run(r#"println(1.5.setScale(0, java.math.RoundingMode.HALF_UP))
println(1.5.setScale(0, java.math.RoundingMode.HALF_EVEN))
println(2.5.setScale(0, java.math.RoundingMode.HALF_EVEN))
println((-1.5).setScale(0, java.math.RoundingMode.CEILING))
println(1.500.setScale(1, java.math.RoundingMode.UNNECESSARY))
try { 1.55.setScale(1, java.math.RoundingMode.UNNECESSARY) } catch (e) { println(e.message) }
println(1.0G.divide(3.0G, 5, java.math.RoundingMode.HALF_UP))
println(2.0G.divide(3.0G, 2, java.math.RoundingMode.DOWN))
try { 1.0G.divide(0G, 2, java.math.RoundingMode.HALF_UP) } catch (e) { println(e.message) }"#);
    assert!(ok);
    assert_eq!(
        out,
        "2\n2\n2\n-1\n1.5\nRounding necessary\n0.33333\n0.66\n/ by zero\n"
    );
}

/// A map carries a key → position index alongside its entry vector so a lookup
/// and an overwrite do not scan. The index is what could silently corrupt
/// insertion order, so this pins the sequences that move an entry: a removal
/// (which shifts every later position), a re-insert of a removed key, an
/// overwrite that must NOT move the key, and a clear.
#[test]
fn map_keeps_insertion_order_across_removal_and_reinsert() {
    let (out, ok) = run(
        r#"def a = [a:1, b:2, c:3]; a.remove('b'); a.d = 4; a.a = 9; println(a)
def b = [a:1, b:2, c:3]; b.remove('a'); b.a = 5; println(b)
def c = [:]; c.a = 1; c.b = 2; c.a = 3; println(c)
def d = [a:1]; d.clear(); d.z = 1; println(d)
def e = [a:1, b:2]; e.remove('a'); e.remove('b'); e.x = 1; e.y = 2; println(e)
def f = [:]; for (i in 1..5) { f["k$i"] = i }; f.remove('k3'); f['k6'] = 6
println(f); println(f.k6); println(f.size())
def g = new TreeMap([c:3, a:1, b:2]); g.d = 4; println(g); println(g.a)"#,
    );
    assert!(ok);
    assert_eq!(
        out,
        "[a:9, c:3, d:4]\n[b:2, c:3, a:5]\n[a:3, b:2]\n[z:1]\n[x:1, y:2]\n\
         [k1:1, k2:2, k4:4, k5:5, k6:6]\n6\n5\n[a:1, b:2, c:3, d:4]\n1\n"
    );
}

/// `put` answers the value it displaced (and `null` for a new key) where the
/// `m[k] = v` spelling answers nothing. The two share one write path, so the
/// return value is the only thing that distinguishes them.
#[test]
fn map_put_answers_the_displaced_value() {
    let (out, ok) = run(r#"def m = [a:1, b:2]
println(m.put('a', 9))
println(m.put('z', 9))
println(m)
println(m.get('a')); println(m.get('q')); println(m.get('q', 7)); println(m)"#);
    assert!(ok);
    assert_eq!(
        out,
        "1\nnull\n[a:9, b:2, z:9]\n9\nnull\n7\n[a:9, b:2, z:9, q:7]\n"
    );
}

/// A list handle answers append, count and positional access off the heap
/// rather than by detaching a copy of every element. The copy is where all the
/// window bookkeeping lives, so these pin what the shortcut must NOT change: a
/// `SubList` still aliases its backing list, still fails fast after a
/// structural change to it, and still resizes it when written through; a
/// negative or out-of-range index still gets its own diagnostic; and `add`,
/// `<<` and `push` still answer three different things.
#[test]
fn list_fast_paths_keep_window_and_index_semantics() {
    let (out, ok) = run(r#"def l = [1, 2, 3, 4, 5]
def s = l.subList(1, 4)
s[0] = 9
println(l); println(s)
s.add(99)
println(l); println(s.size())
def m = [1, 2, 3]
def w = m.subList(0, 2)
m.add(4)
try { println(w) } catch (e) { println(e.getClass().name) }
println(m[0]); println(m[-1]); println(m[9])
try { m.get(9) } catch (e) { println(e.message) }
try { m.get(-1) } catch (e) { println(e.getClass().name) }
println(m.getAt(-1))
def n = [1, 2, 3]
println(n.add(4)); println(n << 5); println(n)
n.push(0); println(n); println(n.pop())"#);
    assert!(ok);
    assert_eq!(
        out,
        "[1, 9, 3, 4, 5]\n[9, 3, 4]\n[1, 9, 3, 4, 99, 5]\n4\n\
         java.util.ConcurrentModificationException\n\
         1\n4\nnull\nIndex 9 out of bounds for length 4\n\
         java.lang.IndexOutOfBoundsException\n4\n\
         true\n[1, 2, 3, 4, 5]\n[1, 2, 3, 4, 5]\n[0, 1, 2, 3, 4, 5]\n0\n"
    );
}

/// A character buffer appends into its stored text rather than reading it out,
/// rebuilding it and writing it back — the shape that made `sb << x` in a loop
/// quadratic. The shortcut must keep the buffer a shared, mutable reference and
/// must not change what each spelling answers, so this pins aliasing, chaining,
/// the mutators that still rebuild (which read through the same handle), and
/// `substring`, the one member that reads a span without mutating.
#[test]
fn string_builder_appends_in_place_and_stays_shared() {
    let (out, ok) = run(r#"def s = new StringBuilder()
def alias = s
s << 'a' << 'b'
alias.append('c')
println(s); println(alias); println(s.is(alias))
println(s.length()); println(s.size()); println(s.isEmpty()); println(new StringBuilder().isEmpty())
def t = new StringBuilder('abcd')
println(t.substring(1)); println(t.substring(1, 3)); println(t)
t.insert(1, 'Z'); println(t)
t.deleteCharAt(0); println(t)
println(t.reverse())
def u = new StringBuilder(); u << 1 << true << null; println(u)
def w = new StringWriter(); w << 'hi'; println(w.toString())"#);
    assert!(ok);
    assert_eq!(
        out,
        "abc\nabc\ntrue\n3\n3\nfalse\ntrue\n\
         bcd\nbc\nabcd\naZbcd\nZbcd\ndcbZ\n1truenull\nhi\n"
    );
}

/// `set << x` is `Set.leftShift` — the `add`-that-answers-the-receiver the
/// method spelling already had. Only the operator was missing it, so `s << 4`
/// raised MissingMethodException where `s.add(4)` worked.
#[test]
fn set_left_shift_adds_and_answers_the_receiver() {
    let (out, ok) = run(r#"def s = new LinkedHashSet([3, 1, 2])
s << 4 << 4
println(s); println(s.getClass().name); println(s.size())
def t = new TreeSet([3, 1])
t << 2
println(t)
def u = [] as Set
println(u.isEmpty()); println((u << 'x').isEmpty())"#);
    assert!(ok);
    assert_eq!(
        out,
        "[3, 1, 2, 4]\njava.util.LinkedHashSet\n4\n[1, 2, 3]\ntrue\nfalse\n"
    );
}

/// A closure captures the *variable*, and a declaration inside a loop body makes
/// a **new** variable each iteration — so the closures built by two iterations
/// hold two different values.
///
/// groovyrs used to give every one of them the last iteration's value, because a
/// captured local was copied into the closure's frame instead of shared through
/// a cell. Every expectation below is Apache Groovy 5.1.0's own output.
#[test]
fn a_declaration_inside_a_loop_body_is_a_fresh_binding_per_iteration() {
    let (out, ok) = run(
        r#"def e=[]; int k=0; while (k<3) { def m=k; e << { m }; k++ }
println(e.collect{it()})
def q=[]; for (x in 0..2) { def y = x*2; q << { y } }
println(q.collect{y -> y()})
def n=[]; for (a in 0..1) { for (b in 0..1) { def p = "$a$b"; n << { p } } }
println(n.collect{it()})"#,
    );
    assert!(ok);
    assert_eq!(out, "[0, 1, 2]\n[0, 2, 4]\n[00, 01, 10, 11]\n");
}

/// The `for` variable itself is the opposite case: Groovy binds it **once** for
/// the whole loop, so every closure built in the body shares it and they all
/// read the value it was left at. Both `for (x in …)` forms and the C-style
/// loop agree, and so does a `while` over a variable declared outside it.
///
/// This is the rule the per-iteration cell above must not overrun — desugaring
/// the loop variable to a declaration inside the body would give each iteration
/// its own binding and print `[0, 1, 2]`, which Groovy does not.
#[test]
fn the_for_variable_is_one_binding_for_the_whole_loop() {
    let (out, ok) = run(r#"def a=[]; for (x in 0..2) a << { x }
println(a.collect{it()})
def b=[]; for (x in ['p','q','r']) b << { x }
println(b.collect{it()})
def c=[]; for (int i=0;i<3;i++) c << { i }
println(c.collect{it()})
def d=[]; int j=0; while (j<3) { d << { j }; j++ }
println(d.collect{it()})
def r=[]; for (x in 0..2) { r << { x }; x = x + 10 }
println(r.collect{it()})"#);
    assert!(ok);
    assert_eq!(
        out,
        "[2, 2, 2]\n[r, r, r]\n[3, 3, 3]\n[3, 3, 3]\n[12, 12, 12]\n"
    );
}

/// Sharing the variable is what a closure that *mutates* an outer name depends
/// on, so boxing must not turn a capture into a private copy: an accumulator
/// still accumulates, a name assigned after the closure was created reads its
/// new value, and a closure parameter is still per-call.
#[test]
fn a_captured_name_is_shared_not_copied() {
    let (out, ok) = run(r#"def cnt=0; def inc={ cnt++ }; inc(); inc(); println(cnt)
def tot=0; def s=[]; for (x in 0..2) { s << { tot += x } }; s.each{it()}; println(tot)
def outer=10; def p=[]; for (x in 0..2) p << { x + outer }; outer=100
println(p.collect{it()})
def fn = { z -> { -> z } }
println([fn(1)(), fn(2)()])
def g=[]; [0,1,2].each { v -> g << { v } }
println(g.collect{it()})
def h=[]; 3.times { t -> h << { t } }
println(h.collect{it()})"#);
    assert!(ok);
    assert_eq!(out, "2\n6\n[102, 102, 102]\n[1, 2]\n[0, 1, 2]\n[0, 1, 2]\n");
}

/// The membership index is an accelerator, never the answer: it must not change
/// which elements a set considers equal, and it must not disturb iteration
/// order. The cross-type probes are the ones that would break if a hash lookup
/// were treated as decisive, because `Set.contains` is `equals` and a hash of
/// the value cannot model it.
#[test]
fn the_set_membership_index_does_not_change_what_a_set_holds() {
    let (out, ok) = run(r#"def s = [3, 1, 2, 1] as Set
println(s); println(s.size())
println(s.add(3)); println(s.add(4)); println(s)
println(s.contains(1)); println(s.contains(9))
def m = [] as Set
m.add(1); m.add('a'); m.add(1); m.add('a'); m.add(2.5)
println(m); println(m.size())
println(m.contains('a')); println(m.contains(2.5))
def n = [] as Set
n.add(null); n.add(null); println(n.size())
def big = [] as Set
big.add(9223372036854775807L); big.add(9223372036854775806L); println(big.size())
def r = [1,2,3,4] as Set
println(r.minus([2,4] as Set)); println(r.intersect([3,4,5] as Set))
r.removeAll([1] as Set); println(r)
r.retainAll([2,3] as Set); println(r)"#);
    assert!(ok);
    assert_eq!(
        out,
        "[3, 1, 2]\n3\nfalse\ntrue\n[3, 1, 2, 4]\ntrue\nfalse\n\
         [1, a, 2.5]\n3\ntrue\ntrue\n1\n2\n\
         [1, 3]\n[3, 4]\n[2, 3, 4]\n[2, 3]\n"
    );
}

/// `withDefault` answers a live **view** onto the receiver, not a copy: the
/// default it writes for a missing key lands in the map it was taken from, a
/// later `put` on that map is visible through the view, and the two compare
/// equal — but it is a distinct handle, and only the view defaults a key.
///
/// groovyrs used to copy, so `m` never saw the default and the view never saw a
/// later `put`. Expectations are Apache Groovy 5.1.0's own output.
#[test]
fn with_default_is_a_view_onto_the_map_it_was_taken_from() {
    let (out, ok) = run(r#"def m = [a:1]
def wd = m.withDefault{ k -> k.toUpperCase() }
println(m['zz']); println(m)
wd['q']
println(m); println(wd)
m.put('b', 2)
println(wd['b']); println(wd)
wd.put('c', 3)
println(m)
println(wd.getClass().getName()); println(wd.is(m)); println(wd == m)"#);
    assert!(ok);
    assert_eq!(
        out,
        "null\n[a:1]\n[a:1, q:Q]\n[a:1, q:Q]\n2\n[a:1, q:Q, b:2]\n\
         [a:1, q:Q, b:2, c:3]\ngroovy.lang.MapWithDefault\nfalse\ntrue\n"
    );
}

/// A compound assignment or a `++`/`--` on a **subscript** or a **property**
/// target. groovyrs parsed neither — `m['a'] += 1`, `l[0]++`, `p.x += 1` were
/// all syntax errors — and the lowering has to evaluate the receiver and the
/// index exactly once, which is why the read is a stack duplicate rather than a
/// second evaluation of the target expression.
#[test]
fn compound_assignment_reaches_subscript_and_property_targets() {
    let (out, ok) = run(r#"def calls = 0
def key = { calls++; 'a' }
def m = [a:1]
m[key()] += 5
println("calls=$calls m=$m")
def rc = 0
def rf = { rc++; m }
rf()['a'] += 5
println("rc=$rc m=$m")
def l = [1,2,3]
l[0] += 10; l[1] *= 3; l[2] -= 1; println(l)
def o = [a:1]; o.a += 7; println(o)
def s = [x:'p']; s['x'] += 'q'; println(s)
def mm = [a:1]; mm['a']++; println(mm)
def nn = [a:1]; nn.a++; println(nn)
def ll = [1,2]; ll[0]++; ll[1]--; println(ll)
def d = [a:10]; d.a /= 4; println(d)
def e = [a:10]; e['a'] %= 3; println(e)"#);
    assert!(ok);
    assert_eq!(
        out,
        "calls=1 m=[a:6]\nrc=1 m=[a:11]\n[11, 6, 2]\n[a:8]\n[x:pq]\n\
         [a:2]\n[a:2]\n[2, 1]\n[a:2.5]\n[a:1]\n"
    );
}

/// A C-style `for` header may declare or update several names at once, and a
/// declarator after the first inherits the first's type.
#[test]
fn a_c_style_for_header_takes_comma_separated_clauses() {
    let (out, ok) = run(r#"for (int i=0, j=3; i<j; i++, j--) println("a i=$i j=$j")
for (i=0, j=5; i<j; i++, j--) println("b i=$i j=$j")
for (int i=0, j=0, k=9; i<2; i++, j+=2, k--) println("c $i $j $k")
for (int i=0, j=6; i<j; i++, j--) { if (i==1) continue; println("d i=$i j=$j") }
int q=0; for (; q<2; q++) println("e $q")"#);
    assert!(ok);
    assert_eq!(
        out,
        "a i=0 j=3\na i=1 j=2\nb i=0 j=5\nb i=1 j=4\nb i=2 j=3\n\
         c 0 0 9\nc 1 2 8\nd i=0 j=6\nd i=2 j=4\ne 0\ne 1\n"
    );
}

/// A varargs closure parameter (`{ Object... xs -> }`) collects every argument
/// from its position onward, and a call that stops short of it still binds it —
/// to an empty list. groovyrs did not parse the `...` at all.
///
/// The collected value is a `List`, where Groovy hands over an `Object[]`; see
/// BUGS.md's *Java arrays* entry for what that costs (`xs.length`,
/// `xs.getClass()`). Everything asserted below agrees with Apache Groovy 5.1.0.
#[test]
fn a_closure_parameter_can_be_varargs() {
    let (out, ok) = run(r#"def v = { Object... xs -> xs.size() + ':' + xs.toList() }
println(v()); println(v(1,2,3))
def v2 = { String pre, Object... xs -> pre + xs.toList() }
println(v2('a', 1, 2)); println(v2('b'))
def f = { int... n -> n.sum() }
println(f(1,2,3))
def g = { a, Object... rest -> "$a|$rest" }
println(g(1)); println(g(1,2,3))
def h = { Object... xs -> xs.collect{ it * 2 } }
println(h(1,2))
def plain = { a, b -> a + b }
println(plain(1,2))"#);
    assert!(ok);
    assert_eq!(
        out,
        "0:[]\n3:[1, 2, 3]\na[1, 2]\nb[]\n6\n1|[]\n1|[2, 3]\n[2, 4]\n3\n"
    );
}

/// The bitwise and exponent compound assignments — `<<=`, `>>=`, `>>>=`, `&=`,
/// `|=`, `^=`, `**=`. groovyrs parsed none of them, on any target.
///
/// They are lowered by the binary-operator path itself, so they inherit its
/// operand routing rather than repeating it. That is what the second block
/// checks: a `BigInteger` operand takes the builtin where an `int` keeps a
/// native op, a shift masks its count to the left operand's *Java* width
/// (`1 << 32` is `1`, `1L << 32` is `4294967296`), and `**` narrows to that same
/// width. Every expectation is Apache Groovy 5.1.0's own output.
#[test]
fn the_bitwise_compound_assignments_reach_every_target() {
    let (out, ok) = run(r#"def a=1; a <<= 3; println(a)
def b=32; b >>= 2; println(b)
def c=-32; c >>>= 2; println(c)
def d=12; d &= 10; println(d)
def e=12; e |= 3; println(e)
def f=12; f ^= 10; println(f)
def g=2; g **= 10; println(g)
def m=[a:1]; m['a'] <<= 3; println(m)
def n=[a:12]; n.a &= 10; println(n)
def l=[1,2]; l[0] |= 6; println(l)
def p=[a:2]; p['a'] **= 8; println(p)
class C { int v = 12 }
def cc = new C(); cc.v &= 10; println(cc.v)"#);
    assert!(ok);
    assert_eq!(
        out,
        "8\n8\n1073741816\n8\n15\n6\n1024\n[a:8]\n[a:8]\n[7, 2]\n[a:256]\n8\n"
    );
}

/// A compound assignment through a subscript evaluates its receiver and index
/// **once**, the bitwise forms included — `m[key()] <<= 5` calls `key` a single
/// time. The read is a stack duplicate, not a second evaluation of the target.
#[test]
fn a_bitwise_compound_assignment_evaluates_its_target_once() {
    let (out, ok) = run(r#"def calls=0; def key={ calls++; 'a' }
def q=[a:1]; q[key()] <<= 5
println("calls=$calls q=$q")"#);
    assert!(ok);
    assert_eq!(out, "calls=1 q=[a:32]\n");
}

/// The compound forms take the same operand routing the binary operators do:
/// a `BigInteger` operand, the `Integer`-versus-`Long` shift width, `**`'s
/// narrowing, and `<<`'s append on a list.
#[test]
fn the_bitwise_compound_assignments_follow_the_operand_types() {
    let (out, ok) = run(r#"def a=1G; a <<= 4; println(a + ' ' + a.getClass().getName())
def b=12G; b &= 10; println(b)
def c=1; c <<= 32; println(c)
long d=1; d <<= 32; println(d)
def e=256; e >>= 33; println(e)
def f=2; f **= 40; println(f + ' ' + f.getClass().getName())
long g=2; g **= 40; println(g + ' ' + g.getClass().getName())
def l=[1,2]; l <<= 3; println(l)
def m=[:]; m['k']=1G; m['k'] <<= 4; println(m)
long o=-1; o >>>= 60; println(o)
def p=[a:[1,2]]; p['a'] <<= 9; println(p)"#);
    assert!(ok);
    assert_eq!(
        out,
        "16 java.math.BigInteger\n8\n1\n4294967296\n128\n\
         1099511627776 java.math.BigInteger\n1099511627776 java.lang.Long\n\
         [1, 2, 3]\n[k:16]\n15\n[a:[1, 2, 9]]\n"
    );
}

/// Java arrays. `new int[3]`, `as int[]`, `.length`, the class descriptors, and
/// the array-returning library methods — none of which groovyrs modeled: an
/// array was a `List`, so `new int[3]` did not resolve and `"abc".bytes.length`
/// raised.
///
/// An array is a list *kind*, the way a `TreeMap` is a `MapKind`. Groovy treats
/// the two almost identically — an array iterates, subscripts, `collect`s and
/// prints as a list does — so what this pins is the small part that differs.
/// Every expectation is Apache Groovy 5.1.0's own output.
#[test]
fn java_arrays_are_a_list_kind_with_their_own_class_and_length() {
    let (out, ok) = run(r#"def a = new int[3]
println("$a ${a.length} ${a.getClass().getName()}")
a[0] = 7; println(a)
def b = [1,2,3] as int[]
println("${b.getClass().getName()} ${b.length}")
println(([1,2,3] as Integer[]).getClass().getName())
println("${new String[2]} ${(new String[2]).getClass().getName()}")
println("${(new long[2]).getClass().getName()}${(new double[2]).getClass().getName()}")
println("${new boolean[2]} ${(new boolean[2]).getClass().getName()}")
println(b instanceof int[])
println((b as List).getClass().getName())"#);
    assert!(ok);
    assert_eq!(
        out,
        "[0, 0, 0] 3 [I\n[7, 0, 0]\n[I 3\n[Ljava.lang.Integer;\n\
         [null, null] [Ljava.lang.String;\n[J[D\n[false, false] [Z\n\
         true\njava.util.ArrayList\n"
    );
}

/// The library methods that answer an array rather than a `List`, and the one
/// array Groovy does not print like a list: a `char[]` prints its characters
/// run together where a `byte[]` prints `[97, 98, 99]`.
#[test]
fn the_array_returning_library_methods_answer_arrays() {
    let (out, ok) = run(r#"def d = "a,b".split(",")
println("${d.getClass().getName()} ${d.length}")
def e = [1,2].toArray()
println("$e ${e.getClass().getName()} ${e.length}")
def f = "abc".toCharArray()
println("$f ${f.getClass().getName()} ${f.length}")
def g = "abc".bytes
println("$g ${g.getClass().getName()} ${g.length}")
println("é".bytes)
def v = { Object... xs -> "${xs.length} ${xs.getClass().getName()}" }
println(v(1,2))"#);
    assert!(ok);
    assert_eq!(
        out,
        "[Ljava.lang.String; 2\n[1, 2] [Ljava.lang.Object; 2\n\
         abc [C 3\n[97, 98, 99] [B 3\n[-61, -87]\n2 [Ljava.lang.Object;\n"
    );
}

/// `.length` stays an error on a `List`. Modeling an array as a plain list
/// would make `[1, 2].length` answer where Groovy raises, which is why the
/// property is gated on the array kind rather than added to every list.
#[test]
fn a_plain_list_still_has_no_length() {
    let (out, err, ok) = run_full("println([1, 2].length)");
    assert!(!ok, "expected a fault, got: {out}");
    assert!(
        err.contains("MissingPropertyException") && err.contains("length"),
        "stderr was: {err}"
    );
}

/// `new int[-1]` is a `NegativeArraySizeException` naming the size, not a
/// zero-length array and not a bare `Throwable`.
#[test]
fn a_negative_array_size_raises() {
    let (out, ok) = run(
        r#"try { new int[-1] } catch (e) { println(e.getClass().getName()); println(e.getMessage()) }"#,
    );
    assert!(ok);
    assert_eq!(out, "java.lang.NegativeArraySizeException\n-1\n");
}

/// A boxed binding's cell is reused when no closure captured it, and that reuse
/// must be invisible. These are the shapes where it could show: a recursive call
/// re-entering the declaration while the caller's cell is still live, one
/// function called twice, a closure built on only some iterations, and a closure
/// that never outlives its iteration.
///
/// Every expectation is Apache Groovy 5.1.0's own output.
#[test]
fn reusing_an_uncaptured_cell_is_invisible() {
    let (out, ok) = run(r#"def out = []
def rec
rec = { n -> def y = n; if (n > 0) rec(n-1); out << { y }; return }
rec(3)
println(out.collect{it()})
def mk = { n -> def z = n * 10; return { z } }
println([mk(1)(), mk(2)(), mk(3)()])
def f = { def acc = []; for (i in 0..2) { def v = i; acc << { v } }; acc }
println(f().collect{it()})
println(f().collect{it()})
def k = []
for (i in 0..3) { def m = i; if (i % 2 == 0) k << { m } }
println(k.collect{it()})
def noesc = 0
for (i in 0..2) { def w = i; def cl = { w }; noesc += cl() }
println(noesc)
def deep = []
for (i in 0..1) { def p = i; for (j in 0..1) { def q = j; deep << { "$p$q" } } }
println(deep.collect{it()})"#);
    assert!(ok);
    assert_eq!(
        out,
        "[0, 1, 2, 3]\n[10, 20, 30]\n[0, 1, 2]\n[0, 1, 2]\n[0, 2]\n3\n\
         [00, 01, 10, 11]\n"
    );
}

/// `methodMissing` and `propertyMissing` — Groovy's last-resort dispatch hooks,
/// which groovyrs did not have: an unresolved call or read raised straight away.
///
/// The hook runs only once every real resolution has failed, the GDK included:
/// `obj.with { … }` is a GDK method on any object, and a hook that shadowed it
/// would turn a working construct into a silently wrong one. Every expectation
/// is Apache Groovy 5.1.0's own output.
#[test]
fn method_missing_is_the_last_resort_not_the_first() {
    let (out, ok) = run(r#"class D {
  def real() { 'real' }
  int n = 5
  def methodMissing(String name, args) { "mm:$name:${args.size()}" }
}
def d = new D()
println(d.real())
println(d.whatever())
println(d.whatever(1,2,3))
println(d.getN())
class E extends D { }
println(new E().inherited(7))
class W { def v = 3; def methodMissing(String n, args) { "mm:$n" } }
def w = new W()
println(w.with { v })
println(w.is(w))
println(w.anything())"#);
    assert!(ok);
    assert_eq!(
        out,
        "real\nmm:whatever:0\nmm:whatever:3\n5\nmm:inherited:1\n3\ntrue\nmm:anything\n"
    );
}

/// A hook that throws propagates its own throwable, and a real failure inside a
/// method the class *does* have is never rerouted into the hook — only a
/// `MissingMethodException` for the call itself hands over.
#[test]
fn a_failure_inside_a_real_method_does_not_reach_method_missing() {
    let (out, ok) = run(r#"class X { def v = 1; def methodMissing(String n, args) { throw new IllegalStateException("boom:$n") } }
try { new X().nope() } catch (e) { println(e.getClass().getName() + ':' + e.getMessage()) }
class Y { def go() { throw new IllegalArgumentException('inner') }; def methodMissing(String n, args) { 'mm' } }
try { new Y().go() } catch (e) { println(e.getClass().getSimpleName() + ':' + e.getMessage()) }
class Z2 { def v = 1 }
try { new Z2().nope() } catch (e) { println(e.getClass().getSimpleName()) }"#);
    assert!(ok);
    assert_eq!(
        out,
        "java.lang.IllegalStateException:boom:nope\n\
         IllegalArgumentException:inner\nMissingMethodException\n"
    );
}

/// `propertyMissing(String name)` reads and `propertyMissing(String name, value)`
/// writes, after the getter/setter and the declared fields — Groovy's own order.
#[test]
fn property_missing_answers_an_unknown_read_and_write() {
    let (out, ok) = run(r#"class P { def propertyMissing(String n) { "pm:$n" }; def known = 'k' }
def p = new P()
println(p.known)
println(p.unknown)
class Q { def propertyMissing(String n, v) { println("set $n=$v") } }
def q = new Q(); q.zzz = 3"#);
    assert!(ok);
    assert_eq!(out, "k\npm:unknown\nset zzz=3\n");
}

/// The spread operator in a list literal (`[a, *b, c]`) and a map literal
/// (`[k: v, *:m]`), neither of which groovyrs parsed.
///
/// Both lower to the `+` operator, because that is what the spread means: `+`
/// on a list concatenates and on a map merges with the right key winning, which
/// is exactly the spread's semantics. `Expr::Iterable` turns the spread operand
/// into its elements — the same conversion `for (x in b)` uses — so a range and
/// a set spread too. Every expectation is Apache Groovy 5.1.0's own output.
#[test]
fn the_spread_operator_works_in_list_and_map_literals() {
    let (out, ok) = run(r#"def l = [1,2]
println([0, *l, 3])
println([*l])
println([*l, *l])
println([*(1..3)])
println([*([9,8] as Set)])
println([*[]])
println([1, *[], 2])
def m = [a:1]
println([b:2, *:m])
println([*:m, a:9])
println([*:m])
println([*:m, *:[c:3]])
println([*:[:], z:1])
println([*m.keySet()])
def r = [1,2,3]
println([*r[0..1]])
println([[1,2], *[[3]]])"#);
    assert!(ok);
    assert_eq!(
        out,
        "[0, 1, 2, 3]\n[1, 2]\n[1, 2, 1, 2]\n[1, 2, 3]\n[9, 8]\n[]\n[1, 2]\n\
         [b:2, a:1]\n[a:9]\n[a:1]\n[a:1, c:3]\n[z:1]\n[a]\n[1, 2]\n[[1, 2], [3]]\n"
    );
}

/// The spread operator in an **argument** position — `f(*args)` — across every
/// call shape: a closure variable, a user function, a method, a constructor,
/// `super(...)`, and a postfix call application.
///
/// A spread cannot be desugared the way a literal's is, because the number of
/// arguments is not known until the operand is evaluated and a call opcode
/// carries its count in the instruction. The whole argument list is built at run
/// time and parked; the call is emitted with a count of zero and takes the
/// parked list. Every expectation is Apache Groovy 5.1.0's own output.
#[test]
fn the_spread_operator_expands_a_calls_arguments() {
    let (out, ok) = run(r#"def f = { a, b -> "$a|$b" }
println(f(*[1,2]))
println(f(1, *[2]))
println(f(*[1], 2))
def add(a, b) { a + b }
println(add(*[1,2]))
def three(a, b, c) { "$a$b$c" }
println(three(*[1,2], 3))
println('abc'.substring(*[1,2]))
println(Math.max(*[3,9]))
def m = [:]; m.put(*['k', 1]); println(m)
def cl = { a, b -> a * b }
println(cl.call(*[3,4]))
def outer = { x -> { y -> "$x$y" } }
println(outer(*[1])(*[2]))"#);
    assert!(ok);
    assert_eq!(
        out,
        "1|2\n1|2\n1|2\n3\n123\nb\n9\n[k:1]\n12\n12\n"
    );
}

/// A spread's interaction with the parameter shapes it has to agree with:
/// varargs (which collects what the spread supplies), a defaulted parameter
/// (which a short spread leaves at its default), a zero-parameter closure, and
/// an empty spread — plus multiple spreads in one call and a spread of a range,
/// a set and an array.
#[test]
fn a_spread_argument_agrees_with_varargs_defaults_and_emptiness() {
    let (out, ok) = run(r#"def v = { Object... xs -> "v:${xs.length}:${xs.toList()}" }
println(v(*[1,2,3]))
println(v(*[]))
println(v(1, *[2,3]))
def d = { a, b = 9 -> "$a/$b" }
println(d(*[1]))
println(d(*[1,2]))
def e = { -> 'noargs' }
println(e(*[]))
def f = { a, b -> "$a|$b" }
println(f(*(1..2)))
println(f(*([7,8] as Set)))
println(f(*([3,4] as int[])))
def g = { a, b, c, d2 -> "$a$b$c$d2" }
println(g(*[1,2], *[3,4]))
println(g(1, *[2,3], 4))
def n = 0
def side = { n++; [1,2] }
println(f(*side()))
println("side=$n")"#);
    assert!(ok);
    assert_eq!(
        out,
        "v:3:[1, 2, 3]\nv:0:[]\nv:3:[1, 2, 3]\n1/9\n1/2\nnoargs\n\
         1|2\n7|8\n3|4\n1234\n1234\n1|2\nside=1\n"
    );
}

/// A spread reaches a constructor and a `super(...)` call, where the argument
/// count picks the constructor — so the arity has to come from the spread list
/// rather than from the opcode, which carries zero.
#[test]
fn a_spread_argument_picks_a_constructor_by_its_runtime_arity() {
    let (out, ok) = run(r#"class P { int a; int b; P(int a, int b) { this.a=a; this.b=b }; String toString() { "P($a,$b)" } }
println(new P(*[1,2]))
class Q extends P { Q(xs) { super(*xs) }; String toString(){ "Q(${a},${b})" } }
println(new Q([3,4]))
class C { def sum(a, b) { a + b }; def go(xs) { sum(*xs) } }
println(new C().go([4,5]))
println(new C().sum(*[6,7]))
def sb = new StringBuilder(*['seed'])
println(sb)"#);
    assert!(ok);
    assert_eq!(out, "P(1,2)\nQ(3,4)\n9\n13\nseed\n");
}

/// A `*` outside an argument list is refused rather than silently mis-run — the
/// literal forms are desugared by the parser, so anywhere else there is nothing
/// to expand it into.
#[test]
fn a_spread_outside_a_call_or_literal_is_a_compile_error() {
    let (out, err, ok) = run_full("def xs = [1,2]\ndef y = *xs\nprintln(y)");
    assert!(!ok, "expected a compile error, got: {out}");
    assert!(err.contains("groovyrs:"), "stderr was: {err}");
}

/// `String.leftShift` answers a `java.lang.StringBuffer`, not a `String`. It is
/// the GDK's builder-append, so the result is mutable, is a reference, and is
/// never `equals` to a `String` of the same characters. groovyrs answered a
/// `String`, which got the characters right and all three of those wrong.
///
/// `==` and `.equals` differ on a buffer and both are pinned: `==` is
/// `compareTo` (a `StringBuffer` is `Comparable`), so two buffers holding the
/// same characters are `==` — but a buffer and a `String` are not comparable to
/// each other, so that is false. `.equals` is `Object`'s, so it is identity.
/// Every expectation is Apache Groovy 5.1.0's own output.
#[test]
fn string_left_shift_answers_a_string_buffer() {
    let (out, ok) = run(r#"def a = 'ab' << 'cd'
println(a)
println(a.getClass().getName())
println(a.reverse())
println(a)
def b = 'x'; b <<= 'y'; println(b); println(b.getClass().getName())
def c = 'p' << 'q'
c.append('r'); println(c)
println(c instanceof StringBuffer)
println(c instanceof CharSequence)
println(('ab' << 'cd') + 'z')
println("${'a' << 'b'}")
def d = 'm' << 'n'
def e = d
e.append('o')
println(d)"#);
    assert!(ok);
    assert_eq!(
        out,
        "abcd\njava.lang.StringBuffer\ndcba\ndcba\nxy\njava.lang.StringBuffer\n\
         pqr\ntrue\ntrue\nabcdz\nab\nmno\n"
    );
}

/// A buffer's `==` compares contents against another buffer and nothing else;
/// its `.equals` is identity.
#[test]
fn a_character_buffer_compares_only_against_another_buffer() {
    let (out, ok) = run(r#"def x = 'a' << 'b'
def y = 'a' << 'b'
def z = x
println(x == x); println(x == y); println(x == z)
println(x.equals(x)); println(x.equals(y))
println('ab' == x)
println(x.toString() == 'ab')
def sb = new StringBuilder('ab')
println(sb == sb); println(sb == new StringBuilder('ab')); println(sb.equals(sb))"#);
    assert!(ok);
    assert_eq!(
        out,
        "true\ntrue\ntrue\ntrue\nfalse\nfalse\ntrue\ntrue\ntrue\ntrue\n"
    );
}

/// `trait` — a stateful interface with method bodies. groovyrs did not parse the
/// keyword at all.
///
/// A trait registers as an interface (non-instantiable, and `instanceof`
/// answers for it) and differs in the two ways that make it a trait: it carries
/// **state**, so every implementing class materialises its fields, and two
/// traits declaring the same method resolve last-declared-first rather than
/// being an ambiguity. Every expectation is Apache Groovy 5.1.0's own output.
#[test]
fn a_trait_carries_state_and_method_bodies() {
    let (out, ok) = run(r#"trait Named { String name = 'anon'; String who() { "I am $name" } }
trait Aged { int age = 0; String info() { "age=$age" } }
class Person implements Named, Aged {}
def p = new Person()
println(p.who())
println(p.info())
p.name = 'bo'; p.age = 7
println(p.who() + ' ' + p.info())
println(p instanceof Named)
println(p instanceof Aged)
class Sub extends Person {}
def s = new Sub(); s.name = 'sub'; println(s.who())"#);
    assert!(ok);
    assert_eq!(
        out,
        "I am anon\nage=0\nI am bo age=7\ntrue\ntrue\nI am sub\n"
    );
}

/// Trait linearization: the class's own method wins over any trait's, the trait
/// declared **last** wins between two that conflict, and `super` inside a trait
/// reaches the trait it extends — through a chain of them.
#[test]
fn trait_methods_resolve_last_declared_first() {
    let (out, ok) = run(r#"trait A { String who() { 'A' } }
trait B { String who() { 'B' } }
class D implements A, B {}
println(new D().who())
class E implements A, B { String who() { 'E' } }
println(new E().who())
trait Base { String tag() { 'base' } }
trait Mid extends Base { String tag() { 'mid>' + super.tag() } }
trait Top extends Mid { String tag() { 'top>' + super.tag() } }
class T implements Top {}
println(new T().tag())
trait Abs { abstract String must(); String call2() { 'via ' + must() } }
class Impl implements Abs { String must() { 'impl' } }
println(new Impl().call2())
trait Cnt { int n = 0; def inc() { n = n + 1; this } }
class K implements Cnt {}
def k = new K(); k.inc().inc(); println(k.n)"#);
    assert!(ok);
    assert_eq!(out, "B\nE\ntop>mid>base\nvia impl\n2\n");
}

/// The AST-transform annotation family — `@ToString`, `@EqualsAndHashCode`,
/// `@TupleConstructor` and `@Canonical` (which is the three together). groovyrs
/// did not parse a class-level annotation at all.
///
/// The *declaration* is a compile-time decision, so it travels as a flag on the
/// class; the generated members live in the host, where an instance's fields,
/// its rendering and its equality already do. A declared member always wins over
/// the generated one, which is what Groovy's transforms do. Every expectation is
/// Apache Groovy 5.1.0's own output.
#[test]
fn the_ast_transform_annotations_generate_their_members() {
    let (out, ok) = run(r#"import groovy.transform.*
@Canonical class C { int a; String b }
def c1 = new C(1,'z'); def c2 = new C(1,'z')
println(c1)
println(c1 == c2)
println(c1.hashCode() == c2.hashCode())
println(c1 == new C(2,'z'))
println(new C(a:5, b:'q'))
println(new C(7))
@ToString class D { int a; String b; String c }
println(new D(a:1, b:'x'))
@ToString(includeNames=true) class E { int a; String b }
println(new E(a:1, b:'y'))
@EqualsAndHashCode class F { int a; String toString() { 'mine' } }
println(new F(a:1))
@ToString class G2 { int a; String toString() { 'declared' } }
println(new G2(a:1))
@Canonical class H2 { int a }
def l = [new H2(1), new H2(2)]
println(l)
println(l.contains(new H2(2)))"#);
    assert!(ok);
    assert_eq!(
        out,
        "C(1, z)\ntrue\ntrue\nfalse\nC(5, q)\nC(7, null)\nD(1, x, null)\n\
         E(a:1, b:y)\nmine\ndeclared\n[H2(1), H2(2)]\ntrue\n"
    );
}

/// Groovy's generated `hashCode` is `org.codehaus.groovy.util.HashCodeHelper`:
/// `initHash()` is 127 and each field folds in as `59 * current + hashCode(field)`.
/// Measured against the helper itself, so the *number* matches rather than
/// merely being self-consistent.
#[test]
fn a_generated_hash_code_matches_groovys_own_number() {
    let (out, ok) = run(r#"import groovy.transform.EqualsAndHashCode
@EqualsAndHashCode class P { int a; String b }
println(new P(a:1,b:'x').hashCode())
@EqualsAndHashCode class Q { int a }
println(new Q(a:0).hashCode())
println(new Q(a:1).hashCode())"#);
    assert!(ok);
    assert_eq!(out, "442266\n7493\n7494\n");
}

/// Groovy's **map constructor** — `new P(a: 1, b: 2)` — and the primitive field
/// defaults it exposes. Named arguments gather into one map passed as the call's
/// first argument, and a class with no matching declared constructor sets the
/// named properties on a default-built instance. An uninitialised field of a
/// primitive type starts at that type's zero, not at `null`.
#[test]
fn named_arguments_build_a_map_and_drive_the_map_constructor() {
    let (out, ok) = run(r#"class P { int a; int b; String toString(){ "P($a,$b)" } }
println(new P(a:1, b:2))
println(new P(b:5))
println(new P())
def f = { Map m, x -> "$m|$x" }
println(f(a:1, 9))
def g(Map m) { m }
println(g(k:1, j:2))
class Q { int a; Q(int a){ this.a = a }; String toString(){"Q($a)"} }
println(new Q(3))
class R { int i; long l; double d; boolean z; String s; String toString(){ "$i/$l/$d/$z/$s" } }
println(new R())"#);
    assert!(ok);
    assert_eq!(
        out,
        "P(1,2)\nP(0,5)\nP(0,0)\n[a:1]|9\n[k:1, j:2]\nQ(3)\n0/0/0.0/false/null\n"
    );
}

/// **GPath**: a property a collection does not itself have is collected from its
/// elements, so `people.name` is every `name` and it chains through nesting.
///
/// The two absences Groovy distinguishes are both pinned: a `null` *element* is
/// skipped rather than contributing `null`, while a missing *map key* still
/// contributes `null`. A property the collection really has — `empty`, `class` —
/// answers instead of spreading. Every expectation is Apache Groovy 5.1.0's own
/// output.
#[test]
fn a_property_a_collection_lacks_is_collected_from_its_elements() {
    let (out, ok) = run(r#"def l = [[n:[a:1]],[n:[a:2]]]
println(l.n)
println(l.n.a)
class P { String name; int age }
def ps = [new P(name:'x',age:1), new P(name:'y',age:2)]
println(ps.name)
println(ps.age)
println(ps.name.size())
println([[a:1],[b:2]].a)
println([].nope)
println(([[a:1],[a:2]] as Set).a)
println([[a:1],[a:2]].a.getClass().getName())
class Q { String n }
println([new Q(n:'q'), null].n)
println([1,2].empty)
println([].empty)
println(([1] as Set).empty)
println([a:1].empty)
println("".empty)
println([1,2].class.name)"#);
    assert!(ok);
    assert_eq!(
        out,
        "[[a:1], [a:2]]\n[1, 2]\n[x, y]\n[1, 2]\n2\n[1, null]\n[]\n[1, 2]\n\
         java.util.ArrayList\n[q]\nfalse\ntrue\nfalse\nnull\ntrue\n\
         java.util.ArrayList\n"
    );
}

/// A closure's `delegate` and `resolveStrategy`, and the `Closure` constants a
/// script names when setting one. A user-set delegate joins the same resolution
/// chain `with`/`tap` install their receiver on, so a name the owner cannot
/// resolve reaches it.
///
/// Every expectation is Apache Groovy 5.1.0's own output.
#[test]
fn a_closure_carries_a_delegate_and_a_resolve_strategy() {
    let (out, ok) = run(r#"println(Closure.OWNER_FIRST)
println(Closure.DELEGATE_FIRST)
println(Closure.OWNER_ONLY)
println(Closure.DELEGATE_ONLY)
def e = { -> 1 }
println(e.resolveStrategy)
e.resolveStrategy = Closure.DELEGATE_FIRST
println(e.resolveStrategy)
e.delegate = [a:1]
println(e.delegate)
def d = { -> foo() }
d.delegate = [foo: { "dele" }]
println(d())
def g = { -> bar }
g.delegate = [bar: 42]
println(g())
def two = { -> tag() }
two.delegate = [tag: { 'first' }]
println(two())
two.delegate = [tag: { 'second' }]
println(two())"#);
    assert!(ok);
    assert_eq!(
        out,
        "0\n1\n2\n3\n0\n1\n[a:1]\ndele\n42\nfirst\nsecond\n"
    );
}

/// Groovy's map-as-object idiom: a map entry holding a closure is callable as a
/// method, which is what makes a plain map usable as a closure's `delegate`. It
/// must not shadow the map's own interface — `[size: 9].size()` is the map's
/// size while `[size: 9].size` is the key.
#[test]
fn a_map_entry_holding_a_closure_is_callable_as_a_method() {
    let (out, ok) = run(r#"def m = [foo:{'x'}, bar:{a -> a*2}]
println(m.foo())
println(m.bar(3))
println(m.size())
println([size: 9].size())
println([size: 9].size)
def g = [greet: { n -> "hi $n" }, count: 0]
println(g.greet('bo'))
println(g.count)"#);
    assert!(ok);
    assert_eq!(out, "x\n6\n2\n1\n9\nhi bo\n0\n");
}

/// Category `use (Cat) { … }` blocks. A category makes a class's methods
/// available on the type of their first parameter — `static int twice(Integer i)`
/// becomes `3.twice()` — for the duration of the block, and only where the
/// receiver has no such method of its own.
///
/// Every expectation is Apache Groovy 5.1.0's own output.
#[test]
fn a_use_block_makes_a_categorys_methods_available() {
    let (out, ok) = run(r#"class NumCat { static int twice(Integer i) { i * 2 } }
class StrCat { static String shout(String s) { s.toUpperCase() + '!' } }
use (NumCat) { println 3.twice() }
use (NumCat, StrCat) { println 4.twice(); println 'hi'.shout() }
use (NumCat) { println([1,2].collect { it.twice() }) }
try { 3.twice() } catch (e) { println e.getClass().getSimpleName() }
class ArgCat { static int plus2(Integer i, int n) { i + n } }
use (ArgCat) { println 5.plus2(3) }
println(use (NumCat) { 6.twice() })"#);
    assert!(ok);
    assert_eq!(out, "6\n8\nHI!\n[2, 4]\nMissingMethodException\n8\n12\n");
}

/// A trailing closure after a *function* call's parenthesised arguments —
/// `f(3) { it * 2 }`. A method call already took one; a function call did not,
/// which is also the shape `use (Cat) { … }` is written in.
#[test]
fn a_function_call_takes_a_trailing_closure() {
    let (out, ok) = run(r#"def f(a, Closure c) { c(a) }
println(f(3) { it * 2 })
def g(Closure c) { c() }
println(g() { 7 })
println([1,2,3].inject(0) { a, b -> a + b })"#);
    assert!(ok);
    assert_eq!(out, "6\n7\n6\n");
}

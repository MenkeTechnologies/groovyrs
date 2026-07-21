//! Integration tests: run `.groovy` scripts through the built `groovy` binary
//! and assert their stdout. Expected outputs are frozen after verifying them
//! byte-for-byte against Apache Groovy 5.0.x, so the suite is self-contained —
//! CI needs no JVM or Groovy install.

use std::process::Command;

/// Run a Groovy source string through the `groovy` binary and return
/// (stdout, ok).
fn run(src: &str) -> (String, bool) {
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
    // A dispatch miss must fault, not mis-run.
    let (_out, ok) = run("def s = \"hi\"\nprintln s.frobnicate()");
    assert!(!ok, "unknown method should fault");
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
    // A call through an undefined name (not a closure) remains an error.
    let (_out, ok) = run("println foo(1)");
    assert!(!ok, "calling an undefined non-closure must fault");
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
    let (_out, ok) = run("def x = new Nonexistent()");
    assert!(!ok, "constructing an unregistered class must fault");
}

#[test]
fn unknown_method_on_instance_faults() {
    let src = "class C { def v = 1 }\ndef c = new C()\nprintln c.nope()";
    let (_out, ok) = run(src);
    assert!(!ok, "an unknown method on an instance must fault");
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

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

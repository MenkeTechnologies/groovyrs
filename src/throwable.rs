//! The built-in throwable hierarchy groovyrs models.
//!
//! Groovy scripts throw and catch JDK types (`Exception`, `IllegalStateException`,
//! `IOException`, …) that have no source declaration in the script, so groovyrs
//! pre-registers them as ordinary classes in the host class registry
//! ([`crate::host`]). That reuses one mechanism for everything: `catch (T e)` is
//! the same superclass-chain walk `instanceof` already does, and a user
//! `class MyEx extends Exception` slots straight into the chain.
//!
//! Each entry is `(short name, direct supertype, package)`. The hierarchy and
//! every qualified name below is verified against Apache Groovy 5.0.7 by walking
//! `getClass().getSuperclass().getName()` on an instance of each type.

/// `(short name, direct supertype — `""` for the root, package)`.
const THROWABLES: &[(&str, &str, &str)] = &[
    ("Throwable", "", "java.lang"),
    ("Exception", "Throwable", "java.lang"),
    ("Error", "Throwable", "java.lang"),
    ("RuntimeException", "Exception", "java.lang"),
    ("IllegalArgumentException", "RuntimeException", "java.lang"),
    (
        "NumberFormatException",
        "IllegalArgumentException",
        "java.lang",
    ),
    ("IllegalStateException", "RuntimeException", "java.lang"),
    ("ArithmeticException", "RuntimeException", "java.lang"),
    ("NullPointerException", "RuntimeException", "java.lang"),
    ("IndexOutOfBoundsException", "RuntimeException", "java.lang"),
    (
        "ArrayIndexOutOfBoundsException",
        "IndexOutOfBoundsException",
        "java.lang",
    ),
    (
        "StringIndexOutOfBoundsException",
        "IndexOutOfBoundsException",
        "java.lang",
    ),
    (
        "UnsupportedOperationException",
        "RuntimeException",
        "java.lang",
    ),
    ("ClassCastException", "RuntimeException", "java.lang"),
    ("InterruptedException", "Exception", "java.lang"),
    ("CloneNotSupportedException", "Exception", "java.lang"),
    ("AssertionError", "Error", "java.lang"),
    ("IOException", "Exception", "java.io"),
    ("FileNotFoundException", "IOException", "java.io"),
    ("NoSuchElementException", "RuntimeException", "java.util"),
    (
        "ConcurrentModificationException",
        "RuntimeException",
        "java.util",
    ),
    ("GroovyRuntimeException", "RuntimeException", "groovy.lang"),
    (
        "MissingMethodException",
        "GroovyRuntimeException",
        "groovy.lang",
    ),
    (
        "MissingPropertyException",
        "GroovyRuntimeException",
        "groovy.lang",
    ),
    (
        "PowerAssertionError",
        "AssertionError",
        "org.codehaus.groovy.runtime.powerassert",
    ),
];

/// Every modeled throwable as `(name, supertype, package)`, root first — so a
/// registry that appends in this order always has a type's supertype already
/// present.
pub fn all() -> impl Iterator<Item = (&'static str, Option<&'static str>, &'static str)> {
    THROWABLES
        .iter()
        .map(|(n, s, p)| (*n, (!s.is_empty()).then_some(*s), *p))
}

/// Is `name` one of the modeled built-in throwables?
pub fn is_builtin(name: &str) -> bool {
    THROWABLES.iter().any(|(n, _, _)| *n == name)
}

/// The fully-qualified name a throwable prints as (`java.lang.Exception`). A
/// name that is not a built-in throwable — a user subclass — prints bare, which
/// is what Groovy does for a script-declared class.
pub fn qualified(name: &str) -> String {
    match THROWABLES.iter().find(|(n, _, _)| *n == name) {
        Some((n, _, pkg)) => format!("{pkg}.{n}"),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every supertype named in the table must itself be in the table, and must
    /// appear before its subtypes — the registry registration order depends on
    /// it, and a typo here would silently break `catch` matching.
    #[test]
    fn supertypes_are_declared_before_their_subtypes() {
        let mut seen: Vec<&str> = Vec::new();
        for (name, sup, _) in all() {
            if let Some(s) = sup {
                assert!(
                    seen.contains(&s),
                    "supertype `{s}` of `{name}` is missing or declared later"
                );
            }
            seen.push(name);
        }
    }

    #[test]
    fn qualified_names_match_the_jdk_packages() {
        assert_eq!(qualified("Exception"), "java.lang.Exception");
        assert_eq!(qualified("IOException"), "java.io.IOException");
        assert_eq!(
            qualified("NoSuchElementException"),
            "java.util.NoSuchElementException"
        );
        assert_eq!(
            qualified("GroovyRuntimeException"),
            "groovy.lang.GroovyRuntimeException"
        );
        // A user subclass keeps its bare script name (`MyEx: q` in Groovy).
        assert_eq!(qualified("MyEx"), "MyEx");
    }
}

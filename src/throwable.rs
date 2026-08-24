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
    // `java.util.Formatter`'s failure surface. Every one of these is an
    // `IllegalFormatException`, so a script's `catch (IllegalArgumentException
    // e)` around a `printf` catches all of them — verified on Apache Groovy
    // 5.1.0 by walking `getSuperclass()` from each.
    (
        "IllegalFormatException",
        "IllegalArgumentException",
        "java.util",
    ),
    (
        "IllegalFormatConversionException",
        "IllegalFormatException",
        "java.util",
    ),
    (
        "IllegalFormatCodePointException",
        "IllegalFormatException",
        "java.util",
    ),
    (
        "IllegalFormatPrecisionException",
        "IllegalFormatException",
        "java.util",
    ),
    (
        "MissingFormatArgumentException",
        "IllegalFormatException",
        "java.util",
    ),
    (
        "MissingFormatWidthException",
        "IllegalFormatException",
        "java.util",
    ),
    (
        "DuplicateFormatFlagsException",
        "IllegalFormatException",
        "java.util",
    ),
    (
        "UnknownFormatConversionException",
        "IllegalFormatException",
        "java.util",
    ),
    (
        "FormatFlagsConversionMismatchException",
        "IllegalFormatException",
        "java.util",
    ),
    // What a malformed (or unsupported) `~/…/` / `=~` pattern raises.
    (
        "PatternSyntaxException",
        "IllegalArgumentException",
        "java.util.regex",
    ),
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
    // What `value as Type` raises when no coercion to `Type` exists. Groovy's
    // own subclass of the JDK's `ClassCastException`, verified on 5.0.8 by
    // `GroovyCastException.class.getSuperclass().getName()`, so a script's
    // `catch (ClassCastException e)` catches a failed cast.
    (
        "GroovyCastException",
        "ClassCastException",
        "org.codehaus.groovy.runtime.typehandling",
    ),
    ("InterruptedException", "Exception", "java.lang"),
    ("CloneNotSupportedException", "Exception", "java.lang"),
    ("AssertionError", "Error", "java.lang"),
    // What runaway recursion raises. `StackOverflowError` is a
    // `VirtualMachineError`, not a plain `Error`, so `catch (VirtualMachineError
    // e)` catches it and `catch (Exception e)` does not — verified on 5.0.8 by
    // walking `StackOverflowError.class.getSuperclass()`:
    // `java.lang.StackOverflowError <- java.lang.VirtualMachineError <-
    //  java.lang.Error <- java.lang.Throwable`.
    ("VirtualMachineError", "Error", "java.lang"),
    ("StackOverflowError", "VirtualMachineError", "java.lang"),
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
        // The loop below is doubly vacuous on its own: it asserts nothing if
        // `all()` yields nothing, and its one assertion is further gated on the
        // supertype being `Some`. A `THROWABLES` whose supertype column got
        // blanked — `""` maps to `None` — would pass having compared zero pairs,
        // and that is exactly the "typo here would silently break `catch`
        // matching" bug this test exists to catch. Pin that there is a table and
        // that nearly all of it is parented before walking it.
        let entries: Vec<_> = all().collect();
        assert!(
            entries.len() >= 29,
            "only {} throwables in the table — it is truncated or `all()` \
             stopped yielding, so the ordering check below guards nothing",
            entries.len()
        );
        let parented = entries.iter().filter(|(_, sup, _)| sup.is_some()).count();
        assert_eq!(
            parented,
            entries.len() - 1,
            "exactly one throwable (`Throwable`) may be rootless; {} of {} have \
             no supertype, so the supertype column has lost entries",
            entries.len() - parented,
            entries.len()
        );

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
        // Groovy's own cast failure lives outside both `java.*` and
        // `groovy.lang` — a script that prints it sees the full path.
        assert_eq!(
            qualified("GroovyCastException"),
            "org.codehaus.groovy.runtime.typehandling.GroovyCastException"
        );
        // Runaway recursion's throwable, and the `VirtualMachineError` that sits
        // between it and `Error` — the reason `catch (Exception e)` does not
        // catch it.
        assert_eq!(
            qualified("StackOverflowError"),
            "java.lang.StackOverflowError"
        );
        assert_eq!(
            qualified("VirtualMachineError"),
            "java.lang.VirtualMachineError"
        );
        // A user subclass keeps its bare script name (`MyEx: q` in Groovy).
        assert_eq!(qualified("MyEx"), "MyEx");
    }
}

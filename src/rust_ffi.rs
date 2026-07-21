//! Groovy wiring for inline Rust FFI (`rust { ... }` blocks).
//!
//! The heavy lifting lives in fusevm: [`fusevm::RustSugar`] scans and rewrites
//! the block at the source level, and `fusevm::ffi` compiles / loads / marshals
//! it. This module only supplies the Groovy-flavored [`fusevm::RustSugar`] config
//! and the desugar entry the pipeline runs before lexing. The emitted
//! `__rust_compile(...)` call and every exported bareword are resolved in
//! [`crate::compiler`] (the `GFFI_COMPILE` / `GFFI_CALL` builtins) and executed
//! by [`crate::host`].
//!
//! A `rust { ... }` block appears as a top-level Groovy script statement (Groovy
//! synthesises the enclosing class/`main` itself), where the desugar replaces it
//! in place with a `__rust_compile("<base64>", <line>)` call statement.

use fusevm::RustSugar;

/// Emit the Groovy statement a `rust { ... }` block desugars to: a call to the
/// `__rust_compile` builtin carrying the base64-encoded block body and its line.
/// base64's alphabet (`A-Za-z0-9+/=`) has no `$`, so it needs no escaping inside
/// the double-quoted Groovy string literal (no GString interpolation is
/// triggered).
fn emit(b64: &str, line: usize) -> String {
    format!("__rust_compile(\"{b64}\", {line})")
}

/// Groovy desugar config. Groovy line comments are `//`, block comments `/* */`.
/// `newline_boundary` is `true` because a `.groovy` script is line-oriented at
/// the top level (statements need no trailing `;`), so a `rust { ... }` block
/// that starts a statement line is recognized; `{`/`}`/`;` are boundaries too.
pub const SUGAR: RustSugar = RustSugar {
    keyword: "rust",
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    newline_boundary: true,
    emit,
};

/// Rewrite every `rust { ... }` block in Groovy source into a `__rust_compile(...)`
/// call, before lexing. No-op when the source has no `rust` token.
pub fn desugar(src: &str) -> String {
    SUGAR.desugar(src)
}

#[cfg(test)]
mod tests {
    #[test]
    fn desugars_top_level_block() {
        let src = "rust { pub extern \"C\" fn add(a: i64, b: i64) -> i64 { a + b } }\nprintln(add(2, 3))\n";
        let out = super::desugar(src);
        assert!(out.contains("__rust_compile("), "no builtin call: {out}");
        assert!(!out.contains("pub extern"), "Rust body leaked: {out}");
        assert!(out.contains("println(add(2, 3))"), "trailing code lost: {out}");
    }

    #[test]
    fn leaves_ordinary_groovy_untouched() {
        let src = "def x = 41 + 1\nprintln(x)\n";
        assert_eq!(super::desugar(src), src);
    }
}

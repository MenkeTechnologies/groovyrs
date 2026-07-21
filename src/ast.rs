//! The Groovy AST groovyrs parses and lowers to fusevm bytecode.
//!
//! Slice 1 targets the Groovy *script* model: a `.groovy` file is a sequence of
//! top-level statements (no enclosing class or `main` — Groovy synthesises those
//! itself). The subset covers `def`/typed local declarations, script-binding
//! assignments, arithmetic / comparison / logic expressions, `if`/`while`, the
//! C-style and `for (x in a..b)` range loops, `break`/`continue`, and the
//! `println`/`print` command calls. Classes, methods, closures, GStrings, and
//! the GDK are parsed no further today (see `BUGS.md`); the AST is shaped to
//! grow into them.

/// A parsed script: the ordered top-level statements.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    /// The statements of the script body, run top to bottom.
    pub body: Vec<Stmt>,
}

/// A Groovy statement with its 1-based source line.
///
/// The line is what `--dap` reports in stack frames and what breakpoints match
/// against: the debug compiler emits a `DBG_LINE` marker carrying `line` before
/// each statement (see `compiler::compile_debug`). Normal (non-debug) runs carry
/// the line as ordinary bytecode line metadata and emit no markers.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    /// 1-based source line the statement begins on.
    pub line: u32,
    /// The statement itself.
    pub kind: StmtKind,
}

impl Stmt {
    /// Wrap a [`StmtKind`] with its source line.
    pub fn new(line: u32, kind: StmtKind) -> Self {
        Stmt { line, kind }
    }
}

/// A Groovy statement kind (the payload of [`Stmt`]).
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    /// A local declaration: `def x = expr`, `int x = expr`, `String s`. The
    /// declared type (`"def"` for `def`) is retained for diagnostics; the
    /// runtime is dynamically typed on the fusevm value model, so it does not
    /// gate execution.
    Local {
        ty: String,
        name: String,
        init: Option<Expr>,
    },
    /// An assignment to a variable: `x = expr`, `x += expr`. A bare `x = …`
    /// with no prior declaration creates a script-binding variable, matching
    /// Groovy.
    Assign {
        name: String,
        op: AssignOp,
        value: Expr,
    },
    /// An expression evaluated for its side effects: `println(x)`.
    Expr(Expr),
    /// `if (cond) { .. } else { .. }`.
    If {
        cond: Expr,
        then: Vec<Stmt>,
        els: Vec<Stmt>,
    },
    /// `while (cond) { .. }`.
    While { cond: Expr, body: Vec<Stmt> },
    /// `for (init; cond; update) { .. }` — the C-style loop. The `for (x in
    /// a..b)` range form is desugared to this by the parser.
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        update: Option<Box<Stmt>>,
        body: Vec<Stmt>,
    },
    /// `break`
    Break,
    /// `continue`
    Continue,
}

/// Compound-assignment operator. `Assign` is a plain `=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

/// A Groovy expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    /// The `null` literal.
    Null,
    /// A bare identifier — a variable read.
    Var(String),
    /// A unary operator applied to one operand (`-x`, `!b`).
    Unary {
        op: UnOp,
        rhs: Box<Expr>,
    },
    /// A binary operator applied to two operands.
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `println(arg)` / `print(arg)` — the top-level Groovy print commands,
    /// accepted with or without parentheses. Modeled directly (rather than as a
    /// general method call) until user methods land.
    Println {
        newline: bool,
        arg: Option<Box<Expr>>,
    },
    /// Post-increment / post-decrement of a variable (`i++`, `i--`), evaluated
    /// as a statement today. The bool is `true` for `++`.
    PostIncDec {
        name: String,
        inc: bool,
    },
    /// A general call expression `name(args...)`. Slice 1 has no user methods, so
    /// the only calls that resolve are the inline-Rust FFI ones: the desugar
    /// target `__rust_compile("<b64>", line)` and every bareword a `rust { ... }`
    /// block exports (`add(2, 3)`). The compiler routes an unknown callee through
    /// the FFI dispatch only when the program contains a `rust { ... }` block;
    /// otherwise it stays an unresolved-reference error.
    Call {
        name: String,
        args: Vec<Expr>,
        line: u32,
    },
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}

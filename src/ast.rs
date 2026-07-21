//! The Groovy AST groovyrs parses and lowers to fusevm bytecode.
//!
//! groovyrs targets the Groovy *script* model: a `.groovy` file is a sequence of
//! top-level statements (no enclosing class or `main` — Groovy synthesises those
//! itself). The subset covers `def`/typed local declarations and functions,
//! script-binding assignments, arithmetic / comparison / logic expressions,
//! ternary / Elvis / safe-navigation, closures (`{ a, b -> … }` / implicit
//! `{ it }`) with the closure-driven GDK and nested-closure upvalue capture,
//! first-class ranges, `if`/`while`, the C-style and `for (x in a..b)` range
//! loops, `break`/`continue`, subscripting (`recv[i]`), the `println`/`print`
//! command calls, and classes (fields, constructors, methods, `this`, property
//! get/set with auto getter/setter, `new`). GStrings are not modeled yet (see
//! `BUGS.md`); the AST is shaped to grow into them.

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
    /// `return` / `return <expr>`. Inside a user function the value is carried
    /// out through `Op::ReturnValue` (a bare `return` returns `null`); at script
    /// top level the value becomes the script's result and execution ends.
    Return { value: Option<Expr> },
    /// A user-defined function: `def name(a, b) { .. }` (or a typed
    /// `Type name(..) { .. }`). Lowered to a fusevm subroutine chunk region with
    /// the call-frame ABI; parameters and locals live in frame slots so recursion
    /// is sound.
    Function {
        name: String,
        params: Vec<String>,
        body: Vec<Stmt>,
    },
    /// A class declaration: `class C { fields; C(..){..}; def m(){..} }`. Fields,
    /// constructors, and methods are hoisted like functions and lowered to
    /// subroutine regions (methods take an implicit leading `this`). See
    /// `compiler::class_def`.
    Class {
        name: String,
        /// The direct superclass name (`class C extends B`), or `None` for a root
        /// class. `implements` clauses are still ignored (dynamic dispatch).
        superclass: Option<String>,
        fields: Vec<Field>,
        ctors: Vec<Ctor>,
        methods: Vec<Method>,
    },
    /// A property assignment to a receiver: `recv.name = value` (e.g. `p.x = 10`
    /// or `this.v = x`). Routes through the host property-set builtin, honouring a
    /// user `set<Name>` setter and Groovy's auto-setter on a field.
    SetProperty {
        recv: Expr,
        name: String,
        value: Expr,
    },
}

/// A class field: `def x` / `Type x [= init]`. The declared type is ignored at
/// runtime (dynamic typing); an absent initializer defaults to `null`.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub init: Option<Expr>,
}

/// A class constructor `C(params) { body }`. Overloads are distinguished by
/// arity at `new` time (Groovy also dispatches constructors by arity here).
#[derive(Debug, Clone, PartialEq)]
pub struct Ctor {
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
}

/// A class method `def m(params) { body }` (or typed). Lowered like a function
/// but with an implicit leading `this` slot; a bare field name in the body reads
/// / writes `this.field`.
#[derive(Debug, Clone, PartialEq)]
pub struct Method {
    pub name: String,
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
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
    /// Post-increment / post-decrement of a variable (`i++`, `i--`). As an
    /// expression it evaluates to the value *before* the update; as a statement
    /// the result is discarded. The bool is `true` for `++`.
    PostIncDec {
        name: String,
        inc: bool,
    },
    /// Pre-increment / pre-decrement of a variable (`++i`, `--i`). Evaluates to
    /// the value *after* the update. The bool is `true` for `++`.
    PreIncDec {
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
    /// Invoke the value produced by an arbitrary expression: `callee(args...)`.
    /// This is the postfix call-application that makes chained calls parse —
    /// `f(a)(b)` is `CallValue { callee: Call(f, [a]), args: [b] }` — and lets a
    /// method result or a bracketed closure be invoked directly. The callee must
    /// evaluate to a closure handle at runtime; otherwise the call faults.
    CallValue {
        callee: Box<Expr>,
        args: Vec<Expr>,
        line: u32,
    },
    /// A list literal `[a, b, c]` (or `[]`). Lowered to `Op::MakeArray`.
    List(Vec<Expr>),
    /// A map literal `[k: v, ...]` (or the empty map `[:]`). Each key is an
    /// expression — a bare identifier key is a string constant (Groovy treats
    /// `[a: 1]` as key `"a"`; use `[(expr): v]` for a computed key). Lowered to
    /// `Op::MakeHash`.
    Map(Vec<(Expr, Expr)>),
    /// A method call on a receiver: `recv.method(args...)`. Routed through the
    /// host GDK dispatch builtin (`crate::host::GMETHOD`). `safe` is `true` for
    /// the safe-navigation form `recv?.method(args)`, which yields `null`
    /// (without dispatching) when the receiver is `null`.
    MethodCall {
        recv: Box<Expr>,
        method: String,
        args: Vec<Expr>,
        line: u32,
        safe: bool,
    },
    /// A property read on a receiver: `recv.name` (e.g. `list.size`,
    /// `str.length`). Routed through the host property builtin
    /// (`crate::host::GPROP`). `safe` is `true` for `recv?.name`, which yields
    /// `null` when the receiver is `null` rather than faulting.
    Property {
        recv: Box<Expr>,
        name: String,
        line: u32,
        safe: bool,
    },
    /// A closure literal `{ a, b -> body }` or the implicit-`it` form
    /// `{ body }`. A first-class callable value: it lowers to a subroutine
    /// region plus a runtime closure handle (`Value::Obj`), invoked through the
    /// existing `Op::Call` frame ABI via the host closure dispatch. Non-parameter
    /// names resolve to the enclosing script bindings (globals), so a closure
    /// captures the script scope it was defined in.
    Closure {
        params: Vec<String>,
        body: Vec<Stmt>,
    },
    /// A first-class integer range `start..end` (inclusive) or `start..<end`
    /// (half-open). Materialised to a Groovy list of the enumerated integers, so
    /// `.size()`, `.contains(x)`, `.each`, and `.collect` all apply.
    Range {
        start: Box<Expr>,
        end: Box<Expr>,
        inclusive: bool,
    },
    /// The ternary conditional `cond ? then : els`. `cond` uses Groovy
    /// truthiness (0/""/empty/null are false).
    Ternary {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    /// The Elvis / null-coalescing operator `lhs ?: rhs`: `lhs` when it is
    /// Groovy-truthy, else `rhs`.
    Elvis {
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// Object construction `new C(args...)`. Allocates a host-heap instance,
    /// runs field initializers, then the arity-matched constructor; yields the
    /// instance handle (`Value::Obj`).
    New {
        class: String,
        args: Vec<Expr>,
        line: u32,
    },
    /// The `this` reference inside a method or constructor body — the receiver
    /// instance, held in frame slot 0.
    This,
    /// The `super` reference inside a method — a receiver for `super.m(args)`,
    /// which statically dispatches `m` starting at the superclass (skipping the
    /// current class's override). Only meaningful as a `MethodCall` receiver.
    Super,
    /// A super-constructor call `super(args)` in a constructor body: runs the
    /// superclass's arity-matched constructor against the current instance.
    SuperCtor {
        args: Vec<Expr>,
        line: u32,
    },
    /// A type test `value instanceof Class` — true when `value`'s class is `Class`
    /// or a subclass, or matches a built-in type name. Yields a `Boolean`.
    InstanceOf {
        value: Box<Expr>,
        class: String,
    },
    /// An index read `recv[index]` — Groovy's subscript operator, dispatched to a
    /// list/map/string element or a user `getAt(index)` overload.
    Index {
        recv: Box<Expr>,
        index: Box<Expr>,
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
    /// `<=>` — three-way compare. On a user-class instance it dispatches
    /// `compareTo`; on primitives it yields the sign (`-1`/`0`/`1`).
    Cmp,
    And,
    Or,
}

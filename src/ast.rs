//! The Groovy AST groovyrs parses and lowers to fusevm bytecode.
//!
//! groovyrs targets the Groovy *script* model: a `.groovy` file is a sequence of
//! top-level statements (no enclosing class or `main` — Groovy synthesises those
//! itself). The subset covers `def`/typed local declarations and functions,
//! script-binding assignments, arithmetic / comparison / logic expressions,
//! ternary / Elvis / safe-navigation, closures (`{ a, b -> … }` / `{ -> … }` /
//! implicit `{ it }`) with the closure-driven GDK and nested-closure upvalue
//! capture, first-class ranges, `GString` interpolation, `if`/`while`, the
//! C-style and `for (x in a..b)` range loops, `break`/`continue`,
//! `try`/`catch`/`finally`/`throw`, subscripting (`recv[i]`), the
//! `println`/`print` command calls, and classes (fields, constructors, methods,
//! `this`, property get/set with auto getter/setter, `new`). See `BUGS.md` for
//! what is still missing.

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
    /// A subscript assignment `recv[index] = value` — Groovy's `putAt`. Held
    /// apart from [`StmtKind::Assign`] because the receiver is an expression and
    /// a list receiver has to be written back (a fusevm list is a value).
    SetIndex {
        recv: Expr,
        index: Expr,
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
    /// `do { .. } while (cond)` — the body runs before the first test, so it
    /// always executes at least once.
    DoWhile { body: Vec<Stmt>, cond: Expr },
    /// `switch (subject) { case L: .. default: .. }`. The cases keep source
    /// order and fall through into one another exactly as Groovy's do; a `break`
    /// leaves the switch. Each `case` label is matched with Groovy's `isCase`
    /// rules, not `==` (see `host::GIS_CASE`).
    Switch {
        subject: Expr,
        cases: Vec<SwitchCase>,
    },
    /// `label: <loop or switch>` — the target a labeled `break`/`continue` names.
    Labeled { label: String, stmt: Box<Stmt> },
    /// `for (init; cond; update) { .. }` — the C-style loop. The `for (x in
    /// a..b)` range form is desugared to this by the parser.
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        update: Option<Box<Stmt>>,
        body: Vec<Stmt>,
    },
    /// `break` / `break label` — leaves the innermost enclosing loop or
    /// `switch`, or the one carrying `label`.
    Break(Option<String>),
    /// `continue` / `continue label` — re-tests the innermost enclosing loop, or
    /// the one carrying `label`. A `switch` is transparent here: a `continue`
    /// inside one continues the loop around it.
    Continue(Option<String>),
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
        /// class.
        superclass: Option<String>,
        /// The names in an `implements A, B` clause — or, for an `interface`, the
        /// names in its own (multiple-inheritance) `extends A, B`. They make
        /// `instanceof` answer and supply `default` method bodies.
        interfaces: Vec<String>,
        /// True when this declaration is an `interface` rather than a `class`.
        /// An interface cannot be instantiated; its method declarations with a
        /// body are Java 8 `default` methods, inherited by every implementor.
        is_interface: bool,
        fields: Vec<Field>,
        ctors: Vec<Ctor>,
        methods: Vec<Method>,
        /// Names declared without a body — an interface's abstract methods. They
        /// bind nothing at runtime (the implementing class supplies the body),
        /// but a bare call to one inside a sibling `default` method must still
        /// compile to `this.m()`, which is what this list drives.
        abstract_methods: Vec<String>,
    },
    /// A property assignment to a receiver: `recv.name = value` (e.g. `p.x = 10`
    /// or `this.v = x`). Routes through the host property-set builtin, honouring a
    /// user `set<Name>` setter and Groovy's auto-setter on a field.
    SetProperty {
        recv: Expr,
        name: String,
        value: Expr,
    },
    /// `try { … } catch (T e) { … }* [finally { … }]`. At least one `catch` or a
    /// `finally` is required (Groovy rejects a bare `try`). See
    /// `compiler::try_stmt` for the lowering — fusevm has no unwind opcode, so an
    /// in-flight exception is a host-side pending value plus compiler-emitted
    /// jumps to the innermost handler.
    Try {
        body: Vec<Stmt>,
        catches: Vec<CatchArm>,
        finally_body: Vec<Stmt>,
    },
    /// `throw <expr>` — park the throwable as the pending exception and unwind to
    /// the innermost enclosing handler (or out of the program).
    Throw(Expr),
    /// `assert cond [: message]`. `text` is the condition's verbatim source,
    /// which Groovy's power-assert rendering prints above the recorded values;
    /// the sub-expressions whose values it records are wrapped in
    /// [`Expr::Recorded`] by the parser.
    Assert {
        cond: Expr,
        message: Option<Expr>,
        text: String,
        /// The condition re-rendered the way Groovy's `Expression.getText()`
        /// does — fully parenthesised, with implicit receivers and qualified
        /// type names. The `assert cond : message` form quotes *this*, not the
        /// source text `text` holds (see `parser::expr_text`).
        ast_text: String,
        /// The `(name, column)` of each bare-variable operand Groovy lists in
        /// the *message* form's `Values:` clause, in source order. The column
        /// is what pairs the name with its recorded value — a name can occur
        /// more than once, and one name can be a prefix of another. Empty when
        /// the condition has no bare-variable operand, which is when Groovy
        /// omits the clause.
        value_names: Vec<(String, u32)>,
    },
}

/// One `case L:` / `default:` section of a [`StmtKind::Switch`], in source
/// order. `label` is `None` for `default`. An empty `body` is how consecutive
/// labels (`case 2: case 3: …`) fall into a shared body.
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchCase {
    pub label: Option<Expr>,
    pub body: Vec<Stmt>,
}

/// One `catch (T e) { … }` arm. `types` holds the caught type names — more than
/// one for Java/Groovy's multi-catch `catch (A | B e)`; an untyped `catch (e)`
/// records `Exception`, which is what Groovy means by it.
#[derive(Debug, Clone, PartialEq)]
pub struct CatchArm {
    pub types: Vec<String>,
    pub name: String,
    pub body: Vec<Stmt>,
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

/// Which Java integer type an integer literal has.
///
/// Groovy decides this from the literal alone: an unsuffixed literal is an
/// `Integer` when its value fits in 32 bits and a `Long` when it does not, and
/// an `L`/`l` suffix makes it a `Long` regardless. The distinction is not
/// cosmetic — it is the width the arithmetic wraps at, so `1000000 * 1000000`
/// is the `Integer` `-727379968` while `1000000L * 1000000` is `1000000000000`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntWidth {
    /// `java.lang.Integer` — arithmetic wraps at 32 bits.
    Int,
    /// `java.lang.Long` — arithmetic wraps at 64 bits.
    Long,
}

/// A Groovy expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// An integer literal and the Java width it carries (see [`IntWidth`]).
    Int(i64, IntWidth),
    /// A `d`/`f`-suffixed decimal literal: an IEEE double.
    Float(f64),
    /// An unsuffixed decimal literal — a `java.math.BigDecimal`, held as its
    /// exact source text (see [`crate::decimal`]).
    Dec(String),
    Str(String),
    /// An interpolating string — Groovy's `GString`. The parts render in order
    /// and concatenate; an embedded object renders through its `toString()`,
    /// which is why this is not simply lowered to `+`.
    GString(Vec<GStringPart>),
    Bool(bool),
    /// A `~/pattern/` literal — Groovy's `java.util.regex.Pattern`. Modeled far
    /// enough to drive a `switch` `case ~/…/:` label, whose `isCase` is a *full*
    /// match of the subject's string form.
    Regex(String),
    /// The `null` literal.
    Null,
    /// A bare identifier — a variable read.
    Var(String),
    /// A sub-expression of an `assert` condition whose value the power-assert
    /// renderer records, tagged with the 1-based source column Groovy prints it
    /// under. Produced only inside an `assert`, so no other program pays for it.
    Recorded {
        col: u32,
        inner: Box<Expr>,
    },
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
        /// True when the literal wrote an explicit parameter list — including
        /// the empty one, `{ -> … }`, which takes no arguments at all. Without a
        /// list (`{ it * 2 }`) Groovy supplies the single implicit parameter
        /// `it`, which is what `false` selects.
        explicit_params: bool,
    },
    /// The sequence a `for (x in <expr>)` walks — the parser wraps the loop's
    /// subject in this, and it lowers to the host's iteration builtin
    /// (`crate::host::GITER`), which yields a list's elements, a map's entries,
    /// a `String`'s characters, nothing for `null`, and any other value once.
    Iterable(Box<Expr>),
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
    /// A coercion `value as Type` — Groovy's `asType`. The right side is a type
    /// *name*, not an expression, so it is held as text.
    Cast {
        value: Box<Expr>,
        ty: String,
    },
}

/// One piece of a [`Expr::GString`]: literal text, or an embedded expression.
#[derive(Debug, Clone, PartialEq)]
pub enum GStringPart {
    Text(String),
    Expr(Expr),
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    /// `~` — bitwise complement.
    BitNot,
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
    /// `**` — Groovy's power operator (right-associative).
    Power,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    /// `>>>` — unsigned (zero-filling) right shift.
    UShr,
    /// `x in coll` — membership, which Groovy routes through `isCase`.
    In,
}

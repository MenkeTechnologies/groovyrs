//! A recursive-descent parser with precedence-climbing for expressions.
//!
//! Grammar: a `.groovy` file is a sequence of top-level statements — the Groovy
//! *script* model, with no enclosing class or `main`. Statements are separated
//! by newlines or `;` (both optional-semicolon and explicit forms). Covered:
//! `def`/typed local declarations, script-binding assignments, functions and
//! classes, `if`/`while`, the C-style `for (;;)` and the `for (x in a..b)` range
//! loop, `break`/`continue`, `try`/`catch`/`finally`/`throw`, closures, and the
//! `println`/`print` command calls (with or without parentheses).
//!
//! `try`, `catch`, `finally`, `throw`, `class`, `extends`, and `instanceof` are
//! *contextual* keywords: the lexer emits them as identifiers and the parser
//! recognises them by position, so a program that uses one as a variable name
//! only breaks where the construct is actually ambiguous.

use crate::ast::*;
use crate::lexer::{GPart, Tok, Token};

/// How deeply source may nest before groovyrs refuses to parse it.
///
/// The parser is recursive descent and the compiler walks the AST recursively,
/// so nesting depth in the source is Rust stack depth twice over — and a third
/// time when the tree drops. Past this, the program is a compile error rather
/// than a `fatal runtime error: stack overflow`, which aborts with no
/// diagnostic and no readable exit status.
///
/// Apache Groovy's own parser gives out well below this (measured on 5.0.8 /
/// JVM 21.0.12: 500 nested parentheses compile and 1000 do not; a 1000-term `+`
/// chain compiles and a 2000-term one does not), so nothing the reference
/// accepts is refused here. `crate::INTERPRETER_STACK_BYTES` is what makes a
/// limit this high servable.
pub const MAX_NESTING: usize = 5000;

/// Parse Groovy `src` into a [`Program`].
///
/// Any inline `rust { ... }` FFI block is rewritten to a `__rust_compile(...)`
/// call by [`crate::rust_ffi::desugar`] before lexing (a no-op when the source
/// has no `rust` token), so the lexer/parser only ever see ordinary Groovy.
pub fn parse(src: &str) -> Result<Program, String> {
    let src = crate::rust_ffi::desugar(src);
    let tokens = crate::lexer::lex(&src)?;
    let mut p = Parser {
        toks: tokens,
        src,
        pos: 0,
        tmp: 0,
        recording: None,
        pending: Vec::new(),
        depth: 0,
    };
    p.program()
}

struct Parser {
    toks: Vec<Token>,
    /// The source the tokens came from, so an `assert` can slice its condition's
    /// verbatim text and derive the columns its values render under.
    src: String,
    pos: usize,
    /// Counter for synthetic temporaries (e.g. `for-in` range endpoints).
    tmp: usize,
    /// While parsing an `assert` condition, the column of its `assert` keyword —
    /// recorded columns are rebased onto it so the rendering reads as if the
    /// statement began the line, which is what Groovy prints. `None` everywhere
    /// else, so no other program pays for recording.
    recording: Option<u32>,
    /// Statements a single source statement expanded into beyond the first —
    /// a multi-declarator `def a = 1, b = 2` or a destructuring `def (a, b) = l`.
    /// Drained by every statement-list site through [`Parser::statements`].
    pending: Vec<Stmt>,
    /// How deep the AST being built is at this point, bounded by
    /// [`MAX_NESTING`]. Maintained at the three places source nesting becomes
    /// tree depth: [`Parser::unary`] (every parenthesised, bracketed, braced or
    /// prefixed expression funnels through it), [`Parser::block`] (statement
    /// nesting), and [`Parser::binary_from`]'s fold (a chain of `n` operators is
    /// an AST `n` deep).
    depth: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].kind
    }

    fn peek_at(&self, n: usize) -> &Tok {
        self.toks
            .get(self.pos + n)
            .map(|t| &t.kind)
            .unwrap_or(&Tok::Eof)
    }

    /// The 1-based source column of the token `n` ahead — characters since the
    /// last newline, which is the column Groovy's power assert renders under.
    fn col_at(&self, n: usize) -> u32 {
        let offset = self
            .toks
            .get(self.pos + n)
            .map_or(self.src.len(), |t| t.offset);
        let line_start = self.src[..offset].rfind('\n').map_or(0, |i| i + 1);
        1 + self.src[line_start..offset].chars().count() as u32
    }

    /// Wrap `e` for the power-assert value recorder when inside an `assert`
    /// condition; a no-op otherwise.
    fn record(&self, col: u32, e: Expr) -> Expr {
        match self.recording {
            Some(base) if col >= base => Expr::Recorded {
                col: col - base + 1,
                inner: Box::new(e),
            },
            _ => e,
        }
    }

    fn line(&self) -> u32 {
        self.toks[self.pos].line
    }

    fn advance(&mut self) -> Tok {
        let t = self.toks[self.pos].kind.clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, want: &Tok) -> Result<(), String> {
        if std::mem::discriminant(self.peek()) == std::mem::discriminant(want) {
            self.advance();
            Ok(())
        } else {
            Err(format!(
                "groovyrs: expected {want} but found {} on line {}",
                self.peek(),
                self.line()
            ))
        }
    }

    fn is(&self, t: &Tok) -> bool {
        std::mem::discriminant(self.peek()) == std::mem::discriminant(t)
    }

    /// Consume any run of statement terminators (`Nl`/`;`). Returns how many.
    fn skip_terminators(&mut self) -> usize {
        let mut n = 0;
        while matches!(self.peek(), Tok::Nl | Tok::Semi) {
            self.advance();
            n += 1;
        }
        n
    }

    /// Skip newlines only (used to allow line-continuation after an operator or
    /// an opening delimiter).
    fn skip_newlines(&mut self) {
        while matches!(self.peek(), Tok::Nl) {
            self.advance();
        }
    }

    fn fresh_tmp(&mut self, tag: &str) -> String {
        let n = self.tmp;
        self.tmp += 1;
        format!("$g_{tag}_{n}")
    }

    /// Parse the whole script: top-level statements until EOF.
    fn program(&mut self) -> Result<Program, String> {
        let mut body = Vec::new();
        self.skip_terminators();
        // Tolerate leading `package`/`import` lines (skipped to a terminator).
        loop {
            match self.peek() {
                Tok::Ident(w) if w == "package" || w == "import" => {
                    while !matches!(self.peek(), Tok::Nl | Tok::Semi | Tok::Eof) {
                        self.advance();
                    }
                    self.skip_terminators();
                }
                _ => break,
            }
        }
        while !self.is(&Tok::Eof) {
            body.extend(self.statements()?);
            self.expect_terminator()?;
            self.skip_terminators();
        }
        Ok(Program { body })
    }

    /// After a statement, require a terminator (`Nl`/`;`) or the end of a block
    /// (`}`) or file. This rejects two statements run together on one line
    /// without a separator.
    fn expect_terminator(&mut self) -> Result<(), String> {
        if matches!(self.peek(), Tok::Nl | Tok::Semi | Tok::RBrace | Tok::Eof) {
            Ok(())
        } else {
            Err(format!(
                "groovyrs: expected end of statement but found {} on line {}",
                self.peek(),
                self.line()
            ))
        }
    }

    /// Parse a `{ ... }` body already past the opening brace; consumes the `}`.
    fn block(&mut self) -> Result<Vec<Stmt>, String> {
        self.deepen()?;
        let r = self.block_inner();
        self.depth -= 1;
        r
    }

    fn block_inner(&mut self) -> Result<Vec<Stmt>, String> {
        let mut out = Vec::new();
        self.skip_terminators();
        while !self.is(&Tok::RBrace) && !self.is(&Tok::Eof) {
            out.extend(self.statements()?);
            self.expect_terminator()?;
            self.skip_terminators();
        }
        self.eat(&Tok::RBrace)?;
        Ok(out)
    }

    /// Parse a `{ ... }` or a single statement into a statement list.
    fn braced_or_single(&mut self) -> Result<Vec<Stmt>, String> {
        self.skip_newlines();
        if self.is(&Tok::LBrace) {
            self.advance();
            self.block()
        } else {
            self.statements()
        }
    }

    /// One source statement, plus anything it expanded into (`def a = 1, b = 2`
    /// is three AST statements, `def (a, b) = l` is three).
    fn statements(&mut self) -> Result<Vec<Stmt>, String> {
        let first = self.statement()?;
        let mut out = vec![first];
        out.append(&mut self.pending);
        Ok(out)
    }

    fn statement(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        let kind = match self.peek() {
            // `class`/`interface`, optionally behind modifiers
            // (`abstract class C`, `public final class C`).
            Tok::Ident(_) if self.type_decl_ahead().is_some() => {
                let is_interface = self.type_decl_ahead() == Some(true);
                while !matches!(self.peek(), Tok::Ident(w) if w == "class" || w == "interface") {
                    self.advance();
                }
                self.class_decl(is_interface)?
            }
            // `try`/`throw` are contextual keywords (the lexer emits them as
            // identifiers); a `try` is only a statement when a block follows.
            Tok::Ident(w) if w == "try" && matches!(self.peek_at(1), Tok::LBrace) => {
                self.try_stmt()?
            }
            Tok::Ident(w) if w == "throw" => {
                self.advance();
                self.skip_newlines();
                StmtKind::Throw(self.expression()?)
            }
            Tok::Assert => self.assert_stmt()?,
            Tok::If => self.if_stmt()?,
            Tok::While => self.while_stmt()?,
            Tok::Do => self.do_while_stmt()?,
            Tok::Switch => self.switch_stmt()?,
            Tok::For => self.for_stmt()?,
            // `label: <loop or switch>` — the only place a bare `ident :` can
            // start a statement, so no other construct is shadowed.
            Tok::Ident(_) if self.starts_a_label() => {
                let label = self.ident()?;
                self.eat(&Tok::Colon)?;
                self.skip_newlines();
                StmtKind::Labeled {
                    label,
                    stmt: Box::new(self.statement()?),
                }
            }
            Tok::Return => {
                // `return` / `return <expr>`: the value is carried out (see
                // `StmtKind::Return`). A bare `return` at end of line returns null.
                self.advance();
                let value = if matches!(self.peek(), Tok::Nl | Tok::Semi | Tok::RBrace | Tok::Eof) {
                    None
                } else {
                    Some(self.expression()?)
                };
                StmtKind::Return { value }
            }
            Tok::Break => {
                self.advance();
                StmtKind::Break(self.opt_jump_label())
            }
            Tok::Continue => {
                self.advance();
                StmtKind::Continue(self.opt_jump_label())
            }
            Tok::LBrace => {
                // A statement-position `{ params -> ... }` is a closure literal
                // (e.g. the closure a closure returns), not a block — route it
                // through the expression path.
                if self.stmt_lbrace_is_closure() {
                    return self.simple_statement();
                }
                // A bare block: flatten into an always-true `if`. Slice 1 has no
                // lexical scopes, so inlining is behavior-preserving.
                self.advance();
                let body = self.block()?;
                StmtKind::If {
                    cond: Expr::Bool(true),
                    then: body,
                    els: vec![],
                }
            }
            // A simple statement already carries its own line — return directly.
            _ => return self.simple_statement(),
        };
        Ok(Stmt::new(line, kind))
    }

    /// Local decl, assignment, or expression statement, wrapped with its line.
    fn simple_statement(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        Ok(Stmt::new(line, self.simple_statement_kind()?))
    }

    /// The kind of a simple statement (local decl / assignment / expression).
    fn simple_statement_kind(&mut self) -> Result<StmtKind, String> {
        let line = self.line();
        // `println`/`print` command statements are expression statements, not
        // declarations — resolve them before the two-idents-in-a-row heuristic.
        if matches!(self.peek(), Tok::Ident(n) if n == "println" || n == "print") {
            let e = self.expression()?;
            return Ok(StmtKind::Expr(e));
        }

        // `def name(params) { .. }` (a function) or `def name [= expr]` (a local).
        if self.is(&Tok::Def) {
            self.advance();
            // `def (a, b) = expr` — multiple assignment. The right side is
            // evaluated once into a temporary, then each name takes its element.
            if self.is(&Tok::LParen) {
                return self.destructuring_decl(line);
            }
            let name = self.ident()?;
            if self.is(&Tok::LParen) {
                return self.function_def(name);
            }
            let init = self.opt_initializer()?;
            // `def a = 1, b = 2` — the declarators after the first become their
            // own statements, queued for the enclosing statement list.
            while self.is(&Tok::Comma) {
                self.advance();
                self.skip_newlines();
                let n = self.ident()?;
                let i = self.opt_initializer()?;
                self.pending.push(Stmt::new(
                    line,
                    StmtKind::Local {
                        ty: "def".into(),
                        name: n,
                        init: i,
                    },
                ));
            }
            return Ok(StmtKind::Local {
                ty: "def".into(),
                name,
                init,
            });
        }

        // Typed declaration `Type name [= expr]` or typed function
        // `Type name(params) { .. }` (two identifiers in a row).
        if self.looks_like_decl() {
            let ty = self.ident()?;
            let name = self.ident()?;
            if self.is(&Tok::LParen) {
                return self.function_def(name);
            }
            let init = self.opt_initializer()?;
            return Ok(StmtKind::Local { ty, name, init });
        }

        // Assignment / post-inc-dec / expression statement.
        if let Tok::Ident(name) = self.peek().clone() {
            let next = self.peek_at(1);
            if let Some(op) = assign_op(next) {
                self.advance(); // name
                self.advance(); // op
                self.skip_newlines();
                let value = self.expression()?;
                return Ok(StmtKind::Assign { name, op, value });
            }
            // `x ?= v` is `x = x ?: v`: the write happens only when `x` is
            // falsy, so it is Groovy *truth* that decides, not just a null test
            // (`def n = 0; n ?= 5` leaves `5`).
            if matches!(next, Tok::ElvisAssign) {
                self.advance(); // name
                self.advance(); // ?=
                self.skip_newlines();
                let value = self.expression()?;
                return Ok(StmtKind::Assign {
                    name: name.clone(),
                    op: AssignOp::Assign,
                    value: Expr::Elvis {
                        lhs: Box::new(Expr::Var(name)),
                        rhs: Box::new(value),
                    },
                });
            }
            if matches!(next, Tok::PlusPlus | Tok::MinusMinus) {
                let inc = matches!(next, Tok::PlusPlus);
                self.advance(); // name
                self.advance(); // ++/--
                return Ok(StmtKind::Expr(Expr::PostIncDec { name, inc }));
            }
        }

        // Fallback: an expression, which may be the left side of a property
        // assignment (`recv.name = value`, `this.v = x`).
        let lhs = self.expression()?;
        // `recv.name ?= v` / `recv[i] ?= v` — the same desugar as the plain-name
        // form above, with the target read once in the source text and twice in
        // the tree (which is what Groovy's own `?=` does).
        if self.is(&Tok::ElvisAssign) {
            self.advance();
            self.skip_newlines();
            let value = self.expression()?;
            let guarded = Expr::Elvis {
                lhs: Box::new(lhs.clone()),
                rhs: Box::new(value),
            };
            return match lhs {
                Expr::Property { recv, name, .. } => Ok(StmtKind::SetProperty {
                    recv: *recv,
                    name,
                    op: AssignOp::Assign,
                    value: guarded,
                }),
                Expr::Index { recv, index, .. } => Ok(StmtKind::SetIndex {
                    recv: *recv,
                    index: *index,
                    op: AssignOp::Assign,
                    value: guarded,
                }),
                _ => Err(format!(
                    "groovyrs: invalid assignment target on line {}",
                    self.line()
                )),
            };
        }
        // `recv.name <op>= value` / `recv[index] <op>= value`, plain `=` included.
        // The compound forms are not rewritten to `t = t <op> v` here: Groovy
        // evaluates the receiver and the index once, so the op is carried into
        // the statement and the compiler duplicates them on the stack.
        if let Some(op) = assign_op(self.peek()) {
            self.advance();
            self.skip_newlines();
            let value = self.expression()?;
            return self.assign_to_target(lhs, op, value);
        }
        // `recv.name++` / `recv[index]--` in statement position, which is
        // `<target> += 1` / `-= 1`. Only as a statement: the postfix value a
        // Groovy expression sees is the *old* one, and nothing here reads it.
        if matches!(self.peek(), Tok::PlusPlus | Tok::MinusMinus)
            && matches!(lhs, Expr::Property { .. } | Expr::Index { .. })
        {
            let op = if matches!(self.peek(), Tok::PlusPlus) {
                AssignOp::Add
            } else {
                AssignOp::Sub
            };
            self.advance();
            return self.assign_to_target(lhs, op, Expr::Int(1, IntWidth::Int));
        }
        Ok(StmtKind::Expr(lhs))
    }

    /// `def (a, b) = expr` — Groovy's multiple assignment. Lowered to a hidden
    /// temporary holding the right side (so it is evaluated exactly once)
    /// followed by one declaration per name taking its positional element; a
    /// name past the end of the right side is `null`, as Groovy's is.
    fn destructuring_decl(&mut self, line: u32) -> Result<StmtKind, String> {
        self.eat(&Tok::LParen)?;
        let mut names = Vec::new();
        while !self.is(&Tok::RParen) {
            // An optional type in front of the name (`def (int a, b) = …`).
            if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Ident(_)) {
                self.advance();
            }
            names.push(self.ident()?);
            if self.is(&Tok::Comma) {
                self.advance();
                self.skip_newlines();
            }
        }
        self.eat(&Tok::RParen)?;
        self.eat(&Tok::Assign)?;
        self.skip_newlines();
        let value = self.expression()?;
        let tmp = self.fresh_tmp("destructure");
        for (i, name) in names.into_iter().enumerate() {
            self.pending.push(Stmt::new(
                line,
                StmtKind::Local {
                    ty: "def".into(),
                    name,
                    init: Some(Expr::Index {
                        recv: Box::new(Expr::Var(tmp.clone())),
                        index: Box::new(Expr::Int(i as i64, IntWidth::Int)),
                        line,
                    }),
                },
            ));
        }
        Ok(StmtKind::Local {
            ty: "def".into(),
            name: tmp,
            init: Some(value),
        })
    }

    /// Build the assignment statement for an expression target — a property or
    /// a subscript. Anything else is not assignable.
    fn assign_to_target(
        &mut self,
        target: Expr,
        op: AssignOp,
        value: Expr,
    ) -> Result<StmtKind, String> {
        match target {
            Expr::Property { recv, name, .. } => Ok(StmtKind::SetProperty {
                recv: *recv,
                name,
                op,
                value,
            }),
            // `recv[index] = value` — Groovy's `putAt`.
            Expr::Index { recv, index, .. } => Ok(StmtKind::SetIndex {
                recv: *recv,
                index: *index,
                op,
                value,
            }),
            _ => Err(format!(
                "groovyrs: invalid assignment target on line {}",
                self.line()
            )),
        }
    }

    /// Parse an optional `= expr` initializer (newlines after `=` continue).
    fn opt_initializer(&mut self) -> Result<Option<Expr>, String> {
        if self.is(&Tok::Assign) {
            self.advance();
            self.skip_newlines();
            Ok(Some(self.expression()?))
        } else {
            Ok(None)
        }
    }

    /// Parse a function definition `name(params) { body }` with the name already
    /// consumed and the `(` as the current token.
    fn function_def(&mut self, name: String) -> Result<StmtKind, String> {
        let params = self.param_list()?;
        self.skip_newlines();
        self.eat(&Tok::LBrace)?;
        let body = self.block()?;
        Ok(StmtKind::Function { name, params, body })
    }

    /// Does a `class`/`interface` declaration start here, possibly behind
    /// modifiers (`abstract class C`, `public final class C`)? Answers
    /// `Some(true)` for an interface, `Some(false)` for a class, `None` when
    /// this is not a type declaration. The keyword must be followed by a name,
    /// so a variable called `class` or `interface` still parses as one.
    fn type_decl_ahead(&self) -> Option<bool> {
        let mut i = 0;
        loop {
            match self.peek_at(i) {
                Tok::Ident(w) if w == "class" || w == "interface" => {
                    return matches!(self.peek_at(i + 1), Tok::Ident(_))
                        .then_some(w == "interface");
                }
                Tok::Ident(w)
                    if matches!(
                        w.as_str(),
                        "public" | "private" | "protected" | "static" | "final" | "abstract"
                    ) =>
                {
                    i += 1;
                }
                _ => return None,
            }
        }
    }

    /// Parse a class or interface declaration
    /// `class Name [extends S] [implements A, B] { members }` /
    /// `interface Name [extends A, B] { members }`. The `class`/`interface`
    /// keyword is the current token.
    ///
    /// A class takes at most one `extends` (single inheritance) plus any number
    /// of `implements` names; an interface's `extends` list is itself a list of
    /// interfaces (Java allows several). Members are fields
    /// (`def x [= init]` / `Type x [= init]`), constructors (`Name(params){..}`),
    /// and methods (`def m(params){..}` / typed). Inside an interface a method
    /// with no body is an abstract declaration and contributes nothing; one with
    /// a body is a `default` method every implementor inherits.
    fn class_decl(&mut self, is_interface: bool) -> Result<StmtKind, String> {
        self.advance(); // `class` / `interface`
        let name = self.ident()?;
        self.skip_newlines();
        let mut superclass = None;
        let mut interfaces = Vec::new();
        while !self.is(&Tok::LBrace) && !self.is(&Tok::Eof) {
            if matches!(self.peek(), Tok::Ident(k) if k == "extends") {
                self.advance();
                // A class extends one superclass; an interface extends a list of
                // interfaces.
                loop {
                    let parent = self.ident()?;
                    if is_interface {
                        interfaces.push(parent);
                    } else {
                        superclass = Some(parent);
                    }
                    if self.is(&Tok::Comma) {
                        self.advance();
                        self.skip_newlines();
                        continue;
                    }
                    break;
                }
                continue;
            }
            if matches!(self.peek(), Tok::Ident(k) if k == "implements") {
                self.advance();
                loop {
                    interfaces.push(self.ident()?);
                    if self.is(&Tok::Comma) {
                        self.advance();
                        self.skip_newlines();
                        continue;
                    }
                    break;
                }
                continue;
            }
            self.advance();
        }
        self.eat(&Tok::LBrace)?;
        let mut fields = Vec::new();
        let mut ctors = Vec::new();
        let mut methods = Vec::new();
        let mut abstract_methods = Vec::new();
        self.skip_terminators();
        while !self.is(&Tok::RBrace) && !self.is(&Tok::Eof) {
            self.class_member(
                &name,
                is_interface,
                &mut fields,
                &mut ctors,
                &mut methods,
                &mut abstract_methods,
            )?;
            self.expect_terminator()?;
            self.skip_terminators();
        }
        self.eat(&Tok::RBrace)?;
        Ok(StmtKind::Class {
            name,
            superclass,
            interfaces,
            is_interface,
            fields,
            ctors,
            methods,
            abstract_methods,
        })
    }

    /// Parse one class member into the appropriate bucket. Leading visibility /
    /// `static` / `final` modifiers are skipped (dynamic runtime).
    ///
    /// In an interface (`in_interface`) a method signature may end without a
    /// body — that abstract declaration binds nothing and is dropped, leaving
    /// dispatch to the implementing class.
    fn class_member(
        &mut self,
        class_name: &str,
        in_interface: bool,
        fields: &mut Vec<Field>,
        ctors: &mut Vec<Ctor>,
        methods: &mut Vec<Method>,
        abstract_methods: &mut Vec<String>,
    ) -> Result<(), String> {
        // Skip annotations (`@Override`, `@SuppressWarnings("x")`, …): the marker,
        // its name, and any parenthesised arguments. They have no runtime effect.
        while self.is(&Tok::At) {
            self.advance(); // `@`
            self.ident()?; // annotation name
            if self.is(&Tok::LParen) {
                let mut depth = 0;
                loop {
                    match self.peek() {
                        Tok::LParen => depth += 1,
                        Tok::RParen => {
                            depth -= 1;
                            self.advance();
                            if depth == 0 {
                                break;
                            }
                            continue;
                        }
                        Tok::Eof => break,
                        _ => {}
                    }
                    self.advance();
                }
            }
            self.skip_newlines();
        }
        // Skip modifier keywords. `default` is a real token (the `switch` label),
        // and in front of an interface method it is a modifier like the rest.
        while self.is(&Tok::Default)
            || matches!(
                self.peek(),
                Tok::Ident(m) if matches!(
                    m.as_str(),
                    "public" | "private" | "protected" | "static" | "final"
                        | "abstract" | "synchronized" | "transient" | "volatile"
                )
            )
        {
            self.advance();
        }
        // `def name` — a field or method.
        if self.is(&Tok::Def) {
            self.advance();
            let name = self.ident()?;
            if self.is(&Tok::LParen) {
                let params = self.param_list()?;
                let Some(body) = self.opt_member_body(in_interface)? else {
                    abstract_methods.push(name);
                    return Ok(());
                };
                methods.push(Method { name, params, body });
            } else {
                let init = self.opt_initializer()?;
                fields.push(Field { name, init });
            }
            return Ok(());
        }
        // A constructor `ClassName(params) { .. }` — a bare name (matching the
        // class) directly followed by `(`.
        if matches!(self.peek(), Tok::Ident(n) if n == class_name)
            && matches!(self.peek_at(1), Tok::LParen)
        {
            self.advance(); // class name
            let params = self.param_list()?;
            let body = self.member_body()?;
            ctors.push(Ctor { params, body });
            return Ok(());
        }
        // A typed member `Type name ...` — a field or method.
        if self.looks_like_decl() {
            self.ident()?; // return / field type (ignored)
            let name = self.ident()?;
            if self.is(&Tok::LParen) {
                let params = self.param_list()?;
                let Some(body) = self.opt_member_body(in_interface)? else {
                    abstract_methods.push(name);
                    return Ok(());
                };
                methods.push(Method { name, params, body });
            } else {
                let init = self.opt_initializer()?;
                fields.push(Field { name, init });
            }
            return Ok(());
        }
        Err(format!(
            "groovyrs: unexpected class member {} on line {}",
            self.peek(),
            self.line()
        ))
    }

    /// Parse a method body, or recognise an interface's abstract declaration.
    ///
    /// Inside an interface a signature may end at the statement terminator
    /// (`String name()` + newline): that declares the method without binding a
    /// body, so `None` comes back and the member is dropped. Everywhere else a
    /// body is required.
    fn opt_member_body(&mut self, in_interface: bool) -> Result<Option<Vec<Stmt>>, String> {
        if in_interface && !self.is(&Tok::LBrace) {
            return Ok(None);
        }
        self.member_body().map(Some)
    }

    /// Parse a method/constructor body `{ ... }` (newlines before `{` allowed).
    fn member_body(&mut self) -> Result<Vec<Stmt>, String> {
        self.skip_newlines();
        self.eat(&Tok::LBrace)?;
        self.block()
    }

    /// Parse a parenthesised parameter list `( [Type] a, [Type] b, ... )`. An
    /// optional `def` or type name in front of each parameter is skipped — the
    /// runtime is dynamically typed, so only the parameter names are retained.
    fn param_list(&mut self) -> Result<Vec<String>, String> {
        self.eat(&Tok::LParen)?;
        self.skip_newlines();
        let mut out = Vec::new();
        if !self.is(&Tok::RParen) {
            loop {
                if self.is(&Tok::Def) {
                    self.advance();
                } else if matches!(self.peek(), Tok::Ident(_))
                    && matches!(self.peek_at(1), Tok::Ident(_))
                {
                    self.advance(); // a type in front of the parameter name
                }
                out.push(self.ident()?);
                self.skip_newlines();
                if self.is(&Tok::Comma) {
                    self.advance();
                    self.skip_newlines();
                    continue;
                }
                break;
            }
        }
        self.eat(&Tok::RParen)?;
        Ok(out)
    }

    /// Heuristic: two identifiers in a row (`Type name`) — a typed declaration.
    /// Optional array brackets on the type (`int[] a`) are skipped.
    fn looks_like_decl(&self) -> bool {
        if !matches!(self.peek(), Tok::Ident(_)) {
            return false;
        }
        let mut j = self.pos + 1;
        while matches!(self.toks.get(j).map(|t| &t.kind), Some(Tok::LBracket))
            && matches!(self.toks.get(j + 1).map(|t| &t.kind), Some(Tok::RBracket))
        {
            j += 2;
        }
        // The name position must be an identifier — but not a contextual operator
        // keyword. `o instanceof P` is a type test, not a declaration of a variable
        // named `instanceof`; likewise `x in xs`.
        matches!(
            self.toks.get(j).map(|t| &t.kind),
            Some(Tok::Ident(n)) if n != "instanceof" && n != "in"
        )
    }

    fn if_stmt(&mut self) -> Result<StmtKind, String> {
        self.eat(&Tok::If)?;
        self.eat(&Tok::LParen)?;
        let cond = self.expression()?;
        self.eat(&Tok::RParen)?;
        let then = self.braced_or_single()?;
        // `else` may follow on the same line or after a newline.
        let save = self.pos;
        self.skip_newlines();
        let els = if self.is(&Tok::Else) {
            self.advance();
            self.braced_or_single()?
        } else {
            self.pos = save;
            vec![]
        };
        Ok(StmtKind::If { cond, then, els })
    }

    /// Parse `try { … } catch (T e) { … }* [finally { … }]`. The `try`
    /// identifier is the current token. A `catch` may name several types
    /// (`catch (A | B e)`) or none at all (`catch (e)`, which Groovy reads as
    /// `Exception`). Groovy requires at least one `catch` or a `finally`.
    fn try_stmt(&mut self) -> Result<StmtKind, String> {
        let line = self.line();
        self.advance(); // `try`
        self.skip_newlines();
        self.eat(&Tok::LBrace)?;
        let body = self.block()?;
        let mut catches = Vec::new();
        let mut finally_body = Vec::new();
        loop {
            let save = self.pos;
            self.skip_newlines();
            match self.peek() {
                Tok::Ident(w) if w == "catch" => {
                    self.advance();
                    self.eat(&Tok::LParen)?;
                    // `Type [| Type]* name`, or a bare `name`. A caught type may
                    // be written fully qualified — `catch
                    // (groovy.lang.MissingMethodException e)` — which is the
                    // only spelling for a type whose simple name a script has
                    // shadowed, and the one the Groovy docs use for its own
                    // exceptions. `type_name` reads the dots; a bare `name`
                    // (the untyped `catch (e)`) has none and reads as itself.
                    let mut words = vec![self.type_name()?];
                    while self.is(&Tok::Pipe) {
                        self.advance();
                        words.push(self.type_name()?);
                    }
                    let (types, name) = if self.is(&Tok::RParen) {
                        // `catch (e)` — untyped, catches every Exception.
                        (vec!["Exception".to_string()], words.remove(0))
                    } else {
                        let name = self.ident()?;
                        (words, name)
                    };
                    self.eat(&Tok::RParen)?;
                    self.skip_newlines();
                    self.eat(&Tok::LBrace)?;
                    let cbody = self.block()?;
                    catches.push(CatchArm {
                        types,
                        name,
                        body: cbody,
                    });
                }
                Tok::Ident(w) if w == "finally" => {
                    self.advance();
                    self.skip_newlines();
                    self.eat(&Tok::LBrace)?;
                    finally_body = self.block()?;
                    break;
                }
                _ => {
                    // Not a clause of this `try` — put the newlines back so the
                    // next statement still sees its terminator.
                    self.pos = save;
                    break;
                }
            }
        }
        if catches.is_empty() && finally_body.is_empty() {
            return Err(format!(
                "groovyrs: `try` needs a `catch` or a `finally` on line {line}"
            ));
        }
        Ok(StmtKind::Try {
            body,
            catches,
            finally_body,
        })
    }

    /// Does an `ident :` at the current position introduce a statement label?
    /// Only when a loop or `switch` follows (after any newlines) — otherwise the
    /// `:` belongs to something else and the statement is an ordinary one.
    fn starts_a_label(&self) -> bool {
        if !matches!(self.peek_at(1), Tok::Colon) {
            return false;
        }
        let mut n = 2;
        while matches!(self.peek_at(n), Tok::Nl) {
            n += 1;
        }
        matches!(
            self.peek_at(n),
            Tok::While | Tok::Do | Tok::For | Tok::Switch
        )
    }

    /// The optional target label of a `break`/`continue` (`break outer`). It has
    /// to be on the same line — a bare `break` followed by a statement starting
    /// with an identifier must not swallow it.
    fn opt_jump_label(&mut self) -> Option<String> {
        match self.peek() {
            Tok::Ident(name) => {
                let name = name.clone();
                self.advance();
                Some(name)
            }
            _ => None,
        }
    }

    /// `assert cond [: message]`.
    ///
    /// The condition is parsed in *recording* mode, so every sub-expression
    /// Groovy's power assert prints a value for is wrapped in
    /// [`Expr::Recorded`] with its source column, and its verbatim source text
    /// is sliced out for the `assert <text>` line above those values.
    fn assert_stmt(&mut self) -> Result<StmtKind, String> {
        let keyword_col = self.col_at(0);
        // Slice from the keyword, not the condition: Groovy reprints the
        // statement's source verbatim, whitespace included.
        let start = self.toks[self.pos].offset;
        self.eat(&Tok::Assert)?;
        self.recording = Some(keyword_col);
        let cond = self.expression();
        self.recording = None;
        let cond = cond?;
        // The condition's text ends where the next token begins; trailing
        // whitespace and the `:` of the message form are trimmed off.
        let end = self.toks[self.pos].offset.min(self.src.len());
        // The renderer refuses a text with line breaks, so a condition wrapped
        // across lines is joined back onto one. The `assert ` keyword the power
        // form prints above the values is prepended host-side, because the
        // `: message` form quotes the condition *without* it.
        let text = self.src[start..end].trim_end().replace('\n', " ");
        let message = if self.is(&Tok::Colon) {
            self.advance();
            self.skip_newlines();
            Some(self.expression()?)
        } else {
            None
        };
        let value_names = assert_value_names(&cond);
        let ast_text = expr_text(&cond);
        Ok(StmtKind::Assert {
            cond,
            message,
            text,
            ast_text,
            value_names,
        })
    }

    /// `do { … } while (cond)`.
    fn do_while_stmt(&mut self) -> Result<StmtKind, String> {
        self.eat(&Tok::Do)?;
        let body = self.braced_or_single()?;
        self.skip_newlines();
        self.eat(&Tok::While)?;
        self.eat(&Tok::LParen)?;
        let cond = self.expression()?;
        self.eat(&Tok::RParen)?;
        Ok(StmtKind::DoWhile { body, cond })
    }

    /// `switch (subject) { case L: … default: … }`. Sections keep source order;
    /// a section with an empty body is how consecutive labels share one.
    fn switch_stmt(&mut self) -> Result<StmtKind, String> {
        self.eat(&Tok::Switch)?;
        self.eat(&Tok::LParen)?;
        let subject = self.expression()?;
        self.eat(&Tok::RParen)?;
        self.skip_newlines();
        self.eat(&Tok::LBrace)?;
        let mut cases: Vec<SwitchCase> = Vec::new();
        self.skip_terminators();
        while !self.is(&Tok::RBrace) && !self.is(&Tok::Eof) {
            let label = match self.peek() {
                Tok::Case => {
                    self.advance();
                    let e = self.expression()?;
                    Some(e)
                }
                Tok::Default => {
                    self.advance();
                    None
                }
                other => {
                    return Err(format!(
                    "groovyrs: expected `case` or `default` in switch but found {other} on line {}",
                    self.line()
                ))
                }
            };
            self.eat(&Tok::Colon)?;
            self.skip_terminators();
            // The section body runs to the next label or the closing brace.
            let mut body = Vec::new();
            while !matches!(
                self.peek(),
                Tok::Case | Tok::Default | Tok::RBrace | Tok::Eof
            ) {
                body.push(self.statement()?);
                self.expect_terminator()?;
                self.skip_terminators();
            }
            cases.push(SwitchCase { label, body });
        }
        self.eat(&Tok::RBrace)?;
        if cases.iter().filter(|c| c.label.is_none()).count() > 1 {
            return Err(format!(
                "groovyrs: duplicate `default` in switch on line {}",
                self.line()
            ));
        }
        Ok(StmtKind::Switch { subject, cases })
    }

    fn while_stmt(&mut self) -> Result<StmtKind, String> {
        self.eat(&Tok::While)?;
        self.eat(&Tok::LParen)?;
        let cond = self.expression()?;
        self.eat(&Tok::RParen)?;
        let body = self.braced_or_single()?;
        Ok(StmtKind::While { cond, body })
    }

    fn for_stmt(&mut self) -> Result<StmtKind, String> {
        self.eat(&Tok::For)?;
        self.eat(&Tok::LParen)?;
        if self.for_is_in() {
            return self.for_in();
        }
        // C-style `for (init; cond; update)`. Both `init` and `update` may be
        // comma-separated lists (`for (int i = 0, j = n; i < j; i++, j--)`).
        let line = self.line();
        let inits = self.for_clause(&Tok::Semi)?;
        self.eat(&Tok::Semi)?;
        let cond = if self.is(&Tok::Semi) {
            None
        } else {
            Some(self.expression()?)
        };
        self.eat(&Tok::Semi)?;
        let updates = self.for_clause(&Tok::RParen)?;
        self.eat(&Tok::RParen)?;
        let body = self.braced_or_single()?;
        // One update statement fits the `For` node; several are wrapped in a
        // block so `continue` still runs them all, which is what the node's
        // single-statement update slot means.
        let update = match updates.len() {
            0 => None,
            1 => Some(Box::new(updates.into_iter().next().unwrap())),
            _ => Some(Box::new(Stmt::new(
                line,
                StmtKind::If {
                    cond: Expr::Bool(true),
                    then: updates,
                    els: vec![],
                },
            ))),
        };
        // Every initializer runs once before the loop, so they are hoisted ahead
        // of it rather than crowded into the single `init` slot — the same block
        // wrapper the `for (x in …)` desugaring uses to scope its temporaries.
        if inits.len() > 1 {
            let loop_stmt = Stmt::new(
                line,
                StmtKind::For {
                    init: None,
                    cond,
                    update,
                    body,
                },
            );
            let mut then = inits;
            then.push(loop_stmt);
            return Ok(StmtKind::If {
                cond: Expr::Bool(true),
                then,
                els: vec![],
            });
        }
        Ok(StmtKind::For {
            init: inits.into_iter().next().map(Box::new),
            cond,
            update,
            body,
        })
    }

    /// The `init` or `update` clause of a C-style `for` header: zero or more
    /// simple statements separated by commas, up to `end`.
    ///
    /// A declarator after the first inherits the first's type, the way Java's
    /// and Groovy's do — `for (int i = 0, j = 3; …)` declares two `int`s, not an
    /// `int` and an assignment to an undeclared `j`.
    fn for_clause(&mut self, end: &Tok) -> Result<Vec<Stmt>, String> {
        if self.is(end) {
            return Ok(Vec::new());
        }
        let line = self.line();
        let first = self.simple_statement()?;
        let decl_ty = match &first.kind {
            StmtKind::Local { ty, .. } => Some(ty.clone()),
            _ => None,
        };
        let mut out = vec![first];
        // A declarator can also have arrived through `self.pending` (`def a = 1,
        // b = 2` queues its later names there); in a header there is no
        // enclosing statement list to drain it into, so take them here.
        out.append(&mut self.pending);
        while self.is(&Tok::Comma) {
            self.advance();
            self.skip_newlines();
            match &decl_ty {
                Some(ty) => {
                    let name = self.ident()?;
                    let init = self.opt_initializer()?;
                    out.push(Stmt::new(
                        line,
                        StmtKind::Local {
                            ty: ty.clone(),
                            name,
                            init,
                        },
                    ));
                }
                None => out.push(self.simple_statement()?),
            }
            out.append(&mut self.pending);
        }
        Ok(out)
    }

    /// Lookahead: is the `for (` header a `for (x in …)` range loop? (An `in`
    /// token appears before the first `;` or the closing `)`.)
    fn for_is_in(&self) -> bool {
        let mut j = self.pos;
        loop {
            match self.toks.get(j).map(|t| &t.kind) {
                Some(Tok::In) => return true,
                Some(Tok::Semi) | Some(Tok::RParen) | Some(Tok::Eof) | None => return false,
                _ => j += 1,
            }
        }
    }

    /// Parse `for ([def|Type] id in start..end)` (or `..<`) and desugar it to a
    /// counting C-style `for` over a *hidden* cursor:
    ///
    /// ```text
    ///   def $lo   = <start>                  ; endpoints evaluated once
    ///   def $hi   = <end>
    ///   def $desc = $lo > $hi                ; 5..1 counts DOWN
    ///   def $chr  = $lo instanceof String    ; 'a'..'e' walks characters
    ///   def $st   = $desc ? -1 : 1
    ///   def $last = <$hi, backed off one step when the range is exclusive>
    ///   for (def $cur = $lo; $desc ? $cur >= $last : $cur <= $last; <step $cur>) {
    ///       def <var> = $cur
    ///       <body>
    ///   }
    /// ```
    ///
    /// Three things this shape buys over incrementing the user's variable
    /// directly, each measured against Apache Groovy:
    ///
    /// * **Direction.** `for (i in 5..1)` yields `5 4 3 2 1`; a bare `i++` loop
    ///   never enters. Exclusive reverse (`5..<1`) drops the *written* endpoint,
    ///   so `$last` steps back toward `$lo` either way.
    /// * **Element type.** `for (c in 'a'..'e')` walks `a b c d e`; `c++` on a
    ///   `String` never terminates. The character case steps with
    ///   `next()`/`previous()`, guarded by a loop-invariant `$chr` so an integer
    ///   range still steps with a native `+`.
    /// * **Body isolation.** `for (i in 0..4) { i = 3 }` still runs five times,
    ///   because the loop counts on `$cur` and `<var>` is a fresh binding per
    ///   iteration — matching Groovy's snapshot iterator.
    ///
    /// A non-range subject (`for (x in coll)`) goes to [`Self::for_in_sequence`].
    fn for_in(&mut self) -> Result<StmtKind, String> {
        let line = self.line();
        // Optional `def`/type in front of the loop variable.
        if self.is(&Tok::Def) {
            self.advance();
        } else if self.looks_like_decl() {
            self.ident()?; // type
        }
        let var = self.ident()?;
        self.eat(&Tok::In)?;
        // Parse the endpoints with `binary` (not `expression`) so the `..`/`..<`
        // range operator is consumed here rather than folded into a `Range` node
        // by `range_expr`.
        let start = self.binary(0)?;
        let inclusive = match self.peek() {
            Tok::DotDot => {
                self.advance();
                true
            }
            Tok::DotDotLt => {
                self.advance();
                false
            }
            // Not a range: `for (x in <collection>)`, desugared below.
            _ => return self.for_in_sequence(line, var, start),
        };
        let end = self.binary(0)?;
        self.eat(&Tok::RParen)?;
        let mut body = self.braced_or_single()?;

        let lo = self.fresh_tmp("lo");
        let hi = self.fresh_tmp("hi");
        let desc = self.fresh_tmp("desc");
        let chr = self.fresh_tmp("chr");
        let st = self.fresh_tmp("st");
        let last = self.fresh_tmp("last");
        let cur = self.fresh_tmp("cur");

        let v = |n: &str| Expr::Var(n.to_string());
        let local = |name: &str, init: Expr| {
            Stmt::new(
                line,
                StmtKind::Local {
                    ty: "def".into(),
                    name: name.to_string(),
                    init: Some(init),
                },
            )
        };
        let call = |recv: Expr, m: &str| Expr::MethodCall {
            recv: Box::new(recv),
            method: m.to_string(),
            args: Vec::new(),
            line,
            safe: false,
        };
        let bin = |op: BinOp, l: Expr, r: Expr| Expr::Binary {
            op,
            lhs: Box::new(l),
            rhs: Box::new(r),
        };
        let ternary = |c: Expr, t: Expr, e: Expr| Expr::Ternary {
            cond: Box::new(c),
            then: Box::new(t),
            els: Box::new(e),
        };
        // Both guards are loop-invariant, and both are usually decidable right
        // here: a numeric literal endpoint is never a character, and two integer
        // literals fix the direction. Folding them keeps `for (i in 0..n)` on
        // the same straight-line increment the naive desugar emitted, so the
        // generality above costs nothing on the shape that dominates.
        let chr_const = literal_number(&start).map(|_| false);
        let desc_const = match (literal_int(&start), literal_int(&end)) {
            (Some(a), Some(b)) => Some(a > b),
            _ => None,
        };
        let st_e = match desc_const {
            Some(d) => Expr::Int(if d { -1 } else { 1 }, IntWidth::Int),
            None => v(&st),
        };

        // One step of the walk, in the direction `$desc` chose. A character
        // range steps with the GDK's `next`/`previous`; anything else (Integer,
        // BigInteger, BigDecimal) adds the loop-invariant `$st`, which keeps an
        // integer range on a native add.
        let step = |e: Expr, forward: bool| {
            let numeric = bin(
                if forward { BinOp::Add } else { BinOp::Sub },
                e.clone(),
                st_e.clone(),
            );
            if chr_const == Some(false) {
                return numeric;
            }
            let (back, fwd) = if forward {
                ("previous", "next")
            } else {
                ("next", "previous")
            };
            let character = match desc_const {
                Some(true) => call(e.clone(), back),
                Some(false) => call(e.clone(), fwd),
                None => ternary(v(&desc), call(e.clone(), back), call(e.clone(), fwd)),
            };
            ternary(v(&chr), character, numeric)
        };

        // The last value the walk may take: the written endpoint, or one step
        // back toward `$lo` when the range is exclusive.
        let last_init = if inclusive {
            v(&hi)
        } else {
            step(v(&hi), false)
        };

        // The loop variable is declared **once**, outside the loop, and assigned
        // per iteration. Groovy's `for (x in …)` binds one variable for the whole
        // loop, so every closure built in the body shares it and they all read
        // the final value — declaring it inside the body would give each
        // iteration its own binding (see `host::GCELL_NEW`).
        let assign_var = Stmt::new(
            line,
            StmtKind::Assign {
                name: var.clone(),
                op: AssignOp::Assign,
                value: v(&cur),
            },
        );
        body.insert(0, assign_var);
        let at_or_before = bin(BinOp::Le, v(&cur), v(&last));
        let at_or_after = bin(BinOp::Ge, v(&cur), v(&last));
        let loop_for = StmtKind::For {
            init: Some(Box::new(local(&cur, v(&lo)))),
            cond: Some(match desc_const {
                Some(true) => at_or_after,
                Some(false) => at_or_before,
                None => ternary(v(&desc), at_or_after, at_or_before),
            }),
            // With both guards folded the walk is a plain ±1 on the cursor, so
            // it lowers to the same `++`/`--` the naive desugar used.
            update: Some(Box::new(Stmt::new(
                line,
                match (chr_const, desc_const) {
                    (Some(false), Some(d)) => StmtKind::Expr(Expr::PostIncDec {
                        name: cur.clone(),
                        inc: !d,
                    }),
                    _ => StmtKind::Assign {
                        name: cur.clone(),
                        op: AssignOp::Assign,
                        value: step(v(&cur), true),
                    },
                },
            ))),
            body,
        };
        // Wrap in an always-true block so the loop's synthetic temps and the
        // loop itself share a frame without introducing a Block node.
        Ok(StmtKind::If {
            cond: Expr::Bool(true),
            then: vec![
                local(&lo, start),
                local(&hi, end),
                local(&desc, bin(BinOp::Gt, v(&lo), v(&hi))),
                local(
                    &chr,
                    Expr::InstanceOf {
                        value: Box::new(v(&lo)),
                        class: "String".into(),
                    },
                ),
                local(
                    &st,
                    ternary(
                        v(&desc),
                        Expr::Int(-1, IntWidth::Int),
                        Expr::Int(1, IntWidth::Int),
                    ),
                ),
                local(&last, last_init),
                local(&var, Expr::Null),
                Stmt::new(line, loop_for),
            ],
            els: vec![],
        })
    }

    /// Desugar `for (var in <collection>)` to a counting loop over the
    /// materialised sequence:
    ///
    /// ```text
    ///   def $seq = <collection as a sequence>
    ///   def $len = $seq.size()
    ///   for (def $i = 0; $i < $len; $i++) { def var = $seq[$i]; <body> }
    /// ```
    ///
    /// The sequence is materialised once ([`Expr::Iterable`]) and its length
    /// read once, so the loop condition stays a native integer compare and a
    /// body that mutates the collection still walks the original elements —
    /// which is what Groovy's snapshot iterator does.
    fn for_in_sequence(
        &mut self,
        line: u32,
        var: String,
        subject: Expr,
    ) -> Result<StmtKind, String> {
        self.eat(&Tok::RParen)?;
        let mut body = self.braced_or_single()?;
        let seq = self.fresh_tmp("seq");
        let len = self.fresh_tmp("len");
        let idx = self.fresh_tmp("idx");
        let local = |name: &str, init: Expr| {
            Stmt::new(
                line,
                StmtKind::Local {
                    ty: "def".into(),
                    name: name.to_string(),
                    init: Some(init),
                },
            )
        };
        // One binding for the whole loop, assigned per iteration — see the range
        // form's note.
        body.insert(
            0,
            Stmt::new(
                line,
                StmtKind::Assign {
                    name: var.clone(),
                    op: AssignOp::Assign,
                    value: Expr::Index {
                        recv: Box::new(Expr::Var(seq.clone())),
                        index: Box::new(Expr::Var(idx.clone())),
                        line,
                    },
                },
            ),
        );
        let loop_for = StmtKind::For {
            init: Some(Box::new(local(&idx, Expr::Int(0, IntWidth::Int)))),
            cond: Some(Expr::Binary {
                op: BinOp::Lt,
                lhs: Box::new(Expr::Var(idx.clone())),
                rhs: Box::new(Expr::Var(len.clone())),
            }),
            update: Some(Box::new(Stmt::new(
                line,
                StmtKind::Expr(Expr::PostIncDec {
                    name: idx,
                    inc: true,
                }),
            ))),
            body,
        };
        Ok(StmtKind::If {
            cond: Expr::Bool(true),
            then: vec![
                local(&seq, Expr::Iterable(Box::new(subject))),
                // `size()`, not the `size` *property* — Groovy has no such
                // property on a list, and this desugar must not depend on one.
                local(
                    &len,
                    Expr::MethodCall {
                        recv: Box::new(Expr::Var(seq)),
                        method: "size".to_string(),
                        args: Vec::new(),
                        line,
                        safe: false,
                    },
                ),
                local(&var, Expr::Null),
                Stmt::new(line, loop_for),
            ],
            els: vec![],
        })
    }

    // ── expressions (precedence climbing) ─────────────────────────────────

    /// The full expression grammar, lowest precedence first:
    /// `conditional` (ternary / elvis) over `range` (`a..b`) over the
    /// precedence-climbing `binary` operators.
    fn expression(&mut self) -> Result<Expr, String> {
        self.conditional()
    }

    /// The ternary `c ? t : e` and Elvis `c ?: e` operators — the lowest
    /// expression precedence, right-associative. Both branch on Groovy
    /// truthiness. A bare operand with no `?`/`?:` passes straight through.
    fn conditional(&mut self) -> Result<Expr, String> {
        let cond = self.range_expr()?;
        match self.peek() {
            Tok::Elvis => {
                self.advance();
                self.skip_newlines();
                let rhs = self.conditional()?;
                Ok(Expr::Elvis {
                    lhs: Box::new(cond),
                    rhs: Box::new(rhs),
                })
            }
            Tok::Question => {
                self.advance();
                self.skip_newlines();
                let then = self.conditional()?;
                self.eat(&Tok::Colon)?;
                self.skip_newlines();
                let els = self.conditional()?;
                Ok(Expr::Ternary {
                    cond: Box::new(cond),
                    then: Box::new(then),
                    els: Box::new(els),
                })
            }
            _ => Ok(cond),
        }
    }

    /// A range expression `a..b` (inclusive) or `a..<b` (half-open). Groovy puts
    /// `..` at the shift band, so its endpoints are parsed one level above that
    /// (`0..n-1` is `0..(n-1)`, but `1..3 as List` casts the whole range).
    fn range_expr(&mut self) -> Result<Expr, String> {
        let start = self.binary(RANGE_OPERAND_BP)?;
        let inclusive = match self.peek() {
            Tok::DotDot => true,
            Tok::DotDotLt => false,
            _ => return self.binary_from(start, 0),
        };
        self.advance();
        self.skip_newlines();
        let end = self.binary(RANGE_OPERAND_BP)?;
        let range = Expr::Range {
            start: Box::new(start),
            end: Box::new(end),
            inclusive,
        };
        // A range is itself an operand of anything below the shift band, so the
        // precedence loop resumes over it (`1..3 as List`, `(1..3) == r`).
        self.binary_from(range, 0)
    }

    fn binary(&mut self, min_bp: u8) -> Result<Expr, String> {
        let lhs = self.unary()?;
        self.binary_from(lhs, min_bp)
    }

    /// The precedence-climbing loop, resumed over an already-parsed left side.
    fn binary_from(&mut self, lhs: Expr, min_bp: u8) -> Result<Expr, String> {
        // A fold of `n` operators is an AST `n` deep, and every level of it is
        // one more frame for the compiler's walk and for the tree's own drop —
        // so the chain counts against the same budget the recursive descent
        // does. The whole chain is one expression, so the budget is released
        // when it finishes rather than per operator.
        let entry = self.depth;
        let r = self.binary_from_inner(lhs, min_bp);
        self.depth = entry;
        r
    }

    fn binary_from_inner(&mut self, mut lhs: Expr, min_bp: u8) -> Result<Expr, String> {
        loop {
            self.deepen()?;
            // `value instanceof Type` — relational precedence, recorded under
            // the `instanceof` keyword's column.
            if RELATIONAL_BP >= min_bp && matches!(self.peek(), Tok::Ident(k) if k == "instanceof")
            {
                let col = self.col_at(0);
                self.advance();
                // Fully qualified is legal here too (`t instanceof
                // java.io.IOException`), and reads the same way `as` and a
                // `catch` clause read their type names.
                let class = self.type_name()?;
                lhs = self.record(
                    col,
                    Expr::InstanceOf {
                        value: Box::new(lhs),
                        class,
                    },
                );
                continue;
            }
            // `value as Type` — a coercion whose right side is a *type name*,
            // not an expression, which is why `"3" as Integer + 1` is 4 and not
            // a parse of `Integer + 1`.
            if RELATIONAL_BP >= min_bp && matches!(self.peek(), Tok::Ident(k) if k == "as") {
                self.advance();
                let ty = self.type_name()?;
                lhs = Expr::Cast {
                    value: Box::new(lhs),
                    ty,
                };
                continue;
            }
            let Some((op, bp)) = binop(self.peek()) else {
                break;
            };
            if bp < min_bp {
                break;
            }
            // Groovy records a binary result under its *operator's* column.
            let col = self.col_at(0);
            self.advance();
            self.skip_newlines(); // a binary operator may continue on the next line
                                  // `**` is the one right-associative operator, so it recurses at its
                                  // own binding power rather than one above it.
            let rhs = self.binary(if matches!(op, BinOp::Power) {
                bp
            } else {
                bp + 1
            })?;
            lhs = self.record(
                col,
                Expr::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            );
        }
        Ok(lhs)
    }

    /// The type name on the right of an `as` coercion: a dotted name, with any
    /// generic argument list skipped (a coercion ignores erasure anyway).
    fn type_name(&mut self) -> Result<String, String> {
        let mut name = self.ident()?;
        while matches!(self.peek(), Tok::Dot) {
            self.advance();
            name.push('.');
            name.push_str(&self.ident()?);
        }
        // A trailing `[]` makes it an array type (`int[]`, `String[]`). Only the
        // *empty* pair: `new int[3]` writes its length there and is read by the
        // `new` form, which looks before calling this.
        while self.is(&Tok::LBracket) && matches!(self.peek_at(1), Tok::RBracket) {
            self.advance();
            self.advance();
            name.push_str("[]");
        }
        Ok(name)
    }

    /// Every expression that can *contain* another one reaches here first — a
    /// parenthesised group, a list or map literal, a closure body, a prefix
    /// operator — so this is where expression nesting is counted.
    fn unary(&mut self) -> Result<Expr, String> {
        self.deepen()?;
        let r = self.unary_inner();
        self.depth -= 1;
        r
    }

    fn unary_inner(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Tok::Minus | Tok::Not | Tok::Tilde => {
                let op = match self.peek() {
                    Tok::Minus => UnOp::Neg,
                    Tok::Tilde => UnOp::BitNot,
                    _ => UnOp::Not,
                };
                // Recorded under the operator's own column.
                let col = self.col_at(0);
                self.advance();
                let rhs = Box::new(self.unary()?);
                Ok(self.record(col, Expr::Unary { op, rhs }))
            }
            Tok::PlusPlus | Tok::MinusMinus => {
                let inc = matches!(self.peek(), Tok::PlusPlus);
                self.advance();
                match self.unary()? {
                    Expr::Var(name) => Ok(Expr::PreIncDec { name, inc }),
                    _ => Err(format!(
                        "groovyrs: prefix `{}` requires a variable on line {}",
                        if inc { "++" } else { "--" },
                        self.line()
                    )),
                }
            }
            _ => self.primary(),
        }
    }

    /// A primary expression: an atom followed by any run of postfix `.member`
    /// method calls (`x.foo(args)`) and property reads (`x.size`).
    fn primary(&mut self) -> Result<Expr, String> {
        let mut e = self.atom()?;
        loop {
            if self.is(&Tok::Dot) || self.is(&Tok::QuestionDot) {
                let safe = self.is(&Tok::QuestionDot);
                let line = self.line();
                self.advance();
                // Groovy records a member access under the *member name's*
                // column, not the receiver's.
                let col = self.col_at(0);
                let member = self.ident()?;
                if self.is(&Tok::LParen) {
                    let mut args = self.call_args()?;
                    // A trailing closure after the parenthesised args:
                    // `list.inject(0) { acc, v -> acc + v }`.
                    if self.is(&Tok::LBrace) {
                        args.push(self.closure_literal()?);
                    }
                    e = self.record(
                        col,
                        Expr::MethodCall {
                            recv: Box::new(e),
                            method: member,
                            args,
                            line,
                            safe,
                        },
                    );
                } else if self.is(&Tok::LBrace) {
                    // Paren-less trailing-closure call: `list.each { it -> ... }`.
                    let clo = self.closure_literal()?;
                    e = self.record(
                        col,
                        Expr::MethodCall {
                            recv: Box::new(e),
                            method: member,
                            args: vec![clo],
                            line,
                            safe,
                        },
                    );
                } else {
                    e = self.record(
                        col,
                        Expr::Property {
                            recv: Box::new(e),
                            name: member,
                            line,
                            safe,
                        },
                    );
                }
            } else if self.is(&Tok::StarDot) {
                // The spread operator `recv*.member` / `recv*.method(args)`:
                // apply the member to every element and collect the results.
                // Groovy's own definition is `recv.collect { it?.member }`, and
                // that is exactly the desugar — including the safe navigation,
                // which is why a `null` element spreads to `null`.
                let line = self.line();
                self.advance();
                let member = self.ident()?;
                let inner = if self.is(&Tok::LParen) {
                    Expr::MethodCall {
                        recv: Box::new(Expr::Var("it".to_string())),
                        method: member,
                        args: self.call_args()?,
                        line,
                        safe: true,
                    }
                } else {
                    Expr::Property {
                        recv: Box::new(Expr::Var("it".to_string())),
                        name: member,
                        line,
                        safe: true,
                    }
                };
                e = Expr::MethodCall {
                    recv: Box::new(e),
                    method: "collect".to_string(),
                    args: vec![Expr::Closure {
                        params: vec!["it".to_string()],
                        body: vec![Stmt::new(line, StmtKind::Expr(inner))],
                        explicit_params: false,
                        varargs: false,
                    }],
                    line,
                    safe: false,
                };
            } else if self.is(&Tok::LParen) {
                // Postfix call-application on a value: `f(a)(b)`, `getFn()(x)`.
                let line = self.line();
                let args = self.call_args()?;
                e = Expr::CallValue {
                    callee: Box::new(e),
                    args,
                    line,
                };
            } else if self.is(&Tok::LBracket) {
                // Subscript `recv[index]`, recorded under the `[` column.
                let line = self.line();
                let col = self.col_at(0);
                self.advance();
                self.skip_newlines();
                let index = self.expression()?;
                self.skip_newlines();
                self.eat(&Tok::RBracket)?;
                e = self.record(
                    col,
                    Expr::Index {
                        recv: Box::new(e),
                        index: Box::new(index),
                        line,
                    },
                );
            } else {
                break;
            }
        }
        Ok(e)
    }

    /// An atom: a literal, a parenthesised expression, a variable/call, or a
    /// list/map literal — the base a [`Parser::primary`] postfix chain builds on.
    fn atom(&mut self) -> Result<Expr, String> {
        match self.peek().clone() {
            Tok::Int(n, width) => {
                self.advance();
                Ok(Expr::Int(n, width))
            }
            Tok::Float(f) => {
                self.advance();
                Ok(Expr::Float(f))
            }
            Tok::Dec(text) => {
                self.advance();
                Ok(Expr::Dec(text))
            }
            Tok::BigInt(text) => {
                self.advance();
                Ok(Expr::BigInt(text))
            }
            Tok::Str(s) => {
                self.advance();
                Ok(Expr::Str(s))
            }
            Tok::Regex(src) => {
                self.advance();
                Ok(Expr::Regex(src))
            }
            // A `GString`: each embedded placeholder carries its own source
            // text, re-parsed here through the ordinary expression grammar.
            Tok::GStr(parts) => {
                self.advance();
                let mut out = Vec::with_capacity(parts.len());
                for part in &parts {
                    out.push(match part {
                        GPart::Text(t) => GStringPart::Text(t.clone()),
                        GPart::Expr(src) => GStringPart::Expr(parse_interpolation(src)?),
                    });
                }
                Ok(Expr::GString(out))
            }
            Tok::True => {
                self.advance();
                Ok(Expr::Bool(true))
            }
            Tok::False => {
                self.advance();
                Ok(Expr::Bool(false))
            }
            Tok::Null => {
                self.advance();
                Ok(Expr::Null)
            }
            Tok::LParen => {
                self.advance();
                let e = self.expression()?;
                self.eat(&Tok::RParen)?;
                Ok(e)
            }
            Tok::LBracket => self.list_or_map(),
            // A `{ ... }` in expression position is a closure literal (map
            // literals use `[...]`, so there is no ambiguity here).
            Tok::LBrace => self.closure_literal(),
            Tok::New => {
                let line = self.line();
                self.advance();
                // `new java.io.IOException("q")` — the qualified spelling reads
                // the same way `as`, `instanceof` and a `catch` clause read
                // theirs; `host::b_new` resolves the package.
                let class = self.type_name()?;
                // `new int[3]` — an array creation. The length is an expression,
                // so it cannot ride in the type name; it becomes the single
                // constructor argument of the array type, which is what
                // `host::b_new` builds from.
                if self.is(&Tok::LBracket) && !class.ends_with("[]") {
                    self.advance();
                    let len = self.expression()?;
                    self.eat(&Tok::RBracket)?;
                    return Ok(Expr::New {
                        class: format!("{class}[]"),
                        args: vec![len],
                        line,
                    });
                }
                let args = if self.is(&Tok::LParen) {
                    self.call_args()?
                } else {
                    Vec::new()
                };
                Ok(Expr::New { class, args, line })
            }
            Tok::Ident(name) => {
                if name == "println" || name == "print" {
                    return self.print_call(&name);
                }
                // `this` is the current instance inside a method/constructor.
                if name == "this" {
                    self.advance();
                    return Ok(Expr::This);
                }
                // `super` — either a super-constructor call `super(args)` or the
                // receiver of a `super.method(args)` static-super dispatch.
                if name == "super" {
                    let line = self.line();
                    self.advance();
                    if self.is(&Tok::LParen) {
                        let args = self.call_args()?;
                        return Ok(Expr::SuperCtor { args, line });
                    }
                    return Ok(Expr::Super);
                }
                let line = self.line();
                // A variable read and a call are both recorded under the
                // identifier's own column.
                let col = self.col_at(0);
                self.advance();
                // A call expression `name(args...)`: a user-defined function or an
                // inline-Rust FFI export (the compiler resolves which).
                if self.is(&Tok::LParen) {
                    let args = self.call_args()?;
                    return Ok(self.record(col, Expr::Call { name, args, line }));
                }
                // Postfix `i++` / `i--` in expression position: yields the value
                // before the update.
                if matches!(self.peek(), Tok::PlusPlus | Tok::MinusMinus) {
                    let inc = matches!(self.peek(), Tok::PlusPlus);
                    self.advance();
                    return Ok(Expr::PostIncDec { name, inc });
                }
                Ok(self.record(col, Expr::Var(name)))
            }
            other => Err(format!(
                "groovyrs: unexpected token {other} in expression on line {}",
                self.line()
            )),
        }
    }

    /// Parse `println`/`print` in either the parenthesised form `println(arg)`
    /// or the paren-less command form `println arg`.
    fn print_call(&mut self, name: &str) -> Result<Expr, String> {
        self.advance(); // println / print
        let newline = name == "println";
        // Parenthesised call.
        if self.is(&Tok::LParen) {
            self.advance();
            let arg = if self.is(&Tok::RParen) {
                None
            } else {
                Some(Box::new(self.expression()?))
            };
            self.eat(&Tok::RParen)?;
            return Ok(Expr::Println { newline, arg });
        }
        // Command form: a bare argument up to the statement terminator. With no
        // argument (`println` at end of line) it prints an empty line.
        let arg = if matches!(self.peek(), Tok::Nl | Tok::Semi | Tok::RBrace | Tok::Eof) {
            None
        } else {
            Some(Box::new(self.expression()?))
        };
        Ok(Expr::Println { newline, arg })
    }

    /// Parse a `[...]` literal: an empty list `[]`, the empty map `[:]`, a map
    /// `[k: v, ...]`, or a list `[a, b, ...]`. Whether it is a list or a map is
    /// decided by whether the first element is followed by `:`.
    fn list_or_map(&mut self) -> Result<Expr, String> {
        self.eat(&Tok::LBracket)?;
        self.skip_newlines();
        // Empty list.
        if self.is(&Tok::RBracket) {
            self.advance();
            return Ok(Expr::List(Vec::new()));
        }
        // The empty map literal is written `[:]`.
        if self.is(&Tok::Colon) {
            self.advance();
            self.eat(&Tok::RBracket)?;
            return Ok(Expr::Map(Vec::new()));
        }
        // Decide list vs map on the first entry: a `key:` prefix means a map.
        let first_key = self.map_key()?;
        if self.is(&Tok::Colon) {
            self.advance();
            self.skip_newlines();
            let first_val = self.expression()?;
            let mut entries = vec![(first_key, first_val)];
            self.skip_newlines();
            while self.is(&Tok::Comma) {
                self.advance();
                self.skip_newlines();
                if self.is(&Tok::RBracket) {
                    break;
                }
                let k = self.map_key()?;
                self.eat(&Tok::Colon)?;
                self.skip_newlines();
                let v = self.expression()?;
                entries.push((k, v));
                self.skip_newlines();
            }
            self.eat(&Tok::RBracket)?;
            return Ok(Expr::Map(entries));
        }
        // A list: the first key was actually the first element expression.
        let mut elems = vec![first_key];
        self.skip_newlines();
        while self.is(&Tok::Comma) {
            self.advance();
            self.skip_newlines();
            if self.is(&Tok::RBracket) {
                break;
            }
            elems.push(self.expression()?);
            self.skip_newlines();
        }
        self.eat(&Tok::RBracket)?;
        Ok(Expr::List(elems))
    }

    /// Parse a map-literal key. A bare identifier is a string constant (Groovy
    /// treats `[a: 1]` as key `"a"`); a parenthesised `(expr)` is a computed key;
    /// otherwise it is an ordinary expression (string/number literal key). When
    /// the `[...]` turns out to be a list, this is just the first element.
    fn map_key(&mut self) -> Result<Expr, String> {
        // A bare identifier immediately followed by `:` is a literal string key.
        if let Tok::Ident(name) = self.peek().clone() {
            if matches!(self.peek_at(1), Tok::Colon) {
                self.advance();
                return Ok(Expr::Str(name));
            }
        }
        self.expression()
    }

    /// Parse a closure literal `{ [params ->] statements }`. The `{` is the
    /// current token. With an explicit parameter list (`{ a, b -> ... }`) the
    /// names bind the closure's arguments; without one (`{ ... }`) the closure
    /// takes a single implicit parameter named `it`. The body is a statement
    /// list whose last expression is the closure's return value.
    fn closure_literal(&mut self) -> Result<Expr, String> {
        self.eat(&Tok::LBrace)?;
        self.skip_newlines();
        let explicit_params = self.has_closure_arrow();
        let mut defaults: Vec<(String, Expr)> = Vec::new();
        let mut varargs = false;
        let params = if explicit_params {
            let mut params = Vec::new();
            // `{ -> … }` declares an empty parameter list: no `it` is supplied.
            if self.is(&Tok::Arrow) {
                self.advance();
                self.skip_newlines();
                let body = self.block()?;
                return Ok(Expr::Closure {
                    params,
                    body,
                    explicit_params,
                    varargs,
                });
            }
            loop {
                // An optional type in front of the parameter (`int a`) — a
                // second identifier follows, so skip the type name. A varargs
                // parameter puts `...` between the two (`Object... xs`), and the
                // type is always written, so the `...` is looked for there.
                if matches!(self.peek(), Tok::Ident(_))
                    && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Ellipsis)
                {
                    self.advance();
                }
                if self.is(&Tok::Ellipsis) {
                    self.advance();
                    varargs = true;
                }
                let name = self.ident()?;
                // `{ a, b = 5 -> … }` — a default value for a trailing
                // parameter. It becomes a guard at the top of the body, because
                // a call that omits the argument arrives with it null.
                if self.is(&Tok::Assign) {
                    self.advance();
                    self.skip_newlines();
                    defaults.push((name.clone(), self.expression()?));
                }
                params.push(name);
                if self.is(&Tok::Comma) {
                    self.advance();
                    self.skip_newlines();
                    continue;
                }
                break;
            }
            self.eat(&Tok::Arrow)?;
            self.skip_newlines();
            params
        } else {
            Vec::new()
        };
        let mut body = self.block()?;
        // Prepend `if (p == null) p = <default>` for each defaulted parameter.
        // (A call that passes an explicit `null` therefore takes the default,
        // where Groovy — which generates one overload per arity — keeps null.)
        for (name, value) in defaults.into_iter().rev() {
            let line = body.first().map(|s| s.line).unwrap_or(0);
            body.insert(
                0,
                Stmt::new(
                    line,
                    StmtKind::If {
                        cond: Expr::Binary {
                            op: BinOp::Eq,
                            lhs: Box::new(Expr::Var(name.clone())),
                            rhs: Box::new(Expr::Null),
                        },
                        then: vec![Stmt::new(
                            line,
                            StmtKind::Assign {
                                name,
                                op: AssignOp::Assign,
                                value,
                            },
                        )],
                        els: Vec::new(),
                    },
                ),
            );
        }
        Ok(Expr::Closure {
            params,
            body,
            explicit_params,
            varargs,
        })
    }

    /// Lookahead at statement start: is the current `{` a closure with an
    /// explicit parameter list (`{ a, b -> ... }`) rather than a bare block? An
    /// implicit-`it` `{ ... }` at statement position stays a block (its value
    /// would be discarded anyway), so only the unambiguous arrow form is
    /// re-routed to the expression path.
    fn stmt_lbrace_is_closure(&self) -> bool {
        if !self.is(&Tok::LBrace) {
            return false;
        }
        let mut j = self.pos + 1;
        // Skip a leading newline run inside the brace.
        while matches!(self.toks.get(j).map(|t| &t.kind), Some(Tok::Nl)) {
            j += 1;
        }
        self.arrow_follows_params(j)
    }

    /// Lookahead: does the closure just entered open with an explicit parameter
    /// list? True when a run of identifiers/commas (the parameter names, with
    /// optional type words) is followed by `->` before anything else.
    fn has_closure_arrow(&self) -> bool {
        self.arrow_follows_params(self.pos)
    }

    /// Scan a candidate closure parameter list starting at token `from`: a run
    /// of names, commas and `= default` values, ending at `->`.
    fn arrow_follows_params(&self, from: usize) -> bool {
        let mut j = from;
        loop {
            match self.toks.get(j).map(|t| &t.kind) {
                // `Ellipsis` is the `...` of a varargs parameter, which sits
                // between the type and the name (`Object... xs`).
                Some(Tok::Ident(_)) | Some(Tok::Comma) | Some(Tok::Ellipsis) => j += 1,
                // A default value runs to the next top-level `,` or `->`.
                Some(Tok::Assign) => {
                    j += 1;
                    let mut depth = 0i32;
                    loop {
                        match self.toks.get(j).map(|t| &t.kind) {
                            Some(Tok::LParen | Tok::LBracket | Tok::LBrace) => depth += 1,
                            Some(Tok::RParen | Tok::RBracket | Tok::RBrace) => depth -= 1,
                            Some(Tok::Comma | Tok::Arrow) if depth == 0 => break,
                            None | Some(Tok::Nl | Tok::Semi | Tok::Eof) if depth == 0 => {
                                return false
                            }
                            None => return false,
                            _ => {}
                        }
                        j += 1;
                    }
                }
                Some(Tok::Arrow) => return true,
                _ => return false,
            }
        }
    }

    /// Parse a parenthesised argument list `( expr, expr, ... )` past the
    /// callee. The opening `(` is the current token; consumes through the
    /// closing `)`. Newlines after `(`, `,`, and before `)` continue the list.
    fn call_args(&mut self) -> Result<Vec<Expr>, String> {
        self.eat(&Tok::LParen)?;
        self.skip_newlines();
        let mut args = Vec::new();
        if !self.is(&Tok::RParen) {
            loop {
                args.push(self.expression()?);
                self.skip_newlines();
                if self.is(&Tok::Comma) {
                    self.advance();
                    self.skip_newlines();
                    continue;
                }
                break;
            }
        }
        self.eat(&Tok::RParen)?;
        Ok(args)
    }

    /// Take one level of the [`MAX_NESTING`] budget, or refuse the program.
    ///
    /// Groovy refuses too — its own parser overflows and reports
    /// `CompilationFailedException: parsing failed`. Measured on Apache Groovy
    /// 5.0.8 / JVM 21.0.12: `println ((((…1…))))` compiles at 500 nested parens
    /// and fails at 1000, and `println 1 + 1 + …` compiles at 1000 terms and
    /// fails at 2000. groovyrs's limit is above both, so nothing the reference
    /// accepts is refused here. What this replaces is not an error message but a
    /// `fatal runtime error: stack overflow` — an abort with no diagnostic and
    /// no exit status a caller can read.
    fn deepen(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            return Err(format!(
                "groovyrs: expression nesting is deeper than {MAX_NESTING} on line {}",
                self.line()
            ));
        }
        Ok(())
    }

    fn ident(&mut self) -> Result<String, String> {
        match self.advance() {
            Tok::Ident(s) => Ok(s),
            other => Err(format!(
                "groovyrs: expected an identifier but found {other} on line {}",
                self.line()
            )),
        }
    }
}

/// Map a token to a compound-assignment operator, if it is one.
fn assign_op(t: &Tok) -> Option<AssignOp> {
    Some(match t {
        Tok::Assign => AssignOp::Assign,
        Tok::PlusAssign => AssignOp::Add,
        Tok::MinusAssign => AssignOp::Sub,
        Tok::StarAssign => AssignOp::Mul,
        Tok::SlashAssign => AssignOp::Div,
        Tok::PercentAssign => AssignOp::Mod,
        // The bitwise / exponent forms carry the operator they apply, so the
        // compiler lowers them through the binary-operator path itself.
        Tok::ShlAssign => AssignOp::Bin(BinOp::Shl),
        Tok::ShrAssign => AssignOp::Bin(BinOp::Shr),
        Tok::UShrAssign => AssignOp::Bin(BinOp::UShr),
        Tok::AmpAssign => AssignOp::Bin(BinOp::BitAnd),
        Tok::PipeAssign => AssignOp::Bin(BinOp::BitOr),
        Tok::CaretAssign => AssignOp::Bin(BinOp::BitXor),
        Tok::PowerAssign => AssignOp::Bin(BinOp::Power),
        _ => return None,
    })
}

/// Binary operator + its binding power (higher binds tighter). The ladder is
/// Groovy's own: `||` < `&&` < `|` < `^` < `&` < `=~`/`==~` < equality <
/// relational (which `instanceof`, `in` and `as` join) < shifts < additive <
/// multiplicative < `**`.
fn binop(t: &Tok) -> Option<(BinOp, u8)> {
    Some(match t {
        Tok::OrOr => (BinOp::Or, 1),
        Tok::AndAnd => (BinOp::And, 2),
        Tok::Pipe => (BinOp::BitOr, 3),
        Tok::Caret => (BinOp::BitXor, 4),
        Tok::Amp => (BinOp::BitAnd, 5),
        // The regex operators bind looser than `==`, so `a == b =~ c` groups as
        // `(a == b) =~ c` — Groovy's own ordering.
        Tok::Match => (BinOp::Match, 6),
        Tok::MatchFull => (BinOp::MatchFull, 6),
        Tok::EqEq => (BinOp::Eq, 7),
        Tok::NotEq => (BinOp::Ne, 7),
        // `<=>` sits at Groovy's equality precedence, below relational ops.
        Tok::Spaceship => (BinOp::Cmp, 7),
        Tok::Lt => (BinOp::Lt, RELATIONAL_BP),
        Tok::Gt => (BinOp::Gt, RELATIONAL_BP),
        Tok::Le => (BinOp::Le, RELATIONAL_BP),
        Tok::Ge => (BinOp::Ge, RELATIONAL_BP),
        Tok::In => (BinOp::In, RELATIONAL_BP),
        Tok::Shl => (BinOp::Shl, 9),
        Tok::Shr => (BinOp::Shr, 9),
        Tok::UShr => (BinOp::UShr, 9),
        Tok::Plus => (BinOp::Add, 10),
        Tok::Minus => (BinOp::Sub, 10),
        Tok::Star => (BinOp::Mul, 11),
        Tok::Slash => (BinOp::Div, 11),
        Tok::Percent => (BinOp::Mod, 11),
        Tok::Power => (BinOp::Power, 12),
        _ => return None,
    })
}

/// The binding power of the relational band — where `instanceof`, `in` and the
/// `as` cast also sit.
const RELATIONAL_BP: u8 = 8;

/// The binding power a range endpoint parses at: one above the shift band,
/// where Groovy puts `..` itself.
const RANGE_OPERAND_BP: u8 = 10;

/// Strip the power-assert recording wrapper off an expression.
fn unrecorded(e: &Expr) -> &Expr {
    match e {
        Expr::Recorded { inner, .. } => unrecorded(inner),
        other => other,
    }
}

/// The variables Groovy lists in the *message* form's `Values:` clause, with the
/// column each was recorded at.
///
/// The rule, read off Apache Groovy 5.0.7: a variable is listed when it is a
/// direct operand of a binary expression — which in Groovy's AST includes the
/// subscript `l[0]` and `instanceof` — or of a `!`, recursively and left to
/// right. A variable reached any other way is not listed, so `s.length() == 9`
/// (a method-call receiver) and `-x == 1` (a unary-minus operand) report no
/// values while `l[0] == 9` and `x % 2 == 9` report `l` and `x`.
/// Whether an expression is a literal *number* (optionally negated). A range
/// endpoint the parser can see is a number is not a character, which lets
/// [`Parser::for_in`] drop the character branch from the walk.
fn literal_number(e: &Expr) -> Option<()> {
    match e {
        Expr::Int(..) | Expr::Float(_) | Expr::Dec(_) | Expr::BigInt(_) => Some(()),
        Expr::Unary { op: UnOp::Neg, rhs } => literal_number(rhs),
        _ => None,
    }
}

/// The literal machine integer an expression is, when the parser can see one.
/// Two such endpoints fix a range's direction at compile time.
fn literal_int(e: &Expr) -> Option<i64> {
    match e {
        Expr::Int(n, _) => Some(*n),
        Expr::Unary { op: UnOp::Neg, rhs } => literal_int(rhs).and_then(i64::checked_neg),
        _ => None,
    }
}

fn assert_value_names(cond: &Expr) -> Vec<(String, u32)> {
    fn walk(e: &Expr, out: &mut Vec<(String, u32)>) {
        // An operand that is a bare variable is what the clause names.
        if let Expr::Recorded { col, inner } = e {
            if let Expr::Var(n) = &**inner {
                out.push((n.clone(), *col));
                return;
            }
        }
        match unrecorded(e) {
            Expr::Binary { lhs, rhs, .. } => {
                walk(lhs, out);
                walk(rhs, out);
            }
            Expr::Index { recv, index, .. } => {
                walk(recv, out);
                walk(index, out);
            }
            Expr::InstanceOf { value, .. } => walk(value, out),
            Expr::Unary { op: UnOp::Not, rhs } => walk(rhs, out),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(cond, &mut out);
    out
}

/// Render `e` the way Groovy's `Expression.getText()` does — the canonical AST
/// text the `assert cond : message` form quotes, which is *not* the source: every
/// binary and `instanceof` is fully parenthesised, a unary wraps its operand, a
/// bare call gets its implicit `this` receiver, and a type name is qualified.
/// Verified against Apache Groovy 5.0.7.
fn expr_text(e: &Expr) -> String {
    let args_text = |args: &[Expr]| args.iter().map(expr_text).collect::<Vec<_>>().join(", ");
    match unrecorded(e) {
        Expr::Int(n, _) => n.to_string(),
        Expr::Float(f) => crate::decimal::format_double(*f),
        Expr::Dec(text) | Expr::BigInt(text) => text.clone(),
        // A `String` constant renders unquoted here, unlike the power form.
        Expr::Str(s) => s.clone(),
        Expr::GString(parts) => parts
            .iter()
            .map(|p| match p {
                GStringPart::Text(t) => t.clone(),
                GStringPart::Expr(inner) => format!("${}", expr_text(inner)),
            })
            .collect(),
        Expr::Bool(b) => b.to_string(),
        Expr::Null => "null".to_string(),
        Expr::Var(n) => n.clone(),
        Expr::Unary { op, rhs } => {
            let sym = match op {
                UnOp::Neg => "-",
                UnOp::Not => "!",
                UnOp::BitNot => "~",
            };
            format!("{sym}({})", expr_text(rhs))
        }
        Expr::Binary { op, lhs, rhs } => {
            format!(
                "({} {} {})",
                expr_text(lhs),
                binop_text(*op),
                expr_text(rhs)
            )
        }
        Expr::InstanceOf { value, class } => {
            format!(
                "({} instanceof {})",
                expr_text(value),
                qualified_type(class)
            )
        }
        Expr::Index { recv, index, .. } => format!("{}[{}]", expr_text(recv), expr_text(index)),
        Expr::Property { recv, name, .. } => format!("{}.{name}", expr_text(recv)),
        Expr::MethodCall {
            recv, method, args, ..
        } => format!("{}.{method}({})", expr_text(recv), args_text(args)),
        // A bare call is a method on the script, so Groovy prints its receiver.
        Expr::Call { name, args, .. } => format!("this.{name}({})", args_text(args)),
        Expr::CallValue { callee, args, .. } => {
            format!("{}({})", expr_text(callee), args_text(args))
        }
        Expr::List(items) => format!("[{}]", args_text(items)),
        Expr::Map(entries) if entries.is_empty() => "[:]".to_string(),
        Expr::Map(entries) => format!(
            "[{}]",
            entries
                .iter()
                .map(|(k, v)| format!("{}:{}", expr_text(k), expr_text(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Range {
            start,
            end,
            inclusive,
        } => format!(
            "({}{}{})",
            expr_text(start),
            if *inclusive { ".." } else { "..<" },
            expr_text(end)
        ),
        // The condition is wrapped once here and again by its own `getText`
        // when it is a binary, which is where Groovy's doubled parens come from.
        Expr::Ternary { cond, then, els } => format!(
            "({}) ? {} : {}",
            expr_text(cond),
            expr_text(then),
            expr_text(els)
        ),
        Expr::Elvis { lhs, rhs } => {
            format!("({0}) ? {0} : {1}", expr_text(lhs), expr_text(rhs))
        }
        Expr::PostIncDec { name, inc } => {
            format!("({name}{})", if *inc { "++" } else { "--" })
        }
        Expr::PreIncDec { name, inc } => {
            format!("({}{name})", if *inc { "++" } else { "--" })
        }
        Expr::New { class, args, .. } => format!("new {class}({})", args_text(args)),
        Expr::This => "this".to_string(),
        Expr::Super => "super".to_string(),
        Expr::Regex(src) => format!("~/{src}/"),
        other => format!("{other:?}"),
    }
}

/// The operator's source spelling, for [`expr_text`].
fn binop_text(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Match => "=~",
        BinOp::MatchFull => "==~",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Gt => ">",
        BinOp::Le => "<=",
        BinOp::Ge => ">=",
        BinOp::Cmp => "<=>",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::Power => "**",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::UShr => ">>>",
        BinOp::In => "in",
    }
}

/// The fully-qualified name Groovy prints for a type in an `instanceof`. A name
/// it does not recognise — a script-declared class — stays bare, which is what
/// Groovy does for a class in the default package.
fn qualified_type(name: &str) -> String {
    let pkg = match name {
        "String" | "CharSequence" | "Integer" | "Long" | "Short" | "Byte" | "Double" | "Float"
        | "Boolean" | "Number" | "Object" | "Character" => "java.lang",
        "List" | "ArrayList" | "Map" | "LinkedHashMap" | "HashMap" | "Collection" | "Iterable"
        | "Set" => "java.util",
        "BigDecimal" | "BigInteger" => "java.math",
        "GString" => "groovy.lang",
        _ if crate::throwable::is_builtin(name) => {
            return crate::throwable::qualified(name);
        }
        _ => return name.to_string(),
    };
    format!("{pkg}.{name}")
}

/// Parse the source of one `${ … }` / `$name` placeholder into an expression.
/// It runs the same lexer and expression grammar as the enclosing script, so an
/// interpolation is not a second, weaker language.
fn parse_interpolation(src: &str) -> Result<Expr, String> {
    let tokens = crate::lexer::lex(src)?;
    let mut p = Parser {
        toks: tokens,
        src: src.to_string(),
        pos: 0,
        tmp: 0,
        // A placeholder is lexed on its own, so its columns are relative to the
        // placeholder rather than the script — see BUGS.md.
        recording: None,
        pending: Vec::new(),
        depth: 0,
    };
    p.skip_newlines();
    let e = p.expression()?;
    p.skip_newlines();
    if !p.is(&Tok::Eof) {
        return Err(format!(
            "groovyrs: trailing input in string interpolation `{src}`"
        ));
    }
    Ok(e)
}

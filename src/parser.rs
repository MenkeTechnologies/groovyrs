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
            body.push(self.statement()?);
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
        let mut out = Vec::new();
        self.skip_terminators();
        while !self.is(&Tok::RBrace) && !self.is(&Tok::Eof) {
            out.push(self.statement()?);
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
            Ok(vec![self.statement()?])
        }
    }

    fn statement(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        let kind = match self.peek() {
            Tok::Ident(w) if w == "class" => self.class_decl()?,
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
        // `println`/`print` command statements are expression statements, not
        // declarations — resolve them before the two-idents-in-a-row heuristic.
        if matches!(self.peek(), Tok::Ident(n) if n == "println" || n == "print") {
            let e = self.expression()?;
            return Ok(StmtKind::Expr(e));
        }

        // `def name(params) { .. }` (a function) or `def name [= expr]` (a local).
        if self.is(&Tok::Def) {
            self.advance();
            let name = self.ident()?;
            if self.is(&Tok::LParen) {
                return self.function_def(name);
            }
            let init = self.opt_initializer()?;
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
        if self.is(&Tok::Assign) {
            if let Expr::Property { recv, name, .. } = lhs {
                self.advance();
                self.skip_newlines();
                let value = self.expression()?;
                return Ok(StmtKind::SetProperty {
                    recv: *recv,
                    name,
                    value,
                });
            }
            return Err(format!(
                "groovyrs: invalid assignment target on line {}",
                self.line()
            ));
        }
        Ok(StmtKind::Expr(lhs))
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

    /// Parse a class declaration `class Name [extends/implements ...] { members }`.
    /// The `class` keyword is the current token. `extends`/`implements` clauses
    /// are tolerated but ignored (single-level dynamic dispatch only). Members are
    /// fields (`def x [= init]` / `Type x [= init]`), constructors
    /// (`Name(params){..}`), and methods (`def m(params){..}` / typed).
    fn class_decl(&mut self) -> Result<StmtKind, String> {
        self.advance(); // `class`
        let name = self.ident()?;
        self.skip_newlines();
        // `extends Super` captures the direct superclass; `implements Y, Z` is
        // still tolerated and ignored (interfaces have no runtime effect here).
        let mut superclass = None;
        while !self.is(&Tok::LBrace) && !self.is(&Tok::Eof) {
            if matches!(self.peek(), Tok::Ident(k) if k == "extends") {
                self.advance();
                superclass = Some(self.ident()?);
                continue;
            }
            self.advance();
        }
        self.eat(&Tok::LBrace)?;
        let mut fields = Vec::new();
        let mut ctors = Vec::new();
        let mut methods = Vec::new();
        self.skip_terminators();
        while !self.is(&Tok::RBrace) && !self.is(&Tok::Eof) {
            self.class_member(&name, &mut fields, &mut ctors, &mut methods)?;
            self.expect_terminator()?;
            self.skip_terminators();
        }
        self.eat(&Tok::RBrace)?;
        Ok(StmtKind::Class {
            name,
            superclass,
            fields,
            ctors,
            methods,
        })
    }

    /// Parse one class member into the appropriate bucket. Leading visibility /
    /// `static` / `final` modifiers are skipped (dynamic runtime).
    fn class_member(
        &mut self,
        class_name: &str,
        fields: &mut Vec<Field>,
        ctors: &mut Vec<Ctor>,
        methods: &mut Vec<Method>,
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
        // Skip modifier keywords.
        while matches!(
            self.peek(),
            Tok::Ident(m) if matches!(
                m.as_str(),
                "public" | "private" | "protected" | "static" | "final"
                    | "abstract" | "synchronized" | "transient" | "volatile"
            )
        ) {
            self.advance();
        }
        // `def name` — a field or method.
        if self.is(&Tok::Def) {
            self.advance();
            let name = self.ident()?;
            if self.is(&Tok::LParen) {
                let params = self.param_list()?;
                let body = self.member_body()?;
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
                let body = self.member_body()?;
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
                    // `Type [| Type]* name`, or a bare `name`.
                    let mut words = vec![self.ident()?];
                    while self.is(&Tok::Pipe) {
                        self.advance();
                        words.push(self.ident()?);
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
        // C-style `for (init; cond; update)`.
        let init = if self.is(&Tok::Semi) {
            None
        } else {
            Some(Box::new(self.simple_statement()?))
        };
        self.eat(&Tok::Semi)?;
        let cond = if self.is(&Tok::Semi) {
            None
        } else {
            Some(self.expression()?)
        };
        self.eat(&Tok::Semi)?;
        let update = if self.is(&Tok::RParen) {
            None
        } else {
            Some(Box::new(self.simple_statement()?))
        };
        self.eat(&Tok::RParen)?;
        let body = self.braced_or_single()?;
        Ok(StmtKind::For {
            init,
            cond,
            update,
            body,
        })
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
    /// counting C-style `for`, evaluating `end` once into a synthetic temp so a
    /// body that mutates the endpoint still iterates the original range.
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
            other => {
                return Err(format!(
                    "groovyrs: only integer ranges (`a..b`, `a..<b`) are supported in `for-in`, found {other} on line {}",
                    self.line()
                ))
            }
        };
        let end = self.binary(0)?;
        self.eat(&Tok::RParen)?;
        let body = self.braced_or_single()?;

        let end_tmp = self.fresh_tmp("end");
        let cmp = if inclusive { BinOp::Le } else { BinOp::Lt };
        let loop_for = StmtKind::For {
            init: Some(Box::new(Stmt::new(
                line,
                StmtKind::Local {
                    ty: "def".into(),
                    name: var.clone(),
                    init: Some(start),
                },
            ))),
            cond: Some(Expr::Binary {
                op: cmp,
                lhs: Box::new(Expr::Var(var.clone())),
                rhs: Box::new(Expr::Var(end_tmp.clone())),
            }),
            update: Some(Box::new(Stmt::new(
                line,
                StmtKind::Expr(Expr::PostIncDec {
                    name: var,
                    inc: true,
                }),
            ))),
            body,
        };
        // Wrap in an always-true block so the endpoint temp and the loop share a
        // frame without introducing a Block node.
        Ok(StmtKind::If {
            cond: Expr::Bool(true),
            then: vec![
                Stmt::new(
                    line,
                    StmtKind::Local {
                        ty: "def".into(),
                        name: end_tmp,
                        init: Some(end),
                    },
                ),
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

    /// A range expression `a..b` (inclusive) or `a..<b` (half-open), sitting
    /// just above the binary operators. `a` and `b` are full binary
    /// expressions, so `0..n-1` parses as `0..(n-1)`.
    fn range_expr(&mut self) -> Result<Expr, String> {
        let start = self.binary(0)?;
        let inclusive = match self.peek() {
            Tok::DotDot => true,
            Tok::DotDotLt => false,
            _ => return Ok(start),
        };
        self.advance();
        self.skip_newlines();
        let end = self.binary(0)?;
        Ok(Expr::Range {
            start: Box::new(start),
            end: Box::new(end),
            inclusive,
        })
    }

    fn binary(&mut self, min_bp: u8) -> Result<Expr, String> {
        let mut lhs = self.unary()?;
        loop {
            // `value instanceof Type` — relational precedence (binding power 4),
            // recorded under the `instanceof` keyword's column.
            if 4 >= min_bp && matches!(self.peek(), Tok::Ident(k) if k == "instanceof") {
                let col = self.col_at(0);
                self.advance();
                let class = self.ident()?;
                lhs = self.record(
                    col,
                    Expr::InstanceOf {
                        value: Box::new(lhs),
                        class,
                    },
                );
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
            let rhs = self.binary(bp + 1)?;
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

    fn unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Tok::Minus | Tok::Not => {
                let op = if matches!(self.peek(), Tok::Minus) {
                    UnOp::Neg
                } else {
                    UnOp::Not
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
            Tok::Int(n) => {
                self.advance();
                Ok(Expr::Int(n))
            }
            Tok::Float(f) => {
                self.advance();
                Ok(Expr::Float(f))
            }
            Tok::Dec(text) => {
                self.advance();
                Ok(Expr::Dec(text))
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
                let class = self.ident()?;
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
                });
            }
            loop {
                // An optional type in front of the parameter (`int a`) — a
                // second identifier follows, so skip the type name.
                if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Ident(_))
                {
                    self.advance();
                }
                params.push(self.ident()?);
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
        let body = self.block()?;
        Ok(Expr::Closure {
            params,
            body,
            explicit_params,
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
        loop {
            match self.toks.get(j).map(|t| &t.kind) {
                Some(Tok::Ident(_)) | Some(Tok::Comma) => j += 1,
                Some(Tok::Arrow) => return true,
                _ => return false,
            }
        }
    }

    /// Lookahead: does the closure just entered open with an explicit parameter
    /// list? True when a run of identifiers/commas (the parameter names, with
    /// optional type words) is followed by `->` before anything else.
    fn has_closure_arrow(&self) -> bool {
        let mut j = self.pos;
        loop {
            match self.toks.get(j).map(|t| &t.kind) {
                Some(Tok::Ident(_)) | Some(Tok::Comma) => j += 1,
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
        _ => return None,
    })
}

/// Binary operator + its binding power (higher binds tighter).
fn binop(t: &Tok) -> Option<(BinOp, u8)> {
    Some(match t {
        Tok::OrOr => (BinOp::Or, 1),
        Tok::AndAnd => (BinOp::And, 2),
        Tok::EqEq => (BinOp::Eq, 3),
        Tok::NotEq => (BinOp::Ne, 3),
        // `<=>` sits at Groovy's equality precedence, below relational ops.
        Tok::Spaceship => (BinOp::Cmp, 3),
        Tok::Lt => (BinOp::Lt, 4),
        Tok::Gt => (BinOp::Gt, 4),
        Tok::Le => (BinOp::Le, 4),
        Tok::Ge => (BinOp::Ge, 4),
        Tok::Plus => (BinOp::Add, 5),
        Tok::Minus => (BinOp::Sub, 5),
        Tok::Star => (BinOp::Mul, 6),
        Tok::Slash => (BinOp::Div, 6),
        Tok::Percent => (BinOp::Mod, 6),
        _ => return None,
    })
}

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
        Expr::Int(n) => n.to_string(),
        Expr::Float(f) => crate::decimal::format_double(*f),
        Expr::Dec(text) => text.clone(),
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
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Gt => ">",
        BinOp::Le => "<=",
        BinOp::Ge => ">=",
        BinOp::Cmp => "<=>",
        BinOp::And => "&&",
        BinOp::Or => "||",
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

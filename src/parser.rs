//! A recursive-descent parser with precedence-climbing for expressions.
//!
//! Grammar (slice 1): a `.groovy` file is a sequence of top-level statements —
//! the Groovy *script* model, with no enclosing class or `main`. Statements are
//! separated by newlines or `;` (both optional-semicolon and explicit forms).
//! Covered: `def`/typed local declarations, script-binding assignments,
//! `if`/`while`, the C-style `for (;;)` and the `for (x in a..b)` range loop,
//! `break`/`continue`, and the `println`/`print` command calls (with or without
//! parentheses).

use crate::ast::*;
use crate::lexer::{Tok, Token};

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
        pos: 0,
        tmp: 0,
    };
    p.program()
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    /// Counter for synthetic temporaries (e.g. `for-in` range endpoints).
    tmp: usize,
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
            Tok::If => self.if_stmt()?,
            Tok::While => self.while_stmt()?,
            Tok::For => self.for_stmt()?,
            Tok::Return => {
                // A script's implicit return value is discarded; model a bare
                // `return`/`return <expr>` as a jump to the end of the script.
                self.advance();
                if matches!(self.peek(), Tok::Nl | Tok::Semi | Tok::RBrace | Tok::Eof) {
                    StmtKind::Break
                } else {
                    // Evaluate the returned expression for side effects, then end.
                    let _ = self.expression()?;
                    StmtKind::Break
                }
            }
            Tok::Break => {
                self.advance();
                StmtKind::Break
            }
            Tok::Continue => {
                self.advance();
                StmtKind::Continue
            }
            Tok::LBrace => {
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

        // `def name [= expr]`
        if self.is(&Tok::Def) {
            self.advance();
            let name = self.ident()?;
            let init = self.opt_initializer()?;
            return Ok(StmtKind::Local {
                ty: "def".into(),
                name,
                init,
            });
        }

        // Typed declaration: `Type name [= expr]` (two identifiers in a row).
        if self.looks_like_decl() {
            let ty = self.ident()?;
            let name = self.ident()?;
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

        // Fallback: an expression statement.
        Ok(StmtKind::Expr(self.expression()?))
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
        matches!(self.toks.get(j).map(|t| &t.kind), Some(Tok::Ident(_)))
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
        let start = self.expression()?;
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
        let end = self.expression()?;
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

    fn expression(&mut self) -> Result<Expr, String> {
        self.binary(0)
    }

    fn binary(&mut self, min_bp: u8) -> Result<Expr, String> {
        let mut lhs = self.unary()?;
        while let Some((op, bp)) = binop(self.peek()) {
            if bp < min_bp {
                break;
            }
            self.advance();
            self.skip_newlines(); // a binary operator may continue on the next line
            let rhs = self.binary(bp + 1)?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Tok::Minus => {
                self.advance();
                Ok(Expr::Unary {
                    op: UnOp::Neg,
                    rhs: Box::new(self.unary()?),
                })
            }
            Tok::Not => {
                self.advance();
                Ok(Expr::Unary {
                    op: UnOp::Not,
                    rhs: Box::new(self.unary()?),
                })
            }
            _ => self.primary(),
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.peek().clone() {
            Tok::Int(n) => {
                self.advance();
                Ok(Expr::Int(n))
            }
            Tok::Float(f) => {
                self.advance();
                Ok(Expr::Float(f))
            }
            Tok::Str(s) => {
                self.advance();
                Ok(Expr::Str(s))
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
            Tok::Ident(name) => {
                if name == "println" || name == "print" {
                    return self.print_call(&name);
                }
                let line = self.line();
                self.advance();
                if matches!(self.peek(), Tok::PlusPlus | Tok::MinusMinus) {
                    return Err(format!(
                        "groovyrs: `{name}++`/`--` is only supported as a statement yet (line {})",
                        self.line()
                    ));
                }
                // A call expression `name(args...)`. Slice 1 has no user methods,
                // so at compile time the only callees that resolve are the inline-
                // Rust FFI ones (the `__rust_compile` desugar target and the
                // barewords a `rust { ... }` block exports).
                if self.is(&Tok::LParen) {
                    let args = self.call_args()?;
                    return Ok(Expr::Call { name, args, line });
                }
                if self.is(&Tok::Dot) {
                    return Err(format!(
                        "groovyrs: property access on `{name}` is not supported yet (line {})",
                        self.line()
                    ));
                }
                Ok(Expr::Var(name))
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

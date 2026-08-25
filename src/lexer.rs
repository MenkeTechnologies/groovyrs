//! A hand-written Groovy lexer.
//!
//! Produces the token stream the parser consumes: identifiers/keywords,
//! integer/decimal/string literals, the operators and punctuation used by
//! declarations, expressions, and control statements, plus the `..`/`..<` range
//! operators. Line/block comments and a leading `#!` shebang are skipped.
//!
//! A double-quoted literal containing a `$` placeholder lexes to a
//! [`Tok::GStr`] carrying its parts — the `${ … }` bodies as raw source, which
//! the parser re-parses with the ordinary expression grammar. A literal with no
//! placeholder, and every single-quoted literal, stays a plain [`Tok::Str`].
//!
//! **Newlines are significant.** Groovy uses newlines as statement terminators
//! (semicolons are optional), so a [`Tok::Nl`] is emitted at each line break at
//! bracket depth 0. Inside `(...)`/`[...]` a newline is swallowed (a statement
//! cannot end mid-parenthesis), matching Groovy's line-continuation rules.
//! Consecutive blank lines collapse to a single `Nl`.

use std::fmt;

/// A lexical token with its 1-based source line (for error reporting).
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: Tok,
    pub line: u32,
    /// Byte offset of the token's first character in the source. `assert` needs
    /// it twice: to slice the condition's verbatim source text, and to derive
    /// the source *column* a power-assert value is rendered under.
    pub offset: usize,
}

/// One piece of a `GString`: either literal text or the *source text* of an
/// embedded expression, which the parser re-parses (so `${a + b}` and `$a.b`
/// reuse the ordinary expression grammar rather than a second one).
#[derive(Debug, Clone, PartialEq)]
pub enum GPart {
    Text(String),
    Expr(String),
}

/// Token kinds.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // literals & names
    /// An integer literal and the Java width it carries — see
    /// [`crate::ast::IntWidth`], which the suffix and the magnitude decide.
    Int(i64, crate::ast::IntWidth),
    /// A `d`/`f`-suffixed decimal literal: an IEEE double.
    Float(f64),
    /// An unsuffixed (or `g`-suffixed) decimal literal, kept as its exact source
    /// text because it is a `java.math.BigDecimal` — the literal's own scale
    /// (`1.50` has two fraction digits, `2.5e7` a scale of -6) is part of the
    /// value and no `f64` can carry it. See [`crate::decimal`].
    Dec(String),
    Str(String),
    /// An integer literal that is a `java.math.BigInteger`: `G`/`g`-suffixed, or
    /// unsuffixed with a magnitude past `Long`. Kept as its digits, because no
    /// machine integer can carry it.
    BigInt(String),
    /// An interpolating (double-quoted) string that actually contains a `$`
    /// placeholder — a Groovy `GString`. A double-quoted literal with no
    /// placeholder stays a plain [`Tok::Str`], so nothing about existing
    /// programs changes.
    GStr(Vec<GPart>),
    Ident(String),
    /// A `~/pattern/` regex literal — Groovy's `Pattern` (see `Expr::Regex`).
    Regex(String),
    // keywords
    Def,
    If,
    Else,
    While,
    Do,
    For,
    In,
    Switch,
    Case,
    Default,
    Assert,
    Return,
    Break,
    Continue,
    True,
    False,
    Null,
    New,
    // punctuation
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Semi,
    Comma,
    Colon,
    Dot,
    DotDot,      // `..` inclusive range
    DotDotLt,    // `..<` half-open range
    Ellipsis,    // `...` varargs parameter
    Arrow,       // `->` closure parameter separator
    Question,    // `?` ternary
    QuestionDot, // `?.` safe navigation
    StarDot,     // `*.` spread — apply the member to every element
    Elvis,       // `?:` elvis / null-coalescing
    /// `?=` elvis assignment: `a ?= b` writes `b` only when `a` is falsy.
    ElvisAssign,
    /// A significant newline (statement terminator at bracket depth 0).
    Nl,
    // operators
    Assign,
    PlusAssign,
    MinusAssign,
    StarAssign,
    SlashAssign,
    PercentAssign,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    PlusPlus,
    MinusMinus,
    EqEq,
    NotEq,
    Lt,
    Gt,
    Le,
    Ge,
    Spaceship, // `<=>` compareTo
    /// `=~` — Groovy's *find* operator, which builds a `java.util.regex.Matcher`
    /// over the left operand with the right one as its pattern.
    Match,
    /// `==~` — Groovy's *match* operator: whether the pattern matches the whole
    /// left operand. Lexed before `==` so `a ==~ b` is never `a == (~b)`.
    MatchFull,
    AndAnd,
    OrOr,
    Not,
    /// `|` — bitwise or, and the separator in a multi-catch type list
    /// (`catch (A | B e)`).
    Pipe,
    /// `**` — Groovy's power operator (right-associative).
    Power,
    Amp,   // `&` bitwise and
    Caret, // `^` bitwise xor
    Tilde, // `~` bitwise complement (a `~/…/` regex literal lexes separately)
    Shl,   // `<<` left shift / `leftShift`
    Shr,   // `>>` arithmetic right shift
    UShr,  // `>>>` unsigned right shift
    ShlAssign,   // `<<=`
    ShrAssign,   // `>>=`
    UShrAssign,  // `>>>=`
    AmpAssign,   // `&=`
    PipeAssign,  // `|=`
    CaretAssign, // `^=`
    PowerAssign, // `**=`
    At,    // `@` annotation marker (e.g. `@Override`)
    Eof,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Is the byte at `i` the start of this literal's closing delimiter — one quote
/// for an ordinary literal, three for a triple-quoted one?
fn at_string_end(bytes: &[u8], i: usize, quote: u8, triple: bool) -> bool {
    if bytes[i] != quote {
        return false;
    }
    !triple || (i + 2 < bytes.len() && bytes[i + 1] == quote && bytes[i + 2] == quote)
}

/// Lex `src` into a token vector terminated by `Tok::Eof`.
pub fn lex(src: &str) -> Result<Vec<Token>, String> {
    let bytes = src.as_bytes();
    let mut i = 0usize;
    let mut line = 1u32;
    let mut out: Vec<Token> = Vec::new();
    // Bracket nesting for `(`/`[`; a newline inside these is a continuation and
    // is not emitted as a statement terminator.
    let mut depth: i32 = 0;

    // Skip a leading `#!` shebang line.
    if bytes.starts_with(b"#!") {
        while i < bytes.len() && bytes[i] != b'\n' {
            i += 1;
        }
    }

    while i < bytes.len() {
        let c = bytes[i] as char;
        // Where this iteration's token starts. Whitespace and comments `continue`
        // before any push, so every token pushed below begins here.
        let tok_start = i;

        // newline: significant at depth 0 (statement terminator), else skipped.
        if c == '\n' {
            line += 1;
            i += 1;
            if depth == 0 && !matches!(out.last().map(|t| &t.kind), None | Some(Tok::Nl)) {
                out.push(Token {
                    kind: Tok::Nl,
                    line,
                    offset: tok_start,
                });
            }
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // comments
        if c == '/' && i + 1 < bytes.len() {
            match bytes[i + 1] as char {
                '/' => {
                    i += 2;
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                '*' => {
                    i += 2;
                    while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                        if bytes[i] == b'\n' {
                            line += 1;
                        }
                        i += 1;
                    }
                    i += 2; // consume closing */
                    continue;
                }
                _ => {}
            }
        }

        // identifiers & keywords
        if c.is_ascii_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < bytes.len() {
                let ch = bytes[i] as char;
                if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' {
                    i += 1;
                } else {
                    break;
                }
            }
            let word = &src[start..i];
            out.push(Token {
                kind: keyword_or_ident(word),
                line,
                offset: tok_start,
            });
            continue;
        }

        // numbers (int or decimal)
        if c.is_ascii_digit() {
            // Radix-prefixed integer literals: `0xFF`, `0b1010`, and the
            // leading-zero octal `011`. These have no fraction and no exponent,
            // so they are scanned here and never reach the decimal path. Groovy
            // reads the digits as a magnitude and gives the literal the smallest
            // type that holds it, exactly as it does for a decimal literal —
            // which is why `0xff` is an `Integer` and `0xFFFFFFFF` is the `Long`
            // `4294967295` rather than Java's `int` overflow error.
            if let Some((value, end, width)) = scan_radix_int(bytes, i, line)? {
                out.push(Token {
                    kind: Tok::Int(value, width),
                    line,
                    offset: tok_start,
                });
                i = end;
                continue;
            }
            let start = i;
            let mut is_float = false;
            i = skip_digits(bytes, i);
            // A `.` is a decimal point only when followed by a digit — otherwise
            // it is the `..`/`..<` range operator (`0..3`) or member access.
            if i < bytes.len()
                && bytes[i] == b'.'
                && i + 1 < bytes.len()
                && (bytes[i + 1] as char).is_ascii_digit()
            {
                is_float = true;
                i = skip_digits(bytes, i + 1);
            }
            // Exponent part: `2.5e7`, `9.999e-4`, `1E+3`. Groovy allows an
            // exponent with or without a fractional part, and it makes the
            // literal a decimal either way. The `e` is only consumed when a
            // valid exponent actually follows, so an identifier starting with
            // `e` is left alone.
            if i < bytes.len() && matches!(bytes[i], b'e' | b'E') {
                let mut j = i + 1;
                if j < bytes.len() && matches!(bytes[j], b'+' | b'-') {
                    j += 1;
                }
                if j < bytes.len() && (bytes[j] as char).is_ascii_digit() {
                    is_float = true;
                    i = skip_digits(bytes, j);
                }
            }
            // integer/long/float/double/BigDecimal suffixes are consumed. The
            // slice ends here rather than being alpha-trimmed, because trimming
            // trailing letters would also eat an exponent's own `e`. The suffix
            // decides the *type*: `f`/`F`/`d`/`D` make an IEEE double, while an
            // unsuffixed (or `g`/`G`-suffixed) decimal is a `BigDecimal` — which
            // is Groovy's default and what `1.50` / `2.5e7` mean.
            let num_end = i;
            let mut is_double = false;
            // An `L` suffix makes the literal a `Long` whatever its magnitude,
            // so `2000000000L + 2000000000L` is `4000000000` where the
            // unsuffixed form wraps to `-294967296`. A `G` suffix on an integer
            // literal asks for a `java.math.BigInteger` outright, however small
            // the value: `123G` is one.
            let mut width = crate::ast::IntWidth::Int;
            let mut big = false;
            if i < bytes.len()
                && matches!(
                    bytes[i],
                    b'L' | b'l' | b'f' | b'F' | b'd' | b'D' | b'g' | b'G'
                )
            {
                if matches!(bytes[i], b'f' | b'F' | b'd' | b'D') {
                    is_float = true;
                    is_double = true;
                }
                if matches!(bytes[i], b'L' | b'l') {
                    width = crate::ast::IntWidth::Long;
                }
                if matches!(bytes[i], b'g' | b'G') {
                    big = true;
                }
                i += 1;
            }
            // `1_000_000` is `1000000`: the separators are not part of the value,
            // and a decimal literal's text is its `BigDecimal` scale, so they
            // come out before either parse.
            let raw = &src[start..num_end];
            let stripped: String;
            let text = if raw.as_bytes().contains(&b'_') {
                stripped = raw.replace('_', "");
                stripped.as_str()
            } else {
                raw
            };
            if is_double {
                let v: f64 = text.parse().map_err(|_| {
                    format!("groovyrs: bad decimal literal `{text}` on line {line}")
                })?;
                out.push(Token {
                    kind: Tok::Float(v),
                    line,
                    offset: tok_start,
                });
            } else if is_float {
                // Validated at lex time so a malformed literal is a lex error,
                // but carried as text: the exact digits are the value.
                if crate::decimal::parse(text).is_none() {
                    return Err(format!(
                        "groovyrs: bad decimal literal `{text}` on line {line}"
                    ));
                }
                out.push(Token {
                    kind: Tok::Dec(text.to_string()),
                    line,
                    offset: tok_start,
                });
            } else {
                // A `G` suffix, or a magnitude past `Long`, makes the literal a
                // `java.math.BigInteger` — which no machine integer holds, so it
                // keeps its digits (`9223372036854775808 + 1` is
                // `9223372036854775809`, not a wrapped negative).
                match text.parse::<i64>() {
                    Ok(v) if !big => {
                        // An unsuffixed literal too large for 32 bits is a
                        // `Long` in Groovy, so its arithmetic must not wrap
                        // at 32.
                        if i32::try_from(v).is_err() {
                            width = crate::ast::IntWidth::Long;
                        }
                        out.push(Token {
                            kind: Tok::Int(v, width),
                            line,
                            offset: tok_start,
                        });
                    }
                    _ if text.bytes().all(|b| b.is_ascii_digit()) => out.push(Token {
                        kind: Tok::BigInt(text.to_string()),
                        line,
                        offset: tok_start,
                    }),
                    _ => {
                        return Err(format!(
                            "groovyrs: bad integer literal `{text}` on line {line}"
                        ))
                    }
                }
            }
            continue;
        }

        // String literals. A single-quoted literal is inert text; a
        // double-quoted one interpolates `$name` / `$a.b` / `${ expr }` and
        // becomes a `GStr` — but only when a placeholder is actually present, so
        // an ordinary `"text"` stays a plain `Str`.
        if c == '"' || c == '\'' {
            let quote = bytes[i];
            let interpolating = quote == b'"';
            // A triple-quoted literal (`"""…"""` / `'''…'''`) spans raw
            // newlines and ends only at a matching run of three quotes.
            let triple = i + 2 < bytes.len() && bytes[i + 1] == quote && bytes[i + 2] == quote;
            let start_line = line;
            i += if triple { 3 } else { 1 };
            let mut s = String::new();
            let mut parts: Vec<GPart> = Vec::new();
            while i < bytes.len() && !at_string_end(bytes, i, quote, triple) {
                // A raw newline is text inside a triple-quoted literal; a
                // single-quoted one has none (the scan below stops at the quote).
                if bytes[i] == b'\n' {
                    line += 1;
                }
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    // `\$` is a literal dollar, so an escape never interpolates.
                    let (text, next) = scan_escape(bytes, i + 1);
                    s.push_str(&text);
                    i = next;
                    continue;
                }
                if interpolating && bytes[i] == b'$' && i + 1 < bytes.len() {
                    // `${ expression }` — balanced braces, skipping nested
                    // string literals so `${ c ? "a" : "b" }` scans correctly.
                    if bytes[i + 1] == b'{' {
                        let (inner, next) = scan_braced(src, i + 2, line)?;
                        line += src[i..next].matches('\n').count() as u32;
                        if !s.is_empty() {
                            parts.push(GPart::Text(std::mem::take(&mut s)));
                        }
                        parts.push(GPart::Expr(inner));
                        i = next;
                        continue;
                    }
                    // `$name`, `$a.b.c` — a dotted *property* path. Groovy stops
                    // at anything else, so `"$n.toString()"` reads the property
                    // `n.toString` and leaves `()` as literal text.
                    if is_ident_start(bytes[i + 1]) {
                        let (path, next) = scan_dotted_path(src, i + 1);
                        if !s.is_empty() {
                            parts.push(GPart::Text(std::mem::take(&mut s)));
                        }
                        parts.push(GPart::Expr(path));
                        i = next;
                        continue;
                    }
                }
                // Decode a full UTF-8 char so multibyte literals survive.
                let ch = src[i..].chars().next().unwrap();
                if ch == '\n' {
                    line += 1;
                }
                s.push(ch);
                i += ch.len_utf8();
            }
            if i >= bytes.len() {
                return Err(format!(
                    "groovyrs: unterminated string literal on line {line}"
                ));
            }
            i += if triple { 3 } else { 1 }; // closing quote(s)
            let kind = if parts.is_empty() {
                Tok::Str(s)
            } else {
                if !s.is_empty() {
                    parts.push(GPart::Text(s));
                }
                Tok::GStr(parts)
            };
            // A literal is recorded at the line it *opened* on, so a paren-less
            // `println """a\nb"""` still reads as one statement.
            out.push(Token {
                kind,
                line: start_line,
                offset: tok_start,
            });
            continue;
        }

        // A `~/pattern/` regex literal — a `java.util.regex.Pattern`.
        if bytes[i] == b'~' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            let (parts, next, lines) = scan_slashy(src, i + 2, line)?;
            line += lines;
            // `~/…/` is a *pattern*, and Groovy compiles the literal text; an
            // interpolating one would have to build the source at run time,
            // which groovyrs does not model.
            let source: String = parts
                .iter()
                .map(|p| match p {
                    GPart::Text(t) => t.clone(),
                    GPart::Expr(e) => format!("${{{e}}}"),
                })
                .collect();
            out.push(Token {
                kind: Tok::Regex(source),
                line,
                offset: tok_start,
            });
            i = next;
            continue;
        }

        // A slashy string `/…/` — an ordinary `String` whose backslashes are
        // *not* escapes, which is why Groovy writes regexes in it: `/\d+/` is
        // the four characters `\d+`. It is only a literal where an expression
        // can begin; after a value the same `/` is division (`a / b`), which is
        // what [`ends_expression`] decides. Comments are matched above, so a
        // leading `//` or `/*` never reaches here.
        if c == '/' && !out.last().map(|t| &t.kind).is_some_and(ends_expression) {
            let (parts, next, lines) = scan_slashy(src, i + 1, line)?;
            let start_line = line;
            line += lines;
            let kind = match parts.as_slice() {
                [] => Tok::Str(String::new()),
                [GPart::Text(t)] => Tok::Str(t.clone()),
                _ => Tok::GStr(parts),
            };
            out.push(Token {
                kind,
                line: start_line,
                offset: tok_start,
            });
            i = next;
            continue;
        }

        // Operators & punctuation, longest match first. The lookahead slices go
        // through `str::get`, which answers `None` rather than panicking when
        // the window ends inside a multi-byte character — `println("١٢")` puts
        // one two bytes past a `(`, and no operator spans a non-ASCII byte
        // anyway.
        // `>>>=` is the only four-character operator, and it has to be matched
        // before `>>>` claims its first three characters and leaves a bare `=`.
        if src.get(i..i + 4).unwrap_or("") == ">>>=" {
            out.push(Token {
                kind: Tok::UShrAssign,
                line,
                offset: tok_start,
            });
            i += 4;
            continue;
        }
        let three = src.get(i..i + 3).unwrap_or("");
        // `==~` before both `==` and `=~`, so it is never split.
        if three == "==~" {
            out.push(Token {
                kind: Tok::MatchFull,
                line,
                offset: tok_start,
            });
            i += 3;
            continue;
        }
        // `...` — a varargs parameter. Checked ahead of `..` so the range
        // operator does not claim the first two dots and leave a stray `.`.
        if three == "..." {
            out.push(Token {
                kind: Tok::Ellipsis,
                line,
                offset: tok_start,
            });
            i += 3;
            continue;
        }
        if three == "..<" {
            out.push(Token {
                kind: Tok::DotDotLt,
                line,
                offset: tok_start,
            });
            i += 3;
            continue;
        }
        if three == "<=>" {
            out.push(Token {
                kind: Tok::Spaceship,
                line,
                offset: tok_start,
            });
            i += 3;
            continue;
        }
        // The three-character compound assignments, each ahead of the operator
        // whose two characters it starts with (`<<=` before `<<`, `**=` before
        // `**`, `>>=` before `>>`).
        if let Some(kind) = match three {
            "<<=" => Some(Tok::ShlAssign),
            ">>=" => Some(Tok::ShrAssign),
            "**=" => Some(Tok::PowerAssign),
            _ => None,
        } {
            out.push(Token {
                kind,
                line,
                offset: tok_start,
            });
            i += 3;
            continue;
        }
        if three == ">>>" {
            out.push(Token {
                kind: Tok::UShr,
                line,
                offset: tok_start,
            });
            i += 3;
            continue;
        }
        let two = src.get(i..i + 2).unwrap_or("");
        let (kind, adv) = match two {
            ".." => (Tok::DotDot, 2),
            "->" => (Tok::Arrow, 2),
            "?." => (Tok::QuestionDot, 2),
            // The spread operator `*.` — unambiguous, because Groovy has no
            // leading-dot decimal literal for a `*` to multiply by.
            "*." => (Tok::StarDot, 2),
            "?:" => (Tok::Elvis, 2),
            "?=" => (Tok::ElvisAssign, 2),
            "&=" => (Tok::AmpAssign, 2),
            "|=" => (Tok::PipeAssign, 2),
            "^=" => (Tok::CaretAssign, 2),
            "+=" => (Tok::PlusAssign, 2),
            "-=" => (Tok::MinusAssign, 2),
            "*=" => (Tok::StarAssign, 2),
            "/=" => (Tok::SlashAssign, 2),
            "%=" => (Tok::PercentAssign, 2),
            "++" => (Tok::PlusPlus, 2),
            "--" => (Tok::MinusMinus, 2),
            "=~" => (Tok::Match, 2),
            "==" => (Tok::EqEq, 2),
            "!=" => (Tok::NotEq, 2),
            "<=" => (Tok::Le, 2),
            ">=" => (Tok::Ge, 2),
            "&&" => (Tok::AndAnd, 2),
            "||" => (Tok::OrOr, 2),
            "**" => (Tok::Power, 2),
            "<<" => (Tok::Shl, 2),
            ">>" => (Tok::Shr, 2),
            _ => match c {
                '{' => (Tok::LBrace, 1),
                '}' => (Tok::RBrace, 1),
                '(' => {
                    depth += 1;
                    (Tok::LParen, 1)
                }
                ')' => {
                    depth = (depth - 1).max(0);
                    (Tok::RParen, 1)
                }
                '[' => {
                    depth += 1;
                    (Tok::LBracket, 1)
                }
                ']' => {
                    depth = (depth - 1).max(0);
                    (Tok::RBracket, 1)
                }
                ';' => (Tok::Semi, 1),
                ',' => (Tok::Comma, 1),
                ':' => (Tok::Colon, 1),
                '.' => (Tok::Dot, 1),
                '=' => (Tok::Assign, 1),
                '+' => (Tok::Plus, 1),
                '-' => (Tok::Minus, 1),
                '*' => (Tok::Star, 1),
                '/' => (Tok::Slash, 1),
                '%' => (Tok::Percent, 1),
                '<' => (Tok::Lt, 1),
                '>' => (Tok::Gt, 1),
                '?' => (Tok::Question, 1),
                '!' => (Tok::Not, 1),
                '|' => (Tok::Pipe, 1),
                '&' => (Tok::Amp, 1),
                '^' => (Tok::Caret, 1),
                // A `~/…/` regex literal is lexed above, so a `~` reaching here
                // is the bitwise complement.
                '~' => (Tok::Tilde, 1),
                '@' => (Tok::At, 1),
                other => {
                    return Err(format!(
                        "groovyrs: unexpected character `{other}` on line {line}"
                    ))
                }
            },
        };
        out.push(Token {
            kind,
            line,
            offset: tok_start,
        });
        i += adv;
    }

    out.push(Token {
        kind: Tok::Eof,
        line,
        offset: bytes.len(),
    });
    Ok(out)
}

/// Can a `/` directly after this token only be division?
///
/// This is the whole of Groovy's slashy-string disambiguation: `/` opens a
/// literal wherever an expression may begin, and divides wherever one has just
/// ended. The tokens below are the ones that end an expression — a name, a
/// literal, a closing bracket, or a postfix `++`/`--`. Everything else (an
/// operator, an opening bracket, a comma, a newline, a keyword, the start of the
/// file) leaves the parser expecting an operand, so the `/` starts a literal.
fn ends_expression(t: &Tok) -> bool {
    matches!(
        t,
        Tok::Ident(_)
            | Tok::Int(..)
            | Tok::Float(_)
            | Tok::Dec(_)
            | Tok::BigInt(_)
            | Tok::Str(_)
            | Tok::GStr(_)
            | Tok::Regex(_)
            | Tok::True
            | Tok::False
            | Tok::Null
            | Tok::RParen
            | Tok::RBracket
            | Tok::RBrace
            | Tok::PlusPlus
            | Tok::MinusMinus
    )
}

/// Scan a slashy literal's body, starting just past the opening `/`. Returns its
/// parts, the index just past the closing `/`, and how many newlines it spanned
/// (a slashy literal may cross lines).
///
/// Backslashes are **not** escapes here — `/\d+/` is the four characters `\d+`,
/// which is the entire reason Groovy has this literal. The single exception is
/// `\/`, the only way to write the delimiter itself. `$name` and `${ … }` still
/// interpolate, exactly as in a double-quoted literal.
fn scan_slashy(src: &str, start: usize, line: u32) -> Result<(Vec<GPart>, usize, u32), String> {
    let bytes = src.as_bytes();
    let mut i = start;
    let mut lines = 0u32;
    let mut text = String::new();
    let mut parts: Vec<GPart> = Vec::new();
    while i < bytes.len() && bytes[i] != b'/' {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            // Only the delimiter and `\uXXXX` are unescaped; every other
            // backslash is part of the pattern and both characters survive.
            // Groovy really does decode `\u` here — `/aAb/` is `aAb` — even
            // though `/\d+/` keeps its backslash.
            if bytes[i + 1] == b'/' {
                text.push('/');
                i += 2;
            } else if bytes[i + 1] == b'u' {
                let (decoded, next) = scan_escape(bytes, i + 1);
                if next == i + 2 {
                    // Not a well-formed `\uXXXX`; keep both characters.
                    text.push('\\');
                    text.push('u');
                } else {
                    text.push_str(&decoded);
                }
                i = next;
            } else {
                text.push('\\');
                text.push(bytes[i + 1] as char);
                i += 2;
            }
            continue;
        }
        if bytes[i] == b'$' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'{' {
                let (inner, next) = scan_braced(src, i + 2, line + lines)?;
                lines += src[i..next].matches('\n').count() as u32;
                if !text.is_empty() {
                    parts.push(GPart::Text(std::mem::take(&mut text)));
                }
                parts.push(GPart::Expr(inner));
                i = next;
                continue;
            }
            if is_ident_start(bytes[i + 1]) {
                let (path, next) = scan_dotted_path(src, i + 1);
                if !text.is_empty() {
                    parts.push(GPart::Text(std::mem::take(&mut text)));
                }
                parts.push(GPart::Expr(path));
                i = next;
                continue;
            }
        }
        let ch = src[i..].chars().next().unwrap();
        if ch == '\n' {
            lines += 1;
        }
        text.push(ch);
        i += ch.len_utf8();
    }
    if i >= bytes.len() {
        return Err(format!(
            "groovyrs: unterminated slashy string on line {line}"
        ));
    }
    if !text.is_empty() {
        parts.push(GPart::Text(text));
    }
    Ok((parts, i + 1, lines))
}

/// Can this byte begin an interpolation placeholder's identifier? `$` is a legal
/// Java identifier character but never starts or continues one *inside* a
/// GString placeholder — Groovy reads `"$a$a"` as two placeholders, not as the
/// single name `a$a` (verified against Apache Groovy 5.0.7).
fn is_ident_start(b: u8) -> bool {
    (b as char).is_ascii_alphabetic() || b == b'_'
}

/// Can this byte continue one?
fn is_ident_part(b: u8) -> bool {
    (b as char).is_ascii_alphanumeric() || b == b'_'
}

/// Scan the body of a `${ … }` placeholder, starting just past the `{`. Returns
/// the inner source and the index just past the matching `}`. Braces inside a
/// nested string literal do not count, so `${ "}" }` and `${ list.collect { it }
/// }` both scan correctly.
fn scan_braced(src: &str, start: usize, line: u32) -> Result<(String, usize), String> {
    let bytes = src.as_bytes();
    let mut i = start;
    let mut depth = 1usize;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok((src[start..i].to_string(), i + 1));
                }
            }
            q @ (b'"' | b'\'') => {
                i += 1;
                while i < bytes.len() && bytes[i] != q {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            _ => {}
        }
        i += 1;
    }
    Err(format!(
        "groovyrs: unterminated `${{` interpolation on line {line}"
    ))
}

/// Scan a `$name` / `$a.b.c` placeholder, starting at the first identifier
/// character. Returns the path source and the index just past it. A `.` only
/// continues the path when an identifier character follows it.
fn scan_dotted_path(src: &str, start: usize) -> (String, usize) {
    let bytes = src.as_bytes();
    let mut i = start;
    while i < bytes.len() && is_ident_part(bytes[i]) {
        i += 1;
    }
    loop {
        if i + 1 < bytes.len() && bytes[i] == b'.' && is_ident_start(bytes[i + 1]) {
            i += 1;
            while i < bytes.len() && is_ident_part(bytes[i]) {
                i += 1;
            }
        } else {
            return (src[start..i].to_string(), i);
        }
    }
}

fn keyword_or_ident(word: &str) -> Tok {
    match word {
        "def" => Tok::Def,
        "if" => Tok::If,
        "else" => Tok::Else,
        "while" => Tok::While,
        "do" => Tok::Do,
        "for" => Tok::For,
        "in" => Tok::In,
        "switch" => Tok::Switch,
        "case" => Tok::Case,
        "default" => Tok::Default,
        "assert" => Tok::Assert,
        "return" => Tok::Return,
        "break" => Tok::Break,
        "continue" => Tok::Continue,
        "true" => Tok::True,
        "false" => Tok::False,
        "null" => Tok::Null,
        "new" => Tok::New,
        _ => Tok::Ident(word.to_string()),
    }
}

/// Advance past a run of decimal digits, allowing Groovy's `_` group
/// separators between them (`1_000_000`). The separator is only skipped when a
/// digit follows, so `1_` ends the number at the `1` and leaves `_` to start an
/// identifier.
fn skip_digits(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() {
        let separator = bytes[i] == b'_' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit();
        if !bytes[i].is_ascii_digit() && !separator {
            break;
        }
        i += 1;
    }
    i
}

/// Scan a radix-prefixed integer literal at `i`: `0x`/`0X` hex, `0b`/`0B`
/// binary, or a leading-zero octal (`011` is `9`). Answers `None` when the text
/// at `i` is an ordinary decimal literal, which the caller scans instead.
///
/// The digits are read as an unsigned magnitude and the literal takes the
/// smallest Java type that holds it — Groovy's rule, under which `0xFF` is the
/// `Integer` `255` and `0xFFFFFFFF` is the `Long` `4294967295`. An explicit `L`
/// suffix forces `Long` and lets the magnitude occupy all 64 bits, so
/// `0xFFFFFFFFFFFFFFFFL` is `-1`.
fn scan_radix_int(
    bytes: &[u8],
    i: usize,
    line: u32,
) -> Result<Option<(i64, usize, crate::ast::IntWidth)>, String> {
    if bytes[i] != b'0' || i + 1 >= bytes.len() {
        return Ok(None);
    }
    let (radix, mut j) = match bytes[i + 1] {
        b'x' | b'X' => (16u32, i + 2),
        b'b' | b'B' => (2u32, i + 2),
        // A leading zero is octal only when octal digits follow. `0`, `0.5` and
        // `0e3` are decimal, and so is `08` — which Groovy rejects, and which
        // the decimal path parses as `8` rather than pretending to be octal.
        b'0'..=b'7' => (8u32, i + 1),
        _ => return Ok(None),
    };
    let start = j;
    let mut digits = String::new();
    while j < bytes.len() {
        let c = bytes[j] as char;
        if c == '_' {
            j += 1;
            continue;
        }
        if !c.is_digit(radix) {
            break;
        }
        digits.push(c);
        j += 1;
    }
    if digits.is_empty() {
        // `0x` with no digits, or a leading zero followed by `8`/`9`: not a
        // radix literal, so the decimal scanner takes it.
        if radix == 8 {
            return Ok(None);
        }
        return Err(format!(
            "groovyrs: bad integer literal `{}` on line {line}",
            String::from_utf8_lossy(&bytes[i..start])
        ));
    }
    // An octal run that turns out to be followed by a `.` or an exponent is a
    // decimal literal after all (`0.5`, `01.5`), so hand it back.
    if radix == 8 && j < bytes.len() && matches!(bytes[j], b'.' | b'e' | b'E' | b'8' | b'9') {
        return Ok(None);
    }
    let mut width = crate::ast::IntWidth::Int;
    if j < bytes.len() && matches!(bytes[j], b'L' | b'l' | b'g' | b'G') {
        width = crate::ast::IntWidth::Long;
        j += 1;
    }
    let magnitude = u64::from_str_radix(&digits, radix)
        .map_err(|_| format!("groovyrs: bad integer literal `0{digits}` on line {line}"))?;
    let value = match width {
        // An `L`-suffixed literal may fill all 64 bits, and the top bit is the
        // sign — Java's `0xFFFFFFFFFFFFFFFFL` is `-1`.
        crate::ast::IntWidth::Long => magnitude as i64,
        crate::ast::IntWidth::Int => match i64::try_from(magnitude) {
            Ok(v) => {
                if i32::try_from(v).is_err() {
                    width = crate::ast::IntWidth::Long;
                }
                v
            }
            // Past `Long` the literal is a `BigInteger`, which groovyrs does not
            // model — a clear lex error beats a silently wrapped value.
            Err(_) => {
                return Err(format!(
                    "groovyrs: integer literal `{}` exceeds Long on line {line}",
                    String::from_utf8_lossy(&bytes[i..j])
                ))
            }
        },
    };
    Ok(Some((value, j, width)))
}

/// Decode the escape starting at `bytes[i]` — the character *after* the
/// backslash — into its text, and answer the index just past it.
///
/// Groovy's set is Java's, and three parts of it were missing: `\b`/`\f`, the
/// one-to-three-digit **octal** escape (`"\101"` is `"A"`, not `"101"`), and
/// `\uXXXX`, which Groovy admits with any number of `u`s (`\uu0041`). Unlike
/// Java, Groovy does *not* preprocess `\u` over the whole source — `def A`
/// is a lexer error there — so this is a string-literal rule, not a source one.
///
/// A `\uXXXX` naming a high surrogate combines with a following `\uXXXX` low
/// surrogate into the one code point they encode. A surrogate with no partner
/// cannot be a Rust `char` and becomes the replacement character.
///
/// An escape naming none of those keeps the character (`"\q"` is `"q"`), which
/// is what Groovy's lexer does.
fn scan_escape(bytes: &[u8], i: usize) -> (String, usize) {
    let one = |c: char, next: usize| (c.to_string(), next);
    match bytes[i] {
        b'n' => one('\n', i + 1),
        b't' => one('\t', i + 1),
        b'r' => one('\r', i + 1),
        b'b' => one('\u{8}', i + 1),
        b'f' => one('\u{c}', i + 1),
        b'\\' => one('\\', i + 1),
        b'"' => one('"', i + 1),
        b'\'' => one('\'', i + 1),
        b'$' => one('$', i + 1),
        b'u' => {
            let (unit, mut j) = match scan_u_escape(bytes, i) {
                Some(v) => v,
                None => return one('u', i + 1),
            };
            // A high surrogate takes its partner with it; the pair is one code
            // point, and only the pair can be represented.
            if (0xD800..0xDC00).contains(&unit)
                && j + 1 < bytes.len()
                && bytes[j] == b'\\'
                && bytes[j + 1] == b'u'
            {
                if let Some((low, k)) = scan_u_escape(bytes, j + 1) {
                    if (0xDC00..0xE000).contains(&low) {
                        j = k;
                        let combined = String::from_utf16_lossy(&[unit, low]);
                        return (combined, j);
                    }
                }
            }
            (String::from_utf16_lossy(&[unit]), j)
        }
        b'0'..=b'7' => {
            // One to three octal digits, and never past `\377` — a leading digit
            // above 3 caps the run at two, exactly as Java's grammar does.
            let max = if bytes[i] <= b'3' { 3 } else { 2 };
            let mut j = i;
            let mut val = 0u32;
            while j < bytes.len() && j - i < max && (b'0'..=b'7').contains(&bytes[j]) {
                val = val * 8 + u32::from(bytes[j] - b'0');
                j += 1;
            }
            (char::from_u32(val).unwrap_or('\u{fffd}').to_string(), j)
        }
        other => one(other as char, i + 1),
    }
}

/// Read `u`+ followed by four hex digits at `bytes[i]` (which is the `u`), and
/// answer the code unit and the index just past the last digit.
fn scan_u_escape(bytes: &[u8], i: usize) -> Option<(u16, usize)> {
    let mut j = i;
    while j < bytes.len() && bytes[j] == b'u' {
        j += 1;
    }
    if j + 4 > bytes.len() {
        return None;
    }
    let hex = std::str::from_utf8(&bytes[j..j + 4]).ok()?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some((u16::from_str_radix(hex, 16).ok()?, j + 4))
}

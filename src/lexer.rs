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
    Int(i64),
    /// A `d`/`f`-suffixed decimal literal: an IEEE double.
    Float(f64),
    /// An unsuffixed (or `g`-suffixed) decimal literal, kept as its exact source
    /// text because it is a `java.math.BigDecimal` — the literal's own scale
    /// (`1.50` has two fraction digits, `2.5e7` a scale of -6) is part of the
    /// value and no `f64` can carry it. See [`crate::decimal`].
    Dec(String),
    Str(String),
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
    Arrow,       // `->` closure parameter separator
    Question,    // `?` ternary
    QuestionDot, // `?.` safe navigation
    StarDot,     // `*.` spread — apply the member to every element
    Elvis,       // `?:` elvis / null-coalescing
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
            let start = i;
            let mut is_float = false;
            while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                i += 1;
            }
            // A `.` is a decimal point only when followed by a digit — otherwise
            // it is the `..`/`..<` range operator (`0..3`) or member access.
            if i < bytes.len()
                && bytes[i] == b'.'
                && i + 1 < bytes.len()
                && (bytes[i + 1] as char).is_ascii_digit()
            {
                is_float = true;
                i += 1;
                while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                    i += 1;
                }
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
                    i = j;
                    while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                        i += 1;
                    }
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
                i += 1;
            }
            let text = &src[start..num_end];
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
                let v: i64 = text.parse().map_err(|_| {
                    format!("groovyrs: bad integer literal `{text}` on line {line}")
                })?;
                out.push(Token {
                    kind: Tok::Int(v),
                    line,
                    offset: tok_start,
                });
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
                    i += 1;
                    s.push(unescape(bytes[i] as char));
                    i += 1;
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

        // A `~/pattern/` regex literal. The `/` delimiter needs no escaping of
        // regex metacharacters, so only `\/` is special inside it — every other
        // backslash escape belongs to the pattern and passes through verbatim.
        if bytes[i] == b'~' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            let mut j = i + 2;
            let mut pattern = String::new();
            loop {
                if j >= bytes.len() || bytes[j] == b'\n' {
                    return Err(format!(
                        "groovyrs: unterminated regex literal on line {line}"
                    ));
                }
                if bytes[j] == b'\\' && j + 1 < bytes.len() {
                    if bytes[j + 1] == b'/' {
                        pattern.push('/');
                    } else {
                        pattern.push('\\');
                        pattern.push(bytes[j + 1] as char);
                    }
                    j += 2;
                    continue;
                }
                if bytes[j] == b'/' {
                    j += 1;
                    break;
                }
                let ch = src[j..].chars().next().unwrap();
                pattern.push(ch);
                j += ch.len_utf8();
            }
            out.push(Token {
                kind: Tok::Regex(pattern),
                line,
                offset: tok_start,
            });
            i = j;
            continue;
        }

        // operators & punctuation (longest match first)
        let three = if i + 2 < bytes.len() {
            &src[i..i + 3]
        } else {
            ""
        };
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
        if three == ">>>" {
            out.push(Token {
                kind: Tok::UShr,
                line,
                offset: tok_start,
            });
            i += 3;
            continue;
        }
        let two = if i + 1 < bytes.len() {
            &src[i..i + 2]
        } else {
            ""
        };
        let (kind, adv) = match two {
            ".." => (Tok::DotDot, 2),
            "->" => (Tok::Arrow, 2),
            "?." => (Tok::QuestionDot, 2),
            // The spread operator `*.` — unambiguous, because Groovy has no
            // leading-dot decimal literal for a `*` to multiply by.
            "*." => (Tok::StarDot, 2),
            "?:" => (Tok::Elvis, 2),
            "+=" => (Tok::PlusAssign, 2),
            "-=" => (Tok::MinusAssign, 2),
            "*=" => (Tok::StarAssign, 2),
            "/=" => (Tok::SlashAssign, 2),
            "%=" => (Tok::PercentAssign, 2),
            "++" => (Tok::PlusPlus, 2),
            "--" => (Tok::MinusMinus, 2),
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

fn unescape(c: char) -> char {
    match c {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        '0' => '\0',
        '\\' => '\\',
        '"' => '"',
        '\'' => '\'',
        '$' => '$',
        other => other,
    }
}

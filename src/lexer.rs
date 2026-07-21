//! A hand-written Groovy lexer.
//!
//! Produces the token stream the parser consumes. Covers the slice-1 surface:
//! identifiers/keywords, integer/decimal/string/char literals, the operators and
//! punctuation used by declarations, expressions, and control statements, plus
//! the `..`/`..<` range operators. Line/block comments and a leading `#!`
//! shebang are skipped.
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
}

/// Token kinds.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // literals & names
    Int(i64),
    Float(f64),
    Str(String),
    Ident(String),
    // keywords
    Def,
    If,
    Else,
    While,
    For,
    In,
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
    DotDot,   // `..` inclusive range
    DotDotLt, // `..<` half-open range
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
    AndAnd,
    OrOr,
    Not,
    Eof,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
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

        // newline: significant at depth 0 (statement terminator), else skipped.
        if c == '\n' {
            line += 1;
            i += 1;
            if depth == 0 && !matches!(out.last().map(|t| &t.kind), None | Some(Tok::Nl)) {
                out.push(Token {
                    kind: Tok::Nl,
                    line,
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
            // integer/long/float/double/BigDecimal suffixes are accepted and dropped
            if i < bytes.len()
                && matches!(
                    bytes[i],
                    b'L' | b'l' | b'f' | b'F' | b'd' | b'D' | b'g' | b'G'
                )
            {
                if matches!(bytes[i], b'f' | b'F' | b'd' | b'D') {
                    is_float = true;
                }
                i += 1;
            }
            let text = src[start..i].trim_end_matches(|ch: char| ch.is_ascii_alphabetic());
            if is_float {
                let v: f64 = text.parse().map_err(|_| {
                    format!("groovyrs: bad decimal literal `{text}` on line {line}")
                })?;
                out.push(Token {
                    kind: Tok::Float(v),
                    line,
                });
            } else {
                let v: i64 = text.parse().map_err(|_| {
                    format!("groovyrs: bad integer literal `{text}` on line {line}")
                })?;
                out.push(Token {
                    kind: Tok::Int(v),
                    line,
                });
            }
            continue;
        }

        // string literals (single- and double-quoted; GString interpolation is
        // not evaluated in slice 1 — the raw text is kept)
        if c == '"' || c == '\'' {
            let quote = bytes[i];
            i += 1;
            let mut s = String::new();
            while i < bytes.len() && bytes[i] != quote {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 1;
                    s.push(unescape(bytes[i] as char));
                    i += 1;
                } else {
                    // Decode a full UTF-8 char so multibyte literals survive.
                    let ch = src[i..].chars().next().unwrap();
                    if ch == '\n' {
                        line += 1;
                    }
                    s.push(ch);
                    i += ch.len_utf8();
                }
            }
            if i >= bytes.len() {
                return Err(format!(
                    "groovyrs: unterminated string literal on line {line}"
                ));
            }
            i += 1; // closing quote
            out.push(Token {
                kind: Tok::Str(s),
                line,
            });
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
                '!' => (Tok::Not, 1),
                other => {
                    return Err(format!(
                        "groovyrs: unexpected character `{other}` on line {line}"
                    ))
                }
            },
        };
        out.push(Token { kind, line });
        i += adv;
    }

    out.push(Token {
        kind: Tok::Eof,
        line,
    });
    Ok(out)
}

fn keyword_or_ident(word: &str) -> Tok {
    match word {
        "def" => Tok::Def,
        "if" => Tok::If,
        "else" => Tok::Else,
        "while" => Tok::While,
        "for" => Tok::For,
        "in" => Tok::In,
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

//! `java.util.regex` on `fancy-regex`.
//!
//! Java's regular expressions are a *backtracking* flavour: they carry
//! backreferences (`\1`, `\k<n>`) and lookaround (`(?<=…)`), both of which the
//! `regex` crate's linear-time automaton rejects by construction. `fancy-regex`
//! layers a backtracking VM over that automaton for exactly those constructs,
//! which makes it the engine whose *expressive power* matches Java's.
//!
//! Matching power is not the same as matching *meaning*, though, and the whole
//! point of this module is the gap between the two. Java and `fancy-regex` share
//! a large common core that maps one-for-one (literals, groups, classes,
//! quantifiers, `\d`/`\w`/`\s`, the anchors, lookaround, backreferences, inline
//! flags), a smaller set that maps mechanically once rewritten (`\Q…\E`,
//! `\h`/`\v`/`\R`, `\Z`, the POSIX `\p{Alpha}` names), and a tail that does not
//! map at all (possessive quantifiers, atomic groups, `\G`, Unicode *blocks*).
//! Handing the tail to the engine anyway would compile — and quietly answer a
//! different question. So [`translate`] rewrites the middle set and **rejects**
//! the tail by name, which keeps groovyrs's standing rule: an unsupported
//! construct is an error, never a wrong answer.
//!
//! On top of the engine this module implements the three `String` methods'
//! *specified* behaviour, which is not what a naive call to the engine gives:
//! `split`'s trailing-empty removal and its leading zero-width-match rule,
//! `replaceAll`/`replaceFirst`'s `$n`/`${name}` replacement grammar with `\`
//! escaping (Java's, not the `regex` crate's), and `matches`'s whole-region
//! anchoring.
//!
//! Groovy calls `java.util.regex` directly — `~/…/` is a `Pattern`, `=~` is a
//! `Matcher`, `==~` is `Matcher.matches()` — so all of the above applies to it
//! unchanged. What Groovy adds on top is a *stateful cursor*: [`Pattern::find`]
//! backs the `Matcher.find()`/`group(n)` walk, which advances through the
//! subject one match at a time.

use fancy_regex::Regex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// A compiled Java pattern.
/// A compiled pattern, or the `PatternSyntaxException` message its source
/// produced. Shared out of the module `CACHE` by handle so a repeated call in a loop
/// neither recompiles nor reallocates.
pub type Compiled = Rc<Result<Pattern, String>>;

pub struct Pattern {
    re: Regex,
}

/// One match: its byte span in the subject, and the text of every group in
/// order with group 0 (the whole match) first. A group that did not participate
/// is `None`, which is Java's `null` from `Matcher.group(n)`.
#[derive(Clone, Debug)]
pub struct Match {
    pub start: usize,
    pub end: usize,
    pub groups: Vec<Option<String>>,
}

thread_local! {
    /// Compiled patterns, keyed by `(source, anchored)`. `String.split` and
    /// friends take the pattern *source* on every call, so a loop over a million
    /// lines would otherwise pay the compile each time; Java's own
    /// `String.matches` has the same shape and the same problem, and real
    /// programs write it in loops anyway.
    static CACHE: RefCell<HashMap<(String, bool), Compiled>> =
        RefCell::new(HashMap::new());
}

/// Compile a `java.util.regex` pattern for searching (`split`, `replaceAll`).
///
/// `Err` carries the message of a `PatternSyntaxException`: either the
/// translation refusing a construct it cannot reproduce, or the engine refusing
/// a malformed one.
pub fn compile(source: &str) -> Compiled {
    cached(source, false)
}

/// Compile a pattern anchored to the **whole** input, which is what
/// `String.matches` matches against.
///
/// The anchors are `\A(?:…)\z`, not `^…$`: Java's default-mode `$` also matches
/// *before* a final line terminator, so `"a\n".matches("a")` would come out true
/// where Java says false.
pub fn compile_whole(source: &str) -> Compiled {
    cached(source, true)
}

fn cached(source: &str, anchored: bool) -> Compiled {
    let key = (source.to_string(), anchored);
    CACHE.with(|c| {
        if let Some(hit) = c.borrow().get(&key) {
            return Rc::clone(hit);
        }
        let built = Rc::new(build(source, anchored));
        c.borrow_mut().insert(key, Rc::clone(&built));
        built
    })
}

fn build(source: &str, anchored: bool) -> Result<Pattern, String> {
    let translated = translate(source)?;
    let final_src = if anchored {
        format!(r"\A(?:{translated})\z")
    } else {
        translated
    };
    match Regex::new(&final_src) {
        Ok(re) => Ok(Pattern { re }),
        Err(e) => Err(format!("{e} near index 0\n{source}")),
    }
}

impl Pattern {
    /// `String.matches(regex)` — true when the whole-input pattern from
    /// [`compile_whole`] matches.
    pub fn matches_whole(&self, text: &str) -> Result<bool, String> {
        self.re.is_match(text).map_err(|e| e.to_string())
    }

    /// `String.split(regex, limit)`.
    ///
    /// Java's rules, none of which a bare engine split gives:
    ///   * A zero-width match at index 0 produces no leading empty field
    ///     (Java 8+), so `"abc".split("")` is `["a", "b", "c"]`, not
    ///     `["", "a", "b", "c"]`.
    ///   * `limit == 0` removes *trailing* empty fields but keeps interior ones.
    ///   * `limit > 0` applies the pattern at most `limit - 1` times and keeps
    ///     every field; `limit < 0` keeps every field with no cap.
    ///   * No match at all yields a one-element array holding the whole input,
    ///     so `"".split(",")` has length 1.
    pub fn split(&self, text: &str, limit: i64) -> Result<Vec<String>, String> {
        let mut parts: Vec<String> = Vec::new();
        let mut start = 0usize; // byte index where the current field begins
        let mut search = 0usize; // byte index the next search starts at
        let mut matched = false;
        while search <= text.len() {
            if limit > 0 && parts.len() as i64 == limit - 1 {
                break;
            }
            let Some(m) = self
                .re
                .find_from_pos(text, search)
                .map_err(|e| e.to_string())?
            else {
                break;
            };
            // A zero-width match at the very start of the input contributes no
            // empty leading field, and must not spin in place.
            if m.start() == 0 && m.end() == 0 {
                search = next_char_boundary(text, 0);
                continue;
            }
            matched = true;
            parts.push(text[start..m.start()].to_string());
            start = m.end();
            search = if m.end() == m.start() {
                next_char_boundary(text, m.end())
            } else {
                m.end()
            };
        }
        if !matched {
            return Ok(vec![text.to_string()]);
        }
        parts.push(text[start..].to_string());
        if limit == 0 {
            while parts.last().is_some_and(|p| p.is_empty()) {
                parts.pop();
            }
        }
        Ok(parts)
    }

    /// One match at or after byte offset `from`: its span and the text of every
    /// group (`None` for a group that did not participate, group 0 first).
    ///
    /// This is `Matcher.find()`'s primitive. The caller keeps the cursor, which
    /// is what makes a `Matcher` stateful the way Java's is — `m.find()` twice
    /// yields two different matches, and `m.group(1)` reads whichever the last
    /// `find` landed on.
    pub fn find(&self, text: &str, from: usize) -> Result<Option<Match>, String> {
        if from > text.len() {
            return Ok(None);
        }
        let Some(caps) = self
            .re
            .captures_from_pos(text, from)
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        let whole = caps.get(0).expect("group 0 always participates");
        Ok(Some(Match {
            start: whole.start(),
            end: whole.end(),
            groups: (0..caps.len())
                .map(|i| caps.get(i).map(|m| m.as_str().to_string()))
                .collect(),
        }))
    }

    /// The number of capturing groups, which is `Matcher.groupCount()` — group 0
    /// (the whole match) is not counted.
    pub fn group_count(&self) -> usize {
        self.re.captures_len().saturating_sub(1)
    }

    /// The index of the group `(?<name>…)` declares, which is what
    /// `Matcher.group(String)` reads. `None` for a name the pattern does not
    /// declare — Java's `IllegalArgumentException`, which the caller raises.
    pub fn group_index(&self, name: &str) -> Option<usize> {
        self.re
            .capture_names()
            .position(|declared| declared == Some(name))
    }

    /// Every match in `text`, walked left to right. A zero-width match advances
    /// by one character so the walk terminates.
    pub fn find_all(&self, text: &str) -> Result<Vec<Match>, String> {
        let mut out = Vec::new();
        let mut at = 0usize;
        while let Some(m) = self.find(text, at)? {
            at = if m.end == m.start {
                next_char_boundary(text, m.end)
            } else {
                m.end
            };
            out.push(m);
        }
        Ok(out)
    }

    /// `String.replaceAll` with a *closure* replacement, which Groovy allows in
    /// place of a replacement string. The closure receives the whole match when
    /// the pattern has no groups, and `(whole, g1, g2, …)` when it has — so
    /// `"a1b2".replaceAll(/(\d)/) { all, d -> "<$d>" }` is `a<1>b<2>`.
    ///
    /// `replace` is called with each match's groups and answers the text to
    /// substitute; an `Err` from it (a fault inside the closure body) stops the
    /// walk and propagates.
    pub fn replace_with(
        &self,
        text: &str,
        first_only: bool,
        mut replace: impl FnMut(&Match) -> Result<String, String>,
    ) -> Result<String, String> {
        let mut out = String::new();
        let mut last = 0usize;
        for m in self.find_all(text)? {
            out.push_str(&text[last..m.start]);
            out.push_str(&replace(&m)?);
            last = m.end;
            if first_only {
                break;
            }
        }
        out.push_str(&text[last..]);
        Ok(out)
    }

    /// `String.replaceAll` / `replaceFirst`. `replacement` is Java's replacement
    /// grammar, expanded by `expand`.
    pub fn replace(
        &self,
        text: &str,
        replacement: &str,
        first_only: bool,
    ) -> Result<String, String> {
        let mut out = String::new();
        let mut last = 0usize;
        let mut search = 0usize;
        while search <= text.len() {
            let Some(caps) = self
                .re
                .captures_from_pos(text, search)
                .map_err(|e| e.to_string())?
            else {
                break;
            };
            let m = caps.get(0).expect("group 0 always participates");
            out.push_str(&text[last..m.start()]);
            out.push_str(&expand(replacement, &caps)?);
            last = m.end();
            search = if m.end() == m.start() {
                next_char_boundary(text, m.end())
            } else {
                m.end()
            };
            if first_only {
                break;
            }
        }
        out.push_str(&text[last..]);
        Ok(out)
    }
}

/// The next `char` boundary at or after `i`, used to step past a zero-width
/// match without splitting a multi-byte character.
fn next_char_boundary(text: &str, i: usize) -> usize {
    match text[i..].chars().next() {
        Some(c) => i + c.len_utf8(),
        None => text.len() + 1,
    }
}

/// Expand Java's replacement grammar against a match.
///
/// `$n` is a group reference whose digits extend only as far as an *existing*
/// group (Java: "the first number is always part of the reference; subsequent
/// digits are incorporated if they would form a legal reference"), `${name}`
/// names one, and `\` escapes the next character literally. A reference to a
/// group the pattern does not have is an `IndexOutOfBoundsException` in Java; a
/// trailing `\` or a bare `$` is an `IllegalArgumentException`. Both surface
/// here as an `Err`, which the caller raises with the right class.
fn expand(replacement: &str, caps: &fancy_regex::Captures) -> Result<String, String> {
    let mut out = String::new();
    let mut it = replacement.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next() {
                Some(e) => out.push(e),
                None => return Err("character to be escaped is missing".to_string()),
            },
            '$' => {
                if it.peek() == Some(&'{') {
                    it.next();
                    let mut name = String::new();
                    loop {
                        match it.next() {
                            Some('}') => break,
                            Some(ch) => name.push(ch),
                            None => {
                                return Err(
                                    "named capturing group is missing trailing '}'".to_string()
                                )
                            }
                        }
                    }
                    match caps.name(&name) {
                        Some(m) => out.push_str(m.as_str()),
                        None => {
                            return Err(format!("No group with name {{{name}}}"));
                        }
                    }
                    continue;
                }
                // Greedy over digits, but only as far as a group that exists.
                let mut n: usize = match it.peek().and_then(|c| c.to_digit(10)) {
                    Some(d) => {
                        it.next();
                        d as usize
                    }
                    None => return Err("Illegal group reference".to_string()),
                };
                if n >= caps.len() {
                    return Err(format!("No group {n}"));
                }
                while let Some(d) = it.peek().and_then(|c| c.to_digit(10)) {
                    let wider = n * 10 + d as usize;
                    if wider >= caps.len() {
                        break;
                    }
                    n = wider;
                    it.next();
                }
                if let Some(m) = caps.get(n) {
                    out.push_str(m.as_str());
                }
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

// ── Java → fancy-regex translation ───────────────────────────────────────
//
// The two flavours share a large core, but their *defaults* differ in ways that
// change answers rather than erroring:
//
//   * `\d`/`\w`/`\s`/`\b` are ASCII-only in Java (UNICODE_CHARACTER_CLASS is off
//     by default) and Unicode-aware in the engine, so
//     `"١٢".replaceAll("\\d", ".")` changes nothing in Java and
//     everything in the engine.
//   * `(?i)` folds ASCII only in Java (UNICODE_CASE is off by default) and folds
//     Unicode in the engine, so `"Ä".matches("(?i)ä")` is false in
//     Java and true in the engine.
//   * `.` excludes Java's five line terminators; the engine excludes only `\n`.
//   * `$` in Java's default mode matches *before* a final line terminator; the
//     engine's `$` matches only at the very end.
//
// Every one of those is rewritten below rather than documented, because each is
// a silently different answer for a program that compiles on both sides.

/// The characters Java calls line terminators — what `.` excludes and what `$`
/// may sit in front of. A vertical tab and a form feed are *not* among them, so
/// `.` matches those.
const TERMINATORS: &str = "\\n\\r\\u{85}\\u{2028}\\u{2029}";
/// Java's `.` outside DOTALL.
const DOT: &str = "[^\\n\\r\\u{85}\\u{2028}\\u{2029}]";
/// Java's `\h` (horizontal whitespace).
const HORIZ: &str = "[ \\t\\u{a0}\\u{1680}\\u{180e}\\u{2000}-\\u{200a}\\u{202f}\\u{205f}\\u{3000}]";
/// Java's `\v` (vertical whitespace).
const VERT: &str = "[\\n\\u{b}\\u{c}\\r\\u{85}\\u{2028}\\u{2029}]";
/// Java's `\R` (any line break) — wider than [`TERMINATORS`], and CRLF first so
/// it is never split in two.
const LINEBREAK: &str = "(?:\\r\\n|[\\n\\u{b}\\u{c}\\r\\u{85}\\u{2028}\\u{2029}])";
/// The ASCII classes Java's `\w`, `\d`, and `\s` name. The engine's POSIX
/// classes are ASCII-only, which is exactly Java's default.
const WORD: &str = "[:word:]";
const DIGIT: &str = "[:digit:]";
const SPACE: &str = "[:space:]";
/// Java's `\b`, spelled out. The engine's own `\b` is Unicode-aware and cannot
/// be switched to ASCII — it rejects `(?-u:...)` outright — so the boundary is
/// built from the lookaround fancy-regex does provide.
const WORD_BOUNDARY: &str = "(?:(?<![[:word:]])(?=[[:word:]])|(?<=[[:word:]])(?![[:word:]]))";
const NOT_WORD_BOUNDARY: &str = "(?:(?<=[[:word:]])(?=[[:word:]])|(?<![[:word:]])(?![[:word:]]))";

/// Which flags are in force at a point in the pattern. Java scopes an inline
/// `(?i)` to the end of its enclosing group, so these are stacked on `(`.
#[derive(Clone, Copy, Default)]
struct Flags {
    /// `(?i)` — ASCII-only case folding, which is *not* the engine's `i`, so it
    /// is expanded into the pattern and the flag itself is dropped.
    ci: bool,
    /// `(?s)` — DOTALL, which both flavours spell the same way.
    dotall: bool,
}

/// Rewrite a `java.util.regex` pattern into `fancy-regex` syntax, or name the
/// construct that has no faithful translation.
///
/// The scan is structural rather than textual: it tracks whether it is inside a
/// character class (where `$`, `(`, and the quantifiers are literal), it
/// consumes every escape as a unit so a `\\` before a metacharacter is never
/// mistaken for the metacharacter itself, and it carries the flag stack that
/// decides how `.` and a bare letter are emitted.
pub fn translate(pattern: &str) -> Result<String, String> {
    let cs: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len());
    let mut i = 0usize;
    let mut in_class = false;
    let mut flags = Flags::default();
    let mut stack: Vec<Flags> = Vec::new();
    while i < cs.len() {
        let c = cs[i];
        match c {
            '\\' => {
                let Some(&e) = cs.get(i + 1) else {
                    return Err(unsupported("a trailing backslash", pattern));
                };
                i += 2;
                match e {
                    // `\Q...\E` quotes a literal run. The markers are escaped
                    // away rather than passed through — the engine has neither.
                    'Q' => {
                        let mut lit = String::new();
                        while i < cs.len() {
                            if cs[i] == '\\' && cs.get(i + 1) == Some(&'E') {
                                i += 2;
                                break;
                            }
                            lit.push(cs[i]);
                            i += 1;
                        }
                        for ch in lit.chars() {
                            push_literal(&mut out, ch, in_class, flags.ci, true);
                        }
                    }
                    'E' => {} // a stray `\E` closes nothing
                    // The predefined classes, pinned to ASCII the way Java's
                    // defaults are.
                    'd' => out.push_str(&posix_class(DIGIT, false, in_class)),
                    'D' => out.push_str(&posix_class(DIGIT, true, in_class)),
                    'w' => out.push_str(&posix_class(WORD, false, in_class)),
                    'W' => out.push_str(&posix_class(WORD, true, in_class)),
                    's' => out.push_str(&posix_class(SPACE, false, in_class)),
                    'S' => out.push_str(&posix_class(SPACE, true, in_class)),
                    'b' | 'B' => {
                        if in_class {
                            return Err(unsupported(
                                &format!("\\{e} inside a character class"),
                                pattern,
                            ));
                        }
                        out.push_str(if e == 'b' {
                            WORD_BOUNDARY
                        } else {
                            NOT_WORD_BOUNDARY
                        });
                    }
                    'h' => out.push_str(HORIZ),
                    'H' => out.push_str(&negate_class(HORIZ)),
                    'v' => out.push_str(VERT),
                    'V' => out.push_str(&negate_class(VERT)),
                    'R' => {
                        if in_class {
                            return Err(unsupported("\\R inside a character class", pattern));
                        }
                        out.push_str(LINEBREAK);
                    }
                    // `\Z` is end-of-input-but-for-a-final-terminator, which the
                    // engine has no anchor for; a lookahead reproduces it.
                    'Z' => {
                        if in_class {
                            return Err(unsupported("\\Z inside a character class", pattern));
                        }
                        out.push_str(&format!("(?={LINEBREAK}?\\z)"));
                    }
                    // Java writes a code point `￿` / `\xFF`; the engine
                    // wants braces. Decoding also lets a case-insensitive run
                    // fold it.
                    'u' | 'x' => {
                        let (ch, next) = read_code_point(&cs, i, e, pattern)?;
                        i = next;
                        push_literal(&mut out, ch, in_class, flags.ci, true);
                    }
                    'p' | 'P' => {
                        let (name, next) = read_braced(&cs, i, pattern, e)?;
                        i = next;
                        out.push_str(&translate_property(e, &name, pattern)?);
                    }
                    // Constructs with no equivalent at all.
                    'G' => return Err(unsupported("\\G (end of previous match)", pattern)),
                    'X' => return Err(unsupported("\\X (grapheme cluster)", pattern)),
                    'c' => return Err(unsupported("\\cX (control character)", pattern)),
                    'N' => return Err(unsupported("\\N{...} (named character)", pattern)),
                    '0' => return Err(unsupported("\\0n (octal escape)", pattern)),
                    // A backreference, `\A`/`\z`, or an escaped metacharacter —
                    // all spelled identically by both flavours.
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            }
            '[' if !in_class => {
                in_class = true;
                out.push('[');
                i += 1;
                if cs.get(i) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                // A `]` immediately after `[` or `[^` is a literal in Java; the
                // engine wants it escaped.
                if cs.get(i) == Some(&']') {
                    out.push_str("\\]");
                    i += 1;
                }
            }
            ']' if in_class => {
                in_class = false;
                out.push(']');
                i += 1;
            }
            // Java's `.` excludes its line terminators, not just `\n`.
            '.' if !in_class => {
                out.push_str(if flags.dotall { "(?s:.)" } else { DOT });
                i += 1;
            }
            // Default-mode `^` is the start of input, full stop.
            '^' if !in_class => {
                out.push_str("\\A");
                i += 1;
            }
            // Default-mode `$` also matches before a *final* line terminator, so
            // `"abc\n".replaceAll("c$", "X")` replaces where `\z` would not.
            '$' if !in_class => {
                out.push_str(&format!("(?=(?:\\r\\n|[{TERMINATORS}])?\\z)"));
                i += 1;
            }
            '(' if !in_class => {
                if cs.get(i + 1) == Some(&'?') {
                    match cs.get(i + 2) {
                        Some('>') => return Err(unsupported("(?> (atomic group)", pattern)),
                        Some('(') => return Err(unsupported("(?(...) (conditional)", pattern)),
                        Some('#') => return Err(unsupported("(?# (comment group)", pattern)),
                        _ => {}
                    }
                    if let Some((letters, scoped, end)) = inline_flags(&cs, i + 2) {
                        let mut next = flags;
                        let mut on = true;
                        for f in letters.chars() {
                            match f {
                                '-' => on = false,
                                'i' => next.ci = on,
                                's' => next.dotall = on,
                                // `m` and `x` differ from the engine's in what
                                // they treat as a line and as ignorable space;
                                // `d`, `u`, and `U` name Java-only modes.
                                other => {
                                    return Err(unsupported(
                                        &format!("(?{other}) (flag with no equivalent)"),
                                        pattern,
                                    ))
                                }
                            }
                        }
                        if scoped {
                            // `(?flags:...)` — the flags cover this group only.
                            stack.push(flags);
                            flags = next;
                            out.push_str("(?:");
                        } else {
                            // `(?flags)` — in force to the end of the enclosing
                            // group, which the stack already restores.
                            flags = next;
                        }
                        i = end + 1;
                        continue;
                    }
                }
                stack.push(flags);
                out.push('(');
                i += 1;
            }
            ')' if !in_class => {
                flags = stack.pop().unwrap_or_default();
                out.push(')');
                i += 1;
            }
            // A possessive quantifier never gives back what it consumed; the
            // engine's greedy one does, which is a different language.
            '*' | '+' | '?' | '}' if !in_class && cs.get(i + 1) == Some(&'+') => {
                return Err(unsupported(
                    &format!("{c}+ (possessive quantifier)"),
                    pattern,
                ));
            }
            // An ASCII letter under `(?i)` becomes both cases explicitly, which
            // is Java's ASCII-only folding; the engine's own `i` folds Unicode.
            other if flags.ci && other.is_ascii_alphabetic() => {
                // Inside a class, `a-z` is a range and both its ends move.
                if in_class && cs.get(i + 1) == Some(&'-') {
                    if let Some(&hi) = cs.get(i + 2) {
                        if hi.is_ascii_alphabetic() {
                            out.push(other);
                            out.push('-');
                            out.push(hi);
                            out.push(flip_case(other));
                            out.push('-');
                            out.push(flip_case(hi));
                            i += 3;
                            continue;
                        }
                    }
                }
                push_literal(&mut out, other, in_class, true, false);
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    Ok(out)
}

/// A POSIX class in the spelling its position needs: bare inside a character
/// class (`[\d-]` becomes `[[:digit:]-]`), bracketed outside it. A negated one
/// nests, which the engine accepts in both positions.
fn posix_class(posix: &str, negated: bool, in_class: bool) -> String {
    match (negated, in_class) {
        (false, true) => posix.to_string(),
        (false, false) => format!("[{posix}]"),
        (true, _) => format!("[^{posix}]"),
    }
}

/// Emit one literal character, escaped for its position, doubling it into both
/// cases when a case-insensitive run is in force. `already_literal` is set for a
/// character that came from `\Q...\E` or a code-point escape, where even a
/// metacharacter must be emitted literally.
fn push_literal(out: &mut String, c: char, in_class: bool, ci: bool, already_literal: bool) {
    if ci && c.is_ascii_alphabetic() {
        if !in_class {
            out.push('[');
        }
        out.push(c);
        out.push(flip_case(c));
        if !in_class {
            out.push(']');
        }
        return;
    }
    if already_literal {
        out.push_str(&fancy_regex::escape(&c.to_string()));
    } else {
        out.push(c);
    }
}

/// The other ASCII case of `c`.
fn flip_case(c: char) -> char {
    if c.is_ascii_uppercase() {
        c.to_ascii_lowercase()
    } else {
        c.to_ascii_uppercase()
    }
}

/// Read Java's `\uHHHH` or `\xHH` / `\x{...}` code point, which the engine
/// spells only in the braced form. Returns the character and the index past it.
fn read_code_point(
    cs: &[char],
    i: usize,
    kind: char,
    pattern: &str,
) -> Result<(char, usize), String> {
    let (digits, next) = if cs.get(i) == Some(&'{') {
        let mut j = i + 1;
        let mut d = String::new();
        while j < cs.len() && cs[j] != '}' {
            d.push(cs[j]);
            j += 1;
        }
        if j == cs.len() {
            return Err(unsupported(
                &format!("\\{kind}{{...}} with no closing brace"),
                pattern,
            ));
        }
        (d, j + 1)
    } else {
        let want = if kind == 'u' { 4 } else { 2 };
        let d: String = cs[i..].iter().take(want).collect();
        if d.chars().count() < want {
            return Err(unsupported(
                &format!("a truncated \\{kind} escape"),
                pattern,
            ));
        }
        (d, i + want)
    };
    match u32::from_str_radix(&digits, 16)
        .ok()
        .and_then(char::from_u32)
    {
        Some(c) => Ok((c, next)),
        None => Err(unsupported(
            &format!("\\{kind} naming no character (`{digits}`)"),
            pattern,
        )),
    }
}

/// Negate a bracketed class produced above (`[...]` becomes `[^...]`).
fn negate_class(class: &str) -> String {
    format!("[^{}", &class[1..])
}

/// Read the `{...}` payload of a `\p`/`\P`, or the single-letter shorthand
/// (`\pL`). Returns the name and the index just past it.
fn read_braced(
    cs: &[char],
    i: usize,
    pattern: &str,
    kind: char,
) -> Result<(String, usize), String> {
    if cs.get(i) != Some(&'{') {
        return match cs.get(i) {
            Some(c) => Ok((c.to_string(), i + 1)),
            None => Err(unsupported(&format!("\\{kind} with no name"), pattern)),
        };
    }
    let mut name = String::new();
    let mut j = i + 1;
    while j < cs.len() && cs[j] != '}' {
        name.push(cs[j]);
        j += 1;
    }
    if j == cs.len() {
        return Err(unsupported(
            &format!("\\{kind}{{...}} with no closing brace"),
            pattern,
        ));
    }
    Ok((name, j + 1))
}

/// Map a Java `\p{...}` property name onto the engine's spelling.
///
/// The POSIX names Java defines itself (`Alpha`, `Digit`, ...) are ASCII-only
/// and become the engine's POSIX classes, which are ASCII-only too. An
/// `Is`-prefixed name is a Unicode script or category, which the engine spells
/// without the prefix. An `In`-prefixed name is a Unicode *block*
/// (`\p{InGreek}` is a code-point range, not a script) and a `java`-prefixed one
/// calls a `Character` predicate — the engine has neither, and approximating
/// either with a script would answer a different question, so both are refused.
fn translate_property(kind: char, name: &str, pattern: &str) -> Result<String, String> {
    let posix = |cls: &str| {
        if kind == 'p' {
            format!("[[:{cls}:]]")
        } else {
            format!("[[:^{cls}:]]")
        }
    };
    Ok(match name {
        "Alpha" => posix("alpha"),
        "Digit" => posix("digit"),
        "Alnum" => posix("alnum"),
        "Upper" => posix("upper"),
        "Lower" => posix("lower"),
        "Space" => posix("space"),
        "Punct" => posix("punct"),
        "XDigit" => posix("xdigit"),
        "ASCII" => posix("ascii"),
        "Blank" => posix("blank"),
        "Cntrl" => posix("cntrl"),
        "Graph" => posix("graph"),
        "Print" => posix("print"),
        n if n.starts_with("In") => {
            return Err(unsupported(
                &format!("\\{kind}{{{n}}} (Unicode block)"),
                pattern,
            ))
        }
        n if n.starts_with("java") => {
            return Err(unsupported(
                &format!("\\{kind}{{{n}}} (java.lang.Character predicate)"),
                pattern,
            ))
        }
        n => format!("\\{kind}{{{}}}", n.strip_prefix("Is").unwrap_or(n)),
    })
}

/// The flag letters of an inline `(?flags)` / `(?flags:...)` group starting at
/// `i`, whether it scopes a group, and the index of its `)` or `:`. `None` when
/// this is not a flag group — a lookaround, a named group, or a plain `(?:`.
fn inline_flags(cs: &[char], i: usize) -> Option<(String, bool, usize)> {
    let mut letters = String::new();
    let mut j = i;
    while let Some(&c) = cs.get(j) {
        match c {
            ')' if !letters.is_empty() => return Some((letters, false, j)),
            ':' if !letters.is_empty() => return Some((letters, true, j)),
            'a'..='z' | 'A'..='Z' | '-' => {
                letters.push(c);
                j += 1;
            }
            _ => return None,
        }
    }
    None
}

/// A `PatternSyntaxException` message naming the construct groovyrs will not
/// pretend to support, in Java's `description near index N` shape followed by
/// the pattern.
fn unsupported(construct: &str, pattern: &str) -> String {
    format!("groovyrs supports no equivalent of {construct} near index 0\n{pattern}")
}

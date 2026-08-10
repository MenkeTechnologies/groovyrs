//! Source-level invariants on the builtin id space.
//!
//! groovyrs hands fusevm a `u16` per host builtin (`Op::CallBuiltin(id, argc)`)
//! and installs a handler for each with `VM::register_builtin`. The ids are
//! hand-assigned `pub const`s in `src/host.rs`, which makes them the one part of
//! the frontend the compiler cannot check: two constants may carry the same
//! number and nothing complains. `register_builtin` keeps the *last* handler
//! registered for an id, so the earlier one is silently replaced and every call
//! site compiled against the earlier constant quietly dispatches to the other
//! builtin — a wrong answer with no error, no warning, and no diff conflict when
//! the two constants were added on separate branches (each side only adds a
//! line, so the merge is clean).
//!
//! Sibling frontends have shipped exactly this bug: two builtins on one id where
//! the later registration ate the earlier handler. These tests read the
//! constants back out of `src/host.rs` and the registrations out of
//! `host::install`, so a collision fails the build instead of mis-running.
//!
//! The same hazard exists a second time over, keyed by *name* instead of by
//! number. groovyrs's GDK is not a map but a set of large `match` blocks —
//! `dispatch_method`, `dispatch_static`, `dispatch_iteration`,
//! `dispatch_set_method`, `static_field`, and the rest — each arm binding one
//! method name for one receiver shape. Two arms that bind the same name for the
//! same receiver under the same guard are a registry with a duplicate key: the
//! first arm answers every call and the second is dead, which is the *earlier*
//! handler winning where `register_builtin` lets the later one win. Either way
//! one of the two bodies never runs.
//!
//! rustc's `unreachable_patterns` lint catches the easy shape of this (and CI
//! runs `clippy -D warnings`), but it gives up as soon as a guard is involved:
//! two arms carrying the same `if` condition compile silently. That is the shape
//! that actually occurs here, because the arity-overloaded GDK methods are
//! already written as guard pairs (`"round" if args.is_empty()` then `"round"`),
//! so the natural way to add a third spelling is to add another guarded arm.
//! [`dispatch_names_are_unique`] reads the arms back out of the source and fails
//! on a duplicate key, admitting the guard pairs because their guards differ.

use std::collections::HashMap;
use std::path::PathBuf;

fn read(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every `pub const NAME: u16 = N;` in `src/host.rs`, as (name, value, line).
fn builtin_id_consts() -> Vec<(String, u16, usize)> {
    let src = read("src/host.rs");
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix("pub const ") else {
            continue;
        };
        let Some((name, rest)) = rest.split_once(':') else {
            continue;
        };
        let Some(value) = rest.trim().strip_prefix("u16") else {
            continue;
        };
        let Some(value) = value.trim().strip_prefix('=') else {
            continue;
        };
        let Some(value) = value.trim().strip_suffix(';') else {
            continue;
        };
        let value: u16 = value
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("src/host.rs:{}: unparsable builtin id: {e}", i + 1));
        out.push((name.trim().to_string(), value, i + 1));
    }
    out
}

/// The constant named by each `vm.register_builtin(NAME, …)` call in the crate,
/// as (name, file:line). `host::DBG_LINE` is written qualified from `lib.rs`, so
/// the leading path is stripped.
fn registrations() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for rel in ["src/host.rs", "src/lib.rs"] {
        for (i, line) in read(rel).lines().enumerate() {
            let Some(rest) = line.split_once("register_builtin(") else {
                continue;
            };
            let Some((arg, _)) = rest.1.split_once(',') else {
                continue;
            };
            let name = arg.trim().rsplit("::").next().unwrap().to_string();
            out.push((name, format!("{rel}:{}", i + 1)));
        }
    }
    out
}

/// The invariant the sibling frontends broke: no two builtin constants may share
/// a numeric id.
#[test]
fn builtin_ids_are_unique() {
    let consts = builtin_id_consts();
    assert!(
        consts.len() > 40,
        "only {} builtin id constants parsed out of src/host.rs — the scraper \
         stopped matching the source, so this test is no longer guarding anything",
        consts.len()
    );

    let mut by_id: HashMap<u16, Vec<(String, usize)>> = HashMap::new();
    for (name, id, line) in &consts {
        by_id.entry(*id).or_default().push((name.clone(), *line));
    }

    let mut clashes: Vec<String> = by_id
        .iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|(id, v)| {
            let who = v
                .iter()
                .map(|(n, l)| format!("{n} (src/host.rs:{l})"))
                .collect::<Vec<_>>()
                .join(" and ");
            format!("  id {id}: {who}")
        })
        .collect();
    clashes.sort();
    assert!(
        clashes.is_empty(),
        "builtin id collision — `register_builtin` keeps only the last handler \
         registered for an id, so the earlier builtin is silently replaced and \
         its call sites dispatch to the wrong handler. Give each constant its \
         own number:\n{}",
        clashes.join("\n")
    );
}

/// A constant may be registered at most once: a second `register_builtin` for
/// the same constant replaces the first handler just as a duplicate id does.
#[test]
fn each_builtin_is_registered_at_most_once() {
    let mut seen: HashMap<String, Vec<String>> = HashMap::new();
    for (name, at) in registrations() {
        seen.entry(name).or_default().push(at);
    }
    let mut dupes: Vec<String> = seen
        .iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|(n, v)| format!("  {n}: {}", v.join(", ")))
        .collect();
    dupes.sort();
    assert!(
        dupes.is_empty(),
        "builtin registered more than once — the last handler wins:\n{}",
        dupes.join("\n")
    );
}

/// Every registration names a constant that exists. Catches a registration left
/// behind after its constant was renamed (which would otherwise only fail once
/// someone re-added a constant under the old name).
#[test]
fn every_registration_names_a_known_constant() {
    let known: Vec<String> = builtin_id_consts().into_iter().map(|(n, ..)| n).collect();
    let unknown: Vec<String> = registrations()
        .into_iter()
        .filter(|(n, _)| !known.contains(n))
        .map(|(n, at)| format!("  {n} at {at}"))
        .collect();
    assert!(
        unknown.is_empty(),
        "register_builtin names a constant that is not a `pub const … : u16` in \
         src/host.rs:\n{}",
        unknown.join("\n")
    );
}

// ── Name-keyed dispatch: the GDK `match` tables ─────────────────────────────

/// One name-bearing arm of one `match` block.
#[derive(Debug)]
struct Arm {
    /// The `match` block the arm belongs to, as `fn:line` of its opening brace.
    /// Keying on the block rather than the enclosing function is what lets an
    /// inner `match method { … }` inside an outer arm's body re-bind a name the
    /// outer arm already matched, which is how several GDK arms are written.
    block: String,
    /// The receiver half of a tuple pattern (`(Value::Str(_), "trim")`), with
    /// binder names normalized away, or `<unary>` for a bare `match method`.
    recv: String,
    /// One string literal from the name half. An or-pattern contributes one arm
    /// per alternative.
    name: String,
    /// The `if …` guard text, empty when the arm is unguarded. Part of the key:
    /// two arms may share a name when their guards select different calls.
    guard: String,
    file: String,
    line: usize,
}

/// Strip string literals, char literals, and line comments so brace counting
/// sees only code. Escapes go first so `'\''` and `"a\"b"` do not confuse it.
fn code_only(line: &str) -> String {
    let mut s = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let (mut in_str, mut in_chr) = (false, false);
    while let Some(c) = chars.next() {
        match c {
            '\\' if in_str || in_chr => {
                chars.next();
            }
            '"' if !in_chr => in_str = !in_str,
            '\'' if !in_str => in_chr = !in_chr,
            '/' if !in_str && !in_chr && chars.peek() == Some(&'/') => break,
            _ if !in_str && !in_chr => s.push(c),
            _ => {}
        }
    }
    s
}

/// Split a tuple pattern into (receiver, name) halves at its top-level comma.
fn split_tuple(pat: &str) -> (String, String) {
    let inner = match pat.strip_prefix('(').and_then(|p| p.strip_suffix(')')) {
        Some(i) => i,
        None => return ("<unary>".to_string(), pat.to_string()),
    };
    let (mut depth, mut quoted) = (0i32, false);
    let bytes: Vec<char> = inner.chars().collect();
    for (k, c) in bytes.iter().enumerate() {
        match c {
            '"' if k == 0 || bytes[k - 1] != '\\' => quoted = !quoted,
            _ if quoted => {}
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                let (a, b) = (
                    bytes[..k].iter().collect::<String>(),
                    bytes[k + 1..].iter().collect::<String>(),
                );
                return (a, b);
            }
            _ => {}
        }
    }
    ("<unary>".to_string(), inner.to_string())
}

/// Every string literal in a pattern's name half, in order.
fn literals(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '"' {
            let mut lit = String::new();
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' {
                    i += 1;
                }
                if i < chars.len() {
                    lit.push(chars[i]);
                }
                i += 1;
            }
            out.push(lit);
        }
        i += 1;
    }
    out
}

/// Scrape the name-bearing `match` arms out of one source file.
///
/// Blocks are tracked by walking `match` keywords and braces in source order, so
/// `Some(match (class, method) {` arms the very brace on its own line and an arm
/// body's `{` opens a non-match block whose contents are skipped.
fn dispatch_arms(rel: &str) -> Vec<Arm> {
    let src = read(rel);
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    let mut stack: Vec<(bool, String)> = Vec::new();
    let mut pending_match = false;
    let mut func = "<top>".to_string();

    for (i, line) in lines.iter().enumerate() {
        if let Some(rest) = line.trim_start().strip_prefix("fn ").or_else(|| {
            line.trim_start()
                .strip_prefix("pub fn ")
                .or_else(|| line.trim_start().strip_prefix("pub(crate) fn "))
        }) {
            func = rest
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
                .unwrap_or("<top>")
                .to_string();
        }

        // Arm recognition runs against the block open at the *start* of the
        // line, since `X => {` both records an arm and opens a block.
        if matches!(stack.last(), Some((true, _))) {
            let t = line.trim();
            if !t.starts_with("//") && (t.starts_with('(') || t.starts_with('"')) {
                // The pattern may wrap; accumulate until the `=>` that ends it.
                let mut pat = t.to_string();
                let mut j = i;
                while !pat.contains("=>") && j + 1 < lines.len() && j - i < 8 {
                    j += 1;
                    let n = lines[j].trim();
                    if !n.starts_with("//") {
                        pat.push(' ');
                        pat.push_str(n);
                    }
                }
                if let Some((p, _)) = pat.split_once("=>") {
                    let p = p.trim_end();
                    let (p, guard) = match p.find(" if ") {
                        Some(at) => (&p[..at], p[at + 4..].trim().to_string()),
                        None => (p, String::new()),
                    };
                    let (recv, namepart) = split_tuple(p.trim());
                    let recv = normalize_binders(recv.trim());
                    let block = stack.last().unwrap().1.clone();
                    for name in literals(&namepart) {
                        out.push(Arm {
                            block: block.clone(),
                            recv: recv.clone(),
                            name,
                            guard: guard.clone(),
                            file: rel.to_string(),
                            line: i + 1,
                        });
                    }
                }
            }
        }

        let c = code_only(line);
        let mut rest = c.as_str();
        while let Some(at) = rest
            .find(['{', '}'])
            .into_iter()
            .chain(word_at(rest, "match"))
            .min()
        {
            let ch = rest.as_bytes()[at];
            if ch == b'{' {
                stack.push((pending_match, format!("{func}:{}", i + 1)));
                pending_match = false;
            } else if ch == b'}' {
                stack.pop();
            } else {
                pending_match = true;
            }
            rest = &rest[at + 1..];
        }
    }
    out
}

/// Byte offset of the next `match` keyword in `s`, if any.
fn word_at(s: &str, word: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut from = 0;
    while let Some(rel) = s[from..].find(word) {
        let at = from + rel;
        let before_ok = at == 0 || !is_ident_byte(bytes[at - 1]);
        let after = at + word.len();
        let after_ok = after >= bytes.len() || !is_ident_byte(bytes[after]);
        if before_ok && after_ok {
            return Some(at);
        }
        from = at + word.len();
    }
    None
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `Value::Str(s)` and `Value::Str(t)` are the same receiver shape; the binder
/// name is not part of the key.
fn normalize_binders(recv: &str) -> String {
    let mut out = String::with_capacity(recv.len());
    let chars: Vec<char> = recv.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '(' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != ')' {
                j += 1;
            }
            let inner: String = chars[i + 1..j.min(chars.len())].iter().collect();
            let inner = inner.trim();
            let is_binder = !inner.is_empty()
                && inner.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
                && inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if is_binder && j < chars.len() {
                out.push_str("(_)");
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Every `.rs` under `src/`, so a dispatch table moved to a new module keeps
/// being guarded.
fn source_files() -> Vec<String> {
    fn walk(dir: &PathBuf, out: &mut Vec<String>, root: &PathBuf) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, out, root);
            } else if p.extension().is_some_and(|e| e == "rs") {
                out.push(
                    p.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    walk(&root.join("src"), &mut out, &root);
    out
}

/// The name-keyed twin of [`builtin_ids_are_unique`]: within one `match` block,
/// one receiver shape, and one guard, a method name may be bound once. A second
/// binding is dead — the first arm answers every call that reaches it.
#[test]
fn dispatch_names_are_unique() {
    let arms: Vec<Arm> = source_files()
        .iter()
        .flat_map(|f| dispatch_arms(f))
        .collect();
    assert!(
        arms.len() > 400,
        "only {} name-keyed match arms parsed out of src/ — the scraper stopped \
         matching the source, so this test is no longer guarding anything",
        arms.len()
    );

    let mut by_key: HashMap<(&str, &str, &str, &str), Vec<&Arm>> = HashMap::new();
    for a in &arms {
        by_key
            .entry((&a.block, &a.recv, &a.name, &a.guard))
            .or_default()
            .push(a);
    }
    let mut clashes: Vec<String> = by_key
        .values()
        .filter(|v| v.len() > 1)
        .map(|v| {
            let a = v[0];
            let where_ = v
                .iter()
                .map(|x| format!("{}:{}", x.file, x.line))
                .collect::<Vec<_>>()
                .join(" and ");
            let guard = if a.guard.is_empty() {
                "no guard".to_string()
            } else {
                format!("guard `{}`", a.guard)
            };
            format!(
                "  `{}` on receiver `{}` ({guard}) in the match at {}: {where_}",
                a.name, a.recv, a.block
            )
        })
        .collect();
    clashes.sort();
    assert!(
        clashes.is_empty(),
        "duplicate name in a dispatch table — the first arm answers every call \
         and the later one is dead code, so whichever body was meant to run may \
         not be the one that does. rustc's `unreachable_patterns` does not see \
         this when the arms carry guards. Give the arms distinguishing guards, \
         or fold them into one:\n{}",
        clashes.join("\n")
    );
}

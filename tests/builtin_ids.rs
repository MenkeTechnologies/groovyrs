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

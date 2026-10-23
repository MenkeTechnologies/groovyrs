//! `java.io.File` with the GDK's text methods, and the line reader that
//! `withReader` and `new StringReader(text)` hand a script. Paths are kept as
//! written (`new File('rel/x.txt').path` stays relative), as Java keeps them.

use super::*;
use std::path::Path;

/// `new File(path)` / `new File(parent, child)`.
pub(super) fn new_file(args: &[Value]) -> Option<Value> {
    let path = match args {
        [p] => groovy_str(p),
        [parent, child] => {
            let parent = file_path(parent).unwrap_or_else(|| groovy_str(parent));
            format!("{}/{}", parent.trim_end_matches('/'), groovy_str(child))
        }
        _ => return None,
    };
    Some(heap_push(HeapObj::File(path)))
}

/// The path of a `File` handle.
pub(super) fn file_path(v: &Value) -> Option<String> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::File(p)) => Some(p.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// A reader over `text`, positioned at its start.
pub(super) fn new_reader(text: String) -> Value {
    heap_push(HeapObj::TextReader { text, pos: 0 })
}

/// The unread rest of a reader handle, if `v` is one.
fn reader_rest(v: &Value) -> Option<String> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::TextReader { text, pos }) => Some(text[*pos..].to_string()),
            _ => None,
        }),
        _ => None,
    }
}

/// Move a reader handle on by `n` bytes.
fn reader_advance(v: &Value, n: usize) {
    if let Value::Obj(id) = v {
        HEAP.with(|h| {
            if let Some(HeapObj::TextReader { pos, .. }) = h.borrow_mut().get_mut(*id as usize) {
                *pos += n;
            }
        });
    }
}

/// The text a `JsonSlurper.parse` source holds: a `File`'s contents, a
/// reader's unread rest, a `byte[]` as UTF-8. `Err` is the I/O failure.
pub(super) fn source_text(v: &Value) -> Option<Result<String, String>> {
    if let Some(path) = file_path(v) {
        return Some(read(&path));
    }
    if let Some(rest) = reader_rest(v) {
        reader_advance(v, rest.len());
        return Some(Ok(rest));
    }
    if array_elem(v) == Some(ArrayElem::Byte) {
        let bytes: Vec<u8> = as_list(v)?
            .iter()
            .map(|b| as_i64(b).unwrap_or(0) as u8)
            .collect();
        return Some(Ok(String::from_utf8_lossy(&bytes).into_owned()));
    }
    None
}

/// The file's contents, or Java's `FileNotFoundException` message.
fn read(path: &str) -> Result<String, String> {
    std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .map_err(|e| not_found(path, &e))
}

/// `FileNotFoundException`'s message: the path and the OS reason.
fn not_found(path: &str, e: &std::io::Error) -> String {
    let reason = match e.kind() {
        std::io::ErrorKind::NotFound => "No such file or directory".to_string(),
        std::io::ErrorKind::PermissionDenied => "Permission denied".to_string(),
        _ => e.to_string(),
    };
    format!("{path} ({reason})")
}

/// The lines of `text` as `readLines` splits them: on `\n`, `\r\n` or `\r`,
/// with no trailing empty line.
fn lines(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => out.push(Value::str(std::mem::take(&mut cur))),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(Value::str(std::mem::take(&mut cur)));
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(Value::str(cur));
    }
    out
}

/// Raise the `FileNotFoundException` / `IOException` an I/O call failed with.
fn raise_io(vm: &mut VM, message: String) -> Value {
    raise(vm, "FileNotFoundException", &message);
    Value::Undef
}

/// The `File` and reader methods. `None` hands the call back to the general
/// dispatch (and its `MissingMethodException`).
pub(super) fn dispatch(vm: &mut VM, recv: &Value, method: &str, args: &[Value]) -> Option<Value> {
    if let Some(path) = file_path(recv) {
        return file_method(vm, recv, &path, method, args);
    }
    reader_rest(recv)?;
    Some(match (method, args) {
        ("readLine", []) => {
            let rest = reader_rest(recv)?;
            if rest.is_empty() {
                return Some(Value::Undef);
            }
            let end = rest.find('\n').map_or(rest.len(), |i| i + 1);
            reader_advance(recv, end);
            let line = rest[..end].trim_end_matches('\n').trim_end_matches('\r');
            Value::str(line.to_string())
        }
        ("getText", []) => {
            let rest = reader_rest(recv)?;
            reader_advance(recv, rest.len());
            Value::str(rest)
        }
        ("readLines", []) => {
            let rest = reader_rest(recv)?;
            reader_advance(recv, rest.len());
            glist(lines(&rest))
        }
        ("close", []) => Value::Undef,
        _ => return None,
    })
}

fn file_method(
    vm: &mut VM,
    recv: &Value,
    path: &str,
    method: &str,
    args: &[Value],
) -> Option<Value> {
    let p = Path::new(path);
    let write = |vm: &mut VM, text: &str, append: bool| {
        use std::io::Write;
        let opened = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(path);
        match opened.and_then(|mut f| f.write_all(text.as_bytes())) {
            Ok(()) => Value::Undef,
            Err(e) => raise_io(vm, not_found(path, &e)),
        }
    };
    Some(match (method, args) {
        ("getText", []) => match read(path) {
            Ok(t) => Value::str(t),
            Err(m) => raise_io(vm, m),
        },
        ("readLines", []) => match read(path) {
            Ok(t) => glist(lines(&t)),
            Err(m) => raise_io(vm, m),
        },
        ("getBytes", []) => match std::fs::read(path) {
            Ok(b) => garray(
                b.into_iter()
                    .map(|x| Value::int(i64::from(x as i8)))
                    .collect(),
                ArrayElem::Byte,
            ),
            Err(e) => raise_io(vm, not_found(path, &e)),
        },
        ("eachLine", [clo]) if closure_meta(clo).is_some() => {
            let text = match read(path) {
                Ok(t) => t,
                Err(m) => return Some(raise_io(vm, m)),
            };
            let two = closure_meta(clo).is_some_and(|m| m.params >= 2);
            let mut last = Value::Undef;
            for (i, line) in lines(&text).into_iter().enumerate() {
                let call = if two {
                    vec![line, Value::int(i as i64 + 1)]
                } else {
                    vec![line]
                };
                match invoke_closure(vm, clo, &call) {
                    Ok(v) => last = v,
                    Err(e) => {
                        fault(vm, e);
                        return Some(Value::Undef);
                    }
                }
                if pending_exc() {
                    return Some(Value::Undef);
                }
            }
            last
        }
        ("setText" | "write", [text]) => write(vm, &groovy_str(text), false),
        ("append" | "leftShift", [text]) => {
            write(vm, &groovy_str(text), true);
            if method == "leftShift" {
                recv.clone()
            } else {
                Value::Undef
            }
        }
        ("withWriter", [clo]) if closure_meta(clo).is_some() => {
            let writer = heap_push(HeapObj::Buffer {
                class: "java.io.BufferedWriter",
                text: String::new(),
            });
            let out = invoke_closure(vm, clo, std::slice::from_ref(&writer));
            let text = as_buffer(&writer).map(|(_, t)| t).unwrap_or_default();
            write(vm, &text, false);
            match out {
                Ok(v) => v,
                Err(e) => {
                    fault(vm, e);
                    Value::Undef
                }
            }
        }
        ("withReader", [clo]) if closure_meta(clo).is_some() => {
            let text = match read(path) {
                Ok(t) => t,
                Err(m) => return Some(raise_io(vm, m)),
            };
            let reader = new_reader(text);
            match invoke_closure(vm, clo, &[reader]) {
                Ok(v) => v,
                Err(e) => {
                    fault(vm, e);
                    Value::Undef
                }
            }
        }
        ("exists", []) => Value::bool(p.exists()),
        ("isFile", []) => Value::bool(p.is_file()),
        ("isDirectory", []) => Value::bool(p.is_dir()),
        ("length" | "size", []) => {
            Value::int(std::fs::metadata(path).map_or(0, |m| m.len() as i64))
        }
        ("delete", []) => Value::bool(if p.is_dir() {
            std::fs::remove_dir(path).is_ok()
        } else {
            std::fs::remove_file(path).is_ok()
        }),
        ("mkdir", []) => Value::bool(std::fs::create_dir(path).is_ok()),
        ("mkdirs", []) => Value::bool(!p.exists() && std::fs::create_dir_all(path).is_ok()),
        ("getName", []) => Value::str(
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        ),
        ("getPath" | "toString", []) => Value::str(path.to_string()),
        ("getParent", []) => match p.parent().map(|q| q.to_string_lossy().into_owned()) {
            Some(q) if !q.is_empty() => Value::str(q),
            _ => Value::Undef,
        },
        ("getAbsolutePath", []) => Value::str(if p.is_absolute() {
            path.to_string()
        } else {
            std::env::current_dir()
                .map(|d| d.join(p).to_string_lossy().into_owned())
                .unwrap_or_else(|_| path.to_string())
        }),
        ("list", []) => match std::fs::read_dir(path) {
            Ok(entries) => garray(
                entries
                    .filter_map(Result::ok)
                    .map(|e| Value::str(e.file_name().to_string_lossy().into_owned()))
                    .collect(),
                ArrayElem::Ref("java.lang.String"),
            ),
            Err(_) => Value::Undef,
        },
        _ => return None,
    })
}

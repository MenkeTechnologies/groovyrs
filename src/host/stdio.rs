//! The script's standard streams: every `System.out` / `println` / `printf`
//! write goes through [`write_out`], and `System.in` reads through
//! [`system_in`]. By default both are the process's descriptors; an embedder
//! ([`crate::embed::run_program`]) redirects them for the interpreter thread with
//! [`capture_stdout`] and [`set_stdin`].

use super::*;
use std::io::{Read, Write};

thread_local! {
    /// `Some` while stdout is captured: the bytes written so far.
    static STDOUT_CAPTURE: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
    /// The text `System.in` reads, set by an embedder. `None` reads the
    /// process's stdin.
    static STDIN_TEXT: RefCell<Option<String>> = const { RefCell::new(None) };
    /// The reader handle `System.in` answers for this run. One handle per run
    /// so successive reads continue where the last left off; cleared with the
    /// heap it lives on.
    static STDIN_HANDLE: RefCell<Option<Value>> = const { RefCell::new(None) };
    /// The status a `System.exit` call ended the run with.
    static EXIT_STATUS: Cell<Option<i32>> = const { Cell::new(None) };
}

/// Start capturing this thread's stdout writes (see [`take_stdout`]).
pub fn capture_stdout() {
    STDOUT_CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
}

/// Stop capturing and return what was written since [`capture_stdout`].
pub fn take_stdout() -> String {
    let bytes = STDOUT_CAPTURE
        .with(|c| c.borrow_mut().take())
        .unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Make `text` the stdin `System.in` reads on this thread.
pub fn set_stdin(text: &str) {
    STDIN_TEXT.with(|s| *s.borrow_mut() = Some(text.to_string()));
}

/// Write `bytes` to the script's stdout: the capture buffer when one is active,
/// the process's stdout otherwise.
pub(super) fn write_out(bytes: &[u8]) {
    let captured = STDOUT_CAPTURE.with(|c| match c.borrow_mut().as_mut() {
        Some(buf) => {
            buf.extend_from_slice(bytes);
            true
        }
        None => false,
    });
    if !captured {
        let _ = std::io::stdout().write_all(bytes);
    }
}

/// Flush the process's stdout (harmless while captured).
pub(super) fn flush_out() {
    let _ = std::io::stdout().flush();
}

/// `System.in`: a line reader over the whole of stdin, read on first use.
pub(super) fn system_in() -> Value {
    if let Some(handle) = STDIN_HANDLE.with(|h| h.borrow().clone()) {
        return handle;
    }
    let text = STDIN_TEXT.with(|s| s.borrow().clone()).unwrap_or_else(|| {
        let mut buf = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    });
    let handle = file::new_reader(text);
    STDIN_HANDLE.with(|h| *h.borrow_mut() = Some(handle.clone()));
    handle
}

/// `System.exit(status)`: record `status` and halt the VM through the fault
/// path, which stops nested runs and suppresses further output the same way an
/// uncaught fault does. The run then returns `Err`, and the caller reads the
/// status back with [`take_exit_status`] to tell an exit from a failure.
pub(super) fn exit(vm: &mut VM, status: i32) {
    EXIT_STATUS.with(|s| s.set(Some(status)));
    fault(vm, format!("System.exit({status})"));
}

/// The status of the `System.exit` that ended the last run, if one did.
pub fn take_exit_status() -> Option<i32> {
    EXIT_STATUS.with(|s| s.take())
}

/// Forget the `System.in` handle and any exit status (called from [`install`]
/// when the heap is cleared).
pub(super) fn reset() {
    STDIN_HANDLE.with(|h| *h.borrow_mut() = None);
    EXIT_STATUS.with(|s| s.set(None));
}

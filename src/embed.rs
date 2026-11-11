//! Run a Groovy program from Rust with a given stdin and get its stdout back.

use crate::host::stdio;

/// Run `src` as a complete program with `input` as its stdin and return
/// everything it wrote to stdout.
///
/// Each call runs on its own interpreter thread ([`crate::on_interpreter_stack`]),
/// so the heap, class registry and stream redirection are fresh per call and
/// concurrent calls never share state. `System.in` reads `input`; `println`,
/// `print`, `printf` and `System.out` writes are collected instead of reaching
/// the process's stdout (`System.err` still goes to stderr). A parse, compile
/// or runtime error, an uncaught exception, a `System.exit` with a nonzero
/// status or a panic in the interpreter returns `Err`; `System.exit(0)` returns
/// `Ok` with the output written before it.
pub fn run_program(src: &str, input: &str) -> Result<String, String> {
    let (src, input) = (src.to_string(), input.to_string());
    let joined = crate::on_interpreter_stack(move || {
        stdio::capture_stdout();
        stdio::set_stdin(&input);
        let outcome = crate::run_str(&src);
        let out = stdio::take_stdout();
        match (outcome, stdio::take_exit_status()) {
            (Ok(_), _) | (Err(_), Some(0)) => Ok(out),
            (Err(_), Some(status)) => Err(format!("exit status {status}")),
            (Err(e), None) => Err(e),
        }
    });
    joined.unwrap_or_else(|payload| {
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "interpreter panicked".to_string());
        Err(format!("groovyrs: panic: {msg}"))
    })
}

//! groovyrs — Groovy as a fusevm frontend.
//!
//! Pipeline: `lexer` → `parser` builds a Groovy script AST → `compiler` lowers
//! it to a `fusevm::Chunk` → fusevm executes it on the shared three-tier
//! Cranelift JIT, calling back into `host` (the strict numeric hook + the
//! Groovy `/` division builtin) for the semantics fusevm's default awk/shell
//! flavour does not provide. There is no bespoke VM or JVM here — execution and
//! codegen live in fusevm, the same engine behind zshrs, stryke, awkrs, elisp,
//! ruby, python, php, node, and java.

pub mod ast;
pub mod banner;
pub mod cli;
pub mod compiler;
pub mod dap;
pub mod decimal;
pub mod format;
pub mod host;
pub mod lexer;
pub mod lsp;
pub mod parser;
pub mod regex;
pub mod rust_ffi;
pub mod throwable;
pub mod tiers;

pub use banner::version_banner;
use fusevm::{VMResult, Value, VM};

/// The stack the interpreter thread gets, in bytes.
///
/// Two groovyrs recursions live on the **Rust** stack rather than on fusevm's
/// heap-allocated frame vector, and both are driven by user input:
///
/// * **Host re-entry.** A GDK method that runs a closure, a user method, a
///   constructor and an operator overload all call `host::run_sub`, which drives
///   a nested `VM::run`. A program that nests those — `go(n) { [1].collect {
///   go(n - 1) } }` — nests one Rust `VM::run` frame per level. Measured on a
///   debug build: ~133 KB of stack per level (the main thread's 8 MiB carried 63
///   of them before `fatal runtime error: stack overflow`).
/// * **Parsing and lowering.** `parser` is recursive descent and `compiler`
///   walks the AST recursively, so nesting depth in the *source* is Rust
///   recursion too.
///
/// [`host::MAX_CALL_DEPTH`] and [`parser::MAX_NESTING`] bound both so runaway
/// input raises a catchable `java.lang.StackOverflowError` instead of aborting
/// the process — but a bound only helps if the stack can actually hold that many
/// levels. This is sized for `MAX_CALL_DEPTH` levels of host re-entry at the
/// measured debug-build cost, with margin. It is address space, not memory: the
/// pages commit as they are touched.
pub const INTERPRETER_STACK_BYTES: usize = 512 * 1024 * 1024;

/// Run `f` on a thread with [`INTERPRETER_STACK_BYTES`] of stack, propagating a
/// panic to the caller.
///
/// Every groovyrs thread-local — the object heap, the class registry, the
/// pending exception, the script class name — belongs to whichever thread runs
/// the interpreter, so `f` must cover the *whole* job: setting the script class,
/// parsing, lowering, and running. The `groovy` binary wraps its entire post-CLI
/// body in one call. An embedder calling [`run_str`] or [`run_file`] directly
/// runs on its own thread and should wrap the call the same way; a default 2 MiB
/// thread cannot hold [`host::MAX_CALL_DEPTH`] host re-entries.
pub fn on_interpreter_stack<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> std::thread::Result<T> {
    std::thread::Builder::new()
        .name("groovyrs".to_string())
        .stack_size(INTERPRETER_STACK_BYTES)
        .spawn(f)
        .expect("spawn the groovyrs interpreter thread")
        .join()
}

/// Parse Groovy `src` to an AST.
pub fn parse(src: &str) -> Result<ast::Program, String> {
    parser::parse(src)
}

/// Parse and lower Groovy `src` to a runnable fusevm chunk.
pub fn compile(src: &str) -> Result<fusevm::Chunk, String> {
    let prog = parser::parse(src)?;
    compiler::compile(&prog)
}

/// Parse and lower Groovy `src` to a debug chunk carrying per-statement
/// `DBG_LINE` markers (for `groovy --dap`).
pub fn compile_debug(src: &str) -> Result<fusevm::Chunk, String> {
    let prog = parser::parse(src)?;
    compiler::compile_debug(&prog)
}

/// Compile a `.groovy` file with debug markers and run it under the debug
/// adapter's line hook. Installs the groovyrs builtins plus a `DBG_LINE` handler
/// that pauses at breakpoints/steps, and deliberately does NOT enable the
/// tracing JIT (a JIT-compiled hot loop would skip the markers). Called by
/// [`dap::run`].
pub fn eval_file_debug(path: &str) -> Result<(), String> {
    let src =
        std::fs::read_to_string(path).map_err(|e| format!("groovyrs: cannot read {path}: {e}"))?;
    let chunk = compile_debug(&src)?;
    let mut vm = VM::new(chunk);
    host::install(&mut vm);
    // Non-capturing closure coerces to the builtin fn pointer.
    vm.register_builtin(host::DBG_LINE, |vm, _argc| {
        crate::dap::on_debug_line(vm);
        Value::Undef
    });
    vm.set_sited_numeric_hook(std::sync::Arc::new(host::sited_numeric_hook));
    let _ = host::take_error();
    host::set_vm_ptr(&mut vm);
    let outcome = vm.run();
    host::clear_vm_ptr();
    match outcome {
        VMResult::Ok(_) | VMResult::Halted => match host::take_error() {
            Some(e) => Err(e),
            None => Ok(()),
        },
        VMResult::Error(e) => Err(e),
    }
}

/// Register the groovyrs builtins + strict numeric hook on a fresh VM, enable
/// the tracing JIT, and run the chunk. Returns the last top-of-stack value.
fn run_chunk(chunk: fusevm::Chunk, argv: &[String]) -> Result<Value, String> {
    let _ = host::take_error(); // clear any stale fault from a prior run
    let names = chunk.names.clone();
    let mut vm = VM::new(chunk);
    host::install(&mut vm);
    // After `install`, which resets the object heap the `args` list lives on.
    host::bind_script_args(&mut vm, &names, argv);
    vm.set_sited_numeric_hook(std::sync::Arc::new(host::sited_numeric_hook));
    vm.enable_tracing_jit();
    // Publish the VM so the numeric hook can re-enter it for operator overloading
    // (the hook receives no VM handle); cleared once the run returns.
    host::set_vm_ptr(&mut vm);
    let outcome = vm.run();
    host::clear_vm_ptr();
    match outcome {
        // A runtime fault raised inside an FFI builtin (block compile failure or a
        // call to an unregistered export) halts the VM and parks its message here.
        VMResult::Ok(v) => match host::take_error() {
            Some(e) => Err(e),
            None => Ok(v),
        },
        VMResult::Halted => match host::take_error() {
            Some(e) => Err(e),
            None => Ok(vm.stack.last().cloned().unwrap_or(Value::Undef)),
        },
        VMResult::Error(e) => Err(e),
    }
}

/// Compile and run a Groovy source string; return the last VM value.
pub fn run_str(src: &str) -> Result<Value, String> {
    run_chunk(compile(src)?, &[])
}

/// Compile and run a Groovy source string with the launcher arguments the
/// script sees as `args`.
pub fn run_str_with_args(src: &str, argv: &[String]) -> Result<Value, String> {
    run_chunk(compile(src)?, argv)
}

/// Read and run a `.groovy` file.
pub fn run_file(path: &str) -> Result<Value, String> {
    let src =
        std::fs::read_to_string(path).map_err(|e| format!("groovyrs: cannot read {path}: {e}"))?;
    // Groovy names the class it compiles a script file into after the file's
    // stem, and a `MissingPropertyException` on a bare name prints that name.
    // The `-e` entry point has no file and uses `script_from_command_line`,
    // which is what `host` defaults to when this is not called.
    if let Some(stem) = std::path::Path::new(path).file_stem() {
        host::set_script_class(&stem.to_string_lossy());
    }
    run_str(&src)
}

/// Compile `src` and return a human-readable disassembly of the fusevm chunk
/// (for `groovy --disasm`).
pub fn disassemble(src: &str) -> Result<String, String> {
    Ok(compile(src)?.disassemble())
}

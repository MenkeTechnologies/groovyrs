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
pub mod host;
pub mod lexer;
pub mod lsp;
pub mod parser;
pub mod rust_ffi;

pub use banner::version_banner;
use fusevm::{VMResult, Value, VM};

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
/// [`dap::launch`].
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
    vm.set_numeric_hook(std::sync::Arc::new(host::numeric_hook));
    let _ = host::take_error();
    match vm.run() {
        VMResult::Ok(_) | VMResult::Halted => match host::take_error() {
            Some(e) => Err(e),
            None => Ok(()),
        },
        VMResult::Error(e) => Err(e),
    }
}

/// Register the groovyrs builtins + strict numeric hook on a fresh VM, enable
/// the tracing JIT, and run the chunk. Returns the last top-of-stack value.
fn run_chunk(chunk: fusevm::Chunk) -> Result<Value, String> {
    let _ = host::take_error(); // clear any stale fault from a prior run
    let mut vm = VM::new(chunk);
    host::install(&mut vm);
    vm.set_numeric_hook(std::sync::Arc::new(host::numeric_hook));
    vm.enable_tracing_jit();
    match vm.run() {
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
    run_chunk(compile(src)?)
}

/// Read and run a `.groovy` file.
pub fn run_file(path: &str) -> Result<Value, String> {
    let src =
        std::fs::read_to_string(path).map_err(|e| format!("groovyrs: cannot read {path}: {e}"))?;
    run_str(&src)
}

/// Compile `src` and return a human-readable disassembly of the fusevm chunk
/// (for `groovy --disasm`).
pub fn disassemble(src: &str) -> Result<String, String> {
    Ok(compile(src)?.disassemble())
}

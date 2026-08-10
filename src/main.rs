//! The `groovy` binary entry point.
//!
//! Runs a `.groovy` script on fusevm, or serves an introspection flag
//! (`--version`, `--dump-tokens`/`--dump-ast`/`--disasm`). Errors go to stderr
//! in terse `groovyrs: <reason>` form; nothing else is printed.

use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = match groovyrs::cli::parse(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    // Everything after this runs on the interpreter thread: parsing, lowering
    // and execution all recurse on the Rust stack in proportion to user input,
    // and every groovyrs thread-local (heap, class registry, pending exception,
    // script class name) belongs to whichever thread does the work. See
    // `groovyrs::on_interpreter_stack`.
    match groovyrs::on_interpreter_stack(move || serve(cli)) {
        Ok(code) => code,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// Everything the `groovy` binary does once its command line has parsed. Runs on
/// the interpreter thread.
fn serve(cli: groovyrs::cli::Cli) -> ExitCode {
    if cli.show_version {
        println!("{}", groovyrs::version_banner());
        return ExitCode::SUCCESS;
    }
    if cli.show_help {
        print!("{}", groovyrs::cli::USAGE);
        return ExitCode::SUCCESS;
    }

    // `--lsp`/`--dap` speak their protocols on stdio and need no file argument.
    if cli.lsp {
        return match groovyrs::lsp::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        };
    }
    if cli.dap {
        return match groovyrs::dap::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        };
    }

    // `-e <script>` runs an inline string; otherwise read the file argument.
    let src = if let Some(script) = cli.eval.clone() {
        script
    } else {
        let Some(file) = cli.file.clone() else {
            return fail("no input file (try `groovy --help`)");
        };
        // Groovy names the class it compiles a script file into after the
        // file's stem, and that name appears in a `MissingPropertyException` on
        // a bare name. `-e` has no file and keeps the default,
        // `script_from_command_line` — the same distinction Groovy makes.
        if let Some(stem) = std::path::Path::new(&file).file_stem() {
            groovyrs::host::set_script_class(&stem.to_string_lossy());
        }
        match std::fs::read_to_string(&file) {
            Ok(s) => s,
            Err(e) => return fail(&format!("cannot read {file}: {e}")),
        }
    };

    if cli.dump_tokens {
        return finish(dump_tokens(&src));
    }
    if cli.dump_ast {
        return finish(dump_ast(&src));
    }
    if cli.disasm {
        return finish(groovyrs::disassemble(&src));
    }
    if cli.tiers {
        return match groovyrs::tiers::report(&src) {
            Ok(r) => {
                println!("{r}");
                ExitCode::SUCCESS
            }
            Err(e) => fail(&e),
        };
    }

    match groovyrs::run_str_with_args(&src, &cli.argv) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
}

fn dump_tokens(src: &str) -> Result<String, String> {
    // Rewrite any inline `rust { ... }` block before lexing, matching the parse
    // path, so `--dump-tokens` shows the tokens actually fed to the parser.
    let src = groovyrs::rust_ffi::desugar(src);
    let toks = groovyrs::lexer::lex(&src)?;
    let mut out = String::new();
    for t in toks {
        out.push_str(&format!("{:>4}  {:?}\n", t.line, t.kind));
    }
    Ok(out)
}

fn dump_ast(src: &str) -> Result<String, String> {
    let prog = groovyrs::parse(src)?;
    Ok(format!("{prog:#?}\n"))
}

fn finish(r: Result<String, String>) -> ExitCode {
    match r {
        Ok(s) => {
            print!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e),
    }
}

fn fail(msg: &str) -> ExitCode {
    let msg = msg.strip_prefix("groovyrs: ").unwrap_or(msg);
    eprintln!("groovyrs: {msg}");
    ExitCode::FAILURE
}

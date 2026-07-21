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

    if cli.show_version {
        println!("{}", groovyrs::version_banner());
        return ExitCode::SUCCESS;
    }
    if cli.show_help {
        print!("{}", groovyrs::cli::USAGE);
        return ExitCode::SUCCESS;
    }

    // `-e <script>` runs an inline string; otherwise read the file argument.
    let src = if let Some(script) = cli.eval.clone() {
        script
    } else {
        let Some(file) = cli.file.clone() else {
            return fail("no input file (try `groovy --help`)");
        };
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

    match groovyrs::run_str(&src) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
}

fn dump_tokens(src: &str) -> Result<String, String> {
    let toks = groovyrs::lexer::lex(src)?;
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

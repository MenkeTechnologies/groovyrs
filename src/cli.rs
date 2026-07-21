//! Command-line parsing for the `groovy` binary.
//!
//! Slice 1 accepts a single-file invocation, an inline `-e <script>`, and a
//! small set of introspection flags. The `groovy` launcher's full option grammar
//! (`-cp`, `-classpath`, `-D…`) grows in later waves; unknown options error
//! rather than being silently ignored.

/// Parsed command line.
#[derive(Debug, Default)]
pub struct Cli {
    /// The `.groovy` file to run, if any.
    pub file: Option<String>,
    /// `-e <script>` — an inline script string to run instead of a file.
    pub eval: Option<String>,
    /// Program arguments after the file (become `args` — unused in slice 1).
    pub argv: Vec<String>,
    pub show_version: bool,
    pub show_help: bool,
    /// `--dump-tokens FILE` — print the lexer token stream and exit.
    pub dump_tokens: bool,
    /// `--dump-ast FILE` — print the parsed AST and exit.
    pub dump_ast: bool,
    /// `--disasm FILE` — print the lowered fusevm bytecode and exit.
    pub disasm: bool,
    /// `--lsp` — speak the Language Server Protocol over stdio.
    pub lsp: bool,
    /// `--dap` — speak the Debug Adapter Protocol over stdio.
    pub dap: bool,
}

/// Parse process args (excluding `argv[0]`).
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let argv: Vec<String> = args.into_iter().collect();
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        match a.as_str() {
            "--version" | "-version" | "-v" => cli.show_version = true,
            "--help" | "-h" | "-help" | "-?" => cli.show_help = true,
            "--dump-tokens" => cli.dump_tokens = true,
            "--dump-ast" => cli.dump_ast = true,
            "--disasm" => cli.disasm = true,
            "--lsp" => cli.lsp = true,
            "--dap" => cli.dap = true,
            "-e" | "--eval" => {
                i += 1;
                let script = argv.get(i).ok_or_else(|| {
                    "groovyrs: option `-e` requires a script argument".to_string()
                })?;
                cli.eval = Some(script.clone());
            }
            _ if a.starts_with('-') && cli.file.is_none() => {
                return Err(format!("groovyrs: unrecognized option `{a}`"))
            }
            _ => {
                if cli.file.is_none() {
                    cli.file = Some(a.clone());
                } else {
                    cli.argv.push(a.clone());
                }
            }
        }
        i += 1;
    }
    Ok(cli)
}

/// `groovy --help` text.
pub const USAGE: &str = "\
usage: groovy [options] <file.groovy> [args...]

options:
  -e, --eval SCRIPT         run an inline script string and exit
  -v, -version, --version   print the version banner and exit
  -h, --help                print this help and exit
  --dump-tokens FILE        print the lexer token stream and exit
  --dump-ast FILE           print the parsed AST and exit
  --disasm FILE             print the lowered fusevm bytecode and exit
  --lsp                     speak the Language Server Protocol over stdio
  --dap                     speak the Debug Adapter Protocol over stdio
";

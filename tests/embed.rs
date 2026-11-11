//! `groovyrs::run_program`: stdout capture, stdin feeding, error and exit
//! handling, and isolation between concurrent calls.

use groovyrs::run_program;

#[test]
fn captures_println_print_and_printf() {
    let out = run_program(
        "println 'hello'\nprint 'a'\nprintf('%d-%s%n', 7, 'x')\nSystem.out.println([1, 2])",
        "",
    );
    assert_eq!(out.unwrap(), "hello\na7-x\n[1, 2]\n");
}

#[test]
fn system_in_reads_input_in_order() {
    let src = "def r = System.in.newReader()\n\
               def first = r.readLine()\n\
               println first.toUpperCase()\n\
               println System.in.readLines().collect { it.toInteger() * 2 }";
    assert_eq!(run_program(src, "abc\n1\n2\n").unwrap(), "ABC\n[2, 4]\n");
}

#[test]
fn errors_and_exit_status() {
    assert!(run_program("println 'x' +", "").is_err());
    let err = run_program("println 'before'\nthrow new RuntimeException('boom')", "").unwrap_err();
    assert!(err.contains("boom"), "{err}");
    assert_eq!(
        run_program("println 'a'\nSystem.exit(0)\nprintln 'b'", "").unwrap(),
        "a\n"
    );
    assert_eq!(
        run_program("System.exit(3)", "").unwrap_err(),
        "exit status 3"
    );
}

#[test]
fn concurrent_calls_keep_their_own_output() {
    let handles: Vec<_> = (0..8)
        .map(|i| {
            std::thread::spawn(move || {
                let src = format!("def n = System.in.newReader().readLine()\n(1..200).each {{ println \"{i}:${{n}}:$it\" }}");
                (i, run_program(&src, &format!("in{i}\n")).unwrap())
            })
        })
        .collect();
    for h in handles {
        let (i, out) = h.join().unwrap();
        let want: String = (1..=200).map(|k| format!("{i}:in{i}:{k}\n")).collect();
        assert_eq!(out, want);
    }
}

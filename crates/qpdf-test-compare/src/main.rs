use std::env;
use std::fs::File;
use std::process::ExitCode;

// `dump_file_to_stdout` gets called from the compare orchestrator in a later
// task; wire the module now so the binary and library share one copy.
#[allow(dead_code)]
mod output;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    run(&args)
}

fn run(args: &[String]) -> ExitCode {
    let whoami = program_name(
        args.first()
            .map(String::as_str)
            .unwrap_or("qpdf-test-compare"),
    );
    if args.len() == 2 && args[1] == "--version" {
        println!("{whoami} from flpdf version {}", flpdf::version());
        return ExitCode::from(0);
    }
    // Accept exactly `actual expected` or `actual expected password`. Anything
    // else prints the usage block and exits 2 (matching qpdf's oracle).
    if args.len() < 3 || args.len() > 4 {
        usage(whoami);
        return ExitCode::from(2);
    }
    let actual_path = args[1].as_str();
    // Scaffold for later tasks: touch the actual file so a missing-input error
    // exercises the panic-free reporting path. The real compare pipeline
    // replaces this in a follow-up task.
    match File::open(actual_path) {
        Ok(_) => {
            eprintln!("{whoami}: comparison not yet implemented");
            ExitCode::from(2)
        }
        Err(err) => {
            eprintln!("{whoami}: {err}");
            ExitCode::from(2)
        }
    }
}

fn program_name(argv0: &str) -> &str {
    argv0.rsplit('/').next().unwrap_or(argv0)
}

fn usage(whoami: &str) {
    // Wording copied verbatim from qpdf's compare-for-test/qpdf-test-compare
    // so the harness behaves identically when scripts scrape stderr.
    eprintln!("Usage: {whoami} actual expected");
    eprintln!(r#"Where "actual" is the actual output and "expected" is the expected"#);
    eprintln!("output of a test, compare the two PDF files. The files are considered");
    eprintln!("to match if all their objects are identical except that, if a stream is");
    eprintln!("compressed with FlateDecode, the uncompressed data must match.");
    eprintln!();
    eprintln!("If the files match, the output is the expected file. Otherwise, it is");
    eprintln!("the actual file. Read comments in the code for rationale.");
}

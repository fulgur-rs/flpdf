use std::env;
use std::process::ExitCode;

// Shared helpers live in the library crate; the binary reaches them via
// `use qpdf_test_compare::...`. Duplicating them with `mod output;` here would
// compile them twice into the binary and add dead-code the compiler can
// rightly warn about.

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
    let expected_path = args[2].as_str();
    let password: &[u8] = args.get(3).map(|s| s.as_bytes()).unwrap_or(b"");
    // qpdf's `QPDF_COMPARE_WHY` env: when set, print the diff reason to
    // stderr and skip the actual-file stdout dump. Presence is enough — the
    // value is ignored (matches `getenv` != nullptr).
    let show_why = env::var_os("QPDF_COMPARE_WHY").is_some();

    // qpdf's oracle reads the file twice: once via QPDF for parsing, then
    // again as a raw byte stream for the stdout dump. We do the same — the
    // second read guarantees stdout is byte-verbatim (no re-serialization).
    let actual_bytes = match std::fs::read(actual_path) {
        Ok(b) => b,
        Err(err) => {
            eprintln!("{whoami}: {err}");
            return ExitCode::from(2);
        }
    };
    let expected_bytes = match std::fs::read(expected_path) {
        Ok(b) => b,
        Err(err) => {
            eprintln!("{whoami}: {err}");
            return ExitCode::from(2);
        }
    };

    let diff = match qpdf_test_compare::compare_files(&actual_bytes, &expected_bytes, password) {
        Ok(d) => d,
        Err(err) => {
            // qpdf's `main()` catches `std::exception` from `compare()` (e.g.
            // parse errors, decode failures) and exits 2 with stderr text and
            // NO stdout output. Match that shape.
            eprintln!("{whoami}: {err}");
            return ExitCode::from(2);
        }
    };

    let (to_output, is_diff) = match diff {
        // difference.empty() -> cat the expected file verbatim, exit 0.
        None => (expected_path, false),
        Some(reason) => {
            if show_why {
                // WHY mode: reason to stderr, skip the stdout dump entirely.
                eprintln!("{reason}");
                return ExitCode::from(2);
            }
            // Default: cat the actual file verbatim, then exit 2.
            (actual_path, true)
        }
    };

    if let Err(err) = qpdf_test_compare::output::dump_file_to_stdout(to_output) {
        eprintln!("{whoami}: {err}");
        return ExitCode::from(2);
    }
    if is_diff {
        ExitCode::from(2)
    } else {
        ExitCode::from(0)
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

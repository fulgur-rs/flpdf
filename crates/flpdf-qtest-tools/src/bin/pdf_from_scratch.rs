//! qpdf `pdf_from_scratch` test-helper process boundary.

use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().collect();
    if args.len() != 2 {
        eprintln!("Usage: pdf_from_scratch n");
        return ExitCode::from(2);
    }

    let test_number = match args[1].to_string_lossy().parse::<i32>() {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    if test_number != 0 {
        eprintln!("invalid test {test_number}");
        return ExitCode::from(2);
    }
    match flpdf_qtest_tools::document_construction::run_from_scratch(test_number) {
        Ok(()) => {
            println!("test {test_number} done");
            ExitCode::from(0)
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(2)
        }
    }
}

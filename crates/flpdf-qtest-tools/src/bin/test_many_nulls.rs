//! qpdf `test_many_nulls` document-generator process boundary.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().collect();
    if args.len() != 2 {
        eprintln!("Usage: test_many_nulls outfile.pdf");
        return ExitCode::from(2);
    }

    match flpdf_qtest_tools::document_construction::run_many_nulls(PathBuf::from(&args[1])) {
        Ok(()) => ExitCode::from(0),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(2)
        }
    }
}

use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

fn report_write_error(error: io::Error) -> ExitCode {
    let mut stderr = io::stderr().lock();
    let _ = writeln!(stderr, "write output: {error}");
    let _ = stderr.flush();
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().collect();
    if args.len() != 2 {
        eprintln!("usage: test_xref INPUT.pdf");
        return ExitCode::from(2);
    }

    let path = PathBuf::from(&args[1]);
    match flpdf_qtest_tools::metadata::format_xref_with_diagnostics(&path) {
        Ok((output, warnings)) => {
            flpdf_qtest_tools::metadata::write_metadata_output(&output, &warnings)
                .map_or_else(report_write_error, |_| ExitCode::from(0))
        }
        Err(error) => {
            let mut stderr = io::stderr().lock();
            let message = flpdf_qtest_tools::metadata::display_error(&path, &error);
            let _ = stderr.write_all(&message);
            let _ = stderr.write_all(b"\n");
            let _ = stderr.flush();
            ExitCode::from(2)
        }
    }
}

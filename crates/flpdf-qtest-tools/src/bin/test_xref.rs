use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().collect();
    if args.len() != 2 {
        eprintln!("usage: test_xref INPUT.pdf");
        return ExitCode::from(2);
    }

    let path = PathBuf::from(&args[1]);
    match flpdf_qtest_tools::metadata::format_xref_with_diagnostics(&path) {
        Ok((output, warnings)) => {
            eprint!("{warnings}");
            print!("{output}");
            ExitCode::from(0)
        }
        Err(error) => {
            eprintln!(
                "{}",
                flpdf_qtest_tools::metadata::display_error(&path, &error)
            );
            ExitCode::from(2)
        }
    }
}

use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().collect();
    match flpdf_qtest_tools::large_file::run(&args) {
        Ok(output) => {
            if let Err(error) = io::stdout().lock().write_all(&output) {
                let _ = writeln!(io::stderr().lock(), "{error}");
                return ExitCode::from(2);
            }
            ExitCode::from(0)
        }
        Err(error) => {
            let mut stdout = io::stdout().lock();
            let _ = stdout.write_all(&error.output);
            let _ = stdout.flush();
            let _ = writeln!(io::stderr().lock(), "{}", error.message);
            ExitCode::from(2)
        }
    }
}

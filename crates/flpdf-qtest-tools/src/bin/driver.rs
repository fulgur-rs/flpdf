use std::env;
use std::io;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    ExitCode::from(flpdf_qtest_tools::driver::run(&args, &mut out, &mut err))
}

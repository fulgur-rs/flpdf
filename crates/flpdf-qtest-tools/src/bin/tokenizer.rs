use std::env;
use std::ffi::OsString;
use std::io;
use std::process::ExitCode;

use flpdf_qtest_tools::tokenizer_runner::{run, RunOutcome};

fn main() -> ExitCode {
    let args: Vec<OsString> = env::args_os().collect();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    match run(&args, &mut out, &mut err) {
        RunOutcome::Exit(status) => ExitCode::from(status),
    }
}

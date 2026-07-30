use std::env;
use std::ffi::OsString;
use std::io;
use std::process::ExitCode;

use flpdf_qtest_tools::character_encoding::{finish, run_pdf_doc_encoding};

fn main() -> ExitCode {
    let args: Vec<OsString> = env::args_os().collect();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    finish(run_pdf_doc_encoding(&args, &mut out, &mut err), &mut err)
}

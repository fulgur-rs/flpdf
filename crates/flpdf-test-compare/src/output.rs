use std::fs::File;
use std::io::{self, Read, Write};

/// Copy `path` verbatim to stdout in 2 KiB chunks (matches qpdf-test-compare's
/// output loop). Returns any file-open or I/O error to the caller.
pub fn dump_file_to_stdout(path: &str) -> io::Result<()> {
    let mut f = File::open(path)?;
    let mut buf = [0u8; 2048];
    let stdout = io::stdout();
    let mut out = stdout.lock();
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        out.write_all(&buf[..n])?;
    }
}

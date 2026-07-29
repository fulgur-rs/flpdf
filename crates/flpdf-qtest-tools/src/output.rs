use std::fs::File;
use std::io::{self, Read, Write};

/// Write every byte to the supplied output, retrying short writes.
pub fn write_bytes(out: &mut dyn Write, bytes: &[u8]) -> io::Result<()> {
    out.write_all(bytes)
}

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

#[cfg(test)]
mod tests {
    use super::write_bytes;
    use std::io::{self, Write};

    #[derive(Default)]
    struct ShortWriter {
        bytes: Vec<u8>,
    }

    impl Write for ShortWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let count = buf.len().min(2);
            self.bytes.extend_from_slice(&buf[..count]);
            Ok(count)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn write_bytes_retries_short_writes_until_all_bytes_are_written() {
        let mut out = ShortWriter::default();
        write_bytes(&mut out, b"abcdef").expect("write bytes");
        assert_eq!(out.bytes, b"abcdef");
    }
}

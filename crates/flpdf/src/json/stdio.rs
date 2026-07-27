//! qpdf correspondence: Pl_StdioFile.cc write and finish semantics for JSON side files.

use std::io::{self, Write};

const BUFFER_CAPACITY: usize = 4096;
const EBADF_ERRNO: i32 = 9;

pub(crate) struct QpdfStdioWriter<W> {
    inner: W,
    buffer: Vec<u8>,
}

impl<W: Write> QpdfStdioWriter<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self {
            inner,
            buffer: Vec::with_capacity(BUFFER_CAPACITY),
        }
    }

    fn drain_for_write(&mut self) -> io::Result<()> {
        self.inner.write_all(&self.buffer)?;
        self.buffer.clear();
        Ok(())
    }

    fn ignore_unless_ebadf(error: io::Error) -> io::Result<()> {
        if error.raw_os_error() == Some(EBADF_ERRNO) {
            Err(error)
        } else {
            Ok(())
        }
    }

    pub(crate) fn finish(&mut self) -> io::Result<()> {
        if let Err(error) = self.inner.write_all(&self.buffer) {
            self.buffer.clear();
            return Self::ignore_unless_ebadf(error);
        }
        self.buffer.clear();
        match self.inner.flush() {
            Ok(()) => Ok(()),
            Err(error) => Self::ignore_unless_ebadf(error),
        }
    }
}

impl<W: Write> Write for QpdfStdioWriter<W> {
    fn write(&mut self, mut input: &[u8]) -> io::Result<usize> {
        let input_len = input.len();

        if !self.buffer.is_empty() {
            let available = BUFFER_CAPACITY - self.buffer.len();
            let copied = available.min(input.len());
            self.buffer.extend_from_slice(&input[..copied]);
            input = &input[copied..];
            if self.buffer.len() == BUFFER_CAPACITY {
                self.drain_for_write()?;
            }
        }

        while input.len() >= BUFFER_CAPACITY {
            self.inner.write_all(&input[..BUFFER_CAPACITY])?;
            input = &input[BUFFER_CAPACITY..];
        }

        self.buffer.extend_from_slice(input);
        Ok(input_len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.drain_for_write()?;
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct ProbeSink {
        bytes: Vec<u8>,
        write_calls: Vec<usize>,
        flush_calls: usize,
        write_errno: Option<i32>,
        flush_errno: Option<i32>,
    }

    impl Write for ProbeSink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.write_calls.push(bytes.len());
            if let Some(errno) = self.write_errno {
                return Err(io::Error::from_raw_os_error(errno));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flush_calls += 1;
            if let Some(errno) = self.flush_errno {
                return Err(io::Error::from_raw_os_error(errno));
            }
            Ok(())
        }
    }

    #[test]
    fn final_enospc_below_stdio_boundary_is_ignored() {
        let sink = ProbeSink {
            write_errno: Some(28),
            ..ProbeSink::default()
        };
        let mut writer = QpdfStdioWriter::new(sink);
        writer.write_all(&vec![b'x'; 4095]).unwrap();
        assert!(writer.finish().is_ok());
        assert_eq!(writer.inner.write_calls, [4095]);
        assert!(writer.inner.bytes.is_empty());
        assert_eq!(writer.inner.flush_calls, 0);
    }

    #[test]
    fn enospc_at_stdio_boundary_is_an_ordinary_write_error() {
        let sink = ProbeSink {
            write_errno: Some(28),
            ..ProbeSink::default()
        };
        let mut writer = QpdfStdioWriter::new(sink);
        let error = writer.write_all(&vec![b'x'; 4096]).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(28));
        assert_eq!(writer.inner.write_calls, [4096]);
        assert!(writer.inner.bytes.is_empty());
        assert_eq!(writer.inner.flush_calls, 0);
    }

    #[test]
    fn final_ebadf_remains_fatal() {
        let sink = ProbeSink {
            write_errno: Some(EBADF_ERRNO),
            ..ProbeSink::default()
        };
        let mut writer = QpdfStdioWriter::new(sink);
        writer.write_all(b"x").unwrap();
        let error = writer.finish().unwrap_err();
        assert_eq!(error.raw_os_error(), Some(EBADF_ERRNO));
        assert_eq!(writer.inner.write_calls, [1]);
        assert!(writer.inner.bytes.is_empty());
        assert_eq!(writer.inner.flush_calls, 0);
    }

    #[test]
    fn non_ebadf_underlying_flush_error_is_ignored() {
        let sink = ProbeSink {
            flush_errno: Some(28),
            ..ProbeSink::default()
        };
        let mut writer = QpdfStdioWriter::new(sink);
        writer.write_all(b"payload").unwrap();
        assert!(writer.finish().is_ok());
        assert_eq!(writer.inner.write_calls, [7]);
        assert_eq!(writer.inner.bytes, b"payload");
        assert_eq!(writer.inner.flush_calls, 1);
    }

    #[test]
    fn writes_above_boundary_preserve_all_normal_file_bytes() {
        let mut writer = QpdfStdioWriter::new(ProbeSink::default());
        let payload = vec![b'x'; 4097];
        writer.write_all(&payload).unwrap();
        writer.finish().unwrap();
        assert_eq!(writer.inner.write_calls, [4096, 1]);
        assert_eq!(writer.inner.bytes, payload);
        assert_eq!(writer.inner.flush_calls, 1);
    }
}

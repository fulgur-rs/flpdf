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

    fn drain_buffer(&mut self) -> Result<usize, (usize, io::Error)> {
        let mut written = 0;
        while !self.buffer.is_empty() {
            match self.inner.write(&self.buffer) {
                Ok(0) => {
                    return Err((
                        written,
                        io::Error::new(io::ErrorKind::WriteZero, "failed to write buffered data"),
                    ));
                }
                Ok(count) => {
                    self.buffer.drain(..count);
                    written += count;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err((written, error)),
            }
        }
        Ok(written)
    }

    fn ignore_unless_ebadf(error: io::Error) -> io::Result<()> {
        if error.raw_os_error() == Some(EBADF_ERRNO) {
            Err(error)
        } else {
            Ok(())
        }
    }

    pub(crate) fn finish(&mut self) -> io::Result<()> {
        if let Err((_, error)) = self.drain_buffer() {
            self.buffer.clear();
            return Self::ignore_unless_ebadf(error);
        }
        match self.inner.flush() {
            Ok(()) => Ok(()),
            Err(error) => Self::ignore_unless_ebadf(error),
        }
    }
}

impl<W: Write> Write for QpdfStdioWriter<W> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if input.is_empty() {
            return Ok(0);
        }

        let old_len = self.buffer.len();
        let copied = (BUFFER_CAPACITY - old_len).min(input.len());
        self.buffer.extend_from_slice(&input[..copied]);
        if self.buffer.len() < BUFFER_CAPACITY {
            return Ok(copied);
        }

        match self.drain_buffer() {
            Ok(_) => Ok(copied),
            Err((written, error)) => {
                let current_written = written.saturating_sub(old_len);
                let remaining_old = old_len.saturating_sub(written);
                self.buffer.truncate(remaining_old);
                if current_written == 0 {
                    Err(error)
                } else {
                    Ok(current_written)
                }
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Err((_, error)) = self.drain_buffer() {
            return Err(error);
        }
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

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

    enum WriteStep {
        Accept(usize),
        Error(i32),
        Interrupted,
        Zero,
    }

    #[derive(Default)]
    struct ScriptedSink {
        bytes: Vec<u8>,
        write_calls: Vec<Vec<u8>>,
        flush_calls: usize,
        write_steps: VecDeque<WriteStep>,
    }

    impl ScriptedSink {
        fn with_write_steps(steps: impl IntoIterator<Item = WriteStep>) -> Self {
            Self {
                write_steps: steps.into_iter().collect(),
                ..Self::default()
            }
        }
    }

    impl Write for ScriptedSink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.write_calls.push(bytes.to_vec());
            match self.write_steps.pop_front() {
                Some(WriteStep::Accept(limit)) => {
                    let written = limit.min(bytes.len());
                    self.bytes.extend_from_slice(&bytes[..written]);
                    Ok(written)
                }
                Some(WriteStep::Error(errno)) => Err(io::Error::from_raw_os_error(errno)),
                Some(WriteStep::Interrupted) => Err(io::ErrorKind::Interrupted.into()),
                Some(WriteStep::Zero) => Ok(0),
                None => {
                    self.bytes.extend_from_slice(bytes);
                    Ok(bytes.len())
                }
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flush_calls += 1;
            Ok(())
        }
    }

    fn write_call_lengths(sink: &ScriptedSink) -> Vec<usize> {
        sink.write_calls.iter().map(Vec::len).collect()
    }

    #[test]
    fn empty_write_returns_zero_without_touching_the_sink() {
        let mut writer =
            QpdfStdioWriter::new(ScriptedSink::with_write_steps([WriteStep::Error(28)]));

        assert_eq!(writer.write(b"").unwrap(), 0);
        assert!(writer.inner.write_calls.is_empty());
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

    #[test]
    fn split_boundary_zero_progress_error_can_retry_without_duplicate() {
        let sink = ScriptedSink::with_write_steps([WriteStep::Error(28)]);
        let mut writer = QpdfStdioWriter::new(sink);
        writer.write_all(&vec![b'x'; 4095]).unwrap();

        let error = writer.write(b"y").unwrap_err();
        assert_eq!(error.raw_os_error(), Some(28));
        assert_eq!(write_call_lengths(&writer.inner), [4096]);
        assert!(writer.inner.bytes.is_empty());

        writer.write_all(b"y").unwrap();
        writer.finish().unwrap();
        let mut expected = vec![b'x'; 4095];
        expected.push(b'y');
        assert_eq!(writer.inner.bytes, expected);
        assert_eq!(write_call_lengths(&writer.inner), [4096, 4096]);
    }

    #[test]
    fn full_buffer_retry_propagates_a_second_zero_progress_error() {
        let sink = ScriptedSink::with_write_steps([WriteStep::Error(28), WriteStep::Error(5)]);
        let mut writer = QpdfStdioWriter::new(sink);
        writer.write_all(&vec![b'x'; 4095]).unwrap();

        let first_error = writer.write(b"y").unwrap_err();
        assert_eq!(first_error.raw_os_error(), Some(28));

        let retry_error = writer.write(b"y").unwrap_err();
        assert_eq!(retry_error.raw_os_error(), Some(5));
        assert!(writer.inner.bytes.is_empty());
        assert_eq!(write_call_lengths(&writer.inner), [4096, 4096]);
    }

    #[test]
    fn partial_progress_then_hard_error_reports_current_bytes_written() {
        let sink = ScriptedSink::with_write_steps([WriteStep::Accept(1024), WriteStep::Error(5)]);
        let mut writer = QpdfStdioWriter::new(sink);
        let payload = vec![b'x'; 4096];

        let written = writer.write(&payload).unwrap();
        assert_eq!(written, 1024);
        assert_eq!(writer.inner.bytes, payload[..1024]);
        assert_eq!(write_call_lengths(&writer.inner), [4096, 3072]);

        writer.write_all(&payload[written..]).unwrap();
        writer.finish().unwrap();
        assert_eq!(writer.inner.bytes, payload);
    }

    #[test]
    fn interrupted_underlying_write_is_retried() {
        let sink =
            ScriptedSink::with_write_steps([WriteStep::Interrupted, WriteStep::Accept(4096)]);
        let mut writer = QpdfStdioWriter::new(sink);
        let payload = vec![b'x'; 4096];

        assert_eq!(writer.write(&payload).unwrap(), 4096);
        assert_eq!(writer.inner.bytes, payload);
        assert_eq!(write_call_lengths(&writer.inner), [4096, 4096]);
    }

    #[test]
    fn split_boundary_write_zero_can_retry_without_duplicate() {
        let sink = ScriptedSink::with_write_steps([WriteStep::Zero]);
        let mut writer = QpdfStdioWriter::new(sink);
        writer.write_all(&vec![b'x'; 4095]).unwrap();

        let error = writer.write(b"y").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WriteZero);
        writer.write_all(b"y").unwrap();
        writer.finish().unwrap();

        let mut expected = vec![b'x'; 4095];
        expected.push(b'y');
        assert_eq!(writer.inner.bytes, expected);
        assert_eq!(write_call_lengths(&writer.inner), [4096, 4096]);
    }

    #[test]
    fn explicit_flush_error_is_strict_and_retry_does_not_duplicate_prefix() {
        let sink = ScriptedSink::with_write_steps([WriteStep::Accept(3), WriteStep::Error(28)]);
        let mut writer = QpdfStdioWriter::new(sink);
        writer.write_all(b"payload").unwrap();

        let error = writer.flush().unwrap_err();
        assert_eq!(error.raw_os_error(), Some(28));
        assert_eq!(writer.inner.bytes, b"pay");

        writer.flush().unwrap();
        assert_eq!(writer.inner.bytes, b"payload");
        assert_eq!(write_call_lengths(&writer.inner), [7, 4, 4]);
        assert_eq!(writer.inner.flush_calls, 1);
    }

    #[test]
    fn repeated_finish_does_not_write_buffered_bytes_twice() {
        let mut writer = QpdfStdioWriter::new(ScriptedSink::default());
        writer.write_all(b"payload").unwrap();

        writer.finish().unwrap();
        writer.finish().unwrap();

        assert_eq!(writer.inner.bytes, b"payload");
        assert_eq!(write_call_lengths(&writer.inner), [7]);
        assert_eq!(writer.inner.flush_calls, 2);
    }
}

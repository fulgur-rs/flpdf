//! qpdf correspondence: Pl_OStream.cc terminal adapter for a writer.

use std::io::Write;

use super::{Pipeline, PipelineResult};

pub struct PlOStream<W: Write> {
    identifier: String,
    writer: W,
    failed: bool,
}

impl<W: Write> PlOStream<W> {
    pub fn new(identifier: impl Into<String>, writer: W) -> Self {
        Self {
            identifier: identifier.into(),
            writer,
            failed: false,
        }
    }
}

impl<W: Write> Pipeline for PlOStream<W> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        if !self.failed && self.writer.write_all(data).is_err() {
            self.failed = true;
        }
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        if !self.failed && self.writer.flush().is_err() {
            self.failed = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::{self, Write};

    use super::PlOStream;
    use crate::pipeline::Pipeline;

    enum WriteStep {
        Accept(usize),
        Error(io::ErrorKind),
    }

    enum FlushStep {
        Succeed,
        Error(io::ErrorKind),
    }

    struct ScriptedWriter {
        write_steps: VecDeque<WriteStep>,
        flush_steps: VecDeque<FlushStep>,
        bytes: Vec<u8>,
        write_calls: usize,
        flush_calls: usize,
    }

    impl ScriptedWriter {
        fn new(write_steps: impl IntoIterator<Item = WriteStep>) -> Self {
            Self {
                write_steps: write_steps.into_iter().collect(),
                flush_steps: VecDeque::new(),
                bytes: Vec::new(),
                write_calls: 0,
                flush_calls: 0,
            }
        }

        fn with_flush_steps(
            write_steps: impl IntoIterator<Item = WriteStep>,
            flush_steps: impl IntoIterator<Item = FlushStep>,
        ) -> Self {
            Self {
                write_steps: write_steps.into_iter().collect(),
                flush_steps: flush_steps.into_iter().collect(),
                bytes: Vec::new(),
                write_calls: 0,
                flush_calls: 0,
            }
        }
    }

    impl Write for ScriptedWriter {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.write_calls += 1;
            match self.write_steps.pop_front() {
                Some(WriteStep::Accept(size)) => {
                    let size = size.min(data.len());
                    self.bytes.extend_from_slice(&data[..size]);
                    Ok(size)
                }
                Some(WriteStep::Error(kind)) => Err(io::Error::from(kind)),
                None => {
                    self.bytes.extend_from_slice(data);
                    Ok(data.len())
                }
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flush_calls += 1;
            match self.flush_steps.pop_front() {
                Some(FlushStep::Succeed) | None => Ok(()),
                Some(FlushStep::Error(kind)) => Err(io::Error::from(kind)),
            }
        }
    }

    #[test]
    fn writer_error_is_sticky_and_nonfatal() {
        let mut writer =
            ScriptedWriter::new([WriteStep::Accept(2), WriteStep::Error(io::ErrorKind::Other)]);
        {
            let mut stage = PlOStream::new("ostream", &mut writer);
            assert!(stage.write(b"abcd").is_ok());
            assert!(stage.write(b"later").is_ok());
            assert!(stage.finish().is_ok());
        }
        assert_eq!(writer.bytes, b"ab");
        assert_eq!(writer.write_calls, 2);
        assert_eq!(writer.flush_calls, 0);
    }

    #[test]
    fn successful_writes_and_repeated_finish_reuse_the_external_writer() {
        let mut writer = ScriptedWriter::with_flush_steps(
            [WriteStep::Accept(2), WriteStep::Accept(2)],
            [FlushStep::Succeed, FlushStep::Succeed],
        );
        {
            let mut stage = PlOStream::new("ostream", &mut writer);
            stage.write(b"ab").unwrap();
            stage.finish().unwrap();
            stage.write(b"cd").unwrap();
            stage.finish().unwrap();
        }
        assert_eq!(writer.bytes, b"abcd");
        assert_eq!(writer.write_calls, 2);
        assert_eq!(writer.flush_calls, 2);
    }

    #[test]
    fn flush_error_becomes_sticky_and_nonfatal() {
        let mut writer = ScriptedWriter::with_flush_steps(
            [WriteStep::Accept(2)],
            [FlushStep::Error(io::ErrorKind::Other)],
        );
        {
            let mut stage = PlOStream::new("ostream", &mut writer);
            stage.finish().unwrap();
            stage.write(b"ab").unwrap();
            stage.finish().unwrap();
        }
        assert_eq!(writer.bytes, b"");
        assert_eq!(writer.write_calls, 0);
        assert_eq!(writer.flush_calls, 1);
    }

    #[test]
    fn empty_write_after_failure_is_a_noop() {
        let mut writer = ScriptedWriter::new([WriteStep::Error(io::ErrorKind::Other)]);
        {
            let mut stage = PlOStream::new("ostream", &mut writer);
            stage.write(b"fail").unwrap();
            stage.write(b"").unwrap();
        }
        assert_eq!(writer.bytes, b"");
        assert_eq!(writer.write_calls, 1);
        assert_eq!(writer.flush_calls, 0);
    }

    #[test]
    fn dropping_pl_ostream_does_not_flush_or_close() {
        let mut writer = ScriptedWriter::new(std::iter::empty::<WriteStep>());
        {
            let mut stage = PlOStream::new("ostream", &mut writer);
            stage.write(b"ab").unwrap();
        }
        assert_eq!(writer.bytes, b"ab");
        assert_eq!(writer.flush_calls, 0);
    }
}

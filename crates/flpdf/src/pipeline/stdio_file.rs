//! qpdf correspondence: Pl_StdioFile.cc partial-write, error, and finish semantics for an externally owned writer.
//!
//! Partial progress is retried, but writer errors (including `Interrupted`) are
//! reported immediately, matching qpdf's zero-result `fwrite` error path.
//! Finish maps only raw `EBADF` to a logic error and ignores other flush
//! failures. `StdioBuffer` supplies the caller-owned 4096-byte stdio boundary
//! without Rust's automatic `Interrupted` retry.

use std::io::{self, Write};

use super::{Pipeline, PipelineError, PipelineResult};

const EBADF_ERRNO: i32 = 9;
const STDIO_BUFFER_CAPACITY: usize = 4096;

pub(crate) struct StdioBuffer<'a> {
    writer: &'a mut dyn Write,
    buffer: Vec<u8>,
    panicked: bool,
}

impl<'a> StdioBuffer<'a> {
    pub(crate) fn new(writer: &'a mut dyn Write) -> Self {
        Self {
            writer,
            buffer: Vec::with_capacity(STDIO_BUFFER_CAPACITY),
            panicked: false,
        }
    }

    fn spare_capacity(&self) -> usize {
        STDIO_BUFFER_CAPACITY - self.buffer.len()
    }

    fn write_buffer_once(&mut self) -> io::Result<usize> {
        self.panicked = true;
        let result = self.writer.write(&self.buffer);
        self.panicked = false;
        result
    }

    fn drain_full_buffer(&mut self) -> io::Result<()> {
        debug_assert_eq!(self.buffer.len(), STDIO_BUFFER_CAPACITY);
        match self.write_buffer_once() {
            Ok(0) => {
                self.buffer.clear();
                Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write buffered data",
                ))
            }
            Ok(count) => {
                self.buffer.drain(..count);
                Ok(())
            }
            Err(error) => {
                self.buffer.clear();
                Err(error)
            }
        }
    }

    fn flush_buffer(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        let buffered = self.buffer.len();
        let result = self.write_buffer_once();
        self.buffer.clear();
        match result {
            Ok(count) if count == buffered => Ok(()),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "failed to write buffered data",
            )),
            Err(error) => Err(error),
        }
    }
}

impl Write for StdioBuffer<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if !self.buffer.is_empty() && data.len() >= self.spare_capacity() {
            let count = self.spare_capacity();
            self.buffer.extend_from_slice(&data[..count]);
            self.drain_full_buffer()?;
            return Ok(count);
        }

        if self.buffer.is_empty() && data.len() >= STDIO_BUFFER_CAPACITY {
            self.panicked = true;
            let result = self.writer.write(data);
            self.panicked = false;
            result
        } else {
            let count = data.len().min(self.spare_capacity());
            self.buffer.extend_from_slice(&data[..count]);
            Ok(count)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_buffer()?;
        self.writer.flush()
    }
}

impl Drop for StdioBuffer<'_> {
    fn drop(&mut self) {
        if !self.panicked {
            let _ = self.flush_buffer();
        }
    }
}

pub struct PlStdioFile<'a> {
    identifier: String,
    writer: &'a mut dyn Write,
}

impl<'a> PlStdioFile<'a> {
    pub fn new(identifier: impl Into<String>, writer: &'a mut dyn Write) -> Self {
        Self {
            identifier: identifier.into(),
            writer,
        }
    }

    fn write_error(&self, source: io::Error) -> PipelineError {
        PipelineError::runtime(format!(
            "{}: Pl_StdioFile::write: {source}",
            self.identifier
        ))
    }
}

impl Pipeline for PlStdioFile<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, mut data: &[u8]) -> PipelineResult<()> {
        while !data.is_empty() {
            match self.writer.write(data) {
                Ok(0) => {
                    let source =
                        io::Error::new(io::ErrorKind::WriteZero, "failed to write buffered data");
                    return Err(self.write_error(source));
                }
                Ok(written) => data = &data[written..],
                Err(source) => return Err(self.write_error(source)),
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        match self.writer.flush() {
            Err(source) if source.raw_os_error() == Some(EBADF_ERRNO) => {
                Err(PipelineError::logic(format!(
                    "{}: Pl_StdioFile::finish: stream already closed",
                    self.identifier
                )))
            }
            Ok(()) | Err(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::{self, Write};

    use super::{PlStdioFile, StdioBuffer};
    use crate::pipeline::{Pipeline, PipelineError};

    const EBADF_ERRNO: i32 = 9;
    const ENOSPC_ERRNO: i32 = 28;

    enum WriteStep {
        Accept(usize),
        Error(io::ErrorKind, &'static str),
        Interrupted,
        Zero,
    }

    enum FlushStep {
        Succeed,
        RawError(i32),
    }

    #[derive(Default)]
    struct ScriptedWriter {
        bytes: Vec<u8>,
        write_inputs: Vec<Vec<u8>>,
        flush_calls: usize,
        write_steps: VecDeque<WriteStep>,
        flush_steps: VecDeque<FlushStep>,
    }

    impl ScriptedWriter {
        fn with_write_steps(steps: impl IntoIterator<Item = WriteStep>) -> Self {
            Self {
                write_steps: steps.into_iter().collect(),
                ..Self::default()
            }
        }

        fn with_flush_steps(steps: impl IntoIterator<Item = FlushStep>) -> Self {
            Self {
                flush_steps: steps.into_iter().collect(),
                ..Self::default()
            }
        }
    }

    impl Write for ScriptedWriter {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.write_inputs.push(data.to_vec());
            match self.write_steps.pop_front() {
                Some(WriteStep::Accept(limit)) => {
                    let written = limit.min(data.len());
                    self.bytes.extend_from_slice(&data[..written]);
                    Ok(written)
                }
                Some(WriteStep::Error(kind, message)) => Err(io::Error::new(kind, message)),
                Some(WriteStep::Interrupted) => Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Interrupted system call",
                )),
                Some(WriteStep::Zero) => Ok(0),
                None => {
                    self.bytes.extend_from_slice(data);
                    Ok(data.len())
                }
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flush_calls += 1;
            match self.flush_steps.pop_front() {
                Some(FlushStep::RawError(errno)) => Err(io::Error::from_raw_os_error(errno)),
                Some(FlushStep::Succeed) | None => Ok(()),
            }
        }
    }

    enum ProbeWriteStep {
        Accept(usize),
        RawError(i32),
        Zero,
    }

    #[derive(Default)]
    struct ProbeSink {
        bytes: Vec<u8>,
        write_lengths: Vec<usize>,
        flush_calls: usize,
        write_steps: VecDeque<ProbeWriteStep>,
    }

    impl ProbeSink {
        fn with_write_steps(steps: impl IntoIterator<Item = ProbeWriteStep>) -> Self {
            Self {
                write_steps: steps.into_iter().collect(),
                ..Self::default()
            }
        }
    }

    impl Write for ProbeSink {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.write_lengths.push(data.len());
            match self.write_steps.pop_front() {
                Some(ProbeWriteStep::Accept(limit)) => {
                    let written = limit.min(data.len());
                    self.bytes.extend_from_slice(&data[..written]);
                    Ok(written)
                }
                Some(ProbeWriteStep::RawError(errno)) => Err(io::Error::from_raw_os_error(errno)),
                Some(ProbeWriteStep::Zero) => Ok(0),
                None => {
                    self.bytes.extend_from_slice(data);
                    Ok(data.len())
                }
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flush_calls += 1;
            Ok(())
        }
    }

    fn input_lengths(writer: &ScriptedWriter) -> Vec<usize> {
        writer.write_inputs.iter().map(Vec::len).collect()
    }

    #[test]
    fn partial_writes_are_retried_until_the_full_input_is_written() {
        let mut writer =
            ScriptedWriter::with_write_steps([WriteStep::Accept(2), WriteStep::Accept(1)]);
        {
            let mut stage = PlStdioFile::new("stdio", &mut writer);
            stage.write(b"abcdef").unwrap();
        }

        assert_eq!(writer.bytes, b"abcdef");
        assert_eq!(input_lengths(&writer), [6, 4, 3]);
    }

    #[test]
    fn interrupted_write_is_reported_without_retry() {
        let mut writer =
            ScriptedWriter::with_write_steps([WriteStep::Interrupted, WriteStep::Accept(3)]);
        let error = {
            let mut stage = PlStdioFile::new("stdio", &mut writer);
            stage.write(b"abc").unwrap_err()
        };

        assert!(matches!(error, PipelineError::Runtime(_)));
        assert_eq!(
            error.to_string(),
            "stdio: Pl_StdioFile::write: Interrupted system call"
        );
        assert!(writer.bytes.is_empty());
        assert_eq!(input_lengths(&writer), [3]);
        assert!(matches!(
            writer.write_steps.front(),
            Some(WriteStep::Accept(3))
        ));
    }

    #[test]
    fn zero_progress_is_runtime_error_with_identifier_and_operation() {
        let mut writer = ScriptedWriter::with_write_steps([WriteStep::Zero]);
        let error = {
            let mut stage = PlStdioFile::new("stdio", &mut writer);
            stage.write(b"abc").unwrap_err()
        };

        assert!(matches!(error, PipelineError::Runtime(_)));
        assert_eq!(
            error.to_string(),
            "stdio: Pl_StdioFile::write: failed to write buffered data"
        );
        assert!(writer.bytes.is_empty());
    }

    #[test]
    fn write_error_is_runtime_error_with_identifier_and_operation() {
        let mut writer =
            ScriptedWriter::with_write_steps([WriteStep::Error(io::ErrorKind::Other, "disk full")]);
        let error = {
            let mut stage = PlStdioFile::new("stdio", &mut writer);
            stage.write(b"abc").unwrap_err()
        };

        assert!(matches!(error, PipelineError::Runtime(_)));
        assert_eq!(error.to_string(), "stdio: Pl_StdioFile::write: disk full");
        assert!(writer.bytes.is_empty());
    }

    #[test]
    fn finish_ebadf_is_exact_stream_already_closed_logic_error() {
        let mut writer = ScriptedWriter::with_flush_steps([FlushStep::RawError(EBADF_ERRNO)]);
        let error = {
            let mut stage = PlStdioFile::new("stdio", &mut writer);
            stage.finish().unwrap_err()
        };

        assert!(matches!(error, PipelineError::Logic(_)));
        assert_eq!(
            error.to_string(),
            "stdio: Pl_StdioFile::finish: stream already closed"
        );
    }

    #[test]
    fn finish_non_ebadf_error_is_ignored() {
        let mut writer = ScriptedWriter::with_flush_steps([FlushStep::RawError(ENOSPC_ERRNO)]);
        {
            let mut stage = PlStdioFile::new("stdio", &mut writer);
            stage.finish().unwrap();
        }

        assert_eq!(writer.flush_calls, 1);
    }

    #[test]
    fn repeated_finish_and_write_after_finish_remain_reusable() {
        let mut writer = ScriptedWriter::with_flush_steps([FlushStep::Succeed, FlushStep::Succeed]);
        {
            let mut stage = PlStdioFile::new("stdio", &mut writer);
            stage.write(b"before").unwrap();
            stage.finish().unwrap();
            stage.write(b"after").unwrap();
            stage.finish().unwrap();
        }

        assert_eq!(writer.bytes, b"beforeafter");
        assert_eq!(writer.flush_calls, 2);
    }

    #[test]
    fn drop_does_not_flush_or_close() {
        let mut writer = ScriptedWriter::default();
        {
            let mut stage = PlStdioFile::new("stdio", &mut writer);
            stage.write(b"payload").unwrap();
        }

        assert_eq!(writer.bytes, b"payload");
        assert_eq!(writer.flush_calls, 0);
    }

    #[test]
    fn buffered_4095_byte_enospc_is_deferred_to_finish_and_ignored() {
        let mut sink = ProbeSink::with_write_steps([ProbeWriteStep::RawError(ENOSPC_ERRNO)]);
        {
            let mut buffered = StdioBuffer::new(&mut sink);
            let mut stage = PlStdioFile::new("stdio", &mut buffered);
            stage.write(&vec![b'x'; 4095]).unwrap();
            stage.finish().unwrap();
        }

        assert_eq!(sink.write_lengths, [4095]);
        assert!(sink.bytes.is_empty());
        assert_eq!(sink.flush_calls, 0);
    }

    #[test]
    fn buffered_short_write_during_finish_is_not_retried() {
        let payload = (0..4095)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let mut sink = ProbeSink::with_write_steps([
            ProbeWriteStep::Accept(1024),
            ProbeWriteStep::Accept(3071),
        ]);
        {
            let mut buffered = StdioBuffer::new(&mut sink);
            let mut stage = PlStdioFile::new("stdio", &mut buffered);
            stage.write(&payload).unwrap();
            stage.finish().unwrap();
        }

        assert_eq!(sink.bytes, payload[..1024]);
        assert_eq!(sink.write_lengths, [4095]);
        assert_eq!(sink.flush_calls, 0);
        assert!(matches!(
            sink.write_steps.front(),
            Some(ProbeWriteStep::Accept(3071))
        ));
    }

    #[test]
    fn buffered_4096_byte_enospc_is_a_write_runtime_error() {
        let mut sink = ProbeSink::with_write_steps([ProbeWriteStep::RawError(ENOSPC_ERRNO)]);
        let error = {
            let mut buffered = StdioBuffer::new(&mut sink);
            let mut stage = PlStdioFile::new("stdio", &mut buffered);
            stage.write(&vec![b'x'; 4096]).unwrap_err()
        };

        assert!(matches!(error, PipelineError::Runtime(_)));
        assert!(error
            .to_string()
            .starts_with("stdio: Pl_StdioFile::write: "));
        assert_eq!(sink.write_lengths, [4096]);
        assert!(sink.bytes.is_empty());
        assert_eq!(sink.flush_calls, 0);
    }

    #[test]
    fn buffered_4097_bytes_preserve_all_successful_bytes() {
        let payload = (0..4097)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let mut sink = ProbeSink::default();
        {
            let mut buffered = StdioBuffer::new(&mut sink);
            let mut stage = PlStdioFile::new("stdio", &mut buffered);
            stage.write(&payload).unwrap();
            stage.finish().unwrap();
        }

        assert_eq!(sink.bytes, payload);
        assert_eq!(sink.write_lengths, [4097]);
        assert_eq!(sink.flush_calls, 1);
    }

    #[test]
    fn buffered_chunk_fills_existing_capacity_before_drain() {
        let tail = (0..4096)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let mut sink = ProbeSink::default();
        {
            let mut buffered = StdioBuffer::new(&mut sink);
            buffered.write_all(b"x").unwrap();
            buffered.write_all(&tail).unwrap();
            buffered.flush().unwrap();
        }

        let mut expected = vec![b'x'];
        expected.extend_from_slice(&tail);
        assert_eq!(sink.bytes, expected);
        assert_eq!(sink.write_lengths, [4096, 1]);
        assert_eq!(sink.flush_calls, 1);
    }

    #[test]
    fn buffered_full_block_zero_progress_is_not_retried() {
        let mut sink =
            ProbeSink::with_write_steps([ProbeWriteStep::Zero, ProbeWriteStep::Accept(4096)]);
        let error = {
            let mut buffered = StdioBuffer::new(&mut sink);
            buffered.write_all(b"x").unwrap();
            buffered.write(&vec![b'y'; 4095]).unwrap_err()
        };

        assert_eq!(error.kind(), io::ErrorKind::WriteZero);
        assert!(sink.bytes.is_empty());
        assert_eq!(sink.write_lengths, [4096]);
        assert_eq!(sink.flush_calls, 0);
        assert!(matches!(
            sink.write_steps.front(),
            Some(ProbeWriteStep::Accept(4096))
        ));
    }

    #[test]
    fn buffered_partial_progress_preserves_the_exact_prefix() {
        let payload = (0..4096)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let mut sink = ProbeSink::with_write_steps([
            ProbeWriteStep::Accept(1024),
            ProbeWriteStep::RawError(ENOSPC_ERRNO),
        ]);
        {
            let mut buffered = StdioBuffer::new(&mut sink);
            let mut stage = PlStdioFile::new("stdio", &mut buffered);
            stage.write(&payload).unwrap();
            stage.finish().unwrap();
        }

        assert_eq!(sink.bytes, payload[..1024]);
        assert_eq!(sink.write_lengths, [4096, 3072]);
        assert_eq!(sink.flush_calls, 0);
    }

    #[test]
    fn buffered_zero_progress_during_finish_is_ignored_without_retry() {
        let mut sink =
            ProbeSink::with_write_steps([ProbeWriteStep::Zero, ProbeWriteStep::Accept(4095)]);
        {
            let mut buffered = StdioBuffer::new(&mut sink);
            let mut stage = PlStdioFile::new("stdio", &mut buffered);
            stage.write(&vec![b'x'; 4095]).unwrap();
            stage.finish().unwrap();
        }

        assert_eq!(sink.write_lengths, [4095]);
        assert!(sink.bytes.is_empty());
        assert!(matches!(
            sink.write_steps.front(),
            Some(ProbeWriteStep::Accept(4095))
        ));
        assert_eq!(sink.flush_calls, 0);
    }
}

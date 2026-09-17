//! Shared logger sinks for info, warning, error, and binary-save output.
//!
//! qpdf correspondence: QPDFLogger.cc shared info, warning, error, and binary-save pipeline routing.
//!
//! qpdf's standard output and error logger sinks use C++ text streams
//! (`QPDFLogger.cc:43-50`). `QPDFLogger::setSave` calls
//! `QUtil::binary_stdout` only when standard output becomes the save sink
//! (`QPDFLogger.cc:197-208`; `QUtil.cc:759-767`).

use crate::pipeline::{Discard, Pipeline, PipelineHandle, PipelineResult, PlOStream};
use crate::{Error, Result};
use std::collections::HashMap;
use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::ThreadId;

const NULL_PIPELINE_MESSAGE: &str =
    "QPDFLogger: requested a null pipeline without null_okay == true";
const STDOUT_ALREADY_USED_MESSAGE: &str =
    "QPDFLogger: called setSave on standard output after standard output has already been used";

/// Reproduce the C runtime's text-mode newline conversion for logger output.
///
/// qpdf keeps stdout and stderr in text mode for diagnostics, but switches
/// stdout to binary mode when it becomes the save destination. Rust's standard
/// streams do not perform that Windows conversion, so the logger owns the
/// conversion state and turns it off when `set_save` selects standard output.
struct TextModeWriter<W> {
    writer: W,
    text_mode: Arc<AtomicBool>,
}

impl<W> TextModeWriter<W> {
    fn new(writer: W, text_mode: Arc<AtomicBool>) -> Self {
        Self { writer, text_mode }
    }
}

impl<W: Write> Write for TextModeWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if !self.text_mode.load(Ordering::Relaxed) || !data.contains(&b'\n') {
            return self.writer.write(data);
        }

        let mut converted =
            Vec::with_capacity(data.len() + data.iter().filter(|&&b| b == b'\n').count());
        for &byte in data {
            if byte == b'\n' {
                converted.extend_from_slice(b"\r\n");
            } else {
                converted.push(byte);
            }
        }
        self.writer.write_all(&converted)?;
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

const STDOUT_LINE_BUFFER_CAPACITY: usize = 4096;

/// Aggregate logger fragments until a complete line is available.
///
/// qpdf configures the C stdout stream as line-buffered before constructing
/// its `Pl_OStream` (`qpdf/qpdf.cc:30`; `QUtil.cc:780-784`). Rust's
/// `std::io::Stdout` also has a line-oriented adapter, but it forwards a
/// pending partial line and the newly completed line as separate writes. The
/// C stdio boundary used by qpdf combines those fragments before the flush.
struct LineBufferedWriter<W> {
    writer: W,
    buffer: Vec<u8>,
}

impl<W> LineBufferedWriter<W> {
    fn new(writer: W) -> Self {
        Self {
            writer,
            buffer: Vec::with_capacity(STDOUT_LINE_BUFFER_CAPACITY),
        }
    }

    fn flush_prefix(&mut self, length: usize) -> io::Result<()>
    where
        W: Write,
    {
        if length == 0 {
            return Ok(());
        }
        self.writer.write_all(&self.buffer[..length])?;
        self.buffer.drain(..length);
        Ok(())
    }

    #[cfg(test)]
    fn into_inner(self) -> W {
        self.writer
    }
}

impl<W: Write> Write for LineBufferedWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut remaining = data;
        while !remaining.is_empty() || self.buffer.len() >= STDOUT_LINE_BUFFER_CAPACITY {
            if self.buffer.len() >= STDOUT_LINE_BUFFER_CAPACITY {
                self.flush_prefix(STDOUT_LINE_BUFFER_CAPACITY)?;
                continue;
            }

            let available = STDOUT_LINE_BUFFER_CAPACITY - self.buffer.len();
            let chunk_length = remaining.len().min(available);
            self.buffer.extend_from_slice(&remaining[..chunk_length]);
            remaining = &remaining[chunk_length..];

            if let Some(newline) = self.buffer.iter().rposition(|&byte| byte == b'\n') {
                self.flush_prefix(newline + 1)?;
            }
        }

        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_prefix(self.buffer.len())?;
        self.writer.flush()
    }
}

fn standard_output_writer<W: Write>(
    writer: W,
    text_mode: Arc<AtomicBool>,
) -> TextModeWriter<LineBufferedWriter<W>> {
    TextModeWriter::new(LineBufferedWriter::new(writer), text_mode)
}

struct PlTrack {
    next: PipelineHandle,
    used: Arc<AtomicBool>,
}

impl Pipeline for PlTrack {
    fn identifier(&self) -> &str {
        "track stdout"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.used.store(true, Ordering::Relaxed);
        self.next.write(data)
    }

    fn finish(&mut self) -> PipelineResult<()> {
        self.next.finish()
    }
}

struct LoggerState {
    discard: PipelineHandle,
    stdout: PipelineHandle,
    stderr: PipelineHandle,
    info: PipelineHandle,
    warn: Option<PipelineHandle>,
    error: PipelineHandle,
    // qpdf-deviation: qpdf has no thread-scoped overlay on the default logger; its answer to per-thread capture is a separate QPDFLogger instance (`include/qpdf/QPDFLogger.hh:33-42`), which the contextless `defaultLogger()->getError()` warning path cannot use
    error_capture: HashMap<ThreadId, PipelineHandle>,
    save: Option<PipelineHandle>,
    stdout_used: Arc<AtomicBool>,
    stdout_text_mode: Arc<AtomicBool>,
}

struct LoggerShared {
    state: Mutex<LoggerState>,
}

impl LoggerShared {
    fn lock(&self) -> MutexGuard<'_, LoggerState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for LoggerShared {
    fn drop(&mut self) {
        let (stdout, stderr) = {
            let state = self
                .state
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (state.stdout.clone(), state.stderr.clone())
        };
        let _ = stdout.finish();
        let _ = stderr.finish();
    }
}

/// Shared qpdf-compatible output router.
#[derive(Clone)]
pub struct QPDFLogger {
    shared: Arc<LoggerShared>,
}

struct ErrorCaptureRestore {
    shared: Arc<LoggerShared>,
    owner: ThreadId,
    previous: Option<PipelineHandle>,
}

impl Drop for ErrorCaptureRestore {
    fn drop(&mut self) {
        let replaced = {
            let mut state = self.shared.lock();
            match self.previous.take() {
                Some(previous) => state.error_capture.insert(self.owner, previous),
                None => state.error_capture.remove(&self.owner),
            }
        };
        drop(replaced);
    }
}

impl QPDFLogger {
    pub fn create() -> Self {
        Self::create_with_line_buffering(true)
    }

    fn create_with_line_buffering(line_buffering: bool) -> Self {
        let stdout_text_mode = Arc::new(AtomicBool::new(cfg!(windows)));
        let real_stdout = if line_buffering {
            PipelineHandle::new(PlOStream::new(
                "standard output",
                standard_output_writer(std::io::stdout(), Arc::clone(&stdout_text_mode)),
            ))
        } else {
            PipelineHandle::new(PlOStream::new(
                "standard output",
                TextModeWriter::new(std::io::stdout(), Arc::clone(&stdout_text_mode)),
            ))
        };
        let stdout_used = Arc::new(AtomicBool::new(false));
        let stdout = PipelineHandle::new(PlTrack {
            next: real_stdout,
            used: Arc::clone(&stdout_used),
        });
        let stderr = PipelineHandle::new(PlOStream::new(
            "standard error",
            TextModeWriter::new(std::io::stderr(), Arc::new(AtomicBool::new(cfg!(windows)))),
        ));
        let discard = PipelineHandle::new(Discard);
        let state = LoggerState {
            discard,
            stdout: stdout.clone(),
            stderr: stderr.clone(),
            info: stdout,
            warn: None,
            error: stderr,
            error_capture: HashMap::new(),
            save: None,
            stdout_used,
            stdout_text_mode,
        };
        Self {
            shared: Arc::new(LoggerShared {
                state: Mutex::new(state),
            }),
        }
    }

    pub fn default_logger() -> Self {
        static DEFAULT_LOGGER: OnceLock<QPDFLogger> = OnceLock::new();
        DEFAULT_LOGGER
            .get_or_init(|| Self::create_with_line_buffering(false))
            .clone()
    }

    pub fn info(&self, data: impl AsRef<[u8]>) -> Result<()> {
        self.get_info()?.write(data.as_ref()).map_err(Error::from)
    }

    pub fn warn(&self, data: impl AsRef<[u8]>) -> Result<()> {
        self.get_warn()?.write(data.as_ref()).map_err(Error::from)
    }

    pub fn error(&self, data: impl AsRef<[u8]>) -> Result<()> {
        self.get_error()?.write(data.as_ref()).map_err(Error::from)
    }

    /// Flush the process-owned standard output and error logger sinks.
    ///
    /// The CLI keeps its logger in a process-wide `OnceLock`, so the final
    /// partial line cannot rely on `QPDFLogger::drop`. qpdf's C stdio flushes
    /// that line during process termination; explicit command completion uses
    /// this equivalent boundary.
    pub fn flush(&self) -> Result<()> {
        self.standard_output().finish().map_err(Error::from)?;
        self.standard_error().finish().map_err(Error::from)
    }

    pub fn get_info(&self) -> Result<PipelineHandle> {
        Ok(self.shared.lock().info.clone())
    }

    pub fn get_warn(&self) -> Result<PipelineHandle> {
        let state = self.shared.lock();
        Ok(state.warn.clone().unwrap_or_else(|| state.error.clone()))
    }

    pub fn get_error(&self) -> Result<PipelineHandle> {
        let state = self.shared.lock();
        let current = std::thread::current().id();
        // qpdf-deviation-start: `QPDFLogger::getError` returns the one error
        // pipeline unconditionally; the owner-thread overlay below exists only
        // so a qtest capture cannot swallow a concurrent thread's warnings
        Ok(state
            .error_capture
            .get(&current)
            .cloned()
            .unwrap_or_else(|| state.error.clone()))
        // qpdf-deviation-end
    }

    /// Run `body` with an error pipeline visible only to the calling thread.
    ///
    /// qtest's contextless object accessors reproduce qpdf warnings through the
    /// process-global default logger. A test capture must not redirect warnings
    /// emitted concurrently by another test, so this scope overlays the error
    /// sink for its owner thread while leaving the normal process sink visible
    /// to every other thread.
    pub fn with_error_capture<T>(&self, pipeline: PipelineHandle, body: impl FnOnce() -> T) -> T {
        let owner = std::thread::current().id();
        let previous = self.shared.lock().error_capture.insert(owner, pipeline);
        let restore = ErrorCaptureRestore {
            shared: Arc::clone(&self.shared),
            owner,
            previous,
        };
        let result = body();
        drop(restore);
        result
    }

    pub fn get_save(&self) -> Result<PipelineHandle> {
        self.get_save_if_set()
            .ok_or_else(|| Error::Internal(NULL_PIPELINE_MESSAGE.to_owned()))
    }

    pub fn get_save_if_set(&self) -> Option<PipelineHandle> {
        self.shared.lock().save.clone()
    }

    pub fn standard_output(&self) -> PipelineHandle {
        self.shared.lock().stdout.clone()
    }

    pub fn standard_error(&self) -> PipelineHandle {
        self.shared.lock().stderr.clone()
    }

    pub fn discard(&self) -> PipelineHandle {
        self.shared.lock().discard.clone()
    }

    pub fn set_info(&self, pipeline: Option<PipelineHandle>) {
        let mut state = self.shared.lock();
        state.info = pipeline.unwrap_or_else(|| {
            if state
                .save
                .as_ref()
                .is_some_and(|save| save.is_same(&state.stdout))
            {
                state.stderr.clone()
            } else {
                state.stdout.clone()
            }
        });
    }

    pub fn set_warn(&self, pipeline: Option<PipelineHandle>) {
        self.shared.lock().warn = pipeline;
    }

    pub fn set_error(&self, pipeline: Option<PipelineHandle>) {
        let mut state = self.shared.lock();
        state.error = pipeline.unwrap_or_else(|| state.stderr.clone());
    }

    pub fn set_save(&self, pipeline: Option<PipelineHandle>, only_if_not_set: bool) -> Result<()> {
        let mut state = self.shared.lock();
        if only_if_not_set && state.save.is_some() {
            return Ok(());
        }
        if same_optional_pipeline(&state.save, &pipeline) {
            return Ok(());
        }
        if pipeline
            .as_ref()
            .is_some_and(|candidate| candidate.is_same(&state.stdout))
        {
            if state.stdout_used.load(Ordering::Relaxed) {
                return Err(Error::Internal(STDOUT_ALREADY_USED_MESSAGE.to_owned()));
            }
            if state.info.is_same(&state.stdout) {
                state.info = state.stderr.clone();
            }
            state.stdout_text_mode.store(false, Ordering::Relaxed);
        }
        state.save = pipeline;
        Ok(())
    }

    pub fn save_to_standard_output(&self, only_if_not_set: bool) -> Result<()> {
        self.set_save(Some(self.standard_output()), only_if_not_set)
    }

    pub fn set_output_streams(
        &self,
        output: Option<PipelineHandle>,
        error: Option<PipelineHandle>,
    ) {
        let mut state = self.shared.lock();
        let output = output.filter(|candidate| !candidate.is_same(&state.stdout));
        let error = error.filter(|candidate| !candidate.is_same(&state.stderr));
        state.info = output.unwrap_or_else(|| {
            if state
                .save
                .as_ref()
                .is_some_and(|save| save.is_same(&state.stdout))
            {
                state.stderr.clone()
            } else {
                state.stdout.clone()
            }
        });
        state.warn = None;
        state.error = error.unwrap_or_else(|| state.stderr.clone());
    }
}

fn same_optional_pipeline(left: &Option<PipelineHandle>, right: &Option<PipelineHandle>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.is_same(right),
        (None, None) => true,
        _ => false,
    }
}

impl fmt::Debug for QPDFLogger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QPDFLogger")
            .field("shared", &Arc::as_ptr(&self.shared))
            .finish()
    }
}

impl PartialEq for QPDFLogger {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }
}

impl Eq for QPDFLogger {}

#[cfg(test)]
mod tests {
    use super::TextModeWriter;
    use crate::{Pipeline, PipelineHandle, PipelineResult};
    use std::io::{self, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread;

    struct RecordingPipeline(Arc<Mutex<Vec<u8>>>);

    impl Pipeline for RecordingPipeline {
        fn identifier(&self) -> &str {
            "logger test capture"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct RecordingWriter {
        writes: Vec<Vec<u8>>,
    }

    impl Write for RecordingWriter {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.writes.push(data.to_vec());
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn standard_output_writer_matches_qpdf_line_buffer_boundary() {
        let text_mode = Arc::new(AtomicBool::new(false));
        let mut writer = super::standard_output_writer(RecordingWriter::default(), text_mode);

        writer.write_all(b"{").unwrap();
        writer.write_all(b"\n  \"key\": ").unwrap();
        writer.write_all(b"1").unwrap();
        writer.write_all(b"\n}").unwrap();
        writer.flush().unwrap();

        let sink = writer.writer.into_inner();
        assert_eq!(
            sink.writes,
            [b"{\n".to_vec(), b"  \"key\": 1\n".to_vec(), b"}".to_vec()]
        );
    }

    #[test]
    fn standard_output_writer_flushes_full_capacity_chunks() {
        let text_mode = Arc::new(AtomicBool::new(false));
        let mut writer = super::standard_output_writer(RecordingWriter::default(), text_mode);
        let data = vec![b'x'; super::STDOUT_LINE_BUFFER_CAPACITY + 1];

        writer.write_all(&data).unwrap();
        writer.flush().unwrap();

        let sink = writer.writer.into_inner();
        assert_eq!(
            sink.writes,
            [vec![b'x'; super::STDOUT_LINE_BUFFER_CAPACITY], vec![b'x'],]
        );
    }

    #[test]
    fn standard_output_writer_does_not_grow_pending_buffer_for_large_write() {
        let text_mode = Arc::new(AtomicBool::new(false));
        let mut writer = super::standard_output_writer(RecordingWriter::default(), text_mode);
        let data = vec![b'x'; super::STDOUT_LINE_BUFFER_CAPACITY * 16 + 1];

        writer.write_all(&data).unwrap();

        assert!(
            writer.writer.buffer.capacity() <= super::STDOUT_LINE_BUFFER_CAPACITY,
            "large writes must not grow the pending line buffer"
        );

        writer.flush().unwrap();
        let sink = writer.writer.into_inner();
        assert_eq!(sink.writes.iter().map(Vec::len).sum::<usize>(), data.len());
    }

    #[test]
    fn text_mode_writer_converts_text_lines_but_preserves_binary_save_data() {
        let text_mode = Arc::new(AtomicBool::new(true));
        let mut output = Vec::new();

        {
            let mut writer = TextModeWriter::new(&mut output, Arc::clone(&text_mode));
            writer.write_all(b"first\nsecond\n").unwrap();
        }
        assert_eq!(output, b"first\r\nsecond\r\n");

        text_mode.store(false, Ordering::Relaxed);
        {
            let mut writer = TextModeWriter::new(&mut output, Arc::clone(&text_mode));
            writer.write_all(b"%PDF-1.7\nraw\r\n").unwrap();
        }
        assert_eq!(output, b"first\r\nsecond\r\n%PDF-1.7\nraw\r\n");
    }

    #[test]
    fn save_to_standard_output_switches_the_logger_to_binary_mode() {
        let logger = super::QPDFLogger::create();
        let text_mode = logger.shared.lock().stdout_text_mode.clone();

        assert_eq!(text_mode.load(Ordering::Relaxed), cfg!(windows));
        logger.save_to_standard_output(false).unwrap();
        assert!(!text_mode.load(Ordering::Relaxed));
    }

    #[test]
    fn error_captures_are_independent_across_threads() {
        let logger = super::QPDFLogger::create();
        let first_bytes = Arc::new(Mutex::new(Vec::new()));
        let second_bytes = Arc::new(Mutex::new(Vec::new()));
        let (first_entered_tx, first_entered_rx) = mpsc::channel();
        let (first_release_tx, first_release_rx) = mpsc::channel();
        let (second_entered_tx, second_entered_rx) = mpsc::channel();
        let (second_release_tx, second_release_rx) = mpsc::channel();

        let first_logger = logger.clone();
        let first_capture = Arc::clone(&first_bytes);
        let first = thread::spawn(move || {
            first_logger.with_error_capture(
                PipelineHandle::new(RecordingPipeline(first_capture)),
                || {
                    first_entered_tx.send(()).unwrap();
                    first_release_rx.recv().unwrap();
                    first_logger.error(b"first\n").unwrap();
                },
            );
        });
        first_entered_rx.recv().unwrap();

        let second_logger = logger.clone();
        let second_capture = Arc::clone(&second_bytes);
        let second = thread::spawn(move || {
            second_logger.with_error_capture(
                PipelineHandle::new(RecordingPipeline(second_capture)),
                || {
                    second_entered_tx.send(()).unwrap();
                    second_release_rx.recv().unwrap();
                    second_logger.error(b"second\n").unwrap();
                },
            );
        });
        second_entered_rx.recv().unwrap();

        first_release_tx.send(()).unwrap();
        first.join().unwrap();
        second_release_tx.send(()).unwrap();
        second.join().unwrap();

        assert_eq!(&*first_bytes.lock().unwrap(), b"first\n");
        assert_eq!(&*second_bytes.lock().unwrap(), b"second\n");
    }

    #[test]
    fn default_logger_does_not_install_the_line_buffered_stdout_adapter() {
        let source = include_str!("logger.rs");
        let start = source
            .find("pub fn default_logger")
            .expect("default logger constructor");
        let body = &source[start
            ..source[start..]
                .find("\n    pub fn info")
                .expect("default logger constructor end")
                + start];

        assert!(
            body.contains("create_with_line_buffering(false)"),
            "the process-global default logger must not retain partial lines in the custom line buffer"
        );
    }
}

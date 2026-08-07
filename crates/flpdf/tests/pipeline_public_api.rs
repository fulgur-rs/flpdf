use flpdf::pipeline::{
    Base64Action, Discard, Pipeline, PipelineError, PipelineHandle, PipelineResult, PlBase64,
    PlConcatenate, PlOStream, PlStdioFile, PlString,
};
use std::cell::Cell;
use std::collections::VecDeque;
use std::io::{self, Cursor, Write};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

struct ExternalSink(Vec<u8>);

impl Pipeline for ExternalSink {
    fn identifier(&self) -> &str {
        "external"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.0.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

#[derive(Default)]
struct RecordingSink {
    bytes: Vec<u8>,
    finish_count: usize,
}

impl Pipeline for RecordingSink {
    fn identifier(&self) -> &str {
        "recording"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.bytes.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        self.finish_count += 1;
        Ok(())
    }
}

#[derive(Default)]
struct RejectingSink {
    bytes: Vec<u8>,
}

impl Pipeline for RejectingSink {
    fn identifier(&self) -> &str {
        "rejecting"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.bytes.extend_from_slice(data);
        Err(PipelineError::runtime("downstream rejected chunk"))
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

#[derive(Default)]
struct AcceptTwoThenError {
    bytes: Vec<u8>,
    write_count: usize,
    flush_count: usize,
}

impl Write for AcceptTwoThenError {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.write_count += 1;
        if self.write_count == 1 {
            let accepted = data.len().min(2);
            self.bytes.extend_from_slice(&data[..accepted]);
            Ok(accepted)
        } else {
            Err(io::Error::other("ostream rejected chunk"))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_count += 1;
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn status(result: PipelineResult<()>) -> String {
    result
        .map(|()| "ok".to_owned())
        .unwrap_or_else(|error| error.message().to_string())
}

#[test]
fn discard_is_a_public_pipeline_with_the_qpdf_identifier() {
    let discard = Discard;
    let pipeline: &dyn Pipeline = &discard;

    assert_eq!(pipeline.identifier(), "discard");
}

#[test]
fn discard_accepts_empty_and_nonempty_writes_across_finish_boundaries() {
    let mut discard = Discard;
    let pipeline: &mut dyn Pipeline = &mut discard;

    pipeline.write(b"").unwrap();
    pipeline.write(b"discarded bytes").unwrap();
    pipeline.finish().unwrap();
    pipeline.finish().unwrap();
    pipeline.write(b"after finish").unwrap();
    pipeline.finish().unwrap();
}

#[test]
fn ostream_can_own_a_writer() {
    let mut stage = PlOStream::new("owned", Cursor::new(Vec::new()));

    stage.write(b"owned bytes").unwrap();
    stage.finish().unwrap();
}

struct SharedSink {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Pipeline for SharedSink {
    fn identifier(&self) -> &str {
        "shared"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.bytes.lock().unwrap().extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

struct LogicSink;

impl Pipeline for LogicSink {
    fn identifier(&self) -> &str {
        "logic"
    }

    fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
        Err(PipelineError::logic("logic detail"))
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

struct PanicOnceSink {
    panicked: bool,
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Pipeline for PanicOnceSink {
    fn identifier(&self) -> &str {
        "panic-once"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        if !self.panicked {
            self.panicked = true;
            panic!("poison the pipeline mutex");
        }
        self.bytes.lock().unwrap().extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

#[test]
fn pipeline_handle_clones_share_writes_and_identity() {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let handle = PipelineHandle::new(SharedSink {
        bytes: Arc::clone(&bytes),
    });
    let clone = handle.clone();
    let distinct = PipelineHandle::new(SharedSink {
        bytes: Arc::clone(&bytes),
    });

    handle.write(b"one").unwrap();
    clone.write(b" two").unwrap();

    assert_eq!(handle.identifier(), "shared");
    assert!(handle.is_same(&clone));
    assert!(!handle.is_same(&distinct));
    assert_eq!(&*bytes.lock().unwrap(), b"one two");
}

#[test]
fn pipeline_handle_preserves_downstream_error_categories() {
    let logic = PipelineHandle::new(LogicSink).write(b"x").unwrap_err();
    let runtime = PipelineHandle::new(RejectingSink::default())
        .write(b"x")
        .unwrap_err();

    assert!(matches!(logic, PipelineError::Logic(_)));
    assert_eq!(logic.message(), "logic detail");
    assert!(matches!(runtime, PipelineError::Runtime(_)));
    assert_eq!(runtime.message(), "downstream rejected chunk");
}

#[test]
fn pipeline_handle_recovers_from_a_poisoned_mutex() {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let handle = PipelineHandle::new(PanicOnceSink {
        panicked: false,
        bytes: Arc::clone(&bytes),
    });

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = handle.write(b"first");
    }));
    assert!(panic.is_err());

    handle.write(b"recovered").unwrap();
    assert_eq!(&*bytes.lock().unwrap(), b"recovered");
}

fn record(
    case_name: &str,
    status: &str,
    bytes: &[u8],
    write_count: usize,
    finish_count: usize,
) -> String {
    format!(
        "{case_name}\t{status}\t{}\t{write_count}\t{finish_count}",
        hex(bytes)
    )
}

fn rust_core_records() -> String {
    let mut records = Vec::new();

    let mut string_null_bytes = Vec::new();
    let string_null_status = {
        let mut stage = PlString::new("string-null", None, &mut string_null_bytes);
        status(stage.write(b"ab"))
    };
    records.push(record(
        "string-null",
        &string_null_status,
        &string_null_bytes,
        1,
        0,
    ));

    let mut string_tee_bytes = Vec::new();
    let mut rejecting = RejectingSink::default();
    let string_tee_status = {
        let mut stage = PlString::new(
            "string-tee-error",
            Some(&mut rejecting),
            &mut string_tee_bytes,
        );
        status(stage.write(b"ab"))
    };
    records.push(record(
        "string-tee-error",
        &string_tee_status,
        &string_tee_bytes,
        1,
        0,
    ));

    let mut concatenate_sink = RecordingSink::default();
    let concatenate_status = {
        let mut stage = PlConcatenate::new("concatenate-finish", &mut concatenate_sink);
        stage
            .write(b"one")
            .and_then(|()| stage.finish())
            .and_then(|()| stage.write(b"two"))
            .and_then(|()| stage.manual_finish())
    };
    records.push(record(
        "concatenate-finish",
        &status(concatenate_status),
        &concatenate_sink.bytes,
        2,
        concatenate_sink.finish_count,
    ));

    let mut encode_sink = RecordingSink::default();
    let encode_status = {
        let mut stage = PlBase64::new("base64", &mut encode_sink, Base64Action::Encode);
        stage
            .write(b"\x00")
            .and_then(|()| stage.write(b"\xff\x10"))
            .and_then(|()| stage.write(b"\x20"))
            .and_then(|()| stage.finish())
    };
    records.push(record(
        "base64-encode-split",
        &status(encode_status),
        &encode_sink.bytes,
        3,
        encode_sink.finish_count,
    ));

    let mut decode_sink = RecordingSink::default();
    let decode_status = {
        let mut stage = PlBase64::new("base64", &mut decode_sink, Base64Action::Decode);
        stage.write(b"-_8=").and_then(|()| stage.finish())
    };
    records.push(record(
        "base64-decode-alias",
        &status(decode_status),
        &decode_sink.bytes,
        1,
        decode_sink.finish_count,
    ));

    let mut padded_sink = RecordingSink::default();
    let padded_status = {
        let mut stage = PlBase64::new("base64", &mut padded_sink, Base64Action::Decode);
        stage.write(b"TQ==AAAA")
    };
    records.push(record(
        "base64-data-after-pad",
        &status(padded_status),
        &padded_sink.bytes,
        1,
        padded_sink.finish_count,
    ));

    let mut writer = AcceptTwoThenError::default();
    let ostream_status = {
        let mut stage = PlOStream::new("ostream-sticky", &mut writer);
        stage.write(b"abcd").and_then(|()| stage.finish())
    };
    records.push(record(
        "ostream-sticky",
        &status(ostream_status),
        &writer.bytes,
        1,
        writer.flush_count,
    ));

    records.join("\n") + "\n"
}

fn rust_stdio_records() -> String {
    const EBADF_ERRNO: i32 = 9;
    const ENOSPC_ERRNO: i32 = 28;

    enum StdioWriteStep {
        Accept(usize),
        Interrupted,
        Zero,
        Error(io::Error),
    }

    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    enum StdioPhase {
        #[default]
        None,
        Write,
        Finish,
    }

    struct StdioSink {
        bytes: Vec<u8>,
        steps: VecDeque<StdioWriteStep>,
        phase: Rc<Cell<StdioPhase>>,
        write_lengths: Vec<usize>,
        finish_lengths: Vec<usize>,
    }

    impl StdioSink {
        fn new(phase: Rc<Cell<StdioPhase>>) -> Self {
            Self {
                bytes: Vec::new(),
                steps: VecDeque::new(),
                phase,
                write_lengths: Vec::new(),
                finish_lengths: Vec::new(),
            }
        }

        fn with_steps(
            phase: Rc<Cell<StdioPhase>>,
            steps: impl IntoIterator<Item = StdioWriteStep>,
        ) -> Self {
            Self {
                steps: steps.into_iter().collect(),
                ..Self::new(phase)
            }
        }
    }

    impl Write for StdioSink {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            match self.phase.get() {
                StdioPhase::Write => self.write_lengths.push(data.len()),
                StdioPhase::Finish => self.finish_lengths.push(data.len()),
                StdioPhase::None => panic!("stdio sink write outside a pipeline operation"),
            }
            match self.steps.pop_front() {
                Some(StdioWriteStep::Accept(limit)) => {
                    let accepted = limit.min(data.len());
                    self.bytes.extend_from_slice(&data[..accepted]);
                    Ok(accepted)
                }
                Some(StdioWriteStep::Interrupted) => Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Interrupted system call",
                )),
                Some(StdioWriteStep::Zero) => Ok(0),
                Some(StdioWriteStep::Error(error)) => Err(error),
                None => {
                    self.bytes.extend_from_slice(data);
                    Ok(data.len())
                }
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct OperationTrace {
        write_count: usize,
        finish_count: usize,
    }

    fn pipeline_write(
        stage: &mut PlStdioFile<'_>,
        phase: &Cell<StdioPhase>,
        operations: &mut OperationTrace,
        data: &[u8],
    ) -> PipelineResult<()> {
        operations.write_count += 1;
        phase.set(StdioPhase::Write);
        let result = stage.write(data);
        phase.set(StdioPhase::None);
        result
    }

    fn pipeline_finish(
        stage: &mut PlStdioFile<'_>,
        phase: &Cell<StdioPhase>,
        operations: &mut OperationTrace,
    ) -> PipelineResult<()> {
        operations.finish_count += 1;
        phase.set(StdioPhase::Finish);
        let result = stage.finish();
        phase.set(StdioPhase::None);
        result
    }

    fn verify_stdio_lifecycle(
        sink: &StdioSink,
        buffered_bytes: Result<Vec<u8>, io::WriterPanicked>,
        expected_buffered_len: usize,
        write_lengths: &[usize],
        finish_lengths: &[usize],
        remaining_accept_limits: &[usize],
    ) {
        assert_eq!(
            buffered_bytes
                .expect("stdio buffer writer must not panic")
                .len(),
            expected_buffered_len
        );
        assert_eq!(sink.steps.len(), remaining_accept_limits.len());
        for (step, expected_limit) in sink.steps.iter().zip(remaining_accept_limits) {
            match step {
                StdioWriteStep::Accept(limit) => assert_eq!(limit, expected_limit),
                _ => panic!("unexpected remaining stdio write step"),
            }
        }
        assert_eq!(sink.phase.get(), StdioPhase::None);
        assert_eq!(sink.write_lengths, write_lengths);
        assert_eq!(sink.finish_lengths, finish_lengths);
    }

    fn no_space_message() -> io::Error {
        io::Error::other("No space left on device")
    }

    fn no_space_os_error() -> io::Error {
        io::Error::from_raw_os_error(ENOSPC_ERRNO)
    }

    fn patterned_bytes(count: usize) -> Vec<u8> {
        (0..count).map(|index| (index % 251) as u8).collect()
    }

    let mut records = Vec::new();

    let phase = Rc::new(Cell::new(StdioPhase::None));
    let sink = StdioSink::with_steps(
        Rc::clone(&phase),
        [StdioWriteStep::Error(no_space_os_error())],
    );
    let mut buffered = io::BufWriter::with_capacity(4096, sink);
    let mut operations = OperationTrace::default();
    let case_status = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        let result = pipeline_write(&mut stage, &phase, &mut operations, &vec![b'x'; 4095]);
        status(result.and_then(|()| pipeline_finish(&mut stage, &phase, &mut operations)))
    };
    let (sink, buffered_bytes) = buffered.into_parts();
    verify_stdio_lifecycle(&sink, buffered_bytes, 4095, &[], &[4095], &[]);
    records.push(record(
        "stdio-4095-enospc",
        &case_status,
        &sink.bytes,
        operations.write_count,
        operations.finish_count,
    ));

    let phase = Rc::new(Cell::new(StdioPhase::None));
    let sink = StdioSink::with_steps(
        Rc::clone(&phase),
        [StdioWriteStep::Error(no_space_message())],
    );
    let mut buffered = io::BufWriter::with_capacity(4096, sink);
    let mut operations = OperationTrace::default();
    let case_status = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        status(pipeline_write(
            &mut stage,
            &phase,
            &mut operations,
            &vec![b'x'; 4096],
        ))
    };
    let (sink, buffered_bytes) = buffered.into_parts();
    verify_stdio_lifecycle(&sink, buffered_bytes, 0, &[4096], &[], &[]);
    records.push(record(
        "stdio-4096-enospc",
        &case_status,
        &sink.bytes,
        operations.write_count,
        operations.finish_count,
    ));

    let payload = patterned_bytes(4097);
    let phase = Rc::new(Cell::new(StdioPhase::None));
    let mut buffered = io::BufWriter::with_capacity(4096, StdioSink::new(Rc::clone(&phase)));
    let mut operations = OperationTrace::default();
    let case_status = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        let result = pipeline_write(&mut stage, &phase, &mut operations, &payload);
        status(result.and_then(|()| pipeline_finish(&mut stage, &phase, &mut operations)))
    };
    let (sink, buffered_bytes) = buffered.into_parts();
    verify_stdio_lifecycle(&sink, buffered_bytes, 0, &[4097], &[], &[]);
    records.push(record(
        "stdio-4097-success",
        &case_status,
        &sink.bytes,
        operations.write_count,
        operations.finish_count,
    ));

    let payload = patterned_bytes(4096);
    let phase = Rc::new(Cell::new(StdioPhase::None));
    let sink = StdioSink::with_steps(
        Rc::clone(&phase),
        [
            StdioWriteStep::Accept(1024),
            StdioWriteStep::Error(no_space_os_error()),
        ],
    );
    let mut buffered = io::BufWriter::with_capacity(4096, sink);
    let mut operations = OperationTrace::default();
    let case_status = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        let result = pipeline_write(&mut stage, &phase, &mut operations, &payload);
        status(result.and_then(|()| pipeline_finish(&mut stage, &phase, &mut operations)))
    };
    let (sink, buffered_bytes) = buffered.into_parts();
    verify_stdio_lifecycle(&sink, buffered_bytes, 3072, &[4096], &[3072], &[]);
    records.push(record(
        "stdio-partial-write",
        &case_status,
        &sink.bytes,
        operations.write_count,
        operations.finish_count,
    ));

    let payload = patterned_bytes(4096);
    let phase = Rc::new(Cell::new(StdioPhase::None));
    let sink = StdioSink::with_steps(
        Rc::clone(&phase),
        [StdioWriteStep::Interrupted, StdioWriteStep::Accept(4096)],
    );
    let mut buffered = io::BufWriter::with_capacity(4096, sink);
    let mut operations = OperationTrace::default();
    let case_status = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        status(pipeline_write(
            &mut stage,
            &phase,
            &mut operations,
            &payload,
        ))
    };
    assert_eq!(
        case_status,
        "stdio: Pl_StdioFile::write: Interrupted system call"
    );
    let (sink, buffered_bytes) = buffered.into_parts();
    verify_stdio_lifecycle(&sink, buffered_bytes, 0, &[4096], &[], &[4096]);
    records.push(record(
        "stdio-interrupted-write",
        &case_status,
        &sink.bytes,
        operations.write_count,
        operations.finish_count,
    ));

    let payload = patterned_bytes(4096);
    let phase = Rc::new(Cell::new(StdioPhase::None));
    let sink = StdioSink::with_steps(Rc::clone(&phase), [StdioWriteStep::Zero]);
    let mut buffered = io::BufWriter::with_capacity(4096, sink);
    let mut operations = OperationTrace::default();
    let error = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        pipeline_write(&mut stage, &phase, &mut operations, &payload).unwrap_err()
    };
    assert!(matches!(error, PipelineError::Runtime(_)));
    assert_eq!(
        error.to_string(),
        "stdio: Pl_StdioFile::write: failed to write buffered data"
    );
    let (sink, buffered_bytes) = buffered.into_parts();
    verify_stdio_lifecycle(&sink, buffered_bytes, 0, &[4096], &[], &[]);
    records.push(record(
        "stdio-zero-progress",
        "runtime",
        &sink.bytes,
        operations.write_count,
        operations.finish_count,
    ));

    let phase = Rc::new(Cell::new(StdioPhase::None));
    let sink = StdioSink::with_steps(
        Rc::clone(&phase),
        [StdioWriteStep::Error(io::Error::from_raw_os_error(
            EBADF_ERRNO,
        ))],
    );
    let mut buffered = io::BufWriter::with_capacity(4096, sink);
    let mut operations = OperationTrace::default();
    let case_status = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        let result = pipeline_write(&mut stage, &phase, &mut operations, b"abc");
        status(result.and_then(|()| pipeline_finish(&mut stage, &phase, &mut operations)))
    };
    let (sink, buffered_bytes) = buffered.into_parts();
    verify_stdio_lifecycle(&sink, buffered_bytes, 3, &[], &[3], &[]);
    records.push(record(
        "stdio-finish-ebadf",
        &case_status,
        &sink.bytes,
        operations.write_count,
        operations.finish_count,
    ));

    let phase = Rc::new(Cell::new(StdioPhase::None));
    let sink = StdioSink::with_steps(
        Rc::clone(&phase),
        [StdioWriteStep::Error(no_space_os_error())],
    );
    let mut buffered = io::BufWriter::with_capacity(4096, sink);
    let mut operations = OperationTrace::default();
    let case_status = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        let result = pipeline_write(&mut stage, &phase, &mut operations, b"abc");
        status(result.and_then(|()| pipeline_finish(&mut stage, &phase, &mut operations)))
    };
    let (sink, buffered_bytes) = buffered.into_parts();
    verify_stdio_lifecycle(&sink, buffered_bytes, 3, &[], &[3], &[]);
    records.push(record(
        "stdio-finish-enospc",
        &case_status,
        &sink.bytes,
        operations.write_count,
        operations.finish_count,
    ));

    let phase = Rc::new(Cell::new(StdioPhase::None));
    let mut buffered = io::BufWriter::with_capacity(4096, StdioSink::new(Rc::clone(&phase)));
    let mut operations = OperationTrace::default();
    let case_status = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        let result = pipeline_write(&mut stage, &phase, &mut operations, b"before");
        let result = result.and_then(|()| pipeline_finish(&mut stage, &phase, &mut operations));
        let result =
            result.and_then(|()| pipeline_write(&mut stage, &phase, &mut operations, b"after"));
        status(result.and_then(|()| pipeline_finish(&mut stage, &phase, &mut operations)))
    };
    let (sink, buffered_bytes) = buffered.into_parts();
    verify_stdio_lifecycle(&sink, buffered_bytes, 0, &[], &[6, 5], &[]);
    records.push(record(
        "stdio-repeated-finish",
        &case_status,
        &sink.bytes,
        operations.write_count,
        operations.finish_count,
    ));

    records.join("\n") + "\n"
}

#[test]
fn downstream_crates_can_implement_pipeline_and_construct_public_pipeline_stages() {
    let mut captured = Vec::new();
    let mut sink = ExternalSink(Vec::new());
    {
        let mut stage = PlString::new("capture", Some(&mut sink), &mut captured);
        stage.write(b"payload").unwrap();
        stage.finish().unwrap();
    }
    assert_eq!(captured, b"payload");
    assert_eq!(sink.0, b"payload");

    let mut concatenate = PlConcatenate::new("concatenate", &mut sink);
    assert_eq!(concatenate.identifier(), "concatenate");
    concatenate.finish().unwrap();
    concatenate.manual_finish().unwrap();

    let mut base64 = PlBase64::new("base64", &mut sink, Base64Action::Encode);
    assert_eq!(base64.identifier(), "base64");
    base64.write(b"M").unwrap();
    base64.finish().unwrap();

    assert_eq!(PipelineError::runtime("failure").message(), "failure");
}

#[test]
fn downstream_crates_can_construct_pl_ostream_with_an_external_writer() {
    let mut writer = Cursor::new(Vec::new());
    {
        let mut stage = PlOStream::new("ostream", &mut writer);
        assert_eq!(stage.identifier(), "ostream");
        stage.write(b"payload").unwrap();
        stage.finish().unwrap();
    }
    assert_eq!(writer.into_inner(), b"payload");
}

#[test]
fn downstream_crates_can_construct_pl_stdio_file_with_an_external_writer() {
    let mut writer = Cursor::new(Vec::new());
    {
        let mut stage = PlStdioFile::new("stdio", &mut writer);
        assert_eq!(stage.identifier(), "stdio");
        stage.write(b"payload").unwrap();
        stage.finish().unwrap();
    }
    assert_eq!(writer.into_inner(), b"payload");
}

#[test]
fn checked_qpdf_core_records_match_rust() {
    assert_eq!(
        rust_core_records(),
        include_str!("../../../tests/oracle/qpdf_json_pipeline_core_records.tsv")
    );
}

#[test]
fn checked_qpdf_stdio_records_match_rust() {
    assert_eq!(
        rust_stdio_records(),
        include_str!("../../../tests/oracle/qpdf_json_pipeline_stdio_records.tsv")
    );
}

#[test]
#[ignore = "live pinned qpdf 11.9.0 JSON pipeline oracle"]
fn live_qpdf_core_records_match_rust() {
    let probe = std::env::var("QPDF_JSON_PIPELINE_PROBE").unwrap();
    let output = std::process::Command::new(probe)
        .arg("core")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        rust_core_records()
    );
}

#[test]
#[ignore = "live pinned qpdf 11.9.0 Pl_StdioFile oracle"]
fn live_qpdf_stdio_records_match_rust() {
    let probe = std::env::var("QPDF_JSON_PIPELINE_PROBE").unwrap();
    let output = std::process::Command::new(probe)
        .arg("stdio")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        rust_stdio_records()
    );
}

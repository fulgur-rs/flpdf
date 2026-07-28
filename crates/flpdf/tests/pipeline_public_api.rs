use flpdf::pipeline::{
    Base64Action, Pipeline, PipelineError, PipelineResult, PlBase64, PlConcatenate, PlOStream,
    PlStdioFile, PlString,
};
use std::collections::VecDeque;
use std::io::{self, Cursor, Write};

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

    #[derive(Default)]
    struct StdioSink {
        bytes: Vec<u8>,
        steps: VecDeque<StdioWriteStep>,
    }

    impl StdioSink {
        fn with_steps(steps: impl IntoIterator<Item = StdioWriteStep>) -> Self {
            Self {
                steps: steps.into_iter().collect(),
                ..Self::default()
            }
        }
    }

    impl Write for StdioSink {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            match self.steps.pop_front() {
                Some(StdioWriteStep::Accept(limit)) => {
                    let accepted = limit.min(data.len());
                    self.bytes.extend_from_slice(&data[..accepted]);
                    Ok(accepted)
                }
                Some(StdioWriteStep::Interrupted) => Err(io::ErrorKind::Interrupted.into()),
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

    let sink = StdioSink::with_steps([StdioWriteStep::Error(no_space_os_error())]);
    let mut buffered = io::BufWriter::with_capacity(4096, sink);
    let case_status = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        status(stage.write(&vec![b'x'; 4095]).and_then(|()| stage.finish()))
    };
    let (sink, _) = buffered.into_parts();
    records.push(record("stdio-4095-enospc", &case_status, &sink.bytes, 1, 1));

    let sink = StdioSink::with_steps([StdioWriteStep::Error(no_space_message())]);
    let mut buffered = io::BufWriter::with_capacity(4096, sink);
    let case_status = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        status(stage.write(&vec![b'x'; 4096]))
    };
    let (sink, _) = buffered.into_parts();
    records.push(record("stdio-4096-enospc", &case_status, &sink.bytes, 1, 0));

    let payload = patterned_bytes(4097);
    let mut buffered = io::BufWriter::with_capacity(4096, StdioSink::default());
    let case_status = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        status(stage.write(&payload).and_then(|()| stage.finish()))
    };
    let (sink, _) = buffered.into_parts();
    records.push(record(
        "stdio-4097-success",
        &case_status,
        &sink.bytes,
        1,
        1,
    ));

    let payload = patterned_bytes(4096);
    let sink = StdioSink::with_steps([
        StdioWriteStep::Accept(1024),
        StdioWriteStep::Error(no_space_os_error()),
    ]);
    let mut buffered = io::BufWriter::with_capacity(4096, sink);
    let case_status = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        status(stage.write(&payload).and_then(|()| stage.finish()))
    };
    let (sink, _) = buffered.into_parts();
    records.push(record(
        "stdio-partial-write",
        &case_status,
        &sink.bytes,
        1,
        1,
    ));

    let sink = StdioSink::with_steps([StdioWriteStep::Interrupted, StdioWriteStep::Zero]);
    let mut buffered = io::BufWriter::with_capacity(4096, sink);
    let case_status = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        status(stage.write(b"abc").and_then(|()| stage.finish()))
    };
    let (sink, _) = buffered.into_parts();
    records.push(record(
        "stdio-interrupted-write",
        &case_status,
        &sink.bytes,
        1,
        1,
    ));

    let sink = StdioSink::with_steps([StdioWriteStep::Zero]);
    let mut buffered = io::BufWriter::with_capacity(4096, sink);
    let case_status = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        status(stage.write(b"abc").and_then(|()| stage.finish()))
    };
    let (sink, _) = buffered.into_parts();
    records.push(record(
        "stdio-zero-progress",
        &case_status,
        &sink.bytes,
        1,
        1,
    ));

    let sink = StdioSink::with_steps([StdioWriteStep::Error(io::Error::from_raw_os_error(
        EBADF_ERRNO,
    ))]);
    let mut buffered = io::BufWriter::with_capacity(4096, sink);
    let case_status = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        status(stage.write(b"abc").and_then(|()| stage.finish()))
    };
    let (sink, _) = buffered.into_parts();
    records.push(record(
        "stdio-finish-ebadf",
        &case_status,
        &sink.bytes,
        1,
        1,
    ));

    let sink = StdioSink::with_steps([StdioWriteStep::Error(no_space_os_error())]);
    let mut buffered = io::BufWriter::with_capacity(4096, sink);
    let case_status = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        status(stage.write(b"abc").and_then(|()| stage.finish()))
    };
    let (sink, _) = buffered.into_parts();
    records.push(record(
        "stdio-finish-enospc",
        &case_status,
        &sink.bytes,
        1,
        1,
    ));

    let mut buffered = io::BufWriter::with_capacity(4096, StdioSink::default());
    let case_status = {
        let mut stage = PlStdioFile::new("stdio", &mut buffered);
        status(
            stage
                .write(b"before")
                .and_then(|()| stage.finish())
                .and_then(|()| stage.write(b"after"))
                .and_then(|()| stage.finish()),
        )
    };
    let (sink, _) = buffered.into_parts();
    records.push(record(
        "stdio-repeated-finish",
        &case_status,
        &sink.bytes,
        2,
        2,
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

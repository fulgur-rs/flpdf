//! qpdf correspondence: live differential instrumentation for Pl_ASCII85Decoder.cc, Pl_ASCIIHexDecoder.cc, and Pl_RunLength.cc.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use super::ascii85_decoder::Ascii85Decoder;
use super::ascii_hex::AsciiHexDecoder;
use super::run_length::{RunLength, RunLengthAction};
use super::test_support::{RecordingSink, TraceCall};
use super::{Pipeline, PipelineError, PipelineResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Codec {
    Ascii85,
    AsciiHex,
    RunLengthDecode,
    RunLengthEncode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Operation {
    Write(Vec<u8>),
    Finish,
}

struct OracleCase {
    name: &'static str,
    codec: Codec,
    fail_writes: Vec<usize>,
    fail_finishes: Vec<usize>,
    operations: Vec<Operation>,
}

impl Codec {
    fn as_probe_arg(self) -> &'static str {
        match self {
            Self::Ascii85 => "ascii85",
            Self::AsciiHex => "asciihex",
            Self::RunLengthDecode => "runlength-decode",
            Self::RunLengthEncode => "runlength-encode",
        }
    }
}

impl Operation {
    fn as_probe_arg(&self) -> String {
        match self {
            Self::Write(data) => format!("w:{}", hex(data)),
            Self::Finish => "f".to_string(),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(encoded, "{byte:02x}").unwrap();
    }
    encoded
}

fn csv_or_dash(values: &[usize]) -> String {
    if values.is_empty() {
        "-".to_string()
    } else {
        values
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn oracle_cases() -> Vec<OracleCase> {
    vec![
        OracleCase {
            name: "ascii85-all-whitespace-and-nul",
            codec: Codec::Ascii85,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![
                Operation::Write(b" \x0c\x0b\t\r\n\0".to_vec()),
                Operation::Finish,
            ],
        },
        OracleCase {
            name: "ascii85-split-eod",
            codec: Codec::Ascii85,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![
                Operation::Write(b"9jqo^~".to_vec()),
                Operation::Write(b" \x0c\x0b\t\r\n>ignored".to_vec()),
                Operation::Finish,
            ],
        },
        OracleCase {
            name: "ascii85-bare-tilde-finish",
            codec: Codec::Ascii85,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![Operation::Write(b"9jqo^~".to_vec()), Operation::Finish],
        },
        OracleCase {
            name: "ascii85-one-digit-zero-write",
            codec: Codec::Ascii85,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![Operation::Write(b"!".to_vec()), Operation::Finish],
        },
        OracleCase {
            name: "ascii85-low32-overflow",
            codec: Codec::Ascii85,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![Operation::Write(b"uuuuu".to_vec()), Operation::Finish],
        },
        OracleCase {
            name: "ascii85-flush-failure-reuse",
            codec: Codec::Ascii85,
            fail_writes: vec![1],
            fail_finishes: vec![],
            operations: vec![
                Operation::Write(b"9jqo^".to_vec()),
                Operation::Write(b"9jqo^".to_vec()),
                Operation::Finish,
            ],
        },
        OracleCase {
            name: "asciihex-partial-and-eod",
            codec: Codec::AsciiHex,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![
                Operation::Write(b"4".to_vec()),
                Operation::Write(b">ignored".to_vec()),
                Operation::Finish,
            ],
        },
        OracleCase {
            name: "asciihex-output-before-error",
            codec: Codec::AsciiHex,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![Operation::Write(b"48\0".to_vec()), Operation::Finish],
        },
        OracleCase {
            name: "asciihex-raw-80-error",
            codec: Codec::AsciiHex,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![Operation::Write(vec![0x80]), Operation::Finish],
        },
        OracleCase {
            name: "asciihex-raw-ff-error",
            codec: Codec::AsciiHex,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![Operation::Write(vec![0xff]), Operation::Finish],
        },
        OracleCase {
            name: "asciihex-flush-failure-reuse",
            codec: Codec::AsciiHex,
            fail_writes: vec![1],
            fail_finishes: vec![],
            operations: vec![
                Operation::Write(b"48".to_vec()),
                Operation::Write(b"48".to_vec()),
                Operation::Finish,
            ],
        },
        OracleCase {
            name: "runlength-decode-eod-continues",
            codec: Codec::RunLengthDecode,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![Operation::Write(vec![0x80, 0x00, b'Z']), Operation::Finish],
        },
        OracleCase {
            name: "runlength-decode-truncated-literal-reuse",
            codec: Codec::RunLengthDecode,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![
                Operation::Write(vec![0x02, b'A']),
                Operation::Finish,
                Operation::Write(b"BC".to_vec()),
                Operation::Finish,
            ],
        },
        OracleCase {
            name: "runlength-decode-truncated-repeat-reuse",
            codec: Codec::RunLengthDecode,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![
                Operation::Write(vec![0xfd]),
                Operation::Finish,
                Operation::Write(b"Z".to_vec()),
                Operation::Finish,
            ],
        },
        OracleCase {
            name: "runlength-decode-repeat-failure",
            codec: Codec::RunLengthDecode,
            fail_writes: vec![2],
            fail_finishes: vec![],
            operations: vec![
                Operation::Write(vec![0xfd]),
                Operation::Write(b"Z".to_vec()),
                Operation::Write(b"Y".to_vec()),
                Operation::Finish,
            ],
        },
        OracleCase {
            name: "runlength-encode-two-byte-run",
            codec: Codec::RunLengthEncode,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![Operation::Write(b"AA".to_vec()), Operation::Finish],
        },
        OracleCase {
            name: "runlength-encode-128-boundaries",
            codec: Codec::RunLengthEncode,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![
                Operation::Write((0_u8..128).collect()),
                Operation::Finish,
                Operation::Write(vec![b'R'; 128]),
                Operation::Finish,
            ],
        },
        OracleCase {
            name: "runlength-encode-payload-failure-retry",
            codec: Codec::RunLengthEncode,
            fail_writes: vec![2],
            fail_finishes: vec![],
            operations: vec![
                Operation::Write(b"AA".to_vec()),
                Operation::Finish,
                Operation::Finish,
            ],
        },
        OracleCase {
            name: "runlength-repeated-finish",
            codec: Codec::RunLengthEncode,
            fail_writes: vec![],
            fail_finishes: vec![1],
            operations: vec![Operation::Finish, Operation::Finish],
        },
    ]
}

fn operation_record(index: usize, result: PipelineResult<()>) -> String {
    match result {
        Ok(()) => format!("op\t{index}\tok\t\n"),
        Err(PipelineError::Logic(message)) => {
            format!("op\t{index}\tlogic\t{}\n", hex(message.as_bytes()))
        }
        Err(PipelineError::Runtime(message)) => {
            format!("op\t{index}\truntime\t{}\n", hex(message.as_bytes()))
        }
    }
}

fn execute_operations(stage: &mut dyn Pipeline, operations: &[Operation]) -> String {
    let mut records = String::new();
    for (index, operation) in operations.iter().enumerate() {
        let result = match operation {
            Operation::Write(data) => stage.write(data),
            Operation::Finish => stage.finish(),
        };
        records.push_str(&operation_record(index, result));
    }
    records
}

fn flpdf_trace(case: &OracleCase) -> String {
    let mut sink = RecordingSink::new(&case.fail_writes, &case.fail_finishes);
    let trace = sink.trace();
    let mut records = match case.codec {
        Codec::Ascii85 => {
            let mut stage = Ascii85Decoder::new("oracle codec", &mut sink);
            execute_operations(&mut stage, &case.operations)
        }
        Codec::AsciiHex => {
            let mut stage = AsciiHexDecoder::new("oracle codec", &mut sink);
            execute_operations(&mut stage, &case.operations)
        }
        Codec::RunLengthDecode => {
            let mut stage = RunLength::new("oracle codec", &mut sink, RunLengthAction::Decode);
            execute_operations(&mut stage, &case.operations)
        }
        Codec::RunLengthEncode => {
            let mut stage = RunLength::new("oracle codec", &mut sink, RunLengthAction::Encode);
            execute_operations(&mut stage, &case.operations)
        }
    };

    let trace = trace.borrow();
    for call in &trace.calls {
        match call {
            TraceCall::Write { data, failed } => {
                writeln!(
                    records,
                    "call\twrite\t{}\t{}\t{}",
                    usize::from(*failed),
                    data.len(),
                    hex(data)
                )
                .unwrap();
            }
            TraceCall::Finish { failed } => {
                writeln!(records, "call\tfinish\t{}\t0\t", usize::from(*failed)).unwrap();
            }
        }
    }
    writeln!(records, "output\t{}", hex(&trace.output)).unwrap();
    records
}

fn is_lower_hex(value: &str) -> bool {
    value.len().is_multiple_of(2)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_canonical_usize(value: &str) -> Option<usize> {
    let parsed = value.parse::<usize>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

fn split_fields<const N: usize>(line: &str) -> Option<[&str; N]> {
    let fields = line.split('\t').collect::<Vec<_>>();
    fields.try_into().ok()
}

fn validate_trace_protocol(trace: &str, operation_count: usize) -> Result<(), String> {
    if !trace.is_ascii() {
        return Err("stdout is not ASCII".to_string());
    }
    let body = trace
        .strip_suffix('\n')
        .ok_or_else(|| "trace has no final newline".to_string())?;
    let lines = body.split('\n').collect::<Vec<_>>();
    if lines.len() <= operation_count {
        return Err("trace is missing its output record".to_string());
    }

    for (index, line) in lines.iter().take(operation_count).enumerate() {
        let [record, observed_index, category, detail] =
            split_fields::<4>(line).ok_or_else(|| format!("malformed operation record {index}"))?;
        if record != "op"
            || parse_canonical_usize(observed_index) != Some(index)
            || !matches!(category, "ok" | "logic" | "runtime")
            || (category == "ok" && !detail.is_empty())
            || (category != "ok" && !is_lower_hex(detail))
        {
            return Err(format!("malformed operation record {index}"));
        }
    }

    let mut successful_output = String::new();
    for line in &lines[operation_count..lines.len() - 1] {
        let [record, kind, failed, length, data] =
            split_fields::<5>(line).ok_or_else(|| "malformed call record".to_string())?;
        let length = parse_canonical_usize(length)
            .ok_or_else(|| "malformed call record length".to_string())?;
        if record != "call"
            || !matches!(failed, "0" | "1")
            || !is_lower_hex(data)
            || data.len() != length.saturating_mul(2)
        {
            return Err("malformed call record".to_string());
        }
        match kind {
            "write" => {
                if failed == "0" {
                    successful_output.push_str(data);
                }
            }
            "finish" if length == 0 && data.is_empty() => {}
            _ => return Err("malformed call record kind".to_string()),
        }
    }

    let output = lines
        .last()
        .and_then(|line| line.strip_prefix("output\t"))
        .ok_or_else(|| "malformed output record".to_string())?;
    if !is_lower_hex(output) || output != successful_output {
        return Err("output record does not match successful writes".to_string());
    }
    Ok(())
}

fn run_qpdf_probe_command(mut command: Command, case: &OracleCase) -> String {
    let output = command
        .arg(case.codec.as_probe_arg())
        .arg(csv_or_dash(&case.fail_writes))
        .arg(csv_or_dash(&case.fail_finishes))
        .args(case.operations.iter().map(Operation::as_probe_arg))
        .output()
        .unwrap_or_else(|error| {
            panic!("execute qpdf stream-codec probe for {}: {error}", case.name)
        });
    assert!(
        output.status.success(),
        "qpdf stream-codec probe failed for {} with status {}: {}",
        case.name,
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let trace = String::from_utf8(output.stdout).unwrap_or_else(|error| {
        panic!(
            "qpdf stream-codec probe produced non-UTF-8 ASCII protocol for {}: {error}",
            case.name
        )
    });
    validate_trace_protocol(&trace, case.operations.len()).unwrap_or_else(|error| {
        panic!(
            "qpdf stream-codec probe protocol corruption for {}: {error}",
            case.name
        )
    });
    trace
}

fn run_qpdf_probe(probe: &Path, case: &OracleCase) -> String {
    run_qpdf_probe_command(Command::new(probe), case)
}

fn assert_qpdf_oracle_matches_with(mut qpdf_trace: impl FnMut(&OracleCase) -> String) {
    for case in oracle_cases() {
        assert_eq!(flpdf_trace(&case), qpdf_trace(&case), "case {}", case.name);
    }
}

fn assert_qpdf_oracle_matches(probe: &Path) {
    assert_qpdf_oracle_matches_with(|case| run_qpdf_probe(probe, case));
}

#[test]
fn oracle_cases_generate_nonempty_stable_flpdf_traces() {
    let cases = oracle_cases();
    assert_eq!(
        cases.iter().map(|case| case.name).collect::<Vec<_>>(),
        [
            "ascii85-all-whitespace-and-nul",
            "ascii85-split-eod",
            "ascii85-bare-tilde-finish",
            "ascii85-one-digit-zero-write",
            "ascii85-low32-overflow",
            "ascii85-flush-failure-reuse",
            "asciihex-partial-and-eod",
            "asciihex-output-before-error",
            "asciihex-raw-80-error",
            "asciihex-raw-ff-error",
            "asciihex-flush-failure-reuse",
            "runlength-decode-eod-continues",
            "runlength-decode-truncated-literal-reuse",
            "runlength-decode-truncated-repeat-reuse",
            "runlength-decode-repeat-failure",
            "runlength-encode-two-byte-run",
            "runlength-encode-128-boundaries",
            "runlength-encode-payload-failure-retry",
            "runlength-repeated-finish",
        ]
    );

    for case in cases {
        assert!(
            !case.operations.is_empty(),
            "case {} has no operations",
            case.name
        );
        let trace = flpdf_trace(&case);
        assert!(
            trace.starts_with("op\t0\t"),
            "case {} trace: {trace:?}",
            case.name
        );
        assert!(
            trace
                .lines()
                .last()
                .is_some_and(|line| line.starts_with("output\t")),
            "case {} trace: {trace:?}",
            case.name
        );
    }
}

#[test]
fn operation_records_bind_exact_pipeline_error_categories() {
    let logic = operation_record(3, Err(PipelineError::logic("same message")));
    let runtime = operation_record(3, Err(PipelineError::runtime("same message")));

    assert_eq!(logic, "op\t3\tlogic\t73616d65206d657373616765\n");
    assert_eq!(runtime, "op\t3\truntime\t73616d65206d657373616765\n");
    assert_ne!(logic, runtime);
}

#[test]
fn probe_argument_encodings_are_stable_for_every_variant() {
    assert_eq!(Codec::Ascii85.as_probe_arg(), "ascii85");
    assert_eq!(Codec::AsciiHex.as_probe_arg(), "asciihex");
    assert_eq!(Codec::RunLengthDecode.as_probe_arg(), "runlength-decode");
    assert_eq!(Codec::RunLengthEncode.as_probe_arg(), "runlength-encode");
    assert_eq!(csv_or_dash(&[]), "-");
    assert_eq!(csv_or_dash(&[1, 3]), "1,3");
    assert_eq!(Operation::Write(vec![]).as_probe_arg(), "w:");
    assert_eq!(Operation::Finish.as_probe_arg(), "f");
}

#[test]
fn protocol_validator_accepts_complete_traces_and_rejects_corruption_shapes() {
    let complete = concat!(
        "op\t0\truntime\t00\n",
        "call\twrite\t0\t1\tab\n",
        "call\twrite\t1\t1\tcd\n",
        "call\tfinish\t0\t0\t\n",
        "output\tab\n",
    );
    assert_eq!(validate_trace_protocol(complete, 1), Ok(()));

    for (corrupt, operations, expected) in [
        ("output\té\n", 0, "stdout is not ASCII"),
        ("op\t0\tok\t\n", 1, "trace is missing its output record"),
        (
            "op\t0\tunknown\t\noutput\t\n",
            1,
            "malformed operation record 0",
        ),
        (
            "call\twrite\t2\t1\t00\noutput\t\n",
            0,
            "malformed call record",
        ),
        (
            "call\tunknown\t0\t0\t\noutput\t\n",
            0,
            "malformed call record kind",
        ),
        (
            "call\twrite\t0\t1\tab\noutput\tcd\n",
            0,
            "output record does not match successful writes",
        ),
    ] {
        assert_eq!(
            validate_trace_protocol(corrupt, operations).unwrap_err(),
            expected,
            "trace: {corrupt:?}"
        );
    }
}

#[test]
fn qpdf_comparison_checks_every_oracle_case() {
    let mut visited = Vec::new();
    assert_qpdf_oracle_matches_with(|case| {
        visited.push(case.name);
        flpdf_trace(case)
    });

    assert_eq!(visited.len(), oracle_cases().len());
}

#[test]
fn qpdf_comparison_rejects_category_mismatch_with_identical_output() {
    let target = oracle_cases()
        .into_iter()
        .find(|case| case.name == "ascii85-all-whitespace-and-nul")
        .unwrap();
    let local = flpdf_trace(&target);
    let changed_category = local.replacen("\truntime\t", "\tlogic\t", 1);
    assert_ne!(local, changed_category);
    assert_eq!(
        local.lines().last().unwrap(),
        changed_category.lines().last().unwrap()
    );

    let panic = std::panic::catch_unwind(|| {
        assert_qpdf_oracle_matches_with(|case| {
            assert_eq!(case.name, target.name);
            changed_category.clone()
        });
    });
    assert!(panic.is_err());
}

#[cfg(unix)]
fn write_test_probe(path: &Path, source: &str) {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, source).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).unwrap();
}

#[cfg(unix)]
fn fake_probe_case() -> OracleCase {
    OracleCase {
        name: "fake-probe",
        codec: Codec::AsciiHex,
        fail_writes: vec![1, 3],
        fail_finishes: vec![2],
        operations: vec![Operation::Write(vec![0x00, 0xab]), Operation::Finish],
    }
}

#[cfg(unix)]
fn run_test_probe(probe: &Path, case: &OracleCase) -> String {
    let mut command = Command::new("/bin/sh");
    command.arg(probe);
    run_qpdf_probe_command(command, case)
}

#[cfg(unix)]
#[test]
fn qpdf_test_probe_does_not_direct_exec_write_open_script() {
    let directory = tempfile::tempdir().unwrap();
    let probe = directory.path().join("probe");
    write_test_probe(
        &probe,
        "#!/bin/sh\nprintf 'op\\t0\\tok\\t\\nop\\t1\\tok\\t\\noutput\\t\\n'\n",
    );
    let _write_open = std::fs::OpenOptions::new()
        .write(true)
        .open(&probe)
        .unwrap();

    assert_eq!(
        run_test_probe(&probe, &fake_probe_case()),
        "op\t0\tok\t\nop\t1\tok\t\noutput\t\n"
    );
}

#[cfg(unix)]
#[test]
fn qpdf_probe_receives_exact_positional_arguments() {
    let directory = tempfile::tempdir().unwrap();
    let probe = directory.path().join("probe");
    let arguments = directory.path().join("probe.args");
    write_test_probe(
        &probe,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >\"$0.args\"\nprintf 'op\\t0\\tok\\t\\nop\\t1\\tok\\t\\noutput\\t\\n'\n",
    );

    assert_eq!(
        run_test_probe(&probe, &fake_probe_case()),
        "op\t0\tok\t\nop\t1\tok\t\noutput\t\n"
    );
    assert_eq!(
        std::fs::read_to_string(arguments).unwrap(),
        "asciihex\n1,3\n2\nw:00ab\nf\n"
    );
}

#[cfg(unix)]
#[test]
fn qpdf_probe_failure_reports_case_stderr_and_exit_status() {
    let directory = tempfile::tempdir().unwrap();
    let probe = directory.path().join("probe");
    write_test_probe(&probe, "#!/bin/sh\nprintf 'probe stderr' >&2\nexit 7\n");

    let panic =
        std::panic::catch_unwind(|| run_test_probe(&probe, &fake_probe_case())).unwrap_err();
    let message = panic.downcast_ref::<String>().unwrap();
    assert!(message.contains("fake-probe"));
    assert!(message.contains("probe stderr"));
    assert!(message.contains("exit status: 7"));
}

#[cfg(unix)]
#[test]
fn qpdf_probe_rejects_non_utf8_stdout() {
    let directory = tempfile::tempdir().unwrap();
    let probe = directory.path().join("probe");
    write_test_probe(&probe, "#!/bin/sh\nprintf '\\377'\n");

    let panic =
        std::panic::catch_unwind(|| run_test_probe(&probe, &fake_probe_case())).unwrap_err();
    let message = panic.downcast_ref::<String>().unwrap();
    assert!(message.contains("fake-probe"));
    assert!(message.contains("UTF-8 ASCII protocol"));
}

#[cfg(unix)]
#[test]
fn qpdf_probe_rejects_stdout_protocol_corruption() {
    let directory = tempfile::tempdir().unwrap();
    let probe = directory.path().join("probe");
    write_test_probe(
        &probe,
        "#!/bin/sh\nprintf 'op\\t0\\tok\\t\\nop\\t1\\tok\\t\\ncall\\twrite\\t0\\t2\\t00\\noutput\\t00\\n'\n",
    );

    let panic =
        std::panic::catch_unwind(|| run_test_probe(&probe, &fake_probe_case())).unwrap_err();
    let message = panic.downcast_ref::<String>().unwrap();
    assert!(message.contains("fake-probe"));
    assert!(message.contains("protocol corruption"));
}

#[cfg(unix)]
#[test]
fn qpdf_probe_execution_failure_reports_the_case() {
    let directory = tempfile::tempdir().unwrap();
    let missing_probe = directory.path().join("missing-probe");

    let panic = std::panic::catch_unwind(|| run_qpdf_probe(&missing_probe, &fake_probe_case()))
        .unwrap_err();
    let message = panic.downcast_ref::<String>().unwrap();
    assert!(message.contains("execute qpdf stream-codec probe for fake-probe"));
}

#[cfg(unix)]
#[test]
fn qpdf_path_comparison_uses_the_probe_boundary() {
    let panic =
        std::panic::catch_unwind(|| assert_qpdf_oracle_matches(Path::new("true"))).unwrap_err();
    let message = panic.downcast_ref::<String>().unwrap();
    assert!(message.contains("ascii85-all-whitespace-and-nul"));
    assert!(message.contains("protocol corruption"));
}

#[test]
#[ignore = "live qpdf 11.9.0 stream-codec oracle"]
// cov:ignore-start: ignored live entry point; ordinary tests cover case generation, local traces, probe arguments, failures, and comparison
fn qpdf_stream_codecs_differential() {
    let probe = std::env::var_os("QPDF_STREAM_CODECS_PROBE")
        .expect("set QPDF_STREAM_CODECS_PROBE to the qpdf 11.9.0 probe");
    assert_qpdf_oracle_matches(Path::new(&probe));
}
// cov:ignore-end

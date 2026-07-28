//! qpdf correspondence: live differential instrumentation for Pl_LZWDecoder.cc and Pl_PNGFilter.cc.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use super::lzw::{pack_codes, LzwDecoder};
use super::png_filter::{PngFilter, PngFilterAction};
use super::test_support::{RecordingSink, TraceCall};
use super::{Pipeline, PipelineError, PipelineResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Codec {
    Lzw {
        early_code_change: bool,
    },
    Png {
        action: PngFilterAction,
        columns: u32,
        colors: u32,
        bits_per_component: u32,
    },
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
    fn as_probe_arg(self) -> String {
        match self {
            Self::Lzw { early_code_change } => {
                format!("lzw:{}", u8::from(early_code_change))
            }
            Self::Png {
                action,
                columns,
                colors,
                bits_per_component,
            } => {
                let name = match action {
                    PngFilterAction::Decode => "png-decode",
                    PngFilterAction::Encode => "png-encode",
                };
                format!("{name}:{columns},{colors},{bits_per_component}")
            }
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

fn lzw(codes: &[u32], early_code_change: bool) -> Vec<u8> {
    pack_codes(codes, early_code_change)
}

fn write_all(data: Vec<u8>) -> Vec<Operation> {
    vec![Operation::Write(data), Operation::Finish]
}

fn literal_run(count: usize) -> Vec<u32> {
    std::iter::once(256u32)
        .chain(std::iter::repeat_n(0x41u32, count))
        .collect()
}

const BYTE_ROW: Codec = Codec::Png {
    action: PngFilterAction::Decode,
    columns: 4,
    colors: 1,
    bits_per_component: 8,
};

const BYTE_ROW_ENCODE: Codec = Codec::Png {
    action: PngFilterAction::Encode,
    columns: 4,
    colors: 1,
    bits_per_component: 8,
};

fn oracle_cases() -> Vec<OracleCase> {
    vec![
        OracleCase {
            name: "lzw-clear-and-eod-only",
            codec: Codec::Lzw {
                early_code_change: true,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(lzw(&[256, 257], true)),
        },
        OracleCase {
            name: "lzw-literals-and-table-codes",
            codec: Codec::Lzw {
                early_code_change: true,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(lzw(&[256, 0x41, 0x42, 258, 259, 257], true)),
        },
        OracleCase {
            name: "lzw-self-referential-code",
            codec: Codec::Lzw {
                early_code_change: true,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(lzw(&[256, 0x41, 0x42, 258, 257], true)),
        },
        OracleCase {
            name: "lzw-intermediate-clear",
            codec: Codec::Lzw {
                early_code_change: true,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(lzw(
                &[256, 0x41, 0x42, 258, 256, 0x43, 0x44, 258, 257],
                true,
            )),
        },
        OracleCase {
            name: "lzw-width-transition-early",
            codec: Codec::Lzw {
                early_code_change: true,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(lzw(&literal_run(1800), true)),
        },
        OracleCase {
            name: "lzw-width-transition-late",
            codec: Codec::Lzw {
                early_code_change: false,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(lzw(&literal_run(1800), false)),
        },
        OracleCase {
            name: "lzw-table-full",
            codec: Codec::Lzw {
                early_code_change: true,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(lzw(&literal_run(3840), true)),
        },
        OracleCase {
            name: "lzw-table-nearly-full",
            codec: Codec::Lzw {
                early_code_change: true,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(lzw(&literal_run(3839), true)),
        },
        OracleCase {
            name: "lzw-bad-code-past-table-end",
            codec: Codec::Lzw {
                early_code_change: true,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(lzw(&[256, 0x41, 259], true)),
        },
        OracleCase {
            name: "lzw-table-code-after-clear",
            codec: Codec::Lzw {
                early_code_change: true,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(lzw(&[256, 258], true)),
        },
        OracleCase {
            name: "lzw-trailing-bits-without-eod",
            codec: Codec::Lzw {
                early_code_change: true,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(lzw(&[256, 0x41, 0x42], true)),
        },
        OracleCase {
            name: "lzw-input-after-eod",
            codec: Codec::Lzw {
                early_code_change: true,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(lzw(&[256, 0x41, 257, 0x42, 0x43], true)),
        },
        OracleCase {
            name: "lzw-eod-latched-across-finish",
            codec: Codec::Lzw {
                early_code_change: true,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![
                Operation::Write(lzw(&[256, 0x41, 0x42, 257], true)),
                Operation::Finish,
                Operation::Write(lzw(&[256, 0x41, 0x42], true)),
                Operation::Finish,
            ],
        },
        OracleCase {
            name: "lzw-state-retained-across-finish",
            codec: Codec::Lzw {
                early_code_change: true,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: {
                let stream = lzw(&[256, 0x41, 0x42, 258], true);
                vec![
                    Operation::Write(stream[..2].to_vec()),
                    Operation::Finish,
                    Operation::Write(stream[2..].to_vec()),
                    Operation::Finish,
                ]
            },
        },
        OracleCase {
            name: "lzw-split-writes",
            codec: Codec::Lzw {
                early_code_change: true,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: {
                let stream = lzw(&[256, 0x41, 0x42, 258, 259, 257], true);
                let mut operations: Vec<Operation> = stream
                    .iter()
                    .map(|byte| Operation::Write(vec![*byte]))
                    .collect();
                operations.push(Operation::Finish);
                operations
            },
        },
        OracleCase {
            name: "lzw-downstream-write-failure",
            codec: Codec::Lzw {
                early_code_change: true,
            },
            fail_writes: vec![2],
            fail_finishes: vec![],
            operations: write_all(lzw(&[256, 0x41, 0x42, 258, 257], true)),
        },
        OracleCase {
            name: "lzw-downstream-finish-failure",
            codec: Codec::Lzw {
                early_code_change: true,
            },
            fail_writes: vec![],
            fail_finishes: vec![1],
            operations: write_all(lzw(&[256, 0x41, 257], true)),
        },
        OracleCase {
            name: "png-decode-every-filter",
            codec: BYTE_ROW,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(vec![
                1, 0x01, 0x01, 0x01, 0x01, //
                2, 0x01, 0x01, 0x01, 0x01, //
                3, 0x07, 0x07, 0x07, 0x07, //
                4, 0x05, 0x05, 0x05, 0x05, //
                0, 0x09, 0x09, 0x09, 0x09,
            ]),
        },
        OracleCase {
            name: "png-decode-paeth-tie-breaks",
            codec: Codec::Png {
                action: PngFilterAction::Decode,
                columns: 2,
                colors: 1,
                bits_per_component: 8,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(vec![0, 100, 200, 4, 156, 0]),
        },
        OracleCase {
            name: "png-decode-unknown-filter-byte",
            codec: BYTE_ROW,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(vec![9, 0x01, 0x02, 0x03, 0x04, 200, 0x05, 0x06, 0x07, 0x08]),
        },
        OracleCase {
            name: "png-decode-truncated-final-row",
            codec: BYTE_ROW,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(vec![0, 0x01, 0x02, 0x03, 0x04, 2, 0xff]),
        },
        OracleCase {
            name: "png-decode-reuse-after-finish",
            codec: BYTE_ROW,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![
                Operation::Write(vec![1, 0x01, 0x01, 0x01, 0x01]),
                Operation::Finish,
                Operation::Write(vec![1, 0x01, 0x01, 0x01, 0x01]),
                Operation::Finish,
            ],
        },
        OracleCase {
            name: "png-decode-multi-byte-pixels",
            codec: Codec::Png {
                action: PngFilterAction::Decode,
                columns: 2,
                colors: 3,
                bits_per_component: 8,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(vec![
                4, 0x01, 0x02, 0x03, 0x01, 0x02, 0x03, //
                3, 0x01, 0x02, 0x03, 0x01, 0x02, 0x03,
            ]),
        },
        OracleCase {
            name: "png-decode-sixteen-bit-samples",
            codec: Codec::Png {
                action: PngFilterAction::Decode,
                columns: 2,
                colors: 1,
                bits_per_component: 16,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(vec![1, 0x01, 0x02, 0x03, 0x04, 4, 0xff, 0xfe, 0x01, 0x02]),
        },
        OracleCase {
            name: "png-decode-one-bit-samples",
            codec: Codec::Png {
                action: PngFilterAction::Decode,
                columns: 3,
                colors: 1,
                bits_per_component: 1,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(vec![1, 0xe0, 2, 0x11]),
        },
        OracleCase {
            name: "png-decode-split-writes",
            codec: BYTE_ROW,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: {
                let data: Vec<u8> = vec![
                    1, 0x01, 0x01, 0x01, 0x01, 2, 0x01, 0x01, 0x01, 0x01, 4, 0x00, 0x00, 0x00, 0x00,
                ];
                let mut operations: Vec<Operation> = data
                    .iter()
                    .map(|byte| Operation::Write(vec![*byte]))
                    .collect();
                operations.push(Operation::Finish);
                operations
            },
        },
        OracleCase {
            name: "png-decode-downstream-write-failure",
            codec: BYTE_ROW,
            fail_writes: vec![1],
            fail_finishes: vec![],
            operations: write_all(vec![2, 0x01, 0x02, 0x03, 0x04, 2, 0x01, 0x02, 0x03, 0x04]),
        },
        OracleCase {
            name: "png-decode-partial-row-write-failure",
            codec: BYTE_ROW,
            fail_writes: vec![1],
            fail_finishes: vec![],
            operations: vec![Operation::Write(vec![0, 0x01]), Operation::Finish],
        },
        OracleCase {
            name: "png-decode-downstream-finish-failure",
            codec: BYTE_ROW,
            fail_writes: vec![],
            fail_finishes: vec![1],
            operations: write_all(vec![0, 0x01, 0x02, 0x03, 0x04]),
        },
        OracleCase {
            name: "png-decode-empty-write-before-a-row",
            codec: BYTE_ROW,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![
                Operation::Write(vec![]),
                Operation::Write(vec![1, 0x01, 0x01, 0x01, 0x01]),
                Operation::Write(vec![]),
                Operation::Finish,
            ],
        },
        OracleCase {
            name: "png-encode-empty-write-before-a-row",
            codec: BYTE_ROW_ENCODE,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![
                Operation::Write(vec![]),
                Operation::Write(vec![0x01, 0x02, 0x03, 0x04]),
                Operation::Write(vec![]),
                Operation::Finish,
            ],
        },
        OracleCase {
            name: "png-encode-split-writes",
            codec: BYTE_ROW_ENCODE,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: {
                let data: Vec<u8> =
                    vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a];
                let mut operations: Vec<Operation> = data
                    .iter()
                    .map(|byte| Operation::Write(vec![*byte]))
                    .collect();
                operations.push(Operation::Finish);
                operations
            },
        },
        OracleCase {
            name: "png-encode-two-rows",
            codec: BYTE_ROW_ENCODE,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]),
        },
        OracleCase {
            name: "png-encode-truncated-final-row",
            codec: BYTE_ROW_ENCODE,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(vec![0x01, 0x02]),
        },
        OracleCase {
            name: "png-encode-reuse-after-finish",
            codec: BYTE_ROW_ENCODE,
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: vec![
                Operation::Write(vec![0x01, 0x02, 0x03, 0x04]),
                Operation::Finish,
                Operation::Write(vec![0x05, 0x06, 0x07, 0x08]),
                Operation::Finish,
            ],
        },
        OracleCase {
            name: "png-encode-header-write-failure",
            codec: BYTE_ROW_ENCODE,
            fail_writes: vec![1],
            fail_finishes: vec![],
            operations: write_all(vec![0x01, 0x02, 0x03, 0x04]),
        },
        OracleCase {
            name: "png-encode-body-write-failure",
            codec: BYTE_ROW_ENCODE,
            fail_writes: vec![3],
            fail_finishes: vec![],
            operations: write_all(vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]),
        },
        OracleCase {
            name: "png-construction-zero-samples-per-pixel",
            codec: Codec::Png {
                action: PngFilterAction::Decode,
                columns: 4,
                colors: 0,
                bits_per_component: 8,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(vec![0, 0x01, 0x02, 0x03, 0x04]),
        },
        OracleCase {
            name: "png-construction-invalid-bits-per-sample",
            codec: Codec::Png {
                action: PngFilterAction::Decode,
                columns: 4,
                colors: 1,
                bits_per_component: 3,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(vec![0, 0x01, 0x02, 0x03, 0x04]),
        },
        OracleCase {
            name: "png-construction-zero-columns",
            codec: Codec::Png {
                action: PngFilterAction::Decode,
                columns: 0,
                colors: 1,
                bits_per_component: 8,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(vec![0, 0x01]),
        },
        OracleCase {
            name: "png-construction-columns-wrap",
            codec: Codec::Png {
                action: PngFilterAction::Decode,
                columns: 536_870_912,
                colors: 1,
                bits_per_component: 8,
            },
            fail_writes: vec![],
            fail_finishes: vec![],
            operations: write_all(vec![0, 0x01]),
        },
    ]
}

fn construction_record(result: &PipelineResult<()>) -> String {
    match result {
        Ok(()) => "ctor\t0\tok\t\n".to_string(),
        Err(PipelineError::Logic(message)) => {
            format!("ctor\t0\tlogic\t{}\n", hex(message.as_bytes()))
        }
        Err(PipelineError::Runtime(message)) => {
            format!("ctor\t0\truntime\t{}\n", hex(message.as_bytes()))
        }
    }
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
        Codec::Lzw { early_code_change } => {
            let mut records = construction_record(&Ok(()));
            let mut stage = LzwDecoder::new("oracle codec", &mut sink, early_code_change);
            records.push_str(&execute_operations(&mut stage, &case.operations));
            records
        }
        Codec::Png {
            action,
            columns,
            colors,
            bits_per_component,
        } => {
            match PngFilter::new(
                "oracle codec",
                &mut sink,
                action,
                columns,
                colors,
                bits_per_component,
            ) {
                Ok(mut stage) => {
                    let mut records = construction_record(&Ok(()));
                    records.push_str(&execute_operations(&mut stage, &case.operations));
                    records
                }
                Err(error) => return construction_record(&Err(error)),
            }
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

fn validate_result_record(line: &str, record: &str, index: usize) -> Result<&'static str, String> {
    let [observed_record, observed_index, category, detail] =
        split_fields::<4>(line).ok_or_else(|| format!("malformed {record} record {index}"))?;
    if observed_record != record
        || parse_canonical_usize(observed_index) != Some(index)
        || !matches!(category, "ok" | "logic" | "runtime")
        || (category == "ok" && !detail.is_empty())
        || (category != "ok" && !is_lower_hex(detail))
    {
        return Err(format!("malformed {record} record {index}"));
    }
    Ok(if category == "ok" { "ok" } else { "error" })
}

fn validate_trace_protocol(trace: &str, operation_count: usize) -> Result<(), String> {
    if !trace.is_ascii() {
        return Err("stdout is not ASCII".to_string());
    }
    let body = trace
        .strip_suffix('\n')
        .ok_or_else(|| "trace has no final newline".to_string())?;
    let lines = body.split('\n').collect::<Vec<_>>();

    let construction = validate_result_record(lines[0], "ctor", 0)?;
    if construction == "error" {
        return (lines.len() == 1)
            .then_some(())
            .ok_or_else(|| "failed construction emitted extra records".to_string());
    }

    if lines.len() <= operation_count + 1 {
        return Err("trace is missing its output record".to_string());
    }
    for index in 0..operation_count {
        validate_result_record(lines[index + 1], "op", index)?;
    }

    let mut successful_output = String::new();
    for line in &lines[operation_count + 1..lines.len() - 1] {
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
        .unwrap_or_else(|error| panic!("execute qpdf LZW/PNG probe for {}: {error}", case.name));
    assert!(
        output.status.success(),
        "qpdf LZW/PNG probe failed for {} with status {}: {}",
        case.name,
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let trace = String::from_utf8(output.stdout).unwrap_or_else(|error| {
        panic!(
            "qpdf LZW/PNG probe produced non-UTF-8 ASCII protocol for {}: {error}",
            case.name
        )
    });
    validate_trace_protocol(&trace, case.operations.len()).unwrap_or_else(|error| {
        panic!(
            "qpdf LZW/PNG probe protocol corruption for {}: {error}",
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

// cov:ignore-start: ignored live entry point; ordinary tests cover case generation, local traces, probe arguments, failures, and comparison
#[test]
#[ignore = "live qpdf 11.9.0 LZW/PNG oracle"]
fn qpdf_lzw_png_differential() {
    let probe = std::env::var_os("QPDF_LZW_PNG_PROBE")
        .expect("set QPDF_LZW_PNG_PROBE to the qpdf 11.9.0 probe");
    assert_qpdf_oracle_matches(Path::new(&probe));
}
// cov:ignore-end

fn assert_qpdf_oracle_matches(probe: &Path) {
    assert_qpdf_oracle_matches_with(|case| run_qpdf_probe(probe, case));
}

#[cfg(test)]
mod tests {
    use super::{
        assert_qpdf_oracle_matches, assert_qpdf_oracle_matches_with, construction_record,
        flpdf_trace, operation_record, oracle_cases, run_qpdf_probe_command,
        validate_trace_protocol, Codec, Operation, OracleCase, PipelineError,
    };
    use crate::pipeline::png_filter::PngFilterAction;
    use std::path::Path;
    use std::process::Command;

    fn fake_case() -> OracleCase {
        OracleCase {
            name: "fake",
            codec: Codec::Lzw {
                early_code_change: true,
            },
            fail_writes: vec![2],
            fail_finishes: vec![3],
            operations: vec![Operation::Write(vec![0x80, 0x10]), Operation::Finish],
        }
    }

    #[test]
    fn probe_arguments_are_stable_for_every_codec_variant() {
        assert_eq!(
            Codec::Lzw {
                early_code_change: true
            }
            .as_probe_arg(),
            "lzw:1"
        );
        assert_eq!(
            Codec::Lzw {
                early_code_change: false
            }
            .as_probe_arg(),
            "lzw:0"
        );
        assert_eq!(
            Codec::Png {
                action: PngFilterAction::Decode,
                columns: 4,
                colors: 2,
                bits_per_component: 16
            }
            .as_probe_arg(),
            "png-decode:4,2,16"
        );
        assert_eq!(
            Codec::Png {
                action: PngFilterAction::Encode,
                columns: 1,
                colors: 1,
                bits_per_component: 8
            }
            .as_probe_arg(),
            "png-encode:1,1,8"
        );
        assert_eq!(Operation::Write(vec![0xab]).as_probe_arg(), "w:ab");
        assert_eq!(Operation::Finish.as_probe_arg(), "f");
    }

    #[test]
    fn operation_records_bind_exact_error_categories() {
        assert_eq!(operation_record(3, Ok(())), "op\t3\tok\t\n");
        assert_eq!(
            operation_record(3, Err(PipelineError::runtime("ab"))),
            "op\t3\truntime\t6162\n"
        );
        assert_eq!(
            operation_record(3, Err(PipelineError::logic("ab"))),
            "op\t3\tlogic\t6162\n"
        );
    }

    #[test]
    fn construction_records_bind_exact_error_categories() {
        assert_eq!(construction_record(&Ok(())), "ctor\t0\tok\t\n");
        assert_eq!(
            construction_record(&Err(PipelineError::runtime("ab"))),
            "ctor\t0\truntime\t6162\n"
        );
        assert_eq!(
            construction_record(&Err(PipelineError::logic("ab"))),
            "ctor\t0\tlogic\t6162\n"
        );
    }

    #[test]
    fn every_oracle_case_produces_a_well_formed_flpdf_trace() {
        let cases = oracle_cases();
        assert!(cases.len() >= 30);

        let mut names: Vec<&str> = cases.iter().map(|case| case.name).collect();
        names.sort_unstable();
        let unique = names.len();
        names.dedup();
        assert_eq!(names.len(), unique, "case names must be unique");

        for case in &cases {
            assert!(
                !case.operations.is_empty(),
                "case {} has no operations",
                case.name
            );
            let trace = flpdf_trace(case);
            assert!(
                trace.starts_with("ctor\t0\t"),
                "case {} trace: {trace:?}",
                case.name
            );
            assert_eq!(
                validate_trace_protocol(&trace, case.operations.len()),
                Ok(()),
                "case {}",
                case.name
            );
        }
    }

    #[test]
    fn failed_construction_suppresses_every_later_record() {
        let case = oracle_cases()
            .into_iter()
            .find(|case| case.name == "png-construction-zero-columns")
            .expect("case is registered");
        let trace = flpdf_trace(&case);
        assert_eq!(trace.lines().count(), 1);
        assert!(trace.starts_with("ctor\t0\truntime\t"));
    }

    #[test]
    fn protocol_validator_rejects_corruption_shapes() {
        let valid = "ctor\t0\tok\t\nop\t0\tok\t\ncall\twrite\t0\t1\t41\noutput\t41\n";
        assert_eq!(validate_trace_protocol(valid, 1), Ok(()));

        for (trace, reason) in [
            ("ctor\t0\tok\t\nop\t0\tok\t\noutput\t\u{80}\n", "non-ASCII"),
            ("ctor\t0\tok\t\nop\t0\tok\t\noutput\t", "no final newline"),
            ("ctor\t0\tok\t\n", "missing output record"),
            (
                "ctor\t0\tbad\t\nop\t0\tok\t\noutput\t\n",
                "bad ctor category",
            ),
            ("ctor\t1\tok\t\nop\t0\tok\t\noutput\t\n", "bad ctor index"),
            ("ctor\t0\tok\tab\nop\t0\tok\t\noutput\t\n", "ok with detail"),
            (
                "ctor\t0\truntime\tzz\nop\t0\tok\t\noutput\t\n",
                "non-hex detail",
            ),
            (
                "ctor\t0\truntime\t61\noutput\t\n",
                "records after failed construction",
            ),
            ("ctor\t0\tok\t\nop\t1\tok\t\noutput\t\n", "bad op index"),
            (
                "ctor\t0\tok\t\nop\t0\tok\t\ncall\twrite\t0\t2\t41\noutput\t41\n",
                "length mismatch",
            ),
            (
                "ctor\t0\tok\t\nop\t0\tok\t\ncall\tflush\t0\t0\t\noutput\t\n",
                "unknown call kind",
            ),
            (
                "ctor\t0\tok\t\nop\t0\tok\t\ncall\twrite\t0\t1\t41\noutput\t42\n",
                "output disagrees with writes",
            ),
            (
                "ctor\t0\tok\t\nop\t0\tok\t\ncall\twrite\t0\t1\t41\nresult\t41\n",
                "missing output prefix",
            ),
        ] {
            assert!(
                validate_trace_protocol(trace, 1).is_err(),
                "validator accepted {reason}"
            );
        }
    }

    #[test]
    fn failed_writes_are_excluded_from_the_output_record() {
        let trace =
            "ctor\t0\tok\t\nop\t0\tok\t\ncall\twrite\t1\t1\t41\ncall\tfinish\t0\t0\t\noutput\t\n";
        assert_eq!(validate_trace_protocol(trace, 1), Ok(()));
    }

    #[test]
    fn probe_receives_exact_positional_arguments() {
        let case = fake_case();
        let mut command = Command::new("printf");
        command.arg("ctor\t0\tok\t\nop\t0\tok\t\nop\t1\tok\t\noutput\t\n");
        // `printf` ignores the extra operands after its format string, so the
        // recorded argument list is what the real probe would receive.
        let arguments: Vec<String> = std::iter::once(case.codec.as_probe_arg())
            .chain([super::csv_or_dash(&case.fail_writes)])
            .chain([super::csv_or_dash(&case.fail_finishes)])
            .chain(case.operations.iter().map(Operation::as_probe_arg))
            .collect();
        assert_eq!(arguments, vec!["lzw:1", "2", "3", "w:8010", "f"]);
        assert_eq!(
            run_qpdf_probe_command(command, &case),
            "ctor\t0\tok\t\nop\t0\tok\t\nop\t1\tok\t\noutput\t\n"
        );
    }

    #[test]
    #[should_panic(expected = "qpdf LZW/PNG probe failed for fake")]
    fn probe_failure_reports_the_case_and_status() {
        run_qpdf_probe_command(Command::new("false"), &fake_case());
    }

    #[test]
    #[should_panic(expected = "execute qpdf LZW/PNG probe for fake")]
    fn probe_execution_failure_reports_the_case() {
        run_qpdf_probe_command(
            Command::new("/nonexistent/flpdf-lzw-png-probe"),
            &fake_case(),
        );
    }

    #[test]
    #[should_panic(expected = "qpdf LZW/PNG probe protocol corruption for fake")]
    fn probe_protocol_corruption_reports_the_case() {
        let mut command = Command::new("printf");
        command.arg("garbage\n");
        run_qpdf_probe_command(command, &fake_case());
    }

    #[test]
    #[should_panic(expected = "non-UTF-8 ASCII protocol for fake")]
    fn probe_non_utf8_stdout_reports_the_case() {
        let mut command = Command::new("printf");
        command.arg("\\377\n");
        run_qpdf_probe_command(command, &fake_case());
    }

    /// Every case is replayed against flpdf itself, which both completes the
    /// comparison loop and proves each trace is reproducible.
    #[test]
    fn comparison_accepts_an_agreeing_oracle() {
        assert_qpdf_oracle_matches_with(flpdf_trace);
    }

    #[test]
    #[should_panic(expected = "case lzw-clear-and-eod-only")]
    fn comparison_rejects_a_disagreeing_oracle() {
        assert_qpdf_oracle_matches_with(|_| "ctor\t0\tok\t\n".to_string());
    }

    #[test]
    #[should_panic(expected = "execute qpdf LZW/PNG probe")]
    fn path_comparison_uses_the_probe_boundary() {
        assert_qpdf_oracle_matches(Path::new("/nonexistent/flpdf-lzw-png-probe"));
    }
}

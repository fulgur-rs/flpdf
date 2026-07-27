//! qpdf correspondence: Pl_RC4.cc bounded streaming over one retained RC4 state.

use super::{Pipeline, PipelineError, PipelineResult};
use crate::security::rc4::Rc4;
use std::ffi::CStr;

pub(crate) const DEFAULT_OUT_BUFFER_SIZE: usize = 65_536;

pub(crate) struct PlRc4<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    rc4: Rc4,
    outbuf: Option<Vec<u8>>,
}

#[allow(dead_code)]
impl<'a> PlRc4<'a> {
    pub(crate) fn new(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        key: &[u8],
    ) -> PipelineResult<Self> {
        Self::with_buffer_size(identifier, next, key, DEFAULT_OUT_BUFFER_SIZE)
    }

    pub(crate) fn from_c_str(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        key: &CStr,
    ) -> PipelineResult<Self> {
        Self::from_cipher(
            identifier,
            next,
            Rc4::from_c_str(key).map_err(|error| PipelineError::runtime(error.to_string()))?,
            DEFAULT_OUT_BUFFER_SIZE,
        )
    }

    pub(crate) fn with_buffer_size(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        key: &[u8],
        out_buffer_size: usize,
    ) -> PipelineResult<Self> {
        Self::from_cipher(
            identifier,
            next,
            Rc4::new(key).map_err(|error| PipelineError::runtime(error.to_string()))?,
            out_buffer_size,
        )
    }

    fn from_cipher(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        rc4: Rc4,
        out_buffer_size: usize,
    ) -> PipelineResult<Self> {
        if out_buffer_size == 0 {
            return Err(PipelineError::logic(
                "Pl_RC4: output buffer size must be greater than zero",
            ));
        }
        Ok(Self {
            identifier: identifier.into(),
            next,
            rc4,
            outbuf: Some(vec![0; out_buffer_size]),
        })
    }
}

impl Pipeline for PlRc4<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        let identifier = &self.identifier;
        let outbuf = self.outbuf.as_mut().ok_or_else(|| {
            PipelineError::logic(format!(
                "{identifier}: Pl_RC4: write() called after finish() called"
            ))
        })?;

        for chunk in data.chunks(outbuf.len()) {
            let output = &mut outbuf[..chunk.len()];
            output.copy_from_slice(chunk);
            self.rc4.process_in_place(output);
            self.next.write(output)?;
        }
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        self.outbuf = None;
        self.next.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{PlRc4, DEFAULT_OUT_BUFFER_SIZE};
    use crate::pipeline::{Pipeline, PipelineError, PipelineResult};
    use crate::security::rc4::Rc4;
    use std::ffi::{CStr, CString};
    use std::path::Path;
    use std::process::Command;
    #[cfg(unix)]
    use std::{fs, os::unix::fs::PermissionsExt};

    #[derive(Default)]
    struct RecordingSink {
        chunks: Vec<Vec<u8>>,
        finishes: usize,
    }

    impl RecordingSink {
        fn bytes(&self) -> Vec<u8> {
            self.chunks.concat()
        }

        fn chunk_lengths(&self) -> Vec<usize> {
            self.chunks.iter().map(Vec::len).collect()
        }
    }

    impl Pipeline for RecordingSink {
        fn identifier(&self) -> &str {
            "recording"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.chunks.push(data.to_vec());
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.finishes += 1;
            Ok(())
        }
    }

    struct WriteFaultSink;

    impl Pipeline for WriteFaultSink {
        fn identifier(&self) -> &str {
            "write-fault"
        }

        fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
            Err(PipelineError::runtime(
                "write-fault: downstream write failed",
            ))
        }

        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FinishFaultSink {
        finishes: usize,
    }

    impl Pipeline for FinishFaultSink {
        fn identifier(&self) -> &str {
            "finish-fault"
        }

        fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.finishes += 1;
            Err(PipelineError::logic(
                "finish-fault: downstream finish failed",
            ))
        }
    }

    fn encrypt_chunks(chunks: &[&[u8]], out_buffer_size: usize) -> Vec<u8> {
        let mut sink = RecordingSink::default();
        {
            let mut stage =
                PlRc4::with_buffer_size("rc4", &mut sink, b"Key", out_buffer_size).unwrap();
            for chunk in chunks {
                stage.write(chunk).unwrap();
            }
            stage.finish().unwrap();
        }
        sink.bytes()
    }

    #[test]
    fn default_buffer_size_and_known_vector_match_qpdf() {
        assert_eq!(DEFAULT_OUT_BUFFER_SIZE, 65_536);
        assert_eq!(
            encrypt_chunks(&[b"Plaintext"], DEFAULT_OUT_BUFFER_SIZE),
            hex::decode("bbf316e8d940af0ad3").unwrap()
        );
    }

    #[test]
    fn retained_state_makes_split_writes_match_one_write() {
        let one = encrypt_chunks(&[b"Plaintext split across writes"], 7);
        let split = encrypt_chunks(&[b"Plain", b"text ", b"split across ", b"writes"], 7);
        assert_eq!(split, one);
    }

    #[test]
    fn default_buffer_emits_qpdf_boundary_chunks() {
        let input = vec![0x5a; DEFAULT_OUT_BUFFER_SIZE + 1];
        let mut sink = RecordingSink::default();
        {
            let mut stage = PlRc4::new("rc4", &mut sink, b"Key").unwrap();
            stage.write(&input).unwrap();
            stage.finish().unwrap();
        }
        assert_eq!(sink.chunk_lengths(), vec![DEFAULT_OUT_BUFFER_SIZE, 1]);
        assert_eq!(sink.bytes().len(), input.len());
    }

    #[test]
    fn custom_buffer_size_controls_only_downstream_chunking() {
        let input: Vec<u8> = (0..19).collect();
        let mut sink = RecordingSink::default();
        {
            let mut stage = PlRc4::with_buffer_size("rc4", &mut sink, b"Key", 8).unwrap();
            stage.write(&input).unwrap();
            stage.finish().unwrap();
        }
        assert_eq!(sink.chunk_lengths(), vec![8, 8, 3]);
        assert_eq!(
            sink.bytes(),
            encrypt_chunks(&[&input], DEFAULT_OUT_BUFFER_SIZE)
        );
    }

    #[test]
    fn empty_write_emits_nothing_and_does_not_advance_state() {
        let after_empty = encrypt_chunks(&[b"", b"payload"], 3);
        let without_empty = encrypt_chunks(&[b"payload"], 3);
        assert_eq!(after_empty, without_empty);
    }

    #[test]
    fn c_string_key_stops_at_first_nul() {
        let key = CStr::from_bytes_until_nul(b"Key\0ignored").unwrap();
        let mut sink = RecordingSink::default();
        {
            let mut stage = PlRc4::from_c_str("rc4", &mut sink, key).unwrap();
            stage.write(b"Plaintext").unwrap();
            stage.finish().unwrap();
        }
        assert_eq!(
            sink.bytes(),
            encrypt_chunks(&[b"Plaintext"], DEFAULT_OUT_BUFFER_SIZE)
        );
    }

    #[test]
    fn out_of_place_pipeline_matches_in_place_core() {
        let mut expected = b"stateful in-place comparison".to_vec();
        Rc4::new(b"Key")
            .unwrap()
            .process_in_place(expected.as_mut_slice());
        assert_eq!(
            encrypt_chunks(&[b"stateful in-place comparison"], 5),
            expected
        );
    }

    #[test]
    fn repeated_finish_propagates_each_time_and_marks_stage_finished() {
        let mut sink = RecordingSink::default();
        {
            let mut stage = PlRc4::new("rc4", &mut sink, b"Key").unwrap();
            stage.finish().unwrap();
            stage.finish().unwrap();
            assert_eq!(
                stage.write(b"x").unwrap_err().to_string(),
                "rc4: Pl_RC4: write() called after finish() called"
            );
        }
        assert_eq!(sink.finishes, 2);
    }

    #[test]
    fn downstream_write_error_is_returned_unchanged() {
        let mut sink = WriteFaultSink;
        let mut stage = PlRc4::new("rc4", &mut sink, b"Key").unwrap();
        let error = stage.write(b"x").unwrap_err();
        assert!(matches!(error, PipelineError::Runtime(_)));
        assert_eq!(error.to_string(), "write-fault: downstream write failed");
    }

    #[test]
    fn failed_downstream_finish_still_marks_stage_finished() {
        let mut sink = FinishFaultSink::default();
        {
            let mut stage = PlRc4::new("rc4", &mut sink, b"Key").unwrap();
            let error = stage.finish().unwrap_err();
            assert!(matches!(error, PipelineError::Logic(_)));
            assert_eq!(error.to_string(), "finish-fault: downstream finish failed");
            assert_eq!(
                stage.write(b"x").unwrap_err().to_string(),
                "rc4: Pl_RC4: write() called after finish() called"
            );
            assert!(matches!(
                stage.finish().unwrap_err(),
                PipelineError::Logic(_)
            ));
        }
        assert_eq!(sink.finishes, 2);
    }

    #[test]
    fn zero_output_buffer_is_rejected_without_processing() {
        let mut sink = RecordingSink::default();
        let error = PlRc4::with_buffer_size("rc4", &mut sink, b"Key", 0)
            .err()
            .expect("zero-sized output buffer must be rejected");
        assert!(matches!(error, PipelineError::Logic(_)));
        assert_eq!(
            error.to_string(),
            "Pl_RC4: output buffer size must be greater than zero"
        );
    }

    #[derive(Clone, Copy)]
    enum OracleKeyMode {
        Explicit,
        CStr,
    }

    struct OracleCase {
        name: &'static str,
        mode: OracleKeyMode,
        key: Vec<u8>,
        input_len: usize,
        write_split: usize,
        out_buffer_size: usize,
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn oracle_input(len: usize) -> Vec<u8> {
        (0..len)
            .map(|index| (index.wrapping_mul(37).wrapping_add(11)) as u8)
            .collect()
    }

    fn oracle_cases() -> Vec<OracleCase> {
        vec![
            OracleCase {
                name: "empty-input",
                mode: OracleKeyMode::Explicit,
                key: b"Key".to_vec(),
                input_len: 0,
                write_split: 0,
                out_buffer_size: DEFAULT_OUT_BUFFER_SIZE,
            },
            OracleCase {
                name: "below-default-boundary",
                mode: OracleKeyMode::Explicit,
                key: (0..16).collect(),
                input_len: DEFAULT_OUT_BUFFER_SIZE - 1,
                write_split: 31,
                out_buffer_size: DEFAULT_OUT_BUFFER_SIZE,
            },
            OracleCase {
                name: "at-default-boundary",
                mode: OracleKeyMode::Explicit,
                key: (1..=5).collect(),
                input_len: DEFAULT_OUT_BUFFER_SIZE,
                write_split: DEFAULT_OUT_BUFFER_SIZE,
                out_buffer_size: DEFAULT_OUT_BUFFER_SIZE,
            },
            OracleCase {
                name: "above-default-boundary",
                mode: OracleKeyMode::Explicit,
                key: b"stream-key".to_vec(),
                input_len: DEFAULT_OUT_BUFFER_SIZE + 1,
                write_split: 17,
                out_buffer_size: DEFAULT_OUT_BUFFER_SIZE,
            },
            OracleCase {
                name: "multiple-default-buffers",
                mode: OracleKeyMode::Explicit,
                key: (0..=255).collect(),
                input_len: DEFAULT_OUT_BUFFER_SIZE * 2 + 1,
                write_split: DEFAULT_OUT_BUFFER_SIZE + 9,
                out_buffer_size: DEFAULT_OUT_BUFFER_SIZE,
            },
            OracleCase {
                name: "custom-buffer",
                mode: OracleKeyMode::Explicit,
                key: b"custom".to_vec(),
                input_len: 211,
                write_split: 100,
                out_buffer_size: 64,
            },
            OracleCase {
                name: "c-string-key",
                mode: OracleKeyMode::CStr,
                key: b"Key\0ignored".to_vec(),
                input_len: 97,
                write_split: 41,
                out_buffer_size: DEFAULT_OUT_BUFFER_SIZE,
            },
        ]
    }

    fn flpdf_oracle_record(case: &OracleCase) -> String {
        let input = oracle_input(case.input_len);
        let mut sink = RecordingSink::default();
        let after_finish;
        {
            let mut stage = match case.mode {
                OracleKeyMode::Explicit => {
                    PlRc4::with_buffer_size("pl-rc4", &mut sink, &case.key, case.out_buffer_size)
                        .unwrap()
                }
                OracleKeyMode::CStr => {
                    let key = CString::new(
                        case.key
                            .split(|byte| *byte == 0)
                            .next()
                            .expect("C-string oracle key has a prefix"),
                    )
                    .unwrap();
                    PlRc4::from_c_str("pl-rc4", &mut sink, &key).unwrap()
                }
            };
            stage.write(&input[..case.write_split]).unwrap();
            stage.write(&input[case.write_split..]).unwrap();
            stage.finish().unwrap();
            stage.finish().unwrap();
            after_finish = stage.write(b"x").unwrap_err().to_string();
        }
        let chunks = sink
            .chunk_lengths()
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "output\t{}\nchunks\t{chunks}\nfinishes\t{}\nafter-finish\t{after_finish}\n",
            hex(&sink.bytes()),
            sink.finishes
        )
    }

    fn run_qpdf_pl_rc4_probe(probe: &Path, case: &OracleCase) -> String {
        let mode = match case.mode {
            OracleKeyMode::Explicit => "explicit",
            OracleKeyMode::CStr => "cstr",
        };
        let output = Command::new(probe)
            .args([
                "pipeline",
                mode,
                &hex(&case.key),
                &case.input_len.to_string(),
                &case.write_split.to_string(),
                &case.out_buffer_size.to_string(),
            ])
            .output()
            .expect("execute qpdf Pl_RC4 probe");
        assert!(
            output.status.success(),
            "qpdf Pl_RC4 probe failed for {}: {}",
            case.name,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("Pl_RC4 probe output is ASCII")
    }

    fn assert_qpdf_pl_rc4_oracle_matches(mut qpdf_records: impl FnMut(&OracleCase) -> String) {
        for case in oracle_cases() {
            assert_eq!(
                flpdf_oracle_record(&case),
                qpdf_records(&case),
                "case {}",
                case.name
            );
        }
    }

    #[test]
    #[ignore = "live qpdf 11.9.0 Pl_RC4 oracle"]
    // cov:ignore-start: ignored live entry point; ordinary tests cover the comparison and process boundary
    fn qpdf_rc4_differential_pl_rc4_pipeline() {
        let probe = std::env::var_os("QPDF_PL_RC4_PROBE")
            .expect("set QPDF_PL_RC4_PROBE to the qpdf 11.9.0 probe");
        assert_qpdf_pl_rc4_oracle_matches(|case| run_qpdf_pl_rc4_probe(Path::new(&probe), case));
    }
    // cov:ignore-end

    #[test]
    fn qpdf_pl_rc4_comparison_checks_every_oracle_case() {
        let mut visited = Vec::new();
        assert_qpdf_pl_rc4_oracle_matches(|case| {
            visited.push(case.name);
            flpdf_oracle_record(case)
        });
        assert_eq!(visited.len(), oracle_cases().len());
    }

    #[cfg(unix)]
    fn write_test_probe(path: &Path, source: &str) {
        fs::write(path, source).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn qpdf_pl_rc4_probe_receives_exact_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        write_test_probe(&probe, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n");
        let case = OracleCase {
            name: "arguments",
            mode: OracleKeyMode::Explicit,
            key: vec![0x01, 0xab],
            input_len: 9,
            write_split: 4,
            out_buffer_size: 7,
        };
        assert_eq!(
            run_qpdf_pl_rc4_probe(&probe, &case),
            "pipeline\nexplicit\n01ab\n9\n4\n7\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn qpdf_pl_rc4_probe_failure_reports_case_and_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        write_test_probe(&probe, "#!/bin/sh\nprintf 'probe stderr' >&2\nexit 7\n");
        let case = OracleCase {
            name: "failure-case",
            mode: OracleKeyMode::Explicit,
            key: vec![1],
            input_len: 0,
            write_split: 0,
            out_buffer_size: DEFAULT_OUT_BUFFER_SIZE,
        };
        let panic = std::panic::catch_unwind(|| run_qpdf_pl_rc4_probe(&probe, &case)).unwrap_err();
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("qpdf Pl_RC4 probe failed for failure-case"));
        assert!(message.contains("probe stderr"));
    }

    #[cfg(unix)]
    #[test]
    fn qpdf_pl_rc4_probe_rejects_non_utf8_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        write_test_probe(&probe, "#!/bin/sh\nprintf '\\377'\n");
        let case = OracleCase {
            name: "non-utf8",
            mode: OracleKeyMode::Explicit,
            key: vec![1],
            input_len: 0,
            write_split: 0,
            out_buffer_size: DEFAULT_OUT_BUFFER_SIZE,
        };
        let panic = std::panic::catch_unwind(|| run_qpdf_pl_rc4_probe(&probe, &case)).unwrap_err();
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("Pl_RC4 probe output is ASCII"));
    }
}

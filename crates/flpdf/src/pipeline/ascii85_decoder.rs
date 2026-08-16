//! qpdf correspondence: Pl_ASCII85Decoder.cc incremental decode state, output, error, and finish semantics.

use super::{Pipeline, PipelineError, PipelineRef, PipelineResult};

pub(crate) struct Ascii85Decoder<'a> {
    identifier: String,
    next: PipelineRef<'a>,
    inbuf: [u8; 5],
    pos: usize,
    eod: u8,
}

#[allow(dead_code)]
impl<'a> Ascii85Decoder<'a> {
    pub(crate) fn new(identifier: impl Into<String>, next: impl Into<PipelineRef<'a>>) -> Self {
        Self {
            identifier: identifier.into(),
            next: next.into(),
            inbuf: [b'u'; 5],
            pos: 0,
            eod: 0,
        }
    }

    fn flush(&mut self) -> PipelineResult<()> {
        if self.pos == 0 {
            return Ok(());
        }

        let mut value = 0u32;
        for byte in self.inbuf {
            value = value.wrapping_mul(85).wrapping_add(u32::from(byte - b'!'));
        }

        let mut out = [0; 4];
        for byte in out.iter_mut().rev() {
            *byte = value as u8;
            value >>= 8;
        }

        let output_len = self.pos - 1;
        self.pos = 0;
        self.inbuf = [b'u'; 5];
        self.next.write(&out[..output_len])
    }
}

impl Pipeline for Ascii85Decoder<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        if self.eod > 1 {
            return Ok(());
        }
        for &byte in data {
            if matches!(byte, b' ' | b'\x0c' | b'\x0b' | b'\t' | b'\r' | b'\n') {
                continue;
            }
            if self.eod > 1 {
                break;
            } else if self.eod == 1 {
                if byte == b'>' {
                    self.flush()?;
                    self.eod = 2;
                } else {
                    return Err(PipelineError::runtime(
                        "broken end-of-data sequence in base 85 data",
                    ));
                }
            } else {
                match byte {
                    b'~' => self.eod = 1,
                    b'z' if self.pos == 0 => self.next.write(&[0; 4])?,
                    b'z' => {
                        return Err(PipelineError::runtime("unexpected z during base 85 decode"));
                    }
                    b'!'..=b'u' => {
                        self.inbuf[self.pos] = byte;
                        self.pos += 1;
                        if self.pos == 5 {
                            self.flush()?;
                        }
                    }
                    _ => {
                        return Err(PipelineError::runtime(
                            "character out of range during base 85 decode",
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        self.flush()?;
        self.next.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::Ascii85Decoder;
    use crate::pipeline::test_support::{RecordingSink, Trace, TraceCall};
    use crate::pipeline::Pipeline;

    fn trace_after(
        write: impl FnOnce(&mut Ascii85Decoder<'_>),
        fail_writes: &[usize],
        fail_finishes: &[usize],
    ) -> Trace {
        let mut sink = RecordingSink::new(fail_writes, fail_finishes);
        let trace = sink.trace();
        {
            let mut decoder = Ascii85Decoder::new("ascii85", &mut sink);
            write(&mut decoder);
        }
        let observed = trace.borrow().clone();
        observed
    }

    #[test]
    fn decodes_qpdf_success_cases() {
        let success_cases = [
            (b"9jqo^".as_slice(), b"Man ".as_slice()),
            (b"z".as_slice(), &[0, 0, 0, 0]),
            (b"!".as_slice(), b"".as_slice()),
            (b"!!".as_slice(), &[0]),
            (b"!!!".as_slice(), &[0, 0]),
            (b"!!!!".as_slice(), &[0, 0, 0]),
            (b"uuuuu".as_slice(), &[0x08, 0x78, 0x0e, 0xc4]),
            (b"9jqo^~ \x0c\x0b\t\r\n>ignored".as_slice(), b"Man "),
        ];

        for (input, expected) in success_cases {
            let mut sink = RecordingSink::new(&[], &[]);
            let trace = sink.trace();
            {
                let mut decoder = Ascii85Decoder::new("ascii85", &mut sink);
                decoder.write(input).unwrap();
                decoder.finish().unwrap();
            }
            assert_eq!(trace.borrow().output, expected, "input: {input:?}");
        }
    }

    #[test]
    fn rejects_qpdf_runtime_error_cases() {
        for (input, expected) in [
            (
                b"!\0".as_slice(),
                "character out of range during base 85 decode",
            ),
            (b"!z".as_slice(), "unexpected z during base 85 decode"),
            (
                b"~X".as_slice(),
                "broken end-of-data sequence in base 85 data",
            ),
        ] {
            let mut sink = RecordingSink::new(&[], &[]);
            let mut decoder = Ascii85Decoder::new("ascii85", &mut sink);
            assert_eq!(decoder.write(input).unwrap_err().to_string(), expected);
        }
    }

    #[test]
    fn identifiers_expose_pipeline_names() {
        let mut sink = RecordingSink::new(&[], &[]);
        assert_eq!(sink.identifier(), "recording");
        let decoder = Ascii85Decoder::new("ascii85", &mut sink);
        assert_eq!(decoder.identifier(), "ascii85");
    }

    #[test]
    fn split_writes_preserve_a_single_full_group_flush() {
        let input = b"9jqo^~>";
        for split in 0..=input.len() {
            let mut sink = RecordingSink::new(&[], &[]);
            let trace = sink.trace();
            {
                let mut decoder = Ascii85Decoder::new("ascii85", &mut sink);
                decoder.write(&input[..split]).unwrap();
                decoder.write(&input[split..]).unwrap();
                decoder.finish().unwrap();
            }
            assert_eq!(
                trace.borrow().calls,
                vec![
                    TraceCall::Write {
                        data: b"Man ".to_vec(),
                        failed: false,
                    },
                    TraceCall::Finish { failed: false },
                ],
                "split: {split}"
            );
        }
    }

    #[test]
    fn finish_flushes_pending_eod_prefix() {
        let trace = trace_after(
            |decoder| {
                decoder.write(b"9jqo^~").unwrap();
                decoder.finish().unwrap();
            },
            &[],
            &[],
        );

        assert_eq!(trace.output, b"Man ");
        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: b"Man ".to_vec(),
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn one_character_final_group_attempts_an_empty_write() {
        let trace = trace_after(
            |decoder| {
                decoder.write(b"!").unwrap();
                decoder.finish().unwrap();
            },
            &[],
            &[],
        );

        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: vec![],
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn ignores_data_from_a_later_write_after_completed_eod() {
        let trace = trace_after(
            |decoder| {
                decoder.write(b"9jqo^~>").unwrap();
                decoder.write(b"ignored").unwrap();
                decoder.finish().unwrap();
            },
            &[],
            &[],
        );

        assert_eq!(trace.output, b"Man ");
        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: b"Man ".to_vec(),
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn recording_sink_finish_failure_is_recorded_and_retryable() {
        let mut sink = RecordingSink::new(&[], &[1]);
        let trace = sink.trace();

        assert_eq!(
            sink.finish().unwrap_err().to_string(),
            "sink finish failure 1"
        );
        sink.finish().unwrap();

        assert_eq!(
            trace.borrow().calls,
            vec![
                TraceCall::Finish { failed: true },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn repeated_finishes_are_forwarded() {
        let trace = trace_after(
            |decoder| {
                decoder.finish().unwrap();
                decoder.finish().unwrap();
            },
            &[],
            &[],
        );

        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Finish { failed: false },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn full_flush_failure_resets_the_group_before_the_next_write() {
        let trace = trace_after(
            |decoder| {
                assert_eq!(
                    decoder.write(b"9jqo^").unwrap_err().to_string(),
                    "sink write failure 1"
                );
                decoder.write(b"9jqo^").unwrap();
            },
            &[1],
            &[],
        );

        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: b"Man ".to_vec(),
                    failed: true,
                },
                TraceCall::Write {
                    data: b"Man ".to_vec(),
                    failed: false,
                },
            ]
        );
    }

    #[test]
    fn partial_flush_failure_resets_the_group_before_the_next_write() {
        let trace = trace_after(
            |decoder| {
                decoder.write(b"!!").unwrap();
                assert_eq!(
                    decoder.finish().unwrap_err().to_string(),
                    "sink write failure 1"
                );
                decoder.write(b"!!").unwrap();
                decoder.finish().unwrap();
            },
            &[1],
            &[],
        );

        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: vec![0],
                    failed: true,
                },
                TraceCall::Write {
                    data: vec![0],
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn failed_eod_flush_waits_for_a_later_end_marker() {
        let trace = trace_after(
            |decoder| {
                decoder.write(b"!~").unwrap();
                assert_eq!(
                    decoder.write(b">").unwrap_err().to_string(),
                    "sink write failure 1"
                );
                decoder.write(b">").unwrap();
                decoder.finish().unwrap();
            },
            &[1],
            &[],
        );

        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: vec![],
                    failed: true,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn flush_failure_suppresses_that_finish_call() {
        let trace = trace_after(
            |decoder| {
                decoder.write(b"!!").unwrap();
                assert_eq!(
                    decoder.finish().unwrap_err().to_string(),
                    "sink write failure 1"
                );
            },
            &[1],
            &[],
        );

        assert_eq!(
            trace.calls,
            vec![TraceCall::Write {
                data: vec![0],
                failed: true,
            }]
        );
    }

    #[test]
    fn can_decode_after_finish_without_an_explicit_eod() {
        let trace = trace_after(
            |decoder| {
                decoder.write(b"!!").unwrap();
                decoder.finish().unwrap();
                decoder.write(b"!!!").unwrap();
                decoder.finish().unwrap();
            },
            &[],
            &[],
        );

        assert_eq!(trace.output, [0, 0, 0]);
        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: vec![0],
                    failed: false,
                },
                TraceCall::Finish { failed: false },
                TraceCall::Write {
                    data: vec![0, 0],
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }
}

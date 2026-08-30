//! qpdf correspondence: Pl_ASCIIHexDecoder.cc incremental decode state, output, error, and finish semantics.

use super::{Pipeline, PipelineError, PipelineRef, PipelineResult};

pub(crate) struct AsciiHexDecoder<'a> {
    identifier: String,
    next: PipelineRef<'a>,
    inbuf: [u8; 2],
    pos: usize,
    eod: bool,
}

impl<'a> AsciiHexDecoder<'a> {
    pub(crate) fn new(identifier: impl Into<String>, next: impl Into<PipelineRef<'a>>) -> Self {
        Self {
            identifier: identifier.into(),
            next: next.into(),
            inbuf: *b"00",
            pos: 0,
            eod: false,
        }
    }

    fn flush(&mut self) -> PipelineResult<()> {
        if self.pos == 0 {
            return Ok(());
        }

        let mut digits = [0; 2];
        for (digit, byte) in digits.iter_mut().zip(self.inbuf) {
            *digit = if byte >= b'A' {
                byte - b'A' + 10
            } else {
                byte - b'0'
            };
        }
        let output = [(digits[0] << 4) + digits[1]];

        self.pos = 0;
        self.inbuf = *b"00";
        self.next.write(&output)
    }
}

impl Pipeline for AsciiHexDecoder<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        if self.eod {
            return Ok(());
        }

        for &byte in data {
            let ch = byte.to_ascii_uppercase();
            match ch {
                b' ' | b'\x0c' | b'\x0b' | b'\t' | b'\r' | b'\n' => {}
                b'>' => {
                    self.eod = true;
                    self.flush()?;
                }
                b'0'..=b'9' | b'A'..=b'F' => {
                    self.inbuf[self.pos] = ch;
                    self.pos += 1;
                    if self.pos == 2 {
                        self.flush()?;
                    }
                }
                _ => {
                    let mut detail = b"character out of range during base Hex decode: ".to_vec();
                    if ch != 0 {
                        detail.push(ch);
                    }
                    return Err(PipelineError::runtime_bytes(detail));
                }
            }
            if self.eod {
                break;
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
    use super::AsciiHexDecoder;
    use crate::pipeline::test_support::{RecordingSink, Trace, TraceCall};
    use crate::pipeline::Pipeline;

    fn trace_after(
        write: impl FnOnce(&mut AsciiHexDecoder<'_>),
        fail_writes: &[usize],
        fail_finishes: &[usize],
    ) -> Trace {
        let mut sink = RecordingSink::new(fail_writes, fail_finishes);
        let trace = sink.trace();
        {
            let mut decoder = AsciiHexDecoder::new("asciihex", &mut sink);
            write(&mut decoder);
        }
        let observed = trace.borrow().clone();
        observed
    }

    #[test]
    fn decodes_qpdf_success_cases() {
        let success_cases = [
            (b"48656c6c6f".as_slice(), b"Hello".as_slice()),
            (b"4f6".as_slice(), &[0x4f, 0x60]),
            (b"4F6C".as_slice(), &[0x4f, 0x6c]),
            (b"4 \x0c\x0b\t\r\n8".as_slice(), &[0x48]),
            (b"4>ignored".as_slice(), &[0x40]),
        ];

        for (input, expected) in success_cases {
            let mut sink = RecordingSink::new(&[], &[]);
            let trace = sink.trace();
            {
                let mut decoder = AsciiHexDecoder::new("asciihex", &mut sink);
                decoder.write(input).unwrap();
                decoder.finish().unwrap();
            }
            assert_eq!(trace.borrow().output, expected, "input: {input:?}");
        }
    }

    #[test]
    fn nul_error_follows_the_first_complete_pair_write() {
        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        let mut decoder = AsciiHexDecoder::new("asciihex", &mut sink);

        assert_eq!(
            decoder.write(b"48\0").unwrap_err().to_string(),
            "character out of range during base Hex decode: "
        );
        assert_eq!(
            trace.borrow().calls,
            vec![TraceCall::Write {
                data: vec![0x48],
                failed: false,
            }]
        );
    }

    #[test]
    fn invalid_character_error_includes_the_visible_uppercase_suffix() {
        let mut sink = RecordingSink::new(&[], &[]);
        let mut decoder = AsciiHexDecoder::new("asciihex", &mut sink);

        assert_eq!(
            decoder.write(b"4G").unwrap_err().to_string(),
            "character out of range during base Hex decode: G"
        );
    }

    #[test]
    fn invalid_high_bytes_remain_raw_internally_and_display_lossily() {
        for byte in [0x80, 0xff] {
            let mut sink = RecordingSink::new(&[], &[]);
            let mut decoder = AsciiHexDecoder::new("asciihex", &mut sink);
            let error = decoder.write(&[byte]).unwrap_err();
            let expected = [
                b"character out of range during base Hex decode: ".as_slice(),
                &[byte],
            ]
            .concat();

            assert!(matches!(error, crate::pipeline::PipelineError::Runtime(_)));
            assert_eq!(error.message_bytes(), expected);
            assert_eq!(
                error.to_string(),
                "character out of range during base Hex decode: \u{fffd}"
            );
        }
    }

    #[test]
    fn identifiers_expose_pipeline_names() {
        let mut sink = RecordingSink::new(&[], &[]);
        assert_eq!(sink.identifier(), "recording");
        let decoder = AsciiHexDecoder::new("asciihex", &mut sink);
        assert_eq!(decoder.identifier(), "asciihex");
    }

    #[test]
    fn split_writes_match_unsplit_one_byte_writes() {
        let input = b"48656c6c6f>";
        let unsplit = trace_after(
            |decoder| {
                decoder.write(input).unwrap();
                decoder.finish().unwrap();
            },
            &[],
            &[],
        );

        for split in 0..=input.len() {
            let trace = trace_after(
                |decoder| {
                    decoder.write(&input[..split]).unwrap();
                    decoder.write(&input[split..]).unwrap();
                    decoder.finish().unwrap();
                },
                &[],
                &[],
            );
            assert_eq!(trace.output, unsplit.output, "split: {split}");
            assert_eq!(trace.calls, unsplit.calls, "split: {split}");
        }
    }

    #[test]
    fn eod_flushes_a_pending_nibble() {
        let trace = trace_after(
            |decoder| {
                decoder.write(b"4>").unwrap();
                decoder.finish().unwrap();
            },
            &[],
            &[],
        );

        assert_eq!(trace.output, [0x40]);
        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: vec![0x40],
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn ignores_data_from_a_later_write_after_eod() {
        let trace = trace_after(
            |decoder| {
                decoder.write(b"48>").unwrap();
                decoder.write(b"ignored").unwrap();
                decoder.finish().unwrap();
            },
            &[],
            &[],
        );

        assert_eq!(trace.output, [0x48]);
        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: vec![0x48],
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn finish_flushes_a_pending_nibble() {
        let trace = trace_after(
            |decoder| {
                decoder.write(b"4").unwrap();
                decoder.finish().unwrap();
            },
            &[],
            &[],
        );

        assert_eq!(trace.output, [0x40]);
        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: vec![0x40],
                    failed: false,
                },
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
    fn full_pair_flush_failure_resets_before_reuse() {
        let trace = trace_after(
            |decoder| {
                assert_eq!(
                    decoder.write(b"48").unwrap_err().to_string(),
                    "sink write failure 1"
                );
                decoder.write(b"48").unwrap();
            },
            &[1],
            &[],
        );

        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: vec![0x48],
                    failed: true,
                },
                TraceCall::Write {
                    data: vec![0x48],
                    failed: false,
                },
            ]
        );
    }

    #[test]
    fn failed_eod_partial_flush_retains_eod_and_suppresses_that_finish() {
        let trace = trace_after(
            |decoder| {
                assert_eq!(
                    decoder.write(b"4>").unwrap_err().to_string(),
                    "sink write failure 1"
                );
                decoder.write(b"8").unwrap();
                decoder.finish().unwrap();
            },
            &[1],
            &[],
        );

        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: vec![0x40],
                    failed: true,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn failed_partial_finish_flush_suppresses_its_downstream_finish() {
        let trace = trace_after(
            |decoder| {
                decoder.write(b"4").unwrap();
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
                data: vec![0x40],
                failed: true,
            }]
        );
    }

    #[test]
    fn can_decode_after_finish_without_an_explicit_eod() {
        let trace = trace_after(
            |decoder| {
                decoder.write(b"4").unwrap();
                decoder.finish().unwrap();
                decoder.write(b"8").unwrap();
                decoder.finish().unwrap();
            },
            &[],
            &[],
        );

        assert_eq!(trace.output, [0x40, 0x80]);
        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: vec![0x40],
                    failed: false,
                },
                TraceCall::Finish { failed: false },
                TraceCall::Write {
                    data: vec![0x80],
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }
}

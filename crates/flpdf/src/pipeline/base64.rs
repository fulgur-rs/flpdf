//! qpdf correspondence: Pl_Base64.cc streaming encode/decode, aliases, padding, and lifecycle.

use super::{Pipeline, PipelineError, PipelineResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base64Action {
    Encode,
    Decode,
}

pub struct PlBase64<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    action: Base64Action,
    buffer: [u8; 4],
    position: usize,
    end_of_data: bool,
    finished: bool,
}

impl<'a> PlBase64<'a> {
    pub fn new(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        action: Base64Action,
    ) -> Self {
        Self {
            identifier: identifier.into(),
            next,
            action,
            buffer: [0; 4],
            position: 0,
            end_of_data: false,
            finished: false,
        }
    }

    fn flush_quantum(&mut self) -> PipelineResult<()> {
        match self.action {
            Base64Action::Encode => self.flush_encode()?,
            Base64Action::Decode => self.flush_decode()?,
        }
        self.position = 0;
        self.buffer.fill(0);
        Ok(())
    }

    fn flush_decode(&mut self) -> PipelineResult<()> {
        if self.end_of_data {
            return Err(PipelineError::runtime(format!(
                "{}: base64 decode: data follows pad characters",
                self.identifier
            )));
        }

        let mut padding = 0;
        let mut output = 0_u32;
        for index in 0..4 {
            let byte = self.buffer[index];
            let value = if let Some(value) = decode_value(byte) {
                value
            } else if byte == b'=' && (index == 3 || (index == 2 && self.buffer[3] == b'=')) {
                padding += 1;
                self.end_of_data = true;
                0
            } else {
                return Err(PipelineError::runtime(format!(
                    "{}: base64 decode: invalid input",
                    self.identifier
                )));
            };
            output |= u32::from(value) << (18 - 6 * index);
        }

        let decoded = [(output >> 16) as u8, (output >> 8) as u8, output as u8];
        self.next.write(&decoded[..3 - padding])
    }

    fn flush_encode(&mut self) -> PipelineResult<()> {
        let input = (u32::from(self.buffer[0]) << 16)
            | (u32::from(self.buffer[1]) << 8)
            | u32::from(self.buffer[2]);
        let mut encoded = [
            encode_value((input >> 18) as u8),
            encode_value(((input >> 12) & 0x3f) as u8),
            encode_value(((input >> 6) & 0x3f) as u8),
            encode_value((input & 0x3f) as u8),
        ];
        for index in 0..3 - self.position {
            encoded[3 - index] = b'=';
        }
        self.next.write(&encoded)
    }
}

impl Pipeline for PlBase64<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        if self.finished {
            return Err(PipelineError::logic("Pl_Base64 used after finished"));
        }

        match self.action {
            Base64Action::Encode => {
                for &byte in data {
                    self.buffer[self.position] = byte;
                    self.position += 1;
                    if self.position == 3 {
                        self.flush_quantum()?;
                    }
                }
            }
            Base64Action::Decode => {
                for &byte in data {
                    if !is_qpdf_space(byte) {
                        self.buffer[self.position] = byte;
                        self.position += 1;
                        if self.position == 4 {
                            self.flush_quantum()?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        if self.position > 0 {
            if self.finished {
                // cov:ignore-start: a successful quantum clears position before finished is set
                return Err(PipelineError::logic("Pl_Base64 used after finished"));
                // cov:ignore-end
            }
            if self.action == Base64Action::Decode {
                self.buffer[self.position..].fill(b'=');
            }
            self.flush_quantum()?;
        }
        self.finished = true;
        self.next.finish()
    }
}

fn is_qpdf_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn decode_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

fn encode_value(value: u8) -> u8 {
    match value {
        0..=25 => b'A' + value,
        26..=51 => b'a' + value - 26,
        52..=61 => b'0' + value - 52,
        62 => b'+',
        63 => b'/',
        _ => unreachable!("six-bit value"), // cov:ignore: all callers mask or shift to six bits
    }
}

#[cfg(test)]
mod tests {
    use super::{Base64Action, PlBase64};
    use crate::pipeline::test_support::{shared_trace, RecordingSink, TraceCall};
    use crate::pipeline::{Pipeline, PipelineError, PipelineResult};

    fn run(action: Base64Action, chunks: &[&[u8]]) -> PipelineResult<Vec<u8>> {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
        {
            let mut stage = PlBase64::new("base64", &mut sink, action);
            for chunk in chunks {
                stage.write(chunk)?;
            }
            stage.finish()?;
        }
        let output = trace.borrow().output.clone();
        Ok(output)
    }

    #[test]
    fn encode_cases() {
        let cases: &[(&[&[u8]], &[u8])] = &[
            (&[b""], b""),
            (&[b"\x00"], b"AA=="),
            (&[b"\x00\xff"], b"AP8="),
            (&[b"\x00\xff\x10"], b"AP8Q"),
            (&[b"\x00", b"\xff", b"\x10\x20"], b"AP8QIA=="),
            (&[b"Man"], b"TWFu"),
        ];

        for &(chunks, expected) in cases {
            assert_eq!(run(Base64Action::Encode, chunks).unwrap(), expected);
        }
    }

    #[test]
    fn decode_cases() {
        let cases: &[(&[&[u8]], &[u8])] = &[
            (&[b"TWFu"], b"Man"),
            (&[b"T", b"W", b"Fu"], b"Man"),
            (&[b" TQ==\r\n"], b"M"),
            (&[b"-\n_8="], b"\xfb\xff"),
            (&[b"TQ"], b"M"),
        ];

        for &(chunks, expected) in cases {
            assert_eq!(run(Base64Action::Decode, chunks).unwrap(), expected);
        }
    }

    #[test]
    fn decode_rejects_invalid_input_with_exact_identifier() {
        let error = run(Base64Action::Decode, &[b"@@@@" as &[u8]]).unwrap_err();

        assert!(matches!(error, PipelineError::Runtime(_)));
        assert_eq!(error.message(), "base64: base64 decode: invalid input");
    }

    #[test]
    fn decode_rejects_data_after_padding_after_preserving_prior_output() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
        let error = {
            let mut stage = PlBase64::new("decoder", &mut sink, Base64Action::Decode);
            stage.write(b"TQ==AAAA").unwrap_err()
        };

        assert!(matches!(error, PipelineError::Runtime(_)));
        assert_eq!(
            error.message(),
            "decoder: base64 decode: data follows pad characters"
        );
        assert_eq!(trace.borrow().output, b"M");
    }

    #[test]
    fn decode_allows_whitespace_after_padding() {
        assert_eq!(
            run(Base64Action::Decode, &[b"TQ== \t\r\n\x0b\x0c"]).unwrap(),
            b"M"
        );
    }

    #[test]
    fn decode_finish_rejects_a_single_symbol_quantum() {
        let error = run(Base64Action::Decode, &[b"T" as &[u8]]).unwrap_err();

        assert!(matches!(error, PipelineError::Runtime(_)));
        assert_eq!(error.message(), "base64: base64 decode: invalid input");
    }

    #[test]
    fn write_after_finish_is_logic_error() {
        let mut sink = RecordingSink::new(&[], &[]);
        let mut stage = PlBase64::new("base64", &mut sink, Base64Action::Encode);
        stage.finish().unwrap();

        let error = stage.write(b"").unwrap_err();
        assert!(matches!(error, PipelineError::Logic(_)));
        assert_eq!(error.message(), "Pl_Base64 used after finished");
    }

    #[test]
    fn repeated_finish_with_no_pending_data_finishes_downstream_each_time() {
        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        {
            let mut stage = PlBase64::new("base64", &mut sink, Base64Action::Encode);
            stage.finish().unwrap();
            stage.finish().unwrap();
        }

        assert_eq!(
            trace.borrow().calls,
            [
                TraceCall::Finish { failed: false },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn finish_write_failure_retains_quantum_for_a_retry() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[1], &[]);
        {
            let mut stage = PlBase64::new("base64", &mut sink, Base64Action::Encode);
            stage.write(b"M").unwrap();
            assert_eq!(
                stage.finish().unwrap_err().message(),
                "sink write failure 1"
            );
            stage.finish().unwrap();
        }

        assert_eq!(
            trace.borrow().calls,
            [
                TraceCall::Write {
                    data: b"TQ==".to_vec(),
                    failed: true,
                },
                TraceCall::Write {
                    data: b"TQ==".to_vec(),
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
        assert_eq!(trace.borrow().output, b"TQ==");
    }

    #[test]
    fn finish_failure_from_downstream_leaves_stage_finished() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[1]);
        {
            let mut stage = PlBase64::new("base64", &mut sink, Base64Action::Encode);
            assert_eq!(
                stage.finish().unwrap_err().message(),
                "sink finish failure 1"
            );
            assert_eq!(
                stage.write(b"").unwrap_err().message(),
                "Pl_Base64 used after finished"
            );
            stage.finish().unwrap();
        }

        assert_eq!(
            trace.borrow().calls,
            [
                TraceCall::Finish { failed: true },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn split_writes_match_single_write_at_every_boundary() {
        let encode_input = b"\x00\xff\x10\x20Man";
        let expected_encoded = run(Base64Action::Encode, &[encode_input]).unwrap();
        for boundary in 0..=encode_input.len() {
            assert_eq!(
                run(
                    Base64Action::Encode,
                    &[&encode_input[..boundary], &encode_input[boundary..]]
                )
                .unwrap(),
                expected_encoded
            );
        }

        let decode_input = b"AP8QIE1hbg==";
        let expected_decoded = run(Base64Action::Decode, &[decode_input]).unwrap();
        for boundary in 0..=decode_input.len() {
            assert_eq!(
                run(
                    Base64Action::Decode,
                    &[&decode_input[..boundary], &decode_input[boundary..]]
                )
                .unwrap(),
                expected_decoded
            );
        }
    }

    #[test]
    fn empty_write_does_not_change_state() {
        assert_eq!(
            run(Base64Action::Encode, &[b"M", b"", b"an"]).unwrap(),
            b"TWFu"
        );
        assert_eq!(
            run(Base64Action::Decode, &[b"T", b"", b"WFu"]).unwrap(),
            b"Man"
        );
    }
}

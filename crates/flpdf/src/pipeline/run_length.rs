//! qpdf correspondence: Pl_RunLength.cc incremental encode and decode state, output, error, and finish semantics.

use super::{Pipeline, PipelineError, PipelineResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunLengthAction {
    Encode,
    Decode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Top,
    Copying,
    Run,
}

pub(crate) struct RunLength<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    action: RunLengthAction,
    state: State,
    length: usize,
    buf: [u8; 128],
}

impl<'a> RunLength<'a> {
    pub(crate) fn new(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        action: RunLengthAction,
    ) -> Self {
        Self {
            identifier: identifier.into(),
            next,
            action,
            state: State::Top,
            length: 0,
            buf: [0; 128],
        }
    }

    fn encode(&mut self, data: &[u8]) -> PipelineResult<()> {
        for &byte in data {
            if matches!(self.state, State::Top) != (self.length <= 1) {
                return Err(PipelineError::logic(
                    "Pl_RunLength::encode: state/length inconsistency",
                ));
            }

            if self.length > 0
                && (matches!(self.state, State::Copying) || self.length < 128)
                && byte == self.buf[self.length - 1]
            {
                if matches!(self.state, State::Copying) {
                    self.length -= 1;
                    self.flush_encode()?;
                    self.buf[0] = byte;
                    self.length = 1;
                }
                self.state = State::Run;
                self.buf[self.length] = byte;
                self.length += 1;
            } else {
                if self.length == 128 || matches!(self.state, State::Run) {
                    self.flush_encode()?;
                } else if self.length > 0 {
                    self.state = State::Copying;
                }
                self.buf[self.length] = byte;
                self.length += 1;
            }
        }
        Ok(())
    }

    fn decode(&mut self, data: &[u8]) -> PipelineResult<()> {
        for &byte in data {
            match self.state {
                State::Top => {
                    if byte < 128 {
                        self.length = usize::from(byte) + 1;
                        self.state = State::Copying;
                    } else if byte > 128 {
                        self.length = 257 - usize::from(byte);
                        self.state = State::Run;
                    }
                }
                State::Copying => {
                    self.next.write(&[byte])?;
                    self.length -= 1;
                    if self.length == 0 {
                        self.state = State::Top;
                    }
                }
                State::Run => {
                    for _ in 0..self.length {
                        self.next.write(&[byte])?;
                    }
                    self.state = State::Top;
                }
            }
        }
        Ok(())
    }

    fn flush_encode(&mut self) -> PipelineResult<()> {
        if matches!(self.state, State::Run) {
            if !(2..=128).contains(&self.length) {
                return Err(PipelineError::logic(
                    "Pl_RunLength: invalid length in flush_encode for run",
                ));
            }
            let header = [(257 - self.length) as u8];
            self.next.write(&header)?;
            self.next.write(&self.buf[..1])?;
        } else if self.length > 0 {
            let header = [(self.length - 1) as u8];
            self.next.write(&header)?;
            self.next.write(&self.buf[..self.length])?;
        }
        self.state = State::Top;
        self.length = 0;
        Ok(())
    }
}

impl Pipeline for RunLength<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        match self.action {
            RunLengthAction::Encode => self.encode(data),
            RunLengthAction::Decode => self.decode(data),
        }
    }

    fn finish(&mut self) -> PipelineResult<()> {
        match self.action {
            RunLengthAction::Encode => {
                self.flush_encode()?;
                self.next.write(&[128])?;
                self.next.finish()
            }
            RunLengthAction::Decode => self.next.finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RunLength, RunLengthAction, State};
    use crate::pipeline::test_support::{RecordingSink, Trace, TraceCall};
    use crate::pipeline::{Pipeline, PipelineError};

    fn decode_trace(
        operation: impl FnOnce(&mut RunLength<'_>),
        fail_writes: &[usize],
        fail_finishes: &[usize],
    ) -> Trace {
        let mut sink = RecordingSink::new(fail_writes, fail_finishes);
        let trace = sink.trace();
        {
            let mut decoder =
                RunLength::new("runlength decode", &mut sink, RunLengthAction::Decode);
            operation(&mut decoder);
        }
        let observed = trace.borrow().clone();
        observed
    }

    #[test]
    fn decode_packets_preserve_output_and_one_byte_downstream_writes_when_chunked() {
        let cases = [
            (vec![0x80], vec![]),
            (vec![0x02, b'A', b'B', b'C', 0x80], b"ABC".to_vec()),
            (vec![0xfe, b'A', 0x80], b"AAA".to_vec()),
            (
                vec![0x7f].into_iter().chain(0_u8..128).collect(),
                (0_u8..128).collect(),
            ),
            (vec![0x81, 0xab], vec![0xab; 128]),
            (vec![0x05, b'A', b'B', b'C'], b"ABC".to_vec()),
            (vec![0xfd], vec![]),
            (vec![0x80, 0x00, b'Z'], b"Z".to_vec()),
        ];

        for (input, expected) in cases {
            let mut expected_calls: Vec<_> = expected
                .iter()
                .map(|&byte| TraceCall::Write {
                    data: vec![byte],
                    failed: false,
                })
                .collect();
            expected_calls.push(TraceCall::Finish { failed: false });

            let unsplit = decode_trace(
                |decoder| {
                    decoder.write(&input).unwrap();
                    decoder.finish().unwrap();
                },
                &[],
                &[],
            );
            assert_eq!(unsplit.output, expected, "unsplit input: {input:?}");
            assert_eq!(unsplit.calls, expected_calls, "unsplit input: {input:?}");

            let bytewise = decode_trace(
                |decoder| {
                    for byte in &input {
                        decoder.write(std::slice::from_ref(byte)).unwrap();
                    }
                    decoder.finish().unwrap();
                },
                &[],
                &[],
            );
            assert_eq!(bytewise.output, expected, "bytewise input: {input:?}");
            assert_eq!(bytewise.calls, expected_calls, "bytewise input: {input:?}");
        }
    }

    #[test]
    fn decode_finish_does_not_discard_a_partial_literal_packet() {
        let trace = decode_trace(
            |decoder| {
                decoder.write(&[0x02, b'A']).unwrap();
                decoder.finish().unwrap();
                decoder.write(b"BC").unwrap();
            },
            &[],
            &[],
        );

        assert_eq!(trace.output, b"ABC");
        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: vec![b'A'],
                    failed: false,
                },
                TraceCall::Finish { failed: false },
                TraceCall::Write {
                    data: vec![b'B'],
                    failed: false,
                },
                TraceCall::Write {
                    data: vec![b'C'],
                    failed: false,
                },
            ]
        );
    }

    #[test]
    fn decode_finish_does_not_discard_a_pending_run() {
        let trace = decode_trace(
            |decoder| {
                decoder.write(&[0xfd]).unwrap();
                decoder.finish().unwrap();
                decoder.write(b"Z").unwrap();
            },
            &[],
            &[],
        );

        assert_eq!(trace.output, b"ZZZZ");
        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Finish { failed: false },
                TraceCall::Write {
                    data: vec![b'Z'],
                    failed: false,
                },
                TraceCall::Write {
                    data: vec![b'Z'],
                    failed: false,
                },
                TraceCall::Write {
                    data: vec![b'Z'],
                    failed: false,
                },
                TraceCall::Write {
                    data: vec![b'Z'],
                    failed: false,
                },
            ]
        );
    }

    #[test]
    fn decode_failed_literal_write_does_not_decrement_remaining_length() {
        let trace = decode_trace(
            |decoder| {
                assert_eq!(
                    decoder.write(&[0x02, b'A']).unwrap_err().to_string(),
                    "sink write failure 1"
                );
                decoder.write(b"BCD").unwrap();
                decoder.finish().unwrap();
            },
            &[1],
            &[],
        );

        assert_eq!(trace.output, b"BCD");
        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: vec![b'A'],
                    failed: true,
                },
                TraceCall::Write {
                    data: vec![b'B'],
                    failed: false,
                },
                TraceCall::Write {
                    data: vec![b'C'],
                    failed: false,
                },
                TraceCall::Write {
                    data: vec![b'D'],
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn decode_failed_run_write_retries_the_full_run_on_a_later_input_byte() {
        let trace = decode_trace(
            |decoder| {
                decoder.write(&[0xfd]).unwrap();
                assert_eq!(
                    decoder.write(b"Z").unwrap_err().to_string(),
                    "sink write failure 2"
                );
                decoder.write(b"Y").unwrap();
                decoder.finish().unwrap();
            },
            &[2],
            &[],
        );

        assert_eq!(trace.output, b"ZYYYY");
        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: vec![b'Z'],
                    failed: false,
                },
                TraceCall::Write {
                    data: vec![b'Z'],
                    failed: true,
                },
                TraceCall::Write {
                    data: vec![b'Y'],
                    failed: false,
                },
                TraceCall::Write {
                    data: vec![b'Y'],
                    failed: false,
                },
                TraceCall::Write {
                    data: vec![b'Y'],
                    failed: false,
                },
                TraceCall::Write {
                    data: vec![b'Y'],
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn decode_repeated_finish_attempts_are_forwarded() {
        let trace = decode_trace(
            |decoder| {
                assert_eq!(
                    decoder.finish().unwrap_err().to_string(),
                    "sink finish failure 1"
                );
                decoder.finish().unwrap();
            },
            &[],
            &[1],
        );

        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Finish { failed: true },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn decode_identifier_exposes_the_constructor_name() {
        let mut sink = RecordingSink::new(&[], &[]);
        let decoder = RunLength::new("custom runlength", &mut sink, RunLengthAction::Decode);

        assert_eq!(decoder.identifier(), "custom runlength");
    }

    #[test]
    fn decode_phase_state_mutation_reaches_qpdf_encode_logic_error_branches() {
        let mut sink = RecordingSink::new(&[], &[]);
        {
            let mut encoder =
                RunLength::new("runlength encode", &mut sink, RunLengthAction::Encode);
            encoder.state = State::Top;
            encoder.length = 2;
            let error = encoder.write(b"X").unwrap_err();
            assert!(matches!(error, PipelineError::Logic(_)));
            assert_eq!(
                error.message_bytes(),
                b"Pl_RunLength::encode: state/length inconsistency"
            );
        }

        for length in [1, 129] {
            let mut encoder =
                RunLength::new("runlength encode", &mut sink, RunLengthAction::Encode);
            encoder.state = State::Run;
            encoder.length = length;
            let error = encoder.finish().unwrap_err();
            assert!(matches!(error, PipelineError::Logic(_)));
            assert_eq!(
                error.message_bytes(),
                b"Pl_RunLength: invalid length in flush_encode for run"
            );
        }
    }

    fn encode_trace(
        operation: impl FnOnce(&mut RunLength<'_>),
        fail_writes: &[usize],
        fail_finishes: &[usize],
    ) -> Trace {
        let mut sink = RecordingSink::new(fail_writes, fail_finishes);
        let trace = sink.trace();
        {
            let mut encoder =
                RunLength::new("runlength encode", &mut sink, RunLengthAction::Encode);
            operation(&mut encoder);
        }
        let observed = trace.borrow().clone();
        observed
    }

    fn encode_once(input: &[u8]) -> Trace {
        encode_trace(
            |encoder| {
                encoder.write(input).unwrap();
                encoder.finish().unwrap();
            },
            &[],
            &[],
        )
    }

    fn successful_write(data: impl Into<Vec<u8>>) -> TraceCall {
        TraceCall::Write {
            data: data.into(),
            failed: false,
        }
    }

    fn failed_write(data: impl Into<Vec<u8>>) -> TraceCall {
        TraceCall::Write {
            data: data.into(),
            failed: true,
        }
    }

    #[test]
    fn encode_qpdf_exact_small_packets() {
        let cases = [
            (b"".as_slice(), vec![0x80]),
            (b"A".as_slice(), vec![0x00, b'A', 0x80]),
            (b"AA".as_slice(), vec![0xff, b'A', 0x80]),
            (b"AB".as_slice(), vec![0x01, b'A', b'B', 0x80]),
            (b"ABCC".as_slice(), vec![0x01, b'A', b'B', 0xff, b'C', 0x80]),
        ];

        for (input, expected) in cases {
            assert_eq!(encode_once(input).output, expected, "input: {input:?}");
        }
    }

    #[test]
    fn encode_distinct_literal_boundaries_use_qpdf_packet_sizes() {
        for length in [127_usize, 128, 129] {
            let input: Vec<_> = (0..length).map(|value| value as u8).collect();
            let mut expected = match length {
                127 => {
                    let mut bytes = vec![0x7e];
                    bytes.extend(0_u8..127);
                    bytes
                }
                128 => {
                    let mut bytes = vec![0x7f];
                    bytes.extend(0_u8..128);
                    bytes
                }
                other => {
                    assert_eq!(other, 129);
                    let mut bytes = vec![0x7f];
                    bytes.extend(0_u8..128);
                    bytes.extend([0x00, 0x80]);
                    bytes
                }
            };
            expected.push(0x80);

            assert_eq!(encode_once(&input).output, expected, "length: {length}");
        }
    }

    #[test]
    fn encode_equal_run_boundaries_use_qpdf_packet_sizes() {
        for (length, expected) in [
            (127, vec![0x82, b'R', 0x80]),
            (128, vec![0x81, b'R', 0x80]),
            (129, vec![0x81, b'R', 0x00, b'R', 0x80]),
        ] {
            assert_eq!(
                encode_once(&vec![b'R'; length]).output,
                expected,
                "length: {length}"
            );
        }
    }

    #[test]
    fn encode_literal_to_run_transitions_match_qpdf_at_boundary_positions() {
        for pair_start in [2_usize, 127, 128, 129] {
            let mut input: Vec<_> = (0..pair_start).map(|value| value as u8).collect();
            input.push((pair_start - 1) as u8);

            let mut expected = match pair_start {
                2 => vec![0x00, 0x00, 0xff, 0x01],
                127 => {
                    let mut bytes = vec![0x7d];
                    bytes.extend(0_u8..126);
                    bytes.extend([0xff, 126]);
                    bytes
                }
                128 => {
                    let mut bytes = vec![0x7e];
                    bytes.extend(0_u8..127);
                    bytes.extend([0xff, 127]);
                    bytes
                }
                other => {
                    assert_eq!(other, 129);
                    let mut bytes = vec![0x7f];
                    bytes.extend(0_u8..128);
                    bytes.extend([0xff, 128]);
                    bytes
                }
            };
            expected.push(0x80);

            assert_eq!(
                encode_once(&input).output,
                expected,
                "equal pair starts at one-based position {pair_start}"
            );
        }
    }

    #[test]
    fn encode_every_split_of_a_mixed_fixture_matches_one_write() {
        let mut input: Vec<_> = (0_u8..100).collect();
        input.extend([0xaa; 20]);
        input.extend(100_u8..180);
        input.extend([0x55; 60]);
        assert_eq!(input.len(), 260);

        let unsplit = encode_once(&input);
        for split in 0..=input.len() {
            let split_trace = encode_trace(
                |encoder| {
                    encoder.write(&input[..split]).unwrap();
                    encoder.write(&input[split..]).unwrap();
                    encoder.finish().unwrap();
                },
                &[],
                &[],
            );
            assert_eq!(split_trace, unsplit, "split: {split}");
        }
    }

    #[test]
    fn encode_after_finish_starts_an_independently_terminated_sequence() {
        let trace = encode_trace(
            |encoder| {
                encoder.write(b"A").unwrap();
                encoder.finish().unwrap();
                encoder.write(b"BB").unwrap();
                encoder.finish().unwrap();
            },
            &[],
            &[],
        );

        assert_eq!(trace.output, [0x00, b'A', 0x80, 0xff, b'B', 0x80]);
        assert_eq!(
            trace.calls,
            vec![
                successful_write([0x00]),
                successful_write(b"A"),
                successful_write([0x80]),
                TraceCall::Finish { failed: false },
                successful_write([0xff]),
                successful_write(b"B"),
                successful_write([0x80]),
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn encode_abcc_preserves_qpdf_downstream_call_granularity() {
        let trace = encode_once(b"ABCC");

        assert_eq!(
            trace.calls,
            vec![
                successful_write([0x01]),
                successful_write(b"AB"),
                successful_write([0xff]),
                successful_write(b"C"),
                successful_write([0x80]),
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn encode_literal_flush_failures_stop_the_call_and_preserve_unreset_state() {
        for (failure, expected_calls) in [
            (
                1,
                vec![
                    failed_write([0x01]),
                    successful_write([0x02]),
                    successful_write(b"ABC"),
                    successful_write([0x80]),
                    TraceCall::Finish { failed: false },
                ],
            ),
            (
                2,
                vec![
                    successful_write([0x01]),
                    failed_write(b"AB"),
                    successful_write([0x02]),
                    successful_write(b"ABC"),
                    successful_write([0x80]),
                    TraceCall::Finish { failed: false },
                ],
            ),
        ] {
            let mut sink = RecordingSink::new(&[failure], &[]);
            let trace = sink.trace();
            {
                let mut encoder =
                    RunLength::new("runlength encode", &mut sink, RunLengthAction::Encode);
                assert_eq!(
                    encoder.write(b"ABCC").unwrap_err().to_string(),
                    format!("sink write failure {failure}")
                );
                assert_eq!(
                    trace.borrow().calls.len(),
                    failure,
                    "no later write after failure {failure}"
                );
                encoder.write(b"C").unwrap();
                encoder.finish().unwrap();
            }

            assert_eq!(trace.borrow().calls, expected_calls, "failure: {failure}");
        }
    }

    #[test]
    fn encode_finish_write_failures_stop_the_call_and_follow_qpdf_retry_order() {
        for (failure, expected_calls) in [
            (
                3,
                vec![
                    successful_write([0x01]),
                    successful_write(b"AB"),
                    failed_write([0xff]),
                    successful_write([0xff]),
                    successful_write(b"C"),
                    successful_write([0x80]),
                    TraceCall::Finish { failed: false },
                ],
            ),
            (
                4,
                vec![
                    successful_write([0x01]),
                    successful_write(b"AB"),
                    successful_write([0xff]),
                    failed_write(b"C"),
                    successful_write([0xff]),
                    successful_write(b"C"),
                    successful_write([0x80]),
                    TraceCall::Finish { failed: false },
                ],
            ),
            (
                5,
                vec![
                    successful_write([0x01]),
                    successful_write(b"AB"),
                    successful_write([0xff]),
                    successful_write(b"C"),
                    failed_write([0x80]),
                    successful_write([0x80]),
                    TraceCall::Finish { failed: false },
                ],
            ),
        ] {
            let mut sink = RecordingSink::new(&[failure], &[]);
            let trace = sink.trace();
            {
                let mut encoder =
                    RunLength::new("runlength encode", &mut sink, RunLengthAction::Encode);
                encoder.write(b"ABCC").unwrap();
                assert_eq!(
                    encoder.finish().unwrap_err().to_string(),
                    format!("sink write failure {failure}")
                );
                assert_eq!(
                    trace.borrow().calls.len(),
                    failure,
                    "no later call after failure {failure}"
                );
                encoder.finish().unwrap();
            }

            assert_eq!(trace.borrow().calls, expected_calls, "failure: {failure}");
        }
    }

    #[test]
    fn encode_downstream_finish_failure_is_retryable_after_the_eod_write() {
        let mut sink = RecordingSink::new(&[], &[1]);
        let trace = sink.trace();
        {
            let mut encoder =
                RunLength::new("runlength encode", &mut sink, RunLengthAction::Encode);
            encoder.write(b"ABCC").unwrap();
            assert_eq!(
                encoder.finish().unwrap_err().to_string(),
                "sink finish failure 1"
            );
            assert_eq!(
                trace.borrow().calls,
                vec![
                    successful_write([0x01]),
                    successful_write(b"AB"),
                    successful_write([0xff]),
                    successful_write(b"C"),
                    successful_write([0x80]),
                    TraceCall::Finish { failed: true },
                ]
            );
            encoder.finish().unwrap();
        }

        assert_eq!(
            trace.borrow().calls,
            vec![
                successful_write([0x01]),
                successful_write(b"AB"),
                successful_write([0xff]),
                successful_write(b"C"),
                successful_write([0x80]),
                TraceCall::Finish { failed: true },
                successful_write([0x80]),
                TraceCall::Finish { failed: false },
            ]
        );
    }
}

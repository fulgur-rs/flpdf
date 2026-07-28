//! qpdf correspondence: Pl_PNGFilter.cc row geometry, buffer rotation, per-filter decoding, hard-coded Up encoding, partial-row finish, and constructor validation.

use super::{Pipeline, PipelineError, PipelineResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PngFilterAction {
    Encode,
    Decode,
}

/// Incremental PNG predictor stage for `/Predictor 10` through `/Predictor 15`.
///
/// The predictor number itself never reaches the component. qpdf selects among
/// the five PNG row filters by reading each row's leading filter byte when
/// decoding, and always emits the Up filter when encoding.
pub(crate) struct PngFilter<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    action: PngFilterAction,
    bytes_per_row: usize,
    bytes_per_pixel: usize,
    row_capacity: usize,
    buf1: Vec<u8>,
    buf2: Vec<u8>,
    cur_is_first: bool,
    has_prev: bool,
    pos: usize,
    incoming: usize,
}

impl<'a> PngFilter<'a> {
    /// Construct the stage for a row geometry.
    ///
    /// The row width is computed with 32-bit wrapping arithmetic because qpdf
    /// evaluates `((columns * bits_per_sample * samples_per_pixel) + 7) / 8` in
    /// `unsigned int` before widening the result. A geometry large enough to
    /// wrap that product therefore reaches the zero-width rejection, and qpdf's
    /// additional `> UINT_MAX - 1` guard is unreachable.
    pub(crate) fn new(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        action: PngFilterAction,
        columns: u32,
        samples_per_pixel: u32,
        bits_per_sample: u32,
    ) -> PipelineResult<Self> {
        if samples_per_pixel == 0 {
            return Err(PipelineError::runtime(
                "PNGFilter created with invalid samples_per_pixel",
            ));
        }
        if !matches!(bits_per_sample, 1 | 2 | 4 | 8 | 16) {
            return Err(PipelineError::runtime(
                "PNGFilter created with invalid bits_per_sample not 1, 2, 4, 8, or 16",
            ));
        }
        let bytes_per_pixel = bits_per_sample
            .wrapping_mul(samples_per_pixel)
            .wrapping_add(7)
            / 8;
        let bytes_per_row = columns
            .wrapping_mul(bits_per_sample)
            .wrapping_mul(samples_per_pixel)
            .wrapping_add(7)
            / 8;
        if bytes_per_row == 0 {
            return Err(PipelineError::runtime(
                "PNGFilter created with invalid columns value",
            ));
        }

        let bytes_per_row = bytes_per_row as usize;
        let row_capacity = bytes_per_row + 1;
        let incoming = match action {
            PngFilterAction::Encode => bytes_per_row,
            PngFilterAction::Decode => bytes_per_row + 1,
        };

        Ok(Self {
            identifier: identifier.into(),
            next,
            action,
            bytes_per_row,
            bytes_per_pixel: bytes_per_pixel as usize,
            row_capacity,
            // qpdf allocates both row buffers in its constructor. flpdf defers
            // that to the first byte written, which cannot change output bytes,
            // downstream call boundaries, or error timing because an unused
            // stage never reads a row. It keeps a stream that carries no data
            // from allocating two buffers sized by an untrusted `/Columns`.
            buf1: Vec::new(),
            buf2: Vec::new(),
            cur_is_first: true,
            has_prev: true,
            pos: 0,
            incoming,
        })
    }

    fn ensure_row_buffers(&mut self) {
        if self.buf1.is_empty() {
            self.buf1 = vec![0; self.row_capacity];
            self.buf2 = vec![0; self.row_capacity];
        }
    }

    /// Borrow the current row for mutation together with the previous row.
    fn split_rows(&mut self) -> (&mut [u8], Option<&[u8]>) {
        let has_prev = self.has_prev;
        if self.cur_is_first {
            let previous = self.buf2.as_slice();
            (self.buf1.as_mut_slice(), has_prev.then_some(previous))
        } else {
            let previous = self.buf1.as_slice();
            (self.buf2.as_mut_slice(), has_prev.then_some(previous))
        }
    }

    fn process_row(&mut self) -> PipelineResult<()> {
        match self.action {
            PngFilterAction::Encode => self.encode_row(),
            PngFilterAction::Decode => self.decode_row(),
        }
    }

    fn decode_row(&mut self) -> PipelineResult<()> {
        let bytes_per_row = self.bytes_per_row;
        let bytes_per_pixel = self.bytes_per_pixel;
        {
            let (current, previous) = self.split_rows();
            if let Some(previous) = previous {
                let filter = current[0];
                let buffer = &mut current[1..=bytes_per_row];
                let above = &previous[1..=bytes_per_row];
                match filter {
                    0 => {}
                    1 => decode_sub(buffer, bytes_per_pixel),
                    2 => decode_up(buffer, above),
                    3 => decode_average(buffer, above, bytes_per_pixel),
                    4 => decode_paeth(buffer, above, bytes_per_pixel),
                    // qpdf ignores an unrecognized filter byte and emits the row
                    // exactly as it arrived.
                    _ => {}
                }
            }
        }
        let current = if self.cur_is_first {
            &self.buf1
        } else {
            &self.buf2
        };
        self.next.write(&current[1..=bytes_per_row])
    }

    fn encode_row(&mut self) -> PipelineResult<()> {
        self.next.write(&[2])?;
        let bytes_per_row = self.bytes_per_row;
        if self.has_prev {
            for index in 0..bytes_per_row {
                let (current, previous) = if self.cur_is_first {
                    (&self.buf1, &self.buf2)
                } else {
                    (&self.buf2, &self.buf1)
                };
                let byte = current[index].wrapping_sub(previous[index]);
                self.next.write(&[byte])?;
            }
        } else {
            let current = if self.cur_is_first {
                &self.buf1
            } else {
                &self.buf2
            };
            self.next.write(&current[..bytes_per_row])?;
        }
        Ok(())
    }

    fn copy_into_current(&mut self, data: &[u8]) {
        let pos = self.pos;
        let current = if self.cur_is_first {
            &mut self.buf1
        } else {
            &mut self.buf2
        };
        current[pos..pos + data.len()].copy_from_slice(data);
    }
}

fn decode_sub(buffer: &mut [u8], bytes_per_pixel: usize) {
    for index in 0..buffer.len() {
        let left = if index >= bytes_per_pixel {
            buffer[index - bytes_per_pixel]
        } else {
            0
        };
        buffer[index] = buffer[index].wrapping_add(left);
    }
}

fn decode_up(buffer: &mut [u8], above: &[u8]) {
    for (byte, up) in buffer.iter_mut().zip(above) {
        *byte = byte.wrapping_add(*up);
    }
}

fn decode_average(buffer: &mut [u8], above: &[u8], bytes_per_pixel: usize) {
    for index in 0..buffer.len() {
        let left = if index >= bytes_per_pixel {
            i32::from(buffer[index - bytes_per_pixel])
        } else {
            0
        };
        let up = i32::from(above[index]);
        buffer[index] = buffer[index].wrapping_add(((left + up) / 2) as u8);
    }
}

fn decode_paeth(buffer: &mut [u8], above: &[u8], bytes_per_pixel: usize) {
    for index in 0..buffer.len() {
        let up = i32::from(above[index]);
        let (left, upper_left) = if index >= bytes_per_pixel {
            (
                i32::from(buffer[index - bytes_per_pixel]),
                i32::from(above[index - bytes_per_pixel]),
            )
        } else {
            (0, 0)
        };
        buffer[index] = buffer[index].wrapping_add(paeth_predictor(left, up, upper_left) as u8);
    }
}

fn abs_diff(a: i32, b: i32) -> i32 {
    if a > b {
        a - b
    } else {
        b - a
    }
}

fn paeth_predictor(a: i32, b: i32, c: i32) -> i32 {
    let p = a + b - c;
    let pa = abs_diff(p, a);
    let pb = abs_diff(p, b);
    let pc = abs_diff(p, c);

    if pa <= pb && pa <= pc {
        return a;
    }
    if pb <= pc {
        return b;
    }
    c
}

impl Pipeline for PngFilter<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        if data.is_empty() {
            return Ok(());
        }
        self.ensure_row_buffers();

        let mut left = self.incoming - self.pos;
        let mut offset = 0;
        let mut len = data.len();
        while len >= left {
            self.copy_into_current(&data[offset..offset + left]);
            offset += left;
            len -= left;

            self.process_row()?;

            self.cur_is_first = !self.cur_is_first;
            self.has_prev = true;
            if self.cur_is_first {
                self.buf1.fill(0);
            } else {
                self.buf2.fill(0);
            }
            left = self.incoming;
            self.pos = 0;
        }
        if len > 0 {
            self.copy_into_current(&data[offset..offset + len]);
        }
        self.pos += len;
        Ok(())
    }

    /// Emit any buffered partial row, then reset and finish downstream.
    ///
    /// A partial row is padded with the zeroes the row buffer already holds and
    /// emitted at full width. The reset drops the previous row, so the first row
    /// written after `finish` is neither filtered on decode nor differenced on
    /// encode.
    fn finish(&mut self) -> PipelineResult<()> {
        if self.pos != 0 {
            self.process_row()?;
        }
        self.has_prev = false;
        self.cur_is_first = true;
        self.pos = 0;
        self.buf1.fill(0);

        self.next.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{PngFilter, PngFilterAction};
    use crate::pipeline::test_support::{RecordingSink, TraceCall};
    use crate::pipeline::{Pipeline, PipelineError};

    struct Geometry {
        columns: u32,
        colors: u32,
        bits: u32,
    }

    const BYTE_ROW: Geometry = Geometry {
        columns: 4,
        colors: 1,
        bits: 8,
    };

    fn run(
        action: PngFilterAction,
        geometry: &Geometry,
        chunks: &[&[u8]],
    ) -> (Vec<u8>, Vec<TraceCall>) {
        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        {
            let mut stage = PngFilter::new(
                "png",
                &mut sink,
                action,
                geometry.columns,
                geometry.colors,
                geometry.bits,
            )
            .expect("construction must succeed");
            for chunk in chunks {
                stage.write(chunk).expect("write must succeed");
            }
            stage.finish().expect("finish must succeed");
        }
        let trace = trace.borrow();
        (trace.output.clone(), trace.calls.clone())
    }

    fn construction_error(columns: u32, colors: u32, bits: u32) -> PipelineError {
        let mut sink = RecordingSink::new(&[], &[]);
        PngFilter::new(
            "png",
            &mut sink,
            PngFilterAction::Decode,
            columns,
            colors,
            bits,
        )
        .err()
        .expect("construction must fail")
    }

    #[test]
    fn identifier_is_retained() {
        let mut sink = RecordingSink::new(&[], &[]);
        let stage =
            PngFilter::new("png decode", &mut sink, PngFilterAction::Decode, 4, 1, 8).unwrap();
        assert_eq!(stage.identifier(), "png decode");
    }

    #[test]
    fn zero_samples_per_pixel_is_rejected() {
        assert_eq!(
            construction_error(4, 0, 8).to_string(),
            "PNGFilter created with invalid samples_per_pixel"
        );
    }

    #[test]
    fn unsupported_bits_per_sample_is_rejected() {
        assert_eq!(
            construction_error(4, 1, 3).to_string(),
            "PNGFilter created with invalid bits_per_sample not 1, 2, 4, 8, or 16"
        );
        for bits in [1, 2, 4, 8, 16] {
            let mut sink = RecordingSink::new(&[], &[]);
            PngFilter::new("png", &mut sink, PngFilterAction::Decode, 4, 1, bits)
                .expect("every legal bit depth is accepted");
        }
    }

    #[test]
    fn zero_row_width_is_rejected() {
        assert_eq!(
            construction_error(0, 1, 8).to_string(),
            "PNGFilter created with invalid columns value"
        );
    }

    #[test]
    fn row_width_that_wraps_32_bit_arithmetic_is_rejected() {
        // 2^29 columns of one 8-bit sample wraps the 32-bit product to zero.
        assert_eq!(
            construction_error(536_870_912, 1, 8).to_string(),
            "PNGFilter created with invalid columns value"
        );
    }

    #[test]
    fn construction_errors_are_runtime_exceptions() {
        assert!(matches!(
            construction_error(0, 1, 8),
            PipelineError::Runtime(_)
        ));
    }

    #[test]
    fn every_row_filter_decodes_against_the_previous_row() {
        let (output, _) = run(
            PngFilterAction::Decode,
            &BYTE_ROW,
            &[&[
                1, 0x01, 0x01, 0x01, 0x01, // Sub
                2, 0x01, 0x01, 0x01, 0x01, // Up
                3, 0x00, 0x00, 0x00, 0x00, // Average
                4, 0x00, 0x00, 0x00, 0x00, // Paeth
                0, 0x09, 0x09, 0x09, 0x09, // None
            ]],
        );
        assert_eq!(
            output,
            vec![
                0x01, 0x02, 0x03, 0x04, //
                0x02, 0x03, 0x04, 0x05, //
                0x01, 0x02, 0x03, 0x04, //
                0x01, 0x02, 0x03, 0x04, //
                0x09, 0x09, 0x09, 0x09,
            ]
        );
    }

    #[test]
    fn unknown_filter_byte_passes_the_row_through() {
        let (output, calls) = run(
            PngFilterAction::Decode,
            &BYTE_ROW,
            &[&[9, 0x01, 0x02, 0x03, 0x04]],
        );
        assert_eq!(output, vec![0x01, 0x02, 0x03, 0x04]);
        assert_eq!(
            calls,
            vec![
                TraceCall::Write {
                    data: vec![0x01, 0x02, 0x03, 0x04],
                    failed: false
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn first_row_is_filtered_against_a_zeroed_previous_row() {
        // Up against an all-zero previous row is the identity, so Sub is the
        // filter that proves the first row is filtered at all.
        let (output, _) = run(
            PngFilterAction::Decode,
            &BYTE_ROW,
            &[&[1, 0x01, 0x01, 0x01, 0x01]],
        );
        assert_eq!(output, vec![0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn multi_byte_pixels_use_the_pixel_stride() {
        let geometry = Geometry {
            columns: 2,
            colors: 3,
            bits: 8,
        };
        let (output, _) = run(
            PngFilterAction::Decode,
            &geometry,
            &[&[1, 0x01, 0x02, 0x03, 0x01, 0x02, 0x03]],
        );
        assert_eq!(output, vec![0x01, 0x02, 0x03, 0x02, 0x04, 0x06]);
    }

    #[test]
    fn sub_pixel_bit_depths_round_the_row_width_up() {
        let geometry = Geometry {
            columns: 3,
            colors: 1,
            bits: 1,
        };
        let (output, calls) = run(PngFilterAction::Decode, &geometry, &[&[0, 0xe0]]);
        assert_eq!(output, vec![0xe0]);
        assert_eq!(
            calls,
            vec![
                TraceCall::Write {
                    data: vec![0xe0],
                    failed: false
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn truncated_final_row_is_emitted_zero_padded_at_finish() {
        let (output, calls) = run(
            PngFilterAction::Decode,
            &BYTE_ROW,
            &[&[0, 0x01, 0x02, 0x03, 0x04, 0, 0xff]],
        );
        assert_eq!(output, vec![0x01, 0x02, 0x03, 0x04, 0xff, 0x00, 0x00, 0x00]);
        assert_eq!(
            calls,
            vec![
                TraceCall::Write {
                    data: vec![0x01, 0x02, 0x03, 0x04],
                    failed: false
                },
                TraceCall::Write {
                    data: vec![0xff, 0x00, 0x00, 0x00],
                    failed: false
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn first_row_after_finish_is_not_filtered() {
        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        {
            let mut stage =
                PngFilter::new("png", &mut sink, PngFilterAction::Decode, 4, 1, 8).unwrap();
            stage.write(&[1, 0x01, 0x01, 0x01, 0x01]).unwrap();
            stage.finish().unwrap();
            stage.write(&[1, 0x01, 0x01, 0x01, 0x01]).unwrap();
            stage.finish().unwrap();
        }
        let trace = trace.borrow();
        assert_eq!(
            trace.output,
            vec![0x01, 0x02, 0x03, 0x04, 0x01, 0x01, 0x01, 0x01]
        );
    }

    #[test]
    fn every_input_split_produces_the_same_decoded_output() {
        let data: &[u8] = &[
            1, 0x01, 0x01, 0x01, 0x01, 2, 0x01, 0x01, 0x01, 0x01, 4, 0x00, 0x00, 0x00, 0x00,
        ];
        let (whole, _) = run(PngFilterAction::Decode, &BYTE_ROW, &[data]);
        for split in 0..=data.len() {
            let (parts, _) = run(
                PngFilterAction::Decode,
                &BYTE_ROW,
                &[&data[..split], &data[split..]],
            );
            assert_eq!(parts, whole, "split at {split}");
        }
    }

    #[test]
    fn encoding_emits_the_up_filter_one_byte_at_a_time() {
        let (output, calls) = run(
            PngFilterAction::Encode,
            &BYTE_ROW,
            &[&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]],
        );
        assert_eq!(
            output,
            vec![2, 0x01, 0x02, 0x03, 0x04, 2, 0x04, 0x04, 0x04, 0x04]
        );
        let expected: Vec<TraceCall> = [
            vec![2],
            vec![0x01],
            vec![0x02],
            vec![0x03],
            vec![0x04],
            vec![2],
            vec![0x04],
            vec![0x04],
            vec![0x04],
            vec![0x04],
        ]
        .into_iter()
        .map(|data| TraceCall::Write {
            data,
            failed: false,
        })
        .chain(std::iter::once(TraceCall::Finish { failed: false }))
        .collect();
        assert_eq!(calls, expected);
    }

    #[test]
    fn encoding_pads_a_truncated_final_row() {
        let (output, _) = run(PngFilterAction::Encode, &BYTE_ROW, &[&[0x01, 0x02]]);
        assert_eq!(output, vec![2, 0x01, 0x02, 0x00, 0x00]);
    }

    #[test]
    fn first_row_after_finish_is_encoded_without_a_previous_row() {
        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        {
            let mut stage =
                PngFilter::new("png", &mut sink, PngFilterAction::Encode, 4, 1, 8).unwrap();
            stage.write(&[0x01, 0x02, 0x03, 0x04]).unwrap();
            stage.finish().unwrap();
            stage.write(&[0x05, 0x06, 0x07, 0x08]).unwrap();
            stage.finish().unwrap();
        }
        let trace = trace.borrow();
        assert_eq!(
            trace.output,
            vec![2, 0x01, 0x02, 0x03, 0x04, 2, 0x05, 0x06, 0x07, 0x08]
        );
        // The row after finish is written as a single chunk because there is no
        // previous row to difference against.
        assert_eq!(
            trace.calls[7],
            TraceCall::Write {
                data: vec![0x05, 0x06, 0x07, 0x08],
                failed: false
            }
        );
    }

    #[test]
    fn empty_input_emits_no_row() {
        let (output, calls) = run(PngFilterAction::Decode, &BYTE_ROW, &[&[]]);
        assert!(output.is_empty());
        assert_eq!(calls, vec![TraceCall::Finish { failed: false }]);
    }

    #[test]
    fn repeated_finish_calls_reach_downstream_each_time() {
        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        {
            let mut stage =
                PngFilter::new("png", &mut sink, PngFilterAction::Decode, 4, 1, 8).unwrap();
            stage.finish().unwrap();
            stage.finish().unwrap();
        }
        assert_eq!(
            trace.borrow().calls,
            vec![
                TraceCall::Finish { failed: false },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn downstream_write_failure_leaves_the_row_unconsumed() {
        let mut sink = RecordingSink::new(&[1], &[]);
        let trace = sink.trace();
        {
            let mut stage =
                PngFilter::new("png", &mut sink, PngFilterAction::Decode, 4, 1, 8).unwrap();
            let error = stage
                .write(&[2, 0x01, 0x02, 0x03, 0x04, 2, 0x01, 0x02, 0x03, 0x04])
                .expect_err("first row write fails");
            assert_eq!(error.to_string(), "sink write failure 1");
            // qpdf leaves `pos` at zero because the row-completion bookkeeping
            // runs after processRow, so finish emits nothing.
            stage.finish().unwrap();
        }
        let trace = trace.borrow();
        assert!(trace.output.is_empty());
        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: vec![0x01, 0x02, 0x03, 0x04],
                    failed: true
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn downstream_failure_on_the_encode_header_stops_the_row() {
        let mut sink = RecordingSink::new(&[1], &[]);
        let trace = sink.trace();
        {
            let mut stage =
                PngFilter::new("png", &mut sink, PngFilterAction::Encode, 4, 1, 8).unwrap();
            assert_eq!(
                stage
                    .write(&[0x01, 0x02, 0x03, 0x04])
                    .expect_err("header write fails")
                    .to_string(),
                "sink write failure 1"
            );
        }
        assert_eq!(
            trace.borrow().calls,
            vec![TraceCall::Write {
                data: vec![2],
                failed: true
            }]
        );
    }

    #[test]
    fn downstream_failure_inside_an_encoded_row_stops_at_that_byte() {
        let mut sink = RecordingSink::new(&[3], &[]);
        let trace = sink.trace();
        {
            let mut stage =
                PngFilter::new("png", &mut sink, PngFilterAction::Encode, 4, 1, 8).unwrap();
            assert_eq!(
                stage
                    .write(&[0x01, 0x02, 0x03, 0x04])
                    .expect_err("body write fails")
                    .to_string(),
                "sink write failure 3"
            );
        }
        assert_eq!(trace.borrow().calls.len(), 3);
    }

    #[test]
    fn downstream_failure_on_a_finish_row_suppresses_downstream_finish() {
        let mut sink = RecordingSink::new(&[1], &[]);
        let trace = sink.trace();
        {
            let mut stage =
                PngFilter::new("png", &mut sink, PngFilterAction::Decode, 4, 1, 8).unwrap();
            stage.write(&[0, 0x01]).unwrap();
            assert_eq!(
                stage
                    .finish()
                    .expect_err("partial row write fails")
                    .to_string(),
                "sink write failure 1"
            );
        }
        assert_eq!(
            trace.borrow().calls,
            vec![TraceCall::Write {
                data: vec![0x01, 0x00, 0x00, 0x00],
                failed: true
            }]
        );
    }

    #[test]
    fn downstream_finish_failure_propagates() {
        let mut sink = RecordingSink::new(&[], &[1]);
        let mut stage = PngFilter::new("png", &mut sink, PngFilterAction::Decode, 4, 1, 8).unwrap();
        assert_eq!(
            stage.finish().unwrap_err().to_string(),
            "sink finish failure 1"
        );
    }
}

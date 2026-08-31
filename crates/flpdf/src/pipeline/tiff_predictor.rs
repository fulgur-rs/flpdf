//! qpdf correspondence: `Pl_TIFFPredictor.cc` incremental TIFF predictor.
//! It covers horizontal differencing, row buffering, packed samples, and
//! finish-time padding.
//!
//! The checked geometry preflight follows qpdf head commit
//! `cf047b20721b18b15525c04b6970e562c90c4a6a`; pinned qpdf 11.9.0's wrapped
//! geometry remains the behavioral oracle for ordinary inputs.

use super::{Pipeline, PipelineError, PipelineRef, PipelineResult};
use crate::bit_stream::{BitStream, BitStreamError};
use crate::bit_writer::BitWriter;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TiffPredictorAction {
    Encode,
    Decode,
}

/// Incremental TIFF horizontal predictor stage for `/Predictor 2`.
pub(crate) struct TiffPredictor<'a> {
    identifier: String,
    next: PipelineRef<'a>,
    action: TiffPredictorAction,
    columns: u32,
    bytes_per_row: usize,
    samples_per_pixel: u32,
    bits_per_sample: u32,
    cur_row: Vec<u8>,
    previous: Vec<i64>,
    out: Vec<u8>,
}

impl<'a> TiffPredictor<'a> {
    /// Construct a TIFF predictor with qpdf's constructor validation and
    /// 32-bit-wrapping row geometry.
    #[cfg(test)]
    pub(crate) fn new(
        identifier: impl Into<String>,
        next: impl Into<PipelineRef<'a>>,
        action: TiffPredictorAction,
        columns: u32,
        samples_per_pixel: u32,
        bits_per_sample: u32,
    ) -> PipelineResult<Self> {
        Self::new_with_memory_limit(
            identifier,
            next,
            action,
            columns,
            samples_per_pixel,
            bits_per_sample,
            None,
        )
    }

    /// Construct a TIFF predictor with qpdf-head's optional row-memory budget.
    pub(crate) fn new_with_memory_limit(
        identifier: impl Into<String>,
        next: impl Into<PipelineRef<'a>>,
        action: TiffPredictorAction,
        columns: u32,
        samples_per_pixel: u32,
        bits_per_sample: u32,
        max_memory: Option<usize>,
    ) -> PipelineResult<Self> {
        if samples_per_pixel == 0 {
            return Err(PipelineError::runtime(
                "TIFFPredictor created with invalid samples_per_pixel",
            ));
        }
        if bits_per_sample == 0 || bits_per_sample > 8 * size_of::<u64>() as u32 {
            return Err(PipelineError::runtime(
                "TIFFPredictor created with invalid bits_per_sample",
            ));
        }

        // qpdf head widened this intermediate before checking it so a
        // wrapped u32 product cannot reach `previous.resize` with a bogus
        // one-byte row. The wide bound must match the narrow check's own
        // established upper bound (`u32::MAX - 1`, not `u32::MAX`): at
        // columns=u32::MAX, colors=1, bits=8 the wide value lands exactly on
        // `u32::MAX`, which a strict `>` bound against `u32::MAX` would
        // accept, even though the narrow computation below wraps this exact
        // geometry down to a plausible-looking 536,870,911-byte (~512 MiB)
        // row instead of rejecting it during construction.
        let bits_per_pixel = u64::from(bits_per_sample) * u64::from(samples_per_pixel);
        if bits_per_pixel + 7 > u64::from(u32::MAX) {
            return Err(PipelineError::runtime(
                "TIFFPredictor created with bits_per_sample and samples_per_pixel values that cause overflow",
            ));
        }
        let bytes_per_row = (u64::from(columns) * bits_per_pixel).div_ceil(8);
        if bytes_per_row == 0 || bytes_per_row > u64::from(u32::MAX - 1) {
            return Err(PipelineError::runtime(
                "TIFFPredictor created with invalid columns value",
            ));
        }
        if let Some(limit) = max_memory
            .map(|limit| u64::try_from(limit).unwrap_or(u64::MAX))
            .filter(|&limit| limit > 0)
        {
            if bytes_per_row > limit / 2 {
                return Err(PipelineError::runtime(
                    "TIFFPredictor memory limit exceeded",
                ));
            }
        }

        // Keep pinned qpdf 11.9.0's wrapped row width for inputs that remain
        // representable after the head preflight. This preserves its
        // observed partial-row and packed-row behavior; ordinary geometry has
        // the same value in both calculations.
        let bytes_per_row = columns
            .wrapping_mul(bits_per_sample)
            .wrapping_mul(samples_per_pixel)
            .wrapping_add(7)
            / 8;
        if bytes_per_row == 0 || bytes_per_row > u32::MAX - 1 {
            return Err(PipelineError::runtime(
                "TIFFPredictor created with invalid columns value",
            ));
        }

        Ok(Self {
            identifier: identifier.into(),
            next: next.into(),
            action,
            columns,
            bytes_per_row: bytes_per_row as usize,
            samples_per_pixel,
            bits_per_sample,
            cur_row: Vec::new(),
            previous: Vec::new(),
            out: Vec::new(),
        })
    }

    fn process_row(&mut self) -> PipelineResult<()> {
        self.previous.resize(self.samples_per_pixel as usize, 0);
        self.previous.fill(0);

        if self.bits_per_sample != 8 {
            let mut input = BitStream::new(&self.cur_row);
            let mut writer = BitWriter::new(&mut self.next);
            for _ in 0..self.columns {
                for previous in &mut self.previous {
                    let sample = input
                        .get_bits_signed(self.bits_per_sample as usize)
                        .map_err(map_bit_stream_error)?;
                    let new_sample = match self.action {
                        TiffPredictorAction::Encode => {
                            let new_sample = sample - *previous;
                            *previous = sample;
                            new_sample
                        }
                        TiffPredictorAction::Decode => {
                            let new_sample = sample + *previous;
                            *previous = new_sample;
                            new_sample
                        }
                    };
                    writer.write_bits_signed(new_sample, self.bits_per_sample as usize)?;
                }
            }
            writer.flush()
        } else {
            self.out.clear();
            let mut next = 0;
            while next < self.cur_row.len() {
                for previous in &mut self.previous {
                    if next == self.cur_row.len() {
                        break;
                    }
                    let sample = i64::from(self.cur_row[next]);
                    let new_sample = match self.action {
                        TiffPredictorAction::Encode => {
                            let new_sample = sample - *previous;
                            *previous = sample;
                            new_sample
                        }
                        TiffPredictorAction::Decode => {
                            let new_sample = sample + *previous;
                            *previous = new_sample;
                            new_sample
                        }
                    };
                    self.out.push(new_sample as u8);
                    next += 1;
                }
            }
            self.next.write(&self.out)
        }
    }
}

fn map_bit_stream_error(error: BitStreamError) -> PipelineError {
    match error {
        BitStreamError::Exhausted { .. } => PipelineError::runtime(error.to_string()),
        BitStreamError::TooWide | BitStreamError::AlignmentOverflow => {
            PipelineError::logic(error.to_string())
        }
    }
}

impl Pipeline for TiffPredictor<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, mut data: &[u8]) -> PipelineResult<()> {
        while !data.is_empty() {
            let remaining = self.bytes_per_row - self.cur_row.len();
            let count = remaining.min(data.len());
            self.cur_row.extend_from_slice(&data[..count]);
            data = &data[count..];

            if self.cur_row.len() == self.bytes_per_row {
                self.process_row()?;
                self.cur_row.clear();
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        if !self.cur_row.is_empty() {
            self.cur_row.resize(self.bytes_per_row, 0);
            self.process_row()?;
        }
        self.cur_row.clear();
        self.next.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{TiffPredictor, TiffPredictorAction};
    use crate::pipeline::test_support::{RecordingSink, TraceCall};
    use crate::pipeline::{Pipeline, PipelineError};

    fn run(
        action: TiffPredictorAction,
        columns: u32,
        colors: u32,
        bits: u32,
        chunks: &[&[u8]],
    ) -> (Vec<u8>, Vec<TraceCall>) {
        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        {
            let mut predictor =
                TiffPredictor::new("tiff", &mut sink, action, columns, colors, bits)
                    .expect("construction must succeed");
            for chunk in chunks {
                predictor.write(chunk).expect("write must succeed");
            }
            predictor.finish().expect("finish must succeed");
        }
        let trace = trace.borrow();
        (trace.output.clone(), trace.calls.clone())
    }

    fn construction_error(columns: u32, colors: u32, bits: u32) -> PipelineError {
        let mut sink = RecordingSink::new(&[], &[]);
        TiffPredictor::new(
            "tiff",
            &mut sink,
            TiffPredictorAction::Decode,
            columns,
            colors,
            bits,
        )
        .err()
        .expect("construction must fail")
    }

    fn construction_error_with_memory_limit(
        columns: u32,
        colors: u32,
        bits: u32,
        max_memory: Option<usize>,
    ) -> PipelineError {
        let mut sink = RecordingSink::new(&[], &[]);
        TiffPredictor::new_with_memory_limit(
            "tiff",
            &mut sink,
            TiffPredictorAction::Decode,
            columns,
            colors,
            bits,
            max_memory,
        )
        .err()
        .expect("construction must fail")
    }

    struct FixtureCase<'a> {
        columns: u32,
        colors: u32,
        bits: u32,
        encoded: &'a [u8],
        decoded: &'a [u8],
    }

    #[test]
    fn identifier_is_retained() {
        let mut sink = RecordingSink::new(&[], &[]);
        let predictor = TiffPredictor::new(
            "tiff decode",
            &mut sink,
            TiffPredictorAction::Decode,
            4,
            1,
            8,
        )
        .unwrap();
        assert_eq!(predictor.identifier(), "tiff decode");
    }

    #[test]
    fn constructor_rejects_invalid_geometry_like_qpdf() {
        assert_eq!(
            construction_error(4, 0, 8).to_string(),
            "TIFFPredictor created with invalid samples_per_pixel"
        );
        assert_eq!(
            construction_error(4, 1, 0).to_string(),
            "TIFFPredictor created with invalid bits_per_sample"
        );
        assert_eq!(
            construction_error(4, 1, 65).to_string(),
            "TIFFPredictor created with invalid bits_per_sample"
        );
        assert_eq!(
            construction_error(0, 1, 8).to_string(),
            "TIFFPredictor created with invalid columns value"
        );
        assert_eq!(
            construction_error(536_870_911, u32::MAX, 8).to_string(),
            "TIFFPredictor created with bits_per_sample and samples_per_pixel values that cause overflow"
        );
        assert_eq!(
            construction_error(u32::MAX, 5, 8).to_string(),
            "TIFFPredictor created with invalid columns value"
        );
    }

    /// `columns=u32::MAX, colors=1, bits=8` makes the raw bit count 8x
    /// larger than `u32::MAX` (34,359,738,360 bits), but the u32-wrapped
    /// byte count collapses to a plausible-looking 536,870,911 bytes
    /// (~512 MiB) after wrapping and dividing by 8. A preflight bound
    /// checked only on the *divided* byte count misses this case entirely
    /// (536,870,911 is far under any reasonable byte-count bound), letting
    /// a malformed one-byte stream trigger a ~512 MiB allocation. The
    /// preflight must bound the undivided bit count instead.
    #[test]
    fn constructor_rejects_columns_that_overflow_before_dividing_by_eight() {
        assert_eq!(
            construction_error(u32::MAX, 1, 8).to_string(),
            "TIFFPredictor created with invalid columns value"
        );
    }

    #[test]
    fn memory_limit_rejects_partial_row_padding_before_allocation() {
        assert_eq!(
            construction_error_with_memory_limit(536_870_911, 1, 8, Some(1 << 20)).to_string(),
            "TIFFPredictor memory limit exceeded"
        );

        let mut sink = RecordingSink::new(&[], &[]);
        assert!(TiffPredictor::new_with_memory_limit(
            "tiff",
            &mut sink,
            TiffPredictorAction::Decode,
            536_870_911,
            1,
            8,
            Some(0),
        )
        .is_ok());

        let mut sink = RecordingSink::new(&[], &[]);
        assert!(TiffPredictor::new_with_memory_limit(
            "tiff",
            &mut sink,
            TiffPredictorAction::Decode,
            4,
            1,
            8,
            Some(1024),
        )
        .is_ok());
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn memory_limit_uses_the_qpdf_unsigned_long_long_range() {
        assert_eq!(
            construction_error_with_memory_limit(
                3_000_000_000,
                1,
                8,
                Some(u64::from(u32::MAX) as usize + 1),
            )
            .to_string(),
            "TIFFPredictor memory limit exceeded"
        );
    }

    #[test]
    fn encode_resets_horizontal_previous_samples_for_each_row() {
        let (encoded, calls) = run(
            TiffPredictorAction::Encode,
            4,
            1,
            8,
            &[&[10, 20], &[30, 40, 50], &[60, 70, 80]],
        );

        assert_eq!(encoded, [10, 10, 10, 10, 50, 10, 10, 10]);
        assert_eq!(calls.last(), Some(&TraceCall::Finish { failed: false }));
    }

    #[test]
    fn decode_reverses_horizontal_differences_across_split_writes() {
        let (decoded, _) = run(
            TiffPredictorAction::Decode,
            4,
            1,
            8,
            &[&[10], &[10, 10, 10, 50], &[10, 10, 10]],
        );

        assert_eq!(decoded, [10, 20, 30, 40, 50, 60, 70, 80]);
    }

    #[test]
    fn finish_zero_pads_and_processes_a_partial_row() {
        let (encoded, _) = run(TiffPredictorAction::Encode, 4, 1, 8, &[&[1, 2]]);

        assert_eq!(encoded, [1, 1, 254, 0]);
    }

    #[test]
    fn packed_four_bit_samples_round_trip() {
        let (encoded, _) = run(TiffPredictorAction::Encode, 4, 1, 4, &[&[0x12, 0x34]]);
        let (decoded, _) = run(TiffPredictorAction::Decode, 4, 1, 4, &[&encoded]);

        assert_eq!(encoded, [0x11, 0x11]);
        assert_eq!(decoded, [0x12, 0x34]);
    }

    #[test]
    fn packed_one_and_two_bit_samples_round_trip() {
        let cases = [(1, [0xf0], [0x80]), (2, [0x1b], [0x15])];

        for (bits, raw, expected_encoded) in cases {
            let (encoded, _) = run(TiffPredictorAction::Encode, 4, 1, bits, &[&raw]);
            let (decoded, _) = run(TiffPredictorAction::Decode, 4, 1, bits, &[&encoded]);

            assert_eq!(encoded, expected_encoded, "{bits}-bit encode");
            assert_eq!(decoded, raw, "{bits}-bit decode");
        }
    }

    #[test]
    fn sixteen_bit_samples_round_trip() {
        let raw = [0x00, 0x01, 0x00, 0x02];
        let (encoded, _) = run(TiffPredictorAction::Encode, 2, 1, 16, &[&raw]);
        let (decoded, _) = run(TiffPredictorAction::Decode, 2, 1, 16, &[&encoded]);

        assert_eq!(encoded, [0x00, 0x01, 0x00, 0x01]);
        assert_eq!(decoded, raw);
    }

    #[test]
    fn previous_samples_are_tracked_per_color() {
        let raw = [10, 20, 30, 40];
        let (encoded, _) = run(TiffPredictorAction::Encode, 2, 2, 8, &[&raw]);
        let (decoded, _) = run(TiffPredictorAction::Decode, 2, 2, 8, &[&encoded]);

        assert_eq!(encoded, [10, 20, 20, 20]);
        assert_eq!(decoded, raw);
    }

    #[test]
    fn qpdf_tiff_fixture_vectors_round_trip() {
        // These bytes are the qpdf 11.9.0
        // libtests/qtest/predictors/tiff-*.{data,decoded} vectors.
        let cases = [
            FixtureCase {
                columns: 16,
                colors: 1,
                bits: 8,
                encoded: &[
                    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
                    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
                    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x57, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff,
                    0xff, 0xff, 0xf6, 0xf6, 0xf6, 0x00, 0x01, 0x02, 0x01,
                ][..],
                decoded: &[
                    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                    0x0e, 0x0f, 0x10, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a,
                    0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x57, 0x58, 0x57, 0x56, 0x55, 0x54, 0x53,
                    0x52, 0x51, 0x47, 0x3d, 0x33, 0x33, 0x34, 0x36, 0x37,
                ][..],
            },
            FixtureCase {
                columns: 8,
                colors: 2,
                bits: 4,
                encoded: &[
                    0xaa, 0xf1, 0xf1, 0xf1, 0xe2, 0x00, 0xb0, 0x78, 0xaa, 0xf1, 0xf1, 0xf1, 0xe2,
                    0x00, 0xb0, 0x78,
                ][..],
                decoded: &[
                    0xaa, 0x9b, 0x8c, 0x7d, 0x5f, 0x5f, 0x0f, 0x77, 0xaa, 0x9b, 0x8c, 0x7d, 0x5f,
                    0x5f, 0x0f, 0x77,
                ][..],
            },
            FixtureCase {
                columns: 4,
                colors: 1,
                bits: 16,
                encoded: &[0x55, 0x55, 0xcd, 0xf0, 0x64, 0x20, 0x39, 0x5b][..],
                decoded: &[0x55, 0x55, 0x23, 0x45, 0x87, 0x65, 0xc0, 0xc0][..],
            },
        ];

        for case in cases {
            let (decoded, _) = run(
                TiffPredictorAction::Decode,
                case.columns,
                case.colors,
                case.bits,
                &[&case.encoded[..3], &case.encoded[3..]],
            );
            let (encoded, _) = run(
                TiffPredictorAction::Encode,
                case.columns,
                case.colors,
                case.bits,
                &[&case.decoded[..1], &case.decoded[1..]],
            );

            assert_eq!(
                decoded, case.decoded,
                "decode {}-{}-{}",
                case.columns, case.colors, case.bits
            );
            assert_eq!(
                encoded, case.encoded,
                "encode {}-{}-{}",
                case.columns, case.colors, case.bits
            );
        }
    }

    #[test]
    fn wrapped_byte_geometry_stops_at_the_available_row_bytes() {
        // 8 * columns * colors wraps to 8, so qpdf's row width is one byte
        // even though the logical row has many more samples than that byte.
        let (encoded, _) = run(TiffPredictorAction::Encode, 178_956_971, 3, 8, &[&[42]]);

        assert_eq!(encoded, [42]);
    }

    #[test]
    fn exhausted_packed_row_maps_to_a_runtime_error() {
        let mut sink = RecordingSink::new(&[], &[]);
        let mut predictor = TiffPredictor::new(
            "tiff",
            &mut sink,
            TiffPredictorAction::Decode,
            2_863_311_531,
            3,
            1,
        )
        .unwrap();

        assert_eq!(
            predictor.write(&[0]).unwrap_err().to_string(),
            "overflow reading bit stream: wanted = 1; available = 0"
        );
    }

    #[test]
    fn packed_widths_above_bitstream_limit_map_to_a_logic_error() {
        let mut sink = RecordingSink::new(&[], &[]);
        let mut predictor =
            TiffPredictor::new("tiff", &mut sink, TiffPredictorAction::Decode, 1, 1, 33).unwrap();

        assert_eq!(
            predictor.write(&[0; 5]).unwrap_err().to_string(),
            "read_bits: too many bits requested"
        );
    }

    #[test]
    fn downstream_write_failure_preserves_the_completed_row() {
        let mut sink = RecordingSink::new(&[1], &[]);
        let trace = sink.trace();
        {
            let mut predictor =
                TiffPredictor::new("tiff", &mut sink, TiffPredictorAction::Encode, 4, 1, 8)
                    .unwrap();
            assert_eq!(
                predictor.write(&[1, 2, 3, 4]).unwrap_err().to_string(),
                "sink write failure 1"
            );
            predictor.finish().unwrap();
        }
        assert_eq!(
            trace.borrow().calls,
            vec![
                TraceCall::Write {
                    data: vec![1, 1, 1, 1],
                    failed: true
                },
                TraceCall::Write {
                    data: vec![1, 1, 1, 1],
                    failed: false
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn downstream_finish_failure_is_forwarded() {
        let mut sink = RecordingSink::new(&[], &[1]);
        let mut predictor =
            TiffPredictor::new("tiff", &mut sink, TiffPredictorAction::Decode, 4, 1, 8).unwrap();
        predictor.write(&[1, 2, 3, 4]).unwrap();
        assert_eq!(
            predictor.finish().unwrap_err().to_string(),
            "sink finish failure 1"
        );
    }
}

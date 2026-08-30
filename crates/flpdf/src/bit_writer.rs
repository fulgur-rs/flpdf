//! qpdf correspondence: BitWriter.cc and bits_functions.hh MSB-first bit packing into a Pipeline stage.

use std::cmp::min;

use crate::pipeline::{Pipeline, PipelineError, PipelineResult};

pub(crate) struct BitWriter<'a> {
    pipeline: &'a mut dyn Pipeline,
    byte: u8,
    bit_offset: usize,
}

impl<'a> BitWriter<'a> {
    pub(crate) fn new(pipeline: &'a mut dyn Pipeline) -> Self {
        Self {
            pipeline,
            byte: 0,
            bit_offset: 7,
        }
    }

    pub(crate) fn write_bits(&mut self, value: u64, mut bits: usize) -> PipelineResult<()> {
        self.validate_width(bits)?;

        while bits > 0 {
            let bits_to_write = min(bits, self.bit_offset + 1);
            let new_bits = (value >> (bits - bits_to_write)) & ((1_u64 << bits_to_write) - 1);
            let bits_left_in_byte = self.bit_offset + 1 - bits_to_write;
            self.byte |= (new_bits as u8) << bits_left_in_byte;

            if bits_left_in_byte == 0 {
                self.pipeline.write(&[self.byte])?;
                self.bit_offset = 7;
                self.byte = 0;
            } else {
                self.bit_offset -= bits_to_write;
            }
            bits -= bits_to_write;
        }

        Ok(())
    }

    pub(crate) fn write_bits_signed(&mut self, value: i64, bits: usize) -> PipelineResult<()> {
        if bits == 0 {
            return Ok(());
        }
        self.validate_width(bits)?;

        let value = if value < 0 {
            (1_u64 << bits).wrapping_add_signed(value)
        } else {
            value as u64
        };
        self.write_bits(value, bits)
    }

    pub(crate) fn flush(&mut self) -> PipelineResult<()> {
        if self.bit_offset < 7 {
            self.write_bits(0, self.bit_offset + 1)?;
        }
        Ok(())
    }

    fn validate_width(&self, bits: usize) -> PipelineResult<()> {
        if bits > 32 {
            return Err(PipelineError::logic("write_bits: too many bits requested"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::BitWriter;
    use crate::bit_stream::BitStream;
    use crate::pipeline::{Pipeline, PipelineError, PipelineResult};

    #[derive(Default)]
    struct TestSink {
        bytes: Vec<u8>,
        finishes: usize,
        fail_writes_remaining: usize,
    }

    impl Pipeline for TestSink {
        fn identifier(&self) -> &str {
            "test sink"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            if self.fail_writes_remaining > 0 {
                self.fail_writes_remaining -= 1;
                return Err(PipelineError::logic(format!(
                    "{}: write failed",
                    self.identifier()
                )));
            }
            self.bytes.extend_from_slice(data);
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.finishes += 1;
            Ok(())
        }
    }

    #[test]
    fn writes_msb_first_flushes_padding_and_does_not_finish_pipeline() {
        let mut sink = TestSink::default();
        sink.finish().unwrap();
        {
            let mut writer = BitWriter::new(&mut sink);
            writer.write_bits(0b101, 3).unwrap();
            writer.write_bits(0b1_0110, 5).unwrap();
            writer.write_bits(0b1100, 4).unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(sink.bytes, [0b1011_0110, 0b1100_0000]);
        assert_eq!(sink.finishes, 1);
    }

    #[test]
    fn writer_reader_round_trip_uses_only_byte_contract() {
        let mut sink = TestSink::default();
        {
            let mut writer = BitWriter::new(&mut sink);
            writer.write_bits(0xdead_beef, 32).unwrap();
            writer.write_bits_signed(-2, 4).unwrap();
            writer.flush().unwrap();
        }
        let mut reader = BitStream::new(&sink.bytes);
        assert_eq!(reader.get_bits(32).unwrap(), 0xdead_beef);
        assert_eq!(reader.get_bits_signed(4).unwrap(), -2);
    }

    #[test]
    fn zero_width_writes_are_noops() {
        let mut sink = TestSink::default();
        {
            let mut writer = BitWriter::new(&mut sink);
            writer.write_bits(0xffff_ffff, 0).unwrap();
            writer.write_bits_signed(-1, 0).unwrap();
            writer.flush().unwrap();
        }
        assert!(sink.bytes.is_empty());
        assert_eq!(sink.finishes, 0);
    }

    #[test]
    fn rejects_widths_above_qpdfs_32_bit_limit_without_writing() {
        let mut sink = TestSink::default();
        {
            let mut writer = BitWriter::new(&mut sink);
            for result in [writer.write_bits(0, 33), writer.write_bits_signed(-1, 33)] {
                assert!(matches!(result.unwrap_err(), PipelineError::Logic(_)));
            }
            writer.flush().unwrap();
        }
        assert!(sink.bytes.is_empty());
    }

    #[test]
    fn signed_writes_encode_qpdf_valid_domain_boundaries() {
        let cases = [
            (1, -1, vec![0x80]),
            (1, 0, vec![0x00]),
            (8, -128, vec![0x80]),
            (8, 127, vec![0x7f]),
            (32, -2_147_483_648, vec![0x80, 0x00, 0x00, 0x00]),
            (32, 2_147_483_647, vec![0x7f, 0xff, 0xff, 0xff]),
        ];

        for (bits, value, expected) in cases {
            let mut sink = TestSink::default();
            {
                let mut writer = BitWriter::new(&mut sink);
                writer.write_bits_signed(value, bits).unwrap();
                writer.flush().unwrap();
            }
            assert_eq!(sink.bytes, expected);
        }
    }

    #[test]
    fn writes_use_the_low_requested_bits() {
        let mut sink = TestSink::default();
        {
            let mut writer = BitWriter::new(&mut sink);
            writer.write_bits(u64::MAX, 4).unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(sink.bytes, [0xf0]);
    }

    #[test]
    fn flush_writes_one_partial_byte_once_and_is_idempotent() {
        let mut sink = TestSink::default();
        {
            let mut writer = BitWriter::new(&mut sink);
            writer.write_bits(0xa5, 8).unwrap();
            writer.write_bits(0b101, 3).unwrap();
            writer.flush().unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(sink.bytes, [0xa5, 0b1010_0000]);
        assert_eq!(sink.finishes, 0);
    }

    #[test]
    fn complete_byte_write_failure_retains_pending_byte_for_identical_retry() {
        let mut complete_sink = TestSink {
            fail_writes_remaining: 1,
            ..TestSink::default()
        };
        {
            let mut writer = BitWriter::new(&mut complete_sink);
            let complete_error = writer.write_bits(0xa5, 8).unwrap_err();
            assert!(matches!(complete_error, PipelineError::Logic(_)));
            writer.write_bits(0xa5, 8).unwrap();
        }
        assert_eq!(complete_sink.bytes, [0xa5]);
    }

    #[test]
    fn partial_byte_flush_failure_retains_pending_byte_for_flush_retry() {
        let mut partial_sink = TestSink {
            fail_writes_remaining: 1,
            ..TestSink::default()
        };
        {
            let mut writer = BitWriter::new(&mut partial_sink);
            writer.write_bits(0b101, 3).unwrap();
            let partial_error = writer.flush().unwrap_err();
            assert!(matches!(partial_error, PipelineError::Logic(_)));
            writer.flush().unwrap();
        }
        assert_eq!(partial_sink.bytes, [0b1010_0000]);
    }
}

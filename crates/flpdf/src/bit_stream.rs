//! qpdf correspondence: BitStream.cc and bits_functions.hh MSB-first bit reading with Rust error values.

use std::cmp::min;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum BitStreamError {
    #[error("overflow reading bit stream: wanted = {wanted}; available = {available}")]
    Exhausted { wanted: usize, available: usize },
    #[error("read_bits: too many bits requested")]
    TooWide,
    #[error("overflow skipping to next byte in bitstream")]
    AlignmentOverflow,
}

pub(crate) struct BitStream<'a> {
    data: &'a [u8],
    byte_position: usize,
    bit_offset: usize,
    bits_available: usize,
}

impl<'a> BitStream<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_position: 0,
            bit_offset: 7,
            bits_available: data.len() * 8,
        }
    }

    #[cfg(test)]
    pub(crate) fn reset(&mut self) {
        self.byte_position = 0;
        self.bit_offset = 7;
        self.bits_available = self.data.len() * 8;
    }

    pub(crate) fn get_bits(&mut self, mut bits: usize) -> Result<u64, BitStreamError> {
        if bits > self.bits_available {
            return Err(BitStreamError::Exhausted {
                wanted: bits,
                available: self.bits_available,
            });
        }
        if bits > 32 {
            return Err(BitStreamError::TooWide);
        }

        let mut result = 0;
        while bits > 0 {
            let byte = self.data[self.byte_position]
                & ((1_u16 << (self.bit_offset + 1)).saturating_sub(1) as u8);
            let to_copy = min(bits, self.bit_offset + 1);
            let leftover = self.bit_offset + 1 - to_copy;
            let byte = byte >> leftover;

            result <<= to_copy;
            result |= u64::from(byte);

            if leftover == 0 {
                self.bit_offset = 7;
                self.byte_position += 1;
            } else {
                self.bit_offset = leftover - 1;
            }
            bits -= to_copy;
            self.bits_available -= to_copy;
        }

        Ok(result)
    }

    pub(crate) fn get_bits_signed(&mut self, bits: usize) -> Result<i64, BitStreamError> {
        let value = self.get_bits(bits)?;
        let sign_bit = 1_u64 << bits.saturating_sub(1);
        if value > sign_bit {
            Ok(value.wrapping_sub(1_u64 << bits) as i64)
        } else {
            Ok(value as i64)
        }
    }

    pub(crate) fn get_bits_i32(&mut self, bits: usize) -> Result<i32, BitStreamError> {
        Ok(self.get_bits(bits)? as i32)
    }

    pub(crate) fn skip_to_next_byte(&mut self) -> Result<(), BitStreamError> {
        if self.bit_offset != 7 {
            let bits_to_skip = self.bit_offset + 1;
            if self.bits_available < bits_to_skip {
                return Err(BitStreamError::AlignmentOverflow);
            }
            self.bit_offset = 7;
            self.byte_position += 1;
            self.bits_available -= bits_to_skip;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{BitStream, BitStreamError};

    #[test]
    fn reads_msb_first_across_bytes_and_resets() {
        let mut bits = BitStream::new(&[0b1011_0110, 0b1100_0011]);
        assert_eq!(bits.get_bits(3).unwrap(), 0b101);
        assert_eq!(bits.get_bits(5).unwrap(), 0b1_0110);
        assert_eq!(bits.get_bits(4).unwrap(), 0b1100);
        bits.reset();
        assert_eq!(bits.get_bits(8).unwrap(), 0b1011_0110);
    }

    #[test]
    fn zero_width_alignment_and_exhaustion_match_qpdf() {
        let mut bits = BitStream::new(&[0x80, 0x5a]);
        assert_eq!(bits.get_bits(0).unwrap(), 0);
        assert_eq!(bits.get_bits(1).unwrap(), 1);
        bits.skip_to_next_byte().unwrap();
        assert_eq!(bits.get_bits(8).unwrap(), 0x5a);
        assert!(matches!(
            bits.get_bits(1),
            Err(BitStreamError::Exhausted {
                wanted: 1,
                available: 0
            })
        ));
        assert_eq!(BitStream::new(&[]).get_bits(0).unwrap(), 0);
    }

    #[test]
    fn rejects_exhaustion_before_too_wide_like_qpdf() {
        assert_eq!(
            BitStream::new(&[0; 4]).get_bits(33),
            Err(BitStreamError::Exhausted {
                wanted: 33,
                available: 32,
            })
        );
        assert_eq!(
            BitStream::new(&[0; 5]).get_bits(33),
            Err(BitStreamError::TooWide)
        );
    }

    #[test]
    fn skips_on_a_byte_boundary_without_consuming_the_next_byte() {
        let mut bits = BitStream::new(&[0xab, 0xcd]);
        assert_eq!(bits.get_bits(8).unwrap(), 0xab);
        bits.skip_to_next_byte().unwrap();
        assert_eq!(bits.get_bits(8).unwrap(), 0xcd);
    }

    #[test]
    fn reports_alignment_overflow_for_an_incomplete_internal_byte() {
        let mut bits = BitStream {
            data: &[0],
            byte_position: 0,
            bit_offset: 3,
            bits_available: 3,
        };
        assert_eq!(
            bits.skip_to_next_byte(),
            Err(BitStreamError::AlignmentOverflow)
        );
    }

    #[test]
    fn reads_signed_boundaries_observed_from_qpdf() {
        let cases = [
            (1, [0x00, 0x00, 0x00, 0x00], 0),
            (1, [0x80, 0x00, 0x00, 0x00], 1),
            (1, [0x80, 0x00, 0x00, 0x00], 1),
            (2, [0x40, 0x00, 0x00, 0x00], 1),
            (2, [0x80, 0x00, 0x00, 0x00], 2),
            (2, [0xc0, 0x00, 0x00, 0x00], -1),
            (8, [0x7f, 0x00, 0x00, 0x00], 127),
            (8, [0x80, 0x00, 0x00, 0x00], 128),
            (8, [0xff, 0x00, 0x00, 0x00], -1),
            (16, [0x7f, 0xff, 0x00, 0x00], 32_767),
            (16, [0x80, 0x00, 0x00, 0x00], 32_768),
            (16, [0xff, 0xff, 0x00, 0x00], -1),
            (32, [0x7f, 0xff, 0xff, 0xff], 2_147_483_647),
            (32, [0x80, 0x00, 0x00, 0x00], 2_147_483_648),
            (32, [0xff, 0xff, 0xff, 0xff], -1),
        ];

        for (width, data, expected) in cases {
            assert_eq!(
                BitStream::new(&data).get_bits_signed(width).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn reads_i32_with_qpdfs_native_int_conversion() {
        assert_eq!(
            BitStream::new(&[0xde, 0xad, 0xbe, 0xef])
                .get_bits_i32(32)
                .unwrap(),
            -559_038_737
        );
    }
}

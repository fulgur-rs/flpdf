//! qpdf correspondence: Pl_LZWDecoder.cc bit accumulation, table growth, code-width transitions, end-of-data latching, output boundaries, and error text.

use super::{Pipeline, PipelineError, PipelineResult};

/// The code value that resets the table and the code width.
const CLEAR_CODE: u32 = 256;

/// The code value that latches end-of-data.
const EOD_CODE: u32 = 257;

/// The first table-backed code value.
const FIRST_TABLE_CODE: u32 = 258;

/// The table index that must never be allocated.
const TABLE_LIMIT: u32 = 4096;

/// Incremental LZW decoder for `LZWDecode` streams.
///
/// The three-byte rotating input buffer, the one-code-per-input-byte cadence,
/// and the mask arithmetic in [`LzwDecoder::send_next_code`] reproduce qpdf's
/// component rather than an equivalent wide bit accumulator, because the
/// downstream write boundaries and the point at which each error is raised are
/// part of the observable contract.
pub(crate) struct LzwDecoder<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,

    // Members used for converting bits to codes.
    buf: [u8; 3],
    code_size: u32,
    buf_next: u32,
    byte_pos: u32,
    bit_pos: u32,
    bits_available: u32,

    // Members used for LZW decompression.
    code_change_delta: bool,
    eod: bool,
    table: Vec<Vec<u8>>,
    last_code: u32,
}

impl<'a> LzwDecoder<'a> {
    pub(crate) fn new(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        early_code_change: bool,
    ) -> Self {
        Self {
            identifier: identifier.into(),
            next,
            buf: [0; 3],
            code_size: 9,
            buf_next: 0,
            byte_pos: 0,
            bit_pos: 0,
            bits_available: 0,
            code_change_delta: early_code_change,
            eod: false,
            table: Vec::new(),
            last_code: CLEAR_CODE,
        }
    }

    /// Extract one code from the rotating buffer and hand it to the table logic.
    fn send_next_code(&mut self) -> PipelineResult<()> {
        let high = self.byte_pos as usize;
        let med = ((self.byte_pos + 1) % 3) as usize;
        let low = ((self.byte_pos + 2) % 3) as usize;

        let bits_from_high = 8 - self.bit_pos;
        let mut bits_from_med = self.code_size - bits_from_high;
        let mut bits_from_low = 0;
        if bits_from_med > 8 {
            bits_from_low = bits_from_med - 8;
            bits_from_med = 8;
        }
        let high_mask = (1u32 << bits_from_high) - 1;
        let med_mask = 0xff - ((1u32 << (8 - bits_from_med)) - 1);
        let low_mask = 0xff - ((1u32 << (8 - bits_from_low)) - 1);
        let mut code = 0u32;
        code += (u32::from(self.buf[high]) & high_mask) << bits_from_med;
        code += (u32::from(self.buf[med]) & med_mask) >> (8 - bits_from_med);
        if bits_from_low != 0 {
            code <<= bits_from_low;
            code += (u32::from(self.buf[low]) & low_mask) >> (8 - bits_from_low);
            self.byte_pos = low as u32;
            self.bit_pos = bits_from_low;
        } else {
            self.byte_pos = med as u32;
            self.bit_pos = bits_from_med;
        }
        if self.bit_pos == 8 {
            self.bit_pos = 0;
            self.byte_pos += 1;
            self.byte_pos %= 3;
        }
        self.bits_available -= self.code_size;

        self.handle_code(code)
    }

    /// Return the first byte of the string a code expands to.
    ///
    /// Both error arms are defensive in qpdf as well: every call site checks the
    /// table bound first, and the two reserved codes are handled before this
    /// function is reached. They are retained for contract parity and are
    /// exercised directly by unit tests.
    fn get_first_char(&self, code: u32) -> PipelineResult<u8> {
        if code < CLEAR_CODE {
            Ok(code as u8)
        } else if code > EOD_CODE {
            let index = (code - FIRST_TABLE_CODE) as usize;
            match self.table.get(index) {
                Some(entry) => Ok(entry[0]),
                None => Err(PipelineError::runtime(
                    "Pl_LZWDecoder::getFirstChar: table overflow",
                )),
            }
        } else {
            Err(PipelineError::runtime(format!(
                "Pl_LZWDecoder::getFirstChar called with invalid code ({code})"
            )))
        }
    }

    /// Append `last_code`'s string extended by `next_char` to the table.
    fn add_to_table(&mut self, next_char: u8) -> PipelineResult<()> {
        let entry = if self.last_code < CLEAR_CODE {
            vec![self.last_code as u8, next_char]
        } else if self.last_code > EOD_CODE {
            let index = (self.last_code - FIRST_TABLE_CODE) as usize;
            let Some(last) = self.table.get(index) else {
                return Err(PipelineError::runtime(
                    "Pl_LZWDecoder::addToTable: table overflow",
                ));
            };
            let mut entry = Vec::with_capacity(last.len() + 1);
            entry.extend_from_slice(last);
            entry.push(next_char);
            entry
        } else {
            return Err(PipelineError::runtime(format!(
                "Pl_LZWDecoder::addToTable called with invalid code ({})",
                self.last_code
            )));
        };
        self.table.push(entry);
        Ok(())
    }

    fn handle_code(&mut self, code: u32) -> PipelineResult<()> {
        if self.eod {
            return Ok(());
        }

        if code == CLEAR_CODE {
            self.table.clear();
            self.code_size = 9;
        } else if code == EOD_CODE {
            self.eod = true;
        } else {
            if self.last_code != CLEAR_CODE {
                // Add the entry the encoder created last time: what was read
                // last, extended by the first character of what is read now.
                let table_size = self.table.len() as u32;
                let mut next_char = 0u8;
                if code < CLEAR_CODE {
                    next_char = code as u8;
                } else if code > EOD_CODE {
                    let index = (code - FIRST_TABLE_CODE) as usize;
                    if index > table_size as usize {
                        return Err(PipelineError::runtime("LZWDecoder: bad code received"));
                    } else if index == table_size as usize {
                        // The encoder would have just created this entry, so its
                        // first character matches the first character of the
                        // previous entry.
                        next_char = self.get_first_char(self.last_code)?;
                    } else {
                        next_char = self.get_first_char(code)?;
                    }
                }
                let new_index = FIRST_TABLE_CODE + table_size;
                if new_index == TABLE_LIMIT {
                    return Err(PipelineError::runtime("LZWDecoder: table full"));
                }
                self.add_to_table(next_char)?;
                let change_index = new_index + u32::from(self.code_change_delta);
                if (change_index == 511) || (change_index == 1023) || (change_index == 2047) {
                    self.code_size += 1;
                }
            }

            if code < CLEAR_CODE {
                self.next.write(&[code as u8])?;
            } else {
                let index = (code - FIRST_TABLE_CODE) as usize;
                if index >= self.table.len() {
                    return Err(PipelineError::runtime(
                        "Pl_LZWDecoder::handleCode: table overflow",
                    ));
                }
                self.next.write(&self.table[index])?;
            }
        }

        self.last_code = code;
        Ok(())
    }
}

impl Pipeline for LzwDecoder<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        for &byte in data {
            self.buf[self.buf_next as usize] = byte;
            self.buf_next += 1;
            if self.buf_next == 3 {
                self.buf_next = 0;
            }
            self.bits_available += 8;
            if self.bits_available >= self.code_size {
                self.send_next_code()?;
            }
        }
        Ok(())
    }

    /// Finish the downstream pipeline without flushing.
    ///
    /// qpdf retains every decoder field here: trailing bits that do not complete
    /// a code are discarded without an error, no end-of-data code is synthesized,
    /// and a later write continues from the retained state.
    fn finish(&mut self) -> PipelineResult<()> {
        self.next.finish()
    }
}

/// Pack explicit codes using qpdf's decoder-side code-width evolution so the
/// produced stream is read back at exactly the intended widths.
#[cfg(test)]
pub(crate) fn pack_codes(codes: &[u32], early: bool) -> Vec<u8> {
    let delta = u32::from(early);
    let mut code_size = 9;
    let mut table_size = 0u32;
    let mut last_code = CLEAR_CODE;
    let mut bits: Vec<u8> = Vec::new();

    for &code in codes {
        for shift in (0..code_size).rev() {
            bits.push(((code >> shift) & 1) as u8);
        }
        if code == CLEAR_CODE {
            table_size = 0;
            code_size = 9;
        } else if code != EOD_CODE && last_code != CLEAR_CODE {
            let new_index = FIRST_TABLE_CODE + table_size;
            table_size += 1;
            let change_index = new_index + delta;
            if (change_index == 511) || (change_index == 1023) || (change_index == 2047) {
                code_size += 1;
            }
        }
        last_code = code;
    }

    while !bits.len().is_multiple_of(8) {
        bits.push(0);
    }
    bits.chunks(8)
        .map(|chunk| chunk.iter().fold(0u8, |byte, &bit| (byte << 1) | bit))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{pack_codes as pack, LzwDecoder};
    use crate::pipeline::test_support::{RecordingSink, TraceCall};
    use crate::pipeline::{Pipeline, PipelineError};

    fn decode_with(codes: &[u32], early: bool) -> (Vec<u8>, Vec<TraceCall>) {
        decode_chunks(&[pack(codes, early)], early)
    }

    fn decode_chunks(chunks: &[Vec<u8>], early: bool) -> (Vec<u8>, Vec<TraceCall>) {
        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        {
            let mut decoder = LzwDecoder::new("lzw decode", &mut sink, early);
            for chunk in chunks {
                decoder.write(chunk).expect("write must succeed");
            }
            decoder.finish().expect("finish must succeed");
        }
        let trace = trace.borrow();
        (trace.output.clone(), trace.calls.clone())
    }

    fn decode_error(codes: &[u32], early: bool) -> PipelineError {
        let mut sink = RecordingSink::new(&[], &[]);
        let mut decoder = LzwDecoder::new("lzw decode", &mut sink, early);
        decoder
            .write(&pack(codes, early))
            .expect_err("write must fail")
    }

    #[test]
    fn identifier_is_retained() {
        let mut sink = RecordingSink::new(&[], &[]);
        let decoder = LzwDecoder::new("lzw decode", &mut sink, true);
        assert_eq!(decoder.identifier(), "lzw decode");
    }

    #[test]
    fn clear_and_eod_only_stream_decodes_to_nothing() {
        let (output, calls) = decode_with(&[256, 257], true);
        assert!(output.is_empty());
        assert_eq!(calls, vec![TraceCall::Finish { failed: false }]);
    }

    #[test]
    fn literal_codes_are_written_one_byte_at_a_time() {
        let (output, calls) = decode_with(&[256, 0x41, 0x42, 257], true);
        assert_eq!(output, b"AB");
        assert_eq!(
            calls,
            vec![
                TraceCall::Write {
                    data: b"A".to_vec(),
                    failed: false
                },
                TraceCall::Write {
                    data: b"B".to_vec(),
                    failed: false
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn self_referential_code_expands_from_the_previous_entry() {
        // Code 258 is created by this very code, so it expands to the previous
        // entry extended by that entry's own first character.
        let (output, calls) = decode_with(&[256, 0x41, 0x42, 258, 257], true);
        assert_eq!(output, b"ABAB");
        assert_eq!(
            calls,
            vec![
                TraceCall::Write {
                    data: b"A".to_vec(),
                    failed: false
                },
                TraceCall::Write {
                    data: b"B".to_vec(),
                    failed: false
                },
                TraceCall::Write {
                    data: b"AB".to_vec(),
                    failed: false
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn table_backed_code_is_written_as_one_downstream_chunk() {
        // Codes 258 and 259 expand to "AB" and "BA", each in a single write.
        let (output, calls) = decode_with(&[256, 0x41, 0x42, 258, 259, 257], true);
        assert_eq!(output, b"ABABBA");
        assert_eq!(
            calls,
            vec![
                TraceCall::Write {
                    data: b"A".to_vec(),
                    failed: false
                },
                TraceCall::Write {
                    data: b"B".to_vec(),
                    failed: false
                },
                TraceCall::Write {
                    data: b"AB".to_vec(),
                    failed: false
                },
                TraceCall::Write {
                    data: b"BA".to_vec(),
                    failed: false
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn intermediate_clear_resets_the_table_and_code_width() {
        let mut codes = vec![256u32];
        codes.extend(std::iter::repeat_n(0x41u32, 300));
        codes.push(256);
        codes.extend([0x42, 0x43, 258, 257]);
        let (output, _) = decode_with(&codes, true);

        let mut expected = vec![b'A'; 300];
        expected.extend_from_slice(b"BCBC");
        assert_eq!(output, expected);
    }

    #[test]
    fn early_code_change_shifts_the_width_transition_by_one_code() {
        // The same code sequence packed for each EarlyChange setting decodes to
        // the same plaintext, which is only possible when the decoder applies
        // the matching transition point.
        let codes: Vec<u32> = std::iter::once(256)
            .chain(std::iter::repeat_n(0x41u32, 300))
            .chain(std::iter::once(257))
            .collect();
        let (early_output, _) = decode_with(&codes, true);
        let (late_output, _) = decode_with(&codes, false);
        assert_eq!(early_output, vec![b'A'; 300]);
        assert_eq!(late_output, vec![b'A'; 300]);
        assert_ne!(pack(&codes, true), pack(&codes, false));
    }

    #[test]
    fn width_transitions_apply_at_1023_and_2047() {
        // 1800 codes cross both the 511 and 1023 boundaries and stop short of
        // 2047, so a wrong transition point would desynchronize the bit reader.
        let codes: Vec<u32> = std::iter::once(256)
            .chain(std::iter::repeat_n(0x41u32, 1800))
            .chain(std::iter::once(257))
            .collect();
        let (output, _) = decode_with(&codes, true);
        assert_eq!(output, vec![b'A'; 1800]);
    }

    #[test]
    fn table_full_is_reported_at_index_4096() {
        // The 3840th literal after a clear code would allocate index 4096.
        let full: Vec<u32> = std::iter::once(256)
            .chain(std::iter::repeat_n(0x41u32, 3840))
            .collect();
        assert_eq!(
            decode_error(&full, true).to_string(),
            "LZWDecoder: table full"
        );

        let nearly_full: Vec<u32> = std::iter::once(256)
            .chain(std::iter::repeat_n(0x41u32, 3839))
            .collect();
        let (output, _) = decode_with(&nearly_full, true);
        assert_eq!(output.len(), 3839);
    }

    #[test]
    fn code_past_the_table_end_is_rejected() {
        assert_eq!(
            decode_error(&[256, 0x41, 259], true).to_string(),
            "LZWDecoder: bad code received"
        );
    }

    #[test]
    fn table_backed_code_immediately_after_clear_overflows() {
        // `last_code` is still the clear code, so no entry is added and the
        // lookup finds an empty table.
        assert_eq!(
            decode_error(&[256, 258], true).to_string(),
            "Pl_LZWDecoder::handleCode: table overflow"
        );
    }

    #[test]
    fn errors_are_reported_as_runtime_exceptions() {
        assert!(matches!(
            decode_error(&[256, 258], true),
            PipelineError::Runtime(_)
        ));
    }

    #[test]
    fn get_first_char_rejects_the_reserved_codes() {
        let mut sink = RecordingSink::new(&[], &[]);
        let decoder = LzwDecoder::new("lzw decode", &mut sink, true);
        assert_eq!(
            decoder.get_first_char(256).unwrap_err().to_string(),
            "Pl_LZWDecoder::getFirstChar called with invalid code (256)"
        );
        assert_eq!(
            decoder.get_first_char(257).unwrap_err().to_string(),
            "Pl_LZWDecoder::getFirstChar called with invalid code (257)"
        );
    }

    #[test]
    fn get_first_char_reports_table_overflow_and_reads_literals() {
        let mut sink = RecordingSink::new(&[], &[]);
        let decoder = LzwDecoder::new("lzw decode", &mut sink, true);
        assert_eq!(decoder.get_first_char(0x41).unwrap(), b'A');
        assert_eq!(
            decoder.get_first_char(258).unwrap_err().to_string(),
            "Pl_LZWDecoder::getFirstChar: table overflow"
        );
    }

    #[test]
    fn add_to_table_rejects_the_reserved_codes_and_missing_entries() {
        let mut sink = RecordingSink::new(&[], &[]);
        let mut decoder = LzwDecoder::new("lzw decode", &mut sink, true);

        decoder.last_code = 257;
        assert_eq!(
            decoder.add_to_table(b'x').unwrap_err().to_string(),
            "Pl_LZWDecoder::addToTable called with invalid code (257)"
        );

        decoder.last_code = 258;
        assert_eq!(
            decoder.add_to_table(b'x').unwrap_err().to_string(),
            "Pl_LZWDecoder::addToTable: table overflow"
        );

        decoder.last_code = 0x41;
        decoder.add_to_table(b'B').expect("literal entry");
        decoder.last_code = 258;
        decoder.add_to_table(b'C').expect("table entry");
        assert_eq!(decoder.table, vec![b"AB".to_vec(), b"ABC".to_vec()]);
    }

    #[test]
    fn trailing_bits_that_do_not_complete_a_code_are_discarded() {
        let (output, calls) = decode_with(&[256, 0x41, 0x42], true);
        assert_eq!(output, b"AB");
        assert_eq!(calls.last(), Some(&TraceCall::Finish { failed: false }));
    }

    #[test]
    fn input_after_end_of_data_is_ignored_but_finish_still_propagates() {
        let (output, calls) = decode_with(&[256, 0x41, 257, 0x42, 0x43], true);
        assert_eq!(output, b"A");
        assert_eq!(
            calls,
            vec![
                TraceCall::Write {
                    data: b"A".to_vec(),
                    failed: false
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn end_of_data_survives_finish_and_suppresses_later_writes() {
        let first = pack(&[256, 0x41, 0x42, 257], true);
        let second = pack(&[256, 0x41, 0x42], true);
        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        {
            let mut decoder = LzwDecoder::new("lzw decode", &mut sink, true);
            decoder.write(&first).unwrap();
            decoder.finish().unwrap();
            decoder.write(&second).unwrap();
            decoder.finish().unwrap();
        }
        let trace = trace.borrow();
        assert_eq!(trace.output, b"AB");
        assert_eq!(
            trace.calls.iter().filter(|call| matches!(call, TraceCall::Finish { .. })).count(),
            2
        );
    }

    #[test]
    fn state_is_retained_across_finish_when_no_end_of_data_was_seen() {
        let stream = pack(&[256, 0x41, 0x42, 258], true);
        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        {
            let mut decoder = LzwDecoder::new("lzw decode", &mut sink, true);
            decoder.write(&stream[..2]).unwrap();
            decoder.finish().unwrap();
            decoder.write(&stream[2..]).unwrap();
            decoder.finish().unwrap();
        }
        let trace = trace.borrow();
        assert_eq!(trace.output, b"ABAB");
    }

    #[test]
    fn every_input_split_produces_the_same_output() {
        let stream = pack(&[256, 0x41, 0x42, 258, 259, 257], true);
        let (whole, _) = decode_chunks(&[stream.clone()], true);
        for split in 0..=stream.len() {
            let (parts, _) = decode_chunks(
                &[stream[..split].to_vec(), stream[split..].to_vec()],
                true,
            );
            assert_eq!(parts, whole, "split at {split}");
        }
    }

    #[test]
    fn downstream_write_failure_stops_at_that_call() {
        let stream = pack(&[256, 0x41, 0x42, 257], true);
        let mut sink = RecordingSink::new(&[2], &[]);
        let trace = sink.trace();
        let error = {
            let mut decoder = LzwDecoder::new("lzw decode", &mut sink, true);
            decoder.write(&stream).expect_err("second write fails")
        };
        assert_eq!(error.to_string(), "sink write failure 2");
        let trace = trace.borrow();
        assert_eq!(
            trace.calls,
            vec![
                TraceCall::Write {
                    data: b"A".to_vec(),
                    failed: false
                },
                TraceCall::Write {
                    data: b"B".to_vec(),
                    failed: true
                },
            ]
        );
    }

    #[test]
    fn downstream_finish_failure_propagates() {
        let mut sink = RecordingSink::new(&[], &[1]);
        let mut decoder = LzwDecoder::new("lzw decode", &mut sink, true);
        assert_eq!(
            decoder.finish().unwrap_err().to_string(),
            "sink finish failure 1"
        );
    }

    #[test]
    fn empty_write_changes_nothing() {
        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        {
            let mut decoder = LzwDecoder::new("lzw decode", &mut sink, true);
            decoder.write(b"").unwrap();
            decoder.finish().unwrap();
        }
        assert_eq!(trace.borrow().calls, vec![TraceCall::Finish { failed: false }]);
    }
}

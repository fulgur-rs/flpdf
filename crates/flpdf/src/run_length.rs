//! RunLengthDecode encode/decode per PDF 1.7 section 7.4.5.
//!
//! ## Decoder notes
//! - Length byte L in 0..=127: copy the next L+1 literal bytes verbatim.
//! - Length byte L == 128 (0x80): EOD marker, stop.
//! - Length byte L in 129..=255: repeat the next single byte (257 - L) times.
//! - If input ends without a 0x80 EOD marker, implicit EOD is accepted (not an
//!   error) **as long as** the read position is at a valid L boundary.
//! - A truncated literal run (L says copy N bytes but fewer than N remain) is
//!   an error.
//! - A truncated repeat (L in 129..=255 but no byte follows) is an error.
//!
//! ## Encoder notes
//! - Greedily splits input into literal runs (1..=128 bytes) and repeat runs
//!   (2..=128 identical bytes).
//! - A repeat run is only emitted when three or more consecutive identical bytes
//!   appear (otherwise the bytes join a literal run).
//! - Literal run of length L: header byte = L - 1, followed by L bytes.
//! - Repeat run of length R: header byte = 257 - R, followed by 1 byte.
//! - Always appends 0x80 (EOD) at the end.
//! - Encoder is infallible.

/// Decode a RunLength-encoded byte slice into raw bytes.
///
/// Returns `Err(String)` with a `RunLengthDecode:` prefix on malformed input.
pub(crate) fn decode(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut pos = 0;

    loop {
        // If we run out of bytes at a clean boundary, treat as implicit EOD.
        if pos >= input.len() {
            break;
        }

        let l = input[pos];
        pos += 1;

        match l {
            128 => {
                // EOD
                break;
            }
            0..=127 => {
                // Literal run: copy next (l + 1) bytes verbatim.
                let count = usize::from(l) + 1;
                if pos + count > input.len() {
                    return Err(format!(
                        "RunLengthDecode: truncated literal run: need {} bytes at position {}, only {} remain",
                        count,
                        pos,
                        input.len() - pos,
                    ));
                }
                out.extend_from_slice(&input[pos..pos + count]);
                pos += count;
            }
            129..=255 => {
                // Repeat run: repeat next byte (257 - l) times.
                let count = usize::from(257u16 - u16::from(l));
                if pos >= input.len() {
                    return Err(format!(
                        "RunLengthDecode: truncated repeat run: no byte at position {pos}"
                    ));
                }
                let byte = input[pos];
                pos += 1;
                for _ in 0..count {
                    out.push(byte);
                }
            }
        }
    }

    Ok(out)
}

/// Encode raw bytes using RunLength encoding (with trailing 0x80 EOD).
///
/// This function never fails.
pub(crate) fn encode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;

    while i < input.len() {
        // Count how many consecutive identical bytes start at i.
        let run_byte = input[i];
        let mut run_len = 1;
        while i + run_len < input.len() && input[i + run_len] == run_byte && run_len < 128 {
            run_len += 1;
        }

        if run_len >= 3 {
            // Emit a repeat run.
            out.push((257u16 - run_len as u16) as u8); // header: 257 - R
            out.push(run_byte);
            i += run_len;
        } else {
            // Collect a literal run. We accumulate bytes until we either:
            // - hit a run of >= 3 identical bytes (which warrants a repeat), or
            // - reach 128 bytes, or
            // - reach end of input.
            let lit_start = i;
            let mut lit_len = 0;

            while i < input.len() && lit_len < 128 {
                // Peek ahead for a run of >= 3.
                let peek_byte = input[i];
                let mut peek_run = 1;
                while i + peek_run < input.len()
                    && input[i + peek_run] == peek_byte
                    && peek_run < 128
                {
                    peek_run += 1;
                }

                if peek_run >= 3 {
                    // Break here; the repeat run will be emitted next iteration.
                    break;
                }

                // Consume exactly peek_run bytes (they are not a worthwhile repeat).
                let take = (peek_run).min(128 - lit_len);
                i += take;
                lit_len += take;
            }

            if lit_len > 0 {
                // Emit literal: header = lit_len - 1, then lit_len bytes.
                out.push(lit_len as u8 - 1);
                out.extend_from_slice(&input[lit_start..lit_start + lit_len]);
            }
        }
    }

    // Append EOD marker.
    out.push(0x80);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- Decoder happy paths -----

    #[test]
    fn decode_empty_just_eod() {
        // Only the EOD byte: should produce empty output.
        assert_eq!(decode(&[0x80]).unwrap(), b"");
    }

    #[test]
    fn decode_short_literal() {
        // Header 0x02 (l=2) → copy 3 literal bytes.
        let input = [0x02, b'A', b'B', b'C', 0x80];
        assert_eq!(decode(&input).unwrap(), b"ABC");
    }

    #[test]
    fn decode_short_repeat() {
        // Header 0xFE (l=254) → 257-254 = 3 copies of 0x41.
        let input = [0xFE, 0x41, 0x80];
        assert_eq!(decode(&input).unwrap(), &[0x41, 0x41, 0x41]);
    }

    #[test]
    fn decode_mixed_literal_and_repeat() {
        // 2 literal bytes, then 4 repeated bytes, then 1 literal byte.
        // Literal 2 bytes (header=0x01): 'X', 'Y'
        // Repeat 4 bytes (header=257-4=253=0xFD): 'Z'
        // Literal 1 byte (header=0x00): 'W'
        // EOD
        let input = [0x01, b'X', b'Y', 0xFD, b'Z', 0x00, b'W', 0x80];
        let expected: Vec<u8> = b"XYZZZZW".to_vec();
        assert_eq!(decode(&input).unwrap(), expected);
    }

    #[test]
    fn decode_max_literal_128_bytes() {
        // Header 0x7F (l=127) → copy 128 literal bytes.
        let mut input = vec![0x7Fu8];
        for i in 0u8..128 {
            input.push(i);
        }
        input.push(0x80); // EOD
        let result = decode(&input).unwrap();
        assert_eq!(result.len(), 128);
        for (i, &b) in result.iter().enumerate() {
            assert_eq!(b, i as u8);
        }
    }

    #[test]
    fn decode_max_repeat_128_copies() {
        // Header 0x81 (l=129) → 257-129 = 128 copies of next byte.
        let input = [0x81, 0xAB, 0x80];
        let result = decode(&input).unwrap();
        assert_eq!(result.len(), 128);
        assert!(result.iter().all(|&b| b == 0xAB));
    }

    // ----- Decoder error paths -----

    #[test]
    fn decode_rejects_truncated_literal() {
        // Header says 6 bytes (l=5), but only 3 follow before EOD.
        let input = [0x05, b'A', b'B', b'C']; // no EOD, only 3 bytes after header
        let result = decode(&input);
        assert!(result.is_err(), "expected error for truncated literal");
        let err = result.unwrap_err();
        assert!(
            err.starts_with("RunLengthDecode:"),
            "error should have RunLengthDecode: prefix, got: {err}"
        );
    }

    #[test]
    fn decode_rejects_truncated_repeat() {
        // Header 0xFD (l=253) → repeat run, but no following byte.
        let input = [0xFD];
        let result = decode(&input);
        assert!(result.is_err(), "expected error for truncated repeat");
        let err = result.unwrap_err();
        assert!(
            err.starts_with("RunLengthDecode:"),
            "error should have RunLengthDecode: prefix, got: {err}"
        );
    }

    #[test]
    fn decode_implicit_eod_at_boundary() {
        // Input ends without 0x80; read position is at a clean L boundary.
        // Should be treated as implicit EOD (not an error).
        let input = [0x00, b'A']; // one literal byte, no EOD
        let result = decode(&input).unwrap();
        assert_eq!(result, b"A");
    }

    // ----- Encoder round-trips -----

    fn round_trip(raw: &[u8]) {
        let encoded = encode(raw);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(
            decoded,
            raw,
            "round-trip failed for input of length {}",
            raw.len()
        );
    }

    #[test]
    fn round_trip_empty() {
        round_trip(b"");
    }

    #[test]
    fn round_trip_127_bytes_one_max_literal_minus_one() {
        let data: Vec<u8> = (0u8..127).collect();
        round_trip(&data);
    }

    #[test]
    fn round_trip_128_bytes_one_max_literal() {
        let data: Vec<u8> = (0u8..128).collect();
        round_trip(&data);
    }

    #[test]
    fn round_trip_129_bytes_max_literal_plus_one() {
        let data: Vec<u8> = (0u8..=128).collect();
        round_trip(&data);
    }

    #[test]
    fn round_trip_all_same_128_bytes_one_max_repeat() {
        let data = vec![0xA5u8; 128];
        round_trip(&data);
    }

    #[test]
    fn round_trip_all_same_129_bytes() {
        let data = vec![0xA5u8; 129];
        round_trip(&data);
    }

    #[test]
    fn round_trip_alternating_bytes_all_literals() {
        // Alternating bytes: 0x00, 0x01, 0x00, 0x01, ...
        // No run of 3+ identical bytes, so encoder must use all literals.
        let data: Vec<u8> = (0u8..128).map(|i| i % 2).collect();
        round_trip(&data);
    }

    #[test]
    fn round_trip_mixed_runs_and_literals() {
        // Hand-crafted deterministic fixture with known mixed content.
        // Pattern: 5 literal bytes, 10 repeated bytes, 3 literal bytes, 8 repeated bytes.
        let mut data = vec![0x10u8, 0x20, 0x30, 0x40, 0x50]; // 5 literals
        data.extend(vec![0xBBu8; 10]); // 10 repeats
        data.extend([0x61u8, 0x62, 0x63]); // 3 literals
        data.extend(vec![0xFFu8; 8]); // 8 repeats
        round_trip(&data);
    }

    #[test]
    fn round_trip_all_bytes() {
        let data: Vec<u8> = (0u8..=255).collect();
        round_trip(&data);
    }

    #[test]
    fn round_trip_deterministic_pseudo_random() {
        // Deterministic fixture using a simple xorshift32 PRNG with a fixed seed.
        // This exercises encoder boundary conditions without requiring the rand crate.
        fn xorshift32(state: &mut u32) -> u8 {
            *state ^= *state << 13;
            *state ^= *state >> 17;
            *state ^= *state << 5;
            (*state & 0xFF) as u8
        }

        let mut state: u32 = 0xDEAD_BEEF;
        let data: Vec<u8> = (0..512).map(|_| xorshift32(&mut state)).collect();
        round_trip(&data);
    }
}

//! qpdf correspondence: Mirrors qpdf 11.9.0 libqpdf/RC4.cc and libqpdf/RC4_native.cc.
//! Stateful RC4 compatibility component for legacy PDF encryption.

use std::ffi::CStr;

use crate::security::primitives::PrimitiveError;

/// Stateful RC4 compatibility cipher mirroring qpdf's `RC4_native`.
///
/// RC4 is cryptographically broken and is retained only for legacy PDF
/// compatibility. Higher layers own the weak-crypto policy.
pub(crate) struct Rc4 {
    state: [u8; 256],
    x: u8,
    y: u8,
}

impl Rc4 {
    /// Initialize RC4 from an explicit non-empty key.
    pub(crate) fn new(key: &[u8]) -> Result<Self, PrimitiveError> {
        if key.is_empty() {
            return Err(PrimitiveError::InvalidLength);
        }

        let mut state = [0; 256];
        for (i, byte) in state.iter_mut().enumerate() {
            *byte = i as u8;
        }
        let mut key_index = 0;
        let mut state_index = 0_u8;
        for i in 0..256 {
            state_index = state_index
                .wrapping_add(key[key_index])
                .wrapping_add(state[i]);
            state.swap(i, usize::from(state_index));
            key_index = (key_index + 1) % key.len();
        }

        Ok(Self { state, x: 0, y: 0 })
    }

    /// Initialize RC4 using qpdf's NUL-terminated key mode.
    ///
    /// Retained for the qpdf-compatible stateful API and exercised by the
    /// differential/unit oracle; current PDF consumers use explicit keys.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "retained for the planned qpdf-compatible API")
    )]
    pub(crate) fn from_c_str(key: &CStr) -> Result<Self, PrimitiveError> {
        Self::new(key.to_bytes())
    }

    /// Return an encrypted/decrypted copy of `input`, retaining stream state.
    ///
    /// Retained alongside the in-place operation for qpdf API parity and
    /// exercised by the differential/unit oracle.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "retained for the planned qpdf-compatible API")
    )]
    pub(crate) fn process(&mut self, input: &[u8]) -> Vec<u8> {
        let mut output = input.to_vec();
        self.process_in_place(&mut output);
        output
    }

    /// Encrypt/decrypt `data` in place, retaining stream state.
    pub(crate) fn process_in_place(&mut self, data: &mut [u8]) {
        for byte in data {
            self.x = self.x.wrapping_add(1);
            self.y = self.y.wrapping_add(self.state[usize::from(self.x)]);
            self.state.swap(usize::from(self.x), usize::from(self.y));
            let key_index =
                self.state[usize::from(self.x)].wrapping_add(self.state[usize::from(self.y)]);
            *byte ^= self.state[usize::from(key_index)];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    #[derive(Clone, Copy)]
    enum OracleKeyMode {
        Explicit,
        CStr,
    }

    struct OracleCase {
        name: &'static str,
        mode: OracleKeyMode,
        key: Vec<u8>,
        input: Vec<u8>,
        split: usize,
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn oracle_cases() -> Vec<OracleCase> {
        vec![
            OracleCase {
                name: "explicit-one-byte-empty-input",
                mode: OracleKeyMode::Explicit,
                key: vec![0x7f],
                input: vec![],
                split: 0,
            },
            OracleCase {
                name: "explicit-five-byte-rfc",
                mode: OracleKeyMode::Explicit,
                key: vec![1, 2, 3, 4, 5],
                input: vec![0; 32],
                split: 7,
            },
            OracleCase {
                name: "explicit-sixteen-byte-in-place",
                mode: OracleKeyMode::Explicit,
                key: (0..16).collect(),
                input: (0..97).map(|i| (i * 17) as u8).collect(),
                split: 31,
            },
            OracleCase {
                name: "explicit-256-byte-key",
                mode: OracleKeyMode::Explicit,
                key: (0..=255).collect(),
                input: (0..64).collect(),
                split: 1,
            },
            OracleCase {
                name: "explicit-key-over-256",
                mode: OracleKeyMode::Explicit,
                key: (0..300).map(|i| (i * 29) as u8).collect(),
                input: (0..129).map(|i| (i * 11) as u8).collect(),
                split: 128,
            },
            OracleCase {
                name: "c-string-first-nul",
                mode: OracleKeyMode::CStr,
                key: b"Key\0ignored suffix".to_vec(),
                input: b"Plaintext split across calls".to_vec(),
                split: 9,
            },
        ]
    }

    fn flpdf_records(case: &OracleCase) -> String {
        let new_cipher = || match case.mode {
            OracleKeyMode::Explicit => Rc4::new(&case.key).unwrap(),
            OracleKeyMode::CStr => {
                Rc4::from_c_str(CStr::from_bytes_until_nul(&case.key).unwrap()).unwrap()
            }
        };

        let mut one_shot = new_cipher();
        let one = one_shot.process(&case.input);
        let mut split_cipher = new_cipher();
        let mut split = split_cipher.process(&case.input[..case.split]);
        split.extend(split_cipher.process(&case.input[case.split..]));
        let mut in_place = case.input.clone();
        new_cipher().process_in_place(&mut in_place);
        format!(
            "one\t{}\nsplit\t{}\nin-place\t{}\n",
            hex(&one),
            hex(&split),
            hex(&in_place)
        )
    }

    fn run_qpdf_probe(probe: &Path, case: &OracleCase) -> String {
        let mode = match case.mode {
            OracleKeyMode::Explicit => "explicit",
            OracleKeyMode::CStr => "cstr",
        };
        let output = Command::new(probe)
            .args([
                mode,
                &hex(&case.key),
                &hex(&case.input),
                &case.split.to_string(),
            ])
            .output()
            .expect("execute qpdf RC4 probe");
        assert!(
            output.status.success(),
            "qpdf RC4 probe failed for {}: {}",
            case.name,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("probe output is ASCII")
    }

    #[test]
    #[ignore = "live qpdf 11.9.0 RC4 oracle"]
    fn qpdf_rc4_differential() {
        let probe = std::env::var_os("QPDF_RC4_PROBE")
            .expect("set QPDF_RC4_PROBE to the qpdf 11.9.0 probe");
        for case in oracle_cases() {
            assert_eq!(
                flpdf_records(&case),
                run_qpdf_probe(Path::new(&probe), &case),
                "case {}",
                case.name
            );
        }
    }

    #[test]
    fn oracle_cases_have_matching_one_split_and_in_place_records() {
        for case in oracle_cases() {
            let records = flpdf_records(&case);
            let mut lines = records.lines().map(|line| line.split_once('\t').unwrap().1);
            let one = lines.next().unwrap();
            assert_eq!(lines.next(), Some(one), "split case {}", case.name);
            assert_eq!(lines.next(), Some(one), "in-place case {}", case.name);
        }
    }

    #[test]
    fn rfc_6229_five_byte_key_keystream() {
        let mut cipher = Rc4::new(&[1, 2, 3, 4, 5]).unwrap();
        assert_eq!(
            cipher.process(&[0; 16]),
            [
                0xb2, 0x39, 0x63, 0x05, 0xf0, 0x3d, 0xc0, 0x27, 0xcc, 0xc3, 0x52, 0x4a, 0x0a, 0x11,
                0x18, 0xa8,
            ]
        );
    }

    #[test]
    fn classic_key_plaintext_vector() {
        let mut cipher = Rc4::new(b"Key").unwrap();
        assert_eq!(
            cipher.process(b"Plaintext"),
            [0xbb, 0xf3, 0x16, 0xe8, 0xd9, 0x40, 0xaf, 0x0a, 0xd3]
        );
    }

    #[test]
    fn accepts_qpdf_explicit_key_lengths() {
        for len in [1, 5, 16, 256, 300] {
            let key = (0..len).map(|i| i as u8).collect::<Vec<_>>();
            let mut cipher = Rc4::new(&key).unwrap();
            assert_eq!(cipher.process(&[0]).len(), 1, "key length {len}");
        }
    }

    #[test]
    fn bytes_after_ksa_window_do_not_change_state() {
        let prefix = (0..=255).map(|i| i as u8).collect::<Vec<_>>();
        let mut key_a = prefix.clone();
        key_a.extend_from_slice(&[1, 2, 3]);
        let mut key_b = prefix;
        key_b.extend_from_slice(&[9, 8, 7]);

        let mut a = Rc4::new(&key_a).unwrap();
        let mut b = Rc4::new(&key_b).unwrap();
        assert_eq!(a.process(&[0; 64]), b.process(&[0; 64]));
    }

    #[test]
    fn split_calls_retain_the_same_state_as_one_call() {
        let input = b"state must continue across process calls";
        let mut one_shot = Rc4::new(b"split-key").unwrap();
        let expected = one_shot.process(input);

        let mut split = Rc4::new(b"split-key").unwrap();
        let mut actual = split.process(&input[..7]);
        actual.extend(split.process(&input[7..]));
        assert_eq!(actual, expected);
    }

    #[test]
    fn allocating_and_in_place_processing_match() {
        let input = b"same input and output pointers are supported";
        let mut allocating = Rc4::new(b"in-place-key").unwrap();
        let expected = allocating.process(input);

        let mut actual = input.to_vec();
        Rc4::new(b"in-place-key")
            .unwrap()
            .process_in_place(&mut actual);
        assert_eq!(actual, expected);
    }

    #[test]
    fn c_string_mode_excludes_the_terminating_nul() {
        let c_key = c"Key";
        let mut c_string = Rc4::from_c_str(c_key).unwrap();
        let mut explicit = Rc4::new(b"Key").unwrap();
        assert_eq!(c_string.process(&[0; 32]), explicit.process(&[0; 32]));
    }

    #[test]
    fn empty_input_does_not_advance_state() {
        let mut after_empty = Rc4::new(b"Key").unwrap();
        assert!(after_empty.process(&[]).is_empty());
        let mut empty_in_place = [];
        after_empty.process_in_place(&mut empty_in_place);

        let mut fresh = Rc4::new(b"Key").unwrap();
        assert_eq!(after_empty.process(b"next"), fresh.process(b"next"));
    }

    #[test]
    fn empty_explicit_and_c_string_keys_are_rejected() {
        assert!(matches!(Rc4::new(b""), Err(PrimitiveError::InvalidLength)));
        let empty = c"";
        assert!(matches!(
            Rc4::from_c_str(empty),
            Err(PrimitiveError::InvalidLength)
        ));
    }
}

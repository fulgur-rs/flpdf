//! Mirrors qpdf 11.9.0 libqpdf/RC4.cc and libqpdf/RC4_native.cc.
//! Stateful RC4 compatibility component for legacy PDF encryption.

use std::ffi::CStr;

use crate::encryption::primitives::PrimitiveError;

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
    #[cfg(unix)]
    use std::{fs, os::unix::fs::PermissionsExt};

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

    fn run_qpdf_probe_command(mut command: Command, case: &OracleCase) -> String {
        let mode = match case.mode {
            OracleKeyMode::Explicit => "explicit",
            OracleKeyMode::CStr => "cstr",
        };
        let output = command
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

    fn run_qpdf_probe(probe: &Path, case: &OracleCase) -> String {
        run_qpdf_probe_command(Command::new(probe), case)
    }

    fn assert_qpdf_oracle_matches(mut qpdf_records: impl FnMut(&OracleCase) -> String) {
        for case in oracle_cases() {
            assert_eq!(
                flpdf_records(&case),
                qpdf_records(&case),
                "case {}",
                case.name
            );
        }
    }

    #[test]
    #[ignore = "live qpdf 11.9.0 RC4 oracle"]
    // cov:ignore-start: ignored live entry point; ordinary tests cover the comparison loop and fake-probe boundary
    fn qpdf_rc4_differential() {
        let probe = std::env::var_os("QPDF_RC4_PROBE")
            .expect("set QPDF_RC4_PROBE to the qpdf 11.9.0 probe");
        assert_qpdf_oracle_matches(|case| run_qpdf_probe(Path::new(&probe), case));
    }
    // cov:ignore-end

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

    /// Write a stand-in probe script.
    ///
    /// The script is handed to `/bin/sh` as an argument rather than executed
    /// directly, so a still-open write handle cannot make the spawn fail with
    /// `ETXTBSY`.
    #[cfg(unix)]
    fn write_test_probe(path: &Path, source: &str) {
        fs::write(path, source).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(unix)]
    fn run_test_probe(probe: &Path, case: &OracleCase) -> String {
        let mut command = Command::new("/bin/sh");
        command.arg(probe);
        run_qpdf_probe_command(command, case)
    }

    #[cfg(unix)]
    #[test]
    fn qpdf_probe_passes_exact_explicit_and_c_string_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        write_test_probe(&probe, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n");

        let explicit = OracleCase {
            name: "explicit-arguments",
            mode: OracleKeyMode::Explicit,
            key: vec![0x01, 0xab],
            input: vec![0x00, 0xff],
            split: 1,
        };
        assert_eq!(
            run_test_probe(&probe, &explicit),
            "explicit\n01ab\n00ff\n1\n"
        );

        let c_string = OracleCase {
            name: "c-string-arguments",
            mode: OracleKeyMode::CStr,
            key: vec![b'K', 0, b'Z'],
            input: vec![b'A'],
            split: 0,
        };
        assert_eq!(run_test_probe(&probe, &c_string), "cstr\n4b005a\n41\n0\n");
    }

    #[cfg(unix)]
    #[test]
    fn probe_that_is_still_open_for_writing_still_runs() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        write_test_probe(&probe, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n");
        let _write_open = std::fs::OpenOptions::new()
            .write(true)
            .open(&probe)
            .unwrap();

        let case = OracleCase {
            name: "write-open-probe",
            mode: OracleKeyMode::Explicit,
            key: vec![0x01],
            input: vec![0x02],
            split: 0,
        };
        assert_eq!(run_test_probe(&probe, &case), "explicit\n01\n02\n0\n");
    }

    #[cfg(unix)]
    #[test]
    fn qpdf_probe_failure_reports_case_and_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        write_test_probe(&probe, "#!/bin/sh\nprintf 'probe stderr' >&2\nexit 7\n");
        let case = OracleCase {
            name: "failure-case",
            mode: OracleKeyMode::Explicit,
            key: vec![1],
            input: vec![],
            split: 0,
        };

        let panic = std::panic::catch_unwind(|| run_test_probe(&probe, &case)).unwrap_err();
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("qpdf RC4 probe failed for failure-case"));
        assert!(message.contains("probe stderr"));
    }

    #[cfg(unix)]
    #[test]
    fn qpdf_probe_rejects_non_utf8_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        write_test_probe(&probe, "#!/bin/sh\nprintf '\\377'\n");
        let case = OracleCase {
            name: "non-utf8",
            mode: OracleKeyMode::Explicit,
            key: vec![1],
            input: vec![],
            split: 0,
        };

        let panic = std::panic::catch_unwind(|| run_test_probe(&probe, &case)).unwrap_err();
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("probe output is ASCII"));
    }

    #[cfg(unix)]
    #[test]
    fn qpdf_probe_spawn_failure_reports_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("missing-probe");
        let case = OracleCase {
            name: "spawn-failure",
            mode: OracleKeyMode::Explicit,
            key: vec![1],
            input: vec![],
            split: 0,
        };

        let panic = std::panic::catch_unwind(|| run_qpdf_probe(&probe, &case)).unwrap_err();
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("execute qpdf RC4 probe"), "{message}");
    }

    #[test]
    fn qpdf_comparison_checks_every_oracle_case() {
        let mut visited = Vec::new();
        assert_qpdf_oracle_matches(|case| {
            visited.push(case.name);
            flpdf_records(case)
        });
        assert_eq!(
            visited,
            [
                "explicit-one-byte-empty-input",
                "explicit-five-byte-rfc",
                "explicit-sixteen-byte-in-place",
                "explicit-256-byte-key",
                "explicit-key-over-256",
                "c-string-first-nul",
            ]
        );
    }

    #[test]
    fn qpdf_comparison_rejects_a_mismatched_record() {
        let panic = std::panic::catch_unwind(|| {
            assert_qpdf_oracle_matches(|case| {
                if case.name == "explicit-five-byte-rfc" {
                    "wrong record".to_string()
                } else {
                    flpdf_records(case)
                }
            });
        });
        assert!(panic.is_err());
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

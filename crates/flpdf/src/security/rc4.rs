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
    pub(crate) fn from_c_str(key: &CStr) -> Result<Self, PrimitiveError> {
        Self::new(key.to_bytes())
    }

    /// Return an encrypted/decrypted copy of `input`, retaining stream state.
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
        let c_key = CStr::from_bytes_with_nul(b"Key\0").unwrap();
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
        let empty = CStr::from_bytes_with_nul(b"\0").unwrap();
        assert!(matches!(
            Rc4::from_c_str(empty),
            Err(PrimitiveError::InvalidLength)
        ));
    }
}

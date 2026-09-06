//! qpdf correspondence: QPDFWriter.hh:641-663 and QPDFWriter.cc:842-847 current data-key state.

use crate::encryption::primitives::compute_data_key;

/// Writer-owned encryption parameters and the key for the object being emitted.
///
/// qpdf stores these values on `QPDFWriter::Members`
/// (`include/qpdf/QPDFWriter.hh:641-663`) and updates `cur_data_key` through
/// `QPDFWriter::setDataKey` (`libqpdf/QPDFWriter.cc:842-847`). The revision is
/// retained even though qpdf's `compute_data_key` implementation does not read
/// it after receiving it.
#[derive(Clone, Debug)]
pub(crate) struct WriterEncryptionState {
    encryption_key: Vec<u8>,
    encrypt_use_aes: bool,
    encryption_v: i32,
    _encryption_r: i32,
    cur_data_key: Option<Vec<u8>>,
}

impl WriterEncryptionState {
    pub(crate) fn new(
        _encrypted: bool,
        encryption_key: Vec<u8>,
        encrypt_use_aes: bool,
        encryption_v: i32,
        _encryption_r: i32,
    ) -> Self {
        Self {
            encryption_key,
            encrypt_use_aes,
            encryption_v,
            _encryption_r,
            cur_data_key: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn encrypt_use_aes(&self) -> bool {
        self.encrypt_use_aes
    }

    pub(crate) fn current_data_key(&self) -> Option<&[u8]> {
        self.cur_data_key.as_deref()
    }

    /// Run one object emission with qpdf's top-level/member key lifecycle.
    ///
    /// `object_stream_index` replaces qpdf's `-1` sentinel with `None`. qpdf
    /// sets and clears the key only for top-level objects
    /// (`libqpdf/QPDFWriter.cc:1785-1796`); ObjStm members are serialized without
    /// an individual key because the container stream is encrypted as a whole.
    ///
    /// qpdf's explicit clear follows successful `unparseObject`. This Rust
    /// wrapper also clears after an `Err`, which cannot change emitted bytes and
    /// prevents failed callbacks from leaking state into later inspection.
    pub(crate) fn with_object_data_key<T, E>(
        &mut self,
        emitted_object_number: u32,
        object_stream_index: Option<u32>,
        emit: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<T, E> {
        if object_stream_index.is_some() {
            return emit(self);
        }

        self.set_data_key(emitted_object_number);
        let result = emit(self);
        self.cur_data_key = None;
        result
    }

    fn set_data_key(&mut self, emitted_object_number: u32) {
        self.cur_data_key = Some(compute_data_key(
            &self.encryption_key,
            emitted_object_number,
            0,
            self.encrypt_use_aes,
            i64::from(self.encryption_v),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::WriterEncryptionState;

    #[test]
    fn rc4_data_key_uses_emitted_number_and_generation_zero() {
        let mut state = WriterEncryptionState::new(true, vec![0x01; 5], false, 2, 3);

        state.set_data_key(1);

        assert!(!state.encrypt_use_aes());
        assert_eq!(
            state.current_data_key(),
            Some([0x95, 0x80, 0x33, 0x93, 0xa0, 0x9e, 0x46, 0xba, 0xb0, 0x04].as_slice())
        );
    }

    #[test]
    fn aes128_data_key_appends_pdf_aes_salt() {
        let mut state = WriterEncryptionState::new(true, vec![0x42; 16], true, 4, 4);

        state.set_data_key(10);

        assert!(state.encrypt_use_aes());
        assert_eq!(
            state.current_data_key(),
            Some(
                [
                    0x4c, 0x7a, 0xd3, 0x28, 0xa6, 0x8a, 0x6a, 0xcb, 0x8b, 0x81, 0xf6, 0x8c, 0x86,
                    0x75, 0x6b, 0x7a,
                ]
                .as_slice()
            )
        );
    }

    #[test]
    fn aes_short_file_key_uses_qpdfs_salted_output_length() {
        let mut state = WriterEncryptionState::new(true, vec![0x42; 7], true, 4, 4);

        state.set_data_key(10);

        assert_eq!(
            state.current_data_key(),
            Some(
                [
                    0x8f, 0xac, 0x28, 0x25, 0xd4, 0xb5, 0x04, 0x0f, 0x9f, 0xe9, 0x99, 0x96, 0x68,
                    0x7f, 0x8a, 0x17,
                ]
                .as_slice()
            )
        );
    }

    #[test]
    fn v5_data_key_is_the_file_key_directly() {
        let file_key: Vec<u8> = (0..32).collect();
        let mut state = WriterEncryptionState::new(true, file_key.clone(), true, 5, 6);

        state.set_data_key(27);

        assert_eq!(state.current_data_key(), Some(file_key.as_slice()));
    }

    #[test]
    fn writer_state_still_follows_qpdf_set_data_key_order() {
        let mut state = WriterEncryptionState::new(false, Vec::new(), false, 0, 0);

        state.set_data_key(7);

        assert_eq!(
            state.current_data_key(),
            Some([0xd7, 0x6b, 0x6e, 0x2a, 0x6e].as_slice())
        );
    }

    #[test]
    fn top_level_emission_sets_key_for_callback_and_clears_after_success() {
        let mut state = WriterEncryptionState::new(true, vec![0x01; 5], false, 2, 3);
        let mut observed = None;

        let result = state.with_object_data_key(1, None, |state| {
            observed = state.current_data_key().map(<[u8]>::to_vec);
            Ok::<_, &'static str>("written")
        });

        assert_eq!(result, Ok("written"));
        assert_eq!(
            observed,
            Some(vec![
                0x95, 0x80, 0x33, 0x93, 0xa0, 0x9e, 0x46, 0xba, 0xb0, 0x04
            ])
        );
        assert_eq!(state.current_data_key(), None);
    }

    #[test]
    fn top_level_emission_clears_key_and_preserves_callback_error() {
        let mut state = WriterEncryptionState::new(true, vec![0x42; 16], true, 4, 4);

        let result = state.with_object_data_key(10, None, |state| {
            assert!(state.current_data_key().is_some());
            Err::<(), _>("emission failed")
        });

        assert_eq!(result, Err("emission failed"));
        assert_eq!(state.current_data_key(), None);
    }

    #[test]
    fn object_stream_member_does_not_receive_an_individual_data_key() {
        let mut state = WriterEncryptionState::new(true, vec![0x42; 16], true, 4, 4);
        let mut observed = Some(vec![0xff]);

        let result = state.with_object_data_key(10, Some(3), |state| {
            observed = state.current_data_key().map(<[u8]>::to_vec);
            Ok::<_, &'static str>(())
        });

        assert_eq!(result, Ok(()));
        assert_eq!(observed, None);
        assert_eq!(state.current_data_key(), None);
    }

    #[test]
    fn invalid_v5_direct_key_length_is_deferred_to_crypto_consumer() {
        let file_key = vec![0x5a; 31];
        let mut state = WriterEncryptionState::new(true, file_key.clone(), true, 5, 6);
        let mut observed = None;

        let result = state.with_object_data_key(44, None, |state| {
            observed = state.current_data_key().map(<[u8]>::to_vec);
            Ok::<_, &'static str>(())
        });

        assert_eq!(result, Ok(()));
        assert_eq!(observed, Some(file_key));
        assert_eq!(state.current_data_key(), None);
    }
}

//! qpdf correspondence: Pl_AES_PDF.cc — AES-128/256 CBC with the PDF block
//! padding of ISO 32000-1 section 7.6.2, streamed one 16-byte block at a time.
//!
//! qpdf reaches AES through `QPDFCryptoImpl::rijndael_init`/`rijndael_process`
//! (`libqpdf/qpdf/Pl_AES_PDF.hh:47`), a provider abstraction this crate replaces
//! with the `aes`/`cbc` crates directly — CLAUDE.md deviation class (B), the
//! same substitution `docs/qpdf-correspondence.md` already records for the
//! crypto provider. The block-at-a-time call shape is preserved: one
//! `rijndael_process` per 16 bytes, with the chaining state living in the
//! cipher rather than beside it.

use super::{Pipeline, PipelineError, PipelineResult};
use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use aes::{Aes128, Aes256};

/// qpdf `QPDFCryptoImpl::rijndael_buf_size` (`Pl_AES_PDF.hh:44`).
const BUF_SIZE: usize = 16;

type Aes128CbcDec = cbc::Decryptor<Aes128>;
type Aes256CbcDec = cbc::Decryptor<Aes256>;
type Aes128CbcEnc = cbc::Encryptor<Aes128>;
type Aes256CbcEnc = cbc::Encryptor<Aes256>;

/// The initialized cipher, standing in for the state qpdf's crypto provider
/// holds between `rijndael_init` and `rijndael_finalize`. `None` before the
/// first block, mirroring qpdf's `first` flag gating `rijndael_init`
/// (`Pl_AES_PDF.cc:152-181`).
enum Cipher {
    Cbc128Decrypt(Box<Aes128CbcDec>),
    Cbc256Decrypt(Box<Aes256CbcDec>),
    Cbc128Encrypt(Box<Aes128CbcEnc>),
    Cbc256Encrypt(Box<Aes256CbcEnc>),
}

impl Cipher {
    /// qpdf `rijndael_process` (`Pl_AES_PDF.cc:182`), one block in, one out.
    fn process(&mut self, inbuf: &[u8; BUF_SIZE], outbuf: &mut [u8; BUF_SIZE]) {
        match self {
            Self::Cbc128Decrypt(cipher) => {
                cipher.decrypt_block_b2b_mut(inbuf.into(), outbuf.into())
            }
            Self::Cbc256Decrypt(cipher) => {
                cipher.decrypt_block_b2b_mut(inbuf.into(), outbuf.into())
            }
            Self::Cbc128Encrypt(cipher) => {
                cipher.encrypt_block_b2b_mut(inbuf.into(), outbuf.into())
            }
            Self::Cbc256Encrypt(cipher) => {
                cipher.encrypt_block_b2b_mut(inbuf.into(), outbuf.into())
            }
        }
    }
}

/// qpdf `Pl_AES_PDF` (`libqpdf/qpdf/Pl_AES_PDF.hh:12-60`).
///
// No production caller until `QPDF::pipeStreamData`'s decrypt path is ported;
// the same not-yet-wired state `PlRc4` and the other `Pl_*` stages carry.
#[allow(dead_code)]
pub(crate) struct PlAesPdf<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    key: Vec<u8>,
    cipher: Option<Cipher>,
    /// qpdf `encrypt` (`Pl_AES_PDF.hh:49`).
    encrypt: bool,
    /// qpdf `cbc_mode` (`Pl_AES_PDF.hh:49`); PDF always uses CBC, and only
    /// `disableCBC` — documented "for testing only" — clears it.
    cbc_mode: bool,
    /// qpdf `first` (`Pl_AES_PDF.hh:50`): whether the next `flush` is the one
    /// that establishes the initialization vector.
    first: bool,
    /// qpdf `offset` (`Pl_AES_PDF.hh:51`): how much of `inbuf` is filled.
    offset: usize,
    inbuf: [u8; BUF_SIZE],
    outbuf: [u8; BUF_SIZE],
    cbc_block: [u8; BUF_SIZE],
    specified_iv: [u8; BUF_SIZE],
    use_zero_iv: bool,
    use_specified_iv: bool,
    disable_padding: bool,
}

#[allow(dead_code)]
impl<'a> PlAesPdf<'a> {
    fn new(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        encrypt: bool,
        key: &[u8],
    ) -> PipelineResult<Self> {
        if key.len() != 16 && key.len() != 32 {
            return Err(PipelineError::logic(format!(
                "Pl_AES_PDF: key must be 16 or 32 bytes, got {}",
                key.len()
            )));
        }
        Ok(Self {
            identifier: identifier.into(),
            next,
            key: key.to_vec(),
            cipher: None,
            encrypt,
            cbc_mode: true,
            first: true,
            offset: 0,
            inbuf: [0; BUF_SIZE],
            outbuf: [0; BUF_SIZE],
            cbc_block: [0; BUF_SIZE],
            specified_iv: [0; BUF_SIZE],
            use_zero_iv: false,
            use_specified_iv: false,
            disable_padding: false,
        })
    }

    /// A decrypting stage, mirroring `Pl_AES_PDF(identifier, next, false, key,
    /// key_bytes)`. Unless a vector is supplied, the initialization vector is
    /// read from the head of the input, which is where a PDF stream carries it.
    ///
    /// # Errors
    ///
    /// [`PipelineError`] when `key` is not 16 or 32 bytes: qpdf's own header
    /// scopes this pipeline to "AES-128 and AES-256"
    /// (`libqpdf/qpdf/Pl_AES_PDF.hh:8-9`).
    pub(crate) fn new_decrypt(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        key: &[u8],
    ) -> PipelineResult<Self> {
        Self::new(identifier, next, false, key)
    }

    /// An encrypting stage, mirroring `Pl_AES_PDF(identifier, next, true, key,
    /// key_bytes)`. Unless a vector is supplied or zeroed, a fresh random
    /// initialization vector is generated and written ahead of the ciphertext.
    ///
    /// # Errors
    ///
    /// Same key-length contract as [`Self::new_decrypt`].
    pub(crate) fn new_encrypt(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        key: &[u8],
    ) -> PipelineResult<Self> {
        Self::new(identifier, next, true, key)
    }

    /// qpdf `Pl_AES_PDF::setIV` (`Pl_AES_PDF.cc:47-58`): supply the vector
    /// rather than generating or reading one. It is not written to the output.
    ///
    /// # Errors
    ///
    /// [`PipelineError`] unless `iv` is exactly one block.
    pub(crate) fn set_iv(&mut self, iv: &[u8]) -> PipelineResult<()> {
        if iv.len() != BUF_SIZE {
            return Err(PipelineError::logic(format!(
                "Pl_AES_PDF: specified initialization vector size in bytes must be {BUF_SIZE}"
            )));
        }
        self.use_specified_iv = true;
        self.specified_iv.copy_from_slice(iv);
        Ok(())
    }

    /// qpdf `Pl_AES_PDF::initializeVector` (`Pl_AES_PDF.cc:126-143`).
    fn initialize_vector(&mut self) -> PipelineResult<()> {
        if self.use_zero_iv {
            self.cbc_block = [0; BUF_SIZE];
        } else if self.use_specified_iv {
            self.cbc_block = self.specified_iv;
        } else {
            // qpdf `QUtil::initializeWithRandomBytes` (`:141`).
            getrandom::getrandom(&mut self.cbc_block).map_err(|error| {
                PipelineError::runtime(format!(
                    "Pl_AES_PDF: OS CSPRNG unavailable for AES IV generation: {error}"
                ))
            })?;
        }
        Ok(())
    }

    /// qpdf `Pl_AES_PDF::flush` (`Pl_AES_PDF.cc:145-199`).
    fn flush(&mut self, strip_padding: bool) -> PipelineResult<()> {
        if self.offset != BUF_SIZE {
            // qpdf `:147-149`.
            return Err(PipelineError::logic(
                "AES pipeline: flush called when buffer was not full",
            ));
        }

        if self.first {
            self.first = false;
            let mut return_after_init = false;
            if self.cbc_mode {
                if self.encrypt {
                    // qpdf `:158-164`: set the vector and, unless it is one the
                    // reader already knows, write it ahead of the ciphertext.
                    self.initialize_vector()?;
                    if !(self.use_zero_iv || self.use_specified_iv) {
                        let iv = self.cbc_block;
                        self.next.write(&iv)?;
                    }
                } else if self.use_zero_iv || self.use_specified_iv {
                    // qpdf `:165-168`: the vector was never written to the
                    // input, so reconstruct rather than consume it.
                    self.initialize_vector()?;
                } else {
                    // qpdf `:169-174`: take the first block of input as the
                    // vector. Nothing is written and the block is consumed.
                    self.cbc_block.copy_from_slice(&self.inbuf);
                    self.offset = 0;
                    return_after_init = true;
                }
            }
            self.cipher = Some(self.build_cipher());
            if return_after_init {
                return Ok(());
            }
        }

        let Some(cipher) = self.cipher.as_mut() else {
            // Unreachable: `first` is cleared only where the cipher is built.
            return Err(PipelineError::logic(
                "AES pipeline: cipher used before initialization",
            ));
        };
        cipher.process(&self.inbuf, &mut self.outbuf);

        let mut bytes = BUF_SIZE;
        if strip_padding {
            // qpdf `:184-196`. Deliberately not PKCS#7-strict: a trailing byte
            // larger than the block size, or trailing bytes that disagree,
            // leaves the block whole instead of failing.
            let last = usize::from(self.outbuf[BUF_SIZE - 1]);
            if last <= BUF_SIZE {
                let strip = (1..=last).all(|i| usize::from(self.outbuf[BUF_SIZE - i]) == last);
                if strip {
                    bytes -= last;
                }
            }
        }
        self.offset = 0;
        self.next.write(&self.outbuf[..bytes])
    }

    /// qpdf `rijndael_init` (`Pl_AES_PDF.cc:176-177`), dispatching on the key
    /// size and direction the constructor already validated.
    fn build_cipher(&self) -> Cipher {
        let iv = &self.cbc_block;
        match (self.encrypt, self.key.len()) {
            (false, 16) => {
                let key: &[u8; 16] = self.key.as_slice().try_into().expect("checked in new");
                Cipher::Cbc128Decrypt(Box::new(Aes128CbcDec::new(key.into(), iv.into())))
            }
            (false, _) => {
                let key: &[u8; 32] = self.key.as_slice().try_into().expect("checked in new");
                Cipher::Cbc256Decrypt(Box::new(Aes256CbcDec::new(key.into(), iv.into())))
            }
            (true, 16) => {
                let key: &[u8; 16] = self.key.as_slice().try_into().expect("checked in new");
                Cipher::Cbc128Encrypt(Box::new(Aes128CbcEnc::new(key.into(), iv.into())))
            }
            (true, _) => {
                let key: &[u8; 32] = self.key.as_slice().try_into().expect("checked in new");
                Cipher::Cbc256Encrypt(Box::new(Aes256CbcEnc::new(key.into(), iv.into())))
            }
        }
    }
}

impl Pipeline for PlAesPdf<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    /// qpdf `Pl_AES_PDF::write` (`Pl_AES_PDF.cc:72-90`): fill `inbuf`, and
    /// flush only when a *further* byte arrives for a full buffer. That leaves
    /// the final block buffered until `finish`, which is what lets the
    /// decrypting side strip its padding and the encrypting side add one.
    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        let mut rest = data;
        while !rest.is_empty() {
            if self.offset == BUF_SIZE {
                self.flush(false)?;
            }
            let available = BUF_SIZE - self.offset;
            let take = rest.len().min(available);
            self.inbuf[self.offset..self.offset + take].copy_from_slice(&rest[..take]);
            self.offset += take;
            rest = &rest[take..];
        }
        Ok(())
    }

    /// qpdf `Pl_AES_PDF::finish` (`Pl_AES_PDF.cc:92-124`).
    fn finish(&mut self) -> PipelineResult<()> {
        if self.encrypt {
            if self.offset == BUF_SIZE {
                self.flush(false)?;
            }
            if !self.disable_padding {
                // qpdf `:96-103`: pad as ISO 32000-1 section 7.6.2 describes,
                // "including providing an entire block of padding if the input
                // was a multiple of 16 bytes".
                let pad = BUF_SIZE - self.offset;
                self.inbuf[self.offset..]
                    .fill(u8::try_from(pad).expect("pad is at most the 16-byte block size"));
                self.offset = BUF_SIZE;
                self.flush(false)?;
            }
        } else {
            if self.offset != BUF_SIZE {
                // qpdf `:107-118`: "This is never supposed to happen as the
                // output is always supposed to be padded. However, we have
                // encountered files for which the output is not a multiple of
                // the block size. In this case, pad with zeroes and hope for
                // the best."
                self.inbuf[self.offset..].fill(0);
                self.offset = BUF_SIZE;
            }
            self.flush(!self.disable_padding)?;
        }
        self.next.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::PlAesPdf;
    use crate::pipeline::buffer::Buffer;
    use crate::pipeline::Pipeline;

    // Independent vectors: produced by `openssl enc -aes-128-cbc` rather than by
    // flpdf's own AES helpers, so a mistake shared with those helpers cannot
    // make these pass.
    //
    //   key = 000102030405060708090a0b0c0d0e0f
    //   iv  = 101112131415161718191a1b1c1d1e1f
    const KEY128: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const IV: [u8; 16] = [
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f,
    ];
    const PLAINTEXT: &[u8] = b"qpdf Pl_AES_PDF parity vector";
    const CIPHERTEXT: [u8; 32] = [
        0x2d, 0xd7, 0xfe, 0x17, 0x4b, 0xf4, 0x79, 0x2c, 0x60, 0xc2, 0xe8, 0x8b, 0x16, 0x48, 0xc5,
        0x2e, 0xc0, 0xab, 0x2e, 0x5f, 0xb6, 0xba, 0x56, 0xe6, 0x1c, 0xe4, 0x88, 0x41, 0x9f, 0x07,
        0x25, 0x69,
    ];

    const PLAINTEXT_32: &[u8] = b"0123456789abcdef0123456789abcdef";
    const CIPHERTEXT_32: [u8; 48] = [
        0xeb, 0x9e, 0x5b, 0xa4, 0x1b, 0x90, 0x2d, 0xb8, 0x25, 0x29, 0x82, 0xaa, 0x1a, 0x23, 0xf4,
        0xbe, 0x91, 0x67, 0x65, 0xb2, 0x9c, 0xa3, 0xa2, 0xe2, 0x72, 0x8e, 0x43, 0x2e, 0x67, 0x0f,
        0x49, 0x6d, 0xd7, 0x0b, 0x33, 0xf4, 0xfe, 0x8c, 0xbe, 0x6e, 0xe3, 0xc2, 0x4e, 0xe9, 0x0e,
        0xdf, 0xe9, 0x9e,
    ];

    fn iv_then_ciphertext() -> Vec<u8> {
        let mut input = IV.to_vec();
        input.extend_from_slice(&CIPHERTEXT);
        input
    }

    // `Pl_AES_PDF::flush` takes the *first input block* as the CBC
    // initialization vector when decrypting without an explicit one, consuming
    // it without emitting anything (`libqpdf/Pl_AES_PDF.cc:158-172`). The
    // caller does not split the IV off; the stage does.
    #[test]
    fn decrypting_consumes_the_leading_block_as_the_initialization_vector() {
        let mut sink = Buffer::new("plaintext", None);
        let mut stage = PlAesPdf::new_decrypt("AES stream decryption", &mut sink, &KEY128)
            .expect("AES-128 key is a supported length");

        stage.write(&iv_then_ciphertext()).expect("write");
        stage.finish().expect("finish");

        assert_eq!(sink.take_buffer().expect("buffer"), PLAINTEXT);
    }

    // `Pl_AES_PDF::finish` pads as ISO 32000-1 section 7.6.2 describes
    // (`libqpdf/Pl_AES_PDF.cc:96-104`) — the same scheme openssl calls PKCS#7,
    // including a whole block of padding when the input is already a multiple
    // of 16. A specified vector is not written to the output (`:160-163`), so
    // the result is directly comparable with openssl's ciphertext.
    #[test]
    fn encrypting_with_a_specified_vector_matches_an_independent_implementation() {
        let mut sink = Buffer::new("ciphertext", None);
        let mut stage = PlAesPdf::new_encrypt("AES stream encryption", &mut sink, &KEY128)
            .expect("AES-128 key is a supported length");
        stage
            .set_iv(&IV)
            .expect("a 16-byte vector is the block size");

        stage.write(PLAINTEXT).expect("write");
        stage.finish().expect("finish");

        assert_eq!(sink.take_buffer().expect("buffer"), CIPHERTEXT);
    }

    // The "entire block of padding if the input was a multiple of 16 bytes"
    // case qpdf's own comment calls out (`libqpdf/Pl_AES_PDF.cc:99-101`).
    #[test]
    fn encrypting_a_whole_number_of_blocks_appends_a_full_padding_block() {
        let mut sink = Buffer::new("ciphertext", None);
        let mut stage = PlAesPdf::new_encrypt("AES stream encryption", &mut sink, &KEY128)
            .expect("AES-128 key is a supported length");
        stage
            .set_iv(&IV)
            .expect("a 16-byte vector is the block size");

        stage.write(PLAINTEXT_32).expect("write");
        stage.finish().expect("finish");

        let out = sink.take_buffer().expect("buffer");
        assert_eq!(out.len(), PLAINTEXT_32.len() + 16, "one full padding block");
        assert_eq!(out, CIPHERTEXT_32);
    }

    // `setIV` throws when the vector is not exactly the block size
    // (`libqpdf/Pl_AES_PDF.cc:47-58`).
    #[test]
    fn a_specified_vector_of_the_wrong_size_is_rejected() {
        let mut sink = Buffer::new("ciphertext", None);
        let mut stage = PlAesPdf::new_encrypt("AES stream encryption", &mut sink, &KEY128)
            .expect("AES-128 key is a supported length");

        let error = stage
            .set_iv(&[0u8; 15])
            .expect_err("15 bytes is not a block");

        assert!(
            error.to_string().contains("must be 16"),
            "unexpected message: {error}"
        );
    }
}

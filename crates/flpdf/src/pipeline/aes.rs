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
use aes::cipher::{BlockDecryptMut, KeyIvInit};
use aes::{Aes128, Aes256};

/// qpdf `QPDFCryptoImpl::rijndael_buf_size` (`Pl_AES_PDF.hh:44`).
const BUF_SIZE: usize = 16;

type Aes128CbcDec = cbc::Decryptor<Aes128>;
type Aes256CbcDec = cbc::Decryptor<Aes256>;

/// The initialized cipher, standing in for the state qpdf's crypto provider
/// holds between `rijndael_init` and `rijndael_finalize`. `None` before the
/// first block, mirroring qpdf's `first` flag gating `rijndael_init`
/// (`Pl_AES_PDF.cc:152-181`).
enum Cipher {
    Cbc128Decrypt(Box<Aes128CbcDec>),
    Cbc256Decrypt(Box<Aes256CbcDec>),
}

impl Cipher {
    fn process(&mut self, inbuf: &[u8; BUF_SIZE], outbuf: &mut [u8; BUF_SIZE]) {
        match self {
            Self::Cbc128Decrypt(cipher) => {
                cipher.decrypt_block_b2b_mut(inbuf.into(), outbuf.into())
            }
            Self::Cbc256Decrypt(cipher) => {
                cipher.decrypt_block_b2b_mut(inbuf.into(), outbuf.into())
            }
        }
    }
}

/// qpdf `Pl_AES_PDF` (`libqpdf/qpdf/Pl_AES_PDF.hh:12-60`).
pub(crate) struct PlAesPdf<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    key: Vec<u8>,
    cipher: Option<Cipher>,
    /// qpdf `first` (`Pl_AES_PDF.hh:50`): whether the next `flush` is the one
    /// that establishes the initialization vector.
    first: bool,
    /// qpdf `offset` (`Pl_AES_PDF.hh:51`): how much of `inbuf` is filled.
    offset: usize,
    inbuf: [u8; BUF_SIZE],
    outbuf: [u8; BUF_SIZE],
    cbc_block: [u8; BUF_SIZE],
    disable_padding: bool,
}

impl<'a> PlAesPdf<'a> {
    /// A decrypting stage, mirroring `Pl_AES_PDF(identifier, next, false, key,
    /// key_bytes)`. The initialization vector is read from the head of the
    /// input, which is where a PDF stream carries it.
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
            first: true,
            offset: 0,
            inbuf: [0; BUF_SIZE],
            outbuf: [0; BUF_SIZE],
            cbc_block: [0; BUF_SIZE],
            disable_padding: false,
        })
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
            // qpdf `:166-171`: decrypting without an explicit vector takes the
            // first block of input as the initialization vector. Nothing is
            // written, and the block is consumed.
            self.cbc_block.copy_from_slice(&self.inbuf);
            self.offset = 0;
            self.cipher = Some(self.build_cipher()?);
            return Ok(());
        }

        let Some(cipher) = self.cipher.as_mut() else {
            // Unreachable: `first` is cleared only where the cipher is built.
            return Err(PipelineError::logic(
                "AES pipeline: cipher used before initialization",
            ));
        };
        // qpdf `:182`, one `rijndael_process` per block.
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

    fn build_cipher(&self) -> PipelineResult<Cipher> {
        let iv = &self.cbc_block;
        match self.key.len() {
            16 => {
                let key: &[u8; 16] = self.key.as_slice().try_into().expect("checked above");
                Ok(Cipher::Cbc128Decrypt(Box::new(Aes128CbcDec::new(
                    key.into(),
                    iv.into(),
                ))))
            }
            // The constructor rejects every other length.
            _ => {
                let key: &[u8; 32] = self.key.as_slice().try_into().expect("checked above");
                Ok(Cipher::Cbc256Decrypt(Box::new(Aes256CbcDec::new(
                    key.into(),
                    iv.into(),
                ))))
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
    /// decrypting side strip its padding.
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

    /// qpdf `Pl_AES_PDF::finish` (`Pl_AES_PDF.cc:92-124`), decrypting side.
    fn finish(&mut self) -> PipelineResult<()> {
        if self.offset != BUF_SIZE {
            // qpdf `:107-118`: "This is never supposed to happen as the output
            // is always supposed to be padded. However, we have encountered
            // files for which the output is not a multiple of the block size.
            // In this case, pad with zeroes and hope for the best."
            self.inbuf[self.offset..].fill(0);
            self.offset = BUF_SIZE;
        }
        self.flush(!self.disable_padding)?;
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
}

//! qpdf correspondence: Pl_AES_PDF.cc AES-128/256 CBC with the PDF block padding of ISO 32000-1 section 7.6.2, streamed one 16-byte block at a time.
//!
//! qpdf reaches AES through `QPDFCryptoImpl::rijndael_init`/`rijndael_process`
//! (`libqpdf/qpdf/Pl_AES_PDF.hh:47`), a provider abstraction this crate replaces
//! with the `aes`/`cbc` crates directly — CLAUDE.md deviation class (B), the
//! same substitution `docs/qpdf-correspondence.md` already records for the
//! crypto provider. The block-at-a-time call shape is preserved: one
//! `rijndael_process` per 16 bytes, with the chaining state living in the
//! cipher rather than beside it.

use super::{Pipeline, PipelineError, PipelineResult};
use aes::cipher::{
    BlockCipherDecrypt, BlockCipherEncrypt, BlockModeDecrypt, BlockModeEncrypt, KeyInit, KeyIvInit,
};
use aes::{Aes128, Aes256};
use std::sync::atomic::{AtomicBool, Ordering};

/// qpdf `QPDFCryptoImpl::rijndael_buf_size` (`Pl_AES_PDF.hh:44`).
const BUF_SIZE: usize = 16;

/// qpdf `Pl_AES_PDF::use_static_iv` (`libqpdf/Pl_AES_PDF.cc:11`), a *static*
/// member rather than a per-instance flag: the PDF writer turns it on
/// (`libqpdf/QPDFWriter.cc:296`) with no handle on the individual stages
/// created inside `QPDF_encryption`, so the switch has to be reachable
/// without one. Documented "for testing only" in qpdf's own header
/// (`libqpdf/qpdf/Pl_AES_PDF.hh:36-37`).
static USE_STATIC_IV: AtomicBool = AtomicBool::new(false);

/// The fixed vector qpdf's `--static-aes-iv` produces: `14 * (1 + i)`
/// (`libqpdf/Pl_AES_PDF.cc:136-139`). CBC writes it at the head of the
/// ciphertext, so it is part of the output bytes rather than an internal
/// detail — which is why the writer, whose own encryption path does not yet
/// run through this stage, reads the value from here rather than repeating it.
pub(crate) fn static_initialization_vector() -> [u8; BUF_SIZE] {
    let mut iv = [0u8; BUF_SIZE];
    for (i, byte) in iv.iter_mut().enumerate() {
        *byte = 14u8.wrapping_mul(1 + u8::try_from(i).expect("i < BUF_SIZE"));
    }
    iv
}

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
    /// `disableCBC` leaves each block independent, with no chaining state.
    Ecb128Decrypt(Box<Aes128>),
    Ecb256Decrypt(Box<Aes256>),
    Ecb128Encrypt(Box<Aes128>),
    Ecb256Encrypt(Box<Aes256>),
}

impl Cipher {
    /// qpdf `rijndael_process` (`Pl_AES_PDF.cc:182`), one block in, one out.
    fn process(&mut self, inbuf: &[u8; BUF_SIZE], outbuf: &mut [u8; BUF_SIZE]) {
        match self {
            Self::Cbc128Decrypt(cipher) => cipher.decrypt_block_b2b(inbuf.into(), outbuf.into()),
            Self::Cbc256Decrypt(cipher) => cipher.decrypt_block_b2b(inbuf.into(), outbuf.into()),
            Self::Cbc128Encrypt(cipher) => cipher.encrypt_block_b2b(inbuf.into(), outbuf.into()),
            Self::Cbc256Encrypt(cipher) => cipher.encrypt_block_b2b(inbuf.into(), outbuf.into()),
            Self::Ecb128Decrypt(cipher) => cipher.decrypt_block_b2b(inbuf.into(), outbuf.into()),
            Self::Ecb256Decrypt(cipher) => cipher.decrypt_block_b2b(inbuf.into(), outbuf.into()),
            Self::Ecb128Encrypt(cipher) => cipher.encrypt_block_b2b(inbuf.into(), outbuf.into()),
            Self::Ecb256Encrypt(cipher) => cipher.encrypt_block_b2b(inbuf.into(), outbuf.into()),
        }
    }
}

/// qpdf `Pl_AES_PDF` (`libqpdf/qpdf/Pl_AES_PDF.hh:12-60`).
///
// The reader's `QPDF::pipeStreamData`-shaped decrypt path and the canonical
// writer stream-encryption path both use this stage. Key-wrap and string
// helpers also reuse it for their block semantics.
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

impl<'a> PlAesPdf<'a> {
    fn new(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        encrypt: bool,
        key: &[u8],
    ) -> PipelineResult<Self> {
        let key = match key.len() {
            16 | 32 => key.to_vec(),
            len if len > 16 && len != 24 => {
                // qpdf's GnuTLS and OpenSSL providers select AES-128 for every
                // key length other than 16/24/32 and configure a 16-byte
                // provider key (`QPDFCrypto_gnutls.cc:197-213`,
                // `QPDFCrypto_openssl.cc:225-241`). The raw V=5 file key is
                // retained by EncryptionState; only this provider-facing
                // projection uses its first 16 bytes.
                key[..16].to_vec()
            }
            24 => {
                // qpdf selects AES-192 here; this pipeline provides AES-128 and
                // AES-256 only.
                return Err(PipelineError::logic(
                    "Pl_AES_PDF: key must be 16 or 32 bytes (qpdf selects AES-192 for a 24-byte key, which is not provided here), got 24",
                ));
            }
            // qpdf-deviation: qpdf's providers read 16 key bytes past the end of a shorter raw key (undefined contents); reject instead of fabricating them
            len => {
                return Err(PipelineError::logic(format!(
                    "Pl_AES_PDF: key must be 16 or 32 bytes, got {len}"
                )))
            }
        };
        Ok(Self {
            identifier: identifier.into(),
            next,
            key,
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
    /// [`PipelineError`] when `key` is neither 16 nor 32 bytes and is not an
    /// overlength raw V=5 key. The latter is accepted because qpdf's crypto
    /// providers project unsupported key lengths to their AES-128 fallback;
    /// the normal PDF-facing contract remains AES-128/AES-256
    /// (`libqpdf/qpdf/Pl_AES_PDF.hh:8-9`).
    pub(crate) fn new_decrypt(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        key: &[u8],
    ) -> PipelineResult<Self> {
        Self::new(identifier, next, false, key)
    }

    /// Decrypt a whole in-memory buffer through this stage and return the
    /// plaintext.
    ///
    /// This is the shape `QPDF::decryptString` uses
    /// (`libqpdf/QPDF_encryption.cc:1011-1027`): it hands `Pl_AES_PDF` the
    /// entire stored string, leading initialization vector included, and reads
    /// the result back out of a `Pl_Buffer`. The vector is *not* split off by
    /// the caller — `flush` consumes the first input block as the vector
    /// (`libqpdf/Pl_AES_PDF.cc:169-172`), which is also why an input of one
    /// block or less yields no plaintext at all.
    ///
    /// Routing through the stage rather than a separate one-shot cipher is
    /// what keeps qpdf's tolerance for malformed ciphertext: a tail shorter
    /// than a block is zero-padded (`Pl_AES_PDF.cc:107-118`) and a trailer
    /// that does not look like padding is left in place (`:183-196`), neither
    /// of which is an error.
    ///
    /// # Errors
    ///
    /// Same key-length contract as [`Self::new_decrypt`], including qpdf's
    /// overlength raw-key provider fallback.
    pub(crate) fn decrypt_to_vec(
        identifier: impl Into<String>,
        data: &[u8],
        key: &[u8],
    ) -> PipelineResult<Vec<u8>> {
        let mut sink = super::buffer::Buffer::new("AES decryption buffer", None);
        {
            // Spelled out rather than `Self::` so the stage borrows `sink` for
            // this block alone instead of the impl's lifetime parameter.
            let mut stage = PlAesPdf::new_decrypt(identifier, &mut sink, key)?;
            stage.write(data)?;
            stage.finish()?;
        }
        sink.take_buffer()
    }

    /// Encrypt a complete buffer with a caller-supplied IV and qpdf's normal
    /// PDF padding, returning only the ciphertext. The caller adds the IV to
    /// the stored string representation when the surrounding qpdf consumer
    /// requires it.
    pub(crate) fn encrypt_to_vec_with_iv(
        identifier: impl Into<String>,
        data: &[u8],
        key: &[u8],
        iv: &[u8; 16],
    ) -> PipelineResult<Vec<u8>> {
        let mut sink = super::buffer::Buffer::new("AES encryption buffer", None);
        {
            let mut stage = PlAesPdf::new_encrypt(identifier, &mut sink, key)?;
            stage.set_iv(iv)?;
            stage.write(data)?;
            stage.finish()?;
        }
        sink.take_buffer()
    }

    /// Run qpdf's `process_with_aes` shape (`QPDF_encryption.cc:209-236`):
    /// select a zero or specified IV, disable PDF padding, feed the same input
    /// the requested number of times, and finish the one stateful stage once.
    ///
    /// This is used for V=5 key wrapping, `/Perms`, and Algorithm 2.B's
    /// repeated AES input. Keeping the loop inside the pipeline owner is
    /// important: qpdf retains CBC state across all repetitions.
    pub(crate) fn process_to_vec_without_padding(
        identifier: impl Into<String>,
        encrypt: bool,
        data: &[u8],
        key: &[u8],
        repetitions: usize,
        iv: Option<&[u8]>,
    ) -> PipelineResult<Vec<u8>> {
        let mut sink = super::buffer::Buffer::new("AES process buffer", None);
        {
            let mut stage = if encrypt {
                PlAesPdf::new_encrypt(identifier, &mut sink, key)?
            } else {
                PlAesPdf::new_decrypt(identifier, &mut sink, key)?
            };
            if let Some(iv) = iv {
                stage.set_iv(iv)?;
            } else {
                stage.use_zero_iv();
            }
            stage.disable_padding();
            for _ in 0..repetitions {
                stage.write(data)?;
            }
            stage.finish()?;
        }
        sink.take_buffer()
    }

    /// An encrypting stage, mirroring `Pl_AES_PDF(identifier, next, true, key,
    /// key_bytes)`. Unless a vector is supplied or zeroed, a fresh random
    /// initialization vector is generated and written ahead of the ciphertext.
    ///
    /// # Errors
    ///
    /// Same key-length contract as [`Self::new_decrypt`], including qpdf's
    /// overlength raw-key provider fallback.
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

    /// qpdf `Pl_AES_PDF::useZeroIV` (`Pl_AES_PDF.cc:36-40`): use an all-zero
    /// vector, which AESV3 key wrapping needs. Like a specified vector it is
    /// not written to the output.
    pub(crate) fn use_zero_iv(&mut self) {
        self.use_zero_iv = true;
    }

    /// qpdf `Pl_AES_PDF::disablePadding` (`Pl_AES_PDF.cc:42-46`): append no
    /// padding block when encrypting and strip none when decrypting, which
    /// AESV3 key wrapping needs.
    pub(crate) fn disable_padding(&mut self) {
        self.disable_padding = true;
    }

    /// qpdf `Pl_AES_PDF::disableCBC` (`Pl_AES_PDF.cc:60-64`), marked "For
    /// testing only; PDF always uses CBC" in qpdf's header
    /// (`libqpdf/qpdf/Pl_AES_PDF.hh:34-35`).
    #[cfg(test)]
    pub(crate) fn disable_cbc(&mut self) {
        self.cbc_mode = false;
    }

    /// qpdf `Pl_AES_PDF::useStaticIV` (`Pl_AES_PDF.cc:66-70`): switch every
    /// stage in the process to a fixed vector. Marked "For testing only" in
    /// qpdf's header (`libqpdf/qpdf/Pl_AES_PDF.hh:36-37`), and global for the
    /// reason [`USE_STATIC_IV`] records.
    #[cfg(test)]
    pub(crate) fn use_static_iv() {
        USE_STATIC_IV.store(true, Ordering::Relaxed);
    }

    /// qpdf `Pl_AES_PDF::initializeVector` (`Pl_AES_PDF.cc:126-143`).
    fn initialize_vector(&mut self) -> PipelineResult<()> {
        if self.use_zero_iv {
            self.cbc_block = [0; BUF_SIZE];
        } else if self.use_specified_iv {
            self.cbc_block = self.specified_iv;
        } else if USE_STATIC_IV.load(Ordering::Relaxed) {
            self.cbc_block = static_initialization_vector();
        } else {
            // qpdf `QUtil::initializeWithRandomBytes` (`:141`).
            // cov:ignore-start: a getrandom failure cannot be injected here
            getrandom::fill(&mut self.cbc_block).map_err(|error| {
                PipelineError::runtime(format!(
                    "Pl_AES_PDF: OS CSPRNG unavailable for AES IV generation: {error}"
                ))
            })?;
            // cov:ignore-end
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
            // cov:ignore-start: `first` is cleared only where the cipher is built
            return Err(PipelineError::logic(
                "AES pipeline: cipher used before initialization",
            ));
            // cov:ignore-end
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
        let key16 = || -> &[u8; 16] { self.key.as_slice().try_into().expect("checked in new") };
        let key32 = || -> &[u8; 32] { self.key.as_slice().try_into().expect("checked in new") };
        match (self.cbc_mode, self.encrypt, self.key.len()) {
            (true, false, 16) => {
                Cipher::Cbc128Decrypt(Box::new(Aes128CbcDec::new(key16().into(), iv.into())))
            }
            (true, false, _) => {
                Cipher::Cbc256Decrypt(Box::new(Aes256CbcDec::new(key32().into(), iv.into())))
            }
            (true, true, 16) => {
                Cipher::Cbc128Encrypt(Box::new(Aes128CbcEnc::new(key16().into(), iv.into())))
            }
            (true, true, _) => {
                Cipher::Cbc256Encrypt(Box::new(Aes256CbcEnc::new(key32().into(), iv.into())))
            }
            (false, false, 16) => Cipher::Ecb128Decrypt(Box::new(Aes128::new(key16().into()))),
            (false, false, _) => Cipher::Ecb256Decrypt(Box::new(Aes256::new(key32().into()))),
            (false, true, 16) => Cipher::Ecb128Encrypt(Box::new(Aes128::new(key16().into()))),
            (false, true, _) => Cipher::Ecb256Encrypt(Box::new(Aes256::new(key32().into()))),
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
mod tests_support {
    use super::{Ordering, USE_STATIC_IV};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// `USE_STATIC_IV` is process-global exactly as qpdf's is, so tests that
    /// depend on its value have to run one at a time and leave it off.
    // The guard is held for its lifetime, not read; `Drop` is what turns the
    // switch back off.
    pub(super) struct StaticIvGuard(
        #[expect(
            dead_code,
            reason = "the mutex guard is held for its Drop-based test lock"
        )]
        MutexGuard<'static, ()>,
    );

    impl Drop for StaticIvGuard {
        fn drop(&mut self) {
            USE_STATIC_IV.store(false, Ordering::Relaxed);
        }
    }

    pub(super) fn static_iv_guard() -> StaticIvGuard {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let guard = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        USE_STATIC_IV.store(false, Ordering::Relaxed);
        StaticIvGuard(guard)
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

    // `useStaticIV` is a *process-global* switch in qpdf
    // (`libqpdf/Pl_AES_PDF.cc:11`, `:66-70`), because the PDF writer sets it
    // (`libqpdf/QPDFWriter.cc:296`) without a handle on the individual stages
    // created deep inside `QPDF_encryption`. `initializeVector` then fills the
    // block with `14 * (1 + i)` (`:133-137`), and CBC writes it ahead of the
    // ciphertext, so it is observable in the output.
    #[test]
    fn the_static_vector_is_qpdfs_fourteen_times_one_plus_index() {
        let _guard = super::tests_support::static_iv_guard();
        PlAesPdf::use_static_iv();

        let mut sink = Buffer::new("ciphertext", None);
        let mut stage = PlAesPdf::new_encrypt("AES stream encryption", &mut sink, &KEY128)
            .expect("AES-128 key is a supported length");
        stage.write(PLAINTEXT).expect("write");
        stage.finish().expect("finish");

        let out = sink.take_buffer().expect("buffer");
        let expected: Vec<u8> = (0..16u8).map(|i| 14u8.wrapping_mul(1 + i)).collect();
        assert_eq!(&out[..16], expected.as_slice());
        assert_eq!(
            &out[..16],
            &[14, 28, 42, 56, 70, 84, 98, 112, 126, 140, 154, 168, 182, 196, 210, 224],
            "matches the bytes /usr/bin/qpdf 11.9.0 writes for --static-aes-iv"
        );
    }

    // `useZeroIV` zeroes the vector and, being one the reader reconstructs,
    // keeps it out of the output (`libqpdf/Pl_AES_PDF.cc:38-42`, `:126-130`,
    // `:160-163`). AESV3 key wrapping needs exactly that.
    #[test]
    fn a_zero_vector_is_not_written_to_the_output() {
        let mut sink = Buffer::new("ciphertext", None);
        let mut stage = PlAesPdf::new_encrypt("AES key wrap", &mut sink, &KEY128)
            .expect("AES-128 key is a supported length");
        stage.use_zero_iv();
        stage.write(PLAINTEXT).expect("write");
        stage.finish().expect("finish");

        let out = sink.take_buffer().expect("buffer");
        assert_eq!(out.len(), 32, "no vector prefix, one padding block");

        let mut back = Buffer::new("plaintext", None);
        let mut reverse = PlAesPdf::new_decrypt("AES key unwrap", &mut back, &KEY128)
            .expect("AES-128 key is a supported length");
        reverse.use_zero_iv();
        reverse.write(&out).expect("write");
        reverse.finish().expect("finish");
        assert_eq!(back.take_buffer().expect("buffer"), PLAINTEXT);
    }

    // `disablePadding` suppresses both the block qpdf would append when
    // encrypting and the strip it would perform when decrypting
    // (`libqpdf/Pl_AES_PDF.cc:44-48`, `:98`, `:120`). AESV3 needs it because
    // the wrapped key is already a block multiple.
    #[test]
    fn padding_can_be_disabled_in_both_directions() {
        let mut sink = Buffer::new("ciphertext", None);
        let mut stage = PlAesPdf::new_encrypt("AES key wrap", &mut sink, &KEY128)
            .expect("AES-128 key is a supported length");
        stage.use_zero_iv();
        stage.disable_padding();
        stage.write(PLAINTEXT_32).expect("write");
        stage.finish().expect("finish");

        let out = sink.take_buffer().expect("buffer");
        assert_eq!(out.len(), 32, "no padding block appended");

        let mut back = Buffer::new("plaintext", None);
        let mut reverse = PlAesPdf::new_decrypt("AES key unwrap", &mut back, &KEY128)
            .expect("AES-128 key is a supported length");
        reverse.use_zero_iv();
        reverse.disable_padding();
        reverse.write(&out).expect("write");
        reverse.finish().expect("finish");
        assert_eq!(back.take_buffer().expect("buffer"), PLAINTEXT_32);
    }

    // AES-256 travels the same path; only the key length picks the cipher
    // (`rijndael_init`'s key_bytes argument, `libqpdf/Pl_AES_PDF.cc:176-177`).
    #[test]
    fn aes_256_round_trips_through_the_generated_vector() {
        let key = [0x5au8; 32];
        let mut sink = Buffer::new("ciphertext", None);
        let mut stage = PlAesPdf::new_encrypt("AES-256 stream encryption", &mut sink, &key)
            .expect("AES-256 key is a supported length");
        stage.write(PLAINTEXT).expect("write");
        stage.finish().expect("finish");
        let out = sink.take_buffer().expect("buffer");
        assert_eq!(out.len(), 16 + 32, "generated vector, then two blocks");

        let mut back = Buffer::new("plaintext", None);
        let mut reverse = PlAesPdf::new_decrypt("AES-256 stream decryption", &mut back, &key)
            .expect("AES-256 key is a supported length");
        reverse.write(&out).expect("write");
        reverse.finish().expect("finish");
        assert_eq!(back.take_buffer().expect("buffer"), PLAINTEXT);
    }

    // Without a static, zero or specified vector the stage draws a fresh one
    // per instance (`QUtil::initializeWithRandomBytes`,
    // `libqpdf/Pl_AES_PDF.cc:140-142`), so two encryptions of the same input
    // differ in their leading block.
    #[test]
    fn a_generated_vector_differs_between_two_encryptions() {
        let _guard = super::tests_support::static_iv_guard();

        let encrypt_once = || {
            let mut sink = Buffer::new("ciphertext", None);
            let mut stage = PlAesPdf::new_encrypt("AES stream encryption", &mut sink, &KEY128)
                .expect("AES-128 key is a supported length");
            stage.write(PLAINTEXT).expect("write");
            stage.finish().expect("finish");
            sink.take_buffer().expect("buffer")
        };

        assert_ne!(encrypt_once()[..16], encrypt_once()[..16]);
    }

    // `disableCBC` drops the chaining entirely, so each block stands alone and
    // no vector is established (`libqpdf/Pl_AES_PDF.cc:60-64`, and the
    // `if (this->cbc_mode)` guard at `:155`). Two identical plaintext blocks
    // therefore encrypt to two identical ciphertext blocks -- the property
    // that makes ECB unsuitable for real data and is why qpdf marks this
    // "For testing only".
    #[test]
    fn disabling_cbc_leaves_identical_blocks_identical() {
        let mut sink = Buffer::new("ciphertext", None);
        let mut stage = PlAesPdf::new_encrypt("AES ECB", &mut sink, &KEY128)
            .expect("AES-128 key is a supported length");
        stage.disable_cbc();
        stage.disable_padding();
        stage.write(PLAINTEXT_32).expect("write");
        stage.finish().expect("finish");

        let out = sink.take_buffer().expect("buffer");
        assert_eq!(out.len(), 32, "no vector prefix and no padding block");
        assert_eq!(out[..16], out[16..], "identical blocks encrypt identically");

        let mut back = Buffer::new("plaintext", None);
        let mut reverse = PlAesPdf::new_decrypt("AES ECB", &mut back, &KEY128)
            .expect("AES-128 key is a supported length");
        reverse.disable_cbc();
        reverse.disable_padding();
        reverse.write(&out).expect("write");
        reverse.finish().expect("finish");
        assert_eq!(back.take_buffer().expect("buffer"), PLAINTEXT_32);
    }

    // qpdf's padding strip is deliberately not PKCS#7-strict
    // (`libqpdf/Pl_AES_PDF.cc:184-196`): a trailing byte larger than the block
    // size is not padding, so the block is emitted whole. flpdf's existing
    // `decrypt_padded::<Pkcs7>` helpers return an error for this input, so
    // a document qpdf accepts would be rejected on that path but not here.
    #[test]
    fn a_trailing_byte_larger_than_the_block_size_is_not_padding() {
        let mut plain = PLAINTEXT_32.to_vec();
        *plain.last_mut().expect("non-empty") = 0xff;

        let out = decrypt_unpadded_ciphertext_of(&plain);

        assert_eq!(out, plain, "255 > 16, so nothing is stripped");
    }

    // Same rule, other half: the trailing bytes have to agree with the count
    // (`libqpdf/Pl_AES_PDF.cc:187-193`).
    #[test]
    fn disagreeing_trailing_bytes_are_not_padding() {
        let mut plain = PLAINTEXT_32.to_vec();
        let len = plain.len();
        plain[len - 2] = 0x09;
        plain[len - 1] = 0x02;

        let out = decrypt_unpadded_ciphertext_of(&plain);

        assert_eq!(out, plain, "the byte before the count disagrees with it");
    }

    // qpdf `:107-118`: "we have encountered files for which the output is not a
    // multiple of the block size. In this case, pad with zeroes and hope for
    // the best." The blocks that did arrive whole must still come out intact.
    #[test]
    fn a_final_block_shorter_than_the_block_size_is_zero_padded_not_rejected() {
        let ciphertext = zero_iv_unpadded_ciphertext_of(PLAINTEXT_32);
        let mut truncated = ciphertext.clone();
        truncated.extend_from_slice(b"12345");

        let mut sink = Buffer::new("plaintext", None);
        let mut stage = PlAesPdf::new_decrypt("AES stream decryption", &mut sink, &KEY128)
            .expect("AES-128 key is a supported length");
        stage.use_zero_iv();
        stage.write(&truncated).expect("write");
        stage
            .finish()
            .expect("a short final block is tolerated, not rejected");

        let out = sink.take_buffer().expect("buffer");
        assert_eq!(&out[..PLAINTEXT_32.len()], PLAINTEXT_32);
    }

    #[test]
    fn ecb_aes_256_round_trips() {
        let key = [0xa5u8; 32];
        let mut sink = Buffer::new("ciphertext", None);
        let mut stage =
            PlAesPdf::new_encrypt("AES-256 ECB", &mut sink, &key).expect("supported key length");
        stage.disable_cbc();
        stage.disable_padding();
        stage.write(PLAINTEXT_32).expect("write");
        stage.finish().expect("finish");
        let out = sink.take_buffer().expect("buffer");
        assert_eq!(out[..16], out[16..], "identical blocks encrypt identically");

        let mut back = Buffer::new("plaintext", None);
        let mut reverse =
            PlAesPdf::new_decrypt("AES-256 ECB", &mut back, &key).expect("supported key length");
        reverse.disable_cbc();
        reverse.disable_padding();
        reverse.write(&out).expect("write");
        reverse.finish().expect("finish");
        assert_eq!(back.take_buffer().expect("buffer"), PLAINTEXT_32);
    }

    #[test]
    fn the_identifier_is_the_one_the_stage_was_built_with() {
        let mut sink = Buffer::new("ciphertext", None);
        let stage = PlAesPdf::new_encrypt("AES stream encryption", &mut sink, &KEY128)
            .expect("AES-128 key is a supported length");

        assert_eq!(stage.identifier(), "AES stream encryption");
    }

    // qpdf `:147-149` guards its own invariant with a logic_error. Nothing on
    // the public surface can reach it -- `write` and `finish` only call `flush`
    // with a full buffer -- so it is exercised directly.
    #[test]
    fn flushing_a_partial_buffer_is_a_logic_error() {
        let mut sink = Buffer::new("ciphertext", None);
        let mut stage = PlAesPdf::new_encrypt("AES stream encryption", &mut sink, &KEY128)
            .expect("AES-128 key is a supported length");
        stage.write(b"short").expect("write");

        let message = stage.flush(false).err().map(|error| error.to_string());

        assert_eq!(
            message
                .as_deref()
                .map(|m| m.contains("buffer was not full")),
            Some(true),
            "a partial buffer must not flush: {message:?}"
        );
    }

    /// Encrypt `plain` with a zero vector and no padding, so the ciphertext is
    /// exactly `plain.len()` bytes and carries no vector prefix.
    fn zero_iv_unpadded_ciphertext_of(plain: &[u8]) -> Vec<u8> {
        let mut sink = Buffer::new("ciphertext", None);
        let mut stage = PlAesPdf::new_encrypt("AES stream encryption", &mut sink, &KEY128)
            .expect("AES-128 key is a supported length");
        stage.use_zero_iv();
        stage.disable_padding();
        stage.write(plain).expect("write");
        stage.finish().expect("finish");
        sink.take_buffer().expect("buffer")
    }

    /// Round-trip `plain` through an unpadded encrypt and a *padding-stripping*
    /// decrypt, which is what puts qpdf's strip rule under test.
    fn decrypt_unpadded_ciphertext_of(plain: &[u8]) -> Vec<u8> {
        let ciphertext = zero_iv_unpadded_ciphertext_of(plain);
        let mut sink = Buffer::new("plaintext", None);
        let mut stage = PlAesPdf::new_decrypt("AES stream decryption", &mut sink, &KEY128)
            .expect("AES-128 key is a supported length");
        stage.use_zero_iv();
        stage.write(&ciphertext).expect("write");
        stage.finish().expect("finish");
        sink.take_buffer().expect("buffer")
    }

    // A 24-byte key remains unsupported by the PDF-facing stage. Raw V=5 keys
    // longer than 32 bytes are a separate qpdf provider edge case: qpdf's
    // GnuTLS/OpenSSL providers select AES-128 and consume the first 16 bytes
    // for an unsupported provider key length (`QPDFCrypto_gnutls.cc:197-213`,
    // `QPDFCrypto_openssl.cc:225-241`).
    #[test]
    fn an_overlength_provider_key_uses_its_aes128_prefix() {
        let mut overlength = KEY128.to_vec();
        overlength.extend_from_slice(&[0xa5; 24]);

        let encrypt = |key: &[u8]| {
            let mut sink = Buffer::new("ciphertext", None);
            let mut stage = PlAesPdf::new_encrypt("AES stream encryption", &mut sink, key)
                .expect("qpdf provider accepts an overlength raw key");
            stage
                .set_iv(&IV)
                .expect("a 16-byte vector is the block size");
            stage.disable_padding();
            stage.write(PLAINTEXT_32).expect("write");
            stage.finish().expect("finish");
            sink.take_buffer().expect("buffer")
        };

        assert_eq!(encrypt(&overlength), encrypt(&KEY128));

        // Any other length that is not 16/24/32 takes the same provider
        // default: a 20-byte raw key encrypts exactly like its 16-byte prefix.
        let mut twenty = KEY128.to_vec();
        twenty.extend_from_slice(&[0x5a; 4]);
        assert_eq!(encrypt(&twenty), encrypt(&KEY128));

        // Below 16 bytes qpdf's providers read past the key buffer, which has
        // no reproducible result; this port rejects such keys.
        let mut sink = Buffer::new("ciphertext", None);
        let short = PlAesPdf::new_encrypt("AES stream encryption", &mut sink, &[0u8; 8])
            .err()
            .map(|error| error.to_string());
        assert!(short.as_deref().is_some_and(|m| m.contains("got 8")));
    }

    // An unsupported key length below the qpdf provider fallback remains
    // rejected: qpdf's own header scopes this pipeline to AES-128 and AES-256
    // (`libqpdf/qpdf/Pl_AES_PDF.hh:8-9`).
    #[test]
    fn a_key_that_is_neither_128_nor_256_bits_is_rejected() {
        let mut sink = Buffer::new("ciphertext", None);

        let message = PlAesPdf::new_encrypt("AES stream encryption", &mut sink, &[0u8; 24])
            .err()
            .map(|error| error.to_string());

        assert_eq!(
            message.as_deref().map(|m| m.contains("16 or 32 bytes")),
            Some(true),
            "24 bytes is not a supported AES key length: {message:?}"
        );
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

    // ── decrypt_to_vec: the one-shot shape QPDF::decryptString uses ─────────

    fn from_hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    /// NIST SP 800-38A section F.2.1 (CBC-AES128), with one block of PDF
    /// padding appended so the stage strips it exactly as `flush` does
    /// (`libqpdf/Pl_AES_PDF.cc:183-196`).
    ///
    /// Key 2b7e151628aed2a6abf7158809cf4f3c, IV 000102030405060708090a0b0c0d0e0f,
    /// plaintext 6bc1bee22e409f96e93d7e117393172a, first ciphertext block
    /// 7649abac8119b246cee98e9b12e9197d. The trailing padding block was
    /// computed with the Python `cryptography` library, so nothing in the
    /// vector comes from flpdf itself.
    #[test]
    fn decrypt_to_vec_matches_the_nist_aes128_cbc_vector() {
        let key = from_hex("2b7e151628aed2a6abf7158809cf4f3c");
        // The stage takes the leading block as the vector, so it is prepended
        // to the ciphertext rather than passed separately.
        let input = from_hex(
            "000102030405060708090a0b0c0d0e0f\
             7649abac8119b246cee98e9b12e9197d\
             8964e0b149c10b7b682e6e39aaeb731c",
        );

        let plaintext = PlAesPdf::decrypt_to_vec("AES string decryption", &input, &key)
            .expect("AES-128 key is a supported length");

        assert_eq!(plaintext, from_hex("6bc1bee22e409f96e93d7e117393172a"));
    }

    /// NIST SP 800-38A section F.2.5 (CBC-AES256), same construction.
    #[test]
    fn decrypt_to_vec_matches_the_nist_aes256_cbc_vector() {
        let key = from_hex(
            "603deb1015ca71be2b73aef0857d7781\
             1f352c073b6108d72d9810a30914dff4",
        );
        let input = from_hex(
            "000102030405060708090a0b0c0d0e0f\
             f58c4c04d6e5f1ba779eabfb5f7bfbd6\
             485a5c81519cf378fa36d42b8547edc0",
        );

        let plaintext = PlAesPdf::decrypt_to_vec("AES string decryption", &input, &key)
            .expect("AES-256 key is a supported length");

        assert_eq!(plaintext, from_hex("6bc1bee22e409f96e93d7e117393172a"));
    }

    /// qpdf's `finish` zero-pads a ciphertext tail shorter than a block rather
    /// than failing — "pad with zeroes and hope for the best"
    /// (`libqpdf/Pl_AES_PDF.cc:107-118`). Here the payload after the vector is
    /// 30 bytes, so the stage decrypts 32 and emits all of them.
    ///
    /// Expected plaintext computed with the Python `cryptography` library over
    /// the zero-padded ciphertext, independently of this implementation.
    #[test]
    fn decrypt_to_vec_zero_pads_a_short_final_block_like_qpdf() {
        let mut input = IV.to_vec();
        input.extend(0x20u8..0x3e); // 30 bytes: not a whole number of blocks

        let plaintext = PlAesPdf::decrypt_to_vec("AES string decryption", &input, &KEY128)
            .expect("AES-128 key is a supported length");

        assert_eq!(
            plaintext,
            from_hex("965d506c8efb62bdf82304a19615e54cf7cb11273e24d150cc5864434df9f4af"),
            "a short tail is zero-padded, not rejected"
        );
    }

    /// The padding strip is skipped, not failed, when the final byte does not
    /// describe a run of equal bytes (`libqpdf/Pl_AES_PDF.cc:185-195`). This
    /// payload decrypts to a block ending in 0x2c, whose preceding bytes
    /// disagree, so all 32 bytes survive.
    ///
    /// Expected plaintext computed with the Python `cryptography` library.
    #[test]
    fn decrypt_to_vec_keeps_a_final_block_whose_trailer_is_not_padding() {
        let mut input = IV.to_vec();
        input.extend(0x40u8..0x60); // 32 bytes: two whole blocks

        let plaintext = PlAesPdf::decrypt_to_vec("AES string decryption", &input, &KEY128)
            .expect("AES-128 key is a supported length");

        assert_eq!(
            plaintext,
            from_hex("313c1f855b4becb74548099f42378521afeba453bfea2ebf7237cdf47ffe442c"),
            "an implausible padding byte leaves the block whole"
        );
    }

    /// An input of one block or less is entirely consumed as the
    /// initialization vector (`libqpdf/Pl_AES_PDF.cc:169-172`), leaving no
    /// ciphertext to decrypt. A short one is zero-padded to a block first.
    #[test]
    fn decrypt_to_vec_yields_nothing_for_an_input_of_one_block_or_less() {
        for len in [0usize, 1, 15, 16] {
            let input = vec![0x5au8; len];
            let plaintext = PlAesPdf::decrypt_to_vec("AES string decryption", &input, &KEY128)
                .expect("AES-128 key is a supported length");
            assert!(
                plaintext.is_empty(),
                "{len} bytes is at most the vector, so no plaintext follows"
            );
        }
    }

    /// The one-shot inherits `new_decrypt`'s key-length contract.
    #[test]
    fn decrypt_to_vec_rejects_an_unsupported_key_length() {
        let error = PlAesPdf::decrypt_to_vec("AES string decryption", &[0u8; 32], &[0u8; 24])
            .expect_err("24 bytes is not a supported AES key length");

        assert!(
            error.to_string().contains("16 or 32 bytes"),
            "unexpected message: {error}"
        );
    }
}

//! qpdf correspondence: Pl_SHA2.cc reusable streaming SHA-256/384/512 digest with next-pipeline passthrough.
//
// `Pl_SHA2::write` in qpdf 11.9.0 only flips `in_progress`; unlike `Pl_MD5::write` it
// never re-initializes the crypto object on reuse after `finish()`. With `bits=0` (the
// uncommitted default) the crypto pointer is never set at all. Both paths dereference a
// null/stale C++ object — undefined behavior qpdf's own `libtests/sha2.cc` never
// exercises (it always calls `resetBits` before each use). There is no oracle-observed
// byte sequence for either path, so this port converts both into a defined logic error
// instead of guessing at unverified C++ UB. `resetBits` remains the one supported way
// to (re)commit to a digest size, matching qpdf's own test usage.
use super::{Pipeline, PipelineError, PipelineResult};
use sha2::{Digest, Sha256, Sha384, Sha512};

enum Sha2Digest {
    Bits256(Sha256),
    Bits384(Sha384),
    Bits512(Sha512),
}

impl Sha2Digest {
    fn new(bits: i32) -> PipelineResult<Self> {
        match bits {
            256 => Ok(Self::Bits256(Sha256::new())),
            384 => Ok(Self::Bits384(Sha384::new())),
            512 => Ok(Self::Bits512(Sha512::new())),
            _ => Err(PipelineError::logic(
                "SHA2_native has bits != 256, 384, or 512",
            )),
        }
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            Self::Bits256(hasher) => Digest::update(hasher, data),
            Self::Bits384(hasher) => Digest::update(hasher, data),
            Self::Bits512(hasher) => Digest::update(hasher, data),
        }
    }

    fn finalize(self) -> Vec<u8> {
        match self {
            Self::Bits256(hasher) => hasher.finalize().to_vec(),
            Self::Bits384(hasher) => hasher.finalize().to_vec(),
            Self::Bits512(hasher) => hasher.finalize().to_vec(),
        }
    }
}

pub(crate) struct PlSha2<'a> {
    identifier: String,
    next: Option<&'a mut dyn Pipeline>,
    digest: Option<Sha2Digest>,
    raw_digest: Option<Vec<u8>>,
    in_progress: bool,
}

#[allow(dead_code)]
impl<'a> PlSha2<'a> {
    pub(crate) fn new(
        identifier: impl Into<String>,
        next: Option<&'a mut dyn Pipeline>,
        bits: i32,
    ) -> PipelineResult<Self> {
        let mut sha2 = Self {
            identifier: identifier.into(),
            next,
            digest: None,
            raw_digest: None,
            in_progress: false,
        };
        if bits != 0 {
            sha2.reset_bits(bits)?;
        }
        Ok(sha2)
    }

    pub(crate) fn reset_bits(&mut self, bits: i32) -> PipelineResult<()> {
        if self.in_progress {
            return Err(PipelineError::logic(
                "bit reset requested for in-progress SHA2 Pipeline",
            ));
        }
        self.digest = Some(Sha2Digest::new(bits)?);
        self.raw_digest = None;
        Ok(())
    }

    pub(crate) fn get_raw_digest(&self) -> PipelineResult<&[u8]> {
        if self.in_progress {
            return Err(PipelineError::logic(
                "digest requested for in-progress SHA2 Pipeline",
            ));
        }
        let identifier = &self.identifier;
        self.raw_digest.as_deref().ok_or_else(|| {
            PipelineError::logic(format!(
                "{identifier}: Pl_SHA2: digest requested before finish() computed one"
            ))
        })
    }

    pub(crate) fn get_hex_digest(&self) -> PipelineResult<String> {
        self.get_raw_digest().map(hex::encode)
    }
}

impl Pipeline for PlSha2<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        let identifier = &self.identifier;
        let digest = self.digest.as_mut().ok_or_else(|| {
            PipelineError::logic(format!(
                "{identifier}: Pl_SHA2: write() called before resetBits() selected a digest size"
            ))
        })?;
        self.in_progress = true;
        digest.update(data);
        if let Some(next) = self.next.as_deref_mut() {
            next.write(data)?;
        }
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        if let Some(next) = self.next.as_deref_mut() {
            next.finish()?;
        }
        if let Some(digest) = self.digest.take() {
            self.raw_digest = Some(digest.finalize());
            self.in_progress = false;
            return Ok(());
        }
        if self.raw_digest.is_some() {
            // Already finished. qpdf's `Pl_SHA2::finish` has no already-finished
            // guard either (unlike `Pl_RC4::finish`, which is intentionally
            // reusable) — it just re-forwards to `next` and returns. Matching that
            // exactly means leaving the existing digest untouched rather than
            // erroring, since the pipeline genuinely was committed and finished.
            return Ok(());
        }
        let identifier = &self.identifier;
        Err(PipelineError::logic(format!(
            "{identifier}: Pl_SHA2: finish() called before resetBits() selected a digest size"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::PlSha2;
    use crate::pipeline::{Pipeline, PipelineError, PipelineResult};

    // ── digest vectors (qpdf `libtests/sha2.cc`, cross-checked against NIST FIPS 180-4) ──

    fn digest_of(bits: i32, input: &[u8]) -> String {
        let mut sha2 = PlSha2::new("sha2", None, bits).unwrap();
        sha2.write(input).unwrap();
        sha2.finish().unwrap();
        sha2.get_hex_digest().unwrap()
    }

    #[test]
    fn sha256_short_vector_matches_fips_and_qpdf() {
        assert_eq!(
            digest_of(256, b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_long_vector_matches_qpdf() {
        assert_eq!(
            digest_of(
                256,
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            ),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn sha256_million_a_matches_qpdf() {
        let input = vec![b'a'; 1_000_000];
        assert_eq!(
            digest_of(256, &input),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn sha384_short_vector_matches_fips_and_qpdf() {
        assert_eq!(
            digest_of(384, b"abc"),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded163\
             1a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
        );
    }

    #[test]
    fn sha384_long_vector_matches_qpdf() {
        assert_eq!(
            digest_of(
                384,
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmn\
                  hijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"
            ),
            "09330c33f71147e83d192fc782cd1b4753111b173b3b05d2\
             2fa08086e3b0f712fcc7c71a557e2db966c3e9fa91746039"
        );
    }

    #[test]
    fn sha384_million_a_matches_qpdf() {
        let input = vec![b'a'; 1_000_000];
        assert_eq!(
            digest_of(384, &input),
            "9d0e1809716474cb086e834e310a4a1ced149e9c00f24852\
             7972cec5704c2a5b07b8b3dc38ecc4ebae97ddd87f3d8985"
        );
    }

    #[test]
    fn sha512_short_vector_matches_fips_and_qpdf() {
        assert_eq!(
            digest_of(512, b"abc"),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }

    #[test]
    fn sha512_long_vector_matches_qpdf() {
        assert_eq!(
            digest_of(
                512,
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmn\
                  hijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"
            ),
            "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018\
             501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909"
        );
    }

    #[test]
    fn sha512_million_a_matches_qpdf() {
        let input = vec![b'a'; 1_000_000];
        assert_eq!(
            digest_of(512, &input),
            "e718483d0ce769644e2e42c7bc15b4638e1f98b13b2044285632a803afa973eb\
             de0ff244877ea60a4cb0432ce577c31beb009c5c2c49aa2e4eadb217ad8cc09b"
        );
    }

    // ── identifier and passthrough ──────────────────────────────────────────

    #[derive(Default)]
    struct RecordingSink {
        chunks: Vec<Vec<u8>>,
        finishes: usize,
    }

    impl Pipeline for RecordingSink {
        fn identifier(&self) -> &str {
            "recording"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.chunks.push(data.to_vec());
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.finishes += 1;
            Ok(())
        }
    }

    #[test]
    fn identifier_matches_the_constructed_value() {
        let sha2 = PlSha2::new("pl-sha2-stage", None, 256).unwrap();
        assert_eq!(Pipeline::identifier(&sha2), "pl-sha2-stage");
    }

    /// A `PlSha2` can itself be another `PlSha2`'s `next`, matching qpdf's stackable
    /// `Pipeline` design (any pipeline is valid downstream of any other).
    #[test]
    fn a_sha2_pipeline_can_be_chained_as_another_ones_next() {
        let mut inner = PlSha2::new("inner", None, 256).unwrap();
        {
            let mut outer =
                PlSha2::new("outer", Some(&mut inner as &mut dyn Pipeline), 384).unwrap();
            outer.write(b"abc").unwrap();
            outer.finish().unwrap();
        }
        assert_eq!(
            inner.get_hex_digest().unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn write_forwards_unmodified_bytes_to_next_once_per_call() {
        let mut sink = RecordingSink::default();
        let hex_digest;
        {
            let mut sha2 = PlSha2::new("sha2", Some(&mut sink as &mut dyn Pipeline), 256).unwrap();
            sha2.write(b"hello ").unwrap();
            sha2.write(b"world").unwrap();
            sha2.finish().unwrap();
            hex_digest = sha2.get_hex_digest().unwrap();
        }
        // Two separate write() calls forward as two separate downstream writes...
        assert_eq!(sink.chunks, vec![b"hello ".to_vec(), b"world".to_vec()]);
        // ...but the digest still accumulates across both, matching a single-shot hash.
        assert_eq!(hex_digest, digest_of(256, b"hello world"));
    }

    #[test]
    fn finish_forwards_to_next_before_finalizing_the_digest() {
        let mut sink = RecordingSink::default();
        {
            let mut sha2 = PlSha2::new("sha2", Some(&mut sink as &mut dyn Pipeline), 256).unwrap();
            sha2.write(b"abc").unwrap();
            sha2.finish().unwrap();
        }
        assert_eq!(sink.finishes, 1);
    }

    struct FinishFaultSink;

    impl Pipeline for FinishFaultSink {
        fn identifier(&self) -> &str {
            "finish-fault"
        }

        fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            Err(PipelineError::logic(
                "finish-fault: downstream finish failed",
            ))
        }
    }

    #[test]
    fn finish_error_from_next_leaves_digest_unfinalized_and_in_progress() {
        let mut fault = FinishFaultSink;
        let mut sha2 = PlSha2::new("sha2", Some(&mut fault as &mut dyn Pipeline), 256).unwrap();
        sha2.write(b"abc").unwrap();

        let error = sha2.finish().unwrap_err();
        assert_eq!(error.to_string(), "finish-fault: downstream finish failed");

        // Ordering proof: `next.finish()` runs before `crypto.finalize()` in qpdf's
        // `Pl_SHA2::finish`. Since `next.finish()` failed, the digest must still be
        // unfinalized and `in_progress` must still be true.
        let digest_error = sha2.get_raw_digest().unwrap_err();
        assert_eq!(
            digest_error.to_string(),
            "digest requested for in-progress SHA2 Pipeline"
        );
    }

    /// qpdf's `Pl_SHA2::finish` has no already-finished guard: a second `finish()`
    /// just re-forwards to `next` and returns normally, leaving the digest as
    /// computed by the first `finish()` (mirrors `pipeline/rc4.rs`'s
    /// `repeated_finish_propagates_each_time_and_marks_stage_finished`).
    #[test]
    fn repeated_finish_forwards_each_time_and_keeps_the_first_digest() {
        let mut sink = RecordingSink::default();
        let hex_digest;
        {
            let mut sha2 = PlSha2::new("sha2", Some(&mut sink as &mut dyn Pipeline), 256).unwrap();
            sha2.write(b"abc").unwrap();
            sha2.finish().unwrap();
            sha2.finish().unwrap();
            hex_digest = sha2.get_hex_digest().unwrap();
        }
        assert_eq!(sink.finishes, 2);
        assert_eq!(
            hex_digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    struct WriteFaultSink;

    impl Pipeline for WriteFaultSink {
        fn identifier(&self) -> &str {
            "write-fault"
        }

        fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
            Err(PipelineError::runtime(
                "write-fault: downstream write failed",
            ))
        }

        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
    }

    #[test]
    fn downstream_write_error_is_returned_unchanged() {
        let mut fault = WriteFaultSink;
        let mut sha2 = PlSha2::new("sha2", Some(&mut fault as &mut dyn Pipeline), 256).unwrap();
        let error = sha2.write(b"x").unwrap_err();
        assert!(matches!(error, PipelineError::Runtime(_)));
        assert_eq!(error.to_string(), "write-fault: downstream write failed");
    }

    #[test]
    fn helper_sink_identifiers_and_noop_halves_are_exercised() {
        assert_eq!(RecordingSink::default().identifier(), "recording");

        assert_eq!(FinishFaultSink.identifier(), "finish-fault");
        // The half of `FinishFaultSink` that no test above exercises: `write()` is a no-op.
        FinishFaultSink.write(b"ignored").unwrap();

        assert_eq!(WriteFaultSink.identifier(), "write-fault");
        // The half of `WriteFaultSink` that no test above exercises: `finish()` is a no-op.
        WriteFaultSink.finish().unwrap();
    }

    /// qpdf FIPS 180-4 empty-string digest. `Pl_SHA2::finish` finalizes unconditionally
    /// (no `in_progress` check), so a committed pipeline that never received a `write()`
    /// must still produce the hash of zero bytes.
    #[test]
    fn finish_with_no_prior_write_computes_the_empty_input_digest() {
        let mut sha2 = PlSha2::new("sha2", None, 256).unwrap();
        sha2.finish().unwrap();
        assert_eq!(
            sha2.get_hex_digest().unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// qpdf's `Pl_SHA2::write` does not special-case `len == 0`: it unconditionally sets
    /// `in_progress` and unconditionally forwards to `next`, unlike e.g. `Pl_Count`/`Pl_RC4`
    /// in this codebase which skip empty writes.
    #[test]
    fn empty_write_still_sets_in_progress_and_forwards_an_empty_chunk() {
        let mut sink = RecordingSink::default();
        {
            let mut sha2 = PlSha2::new("sha2", Some(&mut sink as &mut dyn Pipeline), 256).unwrap();
            sha2.write(b"").unwrap();
            let in_progress_error = sha2.get_raw_digest().unwrap_err();
            assert_eq!(
                in_progress_error.to_string(),
                "digest requested for in-progress SHA2 Pipeline"
            );
            sha2.finish().unwrap();
        }
        assert_eq!(sink.chunks, vec![Vec::<u8>::new()]);
    }

    #[test]
    fn no_next_pipeline_still_computes_a_digest() {
        let mut sha2 = PlSha2::new("sha2", None, 256).unwrap();
        sha2.write(b"abc").unwrap();
        sha2.finish().unwrap();
        assert_eq!(
            sha2.get_hex_digest().unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    // ── in-progress guards (qpdf `Pl_SHA2::resetBits` / `getRawDigest`) ─────

    #[test]
    fn reset_bits_while_in_progress_is_rejected() {
        let mut sha2 = PlSha2::new("sha2", None, 256).unwrap();
        sha2.write(b"abc").unwrap();
        let error = sha2.reset_bits(256).unwrap_err();
        assert_eq!(
            error.to_string(),
            "bit reset requested for in-progress SHA2 Pipeline"
        );
    }

    #[test]
    fn raw_digest_while_in_progress_is_rejected() {
        let mut sha2 = PlSha2::new("sha2", None, 256).unwrap();
        sha2.write(b"abc").unwrap();
        let error = sha2.get_raw_digest().unwrap_err();
        assert_eq!(
            error.to_string(),
            "digest requested for in-progress SHA2 Pipeline"
        );
    }

    #[test]
    fn hex_digest_while_in_progress_is_rejected() {
        let mut sha2 = PlSha2::new("sha2", None, 256).unwrap();
        sha2.write(b"abc").unwrap();
        let error = sha2.get_hex_digest().unwrap_err();
        assert_eq!(
            error.to_string(),
            "digest requested for in-progress SHA2 Pipeline"
        );
    }

    #[test]
    fn invalid_bit_length_is_rejected_at_construction() {
        let error = PlSha2::new("sha2", None, 128)
            .err()
            .expect("bits=128 must be rejected");
        assert_eq!(
            error.to_string(),
            "SHA2_native has bits != 256, 384, or 512"
        );
    }

    #[test]
    fn invalid_bit_length_is_rejected_by_reset_bits() {
        let mut sha2 = PlSha2::new("sha2", None, 256).unwrap();
        sha2.write(b"abc").unwrap();
        sha2.finish().unwrap();
        let error = sha2.reset_bits(1).unwrap_err();
        assert_eq!(
            error.to_string(),
            "SHA2_native has bits != 256, 384, or 512"
        );
    }

    // ── reuse across resetBits cycles (mirrors qpdf's own `libtests/sha2.cc`) ─

    #[test]
    fn same_pipeline_is_reusable_across_reset_bits_cycles_with_different_lengths() {
        let mut sha2 = PlSha2::new("sha2", None, 0).unwrap();

        sha2.reset_bits(256).unwrap();
        sha2.write(b"abc").unwrap();
        sha2.finish().unwrap();
        assert_eq!(
            sha2.get_hex_digest().unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );

        sha2.reset_bits(384).unwrap();
        sha2.write(b"abc").unwrap();
        sha2.finish().unwrap();
        assert_eq!(
            sha2.get_hex_digest().unwrap(),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded163\
             1a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
        );
    }

    // ── uncommitted-pipeline safety (qpdf leaves `bits=0` uncommitted; the C++ crypto
    //    pointer is null and `write()`/`finish()` would dereference it. Converting that
    //    crash into a defined logic error is the mandatory Rust-safety translation of
    //    C++ UB — see module doc.) ─────────────────────────────────────────────────

    #[test]
    fn write_before_reset_bits_is_rejected() {
        let mut sha2 = PlSha2::new("sha2-stage", None, 0).unwrap();
        let error = sha2.write(b"abc").unwrap_err();
        assert_eq!(
            error.to_string(),
            "sha2-stage: Pl_SHA2: write() called before resetBits() selected a digest size"
        );
    }

    #[test]
    fn finish_before_reset_bits_is_rejected() {
        let mut sha2 = PlSha2::new("sha2-stage", None, 0).unwrap();
        let error = sha2.finish().unwrap_err();
        assert_eq!(
            error.to_string(),
            "sha2-stage: Pl_SHA2: finish() called before resetBits() selected a digest size"
        );
    }

    #[test]
    fn digest_before_finish_is_rejected() {
        let sha2 = PlSha2::new("sha2-stage", None, 256).unwrap();
        let error = sha2.get_raw_digest().unwrap_err();
        assert_eq!(
            error.to_string(),
            "sha2-stage: Pl_SHA2: digest requested before finish() computed one"
        );
    }

    #[test]
    fn write_after_finish_without_reset_bits_is_rejected() {
        let mut sha2 = PlSha2::new("sha2-stage", None, 256).unwrap();
        sha2.write(b"abc").unwrap();
        sha2.finish().unwrap();
        let error = sha2.write(b"more").unwrap_err();
        assert_eq!(
            error.to_string(),
            "sha2-stage: Pl_SHA2: write() called before resetBits() selected a digest size"
        );
    }
}

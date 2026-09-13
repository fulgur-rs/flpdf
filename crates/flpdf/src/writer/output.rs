//! qpdf correspondence: QPDFWriter.cc pipeline ownership, Pl_Count accepted-byte accounting, PipelinePopper segment scopes, and deterministic-ID digest boundaries.

use crate::{Error, Result};
use md5::{Digest as _, Md5};
use std::io::{self, ErrorKind, Write};
use std::ops::Range;

/// Final-output destination for the writer's counted pipeline boundary.
///
/// This separates short-write handling from document lifecycle: a producer may
/// finish one filtered segment without also finalizing the document output.
pub(crate) trait OutputTarget {
    fn write_chunk(&mut self, bytes: &[u8]) -> io::Result<usize>;
    fn finish_segment(&mut self) -> Result<()>;
    fn finish_document(&mut self) -> Result<()>;

    /// Patch an equal-width region after all forward output has been emitted.
    ///
    /// qpdf's linearization pass writes its xref regions forward and does not
    /// need this operation. The final flpdf linearization pass retains a Vec so
    /// its existing fixed-width back-patches can remain local to that buffer.
    fn patch_bytes(&mut self, _range: Range<usize>, _bytes: &[u8]) -> Result<()> {
        Err(Error::Unsupported(
            "output target does not support in-place patching".to_string(),
        ))
    }
}

/// Target adapter for bounded writer-owned buffers.
///
/// Linearization and length-before-payload serializers keep local `Vec`
/// ownership, but still enter the canonical serializer through an
/// [`OutputSink`]. Final-output coordinates are therefore counted only by the
/// outer sink when the completed local payload is consumed.
impl OutputTarget for Vec<u8> {
    fn write_chunk(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn finish_segment(&mut self) -> Result<()> {
        Ok(())
    }

    fn finish_document(&mut self) -> Result<()> {
        Ok(())
    }

    fn patch_bytes(&mut self, range: Range<usize>, bytes: &[u8]) -> Result<()> {
        if range.start > range.end || range.end > self.len() {
            return Err(Error::Unsupported(
                "writer output patch range is out of bounds".to_string(),
            ));
        }
        if range.len() != bytes.len() {
            return Err(Error::Unsupported(
                "writer output patch changes byte width".to_string(),
            ));
        }
        self[range].copy_from_slice(bytes);
        Ok(())
    }
}

/// Run one canonical serializer operation against a bounded local buffer.
pub(crate) fn with_buffer_sink<T>(
    bytes: &mut Vec<u8>,
    write: impl FnOnce(&mut OutputSink<'_>) -> Result<T>,
) -> Result<T> {
    let position = u64::try_from(bytes.len())
        .map_err(|_| Error::Unsupported("writer buffer position exceeds u64 range".to_string()))?;
    let mut sink = OutputSink::new_at_position(bytes, position);
    write(&mut sink)
}

/// qpdf-shaped final-output counter and optional deterministic-ID digest.
///
/// Only bytes accepted by the final target advance this counter or digest.
/// Local stream, object-stream, and xref buffers deliberately remain outside
/// this boundary.
pub(crate) struct OutputSink<'a> {
    target: &'a mut dyn OutputTarget,
    position: u64,
    last_byte: Option<u8>,
    digest: DigestState,
}

enum DigestState {
    Disabled,
    Active(Md5),
    Suspended(Md5),
}

enum WriteFailure {
    Io(io::Error),
    PositionOverflow,
}

impl<'a> OutputSink<'a> {
    pub(crate) fn new(target: &'a mut dyn OutputTarget) -> Self {
        Self::new_at_position(target, 0)
    }

    fn new_at_position(target: &'a mut dyn OutputTarget, position: u64) -> Self {
        Self {
            target,
            position,
            last_byte: None,
            digest: DigestState::Disabled,
        }
    }

    /// Write all bytes while counting only the portion accepted by the target.
    pub(crate) fn write_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.write_bytes_inner(bytes).map_err(|error| match error {
            WriteFailure::Io(error) => Error::Io(error),
            WriteFailure::PositionOverflow => {
                Error::Unsupported("writer output position exceeds u64 range".to_string())
            }
        })
    }

    pub(crate) const fn position(&self) -> u64 {
        self.position
    }

    pub(crate) fn position_usize(&self) -> Result<usize> {
        usize::try_from(self.position).map_err(|_| {
            Error::Unsupported("writer output position exceeds usize range".to_string())
        })
    }

    pub(crate) const fn last_byte(&self) -> Option<u8> {
        self.last_byte
    }

    /// Begin the final-output MD5 used to derive a deterministic trailer ID.
    pub(crate) fn begin_digest(&mut self) {
        self.digest = DigestState::Active(Md5::new());
    }

    /// Stop digesting immediately after the final `/ID [` marker.
    pub(crate) fn suspend_digest(&mut self) {
        self.digest = match std::mem::replace(&mut self.digest, DigestState::Disabled) {
            DigestState::Active(digest) => DigestState::Suspended(digest),
            state => state,
        };
    }

    /// Return the final-output digest after the caller has reached its cutoff.
    pub(crate) fn take_digest(&mut self) -> Result<[u8; 16]> {
        let digest = match std::mem::replace(&mut self.digest, DigestState::Disabled) {
            DigestState::Active(digest) | DigestState::Suspended(digest) => digest,
            DigestState::Disabled => {
                return Err(Error::Internal(
                    "digest requested for a disabled MD5 Pipeline".to_string(),
                ));
            }
        };
        let digest = digest.finalize();
        let mut result = [0; 16];
        result.copy_from_slice(&digest);
        Ok(result)
    }

    pub(crate) fn finish_segment(&mut self) -> Result<()> {
        self.target.finish_segment()
    }

    pub(crate) fn finish_document(&mut self) -> Result<()> {
        self.target.finish_document()
    }

    /// Apply a fixed-width patch owned by the output target.
    pub(crate) fn patch_bytes(&mut self, range: Range<usize>, bytes: &[u8]) -> Result<()> {
        self.target.patch_bytes(range, bytes)
    }

    fn write_bytes_inner(&mut self, mut bytes: &[u8]) -> std::result::Result<(), WriteFailure> {
        while !bytes.is_empty() {
            let written = match self.target.write_chunk(bytes) {
                Ok(0) => {
                    return Err(WriteFailure::Io(io::Error::new(
                        ErrorKind::WriteZero,
                        "failed to write whole buffer",
                    )));
                }
                Ok(written) if written <= bytes.len() => written,
                Ok(_) => {
                    return Err(WriteFailure::Io(io::Error::new(
                        ErrorKind::InvalidData,
                        "writer output target reported more bytes than provided",
                    )));
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(WriteFailure::Io(error)),
            };

            let written_u64 = u64::try_from(written).map_err(|_| WriteFailure::PositionOverflow)?;
            let position = self
                .position
                .checked_add(written_u64)
                .ok_or(WriteFailure::PositionOverflow)?;
            let accepted = &bytes[..written];
            if let DigestState::Active(digest) = &mut self.digest {
                digest.update(accepted);
            }
            self.position = position;
            self.last_byte = accepted.last().copied();
            bytes = &bytes[written..];
        }
        Ok(())
    }
}

impl Write for OutputSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.write_bytes_inner(bytes).map_err(|error| match error {
            WriteFailure::Io(error) => error,
            WriteFailure::PositionOverflow => {
                io::Error::other("writer output position exceeds u64 range")
            }
        })?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Build the qpdf `generateID` second-MD5 seed from the final-output digest.
///
/// `info_suffix` is the writer's existing raw decoded `/Info` string suffix.
/// qpdf passes this seed through `MD5::encodeString`, so a NUL in an `/Info`
/// value terminates the C string before the second MD5 is calculated.
pub(crate) fn deterministic_id_second_seed(
    output_digest: &[u8; 16],
    info_suffix: &[u8],
) -> Result<Vec<u8>> {
    let suffix_len = info_suffix
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(info_suffix.len());
    let capacity = 32_usize
        .checked_add(b" QPDF ".len())
        .and_then(|length| length.checked_add(suffix_len))
        .ok_or_else(|| Error::Unsupported("deterministic ID seed is too large".to_string()))?;
    let mut seed = Vec::with_capacity(capacity);
    for byte in output_digest {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        seed.push(HEX[(byte >> 4) as usize]);
        seed.push(HEX[(byte & 0x0f) as usize]);
    }
    seed.extend_from_slice(b" QPDF ");
    seed.extend_from_slice(&info_suffix[..suffix_len]);
    Ok(seed)
}

#[cfg(test)]
mod tests {
    use super::{OutputSink, OutputTarget};
    use crate::{Error, Result};
    use md5::Digest as _;
    use std::collections::VecDeque;
    use std::io::{self, ErrorKind, Write as _};

    #[derive(Default)]
    struct VecOutputTarget {
        bytes: Vec<u8>,
        writes: VecDeque<io::Result<usize>>,
        segment_finishes: usize,
        document_finishes: usize,
        segment_error: Option<Error>,
        document_error: Option<Error>,
    }

    impl OutputTarget for VecOutputTarget {
        fn write_chunk(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let written = self.writes.pop_front().unwrap_or(Ok(bytes.len()))?;
            self.bytes.extend_from_slice(&bytes[..written]);
            Ok(written)
        }

        fn finish_segment(&mut self) -> Result<()> {
            self.segment_finishes += 1;
            self.segment_error.take().map_or(Ok(()), Err)
        }

        fn finish_document(&mut self) -> Result<()> {
            self.document_finishes += 1;
            self.document_error.take().map_or(Ok(()), Err)
        }
    }

    #[test]
    fn writes_short_chunks_and_counts_only_accepted_bytes() {
        let mut target = VecOutputTarget {
            writes: VecDeque::from([Ok(2), Ok(1), Ok(2)]),
            ..Default::default()
        };
        let mut sink = OutputSink::new(&mut target);

        sink.write_bytes(b"abcde").expect("short writes complete");

        assert_eq!(sink.position(), 5);
        assert_eq!(sink.last_byte(), Some(b'e'));
        drop(sink);
        assert_eq!(target.bytes, b"abcde");
    }

    #[test]
    fn retries_interrupted_write_without_advancing_position() {
        let mut target = VecOutputTarget {
            writes: VecDeque::from([Err(io::Error::from(ErrorKind::Interrupted)), Ok(3)]),
            ..Default::default()
        };
        let mut sink = OutputSink::new(&mut target);

        sink.write_bytes(b"abc").expect("interrupted write retries");

        assert_eq!(sink.position(), 3);
        assert_eq!(sink.last_byte(), Some(b'c'));
    }

    #[test]
    fn rejects_zero_progress_write() {
        let mut target = VecOutputTarget {
            writes: VecDeque::from([Ok(0)]),
            ..Default::default()
        };
        let mut sink = OutputSink::new(&mut target);

        let error = sink.write_bytes(b"a").expect_err("zero write fails");

        assert!(matches!(error, Error::Io(error) if error.kind() == ErrorKind::WriteZero));
        assert_eq!(sink.position(), 0);
        assert_eq!(sink.last_byte(), None);
    }

    #[test]
    fn rejects_a_target_that_overreports_accepted_bytes() {
        struct OverreportingTarget;

        impl OutputTarget for OverreportingTarget {
            fn write_chunk(&mut self, bytes: &[u8]) -> io::Result<usize> {
                Ok(bytes.len() + 1)
            }

            fn finish_segment(&mut self) -> Result<()> {
                Ok(())
            }

            fn finish_document(&mut self) -> Result<()> {
                Ok(())
            }
        }

        let mut target = OverreportingTarget;
        OutputTarget::finish_segment(&mut target)
            .expect("overreporting test target segment finish");
        OutputTarget::finish_document(&mut target)
            .expect("overreporting test target document finish");
        let mut sink = OutputSink::new(&mut target);
        let error = sink
            .write_bytes(b"abc")
            .expect_err("an impossible target byte count must be rejected");

        assert!(matches!(error, Error::Io(error) if error.kind() == ErrorKind::InvalidData));
        assert_eq!(sink.position(), 0);
        assert_eq!(sink.last_byte(), None);
    }

    #[test]
    fn rejects_position_overflow_without_updating_last_byte() {
        let mut target = VecOutputTarget::default();
        let mut sink = OutputSink::new(&mut target);
        sink.position = u64::MAX;

        let error = sink.write_bytes(b"a").expect_err("position overflow fails");

        assert!(matches!(error, Error::Unsupported(message) if message.contains("position")));
        assert_eq!(sink.position(), u64::MAX);
        assert_eq!(sink.last_byte(), None);
    }

    #[test]
    fn segment_and_document_finishes_are_distinct() {
        let mut target = VecOutputTarget::default();
        let mut sink = OutputSink::new(&mut target);

        sink.finish_segment().expect("segment finish");
        sink.finish_document().expect("document finish");

        drop(sink);
        assert_eq!(target.segment_finishes, 1);
        assert_eq!(target.document_finishes, 1);
    }

    #[test]
    fn reports_target_finish_failure() {
        let mut target = VecOutputTarget {
            document_error: Some(Error::System("document finish failed".into())),
            ..Default::default()
        };
        let mut sink = OutputSink::new(&mut target);

        let error = sink.finish_document().expect_err("document finish fails");

        assert!(matches!(error, Error::System(message) if message == "document finish failed"));
    }

    #[test]
    fn bounded_buffer_target_keeps_segment_and_document_finish_non_terminal() {
        let mut bytes = b"prefix".to_vec();
        let mut sink = OutputSink::new(&mut bytes);

        sink.finish_segment()
            .expect("buffer segment finish is a no-op");
        sink.finish_document()
            .expect("buffer document finish is a no-op");
        sink.write_bytes(b"-suffix")
            .expect("buffer remains writable after local finishes");

        drop(sink);
        assert_eq!(bytes, b"prefix-suffix");
    }

    #[test]
    fn vec_target_accepts_only_fixed_width_in_place_patches() {
        let mut bytes = b"abcdef".to_vec();
        let mut sink = OutputSink::new(&mut bytes);

        sink.patch_bytes(1..3, b"XY")
            .expect("equal-width patch is accepted");
        let error = sink
            .patch_bytes(3..5, b"Q")
            .expect_err("variable-width patch is rejected");

        assert!(matches!(error, Error::Unsupported(message) if message.contains("width")));
        drop(sink);
        assert_eq!(bytes, b"aXYdef");
    }

    #[test]
    fn pass1_digest_counts_forward_bytes_without_requiring_a_body_buffer() {
        let mut target = VecOutputTarget::default();
        let mut sink = OutputSink::new(&mut target);
        sink.begin_digest();
        sink.write_bytes(b"pass-1").expect("pass-1 bytes are accepted");

        let digest = sink.take_digest().expect("pass-1 digest is enabled");
        let expected: [u8; 16] = md5::Md5::digest(b"pass-1").into();
        assert_eq!(sink.position(), 6);
        assert_eq!(digest, expected);
        drop(sink);
        assert_eq!(target.bytes, b"pass-1");
    }

    #[test]
    fn digest_stops_after_identifier_opening_marker() {
        let mut target = VecOutputTarget::default();
        let mut sink = OutputSink::new(&mut target);
        sink.begin_digest();

        sink.write_bytes(b"trailer /ID [")
            .expect("write identifier opening");
        sink.suspend_digest();
        sink.write_bytes(b"<identifier>]")
            .expect("write identifier bytes");

        let digest = sink.take_digest().expect("digest is enabled");
        let expected = md5::Md5::digest(b"trailer /ID [");
        let mut expected_bytes = [0; 16];
        expected_bytes.copy_from_slice(&expected);
        assert_eq!(digest, expected_bytes);
    }

    #[test]
    fn suspending_a_disabled_digest_does_not_enable_it() {
        let mut target = VecOutputTarget::default();
        let mut sink = OutputSink::new(&mut target);

        sink.suspend_digest();
        let error = sink
            .take_digest()
            .expect_err("a disabled digest must stay disabled");

        assert!(matches!(error, Error::Internal(message) if message.contains("disabled MD5")));
    }

    #[test]
    fn write_trait_uses_the_same_short_write_and_flush_contract() {
        let mut target = VecOutputTarget {
            writes: VecDeque::from([Ok(1), Ok(2)]),
            ..Default::default()
        };
        let mut sink = OutputSink::new(&mut target);

        sink.write_all(b"abc")
            .expect("Write::write_all completes through short writes");
        sink.flush()
            .expect("the counted sink has no local flush state");

        assert_eq!(sink.position(), 3);
        assert_eq!(sink.last_byte(), Some(b'c'));
        drop(sink);
        assert_eq!(target.bytes, b"abc");
    }

    #[test]
    fn write_trait_reports_position_overflow_as_io_error() {
        let mut target = VecOutputTarget::default();
        let mut sink = OutputSink::new(&mut target);
        sink.position = u64::MAX;

        let error = sink
            .write_all(b"a")
            .expect_err("Write maps counted-position overflow to io::Error");

        assert_eq!(error.kind(), ErrorKind::Other);
        assert!(error.to_string().contains("position exceeds u64"));
        assert_eq!(sink.position(), u64::MAX);
    }

    #[test]
    fn write_trait_preserves_target_io_error_kind() {
        let mut target = VecOutputTarget {
            writes: VecDeque::from([Ok(0)]),
            ..Default::default()
        };
        let mut sink = OutputSink::new(&mut target);

        let error = sink
            .write_all(b"a")
            .expect_err("Write preserves the target's zero-progress error");

        assert_eq!(error.kind(), ErrorKind::WriteZero);
        assert_eq!(sink.position(), 0);
    }

    #[test]
    fn segment_finish_preserves_position_and_active_digest() {
        let mut target = VecOutputTarget::default();
        let mut sink = OutputSink::new(&mut target);
        sink.begin_digest();
        sink.write_bytes(b"body-stream")
            .expect("write stream bytes");
        sink.finish_segment().expect("finish stream segment");
        assert_eq!(sink.position(), 11);
        sink.write_bytes(b" /ID [").expect("write ID opening");
        sink.suspend_digest();

        let digest = sink.take_digest().expect("digest survives segment finish");
        let expected = md5::Md5::digest(b"body-stream /ID [");
        assert_eq!(digest.as_slice(), expected.as_slice());
        drop(sink);
        assert_eq!(target.segment_finishes, 1);
        assert_eq!(target.document_finishes, 0);
    }

    #[test]
    fn second_digest_seed_truncates_info_at_first_nul() {
        let seed = super::deterministic_id_second_seed(&[0xab; 16], b" raw-info\0excluded-info")
            .expect("seed fits in memory");

        assert_eq!(seed, b"abababababababababababababababab QPDF  raw-info");
    }
}

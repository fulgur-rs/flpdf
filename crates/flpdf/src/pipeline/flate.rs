//! qpdf correspondence: Pl_Flate.cc streaming inflate, deflate, warning callback, compression-level, and finish responsibilities via flate2.
//!
//! qpdf's `QPDFJob::setWriterOptions` applies the process-wide compression
//! level before any writer streams are created (`QPDFJob.cc:2847-2851`), so
//! the crate-private setter below intentionally updates the same shared codec
//! state for the canonical writer.

use super::{Pipeline, PipelineError, PipelineRef, PipelineResult};
use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};
use std::sync::atomic::{AtomicI32, Ordering};

pub(crate) const DEFAULT_OUT_BUFFER_SIZE: usize = 65_536;
const Z_BUF_ERROR: i32 = -5;
const BUF_ERROR_WARNING: &str = "input stream is complete but output may still be valid";
static COMPRESSION_LEVEL: AtomicI32 = AtomicI32::new(-1);

#[cfg(test)]
pub(crate) static COMPRESSION_LEVEL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The zlib operation performed by [`PlFlate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlateAction {
    /// Decode a zlib stream.
    Inflate,
    /// Encode a zlib stream.
    Deflate,
}

enum FlateCodec {
    Inflate(Decompress),
    Deflate(Compress),
}

#[derive(Debug, Clone, Copy)]
enum InflatePhase {
    Header,
    DictId,
    Body,
    Trailer,
    Ended,
}

struct InflateState {
    phase: InflatePhase,
    header: [u8; 2],
    header_len: usize,
    dictid_len: usize,
    trailer: [u8; 4],
    trailer_len: usize,
    adler_a: u32,
    adler_b: u32,
}

impl InflateState {
    fn new() -> Self {
        Self {
            phase: InflatePhase::Header,
            header: [0; 2],
            header_len: 0,
            dictid_len: 0,
            trailer: [0; 4],
            trailer_len: 0,
            adler_a: 1,
            adler_b: 0,
        }
    }

    fn update_adler(&mut self, data: &[u8]) {
        const ADLER_MODULUS: u32 = 65_521;
        for byte in data {
            self.adler_a = (self.adler_a + u32::from(*byte)) % ADLER_MODULUS;
            self.adler_b = (self.adler_b + self.adler_a) % ADLER_MODULUS;
        }
    }

    fn adler(&self) -> u32 {
        (self.adler_b << 16) | self.adler_a
    }
}

type WarnCallback<'a> = dyn FnMut(&str, i32) -> PipelineResult<()> + 'a;

pub(crate) struct Flate<'a> {
    identifier: String,
    next: PipelineRef<'a>,
    action: FlateAction,
    compression_level: Option<i32>,
    codec: Option<FlateCodec>,
    inflate_state: InflateState,
    finished: bool,
    output: Vec<u8>,
    warn_callback: Option<Box<WarnCallback<'a>>>,
}

impl<'a> Flate<'a> {
    pub(crate) fn new(
        identifier: impl Into<String>,
        next: impl Into<PipelineRef<'a>>,
        action: FlateAction,
        out_buffer_size: usize,
    ) -> PipelineResult<Self> {
        Self::new_with_compression_level(identifier, next, action, out_buffer_size, None)
    }

    fn new_with_compression_level(
        identifier: impl Into<String>,
        next: impl Into<PipelineRef<'a>>,
        action: FlateAction,
        out_buffer_size: usize,
        compression_level: Option<i32>,
    ) -> PipelineResult<Self> {
        let identifier = identifier.into();
        if out_buffer_size == 0 {
            return Err(PipelineError::runtime(
                "Pl_Flate: output buffer size must be greater than zero",
            ));
        }
        if out_buffer_size > u32::MAX as usize {
            return Err(PipelineError::runtime(
                "Pl_Flate: zlib doesn't support buffer sizes larger than unsigned int",
            ));
        }

        Ok(Self {
            identifier,
            next: next.into(),
            action,
            compression_level,
            codec: None,
            inflate_state: InflateState::new(),
            finished: false,
            output: vec![0; out_buffer_size],
            warn_callback: None,
        })
    }

    pub(crate) fn set_compression_level(level: i32) -> PipelineResult<()> {
        if level != -1 && !(0..=9).contains(&level) {
            return Err(PipelineError::runtime(
                "Pl_Flate: compression level must be -1 or between 0 and 9",
            ));
        }
        COMPRESSION_LEVEL.store(level, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) fn set_warn_callback(
        &mut self,
        callback: impl FnMut(&str, i32) -> PipelineResult<()> + 'a,
    ) {
        self.warn_callback = Some(Box::new(callback));
    }

    fn warn(&mut self, message: &str, code: i32) -> PipelineResult<()> {
        let Some(callback) = self.warn_callback.as_mut() else {
            return Ok(());
        };
        callback(message, code)
    }

    fn handle_buf_error(&mut self, status: Status) -> PipelineResult<bool> {
        if status != Status::BufError {
            return Ok(false);
        }
        self.warn(BUF_ERROR_WARNING, Z_BUF_ERROR)?;
        Ok(true)
    }

    fn initialize_codec(&mut self) -> PipelineResult<()> {
        if self.codec.is_some() {
            return Ok(());
        }
        self.codec = Some(match self.action {
            FlateAction::Inflate => FlateCodec::Inflate(Decompress::new(false)),
            FlateAction::Deflate => {
                let level = self
                    .compression_level
                    .unwrap_or_else(|| COMPRESSION_LEVEL.load(Ordering::Relaxed));
                let compression = match level {
                    -1 => Compression::default(),
                    0..=9 => Compression::new(level as u32),
                    _ => {
                        return Err(PipelineError::runtime(format!(
                            "{}: deflate: Init: zlib stream error",
                            self.identifier
                        )))
                    }
                };
                FlateCodec::Deflate(Compress::new(compression, true))
            }
        });
        Ok(())
    }

    fn zlib_format_error(&self, detail: &'static str) -> PipelineError {
        PipelineError::runtime(format!("{}: inflate: data: {detail}", self.identifier))
    }

    fn consume_zlib_header(&mut self, data: &[u8]) -> PipelineResult<usize> {
        let state = &mut self.inflate_state;
        let consumed = (2 - state.header_len).min(data.len());
        state.header[state.header_len..state.header_len + consumed]
            .copy_from_slice(&data[..consumed]);
        state.header_len += consumed;
        if state.header_len != 2 {
            return Ok(consumed);
        }

        let [cmf, flg] = state.header;
        if ((u16::from(cmf) << 8) | u16::from(flg)) % 31 != 0 {
            return Err(self.zlib_format_error("incorrect header check"));
        }
        if cmf & 0x0f != 8 {
            return Err(self.zlib_format_error("unknown compression method"));
        }
        if cmf >> 4 > 7 {
            return Err(self.zlib_format_error("invalid window size"));
        }
        self.inflate_state.phase = if flg & 0x20 != 0 {
            InflatePhase::DictId
        } else {
            InflatePhase::Body
        };
        Ok(consumed)
    }

    fn consume_zlib_dictid(&mut self, data: &[u8]) -> PipelineResult<usize> {
        let remaining = 4 - self.inflate_state.dictid_len;
        let consumed = remaining.min(data.len());
        self.inflate_state.dictid_len += consumed;
        if self.inflate_state.dictid_len == 4 {
            return Err(self.zlib_format_error("zlib unknown error (2)"));
        }
        Ok(consumed)
    }

    fn consume_zlib_trailer(&mut self, data: &[u8]) {
        let state = &mut self.inflate_state;
        let consumed = (4 - state.trailer_len).min(data.len());
        state.trailer[state.trailer_len..state.trailer_len + consumed]
            .copy_from_slice(&data[..consumed]);
        state.trailer_len += consumed;
        if state.trailer_len == 4 {
            let expected = u32::from_be_bytes(state.trailer);
            let _checksum_matches = expected == state.adler();
            // qpdf intentionally accepts the checksum-only mismatch represented
            // by zlib's exact "incorrect data check" diagnostic.
            state.phase = InflatePhase::Ended;
        }
    }

    fn write_deflate(
        &mut self,
        codec: &mut Compress,
        data: &[u8],
        flush: FlushCompress,
    ) -> PipelineResult<()> {
        let mut input_offset = 0;

        loop {
            let before_in = codec.total_in();
            let before_out = codec.total_out();
            let result = codec.compress(&data[input_offset..], &mut self.output, flush);
            let consumed = (codec.total_in() - before_in) as usize;
            let produced = (codec.total_out() - before_out) as usize;
            input_offset += consumed;
            let identifier = &self.identifier;
            let detail = "deflate: data: zlib compression error";
            let status =
                result.map_err(|_| PipelineError::runtime(format!("{identifier}: {detail}")))?;

            if produced > 0 {
                self.next.write(&self.output[..produced])?;
            }

            if status == Status::StreamEnd || self.handle_buf_error(status)? {
                return Ok(());
            }
            if flush != FlushCompress::Finish
                && input_offset == data.len()
                && produced < self.output.len()
            {
                return Ok(());
            }
        }
    }

    fn write_inflate(
        &mut self,
        codec: &mut Decompress,
        data: &[u8],
        flush: FlushDecompress,
    ) -> PipelineResult<()> {
        let mut input_offset = 0;

        loop {
            match self.inflate_state.phase {
                InflatePhase::Header => {
                    input_offset += self.consume_zlib_header(&data[input_offset..])?;
                    if matches!(self.inflate_state.phase, InflatePhase::Header) {
                        if flush == FlushDecompress::Finish {
                            self.warn(BUF_ERROR_WARNING, Z_BUF_ERROR)?;
                        }
                        return Ok(());
                    }
                    if input_offset == data.len() && flush != FlushDecompress::Finish {
                        return Ok(());
                    }
                    if matches!(self.inflate_state.phase, InflatePhase::DictId) {
                        continue;
                    }
                }
                InflatePhase::DictId => {
                    self.consume_zlib_dictid(&data[input_offset..])?;
                    if flush == FlushDecompress::Finish {
                        self.warn(BUF_ERROR_WARNING, Z_BUF_ERROR)?;
                    }
                    return Ok(());
                }
                InflatePhase::Trailer => {
                    self.consume_zlib_trailer(&data[input_offset..]);
                    if matches!(self.inflate_state.phase, InflatePhase::Trailer)
                        && flush == FlushDecompress::Finish
                    {
                        self.warn(BUF_ERROR_WARNING, Z_BUF_ERROR)?;
                    }
                    return Ok(());
                }
                InflatePhase::Ended => return Ok(()),
                InflatePhase::Body => {}
            }

            let before_in = codec.total_in();
            let before_out = codec.total_out();
            let result = codec.decompress(&data[input_offset..], &mut self.output, flush);
            let consumed = (codec.total_in() - before_in) as usize;
            let produced = (codec.total_out() - before_out) as usize;
            input_offset += consumed;
            let status = match result {
                Ok(status) => status,
                Err(source) => {
                    let detail = source.message().unwrap_or("zlib decompression error");
                    return Err(PipelineError::runtime(format!(
                        "{}: inflate: data: {detail}",
                        self.identifier
                    )));
                }
            };

            if self.handle_buf_error(status)? {
                return Ok(());
            }

            if produced > 0 {
                self.inflate_state.update_adler(&self.output[..produced]);
                self.next.write(&self.output[..produced])?;
            }

            if status == Status::StreamEnd {
                self.inflate_state.phase = InflatePhase::Trailer;
                continue;
            }
            if input_offset == data.len() && produced < self.output.len() {
                return Ok(());
            }
        }
    }

    fn process_codec(&mut self, data: &[u8], finishing: bool) -> PipelineResult<()> {
        let Some(mut codec) = self.codec.take() else {
            return Ok(());
        };
        let result = match &mut codec {
            FlateCodec::Inflate(codec) => self.write_inflate(
                codec,
                data,
                if finishing {
                    FlushDecompress::Finish
                } else {
                    FlushDecompress::Sync
                },
            ),
            FlateCodec::Deflate(codec) => self.write_deflate(
                codec,
                data,
                if finishing {
                    FlushCompress::Finish
                } else {
                    FlushCompress::None
                },
            ),
        };
        self.codec = Some(codec);
        result
    }

    fn finish_codec(&mut self) -> PipelineResult<()> {
        self.process_codec(&[], true)
    }
}

impl Pipeline for Flate<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        if self.finished {
            return Err(PipelineError::logic(format!(
                "{}: Pl_Flate: write() called after finish() called",
                self.identifier
            )));
        }
        if data.is_empty() {
            return Ok(());
        }

        self.initialize_codec()?;
        self.process_codec(data, false)
    }

    fn finish(&mut self) -> PipelineResult<()> {
        if self.finished {
            return self.next.finish();
        }

        match self.finish_codec() {
            Ok(()) => {
                self.finished = true;
                self.codec = None;
                self.next.finish()
            }
            Err(first) => {
                let _ = self.next.finish();
                Err(PipelineError::runtime(first.to_string()))
            }
        }
    }
}

/// Public qpdf-shaped `Pl_Flate` pipeline.
///
/// qpdf exposes `Pl_Flate` to standalone tools such as `zlib-flate`, while
/// the writer's internal pipeline also needs the same codec. This wrapper
/// exposes only the public qpdf consumer boundary and keeps the ownership
/// adapter (`PipelineRef`) private to the crate.
pub struct PlFlate<'a> {
    inner: Flate<'a>,
}

impl<'a> PlFlate<'a> {
    /// Construct a zlib pipeline with qpdf's default 65,536-byte output buffer.
    pub fn new(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        action: FlateAction,
    ) -> PipelineResult<Self> {
        Self::new_with_buffer_size(identifier, next, action, DEFAULT_OUT_BUFFER_SIZE)
    }

    /// Construct a zlib pipeline with an explicit output buffer size.
    pub fn new_with_buffer_size(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        action: FlateAction,
        out_buffer_size: usize,
    ) -> PipelineResult<Self> {
        Ok(Self {
            inner: Flate::new(identifier, next, action, out_buffer_size)?,
        })
    }

    /// Construct a zlib pipeline with an explicit qpdf compression level.
    ///
    /// The level is validated lazily on the first non-empty deflate write,
    /// matching qpdf's `Pl_Flate::setCompressionLevel` followed by zlib's
    /// `deflateInit` timing. `-1` selects zlib's default level.
    pub fn new_with_compression_level(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        action: FlateAction,
        compression_level: i32,
    ) -> PipelineResult<Self> {
        Ok(Self {
            inner: Flate::new_with_compression_level(
                identifier,
                next,
                action,
                DEFAULT_OUT_BUFFER_SIZE,
                Some(compression_level),
            )?, // cov:ignore: the public constructor always uses its fixed valid default buffer size
        })
    }

    /// Set qpdf's process-wide deflate compression level.
    pub fn set_compression_level(level: i32) -> PipelineResult<()> {
        Flate::set_compression_level(level)
    }

    /// Install qpdf's warning callback for inflate `Z_BUF_ERROR` conditions.
    pub fn set_warn_callback(
        &mut self,
        callback: impl FnMut(&str, i32) -> PipelineResult<()> + 'a,
    ) {
        self.inner.set_warn_callback(callback);
    }
}

impl Pipeline for PlFlate<'_> {
    fn identifier(&self) -> &str {
        self.inner.identifier()
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.inner.write(data)
    }

    fn finish(&mut self) -> PipelineResult<()> {
        self.inner.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Flate, FlateAction, PlFlate, BUF_ERROR_WARNING, COMPRESSION_LEVEL_TEST_LOCK, Z_BUF_ERROR,
    };
    use crate::pipeline::buffer::Buffer;
    use crate::pipeline::{Pipeline, PipelineError, PipelineResult};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    fn deflate_chunks(chunks: &[&[u8]], out_buffer_size: usize) -> PipelineResult<Vec<u8>> {
        let _guard = COMPRESSION_LEVEL_TEST_LOCK.lock().unwrap();
        Flate::set_compression_level(-1)?;
        let mut sink = Buffer::new("sink", None);
        {
            let mut flate = Flate::new("flate", &mut sink, FlateAction::Deflate, out_buffer_size)?;
            for chunk in chunks {
                flate.write(chunk)?;
            }
            flate.finish()?;
        }
        sink.take_buffer()
    }

    fn inflate_chunks<'b>(
        chunks: impl IntoIterator<Item = &'b [u8]>,
        out_buffer_size: usize,
    ) -> PipelineResult<Vec<u8>> {
        let mut sink = Buffer::new("sink", None);
        {
            let mut flate = Flate::new("flate", &mut sink, FlateAction::Inflate, out_buffer_size)?;
            for chunk in chunks {
                flate.write(chunk)?;
            }
            flate.finish()?;
        }
        sink.take_buffer()
    }

    fn small_window_distance_vector() -> Vec<u8> {
        let mut encoded = vec![0x08, 0x1d, 0x73];
        encoded.extend(std::iter::repeat_n(0x74, 256));
        encoded.extend_from_slice(&[0x04, 0x06, 0x00, 0x00, 0xa9, 0xfd, 0x42, 0x05]);
        encoded
    }

    fn valid_prefix_then_invalid_stored_block() -> (Vec<u8>, Vec<u8>) {
        let mut encoded = vec![
            0x78, 0x01, // zlib header
            0x00, 0xff, 0xff, 0x00, 0x00, // non-final stored block, 65,535 bytes
        ];
        encoded.extend(std::iter::repeat_n(b'A', 65_535));
        encoded.extend_from_slice(&[
            0x00, 0x02, 0x00, 0xfd, 0xff, b'B', b'C', // valid non-final two-byte block
            0x01, 0x01, 0x00, 0x00, 0x00, // final block with invalid LEN/NLEN
        ]);

        let mut decoded = vec![b'A'; 65_535];
        decoded.extend_from_slice(b"BC");
        (encoded, decoded)
    }

    fn deflate_after_level_change(
        input: &[u8],
        level_at_new: i32,
        level_at_first_write: i32,
    ) -> Vec<u8> {
        Flate::set_compression_level(level_at_new).unwrap();
        let mut sink = Buffer::new("sink", None);
        {
            let mut flate = Flate::new("flate", &mut sink, FlateAction::Deflate, 17).unwrap();
            Flate::set_compression_level(level_at_first_write).unwrap();
            flate.write(input).unwrap();
            flate.finish().unwrap();
        }
        sink.take_buffer().unwrap()
    }

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

    #[derive(Default)]
    struct FinishFaultSink {
        finishes: usize,
    }

    impl Pipeline for FinishFaultSink {
        fn identifier(&self) -> &str {
            "finish-fault"
        }

        fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.finishes += 1;
            Err(PipelineError::logic(format!(
                "{}: finish failed",
                self.identifier()
            )))
        }
    }

    struct ArmedWriteFaultSink {
        armed: Rc<Cell<bool>>,
        finishes: usize,
    }

    impl Pipeline for ArmedWriteFaultSink {
        fn identifier(&self) -> &str {
            "armed-write-fault"
        }

        fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
            if self.armed.get() {
                return Err(PipelineError::logic(format!(
                    "{}: write failed",
                    self.identifier()
                )));
            }
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.finishes += 1;
            Ok(())
        }
    }

    #[test]
    fn deflate_is_invariant_to_input_chunking_and_finishes_zlib_stream() {
        let input = b"abcabcabcabcabcabc";
        let one = deflate_chunks(&[input.as_slice()], 65_536).unwrap();
        let many = deflate_chunks(&[b"a", b"bcabc", b"abcabcabcabc"], 3).unwrap();
        assert_eq!(one, many);

        let mut decoder = flate2::read::ZlibDecoder::new(one.as_slice());
        let mut decoded = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();
        assert_eq!(decoded, input);
    }

    #[test]
    fn write_after_finish_matches_qpdf_logic_error() {
        let mut sink = Buffer::new("sink", None);
        let mut flate = Flate::new("flate", &mut sink, FlateAction::Deflate, 8).unwrap();
        flate.finish().unwrap();
        let err = flate.write(b"x").unwrap_err();
        assert_eq!(
            err.to_string(),
            "flate: Pl_Flate: write() called after finish() called"
        );
    }

    #[test]
    fn inflate_is_invariant_to_input_and_output_boundaries() {
        let encoded = deflate_chunks(&[b"payload payload payload"], 7).unwrap();
        let decoded = inflate_chunks(encoded.chunks(2), 3).unwrap();
        assert_eq!(decoded, b"payload payload payload");
    }

    #[test]
    fn compression_level_setter_accepts_qpdf_zlib_values() {
        let _guard = COMPRESSION_LEVEL_TEST_LOCK.lock().unwrap();
        Flate::set_compression_level(-1).unwrap();
        Flate::set_compression_level(0).unwrap();
        Flate::set_compression_level(1).unwrap();
        Flate::set_compression_level(9).unwrap();
        assert!(Flate::set_compression_level(-2).is_err());
        assert!(Flate::set_compression_level(10).is_err());
        Flate::set_compression_level(-1).unwrap();
    }

    #[test]
    fn invalid_deflate_level_fails_at_first_nonempty_write_like_qpdf() {
        let _guard = COMPRESSION_LEVEL_TEST_LOCK.lock().unwrap();
        let mut sink = Buffer::new("sink", None);
        let mut flate = Flate::new_with_compression_level(
            "flate",
            &mut sink,
            FlateAction::Deflate,
            8,
            Some(10),
        )
        .unwrap();
        let error = flate.write(b"payload").unwrap_err();
        assert_eq!(error.message(), "flate: deflate: Init: zlib stream error");
    }

    #[test]
    fn invalid_deflate_level_is_not_initialized_for_empty_input() {
        let _guard = COMPRESSION_LEVEL_TEST_LOCK.lock().unwrap();
        let mut sink = Buffer::new("sink", None);
        let mut flate = Flate::new_with_compression_level(
            "flate",
            &mut sink,
            FlateAction::Deflate,
            8,
            Some(10),
        )
        .unwrap();
        flate.write(b"").unwrap();
        flate.finish().unwrap();
        drop(flate);
        assert!(sink.take_buffer().unwrap().is_empty());
    }

    #[test]
    fn public_pl_flate_boundary_delegates_to_the_canonical_stage() {
        let _guard = COMPRESSION_LEVEL_TEST_LOCK.lock().unwrap();
        PlFlate::set_compression_level(1).unwrap();
        let mut sink = Buffer::new("sink", None);
        let mut flate = PlFlate::new("public flate", &mut sink, FlateAction::Deflate).unwrap();
        assert_eq!(flate.identifier(), "public flate");
        flate.write(b"public boundary").unwrap();
        flate.finish().unwrap();
        drop(flate);
        assert!(!sink.take_buffer().unwrap().is_empty());
        Flate::set_compression_level(-1).unwrap();
    }

    #[test]
    fn deflate_reads_process_level_at_first_nonempty_write() {
        let _guard = COMPRESSION_LEVEL_TEST_LOCK.lock().unwrap();
        let input = b"abcdefghijklmnopqrstuvwabcdefghijklmnopqrstuvwabcdefghijklmnopqrstuvw\
            abcdefghijklmnopqrstuvwabcdefghijklmnopqrstuvwabcdefghijklmnopqrstuvw";
        let changed_after_new = deflate_after_level_change(input, -1, 1);
        let level_one = deflate_after_level_change(input, 1, 1);
        let level_nine = deflate_after_level_change(input, 9, 9);
        Flate::set_compression_level(-1).unwrap();

        assert_eq!(changed_after_new, level_one);
        assert_ne!(level_one, level_nine);
    }

    #[test]
    fn codec_finish_error_still_finishes_downstream_and_keeps_first_error() {
        let mut sink = FinishFaultSink::default();
        let mut flate = Flate::new("inflate", &mut sink, FlateAction::Inflate, 8).unwrap();
        flate.write(b"not zlib").unwrap_or(());
        let err = flate.finish().unwrap_err();
        assert!(matches!(err, PipelineError::Runtime(_)));
        assert_eq!(
            err.to_string(),
            "inflate: inflate: data: incorrect header check"
        );
        drop(flate);
        assert_eq!(sink.finishes, 1);
    }

    #[test]
    fn zero_output_buffer_is_rejected_as_runtime_error() {
        let mut sink = RecordingSink::default();
        let err = Flate::new("flate", &mut sink, FlateAction::Inflate, 0)
            .err()
            .unwrap();
        assert!(matches!(err, PipelineError::Runtime(_)));
        assert_eq!(
            err.to_string(),
            "Pl_Flate: output buffer size must be greater than zero"
        );
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn output_buffer_larger_than_zlibs_u32_domain_is_rejected_before_allocation() {
        let mut sink = RecordingSink::default();
        let err = Flate::new(
            "oversized flate",
            &mut sink,
            FlateAction::Inflate,
            u32::MAX as usize + 1,
        )
        .err()
        .expect("the zlib output-buffer domain ends at u32::MAX");
        assert!(matches!(err, PipelineError::Runtime(_)));
        assert_eq!(
            err.message(),
            "Pl_Flate: zlib doesn't support buffer sizes larger than unsigned int"
        );
        assert!(sink.chunks.is_empty());
        assert_eq!(sink.finishes, 0);
    }

    #[test]
    fn qpdf_buf_error_vector_warns_after_preserving_valid_output() {
        // Generated by the task's qpdf 11.9.0 Pl_Flate oracle probe from
        // compress2("a"), truncated at the independently observed boundary.
        const ENCODED_PREFIX: [u8; 4] = [0x78, 0x9c, 0x4b, 0x04];
        let warnings = Rc::new(RefCell::new(Vec::new()));
        let mut sink = RecordingSink::default();
        {
            let mut flate = Flate::new("inflate", &mut sink, FlateAction::Inflate, 1).unwrap();
            let observed = Rc::clone(&warnings);
            flate.set_warn_callback(move |message, code| {
                observed.borrow_mut().push((message.to_owned(), code));
                Ok(())
            });
            flate.write(&ENCODED_PREFIX).unwrap();
        }

        assert_eq!(sink.chunks.concat(), b"a");
        assert_eq!(
            *warnings.borrow(),
            [(
                "input stream is complete but output may still be valid".to_owned(),
                -5
            )]
        );
    }

    #[test]
    fn qpdf_incorrect_data_check_vector_preserves_complete_output() {
        // Generated by the task's qpdf 11.9.0 Pl_Flate oracle probe by
        // corrupting only compress2("checksum payload")'s Adler-32 trailer.
        const BAD_CHECKSUM: [u8; 24] = [
            0x78, 0x9c, 0x4b, 0xce, 0x48, 0x4d, 0xce, 0x2e, 0x2e, 0xcd, 0x55, 0x28, 0x48, 0xac,
            0xcc, 0xc9, 0x4f, 0x4c, 0x01, 0x00, 0x36, 0x17, 0x06, 0x5f,
        ];
        let decoded = inflate_chunks([BAD_CHECKSUM.as_slice()], 3).unwrap();
        assert_eq!(decoded, b"checksum payload");
    }

    #[test]
    fn corrupt_deflate_body_is_not_suppressed_as_a_checksum_error() {
        // A zlib header followed by a final stored block whose LEN/NLEN pair is
        // invalid, plus four trailer-shaped bytes. This must remain a codec
        // error rather than being confused with qpdf's checksum-only exception.
        const INVALID_STORED_BLOCK: [u8; 11] = [
            0x78, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
        ];
        let mut sink = RecordingSink::default();
        let mut flate = Flate::new("inflate", &mut sink, FlateAction::Inflate, 3).unwrap();
        let err = flate.write(&INVALID_STORED_BLOCK).unwrap_err();
        assert!(matches!(err, PipelineError::Runtime(_)));
        assert!(err.to_string().starts_with("inflate: inflate: data: "));
    }

    #[test]
    fn codec_error_keeps_forwarded_prefix_and_finishes_downstream_once() {
        let (encoded, decoded_prefix) = valid_prefix_then_invalid_stored_block();
        let mut sink = RecordingSink::default();
        {
            let mut flate =
                Flate::new("stream inflate", &mut sink, FlateAction::Inflate, 65_536).unwrap();
            let error = flate.write(&encoded).unwrap_err();
            assert!(
                error
                    .message()
                    .starts_with("stream inflate: inflate: data:"),
                "{error}"
            );
            let _ = flate.finish();
        }

        assert_eq!(sink.chunks.concat(), decoded_prefix[..65_536]);
        assert_eq!(sink.finishes, 1);
    }

    #[test]
    fn small_cinfo_distance_257_matches_qpdf_compatibility() {
        let encoded = small_window_distance_vector();
        let decoded = inflate_chunks([encoded.as_slice()], 17).unwrap();
        assert_eq!(decoded, vec![b'A'; 260]);
    }

    #[test]
    fn zlib_header_errors_follow_qpdf_libz_precedence() {
        for (header, detail) in [
            ([0x79, 0x00], "incorrect header check"),
            ([0x88, 0x00], "incorrect header check"),
            ([0x78, 0x00], "incorrect header check"),
            ([0x79, 0x18], "unknown compression method"),
            ([0x88, 0x1c], "invalid window size"),
        ] {
            let mut sink = RecordingSink::default();
            assert_eq!(sink.identifier(), "recording");
            let mut flate = Flate::new("inflate", &mut sink, FlateAction::Inflate, 3).unwrap();
            let err = flate.write(&header).unwrap_err();
            assert!(matches!(err, PipelineError::Runtime(_)));
            assert_eq!(err.message(), format!("inflate: inflate: data: {detail}"));
        }
    }

    #[test]
    fn fdict_header_only_waits_for_dictid_then_finish_warns() {
        let warnings = Rc::new(RefCell::new(Vec::new()));
        let mut sink = RecordingSink::default();
        {
            let mut flate =
                Flate::new("oracle inflate", &mut sink, FlateAction::Inflate, 3).unwrap();
            let observed = Rc::clone(&warnings);
            flate.set_warn_callback(move |message, code| {
                observed.borrow_mut().push((message.to_owned(), code));
                Ok(())
            });
            flate.write(&[0x78, 0x20]).unwrap();
            flate.finish().unwrap();
        }
        assert_eq!(
            warnings.borrow().as_slice(),
            [(BUF_ERROR_WARNING.to_owned(), Z_BUF_ERROR)]
        );
        assert_eq!(sink.finishes, 1);
        assert!(sink.chunks.is_empty());
    }

    #[test]
    fn incomplete_fdict_dictid_waits_across_chunks_then_finish_warns() {
        let warnings = Rc::new(RefCell::new(Vec::new()));
        let mut sink = RecordingSink::default();
        {
            let mut flate =
                Flate::new("oracle inflate", &mut sink, FlateAction::Inflate, 3).unwrap();
            let observed = Rc::clone(&warnings);
            flate.set_warn_callback(move |message, code| {
                observed.borrow_mut().push((message.to_owned(), code));
                Ok(())
            });
            flate.write(&[0x78]).unwrap();
            flate.write(&[0x20, 0x00]).unwrap();
            flate.write(&[0x00, 0x00]).unwrap();
            flate.finish().unwrap();
        }
        assert_eq!(
            warnings.borrow().as_slice(),
            [(BUF_ERROR_WARNING.to_owned(), Z_BUF_ERROR)]
        );
        assert_eq!(sink.finishes, 1);
        assert!(sink.chunks.is_empty());
    }

    #[test]
    fn complete_fdict_dictid_reports_qpdf_unknown_error_two() {
        let mut sink = RecordingSink::default();
        let mut flate = Flate::new("oracle inflate", &mut sink, FlateAction::Inflate, 3).unwrap();
        let err = flate
            .write(&[0x78, 0x20, 0x00, 0x00, 0x00, 0x01])
            .unwrap_err();
        assert!(matches!(err, PipelineError::Runtime(_)));
        assert_eq!(
            err.message(),
            "oracle inflate: inflate: data: zlib unknown error (2)"
        );
        drop(flate);
        assert!(sink.chunks.is_empty());
    }

    #[test]
    fn fdict_dictid_error_occurs_only_after_fourth_dictid_byte_for_every_split() {
        const FDICT_STREAM: [u8; 6] = [0x78, 0x20, 0x00, 0x00, 0x00, 0x01];

        for split in 1..FDICT_STREAM.len() {
            let mut sink = RecordingSink::default();
            let mut flate =
                Flate::new("oracle inflate", &mut sink, FlateAction::Inflate, 3).unwrap();
            flate.write(&FDICT_STREAM[..split]).unwrap();
            let err = flate.write(&FDICT_STREAM[split..]).unwrap_err();
            assert!(matches!(err, PipelineError::Runtime(_)));
            assert_eq!(
                err.message(),
                "oracle inflate: inflate: data: zlib unknown error (2)"
            );
        }
    }

    #[test]
    fn fdict_finish_keeps_local_error_and_still_finishes_downstream() {
        let mut sink = FinishFaultSink::default();
        let mut flate = Flate::new("oracle inflate", &mut sink, FlateAction::Inflate, 3).unwrap();
        assert_eq!(
            flate
                .write(&[0x78, 0x20, 0x00, 0x00, 0x00, 0x01])
                .unwrap_err()
                .message(),
            "oracle inflate: inflate: data: zlib unknown error (2)"
        );

        let err = flate.finish().unwrap_err();
        assert!(matches!(err, PipelineError::Runtime(_)));
        assert_eq!(
            err.message(),
            "oracle inflate: inflate: data: zlib unknown error (2)"
        );
        drop(flate);
        assert_eq!(sink.finishes, 1);
    }

    #[test]
    fn failed_fdict_finish_retains_qpdf_state_for_repeat_finish_and_write() {
        const FDICT_STREAM: [u8; 6] = [0x78, 0x20, 0x00, 0x00, 0x00, 0x01];
        const EXPECTED: &str = "oracle inflate: inflate: data: zlib unknown error (2)";

        let mut sink = RecordingSink::default();
        let mut flate = Flate::new("oracle inflate", &mut sink, FlateAction::Inflate, 3).unwrap();
        assert_eq!(
            flate.write(&FDICT_STREAM).unwrap_err().to_string(),
            EXPECTED
        );
        assert_eq!(flate.finish().unwrap_err().to_string(), EXPECTED);
        assert_eq!(flate.finish().unwrap_err().to_string(), EXPECTED);
        assert_eq!(flate.write(b"x").unwrap_err().to_string(), EXPECTED);
        drop(flate);
        assert_eq!(sink.finishes, 2);
    }

    #[test]
    fn split_header_and_trailer_are_consumed_incrementally() {
        let encoded = deflate_chunks(&[b"split framing"], 5).unwrap();
        let trailer_start = encoded.len() - 4;
        let mut sink = Buffer::new("sink", None);
        {
            let mut flate = Flate::new("inflate", &mut sink, FlateAction::Inflate, 3).unwrap();
            flate.write(&encoded[..1]).unwrap();
            flate.write(&encoded[1..trailer_start + 2]).unwrap();
            flate.write(&encoded[trailer_start + 2..]).unwrap();
            flate.finish().unwrap();
        }
        assert_eq!(sink.take_buffer().unwrap(), b"split framing");
    }

    #[test]
    fn inflate_accepts_zero_output_body_prefix_and_ignores_bytes_after_trailer() {
        let encoded = deflate_chunks(&[b"framing with trailing bytes"], 5).unwrap();
        let mut final_chunk = encoded[3..].to_vec();
        final_chunk.extend_from_slice(b"ignored");
        let mut sink = RecordingSink::default();
        {
            let mut flate = Flate::new("inflate", &mut sink, FlateAction::Inflate, 3).unwrap();
            flate.write(&encoded[..2]).unwrap();
            flate.write(&encoded[2..3]).unwrap();
            flate.write(&final_chunk).unwrap();
            flate.finish().unwrap();
        }
        assert_eq!(sink.chunks.concat(), b"framing with trailing bytes");
    }

    #[test]
    fn finish_with_incomplete_header_warns_and_finishes_downstream() {
        let warnings = Rc::new(RefCell::new(Vec::new()));
        let mut sink = RecordingSink::default();
        {
            let mut flate = Flate::new("inflate", &mut sink, FlateAction::Inflate, 3).unwrap();
            let observed = Rc::clone(&warnings);
            flate.set_warn_callback(move |message, code| {
                observed.borrow_mut().push((message.to_owned(), code));
                Ok(())
            });
            flate.write(&[0x78]).unwrap();
            flate.finish().unwrap();
        }
        assert_eq!(
            warnings.borrow().as_slice(),
            [(BUF_ERROR_WARNING.to_owned(), -5)]
        );
        assert_eq!(sink.finishes, 1);
    }

    #[test]
    fn finish_with_incomplete_trailer_warns_after_preserving_output() {
        let encoded = deflate_chunks(&[b"truncated trailer"], 5).unwrap();
        let warnings = Rc::new(RefCell::new(Vec::new()));
        let mut sink = RecordingSink::default();
        {
            let mut flate = Flate::new("inflate", &mut sink, FlateAction::Inflate, 3).unwrap();
            let observed = Rc::clone(&warnings);
            flate.set_warn_callback(move |message, code| {
                observed.borrow_mut().push((message.to_owned(), code));
                Ok(())
            });
            flate.write(&encoded[..encoded.len() - 2]).unwrap();
            flate.finish().unwrap();
        }
        assert_eq!(sink.chunks.concat(), b"truncated trailer");
        assert_eq!(
            warnings.borrow().as_slice(),
            [(BUF_ERROR_WARNING.to_owned(), -5)]
        );
        assert_eq!(sink.finishes, 1);
    }

    #[test]
    fn buf_error_without_callback_is_nonfatal() {
        const ENCODED_PREFIX: [u8; 4] = [0x78, 0x9c, 0x4b, 0x04];
        let mut sink = RecordingSink::default();
        let mut flate = Flate::new("inflate", &mut sink, FlateAction::Inflate, 1).unwrap();
        flate.write(&ENCODED_PREFIX).unwrap();
        drop(flate);
        assert_eq!(sink.chunks.concat(), b"a");
    }

    #[test]
    fn downstream_finish_error_is_returned_when_local_finish_succeeds() {
        let mut sink = FinishFaultSink::default();
        let mut flate = Flate::new("flate", &mut sink, FlateAction::Deflate, 8).unwrap();
        flate.write(b"x").unwrap();
        let err = flate.finish().unwrap_err();
        assert!(matches!(err, PipelineError::Logic(_)));
        assert_eq!(err.to_string(), "finish-fault: finish failed");
        drop(flate);
        assert_eq!(sink.finishes, 1);
    }

    #[test]
    fn finish_callback_logic_is_reconstructed_as_runtime_like_qpdf() {
        let mut sink = RecordingSink::default();
        let mut flate = Flate::new("inflate", &mut sink, FlateAction::Inflate, 3).unwrap();
        flate.set_warn_callback(|_, _| {
            Err(PipelineError::logic(
                "warning consumer: finish callback rejected warning",
            ))
        });
        flate.write(&[0x78]).unwrap();

        let err = flate.finish().unwrap_err();
        assert!(matches!(err, PipelineError::Runtime(_)));
        assert_eq!(
            err.to_string(),
            "warning consumer: finish callback rejected warning"
        );
        drop(flate);
        assert_eq!(sink.finishes, 1);
    }

    #[test]
    fn finish_downstream_write_logic_is_reconstructed_as_runtime_like_qpdf() {
        let armed = Rc::new(Cell::new(false));
        let mut sink = ArmedWriteFaultSink {
            armed: Rc::clone(&armed),
            finishes: 0,
        };
        let mut flate = Flate::new("flate", &mut sink, FlateAction::Deflate, 8).unwrap();
        flate.write(b"x").unwrap();
        armed.set(true);

        let err = flate.finish().unwrap_err();
        assert!(matches!(err, PipelineError::Runtime(_)));
        assert_eq!(err.to_string(), "armed-write-fault: write failed");
        drop(flate);
        assert_eq!(sink.finishes, 1);
    }

    #[test]
    fn write_after_inflate_stream_end_is_not_write_after_finish() {
        let encoded = deflate_chunks(&[b"ended"], 8).unwrap();
        let mut sink = Buffer::new("sink", None);
        {
            let mut flate = Flate::new("inflate", &mut sink, FlateAction::Inflate, 2).unwrap();
            flate.write(&encoded).unwrap();
            flate.write(b"ignored").unwrap();
            flate.finish().unwrap();
        }
        assert_eq!(sink.take_buffer().unwrap(), b"ended");
    }

    #[test]
    fn repeated_finish_only_repeats_downstream_finish_and_stays_terminal() {
        let mut sink = RecordingSink::default();
        {
            let mut flate = Flate::new("flate", &mut sink, FlateAction::Deflate, 8).unwrap();
            assert_eq!(flate.identifier(), "flate");
            flate.write(b"").unwrap();
            flate.write(b"x").unwrap();
            flate.finish().unwrap();
            flate.finish().unwrap();
            assert!(matches!(
                flate.write(b"y").unwrap_err(),
                PipelineError::Logic(_)
            ));
        }
        assert_eq!(sink.finishes, 2);
    }

    #[test]
    fn warn_callback_error_propagates_unchanged_like_qpdf() {
        const ENCODED_PREFIX: [u8; 4] = [0x78, 0x9c, 0x4b, 0x04];
        let mut sink = RecordingSink::default();
        let mut flate = Flate::new("inflate", &mut sink, FlateAction::Inflate, 1).unwrap();
        flate.set_warn_callback(|_, _| {
            Err(PipelineError::logic(
                "warning consumer: callback rejected warning",
            ))
        });

        let err = flate.write(&ENCODED_PREFIX).unwrap_err();
        assert_eq!(
            err.to_string(),
            "warning consumer: callback rejected warning"
        );
        assert!(matches!(err, PipelineError::Logic(_)));
    }

    #[test]
    fn malformed_input_is_a_qpdf_runtime_error() {
        let mut sink = RecordingSink::default();
        let mut flate = Flate::new("inflate", &mut sink, FlateAction::Inflate, 8).unwrap();
        let err = flate.write(b"not zlib").unwrap_err();
        assert!(matches!(err, PipelineError::Runtime(_)));
        assert_eq!(
            err.to_string(),
            "inflate: inflate: data: incorrect header check"
        );
    }
}

use super::{Pipeline, PipelineError, PipelineResult};
use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};
use std::fmt;
use std::sync::atomic::{AtomicI32, Ordering};

#[allow(dead_code)]
pub(crate) const DEFAULT_OUT_BUFFER_SIZE: usize = 65_536;
const Z_BUF_ERROR: i32 = -5;
const BUF_ERROR_WARNING: &str = "input stream is complete but output may still be valid";
static COMPRESSION_LEVEL: AtomicI32 = AtomicI32::new(-1);

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlateAction {
    Inflate,
    Deflate,
}

enum FlateCodec {
    Inflate(Decompress),
    Deflate(Compress),
}

#[derive(Debug, Clone, Copy)]
enum InflatePhase {
    Header { bytes: [u8; 2], len: usize },
    Body,
    Trailer { bytes: [u8; 4], len: usize },
    Ended,
}

struct InflateState {
    phase: InflatePhase,
    adler_a: u32,
    adler_b: u32,
}

impl InflateState {
    fn new() -> Self {
        Self {
            phase: InflatePhase::Header {
                bytes: [0; 2],
                len: 0,
            },
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

#[derive(Debug)]
struct ZlibFormatError(&'static str);

impl fmt::Display for ZlibFormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ZlibFormatError {}

pub(crate) struct Flate<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    action: FlateAction,
    codec: Option<FlateCodec>,
    inflate_state: InflateState,
    finished: bool,
    output: Vec<u8>,
    warn_callback: Option<Box<dyn FnMut(&str, i32) -> PipelineResult<()> + 'a>>,
}

#[allow(dead_code)]
impl<'a> Flate<'a> {
    pub(crate) fn new(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        action: FlateAction,
        out_buffer_size: usize,
    ) -> PipelineResult<Self> {
        let identifier = identifier.into();
        if out_buffer_size == 0 {
            return Err(PipelineError::state(
                identifier,
                "output buffer size must be greater than zero",
            ));
        }

        Ok(Self {
            identifier,
            next,
            action,
            codec: None,
            inflate_state: InflateState::new(),
            finished: false,
            output: vec![0; out_buffer_size],
            warn_callback: None,
        })
    }

    pub(crate) fn set_compression_level(level: i32) -> PipelineResult<()> {
        if level != -1 && !(1..=9).contains(&level) {
            return Err(PipelineError::state(
                "flate",
                "compression level must be -1 or between 1 and 9",
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
        callback(message, code).map_err(|source| {
            PipelineError::callback_with_source(
                &self.identifier,
                format!("warning callback failed: {source}"),
                source,
            )
        })
    }

    fn initialize_codec(&mut self) {
        if self.codec.is_some() {
            return;
        }
        self.codec = Some(match self.action {
            FlateAction::Inflate => FlateCodec::Inflate(Decompress::new(false)),
            FlateAction::Deflate => {
                let level = COMPRESSION_LEVEL.load(Ordering::Relaxed);
                let compression = if level == -1 {
                    Compression::default()
                } else {
                    Compression::new(level as u32)
                };
                FlateCodec::Deflate(Compress::new(compression, true))
            }
        });
    }

    fn zlib_format_error(&self, detail: &'static str) -> PipelineError {
        PipelineError::codec_with_source(
            &self.identifier,
            format!("inflate: data: {detail}"),
            ZlibFormatError(detail),
        )
    }

    fn consume_zlib_header(&mut self, data: &[u8]) -> PipelineResult<usize> {
        let InflatePhase::Header { bytes, len } = &mut self.inflate_state.phase else {
            return Ok(0);
        };
        let consumed = (2 - *len).min(data.len());
        bytes[*len..*len + consumed].copy_from_slice(&data[..consumed]);
        *len += consumed;
        if *len != 2 {
            return Ok(consumed);
        }

        let [cmf, flg] = *bytes;
        if cmf & 0x0f != 8 {
            return Err(self.zlib_format_error("unknown compression method"));
        }
        if cmf >> 4 > 7 {
            return Err(self.zlib_format_error("invalid window size"));
        }
        if ((u16::from(cmf) << 8) | u16::from(flg)) % 31 != 0 {
            return Err(self.zlib_format_error("incorrect header check"));
        }
        if flg & 0x20 != 0 {
            return Err(self.zlib_format_error("preset dictionary is not supported"));
        }
        self.inflate_state.phase = InflatePhase::Body;
        Ok(consumed)
    }

    fn consume_zlib_trailer(&mut self, data: &[u8]) -> usize {
        let InflatePhase::Trailer { bytes, len } = &mut self.inflate_state.phase else {
            return 0;
        };
        let consumed = (4 - *len).min(data.len());
        bytes[*len..*len + consumed].copy_from_slice(&data[..consumed]);
        *len += consumed;
        if *len == 4 {
            let expected = u32::from_be_bytes(*bytes);
            let _checksum_matches = expected == self.inflate_state.adler();
            // qpdf intentionally accepts the checksum-only mismatch represented
            // by zlib's exact "incorrect data check" diagnostic.
            self.inflate_state.phase = InflatePhase::Ended;
        }
        consumed
    }

    fn write_deflate(&mut self, data: &[u8], flush: FlushCompress) -> PipelineResult<()> {
        let Some(FlateCodec::Deflate(codec)) = self.codec.as_mut() else {
            return Err(PipelineError::state(
                &self.identifier,
                "deflate codec is unavailable",
            ));
        };
        let mut input_offset = 0;

        loop {
            let before_in = codec.total_in();
            let before_out = codec.total_out();
            let result = codec.compress(&data[input_offset..], &mut self.output, flush);
            let consumed = (codec.total_in() - before_in) as usize;
            let produced = (codec.total_out() - before_out) as usize;
            input_offset += consumed;
            let status = match result {
                Ok(status) => status,
                Err(source) => {
                    let detail = source
                        .message()
                        .unwrap_or("zlib compression error")
                        .to_owned();
                    return Err(PipelineError::codec_with_source(
                        &self.identifier,
                        format!("deflate: data: {detail}"),
                        source,
                    ));
                }
            };

            if status == Status::BufError {
                self.warn(BUF_ERROR_WARNING, Z_BUF_ERROR)?;
                if flush == FlushCompress::Finish {
                    return Err(PipelineError::codec(
                        &self.identifier,
                        "deflate: End: zlib data error",
                    ));
                }
                return Ok(());
            }

            if produced > 0 {
                self.next.write(&self.output[..produced])?;
            }

            if status == Status::StreamEnd {
                return Ok(());
            }
            if flush != FlushCompress::Finish
                && input_offset == data.len()
                && produced < self.output.len()
            {
                return Ok(());
            }
            if consumed == 0 && produced == 0 {
                return Err(PipelineError::codec(
                    &self.identifier,
                    "deflate: data: codec made no progress",
                ));
            }
        }
    }

    fn write_inflate(&mut self, data: &[u8], flush: FlushDecompress) -> PipelineResult<()> {
        let mut input_offset = 0;

        loop {
            match self.inflate_state.phase {
                InflatePhase::Header { .. } => {
                    input_offset += self.consume_zlib_header(&data[input_offset..])?;
                    if matches!(self.inflate_state.phase, InflatePhase::Header { .. }) {
                        if flush == FlushDecompress::Finish {
                            self.warn(BUF_ERROR_WARNING, Z_BUF_ERROR)?;
                        }
                        return Ok(());
                    }
                    if input_offset == data.len() && flush != FlushDecompress::Finish {
                        return Ok(());
                    }
                }
                InflatePhase::Trailer { .. } => {
                    input_offset += self.consume_zlib_trailer(&data[input_offset..]);
                    if matches!(self.inflate_state.phase, InflatePhase::Trailer { .. })
                        && flush == FlushDecompress::Finish
                    {
                        self.warn(BUF_ERROR_WARNING, Z_BUF_ERROR)?;
                    }
                    if input_offset == data.len()
                        || matches!(self.inflate_state.phase, InflatePhase::Ended)
                    {
                        return Ok(());
                    }
                }
                InflatePhase::Ended => return Ok(()),
                InflatePhase::Body => {}
            }

            let Some(FlateCodec::Inflate(codec)) = self.codec.as_mut() else {
                return Err(PipelineError::state(
                    &self.identifier,
                    "inflate codec is unavailable",
                ));
            };
            let before_in = codec.total_in();
            let before_out = codec.total_out();
            let result = codec.decompress(&data[input_offset..], &mut self.output, flush);
            let consumed = (codec.total_in() - before_in) as usize;
            let produced = (codec.total_out() - before_out) as usize;
            input_offset += consumed;
            let status = match result {
                Ok(status) => status,
                Err(source) => {
                    let detail = source
                        .message()
                        .unwrap_or("zlib decompression error")
                        .to_owned();
                    return Err(PipelineError::codec_with_source(
                        &self.identifier,
                        format!("inflate: data: {detail}"),
                        source,
                    ));
                }
            };

            if status == Status::BufError {
                self.warn(BUF_ERROR_WARNING, Z_BUF_ERROR)?;
                return Ok(());
            }

            if produced > 0 {
                self.inflate_state.update_adler(&self.output[..produced]);
                self.next.write(&self.output[..produced])?;
            }

            if status == Status::StreamEnd {
                self.inflate_state.phase = InflatePhase::Trailer {
                    bytes: [0; 4],
                    len: 0,
                };
                continue;
            }
            if input_offset == data.len() && produced < self.output.len() {
                return Ok(());
            }
            if consumed == 0 && produced == 0 {
                return Err(PipelineError::codec(
                    &self.identifier,
                    "inflate: data: codec made no progress",
                ));
            }
        }
    }

    fn finish_codec(&mut self) -> PipelineResult<()> {
        match self.action {
            FlateAction::Deflate if self.codec.is_some() => {
                self.write_deflate(&[], FlushCompress::Finish)
            }
            FlateAction::Inflate
                if self.codec.is_some()
                    && !matches!(self.inflate_state.phase, InflatePhase::Ended) =>
            {
                self.write_inflate(&[], FlushDecompress::Finish)
            }
            FlateAction::Inflate | FlateAction::Deflate => Ok(()),
        }
    }
}

impl Pipeline for Flate<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        if self.finished {
            return Err(PipelineError::state(
                &self.identifier,
                "write called after finish",
            ));
        }
        if data.is_empty() {
            return Ok(());
        }

        self.initialize_codec();
        match self.action {
            FlateAction::Inflate => self.write_inflate(data, FlushDecompress::Sync),
            FlateAction::Deflate => self.write_deflate(data, FlushCompress::None),
        }
    }

    fn finish(&mut self) -> PipelineResult<()> {
        let local_result = if self.finished {
            Ok(())
        } else {
            self.finish_codec()
        };
        self.finished = true;
        self.codec = None;
        let downstream_result = self.next.finish();
        match (local_result, downstream_result) {
            (Err(first), _) => Err(first),
            (Ok(()), Err(downstream)) => Err(downstream),
            (Ok(()), Ok(())) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Flate, FlateAction};
    use crate::pipeline::buffer::Buffer;
    use crate::pipeline::{Pipeline, PipelineError, PipelineErrorKind, PipelineResult};
    use std::cell::RefCell;
    use std::error::Error as _;
    use std::rc::Rc;
    use std::sync::Mutex;

    static COMPRESSION_LEVEL_TEST_LOCK: Mutex<()> = Mutex::new(());

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
            Err(PipelineError::state(self.identifier(), "finish failed"))
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
    fn write_after_finish_reports_stage_state_error() {
        let mut sink = Buffer::new("sink", None);
        let mut flate = Flate::new("flate", &mut sink, FlateAction::Deflate, 8).unwrap();
        flate.finish().unwrap();
        let err = flate.write(b"x").unwrap_err();
        assert_eq!(err.stage(), "flate");
        assert_eq!(err.kind(), PipelineErrorKind::State);
    }

    #[test]
    fn inflate_is_invariant_to_input_and_output_boundaries() {
        let encoded = deflate_chunks(&[b"payload payload payload"], 7).unwrap();
        let decoded = inflate_chunks(encoded.chunks(2), 3).unwrap();
        assert_eq!(decoded, b"payload payload payload");
    }

    #[test]
    fn compression_level_accepts_qpdf_domain_only() {
        let _guard = COMPRESSION_LEVEL_TEST_LOCK.lock().unwrap();
        Flate::set_compression_level(-1).unwrap();
        Flate::set_compression_level(1).unwrap();
        Flate::set_compression_level(9).unwrap();
        assert!(Flate::set_compression_level(0).is_err());
        assert!(Flate::set_compression_level(10).is_err());
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
        assert_eq!(err.stage(), "inflate");
        drop(flate);
        assert_eq!(sink.finishes, 1);
    }

    #[test]
    fn zero_output_buffer_is_rejected_as_state_error() {
        let mut sink = RecordingSink::default();
        let err = Flate::new("flate", &mut sink, FlateAction::Inflate, 0)
            .err()
            .unwrap();
        assert_eq!(err.stage(), "flate");
        assert_eq!(err.kind(), PipelineErrorKind::State);
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
        assert_eq!(err.stage(), "inflate");
        assert_eq!(err.kind(), PipelineErrorKind::Codec);
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
            flate.write(b"x").unwrap();
            flate.finish().unwrap();
            flate.finish().unwrap();
            assert_eq!(
                flate.write(b"y").unwrap_err().kind(),
                PipelineErrorKind::State
            );
        }
        assert_eq!(sink.finishes, 2);
    }

    #[test]
    fn warn_callback_error_is_wrapped_with_stage_and_source() {
        const ENCODED_PREFIX: [u8; 4] = [0x78, 0x9c, 0x4b, 0x04];
        let mut sink = RecordingSink::default();
        let mut flate = Flate::new("inflate", &mut sink, FlateAction::Inflate, 1).unwrap();
        flate.set_warn_callback(|_, _| {
            Err(PipelineError::state(
                "warning consumer",
                "callback rejected warning",
            ))
        });

        let err = flate.write(&ENCODED_PREFIX).unwrap_err();
        assert_eq!(err.stage(), "inflate");
        assert_eq!(err.kind(), PipelineErrorKind::Callback);
        assert_eq!(
            err.source().unwrap().to_string(),
            "warning consumer: callback rejected warning"
        );
    }

    #[test]
    fn malformed_input_codec_error_preserves_dependency_source() {
        let mut sink = RecordingSink::default();
        let mut flate = Flate::new("inflate", &mut sink, FlateAction::Inflate, 8).unwrap();
        let err = flate.write(b"not zlib").unwrap_err();
        assert_eq!(err.stage(), "inflate");
        assert_eq!(err.kind(), PipelineErrorKind::Codec);
        assert!(err.source().is_some());
    }
}

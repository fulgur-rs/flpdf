use super::{Pipeline, PipelineError, PipelineResult};
use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};
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

pub(crate) struct Flate<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    codec: Option<FlateCodec>,
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

        let codec = match action {
            FlateAction::Inflate => FlateCodec::Inflate(Decompress::new(true)),
            FlateAction::Deflate => {
                let level = COMPRESSION_LEVEL.load(Ordering::Relaxed);
                let compression = if level == -1 {
                    Compression::default()
                } else {
                    Compression::new(level as u32)
                };
                FlateCodec::Deflate(Compress::new(compression, true))
            }
        };
        Ok(Self {
            identifier,
            next,
            codec: Some(codec),
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
        let Some(FlateCodec::Inflate(codec)) = self.codec.as_mut() else {
            return Err(PipelineError::state(
                &self.identifier,
                "inflate codec is unavailable",
            ));
        };
        let mut input_offset = 0;

        loop {
            let before_in = codec.total_in();
            let before_out = codec.total_out();
            let result = codec.decompress(&data[input_offset..], &mut self.output, flush);
            let consumed = (codec.total_in() - before_in) as usize;
            let produced = (codec.total_out() - before_out) as usize;
            input_offset += consumed;
            let status = match result {
                Ok(status) => status,
                Err(source) if source.message() == Some("incorrect data check") => {
                    Status::StreamEnd
                }
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
                self.next.write(&self.output[..produced])?;
            }

            if status == Status::StreamEnd {
                self.codec = None;
                return Ok(());
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
        match self.codec.as_ref() {
            Some(FlateCodec::Deflate(codec)) if codec.total_in() != 0 || codec.total_out() != 0 => {
                self.write_deflate(&[], FlushCompress::Finish)
            }
            Some(FlateCodec::Inflate(codec)) if codec.total_in() != 0 || codec.total_out() != 0 => {
                self.write_inflate(&[], FlushDecompress::Finish)
            }
            _ => Ok(()),
        }
    }
}

impl Pipeline for Flate<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        match self.codec.as_ref() {
            None => Err(PipelineError::state(
                &self.identifier,
                "write called after finish",
            )),
            Some(_) if data.is_empty() => Ok(()),
            Some(FlateCodec::Inflate(_)) => self.write_inflate(data, FlushDecompress::Sync),
            Some(FlateCodec::Deflate(_)) => self.write_deflate(data, FlushCompress::None),
        }
    }

    fn finish(&mut self) -> PipelineResult<()> {
        let local_result = self.finish_codec();
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

    #[cfg(feature = "qpdf-zlib-compat")]
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

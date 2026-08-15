//! qpdf correspondence: `Pl_DCT` buffers compressed input and decodes it on `finish`, emitting one decoded scanline at a time to the next pipeline.

use super::buffer::Buffer;
use super::{Pipeline, PipelineError, PipelineRef, PipelineResult};

#[cfg(feature = "qpdf-libjpeg-compat")]
use flpdf_libjpeg_compat::DecodeError;

#[allow(dead_code)]
pub(crate) struct PlDct<'a> {
    identifier: String,
    next: PipelineRef<'a>,
    buffer: Buffer<'static>,
}

#[allow(dead_code)]
impl<'a> PlDct<'a> {
    pub(crate) fn new(identifier: impl Into<String>, next: impl Into<PipelineRef<'a>>) -> Self {
        Self {
            identifier: identifier.into(),
            next: next.into(),
            buffer: Buffer::new("DCT buffer", None),
        }
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    fn jpeg_error(&self, error: libjpeg_turbo_rs::JpegError, data: &[u8]) -> PipelineError {
        if let [first, second, ..] = data {
            if !data.starts_with(&[0xff, 0xd8]) {
                return self.runtime_error(format!(
                    "Not a JPEG file: starts with 0x{first:02x} 0x{second:02x}"
                ));
            }
        }
        // qpdf's whole-buffer `jpeg_source_mgr` (`Pl_DCT.cc:199-206`,
        // `fill_buffer_input_buffer`) throws exactly this message whenever
        // libjpeg asks for more bytes than the supplied buffer holds — for a
        // buffer too short to even read the SOI marker (0 or 1 bytes, so the
        // check above never runs) and for a buffer that starts with a valid
        // SOI but runs out mid-header or mid-scan. `libjpeg-turbo-rs` reports
        // both as `JpegError::UnexpectedEof`, so normalize both to match
        // qpdf's observed diagnostic (verified against `qpdf --show-object
        // --filtered-stream-data` on a 1-byte `/DCTDecode` stream).
        if matches!(error, libjpeg_turbo_rs::JpegError::UnexpectedEof) {
            return self.runtime_error("invalid jpeg data reading from buffer");
        }
        PipelineError::runtime(format!("{}: {error}", self.identifier))
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    fn runtime_error(&self, message: impl AsRef<str>) -> PipelineError {
        PipelineError::runtime(format!("{}: {}", self.identifier, message.as_ref()))
    }
    #[cfg(feature = "qpdf-libjpeg-compat")]
    fn decode_with_compat_backend(&mut self, data: &[u8]) -> PipelineResult<()> {
        let result = {
            let next = &mut self.next;
            flpdf_libjpeg_compat::decode_scanlines(data, &mut |row| next.write(row))
        };

        match result {
            Ok(()) => self.next.finish(),
            Err(DecodeError::Codec(message)) => Err(PipelineError::runtime(format!(
                "{}: {message}",
                self.identifier
            ))),
            Err(DecodeError::Callback(error)) => Err(error),
            Err(DecodeError::CallbackPanicked) => Err(PipelineError::runtime(format!(
                "{}: downstream pipeline panicked",
                self.identifier
            ))),
            Err(DecodeError::CallbackFailure(message)) => Err(PipelineError::runtime(format!(
                "{}: {message}",
                self.identifier
            ))),
        }
    }
}
impl Pipeline for PlDct<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.buffer.write(data)
    }

    fn finish(&mut self) -> PipelineResult<()> {
        self.buffer.finish()?;
        let data = self.buffer.take_buffer()?;
        if data.is_empty() {
            return self.next.finish();
        }

        #[cfg(feature = "qpdf-libjpeg-compat")]
        return self.decode_with_compat_backend(&data);

        #[cfg(not(feature = "qpdf-libjpeg-compat"))]
        {
            let mut decoder = libjpeg_turbo_rs::ScanlineDecoder::new(&data)
                .map_err(|error| self.jpeg_error(error, &data))?;
            let (precision, width, height, components) = {
                let header = decoder.header();
                (
                    header.precision,
                    header.width(),
                    header.height(),
                    header.components.len(),
                )
            };

            if precision != 8 {
                return Err(self.runtime_error(format!(
                    "sample precision {precision} (only 8-bit supported)"
                )));
            }

            let bytes_per_pixel = match components {
                1 => 1,
                3 => 3,
                4 => 4,
                _ => {
                    return Err(self
                        .runtime_error(format!("unsupported JPEG component count {components}")));
                }
            };
            // cov:ignore-start: JPEG width is u16 and supported bpp is at most 4, so usize multiplication cannot overflow
            let row_length = width.checked_mul(bytes_per_pixel).ok_or_else(|| {
                self.runtime_error(format!("scanline byte length overflow for width {width}"))
            })?;
            // cov:ignore-end
            let mut row = vec![0u8; row_length];

            for _ in 0..height {
                decoder
                    .read_scanline(&mut row)
                    .map_err(|error| self.jpeg_error(error, &data))?;
                // cov:ignore-start: ScanlineDecoder writes into this caller-owned slice and returns no row with a different length
                if row.len() != row_length {
                    return Err(self.runtime_error(format!(
                        "decoded scanline length {}, expected {row_length}",
                        row.len()
                    )));
                }
                // cov:ignore-end
                self.next.write(&row)?;
            }

            decoder
                .finish()
                .map_err(|error| self.jpeg_error(error, &data))?;
            self.next.finish()
        }
    }
}

#[cfg(all(test, feature = "qpdf-libjpeg-compat"))]
mod tests {
    use super::PlDct;
    use crate::pipeline::{Pipeline, PipelineResult};

    struct Sink;

    impl Pipeline for Sink {
        fn identifier(&self) -> &str {
            "DCT compatibility test sink"
        }

        fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
    }

    #[test]
    fn compatibility_test_sink_implements_pipeline_methods() {
        let mut sink = Sink;
        assert_eq!(sink.identifier(), "DCT compatibility test sink");
        sink.write(b"compatibility test")
            .expect("compatibility test sink write must succeed");
        sink.finish()
            .expect("compatibility test sink finish must succeed");
    }

    #[test]
    fn libjpeg_compat_backend_preserves_libjpeg_diagnostic() {
        let mut sink = Sink;
        let mut stage = PlDct::new("DCT decode", &mut sink);
        stage
            .write(&[0xff, 0xd8, 0xff, 0xd9])
            .expect("DCT stage buffers input");

        let error = stage
            .finish()
            .expect_err("invalid JPEG must preserve the libjpeg diagnostic");

        assert!(
            error
                .to_string()
                .contains("JPEG datastream contains no image"),
            "unexpected compatibility diagnostic: {error}"
        );
    }
}

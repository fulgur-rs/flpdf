//! qpdf correspondence: `Pl_DCT` buffers compressed input and decodes it on
//! `finish`, emitting one decoded scanline at a time to the next pipeline.

use super::buffer::Buffer;
use super::{Pipeline, PipelineError, PipelineResult};

pub(crate) struct PlDct<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    buffer: Buffer<'static>,
}

impl<'a> PlDct<'a> {
    pub(crate) fn new(identifier: impl Into<String>, next: &'a mut dyn Pipeline) -> Self {
        Self {
            identifier: identifier.into(),
            next,
            buffer: Buffer::new("DCT buffer", None),
        }
    }

    fn jpeg_error(&self, error: libjpeg_turbo_rs::JpegError) -> PipelineError {
        PipelineError::runtime(format!("{}: {error}", self.identifier))
    }

    fn runtime_error(&self, message: impl AsRef<str>) -> PipelineError {
        PipelineError::runtime(format!("{}: {}", self.identifier, message.as_ref()))
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

        let mut decoder = libjpeg_turbo_rs::ScanlineDecoder::new(&data)
            .map_err(|error| self.jpeg_error(error))?;
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
                return Err(
                    self.runtime_error(format!("unsupported JPEG component count {components}"))
                );
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
                .map_err(|error| self.jpeg_error(error))?;
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

        decoder.finish().map_err(|error| self.jpeg_error(error))?;
        self.next.finish()
    }
}

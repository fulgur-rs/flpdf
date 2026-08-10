//! qpdf correspondence: `Pl_DCT` buffers compressed input and decodes it on
//! `finish`, emitting one decoded scanline at a time to the next pipeline.

use super::buffer::Buffer;
use super::{Pipeline, PipelineError, PipelineResult};

#[cfg(feature = "qpdf-libjpeg-compat")]
mod jpeg_compat {
    use std::os::raw::{c_char, c_int, c_uchar, c_void};

    pub(super) const SUCCESS: c_int = 0;
    pub(super) const CODEC_ERROR: c_int = 1;
    pub(super) const CALLBACK_ERROR: c_int = 2;
    pub(super) const CALLBACK_FAILURE: c_int = 1;

    extern "C" {
        pub(super) fn flpdf_jpeg_decode_scanlines(
            data: *const c_uchar,
            data_len: usize,
            callback: unsafe extern "C" fn(*mut c_void, *const c_uchar, usize) -> c_int,
            user: *mut c_void,
            error_message: *mut c_char,
            error_message_len: usize,
        ) -> c_int;
    }
}

#[cfg(feature = "qpdf-libjpeg-compat")]
struct CallbackState<'a> {
    next: &'a mut dyn Pipeline,
    error: Option<PipelineError>,
}

#[cfg(feature = "qpdf-libjpeg-compat")]
unsafe extern "C" fn jpeg_scanline_callback(
    user: *mut std::os::raw::c_void,
    row: *const std::os::raw::c_uchar,
    row_len: usize,
) -> std::os::raw::c_int {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::slice;

    if user.is_null() {
        return jpeg_compat::CALLBACK_FAILURE;
    }

    let state = unsafe { &mut *(user.cast::<CallbackState<'_>>()) };
    let result = catch_unwind(AssertUnwindSafe(|| {
        if state.error.is_some() {
            return jpeg_compat::CALLBACK_FAILURE;
        }
        if row.is_null() && row_len != 0 {
            state.error = Some(PipelineError::runtime(
                "DCT decode: compatibility backend returned a null scanline",
            ));
            return jpeg_compat::CALLBACK_FAILURE;
        }

        let row = if row_len == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(row, row_len) }
        };
        match state.next.write(row) {
            Ok(()) => jpeg_compat::SUCCESS,
            Err(error) => {
                state.error = Some(error);
                jpeg_compat::CALLBACK_FAILURE
            }
        }
    }));

    match result {
        Ok(result) => result,
        Err(_) => {
            if state.error.is_none() {
                state.error = Some(PipelineError::runtime(
                    "DCT decode: downstream pipeline panicked",
                ));
            }
            jpeg_compat::CALLBACK_FAILURE
        }
    }
}

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

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    fn jpeg_error(&self, error: libjpeg_turbo_rs::JpegError) -> PipelineError {
        PipelineError::runtime(format!("{}: {error}", self.identifier))
    }

    fn runtime_error(&self, message: impl AsRef<str>) -> PipelineError {
        PipelineError::runtime(format!("{}: {}", self.identifier, message.as_ref()))
    }

    #[cfg(feature = "qpdf-libjpeg-compat")]
    fn decode_with_libjpeg(&mut self, data: &[u8]) -> PipelineResult<()> {
        use std::ffi::CStr;
        use std::os::raw::{c_char, c_void};

        let mut state = CallbackState {
            next: self.next,
            error: None,
        };
        let mut error_message = [0 as c_char; 256];
        let result = unsafe {
            jpeg_compat::flpdf_jpeg_decode_scanlines(
                data.as_ptr(),
                data.len(),
                jpeg_scanline_callback,
                (&mut state as *mut CallbackState<'_>).cast::<c_void>(),
                error_message.as_mut_ptr(),
                error_message.len(),
            )
        };
        let downstream_error = state.error.take();
        drop(state);

        if let Some(error) = downstream_error {
            return Err(error);
        }
        match result {
            jpeg_compat::SUCCESS => self.next.finish(),
            jpeg_compat::CODEC_ERROR => {
                let diagnostic =
                    unsafe { CStr::from_ptr(error_message.as_ptr()) }.to_string_lossy();
                let diagnostic = if diagnostic.is_empty() {
                    "libjpeg decode failed"
                } else {
                    diagnostic.as_ref()
                };
                Err(self.compatibility_codec_error(diagnostic))
            }
            jpeg_compat::CALLBACK_ERROR => Err(self
                .runtime_error("compatibility backend callback failed without a downstream error")),
            status => Err(self.runtime_error(format!(
                "compatibility backend returned unknown status {status}"
            ))),
        }
    }

    #[cfg(feature = "qpdf-libjpeg-compat")]
    fn compatibility_codec_error(&self, diagnostic: &str) -> PipelineError {
        if let Some(precision) = diagnostic.strip_prefix("Unsupported JPEG data precision ") {
            return self.runtime_error(format!(
                "sample precision {precision} (only 8-bit supported)"
            ));
        }

        if diagnostic == "Invalid JPEG file structure: missing SOS marker" {
            return self.runtime_error("unexpected end of data");
        }

        if let Some(bytes) = diagnostic.strip_prefix("Not a JPEG file: starts with 0x") {
            let mut values = bytes.split_whitespace();
            let _first = values.next();
            if let Some(second) = values.next() {
                let second = second.strip_prefix("0x").unwrap_or(second);
                if let Ok(marker) = u8::from_str_radix(second, 16) {
                    return self.runtime_error(format!("unexpected marker: 0xFF{marker:02X}"));
                }
            }
        }
        self.runtime_error(diagnostic)
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
        return self.decode_with_libjpeg(&data);

        #[cfg(not(feature = "qpdf-libjpeg-compat"))]
        {
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

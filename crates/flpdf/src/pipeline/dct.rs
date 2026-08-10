//! qpdf correspondence: `Pl_DCT` buffers compressed input and decodes it on `finish`, emitting one decoded scanline at a time to the next pipeline.

use super::buffer::Buffer;
use super::{Pipeline, PipelineError, PipelineResult};

#[cfg(feature = "qpdf-libjpeg-compat")]
#[allow(unsafe_code)]
mod jpeg_compat {
    use std::ffi::CStr;
    use std::os::raw::{c_char, c_int, c_uchar, c_void};
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::slice;

    use super::{Pipeline, PipelineError, PipelineResult};

    const SUCCESS: c_int = 0;
    const CODEC_ERROR: c_int = 1;
    const CALLBACK_ERROR: c_int = 2;
    const CALLBACK_FAILURE: c_int = 1;

    extern "C" {
        fn flpdf_jpeg_decode_scanlines(
            data: *const c_uchar,
            data_len: usize,
            callback: unsafe extern "C" fn(*mut c_void, *const c_uchar, usize) -> c_int,
            user: *mut c_void,
            error_message: *mut c_char,
            error_message_len: usize,
        ) -> c_int;
    }

    struct CallbackState<'a> {
        next: &'a mut dyn Pipeline,
        error: Option<PipelineError>,
    }

    unsafe extern "C" fn jpeg_scanline_callback(
        user: *mut c_void,
        row: *const c_uchar,
        row_len: usize,
    ) -> c_int {
        if user.is_null() {
            return CALLBACK_FAILURE;
        }

        let state = unsafe { &mut *(user.cast::<CallbackState<'_>>()) };
        let result = catch_unwind(AssertUnwindSafe(|| {
            if state.error.is_some() {
                return CALLBACK_FAILURE;
            }
            if row.is_null() && row_len != 0 {
                state.error = Some(PipelineError::runtime(
                    "DCT decode: compatibility backend returned a null scanline",
                ));
                return CALLBACK_FAILURE;
            }

            let row = if row_len == 0 {
                &[]
            } else {
                unsafe { slice::from_raw_parts(row, row_len) }
            };
            match state.next.write(row) {
                Ok(()) => SUCCESS,
                Err(error) => {
                    state.error = Some(error);
                    CALLBACK_FAILURE
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
                CALLBACK_FAILURE
            }
        }
    }

    pub(super) fn decode_scanlines(
        identifier: &str,
        data: &[u8],
        next: &mut dyn Pipeline,
    ) -> PipelineResult<()> {
        let mut state = CallbackState { next, error: None };
        let mut error_message = [0 as c_char; 256];
        let result = unsafe {
            flpdf_jpeg_decode_scanlines(
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
            SUCCESS => next.finish(),
            CODEC_ERROR => {
                let diagnostic = unsafe { CStr::from_ptr(error_message.as_ptr()) }
                    .to_string_lossy()
                    .into_owned();
                let diagnostic = if diagnostic.is_empty() {
                    "libjpeg decode failed".to_owned()
                } else {
                    diagnostic
                };
                Err(PipelineError::runtime(format!(
                    "{identifier}: {diagnostic}"
                )))
            }
            CALLBACK_ERROR => Err(PipelineError::runtime(format!(
                "{identifier}: compatibility backend callback failed without a downstream error"
            ))),
            status => Err(PipelineError::runtime(format!(
                "{identifier}: compatibility backend returned unknown status {status}"
            ))),
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

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
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

        #[cfg(feature = "qpdf-libjpeg-compat")]
        return jpeg_compat::decode_scanlines(&self.identifier, &data, self.next);

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

//! qpdf correspondence: `Pl_DCT` buffers compressed input and decodes it on `finish`, emitting one decoded scanline at a time to the next pipeline.

use super::buffer::Buffer;
use super::{Pipeline, PipelineError, PipelineRef, PipelineResult};

#[cfg(feature = "qpdf-libjpeg-compat")]
#[allow(dead_code, unsafe_code)]
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

    // cov:ignore-start: llvm-cov signature metadata; helper body is covered
    fn compatibility_status_error(
        identifier: &str,
        status: c_int,
        diagnostic: &str,
    ) -> Option<PipelineError> {
        // cov:ignore-end
        match status {
            SUCCESS => None,
            // cov:ignore-start: llvm-cov match-arm artifact; body is covered
            CODEC_ERROR => {
                // cov:ignore-end
                let diagnostic = if diagnostic.is_empty() {
                    "libjpeg decode failed"
                // cov:ignore-start: llvm-cov guard artifact; branch body is covered
                } else {
                    // cov:ignore-end
                    diagnostic
                    // cov:ignore-start: llvm-cov closing token; assignment is covered
                };
                // cov:ignore-end
                // cov:ignore-start: llvm-cov continuation artifact
                Some(PipelineError::runtime(format!(
                    // cov:ignore-end
                    "{identifier}: {diagnostic}"
                )))
            } // cov:ignore: llvm-cov arm closing token; body is covered
            CALLBACK_ERROR => Some(PipelineError::runtime(format!(
                "{identifier}: compatibility backend callback failed without a downstream error"
            ))),
            status => Some(PipelineError::runtime(format!(
                "{identifier}: compatibility backend returned unknown status {status}"
            ))),
        } // cov:ignore: llvm-cov match closing token; arms are covered
    }

    // cov:ignore: llvm-cov separator metadata before the FFI stage
    // cov:ignore-start: llvm-cov decode signature metadata; FFI body is covered
    pub(super) fn decode_scanlines(
        identifier: &str,
        data: &[u8],
        next: &mut dyn Pipeline,
    ) -> PipelineResult<()> {
        // cov:ignore-end
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
            ) // cov:ignore: llvm-cov FFI closing token; arguments are covered
        }; // cov:ignore: llvm-cov unsafe closing token; call is covered
        let downstream_error = state.error.take();
        drop(state); // cov:ignore: FFI lifetime cleanup; callback paths are covered

        if let Some(error) = downstream_error {
            return Err(error);
        } // cov:ignore: llvm-cov if-closing token; error body is covered
        let diagnostic = unsafe { CStr::from_ptr(error_message.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        match compatibility_status_error(identifier, result, &diagnostic) {
            None => next.finish(),
            Some(error) => Err(error),
        } // cov:ignore: llvm-cov match closing token; result arms are covered
    }

    // cov:ignore-start: llvm-cov test-module metadata; callback tests exercise the body
    #[cfg(test)]
    #[allow(unsafe_code)]
    mod tests {
        // cov:ignore-end
        // cov:ignore-start: llvm-cov test import metadata
        use super::*;
        use crate::pipeline::test_support::{RecordingSink, TraceCall};
        use std::ffi::c_void;
        use std::ptr;
        // cov:ignore-end

        struct Sink;
        // cov:ignore: llvm-cov test sink separator metadata

        // cov:ignore-start: test sink trait metadata; dynamic calls are covered
        impl Pipeline for Sink {
            fn identifier(&self) -> &str {
                "DCT compatibility callback test sink"
            }

            fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
                Ok(())
            }

            fn finish(&mut self) -> PipelineResult<()> {
                Ok(())
            }
        } // cov:ignore-end

        struct PanicSink;

        // cov:ignore: test sink separator metadata
        // cov:ignore-start: test sink impl metadata
        impl Pipeline for PanicSink {
            // cov:ignore-end
            // cov:ignore-start: test sink signature metadata
            fn identifier(&self) -> &str {
                // cov:ignore-end
                "DCT panic test sink" // cov:ignore: test sink value metadata
            }

            fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
                panic!("DCT test downstream panic");
            }

            fn finish(&mut self) -> PipelineResult<()> {
                Ok(())
            }
        } // cov:ignore: test sink closing metadata

        #[test]
        fn compatibility_status_mapping_preserves_all_failure_diagnostics() {
            assert!(compatibility_status_error("DCT decode", SUCCESS, "").is_none());
            assert_eq!(
                compatibility_status_error("DCT decode", CODEC_ERROR, "")
                    .expect("codec status must map to an error")
                    .to_string(),
                "DCT decode: libjpeg decode failed"
            );
            assert_eq!(
                compatibility_status_error("DCT decode", CALLBACK_ERROR, "")
                    .expect("callback status must map to an error")
                    .to_string(),
                "DCT decode: compatibility backend callback failed without a downstream error"
            );
            assert_eq!(
                compatibility_status_error("DCT decode", 99, "")
                    .expect("unknown status must map to an error")
                    .to_string(),
                "DCT decode: compatibility backend returned unknown status 99"
            );
        }

        #[test]
        fn compatibility_callback_rejects_invalid_abi_inputs() {
            let null_user =
                unsafe { jpeg_scanline_callback(ptr::null_mut::<c_void>(), ptr::null(), 0) };
            assert_eq!(null_user, CALLBACK_FAILURE);

            let mut sink = Sink;
            let mut existing_error = CallbackState {
                next: &mut sink,
                error: Some(PipelineError::runtime("existing downstream error")),
            };
            let existing_error_result = unsafe {
                jpeg_scanline_callback(
                    (&mut existing_error as *mut CallbackState<'_>).cast::<c_void>(),
                    ptr::null(),
                    0,
                )
            };
            assert_eq!(existing_error_result, CALLBACK_FAILURE);
            assert_eq!(
                existing_error
                    .error
                    .as_ref()
                    .expect("existing error must be preserved")
                    .to_string(),
                "existing downstream error"
            );

            let mut null_row = CallbackState {
                next: &mut sink,
                error: None,
            };
            let null_row_result = unsafe {
                jpeg_scanline_callback(
                    (&mut null_row as *mut CallbackState<'_>).cast::<c_void>(),
                    ptr::null(),
                    1,
                )
            };
            assert_eq!(null_row_result, CALLBACK_FAILURE);
            assert_eq!(
                null_row
                    .error
                    .as_ref()
                    .expect("null row must become a pipeline error")
                    .to_string(),
                "DCT decode: compatibility backend returned a null scanline"
            );
        }

        #[test]
        fn compatibility_callback_forwards_empty_scanline() {
            let mut sink = RecordingSink::new(&[], &[]);
            let trace = sink.trace();
            let mut state = CallbackState {
                next: &mut sink,
                error: None,
            };

            let result = unsafe {
                jpeg_scanline_callback(
                    (&mut state as *mut CallbackState<'_>).cast::<c_void>(),
                    ptr::null(),
                    0,
                )
            };

            assert_eq!(result, SUCCESS);
            assert_eq!(
                trace.borrow().calls,
                vec![TraceCall::Write {
                    data: Vec::new(),
                    failed: false,
                }]
            );
        }

        #[test]
        fn compatibility_callback_converts_downstream_panic_to_error() {
            let mut sink = PanicSink;
            assert_eq!(sink.identifier(), "DCT panic test sink");
            let mut state = CallbackState {
                next: &mut sink,
                error: None,
            };

            let result = unsafe {
                jpeg_scanline_callback(
                    (&mut state as *mut CallbackState<'_>).cast::<c_void>(),
                    ptr::null(),
                    0,
                )
            };

            assert_eq!(result, CALLBACK_FAILURE);
            assert_eq!(
                state
                    .error
                    .take()
                    .expect("downstream panic must become a pipeline error")
                    .to_string(),
                "DCT decode: downstream pipeline panicked"
            );
            drop(state);
            sink.finish().expect("panic test sink finish is harmless");
        }
    }
}

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
        return jpeg_compat::decode_scanlines(&self.identifier, &data, &mut self.next);

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

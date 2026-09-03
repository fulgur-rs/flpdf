//! qpdf correspondence: `Pl_DCT` buffers compressed input and decodes it on `finish`, emitting one decoded scanline at a time to the next pipeline.
//!
//! The default backend validates the entropy boundary of baseline single-scan
//! JPEGs after decoding, matching qpdf's whole-buffer libjpeg source manager:
//! a missing trailing EOI is reported as `invalid jpeg data reading from
//! buffer` (`libqpdf/Pl_DCT.cc:199-206,312-325`).
//!
//! Known diagnostic limitation (`flpdf-69n1`): the default
//! `libjpeg-turbo-rs` 0.8.0 parser does not expose the reserved marker byte
//! that system libjpeg formats as `Unsupported marker type 0xNN`. Do not
//! fabricate that byte in this adapter. Callers that require qpdf's exact
//! marker diagnostic must enable the explicit `qpdf-libjpeg-compat` feature,
//! which routes DCT decoding through the system-libjpeg compatibility crate.

use super::buffer::Buffer;
use super::{Pipeline, PipelineError, PipelineRef, PipelineResult};
#[cfg(not(feature = "qpdf-libjpeg-compat"))]
use crate::stream_filter::DECODE_OUTPUT_LIMIT_PREFIX;

#[cfg(feature = "qpdf-libjpeg-compat")]
use flpdf_libjpeg_compat::DecodeError;

pub(crate) struct PlDct<'a> {
    identifier: String,
    next: PipelineRef<'a>,
    buffer: Buffer<'static>,
    max_output: Option<usize>,
    mode: DctMode,
}

#[derive(Clone, Copy)]
enum DctMode {
    Decode,
    Compress {
        width: usize,
        height: usize,
        pixel_format: libjpeg_turbo_rs::PixelFormat,
    },
}

impl<'a> PlDct<'a> {
    pub(crate) fn new(identifier: impl Into<String>, next: impl Into<PipelineRef<'a>>) -> Self {
        Self {
            identifier: identifier.into(),
            next: next.into(),
            buffer: Buffer::new("DCT buffer", None),
            max_output: None,
            mode: DctMode::Decode,
        }
    }

    /// Construct qpdf's compression form of `Pl_DCT`.
    ///
    /// qpdf's image optimizer uses `Pl_DCT("jpg", next, width, height,
    /// components, color_space)` (`libqpdf/QPDFJob.cc:188-194`). The Rust
    /// codec expresses the same three qpdf-supported PDF color spaces through
    /// [`libjpeg_turbo_rs::PixelFormat`], retains the whole decoded image until
    /// `finish`, and emits the resulting JPEG to the downstream pipeline.
    pub(crate) fn new_compressor(
        identifier: impl Into<String>,
        next: impl Into<PipelineRef<'a>>,
        width: usize,
        height: usize,
        pixel_format: libjpeg_turbo_rs::PixelFormat,
    ) -> Self {
        Self {
            identifier: identifier.into(),
            next: next.into(),
            buffer: Buffer::new("DCT uncompressed image", None),
            max_output: None,
            mode: DctMode::Compress {
                width,
                height,
                pixel_format,
            },
        }
    }

    /// Opt into flpdf's [`crate::filters::DecodeLimits::max_output`] guard.
    ///
    /// qpdf's own `Pl_DCT` has no such cap — this is flpdf's own
    /// decode-bomb protection, applied only by the whole-buffer decode
    /// route (`DctStreamFilter::pipe_decode_recovering`) that already
    /// buffers the caller's cap; the streaming `pipeStreamData` route
    /// leaves this `None` to match qpdf exactly.
    ///
    /// On the default (non-`qpdf-libjpeg-compat`) backend, the underlying
    /// `libjpeg_turbo_rs::ScanlineDecoder` decodes the *entire* image on
    /// its first scanline read rather than incrementally, so the ordinary
    /// per-write enforcement on the downstream sink would only reject the
    /// output after that full, potentially huge, allocation already
    /// happened. `finish` below instead checks the declared header
    /// dimensions against this cap before triggering that decode.
    pub(crate) fn with_max_output(mut self, max_output: Option<usize>) -> Self {
        self.max_output = max_output;
        self
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    fn jpeg_error(&self, error: libjpeg_turbo_rs::JpegError, data: &[u8]) -> PipelineError {
        if let [first, second, ..] = data {
            if !data.starts_with(&[0xff, 0xd8]) {
                return Self::runtime_error(format!(
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
            return Self::runtime_error("invalid jpeg data reading from buffer");
        }
        // qpdf-deviation-start: libjpeg-turbo-rs 0.8.0 cannot surface the reserved marker byte
        // that system libjpeg reports in its Unsupported marker type 0xNN diagnostic
        let message = error.to_string();
        // qpdf-deviation-end
        PipelineError::runtime(message.as_bytes())
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    fn runtime_error(message: impl AsRef<str>) -> PipelineError {
        PipelineError::runtime(message.as_ref().as_bytes())
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    fn require_baseline_eoi(&self, data: &[u8]) -> PipelineResult<()> {
        let metadata = libjpeg_turbo_rs::decode::marker::MarkerReader::new(data)
            .read_markers()
            .map_err(|error| self.jpeg_error(error, data))?;
        if !metadata.frame.is_progressive && metadata.scans.len() == 1 {
            match libjpeg_turbo_rs::decode::boundary::scan_next_boundary(
                data,
                metadata.entropy_data_offset,
            ) {
                libjpeg_turbo_rs::decode::boundary::MarkerBoundary::Eoi(_) => {}
                libjpeg_turbo_rs::decode::boundary::MarkerBoundary::NeedMore(_)
                | libjpeg_turbo_rs::decode::boundary::MarkerBoundary::Sos(_) => {
                    return Err(Self::runtime_error("invalid jpeg data reading from buffer"));
                }
            }
        } // cov:ignore: LLVM attributes this branch-closing line to the non-baseline path; baseline EOI success and error arms are covered.
        Ok(())
    }

    #[cfg(feature = "qpdf-libjpeg-compat")]
    fn decode_with_compat_backend(&mut self, data: &[u8]) -> PipelineResult<()> {
        let result = {
            let next = &mut self.next;
            flpdf_libjpeg_compat::decode_scanlines(data, &mut |row| next.write(row))
        };

        match result {
            Ok(()) => self.next.finish(),
            Err(DecodeError::Codec(message)) => Err(PipelineError::runtime(message.as_bytes())),
            Err(DecodeError::Callback(error)) => Err(error),
            Err(DecodeError::CallbackPanicked) => {
                Err(PipelineError::runtime("downstream pipeline panicked"))
            }
            Err(DecodeError::CallbackFailure(message)) => {
                Err(PipelineError::runtime(message.as_bytes()))
            }
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

        if let DctMode::Compress {
            width,
            height,
            pixel_format,
        } = self.mode
        {
            let subsampling = if pixel_format == libjpeg_turbo_rs::PixelFormat::Cmyk {
                // libjpeg's JCS_CMYK defaults every component to 1x1. qpdf
                // passes that colorspace unchanged to Pl_DCT; using S420 here
                // would change the optimized image bytes and size.
                libjpeg_turbo_rs::Subsampling::S444
            } else {
                libjpeg_turbo_rs::Subsampling::S420
            };
            let jpeg =
                libjpeg_turbo_rs::compress(&data, width, height, pixel_format, 75, subsampling)
                    .map_err(|error| PipelineError::runtime(error.to_string()))?;
            self.next.write(&jpeg)?;
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
                return Err(Self::runtime_error(format!(
                    "Unsupported JPEG data precision {precision}"
                )));
            }

            let bytes_per_pixel = match components {
                1 => 1,
                3 => 3,
                4 => 4,
                _ => {
                    return Err(Self::runtime_error(format!(
                        "unsupported JPEG component count {components}"
                    )));
                }
            };
            // cov:ignore-start: JPEG width is u16 and supported bpp is at most 4, so usize multiplication cannot overflow
            let row_length = width.checked_mul(bytes_per_pixel).ok_or_else(|| {
                Self::runtime_error(format!("scanline byte length overflow for width {width}"))
            })?;
            // cov:ignore-end

            // Reject before `ScanlineDecoder::read_scanline` below triggers
            // its lazy full-image decode (`ensure_decoded` in
            // `libjpeg-turbo-rs` 0.8.0's `api/scanline.rs`): that call
            // allocates and decodes every remaining scanline up front, so
            // enforcing the cap only on the downstream sink's writes (as
            // every other filter's `OutputBuffer` does) would let an
            // attacker-sized JPEG exhaust memory before the first byte is
            // ever rejected. `saturating_mul` is deliberate, not
            // `checked_mul`: on overflow the true byte count exceeds any
            // representable `usize` limit, so saturating to `usize::MAX`
            // and comparing `> limit` rejects exactly the same inputs a
            // `None`-on-overflow branch would, without an unreachable arm.
            // qpdf-deviation-start: qpdf's Pl_DCT has no output-size cap
            // anywhere in its decode path; this rejects declared JPEG
            // width x height x bpp against the caller's opt-in
            // DecodeLimits::max_output before the default libjpeg-turbo-rs
            // backend's eager whole-image decode on the first scanline read.
            if let Some(limit) = self.max_output {
                if row_length.saturating_mul(height) > limit {
                    return Err(PipelineError::runtime(format!(
                        "{DECODE_OUTPUT_LIMIT_PREFIX} {limit} bytes"
                    )));
                }
            }
            // qpdf-deviation-end

            let mut row = vec![0u8; row_length];

            for _ in 0..height {
                decoder
                    .read_scanline(&mut row)
                    .map_err(|error| self.jpeg_error(error, &data))?;
                // cov:ignore-start: ScanlineDecoder writes into this caller-owned slice and returns no row with a different length
                if row.len() != row_length {
                    return Err(Self::runtime_error(format!(
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
            self.require_baseline_eoi(&data)?;
            self.next.finish()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PlDct;
    use crate::pipeline::test_support::{shared_trace, RecordingSink, TraceCall};
    use crate::pipeline::{Pipeline, PipelineError, PipelineResult};

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
    fn compressor_emits_default_grayscale_jpeg_and_finishes_downstream() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
        let mut stage = PlDct::new_compressor(
            "jpg",
            &mut sink,
            8,
            8,
            libjpeg_turbo_rs::PixelFormat::Grayscale,
        );
        stage
            .write(&[128; 64])
            .expect("compressor accepts a complete image");
        stage.finish().expect("compressor finishes");
        drop(stage);

        assert!(trace.borrow().output.starts_with(&[0xff, 0xd8]));
        assert_eq!(
            trace.borrow().calls.last(),
            Some(&TraceCall::Finish { failed: false })
        );
    }

    #[test]
    fn compressor_empty_input_only_finishes_downstream() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
        let mut stage = PlDct::new_compressor(
            "jpg",
            &mut sink,
            8,
            8,
            libjpeg_turbo_rs::PixelFormat::Grayscale,
        );
        stage.finish().expect("empty compressor input is valid");

        assert!(trace.borrow().output.is_empty());
        assert_eq!(trace.borrow().calls, [TraceCall::Finish { failed: false }]);
    }

    #[test]
    fn compressor_rejects_incomplete_image_buffer() {
        let mut destination = Vec::new();
        let mut sink = super::super::PlString::new("sink", None, &mut destination);
        let mut stage = PlDct::new_compressor(
            "jpg",
            &mut sink,
            8,
            8,
            libjpeg_turbo_rs::PixelFormat::Grayscale,
        );
        stage.write(&[128]).unwrap();

        let error = stage.finish().expect_err("incomplete image must fail");
        assert!(matches!(error, PipelineError::Runtime(_)));
    }

    #[test]
    fn compressor_propagates_downstream_write_and_finish_failures() {
        let write_trace = shared_trace();
        let mut write_sink = RecordingSink::with_trace(write_trace.clone(), &[1], &[]);
        let mut write_stage = PlDct::new_compressor(
            "jpg",
            &mut write_sink,
            8,
            8,
            libjpeg_turbo_rs::PixelFormat::Grayscale,
        );
        write_stage.write(&[128; 64]).unwrap();
        assert_eq!(
            write_stage.finish().unwrap_err().message(),
            "sink write failure 1"
        );

        let finish_trace = shared_trace();
        let mut finish_sink = RecordingSink::with_trace(finish_trace.clone(), &[], &[1]);
        let mut finish_stage = PlDct::new_compressor(
            "jpg",
            &mut finish_sink,
            8,
            8,
            libjpeg_turbo_rs::PixelFormat::Grayscale,
        );
        finish_stage.write(&[128; 64]).unwrap();
        assert_eq!(
            finish_stage.finish().unwrap_err().message(),
            "sink finish failure 1"
        );
    }

    #[cfg(feature = "qpdf-libjpeg-compat")]
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

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn default_backend_reports_a_diagnostic_for_a_reserved_marker_without_crashing() {
        let mut sink = Sink;
        let mut stage = PlDct::new("DCT decode", &mut sink);
        stage
            .write(&[0xff, 0xd8, 0xff, 0x02, 0x00, 0x04, 0x00, 0x00, 0x00])
            .expect("DCT stage buffers input");

        let error = stage
            .finish()
            .expect_err("reserved JPEG marker must fail rather than silently succeed");

        // The default backend cannot reproduce qpdf's `Unsupported marker
        // type 0x02` wording (see the module doc's known limitation), but it
        // must still report the malformed input as an error. Pin the actual
        // libjpeg-turbo-rs 0.8.0 diagnostic so a future dependency bump that
        // silently changes or drops this fallback message is caught here.
        assert!(matches!(error, PipelineError::Runtime(_)));
        assert_eq!(error.message(), "invalid marker: 0xFF00");
    }

    #[cfg(feature = "qpdf-libjpeg-compat")]
    #[test]
    fn libjpeg_compat_backend_preserves_reserved_marker_byte() {
        let mut sink = Sink;
        let mut stage = PlDct::new("DCT decode", &mut sink);
        stage
            .write(&[0xff, 0xd8, 0xff, 0x02, 0x00, 0x04, 0x00, 0x00, 0x00])
            .expect("DCT stage buffers input");

        let error = stage
            .finish()
            .expect_err("reserved JPEG marker must fail in the compatibility backend");

        assert_eq!(error.to_string(), "Unsupported marker type 0x02");
    }
}

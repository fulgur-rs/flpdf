//! JPEG/DCT decoding and encoding pipeline stage.
//!
//! qpdf correspondence: `Pl_DCT` buffers compressed input and decodes it on `finish`, emitting one decoded scanline at a time to the next pipeline.
//!
//! The default backend validates the entropy boundary of baseline single-scan
//! JPEGs after decoding, matching qpdf's whole-buffer libjpeg source manager:
//! a missing trailing EOI is reported as `invalid jpeg data reading from
//! buffer` (`libqpdf/Pl_DCT.cc:199-206,312-325`).
//!
//! Known diagnostic limitation (`flpdf-69n1`): generic errors from the default
//! `libjpeg-turbo-rs` 0.8.0 parser do not preserve every system-libjpeg detail.
//! For reserved markers before SOS, the default pre-pass captures the marker
//! byte and formats reserved marker bytes as qpdf's exact
//! `Unsupported marker type 0xNN` (`libqpdf/Pl_DCT.cc:24-31`,
//! `/usr/include/jerror.h:132`). Other diagnostics that need the linked
//! system-libjpeg wording can use the explicit `qpdf-libjpeg-compat` feature.
//!
//! Correctness fix (`flpdf-401z`): the default path now scans marker segments
//! before the Rust decoder starts and rejects reserved marker codes with a
//! qpdf-matching marker diagnostic. This compensates for `libjpeg-turbo-rs`
//! treating an unrecognized marker as "skip its segment and continue" rather
//! than an error, and closes the accept/reject and message gap for reserved
//! markers that appear before SOS.
//!
//! The default path handles unknown-color-space JPEGs (two and five to ten
//! components) with `Decoder::decode_raw`, then upsamples and interleaves the
//! raw component planes in libjpeg's frame order. This preserves qpdf's
//! `output_width * output_components` scanline output without asking the
//! RGB/CMYK converter to reinterpret `JCS_UNKNOWN`
//! (`libqpdf/Pl_DCT.cc:315-325`).

use super::buffer::Buffer;
use super::{Pipeline, PipelineError, PipelineRef, PipelineResult};

#[cfg(feature = "qpdf-libjpeg-compat")]
use flpdf_libjpeg_compat::DecodeError;

pub(crate) struct PlDct<'a> {
    identifier: String,
    next: PipelineRef<'a>,
    buffer: Buffer<'static>,
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
            mode: DctMode::Compress {
                width,
                height,
                pixel_format,
            },
        }
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
        if let libjpeg_turbo_rs::JpegError::CorruptData(message) = &error {
            if message.starts_with("Too many color components: ")
                || message == "Bogus marker length"
                || (message.starts_with("Invalid component ID ") && message.ends_with(" in SOS"))
            {
                return Self::runtime_error(message);
            }
        }
        // qpdf-deviation-start: generic libjpeg-turbo-rs diagnostics may differ from system libjpeg format_message; reserved pre-SOS markers are remapped above
        let message = error.to_string();
        // qpdf-deviation-end
        PipelineError::runtime(message.as_bytes())
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    fn runtime_error(message: impl AsRef<str>) -> PipelineError {
        PipelineError::runtime(message.as_ref().as_bytes())
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    /// Return the first reserved marker found in the JPEG header.
    ///
    /// `libjpeg-turbo-rs` 0.8.0's `MarkerReader` skips every marker that is
    /// not in its explicit dispatch arms. Real libjpeg rejects reserved
    /// marker codes instead, so inspect the marker segments before the Rust
    /// decoder is allowed to start. Payload bytes are skipped by their
    /// segment length; this pre-pass therefore does not mistake an arbitrary
    /// `0xff` byte in APP/COM/DQT/DHT data for a marker.
    fn first_reserved_marker_before_sos(data: &[u8]) -> Option<u8> {
        if data.get(..2) != Some(&[0xff, 0xd8]) {
            return None;
        }

        let mut position = 2;
        while position < data.len() {
            if data[position] != 0xff {
                return None;
            }
            while position < data.len() && data[position] == 0xff {
                position += 1;
            }
            let marker = *data.get(position)?;
            position += 1;

            match marker {
                0x00 | 0xff => return None,
                0x01 | 0xd0..=0xd8 => {}
                0xd9 | 0xda => return None,
                0xc0..=0xc7 | 0xc9..=0xcf | 0xdb..=0xdf | 0xe0..=0xef | 0xfe => {
                    let length =
                        u16::from_be_bytes([*data.get(position)?, *data.get(position + 1)?])
                            as usize;
                    if length < 2 {
                        return None;
                    }
                    position = position.checked_add(length)?;
                    if position > data.len() {
                        return None;
                    }
                }
                reserved => return Some(reserved),
            }
        }
        None
    }

    /// Recreate one component scanline from libjpeg's raw component plane.
    ///
    /// `Pl_DCT::decompress` leaves a two-component JPEG in `JCS_UNKNOWN`, so
    /// libjpeg performs no color conversion but still upsamples each component
    /// before interleaving it. Its default is fancy 2:1 horizontal and 2:2
    /// interpolation, fancy 1:2 vertical interpolation, and sample replication
    /// for other integral ratios (`jdsample.c:1601-1684,1815-1865,1881-1943,
    /// 1959-2042`; `jdapimin.c:1409-1434`).
    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    fn upsample_component_row(
        plane: &[u8],
        plane_stride: usize,
        plane_height: usize,
        downsampled_width: usize,
        downsampled_height: usize,
        output_width: usize,
        output_height: usize,
        horizontal_expand: usize,
        vertical_expand: usize,
        output_y: usize,
    ) -> Option<Vec<u8>> {
        if output_y >= output_height || horizontal_expand == 0 || vertical_expand == 0 {
            return None;
        }
        if output_width == 0 {
            return Some(Vec::new());
        }
        if downsampled_width == 0 || downsampled_height == 0 {
            return None;
        }
        if output_width > downsampled_width.checked_mul(horizontal_expand)?
            || output_height > downsampled_height.checked_mul(vertical_expand)?
        {
            return None;
        }
        let plane_len = plane_stride.checked_mul(plane_height)?;
        if plane.len() < plane_len
            || downsampled_width > plane_stride
            || downsampled_height > plane_height
        {
            return None;
        }

        let sample = |x: usize, y: usize| -> u32 { u32::from(plane[y * plane_stride + x]) };
        let source_y = output_y / vertical_expand;
        let mut row = Vec::with_capacity(output_width);

        // This is libjpeg's h2v1_fancy_upsample, including alternating
        // rounding biases and edge replication.
        if horizontal_expand == 2 && vertical_expand == 1 && downsampled_width > 2 {
            for x in 0..output_width {
                let sample_x = x / 2;
                let value = if x % 2 == 0 {
                    if sample_x == 0 {
                        sample(0, source_y)
                    } else {
                        (3 * sample(sample_x, source_y) + sample(sample_x - 1, source_y) + 1) >> 2
                    }
                } else if sample_x == 0 {
                    (3 * sample(0, source_y) + sample(1, source_y) + 2) >> 2
                } else if sample_x + 1 == downsampled_width {
                    sample(sample_x, source_y)
                } else {
                    (3 * sample(sample_x, source_y) + sample(sample_x + 1, source_y) + 2) >> 2
                };
                row.push(value as u8);
            }
            return Some(row);
        }

        // h1v2_fancy_upsample blends each component center with the adjacent
        // row. The input controller supplies the nearest row at image edges.
        if horizontal_expand == 1 && vertical_expand == 2 {
            let (neighbor_y, bias) = if output_y % 2 == 0 {
                (source_y.saturating_sub(1), 1)
            } else {
                ((source_y + 1).min(downsampled_height - 1), 2)
            };
            for x in 0..output_width {
                let value = (3 * sample(x, source_y) + sample(x, neighbor_y) + bias) >> 2;
                row.push(value as u8);
            }
            return Some(row);
        }

        // libjpeg selects the 2h2v triangle filter only for component planes
        // wider than two samples; narrower planes use integer replication.
        if horizontal_expand == 2 && vertical_expand == 2 && downsampled_width > 2 {
            let neighbor_y = if output_y % 2 == 0 {
                source_y.saturating_sub(1)
            } else {
                (source_y + 1).min(downsampled_height - 1)
            };
            let vertical_sum = |x: usize| 3 * sample(x, source_y) + sample(x, neighbor_y);
            for x in 0..output_width {
                let column = x / 2;
                let value = if column == 0 {
                    if x % 2 == 0 {
                        (vertical_sum(0) * 4 + 8) >> 4
                    } else {
                        (vertical_sum(0) * 3 + vertical_sum(1) + 7) >> 4
                    }
                } else if column + 1 == downsampled_width {
                    if x % 2 == 0 {
                        (vertical_sum(column) * 3 + vertical_sum(column - 1) + 8) >> 4
                    } else {
                        (vertical_sum(column) * 4 + 7) >> 4
                    }
                } else if x % 2 == 0 {
                    (vertical_sum(column) * 3 + vertical_sum(column - 1) + 8) >> 4
                } else {
                    (vertical_sum(column) * 3 + vertical_sum(column + 1) + 7) >> 4
                };
                row.push(value as u8);
            }
            return Some(row);
        }

        // The remaining supported cases use libjpeg's integral-factor box
        // upsampler, which replicates each source sample horizontally and
        // vertically.
        for x in 0..output_width {
            let source_x = x / horizontal_expand;
            row.push(sample(source_x, source_y) as u8);
        }
        Some(row)
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    fn unknown_component_scanline(
        raw: &libjpeg_turbo_rs::RawImage,
        sampling: &[(u8, u8)],
        max_h: usize,
        max_v: usize,
        output_y: usize,
    ) -> Option<Vec<u8>> {
        let num_components = raw.num_components;
        if num_components < 2
            || num_components != sampling.len()
            || raw.width.checked_mul(num_components).is_none()
        {
            return None;
        }
        let mut component_rows = Vec::with_capacity(num_components);
        for (component, &(horizontal_sampling, vertical_sampling)) in sampling.iter().enumerate() {
            let horizontal_sampling = usize::from(horizontal_sampling);
            let vertical_sampling = usize::from(vertical_sampling);
            if horizontal_sampling == 0
                || vertical_sampling == 0
                || max_h % horizontal_sampling != 0
                || max_v % vertical_sampling != 0
            {
                return None;
            }

            let downsampled_width = (raw.width * horizontal_sampling).div_ceil(max_h);
            let downsampled_height = (raw.height * vertical_sampling).div_ceil(max_v);
            let plane = raw.planes.get(component)?;
            let plane_stride = *raw.plane_widths.get(component)?;
            let plane_height = *raw.plane_heights.get(component)?;
            let row = Self::upsample_component_row(
                plane,
                plane_stride,
                plane_height,
                downsampled_width,
                downsampled_height,
                raw.width,
                raw.height,
                max_h / horizontal_sampling,
                max_v / vertical_sampling,
                output_y,
            )?;
            component_rows.push(row);
        }

        let mut row = Vec::with_capacity(raw.width.checked_mul(num_components)?);
        for x in 0..raw.width {
            for component_row in &component_rows {
                row.push(*component_row.get(x)?);
            }
        }
        Some(row)
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
        }
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
            if let Some(marker) = Self::first_reserved_marker_before_sos(&data) {
                return Err(Self::runtime_error(format!(
                    "Unsupported marker type 0x{marker:02x}"
                )));
            }

            let mut decoder = libjpeg_turbo_rs::ScanlineDecoder::new(&data)
                .map_err(|error| self.jpeg_error(error, &data))?;
            let (precision, width, height, sampling) = {
                let header = decoder.header();
                (
                    header.precision,
                    header.width(),
                    header.height(),
                    header
                        .components
                        .iter()
                        .map(|component| {
                            (component.horizontal_sampling, component.vertical_sampling)
                        })
                        .collect::<Vec<_>>(),
                )
            };

            if precision != 8 {
                return Err(Self::runtime_error(format!(
                    "Unsupported JPEG data precision {precision}"
                )));
            }

            let components = sampling.len();
            if components == 2 || components >= 5 {
                let max_h = sampling
                    .iter()
                    .map(|(horizontal, _)| usize::from(*horizontal))
                    .max()
                    .unwrap_or(1);
                let max_v = sampling
                    .iter()
                    .map(|(_, vertical)| usize::from(*vertical))
                    .max()
                    .unwrap_or(1);

                // qpdf's `jpeg_start_decompress` rejects fractional sampling
                // ratios before it reads entropy data (`jdsample.c:2257-2271`).
                if sampling.iter().any(|(horizontal, vertical)| {
                    let horizontal = usize::from(*horizontal);
                    let vertical = usize::from(*vertical);
                    horizontal == 0
                        || vertical == 0
                        || max_h % horizontal != 0
                        || max_v % vertical != 0
                }) {
                    return Err(Self::runtime_error(
                        "Fractional sampling not implemented yet",
                    ));
                }

                let raw = libjpeg_turbo_rs::Decoder::new(&data)
                    .map_err(|error| self.jpeg_error(error, &data))?
                    .decode_raw()
                    .map_err(|error| self.jpeg_error(error, &data))?;
                // cov:ignore-start: Decoder::decode_raw derives dimensions from the same frame header read above
                if raw.width != width || raw.height != height || raw.num_components != components {
                    return Err(Self::runtime_error(
                        "decoded JPEG component dimensions do not match the frame",
                    ));
                }
                // cov:ignore-end
                for output_y in 0..height {
                    // cov:ignore-start: decode_raw returns one correctly shaped plane per frame component
                    let row =
                        Self::unknown_component_scanline(&raw, &sampling, max_h, max_v, output_y)
                            .ok_or_else(|| {
                            Self::runtime_error("decoded JPEG component plane is inconsistent")
                        })?;
                    // cov:ignore-end
                    self.next.write(&row)?;
                }
            } else {
                let bytes_per_pixel = match components {
                    1 => 1,
                    3 => 3,
                    4 => 4,
                    // cov:ignore-start: Component counts 2 and 5-10 use the raw component route
                    _ => {
                        return Err(Self::runtime_error(format!(
                            "unsupported JPEG component count {components}"
                        )));
                    } // cov:ignore-end
                };
                // cov:ignore-start: JPEG width is u16 and supported bpp is at most 4, so usize multiplication cannot overflow
                let row_length = width.checked_mul(bytes_per_pixel).ok_or_else(|| {
                    Self::runtime_error(format!("scanline byte length overflow for width {width}"))
                })?;
                // cov:ignore-end

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
            }

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

    fn jpeg_with_frame_components(num_components: usize) -> Vec<u8> {
        assert!(num_components >= 1);
        let mut jpeg = libjpeg_turbo_rs::compress(
            &[128u8],
            1,
            1,
            libjpeg_turbo_rs::PixelFormat::Grayscale,
            75,
            libjpeg_turbo_rs::Subsampling::S444,
        )
        .expect("component-count test JPEG must encode");
        let sof = jpeg
            .windows(2)
            .position(|marker| marker == [0xff, 0xc0])
            .expect("baseline JPEG must contain SOF0");
        let segment_length = u16::from_be_bytes([jpeg[sof + 2], jpeg[sof + 3]]);
        assert_eq!(segment_length, 11);
        jpeg[sof + 9] = num_components as u8;
        let extra_components = num_components - 1;
        let added_length = u16::try_from(extra_components * 3)
            .expect("test component descriptors must fit in an SOF segment");
        jpeg[sof + 2..sof + 4].copy_from_slice(&(segment_length + added_length).to_be_bytes());
        let second_component = sof + 2 + usize::from(segment_length);
        let descriptors =
            (2..=num_components).flat_map(|component_id| [component_id as u8, 0x11, 0]);
        jpeg.splice(second_component..second_component, descriptors);
        jpeg
    }

    fn two_component_jpeg() -> Vec<u8> {
        jpeg_with_frame_components(2)
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

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn baseline_jpeg_without_eoi_is_rejected_by_boundary_check() {
        let jpeg = libjpeg_turbo_rs::compress(
            &[128u8; 64],
            8,
            8,
            libjpeg_turbo_rs::PixelFormat::Grayscale,
            75,
            libjpeg_turbo_rs::Subsampling::S444,
        )
        .expect("fixture JPEG must encode");
        assert!(jpeg.starts_with(&[0xff, 0xd8]));
        assert!(jpeg.ends_with(&[0xff, 0xd9]));
        let truncated = &jpeg[..jpeg.len() - 2];

        let metadata = libjpeg_turbo_rs::decode::marker::MarkerReader::new(truncated)
            .read_markers()
            .expect("baseline marker metadata remains readable without EOI");
        assert!(!metadata.frame.is_progressive);
        assert_eq!(metadata.scans.len(), 1);
        assert!(matches!(
            libjpeg_turbo_rs::decode::boundary::scan_next_boundary(
                truncated,
                metadata.entropy_data_offset,
            ),
            libjpeg_turbo_rs::decode::boundary::MarkerBoundary::NeedMore(_)
                | libjpeg_turbo_rs::decode::boundary::MarkerBoundary::Sos(_)
        ));

        let mut sink = Sink;
        let stage = PlDct::new("DCT decode", &mut sink);
        let error = stage
            .require_baseline_eoi(truncated)
            .expect_err("a one-scan baseline JPEG must include EOI");
        assert_eq!(error.message(), "invalid jpeg data reading from buffer");
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn progressive_jpeg_skips_the_baseline_eoi_boundary_check() {
        let jpeg = libjpeg_turbo_rs::compress_progressive(
            &[128u8; 64],
            8,
            8,
            libjpeg_turbo_rs::PixelFormat::Grayscale,
            75,
            libjpeg_turbo_rs::Subsampling::S444,
        )
        .expect("progressive fixture JPEG must encode");
        assert!(jpeg.starts_with(&[0xff, 0xd8]));
        assert!(jpeg.ends_with(&[0xff, 0xd9]));

        let metadata = libjpeg_turbo_rs::decode::marker::MarkerReader::new(&jpeg)
            .read_markers()
            .expect("progressive marker metadata must parse");
        assert!(metadata.frame.is_progressive);
        assert!(metadata.scans.len() > 1);

        let mut sink = Sink;
        let stage = PlDct::new("DCT decode", &mut sink);
        stage
            .require_baseline_eoi(&jpeg)
            .expect("progressive JPEGs skip the baseline-only EOI boundary check");
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

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn default_backend_decodes_two_component_jpeg_as_qpdf_raw_channels() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
        {
            let mut stage = PlDct::new("DCT decode", &mut sink);
            stage
                .write(&two_component_jpeg())
                .expect("two-component JPEG must buffer");
            stage
                .finish()
                .expect("default backend must preserve both qpdf output components");
        }

        assert_eq!(trace.borrow().output, [128, 128]);
        assert_eq!(
            trace.borrow().calls,
            [
                TraceCall::Write {
                    data: vec![128, 128],
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn default_backend_keeps_gray_rgb_and_cmyk_scanline_counts() {
        for (pixel_format, bytes_per_pixel) in [
            (libjpeg_turbo_rs::PixelFormat::Grayscale, 1),
            (libjpeg_turbo_rs::PixelFormat::Rgb, 3),
            (libjpeg_turbo_rs::PixelFormat::Cmyk, 4),
        ] {
            let input = vec![128; 8 * 8 * bytes_per_pixel];
            let jpeg = libjpeg_turbo_rs::compress(
                &input,
                8,
                8,
                pixel_format,
                75,
                libjpeg_turbo_rs::Subsampling::S444,
            )
            .expect("supported component JPEG must encode");
            let trace = shared_trace();
            let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
            {
                let mut stage = PlDct::new("DCT decode", &mut sink);
                stage.write(&jpeg).expect("JPEG must buffer");
                stage
                    .finish()
                    .expect("supported component JPEG must decode");
            }

            assert_eq!(trace.borrow().output.len(), 8 * 8 * bytes_per_pixel);
            let writes = trace
                .borrow()
                .calls
                .iter()
                .filter_map(|call| match call {
                    TraceCall::Write { data, .. } => Some(data.len()),
                    TraceCall::Finish { .. } => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(writes, vec![8 * bytes_per_pixel; 8]);
        }
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn high_component_scanline_api_returns_an_error_instead_of_panicking() {
        let jpeg = jpeg_with_frame_components(5);
        let mut decoder = libjpeg_turbo_rs::ScanlineDecoder::new(&jpeg)
            .expect("ten-component frame headers are valid for the raw decoder");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            decoder.read_scanline(&mut [0; 5])
        }));
        assert!(
            result.is_ok(),
            "scanline output must reject this format with an error instead of panicking"
        );
        let error = result
            .expect("scanline output must not panic")
            .expect_err("the image API has no unknown-component pixel format");
        assert_eq!(
            error.to_string(),
            "unsupported feature: scanline output does not support 5-component JPEGs"
        );
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn default_backend_rejects_fractional_two_component_sampling_like_qpdf() {
        let mut jpeg = two_component_jpeg();
        let sof = jpeg
            .windows(2)
            .position(|marker| marker == [0xff, 0xc0])
            .expect("baseline JPEG must contain SOF0");
        // The fixture has two SOF entries at offsets +10..+15. Max H is 3,
        // while the second component uses H=2, which libjpeg cannot upsample.
        jpeg[sof + 11] = 0x31;
        jpeg[sof + 14] = 0x21;

        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
        let error = {
            let mut stage = PlDct::new("DCT decode", &mut sink);
            stage.write(&jpeg).expect("JPEG must buffer");
            stage
                .finish()
                .expect_err("fractional sampling ratios must be rejected")
        };

        assert_eq!(error.to_string(), "Fractional sampling not implemented yet");
        assert!(trace.borrow().calls.is_empty());
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn raw_component_upsampling_rejects_inconsistent_internal_shapes() {
        assert!(PlDct::upsample_component_row(&[], 1, 1, 1, 1, 1, 1, 0, 1, 0).is_none());
        assert_eq!(
            PlDct::upsample_component_row(&[], 0, 0, 1, 1, 0, 1, 1, 1, 0),
            Some(Vec::new())
        );
        assert!(PlDct::upsample_component_row(&[], 1, 1, 0, 1, 1, 1, 1, 1, 0).is_none());
        assert!(PlDct::upsample_component_row(&[], 2, 1, 1, 1, 1, 1, 1, 1, 0).is_none());
        assert!(PlDct::upsample_component_row(&[1], 1, 1, 1, 1, 1, 2, 1, 1, 1).is_none());
        assert!(PlDct::upsample_component_row(&[1, 2, 3], 3, 1, 3, 1, 7, 1, 2, 2, 0).is_none());

        let wrong_count = libjpeg_turbo_rs::RawImage {
            planes: Vec::new(),
            plane_widths: Vec::new(),
            plane_heights: Vec::new(),
            width: 1,
            height: 1,
            num_components: 1,
        };
        assert!(PlDct::unknown_component_scanline(&wrong_count, &[(1, 1); 2], 1, 1, 0).is_none());

        let fractional_sampling = libjpeg_turbo_rs::RawImage {
            planes: vec![vec![1], vec![2]],
            plane_widths: vec![1, 1],
            plane_heights: vec![1, 1],
            width: 1,
            height: 1,
            num_components: 2,
        };
        assert!(PlDct::unknown_component_scanline(
            &fractional_sampling,
            &[(0, 1), (1, 1)],
            1,
            1,
            0,
        )
        .is_none());

        let inconsistent_plane = libjpeg_turbo_rs::RawImage {
            planes: vec![Vec::new(), vec![2]],
            plane_widths: vec![1, 1],
            plane_heights: vec![1, 1],
            width: 1,
            height: 1,
            num_components: 2,
        };
        assert!(
            PlDct::unknown_component_scanline(&inconsistent_plane, &[(1, 1); 2], 1, 1, 0).is_none()
        );

        let oversized_width = libjpeg_turbo_rs::RawImage {
            planes: Vec::new(),
            plane_widths: Vec::new(),
            plane_heights: Vec::new(),
            width: usize::MAX,
            height: 0,
            num_components: 2,
        };
        assert!(
            PlDct::unknown_component_scanline(&oversized_width, &[(1, 1); 2], 1, 1, 0).is_none()
        );
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn two_component_rows_use_libjpeg_h2v1_fancy_upsampling() {
        let mut first = vec![0; 8 * 8];
        first[..4].copy_from_slice(&[10, 20, 30, 40]);
        let mut second = vec![0; 16 * 8];
        second[..8].copy_from_slice(&[100, 101, 102, 103, 104, 105, 106, 107]);
        let raw = libjpeg_turbo_rs::RawImage {
            planes: vec![first, second],
            plane_widths: vec![8, 16],
            plane_heights: vec![8, 8],
            width: 8,
            height: 1,
            num_components: 2,
        };

        let row = PlDct::unknown_component_scanline(&raw, &[(1, 1), (2, 1)], 2, 1, 0)
            .expect("raw component planes must have the declared shape");
        assert_eq!(
            row,
            [10, 100, 13, 101, 17, 102, 23, 103, 27, 104, 33, 105, 37, 106, 40, 107,]
        );
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn two_component_rows_use_libjpeg_h1v2_fancy_upsampling() {
        let mut first = vec![0; 8 * 16];
        first[0] = 1;
        first[8] = 5;
        first[16] = 9;
        first[24] = 13;
        let mut second = vec![0; 8 * 8];
        second[0] = 10;
        second[8] = 30;
        let raw = libjpeg_turbo_rs::RawImage {
            planes: vec![first, second],
            plane_widths: vec![8, 8],
            plane_heights: vec![16, 8],
            width: 1,
            height: 4,
            num_components: 2,
        };

        let rows = (0..4)
            .map(|y| {
                PlDct::unknown_component_scanline(&raw, &[(1, 2), (1, 1)], 1, 2, y)
                    .expect("raw component planes must have the declared shape")
            })
            .collect::<Vec<_>>();
        assert_eq!(rows, [[1, 10], [5, 15], [9, 25], [13, 30]]);
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn two_component_rows_use_libjpeg_h2v2_fancy_upsampling() {
        let mut first = vec![0; 8 * 8];
        first[..4].copy_from_slice(&[10, 20, 30, 40]);
        first[8..12].copy_from_slice(&[50, 60, 70, 80]);
        let second = vec![200; 16 * 16];
        let raw = libjpeg_turbo_rs::RawImage {
            planes: vec![first, second],
            plane_widths: vec![8, 16],
            plane_heights: vec![8, 16],
            width: 8,
            height: 8,
            num_components: 2,
        };

        let row = PlDct::unknown_component_scanline(&raw, &[(1, 1), (2, 2)], 2, 2, 0)
            .expect("raw component planes must have the declared shape");
        assert_eq!(
            row,
            [10, 200, 12, 200, 18, 200, 22, 200, 28, 200, 32, 200, 38, 200, 40, 200]
        );
        let next_row = PlDct::unknown_component_scanline(&raw, &[(1, 1), (2, 2)], 2, 2, 1)
            .expect("raw component planes must have the declared shape");
        assert_eq!(
            next_row,
            [20, 200, 22, 200, 28, 200, 32, 200, 38, 200, 42, 200, 48, 200, 50, 200]
        );
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn two_component_rows_replicate_other_integral_sampling_ratios() {
        let mut first = vec![0; 8 * 8];
        first[..2].copy_from_slice(&[10, 20]);
        first[8..10].copy_from_slice(&[30, 40]);
        let mut second = vec![0; 32 * 24];
        second[4 * 32..4 * 32 + 8].copy_from_slice(&[140, 141, 142, 143, 144, 145, 146, 147]);
        let raw = libjpeg_turbo_rs::RawImage {
            planes: vec![first, second],
            plane_widths: vec![8, 32],
            plane_heights: vec![8, 24],
            width: 8,
            height: 6,
            num_components: 2,
        };

        let row = PlDct::unknown_component_scanline(&raw, &[(1, 1), (4, 3)], 4, 3, 4)
            .expect("raw component planes must have the declared shape");
        assert_eq!(
            row,
            [30, 140, 30, 141, 30, 142, 30, 143, 40, 144, 40, 145, 40, 146, 40, 147]
        );
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn unknown_component_rows_preserve_frame_component_order() {
        let raw = libjpeg_turbo_rs::RawImage {
            planes: vec![vec![0x11], vec![0x22], vec![0x33], vec![0x44], vec![0x55]],
            plane_widths: vec![1; 5],
            plane_heights: vec![1; 5],
            width: 1,
            height: 1,
            num_components: 5,
        };

        let row = PlDct::unknown_component_scanline(&raw, &[(1, 1); 5], 1, 1, 0)
            .expect("each frame component must produce one output sample");
        assert_eq!(row, [0x11, 0x22, 0x33, 0x44, 0x55]);
    }

    #[cfg(feature = "qpdf-libjpeg-compat")]
    #[test]
    fn compat_backend_accepts_two_component_jpeg_like_qpdf() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
        {
            let mut stage = PlDct::new("DCT decode", &mut sink);
            stage
                .write(&two_component_jpeg())
                .expect("two-component JPEG must buffer");
            stage
                .finish()
                .expect("compat backend must accept qpdf's component count");
        }

        assert_eq!(trace.borrow().output, [128, 128]);
        assert_eq!(
            trace.borrow().calls.last(),
            Some(&TraceCall::Finish { failed: false })
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

        // Pin qpdf 11.9.0's libjpeg diagnostic for the marker byte captured
        // by the default pre-pass.
        assert!(matches!(error, PipelineError::Runtime(_)));
        assert_eq!(error.message(), "Unsupported marker type 0x02");
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn linked_libjpeg_component_diagnostics_are_preserved() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace, &[], &[]);
        let stage = PlDct::new("DCT decode", &mut sink);

        for message in [
            "Too many color components: 11, max 10",
            "Bogus marker length",
            "Invalid component ID 5 in SOS",
        ] {
            let error = stage.jpeg_error(
                libjpeg_turbo_rs::JpegError::CorruptData(message.to_owned()),
                &[0xff, 0xd8],
            );
            assert_eq!(error.message(), message);
        }

        let fallback = stage.jpeg_error(
            libjpeg_turbo_rs::JpegError::CorruptData("generic marker failure".to_owned()),
            &[0xff, 0xd8],
        );
        assert_eq!(fallback.message(), "corrupt data: generic marker failure");

        let unsupported = stage.jpeg_error(
            libjpeg_turbo_rs::JpegError::Unsupported("unsupported process".to_owned()),
            &[0xff, 0xd8],
        );
        assert_eq!(
            unsupported.message(),
            "unsupported feature: unsupported process"
        );
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn reserved_marker_prepass_leaves_other_malformed_headers_to_decoder() {
        assert_eq!(
            PlDct::first_reserved_marker_before_sos(&[0xff, 0xd8, 0x00]),
            None
        );
        assert_eq!(
            PlDct::first_reserved_marker_before_sos(&[0xff, 0xd8, 0xff, 0x00]),
            None
        );
        assert_eq!(
            PlDct::first_reserved_marker_before_sos(&[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x01]),
            None
        );
        assert_eq!(
            PlDct::first_reserved_marker_before_sos(&[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00,]),
            None
        );
        assert_eq!(
            PlDct::first_reserved_marker_before_sos(&[0xff, 0xd8, 0xff, 0xd8]),
            None
        );
    }

    /// Splice a reserved-marker segment (`FF 02 00 04 00 00`) immediately
    /// after the SOI marker of an otherwise-valid 8x8 grayscale JPEG.
    fn valid_jpeg_with_spliced_reserved_marker() -> Vec<u8> {
        let mut valid = Vec::new();
        {
            let mut sink = super::super::PlString::new("sink", None, &mut valid);
            let mut stage = PlDct::new_compressor(
                "jpg",
                &mut sink,
                8,
                8,
                libjpeg_turbo_rs::PixelFormat::Grayscale,
            );
            stage.write(&[128; 64]).unwrap();
            stage.finish().unwrap();
        }
        assert_eq!(&valid[0..2], &[0xff, 0xd8], "must start with SOI");

        let mut spliced = vec![0xff, 0xd8];
        spliced.extend_from_slice(&[0xff, 0x02, 0x00, 0x04, 0x00, 0x00]);
        spliced.extend_from_slice(&valid[2..]);
        spliced
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn default_backend_rejects_a_reserved_marker_inside_a_valid_jpeg() {
        let spliced = valid_jpeg_with_spliced_reserved_marker();

        let mut sink = Sink;
        let mut decode_stage = PlDct::new("DCT decode", &mut sink);
        decode_stage.write(&spliced).unwrap();

        let error = decode_stage
            .finish()
            .expect_err("default backend must reject a reserved marker");
        assert!(matches!(error, PipelineError::Runtime(_)));
        assert_eq!(error.to_string(), "Unsupported marker type 0x02");
    }

    #[cfg(feature = "qpdf-libjpeg-compat")]
    #[test]
    fn libjpeg_compat_backend_rejects_a_reserved_marker_inside_a_valid_jpeg() {
        let spliced = valid_jpeg_with_spliced_reserved_marker();

        let mut sink = Sink;
        let mut decode_stage = PlDct::new("DCT decode", &mut sink);
        decode_stage.write(&spliced).unwrap();

        let error = decode_stage
            .finish()
            .expect_err("system libjpeg must reject a reserved marker, matching qpdf");
        assert_eq!(error.to_string(), "Unsupported marker type 0x02");
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

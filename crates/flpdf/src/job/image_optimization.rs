//! qpdf 11.9.0 image-optimization transformation.
//!
//! The implementation follows `QPDFJob::ImageOptimizer` and the surrounding
//! `QPDFJob::handleTransformations` call order (`libqpdf/QPDFJob.cc:36-236,
//! 2137-2174`). Image data is decoded through the canonical specialized stream
//! pipeline, encoded through the qpdf-shaped `Pl_DCT` compression stage, and
//! installed as a deferred stream provider only when the encoded payload is
//! smaller than the original.

use crate::object_handle::ObjectHandle;
use crate::pipeline::count::Count;
use crate::pipeline::dct::PlDct;
use crate::pipeline::{Discard, Pipeline};
use crate::writer::DecodeLevel;
use crate::{PageDocumentHelper, PageObjectHelper, Pdf, QPDFLogger, Result, StreamDataProvider};
use std::io::{Read, Seek};
use std::rc::Rc;

const DEFAULT_OI_MIN_WIDTH: u32 = 128;
const DEFAULT_OI_MIN_HEIGHT: u32 = 128;
const DEFAULT_OI_MIN_AREA: u32 = 16_384;
const DEFAULT_II_MIN_BYTES: usize = 1_024;

/// Options controlling qpdf's `--optimize-images` transformation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageOptimizationOptions {
    /// Minimum image width. Images at or below this width are retained.
    pub min_width: u32,
    /// Minimum image height. Images at or below this height are retained.
    pub min_height: u32,
    /// Minimum image area. Images at or below this area are retained.
    pub min_area: u32,
    /// Minimum encoded inline-image payload for externalization.
    pub inline_min_bytes: usize,
    /// Do not externalize inline images before optimizing XObjects.
    pub keep_inline_images: bool,
}

impl Default for ImageOptimizationOptions {
    fn default() -> Self {
        Self {
            min_width: DEFAULT_OI_MIN_WIDTH,
            min_height: DEFAULT_OI_MIN_HEIGHT,
            min_area: DEFAULT_OI_MIN_AREA,
            inline_min_bytes: DEFAULT_II_MIN_BYTES,
            keep_inline_images: false,
        }
    }
}

/// Apply qpdf's image transformation to every page and recursively reachable
/// Form XObject.
///
/// `logger` and `message_prefix` are the job-owned diagnostic route. The
/// transformation itself mutates only the document's canonical ObjectHandle
/// graph; image bytes remain deferred until the writer consumes the new
/// provider-backed stream, matching qpdf's `replaceStreamData` provider path.
pub fn optimize_images<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    logger: &QPDFLogger,
    message_prefix: &str,
    verbose: bool,
    options: ImageOptimizationOptions,
) -> Result<()> {
    if !options.keep_inline_images {
        let page_refs = PageDocumentHelper::new(pdf).get_all_pages()?;
        for page_ref in page_refs {
            PageObjectHelper::new(page_ref, pdf)
                .externalize_inline_images(options.inline_min_bytes, false)?;
        }
    }

    let page_refs = PageDocumentHelper::new(pdf).get_all_pages()?;
    for (page_index, page_ref) in page_refs.into_iter().enumerate() {
        let page_number = page_index + 1;
        let mut replacements = Vec::new();
        {
            let mut page = PageObjectHelper::new(page_ref, pdf);
            page.for_each_image(true, |image, xobjects, key| {
                let description = format!(
                    "image {} on page {page_number}",
                    String::from_utf8_lossy(&key)
                );
                if !ImageOptimizer::preflight(&image)? {
                    log_skip(
                        logger,
                        message_prefix,
                        verbose,
                        &description,
                        SkipReason::UnableToDecode,
                    )?;
                    return Ok(());
                }
                let optimizer = match ImageOptimizer::prepare(image.clone(), options)? {
                    PrepareResult::Ready(optimizer) => optimizer,
                    PrepareResult::Skip(reason) => {
                        log_skip(logger, message_prefix, verbose, &description, reason)?;
                        return Ok(());
                    }
                };

                let Some(evaluation) = optimizer.evaluate()? else {
                    return Ok(());
                };
                match evaluation {
                    Evaluation::NotSmaller => {
                        log_verbose(
                            logger,
                            message_prefix,
                            verbose,
                            &description,
                            "not optimizing because DCT compression does not reduce image size"
                                .to_owned(),
                        )?;
                    }
                    Evaluation::Smaller {
                        original_length,
                        compressed_length,
                    } => {
                        log_verbose(
                            logger,
                            message_prefix,
                            verbose,
                            &description,
                            format!(
                                "optimizing image reduces size from {original_length} to {compressed_length}"
                            ),
                        )?;

                        let new_image = image.copy_stream()?;
                        new_image.replace_stream_data_provider(
                            Rc::new(optimizer),
                            Some(ObjectHandle::name(b"DCTDecode".to_vec())),
                            Some(ObjectHandle::null()),
                        )?;
                        replacements.push((xobjects, key, new_image));
                    }
                }
                Ok(())
            })?;
        }

        for (xobjects, key, new_image) in replacements {
            xobjects.replace_key(&key, new_image.clone())?;
            pdf.mark_object_handle_dirty(&new_image)?;
            pdf.mark_object_handle_dirty(&xobjects)?;
        }
    }
    Ok(())
}

fn log_skip(
    logger: &QPDFLogger,
    message_prefix: &str,
    verbose: bool,
    description: &str,
    reason: SkipReason,
) -> Result<()> {
    log_verbose(
        logger,
        message_prefix,
        verbose,
        description,
        reason.message().to_owned(),
    )
}

fn log_verbose(
    logger: &QPDFLogger,
    message_prefix: &str,
    verbose: bool,
    description: &str,
    message: String,
) -> Result<()> {
    if verbose {
        logger.info(format!("{message_prefix}: {description}: {message}\n"))?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum SkipReason {
    MissingKeys,
    BitsPerComponent,
    Colorspace,
    TooSmall,
    UnableToDecode,
}

impl SkipReason {
    fn message(self) -> &'static str {
        match self {
            Self::MissingKeys => "not optimizing because image dictionary is missing required keys",
            Self::BitsPerComponent => {
                "not optimizing because image has other than 8 bits per component"
            }
            Self::Colorspace => {
                "not optimizing because qpdf can't optimize images with this colorspace"
            }
            Self::TooSmall => {
                "not optimizing because image is smaller than requested minimum dimensions"
            }
            Self::UnableToDecode => {
                "not optimizing because unable to decode data or data already uses DCT"
            }
        }
    }
}

enum PrepareResult {
    Ready(ImageOptimizer),
    Skip(SkipReason),
}

enum Evaluation {
    NotSmaller,
    Smaller {
        original_length: u64,
        compressed_length: u64,
    },
}

struct ImageOptimizer {
    image: ObjectHandle,
    width: u32,
    height: u32,
    pixel_format: libjpeg_turbo_rs::PixelFormat,
}

impl ImageOptimizer {
    fn preflight(image: &ObjectHandle) -> Result<bool> {
        let mut discard = Discard;
        let mut filtering_attempted = false;
        let succeeded = image.pipe_stream_data(
            &mut discard,
            &mut filtering_attempted,
            0,
            DecodeLevel::Specialized,
            true,
            false,
        )?;
        Ok(succeeded && filtering_attempted)
    }

    fn prepare(image: ObjectHandle, options: ImageOptimizationOptions) -> Result<PrepareResult> {
        let dictionary = image.as_stream_dict().ok_or_else(|| {
            crate::Error::Internal("image XObject has no stream dictionary".into())
        })?;
        let width_value = dictionary.try_get_key(b"/Width")?;
        let height_value = dictionary.try_get_key(b"/Height")?;
        if !width_value.try_is_number()? || !height_value.try_is_number()? {
            return Ok(PrepareResult::Skip(SkipReason::MissingKeys));
        }
        let width = qpdf_dimension(&width_value)?;
        let height = qpdf_dimension(&height_value)?;

        let bits_per_component = dictionary.try_get_key(b"/BitsPerComponent")?;
        if !bits_per_component.try_is_integer()? || bits_per_component.try_get_int_value()? != 8 {
            return Ok(PrepareResult::Skip(SkipReason::BitsPerComponent));
        }

        let colorspace = dictionary.try_get_key(b"/ColorSpace")?;
        colorspace.try_dereference()?;
        let Some(colorspace) = colorspace.as_name() else {
            return Ok(PrepareResult::Skip(SkipReason::Colorspace));
        };
        let pixel_format = match colorspace.as_slice() {
            b"DeviceRGB" => libjpeg_turbo_rs::PixelFormat::Rgb,
            b"DeviceGray" => libjpeg_turbo_rs::PixelFormat::Grayscale,
            b"DeviceCMYK" => libjpeg_turbo_rs::PixelFormat::Cmyk,
            _ => return Ok(PrepareResult::Skip(SkipReason::Colorspace)),
        };

        let area = width.wrapping_mul(height);
        if (options.min_width > 0 && width <= options.min_width)
            || (options.min_height > 0 && height <= options.min_height)
            || (options.min_area > 0 && area <= options.min_area)
        {
            return Ok(PrepareResult::Skip(SkipReason::TooSmall));
        }

        Ok(PrepareResult::Ready(Self {
            image,
            width,
            height,
            pixel_format,
        }))
    }

    fn evaluate(&self) -> Result<Option<Evaluation>> {
        let mut discard = Discard;
        let mut count = Count::new("count", &mut discard);
        let mut encoder = self.encoder(&mut count);
        let mut filtering_attempted = false;
        let succeeded = self.image.pipe_stream_data(
            &mut encoder,
            &mut filtering_attempted,
            0,
            DecodeLevel::Specialized,
            false,
            false,
        )?;
        drop(encoder);
        if !succeeded {
            return Ok(None);
        }

        let dictionary = self.image.as_stream_dict().ok_or_else(|| {
            crate::Error::Internal("image XObject has no stream dictionary".into())
        })?;
        let original_length = dictionary
            .try_get_key(b"/Length")?
            .try_get_int_value()?
            .max(0) as u64;
        let compressed_length = count.count();
        if compressed_length >= original_length {
            Ok(Some(Evaluation::NotSmaller))
        } else {
            Ok(Some(Evaluation::Smaller {
                original_length,
                compressed_length,
            }))
        }
    }

    fn encoder<'a>(&self, next: &'a mut dyn Pipeline) -> PlDct<'a> {
        PlDct::new_compressor(
            "jpg",
            next,
            self.width as usize,
            self.height as usize,
            self.pixel_format,
        )
    }
}

impl StreamDataProvider for ImageOptimizer {
    fn provide_stream_data_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
    ) -> Result<()> {
        let mut encoder = self.encoder(pipeline);
        let mut filtering_attempted = false;
        let _ = self.image.pipe_stream_data(
            &mut encoder,
            &mut filtering_attempted,
            0,
            DecodeLevel::Specialized,
            false,
            false,
        )?;
        Ok(())
    }
}

fn qpdf_dimension(value: &ObjectHandle) -> Result<u32> {
    if value.try_is_integer()? {
        let value = value.try_get_int_value()?;
        return Ok(value.clamp(0, i64::from(u32::MAX)) as u32);
    }
    let value = value.try_get_numeric_value()?;
    if value.is_nan() {
        return Ok(0);
    }
    Ok(value.clamp(0.0, f64::from(u32::MAX)) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::PlString;
    use std::rc::Rc;

    fn image_dictionary(width: ObjectHandle, height: ObjectHandle) -> ObjectHandle {
        ObjectHandle::dictionary(vec![
            (b"/Width".to_vec(), width),
            (b"/Height".to_vec(), height),
            (b"/BitsPerComponent".to_vec(), ObjectHandle::integer(8)),
            (
                b"/ColorSpace".to_vec(),
                ObjectHandle::name(b"DeviceGray".to_vec()),
            ),
            (b"/Length".to_vec(), ObjectHandle::integer(0)),
        ])
    }

    fn direct_image(width: usize, height: usize, data: Vec<u8>) -> ObjectHandle {
        let dictionary = image_dictionary(
            ObjectHandle::integer(width as i64),
            ObjectHandle::integer(height as i64),
        );
        dictionary
            .replace_key(b"/Length", ObjectHandle::integer(data.len() as i64))
            .unwrap();
        ObjectHandle::stream(dictionary, Rc::new(data))
    }

    #[test]
    fn prepare_matches_qpdf_metadata_skip_reasons_and_numeric_dimensions() {
        assert!(matches!(
            ImageOptimizer::prepare(
                ObjectHandle::stream(
                    ObjectHandle::dictionary(vec![(
                        b"/Width".to_vec(),
                        ObjectHandle::integer(200),
                    )]),
                    Rc::new(Vec::new()),
                ),
                ImageOptimizationOptions::default(),
            )
            .unwrap(),
            PrepareResult::Skip(SkipReason::MissingKeys)
        ));

        let bad_bits = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"/Width".to_vec(), ObjectHandle::integer(200)),
                (b"/Height".to_vec(), ObjectHandle::integer(200)),
                (b"/BitsPerComponent".to_vec(), ObjectHandle::integer(1)),
            ]),
            Rc::new(Vec::new()),
        );
        assert!(matches!(
            ImageOptimizer::prepare(bad_bits, ImageOptimizationOptions::default()).unwrap(),
            PrepareResult::Skip(SkipReason::BitsPerComponent)
        ));

        let bad_colorspace = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"/Width".to_vec(), ObjectHandle::integer(200)),
                (b"/Height".to_vec(), ObjectHandle::integer(200)),
                (b"/BitsPerComponent".to_vec(), ObjectHandle::integer(8)),
                (
                    b"/ColorSpace".to_vec(),
                    ObjectHandle::name(b"Pattern".to_vec()),
                ),
            ]),
            Rc::new(Vec::new()),
        );
        assert!(matches!(
            ImageOptimizer::prepare(bad_colorspace, ImageOptimizationOptions::default()).unwrap(),
            PrepareResult::Skip(SkipReason::Colorspace)
        ));

        let mut small_options = ImageOptimizationOptions::default();
        small_options.min_width = 200;
        let small = ObjectHandle::stream(
            image_dictionary(ObjectHandle::integer(200), ObjectHandle::integer(200)),
            Rc::new(Vec::new()),
        );
        assert!(matches!(
            ImageOptimizer::prepare(small, small_options).unwrap(),
            PrepareResult::Skip(SkipReason::TooSmall)
        ));

        assert_eq!(qpdf_dimension(&ObjectHandle::real(200.9)).unwrap(), 200);
        assert_eq!(qpdf_dimension(&ObjectHandle::real(f64::NAN)).unwrap(), 0);
        assert_eq!(qpdf_dimension(&ObjectHandle::integer(-1)).unwrap(), 0);
        assert_eq!(
            qpdf_dimension(&ObjectHandle::integer(i64::MAX)).unwrap(),
            u32::MAX
        );
    }

    #[test]
    fn prepare_rejects_a_non_stream_image_handle() {
        let result = ImageOptimizer::prepare(
            ObjectHandle::integer(1),
            ImageOptimizationOptions::default(),
        );
        assert!(
            matches!(result, Err(crate::Error::Internal(message)) if message == "image XObject has no stream dictionary")
        );
    }

    #[test]
    fn optimizer_preflight_evaluate_and_provider_share_the_same_source() {
        let image = direct_image(200, 200, vec![128; 40_000]);
        assert!(ImageOptimizer::preflight(&image).unwrap());
        let optimizer = match ImageOptimizer::prepare(
            image,
            ImageOptimizationOptions {
                min_width: 0,
                min_height: 0,
                min_area: 0,
                ..ImageOptimizationOptions::default()
            },
        )
        .unwrap()
        {
            PrepareResult::Ready(optimizer) => optimizer,
            PrepareResult::Skip(_) => panic!("large grayscale image should be eligible"),
        };
        assert!(matches!(
            optimizer.evaluate().unwrap(),
            Some(Evaluation::Smaller { .. })
        ));

        let mut output = Vec::new();
        let mut sink = PlString::new("sink", None, &mut output);
        optimizer
            .provide_stream_data_by_id(0, 0, &mut sink)
            .expect("provider emits the same deterministic JPEG");
        assert!(output.starts_with(&[0xff, 0xd8]));
    }

    #[test]
    fn optimizer_keeps_a_source_when_jpeg_does_not_shrink_it() {
        let image = direct_image(1, 1, vec![128]);
        let optimizer = match ImageOptimizer::prepare(
            image,
            ImageOptimizationOptions {
                min_width: 0,
                min_height: 0,
                min_area: 0,
                ..ImageOptimizationOptions::default()
            },
        )
        .unwrap()
        {
            PrepareResult::Ready(optimizer) => optimizer,
            PrepareResult::Skip(_) => panic!("minimum thresholds are disabled"),
        };
        assert!(matches!(
            optimizer.evaluate().unwrap(),
            Some(Evaluation::NotSmaller)
        ));
    }
}

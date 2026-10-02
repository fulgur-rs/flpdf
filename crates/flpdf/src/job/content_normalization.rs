//! Normalize page content streams for qpdf's linearized writer path.
//!
//! qpdf correspondence: `QPDFJob` stores `normalizeContent` separately and
//! reapplies it while setting writer options (`QPDFJob.cc:2847-2863`). The
//! pre-write mutation keeps flpdf's linearization planner aligned with that
//! policy for cleanly decodable streams. Unsupported filters and streams whose
//! decoders warn or fail stay untouched so the writer can apply qpdf's raw
//! retry behavior.

use crate::content_normalizer::normalize_content_stream;
use crate::writer::DecodeLevel as StreamDecodeLevel;
use crate::{Error, ObjectHandle, ObjectRef, PageDocumentHelper, Pdf, QpdfErrorCode, Result};
use std::collections::HashSet;
use std::io::{Read, Seek};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ContentNormalizationWarning {
    pub(super) parsed_offset: Option<u64>,
    pub(super) last_token_was_bad: bool,
}

/// Normalize all page content streams in an in-memory PDF graph.
///
/// This runs after page selection and transformations, immediately before a
/// linearized writer starts planning output. Page order, indirect `/Contents`
/// handling, alias de-duplication, and warning order match the former CLI
/// stage.
pub(super) fn normalize_page_contents<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> Result<Vec<ContentNormalizationWarning>> {
    let mut warnings = Vec::new();
    let mut seen = HashSet::new();
    let pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    for page in pages {
        warnings.extend(apply_normalize_content(pdf, page, &mut seen)?);
    }
    Ok(warnings)
}

/// Normalize the content stream(s) for a single page.
fn apply_normalize_content<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page: ObjectHandle,
    seen: &mut HashSet<ObjectRef>,
) -> Result<Vec<ContentNormalizationWarning>> {
    let mut warnings = Vec::new();
    let contents = page.try_get_key(b"/Contents")?;
    let contents_ref = contents.object_ref();

    let mut streams = Vec::new();
    if contents.try_is_stream_of_type(b"", b"")? {
        if let Some(stream_ref) = contents_ref {
            streams.push((stream_ref, contents));
        }
    } else if contents.try_is_array()? {
        let items = contents.try_get_array_as_vector()?;
        for item in items {
            let item_ref = item.object_ref();
            if item.try_is_stream_of_type(b"", b"")? {
                if let Some(item_ref) = item_ref {
                    streams.push((item_ref, item));
                }
            } // cov:ignore: LLVM maps the covered array-member stream branch to its condition line
        }
    } // cov:ignore: LLVM maps the covered Contents-array branch to its array guard

    for (stream_ref, stream) in streams {
        if let Some(last_bad) = normalize_and_store_stream_handle(pdf, stream_ref, stream, seen)? {
            warnings.push(last_bad);
        }
    }
    Ok(warnings)
}

/// Normalize a stream through its live ObjectHandle pipeline and mutate the
/// same stream so the writer observes the canonical decoded bytes and length.
fn normalize_and_store_stream_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    stream_ref: ObjectRef,
    stream: ObjectHandle,
    seen: &mut HashSet<ObjectRef>,
) -> Result<Option<ContentNormalizationWarning>> {
    if !seen.insert(stream_ref) {
        return Ok(None);
    }

    // qpdf's writer checks filterability before applying content normalization
    // (`QPDF_Stream.cc:379-435,488-512; QPDFWriter.cc:1272-1305`). If any
    // filter is unsupported, it preserves the raw stream and its filter dictionary.
    // Probe with warning delivery suppressed here; the writer will perform
    // the observable probe later and emit any malformed-filter warning once.
    let suppress_warnings = pdf.suppress_warnings();
    pdf.set_suppress_warnings(true);
    let filterable = match stream.stream_data_filterable(StreamDecodeLevel::All) {
        Ok(filterable) => filterable,
        Err(error) => {
            pdf.set_suppress_warnings(suppress_warnings);
            return Err(error);
        }
    };
    if !filterable {
        pdf.set_suppress_warnings(suppress_warnings);
        return Ok(None);
    }

    // Decode errors and decoder warnings must be left to the writer's retry
    // loop. qpdf pipes the original stream there and may retry without
    // filtering; doing that work here would either abort early or duplicate
    // its pass-specific warnings (`QPDFWriter.cc:1272-1305`).
    let warnings_before_decode = pdf.num_warnings();
    let decoded = stream.get_stream_data(StreamDecodeLevel::All);
    let decode_warned = pdf.num_warnings() != warnings_before_decode;
    pdf.set_suppress_warnings(suppress_warnings);
    let decoded = match decoded {
        Ok(_) if decode_warned => return Ok(None),
        Ok(decoded) => decoded,
        Err(Error::Unsupported(_)) => return Ok(None),
        Err(Error::QpdfExc(error))
            if error.get_error_code() == QpdfErrorCode::Unsupported
                && error.get_message_detail() == b"getStreamData called on unfilterable stream" =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let normalized = normalize_content_stream(decoded.as_ref());
    let warning = normalized
        .any_bad_tokens()
        .then(|| ContentNormalizationWarning {
            parsed_offset: u64::try_from(stream.get_parsed_offset()).ok(),
            last_token_was_bad: normalized.last_token_was_bad(),
        });
    let normalized = normalized.into_bytes();
    let length =
        i64::try_from(normalized.len()).map_err(|error| crate::Error::System(error.to_string()))?;

    // The normalized payload is raw. Install a direct /Length and remove
    // filter/encoding keys through the stream's canonical live handle.
    let normalized = std::rc::Rc::new(normalized);
    stream.replace_stream_data(
        std::rc::Rc::clone(&normalized),
        Some(ObjectHandle::null()),
        Some(ObjectHandle::null()),
    );
    if let Some(dict) = stream.as_stream_dict() {
        dict.replace_key(b"/Length", ObjectHandle::integer(length))?;
    } // cov:ignore: LLVM maps the covered stream-dictionary replacement to the if-let line
    stream.mark_content_normalization_applied();
    Ok(warning)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    enum DecodeFailure {
        Unsupported,
        System,
    }

    struct DecodeFailureFilter(DecodeFailure);

    impl crate::StreamFilter for DecodeFailureFilter {
        fn get_decode_pipeline<'a>(
            &mut self,
            _next: crate::pipeline::PipelineRef<'a>,
        ) -> Result<crate::OwnedDecodePipeline<'a>> {
            match self.0 {
                DecodeFailure::Unsupported => Err(Error::Unsupported(
                    "normalization decode unsupported".to_owned(),
                )),
                DecodeFailure::System => {
                    Err(Error::System("normalization decode failed".to_owned()))
                }
            }
        }
    }

    fn filtered_stream(filter: &[u8]) -> ObjectHandle {
        ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"Filter".to_vec(),
                ObjectHandle::name(filter.to_vec()),
            )]),
            std::rc::Rc::new(b"encoded".to_vec()),
        )
    }

    fn page_pdf_with_contents_array(streams: &[&[u8]], members: &[usize]) -> Vec<u8> {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let size = 4 + streams.len();
        let mut offsets = vec![0usize; size];
        offsets[1] = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        offsets[2] = bytes.len();
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let contents = members
            .iter()
            .map(|index| format!("{} 0 R", 4 + index))
            .collect::<Vec<_>>()
            .join(" ");
        offsets[3] = bytes.len();
        bytes.extend_from_slice(
            format!(
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] \
                 /Resources << >> /Contents [{contents}] >>\nendobj\n"
            )
            .as_bytes(),
        );
        for (index, content) in streams.iter().enumerate() {
            let object = 4 + index;
            offsets[object] = bytes.len();
            bytes.extend_from_slice(
                format!("{object} 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes(),
            );
            bytes.extend_from_slice(content);
            bytes.extend_from_slice(b"\nendstream\nendobj\n");
        }
        let xref_start = bytes.len();
        bytes.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for offset in offsets.iter().skip(1) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        bytes
    }

    #[test]
    fn linearized_normalization_walks_contents_arrays_and_updates_each_length() {
        let bytes = page_pdf_with_contents_array(&[b"q\r\nQ", b"q\rQ"], &[0, 1]);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("synthetic page PDF opens");
        let pages = PageDocumentHelper::new(&mut pdf)
            .get_all_pages()
            .expect("page tree resolves");

        assert!(normalize_page_contents(&mut pdf)
            .expect("array content streams normalize")
            .is_empty());

        let contents = pages[0]
            .try_get_key(b"/Contents")
            .expect("page Contents array");
        let items = contents
            .try_get_array_as_vector()
            .expect("array members resolve");
        assert_eq!(items.len(), 2);
        for item in items {
            assert_eq!(
                item.get_stream_data(StreamDecodeLevel::All)
                    .expect("normalized stream bytes")
                    .as_slice(),
                b"q\nQ"
            );
            let dictionary = item.as_stream_dict().expect("stream dictionary");
            assert_eq!(
                dictionary
                    .try_get_key(b"/Length")
                    .expect("stream length")
                    .try_as_integer()
                    .expect("integer length"),
                Some(3)
            );
        }
    }

    #[test]
    fn linearized_normalization_deduplicates_repeated_array_streams() {
        let bytes = page_pdf_with_contents_array(&[b"\r<0g"], &[0, 0]);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("synthetic page PDF opens");
        let warnings = normalize_page_contents(&mut pdf).expect("array streams normalize");
        assert_eq!(warnings.len(), 1, "one aliased stream warns only once");
    }

    #[test]
    fn unsupported_registered_filter_is_left_for_the_writer_retry_path() {
        crate::register_stream_filter(b"/FlpdfContentNormalizationUnsupported", || {
            Ok(DecodeFailureFilter(DecodeFailure::Unsupported))
        });
        let mut pdf = Pdf::empty().expect("empty PDF opens");
        pdf.set_suppress_warnings(false);
        let stream = filtered_stream(b"FlpdfContentNormalizationUnsupported");

        let result = normalize_and_store_stream_handle(
            &mut pdf,
            ObjectRef::new(90, 0),
            stream.clone(),
            &mut HashSet::new(),
        )
        .expect("unsupported decoding belongs to the writer retry path");

        assert!(result.is_none());
        assert!(!pdf.suppress_warnings(), "warning policy must be restored");
        assert_eq!(
            stream
                .get_raw_stream_data()
                .expect("raw stream remains readable")
                .as_slice(),
            b"encoded"
        );
        assert!(stream
            .as_stream_dict()
            .unwrap()
            .try_get_key(b"/Filter")
            .unwrap()
            .try_is_name_and_equals(b"FlpdfContentNormalizationUnsupported")
            .unwrap());
    }

    #[test]
    fn decode_system_error_propagates_after_restoring_warning_policy() {
        crate::register_stream_filter(b"/FlpdfContentNormalizationSystemDecode", || {
            Ok(DecodeFailureFilter(DecodeFailure::System))
        });
        let mut pdf = Pdf::empty().expect("empty PDF opens");
        pdf.set_suppress_warnings(false);
        let error = normalize_and_store_stream_handle(
            &mut pdf,
            ObjectRef::new(91, 0),
            filtered_stream(b"FlpdfContentNormalizationSystemDecode"),
            &mut HashSet::new(),
        )
        .expect_err("non-retryable decode errors must reach the Job boundary");

        assert!(matches!(
            error,
            Error::System(message) if message == "normalization decode failed"
        ));
        assert!(!pdf.suppress_warnings(), "warning policy must be restored");
    }

    #[test]
    fn filterability_error_propagates_after_restoring_warning_policy() {
        crate::register_stream_filter(
            b"/FlpdfContentNormalizationFactoryError",
            || -> Result<DecodeFailureFilter> {
                Err(Error::System(
                    "normalization filter factory failed".to_owned(),
                ))
            },
        );
        let mut pdf = Pdf::empty().expect("empty PDF opens");
        pdf.set_suppress_warnings(false);
        let error = normalize_and_store_stream_handle(
            &mut pdf,
            ObjectRef::new(92, 0),
            filtered_stream(b"FlpdfContentNormalizationFactoryError"),
            &mut HashSet::new(),
        )
        .expect_err("filterability errors must reach the Job boundary");

        assert!(matches!(
            error,
            Error::System(message) if message == "normalization filter factory failed"
        ));
        assert!(!pdf.suppress_warnings(), "warning policy must be restored");
    }
}

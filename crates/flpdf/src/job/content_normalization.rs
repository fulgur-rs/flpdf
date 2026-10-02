//! Normalize page content streams for qpdf's linearized writer path.
//!
//! qpdf correspondence: `QPDFJob` stores `normalizeContent` separately and
//! reapplies it while setting writer options (`QPDFJob.cc:2847-2863`). The
//! pre-write stream mutation keeps flpdf's linearization planner aligned with
//! that selected normalization policy.

use crate::content_normalizer::normalize_content_stream;
use crate::writer::DecodeLevel as StreamDecodeLevel;
use crate::{ObjectHandle, ObjectRef, PageDocumentHelper, Pdf, Result};
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
        warnings.extend(apply_normalize_content(page, &mut seen)?);
    }
    Ok(warnings)
}

/// Normalize the content stream(s) for a single page.
fn apply_normalize_content(
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
            }
        }
    }

    for (stream_ref, stream) in streams {
        if let Some(last_bad) = normalize_and_store_stream_handle(stream_ref, stream, seen)? {
            warnings.push(last_bad);
        }
    }
    Ok(warnings)
}

/// Normalize a stream through its live ObjectHandle pipeline and mutate the
/// same stream so the writer observes the canonical decoded bytes and length.
fn normalize_and_store_stream_handle(
    stream_ref: ObjectRef,
    stream: ObjectHandle,
    seen: &mut HashSet<ObjectRef>,
) -> Result<Option<ContentNormalizationWarning>> {
    if !seen.insert(stream_ref) {
        return Ok(None);
    }

    let decoded = stream.get_stream_data(StreamDecodeLevel::All)?;
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
    }
    stream.mark_content_normalization_applied();
    Ok(warning)
}

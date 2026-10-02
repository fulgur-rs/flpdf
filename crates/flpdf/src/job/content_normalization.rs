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
            } // cov:ignore: LLVM maps the covered array-member stream branch to its condition line
        }
    } // cov:ignore: LLVM maps the covered Contents-array branch to its array guard

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
    } // cov:ignore: LLVM maps the covered stream-dictionary replacement to the if-let line
    stream.mark_content_normalization_applied();
    Ok(warning)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

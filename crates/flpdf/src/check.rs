//! qpdf correspondence: QPDFJob.cc document-check surface over reader diagnostics.
//! Lightweight document validator.
//!
//! Mirrors the surface of qpdf's `--check`: open the document (with the recovery path
//! enabled), report parser warnings, and flag a few high-level invariants that would
//! cause downstream tools to fail.

use crate::filters::{
    decode_stream_data_with_limits_and_warnings, is_decode_output_limit_error, DecodeLimits,
};
use crate::stream_filter::normalize_filter_name;
use crate::{Diagnostic, Diagnostics, Dictionary, Error, Object, Pdf, PdfOpenOptions};
use std::io::{Read, Seek};

/// Result of [`check_reader`].
///
/// `valid` is `true` when no [`Diagnostic`] of severity `Error` was produced. Warnings
/// alone (for example parser or recovery warnings) do not flip the flag.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CheckReport {
    pub valid: bool,
    pub diagnostics: Diagnostics,
    /// Document summary, or `None` when the open path failed before a document
    /// object existed (e.g. an unrecoverable parse error).
    pub summary: Option<CheckSummary>,
}

/// Document-level summary captured by [`check_reader`] when the input opened.
///
/// Backs a `qpdf --check`-style banner (header version, encryption and
/// linearization status) without re-opening the document.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CheckSummary {
    /// PDF version from the file header, e.g. `"1.7"` (no `%PDF-` prefix).
    pub version: String,
    /// Whether the document authenticated an `/Encrypt` dictionary on open.
    pub encrypted: bool,
    /// Whether the document carries a linearization hint object.
    pub linearized: bool,
    /// Adobe extension level from the catalog's `/Extensions /ADBE
    /// /ExtensionLevel`, when present. qpdf appends this to the version banner
    /// (e.g. `PDF Version: 1.7 extension level 8`).
    pub extension_level: Option<i64>,
}

/// Validate the document behind `reader` using the repair-enabled open path.
///
/// Input-derived structural and open failures are downgraded to error diagnostics.
/// [`Error::Encrypted`], [`Error::System`], and [`Error::Internal`] propagate instead.
/// Equivalent to `qpdf --check` (which also runs the recovery heuristics).
///
/// # Errors
///
/// - [`Error::Encrypted`] when the document is encrypted and cannot be opened; unlike
///   other input-facing open failures, this is propagated rather than downgraded to a
///   diagnostic. If repair warnings were collected first, the returned value is
///   [`Error::OpenFailure`]; inspect [`Error::open_failure`] to recover the
///   encrypted terminal source and its diagnostics.
/// - [`Error::System`] or [`Error::Internal`] when runtime or logic failures occur while
///   delivering warnings through the configured logger; these are propagated unchanged.
/// - A failed linearization probe via [`Pdf::is_linearized`] is recorded as a
///   warning [`Diagnostic`] and the document is treated as non-linearized; the
///   error is not propagated.
///
/// Other failures from the repair-enabled open path are turned into an error
/// [`Diagnostic`] and returned inside an `Ok(CheckReport)`.
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::check_reader;
///
/// let report = check_reader(BufReader::new(File::open("input.pdf")?))?;
/// println!("valid: {}", report.valid);
/// for diagnostic in report.diagnostics.entries() {
///     println!("{diagnostic:?}");
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn check_reader<R: Read + Seek + 'static>(reader: R) -> crate::Result<CheckReport> {
    check_reader_inner(reader, true)
}

/// Validate the document with explicit open options.
///
/// # Errors
///
/// - When `options.repair` is set, behaves like [`check_reader`]: encrypted,
///   system, and internal terminal sources are propagated from the open path,
///   possibly inside [`Error::OpenFailure`] when repair diagnostics were
///   collected; other input-facing open failures become an error [`Diagnostic`]
///   inside an `Ok(CheckReport)`.
/// - When `options.repair` is clear, any error from [`Pdf::open_with_options`] is
///   propagated unchanged (e.g. [`Error::Io`], [`Error::Parse`], [`Error::Encrypted`]).
/// - A failed linearization probe via [`Pdf::is_linearized`] is recorded as a
///   warning [`Diagnostic`] and the document is treated as non-linearized; the
///   error is not propagated.
pub fn check_reader_with_options<R: Read + Seek + 'static>(
    reader: R,
    options: PdfOpenOptions,
) -> crate::Result<CheckReport> {
    check_reader_inner_with_options(reader, options, DecodeLimits::default())
}

/// Validate the document with explicit open options and an opt-in decode-output
/// limit.
///
/// Behaves like [`check_reader_with_options`], but bounds each page content
/// stream's `FlateDecode`, `LZWDecode`, `ASCII85Decode`, `ASCIIHexDecode`, or
/// `RunLengthDecode` output to [`DecodeLimits::max_output`]. A stream whose
/// decoded output would exceed that cap is reported as a warning (a
/// decompression-bomb guard trip), not a stream-encoding error: the stream is
/// intact, merely larger than the caller allowed. With
/// [`DecodeLimits::default`] (no cap) this is identical to
/// [`check_reader_with_options`].
///
/// # Limitations
///
/// A stream capped in any of the codecs above is not fully decoded, so errors
/// that only a complete decode would surface — corruption past the cap — are
/// not reported for that stream while a cap is in effect; with
/// [`DecodeLimits::default`] they still surface as errors. Invalid
/// `/DecodeParms` are unaffected, because the filter validates its parameters
/// and builds its pipeline before decoding any bytes. Content streams carrying an explicit `/Crypt`
/// filter are decoded during object resolution and are not bounded by this cap.
///
/// # Errors
///
/// Same as [`check_reader_with_options`], including propagation of
/// [`Error::Encrypted`], [`Error::System`], and [`Error::Internal`] from the repair-enabled
/// open path.
pub fn check_reader_with_options_and_limits<R: Read + Seek + 'static>(
    reader: R,
    options: PdfOpenOptions,
    limits: DecodeLimits,
) -> crate::Result<CheckReport> {
    check_reader_inner_with_options(reader, options, limits)
}

/// Validate the document behind `reader` without running the recovery heuristics.
///
/// A failed strict parse is propagated as a hard [`crate::Error`] rather than turned
/// into a diagnostic; the caller is expected to handle the I/O error explicitly.
/// Equivalent to `qpdf --check` without `--password=...`-style recovery toggles.
///
/// # Errors
///
/// - Propagates any error from [`Pdf::open_with_options`] unchanged, since the
///   recovery heuristics are disabled (e.g. [`Error::Io`], [`Error::Parse`],
///   [`Error::Encrypted`]).
/// - A failed linearization probe via [`Pdf::is_linearized`] is recorded as a
///   warning [`Diagnostic`] and the document is treated as non-linearized; the
///   error is not propagated.
pub fn check_reader_strict<R: Read + Seek + 'static>(reader: R) -> crate::Result<CheckReport> {
    check_reader_inner(reader, false)
}

fn check_reader_inner<R: Read + Seek + 'static>(
    reader: R,
    allow_repair: bool,
) -> crate::Result<CheckReport> {
    check_reader_inner_with_options(
        reader,
        PdfOpenOptions {
            repair: allow_repair,
            ..PdfOpenOptions::default()
        },
        DecodeLimits::default(),
    )
}

fn check_reader_inner_with_options<R: Read + Seek + 'static>(
    reader: R,
    options: PdfOpenOptions,
    limits: DecodeLimits,
) -> crate::Result<CheckReport> {
    let allow_repair = options.repair;
    let mut pdf = if allow_repair {
        match Pdf::open_with_options(reader, options) {
            Ok(pdf) => pdf,
            Err(error)
                if matches!(
                    open_failure_source(&error),
                    Error::Encrypted(_) | Error::System(_) | Error::Internal(_)
                ) =>
            {
                return Err(error);
            }
            Err(error) => {
                let mut diagnostics = error
                    .open_failure()
                    .map(|(_, diagnostics)| diagnostics.clone())
                    .unwrap_or_default();
                diagnostics.push(Diagnostic::error(error.to_string(), None));
                return Ok(CheckReport {
                    valid: false,
                    diagnostics,
                    summary: None,
                });
            }
        }
    } else {
        Pdf::open_with_options(reader, options)?
    };

    Ok(check_pdf_with_limits(&mut pdf, limits))
}

/// Validate an already-opened document using the same checks as [`check_reader`].
///
/// This is the document-consumer half of qpdf's `--check` path. It is used by
/// job inputs that do not originate from a PDF byte stream, such as
/// `--json-input` and `--update-from-json`, after their canonical document
/// boundary has been created and updated.
pub fn check_pdf_with_limits<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    limits: DecodeLimits,
) -> CheckReport {
    // `repair_diagnostics` already hands back an owned snapshot, so this is
    // the copy — no second `.clone()` needed.
    let mut diagnostics = pdf.repair_diagnostics();
    let repair_diagnostics_start = diagnostics.entries().len();
    if pdf.uses_weak_crypto() {
        diagnostics.push(Diagnostic::warning(
            "encrypted PDF uses weak crypto; processing continued",
            None,
        ));
    }
    if pdf.root_ref().is_none() {
        diagnostics.push(Diagnostic::error("trailer is missing /Root", None));
    }
    // The document already opened, so a failed linearization probe must not
    // sink the whole check. Downgrade the error to a warning and treat the file
    // as non-linearized rather than propagating a hard error.
    let linearized = match pdf.is_linearized() {
        Ok(value) => value,
        Err(error) => {
            diagnostics.push(Diagnostic::warning(
                format!("failed to inspect linearization hint: {error}"),
                None,
            ));
            false
        }
    };

    // Decode every page's content stream(s); a genuine decode failure is a
    // stream-encoding error. qpdf --check does the same and exits 2 on a broken
    // content stream. The whole-document page walk here is deliberate: --check is
    // a full-document audit, the one place flpdf's lazy-load discipline is
    // intentionally relaxed.
    check_content_streams(pdf, &mut diagnostics, limits);
    for diagnostic in pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .skip(repair_diagnostics_start)
        .cloned()
    {
        diagnostics.push(diagnostic);
    }

    let summary = CheckSummary {
        version: pdf.version().to_string(),
        encrypted: pdf.is_encrypted(),
        linearized,
        extension_level: pdf.adobe_extension_level(),
    };

    CheckReport {
        valid: !diagnostics.has_errors(),
        diagnostics,
        summary: Some(summary),
    }
}

fn open_failure_source(error: &Error) -> &Error {
    error
        .open_failure()
        .map_or(error, |(source, _diagnostics)| source)
}

/// The generalized filters flpdf fully decodes. A decode failure on a stream
/// whose `/Filter` chain is entirely generalized is a genuine encoding error;
/// any other codec (image passthrough, `Crypt`, unknown) means flpdf cannot
/// judge corruption, so the failure must be ignored rather than reported.
const GENERALIZED_FILTERS: [&[u8]; 5] = [
    b"FlateDecode",
    b"LZWDecode",
    b"ASCII85Decode",
    b"ASCIIHexDecode",
    b"RunLengthDecode",
];

/// Return `true` when `dict`'s `/Filter` is absent (no-op decode) or names only
/// generalized codecs flpdf decodes. A `/Filter` stored as an indirect
/// reference, a non-name entry, or any non-generalized codec yields `false`
/// (the stream is not classified as decodable, so a later decode failure is not
/// reported as a stream-encoding error).
fn content_filter_chain_is_generalized(dict: &Dictionary) -> bool {
    fn is_generalized(name: &[u8]) -> bool {
        GENERALIZED_FILTERS.contains(&normalize_filter_name(name))
    }
    // An indirect `/Filter` on a content stream is essentially never seen;
    // treating it conservatively as "skip" trades a vanishing parity gap for
    // zero false positives — flpdf must never report a valid image-bearing
    // stream as corrupt.
    match dict.get("Filter") {
        None => true,
        Some(Object::Name(name)) => is_generalized(name),
        Some(Object::Array(elems)) => elems
            .iter()
            .all(|e| matches!(e, Object::Name(n) if is_generalized(n))),
        Some(_) => false,
    }
}

/// Decode each page's content stream(s) and push an error `Diagnostic` for any
/// genuine decode failure. Streams whose `/Filter` chain is not fully
/// generalized are skipped — flpdf cannot decode them and so cannot tell
/// corruption from an unsupported codec. Structural problems that block
/// enumeration downgrade to a warning so the already-opened document still
/// yields a report.
fn check_content_streams<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    diagnostics: &mut Diagnostics,
    limits: DecodeLimits,
) {
    let page_refs = match crate::pages::page_refs(pdf) {
        Ok(refs) => refs,
        Err(error) => {
            diagnostics.push(Diagnostic::warning(
                format!("could not enumerate pages for content-stream check: {error}"),
                None,
            ));
            return;
        }
    };
    // --check is a deliberate full-document audit (like qpdf): every page's
    // content stream is decoded, so this whole-document walk intentionally
    // relaxes flpdf's usual lazy-load / bounded-traversal discipline.
    for (index, page_ref) in page_refs.iter().enumerate() {
        let page_number = index + 1; // 1-based, matching qpdf's "page N"
        let entries = match crate::pages::page_content_stream_entries(pdf, *page_ref) {
            Ok(entries) => entries,
            Err(error) => {
                diagnostics.push(Diagnostic::warning(
                    format!("page {page_number}: could not read content streams: {error}"),
                    None,
                ));
                continue;
            }
        };
        for (stream_ref, stream) in entries {
            if !content_filter_chain_is_generalized(&stream.dict) {
                continue;
            }
            // qpdf renders the location as "content stream object N G" (no
            // trailing " R"); format the number/generation pair directly.
            let location = match stream_ref {
                Some(r) => format!("content stream object {} {}", r.number, r.generation),
                None => "inline content stream".to_string(),
            };
            let mut decode_warnings = Vec::new();
            let result = decode_stream_data_with_limits_and_warnings(
                &stream.dict,
                &stream.data,
                limits,
                &mut |message, _code| {
                    decode_warnings.push(message.to_string());
                    Ok(())
                },
            );
            for warning in decode_warnings {
                diagnostics.push(Diagnostic::warning(
                    format!("page {page_number}: {location}: {warning}"),
                    None,
                ));
            }
            // A decode `Err` is one of two things:
            //   * the opt-in output cap tripped — the stream is intact, just
            //     larger than the configured limit, so this is a deliberate
            //     decode-bomb guard, reported as a WARNING (qpdf's posture:
            //     exceeding flate_max_memory is a warning, not an error);
            //   * any other failure means the stream cannot be decoded as
            //     declared (corrupt payload, `/Filter` chain past the cap, bad
            //     `/DecodeParms`) — a genuine stream-encoding ERROR.
            if let Err(error) = result {
                if is_decode_output_limit_error(&error) {
                    // The guard only trips when a cap is set, so `max_output` is
                    // always `Some` here; `unwrap_or_default` echoes the cap into
                    // the diagnostic without an unreachable branch or a panic.
                    let limit = limits.max_output.unwrap_or_default();
                    diagnostics.push(Diagnostic::warning(
                        format!(
                            "page {page_number}: {location}: decoded output exceeds the configured limit of {limit} bytes; skipped (decode-bomb guard)"
                        ),
                        None,
                    ));
                } else {
                    diagnostics.push(Diagnostic::error(
                        format!(
                            "page {page_number}: {location}: errors while decoding content stream"
                        ),
                        None,
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::encode_stream_data;
    use crate::{ObjectRef, Severity, Stream};
    use std::io::Cursor;

    #[test]
    fn filter_chain_classification() {
        // FlateDecode alone → generalized.
        let mut flate = Dictionary::new();
        flate.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        assert!(content_filter_chain_is_generalized(&flate));

        // Every remaining generalized codec is classified as decodable, so a
        // typo in any GENERALIZED_FILTERS entry flips one of these assertions.
        for codec in [
            &b"LZWDecode"[..],
            &b"ASCIIHexDecode"[..],
            &b"RunLengthDecode"[..],
        ] {
            let mut dict = Dictionary::new();
            dict.insert("Filter", Object::Name(codec.to_vec()));
            assert!(content_filter_chain_is_generalized(&dict));
        }

        // No /Filter → trivially decodable (no-op decode).
        let none = Dictionary::new();
        assert!(content_filter_chain_is_generalized(&none));

        // DCTDecode (image codec) → not generalized.
        let mut dct = Dictionary::new();
        dct.insert("Filter", Object::Name(b"DCTDecode".to_vec()));
        assert!(!content_filter_chain_is_generalized(&dct));

        // Mixed array with a non-generalized member → not generalized.
        let mut mixed = Dictionary::new();
        mixed.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"ASCII85Decode".to_vec()),
                Object::Name(b"DCTDecode".to_vec()),
            ]),
        );
        assert!(!content_filter_chain_is_generalized(&mixed));

        // Array of only generalized codecs → generalized.
        let mut all_generalized = Dictionary::new();
        all_generalized.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"ASCII85Decode".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ]),
        );
        assert!(content_filter_chain_is_generalized(&all_generalized));

        // Indirect /Filter → cannot judge → skip (not generalized).
        let mut indirect = Dictionary::new();
        indirect.insert("Filter", Object::Reference(ObjectRef::new(9, 0)));
        assert!(!content_filter_chain_is_generalized(&indirect));

        // Non-name, non-array /Filter (e.g. an integer) → not generalized.
        let mut weird = Dictionary::new();
        weird.insert("Filter", Object::Integer(42));
        assert!(!content_filter_chain_is_generalized(&weird));
    }

    /// Minimal valid single-page PDF (`%PDF-1.4`), not encrypted, not linearized.
    fn minimal_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n"
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn summary_present_for_clean_document() {
        let report = check_reader_strict(Cursor::new(minimal_pdf_bytes())).unwrap();
        assert!(report.valid);
        let summary = report.summary.expect("summary present when document opens");
        assert_eq!(summary.version, "1.4");
        assert!(!summary.encrypted);
        assert!(!summary.linearized);
        assert_eq!(summary.extension_level, None);
    }

    /// `%PDF-1.7` document whose catalog reaches an Adobe extension level via an
    /// *indirect* `/Extensions` reference (object 4), with an inline `/ADBE`
    /// dictionary and an inline integer `/ExtensionLevel`.
    fn extension_level_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.7\n");
        let off1 = pdf.len();
        pdf.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions 4 0 R >>\nendobj\n",
        );
        let off2 = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let off4 = pdf.len();
        pdf.extend_from_slice(
            b"4 0 obj\n<< /ADBE << /BaseVersion /1.7 /ExtensionLevel 8 >> >>\nendobj\n",
        );
        let xref_start = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn summary_reports_adobe_extension_level() {
        let report = check_reader_strict(Cursor::new(extension_level_pdf_bytes())).unwrap();
        let summary = report.summary.expect("summary present");
        assert_eq!(summary.version, "1.7");
        assert_eq!(summary.extension_level, Some(8));
    }

    #[test]
    fn summary_none_when_open_fails() {
        // Header present but no recoverable structure: the repair-enabled open
        // path fails and is downgraded to an error diagnostic, so no document
        // object is available to summarise.
        let report = check_reader(Cursor::new(
            b"%PDF-1.4\nthis is not a valid pdf at all\n%%EOF\n".to_vec(),
        ))
        .unwrap();
        assert!(!report.valid);
        assert!(report.summary.is_none());
    }

    /// Reader that remains usable while the document is opened, then fails
    /// after the exact 1024-byte prefix read owned by `Pdf::is_linearized`.
    struct FailingLinearizationPrefixReader {
        inner: Cursor<Vec<u8>>,
        fail_after_prefix: bool,
    }

    impl std::io::Read for FailingLinearizationPrefixReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.fail_after_prefix {
                return Err(std::io::Error::other("linearization prefix read failed"));
            }
            let read = self.inner.read(buffer)?;
            if buffer.len() == 1024 {
                self.fail_after_prefix = true;
            }
            Ok(read)
        }
    }

    impl std::io::Seek for FailingLinearizationPrefixReader {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    #[test]
    fn linearization_probe_error_downgraded_to_warning() {
        // A source-operation failure during the probe must be downgraded to a
        // warning rather than propagating out of `check_reader`, so the caller
        // still receives a report with `linearized = false`.
        let report = check_reader_strict(FailingLinearizationPrefixReader {
            inner: Cursor::new(minimal_pdf_bytes()),
            fail_after_prefix: false,
        })
        .unwrap();
        let summary = report
            .summary
            .expect("summary present: the document opened");
        assert!(!summary.linearized);
        assert!(report.diagnostics.entries().iter().any(|d| {
            d.severity == Severity::Warning && d.message.contains("linearization hint")
        }));
    }

    // -----------------------------------------------------------------------
    // Content-stream check: builders
    // -----------------------------------------------------------------------

    /// Build a single-page PDF whose Page (`3 0 obj`) carries `contents_entry`
    /// verbatim as its `/Contents` value, plus pre-built extra object bytes
    /// appended in order and numbered contiguously from `4`. The tuple's first
    /// element documents the expected object number at the call site; the
    /// builder derives the xref entries from append order. The xref/trailer
    /// mechanics mirror the page-tree test builders in `pages.rs`.
    ///
    /// Object layout: `1` Catalog, `2` Pages, `3` Page, `4+` extras.
    fn content_pdf(contents_entry: &str, extra_objects: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        let page_obj = format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {contents_entry} >>\nendobj\n"
        );
        pdf.extend_from_slice(page_obj.as_bytes());

        let mut extra_offsets: Vec<u64> = Vec::new();
        for (_, body) in extra_objects.iter() {
            extra_offsets.push(pdf.len() as u64);
            pdf.extend_from_slice(body);
        }

        let xref_start = pdf.len() as u64;
        // Callers supply extra objects numbered contiguously from 4, so the
        // xref's in-use entries follow object order with no free-entry gaps.
        let total = 4 + extra_offsets.len();
        let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
        xref.push_str(&format!("{off1:010} 00000 n \n"));
        xref.push_str(&format!("{off2:010} 00000 n \n"));
        xref.push_str(&format!("{off3:010} 00000 n \n"));
        for off in &extra_offsets {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// A FlateDecode stream object encoded via the in-crate encoder so it
    /// round-trips cleanly through `decode_stream_data`.
    fn clean_flate_object(num: u32, body: &[u8]) -> Vec<u8> {
        let mut flate_dict = Dictionary::new();
        flate_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let encoded = encode_stream_data(&flate_dict, body).unwrap();
        let mut out = Vec::new();
        out.extend_from_slice(
            format!(
                "{num} 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n",
                encoded.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(&encoded);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        out
    }

    /// A stream object that declares a `/Filter` but whose payload is not valid
    /// for that codec — i.e. genuinely corrupt encoded data.
    fn corrupt_filtered_object(num: u32, filter: &str, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(
            format!(
                "{num} 0 obj\n<< /Filter /{filter} /Length {} >>\nstream\n",
                payload.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(payload);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        out
    }

    fn corrupt_flate_content_pdf() -> Vec<u8> {
        // /Contents 4 0 R: FlateDecode framing over bytes that are not a valid
        // zlib stream.
        content_pdf(
            "4 0 R",
            &[(
                4,
                corrupt_filtered_object(4, "FlateDecode", b"not a zlib stream at all"),
            )],
        )
    }

    fn truncated_flate_content_pdf() -> Vec<u8> {
        content_pdf(
            "4 0 R",
            &[(4, corrupt_filtered_object(4, "FlateDecode", b"\x78"))],
        )
    }

    fn truncated_abbreviated_flate_content_pdf() -> Vec<u8> {
        content_pdf("4 0 R", &[(4, corrupt_filtered_object(4, "Fl", b"\x78"))])
    }

    fn ascii_hex_error_then_flate_warning_content_pdf() -> Vec<u8> {
        let object = b"4 0 obj\n\
                       << /Filter [/ASCIIHexDecode /FlateDecode] /Length 3 >>\n\
                       stream\n\
                       78G\n\
                       endstream\n\
                       endobj\n"
            .to_vec();
        content_pdf("4 0 R", &[(4, object)])
    }

    fn malformed_header_abbreviated_flate_content_pdf() -> Vec<u8> {
        content_pdf(
            "4 0 R",
            &[(4, corrupt_filtered_object(4, "Fl", b"\x78\x00"))],
        )
    }

    fn truncated_flate_then_invalid_ascii85_content_pdf() -> Vec<u8> {
        let mut flate_dict = Dictionary::new();
        flate_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        // Generate the compressed bytes in-process, then remove only the zlib
        // trailer: Flate emits the invalid ASCII85 byte before warning that its
        // input ended incomplete, so the next declared decoder still runs.
        let mut encoded = encode_stream_data(&flate_dict, b"\x7f").unwrap();
        encoded.pop();

        let mut object = Vec::new();
        object.extend_from_slice(
            format!(
                "4 0 obj\n<< /Filter [/FlateDecode /ASCII85Decode] /Length {} >>\nstream\n",
                encoded.len()
            )
            .as_bytes(),
        );
        object.extend_from_slice(&encoded);
        object.extend_from_slice(b"\nendstream\nendobj\n");
        content_pdf("4 0 R", &[(4, object)])
    }

    fn corrupt_ascii85_content_pdf() -> Vec<u8> {
        // ASCII85Decode framing over a byte that is invalid in an ASCII85 body.
        content_pdf(
            "4 0 R",
            &[(
                4,
                // 0x7F (DEL) is outside the legal ASCII85 alphabet.
                corrupt_filtered_object(4, "ASCII85Decode", b"abc\x7fdef~>"),
            )],
        )
    }

    fn content_array_one_corrupt_pdf() -> Vec<u8> {
        // /Contents [4 0 R 5 0 R]: a clean stream followed by a corrupt one.
        content_pdf(
            "[ 4 0 R 5 0 R ]",
            &[
                (4, clean_flate_object(4, b"BT /F1 12 Tf (ok) Tj ET")),
                (
                    5,
                    corrupt_filtered_object(5, "FlateDecode", b"garbage not zlib"),
                ),
            ],
        )
    }

    fn clean_flate_content_pdf() -> Vec<u8> {
        content_pdf(
            "4 0 R",
            &[(4, clean_flate_object(4, b"BT /F1 12 Tf (hi) Tj ET"))],
        )
    }

    /// A single-page PDF whose `/Contents 4 0 R` is a *valid* FlateDecode stream
    /// that inflates to `decoded_len` bytes — small compressed, large inflated:
    /// a decompression bomb relative to a tight output cap.
    fn bomb_flate_content_pdf(decoded_len: usize) -> Vec<u8> {
        content_pdf(
            "4 0 R",
            &[(4, clean_flate_object(4, &vec![0u8; decoded_len]))],
        )
    }

    /// A FlateDecode image XObject (`num 0 obj`) whose payload is corrupt.
    fn corrupt_flate_image_object(num: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let payload = b"garbage not a zlib stream";
        out.extend_from_slice(
            format!(
                "{num} 0 obj\n<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /FlateDecode /Length {} >>\nstream\n",
                payload.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(payload);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        out
    }

    /// A single-page PDF whose content stream (`4 0 R`) is clean but whose
    /// `/Resources` wires `/Im0` to a corrupt FlateDecode image XObject
    /// (`5 0 R`). The image is reachable from the page, yet `--check` must not
    /// decode it: qpdf decodes page content streams only, leaving image
    /// XObjects untouched. Built bespoke (not via `content_pdf`) so the page
    /// dictionary can carry the `/Resources` link that makes the image a real
    /// referenced XObject rather than an orphan.
    fn corrupt_flate_image_pdf() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /XObject << /Im0 5 0 R >> >> >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(&clean_flate_object(
            4,
            b"BT /F1 12 Tf (hi) Tj ET q /Im0 Do Q",
        ));

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(&corrupt_flate_image_object(5));

        let xref_start = pdf.len() as u64;
        let mut xref = String::from("xref\n0 6\n0000000000 65535 f \n");
        for off in [off1, off2, off3, off4, off5] {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn dct_content_stream_pdf() -> Vec<u8> {
        // Abnormal but legal-to-parse: a content stream declaring /DCTDecode.
        // flpdf cannot decode DCT, so it must be skipped, not flagged.
        content_pdf(
            "4 0 R",
            &[(
                4,
                corrupt_filtered_object(4, "DCTDecode", b"not really jpeg bytes"),
            )],
        )
    }

    /// Page whose `/Contents` is a *direct inline* FlateDecode stream (no
    /// indirect ref), corrupt — exercising the `None` terminal-ref location arm.
    fn corrupt_inline_content_pdf() -> Vec<u8> {
        let payload = b"not a zlib stream";
        let contents = format!(
            "<< /Filter /FlateDecode /Length {} >>\nstream\n{}\nendstream",
            payload.len(),
            std::str::from_utf8(payload).unwrap()
        );
        content_pdf(&contents, &[])
    }

    /// Catalog has `/Root` but the catalog dictionary has no `/Pages` entry, so
    /// `page_refs` errors with `/Pages` missing while `/Root` resolves — the
    /// page-enumeration warning arm.
    fn no_pages_entry_pdf() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(
            format!("xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \n").as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    // -----------------------------------------------------------------------
    // Content-stream check: tests
    // -----------------------------------------------------------------------

    #[test]
    fn corrupt_flate_content_stream_is_error() {
        let report = check_reader_strict(Cursor::new(corrupt_flate_content_pdf())).unwrap();
        assert!(!report.valid);
        assert!(report.diagnostics.entries().iter().any(|d| {
            d.severity == Severity::Error
                && d.message.contains("errors while decoding content stream")
        }));
    }

    #[test]
    fn corrupt_ascii85_content_stream_is_error() {
        let report = check_reader_strict(Cursor::new(corrupt_ascii85_content_pdf())).unwrap();
        assert!(!report.valid);
        assert!(report.diagnostics.entries().iter().any(|d| {
            d.severity == Severity::Error
                && d.message.contains("errors while decoding content stream")
        }));
    }

    #[test]
    fn content_array_with_one_corrupt_stream_is_error() {
        let report = check_reader_strict(Cursor::new(content_array_one_corrupt_pdf())).unwrap();
        assert!(!report.valid);
        // The corrupt element is the second array member, object `5 0 R`, so the
        // diagnostic names that object (with no trailing " R", matching qpdf).
        assert!(report.diagnostics.entries().iter().any(|d| {
            d.severity == Severity::Error
                && d.message.contains("errors while decoding content stream")
                // Trailing colon (not " R:") pins the no-trailing-R qpdf wording.
                && d.message.contains("content stream object 5 0:")
        }));
    }

    #[test]
    fn clean_content_stream_keeps_valid() {
        let report = check_reader_strict(Cursor::new(clean_flate_content_pdf())).unwrap();
        assert!(report.valid);
        assert!(!report
            .diagnostics
            .entries()
            .iter()
            .any(|d| d.message.contains("content stream")));
    }

    #[test]
    fn corrupt_flate_image_xobject_not_checked() {
        // A corrupt FlateDecode image XObject in /Resources must NOT flip valid:
        // qpdf --check decodes page content streams only.
        let report = check_reader_strict(Cursor::new(corrupt_flate_image_pdf())).unwrap();
        assert!(report.valid);
    }

    #[test]
    fn dct_content_stream_skipped_no_false_error() {
        // A /DCTDecode content stream is skipped (flpdf cannot decode it), so a
        // would-be decode failure is never reported as an error.
        let report = check_reader_strict(Cursor::new(dct_content_stream_pdf())).unwrap();
        assert!(report.valid);
        assert!(!report
            .diagnostics
            .entries()
            .iter()
            .any(|d| d.message.contains("content stream")));
    }

    #[test]
    fn direct_inline_content_stream_is_a_qpdf_style_structural_warning() {
        // qpdf 11.9.0 treats this spec-invalid direct stream as malformed page
        // syntax, emits object/page warnings, and completes `--check` with
        // warnings rather than reporting a corrupt decoded content stream.
        let report = check_reader_strict(Cursor::new(corrupt_inline_content_pdf())).unwrap();
        assert!(report.valid);
        assert_eq!(
            report.diagnostics.entries(),
            &[Diagnostic::warning(
                "could not enumerate pages for content-stream check: parse error at byte 109: expected byte 47",
                None,
            )]
        );
    }

    #[test]
    fn page_enumeration_failure_downgraded_to_warning() {
        // The catalog resolves (so /Root is present, no missing-/Root error) but
        // has no /Pages, so `page_refs` errors. The content-stream check must
        // downgrade that to a warning rather than flipping the report invalid.
        let report = check_reader_strict(Cursor::new(no_pages_entry_pdf())).unwrap();
        assert!(report.valid);
        assert!(report.diagnostics.entries().iter().any(|d| {
            d.severity == Severity::Warning
                && d.message
                    .contains("could not enumerate pages for content-stream check")
        }));
    }

    #[test]
    fn per_page_content_read_failure_downgraded_to_warning() {
        // `/Contents 42` is a bare integer: page enumeration succeeds, but the
        // per-page content-stream collection errors. That structural problem is
        // a warning, not an error, so the report stays valid.
        let report = check_reader_strict(Cursor::new(content_pdf("42", &[]))).unwrap();
        assert!(report.valid);
        assert!(report.diagnostics.entries().iter().any(|d| {
            d.severity == Severity::Warning && d.message.contains("could not read content streams")
        }));
    }

    #[test]
    fn decode_output_one_over_limit_warns_not_errors() {
        // 1025 inflated bytes under a 1024-byte cap: the stream is intact, so the
        // guard trips as a WARNING (still valid), never a decode error.
        let report = check_reader_with_options_and_limits(
            Cursor::new(bomb_flate_content_pdf(1025)),
            PdfOpenOptions {
                repair: false,
                ..PdfOpenOptions::default()
            },
            crate::filters::DecodeLimits {
                max_output: Some(1024),
                ..crate::filters::DecodeLimits::default()
            },
        )
        .unwrap();
        assert!(report.valid); // warning only -> still valid
                               // The warning names the offending stream (acceptance: "naming the
                               // stream") and marks it as a guard trip.
        assert!(report.diagnostics.entries().iter().any(|d| {
            d.severity == Severity::Warning
                && d.message.contains("content stream object")
                && d.message.contains("decode-bomb guard")
        }));
        // The guard trip must never be reported as a decode error: that message
        // must appear in no diagnostic at all. (Checking the message regardless
        // of severity also keeps the closure free of a short-circuit that would
        // leave the `contains` arm unevaluated when no error exists.)
        assert!(!report
            .diagnostics
            .entries()
            .iter()
            .any(|d| d.message.contains("errors while decoding content stream")));
    }

    #[test]
    fn truncated_flate_content_is_a_warning_not_an_error() {
        let report = check_reader_with_options(
            Cursor::new(truncated_flate_content_pdf()),
            PdfOpenOptions {
                repair: false,
                ..PdfOpenOptions::default()
            },
        )
        .unwrap();

        assert!(report.valid);
        assert!(report.diagnostics.entries().iter().any(|diagnostic| {
            diagnostic.severity == Severity::Warning
                && diagnostic.message.contains("content stream object 4 0")
                && diagnostic
                    .message
                    .contains("input stream is complete but output may still be valid")
        }));
        assert!(!report
            .diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error));
    }

    #[test]
    fn abbreviated_truncated_flate_content_is_a_warning_not_an_error() {
        let report = check_reader_with_options(
            Cursor::new(truncated_abbreviated_flate_content_pdf()),
            PdfOpenOptions {
                repair: false,
                ..PdfOpenOptions::default()
            },
        )
        .unwrap();

        assert!(report.valid);
        assert!(report.diagnostics.entries().iter().any(|diagnostic| {
            diagnostic.severity == Severity::Warning
                && diagnostic.message.contains("content stream object 4 0")
                && diagnostic
                    .message
                    .contains("input stream is complete but output may still be valid")
        }));
        assert!(!report
            .diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error));
    }

    #[test]
    fn abbreviated_malformed_flate_header_is_a_content_stream_error() {
        let report =
            check_reader_strict(Cursor::new(malformed_header_abbreviated_flate_content_pdf()))
                .unwrap();

        assert!(!report.valid);
        assert!(report.diagnostics.entries().iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.message.contains("content stream object 4 0")
                && diagnostic
                    .message
                    .contains("errors while decoding content stream")
        }));
    }

    #[test]
    fn truncated_flate_warning_survives_later_ascii85_error() {
        let report = check_reader_with_options(
            Cursor::new(truncated_flate_then_invalid_ascii85_content_pdf()),
            PdfOpenOptions {
                repair: false,
                ..PdfOpenOptions::default()
            },
        )
        .unwrap();

        assert!(!report.valid);
        assert!(report.diagnostics.entries().iter().any(|diagnostic| {
            diagnostic.severity == Severity::Warning
                && diagnostic.message.contains("content stream object 4 0")
                && diagnostic
                    .message
                    .contains("input stream is complete but output may still be valid")
        }));
        assert!(report.diagnostics.entries().iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.message.contains("content stream object 4 0")
                && diagnostic
                    .message
                    .contains("errors while decoding content stream")
        }));
    }

    #[test]
    fn post_error_flate_warning_reaches_the_content_check() {
        let report = check_reader_with_options(
            Cursor::new(ascii_hex_error_then_flate_warning_content_pdf()),
            PdfOpenOptions {
                repair: false,
                ..PdfOpenOptions::default()
            },
        )
        .unwrap();

        assert!(!report.valid);
        assert!(report.diagnostics.entries().iter().any(|diagnostic| {
            diagnostic.severity == Severity::Warning
                && diagnostic.message.contains("content stream object 4 0")
                && diagnostic
                    .message
                    .contains("input stream is complete but output may still be valid")
        }));
        assert!(report.diagnostics.entries().iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.message.contains("content stream object 4 0")
                && diagnostic
                    .message
                    .contains("errors while decoding content stream")
        }));
    }

    #[test]
    fn direct_content_stream_warning_names_inline_location() {
        let mut pdf = Pdf::open(Cursor::new(clean_flate_content_pdf())).unwrap();
        let mut flate_dict = Dictionary::new();
        flate_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let mut page = Dictionary::new();
        page.insert("Type", Object::Name(b"Page".to_vec()));
        page.insert(
            "Contents",
            Object::Stream(Stream::new(flate_dict, b"\x78".to_vec())),
        );
        pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

        let mut diagnostics = Diagnostics::default();
        check_content_streams(&mut pdf, &mut diagnostics, DecodeLimits::default());

        assert!(diagnostics.entries().iter().any(|diagnostic| {
            diagnostic.severity == Severity::Warning
                && diagnostic.message.contains("inline content stream")
                && diagnostic
                    .message
                    .contains("input stream is complete but output may still be valid")
        }));
    }

    #[test]
    fn decode_output_exactly_at_limit_is_clean() {
        // Boundary: inflated output == cap succeeds (no warning, no error).
        let report = check_reader_with_options_and_limits(
            Cursor::new(bomb_flate_content_pdf(1024)),
            PdfOpenOptions {
                repair: false,
                ..PdfOpenOptions::default()
            },
            crate::filters::DecodeLimits {
                max_output: Some(1024),
                ..crate::filters::DecodeLimits::default()
            },
        )
        .unwrap();
        assert!(report.valid);
        assert!(!report
            .diagnostics
            .entries()
            .iter()
            .any(|d| d.message.contains("content stream")));
    }

    #[test]
    fn decode_limit_does_not_mask_corruption() {
        // With a cap set, a genuinely corrupt FlateDecode stream is still an
        // ERROR, not a guard warning — the limit path must not swallow real
        // decode failures.
        let report = check_reader_with_options_and_limits(
            Cursor::new(corrupt_flate_content_pdf()),
            PdfOpenOptions {
                repair: false,
                ..PdfOpenOptions::default()
            },
            crate::filters::DecodeLimits {
                max_output: Some(1024),
                ..crate::filters::DecodeLimits::default()
            },
        )
        .unwrap();
        assert!(!report.valid);
        assert!(report.diagnostics.entries().iter().any(|d| {
            d.severity == Severity::Error
                && d.message.contains("errors while decoding content stream")
        }));
    }

    #[test]
    fn unlimited_default_decodes_large_stream_without_warning() {
        // Regression guard: with no cap (default), the same large stream decodes
        // fine — behaviour is unchanged from before the limit existed.
        let report = check_reader_with_options_and_limits(
            Cursor::new(bomb_flate_content_pdf(64 * 1024)),
            PdfOpenOptions {
                repair: false,
                ..PdfOpenOptions::default()
            },
            crate::filters::DecodeLimits::default(),
        )
        .unwrap();
        assert!(report.valid);
        assert!(!report
            .diagnostics
            .entries()
            .iter()
            .any(|d| d.message.contains("content stream")));
    }
}

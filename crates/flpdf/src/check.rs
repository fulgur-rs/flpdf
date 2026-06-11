//! Lightweight document validator.
//!
//! Mirrors the surface of qpdf's `--check`: open the document (with the recovery path
//! enabled), report parser warnings, and flag a few high-level invariants that would
//! cause downstream tools to fail.

use crate::{Diagnostic, Diagnostics, Error, Pdf, PdfOpenOptions};
use std::io::{Read, Seek};

/// Result of [`check_reader`].
///
/// `valid` is `true` when no [`Diagnostic`] of severity `Error` was produced. Warnings
/// alone (e.g. linearization advisories) do not flip the flag.
#[derive(Debug, Clone)]
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
pub struct CheckSummary {
    /// PDF version from the file header, e.g. `"1.7"` (no `%PDF-` prefix).
    pub version: String,
    /// Whether the document authenticated an `/Encrypt` dictionary on open.
    pub encrypted: bool,
    /// Whether the document carries a linearization hint object.
    pub linearized: bool,
}

/// Validate the document behind `reader` using the repair-enabled open path.
///
/// Errors during the strict parse are downgraded to a single error diagnostic so the
/// caller always receives a report. Equivalent to `qpdf --check` (which also runs the
/// recovery heuristics).
///
/// # Errors
///
/// - [`Error::Encrypted`] when the document is encrypted and cannot be opened; unlike
///   other open failures, this is propagated rather than downgraded to a diagnostic.
/// - Propagates errors from [`Pdf::linearized_hint_ref`] (resolving object `(1, 0)`)
///   raised while probing for linearization.
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
pub fn check_reader<R: Read + Seek>(reader: R) -> crate::Result<CheckReport> {
    check_reader_inner(reader, true)
}

/// Validate the document with explicit open options.
///
/// # Errors
///
/// - When `options.repair` is set, behaves like [`check_reader`]: only
///   [`Error::Encrypted`] is propagated from the open path, while other open
///   failures become an error [`Diagnostic`] inside an `Ok(CheckReport)`.
/// - When `options.repair` is clear, any error from [`Pdf::open_with_options`] is
///   propagated unchanged (e.g. [`Error::Io`], [`Error::Parse`], [`Error::Encrypted`]).
/// - Propagates errors from [`Pdf::linearized_hint_ref`] (resolving object `(1, 0)`)
///   raised while probing for linearization.
pub fn check_reader_with_options<R: Read + Seek>(
    reader: R,
    options: PdfOpenOptions,
) -> crate::Result<CheckReport> {
    check_reader_inner_with_options(reader, options)
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
/// - Propagates errors from [`Pdf::linearized_hint_ref`] (resolving object `(1, 0)`)
///   raised while probing for linearization.
pub fn check_reader_strict<R: Read + Seek>(reader: R) -> crate::Result<CheckReport> {
    check_reader_inner(reader, false)
}

fn check_reader_inner<R: Read + Seek>(reader: R, allow_repair: bool) -> crate::Result<CheckReport> {
    check_reader_inner_with_options(
        reader,
        PdfOpenOptions {
            repair: allow_repair,
            ..PdfOpenOptions::default()
        },
    )
}

fn check_reader_inner_with_options<R: Read + Seek>(
    reader: R,
    options: PdfOpenOptions,
) -> crate::Result<CheckReport> {
    let allow_repair = options.repair;
    let mut pdf = if allow_repair {
        match Pdf::open_with_options(reader, options) {
            Ok(pdf) => pdf,
            Err(error @ Error::Encrypted(_)) => return Err(error),
            Err(error) => {
                let mut diagnostics = Diagnostics::default();
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

    let mut diagnostics = pdf.repair_diagnostics().clone();
    if pdf.uses_weak_crypto() {
        diagnostics.push(Diagnostic::warning(
            "encrypted PDF uses weak crypto; processing continued",
            None,
        ));
    }
    if pdf.trailer().get_ref("Root").is_none() {
        diagnostics.push(Diagnostic::error("trailer is missing /Root", None));
    }
    let linearized = is_linearized_pdf(&mut pdf)?;
    if linearized {
        diagnostics.push(Diagnostic::warning(
            "linearized PDF detected: rewrite support preserves hint object but does not recompute linearization tables",
            None,
        ));
    }

    let summary = CheckSummary {
        version: pdf.version().to_string(),
        encrypted: pdf.is_encrypted(),
        linearized,
    };

    Ok(CheckReport {
        valid: !diagnostics.has_errors(),
        diagnostics,
        summary: Some(summary),
    })
}

fn is_linearized_pdf<R: Read + Seek>(reader: &mut Pdf<R>) -> crate::Result<bool> {
    reader.linearized_hint_ref().map(|hint| hint.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
}

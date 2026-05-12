//! Lightweight document validator.
//!
//! Mirrors the surface of qpdf's `--check`: open the document (with the recovery path
//! enabled), report parser warnings, and flag a few high-level invariants that would
//! cause downstream tools to fail.

use crate::{Diagnostic, Diagnostics, Pdf, PdfOpenOptions};
use std::io::{Read, Seek};

/// Result of [`check_reader`].
///
/// `valid` is `true` when no [`Diagnostic`] of severity `Error` was produced. Warnings
/// alone (e.g. linearization advisories) do not flip the flag.
#[derive(Debug, Clone)]
pub struct CheckReport {
    pub valid: bool,
    pub diagnostics: Diagnostics,
}

/// Validate the document behind `reader` using the repair-enabled open path.
///
/// Errors during the strict parse are downgraded to a single error diagnostic so the
/// caller always receives a report. Equivalent to `qpdf --check` (which also runs the
/// recovery heuristics).
pub fn check_reader<R: Read + Seek>(reader: R) -> crate::Result<CheckReport> {
    check_reader_inner(reader, true)
}

/// Validate the document with explicit open options.
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
            Err(error) => {
                let mut diagnostics = Diagnostics::default();
                diagnostics.push(Diagnostic::error(error.to_string(), None));
                return Ok(CheckReport {
                    valid: false,
                    diagnostics,
                });
            }
        }
    } else {
        Pdf::open_with_options(reader, options)?
    };

    let mut diagnostics = pdf.repair_diagnostics().clone();
    if pdf.trailer().get_ref("Root").is_none() {
        diagnostics.push(Diagnostic::error("trailer is missing /Root", None));
    }
    if is_linearized_pdf(&mut pdf)? {
        diagnostics.push(Diagnostic::warning(
            "linearized PDF detected: rewrite support preserves hint object but does not recompute linearization tables",
            None,
        ));
    }

    Ok(CheckReport {
        valid: !diagnostics.has_errors(),
        diagnostics,
    })
}

fn is_linearized_pdf<R: Read + Seek>(reader: &mut Pdf<R>) -> crate::Result<bool> {
    reader.linearized_hint_ref().map(|hint| hint.is_some())
}

//! Byte-identity: flpdf `disable_digital_signatures` + full rewrite ==
//! `qpdf --remove-restrictions --static-id`.
//!
//! Proves flpdf's `--remove-restrictions` signature handling is a byte-for-byte
//! faithful port of qpdf 11.9.0's `QPDFAcroFormDocumentHelper::disableDigitalSignatures`.
//!
//! The three common cases:
//!   1. catalog `/Perms /DocMDP` only (no `/AcroForm`) -> `/Perms` removed, sig GC'd;
//!   2. AcroForm `/Sig` field not referenced from a page `/Annots` -> `/Fields`
//!      emptied, field + sig dict GC'd, `/SigFlags` 0;
//!   3. AcroForm `/Sig` field also referenced from `/Annots` (merged widget) ->
//!      `/Fields` emptied, widget survives as a plain annotation (`/FT`/`/V`
//!      stripped, `/T` kept), sig dict GC'd, `/SigFlags` 0.
//!
//! Plus five `getFormFields` edge cases that decide which `/Sig` fields are
//! disabled:
//!   4. non-terminal `/Sig` parent grouping a widget via `/Kids` -> signature
//!      kept (only the widget is a field; the parent carries no signature keys),
//!      only `/SigFlags` zeroed;
//!   5. terminal `/Sig` field that is not an annotation (no `/Rect`/`/Subtype`/
//!      `/AP`) -> not a form field, signature kept, only `/SigFlags` zeroed;
//!   6. `/FT /Sig` widget on a page `/Annots` but absent from `/Fields` ->
//!      discovered by the orphan-widget pass and disabled (`/FT`/`/V` removed,
//!      sig dict GC'd);
//!   7. unsigned `/FT /Sig` widget (no `/V`, no `/SigFlags`) -> still a form
//!      field, `/FT` removed and erased from `/Fields` (widget survives);
//!   8. `/Sig` parent whose `/Kids` holds a pure widget (no `/Parent`, no `/T`)
//!      -> the parent is the owning field: disabled and erased from `/Fields`
//!      (parent then GC'd).
//!
//! Plus two structural corner cases:
//!   9. `/V` signature dict also reachable from catalog `/DSS` -> the dict is
//!      kept (only the field's `/V` is removed; the write-time reachability GC
//!      keeps it because `/DSS` still references it), `/Fields` emptied,
//!      `/SigFlags` 0, the widget survives as a plain annotation;
//!  10. `/AcroForm /Fields` is an indirect array -> the array object is mutated
//!      in place (erased to empty) and `/Fields` stays a reference, matching
//!      qpdf (it does not inline a new direct array).
//!
//! These fixtures are content-stream-free, so byte-identity is independent of the
//! deflate backend — this file is NOT gated on `qpdf-zlib-compat`.

use flpdf::{
    disable_digital_signatures, write_pdf_with_options, NewlineBeforeEndstream, Pdf, WriteOptions,
};
use std::path::Path;

/// Full-rewrite `fixture` after `disable_digital_signatures`, with the
/// qpdf-matching option set, and return the bytes.
fn remove_restrictions_qpdf_equivalent(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    disable_digital_signatures(&mut pdf).unwrap();

    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.static_id = true;
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;

    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &opts).unwrap();
    out
}

fn golden(stem: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(stem)
        .join("remove-restrictions.pdf");
    std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
}

fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    if a == b {
        return None;
    }
    let common = a.len().min(b.len());
    for i in 0..common {
        if a[i] != b[i] {
            return Some(i);
        }
    }
    Some(common)
}

fn assert_parity(fixture: &str, stem: &str) {
    let actual = remove_restrictions_qpdf_equivalent(fixture);
    let expected = golden(stem);
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{fixture}: not byte-identical to qpdf --remove-restrictions golden \
             (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            &actual[lo..(off + 16).min(actual.len())],
            &expected[lo..(off + 16).min(expected.len())],
        );
    }
}

#[test]
fn perms_docmdp_only_is_byte_identical_to_qpdf() {
    assert_parity("perms-docmdp-one-page.pdf", "perms-docmdp-one-page");
}

#[test]
fn acroform_sig_field_only_is_byte_identical_to_qpdf() {
    assert_parity("acroform-sig-field-only.pdf", "acroform-sig-field-only");
}

#[test]
fn acroform_sig_widget_survives_as_annotation_byte_identical_to_qpdf() {
    assert_parity("acroform-sig-widget.pdf", "acroform-sig-widget");
}

#[test]
fn acroform_sig_nonterminal_parent_keeps_signature_byte_identical_to_qpdf() {
    // A /Sig parent groups a widget via /Kids: only the widget is a form field,
    // it carries no signature keys of its own, and it is not a top-level /Fields
    // entry, so qpdf keeps the whole signature; only /SigFlags is zeroed.
    assert_parity(
        "acroform-sig-nonterminal-parent.pdf",
        "acroform-sig-nonterminal-parent",
    );
}

#[test]
fn acroform_sig_nonannotation_terminal_kept_byte_identical_to_qpdf() {
    // A terminal /Sig field with no /Rect//Subtype//AP is not an annotation, so
    // it is not a form field; qpdf keeps it and only zeroes /SigFlags.
    assert_parity(
        "acroform-sig-nonannotation-terminal.pdf",
        "acroform-sig-nonannotation-terminal",
    );
}

#[test]
fn acroform_sig_orphan_widget_disabled_byte_identical_to_qpdf() {
    // A /FT /Sig widget in a page /Annots but absent from /Fields is discovered
    // by qpdf's orphan-widget pass and disabled (/FT//V removed, sig dict GC'd).
    assert_parity(
        "acroform-sig-orphan-widget.pdf",
        "acroform-sig-orphan-widget",
    );
}

#[test]
fn acroform_sig_unsigned_placeholder_disabled_byte_identical_to_qpdf() {
    // A /FT /Sig widget with no /V and no /SigFlags is still a form field;
    // qpdf removes /FT and erases it from /Fields (the widget itself survives).
    assert_parity(
        "acroform-sig-unsigned-placeholder.pdf",
        "acroform-sig-unsigned-placeholder",
    );
}

#[test]
fn acroform_sig_parent_pure_widget_kid_disables_parent_byte_identical_to_qpdf() {
    // A /Sig parent whose /Kids holds a pure widget (no /Parent, no /T): the
    // widget is an annotation but not a field, so its owning field is the
    // parent, which qpdf disables and erases from /Fields (parent then GC's).
    assert_parity(
        "acroform-sig-parent-pure-widget-kid.pdf",
        "acroform-sig-parent-pure-widget-kid",
    );
}

#[test]
fn dss_shared_sig_dict_survives_byte_identical_to_qpdf() {
    // The /V signature dict is also referenced from catalog /DSS, so qpdf's
    // write-time GC keeps it (only the field's /V is removed). flpdf must not
    // eagerly delete it, or the /DSS reference would dangle to null.
    assert_parity("acroform-sig-dss-shared.pdf", "acroform-sig-dss-shared");
}

#[test]
fn indirect_fields_array_preserved_byte_identical_to_qpdf() {
    // /AcroForm /Fields is an indirect array: qpdf erases items from the
    // original array object and keeps /Fields indirect, rather than inlining a
    // new direct array into the /AcroForm dictionary.
    assert_parity(
        "acroform-sig-indirect-fields.pdf",
        "acroform-sig-indirect-fields",
    );
}

//! Integration tests: AcroForm/annotation transform observable equivalence
//!
//! Covers: generate-appearances (Tx value→Tj, checkbox/radio state sync,
//! combo value→Tj), flatten=all (Do in page content, Annots empty),
//! flatten=print (Print-bit annot removed, non-Print annot kept),
//! flatten-rotation (CLI e2e).
//!
//! # Observable-equivalence strategy
//!
//! These tests do **not** perform byte-level or pixel comparisons.  Instead,
//! they re-parse the output PDF and assert on structural/content markers:
//!
//! - Appearance generation: widget `/AP/N` is present and, where possible,
//!   its (uncompressed) content stream contains the expected operators.
//! - Flattening: annotation removed from `/Annots`, `Do` appears in page
//!   content stream (decoded).
//! - flatten=print: two annotations — one with Print bit (0x4 in /F), one
//!   without — only the Print-bit one is removed.
//!
//! Tests that inspect raw page or appearance bytes use `--compress-streams=n`.
//! The existing-appearance reuse test intentionally uses `--compress-streams=y`
//! and decodes through the canonical `ObjectHandle` API so the token-filter
//! writer path is exercised.
//!
//! # qpdf divergence
//!
//! See `docs/qpdf-compat-decisions.md` §AcroForm & annotation transforms.

use assert_cmd::Command;
use flpdf::{AnnotationObjectHelper, DecodeLevel, Pdf};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

mod common;
use common::PdfCanonicalTestExt;
use common::{first_widget_ref, page_annotation_handles};

// ── Fixture helpers ───────────────────────────────────────────────────────────

/// Build a minimal PDF from a flat list of object bodies (1-indexed from 1).
fn assemble_pdf(objects: &[Vec<u8>]) -> Vec<u8> {
    use std::io::Write;
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for object in objects {
        offsets.push(bytes.len() as u32);
        bytes.extend_from_slice(object);
    }
    let start_xref = bytes.len();
    // Write directly into the byte buffer (writeln! to a Vec<u8> is infallible)
    // instead of allocating an intermediate String per line.
    let _ = writeln!(&mut bytes, "xref\n0 {}", objects.len() + 1);
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for &offset in &offsets {
        let _ = writeln!(&mut bytes, "{offset:010} 00000 n ");
    }
    let _ = writeln!(
        &mut bytes,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
        objects.len() + 1,
        start_xref
    );
    bytes
}

/// Single-page AcroForm PDF with a Tx widget that has `/V` but no `/AP`.
/// Objects: 1=Catalog, 2=Pages, 3=Page, 4=Widget, 5=Contents
fn tx_widget_without_ap() -> Vec<u8> {
    assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
          /AcroForm << /Fields [4 0 R] /DR << >> /DA (/Helv 12 Tf 0 g) >> >>\nendobj\n"
            .to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 5 0 R /Annots [4 0 R] >>\nendobj\n"
            .to_vec(),
        // Widget: /FT /Tx, /V (Hello), /DA, /Rect with non-degenerate size
        b"4 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (name1) \
          /V (Hello) /DA (/Helv 12 Tf 0 g) \
          /Rect [100 700 300 720] /P 3 0 R >>\nendobj\n"
            .to_vec(),
        b"5 0 obj\n<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\nendobj\n".to_vec(),
    ])
}

/// Same fixture as [`tx_widget_without_ap`], but requires viewer-side
/// appearance generation until the CLI's generation pass clears the flag.
fn tx_widget_without_ap_needing_appearances() -> Vec<u8> {
    assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
          /AcroForm << /Fields [4 0 R] /NeedAppearances true /DR << >> /DA (/Helv 12 Tf 0 g) >> >>\nendobj\n"
            .to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 5 0 R /Annots [4 0 R] >>\nendobj\n"
            .to_vec(),
        b"4 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (name1) \
          /V (Hello) /DA (/Helv 12 Tf 0 g) \
          /Rect [100 700 300 720] /P 3 0 R >>\nendobj\n"
            .to_vec(),
        b"5 0 obj\n<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\nendobj\n".to_vec(),
    ])
}

/// Same as [`tx_widget_without_ap_needing_appearances`], except `/AP/N` is an
/// indirect null. qpdf treats that as a missing normal appearance and replaces
/// it while generating appearances.
fn tx_widget_with_null_ap_n_needing_appearances() -> Vec<u8> {
    assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
          /AcroForm << /Fields [4 0 R] /NeedAppearances true /DR << >> /DA (/Helv 12 Tf 0 g) >> >>\nendobj\n"
            .to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 5 0 R /Annots [4 0 R] >>\nendobj\n"
            .to_vec(),
        b"4 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (name1) \
          /V (Hello) /DA (/Helv 12 Tf 0 g) \
          /Rect [100 700 300 720] /P 3 0 R /AP << /N 6 0 R >> >>\nendobj\n"
            .to_vec(),
        b"5 0 obj\n<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\nendobj\n".to_vec(),
        b"6 0 obj\nnull\nendobj\n".to_vec(),
    ])
}

/// Single-page AcroForm PDF with a Tx widget that has `/V` AND an existing
/// `/AP/N` Form XObject containing the literal value bytes.
/// Objects: 1=Catalog, 2=Pages, 3=Page, 4=Widget, 5=Contents, 6=AP/N XObject
fn tx_widget_with_ap() -> Vec<u8> {
    tx_widget_with_ap_needing(false)
}

/// Same existing-appearance fixture, with `/NeedAppearances true` so the
/// input also exercises qpdf's outer appearance-generation gate.
fn tx_widget_with_ap_needing_appearances() -> Vec<u8> {
    tx_widget_with_ap_needing(true)
}

fn tx_widget_with_ap_needing(need_appearances: bool) -> Vec<u8> {
    let acroform = if need_appearances {
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
          /AcroForm << /Fields [4 0 R] /NeedAppearances true /DR << >> /DA (/Helv 12 Tf 0 g) >> >>\nendobj\n"
            .to_vec()
    } else {
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
          /AcroForm << /Fields [4 0 R] /DR << >> /DA (/Helv 12 Tf 0 g) >> >>\nendobj\n"
            .to_vec()
    };
    assemble_pdf(&[
        acroform,
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 5 0 R /Annots [4 0 R] >>\nendobj\n"
            .to_vec(),
        b"4 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (name1) \
          /V (Hello) /DA (/Helv 12 Tf 0 g) \
          /Rect [100 700 300 720] /P 3 0 R \
          /AP << /N 6 0 R >> >>\nendobj\n"
            .to_vec(),
        b"5 0 obj\n<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\nendobj\n".to_vec(),
        b"6 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 200 20] \
          /Length 17 >>\nstream\nBT (Hello) Tj ET\nendstream\nendobj\n"
            .to_vec(),
    ])
}

/// A nested Tx field whose merged terminal widget carries a local value that
/// differs from its top-level ancestor. qpdf's appearance-generation route
/// resolves the widget association directly, so it must render
/// `(child-value)` rather than the ancestor's `(parent-value)`.
fn nested_tx_widget_with_local_value() -> Vec<u8> {
    assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
          /AcroForm << /Fields [4 0 R] /NeedAppearances true \
          /DR << /Font << /Helv 7 0 R >> >> /DA (/Helv 12 Tf 0 g) >> >>\nendobj\n"
            .to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 6 0 R /Annots [5 0 R] >>\nendobj\n"
            .to_vec(),
        b"4 0 obj\n<< /FT /Tx /T (parent) /V (parent-value) /DA (/Helv 12 Tf 0 g) \
          /Kids [5 0 R] >>\nendobj\n"
            .to_vec(),
        b"5 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (child) \
          /V (child-value) /DA (/Helv 12 Tf 0 g) /Parent 4 0 R \
          /Rect [100 700 300 720] /P 3 0 R \
          /AP << /N 8 0 R >> >>\nendobj\n"
            .to_vec(),
        b"6 0 obj\n<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\nendobj\n".to_vec(),
        b"7 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>\nendobj\n".to_vec(),
        b"8 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 200 20] \
          /Resources << /ProcSet [/PDF /Text] /Font << /Helv 7 0 R >> >> \
          /Length 33 >>\nstream\n/Tx BMC\nBT (old-value) Tj ET\nEMC\nendstream\nendobj\n"
            .to_vec(),
    ])
}

fn widget_normal_appearance_data(path: &Path) -> Vec<u8> {
    let mut pdf = Pdf::open(BufReader::new(File::open(path).unwrap())).unwrap();
    let widget_ref = first_widget_ref(&mut pdf);
    let mut helper = AnnotationObjectHelper::new(widget_ref, &mut pdf);
    helper
        .get_appearance_stream(b"N", None)
        .unwrap()
        .get_stream_data(DecodeLevel::Generalized)
        .unwrap()
        .to_vec()
}

/// Single-page AcroForm PDF with a checkbox (Btn, no pushbutton/radio bits).
/// Widget has /FT /Btn, /AS /Off (unchecked state), /Rect, no /AP.
fn checkbox_widget_without_ap() -> Vec<u8> {
    assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
          /AcroForm << /Fields [4 0 R] /DR << >> >> >>\nendobj\n"
            .to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 5 0 R /Annots [4 0 R] >>\nendobj\n"
            .to_vec(),
        // Checkbox: /Ff 0 (no pushbutton bit 17, no radio bit 16)
        b"4 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Btn /T (cb1) \
          /Ff 0 /AS /Off \
          /Rect [100 700 120 720] /P 3 0 R >>\nendobj\n"
            .to_vec(),
        b"5 0 obj\n<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\nendobj\n".to_vec(),
    ])
}

/// Checkbox whose non-`/Off` value has no normal appearance dictionary.
/// qpdf still routes every `/Btn` through `setV(getValue())`, normalizing the
/// fallback on-state to `/Yes` even though it cannot update `/AS` without an
/// identifiable appearance-bearing widget.
fn checkbox_widget_with_value_without_ap_needing_appearances() -> Vec<u8> {
    assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] /NeedAppearances true /DR << >> >> >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Annots [4 0 R] >>\nendobj\n".to_vec(),
        b"4 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Btn /T (cb1) /Ff 0 /V /On /AS /Off /Rect [100 700 120 720] /P 3 0 R >>\nendobj\n".to_vec(),
        b"5 0 obj\n<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\nendobj\n".to_vec(),
    ])
}

/// Checkbox whose existing state appearances disagree with its `/V`.
/// qpdf 11.9.0 leaves those appearances in place and resets `/AS` to `/V`.
fn checkbox_widget_with_ap_needing_appearances() -> Vec<u8> {
    assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] /NeedAppearances true /DR << >> >> >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Annots [4 0 R] >>\nendobj\n".to_vec(),
        b"4 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Btn /T (cb1) /Ff 0 /V /Yes /AS /Off /Rect [100 700 120 720] /P 3 0 R /AP << /N << /Off 6 0 R /Yes 7 0 R >> >> >>\nendobj\n".to_vec(),
        b"5 0 obj\n<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\nendobj\n".to_vec(),
        b"6 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length 0 >>\nstream\n\nendstream\nendobj\n".to_vec(),
        b"7 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length 0 >>\nstream\n\nendstream\nendobj\n".to_vec(),
    ])
}

/// Direct radio widget with an existing appearance. qpdf only synchronizes
/// radio widgets selected through a field `/Kids` array.
fn direct_radio_widget_with_ap_needing_appearances() -> Vec<u8> {
    assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] /NeedAppearances true /DR << >> >> >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Annots [4 0 R] >>\nendobj\n".to_vec(),
        b"4 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Btn /T (rd1) /Ff 32768 /V /Yes /AS /Off /Rect [100 700 120 720] /P 3 0 R /AP << /N << /Off 6 0 R /Yes 7 0 R >> >> >>\nendobj\n".to_vec(),
        b"5 0 obj\n<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\nendobj\n".to_vec(),
        b"6 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length 0 >>\nstream\n\nendstream\nendobj\n".to_vec(),
        b"7 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length 0 >>\nstream\n\nendstream\nendobj\n".to_vec(),
    ])
}

/// Single-page AcroForm PDF with a radio button widget.
/// /Ff bit 16 (0x8000) = radio, bit 17 clear.
fn radio_widget_without_ap() -> Vec<u8> {
    assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
          /AcroForm << /Fields [4 0 R] /DR << >> >> >>\nendobj\n"
            .to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 5 0 R /Annots [4 0 R] >>\nendobj\n"
            .to_vec(),
        // Radio: /Ff 32768 (0x8000 = bit 16 set, bit 17 clear)
        b"4 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Btn /T (rd1) \
          /Ff 32768 /AS /Off \
          /Rect [200 700 220 720] /P 3 0 R >>\nendobj\n"
            .to_vec(),
        b"5 0 obj\n<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\nendobj\n".to_vec(),
    ])
}

/// Single-page AcroForm PDF with a combo-box (Ch, Ff bit 18 = 0x20000).
/// /V holds the selected option string; /Opt not required for appearance.
fn combo_widget_without_ap() -> Vec<u8> {
    assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
          /AcroForm << /Fields [4 0 R] /NeedAppearances true /DR << >> /DA (/Helv 10 Tf 0 g) >> >>\nendobj\n"
            .to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 5 0 R /Annots [4 0 R] >>\nendobj\n"
            .to_vec(),
        // Combo: /FT /Ch, /Ff 131072 (0x20000 = bit 18), /V = selected option
        b"4 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Ch /T (combo1) \
          /Ff 131072 /V (Option2) /DA (/Helv 10 Tf 0 g) \
          /Rect [100 650 300 670] /P 3 0 R >>\nendobj\n"
            .to_vec(),
        b"5 0 obj\n<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\nendobj\n".to_vec(),
    ])
}

/// Single-page PDF with two annotations that both have an /AP/N XObject.
/// Annotation 4: /F 4 (Print bit set)  → should be flattened in Print mode.
/// Annotation 5: /F 0 (no Print bit)   → should survive in Print mode.
/// Annotation 6, 7: the two AP/N XObjects (minimal).
fn two_annots_print_and_non_print() -> Vec<u8> {
    assemble_pdf(&[
        // 1=Catalog (no AcroForm needed; these are plain annotations)
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 8 0 R /Annots [4 0 R 5 0 R] >>\nendobj\n"
            .to_vec(),
        // Annot with Print bit (/F = 4 = 0x4)
        b"4 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (f1) \
          /V (A) /F 4 /Rect [50 700 150 720] /P 3 0 R /AP << /N 6 0 R >> >>\nendobj\n"
            .to_vec(),
        // Annot without Print bit (/F = 0)
        b"5 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (f2) \
          /V (B) /F 0 /Rect [200 700 300 720] /P 3 0 R /AP << /N 7 0 R >> >>\nendobj\n"
            .to_vec(),
        // AP/N for annot 4
        b"6 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] \
          /Length 13 >>\nstream\nBT (A) Tj ET\nendstream\nendobj\n"
            .to_vec(),
        // AP/N for annot 5
        b"7 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] \
          /Length 13 >>\nstream\nBT (B) Tj ET\nendstream\nendobj\n"
            .to_vec(),
        // Page content
        b"8 0 obj\n<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\nendobj\n".to_vec(),
    ])
}

// ── Tests: generate-appearances ───────────────────────────────────────────────

/// The qpdf-shaped top-level argv surface must route appearance generation to
/// the same canonical helper as the native rewrite subcommand.
#[test]
fn top_level_generate_appearances_routes_to_canonical_writer() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("top-level-tx.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_widget_with_ap_needing_appearances()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--qdf",
            "--no-original-object-ids",
            "--static-id",
            "--generate-appearances",
            "--compress-streams=n",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let widget_ref = first_widget_ref(&mut pdf);
    let mut form = flpdf::AcroFormDocumentHelper::new(&mut pdf).unwrap();
    assert!(!form.get_need_appearances().unwrap());
    drop(form);
    let mut helper = AnnotationObjectHelper::new(widget_ref, &mut pdf);
    let appearance = helper
        .get_appearance_stream(b"N", None)
        .unwrap()
        .get_stream_data(DecodeLevel::Generalized)
        .expect("top-level generation must install /AP/N");
    assert!(
        appearance.windows(2).any(|window| window == b"Tj"),
        "top-level --generate-appearances must render the field value"
    );
}

/// qpdf accepts the linearized combination even though its two-pass writer
/// may expose the known stale token-filter content on the second pass. The
/// CLI must preserve qpdf's acceptance and output lifecycle rather than
/// rejecting the option combination before the writer runs.
#[test]
fn native_linearize_generate_appearances_is_accepted_like_qpdf() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("linearized-tx.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_widget_with_ap_needing_appearances()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            "--static-id",
            "--generate-appearances",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();
    assert!(
        output.is_file(),
        "accepted linearized job must write output"
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["check-linearization", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("no linearization errors"));
}

/// qpdf's `generateAppearancesIfNeeded` returns before scanning pages when
/// `/NeedAppearances` is absent or false. The CLI option must preserve that
/// gate rather than treating the option as an unconditional renderer switch.
#[test]
fn generate_appearances_without_need_marker_is_a_noop() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("tx-no-marker.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_widget_without_ap()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--generate-appearances", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let widget_ref = first_widget_ref(&mut pdf);
    let mut helper = AnnotationObjectHelper::new(widget_ref, &mut pdf);
    assert!(
        helper.get_appearance_dictionary().unwrap().is_null(),
        "qpdf leaves a widget without /AP unchanged when /NeedAppearances is false"
    );
}

/// `--generate-appearances` on a Tx widget adds `/AP/N`, and the uncompressed
/// content stream of that XObject contains a `Tj` operator (the value text is
/// rendered).
#[test]
fn generate_appearances_tx_ap_n_contains_tj() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("tx.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_widget_without_ap_needing_appearances()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--generate-appearances", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let widget_ref = first_widget_ref(&mut pdf);

    // /AP/N must be present after generate-appearances, and resolve to the
    // Form XObject stream.
    let mut helper = AnnotationObjectHelper::new(widget_ref, &mut pdf);
    let n = helper.get_appearance_stream(b"N", None).unwrap();
    let data = n
        .get_stream_data(DecodeLevel::Generalized)
        .expect("Tx widget should have /AP/N after --generate-appearances");

    // The uncompressed content stream must contain "Tj" (the text-show operator).
    // We use --compress-streams=n so the stream data is the raw uncompressed bytes.
    assert!(
        data.windows(2).any(|w| w == b"Tj"),
        "/AP/N content stream must contain Tj operator (observable: value rendered); \
         stream bytes: {:?}",
        std::str::from_utf8(&data).unwrap_or("<non-utf8>")
    );
}

/// `--generate-appearances` updates a non-button widget that already has
/// `/AP/N`, matching qpdf's `ValueSetter` reuse path.
#[test]
fn generate_appearances_tx_reuses_existing_ap() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("tx_ap.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_widget_with_ap_needing_appearances()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--generate-appearances", "--compress-streams=y"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    // /AP/N must still be present and must have gone through the canonical
    // renderer's existing-stream token-filter path.
    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let widget_ref = first_widget_ref(&mut pdf);
    let mut helper = AnnotationObjectHelper::new(widget_ref, &mut pdf);
    let n_handle = helper
        .get_appearance_stream(b"N", None)
        .expect("/AP must survive --generate-appearances for widget that already has one");

    assert!(
        n_handle.object_ref().is_some(),
        "/AP/N must remain an indirect stream reference"
    );
    assert!(
        n_handle.as_stream_dict().is_some(),
        "/AP/N must be a stream"
    );
    let data = n_handle.get_stream_data(DecodeLevel::Generalized).unwrap();
    let as_str = std::str::from_utf8(data.as_slice()).unwrap_or("<non-utf8>");

    assert!(
        data.windows(7).any(|w| w == b"/Tx BMC"),
        "existing /AP/N must be updated through the generated-appearance path; data={as_str:?}"
    );
    assert!(
        data.windows(2).any(|w| w == b"Tf"),
        "generated /AP/N must carry the default-appearance font selection; data={as_str:?}"
    );
    // This fixture has no `/Tx BMC` wrapper. qpdf's ValueSetter therefore
    // keeps the source tokens and appends a generated marked-content block at
    // EOF (`QPDFFormFieldObjectHelper.cc:524-570`), rather than discarding the
    // original bytes.
    assert!(
        data.windows(16).any(|w| w == b"BT (Hello) Tj ET"),
        "qpdf's no-wrapper fallback must preserve source appearance content; data={as_str:?}"
    );
}

/// Appearance generation must use qpdf's direct widget-to-field association,
/// not the top-level projection exposed by page annotation enumeration.
#[test]
fn generate_appearances_uses_nested_terminal_field_value() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("nested-tx.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    std::fs::write(&input, nested_tx_widget_with_local_value()).unwrap();
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--generate-appearances", "--compress-streams=y"])
        .arg(&input)
        .arg(&flpdf_output)
        .assert()
        .success();

    let flpdf_data = widget_normal_appearance_data(&flpdf_output);
    assert!(
        flpdf_data
            .windows(b"child-value".len())
            .any(|w| w == b"child-value"),
        "flpdf must render the same terminal field value; data={flpdf_data:?}"
    );
    assert!(
        !flpdf_data
            .windows(b"parent-value".len())
            .any(|w| w == b"parent-value"),
        "flpdf must not render the top-level ancestor's value; data={flpdf_data:?}"
    );
}

/// qpdf does not synthesize button appearances when none exists.
#[test]
fn generate_appearances_checkbox_without_ap_leaves_ap_absent() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("cb.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, checkbox_widget_without_ap()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--generate-appearances", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let widget_ref = first_widget_ref(&mut pdf);
    let mut helper = AnnotationObjectHelper::new(widget_ref, &mut pdf);
    assert!(
        helper.get_appearance_dictionary().unwrap().is_null(),
        "qpdf leaves a button without /AP unchanged"
    );
}

#[test]
fn generate_appearances_routes_checkbox_without_ap_through_set_value() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("cb-value-no-ap.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(
        &input,
        checkbox_widget_with_value_without_ap_needing_appearances(),
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--generate-appearances", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .failure()
        .code(3)
        .stderr(predicates::str::contains(
            "unable to set the value of this checkbox",
        ));

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let widget_ref = first_widget_ref(&mut pdf);
    let widget = pdf.resolve_canonical_object(widget_ref).unwrap();
    assert_eq!(
        widget.try_get_key(b"/V").unwrap().as_name(),
        Some(b"Yes".to_vec())
    );
    assert_eq!(
        widget.try_get_key(b"/AS").unwrap().as_name(),
        Some(b"Off".to_vec())
    );
    assert!(!widget.try_has_key(b"/AP").unwrap());
}

/// qpdf does not synthesize radio appearances when none exists.
#[test]
fn generate_appearances_radio_without_ap_leaves_ap_absent() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("rd.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, radio_widget_without_ap()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--generate-appearances", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let widget_ref = first_widget_ref(&mut pdf);
    let mut helper = AnnotationObjectHelper::new(widget_ref, &mut pdf);
    assert!(
        helper.get_appearance_dictionary().unwrap().is_null(),
        "qpdf leaves a radio button without /AP unchanged"
    );
}

/// Existing checkbox appearances are preserved while `/AS` is synchronized to `/V`.
#[test]
fn generate_appearances_checkbox_with_ap_synchronizes_as_to_value() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("cb-existing-ap.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, checkbox_widget_with_ap_needing_appearances()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--generate-appearances", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let widget_ref = first_widget_ref(&mut pdf);
    let widget = pdf.resolve_canonical_object(widget_ref).unwrap();
    assert_eq!(
        widget.try_get_key(b"/AS").unwrap().as_name(),
        Some(b"Yes".to_vec())
    );
    let ap = widget.try_get_key(b"/AP").unwrap();
    let normal = ap.try_get_key(b"/N").unwrap();
    assert!(normal.try_has_key(b"/Off").unwrap() && normal.try_has_key(b"/Yes").unwrap());
}

/// A direct radio widget retains `/AS`; qpdf requires a parent field `/Kids`
/// array before it can identify and synchronize its child widget.
#[test]
fn generate_appearances_direct_radio_with_ap_leaves_as_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("rd-existing-ap.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, direct_radio_widget_with_ap_needing_appearances()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--generate-appearances", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .failure()
        .code(3)
        .stderr(predicates::str::contains(
            "don't know how to set the value of this field as a radio button",
        ));

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let widget_ref = first_widget_ref(&mut pdf);
    let widget = pdf.resolve_canonical_object(widget_ref).unwrap();
    assert_eq!(
        widget.try_get_key(b"/AS").unwrap().as_name(),
        Some(b"Off".to_vec())
    );
    let ap = widget.try_get_key(b"/AP").unwrap();
    let normal = ap.try_get_key(b"/N").unwrap();
    assert!(normal.try_has_key(b"/Off").unwrap() && normal.try_has_key(b"/Yes").unwrap());
}

/// `--generate-appearances` on a combo-box widget adds `/AP/N`, and the
/// content stream contains `Tj` (the selected option value is rendered).
#[test]
fn generate_appearances_combo_ap_n_contains_tj() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("combo.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, combo_widget_without_ap()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--generate-appearances", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let widget_ref = first_widget_ref(&mut pdf);
    let mut helper = AnnotationObjectHelper::new(widget_ref, &mut pdf);
    let n = helper.get_appearance_stream(b"N", None).unwrap();
    let data = n
        .get_stream_data(DecodeLevel::Generalized)
        .expect("combo widget should have /AP/N after --generate-appearances");
    assert!(
        data.windows(2).any(|w| w == b"Tj"),
        "combo /AP/N content stream must contain Tj (selected value rendered); \
         stream={:?}",
        std::str::from_utf8(&data).unwrap_or("<non-utf8>")
    );
}

// ── Tests: flatten=all ────────────────────────────────────────────────────────

/// `--flatten-annotations=all` bakes the widget's appearance into page content:
/// - the annotation is removed from `/Annots`
/// - the page content stream contains a `Do` operator (the XObject invocation)
#[test]
fn flatten_all_annot_removed_and_do_in_content() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("tx_ap.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_widget_with_ap()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--flatten-annotations=all",
            "--compress-streams=n",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let page_refs = flpdf::pages::page_refs(&mut pdf).unwrap();

    // Annotation must be gone from /Annots.
    let annots = page_annotation_handles(&mut pdf, page_refs[0]);
    assert!(
        annots.is_empty(),
        "flatten=all must remove widget from /Annots, found {} annotation(s)",
        annots.len()
    );

    // Page content must contain a Do operator (the flattened XObject).
    let content = flpdf::pages::page_content_bytes(&mut pdf, page_refs[0]).unwrap();
    assert!(
        content.windows(2).any(|w| w == b"Do"),
        "flatten=all must insert a Do operator into page content; \
         content={:?}",
        std::str::from_utf8(&content).unwrap_or("<non-utf8>")
    );
}

/// `--generate-appearances` + `--flatten-annotations=all` pipeline: a Tx
/// widget without an initial `/AP` gets an appearance generated, then is
/// flattened.  Both steps must leave the annotation absent from `/Annots` and
/// a `Do` in page content.
#[test]
fn generate_then_flatten_all_do_in_content() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("tx_no_ap.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_widget_without_ap_needing_appearances()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--generate-appearances",
            "--flatten-annotations=all",
            "--compress-streams=n",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let page_refs = flpdf::pages::page_refs(&mut pdf).unwrap();

    let annots = page_annotation_handles(&mut pdf, page_refs[0]);
    assert!(
        annots.is_empty(),
        "generate+flatten=all must remove widget from /Annots, found {} annotation(s)",
        annots.len()
    );

    let content = flpdf::pages::page_content_bytes(&mut pdf, page_refs[0]).unwrap();
    assert!(
        content.windows(2).any(|w| w == b"Do"),
        "generate+flatten=all must insert Do into page content; \
         content={:?}",
        std::str::from_utf8(&content).unwrap_or("<non-utf8>")
    );
}

/// qpdf clears `/NeedAppearances` after generating widget appearances, so the
/// immediately following flatten pass must not preserve those widgets.
#[test]
fn generate_then_flatten_clears_need_appearances() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("tx_need_appearances.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_widget_without_ap_needing_appearances()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--generate-appearances",
            "--flatten-annotations=all",
            "--compress-streams=n",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let page_ref = flpdf::pages::page_refs(&mut pdf).unwrap()[0];
    assert!(
        page_annotation_handles(&mut pdf, page_ref).is_empty(),
        "generated widget must be flattened after qpdf clears NeedAppearances"
    );
}

#[test]
fn generate_then_flatten_replaces_indirect_null_normal_appearance() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("tx_null_ap_n.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_widget_with_null_ap_n_needing_appearances()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--generate-appearances",
            "--flatten-annotations=all",
            "--compress-streams=n",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let page_ref = flpdf::pages::page_refs(&mut pdf).unwrap()[0];
    assert!(
        page_annotation_handles(&mut pdf, page_ref).is_empty(),
        "generated widget must be flattened after qpdf clears NeedAppearances"
    );
    let content = flpdf::pages::page_content_bytes(&mut pdf, page_ref).unwrap();
    assert!(
        content.windows(2).any(|window| window == b"Do"),
        "an indirect null /AP/N must be regenerated before flattening; content={:?}",
        std::str::from_utf8(&content).unwrap_or("<non-utf8>")
    );
}

// ── Tests: flatten=print ──────────────────────────────────────────────────────

/// `--flatten-annotations=print` draws only Print annotations, but qpdf drops
/// every annotation that has a selected appearance stream.
#[test]
fn flatten_print_draws_print_bit_annot_and_removes_all_selected_appearances() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("two_annots.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, two_annots_print_and_non_print()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--flatten-annotations=print",
            "--compress-streams=n",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let page_refs = flpdf::pages::page_refs(&mut pdf).unwrap();

    let annots = page_annotation_handles(&mut pdf, page_refs[0]);
    assert!(annots.is_empty());

    // Page content must have a Do (from the Print-bit annotation being flattened).
    let content = flpdf::pages::page_content_bytes(&mut pdf, page_refs[0]).unwrap();
    assert!(
        content.windows(2).any(|w| w == b"Do"),
        "flatten=print must insert Do for the Print-bit annotation; \
         content={:?}",
        std::str::from_utf8(&content).unwrap_or("<non-utf8>")
    );
}

// ── Tests: flatten-rotation (CLI e2e) ─────────────────────────────────────────

/// `--flatten-rotation` removes `/Rotate` from a page dictionary (e2e CLI
/// gate that complements the unit tests in the library crate).  This test
/// does not duplicate the existing `rewrite_flatten_rotation_removes_rotate`
/// in cli_tests.rs; it adds a second fixture (two-page PDF) to verify that
/// all pages are processed, not just the first.
#[test]
fn flatten_rotation_processes_all_pages() {
    // Two-page PDF: page 1 has /Rotate 90, page 2 has /Rotate 180.
    let input_bytes = assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 2 /Kids [3 0 R 5 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Rotate 90 \
          /MediaBox [0 0 200 100] /Contents 4 0 R >>\nendobj\n"
            .to_vec(),
        b"4 0 obj\n<< /Length 14 >>\nstream\nBT (p1) Tj ET\nendstream\nendobj\n".to_vec(),
        b"5 0 obj\n<< /Type /Page /Parent 2 0 R /Rotate 180 \
          /MediaBox [0 0 612 792] /Contents 6 0 R >>\nendobj\n"
            .to_vec(),
        b"6 0 obj\n<< /Length 14 >>\nstream\nBT (p2) Tj ET\nendstream\nendobj\n".to_vec(),
    ]);

    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("two_rotated.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, input_bytes).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--flatten-rotation", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let page_refs = flpdf::pages::page_refs(&mut pdf).unwrap();
    assert_eq!(page_refs.len(), 2, "output must have 2 pages");

    for (i, &page_ref) in page_refs.iter().enumerate() {
        let page_obj = pdf.resolve_canonical_object(page_ref).unwrap();
        let rotate = page_obj.try_get_key(b"/Rotate").unwrap().as_integer();
        assert!(
            rotate.is_none() || rotate == Some(0),
            "page {} /Rotate should be absent or 0 after --flatten-rotation, got {rotate:?}",
            i + 1
        );
    }
}

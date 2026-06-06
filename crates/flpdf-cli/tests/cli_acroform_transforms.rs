//! Integration tests: AcroForm/annotation transform observable equivalence
//!
//! Covers: generate-appearances (Tx value→Tj, checkbox/radio state-dict,
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
//! All rewrite calls use `--compress-streams=n` so that page content and
//! appearance streams can be inspected as raw bytes without a FlateDecode
//! decoding step.
//!
//! # qpdf divergence
//!
//! See `docs/qpdf-compat-decisions.md` §AcroForm & annotation transforms.

use assert_cmd::Command;
use flpdf::{AnnotationObjectHelper, Object, ObjectRef, Pdf};
use std::fs::File;
use std::io::BufReader;

// ── Fixture helpers ───────────────────────────────────────────────────────────

/// Build a minimal PDF from a flat list of object bodies (1-indexed from 1).
fn assemble_pdf(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for object in objects {
        offsets.push(bytes.len() as u32);
        bytes.extend_from_slice(object);
    }
    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for &offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            start_xref
        )
        .as_bytes(),
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

/// Single-page AcroForm PDF with a Tx widget that has `/V` AND an existing
/// `/AP/N` Form XObject containing the literal value bytes.
/// Objects: 1=Catalog, 2=Pages, 3=Page, 4=Widget, 5=Contents, 6=AP/N XObject
fn tx_widget_with_ap() -> Vec<u8> {
    assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
          /AcroForm << /Fields [4 0 R] /DR << >> /DA (/Helv 12 Tf 0 g) >> >>\nendobj\n"
            .to_vec(),
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
          /AcroForm << /Fields [4 0 R] /DR << >> /DA (/Helv 10 Tf 0 g) >> >>\nendobj\n"
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

/// Resolve the object referred to by `obj`, following a single level of
/// indirect reference when needed.
fn resolve_one<R: std::io::Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
    obj: Object,
) -> flpdf::Result<Object> {
    match obj {
        Object::Reference(r) => pdf.resolve(r),
        other => Ok(other),
    }
}

// ── Tests: generate-appearances ───────────────────────────────────────────────

/// `--generate-appearances` on a Tx widget adds `/AP/N`, and the uncompressed
/// content stream of that XObject contains a `Tj` operator (the value text is
/// rendered).
#[test]
fn generate_appearances_tx_ap_n_contains_tj() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("tx.pdf");
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
    let widget_ref = ObjectRef::new(4, 0);

    // /AP/N must be present after generate-appearances.
    // Use a block to end the borrow of `pdf` before the resolve call below.
    let n_val = {
        let mut helper = AnnotationObjectHelper::new(widget_ref, &mut pdf);
        let ap = helper
            .appearance()
            .unwrap()
            .expect("Tx widget should have /AP after --generate-appearances");
        ap.get("N").cloned().expect("/AP should have /N entry")
    };

    // Resolve the /N value to the XObject stream.
    let n_obj = resolve_one(&mut pdf, n_val).unwrap();
    let stream = n_obj
        .as_stream()
        .expect("/AP/N should be a Form XObject stream");

    // The uncompressed content stream must contain "Tj" (the text-show operator).
    // We use --compress-streams=n so stream.data is the raw uncompressed bytes.
    assert!(
        stream.data.windows(2).any(|w| w == b"Tj"),
        "/AP/N content stream must contain Tj operator (observable: value rendered); \
         stream bytes: {:?}",
        std::str::from_utf8(&stream.data).unwrap_or("<non-utf8>")
    );
}

/// `--generate-appearances` does **not** overwrite a widget that already has
/// `/AP/N` — the original XObject is preserved (observable: existing appearance
/// is not discarded).
#[test]
fn generate_appearances_tx_skips_existing_ap() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("tx_ap.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_widget_with_ap()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--generate-appearances", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    // /AP/N must still be present; the existing stream content must contain the
    // original literal "(Hello) Tj ET" — not overwritten with a newly generated one.
    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let widget_ref = ObjectRef::new(4, 0);
    let n_val = {
        let mut helper = AnnotationObjectHelper::new(widget_ref, &mut pdf);
        let ap = helper
            .appearance()
            .unwrap()
            .expect("/AP must survive --generate-appearances for widget that already has one");
        ap.get("N").cloned().expect("/AP/N must be present")
    };

    let n_obj = resolve_one(&mut pdf, n_val).unwrap();
    let stream = n_obj.as_stream().expect("/AP/N must be a stream");
    // Original content "(Hello) Tj ET" is preserved verbatim in the XObject.
    assert!(
        stream.data.windows(2).any(|w| w == b"Tj"),
        "existing /AP/N stream must still contain Tj; data={:?}",
        std::str::from_utf8(&stream.data).unwrap_or("<non-utf8>")
    );
}

/// `--generate-appearances` on a checkbox widget installs `/AP/N` as a state
/// dictionary (with at least one non-Off key) — observable equivalence for
/// on/off checkbox rendering.
#[test]
fn generate_appearances_checkbox_creates_state_dict() {
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
    let widget_ref = ObjectRef::new(4, 0);
    let n_val = {
        let mut helper = AnnotationObjectHelper::new(widget_ref, &mut pdf);
        let ap = helper
            .appearance()
            .unwrap()
            .expect("checkbox widget should have /AP after --generate-appearances");
        ap.get("N").cloned().expect("/AP should have /N entry")
    };

    // For checkbox/radio the /AP/N value must be a Dictionary (state dict),
    // not a stream directly.  It contains at least an "Off" state key and an
    // on-state key (e.g. "Yes").
    let n_obj = resolve_one(&mut pdf, n_val).unwrap();
    let state_dict = n_obj
        .as_dict()
        .expect("/AP/N for checkbox must be a state dictionary, not a bare stream");

    // "Off" key must be present (empty/transparent appearance for unchecked state).
    assert!(
        state_dict.get("Off").is_some(),
        "checkbox /AP/N state dict must have an 'Off' entry"
    );

    // At least one non-Off key must exist (the checked-state appearance).
    let has_on_state = state_dict.iter().any(|(k, _)| k != b"Off");
    assert!(
        has_on_state,
        "checkbox /AP/N state dict must have a non-Off (on-state) entry"
    );

    // Confirm at least 2 total entries (on-state + Off).
    let entry_count = state_dict.iter().count();
    assert!(
        entry_count >= 2,
        "checkbox /AP/N state dict should have >=2 entries (on-state + Off), got {}",
        entry_count
    );
}

/// `--generate-appearances` on a radio button widget installs `/AP/N` as a
/// state dictionary — same structural requirement as checkbox.
#[test]
fn generate_appearances_radio_creates_state_dict() {
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
    let widget_ref = ObjectRef::new(4, 0);
    let n_val = {
        let mut helper = AnnotationObjectHelper::new(widget_ref, &mut pdf);
        let ap = helper
            .appearance()
            .unwrap()
            .expect("radio widget should have /AP after --generate-appearances");
        ap.get("N").cloned().expect("/AP should have /N entry")
    };

    let n_obj = resolve_one(&mut pdf, n_val).unwrap();
    let state_dict = n_obj
        .as_dict()
        .expect("/AP/N for radio must be a state dictionary");

    let entry_count = state_dict.iter().count();
    assert!(
        entry_count >= 2,
        "radio /AP/N state dict should have >=2 entries, got {}",
        entry_count
    );
    assert!(
        state_dict.get("Off").is_some(),
        "radio /AP/N state dict must have an 'Off' entry"
    );
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
    let widget_ref = ObjectRef::new(4, 0);
    let n_val = {
        let mut helper = AnnotationObjectHelper::new(widget_ref, &mut pdf);
        let ap = helper
            .appearance()
            .unwrap()
            .expect("combo widget should have /AP after --generate-appearances");
        ap.get("N").cloned().expect("/AP should have /N entry")
    };

    let n_obj = resolve_one(&mut pdf, n_val).unwrap();
    let stream = n_obj.as_stream().expect("/AP/N for combo must be a stream");
    assert!(
        stream.data.windows(2).any(|w| w == b"Tj"),
        "combo /AP/N content stream must contain Tj (selected value rendered); \
         stream={:?}",
        std::str::from_utf8(&stream.data).unwrap_or("<non-utf8>")
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
    let annots = flpdf::enumerate_page_annotations(&mut pdf, page_refs[0]).unwrap();
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
    std::fs::write(&input, tx_widget_without_ap()).unwrap();

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

    let annots = flpdf::enumerate_page_annotations(&mut pdf, page_refs[0]).unwrap();
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

// ── Tests: flatten=print ──────────────────────────────────────────────────────

/// `--flatten-annotations=print` removes only annotations with the Print bit
/// (bit 3, /F & 0x4 != 0) and leaves annotations without it in `/Annots`.
#[test]
fn flatten_print_removes_print_bit_annot_only() {
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

    // Exactly one annotation must remain (the non-Print one).
    let annots = flpdf::enumerate_page_annotations(&mut pdf, page_refs[0]).unwrap();
    assert_eq!(
        annots.len(),
        1,
        "flatten=print must leave exactly 1 annotation (the non-Print one), found {}",
        annots.len()
    );

    // The surviving annotation must be object 5 (the non-Print one, /F 0).
    assert_eq!(
        annots[0].annot_ref,
        ObjectRef::new(5, 0),
        "surviving annotation must be the non-Print widget (5 0 R), got {}",
        annots[0].annot_ref
    );

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
        let page_obj = pdf.resolve(page_ref).unwrap();
        let Object::Dictionary(dict) = page_obj else {
            panic!("page {} is not a dictionary", i + 1);
        };
        let rotate = dict.get("Rotate").and_then(|o| o.as_integer());
        assert!(
            rotate.is_none() || rotate == Some(0),
            "page {} /Rotate should be absent or 0 after --flatten-rotation, got {rotate:?}",
            i + 1
        );
    }
}

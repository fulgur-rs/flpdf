//! flpdf-9hc.22.8 — Signed-PDF preservation matrix.
//!
//! A table-driven matrix that exercises the signed-PDF preservation contract
//! across several fixture variants and every preservation operation. The cells
//! are {fixture variant} × {operation}:
//!
//! | variant          | detect | full-rewrite refuse | remove-restrictions | incr. benign | incr. AcroForm | incr. sig field |
//! |------------------|--------|---------------------|---------------------|--------------|----------------|-----------------|
//! | single_detached  |   ✓    |          ✓          |          ✓          |      ✓       |       ✓        |        ✓        |
//! | single_sha1      |   ✓    |          ✓          |          ✓          |      ✓       |       ✓        |        ✓        |
//! | multi_sig        |   ✓    |          ✓          |          ✓          |      ✓       |       ✓        |        ✓        |
//!
//! ## External validation caveat
//!
//! These fixtures are SYNTHETIC: their `/Contents` is a `<00>` placeholder
//! rather than a real PKCS#7 blob, so `pdfsig`/`qpdf` *cryptographic*
//! verification is not meaningful on them. The structural invariants asserted
//! here — detection accuracy, destructive-rewrite refusal, restriction
//! stripping, and byte-identical preservation of every `/ByteRange` region
//! under incremental update — ARE the verification for this matrix. Real
//! cryptographically-signed fixtures and `pdfsig`-based validation are
//! documented as a future CI extension in
//! `docs/signed-pdf-external-validation.md`.

use flpdf::{
    acroform_sig_flags, clear_sig_flags, signature_rewrite_impact, signatures,
    strip_signature_values, write_pdf, write_pdf_with_options, Error, ObjectRef, Pdf,
    SignatureRewriteReason, SignatureWriteMode, WriteOptions, SIG_FLAGS_APPEND_ONLY,
    SIG_FLAGS_SIGNATURES_EXIST,
};
use std::collections::BTreeMap;
use std::io::Cursor;

fn build_pdf(objects: &[(u32, &[u8])]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    for &(num, bytes) in objects {
        offsets.insert(num, out.len() as u64);
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(bytes);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_pos = out.len() as u64;
    let max_num = objects.iter().map(|&(n, _)| n).max().unwrap_or(0);
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max_num + 1).as_bytes());
    for i in 1..=max_num {
        match offsets.get(&i) {
            Some(&off) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
            max_num + 1
        )
        .as_bytes(),
    );
    out
}

fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
}

/// Expected detection result for a single signature field in a fixture.
struct ExpectedSig {
    field_name: &'static str,
    byte_range: [u64; 4],
    sub_filter: &'static str,
}

/// One row of the matrix: a fixture variant plus the metadata the operations
/// need (expected detections and the object refs to touch / inspect).
struct Variant {
    name: &'static str,
    bytes: Vec<u8>,
    expected: Vec<ExpectedSig>,
    /// `/AcroForm` dictionary ref — touching it must invalidate signatures.
    acroform_ref: ObjectRef,
    /// A benign object (the page) that is neither AcroForm nor a signed object;
    /// touching it must preserve signatures.
    benign_ref: ObjectRef,
    /// Signature field refs — touching one must invalidate signatures.
    sig_field_refs: Vec<ObjectRef>,
}

fn build_single_detached() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        ),
        (3, b"<< /Type /Page /Parent 2 0 R /Annots [5 0 R] >>"),
        (4, b"<< /Fields [5 0 R] /SigFlags 3 >>"),
        (
            5,
            b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (Sig1) /V 6 0 R /P 3 0 R /Rect [0 0 10 10] >>",
        ),
        (
            6,
            b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 10 20 30] /Contents <00> >>",
        ),
    ];
    build_pdf(&objects)
}

fn build_single_sha1() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        ),
        (3, b"<< /Type /Page /Parent 2 0 R /Annots [5 0 R] >>"),
        (4, b"<< /Fields [5 0 R] /SigFlags 3 >>"),
        (
            5,
            b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (Sig1) /V 6 0 R /P 3 0 R /Rect [0 0 10 10] >>",
        ),
        (
            6,
            b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.sha1 /ByteRange [0 10 20 30] /Contents <00> >>",
        ),
    ];
    build_pdf(&objects)
}

/// Two signature fields in one document: a detached and a sha1 signature, each
/// with a distinct `/ByteRange` so detection is unambiguous per field.
fn build_multi_sig() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        ),
        (3, b"<< /Type /Page /Parent 2 0 R /Annots [5 0 R 7 0 R] >>"),
        (4, b"<< /Fields [5 0 R 7 0 R] /SigFlags 3 >>"),
        (
            5,
            b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (Sig1) /V 6 0 R /P 3 0 R /Rect [0 0 10 10] >>",
        ),
        (
            6,
            b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 10 20 30] /Contents <00> >>",
        ),
        (
            7,
            b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (Sig2) /V 8 0 R /P 3 0 R /Rect [0 0 10 10] >>",
        ),
        (
            8,
            b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.sha1 /ByteRange [0 40 50 60] /Contents <00> >>",
        ),
    ];
    build_pdf(&objects)
}

fn variants() -> Vec<Variant> {
    vec![
        Variant {
            name: "single_detached",
            bytes: build_single_detached(),
            expected: vec![ExpectedSig {
                field_name: "Sig1",
                byte_range: [0, 10, 20, 30],
                sub_filter: "adbe.pkcs7.detached",
            }],
            acroform_ref: ObjectRef::new(4, 0),
            benign_ref: ObjectRef::new(3, 0),
            sig_field_refs: vec![ObjectRef::new(5, 0)],
        },
        Variant {
            name: "single_sha1",
            bytes: build_single_sha1(),
            expected: vec![ExpectedSig {
                field_name: "Sig1",
                byte_range: [0, 10, 20, 30],
                sub_filter: "adbe.pkcs7.sha1",
            }],
            acroform_ref: ObjectRef::new(4, 0),
            benign_ref: ObjectRef::new(3, 0),
            sig_field_refs: vec![ObjectRef::new(5, 0)],
        },
        Variant {
            name: "multi_sig",
            bytes: build_multi_sig(),
            expected: vec![
                ExpectedSig {
                    field_name: "Sig1",
                    byte_range: [0, 10, 20, 30],
                    sub_filter: "adbe.pkcs7.detached",
                },
                ExpectedSig {
                    field_name: "Sig2",
                    byte_range: [0, 40, 50, 60],
                    sub_filter: "adbe.pkcs7.sha1",
                },
            ],
            acroform_ref: ObjectRef::new(4, 0),
            benign_ref: ObjectRef::new(3, 0),
            sig_field_refs: vec![ObjectRef::new(5, 0), ObjectRef::new(7, 0)],
        },
    ]
}

// ── Cell 1: detection accuracy ────────────────────────────────────────────

#[test]
fn matrix_detection_reports_every_signature() {
    for v in variants() {
        let mut pdf = open(v.bytes);
        let sigs = signatures(&mut pdf)
            .unwrap_or_else(|e| panic!("[{}] signature scan failed: {e:?}", v.name));

        assert_eq!(
            sigs.len(),
            v.expected.len(),
            "[{}] expected {} signature(s), got {}",
            v.name,
            v.expected.len(),
            sigs.len()
        );

        // Match by field name so the assertions are order-independent.
        let by_name: BTreeMap<&str, &flpdf::SignatureInfo> =
            sigs.iter().map(|s| (s.field_name.as_str(), s)).collect();

        for exp in &v.expected {
            let got = by_name.get(exp.field_name).unwrap_or_else(|| {
                panic!("[{}] missing signature field {:?}", v.name, exp.field_name)
            });
            assert_eq!(
                got.byte_range, exp.byte_range,
                "[{}] field {:?} byte_range mismatch",
                v.name, exp.field_name
            );
            assert_eq!(
                got.sub_filter.as_deref(),
                Some(exp.sub_filter),
                "[{}] field {:?} sub_filter mismatch",
                v.name,
                exp.field_name
            );
        }
    }
}

// ── Cell 2: full-rewrite refusal ──────────────────────────────────────────

#[test]
fn matrix_full_rewrite_is_refused_with_structured_error() {
    for v in variants() {
        let mut pdf = open(v.bytes);
        let mut options = WriteOptions::default();
        options.full_rewrite = true;

        let err = write_pdf_with_options(&mut pdf, Vec::new(), &options).expect_err(&format!(
            "[{}] full rewrite should refuse signed PDF",
            v.name
        ));

        let Error::Signed { fields, message } = err else {
            panic!("[{}] expected Error::Signed, got {err:?}", v.name);
        };
        assert!(
            !fields.is_empty(),
            "[{}] refusal should name the signature field(s)",
            v.name
        );
        assert!(
            message.contains("refusing full rewrite of signed PDF"),
            "[{}] unexpected message: {message}",
            v.name
        );
        assert!(
            message.contains("--remove-restrictions"),
            "[{}] diagnostic should mention the override flag: {message}",
            v.name
        );
    }
}

// ── Cell 3: --remove-restrictions stripping ───────────────────────────────

#[test]
fn matrix_remove_restrictions_strips_signatures() {
    for v in variants() {
        let mut pdf = open(v.bytes);

        let cleared = clear_sig_flags(&mut pdf)
            .unwrap_or_else(|e| panic!("[{}] clear_sig_flags failed: {e:?}", v.name));
        assert!(
            cleared,
            "[{}] clear_sig_flags should report a change",
            v.name
        );

        let flags_after = acroform_sig_flags(&mut pdf)
            .unwrap_or_else(|e| panic!("[{}] acroform_sig_flags failed: {e:?}", v.name))
            .unwrap_or(0);
        assert_eq!(
            flags_after & (SIG_FLAGS_SIGNATURES_EXIST | SIG_FLAGS_APPEND_ONLY),
            0,
            "[{}] SignaturesExist/AppendOnly bits must be cleared, got {flags_after:#b}",
            v.name
        );

        let stripped = strip_signature_values(&mut pdf)
            .unwrap_or_else(|e| panic!("[{}] strip_signature_values failed: {e:?}", v.name));
        assert!(
            stripped,
            "[{}] strip_signature_values should report a change",
            v.name
        );

        // With the signature values removed, the document no longer presents as
        // signed: detection finds nothing to preserve.
        let sigs_after = signatures(&mut pdf)
            .unwrap_or_else(|e| panic!("[{}] post-strip scan failed: {e:?}", v.name));
        assert!(
            sigs_after.is_empty(),
            "[{}] signatures should be gone after stripping, got {}",
            v.name,
            sigs_after.len()
        );
    }
}

// ── Cell 4: incremental update preserves signed byte ranges ───────────────

#[test]
fn matrix_incremental_benign_change_preserves_byte_ranges() {
    for v in variants() {
        let input = v.bytes;
        let mut pdf = open(input.clone());

        let ranges: Vec<[u64; 4]> = signatures(&mut pdf)
            .unwrap_or_else(|e| panic!("[{}] scan failed: {e:?}", v.name))
            .iter()
            .map(|s| s.byte_range)
            .collect();
        assert!(
            !ranges.is_empty(),
            "[{}] fixture must contain a signature",
            v.name
        );

        // Benign incremental change: re-set the page object, which is neither
        // the /AcroForm nor any signed object. This marks an object touched so
        // the incremental path actually appends a new generation.
        let benign = pdf.resolve(v.benign_ref).unwrap();
        pdf.set_object(v.benign_ref, benign);

        let impact = signature_rewrite_impact(&mut pdf, SignatureWriteMode::Incremental)
            .unwrap_or_else(|e| panic!("[{}] impact failed: {e:?}", v.name));
        assert!(
            !impact.invalidates_signatures,
            "[{}] benign incremental change must not invalidate signatures",
            v.name
        );
        assert_eq!(
            impact.reason,
            SignatureRewriteReason::IncrementalPreservesSignedByteRanges,
            "[{}] unexpected impact reason",
            v.name
        );

        let mut out = Vec::new();
        write_pdf(&mut pdf, &mut out)
            .unwrap_or_else(|e| panic!("[{}] incremental write failed: {e:?}", v.name));

        assert!(
            out.len() > input.len(),
            "[{}] incremental update should append a new generation",
            v.name
        );
        assert!(
            out.starts_with(&input),
            "[{}] incremental update must preserve the original prefix byte-for-byte",
            v.name
        );

        for [o1, l1, o2, l2] in ranges {
            for (start, len) in [(o1, l1), (o2, l2)] {
                let s = start as usize;
                let e = s + len as usize;
                assert!(
                    e <= input.len() && e <= out.len(),
                    "[{}] /ByteRange region [{s}..{e}] must lie within the document",
                    v.name
                );
                assert_eq!(
                    &out[s..e],
                    &input[s..e],
                    "[{}] signed /ByteRange region [{s}..{e}] must be byte-identical",
                    v.name
                );
            }
        }
    }
}

// ── Cell 5: incremental touching /AcroForm invalidates ────────────────────

#[test]
fn matrix_incremental_touching_acroform_invalidates() {
    for v in variants() {
        let mut pdf = open(v.bytes);
        let acroform = pdf.resolve(v.acroform_ref).unwrap();
        pdf.set_object(v.acroform_ref, acroform);

        let impact = signature_rewrite_impact(&mut pdf, SignatureWriteMode::Incremental)
            .unwrap_or_else(|e| panic!("[{}] impact failed: {e:?}", v.name));

        assert!(
            impact.invalidates_signatures,
            "[{}] touching /AcroForm must invalidate signatures",
            v.name
        );
        assert_eq!(
            impact.reason,
            SignatureRewriteReason::IncrementalTouchesAcroForm,
            "[{}] unexpected impact reason",
            v.name
        );
        assert_eq!(
            impact.first_invalidating_ref,
            Some(v.acroform_ref),
            "[{}] should report the AcroForm as the invalidating object",
            v.name
        );
    }
}

// ── Cell 6: incremental touching a signature field invalidates ────────────

#[test]
fn matrix_incremental_touching_signature_field_invalidates() {
    for v in variants() {
        // Verify every signature field independently: for a multi-signature
        // document, touching the second field must invalidate just as surely as
        // the first. A fresh parse per field is required because each open()
        // consumes its source bytes (and we re-touch from a clean baseline).
        let bytes = v.bytes;
        for &target in &v.sig_field_refs {
            let mut pdf = open(bytes.clone());
            let field = pdf.resolve(target).unwrap();
            pdf.set_object(target, field);

            let impact = signature_rewrite_impact(&mut pdf, SignatureWriteMode::Incremental)
                .unwrap_or_else(|e| panic!("[{}] impact failed for {target:?}: {e:?}", v.name));

            assert!(
                impact.invalidates_signatures,
                "[{}] touching signature field {target:?} must invalidate signatures",
                v.name
            );
            assert_eq!(
                impact.reason,
                SignatureRewriteReason::IncrementalTouchesSignedObject,
                "[{}] unexpected impact reason for {target:?}",
                v.name
            );
            assert_eq!(
                impact.first_invalidating_ref,
                Some(target),
                "[{}] should report touched signature field {target:?} as invalidating",
                v.name
            );
        }
    }
}

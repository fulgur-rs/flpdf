//! Orchestrator for qpdf v11.9.0's `compare(actual_filename, expected_filename,
//! password)` from `compare-for-test/qpdf-test-compare.cc`.
//!
//! Loads both files with [`Pdf::open_mem_owned_with_options`], cleans the
//! trailer (mask `/Length` and `/ID`), compares the trailers, cleans the
//! encryption dict (strip `/O /OE /U /UE /Perms`), then walks the live
//! object refs in ascending `(number, generation)` order and delegates each
//! pair to [`compare_objects`].

use flpdf::{Object, Pdf, PdfOpenOptions};

use crate::clean::{clean_encryption, clean_trailer};
use crate::compare::compare_objects;

/// Compare two in-memory PDFs the way qpdf's `qpdf-test-compare` compares
/// two on-disk files.
///
/// Returns `Ok(None)` when the two documents are equivalent under the
/// qpdf-test-compare rules (trailer `/Length` and `/ID` halves masked,
/// encryption hashes stripped, streams with `/Type /XRef` skipped, streams
/// with `/FlateDecode` compared by decoded payload). Returns
/// `Ok(Some(reason))` when they differ; `reason` is one of qpdf's fixed
/// diff strings (`"trailer: ..."`, `"different number of objects"`,
/// `"different object IDs"`, or `"<N G>: ..."`).
///
/// `password` is forwarded to both opens; supply `b""` for unencrypted
/// input.
///
/// # Errors
///
/// Propagates any [`flpdf::Error`] from parsing either input (invalid PDF
/// structure, wrong password, corrupt object stream, etc.) or from
/// resolving individual objects during the per-object walk.
pub fn compare_files(
    actual_bytes: &[u8],
    expected_bytes: &[u8],
    password: &[u8],
) -> flpdf::Result<Option<String>> {
    let open_options = || PdfOpenOptions {
        password: password.to_vec(),
        // qpdf's processFile accepts RC4-backed handlers by default. Mirror
        // that so encrypted fixtures don't require a feature flag.
        allow_weak_crypto: true,
        ..PdfOpenOptions::default()
    };
    // `open_mem_owned_with_options` takes an owned `Vec<u8>`; the `.to_vec()`
    // allocation is required by the API and cannot be avoided from an
    // `&[u8]` caller.
    let mut actual = Pdf::open_mem_owned_with_options(actual_bytes.to_vec(), open_options())?;
    let mut expected = Pdf::open_mem_owned_with_options(expected_bytes.to_vec(), open_options())?;

    // Trailer compare: clone → clean → serialize-compare via `compare_objects`.
    // Cloning the trailer is unavoidable — `trailer()` returns `&Dictionary`
    // and `clean_trailer` needs `&mut Dictionary` — but the trailer is small.
    let mut act_trailer = actual.trailer().clone();
    let mut exp_trailer = expected.trailer().clone();
    clean_trailer(&mut act_trailer);
    clean_trailer(&mut exp_trailer);
    let trailer_diff = compare_objects(
        "trailer",
        &Object::Dictionary(act_trailer),
        &Object::Dictionary(exp_trailer),
    );
    if !trailer_diff.is_empty() {
        return Ok(Some(trailer_diff));
    }

    clean_encryption(&mut actual)?;
    clean_encryption(&mut expected)?;

    let a_refs = actual.live_object_refs();
    let e_refs = expected.live_object_refs();
    if a_refs.len() != e_refs.len() {
        return Ok(Some("different number of objects".to_string()));
    }
    for (a_ref, e_ref) in a_refs.iter().zip(e_refs.iter()) {
        if a_ref != e_ref {
            return Ok(Some("different object IDs".to_string()));
        }
        // `resolve` returns an owned Object; do NOT clone — pass by `&`.
        let a_obj = actual.resolve(*a_ref)?;
        let e_obj = expected.resolve(*e_ref)?;
        // qpdf's `QPDFObjGen::unparse()` emits "N G" (no trailing R);
        // `ObjectRef::Display` emits "N G R". Format explicitly to mirror
        // qpdf so per-object labels match the oracle byte-for-byte.
        let label = format!("{} {}", a_ref.number, a_ref.generation);
        let diff = compare_objects(&label, &a_obj, &e_obj);
        if !diff.is_empty() {
            return Ok(Some(diff));
        }
    }
    Ok(None)
}

//! Orchestrator for qpdf v11.9.0's `compare(actual_filename, expected_filename,
//! password)` from `compare-for-test/qpdf-test-compare.cc`.
//!
//! Loads both files with [`Pdf::open_mem_owned_with_options`], cleans the
//! trailer (mask `/Length` and `/ID`), compares the trailers, cleans the
//! encryption dict (strip `/O /OE /U /UE /Perms`), then walks the live
//! object refs in ascending `(number, generation)` order and delegates each
//! pair to [`compare_objects`].
//!
//! Oracle: qpdf 11.9.0 `compare-for-test/qpdf-test-compare.cc:148-181`.

use flpdf::{Pdf, PdfOpenOptions};

use crate::clean::{clean_encryption_handle, clean_trailer_handle};
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
/// structure, wrong password, corrupt object stream, etc.), from resolving
/// individual objects during the per-object walk, or from decoding a
/// `/FlateDecode` payload in the stream branch. Decode failures reach the
/// same exit-2-with-stderr-only path as parse failures — mirroring qpdf's
/// oracle, whose `getStreamData()` exception is caught by `main()` and
/// printed as an error with no stdout dump.
pub fn compare_files(
    actual_bytes: &[u8],
    expected_bytes: &[u8],
    password: &[u8],
) -> flpdf::Result<Option<String>> {
    let open_options = || PdfOpenOptions {
        password: password.to_vec(),
        ..PdfOpenOptions::default()
    };
    // `open_mem_owned_with_options` takes an owned `Vec<u8>`; the `.to_vec()`
    // allocation is required by the API and cannot be avoided from an
    // `&[u8]` caller.
    let mut actual = Pdf::open_mem_owned_with_options(actual_bytes.to_vec(), open_options())?;
    let mut expected = Pdf::open_mem_owned_with_options(expected_bytes.to_vec(), open_options())?;

    // qpdf's getTrailer() returns a live ObjectHandle. Build that canonical
    // view before cleaning so trailer masking and the later object walk share
    // the same identity graph rather than cloning a legacy Dictionary.
    let act_trailer = actual.trailer_handle();
    let exp_trailer = expected.trailer_handle();
    clean_trailer_handle(&mut actual, &act_trailer)?;
    clean_trailer_handle(&mut expected, &exp_trailer)?;
    let trailer_diff = compare_objects(
        "trailer",
        &act_trailer,
        &exp_trailer,
        &mut actual,
        &mut expected,
    )?;
    if !trailer_diff.is_empty() {
        return Ok(Some(trailer_diff));
    }

    clean_encryption_handle(&mut actual, &act_trailer)?;
    clean_encryption_handle(&mut expected, &exp_trailer)?;

    let a_refs = actual.live_object_refs();
    let e_refs = expected.live_object_refs();
    if a_refs.len() != e_refs.len() {
        return Ok(Some("different number of objects".to_string()));
    }
    for (a_ref, e_ref) in a_refs.iter().zip(e_refs.iter()) {
        if a_ref != e_ref {
            return Ok(Some("different object IDs".to_string()));
        }
        // qpdf's getAllObjects() returns canonical handles. Do not materialize
        // an Object snapshot here; compare_objects resolves only the current
        // top-level handle and keeps nested references indirect.
        let a_obj = actual.get_object_handle(*a_ref);
        let e_obj = expected.get_object_handle(*e_ref);
        // qpdf's `QPDFObjGen::unparse()` emits "N G" (no trailing R);
        // `ObjectRef::Display` emits "N G R". Format explicitly to mirror
        // qpdf so per-object labels match the oracle byte-for-byte.
        let label = format!("{} {}", a_ref.number, a_ref.generation);
        let diff = compare_objects(&label, &a_obj, &e_obj, &mut actual, &mut expected)?;
        if !diff.is_empty() {
            return Ok(Some(diff));
        }
    }
    Ok(None)
}

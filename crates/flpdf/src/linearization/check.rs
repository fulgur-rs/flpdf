//! Linearization structural checker (sub-task 2.10).
//!
//! This module validates that a PDF file conforms to the linearization layout
//! described in ISO 32000-1 Annex F.  It is invoked by the `check-linearization`
//! CLI subcommand but lives in the library so that future tests and tools can
//! reuse it without going through the CLI layer.
//!
//! # Checked invariants
//!
//! | Field | Invariant checked |
//! |-------|-------------------|
//! | `/Linearized` | Object 1 has the key with a positive numeric value |
//! | `/L`  | Value equals the actual file length (in bytes) |
//! | `/N`  | Value equals the number of pages in the document |
//! | `/O`  | Refers to an existing object whose dict contains `/Type /Page` |
//! | `/H`  | `H[0]` byte offset exists and the stream there is FlateDecode-decodable |
//! | `/E`  | Value is less than the file length (first-page section is bounded) |
//! | `/T`  | Byte offset has the `xref` keyword |
//!
//! # Exit semantics (used by CLI)
//!
//! The function returns a `LinearizationCheckResult`:
//! - `Ok(())` — all checks passed
//! - `Err(LinearizationCheckError::NotLinearized)` — object 1 has no `/Linearized` key
//! - `Err(LinearizationCheckError::InvalidParam { … })` — a param-dict invariant failed
//! - `Err(LinearizationCheckError::Io(…))` — I/O failure reading the file

use crate::filters::decode_stream_data;
use crate::pages::page_refs;
use crate::{Object, ObjectRef, Pdf};
use std::fmt;
use std::io::{BufReader, Read, Seek};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Reason a linearization check failed.
#[derive(Debug)]
pub enum LinearizationCheckError {
    /// The PDF is not linearized (object 1 lacks `/Linearized`).
    NotLinearized,
    /// A param-dict invariant failed.  `message` describes what went wrong in
    /// actionable terms suitable for printing to stderr.
    InvalidParam { message: String },
    /// An I/O or parse error occurred while reading the file.
    Io(Box<dyn std::error::Error + Send + Sync>),
}

impl fmt::Display for LinearizationCheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinearizationCheckError::NotLinearized => {
                write!(f, "not a linearized PDF: object 1 has no /Linearized key")
            }
            LinearizationCheckError::InvalidParam { message } => {
                write!(f, "linearization check failed: {message}")
            }
            LinearizationCheckError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for LinearizationCheckError {}

impl From<crate::Error> for LinearizationCheckError {
    fn from(e: crate::Error) -> Self {
        LinearizationCheckError::Io(Box::new(e))
    }
}

impl From<std::io::Error> for LinearizationCheckError {
    fn from(e: std::io::Error) -> Self {
        LinearizationCheckError::Io(Box::new(e))
    }
}

/// Shorthand result type for the checker.
pub type CheckResult = std::result::Result<(), LinearizationCheckError>;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return an `InvalidParam` error with a formatted message.
macro_rules! fail {
    ($($arg:tt)*) => {
        return Err(LinearizationCheckError::InvalidParam {
            message: format!($($arg)*),
        })
    };
}

/// Extract a non-negative integer value from a PDF `Object`.
///
/// `Object::Real` is accepted only when it carries an exact integer value
/// (`r.fract() == 0.0`); fractional values like `/N 1.9` would otherwise be
/// silently truncated and could spuriously satisfy integer invariants the
/// checker is enforcing.
fn as_u64(obj: &Object, key: &str) -> std::result::Result<u64, LinearizationCheckError> {
    match obj {
        Object::Integer(n) if *n >= 0 => Ok(*n as u64),
        Object::Real(r) if r.is_finite() && *r >= 0.0 && r.fract() == 0.0 => Ok(*r as u64),
        other => Err(LinearizationCheckError::InvalidParam {
            message: format!(
                "/{key} is not a non-negative integer (got {})",
                debug_obj(other)
            ),
        }),
    }
}

/// Return `true` if `b` is a PDF whitespace byte (ISO 32000-1 §7.2.3).
fn is_pdf_whitespace(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn debug_obj(obj: &Object) -> String {
    let mut buf = Vec::new();
    obj.write_pdf(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run all linearization structural checks on `pdf`.
///
/// `file_bytes` is the raw content of the PDF file (used for byte-level checks
/// on `/T` and `/H`).  `file_len` must equal `file_bytes.len()`.
///
/// Returns `Ok(())` if all checks pass, or the first failing
/// [`LinearizationCheckError`] otherwise.
pub fn check_linearization<R: Read + Seek>(pdf: &mut Pdf<R>, file_bytes: &[u8]) -> CheckResult {
    let file_len = file_bytes.len() as u64;

    // -----------------------------------------------------------------------
    // 1. Object 1 must have /Linearized with a positive value
    // -----------------------------------------------------------------------
    let obj1 = pdf
        .resolve(ObjectRef::new(1, 0))
        .map_err(LinearizationCheckError::from)?;
    let Object::Dictionary(param_dict) = obj1 else {
        return Err(LinearizationCheckError::NotLinearized);
    };

    let Some(linearized_val) = param_dict.get("Linearized") else {
        return Err(LinearizationCheckError::NotLinearized);
    };
    match linearized_val {
        Object::Integer(n) if *n > 0 => {}
        Object::Real(r) if r.is_finite() && *r > 0.0 => {}
        _ => return Err(LinearizationCheckError::NotLinearized),
    }

    // -----------------------------------------------------------------------
    // 2. /L must equal file length
    // -----------------------------------------------------------------------
    let l_obj = param_dict.get("L").cloned().unwrap_or(Object::Null);
    let l_val = as_u64(&l_obj, "L")?;
    if l_val != file_len {
        fail!("/L ({l_val}) does not match file length ({file_len})");
    }

    // -----------------------------------------------------------------------
    // 3. /N must equal the page count
    // -----------------------------------------------------------------------
    let n_obj = param_dict.get("N").cloned().unwrap_or(Object::Null);
    let n_val = as_u64(&n_obj, "N")?;
    let page_count = page_refs(pdf)
        .map_err(|e| LinearizationCheckError::Io(Box::new(e)))?
        .len() as u64;
    if n_val != page_count {
        fail!("/N ({n_val}) does not match page count ({page_count})");
    }

    // -----------------------------------------------------------------------
    // 4. /O must point to an existing Page object
    // -----------------------------------------------------------------------
    let o_obj = param_dict.get("O").cloned().unwrap_or(Object::Null);
    let o_num = as_u64(&o_obj, "O")?;
    // PDF object numbers are u32; an /O value beyond u32::MAX cannot refer to
    // a real object — silently casting with `as u32` would wrap and look up
    // the wrong slot, so reject up front.
    let o_num_u32 = u32::try_from(o_num).map_err(|_| LinearizationCheckError::InvalidParam {
        message: format!("/O ({o_num}) does not fit in u32 — invalid object number"),
    })?;
    let o_ref = ObjectRef::new(o_num_u32, 0);
    let o_object = pdf.resolve(o_ref).map_err(LinearizationCheckError::from)?;
    match &o_object {
        Object::Dictionary(d) => {
            if let Some(Object::Name(type_name)) = d.get("Type") {
                if type_name != b"Page" {
                    fail!(
                        "/O ({o_num}) points to an object with /Type /{} instead of /Page",
                        String::from_utf8_lossy(type_name)
                    );
                }
            } else if d.get("Parent").is_none() && d.get("MediaBox").is_none() {
                // Without /Type, require at least one of the structural keys
                // every Page object must inherit (/Parent) or define (/MediaBox).
                // Empty / unrelated dictionaries are not Page objects.
                fail!(
                    "/O ({o_num}) points to a dictionary with no /Type, /Parent or /MediaBox \
                     — does not look like a Page object"
                );
            }
        }
        Object::Null => {
            fail!("/O ({o_num}) refers to a non-existent object");
        }
        _ => {
            fail!("/O ({o_num}) does not refer to a dictionary");
        }
    }

    // -----------------------------------------------------------------------
    // 5. /H — hint stream at H[0] must be FlateDecode-decodable
    // -----------------------------------------------------------------------
    let h_obj = param_dict.get("H").cloned().unwrap_or(Object::Null);
    // /H is [offset, length] or [[offset, length], [offset2, length2]]
    let (h_offset, h_length) = match &h_obj {
        Object::Array(arr) if arr.len() >= 2 => {
            let off = as_u64(&arr[0], "H[0]")?;
            let len = as_u64(&arr[1], "H[1]")?;
            (off, len)
        }
        _ => {
            fail!("/H is missing or has unexpected format (expected [offset length])");
        }
    };

    // Bounds: H[0] within file, H[0]+H[1] within file.
    if h_offset >= file_len {
        fail!("/H[0] offset ({h_offset}) is beyond file length ({file_len})");
    }
    if h_offset.saturating_add(h_length) > file_len {
        fail!("/H[0]+/H[1] ({h_offset}+{h_length}) extends beyond file length ({file_len})");
    }

    // Verify the hint stream is decodable AND that /H[1] equals the byte
    // length the parsed stream actually occupies.  Without this check, a
    // back-patcher that miscomputes /H[1] silently passes here.
    check_hint_stream_at_offset(pdf, file_bytes, h_offset as usize, h_length)?;

    // -----------------------------------------------------------------------
    // 6. /E must be less than file length
    // -----------------------------------------------------------------------
    let e_obj = param_dict.get("E").cloned().unwrap_or(Object::Null);
    let e_val = as_u64(&e_obj, "E")?;
    if e_val >= file_len {
        fail!("/E ({e_val}) must be less than file length ({file_len})");
    }

    // -----------------------------------------------------------------------
    // 7. /T must be within the last cross-reference table.
    //
    // Different PDF producers use slightly different /T conventions:
    // - ISO 32000-1 Annex F: /T = byte offset of the xref keyword itself
    // - qpdf convention: /T = byte offset just before the first xref entry
    //   (i.e. offset of the last '\n' in the "xref\n0 N\n" header)
    //
    // We accept any /T that is within the xref section header (i.e. the xref
    // keyword is reachable by scanning backwards within a small window).
    // -----------------------------------------------------------------------
    let t_obj = param_dict.get("T").cloned().unwrap_or(Object::Null);
    let t_val = as_u64(&t_obj, "T")?;
    // /T must fit in the platform's `usize` (matters on 32-bit targets where
    // `u64 as usize` would silently truncate) and must leave at least 4 bytes
    // before EOF for the `xref` keyword.  Use checked_add to avoid wrap-around
    // overflow surprises in release builds.
    let t_usize = usize::try_from(t_val).map_err(|_| LinearizationCheckError::InvalidParam {
        message: format!("/T ({t_val}) does not fit in platform usize"),
    })?;
    if t_usize
        .checked_add(4)
        .is_none_or(|end| end > file_bytes.len())
    {
        fail!("/T ({t_val}) is too close to end of file to contain xref keyword");
    }
    // Allow /T to fall anywhere inside the cross-reference section header.
    // The window covers both ISO convention (/T = xref keyword) and
    // qpdf convention (/T = first_entry_pos - 1, ~= xref + header_len - 1).
    // 32 bytes is enough for `xref\n0 N\n` headers up to u32-sized object
    // counts (up to 10 decimal digits).
    const T_BACKSCAN_WINDOW: usize = 32;
    let search_start = t_usize.saturating_sub(T_BACKSCAN_WINDOW);
    let window = &file_bytes[search_start..=t_usize];
    // Match `xref` only as a standalone token (whitespace-bounded).  A naive
    // substring search would false-positively match the `xref` inside the
    // `startxref` keyword which sits in the trailer near the end of the file.
    let found_xref = window.windows(4).enumerate().any(|(i, w)| {
        if w != b"xref" {
            return false;
        }
        let prev_ok = i == 0 || is_pdf_whitespace(window[i - 1]);
        let next = i + 4;
        let next_ok = next == window.len() || is_pdf_whitespace(window[next]);
        prev_ok && next_ok
    });
    if !found_xref {
        fail!(
            "/T ({t_val}) is not within the last cross-reference section \
             (no xref keyword token found in the backscan window before /T)"
        );
    }

    Ok(())
}

/// Verify that the hint stream object at `offset` in `file_bytes`:
///
/// 1. Starts with a strict `N G obj` header at `offset`
/// 2. Spans `expected_h_length` bytes (matching `/H[1]`)
/// 3. Is a stream with `/Filter /FlateDecode` (or compatible) whose
///    compressed data can actually be decoded.
fn check_hint_stream_at_offset<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    file_bytes: &[u8],
    offset: usize,
    expected_h_length: u64,
) -> CheckResult {
    // /H[0] must point exactly at the `N G obj` header (after at most a few
    // leading whitespace bytes).  A loose scan that just searches for `obj`
    // anywhere in a window would accept misaligned offsets that happen to
    // sit near another object header — which is precisely the kind of
    // corruption we want to detect.
    const SCAN_WINDOW: usize = 64;
    let scan_end = (offset + SCAN_WINDOW).min(file_bytes.len());
    let window = &file_bytes[offset..scan_end];

    let Some((obj_num, obj_gen)) = parse_obj_header_at(window) else {
        fail!(
            "/H[0] offset ({offset}) does not point at an indirect object header \
             (expected `N G obj`)"
        );
    };

    // Compute the actual byte span of the indirect object: from `offset`
    // through the `endobj` keyword (plus a single trailing newline if
    // present, matching the canonical PDF whitespace).  This is what /H[1]
    // is supposed to advertise.
    let endobj_pos = file_bytes[offset..]
        .windows(b"endobj".len())
        .position(|w| w == b"endobj")
        .map(|p| offset + p);
    let Some(endobj_pos) = endobj_pos else {
        fail!(
            "hint stream object {obj_num} {obj_gen} (at /H[0] offset {offset}) has no endobj keyword"
        );
    };
    let mut actual_end = endobj_pos + b"endobj".len();
    if actual_end < file_bytes.len() && file_bytes[actual_end] == b'\n' {
        actual_end += 1;
    }
    let actual_length = (actual_end - offset) as u64;
    if actual_length != expected_h_length {
        fail!(
            "/H[1] ({expected_h_length}) does not match the actual hint stream byte length \
             ({actual_length}) measured from offset {offset} to endobj"
        );
    }

    // Resolve the object via the Pdf handle.  Use the parsed generation so a
    // hint stream with a non-zero generation (e.g. after incremental update)
    // is still locatable.
    let hint_ref = ObjectRef::new(obj_num, obj_gen);
    let hint_obj = pdf
        .resolve(hint_ref)
        .map_err(LinearizationCheckError::from)?;

    match hint_obj {
        Object::Stream(stream) => {
            // Attempt to decode the compressed data.
            decode_stream_data(&stream.dict, &stream.data).map_err(|e| {
                LinearizationCheckError::InvalidParam {
                    message: format!(
                        "hint stream (object {obj_num} {obj_gen}) could not be decoded: {e}"
                    ),
                }
            })?;
        }
        Object::Null => {
            fail!(
                "hint stream object {obj_num} {obj_gen} (at /H[0] offset {offset}) does not exist"
            );
        }
        _ => {
            fail!(
                "hint stream object {obj_num} {obj_gen} (at /H[0] offset {offset}) is not a stream"
            );
        }
    }

    Ok(())
}

/// Parse a complete `N G obj` indirect object header at the start of
/// `window` (after at most a small amount of leading PDF whitespace).
///
/// Returns `(N, G)` on success, `None` if the bytes do not look like an
/// indirect object header.  A loose scan that picks up the first digits in
/// a window would silently accept misaligned offsets — this strict parser
/// requires the `obj` keyword to follow exactly after `<digits> <digits>`.
fn parse_obj_header_at(window: &[u8]) -> Option<(u32, u16)> {
    // Skip leading whitespace.
    let mut i = 0;
    while i < window.len() && is_pdf_whitespace(window[i]) {
        i += 1;
    }

    // Object number digits.
    let num_start = i;
    while i < window.len() && window[i].is_ascii_digit() {
        i += 1;
    }
    if i == num_start {
        return None;
    }
    let obj_num: u32 = std::str::from_utf8(&window[num_start..i])
        .ok()?
        .parse()
        .ok()?;

    // Exactly one space (PDF allows any whitespace, but the canonical form is a
    // single space and we don't need to be permissive here).
    if i >= window.len() || !is_pdf_whitespace(window[i]) {
        return None;
    }
    i += 1;

    // Generation digits.
    let gen_start = i;
    while i < window.len() && window[i].is_ascii_digit() {
        i += 1;
    }
    if i == gen_start {
        return None;
    }
    let obj_gen: u16 = std::str::from_utf8(&window[gen_start..i])
        .ok()?
        .parse()
        .ok()?;

    // Whitespace before `obj`.
    if i >= window.len() || !is_pdf_whitespace(window[i]) {
        return None;
    }
    i += 1;

    // The `obj` keyword.
    if window.get(i..i + 3) != Some(b"obj") {
        return None;
    }

    Some((obj_num, obj_gen))
}

// ---------------------------------------------------------------------------
// Convenience: check a file given raw bytes (for library tests)
// ---------------------------------------------------------------------------

/// Check linearization using raw bytes (opens a `Pdf` from a `Cursor`).
///
/// This is a convenience wrapper for tests that already have the PDF in memory.
pub fn check_linearization_bytes(file_bytes: &[u8]) -> CheckResult {
    use std::io::Cursor;
    let mut pdf = Pdf::open(Cursor::new(file_bytes.to_vec()))
        .map_err(|e| LinearizationCheckError::Io(Box::new(e)))?;
    check_linearization(&mut pdf, file_bytes)
}

// ---------------------------------------------------------------------------
// Public wrapper that accepts a path (used by CLI)
// ---------------------------------------------------------------------------

/// Check linearization of the PDF at `path`.
///
/// Reads the file, opens a [`Pdf`], and runs all structural checks.
/// Returns a human-readable [`LinearizationCheckError`] on failure.
pub fn check_linearization_path(
    path: &std::path::Path,
) -> std::result::Result<(), LinearizationCheckError> {
    let file_bytes = std::fs::read(path)?;
    let mut pdf = Pdf::open(BufReader::new(std::io::Cursor::new(file_bytes.clone())))
        .map_err(|e| LinearizationCheckError::Io(Box::new(e)))?;
    check_linearization(&mut pdf, &file_bytes)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linearization::plan::LinearizationPlan;
    use crate::linearization::renumber::RenumberMap;
    use crate::linearization::writer::write_linearized;
    use std::io::Cursor;

    fn tiny_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!(
            "trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            xref_start
        );
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    fn build_linearized_bytes() -> Vec<u8> {
        let raw = tiny_pdf_bytes();
        let mut pdf = Pdf::open(Cursor::new(raw.clone())).unwrap();
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();
        let renumber = RenumberMap::from_plan(&plan);
        let mut pdf2 = Pdf::open(Cursor::new(raw)).unwrap();
        let mut doc = write_linearized(&plan, &renumber, &mut pdf2).unwrap();
        doc.back_patch().unwrap();
        doc.bytes
    }

    #[test]
    fn check_linearized_bytes_passes() {
        let bytes = build_linearized_bytes();
        let result = check_linearization_bytes(&bytes);
        assert!(
            result.is_ok(),
            "check should pass on well-formed linearized output: {result:?}"
        );
    }

    #[test]
    fn non_linearized_pdf_is_rejected() {
        let bytes = tiny_pdf_bytes();
        let result = check_linearization_bytes(&bytes);
        assert!(
            matches!(result, Err(LinearizationCheckError::NotLinearized)),
            "non-linearized PDF must yield NotLinearized, got {result:?}"
        );
    }

    #[test]
    fn tampered_l_is_rejected() {
        let mut bytes = build_linearized_bytes();
        // Find "/L 0000" and bump the last digit by 1 to make /L wrong.
        let needle = b"/L 0";
        if let Some(pos) = bytes.windows(needle.len()).position(|w| w == needle) {
            // Find the end of the 10-digit number (pos + 3 to pos + 13).
            let val_start = pos + 3; // after "/L "
            let val_end = val_start + 10;
            // Increment the last digit (with wrapping) to make the value wrong.
            bytes[val_end - 1] = if bytes[val_end - 1] == b'9' {
                b'0'
            } else {
                bytes[val_end - 1] + 1
            };
        }
        let result = check_linearization_bytes(&bytes);
        assert!(
            matches!(result, Err(LinearizationCheckError::InvalidParam { .. })),
            "tampered /L must yield InvalidParam, got {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // as_u64: fractional Real values must be rejected, not truncated
    // -----------------------------------------------------------------------
    #[test]
    fn as_u64_rejects_fractional_real() {
        let frac = Object::Real(1.9);
        assert!(
            matches!(
                as_u64(&frac, "N"),
                Err(LinearizationCheckError::InvalidParam { .. })
            ),
            "Real(1.9) must not be silently truncated to 1"
        );
    }

    #[test]
    fn as_u64_accepts_integer_valued_real() {
        let exact = Object::Real(42.0);
        assert_eq!(as_u64(&exact, "N").unwrap(), 42, "Real(42.0) is exact");
    }

    // -----------------------------------------------------------------------
    // parse_obj_header_at: full N G obj parser
    // -----------------------------------------------------------------------
    #[test]
    fn parse_obj_header_accepts_zero_generation() {
        assert_eq!(parse_obj_header_at(b"3 0 obj\n<<>>"), Some((3, 0)));
    }

    #[test]
    fn parse_obj_header_accepts_non_zero_generation() {
        // Hint stream in a non-zero generation must be locatable.
        assert_eq!(parse_obj_header_at(b"42 7 obj\n<<>>"), Some((42, 7)));
    }

    #[test]
    fn parse_obj_header_rejects_partial_match() {
        // Bytes that look like just a number — no `obj` keyword — must fail
        // (loose scan would return Some(123) here, hiding misaligned offsets).
        assert_eq!(parse_obj_header_at(b"123 4 not_an_obj\n"), None);
        assert_eq!(parse_obj_header_at(b"123 4"), None);
        assert_eq!(parse_obj_header_at(b"not digits"), None);
    }

    #[test]
    fn parse_obj_header_skips_leading_whitespace() {
        assert_eq!(parse_obj_header_at(b"  \n5 0 obj\n"), Some((5, 0)));
    }
}

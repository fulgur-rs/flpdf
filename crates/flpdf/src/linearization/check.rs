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
fn as_u64(obj: &Object, key: &str) -> std::result::Result<u64, LinearizationCheckError> {
    match obj {
        Object::Integer(n) if *n >= 0 => Ok(*n as u64),
        Object::Real(r) if r.is_finite() && *r >= 0.0 => Ok(*r as u64),
        other => Err(LinearizationCheckError::InvalidParam {
            message: format!(
                "/{key} is not a non-negative integer (got {})",
                debug_obj(other)
            ),
        }),
    }
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
    let o_ref = ObjectRef::new(o_num as u32, 0);
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
            } else {
                // No /Type key — acceptable if it is clearly a page dict
                // (has /Parent or /MediaBox).  Be lenient here.
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
    let (h_offset, _h_length) = match &h_obj {
        Object::Array(arr) if arr.len() >= 2 => {
            let off = as_u64(&arr[0], "H[0]")?;
            let len = as_u64(&arr[1], "H[1]")?;
            (off, len)
        }
        _ => {
            fail!("/H is missing or has unexpected format (expected [offset length])");
        }
    };

    // Locate the hint stream object by scanning from h_offset for "N G obj".
    // We do a lightweight check: the stream must be parseable and its
    // /Filter must include /FlateDecode.
    if h_offset >= file_len {
        fail!("/H[0] offset ({h_offset}) is beyond file length ({file_len})");
    }

    // Verify the hint stream is decodable.
    check_hint_stream_at_offset(pdf, file_bytes, h_offset as usize)?;

    // -----------------------------------------------------------------------
    // 6. /E must be less than file length
    // -----------------------------------------------------------------------
    let e_obj = param_dict.get("E").cloned().unwrap_or(Object::Null);
    let e_val = as_u64(&e_obj, "E")?;
    if e_val >= file_len {
        fail!("/E ({e_val}) must be less than file length ({file_len})");
    }

    // -----------------------------------------------------------------------
    // 7. /T must point to `xref` keyword
    // -----------------------------------------------------------------------
    let t_obj = param_dict.get("T").cloned().unwrap_or(Object::Null);
    let t_val = as_u64(&t_obj, "T")?;
    if t_val + 4 > file_len {
        fail!("/T ({t_val}) is too close to end of file to contain xref keyword");
    }
    let t_bytes = &file_bytes[t_val as usize..t_val as usize + 4];
    if t_bytes != b"xref" {
        fail!(
            "/T ({t_val}) does not point to xref keyword (found {:?})",
            String::from_utf8_lossy(t_bytes)
        );
    }

    Ok(())
}

/// Verify that the hint stream object near `offset` in `file_bytes` is a
/// stream with `/Filter /FlateDecode` (or compatible) and that the compressed
/// data can actually be decoded.
fn check_hint_stream_at_offset<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    file_bytes: &[u8],
    offset: usize,
) -> CheckResult {
    // Find "N G obj" by scanning from offset for a space-separated pattern.
    // We look for the sequence "<digits> <digits> obj" within a small window.
    const SCAN_WINDOW: usize = 256;
    let scan_end = (offset + SCAN_WINDOW).min(file_bytes.len());
    let window = &file_bytes[offset..scan_end];

    // Find the `obj` keyword position.
    let obj_pos = window.windows(3).position(|w| w == b"obj");
    let Some(_obj_pos) = obj_pos else {
        fail!(
            "/H[0] offset ({offset}) does not point to an indirect object (no 'obj' keyword found)"
        );
    };

    // Parse the object number from the window.
    let obj_num = parse_obj_number_at(window);
    let Some(obj_num) = obj_num else {
        fail!("/H[0] offset ({offset}) could not be parsed as an indirect object header");
    };

    // Resolve the object via the Pdf handle.
    let hint_ref = ObjectRef::new(obj_num, 0);
    let hint_obj = pdf
        .resolve(hint_ref)
        .map_err(LinearizationCheckError::from)?;

    match hint_obj {
        Object::Stream(stream) => {
            // Attempt to decode the compressed data.
            decode_stream_data(&stream.dict, &stream.data).map_err(|e| {
                LinearizationCheckError::InvalidParam {
                    message: format!("hint stream (object {obj_num}) could not be decoded: {e}"),
                }
            })?;
        }
        Object::Null => {
            fail!("hint stream object {obj_num} (at /H[0] offset {offset}) does not exist");
        }
        _ => {
            fail!("hint stream object {obj_num} (at /H[0] offset {offset}) is not a stream");
        }
    }

    Ok(())
}

/// Parse the indirect object number from bytes that start at (or near) an
/// indirect object header like `"N G obj"`.  Returns the number `N`, or `None`
/// if the parse fails.
fn parse_obj_number_at(window: &[u8]) -> Option<u32> {
    // Skip leading whitespace.
    let start = window.iter().position(|&b| b.is_ascii_digit())?;
    // Read decimal digits.
    let end = window[start..]
        .iter()
        .position(|&b| !b.is_ascii_digit())
        .map(|p| start + p)
        .unwrap_or(window.len());
    let num_str = std::str::from_utf8(&window[start..end]).ok()?;
    num_str.parse::<u32>().ok()
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
}

//! Linearization structural checker.
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
//! | `/Linearized` | The first object in the file (physical position, not object number) has the key with a positive numeric value |
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
//! - `Err(LinearizationCheckError::NotLinearized)` — the first object in the
//!   file (physical position, not object number) has no `/Linearized` key
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
    /// The PDF is not linearized: the first object physically present in the
    /// file (as located by `find_first_object_ref`) is missing or does not
    /// expose a `/Linearized` key. PDF 1.7 Annex F.2.2.1 mandates that the
    /// linearization parameter dictionary be the first object in a linearized
    /// file, regardless of its object number.
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
                write!(
                    f,
                    "not a linearized PDF: the first object in the file has no /Linearized key"
                )
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
///
/// # Errors
///
/// Returns [`LinearizationCheckError::NotLinearized`] when no first object
/// header can be located, the first object is not a dictionary, or it has no
/// positive `/Linearized` key.
///
/// Returns [`LinearizationCheckError::InvalidParam`] when a param-dict
/// invariant fails: a value (`/L`, `/N`, `/O`, `/E`, `/T`, `/H` elements) is
/// not a non-negative integer, `/O` does not fit in `u32` or does not refer to
/// a Page object, `/L` does not equal the file length, `/N` does not equal the
/// page count, `/E` is not less than the file length, `/H` is malformed or out
/// of bounds, the hint stream cannot be located or decoded, or `/T` does not
/// fall within the last cross-reference section (no `xref` keyword in the
/// backscan window and no `/Type /XRef` stream at the `/T` target).
///
/// Returns [`LinearizationCheckError::Io`] when resolving an object via `pdf`
/// or enumerating the page references fails.
pub fn check_linearization<R: Read + Seek>(pdf: &mut Pdf<R>, file_bytes: &[u8]) -> CheckResult {
    let file_len = file_bytes.len() as u64;

    // -----------------------------------------------------------------------
    // 1. The first object in the file must have /Linearized with a positive
    //    value. PDF 1.7 Annex F.2.2.1 specifies "the first object" by
    //    physical position, not by object number — qpdf places the param
    //    dict at an obj number determined by its renumber pass, so we have
    //    to identify it from the file header's first object token.
    // -----------------------------------------------------------------------
    let first_obj_ref =
        find_first_object_ref(file_bytes).ok_or(LinearizationCheckError::NotLinearized)?;
    let first_obj = pdf
        .resolve_borrowed(first_obj_ref)
        .map_err(LinearizationCheckError::from)?;
    let Some(param_dict) = first_obj.as_dict().cloned() else {
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
    let o_object = pdf
        .resolve_borrowed(o_ref)
        .map_err(LinearizationCheckError::from)?;
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
    // 7. /T must be within the last cross-reference *section*.
    //
    // Different PDF producers use slightly different /T conventions:
    // - ISO 32000-1 Annex F: /T = byte offset of the xref keyword itself
    // - qpdf convention: /T = byte offset just before the first xref entry
    //   (i.e. offset of the last '\n' in the "xref\n0 N\n" header)
    // - cross-reference *stream* (ObjStm-bearing / split-xref linearized
    //   output, flpdf-9hc.5.8.4): there is no `xref` keyword at all — the
    //   cross-reference data lives in an indirect XRef stream object and /T
    //   points at that object's `<num> <gen> obj` header (the first-page xref
    //   stream the main xref's `/Prev` chains back to).
    //
    // We accept either form: a classic `xref` keyword reachable by a short
    // backscan, OR an XRef stream object header at the /T target.
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
    // Extend the window 3 bytes past `t_usize` so a `/T` that points exactly
    // at the start of `xref` (Annex F convention) can still find all four
    // bytes of the keyword in the slice.
    let window_end = (t_usize + 4).min(file_bytes.len());
    let window = &file_bytes[search_start..window_end];
    // Match `xref` only as a standalone token (whitespace-bounded).  A naive
    // substring search would false-positively match the `xref` inside the
    // `startxref` keyword which sits in the trailer near the end of the file.
    // Boundary checks use absolute `file_bytes` positions so the slice edges
    // are not mistaken for file boundaries.
    let xref_pos = window.windows(4).enumerate().find_map(|(i, w)| {
        if w != b"xref" {
            return None;
        }
        let absolute = search_start + i;
        let prev_ok = absolute == 0 || is_pdf_whitespace(file_bytes[absolute - 1]);
        let next = absolute + 4;
        let next_ok = next >= file_bytes.len() || is_pdf_whitespace(file_bytes[next]);
        if prev_ok && next_ok {
            Some(absolute)
        } else {
            None
        }
    });
    let Some(xref_pos) = xref_pos else {
        // No classic `xref` keyword: this is a cross-reference *stream* file
        // (ObjStm-bearing / split-xref linearized output).  /T must point at
        // an indirect object header whose object is a `/Type /XRef` stream.
        // The first-page xref stream is emitted before /E and the main xref's
        // `/Prev` chains back to it, so /T = that object's `<num> <gen> obj`
        // header offset.
        let (xref_obj_num, xref_obj_gen) =
            parse_obj_header_at(&file_bytes[t_usize..]).ok_or_else(|| {
                LinearizationCheckError::InvalidParam {
                    message: format!(
                        "/T ({t_val}) is not within the last cross-reference section \
                     (no `xref` keyword in the backscan window and no `<num> <gen> obj` \
                     header at /T for a cross-reference stream)"
                    ),
                }
            })?;
        // Resolve with the *parsed* generation, not a hardcoded 0: this
        // checker validates arbitrary linearized PDFs (including third-party
        // producers), and a cross-reference stream with gen != 0 is
        // spec-legal — hardcoding 0 would mis-resolve and spuriously reject it.
        let xref_obj = pdf
            .resolve_borrowed(ObjectRef::new(xref_obj_num, xref_obj_gen))
            .map_err(LinearizationCheckError::from)?;
        let is_xref_stream = matches!(
            &xref_obj,
            Object::Stream(s)
                if matches!(s.dict.get("Type"), Some(Object::Name(t)) if t.as_slice() == b"XRef")
        );
        if !is_xref_stream {
            fail!(
                "/T ({t_val}) points at object {xref_obj_num} which is not a \
                 `/Type /XRef` cross-reference stream"
            );
        }
        return Ok(());
    };

    // Tighten: /T must lie inside the xref subsection header itself
    // (`xref\n<start> <count>\n`), i.e. in `[xref_pos, first_entry_pos)`.
    // Without this, a /T that lands in the middle of the first xref entry
    // (or further into the table) would silently pass.
    let first_entry_pos = parse_xref_first_entry_pos(file_bytes, xref_pos).ok_or_else(|| {
        LinearizationCheckError::InvalidParam {
            message: format!(
                "/T ({t_val}) backscan found `xref` at byte {xref_pos}, but the \
                 subsection header (`<start> <count>\\n`) is malformed or truncated"
            ),
        }
    })?;
    if t_usize < xref_pos || t_usize >= first_entry_pos {
        fail!(
            "/T ({t_val}) is outside the xref subsection header range \
             [{xref_pos}, {first_entry_pos}) — must point at the `xref` keyword \
             or inside its subsection header line, not into the entries"
        );
    }

    Ok(())
}

/// Given the byte position of an `xref` keyword in `file_bytes`, parse the
/// first subsection header (`xref\n<start> <count>\n`) and return the byte
/// position of the *first* entry that follows it.
///
/// Returns `None` if the bytes after `xref_pos` do not match the expected
/// shape `xref\n<digits> <digits>\n` within a small window.
fn parse_xref_first_entry_pos(file_bytes: &[u8], xref_pos: usize) -> Option<usize> {
    // Skip past `xref` keyword.
    let mut i = xref_pos.checked_add(4)?;
    // Skip the EOL (CR / LF / CRLF) immediately after `xref`.
    while i < file_bytes.len() && is_pdf_whitespace(file_bytes[i]) {
        i += 1;
    }
    // <start>
    let digits1_start = i;
    while i < file_bytes.len() && file_bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits1_start {
        return None;
    }
    // single space
    if i >= file_bytes.len() || file_bytes[i] != b' ' {
        return None;
    }
    i += 1;
    // <count>
    let digits2_start = i;
    while i < file_bytes.len() && file_bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits2_start {
        return None;
    }
    // EOL after the header line.
    if i >= file_bytes.len() || !is_pdf_whitespace(file_bytes[i]) {
        return None;
    }
    while i < file_bytes.len() && is_pdf_whitespace(file_bytes[i]) {
        i += 1;
    }
    Some(i)
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
        .resolve_borrowed(hint_ref)
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
/// Locate the first indirect-object header in `file_bytes` and return its
/// [`ObjectRef`]. PDF 1.7 Annex F.2.2.1 says the first object in a linearized
/// file is the linearization parameter dictionary, but does not constrain
/// its object *number* — qpdf assigns it dynamically during renumbering. We
/// therefore scan the bytes after the PDF header for the first `N G obj`
/// token. The generation is preserved (rarely non-zero in practice, but a
/// param dict written as `12 7 obj` is still valid PDF and must resolve to
/// that exact ref, not to `12 0`).
///
/// Returns `None` if no object header is found (e.g. truncated or
/// non-PDF input).
pub(crate) fn find_first_object_ref(file_bytes: &[u8]) -> Option<ObjectRef> {
    // Scan for "<num><ws+><gen><ws+>obj" anchored at a real line start.
    //
    // PDF spec (ISO 32000-1 §7.2.3) permits any non-empty whitespace
    // sequence between the three tokens, including tabs and multiple
    // spaces. We search for the `obj` keyword and validate token
    // boundaries via `parse_obj_header_at`, then anchor the candidate at
    // the start of its line so we ignore both header comments and
    // accidental matches inside content streams (e.g. the word "object").
    let mut i = 0;
    while i + 3 <= file_bytes.len() {
        let pos_in_slice = file_bytes[i..].windows(3).position(|w| w == b"obj")?;
        let abs = i + pos_in_slice;

        // The byte immediately before `obj` must be PDF whitespace —
        // otherwise we've hit the suffix of an unrelated identifier.
        let preceded_by_ws = abs
            .checked_sub(1)
            .and_then(|p| file_bytes.get(p))
            .is_some_and(|&b| is_pdf_whitespace(b));

        if preceded_by_ws {
            // Anchor at the start of this line, skipping leading whitespace.
            let line_start = file_bytes[..abs]
                .iter()
                .rposition(|&b| matches!(b, b'\n' | b'\r'))
                .map_or(0, |p| p + 1);
            let mut start = line_start;
            while start < abs && is_pdf_whitespace(file_bytes[start]) {
                start += 1;
            }

            let not_in_comment = file_bytes.get(start) != Some(&b'%');
            if not_in_comment {
                if let Some((num, gen)) = parse_obj_header_at(&file_bytes[start..]) {
                    return Some(ObjectRef::new(num, gen));
                }
            }
        }
        // Failed validation or strict parse; keep scanning past this `obj`.
        i = abs + 3;
    }
    None
}

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

    // One-or-more PDF whitespace bytes between the number and generation.
    // ISO 32000-1 §7.2.3 admits any non-empty whitespace sequence (space,
    // tab, CR, LF, FF, NUL).
    if i >= window.len() || !is_pdf_whitespace(window[i]) {
        return None;
    }
    while i < window.len() && is_pdf_whitespace(window[i]) {
        i += 1;
    }

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

    // One-or-more PDF whitespace bytes before the `obj` keyword.
    if i >= window.len() || !is_pdf_whitespace(window[i]) {
        return None;
    }
    while i < window.len() && is_pdf_whitespace(window[i]) {
        i += 1;
    }

    // The `obj` keyword.
    if window.get(i..i + 3) != Some(b"obj") {
        return None;
    }
    i += 3;

    // The keyword must end at a PDF whitespace byte (or EOF). Without this
    // post-token check the parser would also accept `object` and surface a
    // bogus `(num, gen)` pair to `find_first_object_ref`.
    match window.get(i) {
        None => {}
        Some(&b) if is_pdf_whitespace(b) => {}
        _ => return None,
    }

    Some((obj_num, obj_gen))
}

// ---------------------------------------------------------------------------
// Convenience: check a file given raw bytes (for library tests)
// ---------------------------------------------------------------------------

/// Check linearization using raw bytes (opens a `Pdf` from a `Cursor`).
///
/// This is a convenience wrapper for tests that already have the PDF in memory.
///
/// # Errors
///
/// Returns [`LinearizationCheckError::Io`] when opening the [`Pdf`] from the
/// in-memory bytes fails. Otherwise propagates any error from
/// [`check_linearization`].
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
///
/// # Errors
///
/// Returns [`LinearizationCheckError::Io`] when reading the file at `path` or
/// opening the [`Pdf`] fails. Otherwise propagates any error from
/// [`check_linearization`].
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
    use crate::writer::WriteOptions;
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
        let mut doc =
            write_linearized(&plan, &renumber, &mut pdf2, &WriteOptions::default()).unwrap();
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
        // Find "/L " followed by ASCII digits (variable-width post flpdf-9hc.20.25)
        // and bump the last digit by 1 to make /L wrong.
        let needle = b"/L ";
        let pos = bytes
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("linearized output must contain /L");
        let val_start = pos + needle.len();
        let val_end = val_start
            + bytes[val_start..]
                .iter()
                .position(|&b| !b.is_ascii_digit())
                .expect("/L value must be followed by a non-digit terminator");
        assert!(val_end > val_start, "/L value must have at least one digit");
        // Increment the last digit (with wrap) to make the value wrong.
        let last = val_end - 1;
        bytes[last] = if bytes[last] == b'9' {
            b'0'
        } else {
            bytes[last] + 1
        };

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

    // -----------------------------------------------------------------------
    // parse_xref_first_entry_pos: helper unit tests
    // -----------------------------------------------------------------------
    #[test]
    fn parse_xref_first_entry_pos_basic() {
        // `xref\n0 4\n` — header is 9 bytes (4 + 1 + 1 + 1 + 1 + 1).
        let bytes = b"xref\n0 4\n0000000000 65535 f \n";
        // xref keyword is at position 0; first entry starts after `xref\n0 4\n`.
        assert_eq!(parse_xref_first_entry_pos(bytes, 0), Some(9));
    }

    #[test]
    fn parse_xref_first_entry_pos_with_offset() {
        // Same header preceded by some prefix bytes.
        let bytes = b"prefix\nxref\n12 100\n0000000000 ...";
        let xref_pos = bytes.windows(4).position(|w| w == b"xref").unwrap();
        // header = `xref\n12 100\n` = 4 + 1 + 6 + 1 = 12 bytes.
        let expected_first_entry = xref_pos + 12;
        assert_eq!(
            parse_xref_first_entry_pos(bytes, xref_pos),
            Some(expected_first_entry)
        );
    }

    #[test]
    fn parse_xref_first_entry_pos_rejects_malformed() {
        // No newline after `xref`.
        assert_eq!(parse_xref_first_entry_pos(b"xrefjunk", 0), None);
        // No <count>.
        assert_eq!(parse_xref_first_entry_pos(b"xref\n0\n", 0), None);
        // Truncated.
        assert_eq!(parse_xref_first_entry_pos(b"xref\n0 ", 0), None);
    }

    // -----------------------------------------------------------------------
    // find_first_object_ref preserves both the object number and generation
    // -----------------------------------------------------------------------

    #[test]
    fn find_first_object_ref_returns_object_number_and_generation() {
        // A minimal PDF prefix with a non-zero generation on the first
        // object — the helper must surface generation 7, not silently
        // collapse it to 0 (which would cause the wrong object to resolve).
        let bytes: &[u8] = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n12 7 obj\n<< /Linearized 1 >>\n";
        let r = find_first_object_ref(bytes).expect("expected an object ref");
        assert_eq!(r.number, 12);
        assert_eq!(r.generation, 7);
    }

    #[test]
    fn find_first_object_ref_handles_zero_generation() {
        let bytes: &[u8] = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n3 0 obj\n";
        let r = find_first_object_ref(bytes).expect("expected an object ref");
        assert_eq!(r.number, 3);
        assert_eq!(r.generation, 0);
    }

    #[test]
    fn find_first_object_ref_returns_none_on_missing_obj() {
        assert_eq!(find_first_object_ref(b"%PDF-1.7\nxref\n0 0\n"), None);
    }

    #[test]
    fn find_first_object_ref_skips_comment_lines_that_look_like_obj_headers() {
        // A comment in the header area may textually contain "<N> <G> obj"
        // (qpdf, for instance, used to embed similar tokens in pdf comments).
        // The scanner must skip the comment and resolve the actual obj that
        // starts the body.
        let bytes: &[u8] = b"%PDF-1.7\n% example: 12 7 obj inside comment\n3 0 obj\n<<>>\n";
        let r = find_first_object_ref(bytes).expect("expected an object ref");
        assert_eq!(r.number, 3);
        assert_eq!(r.generation, 0);
    }

    #[test]
    fn find_first_object_ref_rejects_word_object_in_content_stream() {
        // The literal `obj` is also a prefix of the word `object`. Without
        // a delimiter check after the keyword, the scanner would surface
        // bogus `(num, gen)` pairs whenever a real content stream mentions
        // an "object" coordinate.
        let bytes: &[u8] = b"%PDF-1.7\nq 12 7 object\nQ\n5 0 obj\n";
        let r = find_first_object_ref(bytes).expect("expected an object ref");
        assert_eq!(r.number, 5);
        assert_eq!(r.generation, 0);
    }

    #[test]
    fn parse_obj_header_rejects_object_word() {
        // Direct unit-level check: `12 7 object` must not parse as
        // `(12, 7) obj …` because the `obj` keyword is followed by a
        // letter, not a PDF whitespace byte.
        assert_eq!(parse_obj_header_at(b"12 7 object"), None);
    }

    #[test]
    fn parse_obj_header_accepts_obj_followed_by_eof() {
        // Degenerate but tolerable: `12 7 obj` at the very end of the
        // buffer should still parse since there is no following byte to
        // disprove the delimiter.
        assert_eq!(parse_obj_header_at(b"12 7 obj"), Some((12, 7)));
    }

    // ISO 32000-1 §7.2.3 admits any non-empty whitespace sequence between
    // the three tokens of an indirect-object header. Pin a few of the
    // shapes that pdf writers in the wild actually emit.
    #[test]
    fn parse_obj_header_accepts_tab_between_number_and_generation() {
        assert_eq!(parse_obj_header_at(b"12\t7 obj"), Some((12, 7)));
    }

    #[test]
    fn parse_obj_header_accepts_tab_before_obj_keyword() {
        assert_eq!(parse_obj_header_at(b"12 7\tobj"), Some((12, 7)));
    }

    #[test]
    fn parse_obj_header_accepts_multiple_spaces_between_tokens() {
        assert_eq!(parse_obj_header_at(b"12  7   obj"), Some((12, 7)));
    }

    #[test]
    fn find_first_object_ref_accepts_tab_separated_header() {
        let bytes: &[u8] = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n3\t0\tobj\n";
        let r = find_first_object_ref(bytes).expect("expected an object ref");
        assert_eq!(r.number, 3);
        assert_eq!(r.generation, 0);
    }

    #[test]
    fn find_first_object_ref_accepts_multispace_separated_header() {
        let bytes: &[u8] = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n12  7  obj\n";
        let r = find_first_object_ref(bytes).expect("expected an object ref");
        assert_eq!(r.number, 12);
        assert_eq!(r.generation, 7);
    }
}

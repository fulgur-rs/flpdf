//! qpdf correspondence: QPDF_linearization.cc `isLinearized` detection and structural validation represented as a standalone checker.
//! Linearization detector and structural checker.
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

use crate::{DecodeLevel, ObjectHandle, ObjectRef, PageDocumentHelper, Pdf, Result};
use std::fmt;
use std::io::{BufReader, Read, Seek};
use std::rc::Rc;

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

impl<R: Read + Seek> Pdf<R> {
    /// Return whether qpdf would classify this document as linearized.
    ///
    /// This ports `QPDF::isLinearized` from
    /// `libqpdf/QPDF_linearization.cc:84-155`: only the first 1024-byte
    /// candidate scan, `/Linearized`, and integer `/L` participate. Candidate
    /// resolution failures become `false`, as qpdf's `QPDF::resolve` converts
    /// damaged objects to null (`libqpdf/QPDF.cc:1700-1753`).
    ///
    /// The predicate lives with the `QPDF_linearization.cc` responsibility;
    /// the source seek/read seam remains owned by the canonical resolver.
    /// The deeper `/N`, `/O`, `/H`, `/T`, and `/P` checks belong to this
    /// module's [`check_linearization`] analogue.
    pub fn is_linearized(&mut self) -> Result<bool> {
        let Some(object_number) = self.resolver.linearization_candidate()? else {
            return Ok(false);
        };
        let Ok(object_number) = u32::try_from(object_number) else {
            return Ok(false);
        };
        if object_number == 0 {
            return Ok(false);
        }

        let candidate = self.get_object_handle(ObjectRef::new(object_number, 0));
        let Some(dictionary) = candidate.try_as_dictionary().ok().flatten() else {
            return Ok(false);
        };

        let Some(linearized) = dictionary.get(&b"/Linearized"[..]) else {
            return Ok(false);
        };
        if linearized.try_dereference().is_err() {
            return Ok(false);
        }
        let Some(value) = linearized
            .as_integer()
            .map(|value| value as f64)
            .or_else(|| linearized.as_real())
        else {
            return Ok(false);
        };
        if !value.is_finite() || value.floor() != 1.0 {
            return Ok(false);
        }

        if let Some(l_value) = dictionary.get(&b"/L"[..]) {
            if l_value.try_dereference().is_err() {
                return Ok(false);
            }
            if let Some(l_value) = l_value.as_integer() {
                let file_size = self.resolver.source_length()?;
                if l_value < 0 || l_value as u64 != file_size {
                    return Ok(false);
                }
            }
        }

        Ok(true)
    }
}

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

/// Extract a qpdf-style non-negative integer from a canonical object handle.
///
/// `readLinearizationData` checks `isInteger()` before reading `/O`, `/E`,
/// `/N`, `/T`, `/P`, and every `/H` item.  Do not accept an integer-valued
/// real here: that would make malformed linearization dictionaries pass the
/// consumer route even though qpdf rejects them.
fn as_u64(obj: &ObjectHandle, key: &str) -> std::result::Result<u64, LinearizationCheckError> {
    obj.try_dereference()
        .map_err(LinearizationCheckError::from)?;
    match obj.as_integer() {
        Some(n) if n >= 0 => Ok(n as u64),
        _ => Err(LinearizationCheckError::InvalidParam {
            message: format!(
                "/{key} is not a non-negative integer (got {})",
                obj.type_name()
            ),
        }),
    }
}

/// Return `true` if `b` is a PDF whitespace byte (ISO 32000-1 §7.2.3).
fn is_pdf_whitespace(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
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
    let first_obj = pdf.get_object_handle(first_obj_ref);
    let Some(_) = first_obj
        .try_as_dictionary()
        .map_err(LinearizationCheckError::from)?
    else {
        return Err(LinearizationCheckError::NotLinearized);
    };

    let linearized_val = first_obj
        .try_get_key(b"/Linearized")
        .map_err(LinearizationCheckError::from)?;
    linearized_val
        .try_dereference()
        .map_err(LinearizationCheckError::from)?;
    let linearized_value = linearized_val
        .as_integer()
        .map(|value| value as f64)
        .or_else(|| linearized_val.as_real());
    if !linearized_value.is_some_and(|value| value.is_finite() && value.floor() == 1.0) {
        return Err(LinearizationCheckError::NotLinearized);
    }

    // -----------------------------------------------------------------------
    // 2. /L must equal file length
    // -----------------------------------------------------------------------
    let l_obj = first_obj
        .try_get_key(b"/L")
        .map_err(LinearizationCheckError::from)?;
    let l_val = as_u64(&l_obj, "L")?;
    if l_val != file_len {
        fail!("/L ({l_val}) does not match file length ({file_len})");
    }

    // -----------------------------------------------------------------------
    // 3. /N must equal the page count
    // -----------------------------------------------------------------------
    let n_obj = first_obj
        .try_get_key(b"/N")
        .map_err(LinearizationCheckError::from)?;
    let n_val = as_u64(&n_obj, "N")?;
    let page_count = PageDocumentHelper::new(pdf)
        .get_all_pages()
        .map_err(|e| LinearizationCheckError::Io(Box::new(e)))?
        .len() as u64;
    if n_val != page_count {
        fail!("/N ({n_val}) does not match page count ({page_count})");
    }

    // -----------------------------------------------------------------------
    // 4. /O must point to an existing Page object
    // -----------------------------------------------------------------------
    let o_obj = first_obj
        .try_get_key(b"/O")
        .map_err(LinearizationCheckError::from)?;
    let o_num = as_u64(&o_obj, "O")?;
    // PDF object numbers are u32; an /O value beyond u32::MAX cannot refer to
    // a real object — silently casting with `as u32` would wrap and look up
    // the wrong slot, so reject up front.
    let o_num_u32 = u32::try_from(o_num).map_err(|_| LinearizationCheckError::InvalidParam {
        message: format!("/O ({o_num}) does not fit in u32 — invalid object number"),
    })?;
    let o_ref = ObjectRef::new(o_num_u32, 0);
    let o_object = pdf.get_object_handle(o_ref);
    let is_null = o_object
        .try_is_null()
        .map_err(LinearizationCheckError::from)?;
    let Some(_) = o_object
        .try_as_dictionary()
        .map_err(LinearizationCheckError::from)?
    else {
        if is_null {
            fail!("/O ({o_num}) refers to a non-existent object");
        }
        fail!("/O ({o_num}) does not refer to a dictionary");
    };

    let type_obj = o_object
        .try_get_key(b"/Type")
        .map_err(LinearizationCheckError::from)?;
    type_obj
        .try_dereference()
        .map_err(LinearizationCheckError::from)?;
    if let Some(type_name) = type_obj.as_name() {
        if type_name != b"Page" {
            fail!(
                "/O ({o_num}) points to an object with /Type /{} instead of /Page",
                String::from_utf8_lossy(&type_name)
            );
        }
    } else if type_obj
        .try_is_null()
        .map_err(LinearizationCheckError::from)?
    {
        let parent = o_object
            .try_get_key(b"/Parent")
            .map_err(LinearizationCheckError::from)?;
        let media_box = o_object
            .try_get_key(b"/MediaBox")
            .map_err(LinearizationCheckError::from)?;
        if parent
            .try_is_null()
            .map_err(LinearizationCheckError::from)?
            && media_box
                .try_is_null()
                .map_err(LinearizationCheckError::from)?
        {
            // Without /Type, require at least one of the structural keys every
            // Page object must inherit or define.
            fail!(
                "/O ({o_num}) points to a dictionary with no /Type, /Parent or /MediaBox \
                 — does not look like a Page object"
            );
        }
    }

    // -----------------------------------------------------------------------
    // 5. /H — hint stream at H[0] must be FlateDecode-decodable
    // -----------------------------------------------------------------------
    let h_obj = first_obj
        .try_get_key(b"/H")
        .map_err(LinearizationCheckError::from)?;
    h_obj
        .try_dereference()
        .map_err(LinearizationCheckError::from)?;
    let Some(h_items) = h_obj.as_array() else {
        fail!("/H is missing or has unexpected format (expected [offset length])");
    };
    if !matches!(h_items.len(), 2 | 4) {
        fail!(
            "/H has the wrong number of items (expected 2 or 4, got {})",
            h_items.len()
        );
    }
    for (index, item) in h_items.iter().enumerate() {
        let _ = as_u64(item, &format!("H[{index}]"))?;
    }
    let h_offset = as_u64(&h_items[0], "H[0]")?;
    let h_length = as_u64(&h_items[1], "H[1]")?;

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
    let e_obj = first_obj
        .try_get_key(b"/E")
        .map_err(LinearizationCheckError::from)?;
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
    let t_obj = first_obj
        .try_get_key(b"/T")
        .map_err(LinearizationCheckError::from)?;
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
        let xref_obj = pdf.get_object_handle(ObjectRef::new(xref_obj_num, xref_obj_gen));
        xref_obj
            .try_dereference()
            .map_err(LinearizationCheckError::from)?;
        let is_xref_stream = if let Some(xref_dict) = xref_obj.as_stream_dict() {
            let type_obj = xref_dict
                .try_get_key(b"/Type")
                .map_err(LinearizationCheckError::from)?;
            type_obj
                .try_dereference()
                .map_err(LinearizationCheckError::from)?;
            type_obj.as_name().as_deref() == Some(b"XRef")
        } else {
            false
        };
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

/// Resolve and decode the hint stream object at `offset` through the canonical
/// object/stream route.
///
/// The raw byte slice is used only to identify the object reference at `/H[0]`.
/// The object itself is then resolved as an [`ObjectHandle`], its qpdf source
/// extents are used for the `/H[1]` check, and `get_stream_data` runs the
/// specialized stream pipeline. This mirrors qpdf's `readHintStream` boundary
/// (`libqpdf/QPDF_linearization.cc:245-321`) without materializing a legacy
/// `Object::Stream` value.
pub(crate) fn load_hint_stream<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    file_bytes: &[u8],
    offset: usize,
    expected_h_length: u64,
) -> Result<(ObjectHandle, Rc<Vec<u8>>)> {
    // /H[0] must point exactly at the `N G obj` header (after at most a few
    // leading whitespace bytes).  A loose scan that just searches for `obj`
    // anywhere in a window would accept misaligned offsets that happen to
    // sit near another object header — which is precisely the kind of
    // corruption we want to detect.
    const SCAN_WINDOW: usize = 64;
    let scan_end = (offset + SCAN_WINDOW).min(file_bytes.len());
    let window = &file_bytes[offset..scan_end];

    let Some((obj_num, obj_gen)) = parse_obj_header_at(window) else {
        return Err(crate::Error::Unsupported(format!(
            "/H[0] offset ({offset}) does not point at an indirect object header \
             (expected `N G obj`)"
        )));
    };

    // Resolve the object via the Pdf handle.  Use the parsed generation so a
    // hint stream with a non-zero generation (e.g. after incremental update)
    // is still locatable.
    let hint_ref = ObjectRef::new(obj_num, obj_gen);
    let hint_obj = pdf.get_object_handle(hint_ref);
    hint_obj.try_dereference()?;
    let Some(hint_dict) = hint_obj.as_stream_dict() else {
        if hint_obj.is_null() {
            return Err(crate::Error::Unsupported(format!(
                "hint stream object {obj_num} {obj_gen} (at /H[0] offset {offset}) does not exist"
            )));
        }
        return Err(crate::Error::Unsupported(format!(
            "hint stream object {obj_num} {obj_gen} (at /H[0] offset {offset}) is not a stream"
        )));
    };

    // qpdf's readHintStream checks the object end span, but if /Length is an
    // indirect object immediately after the stream it checks the length
    // object's extent instead (`QPDF_linearization.cc:269-294`).
    let (mut end_before_space, mut end_after_space) = hint_obj.end_offsets();
    let length_obj = hint_dict.try_get_key(b"/Length")?;
    if length_obj.object_ref().is_some() {
        length_obj.try_dereference()?;
        (end_before_space, end_after_space) = length_obj.end_offsets();
    }
    let computed_end = offset
        .checked_add(usize::try_from(expected_h_length).map_err(|_| {
            crate::Error::Unsupported(format!(
                "/H[1] ({expected_h_length}) does not fit in platform usize"
            ))
        })?)
        .ok_or_else(|| {
            crate::Error::Unsupported(format!(
                "/H[0] ({offset}) plus /H[1] ({expected_h_length}) overflows"
            ))
        })?;
    if end_before_space >= 0
        && end_after_space >= 0
        && (i64::try_from(computed_end).unwrap_or(i64::MAX) < end_before_space
            || i64::try_from(computed_end).unwrap_or(i64::MAX) > end_after_space)
    {
        return Err(crate::Error::Unsupported(format!(
            "/H[1] ({expected_h_length}) does not match hint stream object span \
             {end_before_space}..{end_after_space} from offset {offset}"
        )));
    }

    let decoded = hint_obj.get_stream_data(DecodeLevel::Specialized)?;
    Ok((hint_dict, decoded))
}

/// Verify that the hint stream can be located, has the qpdf source extent
/// advertised by `/H[1]`, and can be decoded by the specialized pipeline.
fn check_hint_stream_at_offset<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    file_bytes: &[u8],
    offset: usize,
    expected_h_length: u64,
) -> CheckResult {
    load_hint_stream(pdf, file_bytes, offset, expected_h_length).map_err(|error| match error {
        crate::Error::Unsupported(message) => LinearizationCheckError::InvalidParam { message },
        error => LinearizationCheckError::Io(Box::new(error)),
    })?;
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
    use crate::writer::WriterOptions;
    use std::io::{Cursor, Read, Seek, SeekFrom};

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

    /// Build a minimal PDF whose first physical object is object `(3, 0)` and
    /// carries the supplied linearization dictionary entries.
    fn linearized_like_pdf_bytes(
        linearized: &[u8],
        l_literal: Option<&[u8]>,
        prefix: &[u8],
    ) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        pdf.extend_from_slice(prefix);
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(b"3 0 obj\n<< /Linearized ");
        pdf.extend_from_slice(linearized);
        if let Some(l_literal) = l_literal {
            pdf.extend_from_slice(b" /L ");
            pdf.extend_from_slice(l_literal);
        }
        pdf.extend_from_slice(b" /N 0 /O 0 /H [0 0] /T 0 /P 0 >>\nendobj\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [4 0 R] /Count 1 >>\nendobj\n");
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n1\nendobj\n");
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 6\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n{off5:010} 00000 n \n"
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 6 /Root 2 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    const LINEARIZED_L_MARKER: &[u8] = b"00000000000000000000";

    fn set_linearized_l(bytes: &mut [u8], value: u64) {
        let marker_start = bytes
            .windows(b"/L ".len())
            .position(|window| window == b"/L ")
            .expect("linearization fixture has /L")
            + b"/L ".len();
        let encoded = format!("{value:020}");
        bytes[marker_start..marker_start + LINEARIZED_L_MARKER.len()]
            .copy_from_slice(encoded.as_bytes());
    }

    /// Keep the document readable until lazy resolution reaches object `5`.
    /// This exercises qpdf's damaged-object-to-null behavior without making
    /// the initial xref/trailer open fail.
    struct FailingObjectReader {
        inner: Cursor<Vec<u8>>,
        failure_offset: u64,
    }

    impl Read for FailingObjectReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.inner.position() == self.failure_offset {
                return Err(std::io::Error::other("linearization object read failed"));
            }
            self.inner.read(buffer)
        }
    }

    impl Seek for FailingObjectReader {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    fn failing_at_object_five(bytes: Vec<u8>) -> FailingObjectReader {
        let failure_offset = bytes
            .windows(b"5 0 obj\n".len())
            .position(|window| window == b"5 0 obj\n")
            .expect("linearization fixture has object 5") as u64;
        FailingObjectReader {
            inner: Cursor::new(bytes),
            failure_offset,
        }
    }

    /// qpdf accepts `/Linearized` only when its numeric floor is exactly one.
    /// It also ignores the deeper hint parameters, which belong to
    /// `checkLinearization`, not `isLinearized`.
    #[test]
    fn is_linearized_uses_qpdf_numeric_floor_and_ignores_deeper_parameters() {
        let mut below_one = Pdf::open_mem_owned(linearized_like_pdf_bytes(
            b".9",
            None,
            b"7 8 not-an-object\n",
        ))
        .expect("open .9 PDF");
        assert!(!below_one.is_linearized().expect("check .9"));

        let mut one_or_above =
            Pdf::open_mem_owned(linearized_like_pdf_bytes(b"1.9", None, b"7 8 obj 42\n"))
                .expect("open 1.9 PDF");
        assert!(one_or_above.is_linearized().expect("check 1.9"));
    }

    #[test]
    fn is_linearized_rejects_zero_and_out_of_range_object_numbers() {
        let mut zero = Pdf::open_mem_owned(linearized_like_pdf_bytes(
            b"1",
            None,
            b"0 0 obj << /Linearized 1 >>\nendobj\n",
        ))
        .expect("open zero-object candidate PDF");
        assert!(!zero.is_linearized().expect("zero object number is false"));

        let mut out_of_range = Pdf::open_mem_owned(linearized_like_pdf_bytes(
            b"1",
            None,
            b"4294967296 0 obj << /Linearized 1 >>\nendobj\n",
        ))
        .expect("open out-of-range candidate PDF");
        assert!(!out_of_range
            .is_linearized()
            .expect("out-of-range object number is false"));
    }

    #[test]
    fn is_linearized_rejects_a_non_numeric_linearized_value() {
        let mut pdf = Pdf::open_mem_owned(linearized_like_pdf_bytes(b"true", None, &[]))
            .expect("open non-numeric /Linearized PDF");

        assert!(!pdf
            .is_linearized()
            .expect("non-numeric /Linearized is false"));
    }

    #[test]
    fn is_linearized_uses_first_object_number_instead_of_object_one() {
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/compat/linearized-one-page.pdf"),
        )
        .expect("linearized fixture");
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open linearized fixture");

        assert!(pdf.is_linearized().expect("check linearized fixture"));
    }

    #[test]
    fn is_linearized_requires_an_integer_l_to_match_the_file_size() {
        let mut matching = linearized_like_pdf_bytes(b"1", Some(LINEARIZED_L_MARKER), &[]);
        let matching_length = matching.len() as u64;
        set_linearized_l(&mut matching, matching_length);
        let mut matching = Pdf::open_mem_owned(matching).expect("open matching /L PDF");
        assert!(matching.is_linearized().expect("check matching /L"));

        let mut mismatch = linearized_like_pdf_bytes(b"1", Some(LINEARIZED_L_MARKER), &[]);
        let mismatch_length = mismatch.len() as u64;
        set_linearized_l(&mut mismatch, mismatch_length + 1);
        let mut mismatch = Pdf::open_mem_owned(mismatch).expect("open mismatching /L PDF");
        assert!(!mismatch.is_linearized().expect("check mismatching /L"));

        let mut non_integer_l =
            Pdf::open_mem_owned(linearized_like_pdf_bytes(b"1", Some(b"1.5"), &[]))
                .expect("open non-integer /L PDF");
        assert!(non_integer_l.is_linearized().expect("check non-integer /L"));
    }

    #[test]
    fn is_linearized_rejects_candidates_after_the_first_1024_bytes() {
        let prefix = vec![b'x'; 1024];
        let mut pdf = Pdf::open_mem_owned(linearized_like_pdf_bytes(b"1", None, &prefix))
            .expect("open late-candidate PDF");

        assert!(!pdf
            .is_linearized()
            .expect("late candidate is outside the scan"));
    }

    #[test]
    fn is_linearized_rejects_a_non_dictionary_first_candidate() {
        let mut bytes = linearized_like_pdf_bytes(b"1", None, &[]);
        let marker = b"<< /Linearized";
        let marker_start = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("linearization dictionary marker")
            + b"<<".len();
        bytes[marker_start..marker_start + 2].copy_from_slice(b"42");
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open non-dictionary PDF");

        assert!(!pdf
            .is_linearized()
            .expect("non-dictionary candidate is false"));
    }

    #[test]
    fn is_linearized_returns_false_for_an_unresolvable_first_candidate() {
        let mut pdf = Pdf::open_mem_owned(linearized_like_pdf_bytes(
            b"1",
            None,
            b"99 0 obj\n<< /Linearized 1 >>\nendobj\n",
        ))
        .expect("open unresolved-candidate PDF");

        assert!(!pdf
            .is_linearized()
            .expect("unresolved candidate is not an error"));
    }

    #[test]
    fn is_linearized_returns_false_when_linearized_key_cannot_be_resolved() {
        let reader = failing_at_object_five(linearized_like_pdf_bytes(b"5 0 R", None, &[]));
        let mut pdf = Pdf::open(reader).expect("open indirect /Linearized PDF");

        assert!(!pdf
            .is_linearized()
            .expect("unresolvable /Linearized is not an error"));
    }

    #[test]
    fn is_linearized_returns_false_when_l_key_cannot_be_resolved() {
        let reader = failing_at_object_five(linearized_like_pdf_bytes(b"1", Some(b"5 0 R"), &[]));
        let mut pdf = Pdf::open(reader).expect("open indirect /L PDF");

        assert!(!pdf
            .is_linearized()
            .expect("unresolvable /L is not an error"));
    }

    fn build_linearized_bytes() -> Vec<u8> {
        let raw = tiny_pdf_bytes();
        let mut pdf = Pdf::open(Cursor::new(raw.clone())).unwrap();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
        let renumber = RenumberMap::from_plan(&plan);
        let mut pdf2 = Pdf::open(Cursor::new(raw)).unwrap();
        let mut doc =
            write_linearized(&plan, &renumber, &mut pdf2, &WriterOptions::default()).unwrap();
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

    #[test]
    fn check_consumer_production_uses_the_canonical_object_handle_route() {
        let source = include_str!("check.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("check.rs has a test module")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            "resolve_borrowed(",
            "decode_stream_data(",
            "page_refs(",
            "Object::",
        ] {
            assert!(
                !production.contains(forbidden),
                "check.rs production must not use legacy {forbidden} route"
            );
        }
    }

    // -----------------------------------------------------------------------
    // as_u64: qpdf's deeper parameters require integer handles
    // -----------------------------------------------------------------------
    #[test]
    fn as_u64_rejects_real() {
        let frac = ObjectHandle::real(1.9);
        assert!(
            matches!(
                as_u64(&frac, "N"),
                Err(LinearizationCheckError::InvalidParam { .. })
            ),
            "real /N must be rejected rather than coerced to an integer"
        );
    }

    #[test]
    fn as_u64_rejects_integer_valued_real() {
        let exact = ObjectHandle::real(42.0);
        assert!(matches!(
            as_u64(&exact, "N"),
            Err(LinearizationCheckError::InvalidParam { .. })
        ));
    }

    /// `as_u64` rejects a real literal because qpdf checks `isInteger()` before
    /// reading the deeper linearization parameters.
    #[test]
    fn as_u64_rejects_integer_valued_real_literal() {
        let lit = ObjectHandle::real_literal(42.0, b"42.0".to_vec());
        assert!(matches!(
            as_u64(&lit, "N"),
            Err(LinearizationCheckError::InvalidParam { .. })
        ));
    }

    /// A fractional `RealLiteral` (source literal `.9`) is rejected as a
    /// non-integer qpdf parameter.
    #[test]
    fn as_u64_rejects_fractional_real_literal() {
        let lit = ObjectHandle::real_literal(0.9, b".9".to_vec());
        assert!(matches!(
            as_u64(&lit, "N"),
            Err(LinearizationCheckError::InvalidParam { .. })
        ));
    }

    /// Build a PDF whose object `(1, 0)` is a "linearization dictionary"
    /// whose `/Linearized` value is a non-canonical real literal (`.9`,
    /// stored as [`Object::RealLiteral`]). The dictionary is intentionally
    /// missing the rest of the required keys (`/L`, `/H`, `/O`, `/E`, `/N`,
    /// `/T`), so downstream checks will fail — but the `RealLiteral` arm at
    /// the /Linearized recognition point (check.rs:185-187) is what we care
    /// about for coverage.
    fn linearization_like_pdf_with_real_literal_linearized() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Linearized .9 >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(b"3 0 obj\n<< /Type /Pages /Kids [4 0 R] /Count 1 >>\nendobj\n");
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
        );
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 2 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    /// qpdf's `isLinearized` rejects a real literal below one before the
    /// deeper linearization dictionary checks run.
    #[test]
    fn check_linearization_rejects_real_literal_below_one() {
        let pdf = linearization_like_pdf_with_real_literal_linearized();
        let err = check_linearization_bytes(&pdf)
            .expect_err("stub fixture cannot satisfy a full linearization check"); // cov:ignore: expect_err panic branch only fires on unexpected Ok
        assert!(
            matches!(err, LinearizationCheckError::NotLinearized),
            "qpdf rejects /Linearized .9 before deeper checks (got {err:?})" // cov:ignore: format arg only evaluated on assert failure
        );
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

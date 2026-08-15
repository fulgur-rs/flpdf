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

use crate::optimization::{ObjectUser, Optimization};
use crate::{DecodeLevel, ObjectHandle, ObjectRef, PageDocumentHelper, Pdf, Result, XrefEntry};
use std::collections::BTreeMap;
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
        let Some(object_ref) = self.linearization_candidate_ref()? else {
            return Ok(false);
        };

        let candidate = self.get_object_handle(object_ref);
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

/// The root keys `calculateLinearizationData` routes to part 4 (open
/// document), not part 6, even when also reachable from the first page
/// (`QPDF_linearization.cc:1046-1050,1089-1097`). `/Encrypt` is a trailer
/// key, checked separately in [`is_open_document_user`].
const OPEN_DOCUMENT_ROOT_KEYS: [&[u8]; 5] = [
    b"ViewerPreferences",
    b"PageMode",
    b"Threads",
    b"OpenAction",
    b"AcroForm",
];

/// `QPDF_linearization.cc:1077-1097`'s `in_open_document` predicate.
fn is_open_document_user(user: &ObjectUser) -> bool {
    match user {
        ObjectUser::TrailerKey(key) => key == b"Encrypt",
        ObjectUser::RootKey(key) => OPEN_DOCUMENT_ROOT_KEYS.contains(&key.as_slice()),
        _ => false,
    }
}

/// Return qpdf's source-extent envelope for the objects assigned to part 6.
///
/// qpdf's `checkLinearizationInternal` does not walk the first page's
/// reachability graph directly: it runs the *same* `optimize(object_stream_data,
/// false)` and `calculateLinearizationData()` machinery the writer uses
/// (`QPDF_linearization.cc:483-497`), then measures `m->part6`'s objects'
/// `end_before_space`/`end_after_space` (`:507-521`). Part 6 membership is an
/// object-user classification, not plain reachability: an object reachable
/// from the first page is still routed elsewhere when it is also reachable
/// through `/Outlines` (part 6 only under `/PageMode /UseOutlines`, otherwise
/// part 9 — `in_outlines` outranks `in_first_page`,
/// `QPDF_linearization.cc:1120,1124-1127`) or through one of the open-document
/// root/trailer keys (always part 4, `:1089-1097,1122-1123`).
///
/// This reuses [`crate::optimization::Optimization`], the ported
/// `QPDF_optimization.cc` object-user primitive that already drives the
/// linearization writer (`linearization/plan.rs`), instead of re-deriving a
/// bespoke reachability walk here — the consumer and producer share qpdf's
/// exact classification rules.
fn first_page_source_extent<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<(i64, i64)> {
    let mut object_stream_data = BTreeMap::new();
    for (object_ref, entry) in pdf.get_xref_table() {
        if let XrefEntry::Compressed { stream, .. } = entry {
            object_stream_data.insert(object_ref.number, stream);
        }
    }
    // qpdf's checkLinearizationInternal calls `optimize(object_stream_data,
    // false)` with no `skip_stream_parameters` override, i.e. it always
    // preserves every stream dictionary key while traversing.
    let optimization = Optimization::optimize(pdf, &object_stream_data, false, |_stream| 0u8)?;

    let outlines_in_first_page = {
        let mut use_outlines_with_outlines = false;
        if let Some(root_ref) = pdf.root_ref() {
            let root = pdf.get_object_handle(root_ref);
            root.try_dereference()?;
            if let Some(root_dict) = root.try_as_dictionary()? {
                let page_mode = root_dict.get(b"/PageMode" as &[u8]).cloned();
                let use_outlines = if let Some(page_mode) = page_mode {
                    page_mode.try_dereference()?;
                    page_mode.as_name().as_deref() == Some(b"UseOutlines")
                } else {
                    false
                };
                if use_outlines {
                    use_outlines_with_outlines = root_dict.contains_key(b"/Outlines" as &[u8]);
                }
            } // cov:ignore: llvm maps the root-dictionary cleanup to this brace
        } // cov:ignore: llvm maps the optional root-ref cleanup to this brace
        use_outlines_with_outlines
    };

    let mut max_end_before_space = -1_i64;
    let mut max_end_after_space = -1_i64;

    for (object_ref, users) in optimization.object_users() {
        let is_root = users.contains(&ObjectUser::Root);
        let in_outlines = users
            .iter()
            .any(|user| matches!(user, ObjectUser::RootKey(key) if key == b"Outlines"));
        let in_open_document = users.iter().any(is_open_document_user);
        let in_first_page = users.contains(&ObjectUser::Page(0));

        // `QPDF_linearization.cc:1118-1127`'s priority-ordered classification:
        // is_root > in_outlines > in_open_document > in_first_page. Both
        // `lc_first_page_private` and `lc_first_page_shared` land in part 6,
        // so a plain `in_first_page` boolean captures membership here.
        let in_part6 = if is_root {
            false
        } else if in_outlines {
            outlines_in_first_page
        } else if in_open_document {
            false
        } else {
            in_first_page
        };
        if !in_part6 {
            continue;
        }

        let object = pdf.get_object_handle(object_ref);
        object.try_dereference()?;
        let (end_before_space, end_after_space) = object.end_offsets();
        if end_before_space < 0 || end_after_space < 0 {
            return Err(crate::Error::Unsupported(format!(
                "linearization part-6 object {object_ref:?} has no source extent"
            )));
        }
        max_end_before_space = max_end_before_space.max(end_before_space);
        max_end_after_space = max_end_after_space.max(end_after_space);
    }

    Ok((max_end_before_space, max_end_after_space))
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
    // 1. Reuse qpdf's isLinearized candidate boundary. It scans only the
    //    first 1024 bytes and resolves the candidate through the generation-
    //    zero canonical handle; a full-file byte scan would accept a late
    //    object that qpdf rejects.
    // -----------------------------------------------------------------------
    if !pdf.is_linearized().map_err(LinearizationCheckError::from)? {
        return Err(LinearizationCheckError::NotLinearized);
    }
    let first_obj_ref = pdf
        .linearization_candidate_ref()
        .map_err(LinearizationCheckError::from)?
        .ok_or(LinearizationCheckError::NotLinearized)?;
    let first_obj = pdf.get_object_handle(first_obj_ref);
    let Some(_) = first_obj
        .try_as_dictionary()
        .map_err(LinearizationCheckError::from)?
    else {
        return Err(LinearizationCheckError::NotLinearized);
    };

    // `is_linearized` owns qpdf's `/L` rule: an integer `/L` must match the
    // file size, while a missing or non-integer `/L` is not rejected here.

    // -----------------------------------------------------------------------
    // 2. /N must equal the page count
    // -----------------------------------------------------------------------
    let n_obj = first_obj
        .try_get_key(b"/N")
        .map_err(LinearizationCheckError::from)?;
    let n_val = as_u64(&n_obj, "N")?;
    let pages = PageDocumentHelper::new(pdf)
        .get_all_pages()
        .map_err(|e| LinearizationCheckError::Io(Box::new(e)))?;
    let page_count = pages.len() as u64;
    if n_val != page_count {
        fail!("/N ({n_val}) does not match page count ({page_count})");
    }

    // -----------------------------------------------------------------------
    // 3. /O must point to the first page object
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
        } // cov:ignore: qpdf's malformed no-Type failure returns before this brace
    } // cov:ignore: llvm maps this completed conditional cleanup to an unhit brace

    // qpdf records this as a linearization warning in
    // `checkLinearizationInternal` (`QPDF_linearization.cc:419-427`).  The
    // flpdf checker has a boolean-equivalent error result rather than qpdf's
    // warning accumulator, so surface the same failed-check condition after
    // validating the referenced object itself.  Keeping the object validation
    // first preserves the useful malformed-object diagnostics for a bad /O.
    let Some(first_page_ref) = pages.first().copied() else {
        fail!("/O ({o_num}) cannot be checked because the document has no pages");
    };
    if first_page_ref.number as u64 != o_num {
        fail!(
            "/O ({o_num}) does not match the first page object ({})",
            first_page_ref.number
        );
    }

    // qpdf accepts an omitted or null `/P`, and accepts any integer (including
    // a negative first-page number); only other resolved types are malformed.
    let p_obj = first_obj
        .try_get_key(b"/P")
        .map_err(LinearizationCheckError::from)?;
    p_obj
        .try_dereference()
        .map_err(LinearizationCheckError::from)?;
    if !p_obj.try_is_null().map_err(LinearizationCheckError::from)? && p_obj.as_integer().is_none()
    {
        fail!("/P is present but is neither an integer nor null");
    }

    // -----------------------------------------------------------------------
    // 4. /H — primary and optional overflow hint streams must be readable
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
    let mut check_hint = |index: usize, offset: u64, length: u64| -> CheckResult {
        if offset >= file_len {
            fail!("/H[{index}] offset ({offset}) is beyond file length ({file_len})");
        }
        let end = offset
            .checked_add(length)
            // cov:ignore-start: parsed PDF integers are i64-backed, so two
            // non-negative values cannot overflow a u64 sum.
            .ok_or_else(|| LinearizationCheckError::InvalidParam {
                message: format!("/H[{index}] offset plus length overflows"),
            })?;
        // cov:ignore-end
        if end > file_len {
            fail!(
                "/H[{index}] offset plus length ({offset}+{length}) extends beyond file length ({file_len})"
            );
        }
        let offset = usize::try_from(offset)
            // cov:ignore-start: only a 32-bit target can reject a parsed u64
            // offset here; the supported CI target is 64-bit.
            .map_err(|_| LinearizationCheckError::InvalidParam {
                message: format!("/H[{index}] offset ({offset}) does not fit in platform usize"),
            })?;
        // cov:ignore-end
        check_hint_stream_at_offset(pdf, file_bytes, offset, length)
    };

    let h_offset = as_u64(&h_items[0], "H[0]")?;
    let h_length = as_u64(&h_items[1], "H[1]")?;
    check_hint(0, h_offset, h_length)?;
    if h_items.len() == 4 {
        let overflow_offset = as_u64(&h_items[2], "H[2]")?;
        let overflow_length = as_u64(&h_items[3], "H[3]")?;
        if overflow_offset != 0 {
            check_hint(2, overflow_offset, overflow_length)?;
        } // cov:ignore: llvm maps the overflow closure cleanup to this brace
    }

    // -----------------------------------------------------------------------
    // 5. /E must match the source extent envelope of qpdf's part 6, not merely
    //    be smaller than EOF.
    // -----------------------------------------------------------------------
    let e_obj = first_obj
        .try_get_key(b"/E")
        .map_err(LinearizationCheckError::from)?;
    let e_val = as_u64(&e_obj, "E")?;
    if e_val >= file_len {
        fail!("/E ({e_val}) must be less than file length ({file_len})");
    }
    let (min_e, max_e) = first_page_source_extent(pdf).map_err(LinearizationCheckError::from)?;
    // cov:ignore-start: source extents are rejected as negative in the canonical
    // object walk before this conversion; parsed PDF integers are i64-backed.
    let min_e = u64::try_from(min_e).map_err(|_| LinearizationCheckError::InvalidParam {
        message: format!("computed part-6 end offset {min_e} is negative"),
    })?;
    let max_e = u64::try_from(max_e).map_err(|_| LinearizationCheckError::InvalidParam {
        message: format!("computed part-6 end offset {max_e} is negative"),
    })?;
    // cov:ignore-end
    if e_val < min_e || e_val > max_e {
        fail!("/E ({e_val}) does not match the part-6 source extent range ({min_e}..{max_e})");
    }

    // -----------------------------------------------------------------------
    // 6. /T must be within the last cross-reference *section*.
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

    // qpdf seeks to /T and consumes only PDF whitespace before comparing the
    // resulting position with its exact `first_xref_item_offset`
    // (`QPDF_linearization.cc:452-470`, populated by `QPDF.cc:845-869`).  A
    // backscan that merely finds an earlier `xref` keyword would accept a
    // header position qpdf rejects, so reproduce that cursor movement here.
    let mut first_entry_cursor = t_usize;
    while first_entry_cursor < first_entry_pos && is_pdf_whitespace(file_bytes[first_entry_cursor])
    {
        first_entry_cursor += 1;
    }
    if first_entry_cursor != first_entry_pos {
        fail!(
            "/T ({t_val}) does not point at the whitespace immediately before the \
             first xref item ({first_entry_pos})"
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
    if offset >= file_bytes.len() {
        return Err(crate::Error::Unsupported(format!(
            "/H[0] offset ({offset}) is beyond file length ({})",
            file_bytes.len()
        )));
    }
    let scan_end = offset.saturating_add(SCAN_WINDOW).min(file_bytes.len());
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
    let hint_obj = pdf.resolve_object_handle_at_offset(offset as u64, hint_ref)?;
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
    // cov:ignore-start: the canonical resolver rejects an unresolved indirect
    // `/Length` while parsing the stream, so a parsed stream always has extents.
    if end_before_space < 0 || end_after_space < 0 {
        return Err(crate::Error::Unsupported(format!(
            "hint stream object {obj_num} {obj_gen} has no source extent"
        )));
    }
    // cov:ignore-end
    // cov:ignore-start: u64 -> usize conversion can fail only on 32-bit
    // targets for a value above usize::MAX; the supported CI target is 64-bit.
    let expected_h_length_usize = usize::try_from(expected_h_length).map_err(|_| {
        crate::Error::Unsupported(format!(
            "/H[1] ({expected_h_length}) does not fit in platform usize"
        ))
    })?;
    // cov:ignore-end
    let computed_end = offset.checked_add(expected_h_length_usize).ok_or_else(|| {
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
#[cfg(test)]
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

/// Test-only fixture transform for qpdf's otherwise rarely emitted four-item
/// `/H` form. The linearization writer reserves whitespace after the parameter
/// dictionary; consume that padding while inserting a second copy of the
/// primary stream's offset/length, so every existing object and xref offset
/// remains unchanged.
#[cfg(test)]
pub(crate) fn add_overflow_hint_items_for_test(mut bytes: Vec<u8>) -> Vec<u8> {
    fn token_range(bytes: &[u8], mut cursor: usize) -> (usize, usize) {
        while cursor < bytes.len() && is_pdf_whitespace(bytes[cursor]) {
            cursor += 1;
        }
        let start = cursor;
        while cursor < bytes.len() && !is_pdf_whitespace(bytes[cursor]) && bytes[cursor] != b']' {
            cursor += 1;
        }
        (start, cursor)
    }

    let h_key = b"/H [";
    let h_key_start = bytes
        .windows(h_key.len())
        .position(|window| window == h_key)
        .expect("linearization parameter dictionary has /H array");
    let h_values_start = h_key_start + h_key.len();
    let (h0_start, h0_end) = token_range(&bytes, h_values_start);
    let (h1_start, h1_end) = token_range(&bytes, h0_end);
    let close = h1_end
        + bytes[h1_end..]
            .iter()
            .position(|&byte| byte == b']')
            .expect("linearization /H array terminator");
    let param_end = close
        + bytes[close..]
            .windows(b"endobj".len())
            .position(|window| window == b"endobj")
            .expect("linearization parameter endobj")
        + b"endobj".len();

    let insertion = {
        let mut value = Vec::with_capacity(2 + h0_end - h0_start + h1_end - h1_start);
        value.push(b' ');
        value.extend_from_slice(&bytes[h0_start..h0_end]);
        value.push(b' ');
        value.extend_from_slice(&bytes[h1_start..h1_end]);
        value
    };
    let delta = insertion.len();
    let padding_boundary = param_end
        + bytes[param_end..]
            .iter()
            .position(|&byte| !is_pdf_whitespace(byte))
            .expect("non-whitespace after the parameter dictionary");
    assert!(padding_boundary >= delta, "parameter padding is too short");
    assert!(
        bytes[padding_boundary - delta..padding_boundary]
            .iter()
            .all(|&byte| is_pdf_whitespace(byte)),
        "parameter dictionary has no trailing padding for /H overflow fixture"
    );

    bytes.splice(close..close, insertion);
    assert!(
        bytes[padding_boundary..padding_boundary + delta]
            .iter()
            .all(|&byte| is_pdf_whitespace(byte)),
        "inserted /H overflow fixture did not land in parameter padding"
    );
    bytes.drain(padding_boundary..padding_boundary + delta);
    bytes
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
        pdf.extend_from_slice(
            b" /N 0 /O 0 /H [00000000000000000000 00000000000000000000] /T 0 /P 0 >>\nendobj\n",
        );
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
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
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

    /// Classic-mode (no object streams) linearize of a real
    /// `tests/fixtures/compat/` fixture, exercised entirely through the
    /// library's own plan/renumber/write/back_patch pipeline — the same
    /// machinery `flpdf rewrite --linearize` drives, verified byte-identical
    /// to `qpdf --linearize` for these fixtures.
    fn linearize_classic_fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat")
            .join(name);
        let raw = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        let mut pdf =
            Pdf::open(Cursor::new(raw.clone())).unwrap_or_else(|e| panic!("open {name}: {e}"));
        let plan = LinearizationPlan::from_pdf(&mut pdf, false)
            .unwrap_or_else(|e| panic!("plan {name}: {e}"));
        let renumber = RenumberMap::from_plan(&plan);
        let mut pdf2 = Pdf::open(Cursor::new(raw)).unwrap_or_else(|e| panic!("reopen {name}: {e}"));
        let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &WriterOptions::default())
            .unwrap_or_else(|e| panic!("write {name}: {e}"));
        doc.back_patch()
            .unwrap_or_else(|e| panic!("back_patch {name}: {e}"));
        doc.bytes
    }

    // Regression coverage for the P1 finding on the part-6 extent walk
    // (`first_page_source_extent`): a hand-rolled first-page reachability
    // walk is not qpdf's actual part-6 object-user classification
    // (`QPDF_linearization.cc:1063-1139`). Confirmed empirically: `qpdf
    // --check-linearization` reports "no linearization errors" on qpdf's own
    // (byte-identical to flpdf's) linearized output for each fixture below,
    // while the walk this replaces rejected them with a computed /E range
    // that excluded the real /E.
    #[test]
    fn check_linearization_passes_for_outlines_shared_page_fixture() {
        // objstm-lin-outlines-shared-page-80-80.pdf: one font is referenced
        // by both the first page and an /Outlines item. qpdf's `in_outlines`
        // classification outranks `in_first_page`
        // (`QPDF_linearization.cc:1120`), so without /PageMode /UseOutlines
        // that font is NOT part 6. The former graph walk had no notion of
        // this priority and pulled the font (and everything after it) into
        // its part-6 extent, producing a range that excluded the real /E.
        let bytes = linearize_classic_fixture("objstm-lin-outlines-shared-page-80-80.pdf");
        let result = check_linearization_bytes(&bytes);
        assert!(
            result.is_ok(),
            "qpdf-faithful linearized output must pass check_linearization: {result:?}"
        );
    }

    #[test]
    fn check_linearization_passes_for_thumb_first_edge_wins_fixture() {
        // objstm-lin-thumb-first-edge-wins.pdf: an object reachable from the
        // first page is also a page thumbnail; qpdf's classification still
        // routes it through in_first_page once thumbs are counted, but the
        // former graph walk's ad hoc /Thumb handling diverged.
        let bytes = linearize_classic_fixture("objstm-lin-thumb-first-edge-wins.pdf");
        let result = check_linearization_bytes(&bytes);
        assert!(
            result.is_ok(),
            "qpdf-faithful linearized output must pass check_linearization: {result:?}"
        );
    }

    #[test]
    fn check_linearization_passes_for_null_visible_thumb_first_edge_fixture() {
        let bytes = linearize_classic_fixture("null-visible-thumb-first-edge.pdf");
        let result = check_linearization_bytes(&bytes);
        assert!(
            result.is_ok(),
            "qpdf-faithful linearized output must pass check_linearization: {result:?}"
        );
    }

    // Regression coverage for the P1 finding on `enqueue_references`'s
    // caller (check.rs:268 in the pre-fix code): when the first page's
    // /Contents is an INDIRECT reference to an array of further indirect
    // content streams, the array branch was never reached because
    // `object.try_as_dictionary()` returns `None` for arrays too, and the
    // fallback only handled streams. Reusing `crate::optimization::
    // Optimization` (whose traversal already threads through `Object::Array`
    // regardless of how it was reached, `optimization.rs:271-281`) fixes
    // this as a side effect of fixing the classification itself.
    #[test]
    fn check_linearization_passes_for_indirect_contents_array_fixture() {
        let bytes = linearize_classic_fixture("qdf-contents-ref-array.pdf");
        let result = check_linearization_bytes(&bytes);
        assert!(
            result.is_ok(),
            "qpdf-faithful linearized output must pass check_linearization: {result:?}"
        );
    }

    fn linearized_fixture_bytes() -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/compat/linearized-one-page.pdf"),
        )
        .expect("linearized fixture")
    }

    /// Replace a parameter value without moving any later PDF offsets. PDF
    /// whitespace after a scalar or array is insignificant, so short test
    /// values are padded to the original byte span.
    fn replace_parameter_value(bytes: &mut [u8], key: &[u8], replacement: &[u8]) {
        let key_start = bytes
            .windows(key.len())
            .position(|window| window == key)
            .expect("parameter key");
        let mut start = key_start + key.len();
        while start < bytes.len() && is_pdf_whitespace(bytes[start]) {
            start += 1;
        }
        let end = if bytes[start] == b'[' {
            start
                + bytes[start..]
                    .iter()
                    .position(|&byte| byte == b']')
                    .expect("array terminator")
                + 1
        } else {
            start
                + bytes[start..]
                    .iter()
                    .position(|&byte| is_pdf_whitespace(byte))
                    .expect("scalar terminator")
        };
        assert!(replacement.len() <= end - start);
        let mut padded = vec![b' '; end - start];
        padded[..replacement.len()].copy_from_slice(replacement);
        bytes[start..end].copy_from_slice(&padded);
    }

    fn replace_parameter_number(bytes: &mut [u8], key: &[u8], value: usize) {
        let key_start = bytes
            .windows(key.len())
            .position(|window| window == key)
            .expect("numeric parameter key");
        let mut start = key_start + key.len();
        while start < bytes.len() && is_pdf_whitespace(bytes[start]) {
            start += 1;
        }
        let end = start
            + bytes[start..]
                .iter()
                .position(|&byte| is_pdf_whitespace(byte))
                .expect("numeric parameter terminator");
        let width = end - start;
        let digits = value.to_string();
        assert!(
            digits.len() <= width,
            "replacement {value} does not fit parameter {key:?} width {width}"
        );
        let mut padded = vec![b'0'; width];
        padded[width - digits.len()..].copy_from_slice(digits.as_bytes());
        bytes[start..end].copy_from_slice(&padded);
    }

    fn object_offset(bytes: &[u8], object_number: u32) -> usize {
        let header = format!("{object_number} 0 obj");
        bytes
            .windows(header.len())
            .position(|window| window == header.as_bytes())
            .expect("object header")
    }

    fn indirect_length_hint_fixture() -> (Vec<u8>, usize, u64) {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.4\n");
        let stream_offset = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Length 2 0 R >>\nstream\nabc\nendstream\nendobj\n");
        let length_offset = bytes.len();
        bytes.extend_from_slice(b"2 0 obj\n3\nendobj\n");
        let catalog_offset = bytes.len();
        bytes.extend_from_slice(b"3 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref_offset = bytes.len();
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{stream_offset:010} 00000 n \n{length_offset:010} 00000 n \n{catalog_offset:010} 00000 n \n"
        );
        bytes.extend_from_slice(xref.as_bytes());
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 3 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        let length_end = bytes
            .windows(b"2 0 obj\n3\nendobj".len())
            .position(|window| window == b"2 0 obj\n3\nendobj")
            .expect("length object")
            + b"2 0 obj\n3\nendobj".len();
        let expected_length = (length_end + 1 - stream_offset) as u64;
        (bytes, stream_offset, expected_length)
    }

    /// Build a small object graph that exercises qpdf's part-6 object-user
    /// boundary: `/PageMode /UseOutlines` adds the outline graph, while a
    /// nested `/Type /Page` object is counted but not traversed further.
    fn source_extent_graph_fixture() -> Vec<u8> {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let objects = [
            (
                1,
                b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /PageMode /UseOutlines /Outlines 6 0 R >>\nendobj\n"
                    .as_slice(),
            ),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".as_slice(),
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Extra 4 0 R >>\nendobj\n"
                    .as_slice(),
            ),
            (
                4,
                b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Leaf 7 0 R >>\nendobj\n"
                    .as_slice(),
            ),
            (
                5,
                b"5 0 obj\n<< /Type /Example /Leaf 7 0 R >>\nendobj\n".as_slice(),
            ),
            (
                6,
                b"6 0 obj\n<< /Type /Outlines /First 7 0 R >>\nendobj\n".as_slice(),
            ),
            (7, b"7 0 obj\n(leaf)\nendobj\n".as_slice()),
        ];
        let mut offsets = [0_u64; 8];
        for (number, object) in objects {
            offsets[number] = bytes.len() as u64;
            bytes.extend_from_slice(object);
        }
        let xref_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"xref\n0 8\n0000000000 65535 f \n");
        for offset in offsets.iter().skip(1) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        bytes
    }

    fn replace_h_values(bytes: &mut [u8], offset: u64, length: u64) {
        let replacement = format!("[{offset:020} {length:020}]");
        replace_parameter_value(bytes, b"/H ", replacement.as_bytes());
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
    fn extent_walk_follows_outlines_and_stops_at_nested_pages() {
        let bytes = source_extent_graph_fixture();
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open extent graph");
        let (end_before_space, end_after_space) =
            first_page_source_extent(&mut pdf).expect("source extent");
        assert!(end_before_space > 0);
        assert!(end_after_space >= end_before_space);
    }

    #[test]
    fn extent_walk_rejects_a_missing_object_source_extent() {
        let bytes = source_extent_graph_fixture();
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open extent graph");
        // Materialize a fresh object with no physical source extent and route
        // it into part 6 through /Outlines (the fixture already carries
        // /PageMode /UseOutlines). This mirrors how qpdf's own optimize() can
        // hand calculateLinearizationData an object with no parsed byte
        // position at all — e.g. a non-indirect /Outlines dictionary qpdf
        // forces indirect before classification (`QPDF_optimization.cc:67-76`).
        let injected_ref = ObjectRef::new(50, 0);
        pdf.set_object(
            injected_ref,
            crate::Object::Dictionary(crate::Dictionary::new()),
        );
        let root_ref = ObjectRef::new(1, 0);
        let mut root = match pdf.resolve(root_ref).expect("resolve root") {
            crate::Object::Dictionary(dict) => dict,
            other => panic!("expected root dictionary, got {other:?}"),
        };
        root.insert("Outlines", crate::Object::Reference(injected_ref));
        pdf.set_object(root_ref, crate::Object::Dictionary(root));

        let error = first_page_source_extent(&mut pdf)
            .expect_err("materialized object has no source extent");
        assert!(
            matches!(error, crate::Error::Unsupported(message) if message.contains("no source extent"))
        );
    }

    #[test]
    fn check_rejects_an_empty_page_tree_and_a_mismatched_page_object() {
        let mut empty_pages = linearized_like_pdf_bytes(b"1", None, &[]);
        replace_parameter_value(&mut empty_pages, b"/O", b"4");
        let page_tree = b"/Kids [4 0 R] /Count 1";
        let empty_page_tree = b"/Kids [] /Count 0";
        let page_tree_start = empty_pages
            .windows(page_tree.len())
            .position(|window| window == page_tree)
            .expect("page tree");
        let mut padded = vec![b' '; page_tree.len()];
        padded[..empty_page_tree.len()].copy_from_slice(empty_page_tree);
        empty_pages[page_tree_start..page_tree_start + page_tree.len()].copy_from_slice(&padded);
        let empty_result = check_linearization_bytes(&empty_pages);
        assert!(matches!(
            empty_result,
            Err(LinearizationCheckError::InvalidParam { ref message })
                if message.contains("document has no pages")
        ));

        let mut mismatch = linearized_fixture_bytes();
        replace_parameter_value(&mut mismatch, b"/O ", b"8");
        let type_marker = b"/Type /Font";
        let type_start = mismatch
            .windows(type_marker.len())
            .rposition(|window| window == type_marker)
            .expect("font type");
        mismatch[type_start..type_start + type_marker.len()].copy_from_slice(b"/Type /Page");
        let mismatch_result = check_linearization_bytes(&mismatch);
        assert!(matches!(
            mismatch_result,
            Err(LinearizationCheckError::InvalidParam { ref message })
                if message.contains("does not match the first page object")
        ));
    }

    #[test]
    fn check_rejects_hint_offsets_outside_the_file() {
        let mut beyond = linearized_like_pdf_bytes(b"1", None, &[]);
        replace_parameter_value(&mut beyond, b"/N ", b"1");
        replace_parameter_value(&mut beyond, b"/O ", b"4");
        replace_h_values(&mut beyond, i64::MAX as u64, 0);
        let beyond_result = check_linearization_bytes(&beyond);
        assert!(matches!(
            beyond_result,
            Err(LinearizationCheckError::InvalidParam { ref message })
                if message.contains("is beyond file length")
        ));

        let mut extends = linearized_like_pdf_bytes(b"1", None, &[]);
        replace_parameter_value(&mut extends, b"/N ", b"1");
        replace_parameter_value(&mut extends, b"/O ", b"4");
        replace_h_values(&mut extends, 1, i64::MAX as u64);
        let extends_result = check_linearization_bytes(&extends);
        assert!(matches!(
            extends_result,
            Err(LinearizationCheckError::InvalidParam { ref message })
                if message.contains("extends beyond file length")
        ));
    }

    #[test]
    fn check_reads_the_four_item_overflow_hint_stream() {
        let bytes = add_overflow_hint_items_for_test(linearized_fixture_bytes());
        let result = check_linearization_bytes(&bytes);
        assert!(
            result.is_ok(),
            "check should load a non-zero overflow hint stream: {result:?}"
        );
    }

    #[test]
    fn check_rejects_null_and_non_dictionary_first_page_objects() {
        let mut missing = linearized_fixture_bytes();
        replace_parameter_value(&mut missing, b"/O ", b"0");
        assert!(matches!(
            check_linearization_bytes(&missing),
            Err(LinearizationCheckError::InvalidParam { ref message })
                if message.contains("non-existent object")
        ));

        let mut stream = linearized_fixture_bytes();
        replace_parameter_value(&mut stream, b"/O ", b"5");
        assert!(matches!(
            check_linearization_bytes(&stream),
            Err(LinearizationCheckError::InvalidParam { ref message })
                if message.contains("does not refer to a dictionary")
        ));
    }

    #[test]
    fn check_rejects_wrong_page_type_and_unstructured_page_dictionary() {
        let mut wrong_type = linearized_fixture_bytes();
        replace_parameter_value(&mut wrong_type, b"/O ", b"4");
        assert!(matches!(
            check_linearization_bytes(&wrong_type),
            Err(LinearizationCheckError::InvalidParam { ref message })
                if message.contains("instead of /Page")
        ));

        let mut unstructured = linearized_fixture_bytes();
        replace_parameter_value(&mut unstructured, b"/O ", b"2");
        assert!(matches!(
            check_linearization_bytes(&unstructured),
            Err(LinearizationCheckError::InvalidParam { ref message })
                if message.contains("does not look like a Page object")
        ));

        // qpdf also accepts a page whose /Type is absent when /Parent and
        // /MediaBox provide the structural page shape.
        let mut inherited = linearized_fixture_bytes();
        let type_marker = b"/Type /Page";
        let type_start = inherited
            .windows(type_marker.len())
            .rposition(|window| window == type_marker)
            .expect("first page type");
        inherited[type_start..type_start + type_marker.len()].copy_from_slice(b"/Type null ");
        let inherited_result = check_linearization_bytes(&inherited);
        // cov:ignore-start: the assertion deliberately proves that the
        // NotLinearized arm is not matched by this malformed-but-linearized input.
        assert!(!matches!(
            inherited_result,
            Err(LinearizationCheckError::NotLinearized)
        ));
        // cov:ignore-end
    }

    #[test]
    fn check_rejects_bad_hint_shape_and_offset() {
        let mut not_array = linearized_fixture_bytes();
        replace_parameter_value(&mut not_array, b"/H ", b"0");
        assert!(matches!(
            check_linearization_bytes(&not_array),
            Err(LinearizationCheckError::InvalidParam { ref message })
                if message.contains("/H is missing or has unexpected format")
        ));

        let mut wrong_cardinality = linearized_fixture_bytes();
        replace_parameter_value(&mut wrong_cardinality, b"/H ", b"[0 0 0]");
        assert!(matches!(
            check_linearization_bytes(&wrong_cardinality),
            Err(LinearizationCheckError::InvalidParam { ref message })
                if message.contains("wrong number of items")
        ));

        let mut bad_offset = linearized_fixture_bytes();
        replace_parameter_number(&mut bad_offset, b"/H [", 0);
        assert!(matches!(
            check_linearization_bytes(&bad_offset),
            Err(LinearizationCheckError::InvalidParam { ref message })
                if message.contains("does not point at an indirect object header")
        ));
    }

    #[test]
    fn check_rejects_a_non_stream_hint_object_and_non_xref_t_object() {
        let bytes = linearized_fixture_bytes();
        let mut non_stream_hint = bytes.clone();
        replace_parameter_number(&mut non_stream_hint, b"/H [", object_offset(&bytes, 4));
        assert!(matches!(
            check_linearization_bytes(&non_stream_hint),
            Err(LinearizationCheckError::InvalidParam { ref message })
                if message.contains("is not a stream")
        ));

        let mut non_xref = bytes;
        let non_xref_target = object_offset(&non_xref, 4);
        replace_parameter_number(&mut non_xref, b"/T ", non_xref_target);
        assert!(matches!(
            check_linearization_bytes(&non_xref),
            Err(LinearizationCheckError::InvalidParam { ref message })
                if message.contains("cross-reference stream")
        ));
    }

    #[test]
    fn check_rejects_an_e_value_outside_the_part6_source_extent() {
        let mut bytes = linearized_fixture_bytes();
        let beyond_part6 = bytes.len() - 1;
        replace_parameter_number(&mut bytes, b"/E ", beyond_part6);
        let result = check_linearization_bytes(&bytes);
        assert!(
            matches!(
                result,
                Err(LinearizationCheckError::InvalidParam { ref message })
                    if message.contains("does not match the part-6 source extent range")
            ),
            "an /E outside qpdf's source extent must be rejected: {result:?}"
        );
    }

    #[test]
    fn check_rejects_a_classic_t_value_that_is_not_the_pre_entry_whitespace() {
        let mut bytes = linearized_fixture_bytes();
        let xref_offset = bytes
            .windows(b"xref\n0".len())
            .rposition(|window| window == b"xref\n0")
            .expect("classic xref section");
        replace_parameter_number(&mut bytes, b"/T ", xref_offset);
        let result = check_linearization_bytes(&bytes);
        assert!(
            matches!(
                result,
                Err(LinearizationCheckError::InvalidParam { ref message })
                    if message.contains("does not point at the whitespace immediately before")
            ),
            "qpdf rejects /T at the xref keyword itself: {result:?}"
        );
    }

    #[test]
    fn load_hint_stream_rejects_bad_offsets_and_checks_indirect_length_extent() {
        let bytes = linearized_fixture_bytes();
        let mut pdf = Pdf::open_mem_owned(bytes.clone()).expect("open fixture");
        assert!(matches!(
            load_hint_stream(&mut pdf, &bytes, 0, 0),
            Err(crate::Error::Unsupported(message))
                if message.contains("does not point at an indirect object header")
        ));

        let mut bytes_with_missing = bytes.clone();
        bytes_with_missing.extend_from_slice(b"% 99 0 obj\n");
        let mut pdf = Pdf::open_mem_owned(bytes.clone()).expect("open fixture");
        assert!(matches!(
            load_hint_stream(&mut pdf, &bytes_with_missing, bytes_with_missing.len(), 0),
            Err(crate::Error::Unsupported(message))
                if message.contains("beyond file length")
        ));

        let (indirect_length, offset, expected_length) = indirect_length_hint_fixture();
        let mut pdf = Pdf::open_mem_owned(indirect_length.clone()).expect("open length fixture");
        let (dict, decoded) = load_hint_stream(&mut pdf, &indirect_length, offset, expected_length)
            .expect("indirect /Length hint stream");
        assert!(dict.try_get_key(b"/Length").unwrap().object_ref().is_some());
        assert_eq!(decoded.as_slice(), b"abc");

        let mut null_object = indirect_length.clone();
        let object_header = b"1 0 obj\n";
        let object_start = null_object
            .windows(object_header.len())
            .position(|window| window == object_header)
            .expect("hint object header");
        let body_start = object_start + object_header.len();
        let endobj_start = null_object[body_start..]
            .windows(b"endobj".len())
            .position(|window| window == b"endobj")
            .map(|relative| body_start + relative)
            .expect("hint object endobj");
        null_object[body_start..endobj_start].fill(b' ');
        null_object[body_start..body_start + b"null".len()].copy_from_slice(b"null");
        let mut pdf = Pdf::open_mem_owned(null_object.clone()).expect("open null hint object");
        let null_result = load_hint_stream(&mut pdf, &null_object, offset, expected_length);
        assert!(matches!(
            null_result,
            Err(crate::Error::Unsupported(ref message))
                if message.contains("does not exist")
        ));

        let mut pdf = Pdf::open_mem_owned(bytes.clone()).expect("open fixture");
        let mismatch = load_hint_stream(&mut pdf, &bytes, object_offset(&bytes, 5), 116);
        assert!(
            matches!(
                mismatch,
                Err(crate::Error::Unsupported(ref message))
                    if message.contains("does not match hint stream object span")
            ),
            "unexpected hint span result: {mismatch:?}"
        );

        let mut pdf = Pdf::open_mem_owned(bytes.clone()).expect("open fixture");
        let overflow = load_hint_stream(&mut pdf, &bytes, object_offset(&bytes, 5), u64::MAX);
        assert!(
            matches!(
                overflow,
                Err(crate::Error::Unsupported(ref message))
                    if message.contains("overflows")
            ),
            "unexpected hint offset overflow result: {overflow:?}"
        );

        // The physical header is authoritative for qpdf's readObjectAtOffset
        // path.  Deliberately point the effective xref row for object 5 at the
        // catalog while leaving its real header at /H[0]; the canonical
        // physical resolver must still load the Flate hint stream.
        let mut xref_mismatch = bytes.clone();
        let hint_offset = object_offset(&xref_mismatch, 5);
        let old_entry = format!("{hint_offset:010} 00000 n ");
        let wrong_entry = format!("{:010} 00000 n ", object_offset(&xref_mismatch, 4));
        let entry_start = xref_mismatch
            .windows(old_entry.len())
            .position(|window| window == old_entry.as_bytes())
            .expect("object 5 xref entry");
        xref_mismatch[entry_start..entry_start + old_entry.len()]
            .copy_from_slice(wrong_entry.as_bytes());
        let mut pdf = Pdf::open_mem_owned(xref_mismatch.clone()).expect("open mismatched xref");
        let (_, decoded) = load_hint_stream(&mut pdf, &xref_mismatch, hint_offset, 118)
            .expect("physical hint header must win over the stale xref row");
        assert!(!decoded.is_empty());
    }

    #[test]
    fn check_maps_hint_stream_failures_to_invalid_param_and_io() {
        let bytes = linearized_fixture_bytes();
        let mut invalid_offset = bytes.clone();
        replace_parameter_number(&mut invalid_offset, b"/H [", 0);
        let mut pdf = Pdf::open_mem_owned(invalid_offset.clone()).expect("open invalid fixture");
        assert!(matches!(
            check_hint_stream_at_offset(&mut pdf, &invalid_offset, 0, 0),
            Err(LinearizationCheckError::InvalidParam { .. })
        ));

        let failure_offset = object_offset(&bytes, 5) as u64;
        let reader = FailingObjectReader {
            inner: Cursor::new(bytes.clone()),
            failure_offset,
        };
        let mut pdf = Pdf::open(reader).expect("open lazy fixture");
        assert!(matches!(
            check_hint_stream_at_offset(&mut pdf, &bytes, failure_offset as usize, 118),
            Err(LinearizationCheckError::Io(_))
        ));
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
            matches!(result, Err(LinearizationCheckError::NotLinearized)),
            "tampered /L must yield NotLinearized, got {result:?}"
        );
    }

    #[test]
    fn check_rejects_a_non_integer_p_parameter() {
        let mut bytes = build_linearized_bytes();
        // qpdf permits a missing /P, but readLinearizationData rejects a
        // present value that is neither an integer nor null. Reuse the
        // fixed-width /L slot so no later physical offsets need repair.
        let key_start = bytes
            .windows(b"/L ".len())
            .position(|window| window == b"/L ")
            .expect("linearized output must contain /L");
        bytes[key_start + 1] = b'P';
        replace_parameter_value(&mut bytes, b"/P ", b"/X");

        let result = check_linearization_bytes(&bytes);
        assert!(
            matches!(
                result,
                Err(LinearizationCheckError::InvalidParam { ref message })
                    if message.contains("/P is present but is neither an integer nor null")
            ),
            "non-integer /P must be invalid, got {result:?}"
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

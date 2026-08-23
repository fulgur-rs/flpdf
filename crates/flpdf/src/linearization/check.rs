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
//! | `/H`  | Primary/overflow streams decode and Page/Shared/Outline tables match qpdf's computed layout |
//! | `/E`  | Value is less than the file length and matches qpdf's part-6 extent envelope |
//! | `/T`  | Whitespace from the byte offset reaches qpdf's first xref item |
//!
//! # Exit semantics (used by CLI)
//!
//! The function returns a `LinearizationCheckResult`:
//! - `Ok(())` — all checks passed
//! - `Err(LinearizationCheckError::NotLinearized)` — the first object in the
//!   file (physical position, not object number) has no `/Linearized` key
//! - `Err(LinearizationCheckError::InvalidParam { … })` — a param-dict invariant failed
//! - `Err(LinearizationCheckError::Io(…))` — I/O failure reading the file

use super::show::{
    read_h_generic, read_h_page_offset, read_h_shared_object, read_hint_offsets, HGeneric,
    HPageOffset, HSharedObject, ShowLinearizationError,
};
use crate::optimization::{ObjectUser, Optimization};
use crate::{DecodeLevel, ObjectHandle, ObjectRef, PageDocumentHelper, Pdf, Result, XrefEntry};
use std::collections::{BTreeMap, BTreeSet};
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

/// Outcome of qpdf's linearization-parameter loading phase for the generic
/// document check.
///
/// `QPDF::readLinearizationData` (`QPDF_linearization.cc:161-230`) runs before
/// `checkLinearizationInternal` and reports malformed parameter dictionaries as
/// warnings. A first-page-object mismatch is a later soft warning
/// (`QPDF_linearization.cc:419-433`), not a malformed-dictionary error. Keep
/// this small preflight separate from [`check_linearization`], whose stricter
/// structural checks are also used by `check-linearization`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinearizationParameterCheck {
    Clean,
    Warning(&'static str),
    Error(&'static str),
}

/// Replay the `/N`, `/O`, and `/P` responsibility boundary used by qpdf's
/// generic document check.
///
/// The required-key type gate follows qpdf's all-keys check, so a malformed
/// `/P` reports the same dictionary-level diagnostic as malformed `/H`, `/O`,
/// `/E`, `/N`, or `/T`. `/N` is checked against the page tree while loading
/// linearization data; `/O` is compared to the first page only after that load
/// succeeds. Hint-stream internals remain owned by [`check_linearization`]'s
/// separate strict route.
pub(crate) fn check_linearization_parameters<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> Result<LinearizationParameterCheck> {
    let Some(object_ref) = pdf.linearization_candidate_ref()? else {
        return Ok(LinearizationParameterCheck::Clean); // cov:ignore: the caller already accepted the same linearization candidate
    };
    let candidate = pdf.get_object_handle(object_ref);
    let Some(_) = candidate.try_as_dictionary()? else {
        return Ok(LinearizationParameterCheck::Clean);
    };

    let h = candidate.try_get_key(b"/H")?;
    let o = candidate.try_get_key(b"/O")?;
    let e = candidate.try_get_key(b"/E")?;
    let n = candidate.try_get_key(b"/N")?;
    let t = candidate.try_get_key(b"/T")?;
    let p = candidate.try_get_key(b"/P")?;

    let h_is_array = resolved_is_array(&h)?;
    let o_is_integer = resolved_is_integer(&o)?;
    let e_is_integer = resolved_is_integer(&e)?;
    let n_is_integer = resolved_is_integer(&n)?;
    let t_is_integer = resolved_is_integer(&t)?;
    let p_is_valid = {
        p.try_dereference()?;
        p.try_is_null()? || p.as_integer().is_some()
    };
    if !(h_is_array && o_is_integer && e_is_integer && n_is_integer && t_is_integer && p_is_valid) {
        return Ok(LinearizationParameterCheck::Error(
            "linearization dictionary: some keys in linearization dictionary are of the wrong type",
        ));
    }

    let pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    let page_count = pages.len() as i64;
    if n.as_integer() != Some(page_count) {
        return Ok(LinearizationParameterCheck::Error(
            "linearization hint table: /N does not match number of pages",
        ));
    }

    let Some(first_page) = pages.first() else {
        return Ok(LinearizationParameterCheck::Clean);
    };
    if o.as_integer() != Some(first_page.number as i64) {
        return Ok(LinearizationParameterCheck::Warning(
            "first page object (/O) mismatch",
        ));
    }

    Ok(LinearizationParameterCheck::Clean)
}

fn resolved_is_array(handle: &ObjectHandle) -> Result<bool> {
    handle.try_dereference()?;
    Ok(handle.as_array().is_some())
}

fn resolved_is_integer(handle: &ObjectHandle) -> Result<bool> {
    handle.try_dereference()?;
    Ok(handle.as_integer().is_some())
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
        _ => {
            let type_name = obj.type_name().map_err(LinearizationCheckError::from)?;
            Err(LinearizationCheckError::InvalidParam {
                message: format!("/{key} is not a non-negative integer (got {type_name})"),
            })
        }
    }
}

fn map_show_error(error: ShowLinearizationError) -> LinearizationCheckError {
    match error {
        ShowLinearizationError::Malformed { message } => {
            LinearizationCheckError::InvalidParam { message }
        }
        ShowLinearizationError::Io(error) => LinearizationCheckError::Io(error),
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
    let optimization = Optimization::optimize(pdf, &object_stream_data, false, |_, _stream| 0u8)?;

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
            // qpdf's `checkLinearizationInternal` initializes both `/E`
            // envelope values to -1 and applies `std::max` to every part-6
            // object (`QPDF_linearization.cc:507-521`). A programmatic
            // `QPDF::replaceObject` therefore contributes no source extent;
            // it does not make the whole check fail. Keep the same sentinel
            // semantics after `Pdf::set_object` clears its cache extent.
            continue;
        }
        max_end_before_space = max_end_before_space.max(end_before_space);
        max_end_after_space = max_end_after_space.max(end_after_space);
    }

    Ok((max_end_before_space, max_end_after_space))
}

/// The portions of `calculateLinearizationData` needed by qpdf's hint-table
/// checks.  The object-user map is the authority here: linearization hint
/// tables describe qpdf's classified object order, not an arbitrary graph walk
/// from each page (`QPDF_linearization.cc:1020-1150`).
struct ComputedHintData {
    optimization: Optimization,
    page_object_counts: Vec<u32>,
    page_shared_objects: Vec<Vec<ObjectRef>>,
    part8_objects: Vec<ObjectRef>,
    outline_root: Option<ObjectRef>,
    outline_objects: BTreeSet<ObjectRef>,
}

fn uncompressed_object_ref(
    object_ref: ObjectRef,
    xref: &BTreeMap<ObjectRef, XrefEntry>,
) -> ObjectRef {
    match xref.get(&object_ref) {
        Some(XrefEntry::Compressed { stream, .. }) => ObjectRef::new(*stream, 0),
        _ => object_ref,
    }
}

fn root_outlines_ref<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Option<ObjectRef>> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(None);
    };
    let root = pdf.get_object_handle(root_ref);
    root.try_dereference()?;
    let outlines = root.try_get_key(b"/Outlines")?;
    Ok(outlines.object_ref().or_else(|| outlines.as_reference()))
}

fn outlines_in_first_page<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<bool> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(false);
    };
    let root = pdf.get_object_handle(root_ref);
    root.try_dereference()?;
    let page_mode = root.try_get_key(b"/PageMode")?;
    page_mode.try_dereference()?;
    let outlines = root.try_get_key(b"/Outlines")?;
    outlines.try_dereference()?;
    let has_outlines = !outlines.try_is_null()?;
    Ok(page_mode.as_name().as_deref() == Some(b"UseOutlines") && has_outlines)
}

/// Reproduce the ordering and page/shared membership inputs that qpdf creates
/// before `checkHPageOffset`, `checkHSharedObject`, and `checkHOutlines`.
fn compute_hint_data<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    pages: &[ObjectRef],
) -> Result<ComputedHintData> {
    let xref = pdf.get_xref_table();
    let mut object_stream_data = BTreeMap::new();
    for (object_ref, entry) in &xref {
        if let XrefEntry::Compressed { stream, .. } = entry {
            object_stream_data.insert(object_ref.number, *stream);
        }
    }
    let optimization = Optimization::optimize(pdf, &object_stream_data, false, |_, _| 0u8)?;

    let mut first_page_private = BTreeSet::new();
    let mut first_page_shared = BTreeSet::new();
    let mut other_page_private = BTreeSet::new();
    let mut other_page_shared = BTreeSet::new();
    let mut outline_objects = BTreeSet::new();

    for (object_ref, users) in optimization.object_users() {
        let mut in_first_page = false;
        let mut other_pages = 0_u32;
        let mut thumbs = 0_u32;
        let mut others = 0_u32;
        let mut in_open_document = false;
        let mut in_outlines = false;
        let mut is_root = false;

        for user in users {
            match user {
                ObjectUser::Bad => {} // cov:ignore: Optimization never records the internal Bad sentinel
                ObjectUser::Page(page) if *page == 0 => in_first_page = true,
                ObjectUser::Page(_) => other_pages += 1,
                ObjectUser::Thumbnail(_) => thumbs += 1,
                ObjectUser::TrailerKey(key) if key == b"Encrypt" => in_open_document = true,
                ObjectUser::TrailerKey(_) => others += 1,
                ObjectUser::RootKey(key) if key == b"Outlines" => in_outlines = true,
                ObjectUser::RootKey(key) if OPEN_DOCUMENT_ROOT_KEYS.contains(&key.as_slice()) => {
                    in_open_document = true
                }
                ObjectUser::RootKey(_) => others += 1,
                ObjectUser::Root => is_root = true,
            }
        }

        if is_root {
            continue;
        }
        if in_outlines {
            outline_objects.insert(object_ref);
        } else if in_open_document {
            continue;
        } else if in_first_page && others == 0 && other_pages == 0 && thumbs == 0 {
            first_page_private.insert(object_ref);
        } else if in_first_page {
            first_page_shared.insert(object_ref);
        } else if other_pages == 1 && others == 0 && thumbs == 0 {
            other_page_private.insert(object_ref);
        } else if other_pages > 1 {
            other_page_shared.insert(object_ref);
        }
    }

    let first_page = pages.first().copied().ok_or_else(|| {
        crate::Error::Unsupported("no pages found while calculating hint data".to_owned())
    })?;
    let first_page = uncompressed_object_ref(first_page, &xref);
    first_page_private.remove(&first_page);

    let outline_root =
        root_outlines_ref(pdf)?.map(|object_ref| uncompressed_object_ref(object_ref, &xref));
    let mut ordered_outlines = Vec::with_capacity(outline_objects.len());
    if let Some(outline_root) = outline_root {
        if outline_objects.remove(&outline_root) {
            ordered_outlines.push(outline_root);
        }
    }
    ordered_outlines.extend(outline_objects.iter().copied());

    let mut part6_objects = Vec::new();
    part6_objects.push(first_page);
    part6_objects.extend(first_page_private.iter().copied());
    part6_objects.extend(first_page_shared.iter().copied());
    if outlines_in_first_page(pdf)? {
        part6_objects.extend(ordered_outlines.iter().copied());
    }

    let mut page_object_counts = Vec::with_capacity(pages.len());
    page_object_counts.push(part6_objects.len() as u32);
    for (page_number, page_ref) in pages.iter().enumerate().skip(1) {
        let page_object = uncompressed_object_ref(*page_ref, &xref);
        let private_count = optimization
            .objects_for(&ObjectUser::Page(page_number as u32))
            .iter()
            .filter(|object_ref| {
                **object_ref != page_object && other_page_private.contains(object_ref)
            })
            .count();
        page_object_counts.push((private_count + 1) as u32);
    }

    let part8_objects: Vec<ObjectRef> = other_page_shared.into_iter().collect();

    let shared_object_numbers: BTreeSet<u32> = part6_objects
        .iter()
        .chain(&part8_objects)
        .map(|r| r.number)
        .collect();
    let mut page_shared_objects = Vec::with_capacity(pages.len());
    page_shared_objects.push(Vec::new());
    for page_number in 1..pages.len() {
        let shared = optimization
            .objects_for(&ObjectUser::Page(page_number as u32))
            .iter()
            .filter(|object_ref| {
                optimization.users_for(**object_ref).len() > 1
                    && shared_object_numbers.contains(&object_ref.number)
            })
            .copied()
            .collect();
        page_shared_objects.push(shared);
    }

    Ok(ComputedHintData {
        optimization,
        page_object_counts,
        page_shared_objects,
        part8_objects,
        outline_root,
        outline_objects: ordered_outlines.into_iter().collect(),
    })
}

fn hint_warning(
    collect_soft_warnings: bool,
    warnings: &mut Vec<String>,
    message: impl Into<String>,
) -> std::result::Result<(), LinearizationCheckError> {
    let message = message.into();
    if collect_soft_warnings {
        warnings.push(message);
        Ok(())
    } else {
        Err(LinearizationCheckError::InvalidParam { message })
    }
}

fn adjusted_hint_offset(offset: u64, h_offset: u64, h_length: u64) -> u64 {
    if offset >= h_offset {
        offset.wrapping_add(h_length)
    } else {
        offset
    }
}

fn linearization_offset(
    xref: &BTreeMap<ObjectRef, XrefEntry>,
    object_ref: ObjectRef,
    seen: &mut BTreeSet<ObjectRef>,
) -> std::result::Result<u64, LinearizationCheckError> {
    if !seen.insert(object_ref) {
        return Err(LinearizationCheckError::InvalidParam {
            message: format!("xref object-stream chain cycles at {object_ref}"),
        });
    }
    let result = match xref.get(&object_ref).copied() {
        Some(XrefEntry::Uncompressed { offset }) => Ok(offset),
        Some(XrefEntry::Compressed { stream, .. }) => {
            linearization_offset(xref, ObjectRef::new(stream, 0), seen)
        }
        Some(XrefEntry::Free { .. }) | None => Err(LinearizationCheckError::InvalidParam {
            message: format!("no usable xref table entry for {object_ref}"),
        }),
    };
    seen.remove(&object_ref);
    result
}

fn length_next_n<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    xref: &BTreeMap<ObjectRef, XrefEntry>,
    first_object: u32,
    nobjects: u64,
    collect_soft_warnings: bool,
    warnings: &mut Vec<String>,
) -> std::result::Result<i128, LinearizationCheckError> {
    // A valid object sequence cannot contain more entries than the input file
    // can contain bytes. This bound keeps a corrupt 32-bit hint count from
    // turning the checker into an unbounded loop while preserving every valid
    // PDF (each indirect object consumes at least one byte).
    if nobjects > xref.len() as u64 + 1_000_000 {
        return Err(LinearizationCheckError::InvalidParam {
            message: format!(
                "hint table object count {nobjects} is unreasonably large for the xref table"
            ),
        });
    }

    let mut length = 0_i128;
    for index in 0..nobjects {
        // cov:ignore-start: the bounded hint count and u32 PDF object domain make these overflow arms unreachable for a valid input
        let object_number = first_object
            .checked_add(u32::try_from(index).map_err(|_| {
                LinearizationCheckError::InvalidParam {
                    message: format!("object sequence starting at {first_object} overflows u32"),
                }
            })?)
            .ok_or_else(|| LinearizationCheckError::InvalidParam {
                message: format!("object sequence starting at {first_object} overflows u32"),
            })?;
        // cov:ignore-end
        let object_ref = ObjectRef::new(object_number, 0);
        if !xref.contains_key(&object_ref) {
            hint_warning(
                collect_soft_warnings,
                warnings,
                format!("no xref table entry for {object_number} 0"),
            )?; // cov:ignore: llvm maps the warning closure cleanup to this continue
            continue;
        }
        let mut seen = BTreeSet::new();
        let offset = linearization_offset(xref, object_ref, &mut seen)?;
        let object = pdf.get_object_handle(object_ref);
        object
            .try_dereference()
            .map_err(LinearizationCheckError::from)?;
        let (_, end_after_space) = object.end_offsets();
        if end_after_space < 0 {
            return Err(LinearizationCheckError::InvalidParam {
                message: format!("object {object_ref} has no source extent"),
            });
        }
        length += i128::from(end_after_space) - i128::from(offset);
    }
    Ok(length)
}

struct HintTableCheckInput<'a> {
    page_hints: &'a HPageOffset,
    shared_hints: &'a HSharedObject,
    outline_hints: Option<&'a HGeneric>,
    h_offset: u64,
    h_length: u64,
    collect_soft_warnings: bool,
    warnings: &'a mut Vec<String>,
}

fn check_hint_tables<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    pages: &[ObjectRef],
    input: HintTableCheckInput<'_>,
) -> std::result::Result<(), LinearizationCheckError> {
    let HintTableCheckInput {
        page_hints,
        shared_hints,
        outline_hints,
        h_offset,
        h_length,
        collect_soft_warnings,
        warnings,
    } = input;
    let computed = compute_hint_data(pdf, pages).map_err(LinearizationCheckError::from)?;
    let xref = pdf.get_xref_table();

    if page_hints.entries.len() != pages.len() {
        return Err(LinearizationCheckError::InvalidParam {
            message: format!(
                "page offset hint table has {} entries but the document has {} pages",
                page_hints.entries.len(),
                pages.len()
            ),
        });
    }

    let mut shared_idx_to_obj = BTreeMap::new();
    // `compute_hint_data` above rejects an empty page list before this point.
    let first_page = pages[0];

    if shared_hints.nshared_total < shared_hints.nshared_first_page {
        hint_warning(
            collect_soft_warnings,
            warnings,
            "shared object hint table: ntotal < nfirst_page",
        )?; // cov:ignore: llvm maps the warning closure cleanup to the following page loop
    } else {
        let mut current_object = first_page.number;
        for index in 0..shared_hints.nshared_total as usize {
            if index == shared_hints.nshared_first_page as usize {
                let first_shared_obj =
                    u32::try_from(shared_hints.first_shared_obj).map_err(|_| {
                        LinearizationCheckError::InvalidParam {
                            message: "first shared object number does not fit in u32".to_owned(),
                        }
                    })?;
                if let Some(first_part8) = computed.part8_objects.first() {
                    if first_shared_obj != first_part8.number {
                        hint_warning(
                            collect_soft_warnings,
                            warnings,
                            format!(
                                "first shared object number mismatch: hint table = {}; computed = {}",
                                shared_hints.first_shared_obj, first_part8.number
                            ),
                        )?; // cov:ignore: llvm maps this warning closure cleanup to the enclosing transition
                    }
                } else {
                    hint_warning(
                        collect_soft_warnings,
                        warnings,
                        "part 8 is empty but nshared_total > nshared_first_page",
                    )?; // cov:ignore: llvm maps this warning closure cleanup to the following object sequence
                }
                current_object = first_shared_obj;
                let object_ref = ObjectRef::new(current_object, 0);
                let mut seen = BTreeSet::new();
                if !computed.part8_objects.is_empty() {
                    let computed_offset = linearization_offset(&xref, object_ref, &mut seen)?;
                    let hint_offset =
                        adjusted_hint_offset(shared_hints.first_shared_offset, h_offset, h_length);
                    if computed_offset != hint_offset {
                        hint_warning(
                            collect_soft_warnings,
                            warnings,
                            format!(
                                "first shared object offset mismatch: hint table = {hint_offset}; computed = {computed_offset}"
                            ),
                        )?; // cov:ignore: llvm maps this warning closure cleanup to the shared-entry loop
                    }
                }
            }

            let entry = shared_hints.entries.get(index).ok_or_else(|| {
                LinearizationCheckError::InvalidParam {
                    message: format!("shared object hint table is missing entry {index}"),
                }
            })?;
            let nobjects = entry.nobjects_minus_one.checked_add(1).ok_or_else(|| {
                LinearizationCheckError::InvalidParam {
                    message: format!("shared object {index} object count overflows"),
                }
            })?;
            let nobjects_u32 =
                u32::try_from(nobjects).map_err(|_| LinearizationCheckError::InvalidParam {
                    message: format!("shared object {index} object count does not fit in u32"),
                })?;
            let computed_length = length_next_n(
                pdf,
                &xref,
                current_object,
                nobjects,
                collect_soft_warnings,
                warnings,
            )?; // cov:ignore: llvm maps this warning closure cleanup to the page-count branch
            let hint_length =
                i128::from(shared_hints.min_group_length) + i128::from(entry.delta_group_length);
            if computed_length != hint_length {
                hint_warning(
                    collect_soft_warnings,
                    warnings,
                    format!(
                        "shared object {index} length mismatch: hint table = {hint_length}; computed = {computed_length}"
                    ),
                )?;
            }
            shared_idx_to_obj.insert(index as u64, current_object);
            current_object = current_object.checked_add(nobjects_u32).ok_or_else(|| {
                LinearizationCheckError::InvalidParam {
                    message: format!("shared object {index} sequence overflows u32"),
                }
            })?;
        }
    }

    // qpdf checks this before entering the per-page loop, but only after
    // `checkHSharedObject` has populated the shared-index map
    // (`QPDF_linearization.cc:624-632`). Preserve that diagnostic order.
    let mut seen = BTreeSet::new();
    let computed_first_page_offset = linearization_offset(&xref, first_page, &mut seen)?;
    let hint_first_page_offset =
        adjusted_hint_offset(page_hints.first_page_offset, h_offset, h_length);
    if hint_first_page_offset != computed_first_page_offset {
        hint_warning(
            collect_soft_warnings,
            warnings,
            "first page object offset mismatch",
        )?;
    }

    for (page_number, (entry, &computed_count)) in page_hints
        .entries
        .iter()
        .zip(&computed.page_object_counts)
        .enumerate()
    {
        let hint_count = page_hints
            .min_nobjects
            .checked_add(u32::try_from(entry.delta_nobjects).map_err(|_| {
                LinearizationCheckError::InvalidParam {
                    message: format!("page {page_number} object count does not fit in u32"),
                }
            })?)
            .ok_or_else(|| LinearizationCheckError::InvalidParam {
                message: format!("page {page_number} object count overflows"),
            })?;
        if hint_count != computed_count {
            hint_warning(
                collect_soft_warnings,
                warnings,
                format!(
                    "object count mismatch for page {page_number}: hint table = {hint_count}; computed = {computed_count}"
                ),
            )?; // cov:ignore: llvm maps this length-call cleanup to the page-length comparison
        }

        let first_object = pages[page_number].number;
        let computed_length = length_next_n(
            pdf,
            &xref,
            first_object,
            hint_count as u64,
            collect_soft_warnings,
            warnings,
        )?; // cov:ignore: llvm maps this length-call cleanup to the following page-length comparison
        let hint_length =
            i128::from(page_hints.min_page_length) + i128::from(entry.delta_page_length);
        if computed_length != hint_length {
            let mut seen = BTreeSet::new();
            let offset = linearization_offset(&xref, pages[page_number], &mut seen)?;
            hint_warning(
                collect_soft_warnings,
                warnings,
                format!(
                    "page length mismatch for page {page_number}: hint table = {hint_length}; computed length = {computed_length} (offset = {offset})"
                ),
            )?; // cov:ignore: llvm maps this warning closure cleanup to the page shared-set loop
        }

        let mut hint_shared = BTreeSet::new();
        for identifier in &entry.shared_identifiers {
            let Some(object_number) = shared_idx_to_obj.get(identifier) else {
                return Err(LinearizationCheckError::InvalidParam {
                    message: format!(
                        "unable to get object for item {identifier} in shared objects hint table"
                    ),
                });
            };
            hint_shared.insert(*object_number);
        }
        let computed_shared: BTreeSet<u32> = computed.page_shared_objects[page_number]
            .iter()
            .map(|object_ref| object_ref.number)
            .collect();

        if page_number == 0 && entry.nshared_objects > 0 {
            hint_warning(
                collect_soft_warnings,
                warnings,
                "page 0 has shared identifier entries",
            )?; // cov:ignore: llvm maps this warning closure cleanup to the computed shared-set loop
        }
        for object_number in hint_shared.difference(&computed_shared) {
            hint_warning(
                collect_soft_warnings,
                warnings,
                format!(
                    "page {page_number}: shared object {object_number}: in hint table but not computed list"
                ),
            )?; // cov:ignore: llvm maps this warning closure cleanup to the hint shared-set loop
        }
        for object_number in computed_shared.difference(&hint_shared) {
            hint_warning(
                collect_soft_warnings,
                warnings,
                format!(
                    "page {page_number}: shared object {object_number}: in computed list but not hint table"
                ),
            )?; // cov:ignore: llvm maps this warning closure cleanup to the computed shared-set loop
        }
    }

    let computed_outline_count = computed.outline_objects.len() as u32;
    let outline_hint = outline_hints.cloned().unwrap_or(HGeneric {
        first_object: 0,
        first_object_offset: 0,
        nobjects: 0,
        group_length: 0,
    });
    if computed_outline_count == outline_hint.nobjects {
        if computed_outline_count != 0 {
            // cov:ignore-start: qpdf's object-user map cannot contain an outline descendant without its root /Outlines object
            let Some(computed_outline_root) = computed.outline_root else {
                return Err(LinearizationCheckError::InvalidParam {
                    message:
                        "outline objects are present but the root /Outlines reference is missing"
                            .to_owned(),
                });
            };
            // cov:ignore-end
            if u64::from(computed_outline_root.number) == outline_hint.first_object {
                let mut seen = BTreeSet::new();
                let computed_offset =
                    linearization_offset(&xref, computed_outline_root, &mut seen)?;
                let mut max_end = 0_i64;
                for object_ref in computed.optimization.objects_for_root_key(b"Outlines") {
                    let object = pdf.get_object_handle(object_ref);
                    object
                        .try_dereference()
                        .map_err(LinearizationCheckError::from)?;
                    max_end = max_end.max(object.end_offsets().1);
                }
                // cov:ignore-start: every object in the canonical source xref has a parsed end extent
                if max_end < 0 {
                    return Err(LinearizationCheckError::InvalidParam {
                        message: "outline objects have no source extent".to_owned(),
                    });
                }
                // cov:ignore-end
                let hint_offset =
                    adjusted_hint_offset(outline_hint.first_object_offset, h_offset, h_length);
                if computed_offset != hint_offset {
                    hint_warning(
                        collect_soft_warnings,
                        warnings,
                        format!(
                            "incorrect offset in outlines table: hint table = {hint_offset}; computed = {computed_offset}"
                        ),
                    )?; // cov:ignore: llvm maps this outline warning closure cleanup to the enclosing branch
                }
                let computed_length = i128::from(max_end) - i128::from(computed_offset);
                if computed_length != i128::from(outline_hint.group_length) {
                    hint_warning(
                        collect_soft_warnings,
                        warnings,
                        format!(
                            "incorrect length in outlines table: hint table = {}; computed = {computed_length}",
                            outline_hint.group_length
                        ),
                    )?; // cov:ignore: llvm maps this outline warning closure cleanup to the enclosing branch
                }
            } else {
                hint_warning(
                    collect_soft_warnings,
                    warnings,
                    "incorrect first object number in outline hints table.",
                )?; // cov:ignore: llvm maps this outline warning closure cleanup to the enclosing branch
            }
        }
    } else {
        hint_warning(
            collect_soft_warnings,
            warnings,
            "incorrect object count in outline hint table",
        )?; // cov:ignore: llvm maps this outline warning closure cleanup to the enclosing branch
    }

    Ok(())
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
/// of bounds, the hint stream cannot be located or decoded, or strict `/T`
/// position comparison reports a mismatch against the xref parser's first item.
///
/// Returns [`LinearizationCheckError::Io`] when resolving an object via `pdf`
/// or enumerating the page references fails.
///
/// # Side effects
///
/// Computing the part-6 (first page) source extent runs the same object
/// classification pass qpdf performs before checking
/// (`QPDF_linearization.cc:495`, `optimize(object_stream_data, false)`),
/// which mutates `pdf`: a direct `/Outlines` dictionary is made indirect and
/// the page tree's inherited attributes may be pushed down. A `pdf` checked
/// this way is not guaranteed to produce identical bytes to an unchecked
/// `pdf` on a subsequent write, matching qpdf's own behavior.
pub fn check_linearization<R: Read + Seek>(pdf: &mut Pdf<R>, file_bytes: &[u8]) -> CheckResult {
    check_linearization_inner(pdf, file_bytes, false, false).map(|_| ())
}

/// Run the linearization checks while retaining qpdf's soft warning messages.
///
/// `skip_first_page_warning` is used after the parameter preflight has already
/// emitted qpdf's `/O` warning. The referenced object is still validated by
/// the shared checker; only the duplicate soft comparison is skipped.
pub(crate) fn check_linearization_warnings<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    file_bytes: &[u8],
    skip_first_page_warning: bool,
) -> std::result::Result<Vec<String>, LinearizationCheckError> {
    check_linearization_inner(pdf, file_bytes, true, skip_first_page_warning)
}

fn check_linearization_inner<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    file_bytes: &[u8],
    collect_soft_warnings: bool,
    skip_first_page_warning: bool,
) -> std::result::Result<Vec<String>, LinearizationCheckError> {
    let file_len = file_bytes.len() as u64;
    let mut warnings = Vec::new();

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
    // qpdf's readLinearizationData only requires `/O` to be an integer. When
    // the parameter preflight already reported the soft `/O` mismatch, skip
    // the stricter object-shape validation too: qpdf continues into
    // checkLinearizationInternal without dereferencing the mismatching object.
    // `as_u64` itself must stay inside this guard: it rejects a negative `/O`
    // as a hard error, but qpdf's preflight has already turned a negative
    // `/O` into the soft "first page object (/O) mismatch" warning by the
    // time `skip_first_page_warning` is set, so re-validating it here would
    // reject a file qpdf only warns about.
    if !skip_first_page_warning {
        let o_num = as_u64(&o_obj, "O")?;
        // PDF object numbers are u32; an /O value beyond u32::MAX cannot refer
        // to a real object — silently casting with `as u32` would wrap and
        // look up the wrong slot, so reject up front.
        let o_num_u32 =
            u32::try_from(o_num).map_err(|_| LinearizationCheckError::InvalidParam {
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
        // `checkLinearizationInternal` (`QPDF_linearization.cc:419-427`).
        let Some(first_page_ref) = pages.first().copied() else {
            fail!("/O ({o_num}) cannot be checked because the document has no pages");
        };
        if first_page_ref.number as u64 != o_num {
            if collect_soft_warnings {
                warnings.push("first page object (/O) mismatch".to_owned());
            } else {
                fail!(
                    "/O ({o_num}) does not match the first page object ({})",
                    first_page_ref.number
                );
            }
        }
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
    let mut load_checked_hint = |index: usize,
                                 offset: u64,
                                 length: u64|
     -> std::result::Result<_, LinearizationCheckError> {
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
        load_hint_stream(pdf, file_bytes, offset, length).map_err(|error| match error {
            crate::Error::Unsupported(message) => LinearizationCheckError::InvalidParam { message },
            error => LinearizationCheckError::Io(Box::new(error)), // cov:ignore: in-memory hint streams cannot produce a reader I/O error
        })
    };

    let h_offset = as_u64(&h_items[0], "H[0]")?;
    let h_length = as_u64(&h_items[1], "H[1]")?;
    let (hint_dict, primary_decompressed) = load_checked_hint(0, h_offset, h_length)?;
    let mut hint_bytes = (*primary_decompressed).clone();
    if h_items.len() == 4 {
        let overflow_offset = as_u64(&h_items[2], "H[2]")?;
        let overflow_length = as_u64(&h_items[3], "H[3]")?;
        if overflow_offset != 0 {
            let (_overflow_dict, overflow_decompressed) =
                load_checked_hint(2, overflow_offset, overflow_length)?;
            // qpdf's readLinearizationData pipes both streams into one
            // Pl_Buffer before reading /S and /O (`QPDF_linearization.cc:241-245`).
            hint_bytes.extend_from_slice(&overflow_decompressed);
        } // cov:ignore: llvm maps the overflow closure cleanup to this brace
    }

    let (shared_offset, outline_offset) = read_hint_offsets(&hint_dict).map_err(map_show_error)?;
    // cov:ignore-start: the shared offset is bounds-checked by the show decoder before this shared checker runs
    if shared_offset >= hint_bytes.len() {
        fail!(
            "hint stream /S offset ({shared_offset}) is out of bounds (hint size {})",
            hint_bytes.len()
        );
    }
    // cov:ignore-end
    // cov:ignore-start: an opened PDF page tree cannot contain more than u32::MAX pages on supported targets
    let n_pages = u32::try_from(n_val).map_err(|_| LinearizationCheckError::InvalidParam {
        message: format!("/N ({n_val}) does not fit in u32"),
    })?;
    // cov:ignore-end
    let page_hints = read_h_page_offset(&hint_bytes, n_pages).map_err(map_show_error)?;
    let shared_hints =
        read_h_shared_object(&hint_bytes[shared_offset..]).map_err(map_show_error)?;
    let outline_hints = match outline_offset {
        Some(offset) => {
            // cov:ignore-start: the outline offset is bounds-checked by the show decoder before this shared checker runs
            if offset >= hint_bytes.len() {
                fail!(
                    "hint stream /O offset ({offset}) is out of bounds (hint size {})",
                    hint_bytes.len()
                );
            }
            // cov:ignore-end
            Some(read_h_generic(&hint_bytes[offset..]).map_err(map_show_error)?)
        }
        None => None,
    };

    // -----------------------------------------------------------------------
    // 5. /T must point at the whitespace immediately before the first xref
    // item. qpdf's xref parser records that item offset while loading the
    // section (`QPDF.cc:845-869,1110-1120`); checkLinearizationInternal then
    // only seeks to `/T`, skips PDF whitespace, and compares positions
    // (`QPDF_linearization.cc:452-470`).
    // -----------------------------------------------------------------------
    let t_obj = first_obj
        .try_get_key(b"/T")
        .map_err(LinearizationCheckError::from)?;
    let t_val = as_u64(&t_obj, "T")?;
    // qpdf's xref parser already recorded the comparison target. Do not
    // rediscover xref syntax from file bytes here: qpdf's /T check only seeks
    // to /T, skips whitespace, and compares positions.
    let mut file_cursor = t_val;
    if t_val < file_bytes.len() as u64 {
        let mut cursor = t_val as usize;
        while cursor < file_bytes.len() && is_pdf_whitespace(file_bytes[cursor]) {
            cursor += 1;
        }
        file_cursor = cursor as u64;
    }
    let computed = pdf.first_xref_item_offset();
    if file_cursor != computed {
        let message = format!(
            "space before first xref item (/T) mismatch (computed = {computed}; file = {file_cursor}"
        );
        if collect_soft_warnings {
            warnings.push(message);
        } else {
            fail!("/T ({t_val}) does not match the first xref item offset ({computed})");
        }
    }
    // -----------------------------------------------------------------------
    // 6. /E must match the source extent envelope of qpdf's part 6, not merely
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
    // qpdf leaves the envelope at (-1, -1) when part-6 objects have no source
    // extents (`QPDF_linearization.cc:507-521`). In that case its `/E` check
    // emits a warning rather than turning the missing metadata into a fatal
    // parse error; this checker has no logger at this boundary, so it skips
    // only the extent-range comparison and retains all structural checks.
    if min_e >= 0 && max_e >= 0 {
        let min_e = min_e as u64;
        let max_e = max_e as u64;
        if e_val < min_e || e_val > max_e {
            if collect_soft_warnings {
                warnings.push(format!(
                    "end of first page section (/E) mismatch: /E = {e_val}; computed = {min_e}..{max_e}"
                ));
            } else {
                fail!(
                    "/E ({e_val}) does not match the part-6 source extent range ({min_e}..{max_e})"
                );
            }
        }
    }

    // qpdf performs the decoded Page/Shared/Outline hint-table checks after
    // `/E` and after its object-user classification pass
    // (`QPDF_linearization.cc:539-835`). Keep malformed bitstreams as hard
    // errors, while routing structural mismatches through the same soft-warning
    // channel used by qpdf's `linearizationWarning`.
    check_hint_tables(
        pdf,
        &pages,
        HintTableCheckInput {
            page_hints: &page_hints,
            shared_hints: &shared_hints,
            outline_hints: outline_hints.as_ref(),
            h_offset,
            h_length,
            collect_soft_warnings,
            warnings: &mut warnings,
        },
    )?;

    Ok(warnings)
}

/// A qpdf `damagedPDF` raised while loading a linearization hint stream.
///
/// `load_hint_stream` keeps its existing checker-facing error text, while the
/// `show-linearization` route needs the source category and offset that qpdf
/// attaches to the same failure.  Keep both forms at this boundary so the
/// shared loader does not make either consumer reconstruct context from a
/// free-form message.
pub(crate) struct HintStreamDamage {
    pub(crate) object: &'static str,
    pub(crate) offset: u64,
    pub(crate) detail: String,
    legacy: String,
}

impl HintStreamDamage {
    fn new(
        object: &'static str,
        offset: u64,
        detail: impl Into<String>,
        legacy: impl Into<String>,
    ) -> Self {
        Self {
            object,
            offset,
            detail: detail.into(),
            legacy: legacy.into(),
        }
    }
}

pub(crate) enum HintStreamLoadError {
    Damage(HintStreamDamage),
    Core(crate::Error),
}

impl From<crate::Error> for HintStreamLoadError {
    fn from(error: crate::Error) -> Self {
        Self::Core(error)
    }
}

/// Resolve and decode the hint stream object at `offset` through the canonical
/// object/stream route, preserving the checker-facing error messages.
///
/// The qpdf damage context used by `show-linearization` is retained internally
/// by [`load_hint_stream_with_damage`].
pub(crate) fn load_hint_stream<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    file_bytes: &[u8],
    offset: usize,
    expected_h_length: u64,
) -> Result<(ObjectHandle, Rc<Vec<u8>>)> {
    load_hint_stream_with_damage(pdf, file_bytes, offset, expected_h_length).map_err(|error| {
        match error {
            HintStreamLoadError::Damage(damage) => crate::Error::Unsupported(damage.legacy),
            HintStreamLoadError::Core(error) => error,
        }
    })
}

/// Resolve and decode a hint stream while retaining qpdf's `damagedPDF`
/// category and source offset for `--show-linearization` warning formatting.
///
/// This mirrors `readHintStream` and `readObjectAtOffset` from
/// `libqpdf/QPDF_linearization.cc:283-322` and `libqpdf/QPDF.cc:1542-1636`.
pub(crate) fn load_hint_stream_with_damage<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    file_bytes: &[u8],
    offset: usize,
    expected_h_length: u64,
) -> std::result::Result<(ObjectHandle, Rc<Vec<u8>>), HintStreamLoadError> {
    // The raw byte slice is used only to identify the object reference at
    // `/H[0]`. The object itself is then resolved as an [`ObjectHandle`], its
    // qpdf source extents are used for the `/H[1]` check, and `get_stream_data`
    // runs the specialized stream pipeline. This mirrors qpdf's
    // `readHintStream` boundary (`QPDF_linearization.cc:245-321`) without
    // materializing a legacy `Object::Stream` value.
    // /H[0] must point exactly at the `N G obj` header (after at most a few
    // leading whitespace bytes).  A loose scan that just searches for `obj`
    // anywhere in a window would accept misaligned offsets that happen to
    // sit near another object header — which is precisely the kind of
    // corruption we want to detect.
    const SCAN_WINDOW: usize = 64;
    if offset >= file_bytes.len() {
        return Err(HintStreamLoadError::Damage(HintStreamDamage::new(
            "linearization hint stream",
            offset as u64,
            "expected n n obj",
            format!(
                "/H[0] offset ({offset}) is beyond file length ({})",
                file_bytes.len()
            ),
        )));
    }
    let scan_end = offset.saturating_add(SCAN_WINDOW).min(file_bytes.len());
    let window = &file_bytes[offset..scan_end];

    let Some((obj_num, obj_gen)) = parse_obj_header_at(window) else {
        return Err(HintStreamLoadError::Damage(HintStreamDamage::new(
            "linearization hint stream",
            offset as u64,
            "expected n n obj",
            format!(
                "/H[0] offset ({offset}) does not point at an indirect object header \
                 (expected `N G obj`)"
            ),
        )));
    };

    // Resolve the object via the Pdf handle.  Use the parsed generation so a
    // hint stream with a non-zero generation (e.g. after incremental update)
    // is still locatable.
    let hint_ref = ObjectRef::new(obj_num, obj_gen);
    let hint_obj = pdf.resolve_object_handle_at_offset(offset as u64, hint_ref)?;
    // qpdf's `InputSource::getLastOffset()` remains at the start of the
    // `endobj` token when `readObject` reports a non-stream hint object. The
    // canonical resolver stores the extent immediately after that token, so
    // subtract the token width to preserve qpdf's damagedPDF location.
    let hint_object_damage_offset = u64::try_from(hint_obj.end_offsets().0)
        .ok()
        .and_then(|offset| offset.checked_sub(b"endobj".len() as u64))
        .unwrap_or_else(|| pdf.source_last_offset());
    let Some(hint_dict) = hint_obj.as_stream_dict() else {
        if hint_obj.is_null() {
            return Err(HintStreamLoadError::Damage(HintStreamDamage::new(
                "linearization dictionary",
                hint_object_damage_offset,
                "hint table is not a stream",
                format!(
                    "hint stream object {obj_num} {obj_gen} (at /H[0] offset {offset}) does not exist"
                ),
            )));
        }
        return Err(HintStreamLoadError::Damage(HintStreamDamage::new(
            "linearization dictionary",
            hint_object_damage_offset,
            "hint table is not a stream",
            format!(
                "hint stream object {obj_num} {obj_gen} (at /H[0] offset {offset}) is not a stream"
            ),
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
        return Err(HintStreamLoadError::Core(crate::Error::Unsupported(
            format!("hint stream object {obj_num} {obj_gen} has no source extent"),
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
        return Err(HintStreamLoadError::Damage(HintStreamDamage::new(
            "linearization dictionary",
            pdf.source_last_offset(),
            "hint table length mismatch",
            format!(
                "/H[1] ({expected_h_length}) does not match hint stream object span \
                 {end_before_space}..{end_after_space} from offset {offset}"
            ),
        )));
    }

    let decoded = hint_obj.get_stream_data(DecodeLevel::Specialized)?;
    Ok((hint_dict, decoded))
}

/// Verify that the hint stream can be located, has the qpdf source extent
/// advertised by `/H[1]`, and can be decoded by the specialized pipeline.
#[cfg(test)]
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

    type DecodedHintTablesForTest = (
        Pdf<Cursor<Vec<u8>>>,
        Vec<ObjectRef>,
        u64,
        u64,
        HPageOffset,
        HSharedObject,
        Option<HGeneric>,
    );

    fn decode_hint_tables_for_test(bytes: &[u8]) -> DecodedHintTablesForTest {
        let mut pdf = Pdf::open_mem_owned(bytes.to_vec()).expect("linearized fixture should open");
        let candidate = pdf
            .linearization_candidate_ref()
            .expect("candidate probe")
            .expect("linearization candidate");
        let candidate = pdf.get_object_handle(candidate);
        candidate.try_dereference().expect("candidate dictionary");
        let h = candidate.try_get_key(b"/H").expect("/H key");
        h.try_dereference().expect("/H array");
        let h_items = h.as_array().expect("/H array value");
        let h_offset = as_u64(&h_items[0], "H[0]").expect("H offset");
        let h_length = as_u64(&h_items[1], "H[1]").expect("H length");
        let (hint_dict, primary) =
            load_hint_stream(&mut pdf, bytes, h_offset as usize, h_length).expect("hint stream");
        let (shared_offset, outline_offset) = read_hint_offsets(&hint_dict)
            .map_err(map_show_error)
            .expect("hint offsets");
        let page_count = PageDocumentHelper::new(&mut pdf)
            .get_all_pages()
            .expect("pages")
            .len() as u32;
        let page_hints = read_h_page_offset(&primary, page_count).expect("page hints");
        let shared_hints = read_h_shared_object(&primary[shared_offset..]).expect("shared hints");
        let outline_hints =
            outline_offset.map(|offset| read_h_generic(&primary[offset..]).expect("outline hints"));
        let pages = PageDocumentHelper::new(&mut pdf)
            .get_all_pages()
            .expect("pages")
            .to_vec();
        (
            pdf,
            pages,
            h_offset,
            h_length,
            page_hints,
            shared_hints,
            outline_hints,
        )
    }

    #[test]
    fn hint_table_checks_reject_page_and_shared_offset_length_mismatches() {
        let bytes = linearized_fixture_bytes();

        let (mut pdf, pages, h_offset, h_length, mut page_hints, shared_hints, outline_hints) =
            decode_hint_tables_for_test(&bytes);
        page_hints.first_page_offset = page_hints.first_page_offset.wrapping_add(1);
        let mut warnings = Vec::new();
        let page_error = check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: outline_hints.as_ref(),
                h_offset,
                h_length,
                collect_soft_warnings: false,
                warnings: &mut warnings,
            },
        )
        .expect_err("a wrong Page Offset first-page offset must fail");
        assert!(page_error
            .to_string()
            .contains("first page object offset mismatch"));

        let (mut pdf, pages, h_offset, h_length, page_hints, mut shared_hints, outline_hints) =
            decode_hint_tables_for_test(&bytes);
        shared_hints.min_group_length = shared_hints.min_group_length.wrapping_add(1);
        let mut warnings = Vec::new();
        let shared_error = check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: outline_hints.as_ref(),
                h_offset,
                h_length,
                collect_soft_warnings: false,
                warnings: &mut warnings,
            },
        )
        .expect_err("a wrong Shared Object group length must fail");
        assert!(shared_error
            .to_string()
            .contains("shared object 0 length mismatch"));
    }

    #[test]
    fn hint_table_checks_reject_an_outline_offset_mismatch() {
        let bytes =
            std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
                "../../tests/golden/references/objstm-lin-outlines-80-80/linearize-classic.pdf",
            ))
            .expect("outline linearized fixture");
        let (mut pdf, pages, h_offset, h_length, page_hints, shared_hints, mut outline_hints) =
            decode_hint_tables_for_test(&bytes);
        let outline_hints = outline_hints
            .as_mut()
            .expect("outline fixture has an Outline hint table");
        outline_hints.first_object_offset = outline_hints.first_object_offset.wrapping_add(1);
        let mut warnings = Vec::new();
        let error = check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: Some(outline_hints),
                h_offset,
                h_length,
                collect_soft_warnings: false,
                warnings: &mut warnings,
            },
        )
        .expect_err("a wrong Outline offset must fail");
        assert!(error
            .to_string()
            .contains("incorrect offset in outlines table"));
    }

    #[test]
    fn hint_table_warning_route_accumulates_qpdf_soft_diagnostics() {
        let bytes = linearized_fixture_bytes();
        let (mut pdf, pages, h_offset, h_length, mut page_hints, mut shared_hints, outline_hints) =
            decode_hint_tables_for_test(&bytes);
        page_hints.first_page_offset = 0;
        page_hints.min_nobjects += 1;
        page_hints.min_page_length += 1;
        page_hints.entries[0].nshared_objects = 1;
        page_hints.entries[0].shared_identifiers = vec![0];
        page_hints.entries[0].shared_numerators = vec![0];
        shared_hints.min_group_length += 1;

        let mut warnings = Vec::new();
        check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: outline_hints.as_ref(),
                h_offset,
                h_length,
                collect_soft_warnings: true,
                warnings: &mut warnings,
            },
        )
        .expect("qpdf hint mismatches are soft warnings");
        assert!(warnings
            .iter()
            .any(|message| message == "first page object offset mismatch"));
        assert!(warnings
            .iter()
            .any(|message| message.starts_with("object count mismatch for page 0:")));
        assert!(warnings
            .iter()
            .any(|message| message.starts_with("page length mismatch for page 0:")));
        assert!(warnings
            .iter()
            .any(|message| message == "page 0 has shared identifier entries"));
        assert!(warnings
            .iter()
            .any(|message| message.starts_with("shared object 0 length mismatch:")));
        assert!(warnings.iter().any(|message| message
            .starts_with("page 0: shared object 6: in hint table but not computed list")));
    }

    #[test]
    fn hint_table_warning_route_checks_part8_offsets_and_lengths() {
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
                "../../tests/golden/references/objstm-lin-otherpage-shared-docother/linearize-objstm.pdf",
            ),
        )
        .expect("part-8 linearized fixture");
        let (mut pdf, pages, h_offset, h_length, page_hints, mut shared_hints, outline_hints) =
            decode_hint_tables_for_test(&bytes);
        shared_hints.first_shared_obj = shared_hints.first_shared_obj.wrapping_add(1);
        shared_hints.first_shared_offset = shared_hints.first_shared_offset.wrapping_add(1);
        shared_hints.min_group_length = shared_hints.min_group_length.wrapping_add(1);
        let mut warnings = Vec::new();
        check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: outline_hints.as_ref(),
                h_offset,
                h_length,
                collect_soft_warnings: true,
                warnings: &mut warnings,
            },
        )
        .expect("qpdf part-8 mismatches are soft warnings");
        assert!(warnings
            .iter()
            .any(|message| message.starts_with("first shared object number mismatch:")));
        assert!(warnings
            .iter()
            .any(|message| message.starts_with("first shared object offset mismatch:")));
        assert!(warnings
            .iter()
            .any(|message| message.starts_with("shared object 2 length mismatch:")));
    }

    #[test]
    fn hint_table_validator_covers_defensive_and_outline_warning_paths() {
        let malformed = map_show_error(ShowLinearizationError::Malformed {
            message: "bad hint bits".to_owned(),
        });
        assert!(matches!(
            malformed,
            LinearizationCheckError::InvalidParam { message } if message == "bad hint bits"
        ));
        let io = map_show_error(ShowLinearizationError::Io(Box::new(std::io::Error::other(
            "hint read failed",
        ))));
        assert!(matches!(io, LinearizationCheckError::Io(_)));

        let mut xref = BTreeMap::new();
        xref.insert(
            ObjectRef::new(1, 0),
            XrefEntry::Compressed {
                stream: 2,
                index: 0,
            },
        );
        xref.insert(ObjectRef::new(2, 0), XrefEntry::Uncompressed { offset: 7 });
        let mut seen = BTreeSet::new();
        assert_eq!(
            linearization_offset(&xref, ObjectRef::new(1, 0), &mut seen).unwrap(),
            7
        );
        let mut cycle = BTreeMap::new();
        cycle.insert(
            ObjectRef::new(3, 0),
            XrefEntry::Compressed {
                stream: 3,
                index: 0,
            },
        );
        assert!(linearization_offset(&cycle, ObjectRef::new(3, 0), &mut BTreeSet::new()).is_err());
        assert!(linearization_offset(
            &BTreeMap::from([(ObjectRef::new(4, 0), XrefEntry::Free { next: 0 })]),
            ObjectRef::new(4, 0),
            &mut BTreeSet::new()
        )
        .is_err());
        assert!(
            linearization_offset(&BTreeMap::new(), ObjectRef::new(5, 0), &mut BTreeSet::new())
                .is_err()
        );

        let mut pdf = Pdf::open_mem_owned(tiny_pdf_bytes()).expect("tiny PDF");
        let mut warnings = Vec::new();
        assert_eq!(
            length_next_n(&mut pdf, &BTreeMap::new(), 1, 1, true, &mut warnings,).unwrap(),
            0
        );
        assert!(warnings
            .iter()
            .any(|message| message == "no xref table entry for 1 0"));
        assert!(length_next_n(
            &mut pdf,
            &BTreeMap::new(),
            1,
            1_000_001,
            true,
            &mut warnings,
        )
        .is_err());

        let no_extent = ObjectRef::new(50, 0);
        pdf.set_object(no_extent, crate::Object::Integer(1));
        assert!(length_next_n(
            &mut pdf,
            &BTreeMap::from([(no_extent, XrefEntry::Uncompressed { offset: 0 })]),
            no_extent.number,
            1,
            true,
            &mut warnings,
        )
        .is_err());
        assert!(compute_hint_data(&mut pdf, &[]).is_err());

        let mut rootless = tiny_pdf_bytes();
        let root_marker = b"/Root 1 0 R";
        let root_start = rootless
            .windows(root_marker.len())
            .position(|window| window == root_marker)
            .expect("trailer root");
        rootless[root_start..root_start + root_marker.len()].fill(b' ');
        let mut rootless = Pdf::open_mem_owned(rootless).expect("rootless PDF");
        assert!(root_outlines_ref(&mut rootless).unwrap().is_none());
        assert!(!outlines_in_first_page(&mut rootless).unwrap());

        let outline_bytes =
            std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
                "../../tests/golden/references/objstm-lin-outlines-80-80/linearize-classic.pdf",
            ))
            .expect("outline fixture");
        let (mut pdf, pages, h_offset, h_length, page_hints, shared_hints, mut outline_hints) =
            decode_hint_tables_for_test(&outline_bytes);
        let outline_hints = outline_hints.as_mut().expect("outline hints");
        outline_hints.first_object = outline_hints.first_object.wrapping_add(1);
        let mut warnings = Vec::new();
        check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: Some(outline_hints),
                h_offset,
                h_length,
                collect_soft_warnings: true,
                warnings: &mut warnings,
            },
        )
        .expect("outline first-object mismatch is a soft warning");
        assert!(warnings
            .iter()
            .any(|message| message == "incorrect first object number in outline hints table."));

        let (mut pdf, pages, h_offset, h_length, page_hints, shared_hints, mut outline_hints) =
            decode_hint_tables_for_test(&outline_bytes);
        let outline_hints = outline_hints.as_mut().expect("outline hints");
        outline_hints.nobjects = 0;
        let mut warnings = Vec::new();
        check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: Some(outline_hints),
                h_offset,
                h_length,
                collect_soft_warnings: true,
                warnings: &mut warnings,
            },
        )
        .expect("outline count mismatch is a soft warning");
        assert!(warnings
            .iter()
            .any(|message| message == "incorrect object count in outline hint table"));

        let bytes = linearized_fixture_bytes();
        let (mut pdf, pages, h_offset, h_length, mut page_hints, shared_hints, outline_hints) =
            decode_hint_tables_for_test(&bytes);
        page_hints.entries.clear();
        let mut warnings = Vec::new();
        assert!(check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: outline_hints.as_ref(),
                h_offset,
                h_length,
                collect_soft_warnings: true,
                warnings: &mut warnings,
            },
        )
        .is_err());

        let (mut pdf, pages, h_offset, h_length, page_hints, mut shared_hints, outline_hints) =
            decode_hint_tables_for_test(&bytes);
        shared_hints.nshared_first_page = 0;
        shared_hints.nshared_total = 1;
        shared_hints.first_shared_obj = u64::MAX;
        let mut warnings = Vec::new();
        assert!(check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: outline_hints.as_ref(),
                h_offset,
                h_length,
                collect_soft_warnings: true,
                warnings: &mut warnings,
            },
        )
        .is_err());

        let (mut pdf, pages, h_offset, h_length, page_hints, mut shared_hints, outline_hints) =
            decode_hint_tables_for_test(&bytes);
        shared_hints.nshared_first_page = shared_hints.nshared_total + 1;
        let mut warnings = Vec::new();
        check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: outline_hints.as_ref(),
                h_offset,
                h_length,
                collect_soft_warnings: true,
                warnings: &mut warnings,
            },
        )
        .expect("ntotal < nfirst_page is a soft warning");
        assert!(warnings
            .iter()
            .any(|message| message == "shared object hint table: ntotal < nfirst_page"));

        let (mut pdf, pages, h_offset, h_length, page_hints, mut shared_hints, outline_hints) =
            decode_hint_tables_for_test(&bytes);
        shared_hints.nshared_total += 1;
        let mut warnings = Vec::new();
        assert!(check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: outline_hints.as_ref(),
                h_offset,
                h_length,
                collect_soft_warnings: true,
                warnings: &mut warnings,
            },
        )
        .is_err());

        let (mut pdf, pages, h_offset, h_length, page_hints, mut shared_hints, outline_hints) =
            decode_hint_tables_for_test(&bytes);
        shared_hints.entries[0].nobjects_minus_one = u64::MAX;
        let mut warnings = Vec::new();
        assert!(check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: outline_hints.as_ref(),
                h_offset,
                h_length,
                collect_soft_warnings: true,
                warnings: &mut warnings,
            },
        )
        .is_err());

        let (mut pdf, pages, h_offset, h_length, page_hints, mut shared_hints, outline_hints) =
            decode_hint_tables_for_test(&bytes);
        shared_hints.entries[0].nobjects_minus_one = u32::MAX as u64;
        let mut warnings = Vec::new();
        assert!(check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: outline_hints.as_ref(),
                h_offset,
                h_length,
                collect_soft_warnings: true,
                warnings: &mut warnings,
            },
        )
        .is_err());

        let (mut pdf, pages, h_offset, h_length, page_hints, mut shared_hints, outline_hints) =
            decode_hint_tables_for_test(&bytes);
        shared_hints.nshared_first_page = 0;
        shared_hints.nshared_total = 1;
        shared_hints.first_shared_obj = u32::MAX as u64;
        let mut warnings = Vec::new();
        assert!(check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: outline_hints.as_ref(),
                h_offset,
                h_length,
                collect_soft_warnings: true,
                warnings: &mut warnings,
            },
        )
        .is_err());

        let (mut pdf, pages, h_offset, h_length, mut page_hints, shared_hints, outline_hints) =
            decode_hint_tables_for_test(&bytes);
        page_hints.entries[0].delta_nobjects = u64::MAX;
        let mut warnings = Vec::new();
        assert!(check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: outline_hints.as_ref(),
                h_offset,
                h_length,
                collect_soft_warnings: true,
                warnings: &mut warnings,
            },
        )
        .is_err());

        let (mut pdf, pages, h_offset, h_length, mut page_hints, shared_hints, outline_hints) =
            decode_hint_tables_for_test(&bytes);
        page_hints.min_nobjects = u32::MAX;
        page_hints.entries[0].delta_nobjects = 1;
        let mut warnings = Vec::new();
        assert!(check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: outline_hints.as_ref(),
                h_offset,
                h_length,
                collect_soft_warnings: true,
                warnings: &mut warnings,
            },
        )
        .is_err());

        let (mut pdf, pages, h_offset, h_length, mut page_hints, shared_hints, outline_hints) =
            decode_hint_tables_for_test(&bytes);
        page_hints.entries[0].nshared_objects = 1;
        page_hints.entries[0].shared_identifiers = vec![u64::MAX];
        page_hints.entries[0].shared_numerators = vec![0];
        let mut warnings = Vec::new();
        assert!(check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: outline_hints.as_ref(),
                h_offset,
                h_length,
                collect_soft_warnings: true,
                warnings: &mut warnings,
            },
        )
        .is_err());

        let (mut pdf, pages, h_offset, h_length, page_hints, shared_hints, mut outline_hints) =
            decode_hint_tables_for_test(&outline_bytes);
        let outline_hints = outline_hints.as_mut().expect("outline hints");
        outline_hints.group_length = outline_hints.group_length.wrapping_add(1);
        let mut warnings = Vec::new();
        check_hint_tables(
            &mut pdf,
            &pages,
            HintTableCheckInput {
                page_hints: &page_hints,
                shared_hints: &shared_hints,
                outline_hints: Some(outline_hints),
                h_offset,
                h_length,
                collect_soft_warnings: true,
                warnings: &mut warnings,
            },
        )
        .expect("outline length mismatch is a soft warning");
        assert!(warnings.iter().any(|message| message
            == "incorrect length in outlines table: hint table = 6265; computed = 6264"));
    }

    #[test]
    fn check_linearization_rejects_a_page_offset_hint_tampered_in_the_stream() {
        use flate2::read::ZlibDecoder;
        use flate2::write::ZlibEncoder;
        use flate2::Compression;

        let mut bytes = linearized_fixture_bytes();
        let hint_object = object_offset(&bytes, 5);
        let stream_marker = b"stream\n";
        let stream_start = hint_object
            + bytes[hint_object..]
                .windows(stream_marker.len())
                .position(|window| window == stream_marker)
                .expect("hint stream marker")
            + stream_marker.len();
        let stream_end = stream_start
            + bytes[stream_start..]
                .windows(b"endstream".len())
                .position(|window| window == b"endstream")
                .expect("hint stream terminator");
        assert_eq!(
            bytes[stream_end - 1],
            b'\n',
            "the committed fixture uses a newline before endstream"
        );
        let framing_len = 1;
        let compressed_end = stream_end - framing_len;
        let compressed = bytes[stream_start..compressed_end].to_vec();
        let framing = bytes[compressed_end..stream_end].to_vec();
        let mut decoded = Vec::new();
        ZlibDecoder::new(compressed.as_slice())
            .read_to_end(&mut decoded)
            .expect("hint stream should decode");
        let original = u32::from_be_bytes(decoded[4..8].try_into().expect("page offset field"));
        decoded[4..8].copy_from_slice(&original.wrapping_add(1).to_be_bytes());
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        std::io::Write::write_all(&mut encoder, &decoded).expect("encode hint stream");
        let tampered = encoder.finish().expect("finish hint stream");
        let old_stream_end = stream_end;
        let old_file_len = bytes.len();
        let mut replacement = tampered;
        let tampered_length = replacement.len();
        replacement.extend_from_slice(&framing);
        bytes.splice(stream_start..stream_end, replacement);
        let delta = bytes.len() - old_file_len;

        // Keep the test fixture structurally valid after changing the stream
        // length. The stream itself moves no earlier object; every later xref
        // entry, /E, /T, /Prev, startxref, and /L moves by delta. The
        // qpdf-zlib-compat backend can produce the same compressed length, in
        // which case all of these updates are intentionally no-ops.
        for cursor in 0..bytes.len().saturating_sub(19) {
            if !bytes[cursor..cursor + 10].iter().all(u8::is_ascii_digit)
                || &bytes[cursor + 10..cursor + 19] != b" 00000 n "
            {
                continue;
            }
            let old_offset = std::str::from_utf8(&bytes[cursor..cursor + 10])
                .expect("xref offset digits")
                .parse::<usize>()
                .expect("xref offset");
            if old_offset >= old_stream_end {
                let new_offset = old_offset + delta;
                bytes[cursor..cursor + 10].copy_from_slice(format!("{new_offset:010}").as_bytes());
            }
        }
        let new_len = bytes.len();
        replace_parameter_number(&mut bytes, b"/Length ", tampered_length);
        replace_parameter_number(&mut bytes, b"/L ", new_len);
        replace_parameter_number(&mut bytes, b"/E ", 1198 + delta);
        replace_parameter_number(&mut bytes, b"/T ", 1523 + delta);
        replace_parameter_number(&mut bytes, b"/Prev ", 1515 + delta);
        replace_parameter_value(
            &mut bytes,
            b"/H ",
            format!("[601 {}]", 118 + delta).as_bytes(),
        );
        let startxref_key = b"startxref\n";
        let startxref_key_start = bytes
            .windows(startxref_key.len())
            .rposition(|window| window == startxref_key)
            .expect("final startxref key");
        let startxref_start = startxref_key_start + startxref_key.len();
        let startxref_end = startxref_start
            + bytes[startxref_start..]
                .iter()
                .position(|&byte| is_pdf_whitespace(byte))
                .expect("final startxref value");
        let startxref_value = "216".to_owned();
        assert!(startxref_value.len() <= startxref_end - startxref_start);
        let mut padded = vec![b'0'; startxref_end - startxref_start];
        let startxref_digits = padded.len() - startxref_value.len();
        padded[startxref_digits..].copy_from_slice(startxref_value.as_bytes());
        bytes[startxref_start..startxref_end].copy_from_slice(&padded);

        let error = check_linearization_bytes(&bytes)
            .expect_err("tampering the Page Offset table must fail linearization checking");
        assert!(error
            .to_string()
            .contains("first page object offset mismatch"));
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
    fn warning_check_accumulates_a_valid_nonfirst_page_object_mismatch() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/golden/references/two-page/linearize.pdf"
        ))
        .to_vec();
        let mut pdf = Pdf::open_mem_owned(bytes.clone()).expect("two-page fixture should open");
        let candidate = pdf
            .linearization_candidate_ref()
            .expect("candidate probe should work")
            .expect("fixture should have a linearization object");
        let candidate_handle = pdf.get_object_handle(candidate);
        candidate_handle
            .try_dereference()
            .expect("candidate should resolve");
        candidate_handle
            .replace_key(b"/O", ObjectHandle::integer(1))
            .expect("candidate should be mutable");

        let warnings = check_linearization_warnings(&mut pdf, &bytes, false)
            .expect("soft mismatch should not abort the warning route");
        assert!(warnings
            .iter()
            .any(|message| message == "first page object (/O) mismatch"));
    }

    #[test]
    fn warning_check_accumulates_an_e_mismatch() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/linearized-one-page.pdf"
        ))
        .to_vec();
        let mut pdf = Pdf::open_mem_owned(bytes.clone()).expect("linearized fixture should open");
        let candidate = pdf
            .linearization_candidate_ref()
            .expect("candidate probe should work")
            .expect("fixture should have a linearization object");
        let candidate_handle = pdf.get_object_handle(candidate);
        candidate_handle
            .try_dereference()
            .expect("candidate should resolve");
        candidate_handle
            .replace_key(b"/E", ObjectHandle::integer(0))
            .expect("candidate should be mutable");

        let warnings = check_linearization_warnings(&mut pdf, &bytes, false)
            .expect("soft mismatch should not abort the warning route");
        assert!(warnings
            .iter()
            .any(|message| message.starts_with("end of first page section (/E) mismatch:")));
    }

    #[test]
    fn warning_check_skips_o_revalidation_for_a_negative_o_after_the_parameter_preflight() {
        // qpdf's `/O` check (`QPDF_linearization.cc:428-433`) is a plain
        // `int` comparison against the first page's object number; it never
        // rejects a negative `/O` as malformed. `check_linearization_parameters`
        // (the preflight `job/check.rs` runs first) reports exactly that soft
        // mismatch and no more; the caller then sets `skip_first_page_warning`
        // and calls `check_linearization_warnings`, which must not re-run
        // `as_u64` on `/O` at that point -- `as_u64` rejects a negative
        // integer as an `InvalidParam` error, which would turn a file qpdf
        // only warns about into a hard failure.
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/linearized-one-page.pdf"
        ))
        .to_vec();
        let mut pdf = Pdf::open_mem_owned(bytes.clone()).expect("linearized fixture should open");
        let candidate = pdf
            .linearization_candidate_ref()
            .expect("candidate probe should work")
            .expect("fixture should have a linearization object");
        let candidate_handle = pdf.get_object_handle(candidate);
        candidate_handle
            .try_dereference()
            .expect("candidate should resolve");
        candidate_handle
            .replace_key(b"/O", ObjectHandle::integer(-1))
            .expect("candidate should be mutable");

        assert_eq!(
            check_linearization_parameters(&mut pdf).expect("preflight should not error"),
            LinearizationParameterCheck::Warning("first page object (/O) mismatch"),
            "the preflight itself must treat a negative /O as a soft mismatch"
        );

        let warnings = check_linearization_warnings(&mut pdf, &bytes, true)
            .expect("a negative /O must not be re-validated once the preflight already warned");
        assert!(
            !warnings
                .iter()
                .any(|message| message.starts_with("first page object (/O)")),
            "the warning route must not duplicate the preflight's /O warning: {warnings:?}"
        );
    }

    #[test]
    fn warning_check_reports_t_mismatch_before_e_mismatch() {
        // qpdf checks /T (`QPDF_linearization.cc:452-470`) before /E
        // (`:496-524`) in `checkLinearizationInternal`, so a file with both
        // mismatches must report the /T warning first.
        let mut bytes = linearized_fixture_bytes();
        let xref_offset = bytes
            .windows(b"xref\n0".len())
            .rposition(|window| window == b"xref\n0")
            .expect("classic xref section");
        replace_parameter_number(&mut bytes, b"/T ", xref_offset);
        let beyond_part6 = bytes.len() - 1;
        replace_parameter_number(&mut bytes, b"/E ", beyond_part6);

        let mut pdf = Pdf::open_mem_owned(bytes.clone()).expect("fixture should open");
        let warnings = check_linearization_warnings(&mut pdf, &bytes, false)
            .expect("both mismatches should be soft warnings");
        let t_index = warnings
            .iter()
            .position(|message| message.starts_with("space before first xref item (/T) mismatch"))
            .expect("/T mismatch warning must be present");
        let e_index = warnings
            .iter()
            .position(|message| message.starts_with("end of first page section (/E) mismatch:"))
            .expect("/E mismatch warning must be present");
        assert!(t_index < e_index, "qpdf reports /T before /E: {warnings:?}");
    }

    #[test]
    fn strict_check_rejects_an_object_number_that_does_not_fit_u32() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/linearized-one-page.pdf"
        ))
        .to_vec();
        let mut pdf = Pdf::open_mem_owned(bytes.clone()).expect("linearized fixture should open");
        let candidate = pdf
            .linearization_candidate_ref()
            .expect("candidate probe should work")
            .expect("fixture should have a linearization object");
        let candidate_handle = pdf.get_object_handle(candidate);
        candidate_handle
            .try_dereference()
            .expect("candidate should resolve");
        candidate_handle
            .replace_key(b"/O", ObjectHandle::integer(i64::MAX))
            .expect("candidate should be mutable");

        let error = check_linearization_warnings(&mut pdf, &bytes, false)
            .expect_err("out-of-range /O should remain a hard validation error");
        assert!(error.to_string().contains("does not fit in u32"));
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
    fn extent_walk_ignores_a_missing_object_source_extent_like_qpdf() {
        let bytes = source_extent_graph_fixture();
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open extent graph");
        // Materialize a fresh object with no physical source extent and route
        // it into part 6 through /Outlines (the fixture already carries
        // /PageMode /UseOutlines). qpdf's own optimize() can hand
        // calculateLinearizationData an object with no parsed byte position
        // at all — e.g. a non-indirect /Outlines dictionary qpdf forces
        // indirect before classification (`QPDF_optimization.cc:67-76`).
        let injected_ref = ObjectRef::new(50, 0);
        pdf.set_object(
            injected_ref,
            crate::Object::Dictionary(crate::Dictionary::new()),
        );
        let root_ref = ObjectRef::new(1, 0);
        let mut root = match pdf.resolve_object(root_ref).expect("resolve root") {
            crate::Object::Dictionary(dict) => dict,
            other => panic!("expected root dictionary, got {other:?}"), // cov:ignore: defensive; the fixture's root is always a dictionary
        };
        root.insert("Outlines", crate::Object::Reference(injected_ref));
        pdf.set_object(root_ref, crate::Object::Dictionary(root));

        let (end_before_space, end_after_space) = first_page_source_extent(&mut pdf)
            .expect("a missing source extent must not abort qpdf's part-6 envelope");
        // The injected object (50 0 obj) is the only part-6 member with no
        // parse extent; `inherited_attrs.rs`'s push now writes back a
        // /Pages node or leaf only when its own dictionary actually
        // changes (`QPDF_optimization.cc:200,222-227` only calls
        // `removeKey`/`replaceKey` on an actual pull-up), so every other
        // part-6 object's real extent survives this walk and the max-fold
        // still produces a genuine positive envelope -- proving the
        // injected object was skipped in isolation rather than degrading
        // the whole computation to the missing-extent sentinel.
        assert_ne!((end_before_space, end_after_space), (-1, -1));
        assert_eq!((end_before_space, end_after_space), (237, 238));
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
                    if message.contains("does not match the first xref item offset")
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
    fn check_rejects_an_e_value_at_or_beyond_file_length() {
        let mut bytes = linearized_fixture_bytes();
        let file_len = bytes.len();
        replace_parameter_number(&mut bytes, b"/E ", file_len);
        let result = check_linearization_bytes(&bytes);
        assert!(
            matches!(
                result,
                Err(LinearizationCheckError::InvalidParam { ref message })
                    if message.contains("must be less than file length")
            ),
            "an /E at file length must be rejected before the source-extent comparison: {result:?}"
        );
    }

    #[test]
    fn check_rejects_a_t_value_inside_an_xref_entry() {
        // /T must point at the `xref` keyword or inside its subsection
        // header line, not into the entries themselves.
        let mut bytes = linearized_fixture_bytes();
        let xref_offset = bytes
            .windows(b"xref\n0".len())
            .rposition(|window| window == b"xref\n0")
            .expect("classic xref section");
        // `xref\n0 3\n` is 9 bytes; land inside the first 20-byte entry.
        let inside_first_entry = xref_offset + 9 + 5;
        replace_parameter_number(&mut bytes, b"/T ", inside_first_entry);
        let result = check_linearization_bytes(&bytes);
        assert!(
            matches!(
                result,
                Err(LinearizationCheckError::InvalidParam { ref message })
                    if message.contains("does not match the first xref item offset")
            ),
            "a /T pointing into an xref entry must be rejected: {result:?}"
        );
    }

    #[test]
    fn warning_check_treats_a_malformed_t_backscan_as_a_soft_warning() {
        let mut bytes = linearized_fixture_bytes();
        let appended_region_start = bytes.len();
        bytes.extend_from_slice(b"\n% xref not-a-number\n");
        let malformed_xref_pos = appended_region_start + 3;
        let new_len = bytes.len();
        replace_parameter_number(&mut bytes, b"/L ", new_len);
        replace_parameter_number(&mut bytes, b"/T ", malformed_xref_pos);

        let mut pdf = Pdf::open_mem_owned(bytes.clone()).expect("linearized fixture should open");
        let warnings = check_linearization_warnings(&mut pdf, &bytes, false)
            .expect("qpdf treats an unparseable /T neighborhood as a warning");
        assert!(
            warnings.iter().any(|message| {
                message.starts_with("space before first xref item (/T) mismatch")
            }),
            "qpdf's /T warning must survive a malformed backscan neighborhood: {warnings:?}"
        );
    }

    #[test]
    fn warning_check_treats_a_t_offset_beyond_eof_as_a_position_warning() {
        let mut bytes = linearized_fixture_bytes();
        replace_parameter_number(&mut bytes, b"/T ", 9999);
        let mut pdf = Pdf::open_mem_owned(bytes.clone()).expect("linearized fixture should open");

        let warnings = check_linearization_warnings(&mut pdf, &bytes, false)
            .expect("qpdf does not structurally parse a /T neighborhood");
        assert!(
            warnings
                .iter()
                .any(|message| message.starts_with("space before first xref item (/T) mismatch")),
            "the out-of-range /T must remain a position warning: {warnings:?}"
        );
    }

    #[test]
    fn xref_parser_records_qpdf_first_xref_item_offset() {
        let bytes = linearized_fixture_bytes();
        let pdf = Pdf::open_mem_owned(bytes).expect("linearized fixture should open");

        assert_eq!(
            pdf.first_xref_item_offset(),
            1524,
            "the xref parser must expose the offset qpdf uses for /T"
        );
    }

    #[test]
    fn first_xref_item_offset_survives_initial_xref_reconstruction() {
        // Keep xref object 0's row intact, but make object 1's fixed-width row
        // invalid. qpdf records the first row's offset before parsing object 1
        // and retains it when read_xref falls back to reconstruct_xref
        // (QPDF.cc:846-869, 626-708), so the later /T check has no false
        // mismatch warning.
        let mut bytes = linearized_fixture_bytes();
        bytes[1544..1554].copy_from_slice(b"XXXXXXXXXX");

        let result = check_linearization_bytes(&bytes);
        assert!(
            result.is_ok(),
            "xref recovery lost qpdf's first row offset: {result:?}"
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
                    if message.contains("does not match the first xref item offset")
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

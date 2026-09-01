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
    /// file is missing or does not
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
    /// candidate scan, `/Linearized`, and integer `/L` participate. Structural
    /// candidate resolution failures become `false`, as qpdf's
    /// `QPDF::resolve` converts damaged objects to null
    /// (`libqpdf/QPDF.cc:1700-1753`); logger and source-operation failures are
    /// returned to the caller.
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
        let Some(dictionary) = candidate.try_as_dictionary()? else {
            return Ok(false);
        };

        let Some(linearized) = dictionary.get(&b"/Linearized"[..]) else {
            return Ok(false);
        };
        linearized.try_dereference()?;
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
            l_value.try_dereference()?;
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
    let optimization =
        Optimization::optimize(pdf, &object_stream_data, false, |_, _stream| Ok(0u8))?;

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
            // semantics after canonical replacement clears its cache extent.
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
    Ok(outlines.object_ref())
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
    let optimization = Optimization::optimize(pdf, &object_stream_data, false, |_, _| Ok(0u8))?;

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
    file_len: u64,
    collect_soft_warnings: bool,
    warnings: &mut Vec<String>,
) -> std::result::Result<i128, LinearizationCheckError> {
    // A valid object sequence cannot contain more entries than the input file
    // can contain bytes, nor drastically more entries than the document
    // actually has objects. Combine both bounds: `file_len` alone is too
    // permissive for a large file with a sparse xref (a small malformed PDF
    // padded with junk bytes could still claim a hint count near the total
    // file size), and the xref-derived bound alone is too permissive for a
    // tiny file (a few-KB PDF could still claim up to xref.len() + 1_000_000
    // objects). Taking the smaller of the two keeps a corrupt 32-bit hint
    // count from turning the checker into an unbounded loop while preserving
    // every valid PDF (each indirect object consumes at least one byte, and a
    // valid hint table never claims more objects than the document has).
    // qpdf 11.9.0's lengthNextN has no corresponding bound
    // (libqpdf/QPDF_linearization.cc:589-604); this is defense-in-depth for
    // malformed input and does not change valid linearization output.
    let bound = file_len.min(xref.len() as u64 + 1_000_000);
    if nobjects > bound {
        return Err(LinearizationCheckError::InvalidParam {
            message: format!("hint table object count {nobjects} exceeds bound {bound}"),
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
    file_len: u64,
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
        file_len,
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
                file_len,
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
            file_len,
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
            file_len,
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
    // materializing a separate stream value.
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
    let (hint_obj, hint_object_damage_offset) =
        pdf.resolve_at_offset_with_damage_offset(offset as u64, hint_ref)?;
    // qpdf's readHintStream supplies no explicit offset for this damage. The
    // resolver returns the operation-specific last offset; retain the source
    // seam as a defensive fallback for recovered empty objects without a
    // trailing token.
    let hint_object_damage_offset =
        hint_object_damage_offset.unwrap_or_else(|| pdf.source_last_offset());
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

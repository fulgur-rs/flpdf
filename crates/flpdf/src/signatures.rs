//! Digital signature helpers.
//!
//! This module has three layers:
//! - read-only AcroForm signature field inspection via [`signatures`];
//! - rewrite-impact checks for whether a writer mode would invalidate existing
//!   signed `/ByteRange`s;
//! - `/AcroForm /SigFlags` primitives ([`acroform_sig_flags`], [`clear_sig_flags`])
//!   that read, surface, and clear the SignaturesExist/AppendOnly bits.

use crate::json_inspect::decode_pdf_text_string;
use crate::{
    Dictionary, Error, FormFieldObjectHelper, Object, ObjectRef, Pdf, Result, WriteOptions,
    DEFAULT_MAX_ACROFORM_DEPTH,
};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

/// Maximum recursion depth for AcroForm signature field traversal.
pub const DEFAULT_MAX_SIGNATURE_FIELD_DEPTH: usize = crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;

/// `/AcroForm /SigFlags` bit 1: the document contains at least one signature field.
pub const SIG_FLAGS_SIGNATURES_EXIST: u32 = 1;
/// `/AcroForm /SigFlags` bit 2 (append-only): the document must only be modified
/// via incremental updates so existing signatures stay valid.
pub const SIG_FLAGS_APPEND_ONLY: u32 = 2;

/// Read-only information about a signed AcroForm signature field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureInfo {
    /// The AcroForm field object containing `/FT /Sig`.
    pub field_ref: ObjectRef,
    /// The field's `/V` signature dictionary reference, when `/V` is indirect.
    pub signature_ref: Option<ObjectRef>,
    /// Dot-joined AcroForm field name path.
    pub field_name: String,
    /// Parsed `/ByteRange` array from the signature dictionary.
    pub byte_range: [u64; 4],
    /// `/SubFilter` name, such as `adbe.pkcs7.detached`.
    pub sub_filter: Option<String>,
    /// Signer name from the signature dictionary's `/Name` entry.
    pub signer_name: Option<String>,
    /// Signing time from the signature dictionary's `/M` entry.
    pub signing_time: Option<String>,
    /// Signature reason from `/Reason`.
    pub reason: Option<String>,
    /// Signature location from `/Location`.
    pub location: Option<String>,
    /// Signature contact information from `/ContactInfo`.
    pub contact_info: Option<String>,
    /// Raw `/Cert` bytes when the signature dictionary exposes a certificate.
    pub certificate: Option<Vec<u8>>,
}

/// Writer mode used for digital-signature preservation checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureWriteMode {
    /// Append an incremental update section after the original bytes.
    Incremental,
    /// Re-emit the whole document, changing signed byte positions.
    FullRewrite,
}

impl SignatureWriteMode {
    fn from_options(options: &WriteOptions) -> Self {
        if options.full_rewrite {
            Self::FullRewrite
        } else {
            Self::Incremental
        }
    }
}

/// Why a rewrite is considered safe or unsafe for existing signatures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureRewriteReason {
    /// No signature fields or `/ByteRange` signature dictionaries were found.
    NoSignatures,
    /// A full rewrite changes object byte positions, invalidating signed ranges.
    FullRewrite,
    /// Incremental update touches the document `/AcroForm` dictionary.
    IncrementalTouchesAcroForm,
    /// Incremental update touches a signature field or `/ByteRange` dictionary.
    IncrementalTouchesSignedObject,
    /// Incremental update appends only unrelated objects.
    IncrementalPreservesSignedByteRanges,
}

/// Result of checking whether a write would preserve existing signatures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureRewriteImpact {
    /// Whether the source document contains signature evidence.
    pub has_signatures: bool,
    /// Whether the requested write mode would invalidate existing signatures.
    pub invalidates_signatures: bool,
    /// Decision reason.
    pub reason: SignatureRewriteReason,
    /// The first touched object that caused an invalidating decision, if any.
    pub first_invalidating_ref: Option<ObjectRef>,
    /// Signature fields and signature dictionaries discovered in the source.
    pub signed_object_refs: Vec<ObjectRef>,
    /// Dirty object refs that an incremental write would append/rewrite.
    pub touched_object_refs: Vec<ObjectRef>,
    /// The object that contains `/AcroForm`, when `/AcroForm` exists.
    pub acroform_ref: Option<ObjectRef>,
    /// The `/AcroForm /SigFlags` bitfield, when present.
    ///
    /// Bit 1 ([`SIG_FLAGS_SIGNATURES_EXIST`]) and bit 2
    /// ([`SIG_FLAGS_APPEND_ONLY`]) are interpreted by [`Self::signatures_exist`]
    /// and [`Self::append_only`]. The flag itself does not change the
    /// `invalidates_signatures` decision; an enforcement layer reads
    /// [`Self::append_only`] to require append-only (incremental) mode.
    pub sig_flags: Option<u32>,
}

impl SignatureRewriteImpact {
    /// Whether `/SigFlags` has the SignaturesExist bit set.
    pub fn signatures_exist(&self) -> bool {
        self.sig_flags
            .is_some_and(|flags| flags & SIG_FLAGS_SIGNATURES_EXIST != 0)
    }

    /// Whether `/SigFlags` has the AppendOnly bit set.
    ///
    /// When set, the document declares that it must only be modified via
    /// incremental updates; a full rewrite would violate the append-only policy.
    pub fn append_only(&self) -> bool {
        self.sig_flags
            .is_some_and(|flags| flags & SIG_FLAGS_APPEND_ONLY != 0)
    }
}

/// Return all signed AcroForm signature fields in document field order.
///
/// # Errors
///
/// - Propagates any error from resolving catalog, `/AcroForm`, and field-tree
///   objects (for example I/O or parse failures surfaced by [`Pdf::resolve`]).
/// - [`Error::Parse`] when a signature field's `/ByteRange` is malformed (not a
///   four-element array of non-negative integers).
pub fn signatures<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Vec<SignatureInfo>> {
    signatures_with_max_depth(pdf, DEFAULT_MAX_SIGNATURE_FIELD_DEPTH)
}

/// Like [`signatures`], but with an explicit field-tree recursion limit.
///
/// # Errors
///
/// - Propagates any error from resolving catalog, `/AcroForm`, and field-tree
///   objects (for example I/O or parse failures surfaced by [`Pdf::resolve`]).
/// - [`Error::Parse`] when a signature field's `/ByteRange` is malformed (not a
///   four-element array of non-negative integers).
pub fn signatures_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    max_depth: usize,
) -> Result<Vec<SignatureInfo>> {
    let Some(catalog_ref) = pdf.root_ref() else {
        return Ok(Vec::new());
    };
    let catalog_obj = pdf.resolve_borrowed(catalog_ref)?;
    let Object::Dictionary(catalog_dict) = catalog_obj else {
        return Ok(Vec::new());
    };
    let Some(acroform_val) = catalog_dict.get("AcroForm").cloned() else {
        return Ok(Vec::new());
    };
    let Some(acroform_dict) = resolve_dictionary(pdf, acroform_val)? else {
        return Ok(Vec::new());
    };

    let Some(fields_obj) = acroform_dict.get("Fields").cloned() else {
        return Ok(Vec::new());
    };
    let fields = resolve_array(pdf, fields_obj)?;
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    for field in fields {
        if let Object::Reference(field_ref) = field {
            walk_signature_field(pdf, field_ref, "", &mut output, &mut seen, 0, max_depth)?;
        }
    }
    Ok(output)
}

/// Return `true` when writing `pdf` with `options` would invalidate signatures.
///
/// This is a convenience predicate over [`signature_rewrite_impact`]. It maps
/// `WriteOptions::full_rewrite = true` to full rewrite and the default writer
/// path to incremental update.
///
/// # Errors
///
/// Propagates any error from [`signature_rewrite_impact`], that is any error
/// from resolving catalog, `/AcroForm`, and signature objects (surfaced by
/// [`Pdf::resolve`]), and [`Error::Unsupported`] when the signature field-tree
/// or known-container recursion depth limit is exceeded.
pub fn would_rewrite_invalidate_signatures<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: &WriteOptions,
) -> Result<bool> {
    Ok(
        signature_rewrite_impact(pdf, SignatureWriteMode::from_options(options))?
            .invalidates_signatures,
    )
}

/// Read the document `/AcroForm /SigFlags` bitfield, if present.
///
/// Returns `None` when there is no `/AcroForm`, no `/SigFlags`, or the value is
/// not a non-negative integer that fits in `u32`. An indirect `/SigFlags`
/// reference (vanishingly rare for a scalar flag) is treated as absent.
///
/// # Errors
///
/// Propagates any error from resolving the catalog and `/AcroForm` objects (for
/// example I/O or parse failures surfaced by [`Pdf::resolve`]).
pub fn acroform_sig_flags<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Option<u32>> {
    Ok(resolve_catalog_acroform(pdf)?
        .and_then(|(_, acroform)| sig_flags_from_acroform_dict(&acroform)))
}

/// Clear the signature-related bits of `/AcroForm /SigFlags`.
///
/// Masks off [`SIG_FLAGS_SIGNATURES_EXIST`] and [`SIG_FLAGS_APPEND_ONLY`] and
/// writes the masked integer back (e.g. `/SigFlags 3` becomes `/SigFlags 0`),
/// marking the containing object dirty. Returns `true` when a bit was actually
/// cleared. Used by the opt-in signature-stripping path; it does not by itself
/// remove signature fields or `/V` dictionaries.
///
/// # Errors
///
/// Propagates any error from resolving the catalog and `/AcroForm` objects (for
/// example I/O or parse failures surfaced by [`Pdf::resolve`]).
pub fn clear_sig_flags<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<bool> {
    let Some((home, mut acroform)) = resolve_catalog_acroform(pdf)? else {
        return Ok(false);
    };
    if !clear_sig_flags_in_dict(&mut acroform) {
        return Ok(false);
    }
    write_back_acroform(pdf, home, acroform);
    Ok(true)
}

/// Remove qpdf-supported security restrictions, mirroring
/// `QPDF::removeSecurityRestrictions` (qpdf 11.9.0).
///
/// Drops the catalog `/Perms` entry unconditionally and, when `/AcroForm` is a
/// dictionary that carries `/SigFlags`, sets `/SigFlags` to `0`. Returns `true`
/// when either change was applied. Used by the `--remove-restrictions`
/// signature-stripping path; it does not remove signature fields.
///
/// # Errors
///
/// Propagates any error from resolving the catalog and `/AcroForm` objects
/// (for example I/O or parse failures surfaced by [`Pdf::resolve`]).
pub fn remove_security_restrictions<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<bool> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(false);
    };
    let Object::Dictionary(mut catalog) = pdf.resolve(root_ref)? else {
        return Ok(false);
    };
    let mut changed = false;
    if catalog.remove("Perms").is_some() {
        pdf.set_object(root_ref, Object::Dictionary(catalog));
        changed = true;
    }
    if let Some((home, mut acroform)) = resolve_catalog_acroform(pdf)? {
        if acroform.get("SigFlags").is_some() && sig_flags_from_acroform_dict(&acroform) != Some(0)
        {
            acroform.insert("SigFlags", Object::Integer(0));
            write_back_acroform(pdf, home, acroform);
            changed = true;
        }
    }
    Ok(changed)
}

/// Disable digital signatures for `--remove-restrictions`, mirroring qpdf's
/// `QPDFAcroFormDocumentHelper::disableDigitalSignatures` (qpdf 11.9.0).
///
/// 1. Calls [`remove_security_restrictions`] (drop catalog `/Perms`, zero
///    `/AcroForm /SigFlags`).
/// 2. Enumerates the document's AcroForm form fields the way qpdf's
///    `getFormFields` does: terminal annotation-fields discovered while walking
///    `/AcroForm /Fields`, plus orphan `/Subtype /Widget` annotations on page
///    `/Annots` that were not associated with a field during that walk.
/// 3. For every enumerated form field whose inherited `/FT` is `/Sig`, removes
///    `/FT`, `/V`, `/SV`, and `/Lock` (the field name `/T` is preserved). The
///    signature dictionary previously referenced by `/V` is not deleted
///    explicitly; on a full rewrite the reachability-based garbage collector
///    drops it when it is no longer referenced, but keeps it when it is still
///    reachable elsewhere (for example from the catalog `/DSS`).
/// 4. Erases those fields' references from the top-level `/AcroForm /Fields`
///    array. An indirect `/Fields` array is mutated in place (the array object
///    is kept and the `/AcroForm /Fields` entry stays indirect); a direct
///    `/Fields` array is rewritten inside the `/AcroForm` dictionary. On a full
///    rewrite a field still reachable from a page `/Annots` survives as a plain
///    annotation; a field-only entry becomes unreferenced and is dropped by
///    garbage collection.
///
/// A `/Sig` field whose `/FT`/`/V` live on a non-terminal parent (the parent
/// groups a widget via `/Kids`) is left intact: only the widget is a form
/// field, it carries no signature keys of its own, and it is not a top-level
/// `/Fields` entry, so the signature survives — matching qpdf.
///
/// Returns `true` when anything changed. `/DSS` is intentionally left untouched,
/// matching qpdf (`removeSecurityRestrictions` removes only `/Perms`).
///
/// # Errors
///
/// Propagates any error from resolving the catalog, `/AcroForm`, `/Fields`,
/// page, and field-tree objects (surfaced by [`Pdf::resolve`]), and
/// [`Error::Unsupported`] when the field-tree traversal depth limit
/// ([`DEFAULT_MAX_ACROFORM_DEPTH`]) or a field's `/Parent` chain depth limit is
/// exceeded.
pub fn disable_digital_signatures<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<bool> {
    let mut changed = remove_security_restrictions(pdf)?;

    let form_fields = collect_signature_form_field_refs(pdf)?;

    let mut to_remove: Vec<ObjectRef> = Vec::new();
    for field_ref in form_fields {
        let field_type = FormFieldObjectHelper::new(field_ref, pdf).field_type()?;
        if field_type.as_deref() != Some(b"Sig") {
            continue;
        }
        // qpdf records every /Sig form field in `to_remove` unconditionally,
        // before attempting removeKey. A field whose /FT//V live on a
        // non-terminal parent has nothing of its own to strip, but is still
        // recorded (removeFormFields then finds it absent from /Fields).
        to_remove.push(field_ref);

        // `field_type` above resolved this ref through the field tree, so it is
        // always a dictionary here.
        let Object::Dictionary(mut dict) = pdf.resolve(field_ref)? else {
            continue; // cov:ignore: a /Sig form-field ref always resolves to a dictionary
        };
        let mut field_changed = false;
        for key in ["FT", "V", "SV", "Lock"] {
            if dict.remove(key).is_some() {
                field_changed = true;
            }
        }
        if field_changed {
            pdf.set_object(field_ref, Object::Dictionary(dict));
            changed = true;
            // The old /V target is intentionally not deleted here. qpdf's
            // disableDigitalSignatures only strips the field keys and lets the
            // write-time reachability GC drop the signature dictionary if it is
            // now unreferenced. A dictionary still reachable elsewhere (for
            // example from the catalog /DSS) must survive, so deleting it here
            // would over-delete and leave a dangling reference.
        }
    }

    // removeFormFields: erase the recorded refs from the top-level /AcroForm
    // /Fields array. qpdf runs this unconditionally; with an empty `to_remove`
    // nothing matches and the array is left untouched. Only refs that are
    // actually top-level /Fields entries are dropped; a field reachable only via
    // a parent's /Kids is unaffected.
    let Some((home, mut acroform)) = resolve_catalog_acroform(pdf)? else {
        return Ok(changed);
    };
    let Some(fields_obj) = acroform.get("Fields").cloned() else {
        return Ok(changed);
    };
    // qpdf erases items from the original /Fields array handle. Capture whether
    // /Fields is stored indirectly before `resolve_array` consumes the value: an
    // indirect array stays indirect (the array object is mutated in place, so
    // the /AcroForm /Fields entry keeps its reference), while a direct array
    // stays direct (rewritten inside the /AcroForm dictionary).
    let fields_ref = fields_obj.as_ref_id();
    let fields = resolve_array(pdf, fields_obj)?;
    let original_len = fields.len();
    let new_fields: Vec<Object> = fields
        .into_iter()
        .filter(|f| !matches!(f, Object::Reference(r) if to_remove.contains(r)))
        .collect();
    if new_fields.len() != original_len {
        match fields_ref {
            Some(fields_ref) => pdf.set_object(fields_ref, Object::Array(new_fields)),
            None => {
                acroform.insert("Fields", Object::Array(new_fields));
                write_back_acroform(pdf, home, acroform);
            }
        }
        changed = true;
    }

    Ok(changed)
}

/// Collect the object refs of every AcroForm form field, mirroring the
/// `field_to_annotations` map keys built by qpdf's
/// `QPDFAcroFormDocumentHelper::analyze` + `traverseField` +
/// `getFormFields` (qpdf 11.9.0).
///
/// Returns an empty set when the catalog `/AcroForm` is absent, is not a
/// dictionary, or carries no `/Fields` key — in which case qpdf's `analyze`
/// returns before its page `/Annots` orphan-widget pass, so that pass is
/// skipped here too.
fn collect_signature_form_field_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> Result<BTreeSet<ObjectRef>> {
    let mut form_field_keys: BTreeSet<ObjectRef> = BTreeSet::new();
    // The ObjGens of annotation nodes already associated with a field (qpdf's
    // annotation_to_field key set), so the orphan pass skips them.
    let mut annotations_seen: BTreeSet<ObjectRef> = BTreeSet::new();

    let Some((_, acroform)) = resolve_catalog_acroform(pdf)? else {
        return Ok(form_field_keys);
    };
    let Some(fields_obj) = acroform.get("Fields").cloned() else {
        return Ok(form_field_keys);
    };
    // A present-but-non-array /Fields is treated as empty (qpdf warns and uses
    // an empty array), but the orphan pass below still runs.
    let fields = resolve_array(pdf, fields_obj)?;

    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    for field in fields {
        if let Object::Reference(field_ref) = field {
            traverse_field(
                pdf,
                field_ref,
                None,
                0,
                &mut visited,
                &mut form_field_keys,
                &mut annotations_seen,
            )?;
        }
    }

    // Orphan page-widget pass: a /Subtype /Widget annotation reachable from a
    // page /Annots that was not associated with a field during the /Fields
    // traversal becomes its own form field.
    for page_ref in crate::pages::page_refs(pdf)? {
        for annot_ref in page_widget_annotation_refs(pdf, page_ref)? {
            if annotations_seen.insert(annot_ref) {
                form_field_keys.insert(annot_ref);
            }
        }
    }

    Ok(form_field_keys)
}

/// One node of qpdf's `traverseField`: classify `field_ref` as a field and/or
/// annotation, recording the owning form-field ref when it is an annotation,
/// and recurse into an array `/Kids`.
fn traverse_field<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_ref: ObjectRef,
    parent_ref: Option<ObjectRef>,
    depth: usize,
    visited: &mut BTreeSet<ObjectRef>,
    form_field_keys: &mut BTreeSet<ObjectRef>,
    annotations_seen: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    if depth > DEFAULT_MAX_ACROFORM_DEPTH {
        return Err(Error::Unsupported(format!(
            "AcroForm field tree depth exceeds maximum of {DEFAULT_MAX_ACROFORM_DEPTH} at {field_ref}"
        )));
    }
    // Non-dictionary fields/annotations are ignored (qpdf warns and skips).
    let Object::Dictionary(dict) = pdf.resolve(field_ref)? else {
        return Ok(());
    };
    // Loop guard, keyed on the object ref (qpdf's ObjGen visited set).
    if !visited.insert(field_ref) {
        return Ok(());
    }

    // A terminal field that looks like an annotation is an annotation (merged
    // widget/field). A node with an array /Kids groups sub-fields instead.
    let mut is_annotation = false;
    let mut is_field = depth == 0;

    if let Some(kids) = resolve_kids_array(pdf, &dict)? {
        is_field = true;
        for kid in kids {
            if let Object::Reference(kid_ref) = kid {
                traverse_field(
                    pdf,
                    kid_ref,
                    Some(field_ref),
                    depth + 1,
                    visited,
                    form_field_keys,
                    annotations_seen,
                )?;
            }
        }
    } else {
        if dict.get("Parent").is_some() {
            is_field = true;
        }
        if dict.get("Subtype").is_some() || dict.get("Rect").is_some() || dict.get("AP").is_some() {
            is_annotation = true;
        }
    }

    if is_annotation {
        // our_field = is_field ? field : parent. `is_field` is false only when
        // depth > 0, where the caller always supplies a parent, so the
        // fallback is never reached.
        let our_field = if is_field {
            field_ref
        } else {
            parent_ref.unwrap_or(field_ref)
        };
        form_field_keys.insert(our_field);
        annotations_seen.insert(field_ref);
    }

    Ok(())
}

/// Resolve a dictionary's `/Kids` value to its array items, following one
/// indirect reference. Returns `None` when `/Kids` is absent or does not
/// resolve to an array (qpdf's `kids.isArray()` gate), distinguishing that from
/// an empty array (`Some(vec![])`).
fn resolve_kids_array<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &Dictionary,
) -> Result<Option<Vec<Object>>> {
    match dict.get("Kids").cloned() {
        Some(Object::Array(items)) => Ok(Some(items)),
        Some(Object::Reference(kids_ref)) => match pdf.resolve(kids_ref)? {
            Object::Array(items) => Ok(Some(items)),
            _ => Ok(None),
        },
        _ => Ok(None),
    }
}

/// Return the object refs of the `/Subtype /Widget` annotations in a leaf
/// page's `/Annots` array, mirroring qpdf's `getWidgetAnnotationsForPage`
/// (`QPDFPageObjectHelper::getAnnotations("/Widget")`). An indirect `/Annots`
/// array is resolved; non-reference entries are skipped.
fn page_widget_annotation_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Vec<ObjectRef>> {
    let annots_obj = match pdf.resolve(page_ref)? {
        Object::Dictionary(page) => page.get("Annots").cloned(),
        _ => None,
    };
    let Some(annots_obj) = annots_obj else {
        return Ok(Vec::new());
    };

    let mut widgets = Vec::new();
    for annot in resolve_array(pdf, annots_obj)? {
        let Object::Reference(annot_ref) = annot else {
            continue;
        };
        if let Object::Dictionary(annot_dict) = pdf.resolve(annot_ref)? {
            if matches!(annot_dict.get("Subtype"), Some(Object::Name(name)) if name.as_slice() == b"Widget")
            {
                widgets.push(annot_ref);
            }
        }
    }
    Ok(widgets)
}

/// Write an updated `/AcroForm` dictionary back to wherever it lives.
///
/// For an indirect `/AcroForm` the dictionary is stored to its own object; for
/// an inline `/AcroForm` the carried catalog is patched and re-stored so the
/// catalog is not clobbered.
fn write_back_acroform<R: Read + Seek>(pdf: &mut Pdf<R>, home: AcroformHome, acroform: Dictionary) {
    match home {
        AcroformHome::Object(acroform_ref) => {
            pdf.set_object(acroform_ref, Object::Dictionary(acroform));
        }
        AcroformHome::Inline {
            root_ref,
            mut catalog,
        } => {
            catalog.insert("AcroForm", Object::Dictionary(acroform));
            pdf.set_object(root_ref, Object::Dictionary(catalog));
        }
    }
}

/// Remove signature values (`/V`) from AcroForm signature fields.
///
/// The field dictionaries themselves are preserved so widgets and field names
/// remain in place, but signed fields no longer point at a signature
/// dictionary. Returns `true` when at least one field value was removed.
///
/// # Errors
///
/// Propagates any error from resolving the catalog, `/AcroForm`, `/Fields`, and
/// field-tree objects (for example I/O or parse failures surfaced by
/// [`Pdf::resolve`]).
pub fn strip_signature_values<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<bool> {
    let Some((_, mut acroform)) = resolve_catalog_acroform(pdf)? else {
        return Ok(false);
    };
    let Some(fields_obj) = acroform.remove("Fields") else {
        return Ok(false);
    };

    let mut changed = false;
    let mut seen = BTreeSet::new();
    for field in resolve_array(pdf, fields_obj)? {
        let Object::Reference(field_ref) = field else {
            continue;
        };
        strip_signature_values_from_field(pdf, field_ref, None, 0, &mut seen, &mut changed)?;
    }
    Ok(changed)
}

/// Where the catalog `/AcroForm` dictionary lives, so an updated copy can be
/// written back to the correct object.
enum AcroformHome {
    /// `/AcroForm` is an indirect object; write the updated dict to this ref.
    Object(ObjectRef),
    /// `/AcroForm` is an inline dictionary in the catalog; carries the catalog
    /// so the entry can be replaced without re-resolving `/Root`.
    Inline {
        root_ref: ObjectRef,
        catalog: Dictionary,
    },
}

/// Resolve the catalog `/AcroForm` to its dictionary plus where it lives,
/// following one indirect reference. Returns `None` when there is no `/Root`
/// dictionary, no `/AcroForm`, or `/AcroForm` is not a dictionary.
fn resolve_catalog_acroform<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> Result<Option<(AcroformHome, Dictionary)>> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(None);
    };
    let Object::Dictionary(catalog) = pdf.resolve(root_ref)? else {
        return Ok(None);
    };
    let Some(acroform) = catalog.get("AcroForm").cloned() else {
        return Ok(None);
    };
    match acroform {
        Object::Reference(acroform_ref) => match pdf.resolve(acroform_ref)? {
            Object::Dictionary(dict) => Ok(Some((AcroformHome::Object(acroform_ref), dict))),
            _ => Ok(None),
        },
        Object::Dictionary(dict) => Ok(Some((AcroformHome::Inline { root_ref, catalog }, dict))),
        _ => Ok(None),
    }
}

/// Extract `/SigFlags` as a `u32` bitfield from an already-resolved `/AcroForm`
/// dictionary. Non-integer or out-of-range values read as absent.
fn sig_flags_from_acroform_dict(acroform: &Dictionary) -> Option<u32> {
    acroform
        .get("SigFlags")
        .and_then(Object::as_integer)
        .and_then(|n| u32::try_from(n).ok())
}

/// Mask off the signature bits of `/SigFlags` in place. Returns `true` if the
/// value changed.
fn clear_sig_flags_in_dict(acroform: &mut Dictionary) -> bool {
    let Some(flags) = sig_flags_from_acroform_dict(acroform) else {
        return false;
    };
    let cleared = flags & !(SIG_FLAGS_SIGNATURES_EXIST | SIG_FLAGS_APPEND_ONLY);
    if cleared == flags {
        return false;
    }
    acroform.insert("SigFlags", Object::Integer(i64::from(cleared)));
    true
}

/// Compute whether a rewrite mode preserves existing signed byte ranges.
///
/// qpdf-compatible decision logic:
/// - unsigned inputs are never reported as signature-invalidating;
/// - full rewrite invalidates any existing signature because object offsets and
///   serialized bytes move;
/// - incremental update preserves signatures when it appends unrelated changes;
/// - incremental update invalidates signatures when it rewrites `/AcroForm` or
///   a signature field/signature dictionary.
///
/// # Errors
///
/// - Propagates any error from resolving the catalog, `/AcroForm`, and
///   signature objects (for example I/O or parse failures surfaced by
///   [`Pdf::resolve`]).
/// - [`Error::Unsupported`] when the signature field-tree or known-container
///   recursion depth limit is exceeded while collecting signed objects.
pub fn signature_rewrite_impact<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    mode: SignatureWriteMode,
) -> Result<SignatureRewriteImpact> {
    let touched_object_refs = pdf.dirty_object_refs();
    let touched: BTreeSet<ObjectRef> = touched_object_refs.iter().copied().collect();
    let rewrite_info = collect_signature_rewrite_info(pdf)?;
    let has_signatures = !rewrite_info.signed_object_refs.is_empty();

    let mut signed_object_refs: Vec<ObjectRef> =
        rewrite_info.signed_object_refs.iter().copied().collect();
    signed_object_refs.sort();

    if !has_signatures {
        return Ok(SignatureRewriteImpact {
            has_signatures: false,
            invalidates_signatures: false,
            reason: SignatureRewriteReason::NoSignatures,
            first_invalidating_ref: None,
            signed_object_refs,
            touched_object_refs,
            acroform_ref: rewrite_info.acroform_ref,
            sig_flags: rewrite_info.sig_flags,
        });
    }

    if mode == SignatureWriteMode::FullRewrite {
        return Ok(SignatureRewriteImpact {
            has_signatures: true,
            invalidates_signatures: true,
            reason: SignatureRewriteReason::FullRewrite,
            first_invalidating_ref: None,
            signed_object_refs,
            touched_object_refs,
            acroform_ref: rewrite_info.acroform_ref,
            sig_flags: rewrite_info.sig_flags,
        });
    }

    if let Some(acroform_ref) = rewrite_info.acroform_ref {
        if touched.contains(&acroform_ref) {
            return Ok(SignatureRewriteImpact {
                has_signatures: true,
                invalidates_signatures: true,
                reason: SignatureRewriteReason::IncrementalTouchesAcroForm,
                first_invalidating_ref: Some(acroform_ref),
                signed_object_refs,
                touched_object_refs,
                acroform_ref: Some(acroform_ref),
                sig_flags: rewrite_info.sig_flags,
            });
        }
    }

    if let Some(object_ref) = signed_object_refs
        .iter()
        .copied()
        .find(|object_ref| touched.contains(object_ref))
    {
        return Ok(SignatureRewriteImpact {
            has_signatures: true,
            invalidates_signatures: true,
            reason: SignatureRewriteReason::IncrementalTouchesSignedObject,
            first_invalidating_ref: Some(object_ref),
            signed_object_refs,
            touched_object_refs,
            acroform_ref: rewrite_info.acroform_ref,
            sig_flags: rewrite_info.sig_flags,
        });
    }

    Ok(SignatureRewriteImpact {
        has_signatures: true,
        invalidates_signatures: false,
        reason: SignatureRewriteReason::IncrementalPreservesSignedByteRanges,
        first_invalidating_ref: None,
        signed_object_refs,
        touched_object_refs,
        acroform_ref: rewrite_info.acroform_ref,
        sig_flags: rewrite_info.sig_flags,
    })
}

#[derive(Debug, Default)]
struct SignatureRewriteInfo {
    acroform_ref: Option<ObjectRef>,
    signed_object_refs: BTreeSet<ObjectRef>,
    sig_flags: Option<u32>,
}

fn collect_signature_rewrite_info<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> Result<SignatureRewriteInfo> {
    let mut info = SignatureRewriteInfo::default();

    if let Some(root_ref) = pdf.root_ref() {
        let root = pdf.resolve(root_ref)?;
        if let Object::Dictionary(catalog) = root {
            if let Some(acroform) = catalog.get("AcroForm").cloned() {
                let (acroform_ref, acroform_dict) = resolve_acroform(pdf, root_ref, acroform)?;
                info.acroform_ref = Some(acroform_ref);
                info.sig_flags = sig_flags_from_acroform_dict(&acroform_dict);
                collect_acroform_signatures(pdf, &acroform_dict, &mut info)?;
            }

            for key in ["Perms", "DSS"] {
                if let Some(value) = catalog.get(key).cloned() {
                    collect_known_signature_value(pdf, value, &mut info, 0)?;
                }
            }
        }
    }

    Ok(info)
}

fn resolve_acroform<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    root_ref: ObjectRef,
    acroform: Object,
) -> Result<(ObjectRef, Dictionary)> {
    match acroform {
        Object::Reference(acroform_ref) => {
            let object = pdf.resolve(acroform_ref)?;
            match object {
                Object::Dictionary(dict) => Ok((acroform_ref, dict)),
                _ => Ok((acroform_ref, Dictionary::new())),
            }
        }
        Object::Dictionary(dict) => Ok((root_ref, dict)),
        _ => Ok((root_ref, Dictionary::new())),
    }
}

fn collect_acroform_signatures<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    acroform: &Dictionary,
    info: &mut SignatureRewriteInfo,
) -> Result<()> {
    let Some(fields) = acroform.get("Fields").cloned() else {
        return Ok(());
    };
    let mut seen: BTreeSet<(ObjectRef, bool)> = BTreeSet::new();
    for field in resolve_array(pdf, fields)? {
        if let Object::Reference(field_ref) = field {
            walk_signature_rewrite_field(pdf, field_ref, None, 0, info, &mut seen)?;
        }
    }
    Ok(())
}

fn strip_signature_values_from_field<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_ref: ObjectRef,
    inherited_type: Option<Vec<u8>>,
    depth: usize,
    seen: &mut BTreeSet<ObjectRef>,
    changed: &mut bool,
) -> Result<()> {
    if depth > DEFAULT_MAX_SIGNATURE_FIELD_DEPTH || !seen.insert(field_ref) {
        return Ok(());
    }

    let Object::Dictionary(mut dict) = pdf.resolve(field_ref)? else {
        return Ok(());
    };

    let field_type = inherited_name(pdf, &dict, "FT")?.or(inherited_type);
    let kids_obj = dict.get("Kids").cloned();

    let signature_value_ref = dict.get("V").and_then(Object::as_ref_id);

    if field_type.as_deref() == Some(b"Sig") && dict.remove("V").is_some() {
        pdf.set_object(field_ref, Object::Dictionary(dict));
        if let Some(signature_ref) = signature_value_ref {
            pdf.delete_object(signature_ref);
        }
        *changed = true;
        if depth == DEFAULT_MAX_SIGNATURE_FIELD_DEPTH {
            return Ok(());
        }

        let Some(kids_obj) = kids_obj else {
            return Ok(());
        };
        return strip_signature_values_from_kids(pdf, kids_obj, field_type, depth, seen, changed);
    }

    if depth == DEFAULT_MAX_SIGNATURE_FIELD_DEPTH {
        return Ok(());
    }

    let Some(kids_obj) = kids_obj else {
        return Ok(());
    };
    strip_signature_values_from_kids(pdf, kids_obj, field_type, depth, seen, changed)
}

fn strip_signature_values_from_kids<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    kids_obj: Object,
    field_type: Option<Vec<u8>>,
    depth: usize,
    seen: &mut BTreeSet<ObjectRef>,
    changed: &mut bool,
) -> Result<()> {
    for kid in resolve_array(pdf, kids_obj)? {
        let Object::Reference(kid_ref) = kid else {
            continue;
        };
        let kid_obj = pdf.resolve_borrowed(kid_ref)?;
        let Object::Dictionary(kid_dict) = kid_obj else {
            continue;
        };
        if is_pure_widget(kid_dict) {
            continue;
        }
        strip_signature_values_from_field(
            pdf,
            kid_ref,
            field_type.clone(),
            depth + 1,
            seen,
            changed,
        )?;
    }

    Ok(())
}

fn walk_signature_rewrite_field<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_ref: ObjectRef,
    inherited_ft: Option<Vec<u8>>,
    depth: usize,
    info: &mut SignatureRewriteInfo,
    seen: &mut BTreeSet<(ObjectRef, bool)>,
) -> Result<()> {
    // Key on (ref, inherited_is_sig) so a node shared between a /Sig parent
    // and a non-/Sig parent is visited once per distinct inheritance context.
    // Each ref appears at most twice → traversal stays linear.
    let inherited_is_sig = inherited_ft.as_deref() == Some(b"Sig");
    if !seen.insert((field_ref, inherited_is_sig)) {
        return Ok(());
    }
    if depth > DEFAULT_MAX_SIGNATURE_FIELD_DEPTH {
        return Err(crate::Error::Unsupported(format!(
            "signature field-tree depth limit {DEFAULT_MAX_SIGNATURE_FIELD_DEPTH} exceeded at {field_ref}"
        )));
    }

    let Object::Dictionary(dict) = pdf.resolve(field_ref)? else {
        return Ok(());
    };

    // Resolve /FT through inherited_name so an indirect-reference /FT is still
    // recognised as a signature field (matches walk_signature_field /
    // strip_signature_values_from_field). inherited_ft remains the top-down
    // fallback supplied by the parent during the Kids descent.
    let field_type = inherited_name(pdf, &dict, "FT")?.or(inherited_ft);

    if field_type.as_deref() == Some(b"Sig") {
        info.signed_object_refs.insert(field_ref);
        if let Some(value) = dict.get("V").cloned() {
            collect_signature_value(pdf, value, info)?;
        }
    }

    if dict.get("ByteRange").is_some() {
        info.signed_object_refs.insert(field_ref);
    }

    if let Some(kids) = dict.get("Kids").cloned() {
        for kid in resolve_array(pdf, kids)? {
            if let Object::Reference(kid_ref) = kid {
                walk_signature_rewrite_field(
                    pdf,
                    kid_ref,
                    field_type.clone(),
                    depth + 1,
                    info,
                    seen,
                )?;
            }
        }
    }

    Ok(())
}

fn collect_known_signature_value<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: Object,
    info: &mut SignatureRewriteInfo,
    depth: usize,
) -> Result<()> {
    if depth > DEFAULT_MAX_SIGNATURE_FIELD_DEPTH {
        return Err(crate::Error::Unsupported(format!(
            "signature known-container depth limit {DEFAULT_MAX_SIGNATURE_FIELD_DEPTH} exceeded"
        )));
    }

    match value {
        Object::Reference(object_ref) => {
            let object = pdf.resolve(object_ref)?;
            if object_has_byte_range(&object) {
                info.signed_object_refs.insert(object_ref);
            }
            collect_known_signature_value(pdf, object, info, depth + 1)?;
        }
        Object::Dictionary(dict) => {
            for (_, value) in dict.iter() {
                collect_known_signature_value(pdf, value.clone(), info, depth + 1)?;
            }
        }
        Object::Array(items) => {
            for item in items {
                collect_known_signature_value(pdf, item, info, depth + 1)?;
            }
        }
        Object::Stream(stream) if stream.dict.get("ByteRange").is_some() => {
            for (_, value) in stream.dict.iter() {
                collect_known_signature_value(pdf, value.clone(), info, depth + 1)?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn collect_signature_value<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: Object,
    info: &mut SignatureRewriteInfo,
) -> Result<()> {
    match value {
        Object::Reference(sig_ref) => {
            let object = pdf.resolve(sig_ref)?;
            if object_has_byte_range(&object) {
                info.signed_object_refs.insert(sig_ref);
            }
        }
        object if object_has_byte_range(&object) => {}
        _ => {}
    }

    Ok(())
}

fn walk_signature_field<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_ref: ObjectRef,
    parent_name: &str,
    output: &mut Vec<SignatureInfo>,
    seen: &mut BTreeSet<ObjectRef>,
    depth: usize,
    max_depth: usize,
) -> Result<()> {
    if depth > max_depth || !seen.insert(field_ref) {
        return Ok(());
    }

    let field_obj = pdf.resolve_borrowed(field_ref)?;
    let Object::Dictionary(field_dict) = field_obj else {
        return Ok(());
    };
    let field_dict = field_dict.clone();

    let field_name = join_field_name(parent_name, text_entry(pdf, &field_dict, "T")?);
    if inherited_name(pdf, &field_dict, "FT")?.as_deref() == Some(b"Sig") {
        if let Some(info) = signature_info_for_field(pdf, field_ref, &field_name, &field_dict)? {
            output.push(info);
        }
    }

    if depth == max_depth {
        return Ok(());
    }

    let Some(kids_obj) = field_dict.get("Kids").cloned() else {
        return Ok(());
    };
    for kid in resolve_array(pdf, kids_obj)? {
        let Object::Reference(kid_ref) = kid else {
            continue;
        };
        let kid_obj = pdf.resolve_borrowed(kid_ref)?;
        let Object::Dictionary(kid_dict) = kid_obj else {
            continue;
        };
        if is_pure_widget(kid_dict) {
            continue;
        }
        walk_signature_field(
            pdf,
            kid_ref,
            &field_name,
            output,
            seen,
            depth + 1,
            max_depth,
        )?;
    }

    Ok(())
}

fn signature_info_for_field<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_ref: ObjectRef,
    field_name: &str,
    field_dict: &Dictionary,
) -> Result<Option<SignatureInfo>> {
    let Some(value) = inherited_field_value(pdf, field_dict, "V")? else {
        return Ok(None);
    };
    let signature_ref = value.as_ref_id();
    let Some(signature_dict) = resolve_dictionary(pdf, value)? else {
        return Ok(None);
    };
    let Some(byte_range_obj) = signature_dict.get("ByteRange").cloned() else {
        return Ok(None);
    };
    let byte_range = parse_byte_range(pdf, byte_range_obj)?;

    Ok(Some(SignatureInfo {
        field_ref,
        signature_ref,
        field_name: field_name.to_string(),
        byte_range,
        sub_filter: name_entry(pdf, &signature_dict, "SubFilter")?,
        signer_name: text_entry(pdf, &signature_dict, "Name")?,
        signing_time: text_entry(pdf, &signature_dict, "M")?,
        reason: text_entry(pdf, &signature_dict, "Reason")?,
        location: text_entry(pdf, &signature_dict, "Location")?,
        contact_info: text_entry(pdf, &signature_dict, "ContactInfo")?,
        certificate: certificate_entry(pdf, &signature_dict)?,
    }))
}

fn resolve_dictionary<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: Object,
) -> Result<Option<Dictionary>> {
    match value {
        Object::Dictionary(dict) => Ok(Some(dict)),
        Object::Reference(object_ref) => match pdf.resolve_borrowed(object_ref)? {
            Object::Dictionary(dict) => Ok(Some(dict.clone())),
            _ => Ok(None),
        },
        _ => Ok(None),
    }
}

fn resolve_array<R: Read + Seek>(pdf: &mut Pdf<R>, value: Object) -> Result<Vec<Object>> {
    match value {
        Object::Array(values) => Ok(values),
        Object::Reference(object_ref) => match pdf.resolve(object_ref)? {
            Object::Array(values) => Ok(values),
            _ => Ok(Vec::new()),
        },
        _ => Ok(Vec::new()),
    }
}

fn parse_byte_range<R: Read + Seek>(pdf: &mut Pdf<R>, value: Object) -> Result<[u64; 4]> {
    let values = match value {
        Object::Array(values) => values,
        Object::Reference(object_ref) => match pdf.resolve_borrowed(object_ref)? {
            Object::Array(values) => values.clone(),
            _ => return Err(invalid_byte_range("must be an array")),
        },
        _ => return Err(invalid_byte_range("must be an array")),
    };
    if values.len() != 4 {
        return Err(invalid_byte_range("must contain exactly four integers"));
    }

    let mut out = [0; 4];
    for (idx, value) in values.iter().enumerate() {
        let Some(n) = value.as_integer() else {
            return Err(invalid_byte_range("must contain only integers"));
        };
        out[idx] = u64::try_from(n)
            .map_err(|_| invalid_byte_range("must contain non-negative integers"))?;
    }
    Ok(out)
}

fn invalid_byte_range(message: &'static str) -> Error {
    Error::parse(0, format!("invalid signature /ByteRange: {message}"))
}

fn inherited_name<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_dict: &Dictionary,
    key: &str,
) -> Result<Option<Vec<u8>>> {
    match inherited_field_value(pdf, field_dict, key)? {
        Some(Object::Name(name)) => Ok(Some(name)),
        Some(Object::Reference(object_ref)) => match pdf.resolve_borrowed(object_ref)? {
            Object::Name(name) => Ok(Some(name.clone())),
            _ => Ok(None),
        },
        _ => Ok(None),
    }
}

fn inherited_field_value<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_dict: &Dictionary,
    key: &str,
) -> Result<Option<Object>> {
    if let Some(local) = field_dict.get(key).cloned() {
        return Ok(Some(local));
    }

    let mut parent = field_dict.get("Parent").cloned();
    let mut seen = BTreeSet::new();
    let mut depth: usize = 0;
    while let Some(Object::Reference(parent_ref)) = parent {
        if depth > DEFAULT_MAX_SIGNATURE_FIELD_DEPTH {
            return Err(Error::Unsupported(format!(
                "signature field-tree depth limit {DEFAULT_MAX_SIGNATURE_FIELD_DEPTH} exceeded at {parent_ref}"
            )));
        }
        if !seen.insert(parent_ref) {
            break;
        }
        match pdf.resolve_borrowed(parent_ref)? {
            Object::Dictionary(parent_dict) => {
                if let Some(value) = parent_dict.get(key).cloned() {
                    return Ok(Some(value));
                }
                parent = parent_dict.get("Parent").cloned();
            }
            _ => break,
        }
        depth += 1;
    }
    Ok(None)
}

fn join_field_name(parent_name: &str, local_name: Option<String>) -> String {
    let local_name = local_name.unwrap_or_default();
    if parent_name.is_empty() {
        local_name
    } else if local_name.is_empty() {
        parent_name.to_string()
    } else {
        format!("{parent_name}.{local_name}")
    }
}

fn is_pure_widget(dict: &Dictionary) -> bool {
    let is_widget = matches!(
        dict.get("Subtype"),
        Some(Object::Name(name)) if name.as_slice() == b"Widget"
    );
    let has_field_entries = dict.get("T").is_some()
        || dict.get("FT").is_some()
        || dict.get("Kids").is_some()
        || dict.get("V").is_some()
        || dict.get("DV").is_some()
        || dict.get("Ff").is_some()
        || dict.get("TU").is_some()
        || dict.get("TM").is_some();

    is_widget && !has_field_entries
}

fn resolve_entry<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &Dictionary,
    key: &str,
) -> Result<Option<Object>> {
    match dict.get(key) {
        Some(Object::Reference(object_ref)) => Ok(Some(pdf.resolve(*object_ref)?)),
        Some(object) => Ok(Some(object.clone())),
        None => Ok(None),
    }
}

fn name_entry<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &Dictionary,
    key: &str,
) -> Result<Option<String>> {
    match resolve_entry(pdf, dict, key)? {
        Some(Object::Name(name)) => Ok(Some(String::from_utf8_lossy(&name).into_owned())),
        _ => Ok(None),
    }
}

fn text_entry<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &Dictionary,
    key: &str,
) -> Result<Option<String>> {
    match resolve_entry(pdf, dict, key)? {
        Some(Object::String(bytes)) => Ok(Some(
            decode_pdf_text_string(&bytes)
                .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into()),
        )),
        _ => Ok(None),
    }
}

fn certificate_entry<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &Dictionary,
) -> Result<Option<Vec<u8>>> {
    match resolve_entry(pdf, dict, "Cert")? {
        Some(Object::String(bytes)) => Ok(Some(bytes)),
        Some(Object::Array(values)) => {
            for value in values {
                match value {
                    Object::String(bytes) => return Ok(Some(bytes)),
                    Object::Reference(object_ref) => {
                        if let Object::String(bytes) = pdf.resolve(object_ref)? {
                            return Ok(Some(bytes));
                        }
                    }
                    _ => {}
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn object_has_byte_range(object: &Object) -> bool {
    match object {
        Object::Dictionary(dict) => dict.get("ByteRange").is_some(),
        Object::Stream(stream) => stream.dict.get("ByteRange").is_some(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // Minimal valid PDF; nodes are supplied via set_object refs (catalog unused).
    fn empty_pdf() -> Pdf<Cursor<Vec<u8>>> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.4\n");
        let off1 = bytes.len() as u64;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Pdf::open(Cursor::new(bytes)).expect("open")
    }

    // Register a /Parent chain obj(start)->obj(start+1)->...->obj(start+len-1).
    // The deepest node carries `key`; the starting dict (returned) only has /Parent.
    fn parent_chain(pdf: &mut Pdf<Cursor<Vec<u8>>>, start: u32, len: u32, key: &str) -> Dictionary {
        for i in 0..len {
            let num = start + i;
            let mut d = Dictionary::new();
            if i + 1 < len {
                d.insert("Parent", Object::Reference(ObjectRef::new(num + 1, 0)));
            } else {
                // deepest node holds the inheritable value
                d.insert(key, Object::Integer(42));
            }
            pdf.set_object(ObjectRef::new(num, 0), Object::Dictionary(d));
        }
        let mut start_dict = Dictionary::new();
        start_dict.insert("Parent", Object::Reference(ObjectRef::new(start, 0)));
        start_dict
    }

    #[test]
    fn inherited_field_value_errors_on_excessive_parent_depth() {
        let mut pdf = empty_pdf();
        // Chain longer than the limit so the guard trips before reaching the leaf.
        let start_dict = parent_chain(
            &mut pdf,
            2,
            (DEFAULT_MAX_SIGNATURE_FIELD_DEPTH as u32) + 5,
            "V",
        );
        let err = inherited_field_value(&mut pdf, &start_dict, "V");
        assert!(matches!(err, Err(Error::Unsupported(_))));
    }

    #[test]
    fn inherited_field_value_resolves_within_limit() {
        let mut pdf = empty_pdf();
        // Short chain: the inherited value must be found, not errored.
        let start_dict = parent_chain(&mut pdf, 2, 4, "V");
        let got = inherited_field_value(&mut pdf, &start_dict, "V").unwrap();
        assert_eq!(got, Some(Object::Integer(42)));
    }

    // Build a dictionary from (key, Object) pairs for the traverse-based
    // enumeration unit tests below.
    fn dict_value(pairs: Vec<(&str, Object)>) -> Dictionary {
        let mut d = Dictionary::new();
        for (k, v) in pairs {
            d.insert(k, v);
        }
        d
    }

    fn dict(pairs: Vec<(&str, Object)>) -> Object {
        Object::Dictionary(dict_value(pairs))
    }

    fn refs_vec(pairs: &[u32]) -> Vec<Object> {
        pairs
            .iter()
            .map(|n| Object::Reference(ObjectRef::new(*n, 0)))
            .collect()
    }

    fn refs(pairs: &[u32]) -> Object {
        Object::Array(refs_vec(pairs))
    }

    #[test]
    fn traverse_field_visited_guard_breaks_kids_cycle() {
        // 5 -> /Kids [6] -> /Kids [5]: the loop guard must stop re-descending 5.
        let mut pdf = empty_pdf();
        pdf.set_object(ObjectRef::new(5, 0), dict(vec![("Kids", refs(&[6]))]));
        pdf.set_object(ObjectRef::new(6, 0), dict(vec![("Kids", refs(&[5]))]));

        let mut visited = BTreeSet::new();
        let mut keys = BTreeSet::new();
        let mut annots = BTreeSet::new();
        traverse_field(
            &mut pdf,
            ObjectRef::new(5, 0),
            None,
            0,
            &mut visited,
            &mut keys,
            &mut annots,
        )
        .unwrap();

        assert!(visited.contains(&ObjectRef::new(5, 0)));
        assert!(visited.contains(&ObjectRef::new(6, 0)));
        // Neither node is an annotation, so no form-field key is recorded.
        assert!(keys.is_empty());
    }

    #[test]
    fn traverse_field_ignores_non_dictionary_node() {
        // A non-dictionary field/annotation is skipped without visiting it.
        let mut pdf = empty_pdf();
        pdf.set_object(ObjectRef::new(5, 0), Object::Integer(7));

        let mut visited = BTreeSet::new();
        let mut keys = BTreeSet::new();
        let mut annots = BTreeSet::new();
        traverse_field(
            &mut pdf,
            ObjectRef::new(5, 0),
            None,
            0,
            &mut visited,
            &mut keys,
            &mut annots,
        )
        .unwrap();

        assert!(keys.is_empty());
        assert!(!visited.contains(&ObjectRef::new(5, 0)));
    }

    #[test]
    fn traverse_field_errs_past_depth_limit() {
        let mut pdf = empty_pdf();
        pdf.set_object(ObjectRef::new(5, 0), dict(vec![]));

        let mut visited = BTreeSet::new();
        let mut keys = BTreeSet::new();
        let mut annots = BTreeSet::new();
        let err = traverse_field(
            &mut pdf,
            ObjectRef::new(5, 0),
            None,
            DEFAULT_MAX_ACROFORM_DEPTH + 1,
            &mut visited,
            &mut keys,
            &mut annots,
        );
        assert!(matches!(err, Err(Error::Unsupported(_))));
    }

    #[test]
    fn resolve_kids_array_resolves_indirect_and_rejects_non_array() {
        let mut pdf = empty_pdf();
        pdf.set_object(ObjectRef::new(9, 0), refs(&[6, 7]));
        pdf.set_object(ObjectRef::new(10, 0), Object::Integer(1));

        // Direct array.
        let direct = dict_value(vec![("Kids", refs(&[6]))]);
        assert_eq!(
            resolve_kids_array(&mut pdf, &direct).unwrap(),
            Some(refs_vec(&[6]))
        );

        // Indirect reference to an array.
        let indirect = dict_value(vec![("Kids", Object::Reference(ObjectRef::new(9, 0)))]);
        assert_eq!(
            resolve_kids_array(&mut pdf, &indirect).unwrap(),
            Some(refs_vec(&[6, 7]))
        );

        // Indirect reference to a non-array resolves to None.
        let non_array = dict_value(vec![("Kids", Object::Reference(ObjectRef::new(10, 0)))]);
        assert_eq!(resolve_kids_array(&mut pdf, &non_array).unwrap(), None);

        // Absent /Kids, and a direct non-array /Kids, both yield None.
        assert_eq!(
            resolve_kids_array(&mut pdf, &dict_value(vec![])).unwrap(),
            None
        );
        let name_kids = dict_value(vec![("Kids", Object::Name(b"x".to_vec()))]);
        assert_eq!(resolve_kids_array(&mut pdf, &name_kids).unwrap(), None);
    }

    #[test]
    fn page_widget_annotation_refs_filters_widgets_and_edge_entries() {
        let mut pdf = empty_pdf();
        // page /Annots mixes: a widget, a non-widget, a non-dict, and a direct
        // (non-reference) entry — only the widget ref is returned.
        pdf.set_object(
            ObjectRef::new(3, 0),
            dict(vec![(
                "Annots",
                Object::Array(vec![
                    Object::Reference(ObjectRef::new(5, 0)),
                    Object::Reference(ObjectRef::new(6, 0)),
                    Object::Reference(ObjectRef::new(7, 0)),
                    Object::Integer(99),
                ]),
            )]),
        );
        pdf.set_object(
            ObjectRef::new(5, 0),
            dict(vec![("Subtype", Object::Name(b"Widget".to_vec()))]),
        );
        pdf.set_object(
            ObjectRef::new(6, 0),
            dict(vec![("Subtype", Object::Name(b"Link".to_vec()))]),
        );
        pdf.set_object(ObjectRef::new(7, 0), Object::Integer(0));

        let widgets = page_widget_annotation_refs(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert_eq!(widgets, vec![ObjectRef::new(5, 0)]);
    }

    #[test]
    fn page_widget_annotation_refs_handles_non_dict_page_and_missing_annots() {
        let mut pdf = empty_pdf();
        // A non-dictionary page yields no widgets.
        pdf.set_object(ObjectRef::new(3, 0), Object::Integer(1));
        assert!(page_widget_annotation_refs(&mut pdf, ObjectRef::new(3, 0))
            .unwrap()
            .is_empty());

        // A page dictionary without /Annots yields no widgets.
        pdf.set_object(
            ObjectRef::new(3, 0),
            dict(vec![("Type", Object::Name(b"Page".to_vec()))]),
        );
        assert!(page_widget_annotation_refs(&mut pdf, ObjectRef::new(3, 0))
            .unwrap()
            .is_empty());
    }
}

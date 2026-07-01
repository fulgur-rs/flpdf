//! Digital signature helpers.
//!
//! This module has three layers:
//! - read-only AcroForm signature field inspection via [`signatures`];
//! - rewrite-impact checks for whether a writer mode would invalidate existing
//!   signed `/ByteRange`s;
//! - `/AcroForm /SigFlags` primitives ([`acroform_sig_flags`], [`clear_sig_flags`])
//!   that read, surface, and clear the SignaturesExist/AppendOnly bits.

use crate::json_inspect::decode_pdf_text_string;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result, WriteOptions};
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
/// 2. For every terminal AcroForm field whose inherited `/FT` is `/Sig`, removes
///    `/FT`, `/V`, `/SV`, and `/Lock` (the field name `/T` is preserved) and
///    deletes the now-orphaned signature dictionary referenced by `/V`.
/// 3. Erases those fields' references from the top-level `/AcroForm /Fields`
///    array. On a full rewrite a field still reachable from a page `/Annots`
///    survives as a plain annotation; a field-only entry becomes unreferenced
///    and is dropped by garbage collection.
///
/// Returns `true` when anything changed. `/DSS` is intentionally left untouched,
/// matching qpdf (`removeSecurityRestrictions` removes only `/Perms`).
///
/// # Errors
///
/// Propagates any error from resolving the catalog, `/AcroForm`, `/Fields`, and
/// field-tree objects (surfaced by [`Pdf::resolve`]), and the depth-limit error
/// path shared with the other signature walkers.
pub fn disable_digital_signatures<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<bool> {
    let mut changed = remove_security_restrictions(pdf)?;

    let Some((home, mut acroform)) = resolve_catalog_acroform(pdf)? else {
        return Ok(changed);
    };
    let Some(fields_obj) = acroform.get("Fields").cloned() else {
        return Ok(changed);
    };
    let fields = resolve_array(pdf, fields_obj)?;

    let mut to_remove: Vec<ObjectRef> = Vec::new();
    let mut seen = BTreeSet::new();
    for field in &fields {
        if let Object::Reference(field_ref) = field {
            disable_sig_field(
                pdf,
                *field_ref,
                None,
                0,
                &mut seen,
                &mut to_remove,
                &mut changed,
            )?;
        }
    }

    if !to_remove.is_empty() {
        let new_fields: Vec<Object> = fields
            .into_iter()
            .filter(|f| !matches!(f, Object::Reference(r) if to_remove.contains(r)))
            .collect();
        acroform.insert("Fields", Object::Array(new_fields));
        write_back_acroform(pdf, home, acroform);
        changed = true;
    }
    Ok(changed)
}

/// Strip signature keys from a single AcroForm field and, for `/Sig` terminals,
/// record the field ref in `to_remove` so it can be erased from the top-level
/// `/Fields` array. Mirrors [`strip_signature_values_from_field`] but removes
/// `/FT`, `/V`, `/SV`, and `/Lock` from any `/Sig` field (not only fields that
/// carry a `/V`) and collects the ref.
fn disable_sig_field<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_ref: ObjectRef,
    inherited_type: Option<Vec<u8>>,
    depth: usize,
    seen: &mut BTreeSet<ObjectRef>,
    to_remove: &mut Vec<ObjectRef>,
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

    if field_type.as_deref() == Some(b"Sig") {
        let signature_value_ref = dict.get("V").and_then(Object::as_ref_id);
        dict.remove("FT");
        dict.remove("V");
        dict.remove("SV");
        dict.remove("Lock");
        pdf.set_object(field_ref, Object::Dictionary(dict));
        if let Some(signature_ref) = signature_value_ref {
            pdf.delete_object(signature_ref);
        }
        to_remove.push(field_ref);
        *changed = true;
        if depth == DEFAULT_MAX_SIGNATURE_FIELD_DEPTH {
            return Ok(());
        }

        let Some(kids_obj) = kids_obj else {
            return Ok(());
        };
        return disable_sig_field_kids(pdf, kids_obj, field_type, depth, seen, to_remove, changed);
    }

    if depth == DEFAULT_MAX_SIGNATURE_FIELD_DEPTH {
        return Ok(());
    }

    let Some(kids_obj) = kids_obj else {
        return Ok(());
    };
    disable_sig_field_kids(pdf, kids_obj, field_type, depth, seen, to_remove, changed)
}

/// Descend into a field's `/Kids`, disabling signature keys on each child field.
/// Mirrors [`strip_signature_values_from_kids`] but threads `to_remove` through
/// [`disable_sig_field`]; pure widget kids are skipped exactly as there.
fn disable_sig_field_kids<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    kids_obj: Object,
    field_type: Option<Vec<u8>>,
    depth: usize,
    seen: &mut BTreeSet<ObjectRef>,
    to_remove: &mut Vec<ObjectRef>,
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
        disable_sig_field(
            pdf,
            kid_ref,
            field_type.clone(),
            depth + 1,
            seen,
            to_remove,
            changed,
        )?;
    }

    Ok(())
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

    // Build a dictionary from (key, Object) pairs for the disable_sig_field
    // walker unit tests below.
    fn dict(pairs: Vec<(&str, Object)>) -> Object {
        let mut d = Dictionary::new();
        for (k, v) in pairs {
            d.insert(k, v);
        }
        Object::Dictionary(d)
    }

    fn sig_name() -> Object {
        Object::Name(b"Sig".to_vec())
    }

    #[test]
    fn disable_sig_field_skips_already_seen_ref() {
        let mut pdf = empty_pdf();
        let field_ref = ObjectRef::new(2, 0);
        pdf.set_object(field_ref, dict(vec![("FT", sig_name())]));

        let mut seen = BTreeSet::new();
        seen.insert(field_ref); // already visited: the walker must bail out
        let mut to_remove = Vec::new();
        let mut changed = false;
        disable_sig_field(
            &mut pdf,
            field_ref,
            None,
            0,
            &mut seen,
            &mut to_remove,
            &mut changed,
        )
        .unwrap();

        assert!(!changed);
        assert!(to_remove.is_empty());
        assert!(
            matches!(pdf.resolve(field_ref).unwrap(), Object::Dictionary(d) if d.get("FT").is_some()),
            "seen ref must be left untouched"
        );
    }

    #[test]
    fn disable_sig_field_ignores_non_dictionary() {
        let mut pdf = empty_pdf();
        let field_ref = ObjectRef::new(2, 0);
        pdf.set_object(field_ref, Object::Integer(7));

        let mut seen = BTreeSet::new();
        let mut to_remove = Vec::new();
        let mut changed = false;
        disable_sig_field(
            &mut pdf,
            field_ref,
            None,
            0,
            &mut seen,
            &mut to_remove,
            &mut changed,
        )
        .unwrap();

        assert!(!changed);
        assert!(to_remove.is_empty());
    }

    #[test]
    fn disable_sig_field_sig_stops_recursion_at_depth_limit() {
        let mut pdf = empty_pdf();
        let parent = ObjectRef::new(2, 0);
        let child = ObjectRef::new(3, 0);
        let sig = ObjectRef::new(5, 0);
        pdf.set_object(
            parent,
            dict(vec![
                ("FT", sig_name()),
                ("V", Object::Reference(sig)),
                ("Kids", Object::Array(vec![Object::Reference(child)])),
            ]),
        );
        pdf.set_object(child, dict(vec![("FT", sig_name())]));
        pdf.set_object(sig, dict(vec![("Type", sig_name())]));

        let mut seen = BTreeSet::new();
        let mut to_remove = Vec::new();
        let mut changed = false;
        disable_sig_field(
            &mut pdf,
            parent,
            None,
            DEFAULT_MAX_SIGNATURE_FIELD_DEPTH,
            &mut seen,
            &mut to_remove,
            &mut changed,
        )
        .unwrap();

        assert!(changed);
        assert_eq!(to_remove, vec![parent]);
        assert!(matches!(
            pdf.resolve(parent).unwrap(),
            Object::Dictionary(p) if p.get("FT").is_none() && p.get("V").is_none()
        ));
        assert_eq!(pdf.resolve(sig).unwrap(), Object::Null);
        // At the depth limit the /Kids are not descended, so the child keeps /FT.
        assert!(
            matches!(pdf.resolve(child).unwrap(), Object::Dictionary(c) if c.get("FT").is_some()),
            "child must not be walked at depth limit"
        );
    }

    #[test]
    fn disable_sig_field_non_sig_stops_recursion_at_depth_limit() {
        let mut pdf = empty_pdf();
        let parent = ObjectRef::new(2, 0);
        let child = ObjectRef::new(3, 0);
        pdf.set_object(
            parent,
            dict(vec![
                ("FT", Object::Name(b"Tx".to_vec())),
                ("Kids", Object::Array(vec![Object::Reference(child)])),
            ]),
        );
        pdf.set_object(
            child,
            dict(vec![("FT", sig_name()), ("V", Object::Integer(1))]),
        );

        let mut seen = BTreeSet::new();
        let mut to_remove = Vec::new();
        let mut changed = false;
        disable_sig_field(
            &mut pdf,
            parent,
            None,
            DEFAULT_MAX_SIGNATURE_FIELD_DEPTH,
            &mut seen,
            &mut to_remove,
            &mut changed,
        )
        .unwrap();

        assert!(
            !changed,
            "non-/Sig field at the depth limit changes nothing"
        );
        assert!(to_remove.is_empty());
        assert!(matches!(
            pdf.resolve(child).unwrap(),
            Object::Dictionary(c) if c.get("FT").is_some() && c.get("V").is_some()
        ));
    }

    #[test]
    fn disable_sig_field_non_sig_descends_into_kids() {
        let mut pdf = empty_pdf();
        let parent = ObjectRef::new(2, 0);
        let child = ObjectRef::new(3, 0);
        let sig = ObjectRef::new(5, 0);
        // Non-/Sig parent grouping a /Sig child: the non-/Sig arm must still
        // recurse into /Kids and strip the child.
        pdf.set_object(
            parent,
            dict(vec![
                ("FT", Object::Name(b"Tx".to_vec())),
                ("Kids", Object::Array(vec![Object::Reference(child)])),
            ]),
        );
        pdf.set_object(
            child,
            dict(vec![("FT", sig_name()), ("V", Object::Reference(sig))]),
        );
        pdf.set_object(sig, dict(vec![("Type", sig_name())]));

        let mut seen = BTreeSet::new();
        let mut to_remove = Vec::new();
        let mut changed = false;
        disable_sig_field(
            &mut pdf,
            parent,
            None,
            0,
            &mut seen,
            &mut to_remove,
            &mut changed,
        )
        .unwrap();

        assert!(changed);
        assert_eq!(to_remove, vec![child], "only the /Sig child is recorded");
        assert!(matches!(
            pdf.resolve(child).unwrap(),
            Object::Dictionary(c) if c.get("FT").is_none() && c.get("V").is_none()
        ));
        assert_eq!(pdf.resolve(sig).unwrap(), Object::Null);
        // The non-/Sig parent itself is preserved.
        assert!(matches!(
            pdf.resolve(parent).unwrap(),
            Object::Dictionary(p) if p.get("FT") == Some(&Object::Name(b"Tx".to_vec()))
        ));
    }
}

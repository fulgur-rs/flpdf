//! qpdf correspondence: QPDFAcroFormDocumentHelper.cc signature disabling and QPDF.cc restriction removal plus flpdf-only inspection.
//! Digital signature helpers.
//!
//! This module has two layers:
//! - read-only AcroForm signature field inspection via [`signatures`];
//! - `/AcroForm /SigFlags` primitives ([`acroform_sig_flags`], [`clear_sig_flags`])
//!   that read, surface, and clear the SignaturesExist/AppendOnly bits.

use crate::form_field_object_helper::FormFieldObjectHelper;
use crate::json_inspect::decode_pdf_text_string;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
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
/// page, and field-tree objects (surfaced by [`Pdf::resolve`]). The shared
/// qpdf `analyze()` traversal ignores field-tree nodes beyond its fixed depth
/// guard, so reaching that guard is not an error here.
pub fn disable_digital_signatures<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<bool> {
    let mut changed = remove_security_restrictions(pdf)?;

    let form_fields = collect_signature_form_field_refs(pdf)?;

    let mut to_remove: Vec<ObjectRef> = Vec::new();
    for field_ref in form_fields {
        let field_type = FormFieldObjectHelper::new(field_ref, pdf).field_type()?;
        if field_type.as_deref() != Some(b"/Sig") {
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
/// Delegates to [`crate::AcroFormDocumentHelper::annotation_to_field_map`]
/// (the shared `analyze()` port, `libqpdf/QPDFAcroFormDocumentHelper.cc:
/// 235-286`) and takes the distinct field values — `field_to_annotations`'s
/// key set is exactly `annotation_to_field`'s value set.
fn collect_signature_form_field_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> Result<BTreeSet<ObjectRef>> {
    Ok(crate::AcroFormDocumentHelper::new(pdf)
        .annotation_to_field_map()?
        .into_values()
        .collect())
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

    let field_type = FormFieldObjectHelper::new(field_ref, pdf)
        .field_type()?
        .map(|name| name.strip_prefix(b"/").unwrap_or(&name).to_vec())
        .or(inherited_type);
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

    let (partial_name, is_signature) = {
        let mut field = FormFieldObjectHelper::new(field_ref, pdf);
        let partial_name = field.partial_name()?;
        let partial_name = (!partial_name.is_empty()).then_some(partial_name);
        let is_signature = field.field_type()?.as_deref() == Some(b"/Sig");
        (partial_name, is_signature)
    };
    let field_name = join_field_name(parent_name, partial_name);
    if is_signature {
        if let Some(info) = signature_info_for_field(pdf, field_ref, &field_name)? {
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
) -> Result<Option<SignatureInfo>> {
    let Some(value) = FormFieldObjectHelper::new(field_ref, pdf).field_value_handle()? else {
        return Ok(None);
    };
    let signature_ref = value.object_ref().or_else(|| value.as_reference());
    let value = pdf.resolve_object_handle_to_terminal(&value)?;
    let value = value.materialize()?; // cov:ignore: field-value helper classifies only signature dictionaries
                                      // cov:ignore-start: pre-existing non-dictionary fallback, unchanged by Result propagation
    let Object::Dictionary(signature_dict) = value else {
        return Ok(None);
    };
    // cov:ignore-end
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

// `traverse_field`/`resolve_kids_array`/`page_widget_annotation_refs` and
// their unit tests moved to `acroform_document_helper.rs` alongside the
// shared `analyze()` port those functions became
// (`AcroFormDocumentHelper::annotation_to_field_map`/`get_field_for_annotation`).

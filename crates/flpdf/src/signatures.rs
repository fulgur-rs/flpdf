//! qpdf correspondence: no qpdf counterpart for signature inspection; qpdf-owned mutation lives in Pdf and AcroFormDocumentHelper.
//!
//! qpdf-owned mutation lives at its corresponding document boundaries:
//! [`crate::Pdf::remove_security_restrictions`] and
//! [`crate::AcroFormDocumentHelper::disable_digital_signatures`].
//!
//! This module has two layers:
//! - read-only AcroForm signature field inspection via [`signatures`];
//! - `/AcroForm /SigFlags` primitives ([`acroform_sig_flags`], [`clear_sig_flags`])
//!   that read, surface, and clear the SignaturesExist/AppendOnly bits.

use crate::form_field_object_helper::FormFieldObjectHelper;
use crate::json_inspect::decode_pdf_text_string;
use crate::object_handle::ObjectHandle;
use crate::{Error, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
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
    let catalog_handle = pdf.get_object_handle(catalog_ref);
    let catalog = resolve_handle(pdf, &catalog_handle)?;
    if catalog.as_dictionary().is_none() {
        return Ok(Vec::new());
    }
    let acroform_value = catalog.try_get_key(b"/AcroForm")?;
    let acroform = resolve_handle(pdf, &acroform_value)?;
    if acroform_value.is_null() || acroform.as_dictionary().is_none() {
        return Ok(Vec::new());
    }

    let fields_obj = acroform.try_get_key(b"/Fields")?;
    if fields_obj.is_null() {
        return Ok(Vec::new());
    }
    let fields = resolve_array(pdf, fields_obj)?;
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    for field in fields {
        if let Some(field_ref) = field.object_ref() {
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
    let Some(acroform) = resolve_catalog_acroform(pdf)? else {
        return Ok(None);
    };
    sig_flags_from_acroform(&acroform)
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
    let Some(acroform) = resolve_catalog_acroform(pdf)? else {
        return Ok(false);
    };
    if !clear_sig_flags_in_handle(&acroform)? {
        return Ok(false);
    }
    pdf.mark_object_handle_dirty(&acroform)?;
    Ok(true)
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
    let Some(acroform) = resolve_catalog_acroform(pdf)? else {
        return Ok(false);
    };
    let fields_obj = acroform.try_get_key(b"/Fields")?;
    if fields_obj.is_null() {
        return Ok(false);
    }

    let mut changed = false;
    let mut seen = BTreeSet::new();
    for field in resolve_array(pdf, fields_obj)? {
        let Some(field_ref) = field.object_ref() else {
            continue;
        };
        strip_signature_values_from_field(pdf, field_ref, None, 0, &mut seen, &mut changed)?;
    }
    Ok(changed)
}

// The returned `ObjectHandle` from `resolve_catalog_acroform` is live, so
// callers mutate it in place and mark the handle dirty. No copied dictionary
// or raw-object write-back boundary is needed.

/// Resolve the catalog `/AcroForm` to its dictionary plus where it lives,
/// following one indirect reference. Returns `None` when there is no `/Root`
/// dictionary, no `/AcroForm`, or `/AcroForm` is not a dictionary.
fn resolve_catalog_acroform<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Option<ObjectHandle>> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(None);
    };
    let catalog_handle = pdf.get_object_handle(root_ref);
    let catalog = resolve_handle(pdf, &catalog_handle)?;
    if catalog.as_dictionary().is_none() {
        return Ok(None);
    }
    let acroform_value = catalog.try_get_key(b"/AcroForm")?;
    let acroform = resolve_handle(pdf, &acroform_value)?;
    Ok(acroform.as_dictionary().map(|_| acroform))
}

/// Extract `/SigFlags` as a `u32` bitfield from an already-resolved `/AcroForm`
/// dictionary. Non-integer or out-of-range values read as absent.
fn sig_flags_from_acroform(acroform: &ObjectHandle) -> Result<Option<u32>> {
    Ok(acroform
        .try_get_key(b"/SigFlags")?
        .as_integer()
        .and_then(|n| u32::try_from(n).ok()))
}

/// Mask off the signature bits of `/SigFlags` in place. Returns `true` if the
/// value changed.
fn clear_sig_flags_in_handle(acroform: &ObjectHandle) -> Result<bool> {
    let Some(flags) = sig_flags_from_acroform(acroform)? else {
        return Ok(false);
    };
    let cleared = flags & !(SIG_FLAGS_SIGNATURES_EXIST | SIG_FLAGS_APPEND_ONLY);
    if cleared == flags {
        return Ok(false);
    }
    acroform.replace_key(b"/SigFlags", ObjectHandle::integer(i64::from(cleared)))?;
    Ok(true)
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

    let field_handle = pdf.get_object_handle(field_ref);
    let field = resolve_handle(pdf, &field_handle)?;
    let Some(entries) = field.as_dictionary() else {
        return Ok(());
    };

    let field_type = FormFieldObjectHelper::new(field_ref, pdf)
        .field_type()?
        .map(|name| name.strip_prefix(b"/").unwrap_or(&name).to_vec())
        .or(inherited_type);
    let kids_obj = entries
        .get(b"/Kids".as_slice())
        .cloned()
        .unwrap_or_else(ObjectHandle::null);

    let signature_value_ref = entries
        .get(b"/V".as_slice())
        .and_then(|value| value.object_ref().or_else(|| value.as_reference()));
    let has_signature_value = entries.contains_key(b"/V".as_slice());

    if field_type.as_deref() == Some(b"Sig") && has_signature_value {
        field.remove_key(b"/V");
        pdf.mark_object_handle_dirty(&field)?;
        if let Some(signature_ref) = signature_value_ref {
            pdf.delete_object(signature_ref);
        }
        *changed = true;
        if depth == DEFAULT_MAX_SIGNATURE_FIELD_DEPTH {
            return Ok(());
        }

        if kids_obj.is_null() {
            return Ok(());
        }
        return strip_signature_values_from_kids(pdf, kids_obj, field_type, depth, seen, changed);
    }

    if depth == DEFAULT_MAX_SIGNATURE_FIELD_DEPTH {
        return Ok(());
    }

    if kids_obj.is_null() {
        return Ok(());
    }
    strip_signature_values_from_kids(pdf, kids_obj, field_type, depth, seen, changed)
}

fn strip_signature_values_from_kids<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    kids_obj: ObjectHandle,
    field_type: Option<Vec<u8>>,
    depth: usize,
    seen: &mut BTreeSet<ObjectRef>,
    changed: &mut bool,
) -> Result<()> {
    for kid in resolve_array(pdf, kids_obj)? {
        let Some(kid_ref) = kid.object_ref() else {
            continue;
        };
        let kid_obj = resolve_handle(pdf, &kid)?;
        let Some(kid_dict) = kid_obj.as_dictionary() else {
            continue;
        };
        if is_pure_widget(&kid_dict) {
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

    let field_handle = pdf.get_object_handle(field_ref);
    let field_obj = resolve_handle(pdf, &field_handle)?;
    let Some(field_dict) = field_obj.as_dictionary() else {
        return Ok(());
    };

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

    let Some(kids_obj) = field_dict.get(b"/Kids".as_slice()).cloned() else {
        return Ok(());
    };
    for kid in resolve_array(pdf, kids_obj)? {
        let Some(kid_ref) = kid.object_ref() else {
            continue;
        };
        let kid_obj = resolve_handle(pdf, &kid)?;
        let Some(kid_dict) = kid_obj.as_dictionary() else {
            continue;
        };
        if is_pure_widget(&kid_dict) {
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
    let value = resolve_handle(pdf, &value)?;
    let Some(signature_dict) = value.as_dictionary() else {
        return Ok(None);
    };
    let Some(byte_range_obj) = signature_dict.get(b"/ByteRange".as_slice()).cloned() else {
        return Ok(None);
    };
    let byte_range = parse_byte_range(pdf, byte_range_obj)?;

    Ok(Some(SignatureInfo {
        field_ref,
        signature_ref,
        field_name: field_name.to_string(),
        byte_range,
        sub_filter: name_entry(pdf, &signature_dict, b"/SubFilter")?,
        signer_name: text_entry(pdf, &signature_dict, b"/Name")?,
        signing_time: text_entry(pdf, &signature_dict, b"/M")?,
        reason: text_entry(pdf, &signature_dict, b"/Reason")?,
        location: text_entry(pdf, &signature_dict, b"/Location")?,
        contact_info: text_entry(pdf, &signature_dict, b"/ContactInfo")?,
        certificate: certificate_entry(pdf, &signature_dict)?,
    }))
}

fn resolve_handle<R: Read + Seek>(pdf: &mut Pdf<R>, handle: &ObjectHandle) -> Result<ObjectHandle> {
    pdf.resolve(handle)?;
    Ok(handle.clone())
}

fn resolve_array<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: ObjectHandle,
) -> Result<Vec<ObjectHandle>> {
    Ok(resolve_handle(pdf, &value)?.as_array().unwrap_or_default())
}

fn parse_byte_range<R: Read + Seek>(pdf: &mut Pdf<R>, value: ObjectHandle) -> Result<[u64; 4]> {
    let values = resolve_handle(pdf, &value)?
        .as_array()
        .ok_or_else(|| invalid_byte_range("must be an array"))?;
    if values.len() != 4 {
        return Err(invalid_byte_range("must contain exactly four integers"));
    }

    let mut out = [0; 4];
    for (idx, value) in values.iter().enumerate() {
        let value = resolve_handle(pdf, value)?;
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

fn is_pure_widget(dict: &BTreeMap<Vec<u8>, ObjectHandle>) -> bool {
    let is_widget = dict
        .get(b"/Subtype".as_slice())
        .and_then(ObjectHandle::as_name)
        .is_some_and(|name| name == b"Widget");
    let has_field_entries = [
        b"/T".as_slice(),
        b"/FT".as_slice(),
        b"/Kids".as_slice(),
        b"/V".as_slice(),
        b"/DV".as_slice(),
        b"/Ff".as_slice(),
        b"/TU".as_slice(),
        b"/TM".as_slice(),
    ]
    .into_iter()
    .any(|key| dict.contains_key(key));

    is_widget && !has_field_entries
}

fn resolve_entry<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &BTreeMap<Vec<u8>, ObjectHandle>,
    key: &[u8],
) -> Result<Option<ObjectHandle>> {
    dict.get(key)
        .cloned()
        .map(|value| resolve_handle(pdf, &value))
        .transpose()
}

fn name_entry<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &BTreeMap<Vec<u8>, ObjectHandle>,
    key: &[u8],
) -> Result<Option<String>> {
    match resolve_entry(pdf, dict, key)? {
        Some(value) => Ok(value
            .as_name()
            .map(|name| String::from_utf8_lossy(&name).into_owned())),
        _ => Ok(None),
    }
}

fn text_entry<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &BTreeMap<Vec<u8>, ObjectHandle>,
    key: &[u8],
) -> Result<Option<String>> {
    match resolve_entry(pdf, dict, key)? {
        Some(value) => Ok(value.as_string().map(|bytes| {
            decode_pdf_text_string(&bytes).unwrap_or_else(|| String::from_utf8_lossy(&bytes).into())
        })),
        _ => Ok(None),
    }
}

fn certificate_entry<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &BTreeMap<Vec<u8>, ObjectHandle>,
) -> Result<Option<Vec<u8>>> {
    match resolve_entry(pdf, dict, b"/Cert")? {
        Some(value) if value.as_string().is_some() => Ok(value.as_string()),
        Some(value) if value.as_array().is_some() => {
            let values = value.as_array().unwrap_or_default();
            for value in values {
                let value = resolve_handle(pdf, &value)?;
                if let Some(bytes) = value.as_string() {
                    return Ok(Some(bytes));
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

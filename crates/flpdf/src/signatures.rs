//! Digital signature helpers.
//!
//! This module has two layers:
//! - read-only AcroForm signature field inspection via [`signatures`];
//! - rewrite-impact checks for whether a writer mode would invalidate existing
//!   signed `/ByteRange`s.

use crate::json_inspect::decode_pdf_text_string;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result, WriteOptions};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

/// Maximum recursion depth for AcroForm signature field traversal.
pub const DEFAULT_MAX_SIGNATURE_FIELD_DEPTH: usize = crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;

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
}

/// Return all signed AcroForm signature fields in document field order.
pub fn signatures<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Vec<SignatureInfo>> {
    signatures_with_max_depth(pdf, DEFAULT_MAX_SIGNATURE_FIELD_DEPTH)
}

/// Like [`signatures`], but with an explicit field-tree recursion limit.
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

    let fields = resolve_array(pdf, acroform_dict.get("Fields").cloned())?;
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
pub fn would_rewrite_invalidate_signatures<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: &WriteOptions,
) -> Result<bool> {
    Ok(
        signature_rewrite_impact(pdf, SignatureWriteMode::from_options(options))?
            .invalidates_signatures,
    )
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
    })
}

#[derive(Debug, Default)]
struct SignatureRewriteInfo {
    acroform_ref: Option<ObjectRef>,
    signed_object_refs: BTreeSet<ObjectRef>,
}

fn collect_signature_rewrite_info<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> Result<SignatureRewriteInfo> {
    let mut info = SignatureRewriteInfo::default();

    if let Some(root_ref) = pdf.root_ref() {
        let root = pdf.resolve(root_ref)?;
        if let Object::Dictionary(catalog) = root {
            if let Some(acroform) = catalog.get("AcroForm").cloned() {
                info.acroform_ref = Some(acroform_ref(root_ref, &acroform));
            }

            for key in ["Perms", "DSS"] {
                if let Some(value) = catalog.get(key).cloned() {
                    collect_known_signature_value(pdf, value, &mut info, 0)?;
                }
            }
        }
    }

    for signature in signatures(pdf)? {
        info.signed_object_refs.insert(signature.field_ref);
        if let Some(signature_ref) = signature.signature_ref {
            info.signed_object_refs.insert(signature_ref);
        }
    }

    Ok(info)
}

fn acroform_ref(root_ref: ObjectRef, acroform: &Object) -> ObjectRef {
    match acroform {
        Object::Reference(acroform_ref) => *acroform_ref,
        _ => root_ref,
    }
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
        Object::Stream(stream) => {
            if stream.dict.get("ByteRange").is_some() {
                for (_, value) in stream.dict.iter() {
                    collect_known_signature_value(pdf, value.clone(), info, depth + 1)?;
                }
            }
        }
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

    for kid in resolve_array(pdf, field_dict.get("Kids").cloned())? {
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

fn resolve_array<R: Read + Seek>(pdf: &mut Pdf<R>, value: Option<Object>) -> Result<Vec<Object>> {
    match value {
        Some(Object::Array(values)) => Ok(values),
        Some(Object::Reference(object_ref)) => match pdf.resolve_borrowed(object_ref)? {
            Object::Array(values) => Ok(values.clone()),
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
    while let Some(Object::Reference(parent_ref)) = parent {
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

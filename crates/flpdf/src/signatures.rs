//! Digital signature field inspection helpers.

use crate::json_inspect::decode_pdf_text_string;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
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
            walk_field(pdf, field_ref, "", &mut output, &mut seen, 0, max_depth)?;
        }
    }
    Ok(output)
}

fn walk_field<R: Read + Seek>(
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
        walk_field(
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

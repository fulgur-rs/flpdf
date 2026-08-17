//! qpdf correspondence: `QPDF_json.cc` validators and `JSONReactor::makeObject`
//! value construction (`libqpdf/QPDF_json.cc:65-209, 732-793`).
//!
//! This is the canonical value boundary for the JSON input importer. It builds
//! document-owned `ObjectHandle` values and never routes through the legacy
//! `Object`/`Pdf::set_object` representation.

use std::io::{Read, Seek};

use super::value::format_qpdf_real;
use super::Json;
use crate::object_handle::ObjectValue;
use crate::{Error, ObjectHandle, ObjectRef, Pdf, Result};

/// Parse qpdf's exact JSON indirect-reference spelling.
///
/// This intentionally accepts only ASCII spaces between the three tokens,
/// matching `QPDF_json.cc:is_indirect_object`: tabs and other JSON whitespace
/// are not accepted by this validator.
pub(crate) fn parse_indirect_reference(value: &[u8]) -> Option<ObjectRef> {
    let mut cursor = 0;
    let object_start = cursor;
    while value.get(cursor).is_some_and(|byte| byte.is_ascii_digit()) {
        cursor += 1;
    }
    if cursor == object_start || value.get(cursor) != Some(&b' ') {
        return None;
    }
    let object = std::str::from_utf8(&value[object_start..cursor])
        .ok()?
        .parse::<u64>()
        .ok()?;

    while value.get(cursor) == Some(&b' ') {
        cursor += 1;
    }
    let generation_start = cursor;
    while value.get(cursor).is_some_and(|byte| byte.is_ascii_digit()) {
        cursor += 1;
    }
    if cursor == generation_start || value.get(cursor) != Some(&b' ') {
        return None;
    }
    let generation = std::str::from_utf8(&value[generation_start..cursor])
        .ok()?
        .parse::<u64>()
        .ok()?;

    while value.get(cursor) == Some(&b' ') {
        cursor += 1;
    }
    if value.get(cursor) != Some(&b'R') || cursor + 1 != value.len() {
        return None;
    }

    if object == 0 {
        return None;
    }
    Some(ObjectRef::new(
        u32::try_from(object).ok()?,
        u16::try_from(generation).ok()?,
    ))
}

/// Parse qpdf's `obj:N G R` object-key spelling.
pub(crate) fn parse_object_key(value: &[u8]) -> Option<ObjectRef> {
    value
        .strip_prefix(b"obj:")
        .and_then(parse_indirect_reference)
}

/// Convert one JSON value to a canonical document-owned handle.
pub(crate) fn json_value_to_handle<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    value: &Json,
) -> Result<ObjectHandle> {
    if value.is_dictionary() {
        let mut entries = std::collections::BTreeMap::new();
        let mut previous_key = None;
        while let Some((raw_key, child)) = value.next_dictionary_item_after(previous_key.as_deref())
        {
            let key = json_dictionary_key(&raw_key)?;
            entries.insert(key, json_value_to_handle(pdf, &child)?);
            previous_key = Some(raw_key);
        }
        return Ok(pdf
            .resolver
            .direct_object_handle(ObjectValue::Dictionary(entries)));
    }

    if value.is_array() {
        let children = value
            .array_items_snapshot()
            .ok_or_else(|| Error::Internal("JSON array disappeared during conversion".into()))?
            .iter()
            .map(|child| json_value_to_handle(pdf, child))
            .collect::<Result<Vec<_>>>()?;
        return Ok(pdf
            .resolver
            .direct_object_handle(ObjectValue::Array(children)));
    }

    if value.is_null() {
        return Ok(pdf.resolver.direct_object_handle(ObjectValue::Null));
    }
    if let Some(boolean) = value.get_bool() {
        return Ok(pdf
            .resolver
            .direct_object_handle(ObjectValue::Boolean(boolean)));
    }
    if let Some(number) = value.get_number() {
        return json_number_to_handle(pdf, &number);
    }
    if let Some(string) = value.get_string() {
        return json_string_to_handle(pdf, &string);
    }

    Err(Error::Internal(
        "JSON value has no initialized qpdf value kind".into(),
    ))
}

fn json_number_to_handle<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    number: &[u8],
) -> Result<ObjectHandle> {
    let text = std::str::from_utf8(number)
        .map_err(|_| Error::Unsupported("invalid JSON number".into()))?;
    if let Ok(integer) = text.parse::<i64>() {
        return Ok(pdf
            .resolver
            .direct_object_handle(ObjectValue::Integer(integer)));
    }

    let real = text
        .parse::<f64>()
        .map_err(|_| Error::Unsupported("invalid JSON number".into()))?;
    if !real.is_finite() {
        return Err(Error::Unsupported("invalid JSON number".into()));
    }
    let literal = if text.contains(['e', 'E']) {
        format_qpdf_real(real)
    } else {
        text.to_owned()
    };
    Ok(pdf.resolver.direct_object_handle(ObjectValue::RealLiteral {
        value: real,
        literal: literal.into_bytes(),
    }))
}

fn json_string_to_handle<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    string: &[u8],
) -> Result<ObjectHandle> {
    if let Some(object_ref) = parse_indirect_reference(string) {
        return Ok(pdf.get_object_handle(object_ref));
    }

    if let Some(unicode) = string.strip_prefix(b"u:") {
        return Ok(pdf.resolver.direct_object_handle(ObjectValue::String(
            crate::pdf_string::new_unicode_string(unicode),
        )));
    }

    if let Some(binary) = string.strip_prefix(b"b:") {
        if binary.len() % 2 != 0 || !binary.iter().all(u8::is_ascii_hexdigit) {
            return Ok(pdf.resolver.direct_object_handle(ObjectValue::Null));
        }
        let decoded = hex::decode(binary)
            .map_err(|_| Error::Unsupported("invalid binary JSON string".into()))?;
        return Ok(pdf
            .resolver
            .direct_object_handle(ObjectValue::String(decoded)));
    }

    if string.first() == Some(&b'/') && string.len() > 1 {
        return Ok(pdf
            .resolver
            .direct_object_handle(ObjectValue::Name(string[1..].to_vec())));
    }

    if string.starts_with(b"n:/") && string.len() > 3 {
        let parsed = ObjectHandle::parse(&string[2..])?;
        let name = parsed
            .as_name()
            .ok_or_else(|| Error::Internal("PDF name parser returned a non-name".into()))?;
        return Ok(pdf.resolver.direct_object_handle(ObjectValue::Name(name)));
    }

    // QPDF_json.cc reports an error and substitutes null for an unrecognized
    // string. The Reactor owns warning/error attribution; this value factory
    // supplies the same null replacement without emitting a second warning.
    Ok(pdf.resolver.direct_object_handle(ObjectValue::Null))
}

fn json_dictionary_key(key: &[u8]) -> Result<Vec<u8>> {
    if key.starts_with(b"n:/") && key.len() > 3 {
        let parsed = ObjectHandle::parse(&key[2..])?;
        let name = parsed.as_name().ok_or_else(|| {
            // cov:ignore-start: a successful parse of slash-prefixed input cannot yield a non-name
            Error::Internal("PDF dictionary key parser returned a non-name".into())
            // cov:ignore-end
        })?; // cov:ignore: same parser invariant
        let mut canonical = Vec::with_capacity(name.len() + 1);
        canonical.push(b'/');
        canonical.extend_from_slice(&name);
        return Ok(canonical);
    }
    Ok(crate::object_handle::canonical_dictionary_key(key))
}

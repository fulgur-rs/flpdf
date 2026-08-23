use std::io::{Read, Seek};

use flpdf::{Dictionary, Error, Object, ObjectHandle, ObjectRef, Pdf};

const MAX_REF_CHAIN_DEPTH: usize = 64;

pub(crate) struct ResolvedStreamDictionary {
    dictionary: Dictionary,
    filterable: bool,
    decode_param_type_warnings: Vec<DecodeParamTypeWarning>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DecodeParamTypeWarning {
    pub(crate) filter_index: usize,
    pub(crate) object_type: &'static str,
    pub(crate) source: DecodeParmsWarningSource,
}

/// Where a `/DecodeParms` type-warning's object/offset attribution comes
/// from, mirroring qpdf's `QPDFObjectHandle::typeWarning` description:
/// whichever object's own bytes physically hold the offending value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecodeParmsWarningSource {
    /// The value sits directly in the stream dictionary itself (no
    /// reference was followed at or above this filter position):
    /// attribute to the stream's own indirect object via the existing
    /// `/DecodeParms` dictionary/array locator.
    StreamDictionary,
    /// The value is the *entire* body of this indirect object (a
    /// reference chain — at this value or its enclosing array — that
    /// terminated on a non-array, or on an array consumed as one unit).
    ObjectBody(ObjectRef),
    /// The value is a direct item at this array index inside this
    /// indirect object's own body: the enclosing `/DecodeParms` array was
    /// itself reached through a reference, but the item at this position
    /// is not itself a reference, so qpdf keeps the array's own
    /// description and its item's own precise parsed position.
    ArrayItem(ObjectRef, usize),
}

impl ResolvedStreamDictionary {
    pub(crate) fn is_filterable(&self) -> bool {
        self.filterable
    }

    pub(crate) fn decode_param_type_warnings(&self) -> &[DecodeParamTypeWarning] {
        &self.decode_param_type_warnings
    }

    /// Return the canonical filter-input dictionary without reintroducing the
    /// removed `&Dictionary` filter API. The qtest driver retains its
    /// materialized dictionary for warning attribution, but codec access now
    /// receives only the `/Filter` and `/DecodeParms` handle view.
    pub(crate) fn filter_input_handle(&self) -> ObjectHandle {
        let mut entries = Vec::new();
        for (key, value) in [
            (b"/Filter".as_slice(), self.dictionary.get(b"Filter")),
            (
                b"/DecodeParms".as_slice(),
                self.dictionary.get(b"DecodeParms"),
            ),
        ] {
            if let Some(value) = value {
                let value = if key == b"/DecodeParms" {
                    decode_params_handle(value)
                } else {
                    filter_value_handle(value)
                };
                entries.push((key.to_vec(), value));
            }
        }
        ObjectHandle::dictionary(entries)
    }
}

fn decode_params_handle(value: &Object) -> ObjectHandle {
    match value {
        Object::Null => ObjectHandle::null(),
        Object::Array(values) => ObjectHandle::array(
            values
                .iter()
                .map(|value| match value {
                    Object::Null => ObjectHandle::null(),
                    Object::Dictionary(_) => filter_value_handle(value),
                    // The qtest driver has already emitted qpdf's type
                    // warning. A consumer filter then sees an empty
                    // dictionary, matching QPDFObjectHandle::getKeys on a
                    // non-dictionary DecodeParms item.
                    _ => ObjectHandle::dictionary(Vec::new()),
                })
                .collect(),
        ),
        Object::Dictionary(_) => filter_value_handle(value),
        // See the array-item case above: qpdf warns and treats a scalar
        // DecodeParms body as an empty dictionary for a consuming filter.
        _ => ObjectHandle::dictionary(Vec::new()),
    }
}

fn filter_value_handle(value: &Object) -> ObjectHandle {
    match value {
        Object::Null => ObjectHandle::null(),
        Object::Boolean(value) => ObjectHandle::boolean(*value),
        Object::Integer(value) => ObjectHandle::integer(*value),
        Object::Real(value) => ObjectHandle::real(*value),
        Object::RealLiteral { value, .. } => ObjectHandle::real(*value),
        Object::Name(value) => ObjectHandle::name(value.clone()),
        Object::String(value) => ObjectHandle::string(value.clone()),
        Object::Array(values) => {
            ObjectHandle::array(values.iter().map(filter_value_handle).collect())
        }
        Object::Dictionary(dictionary) => ObjectHandle::dictionary(
            dictionary
                .iter()
                .map(|(key, value)| {
                    let mut canonical = Vec::with_capacity(key.len() + 1);
                    canonical.push(b'/');
                    canonical.extend_from_slice(key);
                    (canonical, filter_value_handle(value))
                })
                .collect(),
        ),
        Object::Reference(_) | Object::Stream(_) | Object::Operator(_) | Object::InlineImage(_) => {
            ObjectHandle::boolean(false)
        }
    }
}

impl std::ops::Deref for ResolvedStreamDictionary {
    type Target = Dictionary;

    fn deref(&self) -> &Self::Target {
        &self.dictionary
    }
}

pub(crate) fn write_object(object: &Object) -> Vec<u8> {
    let mut bytes = Vec::new();
    object.write_pdf(&mut bytes);
    bytes
}

pub(crate) fn write_qpdf_object<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    object: &Object,
) -> flpdf::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    write_qpdf_object_into(pdf, object, &mut bytes)?;
    Ok(bytes)
}

fn write_qpdf_object_into<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    object: &Object,
    bytes: &mut Vec<u8>,
) -> flpdf::Result<()> {
    match object {
        Object::Array(values) => {
            bytes.extend_from_slice(b"[ ");
            for value in values {
                write_qpdf_object_into(pdf, value, bytes)?;
                bytes.push(b' ');
            }
            bytes.push(b']');
        }
        Object::Dictionary(dictionary) => {
            bytes.extend_from_slice(b"<< ");
            for (key, value) in dictionary.iter() {
                let (resolved, indirect, _terminal) = resolve_chain(pdf, value.clone())?;
                if resolved.is_null() {
                    continue;
                }
                Object::Name(key.to_vec()).write_pdf(bytes);
                bytes.push(b' ');
                if let Some(reference) = indirect {
                    Object::Reference(reference).write_pdf(bytes);
                } else {
                    write_qpdf_object_into(pdf, &resolved, bytes)?;
                }
                bytes.push(b' ');
            }
            bytes.extend_from_slice(b">>");
        }
        _ => object.write_pdf(bytes),
    }
    Ok(())
}

pub(crate) fn resolve_chain<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    mut value: Object,
) -> flpdf::Result<(Object, Option<ObjectRef>, Option<ObjectRef>)> {
    let mut indirect = None;
    let mut terminal_indirect = None;
    for _ in 0..MAX_REF_CHAIN_DEPTH {
        let Object::Reference(reference) = value else {
            return Ok((value, indirect, terminal_indirect));
        };
        indirect.get_or_insert(reference);
        terminal_indirect = Some(reference);
        value = pdf.resolve_borrowed(reference)?.clone();
    }
    if matches!(value, Object::Reference(_)) {
        Err(Error::parse(
            0,
            format!("object reference chain exceeds {MAX_REF_CHAIN_DEPTH} hops"),
        ))
    } else {
        Ok((value, indirect, terminal_indirect))
    }
}

pub(crate) fn resolve_stream_dictionary<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    source: &Dictionary,
) -> flpdf::Result<ResolvedStreamDictionary> {
    let filter = source
        .get(b"Filter")
        .cloned()
        .map(|value| resolve_filter_structure(pdf, value))
        .transpose()?;
    let filter_names = filter.as_ref().and_then(resolved_filter_names);
    // QPDF_Stream::filterable checks every name against its factory table
    // before it asks for /DecodeParms. In particular, an unknown later name
    // prevents a preceding known filter from traversing a diagnostic parameter
    // reference chain.
    let mut filterable = filter_names
        .as_deref()
        .is_none_or(|names| names.iter().all(|name| qpdf_filter_factory_exists(name)));

    let mut decode_parms_sources: Vec<DecodeParmsWarningSource> = Vec::new();
    let mut resolved = Dictionary::new();
    for (key, value) in source.iter() {
        let value = if key == b"Filter" {
            filter.clone().unwrap_or_else(|| value.clone())
        } else if key == b"DecodeParms" {
            match filter_names.as_deref() {
                Some(names) if filterable => {
                    let (value, sources) = resolve_decode_params(pdf, names, value.clone())?;
                    decode_parms_sources = sources;
                    value
                }
                None => value.clone(),
                Some(_) => value.clone(),
            }
        } else {
            value.clone()
        };
        resolved.insert(key, value);
    }
    let mut decode_param_type_warnings = Vec::new();
    if filterable {
        if let Some(names) = filter_names.as_deref() {
            if let Some(params) = aligned_decode_params(names, resolved.get(b"DecodeParms")) {
                for (filter_index, (filter, params)) in names.iter().zip(params).enumerate() {
                    let normalized = normalized_filter_name(filter);
                    if params.is_some_and(|params| {
                        !matches!(params, Object::Null | Object::Dictionary(_))
                    }) && matches!(normalized, b"Crypt" | b"FlateDecode" | b"LZWDecode")
                    {
                        decode_param_type_warnings.push(DecodeParamTypeWarning {
                            filter_index,
                            object_type: qpdf_object_type_name(
                                params.expect("non-null DecodeParms"),
                            ),
                            source: decode_parms_sources
                                .get(filter_index)
                                .copied()
                                .unwrap_or(DecodeParmsWarningSource::StreamDictionary),
                        });
                    }
                    if normalized == b"Crypt" && !crypt_decode_params_filterable(params) {
                        filterable = false;
                    }
                }
                remove_identity_crypt_stages(&mut resolved, names);
            }
        }
    }
    Ok(ResolvedStreamDictionary {
        dictionary: resolved,
        filterable,
        decode_param_type_warnings,
    })
}

fn aligned_decode_params<'a>(
    filters: &[&[u8]],
    decode_params: Option<&'a Object>,
) -> Option<Vec<Option<&'a Object>>> {
    match decode_params {
        None | Some(Object::Null) => Some(vec![None; filters.len()]),
        Some(Object::Array(values)) if values.is_empty() => Some(vec![None; filters.len()]),
        Some(Object::Array(values)) if values.len() == filters.len() => Some(
            values
                .iter()
                .map(|value| (!value.is_null()).then_some(value))
                .collect(),
        ),
        Some(Object::Array(_)) => None,
        Some(value) => Some(vec![Some(value); filters.len()]),
    }
}

fn crypt_decode_params_filterable(decode_params: Option<&Object>) -> bool {
    let Some(decode_params) = decode_params else {
        return true;
    };
    if decode_params.is_null() {
        return true;
    }
    let Some(dictionary) = decode_params.as_dict() else {
        // QPDFObjectHandle::getKeys warns and returns an empty key set.
        return true;
    };
    let visible: Vec<(&[u8], &Object)> = dictionary
        .iter()
        .filter(|(_, value)| !value.is_null())
        .collect();
    let type_is_valid = visible
        .iter()
        .find(|(key, _)| *key == b"Type")
        .is_none_or(|(_, value)| value.as_name() == Some(b"CryptFilterDecodeParms".as_slice()));
    type_is_valid
        && visible
            .iter()
            .all(|(key, _)| matches!(*key, b"Type" | b"Name"))
}

fn qpdf_object_type_name(object: &Object) -> &'static str {
    match object {
        Object::Null => "null",
        Object::Boolean(_) => "boolean",
        Object::Integer(_) => "integer",
        Object::Real(_) | Object::RealLiteral { .. } => "real",
        Object::String(_) => "string",
        Object::Name(_) => "name",
        Object::Array(_) => "array",
        Object::Dictionary(_) => "dictionary",
        Object::Stream(_) => "stream",
        Object::Operator(_) => "operator",
        Object::InlineImage(_) => "inline-image",
        Object::Reference(_) => "unresolved",
    }
}

fn remove_identity_crypt_stages(dictionary: &mut Dictionary, filters: &[&[u8]]) {
    let crypt_indices: Vec<usize> = filters
        .iter()
        .enumerate()
        .filter_map(|(index, filter)| (normalized_filter_name(filter) == b"Crypt").then_some(index))
        .collect();
    if crypt_indices.is_empty() {
        return;
    }

    match dictionary.get(b"Filter").cloned() {
        Some(Object::Name(name)) if normalized_filter_name(&name) == b"Crypt" => {
            dictionary.insert(b"Filter", Object::Null);
        }
        Some(Object::Array(values)) => {
            let values = values
                .into_iter()
                .enumerate()
                .filter_map(|(index, value)| (!crypt_indices.contains(&index)).then_some(value))
                .collect();
            dictionary.insert(b"Filter", Object::Array(values));
        }
        _ => {}
    }

    if let Some(Object::Array(values)) = dictionary.get(b"DecodeParms").cloned() {
        if values.len() == filters.len() {
            let values = values
                .into_iter()
                .enumerate()
                .filter_map(|(index, value)| (!crypt_indices.contains(&index)).then_some(value))
                .collect();
            dictionary.insert(b"DecodeParms", Object::Array(values));
        }
    }
}

fn resolved_filter_names(filter: &Object) -> Option<Vec<&[u8]>> {
    match filter {
        Object::Null => Some(Vec::new()),
        Object::Name(name) => Some(vec![name]),
        Object::Array(values) => values.iter().map(Object::as_name).collect(),
        _ => None,
    }
}

fn normalized_filter_name(name: &[u8]) -> &[u8] {
    match name {
        b"AHx" => b"ASCIIHexDecode",
        b"A85" => b"ASCII85Decode",
        b"Fl" => b"FlateDecode",
        b"LZW" => b"LZWDecode",
        b"RL" => b"RunLengthDecode",
        b"CCF" => b"CCITTFaxDecode",
        b"DCT" => b"DCTDecode",
        name => name,
    }
}

fn qpdf_filter_factory_exists(name: &[u8]) -> bool {
    matches!(
        normalized_filter_name(name),
        b"Crypt"
            | b"FlateDecode"
            | b"LZWDecode"
            | b"RunLengthDecode"
            | b"DCTDecode"
            | b"ASCII85Decode"
            | b"ASCIIHexDecode"
    )
}

fn filter_consumes_decode_key(filter: &[u8], key: &[u8]) -> bool {
    match normalized_filter_name(filter) {
        b"FlateDecode" => matches!(
            key,
            b"Predictor" | b"Columns" | b"Colors" | b"BitsPerComponent"
        ),
        b"LZWDecode" => matches!(
            key,
            b"Predictor" | b"Columns" | b"Colors" | b"BitsPerComponent" | b"EarlyChange"
        ),
        // SF_Crypt::setDecodeParms calls getKeys(), which resolves every value
        // to omit null entries before validating /Type, /Name, and unknown keys.
        b"Crypt" => true,
        _ => false,
    }
}

fn resolve_decode_param_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filters: &[&[u8]],
    dictionary: Dictionary,
) -> flpdf::Result<Object> {
    let entries: Vec<(Vec<u8>, Object)> = dictionary
        .iter()
        .map(|(key, value)| (key.to_vec(), value.clone()))
        .collect();
    let mut resolved = Dictionary::new();
    for (key, value) in entries {
        let consumed = filters
            .iter()
            .any(|filter| filter_consumes_decode_key(filter, &key));
        let value = if consumed {
            resolve_chain(pdf, value)?.0
        } else {
            value
        };
        // QPDFObjectHandle::getKeys omits direct values and indirect values
        // resolving to null. Keep that visibility rule for keys consumed by
        // the decoder so a null /Predictor (etc.) uses qpdf's default.
        if !consumed || !value.is_null() {
            resolved.insert(key, value);
        }
    }
    Ok(Object::Dictionary(resolved))
}

/// Resolve one `/DecodeParms` value (a scalar, or one aligned array item)
/// against `filters`, returning the resolved value plus the indirect object
/// (if any) whose own bytes directly hold it — i.e. the terminal reference
/// of *this* value's own chain, not any enclosing container's.
fn resolve_decode_param_for_filters<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filters: &[&[u8]],
    value: Object,
) -> flpdf::Result<(Object, Option<ObjectRef>)> {
    let (value, _, terminal_ref) = resolve_chain(pdf, value)?;
    let value = match value {
        Object::Dictionary(dictionary) => resolve_decode_param_dict(pdf, filters, dictionary)?,
        other => other,
    };
    Ok((value, terminal_ref))
}

/// Resolve `/DecodeParms` and, for each filter position, where a type
/// warning at that position should be attributed.
///
/// qpdf attributes a non-dictionary `/DecodeParms` value to the innermost
/// indirect object whose own bytes physically hold the offending token: an
/// array item that is itself a reference wins over its enclosing array
/// (attributed to that item's own object, as its whole body), but a direct
/// item inherits the enclosing array's own indirect object — as one item
/// inside that array — when the array itself was reached through a
/// reference, rather than reporting no indirection at all.
fn resolve_decode_params<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filters: &[&[u8]],
    value: Object,
) -> flpdf::Result<(Object, Vec<DecodeParmsWarningSource>)> {
    let (value, _, container_ref) = resolve_chain(pdf, value)?;
    match value {
        Object::Array(values) if values.len() == filters.len() => {
            let mut resolved_values = Vec::with_capacity(values.len());
            let mut sources = Vec::with_capacity(values.len());
            for (index, (value, filter)) in
                values.into_iter().zip(filters.iter().copied()).enumerate()
            {
                let (value, item_ref) = resolve_decode_param_for_filters(pdf, &[filter], value)?;
                resolved_values.push(value);
                sources.push(match (item_ref, container_ref) {
                    (Some(item_ref), _) => DecodeParmsWarningSource::ObjectBody(item_ref),
                    (None, Some(container_ref)) => {
                        DecodeParmsWarningSource::ArrayItem(container_ref, index)
                    }
                    (None, None) => DecodeParmsWarningSource::StreamDictionary,
                });
            }
            Ok((Object::Array(resolved_values), sources))
        }
        Object::Array(values) => {
            let sources = vec![DecodeParmsWarningSource::StreamDictionary; values.len()];
            Ok((Object::Array(values), sources))
        }
        other => {
            let (value, _) = resolve_decode_param_for_filters(pdf, filters, other)?;
            let source = container_ref
                .map(DecodeParmsWarningSource::ObjectBody)
                .unwrap_or(DecodeParmsWarningSource::StreamDictionary);
            Ok((value, vec![source; filters.len()]))
        }
    }
}

fn resolve_filter_structure<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: Object,
) -> flpdf::Result<Object> {
    let (resolved, _, _) = resolve_chain(pdf, value)?;
    match resolved {
        Object::Array(values) => {
            let values = values
                .into_iter()
                .map(|value| resolve_chain(pdf, value).map(|(value, _, _)| value))
                .collect::<flpdf::Result<Vec<_>>>()?;
            Ok(Object::Array(values))
        }
        other => Ok(other),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        crypt_decode_params_filterable, qpdf_object_type_name, remove_identity_crypt_stages,
        resolve_chain, resolve_stream_dictionary,
    };
    use flpdf::{Dictionary, Object, ObjectRef, Pdf, Stream};
    use std::io::Cursor;

    fn pdf_with_objects(objects: &[(u32, &[u8])], trailer_extra: &[u8]) -> Vec<u8> {
        let max_object = objects.iter().map(|(number, _)| *number).max().unwrap_or(2);
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = vec![None; (max_object + 1) as usize];
        for (number, body) in objects {
            offsets[*number as usize] = Some(bytes.len());
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = bytes.len();
        bytes.extend_from_slice(format!("xref\n0 {}\n", max_object + 1).as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets.into_iter().skip(1) {
            match offset {
                Some(offset) => {
                    bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes())
                }
                None => bytes.extend_from_slice(b"0000000000 00000 f \n"),
            }
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size {} /Root 1 0 R", max_object + 1).as_bytes(),
        );
        bytes.extend_from_slice(trailer_extra);
        bytes.extend_from_slice(format!(" >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
        bytes
    }

    fn handle_pdf(trailer_extra: &[u8]) -> Pdf<Cursor<Vec<u8>>> {
        let bytes = pdf_with_objects(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Count 0 /Kids [ ] >>"),
                (6, b"7 0 R"),
                (7, b"true"),
                (8, b"null"),
                (9, b"<< /Length 3 >>\nstream\nabc\nendstream"),
                (10, b"11 0 R"),
                (11, b"/FlateDecode"),
                (12, b"<< /Predictor 13 0 R /Columns 14 0 R >>"),
                (13, b"15"),
                (14, b"3"),
            ],
            trailer_extra,
        );
        Pdf::open_mem_owned(bytes).expect("open handle fixture")
    }

    #[test]
    fn decode_param_type_names_cover_every_qpdf_object_kind() {
        let cases = [
            (Object::Null, "null"),
            (Object::Boolean(false), "boolean"),
            (Object::Integer(1), "integer"),
            (Object::Real(1.5), "real"),
            (
                Object::RealLiteral {
                    value: 0.4,
                    literal: b".4".to_vec(),
                },
                "real",
            ),
            (Object::String(b"s".to_vec()), "string"),
            (Object::Name(b"N".to_vec()), "name"),
            (Object::Array(Vec::new()), "array"),
            (Object::Dictionary(Dictionary::new()), "dictionary"),
            (
                Object::Stream(Stream::new(Dictionary::new(), Vec::new())),
                "stream",
            ),
            (Object::Operator(b"q".to_vec()), "operator"),
            (Object::InlineImage(b"abc".to_vec()), "inline-image"),
            (Object::Reference(ObjectRef::new(99, 0)), "unresolved"),
        ];

        for (object, expected) in cases {
            assert_eq!(qpdf_object_type_name(&object), expected);
        }
    }

    #[test]
    fn crypt_helpers_accept_nullish_params_and_leave_defensive_mismatches_unchanged() {
        assert!(crypt_decode_params_filterable(Some(&Object::Null)));
        assert!(crypt_decode_params_filterable(Some(&Object::Integer(42))));

        let mut dictionary = Dictionary::new();
        dictionary.insert(b"Filter", Object::Integer(1));
        dictionary.insert(
            b"DecodeParms",
            Object::Array(vec![Object::Null, Object::Null]),
        );
        let original = dictionary.clone();

        remove_identity_crypt_stages(&mut dictionary, &[b"Crypt".as_slice()]);

        assert_eq!(dictionary, original);
    }

    #[test]
    fn stream_filter_and_decode_params_reference_chains_are_fully_resolved() {
        let mut pdf = handle_pdf(b"");
        pdf.set_object(
            ObjectRef::new(10, 0),
            Object::Reference(ObjectRef::new(11, 0)),
        );
        let mut dictionary = Dictionary::new();
        dictionary.insert(b"Filter", Object::Reference(ObjectRef::new(10, 0)));
        dictionary.insert(
            b"DecodeParms",
            Object::Array(vec![Object::Reference(ObjectRef::new(12, 0))]),
        );

        let resolved =
            resolve_stream_dictionary(&mut pdf, &dictionary).expect("resolve stream dictionary");
        assert_eq!(
            resolved.get(b"Filter"),
            Some(&Object::Name(b"FlateDecode".to_vec()))
        );
        let params = resolved
            .get(b"DecodeParms")
            .and_then(Object::as_array)
            .and_then(|items| items.first())
            .and_then(Object::as_dict)
            .expect("resolved DecodeParms dictionary");
        assert_eq!(params.get(b"Predictor"), Some(&Object::Integer(15)));
        assert_eq!(params.get(b"Columns"), Some(&Object::Integer(3)));
    }

    #[test]
    fn reference_chain_accepts_64_hops_and_rejects_65() {
        fn install_chain(pdf: &mut Pdf<Cursor<Vec<u8>>>, hops: u32) {
            for index in 0..hops {
                let object_ref = ObjectRef::new(100 + index, 0);
                let value = if index + 1 == hops {
                    Object::Boolean(true)
                } else {
                    Object::Reference(ObjectRef::new(101 + index, 0))
                };
                pdf.set_object(object_ref, value);
            }
        }

        let mut accepted = handle_pdf(b"");
        install_chain(&mut accepted, 64);
        let (resolved, indirect, terminal_indirect) =
            resolve_chain(&mut accepted, Object::Reference(ObjectRef::new(100, 0)))
                .expect("64-hop reference chain");
        assert_eq!(resolved.as_bool(), Some(true));
        assert_eq!(indirect, Some(ObjectRef::new(100, 0)));
        assert_eq!(terminal_indirect, Some(ObjectRef::new(163, 0)));

        let mut rejected = handle_pdf(b"");
        install_chain(&mut rejected, 65);
        let result = resolve_chain(&mut rejected, Object::Reference(ObjectRef::new(100, 0)));
        assert!(result.is_err(), "65-hop reference chain was accepted");
        let error = result.expect_err("65-hop error");
        assert!(error.to_string().contains("exceeds 64 hops"));
    }

    #[test]
    fn stream_parameter_resolution_ignores_deep_unknown_values() {
        let mut pdf = handle_pdf(b"");
        let metadata = (0..64).fold(Object::Integer(1), |value, _| Object::Array(vec![value]));
        let mut params = Dictionary::new();
        params.insert(b"Predictor", Object::Reference(ObjectRef::new(13, 0)));
        params.insert(b"Metadata", metadata.clone());
        let mut dictionary = Dictionary::new();
        dictionary.insert(b"Filter", Object::Name(b"FlateDecode".to_vec()));
        dictionary.insert(b"DecodeParms", Object::Dictionary(params));

        let resolved = resolve_stream_dictionary(&mut pdf, &dictionary)
            .expect("resolve consumed stream parameters");
        let resolved_params = resolved
            .get(b"DecodeParms")
            .and_then(Object::as_dict)
            .expect("resolved DecodeParms");
        assert_eq!(
            resolved_params.get(b"Predictor"),
            Some(&Object::Integer(15))
        );
        assert_eq!(resolved_params.get(b"Metadata"), Some(&metadata));
    }

    #[test]
    fn known_decode_parameter_nulls_are_omitted_after_direct_or_indirect_resolution() {
        for predictor in [Object::Null, Object::Reference(ObjectRef::new(21, 0))] {
            let mut pdf = handle_pdf(b"");
            pdf.set_object(ObjectRef::new(21, 0), Object::Null);
            let mut params = Dictionary::new();
            params.insert(b"Predictor", predictor);
            let mut dictionary = Dictionary::new();
            dictionary.insert(b"Filter", Object::Name(b"FlateDecode".to_vec()));
            dictionary.insert(b"DecodeParms", Object::Dictionary(params));

            let resolved = resolve_stream_dictionary(&mut pdf, &dictionary)
                .expect("resolve DecodeParms with a null known key");
            let params = resolved
                .get(b"DecodeParms")
                .and_then(Object::as_dict)
                .expect("resolved DecodeParms dictionary");
            assert!(
                params.get(b"Predictor").is_none(),
                "a resolved null /Predictor must be absent so qpdf defaults apply"
            );
        }
    }

    #[test]
    fn unsupported_filter_chain_skips_decode_parameter_resolution() {
        let mut pdf = handle_pdf(b"");
        for index in 0..65 {
            let reference = ObjectRef::new(100 + index, 0);
            let value = if index == 64 {
                Object::Null
            } else {
                Object::Reference(ObjectRef::new(101 + index, 0))
            };
            pdf.set_object(reference, value);
        }
        let mut params = Dictionary::new();
        params.insert(b"Predictor", Object::Reference(ObjectRef::new(100, 0)));
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            b"Filter",
            Object::Array(vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"BogusDecode".to_vec()),
            ]),
        );
        dictionary.insert(b"DecodeParms", Object::Dictionary(params.clone()));

        let resolved = resolve_stream_dictionary(&mut pdf, &dictionary)
            .expect("unsupported filters must not resolve DecodeParms");
        assert!(
            !resolved.is_filterable(),
            "an unsupported filter anywhere in the chain must stop filtering before DecodeParms"
        );
        assert_eq!(
            resolved.get(b"DecodeParms"),
            Some(&Object::Dictionary(params)),
            "the diagnostic reference must remain untouched behind an unsupported filter chain"
        );
    }

    #[test]
    fn early_change_is_resolved_for_lzw_but_not_flate() {
        for (filter, expected_early_change) in [
            (
                b"FlateDecode".as_slice(),
                Object::Reference(ObjectRef::new(20, 0)),
            ),
            (b"LZWDecode".as_slice(), Object::Integer(0)),
        ] {
            let mut pdf = handle_pdf(b"");
            pdf.set_object(ObjectRef::new(20, 0), Object::Integer(0));
            let mut params = Dictionary::new();
            params.insert(b"EarlyChange", Object::Reference(ObjectRef::new(20, 0)));
            let mut dictionary = Dictionary::new();
            dictionary.insert(b"Filter", Object::Name(filter.to_vec()));
            dictionary.insert(b"DecodeParms", Object::Dictionary(params));

            let resolved = resolve_stream_dictionary(&mut pdf, &dictionary)
                .expect("resolve stream dictionary");
            let params = resolved
                .get(b"DecodeParms")
                .and_then(Object::as_dict)
                .expect("resolved DecodeParms dictionary");
            assert_eq!(params.get(b"EarlyChange"), Some(&expected_early_change));
        }
    }

    #[test]
    fn filter_aliases_resolve_their_consumed_decode_parameters() {
        for (filter, key, expected) in [
            (
                b"Fl".as_slice(),
                b"Predictor".as_slice(),
                Object::Integer(15),
            ),
            (
                b"LZW".as_slice(),
                b"EarlyChange".as_slice(),
                Object::Integer(0),
            ),
        ] {
            let mut pdf = handle_pdf(b"");
            pdf.set_object(ObjectRef::new(20, 0), Object::Integer(0));
            let mut params = Dictionary::new();
            let value = if key == b"Predictor" {
                Object::Reference(ObjectRef::new(13, 0))
            } else {
                Object::Reference(ObjectRef::new(20, 0))
            };
            params.insert(key, value);
            let mut dictionary = Dictionary::new();
            dictionary.insert(b"Filter", Object::Name(filter.to_vec()));
            dictionary.insert(b"DecodeParms", Object::Dictionary(params));

            let resolved = resolve_stream_dictionary(&mut pdf, &dictionary)
                .expect("resolve stream dictionary");
            let params = resolved
                .get(b"DecodeParms")
                .and_then(Object::as_dict)
                .expect("resolved DecodeParms dictionary");
            assert_eq!(params.get(key), Some(&expected));
        }
    }

    #[test]
    fn qpdf_factory_aliases_keep_supported_non_predictor_parameters_shallow() {
        for alias in [b"A85".as_slice(), b"RL".as_slice(), b"DCT".as_slice()] {
            let mut pdf = handle_pdf(b"");
            let mut params = Dictionary::new();
            params.insert(b"Predictor", Object::Reference(ObjectRef::new(13, 0)));
            let mut dictionary = Dictionary::new();
            dictionary.insert(b"Filter", Object::Name(alias.to_vec()));
            dictionary.insert(b"DecodeParms", Object::Dictionary(params));

            let resolved = resolve_stream_dictionary(&mut pdf, &dictionary)
                .expect("qpdf-supported alias must not reject DecodeParms");
            assert!(
                resolved.is_filterable(),
                "{alias:?} must reach qpdf's supported-filter path"
            );
            let params = resolved
                .get(b"DecodeParms")
                .and_then(Object::as_dict)
                .expect("direct DecodeParms dictionary is retained");
            assert_eq!(
                params.get(b"Predictor"),
                Some(&Object::Reference(ObjectRef::new(13, 0))),
                "{alias:?} must not consume Flate/LZW-only predictor keys"
            );
        }
    }

    #[test]
    fn ccf_alias_marks_the_chain_unfilterable_without_resolving_decode_parms() {
        let mut pdf = handle_pdf(b"");
        for index in 0..65 {
            let reference = ObjectRef::new(100 + index, 0);
            let value = if index == 64 {
                Object::Null
            } else {
                Object::Reference(ObjectRef::new(101 + index, 0))
            };
            pdf.set_object(reference, value);
        }
        let mut dictionary = Dictionary::new();
        dictionary.insert(b"Filter", Object::Name(b"CCF".to_vec()));
        dictionary.insert(b"DecodeParms", Object::Reference(ObjectRef::new(100, 0)));

        let resolved = resolve_stream_dictionary(&mut pdf, &dictionary)
            .expect("unfilterable CCF must not follow DecodeParms");
        assert!(
            !resolved.is_filterable(),
            "qpdf's factory table has no CCITTFaxDecode decoder"
        );
        assert_eq!(
            resolved.get(b"DecodeParms"),
            Some(&Object::Reference(ObjectRef::new(100, 0))),
            "the unsupported CCF chain must leave DecodeParms untouched"
        );
    }

    #[test]
    fn matching_filter_arrays_resolve_only_their_paired_parameters() {
        let mut pdf = handle_pdf(b"");
        pdf.set_object(ObjectRef::new(20, 0), Object::Integer(0));
        let mut flate_params = Dictionary::new();
        flate_params.insert(b"Predictor", Object::Reference(ObjectRef::new(13, 0)));
        flate_params.insert(b"EarlyChange", Object::Reference(ObjectRef::new(20, 0)));
        let mut lzw_params = Dictionary::new();
        lzw_params.insert(b"Predictor", Object::Reference(ObjectRef::new(14, 0)));
        lzw_params.insert(b"EarlyChange", Object::Reference(ObjectRef::new(20, 0)));
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            b"Filter",
            Object::Array(vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"LZWDecode".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ]),
        );
        dictionary.insert(
            b"DecodeParms",
            Object::Array(vec![
                Object::Dictionary(flate_params),
                Object::Dictionary(lzw_params),
                Object::Null,
            ]),
        );

        let resolved =
            resolve_stream_dictionary(&mut pdf, &dictionary).expect("resolve stream dictionary");
        let params = resolved
            .get(b"DecodeParms")
            .and_then(Object::as_array)
            .expect("resolved DecodeParms array");
        let flate_params = params[0].as_dict().expect("Flate parameters");
        assert_eq!(flate_params.get(b"Predictor"), Some(&Object::Integer(15)));
        assert_eq!(
            flate_params.get(b"EarlyChange"),
            Some(&Object::Reference(ObjectRef::new(20, 0)))
        );
        let lzw_params = params[1].as_dict().expect("LZW parameters");
        assert_eq!(lzw_params.get(b"Predictor"), Some(&Object::Integer(3)));
        assert_eq!(lzw_params.get(b"EarlyChange"), Some(&Object::Integer(0)));
        assert_eq!(params[2], Object::Null);
    }

    #[test]
    fn malformed_filter_structure_leaves_decode_parameters_unresolved() {
        let mut pdf = handle_pdf(b"");
        let mut filter = Dictionary::new();
        filter.insert(b"Type", Object::Name(b"FlateDecode".to_vec()));
        let mut params = Dictionary::new();
        params.insert(b"Predictor", Object::Reference(ObjectRef::new(13, 0)));
        let mut dictionary = Dictionary::new();
        dictionary.insert(b"Filter", Object::Dictionary(filter));
        dictionary.insert(b"DecodeParms", Object::Dictionary(params));

        let resolved =
            resolve_stream_dictionary(&mut pdf, &dictionary).expect("resolve stream dictionary");
        let params = resolved
            .get(b"DecodeParms")
            .and_then(Object::as_dict)
            .expect("preserved DecodeParms dictionary");
        assert_eq!(
            params.get(b"Predictor"),
            Some(&Object::Reference(ObjectRef::new(13, 0)))
        );
    }

    #[test]
    fn null_filter_resolves_parameter_container_but_not_its_values() {
        let mut pdf = handle_pdf(b"");
        let mut dictionary = Dictionary::new();
        dictionary.insert(b"Filter", Object::Null);
        dictionary.insert(b"DecodeParms", Object::Reference(ObjectRef::new(12, 0)));

        let resolved =
            resolve_stream_dictionary(&mut pdf, &dictionary).expect("resolve stream dictionary");
        let params = resolved
            .get(b"DecodeParms")
            .and_then(Object::as_dict)
            .expect("resolved DecodeParms container");
        assert_eq!(
            params.get(b"Predictor"),
            Some(&Object::Reference(ObjectRef::new(13, 0)))
        );
        assert_eq!(
            params.get(b"Columns"),
            Some(&Object::Reference(ObjectRef::new(14, 0)))
        );
    }

    #[test]
    fn nested_filter_array_stays_opaque_and_leaves_decode_parameters_unresolved() {
        let mut pdf = handle_pdf(b"");
        let filter = (0..64).fold(Object::Name(b"FlateDecode".to_vec()), |value, _| {
            Object::Array(vec![value])
        });
        let mut params = Dictionary::new();
        params.insert(b"Predictor", Object::Reference(ObjectRef::new(13, 0)));
        let mut dictionary = Dictionary::new();
        dictionary.insert(b"Filter", filter.clone());
        dictionary.insert(b"DecodeParms", Object::Dictionary(params));

        let resolved =
            resolve_stream_dictionary(&mut pdf, &dictionary).expect("resolve shallow Filter");
        assert_eq!(resolved.get(b"Filter"), Some(&filter));
        let params = resolved
            .get(b"DecodeParms")
            .and_then(Object::as_dict)
            .expect("preserved DecodeParms dictionary");
        assert_eq!(
            params.get(b"Predictor"),
            Some(&Object::Reference(ObjectRef::new(13, 0)))
        );
    }

    #[test]
    fn shared_decode_parameters_use_the_union_of_valid_filter_keys() {
        let mut pdf = handle_pdf(b"");
        pdf.set_object(ObjectRef::new(20, 0), Object::Integer(0));
        let mut params = Dictionary::new();
        params.insert(b"Predictor", Object::Reference(ObjectRef::new(13, 0)));
        params.insert(b"EarlyChange", Object::Reference(ObjectRef::new(20, 0)));
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            b"Filter",
            Object::Array(vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"LZWDecode".to_vec()),
            ]),
        );
        dictionary.insert(b"DecodeParms", Object::Dictionary(params));

        let resolved =
            resolve_stream_dictionary(&mut pdf, &dictionary).expect("resolve stream dictionary");
        let params = resolved
            .get(b"DecodeParms")
            .and_then(Object::as_dict)
            .expect("resolved shared DecodeParms dictionary");
        assert_eq!(params.get(b"Predictor"), Some(&Object::Integer(15)));
        assert_eq!(params.get(b"EarlyChange"), Some(&Object::Integer(0)));
    }

    #[test]
    fn crypt_decode_params_validate_only_qpdfs_visible_keys_and_type() {
        let cases = [
            (
                vec![
                    (
                        b"Type".to_vec(),
                        Object::Name(b"CryptFilterDecodeParms".to_vec()),
                    ),
                    (b"Name".to_vec(), Object::Integer(42)),
                ],
                true,
            ),
            (
                vec![(b"Type".to_vec(), Object::Name(b"Wrong".to_vec()))],
                false,
            ),
            (vec![(b"Extra".to_vec(), Object::Integer(1))], false),
            (vec![(b"Extra".to_vec(), Object::Null)], true),
        ];

        for (entries, expected_filterable) in cases {
            let mut pdf = handle_pdf(b"");
            let mut params = Dictionary::new();
            for (key, value) in entries {
                params.insert(key, value);
            }
            let mut dictionary = Dictionary::new();
            dictionary.insert(b"Filter", Object::Name(b"Crypt".to_vec()));
            dictionary.insert(b"DecodeParms", Object::Dictionary(params));

            let resolved = resolve_stream_dictionary(&mut pdf, &dictionary)
                .expect("resolve Crypt DecodeParms");

            assert_eq!(resolved.is_filterable(), expected_filterable);
            assert_eq!(resolved.get(b"Filter"), Some(&Object::Null));
        }
    }

    #[test]
    fn crypt_removal_keeps_filter_and_decode_param_array_alignment() {
        let mut pdf = handle_pdf(b"");
        let mut crypt = Dictionary::new();
        crypt.insert(b"Type", Object::Name(b"CryptFilterDecodeParms".to_vec()));
        let mut flate = Dictionary::new();
        flate.insert(b"Predictor", Object::Integer(1));
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            b"Filter",
            Object::Array(vec![
                Object::Name(b"Crypt".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"Crypt".to_vec()),
            ]),
        );
        dictionary.insert(
            b"DecodeParms",
            Object::Array(vec![
                Object::Dictionary(crypt),
                Object::Dictionary(flate.clone()),
                Object::Null,
            ]),
        );

        let resolved =
            resolve_stream_dictionary(&mut pdf, &dictionary).expect("resolve Crypt chain");

        assert!(resolved.is_filterable());
        assert_eq!(
            resolved.get(b"Filter"),
            Some(&Object::Array(vec![Object::Name(b"FlateDecode".to_vec())]))
        );
        assert_eq!(
            resolved.get(b"DecodeParms"),
            Some(&Object::Array(vec![Object::Dictionary(flate)]))
        );
    }

    #[test]
    fn crypt_removal_does_not_hide_a_decode_param_length_mismatch() {
        let mut pdf = handle_pdf(b"");
        let filters = Object::Array(vec![
            Object::Name(b"Crypt".to_vec()),
            Object::Name(b"FlateDecode".to_vec()),
        ]);
        let decode_params = Object::Array(vec![Object::Null]);
        let mut dictionary = Dictionary::new();
        dictionary.insert(b"Filter", filters.clone());
        dictionary.insert(b"DecodeParms", decode_params.clone());

        let resolved =
            resolve_stream_dictionary(&mut pdf, &dictionary).expect("resolve mismatched chain");

        assert!(resolved.is_filterable());
        assert_eq!(resolved.get(b"Filter"), Some(&filters));
        assert_eq!(resolved.get(b"DecodeParms"), Some(&decode_params));
    }

    #[test]
    fn nondictionary_decode_param_warnings_keep_filter_indices_and_types() {
        let mut pdf = handle_pdf(b"");
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            b"Filter",
            Object::Array(vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"ASCIIHexDecode".to_vec()),
                Object::Name(b"LZWDecode".to_vec()),
            ]),
        );
        dictionary.insert(b"DecodeParms", Object::Integer(42));

        let resolved = resolve_stream_dictionary(&mut pdf, &dictionary)
            .expect("resolve shared non-dictionary DecodeParms");

        assert_eq!(
            resolved.decode_param_type_warnings(),
            [
                super::DecodeParamTypeWarning {
                    filter_index: 0,
                    object_type: "integer",
                    source: super::DecodeParmsWarningSource::StreamDictionary,
                },
                super::DecodeParamTypeWarning {
                    filter_index: 2,
                    object_type: "integer",
                    source: super::DecodeParmsWarningSource::StreamDictionary,
                },
            ]
        );
    }

    #[test]
    fn decode_param_warning_attributes_indirect_scalar_to_its_own_object() {
        let mut pdf = handle_pdf(b"");
        let mut dictionary = Dictionary::new();
        dictionary.insert(b"Filter", Object::Name(b"FlateDecode".to_vec()));
        dictionary.insert(b"DecodeParms", Object::Reference(ObjectRef::new(13, 0)));

        let resolved = resolve_stream_dictionary(&mut pdf, &dictionary)
            .expect("resolve indirect non-dictionary DecodeParms");

        assert_eq!(
            resolved.decode_param_type_warnings(),
            [super::DecodeParamTypeWarning {
                filter_index: 0,
                object_type: "integer",
                source: super::DecodeParmsWarningSource::ObjectBody(ObjectRef::new(13, 0)),
            }]
        );
    }

    #[test]
    fn decode_param_warning_array_items_attribute_indirect_items_to_themselves() {
        let mut pdf = handle_pdf(b"");
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            b"Filter",
            Object::Array(vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"LZWDecode".to_vec()),
            ]),
        );
        dictionary.insert(
            b"DecodeParms",
            Object::Array(vec![
                Object::Reference(ObjectRef::new(13, 0)),
                Object::Integer(7),
            ]),
        );

        let resolved = resolve_stream_dictionary(&mut pdf, &dictionary)
            .expect("resolve array DecodeParms with an indirect item");

        assert_eq!(
            resolved.decode_param_type_warnings(),
            [
                super::DecodeParamTypeWarning {
                    filter_index: 0,
                    object_type: "integer",
                    source: super::DecodeParmsWarningSource::ObjectBody(ObjectRef::new(13, 0)),
                },
                super::DecodeParamTypeWarning {
                    filter_index: 1,
                    object_type: "integer",
                    source: super::DecodeParmsWarningSource::StreamDictionary,
                },
            ]
        );
    }

    #[test]
    fn decode_param_warning_direct_array_item_attributes_to_the_containers_own_array_index() {
        // qpdf attributes the warning to the array's own indirect object,
        // at this item's own array index, when an item embedded directly
        // in it (not itself a reference) is the offending value.
        let bytes = pdf_with_objects(&[(1, b"<< /Type /Catalog >>"), (5, b"[ 9 9 ]")], b"");
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open indirect-array fixture");
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            b"Filter",
            Object::Array(vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"LZWDecode".to_vec()),
            ]),
        );
        dictionary.insert(b"DecodeParms", Object::Reference(ObjectRef::new(5, 0)));

        let resolved = resolve_stream_dictionary(&mut pdf, &dictionary)
            .expect("resolve indirect array with direct items");

        assert_eq!(
            resolved.decode_param_type_warnings(),
            [
                super::DecodeParamTypeWarning {
                    filter_index: 0,
                    object_type: "integer",
                    source: super::DecodeParmsWarningSource::ArrayItem(ObjectRef::new(5, 0), 0),
                },
                super::DecodeParamTypeWarning {
                    filter_index: 1,
                    object_type: "integer",
                    source: super::DecodeParmsWarningSource::ArrayItem(ObjectRef::new(5, 0), 1),
                },
            ]
        );
    }
}

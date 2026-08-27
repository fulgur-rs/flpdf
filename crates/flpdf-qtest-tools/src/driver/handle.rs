use std::io::{Read, Seek};

use flpdf::{Error, ObjectHandle, ObjectRef, Pdf};

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
    /// The value sits directly in the stream dictionary itself (no reference
    /// was followed at or above this filter position).
    StreamDictionary,
    /// The value is the entire body of this indirect object.
    ObjectBody(ObjectRef),
    /// The value is a direct item at this array index inside an indirect
    /// object's own array body.
    ArrayItem(ObjectRef, usize),
}

/// The resolved stream-dictionary view consumed by qtest's filter diagnostics
/// and decode path. The dictionary remains a live handle graph; no owned raw
/// object snapshot is retained.
pub(crate) struct ResolvedStreamDictionary {
    dictionary: ObjectHandle,
    filterable: bool,
    decode_param_type_warnings: Vec<DecodeParamTypeWarning>,
}

impl ResolvedStreamDictionary {
    pub(crate) fn is_filterable(&self) -> bool {
        self.filterable
    }

    pub(crate) fn decode_param_type_warnings(&self) -> &[DecodeParamTypeWarning] {
        &self.decode_param_type_warnings
    }

    pub(crate) fn filter_input_handle(&self) -> ObjectHandle {
        self.dictionary.clone()
    }
}

/// Resolve one canonical handle through the qpdf object cache, retaining its
/// own indirect identity for qtest warning attribution.
pub(crate) fn resolve_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: &ObjectHandle,
) -> flpdf::Result<(ObjectHandle, Option<ObjectRef>, Option<ObjectRef>)> {
    let object_ref = value.object_ref();
    pdf.resolve(value)?;
    Ok((value.clone(), object_ref, object_ref))
}

/// Write a resolved handle using qpdf's dictionary null-visibility and child
/// indirect-identity rules. This is the qtest driver's diagnostic serializer,
/// not a second PDF writer.
pub(crate) fn write_qpdf_object_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: &ObjectHandle,
) -> flpdf::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    write_qpdf_handle_into(pdf, value, false, &mut bytes)?;
    Ok(bytes)
}

fn write_qpdf_handle_into<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: &ObjectHandle,
    preserve_indirect: bool,
    bytes: &mut Vec<u8>,
) -> flpdf::Result<()> {
    if preserve_indirect && value.object_ref().is_some() {
        pdf.resolve(value)?;
        bytes.extend_from_slice(&value.unparse());
        return Ok(());
    }

    pdf.resolve(value)?;
    let value = value.clone();
    if let Some(items) = value.as_array() {
        bytes.extend_from_slice(b"[ ");
        for item in items {
            write_qpdf_handle_into(pdf, &item, true, bytes)?;
            bytes.push(b' ');
        }
        bytes.push(b']');
        return Ok(());
    }
    if let Some(entries) = value.as_dictionary() {
        bytes.extend_from_slice(b"<< ");
        for (key, child) in entries {
            pdf.resolve(&child)?;
            if child.is_null() {
                continue;
            }
            let name = key.strip_prefix(b"/").unwrap_or(&key);
            bytes.extend_from_slice(&ObjectHandle::name(name.to_vec()).unparse_resolved());
            bytes.push(b' ');
            write_qpdf_handle_into(pdf, &child, true, bytes)?;
            bytes.push(b' ');
        }
        bytes.extend_from_slice(b">>");
        return Ok(());
    }
    if let Some(stream_dict) = value.as_stream_dict() {
        return write_qpdf_handle_into(pdf, &stream_dict, false, bytes);
    }
    bytes.extend_from_slice(&value.unparse_resolved());
    Ok(())
}

pub(crate) fn resolve_stream_dictionary_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    source: &ObjectHandle,
) -> flpdf::Result<ResolvedStreamDictionary> {
    pdf.resolve(source)?;
    let source = source.clone();
    let filter_value = source.get_key(b"/Filter");
    let filter = if filter_value.is_null() {
        None
    } else {
        Some(resolve_filter_structure_handle(pdf, &filter_value)?)
    };
    let filter_names = filter.as_ref().and_then(resolved_filter_names_handle);
    let mut filterable = filter_names
        .as_deref()
        .is_none_or(|names| names.iter().all(|name| qpdf_filter_factory_exists(name)));

    let decode_params_value = source.get_key(b"/DecodeParms");
    let mut decode_parms_sources = Vec::new();
    let resolved_decode_params = if filterable {
        match filter_names.as_deref() {
            Some(names) => {
                let (value, sources) =
                    resolve_decode_params_handle(pdf, names, &decode_params_value)?;
                decode_parms_sources = sources;
                value
            }
            None => decode_params_value.clone(),
        }
    } else {
        decode_params_value.clone()
    };

    let mut entries = source
        .as_dictionary()
        .ok_or_else(|| Error::System("stream dictionary access on non-dictionary object".into()))?;
    if let Some(filter) = filter {
        entries.insert(b"/Filter".to_vec(), filter);
    }
    if !decode_params_value.is_null() || !resolved_decode_params.is_null() {
        entries.insert(b"/DecodeParms".to_vec(), resolved_decode_params);
    }
    let resolved = ObjectHandle::dictionary(entries.into_iter().collect());

    let mut warnings = Vec::new();
    if filterable {
        if let Some(names) = filter_names.as_deref() {
            let params =
                aligned_decode_params_handle(pdf, &resolved.get_key(b"/DecodeParms"), names)?;
            for (filter_index, (filter_name, params)) in names.iter().zip(params).enumerate() {
                let normalized = normalized_filter_name(filter_name);
                if params.as_ref().is_some_and(|params| !params.is_null())
                    && params
                        .as_ref()
                        .is_some_and(|params| params.as_dictionary().is_none())
                    && matches!(normalized, b"Crypt" | b"FlateDecode" | b"LZWDecode")
                {
                    let params = params.as_ref().expect("non-null DecodeParms");
                    warnings.push(DecodeParamTypeWarning {
                        filter_index,
                        object_type: params.type_name().unwrap_or("unresolved"),
                        source: decode_parms_sources
                            .get(filter_index)
                            .copied()
                            .unwrap_or(DecodeParmsWarningSource::StreamDictionary),
                    });
                }
                if normalized == b"Crypt"
                    && !crypt_decode_params_filterable_handle(pdf, params.as_ref())?
                {
                    filterable = false;
                }
            }
            remove_identity_crypt_stages_handle(&resolved, names)?;
        }
    }
    Ok(ResolvedStreamDictionary {
        dictionary: resolved,
        filterable,
        decode_param_type_warnings: warnings,
    })
}

fn resolved_filter_names_handle(filter: &ObjectHandle) -> Option<Vec<Vec<u8>>> {
    if filter.is_null() {
        return Some(Vec::new());
    }
    if let Some(name) = filter.as_name() {
        return Some(vec![name]);
    }
    filter
        .as_array()?
        .into_iter()
        .map(|value| value.as_name())
        .collect()
}

fn resolve_filter_structure_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: &ObjectHandle,
) -> flpdf::Result<ObjectHandle> {
    pdf.resolve(value)?;
    let value = value.clone();
    if let Some(values) = value.as_array() {
        let values = values
            .into_iter()
            .map(|value| {
                pdf.resolve(&value)?;
                Ok(value)
            })
            .collect::<flpdf::Result<Vec<_>>>()?;
        return Ok(ObjectHandle::array(values));
    }
    Ok(value)
}

fn aligned_decode_params_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    decode_params: &ObjectHandle,
    filters: &[Vec<u8>],
) -> flpdf::Result<Vec<Option<ObjectHandle>>> {
    pdf.resolve(decode_params)?;
    let decode_params = decode_params.clone();
    if decode_params.is_null() {
        return Ok(vec![None; filters.len()]);
    }
    let Some(values) = decode_params.as_array() else {
        return Ok(vec![Some(decode_params); filters.len()]);
    };
    if values.is_empty() {
        return Ok(vec![None; filters.len()]);
    }
    if values.len() != filters.len() {
        return Ok(Vec::new());
    }
    values
        .into_iter()
        .map(|value| {
            pdf.resolve(&value)?;
            Ok((!value.is_null()).then_some(value))
        })
        .collect()
}

fn crypt_decode_params_filterable_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    decode_params: Option<&ObjectHandle>,
) -> flpdf::Result<bool> {
    let Some(decode_params) = decode_params else {
        return Ok(true);
    };
    pdf.resolve(decode_params)?;
    let decode_params = decode_params.clone();
    if decode_params.is_null() {
        return Ok(true);
    }
    let Some(entries) = decode_params.as_dictionary() else {
        return Ok(true);
    };
    let mut visible = Vec::new();
    for (key, value) in entries {
        pdf.resolve(&value)?;
        if !value.is_null() {
            visible.push((key, value));
        }
    }
    let type_is_valid = visible
        .iter()
        .find(|(key, _)| key.as_slice() == b"/Type")
        .is_none_or(|(_, value)| value.as_name().as_deref() == Some(b"CryptFilterDecodeParms"));
    Ok(type_is_valid
        && visible
            .iter()
            .all(|(key, _)| matches!(key.as_slice(), b"/Type" | b"/Name")))
}

fn resolve_decode_param_dict_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filters: &[Vec<u8>],
    entries: std::collections::BTreeMap<Vec<u8>, ObjectHandle>,
) -> flpdf::Result<ObjectHandle> {
    let mut resolved = Vec::new();
    for (key, value) in entries {
        let consumed = filters.iter().any(|filter| {
            filter_consumes_decode_key(filter, key.strip_prefix(b"/").unwrap_or(&key))
        });
        let value = if consumed {
            pdf.resolve(&value)?;
            value
        } else {
            value
        };
        if !consumed || !value.is_null() {
            resolved.push((key, value));
        }
    }
    Ok(ObjectHandle::dictionary(resolved))
}

fn resolve_decode_param_for_filters_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filters: &[Vec<u8>],
    value: &ObjectHandle,
) -> flpdf::Result<(ObjectHandle, Option<ObjectRef>)> {
    let (value, _, terminal_ref) = resolve_handle(pdf, value)?;
    let value = if let Some(entries) = value.as_dictionary() {
        resolve_decode_param_dict_handle(pdf, filters, entries)?
    } else {
        value
    };
    Ok((value, terminal_ref))
}

fn resolve_decode_params_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filters: &[Vec<u8>],
    value: &ObjectHandle,
) -> flpdf::Result<(ObjectHandle, Vec<DecodeParmsWarningSource>)> {
    let (value, _, container_ref) = resolve_handle(pdf, value)?;
    let Some(values) = value.as_array() else {
        let (value, _) = resolve_decode_param_for_filters_handle(pdf, filters, &value)?;
        let source = container_ref
            .map(DecodeParmsWarningSource::ObjectBody)
            .unwrap_or(DecodeParmsWarningSource::StreamDictionary);
        return Ok((value, vec![source; filters.len()]));
    };
    if values.len() != filters.len() {
        return Ok((
            value,
            vec![DecodeParmsWarningSource::StreamDictionary; values.len()],
        ));
    }
    let mut resolved_values = Vec::with_capacity(values.len());
    let mut sources = Vec::with_capacity(values.len());
    for (index, (value, filter)) in values.into_iter().zip(filters.iter()).enumerate() {
        let (value, item_ref) =
            resolve_decode_param_for_filters_handle(pdf, std::slice::from_ref(filter), &value)?;
        resolved_values.push(value);
        sources.push(match (item_ref, container_ref) {
            (Some(item_ref), _) => DecodeParmsWarningSource::ObjectBody(item_ref),
            (None, Some(container_ref)) => {
                DecodeParmsWarningSource::ArrayItem(container_ref, index)
            }
            (None, None) => DecodeParmsWarningSource::StreamDictionary,
        });
    }
    Ok((ObjectHandle::array(resolved_values), sources))
}

fn remove_identity_crypt_stages_handle(
    dictionary: &ObjectHandle,
    filters: &[Vec<u8>],
) -> flpdf::Result<()> {
    let crypt_indices: Vec<usize> = filters
        .iter()
        .enumerate()
        .filter_map(|(index, filter)| (normalized_filter_name(filter) == b"Crypt").then_some(index))
        .collect();
    if crypt_indices.is_empty() {
        return Ok(());
    }
    let filter = dictionary.get_key(b"/Filter");
    if filter
        .as_name()
        .is_some_and(|name| normalized_filter_name(&name) == b"Crypt")
    {
        dictionary.remove_key(b"/Filter");
    } else if let Some(values) = filter.as_array() {
        dictionary.replace_key(
            b"/Filter",
            ObjectHandle::array(
                values
                    .into_iter()
                    .enumerate()
                    .filter_map(|(index, value)| (!crypt_indices.contains(&index)).then_some(value))
                    .collect(),
            ),
        )?;
    }
    let decode_params = dictionary.get_key(b"/DecodeParms");
    if let Some(values) = decode_params.as_array() {
        let values = values
            .into_iter()
            .enumerate()
            .filter_map(|(index, value)| (!crypt_indices.contains(&index)).then_some(value))
            .collect();
        dictionary.replace_key(b"/DecodeParms", ObjectHandle::array(values))?;
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::rc::Rc;

    fn pdf_with_objects(objects: &[(u32, &[u8])]) -> Vec<u8> {
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
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
                max_object + 1
            )
            .as_bytes(),
        );
        bytes
    }

    fn handle_pdf() -> Pdf<Cursor<Vec<u8>>> {
        let bytes = pdf_with_objects(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Count 0 /Kids [ ] >>"),
            (5, b"[ 9 9 ]"),
            (6, b"7 0 R"),
            (7, b"true"),
            (8, b"null"),
            (9, b"<< /Length 3 >>\nstream\nabc\nendstream"),
            (10, b"11 0 R"),
            (11, b"/FlateDecode"),
            (12, b"<< /Predictor 13 0 R /Columns 14 0 R >>"),
            (13, b"15"),
            (14, b"3"),
        ]);
        Pdf::open_mem_owned(bytes).expect("open handle fixture")
    }

    #[test]
    fn handle_constructors_report_qpdf_object_types() {
        let cases = [
            (ObjectHandle::null(), "null"),
            (ObjectHandle::boolean(false), "boolean"),
            (ObjectHandle::integer(1), "integer"),
            (ObjectHandle::real(1.5), "real"),
            (ObjectHandle::real_literal(0.4, b".4".to_vec()), "real"),
            (ObjectHandle::string(b"s".to_vec()), "string"),
            (ObjectHandle::name(b"N".to_vec()), "name"),
            (ObjectHandle::array(Vec::new()), "array"),
            (ObjectHandle::dictionary(Vec::new()), "dictionary"),
            (
                ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(Vec::new())),
                "stream",
            ),
            (ObjectHandle::operator(b"q".to_vec()), "operator"),
            (ObjectHandle::inline_image(b"abc".to_vec()), "inline-image"),
        ];

        for (handle, expected) in cases {
            assert_eq!(handle.type_name().expect("type name"), expected);
        }
    }

    #[test]
    fn handle_resolution_keeps_canonical_indirect_identity() {
        let mut pdf = handle_pdf();
        let value = pdf.get_object_handle(ObjectRef::new(11, 0));

        let (terminal, first_ref, terminal_ref) =
            resolve_handle(&mut pdf, &value).expect("resolve object handle");

        assert_eq!(first_ref, Some(ObjectRef::new(11, 0)));
        assert_eq!(terminal_ref, Some(ObjectRef::new(11, 0)));
        assert_eq!(terminal.as_name(), Some(b"FlateDecode".to_vec()));
    }

    #[test]
    fn stream_dictionary_resolution_consumes_only_filter_parameters() {
        let mut pdf = handle_pdf();
        let source = ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                pdf.get_object_handle(ObjectRef::new(11, 0)),
            ),
            (
                b"DecodeParms".to_vec(),
                ObjectHandle::array(vec![pdf.get_object_handle(ObjectRef::new(12, 0))]),
            ),
        ]);

        let resolved =
            resolve_stream_dictionary_handle(&mut pdf, &source).expect("resolve stream dictionary");
        assert!(resolved.is_filterable());
        let dictionary = resolved.filter_input_handle();
        assert_eq!(
            dictionary.get_key(b"/Filter").as_name(),
            Some(b"FlateDecode".to_vec())
        );
        let params = dictionary
            .get_key(b"/DecodeParms")
            .as_array()
            .and_then(|items| items.first().cloned())
            .and_then(|item| item.as_dictionary())
            .expect("resolved DecodeParms dictionary");
        assert_eq!(
            params
                .get(b"/Predictor".as_slice())
                .and_then(ObjectHandle::as_integer),
            Some(15)
        );
        assert_eq!(
            params
                .get(b"/Columns".as_slice())
                .and_then(ObjectHandle::as_integer),
            Some(3)
        );
    }

    #[test]
    fn dictionary_serializer_omits_children_resolving_to_null() {
        let mut pdf = handle_pdf();
        let source = ObjectHandle::dictionary(vec![
            (
                b"gone".to_vec(),
                pdf.get_object_handle(ObjectRef::new(8, 0)),
            ),
            (b"keep".to_vec(), ObjectHandle::boolean(true)),
        ]);

        let bytes = write_qpdf_object_handle(&mut pdf, &source).expect("serialize dictionary");

        assert_eq!(bytes, b"<< /keep true >>");
    }

    #[test]
    fn known_null_decode_parameters_are_omitted() {
        let mut pdf = handle_pdf();
        let source = ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
            (
                b"DecodeParms".to_vec(),
                ObjectHandle::dictionary(vec![
                    (b"Predictor".to_vec(), ObjectHandle::null()),
                    (b"Metadata".to_vec(), ObjectHandle::integer(1)),
                ]),
            ),
        ]);

        let resolved =
            resolve_stream_dictionary_handle(&mut pdf, &source).expect("resolve DecodeParms");
        let params = resolved
            .filter_input_handle()
            .get_key(b"/DecodeParms")
            .as_dictionary()
            .expect("resolved DecodeParms");
        assert!(!params.contains_key(b"/Predictor".as_slice()));
        assert_eq!(
            params
                .get(b"/Metadata".as_slice())
                .and_then(ObjectHandle::as_integer),
            Some(1)
        );
    }

    #[test]
    fn crypt_identity_stage_is_removed_and_invalid_params_disable_filtering() {
        let mut pdf = handle_pdf();
        let source = ObjectHandle::dictionary(vec![
            (b"Filter".to_vec(), ObjectHandle::name(b"Crypt".to_vec())),
            (
                b"DecodeParms".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"Type".to_vec(),
                    ObjectHandle::name(b"Wrong".to_vec()),
                )]),
            ),
        ]);

        let resolved =
            resolve_stream_dictionary_handle(&mut pdf, &source).expect("resolve Crypt parameters");

        assert!(!resolved.is_filterable());
        assert!(!resolved.filter_input_handle().has_key(b"/Filter"));
    }

    #[test]
    fn object_serializer_inlines_a_direct_stream_dictionary() {
        let mut pdf = handle_pdf();
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"/Length".to_vec(), ObjectHandle::integer(3))]),
            Rc::new(b"abc".to_vec()),
        );

        assert_eq!(
            write_qpdf_object_handle(&mut pdf, &stream).expect("serialize stream"),
            b"<< /Length 3 >>"
        );
    }

    #[test]
    fn stream_dictionary_without_a_filter_is_unfiltered() {
        let mut pdf = handle_pdf();
        let source = ObjectHandle::dictionary(Vec::new());

        let resolved = resolve_stream_dictionary_handle(&mut pdf, &source)
            .expect("resolve stream dictionary without Filter");

        assert!(resolved.is_filterable());
        assert!(!resolved.filter_input_handle().has_key(b"/Filter"));
    }

    #[test]
    fn an_indirect_filter_resolving_to_null_is_an_empty_filter_chain() {
        let mut pdf = handle_pdf();
        let source = ObjectHandle::dictionary(vec![(
            b"/Filter".to_vec(),
            pdf.get_object_handle(ObjectRef::new(99, 0)),
        )]);

        let resolved = resolve_stream_dictionary_handle(&mut pdf, &source)
            .expect("resolve dangling Filter handle");

        assert!(resolved.is_filterable());
        assert!(resolved.filter_input_handle().get_key(b"/Filter").is_null());
    }

    #[test]
    fn an_empty_decode_parameter_array_aligns_as_absent_parameters() {
        let mut pdf = handle_pdf();
        let source = ObjectHandle::dictionary(vec![
            (
                b"/Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
            (b"/DecodeParms".to_vec(), ObjectHandle::array(Vec::new())),
        ]);

        let resolved = resolve_stream_dictionary_handle(&mut pdf, &source)
            .expect("resolve empty DecodeParms array");

        assert!(resolved.is_filterable());
        assert!(resolved.decode_param_type_warnings().is_empty());
    }

    #[test]
    fn crypt_null_decode_parameters_are_accepted() {
        let mut pdf = handle_pdf();
        let null = ObjectHandle::null();

        assert!(crypt_decode_params_filterable_handle(&mut pdf, Some(&null))
            .expect("inspect null Crypt DecodeParms"));
    }

    #[test]
    fn crypt_scalar_decode_parameters_are_non_dictionary_warnings_but_filterable() {
        let mut pdf = handle_pdf();
        let source = ObjectHandle::dictionary(vec![
            (b"/Filter".to_vec(), ObjectHandle::name(b"Crypt".to_vec())),
            (b"/DecodeParms".to_vec(), ObjectHandle::integer(42)),
        ]);

        let resolved = resolve_stream_dictionary_handle(&mut pdf, &source)
            .expect("resolve scalar Crypt DecodeParms");

        assert!(resolved.is_filterable());
        assert_eq!(
            resolved.decode_param_type_warnings()[0].object_type,
            "integer"
        );
    }

    #[test]
    fn crypt_filter_array_removes_only_the_crypt_stage() {
        let mut pdf = handle_pdf();
        let source = ObjectHandle::dictionary(vec![
            (
                b"/Filter".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::name(b"Crypt".to_vec()),
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                ]),
            ),
            (
                b"/DecodeParms".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::dictionary(Vec::new()),
                    ObjectHandle::null(),
                ]),
            ),
        ]);

        let resolved = resolve_stream_dictionary_handle(&mut pdf, &source)
            .expect("resolve a filter array containing Crypt");
        let filters = resolved
            .filter_input_handle()
            .get_key(b"/Filter")
            .as_array()
            .expect("remaining filter array");

        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].as_name(), Some(b"FlateDecode".to_vec()));

        // The removed Crypt stage's /DecodeParms slot (index 0, the empty
        // dictionary) must be dropped along with it, so the remaining
        // FlateDecode stage still pairs with its own params (the trailing
        // null, at the pre-removal index 1) rather than being shifted onto
        // the wrong slot.
        let decode_parms = resolved
            .filter_input_handle()
            .get_key(b"/DecodeParms")
            .as_array()
            .expect("remaining DecodeParms array");
        assert_eq!(decode_parms.len(), 1);
        assert!(
            decode_parms[0].is_null(),
            "the surviving FlateDecode stage must keep its own (null) params, not the \
             removed Crypt stage's empty dictionary"
        );
    }

    #[test]
    fn non_dictionary_decode_parameter_warnings_keep_indices_and_types() {
        let mut pdf = handle_pdf();
        let source = ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                    ObjectHandle::name(b"LZWDecode".to_vec()),
                ]),
            ),
            (b"DecodeParms".to_vec(), ObjectHandle::integer(42)),
        ]);

        let resolved =
            resolve_stream_dictionary_handle(&mut pdf, &source).expect("resolve DecodeParms");

        assert_eq!(
            resolved.decode_param_type_warnings(),
            [
                DecodeParamTypeWarning {
                    filter_index: 0,
                    object_type: "integer",
                    source: DecodeParmsWarningSource::StreamDictionary,
                },
                DecodeParamTypeWarning {
                    filter_index: 1,
                    object_type: "integer",
                    source: DecodeParmsWarningSource::StreamDictionary,
                },
            ]
        );
    }

    #[test]
    fn direct_items_in_an_indirect_decode_parameter_array_keep_array_provenance() {
        let mut pdf = handle_pdf();
        let source = ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                    ObjectHandle::name(b"LZWDecode".to_vec()),
                ]),
            ),
            (
                b"DecodeParms".to_vec(),
                pdf.get_object_handle(ObjectRef::new(5, 0)),
            ),
        ]);

        let resolved =
            resolve_stream_dictionary_handle(&mut pdf, &source).expect("resolve array parameters");

        assert_eq!(
            resolved.decode_param_type_warnings(),
            [
                DecodeParamTypeWarning {
                    filter_index: 0,
                    object_type: "integer",
                    source: DecodeParmsWarningSource::ArrayItem(ObjectRef::new(5, 0), 0),
                },
                DecodeParamTypeWarning {
                    filter_index: 1,
                    object_type: "integer",
                    source: DecodeParmsWarningSource::ArrayItem(ObjectRef::new(5, 0), 1),
                },
            ]
        );
    }
}

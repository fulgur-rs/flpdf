use std::io::{Read, Seek};

use flpdf::{Dictionary, Error, Object, ObjectRef, Pdf};

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
}

impl ResolvedStreamDictionary {
    pub(crate) fn is_filterable(&self) -> bool {
        self.filterable
    }

    pub(crate) fn decode_param_type_warnings(&self) -> &[DecodeParamTypeWarning] {
        &self.decode_param_type_warnings
    }
}

impl std::ops::Deref for ResolvedStreamDictionary {
    type Target = Dictionary;

    fn deref(&self) -> &Self::Target {
        &self.dictionary
    }
}

pub(crate) struct Handle {
    resolved: Object,
    indirect: Option<ObjectRef>,
    terminal_indirect: Option<ObjectRef>,
}

impl Handle {
    pub(crate) fn from_value<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        value: Object,
    ) -> flpdf::Result<Self> {
        let (resolved, indirect, terminal_indirect) = resolve_chain(pdf, value)?;
        Ok(Self {
            resolved,
            indirect,
            terminal_indirect,
        })
    }

    pub(crate) fn get_key<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        dictionary: &Dictionary,
        key: &[u8],
    ) -> flpdf::Result<Self> {
        Self::from_value(pdf, dictionary.get(key).cloned().unwrap_or(Object::Null))
    }

    pub(crate) fn has_key<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        dictionary: &Dictionary,
        key: &[u8],
    ) -> flpdf::Result<bool> {
        Ok(!Self::get_key(pdf, dictionary, key)?.is_null())
    }

    pub(crate) fn is_indirect(&self) -> bool {
        self.indirect.is_some()
    }

    pub(crate) fn indirect_ref(&self) -> Option<ObjectRef> {
        self.indirect
    }

    pub(crate) fn terminal_indirect_ref(&self) -> Option<ObjectRef> {
        self.terminal_indirect
    }

    pub(crate) fn is_null(&self) -> bool {
        self.resolved.is_null()
    }

    pub(crate) fn as_bool(&self) -> Option<bool> {
        self.resolved.as_bool()
    }

    pub(crate) fn resolved(&self) -> &Object {
        &self.resolved
    }

    pub(crate) fn array_item_indirectness(&self) -> flpdf::Result<Vec<bool>> {
        let values = self
            .resolved
            .as_array()
            .ok_or_else(|| Error::System("array access on non-array object".to_string()))?;
        Ok(values
            .iter()
            .map(|value| matches!(value, Object::Reference(_)))
            .collect())
    }

    pub(crate) fn dictionary_items<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
    ) -> flpdf::Result<Vec<(Vec<u8>, Self)>> {
        let values: Vec<(Vec<u8>, Object)> = self
            .resolved
            .as_dict()
            .ok_or_else(|| Error::System("dictionary access on non-dictionary object".to_string()))?
            .iter()
            .map(|(key, value)| (key.to_vec(), value.clone()))
            .collect();
        let mut items = Vec::new();
        for (key, value) in values {
            let value = Self::from_value(pdf, value)?;
            if !value.is_null() {
                items.push((key, value));
            }
        }
        Ok(items)
    }

    pub(crate) fn type_code(&self) -> u8 {
        self.type_info().0
    }

    pub(crate) fn type_name(&self) -> &'static str {
        self.type_info().1
    }

    fn type_info(&self) -> (u8, &'static str) {
        match self.resolved {
            Object::Null => (2, "null"),
            Object::Boolean(_) => (3, "boolean"),
            Object::Integer(_) => (4, "integer"),
            Object::Real(_) | Object::RealLiteral { .. } => (5, "real"),
            Object::String(_) => (6, "string"),
            Object::Name(_) => (7, "name"),
            Object::Array(_) => (8, "array"),
            Object::Dictionary(_) => (9, "dictionary"),
            Object::Stream(_) => (10, "stream"),
            Object::Operator(_) => (11, "operator"),
            Object::InlineImage(_) => (12, "inline-image"),
            Object::Reference(_) => (13, "unresolved"),
        }
    }

    pub(crate) fn unparse<R: Read + Seek>(&self, pdf: &mut Pdf<R>) -> flpdf::Result<Vec<u8>> {
        match self.indirect {
            Some(reference) => Ok(write_object(&Object::Reference(reference))),
            None => write_qpdf_object(pdf, &self.resolved),
        }
    }

    pub(crate) fn unparse_resolved<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
    ) -> flpdf::Result<Vec<u8>> {
        if matches!(self.resolved, Object::Stream(_)) {
            self.unparse(pdf)
        } else {
            write_qpdf_object(pdf, &self.resolved)
        }
    }
}

fn write_object(object: &Object) -> Vec<u8> {
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
                let value = Handle::from_value(pdf, value.clone())?;
                if value.is_null() {
                    continue;
                }
                Object::Name(key.to_vec()).write_pdf(bytes);
                bytes.push(b' ');
                if let Some(reference) = value.indirect_ref() {
                    Object::Reference(reference).write_pdf(bytes);
                } else {
                    write_qpdf_object_into(pdf, value.resolved(), bytes)?;
                }
                bytes.push(b' ');
            }
            bytes.extend_from_slice(b">>");
        }
        _ => object.write_pdf(bytes),
    }
    Ok(())
}

fn resolve_chain<R: Read + Seek>(
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

    let mut resolved = Dictionary::new();
    for (key, value) in source.iter() {
        let value = if key == b"Filter" {
            filter.clone().unwrap_or_else(|| value.clone())
        } else if key == b"DecodeParms" {
            match filter_names.as_deref() {
                Some(names) if filterable => resolve_decode_params(pdf, names, value.clone())?,
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

fn resolve_decode_param_for_filters<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filters: &[&[u8]],
    value: Object,
) -> flpdf::Result<Object> {
    let value = resolve_chain(pdf, value)?.0;
    match value {
        Object::Dictionary(dictionary) => resolve_decode_param_dict(pdf, filters, dictionary),
        other => Ok(other),
    }
}

fn resolve_decode_params<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filters: &[&[u8]],
    value: Object,
) -> flpdf::Result<Object> {
    let value = resolve_chain(pdf, value)?.0;
    match value {
        Object::Array(values) if values.len() == filters.len() => {
            let values = values
                .into_iter()
                .zip(filters.iter().copied())
                .map(|(value, filter)| resolve_decode_param_for_filters(pdf, &[filter], value))
                .collect::<flpdf::Result<Vec<_>>>()?;
            Ok(Object::Array(values))
        }
        Object::Array(values) => Ok(Object::Array(values)),
        other => resolve_decode_param_for_filters(pdf, filters, other),
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
    use super::{resolve_stream_dictionary, Handle};
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
    fn missing_direct_and_indirect_null_all_have_qpdf_null_semantics() {
        for trailer_extra in [
            b"".as_slice(),
            b" /QTest null".as_slice(),
            b" /QTest 99 0 R".as_slice(),
            b" /QTest 8 0 R".as_slice(),
        ] {
            let mut pdf = handle_pdf(trailer_extra);
            let trailer = pdf.trailer().clone();
            let handle = Handle::get_key(&mut pdf, &trailer, b"QTest").expect("get /QTest handle");
            assert!(!Handle::has_key(&mut pdf, &trailer, b"QTest").expect("has /QTest"));
            assert_eq!(handle.type_code(), 2);
            assert_eq!(handle.type_name(), "null");
        }
    }

    #[test]
    fn reference_chain_resolves_but_unparse_retains_the_first_reference() {
        let mut pdf = handle_pdf(b" /QTest 6 0 R");
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Reference(ObjectRef::new(7, 0)),
        );
        let trailer = pdf.trailer().clone();
        let handle = Handle::get_key(&mut pdf, &trailer, b"QTest").expect("get /QTest");
        assert!(handle.is_indirect());
        assert_eq!(handle.as_bool(), Some(true));
        assert_eq!(handle.unparse(&mut pdf).expect("unparse"), b"6 0 R");
        assert_eq!(
            handle.unparse_resolved(&mut pdf).expect("unparse resolved"),
            b"true"
        );
    }

    #[test]
    fn qpdf_type_codes_and_names_are_explicit() {
        let mut pdf = handle_pdf(b"");
        let cases = [
            (Object::Null, 2, "null"),
            (Object::Boolean(false), 3, "boolean"),
            (Object::Integer(1), 4, "integer"),
            (Object::Real(1.5), 5, "real"),
            (Object::String(b"s".to_vec()), 6, "string"),
            (Object::Name(b"N".to_vec()), 7, "name"),
            (Object::Array(vec![]), 8, "array"),
            (Object::Dictionary(Dictionary::new()), 9, "dictionary"),
            (
                Object::Stream(Stream::new(Dictionary::new(), vec![])),
                10,
                "stream",
            ),
            (Object::Operator(b"q".to_vec()), 11, "operator"),
            (Object::InlineImage(b"abc".to_vec()), 12, "inline-image"),
        ];
        for (object, code, name) in cases {
            let handle = Handle::from_value(&mut pdf, object).expect("build handle");
            assert_eq!(handle.type_code(), code);
            assert_eq!(handle.type_name(), name);
        }
        let unresolved = Handle {
            resolved: Object::Reference(ObjectRef::new(99, 0)),
            indirect: None,
            terminal_indirect: None,
        };
        assert_eq!(unresolved.type_code(), 13);
        assert_eq!(unresolved.type_name(), "unresolved");
    }

    #[test]
    fn array_and_dictionary_items_preserve_child_indirectness() {
        let mut pdf = handle_pdf(b"");
        let array = Handle::from_value(
            &mut pdf,
            Object::Array(vec![
                Object::Integer(1),
                Object::Reference(ObjectRef::new(7, 0)),
            ]),
        )
        .expect("array handle");
        assert_eq!(
            array
                .array_item_indirectness()
                .expect("array item metadata"),
            vec![false, true]
        );

        let mut dictionary = Dictionary::new();
        dictionary.insert(b"a", Object::Reference(ObjectRef::new(7, 0)));
        dictionary.insert(b"b", Object::Boolean(false));
        let dictionary =
            Handle::from_value(&mut pdf, Object::Dictionary(dictionary)).expect("dict handle");
        let items = dictionary
            .dictionary_items(&mut pdf)
            .expect("dictionary items");
        assert_eq!(items[0].0, b"a");
        assert!(items[0].1.is_indirect());
        assert_eq!(items[1].0, b"b");
        assert!(!items[1].1.is_indirect());
    }

    #[test]
    fn stream_unparse_resolved_stays_an_indirect_reference() {
        let mut pdf = handle_pdf(b" /QTest 9 0 R");
        let trailer = pdf.trailer().clone();
        let handle = Handle::get_key(&mut pdf, &trailer, b"QTest").expect("stream handle");
        assert_eq!(handle.unparse(&mut pdf).expect("unparse"), b"9 0 R");
        assert_eq!(
            handle.unparse_resolved(&mut pdf).expect("unparse resolved"),
            b"9 0 R"
        );
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
        let handle = Handle::from_value(&mut accepted, Object::Reference(ObjectRef::new(100, 0)))
            .expect("64-hop reference chain");
        assert_eq!(handle.as_bool(), Some(true));

        let mut rejected = handle_pdf(b"");
        install_chain(&mut rejected, 65);
        let result = Handle::from_value(&mut rejected, Object::Reference(ObjectRef::new(100, 0)));
        assert!(result.is_err(), "65-hop reference chain was accepted");
        let error = result.err().expect("65-hop error");
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
                },
                super::DecodeParamTypeWarning {
                    filter_index: 2,
                    object_type: "integer",
                },
            ]
        );
    }
}

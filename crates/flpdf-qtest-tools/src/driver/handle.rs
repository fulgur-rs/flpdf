use std::io::{Read, Seek};

use flpdf::{Dictionary, Error, Object, ObjectRef, Pdf};

const MAX_REF_CHAIN_DEPTH: usize = 64;

pub(crate) struct Handle {
    resolved: Object,
    indirect: Option<ObjectRef>,
}

impl Handle {
    pub(crate) fn from_value<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        value: Object,
    ) -> flpdf::Result<Self> {
        let (resolved, indirect) = resolve_chain(pdf, value)?;
        Ok(Self { resolved, indirect })
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

    pub(crate) fn is_null(&self) -> bool {
        self.resolved.is_null()
    }

    pub(crate) fn as_bool(&self) -> Option<bool> {
        self.resolved.as_bool()
    }

    pub(crate) fn resolved(&self) -> &Object {
        &self.resolved
    }

    pub(crate) fn array_items<R: Read + Seek>(&self, pdf: &mut Pdf<R>) -> flpdf::Result<Vec<Self>> {
        let values = self
            .resolved
            .as_array()
            .ok_or_else(|| Error::System("array access on non-array object".to_string()))?
            .to_vec();
        values
            .into_iter()
            .map(|value| Self::from_value(pdf, value))
            .collect()
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
        values
            .into_iter()
            .map(|(key, value)| Ok((key, Self::from_value(pdf, value)?)))
            .collect()
    }

    pub(crate) fn type_code(&self) -> u8 {
        match self.resolved {
            Object::Null => 2,
            Object::Boolean(_) => 3,
            Object::Integer(_) => 4,
            Object::Real(_) | Object::RealLiteral { .. } => 5,
            Object::String(_) => 6,
            Object::Name(_) => 7,
            Object::Array(_) => 8,
            Object::Dictionary(_) => 9,
            Object::Stream(_) => 10,
            Object::Operator(_) => 11,
            Object::InlineImage(_) => 12,
            Object::Reference(_) => 13,
        }
    }

    pub(crate) fn type_name(&self) -> &'static str {
        match self.type_code() {
            2 => "null",
            3 => "boolean",
            4 => "integer",
            5 => "real",
            6 => "string",
            7 => "name",
            8 => "array",
            9 => "dictionary",
            10 => "stream",
            11 => "operator",
            12 => "inline-image",
            13 => "unresolved",
            _ => unreachable!("type_code only returns the qpdf object table"),
        }
    }

    pub(crate) fn unparse(&self) -> Vec<u8> {
        match self.indirect {
            Some(reference) => write_object(&Object::Reference(reference)),
            None => write_object(&self.resolved),
        }
    }

    pub(crate) fn unparse_resolved(&self) -> Vec<u8> {
        if matches!(self.resolved, Object::Stream(_)) {
            self.unparse()
        } else {
            write_object(&self.resolved)
        }
    }
}

fn write_object(object: &Object) -> Vec<u8> {
    let mut bytes = Vec::new();
    object.write_pdf(&mut bytes);
    bytes
}

fn resolve_chain<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    mut value: Object,
) -> flpdf::Result<(Object, Option<ObjectRef>)> {
    let mut indirect = None;
    for _ in 0..MAX_REF_CHAIN_DEPTH {
        let Object::Reference(reference) = value else {
            return Ok((value, indirect));
        };
        indirect.get_or_insert(reference);
        value = pdf.resolve_borrowed(reference)?.clone();
    }
    if matches!(value, Object::Reference(_)) {
        Err(Error::parse(
            0,
            format!("object reference chain exceeds {MAX_REF_CHAIN_DEPTH} hops"),
        ))
    } else {
        Ok((value, indirect))
    }
}

pub(crate) fn resolve_stream_dictionary<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    source: &Dictionary,
) -> flpdf::Result<Dictionary> {
    let mut resolved = Dictionary::new();
    for (key, value) in source.iter() {
        let value = if key == b"Filter" || key == b"DecodeParms" {
            resolve_nested(pdf, value.clone(), 0)?
        } else {
            value.clone()
        };
        resolved.insert(key, value);
    }
    Ok(resolved)
}

fn resolve_nested<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: Object,
    depth: usize,
) -> flpdf::Result<Object> {
    if depth >= MAX_REF_CHAIN_DEPTH {
        return Err(Error::parse(
            0,
            format!("stream parameter nesting exceeds {MAX_REF_CHAIN_DEPTH} levels"),
        ));
    }
    let (resolved, _) = resolve_chain(pdf, value)?;
    match resolved {
        Object::Array(values) => {
            let values = values
                .into_iter()
                .map(|value| resolve_nested(pdf, value, depth + 1))
                .collect::<flpdf::Result<Vec<_>>>()?;
            Ok(Object::Array(values))
        }
        Object::Dictionary(dictionary) => {
            let entries: Vec<(Vec<u8>, Object)> = dictionary
                .iter()
                .map(|(key, value)| (key.to_vec(), value.clone()))
                .collect();
            let mut resolved = Dictionary::new();
            for (key, value) in entries {
                resolved.insert(key, resolve_nested(pdf, value, depth + 1)?);
            }
            Ok(Object::Dictionary(resolved))
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
        assert_eq!(handle.unparse(), b"6 0 R");
        assert_eq!(handle.unparse_resolved(), b"true");
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
        let items = array.array_items(&mut pdf).expect("array items");
        assert!(!items[0].is_indirect());
        assert!(items[1].is_indirect());

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
        assert_eq!(handle.unparse(), b"9 0 R");
        assert_eq!(handle.unparse_resolved(), b"9 0 R");
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
}

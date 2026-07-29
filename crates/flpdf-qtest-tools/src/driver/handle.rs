use std::io::{Read, Seek};

use flpdf::{Dictionary, Error, Object, ObjectRef, Pdf};

const MAX_REF_CHAIN_DEPTH: usize = 64;

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
    let (resolved, _, _) = resolve_chain(pdf, value)?;
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
    fn stream_parameter_nesting_rejects_depth_64() {
        let mut pdf = handle_pdf(b"");
        let nested = (0..64).fold(Object::Integer(1), |value, _| Object::Array(vec![value]));
        let mut dictionary = Dictionary::new();
        dictionary.insert(b"DecodeParms", nested);
        let error = resolve_stream_dictionary(&mut pdf, &dictionary)
            .expect_err("64-level stream parameter nesting");
        assert!(error.to_string().contains("exceeds 64 levels"));
    }
}

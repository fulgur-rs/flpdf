use crate::{Dictionary, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

pub(crate) fn reference_is_valid(reference: ObjectRef) -> bool {
    reference.number > 0 && reference.generation < u16::MAX
}

pub(crate) fn reference_is_null<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    reference: ObjectRef,
) -> Result<bool> {
    if !reference_is_valid(reference) {
        return Ok(true);
    }
    let mut current = reference;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current) {
            return Ok(true);
        }
        match pdf.resolve_qpdf_json_object_borrowed(current)? {
            Object::Null => return Ok(true),
            Object::Reference(next) => current = *next,
            _ => return Ok(false),
        }
    }
}

pub(crate) fn value_is_null<R: Read + Seek>(pdf: &mut Pdf<R>, value: &Object) -> Result<bool> {
    match value {
        Object::Null => Ok(true),
        Object::Reference(reference) => reference_is_null(pdf, *reference),
        _ => Ok(false),
    }
}

pub(crate) fn snapshot_entries(dict: &Dictionary, skip_length: bool) -> Vec<(Vec<u8>, Object)> {
    dict.iter()
        .filter(|(key, _)| !(skip_length && *key == b"Length"))
        .map(|(key, value)| (key.to_vec(), value.clone()))
        .collect()
}

pub(crate) fn visible_entries<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    entries: Vec<(Vec<u8>, Object)>,
) -> Result<Vec<(Vec<u8>, Object)>> {
    let mut visible = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        if !value_is_null(pdf, &value)? {
            visible.push((key, value));
        }
    }
    Ok(visible)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn null_fixture_bytes() -> Vec<u8> {
        let bodies: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, b"<< /Type /Page /Parent 2 0 R >>"),
            (4, b"null"),
            (5, b"4 0 R"),
            (6, b"7 0 R"),
            (7, b"6 0 R"),
        ];
        let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = [0usize; 9];
        for (number, body) in bodies {
            offsets[*number as usize] = out.len();
            out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(b"xref\n0 9\n0000000000 65535 f \n");
        for offset in offsets.iter().skip(1) {
            if *offset == 0 {
                out.extend_from_slice(b"0000000000 65535 f \n");
            } else {
                out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
            }
        }
        out.extend_from_slice(b"trailer\n<< /Size 9 /Root 1 0 R >>\n");
        out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
        out
    }

    fn open_null_fixture() -> Pdf<Cursor<Vec<u8>>> {
        let mut pdf =
            Pdf::open(Cursor::new(null_fixture_bytes())).expect("null fixture should parse");
        // qpdf file-object recovery deliberately parses a top-level bare `N G R`
        // as an integer. Seed the cached holder values as references so this
        // fixture exercises the shared reference-chain predicate itself.
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Reference(ObjectRef::new(4, 0)),
        );
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Reference(ObjectRef::new(7, 0)),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Reference(ObjectRef::new(6, 0)),
        );
        pdf
    }

    #[test]
    fn qpdf_null_classifies_direct_missing_free_real_and_holder_values() {
        let mut pdf = open_null_fixture();
        assert!(value_is_null(&mut pdf, &Object::Null).unwrap());
        assert!(reference_is_null(&mut pdf, ObjectRef::new(0, 0)).unwrap());
        assert!(reference_is_null(&mut pdf, ObjectRef::new(99, 0)).unwrap());
        assert!(reference_is_null(&mut pdf, ObjectRef::new(8, 0)).unwrap());
        assert!(reference_is_null(&mut pdf, ObjectRef::new(4, 0)).unwrap());
        assert!(reference_is_null(&mut pdf, ObjectRef::new(5, 0)).unwrap());
        assert!(!reference_is_null(&mut pdf, ObjectRef::new(1, 0)).unwrap());
    }

    #[test]
    fn qpdf_null_terminates_holder_cycles_as_null() {
        let mut pdf = open_null_fixture();
        assert!(reference_is_null(&mut pdf, ObjectRef::new(6, 0)).unwrap());
    }

    #[test]
    fn snapshot_and_visible_entries_preserve_sources_and_filter_nulls() {
        let mut pdf = open_null_fixture();
        let mut dict = Dictionary::new();
        dict.insert("Zulu", Object::Integer(26));
        dict.insert("Length", Object::Integer(5));
        dict.insert("DirectNull", Object::Null);
        dict.insert("RefNull", Object::Reference(ObjectRef::new(4, 0)));
        dict.insert("Visible", Object::Reference(ObjectRef::new(1, 0)));
        let dict_before = dict.clone();
        let refs_before = pdf.live_object_refs();
        let values_before = [
            pdf.resolve_qpdf_json_object(ObjectRef::new(1, 0)).unwrap(),
            pdf.resolve_qpdf_json_object(ObjectRef::new(4, 0)).unwrap(),
            pdf.resolve_qpdf_json_object(ObjectRef::new(5, 0)).unwrap(),
        ];

        let snapshot = snapshot_entries(&dict, false);
        assert_eq!(
            snapshot
                .iter()
                .map(|(key, _)| key.as_slice())
                .collect::<Vec<_>>(),
            [
                b"DirectNull".as_slice(),
                b"Length",
                b"RefNull",
                b"Visible",
                b"Zulu"
            ]
        );
        assert_eq!(dict, dict_before);

        let without_length = snapshot_entries(&dict, true);
        assert_eq!(
            without_length
                .iter()
                .map(|(key, _)| key.as_slice())
                .collect::<Vec<_>>(),
            [b"DirectNull".as_slice(), b"RefNull", b"Visible", b"Zulu"]
        );
        assert_eq!(dict, dict_before);

        let visible = visible_entries(&mut pdf, snapshot).unwrap();
        assert_eq!(
            visible
                .iter()
                .map(|(key, _)| key.as_slice())
                .collect::<Vec<_>>(),
            [b"Length".as_slice(), b"Visible", b"Zulu"]
        );
        assert_eq!(dict, dict_before);
        assert_eq!(pdf.live_object_refs(), refs_before);
        assert_eq!(
            [
                pdf.resolve_qpdf_json_object(ObjectRef::new(1, 0)).unwrap(),
                pdf.resolve_qpdf_json_object(ObjectRef::new(4, 0)).unwrap(),
                pdf.resolve_qpdf_json_object(ObjectRef::new(5, 0)).unwrap(),
            ],
            values_before
        );
    }
}

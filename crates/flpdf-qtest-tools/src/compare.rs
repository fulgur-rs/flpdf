//! Per-object semantic comparison matching qpdf v11.9.0's
//! `compareObjects(label, act, exp)` from `compare-for-test/qpdf-test-compare.cc`.
//!
//! The comparator consumes the canonical `ObjectHandle` graph throughout. It
//! returns `Ok("")` for equal objects, a qpdf-shaped reason for a mismatch,
//! and propagates stream decode failures to the harness.

use std::io::{Read, Seek};
use std::rc::Rc;

use flpdf::{DecodeLevel, ObjectHandle, Pdf};

/// Compare two canonical [`ObjectHandle`]s the way qpdf's
/// `qpdf-test-compare` does.
pub fn compare_objects<A, E>(
    label: &str,
    act: &ObjectHandle,
    exp: &ObjectHandle,
    actual_pdf: &mut Pdf<A>,
    expected_pdf: &mut Pdf<E>,
) -> flpdf::Result<String>
where
    A: Read + Seek,
    E: Read + Seek,
{
    actual_pdf.resolve(act)?;
    let act = act.clone();
    expected_pdf.resolve(exp)?;
    let exp = exp.clone();
    if act.type_code()? != exp.type_code()? {
        return Ok(format!("{label}: different types"));
    }
    if act.as_stream_dict().is_some() {
        return compare_streams(label, &act, &exp, actual_pdf, expected_pdf);
    }

    let mut actual_seen = Vec::new();
    resolve_compare_children(&act, actual_pdf, &mut actual_seen, 0)?;
    let mut expected_seen = Vec::new();
    resolve_compare_children(&exp, expected_pdf, &mut expected_seen, 0)?;
    if act.try_unparse_resolved()? != exp.try_unparse_resolved()? {
        return Ok(format!("{label}: object contents differ"));
    }
    Ok(String::new())
}

fn compare_streams<A, E>(
    label: &str,
    act: &ObjectHandle,
    exp: &ObjectHandle,
    actual_pdf: &mut Pdf<A>,
    expected_pdf: &mut Pdf<E>,
) -> flpdf::Result<String>
where
    A: Read + Seek,
    E: Read + Seek,
{
    let act_dict = act
        .as_stream_dict()
        .ok_or_else(|| flpdf::Error::Internal("actual object lost its stream dictionary".into()))?
        .shallow_copy()?;
    let exp_dict = exp
        .as_stream_dict()
        .ok_or_else(|| flpdf::Error::Internal("expected object lost its stream dictionary".into()))?
        .shallow_copy()?;
    act_dict.remove_key(b"/Length")?;
    exp_dict.remove_key(b"/Length")?;

    // qpdf's dictionary unparse resolves direct children for null visibility,
    // while indirect children remain opaque reference tokens.
    let mut actual_seen = Vec::new();
    resolve_compare_children(&act_dict, actual_pdf, &mut actual_seen, 0)?;
    let mut expected_seen = Vec::new();
    resolve_compare_children(&exp_dict, expected_pdf, &mut expected_seen, 0)?;
    if act_dict.try_unparse_resolved()? != exp_dict.try_unparse_resolved()? {
        return Ok(format!("{label}: stream dictionaries differ"));
    }

    if stream_is_xref(&act_dict)? {
        return Ok(String::new());
    }
    if stream_uses_flatedecode(&act_dict)? {
        let decoded_act = act.get_stream_data(DecodeLevel::Generalized)?;
        let decoded_exp = exp.get_stream_data(DecodeLevel::Generalized)?;
        return Ok(compare_stream_bytes(label, &decoded_act, &decoded_exp));
    }

    let act_data = raw_stream_data(act)?;
    let exp_data = raw_stream_data(exp)?;
    Ok(compare_stream_bytes(
        label,
        act_data.as_ref(),
        exp_data.as_ref(),
    ))
}

fn compare_stream_bytes(label: &str, act_bytes: &[u8], exp_bytes: &[u8]) -> String {
    if act_bytes.len() != exp_bytes.len() {
        return format!("{label}: stream data size differs");
    }
    if act_bytes != exp_bytes {
        return format!("{label}: stream data differs");
    }
    String::new()
}

fn raw_stream_data(handle: &ObjectHandle) -> flpdf::Result<Rc<Vec<u8>>> {
    handle
        .as_stream_data()
        .map_or_else(|| handle.get_raw_stream_data(), Ok)
}

fn stream_is_xref(stream_dict: &ObjectHandle) -> flpdf::Result<bool> {
    let type_handle = stream_dict.try_get_key(b"/Type")?;
    type_handle.try_is_name_and_equals(b"XRef")
}

fn stream_uses_flatedecode(stream_dict: &ObjectHandle) -> flpdf::Result<bool> {
    let filter = stream_dict.try_get_key(b"/Filter")?;
    if filter.try_is_name_and_equals(b"FlateDecode")? {
        return Ok(true);
    }
    if !filter.try_is_array()? {
        return Ok(false);
    }
    for index in 0..filter.try_get_array_n_items()? {
        let index = i64::try_from(index)
            .map_err(|_| flpdf::Error::Internal("filter array exceeds qpdf index range".into()))?;
        if filter
            .try_get_array_item(index)?
            .try_is_name_and_equals(b"FlateDecode")?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn resolve_compare_children<R: Read + Seek>(
    handle: &ObjectHandle,
    pdf: &mut Pdf<R>,
    seen: &mut Vec<ObjectHandle>,
    depth: usize,
) -> flpdf::Result<()> {
    if depth > 500
        || seen
            .iter()
            .any(|ancestor| ancestor.is_same_object_as(handle))
    {
        return Ok(());
    }
    if handle.try_is_array()? {
        seen.push(handle.clone());
        for child in handle.try_get_array_as_vector()? {
            if !child.is_direct() {
                continue;
            }
            pdf.resolve(&child)?;
            let terminal = child.clone();
            if terminal.try_is_dictionary()? || terminal.try_is_array()? {
                resolve_compare_children(&terminal, pdf, seen, depth + 1)?;
            }
        }
        seen.pop();
        return Ok(());
    }

    if !handle.try_is_dictionary()? {
        return Ok(());
    }
    let entries = handle.try_get_dict_as_map()?;
    seen.push(handle.clone());
    for child in entries.into_values() {
        let child_is_direct = child.is_direct();
        pdf.resolve(&child)?;
        let terminal = child.clone();
        if child_is_direct && (terminal.try_is_dictionary()? || terminal.try_is_array()?) {
            resolve_compare_children(&terminal, pdf, seen, depth + 1)?;
        }
    }
    seen.pop();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use flpdf::ObjectRef;
    use std::io::Cursor;

    const MINIMAL_PDF: &[u8] = include_bytes!("../../../tests/fixtures/minimal.pdf");

    fn dummy_pdf() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open_mem_owned(MINIMAL_PDF.to_vec()).expect("open dummy PDF")
    }

    fn zlib(bytes: &[u8], level: Compression) -> Vec<u8> {
        use std::io::Write;
        let mut encoder = ZlibEncoder::new(Vec::new(), level);
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn xref_stream_classification_resolves_the_type_name() {
        let pdf = dummy_pdf();
        let xref_type = pdf
            .make_indirect_from_object_handle(ObjectHandle::name(b"XRef".to_vec()))
            .expect("allocate an indirect /XRef name");
        let xref_dictionary = ObjectHandle::dictionary(vec![(b"/Type".to_vec(), xref_type)]);
        let other_dictionary = ObjectHandle::dictionary(vec![(
            b"/Type".to_vec(),
            ObjectHandle::name(b"Page".to_vec()),
        )]);

        assert!(stream_is_xref(&xref_dictionary).unwrap());
        assert!(!stream_is_xref(&other_dictionary).unwrap());
    }

    #[test]
    fn direct_values_compare_by_qpdf_unparse_shape() {
        let mut actual_pdf = dummy_pdf();
        let mut expected_pdf = dummy_pdf();
        assert_eq!(
            compare_objects(
                "integer",
                &ObjectHandle::integer(42),
                &ObjectHandle::integer(42),
                &mut actual_pdf,
                &mut expected_pdf,
            )
            .unwrap(),
            ""
        );
        assert_eq!(
            compare_objects(
                "different",
                &ObjectHandle::integer(1),
                &ObjectHandle::name(b"n".to_vec()),
                &mut actual_pdf,
                &mut expected_pdf,
            )
            .unwrap(),
            "different: different types"
        );
    }

    #[test]
    fn compare_propagates_qpdf_unparse_errors() {
        let mut actual_pdf = dummy_pdf();
        let mut expected_pdf = dummy_pdf();
        let actual = actual_pdf.new_reserved().expect("actual reserved object");
        let expected = expected_pdf
            .new_reserved()
            .expect("expected reserved object");

        let error = compare_objects(
            "reserved",
            &actual,
            &expected,
            &mut actual_pdf,
            &mut expected_pdf,
        )
        .expect_err("qpdf unparse errors must cross the comparator Result boundary");

        assert!(error
            .to_string()
            .contains("attempting to unparse a reserved object"));
    }

    #[test]
    fn direct_null_dictionary_entries_are_omitted() {
        let actual = ObjectHandle::dictionary(vec![
            (b"/Null".to_vec(), ObjectHandle::null()),
            (b"/Value".to_vec(), ObjectHandle::integer(1)),
        ]);
        let expected =
            ObjectHandle::dictionary(vec![(b"/Value".to_vec(), ObjectHandle::integer(1))]);
        let mut actual_pdf = dummy_pdf();
        let mut expected_pdf = dummy_pdf();
        assert_eq!(
            compare_objects(
                "dictionary",
                &actual,
                &expected,
                &mut actual_pdf,
                &mut expected_pdf,
            )
            .unwrap(),
            ""
        );
    }

    #[test]
    fn indirect_array_children_remain_opaque_during_compare() {
        let mut actual_pdf = dummy_pdf();
        let mut expected_pdf = dummy_pdf();
        let actual_missing = actual_pdf.get_object_handle(ObjectRef::new(99, 0));
        let expected_missing = expected_pdf.get_object_handle(ObjectRef::new(100, 0));
        let actual = ObjectHandle::array(vec![actual_missing.clone(), ObjectHandle::integer(1)]);
        let expected =
            ObjectHandle::array(vec![expected_missing.clone(), ObjectHandle::integer(2)]);

        assert_eq!(
            compare_objects(
                "array",
                &actual,
                &expected,
                &mut actual_pdf,
                &mut expected_pdf,
            )
            .unwrap(),
            "array: object contents differ"
        );
        // unparse_resolved_child checks object_ref() before resolving (see
        // object_handle.rs's own doc for that function), so an array's
        // indirect children are never resolved just to report their own
        // indirectness -- matching qpdf's array unparse, which needs the
        // resolve() call to succeed only to read the element's own
        // object/generation identity, not to serialize it.
        assert!(!actual_missing.is_resolved() && !expected_missing.is_resolved());
        assert_eq!(actual_missing.unparse(), b"99 0 R");
        assert_eq!(expected_missing.unparse(), b"100 0 R");
    }

    #[test]
    fn direct_dictionaries_inside_arrays_resolve_null_visibility() {
        let mut actual_pdf = dummy_pdf();
        let mut expected_pdf = dummy_pdf();
        let actual = ObjectHandle::array(vec![ObjectHandle::dictionary(vec![(
            b"/Null".to_vec(),
            actual_pdf.get_object_handle(ObjectRef::new(99, 0)),
        )])]);
        let expected = ObjectHandle::array(vec![ObjectHandle::dictionary(vec![(
            b"/Null".to_vec(),
            expected_pdf.get_object_handle(ObjectRef::new(100, 0)),
        )])]);

        assert_eq!(
            compare_objects(
                "array-dict",
                &actual,
                &expected,
                &mut actual_pdf,
                &mut expected_pdf,
            )
            .unwrap(),
            ""
        );
    }

    #[test]
    fn identical_streams_ignore_length_and_compare_payload() {
        let actual = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"/Length".to_vec(), ObjectHandle::integer(1))]),
            Rc::new(b"same".to_vec()),
        );
        let expected = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"/Length".to_vec(), ObjectHandle::integer(999))]),
            Rc::new(b"same".to_vec()),
        );
        let mut actual_pdf = dummy_pdf();
        let mut expected_pdf = dummy_pdf();
        assert_eq!(
            compare_objects(
                "stream",
                &actual,
                &expected,
                &mut actual_pdf,
                &mut expected_pdf,
            )
            .unwrap(),
            ""
        );
    }

    #[test]
    fn flate_streams_compare_decoded_payloads() {
        let source = b"same decoded stream payload";
        let actual = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"/Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            )]),
            Rc::new(zlib(source, Compression::none())),
        );
        let expected = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"/Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            )]),
            Rc::new(zlib(source, Compression::best())),
        );
        let mut actual_pdf = dummy_pdf();
        let mut expected_pdf = dummy_pdf();
        assert_eq!(
            compare_objects(
                "flate",
                &actual,
                &expected,
                &mut actual_pdf,
                &mut expected_pdf,
            )
            .unwrap(),
            ""
        );
    }

    #[test]
    fn flate_decode_failure_is_propagated() {
        let make = || {
            ObjectHandle::stream(
                ObjectHandle::dictionary(vec![(
                    b"/Filter".to_vec(),
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                )]),
                Rc::new(b"not zlib".to_vec()),
            )
        };
        let actual = make();
        let expected = make();
        let mut actual_pdf = dummy_pdf();
        let mut expected_pdf = dummy_pdf();
        assert!(compare_objects(
            "flate-error",
            &actual,
            &expected,
            &mut actual_pdf,
            &mut expected_pdf,
        )
        .is_err());
    }

    #[test]
    fn stream_filter_detection_matches_qpdf_name_and_array_order() {
        let direct_name = ObjectHandle::dictionary(vec![(
            b"/Filter".to_vec(),
            ObjectHandle::name(b"FlateDecode".to_vec()),
        )]);
        assert!(stream_uses_flatedecode(&direct_name).expect("check direct name filter"));

        let scalar =
            ObjectHandle::dictionary(vec![(b"/Filter".to_vec(), ObjectHandle::integer(7))]);
        assert!(!stream_uses_flatedecode(&scalar).expect("check scalar filter"));

        let array_with_non_name = ObjectHandle::dictionary(vec![(
            b"/Filter".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::name(b"ASCII85Decode".to_vec()),
                ObjectHandle::integer(7),
            ]),
        )]);
        assert!(!stream_uses_flatedecode(&array_with_non_name).expect("check filter array"));

        let pdf = dummy_pdf();
        let indirect_name = pdf
            .make_indirect_from_object_handle(ObjectHandle::name(b"FlateDecode".to_vec()))
            .expect("make the filter name indirect");
        let array_with_indirect_name = ObjectHandle::dictionary(vec![(
            b"/Filter".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::name(b"ASCII85Decode".to_vec()),
                indirect_name,
            ]),
        )]);
        assert!(stream_uses_flatedecode(&array_with_indirect_name).expect("resolve array filter"));
    }
}

//! Integration coverage for the public qpdf-shaped form-field helper.

use flpdf::form_field_object_helper::FormFieldObjectHelper;
use flpdf::{Error, ObjectHandle, ObjectRef, Pdf};
use std::io::Cursor;

mod common;
use common::{build_pdf, write_default};

fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
}

fn doc(mut objects: Vec<(u32, String)>) -> Vec<u8> {
    let mut base = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (3, "<< /Type /Page /Parent 2 0 R >>".to_string()),
    ];
    base.append(&mut objects);
    build_pdf(&base, 1)
}

fn doc_with_acroform(mut objects: Vec<(u32, String)>) -> Vec<u8> {
    let mut base = vec![
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /AcroForm 20 0 R >>".to_string(),
        ),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (3, "<< /Type /Page /Parent 2 0 R >>".to_string()),
    ];
    base.append(&mut objects);
    build_pdf(&base, 1)
}

fn doc_with_root(root: &str, mut objects: Vec<(u32, String)>) -> Vec<u8> {
    let mut base = vec![
        (1, root.to_owned()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
        (3, "<< /Type /Page /Parent 2 0 R >>".to_owned()),
    ];
    base.append(&mut objects);
    build_pdf(&base, 1)
}

fn resolved_handle(pdf: &mut Pdf<Cursor<Vec<u8>>>, object_ref: ObjectRef) -> ObjectHandle {
    let handle = pdf.get_object_handle(object_ref);
    pdf.resolve(&handle).expect("resolve object");
    handle
}

fn resolved_value(pdf: &mut Pdf<Cursor<Vec<u8>>>, value: ObjectHandle) -> ObjectHandle {
    pdf.resolve(&value).expect("resolve value");
    value
}

fn resolved_key(pdf: &mut Pdf<Cursor<Vec<u8>>>, object: &ObjectHandle, key: &[u8]) -> ObjectHandle {
    resolved_value(pdf, object.get_key(key))
}

fn has_entry(object: &ObjectHandle, key: &[u8]) -> bool {
    object
        .as_dictionary()
        .expect("entry lookup requires a dictionary")
        .contains_key(key)
}

fn assert_direct_dictionary(object: &ObjectHandle, description: &str) {
    assert!(
        object.object_ref().is_none(),
        "{description} must retain direct identity"
    );
    assert!(
        object.as_dictionary().is_some(),
        "{description} must remain a dictionary"
    );
}

fn direct_entry(object: &ObjectHandle, key: &[u8]) -> ObjectHandle {
    let value = object
        .as_dictionary()
        .expect("direct entry lookup requires a dictionary")
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("missing direct dictionary entry {key:?}"));
    assert!(
        value.object_ref().is_none(),
        "dictionary entry {key:?} must retain direct identity"
    );
    value
}

fn key_name(_pdf: &mut Pdf<Cursor<Vec<u8>>>, object: &ObjectHandle, key: &[u8]) -> Option<Vec<u8>> {
    direct_entry(object, key).as_name()
}

fn key_string(
    _pdf: &mut Pdf<Cursor<Vec<u8>>>,
    object: &ObjectHandle,
    key: &[u8],
) -> Option<Vec<u8>> {
    direct_entry(object, key).as_string()
}

fn key_boolean(_pdf: &mut Pdf<Cursor<Vec<u8>>>, object: &ObjectHandle, key: &[u8]) -> Option<bool> {
    direct_entry(object, key).as_boolean()
}

#[test]
fn exposes_qpdf_form_field_helper_from_its_own_module() {
    let _ = std::any::type_name::<FormFieldObjectHelper<'static, Cursor<Vec<u8>>>>();
}

#[test]
fn reads_indirect_field_attributes_and_names() {
    // qpdf's getKey dereferences leaf objects for the field type, value, and
    // all three field-name accessors.
    let bytes = doc(vec![
        (
            10,
            "<< /FT 20 0 R /V 21 0 R /DV 22 0 R /Ff 23 0 R /T 24 0 R /TU 25 0 R /TM 26 0 R >>"
                .into(),
        ),
        (20, "/Tx".into()),
        (21, "(current)".into()),
        (22, "(default)".into()),
        (23, "4097".into()),
        (24, "(partial)".into()),
        (25, "(alternative)".into()),
        (26, "(mapping)".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);

    assert_eq!(field.field_type().unwrap(), Some(b"/Tx".to_vec()));
    assert_eq!(
        field
            .field_value()
            .unwrap()
            .and_then(|value| value.as_string()),
        Some(b"current".to_vec())
    );
    assert_eq!(
        field
            .field_default_value()
            .unwrap()
            .and_then(|value| value.as_string()),
        Some(b"default".to_vec())
    );
    assert_eq!(field.field_flags().unwrap(), Some(4097));
    assert_eq!(field.partial_name().unwrap(), "partial");
    assert_eq!(field.alternative_name().unwrap(), "alternative");
    assert_eq!(field.mapping_name().unwrap(), "mapping");
}

#[test]
fn typed_inheritable_values_follow_terminal_holder_chains_without_losing_raw_identity() {
    let bytes = doc(vec![
        (
            10,
            "<< /V 20 0 R /DV 23 0 R /Ff 26 0 R /Parent 11 0 R >>".into(),
        ),
        (11, "<< /V (parent) /DV (parent-default) /Ff 1 >>".into()),
        (20, "(current)".into()),
        (23, "(default)".into()),
        (26, "4097".into()),
    ]);
    let mut pdf = open(bytes);

    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(
        field
            .field_value()
            .unwrap()
            .and_then(|value| value.as_string()),
        Some(b"current".to_vec())
    );
    assert_eq!(
        field
            .field_default_value()
            .unwrap()
            .and_then(|value| value.as_string()),
        Some(b"default".to_vec())
    );
    assert_eq!(field.field_flags().unwrap(), Some(4097));
    assert_eq!(
        field.field_value_handle().unwrap().unwrap().object_ref(),
        Some(ObjectRef::new(20, 0)),
        "the typed terminal value must not replace qpdf's raw indirect handle identity"
    );
}

#[test]
fn field_name_accessors_follow_terminal_holder_chains() {
    let bytes = doc(vec![
        (10, "<< /T 20 0 R /TU 23 0 R /TM 26 0 R >>".into()),
        (20, "(partial)".into()),
        (23, "(alternative)".into()),
        (26, "(mapping)".into()),
    ]);
    let mut pdf = open(bytes);

    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.partial_name().unwrap(), "partial");
    assert_eq!(field.fully_qualified_name().unwrap(), "partial");
    assert_eq!(field.alternative_name().unwrap(), "alternative");
    assert_eq!(field.mapping_name().unwrap(), "mapping");
}

#[test]
fn qualifies_names_from_the_parent_chain() {
    let bytes = doc(vec![
        (10, "<< /T (child) /Parent 11 0 R >>".into()),
        (11, "<< /T (group) /Parent 12 0 R >>".into()),
        (12, "<< /T (top) >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.fully_qualified_name().unwrap(), "top.group.child");
}

#[test]
fn qualifies_names_through_direct_parent_handles() {
    let bytes = doc(vec![(
        10,
        "<< /T (child) /Parent << /T (parent) /Parent << /T (top) >> >> >>".into(),
    )]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);

    assert_eq!(field.fully_qualified_name().unwrap(), "top.parent.child");
}

#[test]
fn inherits_values_through_direct_parent_handles() {
    let bytes = doc(vec![(
        10,
        "<< /Parent << /Parent << /V (top) >> /V (parent) >> >>".into(),
    )]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);

    assert_eq!(
        field
            .field_value()
            .unwrap()
            .and_then(|value| value.as_string()),
        Some(b"parent".to_vec())
    );
}

#[test]
fn fully_qualified_name_terminates_on_a_reciprocal_direct_parent_cycle() {
    // qpdf's own `QPDFObjGen::set` cannot key a direct object (its
    // `QPDFObjGen` is always `(0, 0)`), so a `/Parent` chain built entirely
    // from direct dictionaries that reciprocally reference each other is
    // never caught by the `seen` set -- in qpdf or here. Real PDF bytes
    // cannot produce this shape (two direct values cannot mutually contain
    // each other in a finite file), but the public `ObjectHandle::replace_key`
    // API can construct it in memory, and `replace_key`'s own doc already
    // records that gap. This must terminate with a bounded error rather than
    // loop forever.
    let bytes = doc(vec![(10, "<< /T (child) /Parent << /T (a) >> >>".into())]);
    let mut pdf = open(bytes);
    let field_ref = ObjectRef::new(10, 0);
    let field = pdf.get_object_handle(field_ref);
    pdf.resolve(&field).unwrap();
    let direct_a = field.get_key(b"/Parent");
    let direct_b =
        ObjectHandle::dictionary(vec![(b"/T".to_vec(), ObjectHandle::string(b"b".to_vec()))]);
    direct_a.replace_key(b"/Parent", direct_b.clone()).unwrap();
    direct_b.replace_key(b"/Parent", direct_a.clone()).unwrap();

    let mut field = FormFieldObjectHelper::new(field_ref, &mut pdf);
    let error = field
        .fully_qualified_name()
        .expect_err("a reciprocal direct /Parent cycle must not loop forever");
    assert!(matches!(error, Error::Unsupported(ref message)
        if message.contains("/Parent cycle of direct dictionaries")));
}

#[test]
fn inherited_value_lookup_terminates_on_a_reciprocal_direct_parent_cycle() {
    // Same reciprocal direct-cycle shape as
    // `fully_qualified_name_terminates_on_a_reciprocal_direct_parent_cycle`,
    // exercised through `resolve_inherited_handle_from` (backing
    // `field_value`) instead. Neither direct dictionary defines `/V`, so the
    // walk can never find a terminal value and must hit the direct-cycle
    // guard.
    let bytes = doc(vec![(10, "<< /Parent << /T (a) >> >>".into())]);
    let mut pdf = open(bytes);
    let field_ref = ObjectRef::new(10, 0);
    let field = pdf.get_object_handle(field_ref);
    pdf.resolve(&field).unwrap();
    let direct_a = field.get_key(b"/Parent");
    let direct_b =
        ObjectHandle::dictionary(vec![(b"/T".to_vec(), ObjectHandle::string(b"b".to_vec()))]);
    direct_a.replace_key(b"/Parent", direct_b.clone()).unwrap();
    direct_b.replace_key(b"/Parent", direct_a.clone()).unwrap();

    let mut field = FormFieldObjectHelper::new(field_ref, &mut pdf);
    let error = field
        .field_value()
        .expect_err("a reciprocal direct /Parent cycle must not loop forever");
    assert!(matches!(error, Error::Unsupported(ref message)
        if message.contains("/Parent cycle of direct dictionaries")));
}

#[test]
fn fully_qualified_name_resolves_a_long_acyclic_direct_parent_chain() {
    // A direct-only `/Parent` chain longer than `DEFAULT_MAX_PAGE_TREE_DEPTH`
    // (100) is a legitimate acyclic shape, not a pathological one -- this
    // codebase's own parser accepts direct nesting up to depth 500
    // (`parser.rs`). The direct-cycle guard must bound an actual repeat, not
    // depth, so a 150-level acyclic direct chain must resolve.
    let bytes = doc(vec![(10, "<< /T (leaf) >>".into())]);
    let mut pdf = open(bytes);
    let field_ref = ObjectRef::new(10, 0);
    let field = pdf.get_object_handle(field_ref);
    pdf.resolve(&field).unwrap();

    let mut parent = ObjectHandle::dictionary(vec![(
        b"/T".to_vec(),
        ObjectHandle::string(b"top".to_vec()),
    )]);
    let mut expected_parts = vec!["top".to_string()];
    for index in 0..149 {
        let name = format!("n{index}");
        expected_parts.push(name.clone());
        parent = ObjectHandle::dictionary(vec![
            (b"/T".to_vec(), ObjectHandle::string(name.into_bytes())),
            (b"/Parent".to_vec(), parent),
        ]);
    }
    field.replace_key(b"/Parent", parent).unwrap();
    expected_parts.push("leaf".to_string());

    let mut field = FormFieldObjectHelper::new(field_ref, &mut pdf);
    let name = field
        .fully_qualified_name()
        .expect("a long acyclic direct /Parent chain must resolve, not error");
    assert_eq!(name, expected_parts.join("."));
}

#[test]
fn field_value_resolves_a_long_acyclic_direct_parent_chain() {
    // Same chain shape as
    // `fully_qualified_name_resolves_a_long_acyclic_direct_parent_chain`, but
    // exercised through `resolve_inherited_handle_from` (backing
    // `field_value`): a genuinely acyclic direct chain past the old depth
    // bound must still resolve the terminal `/V`.
    let bytes = doc(vec![(10, "<< >>".into())]);
    let mut pdf = open(bytes);
    let field_ref = ObjectRef::new(10, 0);
    let field = pdf.get_object_handle(field_ref);
    pdf.resolve(&field).unwrap();

    let mut parent = ObjectHandle::dictionary(vec![(
        b"/V".to_vec(),
        ObjectHandle::string(b"top-value".to_vec()),
    )]);
    for _ in 0..149 {
        parent = ObjectHandle::dictionary(vec![(b"/Parent".to_vec(), parent)]);
    }
    field.replace_key(b"/Parent", parent).unwrap();

    let mut field = FormFieldObjectHelper::new(field_ref, &mut pdf);
    let value = field
        .field_value()
        .expect("a long acyclic direct /Parent chain must resolve, not error")
        .and_then(|value| value.as_string());
    assert_eq!(value, Some(b"top-value".to_vec()));
}

#[test]
fn decodes_pdf_text_strings_in_field_names() {
    // qpdf's getUTF8Value() decodes PDFDocEncoding and UTF-16BE before
    // assembling the public field-name strings.
    let bytes = doc(vec![(
        10,
        "<< /T <FEFF006300680069006C0064> /TU <FEFF30E630FC30B630FC> /TM <FEFF30DE30C330D7> >>"
            .into(),
    )]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.partial_name().unwrap(), "child");
    assert_eq!(field.alternative_name().unwrap(), "ユーザー");
    assert_eq!(field.mapping_name().unwrap(), "マップ");
}

#[test]
fn field_names_use_qpdf_lossy_text_string_conversion() {
    let bytes = doc(vec![(
        10,
        "<< /T <7F> /TU <FEFFD800> /TM <FEFF004100> >>".into(),
    )]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);

    assert_eq!(field.partial_name().unwrap(), "�");
    assert_eq!(field.alternative_name().unwrap(), "");
    assert_eq!(field.mapping_name().unwrap(), "A");
}

#[test]
fn mapping_name_falls_back_to_alternative_then_qualified_name() {
    let bytes = doc(vec![
        (10, "<< /T (child) /Parent 11 0 R >>".into()),
        (11, "<< /T (parent) >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.mapping_name().unwrap(), "parent.child");
    let bytes = doc(vec![(10, "<< /T (child) /TU (alt) >>".into())]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.mapping_name().unwrap(), "alt");
}

#[test]
fn name_walkers_terminate_on_parent_cycles() {
    let bytes = doc(vec![
        (10, "<< /T (child) /Parent 11 0 R >>".into()),
        (11, "<< /T (parent) /Parent 10 0 R >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.fully_qualified_name().unwrap(), "parent.child");
}

#[test]
fn non_dictionary_field_has_no_readable_attributes() {
    let bytes = doc(vec![(10, "42".into())]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.field_type().unwrap(), None);
    assert!(field.field_value().unwrap().is_none());
    assert!(field.field_default_value().unwrap().is_none());
    assert_eq!(field.field_flags().unwrap(), None);
    assert_eq!(field.fully_qualified_name().unwrap(), "");
    assert_eq!(field.alternative_name().unwrap(), "");
    assert_eq!(field.mapping_name().unwrap(), "");
}

#[test]
fn mutating_a_non_dictionary_field_is_a_qpdf_style_no_op() {
    // QPDFObjectHandle::replaceKey warns and returns when the target is not a
    // dictionary; public field mutation follows the same no-op boundary.
    let bytes = doc(vec![(10, "42".into())]);
    let mut pdf = open(bytes);
    let before = resolved_handle(&mut pdf, ObjectRef::new(10, 0));

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value_string("value", true)
        .expect("non-dictionary field mutation is ignored");

    let after = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(before.is_same_object_as(&after));
    assert_eq!(after.as_integer(), Some(42));
}

#[test]
fn field_type_wrong_type_on_child_stops_parent_inheritance() {
    // qpdf's getInheritableFieldValue stops at a present, non-null `/FT` even
    // when getFieldType then rejects the value for not being a name.
    let bytes = doc(vec![
        (10, "<< /FT 42 /Parent 11 0 R >>".into()),
        (11, "<< /FT /Tx >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.field_type().unwrap(), None);
}

#[test]
fn field_type_follows_multi_hop_reference_holders_before_testing_its_type() {
    // qpdf's `getKey` dereferences an indirect holder chain before
    // `getFieldType` decides whether the inherited value is a name.
    let bytes = doc(vec![
        (10, "<< /FT 20 0 R /Parent 11 0 R >>".into()),
        (11, "<< /FT /Ch >>".into()),
        (20, "/Tx".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);

    assert_eq!(field.field_type().unwrap(), Some(b"/Tx".to_vec()));
}

#[test]
fn value_reference_skips_a_child_reference_that_resolves_to_null() {
    let bytes = doc(vec![
        (10, "<< /V 20 0 R /Parent 11 0 R >>".into()),
        (11, "<< /V 21 0 R >>".into()),
        (20, "null".into()),
        (21, "<< /ByteRange [0 1 2 3] >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(
        field.field_value_reference().unwrap(),
        Some(ObjectRef::new(21, 0))
    );
}

#[test]
fn field_flags_wrong_type_on_child_stops_parent_inheritance_with_zero() {
    // qpdf's getFlags converts a non-integer inheritable `/Ff` to zero rather
    // than consulting an ancestor's integer flag value.
    let bytes = doc(vec![
        (10, "<< /Ff /Nope /Parent 11 0 R >>".into()),
        (11, "<< /Ff 1 >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.field_flags().unwrap(), Some(0));
}

#[test]
fn classifies_qpdf_field_types_from_inherited_type_and_flags() {
    let cases = [
        ("/Tx", 0, true, false, false, false, false),
        ("/Btn", 0, false, true, false, false, false),
        ("/Btn", 1 << 15, false, false, true, false, false),
        ("/Btn", 1 << 16, false, false, false, true, false),
        ("/Ch", 0, false, false, false, false, true),
    ];

    for (field_type, flags, text, checkbox, radio, pushbutton, choice) in cases {
        let bytes = doc(vec![
            (10, "<< /Parent 11 0 R >>".to_string()),
            (11, format!("<< /FT {field_type} /Ff {flags} >>")),
        ]);
        let mut pdf = open(bytes);
        let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);

        assert_eq!(field.is_text().unwrap(), text, "{field_type} /Ff {flags}");
        assert_eq!(
            field.is_checkbox().unwrap(),
            checkbox,
            "{field_type} /Ff {flags}"
        );
        assert_eq!(
            field.is_radio_button().unwrap(),
            radio,
            "{field_type} /Ff {flags}"
        );
        assert_eq!(
            field.is_pushbutton().unwrap(),
            pushbutton,
            "{field_type} /Ff {flags}"
        );
        assert_eq!(
            field.is_choice().unwrap(),
            choice,
            "{field_type} /Ff {flags}"
        );
    }
}

#[test]
fn choices_returns_only_string_options_from_an_indirect_inherited_array() {
    // qpdf's getChoices() (`QPDFFormFieldObjectHelper.cc:268-285`) accepts
    // only string array items. A two-string export/display pair is ignored.
    let bytes = doc(vec![
        (10, "<< /FT /Ch /Parent 11 0 R >>".into()),
        (11, "<< /Opt 12 0 R >>".into()),
        (12, "[(one) [(export) (display)] 42 (two)]".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.choices().unwrap(), vec!["one", "two"]);

    let bytes = doc(vec![
        (10, "<< /FT /Ch /Opt [12 0 R (direct)] >>".into()),
        (12, "(indirect)".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.choices().unwrap(), vec!["indirect", "direct"]);

    let bytes = doc(vec![(10, "<< /FT /Tx /Opt [(one)] >>".into())]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert!(field.choices().unwrap().is_empty());
}

#[test]
fn reads_metadata_from_field_inheritance_then_acroform() {
    let bytes = doc_with_acroform(vec![
        (10, "<< /Parent 11 0 R >>".into()),
        (11, "<< /DA 12 0 R /Q 2 >>".into()),
        (20, "<< /DR 21 0 R /DA 22 0 R /Q 1 >>".into()),
        (12, "(/Helv 9 Tf 0 g)".into()),
        (21, "<< /Font << /Helv 23 0 R >> >>".into()),
        (22, "(/Helv 8 Tf 0 g)".into()),
        (
            23,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        ),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);

    assert_eq!(field.default_appearance().unwrap(), "/Helv 9 Tf 0 g");
    assert_eq!(field.quadding().unwrap(), 2);
    let resources = field.default_resources().unwrap();
    assert!(resources.is_some_and(|value| value.as_dictionary().is_some()));

    let bytes = doc_with_acroform(vec![
        (10, "<< >>".into()),
        (20, "<< /DA 22 0 R /Q 1 >>".into()),
        (22, "(/Helv 8 Tf 0 g)".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.default_appearance().unwrap(), "/Helv 8 Tf 0 g");
    assert_eq!(field.quadding().unwrap(), 1);
    assert!(field.default_resources().unwrap().is_none());

    let bytes = doc_with_acroform(vec![
        (10, "<< /Parent 11 0 R /DA /Wrong /Q /Wrong >>".into()),
        (11, "<< /DA (ignored) /Q 2 >>".into()),
        (20, "<< /DA (fallback) /Q 1 >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.default_appearance().unwrap(), "fallback");
    assert_eq!(field.quadding().unwrap(), 1);
}

#[test]
fn default_appearance_follows_a_long_acyclic_parent_chain_before_acroform_fallback() {
    let mut objects = Vec::new();
    for number in 100..=201 {
        let dictionary = if number == 201 {
            "<< >>".to_string()
        } else {
            format!("<< /Parent {} 0 R >>", number + 1)
        };
        objects.push((number, dictionary));
    }
    objects.push((20, "<< /DA (/Helv 8 Tf 0 g) >>".into()));
    let mut pdf = open(doc_with_acroform(objects));

    let appearance = FormFieldObjectHelper::new(ObjectRef::new(100, 0), &mut pdf)
        .default_appearance()
        .expect("qpdf inheritance walk is cycle-bounded, not depth-bounded");

    assert_eq!(appearance, "/Helv 8 Tf 0 g");
}

#[test]
fn checked_requires_checkbox_and_an_on_name_value() {
    let cases = [
        ("/Btn", 0, "/On", true),
        ("/Btn", 0, "/Off", false),
        ("/Btn", 1 << 15, "/On", false),
        ("/Btn", 0, "(not-a-name)", false),
    ];
    for (field_type, flags, value, expected) in cases {
        let bytes = doc(vec![
            (10, format!("<< /FT {field_type} /Ff {flags} /V 11 0 R >>")),
            (11, value.into()),
        ]);
        let mut pdf = open(bytes);
        let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
        assert_eq!(field.is_checked().unwrap(), expected);
    }

    let bytes = doc(vec![(10, "<< /FT /Btn /Ff 0 >>".into())]);
    let mut pdf = open(bytes);
    assert!(!FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .is_checked()
        .unwrap());
}

#[test]
fn checked_inherits_parent_value_and_honors_child_override() {
    let bytes = doc(vec![
        (10, "<< /Parent 11 0 R >>".into()),
        (11, "<< /FT /Btn /Ff 0 /V /On >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert!(field.is_checked().unwrap());

    let bytes = doc(vec![
        (10, "<< /Parent 11 0 R /V /Off >>".into()),
        (11, "<< /FT /Btn /Ff 0 /V /On >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert!(!field.is_checked().unwrap());
}

#[test]
fn exposes_remaining_qpdf_read_and_traversal_accessors() {
    let bytes = doc(vec![
        (10, "<< /Parent 11 0 R /V 13 0 R /DV 14 0 R >>".into()),
        (
            11,
            "<< /Parent 12 0 R /CustomString 15 0 R /CustomName 16 0 R >>".into(),
        ),
        (12, "<< >>".into()),
        (13, "(current)".into()),
        (14, "(default)".into()),
        (15, "(inherited)".into()),
        (16, "/InheritedName".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);

    assert!(!field.is_null().unwrap());
    assert_eq!(field.parent().unwrap(), Some(ObjectRef::new(11, 0)));
    assert_eq!(
        field.get_top_level_field().unwrap(),
        (ObjectRef::new(12, 0), true)
    );
    assert_eq!(
        field
            .inheritable_value(b"CustomString")
            .unwrap()
            .and_then(|value| value.as_string()),
        Some(b"inherited".to_vec())
    );
    assert_eq!(
        field.inheritable_string(b"CustomString").unwrap(),
        "inherited"
    );
    assert_eq!(
        field.inheritable_name(b"CustomName").unwrap(),
        b"/InheritedName"
    );
    assert_eq!(
        field.value().unwrap().and_then(|value| value.as_string()),
        Some(b"current".to_vec())
    );
    assert_eq!(field.value_as_string().unwrap(), "current");
    assert_eq!(
        field
            .default_value()
            .unwrap()
            .and_then(|value| value.as_string()),
        Some(b"default".to_vec())
    );
    assert_eq!(field.default_value_as_string().unwrap(), "default");
    assert_eq!(field.flags().unwrap(), 0);

    let bytes = doc(vec![(10, "null".into())]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert!(field.is_null().unwrap());
}

#[test]
fn get_top_level_field_stops_when_a_parent_chain_returns_to_a_seen_handle() {
    let bytes = doc(vec![
        (10, "<< /Parent 11 0 R >>".into()),
        (11, "<< /Parent 10 0 R >>".into()),
    ]);
    let mut pdf = open(bytes);

    assert_eq!(
        FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
            .get_top_level_field()
            .unwrap(),
        (ObjectRef::new(10, 0), true)
    );
}

#[test]
fn parent_returns_none_for_a_null_parent() {
    let bytes = doc(vec![(10, "<< /Parent null >>".into())]);
    let mut pdf = open(bytes);

    assert_eq!(
        FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
            .parent()
            .unwrap(),
        None
    );
}

#[test]
fn get_parent_returns_a_null_handle_for_a_non_dictionary_field() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let mut field = FormFieldObjectHelper::from_object_handle(ObjectHandle::integer(42), &mut pdf);

    assert!(field
        .get_parent()
        .expect("get parent from non-dictionary field")
        .is_null());
}

#[test]
fn is_null_resolves_a_multi_hop_field_holder_to_its_terminal_value() {
    let bytes = doc(vec![(10, "null".into())]);
    let mut pdf = open(bytes);

    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);

    assert!(field.is_null().unwrap());
}

#[test]
fn set_field_attribute_string_writes_a_qpdf_unicode_string_on_the_field() {
    let bytes = doc(vec![
        (10, "<< /Parent 11 0 R >>".into()),
        (11, "<< >>".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_field_attribute_string(b"TU", "日本語")
        .expect("attribute set");

    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(
        field.as_dictionary().is_some(),
        "field must stay a dictionary"
    );
    assert_eq!(
        key_string(&mut pdf, &field, b"/TU"),
        Some(flpdf::pdf_string::new_unicode_string("日本語".as_bytes()))
    );
}

#[test]
fn set_field_attribute_preserves_non_utf8_key_bytes() {
    let bytes = doc(vec![(10, "<< /FT /Tx >>".into())]);
    let mut pdf = open(bytes);
    let raw_key = b"Custom\xffKey";

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_field_attribute(raw_key, ObjectHandle::integer(42))
        .unwrap();

    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    let dictionary = field.as_dictionary().expect("field dictionary");
    let mut canonical_raw_key = Vec::with_capacity(raw_key.len() + 1);
    canonical_raw_key.push(b'/');
    canonical_raw_key.extend_from_slice(raw_key);
    assert_eq!(
        dictionary
            .get(&canonical_raw_key)
            .and_then(|value| value.as_integer()),
        Some(42)
    );
    assert!(!dictionary.contains_key(b"/Custom\xef\xbf\xbdKey".as_slice()));
}

#[test]
fn set_value_marks_text_and_choice_fields_as_needing_appearances() {
    for field_type in ["/Tx", "/Ch"] {
        let bytes = doc_with_acroform(vec![
            (10, format!("<< /FT {field_type} >>")),
            (20, "<< >>".into()),
        ]);
        let mut pdf = open(bytes);
        FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
            .set_value_string("日本語", true)
            .expect("set text value");

        let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
        assert!(
            field.as_dictionary().is_some(),
            "field must be a dictionary"
        );
        assert_eq!(
            key_string(&mut pdf, &field, b"/V"),
            Some(flpdf::pdf_string::new_unicode_string("日本語".as_bytes()))
        );
        let acroform = resolved_handle(&mut pdf, ObjectRef::new(20, 0));
        assert!(
            acroform.as_dictionary().is_some(),
            "AcroForm must be a dictionary"
        );
        assert_eq!(
            key_boolean(&mut pdf, &acroform, b"/NeedAppearances"),
            Some(true)
        );
    }
}

#[test]
fn set_value_marks_the_terminal_acroform_reference_as_needing_appearances() {
    // qpdf mutates the live AcroForm dictionary selected from the catalog.
    let bytes = doc_with_acroform(vec![(10, "<< /FT /Tx >>".into()), (20, "<< >>".into())]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value_string("value", true)
        .expect("set text value");

    let acroform = resolved_handle(&mut pdf, ObjectRef::new(20, 0));
    assert_eq!(
        key_boolean(&mut pdf, &acroform, b"/NeedAppearances"),
        Some(true)
    );
}

#[test]
fn default_resources_preserves_a_natural_indirect_holder_identity() {
    // qpdf's getDefaultResources returns the live value from /AcroForm/DR
    // without collapsing its indirect identity (`QPDFFormFieldObjectHelper.cc:
    // 50-63,191-194`). The cycle is nested in valid dictionaries, so the
    // traversal exercises the same natural graph that qpdf parses without the
    // removed set_object reference-value seam.
    let bytes = doc_with_acroform(vec![
        (10, "<< >>".into()),
        (20, "<< /DR 21 0 R >>".into()),
        (21, "<< /Font 22 0 R >>".into()),
        (22, "<< /Font 21 0 R >>".into()),
    ]);
    let mut pdf = open(bytes);

    let resources = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .default_resources()
        .expect("resolve default resources")
        .expect("default resources handle");
    assert_eq!(resources.object_ref(), Some(ObjectRef::new(21, 0)));
    let nested = resolved_key(&mut pdf, &resources, b"/Font");
    assert_eq!(nested.object_ref(), Some(ObjectRef::new(22, 0)));
    let back = resolved_key(&mut pdf, &nested, b"/Font");
    assert!(back.is_same_object_as(&resources));
}

#[test]
fn set_value_marks_the_live_catalog_acroform_and_writer_observes_it() {
    let bytes = doc_with_acroform(vec![
        (10, "<< /FT /Tx >>".into()),
        (20, "<< /Fields [10 0 R] >>".into()),
    ]);
    let mut pdf = open(bytes);
    let root_ref = pdf.root_ref().expect("catalog reference");
    let root = pdf.get_object_handle(root_ref);
    pdf.resolve(&root).expect("catalog handle");
    let acroform = root.get_key(b"/AcroForm");
    pdf.resolve(&acroform).expect("AcroForm handle");
    assert!(acroform.as_dictionary().is_some());
    let same_acroform = pdf.get_object_handle(ObjectRef::new(20, 0));
    assert!(acroform.is_same_object_as(&same_acroform));

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value_string("value", true)
        .expect("set text value");
    assert_eq!(
        same_acroform.get_key(b"/NeedAppearances").as_boolean(),
        Some(true)
    );

    let mut output = Vec::new();
    write_default(&mut pdf, &mut output).expect("write mutated document");
    let mut reopened = open(output);
    let reopened_root_ref = reopened.root_ref().expect("rewritten catalog");
    let reopened_root = reopened.get_object_handle(reopened_root_ref);
    reopened
        .resolve(&reopened_root)
        .expect("rewritten catalog handle");
    let reopened_acroform = reopened_root.get_key(b"/AcroForm");
    reopened
        .resolve(&reopened_acroform)
        .expect("rewritten AcroForm handle");
    assert_eq!(
        reopened_acroform.get_key(b"/NeedAppearances").as_boolean(),
        Some(true)
    );
}

#[test]
fn set_value_updates_checkbox_and_radio_widget_states_without_need_appearances() {
    let bytes = doc_with_acroform(vec![
        (
            10,
            "<< /FT /Btn /AP << /N << /Off null /Chosen null >> >> >>".into(),
        ),
        (20, "<< >>".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"anything-but-Off".to_vec()), true)
        .expect("set checkbox value");
    let checkbox = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(
        checkbox.as_dictionary().is_some(),
        "checkbox must be a dictionary"
    );
    assert_eq!(
        key_name(&mut pdf, &checkbox, b"/V").as_deref(),
        Some(b"Chosen".as_slice())
    );
    assert_eq!(
        key_name(&mut pdf, &checkbox, b"/AS").as_deref(),
        Some(b"Chosen".as_slice())
    );

    let bytes = doc_with_acroform(vec![
        (10, "<< /FT /Btn /Ff 32768 /Kids [11 0 R 12 0 R] >>".into()),
        (11, "<< /AP << /N << /Off null /First null >> >> >>".into()),
        (12, "<< /AP << /N << /Off null /Second null >> >> >>".into()),
        (20, "<< >>".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"Second".to_vec()), true)
        .expect("set radio value");
    for (reference, expected) in [(11, b"Off".as_slice()), (12, b"Second".as_slice())] {
        let widget = resolved_handle(&mut pdf, ObjectRef::new(reference, 0));
        assert!(
            widget.as_dictionary().is_some(),
            "widget must be a dictionary"
        );
        assert_eq!(
            key_name(&mut pdf, &widget, b"/AS").as_deref(),
            Some(expected)
        );
    }
    let acroform = resolved_handle(&mut pdf, ObjectRef::new(20, 0));
    assert!(
        acroform.as_dictionary().is_some(),
        "AcroForm must be a dictionary"
    );
    assert!(!has_entry(&acroform, b"/NeedAppearances"));
}

#[test]
fn set_value_turns_an_existing_checkbox_off_and_leaves_pushbuttons_unchanged() {
    let bytes = doc(vec![(
        10,
        "<< /FT /Btn /V /Chosen /AS /Chosen /AP << /N << /Off null /Chosen null >> >> >>".into(),
    )]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"Off".to_vec()), true)
        .expect("turn checkbox off");
    let checkbox = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(
        checkbox.as_dictionary().is_some(),
        "checkbox must be a dictionary"
    );
    assert_eq!(
        key_name(&mut pdf, &checkbox, b"/V").as_deref(),
        Some(b"Off".as_slice())
    );
    assert_eq!(
        key_name(&mut pdf, &checkbox, b"/AS").as_deref(),
        Some(b"Off".as_slice())
    );

    let bytes = doc(vec![(
        10,
        "<< /FT /Btn /Ff 65536 /V /Existing /AS /Existing /AP << /N /Existing >> >>".into(),
    )]);
    let mut pdf = open(bytes);
    let before = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    let before_snapshot = before.unparse_resolved();
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"New".to_vec()), true)
        .expect("pushbutton value is ignored");
    let after = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(before.is_same_object_as(&after));
    assert_eq!(after.unparse_resolved(), before_snapshot);
}

#[test]
fn set_value_updates_a_checkbox_direct_kid_widget() {
    // qpdf calls `getKey("/AP")` on each /Kids array item, including direct
    // dictionaries, and writes both the field value and that widget's state.
    let bytes = doc(vec![(
        10,
        "<< /FT /Btn /Kids [ << /AP << /N << /Off null /Chosen null >> >> >> ] >>".into(),
    )]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), true)
        .expect("set direct-widget checkbox value");

    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(
        field.as_dictionary().is_some(),
        "checkbox field must be a dictionary"
    );
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"Chosen".as_slice())
    );
    let kids = resolved_key(&mut pdf, &field, b"/Kids")
        .as_array()
        .expect("checkbox must retain direct widget child");
    let widget = resolved_value(&mut pdf, kids.into_iter().next().expect("widget"));
    assert_direct_dictionary(&widget, "widget");
    assert_eq!(
        key_name(&mut pdf, &widget, b"/AS").as_deref(),
        Some(b"Chosen".as_slice())
    );
}

#[test]
fn set_value_preserves_kids_order_when_direct_widget_precedes_a_reference() {
    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Kids [ << /AP << /N << /Off null /Direct null >> >> >> 11 0 R ] >>"
                .into(),
        ),
        (
            11,
            "<< /AP << /N << /Off null /Indirect null >> >> >>".into(),
        ),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .expect("set checkbox value");

    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(
        field.as_dictionary().is_some(),
        "field must be a dictionary"
    );
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"Direct".as_slice())
    );
    let kids = resolved_key(&mut pdf, &field, b"/Kids")
        .as_array()
        .expect("kids must be an array");
    let direct = resolved_value(&mut pdf, kids[0].clone());
    assert_direct_dictionary(&direct, "first kid");
    assert_eq!(
        key_name(&mut pdf, &direct, b"/AS").as_deref(),
        Some(b"Direct".as_slice())
    );
    let indirect = resolved_handle(&mut pdf, ObjectRef::new(11, 0));
    assert!(
        indirect.as_dictionary().is_some(),
        "indirect widget must be a dictionary"
    );
    assert!(!has_entry(&indirect, b"/AS"));
}

#[test]
fn set_value_skips_non_dictionary_indirect_checkbox_kids_before_a_valid_widget() {
    // qpdf's `getKey` on a malformed indirect kid yields null; it continues
    // scanning `/Kids` and updates a later widget dictionary.
    let bytes = doc(vec![
        (10, "<< /FT /Btn /Kids [11 0 R 12 0 R] >>".into()),
        (11, "null".into()),
        (12, "<< /AP << /N << /Off null /Chosen null >> >> >>".into()),
    ]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .expect("set checkbox value");

    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"Chosen".as_slice())
    );
    let widget = resolved_handle(&mut pdf, ObjectRef::new(12, 0));
    assert_eq!(
        key_name(&mut pdf, &widget, b"/AS").as_deref(),
        Some(b"Chosen".as_slice())
    );
}

#[test]
fn set_value_updates_checkbox_state_for_a_non_dictionary_direct_appearance() {
    // qpdf treats any non-null field-level `/AP` as the annotation to update;
    // only its on-state lookup requires a dictionary, so `/Yes` is the
    // fallback when `/AP` itself is malformed.
    let bytes = doc(vec![(10, "<< /FT /Btn /AP 42 >>".into())]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .expect("set checkbox value with malformed appearance");
    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(
        field.as_dictionary().is_some(),
        "checkbox field must be a dictionary"
    );
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"Yes".as_slice())
    );
    assert_eq!(
        key_name(&mut pdf, &field, b"/AS").as_deref(),
        Some(b"Yes".as_slice())
    );
}

#[test]
fn checkbox_propagates_an_unresolvable_normal_appearance_error() {
    let bytes = doc(vec![(10, "<< /FT /Btn /AP << /N 99 0 R >> >>".into())]);
    let mut pdf = open(bytes);

    let result = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false);

    assert!(result.is_ok());
    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"Yes".as_slice())
    );
}

#[test]
fn set_value_resolves_null_parent_and_indirect_kids_for_button_widgets() {
    // qpdf's `getKey("/Parent")` returns a null object for an explicit
    // `/Parent null`, so `setRadioButtonValue` treats this as a top-level
    // field. `getKey("/Kids")` also resolves its indirect array.
    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Ff 32768 /Parent null /Kids 13 0 R >>".into(),
        ),
        (11, "<< /AP << /N << /Off null /First null >> >> >>".into()),
        (12, "<< /AP << /N << /Off null /Second null >> >> >>".into()),
        (13, "[11 0 R 12 0 R]".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"Second".to_vec()), true)
        .expect("set indirect-kids radio value");
    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(
        field.as_dictionary().is_some(),
        "radio field must be a dictionary"
    );
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"Second".as_slice())
    );
    for (reference, expected) in [(11, b"Off".as_slice()), (12, b"Second".as_slice())] {
        let widget = resolved_handle(&mut pdf, ObjectRef::new(reference, 0));
        assert!(
            widget.as_dictionary().is_some(),
            "widget must be a dictionary"
        );
        assert_eq!(
            key_name(&mut pdf, &widget, b"/AS").as_deref(),
            Some(expected)
        );
    }

    let bytes = doc(vec![
        (10, "<< /FT /Btn /Kids 12 0 R >>".into()),
        (11, "<< /AP << /N << /Off null /Chosen null >> >> >>".into()),
        (12, "[11 0 R]".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), true)
        .expect("set indirect-kids checkbox value");
    let widget = resolved_handle(&mut pdf, ObjectRef::new(11, 0));
    assert!(
        widget.as_dictionary().is_some(),
        "widget must be a dictionary"
    );
    assert_eq!(
        key_name(&mut pdf, &widget, b"/AS").as_deref(),
        Some(b"Chosen".as_slice())
    );
}

#[test]
fn set_value_updates_a_radio_grandchild_widget_and_direct_kid_dictionaries() {
    // `setRadioButtonValue` examines only one `/Kids` level when a radio
    // child field has no `/AP`. qpdf object handles allow both that child and
    // its widget to be direct dictionaries in their respective arrays.
    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Ff 32768 /Parent null /Kids [ << /Kids [ << /AP << /N << /Off null /On null >> >> >> ] >> ] >>"
                .into(),
        ),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), true)
        .expect("set nested radio value");

    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(
        field.as_dictionary().is_some(),
        "radio field must be a dictionary"
    );
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"On".as_slice())
    );
    let children = resolved_key(&mut pdf, &field, b"/Kids")
        .as_array()
        .expect("radio field must retain direct children");
    let child = resolved_value(&mut pdf, children.into_iter().next().expect("child field"));
    assert_direct_dictionary(&child, "child field");
    let widgets = resolved_key(&mut pdf, &child, b"/Kids")
        .as_array()
        .expect("child field must retain direct widget children");
    let widget = resolved_value(&mut pdf, widgets.into_iter().next().expect("widget"));
    assert_direct_dictionary(&widget, "widget");
    assert_eq!(
        key_name(&mut pdf, &widget, b"/AS").as_deref(),
        Some(b"On".as_slice())
    );
}

#[test]
fn set_value_turns_on_state_off_when_radio_appearance_is_non_null_but_not_a_dictionary() {
    // qpdf selects a radio kid as soon as `/AP` is non-null. It still writes
    // `/AS /Off` when that appearance cannot have an `/N` dictionary, whether
    // `/AP` is direct or indirect.
    let bytes = doc(vec![
        (10, "<< /FT /Btn /Ff 32768 /Kids [11 0 R 12 0 R] >>".into()),
        (11, "<< /AP /Bogus >>".into()),
        (12, "<< /AP 13 0 R >>".into()),
        (13, "/AlsoBogus".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), true)
        .expect("set radio value with malformed appearances");

    for reference in [11, 12] {
        let widget = resolved_handle(&mut pdf, ObjectRef::new(reference, 0));
        assert!(
            widget.as_dictionary().is_some(),
            "widget must be a dictionary"
        );
        assert_eq!(
            key_name(&mut pdf, &widget, b"/AS").as_deref(),
            Some(b"Off".as_slice())
        );
    }
}

#[test]
fn generates_a_field_value_on_its_separate_widget() {
    let bytes = doc_with_acroform(vec![
        (
            10,
            "<< /FT /Tx /V (value) /DA (/Helv 12 Tf 0 g) /Kids [11 0 R] >>".into(),
        ),
        (
            11,
            "<< /Subtype /Widget /Parent 12 0 R /Rect [0 0 100 20] >>".into(),
        ),
        (12, "<< /FT /Tx /V (wrong widget value) >>".into()),
        (20, "<< >>".into()),
    ]);
    let mut pdf = open(bytes);

    assert!(FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .generate_appearance_for(ObjectRef::new(11, 0))
        .unwrap()
        .is_some());
    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(
        !has_entry(&field, b"/AP"),
        "terminal field must not receive its widget's appearance"
    );

    let widget = resolved_handle(&mut pdf, ObjectRef::new(11, 0));
    let ap = resolved_key(&mut pdf, &widget, b"/AP");
    assert!(ap.as_dictionary().is_some());
    let appearance = resolved_key(&mut pdf, &ap, b"/N");
    assert!(
        appearance.as_stream_dict().is_some(),
        "normal appearance must be a stream"
    );
    let appearance_data = appearance
        .get_raw_stream_data()
        .expect("appearance data")
        .to_vec();
    assert!(appearance_data
        .windows(b"(value)".len())
        .any(|w| w == b"(value)"));
    assert!(!appearance_data
        .windows(b"(wrong widget value)".len())
        .any(|w| w == b"(wrong widget value)"));
}

#[test]
fn generate_appearance_dispatches_only_text_and_choice_fields() {
    let bytes = doc_with_acroform(vec![
        (
            10,
            "<< /FT /Tx /V (value) /Rect [0 0 100 20] /DA (/Helv 12 Tf 0 g) >>".into(),
        ),
        (20, "<< >>".into()),
    ]);
    let mut pdf = open(bytes);
    assert!(FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .generate_appearance_for(ObjectRef::new(10, 0))
        .expect("text appearance")
        .is_some());

    let bytes = doc_with_acroform(vec![
        (
            10,
            "<< /FT /Ch /V (value) /Opt [(value)] /Rect [0 0 100 20] /DA (/Helv 12 Tf 0 g) >>"
                .into(),
        ),
        (20, "<< >>".into()),
    ]);
    let mut pdf = open(bytes);
    assert!(FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .generate_appearance_for(ObjectRef::new(10, 0))
        .expect("choice appearance")
        .is_some());

    let bytes = doc(vec![(10, "<< /FT /Btn >>".into())]);
    let mut pdf = open(bytes);
    assert_eq!(
        FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
            .generate_appearance_for(ObjectRef::new(10, 0))
            .expect("button is skipped"),
        None
    );
}

#[test]
fn consumer_clear_need_appearances_uses_the_form_field_helper_boundary() {
    let bytes = doc_with_acroform(vec![
        (
            10,
            "<< /FT /Tx /V (value) /Rect [0 0 100 20] /DA (/Helv 12 Tf 0 g) >>".into(),
        ),
        (20, "<< /NeedAppearances true >>".into()),
    ]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::clear_need_appearances_after_generation(&mut pdf)
        .expect("clear generated-appearance marker");

    let acroform = resolved_handle(&mut pdf, ObjectRef::new(20, 0));
    assert!(
        acroform.as_dictionary().is_some(),
        "AcroForm must be a dictionary"
    );
    assert!(!has_entry(&acroform, b"/NeedAppearances"));
}

#[test]
fn field_accessors_return_qpdf_defaults_for_missing_or_wrong_typed_values() {
    // qpdf's string/name/choice accessors return their empty defaults rather
    // than coercing an object of the wrong PDF type.
    let bytes = doc_with_acroform(vec![
        (10, "<< /CustomString /NotAString /CustomName (not-a-name) /FT /Ch /Opt 42 /Q /Bad /T /NotAString >>".into()),
        (20, "<< /Q /AlsoBad >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);

    assert_eq!(field.inheritable_string(b"CustomString").unwrap(), "");
    assert_eq!(field.inheritable_name(b"CustomName").unwrap(), b"");
    assert_eq!(field.partial_name().unwrap(), "");
    assert_eq!(field.choices().unwrap(), Vec::<String>::new());
    assert_eq!(field.quadding().unwrap(), 0);

    let bytes = doc(vec![(10, "<< /FT /Ch >>".into())]);
    let mut pdf = open(bytes);
    assert!(FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .choices()
        .unwrap()
        .is_empty());
}

#[test]
fn button_values_ignore_non_names_and_malformed_widget_containers() {
    // qpdf 11.9.0 `setV` (QPDFFormFieldObjectHelper.cc:306-330) ignores
    // non-name button values. Its radio helper also leaves a non-array /Kids
    // container untouched rather than creating a field value.
    let bytes = doc_with_acroform(vec![
        (
            10,
            "<< /FT /Btn /AP << /N << /Off null /On null >> >> >>".into(),
        ),
        (20, "<< >>".into()),
    ]);
    let mut pdf = open(bytes);
    let before = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    let before_snapshot = before.unparse_resolved();
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::string(b"not-a-name".to_vec()), true)
        .unwrap();
    let after = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(before.is_same_object_as(&after));
    assert_eq!(after.unparse_resolved(), before_snapshot);
    let acroform = resolved_handle(&mut pdf, ObjectRef::new(20, 0));
    assert!(!has_entry(&acroform, b"/NeedAppearances"));

    let bytes = doc(vec![(
        10,
        "<< /FT /Btn /Ff 32768 /Kids /NotAnArray >>".into(),
    )]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .unwrap();
    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(!has_entry(&field, b"/V"));
}

#[test]
fn checkbox_defaults_to_yes_when_no_usable_widget_appearance_exists() {
    // qpdf chooses /Yes when a checkbox is set on but neither the field nor a
    // direct widget offers a usable normal-appearance on-state.
    let bytes = doc(vec![(
        10,
        "<< /FT /Btn /Kids [42 << /AP null >>] >>".into(),
    )]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .unwrap();
    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"Yes".as_slice())
    );
}

#[test]
fn checkbox_updates_a_direct_widget_through_an_indirect_kids_array() {
    // qpdf's object handles dereference the /Kids holder before selecting the
    // first direct widget with a non-null /AP.
    let bytes = doc(vec![
        (10, "<< /FT /Btn /Kids 11 0 R >>".into()),
        (
            11,
            "[<< /AP << /N << /Off null /Chosen null >> >> >>]".into(),
        ),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .unwrap();

    let kids_holder = resolved_handle(&mut pdf, ObjectRef::new(11, 0));
    let kids = kids_holder
        .as_array()
        .expect("/Kids holder must remain an array");
    let widget = resolved_value(&mut pdf, kids[0].clone());
    assert_direct_dictionary(&widget, "first /Kids item");
    assert_eq!(
        key_name(&mut pdf, &widget, b"/AS").as_deref(),
        Some(b"Chosen".as_slice())
    );
}

#[test]
fn radio_updates_parent_group_and_preserves_malformed_children() {
    // qpdf 11.9.0 `setRadioButtonValue` recurses to a top-level radio parent
    // (cc:348-365), then only changes child widgets that have a non-null /AP.
    let bytes = doc(vec![
        (10, "<< /FT /Btn /Ff 32768 /Parent 11 0 R >>".into()),
        (
            11,
            "<< /FT /Btn /Ff 32768 /Kids [10 0 R 12 0 R 13 0 R] >>".into(),
        ),
        (12, "42".into()),
        (
            13,
            "<< /AP << /N << /Off null /Selected null >> >> >>".into(),
        ),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"Selected".to_vec()), false)
        .unwrap();

    let parent = resolved_handle(&mut pdf, ObjectRef::new(11, 0));
    assert_eq!(
        key_name(&mut pdf, &parent, b"/V").as_deref(),
        Some(b"Selected".as_slice())
    );
    assert_eq!(
        resolved_handle(&mut pdf, ObjectRef::new(12, 0)).as_integer(),
        Some(42)
    );
    let widget = resolved_handle(&mut pdf, ObjectRef::new(13, 0));
    assert_eq!(
        key_name(&mut pdf, &widget, b"/AS").as_deref(),
        Some(b"Selected".as_slice())
    );
}

#[test]
fn radio_keeps_direct_children_without_appearance_or_grandchildren() {
    // A direct child that has neither /AP nor /Kids is not a selectable radio
    // widget; qpdf retains it while updating later selectable siblings.
    let bytes = doc(vec![(
        10,
        "<< /FT /Btn /Ff 32768 /Kids [ << /T (group-only) >> << /AP << /N << /Off null /On null >> >> >> ] >>"
            .into(),
    )]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .unwrap();

    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    let kids = resolved_key(&mut pdf, &field, b"/Kids")
        .as_array()
        .expect("kids array");
    let first = resolved_value(&mut pdf, kids[0].clone());
    assert_direct_dictionary(&first, "first radio child");
    assert!(!has_entry(&first, b"/AS"));
    let second = resolved_value(&mut pdf, kids[1].clone());
    assert_direct_dictionary(&second, "second radio child");
    assert_eq!(
        key_name(&mut pdf, &second, b"/AS").as_deref(),
        Some(b"On".as_slice())
    );
}

#[test]
fn get_top_level_field_has_no_depth_limit_and_reaches_the_terminal_value() {
    // qpdf's `getTopLevelField` (`QPDFFormFieldObjectHelper.cc:35-46`) climbs
    // `/Parent` with a pure cycle guard and no depth bound. A 101-level
    // indirect chain (deeper than the unrelated page-tree depth limit that
    // used to be misapplied here) must resolve to its terminal node, the
    // same way `field_value_reference`'s inherited-value walk already does.
    let mut objects = Vec::new();
    for number in 10..=111 {
        let dictionary = if number == 111 {
            "<< /V 112 0 R >>".to_string()
        } else {
            format!("<< /Parent {} 0 R >>", number + 1)
        };
        objects.push((number, dictionary));
    }
    objects.push((112, "(value)".into()));
    let mut pdf = open(doc(objects));

    let top_level = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .get_top_level_field()
        .expect("a long acyclic indirect /Parent chain must resolve, not error");
    assert_eq!(top_level, (ObjectRef::new(111, 0), true));

    let field_value_reference = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .field_value_reference()
        .unwrap();
    assert_eq!(field_value_reference, Some(ObjectRef::new(112, 0)));
}

#[test]
fn clear_need_appearances_leaves_non_true_and_malformed_acroforms_unchanged() {
    for acroform in ["<< /NeedAppearances false >>", "42"] {
        let bytes = doc_with_acroform(vec![(10, "<< /FT /Tx >>".into()), (20, acroform.into())]);
        let mut pdf = open(bytes);
        let before = resolved_handle(&mut pdf, ObjectRef::new(20, 0));
        let before_snapshot = before.unparse_resolved();
        FormFieldObjectHelper::clear_need_appearances_after_generation(&mut pdf).unwrap();
        let after = resolved_handle(&mut pdf, ObjectRef::new(20, 0));
        assert!(before.is_same_object_as(&after));
        assert_eq!(after.unparse_resolved(), before_snapshot);
    }

    let bytes = doc(vec![(10, "<< /FT /Tx >>".into())]);
    let mut pdf = open(bytes);
    let root = pdf.root_ref().unwrap();
    FormFieldObjectHelper::clear_need_appearances_after_generation(&mut pdf).unwrap();
    let root = resolved_handle(&mut pdf, root);
    assert!(!has_entry(&root, b"/AcroForm"));
}

#[test]
fn form_field_operations_are_noops_without_a_catalog_root() {
    // The public parser accepts a trailer without /Root. qpdf's document
    // helpers treat that shape as a no-op for document-level AcroForm work.
    let bytes = String::from_utf8(doc(vec![(10, "<< /FT /Tx >>".into())]))
        .unwrap()
        .replace(" /Root 1 0 R", "")
        .into_bytes();
    let mut pdf = open(bytes);
    assert_eq!(pdf.root_ref(), None);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"RawName".to_vec()), true)
        .unwrap();
    FormFieldObjectHelper::clear_need_appearances_after_generation(&mut pdf).unwrap();
    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"RawName".as_slice())
    );
}

#[test]
fn checkbox_keeps_unusable_kids_and_radio_stops_at_non_top_level_fields() {
    // A checkbox whose indirect /Kids holder is not an array falls back to
    // /Yes. A radio child whose parent is not top-level leaves its own value
    // alone, matching qpdf's setRadioButtonValue parent guard.
    let bytes = doc(vec![
        (10, "<< /FT /Btn /Kids 11 0 R >>".into()),
        (11, "42".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .unwrap();
    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"Yes".as_slice())
    );

    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Ff 32768 /Parent 11 0 R /Kids [12 0 R] >>".into(),
        ),
        (11, "<< /Parent 13 0 R >>".into()),
        (12, "<< /AP << /N << /Off null /On null >> >> >>".into()),
        (13, "<< >>".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .unwrap();
    let child = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(!has_entry(&child, b"/V"));
}

#[test]
fn radio_preserves_unselectable_direct_and_indirect_grandchildren() {
    // qpdf only selects the first grandchild with a usable /AP. Malformed
    // direct objects, indirect non-dictionaries, and dictionaries without
    // /AP remain in the original /Kids structure.
    let bytes = doc(vec![
        (10, "<< /FT /Btn /Ff 32768 /Kids [11 0 R] >>".into()),
        (11, "<< /Kids 12 0 R >>".into()),
        (12, "[42 13 0 R << /T (no-appearance) >> 14 0 R]".into()),
        (13, "null".into()),
        (14, "<< /AP << /N << /Off null /On null >> >> >>".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .unwrap();

    let widget = resolved_handle(&mut pdf, ObjectRef::new(14, 0));
    assert_eq!(
        key_name(&mut pdf, &widget, b"/AS").as_deref(),
        Some(b"On".as_slice())
    );
    let holder = resolved_handle(&mut pdf, ObjectRef::new(12, 0));
    let items = holder.as_array().expect("holder array");
    assert_eq!(
        resolved_value(&mut pdf, items[0].clone()).as_integer(),
        Some(42)
    );
    assert!(resolved_value(&mut pdf, items[1].clone())
        .object_ref()
        .is_some());
    let malformed = resolved_value(&mut pdf, items[2].clone());
    assert_direct_dictionary(&malformed, "direct malformed child");
    assert!(!has_entry(&malformed, b"/AS"));
}

#[test]
fn text_appearance_uses_standard_font_fallback_without_acroform_resources() {
    // qpdf can generate a text appearance from /DA even when the catalog has
    // no /AcroForm /DR font dictionary; standard-14 metrics provide the
    // fallback. This exercises the helper's absent-default-resources path.
    let bytes = doc(vec![(
        10,
        "<< /FT /Tx /V (value) /Rect [0 0 100 20] /DA (/Helv 12 Tf 0 g) >>".into(),
    )]);
    let mut pdf = open(bytes);
    assert!(FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .generate_appearance_for(ObjectRef::new(10, 0))
        .unwrap()
        .is_some());
}

#[test]
fn radio_value_with_a_non_dictionary_parent_is_a_qpdf_noop() {
    // qpdf 11.9.0 QPDFFormFieldObjectHelper.cc:358-373 checks that /Parent
    // is a dictionary before recursing and otherwise leaves this child alone.
    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Ff 32768 /Parent 11 0 R /Kids [12 0 R] >>".into(),
        ),
        (11, "42".into()),
        (12, "<< /AP << /N << /Off null /On null >> >> >>".into()),
    ]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .expect("qpdf ignores a non-dictionary radio parent");

    let child = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(!has_entry(&child, b"/V"));
}

#[test]
fn value_updates_cover_non_button_and_checkbox_document_boundaries() {
    let bytes = doc_with_acroform(vec![(10, "<< /FT /Tx >>".into()), (20, "42".into())]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"raw".to_vec()), true)
        .unwrap();
    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"raw".as_slice())
    );

    let bytes = doc_with_root(
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << >> >>",
        vec![(10, "<< /FT /Tx >>".into())],
    );
    let mut pdf = open(bytes);
    let root = pdf.root_ref().unwrap();
    let catalog = resolved_handle(&mut pdf, root);
    let acroform = resolved_key(&mut pdf, &catalog, b"/AcroForm");
    assert_direct_dictionary(&acroform, "direct AcroForm");
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"raw".to_vec()), true)
        .unwrap();
    let acroform = resolved_key(&mut pdf, &catalog, b"/AcroForm");
    assert_eq!(
        key_boolean(&mut pdf, &acroform, b"/NeedAppearances"),
        Some(true)
    );

    let bytes = doc(vec![(10, "<< /FT /Btn >>".into())]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .unwrap();
    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"Yes".as_slice())
    );
}

#[test]
fn clear_need_appearances_handles_direct_and_non_dictionary_catalog_values() {
    let bytes = doc_with_root(
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /NeedAppearances true >> >>",
        Vec::new(),
    );
    let mut pdf = open(bytes);
    let root = pdf.root_ref().unwrap();
    let catalog = resolved_handle(&mut pdf, root);
    let acroform = resolved_key(&mut pdf, &catalog, b"/AcroForm");
    assert_direct_dictionary(&acroform, "direct AcroForm");
    FormFieldObjectHelper::clear_need_appearances_after_generation(&mut pdf).unwrap();
    let acroform = resolved_key(&mut pdf, &catalog, b"/AcroForm");
    assert!(!has_entry(&acroform, b"/NeedAppearances"));

    let bytes = doc_with_root("null", vec![(10, "<< >>".into())]);
    let mut pdf = open(bytes);
    let root = pdf.root_ref().unwrap();
    assert!(resolved_handle(&mut pdf, root).is_null());
    FormFieldObjectHelper::clear_need_appearances_after_generation(&mut pdf).unwrap();
}

#[test]
fn radio_updates_preserve_all_unselectable_kid_shapes() {
    // qpdf only updates a widget whose /AP is non-null; scalar children and
    // child /Kids holders of the wrong type remain untouched.
    for kids in [
        "[42]",
        "[<< /Kids [42] >>]",
        "[<< /Kids 12 0 R >>]",
        "[<< /Kids 42 >>]",
        "[12 0 R]",
    ] {
        let bytes = doc(vec![
            (10, format!("<< /FT /Btn /Ff 32768 /Kids {kids} >>")),
            (12, "42".into()),
        ]);
        let mut pdf = open(bytes);
        FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
            .set_value(ObjectHandle::name(b"On".to_vec()), false)
            .unwrap();
        let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
        assert_eq!(
            key_name(&mut pdf, &field, b"/V").as_deref(),
            Some(b"On".as_slice())
        );
    }

    let bytes = doc(vec![
        (10, "<< /FT /Btn /Ff 32768 /Kids [12 0 R] >>".into()),
        (12, "<< >>".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .unwrap();
    let widget = resolved_handle(&mut pdf, ObjectRef::new(12, 0));
    assert!(!has_entry(&widget, b"/AS"));

    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Ff 32768 /Parent 11 0 R /Kids [12 0 R] >>".into(),
        ),
        (11, "<< /FT /Tx >>".into()),
        (12, "<< /AP << /N << /Off null /On null >> >> >>".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .unwrap();
    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(!has_entry(&field, b"/V"));
}

#[test]
fn radio_skips_an_indirect_grandchild_without_appearance() {
    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Ff 32768 /Kids [ << /Kids [11 0 R 12 0 R] >> ] >>".into(),
        ),
        (11, "<< /Subtype /Widget >>".into()),
        (12, "<< /AP << /N << /Off null /On null >> >> >>".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .expect("skip the first grandchild and update the usable widget");
    let widget = resolved_handle(&mut pdf, ObjectRef::new(12, 0));
    assert!(
        widget.as_dictionary().is_some(),
        "widget must be a dictionary"
    );
    assert_eq!(
        key_name(&mut pdf, &widget, b"/AS").as_deref(),
        Some(b"On".as_slice())
    );
}

#[test]
fn raw_value_reference_skips_null_cycles_and_non_dictionary_fields() {
    let bytes = doc(vec![
        (10, "<< /V null /Parent 11 0 R >>".into()),
        (11, "<< /V 12 0 R >>".into()),
        (12, "(value)".into()),
    ]);
    let mut pdf = open(bytes);
    assert_eq!(
        FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
            .field_value_reference()
            .unwrap(),
        Some(ObjectRef::new(12, 0))
    );

    let bytes = doc(vec![
        (10, "<< /Parent 11 0 R >>".into()),
        (11, "<< /Parent 10 0 R >>".into()),
    ]);
    let mut pdf = open(bytes);
    assert_eq!(
        FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
            .field_value_reference()
            .unwrap(),
        None
    );

    let bytes = doc(vec![(10, "null".into())]);
    let mut pdf = open(bytes);
    assert_eq!(
        FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
            .field_value_reference()
            .unwrap(),
        None
    );

    let bytes = String::from_utf8(doc(vec![(10, "<< >>".into())]))
        .unwrap()
        .replace(" /Root 1 0 R", "")
        .into_bytes();
    let mut pdf = open(bytes);
    assert!(FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .default_resources()
        .unwrap()
        .is_none());
}

#[test]
fn unknown_da_font_falls_back_without_acroform_resources() {
    let bytes = doc(vec![(
        10,
        "<< /FT /Tx /V (value) /Rect [0 0 100 20] /DA (/F1 12 Tf 0 g) >>".into(),
    )]);
    let mut pdf = open(bytes);
    assert!(FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .generate_appearance_for(ObjectRef::new(10, 0))
        .unwrap()
        .is_some());
}

#[test]
fn checkbox_uses_an_indirect_widget_appearance_annotation() {
    let bytes = doc(vec![
        (10, "<< /FT /Btn /Kids [11 0 R] >>".into()),
        (11, "<< /AP << /N << /Off null /On null >> >> >>".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .unwrap();
    let widget = resolved_handle(&mut pdf, ObjectRef::new(11, 0));
    assert_eq!(
        key_name(&mut pdf, &widget, b"/AS").as_deref(),
        Some(b"On".as_slice())
    );
}

#[test]
fn checkbox_updates_a_widget_behind_a_multi_hop_kid_holder() {
    let bytes = doc(vec![
        (10, "<< /FT /Btn /Kids [11 0 R] >>".into()),
        (11, "<< /AP << /N << /Off null /Chosen null >> >> >>".into()),
    ]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .expect("set checkbox value through widget holder");

    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"Chosen".as_slice())
    );
    let widget = resolved_handle(&mut pdf, ObjectRef::new(11, 0));
    assert_eq!(
        key_name(&mut pdf, &widget, b"/AS").as_deref(),
        Some(b"Chosen".as_slice())
    );
}

#[test]
fn set_value_dispatches_radio_and_non_button_appearance_updates() {
    let bytes = doc(vec![
        (10, "<< /FT /Btn /Ff 32768 /Kids [11 0 R 12 0 R] >>".into()),
        (11, "<< >>".into()),
        (
            12,
            "<< /AP << /N << /Off null /Selected null >> >> >>".into(),
        ),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"Selected".to_vec()), false)
        .unwrap();
    let widget = resolved_handle(&mut pdf, ObjectRef::new(12, 0));
    assert_eq!(
        key_name(&mut pdf, &widget, b"/AS").as_deref(),
        Some(b"Selected".as_slice())
    );

    let bytes = doc_with_acroform(vec![(10, "<< /FT /Tx >>".into()), (20, "<< >>".into())]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"raw-name".to_vec()), true)
        .unwrap();
    let acroform = resolved_handle(&mut pdf, ObjectRef::new(20, 0));
    assert_eq!(
        key_boolean(&mut pdf, &acroform, b"/NeedAppearances"),
        Some(true)
    );
}

#[test]
fn checkbox_selects_an_indirect_widget_after_unselectable_kids() {
    let bytes = doc(vec![
        (10, "<< /FT /Btn /Kids [11 0 R 12 0 R] >>".into()),
        (11, "<< >>".into()),
        (12, "<< /AP << /N << /Off null /On null >> >> >>".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .unwrap();
    let widget = resolved_handle(&mut pdf, ObjectRef::new(12, 0));
    assert_eq!(
        key_name(&mut pdf, &widget, b"/AS").as_deref(),
        Some(b"On".as_slice())
    );
}

#[test]
fn choices_resolve_each_indirect_item_to_its_terminal_string() {
    let bytes = doc(vec![
        (10, "<< /FT /Ch /Opt [20 0 R] >>".into()),
        (20, "(terminal)".into()),
    ]);
    let mut pdf = open(bytes);

    let choices = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .choices()
        .unwrap();

    assert_eq!(choices, vec!["terminal"]);
}

#[test]
fn get_top_level_field_stops_before_a_parent_that_resolves_to_null() {
    let bytes = doc(vec![
        (10, "<< /Parent 20 0 R >>".into()),
        (20, "null".into()),
    ]);
    let mut pdf = open(bytes);

    assert_eq!(
        FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
            .get_top_level_field()
            .unwrap(),
        (ObjectRef::new(10, 0), false)
    );
}

#[test]
fn get_top_level_field_stops_at_a_direct_parent_since_only_indirect_can_be_reported() {
    // `get_top_level_field` returns an `ObjectRef`, which cannot represent a
    // direct dictionary, so a `/Parent` value that resolves to a direct
    // dictionary (rather than an indirect reference) stops the climb at the
    // last indirect ancestor instead of erroring or continuing into it.
    let bytes = doc(vec![(10, "<< /Parent << /T (a) >> >>".into())]);
    let mut pdf = open(bytes);

    assert_eq!(
        FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
            .get_top_level_field()
            .unwrap(),
        (ObjectRef::new(10, 0), false)
    );
}

#[test]
fn set_value_mutates_the_terminal_field_dictionary_without_replacing_holders() {
    let bytes = doc(vec![(10, "<< /FT /Tx >>".into())]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::string(b"updated".to_vec()), false)
        .unwrap();

    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert_eq!(
        key_string(&mut pdf, &field, b"/V"),
        Some(flpdf::pdf_string::new_unicode_string(b"updated"))
    );
}

#[test]
fn checkbox_updates_a_direct_kid_when_the_field_is_behind_multi_hop_holders() {
    let bytes = doc(vec![(
        10,
        "<< /FT /Btn /Kids [ << /AP << /N << /Off null /Chosen null >> >> >> ] >>".into(),
    )]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .expect("set checkbox value through field holders");

    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(field.as_dictionary().is_some(), "terminal checkbox field");
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"Chosen".as_slice())
    );
    let kids = resolved_key(&mut pdf, &field, b"/Kids")
        .as_array()
        .expect("checkbox kids must stay an array");
    let widget = resolved_value(&mut pdf, kids[0].clone());
    assert_direct_dictionary(&widget, "checkbox widget");
    assert_eq!(
        key_name(&mut pdf, &widget, b"/AS").as_deref(),
        Some(b"Chosen".as_slice())
    );
}

#[test]
fn radio_updates_widgets_behind_a_multi_hop_kids_holder() {
    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Ff 32768 /Parent null /Kids 20 0 R >>".into(),
        ),
        (11, "<< /AP << /N << /Off null /First null >> >> >>".into()),
        (
            12,
            "<< /AP << /N << /Off null /Selected null >> >> >>".into(),
        ),
        (20, "[11 0 R 12 0 R]".into()),
    ]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"Selected".to_vec()), false)
        .unwrap();

    let first_widget = resolved_handle(&mut pdf, ObjectRef::new(11, 0));
    assert_eq!(
        key_name(&mut pdf, &first_widget, b"/AS").as_deref(),
        Some(b"Off".as_slice())
    );
    let second_widget = resolved_handle(&mut pdf, ObjectRef::new(12, 0));
    assert_eq!(
        key_name(&mut pdf, &second_widget, b"/AS").as_deref(),
        Some(b"Selected".as_slice())
    );
}

#[test]
fn radio_updates_a_widget_behind_a_multi_hop_child_holder() {
    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Ff 32768 /Parent null /Kids [11 0 R] >>".into(),
        ),
        (
            11,
            "<< /AP << /N << /Off null /Selected null >> >> >>".into(),
        ),
    ]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"Selected".to_vec()), false)
        .expect("set radio value through child holder");

    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"Selected".as_slice())
    );
    let widget = resolved_handle(&mut pdf, ObjectRef::new(11, 0));
    assert_eq!(
        key_name(&mut pdf, &widget, b"/AS").as_deref(),
        Some(b"Selected".as_slice())
    );
}

#[test]
fn radio_delegates_through_a_multi_hop_parent_holder() {
    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Ff 32768 /Parent 20 0 R /AP << /N << /Off null /Selected null >> >> >>"
                .into(),
        ),
        (
            20,
            "<< /FT /Btn /Ff 32768 /Parent null /Kids [10 0 R] >>".into(),
        ),
    ]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"Selected".to_vec()), false)
        .expect("delegate radio value through parent holders");

    let parent = resolved_handle(&mut pdf, ObjectRef::new(20, 0));
    assert_eq!(
        key_name(&mut pdf, &parent, b"/V").as_deref(),
        Some(b"Selected".as_slice())
    );
    let child = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert_eq!(
        key_name(&mut pdf, &child, b"/AS").as_deref(),
        Some(b"Selected".as_slice())
    );
}

#[test]
fn radio_with_a_terminal_non_radio_parent_is_a_noop() {
    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Ff 32768 /Parent 20 0 R /AP << /N << /Off null /Selected null >> >> >>"
                .into(),
        ),
        (20, "<< /FT /Tx >>".into()),
    ]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"Selected".to_vec()), false)
        .expect("ignore a radio child whose terminal parent is not radio");

    let child = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(!has_entry(&child, b"/V"));
    assert!(!has_entry(&child, b"/AS"));
}

#[test]
fn radio_with_a_non_null_parent_marker_is_a_noop() {
    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Ff 32768 /Parent 20 0 R /AP << /N << /Off null /Selected null >> >> >>"
                .into(),
        ),
        (20, "<< /FT /Btn /Ff 32768 /Parent /NotNull >>".into()),
    ]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"Selected".to_vec()), false)
        .expect("ignore a radio child whose parent's parent marker is non-null");

    let child = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert!(!has_entry(&child, b"/V"));
    assert!(!has_entry(&child, b"/AS"));
}

#[test]
fn radio_treats_a_cyclic_parent_holder_as_null() {
    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Ff 32768 /Parent 20 0 R /Kids [11 0 R] >>".into(),
        ),
        (
            11,
            "<< /AP << /N << /Off null /Selected null >> >> >>".into(),
        ),
        (20, "null".into()),
    ]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"Selected".to_vec()), false)
        .expect("set a radio value when the parent holder is cyclic");

    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"Selected".as_slice())
    );
    let widget = resolved_handle(&mut pdf, ObjectRef::new(11, 0));
    assert_eq!(
        key_name(&mut pdf, &widget, b"/AS").as_deref(),
        Some(b"Selected".as_slice())
    );
}

#[test]
fn radio_follows_a_multi_hop_nested_kids_holder() {
    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Ff 32768 /Parent null /Kids [11 0 R] >>".into(),
        ),
        (11, "<< /Kids 20 0 R >>".into()),
        (
            12,
            "<< /AP << /N << /Off null /Selected null >> >> >>".into(),
        ),
        (20, "[12 0 R]".into()),
    ]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"Selected".to_vec()), false)
        .expect("set radio value through nested kids holders");

    let widget = resolved_handle(&mut pdf, ObjectRef::new(12, 0));
    assert_eq!(
        key_name(&mut pdf, &widget, b"/AS").as_deref(),
        Some(b"Selected".as_slice())
    );
}

#[test]
fn radio_follows_a_multi_hop_nested_widget_holder() {
    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Ff 32768 /Parent null /Kids [11 0 R] >>".into(),
        ),
        (11, "<< /Kids [20 0 R] >>".into()),
        (
            20,
            "<< /AP << /N << /Off null /Selected null >> >> >>".into(),
        ),
    ]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"Selected".to_vec()), false)
        .expect("set radio value through nested widget holders");

    let widget = resolved_handle(&mut pdf, ObjectRef::new(20, 0));
    assert_eq!(
        key_name(&mut pdf, &widget, b"/AS").as_deref(),
        Some(b"Selected".as_slice())
    );
}

#[test]
fn checkbox_does_not_update_as_for_a_cyclic_appearance_holder() {
    let bytes = doc(vec![
        (10, "<< /FT /Btn /AP 20 0 R /AS /Off >>".into()),
        (20, "null".into()),
    ]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .expect("set checkbox value with cyclic appearance holder");

    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"Yes".as_slice())
    );
    assert_eq!(
        key_name(&mut pdf, &field, b"/AS").as_deref(),
        Some(b"Off".as_slice())
    );
}

#[test]
#[ignore = "subprocess-only stack-overflow regression probe"]
fn checkbox_cyclic_kids_probe() {
    assert_eq!(
        std::env::var_os("FLPDF_CHECKBOX_CYCLE_PROBE").as_deref(),
        Some(std::ffi::OsStr::new("1"))
    );
    let bytes = doc(vec![
        (10, "<< /FT /Btn /Kids 20 0 R >>".into()),
        (20, "21 0 R".into()),
        (21, "20 0 R".into()),
    ]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(ObjectHandle::name(b"On".to_vec()), false)
        .unwrap();

    let field = resolved_handle(&mut pdf, ObjectRef::new(10, 0));
    assert_eq!(
        key_name(&mut pdf, &field, b"/V").as_deref(),
        Some(b"Yes".as_slice())
    );
}

#[test]
fn checkbox_cyclic_kids_do_not_overflow_the_stack() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "checkbox_cyclic_kids_probe",
            "--ignored",
            "--nocapture",
        ])
        .env("FLPDF_CHECKBOX_CYCLE_PROBE", "1")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "cycle probe failed: status={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

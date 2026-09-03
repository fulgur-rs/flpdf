use std::ffi::OsStr;
use std::io::{Read, Seek, Write};

use flpdf::{
    AcroFormDocumentHelper, AnnotationObjectHelper, Error, FormFieldObjectHelper, Matrix, NameTree,
    NumberTree, ObjectHandle, ObjectHandleMatrix, OutlineDocumentHelper, PageDocumentHelper,
    PageLabelDocumentHelper, Pdf, PdfWriter, Rectangle,
};

use super::{emit_new_diagnostics, format_nntree_exception};
use crate::output::write_bytes;

// Shared helpers for test_46/test_48 (qpdf's number-tree/name-tree driver
// tests) and test_47/test_49 (page-label/outline document-helper tests).

/// Read a name/number tree value through its canonical `ObjectHandle` route.
/// qpdf's tree iterators keep the value handle live until the consumer asks
/// for a typed value, so this helper resolves the handle once and applies the
/// empty-string fallback used by the corresponding qpdf accessor.
fn tree_string_value<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: &ObjectHandle,
) -> flpdf::Result<Vec<u8>> {
    pdf.resolve(value)?;
    Ok(value.as_string().unwrap_or_default())
}

/// Resolve `handle`, then read `key` from it — `ObjectHandle::get_key`
/// never resolves on its own (`object_handle.rs:2769-2787`), matching
/// `QPDFObjectHandle::getKey`'s own internal `dereference()` call
/// (`libqpdf/QPDFObjectHandle.cc:979-990`). Returns the resolved child, so
/// chaining two calls resolves at every hop the way qpdf's own
/// `a.getKey("/X").getKey("/Y")` chase does.
fn chase_key<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
    key: &[u8],
) -> flpdf::Result<ObjectHandle> {
    pdf.resolve(handle)?;
    let child = handle.get_key(key);
    pdf.resolve(&child)?;
    Ok(child)
}

pub(crate) fn run_test_42<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 qpdf/test_driver.cc:1407-1549. Keep this as a thin
    // consumer of ObjectHandle's qpdf-shaped accessors: warning text is
    // produced by the handle's document resolver and drained here at the
    // same boundaries as qpdf's logger. qpdf's C++ test binds `auto&` to the
    // iterator's internal `ivalue`; this Rust API returns ObjectHandle values,
    // so copied handles stay stable and later positions use fresh `current()`
    // calls.
    let qtest = pdf.trailer_key_handle(b"QTest");
    pdf.resolve(&qtest)?;
    let qtest = qtest.clone();
    let dictionary = qtest.get_key(b"/Dictionary");
    pdf.resolve(&dictionary)?;
    let dictionary = dictionary.clone();
    let key2 = dictionary.get_key(b"/Key2");
    pdf.resolve(&key2)?;
    let array = key2.clone();
    let integer = qtest.get_key(b"/Integer");
    pdf.resolve(&integer)?;

    assert!(array.try_is_array()?);
    {
        let items = array.try_array_items()?;
        let mut cursor = items.begin();
        let i_value = cursor.current();
        assert_eq!(i_value.try_get_name()?, b"/Item0");
        cursor.previous();
        assert_eq!(i_value.try_get_name()?, b"/Item0");
        cursor.next();
        cursor.next();
        cursor.next();
        assert!(cursor.is_end());
        cursor.next();
        assert!(cursor.is_end());
        assert!(!cursor.current().is_initialized());
        assert!(i_value.is_initialized());
        assert_eq!(i_value.try_get_name()?, b"/Item0");
        cursor.previous();
        assert_eq!(cursor.current().try_get_name()?, b"/Item2");
        assert_eq!(i_value.try_get_name()?, b"/Item0");
        assert_eq!(cursor.current().try_get_name()?, b"/Item2");
    }

    assert!(dictionary.try_is_dictionary()?);
    {
        let items = dictionary.try_dict_items()?;
        let mut cursor = items.begin();
        let entry = cursor.current();
        assert_eq!(entry.key, b"/Key1");
        assert_eq!(entry.value.try_get_name()?, b"/Value1");
        cursor.next();
        cursor.next();
        assert!(cursor.is_end());
        assert!(!cursor.current().value.is_initialized());
        assert!(entry.value.is_initialized());
        assert_eq!(entry.value.try_get_name()?, b"/Value1");
    }

    qtest.try_get_string_value()?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(array.try_get_array_item(-1)?.is_null());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(array.try_get_array_item(16_059)?.is_null());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(integer.try_get_array_item(0)?.is_null());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    integer.try_append_array_item(ObjectHandle::null())?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    array.try_erase_array_item_at(-1)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    array.try_erase_array_item_at(16_059)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    array.try_insert_array_item_at(42, ObjectHandle::name(b"Dontpanic".to_vec()))?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    array.try_set_array_item_at(42, ObjectHandle::name(b"Dontpanic".to_vec()))?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    integer.try_erase_array_item_at(0)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    integer.try_insert_array_item_at(0, ObjectHandle::null())?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    integer.try_set_array_items(Vec::new())?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    integer.try_set_array_item_at(0, ObjectHandle::null())?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert_eq!(integer.try_get_array_n_items()?, 0);
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(integer.try_get_array_as_vector()?.is_empty());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(!integer.try_get_bool_value()?);
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(integer.try_get_dict_as_map()?.is_empty());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(integer.try_get_keys()?.is_empty());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(!integer.try_get_has_key(b"/Potato")?);
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    integer.remove_key_and_get_old(b"/Potato")?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    integer.replace_key(b"/Potato", ObjectHandle::null())?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    integer.replace_key(b"/Potato", ObjectHandle::integer(1))?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(ObjectHandle::null()
        .try_get_key_if_dict(b"/Integer")?
        .try_get_key_if_dict(b"/Potato")?
        .is_null());

    let integer_from_qtest = qtest.try_get_key(b"/Integer")?;
    integer_from_qtest.try_get_key_if_dict(b"/Potato")?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    integer_from_qtest.try_get_key(b"/Potato")?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(integer.try_get_inline_image_value()?.is_empty());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert_eq!(dictionary.try_get_int_value()?, 0);
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert_eq!(integer.try_get_name()?, b"/QPDFFakeName");
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert_eq!(integer.try_get_operator_value()?, b"QPDFFAKE");
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert_eq!(dictionary.try_get_real_value()?, b"0.0");
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(integer.try_get_string_value()?.is_empty());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(integer.try_get_utf8_value()?.is_empty());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert_eq!(dictionary.try_get_numeric_value()?, 0.0);
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    writeln!(stderr, "One error")?;
    assert!(array
        .try_get_array_item(0)?
        .try_get_string_value()?
        .is_empty());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    writeln!(stderr, "One error")?;
    assert!(dictionary
        .try_get_key(b"/Quack")?
        .try_get_string_value()?
        .is_empty());
    assert!(dictionary
        .try_get_key_if_dict(b"/Quack")?
        .try_get_string_value()?
        .is_empty());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let nested_dictionary = array.try_get_array_item(1)?;
    assert!(nested_dictionary.try_is_dictionary()?);
    let nested_array = nested_dictionary.try_get_key(b"/K")?;
    assert!(nested_array.try_is_array()?);
    let nested_name = nested_array.try_get_array_item(0)?;
    assert!(nested_name.try_is_name()?);
    assert_eq!(nested_name.try_get_name()?, b"/V");

    writeln!(stderr, "Two errors")?;
    let invalid_item = array.try_get_array_item(16_059)?;
    assert!(invalid_item.try_get_string_value()?.is_empty());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    writeln!(stderr, "One error")?;
    array
        .try_get_array_item(1)?
        .try_get_key(b"/K")?
        .try_get_array_item(0)?
        .try_get_string_value()?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    let page_ref = PageDocumentHelper::new(pdf)
        .get_all_pages()?
        .into_iter()
        .next()
        .expect("qpdf test_42 requires one page");
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page)?;
    let contents = page.try_get_key(b"/Contents")?;
    pdf.resolve(&contents)?;
    let stream_dictionary = contents
        .as_stream_dict()
        .expect("qpdf test_42 requires a stream contents object");
    pdf.resolve(&stream_dictionary)?;
    assert_eq!(
        stream_dictionary.try_get_key(b"/Potato")?.try_get_name()?,
        b"/QPDFFakeName"
    );
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    assert_eq!(integer.try_get_array_as_rectangle()?, Rectangle::default());
    let rectangle = ObjectHandle::new_from_rectangle(Rectangle::new(1.2, 3.4, 5.6, 7.8));
    let rectangle_value = rectangle.try_get_array_as_rectangle()?;
    assert!(rectangle.try_is_rectangle()?);
    assert!(rectangle_value.llx > 1.19 && rectangle_value.llx < 1.21);
    assert!(rectangle_value.lly > 3.39 && rectangle_value.lly < 3.41);
    assert!(rectangle_value.urx > 5.59 && rectangle_value.urx < 5.61);
    assert!(rectangle_value.ury > 7.79 && rectangle_value.ury < 7.81);
    for input in [b"[1 2 3 4 5]".as_slice(), b"[1 2 3]", b"[1 2 false 4]"] {
        let value = ObjectHandle::parse(input)?;
        assert!(!value.try_is_rectangle()?);
        assert_eq!(value.try_get_array_as_rectangle()?, Rectangle::default());
    }

    let matrix =
        ObjectHandle::new_from_matrix(ObjectHandleMatrix::new(1.2, 3.4, 5.6, 7.8, 9.1, 2.3));
    let matrix_value = matrix.try_get_array_as_matrix()?;
    assert!(matrix.try_is_matrix()?);
    assert!(matrix_value.a > 1.19 && matrix_value.a < 1.21);
    assert!(matrix_value.b > 3.39 && matrix_value.b < 3.41);
    assert!(matrix_value.c > 5.59 && matrix_value.c < 5.61);
    assert!(matrix_value.d > 7.79 && matrix_value.d < 7.81);
    assert!(matrix_value.e > 9.09 && matrix_value.e < 9.11);
    assert!(matrix_value.f > 2.29 && matrix_value.f < 2.31);
    let qpdf_matrix = ObjectHandle::new_from_qpdf_matrix(Matrix::new(1.2, 3.4, 5.6, 7.8, 9.1, 2.3));
    let qpdf_matrix_value = qpdf_matrix.try_get_array_as_matrix()?;
    assert!(qpdf_matrix.try_is_matrix()?);
    assert!(qpdf_matrix_value.a > 1.19 && qpdf_matrix_value.a < 1.21);
    assert!(qpdf_matrix_value.b > 3.39 && qpdf_matrix_value.b < 3.41);
    assert!(qpdf_matrix_value.c > 5.59 && qpdf_matrix_value.c < 5.61);
    assert!(qpdf_matrix_value.d > 7.79 && qpdf_matrix_value.d < 7.81);
    assert!(qpdf_matrix_value.e > 9.09 && qpdf_matrix_value.e < 9.11);
    assert!(qpdf_matrix_value.f > 2.29 && qpdf_matrix_value.f < 2.31);
    for input in [
        b"[1 2 3 4 5]".as_slice(),
        b"[1 2 3 4 5 6 7]",
        b"[1 2 3 false 5 6 7]",
        b"42",
    ] {
        let value = ObjectHandle::parse(input)?;
        assert!(!value.try_is_matrix()?);
        assert_eq!(
            value.try_get_array_as_matrix()?,
            ObjectHandleMatrix::default()
        );
    }

    let uninitialized = ObjectHandle::uninitialized();
    assert!(!uninitialized.is_initialized());
    assert!(!uninitialized.try_is_integer()?);
    assert!(!uninitialized.try_is_dictionary()?);
    assert!(!uninitialized.try_is_scalar()?);
    Ok(())
}

pub(crate) fn run_test_43<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 qpdf/test_driver.cc:1551-1609.
    // Constructing the helper performs qpdf's eager analyze() pass, so drain
    // its warnings before emitting the first test line just as qpdf's logger
    // does before the field loop.
    let (has_acroform, fields) = {
        let mut acroform = AcroFormDocumentHelper::new(pdf)?;
        let has_acroform = acroform.has_acro_form()?;
        let fields = if has_acroform {
            acroform.get_form_fields()?
        } else {
            Vec::new()
        };
        (has_acroform, fields)
    };
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    if !has_acroform {
        writeln!(stdout, "no forms")?;
        return Ok(());
    }

    writeln!(stdout, "iterating over form fields")?;
    for field in fields {
        write!(stdout, "Field: ")?;
        write_bytes(stdout, &field.unparse())?;
        writeln!(stdout)?;

        let mut node = field.clone();
        while !node.is_null() {
            let parent = FormFieldObjectHelper::from_object_handle(node, pdf).get_parent()?;
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            if parent.is_null() {
                writeln!(stdout, "  Parent: none")?;
                break;
            }
            write!(stdout, "  Parent: ")?;
            write_bytes(stdout, &parent.unparse())?;
            writeln!(stdout)?;
            node = parent;
        }

        let fully_qualified_name = {
            let mut field_helper = FormFieldObjectHelper::from_object_handle(field.clone(), pdf);
            field_helper.fully_qualified_name()?
        };
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        writeln!(stdout, "  Fully qualified name: {fully_qualified_name}")?;

        let partial_name = {
            let mut field_helper = FormFieldObjectHelper::from_object_handle(field.clone(), pdf);
            field_helper.partial_name()?
        };
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        writeln!(stdout, "  Partial name: {partial_name}")?;

        let alternative_name = {
            let mut field_helper = FormFieldObjectHelper::from_object_handle(field.clone(), pdf);
            field_helper.alternative_name()?
        };
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        writeln!(stdout, "  Alternative name: {alternative_name}")?;

        let mapping_name = {
            let mut field_helper = FormFieldObjectHelper::from_object_handle(field.clone(), pdf);
            field_helper.mapping_name()?
        };
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        writeln!(stdout, "  Mapping name: {mapping_name}")?;

        let field_type = {
            let mut field_helper = FormFieldObjectHelper::from_object_handle(field.clone(), pdf);
            field_helper.field_type()?.unwrap_or_default()
        };
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        write!(stdout, "  Field type: ")?;
        write_bytes(stdout, &field_type)?;
        writeln!(stdout)?;

        let value = {
            let mut field_helper = FormFieldObjectHelper::from_object_handle(field.clone(), pdf);
            field_helper.value()?.unwrap_or_else(ObjectHandle::null)
        };
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        write!(stdout, "  Value: ")?;
        write_bytes(stdout, &value.unparse())?;
        writeln!(stdout)?;

        let value_as_string = {
            let mut field_helper = FormFieldObjectHelper::from_object_handle(field.clone(), pdf);
            field_helper.value_as_string()?
        };
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        writeln!(stdout, "  Value as string: {value_as_string}")?;

        let default_value = {
            let mut field_helper = FormFieldObjectHelper::from_object_handle(field.clone(), pdf);
            field_helper
                .default_value()?
                .unwrap_or_else(ObjectHandle::null)
        };
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        write!(stdout, "  Default value: ")?;
        write_bytes(stdout, &default_value.unparse())?;
        writeln!(stdout)?;

        let default_value_as_string = {
            let mut field_helper = FormFieldObjectHelper::from_object_handle(field.clone(), pdf);
            field_helper.default_value_as_string()?
        };
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        writeln!(
            stdout,
            "  Default value as string: {default_value_as_string}"
        )?;

        let default_appearance = {
            let mut field_helper = FormFieldObjectHelper::from_object_handle(field.clone(), pdf);
            field_helper.default_appearance()?
        };
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        writeln!(stdout, "  Default appearance: {default_appearance}")?;

        let quadding = {
            let mut field_helper = FormFieldObjectHelper::from_object_handle(field.clone(), pdf);
            field_helper.quadding()?
        };
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        writeln!(stdout, "  Quadding: {quadding}")?;

        let annotations = {
            let mut acroform = AcroFormDocumentHelper::new(pdf)?;
            acroform.get_annotations_for_field(field.clone())?
        };
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

        for annotation in annotations {
            write!(stdout, "  Annotation: ")?;
            write_bytes(stdout, &annotation.unparse())?;
            writeln!(stdout)?;
        }
    }

    writeln!(stdout, "iterating over annotations per page")?;
    let pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    for page_ref in pages {
        let page = pdf.get_object_handle(page_ref);
        write!(stdout, "Page: ")?;
        write_bytes(stdout, &page.unparse())?;
        writeln!(stdout)?;

        let annotations = {
            let mut acroform = AcroFormDocumentHelper::new(pdf)?;
            acroform.get_widget_annotations_for_page(page_ref)?
        };
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        for annotation in annotations {
            write!(stdout, "  Annotation: ")?;
            write_bytes(stdout, &annotation.unparse())?;
            writeln!(stdout)?;

            let field = {
                let mut acroform = AcroFormDocumentHelper::new(pdf)?;
                acroform.get_field_for_annotation_handle(annotation.clone())?
            };
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            write!(stdout, "    Field: ")?;
            write_bytes(stdout, &field.unparse())?;
            writeln!(stdout)?;

            let subtype = {
                let mut annotation_helper =
                    AnnotationObjectHelper::from_object_handle(annotation.clone(), pdf);
                annotation_helper.get_subtype()?
            };
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            write!(stdout, "    Subtype: /")?;
            write_bytes(stdout, &subtype)?;
            writeln!(stdout)?;

            let rect = {
                let mut annotation_helper =
                    AnnotationObjectHelper::from_object_handle(annotation.clone(), pdf);
                annotation_helper.get_rect()?
            };
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            writeln!(
                stdout,
                "    Rect: [{}, {}, {}, {}]",
                rect.llx, rect.lly, rect.urx, rect.ury
            )?;

            let state = {
                let mut annotation_helper =
                    AnnotationObjectHelper::from_object_handle(annotation.clone(), pdf);
                annotation_helper.get_appearance_state()?
            };
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            if !state.is_empty() {
                write!(stdout, "    Appearance state: /")?;
                write_bytes(stdout, &state)?;
                writeln!(stdout)?;
            }

            let normal_appearance = {
                let mut annotation_helper =
                    AnnotationObjectHelper::from_object_handle(annotation.clone(), pdf);
                annotation_helper.get_appearance_stream(b"N", None)?
            };
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            write!(stdout, "    Appearance stream (/N): ")?;
            write_bytes(stdout, &normal_appearance.unparse())?;
            writeln!(stdout)?;

            let state_appearance = {
                let mut annotation_helper =
                    AnnotationObjectHelper::from_object_handle(annotation, pdf);
                annotation_helper.get_appearance_stream(b"N", Some(b"3"))?
            };
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            write!(stdout, "    Appearance stream (/N, /3): ")?;
            write_bytes(stdout, &state_appearance.unparse())?;
            writeln!(stdout)?;
        }
    }
    Ok(())
}

pub(crate) fn run_test_44<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 qpdf/test_driver.cc:1611-1629.
    let fields = {
        let mut acroform = AcroFormDocumentHelper::new(pdf)?;
        acroform.get_form_fields()?
    };
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    for field in fields {
        let mut field_helper = FormFieldObjectHelper::from_object_handle(field, pdf);
        if field_helper.field_type()?.as_deref() == Some(b"/Tx") {
            field_helper.set_value_string("3.14 ÷ 0", true)?;
            writeln!(
                stdout,
                "Set field value: {} -> {}",
                field_helper.fully_qualified_name()?,
                field_helper.value_as_string()?
            )?;
        }
    }

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_qdf_mode(true);
    writer.set_static_id(true);
    writer.set_suppress_original_object_ids(true);
    writer.write()?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    Ok(())
}

pub(crate) fn run_test_45<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 qpdf/test_driver.cc:1631-1643.
    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_static_id(true);
    writer.write()?;

    // GAP(QPDF::getWarnings): qpdf's `pdf.getWarnings()` returns every
    // `QPDFExc` accumulated in `m->warnings` across the `QPDF` instance's
    // whole lifetime, including ones `QPDFWriter::write` raises through
    // `pipeStreamData`'s warn callback while copying stream data. flpdf's
    // writer (`crates/flpdf/src/writer.rs`, `writer/*.rs`) never calls
    // `Pdf::push_warning` (confirmed by grep: no hits in either), so
    // `Pdf::repair_diagnostics()` -- the crate's `m->warnings`-equivalent
    // sink, also fed by `nntree.rs`/`object_copy.rs` outside repair --
    // reflects only open-time diagnostics here, not any write-time ones a
    // real qpdf run against an obfuscated file could add. There is no
    // accessor with qpdf's full-lifecycle coverage, so the `exit(3)` gate
    // is skipped.
    Ok(())
}

pub(crate) fn run_test_46<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 qpdf/test_driver.cc:1645-1782. Crafted for
    // number-tree.pdf. `NumberTree`/`NumberTreeCursor` (`nntree.rs`) are a
    // direct qpdf-compatible port of `QPDFNumberTreeObjectHelper`/its
    // `iterator`: advancing an end cursor selects the first entry and
    // moving one backward selects the last, matching qpdf's own wrap
    // behavior (`NumberTreeCursor::next`/`::previous` doc comments).
    let qtest = pdf.trailer_key_handle(b"QTest");
    let mut ntoh = NumberTree::new(qtest, true);

    let mut cursor = ntoh.begin(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    while let Some((key, value)) = cursor.current() {
        let text = tree_string_value(pdf, &value)?;
        write!(stdout, "{key} ")?;
        write_bytes(stdout, &text)?;
        writeln!(stdout)?;
        cursor.next(&mut ntoh, pdf)?;
    }

    let ntoh_map = ntoh.as_map(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    for (key, value) in &ntoh_map {
        let text = tree_string_value(pdf, value)?;
        write!(stdout, "{key} ")?;
        write_bytes(stdout, &text)?;
        writeln!(stdout)?;
    }

    assert_eq!(1, ntoh.min(pdf)?);
    assert_eq!(29, ntoh.max(pdf)?);
    assert!(ntoh.has_index(pdf, 6)?);
    assert!(!ntoh.has_index(pdf, 500)?);
    assert!(ntoh.find_object(pdf, 4)?.is_none());
    let three = ntoh.find_object(pdf, 3)?.expect("index 3 present");
    assert_eq!(tree_string_value(pdf, &three)?, b"three");
    assert!(ntoh.find_object_at_or_below(pdf, 0)?.is_none());
    let (six, offset) = ntoh
        .find_object_at_or_below(pdf, 8)?
        .expect("index at or below 8 present");
    assert_eq!(tree_string_value(pdf, &six)?, b"six");
    assert_eq!(2, offset);

    let mut new1 = NumberTree::new_empty(pdf, true)?;
    let mut iter1 = new1.begin(pdf)?;
    assert!(iter1 == new1.end());
    iter1.next(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    iter1.previous(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    new1.insert(pdf, 1, ObjectHandle::string(b"1".to_vec()))?;
    iter1.next(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 1").0, 1);
    // qpdf's `auto& iter1_val = *iter1;` aliases the iterator's own current
    // value in place -- `NNTreeIterator::operator*` returns a reference
    // into a member the iterator itself updates on every subsequent move
    // (`libqpdf/NNTree.cc`), so `iter1_val` is never a frozen snapshot: it
    // is always the same observation as `iter1.current()` at whatever point
    // it is read. Every later `iter1_val.*` assertion below is therefore
    // ported as a repeated `iter1.current()` read rather than a separate
    // value.
    iter1.previous(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    iter1.previous(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 1").0, 1);
    new1.insert(pdf, 2, ObjectHandle::string(b"2".to_vec()))?;
    iter1.next(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 2").0, 2);
    iter1.next(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    assert!(iter1.current().is_none());
    iter1.next(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 1").0, 1);
    iter1.previous(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    iter1.previous(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 2").0, 2);

    writeln!(stdout, "insertAfter")?;
    let mut new2 = NumberTree::new_empty(pdf, true)?;
    let mut iter2 = new2.begin(pdf)?;
    assert!(iter2 == new2.end());
    iter2.insert_after(&mut new2, pdf, 3, ObjectHandle::string(b"3!".to_vec()))?;
    assert_eq!(iter2.current().expect("cursor at 3").0, 3);
    iter2.insert_after(&mut new2, pdf, 4, ObjectHandle::string(b"4!".to_vec()))?;
    assert_eq!(iter2.current().expect("cursor at 4").0, 4);
    let mut cursor = new2.begin(pdf)?;
    while let Some((key, value)) = cursor.current() {
        write!(stdout, "{key} ")?;
        write_bytes(stdout, &value.unparse())?;
        writeln!(stdout)?;
        cursor.next(&mut new2, pdf)?;
    }

    writeln!(stdout, "/Bad1")?;
    let mut bad1 = NumberTree::new(pdf.trailer_key_handle(b"Bad1"), true);
    let bad1_begin = bad1.begin(pdf)?;
    assert!(bad1_begin == bad1.end());
    let bad1_last = bad1.last(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(bad1_last == bad1.end());

    writeln!(stdout, "/Bad2")?;
    let mut bad2 = NumberTree::new(pdf.trailer_key_handle(b"Bad2"), true);
    let mut cursor = bad2.begin(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    while let Some((key, value)) = cursor.current() {
        write!(stdout, "{key} ")?;
        write_bytes(stdout, &value.unparse())?;
        writeln!(stdout)?;
        cursor.next(&mut bad2, pdf)?;
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    }

    for key in [&b"Empty1"[..], &b"Empty2"[..]] {
        write!(stdout, "/")?;
        write_bytes(stdout, key)?;
        writeln!(stdout)?;
        let mut empty = NumberTree::new(pdf.trailer_key_handle(key), true);
        let empty_begin = empty.begin(pdf)?;
        assert!(empty_begin == empty.end());
        let empty_last = empty.last(pdf)?;
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        assert!(empty_last == empty.end());

        let inserted = empty.insert(pdf, 5, ObjectHandle::string(b"5".to_vec()))?;
        let (inserted_key, inserted_value) = inserted.current().expect("index 5 present");
        assert_eq!(inserted_key, 5);
        assert_eq!(tree_string_value(pdf, &inserted_value)?, b"5");
        assert_eq!(empty.begin(pdf)?.current().expect("begin at 5").0, 5);
        assert_eq!(empty.last(pdf)?.current().expect("last at 5").0, 5);
        let begin_value = empty.begin(pdf)?.current().expect("begin value at 5").1;
        assert_eq!(tree_string_value(pdf, &begin_value)?, b"5");

        let inserted = empty.insert(pdf, 5, ObjectHandle::string(b"5+".to_vec()))?;
        let (inserted_key, inserted_value) = inserted.current().expect("index 5 present");
        assert_eq!(inserted_key, 5);
        assert_eq!(tree_string_value(pdf, &inserted_value)?, b"5+");
        let begin_value = empty.begin(pdf)?.current().expect("begin value at 5+").1;
        assert_eq!(tree_string_value(pdf, &begin_value)?, b"5+");

        let inserted = empty.insert(pdf, 6, ObjectHandle::string(b"6".to_vec()))?;
        let (inserted_key, inserted_value) = inserted.current().expect("index 6 present");
        assert_eq!(inserted_key, 6);
        assert_eq!(tree_string_value(pdf, &inserted_value)?, b"6");
        let begin_value = empty.begin(pdf)?.current().expect("begin still at 5+").1;
        assert_eq!(tree_string_value(pdf, &begin_value)?, b"5+");
        assert_eq!(empty.last(pdf)?.current().expect("last at 6").0, 6);
        let last_value = empty.last(pdf)?.current().expect("last value at 6").1;
        assert_eq!(tree_string_value(pdf, &last_value)?, b"6");
    }

    writeln!(stdout, "Insert into invalid")?;
    let mut invalid1 = NumberTree::new(ObjectHandle::dictionary(Vec::new()), true);
    let invalid_insert = invalid1.insert(pdf, 1, ObjectHandle::null());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    if let Err(error) = invalid_insert {
        write_nntree_error(stdout, filename, &error)?;
    }

    writeln!(stdout, "/Bad3, no repair")?;
    let bad3_object = pdf.trailer_key_handle(b"Bad3");
    let mut bad3 = NumberTree::new(bad3_object.clone(), false);
    let mut cursor = bad3.begin(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    while let Some((key, value)) = cursor.current() {
        write!(stdout, "{key} ")?;
        write_bytes(stdout, &value.unparse())?;
        writeln!(stdout)?;
        cursor.next(&mut bad3, pdf)?;
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    }
    assert!(!kids_item_0_is_indirect(pdf, &bad3_object)?);

    writeln!(stdout, "/Bad3, repair")?;
    let mut bad3 = NumberTree::new(bad3_object.clone(), true);
    let mut cursor = bad3.begin(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    while let Some((key, value)) = cursor.current() {
        write!(stdout, "{key} ")?;
        write_bytes(stdout, &value.unparse())?;
        writeln!(stdout)?;
        cursor.next(&mut bad3, pdf)?;
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    }
    assert!(kids_item_0_is_indirect(pdf, &bad3_object)?);

    writeln!(stdout, "/Bad4 -- missing limits")?;
    let mut bad4 = NumberTree::new(pdf.trailer_key_handle(b"Bad4"), true);
    bad4.insert(pdf, 5, ObjectHandle::string(b"5".to_vec()))?;
    let mut cursor = bad4.begin(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    while let Some((key, value)) = cursor.current() {
        write!(stdout, "{key} ")?;
        write_bytes(stdout, &value.unparse())?;
        writeln!(stdout)?;
        cursor.next(&mut bad4, pdf)?;
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    }

    writeln!(stdout, "/Bad5 -- limit errors")?;
    let mut bad5 = NumberTree::new(pdf.trailer_key_handle(b"Bad5"), true);
    let found = bad5.find(pdf, 10, false)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(found == bad5.end());

    Ok(())
}

fn write_nntree_error(
    stdout: &mut dyn Write,
    filename: &[u8],
    error: &Error,
) -> std::io::Result<()> {
    if let Error::Parse { message, .. } = error {
        if let Some(exception) = format_nntree_exception(filename, message) {
            stdout.write_all(&exception)?;
            stdout.write_all(b"\n")?;
            return Ok(());
        }
    }
    writeln!(stdout, "{error}")
}

/// Whether the root's `/Kids` array's first item is stored indirectly --
/// qpdf's `bad3_oh.getKey("/Kids").getArrayItem(0).isIndirect()`.
fn kids_item_0_is_indirect<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    root: &ObjectHandle,
) -> flpdf::Result<bool> {
    pdf.resolve(root)?;
    let root = root.clone();
    let Some(dict) = root.as_dictionary() else {
        return Ok(false);
    };
    let Some(kids) = dict.get(b"/Kids".as_slice()) else {
        return Ok(false);
    };
    pdf.resolve(kids)?;
    let kids = kids.clone();
    let Some(items) = kids.as_array() else {
        return Ok(false);
    };
    Ok(items.first().is_some_and(ObjectHandle::is_indirect))
}

pub(crate) fn run_test_47<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 qpdf/test_driver.cc:1784-1796.
    let root_handle = match pdf.root_ref() {
        Some(root_ref) => pdf.get_object_handle(root_ref),
        None => ObjectHandle::null(),
    };
    let pages_handle = chase_key(pdf, &root_handle, b"/Pages")?;
    let count_handle = chase_key(pdf, &pages_handle, b"/Count")?;
    let npages = count_handle.as_integer().unwrap_or(0);
    let mut labels = Vec::new();
    // qpdf's `npages - 1` is `long long` arithmetic with no underflow guard;
    // `checked_sub` falls back to the same "empty inclusive range" shape
    // (`end < start`) an `npages == 0` document would already produce there.
    let end_idx = npages.checked_sub(1).unwrap_or(-1);
    PageLabelDocumentHelper::new(pdf).get_labels_for_page_range(0, end_idx, 1, &mut labels)?;
    // qpdf's `labels` is a flat `[idx0, dict0, idx1, dict1, ...]` vector
    // (hence its `labels.size() % 2 == 0` assertion); flpdf's
    // `get_labels_for_page_range` returns the same content already paired
    // as `Vec<(i64, ObjectHandle)>`, so the parity check has no Rust
    // analogue to port -- it is tautologically true of the pair type.
    for (index, label) in &labels {
        write!(stdout, "{index} ")?;
        write_bytes(stdout, &label.unparse())?;
        writeln!(stdout)?;
    }
    Ok(())
}

pub(crate) fn run_test_48<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 qpdf/test_driver.cc:1798-1921. Crafted for name-tree.pdf.
    // `NameTree`/`NameTreeCursor` (`nntree.rs`) mirror
    // `QPDFNameTreeObjectHelper`/its `iterator` exactly as `NumberTree` does
    // for `QPDFNumberTreeObjectHelper` in test_46 -- see that function's
    // header comment for the shared iterator-wrap and value-aliasing notes,
    // which apply identically here.
    let qtest = pdf.trailer_key_handle(b"QTest");
    let mut ntoh = NameTree::new(qtest, true);

    let mut cursor = ntoh.begin(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    while let Some((key, value)) = cursor.current() {
        write_bytes(stdout, &key)?;
        write!(stdout, " -> ")?;
        let text = tree_string_value(pdf, &value)?;
        write_bytes(stdout, &text)?;
        writeln!(stdout)?;
        cursor.next(&mut ntoh, pdf)?;
    }

    let ntoh_map = ntoh.as_map(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    for (key, value) in &ntoh_map {
        write_bytes(stdout, key)?;
        write!(stdout, " -> ")?;
        let text = tree_string_value(pdf, value)?;
        write_bytes(stdout, &text)?;
        writeln!(stdout)?;
    }

    assert!(ntoh.has_name(pdf, "11 elephant")?);
    assert!(ntoh.has_name(pdf, "07 sev\u{2022}n")?);
    assert!(!ntoh.has_name(pdf, "potato")?);
    assert!(ntoh.find_object(pdf, "potato")?.is_none());
    let seven = ntoh
        .find_object(pdf, "07 sev\u{2022}n")?
        .expect("07 sev*n present");
    assert_eq!(tree_string_value(pdf, &seven)?, b"seven!");
    let (last_key, last_value) = ntoh
        .last(pdf)?
        .current()
        .expect("name tree has a last entry");
    assert_eq!(last_key, b"29 twenty-nine");
    pdf.resolve(&last_value)?;
    let last_raw = last_value.as_string().unwrap_or_default();
    assert_eq!(flpdf::pdf_string::utf8_value(&last_raw), b"twenty-nine!");

    let mut new1 = NameTree::new_empty(pdf, true)?;
    let mut iter1 = new1.begin(pdf)?;
    assert!(iter1 == new1.end());
    iter1.next(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    iter1.previous(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    new1.insert(pdf, "1", ObjectHandle::string(b"1".to_vec()))?;
    iter1.next(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 1").0, b"1");
    // See test_46's header comment: `iter1_val` is a live alias to
    // `iter1`'s own current value, so every subsequent `iter1_val.*`
    // assertion below is ported as a repeated `iter1.current()` read.
    iter1.previous(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    iter1.previous(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 1").0, b"1");
    new1.insert(pdf, "2", ObjectHandle::string(b"2".to_vec()))?;
    iter1.next(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 2").0, b"2");
    iter1.next(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    assert!(iter1.current().is_none());
    iter1.next(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 1").0, b"1");
    iter1.previous(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    iter1.previous(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 2").0, b"2");

    writeln!(stdout, "insertAfter")?;
    let mut new2 = NameTree::new_empty(pdf, true)?;
    let mut iter2 = new2.begin(pdf)?;
    assert!(iter2 == new2.end());
    iter2.insert_after(&mut new2, pdf, "3", ObjectHandle::string(b"3!".to_vec()))?;
    assert_eq!(iter2.current().expect("cursor at 3").0, b"3");
    iter2.insert_after(&mut new2, pdf, "4", ObjectHandle::string(b"4!".to_vec()))?;
    assert_eq!(iter2.current().expect("cursor at 4").0, b"4");
    let mut cursor = new2.begin(pdf)?;
    while let Some((key, value)) = cursor.current() {
        write_bytes(stdout, &key)?;
        write!(stdout, " ")?;
        write_bytes(stdout, &value.unparse())?;
        writeln!(stdout)?;
        cursor.next(&mut new2, pdf)?;
    }

    for key in [&b"Empty1"[..], &b"Empty2"[..]] {
        write!(stdout, "/")?;
        write_bytes(stdout, key)?;
        writeln!(stdout)?;
        let mut empty = NameTree::new(pdf.trailer_key_handle(key), true);
        let empty_begin = empty.begin(pdf)?;
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        assert!(empty_begin == empty.end());
        assert!(empty.last(pdf)? == empty.end());

        let inserted = empty.insert(pdf, "five", ObjectHandle::string(b"5".to_vec()))?;
        let (inserted_key, inserted_value) = inserted.current().expect("key five present");
        assert_eq!(inserted_key, b"five");
        assert_eq!(tree_string_value(pdf, &inserted_value)?, b"5");
        assert_eq!(
            empty.begin(pdf)?.current().expect("begin at five").0,
            b"five"
        );
        assert_eq!(empty.last(pdf)?.current().expect("last at five").0, b"five");
        let begin_value = empty.begin(pdf)?.current().expect("begin value at five").1;
        assert_eq!(tree_string_value(pdf, &begin_value)?, b"5");

        let inserted = empty.insert(pdf, "five", ObjectHandle::string(b"5+".to_vec()))?;
        let (inserted_key, inserted_value) = inserted.current().expect("key five present");
        assert_eq!(inserted_key, b"five");
        assert_eq!(tree_string_value(pdf, &inserted_value)?, b"5+");
        let begin_value = empty.begin(pdf)?.current().expect("begin value at 5+").1;
        assert_eq!(tree_string_value(pdf, &begin_value)?, b"5+");

        let inserted = empty.insert(pdf, "six", ObjectHandle::string(b"6".to_vec()))?;
        let (inserted_key, inserted_value) = inserted.current().expect("key six present");
        assert_eq!(inserted_key, b"six");
        assert_eq!(tree_string_value(pdf, &inserted_value)?, b"6");
        let begin_value = empty.begin(pdf)?.current().expect("begin still at 5+").1;
        assert_eq!(tree_string_value(pdf, &begin_value)?, b"5+");
        assert_eq!(empty.last(pdf)?.current().expect("last at six").0, b"six");
        let last_value = empty.last(pdf)?.current().expect("last value at six").1;
        assert_eq!(tree_string_value(pdf, &last_value)?, b"6");
    }

    writeln!(stdout, "/Bad1 -- wrong key type")?;
    let mut bad1 = NameTree::new(pdf.trailer_key_handle(b"Bad1"), true);
    let found = bad1.find(pdf, "G", true)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert_eq!(found.current().expect("closest key below G").0, b"A");
    let mut cursor = bad1.begin(pdf)?;
    while let Some((key, _)) = cursor.current() {
        write_bytes(stdout, &key)?;
        writeln!(stdout)?;
        cursor.next(&mut bad1, pdf)?;
    }

    writeln!(stdout, "/Bad2 -- invalid kid")?;
    let mut bad2 = NameTree::new(pdf.trailer_key_handle(b"Bad2"), true);
    let found = bad2.find(pdf, "G", true)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert_eq!(found.current().expect("closest key below G").0, b"B");
    let mut cursor = bad2.begin(pdf)?;
    while let Some((key, _)) = cursor.current() {
        write_bytes(stdout, &key)?;
        writeln!(stdout)?;
        cursor.next(&mut bad2, pdf)?;
    }

    writeln!(stdout, "/Bad3 -- invalid kid")?;
    let mut bad3 = NameTree::new(pdf.trailer_key_handle(b"Bad3"), true);
    let found = bad3.find(pdf, "G", true)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(found == bad3.end());

    writeln!(stdout, "/Bad4 -- invalid kid")?;
    let mut bad4 = NameTree::new(pdf.trailer_key_handle(b"Bad4"), true);
    let found = bad4.find(pdf, "F", true)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert_eq!(found.current().expect("closest key below F").0, b"C");
    let mut cursor = bad4.begin(pdf)?;
    while let Some((key, _)) = cursor.current() {
        write_bytes(stdout, &key)?;
        writeln!(stdout)?;
        cursor.next(&mut bad4, pdf)?;
    }

    writeln!(stdout, "/Bad5 -- loop in find")?;
    let mut bad5 = NameTree::new(pdf.trailer_key_handle(b"Bad5"), true);
    let found = bad5.find(pdf, "F", true)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert_eq!(found.current().expect("closest key below F").0, b"D");

    writeln!(stdout, "/Bad6 -- bad limits")?;
    let mut bad6 = NameTree::new(pdf.trailer_key_handle(b"Bad6"), true);
    let inserted = bad6.insert(pdf, "H", ObjectHandle::null())?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert_eq!(inserted.current().expect("key H present").0, b"H");

    Ok(())
}

pub(crate) fn run_test_49<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 qpdf/test_driver.cc:1923-1937.
    //
    // `OutlineTree::get_outlines_for_page` is qpdf's
    // `QPDFOutlineDocumentHelper::getOutlinesForPage`
    // (`outline_object_helper.rs:266-285`'s own doc cites the qpdf source), and
    // `OutlineItem::get_title`/`::get_dest` are the decoded `/Title` and
    // resolved destination `getTitle()`/`getDest()` produce, recomputed live
    // on every call like qpdf's own accessors.
    //
    // qpdf constructs `QPDFOutlineDocumentHelper odh(pdf)` -- which walks the
    // top-level `/Outlines` `/First`/`/Next` chain in its constructor
    // (`libqpdf/QPDFOutlineDocumentHelper.cc:5-21`) -- before it lists pages
    // via `QPDFPageDocumentHelper(pdf).getAllPages()`. `OutlineDocumentHelper`
    // has no constructor-time side effect of its own; `get_tree` is where the
    // equivalent top-level walk happens, so it must run before page listing
    // to preserve that order. The tree-building helper is dropped immediately
    // after so the page list can borrow `pdf` again; a fresh helper serves
    // the per-page loop below (each item's `title`/`dest` calls already
    // resolve live off the item's own handle, so a different helper instance
    // observes the same catalog and produces identical results).
    let tree = {
        let mut tree_helper = OutlineDocumentHelper::new(pdf);
        tree_helper.get_tree()?
    };
    let pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    let mut helper = OutlineDocumentHelper::new(pdf);
    for (pageno, page_ref) in pages.into_iter().enumerate() {
        let mut lines: Vec<(String, Vec<u8>)> = Vec::new();
        for (_, item) in tree.get_outlines_for_page(&mut helper, Some(page_ref))? {
            let title = item.get_title(&mut helper)?;
            let dest = item.get_dest(&mut helper)?.unparse_resolved();
            lines.push((title, dest));
        }
        for (title, dest) in lines {
            write!(stdout, "page {pageno}: {title} -> ")?;
            write_bytes(stdout, &dest)?;
            writeln!(stdout)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        chase_key, kids_item_0_is_indirect, run_test_42, run_test_43, run_test_44, run_test_46,
        tree_string_value, write_nntree_error,
    };
    use flpdf::{AcroFormDocumentHelper, ObjectHandle, Pdf, PdfOpenOptions};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    struct CurrentDirGuard(PathBuf);

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.0).expect("restore current directory");
        }
    }

    fn minimal_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        Pdf::open_mem_owned_with_options(
            include_bytes!("../../../../tests/fixtures/minimal.pdf").to_vec(),
            PdfOpenOptions::default(),
        )
        .expect("open minimal fixture")
    }

    fn pdf_with_form_fields_and_widgets() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let objects: &[(u32, &[u8])] = &[
            (
                1,
                b"<< /Type /Catalog /Pages 2 0 R /AcroForm 20 0 R >>",
            ),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R 6 0 R] >>",
            ),
            (
                4,
                b"<< /FT /Tx /T (alpha) /V (old) /DV (default) /DA (field-da) /Q 2 /Subtype /Widget /Rect [1 2 3 4] /AS /On /AP << /N << /On 8 0 R /3 9 0 R >> >> >>",
            ),
            (5, b"<< /T (group) /Kids [6 0 R] >>"),
            (
                6,
                b"<< /Parent 5 0 R /FT /Tx /T (child) /V (value) /DV (dvalue) /Subtype /Widget /Rect [5 6 7 8] /AS /Off /AP << /N << /Off 10 0 R >> >> >>",
            ),
            (8, b"<< /Length 0 >>\nstream\n\nendstream"),
            (9, b"<< /Length 0 >>\nstream\n\nendstream"),
            (10, b"<< /Length 0 >>\nstream\n\nendstream"),
            (20, b"<< /Fields [4 0 R 5 0 R] /DA (acro-da) /Q 1 >>"),
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = BTreeMap::new();
        for &(number, body) in objects {
            offsets.insert(number, bytes.len());
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = bytes.len();
        let size = objects.last().expect("form fixture objects").0 + 1;
        bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for number in 1..size {
            match offsets.get(&number) {
                Some(offset) => {
                    bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes())
                }
                None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        Pdf::open_mem_owned_with_options(bytes, PdfOpenOptions::default())
            .expect("open form fixture")
    }

    fn pdf_with_non_array_form_fields() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
            (4, b"<< /Fields 5 0 R >>"),
            (5, b"42"),
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = BTreeMap::new();
        for &(number, body) in objects {
            offsets.insert(number, bytes.len());
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = bytes.len();
        let size = objects.last().expect("malformed form fixture objects").0 + 1;
        bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for number in 1..size {
            match offsets.get(&number) {
                Some(offset) => {
                    bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes())
                }
                None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        Pdf::open_mem_owned_with_options(
            bytes,
            PdfOpenOptions {
                suppress_warnings: true,
                description: b"form-bad-fields-array.pdf".to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .expect("open malformed form fixture")
    }

    fn pdf_with_non_dictionary_field_parent() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [6 0 R] >>",
            ),
            (4, b"<< /Fields [6 0 R] >>"),
            (5, b"42"),
            (
                6,
                b"<< /Parent 5 0 R /FT /Tx /T (child) /Subtype /Widget /Rect [1 2 3 4] >>",
            ),
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = BTreeMap::new();
        for &(number, body) in objects {
            offsets.insert(number, bytes.len());
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = bytes.len();
        let size = objects.last().expect("parent fixture objects").0 + 1;
        bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for number in 1..size {
            match offsets.get(&number) {
                Some(offset) => {
                    bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes())
                }
                None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        Pdf::open_mem_owned_with_options(
            bytes,
            PdfOpenOptions {
                suppress_warnings: true,
                description: b"form-parent-error.pdf".to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .expect("open non-dictionary parent fixture")
    }

    fn pdf_with_direct_orphan_widget() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let objects: &[(u32, &[u8])] = &[
            (
                1,
                b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>",
            ),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [<< /Type /Annot /Subtype /Widget /Rect [0 0 10 10] >> << /Type /Annot /Subtype /Widget /Rect [20 20 30 30] >>] >>",
            ),
            (4, b"<< /Fields [] >>"),
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = BTreeMap::new();
        for &(number, body) in objects {
            offsets.insert(number, bytes.len());
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = bytes.len();
        let size = objects.last().expect("direct orphan fixture objects").0 + 1;
        bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for number in 1..size {
            match offsets.get(&number) {
                Some(offset) => {
                    bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes())
                }
                None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        Pdf::open_mem_owned_with_options(
            bytes,
            PdfOpenOptions {
                suppress_warnings: true,
                description: b"direct-orphan.pdf".to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .expect("open direct orphan fixture")
    }

    fn pdf_with_object_types_qtest() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
            ),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = [0usize; 5];
        for &(number, body) in objects {
            offsets[number as usize] = bytes.len();
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = bytes.len();
        bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        for offset in offsets.into_iter().skip(1) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size 5 /Root 1 0 R /QTest << /Dictionary << /Key1 /Value1 /Key2 [ /Item0 << /K [ /V ] >> /Item2 ] >> /Integer 1 >> >>\nstartxref\n{xref_offset}\n%%EOF\n"
            )
            .as_bytes(),
        );
        bytes
    }

    fn pdf_with_number_trees() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Count 0 /Kids [] >>"),
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = [0usize; 3];
        for &(number, body) in objects {
            offsets[number as usize] = bytes.len();
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = bytes.len();
        bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
        for offset in offsets.into_iter().skip(1) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        let trailer = concat!(
            "trailer\n<< /Size 3 /Root 1 0 R ",
            "/QTest << /Nums [",
            "1 (one) 2 (two) 3 (three) 5 (five) 6 (six) ",
            "9 (nine) 11 (elephant) 12 (twelve) 15 (fifteen) ",
            "19 (nineteen) 20 (twenty) 22 (twenty-two) ",
            "23 (twenty-three) 29 (twenty-nine)] >> ",
            "/Bad1 << /Nums [] >> ",
            "/Bad2 << /Nums [10 (10) 15 (15) 35 (35) 38 (38)] >> ",
            "/Empty1 << /Nums [] >> /Empty2 << /Nums [] >> ",
            "/Bad3 << /Kids [<< /Nums [0 (zero) 10 (ten)] >>] >> ",
            "/Bad4 << /Nums [] >> /Bad5 << /Nums [] >> >>\n",
        );
        bytes.extend_from_slice(trailer.as_bytes());
        bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        bytes
    }

    fn pdf_with_name_trees() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Count 0 /Kids [] >>"),
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = [0usize; 3];
        for &(number, body) in objects {
            offsets[number as usize] = bytes.len();
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = bytes.len();
        bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
        for offset in offsets.into_iter().skip(1) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        let trailer = concat!(
            "trailer\n<< /Size 3 /Root 1 0 R ",
            "/QTest << /Names [",
            "(07 sev\\200n) (seven!) ",
            "(11 elephant) (elephant?) ",
            "(29 twenty-nine) (twenty-nine!)] >> ",
            "/Bad1 << /Names [(A) (a)] >> ",
            "/Bad2 << /Names [(B) (b)] >> ",
            "/Bad3 << /Names [] >> ",
            "/Bad4 << /Names [(C) (c)] >> ",
            "/Bad5 << /Names [(D) (d)] >> ",
            "/Bad6 << /Names [] >> ",
            "/Empty1 << /Names [] >> ",
            "/Empty2 << /Names [] >> >>\n",
        );
        bytes.extend_from_slice(trailer.as_bytes());
        bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        bytes
    }

    #[test]
    fn tree_helpers_resolve_canonical_handles_one_hop() {
        let mut pdf = minimal_pdf();
        let value = ObjectHandle::string(b"value".to_vec());
        assert_eq!(tree_string_value(&mut pdf, &value).unwrap(), b"value");

        let root = pdf.trailer_key_handle(b"Root");
        let pages = chase_key(&mut pdf, &root, b"/Pages").expect("resolve /Pages");
        assert_eq!(
            pages.object_ref().map(|object_ref| object_ref.number),
            Some(2)
        );
    }

    #[test]
    fn object_type_and_form_presence_paths_use_canonical_resolution() {
        let mut pdf = Pdf::open_mem_owned_with_options(
            pdf_with_object_types_qtest(),
            PdfOpenOptions::default(),
        )
        .expect("open object-types fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;
        run_test_42(
            &mut pdf,
            b"object-types.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 42");
        assert!(stdout.is_empty());
        let warning_text = String::from_utf8(stderr).expect("warnings are UTF-8");
        assert_eq!(
            warning_text,
            concat!(
                "WARNING: operation for string attempted on object of type dictionary: returning empty string\n",
                "WARNING: returning null for out of bounds array access\n",
                "WARNING: returning null for out of bounds array access\n",
                "WARNING: operation for array attempted on object of type integer: returning null\n",
                "WARNING: operation for array attempted on object of type integer: ignoring attempt to append item\n",
                "WARNING: ignoring attempt to erase out of bounds array item\n",
                "WARNING: ignoring attempt to erase out of bounds array item\n",
                "WARNING: ignoring attempt to insert out of bounds array item\n",
                "WARNING: ignoring attempt to set out of bounds array item\n",
                "WARNING: operation for array attempted on object of type integer: ignoring attempt to erase item\n",
                "WARNING: operation for array attempted on object of type integer: ignoring attempt to insert item\n",
                "WARNING: operation for array attempted on object of type integer: ignoring attempt to replace items\n",
                "WARNING: operation for array attempted on object of type integer: ignoring attempt to set item\n",
                "WARNING: operation for array attempted on object of type integer: treating as empty\n",
                "WARNING: operation for array attempted on object of type integer: treating as empty\n",
                "WARNING: operation for boolean attempted on object of type integer: returning false\n",
                "WARNING: operation for dictionary attempted on object of type integer: treating as empty\n",
                "WARNING: operation for dictionary attempted on object of type integer: treating as empty\n",
                "WARNING: operation for dictionary attempted on object of type integer: returning false for a key containment request\n",
                "WARNING: operation for dictionary attempted on object of type integer: ignoring key removal request\n",
                "WARNING: operation for dictionary attempted on object of type integer: ignoring key replacement request\n",
                "WARNING: operation for dictionary attempted on object of type integer: ignoring key replacement request\n",
                "WARNING: operation for dictionary attempted on object of type integer: returning null for attempted key retrieval\n",
                "WARNING: operation for dictionary attempted on object of type integer: returning null for attempted key retrieval\n",
                "WARNING: operation for inlineimage attempted on object of type integer: returning empty data\n",
                "WARNING: operation for integer attempted on object of type dictionary: returning 0\n",
                "WARNING: operation for name attempted on object of type integer: returning dummy name\n",
                "WARNING: operation for operator attempted on object of type integer: returning fake value\n",
                "WARNING: operation for real attempted on object of type dictionary: returning 0.0\n",
                "WARNING: operation for string attempted on object of type integer: returning empty string\n",
                "WARNING: operation for string attempted on object of type integer: returning empty string\n",
                "WARNING: operation for number attempted on object of type dictionary: returning 0\n",
                "One error\n",
                "WARNING: operation for string attempted on object of type name: returning empty string\n",
                "One error\n",
                "WARNING:  -> dictionary key /Quack: operation for string attempted on object of type null: returning empty string\n",
                "WARNING:  -> dictionary key /Quack: operation for string attempted on object of type null: returning empty string\n",
                "Two errors\n",
                "WARNING: returning null for out of bounds array access\n",
                "WARNING:  -> null returned from invalid array access: operation for string attempted on object of type null: returning empty string\n",
                "One error\n",
                "WARNING: operation for string attempted on object of type name: returning empty string\n",
                "WARNING: , object 4 0 at offset 212 -> dictionary key /Potato: operation for name attempted on object of type null: returning dummy name\n",
            )
        );
        assert!(!warning_text.contains("test 42 done"));

        let mut pdf = minimal_pdf();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;
        run_test_43(
            &mut pdf,
            b"minimal.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 43");
        assert_eq!(stdout, b"no forms\n");
        assert!(stderr.is_empty());
    }

    #[test]
    fn test_43_executes_terminal_field_and_widget_consumers() {
        let mut pdf = pdf_with_form_fields_and_widgets();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_43(
            &mut pdf,
            b"form-consumer.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 43");

        assert_eq!(
            stdout,
            concat!(
                "iterating over form fields\n",
                "Field: 4 0 R\n",
                "  Parent: none\n",
                "  Fully qualified name: alpha\n",
                "  Partial name: alpha\n",
                "  Alternative name: alpha\n",
                "  Mapping name: alpha\n",
                "  Field type: /Tx\n",
                "  Value: (old)\n",
                "  Value as string: old\n",
                "  Default value: (default)\n",
                "  Default value as string: default\n",
                "  Default appearance: field-da\n",
                "  Quadding: 2\n",
                "  Annotation: 4 0 R\n",
                "Field: 6 0 R\n",
                "  Parent: 5 0 R\n",
                "  Parent: none\n",
                "  Fully qualified name: group.child\n",
                "  Partial name: child\n",
                "  Alternative name: group.child\n",
                "  Mapping name: group.child\n",
                "  Field type: /Tx\n",
                "  Value: (value)\n",
                "  Value as string: value\n",
                "  Default value: (dvalue)\n",
                "  Default value as string: dvalue\n",
                "  Default appearance: acro-da\n",
                "  Quadding: 1\n",
                "  Annotation: 6 0 R\n",
                "iterating over annotations per page\n",
                "Page: 3 0 R\n",
                "  Annotation: 4 0 R\n",
                "    Field: 4 0 R\n",
                "    Subtype: /Widget\n",
                "    Rect: [1, 2, 3, 4]\n",
                "    Appearance state: /On\n",
                "    Appearance stream (/N): 8 0 R\n",
                "    Appearance stream (/N, /3): 9 0 R\n",
                "  Annotation: 6 0 R\n",
                "    Field: 6 0 R\n",
                "    Subtype: /Widget\n",
                "    Rect: [5, 6, 7, 8]\n",
                "    Appearance state: /Off\n",
                "    Appearance stream (/N): 10 0 R\n",
                "    Appearance stream (/N, /3): null\n"
            )
            .as_bytes()
        );
        assert!(stderr.is_empty());
    }

    #[test]
    fn test_43_reports_a_non_array_acroform_fields_warning() {
        let mut pdf = pdf_with_non_array_form_fields();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_43(
            &mut pdf,
            b"form-bad-fields-array.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 43");

        assert!(stderr
            .windows(b"/Fields key of /AcroForm dictionary is not an array; ignoring\n".len())
            .any(|window| {
                window == b"/Fields key of /AcroForm dictionary is not an array; ignoring\n"
            }));
    }

    #[test]
    fn test_43_flushes_warnings_raised_during_field_consumption() {
        let mut pdf = pdf_with_non_dictionary_field_parent();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_43(
            &mut pdf,
            b"form-parent-error.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 43");

        assert!(stderr.windows(
            b"operation for dictionary attempted on object of type integer: returning null for attempted key retrieval\n".len()
        ).any(|window| {
            window
                == b"operation for dictionary attempted on object of type integer: returning null for attempted key retrieval\n"
        }));
    }

    #[test]
    fn get_form_fields_preserves_qpdf_zero_objgen_orphan_membership() {
        let mut pdf = pdf_with_direct_orphan_widget();
        let (fields, annotations, widgets, first_field, second_field) = {
            let mut acroform = AcroFormDocumentHelper::new(&mut pdf).expect("AcroForm helper");
            let widgets = acroform
                .get_widget_annotations_for_page(flpdf::ObjectRef::new(3, 0))
                .expect("get page widgets");
            assert_eq!(widgets.len(), 2);
            let first_field = acroform
                .get_field_for_annotation_handle(widgets[0].clone())
                .expect("get first orphan field");
            let second_field = acroform
                .get_field_for_annotation_handle(widgets[1].clone())
                .expect("get second orphan field");
            let fields = acroform.get_form_fields().expect("get form fields");
            let annotations = acroform
                .get_annotations_for_field(ObjectHandle::null())
                .expect("get orphan annotations");
            (fields, annotations, widgets, first_field, second_field)
        };

        assert_eq!(fields.len(), 1);
        assert!(fields[0].is_null());
        assert_eq!(annotations.len(), 1);
        assert!(annotations[0].object_ref().is_none());
        assert_eq!(widgets.len(), 2);
        assert!(first_field.is_same_object_as(&annotations[0]));
        assert!(second_field.is_same_object_as(&first_field));
    }

    #[test]
    fn test_44_mutates_live_text_fields_and_writes_qdf() {
        let _lock = super::super::CURRENT_DIR_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("acquire current-directory test lock");
        let directory = tempfile::tempdir().expect("create test directory");
        let previous = std::env::current_dir().expect("read current directory");
        std::env::set_current_dir(directory.path()).expect("enter test directory");
        let _restore = CurrentDirGuard(previous);

        let mut pdf = pdf_with_form_fields_and_widgets();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_44(
            &mut pdf,
            b"form-consumer.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 44");

        assert_eq!(
            stdout,
            b"Set field value: alpha -> 3.14 \xc3\xb7 0\n\
Set field value: group.child -> 3.14 \xc3\xb7 0\n"
        );
        assert!(stderr.is_empty());

        let written = std::fs::read("a.pdf").expect("test 44 output");
        let mut written = Pdf::open_mem_owned(written).expect("reopen test 44 output");
        let fields = {
            let mut acroform =
                flpdf::AcroFormDocumentHelper::new(&mut written).expect("reopen AcroForm helper");
            acroform
                .get_form_fields()
                .expect("read written form fields")
        };
        assert_eq!(fields.len(), 2);
        for field in fields {
            let mut field = flpdf::FormFieldObjectHelper::from_object_handle(field, &mut written);
            assert_eq!(
                field.value_as_string().expect("read updated field"),
                "3.14 ÷ 0"
            );
        }
    }

    #[test]
    fn kids_item_probe_resolves_the_page_tree_handle_once() {
        let mut pdf = minimal_pdf();
        let root = pdf.trailer_key_handle(b"Root");
        let pages = chase_key(&mut pdf, &root, b"/Pages").expect("resolve /Pages");
        assert!(!kids_item_0_is_indirect(&mut pdf, &pages).expect("inspect /Kids"));
    }

    #[test]
    fn number_tree_driver_fixture_covers_qpdf_warning_boundaries() {
        let mut pdf = Pdf::open_mem_owned_with_options(
            pdf_with_number_trees(),
            PdfOpenOptions {
                suppress_warnings: true,
                description: b"number-tree.pdf".to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .expect("open number-tree fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_46(
            &mut pdf,
            b"number-tree.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 46");

        assert!(stdout
            .windows(b"29 twenty-nine\n".len())
            .any(|window| window == b"29 twenty-nine\n"));
        assert!(stdout
            .windows(
                b"number-tree.pdf (Name/Number tree node): unable to find a valid items node".len()
            )
            .any(|window| {
                window
                    == b"number-tree.pdf (Name/Number tree node): unable to find a valid items node"
            }));
        assert!(!stderr.is_empty());
    }

    #[test]
    fn nntree_error_writer_falls_back_for_non_structural_errors() {
        let mut stdout = Vec::new();
        write_nntree_error(
            &mut stdout,
            b"number-tree.pdf",
            &flpdf::Error::System("ordinary error".to_owned()),
        )
        .expect("write fallback error");
        assert_eq!(stdout, b"ordinary error\n");
    }

    #[test]
    fn name_tree_last_value_uses_the_canonical_resolver() {
        let mut pdf =
            Pdf::open_mem_owned_with_options(pdf_with_name_trees(), PdfOpenOptions::default())
                .expect("open name tree fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        super::run_test_48(
            &mut pdf,
            b"name-tree.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 48");

        assert!(stdout
            .windows(b"29 twenty-nine -> ".len())
            .any(|window| window == b"29 twenty-nine -> "));
        assert!(stderr.is_empty());
    }
}

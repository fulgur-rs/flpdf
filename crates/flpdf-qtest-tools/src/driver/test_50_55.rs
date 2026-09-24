use std::ffi::OsStr;
use std::io::{Read, Seek, Write};

use flpdf::form_field_object_helper::FormFieldObjectHelper;
use flpdf::json_inspect::pdf_object_to_json;
use flpdf::page_document_helper::PageDocumentHelper;
use flpdf::page_object_helper::PageObjectHelper;
use flpdf::writer::{ObjectStreamMode, PdfWriter};
use flpdf::{Error, ObjectHandle, Pdf};

use super::{emit_new_diagnostics, os_str_diagnostic_bytes};

/// test_driver.cc:1939-1953 (`test_50`). Dictionary merge test crafted to
/// work with `merge-dict.pdf`.
pub(crate) fn run_test_50<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let d1_handle = pdf.trailer_key_handle(b"Dict1");
    let d2_handle = pdf.trailer_key_handle(b"Dict2");

    // qpdf's `mergeResources` resolves both operands through
    // `isDictionary()` before inspecting their entries. The canonical
    // `ObjectHandle::merge_resources` owns that same resolution boundary, so
    // keep the trailer children unresolved until the merge operation itself;
    // qpdf's merge operation never replaces an indirect identity with a
    // copied terminal value.
    let d1 = d1_handle.clone();
    let d2 = d2_handle.clone();

    let merge_result = d1.merge_resources(&d2, None);
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    merge_result?;

    // `d1.getJSON(JSON::LATEST)` uses qpdf's default
    // `dereference_indirect = false` (`include/qpdf/QPDFObjectHandle.hh`):
    // if `/Dict1` is stored as an indirect reference, this still prints only
    // its own "N G R" unparse as a JSON string
    // (`QPDFObjectHandle::getJSON`, `libqpdf/QPDFObjectHandle.cc:1613-1627`)
    // even though `merge_resources` above already mutated the object it
    // points to. `pdf_object_to_json` implements the identical
    // never-resolve-the-top-level contract, so pass the original trailer
    // child handle rather than materializing a separate JSON value; for a
    // direct `/Dict1` the two handles share the same state, so this still
    // shows the merged dictionary.
    let json = pdf_object_to_json(&d1_handle).map_err(|error| Error::System(error.to_string()))?;
    let unparsed = json.unparse()?;
    stdout.write_all(&unparsed)?;
    writeln!(stdout)?;

    // Top-level type mismatch: qpdf's `d2.getKey("/k1")` result need not be a
    // dictionary (deliberately mismatched by this test). qpdf's
    // `mergeResources` call happens unconditionally regardless of that
    // value's type; whether it turns out to be a no-op depends on the
    // resolved type, matching `merge_resources`'s own no-op contract for a
    // non-dictionary `other`.
    let d2_k1 = d2.try_get_key(b"/k1");
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let d2_k1 = d2_k1?;
    let merge_result = d1.merge_resources(&d2_k1, None);
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    merge_result?;

    // qpdf iterates `d1`'s top-level keys whose already-merged value is itself
    // a dictionary, printing the sorted names returned by
    // `getResourceNames` (`qpdf/test_driver.cc:1940-1953`,
    // `libqpdf/QPDFObjectHandle.cc:1156-1170`). The canonical Rust primitive
    // owns the same receiver/value resolution boundary and returns the raw
    // dictionary keys, so the driver only performs the qpdf consumer's byte
    // output step here.
    let qpdf_flush_result_61 = d1.get_resource_names();
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let resource_names = qpdf_flush_result_61?;
    for name in resource_names {
        stdout.write_all(&name)?;
        stdout.write_all(b"\n")?;
    }
    Ok(())
}

/// test_driver.cc:1955-1997 (`test_51`). Radio button and checkbox field
/// setting; the input files must have radio buttons named `r1`/`r2` and
/// checkboxes named `checkbox1`/`checkbox2` (`button-set*.pdf`).
pub(crate) fn run_test_51<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf's getRoot/getKey/getArrayNItems/getArrayItem/getUTF8Value calls
    // resolve their receivers at each accessor boundary
    // (`qpdf/test_driver.cc:1955-1997`). Keep that order and flush the shared
    // diagnostic collection before any subsequent observable output.
    let root = pdf.root_handle();
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let root = root?;
    let acroform = root.try_get_key(b"/AcroForm");
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let acroform = acroform?;
    let fields = acroform.try_get_key(b"/Fields");
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let fields = fields?;
    let count = fields.try_get_array_n_items();
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let count = count?;

    for index in 0..count {
        let field = fields.try_get_array_item(index as i64);
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        let field = field?;
        let t = field.try_get_key(b"/T");
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        let t = t?;
        let is_string = t.try_is_string();
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        let is_string = is_string?;
        if !is_string {
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            continue;
        }
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        let utf8 = t.try_get_utf8_value();
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        let utf8 = utf8?;

        if utf8 == b"r1" {
            writeln!(stdout, "setting r1 via parent")?;
            let mut foh = FormFieldObjectHelper::from_object_handle(field.clone(), pdf);
            let qpdf_flush_result_62 = foh.set_value(ObjectHandle::name(b"2".to_vec()), true);
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            qpdf_flush_result_62?;
        } else if utf8 == b"r2" {
            writeln!(stdout, "setting r2 via child")?;
            let kids = field.try_get_key(b"/Kids");
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            let kids = kids?;
            let kid = kids.try_get_array_item(1);
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            let kid = kid?;
            let mut foh = FormFieldObjectHelper::from_object_handle(kid, pdf);
            let qpdf_flush_result_63 = foh.set_value(ObjectHandle::name(b"3".to_vec()), true);
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            qpdf_flush_result_63?;
        } else if utf8 == b"checkbox1" {
            writeln!(stdout, "turning checkbox1 on")?;
            // The value that eventually gets set is based on what's allowed
            // in /N and may not match this value (matches qpdf's own comment:
            // setV maps any non-/Off name to "checked").
            let mut foh = FormFieldObjectHelper::from_object_handle(field, pdf);
            let qpdf_flush_result_64 = foh.set_value(ObjectHandle::name(b"Sure".to_vec()), true);
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            qpdf_flush_result_64?;
        } else if utf8 == b"checkbox2" {
            writeln!(stdout, "turning checkbox2 off")?;
            let mut foh = FormFieldObjectHelper::from_object_handle(field, pdf);
            let qpdf_flush_result_65 = foh.set_value(ObjectHandle::name(b"Off".to_vec()), true);
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            qpdf_flush_result_65?;
        }
    }

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_qdf_mode(true);
    writer.set_static_id(true);
    writer.write()?;
    Ok(())
}

/// test_driver.cc:1999-2022 (`test_52`). Sets a field value for
/// appearance-stream generation testing.
pub(crate) fn run_test_52<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf dereferences `arg2` (a `char const*`) without a null check --
    // undefined behavior in the C++ original if the caller omits it.
    // `expect` is the closest controlled stand-in for that same
    // missing-argument case, rather than silently substituting empty bytes.
    let arg2 = arg2.expect("qpdf's test_52 dereferences arg2 without checking for null");

    // qpdf's getRoot/getKey/getArrayNItems/getArrayItem/getUTF8Value calls
    // resolve their receivers at each accessor boundary
    // (`qpdf/test_driver.cc:1999-2022`). Keep that order and flush the shared
    // diagnostic collection before any subsequent observable output.
    let root = match pdf.root_handle() {
        Ok(root) => root,
        Err(error) => {
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            return Err(error);
        }
    };
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let acroform = root.try_get_key(b"/AcroForm");
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let acroform = acroform?;
    let fields = acroform.try_get_key(b"/Fields");
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let fields = fields?;
    let count = fields.try_get_array_n_items();
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let count = count?;

    for index in 0..count {
        let field = fields.try_get_array_item(index as i64);
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        let field = field?;
        let t = field.try_get_key(b"/T");
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        let t = t?;
        let is_string = t.try_is_string();
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        let is_string = is_string?;
        if !is_string {
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            continue;
        }
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        let utf8 = t.try_get_utf8_value();
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        let utf8 = utf8?;

        if utf8 == b"list1" {
            writeln!(stdout, "setting list1 value")?;
            // `newString` stores `arg2`'s raw bytes as-is -- unlike
            // `newUnicodeString`, this is qpdf's literal-string
            // constructor, not a UTF-8-to-UTF-16 conversion. (`set_value`
            // below still re-encodes it as a Unicode string for a text
            // field, matching `QPDFFormFieldObjectHelper::setV`'s own
            // `value.isString()` branch.)
            let value = ObjectHandle::string(os_str_diagnostic_bytes(arg2).into_owned());
            let mut foh = FormFieldObjectHelper::from_object_handle(field, pdf);
            let qpdf_flush_result_66 = foh.set_value(value, true);
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            qpdf_flush_result_66?;
        }
    }

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.write()?;
    Ok(())
}

/// test_driver.cc:2024-2041 (`test_53`). Get-all-objects and dangling-ref
/// handling.
pub(crate) fn run_test_53<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf allocates the next generation-zero object through its document.
    // getRoot() (libqpdf/QPDF.cc:2355-2367) accepts a direct or indirect
    // /Root dictionary, so use the live root handle rather than requiring
    // an indirect object identity.
    let root = pdf.root_handle()?;

    let qpdf_flush_result_67 =
        pdf.make_indirect_object_handle(ObjectHandle::string(b"potato".to_vec()));
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let new_object = qpdf_flush_result_67?;
    stdout.write_all(b"new object: ")?;
    stdout.write_all(&new_object.unparse())?;
    stdout.write_all(b"\n")?;

    root.replace_key(b"/Q1", new_object)?;

    writeln!(stdout, "all objects")?;
    for object in pdf.get_all_objects()? {
        stdout.write_all(&object.unparse())?;
        stdout.write_all(b"\n")?;
    }

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_static_id(true);
    writer.set_preserve_unreferenced_objects(true);
    writer.write()?;
    Ok(())
}

/// test_driver.cc:2043-2054 (`test_54`). Tests `getFinalVersion`; must be
/// invoked with a file whose final version is not 1.5.
pub(crate) fn run_test_54<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf constructs `QPDFWriter w(pdf, "a.pdf")` (opening/truncating
    // "a.pdf" at construction) before the assert below. `PdfWriter::new`
    // borrows `pdf` mutably for its own lifetime, so `pdf.version()` can no
    // longer be called once the writer exists; reading it first instead of
    // after construction does not change any printed byte -- the assert
    // has no observable success-path output, and the writer's file-open
    // side effect on disk is not part of this driver's stdout/stderr
    // contract.
    let version = pdf.version().to_string();

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    assert_ne!(version, "1.5");
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    // qpdf calls `getFinalVersion()` twice: once for the `if` condition,
    // once again to print it.
    let final_version = writer.get_final_version()?;
    if final_version != "1.5" {
        let final_version_again = writer.get_final_version()?;
        writeln!(stdout, "oops: {final_version_again}")?;
    }
    Ok(())
}

/// test_driver.cc:2056-2071 (`test_55`). Form XObjects.
pub(crate) fn run_test_55<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let mut helper = PageDocumentHelper::new(pdf);
    let qpdf_flush_result_68 = helper.get_all_pages();
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let pages = qpdf_flush_result_68?;
    // qpdf constructs the array before the loop and appends both
    // `getFormXObjectForPage()` and `getFormXObjectForPage(false)` for each
    // page (`qpdf/test_driver.cc:2056-2064`). The canonical page helper owns
    // the page-to-Form-XObject conversion, including inherited attributes,
    // lazy content, and the conditional transformation matrix
    // (`libqpdf/QPDFPageObjectHelper.cc:706-733`).
    let qtest = ObjectHandle::array(Vec::new());
    for page_ref in pages {
        let transformed = {
            let mut page = PageObjectHelper::new(page_ref, pdf);
            page.get_form_xobject_for_page(true)
        };
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        let transformed = transformed?;
        qtest.append_array_item(transformed)?;

        let untransformed = {
            let mut page = PageObjectHelper::new(page_ref, pdf);
            page.get_form_xobject_for_page(false)
        };
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        let untransformed = untransformed?;
        qtest.append_array_item(untransformed)?;
    }
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    // Keep the finished array on the live trailer, matching qpdf's
    // `getTrailer().replaceKey("/QTest", qtest)` before construction of the
    // writer (`qpdf/test_driver.cc:2065`).
    let trailer = pdf.trailer();
    trailer.replace_key(b"/QTest", qtest)?;

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_qdf_mode(true);
    writer.set_static_id(true);
    let write_result = writer.write();
    drop(writer);
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    write_result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{run_test_50, run_test_51, run_test_52};
    use flpdf::{ObjectHandle, Pdf, PdfOpenOptions};
    use std::ffi::OsStr;
    use std::io;
    use std::path::PathBuf;

    struct FailAfterWrites {
        remaining: usize,
    }

    impl io::Write for FailAfterWrites {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "test output failure",
                ));
            }
            self.remaining -= 1;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct CurrentDirGuard(PathBuf);

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.0).expect("restore current directory");
        }
    }

    fn radio_widget_with_appearance() -> ObjectHandle {
        ObjectHandle::dictionary(vec![
            (
                b"/AP".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    ObjectHandle::dictionary(vec![
                        (b"/Off".to_vec(), ObjectHandle::null()),
                        (b"/3".to_vec(), ObjectHandle::null()),
                    ]),
                )]),
            ),
            (b"/AS".to_vec(), ObjectHandle::name(b"Off".to_vec())),
        ])
    }

    fn broken_button_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let mut pdf = Pdf::open_mem_owned_with_options(
            include_bytes!("../../../../tests/fixtures/minimal.pdf").to_vec(),
            PdfOpenOptions {
                description: b"button-set-broken.pdf".to_vec(),
                suppress_warnings: true,
                ..PdfOpenOptions::default()
            },
        )
        .expect("open minimal PDF");
        let radio = pdf
            .make_indirect_object_handle(ObjectHandle::dictionary(vec![
                (b"/FT".to_vec(), ObjectHandle::name(b"Btn".to_vec())),
                (b"/Ff".to_vec(), ObjectHandle::integer(1 << 15)),
                (b"/T".to_vec(), ObjectHandle::string(b"r1".to_vec())),
                (
                    b"/Kids".to_vec(),
                    ObjectHandle::array(vec![ObjectHandle::dictionary(Vec::new())]),
                ),
            ]))
            .expect("allocate broken radio field");
        let checkbox = pdf
            .make_indirect_object_handle(ObjectHandle::dictionary(vec![
                (b"/FT".to_vec(), ObjectHandle::name(b"Btn".to_vec())),
                (b"/T".to_vec(), ObjectHandle::string(b"checkbox1".to_vec())),
            ]))
            .expect("allocate broken checkbox field");
        let checkbox2 = pdf
            .make_indirect_object_handle(ObjectHandle::dictionary(vec![
                (b"/FT".to_vec(), ObjectHandle::name(b"Btn".to_vec())),
                (b"/T".to_vec(), ObjectHandle::string(b"checkbox2".to_vec())),
                (
                    b"/AP".to_vec(),
                    ObjectHandle::dictionary(vec![(
                        b"/N".to_vec(),
                        ObjectHandle::dictionary(vec![
                            (b"/Off".to_vec(), ObjectHandle::null()),
                            (b"/Yes".to_vec(), ObjectHandle::null()),
                        ]),
                    )]),
                ),
                (b"/AS".to_vec(), ObjectHandle::name(b"Yes".to_vec())),
            ]))
            .expect("allocate intact checkbox field");
        let radio_kid1 = pdf
            .make_indirect_object_handle(radio_widget_with_appearance())
            .expect("allocate first radio widget");
        let radio_kid2 = pdf
            .make_indirect_object_handle(radio_widget_with_appearance())
            .expect("allocate second radio widget");
        let radio2 = pdf
            .make_indirect_object_handle(ObjectHandle::dictionary(vec![
                (b"/FT".to_vec(), ObjectHandle::name(b"Btn".to_vec())),
                (b"/Ff".to_vec(), ObjectHandle::integer(1 << 15)),
                (b"/T".to_vec(), ObjectHandle::string(b"r2".to_vec())),
                (
                    b"/Kids".to_vec(),
                    ObjectHandle::array(vec![radio_kid1.clone(), radio_kid2.clone()]),
                ),
            ]))
            .expect("allocate intact radio field");
        for kid in [&radio_kid1, &radio_kid2] {
            kid.replace_key(b"/Parent", radio2.clone())
                .expect("link radio widget to parent");
        }
        let acroform = ObjectHandle::dictionary(vec![(
            b"/Fields".to_vec(),
            ObjectHandle::array(vec![radio, checkbox, checkbox2, radio2]),
        )]);
        let root = pdf.root_handle().expect("root");
        root.replace_key(b"/AcroForm", acroform)
            .expect("install AcroForm");
        pdf
    }

    fn direct_checkbox_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let mut pdf = Pdf::open_mem_owned_with_options(
            include_bytes!("../../../../tests/fixtures/minimal.pdf").to_vec(),
            PdfOpenOptions {
                description: b"button-set-direct.pdf".to_vec(),
                suppress_warnings: true,
                ..PdfOpenOptions::default()
            },
        )
        .expect("open minimal PDF");
        let checkbox = ObjectHandle::dictionary(vec![
            (b"/FT".to_vec(), ObjectHandle::name(b"Btn".to_vec())),
            (b"/T".to_vec(), ObjectHandle::string(b"checkbox1".to_vec())),
            (
                b"/AP".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    ObjectHandle::dictionary(vec![
                        (b"/Off".to_vec(), ObjectHandle::null()),
                        (b"/On".to_vec(), ObjectHandle::null()),
                    ]),
                )]),
            ),
        ]);
        let acroform = ObjectHandle::dictionary(vec![(
            b"/Fields".to_vec(),
            ObjectHandle::array(vec![checkbox]),
        )]);
        let root = pdf.root_handle().expect("root");
        root.replace_key(b"/AcroForm", acroform)
            .expect("install AcroForm");
        pdf
    }

    fn direct_text_field_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let mut pdf = Pdf::open_mem_owned_with_options(
            include_bytes!("../../../../tests/fixtures/minimal.pdf").to_vec(),
            PdfOpenOptions {
                description: b"appearance-direct.pdf".to_vec(),
                suppress_warnings: true,
                ..PdfOpenOptions::default()
            },
        )
        .expect("open minimal PDF");
        let text_field = ObjectHandle::dictionary(vec![
            (b"/FT".to_vec(), ObjectHandle::name(b"Tx".to_vec())),
            (b"/T".to_vec(), ObjectHandle::string(b"list1".to_vec())),
        ]);
        let acroform = ObjectHandle::dictionary(vec![(
            b"/Fields".to_vec(),
            ObjectHandle::array(vec![text_field]),
        )]);
        let root = pdf.root_handle().expect("root");
        root.replace_key(b"/AcroForm", acroform)
            .expect("install AcroForm");
        pdf
    }

    #[test]
    fn test_51_drains_each_broken_button_warning_after_its_operation() {
        let _lock = super::super::CURRENT_DIR_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("acquire current-directory test lock");
        let directory = tempfile::tempdir().expect("create test directory");
        let previous = std::env::current_dir().expect("read current directory");
        std::env::set_current_dir(directory.path()).expect("enter test directory");
        let _restore = CurrentDirGuard(previous);

        let mut pdf = broken_button_pdf();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_51(
            &mut pdf,
            b"button-set-broken.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 51");

        assert_eq!(
            stdout,
            b"setting r1 via parent\nturning checkbox1 on\nturning checkbox2 off\nsetting r2 via child\n"
        );
        let warning = String::from_utf8(stderr).expect("warnings are UTF-8");
        assert_eq!(warning.matches("unable to set the value").count(), 2);
        assert!(warning.contains("unable to set the value of this radio button"));
        assert!(warning.contains("unable to set the value of this checkbox"));
        assert!(directory.path().join("a.pdf").is_file());
    }

    #[test]
    fn test_51_accepts_a_direct_checkbox_field_handle() {
        let _lock = super::super::CURRENT_DIR_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("acquire current-directory test lock");
        let directory = tempfile::tempdir().expect("create test directory");
        let previous = std::env::current_dir().expect("read current directory");
        std::env::set_current_dir(directory.path()).expect("enter test directory");
        let _restore = CurrentDirGuard(previous);

        let mut pdf = direct_checkbox_pdf();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_51(
            &mut pdf,
            b"button-set-direct.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("qpdf accepts a direct field handle");

        assert_eq!(stdout, b"turning checkbox1 on\n");
        assert!(stderr.is_empty());
        assert!(directory.path().join("a.pdf").is_file());
    }

    #[test]
    fn test_52_accepts_a_direct_text_field_handle() {
        let _lock = super::super::CURRENT_DIR_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("acquire current-directory test lock");
        let directory = tempfile::tempdir().expect("create test directory");
        let previous = std::env::current_dir().expect("read current directory");
        std::env::set_current_dir(directory.path()).expect("enter test directory");
        let _restore = CurrentDirGuard(previous);

        let mut pdf = direct_text_field_pdf();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_52(
            &mut pdf,
            b"appearance-direct.pdf",
            Some(OsStr::new("five")),
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("qpdf accepts a direct text field handle");

        assert_eq!(stdout, b"setting list1 value\n");
        assert!(stderr.is_empty());
        assert!(directory.path().join("a.pdf").is_file());
    }

    fn pdf_with_merge_dictionaries() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 4 0 R >>"),
            (2, b"<< /Font << /F1 5 0 R >> /XObject << >> >>"),
            (3, b"<< /k1 true /Font << /F2 6 0 R >> >>"),
            (4, b"<< /Type /Pages /Count 0 /Kids [] >>"),
            (5, b"<< >>"),
            (6, b"<< >>"),
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = vec![0usize; 7];
        for &(number, body) in objects {
            offsets[number as usize] = bytes.len();
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = bytes.len();
        bytes.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
        for offset in offsets.into_iter().skip(1) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size 7 /Root 1 0 R /Dict1 2 0 R /Dict2 3 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
            )
            .as_bytes(),
        );
        bytes
    }

    #[test]
    fn dictionary_merge_resolves_each_trailer_handle_once() {
        let mut pdf = Pdf::open_mem_owned_with_options(
            pdf_with_merge_dictionaries(),
            PdfOpenOptions::default(),
        )
        .expect("open merge dictionary fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = pdf.repair_diagnostics().entries().len();

        run_test_50(
            &mut pdf,
            b"merge-dict.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 50");

        assert!(stderr.is_empty());
        assert!(
            stdout.ends_with(b"/F1\n/F2\n"),
            "test 50 must emit the merged resource names in sorted order: {stdout:?}"
        );
    }

    #[test]
    fn dictionary_merge_propagates_a_resource_name_output_failure() {
        let mut pdf = Pdf::open_mem_owned_with_options(
            pdf_with_merge_dictionaries(),
            PdfOpenOptions::default(),
        )
        .expect("open merge dictionary fixture");
        let mut stdout = FailAfterWrites { remaining: 2 };
        let mut stderr = Vec::new();
        let mut diagnostics_written = pdf.repair_diagnostics().entries().len();

        let result = run_test_50(
            &mut pdf,
            b"merge-dict.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        );

        assert!(
            result.is_err(),
            "resource-name output failure must propagate"
        );
        assert!(stderr.is_empty());
    }

    /// Like [`pdf_with_merge_dictionaries`], but `/Dict1` (object 2) is
    /// missing its `endobj` keyword, so parsing it lazily during the first
    /// `merge_resources` call records a repair warning; and `/Dict2` (object
    /// 3) carries a new resource category (`/ExtGState`, absent from
    /// `/Dict1`) whose value is a stream object (object 7), which makes
    /// `merge_resources`'s `shallow_copy` on that new category fail with
    /// "stream objects cannot be cloned" (`ObjectHandle::shallow_copy`,
    /// `object_handle.rs:5480-5489`) -- both conditions flpdf-v9z62 needs to
    /// demonstrate the diagnostic-drop bug in the first merge_resources call.
    fn pdf_with_merge_dictionaries_and_pending_repair_warning() -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = vec![0usize; 8];

        offsets[1] = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 4 0 R >>\nendobj\n");

        // Object 2 (/Dict1): deliberately no "endobj" -- object 3's header
        // follows directly, so parsing object 2 lazily during the first
        // merge_resources call must recover and record an "expected endobj"
        // repair warning.
        offsets[2] = bytes.len();
        bytes.extend_from_slice(b"2 0 obj\n<< /Font << /F1 5 0 R >> /XObject << >> >>\n");

        offsets[3] = bytes.len();
        bytes.extend_from_slice(
            b"3 0 obj\n<< /k1 true /Font << /F2 6 0 R >> /ExtGState 7 0 R >>\nendobj\n",
        );

        offsets[4] = bytes.len();
        bytes.extend_from_slice(b"4 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

        offsets[5] = bytes.len();
        bytes.extend_from_slice(b"5 0 obj\n<< >>\nendobj\n");

        offsets[6] = bytes.len();
        bytes.extend_from_slice(b"6 0 obj\n<< >>\nendobj\n");

        offsets[7] = bytes.len();
        bytes.extend_from_slice(b"7 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n");

        let xref_offset = bytes.len();
        bytes.extend_from_slice(b"xref\n0 8\n0000000000 65535 f \n");
        for offset in offsets.into_iter().skip(1) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size 8 /Root 1 0 R /Dict1 2 0 R /Dict2 3 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
            )
            .as_bytes(),
        );
        bytes
    }

    #[test]
    fn dictionary_merge_flushes_a_pending_repair_warning_before_its_own_failure() {
        let mut pdf = Pdf::open_mem_owned_with_options(
            pdf_with_merge_dictionaries_and_pending_repair_warning(),
            PdfOpenOptions {
                description: b"merge-dict-pending-warning.pdf".to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .expect("open merge dictionary fixture with pending repair warning");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = pdf.repair_diagnostics().entries().len();

        let result = run_test_50(
            &mut pdf,
            b"merge-dict-pending-warning.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        );

        assert!(
            result.is_err(),
            "the /ExtGState stream category must fail merge_resources's shallow_copy"
        );
        // This is the regression this test pins: before flpdf-v9z62, the
        // `?` on `merge_resources` returned before `emit_new_diagnostics`
        // ever ran, so the "expected endobj" warning recorded while lazily
        // resolving /Dict1 was silently dropped -- stderr stayed empty even
        // though a repair diagnostic was pending. qpdf's own logger prints
        // each warning synchronously as it is recorded, so the warning is
        // always visible before the subsequent exception, regardless of
        // whether the merge that provoked it succeeds.
        assert!(
            stderr
                .windows(b"expected endobj".len())
                .any(|window| window == b"expected endobj"),
            "the /Dict1 repair warning recorded during the failing merge must still reach stderr: {:?}",
            String::from_utf8_lossy(&stderr)
        );
        assert_eq!(
            pdf.repair_diagnostics().entries().len(),
            diagnostics_written,
            "the flushed warning must be marked written so it is not re-emitted by a later flush"
        );
    }
}

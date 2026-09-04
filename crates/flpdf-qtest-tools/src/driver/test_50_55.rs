use std::ffi::OsStr;
use std::io::{Read, Seek, Write};

use flpdf::form_field_object_helper::FormFieldObjectHelper;
use flpdf::json_inspect::pdf_object_to_json;
use flpdf::page_document_helper::PageDocumentHelper;
use flpdf::writer::{ObjectStreamMode, PdfWriter};
use flpdf::{Error, ObjectHandle, Pdf};

use super::{emit_new_diagnostics, os_str_diagnostic_bytes};

/// Text of the "form field object must be indirect" internal error used by
/// [`run_test_51`]/[`run_test_52`] below.
///
/// qpdf's `QPDFFormFieldObjectHelper` wraps a `QPDFObjectHandle` directly and
/// tolerates a direct (non-indirect) field object -- `setFieldAttribute`'s
/// underlying `replaceKey` is simply a no-op on a null/malformed handle
/// (`QPDFObjectHandle::replaceKey`, `libqpdf/QPDFObjectHandle.cc:1199-1209`).
/// flpdf's `FormFieldObjectHelper::new` instead requires a real
/// [`flpdf::ObjectRef`] identity, so a direct field object (never produced by
/// a well-formed `/Fields`/`/Kids` array, which the PDF spec requires to hold
/// indirect references) has no faithful path here.
const FIELD_MUST_BE_INDIRECT: &str = "form field object must be indirect";

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

    // `ObjectHandle::merge_resources` requires both operands already
    // resolved ("a no-op unless both `self` and `other` are dictionaries",
    // checked via `as_dictionary`, which "never performs resolution
    // itself") -- unlike qpdf's `mergeResources`, whose `isDictionary()`/
    // `getKey()` calls dereference implicitly. Resolve each handle once,
    // then mutate the same canonical handles that came from the trailer;
    // qpdf's merge operation never replaces an indirect identity with a
    // copied terminal value.
    pdf.resolve(&d1_handle)?;
    pdf.resolve(&d2_handle)?;
    let d1 = d1_handle.clone();
    let d2 = d2_handle.clone();
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    d1.merge_resources(&d2, None)?;
    pdf.mark_object_handle_dirty(&d1)?;

    // `d1.getJSON(JSON::LATEST)` uses qpdf's default
    // `dereference_indirect = false` (`include/qpdf/QPDFObjectHandle.hh`):
    // if `/Dict1` is stored as an indirect reference, this still prints only
    // its own "N G R" unparse as a JSON string
    // (`QPDFObjectHandle::getJSON`, `libqpdf/QPDFObjectHandle.cc:1613-1627`)
    // even though `merge_resources` above already mutated the object it
    // points to. `pdf_object_to_json` implements the identical
    // never-resolve-the-top-level contract, so passing the *original*
    // `d1_handle` here (not the resolved `d1`) is what reproduces that; for
    // a direct `/Dict1` the two handles share the same state, so this still
    // shows the merged dictionary.
    let json = pdf_object_to_json(&d1_handle).map_err(|error| Error::System(error.to_string()))?;
    let unparsed = json.unparse()?;
    stdout.write_all(&unparsed)?;
    writeln!(stdout)?;

    // Top-level type mismatch: `d2.getKey("/k1")` need not itself be a
    // dictionary (deliberately mismatched by this test). qpdf's
    // `mergeResources` call happens unconditionally regardless of that
    // value's type; whether it turns out to be a no-op depends on the
    // resolved type, matching `merge_resources`'s own no-op contract for a
    // non-dictionary `other`.
    let d2_k1_handle = d2.get_key(b"/k1");
    pdf.resolve(&d2_k1_handle)?;
    let d2_k1 = d2_k1_handle.clone();
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    d1.merge_resources(&d2_k1, None)?;
    pdf.mark_object_handle_dirty(&d1)?;

    // qpdf iterates `d1`'s top-level keys whose already-merged value is itself
    // a dictionary, printing the sorted names returned by
    // `getResourceNames` (`qpdf/test_driver.cc:1940-1953`,
    // `libqpdf/QPDFObjectHandle.cc:1156-1170`). The canonical Rust primitive
    // owns the same receiver/value resolution boundary and returns the raw
    // dictionary keys, so the driver only performs the qpdf consumer's byte
    // output step here.
    let resource_names = d1.get_resource_names()?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    for name in resource_names {
        stdout.write_all(&name)?;
        stdout.write_all(b"\n")?;
    }
    Ok(())
}

/// Resolve one handle hop that qpdf would traverse via `getArrayItem`,
/// draining any repair diagnostics the resolution itself surfaces before
/// the caller reads the resolved value -- matching `test_0_1.rs`'s own
/// resolve-then-drain pattern.
fn resolve_and_drain<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
    filename: &[u8],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<(ObjectHandle, Option<flpdf::ObjectRef>)> {
    pdf.resolve(handle)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    Ok((handle.clone(), handle.object_ref()))
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
    let root_handle = pdf.trailer_key_handle(b"Root");
    let (root, _) = resolve_and_drain(
        pdf,
        &root_handle,
        filename,
        stdout,
        stderr,
        diagnostics_written,
    )?;

    let acroform_handle = root.get_key(b"/AcroForm");
    let (acroform, _) = resolve_and_drain(
        pdf,
        &acroform_handle,
        filename,
        stdout,
        stderr,
        diagnostics_written,
    )?;

    let fields_handle = acroform.get_key(b"/Fields");
    let (fields, _) = resolve_and_drain(
        pdf,
        &fields_handle,
        filename,
        stdout,
        stderr,
        diagnostics_written,
    )?;

    // qpdf's `getArrayNItems`/`getArrayItem` on a non-array both warn and
    // behave as an empty array; `as_array().unwrap_or_default()` matches
    // that fallback.
    for item in fields.as_array().unwrap_or_default() {
        let (field, field_ref) =
            resolve_and_drain(pdf, &item, filename, stdout, stderr, diagnostics_written)?;

        let t_handle = field.get_key(b"/T");
        let (t, _) = resolve_and_drain(
            pdf,
            &t_handle,
            filename,
            stdout,
            stderr,
            diagnostics_written,
        )?;
        let Some(raw) = t.as_string() else {
            continue;
        };
        let utf8 = flpdf::pdf_string::utf8_value(&raw);

        if utf8 == b"r1" {
            writeln!(stdout, "setting r1 via parent")?;
            let field_ref =
                field_ref.ok_or_else(|| Error::System(FIELD_MUST_BE_INDIRECT.to_string()))?;
            {
                let mut foh = FormFieldObjectHelper::new(field_ref, pdf);
                foh.set_value(ObjectHandle::name(b"2".to_vec()), true)?;
            }
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        } else if utf8 == b"r2" {
            writeln!(stdout, "setting r2 via child")?;
            let kids_handle = field.get_key(b"/Kids");
            let (kids, _) = resolve_and_drain(
                pdf,
                &kids_handle,
                filename,
                stdout,
                stderr,
                diagnostics_written,
            )?;
            // qpdf's `getArrayItem(1)` on a too-short array warns and
            // returns null rather than crashing; a null "field" then makes
            // `QPDFFormFieldObjectHelper::setV`'s underlying `replaceKey`
            // a silent no-op. flpdf's `FormFieldObjectHelper::new` requires
            // a real `ObjectRef`, so that specific out-of-range case has no
            // faithful path here and instead surfaces as an internal error
            // below -- the fixtures this test is written against always
            // provide a real second `/Kids` entry.
            let kid_handle = kids
                .as_array()
                .unwrap_or_default()
                .into_iter()
                .nth(1)
                .unwrap_or_else(ObjectHandle::null);
            let (_kid, kid_ref) = resolve_and_drain(
                pdf,
                &kid_handle,
                filename,
                stdout,
                stderr,
                diagnostics_written,
            )?;
            let kid_ref =
                kid_ref.ok_or_else(|| Error::System(FIELD_MUST_BE_INDIRECT.to_string()))?;
            {
                let mut foh = FormFieldObjectHelper::new(kid_ref, pdf);
                foh.set_value(ObjectHandle::name(b"3".to_vec()), true)?;
            }
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        } else if utf8 == b"checkbox1" {
            writeln!(stdout, "turning checkbox1 on")?;
            let field_ref =
                field_ref.ok_or_else(|| Error::System(FIELD_MUST_BE_INDIRECT.to_string()))?;
            {
                let mut foh = FormFieldObjectHelper::new(field_ref, pdf);
                // The value that eventually gets set is based on what's
                // allowed in /N and may not match this value (matches qpdf's
                // own comment: `setV` maps any non-`/Off` name to "checked").
                foh.set_value(ObjectHandle::name(b"Sure".to_vec()), true)?;
            }
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        } else if utf8 == b"checkbox2" {
            writeln!(stdout, "turning checkbox2 off")?;
            let field_ref =
                field_ref.ok_or_else(|| Error::System(FIELD_MUST_BE_INDIRECT.to_string()))?;
            {
                let mut foh = FormFieldObjectHelper::new(field_ref, pdf);
                foh.set_value(ObjectHandle::name(b"Off".to_vec()), true)?;
            }
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
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

    let root_handle = pdf.trailer_key_handle(b"Root");
    let (root, _) = resolve_and_drain(
        pdf,
        &root_handle,
        filename,
        stdout,
        stderr,
        diagnostics_written,
    )?;

    let acroform_handle = root.get_key(b"/AcroForm");
    let (acroform, _) = resolve_and_drain(
        pdf,
        &acroform_handle,
        filename,
        stdout,
        stderr,
        diagnostics_written,
    )?;

    let fields_handle = acroform.get_key(b"/Fields");
    let (fields, _) = resolve_and_drain(
        pdf,
        &fields_handle,
        filename,
        stdout,
        stderr,
        diagnostics_written,
    )?;

    for item in fields.as_array().unwrap_or_default() {
        let (field, field_ref) =
            resolve_and_drain(pdf, &item, filename, stdout, stderr, diagnostics_written)?;

        let t_handle = field.get_key(b"/T");
        let (t, _) = resolve_and_drain(
            pdf,
            &t_handle,
            filename,
            stdout,
            stderr,
            diagnostics_written,
        )?;
        let Some(raw) = t.as_string() else {
            continue;
        };
        let utf8 = flpdf::pdf_string::utf8_value(&raw);

        if utf8 == b"list1" {
            writeln!(stdout, "setting list1 value")?;
            let field_ref =
                field_ref.ok_or_else(|| Error::System(FIELD_MUST_BE_INDIRECT.to_string()))?;
            // `newString` stores `arg2`'s raw bytes as-is -- unlike
            // `newUnicodeString`, this is qpdf's literal-string
            // constructor, not a UTF-8-to-UTF-16 conversion. (`set_value`
            // below still re-encodes it as a Unicode string for a text
            // field, matching `QPDFFormFieldObjectHelper::setV`'s own
            // `value.isString()` branch.)
            let value = ObjectHandle::string(os_str_diagnostic_bytes(arg2).into_owned());
            let mut foh = FormFieldObjectHelper::new(field_ref, pdf);
            foh.set_value(value, true)?;
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

    let new_object = pdf.make_indirect_object_handle(ObjectHandle::string(b"potato".to_vec()))?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    stdout.write_all(b"new object: ")?;
    stdout.write_all(&new_object.unparse())?;
    stdout.write_all(b"\n")?;

    root.replace_key(b"/Q1", new_object)?;
    pdf.mark_object_handle_dirty(&root)?;

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
    let _pages = helper.get_all_pages()?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    // qpdf builds the empty `/QTest` array before its per-page loop
    // (`QPDFObjectHandle qtest = QPDFObjectHandle::newArray();`); kept here
    // for the same order of operations even though nothing below can
    // populate or use it.
    let _qtest = ObjectHandle::array(Vec::new());

    // GAP(QPDFPageObjectHelper::getFormXObjectForPage): for each page, qpdf
    // appends `ph.getFormXObjectForPage()` (default
    // `handle_from_transformation = true`) and
    // `ph.getFormXObjectForPage(false)` to `/QTest`, replaces the
    // trailer's `/QTest` with the finished array, then writes `a.pdf` in
    // QDF/static-ID mode (`libqpdf/QPDFPageObjectHelper.cc`). flpdf's
    // page-to-Form-XObject conversion exists (`page_form_xobject.rs`,
    // `get_form_xobject_for_page`) but that module is declared
    // `pub(crate) mod page_form_xobject` (`lib.rs:136`), so it is
    // unreachable from this crate. Since the array this test's entire
    // output depends on can never be built, nothing below this point --
    // including the QDF/static-ID `a.pdf` write, which would otherwise
    // fabricate a trailer without the array qpdf actually produces -- is
    // emitted. The `get_all_pages()` call above is real, not gapped: it is
    // qpdf's own repair pass over `/Pages` (`QPDFPageDocumentHelper::
    // getAllPages`), so any repair diagnostics it surfaces still reach
    // stderr through `emit_new_diagnostics` even though -- unlike a full
    // qpdf run -- no `a.pdf` capturing the repaired tree is ever written.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{resolve_and_drain, run_test_50, run_test_51};
    use flpdf::{ObjectHandle, Pdf, PdfOpenOptions};
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
            pdf.mark_object_handle_dirty(kid)
                .expect("mark radio widget dirty");
        }
        let acroform = ObjectHandle::dictionary(vec![(
            b"/Fields".to_vec(),
            ObjectHandle::array(vec![radio, checkbox, checkbox2, radio2]),
        )]);
        let root = pdf.root_handle().expect("root");
        root.replace_key(b"/AcroForm", acroform)
            .expect("install AcroForm");
        pdf.mark_object_handle_dirty(&root)
            .expect("mark catalog dirty");
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

    #[test]
    fn resolve_and_drain_returns_the_canonical_handle_identity() {
        let mut pdf = Pdf::open_mem_owned_with_options(
            include_bytes!("../../../../tests/fixtures/minimal.pdf").to_vec(),
            PdfOpenOptions::default(),
        )
        .expect("open minimal fixture");
        let root = pdf.trailer_key_handle(b"Root");
        let expected_ref = pdf.root_ref();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = pdf.repair_diagnostics().entries().len();

        let (resolved, object_ref) = resolve_and_drain(
            &mut pdf,
            &root,
            b"minimal.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("resolve root");

        assert_eq!(object_ref, expected_ref);
        assert!(resolved.as_dictionary().is_some());
        assert!(stderr.is_empty());
        assert!(stdout.is_empty());
    }
}

//! Integration tests for [`flpdf::FileSpec`] and [`flpdf::EmbeddedFileStream`].
//!
//! All tests build minimal in-memory PDFs without touching the filesystem.
//! The PDF byte sequences are hand-crafted to exercise the typed accessor
//! methods.  A separate test also opens the real fixture
//! `tests/fixtures/compat/attachment-two-page.pdf` to validate against a
//! production-generated document.

use flpdf::pipeline::Pipeline;
use flpdf::{
    add_attachment_from_path, ascii_filename_fallback, encode_utf16be, extract_attachment,
    format_pdf_date, md5_checksum, Dictionary, EmbeddedFileStream, Error, FileParamDates, FileSpec,
    FileSpecBuilder, Object, ObjectHandle, ObjectRef, Pdf, StreamDataProvider,
};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::rc::Rc;

mod common;
use common::{write_with_settings_and_mapping, WriterTestSettings};

// ── Minimal PDF builder ───────────────────────────────────────────────────────

/// Build a minimal one-page PDF that contains one `/Filespec` (obj 5) pointing
/// at one `/EmbeddedFile` stream (obj 6).
///
/// Object layout:
///   1 0 R  Catalog   (/Names /EmbeddedFiles → 3 0 R)
///   2 0 R  Pages     (/Kids [4 0 R])
///   3 0 R  Name-tree node  (/Names [(attachment.txt) 5 0 R])
///   4 0 R  Page
///   5 0 R  Filespec  (/F /UF /Desc /AFRelationship /EF << /F 6 0 R /UF 6 0 R >>)
///   6 0 R  EmbeddedFile stream  (uncompressed payload b"Hello, world!\n")
fn build_attachment_pdf(filespec_extras: &str, ef_params: &str, payload: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();

    // 1 0 R — Catalog
    offsets.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Names 3 0 R >>\nendobj\n");

    // 2 0 R — Pages
    offsets.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [ 4 0 R ] /Count 1 >>\nendobj\n");

    // 3 0 R — EmbeddedFiles name tree (flat leaf)
    offsets.insert(3, out.len() as u64);
    out.extend_from_slice(
        b"3 0 obj\n<< /EmbeddedFiles << /Names [ (attachment.txt) 5 0 R ] >> >>\nendobj\n",
    );

    // 4 0 R — Page
    offsets.insert(4, out.len() as u64);
    out.extend_from_slice(
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [ 0 0 612 792 ] >>\nendobj\n",
    );

    // 5 0 R — Filespec
    offsets.insert(5, out.len() as u64);
    let filespec_body = format!(
        "5 0 obj\n<< /Type /Filespec /F (attachment.txt) /UF (attachment.txt) /EF << /F 6 0 R /UF 6 0 R >> {filespec_extras} >>\nendobj\n"
    );
    out.extend_from_slice(filespec_body.as_bytes());

    // 6 0 R — EmbeddedFile stream (no compression for simplicity)
    offsets.insert(6, out.len() as u64);
    let ef_header = format!(
        "6 0 obj\n<< /Type /EmbeddedFile /Length {} {ef_params} >>\nstream\n",
        payload.len()
    );
    out.extend_from_slice(ef_header.as_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(b"\nendstream\nendobj\n");

    // xref
    let xref_start = out.len() as u64;
    let n = 7u32; // 0..6
    out.extend_from_slice(format!("xref\n0 {n}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for i in 1..n {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&i]).as_bytes());
    }
    let trailer = format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

/// Add an unrelated xref entry whose body is malformed. This lets a helper
/// setter exercise the failure path in Pdf's direct-owner discovery without
/// making the Filespec owner itself malformed.
fn attachment_pdf_with_malformed_unrelated_object() -> Vec<u8> {
    let mut out = build_attachment_pdf("", "", b"data");
    let xref_start = out
        .windows(b"xref\n".len())
        .position(|window| window == b"xref\n")
        .expect("fixture has xref");
    out.truncate(xref_start);

    let mut offsets = Vec::new();
    for number in 1..=6 {
        let header = format!("{number} 0 obj\n");
        offsets.push(
            out.windows(header.len())
                .position(|window| window == header.as_bytes())
                .expect("fixture object header") as u64,
        );
    }
    let malformed_offset = out.len() as u64;
    out.extend_from_slice(b"7 0 obj\n<< /Broken [ >>\nendobj\n");

    let rebuilt_xref = out.len();
    out.extend_from_slice(b"xref\n0 8\n0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("{malformed_offset:010} 00000 n \n").as_bytes());
    out.extend_from_slice(
        format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{rebuilt_xref}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

#[test]
fn embedded_file_resolves_indirect_ef_dictionary() {
    let mut pdf = open(build_attachment_pdf("", "", b"payload"));
    let Object::Dictionary(mut fs_dict) = pdf.resolve_object(ObjectRef::new(5, 0)).unwrap() else {
        panic!("expected filespec dict");
    };
    let ef_dict = fs_dict.get("EF").cloned().expect("/EF dict");
    pdf.set_object(ObjectRef::new(7, 0), ef_dict);
    fs_dict.insert("EF", Object::Reference(ObjectRef::new(7, 0)));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(fs_dict));
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();

    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");

    assert_eq!(ef.payload().unwrap(), b"payload");
}

#[test]
fn embedded_file_payload_reads_lazy_original_source() {
    let mut pdf = open(build_attachment_pdf("", "", b"lazy attachment"));

    // Resolve the canonical stream through the new raw-data primitive first.
    // A parsed stream keeps no replacement buffer: its bytes remain in the
    // document source, precisely as qpdf's QPDF_Stream does.
    let stream = pdf.get_object_handle(ObjectRef::new(6, 0));
    assert_eq!(
        stream.get_raw_stream_data().unwrap().as_slice(),
        b"lazy attachment"
    );
    assert!(stream.as_stream_data().is_none());

    let mut filespec =
        FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).expect("filespec");
    let embedded = filespec
        .embedded_file()
        .expect("embedded file lookup")
        .expect("embedded file stream");
    assert_eq!(embedded.payload().expect("payload"), b"lazy attachment");
}

// ── Helper: open PDF from bytes ───────────────────────────────────────────────

fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(bytes)).expect("Pdf::open")
}

// ── FileSpec::filename ────────────────────────────────────────────────────────

#[test]
fn filename_returns_f_bytes() {
    let bytes = build_attachment_pdf("", "", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let name = fs.filename().expect("filename()");
    assert_eq!(name, Some(b"attachment.txt".to_vec()));
}

// ── qpdf-shaped FileSpec getters ────────────────────────────────────────────

#[test]
fn get_filename_prefers_uf_and_decodes_pdf_text() {
    // This fails if lookup uses alphabetical order, if it chooses /F before
    // /UF, or if it exposes the stored UTF-16BE bytes rather than qpdf's
    // getUTF8Value() result.
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    let Object::Dictionary(mut filespec) = pdf.resolve_object(ObjectRef::new(5, 0)).unwrap() else {
        panic!("expected filespec dictionary");
    };
    filespec.insert("F", Object::String(b"fallback.txt".to_vec()));
    filespec.insert("UF", Object::String(encode_utf16be("東京.txt")));
    filespec.insert("Unix", Object::String(b"unix.txt".to_vec()));
    filespec.insert("DOS", Object::String(b"dos.txt".to_vec()));
    filespec.insert("Mac", Object::String(b"mac.txt".to_vec()));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(filespec));

    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    assert_eq!(fs.get_filename().unwrap(), "東京.txt".as_bytes().to_vec());
}

#[test]
fn filespec_helper_accepts_a_direct_dictionary_handle() {
    // QPDFFileSpecObjectHelper accepts QPDFObjectHandle, not only an
    // indirect object number. A direct dictionary must therefore participate
    // in the same preferred filename lookup.
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    let direct = ObjectHandle::dictionary(vec![(
        b"UF".to_vec(),
        ObjectHandle::string(encode_utf16be("直接.txt")),
    )]);
    let mut filespec = FileSpec::new(direct, &mut pdf).unwrap();

    filespec.set_description("direct description").unwrap();

    assert_eq!(
        filespec.get_filename().unwrap(),
        "直接.txt".as_bytes().to_vec()
    );
    assert_eq!(
        filespec.get_description().unwrap(),
        b"direct description".to_vec()
    );
}

#[test]
fn qpdf_helpers_treat_a_nonmatching_direct_handle_as_empty_or_noop() {
    // qpdf helpers accept any ObjectHandle. Its null-dictionary semantics make
    // all Filespec getters empty and its setters no-ops; an EF helper similarly
    // has no metadata and rejects payload access when its handle is not a
    // stream.
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    let mut filespec = FileSpec::new(ObjectHandle::null(), &mut pdf).unwrap();
    assert_eq!(filespec.filename().unwrap(), None);
    assert_eq!(filespec.uf().unwrap(), None);
    assert_eq!(filespec.description().unwrap(), None);
    assert_eq!(filespec.af_relationship().unwrap(), None);
    assert_eq!(filespec.get_description().unwrap(), Vec::<u8>::new());
    assert_eq!(filespec.get_filename().unwrap(), Vec::<u8>::new());
    assert!(filespec.get_filenames().unwrap().is_empty());
    assert!(filespec.get_embedded_file_streams().unwrap().is_null());
    assert!(filespec.get_embedded_file_stream("").unwrap().is_null());
    assert!(filespec.embedded_file().unwrap().is_none());
    filespec.set_description("ignored").unwrap();
    filespec.set_filename("ignored", None).unwrap();
    drop(filespec);

    let mut embedded = EmbeddedFileStream::new(ObjectHandle::null(), &mut pdf).unwrap();
    assert!(embedded.payload().is_err());
    assert_eq!(embedded.mimetype().unwrap(), None);
    assert_eq!(embedded.creation_date().unwrap(), None);
    assert_eq!(embedded.modification_date().unwrap(), None);
    assert_eq!(embedded.checksum().unwrap(), None);
    assert_eq!(embedded.size().unwrap(), None);
    embedded.set_creation_date(b"ignored").unwrap();
    embedded.set_mod_date(b"ignored").unwrap();
    embedded.set_subtype(b"ignored").unwrap();
}

#[test]
fn qpdf_public_helper_surface_uses_object_handles_and_fluent_setters() {
    // The qpdf headers expose object-handle factories/getters and fluent
    // setters. Keep that boundary in the Rust translation instead of leaking
    // ObjectRef or raw Object through the qpdf-shaped methods.
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    let embedded_handle = EmbeddedFileStream::create_ef_stream(&mut pdf, b"payload").unwrap();
    assert!(embedded_handle.object_ref().is_some());
    let filespec_handle =
        FileSpec::create_file_spec(&mut pdf, "handle.txt", embedded_handle.clone()).unwrap();
    assert!(filespec_handle.object_ref().is_some());

    let mut filespec = FileSpec::new(filespec_handle, &mut pdf).unwrap();
    filespec
        .set_description("description")
        .unwrap()
        .set_filename("handle.txt", None)
        .unwrap();
    let stream_handle = filespec.get_embedded_file_stream("F").unwrap();
    assert_eq!(stream_handle.object_ref(), embedded_handle.object_ref());
    assert!(filespec
        .get_embedded_file_streams()
        .unwrap()
        .as_dictionary()
        .is_some());
    drop(filespec);

    let mut embedded = EmbeddedFileStream::new(embedded_handle, &mut pdf).unwrap();
    embedded
        .set_creation_date(b"D:20260101000000Z")
        .unwrap()
        .set_mod_date(b"D:20260202000000Z")
        .unwrap()
        .set_subtype(b"text/plain")
        .unwrap();
    assert_eq!(embedded.get_subtype().unwrap(), b"text/plain");
}

#[test]
fn factories_allocate_after_handle_only_objects() {
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    let handle_only = pdf
        .make_indirect_object_handle(ObjectHandle::integer(17))
        .unwrap();
    let handle_only_ref = handle_only.object_ref().unwrap();

    let embedded = EmbeddedFileStream::create_ef_stream(&mut pdf, b"payload").unwrap();
    assert_ne!(embedded.object_ref(), Some(handle_only_ref));
    assert_eq!(
        pdf.resolve_object(handle_only_ref).unwrap(),
        Object::Integer(17),
        "factory allocation must not clobber a handle-only object"
    );
}

#[test]
fn filespec_factory_indirectizes_a_direct_embedded_stream() {
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    let direct_stream = ObjectHandle::stream(
        ObjectHandle::dictionary(vec![]),
        Rc::new(b"payload".to_vec()),
    );
    let filespec_handle =
        FileSpec::create_file_spec(&mut pdf, "direct.bin", direct_stream).unwrap();

    let mut filespec = FileSpec::new(filespec_handle, &mut pdf).unwrap();
    let stream_handle = filespec.get_embedded_file_stream("F").unwrap();
    assert!(stream_handle.object_ref().is_some());
    assert_eq!(
        filespec
            .embedded_file()
            .unwrap()
            .unwrap()
            .payload()
            .unwrap(),
        b"payload"
    );
}

#[test]
fn filespec_direct_setter_persists_without_resolving_unrelated_object() {
    // This fails if a direct Filespec child needs a document-wide owner scan:
    // object 7 is malformed but unrelated to the reachable owner at object 8.
    // qpdf mutates the shared direct child without touching it.
    let mut pdf = open(attachment_pdf_with_malformed_unrelated_object());
    let owner_ref = ObjectRef::new(8, 0);
    let mut owner_dict = Dictionary::new();
    owner_dict.insert("Filespec", Object::Dictionary(Dictionary::new()));
    pdf.set_object(owner_ref, Object::Dictionary(owner_dict));
    let catalog_ref = pdf.root_ref().expect("fixture has a catalog");
    let Object::Dictionary(mut catalog) = pdf.resolve_object(catalog_ref).unwrap() else {
        panic!("fixture catalog must be a dictionary");
    };
    catalog.insert("TestOwner", Object::Reference(owner_ref));
    pdf.set_object(catalog_ref, Object::Dictionary(catalog));
    let owner = pdf.get_object_handle(owner_ref);
    pdf.resolve(&owner).unwrap();
    let direct_filespec = owner.get_key(b"/Filespec");

    let mut filespec = FileSpec::new(direct_filespec.clone(), &mut pdf).unwrap();
    filespec.set_description("new description").unwrap();
    drop(filespec);

    let settings = WriterTestSettings {
        object_streams: flpdf::ObjectStreamMode::Disable,
        ..WriterTestSettings::default()
    };
    let (out, mapping) = write_with_settings_and_mapping(&mut pdf, &settings, &[owner_ref])
        .expect("qpdf writer output");
    let owner_output = mapping[&owner_ref];

    let mut reopened = open(out);
    let Object::Dictionary(owner) = reopened.resolve_object(owner_output).unwrap() else {
        panic!("expected owner dictionary");
    };
    let Some(Object::Dictionary(filespec)) = owner.get("Filespec") else {
        panic!("expected direct Filespec dictionary");
    };

    assert_eq!(
        filespec.get("Desc"),
        Some(&Object::String(b"new description".to_vec())),
        "the direct Filespec mutation must be emitted in the fresh rewrite"
    );
}

#[test]
fn direct_embedded_stream_metadata_setters_update_existing_and_new_params() {
    // qpdf's helpers also accept direct stream handles. Exercise both direct
    // /Params update and creation: neither path has an indirect owner to mark.
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    let direct_params = ObjectHandle::dictionary(vec![(
        b"Params".to_vec(),
        ObjectHandle::dictionary(vec![(
            b"CreationDate".to_vec(),
            ObjectHandle::string(b"old".to_vec()),
        )]),
    )]);
    let mut existing = EmbeddedFileStream::new(
        ObjectHandle::stream(direct_params, Rc::new(b"existing".to_vec())),
        &mut pdf,
    )
    .unwrap();
    existing.set_creation_date(b"new").unwrap();
    existing.set_subtype(b"application/test").unwrap();
    assert_eq!(existing.creation_date().unwrap(), Some(b"new".to_vec()));
    assert_eq!(
        existing.mimetype().unwrap(),
        Some(b"application/test".to_vec())
    );

    let mut absent = EmbeddedFileStream::new(
        ObjectHandle::stream(
            ObjectHandle::dictionary(vec![]),
            Rc::new(b"absent".to_vec()),
        ),
        &mut pdf,
    )
    .unwrap();
    absent.set_mod_date(b"created").unwrap();
    assert_eq!(
        absent.modification_date().unwrap(),
        Some(b"created".to_vec())
    );
}

#[test]
fn filespec_factory_rejects_an_indirect_handle_from_another_pdf() {
    let mut source = open(build_attachment_pdf("", "", b"source"));
    let foreign = EmbeddedFileStream::create_ef_stream(&mut source, b"payload").unwrap();
    let mut destination = open(build_attachment_pdf("", "", b"destination"));

    assert!(matches!(
        FileSpec::create_file_spec(&mut destination, "foreign.bin", foreign),
        Err(Error::Unsupported(message)) if message == "embedded-file handle belongs to another Pdf"
    ));
}

#[test]
fn embedded_file_helper_chases_a_reference_holder_to_the_terminal_stream() {
    let mut pdf = open(build_attachment_pdf("", "", b"payload"));
    let stream_ref = ObjectRef::new(6, 0);
    let holder_ref = ObjectRef::new(7, 0);
    pdf.set_object(holder_ref, Object::Reference(stream_ref));
    let holder = pdf.get_object_handle(holder_ref);
    let mut embedded = EmbeddedFileStream::new(holder, &mut pdf).unwrap();

    assert_eq!(embedded.payload().unwrap(), b"payload");
    embedded.set_subtype(b"application/pdf").unwrap();
    drop(embedded);
    let Object::Stream(stream) = pdf.resolve_object(stream_ref).unwrap() else {
        panic!("expected stream");
    };
    assert_eq!(
        stream.dict.get("Subtype"),
        Some(&Object::Name(b"application/pdf".to_vec()))
    );
}

#[test]
fn get_filenames_returns_only_string_name_keys_as_utf8() {
    // This fails if a non-string name key leaks into qpdf's getFilenames
    // result, or if the qpdf UTF-8 text conversion is skipped.
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    let Object::Dictionary(mut filespec) = pdf.resolve_object(ObjectRef::new(5, 0)).unwrap() else {
        panic!("expected filespec dictionary");
    };
    filespec.insert("UF", Object::String(encode_utf16be("日本語.txt")));
    filespec.insert("F", Object::String(b"fallback.txt".to_vec()));
    filespec.insert("Unix", Object::Integer(7));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(filespec));

    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    assert_eq!(
        fs.get_filenames().unwrap(),
        BTreeMap::from([
            ("/F".to_string(), b"fallback.txt".to_vec()),
            ("/UF".to_string(), "日本語.txt".as_bytes().to_vec()),
        ])
    );
}

#[test]
fn get_filename_returns_none_when_no_recognized_entry_is_a_string() {
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    let Object::Dictionary(mut filespec) = pdf.resolve_object(ObjectRef::new(5, 0)).unwrap() else {
        panic!("expected filespec dictionary");
    };
    filespec.insert("UF", Object::Integer(7));
    filespec.insert("F", Object::Name(b"not-a-string".to_vec()));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(filespec));

    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    assert_eq!(fs.get_filename().unwrap(), Vec::<u8>::new());
}

#[test]
fn get_embedded_file_stream_returns_requested_entry_and_ef_dictionary() {
    // This fails if a named request applies the preferred-key stream filter,
    // or if the raw /EF dictionary is reconstructed instead of returned.
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();

    assert_eq!(
        fs.get_embedded_file_stream("F").unwrap().object_ref(),
        Some(ObjectRef::new(6, 0))
    );
    let entries = fs
        .get_embedded_file_streams()
        .unwrap()
        .as_dictionary()
        .expect("expected /EF dictionary");
    assert_eq!(
        entries
            .get(b"/F".as_slice())
            .and_then(ObjectHandle::object_ref),
        Some(ObjectRef::new(6, 0))
    );
    assert_eq!(
        entries
            .get(b"/UF".as_slice())
            .and_then(ObjectHandle::object_ref),
        Some(ObjectRef::new(6, 0))
    );
}

#[test]
fn get_embedded_file_stream_accepts_qpdf_filename_keys() {
    // qpdf's getFilenames() returns slash-prefixed keys, and each must be
    // directly usable as getEmbeddedFileStream(key).
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();

    assert_eq!(
        fs.get_embedded_file_stream("/F").unwrap().object_ref(),
        Some(ObjectRef::new(6, 0))
    );
}

#[test]
fn get_embedded_file_stream_returns_null_when_no_candidate_is_a_stream() {
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    let Object::Dictionary(mut filespec) = pdf.resolve_object(ObjectRef::new(5, 0)).unwrap() else {
        panic!("expected filespec dictionary");
    };
    let mut entries = Dictionary::new();
    entries.insert("UF", Object::Integer(3));
    filespec.insert("EF", Object::Dictionary(entries));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(filespec));

    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    assert!(fs.get_embedded_file_stream("").unwrap().is_null());
}

#[test]
fn qpdf_string_getters_preserve_invalid_utf8_bytes_without_panicking() {
    // QPDFObjectHandle::getUTF8Value() returns std::string, whose bytes need
    // not be valid UTF-8 when a stored string uses an explicit UTF-8 BOM.
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    let Object::Dictionary(mut filespec) = pdf.resolve_object(ObjectRef::new(5, 0)).unwrap() else {
        panic!("expected filespec dictionary");
    };
    filespec.insert("Desc", Object::String(vec![0xef, 0xbb, 0xbf, 0xff]));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(filespec));

    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    assert_eq!(fs.get_description().unwrap(), vec![0xff]);
}

#[test]
fn qpdf_string_getters_resolve_indirect_strings_before_selecting_names() {
    // QPDFObjectHandle::isString() dereferences its object handle, so a
    // higher-priority indirect /UF must win over a direct /F. The same holds
    // for getDescription() and every value returned by getFilenames().
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    pdf.set_object(
        ObjectRef::new(7, 0),
        Object::String(encode_utf16be("東京.txt")),
    );
    pdf.set_object(
        ObjectRef::new(8, 0),
        Object::String(b"fallback.txt".to_vec()),
    );
    pdf.set_object(ObjectRef::new(9, 0), Object::String(encode_utf16be("概要")));
    let Object::Dictionary(mut filespec) = pdf.resolve_object(ObjectRef::new(5, 0)).unwrap() else {
        panic!("expected filespec dictionary");
    };
    filespec.insert("UF", Object::Reference(ObjectRef::new(7, 0)));
    filespec.insert("F", Object::Reference(ObjectRef::new(8, 0)));
    filespec.insert("Desc", Object::Reference(ObjectRef::new(9, 0)));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(filespec));

    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    assert_eq!(fs.get_description().unwrap(), "概要".as_bytes().to_vec());
    assert_eq!(fs.get_filename().unwrap(), "東京.txt".as_bytes().to_vec());
    assert_eq!(
        fs.get_filenames().unwrap(),
        BTreeMap::from([
            ("/F".to_string(), b"fallback.txt".to_vec()),
            ("/UF".to_string(), "東京.txt".as_bytes().to_vec()),
        ])
    );
}

#[test]
fn filespec_factories_reject_exhausted_object_number_space() {
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    pdf.set_object(ObjectRef::new(u32::MAX, 0), Object::Null);
    let embedded_file = pdf.get_object_handle(ObjectRef::new(6, 0));

    let embedded_error = EmbeddedFileStream::create_ef_stream(&mut pdf, b"payload")
        .expect_err("qpdf newStream must reject the signed object-number boundary");
    assert_eq!(
        embedded_error.to_string(),
        "unsupported PDF feature: max object id is too high to create new objects"
    );

    let filespec_error = FileSpec::create_file_spec(&mut pdf, b"report.txt", embedded_file)
        .expect_err("Filespec factory must reject the exhausted allocation boundary");
    // qpdf's QPDF::nextObjGen uses the signed object-id boundary for every
    // makeIndirectObject call (`libqpdf/QPDF.cc:1872-1879`), including the
    // Filespec factory. The old Object-based helper exposed its own
    // "object-number space exhausted" text; that is not a qpdf contract.
    assert!(
        matches!(filespec_error, Error::Unsupported(message) if message == "max object id is too high to create new objects"),
        "Filespec factory must return qpdf's allocation error instead of wrapping object 0"
    );
}

// ── FileSpec::uf ──────────────────────────────────────────────────────────────

#[test]
fn uf_returns_uf_bytes() {
    let bytes = build_attachment_pdf("", "", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let uf = fs.uf().expect("uf()");
    assert_eq!(uf, Some(b"attachment.txt".to_vec()));
}

// ── FileSpec::description ─────────────────────────────────────────────────────

#[test]
fn description_returns_desc_when_present() {
    let bytes = build_attachment_pdf("/Desc (A test file)", "", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let desc = fs.description().expect("description()");
    assert_eq!(desc, Some(b"A test file".to_vec()));
}

#[test]
fn description_returns_none_when_absent() {
    let bytes = build_attachment_pdf("", "", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    assert_eq!(fs.description().expect("description()"), None);
}

// ── FileSpec::af_relationship ─────────────────────────────────────────────────

#[test]
fn af_relationship_returns_name_when_present() {
    let bytes = build_attachment_pdf("/AFRelationship /Source", "", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let rel = fs.af_relationship().expect("af_relationship()");
    assert_eq!(rel, Some(b"Source".to_vec()));
}

#[test]
fn af_relationship_returns_none_when_absent() {
    let bytes = build_attachment_pdf("", "", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    assert_eq!(fs.af_relationship().expect("af_relationship()"), None);
}

// ── EmbeddedFileStream::payload ───────────────────────────────────────────────

#[test]
fn payload_returns_raw_decoded_bytes() {
    let expected = b"Hello, world!\n";
    let bytes = build_attachment_pdf("", "", expected);
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()");
    let ef = ef.expect("Some(EmbeddedFileStream)");
    let payload = ef.payload().expect("payload()");
    assert_eq!(payload, expected.to_vec());
}

// ── EmbeddedFileStream::mimetype ──────────────────────────────────────────────

#[test]
fn mimetype_returns_subtype_name() {
    let bytes = build_attachment_pdf("", "/Subtype /application#2fplain", b"text");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    // /Subtype is stored as raw name bytes (no leading /); the `#2f`
    // name escape decodes to `/`.
    assert_eq!(
        ef.mimetype().expect("mimetype()"),
        Some(b"application/plain".to_vec())
    );
}

#[test]
fn mimetype_returns_none_when_absent() {
    let bytes = build_attachment_pdf("", "", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(ef.mimetype().expect("mimetype()"), None);
}

// ── EmbeddedFileStream: /Params sub-dict ─────────────────────────────────────

/// Build a PDF with a `/Params` sub-dictionary on the EmbeddedFile stream.
fn build_pdf_with_params(params_body: &str, payload: &[u8]) -> Vec<u8> {
    let ef_params = format!("/Params << {params_body} >>");
    build_attachment_pdf("", &ef_params, payload)
}

#[test]
fn creation_date_returns_raw_pdf_date() {
    let bytes = build_pdf_with_params("/CreationDate (D:20260101000000Z)", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let date = ef.creation_date().expect("creation_date()");
    assert_eq!(date, Some(b"D:20260101000000Z".to_vec()));
}

#[test]
fn modification_date_returns_raw_pdf_date() {
    let bytes = build_pdf_with_params("/ModDate (D:20260202120000+09'00')", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let date = ef.modification_date().expect("modification_date()");
    assert_eq!(date, Some(b"D:20260202120000+09'00'".to_vec()));
}

#[test]
fn checksum_returns_raw_bytes() {
    // 16-byte MD5 checksum as a PDF hex string
    let bytes = build_pdf_with_params("/CheckSum <542266a1f565c3e5d8cfbd55eb7dfa40>", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(
        ef.checksum().expect("checksum()"),
        Some(vec![
            0x54, 0x22, 0x66, 0xa1, 0xf5, 0x65, 0xc3, 0xe5, 0xd8, 0xcf, 0xbd, 0x55, 0xeb, 0x7d,
            0xfa, 0x40,
        ])
    );
}

#[test]
fn size_returns_integer() {
    let bytes = build_pdf_with_params("/Size 95", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let sz = ef.size().expect("size()");
    assert_eq!(sz, Some(95));
}

#[test]
fn qpdf_size_clamps_to_unsigned_int_range() {
    let bytes = build_pdf_with_params("/Size 4294967296", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(ef.get_size().unwrap(), u32::MAX as usize);
}

#[test]
fn qpdf_size_returns_zero_for_negative_integer() {
    let bytes = build_pdf_with_params("/Size -1", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(ef.get_size().unwrap(), 0);
}

#[test]
fn indirect_metadata_scalars_are_dereferenced() {
    // qpdf's QPDFObjectHandle dereferences every value before `isString`,
    // `isInteger`, or `isName` inspects it. /Params itself is not the only
    // indirection point: each scalar and /Subtype may be a holder reference.
    let mut pdf = open(build_pdf_with_params(
        "/CreationDate (D:20260101000000Z) /ModDate (D:20260202000000Z) /Size 95 /CheckSum <00112233445566778899aabbccddeeff>",
        b"data",
    ));
    let Object::Stream(mut stream) = pdf.resolve_object(ObjectRef::new(6, 0)).unwrap() else {
        panic!("expected embedded-file stream");
    };
    let Object::Dictionary(mut params) = stream.dict.remove("Params").unwrap() else {
        panic!("expected /Params dictionary");
    };

    for (key, object_ref) in [
        ("CreationDate", ObjectRef::new(7, 0)),
        ("ModDate", ObjectRef::new(8, 0)),
        ("Size", ObjectRef::new(9, 0)),
        ("CheckSum", ObjectRef::new(10, 0)),
    ] {
        let value = params.remove(key).expect("fixture metadata value");
        pdf.set_object(object_ref, value);
        params.insert(key, Object::Reference(object_ref));
    }
    stream.dict.insert("Params", Object::Dictionary(params));
    pdf.set_object(
        ObjectRef::new(12, 0),
        Object::Name(b"application/pdf".to_vec()),
    );
    stream
        .dict
        .insert("Subtype", Object::Reference(ObjectRef::new(11, 0)));
    pdf.set_object(
        ObjectRef::new(11, 0),
        Object::Reference(ObjectRef::new(12, 0)),
    );
    pdf.set_object(ObjectRef::new(6, 0), Object::Stream(stream));

    let mut filespec =
        FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let embedded = filespec.embedded_file().unwrap().expect("embedded file");
    assert_eq!(
        embedded.mimetype().unwrap(),
        Some(b"application/pdf".to_vec())
    );
    assert_eq!(
        embedded.creation_date().unwrap(),
        Some(b"D:20260101000000Z".to_vec())
    );
    assert_eq!(
        embedded.modification_date().unwrap(),
        Some(b"D:20260202000000Z".to_vec())
    );
    assert_eq!(embedded.size().unwrap(), Some(95));
    assert_eq!(
        embedded.checksum().unwrap(),
        Some(vec![
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ])
    );
}

// ── qpdf-shaped EmbeddedFileStream metadata and mutation ────────────────────

#[test]
fn embedded_file_setters_update_the_live_stream_and_qpdf_getters() {
    // This fails if a setter only changes a retained stream copy, if /Params
    // is not created, or if qpdf's UTF-8 string view is skipped on readback.
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    {
        let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
        let mut ef = fs.embedded_file().unwrap().expect("embedded file");
        ef.set_creation_date(b"D:20260101000000Z").unwrap();
        ef.set_mod_date(b"D:20260202000000Z").unwrap();
        ef.set_subtype(b"application/pdf").unwrap();

        assert_eq!(
            ef.get_creation_date().unwrap(),
            b"D:20260101000000Z".to_vec()
        );
        assert_eq!(ef.get_mod_date().unwrap(), b"D:20260202000000Z".to_vec());
        assert_eq!(ef.get_subtype().unwrap(), b"application/pdf".to_vec());
        assert_eq!(ef.get_size().unwrap(), 0);
        assert_eq!(ef.get_checksum().unwrap(), Vec::<u8>::new());
    }

    let Object::Stream(stream) = pdf.resolve_object(ObjectRef::new(6, 0)).unwrap() else {
        panic!("expected embedded-file stream");
    };
    let Object::Dictionary(params) = stream.dict.get("Params").unwrap() else {
        panic!("expected /Params dictionary");
    };
    assert_eq!(
        params.get("CreationDate"),
        Some(&Object::String(b"D:20260101000000Z".to_vec()))
    );
    assert_eq!(
        params.get("ModDate"),
        Some(&Object::String(b"D:20260202000000Z".to_vec()))
    );
    assert_eq!(
        stream.dict.get("Subtype"),
        Some(&Object::Name(b"application/pdf".to_vec()))
    );
}

#[test]
fn metadata_setter_invalidates_a_previously_materialized_stream() {
    // A qpdf object handle is the live value. Once metadata changes through
    // that handle, a later legacy resolve and the writer must not reuse the
    // pre-mutation materialized stream dictionary.
    let mut pdf = open(build_attachment_pdf("", "", b"payload"));
    let Object::Stream(before) = pdf.resolve_object(ObjectRef::new(6, 0)).unwrap() else {
        panic!("expected embedded-file stream");
    };
    assert!(before.dict.get("Subtype").is_none());

    {
        let mut filespec =
            FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
        let mut embedded = filespec.embedded_file().unwrap().expect("embedded file");
        embedded.set_subtype(b"application/pdf").unwrap();
    }

    let Object::Stream(after) = pdf.resolve_object(ObjectRef::new(6, 0)).unwrap() else {
        panic!("expected embedded-file stream");
    };
    assert_eq!(
        after.dict.get("Subtype"),
        Some(&Object::Name(b"application/pdf".to_vec()))
    );
}

#[test]
fn embedded_file_setter_updates_indirect_params_dictionary() {
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    let mut params = Dictionary::new();
    params.insert("Size", Object::Integer(4));
    pdf.set_object(ObjectRef::new(7, 0), Object::Dictionary(params));
    let Object::Stream(mut stream) = pdf.resolve_object(ObjectRef::new(6, 0)).unwrap() else {
        panic!("expected embedded-file stream");
    };
    stream
        .dict
        .insert("Params", Object::Reference(ObjectRef::new(7, 0)));
    pdf.set_object(ObjectRef::new(6, 0), Object::Stream(stream));

    {
        let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
        fs.embedded_file()
            .unwrap()
            .unwrap()
            .set_creation_date(b"D:20260101000000Z")
            .unwrap();
    }

    let Object::Dictionary(params) = pdf.resolve_object(ObjectRef::new(7, 0)).unwrap() else {
        panic!("expected indirect /Params dictionary");
    };
    assert_eq!(
        params.get("CreationDate"),
        Some(&Object::String(b"D:20260101000000Z".to_vec()))
    );
}

#[test]
fn embedded_file_setter_replaces_non_dictionary_indirect_params() {
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    pdf.set_object(ObjectRef::new(7, 0), Object::Integer(4));
    let Object::Stream(mut stream) = pdf.resolve_object(ObjectRef::new(6, 0)).unwrap() else {
        panic!("expected embedded-file stream");
    };
    stream
        .dict
        .insert("Params", Object::Reference(ObjectRef::new(7, 0)));
    pdf.set_object(ObjectRef::new(6, 0), Object::Stream(stream));

    {
        let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
        fs.embedded_file()
            .unwrap()
            .unwrap()
            .set_mod_date(b"D:20260202000000Z")
            .unwrap();
    }

    let Object::Stream(stream) = pdf.resolve_object(ObjectRef::new(6, 0)).unwrap() else {
        panic!("expected embedded-file stream");
    };
    let Object::Dictionary(params) = stream.dict.get("Params").unwrap() else {
        panic!("expected replacement /Params dictionary");
    };
    assert_eq!(
        params.get("ModDate"),
        Some(&Object::String(b"D:20260202000000Z".to_vec()))
    );
}

#[test]
fn qpdf_factories_create_filespec_and_embedded_file_objects() {
    // This fails if either factory omits qpdf's /Type, computed EF parameters,
    // shared /EF references, or newUnicodeString filename storage.
    let mut pdf = open(build_attachment_pdf("", "", b"seed"));
    let ef_handle = EmbeddedFileStream::create_ef_stream(&mut pdf, b"payload").unwrap();
    let ef_ref = ef_handle.object_ref().unwrap();
    let filespec_handle = FileSpec::create_file_spec(&mut pdf, "report.txt", ef_handle).unwrap();
    let filespec_ref = filespec_handle.object_ref().unwrap();

    let Object::Stream(ef) = pdf.resolve_object(ef_ref).unwrap() else {
        panic!("expected EmbeddedFile stream");
    };
    assert_eq!(
        ef.dict.get("Type"),
        Some(&Object::Name(b"EmbeddedFile".to_vec()))
    );
    let Object::Dictionary(params) = ef.dict.get("Params").unwrap() else {
        panic!("expected /Params");
    };
    assert_eq!(params.get("Size"), Some(&Object::Integer(7)));
    assert_eq!(
        params.get("CheckSum"),
        Some(&Object::String(md5_checksum(b"payload")))
    );

    let Object::Dictionary(filespec) = pdf.resolve_object(filespec_ref).unwrap() else {
        panic!("expected Filespec dictionary");
    };
    assert_eq!(
        filespec.get("Type"),
        Some(&Object::Name(b"Filespec".to_vec()))
    );
    assert_eq!(
        filespec.get("F"),
        Some(&Object::String(b"report.txt".to_vec()))
    );
    assert_eq!(
        filespec.get("UF"),
        Some(&Object::String(b"report.txt".to_vec()))
    );
    let Object::Dictionary(ef_entries) = filespec.get("EF").unwrap() else {
        panic!("expected /EF");
    };
    assert_eq!(ef_entries.get("F"), Some(&Object::Reference(ef_ref)));
    assert_eq!(ef_entries.get("UF"), Some(&Object::Reference(ef_ref)));
}

#[test]
fn filespec_setters_use_qpdf_unicode_and_compatibility_rules() {
    // This fails if description/filename writes bypass newUnicodeString, or
    // if a non-empty compatibility name does not replace /F alone.
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    {
        let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
        fs.set_description("概要").unwrap();
        fs.set_filename("東京.txt", Some(b"fallback.txt".as_slice()))
            .unwrap();
    }

    let Object::Dictionary(filespec) = pdf.resolve_object(ObjectRef::new(5, 0)).unwrap() else {
        panic!("expected Filespec dictionary");
    };
    assert_eq!(
        filespec.get("Desc"),
        Some(&Object::String(encode_utf16be("概要")))
    );
    assert_eq!(
        filespec.get("UF"),
        Some(&Object::String(encode_utf16be("東京.txt")))
    );
    assert_eq!(
        filespec.get("F"),
        Some(&Object::String(b"fallback.txt".to_vec()))
    );
}

#[test]
fn filespec_set_filename_preserves_non_utf8_compatibility_bytes() {
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    {
        let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
        fs.set_filename("東京.txt", Some(&[0x80, 0xff][..]))
            .unwrap();
    }

    let Object::Dictionary(filespec) = pdf.resolve_object(ObjectRef::new(5, 0)).unwrap() else {
        panic!("expected Filespec dictionary");
    };
    assert_eq!(filespec.get("F"), Some(&Object::String(vec![0x80, 0xff])));
}

#[test]
fn filespec_set_filename_normalizes_non_utf8_unicode_bytes_like_qpdf() {
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    {
        let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
        fs.set_filename([0xff], None).unwrap();
    }

    let Object::Dictionary(filespec) = pdf.resolve_object(ObjectRef::new(5, 0)).unwrap() else {
        panic!("expected Filespec dictionary");
    };
    assert_eq!(
        filespec.get("UF"),
        Some(&Object::String(encode_utf16be("�")))
    );
    assert_eq!(filespec.get("F"), filespec.get("UF"));
}

#[test]
fn builder_composes_helpers_for_qpdf_unicode_description() {
    let mut pdf = open(build_attachment_pdf("", "", b"seed"));
    let filespec_ref = FileSpecBuilder::new("report.txt", b"payload".as_slice())
        .description("概要")
        .build(&mut pdf)
        .unwrap();

    let Object::Dictionary(filespec) = pdf.resolve_object(filespec_ref).unwrap() else {
        panic!("expected Filespec dictionary");
    };
    assert_eq!(
        filespec.get("Desc"),
        Some(&Object::String(encode_utf16be("概要")))
    );
}

#[test]
fn qpdf_path_factories_read_payload_and_make_filespec() {
    // This fails if the Rust path equivalents of qpdf's file-provider
    // overloads fail to preserve the file's decoded bytes.
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), b"from-path").unwrap();
    let mut pdf = open(build_attachment_pdf("", "", b"seed"));

    let ef_handle = EmbeddedFileStream::create_ef_stream_from_path(&mut pdf, file.path()).unwrap();
    let ef_ref = ef_handle.object_ref().unwrap();
    let fs_ref = FileSpec::create_file_spec_from_path(&mut pdf, "path.txt", file.path())
        .unwrap()
        .object_ref()
        .unwrap();
    assert_eq!(
        pdf.resolve_object(ef_ref)
            .unwrap()
            .as_stream()
            .unwrap()
            .data,
        b"from-path"
    );
    assert_eq!(
        ef_handle.get_raw_stream_data().unwrap().as_slice(),
        b"from-path",
        "path provider must remain readable after finalization"
    );
    assert_eq!(
        ef_handle.get_raw_stream_data().unwrap().as_slice(),
        b"from-path",
        "path provider must be repeatable"
    );
    let mut fs = FileSpec::new(pdf.get_object_handle(fs_ref), &mut pdf).unwrap();
    assert_eq!(
        fs.embedded_file().unwrap().unwrap().payload().unwrap(),
        b"from-path"
    );
}

#[test]
fn qpdf_path_factory_maps_open_errors_at_the_provider_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing-attachment.bin");
    let mut pdf = Pdf::empty().unwrap();

    let Err(error) = EmbeddedFileStream::create_ef_stream_from_path(&mut pdf, &missing) else {
        panic!("a missing provider path must fail while computing embedded metadata");
    };

    assert_eq!(
        error.to_string(),
        format!("open {}: No such file or directory", missing.display())
    );
}

struct ChunkedProvider {
    chunks: Vec<Vec<u8>>,
    calls: Rc<Cell<usize>>,
}

struct FailingProvider;

struct ErrorProvider;

impl StreamDataProvider for FailingProvider {
    fn supports_retry(&self) -> bool {
        true
    }

    fn provide_stream_data_with_retry_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
        _suppress_warnings: bool,
        _will_retry: bool,
    ) -> flpdf::Result<bool> {
        pipeline.write(b"partial").map_err(Error::from)?;
        pipeline.finish().map_err(Error::from)?;
        Ok(false)
    }
}

impl StreamDataProvider for ErrorProvider {
    fn provide_stream_data_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        _pipeline: &mut dyn Pipeline,
    ) -> flpdf::Result<()> {
        Err(Error::System("provider failure".to_owned()))
    }
}

impl StreamDataProvider for ChunkedProvider {
    fn provide_stream_data_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
    ) -> flpdf::Result<()> {
        self.calls.set(self.calls.get() + 1);
        for chunk in &self.chunks {
            pipeline.write(chunk).map_err(Error::from)?;
        }
        pipeline.finish().map_err(Error::from)
    }
}

#[test]
fn qpdf_provider_factory_is_deferred_and_publishes_streamed_metadata() {
    let payload = b"first\0second\nthird".to_vec();
    let chunks = vec![
        payload[..5].to_vec(),
        payload[5..11].to_vec(),
        payload[11..].to_vec(),
    ];
    let calls = Rc::new(Cell::new(0));
    let provider = Rc::new(ChunkedProvider {
        chunks,
        calls: Rc::clone(&calls),
    });
    let mut pdf = Pdf::empty().expect("empty PDF");

    let stream = EmbeddedFileStream::create_ef_stream_from_provider(&mut pdf, provider)
        .expect("provider factory");

    assert_eq!(calls.get(), 1, "finalization pipes the provider once");
    let dict = stream.as_stream_dict().expect("embedded-file dictionary");
    assert_eq!(
        dict.get_key(b"/Type").as_name(),
        Some(b"EmbeddedFile".to_vec())
    );
    let params = dict.get_key(b"/Params");
    assert_eq!(
        params.get_key(b"/Size").as_integer(),
        Some(payload.len() as i64)
    );
    assert_eq!(
        params.get_key(b"/CheckSum").as_string(),
        Some(md5_checksum(&payload))
    );

    assert_eq!(
        stream
            .get_raw_stream_data()
            .expect("repeat provider pipe")
            .as_slice(),
        payload
    );
    assert_eq!(calls.get(), 2, "provider remains repeatable and deferred");
}

#[test]
fn qpdf_provider_factory_with_failed_pipe_does_not_publish_embedded_metadata() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let stream =
        EmbeddedFileStream::create_ef_stream_from_provider(&mut pdf, Rc::new(FailingProvider))
            .expect("qpdf warns and returns the stream after a failed provider pipe");

    let dict = stream.as_stream_dict().expect("embedded-file dictionary");
    assert_eq!(
        dict.get_key(b"/Type").as_name(),
        Some(b"EmbeddedFile".to_vec())
    );
    assert!(
        !dict.has_key(b"/Params"),
        "failed pipe must not publish metadata"
    );
    assert!(pdf.repair_diagnostics().entries().iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("unable to get stream data for new embedded file stream")
    }));
}

#[test]
fn qpdf_provider_factory_propagates_provider_errors() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let error =
        EmbeddedFileStream::create_ef_stream_from_provider(&mut pdf, Rc::new(ErrorProvider))
            .expect_err("provider errors must cross the qpdf stream boundary");

    assert_eq!(error.to_string(), "provider failure");
}

#[test]
fn params_absent_returns_none_for_all_fields() {
    let bytes = build_attachment_pdf("", "", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(ef.creation_date().expect("creation_date"), None);
    assert_eq!(ef.modification_date().expect("modification_date"), None);
    assert_eq!(ef.checksum().expect("checksum"), None);
    assert_eq!(ef.size().expect("size"), None);
}

#[test]
fn qpdf_getters_use_empty_defaults_for_missing_string_values() {
    // qpdf's std::string getters cannot distinguish an absent field from an
    // empty string. The Rust qpdf-shaped surface mirrors that observable
    // contract; raw optional accessors remain the inspection-level API.
    let mut pdf = open(build_attachment_pdf("", "", b"data"));
    let mut filespec =
        FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let embedded = filespec.embedded_file().unwrap().expect("embedded file");

    assert_eq!(embedded.get_creation_date().unwrap(), Vec::<u8>::new());
    assert_eq!(embedded.get_mod_date().unwrap(), Vec::<u8>::new());
    assert_eq!(embedded.get_subtype().unwrap(), Vec::<u8>::new());
    assert_eq!(embedded.get_checksum().unwrap(), Vec::<u8>::new());
}

// ── embedded_file returns None when /EF is missing ───────────────────────────

#[test]
fn embedded_file_returns_none_when_ef_absent() {
    // A Filespec without /EF
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    offsets.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [ 3 0 R ] /Count 1 >>\nendobj\n");
    offsets.insert(3, out.len() as u64);
    out.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [ 0 0 612 792 ] >>\nendobj\n",
    );
    offsets.insert(4, out.len() as u64);
    out.extend_from_slice(b"4 0 obj\n<< /Type /Filespec /F (readme.txt) >>\nendobj\n");
    let xref_start = out.len() as u64;
    out.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for i in 1..5u32 {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&i]).as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );

    let mut pdf = open(out);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(4, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()");
    assert!(ef.is_none(), "expected None when /EF absent");
}

// ── Fixture test: attachment-two-page.pdf ─────────────────────────────────────

#[test]
fn fixture_attachment_two_page() {
    // Locate the fixture relative to the crate root.
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");
    if !fixture.exists() {
        // If the fixture is not present (e.g. in a stripped CI checkout), skip.
        eprintln!("skipping fixture test: {:?} not found", fixture);
        return;
    }

    let data = std::fs::read(&fixture).expect("read fixture");
    let mut pdf = Pdf::open(Cursor::new(data)).expect("Pdf::open fixture");

    // In attachment-two-page.pdf:
    //   5 0 R  Filespec  (/F (attachment.txt) /UF (attachment.txt) /EF << /F 8 0 R /UF 8 0 R >>)
    //   8 0 R  EmbeddedFile stream (FlateDecode, /Params /Size 95)
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();

    // filename
    let name = fs.filename().expect("filename()");
    assert_eq!(name, Some(b"attachment.txt".to_vec()));

    // uf
    let uf = fs.uf().expect("uf()");
    assert_eq!(uf, Some(b"attachment.txt".to_vec()));

    // embedded file
    let ef = fs.embedded_file().expect("embedded_file()");
    let ef = ef.expect("Some(EmbeddedFileStream)");

    // payload: decompress the FlateDecode stream; fixture declares /Size 95
    let payload = ef.payload().expect("payload()");
    assert_eq!(
        payload.len(),
        95,
        "expected 95 uncompressed bytes, got {}",
        payload.len()
    );

    // size
    let sz = ef.size().expect("size()");
    assert_eq!(sz, Some(95));

    // creation_date: raw PDF date
    let created = ef.creation_date().expect("creation_date()");
    assert_eq!(created, Some(b"D:20260101000000Z".to_vec()));

    // modification_date
    let modified = ef.modification_date().expect("modification_date()");
    assert_eq!(modified, Some(b"D:20260101000000Z".to_vec()));

    // checksum: 16 raw bytes (MD5)
    let cs = ef.checksum().expect("checksum()");
    let cs = cs.expect("Some checksum");
    assert_eq!(cs.len(), 16);
}

// ── /EF key priority order (UF > F > Unix > Mac > DOS) ───────────────────────

/// Build a PDF whose `/EF` sub-dict maps the given key→stream pairs.
/// Each `(key, payload)` becomes a distinct `/EmbeddedFile` stream so the
/// caller can tell which key was selected by inspecting the returned payload.
fn build_pdf_with_ef_keys(pairs: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();

    offsets.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [ 4 0 R ] /Count 1 >>\nendobj\n");
    offsets.insert(4, out.len() as u64);
    out.extend_from_slice(
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [ 0 0 612 792 ] >>\nendobj\n",
    );

    // Streams start at object number 6; build the /EF dict referencing them.
    let mut ef_entries = String::new();
    for (i, (key, _)) in pairs.iter().enumerate() {
        let obj = 6 + i as u32;
        ef_entries.push_str(&format!("/{key} {obj} 0 R "));
    }
    offsets.insert(5, out.len() as u64);
    let filespec =
        format!("5 0 obj\n<< /Type /Filespec /F (a.txt) /EF << {ef_entries}>> >>\nendobj\n");
    out.extend_from_slice(filespec.as_bytes());

    for (i, (_, payload)) in pairs.iter().enumerate() {
        let obj = 6 + i as u32;
        offsets.insert(obj, out.len() as u64);
        let hdr = format!(
            "{obj} 0 obj\n<< /Type /EmbeddedFile /Length {} >>\nstream\n",
            payload.len()
        );
        out.extend_from_slice(hdr.as_bytes());
        out.extend_from_slice(payload);
        out.extend_from_slice(b"\nendstream\nendobj\n");
    }

    let xref_start = out.len() as u64;
    let n = 6 + pairs.len() as u32;
    out.extend_from_slice(format!("xref\n0 {n}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for i in 1..n {
        match offsets.get(&i) {
            Some(off) => {
                out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
            }
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

#[test]
fn embedded_file_prefers_uf_over_f() {
    // /F and /UF point at different streams; /UF must win.
    let bytes = build_pdf_with_ef_keys(&[("F", b"from-F"), ("UF", b"from-UF")]);
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(ef.payload().expect("payload()"), b"from-UF".to_vec());
}

#[test]
fn embedded_file_falls_back_to_platform_keys() {
    // Only /Unix present — must still resolve via the fallback chain.
    let bytes = build_pdf_with_ef_keys(&[("Unix", b"unix-payload")]);
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(ef.payload().expect("payload()"), b"unix-payload".to_vec());
}

// ── Indirect /Params reference resolution ────────────────────────────────────

#[test]
fn params_indirect_reference_resolves() {
    // EmbeddedFile stream's /Params is an indirect reference (7 0 R).
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    offsets.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [ 4 0 R ] /Count 1 >>\nendobj\n");
    offsets.insert(4, out.len() as u64);
    out.extend_from_slice(
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [ 0 0 612 792 ] >>\nendobj\n",
    );
    offsets.insert(5, out.len() as u64);
    out.extend_from_slice(
        b"5 0 obj\n<< /Type /Filespec /F (a.txt) /EF << /F 6 0 R >> >>\nendobj\n",
    );
    let payload = b"indirect-params";
    offsets.insert(6, out.len() as u64);
    out.extend_from_slice(
        format!(
            "6 0 obj\n<< /Type /EmbeddedFile /Length {} /Params 7 0 R >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(payload);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    offsets.insert(7, out.len() as u64);
    out.extend_from_slice(
        b"7 0 obj\n<< /Size 15 /CheckSum (0123456789abcdef) /CreationDate (D:20260101000000Z) >>\nendobj\n",
    );
    let xref_start = out.len() as u64;
    let n = 8u32;
    out.extend_from_slice(format!("xref\n0 {n}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for i in 1..n {
        match offsets.get(&i) {
            Some(off) => {
                out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
            }
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );

    let mut pdf = open(out);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(ef.size().expect("size()"), Some(15));
    assert_eq!(
        ef.checksum().expect("checksum()"),
        Some(b"0123456789abcdef".to_vec())
    );
    assert_eq!(
        ef.creation_date().expect("creation_date()"),
        Some(b"D:20260101000000Z".to_vec())
    );
}

#[test]
fn embedded_file_skips_non_stream_higher_priority_key() {
    // /EF << /UF 7 0 R /F 6 0 R >> where 7 0 R is a dictionary (not a
    // stream) and 6 0 R is a valid /EmbeddedFile. /UF is higher priority
    // but must be skipped so /F's stream is returned.
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    offsets.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [ 4 0 R ] /Count 1 >>\nendobj\n");
    offsets.insert(4, out.len() as u64);
    out.extend_from_slice(
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [ 0 0 612 792 ] >>\nendobj\n",
    );
    offsets.insert(5, out.len() as u64);
    out.extend_from_slice(
        b"5 0 obj\n<< /Type /Filespec /F (a.txt) /EF << /UF 7 0 R /F 6 0 R >> >>\nendobj\n",
    );
    let payload = b"from-F-stream";
    offsets.insert(6, out.len() as u64);
    out.extend_from_slice(
        format!(
            "6 0 obj\n<< /Type /EmbeddedFile /Length {} >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(payload);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    offsets.insert(7, out.len() as u64);
    out.extend_from_slice(b"7 0 obj\n<< /NotAStream true >>\nendobj\n");
    let xref_start = out.len() as u64;
    let n = 8u32;
    out.extend_from_slice(format!("xref\n0 {n}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for i in 1..n {
        match offsets.get(&i) {
            Some(off) => {
                out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
            }
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );

    let mut pdf = open(out);
    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(ef.payload().expect("payload()"), payload.to_vec());
}

// ── FileSpecBuilder ───────────────────────────────────────────────────────────

/// Build a minimal one-page PDF in memory and return it as a `Pdf`.
///
/// Object layout:
///   1 0 R  Catalog  (/Pages 2 0 R)
///   2 0 R  Pages    (/Kids [3 0 R])
///   3 0 R  Page
fn build_minimal_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();

    offsets.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    offsets.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [ 3 0 R ] /Count 1 >>\nendobj\n");

    offsets.insert(3, out.len() as u64);
    out.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [ 0 0 612 792 ] >>\nendobj\n",
    );

    let xref_start = out.len() as u64;
    out.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for i in 1u32..4 {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&i]).as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );

    open(out)
}

#[test]
fn attachment_path_facade_rejects_a_path_without_a_basename() {
    let mut pdf = build_minimal_pdf();
    let error = add_attachment_from_path(&mut pdf, b"root", Path::new("/"))
        .expect_err("a root path has no attachment basename");
    assert!(error.to_string().contains("path has no basename"));
}

#[cfg(unix)]
#[test]
fn attachment_path_facade_rejects_a_non_utf8_basename() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let mut pdf = build_minimal_pdf();
    let path = PathBuf::from(OsString::from_vec(vec![b'b', 0xff]));
    let error = add_attachment_from_path(&mut pdf, b"bad", &path)
        .expect_err("a non-UTF-8 basename cannot become /UF");
    assert!(error.to_string().contains("basename is not valid UTF-8"));
}

#[test]
fn ascii_filename_fallback_uses_qpdf_attachment_for_punctuation_only_names() {
    assert_eq!(ascii_filename_fallback("..."), b"attachment");
    assert_eq!(ascii_filename_fallback(""), b"attachment");
}

#[test]
fn extract_attachment_reports_missing_tree_keys_without_available_keys() {
    let mut pdf = build_minimal_pdf();
    let error = extract_attachment(&mut pdf, b"missing").expect_err("tree key is absent");
    assert!(error.to_string().contains("no attachments present"));
}

#[test]
fn extract_attachment_reports_a_filespec_without_a_stream() {
    let mut bytes = build_attachment_pdf("", "", b"payload");
    let old = b"/F 6 0 R /UF 6 0 R";
    let new = b"/F 0 0 R /UF 0 0 R";
    let position = bytes
        .windows(old.len())
        .position(|window| window == old)
        .expect("attachment fixture has both EF references");
    bytes[position..position + old.len()].copy_from_slice(new);
    let mut pdf = open(bytes);
    let error = extract_attachment(&mut pdf, b"attachment.txt")
        .expect_err("null EF candidates must report a missing stream");
    assert!(error
        .to_string()
        .contains("no resolvable /EmbeddedFile stream"));
}

// ── helper: encode_utf16be ────────────────────────────────────────────────────

#[test]
fn encode_utf16be_bom_and_codepoints() {
    let bytes = encode_utf16be("hi");
    // BOM (FE FF) + 'h' (00 68) + 'i' (00 69)
    assert_eq!(bytes, vec![0xFE, 0xFF, 0x00, 0x68, 0x00, 0x69]);
}

#[test]
fn encode_utf16be_empty_string_is_bom_only() {
    assert_eq!(encode_utf16be(""), vec![0xFE, 0xFF]);
}

// ── helper: format_pdf_date ───────────────────────────────────────────────────

#[test]
fn format_pdf_date_utc() {
    assert_eq!(
        format_pdf_date(2026, 1, 1, 0, 0, 0),
        b"D:20260101000000Z".to_vec()
    );
}

#[test]
fn format_pdf_date_nonzero_time() {
    assert_eq!(
        format_pdf_date(2025, 12, 31, 23, 59, 59),
        b"D:20251231235959Z".to_vec()
    );
}

// (the public `escape_pdf_name` helper was removed in roborev #920; name
// escaping is now serializer-internal — see
// `builder_mimetype_with_slash_round_trips_through_pdf_serialization` for the
// end-to-end guarantee.)

// ── helper: md5_checksum ──────────────────────────────────────────────────────

#[test]
fn md5_checksum_length_and_known_value() {
    // MD5 of empty string is d41d8cd98f00b204e9800998ecf8427e
    let cs = md5_checksum(b"");
    assert_eq!(cs.len(), 16);
    assert_eq!(
        cs,
        vec![
            0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8,
            0x42, 0x7e
        ]
    );
}

// ── FileSpecBuilder: round-trip via FileSpec reader ───────────────────────────

/// Round-trip: build a /Filespec with all optional fields set, then read it
/// back through `FileSpec` and `EmbeddedFileStream` and verify every field.
#[test]
fn builder_round_trip_all_fields() {
    let mut pdf = build_minimal_pdf();

    let payload = b"Hello, PDF attachment!\n";
    let dates = FileParamDates {
        creation: Some((2026, 1, 15, 9, 30, 0)),
        modification: Some((2026, 2, 20, 14, 0, 0)),
    };

    let filespec_ref = FileSpecBuilder::new("report.txt", payload.as_slice())
        .mimetype(b"text/plain")
        .description(b"Annual report attachment")
        .af_relationship(b"Data")
        .dates(dates)
        .build(&mut pdf)
        .expect("build()");

    // ── /F (filename) ────────────────────────────────────────────────────────
    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
    let fname = fs.filename().expect("filename()");
    assert_eq!(fname, Some(b"report.txt".to_vec()), "/F mismatch");

    // ── /UF (qpdf newUnicodeString) ──────────────────────────────────────────
    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
    let uf = fs.uf().expect("uf()").expect("/UF should be present");
    assert_eq!(uf, b"report.txt", "ASCII /UF must be PDFDocEncoding");

    // ── /Desc ────────────────────────────────────────────────────────────────
    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
    let desc = fs.description().expect("description()");
    assert_eq!(
        desc,
        Some(b"Annual report attachment".to_vec()),
        "/Desc mismatch"
    );

    // ── /AFRelationship ───────────────────────────────────────────────────────
    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
    let rel = fs.af_relationship().expect("af_relationship()");
    assert_eq!(rel, Some(b"Data".to_vec()), "/AFRelationship mismatch");

    // ── /EmbeddedFile payload ─────────────────────────────────────────────────
    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
    let ef = fs
        .embedded_file()
        .expect("embedded_file()")
        .expect("Some(EmbeddedFileStream)");
    let got_payload = ef.payload().expect("payload()");
    assert_eq!(got_payload, payload.to_vec(), "payload mismatch");

    // ── MIME type (round-trips through name escape) ───────────────────────────
    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let mime = ef.mimetype().expect("mimetype()");
    assert_eq!(
        mime,
        Some(b"text/plain".to_vec()),
        "/Subtype (MIME) mismatch"
    );

    // ── /Params /Size ─────────────────────────────────────────────────────────
    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let sz = ef.size().expect("size()");
    assert_eq!(sz, Some(payload.len() as i64), "/Params /Size mismatch");

    // ── /Params /CheckSum (MD5 of payload) ───────────────────────────────────
    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let cs = ef.checksum().expect("checksum()").expect("Some checksum");
    assert_eq!(cs.len(), 16, "checksum must be 16 bytes");
    assert_eq!(
        cs,
        md5_checksum(payload),
        "checksum must match MD5 of payload"
    );

    // ── /Params /CreationDate ─────────────────────────────────────────────────
    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let cdate = ef.creation_date().expect("creation_date()");
    assert_eq!(
        cdate,
        Some(b"D:20260115093000Z".to_vec()),
        "/Params /CreationDate mismatch"
    );

    // ── /Params /ModDate ──────────────────────────────────────────────────────
    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let mdate = ef.modification_date().expect("modification_date()");
    assert_eq!(
        mdate,
        Some(b"D:20260220140000Z".to_vec()),
        "/Params /ModDate mismatch"
    );
}

/// Round-trip with minimal fields (no optional fields set).
#[test]
fn builder_round_trip_minimal() {
    let mut pdf = build_minimal_pdf();
    let payload = b"tiny";

    let filespec_ref = FileSpecBuilder::new("tiny.bin", payload.as_slice())
        .build(&mut pdf)
        .expect("build()");

    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
    assert_eq!(
        fs.filename().expect("filename()"),
        Some(b"tiny.bin".to_vec())
    );

    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
    let uf = fs.uf().expect("uf()").expect("/UF present");
    assert_eq!(uf, b"tiny.bin", "ASCII /UF must be PDFDocEncoding");

    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
    assert_eq!(fs.description().expect("description()"), None);

    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
    assert_eq!(fs.af_relationship().expect("af_relationship()"), None);

    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(ef.payload().expect("payload()"), payload.to_vec());
    assert_eq!(ef.mimetype().expect("mimetype()"), None);
    assert_eq!(ef.creation_date().expect("creation_date()"), None);
    assert_eq!(ef.modification_date().expect("modification_date()"), None);
    assert_eq!(ef.size().expect("size()"), Some(4));
    assert_eq!(
        ef.checksum().expect("checksum()"),
        Some(md5_checksum(payload))
    );
}

/// /UF follows qpdf's newUnicodeString rule for an ASCII filename.
#[test]
fn builder_uf_uses_pdfdocencoding_for_ascii() {
    let mut pdf = build_minimal_pdf();
    let payload = b"data";
    let filespec_ref = FileSpecBuilder::new("ascii.txt", payload.as_slice())
        .build(&mut pdf)
        .expect("build()");

    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
    let uf = fs.uf().expect("uf()").expect("/UF present");

    assert_eq!(uf, b"ascii.txt");
}

/// /Params date format must follow D:YYYYMMDDHHmmSSZ.
#[test]
fn builder_params_date_format_is_pdf_date() {
    let mut pdf = build_minimal_pdf();
    let payload = b"content";
    let filespec_ref = FileSpecBuilder::new("f.txt", payload.as_slice())
        .dates(FileParamDates {
            creation: Some((2026, 6, 15, 12, 30, 45)),
            modification: None,
        })
        .build(&mut pdf)
        .expect("build()");

    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let cdate = ef.creation_date().expect("creation_date()").expect("Some");
    // D:YYYYMMDDHHmmSSZ
    assert_eq!(cdate, b"D:20260615123045Z".to_vec());
    // Must start with "D:"
    assert!(cdate.starts_with(b"D:"), "PDF date must start with D:");
    // Year must be 4 digits at position 2..6
    assert_eq!(&cdate[2..6], b"2026");
}

/// End-to-end: build a /Filespec whose MIME type contains a `/`
/// (`application/pdf`), serialize the whole document to PDF bytes via
/// `write_pdf`, reopen the serialized bytes, and verify `/Subtype`
/// round-trips back to `application/pdf`.
///
/// This guards the serializer's name-escaping: `Object::Name` holds
/// decoded bytes, so `application/pdf` must be written as
/// `/application#2fpdf` and decoded back on read. Without escaping the
/// `/` would split the name token and corrupt `/Subtype`.
#[test]
fn builder_mimetype_with_slash_round_trips_through_pdf_serialization() {
    let mut pdf = build_minimal_pdf();
    let payload = b"%PDF-1.4 fake nested pdf";

    let filespec_ref = FileSpecBuilder::new("nested.pdf", payload.as_slice())
        .mimetype(b"application/pdf")
        .build(&mut pdf)
        .expect("build()");

    // The qpdf-style full rewrite emits reachable objects. Attach the
    // builder result to the catalog just as a caller would through the
    // embedded-files name tree before writing the document.
    let catalog_ref = pdf.root_ref().expect("minimal fixture has a catalog");
    let Object::Dictionary(mut catalog) = pdf.resolve_object(catalog_ref).unwrap() else {
        panic!("minimal fixture catalog must be a dictionary");
    };
    let mut embedded_files = Dictionary::new();
    embedded_files.insert(
        "Names",
        Object::Array(vec![
            Object::String(b"nested.pdf".to_vec()),
            Object::Reference(filespec_ref),
        ]),
    );
    let mut names = Dictionary::new();
    names.insert("EmbeddedFiles", Object::Dictionary(embedded_files));
    catalog.insert("Names", Object::Dictionary(names));
    pdf.set_object(catalog_ref, Object::Dictionary(catalog));

    // Serialize the whole document to PDF bytes. Keep object streams disabled
    // so the name token itself is visible in the output bytes.
    let settings = WriterTestSettings {
        object_streams: flpdf::ObjectStreamMode::Disable,
        ..WriterTestSettings::default()
    };
    let (serialized, mapping) =
        write_with_settings_and_mapping(&mut pdf, &settings, &[filespec_ref])
            .expect("qpdf writer output");
    let filespec_output = mapping[&filespec_ref];

    // The escaped name must appear literally in the byte stream, and the
    // unescaped form must NOT (which would mean the `/` split the token).
    let needle = b"/application#2fpdf";
    assert!(
        serialized.windows(needle.len()).any(|w| w == needle),
        "serialized PDF must contain escaped /Subtype name /application#2fpdf"
    );

    // Reopen the serialized bytes and read /Subtype back.
    let mut pdf2 = open(serialized);
    let mut fs = FileSpec::new(pdf2.get_object_handle(filespec_output), &mut pdf2).unwrap();
    let ef = fs
        .embedded_file()
        .expect("embedded_file()")
        .expect("Some(EmbeddedFileStream)");
    let mime = ef.mimetype().expect("mimetype()");
    assert_eq!(
        mime,
        Some(b"application/pdf".to_vec()),
        "/Subtype must round-trip back to application/pdf after serialization"
    );
}

// ── Holder-chain (multi-hop indirect) resolution ──────────────────────────────
//
// A value reachable by indirection may sit behind more than one hop
// (`a 0 R -> b 0 R -> value`). The single-hop `resolve_borrowed` used to drop
// such carriers; `resolve_ref_chain` follows them to the terminal. Each test
// below 2-hops ONLY the value under test and keeps every sibling link
// single-hop, so a pre-fix failure attributes cleanly to one site.

/// Site 1: `EmbeddedFileStream::new` resolves `/Params` through a holder chain.
/// `/Params` is `8 0 R -> 9 0 R -> << /Size 42 >>`; `/EF` stays single-hop.
#[test]
fn params_follows_holder_chain() {
    // Start from a PDF whose /Params is the direct dict << /Size 42 >>.
    let mut pdf = open(build_pdf_with_params("/Size 42", b"data"));
    // Rewrite the EmbeddedFile stream's /Params to a two-hop carrier
    // 8 0 R -> 9 0 R -> << /Size 42 >>.
    let Object::Stream(mut ef_stream) = pdf.resolve_object(ObjectRef::new(6, 0)).unwrap() else {
        panic!("expected EmbeddedFile stream");
    };
    let params = ef_stream.dict.get("Params").cloned().expect("/Params");
    pdf.set_object(ObjectRef::new(9, 0), params);
    pdf.set_object(
        ObjectRef::new(8, 0),
        Object::Reference(ObjectRef::new(9, 0)),
    );
    ef_stream
        .dict
        .insert("Params", Object::Reference(ObjectRef::new(8, 0)));
    pdf.set_object(ObjectRef::new(6, 0), Object::Stream(ef_stream));

    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(
        ef.size().expect("size()"),
        Some(42),
        "/Params reached via a two-hop chain must resolve"
    );
}

/// Site 2: `FileSpec::embedded_file` resolves the `/EF` sub-dictionary through a
/// holder chain. `/EF` is `7 0 R -> 8 0 R -> << /F 6 0 R /UF 6 0 R >>`; the
/// stream entries inside `/EF` stay single-hop.
#[test]
fn embedded_file_ef_dict_follows_holder_chain() {
    let mut pdf = open(build_attachment_pdf("", "", b"ef-payload"));
    let Object::Dictionary(mut fs_dict) = pdf.resolve_object(ObjectRef::new(5, 0)).unwrap() else {
        panic!("expected filespec dict");
    };
    let ef_dict = fs_dict.get("EF").cloned().expect("/EF dict");
    // Two-hop carrier: /EF 7 0 R -> 8 0 R -> << /F 6 0 R /UF 6 0 R >>.
    pdf.set_object(ObjectRef::new(8, 0), ef_dict);
    pdf.set_object(
        ObjectRef::new(7, 0),
        Object::Reference(ObjectRef::new(8, 0)),
    );
    fs_dict.insert("EF", Object::Reference(ObjectRef::new(7, 0)));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(fs_dict));

    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(
        ef.payload().expect("payload()"),
        b"ef-payload",
        "/EF reached via a two-hop chain must resolve"
    );
}

/// Site 3: `FileSpec::embedded_file` resolves an `/EF` candidate stream through
/// a holder chain. `/EF /F` and `/UF` are `7 0 R -> 6 0 R -> <stream>`; the
/// `/EF` sub-dictionary itself stays single-hop (direct dict).
#[test]
fn embedded_file_stream_entry_follows_holder_chain() {
    let mut pdf = open(build_attachment_pdf("", "", b"stream-payload"));
    let Object::Dictionary(mut fs_dict) = pdf.resolve_object(ObjectRef::new(5, 0)).unwrap() else {
        panic!("expected filespec dict");
    };
    // /EF stays a direct dict; only its stream entries become two-hop.
    // /EF /F and /UF -> 7 0 R -> 6 0 R (the real stream).
    pdf.set_object(
        ObjectRef::new(7, 0),
        Object::Reference(ObjectRef::new(6, 0)),
    );
    let mut ef_dict = match fs_dict.get("EF").cloned().expect("/EF dict") {
        Object::Dictionary(d) => d,
        _ => panic!("expected /EF dict"),
    };
    ef_dict.insert("F", Object::Reference(ObjectRef::new(7, 0)));
    ef_dict.insert("UF", Object::Reference(ObjectRef::new(7, 0)));
    fs_dict.insert("EF", Object::Dictionary(ef_dict));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(fs_dict));

    let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(
        ef.payload().expect("payload()"),
        b"stream-payload",
        "/EF candidate stream reached via a two-hop chain must resolve"
    );
}

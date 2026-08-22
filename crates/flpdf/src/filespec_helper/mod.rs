//! qpdf correspondence: QPDFFileSpecObjectHelper.cc and QPDFEFStreamObjectHelper.cc.
//! Typed wrappers for `/Filespec` dictionaries and `/EmbeddedFile` streams,
//! plus a builder for constructing them.
//!
//! [`FileSpec`] wraps a `/Filespec` dictionary and exposes ergonomic, typed
//! accessors for all common fields (filename, description, embedded file
//! stream, etc.).  [`EmbeddedFileStream`] wraps the embedded `/EmbeddedFile`
//! stream reachable via the `/EF` sub-dictionary and exposes its payload and
//! metadata (MIME type, dates, checksum, size).
//!
//! [`FileSpecBuilder`] constructs a `/Filespec` dictionary and its associated
//! `/EmbeddedFile` stream in-memory and registers them in a [`crate::Pdf`] document via
//! qpdf's indirect-handle factory.  The returned [`crate::ObjectRef`] can then be inserted into
//! the `/Names /EmbeddedFiles` name tree using
//! [`crate::embedded_files::insert_embedded_file`].
//!
//! [`FileSpec`] and [`EmbeddedFileStream`] own qpdf-shaped object handles and
//! resolve dictionaries from the live document on each operation. Their
//! setters mutate those dictionary handles in place, including an indirect
//! `/Params` dictionary when one is present.
//!
//! # Design
//!
//! PDF key naming follows ISO 32000-1 §7.11.  The `/EF` lookup priority used
//! here mirrors qpdf's `QPDFFileSpecObjectHelper::name_keys` order
//! (`QPDFFileSpecObjectHelper.cc`), which is also what its `preferredcontents`
//! JSON output uses: `/UF` › `/F` › `/Unix` › `/DOS` › `/Mac`.
//!
//! Date strings (e.g. `/Params /CreationDate`) are returned as raw PDF date
//! byte sequences (`D:YYYYMMDDHHmmSSOHH'mm'`).  No date parsing is performed.
//!
//! # Examples
//!
//! ## Read filename and payload from a `/Filespec` object
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::{BufReader, Cursor};
//! use flpdf::{FileSpec, ObjectRef, Pdf};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("with-attachment.pdf")?))?;
//!
//! // Assume we know the /Filespec object reference (e.g. from walking /Names).
//! let filespec_ref = ObjectRef::new(5, 0);
//! let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
//!
//! if let Some(name) = fs.filename()? {
//!     println!("filename: {}", String::from_utf8_lossy(&name));
//! }
//! if let Some(mut ef) = fs.embedded_file()? {
//!     let bytes = ef.payload()?;
//!     println!("{} payload bytes", bytes.len());
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Inspect embedded file metadata
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{FileSpec, ObjectRef, Pdf};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("with-attachment.pdf")?))?;
//! let filespec_ref = ObjectRef::new(5, 0);
//! let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
//!
//! if let Some(mut ef) = fs.embedded_file()? {
//!     if let Some(mime) = ef.mimetype()? {
//!         println!("MIME: {}", String::from_utf8_lossy(&mime));
//!     }
//!     if let Some(created) = ef.creation_date()? {
//!         // raw PDF date string, e.g. b"D:20260101000000Z"
//!         println!("created: {}", String::from_utf8_lossy(&created));
//!     }
//!     if let Some(sz) = ef.size()? {
//!         println!("uncompressed size: {sz}");
//!     }
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod embedded_file_stream;
mod filespec;
mod shared;

pub use embedded_file_stream::EmbeddedFileStream;
pub use filespec::{FileParamDates, FileSpec, FileSpecBuilder};
pub(crate) use shared::qpdf_style_open_error;
pub use shared::{encode_utf16be, format_pdf_date, md5_checksum};

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedded_files::{insert_embedded_file, list_embedded_files};
    use crate::filters::decode_stream_data;
    use crate::job::{
        add_attachment_from_path, extract_attachment, extract_attachment_to_path, write_attachment,
    };
    use crate::{Dictionary, Object, ObjectHandle, ObjectRef, Pdf};
    use std::io::Cursor;

    // ── Minimal PDF fixture ───────────────────────────────────────────────────

    fn minimal_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    fn open_minimal() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open minimal PDF")
    }

    #[test]
    fn builder_rejects_a_non_utf8_filename_without_a_unicode_override() {
        let mut pdf = open_minimal();
        let error = FileSpecBuilder::new(b"\xff.txt", b"payload".as_slice())
            .build(&mut pdf)
            .expect_err("/UF requires an explicit Unicode filename for non-UTF-8 bytes");
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: FileSpecBuilder: filename is not valid UTF-8; cannot encode /UF"
        );
    }

    #[test]
    fn embedded_file_finalizer_rejects_a_non_stream_handle() {
        let mut pdf = open_minimal();
        let error = EmbeddedFileStream::new_from_stream(&mut pdf, ObjectHandle::null())
            .expect_err("the shared finalizer requires a stream handle");

        assert_eq!(
            error.to_string(),
            "EmbeddedFile factory received a non-stream object"
        );
    }

    #[test]
    fn pipe_stream_data_rejects_a_null_embedded_file_handle() {
        let mut pdf = open_minimal();
        let embedded_file = EmbeddedFileStream::new(ObjectHandle::null(), &mut pdf)
            .expect("a direct null handle is a valid wrapper input");
        let mut discard = crate::pipeline::Discard;

        let error = embedded_file
            .pipe_stream_data(&mut discard)
            .expect_err("a null embedded file stream must be rejected");
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: expected an /EmbeddedFile stream object"
        );
    }

    #[test]
    fn filespec_helper_chases_a_reference_holder_to_the_terminal_dictionary() {
        let mut pdf = open_minimal();
        let filespec_ref = ObjectRef::new(5, 0);
        let holder_ref = ObjectRef::new(6, 0);
        let mut filespec = Dictionary::new();
        filespec.insert("F", Object::String(b"terminal.txt".to_vec()));
        pdf.set_object(filespec_ref, Object::Dictionary(filespec));
        pdf.set_object(holder_ref, Object::Reference(filespec_ref));

        let mut helper = FileSpec::new(pdf.get_object_handle(holder_ref), &mut pdf).unwrap();
        assert_eq!(helper.get_filename().unwrap(), b"terminal.txt");
        helper.set_description("terminal description").unwrap();
        drop(helper);

        let filespec = pdf
            .resolve(filespec_ref)
            .unwrap()
            .into_dict()
            .expect("terminal object must be a Filespec dictionary");
        assert_eq!(
            filespec.get("Desc"),
            Some(&Object::String(b"terminal description".to_vec()))
        );
    }

    #[test]
    fn direct_null_filespec_stream_entries_are_empty() {
        let mut pdf = open_minimal();
        let mut helper = FileSpec::new(ObjectHandle::null(), &mut pdf).unwrap();

        assert!(helper
            .get_embedded_file_stream_entries()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn filespec_helper_marks_an_indirect_owner_of_a_direct_dictionary_dirty() {
        let mut pdf = open_minimal();
        let owner_ref = ObjectRef::new(5, 0);
        let mut filespec = Dictionary::new();
        filespec.insert("F", Object::String(b"direct.txt".to_vec()));
        let mut owner = Dictionary::new();
        owner.insert("FS", Object::Dictionary(filespec));
        pdf.set_object(owner_ref, Object::Dictionary(owner));
        let owner = pdf.get_object_handle(owner_ref);
        pdf.resolve_object_handle(&owner).unwrap();
        let direct_filespec = owner.get_key(b"/FS");
        pdf.clear_dirty(owner_ref);

        let mut helper = FileSpec::new(direct_filespec, &mut pdf).unwrap();
        helper.set_description("persisted through owner").unwrap();
        drop(helper);

        assert!(pdf.is_dirty(owner_ref));
    }

    #[test]
    fn helper_constructors_reject_indirect_handles_from_another_pdf() {
        let mut source = open_minimal();
        let foreign_filespec = source.get_object_handle(ObjectRef::new(1, 0));
        let foreign_stream = source.get_object_handle(ObjectRef::new(2, 0));
        let mut destination = open_minimal();

        assert!(FileSpec::new(foreign_filespec, &mut destination).is_err());
        assert!(EmbeddedFileStream::new(foreign_stream, &mut destination).is_err());
    }

    #[test]
    fn filespec_constructor_rejects_a_direct_child_from_another_pdf() {
        let mut source = open_minimal();
        let owner_ref = ObjectRef::new(5, 0);
        let mut filespec = Dictionary::new();
        filespec.insert("F", Object::String(b"foreign.txt".to_vec()));
        let mut owner_dict = Dictionary::new();
        owner_dict.insert("FS", Object::Dictionary(filespec));
        source.set_object(owner_ref, Object::Dictionary(owner_dict));
        let owner = source.get_object_handle(owner_ref);
        source.resolve_object_handle(&owner).unwrap();
        let foreign_direct_filespec = owner.get_key(b"/FS");
        assert!(foreign_direct_filespec.is_direct());

        let mut destination = open_minimal();
        let error = FileSpec::new(foreign_direct_filespec, &mut destination)
            .err()
            .expect("a direct child owned by another Pdf must be rejected");
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: Filespec handle belongs to another Pdf"
        );
    }

    #[test]
    fn rejecting_a_foreign_handle_does_not_reserve_its_object_number() {
        let mut source = open_minimal();
        let foreign = source.get_object_handle(ObjectRef::new(99, 0));
        let mut destination = open_minimal();

        assert!(FileSpec::new(foreign, &mut destination).is_err());
        assert_eq!(
            EmbeddedFileStream::create_ef_stream(&mut destination, b"payload")
                .unwrap()
                .object_ref(),
            Some(ObjectRef::new(4, 0))
        );
    }

    #[test]
    fn create_filespec_rejects_a_foreign_handle_without_registering_its_ref() {
        let mut source = open_minimal();
        let foreign = source.get_object_handle(ObjectRef::new(99, 0));
        let mut destination = open_minimal();

        assert!(FileSpec::create_file_spec(&mut destination, b"foreign.bin", foreign).is_err());
        assert_eq!(
            EmbeddedFileStream::create_ef_stream(&mut destination, b"payload")
                .unwrap()
                .object_ref(),
            Some(ObjectRef::new(4, 0)),
            "rejecting a foreign factory input must not register its object number"
        );
    }

    #[test]
    fn create_filespec_accepts_a_direct_value_with_a_foreign_descendant() {
        let mut source = open_minimal();
        let foreign = source.get_object_handle(ObjectRef::new(99, 0));
        let direct = ObjectHandle::dictionary(vec![(b"Foreign".to_vec(), foreign)]);
        let mut destination = open_minimal();

        assert!(FileSpec::create_file_spec(&mut destination, b"direct.bin", direct).is_ok());
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Resolve the /EmbeddedFile stream dict for a filespec ref.
    fn resolve_ef_stream(
        pdf: &mut Pdf<Cursor<Vec<u8>>>,
        fs_ref: ObjectRef,
    ) -> crate::object::Stream {
        let Some(fs_dict) = pdf
            .resolve_borrowed(fs_ref)
            .expect("resolve filespec")
            .as_dict()
        else {
            panic!("expected dictionary");
        };
        let ef_sub = match fs_dict.get("EF") {
            Some(Object::Dictionary(d)) => d.clone(),
            _ => panic!("missing /EF"),
        };
        let stream_ref = match ef_sub.get("F") {
            Some(Object::Reference(r)) => *r,
            _ => panic!("missing /EF /F ref"),
        };
        match pdf.resolve_borrowed(stream_ref).expect("resolve stream") {
            Object::Stream(s) => s.clone(),
            _ => panic!("expected stream"),
        }
    }

    // ── Tests: FileSpecBuilder with compress(false) — existing behaviour ───────

    #[test]
    fn builder_uncompressed_round_trip() {
        let mut pdf = open_minimal();
        let raw = b"hello world";
        let fs_ref = FileSpecBuilder::new("test.txt", raw.as_ref())
            .build(&mut pdf)
            .expect("build");

        let stream = resolve_ef_stream(&mut pdf, fs_ref);
        // No /Filter in uncompressed stream
        assert!(
            stream.dict.get("Filter").is_none(),
            "uncompressed stream must have no /Filter"
        );
        let decoded = decode_stream_data(&stream.dict, &stream.data).expect("decode");
        assert_eq!(decoded, raw);
    }

    #[test]
    fn builder_compressed_f_and_uf_follow_qpdf_unicode_string_rules() {
        let mut pdf = open_minimal();
        let raw = b"payload";
        let fs_ref = FileSpecBuilder::new("myfile.txt", raw.as_ref())
            .build(&mut pdf)
            .expect("build");

        let Some(fs_dict) = pdf
            .resolve_borrowed(fs_ref)
            .expect("resolve filespec")
            .as_dict()
        else {
            panic!("expected dictionary");
        };
        let f = match fs_dict.get("F") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /F"),
        };
        let uf = match fs_dict.get("UF") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /UF"),
        };
        assert_eq!(f, b"myfile.txt", "/F must be the filename");
        assert_eq!(uf, b"myfile.txt", "/UF must use qpdf newUnicodeString");
    }

    #[test]
    fn builder_allows_distinct_ascii_f_and_unicode_uf() {
        let mut pdf = open_minimal();
        let raw = b"payload";
        let fs_ref = FileSpecBuilder::new("____.pdf", raw.as_ref())
            .uf_filename("レポート.pdf")
            .build(&mut pdf)
            .expect("build");

        let Some(fs_dict) = pdf
            .resolve_borrowed(fs_ref)
            .expect("resolve filespec")
            .as_dict()
        else {
            panic!("expected dictionary");
        };
        let f = match fs_dict.get("F") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /F"),
        };
        let uf = match fs_dict.get("UF") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /UF"),
        };

        assert_eq!(f, b"____.pdf", "/F must be ASCII fallback");
        assert_eq!(
            uf,
            encode_utf16be("レポート.pdf"),
            "/UF must preserve the Unicode filename"
        );
    }

    // ── Tests: FileSpecBuilder → insert_embedded_file → list ─────────────────

    #[test]
    fn compressed_filespec_retrievable_via_list() {
        let mut pdf = open_minimal();
        let raw = b"retrievable payload";
        let fs_ref = FileSpecBuilder::new("list-test.txt", raw.as_ref())
            .build(&mut pdf)
            .expect("build");
        insert_embedded_file(&mut pdf, b"list-test.txt", fs_ref).expect("insert");

        let entries = list_embedded_files(&mut pdf).expect("list");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, b"list-test.txt");
        assert_eq!(entries[0].1, fs_ref);
    }

    #[test]
    fn existing_attachment_survives_second_insertion() {
        let mut pdf = open_minimal();

        // Insert first attachment (uncompressed for variety)
        let raw1 = b"first attachment";
        let fs1 = FileSpecBuilder::new("first.txt", raw1.as_ref())
            .build(&mut pdf)
            .expect("build first");
        insert_embedded_file(&mut pdf, b"first.txt", fs1).expect("insert first");

        // Insert second attachment (compressed)
        let raw2 = b"second attachment with more data";
        let fs2 = FileSpecBuilder::new("second.txt", raw2.as_ref())
            .build(&mut pdf)
            .expect("build second");
        insert_embedded_file(&mut pdf, b"second.txt", fs2).expect("insert second");

        let entries = list_embedded_files(&mut pdf).expect("list");
        assert_eq!(entries.len(), 2, "both attachments must survive");
        let keys: Vec<&[u8]> = entries.iter().map(|(k, _)| k.as_slice()).collect();
        assert!(
            keys.contains(&b"first.txt".as_ref()),
            "first.txt must be present"
        );
        assert!(
            keys.contains(&b"second.txt".as_ref()),
            "second.txt must be present"
        );
    }

    // ── Tests: add_attachment_from_path ───────────────────────────────────────

    #[test]
    fn add_attachment_from_path_round_trip() {
        let mut pdf = open_minimal();

        // Write a temp file to attach.
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("hello.txt");
        let raw = b"Hello from disk!";
        std::fs::write(&file_path, raw).expect("write temp file");

        let fs_ref = add_attachment_from_path(&mut pdf, b"hello.txt", &file_path).expect("attach");

        // Verify retrievable via list_embedded_files
        let entries = list_embedded_files(&mut pdf).expect("list");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, b"hello.txt");
        assert_eq!(entries[0].1, fs_ref);

        // qpdf's addAttachments delegates to createFileSpec/createEFStream;
        // stream compression is selected later by the writer, not this helper.
        let stream = resolve_ef_stream(&mut pdf, fs_ref);
        assert_eq!(
            stream.dict.get("Filter"),
            None,
            "attachment construction must not install a helper-local filter"
        );
        assert_eq!(stream.data, raw);
    }

    #[test]
    fn add_attachment_from_path_checksum_and_size() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("data.bin");
        let raw = b"deterministic checksum test data";
        std::fs::write(&file_path, raw).expect("write");

        let fs_ref = add_attachment_from_path(&mut pdf, b"data.bin", &file_path).expect("attach");

        let stream = resolve_ef_stream(&mut pdf, fs_ref);
        let params = match stream.dict.get("Params") {
            Some(Object::Dictionary(d)) => d.clone(),
            _ => panic!("missing /Params"),
        };
        let size = match params.get("Size") {
            Some(Object::Integer(n)) => *n,
            _ => panic!("missing /Params /Size"),
        };
        let checksum = match params.get("CheckSum") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /Params /CheckSum"),
        };
        assert_eq!(
            size,
            raw.len() as i64,
            "/Params /Size must match raw length"
        );
        assert_eq!(
            checksum,
            vec![
                0xcf, 0x5e, 0x73, 0xd1, 0x4d, 0xf5, 0xca, 0xd1, 0x94, 0xb0, 0x9e, 0xe5, 0x79, 0xf2,
                0x54, 0x9d,
            ],
            "/Params /CheckSum must be the MD5 of raw bytes"
        );
    }

    #[test]
    fn add_attachment_from_path_f_and_uf_follow_qpdf_unicode_string_rules() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("report.pdf");
        std::fs::write(&file_path, b"fake pdf content").expect("write");

        let fs_ref = add_attachment_from_path(&mut pdf, b"report.pdf", &file_path).expect("attach");

        let Some(fs_dict) = pdf.resolve_borrowed(fs_ref).expect("resolve").as_dict() else {
            panic!("expected dict");
        };
        let f = match fs_dict.get("F") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /F"),
        };
        let uf = match fs_dict.get("UF") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /UF"),
        };
        assert_eq!(f, b"report.pdf", "/F must be basename");
        assert_eq!(uf, b"report.pdf", "/UF must use qpdf's PDFDocEncoding form");
    }

    #[test]
    fn add_attachment_from_path_errors_on_missing_file() {
        let mut pdf = open_minimal();
        let result =
            add_attachment_from_path(&mut pdf, b"missing.txt", "/this/does/not/exist/missing.txt");
        assert!(result.is_err(), "must error when file does not exist");
        let err = result.unwrap_err();
        assert_eq!(
            err.to_string(),
            "open /this/does/not/exist/missing.txt: No such file or directory"
        );
    }

    #[test]
    fn add_attachment_from_path_accepts_non_ascii_basename() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("レポート.pdf");
        std::fs::write(&file_path, b"payload").expect("write temp file");

        let fs_ref = add_attachment_from_path(&mut pdf, "レポート.pdf".as_bytes(), &file_path)
            .expect("attach non-ASCII basename");

        let Some(fs_dict) = pdf.resolve_borrowed(fs_ref).expect("resolve").as_dict() else {
            panic!("expected dict");
        };
        let f = match fs_dict.get("F") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /F"),
        };
        let uf = match fs_dict.get("UF") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /UF"),
        };

        assert_eq!(f, b"____.pdf", "/F must be ASCII-safe fallback");
        assert_eq!(
            uf,
            encode_utf16be("レポート.pdf"),
            "/UF must preserve the Unicode basename"
        );
    }

    // ── Tests: extract_attachment / write_attachment / extract_attachment_to_path ─

    #[test]
    fn extract_attachment_small_round_trip() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("small.txt");
        let raw = b"Hello, world!";
        std::fs::write(&path, raw).expect("write");
        add_attachment_from_path(&mut pdf, b"small.txt", &path).expect("attach");

        let extracted = extract_attachment(&mut pdf, b"small.txt").expect("extract");
        assert_eq!(
            extracted.as_slice(),
            raw.as_ref(),
            "small file round-trip must match"
        );
    }

    #[test]
    fn extract_attachment_large_round_trip() {
        // 128 KiB of repeating pseudo-random-ish bytes — exercises compressor splits.
        let raw: Vec<u8> = (0u8..=255).cycle().take(128 * 1024).collect();
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("large.bin");
        std::fs::write(&path, &raw).expect("write");
        add_attachment_from_path(&mut pdf, b"large.bin", &path).expect("attach");

        let extracted = extract_attachment(&mut pdf, b"large.bin").expect("extract");
        assert_eq!(extracted, raw, "large file round-trip must match");
    }

    #[test]
    fn extract_attachment_binary_with_nuls_round_trip() {
        // 4096 bytes including NUL bytes, exercises binary safety.
        let raw: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("binary.bin");
        std::fs::write(&path, &raw).expect("write");
        add_attachment_from_path(&mut pdf, b"binary.bin", &path).expect("attach");

        let extracted = extract_attachment(&mut pdf, b"binary.bin").expect("extract");
        assert_eq!(extracted, raw, "binary file round-trip must match");
    }

    #[test]
    fn write_attachment_to_vec_matches_extract() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("vec-test.txt");
        let raw = b"write_attachment test payload";
        std::fs::write(&path, raw).expect("write");
        add_attachment_from_path(&mut pdf, b"vec-test.txt", &path).expect("attach");

        let mut buf = Vec::new();
        write_attachment(&mut pdf, b"vec-test.txt", &mut buf).expect("write_attachment");
        assert_eq!(
            buf.as_slice(),
            raw.as_ref(),
            "write_attachment output must match raw"
        );
    }

    #[test]
    fn extract_attachment_to_path_round_trip() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");

        let src_path = dir.path().join("source.bin");
        let raw: Vec<u8> = (0u8..=255).cycle().take(2048).collect();
        std::fs::write(&src_path, &raw).expect("write source");
        add_attachment_from_path(&mut pdf, b"source.bin", &src_path).expect("attach");

        let out_path = dir.path().join("extracted.bin");
        extract_attachment_to_path(&mut pdf, b"source.bin", &out_path)
            .expect("extract_attachment_to_path");

        let read_back = std::fs::read(&out_path).expect("read back");
        assert_eq!(read_back, raw, "extract_to_path round-trip must match");
    }

    #[test]
    fn extract_attachment_missing_key_is_actionable_error() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("real.txt");
        std::fs::write(&path, b"real content").expect("write");
        add_attachment_from_path(&mut pdf, b"real.txt", &path).expect("attach");

        let err =
            extract_attachment(&mut pdf, b"missing-key").expect_err("must error for absent key");
        let msg = err.to_string();
        assert!(
            msg.contains("missing-key"),
            "error message must contain the missing key name, got: {msg}"
        );
        // Available keys hint must be present
        assert!(
            msg.contains("real.txt"),
            "error message must list available keys, got: {msg}"
        );
    }

    #[test]
    fn extract_attachment_from_compat_fixture() {
        // attachment-two-page.pdf contains an attachment under the key "attachment.txt"
        // with an uncompressed size of 95 bytes (from /Params /Size).
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/tests/fixtures/compat/attachment-two-page.pdf"
        );
        // The compat fixture is committed to the repo, so a missing file is a
        // real regression — fail loudly instead of silently skipping, which
        // could turn this into a false-positive pass (CodeRabbit).
        let file = std::fs::File::open(path)
            .expect("compat fixture missing: tests/fixtures/compat/attachment-two-page.pdf");
        let mut pdf = crate::Pdf::open(std::io::BufReader::new(file)).expect("open compat fixture");

        let entries = crate::embedded_files::list_embedded_files(&mut pdf).expect("list");
        assert!(
            !entries.is_empty(),
            "fixture must have at least one attachment"
        );

        // Use the first available key.
        let key = entries[0].0.clone();
        let extracted = extract_attachment(&mut pdf, &key).expect("extract from compat fixture");
        assert!(!extracted.is_empty(), "extracted bytes must be non-empty");

        // The fixture reports /Params /Size 95 — the extracted bytes must match.
        let mut fs = FileSpec::new(pdf.get_object_handle(entries[0].1), &mut pdf).unwrap();
        let ef = fs
            .embedded_file()
            .expect("embedded_file")
            .expect("must have embedded file");
        let reported_size = ef.size().expect("size").expect("size must be present");
        assert_eq!(
            extracted.len() as i64,
            reported_size,
            "extracted length must equal /Params /Size"
        );
    }
}

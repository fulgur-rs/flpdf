//! qpdf correspondence: QPDFJob.cc attachment enumeration and display formatting.
//! Structured enumeration and formatted display of PDF attachments.
//!
//! `QPDFJob::list_attachments` owns the public inspection route. It delegates
//! the qpdf-compatible byte rendering to [`format_attachment_list_with_sink`],
//! which is retained as the sink boundary for the job's logger.
//!
//! # Listing format
//!
//! Without `verbose` each attachment contributes exactly one line, naming the
//! name-tree key and the object/generation of its embedded file stream:
//!
//! ```text
//! potato.png -> 6,0
//! ```
//!
//! With `verbose` the header line is followed by the preferred name, every
//! recognized name key, and every `/EF` entry with that stream's parameters:
//!
//! ```text
//! potato.png -> 6,0
//!   description: <only when /Desc is non-empty>
//!   preferred name: π.png
//!   all names:
//!     /F -> π.png
//!     /UF -> π.png
//!   all data streams:
//!     /F -> 6,0
//!       creation date: D:20220215153939-05'00'
//!       modification date: D:20220215153939-05'00'
//!       mime type:
//!       checksum: c55e70c0c72d7eaf01230124fe5ff2d9
//! ```
//!
//! Absent values render as an empty string after the label — the label and its
//! single trailing space are still written, as in the `mime type:` line above.
//!
//! The structured [`AttachmentInfo`] type remains for the separate public API
//! visibility decision tracked by `flpdf-xsq1`; the qpdf job listing itself is
//! intentionally the only supported inspection consumer here.

use super::checksum_to_hex;
use crate::filespec_helper::{EmbeddedFileStream, FileSpec};
use crate::object_handle::ObjectHandle;
use crate::{ObjectRef, Pdf, Result};
use std::io::{Read, Seek};

// ── AttachmentInfo ────────────────────────────────────────────────────────────

/// Structured metadata for a single PDF attachment.
///
/// Fields are `Option<Vec<u8>>` (or `Option<i64>` for `size`) because any
/// field may be absent in a well-formed or partially-formed PDF.
///
/// `key` and `filespec_ref` are always present — they come from the
/// `/Names /EmbeddedFiles` name tree and are required to have found the entry
/// in the first place.
#[derive(Debug, Clone, PartialEq)]
pub struct AttachmentInfo {
    /// Raw name-tree key (the bytes used to look up this attachment).
    pub key: Vec<u8>,
    /// Object reference of the `/Filespec` dictionary.
    pub filespec_ref: ObjectRef,
    /// Display name: decoded `/UF` (preferred) or decoded `/F`.  `None` when
    /// both are absent.
    pub display_name: Option<String>,
    /// Uncompressed file size from `/Params /Size`.
    pub size: Option<i64>,
    /// MIME type from `/EmbeddedFile /Subtype` (raw bytes from PDF Name).
    pub mimetype: Option<Vec<u8>>,
    /// Raw PDF date string from `/Params /CreationDate`.
    pub creation_date: Option<Vec<u8>>,
    /// Raw PDF date string from `/Params /ModDate`.
    pub modification_date: Option<Vec<u8>>,
    // ── verbose-only fields ───────────────────────────────────────────────
    /// Human-readable description from `/Filespec /Desc`.
    pub description: Option<Vec<u8>>,
    /// Associated-file relationship from `/Filespec /AFRelationship`.
    pub af_relationship: Option<Vec<u8>>,
    /// MD5 checksum from `/Params /CheckSum` (raw bytes; displayed as hex).
    pub checksum: Option<Vec<u8>>,
}

/// Format the attachment list while forwarding each emitted fragment to a
/// caller-owned sink.
///
/// qpdf writes attachment output directly to its info pipeline.  Keeping the
/// sink boundary here lets the CLI preserve output already written when a
/// later verbose metadata accessor throws (for example, the `creation date:`
/// prefix before `getDict()` rejects a non-stream `/EF` value).
pub fn format_attachment_list_with_sink<R, F>(
    pdf: &mut Pdf<R>,
    verbose: bool,
    mut sink: F,
) -> Result<Option<Vec<u8>>>
where
    R: Read + Seek,
    F: FnMut(&[u8]) -> Result<()>,
{
    let mut helper = pdf.embedded_files();
    if !helper.has_embedded_files()? {
        return Ok(None);
    }
    // The name-tree walker already hands out qpdf's UTF-8 view of each key, so
    // the map is keyed and ordered by the converted bytes, as qpdf's is.
    let entries = helper.get_embedded_files()?;

    let mut out = ListingOutput::new(&mut sink);
    for (key, filespec) in entries {
        let ef_entries = {
            let mut file_spec = FileSpec::new(filespec, pdf)?;
            let stream = file_spec.get_embedded_file_stream("")?;
            out.extend_from_slice(&key)?;
            out.extend_from_slice(b" -> ")?;
            out.extend_from_slice(object_generation(&stream).as_bytes())?;
            out.push(b'\n')?;

            if !verbose {
                continue;
            }

            let description = file_spec.get_description()?;
            if !description.is_empty() {
                out.push_labelled(b"  description: ", &description)?;
            }
            let filename = file_spec.get_filename()?;
            out.push_labelled(b"  preferred name: ", &filename)?;
            out.extend_from_slice(b"  all names:\n")?;
            for (name_key, name) in file_spec.get_filenames()? {
                out.extend_from_slice(b"    ")?;
                out.extend_from_slice(name_key.as_bytes())?;
                out.extend_from_slice(b" -> ")?;
                out.extend_from_slice(&name)?;
                out.push(b'\n')?;
            }
            out.extend_from_slice(b"  all data streams:\n")?;
            file_spec.get_embedded_file_stream_entries()?
        };

        // qpdf walks the raw /EF dictionary, so an entry under a key outside
        // the recognized name keys is listed too; keys whose value is null are
        // skipped, as they are by QPDFObjectHandle::getKeys.
        for (stream_key, stream) in ef_entries {
            pdf.resolve(&stream)?;
            if stream.is_null() {
                continue;
            }
            out.extend_from_slice(b"    ")?;
            out.extend_from_slice(&stream_key)?;
            out.extend_from_slice(b" -> ")?;
            out.extend_from_slice(object_generation(&stream).as_bytes())?;
            out.push(b'\n')?;

            let embedded_file = EmbeddedFileStream::new(stream, pdf)?;
            out.extend_from_slice(b"      creation date: ")?;
            out.extend_from_slice(&embedded_file.get_creation_date()?)?;
            out.push(b'\n')?;
            out.extend_from_slice(b"      modification date: ")?;
            out.extend_from_slice(&embedded_file.get_mod_date()?)?;
            out.push(b'\n')?;
            out.extend_from_slice(b"      mime type: ")?;
            out.extend_from_slice(&embedded_file.get_subtype()?)?;
            out.push(b'\n')?;
            out.extend_from_slice(b"      checksum: ")?;
            out.extend_from_slice(checksum_to_hex(&embedded_file.get_checksum()?).as_bytes())?;
            out.push(b'\n')?;
        }
    }
    Ok(Some(out.finish()))
}

struct ListingOutput<'a, F> {
    bytes: Vec<u8>,
    sink: &'a mut F,
}

impl<'a, F> ListingOutput<'a, F>
where
    F: FnMut(&[u8]) -> Result<()>,
{
    fn new(sink: &'a mut F) -> Self {
        Self {
            bytes: Vec::new(),
            sink,
        }
    }

    fn extend_from_slice(&mut self, data: &[u8]) -> Result<()> {
        self.bytes.extend_from_slice(data);
        (self.sink)(data)
    }

    fn push(&mut self, byte: u8) -> Result<()> {
        self.bytes.push(byte);
        (self.sink)(&[byte])
    }

    /// Append `label`, `value`, and a newline in qpdf's expression order.
    fn push_labelled(&mut self, label: &[u8], value: &[u8]) -> Result<()> {
        self.extend_from_slice(label)?;
        self.extend_from_slice(value)?;
        self.push(b'\n')
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// Render a handle's object and generation the way `QPDFObjGen::unparse(',')`
/// does, reporting `0,0` for a direct object.
fn object_generation(handle: &ObjectHandle) -> String {
    match handle.object_ref() {
        Some(object_ref) => format!("{},{}", object_ref.number, object_ref.generation),
        None => "0,0".to_owned(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedded_files::insert_embedded_file;
    use crate::filespec_helper::{encode_utf16be, FileParamDates, FileSpecBuilder};
    use crate::job::QPDFJob;
    use crate::pipeline::{Pipeline, PipelineError, PipelineHandle, PipelineResult};
    use crate::{Error, ObjectHandle, ObjectRef, Pdf, QPDFLogger};
    use std::io::{Cursor, Read, Seek};
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    struct InfoCapture {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl Pipeline for InfoCapture {
        fn identifier(&self) -> &str {
            "attachment-list test capture"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.bytes
                .lock()
                .map_err(|_| PipelineError::runtime("attachment-list capture mutex poisoned"))?
                .extend_from_slice(data);
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
    }

    #[test]
    fn info_capture_exposes_the_pipeline_lifecycle() {
        let mut capture = InfoCapture {
            bytes: Arc::new(Mutex::new(Vec::new())),
        };
        assert_eq!(capture.identifier(), "attachment-list test capture");
        capture.write(b"lifecycle").expect("capture write");
        capture.finish().expect("capture finish");
    }

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

    fn inline_non_dictionary_filespec_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles << /Names [(k.txt) (not-a-filespec)] >> >> >>\nendobj\n",
        );
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

    // ── Fixture PDF with actual attachment ────────────────────────────────────

    #[test]
    fn fixture_attachment_two_page_returns_one_or_more() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/attachment-two-page.pdf"
        );
        let f = std::fs::File::open(path);
        if f.is_err() {
            // Fixture absent — skip gracefully.
            return;
        }
        let mut pdf = Pdf::open(std::io::BufReader::new(f.unwrap())).expect("open fixture");
        let listed = as_text(&listing(&mut pdf, false));
        assert!(
            listed.contains("attachment.txt -> "),
            "fixture must list at least one attachment: {listed:?}"
        );
    }

    // ── Empty document → empty list ───────────────────────────────────────────

    #[test]
    fn empty_document_returns_empty_list() {
        let mut pdf = open_minimal();
        assert_eq!(
            listing(&mut pdf, false),
            b"test.pdf has no embedded files\n",
            "qpdf's canonical job route owns the no-attachments message"
        );
    }

    // ── Filespec construction helpers ─────────────────────────────────────────

    fn next_ref(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> ObjectRef {
        pdf.next_available_object_ref()
            .expect("object-number space must have room in the test fixture")
    }

    fn object_ref(pdf: &mut Pdf<Cursor<Vec<u8>>>, object_ref: ObjectRef) -> ObjectHandle {
        pdf.get_object_handle(object_ref)
    }

    #[derive(Clone)]
    struct HandleDict(ObjectHandle);

    impl HandleDict {
        fn new() -> Self {
            Self(ObjectHandle::dictionary(Vec::new()))
        }

        fn from_entries(entries: Vec<(Vec<u8>, ObjectHandle)>) -> Self {
            Self(ObjectHandle::dictionary(entries))
        }

        fn insert(&self, key: &str, value: ObjectHandle) {
            let mut key_bytes = Vec::with_capacity(key.len() + 1);
            key_bytes.push(b'/');
            key_bytes.extend_from_slice(key.as_bytes());
            self.0
                .replace_key(&key_bytes, value)
                .expect("test fixture dictionary insertion");
        }

        fn into_handle(self) -> ObjectHandle {
            self.0
        }
    }

    /// Store an `/EmbeddedFile` stream and return its reference.
    ///
    /// `params` populates `/Params`; `subtype` populates `/Subtype`.  Both are
    /// omitted entirely when `None`, which is what an attachment written by a
    /// producer that records no metadata looks like.
    fn add_ef_stream(
        pdf: &mut Pdf<Cursor<Vec<u8>>>,
        params: Option<HandleDict>,
        subtype: Option<&[u8]>,
    ) -> ObjectRef {
        let stream_ref = next_ref(pdf);
        let dict = HandleDict::new();
        dict.insert("Type", ObjectHandle::name(b"EmbeddedFile".to_vec()));
        dict.insert("Length", ObjectHandle::integer(4));
        if let Some(subtype) = subtype {
            dict.insert("Subtype", ObjectHandle::name(subtype.to_vec()));
        }
        if let Some(params) = params {
            dict.insert("Params", params.into_handle());
        }
        pdf.replace_object(
            stream_ref,
            ObjectHandle::stream(dict.into_handle(), Rc::new(b"data".to_vec())),
        )
        .expect("install embedded-file stream fixture");
        stream_ref
    }

    /// Store `filespec` and register it in the EmbeddedFiles name tree.
    fn attach(pdf: &mut Pdf<Cursor<Vec<u8>>>, key: &[u8], filespec: HandleDict) -> ObjectRef {
        let filespec_ref = next_ref(pdf);
        pdf.replace_object(filespec_ref, filespec.into_handle())
            .expect("install Filespec fixture");
        insert_embedded_file(pdf, key, filespec_ref).expect("insert");
        filespec_ref
    }

    /// Build `/Params` with the standard four entries.
    fn full_params() -> HandleDict {
        let params = HandleDict::new();
        params.insert("Size", ObjectHandle::integer(4));
        params.insert(
            "CreationDate",
            ObjectHandle::string(b"D:20240101000000Z".to_vec()),
        );
        params.insert(
            "ModDate",
            ObjectHandle::string(b"D:20240102000000Z".to_vec()),
        );
        params.insert(
            "CheckSum",
            ObjectHandle::string(vec![0x00, 0x1f, 0xa0, 0xff]),
        );
        params
    }

    fn run_listing<R: Read + Seek>(pdf: &mut Pdf<R>, verbose: bool) -> Result<Vec<u8>> {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let logger = QPDFLogger::create();
        logger.set_info(Some(PipelineHandle::new(InfoCapture {
            bytes: Arc::clone(&bytes),
        })));
        logger.set_warn(Some(logger.discard()));
        logger.set_error(Some(logger.discard()));
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.set_input_name("test.pdf");
        job.list_attachments(pdf, verbose)?;
        let captured = bytes
            .lock()
            .map_err(|_| Error::Internal("attachment-list capture mutex poisoned".to_owned()))?
            .clone();
        Ok(captured)
    }

    fn listing<R: Read + Seek>(pdf: &mut Pdf<R>, verbose: bool) -> Vec<u8> {
        run_listing(pdf, verbose).expect("list attachments")
    }

    fn as_text(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).expect("listing must be valid UTF-8 in these fixtures")
    }

    // ── Non-verbose output is one line per attachment ─────────────────────────

    #[test]
    fn non_verbose_lists_only_the_header_line() {
        let mut pdf = open_minimal();
        let stream_ref = add_ef_stream(&mut pdf, Some(full_params()), Some(b"text/plain"));
        let ef = HandleDict::new();
        ef.insert("F", object_ref(&mut pdf, stream_ref));
        let filespec = HandleDict::new();
        filespec.insert("Type", ObjectHandle::name(b"Filespec".to_vec()));
        filespec.insert("F", ObjectHandle::string(b"a.txt".to_vec()));
        filespec.insert("Desc", ObjectHandle::string(b"described".to_vec()));
        filespec.insert("EF", HandleDict::into_handle(ef));
        attach(&mut pdf, b"a.txt", filespec);

        let out = listing(&mut pdf, false);
        assert_eq!(
            as_text(&out),
            format!("a.txt -> {},0\n", stream_ref.number),
            "qpdf writes the header line outside its verbose block, so plain \
             mode emits nothing else"
        );
    }

    // ── Header object/generation comes from the embedded file stream ──────────

    #[test]
    fn header_objgen_is_the_stream_not_the_filespec() {
        let mut pdf = open_minimal();
        let stream_ref = add_ef_stream(&mut pdf, None, None);
        let ef = HandleDict::new();
        ef.insert("F", object_ref(&mut pdf, stream_ref));
        let filespec = HandleDict::new();
        filespec.insert("F", ObjectHandle::string(b"a.txt".to_vec()));
        filespec.insert("EF", HandleDict::into_handle(ef));
        let filespec_ref = attach(&mut pdf, b"a.txt", filespec);

        assert_ne!(
            stream_ref.number, filespec_ref.number,
            "fixture must distinguish the two objects"
        );
        let out = as_text(&listing(&mut pdf, false));
        assert_eq!(out, format!("a.txt -> {},0\n", stream_ref.number));
        assert!(
            !out.contains(&format!("{},0", filespec_ref.number)),
            "the /Filespec object number must not appear: {out:?}"
        );
    }

    // ── Verbose block structure ───────────────────────────────────────────────

    #[test]
    fn verbose_block_matches_qpdf_structure() {
        let mut pdf = open_minimal();
        let stream_ref = add_ef_stream(&mut pdf, Some(full_params()), Some(b"text/plain"));
        let ef = HandleDict::new();
        ef.insert("F", object_ref(&mut pdf, stream_ref));
        ef.insert("UF", object_ref(&mut pdf, stream_ref));
        let filespec = HandleDict::new();
        filespec.insert("Type", ObjectHandle::name(b"Filespec".to_vec()));
        filespec.insert("F", ObjectHandle::string(b"a.txt".to_vec()));
        filespec.insert("UF", ObjectHandle::string(encode_utf16be("π.txt")));
        filespec.insert("EF", HandleDict::into_handle(ef));
        attach(&mut pdf, b"a.txt", filespec);

        let n = stream_ref.number;
        let expected = format!(
            "a.txt -> {n},0\n\
             \x20 preferred name: π.txt\n\
             \x20 all names:\n\
             \x20   /F -> a.txt\n\
             \x20   /UF -> π.txt\n\
             \x20 all data streams:\n\
             \x20   /F -> {n},0\n\
             \x20     creation date: D:20240101000000Z\n\
             \x20     modification date: D:20240102000000Z\n\
             \x20     mime type: text/plain\n\
             \x20     checksum: 001fa0ff\n\
             \x20   /UF -> {n},0\n\
             \x20     creation date: D:20240101000000Z\n\
             \x20     modification date: D:20240102000000Z\n\
             \x20     mime type: text/plain\n\
             \x20     checksum: 001fa0ff\n"
        );
        assert_eq!(as_text(&listing(&mut pdf, true)), expected);
    }

    // ── /Desc placement ───────────────────────────────────────────────────────

    #[test]
    fn description_precedes_preferred_name_and_only_when_present() {
        let mut pdf = open_minimal();
        let stream_ref = add_ef_stream(&mut pdf, None, None);
        let ef = HandleDict::new();
        ef.insert("F", object_ref(&mut pdf, stream_ref));
        let filespec = HandleDict::new();
        filespec.insert("F", ObjectHandle::string(b"a.txt".to_vec()));
        filespec.insert("Desc", ObjectHandle::string(b"my description".to_vec()));
        filespec.insert("EF", HandleDict::into_handle(ef));
        attach(&mut pdf, b"a.txt", filespec);

        let with_desc = as_text(&listing(&mut pdf, true));
        let lines: Vec<&str> = with_desc.lines().collect();
        assert_eq!(lines[1], "  description: my description");
        assert_eq!(lines[2], "  preferred name: a.txt");

        // An empty /Desc drops the line entirely rather than printing a label.
        let mut pdf = open_minimal();
        let stream_ref = add_ef_stream(&mut pdf, None, None);
        let ef = HandleDict::new();
        ef.insert("F", object_ref(&mut pdf, stream_ref));
        let filespec = HandleDict::new();
        filespec.insert("F", ObjectHandle::string(b"a.txt".to_vec()));
        filespec.insert("Desc", ObjectHandle::string(Vec::new()));
        filespec.insert("EF", HandleDict::into_handle(ef));
        attach(&mut pdf, b"a.txt", filespec);

        let without_desc = as_text(&listing(&mut pdf, true));
        assert!(
            !without_desc.contains("description:"),
            "an empty /Desc must not produce a line: {without_desc:?}"
        );
        assert_eq!(without_desc.lines().nth(1), Some("  preferred name: a.txt"));
    }

    // ── Absent metadata leaves the value empty ────────────────────────────────

    #[test]
    fn absent_values_render_empty_after_the_label() {
        let mut pdf = open_minimal();
        let stream_ref = add_ef_stream(&mut pdf, None, None);
        let ef = HandleDict::new();
        ef.insert("F", object_ref(&mut pdf, stream_ref));
        let filespec = HandleDict::new();
        filespec.insert("F", ObjectHandle::string(b"a.txt".to_vec()));
        filespec.insert("EF", HandleDict::into_handle(ef));
        attach(&mut pdf, b"a.txt", filespec);

        let out = as_text(&listing(&mut pdf, true));
        assert!(out.contains("      creation date: \n"), "{out:?}");
        assert!(out.contains("      modification date: \n"), "{out:?}");
        assert!(out.contains("      mime type: \n"), "{out:?}");
        assert!(out.contains("      checksum: \n"), "{out:?}");
        assert!(
            !out.contains("(none)"),
            "qpdf never substitutes a placeholder: {out:?}"
        );
        assert!(
            !out.contains("size:") && !out.contains("af relationship:"),
            "qpdf's listing has no size or AFRelationship line: {out:?}"
        );
    }

    // ── /EF keys outside the recognized name keys ─────────────────────────────

    #[test]
    fn data_streams_list_every_ef_key_but_names_stay_recognized() {
        let mut pdf = open_minimal();
        let stream_ref = add_ef_stream(&mut pdf, None, None);
        let ef = HandleDict::new();
        ef.insert("F", object_ref(&mut pdf, stream_ref));
        ef.insert("Zed", object_ref(&mut pdf, stream_ref));
        let filespec = HandleDict::new();
        filespec.insert("F", ObjectHandle::string(b"a.txt".to_vec()));
        filespec.insert("Zed", ObjectHandle::string(b"ignored.txt".to_vec()));
        filespec.insert("EF", HandleDict::into_handle(ef));
        attach(&mut pdf, b"a.txt", filespec);

        let out = as_text(&listing(&mut pdf, true));
        assert!(
            out.contains(&format!("    /Zed -> {},0\n", stream_ref.number)),
            "an /EF key outside the name keys is still a data stream: {out:?}"
        );
        assert!(
            !out.contains("/Zed -> ignored.txt"),
            "`all names` only covers the recognized name keys: {out:?}"
        );
    }

    // ── Null /EF entries are skipped ──────────────────────────────────────────

    #[test]
    fn null_ef_entries_are_skipped() {
        let mut pdf = open_minimal();
        let stream_ref = add_ef_stream(&mut pdf, None, None);
        let missing = ObjectRef::new(stream_ref.number + 40, 0);
        let ef = HandleDict::from_entries(vec![(b"/UF".to_vec(), ObjectHandle::null())]);
        ef.insert("F", object_ref(&mut pdf, stream_ref));
        ef.insert("Unix", object_ref(&mut pdf, missing));
        let filespec = HandleDict::new();
        filespec.insert("F", ObjectHandle::string(b"a.txt".to_vec()));
        filespec.insert("EF", HandleDict::into_handle(ef));
        attach(&mut pdf, b"a.txt", filespec);

        let out = as_text(&listing(&mut pdf, true));
        assert!(out.contains("    /F -> "), "{out:?}");
        assert!(
            !out.contains("    /UF -> "),
            "a direct null /EF value is not a key: {out:?}"
        );
        assert!(
            !out.contains("    /Unix -> "),
            "a reference to a missing object resolves to null: {out:?}"
        );
    }

    // ── A filespec with no usable stream ──────────────────────────────────────

    #[test]
    fn filespec_without_ef_reports_zero_objgen() {
        let mut pdf = open_minimal();
        let filespec = HandleDict::new();
        filespec.insert("F", ObjectHandle::string(b"c.txt".to_vec()));
        attach(&mut pdf, b"c.txt", filespec);

        assert_eq!(
            as_text(&listing(&mut pdf, true)),
            "c.txt -> 0,0\n  preferred name: c.txt\n  all names:\n    /F -> c.txt\n  \
             all data streams:\n",
            "a missing /EF still prints the header and both section labels"
        );

        let diagnostics = pdf.repair_diagnostics();
        let warnings: Vec<_> = diagnostics
            .entries()
            .iter()
            .filter(|diagnostic| {
                diagnostic
                    .message
                    .contains("operation for dictionary attempted on object of type null")
            })
            .collect();
        assert_eq!(
            warnings.len(),
            2,
            "qpdf ditems() asks the missing /EF null for keys at begin and end"
        );
        assert!(warnings
            .iter()
            .all(|diagnostic| diagnostic.message.contains("treating as empty")));
    }

    #[test]
    fn ef_value_that_is_not_a_stream_fails_when_metadata_is_read() {
        let mut pdf = open_minimal();
        let dict_ref = next_ref(&mut pdf);
        let not_a_stream = HandleDict::new();
        not_a_stream.insert("Type", ObjectHandle::name(b"EmbeddedFile".to_vec()));
        pdf.replace_object(dict_ref, HandleDict::into_handle(not_a_stream))
            .expect("install non-stream fixture");
        let ef = HandleDict::new();
        ef.insert("F", object_ref(&mut pdf, dict_ref));
        let filespec = HandleDict::new();
        filespec.insert("F", ObjectHandle::string(b"g.txt".to_vec()));
        filespec.insert("EF", HandleDict::into_handle(ef));
        attach(&mut pdf, b"g.txt", filespec);

        let error = run_listing(&mut pdf, true)
            .expect_err("qpdf reads stream metadata after listing a raw non-stream /EF value");
        assert_eq!(
            error.to_string(),
            "operation for stream attempted on object of type dictionary"
        );
        assert_eq!(dict_ref.generation, 0);
    }

    // ── Name tree presence decides the "no embedded files" branch ─────────────

    #[test]
    fn document_without_name_tree_returns_none() {
        let mut pdf = open_minimal();
        assert_eq!(
            listing(&mut pdf, true),
            b"test.pdf has no embedded files\n",
            "the canonical job route owns the no-embedded-files branch"
        );
    }

    #[test]
    fn empty_name_tree_lists_nothing_but_is_not_none() {
        let mut pdf = open_minimal();
        let root = pdf.root_ref().expect("catalog");
        let catalog = pdf.get_object_handle(root);
        pdf.resolve(&catalog).expect("resolve catalog");
        let tree = HandleDict::new();
        tree.insert("Names", ObjectHandle::array(Vec::new()));
        let names = HandleDict::new();
        names.insert("EmbeddedFiles", tree.into_handle());
        catalog
            .replace_key(b"/Names", names.into_handle())
            .expect("install empty EmbeddedFiles tree");
        pdf.mark_object_handle_dirty(&catalog)
            .expect("mark catalog dirty");

        assert_eq!(
            listing(&mut pdf, true),
            Vec::<u8>::new(),
            "an empty tree lists nothing, but the document does have the tree"
        );
    }

    /// Point `/Names /EmbeddedFiles` straight at `[key value]`, bypassing the
    /// name-tree writer so the value shape can be chosen freely.
    fn attach_raw_tree_value(pdf: &mut Pdf<Cursor<Vec<u8>>>, key: &[u8], value: ObjectHandle) {
        let root = pdf.root_ref().expect("catalog");
        let catalog = pdf.get_object_handle(root);
        pdf.resolve(&catalog).expect("resolve catalog");
        let tree = HandleDict::new();
        tree.insert(
            "Names",
            ObjectHandle::array(vec![ObjectHandle::string(key.to_vec()), value]),
        );
        let names = HandleDict::new();
        names.insert("EmbeddedFiles", tree.into_handle());
        catalog
            .replace_key(b"/Names", names.into_handle())
            .expect("install raw EmbeddedFiles tree fixture");
        pdf.mark_object_handle_dirty(&catalog)
            .expect("mark catalog dirty");
    }

    // ── Name-tree value shapes ────────────────────────────────────────────────

    #[test]
    fn inline_filespec_value_is_listed() {
        let mut pdf = open_minimal();
        let stream_ref = add_ef_stream(&mut pdf, None, None);
        let ef = HandleDict::new();
        ef.insert("F", object_ref(&mut pdf, stream_ref));
        let filespec = HandleDict::new();
        filespec.insert("Type", ObjectHandle::name(b"Filespec".to_vec()));
        filespec.insert("F", ObjectHandle::string(b"j.txt".to_vec()));
        filespec.insert("EF", HandleDict::into_handle(ef));
        // A name-tree leaf may hold the /Filespec inline instead of by
        // reference; it must list exactly as an indirect one does.
        attach_raw_tree_value(&mut pdf, b"j.txt", HandleDict::into_handle(filespec));

        let n = stream_ref.number;
        assert_eq!(
            as_text(&listing(&mut pdf, true)),
            format!(
                "j.txt -> {n},0\n\
                 \x20 preferred name: j.txt\n\
                 \x20 all names:\n\
                 \x20   /F -> j.txt\n\
                 \x20 all data streams:\n\
                 \x20   /F -> {n},0\n\
                 \x20     creation date: \n\
                 \x20     modification date: \n\
                 \x20     mime type: \n\
                 \x20     checksum: \n"
            )
        );
    }

    #[test]
    fn non_dictionary_filespec_value_still_lists_the_key() {
        // qpdf warns ("Embedded file object is not a dictionary") and carries
        // on with empty values rather than failing the listing.
        let mut direct_pdf = Pdf::open(Cursor::new(inline_non_dictionary_filespec_pdf_bytes()))
            .expect("open direct non-dictionary Filespec fixture");
        assert_eq!(
            as_text(&listing(&mut direct_pdf, true)),
            "k.txt -> 0,0\n  preferred name: \n  all names:\n  all data streams:\n",
        );
        assert!(
            direct_pdf
                .repair_diagnostics()
                .entries()
                .iter()
                .any(|diagnostic| diagnostic
                    .message
                    .contains("Embedded file object is not a dictionary")),
            "QPDFFileSpecObjectHelper must warn for a direct non-dictionary Filespec"
        );

        let mut dangling_pdf = open_minimal();
        let dangling = object_ref(&mut dangling_pdf, ObjectRef::new(4096, 0));
        attach_raw_tree_value(&mut dangling_pdf, b"k.txt", dangling);
        assert_eq!(
            as_text(&listing(&mut dangling_pdf, true)),
            "k.txt -> 0,0\n  preferred name: \n  all names:\n  all data streams:\n",
        );
        assert!(
            dangling_pdf
                .repair_diagnostics()
                .entries()
                .iter()
                .any(|diagnostic| diagnostic
                    .message
                    .contains("Embedded file object is not a dictionary")),
            "QPDFFileSpecObjectHelper must warn for a dangling Filespec"
        );
    }

    // ── Name-tree keys use qpdf's UTF-8 view ──────────────────────────────────

    #[test]
    fn header_key_uses_the_qpdf_utf8_view() {
        let mut pdf = open_minimal();
        // Name-tree keys are qpdf's UTF-8 view on the way in; the tree stores
        // them as UTF-16BE PDF strings, so a listing that echoed the stored
        // bytes would be mojibake.
        for (key, filename) in [
            ("π.txt".as_bytes().to_vec(), b"pi.txt".as_slice()),
            ("café.txt".as_bytes().to_vec(), b"cafe.txt".as_slice()),
        ] {
            let stream_ref = add_ef_stream(&mut pdf, None, None);
            let ef = HandleDict::new();
            ef.insert("F", object_ref(&mut pdf, stream_ref));
            let filespec = HandleDict::new();
            filespec.insert("F", ObjectHandle::string(filename.to_vec()));
            filespec.insert("EF", HandleDict::into_handle(ef));
            attach(&mut pdf, &key, filespec);
        }

        let out = as_text(&listing(&mut pdf, false));
        let keys: Vec<&str> = out
            .lines()
            .map(|line| line.split(" -> ").next().expect("header key"))
            .collect();
        assert_eq!(
            keys,
            vec!["café.txt", "π.txt"],
            "keys decode through qpdf's UTF-8 view and order by the converted \
             bytes: {out:?}"
        );
    }

    // ── /UF absent → /F used as display name ─────────────────────────────────

    #[test]
    fn f_only_filespec_uses_f_as_display_name() {
        let mut pdf = open_minimal();

        // Minimal EmbeddedFile stream (no /Params → missing size/dates/checksum).
        let stream_ref = add_ef_stream(&mut pdf, None, None);
        let ef_sub = HandleDict::new();
        ef_sub.insert("F", object_ref(&mut pdf, stream_ref));

        // Filespec with /F only (no /UF).
        let fs_dict = HandleDict::new();
        fs_dict.insert("Type", ObjectHandle::name(b"Filespec".to_vec()));
        fs_dict.insert("F", ObjectHandle::string(b"only-f.txt".to_vec()));
        fs_dict.insert("EF", HandleDict::into_handle(ef_sub));
        attach(&mut pdf, b"only-f.txt", fs_dict);

        let listed = as_text(&listing(&mut pdf, true));
        assert!(
            listed.contains("  preferred name: only-f.txt\n"),
            "the listing falls back to /F for the preferred name: {listed:?}"
        );
    }

    // ── /UF (UTF-16BE) is decoded correctly ──────────────────────────────────

    #[test]
    fn uf_utf16be_is_decoded_for_display() {
        let mut pdf = open_minimal();

        // FileSpecBuilder writes /UF as UTF-16BE.
        let fs_ref = FileSpecBuilder::new("hello.txt", b"hi")
            .build(&mut pdf)
            .expect("build");
        insert_embedded_file(&mut pdf, b"hello.txt", fs_ref).expect("insert");

        let listed = as_text(&listing(&mut pdf, true));
        assert!(
            listed.contains("  preferred name: hello.txt\n"),
            "/UF must decode to the preferred display name: {listed:?}"
        );
    }

    // ── checksum is hex-encoded ───────────────────────────────────────────────

    #[test]
    fn checksum_displayed_as_lowercase_hex() {
        let mut pdf = open_minimal();

        let payload = b"checksum test";
        let fs_ref = FileSpecBuilder::new("chk.txt", payload.as_ref())
            .build(&mut pdf)
            .expect("build");
        insert_embedded_file(&mut pdf, b"chk.txt", fs_ref).expect("insert");

        let verbose = as_text(&listing(&mut pdf, true));
        // The checksum line must contain lowercase hex, not raw bytes.
        // The actual MD5 of b"checksum test" is deterministic; verify format.
        let chk_line = verbose
            .lines()
            .find(|l| l.trim_start().starts_with("checksum:"))
            .expect("checksum line must be present");
        let hex_part = chk_line.split(':').nth(1).unwrap_or("").trim();
        // Must be all hex digits (32 chars for MD5).
        assert!(
            hex_part.len() == 32 && hex_part.chars().all(|c| c.is_ascii_hexdigit()),
            "checksum must be 32-char lowercase hex: {hex_part:?}"
        );
        assert!(
            hex_part == hex_part.to_lowercase(),
            "checksum must be lowercase: {hex_part:?}"
        );
    }

    // ── dates and full metadata with FileSpecBuilder ──────────────────────────

    #[test]
    fn full_metadata_attachment() {
        let mut pdf = open_minimal();

        let dates = FileParamDates {
            creation: Some((2026, 1, 1, 0, 0, 0)),
            modification: Some((2026, 6, 15, 12, 30, 0)),
        };
        let fs_ref = FileSpecBuilder::new("full.txt", b"full payload")
            .mimetype(b"text/plain")
            .description(b"Full test attachment")
            .af_relationship(b"Data")
            .dates(dates)
            .build(&mut pdf)
            .expect("build");
        insert_embedded_file(&mut pdf, b"full.txt", fs_ref).expect("insert");

        // The listing renders the same document through qpdf's layout.
        let formatted = as_text(&listing(&mut pdf, true));
        assert!(
            formatted.contains("      mime type: text/plain\n"),
            "{formatted:?}"
        );
        assert!(
            formatted.contains("      creation date: D:20260101000000Z\n"),
            "{formatted:?}"
        );
        assert!(
            formatted.contains("      modification date: D:20260615123000Z\n"),
            "{formatted:?}"
        );
        assert!(
            formatted.contains("  description: Full test attachment\n"),
            "{formatted:?}"
        );
        assert!(
            !formatted.contains("af relationship"),
            "qpdf's listing has no /AFRelationship line: {formatted:?}"
        );
    }

    // The /Desc verbose output must decode PDF text
    // strings (here UTF-16BE) instead of showing mojibake.
    #[test]
    fn verbose_description_decodes_utf16be() {
        let mut pdf = open_minimal();
        let stream_ref = add_ef_stream(&mut pdf, None, None);
        let ef = HandleDict::new();
        ef.insert("F", object_ref(&mut pdf, stream_ref));
        let filespec = HandleDict::new();
        filespec.insert("F", ObjectHandle::string(b"k.txt".to_vec()));
        filespec.insert("Desc", ObjectHandle::string(encode_utf16be("dé")));
        filespec.insert("EF", HandleDict::into_handle(ef));
        attach(&mut pdf, b"k.txt", filespec);

        let formatted = as_text(&listing(&mut pdf, true));
        assert!(
            formatted.contains("  description: dé\n"),
            "UTF-16BE /Desc must be decoded: {formatted:?}"
        );
    }
}

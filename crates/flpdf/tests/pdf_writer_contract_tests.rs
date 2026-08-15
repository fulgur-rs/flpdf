use std::cell::RefCell;
use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::process::Command;
use std::rc::Rc;

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use flpdf::pipeline::{Pipeline, PipelineResult};
use flpdf::{
    apply_stream_compress_policy, pages, CompressStreams, CopyEncryptionSource, DecodeLevel,
    Dictionary, EncryptParams, Object, ObjectKeyAlg, ObjectRef, ObjectStreamMode, Pdf,
    PdfOpenOptions, PdfWriter, Stream, StreamDataMode, XrefEntry,
};

mod common;
use common::{write_with_settings, WriterTestSettings};

#[derive(Clone)]
struct SharedBytes(Rc<RefCell<Vec<u8>>>);

impl Write for SharedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct RecordingPipeline {
    bytes: Rc<RefCell<Vec<u8>>>,
    writes: Rc<RefCell<usize>>,
    finishes: Rc<RefCell<usize>>,
}

impl Pipeline for RecordingPipeline {
    fn identifier(&self) -> &str {
        "qpdf-writer-contract"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        *self.writes.borrow_mut() += 1;
        self.bytes.borrow_mut().extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        *self.finishes.borrow_mut() += 1;
        Ok(())
    }
}

fn open_minimal_pdf() -> flpdf::Result<Pdf<BufReader<File>>> {
    let file = File::open("../../tests/fixtures/minimal.pdf")?;
    Pdf::open(BufReader::new(file))
}

fn qpdf_11_9_0() -> flpdf::Result<()> {
    let output = Command::new("qpdf").arg("--version").output()?;
    assert!(output.status.success(), "qpdf --version failed: {output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    let version_line = text
        .lines()
        .find(|line| line.starts_with("qpdf version "))
        .expect("qpdf --version must report a standard version line");
    let reported_version = version_line
        .split_whitespace()
        .nth(2)
        .expect("qpdf version line must contain a version token");
    assert_eq!(
        reported_version, "11.9.0",
        "unexpected qpdf version output: {text}"
    );
    Ok(())
}

fn synthetic_unreferenced_object_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>".as_slice(),
        b"<< /Type /Pages /Count 0 /Kids [] >>".as_slice(),
        b"<< /Marker (unreferenced-marker) >>".as_slice(),
    ];
    let mut offsets = Vec::with_capacity(objects.len());

    for (number, body) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", number + 1).as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn synthetic_pclm_image_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    let bodies: &[(u32, &[u8])] = &[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /Resources 4 0 R /Contents 5 0 R >>",
        ),
        (4, b"<< /XObject << /Im1 6 0 R /Im2 7 0 R >> >>"),
        (5, b"<< /Length 5 >>\nstream\nBT ET\nendstream"),
    ];
    for (number, body) in bodies {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    for (number, data) in [(6, 0_u8), (7, 1_u8)] {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!(
            "{number} 0 obj\n<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\n"
        ).as_bytes());
        bytes.push(data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
    }
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 8\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Build a one-page PDF whose `/Contents` is a valid RunLengthDecode stream.
///
/// The `0x02 ABC 0x80` payload is one literal packet followed by EOD. qpdf's
/// `QPDF_Stream::pipeStreamData` (QPDF_Stream.cc:488-665) treats RunLength as
/// specialized, so decode levels below specialized must preserve this exact
/// filter/data pair as an all-or-nothing chain decision.
fn synthetic_runlength_contents_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>".as_slice(),
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>".as_slice(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Resources << >> /Contents 4 0 R >>"
            .as_slice(),
    ];
    let mut offsets = Vec::with_capacity(4);

    for (number, body) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", number + 1).as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }

    offsets.push(bytes.len());
    bytes.extend_from_slice(b"4 0 obj\n<< /Length 5 /Filter /RunLengthDecode >>\nstream\n");
    bytes.extend_from_slice(&[0x02, b'A', b'B', b'C', 0x80]);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

/// Build the same one-page shape with a generalized Flate stream. The test
/// mutates the resolved stream after open so the reader can establish the
/// object graph before the writer exercises malformed `/DecodeParms` handling.
fn synthetic_flate_contents_pdf(filter_as_array: bool) -> Vec<u8> {
    synthetic_flate_contents_pdf_with_payload(filter_as_array, b"ABC")
}

fn synthetic_flate_contents_pdf_with_payload(filter_as_array: bool, payload: &[u8]) -> Vec<u8> {
    // Use a stored zlib block so recompression with the writer's default
    // compression has an observable raw-byte effect.
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::none());
    encoder
        .write_all(payload)
        .expect("zlib encoder accepts fixture data");
    let compressed = encoder.finish().expect("zlib encoder finishes");

    let mut bytes = b"%PDF-1.4\n".to_vec();
    let objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>".as_slice(),
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>".as_slice(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Resources << >> /Contents 4 0 R >>"
            .as_slice(),
    ];
    let mut offsets = Vec::with_capacity(4);
    for (number, body) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", number + 1).as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    offsets.push(bytes.len());
    let filter = if filter_as_array {
        "[ /FlateDecode ]"
    } else {
        "/FlateDecode"
    };
    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Length {} /Filter {filter} >>\nstream\n",
            compressed.len(),
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&compressed);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

/// Build the same one-page shape with an explicit null `/Filter`. qpdf treats
/// null exactly like an absent filter and may therefore apply output
/// compression to the raw stream data.
fn synthetic_null_filter_contents_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>".as_slice(),
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>".as_slice(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Resources << >> /Contents 4 0 R >>"
            .as_slice(),
    ];
    let mut offsets = Vec::with_capacity(4);
    for (number, body) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", number + 1).as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    offsets.push(bytes.len());
    bytes.extend_from_slice(
        b"4 0 obj\n<< /Length 3 /Filter null >>\nstream\nABC\nendstream\nendobj\n",
    );

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn direct_content_stream(payload: &[u8]) -> Stream {
    let mut dict = Dictionary::new();
    dict.insert(
        "Length",
        Object::Integer(i64::try_from(payload.len()).expect("small direct content stream")),
    );
    Stream::new(dict, payload.to_vec())
}

fn direct_content_stream_with_null_key(payload: &[u8]) -> Stream {
    let mut dict = Dictionary::new();
    dict.insert(
        "Length",
        Object::Integer(i64::try_from(payload.len()).expect("small direct content stream")),
    );
    dict.insert("Metadata", Object::Null);
    Stream::new(dict, payload.to_vec())
}

fn direct_content_stream_with_null_reference(payload: &[u8], object_ref: ObjectRef) -> Stream {
    let mut dict = Dictionary::new();
    dict.insert(
        "Length",
        Object::Integer(i64::try_from(payload.len()).expect("small direct content stream")),
    );
    dict.insert("Metadata", Object::Reference(object_ref));
    Stream::new(dict, payload.to_vec())
}

fn page_object(contents: Object) -> Object {
    let mut dict = Dictionary::new();
    dict.insert("Type", Object::Name(b"Page".to_vec()));
    dict.insert("Parent", Object::Reference(ObjectRef::new(2, 0)));
    dict.insert(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(1),
            Object::Integer(1),
        ]),
    );
    dict.insert("Resources", Object::Dictionary(Dictionary::new()));
    dict.insert("Contents", contents);
    Object::Dictionary(dict)
}

fn synthetic_content_holder_shapes_pdf() -> flpdf::Result<Pdf<Cursor<Vec<u8>>>> {
    let mut pdf = Pdf::open(Cursor::new(synthetic_flate_contents_pdf(false)))?;

    // Page 3: direct /Contents Stream.
    pdf.set_object(
        ObjectRef::new(3, 0),
        page_object(Object::Stream(direct_content_stream(b"P3\rD"))),
    );

    // Page 5: direct /Contents array whose element is ref -> ref -> Stream.
    pdf.set_object(
        ObjectRef::new(5, 0),
        page_object(Object::Array(vec![Object::Reference(ObjectRef::new(
            11, 0,
        ))])),
    );
    pdf.set_object(
        ObjectRef::new(11, 0),
        Object::Reference(ObjectRef::new(12, 0)),
    );
    pdf.set_object(
        ObjectRef::new(12, 0),
        Object::Stream(direct_content_stream(b"P5\rR")),
    );

    // Page 6: direct /Contents array containing a direct Stream.
    pdf.set_object(
        ObjectRef::new(6, 0),
        page_object(Object::Array(vec![Object::Stream(direct_content_stream(
            b"P6\rD",
        ))])),
    );

    // Page 4: /Contents ref -> ref -> Stream.
    pdf.set_object(
        ObjectRef::new(4, 0),
        page_object(Object::Reference(ObjectRef::new(8, 0))),
    );
    pdf.set_object(
        ObjectRef::new(8, 0),
        Object::Reference(ObjectRef::new(9, 0)),
    );
    pdf.set_object(
        ObjectRef::new(9, 0),
        Object::Stream(direct_content_stream(b"P4\rR")),
    );

    // Page 7: /Contents ref -> ref -> Array containing a direct Stream and
    // an array element ref -> ref -> Stream.
    pdf.set_object(
        ObjectRef::new(7, 0),
        page_object(Object::Reference(ObjectRef::new(13, 0))),
    );
    pdf.set_object(
        ObjectRef::new(13, 0),
        Object::Reference(ObjectRef::new(14, 0)),
    );
    pdf.set_object(
        ObjectRef::new(14, 0),
        Object::Array(vec![
            Object::Stream(direct_content_stream(b"P7\rD")),
            Object::Reference(ObjectRef::new(15, 0)),
        ]),
    );
    pdf.set_object(
        ObjectRef::new(15, 0),
        Object::Reference(ObjectRef::new(16, 0)),
    );
    pdf.set_object(
        ObjectRef::new(16, 0),
        Object::Stream(direct_content_stream(b"P7\rR")),
    );

    let pages_ref = ObjectRef::new(2, 0);
    let mut pages = pdf.resolve(pages_ref)?.clone();
    let pages_dict = pages.as_dict_mut().expect("pages dictionary");
    pages_dict.insert("Count", Object::Integer(5));
    pages_dict.insert(
        "Kids",
        Object::Array(
            [3, 4, 5, 6, 7]
                .into_iter()
                .map(|number| Object::Reference(ObjectRef::new(number, 0)))
                .collect(),
        ),
    );
    pdf.set_object(pages_ref, pages);
    Ok(pdf)
}

// qpdf 11.9.0 accepts the direct-array and ref-to-array fixtures below.
// Separate raw fixtures for ref -> ref -> stream and ref -> ref -> array were
// probed during this follow-up; qpdf --check rejects both with `expected
// endobj` and reports that the page Contents is neither a stream nor an array.
// Those invalid graphs remain covered by synthetic_content_holder_shapes_pdf,
// whose output is intentionally inspected as bytes without qpdf --check.
#[derive(Clone, Copy)]
enum ValidContentsHolderShape {
    DirectArray,
    RefArray,
}

fn valid_contents_holder_shape_pdf(shape: ValidContentsHolderShape) -> Vec<u8> {
    let (page_contents, extra_objects): (&[u8], Vec<Vec<u8>>) = match shape {
        ValidContentsHolderShape::DirectArray => (
            b"[4 0 R 5 0 R]",
            vec![
                b"<< /Length 3 >>\nstream\nA\rB\nendstream".to_vec(),
                b"<< /Length 3 >>\nstream\nC\rD\nendstream".to_vec(),
            ],
        ),
        ValidContentsHolderShape::RefArray => (
            b"4 0 R",
            vec![
                b"[5 0 R 6 0 R]".to_vec(),
                b"<< /Length 3 >>\nstream\nA\rB\nendstream".to_vec(),
                b"<< /Length 3 >>\nstream\nC\rD\nendstream".to_vec(),
            ],
        ),
    };

    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Resources << >> /Contents {} >>",
            String::from_utf8(page_contents.to_vec()).expect("literal PDF reference syntax")
        )
        .into_bytes(),
    ];
    objects.extend(extra_objects);

    let mut offsets = Vec::with_capacity(objects.len());
    for (number, body) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", number + 1).as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    bytes
}

fn rewrite_valid_contents_holder_shape(
    shape: ValidContentsHolderShape,
) -> flpdf::Result<(Vec<u8>, Vec<u8>)> {
    qpdf_11_9_0()?;
    let source = valid_contents_holder_shape_pdf(shape);
    assert_qpdf_check(&source)?;

    let mut pdf = Pdf::open(Cursor::new(source.clone()))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_qdf_mode(true);
    writer.set_static_id(true);
    writer.set_output_memory()?;
    writer.write()?;
    Ok((source, writer.get_buffer()?))
}

fn assert_normalized_content_payloads(output: &[u8], shape: &str) {
    for payload in [b"A\nB".as_slice(), b"C\nD".as_slice()] {
        assert!(
            contains_bytes(output, payload),
            "{shape} must contain normalized payload {payload:?}"
        );
    }
    assert!(
        !contains_bytes(output, b"A\rB") && !contains_bytes(output, b"C\rD"),
        "{shape} must not retain CRLF payloads"
    );
}

fn attach_flate_metadata(pdf: &mut Pdf<Cursor<Vec<u8>>>, payload: &[u8]) -> ObjectRef {
    let metadata_ref = ObjectRef::new(15, 0);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::none());
    encoder
        .write_all(payload)
        .expect("zlib encoder accepts fixture data");
    let metadata_data = encoder.finish().expect("zlib encoder finishes");
    let mut metadata_dict = Dictionary::new();
    metadata_dict.insert("Type", Object::Name(b"Metadata".to_vec()));
    metadata_dict.insert("Subtype", Object::Name(b"XML".to_vec()));
    metadata_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    metadata_dict.insert(
        "Length",
        Object::Integer(i64::try_from(metadata_data.len()).expect("small fixture")),
    );
    pdf.set_object(
        metadata_ref,
        Object::Stream(Stream::new(metadata_dict, metadata_data)),
    );
    let root_ref = pdf.root_ref().expect("catalog reference");
    let mut catalog = pdf.resolve(root_ref).expect("catalog must resolve").clone();
    catalog
        .as_dict_mut()
        .expect("catalog dictionary")
        .insert("Metadata", Object::Reference(metadata_ref));
    pdf.set_object(root_ref, catalog);
    metadata_ref
}

fn attach_plain_metadata(pdf: &mut Pdf<Cursor<Vec<u8>>>, payload: &[u8]) -> ObjectRef {
    let metadata_ref = ObjectRef::new(20, 0);
    let mut metadata_dict = Dictionary::new();
    metadata_dict.insert("Type", Object::Name(b"Metadata".to_vec()));
    metadata_dict.insert("Subtype", Object::Name(b"XML".to_vec()));
    metadata_dict.insert(
        "Length",
        Object::Integer(i64::try_from(payload.len()).expect("small metadata fixture")),
    );
    pdf.set_object(
        metadata_ref,
        Object::Stream(Stream::new(metadata_dict, payload.to_vec())),
    );
    let root_ref = pdf.root_ref().expect("catalog reference");
    let mut catalog = pdf.resolve(root_ref).expect("catalog must resolve").clone();
    catalog
        .as_dict_mut()
        .expect("catalog dictionary")
        .insert("Metadata", Object::Reference(metadata_ref));
    pdf.set_object(root_ref, catalog);
    metadata_ref
}

fn copy_encryption_source_from_donor(donor: &mut Pdf<Cursor<Vec<u8>>>) -> CopyEncryptionSource {
    let encrypt_ref = donor
        .trailer()
        .get_ref("Encrypt")
        .expect("encrypted donor must have /Encrypt");
    let encrypt_dict = donor
        .resolve(encrypt_ref)
        .expect("donor /Encrypt must resolve")
        .as_dict()
        .expect("donor /Encrypt must be a dictionary")
        .clone();
    let id0 = donor
        .trailer()
        .get("ID")
        .and_then(Object::as_array)
        .and_then(|ids| ids.first())
        .and_then(Object::as_string)
        .expect("encrypted donor must have /ID[0]")
        .to_vec();
    CopyEncryptionSource {
        encrypt_dict,
        file_key: donor
            .encryption_file_key()
            .expect("authenticated donor must expose file key"),
        id0,
        object_key_alg: ObjectKeyAlg::Aes,
    }
}

fn metadata_snapshot(bytes: Vec<u8>) -> (Option<Object>, Vec<u8>) {
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("rewritten PDF must reopen");
    let root_ref = pdf.root_ref().expect("catalog reference");
    let metadata_ref = pdf
        .resolve(root_ref)
        .expect("catalog must resolve")
        .as_dict()
        .expect("catalog must be a dictionary")
        .get_ref("Metadata")
        .expect("catalog must reference metadata");
    let metadata = pdf
        .resolve(metadata_ref)
        .expect("metadata must resolve")
        .as_stream()
        .expect("metadata must be a stream")
        .clone();
    (metadata.dict.get("Filter").cloned(), metadata.data)
}

fn assert_qpdf_check(bytes: &[u8]) -> flpdf::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("qpdf-writer-contract.pdf");
    std::fs::write(&path, bytes)?;
    let check = Command::new("qpdf").arg("--check").arg(&path).output()?;
    assert!(check.status.success(), "qpdf --check failed: {check:?}");
    Ok(())
}

fn runlength_contents_snapshot(bytes: Vec<u8>) -> (Option<Object>, Vec<u8>) {
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("rewritten PDF must reopen");
    let catalog_ref = pdf.root_ref().expect("fixture must have a catalog");
    let catalog = pdf
        .resolve(catalog_ref)
        .expect("catalog must resolve")
        .clone();
    let pages_ref = catalog
        .as_dict()
        .expect("catalog must be a dictionary")
        .get_ref("Pages")
        .expect("catalog must reference pages");
    let pages = pdf.resolve(pages_ref).expect("pages must resolve").clone();
    let page_ref = pages
        .as_dict()
        .expect("pages must be a dictionary")
        .get("Kids")
        .and_then(Object::as_array)
        .and_then(|kids| kids.first())
        .and_then(Object::as_ref_id)
        .expect("pages must contain one page reference");
    let page = pdf.resolve(page_ref).expect("page must resolve").clone();
    let contents_ref = page
        .as_dict()
        .expect("page must be a dictionary")
        .get_ref("Contents")
        .expect("page must reference contents");
    let stream = pdf
        .resolve(contents_ref)
        .expect("contents must resolve")
        .as_stream()
        .expect("contents must be a stream")
        .clone();
    (stream.dict.get("Filter").cloned(), stream.data)
}

fn rewrite_runlength_contents(
    decode_level: Option<DecodeLevel>,
    stream_data_mode: Option<StreamDataMode>,
) -> flpdf::Result<Vec<u8>> {
    let mut pdf = Pdf::open(Cursor::new(synthetic_runlength_contents_pdf()))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    if let Some(level) = decode_level {
        writer.set_decode_level(level);
    }
    if let Some(mode) = stream_data_mode {
        writer.set_stream_data_mode(mode);
    }
    writer.set_output_memory()?;
    writer.write()?;
    writer.get_buffer()
}

fn decoded_runlength_snapshot(bytes: Vec<u8>) -> flpdf::Result<(Option<Object>, Vec<u8>)> {
    let (filter, data) = runlength_contents_snapshot(bytes);
    let mut dict = Dictionary::new();
    if let Some(filter) = filter.clone() {
        dict.insert("Filter", filter);
    }
    Ok((filter, flpdf::filters::decode_stream_data(&dict, &data)?))
}

#[test]
fn write_before_output_returns_err() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);
    assert!(writer.write().is_err());
    Ok(())
}

#[test]
fn get_buffer_before_write_returns_err() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory()?;
    assert!(writer.get_buffer().is_err());
    Ok(())
}

#[test]
fn output_configuration_can_happen_only_once() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory()?;
    assert!(writer.set_output_memory().is_err());
    Ok(())
}

#[test]
fn write_can_happen_only_once() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory()?;
    writer.write()?;
    assert!(writer.write().is_err());
    Ok(())
}

#[test]
fn get_buffer_from_non_memory_writer_returns_err() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_writer(Cursor::new(Vec::new()))?;
    writer.write()?;
    assert!(writer.get_buffer().is_err());
    Ok(())
}

#[test]
fn get_buffer_after_first_retrieval_returns_err() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory()?;
    writer.write()?;
    let _ = writer.get_buffer()?;
    assert!(writer.get_buffer().is_err());
    Ok(())
}

#[test]
fn qpdf_checks_pdf_writer_memory_output() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory()?;
    writer.write()?;
    let buffer = writer.get_buffer()?;

    let dir = tempfile::tempdir()?;
    let output_path = dir.path().join("rewrite.pdf");
    std::fs::write(&output_path, buffer)?;
    let check = Command::new("qpdf")
        .arg("--check")
        .arg(&output_path)
        .output()?;
    assert!(check.status.success(), "qpdf --check failed: {check:?}");
    Ok(())
}

#[test]
fn pdf_writer_linearization_is_a_canonical_output_route() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let file = File::open("../../tests/fixtures/compat/one-page.pdf")?;
    let mut pdf = Pdf::open(BufReader::new(file))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;

    assert!(output
        .windows(b"/Linearized".len())
        .any(|window| window == b"/Linearized"));
    let dir = tempfile::tempdir()?;
    let output_path = dir.path().join("linearized.pdf");
    std::fs::write(&output_path, output)?;
    let check = Command::new("qpdf")
        .arg("--check-linearization")
        .arg(&output_path)
        .output()?;
    assert!(
        check.status.success(),
        "qpdf --check-linearization failed: {check:?}"
    );
    Ok(())
}

#[test]
fn pdf_writer_linearization_places_extra_header_after_parameter_dictionary() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let file = File::open("../../tests/fixtures/compat/one-page.pdf")?;
    let mut pdf = Pdf::open(BufReader::new(file))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_extra_header_text("% linearized-extra");
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;

    let linearized = output
        .windows(b"/Linearized".len())
        .position(|window| window == b"/Linearized")
        .expect("linearization parameter dictionary must be present");
    let extra = output
        .windows(b"% linearized-extra\n".len())
        .position(|window| window == b"% linearized-extra\n")
        .expect("extra header text must be emitted with a trailing newline");
    assert!(extra > linearized);

    let dir = tempfile::tempdir()?;
    let output_path = dir.path().join("linearized-extra-header.pdf");
    std::fs::write(&output_path, output)?;
    let check = Command::new("qpdf")
        .arg("--check-linearization")
        .arg(&output_path)
        .output()?;
    assert!(
        check.status.success(),
        "qpdf --check-linearization failed: {check:?}"
    );
    Ok(())
}

#[test]
fn pdf_writer_linearization_owns_pass1_and_result_metadata() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let file = File::open("../../tests/fixtures/compat/one-page.pdf")?;
    let mut pdf = Pdf::open(BufReader::new(file))?;
    let dir = tempfile::tempdir()?;
    let pass1_path = dir.path().join("pass1.pdf");
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_linearization_pass1_filename(&pass1_path);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;

    assert!(!std::fs::read(&pass1_path)?.is_empty());
    let root = writer
        .get_renumbered_obj_gen(ObjectRef::new(1, 0))?
        .expect("linearized Catalog must be present in the result map");
    let xref = writer.get_written_xref_table()?;
    let XrefEntry::Uncompressed { offset } = xref
        .get(&root)
        .expect("linearized Catalog must have a written xref entry")
    else {
        panic!("linearized Catalog must be uncompressed");
    };
    assert!(output[*offset as usize..].starts_with(format!("{} 0 obj\n", root.number).as_bytes()));
    Ok(())
}

#[test]
fn pdf_writer_linearization_preserves_authenticated_source_encryption() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let password = b"linearize-source".to_vec();
    let mut source_input = Pdf::open(Cursor::new(synthetic_flate_contents_pdf(false)))?;
    let source_settings = WriterTestSettings {
        object_streams: ObjectStreamMode::Disable,
        compress_streams: CompressStreams::No,
        static_id: true,
        static_aes_iv: true,
        encrypt: Some(EncryptParams::v4_aes128(password.clone(), password.clone())),
        ..WriterTestSettings::default()
    };
    let mut encrypted_source = Vec::new();
    write_with_settings(&mut source_input, &mut encrypted_source, &source_settings)?;
    let mut source = Pdf::open_with_options(
        Cursor::new(encrypted_source),
        PdfOpenOptions {
            password: password.clone(),
            ..PdfOpenOptions::default()
        },
    )?;
    let mut writer = PdfWriter::new(&mut source);
    writer.set_linearization(true);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;

    let rewritten = Pdf::open_with_options(
        Cursor::new(output.clone()),
        PdfOpenOptions {
            password: password.clone(),
            ..PdfOpenOptions::default()
        },
    )?;
    assert!(rewritten.is_encrypted());
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("linearized-encrypted.pdf");
    std::fs::write(&path, output)?;
    let check = Command::new("qpdf")
        .arg("--password=linearize-source")
        .arg("--check-linearization")
        .arg(&path)
        .output()?;
    assert!(check.status.success(), "qpdf check failed: {check:?}");
    assert!(rewritten.root_ref().is_some());
    Ok(())
}

#[test]
fn pdf_writer_linearization_supports_encryption_with_object_streams() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let password = b"linearize-encrypt".to_vec();
    let mut pdf = Pdf::open(Cursor::new(synthetic_flate_contents_pdf(false)))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_static_id(true);
    writer.set_static_aes_iv(true);
    writer.set_encryption_parameters(EncryptParams::v4_aes128(password.clone(), password.clone()));
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;

    let rewritten = Pdf::open_with_options(
        Cursor::new(output.clone()),
        PdfOpenOptions {
            password: password.clone(),
            ..PdfOpenOptions::default()
        },
    )?;
    assert!(rewritten.is_encrypted());
    assert!(output
        .windows(b"/Type /ObjStm".len())
        .any(|window| window == b"/Type /ObjStm"));
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("linearized-encrypted-objstm.pdf");
    std::fs::write(&path, output)?;
    let check = Command::new("qpdf")
        .arg("--password=linearize-encrypt")
        .arg("--check-linearization")
        .arg(&path)
        .output()?;
    assert!(check.status.success(), "qpdf check failed: {check:?}");
    Ok(())
}

#[test]
fn pdf_writer_full_rewrite_removes_prev_from_incremental_source() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    // Build the source revision directly. qpdf has no incremental writer, so
    // this test must not use the removed flpdf append-only route merely to
    // manufacture a `/Prev` chain.
    let mut incremental = std::fs::read("../../tests/fixtures/minimal.pdf")?;
    let marker = b"startxref\n";
    let marker_offset = incremental
        .windows(marker.len())
        .rposition(|window| window == marker)
        .expect("minimal.pdf has startxref");
    let previous_xref: u64 = std::str::from_utf8(
        incremental[marker_offset + marker.len()..]
            .split(|byte| *byte == b'\n')
            .next()
            .expect("minimal.pdf has startxref value"),
    )
    .map_err(|error| flpdf::Error::Unsupported(format!("invalid fixture startxref: {error}")))?
    .trim()
    .parse()
    .map_err(|error| flpdf::Error::Unsupported(format!("invalid fixture startxref: {error}")))?;
    if !incremental.ends_with(b"\n") {
        incremental.push(b'\n');
    }
    let root_offset = incremental.len();
    incremental.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let xref_offset = incremental.len();
    incremental.extend_from_slice(
        format!(
            "xref\n1 1\n{root_offset:010} 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R /Prev {previous_xref} >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );
    assert!(incremental.windows(5).any(|window| window == b"/Prev"));

    let mut rewritten_pdf = Pdf::open(Cursor::new(incremental))?;
    let mut writer = PdfWriter::new(&mut rewritten_pdf);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;
    assert!(output.starts_with(b"%PDF-"));
    assert!(!output.windows(5).any(|window| window == b"/Prev"));

    let dir = tempfile::tempdir()?;
    let output_path = dir.path().join("full-rewrite.pdf");
    std::fs::write(&output_path, output)?;
    let check = Command::new("qpdf")
        .arg("--check")
        .arg(&output_path)
        .output()?;
    assert!(check.status.success(), "qpdf --check failed: {check:?}");
    Ok(())
}

#[test]
fn pdf_writer_qdf_normalizes_crlf_page_contents() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = Pdf::open(Cursor::new(synthetic_flate_contents_pdf_with_payload(
        false, b"A\rB",
    )))?;
    let mut output = Vec::new();
    let settings = WriterTestSettings {
        qdf: true,
        object_streams: ObjectStreamMode::Disable,
        ..WriterTestSettings::default()
    };
    write_with_settings(&mut pdf, &mut output, &settings)?;
    assert_qpdf_check(&output)?;

    let (filter, data) = runlength_contents_snapshot(output);
    assert_eq!(filter, None);
    assert_eq!(data, b"A\nB");
    Ok(())
}

#[test]
fn pdf_writer_qdf_suppresses_null_keys_in_direct_content_streams() -> flpdf::Result<()> {
    let mut pdf = Pdf::open(Cursor::new(synthetic_flate_contents_pdf(false)))?;
    pdf.set_object(
        ObjectRef::new(3, 0),
        page_object(Object::Stream(direct_content_stream_with_null_key(b"AB"))),
    );

    let mut output = Vec::new();
    let settings = WriterTestSettings {
        qdf: true,
        object_streams: ObjectStreamMode::Disable,
        static_id: true,
        ..WriterTestSettings::default()
    };
    write_with_settings(&mut pdf, &mut output, &settings)?;

    assert!(contains_bytes(&output, b"AB"));
    assert!(!contains_bytes(&output, b"/Metadata null"));
    Ok(())
}

#[test]
fn pdf_writer_suppresses_null_keys_in_direct_content_streams() -> flpdf::Result<()> {
    let mut pdf = Pdf::open(Cursor::new(synthetic_flate_contents_pdf(false)))?;
    pdf.set_object(
        ObjectRef::new(3, 0),
        page_object(Object::Stream(direct_content_stream_with_null_key(b"AB"))),
    );

    let mut output = Vec::new();
    let settings = WriterTestSettings {
        content_normalization: true,
        compress_streams: CompressStreams::No,
        object_streams: ObjectStreamMode::Disable,
        static_id: true,
        ..WriterTestSettings::default()
    };
    write_with_settings(&mut pdf, &mut output, &settings)?;

    assert!(contains_bytes(&output, b"AB"));
    assert!(!contains_bytes(&output, b"/Metadata null"));
    Ok(())
}

#[test]
fn pdf_writer_qdf_suppresses_resolved_null_keys_in_direct_content_streams() -> flpdf::Result<()> {
    let null_ref = ObjectRef::new(4, 0);
    let mut pdf = Pdf::open(Cursor::new(synthetic_flate_contents_pdf(false)))?;
    pdf.set_object(null_ref, Object::Null);
    pdf.set_object(
        ObjectRef::new(3, 0),
        page_object(Object::Stream(direct_content_stream_with_null_reference(
            b"A\rB", null_ref,
        ))),
    );

    let mut output = Vec::new();
    let settings = WriterTestSettings {
        qdf: true,
        object_streams: ObjectStreamMode::Disable,
        static_id: true,
        ..WriterTestSettings::default()
    };
    write_with_settings(&mut pdf, &mut output, &settings)?;

    assert!(contains_bytes(&output, b"A\nB"));
    assert!(!contains_bytes(&output, b"/Metadata null"));
    assert!(!contains_bytes(&output, b"4 0 obj"));
    Ok(())
}

#[test]
fn pdf_writer_encrypted_direct_content_streams_use_handle_string_encryption() -> flpdf::Result<()> {
    let password = b"direct-content-password".to_vec();
    let mut params = EncryptParams::v4_aes128(password.clone(), password);
    params.encrypt_metadata = true;
    let mut pdf = Pdf::open(Cursor::new(synthetic_flate_contents_pdf(false)))?;
    pdf.set_object(
        ObjectRef::new(3, 0),
        page_object(Object::Stream(direct_content_stream_with_null_key(b"AB"))),
    );

    let mut output = Vec::new();
    let settings = WriterTestSettings {
        qdf: true,
        object_streams: ObjectStreamMode::Disable,
        static_aes_iv: true,
        static_id: true,
        encrypt: Some(params),
        ..WriterTestSettings::default()
    };
    write_with_settings(&mut pdf, &mut output, &settings)?;

    assert!(contains_bytes(&output, b"stream"));
    assert!(contains_bytes(&output, b"endstream"));
    assert!(!contains_bytes(&output, b"/Metadata null"));
    Ok(())
}

#[test]
fn pdf_writer_qdf_normalizes_every_page_contents_shape() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = synthetic_content_holder_shapes_pdf()?;

    let source_pages = pages::page_refs(&mut pdf)?;
    let terminal_refs: Vec<_> = source_pages
        .iter()
        .map(|page_ref| {
            pages::page_content_stream_entries_tolerant(&mut pdf, *page_ref).map(|entries| {
                entries
                    .into_iter()
                    .map(|(reference, _)| reference)
                    .collect()
            })
        })
        .collect::<flpdf::Result<Vec<Vec<_>>>>()?;
    assert_eq!(terminal_refs[0], vec![None]);
    assert_eq!(terminal_refs[1], vec![Some(ObjectRef::new(9, 0))]);
    assert_eq!(terminal_refs[2], vec![Some(ObjectRef::new(12, 0))]);
    assert_eq!(terminal_refs[3], vec![None]);
    assert_eq!(terminal_refs[4], vec![None, Some(ObjectRef::new(16, 0))]);

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_qdf_mode(true);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;
    // Direct Stream values are valid in flpdf's in-memory Object graph but
    // cannot be reopened by qpdf as indirect PDF stream objects. This
    // synthetic graph therefore covers direct streams/arrays and the
    // ref -> ref -> stream/array cases as byte-level normalization checks;
    // the ordinary indirect fixture tests above and below retain qpdf
    // --check coverage for valid serialized PDFs.
    for (shape, normalized) in [
        ("direct Stream", b"P3\nD".as_slice()),
        ("ref -> ref -> Stream", b"P4\nR".as_slice()),
        ("array element ref -> ref -> Stream", b"P5\nR".as_slice()),
        ("direct array element Stream", b"P6\nD".as_slice()),
        ("ref -> ref -> Array direct Stream", b"P7\nD".as_slice()),
        (
            "ref -> ref -> Array element ref -> ref -> Stream",
            b"P7\nR".as_slice(),
        ),
    ] {
        assert!(
            contains_bytes(&output, normalized),
            "{shape} must be normalized independently"
        );
    }
    assert!(!contains_bytes(&output, b"\r"));
    Ok(())
}

#[test]
fn pdf_writer_qdf_normalizes_qpdf_checked_direct_contents_array() -> flpdf::Result<()> {
    let (_source, output) =
        rewrite_valid_contents_holder_shape(ValidContentsHolderShape::DirectArray)?;
    assert_qpdf_check(&output)?;
    assert_normalized_content_payloads(&output, "direct /Contents array");
    Ok(())
}

#[test]
fn pdf_writer_qdf_normalizes_qpdf_checked_contents_ref_array() -> flpdf::Result<()> {
    let (_source, output) =
        rewrite_valid_contents_holder_shape(ValidContentsHolderShape::RefArray)?;
    assert_qpdf_check(&output)?;
    assert_normalized_content_payloads(&output, "/Contents ref -> array");
    Ok(())
}

#[test]
fn pdf_writer_copy_encryption_preserves_donor_cleartext_metadata_policy() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let password = b"donor-user";

    let mut donor_input = Pdf::open(Cursor::new(synthetic_flate_contents_pdf(false)))?;
    attach_plain_metadata(&mut donor_input, b"donor cleartext metadata");
    let mut donor_settings = WriterTestSettings {
        object_streams: ObjectStreamMode::Disable,
        compress_streams: CompressStreams::No,
        static_id: true,
        static_aes_iv: true,
        ..WriterTestSettings::default()
    };
    let mut params = EncryptParams::v4_aes128(password.to_vec(), password.to_vec());
    params.encrypt_metadata = false;
    donor_settings.encrypt = Some(params);
    let mut donor_bytes = Vec::new();
    write_with_settings(&mut donor_input, &mut donor_bytes, &donor_settings)?;

    let mut donor = Pdf::open_with_options(
        Cursor::new(donor_bytes),
        PdfOpenOptions {
            password: password.to_vec(),
            ..PdfOpenOptions::default()
        },
    )?;
    assert!(
        !donor
            .encryption_info()?
            .expect("donor must be encrypted")
            .encrypt_metadata
    );
    let copy_source = copy_encryption_source_from_donor(&mut donor);

    let target_metadata = b"target cleartext metadata";
    let mut target = Pdf::open(Cursor::new(synthetic_flate_contents_pdf(false)))?;
    attach_plain_metadata(&mut target, target_metadata);
    let copy_settings = WriterTestSettings {
        object_streams: ObjectStreamMode::Disable,
        compress_streams: CompressStreams::No,
        static_aes_iv: true,
        copy_encryption: Some(copy_source),
        ..WriterTestSettings::default()
    };
    let mut output = Vec::new();
    write_with_settings(&mut target, &mut output, &copy_settings)?;

    assert!(contains_bytes(&output, b"/EncryptMetadata false"));
    assert!(
        contains_bytes(&output, target_metadata),
        "the Catalog /Metadata payload must remain cleartext"
    );
    assert!(
        !contains_bytes(&output, b"/Crypt"),
        "copy-encryption cleartext metadata must not acquire an Identity Crypt filter"
    );

    let dir = tempfile::tempdir()?;
    let encrypted_path = dir.path().join("copied-cleartext-metadata.pdf");
    let decrypted_path = dir.path().join("copied-cleartext-metadata-decrypted.pdf");
    std::fs::write(&encrypted_path, &output)?;
    let check = Command::new("qpdf")
        .arg("--password=donor-user")
        .arg("--check")
        .arg(&encrypted_path)
        .output()?;
    assert!(check.status.success(), "qpdf --check failed: {check:?}");
    let decrypt = Command::new("qpdf")
        .arg("--password=donor-user")
        .arg("--decrypt")
        .arg(&encrypted_path)
        .arg(&decrypted_path)
        .output()?;
    assert!(
        decrypt.status.success(),
        "qpdf --decrypt failed: {decrypt:?}"
    );

    let mut reopened = Pdf::open_with_options(
        Cursor::new(output),
        PdfOpenOptions {
            password: password.to_vec(),
            ..PdfOpenOptions::default()
        },
    )?;
    assert!(reopened.is_encrypted());
    let rewritten_root_ref = reopened.root_ref().expect("rewritten Catalog reference");
    let rewritten_metadata_ref = reopened
        .resolve(rewritten_root_ref)?
        .as_dict()
        .expect("rewritten Catalog must be a dictionary")
        .get_ref("Metadata")
        .expect("rewritten Catalog must reference metadata");
    let metadata = reopened.resolve(rewritten_metadata_ref)?.clone();
    let metadata = metadata.as_stream().expect("metadata must be a stream");
    assert_eq!(metadata.data, target_metadata);
    assert_eq!(metadata.dict.get("Filter"), None);
    assert_eq!(metadata.dict.get("DecodeParms"), None);
    Ok(())
}

#[test]
fn pdf_writer_preserves_v4_aes_encryption_for_encrypted_input() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut source = Pdf::open_with_options(
        BufReader::new(File::open(
            "../../tests/fixtures/encrypted/v4-aes-128-r4.pdf",
        )?),
        PdfOpenOptions {
            password: b"user-v4-aes".to_vec(),
            ..PdfOpenOptions::default()
        },
    )?;
    let source_info = source
        .encryption_info()?
        .expect("encrypted fixture must expose encryption parameters");
    assert!(source.is_encrypted());
    assert!(source.user_password_matched());

    let mut writer = PdfWriter::new(&mut source);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;

    let mut rewritten = Pdf::open_with_options(
        Cursor::new(output),
        PdfOpenOptions {
            password: b"user-v4-aes".to_vec(),
            ..PdfOpenOptions::default()
        },
    )?;
    let output_info = rewritten
        .encryption_info()?
        .expect("rewritten PDF must remain encrypted");
    assert!(rewritten.is_encrypted());
    assert!(rewritten.user_password_matched());
    assert_eq!(output_info.v, source_info.v);
    assert_eq!(output_info.r, source_info.r);
    assert_eq!(output_info.length_bits, source_info.length_bits);
    assert_eq!(output_info.filter, source_info.filter);
    assert_eq!(output_info.permissions, source_info.permissions);
    assert_eq!(output_info.encrypt_metadata, source_info.encrypt_metadata);
    assert_eq!(output_info.stream_method, source_info.stream_method);
    assert_eq!(output_info.string_method, source_info.string_method);
    assert_eq!(output_info.eff_method, source_info.eff_method);
    assert_eq!(
        output_info.named_crypt_filters,
        source_info.named_crypt_filters
    );
    Ok(())
}

#[test]
fn pdf_writer_pclm_disables_source_encryption_and_stream_rewriting() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut source = Pdf::open_with_options(
        BufReader::new(File::open(
            "../../tests/fixtures/encrypted/v4-aes-128-r4.pdf",
        )?),
        PdfOpenOptions {
            password: b"user-v4-aes".to_vec(),
            ..PdfOpenOptions::default()
        },
    )?;
    let mut writer = PdfWriter::new(&mut source);
    writer.set_pclm(true);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;

    let mut rewritten = Pdf::open(Cursor::new(output.clone()))?;
    assert!(!rewritten.is_encrypted());
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("pclm-clear.pdf");
    std::fs::write(&path, output)?;
    let check = Command::new("qpdf").arg("--check").arg(&path).output()?;
    assert!(check.status.success(), "qpdf --check failed: {check:?}");
    let root = rewritten.root_ref().expect("PCLm output must have a root");
    assert!(rewritten.resolve(root)?.as_dict().is_some());
    Ok(())
}

#[test]
fn pdf_writer_preserves_all_standard_encryption_revisions() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let cases = [
        ("v1-rc4-40-r2.pdf", b"user-v1".as_slice(), true, "none"),
        ("v2-rc4-128-r3.pdf", b"user-v2".as_slice(), true, "none"),
        (
            "v4-rc4-128-r4.pdf",
            b"user-v4-rc4".as_slice(),
            true,
            "AESv2",
        ),
        (
            "v4-aes-128-r4.pdf",
            b"user-v4-aes".as_slice(),
            false,
            "AESv2",
        ),
        ("v5-aes-256-r5.pdf", b"user-v5-r5".as_slice(), true, "AESv3"),
        (
            "v5-aes-256-r6.pdf",
            b"user-v5-r6".as_slice(),
            false,
            "AESv3",
        ),
    ];

    for (fixture, password, allow_weak_crypto, expected_method) in cases {
        let mut source = Pdf::open_with_options(
            BufReader::new(File::open(format!(
                "../../tests/fixtures/encrypted/{fixture}"
            ))?),
            PdfOpenOptions {
                password: password.to_vec(),
                allow_weak_crypto,
                ..PdfOpenOptions::default()
            },
        )?;
        let source_info = source
            .encryption_info()?
            .expect("encrypted fixture must expose encryption parameters");
        let mut writer = PdfWriter::new(&mut source);
        writer.set_object_stream_mode(ObjectStreamMode::Disable);
        writer.set_output_memory()?;
        writer.write()?;
        let output = writer.get_buffer()?;

        let mut rewritten = Pdf::open_with_options(
            Cursor::new(output.clone()),
            PdfOpenOptions {
                password: password.to_vec(),
                allow_weak_crypto,
                ..PdfOpenOptions::default()
            },
        )?;
        let output_info = rewritten
            .encryption_info()?
            .expect("rewritten PDF must remain encrypted");
        assert_eq!(output_info.v, source_info.v, "{fixture} /V changed");
        assert_eq!(output_info.r, source_info.r, "{fixture} /R changed");
        assert_eq!(output_info.length_bits, source_info.length_bits);
        assert_eq!(output_info.permissions, source_info.permissions);
        assert_eq!(output_info.encrypt_metadata, source_info.encrypt_metadata);
        assert_eq!(output_info.stream_method, expected_method, "{fixture}");
        assert_eq!(output_info.string_method, expected_method, "{fixture}");
        assert_eq!(output_info.eff_method, expected_method, "{fixture}");

        let dir = tempfile::tempdir()?;
        let output_path = dir.path().join(fixture);
        std::fs::write(&output_path, output)?;
        let check = Command::new("qpdf")
            .arg(format!("--password={}", String::from_utf8_lossy(password)))
            .arg("--check")
            .arg(&output_path)
            .output()?;
        assert!(
            check.status.success(),
            "qpdf --check failed for {fixture}: {check:?}"
        );
    }
    Ok(())
}

#[test]
fn pdf_writer_forced_incompatible_version_drops_preserved_encryption() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut source = Pdf::open_with_options(
        BufReader::new(File::open(
            "../../tests/fixtures/encrypted/v4-aes-128-r4.pdf",
        )?),
        PdfOpenOptions {
            password: b"user-v4-aes".to_vec(),
            ..PdfOpenOptions::default()
        },
    )?;
    let mut writer = PdfWriter::new(&mut source);
    writer.force_pdf_version("1.5", 0);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;
    assert!(output.starts_with(b"%PDF-1.5\n"));

    let rewritten = Pdf::open(Cursor::new(output))?;
    assert!(!rewritten.is_encrypted());
    Ok(())
}

#[test]
fn pdf_writer_preserve_controls_and_transforms_disable_source_encryption() -> flpdf::Result<()> {
    for transform in ["preserve-false", "decode-generalized", "qdf"] {
        let mut source = Pdf::open_with_options(
            BufReader::new(File::open(
                "../../tests/fixtures/encrypted/v4-aes-128-r4.pdf",
            )?),
            PdfOpenOptions {
                password: b"user-v4-aes".to_vec(),
                ..PdfOpenOptions::default()
            },
        )?;
        let mut writer = PdfWriter::new(&mut source);
        match transform {
            "preserve-false" => writer.set_preserve_encryption(false),
            "decode-generalized" => writer.set_decode_level(DecodeLevel::Generalized),
            "qdf" => writer.set_qdf_mode(true),
            _ => unreachable!(),
        }
        writer.set_output_memory()?;
        writer.write()?;
        let output = writer.get_buffer()?;
        let rewritten = Pdf::open(Cursor::new(output))?;
        assert!(
            !rewritten.is_encrypted(),
            "{transform} must disable preservation"
        );
    }
    Ok(())
}

#[test]
fn pdf_writer_explicit_encryption_takes_precedence_over_source_preservation() -> flpdf::Result<()> {
    let mut source = Pdf::open_with_options(
        BufReader::new(File::open(
            "../../tests/fixtures/encrypted/v4-aes-128-r4.pdf",
        )?),
        PdfOpenOptions {
            password: b"user-v4-aes".to_vec(),
            ..PdfOpenOptions::default()
        },
    )?;
    let mut writer = PdfWriter::new(&mut source);
    writer.set_encryption_parameters(EncryptParams::v4_aes128(
        b"replacement-user".to_vec(),
        b"replacement-owner".to_vec(),
    ));
    writer.set_static_aes_iv(true);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;

    let rewritten = Pdf::open_with_options(
        Cursor::new(output),
        PdfOpenOptions {
            password: b"replacement-user".to_vec(),
            ..PdfOpenOptions::default()
        },
    )?;
    assert!(rewritten.is_encrypted());
    assert!(rewritten.user_password_matched());
    Ok(())
}

#[test]
fn pdf_writer_copy_encryption_defaults_absent_encrypt_metadata_to_encrypted() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let password = b"donor-user";

    let mut donor_input = Pdf::open(Cursor::new(synthetic_flate_contents_pdf(false)))?;
    let donor_settings = WriterTestSettings {
        object_streams: ObjectStreamMode::Disable,
        compress_streams: CompressStreams::No,
        static_id: true,
        static_aes_iv: true,
        ..WriterTestSettings::default()
    };
    let mut params = EncryptParams::v4_aes128(password.to_vec(), password.to_vec());
    params.encrypt_metadata = true;
    let donor_settings = WriterTestSettings {
        encrypt: Some(params),
        ..donor_settings
    };
    let mut donor_bytes = Vec::new();
    write_with_settings(&mut donor_input, &mut donor_bytes, &donor_settings)?;

    let mut donor = Pdf::open_with_options(
        Cursor::new(donor_bytes),
        PdfOpenOptions {
            password: password.to_vec(),
            ..PdfOpenOptions::default()
        },
    )?;
    let mut copy_source = copy_encryption_source_from_donor(&mut donor);
    copy_source.encrypt_dict.remove("EncryptMetadata");

    let target_metadata = b"target encrypted metadata";
    let mut target = Pdf::open(Cursor::new(synthetic_flate_contents_pdf(false)))?;
    attach_plain_metadata(&mut target, target_metadata);
    let copy_settings = WriterTestSettings {
        object_streams: ObjectStreamMode::Disable,
        compress_streams: CompressStreams::No,
        static_aes_iv: true,
        copy_encryption: Some(copy_source),
        ..WriterTestSettings::default()
    };
    let mut output = Vec::new();
    write_with_settings(&mut target, &mut output, &copy_settings)?;

    assert!(
        !contains_bytes(&output, target_metadata),
        "absent /EncryptMetadata must encrypt the metadata payload"
    );

    let dir = tempfile::tempdir()?;
    let encrypted_path = dir.path().join("copied-encrypted-metadata.pdf");
    let decrypted_path = dir.path().join("copied-encrypted-metadata-decrypted.pdf");
    std::fs::write(&encrypted_path, &output)?;
    let check = Command::new("qpdf")
        .arg("--password=donor-user")
        .arg("--check")
        .arg(&encrypted_path)
        .output()?;
    assert!(check.status.success(), "qpdf --check failed: {check:?}");
    let decrypt = Command::new("qpdf")
        .arg("--password=donor-user")
        .arg("--decrypt")
        .arg(&encrypted_path)
        .arg(&decrypted_path)
        .output()?;
    assert!(
        decrypt.status.success(),
        "qpdf --decrypt failed: {decrypt:?}"
    );
    Ok(())
}

#[test]
fn pdf_writer_standard_encryption_false_keeps_metadata_cleartext() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let password = b"standard-user";
    let metadata_payload = b"standard cleartext metadata";
    let mut pdf = Pdf::open(Cursor::new(synthetic_flate_contents_pdf(false)))?;
    attach_plain_metadata(&mut pdf, metadata_payload);

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_static_aes_iv(true);
    let mut params = EncryptParams::v4_aes128(password.to_vec(), password.to_vec());
    params.encrypt_metadata = false;
    writer.set_encryption_parameters(params);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;

    assert!(contains_bytes(&output, b"/EncryptMetadata false"));
    assert!(
        contains_bytes(&output, metadata_payload),
        "standard encryption must leave metadata payload cleartext"
    );

    let dir = tempfile::tempdir()?;
    let encrypted_path = dir.path().join("standard-encrypted-metadata.pdf");
    std::fs::write(&encrypted_path, &output)?;
    let check = Command::new("qpdf")
        .arg("--password=standard-user")
        .arg("--check")
        .arg(&encrypted_path)
        .output()?;
    assert!(check.status.success(), "qpdf --check failed: {check:?}");

    let mut reopened = Pdf::open_with_options(
        Cursor::new(output),
        PdfOpenOptions {
            password: password.to_vec(),
            ..PdfOpenOptions::default()
        },
    )?;
    let root_ref = reopened.root_ref().expect("rewritten Catalog reference");
    let metadata_ref = reopened
        .resolve(root_ref)?
        .as_dict()
        .expect("rewritten Catalog must be a dictionary")
        .get_ref("Metadata")
        .expect("rewritten Catalog must reference metadata");
    let metadata = reopened.resolve(metadata_ref)?.clone();
    let metadata = metadata.as_stream().expect("metadata must be a stream");
    assert_eq!(metadata.data, metadata_payload);
    assert_eq!(metadata.dict.get("Filter"), None);
    assert_eq!(metadata.dict.get("DecodeParms"), None);
    Ok(())
}

#[test]
fn pdf_writer_qdf_memory_output_checks_indirect_contents_array_fixture() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let file = File::open("../../tests/fixtures/compat/qdf-contents-ref-array.pdf")?;
    let mut pdf = Pdf::open(BufReader::new(file))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_qdf_mode(true);
    writer.set_static_id(true);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;

    assert_qpdf_check(&output)?;
    let contents_marker = b"%% Contents for page 1\n";
    assert_eq!(
        output
            .windows(contents_marker.len())
            .filter(|window| *window == contents_marker)
            .count(),
        2,
        "QDF must mark both indirect array-element streams"
    );
    assert!(contains_bytes(
        &output,
        b"%% Contents for page 1\n%% Original object ID: 6 0\n"
    ));
    assert!(contains_bytes(
        &output,
        b"%% Contents for page 1\n%% Original object ID: 7 0\n"
    ));
    assert!(contains_bytes(&output, b"stream\nA\nendstream"));
    assert!(contains_bytes(&output, b"stream\nB\nendstream"));
    Ok(())
}

#[test]
fn pdf_writer_preserves_unreferenced_objects_only_when_enabled() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let source = synthetic_unreferenced_object_pdf();
    let marker = b"unreferenced-marker";

    let mut default_pdf = Pdf::open(Cursor::new(source.clone()))?;
    let mut default_writer = PdfWriter::new(&mut default_pdf);
    default_writer.set_object_stream_mode(ObjectStreamMode::Disable);
    default_writer.set_output_memory()?;
    default_writer.write()?;
    let default_output = default_writer.get_buffer()?;
    assert!(!contains_bytes(&default_output, marker));

    let mut preserved_pdf = Pdf::open(Cursor::new(source))?;
    let mut preserved_writer = PdfWriter::new(&mut preserved_pdf);
    preserved_writer.set_object_stream_mode(ObjectStreamMode::Disable);
    preserved_writer.set_preserve_unreferenced_objects(true);
    preserved_writer.set_static_id(true);
    preserved_writer.set_output_memory()?;
    preserved_writer.write()?;
    let preserved_output = preserved_writer.get_buffer()?;
    assert!(contains_bytes(&preserved_output, marker));
    assert!(!contains_bytes(&preserved_output, b"/Prev"));

    let dir = tempfile::tempdir()?;
    let output_path = dir.path().join("preserved-unreferenced.pdf");
    std::fs::write(&output_path, preserved_output)?;
    let check = Command::new("qpdf")
        .arg("--check")
        .arg(&output_path)
        .output()?;
    assert!(check.status.success(), "qpdf --check failed: {check:?}");

    Ok(())
}

#[test]
fn memory_full_rewrite_has_fresh_output() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory()?;
    writer.set_static_id(true);
    writer.write()?;

    let buffer = writer.get_buffer()?;
    assert!(buffer.starts_with(b"%PDF-"));
    assert!(!buffer.windows(5).any(|window| window == b"/Prev"));

    Ok(())
}

#[test]
fn final_version_is_available_before_write() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);
    let version = writer.get_final_version()?;
    writer.set_output_memory()?;
    writer.write()?;

    let buffer = writer.get_buffer()?;
    let mut first_line = String::new();
    BufReader::new(Cursor::new(buffer)).read_line(&mut first_line)?;
    assert!(first_line.contains(&version));

    Ok(())
}

#[test]
fn qpdf_setter_surface_compiles() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_stream_data_mode(StreamDataMode::Preserve);
    writer.set_decode_level(DecodeLevel::Generalized);
    writer.set_compress_streams(false);
    writer.set_recompress_flate(true);
    writer.set_content_normalization(false);
    writer.set_qdf_mode(true);
    writer.set_preserve_unreferenced_objects(true);
    writer.set_newline_before_endstream(false);
    writer.set_deterministic_id(true);
    writer.set_static_id(true);
    writer.set_linearization(false);
    writer.set_output_memory()?;

    Ok(())
}

#[test]
fn writer_result_surface_compiles() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);

    assert!(writer.get_renumbered_obj_gen(ObjectRef::new(1, 0)).is_err());
    assert!(writer.get_written_xref_table().is_err());

    writer.set_output_memory()?;
    writer.write()?;

    let output = writer.get_buffer()?;
    assert_eq!(
        writer.get_renumbered_obj_gen(ObjectRef::new(1, 0))?,
        Some(ObjectRef::new(1, 0))
    );
    assert_eq!(
        writer.get_renumbered_obj_gen(ObjectRef::new(2, 0))?,
        Some(ObjectRef::new(2, 0))
    );
    let xref = writer.get_written_xref_table()?;
    assert!(!xref.keys().any(|object_ref| object_ref.number == 0));
    assert!(!xref
        .values()
        .any(|entry| matches!(entry, XrefEntry::Free { .. })));
    assert!(xref.keys().all(|object_ref| object_ref.generation == 0));
    for number in [1_u32, 2] {
        let entry = xref
            .get(&ObjectRef::new(number, 0))
            .expect("emitted object must have a result xref entry");
        let XrefEntry::Uncompressed { offset } = entry else {
            panic!("minimal object {number} must be uncompressed: {entry:?}");
        };
        let marker = format!("{number} 0 obj\n");
        assert_eq!(
            &output[*offset as usize..][..marker.len()],
            marker.as_bytes()
        );
    }

    Ok(())
}

#[test]
fn writer_result_reports_generated_object_stream_members_and_xref_object() -> flpdf::Result<()> {
    let source = synthetic_unreferenced_object_pdf();
    let mut pdf = Pdf::open(Cursor::new(source))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_output_memory()?;
    writer.write()?;

    let output = writer.get_buffer()?;
    let catalog = writer
        .get_renumbered_obj_gen(ObjectRef::new(1, 0))?
        .expect("catalog must be emitted");
    let pages = writer
        .get_renumbered_obj_gen(ObjectRef::new(2, 0))?
        .expect("pages must be emitted");
    let xref = writer.get_written_xref_table()?;
    for member in [catalog, pages] {
        let Some(XrefEntry::Compressed { stream, index }) = xref.get(&member) else {
            panic!("{member:?} must have a type-2 xref entry");
        };
        let Some(XrefEntry::Uncompressed { offset }) = xref.get(&ObjectRef::new(*stream, 0)) else {
            panic!("type-2 container {stream} must have a type-1 xref entry");
        };
        let container = &output[*offset as usize..];
        let stream_start = container
            .windows(b"stream\n".len())
            .position(|window| window == b"stream\n")
            .expect("ObjStm must have a stream payload")
            + b"stream\n".len();
        let stream_end = container
            .windows(b"\nendstream".len())
            .position(|window| window == b"\nendstream")
            .expect("ObjStm stream must terminate");
        let first = container[..stream_start]
            .windows(b"/First ".len())
            .position(|window| window == b"/First ")
            .and_then(|start| {
                std::str::from_utf8(&container[start + b"/First ".len()..stream_start])
                    .ok()?
                    .split_whitespace()
                    .next()?
                    .parse::<usize>()
                    .ok()
            })
            .expect("ObjStm dictionary must declare /First");
        let mut decoded = Vec::new();
        ZlibDecoder::new(&container[stream_start..stream_end]).read_to_end(&mut decoded)?;
        let header = std::str::from_utf8(&decoded[..first]).expect("ObjStm header is ASCII");
        let object_numbers: Vec<u32> = header
            .split_whitespace()
            .step_by(2)
            .map(|number| number.parse().expect("ObjStm header object number"))
            .collect();
        assert_eq!(object_numbers[*index as usize], member.number);
    }
    let xref_object = xref.iter().find_map(|(object_ref, entry)| match entry {
        XrefEntry::Uncompressed { offset }
            if output[*offset as usize..]
                .starts_with(format!("{} 0 obj\n", object_ref.number).as_bytes())
                && contains_bytes(&output[*offset as usize..], b"/Type /XRef") =>
        {
            Some(*object_ref)
        }
        _ => None,
    });
    assert!(
        xref_object.is_some(),
        "result must include synthetic xref object"
    );

    Ok(())
}

#[test]
fn writer_result_reports_qdf_length_holders() -> flpdf::Result<()> {
    let mut pdf = Pdf::open(Cursor::new(synthetic_flate_contents_pdf(false)))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_qdf_mode(true);
    writer.set_output_memory()?;
    writer.write()?;

    let output = writer.get_buffer()?;
    let mapped: std::collections::BTreeSet<_> = (1..=4)
        .filter_map(|number| {
            writer
                .get_renumbered_obj_gen(ObjectRef::new(number, 0))
                .transpose()
        })
        .collect::<flpdf::Result<_>>()?;
    let xref = writer.get_written_xref_table()?;
    let has_length_holder = xref.iter().any(|(object_ref, entry)| {
        !mapped.contains(object_ref)
            && matches!(entry, XrefEntry::Uncompressed { offset }
                if output[*offset as usize..].starts_with(
                    format!("{} 0 obj\n", object_ref.number).as_bytes(),
                )
                && output[*offset as usize..]
                    .splitn(3, |byte| *byte == b'\n')
                    .nth(1)
                    .is_some_and(|body| body.iter().all(u8::is_ascii_digit)))
    });
    assert!(
        has_length_holder,
        "QDF result must include a /Length holder"
    );

    Ok(())
}

#[test]
fn set_output_file_writes_a_fresh_qpdf_checked_pdf() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);
    let dir = tempfile::tempdir()?;
    let output_path = dir.path().join("output-file.pdf");
    writer.set_output_file(&output_path)?;
    writer.write()?;

    let output = std::fs::read(&output_path)?;
    assert!(output.starts_with(b"%PDF-"));
    assert!(!output.windows(5).any(|window| window == b"/Prev"));
    let check = Command::new("qpdf")
        .arg("--check")
        .arg(&output_path)
        .output()?;
    assert!(check.status.success(), "qpdf --check failed: {check:?}");
    Ok(())
}

#[test]
fn set_output_writer_writes_to_an_owned_sink() -> flpdf::Result<()> {
    let bytes = Rc::new(RefCell::new(Vec::new()));
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_writer(SharedBytes(Rc::clone(&bytes)))?;
    writer.write()?;

    let output = bytes.borrow();
    assert!(output.starts_with(b"%PDF-"));
    assert!(!output.windows(5).any(|window| window == b"/Prev"));
    Ok(())
}

#[test]
fn set_output_pipeline_writes_and_finishes_once() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let bytes = Rc::new(RefCell::new(Vec::new()));
    let writes = Rc::new(RefCell::new(0));
    let finishes = Rc::new(RefCell::new(0));
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_pipeline(RecordingPipeline {
        bytes: Rc::clone(&bytes),
        writes: Rc::clone(&writes),
        finishes: Rc::clone(&finishes),
    })?;
    writer.write()?;

    let output = bytes.borrow();
    assert_eq!(*writes.borrow(), 1);
    assert_eq!(*finishes.borrow(), 1);
    assert!(output.starts_with(b"%PDF-"));
    assert!(!output.windows(5).any(|window| window == b"/Prev"));
    let dir = tempfile::tempdir()?;
    let output_path = dir.path().join("pipeline.pdf");
    std::fs::write(&output_path, &*output)?;
    let check = Command::new("qpdf")
        .arg("--check")
        .arg(&output_path)
        .output()?;
    assert!(check.status.success(), "qpdf --check failed: {check:?}");
    Ok(())
}

#[test]
fn pdf_version_setters_are_reflected_in_the_output_header() -> flpdf::Result<()> {
    for (setter, expected) in [("minimum", "%PDF-1.7"), ("force", "%PDF-1.5")] {
        let mut pdf = open_minimal_pdf()?;
        let mut writer = PdfWriter::new(&mut pdf);
        match setter {
            "minimum" => writer.set_minimum_pdf_version("1.7", 0),
            "force" => writer.force_pdf_version("1.5", 0),
            _ => unreachable!(),
        }
        writer.set_output_memory()?;
        writer.write()?;
        let output = writer.get_buffer()?;
        assert!(output.starts_with(expected.as_bytes()));
    }
    Ok(())
}

#[test]
fn pdf_writer_preserves_forced_pdf_extension_pair() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.force_pdf_version("1.7", 8);
    writer.set_output_memory()?;
    writer.write()?;

    let output = writer.get_buffer()?;
    assert!(output.starts_with(b"%PDF-1.7\n"));
    assert!(contains_bytes(&output, b"/BaseVersion /1.7"));
    assert!(contains_bytes(&output, b"/ExtensionLevel 8"));
    Ok(())
}

#[test]
fn pdf_writer_emits_extra_header_text_after_qpdf_header() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_extra_header_text("% flpdf-extra");
    writer.set_output_memory()?;
    writer.write()?;

    let output = writer.get_buffer()?;
    let marker = b"%\xbf\xf7\xa2\xfe\n";
    let marker_end = output
        .windows(marker.len())
        .position(|window| window == marker)
        .map(|offset| offset + marker.len())
        .expect("qpdf binary marker must be present");
    assert_eq!(&output[marker_end..][..14], b"% flpdf-extra\n");
    Ok(())
}

#[test]
fn pdf_writer_emits_pclm_header_and_a_qpdf_checked_pdf() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_pclm(true);
    writer.set_output_memory()?;
    writer.write()?;

    let output = writer.get_buffer()?;
    assert!(output.starts_with(b"%PDF-1.7\n%PCLm 1.0\n"));
    assert!(!output.starts_with(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n"));

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("pclm.pdf");
    std::fs::write(&path, output)?;
    let check = Command::new("qpdf").arg("--check").arg(&path).output()?;
    assert!(check.status.success(), "qpdf --check failed: {check:?}");
    Ok(())
}

#[test]
fn pdf_writer_pclm_uses_page_strip_fifo_and_synthetic_transforms() -> flpdf::Result<()> {
    let mut pdf = Pdf::open(Cursor::new(synthetic_pclm_image_pdf()))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_pclm(true);
    writer.set_output_memory()?;
    writer.write()?;

    let output = writer.get_buffer()?;
    assert_eq!(
        writer.get_renumbered_obj_gen(ObjectRef::new(3, 0))?,
        Some(ObjectRef::new(1, 0))
    );
    assert_eq!(
        writer.get_renumbered_obj_gen(ObjectRef::new(5, 0))?,
        Some(ObjectRef::new(2, 0))
    );
    assert_eq!(
        writer.get_renumbered_obj_gen(ObjectRef::new(6, 0))?,
        Some(ObjectRef::new(3, 0))
    );
    assert_eq!(
        writer.get_renumbered_obj_gen(ObjectRef::new(7, 0))?,
        Some(ObjectRef::new(5, 0))
    );
    assert_eq!(
        writer.get_renumbered_obj_gen(ObjectRef::new(1, 0))?,
        Some(ObjectRef::new(7, 0))
    );
    assert_eq!(
        output
            .windows(b"q /image Do Q\n".len())
            .filter(|window| *window == b"q /image Do Q\n")
            .count(),
        2
    );
    let body_end = output
        .windows(b"xref\n".len())
        .position(|window| window == b"xref\n")
        .expect("PCLm output must have an xref table");
    let headings = [
        b"1 0 obj\n".as_slice(),
        b"2 0 obj\n".as_slice(),
        b"3 0 obj\n".as_slice(),
        b"4 0 obj\n".as_slice(),
        b"5 0 obj\n".as_slice(),
        b"6 0 obj\n".as_slice(),
        b"7 0 obj\n".as_slice(),
        b"8 0 obj\n".as_slice(),
        b"9 0 obj\n".as_slice(),
    ];
    let positions: Vec<usize> = headings
        .iter()
        .map(|heading| {
            output[..body_end]
                .windows(heading.len())
                .position(|window| window == *heading)
                .expect("every PCLm output object must be present")
        })
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    Ok(())
}

#[test]
fn pdf_writer_progress_finishes_after_the_output_sink() -> flpdf::Result<()> {
    let events = Rc::new(RefCell::new(Vec::<u8>::new()));
    let mut pdf = open_minimal_pdf()?;
    let mut writer = PdfWriter::new(&mut pdf);
    let events_for_reporter = Rc::clone(&events);
    writer.register_progress_reporter(Box::new(move |percent| {
        events_for_reporter.borrow_mut().push(percent);
    }));
    writer.set_output_memory()?;
    writer.write()?;

    let events = events.borrow();
    assert!(!events.is_empty());
    assert_eq!(events.first(), Some(&0));
    assert_eq!(events.last(), Some(&100));
    assert!(events.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(events.iter().all(|percent| *percent <= 100));
    Ok(())
}

#[test]
fn pdf_writer_linearization_reports_progress_before_sink_completion() -> flpdf::Result<()> {
    let events = Rc::new(RefCell::new(Vec::<u8>::new()));
    let file = File::open("../../tests/fixtures/compat/one-page.pdf")?;
    let mut pdf = Pdf::open(BufReader::new(file))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    let events_for_reporter = Rc::clone(&events);
    writer.register_progress_reporter(Box::new(move |percent| {
        events_for_reporter.borrow_mut().push(percent);
    }));
    writer.set_output_memory()?;
    writer.write()?;

    let events = events.borrow();
    assert_eq!(events.first(), Some(&0));
    assert_eq!(events.last(), Some(&100));
    assert!(events.iter().any(|percent| (1..=99).contains(percent)));
    assert!(events.windows(2).all(|pair| pair[0] <= pair[1]));
    Ok(())
}

#[test]
fn pdf_writer_default_and_generalized_levels_preserve_runlength_source() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let expected_filter = Some(Object::Name(b"RunLengthDecode".to_vec()));
    let expected_data = vec![0x02, b'A', b'B', b'C', 0x80];

    for level in [
        None,
        Some(DecodeLevel::None),
        Some(DecodeLevel::Generalized),
    ] {
        let output = rewrite_runlength_contents(level, None)?;
        let dir = tempfile::tempdir()?;
        let output_path = dir.path().join("runlength.pdf");
        std::fs::write(&output_path, &output)?;
        let check = Command::new("qpdf")
            .arg("--check")
            .arg(&output_path)
            .output()?;
        assert!(check.status.success(), "qpdf --check failed: {check:?}");
        let (filter, data) = runlength_contents_snapshot(output);
        assert_eq!(
            filter, expected_filter,
            "RunLength filter for level {level:?}"
        );
        assert_eq!(data, expected_data, "RunLength data for level {level:?}");
    }
    Ok(())
}

#[test]
fn pdf_writer_specialized_decodes_runlength_before_default_compression() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let output = rewrite_runlength_contents(Some(DecodeLevel::Specialized), None)?;
    assert_qpdf_check(&output)?;
    let (filter, decoded) = decoded_runlength_snapshot(output)?;

    assert_eq!(filter, Some(Object::Name(b"FlateDecode".to_vec())));
    assert_eq!(decoded, b"ABC");
    Ok(())
}

#[test]
fn pdf_writer_uncompress_uses_generalized_floor_for_runlength() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let output = rewrite_runlength_contents(None, Some(StreamDataMode::Uncompress))?;
    assert_qpdf_check(&output)?;
    let (filter, data) = runlength_contents_snapshot(output);

    assert_eq!(filter, Some(Object::Name(b"RunLengthDecode".to_vec())));
    assert_eq!(data, vec![0x02, b'A', b'B', b'C', 0x80]);
    Ok(())
}

#[test]
fn pdf_writer_stream_data_setter_order_preserves_decode_level_semantics() -> flpdf::Result<()> {
    qpdf_11_9_0()?;

    for decode_first in [true, false] {
        let mut pdf = Pdf::open(Cursor::new(synthetic_runlength_contents_pdf()))?;
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_object_stream_mode(ObjectStreamMode::Disable);
        if decode_first {
            writer.set_decode_level(DecodeLevel::Specialized);
            writer.set_stream_data_mode(StreamDataMode::Uncompress);
        } else {
            writer.set_stream_data_mode(StreamDataMode::Uncompress);
            writer.set_decode_level(DecodeLevel::Specialized);
        }
        writer.set_output_memory()?;
        writer.write()?;
        let output = writer.get_buffer()?;
        assert_qpdf_check(&output)?;
        let (filter, data) = runlength_contents_snapshot(output);
        assert_eq!(filter, None, "decode_first={decode_first}");
        assert_eq!(data, b"ABC", "decode_first={decode_first}");
    }

    let mut pdf = Pdf::open(Cursor::new(synthetic_runlength_contents_pdf()))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_decode_level(DecodeLevel::Specialized);
    writer.set_stream_data_mode(StreamDataMode::Preserve);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;
    assert_qpdf_check(&output)?;
    let (filter, data) = runlength_contents_snapshot(output);
    assert_eq!(filter, Some(Object::Name(b"RunLengthDecode".to_vec())));
    assert_eq!(data, vec![0x02, b'A', b'B', b'C', 0x80]);

    let mut pdf = Pdf::open(Cursor::new(synthetic_runlength_contents_pdf()))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_stream_data_mode(StreamDataMode::Preserve);
    writer.set_decode_level(DecodeLevel::Specialized);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;
    assert_qpdf_check(&output)?;
    let (filter, data) = runlength_contents_snapshot(output);
    assert_eq!(filter, None);
    assert_eq!(data, b"ABC");

    Ok(())
}

#[test]
fn pdf_writer_none_level_still_recompresses_generalized_filter_when_compressing(
) -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let source = synthetic_flate_contents_pdf(false);
    let (source_filter, source_data) = runlength_contents_snapshot(source.clone());
    assert_eq!(source_filter, Some(Object::Name(b"FlateDecode".to_vec())));

    let mut pdf = Pdf::open(Cursor::new(source))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_decode_level(DecodeLevel::None);
    writer.set_compress_streams(true);
    writer.set_recompress_flate(true);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;
    assert_qpdf_check(&output)?;

    let (filter, decoded) = decoded_runlength_snapshot(output.clone())?;
    let (_, output_data) = runlength_contents_snapshot(output);
    assert_eq!(filter, Some(Object::Name(b"FlateDecode".to_vec())));
    assert_eq!(decoded, b"ABC");
    assert_ne!(
        output_data, source_data,
        "compress=true must filter generalized streams even at decode level none"
    );
    Ok(())
}

#[test]
fn pdf_writer_set_compress_streams_false_preserves_generalized_flate_source() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let source = synthetic_flate_contents_pdf(false);
    let (source_filter, source_data) = runlength_contents_snapshot(source.clone());

    let mut pdf = Pdf::open(Cursor::new(source))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_compress_streams(false);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;
    assert_qpdf_check(&output)?;

    let (filter, data) = runlength_contents_snapshot(output);
    assert_eq!(filter, source_filter);
    assert_eq!(data, source_data);
    Ok(())
}

#[test]
fn pdf_writer_qdf_defaults_decode_generalized_streams_without_compression() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = Pdf::open(Cursor::new(synthetic_flate_contents_pdf_with_payload(
        false, b"A\rB",
    )))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_qdf_mode(true);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;
    assert_qpdf_check(&output)?;

    let (filter, data) = runlength_contents_snapshot(output);
    assert_eq!(filter, None);
    assert_eq!(data, b"A\nB");
    Ok(())
}

#[test]
fn pdf_writer_qdf_normalizes_only_page_contents() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = Pdf::open(Cursor::new(synthetic_flate_contents_pdf_with_payload(
        false, b"A\rB",
    )))?;

    let metadata_ref = ObjectRef::new(5, 0);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::none());
    encoder.write_all(b"M\rN")?;
    let metadata_data = encoder.finish()?;
    let mut metadata_dict = Dictionary::new();
    metadata_dict.insert("Type", Object::Name(b"Metadata".to_vec()));
    metadata_dict.insert("Subtype", Object::Name(b"XML".to_vec()));
    metadata_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    metadata_dict.insert(
        "Length",
        Object::Integer(i64::try_from(metadata_data.len()).expect("small fixture")),
    );
    pdf.set_object(
        metadata_ref,
        Object::Stream(Stream::new(metadata_dict, metadata_data)),
    );
    let root_ref = pdf.root_ref().expect("catalog reference");
    let mut catalog = pdf.resolve(root_ref)?.clone();
    catalog
        .as_dict_mut()
        .expect("catalog dictionary")
        .insert("Metadata", Object::Reference(metadata_ref));
    pdf.set_object(root_ref, catalog);

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_qdf_mode(true);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;
    assert_qpdf_check(&output)?;

    let (_, contents) = runlength_contents_snapshot(output.clone());
    assert_eq!(contents, b"A\nB");

    let mut rewritten = Pdf::open(Cursor::new(output))?;
    let root_ref = rewritten.root_ref().expect("catalog reference");
    let metadata_ref = rewritten
        .resolve(root_ref)?
        .as_dict()
        .expect("catalog dictionary")
        .get_ref("Metadata")
        .expect("metadata reference");
    let metadata = rewritten.resolve(metadata_ref)?.clone();
    let metadata = metadata.as_stream().expect("metadata stream");
    let decoded = flpdf::filters::decode_stream_data(&metadata.dict, &metadata.data)?;
    assert_eq!(decoded, b"M\rN");
    Ok(())
}

#[test]
fn pdf_writer_qdf_normalization_filters_at_explicit_none_decode_level() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = Pdf::open(Cursor::new(synthetic_flate_contents_pdf_with_payload(
        false, b"A\rB",
    )))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_qdf_mode(true);
    writer.set_decode_level(DecodeLevel::None);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;
    assert_qpdf_check(&output)?;

    let (filter, data) = runlength_contents_snapshot(output);
    assert_eq!(filter, None);
    assert_eq!(data, b"A\nB");
    Ok(())
}

#[test]
fn pdf_writer_qdf_explicit_false_suppresses_content_normalization() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = Pdf::open(Cursor::new(synthetic_flate_contents_pdf_with_payload(
        false, b"A\rB",
    )))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_qdf_mode(true);
    writer.set_content_normalization(false);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;
    assert_qpdf_check(&output)?;

    let (filter, data) = runlength_contents_snapshot(output);
    assert_eq!(filter, None);
    assert_eq!(data, b"A\rB");
    Ok(())
}

#[test]
fn pdf_writer_explicit_content_normalization_applies_without_qdf() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = Pdf::open(Cursor::new(synthetic_flate_contents_pdf_with_payload(
        false, b"A\rB",
    )))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_content_normalization(true);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;
    assert_qpdf_check(&output)?;

    let (filter, decoded) = decoded_runlength_snapshot(output)?;
    assert_eq!(filter, Some(Object::Name(b"FlateDecode".to_vec())));
    assert_eq!(decoded, b"A\nB");
    Ok(())
}

#[test]
fn pdf_writer_qdf_ignores_explicit_compression_setting() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = Pdf::open(Cursor::new(synthetic_flate_contents_pdf(false)))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_qdf_mode(true);
    writer.set_compress_streams(true);
    writer.set_recompress_flate(true);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;
    assert_qpdf_check(&output)?;

    let (filter, decoded) = decoded_runlength_snapshot(output)?;
    assert_eq!(filter, None);
    assert_eq!(decoded, b"ABC");
    Ok(())
}

#[test]
fn pdf_writer_qdf_explicit_compression_uncompresses_metadata_without_normalizing(
) -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = Pdf::open(Cursor::new(synthetic_flate_contents_pdf_with_payload(
        false, b"A\rB",
    )))?;
    attach_flate_metadata(&mut pdf, b"M\rN");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_qdf_mode(true);
    writer.set_compress_streams(true);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;
    assert_qpdf_check(&output)?;

    let (filter, data) = metadata_snapshot(output);
    assert_eq!(filter, None);
    assert_eq!(data, b"M\rN");
    Ok(())
}

#[test]
fn pdf_writer_treats_null_filter_as_unfiltered_stream() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = Pdf::open(Cursor::new(synthetic_null_filter_contents_pdf()))?;
    // The reader normalizes a parsed null dictionary value to the same lookup
    // result as an absent key. Reinsert the explicit null on the resolved
    // stream so this contract also exercises PdfWriter's filter gate with
    // `Some(Object::Null)`.
    let root_ref = pdf.root_ref().expect("catalog reference");
    let pages_ref = pdf
        .resolve(root_ref)?
        .as_dict()
        .expect("catalog dictionary")
        .get_ref("Pages")
        .expect("pages reference");
    let pages = pdf.resolve(pages_ref)?.clone();
    let page_ref = pages
        .as_dict()
        .expect("pages dictionary")
        .get("Kids")
        .and_then(Object::as_array)
        .and_then(|kids| kids.first())
        .and_then(Object::as_ref_id)
        .expect("page reference");
    let page = pdf.resolve(page_ref)?.clone();
    let contents_ref = page
        .as_dict()
        .expect("page dictionary")
        .get_ref("Contents")
        .expect("contents reference");
    let mut contents = pdf
        .resolve(contents_ref)?
        .as_stream()
        .expect("contents stream")
        .clone();
    contents.dict.insert("Filter", Object::Null);
    pdf.set_object(contents_ref, Object::Stream(contents));

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;
    assert_qpdf_check(&output)?;

    let (filter, decoded) = decoded_runlength_snapshot(output)?;
    assert_eq!(filter, Some(Object::Name(b"FlateDecode".to_vec())));
    assert_eq!(decoded, b"ABC");
    Ok(())
}

#[test]
fn public_compress_no_decodes_runlength_source() {
    let mut dict = Dictionary::new();
    dict.insert("Filter", Object::Name(b"RunLengthDecode".to_vec()));
    dict.insert("Length", Object::Integer(5));
    let source = Stream::new(dict, vec![0x02, b'A', b'B', b'C', 0x80]);

    let Object::Stream(output) = apply_stream_compress_policy(&source, CompressStreams::No) else {
        panic!("stream compression policy must return a stream");
    };

    assert_eq!(output.dict.get("Filter"), None);
    assert_eq!(output.dict.get("DecodeParms"), None);
    assert_eq!(output.dict.get("Length"), Some(&Object::Integer(3)));
    assert_eq!(output.data, b"ABC");
}

#[test]
fn public_compress_policy_treats_null_filter_as_no_filters() {
    let mut dict = Dictionary::new();
    dict.insert("Filter", Object::Null);
    dict.insert("Length", Object::Integer(3));
    let source = Stream::new(dict, b"ABC".to_vec());

    let Object::Stream(output) = apply_stream_compress_policy(&source, CompressStreams::Yes) else {
        panic!("stream compression policy must return a stream");
    };

    assert_eq!(
        output.dict.get("Filter"),
        Some(&Object::Name(b"FlateDecode".to_vec()))
    );
    let decoded = flpdf::filters::decode_stream_data(&output.dict, &output.data)
        .expect("compressed null-filter source must decode");
    assert_eq!(decoded, b"ABC");
}

#[test]
fn pdf_writer_does_not_apply_lone_flate_fast_path_to_array_form() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let source = synthetic_flate_contents_pdf(true);
    let (source_filter, source_data) = runlength_contents_snapshot(source.clone());
    assert_eq!(
        source_filter,
        Some(Object::Array(vec![Object::Name(b"FlateDecode".to_vec())]))
    );

    let mut pdf = Pdf::open(Cursor::new(source))?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;
    assert_qpdf_check(&output)?;

    let (filter, decoded) = decoded_runlength_snapshot(output.clone())?;
    let (_, output_data) = runlength_contents_snapshot(output);
    assert_eq!(filter, Some(Object::Name(b"FlateDecode".to_vec())));
    assert_eq!(decoded, b"ABC");
    assert_ne!(
        output_data, source_data,
        "qpdf's lone-Flate fast path applies only to the bare name form"
    );
    Ok(())
}

#[test]
fn pdf_writer_preserves_source_when_decode_parms_shape_is_invalid() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = Pdf::open(Cursor::new(synthetic_flate_contents_pdf(false)))?;
    let root_ref = pdf.root_ref().expect("catalog reference");
    let pages_ref = pdf
        .resolve(root_ref)?
        .as_dict()
        .expect("catalog dictionary")
        .get_ref("Pages")
        .expect("pages reference");
    let pages = pdf.resolve(pages_ref)?.clone();
    let page_ref = pages
        .as_dict()
        .expect("pages dictionary")
        .get("Kids")
        .and_then(Object::as_array)
        .and_then(|kids| kids.first())
        .and_then(Object::as_ref_id)
        .expect("page reference");
    let page = pdf.resolve(page_ref)?.clone();
    let contents_ref = page
        .as_dict()
        .expect("page dictionary")
        .get_ref("Contents")
        .expect("contents reference");
    let mut stream = pdf
        .resolve(contents_ref)?
        .as_stream()
        .expect("stream")
        .clone();
    let source_data = stream.data.clone();
    let invalid_decode_parms = Object::Array(vec![
        Object::Dictionary(Dictionary::new()),
        Object::Dictionary(Dictionary::new()),
    ]);
    stream
        .dict
        .insert("DecodeParms", invalid_decode_parms.clone());
    pdf.set_object(contents_ref, Object::Stream(stream));
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_recompress_flate(true);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;

    let mut rewritten = Pdf::open(Cursor::new(output))?;
    let root_ref = rewritten.root_ref().expect("catalog reference");
    let stream_ref = rewritten
        .resolve(root_ref)?
        .as_dict()
        .expect("catalog dictionary")
        .get_ref("Pages")
        .expect("pages reference");
    let pages = rewritten.resolve(stream_ref)?.clone();
    let page_ref = pages
        .as_dict()
        .expect("pages dictionary")
        .get("Kids")
        .and_then(Object::as_array)
        .and_then(|kids| kids.first())
        .and_then(Object::as_ref_id)
        .expect("page reference");
    let page = rewritten.resolve(page_ref)?.clone();
    let contents_ref = page
        .as_dict()
        .expect("page dictionary")
        .get_ref("Contents")
        .expect("contents reference");
    let stream = rewritten.resolve(contents_ref)?.clone();
    let stream = stream.as_stream().expect("stream");
    assert_eq!(
        stream.dict.get("Filter"),
        Some(&Object::Name(b"FlateDecode".to_vec()))
    );
    assert_eq!(stream.dict.get("DecodeParms"), Some(&invalid_decode_parms));
    assert_eq!(stream.data, source_data);
    Ok(())
}

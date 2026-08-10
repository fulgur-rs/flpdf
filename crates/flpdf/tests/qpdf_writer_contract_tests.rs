use std::cell::RefCell;
use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Write};
use std::process::Command;
use std::rc::Rc;

use flate2::write::ZlibEncoder;
use flate2::Compression;
use flpdf::pipeline::{Pipeline, PipelineResult};
use flpdf::{
    write_pdf, DecodeLevel, Dictionary, Object, ObjectRef, ObjectStreamMode, Pdf, QPDFWriter,
    StreamDataMode,
};

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
    // Use a stored zlib block so recompression with the writer's default
    // compression has an observable raw-byte effect.
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::none());
    encoder
        .write_all(b"ABC")
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
    let mut writer = QPDFWriter::new(&mut pdf);
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
    let mut writer = QPDFWriter::new(&mut pdf);
    assert!(writer.write().is_err());
    Ok(())
}

#[test]
fn get_buffer_before_write_returns_err() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    writer.set_output_memory()?;
    assert!(writer.get_buffer().is_err());
    Ok(())
}

#[test]
fn output_configuration_can_happen_only_once() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    writer.set_output_memory()?;
    assert!(writer.set_output_memory().is_err());
    Ok(())
}

#[test]
fn write_can_happen_only_once() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    writer.set_output_memory()?;
    writer.write()?;
    assert!(writer.write().is_err());
    Ok(())
}

#[test]
fn get_buffer_from_non_memory_writer_returns_err() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    writer.set_output_writer(Cursor::new(Vec::new()))?;
    writer.write()?;
    assert!(writer.get_buffer().is_err());
    Ok(())
}

#[test]
fn get_buffer_after_first_retrieval_returns_err() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    writer.set_output_memory()?;
    writer.write()?;
    let _ = writer.get_buffer()?;
    assert!(writer.get_buffer().is_err());
    Ok(())
}

#[test]
fn qpdf_checks_qpdf_writer_memory_output() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
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
fn qpdf_writer_full_rewrite_removes_prev_from_incremental_source() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let source = std::fs::read("../../tests/fixtures/minimal.pdf")?;
    let mut incremental_pdf = Pdf::open(Cursor::new(source))?;
    let root_ref = incremental_pdf.root_ref().expect("minimal.pdf has /Root");
    let root = incremental_pdf.resolve(root_ref)?;
    incremental_pdf.set_object(root_ref, root);
    let mut incremental = Vec::new();
    write_pdf(&mut incremental_pdf, &mut incremental)?;
    assert!(incremental.windows(5).any(|window| window == b"/Prev"));

    let mut rewritten_pdf = Pdf::open(Cursor::new(incremental))?;
    let mut writer = QPDFWriter::new(&mut rewritten_pdf);
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
fn qpdf_writer_preserves_unreferenced_objects_only_when_enabled() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let source = synthetic_unreferenced_object_pdf();
    let marker = b"unreferenced-marker";

    let mut default_pdf = Pdf::open(Cursor::new(source.clone()))?;
    let mut default_writer = QPDFWriter::new(&mut default_pdf);
    default_writer.set_object_stream_mode(ObjectStreamMode::Disable);
    default_writer.set_output_memory()?;
    default_writer.write()?;
    let default_output = default_writer.get_buffer()?;
    assert!(!contains_bytes(&default_output, marker));

    let mut preserved_pdf = Pdf::open(Cursor::new(source))?;
    let mut preserved_writer = QPDFWriter::new(&mut preserved_pdf);
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
    let mut writer = QPDFWriter::new(&mut pdf);
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
    let mut writer = QPDFWriter::new(&mut pdf);
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
    let mut writer = QPDFWriter::new(&mut pdf);
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
    let mut writer = QPDFWriter::new(&mut pdf);
    writer.set_output_memory()?;
    writer.write()?;

    let _ = writer.get_renumbered_obj_gen(ObjectRef::new(1, 0));
    let _ = writer.get_written_xref_table();

    Ok(())
}

#[test]
fn set_output_file_writes_a_fresh_qpdf_checked_pdf() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
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
    let mut writer = QPDFWriter::new(&mut pdf);
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
    let mut writer = QPDFWriter::new(&mut pdf);
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
        let mut writer = QPDFWriter::new(&mut pdf);
        match setter {
            "minimum" => writer.set_minimum_pdf_version("1.7", 0)?,
            "force" => writer.force_pdf_version("1.5", 0)?,
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
fn qpdf_writer_default_and_generalized_levels_preserve_runlength_source() -> flpdf::Result<()> {
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
fn qpdf_writer_specialized_decodes_runlength_before_default_compression() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let output = rewrite_runlength_contents(Some(DecodeLevel::Specialized), None)?;
    assert_qpdf_check(&output)?;
    let (filter, decoded) = decoded_runlength_snapshot(output)?;

    assert_eq!(filter, Some(Object::Name(b"FlateDecode".to_vec())));
    assert_eq!(decoded, b"ABC");
    Ok(())
}

#[test]
fn qpdf_writer_uncompress_uses_generalized_floor_for_runlength() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let output = rewrite_runlength_contents(None, Some(StreamDataMode::Uncompress))?;
    assert_qpdf_check(&output)?;
    let (filter, data) = runlength_contents_snapshot(output);

    assert_eq!(filter, Some(Object::Name(b"RunLengthDecode".to_vec())));
    assert_eq!(data, vec![0x02, b'A', b'B', b'C', 0x80]);
    Ok(())
}

#[test]
fn qpdf_writer_stream_data_setter_order_preserves_decode_level_semantics() -> flpdf::Result<()> {
    qpdf_11_9_0()?;

    for decode_first in [true, false] {
        let mut pdf = Pdf::open(Cursor::new(synthetic_runlength_contents_pdf()))?;
        let mut writer = QPDFWriter::new(&mut pdf);
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
    let mut writer = QPDFWriter::new(&mut pdf);
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
    let mut writer = QPDFWriter::new(&mut pdf);
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
fn qpdf_writer_none_level_still_recompresses_generalized_filter_when_compressing(
) -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let source = synthetic_flate_contents_pdf(false);
    let (source_filter, source_data) = runlength_contents_snapshot(source.clone());
    assert_eq!(source_filter, Some(Object::Name(b"FlateDecode".to_vec())));

    let mut pdf = Pdf::open(Cursor::new(source))?;
    let mut writer = QPDFWriter::new(&mut pdf);
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
fn qpdf_writer_does_not_apply_lone_flate_fast_path_to_array_form() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let source = synthetic_flate_contents_pdf(true);
    let (source_filter, source_data) = runlength_contents_snapshot(source.clone());
    assert_eq!(
        source_filter,
        Some(Object::Array(vec![Object::Name(b"FlateDecode".to_vec())]))
    );

    let mut pdf = Pdf::open(Cursor::new(source))?;
    let mut writer = QPDFWriter::new(&mut pdf);
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
fn qpdf_writer_preserves_source_when_decode_parms_shape_is_invalid() -> flpdf::Result<()> {
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
    let mut writer = QPDFWriter::new(&mut pdf);
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

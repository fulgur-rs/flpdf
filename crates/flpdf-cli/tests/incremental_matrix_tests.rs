//! Deterministic semantic/structural gate for flpdf incremental output.
//!
//! qpdf 11.9.0 writes a fresh file and therefore cannot provide an
//! incremental byte stream.  This matrix uses qpdf only as the final-document
//! oracle and checks the appended revision through flpdf's public reader API.

use assert_cmd::Command as CargoCommand;
use flpdf::{
    load_xref_and_trailer, Dictionary, Object, ObjectRef, ObjectStreamMode, Pdf, WriteOptions,
    XrefEntry, XrefForm,
};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::{tempdir, TempDir};

const STATIC_ID: &[u8] = &[
    0x31, 0x41, 0x59, 0x26, 0x53, 0x58, 0x97, 0x93, 0x23, 0x84, 0x62, 0x64, 0x33, 0x83, 0x27, 0x95,
];
const COMPAT_FIXTURE_DIR: &str = "../../tests/fixtures/compat";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatrixStatus {
    Supported,
    Warning,
    Excluded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MatrixRow {
    name: &'static str,
    status: MatrixStatus,
    qpdf_byte_gate: bool,
}

const MATRIX: &[MatrixRow] = &[
    MatrixRow {
        name: "classic-xref-touched",
        status: MatrixStatus::Supported,
        qpdf_byte_gate: false,
    },
    MatrixRow {
        name: "xref-stream-touched",
        status: MatrixStatus::Supported,
        qpdf_byte_gate: false,
    },
    MatrixRow {
        name: "incremental-generated-objstm",
        status: MatrixStatus::Supported,
        qpdf_byte_gate: false,
    },
    MatrixRow {
        name: "delete-free-reuse",
        status: MatrixStatus::Supported,
        qpdf_byte_gate: false,
    },
    MatrixRow {
        name: "multi-update-prev-chain",
        status: MatrixStatus::Supported,
        qpdf_byte_gate: false,
    },
    MatrixRow {
        name: "warning-exit",
        status: MatrixStatus::Warning,
        qpdf_byte_gate: false,
    },
    MatrixRow {
        name: "encrypted-source-policy",
        status: MatrixStatus::Excluded,
        qpdf_byte_gate: false,
    },
];

#[test]
fn incremental_matrix_has_explicit_supported_and_excluded_tuples() {
    let report = run_incremental_matrix();

    assert_eq!(
        report,
        vec![
            ("classic-xref-touched", MatrixStatus::Supported),
            ("xref-stream-touched", MatrixStatus::Supported),
            ("incremental-generated-objstm", MatrixStatus::Supported),
            ("delete-free-reuse", MatrixStatus::Supported),
            ("multi-update-prev-chain", MatrixStatus::Supported),
            ("warning-exit", MatrixStatus::Warning),
            ("encrypted-source-policy", MatrixStatus::Excluded),
        ]
    );

    assert!(
        MATRIX.iter().all(|row| !row.qpdf_byte_gate),
        "incremental output must never acquire a qpdf byte-identity gate"
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedEntry {
    Uncompressed,
    Compressed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClassicEntry {
    offset: u64,
    generation: u16,
    in_use: bool,
}

fn run_incremental_matrix() -> Vec<(&'static str, MatrixStatus)> {
    if !require_qpdf() {
        return MATRIX.iter().map(|row| (row.name, row.status)).collect();
    }

    let temp = tempdir().expect("create incremental matrix tempdir");
    run_classic_xref_touched(&temp);
    run_xref_stream_touched(&temp);
    run_incremental_generated_objstm(&temp);
    run_delete_free_reuse(&temp);
    run_multi_update_prev_chain(&temp);
    run_warning_exit(&temp);

    // Encrypted-source incremental handling belongs to flpdf-9hc.29. Keeping
    // the row in the manifest makes accidentally comparing it as a supported
    // qpdf tuple a test-visible policy change.
    let excluded = MATRIX
        .iter()
        .find(|row| row.name == "encrypted-source-policy")
        .expect("encrypted-source-policy matrix row");
    assert_eq!(excluded.status, MatrixStatus::Excluded);
    assert!(!excluded.qpdf_byte_gate);

    MATRIX.iter().map(|row| (row.name, row.status)).collect()
}

fn run_classic_xref_touched(temp: &TempDir) {
    let source = build_classic_source();
    let touched = ObjectRef::new(3, 0);
    let replacement = page_dictionary(b"classic-touched");
    let output = write_incremental(source.clone(), ObjectStreamMode::Preserve, |pdf| {
        pdf.set_object(touched, replacement.clone());
    });
    let path = write_case_output(temp, "classic-xref-touched", &output);

    assert_revision_invariants(
        &source,
        &output,
        XrefForm::Table,
        touched,
        ExpectedEntry::Uncompressed,
        &replacement,
    );
    assert_appended_object_body(&source, &output, touched, &replacement);
    assert_qpdf_final_oracle(&path, 0);
}

fn run_xref_stream_touched(temp: &TempDir) {
    let source = build_xref_stream_source();
    let touched = ObjectRef::new(2, 0);
    let replacement = pages_dictionary(b"xref-stream-touched");
    let output = write_incremental(source.clone(), ObjectStreamMode::Preserve, |pdf| {
        pdf.set_object(touched, replacement.clone());
    });
    let path = write_case_output(temp, "xref-stream-touched", &output);

    assert_revision_invariants(
        &source,
        &output,
        XrefForm::Stream,
        touched,
        ExpectedEntry::Uncompressed,
        &replacement,
    );
    assert_appended_object_body(&source, &output, touched, &replacement);
    assert_qpdf_final_oracle(&path, 0);
}

fn run_incremental_generated_objstm(temp: &TempDir) {
    let source = build_xref_stream_source();
    let touched = ObjectRef::new(2, 0);
    let replacement = pages_dictionary(b"incremental-generated-objstm");
    let output = write_incremental(source.clone(), ObjectStreamMode::Generate, |pdf| {
        pdf.set_object(touched, replacement.clone());
    });
    let path = write_case_output(temp, "incremental-generated-objstm", &output);

    assert_revision_invariants(
        &source,
        &output,
        XrefForm::Stream,
        touched,
        ExpectedEntry::Compressed,
        &replacement,
    );
    let appended = &output[source.len()..];
    assert!(
        appended
            .windows(b"/Type /ObjStm".len())
            .any(|window| window == b"/Type /ObjStm"),
        "Generate mode must append an ObjStm container for the eligible Pages object"
    );
    assert_qpdf_final_oracle(&path, 0);
}

fn run_delete_free_reuse(temp: &TempDir) {
    let source = build_classic_source();
    let deleted = ObjectRef::new(4, 0);
    let first = write_incremental(source.clone(), ObjectStreamMode::Preserve, |pdf| {
        pdf.delete_object(deleted);
    });
    let first_path = write_case_output(temp, "delete-free-reuse-first", &first);

    assert_basic_revision_invariants(&source, &first, XrefForm::Table);
    let first_entries = latest_classic_xref_entries(&first);
    assert_eq!(
        first_entries.get(&deleted.number),
        Some(&ClassicEntry {
            offset: 0,
            generation: 1,
            in_use: false,
        }),
        "deleting a live generation must append a generation+1 free entry"
    );
    assert_qpdf_final_oracle(&first_path, 0);

    let replacement = touched_dictionary(b"reused-generation");
    let reused = write_incremental(first.clone(), ObjectStreamMode::Preserve, |pdf| {
        pdf.set_object(ObjectRef::new(deleted.number, 1), replacement.clone());
    });
    let reused_path = write_case_output(temp, "delete-free-reuse", &reused);

    assert_revision_invariants(
        &first,
        &reused,
        XrefForm::Table,
        ObjectRef::new(deleted.number, 1),
        ExpectedEntry::Uncompressed,
        &replacement,
    );
    assert_appended_object_body(
        &first,
        &reused,
        ObjectRef::new(deleted.number, 1),
        &replacement,
    );
    assert_qpdf_final_oracle(&reused_path, 0);
}

fn run_multi_update_prev_chain(temp: &TempDir) {
    let source = build_classic_source();
    let touched = ObjectRef::new(3, 0);
    let first_value = page_dictionary(b"multi-update-1");
    let first = write_incremental(source.clone(), ObjectStreamMode::Preserve, |pdf| {
        pdf.set_object(touched, first_value.clone());
    });
    let first_path = write_case_output(temp, "multi-update-prev-chain-first", &first);

    assert_revision_invariants(
        &source,
        &first,
        XrefForm::Table,
        touched,
        ExpectedEntry::Uncompressed,
        &first_value,
    );
    assert_qpdf_final_oracle(&first_path, 0);

    let second_value = page_dictionary(b"multi-update-2");
    let second = write_incremental(first.clone(), ObjectStreamMode::Preserve, |pdf| {
        pdf.set_object(touched, second_value.clone());
    });
    let second_path = write_case_output(temp, "multi-update-prev-chain", &second);

    assert_revision_invariants(
        &first,
        &second,
        XrefForm::Table,
        touched,
        ExpectedEntry::Uncompressed,
        &second_value,
    );
    assert_eq!(
        trailer_integer(&second, "Prev"),
        Some(parse_startxref(&first) as i64),
        "the second update must point to the first update's startxref"
    );
    assert_qpdf_final_oracle(&second_path, 0);
}

fn run_warning_exit(temp: &TempDir) {
    let source = fixture_path("chained-indirect-contents.pdf");
    let output = temp.path().join("warning-exit.pdf");

    let qpdf_source_check = qpdf_command(&["--check", source.to_str().unwrap()]);
    assert_eq!(
        qpdf_source_check.status.code(),
        Some(3),
        "warning matrix fixture must be qpdf warning-only"
    );

    let result = CargoCommand::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "rewrite",
            "--static-id",
            source.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .output()
        .expect("run flpdf warning rewrite");
    assert_eq!(result.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("operation succeeded with warnings"),
        "warning rewrite must report qpdf-compatible exit-3 completion"
    );
    let output_bytes = fs::read(&output).expect("warning rewrite must finish its output");
    assert!(
        !output_bytes.is_empty(),
        "warning rewrite must produce a PDF"
    );
    assert_eq!(qpdf_check_status(&output), 3);
    assert_qpdf_final_oracle(&output, 3);
}

fn write_incremental<F>(source: Vec<u8>, object_streams: ObjectStreamMode, mutate: F) -> Vec<u8>
where
    F: FnOnce(&mut Pdf<Cursor<Vec<u8>>>),
{
    let mut pdf = Pdf::open(Cursor::new(source)).expect("open incremental source");
    mutate(&mut pdf);

    let mut options = WriteOptions::default();
    options.full_rewrite = false;
    options.object_streams = object_streams;
    options.static_id = true;

    let mut output = Vec::new();
    flpdf::write_pdf_with_options(&mut pdf, &mut output, &options)
        .expect("write deterministic incremental output");
    output
}

fn assert_revision_invariants(
    source: &[u8],
    output: &[u8],
    expected_form: XrefForm,
    touched: ObjectRef,
    expected_entry: ExpectedEntry,
    replacement: &Object,
) {
    assert_basic_revision_invariants(source, output, expected_form);

    let mut reader = Cursor::new(output);
    let loaded = load_xref_and_trailer(&mut reader).expect("load output xref");
    match (expected_entry, loaded.entries.get(&touched)) {
        (ExpectedEntry::Uncompressed, Some(XrefEntry::Uncompressed { offset })) => {
            assert!(
                *offset >= source.len() as u64,
                "rewritten object offset must lie in the appended revision"
            );
        }
        (ExpectedEntry::Compressed, Some(XrefEntry::Compressed { stream, index })) => {
            assert!(
                *stream > touched.number,
                "new ObjStm must use a fresh object number"
            );
            assert_eq!(
                *index, 0,
                "the one-member generated ObjStm must use index zero"
            );
        }
        (expected, actual) => {
            panic!("object {touched:?} expected {expected:?} xref entry, got {actual:?}")
        }
    }

    let mut reopened = Pdf::open(Cursor::new(output.to_vec())).expect("open output PDF");
    assert_eq!(
        reopened.resolve(touched).expect("resolve touched object"),
        *replacement,
        "the final document must expose the mutated object"
    );
}

fn assert_basic_revision_invariants(source: &[u8], output: &[u8], expected_form: XrefForm) {
    assert!(
        output.len() > source.len(),
        "incremental output must append bytes"
    );
    assert_eq!(
        &output[..source.len()],
        source,
        "incremental output must preserve the source prefix exactly"
    );

    let mut source_reader = Cursor::new(source);
    let source_loaded = load_xref_and_trailer(&mut source_reader).expect("load source xref");
    let mut output_reader = Cursor::new(output);
    let output_loaded = load_xref_and_trailer(&mut output_reader).expect("load output xref");
    assert_eq!(output_loaded.last_xref_form, expected_form);
    assert_eq!(source_loaded.last_xref_form, expected_form);
    assert_eq!(
        trailer_integer(output, "Prev"),
        Some(source_loaded.startxref as i64),
        "each incremental trailer must point to the previous startxref"
    );

    let source_root = Pdf::open(Cursor::new(source.to_vec()))
        .expect("open source")
        .root_ref();
    let output_root = Pdf::open(Cursor::new(output.to_vec()))
        .expect("open output")
        .root_ref();
    assert_eq!(
        source_root, output_root,
        "incremental update must preserve /Root"
    );

    let source_size = as_integer(source_loaded.trailer.get("Size")).expect("source /Size");
    let output_size = as_integer(output_loaded.trailer.get("Size")).expect("output /Size");
    assert!(
        output_size >= source_size,
        "incremental /Size must not shrink ({output_size} < {source_size})"
    );

    let source_id = id_pair(&source_loaded.trailer).expect("source /ID pair");
    let output_id = id_pair(&output_loaded.trailer).expect("output /ID pair");
    assert_eq!(
        output_id.0, source_id.0,
        "incremental /ID[0] must be permanent"
    );
    assert_eq!(
        output_id.1, STATIC_ID,
        "static-id must control incremental /ID[1]"
    );
}

fn assert_appended_object_body(
    source: &[u8],
    output: &[u8],
    object_ref: ObjectRef,
    expected: &Object,
) {
    let header = format!("{} {} obj\n", object_ref.number, object_ref.generation);
    let suffix = &output[source.len()..];
    let header_offset = find_subslice(suffix, header.as_bytes())
        .map(|offset| offset + header.len())
        .unwrap_or_else(|| panic!("appended object header missing: {header:?}"));
    let body_end = find_subslice(&suffix[header_offset..], b"\nendobj\n")
        .map(|offset| header_offset + offset)
        .expect("appended object end marker");

    let mut expected_body = Vec::new();
    expected.write_pdf(&mut expected_body);
    assert_eq!(
        &suffix[header_offset..body_end],
        expected_body,
        "the appended indirect-object body must match the mutated value"
    );
}

fn assert_qpdf_final_oracle(path: &Path, expected_check_status: i32) {
    let check = qpdf_command(&["--check", path.to_str().unwrap()]);
    assert_eq!(
        check.status.code(),
        Some(expected_check_status),
        "unexpected qpdf --check status for {}: {}",
        path.display(),
        String::from_utf8_lossy(&check.stderr)
    );

    let full = path.with_extension("qpdf-full.pdf");
    let rewritten = qpdf_command(&[
        "--warning-exit-0",
        "--static-id",
        path.to_str().unwrap(),
        full.to_str().unwrap(),
    ]);
    assert!(
        rewritten.status.success(),
        "qpdf full-rewrite oracle failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&rewritten.stderr)
    );

    assert_eq!(
        qpdf_text(&["--show-npages", path.to_str().unwrap()]),
        qpdf_text(&["--show-npages", full.to_str().unwrap()]),
        "qpdf final semantic page count must survive its full rewrite"
    );
    assert_eq!(
        normalize_page_listing(&qpdf_text(&["--show-pages", path.to_str().unwrap()])),
        normalize_page_listing(&qpdf_text(&["--show-pages", full.to_str().unwrap()])),
        "qpdf final page structure must survive its full rewrite"
    );

    let actual = normalized_qpdf_json(path);
    let oracle = normalized_qpdf_json(&full);
    assert_eq!(
        actual, oracle,
        "incremental final document must match qpdf's normalized semantic/structural oracle;\
         xref metadata (/Prev, /Size, /ID), xref-only objects, and object-number spelling are\
         intentionally ignored"
    );
}

fn normalized_qpdf_json(path: &Path) -> Value {
    let output = qpdf_command(&["--warning-exit-0", "--json=2", path.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "qpdf JSON oracle failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let json: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("invalid qpdf JSON for {}: {error}", path.display()));

    let qpdf = json
        .get("qpdf")
        .and_then(Value::as_array)
        .expect("qpdf JSON top-level array");
    let mut metadata = qpdf
        .first()
        .and_then(Value::as_object)
        .cloned()
        .expect("qpdf JSON metadata object");
    // Object numbering can change when qpdf removes or regenerates an
    // incremental xref stream object during its fresh full rewrite. The
    // canonical object graph below is the structural comparison; this is
    // writer bookkeeping.
    metadata.remove("maxobjectid");

    let objects = qpdf
        .get(1)
        .and_then(Value::as_object)
        .cloned()
        .expect("qpdf JSON object table");
    let trailer = objects
        .get("trailer")
        .and_then(|value| value.get("value"))
        .and_then(Value::as_object)
        .expect("qpdf JSON trailer value");

    // The trailer's /Prev, /ID, /Size and xref-stream-only fields describe
    // the serialization revision, not the final document. The remaining
    // trailer entries (/Root, /Info, /Encrypt, or a future document-level
    // extension) seed the canonical reachable-object graph.
    let mut semantic_trailer = Map::new();
    for (key, value) in trailer {
        if !matches!(
            key.as_str(),
            "/ID"
                | "/Prev"
                | "/Size"
                | "/Index"
                | "/W"
                | "/Length"
                | "/Filter"
                | "/DecodeParms"
                | "/Type"
        ) {
            semantic_trailer.insert(key.clone(), value.clone());
        }
    }

    let mut canonicalizer = Canonicalizer {
        objects: &objects,
        references: BTreeMap::new(),
    };
    let document = canonicalizer.value(&Value::Object(semantic_trailer));

    let mut normalized = json.as_object().expect("qpdf JSON object").clone();
    // The top-level page listing contains object-number spellings. The
    // reachable graph and the explicit --show-pages assertion already cover
    // its semantics without making qpdf's renumbering a false mismatch.
    normalized.remove("pages");
    normalized.insert(
        "qpdf".to_string(),
        Value::Array(vec![
            Value::Object(metadata),
            Value::Object(Map::from_iter([("document".to_string(), document)])),
        ]),
    );
    Value::Object(normalized)
}

struct Canonicalizer<'a> {
    objects: &'a Map<String, Value>,
    references: BTreeMap<String, usize>,
}

impl Canonicalizer<'_> {
    fn value(&mut self, value: &Value) -> Value {
        match value {
            Value::String(reference) if is_reference(reference) => self.reference(reference),
            Value::Array(values) => {
                Value::Array(values.iter().map(|value| self.value(value)).collect())
            }
            Value::Object(values) => Value::Object(
                values
                    .iter()
                    .map(|(key, value)| (key.clone(), self.value(value)))
                    .collect(),
            ),
            _ => value.clone(),
        }
    }

    fn reference(&mut self, reference: &str) -> Value {
        if let Some(id) = self.references.get(reference) {
            return Value::Object(Map::from_iter([(
                "$ref".to_string(),
                Value::Number((*id as u64).into()),
            )]));
        }

        let id = self.references.len();
        self.references.insert(reference.to_string(), id);
        let object_key = format!("obj:{reference}");
        let object = self
            .objects
            .get(&object_key)
            .unwrap_or_else(|| panic!("qpdf JSON missing referenced object {object_key}"));
        let value = object
            .get("value")
            .or_else(|| object.get("stream"))
            .unwrap_or(object);
        Value::Object(Map::from_iter([
            ("$id".to_string(), Value::Number((id as u64).into())),
            ("$value".to_string(), self.value(value)),
        ]))
    }
}

fn is_reference(value: &str) -> bool {
    let mut parts = value.split_whitespace();
    let Some(number) = parts.next() else {
        return false;
    };
    let Some(generation) = parts.next() else {
        return false;
    };
    parts.next() == Some("R")
        && parts.next().is_none()
        && number.parse::<u32>().is_ok()
        && generation.parse::<u16>().is_ok()
}

fn build_classic_source() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let objects = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".as_slice(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".as_slice(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".as_slice(),
        b"4 0 obj\n<< /Original true >>\nendobj\n".as_slice(),
    ];
    let mut offsets = Vec::new();
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }
    let startxref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 5 /Root 1 0 R /ID [<00112233><44556677>] >>\n\
             startxref\n{startxref}\n%%EOF\n"
        )
        .replace("             ", "")
        .as_bytes(),
    );
    bytes
}

fn build_xref_stream_source() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let objects = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".as_slice(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".as_slice(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".as_slice(),
    ];
    let mut offsets = Vec::new();
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }

    let xref_offset = bytes.len();
    let mut entries = Vec::new();
    append_xref_stream_entry(&mut entries, 0, 0, u16::MAX);
    for offset in offsets {
        append_xref_stream_entry(&mut entries, 1, offset as u32, 0);
    }
    append_xref_stream_entry(&mut entries, 1, xref_offset as u32, 0);

    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /W [1 4 2] /Index [0 5] \
             /ID [<00112233><44556677>] /Length {} >>\nstream\n",
            entries.len()
        )
        .replace("             ", "")
        .as_bytes(),
    );
    bytes.extend_from_slice(&entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    bytes
}

fn append_xref_stream_entry(entries: &mut Vec<u8>, kind: u8, field1: u32, field2: u16) {
    entries.push(kind);
    entries.extend_from_slice(&field1.to_be_bytes());
    entries.extend_from_slice(&field2.to_be_bytes());
}

fn pages_dictionary(tag: &[u8]) -> Object {
    let mut dict = Dictionary::new();
    dict.insert("Type", Object::Name(b"Pages".to_vec()));
    dict.insert(
        "Kids",
        Object::Array(vec![Object::Reference(ObjectRef::new(3, 0))]),
    );
    dict.insert("Count", Object::Integer(1));
    dict.insert("FlpdfMatrixTag", Object::String(tag.to_vec()));
    Object::Dictionary(dict)
}

fn page_dictionary(tag: &[u8]) -> Object {
    let mut dict = Dictionary::new();
    dict.insert("Type", Object::Name(b"Page".to_vec()));
    dict.insert("Parent", Object::Reference(ObjectRef::new(2, 0)));
    dict.insert(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ]),
    );
    dict.insert("FlpdfMatrixTag", Object::String(tag.to_vec()));
    Object::Dictionary(dict)
}

fn touched_dictionary(tag: &[u8]) -> Object {
    let mut dict = Dictionary::new();
    dict.insert("FlpdfMatrixTag", Object::String(tag.to_vec()));
    Object::Dictionary(dict)
}

fn write_case_output(temp: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
    let path = temp.path().join(format!("{name}.pdf"));
    fs::write(&path, bytes).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    path
}

fn latest_classic_xref_entries(bytes: &[u8]) -> BTreeMap<u32, ClassicEntry> {
    let start = parse_startxref(bytes) as usize;
    let mut lines = bytes[start..].split(|byte| *byte == b'\n');
    assert_eq!(trim_ascii(lines.next().expect("xref marker")), b"xref");
    let mut entries = BTreeMap::new();

    loop {
        let line = trim_ascii(next_nonempty_line(&mut lines));
        if line == b"trailer" {
            break;
        }
        let fields = ascii_fields(line);
        assert_eq!(fields.len(), 2, "xref subsection header: {line:?}");
        let first: u32 = std::str::from_utf8(fields[0]).unwrap().parse().unwrap();
        let count: u32 = std::str::from_utf8(fields[1]).unwrap().parse().unwrap();
        for number in first..first + count {
            let row = trim_ascii(lines.next().expect("xref entry row"));
            let fields = ascii_fields(row);
            assert_eq!(fields.len(), 3, "xref entry row: {row:?}");
            let offset: u64 = std::str::from_utf8(fields[0]).unwrap().parse().unwrap();
            let generation: u16 = std::str::from_utf8(fields[1]).unwrap().parse().unwrap();
            entries.insert(
                number,
                ClassicEntry {
                    offset,
                    generation,
                    in_use: fields[2] == b"n",
                },
            );
        }
    }
    entries
}

fn id_pair(dict: &Dictionary) -> Option<(Vec<u8>, Vec<u8>)> {
    let Object::Array(items) = dict.get("ID")? else {
        return None;
    };
    if items.len() != 2 {
        return None;
    }
    let (Object::String(first), Object::String(second)) = (&items[0], &items[1]) else {
        return None;
    };
    Some((first.clone(), second.clone()))
}

fn trailer_integer(bytes: &[u8], key: &str) -> Option<i64> {
    let pdf = Pdf::open(Cursor::new(bytes.to_vec())).ok()?;
    match pdf.trailer().get(key) {
        Some(Object::Integer(value)) => Some(*value),
        _ => None,
    }
}

fn as_integer(object: Option<&Object>) -> Option<i64> {
    match object {
        Some(Object::Integer(value)) => Some(*value),
        _ => None,
    }
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(COMPAT_FIXTURE_DIR)
        .join(name)
}

fn require_qpdf() -> bool {
    let Some(version) = qpdf_version() else {
        if std::env::var_os("CI").is_some() && cfg!(target_os = "linux") {
            panic!("incremental matrix requires qpdf 11.9.0 on Linux CI");
        }
        eprintln!("qpdf not available; skipping incremental matrix oracle");
        return false;
    };
    assert_eq!(
        version, "11.9.0",
        "incremental matrix must use the pinned qpdf 11.9.0 oracle"
    );
    true
}

fn qpdf_version() -> Option<String> {
    let output = Command::new("qpdf").arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .next()?
        .strip_prefix("qpdf version ")
        .map(ToString::to_string)
}

fn qpdf_check_status(path: &Path) -> i32 {
    qpdf_command(&["--check", path.to_str().unwrap()])
        .status
        .code()
        .unwrap_or(-1)
}

fn qpdf_text(args: &[&str]) -> String {
    let mut full_args = vec!["--warning-exit-0"];
    full_args.extend_from_slice(args);
    let output = qpdf_command(&full_args);
    assert!(
        output.status.success(),
        "qpdf command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("qpdf text output must be UTF-8")
        .trim()
        .to_string()
}

fn qpdf_command(args: &[&str]) -> Output {
    Command::new("qpdf")
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("failed to invoke qpdf: {error}"))
}

fn normalize_page_listing(listing: &str) -> String {
    listing
        .lines()
        .map(|line| {
            if let Some(prefix) = line.strip_prefix("page ") {
                let page_number = prefix
                    .split_once(':')
                    .map(|(number, _)| number)
                    .unwrap_or(prefix);
                format!("page {page_number}:")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_startxref(bytes: &[u8]) -> u64 {
    let eof = bytes
        .windows(b"%%EOF".len())
        .rposition(|window| window == b"%%EOF")
        .unwrap_or(bytes.len());
    let search = &bytes[..eof];
    let marker = b"startxref";
    let position = search
        .windows(marker.len())
        .rposition(|window| window == marker)
        .expect("startxref marker");
    let mut cursor = position + marker.len();
    while cursor < search.len() && search[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    let start = cursor;
    while cursor < search.len() && search[cursor].is_ascii_digit() {
        cursor += 1;
    }
    assert!(start < cursor, "startxref offset");
    std::str::from_utf8(&search[start..cursor])
        .expect("startxref digits")
        .parse()
        .expect("startxref integer")
}

fn next_nonempty_line<'a, I>(lines: &mut I) -> &'a [u8]
where
    I: Iterator<Item = &'a [u8]>,
{
    lines
        .find(|line| !trim_ascii(line).is_empty())
        .expect("line")
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map(|position| position + 1)
        .unwrap_or(start);
    &bytes[start..end]
}

fn ascii_fields(line: &[u8]) -> Vec<&[u8]> {
    line.split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty())
        .collect()
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

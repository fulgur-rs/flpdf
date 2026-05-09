use assert_cmd::Command;
use flpdf::{load_xref_and_trailer, write_pdf, Object, ObjectRef, Pdf, XrefForm};
use std::collections::BTreeMap;
use std::fs;
use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;
use tempfile::tempdir;

const COMPAT_FIXTURE_DIR: &str = "../../tests/fixtures/compat";

#[test]
fn qpdf_inspect_pages_matrix_matches_golden() {
    if !is_qpdf_available() {
        return;
    }

    let expected = load_name_value_golden("inspect-npages.txt");
    for (name, expected_pages) in expected {
        let fixture = fixture_path(&name);
        let actual_pages = run_qpdf(&["--show-npages", fixture.to_str().unwrap()]);
        assert_eq!(actual_pages, expected_pages);

        let tmp = tempdir().unwrap();
        let rewritten = tmp.path().join(format!("rewrite-{name}"));

        let mut cmd = Command::cargo_bin("flpdf").unwrap();
        cmd.args([fixture.to_str().unwrap(), rewritten.to_str().unwrap()])
            .assert()
            .success();

        let rewritten_pages = run_qpdf(&["--show-npages", rewritten.to_str().unwrap()]);
        assert_eq!(rewritten_pages, expected_pages);
    }
}

#[test]
fn qpdf_pages_range_roundtrip_matches_selected_output() {
    if !is_qpdf_available() {
        return;
    }

    let tmp = tempdir().unwrap();
    let extracted = tmp.path().join("selected-page-1.pdf");
    let fixture = fixture_path("two-page.pdf");

    run_qpdf_with_args(&[
        fixture.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        "--",
        extracted.to_str().unwrap(),
    ]);

    let actual_pages = run_qpdf(&["--show-pages", extracted.to_str().unwrap()]);
    assert!(actual_pages.starts_with("page 1:"));
    assert!(actual_pages.contains("content:"));
    assert_eq!(
        run_qpdf(&["--show-npages", extracted.to_str().unwrap()]),
        "1"
    );

    let rewritten = tmp.path().join("selected-page-1-rewrite.pdf");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([extracted.to_str().unwrap(), rewritten.to_str().unwrap()])
        .assert()
        .success();
    assert_eq!(
        run_qpdf(&["--show-npages", rewritten.to_str().unwrap()]),
        "1"
    );
}

#[test]
fn qpdf_split_and_merge_matrix_roundtrip_still_parsable() {
    if !is_qpdf_available() {
        return;
    }

    let tmp = tempdir().unwrap();
    let split_dir = tmp.path().join("split");
    fs::create_dir(&split_dir).unwrap();

    let split_fixture = fixture_path("three-page.pdf");
    let split_pattern = split_dir.join("split-%d.pdf");
    run_qpdf_with_args(&[
        "--split-pages=1",
        split_fixture.to_str().unwrap(),
        split_pattern.to_str().unwrap(),
    ]);

    let expected = load_name_value_golden("split-three-page.txt");
    let mut split_outputs: Vec<PathBuf> = expected
        .keys()
        .map(|file_name| split_dir.join(file_name))
        .collect();
    split_outputs.sort();

    for path in split_outputs {
        let expected_pages = expected
            .get(path.file_name().unwrap().to_str().unwrap())
            .unwrap();
        assert_eq!(
            run_qpdf(&["--show-npages", path.to_str().unwrap()]),
            *expected_pages
        );

        let rewritten = tmp.path().join(format!(
            "rewritten-{}",
            path.file_name().unwrap().to_str().unwrap()
        ));
        let mut cmd = Command::cargo_bin("flpdf").unwrap();
        cmd.args([path.to_str().unwrap(), rewritten.to_str().unwrap()])
            .assert()
            .success();
        assert_eq!(
            run_qpdf(&["--show-npages", rewritten.to_str().unwrap()]),
            *expected_pages
        );
    }

    let merged = tmp.path().join("merged.pdf");
    run_qpdf_with_args(&[
        "--empty",
        "--pages",
        fixture_path("one-page.pdf").to_str().unwrap(),
        fixture_path("two-page.pdf").to_str().unwrap(),
        "--",
        merged.to_str().unwrap(),
    ]);

    let expected_merged = load_lines("merge-one-plus-two-pages.txt")
        .first()
        .cloned()
        .expect("golden file merge-one-plus-two-pages.txt should not be empty");
    assert_eq!(
        run_qpdf(&["--show-npages", merged.to_str().unwrap()]),
        expected_merged
    );
    let rewrite = tmp.path().join("merged-rewrite.pdf");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([merged.to_str().unwrap(), rewrite.to_str().unwrap()])
        .assert()
        .success();
    assert_eq!(
        run_qpdf(&["--show-npages", rewrite.to_str().unwrap()]),
        expected_merged
    );
}

#[test]
fn qpdf_incremental_touched_only_emission_keeps_prefix_and_pages() {
    if !is_qpdf_available() {
        return;
    }

    let tmp = tempdir().unwrap();
    let source = qpdf_with_object_streams(&tmp.path().join("touched"), "two-page.pdf");
    let source_bytes = fs::read(&source).unwrap();

    let mut pdf = Pdf::open(BufReader::new(File::open(&source).unwrap())).unwrap();
    let root = pdf.root_ref().expect("expected /Root in source");
    let original_root = pdf.resolve(root).unwrap();
    let Object::Dictionary(mut root_dict) = original_root else {
        panic!("expected root to be a dictionary");
    };
    root_dict.insert("FlpdfRegression", Object::Boolean(true));
    pdf.set_object(root, Object::Dictionary(root_dict));

    let output = tmp.path().join("touched-out.pdf");
    let mut out = File::create(&output).unwrap();
    write_pdf(&mut pdf, &mut out).unwrap();

    let rewritten = fs::read(&output).unwrap();
    assert!(rewritten.len() > source_bytes.len());
    assert_eq!(&rewritten[..source_bytes.len()], &source_bytes);

    let expected_pages = qpdf_show_npages(&source);
    assert_eq!(expected_pages, qpdf_show_npages(&output));

    qpdf_check(&source);
    qpdf_check(&output);
}

#[test]
fn qpdf_incremental_rewrite_of_xref_stream_input_preserves_structure() {
    if !is_qpdf_available() {
        return;
    }

    let tmp = tempdir().unwrap();
    let source = qpdf_with_object_streams(&tmp.path().join("xref"), "one-page.pdf");
    let source_bytes = fs::read(&source).unwrap();
    assert!(bytes_use_xref_stream(&source_bytes));

    let output = tmp.path().join("xref-output.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([source.to_str().unwrap(), output.to_str().unwrap()])
        .assert()
        .success();

    assert_eq!(qpdf_show_npages(&source), qpdf_show_npages(&output));
    qpdf_check(&output);
}

#[test]
fn qpdf_incremental_xref_stream_form_round_trips() {
    if !is_qpdf_available() {
        return;
    }

    let tmp = tempdir().unwrap();
    let source = qpdf_with_object_streams(&tmp.path().join("qpdf-xref-roundtrip"), "one-page.pdf");
    let source_bytes = fs::read(&source).unwrap();
    assert!(bytes_use_xref_stream(&source_bytes));

    let output = tmp.path().join("qpdf-xref-roundtrip-rewrite.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([source.to_str().unwrap(), output.to_str().unwrap()])
        .assert()
        .success();

    let rewritten = fs::read(&output).unwrap();
    assert!(bytes_use_xref_stream(&rewritten));
    assert_eq!(qpdf_show_npages(&source), qpdf_show_npages(&output));
    qpdf_check(&output);

    let mut cursor = Cursor::new(&rewritten);
    let loaded = load_xref_and_trailer(&mut cursor).unwrap();
    assert_eq!(loaded.last_xref_form, XrefForm::Stream);
}

#[test]
fn qpdf_incremental_prev_chain_and_page_counts_are_stable() {
    if !is_qpdf_available() {
        return;
    }

    let tmp = tempdir().unwrap();
    let base = qpdf_with_object_streams(&tmp.path().join("prev"), "three-page.pdf");
    let mut current = base.clone();
    let mut generations = vec![current.clone()];

    for index in 0..3 {
        let next = tmp.path().join(format!("prev-chain-{index}.pdf"));
        Command::cargo_bin("flpdf")
            .unwrap()
            .args([current.to_str().unwrap(), next.to_str().unwrap()])
            .assert()
            .success();

        qpdf_check(&next);
        generations.push(next.clone());
        current = next;
    }

    let expected_pages = qpdf_show_npages(&generations[0]);
    for generation in &generations {
        assert_eq!(expected_pages, qpdf_show_npages(generation));
    }

    for pair in generations.windows(2) {
        let previous = &pair[0];
        let current = &pair[1];

        let previous_startxref = parse_startxref(&fs::read(previous).unwrap());
        let pdf = Pdf::open(BufReader::new(File::open(current).unwrap())).unwrap();
        let prev = pdf
            .trailer()
            .get("Prev")
            .and_then(as_integer)
            .expect("expected /Prev in incremental generation");

        assert_eq!(prev as u64, previous_startxref);
    }
}

#[test]
fn qpdf_incremental_object_stream_member_rewrite_stays_qpdf_compatible() {
    if !is_qpdf_available() {
        return;
    }

    let tmp = tempdir().unwrap();
    let source = qpdf_with_object_streams(&tmp.path().join("objstm"), "three-page.pdf");

    let mut candidates = qpdf_compressed_object_refs(&source);
    candidates.retain(|object_ref| matches!(object_ref.number, 1..));
    assert!(
        !candidates.is_empty(),
        "expected compressed object refs in source"
    );

    let mut pdf = Pdf::open(BufReader::new(File::open(&source).unwrap())).unwrap();
    let mut touched = None;

    for candidate in candidates.drain(..) {
        let original = pdf.resolve(candidate).unwrap();
        if let Some(touched_object) = touch_object_for_regression(original) {
            pdf.set_object(candidate, touched_object);
            touched = Some(candidate);
            break;
        }
    }

    let touched = touched.expect("unable to select a compressed object to touch");
    let output = tmp.path().join("objstm-output.pdf");
    let mut out = File::create(&output).unwrap();
    write_pdf(&mut pdf, &mut out).unwrap();

    assert_eq!(qpdf_show_npages(&source), qpdf_show_npages(&output));
    qpdf_check(&output);

    let mut rewritten = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let touched_value = rewritten.resolve(touched).unwrap();
    assert_ne!(touched_value, Object::Null);

    let mut original = Pdf::open(BufReader::new(File::open(&source).unwrap())).unwrap();
    let original_value = original.resolve(touched).unwrap();
    assert_ne!(touched_value, original_value);
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(COMPAT_FIXTURE_DIR)
        .join(name)
}

fn is_qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_qpdf(args: &[&str]) -> String {
    let output = ShellCommand::new("qpdf")
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("failed to invoke qpdf: {err}"));
    assert!(
        output.status.success(),
        "qpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap_or_else(|err| panic!("qpdf output was not utf8: {err}"))
        .trim()
        .to_string()
}

fn run_qpdf_with_args(args: &[&str]) {
    let output = ShellCommand::new("qpdf")
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("failed to invoke qpdf: {err}"));
    assert!(
        output.status.success(),
        "qpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn load_lines(file_name: &str) -> Vec<String> {
    fs::read_to_string(golden_path(file_name))
        .unwrap_or_else(|err| panic!("failed to read golden file {file_name}: {err}"))
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn load_name_value_golden(file_name: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for line in load_lines(file_name) {
        let mut split = line.splitn(2, ' ');
        let name = split.next().unwrap_or_default().to_string();
        let value = split.next().unwrap_or_default().to_string();
        values.insert(name, value);
    }
    values
}

fn qpdf_check(path: &Path) {
    run_qpdf_with_args(&["--check", path.to_str().unwrap()]);
}

fn as_integer(object: &Object) -> Option<i64> {
    match object {
        Object::Integer(value) => Some(*value),
        _ => None,
    }
}

fn qpdf_show_npages(path: &Path) -> String {
    run_qpdf(&["--show-npages", path.to_str().unwrap()])
}

fn qpdf_lines_for_show_xref(path: &Path) -> Vec<String> {
    run_qpdf(&["--show-xref", path.to_str().unwrap()])
        .lines()
        .map(ToString::to_string)
        .collect()
}

fn qpdf_compressed_object_refs(path: &Path) -> Vec<ObjectRef> {
    let mut refs = Vec::new();
    for line in qpdf_lines_for_show_xref(path) {
        let Some((ref_token, rest)) = line.split_once(':') else {
            continue;
        };
        if !rest.contains("compressed; stream =") {
            continue;
        }

        let Some((number, generation)) = ref_token.split_once('/') else {
            continue;
        };

        let number = match number.trim().parse::<u32>() {
            Ok(number) => number,
            Err(_) => continue,
        };
        let generation = match generation.trim().parse::<u16>() {
            Ok(generation) => generation,
            Err(_) => continue,
        };

        refs.push(ObjectRef::new(number, generation));
    }

    refs
}

fn touch_object_for_regression(object: Object) -> Option<Object> {
    match object {
        Object::Dictionary(mut dict) => {
            dict.insert("FlpdfRegression", Object::Boolean(true));
            Some(Object::Dictionary(dict))
        }
        Object::Array(mut array) => {
            array.push(Object::Integer(1));
            Some(Object::Array(array))
        }
        Object::String(mut bytes) => {
            bytes.push(b'!');
            Some(Object::String(bytes))
        }
        Object::Stream(mut stream) => {
            stream.dict.insert("FlpdfRegression", Object::Boolean(true));
            Some(Object::Stream(stream))
        }
        Object::Integer(value) => Some(Object::Integer(value.saturating_add(1))),
        _ => None,
    }
}

fn qpdf_with_object_streams(dir: &Path, fixture_name: &str) -> PathBuf {
    let source = fixture_path(fixture_name);
    let output = dir.join(fixture_name);
    fs::create_dir_all(dir).unwrap();
    run_qpdf_with_args(&[
        "--object-streams=generate",
        source.to_str().unwrap(),
        output.to_str().unwrap(),
    ]);
    output
}

fn bytes_use_xref_stream(bytes: &[u8]) -> bool {
    let has_xref_keyword = bytes
        .windows(b"/Type /XRef".len())
        .any(|window| window == b"/Type /XRef");
    let has_ascii_xref = bytes
        .windows(b"\nxref\n".len())
        .any(|window| window == b"\nxref\n");
    has_xref_keyword && !has_ascii_xref
}

fn parse_startxref(bytes: &[u8]) -> u64 {
    let marker = b"startxref";
    let eof = bytes
        .windows(b"%%EOF".len())
        .rposition(|window| window == b"%%EOF")
        .unwrap_or(bytes.len());
    let search = &bytes[..eof];

    let Some(pos) = search
        .windows(marker.len())
        .rposition(|window| window == marker)
    else {
        panic!("missing startxref marker")
    };

    let mut cursor = pos + marker.len();
    while cursor < search.len() && search[cursor].is_ascii_whitespace() {
        cursor += 1;
    }

    let start = cursor;
    while cursor < search.len() && search[cursor].is_ascii_digit() {
        cursor += 1;
    }

    if start == cursor {
        panic!("missing startxref offset")
    }

    let value = std::str::from_utf8(&search[start..cursor]).unwrap();
    value.parse::<u64>().unwrap()
}

fn golden_path(file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/golden")
        .join(file)
}

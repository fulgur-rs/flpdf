//! Page-operation observable-behavior matrix vs qpdf 11.9.0 (flpdf-9hc.8.13).
//!
//! Truth source: `/usr/bin/qpdf` 11.9.0. Every cell runs the *same* inputs and
//! flags through both `qpdf` and the `flpdf` binary, then compares the
//! observable result (resulting page count, per-page `/Rotate`, split file
//! names, collate ordering). Equality is asserted for parity cells.
//! Intentional, documented divergences are asserted as EXPECTED (flpdf's value
//! plus a comment explaining why it differs from qpdf). Genuinely unknown
//! divergences discovered while writing this matrix are marked `#[ignore]`
//! with a descriptive reason and reported for a follow-up at the originating
//! layer (this subtask only adds tests; it does not patch lower layers).
//!
//! Matrix axes:
//!   --pages     { single range, multi-input (repeat), :odd, :even,
//!                 reverse z-1, '.' shorthand, password }
//!   --rotate    { +N delta, -N delta, no-sign assign, :range, repeated,
//!                 output-page numbering under --pages }
//!   --split-pages { 1, 2, N>=npages, leading-dot template }
//!   --collate   { default, N>1 }
//!   combinations  { pages+rotate, pages+split, pages+collate }
//!
//! qpdf observation basis is recorded inline per cell (commands were run
//! against qpdf 11.9.0 on the same fixtures during authoring; the tests
//! re-derive qpdf's answer at runtime so they cannot silently rot).

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::Command as Shell;

// NOTE: qpdf's machine-readable inspection is `--json=2`, but this crate has no
// JSON dependency and the test-only subtask must not add one. Instead we read
// qpdf's *output PDF* with flpdf's own `--show-pages` / `--show-npages` — the
// SAME reader is then applied to both tools' outputs, so any divergence is in
// the page-op transform, not in how the observable property is measured. The
// qpdf-produced files are independently structurally valid (asserted via
// `flpdf --check` is implicitly exercised by --show-pages succeeding).

const THREE_PAGE: &str = "../../tests/fixtures/compat/three-page.pdf";
const ONE_PAGE: &str = "../../tests/fixtures/compat/one-page.pdf";
const TWO_PAGE: &str = "../../tests/fixtures/compat/two-page.pdf";
const ONE_PAGE_V17: &str = "../../tests/fixtures/compat/one-page-v17.pdf";
const ENCRYPTED_R4_EMPTY_PASSWORD: &str = "../../tests/fixtures/compat/encrypted-r4-three-page.pdf";
const PRIMARY_CATALOG_METADATA: &str =
    "../../tests/fixtures/compat/catalog-otherpage-other-info-two-page.pdf";
const PRIMARY_CATALOG_NO_INFO: &str =
    "../../tests/fixtures/compat/catalog-otherpage-other-two-page.pdf";

/// Absolute path to a fixture (so a per-cell `cwd` change is unnecessary).
fn fixture_abs(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// `qpdf` binary path (the project's pinned truth source).
const QPDF: &str = "/usr/bin/qpdf";

/// The qpdf release this matrix's expected values were derived from. If the
/// installed qpdf differs, the observable behaviour may differ too, so the
/// affected cells skip rather than silently validate against a different
/// truth source.
const EXPECTED_QPDF_VERSION: &str = "11.9.0";

fn qpdf_available() -> bool {
    if !Path::new(QPDF).exists() {
        return false;
    }
    // `qpdf --version` first stdout line is exactly "qpdf version <v>".
    // Require an *exact* first-line match so a patched/suffixed build
    // (e.g. "11.9.0-ubuntu2") or an unrelated bundled-library version line
    // is not mistaken for the pinned oracle.
    match Shell::new(QPDF).arg("--version").output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.lines().next().map(str::trim)
                == Some(&format!("qpdf version {EXPECTED_QPDF_VERSION}"))
        }
        Err(_) => false,
    }
}

/// Run qpdf with `args`; return (success, stdout) — stderr is folded into the
/// failure path only.
fn run_qpdf(args: &[&str]) -> (bool, String) {
    let out = Shell::new(QPDF)
        .args(args)
        .output()
        .expect("qpdf should spawn");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// Run the flpdf binary with `args`; assert success and return stdout.
fn flpdf_ok(args: &[&str]) -> String {
    let out = Command::cargo_bin("flpdf")
        .unwrap()
        .args(args)
        .assert()
        .success();
    String::from_utf8_lossy(&out.get_output().stdout).into_owned()
}

/// Show one qpdf object from an output file. Object numbers differ between
/// tools, so the metadata tests assert structural keys and stable ID[0]
/// ownership rather than byte-identical serialization.
fn show_qpdf_object(path: &Path, object: &str) -> String {
    let selector = format!("--show-object={object}");
    let (ok, stdout) = run_qpdf(&[selector.as_str(), path.to_str().unwrap()]);
    assert!(ok, "qpdf object inspection failed for {object}: {stdout}");
    stdout
}

fn first_id_hex(trailer: &str) -> Option<&str> {
    trailer.split("/ID [ <").nth(1)?.split('>').next()
}

/// Page count read from `path` via flpdf's `--show-npages`. Applied uniformly
/// to qpdf-produced and flpdf-produced files (common reader).
fn npages_of(path: &Path) -> usize {
    let out = flpdf_ok(&["--show-npages", path.to_str().unwrap()]);
    out.trim()
        .lines()
        .next()
        .unwrap()
        .trim()
        .parse()
        .expect("npages integer")
}

fn npages_of_with_password(path: &Path, password: &str) -> usize {
    let out = flpdf_ok(&[
        &format!("--password={password}"),
        "--show-npages",
        path.to_str().unwrap(),
    ]);
    out.trim()
        .lines()
        .next()
        .unwrap()
        .trim()
        .parse()
        .expect("npages integer")
}

/// Per-page `/Rotate` values read from `path` via flpdf's `--show-pages`
/// (`  rotate: <n>` lines), in page order. Common reader for both tools'
/// outputs.
fn rotates_of(path: &Path) -> Vec<i64> {
    let out = flpdf_ok(&["--show-pages", path.to_str().unwrap()]);
    out.lines()
        .filter_map(|l| l.trim().strip_prefix("rotate: "))
        .map(|n| n.trim().parse().expect("rotate integer"))
        .collect()
}

/// Per-page `/MediaBox` values read from `path` via flpdf's `--show-pages`
/// (`  media-box: <arr>` lines), in page order. Used as a stable per-page
/// identity marker so order-sensitive ops (reverse, collate) can assert the
/// *sequence* matches qpdf, not merely the count.
fn media_boxes_of(path: &Path) -> Vec<String> {
    let out = flpdf_ok(&["--show-pages", path.to_str().unwrap()]);
    out.lines()
        .filter_map(|l| l.trim().strip_prefix("media-box: "))
        .map(|s| s.trim().to_string())
        .collect()
}

/// Return page object references in output order. The page refs are read from
/// the common flpdf inspection surface, but the objects themselves are then
/// inspected without resolving inherited attributes.
fn page_refs_of(path: &Path) -> Vec<String> {
    let out = flpdf_ok(&["--show-pages", path.to_str().unwrap()]);
    out.lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("page ")
                .and_then(|rest| rest.split_once(": "))
                .map(|(_, object)| object.trim().to_owned())
        })
        .collect()
}

/// Return whether each page dictionary owns `/MediaBox` and `/Rotate`.
///
/// qpdf's `QPDFPageObjectHelper::getAttribute` (`QPDFPageObjectHelper.cc:
/// 218-262`) deliberately resolves inherited values for page operations, so
/// `--show-pages` cannot establish where a key is stored. `--show-object`
/// prints the page dictionary itself and therefore preserves this structural
/// distinction for the matrix.
fn own_page_attributes_of(path: &Path) -> Vec<(bool, bool)> {
    page_refs_of(path)
        .into_iter()
        .map(|object| {
            let selector = format!("--show-object={object}");
            let output = flpdf_ok(&[&selector, path.to_str().unwrap()]);
            let keys = top_level_dict_keys(&output);
            (keys.contains(&"/MediaBox"), keys.contains(&"/Rotate"))
        })
        .collect()
}

/// Return the key tokens that appear directly inside the outermost `<< >>`
/// dictionary of `output` (as `--show-object` prints one object per
/// invocation), skipping any nested dictionary or array. A `/Name` token
/// nested inside a value (e.g. a `/Rotate` key inside a nested
/// `/Resources`/`/XObject` dictionary) must not be mistaken for a key the
/// outer dictionary itself owns, and a `/Name` that is itself some other
/// key's *value* (e.g. `/Foo /Rotate`) must not be mistaken for a key
/// either: PDF dictionaries strictly alternate key/value pairs, so only a
/// token in key position at depth 1 is recorded. A literal-string value
/// (`( ... )`) can contain embedded whitespace, so `split_whitespace`
/// divides it into multiple tokens; those are consumed by tracking `(`/`)`
/// balance until the string closes, not treated as one scalar token. This
/// literal-string scan runs at every nesting depth, not only depth 1: a
/// literal string nested inside an array or dictionary can itself contain a
/// `[`, `]`, `<<`, or `>>` character (split into its own whitespace token,
/// e.g. `(a ] b)` -> `"(a"`, `"]"`, `"b)"`), and that must not be
/// reinterpreted as a real composite delimiter and shift the depth count.
fn top_level_dict_keys(output: &str) -> Vec<&str> {
    let tokens: Vec<&str> = output.split_whitespace().collect();
    let mut depth = 0i32;
    let mut expect_key = false;
    let mut keys = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i].starts_with('(') {
            // A literal string. PDF syntax allows both balanced, unescaped
            // nested parens and backslash-escaped `\(`/`\)` inside one
            // (flpdf's own writer emits the latter, `write_literal_string`);
            // consume whitespace tokens (which may include embedded spaces
            // from a multi-word string, or delimiter-like characters split
            // into their own tokens) until an *unescaped* paren balance
            // returns to 0.
            let mut paren_depth = 0i32;
            let mut escaped = false;
            loop {
                for byte in tokens[i].bytes() {
                    if escaped {
                        escaped = false;
                        continue;
                    }
                    match byte {
                        b'\\' => escaped = true,
                        b'(' => paren_depth += 1,
                        b')' => paren_depth -= 1,
                        _ => {}
                    }
                }
                i += 1;
                if paren_depth <= 0 || i >= tokens.len() {
                    break;
                }
            }
            if depth == 1 {
                expect_key = true;
            }
            continue;
        }
        match tokens[i] {
            "<<" => {
                depth += 1;
                if depth == 1 {
                    expect_key = true;
                }
                i += 1;
            }
            ">>" => {
                depth -= 1;
                if depth == 1 {
                    expect_key = true;
                }
                i += 1;
            }
            "[" => {
                depth += 1;
                i += 1;
            }
            "]" => {
                depth -= 1;
                if depth == 1 {
                    expect_key = true;
                }
                i += 1;
            }
            token if depth == 1 && expect_key => {
                if token.starts_with('/') {
                    keys.push(token);
                }
                expect_key = false;
                i += 1;
            }
            _ if depth == 1 => {
                // Consuming a top-level value. `<<`/`[` composite values and
                // literal strings are handled by the arms above (expect_key
                // stays false until the value closes); a remaining scalar
                // value is either a 3-token indirect reference ("N G R") or
                // a single token (Name/Number/Boolean/hex string).
                let is_reference = i + 2 < tokens.len()
                    && tokens[i].bytes().all(|b| b.is_ascii_digit())
                    && tokens[i + 1].bytes().all(|b| b.is_ascii_digit())
                    && tokens[i + 2] == "R";
                i += if is_reference { 3 } else { 1 };
                expect_key = true;
            }
            _ => i += 1,
        }
    }
    keys
}

#[test]
fn top_level_dict_keys_ignores_nested_names_with_the_same_key_name() {
    // A page with a nested /Resources /XObject dictionary that happens to
    // contain a key literally named /Rotate must not be reported as if the
    // page dictionary itself owned /Rotate.
    let output = "<< /Resources << /XObject << /Rotate 4 0 R >> >> /Type /Page >>";
    let keys = top_level_dict_keys(output);
    assert!(
        !keys.contains(&"/Rotate"),
        "a /Rotate key nested inside /Resources/XObject must not count as an own key: {keys:?}"
    );
    assert!(keys.contains(&"/Resources"), "got {keys:?}");
    assert!(keys.contains(&"/Type"), "got {keys:?}");
}

#[test]
fn top_level_dict_keys_ignores_a_name_valued_extension_entry() {
    // A page with an unrelated top-level entry whose *value* happens to be
    // the bare Name /Rotate (e.g. /Foo /Rotate) must not be reported as if
    // the page dictionary owned a /Rotate key: PDF dictionaries strictly
    // alternate key/value, and /Rotate here is in value position.
    let output = "<< /Foo /Rotate /Type /Page >>";
    let keys = top_level_dict_keys(output);
    assert!(
        !keys.contains(&"/Rotate"),
        "a name-valued /Foo entry must not manufacture an owned /Rotate key: {keys:?}"
    );
    assert!(keys.contains(&"/Foo"), "got {keys:?}");
    assert!(keys.contains(&"/Type"), "got {keys:?}");
    assert!(
        !keys.contains(&"/Page"),
        "/Type's own value /Page must not be reported as a key either: {keys:?}"
    );
}

#[test]
fn top_level_dict_keys_consumes_a_multi_word_literal_string_value() {
    // A literal-string value containing embedded whitespace splits into
    // multiple `split_whitespace` tokens (e.g. "(two words)" -> "(two",
    // "words)"). The real key that follows it must still be recognized as
    // a key, not consumed as if it were a second value token.
    let output = "<< /Foo (two words) /MediaBox [ 0 0 612 792 ] /Type /Page >>";
    let keys = top_level_dict_keys(output);
    assert!(
        keys.contains(&"/MediaBox"),
        "a multi-word literal-string value before /MediaBox must not swallow the real key: {keys:?}"
    );
    assert!(keys.contains(&"/Foo"), "got {keys:?}");
    assert!(keys.contains(&"/Type"), "got {keys:?}");
}

#[test]
fn top_level_dict_keys_handles_a_literal_string_with_nested_parens() {
    // PDF literal strings permit balanced, unescaped nested parens; the
    // string must not be considered "closed" at the first inner `)`.
    let output = "<< /Foo (a (nested) string) /MediaBox [ 0 0 612 792 ] >>";
    let keys = top_level_dict_keys(output);
    assert!(
        keys.contains(&"/MediaBox"),
        "a nested-paren literal string must not swallow the real key: {keys:?}"
    );
}

#[test]
fn top_level_dict_keys_honors_escaped_parens_in_a_literal_string() {
    // flpdf's own writer (`write_literal_string`) emits a literal paren as
    // `\(`/`\)`. An escaped `\)` must not be mistaken for the string's
    // terminator, or the scan stops early and shifts the rest of the
    // key/value parsing, potentially dropping the real key that follows.
    let output = r"<< /Foo (a \) words) /MediaBox [ 0 0 612 792 ] >>";
    let keys = top_level_dict_keys(output);
    assert!(
        keys.contains(&"/MediaBox"),
        "an escaped close-paren inside a literal string must not swallow the real key: {keys:?}"
    );
}

#[test]
fn top_level_dict_keys_ignores_composite_delimiters_inside_a_nested_literal_string() {
    // A literal string nested inside an array (depth 2) can itself contain
    // `]` as a standalone whitespace token. Because literal-string scanning
    // used to be gated on depth == 1, that `]` was reinterpreted as the
    // array's own terminator, prematurely dropping to depth 1 and then to
    // depth 0 at the real terminator — losing track of the real /MediaBox
    // key that follows.
    let output = "<< /Foo [ (a ] b) ] /MediaBox [ 0 0 612 792 ] >>";
    let keys = top_level_dict_keys(output);
    assert!(
        keys.contains(&"/MediaBox"),
        "a `]` character inside a nested literal string must not be mistaken \
         for the array's real terminator: {keys:?}"
    );
    assert!(keys.contains(&"/Foo"), "got {keys:?}");
}

fn assert_own_page_attributes_match(qpdf_output: &Path, flpdf_output: &Path) {
    assert_eq!(
        own_page_attributes_of(flpdf_output),
        own_page_attributes_of(qpdf_output),
        "flpdf and qpdf must agree on page-dictionary own-key presence"
    );
}

/// Build one page whose effective MediaBox/Rotate values are inherited when
/// `materialize` is false and written directly on the page when it is true.
fn page_attribute_presence_pdf(materialize: bool) -> tempfile::NamedTempFile {
    use std::io::Write;

    let page_attributes = if materialize {
        " /MediaBox [0 0 612 792] /Rotate 90"
    } else {
        ""
    };
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] /Rotate 90 >>".to_owned(),
        format!("<< /Type /Page /Parent 2 0 R /Resources << >>{page_attributes} >>"),
    ];
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (number, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", number + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    let mut output = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
    output.write_all(&bytes).unwrap();
    output.flush().unwrap();
    output
}

#[test]
fn matrix_own_page_keys_do_not_use_inherited_show_pages_values() {
    let inherited = page_attribute_presence_pdf(false);
    let direct = page_attribute_presence_pdf(true);

    assert_eq!(
        media_boxes_of(inherited.path()),
        media_boxes_of(direct.path())
    );
    assert_eq!(rotates_of(inherited.path()), rotates_of(direct.path()));
    assert_eq!(
        own_page_attributes_of(inherited.path()),
        vec![(false, false)]
    );
    assert_eq!(own_page_attributes_of(direct.path()), vec![(true, true)]);
}

#[test]
fn matrix_pages_materializes_inherited_own_keys_like_qpdf() {
    // The static comparison above only checks the two source fixtures
    // directly; it never sends the inherited variant through a page
    // operation. `--pages` selection flattens the page tree
    // (`QPDFPageObjectHelper::getAttribute`, `QPDFPageObjectHelper.cc:
    // 218-262`), so both tools must materialize the inherited /MediaBox and
    // /Rotate as *own* keys on the selected output page — a regression that
    // dropped or mis-copied an inherited attribute during --pages would
    // otherwise go undetected, since every other operation cell in this
    // matrix uses fixtures whose pages already own these keys directly.
    if !qpdf_available() {
        return;
    }
    let src = page_attribute_presence_pdf(false);
    let tmp = tempfile::tempdir().unwrap();
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.path().to_str().unwrap(),
        "--pages",
        ".",
        "1",
        "--",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.path().to_str().unwrap(),
        "--pages",
        ".",
        "1",
        "--",
        f.to_str().unwrap(),
    ]);

    assert_eq!(
        own_page_attributes_of(&q),
        vec![(true, true)],
        "qpdf --pages must materialize the inherited attributes as own keys"
    );
    assert_own_page_attributes_match(&q, &f);
}

/// Write a structurally valid `n`-page PDF whose pages have *distinct*
/// MediaBox widths (page `i` → `[0 0 (i*100) 200]`). The width uniquely
/// identifies each source page, so a reordering op's output page sequence
/// can be compared element-by-element against qpdf's.
fn distinct_pages_pdf(n: usize) -> tempfile::NamedTempFile {
    use std::io::Write;
    let mut buf: Vec<u8> = b"%PDF-1.5\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    let kids: String = (0..n)
        .map(|i| format!("{} 0 R", 3 + i))
        .collect::<Vec<_>>()
        .join(" ");

    offsets.push(buf.len());
    buf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.push(buf.len());
    buf.extend_from_slice(
        format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {n} >>\nendobj\n").as_bytes(),
    );
    for i in 0..n {
        offsets.push(buf.len());
        let w = (i + 1) * 100;
        // `/Resources` is a required (inheritable) Page attribute; qpdf 12.x
        // warns ("Resources is missing or invalid") and bumps `qpdf --check`
        // to exit 3 without it, where qpdf 11.x stayed silent.
        buf.extend_from_slice(
            format!(
                "{} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} 200] \
                 /Resources << >> >>\nendobj\n",
                3 + i
            )
            .as_bytes(),
        );
    }
    let xref_pos = buf.len();
    let total = n + 3; // objs 0..=(n+2)
    buf.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
    for &off in &offsets {
        buf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    buf.extend_from_slice(
        format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n")
            .as_bytes(),
    );
    let mut f = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
    f.write_all(&buf).unwrap();
    f.flush().unwrap();
    f
}

/// Sorted list of split output basenames matching `<stem>-*.pdf` in `dir`.
fn split_outputs(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".pdf") && n.contains('-'))
        .collect();
    names.sort();
    names
}

fn assert_qpdf_cleartext_chunk(path: &Path) {
    let check = Shell::new(QPDF)
        .args(["--check", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "qpdf --check should accept {}: {}",
        path.display(),
        String::from_utf8_lossy(&check.stderr)
    );

    let encryption = Shell::new(QPDF)
        .args(["--show-encryption", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        encryption.status.success(),
        "qpdf should inspect {}: {}",
        path.display(),
        String::from_utf8_lossy(&encryption.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&encryption.stdout).trim(),
        "File is not encrypted",
        "split chunk {} should be cleartext",
        path.display()
    );
}

fn assert_qpdf_encrypted_output(path: &Path, password: &str) {
    let inspection = Shell::new(QPDF)
        .args([
            &format!("--password={password}"),
            "--show-encryption",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        inspection.status.success(),
        "qpdf should open encrypted output {} with the primary password: {}",
        path.display(),
        String::from_utf8_lossy(&inspection.stderr)
    );
    assert_ne!(
        String::from_utf8_lossy(&inspection.stdout).trim(),
        "File is not encrypted",
        "{} must remain encrypted",
        path.display()
    );
}

fn assert_qpdf_rejects_password(path: &Path, password: &str) {
    let inspection = Shell::new(QPDF)
        .args([
            &format!("--password={password}"),
            "--check",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        !inspection.status.success(),
        "qpdf must reject a wrong password for {}",
        path.display()
    );
}

// ===========================================================================
// --pages : page-selection parity
// ===========================================================================

#[test]
fn pages_single_range_matches_qpdf_count() {
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "2-3",
        "--",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "2-3",
        "--",
        f.to_str().unwrap(),
    ]);

    assert_eq!(npages_of(&q), 2, "qpdf observed: 3p --pages . 2-3 -> 2");
    assert_eq!(npages_of(&f), npages_of(&q), "flpdf must match qpdf");
}

#[test]
fn pages_odd_parity_is_position_based_like_qpdf() {
    // Documented divergence #1 (EXPECTED, but qpdf-CORRECT): `:odd` selects by
    // POSITION within the resulting set, not by page number. qpdf 11.9.0:
    // `3p --pages . 1-3:odd -> 2 pages` (positions 1,3 of [1,2,3] => pages
    // 1,3). flpdf matches qpdf here, so this is asserted as PARITY.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-3:odd",
        "--",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-3:odd",
        "--",
        f.to_str().unwrap(),
    ]);

    assert_eq!(npages_of(&q), 2);
    assert_eq!(npages_of(&f), npages_of(&q));
}

#[test]
fn pages_even_parity_is_position_based_like_qpdf() {
    // qpdf 11.9.0: `3p --pages . 1-3:even -> 1 page` (position 2 of [1,2,3]).
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-3:even",
        "--",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-3:even",
        "--",
        f.to_str().unwrap(),
    ]);

    assert_eq!(npages_of(&q), 1);
    assert_eq!(npages_of(&f), npages_of(&q));
}

#[test]
fn pages_reverse_range_matches_qpdf() {
    // `z-1` = last..first. qpdf 11.9.0: 3p -> 3 pages (reversed order).
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    // Distinct MediaBox widths (100,200,300) make page identity observable,
    // so we assert the *reversed order*, not just the count.
    let src_file = distinct_pages_pdf(3);
    let src = src_file.path();
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "z-1",
        "--",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "z-1",
        "--",
        f.to_str().unwrap(),
    ]);

    assert_eq!(npages_of(&q), 3);
    assert_eq!(npages_of(&f), npages_of(&q));
    // z-1 = last..first → widths reversed.
    let q_boxes = media_boxes_of(&q);
    assert_eq!(
        q_boxes,
        vec![
            "[ 0 0 300 200 ]".to_string(),
            "[ 0 0 200 200 ]".to_string(),
            "[ 0 0 100 200 ]".to_string(),
        ],
        "qpdf z-1 should reverse page order"
    );
    assert_eq!(
        media_boxes_of(&f),
        q_boxes,
        "flpdf z-1 page order must match qpdf"
    );
    assert_own_page_attributes_match(&q, &f);
}

#[test]
fn pages_dot_shorthand_single_page_matches_qpdf() {
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        "--",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        "--",
        f.to_str().unwrap(),
    ]);

    assert_eq!(npages_of(&q), 1);
    assert_eq!(npages_of(&f), npages_of(&q));
}

#[test]
fn pages_multi_input_same_file_repeated_matches_qpdf() {
    // `. 1 . 3` repeats the primary input → single-document case in flpdf,
    // 2 pages out. qpdf 11.9.0 produces the same count.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    // Distinct widths so `. 1 . 3` order (page1 then page3) is asserted.
    let src_file = distinct_pages_pdf(3);
    let src = src_file.path();
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        ".",
        "3",
        "--",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        ".",
        "3",
        "--",
        f.to_str().unwrap(),
    ]);

    assert_eq!(npages_of(&q), 2);
    assert_eq!(npages_of(&f), npages_of(&q));
    // Selected pages 1 then 3 → widths 100 then 300, in that order.
    let q_boxes = media_boxes_of(&q);
    assert_eq!(
        q_boxes,
        vec!["[ 0 0 100 200 ]".to_string(), "[ 0 0 300 200 ]".to_string()],
        "qpdf `. 1 . 3` should yield pages 1,3 in order"
    );
    assert_eq!(
        media_boxes_of(&f),
        q_boxes,
        "flpdf repeated-same-file selection order must match qpdf"
    );
    assert_own_page_attributes_match(&q, &f);
}

#[test]
fn pages_cross_document_merge_matches_qpdf() {
    // qpdf 11.9.0 keeps the primary document as the catalog base while
    // copying selected pages from a distinct secondary document. The page
    // widths below identify the source page and therefore check both the
    // foreign-copy route and the global selection order.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let primary_file = distinct_pages_pdf(3);
    let secondary_file = distinct_pages_pdf(2);
    let primary = primary_file.path();
    let secondary = secondary_file.path();
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    let (ok, _) = run_qpdf(&[
        primary.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        secondary.to_str().unwrap(),
        "1-2",
        "--",
        q.to_str().unwrap(),
    ]);
    assert!(ok, "qpdf is expected to accept cross-document merge");

    // The qpdf-shaped job consumer handles this ordinary multi-source command.
    flpdf_ok(&[
        primary.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        secondary.to_str().unwrap(),
        "1-2",
        "--",
        f.to_str().unwrap(),
    ]);

    assert_eq!(media_boxes_of(&f), media_boxes_of(&q));
    assert_own_page_attributes_match(&q, &f);
}

#[test]
fn pages_cross_document_collate_matches_qpdf() {
    // qpdf collates specification occurrences, not source-document groups:
    // A1,B1,A2,B2,B3 for A=2 pages and B=3 pages with --collate.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let primary_file = distinct_pages_pdf(2);
    let secondary_file = distinct_pages_pdf(3);
    let primary = primary_file.path();
    let secondary = secondary_file.path();
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    let common = [
        primary.to_str().unwrap(),
        "--pages",
        ".",
        "1-2",
        secondary.to_str().unwrap(),
        "1-3",
        "--",
    ];
    let mut q_args = common.to_vec();
    q_args.extend(["--collate", q.to_str().unwrap()]);
    let mut f_args = common.to_vec();
    f_args.extend(["--collate", f.to_str().unwrap()]);

    let (ok, _) = run_qpdf(&q_args);
    assert!(ok, "qpdf is expected to accept cross-document collate");
    flpdf_ok(&f_args);
    assert_eq!(media_boxes_of(&f), media_boxes_of(&q));
    assert_own_page_attributes_match(&q, &f);
}

// ===========================================================================
// --rotate : rotation parity and qpdf sign semantics
// ===========================================================================

#[test]
fn rotate_plus_delta_matches_qpdf() {
    // `+90` is a relative (delta) rotation in both tools. From /Rotate 0 the
    // result is 90 for every page. qpdf 11.9.0 verified.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[src.to_str().unwrap(), "--rotate=+90", q.to_str().unwrap()]);
    flpdf_ok(&[src.to_str().unwrap(), f.to_str().unwrap(), "--rotate=+90"]);

    assert_eq!(rotates_of(&q), vec![90, 90, 90]);
    assert_eq!(rotates_of(&f), rotates_of(&q));
    assert_own_page_attributes_match(&q, &f);
}

#[test]
fn rotate_minus_delta_matches_qpdf() {
    // `-90` relative → 270 from a base of 0. qpdf 11.9.0 verified.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[src.to_str().unwrap(), "--rotate=-90", q.to_str().unwrap()]);
    flpdf_ok(&[src.to_str().unwrap(), f.to_str().unwrap(), "--rotate=-90"]);

    assert_eq!(rotates_of(&q), vec![270, 270, 270]);
    assert_eq!(rotates_of(&f), rotates_of(&q));
    assert_own_page_attributes_match(&q, &f);
}

#[test]
fn rotate_plus_delta_accumulates_on_nonzero_base_like_qpdf() {
    // Two-step: base /Rotate 90 (via +90), then +90 again → 180. The relative
    // (+) form composes with the existing /Rotate identically in both tools.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let base = tmp.path().join("base90.pdf");
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    // Build the rotated base with qpdf (truth source) so both tools start
    // from byte-identical input.
    run_qpdf(&[
        src.to_str().unwrap(),
        "--rotate=+90",
        base.to_str().unwrap(),
    ]);
    assert_eq!(rotates_of(&base), vec![90, 90, 90]);

    run_qpdf(&[base.to_str().unwrap(), "--rotate=+90", q.to_str().unwrap()]);
    flpdf_ok(&[base.to_str().unwrap(), f.to_str().unwrap(), "--rotate=+90"]);

    assert_eq!(rotates_of(&q), vec![180, 180, 180]);
    assert_eq!(rotates_of(&f), rotates_of(&q));
    assert_own_page_attributes_match(&q, &f);
}

#[test]
fn rotate_no_sign_is_absolute_like_qpdf() {
    // qpdf's unsigned angle is an absolute assignment. Both tools must keep
    // the existing /Rotate 90 at 90 when applying --rotate=90.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let base = tmp.path().join("base90.pdf");
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--rotate=+90",
        base.to_str().unwrap(),
    ]);
    assert_eq!(rotates_of(&base), vec![90, 90, 90]);

    run_qpdf(&[base.to_str().unwrap(), "--rotate=90", q.to_str().unwrap()]);
    flpdf_ok(&[base.to_str().unwrap(), f.to_str().unwrap(), "--rotate=90"]);

    assert_eq!(
        rotates_of(&q),
        vec![90, 90, 90],
        "qpdf no-sign rotate is absolute assignment"
    );
    assert_eq!(
        rotates_of(&f),
        rotates_of(&q),
        "flpdf no-sign rotate must match qpdf absolute assignment"
    );
}

#[test]
fn rotate_with_range_matches_qpdf() {
    // `+90:2` rotates only page 2. qpdf 11.9.0: [0, 90, 0].
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[src.to_str().unwrap(), "--rotate=+90:2", q.to_str().unwrap()]);
    flpdf_ok(&[src.to_str().unwrap(), f.to_str().unwrap(), "--rotate=+90:2"]);

    assert_eq!(rotates_of(&q), vec![0, 90, 0]);
    assert_eq!(rotates_of(&f), rotates_of(&q));
    assert_own_page_attributes_match(&q, &f);
}

#[test]
fn rotate_repeated_specs_apply_in_order_like_qpdf() {
    // `--rotate=+90:1 --rotate=180:3` → page1=90, page2=0, page3=180.
    // qpdf 11.9.0 verified.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--rotate=+90:1",
        "--rotate=180:3",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        f.to_str().unwrap(),
        "--rotate=+90:1",
        "--rotate=180:3",
    ]);

    // page3 uses unsigned 180 on a base of 0, so the absolute assignment is
    // also numerically equal to an additive +180 here.
    assert_eq!(rotates_of(&q), vec![90, 0, 180]);
    assert_eq!(rotates_of(&f), rotates_of(&q));
    assert_own_page_attributes_match(&q, &f);
}

// ===========================================================================
// --split-pages : chunking + filename parity / divergence
// ===========================================================================

#[test]
fn split_pages_one_filename_matches_qpdf() {
    // qpdf 11.9.0: `3p --split-pages=1` → q-1.pdf, q-2.pdf, q-3.pdf.
    // flpdf now matches: chunk_size==1 uses the single-number suffix
    // (job::page_split::chunk_output_path). Regression guard for flpdf-s5e.
    let qdir = tempfile::tempdir().unwrap();
    let fdir = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);

    // Cross-check the qpdf baseline only where qpdf is present (skipped on
    // hosts without qpdf on PATH); the flpdf assertion below always runs
    // against the expected split-output names.
    if qpdf_available() {
        run_qpdf(&[
            src.to_str().unwrap(),
            "--split-pages=1",
            qdir.path().join("o.pdf").to_str().unwrap(),
        ]);
        assert_eq!(
            split_outputs(qdir.path()),
            vec!["o-1.pdf", "o-2.pdf", "o-3.pdf"],
            "qpdf observed baseline"
        );
    }
    flpdf_ok(&[
        src.to_str().unwrap(),
        fdir.path().join("o.pdf").to_str().unwrap(),
        "--split-pages=1",
    ]);
    assert_eq!(
        split_outputs(fdir.path()),
        vec!["o-1.pdf", "o-2.pdf", "o-3.pdf"],
        "flpdf split-pages=1 naming must match qpdf"
    );
}

#[test]
fn split_pages_two_filenames_match_qpdf() {
    // qpdf 11.9.0: `3p --split-pages=2` → o-1-2.pdf, o-3-3.pdf (range form,
    // trailing single-page chunk still keeps lo-hi). flpdf matches exactly.
    let qdir = tempfile::tempdir().unwrap();
    let fdir = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);

    if qpdf_available() {
        run_qpdf(&[
            src.to_str().unwrap(),
            "--split-pages=2",
            qdir.path().join("o.pdf").to_str().unwrap(),
        ]);
        assert_eq!(
            split_outputs(qdir.path()),
            vec!["o-1-2.pdf", "o-3-3.pdf"],
            "qpdf observed baseline"
        );
    }
    flpdf_ok(&[
        src.to_str().unwrap(),
        fdir.path().join("o.pdf").to_str().unwrap(),
        "--split-pages=2",
    ]);
    assert_eq!(split_outputs(fdir.path()), vec!["o-1-2.pdf", "o-3-3.pdf"]);
}

#[test]
fn split_pages_n_ge_npages_single_file_matches_qpdf() {
    // qpdf 11.9.0: `3p --split-pages=5` → one file o-1-3.pdf. flpdf matches.
    let qdir = tempfile::tempdir().unwrap();
    let fdir = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);

    if qpdf_available() {
        run_qpdf(&[
            src.to_str().unwrap(),
            "--split-pages=5",
            qdir.path().join("o.pdf").to_str().unwrap(),
        ]);
        assert_eq!(split_outputs(qdir.path()), vec!["o-1-3.pdf"]);
    }
    flpdf_ok(&[
        src.to_str().unwrap(),
        fdir.path().join("o.pdf").to_str().unwrap(),
        "--split-pages=5",
    ]);
    assert_eq!(split_outputs(fdir.path()), vec!["o-1-3.pdf"]);
}

#[test]
fn split_pages_leading_dot_template_matches_qpdf() {
    // Documented divergence #2 (actually PARITY): leading-dot template `.pdf`
    // → empty stem, ".pdf" treated as extension → `-1-2.pdf`, `-3-3.pdf`.
    // qpdf 11.9.0 produces the same; assert exact parity.
    let qdir = tempfile::tempdir().unwrap();
    let fdir = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);

    if qpdf_available() {
        run_qpdf(&[
            src.to_str().unwrap(),
            "--split-pages=2",
            qdir.path().join(".pdf").to_str().unwrap(),
        ]);
        assert_eq!(
            split_outputs(qdir.path()),
            vec!["-1-2.pdf", "-3-3.pdf"],
            "qpdf leading-dot baseline"
        );
    }
    flpdf_ok(&[
        src.to_str().unwrap(),
        fdir.path().join(".pdf").to_str().unwrap(),
        "--split-pages=2",
    ]);
    assert_eq!(split_outputs(fdir.path()), vec!["-1-2.pdf", "-3-3.pdf"]);
}

#[test]
fn split_pages_zero_matches_qpdf_unsplit_output() {
    // qpdf stores zero in its signed split-pages field, then treats it as
    // falsy at output dispatch and writes one ordinary output file.
    if !qpdf_available() {
        return;
    }
    let qdir = tempfile::tempdir().unwrap();
    let fdir = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = qdir.path().join("q.pdf");
    let f = fdir.path().join("f.pdf");

    let (q_ok, q_stdout) = run_qpdf(&[
        src.to_str().unwrap(),
        "--split-pages=0",
        q.to_str().unwrap(),
    ]);
    assert!(q_ok, "qpdf split-pages=0 failed: {q_stdout}");
    assert!(q.is_file());
    assert!(split_outputs(qdir.path()).is_empty());

    flpdf_ok(&[
        src.to_str().unwrap(),
        "--split-pages=0",
        f.to_str().unwrap(),
    ]);
    assert!(f.is_file());
    assert!(split_outputs(fdir.path()).is_empty());
    assert_eq!(npages_of(&f), npages_of(&q));
    assert_eq!(rotates_of(&f), rotates_of(&q));
}

#[test]
fn pages_then_split_pages_zero_matches_qpdf_unsplit_output() {
    // With page selection, qpdf still applies the selection and then writes
    // one ordinary output because zero disables only the split writer branch.
    if !qpdf_available() {
        return;
    }
    let qdir = tempfile::tempdir().unwrap();
    let fdir = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = qdir.path().join("q.pdf");
    let f = fdir.path().join("f.pdf");

    let (q_ok, q_stdout) = run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-2",
        "--",
        "--split-pages=0",
        q.to_str().unwrap(),
    ]);
    assert!(q_ok, "qpdf pages+split-pages=0 failed: {q_stdout}");
    assert_eq!(npages_of(&q), 2);
    assert!(split_outputs(qdir.path()).is_empty());

    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-2",
        "--",
        "--split-pages=0",
        f.to_str().unwrap(),
    ]);
    assert_eq!(npages_of(&f), npages_of(&q));
    assert!(split_outputs(fdir.path()).is_empty());
}

#[test]
fn top_level_split_pages_verbose_preserves_earlier_reports_after_later_failure() {
    // qpdf reports each successfully written chunk before attempting the next
    // one. A later destination failure must not erase the earlier report.
    if !qpdf_available() {
        return;
    }
    let qdir = tempfile::tempdir().unwrap();
    let fdir = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    std::fs::create_dir(qdir.path().join("q-2.pdf")).unwrap();
    std::fs::create_dir(fdir.path().join("f-2.pdf")).unwrap();

    let (q_ok, q_stdout) = run_qpdf(&[
        "--verbose",
        "--split-pages=1",
        src.to_str().unwrap(),
        qdir.path().join("q.pdf").to_str().unwrap(),
    ]);
    assert!(!q_ok, "qpdf should fail on the occupied second chunk");
    assert!(
        q_stdout.contains("wrote file ") && q_stdout.contains("q-1.pdf"),
        "qpdf must report the first chunk before the later failure: {q_stdout}"
    );

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--verbose",
            "--split-pages=1",
            src.to_str().unwrap(),
            fdir.path().join("f.pdf").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(flpdf.status.code(), Some(2));
    let flpdf_stdout = String::from_utf8_lossy(&flpdf.stdout);
    assert!(
        flpdf_stdout.contains("wrote file ") && flpdf_stdout.contains("f-1.pdf"),
        "flpdf must preserve the first chunk report before the later failure: {flpdf_stdout}"
    );
    assert!(fdir.path().join("f-1.pdf").is_file());
}

#[test]
fn pages_then_split_pages_verbose_preserves_earlier_reports_after_later_failure() {
    // The --pages pipeline reaches the same canonical split job after its
    // in-memory page selection. It must retain qpdf's per-chunk report timing.
    if !qpdf_available() {
        return;
    }
    let qdir = tempfile::tempdir().unwrap();
    let fdir = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    std::fs::create_dir(qdir.path().join("q-pages-2.pdf")).unwrap();
    std::fs::create_dir(fdir.path().join("f-pages-2.pdf")).unwrap();

    let (q_ok, q_stdout) = run_qpdf(&[
        "--verbose",
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-3",
        "--",
        "--split-pages=1",
        qdir.path().join("q-pages.pdf").to_str().unwrap(),
    ]);
    assert!(!q_ok, "qpdf should fail on the occupied second chunk");
    assert!(
        q_stdout.contains("wrote file ") && q_stdout.contains("q-pages-1.pdf"),
        "qpdf must report the first pages+split chunk before the later failure: {q_stdout}"
    );

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--verbose",
            src.to_str().unwrap(),
            "--pages",
            ".",
            "1-3",
            "--",
            "--split-pages=1",
            fdir.path().join("f-pages.pdf").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(flpdf.status.code(), Some(2));
    let flpdf_stdout = String::from_utf8_lossy(&flpdf.stdout);
    assert!(
        flpdf_stdout.contains("wrote file ") && flpdf_stdout.contains("f-pages-1.pdf"),
        "flpdf must preserve the first pages+split report before the later failure: {flpdf_stdout}"
    );
    assert!(fdir.path().join("f-pages-1.pdf").is_file());
}

#[test]
fn split_pages_encrypted_primary_matches_qpdf_cleartext_chunks() {
    // qpdf 11.9.0 builds a fresh empty output document for every split chunk;
    // source-encryption preservation therefore does not carry into the
    // chunks. The intermediate handed to flpdf's split loop must likewise be
    // cleartext so the loop never reopens encrypted bytes without a password.
    let tmp = tempfile::tempdir().unwrap();
    let Some(enc) = make_encrypted_three_page(tmp.path(), "secretpw") else {
        return;
    };
    let qdir = tempfile::tempdir().unwrap();
    let fdir = tempfile::tempdir().unwrap();
    let qtemplate = qdir.path().join("o.pdf");
    let ftemplate = fdir.path().join("o.pdf");

    let (qpdf_ok, _) = run_qpdf(&[
        "--password=secretpw",
        enc.to_str().unwrap(),
        "--split-pages=1",
        qtemplate.to_str().unwrap(),
    ]);
    assert!(qpdf_ok, "qpdf encrypted split should succeed");
    assert_eq!(
        split_outputs(qdir.path()),
        vec!["o-1.pdf", "o-2.pdf", "o-3.pdf"]
    );

    for name in ["o-1.pdf", "o-2.pdf", "o-3.pdf"] {
        assert_qpdf_cleartext_chunk(&qdir.path().join(name));
    }

    let wrong_output = fdir.path().join("wrong.pdf");
    let wrong = Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            enc.to_str().unwrap(),
            "--password=wrong",
            wrong_output.to_str().unwrap(),
            "--split-pages=1",
        ])
        .output()
        .unwrap();
    assert!(!wrong.status.success(), "wrong password must fail");
    assert!(
        String::from_utf8_lossy(&wrong.stderr).contains("invalid password"),
        "wrong password should report authentication failure: {}",
        String::from_utf8_lossy(&wrong.stderr)
    );
    assert!(
        split_outputs(fdir.path()).is_empty(),
        "wrong-password split must not emit chunks"
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            enc.to_str().unwrap(),
            "--password=secretpw",
            ftemplate.to_str().unwrap(),
            "--split-pages=1",
        ])
        .assert()
        .success();

    assert_eq!(
        split_outputs(fdir.path()),
        vec!["o-1.pdf", "o-2.pdf", "o-3.pdf"]
    );
    for name in ["o-1.pdf", "o-2.pdf", "o-3.pdf"] {
        let path = fdir.path().join(name);
        assert_eq!(npages_of(&path), 1, "flpdf chunk {name} should be readable");
        assert_qpdf_cleartext_chunk(&path);
    }
}

#[test]
fn pages_then_split_encrypted_primary_matches_qpdf_cleartext_chunks() {
    // The --pages consumer reaches the same canonical split_pages boundary
    // after rebuilding the selected page tree. qpdf still creates fresh,
    // cleartext split chunks for the encrypted primary input.
    let tmp = tempfile::tempdir().unwrap();
    let Some(enc) = make_encrypted_three_page(tmp.path(), "secretpw") else {
        return;
    };
    let qdir = tempfile::tempdir().unwrap();
    let fdir = tempfile::tempdir().unwrap();
    let qtemplate = qdir.path().join("o.pdf");
    let ftemplate = fdir.path().join("o.pdf");

    let (qpdf_ok, _) = run_qpdf(&[
        enc.to_str().unwrap(),
        "--password=secretpw",
        "--pages",
        ".",
        "1-3",
        "--",
        "--split-pages=1",
        qtemplate.to_str().unwrap(),
    ]);
    assert!(qpdf_ok, "qpdf encrypted pages+split should succeed");
    assert_eq!(
        split_outputs(qdir.path()),
        vec!["o-1.pdf", "o-2.pdf", "o-3.pdf"]
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            enc.to_str().unwrap(),
            "--password=secretpw",
            "--pages",
            ".",
            "1-3",
            "--",
            "--split-pages=1",
            ftemplate.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert_eq!(
        split_outputs(fdir.path()),
        vec!["o-1.pdf", "o-2.pdf", "o-3.pdf"]
    );
    for name in ["o-1.pdf", "o-2.pdf", "o-3.pdf"] {
        let path = fdir.path().join(name);
        assert_eq!(npages_of(&path), 1, "flpdf chunk {name} should be readable");
        assert_qpdf_cleartext_chunk(&path);
    }
}

// ===========================================================================
// --collate : interleave parity
// ===========================================================================

#[test]
fn collate_default_matches_qpdf_count() {
    // `--pages . 1-2 . 3 -- --collate` interleaves the two selections. qpdf
    // 11.9.0: 3 pages out. flpdf single-document collate matches the count.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    // Distinct widths so the *interleave order* is observable, not just count.
    let src_file = distinct_pages_pdf(3);
    let src = src_file.path();
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-2",
        ".",
        "3",
        "--",
        "--collate",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-2",
        ".",
        "3",
        "--",
        "--collate",
        f.to_str().unwrap(),
    ]);

    assert_eq!(npages_of(&q), 3);
    assert_eq!(npages_of(&f), npages_of(&q));
    // qpdf is the oracle for the interleave order; flpdf must match it
    // page-for-page (collate ordering is the observable behaviour here).
    let q_boxes = media_boxes_of(&q);
    assert_eq!(q_boxes.len(), 3, "sanity: 3 collated pages");
    assert_eq!(
        media_boxes_of(&f),
        q_boxes,
        "flpdf --collate page order must match qpdf, not just the count"
    );
}

#[test]
fn collate_n_gt_1_matches_qpdf_count() {
    // `--pages . 1-3 -- --collate=2`. qpdf 11.9.0: 3 pages. flpdf matches.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src_file = distinct_pages_pdf(3);
    let src = src_file.path();
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-3",
        "--",
        "--collate=2",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-3",
        "--",
        "--collate=2",
        f.to_str().unwrap(),
    ]);

    assert_eq!(npages_of(&q), 3);
    assert_eq!(npages_of(&f), npages_of(&q));
    let q_boxes = media_boxes_of(&q);
    assert_eq!(q_boxes.len(), 3);
    assert_eq!(
        media_boxes_of(&f),
        q_boxes,
        "flpdf --collate=2 page order must match qpdf"
    );
}

#[test]
fn collate_comma_separated_values_match_qpdf_page_order() {
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let primary_file = distinct_pages_pdf(3);
    let secondary_file = distinct_pages_pdf(4);
    let primary = primary_file.path();
    let secondary = secondary_file.path();
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    let common = [
        primary.to_str().unwrap(),
        "--pages",
        ".",
        "1-3",
        secondary.to_str().unwrap(),
        "1-4",
        "--",
    ];
    let mut q_args = common.to_vec();
    q_args.extend(["--collate=2,3", q.to_str().unwrap()]);
    let mut f_args = common.to_vec();
    f_args.extend(["--collate=2,3", f.to_str().unwrap()]);

    let (q_ok, _) = run_qpdf(&q_args);
    assert!(q_ok, "qpdf must accept comma-separated collate values");
    flpdf_ok(&f_args);

    assert_eq!(media_boxes_of(&f), media_boxes_of(&q));
    assert_eq!(npages_of(&f), 7);
}

#[test]
fn collate_repeated_values_match_the_equivalent_qpdf_comma_list() {
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let primary_file = distinct_pages_pdf(3);
    let secondary_file = distinct_pages_pdf(4);
    let primary = primary_file.path();
    let secondary = secondary_file.path();
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    let common = [
        primary.to_str().unwrap(),
        "--pages",
        ".",
        "1-3",
        secondary.to_str().unwrap(),
        "1-4",
        "--",
    ];
    let mut q_args = common.to_vec();
    q_args.extend(["--collate=2", "--collate=3", q.to_str().unwrap()]);
    let mut f_args = common.to_vec();
    f_args.extend(["--collate=2", "--collate=3", f.to_str().unwrap()]);

    let (q_ok, _) = run_qpdf(&q_args);
    assert!(q_ok, "qpdf must accept repeated collate parameters");
    flpdf_ok(&f_args);

    assert_eq!(media_boxes_of(&f), media_boxes_of(&q));
}

#[test]
fn collate_zero_matches_qpdf_empty_page_result() {
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let source_file = distinct_pages_pdf(3);
    let source = source_file.path();
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");
    let common = [
        source.to_str().unwrap(),
        "--pages",
        ".",
        "1-3",
        ".",
        "1-3",
        "--",
    ];
    let mut q_args = common.to_vec();
    q_args.extend(["--collate=0", q.to_str().unwrap()]);
    let mut f_args = common.to_vec();
    f_args.extend(["--collate=0", f.to_str().unwrap()]);

    let (q_ok, _) = run_qpdf(&q_args);
    assert!(q_ok, "qpdf accepts zero collate groups");
    flpdf_ok(&f_args);

    assert_eq!(npages_of(&q), 0);
    assert_eq!(npages_of(&f), 0);
    assert_eq!(media_boxes_of(&f), media_boxes_of(&q));
}

#[test]
fn collate_zero_with_rotate_matches_qpdf_no_op() {
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let source_file = distinct_pages_pdf(3);
    let source = source_file.path();
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");
    let common = [
        source.to_str().unwrap(),
        "--pages",
        ".",
        "1-3",
        ".",
        "1-3",
        "--",
        "--collate=0",
        "--rotate=90",
    ];
    let mut q_args = common.to_vec();
    q_args.push(q.to_str().unwrap());
    let mut f_args = common.to_vec();
    f_args.push(f.to_str().unwrap());

    let (q_ok, _) = run_qpdf(&q_args);
    assert!(
        q_ok,
        "qpdf treats a zero-page --collate result as a rotate no-op"
    );
    flpdf_ok(&f_args);

    assert_eq!(npages_of(&q), 0);
    assert_eq!(npages_of(&f), 0);
}

#[test]
fn collate_zero_with_split_pages_matches_qpdf_no_op() {
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let source_file = distinct_pages_pdf(3);
    let source = source_file.path();
    let q = tmp.path().join("q-%d.pdf");
    let f = tmp.path().join("f-%d.pdf");
    let common = [
        source.to_str().unwrap(),
        "--pages",
        ".",
        "1-3",
        ".",
        "1-3",
        "--",
        "--collate=0",
        "--split-pages=1",
    ];
    let mut q_args = common.to_vec();
    q_args.push(q.to_str().unwrap());
    let mut f_args = common.to_vec();
    f_args.push(f.to_str().unwrap());

    let (q_ok, _) = run_qpdf(&q_args);
    assert!(
        q_ok,
        "qpdf treats a zero-page --collate result as a split-pages no-op"
    );
    flpdf_ok(&f_args);

    assert!(
        !tmp.path().join("q-1.pdf").exists(),
        "qpdf writes no split chunks for an empty page selection"
    );
    assert!(
        !tmp.path().join("f-1.pdf").exists(),
        "flpdf must match qpdf and write no split chunks"
    );
}

#[test]
fn collate_invalid_parameter_without_pages_is_rejected_like_qpdf() {
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let source_file = distinct_pages_pdf(1);
    let source = source_file.path();
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    let q_output = Shell::new(QPDF)
        .args([
            "--collate=2,",
            source.to_str().unwrap(),
            q.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let f_output = Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--collate=2,",
            source.to_str().unwrap(),
            f.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(q_output.status.code(), Some(2));
    assert_eq!(f_output.status.code(), Some(2));
}

#[test]
fn collate_vector_cardinality_error_matches_qpdf() {
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let source_file = distinct_pages_pdf(3);
    let source = source_file.path();
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");
    let common = [
        source.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        ".",
        "2",
        ".",
        "3",
        "--",
    ];
    let mut q_args = common.to_vec();
    q_args.extend(["--collate=2,3", q.to_str().unwrap()]);
    let mut f_args = common.to_vec();
    f_args.extend(["--collate=2,3", f.to_str().unwrap()]);

    let q_output = Shell::new(QPDF).args(q_args).output().unwrap();
    let f_output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(f_args)
        .output()
        .unwrap();

    assert_eq!(q_output.status.code(), Some(2));
    assert_eq!(f_output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&f_output.stderr).contains("one value per page specification"));
}

// ===========================================================================
// Combinations
// ===========================================================================

#[test]
fn pages_then_rotate_uses_output_page_numbering_like_qpdf() {
    // `--pages . 2-3 -- --rotate=+90:1` rotates the FIRST EXTRACTED page
    // (output numbering). qpdf 11.9.0: extracted [src2, src3] → rotates
    // [90, 0]. flpdf matches.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "2-3",
        "--",
        "--rotate=+90:1",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "2-3",
        "--",
        "--rotate=+90:1",
        f.to_str().unwrap(),
    ]);

    assert_eq!(rotates_of(&q), vec![90, 0]);
    assert_eq!(rotates_of(&f), rotates_of(&q));
    assert_own_page_attributes_match(&q, &f);
}

#[test]
fn pages_then_split_pages_combined_matches_qpdf() {
    // `--pages . 1-3 -- --split-pages=2`. qpdf 11.9.0: o-1-2.pdf, o-3-3.pdf.
    // flpdf matches (split_pages=2 keeps range form — parity).
    let qdir = tempfile::tempdir().unwrap();
    let fdir = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);

    if qpdf_available() {
        run_qpdf(&[
            src.to_str().unwrap(),
            "--pages",
            ".",
            "1-3",
            "--",
            "--split-pages=2",
            qdir.path().join("o.pdf").to_str().unwrap(),
        ]);
        assert_eq!(split_outputs(qdir.path()), vec!["o-1-2.pdf", "o-3-3.pdf"]);
    }
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-3",
        "--",
        "--split-pages=2",
        fdir.path().join("o.pdf").to_str().unwrap(),
    ]);
    assert_eq!(split_outputs(fdir.path()), vec!["o-1-2.pdf", "o-3-3.pdf"]);
}

// ===========================================================================
// --pages with passwords (encrypted-source scope boundary)
// ===========================================================================

/// Build an AES-256 encrypted copy of THREE_PAGE (user==owner password) using
/// qpdf, returning its path inside `dir`. Skips (returns None) if qpdf is
/// unavailable.
fn make_encrypted_three_page(dir: &Path, pw: &str) -> Option<PathBuf> {
    if !qpdf_available() {
        return None;
    }
    let src = fixture_abs(THREE_PAGE);
    let enc = dir.join("enc3.pdf");
    let (ok, _) = run_qpdf(&[
        "--encrypt",
        pw,
        pw,
        "256",
        "--",
        src.to_str().unwrap(),
        enc.to_str().unwrap(),
    ]);
    if ok {
        Some(enc)
    } else {
        None
    }
}

#[test]
fn pages_secondary_encrypted_input_matches_qpdf() {
    // qpdf pulls pages from an encrypted secondary (given its password) and
    // writes a decrypted output. The canonical flpdf writer follows the same
    // page-operation route and must accept the authenticated secondary.
    let tmp = tempfile::tempdir().unwrap();
    let Some(enc) = make_encrypted_three_page(tmp.path(), "secretpw") else {
        return;
    };
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    // qpdf: per-input password on the encrypted secondary → succeeds.
    let three = fixture_abs(THREE_PAGE);
    let (ok, _) = run_qpdf(&[
        three.to_str().unwrap(),
        "--pages",
        enc.to_str().unwrap(),
        "--password=secretpw",
        "1-2",
        "--",
        q.to_str().unwrap(),
    ]);
    // qpdf produces a decrypted 2-page output.
    assert!(ok || q.exists(), "qpdf is expected to accept the merge");

    // flpdf: the authenticated secondary follows the same fresh-output route.
    flpdf_ok(&[
        three.to_str().unwrap(),
        "--pages",
        enc.to_str().unwrap(),
        "--password=secretpw",
        "1-2",
        "--",
        f.to_str().unwrap(),
    ]);
    assert_eq!(npages_of(&q), npages_of(&f));
}

#[test]
fn pages_primary_password_does_not_fall_back_to_secondary() {
    // qpdf keeps the top-level password on the primary input. A secondary
    // source without a page-spec password is opened with its own empty/default
    // password attempt, not with the primary password.
    let tmp = tempfile::tempdir().unwrap();
    let Some(enc) = make_encrypted_three_page(tmp.path(), "secretpw") else {
        return;
    };
    let primary = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    let (q_ok, _) = run_qpdf(&[
        "--password=secretpw",
        primary.to_str().unwrap(),
        "--pages",
        enc.to_str().unwrap(),
        "1",
        "--",
        q.to_str().unwrap(),
    ]);
    assert!(
        !q_ok,
        "qpdf must not reuse the primary password for the secondary"
    );
    assert!(
        !q.exists(),
        "qpdf must not leave an output after authentication failure"
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--password=secretpw",
            primary.to_str().unwrap(),
            "--pages",
            enc.to_str().unwrap(),
            "1",
            "--",
            f.to_str().unwrap(),
        ])
        .assert()
        .failure();
    assert!(
        !f.exists(),
        "flpdf must not leave an output after secondary authentication failure"
    );
}

#[test]
fn pages_primary_encrypted_toplevel_password_matches_qpdf() {
    // qpdf authenticates the primary with the top-level password before
    // planning the selected pages. The same password must reach both the
    // planning and rebuild opens in flpdf.
    let tmp = tempfile::tempdir().unwrap();
    let Some(enc) = make_encrypted_three_page(tmp.path(), "secretpw") else {
        return;
    };
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    let (ok, _) = run_qpdf(&[
        enc.to_str().unwrap(),
        "--password=secretpw",
        "--pages",
        ".",
        "2-3",
        "--",
        q.to_str().unwrap(),
    ]);
    assert!(ok || q.exists(), "qpdf is expected to accept the merge");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            enc.to_str().unwrap(),
            "--password=secretpw",
            "--pages",
            ".",
            "2-3",
            "--",
            f.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert_eq!(
        npages_of_with_password(&q, "secretpw"),
        npages_of_with_password(&f, "secretpw")
    );
}

#[test]
fn pages_encrypted_primary_plaintext_secondary_preserves_primary_encryption() {
    // qpdf 11.9.0 keeps the primary document as the output/base document for
    // --pages (libqpdf/QPDFJob.cc:2360-2633). Therefore importing pages from a
    // plaintext secondary does not turn an authenticated encrypted primary
    // into a plaintext output.
    let tmp = tempfile::tempdir().unwrap();
    let Some(enc) = make_encrypted_three_page(tmp.path(), "secretpw") else {
        return;
    };
    let secondary = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    let (ok, stderr) = run_qpdf(&[
        enc.to_str().unwrap(),
        "--password=secretpw",
        "--pages",
        secondary.to_str().unwrap(),
        "1-2",
        "--",
        q.to_str().unwrap(),
    ]);
    assert!(
        ok || q.exists(),
        "qpdf primary/secondary merge failed: {stderr}"
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            enc.to_str().unwrap(),
            "--password=secretpw",
            "--pages",
            secondary.to_str().unwrap(),
            "1-2",
            "--",
            f.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert_qpdf_encrypted_output(&q, "secretpw");
    assert_qpdf_encrypted_output(&f, "secretpw");
    assert_qpdf_rejects_password(&q, "wrong");
    assert_qpdf_rejects_password(&f, "wrong");
    assert_eq!(
        npages_of_with_password(&q, "secretpw"),
        npages_of_with_password(&f, "secretpw")
    );
}

#[test]
fn pages_empty_password_encrypted_primary_preserves_primary_encryption() {
    // qpdf 11.9.0 accepts the empty user password for this R4 fixture and
    // keeps the authenticated primary as the encrypted output/base document
    // when a plaintext secondary contributes a page.
    if !qpdf_available() {
        eprintln!("qpdf {EXPECTED_QPDF_VERSION} unavailable; skipping empty-password encryption differential");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let primary = fixture_abs(ENCRYPTED_R4_EMPTY_PASSWORD);
    let secondary = fixture_abs(TWO_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    let (q_ok, q_stderr) = run_qpdf(&[
        primary.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        secondary.to_str().unwrap(),
        "1",
        "--",
        q.to_str().unwrap(),
    ]);
    assert!(q_ok || q.exists(), "qpdf page merge failed: {q_stderr}");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            primary.to_str().unwrap(),
            "--password=",
            "--pages",
            ".",
            "1",
            secondary.to_str().unwrap(),
            "1",
            "--",
            f.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert_qpdf_encrypted_output(&q, "");
    assert_qpdf_encrypted_output(&f, "");
    assert_qpdf_rejects_password(&q, "wrong");
    assert_qpdf_rejects_password(&f, "wrong");
}

#[test]
fn pages_secondary_version_floor_matches_qpdf() {
    // QPDFJob accumulates the source version from every input before it
    // configures the writer (QPDFJob.cc:1714-1715, 2847-2918). A PDF 1.7
    // secondary must therefore raise the fresh multi-source output above the
    // merged document's PDF 1.3 baseline.
    if !qpdf_available() {
        eprintln!("qpdf {EXPECTED_QPDF_VERSION} unavailable; skipping version-floor differential");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let primary = fixture_abs(THREE_PAGE);
    let secondary = fixture_abs(ONE_PAGE_V17);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    let (q_ok, stderr) = run_qpdf(&[
        primary.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        secondary.to_str().unwrap(),
        "1",
        "--",
        q.to_str().unwrap(),
    ]);
    assert!(q_ok || q.exists(), "qpdf page merge failed: {stderr}");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            primary.to_str().unwrap(),
            f.to_str().unwrap(),
            "--pages",
            ".",
            "1",
            secondary.to_str().unwrap(),
            "1",
            "--",
        ])
        .assert()
        .success();

    let q_bytes = std::fs::read(&q).unwrap();
    let f_bytes = std::fs::read(&f).unwrap();
    let expected_header = b"%PDF-1.7\n";
    assert!(q_bytes.starts_with(expected_header));
    assert!(
        f_bytes.starts_with(expected_header),
        "multi-source page output must propagate the highest source version"
    );

    let q_min = tmp.path().join("q-min.pdf");
    let f_min = tmp.path().join("f-min.pdf");
    let (q_min_ok, stderr) = run_qpdf(&[
        "--min-version=1.6",
        primary.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        secondary.to_str().unwrap(),
        "1",
        "--",
        q_min.to_str().unwrap(),
    ]);
    assert!(
        q_min_ok || q_min.exists(),
        "qpdf min-version merge failed: {stderr}"
    );
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            primary.to_str().unwrap(),
            f_min.to_str().unwrap(),
            "--min-version=1.6",
            "--pages",
            ".",
            "1",
            secondary.to_str().unwrap(),
            "1",
            "--",
        ])
        .assert()
        .success();
    assert!(std::fs::read(&q_min).unwrap().starts_with(expected_header));
    assert!(std::fs::read(&f_min).unwrap().starts_with(expected_header));

    let q_min_above = tmp.path().join("q-min-above.pdf");
    let f_min_above = tmp.path().join("f-min-above.pdf");
    let (q_min_above_ok, stderr) = run_qpdf(&[
        "--min-version=2.0",
        primary.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        secondary.to_str().unwrap(),
        "1",
        "--",
        q_min_above.to_str().unwrap(),
    ]);
    assert!(
        q_min_above_ok || q_min_above.exists(),
        "qpdf min-version-above-floor merge failed: {stderr}"
    );
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            primary.to_str().unwrap(),
            f_min_above.to_str().unwrap(),
            "--min-version=2.0",
            "--pages",
            ".",
            "1",
            secondary.to_str().unwrap(),
            "1",
            "--",
        ])
        .assert()
        .success();
    let min_above_header = b"%PDF-2.0\n";
    assert!(
        std::fs::read(&q_min_above)
            .unwrap()
            .starts_with(min_above_header),
        "qpdf --min-version above the source floor must win"
    );
    assert!(
        std::fs::read(&f_min_above)
            .unwrap()
            .starts_with(min_above_header),
        "--min-version above the source floor must override the floor, matching qpdf"
    );

    let q_force = tmp.path().join("q-force.pdf");
    let f_force = tmp.path().join("f-force.pdf");
    let (q_force_ok, stderr) = run_qpdf(&[
        "--force-version=1.4",
        primary.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        secondary.to_str().unwrap(),
        "1",
        "--",
        q_force.to_str().unwrap(),
    ]);
    assert!(
        q_force_ok || q_force.exists(),
        "qpdf force-version merge failed: {stderr}"
    );
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            primary.to_str().unwrap(),
            f_force.to_str().unwrap(),
            "--force-version=1.4",
            "--pages",
            ".",
            "1",
            secondary.to_str().unwrap(),
            "1",
            "--",
        ])
        .assert()
        .success();
    let forced_header = b"%PDF-1.4\n";
    assert!(std::fs::read(&q_force).unwrap().starts_with(forced_header));
    assert!(std::fs::read(&f_force).unwrap().starts_with(forced_header));
}

#[test]
fn pages_preserves_primary_catalog_and_trailer_metadata() {
    // QPDFJob mutates the authenticated primary in place
    // (QPDFJob.cc:2462-2472), so page selection must not replace its Catalog or
    // trailer with secondary metadata. The object numbers and generated ID[1]
    // differ after rewriting; compare stable ownership and remapped shape.
    if !qpdf_available() {
        eprintln!(
            "qpdf {EXPECTED_QPDF_VERSION} unavailable; skipping primary metadata differential"
        );
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let metadata_primary = fixture_abs(PRIMARY_CATALOG_METADATA);
    let one_page = fixture_abs(ONE_PAGE);
    let q_primary = tmp.path().join("q-primary.pdf");
    let f_primary = tmp.path().join("f-primary.pdf");

    let (q_ok, stderr) = run_qpdf(&[
        metadata_primary.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        one_page.to_str().unwrap(),
        "1",
        "--",
        q_primary.to_str().unwrap(),
    ]);
    assert!(
        q_ok || q_primary.exists(),
        "qpdf metadata merge failed: {stderr}"
    );
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            metadata_primary.to_str().unwrap(),
            f_primary.to_str().unwrap(),
            "--pages",
            ".",
            "1",
            one_page.to_str().unwrap(),
            "1",
            "--",
        ])
        .assert()
        .success();

    let q_trailer = show_qpdf_object(&q_primary, "trailer");
    let f_trailer = show_qpdf_object(&f_primary, "trailer");
    let q_catalog = show_qpdf_object(&q_primary, "1");
    let f_catalog = show_qpdf_object(&f_primary, "1");
    assert!(q_trailer.contains("/Info"));
    assert!(f_trailer.contains("/Info"));
    assert!(q_catalog.contains("/Ref2"));
    assert!(f_catalog.contains("/Ref2"));

    // A primary ID's first element is stable across a qpdf rewrite and is the
    // identity that qpdf preserves when it carries the primary document base.
    let two_page = fixture_abs(TWO_PAGE);
    let q_id = tmp.path().join("q-id.pdf");
    let f_id = tmp.path().join("f-id.pdf");
    let (q_ok, stderr) = run_qpdf(&[
        two_page.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        one_page.to_str().unwrap(),
        "1",
        "--",
        q_id.to_str().unwrap(),
    ]);
    assert!(q_ok || q_id.exists(), "qpdf ID merge failed: {stderr}");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            two_page.to_str().unwrap(),
            f_id.to_str().unwrap(),
            "--pages",
            ".",
            "1",
            one_page.to_str().unwrap(),
            "1",
            "--",
        ])
        .assert()
        .success();
    let q_id_trailer = show_qpdf_object(&q_id, "trailer");
    let f_id_trailer = show_qpdf_object(&f_id, "trailer");
    let q_id_hex = first_id_hex(&q_id_trailer);
    let f_id_hex = first_id_hex(&f_id_trailer);
    assert_eq!(q_id_hex, f_id_hex);

    // Conversely, metadata carried only by a secondary must not appear when
    // the primary has no corresponding `/Info`. This primary fixture has an
    // unrelated `/Ref2`, so the check also proves that primary Catalog keys
    // remain while the secondary `/Info` is excluded.
    let plain_primary = fixture_abs(PRIMARY_CATALOG_NO_INFO);
    let q_secondary = tmp.path().join("q-secondary.pdf");
    let f_secondary = tmp.path().join("f-secondary.pdf");
    let (q_ok, stderr) = run_qpdf(&[
        plain_primary.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        one_page.to_str().unwrap(),
        "1",
        "--",
        q_secondary.to_str().unwrap(),
    ]);
    assert!(
        q_ok || q_secondary.exists(),
        "qpdf secondary metadata merge failed: {stderr}"
    );
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            plain_primary.to_str().unwrap(),
            f_secondary.to_str().unwrap(),
            "--pages",
            ".",
            "1",
            one_page.to_str().unwrap(),
            "1",
            "--",
        ])
        .assert()
        .success();
    for path in [&q_secondary, &f_secondary] {
        let trailer = show_qpdf_object(path, "trailer");
        let catalog = show_qpdf_object(path, "1");
        assert!(!trailer.contains("/Info"));
        assert!(catalog.contains("/Ref2"));
    }
}

#[test]
fn pages_secondary_extension_level_matches_qpdf() {
    // The source extension level is part of qpdf's accumulated input version
    // and must reach the fresh output Catalog as well as the header.
    if !qpdf_available() {
        eprintln!(
            "qpdf {EXPECTED_QPDF_VERSION} unavailable; skipping extension-level differential"
        );
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let primary = fixture_abs(THREE_PAGE);
    let secondary = fixture_abs("../../tests/fixtures/compat/one-page-enc-u.pdf");
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    let (q_ok, stderr) = run_qpdf(&[
        primary.to_str().unwrap(),
        "--pages",
        secondary.to_str().unwrap(),
        "--password=u",
        "1",
        "--",
        q.to_str().unwrap(),
    ]);
    assert!(
        q_ok || q.exists(),
        "qpdf extension-level merge failed: {stderr}"
    );
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            primary.to_str().unwrap(),
            f.to_str().unwrap(),
            "--pages",
            secondary.to_str().unwrap(),
            "--password=u",
            "1",
            "--",
        ])
        .assert()
        .success();

    for path in [&q, &f] {
        let check = Shell::new(QPDF)
            .args(["--check", path.to_str().unwrap()])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&check.stdout);
        assert!(
            stdout.contains("PDF Version: 1.7 extension level 8"),
            "expected qpdf check to report the propagated extension level: {stdout}"
        );
    }
}

#[test]
fn pages_qdf_with_encrypted_primary_produces_cleartext_output() {
    // qpdf's QDF output is always cleartext regardless of source encryption
    // (single-document contract exercised by
    // cell_a_encrypted_input_is_transparently_decrypted_by_qdf). --qdf must
    // keep that contract for the multi-source --pages merge path too, even
    // though the primary's authenticated CopyEncryptionSource is carried
    // forward for the non-QDF case.
    let tmp = tempfile::tempdir().unwrap();
    let Some(enc) = make_encrypted_three_page(tmp.path(), "secretpw") else {
        return;
    };
    let secondary = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    let (ok, stderr) = run_qpdf(&[
        "--qdf",
        "--password=secretpw",
        enc.to_str().unwrap(),
        "--pages",
        secondary.to_str().unwrap(),
        "1-2",
        "--",
        q.to_str().unwrap(),
    ]);
    assert!(ok || q.exists(), "qpdf --qdf merge failed: {stderr}");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--qdf",
            "--password=secretpw",
            enc.to_str().unwrap(),
            "--pages",
            secondary.to_str().unwrap(),
            "1-2",
            "--",
            f.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert_qpdf_cleartext_chunk(&q);
    assert_qpdf_cleartext_chunk(&f);
    assert_eq!(npages_of(&q), npages_of(&f));
}

#[test]
fn pages_stream_data_uncompress_with_encrypted_primary_produces_cleartext_output() {
    // qpdf's implicit encryption preservation requires DecodeLevel::None
    // (PdfWriter::prepared_write_options's can_preserve guard,
    // writer.rs:645-652). --stream-data=uncompress/compress
    // raise the decode level above None, so the --pages merge path must
    // not carry the primary's encryption donor forward in that case
    // either, the same way it must not for --qdf.
    let tmp = tempfile::tempdir().unwrap();
    let Some(enc) = make_encrypted_three_page(tmp.path(), "secretpw") else {
        return;
    };
    let secondary = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    let (ok, stderr) = run_qpdf(&[
        "--password=secretpw",
        "--stream-data=uncompress",
        "--pages",
        secondary.to_str().unwrap(),
        "1-2",
        "--",
        enc.to_str().unwrap(),
        q.to_str().unwrap(),
    ]);
    assert!(
        ok || q.exists(),
        "qpdf --stream-data merge failed: {stderr}"
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            enc.to_str().unwrap(),
            f.to_str().unwrap(),
            "--password=secretpw",
            "--stream-data=uncompress",
            "--pages",
            secondary.to_str().unwrap(),
            "1-2",
            "--",
        ])
        .assert()
        .success();

    assert_qpdf_cleartext_chunk(&q);
    assert_qpdf_cleartext_chunk(&f);
    assert_eq!(npages_of(&q), npages_of(&f));
}

#[test]
fn pages_decode_level_with_encrypted_primary_produces_cleartext_output() {
    // Same can_preserve guard as
    // pages_stream_data_uncompress_with_encrypted_primary_produces_cleartext_output,
    // but reached through an explicit non-`none` `--decode-level` instead of
    // `--stream-data`: both raise the writer's decode level above `None`, so
    // the --pages merge path must not carry the primary's encryption donor
    // forward for either.
    let tmp = tempfile::tempdir().unwrap();
    let Some(enc) = make_encrypted_three_page(tmp.path(), "secretpw") else {
        return;
    };
    let secondary = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    let (ok, stderr) = run_qpdf(&[
        "--password=secretpw",
        "--decode-level=generalized",
        "--pages",
        secondary.to_str().unwrap(),
        "1-2",
        "--",
        enc.to_str().unwrap(),
        q.to_str().unwrap(),
    ]);
    assert!(
        ok || q.exists(),
        "qpdf --decode-level merge failed: {stderr}"
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            enc.to_str().unwrap(),
            f.to_str().unwrap(),
            "--password=secretpw",
            "--decode-level=generalized",
            "--pages",
            secondary.to_str().unwrap(),
            "1-2",
            "--",
        ])
        .assert()
        .success();

    assert_qpdf_cleartext_chunk(&q);
    assert_qpdf_cleartext_chunk(&f);
    assert_eq!(npages_of(&q), npages_of(&f));
}

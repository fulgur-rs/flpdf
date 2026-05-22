//! End-to-end QDF round-trip + `qdf-fix` test matrix (flpdf-9hc.6.9, FINAL
//! leaf of epic flpdf-9hc.6). TEST-ONLY: no `src` changes; drives the real
//! `flpdf` binary via assert_cmd exactly as a user / qpdf would.
//!
//! Matrix:
//!  (a) `rewrite --qdf <in> <out.qdf>` — structural invariants asserted
//!      unconditionally; qpdf parity (check + page count + re-canonicalize)
//!      asserted only when qpdf is present.
//!  (b) `qdf-fix` on committed hand-edited corruptions (stale /Length holder,
//!      lengthened stream payload, shifted xref offsets) — holder recomputed,
//!      qpdf accepts the fixed file, the human-edited bytes survive,
//!      idempotent on a second application.
//!  (c) `rewrite --qdf` then `rewrite` (no --qdf) returns to normal form
//!      (no `%QDF-1.0` / `%% Original object ID:`; parses; page-equivalent).
//!      Exercises that flpdf can re-read its own QDF (indirect /Length, m41).
//!  (d) `qdf-fix` is a byte no-op on a valid clean QDF and idempotent.
//!
//! qpdf is the external oracle. Every live-qpdf assertion is gated behind an
//! availability check (same hard-fail-on-CI / soft-skip-locally policy as
//! `cli_qdf.rs` / `cli_object_streams_qpdf_parity.rs`).
//!
//! NOTE on `real-numbers-regression.pdf`: this is an intentionally malformed
//! regression fixture. `qpdf --check` returns exit 3 ("succeeded with
//! warnings") on the ORIGINAL file as well as on flpdf's QDF rewrite of it
//! (object 5 is an integer where a dict is expected). QDF does not introduce
//! the warning, so the qpdf-parity cell applies a "QDF must be no worse than
//! the original input under qpdf" rule for this one fixture rather than the
//! literal `== 0` rule (empirically verified during 6.9 bring-up). All
//! flpdf-internal invariants and content equivalence still hold exactly.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;

// ---------------------------------------------------------------------------
// qpdf availability guard (same policy as cli_qdf.rs)
// ---------------------------------------------------------------------------

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `true` when the caller should skip live-qpdf assertions. Panics on Linux
/// CI where qpdf is a hard dependency.
#[must_use]
fn skip_if_qpdf_missing() -> bool {
    if qpdf_available() {
        return false;
    }
    let on_ci = std::env::var_os("CI").is_some();
    if on_ci {
        panic!(
            "qpdf is required for cli_qdf_roundtrip_matrix tests on CI; \
             install qpdf in the workflow before running this test suite"
        );
    }
    eprintln!(
        "skipping live-qpdf assertions: qpdf not available (target_os={}, CI={})",
        std::env::consts::OS,
        on_ci
    );
    true
}

/// Sentinel returned by [`qpdf_check_code`] when qpdf 12.x aborts `--check`
/// with its upstream empty-page-tree `vector::at()` crash (flpdf-d4k) instead
/// of producing a real exit code. The zero-page fixture (`minimal.pdf`) is a
/// valid PDF — qpdf 11.x and the spec accept it — so callers treat this as
/// "oracle unavailable for this input" and skip the qpdf gate. Self-heals
/// once qpdf fixes the bug upstream.
const QPDF_EMPTY_PAGE_TREE_BUG: i32 = i32::MIN;

/// Exit code of `qpdf --check <path>` (0 = clean, 3 = succeeded with
/// warnings, 2 = errors), or [`QPDF_EMPTY_PAGE_TREE_BUG`] when qpdf hit the
/// upstream zero-page crash. libstdc++ renders the crash as
/// `vector::_M_range_check: ...`, libc++ (macOS) as a bare `vector`; both
/// surface on stderr as a line starting `ERROR: vector`.
fn qpdf_check_code(path: &Path) -> i32 {
    let out = ShellCommand::new("qpdf")
        .args(["--check", path.to_str().unwrap()])
        .output()
        .expect("failed to spawn qpdf");
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Anchored to a line *starting* `ERROR: vector` (not an unanchored
    // substring) so a real qpdf error that merely mentions the word is not
    // misclassified as the upstream zero-page crash.
    if !out.status.success()
        && stderr
            .lines()
            .any(|line| line.trim_end().starts_with("ERROR: vector"))
    {
        return QPDF_EMPTY_PAGE_TREE_BUG;
    }
    let code = out.status.code().unwrap_or(-1);
    // `.output()` captures qpdf's stderr (needed for the signature check
    // above); surface it on a genuine failure so a CI assertion carries
    // qpdf's own diagnostic rather than only an exit code.
    if code != 0 && !stderr.trim().is_empty() {
        eprintln!("qpdf --check {} (exit {code}):\n{stderr}", path.display());
    }
    code
}

/// `qpdf --qdf <path> -` re-canonicalization exit code (qpdf re-parses the
/// file and emits its own QDF to stdout).
fn qpdf_recanonicalize_code(path: &Path) -> i32 {
    ShellCommand::new("qpdf")
        .args(["--qdf", path.to_str().unwrap(), "-"])
        .output()
        .expect("failed to spawn qpdf")
        .status
        .code()
        .unwrap_or(-1)
}

/// `true` when `actual` is "no worse than" `baseline` under qpdf exit-code
/// *semantics* — NOT numeric ordering. qpdf codes: 0 = clean, 3 = succeeded
/// with warnings, 2 = errors. Badness ranks 0 < 3 < 2, so a naive `actual <=
/// baseline` would wrongly accept a warnings(3) -> errors(2) regression.
/// Acceptable outcomes: a clean result (0), or exactly the same code as the
/// baseline (e.g. an intentionally-warning fixture staying at 3).
fn qpdf_no_worse_than(actual: i32, baseline: i32) -> bool {
    // qpdf exit codes: 0 = clean, 3 = warnings only, 2 = errors. The
    // "badness" order is 0 < 3 < 2 (NOT numeric): `actual` is acceptable iff
    // it is no worse than `baseline`. This correctly allows improvements such
    // as errors(2) -> warnings(3) and warnings(3) -> clean(0), and rejects
    // regressions such as warnings(3) -> errors(2).
    fn rank(code: i32) -> u8 {
        match code {
            0 => 0, // clean
            3 => 1, // warnings only
            2 => 2, // errors
            _ => 3, // anything else: treat as worst
        }
    }
    rank(actual) <= rank(baseline)
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// `tests/fixtures` resolved relative to the crate manifest.
///
/// `canonicalize()` makes the path absolute and verifies it exists, but on
/// Windows it returns a `\\?\`-verbatim path that the bundled `qpdf` binary
/// cannot open (`qpdf: open ...: No such file or directory`). Strip that
/// prefix so fixture paths handed to qpdf stay openable on every platform.
fn fixtures_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .canonicalize()
        .expect("tests/fixtures must exist");
    // `\\?\D:\...` -> `D:\...`. CI/test checkouts always live on a drive,
    // never a `\\?\UNC\...` share, so a plain prefix strip is sufficient.
    match dir.to_str() {
        Some(s) => PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(s)),
        None => dir,
    }
}

fn fixture(rel: &str) -> PathBuf {
    fixtures_dir().join(rel)
}

fn flpdf() -> Command {
    Command::cargo_bin("flpdf").unwrap()
}

// ---------------------------------------------------------------------------
// Byte helpers
// ---------------------------------------------------------------------------

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// Number of `stream` keywords (proxy for "fixture has stream objects").
fn has_stream(bytes: &[u8]) -> bool {
    bytes
        .split(|&b| b == b'\n')
        .any(|line| line == b"stream" || line == b"stream\r")
}

/// `true` when an `/Length N 0 R` (indirect length holder) appears.
fn has_indirect_length(bytes: &[u8]) -> bool {
    let key = b"/Length ";
    bytes
        .windows(key.len())
        .enumerate()
        .filter(|(_, w)| *w == key)
        .any(|(i, _)| {
            let tail = &bytes[i + key.len()..];
            let mut j = 0;
            while j < tail.len() && tail[j].is_ascii_digit() {
                j += 1;
            }
            j > 0 && tail[j..].starts_with(b" 0 R")
        })
}

/// Page count via `flpdf pages <path>` (counts `page N:` lines). Works on
/// both normal PDFs and QDF files since both parse through flpdf.
fn page_count(path: &Path) -> usize {
    let out = flpdf()
        .args(["pages", path.to_str().unwrap()])
        .output()
        .expect("flpdf pages failed to run");
    assert!(
        out.status.success(),
        "flpdf pages must succeed on {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with("page ") && l.contains(':'))
        .count()
}

/// Assert the *unconditional* QDF structural invariants. `expect_streams`
/// gates the indirect-`/Length` invariant: stream-less fixtures (e.g.
/// `minimal.pdf`, a zero-page catalog) legitimately have no holder objects.
fn assert_qdf_invariants(bytes: &[u8], expect_streams: bool) {
    assert!(
        contains(bytes, b"%QDF-1.0"),
        "QDF must carry the %QDF-1.0 header marker"
    );
    // Every `%% Original object ID: A B` line must be immediately followed by
    // the matching `A B obj` line (the annotation is well-formed and adjacent
    // to its object). Synthetic length-holder objects (flpdf-9hc.6.12) carry
    // no such comment by design and are simply not annotated — so we assert
    // structural correctness of each annotation, not a bare substring.
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.split('\n').collect();
    let mut annotated = 0usize;
    for (i, line) in lines.iter().enumerate() {
        let Some(id) = line.strip_prefix("%% Original object ID: ") else {
            continue;
        };
        annotated += 1;
        let expected_obj = format!("{} obj", id.trim());
        assert_eq!(
            lines.get(i + 1).map(|s| s.trim_end_matches('\r')),
            Some(expected_obj.as_str()),
            "QDF `%% Original object ID: {id}` must be immediately followed \
             by `{expected_obj}`"
        );
    }
    assert!(
        annotated > 0,
        "QDF must annotate its indirect objects with %% Original object ID:"
    );
    assert!(
        contains(bytes, b"\nxref\n"),
        "QDF must use a classic `xref` table"
    );
    assert!(
        contains(bytes, b"trailer"),
        "QDF must use a classic `trailer`"
    );
    assert!(
        !contains(bytes, b"/Type /XRef"),
        "QDF must not use a cross-reference stream"
    );
    assert!(
        !contains(bytes, b"/Type /ObjStm"),
        "QDF must not use object streams"
    );
    if expect_streams {
        assert!(
            has_stream(bytes),
            "fixture was expected to contain stream objects"
        );
        assert!(
            has_indirect_length(bytes),
            "QDF stream objects must use an indirect `/Length N 0 R` holder"
        );
    }
}

// ---------------------------------------------------------------------------
// Fixture inventory for cell (a) / (c).
//
// `expect_streams`: whether the fixture produces stream objects in QDF
//   (minimal.pdf is a zero-page catalog with no content streams).
// `qpdf_clean`: whether `qpdf --check` is expected to return exit 0 on a
//   well-formed rewrite. `real-numbers-regression.pdf` is intentionally
//   malformed and yields exit 3 on the ORIGINAL input too, so QDF parity for
//   it is asserted as "no worse than the original" rather than "== 0".
// ---------------------------------------------------------------------------

struct FixtureSpec {
    rel: &'static str,
    expect_streams: bool,
    qpdf_clean: bool,
}

const FIXTURES: &[FixtureSpec] = &[
    FixtureSpec {
        rel: "minimal.pdf",
        expect_streams: false,
        qpdf_clean: true,
    },
    FixtureSpec {
        rel: "real-numbers-regression.pdf",
        expect_streams: true,
        qpdf_clean: false,
    },
    FixtureSpec {
        rel: "compat/three-page.pdf",
        expect_streams: true,
        qpdf_clean: true,
    },
    FixtureSpec {
        rel: "compat/one-page.pdf",
        expect_streams: true,
        qpdf_clean: true,
    },
    FixtureSpec {
        rel: "compat/two-page.pdf",
        expect_streams: true,
        qpdf_clean: true,
    },
    FixtureSpec {
        rel: "compat/multi-contents-one-page.pdf",
        expect_streams: true,
        qpdf_clean: true,
    },
    FixtureSpec {
        rel: "compat/unref-resources-one-page.pdf",
        expect_streams: true,
        qpdf_clean: true,
    },
];

// ===========================================================================
// (a) WRITE + qpdf-ACCEPTS + CONTENT-EQUIVALENT
// ===========================================================================

#[test]
fn cell_a_rewrite_qdf_invariants_and_qpdf_parity() {
    let tmp = tempfile::tempdir().unwrap();
    let qpdf_missing = skip_if_qpdf_missing();

    for spec in FIXTURES {
        let input = fixture(spec.rel);
        let out = tmp
            .path()
            .join(format!("{}.qdf", spec.rel.replace(['/', '.'], "_")));

        flpdf()
            .args([
                "rewrite",
                "--qdf",
                "--static-id",
                input.to_str().unwrap(),
                out.to_str().unwrap(),
            ])
            .assert()
            .success();

        let bytes = std::fs::read(&out).unwrap();
        assert_qdf_invariants(&bytes, spec.expect_streams);

        // Content equivalence (unconditional, via flpdf's own parser): the
        // QDF must report the same page count as the source.
        assert_eq!(
            page_count(&input),
            page_count(&out),
            "QDF page count must equal the source for {}",
            spec.rel
        );

        if qpdf_missing {
            continue;
        }

        let out_code = qpdf_check_code(&out);
        if out_code == QPDF_EMPTY_PAGE_TREE_BUG {
            eprintln!(
                "qpdf hit the empty-page-tree --check bug (flpdf-d4k); \
                 skipping the qpdf gate for zero-page fixture {}",
                spec.rel
            );
            continue;
        }
        if spec.qpdf_clean {
            assert_eq!(
                out_code, 0,
                "qpdf --check must pass cleanly on QDF rewrite of {}",
                spec.rel
            );
        } else {
            // Intentionally malformed fixture: QDF must be no worse than the
            // original under qpdf (see module NOTE).
            let orig_code = qpdf_check_code(&input);
            assert!(
                qpdf_no_worse_than(out_code, orig_code),
                "QDF rewrite of {} must not be worse than the original \
                 under qpdf --check (orig={}, qdf={})",
                spec.rel,
                orig_code,
                out_code
            );
        }

        // qpdf can re-canonicalize the flpdf QDF (round-trips through qpdf).
        let recanon = qpdf_recanonicalize_code(&out);
        let recanon_ok = if spec.qpdf_clean {
            recanon == 0
        } else {
            qpdf_no_worse_than(recanon, qpdf_check_code(&input))
        };
        assert!(
            recanon_ok,
            "qpdf --qdf re-canonicalization of {} unexpectedly failed (code={})",
            spec.rel, recanon
        );
    }
}

#[test]
fn cell_a_encrypted_input_is_transparently_decrypted_by_qdf() {
    // DESIGN anticipated that the QDF path would *reject* encrypted input and
    // made asserting that rejection optional. Empirically (6.9 bring-up)
    // flpdf instead transparently decrypts an empty-user-password file (same
    // as qpdf) and emits a valid, unencrypted QDF. That is correct, useful
    // behavior — not a product bug — so this cell pins the *actual* boundary:
    // encrypted input -> success, output is unencrypted canonical QDF.
    let input = fixture("compat/encrypted-r4-three-page.pdf");
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("enc.qdf");

    flpdf()
        .args([
            "rewrite",
            "--qdf",
            "--static-id",
            input.to_str().unwrap(),
            out.to_str().unwrap(),
        ])
        .assert()
        .success();

    let bytes = std::fs::read(&out).unwrap();
    assert_qdf_invariants(&bytes, true);
    assert!(
        !contains(&bytes, b"/Encrypt"),
        "QDF rewrite of an encrypted input must be unencrypted (no /Encrypt)"
    );

    if !skip_if_qpdf_missing() {
        assert_eq!(
            qpdf_check_code(&out),
            0,
            "QDF rewrite of an encrypted input must pass qpdf --check"
        );
    }
}

// ===========================================================================
// (b) FIX-QDF on committed HAND-EDITED corruptions
// ===========================================================================

/// `tests/fixtures/qdf-roundtrip/<name>` — clean + hand-edited variants are
/// committed for determinism (generated via `rewrite --qdf --static-id`).
fn rt_fixture(name: &str) -> PathBuf {
    fixture("qdf-roundtrip").join(name)
}

/// Run one corruption cell: fix it, assert qpdf accepts the result (if
/// present), the injected human-edited bytes survive, the indirect length
/// holder(s) are present, and `qdf-fix` is idempotent.
fn run_fix_cell(corrupt_name: &str, surviving_bytes: &[u8]) {
    let tmp = tempfile::tempdir().unwrap();
    let corrupt = rt_fixture(corrupt_name);
    let fixed = tmp.path().join("fixed.qdf");
    let fixed2 = tmp.path().join("fixed2.qdf");

    // The committed corruption must actually be invalid (otherwise the cell
    // proves nothing). qpdf returns 3 (warnings) / 2 (errors) — anything
    // non-zero — on these.
    if !skip_if_qpdf_missing() {
        assert_ne!(
            qpdf_check_code(&corrupt),
            0,
            "committed corruption {corrupt_name} must fail qpdf --check pre-fix"
        );
    }

    flpdf()
        .args([
            "qdf-fix",
            corrupt.to_str().unwrap(),
            fixed.to_str().unwrap(),
        ])
        .assert()
        .success();

    let fixed_bytes = std::fs::read(&fixed).unwrap();

    // The hand-edited textual content must be preserved by qdf-fix (this is
    // the whole point of *fix* vs. *re-rewrite*).
    assert!(
        contains(&fixed_bytes, surviving_bytes),
        "qdf-fix must preserve the human-edited bytes ({:?}) in {corrupt_name}",
        String::from_utf8_lossy(surviving_bytes)
    );

    // The recomputed indirect length holder(s) must still be present.
    assert!(
        has_indirect_length(&fixed_bytes),
        "qdf-fix output must keep indirect `/Length N 0 R` holders \
         ({corrupt_name})"
    );

    if !skip_if_qpdf_missing() {
        assert_eq!(
            qpdf_check_code(&fixed),
            0,
            "qdf-fix output must pass qpdf --check cleanly ({corrupt_name})"
        );
    }

    // Idempotence: qdf-fix(qdf-fix(corrupt)) == qdf-fix(corrupt).
    flpdf()
        .args(["qdf-fix", fixed.to_str().unwrap(), fixed2.to_str().unwrap()])
        .assert()
        .success();
    assert_eq!(
        fixed_bytes,
        std::fs::read(&fixed2).unwrap(),
        "qdf-fix must be idempotent on its own output ({corrupt_name})"
    );
}

#[test]
fn cell_b_fix_stale_length_holder() {
    // Holder object 12's body was hand-edited 90 -> 7. qdf-fix must
    // recompute it. The page-3 stream text is a stable surviving marker.
    run_fix_cell("three-page-stale-length.qdf", b"(Fixture page 3) Tj");
}

#[test]
fn cell_b_fix_lengthened_stream_payload() {
    // A content stream was hand-edited longer ("EDITED!!!"). qdf-fix must
    // recompute the holder *and* keep the human edit verbatim.
    run_fix_cell(
        "three-page-edited-payload.qdf",
        b"(Fixture page 1 EDITED!!!) Tj",
    );
}

#[test]
fn cell_b_fix_shifted_byte_offsets() {
    // A comment line was hand-inserted into object 1's body, staling every
    // later xref offset. qdf-fix must rebuild the table and keep the line.
    run_fix_cell(
        "three-page-shifted-offsets.qdf",
        b"% hand-inserted line shifting offsets",
    );
}

// ===========================================================================
// (c) QDF -> NORMAL round-trip (flpdf re-reads its own QDF; m41)
// ===========================================================================

#[test]
fn cell_c_qdf_to_normal_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let qpdf_missing = skip_if_qpdf_missing();

    for spec in FIXTURES {
        let input = fixture(spec.rel);
        let qdf = tmp
            .path()
            .join(format!("{}.qdf", spec.rel.replace(['/', '.'], "_")));
        let normal = tmp
            .path()
            .join(format!("{}.normal.pdf", spec.rel.replace(['/', '.'], "_")));

        flpdf()
            .args([
                "rewrite",
                "--qdf",
                "--static-id",
                input.to_str().unwrap(),
                qdf.to_str().unwrap(),
            ])
            .assert()
            .success();

        // Plain rewrite (NO --qdf) of the QDF file. This requires flpdf to
        // re-read its own QDF, including indirect /Length holders (m41).
        flpdf()
            .args(["rewrite", qdf.to_str().unwrap(), normal.to_str().unwrap()])
            .assert()
            .success();

        let normal_bytes = std::fs::read(&normal).unwrap();
        assert!(
            !contains(&normal_bytes, b"%QDF-1.0"),
            "QDF must not be sticky: normal rewrite of {} still has %QDF-1.0",
            spec.rel
        );
        assert!(
            !contains(&normal_bytes, b"%% Original object ID:"),
            "normal rewrite of {} still carries %% Original object ID:",
            spec.rel
        );

        // It must parse and be page-equivalent to the original.
        flpdf()
            .args(["--check", normal.to_str().unwrap()])
            .assert()
            .success();
        assert_eq!(
            page_count(&input),
            page_count(&normal),
            "QDF->normal round-trip changed the page count for {}",
            spec.rel
        );

        if qpdf_missing {
            continue;
        }
        let code = qpdf_check_code(&normal);
        if code == QPDF_EMPTY_PAGE_TREE_BUG {
            eprintln!(
                "qpdf hit the empty-page-tree --check bug (flpdf-d4k); \
                 skipping the qpdf gate for zero-page fixture {}",
                spec.rel
            );
            continue;
        }
        if spec.qpdf_clean {
            assert_eq!(
                code, 0,
                "qpdf --check must pass on the QDF->normal round-trip of {}",
                spec.rel
            );
        } else {
            assert!(
                qpdf_no_worse_than(code, qpdf_check_code(&input)),
                "QDF->normal of {} must be no worse than original under qpdf",
                spec.rel
            );
        }
    }
}

// ===========================================================================
// (d) fix-qdf idempotent / byte no-op on a valid clean QDF
// ===========================================================================

#[test]
fn cell_d_fix_qdf_is_noop_and_idempotent_on_clean() {
    for clean_name in ["minimal-clean.qdf", "three-page-clean.qdf"] {
        let clean = rt_fixture(clean_name);
        let tmp = tempfile::tempdir().unwrap();
        let out1 = tmp.path().join("out1.qdf");
        let out2 = tmp.path().join("out2.qdf");

        flpdf()
            .args(["qdf-fix", clean.to_str().unwrap(), out1.to_str().unwrap()])
            .assert()
            .success();
        flpdf()
            .args(["qdf-fix", out1.to_str().unwrap(), out2.to_str().unwrap()])
            .assert()
            .success();

        let clean_bytes = std::fs::read(&clean).unwrap();
        let b1 = std::fs::read(&out1).unwrap();
        let b2 = std::fs::read(&out2).unwrap();
        assert_eq!(
            clean_bytes, b1,
            "qdf-fix must be a byte no-op on a valid clean QDF ({clean_name})"
        );
        assert_eq!(
            b1, b2,
            "qdf-fix must be idempotent on a valid clean QDF ({clean_name})"
        );

        // And the committed clean fixture itself must satisfy the QDF
        // invariants (guards against fixture rot).
        assert_qdf_invariants(&clean_bytes, clean_name != "minimal-clean.qdf");
        if !skip_if_qpdf_missing() {
            let code = qpdf_check_code(&clean);
            if code == QPDF_EMPTY_PAGE_TREE_BUG {
                eprintln!(
                    "qpdf hit the empty-page-tree --check bug (flpdf-d4k); \
                     skipping the qpdf gate for zero-page fixture {clean_name}"
                );
            } else {
                assert_eq!(
                    code, 0,
                    "committed clean fixture {clean_name} must pass qpdf --check"
                );
            }
        }
    }
}

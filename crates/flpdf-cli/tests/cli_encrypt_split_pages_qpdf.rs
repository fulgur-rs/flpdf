#![cfg(feature = "qpdf-zlib-compat")]
//! qpdf 11.9.0 parity for `--encrypt` combined with `--split-pages`.
//!
//! `QPDFJob::doSplitPages` (`libqpdf/QPDFJob.cc:2940-3027`) builds a fresh
//! `QPDF`/`emptyPDF()` per chunk and constructs a fresh `QPDFWriter` for it,
//! then re-runs `setWriterOptions` inside that same loop
//! (`QPDFJob.cc:3019-3021` reaching `QPDFJob.cc:2856`). Encryption is therefore
//! configured once per chunk, so every chunk carries its own `/ID` and its own
//! `/Encrypt` dictionary rather than sharing the primary's.
//!
//! # Which handlers are byte-comparable
//!
//! Revisions 2, 3 and 4 derive the file key from the padded password, `/ID[0]`
//! and `/P` (`QPDF::compute_encryption_key`, `QPDF_encryption.cc:363-404`), so
//! `--static-id` alone makes RC4 output reproducible, and `--static-aes-iv`
//! additionally pins the per-stream AESV2 initialization vector
//! (`QPDF_encryption.cc:652`). Those cases are pinned here as whole-output byte
//! equality on both CLI routes.
//!
//! Revision 6 (`--encrypt … 256`) is *not* byte-comparable between two
//! processes: `QPDF::compute_encryption_parameters_V5`
//! (`QPDF_encryption.cc:1180-1198`) draws the file encryption key itself from
//! `QUtil::initializeWithRandomBytes`, and Algorithms 8 and 9 draw both salts
//! the same way (`QPDF_encryption.cc:610`, `:629`). Neither `--static-id` nor
//! `--static-aes-iv` suppresses those draws, so two runs of real qpdf 11.9.0 on
//! the same input already differ inside the first encrypted stream and inside
//! `/O`, `/U`, `/OE`, `/UE` and `/Perms`. Comparing raw R6 output against
//! qpdf's measures qpdf's RNG, not parity. The R6 case is pinned here by the
//! two properties that *are* reproducible: the decrypted document bytes, and
//! the `/Encrypt` dictionary with its five random strings reduced to their
//! lengths.
//!
//! Whole-output comparison needs the qpdf-zlib-compat feature; the default
//! miniz_oxide backend intentionally produces different, valid Flate bytes.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

const COMPAT: &str = "../../tests/fixtures/compat";
const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

/// User password for every case here; the R6 comparison authenticates with it
/// to decrypt both sides.
const USER_PASSWORD: &str = "u";

/// One split chunk: the part of its filename after the output template's stem,
/// and its bytes.
type Chunk = (String, Vec<u8>);

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(COMPAT)
        .join(name)
}

fn skip_if_qpdf_missing() -> bool {
    let version = ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| {
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
        });
    if version
        .as_deref()
        .is_some_and(|stdout| stdout.lines().next() == Some(EXPECTED_QPDF_VERSION))
    {
        return false;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf 11.9.0 is required for --encrypt + --split-pages parity: {version:?}");
    }
    eprintln!("skipping: qpdf 11.9.0 is not available: {version:?}");
    true
}

fn run_qpdf(args: &[&str]) -> Output {
    ShellCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(args: &[&str]) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        // `--static-id` warns on every run; the warning is not part of what
        // this parity test pins.
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(args)
        .output()
        .expect("flpdf should spawn")
}

fn assert_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed ({:?}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Collect every chunk written from the `<stem>.pdf` template, sorted by name
/// and keyed by the suffix qpdf appended to the stem.
///
/// qpdf derives split names from the template's `.pdf` suffix
/// (`libqpdf/QPDFJob.cc:2950-2959`), writing the single-page form `<stem>-1`
/// for `--split-pages=1` and the range form `<stem>-1-2` otherwise, so the
/// suffixes are part of what this comparison pins.
fn split_chunks(directory: &Path, stem: &str) -> Vec<Chunk> {
    let prefix = format!("{stem}-");
    let mut chunks: Vec<Chunk> = std::fs::read_dir(directory)
        .expect("split output directory should be readable")
        .map(|entry| entry.expect("split output entry should be readable").path())
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?.to_owned();
            let suffix = name.strip_prefix(&prefix)?;
            name.ends_with(".pdf").then(|| {
                let bytes = std::fs::read(&path).expect("split chunk should be readable");
                (suffix.to_owned(), bytes)
            })
        })
        .collect();
    chunks.sort_by(|left, right| left.0.cmp(&right.0));
    assert!(
        !chunks.is_empty(),
        "no split chunk was written for {stem} in {}",
        directory.display()
    );
    chunks
}

fn suffixes(chunks: &[Chunk]) -> Vec<&str> {
    chunks.iter().map(|(suffix, _)| suffix.as_str()).collect()
}

fn assert_chunks_match(expected: &[Chunk], actual: &[Chunk], label: &str) {
    // The suffixes encode the chunk count and the page-range spelling, so
    // comparing them first turns a naming regression into its own message.
    assert_eq!(
        suffixes(expected),
        suffixes(actual),
        "{label}: split chunk names must match qpdf's"
    );
    for ((suffix, expected_bytes), (_, actual_bytes)) in expected.iter().zip(actual) {
        assert_eq!(
            expected_bytes, actual_bytes,
            "{label}: chunk -{suffix} must be byte-identical to qpdf 11.9.0"
        );
    }
}

/// Every security handler whose output is reproducible across processes, with
/// the flags that pin its remaining randomness.
///
/// R2 (`40`) and R3 (`128`) are RC4 and need only `--static-id`; R4 with
/// `--use-aes=y` additionally needs `--static-aes-iv`, because AESV2 writes a
/// random initialization vector in front of every encrypted string and stream.
const DETERMINISTIC_HANDLERS: [(&str, &[&str]); 3] = [
    (
        "rc4-40",
        &["--allow-weak-crypto", "--encrypt", "u", "o", "40", "--"],
    ),
    (
        "rc4-128",
        &["--allow-weak-crypto", "--encrypt", "u", "o", "128", "--"],
    ),
    (
        "aesv2-128",
        &[
            "--static-aes-iv",
            "--encrypt",
            "u",
            "o",
            "128",
            "--use-aes=y",
            "--",
        ],
    ),
];

/// `--encrypt` + `--split-pages` stays byte-identical to qpdf for every
/// security handler whose output is reproducible across processes.
///
/// Both CLI surfaces are covered: the top-level qpdf-shaped route, where
/// `QPDFJob` owns the split, and the `rewrite` subcommand, where the CLI does.
#[test]
fn deterministic_handlers_split_byte_identical_to_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    for source in ["two-page.pdf", "three-page.pdf"] {
        let input = fixture(source);
        let input = input.to_str().expect("fixture paths are UTF-8");
        for chunk_size in ["1", "2"] {
            let split = format!("--split-pages={chunk_size}");
            for (handler, encrypt_args) in DETERMINISTIC_HANDLERS {
                let label = format!("{source} split={chunk_size} {handler}");
                let temp = tempfile::tempdir().expect("temporary directory");
                let oracle_template = temp.path().join("oracle.pdf");
                let top_level_template = temp.path().join("top-level.pdf");
                let rewrite_template = temp.path().join("rewrite.pdf");

                let mut oracle_args = vec!["--static-id"];
                oracle_args.extend_from_slice(encrypt_args);
                oracle_args.push(&split);
                oracle_args.push(input);
                oracle_args.push(oracle_template.to_str().expect("temp paths are UTF-8"));
                assert_success(&format!("qpdf {label}"), &run_qpdf(&oracle_args));
                let oracle = split_chunks(temp.path(), "oracle");

                // The top-level route hands --split-pages to QPDFJob itself,
                // exactly as qpdf's own CLI does.
                let mut top_level_args = vec!["--static-id"];
                top_level_args.extend_from_slice(encrypt_args);
                top_level_args.push(&split);
                top_level_args.push(input);
                top_level_args.push(top_level_template.to_str().expect("temp paths are UTF-8"));
                assert_success(
                    &format!("flpdf top-level {label}"),
                    &run_flpdf(&top_level_args),
                );
                assert_chunks_match(
                    &oracle,
                    &split_chunks(temp.path(), "top-level"),
                    &format!("top-level route {label}"),
                );

                // The rewrite subcommand splits the rewritten document itself,
                // reaching the same per-chunk writer configuration by a
                // different path.
                let mut rewrite_args = vec!["rewrite", "--static-id", &split];
                rewrite_args.extend_from_slice(encrypt_args);
                rewrite_args.push(input);
                rewrite_args.push(rewrite_template.to_str().expect("temp paths are UTF-8"));
                assert_success(&format!("flpdf rewrite {label}"), &run_flpdf(&rewrite_args));
                assert_chunks_match(
                    &oracle,
                    &split_chunks(temp.path(), "rewrite"),
                    &format!("rewrite route {label}"),
                );
            }
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn find_last(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

/// Return the whole `N G obj … endobj` block holding the standard security
/// handler dictionary, object header included.
///
/// The `/Encrypt` dictionary is itself never encrypted (ISO 32000-1 §7.6.2),
/// so it is readable in the raw output.
fn encrypt_object(bytes: &[u8]) -> Vec<u8> {
    let marker = find(bytes, b"/Filter /Standard").expect("encrypted output carries /Encrypt");
    let header = find_last(&bytes[..marker], b" obj\n").expect("/Encrypt is an indirect object");
    let line_start = bytes[..header]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |position| position + 1);
    let end = marker + find(&bytes[marker..], b"endobj").expect("the object is terminated");
    bytes[line_start..end + b"endobj".len()].to_vec()
}

/// Return the `/U <…>` entry of an `/Encrypt` object verbatim.
fn user_entry(object: &[u8]) -> Vec<u8> {
    let start = find(object, b"/U <").expect("R6 output carries /U");
    let close = start + find(&object[start..], b">").expect("/U is terminated");
    object[start..=close].to_vec()
}

/// Replace every hex string with `<hex:LEN>`, leaving dictionary delimiters,
/// keys, names and numbers verbatim.
///
/// In an R6 `/Encrypt` dictionary the hex strings are exactly `/O`, `/U`,
/// `/OE`, `/UE` and `/Perms`, all derived from the random draws at
/// `QPDF_encryption.cc:610`, `:629` and `:1198`. Their widths are fixed by
/// Algorithms 8, 9 and 10, so comparing lengths still rejects a wrong-width
/// entry while ignoring the bytes qpdf itself does not reproduce.
fn hex_strings_reduced_to_lengths(object: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(object.len());
    let mut index = 0;
    while index < object.len() {
        if object[index..].starts_with(b"<<") || object[index..].starts_with(b">>") {
            normalized.extend_from_slice(&object[index..index + 2]);
            index += 2;
            continue;
        }
        if object[index] == b'<' {
            let close = index + find(&object[index..], b">").expect("a hex string is terminated");
            let content = &object[index + 1..close];
            if !content.is_empty() && content.iter().all(u8::is_ascii_hexdigit) {
                normalized.extend_from_slice(format!("<hex:{}>", content.len()).as_bytes());
            } else {
                normalized.extend_from_slice(&object[index..=close]);
            }
            index = close + 1;
            continue;
        }
        normalized.push(object[index]);
        index += 1;
    }
    normalized
}

/// Replace every stream body with `<stream:LEN>`, keeping the `/Length` that
/// introduced it and every byte around it.
///
/// Stream bodies are the only ciphertext outside the `/Encrypt` dictionary in
/// the documents this test writes, so eliding them leaves the header, the
/// binary marker, every object dictionary, the cross-reference table, the
/// trailer and `startxref` comparable verbatim between two R6 outputs.
///
/// qpdf's default `--newline-before-endstream=no` writes the body directly
/// against `endstream` (`QPDFWriter::writeString`, `QPDFWriter.cc:1247-1256`),
/// so the body length comes from the dictionary's own `/Length`, and the
/// `endstream` landing check rejects any stream this simplification cannot
/// measure (an indirect `/Length`, for instance).
fn stream_bodies_elided(bytes: &[u8]) -> Vec<u8> {
    const OPEN: &[u8] = b"\nstream\n";
    let mut elided = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(offset) = find(&bytes[index..], OPEN) {
        let body = index + offset + OPEN.len();
        let length_key = index
            + find_last(&bytes[index..body], b"/Length ").expect("a stream declares its /Length");
        let digits: Vec<u8> = bytes[length_key + b"/Length ".len()..]
            .iter()
            .copied()
            .take_while(u8::is_ascii_digit)
            .collect();
        let length: usize = String::from_utf8(digits)
            .expect("ASCII digits are UTF-8")
            .parse()
            .expect("/Length is a direct integer in this output");
        let end = body + length;
        assert!(
            bytes[end..].starts_with(b"endstream"),
            "a stream body of the declared /Length {length} must end at endstream"
        );
        elided.extend_from_slice(&bytes[index..body]);
        elided.extend_from_slice(format!("<stream:{length}>").as_bytes());
        index = end;
    }
    elided.extend_from_slice(&bytes[index..]);
    elided
}

/// Reduce a whole R6 output to everything qpdf reproduces run to run: the
/// document structure with stream ciphertext elided and the `/Encrypt`
/// dictionary's random entries reduced to their widths.
fn reproducible_skeleton(bytes: &[u8]) -> Vec<u8> {
    let elided = stream_bodies_elided(bytes);
    let object = encrypt_object(&elided);
    let start = find(&elided, &object).expect("the /Encrypt object was taken from this output");
    let mut skeleton = elided[..start].to_vec();
    skeleton.extend_from_slice(&hex_strings_reduced_to_lengths(&object));
    skeleton.extend_from_slice(&elided[start + object.len()..]);
    skeleton
}

/// Decrypt `input` with qpdf so two R6 outputs can be compared past the file
/// keys each process drew independently.
fn decrypted(input: &Path, output: &Path) -> Vec<u8> {
    let password = format!("--password={USER_PASSWORD}");
    let result = run_qpdf(&[
        &password,
        "--decrypt",
        "--static-id",
        input.to_str().expect("temp paths are UTF-8"),
        output.to_str().expect("temp paths are UTF-8"),
    ]);
    assert_success(&format!("qpdf --decrypt of {}", input.display()), &result);
    std::fs::read(output).expect("decrypted output should be readable")
}

/// `--encrypt … 256` + `--split-pages` matches qpdf everywhere qpdf is itself
/// reproducible.
///
/// The first assertion is the reason the rest exist: real qpdf 11.9.0, run
/// twice with `--static-id --static-aes-iv`, already produces different R6
/// bytes, so "flpdf's R6 split output differs from qpdf's at byte N" is not by
/// itself a parity signal.
#[test]
fn v5_split_matches_qpdf_past_its_own_randomness() {
    if skip_if_qpdf_missing() {
        return;
    }
    let input = fixture("three-page.pdf");
    let input = input.to_str().expect("fixture paths are UTF-8");
    let temp = tempfile::tempdir().expect("temporary directory");
    let common: [&str; 8] = [
        "--static-id",
        "--static-aes-iv",
        "--encrypt",
        USER_PASSWORD,
        "o",
        "256",
        "--",
        "--split-pages=1",
    ];
    let run = |program: &dyn Fn(&[&str]) -> Output, stem: &str, label: &str| -> Vec<Chunk> {
        let template = temp.path().join(format!("{stem}.pdf"));
        let mut args = common.to_vec();
        args.push(input);
        args.push(template.to_str().expect("temp paths are UTF-8"));
        assert_success(label, &program(&args));
        split_chunks(temp.path(), stem)
    };

    let oracle_first = run(&run_qpdf, "oracle-a", "qpdf R6 split (first run)");
    let oracle_second = run(&run_qpdf, "oracle-b", "qpdf R6 split (second run)");
    for ((suffix, first), (_, second)) in oracle_first.iter().zip(&oracle_second) {
        assert_eq!(
            first.len(),
            second.len(),
            "qpdf's own R6 chunks must at least agree in length (-{suffix})"
        );
        assert_ne!(
            first, second,
            "qpdf 11.9.0 draws the R6 file key and both salts randomly \
             (QPDF_encryption.cc:1180-1198), so two runs must differ (-{suffix}); \
             if they ever stop differing, the R6 checks below can be tightened \
             into a plain byte comparison"
        );
    }

    let actual = run(&run_flpdf, "flpdf", "flpdf R6 split");
    assert_eq!(
        suffixes(&oracle_first),
        suffixes(&actual),
        "R6 split chunk names must match qpdf's"
    );
    for (suffix, actual_bytes) in &actual {
        let (_, oracle_bytes) = oracle_first
            .iter()
            .find(|(oracle_suffix, _)| oracle_suffix == suffix)
            .expect("chunk suffixes were just asserted equal");
        // Every R6 random draw has a fixed width, so a length difference is a
        // real structural difference even where the bytes cannot match.
        assert_eq!(
            oracle_bytes.len(),
            actual_bytes.len(),
            "R6 chunk -{suffix} must be the same size as qpdf's"
        );
        assert_eq!(
            hex_strings_reduced_to_lengths(&encrypt_object(oracle_bytes)),
            hex_strings_reduced_to_lengths(&encrypt_object(actual_bytes)),
            "R6 chunk -{suffix} must carry qpdf's /Encrypt dictionary (object \
             number, key order, /V /R /Length /P /CF /StmF /StrF, and the width \
             of every random entry)"
        );
        // Everything except the ciphertext itself: header, binary marker,
        // object dictionaries, stream /Length values, cross-reference table,
        // trailer and startxref.
        assert_eq!(
            reproducible_skeleton(oracle_bytes),
            reproducible_skeleton(actual_bytes),
            "R6 chunk -{suffix} must match qpdf's document structure outside \
             the bytes qpdf itself does not reproduce"
        );

        let oracle_raw = temp.path().join(format!("oracle-raw-{suffix}"));
        let actual_raw = temp.path().join(format!("actual-raw-{suffix}"));
        std::fs::write(&oracle_raw, oracle_bytes).expect("oracle chunk should be writable");
        std::fs::write(&actual_raw, actual_bytes).expect("chunk should be writable");
        assert_eq!(
            decrypted(
                &oracle_raw,
                &temp.path().join(format!("oracle-plain-{suffix}"))
            ),
            decrypted(
                &actual_raw,
                &temp.path().join(format!("actual-plain-{suffix}"))
            ),
            "R6 chunk -{suffix} must decrypt to qpdf's document bytes"
        );
    }

    // Each chunk is written by its own QPDFWriter, so its encryption
    // parameters are drawn independently (QPDFJob.cc:3019-3021) rather than
    // carried over from the chunk before.
    let mut seen: Vec<Vec<u8>> = Vec::new();
    for (suffix, bytes) in &actual {
        let entry = user_entry(&encrypt_object(bytes));
        assert!(
            !seen.contains(&entry),
            "chunk -{suffix} must draw its own R6 parameters, not reuse an earlier chunk's"
        );
        seen.push(entry);
    }
    assert_eq!(
        seen.len(),
        oracle_first.len(),
        "every chunk must have been inspected"
    );
}

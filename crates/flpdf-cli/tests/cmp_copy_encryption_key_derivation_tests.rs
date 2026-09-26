#![cfg(feature = "qpdf-zlib-compat")]
//! qpdf 11.9.0 parity for the V<5 copy-encryption key re-derivation.
//!
//! `QPDFWriter::setEncryptionParametersInternal` (`libqpdf/QPDFWriter.cc:832-839`)
//! re-derives the output file key for V<5 with
//! `QPDF::compute_encryption_key(getPaddedUserPassword(), EncryptionData(V, R,
//! /Length / 8, ...))` and keeps the donor's authenticated key only for V>=5.
//! Neither branch checks the donor `/Length` against the key length the reader
//! actually authenticated with, so a donor whose `/Length` disagrees with its
//! real key length is copied rather than rejected, and a donor opened with
//! `--password-is-hex-key` (whose padded user password stays empty,
//! `QPDF_encryption.cc:930-934`) yields a key derived from the empty password.
//!
//! The same function also consumes the donor's `/O` and `/U` verbatim
//! (`QPDFWriter.cc:693-694` reads `getStringValue()`, not the padded copy the
//! reader built), so a donor storing fewer than 32 bytes in either entry
//! reaches the NUL padding of `pad_short_parameter`
//! (`QPDF_encryption.cc:316-321`, applied at `:807-808`) on the way in and is
//! re-emitted at its stored length on the way out. Those cases are pinned here
//! too.
//!
//! Both `--copy-encryption` and preserve-encryption of the primary input reach
//! the same function (`QPDFWriter.cc:2099-2101`), so every case is pinned on
//! both routes.
//!
//! Full output-byte comparison needs the qpdf-zlib-compat feature; the default
//! backend intentionally has different, valid Flate bytes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PLAIN_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/one-page.pdf"
);
const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    let available = Command::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == EXPECTED_QPDF_VERSION)
        })
        .unwrap_or(false);
    if !available && std::env::var_os("CI").is_some() {
        panic!("qpdf 11.9.0 is required for copy-encryption key-derivation parity");
    }
    available
}

fn run_qpdf(args: &[&str]) -> Output {
    let output = Command::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf 11.9.0 must run");
    assert!(
        output.status.success(),
        "qpdf {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// Build an encrypted donor and rewrite one same-width `/Length` field in its
/// `/Encrypt` dictionary.
///
/// The dictionary is never itself encrypted and the replacement has the same
/// byte width, so every xref offset stays valid.
fn donor_with_patched_length(
    directory: &Path,
    name: &str,
    encrypt_args: &[&str],
    old_length: &[u8],
    new_length: &[u8],
) -> PathBuf {
    assert_eq!(old_length.len(), new_length.len());
    let path = directory.join(format!("{name}.pdf"));
    let path_string = path.to_str().expect("temporary path must be UTF-8");
    let mut args = vec!["--static-id", "--allow-weak-crypto", "--encrypt"];
    args.extend_from_slice(encrypt_args);
    args.extend_from_slice(&["--", PLAIN_FIXTURE, path_string]);
    run_qpdf(&args);

    let mut bytes = std::fs::read(&path).expect("read encrypted donor");
    let offsets: Vec<usize> = bytes
        .windows(old_length.len())
        .enumerate()
        .filter_map(|(index, window)| (window == old_length).then_some(index))
        .collect();
    assert_eq!(
        offsets.len(),
        1,
        "{name}: donor must contain exactly one {:?} marker",
        String::from_utf8_lossy(old_length)
    );
    bytes[offsets[0]..offsets[0] + new_length.len()].copy_from_slice(new_length);
    std::fs::write(&path, bytes).expect("write patched donor");
    path
}

/// Build an encrypted donor whose `/O` or `/U` stores fewer than 32 bytes,
/// keeping every xref offset valid.
///
/// qpdf writes both entries as a fixed-width hex string (`<` + 64 hex digits +
/// `>`, 66 bytes), and PDF allows arbitrary whitespace between dictionary
/// entries, so dropping `32 - kept_bytes` bytes of value and appending twice as
/// many spaces is a same-width rewrite: the file length, the `/Encrypt` object's
/// extent, the xref table and `startxref` all stay as qpdf wrote them. No
/// decrypt / re-encrypt round trip is needed to reach a genuinely short stored
/// entry.
///
/// `dropped_tail_must_be_nul` states which entry is being shortened.
/// Algorithm 2 hashes the padded `/O` (`QPDF_encryption.cc:385`), so
/// truncating `/O` keeps the donor authenticatable only when the bytes dropped
/// are already NUL and the padding restores them exactly; the caller picks an
/// owner password that produces such an `/O`. `/U` carries no such constraint
/// at R>=3, where `check_user_password_V4` compares only the leading
/// `sizeof(MD5::Digest)` bytes (`QPDF_encryption.cc:511-518`).
fn donor_with_truncated_uo_parameter(
    directory: &Path,
    name: &str,
    encrypt_args: &[&str],
    key: &str,
    kept_bytes: usize,
    dropped_tail_must_be_nul: bool,
) -> PathBuf {
    assert!(kept_bytes < 32, "{name}: a full-width entry is not short");
    let path = directory.join(format!("{name}.pdf"));
    let path_string = path.to_str().expect("temporary path must be UTF-8");
    let mut args = vec!["--static-id", "--allow-weak-crypto", "--encrypt"];
    args.extend_from_slice(encrypt_args);
    args.extend_from_slice(&["--", PLAIN_FIXTURE, path_string]);
    run_qpdf(&args);

    let bytes = std::fs::read(&path).expect("read encrypted donor");
    let marker = format!("/{key} <");
    let starts: Vec<usize> = bytes
        .windows(marker.len())
        .enumerate()
        .filter_map(|(index, window)| (window == marker.as_bytes()).then_some(index))
        .collect();
    assert_eq!(
        starts.len(),
        1,
        "{name}: donor must contain exactly one {marker:?} marker"
    );
    let value_start = starts[0] + marker.len();
    let value = &bytes[value_start..value_start + 64];
    assert!(
        value.iter().all(u8::is_ascii_hexdigit),
        "{name}: /{key} must be a 32-byte hex string"
    );
    assert_eq!(
        bytes[value_start + 64],
        b'>',
        "{name}: /{key} must be exactly 32 bytes wide"
    );
    if dropped_tail_must_be_nul {
        assert!(
            value[kept_bytes * 2..].iter().all(|digit| *digit == b'0'),
            "{name}: the /{key} bytes this truncation drops are {:?}, not NUL, so NUL padding \
             would not restore the value the donor was encrypted with; pick a password whose \
             /{key} ends in NUL",
            String::from_utf8_lossy(&value[kept_bytes * 2..])
        );
    }

    let mut rewritten = Vec::with_capacity(bytes.len());
    rewritten.extend_from_slice(&bytes[..value_start]);
    rewritten.extend_from_slice(&value[..kept_bytes * 2]);
    rewritten.push(b'>');
    rewritten.resize(value_start + 65, b' ');
    rewritten.extend_from_slice(&bytes[value_start + 65..]);
    assert_eq!(
        rewritten.len(),
        bytes.len(),
        "{name}: the rewrite must not move any xref offset"
    );
    std::fs::write(&path, rewritten).expect("write truncated donor");
    path
}

fn run_flpdf(args: &[&str]) -> Output {
    Command::new(assert_cmd::cargo::cargo_bin!("flpdf"))
        .env("FLPDF_PROGNAME", "qpdf")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(args)
        .output()
        .expect("flpdf must run")
}

/// Run both tools with the same argument list, substituting each one's own
/// output path for `@OUT@`, and assert byte-for-byte agreement.
fn assert_same_output(directory: &Path, case: &str, args: &[&str]) {
    let qpdf_output = directory.join(format!("{case}-qpdf.pdf"));
    let flpdf_output = directory.join(format!("{case}-flpdf.pdf"));
    let substitute = |path: &Path| -> Vec<String> {
        args.iter()
            .map(|argument| {
                if *argument == "@OUT@" {
                    path.to_str().expect("output path must be UTF-8").to_owned()
                } else {
                    (*argument).to_owned()
                }
            })
            .collect()
    };

    let qpdf_args = substitute(&qpdf_output);
    let qpdf_args: Vec<&str> = qpdf_args.iter().map(String::as_str).collect();
    let qpdf = run_qpdf(&qpdf_args);

    let flpdf_args = substitute(&flpdf_output);
    let flpdf_args: Vec<&str> = flpdf_args.iter().map(String::as_str).collect();
    let flpdf = run_flpdf(&flpdf_args);

    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "{case}: exit code; flpdf stderr: {}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&flpdf.stderr),
        String::from_utf8_lossy(&qpdf.stderr),
        "{case}: stderr"
    );

    let qpdf_bytes = std::fs::read(&qpdf_output).expect("read qpdf output");
    let flpdf_bytes = std::fs::read(&flpdf_output).expect("read flpdf output");
    assert!(!qpdf_bytes.is_empty(), "{case}: qpdf wrote no output");
    assert_eq!(flpdf_bytes, qpdf_bytes, "{case}: output bytes");
}

/// Both routes into `setEncryptionParametersInternal`: the donor supplied via
/// `--copy-encryption`, and the same donor as the primary input with its
/// encryption preserved.
fn assert_both_routes(directory: &Path, case: &str, donor: &Path, password: &str) {
    let donor = donor.to_str().expect("donor path must be UTF-8");
    let copy_encryption = format!("--copy-encryption={donor}");
    let donor_password = format!("--encryption-file-password={password}");
    assert_same_output(
        directory,
        &format!("copy-{case}"),
        &[
            "--static-id",
            "--static-aes-iv",
            "--allow-weak-crypto",
            PLAIN_FIXTURE,
            "@OUT@",
            &copy_encryption,
            &donor_password,
        ],
    );

    let primary_password = format!("--password={password}");
    assert_same_output(
        directory,
        &format!("primary-{case}"),
        &[
            "--static-id",
            "--static-aes-iv",
            "--allow-weak-crypto",
            &primary_password,
            donor,
            "@OUT@",
        ],
    );
}

#[test]
fn copy_encryption_rederives_the_key_for_a_malformed_length() {
    if !qpdf_available() {
        eprintln!("skipping qpdf differential: qpdf 11.9.0 is not available");
        return;
    }
    let directory = tempfile::tempdir().expect("temporary directory");
    let rc4_128: &[&str] = &[
        "--user-password=u",
        "--owner-password=o",
        "--bits=128",
        "--use-aes=n",
    ];
    let aes_256: &[&str] = &["--user-password=u", "--owner-password=o", "--bits=256"];

    // /Length 32 -> a 4-byte key, shorter than the 16 bytes the reader used.
    let short = donor_with_patched_length(
        directory.path(),
        "v2-len032",
        rc4_128,
        b"/Length 128 /O",
        b"/Length 032 /O",
    );
    assert_both_routes(directory.path(), "v2-len032", &short, "u");

    // /Length 240 -> 30 bytes, clamped back to the 16-byte MD5 digest.
    let long = donor_with_patched_length(
        directory.path(),
        "v2-len240",
        rc4_128,
        b"/Length 128 /O",
        b"/Length 240 /O",
    );
    assert_both_routes(directory.path(), "v2-len240", &long, "u");

    // V=5 keeps the authenticated 32-byte key and copies /Length verbatim.
    let v5 = donor_with_patched_length(
        directory.path(),
        "v5-len128",
        aes_256,
        b"/Length 256 /O",
        b"/Length 128 /O",
    );
    assert_both_routes(directory.path(), "v5-len128", &v5, "u");
}

/// `QPDF::compute_data_key` (`libqpdf/QPDF_encryption.cc:325-357`) appends a
/// fixed 9 bytes to the file key before hashing for AES (3-byte object id +
/// 2-byte generation + 4-byte `sAlT`), then clamps to 16: per-object key
/// length is `min(key_len + 9, 16)`. Every `key_len < 7` -- i.e. every
/// `/Length < 56` -- therefore yields a per-object key shorter than the 16
/// bytes the reader authenticated with, not just the single `/Length 032`
/// point the malformed-length test above covers. Sweep the range this
/// construction (patching `/Length` on an already-encrypted donor, same as
/// the test above) can actually authenticate: `000` through `032` open with
/// the original password under real qpdf 11.9.0; `040` and above do not
/// (`qpdf --show-npages`: "invalid password", reproduced directly with this
/// same donor-patching method, for both `--use-aes=n` and AES). That
/// falsifies this construction as a way to reach `040`-`048` -- a donor
/// authenticated at key_len 16 does not stay authenticatable once `/Length`
/// is relabeled down into that sub-range, regardless of what
/// `compute_data_key`'s clamped length would be if it were reached. A donor
/// actually reaching that sub-range (if one exists) needs a different
/// construction than this one.
#[test]
fn copy_encryption_rederives_the_key_across_the_full_short_length_range() {
    if !qpdf_available() {
        eprintln!("skipping qpdf differential: qpdf 11.9.0 is not available");
        return;
    }
    let directory = tempfile::tempdir().expect("temporary directory");
    let rc4_128: &[&str] = &[
        "--user-password=u",
        "--owner-password=o",
        "--bits=128",
        "--use-aes=n",
    ];

    for length in ["000", "008", "016", "024", "032"] {
        let case = format!("v2-len{length}");
        let donor = donor_with_patched_length(
            directory.path(),
            &case,
            rc4_128,
            b"/Length 128 /O",
            format!("/Length {length} /O").as_bytes(),
        );
        assert_both_routes(directory.path(), &case, &donor, "u");
    }
}

/// A donor that really stores fewer than 32 bytes in `/O` or `/U` and still
/// authenticates under real qpdf 11.9.0.
///
/// `pad_short_parameter` (`QPDF_encryption.cc:316-321`) NUL-pads a short V<5
/// `/O` / `/U` up to 32 bytes at `:807-808` before the exact-length check, so
/// the padding is not confined to hand-built dictionaries: a plain file with a
/// short entry reaches it. Both entries reach it from a donor that opens, but
/// for different reasons, and the two cases discriminate different things.
///
/// `/U` shortened to its leading 16 bytes: `check_user_password_V4`
/// (`QPDF_encryption.cc:511-518`) compares `to_compare = (R >= 3) ? 16 : 32`
/// bytes, so at R>=3 the dropped tail is never examined and the donor opens
/// with its original password. Nothing in the V<5 path consumes `/U[16..32]`
/// afterwards -- the file key comes from `/O`, `/P` and `/ID[0]` -- and
/// `copyEncryptionParameters` re-emits the stored 16 bytes verbatim
/// (`QPDFWriter.cc:694` reads `getStringValue()`). This case pins reachability
/// and non-rejection, not the padding's effect on any derived value. It needs
/// R>=3: an R=2 donor compares all 32 bytes and would reject the same
/// truncation.
///
/// `/O` shortened to 31 bytes: this one is load-bearing. Algorithm 2 hashes
/// the padded `/O` into the file key (`QPDF_encryption.cc:385`), so an
/// implementation that fed 31 bytes in would derive a different key and diverge
/// in the output bytes. Keeping the donor authenticatable requires an `/O`
/// whose last byte is already NUL, which `compute_O_value`
/// (`QPDF_encryption.cc:428-450`) fixes from the two passwords, R and the key
/// length alone -- no `/ID`, no randomness. Owner password `o295` is the first
/// of `o0`, `o1`, ... that yields one for user password `u` at RC4-128 R=3
/// under qpdf 11.9.0; `donor_with_truncated_uo_parameter` asserts that
/// precondition rather than trusting it.
#[test]
fn copy_encryption_carries_a_short_uo_parameter_from_an_authenticated_donor() {
    if !qpdf_available() {
        eprintln!("skipping qpdf differential: qpdf 11.9.0 is not available");
        return;
    }
    let directory = tempfile::tempdir().expect("temporary directory");

    // V=2 R=3: /U truncated to the 16 bytes the R>=3 user-password check reads.
    let rc4_128: &[&str] = &[
        "--user-password=u",
        "--owner-password=o",
        "--bits=128",
        "--use-aes=n",
    ];
    let short_u_v2 =
        donor_with_truncated_uo_parameter(directory.path(), "v2-u16", rc4_128, "U", 16, false);
    assert_both_routes(directory.path(), "v2-u16", &short_u_v2, "u");

    // V=4 R=4 reaches the same padding through the AES branch of the writer.
    let aes_128: &[&str] = &[
        "--user-password=u",
        "--owner-password=o",
        "--bits=128",
        "--use-aes=y",
    ];
    let short_u_v4 =
        donor_with_truncated_uo_parameter(directory.path(), "v4-u16", aes_128, "U", 16, false);
    assert_both_routes(directory.path(), "v4-u16", &short_u_v4, "u");

    // /O truncated to 31 bytes: the NUL padding has to restore the 32nd byte
    // for Algorithm 2, on the user-password route and on the owner-password
    // route that RC4-decrypts the padded /O (`QPDF_encryption.cc:542-567`,
    // reading all 32 bytes at `:550`).
    let nul_tailed_o: &[&str] = &[
        "--user-password=u",
        "--owner-password=o295",
        "--bits=128",
        "--use-aes=n",
    ];
    let short_o =
        donor_with_truncated_uo_parameter(directory.path(), "v2-o31", nul_tailed_o, "O", 31, true);
    assert_both_routes(directory.path(), "v2-o31-user", &short_o, "u");
    assert_both_routes(directory.path(), "v2-o31-owner", &short_o, "o295");
}

#[test]
fn copy_encryption_rederives_the_key_from_an_empty_hex_key_password() {
    if !qpdf_available() {
        eprintln!("skipping qpdf differential: qpdf 11.9.0 is not available");
        return;
    }
    let directory = tempfile::tempdir().expect("temporary directory");
    let donor = directory.path().join("rc4-128.pdf");
    run_qpdf(&[
        "--static-id",
        "--allow-weak-crypto",
        "--encrypt",
        "--user-password=u",
        "--owner-password=o",
        "--bits=128",
        "--use-aes=n",
        "--",
        PLAIN_FIXTURE,
        donor.to_str().expect("donor path must be UTF-8"),
    ]);

    let shown = run_qpdf(&[
        "--show-encryption",
        "--show-encryption-key",
        "--password=u",
        donor.to_str().expect("donor path must be UTF-8"),
    ]);
    let shown = String::from_utf8(shown.stdout).expect("qpdf report must be UTF-8");
    let key = shown
        .lines()
        .find_map(|line| line.strip_prefix("Encryption key = "))
        .expect("qpdf must report the donor's encryption key")
        .to_owned();

    // The supplied raw key never reaches the output: `getPaddedUserPassword()`
    // stays empty for a hex-key open, so every length behaves the same.
    for (case, raw_key) in [
        ("exact", key.clone()),
        ("long", format!("{key}0011223344")),
        ("short", "6836c004cc15007d".to_owned()),
        ("empty", String::new()),
    ] {
        let donor_path = donor.to_str().expect("donor path must be UTF-8");
        let copy_encryption = format!("--copy-encryption={donor_path}");
        let donor_password = format!("--encryption-file-password={raw_key}");
        assert_same_output(
            directory.path(),
            &format!("hexkey-{case}"),
            &[
                "--static-id",
                "--static-aes-iv",
                "--allow-weak-crypto",
                "--password-is-hex-key",
                PLAIN_FIXTURE,
                "@OUT@",
                &copy_encryption,
                &donor_password,
            ],
        );

        let primary_password = format!("--password={raw_key}");
        assert_same_output(
            directory.path(),
            &format!("hexkey-primary-{case}"),
            &[
                "--static-id",
                "--static-aes-iv",
                "--allow-weak-crypto",
                "--password-is-hex-key",
                &primary_password,
                donor_path,
                "@OUT@",
            ],
        );
    }
}

/// A V=4 donor with `/Length 040` yields a 5-byte file key, hence a 14-byte
/// per-object AES key (`QPDF::compute_data_key`). qpdf passes that raw key to
/// its AES-128 fallback, whose provider reads 16 bytes from the shorter
/// buffer (`QPDFCrypto_gnutls.cc:197-213`, `QPDFCrypto_openssl.cc:225-241`).
/// This read is undefined, so the test compares bytes only when two qpdf
/// runs with fixed ID and IV produce the same output.
#[test]
fn copy_encryption_uses_qpdf_provider_fallback_for_a_short_object_key() {
    if !qpdf_available() {
        eprintln!("skipping qpdf differential: qpdf 11.9.0 is not available");
        return;
    }
    let directory = tempfile::tempdir().expect("temporary directory");
    let donor = donor_with_patched_length(
        directory.path(),
        "v4-len040",
        &[
            "--user-password=u",
            "--owner-password=o",
            "--bits=128",
            "--use-aes=y",
        ],
        b"/Length 128 /O",
        b"/Length 040 /O",
    );
    let donor_path = donor.to_str().expect("donor path must be UTF-8");
    let qpdf_output = directory.path().join("v4-len040-qpdf.pdf");
    let qpdf_repeat_output = directory.path().join("v4-len040-qpdf-repeat.pdf");
    let flpdf_output = directory.path().join("v4-len040-flpdf.pdf");
    let args = [
        "--static-id",
        "--static-aes-iv",
        "--allow-weak-crypto",
        "--password=u",
        donor_path,
    ];
    let qpdf = run_qpdf(&[
        args[0],
        args[1],
        args[2],
        args[3],
        args[4],
        qpdf_output
            .to_str()
            .expect("qpdf output path must be UTF-8"),
    ]);
    let qpdf_repeat = run_qpdf(&[
        args[0],
        args[1],
        args[2],
        args[3],
        args[4],
        qpdf_repeat_output
            .to_str()
            .expect("repeated qpdf output path must be UTF-8"),
    ]);
    let flpdf = run_flpdf(&[
        args[0],
        args[1],
        args[2],
        args[3],
        args[4],
        flpdf_output
            .to_str()
            .expect("flpdf output path must be UTF-8"),
    ]);

    assert_eq!(qpdf_repeat.status.code(), qpdf.status.code());
    assert_eq!(qpdf_repeat.stderr, qpdf.stderr);
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stderr, qpdf.stderr);
    let qpdf_bytes = std::fs::read(qpdf_output).expect("read qpdf output");
    let qpdf_repeat_bytes = std::fs::read(qpdf_repeat_output).expect("read repeated qpdf output");
    let flpdf_bytes = std::fs::read(flpdf_output).expect("read flpdf output");
    if qpdf_bytes == qpdf_repeat_bytes {
        assert_eq!(
            flpdf_bytes, qpdf_bytes,
            "flpdf output should match reproducible qpdf short-key output"
        );
    }
    // When qpdf's out-of-bounds provider read changes between runs, its byte
    // output is undefined; exit status and diagnostics remain the observable
    // contract for that case.
    let stderr = String::from_utf8_lossy(&flpdf.stderr).into_owned();
    assert!(
        stderr.is_empty(),
        "the qpdf provider fallback must not reject the short key: {stderr}"
    );
}

/// A five-byte raw hex key on a V=4 AES donor produces a 14-byte per-object
/// key. Compare the CLI's `--password-is-hex-key` route with qpdf when qpdf's
/// out-of-bounds AES provider read happens to repeat deterministically.
#[test]
fn raw_five_byte_hex_key_matches_repeatable_qpdf_output() {
    if !qpdf_available() {
        eprintln!("skipping qpdf differential: qpdf 11.9.0 is not available");
        return;
    }
    let directory = tempfile::tempdir().expect("temporary directory");
    let donor = donor_with_patched_length(
        directory.path(),
        "v4-len040-raw-hex-key",
        &[
            "--user-password=u",
            "--owner-password=o",
            "--bits=128",
            "--use-aes=y",
        ],
        b"/Length 128 /O",
        b"/Length 040 /O",
    );
    let donor_path = donor.to_str().expect("donor path must be UTF-8");
    let qpdf_output = directory.path().join("raw-key-qpdf.pdf");
    let qpdf_repeat_output = directory.path().join("raw-key-qpdf-repeat.pdf");
    let flpdf_output = directory.path().join("raw-key-flpdf.pdf");
    let args = [
        "--static-id",
        "--static-aes-iv",
        "--allow-weak-crypto",
        "--password-is-hex-key",
        "--password=6161616161",
        donor_path,
    ];
    let mut qpdf_args = args.to_vec();
    qpdf_args.push(qpdf_output.to_str().expect("output path must be UTF-8"));
    let qpdf = run_qpdf(&qpdf_args);
    let mut qpdf_repeat_args = args.to_vec();
    qpdf_repeat_args.push(
        qpdf_repeat_output
            .to_str()
            .expect("repeated output path must be UTF-8"),
    );
    let qpdf_repeat = run_qpdf(&qpdf_repeat_args);
    let mut flpdf_args = args.to_vec();
    flpdf_args.push(flpdf_output.to_str().expect("output path must be UTF-8"));
    let flpdf = run_flpdf(&flpdf_args);

    assert_eq!(qpdf_repeat.stdout, qpdf.stdout, "qpdf stdout must repeat");
    assert_eq!(qpdf_repeat.stderr, qpdf.stderr, "qpdf stderr must repeat");
    assert_eq!(flpdf.status.code(), qpdf.status.code(), "exit status");
    assert_eq!(flpdf.stdout, qpdf.stdout, "stdout");
    assert_eq!(flpdf.stderr, qpdf.stderr, "stderr");

    let qpdf_bytes = std::fs::read(qpdf_output).expect("read qpdf output");
    let qpdf_repeat_bytes = std::fs::read(qpdf_repeat_output).expect("read repeated qpdf output");
    let flpdf_bytes = std::fs::read(flpdf_output).expect("read flpdf output");
    if qpdf_bytes == qpdf_repeat_bytes {
        assert!(
            flpdf_bytes == qpdf_bytes,
            "raw five-byte hex-key output must match repeatable qpdf bytes (flpdf {flpdf_len} bytes, qpdf {qpdf_len} bytes)",
            flpdf_len = flpdf_bytes.len(),
            qpdf_len = qpdf_bytes.len()
        );
    }
}

/// Same-width in-place patch of an existing donor. The `/Encrypt` dictionary
/// is never itself encrypted, so replacing a marker of identical width keeps
/// every xref offset valid.
fn patch_existing(directory: &Path, name: &str, source: &Path, old: &[u8], new: &[u8]) -> PathBuf {
    assert_eq!(old.len(), new.len());
    let path = directory.join(format!("{name}.pdf"));
    let mut bytes = std::fs::read(source).expect("read donor");
    let offsets: Vec<usize> = bytes
        .windows(old.len())
        .enumerate()
        .filter_map(|(index, window)| (window == old).then_some(index))
        .collect();
    assert_eq!(
        offsets.len(),
        1,
        "{name}: donor must contain exactly one {:?} marker",
        String::from_utf8_lossy(old)
    );
    bytes[offsets[0]..offsets[0] + new.len()].copy_from_slice(new);
    std::fs::write(&path, bytes).expect("write patched donor");
    path
}

/// qpdf's `initializeEncryption` accepts the full V∈{1,2,4,5} × R∈2..=6
/// cross product (`libqpdf/QPDF_encryption.cc:787-795`). flpdf used to refuse
/// V=4 R=3, V=4 R=5, and V=2 R=4 outright, so a rewrite of such a document
/// could not reach the canonical writer at all. The version floors those
/// cells produce are keyed on `/R` (`libqpdf/QPDFWriter.cc:806-814`), which
/// the primary route exercises through the document header.
#[test]
fn copy_encryption_accepts_the_qpdf_vr_set() {
    if !qpdf_available() {
        eprintln!("skipping qpdf differential: qpdf 11.9.0 is not available");
        return;
    }
    let directory = tempfile::tempdir().expect("temporary directory");
    let build_donor = |name: &str, encrypt_args: &[&str]| -> PathBuf {
        let path = directory.path().join(format!("{name}.pdf"));
        let mut args = vec!["--static-id", "--allow-weak-crypto", "--encrypt"];
        args.extend_from_slice(encrypt_args);
        args.extend_from_slice(&[
            "--",
            PLAIN_FIXTURE,
            path.to_str().expect("path must be UTF-8"),
        ]);
        run_qpdf(&args);
        path
    };
    let rc4_128 = build_donor(
        "rc4-128",
        &[
            "--user-password=u",
            "--owner-password=o",
            "--bits=128",
            "--use-aes=n",
        ],
    );
    let aes_128 = build_donor(
        "aes-128",
        &[
            "--user-password=u",
            "--owner-password=o",
            "--bits=128",
            "--use-aes=y",
        ],
    );

    for (case, source, old, new) in [
        ("v4-r3", &aes_128, &b"/R 4 /StmF"[..], &b"/R 3 /StmF"[..]),
        ("v4-r5", &aes_128, &b"/R 4 /StmF"[..], &b"/R 5 /StmF"[..]),
        ("v2-r4", &rc4_128, &b"/R 3 /U"[..], &b"/R 4 /U"[..]),
    ] {
        let patched = patch_existing(directory.path(), case, source, old, new);
        assert_both_routes(directory.path(), case, &patched, "u");
    }
}

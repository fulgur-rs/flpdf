//! Byte-level `/ID` parity between flpdf's `--deterministic-id` and qpdf
//! 11.9.0's `--deterministic-id`, under the `qpdf-zlib-compat` feature.
//!
//! qpdf derives the deterministic file identifier as a two-level MD5
//! (`QPDFWriter::generateID`): the *changing* identifier `/ID[1]` is
//! `md5(seed)`, where `seed = hex(md5(output up to and INCLUDING the `[` that
//! opens the `/ID` array)) + " QPDF "` followed, for every `/Info` entry whose
//! value is a string (iterated in sorted key order), by `" " + decoded value`.
//! The *permanent* identifier `/ID[0]` is the source `/ID[0]` when present,
//! else equal to `/ID[1]`.
//!
//! Achieving byte-identical output requires flpdf's body serialization to match
//! qpdf's (established for `--static-id` by the Catalog-first renumber + qpdf
//! key ordering), which the `qpdf-zlib-compat` feature pins for any compressed
//! streams. The fixture below is stream-free so the only thing under test is
//! the `/ID` algorithm and the classic-xref trailer layout.
//!
//! The golden constants were captured from qpdf 11.9.0 on the exact fixture
//! bytes produced by [`one_page_with_info_fixture`]; the test also re-runs qpdf
//! live and compares full bytes when the `qpdf` binary is on `PATH`.

#![cfg(feature = "qpdf-zlib-compat")]

use flpdf::{Pdf, WriteOptions};

/// A minimal one-page classic-xref PDF with NO streams and an `/Info`
/// dictionary whose keys are intentionally out of sorted order (`/Title`
/// precedes `/Author`), carry a non-string entry (`/Count 3`, skipped by the
/// seed), and include an escaped literal (`(Hello\)World)` -> `Hello)World`
/// after PDF unescaping). This exercises the seed's `/Info` path (sorted-key
/// order, non-string skip, unescape) while staying byte-parity-friendly.
fn one_page_with_info_fixture() -> Vec<u8> {
    let objs: [&[u8]; 4] = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        b"<< /Title (Hello\\)World) /Author (Bob) /Count 3 >>",
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, obj) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(obj);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objs.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 4 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// Like [`one_page_with_info_fixture`] but the `/Info` `/Title` is a UTF-16BE
/// string (BOM `FEFF` then ASCII code units `00xx`) carrying NUL bytes. qpdf
/// hashes the deterministic-`/ID` seed via `encodeString(seed.c_str())`, which
/// stops at the first NUL, so only the bytes before the first `00` contribute
/// to `/ID[1]`. This fixture pins that strlen truncation against real qpdf.
fn one_page_with_utf16_nul_info_fixture() -> Vec<u8> {
    // <feff00480049> = UTF-16BE BOM + "HI"; the first NUL is the high byte of
    // 'H' (00 48), so qpdf truncates the seed immediately after the BOM.
    let objs: [&[u8]; 4] = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        b"<< /Title <feff00480049> >>",
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, obj) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(obj);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objs.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 4 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn write_deterministic(fixture: &[u8]) -> Vec<u8> {
    let mut pdf = Pdf::open_mem(fixture).expect("fixture must open");
    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.deterministic_id = true;
    let mut out = Vec::new();
    flpdf::write_pdf_with_options(&mut pdf, &mut out, &opts).expect("deterministic write");
    out
}

/// Run `qpdf --deterministic-id --object-streams=disable` on `fixture` and
/// return its output bytes, or `None` when the `qpdf` binary is unavailable
/// (e.g. minimal CI images). Mirrors the temp-file dance of
/// [`deterministic_id_matches_live_qpdf_when_available`].
fn run_live_qpdf_deterministic(fixture: &[u8]) -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join(format!(
        "flpdf-det-parity-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).ok()?;
    let src = dir.join("src.pdf");
    let qout = dir.join("qpdf.pdf");
    if std::fs::write(&src, fixture).is_err() {
        let _ = std::fs::remove_dir_all(&dir);
        return None;
    }
    let status = std::process::Command::new("qpdf")
        .args(["--deterministic-id", "--object-streams=disable"])
        .arg(&src)
        .arg(&qout)
        .status();
    let result = match status {
        Ok(status) if status.success() => std::fs::read(&qout).ok(),
        // qpdf absent or failed: caller falls back to the committed assertions.
        _ => None,
    };
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// Offset of the opening `[` of the LAST `/ID` array (qpdf captures the running
/// digest immediately after writing this byte).
fn id_array_bracket_offset(bytes: &[u8]) -> usize {
    let id_pos = bytes
        .windows(3)
        .rposition(|w| w == b"/ID")
        .expect("output must contain /ID");
    id_pos
        + bytes[id_pos..]
            .iter()
            .position(|&b| b == b'[')
            .expect("/ID must be followed by an array")
}

fn lowercase_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn extract_id_words(bytes: &[u8]) -> (String, String) {
    let bracket = id_array_bracket_offset(bytes);
    let tail = &bytes[bracket..];
    // [<id0_hex><id1_hex>]
    let first_lt = tail.iter().position(|&b| b == b'<').unwrap();
    let first_gt = tail.iter().position(|&b| b == b'>').unwrap();
    let id0 = std::str::from_utf8(&tail[first_lt + 1..first_gt]).unwrap();
    let rest = &tail[first_gt + 1..];
    let second_lt = rest.iter().position(|&b| b == b'<').unwrap();
    let second_gt = rest.iter().position(|&b| b == b'>').unwrap();
    let id1 = std::str::from_utf8(&rest[second_lt + 1..second_gt]).unwrap();
    (id0.to_string(), id1.to_string())
}

// Golden values captured from qpdf 11.9.0 (`qpdf --deterministic-id
// --object-streams=disable`) on `one_page_with_info_fixture()`.
const GOLDEN_ID0: &str = "28ad45ebf68aa2a6d29aa8aec13979d0";
const GOLDEN_ID1: &str = "28ad45ebf68aa2a6d29aa8aec13979d0";
/// `hex(md5(output[0..=bracket]))` — the seed's first component. Pinning this
/// proves the digest range includes the `[` (the off-by-one qpdf cares about).
const GOLDEN_DET_DATA: &str = "670b3a737b03ae2af5774e7e73b26a0e";

#[test]
fn deterministic_id_matches_qpdf_golden_id_words() {
    let out = write_deterministic(&one_page_with_info_fixture());
    let (id0, id1) = extract_id_words(&out);
    assert_eq!(id0, GOLDEN_ID0, "/ID[0] diverged from qpdf 11.9.0 golden");
    assert_eq!(id1, GOLDEN_ID1, "/ID[1] diverged from qpdf 11.9.0 golden");
}

#[test]
fn deterministic_id_intermediate_body_digest_matches_golden() {
    // Verify the intermediate seed component on the REAL patched output bytes
    // (not just at analysis time): the digest range must end at the byte after
    // the `/ID` array's `[`.
    use md5::Digest as _;
    let out = write_deterministic(&one_page_with_info_fixture());
    let bracket = id_array_bracket_offset(&out);
    let det_data = lowercase_hex(&md5::Md5::digest(&out[..=bracket]));
    assert_eq!(
        det_data, GOLDEN_DET_DATA,
        "intermediate body digest (seed component) diverged from golden"
    );
}

#[test]
fn deterministic_id_matches_live_qpdf_when_available() {
    // Skip silently when qpdf is not installed (e.g. minimal CI images); the
    // committed goldens above still pin the algorithm. When qpdf IS present,
    // assert FULL byte-for-byte parity, the strongest possible check.
    let fixture = one_page_with_info_fixture();
    let Some(qpdf_bytes) = run_live_qpdf_deterministic(&fixture) else {
        // qpdf not on PATH: rely on the committed goldens.
        return;
    };
    let flpdf_bytes = write_deterministic(&fixture);
    assert_eq!(
        flpdf_bytes, qpdf_bytes,
        "flpdf --deterministic-id output must be byte-identical to qpdf 11.9.0"
    );
}

// Golden /ID captured from qpdf 11.9.0 on `one_page_with_utf16_nul_info_fixture()`.
// The /Title is UTF-16BE <feff00480049> ("HI"); qpdf truncates the seed at the
// first NUL (the high byte of 'H'), so /ID[1] hashes only the seed up to and
// including the BOM bytes. Without truncation flpdf would compute a different
// value, so pinning this golden proves the NUL cut matches qpdf.
const GOLDEN_NUL_ID0: &str = "60d21a67ea0876d606835017f9ad4e25";
const GOLDEN_NUL_ID1: &str = "60d21a67ea0876d606835017f9ad4e25";

#[test]
fn deterministic_id_nul_info_matches_qpdf_golden() {
    let out = write_deterministic(&one_page_with_utf16_nul_info_fixture());
    let (id0, id1) = extract_id_words(&out);
    assert_eq!(
        id0, GOLDEN_NUL_ID0,
        "/ID[0] for NUL-bearing /Info diverged from qpdf 11.9.0 golden"
    );
    assert_eq!(
        id1, GOLDEN_NUL_ID1,
        "/ID[1] for NUL-bearing /Info diverged from qpdf 11.9.0 golden \
         (seed must be truncated at the first NUL like qpdf's encodeString)"
    );
}

#[test]
fn deterministic_id_nul_info_matches_live_qpdf_when_available() {
    // Strongest check for the NUL-truncation case: full byte parity with the
    // real qpdf binary when it is on PATH; otherwise the committed golden above
    // still pins the truncated /ID.
    let fixture = one_page_with_utf16_nul_info_fixture();
    let Some(qpdf_bytes) = run_live_qpdf_deterministic(&fixture) else {
        return;
    };
    let flpdf_bytes = write_deterministic(&fixture);
    assert_eq!(
        flpdf_bytes, qpdf_bytes,
        "flpdf --deterministic-id output for NUL-bearing /Info must be \
         byte-identical to qpdf 11.9.0"
    );
}

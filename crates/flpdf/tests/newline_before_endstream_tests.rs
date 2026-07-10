//! Tests for the `NewlineBeforeEndstream` toggle (flpdf-9hc.12.6).
//!
//! Covers:
//!   (a) Yes: `endstream` is preceded by exactly one `\n`; `/Length` excludes it.
//!       Tested with both an EOL-terminating payload and a non-EOL payload to
//!       confirm unconditional insertion.
//!   (b) No + payload ends with EOL: `endstream` immediately follows payload
//!       (no extra newline inserted).
//!   (c) No + payload does NOT end with EOL: exactly one `\n` is inserted for
//!       ISO 32000-1 parseability.
//!   (d) Both modes: write a minimal PDF, re-open it, and verify stream data
//!       round-trips correctly.
//!   (e) ObjStm container and xref stream paths also apply Yes-mode consistently.
//!   (f) QDF: the writer promotes only `Never` to `No` internally so
//!       `endstream` is line-anchored; explicit `Yes` and `No` pass through
//!       unchanged. `No` skips the added `\n` only when the payload ends in
//!       exactly `\n` (matches qpdf's `(last_char != '\n')` check in
//!       QPDFWriter.cc:1560 — bare-`\r` and `\r\n` endings still receive an
//!       added `\n`, and explicit `Yes` always adds one regardless of the
//!       payload's last byte).
//!
//! Unit tests (a–c) exercise `write_stream_to_buf` directly so they need no
//! on-disk fixture.  Integration tests (d–f) use `write_pdf_with_options`.
//! End-to-end / CLI matrix tests are the responsibility of flpdf-9hc.12.8.

use flpdf::{
    write_pdf_with_options, write_qdf, write_stream_to_buf, Dictionary, NewlineBeforeEndstream,
    Object, Pdf, Stream, WriteOptions,
};
use std::io::Cursor;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a `Stream` with optional `/Filter` and the given raw `data`.
/// `/Length` is set to `data.len()` (raw payload length, not including any
/// newline that `write_stream_to_buf` may insert before `endstream`).
fn make_stream(filter: Option<&[u8]>, data: Vec<u8>) -> Stream {
    let mut dict = Dictionary::new();
    if let Some(f) = filter {
        dict.insert("Filter", Object::Name(f.to_vec()));
    }
    dict.insert(
        "Length",
        Object::Integer(i64::try_from(data.len()).unwrap()),
    );
    Stream::new(dict, data)
}

/// Search `buf` for the last occurrence of `needle` and return its start index.
fn rfind(buf: &[u8], needle: &[u8]) -> Option<usize> {
    buf.windows(needle.len()).rposition(|w| w == needle)
}

/// Build a minimal valid PDF (1.4, xref table) with one content stream.
///
/// Returns `(pdf_bytes, raw_payload)`.
/// The content stream is written with raw (unfiltered) payload so that
/// `write_pdf_with_options` with `compress_streams = No` can round-trip it.
fn build_minimal_pdf(payload: &[u8]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();

    // The Catalog references the stream (obj 3) via /Metadata so it stays
    // reachable from /Root and survives the writer's Catalog-first reachability
    // walk (which drops objects unreachable from /Root).
    let cat_offset = bytes.len();
    bytes
        .extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Metadata 3 0 R >>\nendobj\n");

    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

    let stream_offset = bytes.len();
    bytes.extend_from_slice(
        format!("3 0 obj\n<< /Length {} >>\nstream\n", payload.len()).as_bytes(),
    );
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 4\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{cat_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{stream_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    bytes
}

/// Resolve the content stream that the Catalog references via `/Metadata`.
///
/// Full-rewrite output is renumbered Catalog-first, so the stream's object
/// number is not stable; navigate by reference from `/Root` instead.
fn resolve_metadata_stream<R: std::io::Read + std::io::Seek>(pdf: &mut Pdf<R>) -> Stream {
    let root = pdf.root_ref().expect("output must have a /Root");
    let metadata_ref = match pdf.resolve(root).expect("resolve /Root") {
        Object::Dictionary(d) => match d.get("Metadata") {
            Some(Object::Reference(r)) => *r,
            other => panic!("Catalog /Metadata must be a reference, got {other:?}"),
        },
        other => panic!("/Root must be a dictionary, got {other:?}"),
    };
    match pdf.resolve(metadata_ref).expect("resolve /Metadata") {
        Object::Stream(s) => s,
        other => panic!("/Metadata must be a stream, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// (a) Yes mode: exactly one \n before endstream, unconditionally
// ---------------------------------------------------------------------------

/// Yes mode, payload does NOT end with \n — must still insert exactly one \n.
#[test]
fn yes_inserts_newline_when_payload_has_no_trailing_eol() {
    let payload = b"no trailing newline".to_vec();
    let stream = make_stream(None, payload.clone());

    let mut buf = Vec::new();
    write_stream_to_buf(&mut buf, &stream, NewlineBeforeEndstream::Yes);

    // Find endstream position.
    let es_pos = rfind(&buf, b"endstream").expect("endstream not found");
    assert!(
        es_pos >= 1,
        "endstream must be preceded by at least one byte"
    );
    assert_eq!(
        buf[es_pos - 1],
        b'\n',
        "Yes mode: byte before endstream must be \\n (no trailing EOL case)"
    );

    // /Length in the dict must equal payload.len(), not payload.len() + 1.
    let declared_len = match stream.dict.get("Length") {
        Some(Object::Integer(n)) => *n as usize,
        other => panic!("unexpected /Length: {other:?}"),
    };
    assert_eq!(
        declared_len,
        payload.len(),
        "/Length must equal raw payload length, not include the inserted newline"
    );
}

/// Yes mode, payload DOES end with \n — must insert one more \n (no dedup).
#[test]
fn yes_inserts_newline_even_when_payload_already_ends_with_eol() {
    let payload = b"payload with trailing newline\n".to_vec();
    let stream = make_stream(None, payload.clone());

    let mut buf = Vec::new();
    write_stream_to_buf(&mut buf, &stream, NewlineBeforeEndstream::Yes);

    let es_pos = rfind(&buf, b"endstream").expect("endstream not found");
    assert!(es_pos >= 2, "need at least 2 bytes before endstream");
    // With Yes mode: payload already ends with \n, and we insert one more.
    // So buf[es_pos-1] == '\n' (inserted) and buf[es_pos-2] == '\n' (payload).
    assert_eq!(
        buf[es_pos - 1],
        b'\n',
        "Yes mode: byte immediately before endstream must be \\n (trailing-EOL payload case)"
    );
    assert_eq!(
        buf[es_pos - 2],
        b'\n',
        "Yes mode: payload's own trailing \\n must still be present at es_pos-2"
    );

    let declared_len = match stream.dict.get("Length") {
        Some(Object::Integer(n)) => *n as usize,
        other => panic!("unexpected /Length: {other:?}"),
    };
    assert_eq!(
        declared_len,
        payload.len(),
        "/Length must equal raw payload length (not include inserted newline)"
    );
}

// ---------------------------------------------------------------------------
// (b) No mode: payload ends with EOL — no extra newline (adjacency)
// ---------------------------------------------------------------------------

#[test]
fn no_does_not_insert_newline_when_payload_ends_with_lf() {
    let payload = b"payload ends with lf\n".to_vec();
    let stream = make_stream(None, payload.clone());

    let mut buf = Vec::new();
    write_stream_to_buf(&mut buf, &stream, NewlineBeforeEndstream::No);

    let es_pos = rfind(&buf, b"endstream").expect("endstream not found");
    // In No mode when payload already ends with \n, endstream is adjacent:
    // buf ends with ...\n<endstream>
    // The \n at es_pos-1 comes from the payload itself, not an inserted one.
    // Verify the byte at es_pos-1 is \n (the payload's last byte).
    assert_eq!(
        buf[es_pos - 1],
        b'\n',
        "No mode (LF tail): endstream must immediately follow payload's trailing \\n"
    );
    // And es_pos-2 must NOT be \n (no double newline inserted).
    let payload_without_lf = &payload[..payload.len() - 1];
    if !payload_without_lf.is_empty() {
        assert_ne!(
            buf[es_pos - 2],
            b'\n',
            "No mode (LF tail): must not insert an extra \\n before the payload's trailing \\n"
        );
    }
}

#[test]
fn no_inserts_newline_when_payload_ends_with_bare_cr() {
    // qpdf's --qdf logic (`QPDFWriter.cc:1560`) only skips the added `\n`
    // when the payload's last byte is exactly `\n`. Bare `\r` (and `\r\n`)
    // endings still receive an added `\n` before `endstream`, so
    // `endstream` remains line-anchored.
    let payload = b"payload ends with cr\r".to_vec();
    let stream = make_stream(None, payload.clone());

    let mut buf = Vec::new();
    write_stream_to_buf(&mut buf, &stream, NewlineBeforeEndstream::No);

    let es_pos = rfind(&buf, b"endstream").expect("endstream not found");
    // No mode + bare-CR tail: an added `\n` sits between the payload and
    // `endstream`, so byte before `endstream` is `\n` and the previous
    // byte is the payload's `\r`.
    assert_eq!(
        buf[es_pos - 1],
        b'\n',
        "No mode (bare-CR tail): flpdf must insert `\\n` before `endstream` \
         (qpdf `(last_char != '\\n')` in QPDFWriter.cc:1560)"
    );
    assert_eq!(
        buf[es_pos - 2],
        b'\r',
        "No mode (bare-CR tail): the payload's trailing `\\r` sits before the added `\\n`"
    );
}

// ---------------------------------------------------------------------------
// (c) No mode: payload does NOT end with EOL — minimal \n inserted
// ---------------------------------------------------------------------------

#[test]
fn no_inserts_minimal_newline_when_payload_has_no_trailing_eol() {
    let payload = b"no eol at end".to_vec();
    let stream = make_stream(None, payload.clone());

    let mut buf = Vec::new();
    write_stream_to_buf(&mut buf, &stream, NewlineBeforeEndstream::No);

    let es_pos = rfind(&buf, b"endstream").expect("endstream not found");
    assert!(es_pos >= 1, "need at least 1 byte before endstream");
    assert_eq!(
        buf[es_pos - 1],
        b'\n',
        "No mode (no EOL tail): must insert exactly one \\n for parseability"
    );
    // The byte before the newline must be the payload's last byte ('d' from "end").
    assert_eq!(
        buf[es_pos - 2],
        payload[payload.len() - 1],
        "No mode: the \\n must directly follow the payload's last byte"
    );
}

// ---------------------------------------------------------------------------
// (c2) Never mode: endstream is always adjacent — no newline ever inserted
//      (matches qpdf's default output: exactly /Length bytes then endstream).
// ---------------------------------------------------------------------------

#[test]
fn never_does_not_insert_newline_for_non_eol_payload() {
    let payload = b"binary-ish tail \xe0".to_vec();
    let stream = make_stream(None, payload.clone());

    let mut buf = Vec::new();
    write_stream_to_buf(&mut buf, &stream, NewlineBeforeEndstream::Never);

    let es_pos = rfind(&buf, b"endstream").expect("endstream not found");
    // endstream immediately follows the payload's last byte; no \n inserted.
    assert_eq!(
        buf[es_pos - 1],
        payload[payload.len() - 1],
        "Never mode: endstream must immediately follow the payload's last byte"
    );
    // Exactly `Length` bytes sit between `stream\n` and `endstream`.
    let stream_kw = rfind(&buf[..es_pos], b"stream\n").expect("stream keyword");
    assert_eq!(
        es_pos - (stream_kw + b"stream\n".len()),
        payload.len(),
        "Never mode: exactly /Length bytes between stream and endstream (no added EOL)"
    );
}

#[test]
fn never_does_not_add_extra_newline_for_eol_payload() {
    let payload = b"payload ends with lf\n".to_vec();
    let stream = make_stream(None, payload.clone());

    let mut buf = Vec::new();
    write_stream_to_buf(&mut buf, &stream, NewlineBeforeEndstream::Never);

    let es_pos = rfind(&buf, b"endstream").expect("endstream not found");
    let stream_kw = rfind(&buf[..es_pos], b"stream\n").expect("stream keyword");
    assert_eq!(
        es_pos - (stream_kw + b"stream\n".len()),
        payload.len(),
        "Never mode: payload written verbatim, no extra EOL even when it ends with \\n"
    );
}

// ---------------------------------------------------------------------------
// (d) Round-trip: write PDF with each mode, re-open, verify stream data
// ---------------------------------------------------------------------------

fn run_round_trip_test(policy: NewlineBeforeEndstream) {
    let raw: &[u8] = b"Round-trip payload for newline_before_endstream test.";
    let source = build_minimal_pdf(raw);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.compress_streams = flpdf::CompressStreams::No; // keep data unmodified
    options.newline_before_endstream = policy;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Re-open the rewritten PDF. Output is renumbered Catalog-first, so
    // navigate to the stream via the Catalog's /Metadata reference.
    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    let stream = resolve_metadata_stream(&mut reopened);

    // Verify the payload is intact.
    assert_eq!(
        stream.data.as_slice(),
        raw,
        "round-trip ({policy:?}): stream data must equal original payload"
    );
}

#[test]
fn round_trip_yes_mode() {
    run_round_trip_test(NewlineBeforeEndstream::Yes);
}

#[test]
fn round_trip_no_mode() {
    run_round_trip_test(NewlineBeforeEndstream::No);
}

/// `Never` writes the payload with `endstream` immediately adjacent (no EOL).
/// flpdf must be able to re-open its own no-EOL-before-endstream output — the
/// reader has to rely on `/Length` and skip the *optional* whitespace before
/// `endstream`. (Round-trip payload does not end in a newline, so this exercises
/// the adjacent-endstream parse path that `Yes`/`No` never produce here.)
#[test]
fn round_trip_never_mode() {
    run_round_trip_test(NewlineBeforeEndstream::Never);
}

/// qdf keeps `endstream` line-anchored regardless of the caller's
/// `newline_before_endstream` policy. QDF form is designed for human editing
/// and requires `endstream` on its own line so `flpdf::fix_qdf` (and qpdf's
/// `fix-qdf`) can locate it; the writer therefore promotes `Never` to `No`
/// internally when `options.qdf` is true. Under `No`, a payload that does not
/// end in EOL gets exactly one framing `\n` before `endstream`; a payload
/// that already ends in EOL takes no additional byte. This asserts the
/// *emitted* bytes for a non-EOL payload: exactly one `\n` sits between the
/// payload and `endstream`, and the dict carries an indirect `/Length`.
/// (Re-opening QDF + `Never` output is a separate code path and is not
/// covered here.)
#[test]
fn qdf_wraps_endstream_on_own_line_regardless_of_caller_policy() {
    // QDF form is designed for human editing and requires `endstream` on its own
    // line so `flpdf::fix_qdf` (like qpdf's `fix-qdf`) can find it. The writer
    // therefore promotes `Never` to `No` internally when `options.qdf` is true,
    // matching qpdf --qdf: a payload that does not end in EOL gets exactly one
    // framing `\n` before `endstream`, so the endstream keyword is line-anchored.
    // Payloads that already end in EOL take no additional byte. The caller's
    // `Never` request is honoured to the extent the format allows: the output
    // is not corrupted.
    let raw: &[u8] = b"qdf never-mode payload";
    let source = build_minimal_pdf(raw);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;
    options.compress_streams = flpdf::CompressStreams::No;
    options.newline_before_endstream = NewlineBeforeEndstream::Never;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Line-anchored endstream: the payload is followed by `\nendstream`, NOT
    // adjacent to the payload. Under No-promotion for this non-EOL payload the
    // writer inserts exactly one framing `\n`.
    let newline_framed = b"qdf never-mode payload\nendstream";
    assert!(
        output
            .windows(newline_framed.len())
            .any(|w| w == newline_framed),
        "qdf keeps `\\nendstream` framing (line-anchored endstream)"
    );
    // QDF splits /Length into an indirect length-holder: `/Length <N> 0 R`.
    // Match that exact shape so an unrelated `N 0 R` (e.g. `/Root 1 0 R`) can't
    // satisfy the assertion.
    let has_indirect_length = output.windows(b"/Length ".len()).enumerate().any(|(i, w)| {
        if w != b"/Length " {
            return false;
        }
        let tail = &output[i + b"/Length ".len()..];
        let digits = tail.iter().take_while(|b| b.is_ascii_digit()).count();
        digits > 0 && tail.get(digits..digits + 4) == Some(&b" 0 R"[..])
    });
    assert!(
        has_indirect_length,
        "qdf must emit an indirect /Length holder (`/Length <N> 0 R`)"
    );
}

/// Re-open a qdf full-rewrite and return the content stream's data for the given
/// raw payload and newline policy. qdf splits `/Length` into an indirect holder,
/// exercising the indirect-`/Length` reader path.
fn round_trip_qdf(raw: &[u8], policy: NewlineBeforeEndstream) -> Vec<u8> {
    let source = build_minimal_pdf(raw);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;
    options.compress_streams = flpdf::CompressStreams::No;
    options.newline_before_endstream = policy;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    resolve_metadata_stream(&mut reopened).data
}

/// qdf + `Never` must RE-OPEN, not just write. The payload is written with
/// `endstream` immediately adjacent (no EOL) and an indirect `/Length` holder
/// whose body is the exact content length. On re-open the byte-level parser
/// cannot line-anchor the adjacent `endstream`, so the reader resolves the
/// holder authoritatively and re-slices. (The payload deliberately ends in `.`,
/// not an EOL, so it takes the adjacent-endstream path; it also contains the
/// substring "endstream", proving the fix is not a naive keyword scan.)
#[test]
fn round_trip_qdf_never_mode() {
    let raw: &[u8] = b"Round-trip payload for newline_before_endstream test.";
    assert_eq!(
        round_trip_qdf(raw, NewlineBeforeEndstream::Never).as_slice(),
        raw,
        "qdf+Never must re-open with the original payload intact"
    );
}

/// Regression guard: the [`write_qdf`] convenience wrapper must go through
/// the same `write_pdf_with_options` promotion that upgrades `Never` to `No`
/// for QDF form. If a future refactor gives `write_qdf` its own emission path
/// that bypasses the promotion, the output would carry adjacent `endstream`
/// framing for non-EOL-ending payloads and `flpdf::fix_qdf` / qpdf's `fix-qdf`
/// would fail on hand edits. The library default `NewlineBeforeEndstream::Never`
/// makes this the exact state the caller reaches with `WriteOptions::default()`.
/// The payload deliberately does not end in `\n`, so under `No` the writer
/// still inserts exactly one framing `\n` before `endstream`.
#[test]
fn write_qdf_wrapper_respects_no_promotion_via_public_api() {
    let raw: &[u8] = b"write_qdf wrapper payload";
    let source = build_minimal_pdf(raw);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut output = Vec::new();
    write_qdf(&mut pdf, &mut output).unwrap();

    let framed = b"write_qdf wrapper payload\nendstream";
    assert!(
        output.windows(framed.len()).any(|w| w == framed),
        "write_qdf must emit `\\nendstream` framing (single `\\n`) regardless of \
         the library default (`Never`) — the QDF No-promotion applies to all \
         public entry points"
    );
}

/// QDF form promotes the caller's `Never` to `No` (see
/// `qdf_wraps_endstream_on_own_line_regardless_of_caller_policy` above),
/// matching
/// qpdf's `--qdf` behaviour: when the payload already ends with `\n`/`\r`,
/// no additional framing byte is inserted, and `/Length` equals the raw
/// payload length. Round-trip characterization: for a payload ending in EOL,
/// the on-disk bytes `abc\nendstream` are ambiguous — they could come from
/// payload `abc` (No policy adds framing `\n`) OR from payload `abc\n` (No
/// policy adds nothing). The reader resolves this ambiguity in QDF mode by
/// keeping the parser's endstream-scan value (strips one framing EOL), so
/// the trailing byte is dropped on re-open. This mirrors qpdf's --qdf
/// re-read behaviour and preserves the pinned QDF idempotence invariant.
#[test]
fn round_trip_qdf_eol_ending_payload_drops_trailing_eol_via_no_promotion() {
    let raw: &[u8] = b"abc\n";
    assert_eq!(
        round_trip_qdf(raw, NewlineBeforeEndstream::Never).as_slice(),
        b"abc",
        "QDF promotes Never to No, so an EOL-ending payload loses its \
         trailing EOL on re-open (the pre-endstream framing byte and the \
         payload's trailing EOL are indistinguishable on disk); matches qpdf"
    );
}

/// A payload ending in bare `\r` under QDF must receive an added framing
/// `\n` before `endstream` — qpdf `--qdf` `(last_char != '\n')` covers only
/// `\n`. Verified against the emitted bytes: the stream body ends
/// `...\r\nendstream`, NOT `...\rendstream`.
#[test]
fn qdf_bare_cr_payload_gets_added_newline() {
    let raw: &[u8] = b"payload with bare cr\r";
    let source = build_minimal_pdf(raw);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;
    options.compress_streams = flpdf::CompressStreams::No;
    // Never → No promotion inside QDF; this exercises the No branch that
    // adds `\n` when the payload does not end in exactly `\n`.
    options.newline_before_endstream = NewlineBeforeEndstream::Never;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    let framed = b"payload with bare cr\r\nendstream";
    assert!(
        output.windows(framed.len()).any(|w| w == framed),
        "QDF+No must add `\\n` after bare `\\r` payload — expected `...\\r\\nendstream`"
    );
}

/// A caller who explicitly requests `Yes` under QDF must get the `Yes`
/// framing — an added `\n` before `endstream` even when the payload already
/// ends with `\n`. Verifies the override only promotes `Never`, never
/// downgrading an explicit `Yes` to `No`.
#[test]
fn qdf_honors_explicit_newline_before_endstream_yes() {
    // Payload already ends with `\n`; `Yes` forces an ADDITIONAL `\n`.
    let raw: &[u8] = b"already lf-ending payload\n";
    let source = build_minimal_pdf(raw);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;
    options.compress_streams = flpdf::CompressStreams::No;
    options.newline_before_endstream = NewlineBeforeEndstream::Yes;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    let framed = b"already lf-ending payload\n\nendstream";
    assert!(
        output.windows(framed.len()).any(|w| w == framed),
        "QDF must honour explicit Yes: payload ends `\\n`, Yes adds a second \
         `\\n`, so `...\\n\\nendstream` must appear in the output"
    );
}

// ---------------------------------------------------------------------------
// Verify /Length excludes the inserted newline in E2E output (raw bytes check)
// ---------------------------------------------------------------------------

/// Write a PDF in Yes mode and verify that in the raw bytes the sequence
/// `endstream` is preceded by exactly one `\n`, and that the /Length
/// value recorded in the stream dict equals the payload length (not +1).
#[test]
fn e2e_yes_mode_endstream_preceded_by_exactly_one_newline_and_length_correct() {
    let raw: &[u8] = b"payload for length check";
    let source = build_minimal_pdf(raw);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.compress_streams = flpdf::CompressStreams::No;
    options.newline_before_endstream = NewlineBeforeEndstream::Yes;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Find all occurrences of `endstream` and check each one.
    let mut pos = 0;
    let mut found = false;
    while let Some(rel) = output[pos..]
        .windows(b"endstream".len())
        .position(|w| w == b"endstream")
    {
        let abs = pos + rel;
        if abs >= 1 {
            assert_eq!(
                output[abs - 1],
                b'\n',
                "Yes mode e2e: every endstream must be preceded by \\n (position {abs})"
            );
            found = true;
        }
        pos = abs + 1;
    }
    assert!(
        found,
        "at least one endstream must be present in the output"
    );

    // Verify /Length in the stream dict via reader. Output is renumbered
    // Catalog-first, so navigate via the Catalog's /Metadata reference.
    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    let stream = resolve_metadata_stream(&mut reopened);
    let declared_len = match stream.dict.get("Length") {
        Some(Object::Integer(n)) => *n as usize,
        other => panic!("unexpected /Length: {other:?}"),
    };
    assert_eq!(
        declared_len,
        raw.len(),
        "/Length must equal raw payload length, not include the inserted newline"
    );
}

// ---------------------------------------------------------------------------
// (e) ObjStm container path: Yes mode applies consistently
// ---------------------------------------------------------------------------

/// Build a minimal PDF-1.5 with one object stream (ObjStm) and verify that
/// in a full-rewrite with `NewlineBeforeEndstream::Yes`, every `endstream`
/// in the output is preceded by `\n`.
#[test]
fn e2e_objstm_path_yes_mode_all_endstreams_preceded_by_newline() {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    // Build a minimal PDF-1.5 with one ObjStm (same fixture as object_streams tests).
    let pages_bytes: &[u8] = b"<< /Type /Pages /Count 0 /Kids [] >>";
    // Build uncompressed ObjStm payload.
    let header = b"2 0\n";
    let mut body = pages_bytes.to_vec();
    body.push(b'\n');
    let mut payload = header.to_vec();
    payload.extend_from_slice(&body);
    let first = header.len();

    // Compress with FlateDecode.
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&payload).unwrap();
    let compressed = enc.finish().unwrap();

    let mut bytes = b"%PDF-1.5\n".to_vec();

    let cat_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let objstm_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Length {} /Filter /FlateDecode >>\nstream\n",
            compressed.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&compressed);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    // XRef stream (W=[1 3 1]).
    fn append_entry(v: &mut Vec<u8>, t: u8, f1: u32, f2: u8) {
        v.push(t);
        let b = f1.to_be_bytes();
        v.extend_from_slice(&b[1..]);
        v.push(f2);
    }
    let xref_offset = bytes.len();
    let mut xe: Vec<u8> = Vec::new();
    append_entry(&mut xe, 0, 0, 0); // 0: free
    append_entry(&mut xe, 1, cat_offset as u32, 0); // 1: Catalog
    append_entry(&mut xe, 2, 3, 0); // 2: Pages in ObjStm 3, idx 0
    append_entry(&mut xe, 1, objstm_offset as u32, 0); // 3: ObjStm
    append_entry(&mut xe, 1, xref_offset as u32, 0); // 4: XRef (self)

    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /W [1 3 1] /Index [0 5] /Length {} >>\nstream\n",
            xe.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xe);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.compress_streams = flpdf::CompressStreams::Yes;
    options.newline_before_endstream = NewlineBeforeEndstream::Yes;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Verify every `endstream` is preceded by `\n`.
    let mut pos = 0;
    let mut count = 0;
    while let Some(rel) = output[pos..]
        .windows(b"endstream".len())
        .position(|w| w == b"endstream")
    {
        let abs = pos + rel;
        assert!(abs >= 1, "endstream at offset {abs} has no preceding byte");
        assert_eq!(
            output[abs - 1],
            b'\n',
            "Yes mode (ObjStm path): endstream at offset {abs} must be preceded by \\n"
        );
        count += 1;
        pos = abs + 1;
    }
    assert!(
        count >= 2,
        "ObjStm output must have at least 2 endstream keywords (ObjStm + xref); got {count}"
    );
}

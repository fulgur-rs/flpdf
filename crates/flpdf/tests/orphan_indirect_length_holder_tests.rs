//! Behavioural coverage for dropping orphaned indirect `/Length` holders on the
//! non-linearized full-rewrite paths (flpdf-sqkq).
//!
//! These assertions are structural (object count, `/Length` form) rather than
//! byte-identical, so they run under the default Pure-Rust deflate — the
//! byte-for-byte parity against qpdf 11.9.0 lives in `cmp_diff_zero_tests` and
//! `cmp_generate_objstm_tests` (gated on `qpdf-zlib-compat`).
//!
//! Fixture `objstm-lin-od-indirect-length.pdf`: the catalog's `/OpenAction`
//! reaches a JavaScript action whose `/JS` stream (obj 6) carries an INDIRECT
//! `/Length` (`7 0 R`). The holder (obj 7) is reachable ONLY through that
//! `/Length` edge, so it orphans once `/Length` is normalized to a direct
//! integer — and qpdf garbage-collects it.

use std::io::Cursor;

use flpdf::{
    write_pdf_with_options, NewlineBeforeEndstream, Object, ObjectStreamMode, Pdf, StreamDataMode,
    WriteOptions,
};

const FIXTURE: &[u8] =
    include_bytes!("../../../tests/fixtures/compat/objstm-lin-od-indirect-length.pdf");

/// Full-rewrite the fixture with `opts` and return the output bytes.
fn rewrite(opts: &WriteOptions) -> Vec<u8> {
    let mut pdf = Pdf::open(Cursor::new(FIXTURE)).expect("open fixture");
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, opts).expect("write");
    out
}

/// Resolve the JavaScript stream (catalog `/OpenAction` -> action `/JS`) in
/// `out` and return its `/Length` dictionary entry.
fn js_stream_length(out: &[u8]) -> Object {
    let mut pdf = Pdf::open(Cursor::new(out.to_vec())).expect("re-open output");
    let root = pdf.root_ref().expect("/Root");
    let catalog = pdf.resolve(root).expect("catalog");
    let open_action = catalog
        .as_dict()
        .and_then(|d| d.get("OpenAction").cloned())
        .expect("/OpenAction");
    let action = match open_action {
        Object::Reference(r) => pdf.resolve(r).expect("action"),
        other => other,
    };
    let js_ref = action
        .as_dict()
        .and_then(|d| d.get("JS").cloned())
        .expect("/JS");
    let js = match js_ref {
        Object::Reference(r) => pdf.resolve(r).expect("js stream"),
        other => other,
    };
    js.as_stream()
        .expect("/JS is a stream")
        .dict
        .get("Length")
        .cloned()
        .expect("/Length present")
}

/// Number of live indirect objects after re-opening `out` (excludes the free
/// `0` head and any deleted/missing slots).
fn object_count(out: &[u8]) -> usize {
    let pdf = Pdf::open(Cursor::new(out.to_vec())).expect("re-open output");
    pdf.live_object_refs().len()
}

/// Base option set: a `--static-id` full rewrite with qpdf's default
/// no-newline-before-endstream framing.
fn base_opts() -> WriteOptions {
    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.static_id = true;
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;
    opts
}

#[test]
fn plain_rewrite_drops_orphan_holder_and_directizes_length() {
    // Default full rewrite: compress=Yes, so `/Length` is direct-ized and the
    // gate (`effective_stream_policy` is Some, not qdf) drops the holder.
    let out = rewrite(&base_opts());

    // Six live objects (Catalog, Pages, Page, content stream, Action, JS stream)
    // — the orphaned holder (originally obj 7) is gone.
    assert_eq!(
        object_count(&out),
        6,
        "orphan /Length holder must be dropped"
    );
    assert!(
        matches!(js_stream_length(&out), Object::Integer(_)),
        "the JS stream's /Length must be direct-ized once the holder is dropped"
    );
}

#[test]
fn generate_drops_orphan_holder_and_directizes_length() {
    let mut opts = base_opts();
    opts.object_streams = ObjectStreamMode::Generate;
    let out = rewrite(&opts);

    assert!(
        matches!(js_stream_length(&out), Object::Integer(_)),
        "generate path must direct-ize /Length and drop the holder"
    );
}

#[test]
fn stream_data_preserve_drops_orphan_holder_and_directizes_length() {
    // `--stream-data=preserve` keeps stream bytes verbatim, but qpdf still
    // normalizes every stream's /Length to a direct integer and garbage-collects
    // the now-orphaned indirect holder. flpdf must match (flpdf-3g8o): the
    // orphan-drop gate fires for every non-qdf mode, not only when streams are
    // recompressed.
    let mut opts = base_opts();
    opts.stream_data = Some(StreamDataMode::Preserve);
    let out = rewrite(&opts);

    // Six live objects (Catalog, Pages, Page, content stream, Action, JS stream)
    // — the orphaned holder (originally obj 7) is gone, just as in the compress
    // and generate paths.
    assert_eq!(
        object_count(&out),
        6,
        "preserve mode must drop the orphaned indirect /Length holder"
    );
    assert!(
        matches!(js_stream_length(&out), Object::Integer(_)),
        "preserve mode must direct-ize the JS stream's /Length once the holder is dropped"
    );
}

/// Assemble a minimal classic (table-xref) PDF from `(object_number, body)`
/// pairs plus a literal `trailer_dict` body (without the surrounding `<< >>`).
fn build_raw_pdf(bodies: &[(u32, &[u8])], trailer_dict: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let max_num = bodies.iter().map(|(n, _)| *n).max().unwrap_or(0);
    let size = max_num + 1;
    let mut offsets = vec![0usize; size as usize];
    for (num, body) in bodies {
        offsets[*num as usize] = out.len();
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for off in offsets.iter().skip(1) {
        if *off == 0 {
            out.extend_from_slice(b"0000000000 65535 f \n");
        } else {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
    }
    out.extend_from_slice(b"trailer\n<< ");
    out.extend_from_slice(trailer_dict);
    out.extend_from_slice(b" >>\n");
    out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

#[test]
fn plain_rewrite_keeps_length_holder_referenced_from_direct_trailer_dict() {
    // flpdf-jnq4 / Codex P2: obj 5 is an indirect `/Length` holder for the page
    // /Contents stream (obj 4) AND is referenced via a nested ref inside a DIRECT
    // `/Info << /Held 5 0 R >>` trailer dict. qpdf's `enqueueObjectsStandard`
    // recurses into direct trailer values, so it numbers and KEEPS obj 5, and
    // rewrites the nested ref to the holder's new number. flpdf must match: with
    // the /Length edge skipped, the holder would otherwise be dropped and the
    // trailer would emit a dangling `/Held` reference.
    let pdf_bytes = build_raw_pdf(
        &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Count 1 /Kids [ 3 0 R ] >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
            ),
            (
                4,
                b"<< /Length 5 0 R >>\nstream\napp.alert('hi');\nendstream",
            ),
            (5, b"16"),
        ],
        b"/Size 6 /Root 1 0 R /Info << /Held 5 0 R >>",
    );

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &base_opts()).expect("write");

    // The holder is kept (referenced from the trailer), so the live set is the 5
    // graph objects — none orphaned, no dangling trailer reference.
    let mut re = Pdf::open(Cursor::new(out)).expect("re-open output");
    let trailer_held = re
        .trailer()
        .get("Info")
        .and_then(|info| info.as_dict())
        .and_then(|d| d.get("Held"))
        .cloned()
        .expect("/Info /Held present");
    let held_ref = match trailer_held {
        Object::Reference(r) => r,
        other => panic!("/Held must stay an indirect reference, got {other:?}"),
    };
    // The reference must resolve to the holder integer in the OUTPUT — i.e. it was
    // renumbered to a live object, not left dangling at a freed/wrong slot.
    let resolved = re.resolve(held_ref).expect("/Held target resolves");
    assert_eq!(
        resolved,
        Object::Integer(16),
        "the trailer /Held nested ref must point to the kept holder (value 16), not dangle"
    );
}

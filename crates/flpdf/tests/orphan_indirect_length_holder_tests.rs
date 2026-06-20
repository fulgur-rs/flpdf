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
fn stream_data_preserve_keeps_indirect_length_and_holder() {
    // `--stream-data=preserve` leaves `/Length` indirect, so the holder is still
    // live: the gate's `effective_stream_policy` is None and nothing is dropped.
    let mut opts = base_opts();
    opts.stream_data = Some(StreamDataMode::Preserve);
    let out = rewrite(&opts);

    // All seven objects survive, and the JS stream still references its holder.
    assert_eq!(
        object_count(&out),
        7,
        "preserve mode keeps the live indirect /Length holder"
    );
    assert!(
        matches!(js_stream_length(&out), Object::Reference(_)),
        "preserve mode must keep the indirect /Length reference"
    );
}

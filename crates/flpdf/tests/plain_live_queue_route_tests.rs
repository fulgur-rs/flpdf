use flpdf::{ObjectStreamMode, Pdf, PdfWriter};
use std::io::Cursor;
use std::path::Path;

#[test]
fn plain_disable_uses_the_live_queue_consumer() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let plain = std::fs::read_to_string(root.join("writer/plain/mod.rs")).unwrap();
    let body = std::fs::read_to_string(root.join("writer/plain/body.rs")).unwrap();

    assert!(plain.contains("write_plain_live_disable"));
    assert!(body.contains("struct LiveQueue"));
    assert!(body.contains("emit_live_disable"));
    assert!(body.contains("enqueue_handle"));
}

#[test]
fn planned_preserve_uses_the_d9_source_membership_owner() {
    // qpdf has one source-membership owner, `getObjectStreamData`
    // (`QPDF.cc:2381-2390`); Preserve's early-return decision consumes that
    // map inside `preserveObjectStreams` (`QPDFWriter.cc:1939-1967`). The
    // planned consumer must not rederive the same fact from a second raw-xref
    // predicate.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let plan = std::fs::read_to_string(root.join("writer/plain/plan.rs")).unwrap();

    assert!(!plan.contains("source_has_compressed_entries"));
    assert!(plan.contains("source_object_stream_data"));
    assert!(plan.contains("get_object_stream_data"));
}

#[test]
fn preserve_without_source_objstm_selects_the_disable_shaped_live_consumer() {
    // qpdf returns from `preserveObjectStreams` when its source ObjStm map is
    // empty (`QPDFWriter.cc:1939-1945`), leaving the same enqueue/writeStandard
    // shape as Disable. The byte differential for the corresponding xref
    // stream fixture lives in `cmp_diff_zero_tests`; this contract keeps the
    // intended Preserve branch connected to that live consumer.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let plain = std::fs::read_to_string(root.join("writer/plain/mod.rs"))
        .unwrap()
        .replace("\r\n", "\n");

    // Pin the whole routing region as one normalized expression rather than
    // a set of substring probes. Every narrower form reviewed here could be
    // stepped around by a mutation just outside the compared slice: a
    // conjunct appended to the binding, an extra condition on the guard, or
    // the return wrapped in a nested `if`. Whitespace is collapsed so a
    // rustfmt reflow is tolerated while any added, removed, or reordered
    // token fails. The structural alternative -- a route-classification
    // helper asserted on input combinations -- is tracked separately.
    let write_plain = plain
        .split_once("pub(crate) fn write_plain<")
        .and_then(|(_, rest)| rest.split_once("\n}\n"))
        .map(|(body, _)| body)
        .expect("write_plain body");
    let region = write_plain
        .split_once("    let is_live_disable_shaped =")
        .map(|(_, rest)| rest)
        .expect("is_live_disable_shaped binding");
    let (before_branch, branch) = region
        .split_once("if is_live_disable_shaped")
        .expect("is_live_disable_shaped branch");
    let branch = branch
        .split_once("\n    }")
        .map(|(body, _)| body)
        .expect("branch body");
    let region = format!(
        "let is_live_disable_shaped ={before_branch}if is_live_disable_shaped{branch}\n    }}"
    );
    let region = region.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(
        region,
        "let is_live_disable_shaped = matches!( options.object_streams, \
         ObjectStreamMode::Disable | ObjectStreamMode::Preserve ); if \
         qdf_or_normalize_live_eligible(options, source_object_stream_data) { let \
         (page_sequences, contents_sequences, content_container_sequences) = \
         live_page_context(pdf, special_streams)?; return write_plain_live( pdf, out, options, \
         generated_id, source_object_stream_data, page_sequences, contents_sequences, \
         content_container_sequences, ); } if is_live_disable_shaped && !options.qdf && \
         !options.content_normalization { return write_plain_live_disable( pdf, out, options, \
         generated_id, source_object_stream_data, ); }",
        "the plain route decision must stay exactly this shape; an empty-membership Preserve \
         rewrite has to reach write_plain_live_disable unconditionally"
    );

    let eligible = plain
        .split_once("pub(crate) fn qdf_or_normalize_live_eligible")
        .and_then(|(_, rest)| rest.split_once(") -> bool {"))
        .and_then(|(_, rest)| rest.split_once("\n}"))
        .map(|(body, _)| body)
        .expect("qdf_or_normalize_live_eligible body");
    let eligible = eligible.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(
        eligible,
        "(options.qdf || options.content_normalization) && matches!( options.object_streams, \
         ObjectStreamMode::Disable | ObjectStreamMode::Preserve ) && (options.object_streams == \
         ObjectStreamMode::Disable || source_object_stream_data.is_empty())",
        "the QDF/normalize live route must stay restricted to QDF or normalization with an \
         empty source membership"
    );
}

#[test]
fn plain_disable_reconciles_direct_adbe_before_live_child_enqueue() {
    // qpdf 11.9.0: QPDFWriter.cc:1418-1430 replaces stale /ADBE before
    // unparseChild can enqueue the obsolete indirect /URL value.
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/adbe-orphan-url.pdf").to_vec(),
    ))
    .expect("open direct ADBE fixture");
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.force_pdf_version("1.7", 8);
    writer.set_static_id(true);
    writer.set_output_memory().expect("configure memory output");
    writer.write().expect("write live queue output");
    let output = writer.get_buffer().expect("read memory output");
    let expected = include_bytes!("../../../tests/fixtures/compat/golden/adbe-orphan-url.qpdf.pdf");
    assert_eq!(
        output, expected,
        "direct ADBE replacement must drop orphan URL"
    );
}

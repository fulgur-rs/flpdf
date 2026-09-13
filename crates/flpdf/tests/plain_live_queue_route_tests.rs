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

    // Bind each assertion to the construct it claims. Independent substring
    // checks over the whole module would still pass if Preserve were dropped
    // from the branch condition, because the same tokens appear in
    // `qdf_or_normalize_live_eligible`.
    let write_plain = plain
        .split_once("pub(crate) fn write_plain<")
        .and_then(|(_, rest)| rest.split_once("\n}\n"))
        .map(|(body, _)| body)
        .expect("write_plain body");
    let shaped = write_plain
        .split_once("let is_live_disable_shaped = matches!(")
        .and_then(|(_, rest)| rest.split_once(");"))
        .map(|(binding, _)| binding)
        .expect("is_live_disable_shaped binding");
    assert!(
        shaped.contains("ObjectStreamMode::Preserve"),
        "Preserve must be part of the Disable-shaped live condition"
    );
    // Pin the complete guard, not just its return. Asserting only the branch
    // body would still pass if a conjunct were added that routes empty-map
    // Preserve to the planner instead, which is the regression this contract
    // exists to catch.
    let guard = write_plain
        .split_once("\n    if is_live_disable_shaped")
        .and_then(|(_, rest)| rest.split_once(" {\n"))
        .map(|(condition, _)| condition.trim())
        .expect("is_live_disable_shaped guard");
    assert_eq!(
        guard, "&& !options.qdf && !options.content_normalization",
        "the Disable-shaped guard must split only on QDF/normalization; any \
         further condition would drop a Preserve case from the live consumer"
    );
    let branch = write_plain
        .split_once("if is_live_disable_shaped")
        .and_then(|(_, rest)| rest.split_once("\n    }"))
        .map(|(branch, _)| branch)
        .expect("is_live_disable_shaped branch");
    assert!(
        branch.contains("return write_plain_live_disable("),
        "the Disable-shaped branch must return the live disable consumer"
    );
    let eligible = plain
        .split_once("pub(crate) fn qdf_or_normalize_live_eligible")
        .and_then(|(_, rest)| rest.split_once("\n}\n"))
        .map(|(body, _)| body)
        .expect("qdf_or_normalize_live_eligible body");
    assert!(
        eligible.contains("source_object_stream_data.is_empty()"),
        "the QDF/normalize live route must key on the empty source membership"
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

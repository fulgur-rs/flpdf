//! Route contracts for the qpdf-shaped PCLm writer boundary.
//!
//! PCLm has no writer of its own: `QPDFWriter::enqueueObjectsPCLm` seeds the
//! same `object_queue` the standard seed fills, and the shared write loop
//! emits it (`libqpdf/QPDFWriter.cc:2999-3005`). These contracts therefore
//! read the seed functions out of the live-queue module.

fn production_source(source: &str) -> &str {
    source
        .split("\n#[cfg(test)]")
        .next()
        .expect("source must contain a test boundary")
}

/// The seed pass: `enqueue_object` plus both `enqueue_objects_*` functions and
/// the `initialize_live_queue` dispatch between them.
fn live_queue_seed_source(source: &str) -> &str {
    let start = source
        .find("\nfn enqueue_object<")
        .expect("live queue seed helper");
    let end = source[start..]
        .find("\nfn emit_live_body<")
        .expect("live queue seed end");
    &source[start..start + end]
}

#[test]
fn pclm_planning_and_emission_do_not_materialize_legacy_objects() {
    let seed = live_queue_seed_source(include_str!("../src/writer/plain/body.rs"));
    assert!(
        !seed.contains(".materialize()"),
        "PCLm seeding must walk live ObjectHandle values"
    );

    let pclm_writer = production_source(include_str!("../src/writer.rs"));
    assert!(
        !pclm_writer.contains(".materialize()"),
        "PCLm emission must not rebuild a legacy Object snapshot"
    );
}

#[test]
fn pclm_seed_uses_the_raw_qpdf_page_handle_route() {
    let seed = live_queue_seed_source(include_str!("../src/writer/plain/body.rs"));

    for forbidden in [
        ".resolve(",
        ".resolve_handle(",
        ".resolve_handle_ref(",
        ".is_null(",
    ] {
        assert!(
            !seed.contains(forbidden),
            "PCLm seeding retains legacy accessor route {forbidden}"
        );
    }
    let pclm_seed = seed
        .split_once("fn enqueue_objects_pclm<")
        .expect("PCLm seed")
        .1
        .split_once("fn initialize_live_queue<")
        .expect("live queue initializer")
        .0;
    assert!(
        pclm_seed.contains("PageDocumentHelper::new(pdf).get_all_pages()?"),
        "PCLm seeding must consume qpdf's repaired raw page-handle list"
    );
    assert!(
        !pclm_seed.contains("pages::page_refs") && !pclm_seed.contains("get_object_handle("),
        "PCLm seeding must not project a raw page handle through ObjectRef"
    );
}

/// qpdf seeds PCLm from `enqueueObjectsPCLm` and every other standard route
/// from `enqueueObjectsStandard`, choosing between them in `writeStandard`
/// (`libqpdf/QPDFWriter.cc:2999-3005`). There is no separate PCLm writer.
#[test]
fn pclm_shares_one_queue_with_the_standard_seed() {
    let writer = include_str!("../src/writer.rs");
    assert!(
        !writer.contains("fn write_pclm"),
        "PCLm must not own a second writer route"
    );

    let seed = live_queue_seed_source(include_str!("../src/writer/plain/body.rs"));
    assert!(
        seed.contains("fn enqueue_objects_pclm<") && seed.contains("fn enqueue_objects_standard<"),
        "both qpdf seed passes live with the one live queue"
    );
    let dispatch = seed
        .split_once("fn initialize_live_queue<")
        .expect("live queue initializer")
        .1;
    assert!(
        dispatch.contains("if options.pclm {")
            && dispatch.contains("enqueue_objects_pclm(&mut queue, pdf)?")
            && dispatch.contains("enqueue_objects_standard(&mut queue, pdf, options)?"),
        "the seed pass is selected once, where qpdf selects it"
    );
}

/// `enqueueObjectsPCLm` never consults `preserve_unreferenced_objects` and
/// seeds only `/Root` from the trimmed trailer, so the PCLm seed takes no
/// writer options at all (`libqpdf/QPDFWriter.cc:2927-2955`).
#[test]
fn pclm_seed_ignores_standard_only_trailer_and_preserve_policy() {
    let seed = live_queue_seed_source(include_str!("../src/writer/plain/body.rs"));
    let seed = seed
        .split_once("fn enqueue_objects_pclm<")
        .expect("PCLm seed")
        .1;
    let seed = seed
        .split_once("\nfn initialize_live_queue<")
        .expect("PCLm seed end")
        .0;
    assert!(
        !seed.contains("preserve_unreferenced_objects"),
        "qpdf's PCLm seed never preserves unreferenced objects"
    );
    assert!(
        !seed.contains("try_as_dictionary"),
        "qpdf's PCLm seed reads only /Root from the trailer"
    );
}

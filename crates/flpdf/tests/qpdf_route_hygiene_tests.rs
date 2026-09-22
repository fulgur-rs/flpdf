use std::fs;
use std::path::Path;

fn source_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read_source(path: impl AsRef<Path>) -> String {
    // Git's default autocrlf=true checkout converts source files to CRLF on
    // Windows; keep structural route guards independent of checkout EOLs.
    fs::read_to_string(source_root().join(path))
        .expect("read source")
        .replace("\r\n", "\n")
}

#[test]
fn dead_qpdf_routes_are_removed_and_canonical_owners_remain() {
    let keys = read_source("encryption/keys.rs");
    assert!(!keys.contains("fn per_object_key("));
    assert!(!keys.contains("#![allow(dead_code)]"));

    let standard = read_source("encryption/standard.rs");
    assert!(!standard.contains("keys::per_object_key"));
    let primitives = read_source("encryption/primitives.rs");
    assert!(primitives.contains("fn compute_data_key("));
    assert!(!read_source("encryption/state.rs").contains("fn compute_data_key("));
    assert!(!read_source("writer/encryption_state.rs").contains("fn compute_data_key("));

    assert!(
        !source_root().join("filters.rs").exists(),
        "qpdf-less filters.rs compatibility module remains"
    );
    assert!(read_source("object_handle.rs").contains("pub fn get_stream_data("));

    let reader = read_source("reader.rs");
    for dead in [
        "fn qtest_object_value_source_offset(",
        "fn qtest_array_item_source_offset(",
        "fn qtest_object_value_source_offsets(",
        "fn qtest_array_item_source_offsets(",
        "fn qtest_decode_parms_source_offset(",
        "fn source_stream_data_offset(",
    ] {
        assert!(
            !reader.contains(dead),
            "dead reader wrapper remains: {dead}"
        );
    }

    let tracked = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/qpdf-route-matrix/tracked-symbols.txt"),
    )
    .expect("read route symbol tracker");
    for dead in [
        "::per_object_key",
        "::decode_stream_data_with_limits",
        "::qtest_object_value_source_offset ",
        "::qtest_array_item_source_offset ",
        "::qtest_object_value_source_offsets",
        "::qtest_array_item_source_offsets",
        "::qtest_decode_parms_source_offset",
        "::read_window",
        "::resolution_fallbacks_remaining",
        "::MAX_RESOLUTION_FALLBACKS",
        "::parse_source_file_object_at",
    ] {
        assert!(
            !tracked.contains(dead),
            "dead route remains tracked: {dead}"
        );
    }
}

#[test]
fn stream_whole_buffer_bridges_are_removed_after_canonical_cutover() {
    assert!(
        !source_root().join("filters.rs").exists(),
        "legacy materialized stream bridge module remains"
    );

    let xref = read_source("xref.rs");
    assert!(
        !xref.contains("decode_stream_data_from_handle("),
        "xref stream decoding still enters the materialized compatibility route"
    );

    let emission = read_source("writer/object_streams/emission.rs");
    assert!(
        !emission.contains("filters::encode_stream_data_from_handle("),
        "ObjStm emission still enters the generic whole-buffer encoder"
    );
    assert!(
        emission.contains("use crate::stream_filter::encode_flate;")
            && emission.contains("encode_flate(&body.bytes)"),
        "ObjStm emission must retain qpdf's direct Flate stage"
    );
}

#[test]
fn canonical_xref_warnings_do_not_use_replay_or_deferred_bridges() {
    let engine = read_source("engine.rs");
    assert!(!engine.contains("replay_warnings("));
    assert!(!engine.contains("install_repair_diagnostics("));

    let resolver = read_source("reader/resolver.rs");
    assert!(!resolver.contains("replay_warnings("));
    assert!(!resolver.contains("defer_live_repair_diagnostics"));
    assert!(!resolver.contains("begin_deferred_repair_diagnostics"));

    let xref = read_source("xref.rs");
    assert!(!xref.contains("DeferredDiagnosticsGuard"));
}

#[test]
fn ownerless_xref_api_is_removed_in_favor_of_the_canonical_pdf_route() {
    let lib = read_source("lib.rs");
    assert!(!lib.contains("load_xref_and_trailer"));
    assert!(!lib.contains("LoadedXref"));

    let xref = read_source("xref.rs");
    assert!(!xref.contains("load_xref_and_trailer"));
    for dead in [
        "pub fn load_xref_and_trailer(",
        "pub fn load_xref_and_trailer_with_repair(",
        "pub fn load_xref_and_trailer_best_effort(",
        "pub struct LoadedXref",
        // The owner-less standalone loaders and the second parser
        // implementation they drove are removed: every xref route now runs
        // through the document's own `CanonicalTrailerOwner`.
        "fn load_xref_state_with_options",
        "fn load_xref_state_from_bytes",
        "Option<&dyn CanonicalTrailerOwner>",
        "struct XrefReadContext",
        "struct BootstrapCache",
        "struct BootstrapHandleDocument",
        "enum XrefReadContextSpec",
        "struct XrefDetachedHandles",
        "fn detach_bootstrap_handle",
        "fn parse_trailer_candidate",
    ] {
        assert!(
            !xref.contains(dead),
            "owner-less xref surface remains: {dead}"
        );
    }
}

#[test]
fn handle_only_helpers_do_not_carry_dead_pdf_parameters() {
    for path in [
        "filespec_helper/embedded_file_stream.rs",
        "nntree.rs",
        "page_annotation_flatten.rs",
        "page_object_helper.rs",
        "page_form_xobject.rs",
        "pages/repair.rs",
        "pages/tree_rebuild.rs",
        "resources.rs",
        "job/rotate.rs",
        // The CLI reaches the same handle-only routes, and the dirty bridge
        // left a dead parameter here too.
        "../../flpdf-cli/src/main.rs",
    ] {
        let source = read_source(path);
        assert!(
            !source.contains("_pdf: &mut Pdf"),
            "handle-only helper in {path} still carries a dead Pdf parameter"
        );
    }
}

/// `QPDF::readToken(input, max_len = 0)` (`libqpdf/QPDF.cc:1535-1539`) is one
/// function; every flpdf realization that used to construct its own
/// `Tokenizer` and call `.read_token(true, ...)` -- `ByteCursor::read_token`,
/// the trailer's `stream`-keyword lookahead, `next_object_stream_integer`'s
/// ObjStm header integers, the xref-reconstruction line scan, and the
/// canonical resolve path's `endstream`/`endobj` framing checks -- now routes
/// through `Tokenizer::read_qpdf_token`. The only direct `.read_token(true,`
/// calls left in the crate are that method's own definition and
/// `inline_lookahead_is_plausible`'s inline-image `EI` lookahead, which is a
/// different qpdf owner (`QPDFTokenizer`'s own lookahead, not `QPDF`'s).
#[test]
fn qpdf_read_token_calls_route_through_one_allow_bad_entrypoint() {
    let tokenizer = read_source("tokenizer.rs");
    assert_eq!(
        tokenizer.matches("read_token(true, ").count(),
        2,
        "tokenizer.rs should have exactly `read_qpdf_token`'s own call and \
         `inline_lookahead_is_plausible`'s out-of-scope EI lookahead"
    );

    for path in ["xref.rs", "reader/resolver.rs"] {
        let source = read_source(path);
        assert_eq!(
            source.matches("read_token(true, ").count(),
            0,
            "{path} should route every allow_bad read through \
             Tokenizer::read_qpdf_token instead of calling read_token(true, ...) directly"
        );
    }
}

/// qpdf has one owner-password entry point, `check_owner_password`
/// (`QPDF_encryption.cc:582-590`), and it always yields the recovered user
/// password through its `std::string& user_password` out-parameter
/// (`:542-567`), which its only call site (`:912`) consumes right away. qpdf
/// likewise has no identity cipher: `decryptString` (`:985-986`) and
/// `decryptStream` (`:1106-1107`) return on `e_none` before deriving a key, so
/// the pass-through decision belongs to the method-selection switch, not to the
/// cipher enums. The overloads that discarded the recovered user password and
/// the `Identity` cipher variants had no qpdf counterpart and are removed.
#[test]
fn standard_handler_has_no_owner_password_or_identity_cipher_overloads() {
    let standard = read_source("encryption/standard.rs");
    for dead in [
        // The `(` suffix keeps each pin off the retained `_with_user_password`
        // (and, for the first, the `_v4`/`_r5`/`_r6`) definitions.
        "fn check_owner_password(",
        "fn check_owner_password_v4(",
        // The cipher pins name the match arms rather than the bare variant
        // declarations: a re-added variant without its arm does not compile,
        // so pinning the arms covers both halves.
        "StringCipher::Identity",
        "StringEncryptCipher::Identity",
    ] {
        assert!(
            !standard.contains(dead),
            "standard-handler surface without a qpdf counterpart remains: {dead}"
        );
    }
    assert!(
        !standard.contains("#![allow(dead_code)]"),
        "standard.rs no longer has dead items and must not carry a blanket allow"
    );
    for owner in [
        "fn check_owner_password_with_user_password(",
        "fn check_owner_password_v4_with_user_password(",
    ] {
        assert!(
            standard.contains(owner),
            "canonical owner-password port missing: {owner}"
        );
    }

    // qpdf's `e_none` lives in the `decryptString`/`decryptStream` method
    // switch, so this is where flpdf must keep the pass-through decision.
    assert!(
        read_source("encryption/state.rs").contains("EncryptionMode::Identity => (None, false)"),
        "the /Identity crypt filter must stay a method-selection no-op, not a cipher variant"
    );
}

#[test]
fn canonical_pdf_open_does_not_snapshot_the_complete_source_for_xref() {
    let engine = read_source("engine.rs");
    let production = engine
        .split_once("\n#[cfg(test)]\nmod tests")
        .map_or(engine.as_str(), |(production, _)| production);

    assert!(
        !production.contains("read_initial_source(&mut reader"),
        "canonical Pdf::open must not materialize the complete source before xref loading"
    );
    assert!(
        !production.contains("load_xref_state_from_bytes("),
        "canonical Pdf::open must load xref state through the live source boundary"
    );
}

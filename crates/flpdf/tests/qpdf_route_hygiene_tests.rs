use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use syn::spanned::Spanned;
use syn::visit::Visit;

fn source_root() -> PathBuf {
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

    // qpdf keeps crypt-filter state in one bare
    // `std::map<std::string, encryption_method_e>` (`QPDF.hh:912`); the parallel
    // `CryptFilter*` table that duplicated it could not represent `/AESV3` or
    // qpdf's `e_unknown` at all, so only the `EncryptionMode` owners remain and
    // the module needs no blanket dead-code allow.
    let crypt_filters = read_source("encryption/crypt_filters.rs");
    assert!(!crypt_filters.contains("#![allow(dead_code)]"));
    for dead in [
        "enum CryptFilterMethod",
        "struct CryptFilter ",
        "enum CryptFilterRef",
        "struct V4UseSiteSelectors",
        "fn eff_or_stm(",
        "fn select_crypt_filter",
        "fn cfm_to_object_key_alg(",
    ] {
        assert!(
            !crypt_filters.contains(dead),
            "non-qpdf crypt-filter surface remains: {dead}"
        );
    }
    for owner in [
        "fn interpret_cf_name(",
        "fn interpret_cf_from_handle(",
        "fn interpret_cf_selector_from_handle(",
        "fn crypt_filter_modes_from_handle(",
        "fn crypt_filter_method_from_handle(",
    ] {
        assert!(
            crypt_filters.contains(owner),
            "canonical crypt-filter owner missing: {owner}"
        );
    }
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

/// Handle-only routes must not keep a document they never touch.
///
/// This supersedes a narrower guard that searched a hand-written list of ten
/// flpdf files plus `flpdf-cli/src/main.rs` for the literal `_pdf: &mut Pdf`.
/// Both halves of that shape leaked:
///
/// * The literal missed every other spelling of the same dead carrier. It does
///   not match `_pdf: &'a mut Pdf<R>` (a lifetime sits between `&` and `mut`),
///   `_pdf: &Pdf<R>` (a shared borrow), `_document: &mut Pdf<R>` (a different
///   name), or `_: &mut Pdf<R>` (a wildcard, which is the same papering-over as
///   an underscore rename).
/// * The file list could only ever be as complete as its last edit. Three live
///   carriers -- `pages.rs`, and two in `acroform_document_helper.rs` -- matched
///   the literal exactly and survived only because those files were never added
///   to it. That is the same failure that let a carrier in
///   `flpdf-cli/src/main.rs` through when the list was first written.
///
/// So the search is structural rather than textual, and the tree is walked
/// rather than enumerated: every `.rs` file under `crates/flpdf/src` and
/// `crates/flpdf-cli/src` is parsed with `syn`, and any `_`-prefixed binding --
/// function parameter, closure parameter, or struct/enum field -- whose type
/// mentions `Pdf` is reported.
///
/// Type aliases are followed: `JobDocument` (`job/lifecycle.rs`) is
/// `Pdf<Box<dyn ReadSeek>>`, so a binding declared with the alias reads as the
/// document it is. The index is built from every free `type X = T;` in the two
/// scanned roots, and a chain of aliases is followed to its end.
///
/// # What this search cannot see
///
/// The walk is syntactic. It knows what a binding's type is *written as*, not
/// what the type checker would make of it, and three shapes therefore stay out
/// of reach. They are recorded here rather than left for the next reader to
/// rediscover, because an undocumented gap is what makes a guard like this
/// accrete one special case per review round:
///
/// * **A field reached through a destructuring parameter.** `fn f(Holder {
///   _pdf }: Holder)` would need the field types of `Holder`. It is left open
///   because it is not a new carrier: a `Holder` whose own field is
///   `_pdf: Pdf<R>` is already reported where that field is *declared*, by the
///   same walk. Destructuring only re-binds a carrier the guard has seen.
/// * **A closure parameter with no type annotation.** `|_pdf| ...` has no type
///   to read; only the inference engine has one. It is left open because a
///   closure is not a signature qpdf mirrors -- the carriers this guard exists
///   to fence out were function parameters and struct fields, which is where
///   flpdf's shapes answer to qpdf's. An annotated closure parameter is still
///   checked.
/// * **A binding name produced by a macro.** `bind!(_pdf)` is a `Pat::Macro`,
///   and the name only exists after expansion.
///
/// `crates/flpdf-qtest-tools/src` is outside those two roots and so is not
/// scanned at all. Its `run_test_NN` dispatch table binds every parameter with
/// a leading underscore because qpdf's `test_driver.cc` gives every test the
/// same signature; that uniformity is the qtest driver's contract, not a dead
/// bridge, and it needs no marker because the walk never reaches it.
///
/// Whether a reported binding should be dropped or kept is a judgment about
/// qpdf, not something this guard can decide: if qpdf's counterpart takes a
/// document, the parameter stays and the divergence is the thing to
/// investigate; if it does not, the parameter goes. A binding that must stay
/// carries a [`ALLOW_MARKER`] comment on the line above it, naming the binding
/// and stating why.
#[test]
fn no_underscore_bound_pdf_carriers_remain_outside_marked_exceptions() {
    let aliases = scanned_alias_index();
    let mut reported: Vec<String> = Vec::new();
    for file in scanned_source_files() {
        let source = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("read {}: {error}", file.display()))
            .replace("\r\n", "\n");
        let display = file.display().to_string();
        for carrier in unmarked_dead_pdf_carriers_with_aliases(&source, &display, &aliases) {
            reported.push(format!(
                "{display}:{} -- `{}` in {} carries a Pdf it never reads",
                carrier.line, carrier.binding, carrier.owner
            ));
        }
    }

    assert!(
        reported.is_empty(),
        "dead Pdf carriers found:\n  {}\n\nDrop the binding if qpdf's \
         counterpart takes no document, or keep it and write \
         `// {ALLOW_MARKER} <reason>` on the line above it.",
        reported.join("\n  ")
    );
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

// ---------------------------------------------------------------------------
// Dead `Pdf` carrier detection
// ---------------------------------------------------------------------------

/// Inline exclusion marker for a binding that must keep the document it does
/// not read.
///
/// Written as `// route-hygiene-allow: <binding> <reason>` on the line directly
/// above the binding's own declaration. The grammar mirrors
/// `// qpdf-deviation:` (`scripts/check-qpdf-deviation-markers.py`): a real
/// `//` line comment with a mandatory reason, so the exclusion is reviewable
/// where it applies instead of hiding in a file-level allowlist. Naming the
/// binding is what keeps it from drifting onto whatever declaration later
/// happens to sit below it. The token is deliberately distinct from
/// `qpdf-deviation`, because that script rejects any occurrence of its own
/// token that is not one of its three well-formed forms.
const ALLOW_MARKER: &str = "route-hygiene-allow:";

/// A `_`-prefixed binding whose type mentions `Pdf`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DeadPdfCarrier {
    /// 1-based line of the binding's own declaration.
    line: usize,
    /// The bound name, or `_` for a wildcard pattern.
    binding: String,
    /// The enclosing item, for the failure message.
    owner: String,
}

/// Every `.rs` file under the two scanned crate roots.
fn scanned_source_files() -> Vec<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for root in ["src", "../flpdf-cli/src"] {
        let root = manifest.join(root);
        let before = files.len();
        collect_rust_files(&root, &mut files);
        assert!(
            files.len() > before,
            "no .rs files under {} -- the walk that replaced the hand-written \
             file list must not silently scan nothing",
            root.display()
        );
    }
    files.sort();
    files
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            collect_rust_files(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

/// Every free `type X = T;` alias in the scanned roots, by alias name.
///
/// A name maps to *all* the types declared under it, because two modules may
/// each declare their own `type XrefWidths = ...`. Keeping both and reporting
/// if any of them reaches `Pdf` is the fail-closed reading of a collision.
///
/// Aliases declared inside `#[cfg(test)]` modules and inside function bodies
/// are indexed too. `syn` does not evaluate `cfg`, so they are in the tree
/// either way, and keeping them can only widen what the search resolves --
/// never narrow it. An alias that names no document costs nothing here.
type AliasIndex = BTreeMap<String, Vec<syn::Type>>;

/// Parse `source` and return every `_`-prefixed binding whose type mentions
/// `Pdf`, in source order, resolving no aliases.
fn dead_pdf_carriers(source: &str) -> Vec<DeadPdfCarrier> {
    dead_pdf_carriers_with_aliases(source, &AliasIndex::new())
}

/// [`dead_pdf_carriers`], resolving type aliases through `aliases`.
fn dead_pdf_carriers_with_aliases(source: &str, aliases: &AliasIndex) -> Vec<DeadPdfCarrier> {
    let file = syn::parse_file(source).expect("scanned source must parse as Rust");
    let mut scan = CarrierScan {
        owners: Vec::new(),
        found: Vec::new(),
        aliases,
    };
    Visit::visit_file(&mut scan, &file);
    scan.found
}

/// Add every free type alias declared in `source` to `index`.
fn collect_aliases(source: &str, index: &mut AliasIndex) {
    struct Collect<'a>(&'a mut AliasIndex);
    impl<'ast> Visit<'ast> for Collect<'_> {
        fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
            self.0
                .entry(strip_raw(&node.ident))
                .or_default()
                .push((*node.ty).clone());
            syn::visit::visit_item_type(self, node);
        }
    }
    let Ok(file) = syn::parse_file(source) else {
        return;
    };
    Visit::visit_file(&mut Collect(index), &file);
}

/// The alias index of the two scanned crate roots.
fn scanned_alias_index() -> AliasIndex {
    let mut index = AliasIndex::new();
    for file in scanned_source_files() {
        let source = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("read {}: {error}", file.display()))
            .replace("\r\n", "\n");
        collect_aliases(&source, &mut index);
    }
    index
}

/// The carriers of `source` that no exclusion marker covers.
///
/// Panics on a malformed marker, and on a marker whose named binding is not
/// the carrier below it. A marker keyed to position alone would drift: replace
/// the binding under it with a different dead carrier and the old reason would
/// silently excuse the new one, which is the file-level allowlist this guard
/// replaced, reintroduced one line at a time.
fn unmarked_dead_pdf_carriers(source: &str, display: &str) -> Vec<DeadPdfCarrier> {
    unmarked_dead_pdf_carriers_with_aliases(source, display, &AliasIndex::new())
}

/// [`unmarked_dead_pdf_carriers`], resolving type aliases through `aliases`.
fn unmarked_dead_pdf_carriers_with_aliases(
    source: &str,
    display: &str,
    aliases: &AliasIndex,
) -> Vec<DeadPdfCarrier> {
    let carriers = dead_pdf_carriers_with_aliases(source, aliases);
    let marked = marked_bindings(source, display);
    for (line, (_, binding)) in &marked {
        assert!(
            carriers
                .iter()
                .any(|carrier| carrier.line == *line && carrier.binding == *binding),
            "{display}:{line}: a `{ALLOW_MARKER}` marker names `{binding}`, but \
             that is not the dead Pdf carrier below it; retarget the marker or \
             drop it"
        );
    }
    carriers
        .into_iter()
        .filter(|carrier| {
            marked.get(&carrier.line).map(|(_, binding)| binding) != Some(&carrier.binding)
        })
        .collect()
}

/// The binding each exclusion marker excuses, by 1-based declaration line,
/// paired with the marker's own line so a collision can name both sites.
fn marked_bindings(source: &str, display: &str) -> BTreeMap<usize, (usize, String)> {
    let comment = format!("// {ALLOW_MARKER}");
    let lines: Vec<&str> = source.lines().collect();
    let attributes = attribute_extents(source);
    let mut marked = BTreeMap::new();
    for (index, line) in lines.iter().enumerate() {
        if !line.contains(ALLOW_MARKER) {
            continue;
        }
        let trimmed = line.trim_start();
        assert!(
            trimmed.starts_with(&comment),
            "{display}:{}: `{ALLOW_MARKER}` must be written as \
             `// {ALLOW_MARKER} <binding> <reason>` on its own line comment",
            index + 1
        );
        let mut text = trimmed[comment.len()..]
            .trim()
            .splitn(2, char::is_whitespace);
        let binding = text.next().unwrap_or_default();
        assert!(
            binding.starts_with('_'),
            "{display}:{}: `{ALLOW_MARKER}` must name the binding it excuses \
             first, as in `{ALLOW_MARKER} _pdf -- <reason>`",
            index + 1
        );
        assert!(
            !text.next().unwrap_or_default().trim().is_empty(),
            "{display}:{}: `{ALLOW_MARKER}` needs a reason after `{binding}`",
            index + 1
        );
        // Blank lines, further comments, and attributes stand between a
        // marker and the declaration it excuses. Attribute extents come from
        // the syntax tree rather than from counting `[` and `]`, because a
        // bracket inside a string literal (`#[doc = "["]`) is not a
        // delimiter and a raw character count never returns to depth zero.
        let mut cursor = index + 1;
        let target = loop {
            let Some(next) = lines.get(cursor) else {
                break None;
            };
            let trimmed = next.trim_start();
            if trimmed.is_empty()
                || trimmed.starts_with("//")
                || line_holds_only_attributes(next, cursor + 1, &attributes)
            {
                cursor += 1;
                continue;
            }
            break Some(cursor);
        };
        let target = target.unwrap_or_else(|| {
            panic!(
                "{display}:{}: `{ALLOW_MARKER}` precedes no declaration",
                index + 1
            )
        });
        // Two markers resolving to the same declaration would let the later
        // one overwrite the earlier: a stale marker naming a binding that is
        // no longer there would then never be rejected. Every marker has to
        // stand on its own, so refuse the collision instead of collapsing it.
        if let Some((previous_line, previous_binding)) =
            marked.insert(target + 1, (index + 1, binding.to_owned()))
        {
            panic!(
                "{display}:{}: `{ALLOW_MARKER} {previous_binding}` and \
                 {display}:{}: `{ALLOW_MARKER} {binding}` both excuse the \
                 declaration at line {}; keep exactly one marker per \
                 declaration so a stale one cannot hide behind a valid one",
                previous_line,
                index + 1,
                target + 1
            );
        }
    }
    marked
}

/// One attribute's source extent: 1-based start and end lines with the
/// 0-based, end-exclusive columns the attribute occupies on each.
#[derive(Debug, Clone, Copy)]
struct AttributeExtent {
    start_line: usize,
    start_column: usize,
    end_line: usize,
    end_column: usize,
}

/// The source extent of every attribute in `source`.
///
/// Returns nothing for a source that does not parse. That is not a silent
/// degradation of the guard: every scanned file is parsed by
/// [`dead_pdf_carriers`] with an `expect`, and the only inputs that reach
/// here without parsing are the synthetic marker-grammar cases below, whose
/// assertions all fire before an attribute could matter.
fn attribute_extents(source: &str) -> Vec<AttributeExtent> {
    #[derive(Default)]
    struct Collect(Vec<AttributeExtent>);
    impl<'ast> Visit<'ast> for Collect {
        fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
            // `#` opens the attribute and the closing `]` of the bracket
            // group ends it; taking both from their own tokens avoids
            // relying on how spans join across a multi-line attribute.
            let start = node.pound_token.span.start();
            let end = node.bracket_token.span.close().end();
            self.0.push(AttributeExtent {
                start_line: start.line,
                start_column: start.column,
                end_line: end.line,
                end_column: end.column,
            });
            syn::visit::visit_attribute(self, node);
        }
    }
    let Ok(file) = syn::parse_file(source) else {
        return Vec::new();
    };
    let mut collect = Collect::default();
    Visit::visit_file(&mut collect, &file);
    collect.0
}

/// Whether `line` (1-based `number`) holds nothing but attribute text.
///
/// Column-aware on purpose: `#[inline] fn f(_pdf: &mut Pdf<R>) {}` puts an
/// attribute and a declaration on one line, and skipping that whole line
/// would step over the declaration the marker is meant to cover.
fn line_holds_only_attributes(line: &str, number: usize, attributes: &[AttributeExtent]) -> bool {
    let mut covered: Vec<bool> = vec![false; line.chars().count()];
    let mut touched = false;
    for extent in attributes {
        if number < extent.start_line || number > extent.end_line {
            continue;
        }
        touched = true;
        let from = if number == extent.start_line {
            extent.start_column
        } else {
            0
        };
        let to = if number == extent.end_line {
            extent.end_column.min(covered.len())
        } else {
            covered.len()
        };
        for slot in covered.iter_mut().take(to).skip(from) {
            *slot = true;
        }
    }
    if !touched {
        return false;
    }
    // Everything the attributes do not cover has to be blank -- except a
    // trailing line comment. `#[allow(dead_code)] // rationale` is a style
    // the scanned sources already use, and its comment characters sit
    // outside the attribute's own extent.
    let mut rest = String::new();
    for (character, inside) in line.chars().zip(&covered) {
        if !*inside {
            rest.push(character);
        }
    }
    let rest = rest.trim();
    rest.is_empty() || rest.starts_with("//")
}

/// An identifier's text without the raw-identifier prefix. `r#_pdf` is an
/// underscore-prefixed binding as far as rustc's unused-variable diagnostic
/// is concerned, and `r#Pdf` names the same type as `Pdf`, so both spellings
/// have to reach the same classification.
fn strip_raw(ident: &proc_macro2::Ident) -> String {
    let text = ident.to_string();
    text.strip_prefix("r#").unwrap_or(&text).to_owned()
}

/// The element types of a tuple type, seen through references and parens, so
/// `(&mut Pdf<R>, usize)` and `&(&mut Pdf<R>, usize)` both pair a tuple
/// pattern's subpatterns with their own component types.
fn tuple_components<'a>(ty: &'a syn::Type, aliases: &'a AliasIndex) -> Option<Vec<&'a syn::Type>> {
    fn walk<'a>(
        ty: &'a syn::Type,
        aliases: &'a AliasIndex,
        seen: &mut BTreeSet<String>,
    ) -> Option<Vec<&'a syn::Type>> {
        match ty {
            syn::Type::Tuple(tuple) => Some(tuple.elems.iter().collect()),
            syn::Type::Reference(reference) => walk(&reference.elem, aliases, seen),
            syn::Type::Paren(paren) => walk(&paren.elem, aliases, seen),
            syn::Type::Group(group) => walk(&group.elem, aliases, seen),
            // A tuple written through an alias -- `type Pair<'a, R> =
            // (usize, &'a mut Pdf<R>)` -- has to be followed, or every
            // subpattern is paired with the whole alias and an unrelated
            // binding reads as a document carrier.
            syn::Type::Path(path) if path.qself.is_none() => {
                let name = strip_raw(&path.path.segments.last()?.ident);
                // A colliding name resolves fail-closed: with more than one
                // candidate there is no single component list to pair
                // against, so the caller keeps the whole type.
                let [only] = aliases.get(&name)?.as_slice() else {
                    return None;
                };
                // `seen` stops `type A = B; type B = A;` from recursing forever.
                if !seen.insert(name) {
                    return None;
                }
                walk(only, aliases, seen)
            }
            _ => None,
        }
    }
    walk(ty, aliases, &mut BTreeSet::new())
}

/// Whether `ty` names `Pdf` anywhere, so `&Pdf<R>`, `&'a mut Pdf<R>`,
/// `crate::Pdf<R>` and `Option<&mut Pdf<R>>` are one case rather than four.
///
/// A name that `aliases` knows is followed to the type it stands for, so
/// `JobDocument` (`job/lifecycle.rs`: `pub type JobDocument =
/// Pdf<Box<dyn ReadSeek>>`) reads as the document it is rather than as an
/// unrelated identifier. Following is by path *segment*, so the qualified
/// `flpdf::job::JobDocument` resolves as readily as the bare spelling, and a
/// chain of aliases is followed to its end.
fn type_mentions_pdf(ty: &syn::Type, aliases: &AliasIndex) -> bool {
    fn walk(ty: &syn::Type, aliases: &AliasIndex, seen: &mut BTreeSet<String>) -> bool {
        struct Idents(Vec<String>);
        impl Visit<'_> for Idents {
            fn visit_ident(&mut self, node: &proc_macro2::Ident) {
                self.0.push(strip_raw(node));
            }
        }
        let mut idents = Idents(Vec::new());
        Visit::visit_type(&mut idents, ty);
        for ident in idents.0 {
            if ident == "Pdf" {
                return true;
            }
            let Some(aliased) = aliases.get(&ident) else {
                continue;
            };
            // `seen` stops `type A = B; type B = A;` from recursing forever.
            if seen.insert(ident) && aliased.iter().any(|aliased| walk(aliased, aliases, seen)) {
                return true;
            }
        }
        false
    }
    walk(ty, aliases, &mut BTreeSet::new())
}

struct CarrierScan<'a> {
    owners: Vec<String>,
    found: Vec<DeadPdfCarrier>,
    aliases: &'a AliasIndex,
}

impl CarrierScan<'_> {
    fn owner(&self) -> String {
        self.owners
            .last()
            .cloned()
            .unwrap_or_else(|| "file scope".to_owned())
    }

    /// Record a parameter list; the `Receiver` (`self`) arm carries no pattern.
    fn record_inputs<'a>(&mut self, inputs: impl Iterator<Item = &'a syn::FnArg>) {
        for input in inputs {
            if let syn::FnArg::Typed(typed) = input {
                self.record(&typed.pat, &typed.ty);
            }
        }
    }

    fn record(&mut self, pat: &syn::Pat, ty: &syn::Type) {
        let binding = match pat {
            syn::Pat::Ident(ident) if strip_raw(&ident.ident).starts_with('_') => {
                strip_raw(&ident.ident)
            }
            // A bare `_` is the same papering-over as an underscore rename.
            syn::Pat::Wild(_) => "_".to_owned(),
            // An ordinary destructuring pattern must not hide a carrier:
            // `fn f<R>((_pdf, _): (&mut Pdf<R>, usize))` binds `_pdf` to the
            // document just as a plain parameter would. Pair each
            // subpattern with its own component type where the pattern and
            // the type line up, and fall back to the whole type otherwise so
            // a carrier is never dropped for want of an exact component.
            syn::Pat::Tuple(tuple) => {
                // `..` stands for however many components the pattern does
                // not name, so index-based pairing only holds up to the rest
                // position. Walk the prefix forwards and the suffix
                // backwards, the way the language matches them.
                // Aliases are followed without substituting type
                // arguments, so `type Pair<R> = (usize, R)` used as
                // `Pair<&mut Pdf<X>>` would resolve to components that no
                // longer mention the document. Losing it that way is a
                // miss, so when the whole type names `Pdf` and no component
                // does, keep the whole type instead.
                let components = tuple_components(ty, self.aliases).filter(|types| {
                    !type_mentions_pdf(ty, self.aliases)
                        || types
                            .iter()
                            .any(|component| type_mentions_pdf(component, self.aliases))
                });
                let elems: Vec<&syn::Pat> = tuple.elems.iter().collect();
                let rest = elems
                    .iter()
                    .position(|element| matches!(element, syn::Pat::Rest(_)));
                let total = components.as_ref().map(Vec::len);
                for (index, element) in elems.iter().enumerate() {
                    if matches!(element, syn::Pat::Rest(_)) {
                        continue;
                    }
                    let position = match (rest, total) {
                        // After the rest, count back from the tuple's end.
                        (Some(rest_at), Some(total)) if index > rest_at => {
                            total.checked_sub(elems.len() - index)
                        }
                        _ => Some(index),
                    };
                    let component = position
                        .and_then(|position| {
                            components
                                .as_ref()
                                .and_then(|types| types.get(position).copied())
                        })
                        .unwrap_or(ty);
                    self.record(element, component);
                }
                return;
            }
            syn::Pat::Reference(inner) => {
                self.record(&inner.pat, ty);
                return;
            }
            syn::Pat::Type(inner) => {
                self.record(&inner.pat, &inner.ty);
                return;
            }
            syn::Pat::Slice(slice) => {
                for element in &slice.elems {
                    self.record(element, ty);
                }
                return;
            }
            syn::Pat::TupleStruct(tuple_struct) => {
                for element in &tuple_struct.elems {
                    self.record(element, ty);
                }
                return;
            }
            syn::Pat::Struct(pattern) => {
                for field in &pattern.fields {
                    self.record(&field.pat, ty);
                }
                return;
            }
            syn::Pat::Or(pattern) => {
                for case in &pattern.cases {
                    self.record(case, ty);
                }
                return;
            }
            syn::Pat::Paren(inner) => {
                self.record(&inner.pat, ty);
                return;
            }
            _ => return,
        };
        if !type_mentions_pdf(ty, self.aliases) {
            return;
        }
        self.found.push(DeadPdfCarrier {
            line: pat.span().start().line,
            binding,
            owner: self.owner(),
        });
    }

    fn scoped(&mut self, owner: String, body: impl FnOnce(&mut Self)) {
        self.owners.push(owner);
        body(self);
        self.owners.pop();
    }
}

impl<'ast> Visit<'ast> for CarrierScan<'_> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.scoped(format!("fn {}", node.sig.ident), |scan| {
            scan.record_inputs(node.sig.inputs.iter());
            syn::visit::visit_block(scan, &node.block);
        });
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.scoped(format!("fn {}", node.sig.ident), |scan| {
            scan.record_inputs(node.sig.inputs.iter());
            syn::visit::visit_block(scan, &node.block);
        });
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        self.scoped(format!("fn {}", node.sig.ident), |scan| {
            scan.record_inputs(node.sig.inputs.iter());
            if let Some(block) = node.default.as_ref() {
                syn::visit::visit_block(scan, block);
            }
        });
    }

    fn visit_expr_closure(&mut self, node: &'ast syn::ExprClosure) {
        for input in &node.inputs {
            if let syn::Pat::Type(typed) = input {
                self.record(&typed.pat, &typed.ty);
            }
        }
        syn::visit::visit_expr(self, &node.body);
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        self.scoped(format!("struct {}", node.ident), |scan| {
            syn::visit::visit_fields(scan, &node.fields);
        });
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        self.scoped(format!("enum {}", node.ident), |scan| {
            for variant in &node.variants {
                syn::visit::visit_fields(scan, &variant.fields);
            }
        });
    }

    fn visit_field(&mut self, node: &'ast syn::Field) {
        let Some(ident) = node.ident.as_ref() else {
            return;
        };
        // Raw spellings reach the same classification here too: `r#_pdf` is
        // an underscore-prefixed field as far as rustc is concerned.
        let binding = strip_raw(ident);
        if binding.starts_with('_') && type_mentions_pdf(&node.ty, self.aliases) {
            self.found.push(DeadPdfCarrier {
                line: ident.span().start().line,
                binding,
                owner: self.owner(),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// The detector's own contract
// ---------------------------------------------------------------------------

/// The spellings the superseded literal `_pdf: &mut Pdf` could not see.
///
/// Each line below is a dead `Pdf` carrier; none of them contains that literal,
/// which is asserted here rather than argued, so the gap the structural search
/// closes stays demonstrable after this guard changes.
#[test]
fn the_structural_search_sees_spellings_the_literal_missed() {
    let source = "\
struct Holder<'a, R> {
    _pdf: &'a mut Pdf<R>,
}
fn shared<R>(_pdf: &Pdf<R>) {}
fn renamed<R>(_document: &mut Pdf<R>) {}
fn wildcard<R>(_: &mut Pdf<R>) {}
fn qualified<R>(_owner: &mut crate::Pdf<R>) {}
fn wrapped<R>(_maybe: Option<&mut Pdf<R>>) {}
fn owned<R>(_taken: Pdf<R>) {}
";
    assert!(
        !source.contains("_pdf: &mut Pdf"),
        "the superseded literal must match none of these carriers"
    );

    let found: Vec<(usize, String)> = dead_pdf_carriers(source)
        .into_iter()
        .map(|carrier| (carrier.line, carrier.binding))
        .collect();
    assert_eq!(
        found,
        vec![
            (2, "_pdf".to_owned()),
            (4, "_pdf".to_owned()),
            (5, "_document".to_owned()),
            (6, "_".to_owned()),
            (7, "_owner".to_owned()),
            (8, "_maybe".to_owned()),
            (9, "_taken".to_owned()),
        ]
    );
}

/// Live parameters, and underscore parameters that carry something else, are
/// not the dead-bridge shape and must not be reported.
#[test]
fn the_structural_search_leaves_live_and_unrelated_bindings_alone() {
    let source = "\
fn live<R>(pdf: &mut Pdf<R>) {}
fn unrelated(_depth: usize, _key: &[u8]) {}
struct Live<'a, R> {
    pdf: &'a mut Pdf<R>,
    _depth: usize,
}
";
    assert_eq!(dead_pdf_carriers(source), Vec::new());
}

/// Carriers are found in every binding position, and named by their owner.
///
/// The owner is for the failure message only, so an item kind the scan does
/// not name -- a `union`, below -- still reports its field, just without one.
/// Reporting is what the guard is for; naming is a convenience.
#[test]
fn the_structural_search_reaches_every_binding_position() {
    let source = "\
impl<'a, R> Helper<'a, R> {
    fn method<S>(&self, _pdf: &mut Pdf<S>) {}
}
trait Route<R> {
    fn required(&self, _pdf: &mut Pdf<R>);
    fn provided(&self, _pdf: &mut Pdf<R>) {}
}
enum Carrier<'a, R> {
    Named { _pdf: &'a mut Pdf<R> },
}
fn outer<R>() {
    fn inner<S>(_pdf: &mut Pdf<S>) {}
    let closure = |_pdf: &mut Pdf<R>| ();
}
union Untracked<'a, R> {
    _pdf: &'a mut Pdf<R>,
}
";
    let found: Vec<(usize, String)> = dead_pdf_carriers(source)
        .into_iter()
        .map(|carrier| (carrier.line, carrier.owner))
        .collect();
    assert_eq!(
        found,
        vec![
            (2, "fn method".to_owned()),
            (5, "fn required".to_owned()),
            (6, "fn provided".to_owned()),
            (9, "enum Carrier".to_owned()),
            (12, "fn inner".to_owned()),
            (13, "fn outer".to_owned()),
            (16, "file scope".to_owned()),
        ]
    );
}

/// A marker covers the declaration on the line below it, skipping the
/// attributes and further comments between the two.
#[test]
fn an_exclusion_marker_covers_the_declaration_it_precedes() {
    let source = "\
// route-hygiene-allow: _pdf -- qpdf's counterpart takes the document.
fn kept<R>(_pdf: &mut Pdf<R>) {}
fn reported<R>(_pdf: &mut Pdf<R>) {}
struct Held<'a, R> {
    // route-hygiene-allow: _pdf -- holds the exclusive borrow, not a value.
    #[allow(dead_code)]
    _pdf: &'a mut Pdf<R>,
}
";
    assert_eq!(
        marked_bindings(source, "synthetic"),
        BTreeMap::from([(2, (1, "_pdf".to_owned())), (7, (5, "_pdf".to_owned())),])
    );
    let found: Vec<usize> = unmarked_dead_pdf_carriers(source, "synthetic")
        .into_iter()
        .map(|carrier| carrier.line)
        .collect();
    assert_eq!(found, vec![3]);
}

/// A destructuring parameter must not hide a carrier: `(_pdf, _)` binds the
/// document exactly as a plain `_pdf` parameter would, and an outer
/// `Pat::Tuple` used to end the scan before the subpattern was ever seen.
#[test]
fn destructured_parameters_still_report_their_pdf_carrier() {
    let source = "\
fn tupled<R>((_pdf, _n): (&mut Pdf<R>, usize)) {}
fn referenced<R>(&(_pdf, _n): &(&mut Pdf<R>, usize)) {}
fn nested<R>(((_pdf, _n), _m): ((&mut Pdf<R>, usize), usize)) {}
fn unrelated<R>((_first, _second): (usize, R)) {}
";
    let found: Vec<(usize, String)> = dead_pdf_carriers(source)
        .into_iter()
        .map(|carrier| (carrier.line, carrier.binding))
        .collect();
    assert_eq!(
        found,
        vec![
            (1, "_pdf".to_owned()),
            (2, "_pdf".to_owned()),
            (3, "_pdf".to_owned()),
        ],
        "each destructured `_pdf` must be reported, and a tuple of unrelated \
         types must not be"
    );
}

/// Two markers resolving to one declaration used to collapse into a single
/// map entry, so a stale marker could ride along behind a valid one.
#[test]
#[should_panic(expected = "both excuse the declaration")]
fn two_markers_for_one_declaration_are_rejected() {
    let source = "\
// route-hygiene-allow: _stale -- names a binding that is not here
// route-hygiene-allow: _pdf -- holds the exclusive borrow, not a value.
fn kept<R>(_pdf: &mut Pdf<R>) {}
";
    let _ = marked_bindings(source, "synthetic");
}
#[test]
#[should_panic(expected = "must name the binding it excuses")]
fn an_exclusion_marker_that_names_no_binding_is_rejected() {
    marked_bindings(
        "// route-hygiene-allow: qpdf takes the document.\nfn f<R>(_pdf: &mut Pdf<R>) {}\n",
        "synthetic",
    );
}

#[test]
#[should_panic(expected = "needs a reason after `_pdf`")]
fn an_exclusion_marker_without_a_reason_is_rejected() {
    marked_bindings(
        "// route-hygiene-allow: _pdf\nfn f<R>(_pdf: &mut Pdf<R>) {}\n",
        "synthetic",
    );
}

#[test]
#[should_panic(expected = "on its own line comment")]
fn the_marker_token_outside_a_line_comment_is_rejected() {
    marked_bindings(
        "let note = \"route-hygiene-allow: smuggled\";\n",
        "synthetic",
    );
}

#[test]
#[should_panic(expected = "precedes no declaration")]
fn an_exclusion_marker_with_nothing_after_it_is_rejected() {
    marked_bindings("// route-hygiene-allow: _pdf -- trailing\n\n", "synthetic");
}

#[test]
#[should_panic(expected = "retarget the marker or drop it")]
fn an_exclusion_marker_that_no_longer_covers_a_carrier_is_rejected() {
    unmarked_dead_pdf_carriers(
        "// route-hygiene-allow: _pdf -- the parameter it excused is gone.\n\
         fn f(depth: usize) {}\n",
        "synthetic",
    );
}

/// The drift the named-binding grammar exists to stop: replacing the binding
/// under a marker with a different dead carrier must not inherit its reason.
#[test]
#[should_panic(expected = "retarget the marker or drop it")]
fn an_exclusion_marker_does_not_follow_a_replaced_binding() {
    unmarked_dead_pdf_carriers(
        "// route-hygiene-allow: _pdf -- holds the exclusive borrow.\n\
         fn f<R>(_document: &mut Pdf<R>) {}\n",
        "synthetic",
    );
}

/// `..` stands for the components the pattern does not name, so a carrier
/// after the rest position lines up with the tuple's *end*, not with its
/// index in the pattern. Index-based pairing paired `_pdf` with a `usize`
/// and reported nothing.
#[test]
fn rest_patterns_pair_suffix_bindings_with_the_tuple_end() {
    let after = "fn probe<R>((_n, .., _pdf): (usize, usize, usize, &mut Pdf<R>)) {}\n";
    assert_eq!(
        dead_pdf_carriers(after).len(),
        1,
        "a carrier after `..` must pair with the tuple's final component"
    );
    let before = "fn probe<R>((_pdf, ..): (&mut Pdf<R>, usize, usize)) {}\n";
    assert_eq!(
        dead_pdf_carriers(before).len(),
        1,
        "a carrier before `..` still pairs by its own index"
    );
    let neither = "fn probe<R>((_n, .., _m): (usize, usize, usize, usize)) {}\n";
    assert!(
        dead_pdf_carriers(neither).is_empty(),
        "a tuple with no Pdf component must not be reported"
    );
}

/// `r#_pdf` is underscore-prefixed as far as rustc's unused-variable
/// diagnostic is concerned, and `r#Pdf` names the same type as `Pdf`;
/// `Ident::to_string` keeps the `r#`, so both spellings used to slip past.
#[test]
fn raw_identifiers_are_classified_like_their_plain_spelling() {
    let raw_binding = "fn probe<R>(r#_pdf: &mut Pdf<R>) {}\n";
    assert_eq!(
        dead_pdf_carriers(raw_binding).len(),
        1,
        "`r#_pdf` is the same dead carrier as `_pdf`"
    );
    let raw_type = "fn probe<R>(_pdf: &mut r#Pdf<R>) {}\n";
    assert_eq!(
        dead_pdf_carriers(raw_type).len(),
        1,
        "`r#Pdf` names the same type as `Pdf`"
    );
}

/// `r#_pdf` as a struct field is the same dead carrier as `_pdf`; the field
/// arm compared the raw text and let it through.
#[test]
fn raw_identifier_fields_are_classified_like_their_plain_spelling() {
    let source = "struct Holder<R> {\n    r#_pdf: Pdf<R>,\n}\n";
    let found: Vec<String> = dead_pdf_carriers(source)
        .into_iter()
        .map(|carrier| carrier.binding)
        .collect();
    assert_eq!(found, vec!["_pdf".to_owned()]);
}

/// A binding declared with a type alias is the same dead carrier as one
/// declared with the type the alias stands for.
///
/// The index comes from the real scanned roots rather than from a synthetic
/// `type Alias = Pdf<R>;`, because what has to hold is that *`JobDocument`*
/// resolves -- the alias that exists in the tree, one keystroke away from
/// spelling a dead carrier the earlier search could not see.
#[test]
fn type_aliases_resolve_to_the_document_they_stand_for() {
    let aliases = scanned_alias_index();
    assert!(
        aliases.contains_key("JobDocument"),
        "job/lifecycle.rs declares `pub type JobDocument = Pdf<Box<dyn ReadSeek>>`; \
         the index of the scanned roots must hold it"
    );

    let aliased = "fn discard(_document: &mut JobDocument) {}\n";
    assert_eq!(
        dead_pdf_carriers(aliased),
        Vec::new(),
        "without the index the alias name is just an identifier"
    );
    let found: Vec<String> = dead_pdf_carriers_with_aliases(aliased, &aliases)
        .into_iter()
        .map(|carrier| carrier.binding)
        .collect();
    assert_eq!(
        found,
        vec!["_document".to_owned()],
        "`JobDocument` is `Pdf<Box<dyn ReadSeek>>` and must be reported as one"
    );

    // A qualified spelling resolves through the same path segment, and an
    // alias that stands for something else is still not a carrier.
    assert_eq!(
        dead_pdf_carriers_with_aliases(
            "fn discard(_document: &mut flpdf::job::JobDocument) {}\n",
            &aliases
        )
        .len(),
        1,
        "the alias resolves by path segment, so a qualified spelling counts too"
    );
    assert!(
        dead_pdf_carriers_with_aliases("fn probe(_widths: XrefWidths) {}\n", &aliases).is_empty(),
        "an alias that does not stand for a document must not be reported"
    );
}

/// Alias chains are followed to their end, a cycle terminates, and two
/// modules declaring the same alias name are both consulted.
#[test]
fn alias_resolution_follows_chains_without_looping() {
    let mut chained = AliasIndex::new();
    collect_aliases(
        "type First = Second;\ntype Second = Pdf<R>;\n",
        &mut chained,
    );
    assert_eq!(
        dead_pdf_carriers_with_aliases("fn probe(_pdf: &mut First) {}\n", &chained).len(),
        1,
        "a chain of aliases must be followed to the document at its end"
    );

    let mut cyclic = AliasIndex::new();
    collect_aliases("type Loop = Knot;\ntype Knot = Loop;\n", &mut cyclic);
    assert!(
        dead_pdf_carriers_with_aliases("fn probe(_pdf: &mut Loop) {}\n", &cyclic).is_empty(),
        "a cyclic alias must terminate without reporting a carrier"
    );

    // Two modules may each declare their own `type Shared = ...`. The index
    // keeps both, and a document behind either spelling is reported.
    let mut collided = AliasIndex::new();
    collect_aliases("type Shared = usize;\n", &mut collided);
    collect_aliases("type Shared = Pdf<R>;\n", &mut collided);
    assert_eq!(
        dead_pdf_carriers_with_aliases("fn probe(_pdf: &mut Shared) {}\n", &collided).len(),
        1,
        "a name collision must resolve fail-closed, not to whichever was read last"
    );
}

/// An attribute is skipped by the extent the syntax tree gives it, not by
/// counting `[` and `]`. A bracket inside a string literal is not a
/// delimiter, and the raw count never returned to depth zero, so the marker
/// search ran off the end of the file and panicked.
#[test]
fn an_unbalanced_bracket_inside_an_attribute_is_not_a_delimiter() {
    let source = "\
// route-hygiene-allow: _pdf -- holds the exclusive borrow, not a value.
#[doc = \"[\"]
fn kept<R>(_pdf: &mut Pdf<R>) {}
";
    assert_eq!(
        marked_bindings(source, "synthetic"),
        BTreeMap::from([(3, (1, "_pdf".to_owned()))]),
        "the marker must key to the declaration below the attribute"
    );
    assert!(
        unmarked_dead_pdf_carriers(source, "synthetic").is_empty(),
        "a correctly marked declaration must not be reported"
    );
}

/// An attribute may carry a trailing line comment. Its comment characters
/// lie outside the attribute's own extent, so a skip that demanded every
/// uncovered character be blank stopped on the attribute line and keyed the
/// marker to it instead of to the declaration below. That style already
/// appears in the scanned sources.
#[test]
fn a_trailing_comment_after_an_attribute_is_still_skipped() {
    let source = "\
// route-hygiene-allow: _pdf -- holds the exclusive borrow, not a value.
#[allow(dead_code)] // the carrier is kept for the route it stands on
fn kept<R>(_pdf: &mut Pdf<R>) {}
";
    assert_eq!(
        marked_bindings(source, "synthetic"),
        BTreeMap::from([(3, (1, "_pdf".to_owned()))]),
        "the marker must reach past the attribute's trailing comment"
    );
    assert!(
        unmarked_dead_pdf_carriers(source, "synthetic").is_empty(),
        "a correctly marked declaration must not be reported"
    );
}

/// A tuple parameter written through an alias has to be followed, or every
/// subpattern is paired with the whole alias: `_index` in
/// `fn f((_index, pdf): Pair<'_, R>)` then reads as a document carrier and
/// the guard rejects valid code.
#[test]
fn a_tuple_alias_pairs_each_subpattern_with_its_own_component() {
    let source = "\
type Pair<'a, R> = (usize, &'a mut Pdf<R>);
fn f<R>((_index, pdf): Pair<'_, R>) {
    let _ = pdf;
}
";
    let mut aliases = AliasIndex::new();
    collect_aliases(source, &mut aliases);
    assert!(
        dead_pdf_carriers_with_aliases(source, &aliases).is_empty(),
        "`_index` is a usize: following the alias must pair it with its own \
         component, not with the whole tuple"
    );

    // The document component itself still has to be seen through the alias.
    let carrying = "\
type Pair<'a, R> = (usize, &'a mut Pdf<R>);
fn f<R>((index, _pdf): Pair<'_, R>) {
    let _ = index;
}
";
    let mut aliases = AliasIndex::new();
    collect_aliases(carrying, &mut aliases);
    assert_eq!(
        dead_pdf_carriers_with_aliases(carrying, &aliases)
            .iter()
            .map(|carrier| carrier.binding.clone())
            .collect::<Vec<_>>(),
        vec!["_pdf".to_owned()],
        "the aliased tuple's document component must still be reported"
    );
}

/// Following an alias does not substitute its type arguments, so a tuple
/// whose document arrives through a parameter -- `type Pair<R> = (usize, R)`
/// used as `Pair<&mut Pdf<X>>` -- resolves to components that no longer
/// mention the document. Dropping it there would be a miss, so the whole
/// type is kept instead.
#[test]
fn an_alias_that_carries_the_document_in_a_type_argument_is_not_lost() {
    let source = "\
type Pair<R> = (usize, R);
fn f<R>((_index, pdf): Pair<&mut Pdf<R>>) {
    let _ = pdf;
}
";
    let mut aliases = AliasIndex::new();
    collect_aliases(source, &mut aliases);
    assert!(
        dead_pdf_carriers_with_aliases(source, &aliases)
            .iter()
            .any(|carrier| carrier.binding == "_index"),
        "a substitution the alias index cannot perform must fall back to the \
         whole type rather than silently drop the document"
    );
}

/// An attribute that shares its line with the declaration must not take the
/// declaration with it: only the columns the attribute occupies are skipped.
///
/// Both orders are checked. An attribute that opens the line is the common
/// one; an attribute whose *last* line carries the declaration after its
/// closing `]` is the inverse, and it is the shape that separates a
/// column-aware skip from one that discards any line an attribute touches.
#[test]
fn an_attribute_sharing_a_line_with_the_declaration_does_not_hide_it() {
    let leading = "\
// route-hygiene-allow: _pdf -- holds the exclusive borrow, not a value.
#[inline] fn kept<R>(_pdf: &mut Pdf<R>) {}
";
    assert_eq!(
        marked_bindings(leading, "synthetic"),
        BTreeMap::from([(2, (1, "_pdf".to_owned()))]),
        "the declaration sits on the attribute's own line and must still be found"
    );
    assert!(
        unmarked_dead_pdf_carriers(leading, "synthetic").is_empty(),
        "a correctly marked declaration must not be reported"
    );

    let trailing = "\
// route-hygiene-allow: _pdf -- holds the exclusive borrow, not a value.
#[allow(
    dead_code
)] fn kept<R>(_pdf: &mut Pdf<R>) {}
";
    assert_eq!(
        marked_bindings(trailing, "synthetic"),
        BTreeMap::from([(4, (1, "_pdf".to_owned()))]),
        "a wrapped attribute whose closing line carries the declaration must \
         not skip past it"
    );
    assert!(
        unmarked_dead_pdf_carriers(trailing, "synthetic").is_empty(),
        "a correctly marked declaration must not be reported"
    );
}

/// rustfmt wraps a long attribute across lines. The marker search skipped
/// only the opening `#[` line and keyed the marker to the continuation,
/// panicking on a declaration the contract explicitly allows.
#[test]
fn a_multiline_attribute_between_marker_and_declaration_is_skipped() {
    let source = "\
// route-hygiene-allow: _pdf -- holds the exclusive borrow, not a value.
#[allow(
    dead_code
)]
fn kept<R>(_pdf: &mut Pdf<R>) {}
";
    assert_eq!(
        marked_bindings(source, "synthetic"),
        BTreeMap::from([(5, (1, "_pdf".to_owned()))]),
        "the marker must key to the declaration, not to an attribute line"
    );
    assert!(
        unmarked_dead_pdf_carriers(source, "synthetic").is_empty(),
        "a correctly marked declaration must not be reported"
    );
}

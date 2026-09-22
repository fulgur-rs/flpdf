use std::collections::BTreeMap;
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
    let mut reported: Vec<String> = Vec::new();
    for file in scanned_source_files() {
        let source = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("read {}: {error}", file.display()))
            .replace("\r\n", "\n");
        let display = file.display().to_string();
        for carrier in unmarked_dead_pdf_carriers(&source, &display) {
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

/// Parse `source` and return every `_`-prefixed binding whose type mentions
/// `Pdf`, in source order.
fn dead_pdf_carriers(source: &str) -> Vec<DeadPdfCarrier> {
    let file = syn::parse_file(source).expect("scanned source must parse as Rust");
    let mut scan = CarrierScan::default();
    Visit::visit_file(&mut scan, &file);
    scan.found
}

/// The carriers of `source` that no exclusion marker covers.
///
/// Panics on a malformed marker, and on a marker whose named binding is not
/// the carrier below it. A marker keyed to position alone would drift: replace
/// the binding under it with a different dead carrier and the old reason would
/// silently excuse the new one, which is the file-level allowlist this guard
/// replaced, reintroduced one line at a time.
fn unmarked_dead_pdf_carriers(source: &str, display: &str) -> Vec<DeadPdfCarrier> {
    let carriers = dead_pdf_carriers(source);
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
        let target = lines.iter().enumerate().skip(index + 1).find(|(_, next)| {
            let next = next.trim_start();
            !next.is_empty() && !next.starts_with("//") && !next.starts_with("#[")
        });
        let (target, _) = target.unwrap_or_else(|| {
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

/// The element types of a tuple type, seen through references and parens, so
/// `(&mut Pdf<R>, usize)` and `&(&mut Pdf<R>, usize)` both pair a tuple
/// pattern's subpatterns with their own component types.
fn tuple_components(ty: &syn::Type) -> Option<Vec<&syn::Type>> {
    match ty {
        syn::Type::Tuple(tuple) => Some(tuple.elems.iter().collect()),
        syn::Type::Reference(reference) => tuple_components(&reference.elem),
        syn::Type::Paren(paren) => tuple_components(&paren.elem),
        syn::Type::Group(group) => tuple_components(&group.elem),
        _ => None,
    }
}

/// Whether `ty` names `Pdf` anywhere, so `&Pdf<R>`, `&'a mut Pdf<R>`,
/// `crate::Pdf<R>` and `Option<&mut Pdf<R>>` are one case rather than four.
fn type_mentions_pdf(ty: &syn::Type) -> bool {
    struct PdfIdent(bool);
    impl Visit<'_> for PdfIdent {
        fn visit_ident(&mut self, node: &proc_macro2::Ident) {
            self.0 |= node == "Pdf";
        }
    }
    let mut finder = PdfIdent(false);
    Visit::visit_type(&mut finder, ty);
    finder.0
}

#[derive(Default)]
struct CarrierScan {
    owners: Vec<String>,
    found: Vec<DeadPdfCarrier>,
}

impl CarrierScan {
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
            syn::Pat::Ident(ident) if ident.ident.to_string().starts_with('_') => {
                ident.ident.to_string()
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
                let components = tuple_components(ty);
                for (index, element) in tuple.elems.iter().enumerate() {
                    let component = components
                        .as_ref()
                        .and_then(|types| types.get(index).copied())
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
        if !type_mentions_pdf(ty) {
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

impl<'ast> Visit<'ast> for CarrierScan {
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
        if ident.to_string().starts_with('_') && type_mentions_pdf(&node.ty) {
            self.found.push(DeadPdfCarrier {
                line: ident.span().start().line,
                binding: ident.to_string(),
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

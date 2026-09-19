//! Route contracts for the page-form-xobject A6/A7 accessor cutover.

use std::fs;
use std::path::PathBuf;

use syn::visit::Visit;

/// Render `page_form_xobject.rs` with every test-only item removed.
///
/// The module gates 18 items on `#[cfg(test)]` before its final `mod tests`,
/// so a text scan has to reason about attribute placement, doc comments,
/// string and comment contents, and the shape of the gated item. Parsing the
/// file with `syn` and dropping the attributed items removes that whole class
/// of edge case: compound predicates like
/// `#[cfg(all(test, feature = "qpdf-zlib-compat"))]` (used at
/// `job/overlay.rs:894`), block doc comments, multiline and raw strings, and
/// comma-delimited fields and variants all fall out of the syntax tree rather
/// than out of a hand-rolled scanner.
fn production_source() -> std::collections::BTreeSet<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/page_form_xobject.rs");
    let source = fs::read_to_string(path).expect("page_form_xobject.rs must be readable");
    production_identifiers(&source)
}

/// Collect every identifier that appears in an item which is not gated
/// test-only.
fn production_identifiers(source: &str) -> std::collections::BTreeSet<String> {
    let file = syn::parse_file(source).expect("page_form_xobject.rs must parse as Rust");
    let mut collector = ProductionCollector::default();
    collector.visit_file(&file);
    collector.identifiers
}

#[derive(Default)]
struct ProductionCollector {
    identifiers: std::collections::BTreeSet<String>,
}

impl<'ast> Visit<'ast> for ProductionCollector {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if is_test_only(item_attrs(item)) {
            return;
        }
        syn::visit::visit_item(self, item);
    }

    fn visit_ident(&mut self, ident: &'ast syn::Ident) {
        self.identifiers.insert(ident.to_string());
    }
}

fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(i) => &i.attrs,
        syn::Item::Enum(i) => &i.attrs,
        syn::Item::ExternCrate(i) => &i.attrs,
        syn::Item::Fn(i) => &i.attrs,
        syn::Item::ForeignMod(i) => &i.attrs,
        syn::Item::Impl(i) => &i.attrs,
        syn::Item::Macro(i) => &i.attrs,
        syn::Item::Mod(i) => &i.attrs,
        syn::Item::Static(i) => &i.attrs,
        syn::Item::Struct(i) => &i.attrs,
        syn::Item::Trait(i) => &i.attrs,
        syn::Item::TraitAlias(i) => &i.attrs,
        syn::Item::Type(i) => &i.attrs,
        syn::Item::Union(i) => &i.attrs,
        syn::Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

/// True when any `#[cfg(...)]` on the item requires `test`.
///
/// Covers the bare `#[cfg(test)]` and compound predicates that include it,
/// such as `#[cfg(all(test, feature = "..."))]`.
fn is_test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("cfg") {
            return false;
        }
        attr.parse_args::<syn::Meta>()
            .map(|meta| cfg_requires_test(&meta))
            .unwrap_or(false)
    })
}

fn cfg_requires_test(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) if list.path.is_ident("all") => list
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
            .map(|nested| nested.iter().any(cfg_requires_test))
            .unwrap_or(false),
        // `any(test, ...)` does not make the item test-only, and `not(test)`
        // makes it production-only; neither should be dropped here.
        _ => false,
    }
}

#[test]
fn production_page_form_xobject_uses_canonical_resolving_routes() {
    let production = production_source();
    // The A6 boundary is assigned to `page_object_helper.rs` because this
    // wrapper performs no type inspection of its own -- it hands the page to
    // the canonical helper. Keeping the non-resolving accessors out of this
    // list would let that logic creep back in undetected.
    for forbidden in [
        "resolve",
        "resolve_handle",
        "resolve_handle_ref",
        "as_dictionary",
        "as_array",
        "as_integer",
        "as_name",
        "is_null",
        "as_string",
        "as_real",
    ] {
        assert!(
            !production.contains(forbidden),
            "page_form_xobject production retains non-canonical route {forbidden}"
        );
    }
    assert!(
        production.contains("PageObjectHelper"),
        "the production wrapper must construct the canonical PageObjectHelper"
    );
    assert!(
        production.contains("get_form_xobject_for_page"),
        "the production wrapper must delegate to the canonical \
         PageObjectHelper::get_form_xobject_for_page instead of resolving handles itself"
    );
}

#[test]
fn parsing_drops_every_test_only_gate_shape() {
    let stripped = production_identifiers(
        r##"
        use core::fmt;
        #[cfg(test)]
        use std::collections::BTreeSet;

        pub(crate) fn shipped() {
            keep_me();
        }

        /** Block doc mentioning resolve_handle_ref. */
        #[cfg(test)]
        fn block_doc_helper() {
            let unmatched = "{";
            dropped_a();
        }

        #[cfg(all(test, feature = "qpdf-zlib-compat"))]
        fn compound_gate_helper() {
            dropped_b();
        }

        #[cfg(test)]
        struct GatedStruct {
            field: usize,
        }

        pub(crate) fn also_shipped() {
            keep_me_too();
        }
        "##,
    );
    assert!(stripped.contains("shipped"), "stripped: {stripped:?}");
    assert!(stripped.contains("also_shipped"), "stripped: {stripped:?}");
    assert!(!stripped.contains("dropped_a"), "stripped: {stripped:?}");
    assert!(!stripped.contains("dropped_b"), "stripped: {stripped:?}");
    assert!(!stripped.contains("BTreeSet"), "stripped: {stripped:?}");
    assert!(!stripped.contains("GatedStruct"), "stripped: {stripped:?}");
    // The block doc's `resolve_handle_ref` must not survive into the scan.
    assert!(
        !stripped.contains("resolve_handle_ref"),
        "stripped: {stripped:?}"
    );
}

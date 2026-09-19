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
    production_calls(&source)
}

/// Collect every identifier that appears in an item which is not gated
/// test-only.
fn production_calls(source: &str) -> std::collections::BTreeSet<String> {
    let file = syn::parse_file(source).expect("page_form_xobject.rs must parse as Rust");
    let mut collector = ProductionCollector::default();
    collector.visit_file(&file);
    collector.calls
}

#[derive(Default)]
struct ProductionCollector {
    calls: std::collections::BTreeSet<String>,
}

impl<'ast> Visit<'ast> for ProductionCollector {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if is_test_only(item_attrs(item)) {
            return;
        }
        syn::visit::visit_item(self, item);
    }

    fn visit_field(&mut self, field: &'ast syn::Field) {
        if is_test_only(&field.attrs) {
            return;
        }
        syn::visit::visit_field(self, field);
    }

    fn visit_variant(&mut self, variant: &'ast syn::Variant) {
        if is_test_only(&variant.attrs) {
            return;
        }
        syn::visit::visit_variant(self, variant);
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        if is_test_only(impl_item_attrs(item)) {
            return;
        }
        syn::visit::visit_impl_item(self, item);
    }

    fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
        if is_test_only(trait_item_attrs(item)) {
            return;
        }
        syn::visit::visit_trait_item(self, item);
    }

    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        if is_test_only(stmt_attrs(stmt)) {
            return;
        }
        syn::visit::visit_stmt(self, stmt);
    }

    /// `Visit` treats a macro's token stream as opaque, so a forbidden call
    /// inside `dbg!(handle.as_dictionary())` would never reach `visit_ident`.
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        syn::visit::visit_macro(self, mac);
        collect_token_calls(mac.tokens.clone(), &mut self.calls);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.calls.insert(unraw(&call.method));
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = call.func.as_ref() {
            if let Some(last) = path.path.segments.last() {
                self.calls.insert(unraw(&last.ident));
            }
        }
        syn::visit::visit_expr_call(self, call);
    }
}

/// Strip a raw identifier's `r#` prefix.
///
/// `pdf.r#resolve(...)` calls the same method as `pdf.resolve(...)`, but syn
/// renders the identifier as `r#resolve`, which would not match the forbidden
/// spelling checked below.
fn unraw(ident: &syn::Ident) -> String {
    let text = ident.to_string();
    text.strip_prefix("r#").unwrap_or(&text).to_owned()
}

fn collect_token_calls(
    tokens: proc_macro2::TokenStream,
    calls: &mut std::collections::BTreeSet<String>,
) {
    let trees: Vec<proc_macro2::TokenTree> = tokens.into_iter().collect();
    for (index, token) in trees.iter().enumerate() {
        match token {
            proc_macro2::TokenTree::Ident(ident) => {
                let followed_by_parens = matches!(
                    trees.get(index + 1),
                    Some(proc_macro2::TokenTree::Group(group))
                        if group.delimiter() == proc_macro2::Delimiter::Parenthesis
                );
                if followed_by_parens {
                    let text = ident.to_string();
                    calls.insert(text.strip_prefix("r#").unwrap_or(&text).to_owned());
                }
            }
            proc_macro2::TokenTree::Group(group) => {
                collect_token_calls(group.stream(), calls);
            }
            _ => {}
        }
    }
}

fn impl_item_attrs(item: &syn::ImplItem) -> &[syn::Attribute] {
    match item {
        syn::ImplItem::Const(i) => &i.attrs,
        syn::ImplItem::Fn(i) => &i.attrs,
        syn::ImplItem::Type(i) => &i.attrs,
        syn::ImplItem::Macro(i) => &i.attrs,
        _ => &[],
    }
}

fn trait_item_attrs(item: &syn::TraitItem) -> &[syn::Attribute] {
    match item {
        syn::TraitItem::Const(i) => &i.attrs,
        syn::TraitItem::Fn(i) => &i.attrs,
        syn::TraitItem::Type(i) => &i.attrs,
        syn::TraitItem::Macro(i) => &i.attrs,
        _ => &[],
    }
}

fn stmt_attrs(stmt: &syn::Stmt) -> &[syn::Attribute] {
    match stmt {
        syn::Stmt::Local(local) => &local.attrs,
        syn::Stmt::Item(item) => item_attrs(item),
        syn::Stmt::Expr(expr, _) => expr_attrs(expr),
        syn::Stmt::Macro(mac) => &mac.attrs,
    }
}

/// Every `syn::Expr` variant that carries attributes.
///
/// A whitelist misses valid gated forms such as
/// `#[cfg(test)] if condition { handle.resolve(); }`, so this covers the full
/// set rather than the handful the wrapper happens to use today.
fn expr_attrs(expr: &syn::Expr) -> &[syn::Attribute] {
    match expr {
        syn::Expr::Array(e) => &e.attrs,
        syn::Expr::Assign(e) => &e.attrs,
        syn::Expr::Async(e) => &e.attrs,
        syn::Expr::Await(e) => &e.attrs,
        syn::Expr::Binary(e) => &e.attrs,
        syn::Expr::Block(e) => &e.attrs,
        syn::Expr::Break(e) => &e.attrs,
        syn::Expr::Call(e) => &e.attrs,
        syn::Expr::Cast(e) => &e.attrs,
        syn::Expr::Closure(e) => &e.attrs,
        syn::Expr::Const(e) => &e.attrs,
        syn::Expr::Continue(e) => &e.attrs,
        syn::Expr::Field(e) => &e.attrs,
        syn::Expr::ForLoop(e) => &e.attrs,
        syn::Expr::Group(e) => &e.attrs,
        syn::Expr::If(e) => &e.attrs,
        syn::Expr::Index(e) => &e.attrs,
        syn::Expr::Infer(e) => &e.attrs,
        syn::Expr::Let(e) => &e.attrs,
        syn::Expr::Lit(e) => &e.attrs,
        syn::Expr::Loop(e) => &e.attrs,
        syn::Expr::Macro(e) => &e.attrs,
        syn::Expr::Match(e) => &e.attrs,
        syn::Expr::MethodCall(e) => &e.attrs,
        syn::Expr::Paren(e) => &e.attrs,
        syn::Expr::Path(e) => &e.attrs,
        syn::Expr::Range(e) => &e.attrs,
        syn::Expr::Reference(e) => &e.attrs,
        syn::Expr::Repeat(e) => &e.attrs,
        syn::Expr::Return(e) => &e.attrs,
        syn::Expr::Struct(e) => &e.attrs,
        syn::Expr::Try(e) => &e.attrs,
        syn::Expr::TryBlock(e) => &e.attrs,
        syn::Expr::Tuple(e) => &e.attrs,
        syn::Expr::Unary(e) => &e.attrs,
        syn::Expr::Unsafe(e) => &e.attrs,
        syn::Expr::While(e) => &e.attrs,
        syn::Expr::Yield(e) => &e.attrs,
        _ => &[],
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
        // `any(...)` is test-only exactly when every alternative is.
        syn::Meta::List(list) if list.path.is_ident("any") => list
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
            .map(|nested| !nested.is_empty() && nested.iter().all(cfg_requires_test))
            .unwrap_or(false),
        // `not(test)` makes the item production-only; never drop it here.
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
    // Identifier presence alone is satisfied by the wrapper's own declaration
    // (it has the same name) and by the `PageObjectHelper` import, so the
    // delegation is checked as an actual method call instead.
    assert!(
        delegates_to_helper(&read_module()),
        "the production wrapper must call helper.get_form_xobject_for_page(true) \
         on a PageObjectHelper it constructed, not resolve handles itself"
    );
}

/// True when a non-test-only function delegates the whole conversion to the
/// canonical helper: `<helper>.get_form_xobject_for_page(true)` on a binding
/// the same function obtained from `PageObjectHelper::new(...)`.
///
/// Recording only the method name would accept
/// `helper.get_form_xobject_for_page(false)`, which is a different qpdf
/// call (`getFormXObjectForPage`'s `handle_transformations`), and recording
/// it anywhere would let a `#[cfg(test)]` helper satisfy the contract while
/// the production wrapper stopped delegating.
fn delegates_to_helper(source: &str) -> bool {
    let file = syn::parse_file(source).expect("page_form_xobject.rs must parse as Rust");
    let mut probe = DelegationProbe::default();
    probe.visit_file(&file);
    probe.delegating_call_seen
}

#[derive(Default)]
struct DelegationProbe {
    /// Bindings constructed from `PageObjectHelper::new(...)` in the function
    /// currently being walked. A file-wide set would let a `helper` built in
    /// some other production function stand in for one the wrapper never
    /// constructed, so each function starts empty and a shadowing `let`
    /// removes the name again.
    helper_bindings: std::collections::BTreeSet<String>,
    delegating_call_seen: bool,
}

impl DelegationProbe {
    fn walk_scoped<F: FnOnce(&mut Self)>(&mut self, walk: F) {
        let outer = std::mem::take(&mut self.helper_bindings);
        walk(self);
        self.helper_bindings = outer;
    }
}

impl<'ast> Visit<'ast> for DelegationProbe {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if is_test_only(item_attrs(item)) {
            return;
        }
        if matches!(item, syn::Item::Fn(_)) {
            self.walk_scoped(|probe| syn::visit::visit_item(probe, item));
            return;
        }
        syn::visit::visit_item(self, item);
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        if is_test_only(impl_item_attrs(item)) {
            return;
        }
        if matches!(item, syn::ImplItem::Fn(_)) {
            self.walk_scoped(|probe| syn::visit::visit_impl_item(probe, item));
            return;
        }
        syn::visit::visit_impl_item(self, item);
    }

    fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
        if is_test_only(trait_item_attrs(item)) {
            return;
        }
        if matches!(item, syn::TraitItem::Fn(_)) {
            self.walk_scoped(|probe| syn::visit::visit_trait_item(probe, item));
            return;
        }
        syn::visit::visit_trait_item(self, item);
    }

    /// A closure body is its own scope for this purpose.
    fn visit_expr_closure(&mut self, closure: &'ast syn::ExprClosure) {
        self.walk_scoped(|probe| syn::visit::visit_expr_closure(probe, closure));
    }

    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        if is_test_only(stmt_attrs(stmt)) {
            return;
        }
        // `let <name> = PageObjectHelper::new(...)` names the receiver the
        // delegating call has to use; any other `let <name>` shadows it.
        if let syn::Stmt::Local(local) = stmt {
            if let syn::Pat::Ident(pat) = &local.pat {
                let constructed = local
                    .init
                    .as_ref()
                    .is_some_and(|init| constructs_helper(&init.expr));
                if constructed {
                    self.helper_bindings.insert(unraw(&pat.ident));
                } else {
                    self.helper_bindings.remove(&unraw(&pat.ident));
                }
            }
        }
        syn::visit::visit_stmt(self, stmt);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if unraw(&call.method) == "get_form_xobject_for_page"
            && receiver_is_helper_binding(&call.receiver, &self.helper_bindings)
            && call_passes_true(call)
        {
            self.delegating_call_seen = true;
        }
        syn::visit::visit_expr_method_call(self, call);
    }
}

fn constructs_helper(expr: &syn::Expr) -> bool {
    let syn::Expr::Call(call) = expr else {
        return false;
    };
    let syn::Expr::Path(path) = call.func.as_ref() else {
        return false;
    };
    let segments: Vec<String> = path
        .path
        .segments
        .iter()
        .map(|segment| unraw(&segment.ident))
        .collect();
    segments == ["PageObjectHelper", "new"]
}

fn receiver_is_helper_binding(
    receiver: &syn::Expr,
    bindings: &std::collections::BTreeSet<String>,
) -> bool {
    match receiver {
        syn::Expr::Path(path) => path
            .path
            .get_ident()
            .is_some_and(|ident| bindings.contains(&unraw(ident))),
        // `(&mut helper).get_form_xobject_for_page(true)` and friends.
        syn::Expr::Reference(inner) => receiver_is_helper_binding(&inner.expr, bindings),
        syn::Expr::Paren(inner) => receiver_is_helper_binding(&inner.expr, bindings),
        syn::Expr::Group(inner) => receiver_is_helper_binding(&inner.expr, bindings),
        _ => false,
    }
}

/// qpdf's `getFormXObjectForPage(handle_transformations = true)` is the
/// canonical call; `false` skips the transformation handling.
fn call_passes_true(call: &syn::ExprMethodCall) -> bool {
    matches!(
        call.args.first(),
        Some(syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Bool(syn::LitBool { value: true, .. }),
            ..
        }))
    )
}

fn read_module() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/page_form_xobject.rs");
    fs::read_to_string(path).expect("page_form_xobject.rs must be readable")
}

#[test]
fn parsing_drops_every_test_only_gate_shape() {
    let stripped = production_calls(
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
    // The set holds calls, not declarations, so the shipped functions appear
    // through what they call.
    assert!(stripped.contains("keep_me"), "stripped: {stripped:?}");
    assert!(stripped.contains("keep_me_too"), "stripped: {stripped:?}");
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

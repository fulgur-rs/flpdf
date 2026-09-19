//! Route contract for the page-form-xobject A6/A7 accessor cutover.
//!
//! `page_form_xobject.rs` ships exactly one item: the `get_form_xobject_for_page`
//! wrapper. Everything else in the module is gated on `#[cfg(test)]`.
//!
//! Earlier revisions of this contract scanned the whole file and tried to
//! decide, node by node, which parts survive into a production build. That
//! meant reimplementing `cfg` evaluation for items, fields, variants, match
//! arms and statements, tracking binding scopes across functions and blocks,
//! and guessing at macro token streams -- a static analyser living inside a
//! test, with a new gap found on every pass.
//!
//! This version asks a narrower question: find the one production item by
//! name, check that it is not gated, and judge only its body. Nothing outside
//! that function can satisfy or break the contract, so the analysis stays as
//! small as the function it describes.

use std::fs;
use std::path::PathBuf;

use syn::visit::Visit;

/// The single production entry point this module ships.
const WRAPPER: &str = "get_form_xobject_for_page";

/// qpdf's `QPDFPageObjectHelper::getFormXObjectForPage`
/// (`libqpdf/QPDFPageObjectHelper.cc:706-732`) owns the conversion; this
/// wrapper's whole job is to hand the page over and return the new object's
/// identity.
#[test]
fn the_production_wrapper_delegates_without_inspecting_handles() {
    let wrapper = production_wrapper();
    let mut audit = WrapperAudit::default();
    // Walk the statements directly: `visit_block` opens a scope, and the
    // wrapper's own body is the outermost one.
    for statement in &wrapper.block.stmts {
        audit.visit_stmt(statement);
    }

    // The A6 boundary belongs to `page_object_helper.rs` because this wrapper
    // performs no type inspection of its own. Each of these is a non-resolving
    // accessor the matrix classifies under A6, or a resolving route under A7.
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
        "as_boolean",
        "as_real_literal",
        "as_number",
        "as_rectangle",
        "as_stream_dict",
    ] {
        assert!(
            !audit.non_helper_calls.contains(forbidden),
            "the wrapper must not call {forbidden} itself: {:?}",
            audit.non_helper_calls
        );
    }

    assert!(
        audit.delegates,
        "the wrapper must call get_form_xobject_for_page(true) on a \
         PageObjectHelper it constructed: {audit:?}"
    );

    // A macro would hide its expansion from this audit. The wrapper uses none,
    // and introducing one has to come with a decision about how to check it.
    assert!(
        !audit.saw_macro,
        "a macro in the wrapper hides calls from this contract; check it \
         explicitly or keep the wrapper macro-free"
    );
}

/// Parse the module and return the sole production `fn`.
///
/// Also asserts the premise this contract rests on: that the wrapper is the
/// only item which is not gated test-only. If the module grows a second
/// production item, this fails rather than silently narrowing its scope.
fn production_wrapper() -> syn::ItemFn {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/page_form_xobject.rs");
    let source = fs::read_to_string(path).expect("page_form_xobject.rs must be readable");
    let file = syn::parse_file(&source).expect("page_form_xobject.rs must parse as Rust");

    let production: Vec<&syn::Item> = file
        .items
        .iter()
        .filter(|item| !is_test_only(item_attrs(item)))
        .filter(|item| !matches!(item, syn::Item::Use(_)))
        .collect();

    assert_eq!(
        production.len(),
        1,
        "this contract assumes one production item; found {}",
        production.len()
    );
    let syn::Item::Fn(wrapper) = production[0] else {
        panic!(
            "the production item must be a function, got {:?}",
            production[0]
        );
    };
    assert_eq!(wrapper.sig.ident, WRAPPER);
    wrapper.clone()
}

/// What the wrapper's body does, as far as this contract cares.
#[derive(Default, Debug)]
struct WrapperAudit {
    /// Bindings this body obtained from `PageObjectHelper::new(...)`.
    helper_bindings: Vec<String>,
    /// Methods called on anything that is not one of those bindings, plus
    /// every free or qualified call.
    non_helper_calls: std::collections::BTreeSet<String>,
    delegates: bool,
    saw_macro: bool,
}

impl WrapperAudit {
    /// A `let` inside a block is scoped to that block: a shadowing binding
    /// must not survive it, and a helper constructed inside must not leak out.
    fn walk_block_scoped<F: FnOnce(&mut Self)>(&mut self, walk: F) {
        let outer = self.helper_bindings.clone();
        walk(self);
        self.helper_bindings = outer;
    }
}

impl<'ast> Visit<'ast> for WrapperAudit {
    fn visit_block(&mut self, block: &'ast syn::Block) {
        self.walk_block_scoped(|audit| syn::visit::visit_block(audit, block));
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        if is_test_only(&local.attrs) {
            return;
        }
        // Rust resolves the initializer before the new binding enters scope,
        // so `let helper = helper.get_form_xobject_for_page(true)?;` still
        // calls on the *outer* helper. Walk the initializer first, then
        // rebind.
        if let Some(init) = local.init.as_ref() {
            self.visit_expr(&init.expr);
            if let Some(diverge) = init.diverge.as_ref() {
                self.visit_expr(&diverge.1);
            }
        }
        // Every name this pattern binds shadows an earlier helper of that
        // name, whether it is a plain `let helper = ...` or a destructuring
        // `let (helper,) = ...`.
        let mut bound = Vec::new();
        collect_pattern_idents(&local.pat, &mut bound);
        self.helper_bindings
            .retain(|binding| !bound.contains(binding));
        if let syn::Pat::Ident(pat) = &local.pat {
            if local
                .init
                .as_ref()
                .is_some_and(|init| constructs_helper(&init.expr))
            {
                self.helper_bindings.push(unraw(&pat.ident));
            }
        }
        self.visit_pat(&local.pat);
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        if is_test_only(&arm.attrs) {
            return;
        }
        syn::visit::visit_arm(self, arm);
    }

    fn visit_field_value(&mut self, field: &'ast syn::FieldValue) {
        if is_test_only(&field.attrs) {
            return;
        }
        syn::visit::visit_field_value(self, field);
    }

    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        if is_test_only(stmt_attrs(stmt)) {
            return;
        }
        syn::visit::visit_stmt(self, stmt);
    }

    /// An attributed expression can sit at any depth -- inside a `let`, a
    /// tuple, an array, a call argument -- so the gate is checked on every
    /// expression rather than only on whole statements.
    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        if is_test_only(expr_attrs(expr)) {
            return;
        }
        syn::visit::visit_expr(self, expr);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        let method = unraw(&call.method);
        let on_helper = receiver_is_helper(&call.receiver, &self.helper_bindings);
        if on_helper {
            if method == WRAPPER && passes_true(call) {
                self.delegates = true;
            }
        } else {
            // Receiver matters: an unrelated type's `as_dictionary()` is not
            // an A6 inspection of `Pdf`/`ObjectHandle`, but neither is it
            // something this wrapper needs, so it is recorded and the
            // assertion above explains what it means.
            self.non_helper_calls.insert(method);
        }
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = call.func.as_ref() {
            if let Some(last) = path.path.segments.last() {
                self.non_helper_calls.insert(unraw(&last.ident));
            }
        }
        syn::visit::visit_expr_call(self, call);
    }

    /// A closure body does not run unless it is called. A delegation that
    /// only appears there leaves the wrapper returning without converting the
    /// page, so it is walked for forbidden calls but cannot satisfy the
    /// delegation assertion.
    fn visit_expr_closure(&mut self, closure: &'ast syn::ExprClosure) {
        let outer_bindings = self.helper_bindings.clone();
        let outer_delegates = self.delegates;
        syn::visit::visit_expr_closure(self, closure);
        self.delegates = outer_delegates;
        self.helper_bindings = outer_bindings;
    }

    /// An `async` block does not run until it is polled, so a delegation
    /// that only appears there leaves the wrapper returning without
    /// converting the page.
    fn visit_expr_async(&mut self, block: &'ast syn::ExprAsync) {
        let outer_bindings = self.helper_bindings.clone();
        let outer_delegates = self.delegates;
        syn::visit::visit_expr_async(self, block);
        self.delegates = outer_delegates;
        self.helper_bindings = outer_bindings;
    }

    /// Same for a nested item: an inner `fn` only runs when called.
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if is_test_only(item_attrs(item)) {
            return;
        }
        let outer_bindings = std::mem::take(&mut self.helper_bindings);
        let outer_delegates = self.delegates;
        syn::visit::visit_item(self, item);
        self.delegates = outer_delegates;
        self.helper_bindings = outer_bindings;
    }

    /// A qualified path used as a value aliases the function it names --
    /// `let inspect = ObjectHandle::as_dictionary;` -- so the callee is
    /// recorded even though no call expression mentions it. A bare local
    /// (single segment) is just a variable and is left alone.
    fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
        if path.path.segments.len() > 1 {
            if let Some(last) = path.path.segments.last() {
                self.non_helper_calls.insert(unraw(&last.ident));
            }
        }
        syn::visit::visit_expr_path(self, path);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.saw_macro = true;
        syn::visit::visit_macro(self, mac);
    }
}

/// Collect every identifier a pattern binds.
fn collect_pattern_idents(pat: &syn::Pat, out: &mut Vec<String>) {
    match pat {
        syn::Pat::Ident(ident) => {
            out.push(unraw(&ident.ident));
            if let Some((_, inner)) = &ident.subpat {
                collect_pattern_idents(inner, out);
            }
        }
        syn::Pat::Tuple(tuple) => tuple
            .elems
            .iter()
            .for_each(|elem| collect_pattern_idents(elem, out)),
        syn::Pat::TupleStruct(tuple) => tuple
            .elems
            .iter()
            .for_each(|elem| collect_pattern_idents(elem, out)),
        syn::Pat::Slice(slice) => slice
            .elems
            .iter()
            .for_each(|elem| collect_pattern_idents(elem, out)),
        syn::Pat::Struct(structure) => structure
            .fields
            .iter()
            .for_each(|field| collect_pattern_idents(&field.pat, out)),
        syn::Pat::Or(alternatives) => alternatives
            .cases
            .iter()
            .for_each(|case| collect_pattern_idents(case, out)),
        syn::Pat::Reference(inner) => collect_pattern_idents(&inner.pat, out),
        syn::Pat::Paren(inner) => collect_pattern_idents(&inner.pat, out),
        syn::Pat::Type(inner) => collect_pattern_idents(&inner.pat, out),
        _ => {}
    }
}

/// See through wrappers that do not change the value.
fn unwrap_transparent(expr: &syn::Expr) -> &syn::Expr {
    match expr {
        syn::Expr::Paren(inner) => unwrap_transparent(&inner.expr),
        syn::Expr::Group(inner) => unwrap_transparent(&inner.expr),
        syn::Expr::Block(block) if block.block.stmts.len() == 1 => {
            match block.block.stmts.first() {
                Some(syn::Stmt::Expr(inner, None)) => unwrap_transparent(inner),
                _ => expr,
            }
        }
        _ => expr,
    }
}

/// `PageObjectHelper::new(<page>, <pdf>)`, however the path is spelled, where
/// the arguments are the wrapper's own parameters.
///
/// qpdf's helper is constructed on the page the caller asked about
/// (`QPDFPageObjectHelper(oh)`); handing it a different page would convert
/// the wrong one while leaving the call shape intact.
fn constructs_helper(expr: &syn::Expr) -> bool {
    let expr = unwrap_transparent(expr);
    let syn::Expr::Call(call) = expr else {
        return false;
    };
    let argument_names: Vec<String> = call
        .args
        .iter()
        .map(|arg| match unwrap_transparent(arg) {
            syn::Expr::Path(path) => path
                .path
                .get_ident()
                .map(unraw)
                .unwrap_or_else(|| "<expr>".to_owned()),
            syn::Expr::Reference(reference) => match unwrap_transparent(&reference.expr) {
                syn::Expr::Path(path) => path
                    .path
                    .get_ident()
                    .map(unraw)
                    .unwrap_or_else(|| "<expr>".to_owned()),
                _ => "<expr>".to_owned(),
            },
            _ => "<expr>".to_owned(),
        })
        .collect();
    if argument_names != ["page_ref", "pdf"] {
        return false;
    }
    let syn::Expr::Path(path) = call.func.as_ref() else {
        return false;
    };
    let mut tail = path
        .path
        .segments
        .iter()
        .rev()
        .map(|segment| unraw(&segment.ident));
    // Matching the tail accepts `crate::page_object_helper::PageObjectHelper::new`
    // as readily as the imported `PageObjectHelper::new`.
    tail.next().as_deref() == Some("new") && tail.next().as_deref() == Some("PageObjectHelper")
}

fn receiver_is_helper(receiver: &syn::Expr, bindings: &[String]) -> bool {
    match receiver {
        syn::Expr::Path(path) => path
            .path
            .get_ident()
            .is_some_and(|ident| bindings.contains(&unraw(ident))),
        syn::Expr::Reference(inner) => receiver_is_helper(&inner.expr, bindings),
        syn::Expr::Paren(inner) => receiver_is_helper(&inner.expr, bindings),
        syn::Expr::Group(inner) => receiver_is_helper(&inner.expr, bindings),
        _ => false,
    }
}

/// qpdf's `getFormXObjectForPage(handle_transformations = true)` is the
/// canonical call; `false` skips the transformation handling.
fn passes_true(call: &syn::ExprMethodCall) -> bool {
    matches!(
        call.args.first(),
        Some(syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Bool(syn::LitBool { value: true, .. }),
            ..
        }))
    )
}

fn unraw(ident: &syn::Ident) -> String {
    let text = ident.to_string();
    text.strip_prefix("r#").unwrap_or(&text).to_owned()
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

fn stmt_attrs(stmt: &syn::Stmt) -> &[syn::Attribute] {
    match stmt {
        syn::Stmt::Local(local) => &local.attrs,
        syn::Stmt::Item(item) => item_attrs(item),
        syn::Stmt::Expr(expr, _) => expr_attrs(expr),
        syn::Stmt::Macro(mac) => &mac.attrs,
    }
}

/// Every `syn::Expr` variant that carries attributes.
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
        syn::Expr::RawAddr(e) => &e.attrs,
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

/// True when any `#[cfg(...)]` on the node requires `test`.
fn is_test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args_with(
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                )
                .map(|metas| metas.iter().any(cfg_requires_test))
                .unwrap_or(false)
    })
}

fn cfg_requires_test(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) if list.path.is_ident("all") => nested(list)
            .map(|metas| metas.iter().any(cfg_requires_test))
            .unwrap_or(false),
        // `any(...)` is test-only exactly when every alternative is.
        syn::Meta::List(list) if list.path.is_ident("any") => nested(list)
            .map(|metas| !metas.is_empty() && metas.iter().all(cfg_requires_test))
            .unwrap_or(false),
        // `not(not(test))` is still test-only; `not(test)` is not.
        syn::Meta::List(list) if list.path.is_ident("not") => nested(list)
            .map(|metas| metas.len() == 1 && cfg_forbids_test(&metas[0]))
            .unwrap_or(false),
        _ => false,
    }
}

/// True when the predicate is false in every `test` build, which makes its
/// negation test-only.
fn cfg_forbids_test(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::List(list) if list.path.is_ident("not") => nested(list)
            .map(|metas| metas.len() == 1 && cfg_requires_test(&metas[0]))
            .unwrap_or(false),
        _ => false,
    }
}

/// Parse a predicate list, accepting the trailing comma Rust allows.
fn nested(list: &syn::MetaList) -> Option<syn::punctuated::Punctuated<syn::Meta, syn::Token![,]>> {
    list.parse_args_with(syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
        .ok()
}

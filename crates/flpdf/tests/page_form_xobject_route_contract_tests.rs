//! Route contracts for the page-form-xobject A6/A7 accessor cutover.

use std::fs;
use std::path::PathBuf;

/// Strip every `#[cfg(test)]`-gated item so the contracts below scan only the
/// shipped route. `page_form_xobject.rs` gates individual `use` statements and
/// helper functions on `#[cfg(test)]` well before its final `mod tests`, so
/// cutting at that module alone would let a test-only reimplementation both
/// fail the forbidden-route assertions and satisfy the delegation assertions.
fn production_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/page_form_xobject.rs");
    let source = fs::read_to_string(path)
        .expect("page_form_xobject.rs must be readable")
        .replace("\r\n", "\n");
    strip_cfg_test_items(&source)
}

/// Drop each `#[cfg(test)]` attribute together with the item it gates.
///
/// A gated `use` (or any other statement) ends at its first `;`; a gated
/// function or module ends when its brace depth returns to zero.
fn strip_cfg_test_items(source: &str) -> String {
    let mut production = String::with_capacity(source.len());
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim() != "#[cfg(test)]" {
            production.push_str(line);
            production.push('\n');
            continue;
        }
        let mut depth = 0usize;
        let mut opened = false;
        for gated in lines.by_ref() {
            depth += gated.matches('{').count();
            depth -= gated.matches('}').count().min(depth);
            if gated.contains('{') {
                opened = true;
            }
            if opened {
                if depth == 0 {
                    break;
                }
            } else if gated.trim_end().ends_with(';') {
                break;
            }
        }
    }
    production
}

#[test]
fn production_page_form_xobject_uses_canonical_resolving_routes() {
    let production = production_source();
    for forbidden in [".resolve(", ".resolve_handle(", ".resolve_handle_ref("] {
        assert!(
            !production.contains(forbidden),
            "page_form_xobject production retains non-canonical route {forbidden}"
        );
    }
    assert!(
        production.contains("PageObjectHelper::new("),
        "the production wrapper must construct the canonical PageObjectHelper"
    );
    assert!(
        production.contains(".get_form_xobject_for_page(true)?"),
        "the production wrapper must delegate to the canonical \
         PageObjectHelper::get_form_xobject_for_page instead of resolving handles itself"
    );
}

#[test]
fn stripping_drops_cfg_test_items_but_keeps_production() {
    let stripped = strip_cfg_test_items(
        "use core::fmt;\n\
         #[cfg(test)]\n\
         use std::collections::BTreeSet;\n\
         pub(crate) fn shipped() {\n    keep_me();\n}\n\
         #[cfg(test)]\n\
         fn helper() {\n    if cond {\n        dropped();\n    }\n}\n\
         #[cfg(test)]\n\
         mod tests {\n    fn inner() {}\n}\n",
    );
    assert!(stripped.contains("use core::fmt;"));
    assert!(stripped.contains("keep_me();"));
    assert!(!stripped.contains("BTreeSet"));
    assert!(!stripped.contains("dropped();"));
    assert!(!stripped.contains("fn inner()"));
}

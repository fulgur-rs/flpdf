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

/// Drop each `#[cfg(test)]` attribute together with the item it gates, and
/// with the doc comment and attributes that precede it.
///
/// Rust puts doc comments and other attributes *before* `#[cfg(test)]`, so
/// stopping at the `#[cfg(test)]` line would leave a gated helper's `///`
/// text in the scanned source -- and a forbidden spelling quoted in that
/// text would fail the contract even though the shipped route is unchanged.
///
/// A gated statement ends at its first `;`; a gated function or module ends
/// when its brace depth returns to zero.
fn strip_cfg_test_items(source: &str) -> String {
    let lines: Vec<&str> = source.split('\n').collect();
    let mut production: Vec<&str> = Vec::with_capacity(lines.len());
    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index];
        if line.trim() != "#[cfg(test)]" {
            production.push(line);
            index += 1;
            continue;
        }
        // Discard the doc comment and attributes already emitted for this item.
        while production.last().is_some_and(|previous| {
            let trimmed = previous.trim();
            trimmed.starts_with("///") || trimmed.starts_with("#[") || trimmed.starts_with("//!")
        }) {
            production.pop();
        }
        index += 1;
        let mut depth = 0usize;
        let mut opened = false;
        while index < lines.len() {
            let scan = scan_code(lines[index]);
            index += 1;
            depth += scan.opens;
            depth -= scan.closes.min(depth);
            if scan.opens > 0 {
                opened = true;
            }
            if opened {
                if depth == 0 {
                    break;
                }
            } else if scan.ends_statement {
                break;
            }
        }
    }
    production.join("\n")
}

#[derive(Default)]
struct LineScan {
    opens: usize,
    closes: usize,
    ends_statement: bool,
}

/// Count braces that are Rust syntax, skipping string, char, and comment text.
///
/// A gated helper may quote an unmatched brace -- `let fragment = "{";`, or a
/// comment mentioning `}` -- and counting those would either consume the
/// production items that follow or stop the skip early and scan test-only
/// code as production.
fn scan_code(line: &str) -> LineScan {
    let mut scan = LineScan::default();
    let bytes = line.as_bytes();
    let mut i = 0usize;
    let mut last_code = None;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => break,
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                last_code = Some(b'"');
                continue;
            }
            // A char literal is at most `'\x41'`; anything longer is a lifetime.
            b'\'' => {
                let close = line[i + 1..]
                    .char_indices()
                    .take(6)
                    .find(|(_, c)| *c == '\'')
                    .map(|(offset, _)| i + 1 + offset);
                if let Some(close) = close {
                    i = close + 1;
                    last_code = Some(b'\'');
                    continue;
                }
                i += 1;
            }
            b'{' => {
                scan.opens += 1;
                last_code = Some(b'{');
                i += 1;
            }
            b'}' => {
                scan.closes += 1;
                last_code = Some(b'}');
                i += 1;
            }
            other => {
                if !other.is_ascii_whitespace() {
                    last_code = Some(other);
                }
                i += 1;
            }
        }
    }
    scan.ends_statement = last_code == Some(b';');
    scan
}

#[test]
fn production_page_form_xobject_uses_canonical_resolving_routes() {
    let production = production_source();
    // The A6 boundary is assigned to `page_object_helper.rs` because this
    // wrapper performs no type inspection of its own -- it hands the page to
    // the canonical helper. Keeping the non-resolving accessors out of this
    // list would let that logic creep back in undetected.
    for forbidden in [
        ".resolve(",
        ".resolve_handle(",
        ".resolve_handle_ref(",
        ".as_dictionary(",
        ".as_array(",
        ".as_integer(",
        ".as_name(",
        ".is_null(",
        ".as_string(",
        ".as_real(",
    ] {
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
fn stripping_ignores_braces_inside_strings_and_comments() {
    // An unmatched open brace in a gated helper's string must not raise the
    // depth counter: a naive count never returns to zero and swallows the
    // production item that follows.
    let stripped = strip_cfg_test_items(
        "#[cfg(test)]\n\
         fn helper() {\n    let fragment = \"{\";\n}\n\
         pub(crate) fn shipped() {\n    keep_me();\n}\n",
    );
    assert!(stripped.contains("keep_me();"), "stripped: {stripped:?}");
    assert!(!stripped.contains("fragment"), "stripped: {stripped:?}");

    // An unmatched close brace must not lower it either: a naive count ends
    // the skip early and scans the rest of the gated helper as production.
    let stripped = strip_cfg_test_items(
        "#[cfg(test)]\n\
         fn helper() {\n    let closing = \"}\";\n    test_only_route();\n}\n\
         pub(crate) fn shipped() {\n    keep_me();\n}\n",
    );
    assert!(stripped.contains("keep_me();"), "stripped: {stripped:?}");
    assert!(
        !stripped.contains("test_only_route"),
        "stripped: {stripped:?}"
    );

    // The same for a comment.
    let stripped = strip_cfg_test_items(
        "#[cfg(test)]\n\
         fn helper() {\n    // an unmatched } in prose\n    test_only_route();\n}\n\
         pub(crate) fn shipped() {\n    keep_me();\n}\n",
    );
    assert!(stripped.contains("keep_me();"), "stripped: {stripped:?}");
    assert!(
        !stripped.contains("test_only_route"),
        "stripped: {stripped:?}"
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

use std::fs;
use std::path::PathBuf;

fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

#[test]
fn rewrite_renumber_is_owned_by_the_writer_module() {
    let source = source_root();
    assert!(
        !source.join("rewrite_renumber.rs").exists(),
        "the crate-level rewrite_renumber route must be removed"
    );
    assert!(
        source.join("writer/rewrite_renumber.rs").is_file(),
        "rewrite_renumber must live under the writer module"
    );

    let lib = fs::read_to_string(source.join("lib.rs")).expect("lib.rs must be readable");
    assert!(
        !lib.contains("mod rewrite_renumber;"),
        "lib.rs must not declare the old crate-level module"
    );

    let writer = fs::read_to_string(source.join("writer.rs")).expect("writer.rs must be readable");
    assert!(
        writer.contains("mod rewrite_renumber;"),
        "writer.rs must declare the writer-owned module"
    );
}

/// Find the end (one past the closing brace) of the item body starting at
/// `body`'s leading `{`, treating braces inside `//` line comments and
/// `"..."` / `b"..."` string literals as inert rather than counting them.
/// `rewrite_renumber.rs` uses only line comments and (byte-)string literals
/// today (no block comments, raw strings, or char literals in its
/// `#[cfg(test)]` regions); this scan covers exactly those forms rather than
/// a full Rust tokenizer.
fn cfg_test_item_body_end(body: &str) -> Option<usize> {
    #[derive(PartialEq)]
    enum State {
        Code,
        LineComment,
        StringLiteral,
    }

    let mut state = State::Code;
    let mut depth: usize = 0;
    let mut chars = body.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        match state {
            State::Code => match ch {
                '/' if chars.peek().map(|&(_, c)| c) == Some('/') => {
                    state = State::LineComment;
                }
                '"' => state = State::StringLiteral,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + ch.len_utf8());
                    }
                }
                _ => {}
            },
            State::LineComment => {
                if ch == '\n' {
                    state = State::Code;
                }
            }
            State::StringLiteral => match ch {
                '\\' => {
                    chars.next();
                }
                '"' => state = State::Code,
                _ => {}
            },
        }
    }
    None
}

/// Remove every `#[cfg(test)]`-attributed item's full body (not just the
/// text before the first marker) so a scan of the remainder covers all
/// production code, including any that follows an early test-only item.
fn strip_cfg_test_items(source: &str) -> String {
    let mut production = String::new();
    let mut rest = source;
    while let Some(marker_pos) = rest.find("#[cfg(test)]") {
        production.push_str(&rest[..marker_pos]);
        let after_marker = &rest[marker_pos..];
        let brace_start = after_marker
            .find('{')
            .expect("a #[cfg(test)] item must have a body");
        let body = &after_marker[brace_start..];
        let end =
            cfg_test_item_body_end(body).expect("a #[cfg(test)] item must have a balanced body");
        rest = &after_marker[brace_start + end..];
    }
    production.push_str(rest);
    production
}

/// True if `haystack` contains `needle` as a whole identifier/path token —
/// not merely as a substring — so `CatalogFirstRenumber::` does not
/// spuriously match inside `CanonicalCatalogFirstRenumber::`.
fn contains_token(haystack: &str, needle: &str) -> bool {
    let is_ident_char = |c: char| c.is_alphanumeric() || c == '_';
    haystack.match_indices(needle).any(|(i, _)| {
        let before_ok = haystack[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !is_ident_char(c));
        let after_ok = haystack[i + needle.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_ident_char(c));
        before_ok && after_ok
    })
}

#[test]
fn production_renumber_route_has_only_the_canonical_handle_engine() {
    let source = fs::read_to_string(source_root().join("writer/rewrite_renumber.rs"))
        .expect("rewrite_renumber.rs must be readable");
    let production = strip_cfg_test_items(&source);

    assert!(
        production.contains("CanonicalCatalogFirstRenumber"),
        "production renumbering must retain the canonical handle engine"
    );
    for forbidden in [
        "struct CatalogFirstRenumber",
        "impl CatalogFirstRenumber",
        "CatalogFirstRenumber",
        "collect_qpdf_enqueue_refs",
    ] {
        assert!(
            !contains_token(&production, forbidden),
            "production renumbering still contains obsolete raw engine token {forbidden:?}"
        );
    }
}

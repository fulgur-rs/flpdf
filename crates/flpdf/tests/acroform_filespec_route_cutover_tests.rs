use std::fs;
use std::path::Path;

fn cfg_test_item_body_end(body: &str) -> Option<usize> {
    #[derive(PartialEq)]
    enum State {
        Code,
        LineComment,
        StringLiteral,
    }

    let mut state = State::Code;
    let mut depth = 0usize;
    let mut chars = body.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        match state {
            State::Code => match character {
                '/' if chars.peek().map(|&(_, next)| next) == Some('/') => {
                    state = State::LineComment;
                }
                '"' => state = State::StringLiteral,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(index + character.len_utf8());
                    }
                }
                _ => {}
            },
            State::LineComment => {
                if character == '\n' {
                    state = State::Code;
                }
            }
            State::StringLiteral => match character {
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

fn production_source(path: &str) -> String {
    let source = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(path))
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
        .replace("\r\n", "\n");
    strip_cfg_test_items(&source)
}

#[test]
fn acroform_filespec_embedded_and_signature_production_routes_are_handle_native() {
    for path in [
        "acroform_document_helper.rs",
        "filespec_helper/filespec.rs",
        "filespec_helper/embedded_file_stream.rs",
        "embedded_files.rs",
        "signatures.rs",
    ] {
        let source = production_source(path);
        for forbidden in [
            ".resolve(",
            "resolve_handle(",
            "resolve_handle_ref(",
            ".get_key(",
            ".has_key(",
        ] {
            assert!(
                !source.contains(forbidden),
                "{path} still contains the legacy resolver/accessor bridge {forbidden}"
            );
        }
        if path == "signatures.rs" {
            assert!(
                !source.contains(".is_null("),
                "{path} still contains a non-resolving null observation"
            );
        }
    }
}

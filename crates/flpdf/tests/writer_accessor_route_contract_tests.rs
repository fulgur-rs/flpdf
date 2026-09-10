/// Find the end (one past the closing brace) of a Rust item body, ignoring
/// braces in line comments and ordinary string literals.
fn item_body_end(body: &str) -> Option<usize> {
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
                    depth = depth.checked_sub(1)?;
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

/// Remove every cfg(test)-attributed item so production-only route assertions
/// remain valid when tests are interleaved with later writer helpers.
fn strip_cfg_test_items(source: &str) -> String {
    let mut production = String::new();
    let mut rest = source;
    while let Some(marker_pos) = rest.find("#[cfg(test)]") {
        production.push_str(&rest[..marker_pos]);
        let after_marker = &rest[marker_pos..];
        let brace_start = after_marker
            .find('{')
            .expect("a cfg(test) item must have a body");
        let body = &after_marker[brace_start..];
        let end = item_body_end(body).expect("a cfg(test) item must balance braces");
        rest = &after_marker[brace_start + end..];
    }
    production.push_str(rest);
    production
}

fn function_body<'a>(source: &'a str, function_name: &str) -> &'a str {
    let signature = format!("fn {function_name}");
    let start = source
        .find(&signature)
        .unwrap_or_else(|| panic!("{function_name} must be present"));
    let after_signature = &source[start..];
    let brace_start = after_signature
        .find('{')
        .unwrap_or_else(|| panic!("{function_name} must have a body"));
    let body = &after_signature[brace_start..];
    let end = item_body_end(body).unwrap_or_else(|| panic!("{function_name} must balance braces"));
    &body[..end]
}

fn assert_local_stream_resolution(body: &str, handle_name: &str, expected: usize) {
    // Formatting may split a receiver and its method across lines, as in
    // `source_handle\n    .as_stream_dict()`. Compact only whitespace so the
    // assertion remains about the receiver, not about rustfmt's line wrapping.
    let body = body.split_whitespace().collect::<String>();
    let resolution = format!("{handle_name}.try_dereference()?");
    let stream_observation = format!("{handle_name}.as_stream_dict(");
    let mut search_from = 0;
    let mut observations = 0;
    while let Some(relative) = body[search_from..].find(&stream_observation) {
        let stream_offset = search_from + relative;
        assert!(
            body[search_from..stream_offset].contains(&resolution),
            "{handle_name} must be resolved in the same control-flow segment before its stream observation"
        );
        observations += 1;
        search_from = stream_offset + stream_observation.len();
    }
    assert_eq!(
        observations, expected,
        "unexpected number of {handle_name} stream observations"
    );
}

#[test]
fn standard_writer_production_uses_canonical_accessor_routes() {
    let source = include_str!("../src/writer.rs").replace("\r\n", "\n");
    let production = strip_cfg_test_items(&source);

    for forbidden in [
        ".resolve(",
        ".resolve_handle(",
        ".resolve_handle_ref(",
        ".get_key(",
        ".has_key(",
        ".is_null(",
    ] {
        assert!(
            !production.contains(forbidden),
            "writer production retains legacy route {forbidden}"
        );
    }
    assert!(
        production.contains("try_dereference")
            && production.contains("try_get_key")
            && production.contains("try_is_null"),
        "writer production must use canonical resolving accessors"
    );
}

#[test]
fn standard_writer_stream_observations_follow_resolution() {
    let source = strip_cfg_test_items(&include_str!("../src/writer.rs").replace("\r\n", "\n"));
    let pclm_body = function_body(&source, "write_pclm");
    assert_local_stream_resolution(pclm_body, "source_handle", 1);

    let standard_body = function_body(&source, "emit_canonical_pdf_inner");
    assert_local_stream_resolution(standard_body, "object_handle", 2);
    assert_local_stream_resolution(standard_body, "source_handle", 1);
}

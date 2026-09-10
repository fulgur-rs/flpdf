//! Route contracts for the bounded linearization accessor cutovers.

use std::fs;
use std::path::PathBuf;

fn production_source(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    let source = strip_cfg_test_items(
        &fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
            .replace("\r\n", "\n"),
    );
    if relative == "src/linearization/show.rs" {
        remove_test_fn(&source, "is_linearized", "read_lin_parameters")
    } else {
        source
    }
}

fn remove_test_fn(source: &str, test_name: &str, next_production_fn: &str) -> String {
    let marker = format!("\n#[cfg(test)]\nfn {test_name}");
    let Some(start) = source.find(&marker) else {
        return source.to_owned();
    };
    let remainder = &source[start + marker.len()..];
    let next = format!("\nfn {next_production_fn}");
    let Some(next_offset) = remainder.find(&next) else {
        return source.to_owned();
    };
    format!("{}{}", &source[..start], &remainder[next_offset..])
}

fn strip_cfg_test_items(source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let mut output = String::new();
    let mut index = 0;
    while index < lines.len() {
        if lines[index].trim() == "#[cfg(test)]" {
            index += 1;
            while index < lines.len() && lines[index].trim().is_empty() {
                index += 1;
            }
            if index >= lines.len() {
                break;
            }
            let item = lines[index].trim_start();
            if item.starts_with("use ") || item.starts_with("type ") {
                index += 1;
                continue;
            }
            let mut depth = 0usize;
            let mut saw_body = false;
            while index < lines.len() {
                for byte in lines[index].bytes() {
                    match byte {
                        b'{' => {
                            depth += 1;
                            saw_body = true;
                        }
                        b'}' if saw_body => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                }
                index += 1;
                if saw_body && depth == 0 {
                    break;
                }
            }
            continue;
        }
        output.push_str(lines[index]);
        output.push('\n');
        index += 1;
    }
    output
}

#[test]
fn linearization_production_consumers_use_resolving_accessor_routes() {
    for file in [
        "src/linearization/check.rs",
        "src/linearization/show.rs",
        "src/linearization/plan.rs",
    ] {
        let source = production_source(file);
        assert!(
            source.contains(".try_"),
            "{file} must use canonical fallible accessors"
        );
        for forbidden in [
            ".resolve(",
            ".resolve_handle(",
            ".resolve_handle_ref(",
            ".get_key(",
            ".has_key(",
            ".as_dictionary(",
            ".as_array(",
            ".as_integer(",
            ".as_name(",
            ".is_null(",
        ] {
            assert!(
                !source.contains(forbidden),
                "{file} production retains non-canonical route {forbidden}"
            );
        }
    }
}

const LINEARIZATION_MODULE: &str = include_str!("../src/linearization/mod.rs");

fn has_module_visibility(module: &str, visibility: &str) -> bool {
    LINEARIZATION_MODULE
        .lines()
        .any(|line| line.trim() == format!("{visibility} mod {module};"))
}

#[test]
fn qpdf_writer_internal_modules_are_not_public() {
    for module in [
        "back_patch",
        "hint_page",
        "hint_shared",
        "hint_stream",
        "part1",
        "plan",
        "renumber",
        "writer",
    ] {
        assert!(
            has_module_visibility(module, "pub(crate)"),
            "{module} must stay available inside flpdf"
        );
        assert!(
            !has_module_visibility(module, "pub"),
            "{module} has no qpdf 11.9.0 public counterpart"
        );
    }
}

#[test]
fn qpdf_public_linearization_inspection_modules_remain_public() {
    assert!(has_module_visibility("check", "pub"));
    assert!(has_module_visibility("show", "pub"));
}

#[test]
fn writer_only_types_are_not_publicly_reexported() {
    for module in [
        "back_patch",
        "hint_page",
        "hint_shared",
        "hint_stream",
        "part1",
        "plan",
        "renumber",
        "writer",
    ] {
        assert!(
            !LINEARIZATION_MODULE
                .lines()
                .any(|line| line.trim().starts_with(&format!("pub use {module}::"))),
            "{module} items must not be part of the external linearization API"
        );
    }
}

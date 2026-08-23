use flpdf::Pdf;
use std::io::Cursor;

#[test]
fn qpdf_named_handle_enumeration_has_no_legacy_alias() {
    let production = include_str!("../src/reader.rs");
    assert!(!production.contains("pub fn get_all_object_handles"));

    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/minimal.pdf").as_slice(),
    ))
    .unwrap();

    assert!(!pdf.get_all_objects().unwrap().is_empty());
}

#[test]
fn qpdf_named_trailer_surface_returns_a_live_handle() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/minimal.pdf").as_slice(),
    ))
    .unwrap();

    assert!(pdf.trailer().is_direct());
    assert!(pdf.trailer().get_key(b"/Root").is_indirect());
}

#[test]
fn qpdf_named_resolve_surface_resolves_a_handle_in_place() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/minimal.pdf").as_slice(),
    ))
    .unwrap();
    // `root_handle()` already resolves its own candidate, so a fresh,
    // still-unresolved handle from `get_object_handle` is what actually
    // exercises `resolve()`'s own effect rather than one it inherits.
    let root_ref = pdf.root_ref().unwrap();
    let root = pdf.get_object_handle(root_ref);
    assert!(
        !root.is_resolved(),
        "a fresh indirect handle starts unresolved"
    );

    pdf.resolve(&root).unwrap();

    assert!(
        root.is_resolved(),
        "resolve() must resolve the handle in place"
    );
    assert!(root.get_key(b"/Pages").is_indirect());
}

#[test]
fn qpdf_cutover_has_no_legacy_handle_aliases() {
    let sources = [("reader.rs", include_str!("../src/reader.rs"))];
    let forbidden = [
        "pub fn resolve_object_handle(",
        "pub fn resolve_object_handle_to_terminal(",
        "pub fn resolve_object_handle_to_terminal_ref(",
    ];

    for (name, source) in sources {
        for needle in forbidden {
            assert!(
                !source.contains(needle),
                "legacy raw-object route marker {needle:?} remains in {name}"
            );
        }
    }
}

#[test]
fn page_form_xobject_test_helpers_use_the_canonical_handle_route() {
    let source = include_str!("../src/page_form_xobject.rs");
    for legacy in [
        "use crate::{Matrix, Object};",
        "resolve_borrowed",
        "resolve_object(",
        "Object::",
    ] {
        assert!(
            !source.contains(legacy),
            "page_form_xobject still contains the raw route marker {legacy:?}"
        );
    }
}

#[test]
fn thread_bead_production_uses_the_canonical_handle_route() {
    let source = include_str!("../src/thread_bead_p.rs");
    // Split at the `mod tests` boundary, not the first `#[cfg(test)]`: an
    // earlier, narrower `#[cfg(test)]` gates only a single test-only import
    // line above every production function, so stopping there would leave
    // `production` covering just the module doc and imports.
    let (before_tests, _) = source
        .split_once("mod tests {")
        .expect("thread_bead_p has a test module");
    // Filter by trimmed line content, not a literal multi-line `\n`-joined
    // substring: `include_str!` reflects the file's on-disk line endings, and
    // a `\r\n` checkout (Windows) would otherwise silently fail to match.
    let production: String = before_tests
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed != "#[cfg(test)]" && trimmed != "use crate::{Dictionary, Object};"
        })
        .collect::<Vec<_>>()
        .join("\n");
    for legacy in [
        "resolve_borrowed",
        "resolve_object(",
        "resolve_ref_chain",
        "Object::",
        "pdf.set_object(",
        "use crate::{Dictionary, Object",
    ] {
        assert!(
            !production.contains(legacy),
            "thread_bead_p production still contains the raw route marker {legacy:?}"
        );
    }
}

#[test]
fn thread_bead_tests_have_no_raw_snapshot_route() {
    let source = include_str!("../src/thread_bead_p.rs");
    for legacy in [
        "Object::",
        "Dictionary",
        "materialize",
        "parse_object",
        "set_object(",
        "resolve_borrowed",
        "resolve_object(",
    ] {
        assert!(
            !source.contains(legacy),
            "thread_bead_p still contains the raw test route marker {legacy:?}"
        );
    }
}

#[test]
fn inherited_attributes_production_uses_the_canonical_handle_route() {
    let source = include_str!("../src/optimization/inherited_attrs.rs");
    let production = source
        .split("#[cfg(test)]")
        .next()
        .expect("inherited_attrs has a production section");
    for legacy in [
        "resolve_borrowed",
        "resolve_object(",
        "terminal_ref_of_chain",
        "Object::",
        "Dictionary",
        "pdf.set_object(",
    ] {
        assert!(
            !production.contains(legacy),
            "inherited_attrs production still contains raw route marker {legacy:?}"
        );
    }
}

#[test]
fn optimization_production_uses_the_canonical_handle_route() {
    let source = include_str!("../src/optimization.rs");
    let production = source
        .split("#[cfg(test)]")
        .next()
        .expect("optimization has a production section");
    for legacy in [
        "use crate::{Object,",
        "resolve_borrowed",
        "resolve_object(",
        "Object::",
        "pdf.set_object(",
        "materialize(",
    ] {
        assert!(
            !production.contains(legacy),
            "optimization production still contains raw route marker {legacy:?}"
        );
    }
}

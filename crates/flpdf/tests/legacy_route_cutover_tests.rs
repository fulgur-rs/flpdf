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
fn acroform_top_level_field_uses_the_canonical_form_field_helper() {
    let acroform = include_str!("../src/acroform_document_helper.rs");
    let page_specs = include_str!("../src/job/page_specs.rs");
    assert!(
        !acroform.contains("pub fn get_top_level_field(&mut self, start: ObjectRef)"),
        "AcroFormDocumentHelper still exposes the duplicate raw top-level-field route"
    );
    assert!(
        page_specs.contains("FormFieldObjectHelper::new"),
        "page-spec field collection must use the canonical FormFieldObjectHelper"
    );
}

#[test]
fn acroform_field_prune_production_uses_the_canonical_handle_route() {
    let source = include_str!("../src/job/acroform_field_prune.rs");
    let production = source
        .split_once("mod tests {")
        .expect("acroform_field_prune has a test module")
        .0;

    for legacy in [
        "resolve_borrowed",
        "resolve_to_terminal",
        "Object::",
        "set_object(",
    ] {
        assert!(
            !production.contains(legacy),
            "acroform_field_prune production still contains the raw route marker {legacy:?}"
        );
    }
    assert!(
        production.contains("replace_key("),
        "acroform_field_prune production must mutate ObjectHandle dictionaries"
    );
    assert!(
        production.contains("mark_object_handle_dirty"),
        "acroform_field_prune production must mark handle mutations dirty"
    );
}

#[test]
fn json_sections_production_uses_canonical_helper_routes() {
    let source = include_str!("../src/job/json_sections.rs");
    for legacy in [
        "use crate::object::{Object",
        "resolve_borrowed",
        "set_object(",
        "NameTree::new",
        "lift_object_to_handle",
        "Object::",
    ] {
        assert!(
            !source.contains(legacy),
            "job/json_sections.rs still contains the raw route marker {legacy:?}"
        );
    }
    assert!(
        source.contains("PageObjectHelper"),
        "JSON pages must use the canonical PageObjectHelper route"
    );
    assert!(
        source.contains("embedded_files"),
        "JSON attachments must use the canonical EmbeddedFileDocumentHelper route"
    );
}

#[test]
fn attachment_list_production_uses_one_hop_handle_resolution() {
    let source = include_str!("../src/job/attachment_list.rs");
    let production = source
        .split_once("mod tests {")
        .expect("attachment_list has a test module")
        .0;
    assert!(
        !production.contains("resolve_to_terminal"),
        "attachment_list production still chases the legacy terminal bridge"
    );
    assert!(
        !production.contains("resolve_borrowed"),
        "attachment_list production still reads the raw Object route"
    );
    assert!(
        production.contains("pdf.resolve(&stream)"),
        "attachment_list production must resolve EF entries through ObjectHandle"
    );
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
        "resolve_handle_chain",
        "as_reference()",
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
fn inherited_attributes_module_has_no_raw_snapshot_or_redirect_route() {
    let source = include_str!("../src/optimization/inherited_attrs.rs");
    for legacy in [
        "resolve_borrowed",
        "resolve_object(",
        "resolve_handle_chain",
        "as_reference()",
        "Object::",
        "use crate::{Dictionary",
        "pdf.set_object(",
    ] {
        assert!(
            !source.contains(legacy),
            "inherited_attrs still contains the raw or redirect marker {legacy:?}"
        );
    }
}

#[test]
fn page_splice_module_has_no_raw_snapshot_route() {
    let source = include_str!("../src/page_splice.rs");
    for legacy in [
        "use crate::Object;",
        "resolve_borrowed",
        "resolve_object(",
        "Object::",
        "Dictionary",
        "materialize(",
        "set_object(",
    ] {
        assert!(
            !source.contains(legacy),
            "page_splice still contains the raw route marker {legacy:?}"
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

#[test]
fn resources_form_pruning_production_uses_the_handle_route() {
    let source = include_str!("../src/resources.rs");
    let prepass = source
        .split("fn collect_used_names_for_form")
        .next()
        .expect("resources has a Form pruning pre-pass");
    for legacy in [
        "resolve_ref_chain",
        "resolve_resource_reference",
        "resolve_object(",
        "let Object::Stream",
        "Object::Dictionary(resources)",
        "Object::Reference(reference)",
        "pdf.set_object(form_ref",
    ] {
        assert!(
            !prepass.contains(legacy),
            "resources Form pruning pre-pass still contains raw route marker {legacy:?}"
        );
    }
}

#[test]
fn page_closure_production_uses_the_canonical_handle_route() {
    let source = include_str!("../src/page_closure.rs");
    let production = source
        .split_once("#[cfg(test)]")
        .expect("page_closure has a test module")
        .0;

    for legacy in ["resolve_borrowed", "Object::"] {
        assert!(
            !production.contains(legacy),
            "page_closure production still contains the raw route marker {legacy:?}"
        );
    }
    for canonical in [
        "get_object_handle",
        "try_get_keys",
        "try_is_dictionary_of_type",
    ] {
        assert!(
            production.contains(canonical),
            "page_closure production must use the canonical handle API {canonical:?}"
        );
    }
}

#[test]
fn overlay_appearance_stream_has_no_raw_snapshot_route() {
    let source = include_str!("../src/overlay_appearance_stream.rs");
    for legacy in [
        "Object as PdfObject",
        "type Object =",
        "resolve_object(",
        "set_object(",
        "PdfObject::",
        "into_stream()",
        "into_dict()",
        // General raw-route markers (not just the former `PdfObject` alias),
        // matching the neighboring cutover guards in this file.
        "use crate::Object",
        "Object::Dictionary",
        "Object::Stream",
        "resolve_borrowed",
        "materialize(",
    ] {
        assert!(
            !source.contains(legacy),
            "overlay_appearance_stream still contains the raw route marker {legacy:?}"
        );
    }
}

#[test]
fn page_extract_production_uses_the_canonical_handle_route() {
    let source = include_str!("../src/page_extract.rs");
    let production = source
        .split_once("#[cfg(test)]")
        .expect("page_extract has a test module")
        .0;
    let production: String = production
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    for legacy in [
        "use crate::{Dictionary",
        "Object,",
        "Object::",
        "resolve_object(",
        "set_object(",
        "Result<Dictionary>",
    ] {
        assert!(
            !production.contains(legacy),
            "page_extract production still contains the raw route marker {legacy:?}"
        );
    }
    for canonical in [
        "get_object_handle",
        "replace_object_handle",
        "shallow_copy",
        "try_get_key",
    ] {
        assert!(
            production.contains(canonical),
            "page_extract production must use the canonical handle API {canonical:?}"
        );
    }
}

#[test]
fn page_object_helper_has_no_legacy_resources_projection() {
    let source = include_str!("../src/page_object_helper.rs");
    assert!(
        !source.contains("pub fn resources("),
        "PageObjectHelper must expose the live get_resources route only"
    );
    assert!(
        !source.contains("fn object_type_name("),
        "raw Object type-name helper should disappear with resources()"
    );
    assert!(
        source.contains("pub fn get_resources("),
        "PageObjectHelper canonical get_resources route must remain"
    );
}

#[test]
fn form_xobject_placement_resolves_properties_through_handles() {
    let source = include_str!("../src/page_object_helper.rs");
    let production = source
        .split("#[cfg(test)]")
        .next()
        .expect("PageObjectHelper source should have a production section");
    let function = production
        .split("pub fn get_matrix_for_form_xobject_placement")
        .nth(1)
        .and_then(|rest| rest.split("    /// Build qpdf's `placeFormXObject`").next())
        .expect("Form placement production function should remain present");

    assert!(
        !function.contains("resolve_to_terminal"),
        "Form placement must not use the non-qpdf terminal-resolution bridge"
    );
    assert!(
        function.contains("self.pdf.resolve(&bbox)"),
        "Form /BBox must be resolved through the canonical handle"
    );
    assert!(
        function.contains("self.pdf.resolve(&form_matrix)"),
        "Form /Matrix must be resolved through the canonical handle"
    );
}

#[test]
fn qtest_test_39_uses_the_canonical_page_resource_route() {
    let source = include_str!("../../flpdf-qtest-tools/src/driver/test_34_41.rs");
    let production: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    for legacy in [
        ".resources()",
        "resolve_chain",
        "Object::Dictionary",
        "Dictionary,",
    ] {
        assert!(
            !production.contains(legacy),
            "qtest test_39 still contains the raw resource route marker {legacy:?}"
        );
    }
    for canonical in [
        "get_resources(false)",
        "resolve_to_terminal",
        "as_dictionary",
    ] {
        assert!(
            production.contains(canonical),
            "qtest test_39 must use the canonical handle route {canonical:?}"
        );
    }
}

use flpdf::Pdf;
use std::io::Cursor;

#[test]
fn signatures_production_uses_the_canonical_handle_route() {
    let source = include_str!("../src/signatures.rs");
    for legacy in [
        "resolve_borrowed",
        "resolve_object(",
        "Object::",
        "use crate::{Dictionary",
        "set_object(",
        "materialize(",
    ] {
        assert!(
            !source.contains(legacy),
            "signatures production still contains the raw route marker {legacy:?}"
        );
    }
    for canonical in ["ObjectHandle", "resolve_handle", "mark_object_handle_dirty"] {
        assert!(
            source.contains(canonical),
            "signatures production must use the canonical handle marker {canonical:?}"
        );
    }
}

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
fn reader_production_has_no_raw_materialization_bridge() {
    let source = include_str!("../src/reader.rs");
    let production = source
        .split_once("\n#[cfg(test)]\nmod tests")
        .expect("reader.rs has a test module")
        .0;

    for legacy in [
        "legacy_materialized_memo",
        "legacy_materialized_replacement_refs",
        "reconcile_legacy_materialized_memos",
        "pub fn resolve_object(",
        "pub fn resolve_borrowed(",
        "lift_object_to_handle",
    ] {
        assert!(
            !production.contains(legacy),
            "reader production still contains the raw materialization bridge {legacy:?}"
        );
    }
}

#[test]
fn structure_tree_removed_page_production_uses_object_handles() {
    let source = include_str!("../src/struct_tree_pg.rs");
    let production = source
        .split_once("\n#[cfg(test)]\nmod tests")
        .expect("struct_tree_pg.rs has a test module")
        .0;

    for legacy in [
        "resolve_borrowed",
        "resolve_object(",
        "Object::",
        "set_object(",
        "Dictionary",
    ] {
        assert!(
            !production.contains(legacy),
            "struct_tree_pg production still contains the raw route marker {legacy:?}"
        );
    }
    for canonical in [
        "ObjectHandle",
        "get_object_handle",
        "replace_key",
        "remove_key",
    ] {
        assert!(
            production.contains(canonical),
            "struct_tree_pg production must use the canonical handle marker {canonical:?}"
        );
    }
}

#[test]
fn objr_annotation_removed_page_production_uses_object_handles() {
    let source = include_str!("../src/objr_obj_annot_p.rs");
    let production = source
        .split_once("\n#[cfg(test)]\nmod tests")
        .expect("objr_obj_annot_p.rs has a test module")
        .0;

    for legacy in [
        "resolve_ref_chain",
        "resolve_borrowed",
        "resolve_object(",
        "Object::",
        "set_object(",
        "Dictionary",
    ] {
        assert!(
            !production.contains(legacy),
            "objr_obj_annot_p production still contains the raw route marker {legacy:?}"
        );
    }
    for canonical in [
        "ObjectHandle",
        "resolve_to_terminal_ref",
        "replace_key",
        "remove_key",
    ] {
        assert!(
            production.contains(canonical),
            "objr_obj_annot_p production must use the canonical handle marker {canonical:?}"
        );
    }
}

#[test]
fn subset_prune_production_uses_handle_reachability() {
    let source = include_str!("../src/subset_prune.rs");
    let production = source
        .split_once("\n#[cfg(test)]\nmod tests")
        .expect("subset_prune.rs has a test module")
        .0;

    for legacy in [
        "resolve_borrowed",
        "resolve_object(",
        "Object::",
        "materialize(",
        "set_object(",
        "Dictionary",
    ] {
        assert!(
            !production.contains(legacy),
            "subset_prune production still contains the raw route marker {legacy:?}"
        );
    }
    for canonical in ["ObjectHandle", "walk_handle_contents", "pdf.resolve"] {
        assert!(
            production.contains(canonical),
            "subset_prune production must use the canonical handle marker {canonical:?}"
        );
    }
}

#[test]
fn outline_remap_production_uses_live_handles() {
    let source = include_str!("../src/outline_dest_remap.rs");
    let production = source
        .split_once("\n#[cfg(test)]\nmod tests")
        .expect("outline_dest_remap.rs has a test module")
        .0;

    for legacy in [
        "resolve_borrowed",
        "resolve_object(",
        "resolve_ref_chain",
        "Object::",
        "set_object(",
        "Dictionary",
        "materialize(",
    ] {
        assert!(
            !production.contains(legacy),
            "outline_dest_remap production still contains the raw route marker {legacy:?}"
        );
    }
    for canonical in [
        "ObjectHandle",
        "resolve_to_terminal",
        "try_get_key",
        "set_object_handle",
    ] {
        assert!(
            production.contains(canonical),
            "outline_dest_remap production must use the canonical handle marker {canonical:?}"
        );
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
fn resource_pruning_callbacks_use_only_the_handle_parser_route() {
    let resources = include_str!("../src/resources.rs");
    let resource_callbacks = resources
        .split_once("fn collect_used_names_for_form")
        .expect("resources has the Form pre-pass")
        .1
        .split_once("#[cfg(test)]")
        .expect("resources has a test module")
        .0;
    for legacy in [
        "parse_content_stream_data",
        "impl ParserCallbacks for ResourceCallbacks",
        "use crate::content_stream::{parse_content_stream_data",
        "Vec<Object>",
        "object: Object,",
        "Object::Operator",
        "Object::InlineImage",
        "struct ResourceCallbacks",
        "finish_inline_header",
        "is_builtin_inline_image_cs",
    ] {
        assert!(
            !resource_callbacks.contains(legacy),
            "resources Form callback still contains the raw parser marker {legacy:?}"
        );
    }
    for canonical in [
        "parse_content_stream_handles",
        "ResourceFinder::default()",
        "has_pending_operands",
    ] {
        assert!(
            resource_callbacks.contains(canonical),
            "resources Form callback must contain the handle parser marker {canonical:?}"
        );
    }

    let finder = include_str!("../src/resource_finder.rs");
    let finder_production = finder
        .split_once("#[cfg(test)]")
        .expect("resource_finder has a test module")
        .0;
    for legacy in [
        "handle_object_borrowed",
        "impl ParserCallbacks for ResourceFinder",
        "use crate::{Object, Result}",
        "last_operator_started_at_boundary",
        "record_resource_name",
    ] {
        assert!(
            !finder_production.contains(legacy),
            "ResourceFinder still contains the raw parser marker {legacy:?}"
        );
    }
    assert!(finder_production.contains("impl ObjectHandleParserCallbacks for ResourceFinder"));

    let replacer = include_str!("../src/resource_replacer.rs");
    let replacer_production = replacer
        .split_once("#[cfg(test)]")
        .expect("resource_replacer has a test module")
        .0;
    for legacy in [
        "parse_content_stream_data",
        "parse_content_stream_data_recovering_inline_image_eof",
    ] {
        assert!(
            !replacer_production.contains(legacy),
            "ResourceReplacer still contains the raw parser marker {legacy:?}"
        );
    }
    assert!(
        replacer_production.contains("parse_content_stream_handles"),
        "ResourceReplacer must use the handle parser"
    );

    let content_stream = include_str!("../src/content_stream.rs");
    assert!(
        !content_stream.contains("parse_content_stream_data_recovering_inline_image_eof"),
        "the raw recovering parser helper must not remain without a production caller"
    );
}

#[test]
fn acroform_active_resolution_uses_live_handle_route() {
    let source = include_str!("../src/acroform_document_helper.rs");
    for marker in ["fn acroform_dict", "fn resolve_dict"] {
        let section = source
            .split_once(marker)
            .expect("AcroForm resolver marker must remain present")
            .1
            .split_once("\n    fn ")
            .expect("AcroForm resolver must be followed by another helper")
            .0;
        for legacy in [
            "resolve_borrowed",
            "resolve_object",
            "Object::Reference",
            "Object::Dictionary",
            "dict.clone()",
        ] {
            assert!(
                !section.contains(legacy),
                "{marker} still contains raw resolution marker {legacy:?}"
            );
        }
        let canonical = if marker == "fn acroform_dict" {
            ["try_get_key", "resolve_to_terminal", "try_as_dictionary"]
        } else {
            ["get_object_handle", "resolve(", "try_as_dictionary"]
        };
        for canonical in canonical {
            assert!(
                section.contains(canonical),
                "{marker} must contain canonical handle marker {canonical:?}"
            );
        }
    }

    for field in ["value", "default_value", "default_appearance"] {
        let marker = format!("pub {field}: Option<ObjectHandle>");
        assert!(
            source.contains(&marker),
            "AcroFormFieldInfo::{field} must preserve live ObjectHandle values"
        );
    }

    let appearance = source
        .split_once("pub fn set_default_appearance")
        .expect("set_default_appearance must remain present")
        .1
        .split_once("\n    fn ")
        .expect("set_default_appearance must be followed by another helper")
        .0;
    for legacy in ["set_object(", "Object::Dictionary", "Object::String"] {
        assert!(
            !appearance.contains(legacy),
            "set_default_appearance still contains raw mutation marker {legacy:?}"
        );
    }
    for canonical in [
        "replace_key",
        "mark_object_handle_dirty",
        "ObjectHandle::string",
    ] {
        assert!(
            appearance.contains(canonical),
            "set_default_appearance must contain canonical marker {canonical:?}"
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
fn overlay_destination_page_rewrite_uses_live_handles() {
    let source = include_str!("../src/job/overlay.rs");
    let function = source
        .split_once("fn apply_overlays_to_page_with_sources")
        .and_then(|(_, rest)| rest.split_once("#[cfg(test)]").map(|(body, _)| body))
        .expect("overlay page application function must remain present");
    let rewrite = function
        .split_once("// 5. Rewrite only")
        .and_then(|(_, rest)| rest.split_once("    Ok(())"))
        .map(|(body, _)| body)
        .expect("overlay page rewrite boundary must remain present");
    let page_helper = source
        .split_once("fn overlay_page_handle")
        .and_then(|(_, rest)| rest.split_once("/// Allocate the next available object reference"))
        .map(|(body, _)| body)
        .expect("overlay page handle helper must remain present");

    for legacy in [
        "resolve_borrowed",
        "resolve_object(",
        "live_annots",
        "page_dictionary(",
    ] {
        assert!(
            !rewrite.contains(legacy),
            "overlay destination page rewrite still contains raw route marker {legacy:?}"
        );
    }
    for canonical in [
        "get_object_handle",
        "replace_key(",
        "mark_object_handle_dirty",
    ] {
        assert!(
            rewrite.contains(canonical),
            "overlay destination page rewrite must use canonical handle marker {canonical:?}"
        );
    }
    assert!(
        page_helper.contains("resolve(&"),
        "overlay page handle helper must resolve the canonical page handle"
    );
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
fn flatten_rotation_reads_boxes_through_handles() {
    let source = include_str!("../src/page_object_helper.rs");
    let production = source
        .split("#[cfg(test)]")
        .next()
        .expect("PageObjectHelper source should have a production section");
    let function = production
        .split("pub fn flatten_rotation")
        .nth(1)
        .and_then(|rest| {
            rest.split("    /// Copy annotations from another page")
                .next()
        })
        .expect("flatten_rotation production function should remain present");

    assert!(
        !function.contains("resolve_to_terminal"),
        "flatten_rotation must not use the non-qpdf terminal-resolution bridge"
    );
    assert!(
        function.contains("self.pdf.resolve(&media)"),
        "flatten_rotation must resolve /MediaBox through the canonical handle"
    );
    assert!(
        function.contains("self.pdf.resolve(&value)"),
        "flatten_rotation must resolve page boxes through canonical handles"
    );
}

#[test]
fn copy_annotations_reads_source_annots_through_handles() {
    let source = include_str!("../src/page_object_helper.rs");
    let production = source
        .split("#[cfg(test)]")
        .next()
        .expect("PageObjectHelper source should have a production section");
    let same_document = production
        .split("fn copy_annotations_with_reserved_names_impl")
        .nth(1)
        .and_then(|rest| rest.split("    /// This is qpdf's foreign-document").next())
        .expect("same-document annotation copy implementation should remain present");
    let foreign_document = production
        .split("fn copy_annotations_from_with_reserved_names_impl")
        .nth(1)
        .and_then(|rest| {
            rest.split("    /// Coalesce the page's content streams")
                .next()
        })
        .expect("foreign-document annotation copy implementation should remain present");

    for (route, function) in [
        ("same-document", same_document),
        ("foreign-document", foreign_document),
    ] {
        assert!(
            !function.contains("resolve_to_terminal"),
            "{route} annotation copy must not use the non-qpdf terminal-resolution bridge"
        );
        assert!(
            function.contains("try_get_key(b\"/Annots\")"),
            "{route} annotation copy must read /Annots from the source handle"
        );
    }
    assert!(
        same_document.contains("self.pdf.resolve(&old_annots)"),
        "same-document annotation copy must resolve /Annots through the destination Pdf"
    );
    assert!(
        foreign_document.contains("source.resolve(&old_annots)"),
        "foreign-document annotation copy must resolve /Annots through the source Pdf"
    );
}

#[test]
fn xobject_traversal_reads_resources_through_handles() {
    let source = include_str!("../src/page_object_helper.rs");
    let production = source
        .split("#[cfg(test)]")
        .next()
        .expect("PageObjectHelper source should have a production section");
    let function = production
        .split("fn for_each_xobject_filtered")
        .nth(1)
        .and_then(|rest| rest.split("    /// Visit image XObjects").next())
        .expect("XObject traversal production function should remain present");

    assert!(
        !function.contains("resolve_to_terminal"),
        "XObject traversal must not use the non-qpdf terminal-resolution bridge"
    );
    assert!(
        function.contains("self.pdf.resolve(&xobjects)"),
        "XObject traversal must resolve /XObject through the canonical handle"
    );
    assert!(
        function.contains("self.pdf.resolve(&object)"),
        "XObject traversal must resolve each entry through its canonical handle"
    );
}

#[test]
fn clear_page_tree_resolves_the_pages_root_through_a_handle() {
    let source = include_str!("../src/page_document_helper.rs");
    let production = source
        .split("#[cfg(test)]")
        .next()
        .expect("PageDocumentHelper source should have a production section");
    let function = production
        .split("fn clear_page_tree")
        .nth(1)
        .expect("clear_page_tree production function should remain present");

    assert!(
        !function.contains("resolve_to_terminal"),
        "clear_page_tree must not use the non-qpdf terminal-resolution bridge"
    );
    assert!(
        function.contains("self.pdf.resolve(&root)"),
        "clear_page_tree must resolve the catalog /Pages handle once"
    );
}

#[test]
fn page_content_bytes_uses_the_canonical_page_contents_route() {
    let source = include_str!("../src/pages.rs");
    let function = source
        .split("pub fn page_content_bytes")
        .nth(1)
        .and_then(|rest| rest.split("/// Resolve a `Page`'s `/Contents`").next())
        .expect("page_content_bytes production function should remain present");

    assert!(
        !function.contains("resolve_to_terminal_ref"),
        "page_content_bytes must not use the non-qpdf holder-chain bridge"
    );
    assert!(
        function.contains("page.get_page_contents()"),
        "page_content_bytes must use ObjectHandle::get_page_contents"
    );
}

#[test]
fn page_content_stream_entries_legacy_route_is_removed() {
    let source = include_str!("../src/pages.rs");
    assert!(
        !source.contains("pub fn page_content_stream_entries"),
        "page content entries must not retain the obsolete raw snapshot facade"
    );
    assert!(
        !source.contains("page_content_stream_entries_tolerant"),
        "page content entries must not retain the tolerant compatibility alias"
    );
}

#[test]
fn writer_page_content_prescan_does_not_chase_holder_chains() {
    let source = include_str!("../src/writer.rs");
    assert!(
        source.contains("pub(crate) fn collect_content_stream_refs"),
        "writer must retain one canonical page-content pre-scan"
    );
    for legacy in [
        "collect_content_stream_refs_tolerant",
        "collect_content_array_holder_refs",
        "resolve_to_terminal_ref",
    ] {
        assert!(
            !source.contains(legacy),
            "writer page-content pre-scan still contains the legacy route marker {legacy:?}"
        );
    }
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

#[test]
fn embedded_files_tests_do_not_keep_the_raw_projection_helpers() {
    let source = include_str!("../src/embedded_files.rs");
    for legacy in [
        "resolve_embedded_file_stream_ref",
        "collect_embedded_file_pairs_raw",
        "raw_object_from_handle",
    ] {
        assert!(
            !source.contains(legacy),
            "embedded_files.rs still contains the obsolete test-only raw helper {legacy:?}"
        );
    }
}

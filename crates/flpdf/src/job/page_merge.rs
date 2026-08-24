//! qpdf correspondence: QPDFJob.cc page-selection merge pipeline split across page-operation modules.
//! Multi-document page merge (qpdf `--pages` parity).
//!
//! [`merge_documents`] copies selected pages from N source documents into one
//! fresh target. `inputs[0]` is the primary: its Catalog/trailer state is the
//! document base, with page-tree, PageLabels, and AcroForm fields updated by
//! the merge-specific consumers; later inputs contribute pages and form fields
//! only. Shared resources within one input are de-duplicated; form-field name
//! collisions are resolved by qpdf's `<name>+<N>` renaming rule.
//!
//! `/PageLabels` is the one document-level structure that is **not**
//! primary-only: it is reconstructed from every input's own labels, one entry
//! per selected page in output order, matching qpdf's `handlePageSpecs`
//! (which calls `getLabelsForPageRange` for each selected page regardless of
//! which input file it came from). No input's named destinations or outline
//! items are copied at all beyond the primary — those structures are not
//! part of any page's object closure, and qpdf's own page-copy mechanism
//! (`addPage` / `copyForeignObject`) never reaches a source document's
//! catalog-level `/Names /Dests`, legacy `/Dests`, or `/Outlines`.
//! Consequently no named-destination "collision" between inputs is possible
//! here (there is nothing from a secondary input to collide with).
//!
//! The target document is built via [`Pdf::empty`] (qpdf's
//! `QPDF::emptyPDF()`). Its merge-specific union copy retains the primary
//! Catalog/trailer graph and the AcroForm handling needed by
//! `QPDFJob::handlePageSpecs`; each source is nevertheless prepared through
//! [`PageDocumentHelper::push_inherited_attributes_to_pages`] and each copied
//! leaf is reparented through a live destination handle, matching qpdf's
//! `insertPage` boundary. Neither the empty-document construction nor this
//! page preparation touches PDF version, so the returned document keeps
//! `emptyPDF()`'s own header version (`"1.3"`) rather than any input's version.
//! Propagating an input's version, as the qpdf CLI's `--pages` does, is
//! `QPDFJob` orchestration layered on top of these library primitives — a
//! distinct responsibility this function does not implement.

use super::acroform_field_prune::DEFAULT_MAX_ACROFORM_DEPTH;
use crate::page_extract::{append_selection_kids, null_copied_removed_pages, target_pages_root};
use crate::page_label_document_helper::{merge_adjacent_ranges, LabelRange};
use crate::pages::page_refs;
use crate::pdf_string::{new_unicode_string, utf8_value};
use crate::resources::{should_remove_unreferenced_resources, RemoveUnreferencedResources};
use crate::subset_prune::sweep_unreachable_objects_except;
use crate::{
    AcroFormDocumentHelper, Error, ObjectHandle, ObjectRef, PageDocumentHelper, PageObjectHelper,
    Pdf, Result,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Seek};

/// One merge input: an opened source document and the 0-based page indices to
/// take from it (arbitrary order, duplicates allowed).
pub struct MergeInput<'a, R: Read + Seek + 'static> {
    /// The opened source document.
    pub source: &'a mut Pdf<R>,
    /// 0-based page indices to copy, in output order.
    pub pages: Vec<usize>,
}

// Primary Catalog and trailer values are copied directly through the canonical
// ObjectHandle foreign copier below. qpdf keeps the primary document in place
// during `handlePageSpecs`; no legacy closure snapshot is needed here.

/// Copy every non-`/Pages` primary Catalog value through the destination's
/// persistent qpdf-shaped foreign copier. qpdf keeps the primary Catalog in
/// place while `handlePageSpecs` mutates its page-owned `/Pages` key
/// (`QPDFJob.cc:2462-2472`); the fresh target supplies that one key itself.
fn wire_primary_catalog<RS: Read + Seek, RT: Read + Seek>(
    source: &mut Pdf<RS>,
    target: &mut Pdf<RT>,
    source_id: u64,
) -> Result<()> {
    let Some(source_catalog_ref) = source.root_ref() else {
        return Ok(()); // cov:ignore: page selection already requires a readable catalog
    };
    let source_catalog_handle = source.get_object_handle(source_catalog_ref);
    let source_catalog = source.resolve_to_terminal(&source_catalog_handle)?;
    if source_catalog.try_as_dictionary()?.is_none() {
        return Ok(()); // cov:ignore: page selection already requires a dictionary catalog
    }
    let Some(target_catalog_ref) = target.root_ref() else {
        return Ok(()); // cov:ignore: Pdf::empty always supplies a target catalog
    };
    let target_catalog_handle = target.get_object_handle(target_catalog_ref);
    let target_catalog = target.resolve_to_terminal(&target_catalog_handle)?;
    if target_catalog.try_as_dictionary()?.is_none() {
        return Ok(()); // cov:ignore: Pdf::empty always supplies a dictionary catalog
    }

    for key in source_catalog.try_get_keys()? {
        if key == b"/Pages" {
            continue;
        }
        let value = source_catalog.try_get_key(&key)?;
        let copied = target.copy_foreign_value(source_id, &value)?;
        target_catalog.replace_key(&key, copied)?;
    }
    target.mark_object_handle_dirty(&target_catalog)?;
    Ok(())
}

/// Copy the primary trailer's non-writer-owned values through the same
/// persistent foreign copier. qpdf rebuilds `/Root`, `/Size`, encryption,
/// and xref-history keys at the writer boundary; `/Info`, `/ID`, and unknown
/// trailer entries remain primary-owned (`QPDFWriter.cc:2907-2913`).
fn wire_primary_trailer<RS: Read + Seek, RT: Read + Seek>(
    source: &mut Pdf<RS>,
    target: &mut Pdf<RT>,
    source_id: u64,
) -> Result<()> {
    let source_trailer = source.trailer();
    let target_trailer = target.trailer();
    // cov:ignore-start: this helper is called with Pdf::empty(), whose
    // construction contract always supplies a target catalog root.
    let Some(root_ref) = target.root_ref() else {
        return Ok(());
    };
    // cov:ignore-end
    for key in source_trailer.try_get_keys()? {
        if matches!(
            key.as_slice(),
            b"/Root"
                | b"/Size"
                | b"/Prev"
                | b"/Encrypt"
                | b"/Type"
                | b"/W"
                | b"/Index"
                | b"/XRefStm"
                | b"/Length"
                | b"/Filter"
                | b"/DecodeParms"
                | b"/F"
                | b"/FFilter"
                | b"/FDecodeParms"
        ) {
            continue;
        }
        let value = source_trailer.try_get_key(&key)?;
        let copied = target.copy_foreign_value(source_id, &value)?;
        target_trailer.replace_key(&key, copied)?;
    }
    target_trailer.replace_key(b"/Root", target.get_object_handle(root_ref))?;
    Ok(())
}

/// Resolve qpdf's `--pages` form-field name collision: return `base` when it is
/// not yet present in `used`, otherwise the first unused `base+1`, `base+2`, …
///
/// This reproduces qpdf 11.9.0's observed renaming: `name`+`name` →
/// `name`, `name+1`; a three-way collision → `name`, `name+1`, `name+2`; and a
/// candidate that itself collides is re-resolved (a field originally named
/// `name+1` whose `name+1` is already taken becomes `name+1+1`).
pub(crate) fn unique_field_name(base: &[u8], used: &BTreeSet<Vec<u8>>) -> Vec<u8> {
    if !used.contains(base) {
        return base.to_vec();
    }
    for n in 1u32.. {
        let mut candidate = base.to_vec();
        candidate.extend_from_slice(format!("+{n}").as_bytes());
        if !used.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("u32 candidate space exhausted") // cov:ignore: 2^32 colliding names is unreachable
}

/// A top-level AcroForm field copied into the target, paired with the partial
/// name (`/T`) read from its source. `partial_name` is `None` for a field with
/// no direct `/T` (such a field is appended without a name-collision check).
struct KeptField {
    /// The field's object ref in the merged target document.
    target_ref: ObjectRef,
    /// The field's `/T` partial name as read from the source (resolved).
    partial_name: Option<Vec<u8>>,
    /// Whether the field came from the primary input (`inputs[0]`).
    is_primary: bool,
}

/// The primary input's inherited `/AcroForm` defaults, captured before copying
/// and remapped onto the merged output's `/AcroForm`.
#[derive(Default)]
struct PrimaryAcroForm {
    /// Whether the primary has a `/DR` default-resource entry. The Catalog
    /// copier carries the value itself; this flag preserves qpdf's decision to
    /// retain an otherwise field-less `/AcroForm`.
    has_dr: bool,
    /// Whether the primary has a `/DA` default-appearance entry.
    has_da: bool,
    /// Whether the primary has an `/AcroForm` dictionary whose `/Fields`
    /// resolves to an array (qpdf's `hasAcroForm() && fields.isArray()`
    /// gate, `QPDFJob.cc:2609-2610`). Drives [`build_merged_acroform`]'s
    /// decision to rebuild or remove `/AcroForm`, independent of `/DR`/`/DA`.
    had_fields_array: bool,
    /// Original names of every primary top-level field, including fields on
    /// pages that are not selected. qpdf keeps those fields in its collision
    /// index until the final page-pruning pass (`QPDFJob.cc:2516-2521,
    /// 2600-2629`), so later inputs must not reuse their names or suffixes.
    original_field_names: BTreeSet<Vec<u8>>,
}

/// Read the primary input's AcroForm shape through canonical handles. The
/// Catalog copy below owns the actual `/DR` and `/DA` graph transfer; this
/// snapshot only carries the qpdf field-rebuild gates.
fn discover_primary_acroform<R: Read + Seek>(source: &mut Pdf<R>) -> Result<PrimaryAcroForm> {
    let mut out = PrimaryAcroForm {
        had_fields_array: source.acroform()?.has_fields_array()?,
        ..Default::default()
    };
    let Some(root_ref) = source.root_ref() else {
        return Ok(out); // cov:ignore: page selection already requires a readable catalog
    };
    let root_handle = source.get_object_handle(root_ref);
    let root = source.resolve_to_terminal(&root_handle)?;
    if root.try_as_dictionary()?.is_none() {
        return Ok(out); // cov:ignore: page selection already requires a dictionary catalog
    }
    let acroform = source.resolve_to_terminal(&root.try_get_key(b"/AcroForm")?)?;
    if acroform.try_as_dictionary()?.is_some() {
        out.has_dr = !acroform.try_get_key(b"/DR")?.is_null();
        out.has_da = !acroform.try_get_key(b"/DA")?.is_null();
    }
    Ok(out)
}

/// Read the partial names (`/T`, resolved) of `source`'s top-level AcroForm
/// fields, in `/Fields` order, paired with the source field ref so a caller can
/// map the ref through that input's copy map. A field whose `/T` is absent
/// yields `None`.
pub(crate) fn source_top_level_field_names<R: Read + Seek>(
    source: &mut Pdf<R>,
) -> Result<Vec<(ObjectRef, Option<Vec<u8>>)>> {
    let top_fields = source.acroform()?.top_level_fields()?;
    let mut out = Vec::with_capacity(top_fields.len());
    for field_ref in top_fields {
        // A top-level `/Fields` element may be a holder chain (a ref to a ref to
        // the field dict). The copy map keys the field by its TERMINAL ref, so
        // normalize to the terminal before recording it — otherwise `map.get` on
        // the holder ref misses and the field is dropped from the merged form. The
        // terminal also feeds the `/T` lookup so a name behind a holder is read.
        let field_handle = source.get_object_handle(field_ref);
        let terminal = source
            .resolve_to_terminal_ref(&field_handle)?
            .1
            .unwrap_or(field_ref);
        let name = resolve_field_partial_name(source, terminal)?;
        out.push((terminal, name));
    }
    Ok(out)
}

/// Resolve a field's `/T` partial name, decoded to qpdf's UTF-8 view. `/T`
/// may be an indirect reference (review rule 2); a resolved non-string or
/// absent `/T` yields `None`.
///
/// qpdf's collision index compares `getFullyQualifiedName()`, itself built
/// from `getUTF8Value()`-decoded `/T` segments
/// (`QPDFFormFieldObjectHelper::getFullyQualifiedName`,
/// `QPDFAcroFormDocumentHelper.cc:82-103`) — never the raw stored bytes. A
/// `/T` stored as UTF-16BE (or PDFDocEncoded with non-ASCII bytes) must
/// therefore be decoded before it is stored or compared here, or an ASCII
/// collision candidate could byte-mismatch a semantically identical reserved
/// name and collide with it in the output.
fn resolve_field_partial_name<R: Read + Seek>(
    source: &mut Pdf<R>,
    field_ref: ObjectRef,
) -> Result<Option<Vec<u8>>> {
    let field_handle = source.get_object_handle(field_ref);
    let field = source.resolve_to_terminal(&field_handle)?;
    if field.try_as_dictionary()?.is_none() {
        return Ok(None); // cov:ignore: a top-level field ref always resolves to a dictionary
    }
    let t_value = field.try_get_key(b"/T")?;
    if t_value.is_null() {
        return Ok(None);
    }
    // `/T` may be stored through more than one indirect hop; follow the whole
    // chain (not a one-hop resolve) so a multi-hop name string is read and
    // used for collision renaming rather than yielding `None`.
    let resolved = source.resolve_to_terminal(&t_value)?;
    Ok(resolved.as_string().map(|raw| utf8_value(&raw)))
}

/// Remove `/AcroForm` from the target's catalog, if present.
///
/// The canonical primary Catalog copier places the primary's `/AcroForm`
/// on the target verbatim (unpruned) before [`build_merged_acroform`] runs.
/// qpdf removes `/AcroForm` entirely once the filtered field count reaches 0
/// (`QPDFJob.cc:2626-2629`, `pdf.getRoot().removeKey("/AcroForm")`); this
/// mirrors that removal for the case where nothing else rebuilds the key.
fn remove_target_acroform<R: Read + Seek>(target: &mut Pdf<R>) -> Result<()> {
    let Some(catalog_ref) = target.root_ref() else {
        return Ok(()); // cov:ignore: the seed target always has a /Root catalog
    };
    let catalog_handle = target.get_object_handle(catalog_ref);
    let catalog = target.resolve_to_terminal(&catalog_handle)?;
    if catalog.try_as_dictionary()?.is_none() {
        return Ok(()); // cov:ignore: the seed catalog is always a dict
    }
    catalog.remove_key(b"/AcroForm");
    target.mark_object_handle_dirty(&catalog)?;
    Ok(())
}

/// Build the merged output's `/AcroForm` from the primary's inherited `/DR` /
/// `/DA` base plus every kept top-level field, applying qpdf's `+N` name
/// collision renaming to fields from later inputs.
///
/// Primary fields keep their names verbatim (they seed the `used` set); a later
/// input's field name is resolved through [`unique_field_name`] and written
/// back onto the copied field as a direct `/T` string. No `/AcroForm` is created
/// when there are no kept fields and the primary carried no `/DR` / `/DA`, so a
/// form-free merge gains no empty `/AcroForm`.
fn build_merged_acroform<R: Read + Seek>(
    target: &mut Pdf<R>,
    primary: &PrimaryAcroForm,
    kept: &[KeptField],
) -> Result<()> {
    if kept.is_empty() && !primary.has_dr && !primary.has_da {
        if primary.had_fields_array {
            // The primary had a /Fields array (so the generic catalog copy
            // placed a real, now-stale /AcroForm on the target) but every
            // field's page was dropped from the selection. Remove it to
            // match qpdf instead of leaving the unpruned copy in place.
            remove_target_acroform(target)?;
        }
        return Ok(());
    }

    // The target is still a transient grouped copy here: repeated-page
    // annotations have not yet gone through the qpdf `fixCopiedAnnotations`
    // replay. Construct only the field-tree cache so that those pending
    // annotations are not reported as source-document orphans.
    let acroform =
        AcroFormDocumentHelper::new_for_field_tree(target)?.canonical_get_or_create_acroform()?;
    if acroform.try_as_dictionary()?.is_none() {
        return Ok(()); // cov:ignore: ensure_acroform_ref always yields a dictionary
    }

    // Seed `used` with the primary's field names (verbatim — the primary is the
    // base document and is never renamed), then append every kept field, in
    // order, renaming later inputs' colliding names via the qpdf `+N` rule.
    let mut used = primary.original_field_names.clone();
    for field in kept {
        if field.is_primary {
            if let Some(name) = &field.partial_name {
                used.insert(name.clone());
            }
        }
    }

    let mut fields = Vec::with_capacity(kept.len());
    for field in kept {
        if !field.is_primary {
            if let Some(name) = &field.partial_name {
                let unique = unique_field_name(name, &used);
                used.insert(unique.clone());
                // qpdf only rewrites `/T` when an actual collision produced a
                // suffix (`if (!append.empty())`,
                // `QPDFAcroFormDocumentHelper.cc:97-102`); a field whose name
                // was already unique keeps its original `/T` bytes/encoding
                // untouched.
                if unique != *name {
                    rename_field(target, field.target_ref, unique)?;
                }
            }
        }
        fields.push(target.get_object_handle(field.target_ref));
    }
    acroform.replace_key(b"/Fields", ObjectHandle::array(fields))?;

    target.mark_object_handle_dirty(&acroform)?;
    Ok(())
}

/// Overwrite the copied field's `/T` with `name` (qpdf's UTF-8-decoded,
/// collision-suffixed name), re-encoded through
/// [`new_unicode_string`] to match qpdf's own rename write
/// (`QPDFObjectHandle::newUnicodeString`, `QPDFAcroFormDocumentHelper.cc:99-102`).
fn rename_field<R: Read + Seek>(
    target: &mut Pdf<R>,
    field_ref: ObjectRef,
    name: Vec<u8>,
) -> Result<()> {
    let field_handle = target.get_object_handle(field_ref);
    let field = target.resolve_to_terminal(&field_handle)?;
    if field.try_as_dictionary()?.is_none() {
        return Ok(()); // cov:ignore: a copied field ref always resolves to a dictionary
    }
    field.replace_key(b"/T", ObjectHandle::string(new_unicode_string(&name)))?;
    target.mark_object_handle_dirty(&field)?;
    Ok(())
}

/// Read a field's direct `/Kids` as the source-space refs they point at, or
/// `None` when the field has no `/Kids` (a terminal field — the widget IS the
/// field). `/Kids` itself may be an indirect reference (review rule 2); a
/// resolved non-array yields an empty list (treated as "no widget kids").
fn field_kid_refs<R: Read + Seek>(
    source: &mut Pdf<R>,
    field_ref: ObjectRef,
) -> Result<Option<Vec<ObjectRef>>> {
    let field_handle = source.get_object_handle(field_ref);
    let field = source.resolve_to_terminal(&field_handle)?;
    if field.try_as_dictionary()?.is_none() {
        return Ok(None); // cov:ignore: a field ref always resolves to a dictionary
    }
    let kids_value = field.try_get_key(b"/Kids")?;
    if kids_value.is_null() {
        return Ok(None);
    }
    let resolved = source.resolve_to_terminal(&kids_value)?;
    let Some(items) = resolved.try_as_array()? else {
        return Ok(Some(Vec::new())); // cov:ignore: a /Kids value resolves to an array in practice
    };
    let mut refs = Vec::with_capacity(items.len());
    for item in items {
        let (_, terminal_ref) = source.resolve_to_terminal_ref(&item)?;
        if let Some(r) = terminal_ref {
            // A `/Kids` element may be a reference to a reference to the field/
            // widget; resolve the holder chain to the terminal ref so trimming
            // compares the same ref that retained-`/Annots` membership records.
            refs.push(r);
        } // cov:ignore: llvm-cov gap-region artifact on the brace closing a `?`-bearing block; the body (the `refs.push`) is covered
    }
    Ok(Some(refs))
}

/// Collect the widget annotation refs that appear directly in the selected
/// pages' `/Annots` arrays (the "retained widget refs"). A widget that is a
/// member of a selected page's `/Annots` is on a surviving page and must be kept
/// by [`trim_field_kids`], whether or not it carries the optional `/P`
/// back-pointer (`/P` is not required by ISO 32000-2 §12.5.2 — it is a
/// convenience pointer, so it cannot be the survival signal).
///
/// `/Annots` may be an inline array or an indirect reference to one; each element
/// is an indirect reference to (or an inline) annotation dict. Only the direct
/// annotation refs are recorded — that is what extract uses to decide a widget
/// survives. Reference-holder traversal is bounded by the canonical
/// `resolve_to_terminal_ref` primitive.
fn collect_retained_widget_refs<R: Read + Seek>(
    source: &mut Pdf<R>,
    selected_pages: &BTreeSet<ObjectRef>,
    retained: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    for &page_ref in selected_pages {
        let page_handle = source.get_object_handle(page_ref);
        let page = source.resolve_to_terminal(&page_handle)?;
        if page.try_as_dictionary()?.is_none() {
            continue; // cov:ignore: a selected page ref always resolves to a dictionary
        }
        let annots_val = page.try_get_key(b"/Annots")?;
        if annots_val.is_null() {
            continue;
        }
        // /Annots: an inline array or an indirect reference to one.
        let concrete = source.resolve_to_terminal(&annots_val)?;
        let Some(elems) = concrete.try_as_array()? else {
            continue; // cov:ignore: a non-array /Annots is malformed
        };
        for elem in elems {
            let (_, terminal_ref) = source.resolve_to_terminal_ref(&elem)?;
            if let Some(r) = terminal_ref {
                // An `/Annots` element may be a reference to a reference to the
                // widget; resolve the holder chain to the terminal widget ref so it
                // matches the field-tree kid ref recorded by `field_kid_refs`.
                retained.insert(r);
            }
        }
    }
    Ok(())
}

/// Resolve a widget's `/P` page reference (review rule 2: `/P` may be indirect),
/// returning the final page `ObjectRef` of the reference chain, or `None` when
/// the widget carries no `/P`.
fn widget_page_ref<R: Read + Seek>(
    source: &mut Pdf<R>,
    widget_ref: ObjectRef,
) -> Result<Option<ObjectRef>> {
    let widget_handle = source.get_object_handle(widget_ref);
    let widget = source.resolve_to_terminal(&widget_handle)?;
    if widget.try_as_dictionary()?.is_none() {
        return Ok(None); // cov:ignore: a widget ref always resolves to a dictionary
    }
    let p_value = widget.try_get_key(b"/P")?;
    if p_value.is_null() {
        // `/P` is optional (ISO 32000-2 §12.5.2): a widget may omit it. Such
        // a widget's survival is decided by retained-`/Annots` membership in
        // trim_field_kids, not by this back-pointer.
        return Ok(None);
    }
    let (_, last_ref) = source.resolve_to_terminal_ref(&p_value)?;
    Ok(last_ref)
}

/// Trim a non-terminal AcroForm field's widget `/Kids` to only the widgets whose
/// `/P` page survived into the output (is in `surviving_pages`), recursing into
/// intermediate sub-fields (fields that themselves carry `/Kids`). Returns:
///
/// - `None` — the field is terminal (no `/Kids`, the widget IS the field); the
///   caller leaves it untouched. This is what protects flat-form fields and the
///   `+N` rename tests, whose widgets carry no `/Kids`.
/// - `Some(survivors)` — the trimmed list of direct kid source-refs to keep. A
///   leaf-widget kid is kept iff it is a `retained_widgets` member (it appears in
///   a selected page's `/Annots`) OR its `/P` resolves to a page in
///   `surviving_pages`; an intermediate sub-field kid is kept iff it has at least
///   one surviving descendant (recursion). An empty `survivors` means no widget
///   survived, so the field should be dropped (top level) or pruned from its
///   parent's `/Kids` (nested).
///
/// The retained-`/Annots` membership is the primary survival signal because a
/// widget's `/P` page back-pointer is optional (ISO 32000-2 §12.5.2); a
/// selected-page widget that omits `/P` must still be kept. The `/P` path is a
/// fallback for a widget reachable through the field tree but not directly listed
/// in a scanned `/Annots`.
///
/// Side effects: rewrites each kept intermediate sub-field's `/Kids` in `target`
/// (mapped through `map`), and records each dropped widget's unselected `/P`
/// page (source-space) into `orphan_pages` so the caller can null it (a dropped
/// widget that also omits `/P` carries no page to null).
///
/// Bounded by `DEFAULT_MAX_ACROFORM_DEPTH` and a `visited` cycle guard (review
/// rule 4): a hostile field tree cannot drive unbounded recursion.
#[allow(clippy::too_many_arguments)]
fn trim_field_kids<R: Read + Seek>(
    source: &mut Pdf<R>,
    target: &mut Pdf<Cursor<Vec<u8>>>,
    field_ref: ObjectRef,
    surviving_pages: &BTreeSet<ObjectRef>,
    retained_widgets: &BTreeSet<ObjectRef>,
    map: &BTreeMap<ObjectRef, ObjectRef>,
    orphan_pages: &mut BTreeSet<ObjectRef>,
    depth: usize,
    visited: &mut BTreeSet<ObjectRef>,
) -> Result<Option<Vec<ObjectRef>>> {
    // cov:ignore-start: depth guard against a hostile >100-deep field tree (matches the acroform helper's depth caps); not driven by well-formed input
    if depth > DEFAULT_MAX_ACROFORM_DEPTH {
        return Err(Error::Unsupported(format!(
            "AcroForm field tree depth exceeds maximum of {DEFAULT_MAX_ACROFORM_DEPTH}"
        )));
    }
    // cov:ignore-end
    if !visited.insert(field_ref) {
        return Ok(Some(Vec::new())); // cov:ignore: a /Kids cycle is malformed; treat as no survivors
    }
    let Some(kids) = field_kid_refs(source, field_ref)? else {
        return Ok(None); // terminal field — nothing to trim
    };

    let mut survivors: Vec<ObjectRef> = Vec::with_capacity(kids.len());
    for kid_ref in kids {
        let kid_kind = trim_field_kids(
            source,
            target,
            kid_ref,
            surviving_pages,
            retained_widgets,
            map,
            orphan_pages,
            depth + 1,
            visited,
        )?; // cov:ignore: `?` Err arm — trim_field_kids errors only on the depth guard, unreachable on well-formed input
        match kid_kind {
            // The kid is itself a non-terminal sub-field.
            Some(sub_survivors) => {
                if sub_survivors.is_empty() {
                    // Whole sub-field is off-tree — prune it from this field's
                    // `/Kids`. Its widgets' orphan pages were recorded by the
                    // recursive call.
                    continue;
                }
                rewrite_field_kids(target, kid_ref, &sub_survivors, map)?;
                survivors.push(kid_ref);
            }
            // The kid is a leaf widget (no `/Kids`). It survives iff it is a
            // retained widget (a member of a selected page's `/Annots`) — the
            // signal extract uses — OR its optional `/P` resolves to a surviving
            // page (a fallback for a widget reached through the field tree but not
            // directly in a scanned `/Annots`). A non-surviving widget's `/P`
            // page, if any, is an off-tree orphan to null; a `/P`-less dropped
            // widget carries no page to null.
            None => {
                if retained_widgets.contains(&kid_ref) {
                    survivors.push(kid_ref);
                } else {
                    match widget_page_ref(source, kid_ref)? {
                        Some(page_ref) if surviving_pages.contains(&page_ref) => {
                            survivors.push(kid_ref)
                        }
                        Some(page_ref) => {
                            orphan_pages.insert(page_ref);
                        }
                        None => {} // cov:ignore: a dropped widget that also omits /P carries no page to null
                    }
                }
            }
        }
    }
    Ok(Some(survivors))
}

/// Overwrite the copied field's `/Kids` with the surviving source kid-refs
/// mapped through this input's copy map. A survivor missing from `map` is
/// skipped (it was not copied); the rewrite never inserts a dangling ref.
fn rewrite_field_kids<R: Read + Seek>(
    target: &mut Pdf<R>,
    src_field_ref: ObjectRef,
    survivors: &[ObjectRef],
    map: &BTreeMap<ObjectRef, ObjectRef>,
) -> Result<()> {
    let Some(target_field_ref) = map.get(&src_field_ref).copied() else {
        return Ok(()); // cov:ignore: a survivor's parent field is always in the copy map
    };
    let field_handle = target.get_object_handle(target_field_ref);
    let field = target.resolve_to_terminal(&field_handle)?;
    if field.try_as_dictionary()?.is_none() {
        return Ok(()); // cov:ignore: a copied field ref always resolves to a dictionary
    }
    let mut kids = Vec::with_capacity(survivors.len());
    for src in survivors {
        if let Some(&target_ref) = map.get(src) {
            kids.push(target.get_object_handle(target_ref));
        }
    }
    field.replace_key(b"/Kids", ObjectHandle::array(kids))?;
    target.mark_object_handle_dirty(&field)?;
    Ok(())
}

/// Merge selected pages from N sources into one fresh document.
///
/// Each [`MergeInput`] pairs an opened source document with the page indices to
/// take from it. Returns an owned in-memory [`Pdf`] whose catalog has a
/// single-level `/Pages` tree containing the selected pages from every input,
/// concatenated
/// in input order and, within each input, in the order given by that input's
/// `pages`. Each input is copied in a single pass with one renumbering map, so
/// objects shared between selected pages of the same input (fonts, images,
/// content streams) appear once per input in the output.
///
/// Inherited page attributes (`/Resources`, `/MediaBox`, `/CropBox`,
/// `/Rotate`) are materialized onto each copied page from its source page
/// tree, and a page selected more than once within an input becomes a shallow
/// clone of its first copy, matching [`extract_pages`](crate::extract_pages).
///
/// Each source is left unmodified. Each selected page is copied with the
/// persistent [`Pdf::copy_foreign_object`] route; the result mirrors
/// [`extract_pages`](crate::extract_pages) for a single input. Write the result
/// with [`crate::PdfWriter`] to produce one fresh qpdf-style output.
///
/// An input may select **no pages** (`pages: vec![]`): it contributes nothing
/// and is not an error. A blank document passed as `inputs[0]` with an empty
/// selection is the qpdf `--empty` analog — the merge then starts from an empty
/// base and inherits no document-level information (a blank primary has none).
///
/// The primary input (`inputs[0]`) remains the document base: its Catalog and
/// trailer metadata (including `/Info`, `/ID`, unknown trailer entries, and
/// unrelated Catalog keys) are copied with indirect references remapped, while
/// the merged page tree and writer-owned trailer structure are rebuilt in the
/// target. Its `/Outlines` tree, `/Names /Dests` named destinations (and the
/// legacy `/Catalog /Dests` dictionary), and `/OpenAction` therefore remain
/// primary-only as part of that Catalog graph. Later inputs contribute pages
/// and selected form fields only — their Catalog/trailer metadata, outlines,
/// and named destinations are not merged. A direct (inline) `/Names /Dests`
/// name-tree root is inherited
/// in either ISO 32000-2 §7.9.6 shape: a `/Names` leaf has its destinations
/// remapped, and a `/Kids` root has its sub-leaves copied and its `/Kids`
/// references remapped to those copies, so the named destinations survive in
/// both forms.
///
/// A destination (annotation `/Dest`, an `/A` or `/AA` `/GoTo` action, including
/// `/Next` continuations and `/GoTo /SD` structure destinations, plus the
/// primary's inherited outline / named / `/OpenAction` destinations) that points
/// at a page not selected from its input keeps its reference, which resolves to
/// a `null` page object in the output.
///
/// A page reached only through a back-pointer from an unselected page — a thread
/// bead's `/P`, a structure element's `/Pg`, or (on malformed input) an
/// annotation's `/P` that names an unselected page rather than the page it sits
/// on — is not yet pruned: it stays out of the output page tree (`/Pages`
/// `/Kids`) but remains a live object in the output, reachable through that
/// surviving back-pointer.
///
/// Interactive form (AcroForm) fields are merged: the primary's `/AcroForm`
/// `/DR` default resources and `/DA` default appearance are the base, and every
/// selected page's top-level field (reached from its widget annotations) is
/// added to the output `/AcroForm /Fields`. A field whose widget is on an
/// unselected page is dropped (qpdf form subset). A non-terminal field whose
/// widget `/Kids` span several pages keeps only the widgets whose page is
/// selected — its `/Kids` are trimmed to those, and the field is dropped
/// entirely only if no widget survives. Top-level field-name (`/T`)
/// collisions are resolved by qpdf's `<name>+<N>` rule: the primary keeps its
/// names and a later input's colliding name becomes the first unused
/// `<name>+1`, `<name>+2`, … . Collision handling is limited to **top-level
/// partial names** (flat forms where the partial name equals the fully-qualified
/// name); nested field-tree fully-qualified-path collisions, and merging later
/// inputs' `/DR` resources, are not handled. A merge of form-free inputs adds no
/// `/AcroForm`.
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{merge_documents, MergeInput, Pdf, PdfWriter};
///
/// let mut a = Pdf::open(BufReader::new(File::open("a.pdf")?))?;
/// let mut b = Pdf::open(BufReader::new(File::open("b.pdf")?))?;
/// let mut inputs = [
///     MergeInput { source: &mut a, pages: vec![0, 1] }, // a's first two pages
///     MergeInput { source: &mut b, pages: vec![0] },    // then b's first page
/// ];
/// let mut merged = merge_documents(&mut inputs)?;
///
/// let mut writer = PdfWriter::new(&mut merged);
/// writer.set_output_file("merged.pdf")?;
/// writer.write()?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// - [`Error::Unsupported`] if the `inputs` slice is empty (an *input* with an
///   empty page selection is permitted; see above), or if a requested page
///   index is out of range for its input.
/// - Propagates resolve/copy errors from the underlying primitives.
pub fn merge_documents<R: Read + Seek>(
    inputs: &mut [MergeInput<'_, R>],
) -> Result<Pdf<Cursor<Vec<u8>>>> {
    merge_documents_with_resource_mode(inputs, RemoveUnreferencedResources::No)
}

/// Merge selected pages while applying qpdf's page-job resource mode to the
/// first copy of each selected source page.
///
/// The public [`merge_documents`] library primitive intentionally retains its
/// historical library-level behaviour: resource pruning belongs to
/// `QPDFJob::handlePageSpecs`, not to the generic page insertion primitive.
/// The job route calls this bounded variant so that qpdf's
/// `--remove-unreferenced-resources={auto,yes,no}` decision is made per source
/// before a page enters the canonical foreign-object copier.
pub(crate) fn merge_documents_with_resource_mode<R: Read + Seek>(
    inputs: &mut [MergeInput<'_, R>],
    resource_mode: RemoveUnreferencedResources,
) -> Result<Pdf<Cursor<Vec<u8>>>> {
    merge_documents_with_resource_mode_and_preserve_primary(inputs, resource_mode, false)
}

/// Merge selected pages while optionally retaining primary-source objects that
/// are not reachable from its trailer roots.
///
/// qpdf keeps the primary QPDF in place while `QPDFJob::handlePageSpecs`
/// copies foreign pages (`libqpdf/QPDFJob.cc:2360-2632`). When the writer's
/// `preserve_unreferenced_objects` option is enabled, those primary objects
/// remain available to `QPDFWriter::enqueueObjectsStandard` (`QPDFWriter.cc:
/// 2907-2913`). The fresh merge target normally has no way to see them, so the
/// job-level page-spec route adds the primary's unreachable live references to
/// the same persistent foreign-object map as its selected pages.
pub(crate) fn merge_documents_with_resource_mode_and_preserve_primary<R: Read + Seek>(
    inputs: &mut [MergeInput<'_, R>],
    resource_mode: RemoveUnreferencedResources,
    preserve_primary_unreferenced: bool,
) -> Result<Pdf<Cursor<Vec<u8>>>> {
    if inputs.is_empty() {
        return Err(Error::Unsupported(
            "merge requires at least one input".to_string(),
        ));
    }

    // qpdf computes the Auto decision once per source QPDF, before any page
    // is copied or the source page tree is flattened. Keep the result by input
    // index so duplicate page specifications share the same source decision.
    let remove_resources: Vec<bool> = inputs
        .iter_mut()
        .map(|input| match resource_mode {
            RemoveUnreferencedResources::No => Ok(false),
            RemoveUnreferencedResources::Yes => Ok(true),
            RemoveUnreferencedResources::Auto => should_remove_unreferenced_resources(input.source),
        })
        .collect::<Result<_>>()?;

    let mut target = Pdf::empty()?;
    let pages_root_ref = target_pages_root(&mut target)?;

    // Output `/Kids`, accumulated across inputs in input/selection order.
    let mut kids: Vec<ObjectRef> = Vec::new();
    // Copied page objects already placed in `kids`, so a page selected more
    // than once becomes a shallow clone rather than a duplicated reference.
    let mut used: BTreeSet<ObjectRef> = BTreeSet::new();

    // AcroForm merge state. `kept_fields` accumulates each input's kept
    // top-level fields (orphan fields on unselected pages are absent from the
    // selected-page map and so never appear). `primary_acroform` holds only the
    // primary's qpdf field-rebuild gates; the Catalog copier carries `/DR` and
    // `/DA` through the canonical foreign map.
    let mut kept_fields: Vec<KeptField> = Vec::new();
    let mut primary_acroform = PrimaryAcroForm::default();
    // Target-side refs of the primary's preserved orphan objects
    // (`--preserve-unreferenced`), protected from the final sweep below.
    // Empty, and therefore a no-op, when preservation is disabled.
    let mut preserved_target_refs: BTreeSet<ObjectRef> = BTreeSet::new();

    // /PageLabels merge state (qpdf `handlePageSpecs` parity). Unlike outlines
    // and named destinations, which are inherited from the primary input
    // only, page labels accumulate across EVERY input: qpdf calls
    // `getLabelsForPageRange` once per selected page regardless of which
    // source file it came from, gating only the final `/PageLabels` install
    // on whether ANY input's own tree carried real labels.
    // One entry per selected page across all inputs, upper-bounded by the
    // total selection count; pre-allocate to avoid repeated regrowth on
    // large merges.
    let total_selected: usize = inputs.iter().map(|i| i.pages.len()).sum();
    let mut label_entries: Vec<(i64, LabelRange)> = Vec::with_capacity(total_selected);
    let mut any_page_labels = false;
    let mut out_pageno: i64 = 0;

    for (input_index, input) in inputs.iter_mut().enumerate() {
        // Document-level structures (outlines, named dests, /OpenAction) are
        // inherited from the PRIMARY input only (qpdf `--pages` parity).
        let is_primary = input_index == 0;

        // A non-primary input that selects no pages contributes nothing (it adds
        // no pages and, with no selected pages, no fields). Skip it before any
        // source read so a malformed but unused secondary (e.g. a broken
        // `/AcroForm /Fields` or `/Pages` tree) cannot abort the whole merge.
        // The primary is always processed: it carries the inherited document-level
        // state even when it contributes no pages of its own.
        if !is_primary && input.pages.is_empty() {
            continue;
        }

        // qpdf's `QPDF::insertPage` prepares every foreign source through
        // `pushInheritedAttributesToPage` before `copyForeignObject`. This
        // source-side mutation promotes shared non-scalar inherited values
        // (such as a direct `/MediaBox` on `/Pages`) once, and only writes a
        // leaf key when an ancestor actually supplies it. In particular, an
        // absent `/Rotate` must stay absent rather than becoming `/Rotate 0`.
        PageDocumentHelper::new(input.source).push_inherited_attributes_to_pages()?;

        // Reconstruct this input's page-label contribution, one entry per
        // selected page (in selection order, duplicates included), before any
        // other per-input processing — purely a `/PageLabels`-tree read,
        // independent of the page-copy/dest/AcroForm machinery below.
        // An input that contributes no pages contributes no labels either —
        // otherwise a primary carrying /PageLabels but pages: vec![] would
        // set `any_page_labels` (and thereby install fabricated labels for
        // every later input's pages) even though none of its own pages are
        // in the output.
        if !input.pages.is_empty() {
            let mut src_labels = input.source.page_labels();
            if src_labels.has_page_labels()? {
                any_page_labels = true;
            }
            let src_indices: Vec<i64> = input.pages.iter().map(|&i| i as i64).collect();
            label_entries.extend(src_labels.labels_for_selection(&src_indices, out_pageno)?);
            out_pageno = out_pageno.saturating_add(input.pages.len() as i64);
        }

        // The primary's `/AcroForm /DR` and `/DA` are the merged form's base; a
        // later input contributes form fields only (its `/DR` / `/DA` are not
        // merged). Read the primary's field-rebuild gates before copying.
        if is_primary {
            primary_acroform = discover_primary_acroform(input.source)?;
        }
        // Source top-level field names, read before the copy severs numbering;
        // each is mapped through this input's copy map below.
        let source_fields = source_top_level_field_names(input.source)?;
        if is_primary {
            primary_acroform.original_field_names.extend(
                source_fields
                    .iter()
                    .filter_map(|(_, name)| name.as_ref().cloned()),
            );
        }

        let all = page_refs(input.source)?;
        // Resolve the selected source page refs (range-checked, duplicates
        // allowed), in selection order.
        let mut selected: Vec<ObjectRef> = Vec::with_capacity(input.pages.len());
        for &idx in &input.pages {
            let page_ref = *all.get(idx).ok_or_else(|| {
                Error::Unsupported(format!(
                    "page index {idx} out of range (input document has {} pages)",
                    all.len()
                ))
            })?;
            selected.push(page_ref);
        }

        // Unique source pages in first-occurrence order; duplicates re-use the
        // same copied object and are shallow-cloned when building /Kids.
        let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
        let mut unique: Vec<ObjectRef> = Vec::with_capacity(selected.len());
        for &page_ref in &selected {
            if seen.insert(page_ref) {
                unique.push(page_ref);
            }
        }

        // qpdf removes resources from each unique source page immediately
        // before its first foreign copy. A later duplicate is a shallow page
        // copy and must not run the resource pass a second time. The source
        // inherited-attribute push above is intentionally before this call:
        // qpdf's page helper sees the effective page attributes at the copy
        // boundary, while the Auto decision above was made against the
        // original page tree.
        if remove_resources[input_index] {
            for &page_ref in &unique {
                PageObjectHelper::new(page_ref, input.source).remove_unreferenced_resources()?;
            }
        }

        // qpdf's QPDFPageDocumentHelper::addPage delegates foreign page
        // insertion to QPDF::copyForeignObject. Keep the primary Catalog and
        // `/Pages` identities in the persistent per-source ObjCopier before
        // copying selected pages, matching qpdf's in-place primary document
        // ownership (`QPDF.cc:2019-2093`).
        let source_id = input.source.unique_id();
        let mut copy_seed = target.take_foreign_object_map(source_id);
        if is_primary {
            let primary_catalog_ref = input
                .source
                .root_ref()
                .expect("page_refs above already required a resolvable /Root");
            let target_catalog_ref = target
                .root_ref()
                .expect("Pdf::empty always populates a root catalog");
            let primary_catalog_handle = input.source.get_object_handle(primary_catalog_ref);
            let primary_catalog = input.source.resolve_to_terminal(&primary_catalog_handle)?;
            let primary_pages_ref = primary_catalog
                .try_get_key(b"/Pages")?
                .object_ref()
                .expect("page_refs above already required an indirect primary /Pages");
            copy_seed.insert(primary_catalog_ref, target_catalog_ref);
            copy_seed.insert(primary_pages_ref, pages_root_ref);
        }
        target.set_foreign_object_map(source_id, copy_seed);
        for &page_ref in &unique {
            let source_page = input.source.get_object_handle(page_ref);
            let copied_page = target.copy_foreign_object(&source_page)?;
            // cov:ignore-start: QPDF::copyForeignObject returns an indirect
            // destination handle for an indirect page root; this guard protects
            // the contract if the allocator ever regresses.
            if copied_page.object_ref().is_none() {
                return Err(Error::Missing("merged page missing from foreign copy map"));
            }
            // cov:ignore-end
        }
        let page_copy_map = target.foreign_object_map_snapshot(source_id);
        // qpdf keeps the primary Catalog and trailer in the same QPDF while
        // `handlePageSpecs` mutates its page tree. Copy their values through
        // the destination-owned ObjectHandle copier, reusing the selected-page
        // map and preserving direct arrays/dictionaries without materializing
        // a legacy Object snapshot.
        if is_primary {
            wire_primary_catalog(input.source, &mut target, source_id)?;
            wire_primary_trailer(input.source, &mut target, source_id)?;
        }

        // `--preserve-unreferenced` mirrors qpdf's writer-side
        // `enqueueObjectsStandard` over the primary's complete live object
        // cache. Copy every semantic object through the same persistent map;
        // ObjStm containers are writer-owned compression artifacts and are
        // intentionally regenerated by the target writer rather than copied as
        // source streams (`QPDFWriter.cc:1093-1103,1955-2003`).
        if is_primary && preserve_primary_unreferenced {
            let target_catalog_ref = target
                .root_ref()
                .expect("Pdf::empty always populates a root catalog");
            for object_ref in input.source.live_object_refs() {
                let source_object = input.source.get_object_handle(object_ref);
                if source_object.try_is_stream_of_type(b"ObjStm", b"")? {
                    continue;
                }
                let copied = target.copy_foreign_object(&source_object)?;
                let Some(target_ref) = copied.object_ref() else {
                    continue; // cov:ignore: an indirect live source always maps to an indirect target
                };
                if target_ref != target_catalog_ref && target_ref != pages_root_ref {
                    preserved_target_refs.insert(target_ref);
                }
            }
        }

        // Keep the completed source map as the local identity view for field
        // trimming, removed-page nulling, and page-tree assembly. The map has
        // been populated only by the canonical foreign copier above.
        let map = target.take_foreign_object_map(source_id);

        null_copied_removed_pages(&mut target, &all, &seen, &map)?;

        // qpdf replaces each copied page's `/Parent` with the destination
        // `/Pages` handle during insertion. Keep the same live-handle boundary
        // after the source-side inherited-attribute push and foreign copy.
        let pages_handle = target.get_object_handle(pages_root_ref);
        for &src_ref in &unique {
            let copied_page_ref = *map
                .get(&src_ref)
                .ok_or(Error::Missing("merged page missing from copy map"))?;
            let page = target.get_object_handle(copied_page_ref);
            target.resolve(&page)?;
            page.replace_key(b"/Parent", pages_handle.clone())?;
            target.mark_object_handle_dirty(&page)?;
        }

        // Record this input's kept top-level fields (those whose source ref was
        // copied — orphan fields on unselected pages are absent from `map` and
        // so dropped, matching qpdf's form subset). The primary's `map` also
        // carries the Catalog-copied AcroForm graph.
        //
        // A NON-TERMINAL field (whose `/Kids` are widget annotations, possibly
        // on different pages) reaches the copy map whenever any one of its
        // widgets is on a selected page; the canonical page map's `/Parent` →
        // sibling-`/Kids` traversal then pulls in the field's widgets on
        // UNSELECTED pages too (and, via each such widget's `/P`, those pages as
        // off-tree orphans). Trim the field's `/Kids` to only the widgets whose
        // `/P` page survived; a field left with zero surviving widgets is
        // dropped entirely (the surrounding `map.get` guard already drops fields
        // never reached, so a zero-survivor trim only happens for malformed
        // shapes where a field ref sits directly in a page `/Annots`). Each
        // dropped widget's unselected page is collected and nulled below, so the
        // output never carries a live orphan `/Type /Page` outside `/Kids`.
        let mut orphan_pages: BTreeSet<ObjectRef> = BTreeSet::new();
        // A widget survives the field-tree trim iff it is a member of a selected
        // page's `/Annots` (or its optional `/P` resolves to a surviving page).
        // Build that retained-widget set once per input from the selected pages'
        // `/Annots`, in source space, so a `/P`-less selected-page widget is kept.
        let mut retained_widgets: BTreeSet<ObjectRef> = BTreeSet::new();
        collect_retained_widget_refs(input.source, &seen, &mut retained_widgets)?;
        for (src_field_ref, partial_name) in source_fields {
            if page_copy_map.contains_key(&src_field_ref) {
                if let Some(&target_ref) = map.get(&src_field_ref) {
                    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
                    let trimmed = trim_field_kids(
                        input.source,
                        &mut target,
                        src_field_ref,
                        &seen,
                        &retained_widgets,
                        &map,
                        &mut orphan_pages,
                        0,
                        &mut visited,
                    )?; // cov:ignore: `?` Err arm — trim_field_kids errors only on the depth guard, unreachable on well-formed input
                    if let Some(survivors) = trimmed {
                        if survivors.is_empty() {
                            // No widget survived: drop the whole field (do not record
                            // it). Its widgets' orphan pages are nulled below.
                            continue;
                        }
                        rewrite_field_kids(&mut target, src_field_ref, &survivors, &map)?;
                    }
                    kept_fields.push(KeptField {
                        target_ref,
                        partial_name,
                        is_primary,
                    });
                }
            }
        }

        // Null the copied placeholder body of each off-tree orphan page reached
        // only through a dropped widget's `/P`. This mirrors the removed-dest
        // null-out above: the page never appears in `/Kids`, and nulling its
        // body (rather than leaving a live `/Type /Page`) keeps the merged form
        // internally consistent. `sweep_unreachable_objects` later GCs the
        // placeholder once no surviving reference points at it.
        for src_page_ref in &orphan_pages {
            if let Some(&new_ref) = map.get(src_page_ref) {
                target.replace_object_handle(new_ref, ObjectHandle::null())?;
            }
        }
        // Append this input's pages to /Kids in selection order, with each
        // input resolved through its own copy map.
        append_selection_kids(&mut target, &selected, &map, &mut used, &mut kids)?;
    }

    // Build the fresh single-level /Pages root over the accumulated kids
    // through the canonical live-handle mutation boundary.
    let root = target.get_object_handle(pages_root_ref);
    target.resolve(&root)?;
    // cov:ignore-start: Pdf::empty() owns the target /Pages dictionary and no
    // merge operation replaces that slot with another value before this point.
    if root.try_as_dictionary()?.is_none() {
        return Err(Error::Unsupported(
            "target /Pages is not a dictionary".to_owned(),
        ));
    }
    // cov:ignore-end
    let kid_handles = kids
        .iter()
        .map(|&kid| target.get_object_handle(kid))
        .collect();
    root.replace_key(b"/Kids", ObjectHandle::array(kid_handles))?;
    root.replace_key(b"/Count", ObjectHandle::integer(kids.len() as i64))?;
    target.mark_object_handle_dirty(&root)?;

    // Build the merged `/AcroForm`: the primary's `/DR` / `/DA` base plus every
    // kept top-level field, with later inputs' colliding `/T` names renamed by
    // qpdf's `+N` rule. Done BEFORE the sweep so the `/DR` fonts (reachable only
    // through `/AcroForm`) are not garbage-collected.
    build_merged_acroform(&mut target, &primary_acroform, &kept_fields)?;

    // Install the merged `/PageLabels`, folding away entries that turn out
    // redundant with the running sequence (qpdf's own accumulating
    // `getLabelsForPageRange` redundancy check). A no-op when no input ever
    // carried real page labels — the target then keeps its fresh, label-less
    // catalog, matching qpdf's `emptyPDF()`-based output.
    if any_page_labels {
        let folded = merge_adjacent_ranges(label_entries);
        target.page_labels().write_reconstructed_labels(&folded)?;
    }

    // Drop the copied ancestor /Pages node(s) and any objects only they
    // referenced before handing the graph to the canonical writer. Run this
    // sweep unconditionally — qpdf's writer only skips reachability pruning
    // for objects the caller explicitly asked to preserve
    // (`QPDFWriter.cc:2907-2913`), not for the rest of the graph — and
    // protect exactly `preserved_target_refs` (empty, so a no-op, when
    // preservation is disabled) so the primary's preserved orphan objects
    // survives while incidental merge artifacts still do not.
    sweep_unreachable_objects_except(&mut target, &preserved_target_refs)?;

    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::{merge_documents, unique_field_name, MergeInput};
    use crate::{Object, Pdf};
    use std::collections::{BTreeMap, BTreeSet};

    fn build_pdf(objects: &[(u32, &str)], root: u32) -> Vec<u8> {
        let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
        let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
        let max = objects.iter().map(|(number, _)| *number).max().unwrap_or(0);
        for (number, body) in objects {
            offsets.insert(*number, out.len() as u64);
            out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let xref_start = out.len() as u64;
        let size = max + 1;
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for number in 1..=max {
            match offsets.get(&number) {
                Some(offset) => {
                    out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes())
                }
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
            )
            .as_bytes(),
        );
        out
    }

    fn used(names: &[&[u8]]) -> BTreeSet<Vec<u8>> {
        names.iter().map(|n| n.to_vec()).collect()
    }

    #[test]
    fn page_merge_production_route_has_no_legacy_borrowed_resolution() {
        let production = include_str!("page_merge.rs")
            .split_once("#[cfg(test)]")
            .expect("page_merge test module marker")
            .0;
        assert_eq!(
            production.matches("resolve_borrowed").count(),
            0,
            "page_merge production must use the canonical ObjectHandle resolver"
        );
    }

    #[test]
    fn unique_field_name_keeps_unused_base() {
        assert_eq!(unique_field_name(b"name", &used(&[])), b"name".to_vec());
        assert_eq!(
            unique_field_name(b"name", &used(&[b"email"])),
            b"name".to_vec()
        );
    }

    #[test]
    fn unique_field_name_appends_plus_one_on_collision() {
        assert_eq!(
            unique_field_name(b"name", &used(&[b"name"])),
            b"name+1".to_vec()
        );
    }

    #[test]
    fn unique_field_name_finds_first_unused_in_sequence() {
        // name, name+1 taken → name+2 (the three-way collision tail).
        assert_eq!(
            unique_field_name(b"name", &used(&[b"name", b"name+1"])),
            b"name+2".to_vec()
        );
    }

    #[test]
    fn unique_field_name_reresolves_colliding_candidate() {
        // A field originally named `name+1` whose `name+1` is already used must
        // re-resolve to `name+1+1` (qpdf 11.9.0 observed behaviour).
        assert_eq!(
            unique_field_name(b"name+1", &used(&[b"name", b"name+1"])),
            b"name+1+1".to_vec()
        );
    }

    /// A malformed indirect document-level root may itself be an unselected
    /// page. It is still copied so the catalog carrier can point at a null page
    /// boundary, but generic root traversal must stop there rather than pulling
    /// the page's `/Contents` into the foreign copier map.
    #[test]
    fn doc_level_page_root_is_null_boundary_without_contents_in_closure() {
        let bytes = build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /OpenAction 4 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (
                    4,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 10 0 R >>",
                ),
                (10, "<< /Length 4 >>\nstream\nDROP\nendstream"),
            ],
            1,
        );

        let mut merge_source = Pdf::open_mem_owned(bytes.clone()).unwrap();
        let mut inputs = [MergeInput {
            source: &mut merge_source,
            pages: vec![0],
        }];
        let mut merged = merge_documents(&mut inputs).unwrap();
        let catalog_ref = merged.root_ref().unwrap();
        let catalog = merged
            .resolve_object(catalog_ref)
            .unwrap()
            .into_dict()
            .unwrap();
        let open_action_ref = catalog
            .get_ref("OpenAction")
            .expect("indirect /OpenAction carrier is retained");
        assert_eq!(
            merged.resolve_object(open_action_ref).unwrap(),
            Object::Null,
            "the unselected page root is copied and nulled"
        );
    }

    /// [`merge_documents`] is a public library primitive
    /// (`pub use page_merge::merge_documents` in `lib.rs`), independently
    /// callable without the CLI `--pages` job's later
    /// `rebuild_acroform_in_final_page_order` correction pass
    /// (`job/page_specs.rs`), which redoes the per-occurrence field copy
    /// through a route that already decodes reserved names. This test
    /// exercises [`build_merged_acroform`]'s own collision avoidance
    /// directly, matching qpdf's `addAndRenameFormFields`
    /// (`QPDFAcroFormDocumentHelper.cc:62-103`): the primary's unselected
    /// field reserves `F+1` even though it is stored as a UTF-16BE text
    /// string (`<FEFF0046002B0031>`, BOM-prefixed), so the secondary's `F`
    /// must resolve to `F+2`, not byte-collide past the undecoded
    /// reservation and reuse `F+1`.
    #[test]
    fn merge_documents_reserves_unselected_primary_names_regardless_of_string_encoding() {
        let primary_bytes = build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 6 0 R >>"),
                (2, "<< /Type /Pages /Count 2 /Kids [3 0 R 4 0 R] >>"),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
                ),
                (
                    4,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [7 0 R] >>",
                ),
                (
                    5,
                    "<< /Type /Annot /Subtype /Widget /FT /Tx /T (F) /Rect [0 0 10 10] /P 3 0 R >>",
                ),
                (6, "<< /Fields [5 0 R 7 0 R] >>"),
                (
                    7,
                    "<< /Type /Annot /Subtype /Widget /FT /Tx /T <FEFF0046002B0031> \
                     /Rect [0 0 10 10] /P 4 0 R >>",
                ),
            ],
            1,
        );
        let secondary_bytes = build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
                (2, "<< /Type /Pages /Count 1 /Kids [3 0 R] >>"),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R] >>",
                ),
                (
                    4,
                    "<< /Type /Annot /Subtype /Widget /FT /Tx /T (F) /Rect [0 0 10 10] /P 3 0 R >>",
                ),
                (5, "<< /Fields [4 0 R] >>"),
            ],
            1,
        );

        let mut primary = Pdf::open_mem_owned(primary_bytes).unwrap();
        let mut secondary = Pdf::open_mem_owned(secondary_bytes).unwrap();
        let mut inputs = [
            MergeInput {
                source: &mut primary,
                pages: vec![0],
            },
            MergeInput {
                source: &mut secondary,
                pages: vec![0],
            },
        ];
        let mut merged = merge_documents(&mut inputs).unwrap();

        let catalog_ref = merged.root_ref().unwrap();
        let catalog = merged
            .resolve_object(catalog_ref)
            .unwrap()
            .into_dict()
            .unwrap();
        let acroform_ref = catalog.get_ref("AcroForm").expect("/AcroForm");
        let acroform = merged
            .resolve_object(acroform_ref)
            .unwrap()
            .into_dict()
            .unwrap();
        let fields = acroform
            .get("Fields")
            .and_then(Object::as_array)
            .expect("/Fields array");
        let names: Vec<Vec<u8>> = fields
            .iter()
            .map(|field| {
                let field_ref = field.as_ref_id().expect("field is an indirect ref");
                let field_dict = merged
                    .resolve_object(field_ref)
                    .unwrap()
                    .into_dict()
                    .unwrap();
                field_dict
                    .get("T")
                    .and_then(Object::as_string)
                    .expect("/T string")
                    .to_vec()
            })
            .collect();

        assert_eq!(
            names,
            vec![b"F".to_vec(), b"F+2".to_vec()],
            "the secondary's colliding field must resolve to F+2, matching qpdf's \
             getUTF8Value()-decoded collision index, not F+1 from an undecoded byte \
             comparison against the UTF-16BE-encoded reservation"
        );
    }
}

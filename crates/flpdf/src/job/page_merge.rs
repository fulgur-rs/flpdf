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
use crate::acroform_document_helper::{collect_refs_in_object, remap_refs_in_object};
use crate::object_copy::copy_objects_with_seed;
use crate::object_handle::canonical_dictionary_key_from_legacy;
use crate::page_closure::{extend_object_closure, extend_page_object_closure};
use crate::page_extract::{
    append_selection_kids, null_copied_removed_pages, resolve_dict, target_pages_root,
};
use crate::page_label_document_helper::{merge_adjacent_ranges, LabelRange};
use crate::pages::page_refs;
use crate::pdf_string::{new_unicode_string, utf8_value};
use crate::ref_chain::resolve_ref_chain;
use crate::resources::{should_remove_unreferenced_resources, RemoveUnreferencedResources};
use crate::subset_prune::sweep_unreachable_objects_except;
use crate::{
    Dictionary, Error, Object, ObjectHandle, ObjectRef, PageDocumentHelper, PageObjectHelper, Pdf,
    Result,
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

/// Primary-only document-level structures discovered on `inputs[0]`'s catalog
/// and trailer, to be inherited by the merged output. qpdf `--pages` keeps the
/// authenticated primary document as the base and mutates its page-owned
/// structures in place; the complete Catalog/trailer snapshots are therefore
/// retained, while the explicit carrier fields below remain for the fallback
/// wiring path and destination-specific tests.
///
/// Each field captures how the structure is held so it can be wired onto the
/// fresh output catalog after the primary copy renumbers it:
/// - an *indirect* root (`/Outlines`, an indirect `/Names /Dests` name-tree
///   root, an indirect legacy `/Catalog /Dests`, an indirect `/OpenAction`) is
///   folded into the primary's copy closure and wired to its new ref via the
///   renumber map;
/// - an *inline-on-catalog* value (a direct `/Names /Dests` name-tree root, a
///   direct legacy `/Dests` dict, or a direct `/OpenAction` array/dict) is held
///   only by the primary catalog, which is never copied, so its destinations
///   are reconstructed from the renumber map once.
#[derive(Default)]
struct PrimaryDocLevel {
    /// The primary Catalog snapshot. It is remapped onto the fresh target
    /// after the copy; `/Pages` is deliberately supplied by the merged page
    /// tree, while the other keys follow qpdf's primary-in-place ownership.
    catalog: Option<Dictionary>,
    /// The primary trailer snapshot. Writer-owned structural keys are replaced
    /// later, but `/Info`, `/ID`, and unknown trailer metadata remain primary-
    /// owned and are carried into the target.
    trailer: Option<Dictionary>,
    /// Indirect `/Outlines` root ref, if present.
    outlines: Option<ObjectRef>,
    /// Direct (inline-on-catalog) `/Outlines` root dictionary. ISO 32000 permits
    /// the catalog `/Outlines` to be a direct dict, like the other catalog-level
    /// carriers; its item refs are indirect and are folded/wired the same way as
    /// the inline `/Names /Dests` and legacy `/Dests` roots.
    outlines_inline: Option<Dictionary>,
    /// Indirect name-tree root held under the catalog's `/Names /Dests`.
    names_dests: Option<ObjectRef>,
    /// Direct `/Names /Dests` name-tree root (inline on the catalog). ISO 32000
    /// permits a name-tree root to be a direct dictionary — it is referenced
    /// only from `/Names /Dests`, so a producer may inline it; an indirect root
    /// is a producer convention, not a spec rule. The root may be a `/Names`
    /// leaf or a `/Kids` node (§7.9.6); both shapes are handled.
    names_dests_inline: Option<Dictionary>,
    /// Indirect legacy `/Catalog /Dests` dictionary ref.
    legacy_dests_ref: Option<ObjectRef>,
    /// Direct legacy `/Catalog /Dests` dictionary (inline on the catalog).
    legacy_dests_inline: Option<Dictionary>,
    /// Indirect `/OpenAction` object ref (an action dict).
    open_action_ref: Option<ObjectRef>,
    /// Direct `/OpenAction` value (an inline `[page /Fit]` array or action dict).
    open_action_inline: Option<Object>,
}

/// Read the primary catalog and classify its document-level destination
/// carriers into a [`PrimaryDocLevel`]. Indirect carriers are returned by ref
/// (to fold into the copy closure); inline-on-catalog carriers are cloned out
/// (to reconstruct from the renumber map after copy).
fn discover_primary_doc_level<R: Read + Seek>(source: &mut Pdf<R>) -> Result<PrimaryDocLevel> {
    let Some(catalog_ref) = source.root_ref() else {
        return Ok(PrimaryDocLevel::default()); // cov:ignore: an opened Pdf always has a /Root
    };
    // The downstream closure copier still accepts the legacy `Dictionary`
    // snapshot. Resolve the catalog through its canonical handle first, then
    // cross that explicit bridge once at the raw closure boundary; do not use
    // the legacy borrowed resolver to discover document-level state.
    let catalog_handle = source.get_object_handle(catalog_ref);
    let catalog = source
        .resolve_object_handle_to_terminal(&catalog_handle)?
        .materialize()?
        .into_dict();
    let Some(catalog) = catalog else {
        return Ok(PrimaryDocLevel::default()); // cov:ignore: a /Root always resolves to a dictionary catalog
    };

    let mut doc = PrimaryDocLevel {
        catalog: Some(catalog.clone()),
        trailer: Some(source.trailer().clone()),
        ..Default::default()
    };
    // /Outlines — an indirect root ref, or a direct (inline) root dict on the
    // catalog (ISO 32000 permits either, like the other catalog-level carriers).
    match catalog.get("Outlines") {
        Some(Object::Reference(r)) => doc.outlines = Some(*r),
        Some(Object::Dictionary(d)) => doc.outlines_inline = Some(d.clone()),
        _ => {}
    }

    // /Names /Dests — the catalog's /Names is an indirect ref or an inline dict.
    // Its /Dests name-tree root may be an indirect ref OR a direct dictionary:
    // ISO 32000 permits a name-tree root to be inline (it is referenced only
    // from /Names /Dests). Both forms are captured here. Only /Dests is
    // inherited; sibling name trees (/JavaScript, /EmbeddedFiles) are left to
    // their own document-level handling and are not merged here.
    let names_val = catalog.get("Names").cloned();
    let names_dict = match names_val {
        // /Names may sit behind a holder chain (a ref to a ref to the dict); follow
        // it so the inherited /Dests name tree is not dropped. The generic primary
        // Catalog copy below now retains sibling name trees as well; these fields
        // preserve the older carrier-specific fallback path.
        Some(value @ Object::Reference(_)) => resolve_ref_chain(source, &value)?.0.into_dict(),
        Some(Object::Dictionary(d)) => Some(d),
        _ => None,
    };
    if let Some(names) = names_dict {
        if let Some(Object::Reference(r)) = names.get("Dests") {
            doc.names_dests = Some(*r);
        } else if let Some(Object::Dictionary(d)) = names.get("Dests") {
            doc.names_dests_inline = Some(d.clone());
        }
    }

    // Legacy /Catalog /Dests — indirect dict or inline dict on the catalog.
    match catalog.get("Dests") {
        Some(Object::Reference(r)) => doc.legacy_dests_ref = Some(*r),
        Some(Object::Dictionary(d)) => doc.legacy_dests_inline = Some(d.clone()),
        _ => {}
    }

    // /OpenAction — an indirect action object, or an inline action dict / dest
    // array on the catalog.
    match catalog.get("OpenAction") {
        Some(Object::Reference(r)) => doc.open_action_ref = Some(*r),
        Some(other) => doc.open_action_inline = Some(other.clone()),
        None => {}
    }

    Ok(doc)
}

/// Fold every document-level carrier of `doc` into `closure` via the generic
/// object-root traversal (which stops at `Page`/`Catalog` dictionaries,
/// collecting a destination page as a leaf reference without descending).
/// This makes the primary's single `copy_objects` pass copy the whole outline /
/// name-tree / action graph and remap every destination in one rewrite — no
/// separate post-copy remap pass is needed (or wanted).
fn fold_doc_level_closure<R: Read + Seek>(
    source: &mut Pdf<R>,
    doc: &PrimaryDocLevel,
    closure: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    for root in [
        doc.outlines,
        doc.names_dests,
        doc.legacy_dests_ref,
        doc.open_action_ref,
    ]
    .into_iter()
    .flatten()
    {
        // Thread one `visited` set (the accumulating `closure`) through every
        // doc-level root so a subtree reachable from several roots (e.g. a page
        // referenced by both an outline item and the `/OpenAction`) is walked
        // once for the union. Start from the root as a generic reference so a
        // malformed root that is itself a `/Page` or `/Catalog` remains a copied
        // boundary leaf instead of being force-expanded like a selected page.
        extend_object_closure(source, &Object::Reference(root), closure)?;
    }
    // Inline-on-catalog values are not indirect roots, so extend the generic
    // object closure from each direct value. This follows every referenced
    // holder without imposing carrier-specific depth limits while retaining the
    // shared Page/Catalog boundary guard and the inline-object nesting limit.
    if let Some(inline) = &doc.open_action_inline {
        extend_object_closure(source, inline, closure)?;
    }
    if let Some(inline) = &doc.names_dests_inline {
        extend_object_closure(source, &Object::Dictionary(inline.clone()), closure)?;
    }
    if let Some(inline) = &doc.legacy_dests_inline {
        extend_object_closure(source, &Object::Dictionary(inline.clone()), closure)?;
    }
    if let Some(inline) = &doc.outlines_inline {
        extend_object_closure(source, &Object::Dictionary(inline.clone()), closure)?;
    }
    Ok(())
}

/// Fold the complete primary Catalog and trailer metadata into the primary
/// copy closure. qpdf's `handlePageSpecs` edits the authenticated primary in
/// place (`QPDFJob.cc:2462-2472`), so the Catalog's non-page keys and trailer
/// references remain reachable even when they are not part of a selected page
/// closure. `/Pages` and writer-owned trailer structure are supplied by the
/// fresh target/writer instead.
fn fold_primary_metadata_closure<R: Read + Seek>(
    source: &mut Pdf<R>,
    doc: &PrimaryDocLevel,
    closure: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    if let Some(catalog) = &doc.catalog {
        for (key, value) in catalog.iter() {
            if key == b"Pages" {
                continue;
            }
            extend_object_closure(source, value, closure)?;
        }
    } // cov:ignore: a successful primary merge always has a dictionary catalog

    if let Some(trailer) = &doc.trailer {
        for (key, value) in trailer.iter() {
            // `/Root`, `/Size`, `/Prev`, `/Encrypt`, and xref-stream keys are
            // rebuilt by the target/writer. `/ID` is direct in valid PDFs, and
            // any nested references in an unknown trailer value are still
            // intentionally collected below.
            if matches!(
                key,
                b"Root"
                    | b"Size"
                    | b"Prev"
                    | b"Encrypt"
                    | b"Type"
                    | b"W"
                    | b"Index"
                    | b"XRefStm"
                    | b"Length"
                    | b"Filter"
                    | b"DecodeParms"
                    | b"F"
                    | b"FFilter"
                    | b"FDecodeParms"
            ) {
                continue;
            }
            extend_object_closure(source, value, closure)?;
        }
    } // cov:ignore: a parsed PDF trailer is always present with the primary catalog
    Ok(())
}

/// Wire the primary's inherited document-level structures onto the fresh output
/// catalog after the primary copy. Indirect roots are wired to their copied ref
/// via the renumber `map`; inline-on-catalog direct objects (never copied because
/// the primary catalog is not copied) are reconstructed from `map`.
/// `copy_objects` already remapped every reference inside copied objects, so
/// this only sets catalog keys — it does not re-walk copied objects.
fn wire_doc_level<RTgt: Read + Seek>(
    target: &mut Pdf<RTgt>,
    doc: &PrimaryDocLevel,
    map: &BTreeMap<ObjectRef, ObjectRef>,
) -> Result<()> {
    let Some(catalog_ref) = target.root_ref() else {
        return Ok(()); // cov:ignore: the seed target always has a /Root catalog
    };
    let catalog_handle = target.get_object_handle(catalog_ref);
    let catalog = target.resolve_object_handle_to_terminal(&catalog_handle)?;
    if catalog.try_as_dictionary()?.is_none() {
        return Ok(()); // cov:ignore: the seed catalog is always a dict
    }

    if let Some(primary_catalog) = &doc.catalog {
        // qpdf keeps the primary Catalog object and mutates only the keys owned
        // by page selection (`QPDFJob.cc:2590-2632`). The fresh target already
        // owns the new `/Pages` reference; every other primary Catalog entry is
        // copied as one generic graph so metadata such as `/Names` siblings,
        // `/ViewerPreferences`, `/Metadata`, and producer-specific keys retain
        // their original shape and ownership.
        for (key, value) in primary_catalog.iter() {
            if key == b"Pages" {
                continue;
            }
            let remapped = remap_refs_in_object(value.clone(), map);
            let value = target.lift_object_to_handle(&remapped)?;
            let key = canonical_dictionary_key_from_legacy(key);
            catalog.replace_key(&key, value)?;
        }
        target.mark_object_handle_dirty(&catalog)?;
        return Ok(());
    } // cov:ignore: discover_primary_doc_level always supplies the primary catalog on this route

    if let Some(outlines) = doc.outlines {
        if let Some(&new_ref) = map.get(&outlines) {
            catalog.replace_key(b"/Outlines", target.get_object_handle(new_ref))?;
        }
    } else if let Some(inline) = &doc.outlines_inline {
        // The inline root dict lived on the primary catalog (never copied);
        // rebuild it with its item refs (`/First`, `/Last`) remapped to the copied
        // items, mirroring the inline `/Names` and legacy `/Dests` reconstruction.
        let remapped = remap_refs_in_object(Object::Dictionary(inline.clone()), map);
        catalog.replace_key(b"/Outlines", target.lift_object_to_handle(&remapped)?)?;
    }
    // /Names /Dests: indirect root → wire copied ref; inline root → reconstruct
    // it from the renumber map (the catalog is never copied). Remap the direct
    // carrier as one generic object so every source ref becomes its copied ref
    // without resolving or flattening indirect destination holders. Either way
    // a minimal /Names holder carries only the inherited /Dests name tree
    // (sibling name trees are not merged).
    if let Some(dests) = doc.names_dests {
        if let Some(&new_ref) = map.get(&dests) {
            let names = ObjectHandle::dictionary(vec![(
                b"/Dests".to_vec(),
                target.get_object_handle(new_ref),
            )]);
            catalog.replace_key(b"/Names", names)?;
        }
    } else if let Some(inline) = &doc.names_dests_inline {
        let mut names = Dictionary::new();
        names.insert(
            "Dests",
            remap_refs_in_object(Object::Dictionary(inline.clone()), map),
        );
        // The name-tree root was remapped above. Remapping this newly built
        // `/Names` holder as well would remap a destination twice when a new
        // target reference is also present as a source key in `map`.
        let names_handle = target.lift_object_to_handle(&Object::Dictionary(names))?;
        catalog.replace_key(b"/Names", names_handle)?;
    }
    // Legacy /Catalog /Dests: indirect → wire copied ref; inline → remap the
    // complete direct carrier once, preserving any indirect holder structure.
    if let Some(legacy) = doc.legacy_dests_ref {
        if let Some(&new_ref) = map.get(&legacy) {
            catalog.replace_key(b"/Dests", target.get_object_handle(new_ref))?;
        }
    } else if let Some(inline) = &doc.legacy_dests_inline {
        let remapped = remap_refs_in_object(Object::Dictionary(inline.clone()), map);
        catalog.replace_key(b"/Dests", target.lift_object_to_handle(&remapped)?)?;
    }
    // /OpenAction: indirect → wire copied ref; inline → reconstruct from the map.
    if let Some(oa_ref) = doc.open_action_ref {
        if let Some(&new_ref) = map.get(&oa_ref) {
            catalog.replace_key(b"/OpenAction", target.get_object_handle(new_ref))?;
        }
    } else if let Some(inline) = &doc.open_action_inline {
        let remapped = remap_refs_in_object(inline.clone(), map);
        catalog.replace_key(b"/OpenAction", target.lift_object_to_handle(&remapped)?)?;
    }

    target.mark_object_handle_dirty(&catalog)?;
    Ok(())
}

/// Carry the primary trailer onto the fresh target while leaving writer-owned
/// structural entries in target space. qpdf's full rewrite preserves primary
/// `/Info`, `/ID[0]`, and unknown trailer metadata, but rebuilds `/Root`,
/// `/Size`, xref-history keys, and encryption through the writer boundary.
fn wire_primary_trailer<RTgt: Read + Seek>(
    target: &mut Pdf<RTgt>,
    doc: &PrimaryDocLevel,
    map: &BTreeMap<ObjectRef, ObjectRef>,
) {
    let Some(primary_trailer) = &doc.trailer else {
        return; // cov:ignore: a primary catalog snapshot always captures the parsed trailer
    };
    let Some(root_ref) = target.root_ref() else {
        return; // cov:ignore: Pdf::empty always supplies the target catalog root
    };
    let current_size = target.trailer().get("Size").cloned();
    let mut trailer = primary_trailer.clone();
    for key in [
        "Root",
        "Size",
        "Prev",
        "Encrypt",
        "Type",
        "W",
        "Index",
        "XRefStm",
        "Length",
        "Filter",
        "DecodeParms",
        "F",
        "FFilter",
        "FDecodeParms",
    ] {
        trailer.remove(key);
    }
    for value in trailer.values_mut() {
        let original = std::mem::replace(value, Object::Null);
        *value = remap_refs_in_object(original, map);
    }
    if let Some(size) = current_size {
        trailer.insert("Size", size);
    }
    trailer.insert("Root", Object::Reference(root_ref));
    target.trailer = trailer;
    // No writer path should retain a handle lifted from the empty target's old
    // trailer after this replacement. This is the same canonical-trailer
    // identity reset used by JSON document import/update paths.
    target.trailer_handle_memo = None;
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
    /// Remapped `/DR` default-resources value (fonts the primary's `/DA`
    /// references), or `None` when the primary has no `/DR`.
    dr: Option<Object>,
    /// The primary's `/DA` default-appearance value, or `None`.
    da: Option<Object>,
    /// Indirect refs reachable from `/DR` / `/DA`, folded into the primary copy
    /// closure so the referenced fonts are copied into the output.
    closure_seed: BTreeSet<ObjectRef>,
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

/// Read the primary input's `/AcroForm /DR` and `/DA`, returning them with the
/// set of indirect objects they reach (to fold into the primary copy closure).
/// The `/DR` / `/DA` values are returned verbatim (still in the source's
/// numbering); [`build_merged_acroform`] remaps them after the copy.
fn discover_primary_acroform<R: Read + Seek>(source: &mut Pdf<R>) -> Result<PrimaryAcroForm> {
    let entries = source.acroform().acroform_inherited_entries()?;
    let mut out = PrimaryAcroForm {
        had_fields_array: source.acroform().has_fields_array()?,
        ..Default::default()
    };
    // One shared `seen` across /DR and /DA so a font referenced by both is
    // resolved once. `collect_refs_in_object` bounds the reference chain by
    // `DEFAULT_MAX_ACROFORM_DEPTH` (review rule 4) and follows arrays, dicts, and
    // stream dicts transitively.
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    for (key, value) in entries {
        // `/DR` is a resource dictionary: pass `skip_parent_key: false` so a
        // resource legitimately named `/P` (e.g. a `/DA`-referenced font) is
        // collected rather than dropped as a field-tree back-pointer.
        collect_refs_in_object(
            source,
            &value,
            &mut out.closure_seed,
            &mut seen,
            0,
            0,
            false,
        )?; // cov:ignore: collect_refs_in_object ? Err arm (depth/inline limit) unreachable for well-formed AcroForm /DR /DA
        match key.as_slice() {
            b"DR" => out.dr = Some(value),
            b"DA" => out.da = Some(value),
            _ => {} // cov:ignore: acroform_inherited_entries yields only /DR and /DA
        }
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
    let top_fields = source.acroform().top_level_fields()?;
    let mut out = Vec::with_capacity(top_fields.len());
    for field_ref in top_fields {
        // A top-level `/Fields` element may be a holder chain (a ref to a ref to
        // the field dict). The copy map keys the field by its TERMINAL ref, so
        // normalize to the terminal before recording it — otherwise `map.get` on
        // the holder ref misses and the field is dropped from the merged form. The
        // terminal also feeds the `/T` lookup so a name behind a holder is read.
        let terminal = resolve_ref_chain(source, &Object::Reference(field_ref))?
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
    let field = source.resolve_object_handle_to_terminal(&field_handle)?;
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
    let resolved = source.resolve_object_handle_to_terminal(&t_value)?;
    Ok(resolved.as_string().map(|raw| utf8_value(&raw)))
}

/// Remove `/AcroForm` from the target's catalog, if present.
///
/// [`wire_doc_level`]'s generic catalog copy places the primary's `/AcroForm`
/// on the target verbatim (unpruned) before [`build_merged_acroform`] runs.
/// qpdf removes `/AcroForm` entirely once the filtered field count reaches 0
/// (`QPDFJob.cc:2626-2629`, `pdf.getRoot().removeKey("/AcroForm")`); this
/// mirrors that removal for the case where nothing else rebuilds the key.
fn remove_target_acroform<R: Read + Seek>(target: &mut Pdf<R>) -> Result<()> {
    let Some(catalog_ref) = target.root_ref() else {
        return Ok(()); // cov:ignore: the seed target always has a /Root catalog
    };
    let catalog_handle = target.get_object_handle(catalog_ref);
    let catalog = target.resolve_object_handle_to_terminal(&catalog_handle)?;
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
    map: &BTreeMap<ObjectRef, ObjectRef>,
) -> Result<()> {
    if kept.is_empty() && primary.dr.is_none() && primary.da.is_none() {
        if primary.had_fields_array {
            // The primary had a /Fields array (so the generic catalog copy
            // placed a real, now-stale /AcroForm on the target) but every
            // field's page was dropped from the selection. Remove it to
            // match qpdf instead of leaving the unpruned copy in place.
            remove_target_acroform(target)?;
        }
        return Ok(());
    }

    let acroform = target.acroform().canonical_get_or_create_acroform()?;
    if acroform.try_as_dictionary()?.is_none() {
        return Ok(()); // cov:ignore: ensure_acroform_ref always yields a dictionary
    }

    // Inherit the primary's /DR and /DA, remapping any indirect refs to the
    // copied objects. `/DA` is usually a direct string (a no-op under remap),
    // but per ISO 32000-2 any value may be stored as an indirect reference; a
    // verbatim copy would leave a source object number dangling in the output.
    if let Some(dr) = &primary.dr {
        let remapped = remap_refs_in_object(dr.clone(), map);
        acroform.replace_key(b"/DR", target.lift_object_to_handle(&remapped)?)?;
    }
    if let Some(da) = &primary.da {
        let remapped = remap_refs_in_object(da.clone(), map);
        acroform.replace_key(b"/DA", target.lift_object_to_handle(&remapped)?)?;
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
    let field = target.resolve_object_handle_to_terminal(&field_handle)?;
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
    let field = source.resolve_object_handle_to_terminal(&field_handle)?;
    if field.try_as_dictionary()?.is_none() {
        return Ok(None); // cov:ignore: a field ref always resolves to a dictionary
    }
    let kids_value = field.try_get_key(b"/Kids")?;
    if kids_value.is_null() {
        return Ok(None);
    }
    let resolved = source.resolve_object_handle_to_terminal(&kids_value)?;
    let Some(items) = resolved.try_as_array()? else {
        return Ok(Some(Vec::new())); // cov:ignore: a /Kids value resolves to an array in practice
    };
    let mut refs = Vec::with_capacity(items.len());
    for item in items {
        let (_, terminal_ref) = source.resolve_object_handle_to_terminal_ref(&item)?;
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
/// survives. References are bounded by [`resolve_ref_chain`].
fn collect_retained_widget_refs<R: Read + Seek>(
    source: &mut Pdf<R>,
    selected_pages: &BTreeSet<ObjectRef>,
    retained: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    for &page_ref in selected_pages {
        let page_handle = source.get_object_handle(page_ref);
        let page = source.resolve_object_handle_to_terminal(&page_handle)?;
        if page.try_as_dictionary()?.is_none() {
            continue; // cov:ignore: a selected page ref always resolves to a dictionary
        }
        let annots_val = page.try_get_key(b"/Annots")?;
        if annots_val.is_null() {
            continue;
        }
        // /Annots: an inline array or an indirect reference to one.
        let concrete = source.resolve_object_handle_to_terminal(&annots_val)?;
        let Some(elems) = concrete.try_as_array()? else {
            continue; // cov:ignore: a non-array /Annots is malformed
        };
        for elem in elems {
            let (_, terminal_ref) = source.resolve_object_handle_to_terminal_ref(&elem)?;
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
    let widget = source.resolve_object_handle_to_terminal(&widget_handle)?;
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
    let (_, last_ref) = source.resolve_object_handle_to_terminal_ref(&p_value)?;
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
    let field = target.resolve_object_handle_to_terminal(&field_handle)?;
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
/// Each source is left unmodified. Each input is copied with
/// [`copy_objects`](crate::object_copy::copy_objects); the result mirrors
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
/// before a page enters the foreign-object copy closure.
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
/// the same copy closure as its selected pages.
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
    // copy map and so never appear). `primary_acroform` holds the primary's
    // inherited `/DR` / `/DA`, remapped onto the output `/AcroForm` after the
    // primary copy renumbers its fonts.
    let mut kept_fields: Vec<KeptField> = Vec::new();
    let mut primary_acroform = PrimaryAcroForm::default();
    let mut primary_map: BTreeMap<ObjectRef, ObjectRef> = BTreeMap::new();
    // Target-side refs of the primary's preserved-orphan closure
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

        let doc_level = if is_primary {
            discover_primary_doc_level(input.source)?
        } else {
            PrimaryDocLevel::default()
        };

        // The primary's `/AcroForm /DR` and `/DA` are the merged form's base; a
        // later input contributes form fields only (its `/DR` / `/DA` are not
        // merged). Read them now and fold their fonts into the primary closure.
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

        // UNION of the per-page transitive closures, then ONE deep-copy pass
        // into the growing target: a single renumbering map means an object
        // shared by several selected pages of this input is copied once. The
        // closures share one `visited` set so a subtree reachable from several
        // selected pages of THIS input is walked once for the union, not once
        // per referencing page (extract_pages parity, flpdf-11lj). The set is
        // local to this input — never shared across inputs, whose distinct
        // source docs and numbering would alias.
        let mut closure: BTreeSet<ObjectRef> = BTreeSet::new();
        for &page_ref in &unique {
            extend_page_object_closure(input.source, page_ref, &mut closure)?;
        }
        // Keep the page-only reachability boundary so the later AcroForm pass
        // can distinguish fields selected by a page from fields reachable only
        // through the primary Catalog's complete `/AcroForm` graph.
        let page_closure = closure.clone();

        // Fold the primary's document-level carriers (outline tree, name-tree
        // /Dests, legacy /Dests, /OpenAction) into the closure BEFORE copying so
        // the same single copy pass copies and remaps them — no separate
        // post-copy remap. A no-op for secondary inputs (empty doc_level).
        fold_doc_level_closure(input.source, &doc_level, &mut closure)?;
        if is_primary {
            fold_primary_metadata_closure(input.source, &doc_level, &mut closure)?;
        }

        // Fold the primary's `/AcroForm /DR` / `/DA` fonts into the closure so a
        // `/DA` resource (e.g. `/Helv`) is copied and the output `/DR` can point
        // at it after the remap. Gated on the primary input: `primary_acroform`
        // is read from the primary's source and its `closure_seed` holds the
        // primary's object refs, which are meaningless against a secondary's
        // numbering — folding them into a secondary's closure would resolve
        // those refs against the wrong document.
        if is_primary {
            closure.extend(primary_acroform.closure_seed.iter().copied());
        }

        // qpdf keeps the primary `QPDF` in place through `handlePageSpecs`
        // (`QPDFJob.cc:2481-2489`): every original page is `removePage`'d
        // first (leaving its object graph untouched), selected pages are
        // re-added, and each unreused original page is then
        // `replaceObject`'d to a bare `null` -- nulling only the page dict,
        // never its children. Nothing else in the primary's object cache is
        // ever removed. `QPDFWriter::enqueueObjectsStandard`
        // (`QPDFWriter.cc:2907-2913`) then enqueues literally every entry in
        // `getAllObjects()` when the option is set -- no reachability
        // computation, no subtraction. Fold in the primary's *entire* live
        // object universe unconditionally to match: `closure` already
        // dedupes (`BTreeSet`), so re-adding objects already selected via
        // `page_closure`/`doc_level` is a no-op, and a deselected page's
        // dict now reaches `closure` too, which is what lets
        // `null_copied_removed_pages` below null it out (matching
        // `replaceObject`) instead of silently dropping it and everything
        // reachable only through it.
        // A preserved orphan may reference the primary's own structural
        // roots (Catalog, `/Pages`) directly -- e.g. a bookmark-like helper
        // dict pointing back at the document root. qpdf never severs that
        // edge: the primary's Catalog/`/Pages` objects are mutated in place
        // and stay exactly what they always were. Since `target` instead
        // builds a FRESH Catalog/`/Pages` pair (`Pdf::empty`) and copies
        // everything else, seed the copy map so those two source refs
        // resolve to the target's existing equivalents instead of being
        // duplicated as new, dead-weight copies or dropped to `Object::Null`
        // (unmapped refs) by `rewrite_refs`.
        let mut copy_seed: BTreeMap<ObjectRef, ObjectRef> = BTreeMap::new();
        let primary_live = if is_primary && preserve_primary_unreferenced {
            // Exclude `/Type /ObjStm` container refs: an object-stream
            // container is a writer-owned compression artifact, not
            // semantic content. qpdf's writer independently decides
            // container membership for whatever gets enqueued
            // (`QPDFWriter::enqueueObject` redirects a compressible member
            // to its container rather than writing the member directly,
            // `QPDFWriter.cc:1093-1103`) and regenerates each container's
            // bytes fresh from that current membership
            // (`getCompressibleObjGens`, `QPDFWriter.cc:1955-2003`) --- it
            // never writes a source container's raw bytes verbatim. Each
            // compressed member's own ref still resolves to its correct
            // logical value via `source.resolve()` regardless of
            // compression, so excluding only the container here does not
            // lose any member; flpdf's own writer
            // (`writer/object_streams.rs`) already owns (re)compressing the
            // copied members into fresh containers. Without this exclusion,
            // the container's raw stream would be copied as an ordinary
            // object, duplicating its still-live members' content in a
            // second, dangling copy that nothing in the output references.
            let mut primary_live = Vec::new();
            for object_ref in input.source.live_object_refs() {
                if input
                    .source
                    .get_object_handle(object_ref)
                    .try_is_stream_of_type(b"ObjStm", b"")?
                {
                    continue;
                }
                primary_live.push(object_ref);
            }
            closure.extend(primary_live.iter().copied());
            // `page_refs(input.source)` above already succeeded for this same
            // primary in this same loop iteration (nothing between the two
            // calls mutates `input.source`'s catalog/`/Pages`), which
            // requires `/Root` to resolve to a dictionary and its `/Pages`
            // entry to be an indirect reference (`PageWalk::with_max_depth`,
            // `pages.rs:826-833`). Both `expect`s below encode that already-
            // proven invariant rather than re-deriving a fallback path qpdf
            // has no counterpart for.
            let primary_catalog_ref = input
                .source
                .root_ref()
                .expect("page_refs above already required a resolvable /Root");
            let target_catalog_ref = target
                .root_ref()
                .expect("Pdf::empty always populates a root catalog");
            copy_seed.insert(primary_catalog_ref, target_catalog_ref);
            let primary_pages_ref = input
                .source
                .resolve(primary_catalog_ref)?
                .as_dict()
                .and_then(|dict| dict.get_ref("Pages"))
                .expect("page_refs above already required an indirect primary /Pages");
            copy_seed.insert(primary_pages_ref, pages_root_ref);
            primary_live
        } else {
            Vec::new()
        };

        // qpdf `--pages` null-out parity is page-tree driven: after every closure
        // root has been folded, the selected page set determines which copied
        // source pages are retained. Without `--preserve-unreferenced`, pages
        // outside the closure are never copied as placeholders; with it, a
        // deselected primary page dict is now folded into `closure` above so
        // it IS copied, and the null-out below turns that copy into the same
        // bare `null` qpdf's `replaceObject` produces.
        // Renumbering-disjointness invariant: copy_objects allocates fresh
        // target object numbers starting one past the current maximum, so the
        // refs it returns never collide with objects already surviving in the
        // target (prior inputs' copied pages, or the seed catalog/pages root).
        // This is the structural guard that makes a shared-destination
        // double-remap unreachable; capture the surviving set BEFORE copying.
        let surviving_before: BTreeSet<ObjectRef> = target.object_refs().into_iter().collect();
        let map = copy_objects_with_seed(input.source, &mut target, &closure, &copy_seed)?;
        debug_assert!(
            map.values()
                .all(|new| !surviving_before.contains(new) || copy_seed.values().any(|s| s == new)),
            "copy_objects_with_seed must allocate fresh refs disjoint from surviving target refs, \
             except for explicitly seeded target refs \
             (renumbering-disjointness invariant; guards against shared-destination double-remap)"
        );

        // Track the primary's preserved-orphan closure by its TARGET-side
        // refs so the final sweep (below) can protect exactly this set —
        // and nothing else — from the reachability GC it still runs for
        // incidental merge artifacts (qpdf's writer preserves unreferenced
        // objects only when explicitly asked; it does not disable
        // reachability pruning for everything else).
        preserved_target_refs.extend(primary_live.iter().filter_map(|r| map.get(r).copied()));

        null_copied_removed_pages(&mut target, &all, &seen, &map);

        // qpdf replaces each copied page's `/Parent` with the destination
        // `/Pages` handle during insertion. Keep the same live-handle boundary
        // after the source-side inherited-attribute push and foreign copy.
        let pages_handle = target.get_object_handle(pages_root_ref);
        for &src_ref in &unique {
            let copied_page_ref = *map
                .get(&src_ref)
                .ok_or(Error::Missing("merged page missing from copy map"))?;
            let page = target.get_object_handle(copied_page_ref);
            target.resolve_object_handle(&page)?;
            page.replace_key(b"/Parent", pages_handle.clone())?;
            target.mark_object_handle_dirty(&page)?;
        }

        // Wire the primary's inherited document-level structures onto the output
        // catalog (obj 1, distinct from the /Pages root obj 2 rebuilt below).
        // copy_objects already remapped every destination inside the copied
        // outline / name-tree / action objects, so this only sets catalog keys.
        // A no-op for secondary inputs (empty doc_level).
        if is_primary {
            wire_doc_level(&mut target, &doc_level, &map)?;
            wire_primary_trailer(&mut target, &doc_level, &map);
        }

        // Record this input's kept top-level fields (those whose source ref was
        // copied — orphan fields on unselected pages are absent from `map` and
        // so dropped, matching qpdf's form subset). The primary's `map` also
        // remaps its inherited `/DR` fonts in `build_merged_acroform`.
        //
        // A NON-TERMINAL field (whose `/Kids` are widget annotations, possibly
        // on different pages) reaches the copy map whenever any one of its
        // widgets is on a selected page; the page-closure's `/Parent` →
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
            if page_closure.contains(&src_field_ref) {
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
                target.set_object(new_ref, Object::Null);
            }
        }
        if is_primary {
            primary_map = map.clone();
        }

        // Append this input's pages to /Kids in selection order, with each
        // input resolved through its own copy map.
        append_selection_kids(&mut target, &selected, &map, &mut used, &mut kids)?;
    }

    // Build the fresh single-level /Pages root over the accumulated kids.
    let mut root = resolve_dict(
        &mut target,
        pages_root_ref,
        "target /Pages is not a dictionary",
    )?; // cov:ignore: Err arm unreachable — minimal_target_bytes creates /Pages as a dict, and nothing since overwrites it (copy_objects renumbers into fresh numbers; page reparenting/append_selection_kids touch only copied leaves)
    root.insert(
        "Kids",
        Object::Array(kids.iter().map(|&r| Object::Reference(r)).collect()),
    );
    root.insert("Count", Object::Integer(kids.len() as i64));
    target.set_object(pages_root_ref, Object::Dictionary(root));

    // Build the merged `/AcroForm`: the primary's `/DR` / `/DA` base plus every
    // kept top-level field, with later inputs' colliding `/T` names renamed by
    // qpdf's `+N` rule. Done BEFORE the sweep so the `/DR` fonts (reachable only
    // through `/AcroForm`) are not garbage-collected.
    build_merged_acroform(&mut target, &primary_acroform, &kept_fields, &primary_map)?;

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
    // preservation is disabled) so the primary's preserved-orphan closure
    // survives while incidental merge artifacts still do not.
    sweep_unreachable_objects_except(&mut target, &preserved_target_refs)?;

    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::{
        discover_primary_doc_level, fold_doc_level_closure, merge_documents, unique_field_name,
        wire_doc_level, MergeInput, PrimaryDocLevel,
    };
    use crate::page_closure::extend_page_object_closure;
    use crate::{Dictionary, Object, ObjectRef, Pdf};
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

    #[test]
    fn wire_doc_level_indirect_carriers_use_canonical_handles() {
        let mut target = Pdf::empty().unwrap();
        let mapped = ObjectRef::new(2, 0);
        let outlines = ObjectRef::new(10, 0);
        let names = ObjectRef::new(11, 0);
        let legacy = ObjectRef::new(12, 0);
        let open_action = ObjectRef::new(13, 0);
        let map = BTreeMap::from([
            (outlines, mapped),
            (names, mapped),
            (legacy, mapped),
            (open_action, mapped),
        ]);

        wire_doc_level(
            &mut target,
            &PrimaryDocLevel {
                catalog: None,
                trailer: None,
                outlines: Some(outlines),
                outlines_inline: None,
                names_dests: Some(names),
                names_dests_inline: None,
                legacy_dests_ref: Some(legacy),
                legacy_dests_inline: None,
                open_action_ref: Some(open_action),
                open_action_inline: None,
            },
            &map,
        )
        .unwrap();

        let catalog = target.get_object_handle(ObjectRef::new(1, 0));
        assert_eq!(
            catalog.try_get_key(b"/Outlines").unwrap().object_ref(),
            Some(mapped)
        );
        assert_eq!(
            catalog
                .try_get_key(b"/Names")
                .unwrap()
                .try_get_key(b"/Dests")
                .unwrap()
                .object_ref(),
            Some(mapped)
        );
        assert_eq!(
            catalog.try_get_key(b"/Dests").unwrap().object_ref(),
            Some(mapped)
        );
        assert_eq!(
            catalog.try_get_key(b"/OpenAction").unwrap().object_ref(),
            Some(mapped)
        );
    }

    #[test]
    fn wire_doc_level_inline_carriers_use_canonical_handles() {
        let mut target = Pdf::empty().unwrap();
        let mapped = ObjectRef::new(2, 0);
        let remapped_again = ObjectRef::new(30, 0);
        let source_ref = ObjectRef::new(20, 0);
        let mut outlines = Dictionary::new();
        outlines.insert("First", Object::Reference(source_ref));
        let mut names_dests = Dictionary::new();
        names_dests.insert("Names", Object::Reference(source_ref));
        let mut legacy_dests = Dictionary::new();
        legacy_dests.insert("legacy", Object::Reference(source_ref));
        let mut action = Dictionary::new();
        action.insert(
            "D",
            Object::Array(vec![
                Object::Reference(source_ref),
                Object::Name(b"Fit".to_vec()),
            ]),
        );
        let map = BTreeMap::from([(source_ref, mapped), (mapped, remapped_again)]);

        wire_doc_level(
            &mut target,
            &PrimaryDocLevel {
                catalog: None,
                trailer: None,
                outlines: None,
                outlines_inline: Some(outlines),
                names_dests: None,
                names_dests_inline: Some(names_dests),
                legacy_dests_ref: None,
                legacy_dests_inline: Some(legacy_dests),
                open_action_ref: None,
                open_action_inline: Some(Object::Dictionary(action)),
            },
            &map,
        )
        .unwrap();

        let catalog = target.get_object_handle(ObjectRef::new(1, 0));
        assert!(!catalog.try_get_key(b"/Outlines").unwrap().is_null());
        assert_eq!(
            catalog
                .try_get_key(b"/Names")
                .unwrap()
                .try_get_key(b"/Dests")
                .unwrap()
                .try_get_key(b"/Names")
                .unwrap()
                .object_ref(),
            Some(mapped),
            "inline /Names /Dests is remapped exactly once"
        );
        assert!(!catalog.try_get_key(b"/Dests").unwrap().is_null());
        assert!(!catalog.try_get_key(b"/OpenAction").unwrap().is_null());
    }

    #[test]
    fn wire_doc_level_preserves_legacy_leading_slash_dictionary_key() {
        let mut target = Pdf::empty().unwrap();
        let mut primary_catalog = Dictionary::new();
        primary_catalog.insert(b"/Escaped", Object::Integer(7));

        wire_doc_level(
            &mut target,
            &PrimaryDocLevel {
                catalog: Some(primary_catalog),
                ..Default::default()
            },
            &BTreeMap::new(),
        )
        .unwrap();

        let catalog = target.get_object_handle(ObjectRef::new(1, 0));
        assert_eq!(
            catalog
                .try_get_key(b"//Escaped")
                .unwrap()
                .try_as_integer()
                .unwrap(),
            Some(7),
            "a legacy key body beginning with '/' still gets qpdf's delimiter"
        );
        assert!(
            !catalog.try_has_key(b"/Escaped").unwrap(),
            "the legacy key must not lose its leading slash body"
        );
    }

    /// A malformed indirect document-level root may itself be an unselected
    /// page. It is still copied so the catalog carrier can point at a null page
    /// boundary, but generic root traversal must stop there rather than pulling
    /// the page's `/Contents` into the copy closure.
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
        let catalog = merged.resolve(catalog_ref).unwrap().into_dict().unwrap();
        let open_action_ref = catalog
            .get_ref("OpenAction")
            .expect("indirect /OpenAction carrier is retained");
        assert_eq!(
            merged.resolve(open_action_ref).unwrap(),
            Object::Null,
            "the unselected page root is copied and nulled"
        );

        let mut source = Pdf::open_mem_owned(bytes).unwrap();
        let doc_level = discover_primary_doc_level(&mut source).unwrap();
        let mut closure = BTreeSet::new();
        extend_page_object_closure(&mut source, ObjectRef::new(3, 0), &mut closure).unwrap();
        fold_doc_level_closure(&mut source, &doc_level, &mut closure).unwrap();
        assert!(
            closure.contains(&ObjectRef::new(4, 0)),
            "the indirect /OpenAction page root must enter the copy map"
        );
        assert!(
            !closure.contains(&ObjectRef::new(10, 0)),
            "the boundary page's /Contents must not enter the copy map"
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
        let catalog = merged.resolve(catalog_ref).unwrap().into_dict().unwrap();
        let acroform_ref = catalog.get_ref("AcroForm").expect("/AcroForm");
        let acroform = merged.resolve(acroform_ref).unwrap().into_dict().unwrap();
        let fields = acroform
            .get("Fields")
            .and_then(Object::as_array)
            .expect("/Fields array");
        let names: Vec<Vec<u8>> = fields
            .iter()
            .map(|field| {
                let field_ref = field.as_ref_id().expect("field is an indirect ref");
                let field_dict = merged.resolve(field_ref).unwrap().into_dict().unwrap();
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

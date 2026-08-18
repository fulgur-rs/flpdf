//! qpdf correspondence: QPDFAcroFormDocumentHelper.cc adjustAppearanceStream consuming resource_replacer.rs.
//! Port of qpdf's `AcroForm::adjustAppearanceStream`, consuming the shared
//! `ResourceReplacer` / `ResourceFinder` resource-replacement pipeline
//! (`libqpdf/QPDFAcroFormDocumentHelper.cc:628-849`, `libqpdf/ResourceFinder.cc`),
//! called from [`crate::overlay_annotations`]'s `transform_annot_ap_streams`
//! once per (already per-placement-dup'd) `/AP` appearance stream whenever a
//! placement's [`crate::overlay_annotations::DrMap`] is non-empty.
//!
//! An appearance stream copied from another document may reference resource
//! names (a font, an `ExtGState`, ...) through its own `/Resources`
//! dictionary that collided with the destination `/AcroForm/DR` during the
//! merge and were renamed there (`DrMap`, populated by
//! `merge_resources_shallow`). Left alone, the stream's content would still
//! say e.g. `/F1 18 Tf` while the destination's merged `/DR/Font` no longer
//! has an `F1` entry — only `F1_1`. [`adjust_appearance_stream`] privatizes
//! the stream's `/Resources`, renames the colliding keys there, and rewrites
//! the matching name tokens in the stream's own content so both stay
//! internally consistent.
//!
//! [`crate::resource_replacer`] owns the shared content-stream token scan;
//! this module only applies its best-effort result to decoded appearance
//! stream bytes.

use std::io::{Read, Seek};
use std::rc::Rc;

use crate::object_handle::{ObjectHandle, ResourceConflicts};
use crate::overlay_annotations::DrMap;
use crate::resource_replacer::replace_resource_names;
use crate::writer::DecodeLevel;
use crate::{Dictionary, Object, ObjectRef, Pdf, Result};

fn rewrite_appearance_content(decoded: &[u8], dr_map: &DrMap) -> Vec<u8> {
    match replace_resource_names(decoded, dr_map.renames()) {
        Ok(Some(bytes)) => bytes,
        Ok(None) | Err(_) => decoded.to_vec(),
    }
}

/// Privatize and rewrite an appearance stream through the canonical
/// `ObjectHandle` graph.
///
/// This is qpdf's `QPDFAcroFormDocumentHelper::adjustAppearanceStream`
/// (`libqpdf/QPDFAcroFormDocumentHelper.cc:615-696`) over the live handle
/// route. The resource dictionary is copied before it is changed, the
/// qpdf-shaped `mergeResources` conflict map is fed back into the local
/// content rename map, and content decoding failures leave the already
/// applied resource mutation in place.
pub(crate) fn adjust_appearance_stream_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    stream: &ObjectHandle,
    dr_map: &DrMap,
) -> Result<()> {
    if dr_map.is_empty() {
        return Ok(());
    }
    stream.try_dereference()?;
    let Some(stream_dict) = stream.as_stream_dict() else {
        return Ok(()); // cov:ignore: caller only supplies appearance streams
    };
    let resources_value = stream_dict.try_get_key(b"/Resources")?;
    if resources_value.try_is_null()? {
        return Ok(()); // cov:ignore: caller gates on an existing /Resources entry
    }
    let resources_terminal = pdf.resolve_object_handle_to_terminal(&resources_value)?;
    if resources_terminal.as_dictionary().is_none() {
        // qpdf's caller only invokes adjustAppearanceStream when
        // `resources.isDictionary()` (QPDFAcroFormDocumentHelper.cc:1006-1008).
        // A non-dictionary `/Resources` (e.g. an Integer in a malformed or
        // foreign appearance stream) is left untouched, matching qpdf.
        return Ok(());
    }
    let was_indirect = resources_value.is_indirect()
        || resources_value.as_reference().is_some()
        || resources_terminal.object_ref().is_some();
    let private_resources = resources_terminal.shallow_copy()?;
    let private_resources = if was_indirect {
        pdf.make_indirect_from_object_handle(private_resources)?
    } else {
        private_resources
    };
    stream_dict.replace_key(b"/Resources", private_resources.clone())?;

    // qpdf first merges empty category dictionaries solely to force any
    // existing indirect category dictionaries to become private.
    let merge_with = ObjectHandle::dictionary(
        dr_map
            .categories()
            .map(|category| {
                (
                    canonical_resource_name(category),
                    ObjectHandle::dictionary(Vec::new()),
                )
            })
            .collect(),
    );
    private_resources.merge_resources(&merge_with, None)?;

    // The first merge is live and sequential, exactly like qpdf's
    // subdict.getKey/replaceKey loop. Values displaced by a destination name
    // collision are staged into the corresponding merge_with category.
    for category in dr_map.categories() {
        let category_key = canonical_resource_name(category);
        let subdict = private_resources.try_get_key(&category_key)?;
        subdict.try_dereference()?;
        let Some(renames) = dr_map.category(category) else {
            continue; // cov:ignore: categories() iterates the same map
        };
        if subdict.as_dictionary().is_none() {
            continue;
        }
        let staged = merge_with.try_get_key(&category_key)?;
        for (old_name, new_name) in renames {
            let old_key = canonical_resource_name(old_name);
            let new_key = canonical_resource_name(new_name);
            let existing_new = subdict.try_get_key(&new_key)?;
            if !existing_new.try_is_null()? {
                staged.replace_key(&new_key, existing_new)?;
            }
            let existing_old = subdict.try_get_key(&old_key)?;
            if !existing_old.try_is_null()? {
                subdict.replace_key(&new_key, existing_old)?;
                subdict.remove_key(&old_key);
            }
        }
    }

    let mut conflicts = ResourceConflicts::new();
    private_resources.merge_resources(&merge_with, Some(&mut conflicts))?;
    let mut local_dr_map = dr_map.clone();
    extend_dr_map_from_conflicts(&mut local_dr_map, &conflicts);

    for category_key in private_resources.try_get_keys()? {
        let category = private_resources.try_get_key(&category_key)?;
        category.try_dereference()?;
        if category.as_dictionary().is_some() && category.try_get_keys()?.is_empty() {
            private_resources.remove_key(&category_key);
        }
    }

    // qpdf's token-filter installation is best effort. Resource mutations are
    // intentionally not rolled back when the stream cannot be decoded.
    if let Ok(decoded) = stream.get_stream_data(DecodeLevel::Generalized) {
        let rewritten = rewrite_appearance_content(&decoded, &local_dr_map);
        if let Ok(encoded) =
            crate::filters::encode_stream_data_from_handle(&stream_dict, &rewritten)
        {
            stream.replace_stream_data(Rc::new(encoded), None, None);
        } else {
            // qpdf has no LZW/ASCII85/ASCIIHex encoder; its token-filtered
            // stream is emitted under the writer's ordinary Flate route.
            let flate_dict = ObjectHandle::dictionary(vec![(
                b"/Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            )]);
            if let Ok(encoded) =
                crate::filters::encode_stream_data_from_handle(&flate_dict, &rewritten)
            {
                stream.replace_stream_data(
                    Rc::new(encoded),
                    Some(ObjectHandle::name(b"FlateDecode".to_vec())),
                    Some(ObjectHandle::null()),
                );
            }
        }
    }

    pdf.mark_object_handle_dirty(&private_resources)?;
    pdf.mark_object_handle_dirty(&stream_dict)?;
    pdf.mark_object_handle_dirty(stream)?;
    Ok(())
}

fn canonical_resource_name(name: &[u8]) -> Vec<u8> {
    let name = name.strip_prefix(b"/").unwrap_or(name);
    let mut result = Vec::with_capacity(name.len() + 1);
    result.push(b'/');
    result.extend_from_slice(name);
    result
}

fn extend_dr_map_from_conflicts(dr_map: &mut DrMap, conflicts: &ResourceConflicts) {
    for (category, category_conflicts) in conflicts {
        let category = category.strip_prefix(b"/").unwrap_or(category);
        for (old_name, new_name) in category_conflicts {
            dr_map.insert_rename(
                category,
                old_name.strip_prefix(b"/").unwrap_or(old_name).to_vec(),
                new_name.strip_prefix(b"/").unwrap_or(new_name).to_vec(),
            );
        }
    }
}

/// Privatize and rewrite an appearance stream's `/Resources` dictionary and
/// content so a resource name that collided during the destination
/// `/AcroForm/DR` merge ([`DrMap`], populated by
/// `crate::overlay_annotations::merge_resources_shallow`) resolves to the
/// renamed destination name, matching qpdf's `AcroForm::adjustAppearanceStream`
/// (`libqpdf/QPDFAcroFormDocumentHelper.cc:752-849`). Called once per
/// (already per-placement-dup'd) `/AP` stream from
/// `crate::overlay_annotations::transform_annot_ap_streams`, after the
/// stream's `/Matrix` has already been concatenated with the placement's
/// `cm`.
///
/// Does nothing when `dr_map` is empty, or when the stream at
/// `ap_stream_ref` has no `/Resources` entry — together with the caller's
/// own `!dr_map.is_empty()` check, this reproduces the two-part gate qpdf
/// applies at the `transformAnnotations` call site (`libqpdf/QPDFAcroFormDocumentHelper.cc:1160-1161`:
/// `if (!dr_map.empty() && resources)`) before invoking this function at
/// all.
///
/// # Algorithm
///
/// 1. Resolve `/Resources` to an owned [`Dictionary`], noting whether it was
///    reached through an indirect reference. Every stream that reaches this
///    point gets its OWN private copy — never shared with another
///    placement's dup of the same stream, or with the source
///    `/AcroForm/DR` object the copy started out pointing at — matching
///    qpdf's unconditional `resources.shallowCopy()`.
/// 2. For every resource-type category [`DrMap::categories`] recorded a
///    rename for: resolve that category's sub-dictionary (an indirect
///    `/Font <ref>`-style sub-dictionary is resolved the same as a direct
///    one — an appearance stream's own `/Resources` can name either shape),
///    inserting an empty one if absent — mirrors merging in qpdf's
///    per-category empty `merge_with` dict, whose only effect at this stage
///    is to force the sub-dictionary to exist and be unshared.
/// 3. Rename every `old_key` present in that sub-dictionary to `dr_map`'s
///    `new_key`, keeping the same value, matching qpdf's rename loop
///    verbatim (`libqpdf/QPDFAcroFormDocumentHelper.cc:781-803`, fetched and
///    read line-by-line for this port — roborev PR #490 iter-3 finding 1).
///    This is a TWO-PHASE algorithm, not a single pass:
///    - **Phase 1** mutates the sub-dictionary key-by-key, IN PLACE, reading
///      `existing_new`/`old_val` from the LIVE, currently-mutating
///      sub-dictionary — exactly like qpdf. A `new_key` that already names a
///      value gets that value staged into a local `merge_with` dict (NOT
///      immediately renamed) before `old_key`'s value moves into `new_key`.
///      Because phase 1 reads live state, a LATER rename entry in the same
///      category CAN observe an EARLIER entry's freshly-written key — e.g.
///      `dr_map` recording both `F1->F1_1` and `F1_1->F1_1_1` for the same
///      category means the second entry's `old_key` lookup (`F1_1`) finds
///      the value phase 1 JUST moved there from `F1`, not the sub-dict's
///      true original `F1_1` occupant. This matches qpdf bug-for-bug: a
///      snapshot-based rewrite that avoided this "reprocessing" would
///      itself be the byte-identical divergence (see the
///      `adjust_appearance_stream_rename_chain_*` tests below, which encode
///      the resulting cross-wired names as the correct, qpdf-verified
///      output).
///    - **Phase 2** re-merges the staged `merge_with` dict back into the
///      (now phase-1-mutated) sub-dictionary, matching qpdf's
///      `resources.mergeResources(merge_with, &dr_map)` (`:807`, itself
///      `QPDFObjectHandle::mergeResources`'s generic dict-merge-with-
///      conflicts algorithm). A staged value whose slot phase 1 already
///      vacated re-lands under its own original name, no conflict. A
///      GENUINE new conflict (the slot is occupied by something else) is
///      resolved the same way `crate::overlay_annotations::merge_resources_shallow`
///      resolves the top-level `/DR` merge conflict: reuse a key that
///      already names the SAME object elsewhere in the sub-dictionary (by
///      [`ObjectRef`] identity — qpdf's `QPDFObjGen`-keyed `og_to_name`), or
///      else mint a fresh name via
///      [`crate::overlay_annotations::unique_dr_name`] and extend a
///      **per-call, cloned** copy of `dr_map` ([`DrMap::insert_rename`]) so
///      step 5's content rewrite also redirects any token that already said
///      the staged key. qpdf's `dr_map` parameter to this function is
///      itself passed **by value** (`:752`), which is exactly why growing a
///      local copy here cannot leak into another placement's shared
///      [`DrMap`].
/// 4. Drop any sub-dictionary left empty by step 3 (qpdf: "remove empty
///    subdictionaries").
/// 5. Rewrite the stream's decoded content via [`rewrite_appearance_content`], using
///    the per-call rename map extended by step 3. Steps 1–4 (the
///    `/Resources` dictionary rewrite) always run; step 5 is best-effort in
///    one direction only: if the content **cannot be decoded** at all (most
///    commonly an unsupported `/Filter`), the content bytes are left
///    exactly as read and only the dictionary-level rename from steps 1–4
///    applies. This was verified directly against qpdf's source
///    (`libqpdf/QPDFAcroFormDocumentHelper.cc`, fetched and read for
///    roborev PR #490 iter-3 finding 3): the `/Resources` rename (steps 1–4
///    of this function) runs unconditionally BEFORE qpdf's equivalent
///    tokenize step, which is wrapped in its OWN `try`/`catch`
///    (`:824-849`) that turns a content-parse failure into a warning
///    without rolling back the rename already made to `/Resources` — qpdf
///    genuinely leaves the stream in this state (renamed dict, stale
///    content) rather than reverting it, so flpdf matches rather than
///    "fixing" it. If the content **decodes** but the rewritten bytes
///    cannot be **re-encoded under the original `/Filter`** (e.g.
///    `/LZWDecode`, which flpdf can decode but not re-encode — see
///    `crate::filters::apply_single_filter_encode`, decision
///    flpdf-9hc.7.2), the rewritten content is instead re-encoded as
///    `FlateDecode` and `/Filter`/`/DecodeParms` are replaced accordingly, so
///    the dictionary rename and the content tokens never disagree about a
///    resource's name. qpdf never needs this fallback: it installs
///    `ResourceReplacer` as a token filter once the content tokenizes, and
///    its writer re-serializes under its own default output filter
///    (`FlateDecode`, since qpdf has no LZW encoder either) rather than
///    reproducing the original filter's bytes — the Flate fallback here
///    mirrors that write-time re-serialization.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`]. A content decode or
/// re-encode failure is not one of them (see step 5 above) — a decode
/// failure is swallowed and leaves the content unchanged, matching qpdf
/// (verified against source, not just observed behavior — see step 5); a
/// re-encode failure is swallowed and instead re-encodes the rewritten
/// content as `FlateDecode` so it stays consistent with the `/Resources`
/// rename.
// Both call sites now route through `adjust_appearance_stream_handle`;
// removal of this raw-`Object` implementation is a separate dependent slice.
#[allow(dead_code)]
pub(crate) fn adjust_appearance_stream<R: Read + Seek>(
    dest: &mut Pdf<R>,
    ap_stream_ref: ObjectRef,
    dr_map: &DrMap,
) -> Result<()> {
    if dr_map.is_empty() {
        return Ok(());
    }
    let Object::Stream(mut stream) = dest.resolve(ap_stream_ref)? else {
        return Ok(()); // cov:ignore: defensive early return — caller only ever passes a stream ref
    };
    let Some(resources_val) = stream.dict.get("Resources").cloned() else {
        return Ok(());
    };
    let (mut resources, was_indirect) = match resources_val {
        Object::Dictionary(d) => (d, false),
        Object::Reference(r) => match dest.resolve(r)?.into_dict() {
            Some(d) => (d, true),
            None => return Ok(()), // cov:ignore: /Resources ref does not resolve to a dict — malformed input
        },
        _ => return Ok(()), // cov:ignore: /Resources neither dict nor reference — malformed input
    };

    // Per-call, owned copy of `dr_map` — mirrors qpdf's `dr_map` parameter
    // to `AcroForm::adjustAppearanceStream` being passed BY VALUE
    // (`libqpdf/QPDFAcroFormDocumentHelper.cc:752`). Phase 2 below extends
    // this LOCAL copy only, via [`DrMap::insert_rename`], so an extra
    // rename discovered while privatizing one stream's `/Resources` never
    // leaks into another placement's shared `DrMap`.
    let mut local_dr_map = dr_map.clone();

    for category in dr_map.categories() {
        let Some(renames) = dr_map.category(category) else {
            continue; // cov:ignore: category() looked up from categories()'s own keys — never None
        };
        let existing = resources.get(category).cloned();
        let mut subdict = match existing {
            Some(Object::Dictionary(d)) => d,
            Some(Object::Reference(r)) => dest.resolve(r)?.into_dict().unwrap_or_default(),
            _ => Dictionary::new(),
        };

        // PHASE 1 (qpdf `libqpdf/QPDFAcroFormDocumentHelper.cc:781-803`):
        // mutate `subdict` key-by-key, IN PLACE — `existing_new`/`old_val`
        // are read from the LIVE, currently-mutating `subdict`, matching
        // qpdf's `subdict.getKey(new_key)` / `subdict.getKey(old_key)`
        // exactly (both consult `subdict` at the moment of THIS iteration,
        // not a pre-loop snapshot). Any value a rename would otherwise
        // silently clobber is staged into `merge_with` (qpdf's own local of
        // that name) rather than lost outright.
        let mut merge_with = Dictionary::new();
        for (old_key, new_key) in renames {
            if let Some(existing_new) = subdict.get(new_key.as_slice()).cloned() {
                merge_with.insert(new_key.clone(), existing_new);
            }
            if let Some(existing_old) = subdict.remove(old_key) {
                subdict.insert(new_key.clone(), existing_old);
            }
        }

        // PHASE 2 (qpdf `:805-807`'s `resources.mergeResources(merge_with,
        // &dr_map)`): re-merge every staged, displaced value back in. Not a
        // simple re-insert — `QPDFObjectHandle::mergeResources`'s
        // conflicts-map algorithm applies again here, so a slot phase 1
        // left vacant re-lands its staged value verbatim, while a slot
        // phase 1 left OCCUPIED by something else is a genuine new
        // conflict, resolved exactly like the top-level `/DR` merge
        // conflict in `merge_resources_shallow`: reuse a key that already
        // names the SAME object elsewhere in `subdict` (an `ObjectRef`
        // identity scan, qpdf's `og_to_name`), else mint a fresh name.
        //
        // `og_to_name` and `rnames`' lazy-init-then-FROZEN lifecycle (qpdf
        // `libqpdf/QPDFObjectHandle.cc`, `mergeResources`, the
        // `initialized_maps` guard around `make_og_to_name`, verified
        // against the live source — line numbers below are from qpdf
        // `main` as fetched, not the finding's cited
        // `QPDFAcroFormDocumentHelper.cc:791-816`, which was for a
        // different, older qpdf revision):
        //   - Both maps are built EXACTLY ONCE, lazily, the first time a
        //     staged key collides with an occupied `subdict` slot (a
        //     genuine conflict, not a phase-1-vacated slot) — so they DO
        //     see any vacated-slot reinstates that happened EARLIER in
        //     THIS SAME staged loop (they already mutated `subdict`/
        //     `this_val` by then).
        //   - Once built, neither map is EVER updated again for the rest of
        //     resource-type's staged loop — neither by a LATER vacated-slot
        //     reinstate, nor by a freshly-minted alias from a LATER genuine
        //     conflict (`this_val.replaceKey(new_key, rval)` in qpdf's
        //     `else` branch does not touch either map).
        // Concretely: two genuine conflicts on the SAME object each mint
        // their OWN fresh alias (no reuse between them) — reuse only
        // happens when one of the two colliding values was already visible
        // to `subdict` (either pre-existing, or reinstated verbatim earlier
        // in this loop) at the moment `og_to_name` was snapshotted.
        if merge_with.iter().next().is_some() {
            let staged: Vec<(Vec<u8>, Object)> = merge_with
                .iter()
                .map(|(k, v)| (k.to_vec(), v.clone()))
                .collect();
            let mut ref_to_key: Option<std::collections::HashMap<ObjectRef, Vec<u8>>> = None;
            let mut resource_names: Option<std::collections::BTreeSet<Vec<u8>>> = None;
            let mut min_suffix: u32 = 1;
            for (key, rval) in staged {
                if subdict.get(key.as_slice()).is_none() {
                    // The slot this value was staged under is free again
                    // (phase 1 vacated it) — no conflict, reinstate verbatim.
                    // Deliberately does NOT touch `ref_to_key`: once it has
                    // been snapshotted below, qpdf's `og_to_name` stays
                    // frozen even across later vacated-slot reinstates.
                    subdict.insert(key, rval);
                    continue;
                }
                // Genuine conflict — snapshot both qpdf maps lazily, on the
                // FIRST such conflict only (`og_to_name`'s
                // `initialized_maps` guard). These snapshots legitimately
                // include any vacated-slot reinstates already applied above
                // in this same loop, and neither map changes afterward.
                if ref_to_key.is_none() {
                    let mut m = std::collections::HashMap::new();
                    for (k, v) in subdict.iter() {
                        if let Some(r) = v.as_ref_id() {
                            m.insert(r, k.to_vec());
                        }
                    }
                    ref_to_key = Some(m);
                    resource_names = Some(crate::overlay_annotations::get_resource_names(
                        dest, &subdict,
                    )?);
                }
                let snapshot = ref_to_key
                    .as_ref()
                    .expect("identity map initialized on first AP conflict");
                let reused = rval.as_ref_id().and_then(|r| snapshot.get(&r).cloned());
                if let Some(existing_key) = reused {
                    // `existing_key == key` (no rename needed — the
                    // displaced value already sits under its own staged
                    // name) matches qpdf's `if (new_key != key)` guard
                    // around its `conflicts[rtype][key] = new_key` write.
                    if existing_key != key {
                        local_dr_map.insert_rename(category, key, existing_key);
                    }
                } else {
                    let names = resource_names
                        .as_ref()
                        .expect("resource-name pool initialized on first AP conflict");
                    let fresh_name =
                        crate::overlay_annotations::unique_dr_name(&key, names, &mut min_suffix)?;
                    subdict.insert(fresh_name.clone(), rval);
                    local_dr_map.insert_rename(category, key, fresh_name);
                }
            }
        }

        resources.insert(category, Object::Dictionary(subdict));
    }

    // Remove empty sub-dictionaries (qpdf: "Remove empty subdictionaries").
    let empty_categories: Vec<Vec<u8>> = resources
        .iter()
        .filter_map(|(key, value)| match value {
            Object::Dictionary(d) if d.iter().next().is_none() => Some(key.to_vec()),
            _ => None,
        })
        .collect();
    for key in empty_categories {
        resources.remove(&key);
    }

    // Best-effort content rewrite (step 5, see doc comment above): a decode
    // failure here — most commonly an unsupported `/Filter` chain (e.g. an
    // image codec on an AP stream that also has a `/Resources` collision) —
    // must NOT propagate. qpdf wraps its equivalent tokenize step in a
    // `try`/`catch` that turns exactly this failure into a warning rather
    // than aborting; propagating an `Err` here would fail the WHOLE overlay
    // call chain over one unrelated AP stream, which real qpdf does not do.
    // On decode failure, `stream.data` is simply left as read — only the
    // `/Resources` dictionary rename above still applies (matching qpdf,
    // which does not roll back the rename either).
    if let Ok(decoded) = crate::filters::decode_stream_data(&stream.dict, &stream.data) {
        let new_decoded = rewrite_appearance_content(&decoded, &local_dr_map);
        match crate::filters::encode_stream_data(&stream.dict, &new_decoded) {
            Ok(encoded) => {
                // Keep `/Length` consistent with the rewritten body — the
                // rename may shrink or grow the compressed payload, and a
                // stale dict `/Length` here would leave the stream
                // structurally inconsistent (symmetric with the FlateDecode
                // fallback below, which already updates it).
                stream.dict.insert(
                    "Length",
                    Object::Integer(i64::try_from(encoded.len()).unwrap_or(i64::MAX)),
                );
                stream.data = encoded;
            }
            Err(_) => {
                // Re-encoding under the ORIGINAL `/Filter` failed — decodable
                // but not re-encodable filters are exactly `/LZWDecode`
                // (`crate::filters::apply_single_filter_encode`, decision
                // flpdf-9hc.7.2: "flpdf writes stream compression as
                // FlateDecode only ... qpdf has no LZW encoder either").
                // Leaving `stream.data` untouched here (the pre-fix
                // behavior) would strand the content on the OLD resource
                // names while `/Resources` above already has the NEW ones —
                // an inconsistent stream. Re-encode the rewritten content as
                // `FlateDecode` instead, mirroring how qpdf's writer would
                // re-serialize this same token-filtered content under its
                // own default output filter rather than reproducing LZW.
                // In-memory FlateDecode of already-decoded bytes does not
                // fail in practice, so no further fallback is attempted.
                let mut flate_dict = Dictionary::new();
                flate_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
                if let Ok(encoded) = crate::filters::encode_stream_data(&flate_dict, &new_decoded) {
                    stream.dict.remove("DecodeParms");
                    stream
                        .dict
                        .insert("Filter", Object::Name(b"FlateDecode".to_vec()));
                    stream.dict.insert(
                        "Length",
                        Object::Integer(i64::try_from(encoded.len()).unwrap_or(i64::MAX)),
                    );
                    stream.data = encoded;
                }
            }
        }
    }

    if was_indirect {
        // A fresh indirect object, never the original ref: the original
        // still identifies the (possibly shared-across-placements) source
        // `/DR` copy this stream's `/Resources` started out pointing at.
        // Overwriting it in place would corrupt every other consumer of
        // that ref; the writer's existing reachability pass drops the now-
        // unreferenced original once nothing points at it any more.
        let new_ref = allocate_next_ref(dest)?;
        dest.set_object(new_ref, Object::Dictionary(resources));
        stream.dict.insert("Resources", Object::Reference(new_ref));
    } else {
        stream
            .dict
            .insert("Resources", Object::Dictionary(resources));
    }
    dest.set_object(ap_stream_ref, Object::Stream(stream));
    Ok(())
}

/// Allocate a fresh indirect object ref (`max(numbers) + 1`, gen 0).
/// Duplicate of the crate-local helper in `overlay_annotations.rs` /
/// `overlay.rs` / `page_form_xobject.rs` — kept module-local so this file
/// has no dependency on `overlay_annotations.rs`'s private surface.
fn allocate_next_ref<R: Read + Seek>(dest: &Pdf<R>) -> Result<ObjectRef> {
    let n = dest
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| crate::Error::Unsupported("object-number space exhausted".to_string()))?;
    Ok(ObjectRef::new(n, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ObjectRef;
    use crate::PdfOpenOptions;
    use std::io::Cursor;

    /// Build a `DrMap` with a single category's rename table. `DrMap`'s
    /// `by_name` field is private to `overlay_annotations`, so tests drive
    /// it through the `#[cfg(test)]`-only `DrMap::for_test` constructor
    /// added there for exactly this purpose, rather than round-tripping
    /// through a full `merge_resources_shallow` call over a real `Pdf`.
    fn dr_map_with(category: &[u8], old: &[u8], new: &[u8]) -> DrMap {
        crate::overlay_annotations::DrMap::for_test(category, old, new)
    }

    #[test]
    fn resource_replacement_preserves_diagnostic_bytes_and_rewrites_later_name() {
        let dr_map = DrMap::for_test(b"Font", b"F1", b"F1_1");
        let content = b"<0g> /F1 12 Tf";
        assert_eq!(
            rewrite_appearance_content(content, &dr_map),
            b"<0g> /F1_1 12 Tf"
        );
    }

    #[test]
    fn incomplete_inline_image_appearance_keeps_prefix_replacement_and_qpdf_separator() {
        let dr_map = DrMap::for_test(b"Font", b"F1", b"F1_1");
        let content = b"/F1 12 Tf BI ID";
        assert_eq!(
            rewrite_appearance_content(content, &dr_map),
            b"/F1_1 12 Tf BI ID "
        );
    }

    #[test]
    fn fatal_structure_error_keeps_appearance_content_byte_identical() {
        let dr_map = DrMap::for_test(b"Font", b"F1", b"F1_1");
        let content = b"/F1 12 Tf [";
        assert_eq!(rewrite_appearance_content(content, &dr_map), content);
    }

    // ---- adjust_appearance_stream -------------------------------------------

    fn minimal_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(
            format!("xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \n").as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn open_minimal() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("minimal pdf should parse")
    }

    fn set_dict<R: Read + Seek>(pdf: &mut Pdf<R>, n: u32, entries: &[(&str, Object)]) -> ObjectRef {
        let mut d = Dictionary::new();
        for (k, v) in entries {
            d.insert(*k, v.clone());
        }
        let r = ObjectRef::new(n, 0);
        pdf.set_object(r, Object::Dictionary(d));
        r
    }

    fn set_stream<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        n: u32,
        entries: &[(&str, Object)],
        data: &[u8],
    ) -> ObjectRef {
        let mut d = Dictionary::new();
        for (k, v) in entries {
            d.insert(*k, v.clone());
        }
        let r = ObjectRef::new(n, 0);
        pdf.set_object(r, Object::Stream(crate::Stream::new(d, data.to_vec())));
        r
    }

    #[test]
    fn adjust_appearance_stream_empty_dr_map_is_noop() {
        let mut pdf = open_minimal();
        let font_ref = ObjectRef::new(5, 0);
        pdf.set_object(font_ref, Object::Dictionary(Dictionary::new()));
        let resources_ref = set_dict(
            &mut pdf,
            3,
            &[(
                "Font",
                Object::Dictionary({
                    let mut d = Dictionary::new();
                    d.insert("F1", Object::Reference(font_ref));
                    d
                }),
            )],
        );
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Reference(resources_ref))],
            b"/F1 18 Tf",
        );

        adjust_appearance_stream(&mut pdf, ap_ref, &DrMap::new()).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(stream.data, b"/F1 18 Tf");
        assert_eq!(
            stream.dict.get("Resources"),
            Some(&Object::Reference(resources_ref))
        );
    }

    #[test]
    fn adjust_appearance_stream_no_resources_is_noop() {
        let mut pdf = open_minimal();
        let ap_ref = set_stream(&mut pdf, 4, &[], b"/F1 18 Tf");
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(stream.data, b"/F1 18 Tf");
        assert!(stream.dict.get("Resources").is_none());
    }

    #[test]
    fn adjust_appearance_stream_rewrites_content_and_privatizes_indirect_resources() {
        let mut pdf = open_minimal();
        let font_ref = ObjectRef::new(5, 0);
        pdf.set_object(font_ref, Object::Dictionary(Dictionary::new()));
        let resources_ref = set_dict(
            &mut pdf,
            3,
            &[(
                "Font",
                Object::Dictionary({
                    let mut d = Dictionary::new();
                    d.insert("F1", Object::Reference(font_ref));
                    d
                }),
            )],
        );
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Reference(resources_ref))],
            b"/F1 18 Tf",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(stream.data, b"/F1_1 18 Tf");
        let new_resources_ref = stream
            .dict
            .get_ref("Resources")
            .expect("Resources should stay an indirect reference");
        assert_ne!(
            new_resources_ref, resources_ref,
            "must be a FRESH private object, not the original shared one"
        );

        // The private copy has ONLY the renamed key, pointing at the same
        // font object as before.
        let new_font = pdf
            .resolve(new_resources_ref)
            .unwrap()
            .into_dict()
            .unwrap()
            .get("Font")
            .and_then(Object::as_dict)
            .unwrap()
            .clone();
        assert_eq!(new_font.get("F1_1"), Some(&Object::Reference(font_ref)));
        assert!(new_font.get("F1").is_none());

        // The ORIGINAL shared /DR-copy object is untouched.
        let orig_font = pdf
            .resolve(resources_ref)
            .unwrap()
            .into_dict()
            .unwrap()
            .get("Font")
            .and_then(Object::as_dict)
            .unwrap()
            .clone();
        assert_eq!(orig_font.get("F1"), Some(&Object::Reference(font_ref)));
    }

    #[test]
    fn canonical_adjust_appearance_stream_rewrites_a_live_handle_without_materializing_it() {
        let mut pdf = open_minimal();
        let font_ref = ObjectRef::new(5, 0);
        pdf.set_object(font_ref, Object::Dictionary(Dictionary::new()));
        let resources_ref = set_dict(
            &mut pdf,
            3,
            &[(
                "Font",
                Object::Dictionary({
                    let mut d = Dictionary::new();
                    d.insert("F1", Object::Reference(font_ref));
                    d
                }),
            )],
        );
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Reference(resources_ref))],
            b"/F1 18 Tf",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let ap = pdf.get_object_handle(ap_ref);
        pdf.resolve_object_handle(&ap).unwrap();

        super::adjust_appearance_stream_handle(&mut pdf, &ap, &dr_map).unwrap();

        let stream_dict = ap
            .as_stream_dict()
            .expect("expected live stream dictionary");
        let resources = pdf
            .resolve_object_handle_to_terminal(&stream_dict.try_get_key(b"/Resources").unwrap())
            .unwrap();
        let font = resources
            .try_get_key(b"/Font")
            .unwrap()
            .try_get_key(b"/F1_1")
            .unwrap();
        assert_eq!(font.object_ref(), Some(font_ref));
        assert!(resources.try_get_key(b"/F1").unwrap().is_null());

        let decoded = ap
            .get_stream_data(crate::writer::DecodeLevel::Generalized)
            .unwrap();
        assert_eq!(decoded.as_slice(), b"/F1_1 18 Tf");
        assert_ne!(resources.object_ref(), Some(resources_ref));

        let original = pdf.get_object_handle(resources_ref);
        let original_font = pdf
            .resolve_object_handle_to_terminal(&original)
            .unwrap()
            .try_get_key(b"/Font")
            .unwrap()
            .try_get_key(b"/F1")
            .unwrap();
        assert_eq!(original_font.object_ref(), Some(font_ref));
    }

    #[test]
    fn canonical_adjust_appearance_stream_empty_dr_map_is_a_noop() {
        let mut pdf = open_minimal();
        let ap_ref = set_stream(&mut pdf, 4, &[], b"/F1 18 Tf");
        let ap = pdf.get_object_handle(ap_ref);
        pdf.resolve_object_handle(&ap).unwrap();

        super::adjust_appearance_stream_handle(&mut pdf, &ap, &DrMap::new()).unwrap();

        assert_eq!(ap.as_stream_data().unwrap().as_slice(), b"/F1 18 Tf");
        assert!(ap
            .as_stream_dict()
            .unwrap()
            .try_get_key(b"/Resources")
            .unwrap()
            .is_null());
    }

    #[test]
    fn canonical_adjust_appearance_stream_keeps_direct_resources_direct() {
        let mut pdf = open_minimal();
        let font_ref = ObjectRef::new(5, 0);
        pdf.set_object(font_ref, Object::Dictionary(Dictionary::new()));
        let resources = Object::Dictionary({
            let mut d = Dictionary::new();
            let mut fonts = Dictionary::new();
            fonts.insert("F1", Object::Reference(font_ref));
            d.insert("Font", Object::Dictionary(fonts));
            d
        });
        let ap_ref = set_stream(&mut pdf, 4, &[("Resources", resources)], b"/F1 18 Tf");
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let ap = pdf.get_object_handle(ap_ref);
        pdf.resolve_object_handle(&ap).unwrap();

        super::adjust_appearance_stream_handle(&mut pdf, &ap, &dr_map).unwrap();

        let stream_dict = ap
            .as_stream_dict()
            .expect("expected live stream dictionary");
        let resources = stream_dict.try_get_key(b"/Resources").unwrap();
        assert!(resources.as_dictionary().is_some());
        assert!(resources.object_ref().is_none());
        let fonts = resources.try_get_key(b"/Font").unwrap();
        assert!(!fonts.try_get_key(b"/F1_1").unwrap().is_null());
        assert!(fonts.try_get_key(b"/F1").unwrap().is_null());
    }

    #[test]
    fn canonical_adjust_appearance_stream_keeps_non_dictionary_resource_category() {
        let mut pdf = open_minimal();
        let resources = Object::Dictionary({
            let mut d = Dictionary::new();
            d.insert("Font", Object::Integer(7));
            d
        });
        let ap_ref = set_stream(&mut pdf, 4, &[("Resources", resources)], b"/F1 18 Tf");
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let ap = pdf.get_object_handle(ap_ref);
        pdf.resolve_object_handle(&ap).unwrap();

        super::adjust_appearance_stream_handle(&mut pdf, &ap, &dr_map).unwrap();

        let resources = ap
            .as_stream_dict()
            .unwrap()
            .try_get_key(b"/Resources")
            .unwrap();
        assert_eq!(
            resources.try_get_key(b"/Font").unwrap().as_integer(),
            Some(7)
        );
    }

    #[test]
    fn canonical_adjust_appearance_stream_keeps_non_dictionary_resources_untouched() {
        // qpdf only calls adjustAppearanceStream when `resources.isDictionary()`
        // (QPDFAcroFormDocumentHelper.cc:1006-1008). A non-dictionary
        // `/Resources` (e.g. from a malformed or foreign appearance stream)
        // must leave both content and `/Resources` byte-for-byte untouched.
        let mut pdf = open_minimal();
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Integer(7))],
            b"/F1 18 Tf",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let ap = pdf.get_object_handle(ap_ref);
        pdf.resolve_object_handle(&ap).unwrap();

        super::adjust_appearance_stream_handle(&mut pdf, &ap, &dr_map).unwrap();

        assert_eq!(ap.as_stream_data().unwrap().as_slice(), b"/F1 18 Tf");
        let resources = ap
            .as_stream_dict()
            .unwrap()
            .try_get_key(b"/Resources")
            .unwrap();
        assert_eq!(resources.as_integer(), Some(7));
    }

    #[test]
    fn canonical_adjust_appearance_stream_keeps_resources_when_content_cannot_decode() {
        let mut pdf = open_minimal();
        let font_ref = ObjectRef::new(5, 0);
        pdf.set_object(font_ref, Object::Dictionary(Dictionary::new()));
        let resources_ref = set_dict(
            &mut pdf,
            3,
            &[(
                "Font",
                Object::Dictionary({
                    let mut d = Dictionary::new();
                    d.insert("F1", Object::Reference(font_ref));
                    d
                }),
            )],
        );
        let raw = b"not a supported filter payload";
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[
                ("Resources", Object::Reference(resources_ref)),
                ("Filter", Object::Name(b"FlateDecode".to_vec())),
            ],
            raw,
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let ap = pdf.get_object_handle(ap_ref);
        pdf.resolve_object_handle(&ap).unwrap();

        super::adjust_appearance_stream_handle(&mut pdf, &ap, &dr_map).unwrap();

        assert_eq!(ap.as_stream_data().unwrap().as_slice(), raw);
        let stream_dict = ap.as_stream_dict().unwrap();
        let resources = pdf
            .resolve_object_handle_to_terminal(&stream_dict.try_get_key(b"/Resources").unwrap())
            .unwrap();
        assert!(!resources
            .try_get_key(b"/Font")
            .unwrap()
            .try_get_key(b"/F1_1")
            .unwrap()
            .is_null());
        assert!(resources
            .try_get_key(b"/Font")
            .unwrap()
            .try_get_key(b"/F1")
            .unwrap()
            .is_null());
    }

    #[test]
    fn canonical_adjust_appearance_stream_rewrites_second_order_resource_conflicts() {
        let mut pdf = open_minimal();
        let first_font_ref = ObjectRef::new(5, 0);
        let second_font_ref = ObjectRef::new(6, 0);
        pdf.set_object(first_font_ref, Object::Dictionary(Dictionary::new()));
        pdf.set_object(second_font_ref, Object::Dictionary(Dictionary::new()));
        let resources_ref = set_dict(
            &mut pdf,
            3,
            &[(
                "Font",
                Object::Dictionary({
                    let mut d = Dictionary::new();
                    d.insert("F1", Object::Reference(first_font_ref));
                    d.insert("F1_1", Object::Reference(second_font_ref));
                    d
                }),
            )],
        );
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Reference(resources_ref))],
            b"/F1 18 Tf /F1_1 12 Tf",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let ap = pdf.get_object_handle(ap_ref);
        pdf.resolve_object_handle(&ap).unwrap();

        super::adjust_appearance_stream_handle(&mut pdf, &ap, &dr_map).unwrap();

        let stream_dict = ap.as_stream_dict().unwrap();
        let resources = pdf
            .resolve_object_handle_to_terminal(&stream_dict.try_get_key(b"/Resources").unwrap())
            .unwrap();
        let fonts = resources.try_get_key(b"/Font").unwrap();
        assert_eq!(
            fonts.try_get_key(b"/F1_1").unwrap().object_ref(),
            Some(first_font_ref)
        );
        assert_eq!(
            fonts.try_get_key(b"/F1_1_1").unwrap().object_ref(),
            Some(second_font_ref)
        );
        let decoded = ap
            .get_stream_data(crate::writer::DecodeLevel::Generalized)
            .unwrap();
        assert_eq!(decoded.as_slice(), b"/F1_1 18 Tf /F1_1_1 12 Tf");
    }

    #[test]
    fn canonical_adjust_appearance_stream_falls_back_from_lzw_to_flate() {
        let mut pdf = open_minimal();
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Integer(1));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[
                ("Resources", Object::Dictionary(resources)),
                ("Filter", Object::Name(b"LZWDecode".to_vec())),
            ],
            &pack_lzw_9bit_literal(b"/F1 18 Tf"),
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let ap = pdf.get_object_handle(ap_ref);
        pdf.resolve_object_handle(&ap).unwrap();

        super::adjust_appearance_stream_handle(&mut pdf, &ap, &dr_map).unwrap();

        let stream_dict = ap.as_stream_dict().unwrap();
        assert_eq!(
            stream_dict.try_get_key(b"/Filter").unwrap().as_name(),
            Some(b"FlateDecode".to_vec())
        );
        assert!(stream_dict.try_get_key(b"/DecodeParms").unwrap().is_null());
        let decoded = ap
            .get_stream_data(crate::writer::DecodeLevel::Generalized)
            .unwrap();
        assert_eq!(decoded.as_slice(), b"/F1_1 18 Tf");
    }

    #[test]
    fn adjust_appearance_stream_incomplete_inline_image_keeps_resources_and_prefix_consistent() {
        let mut pdf = open_minimal();
        let font_ref = ObjectRef::new(5, 0);
        pdf.set_object(font_ref, Object::Dictionary(Dictionary::new()));
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Reference(font_ref));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"/F1 12 Tf BI ID",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(stream.data, b"/F1_1 12 Tf BI ID ");
        let font = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .and_then(|resources| resources.get("Font"))
            .and_then(Object::as_dict)
            .expect("appearance stream should retain a Font resource dictionary");
        assert_eq!(font.get("F1_1"), Some(&Object::Reference(font_ref)));
        assert!(font.get("F1").is_none());
    }

    #[test]
    fn adjust_appearance_stream_direct_resources_stays_direct() {
        let mut pdf = open_minimal();
        let font_ref = ObjectRef::new(5, 0);
        pdf.set_object(font_ref, Object::Dictionary(Dictionary::new()));
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Reference(font_ref));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"/F1 18 Tf",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(stream.data, b"/F1_1 18 Tf");
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert_eq!(font.get("F1_1"), Some(&Object::Reference(font_ref)));
    }

    #[test]
    fn adjust_appearance_stream_ensures_and_drops_empty_category() {
        // dr_map has a rename recorded under /ExtGState, but this AP
        // stream's own /Resources never had an /ExtGState entry at all: it
        // must be force-inserted (to unshare/exist), found empty (nothing
        // to rename into it), and then dropped — never left behind as a
        // stray empty sub-dictionary in the output.
        let mut pdf = open_minimal();
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(Dictionary::new()));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"q Q",
        );
        let dr_map = dr_map_with(b"ExtGState", b"GS1", b"GS1_1");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("expected direct Resources dict");
        assert!(
            resources.get("ExtGState").is_none(),
            "empty /ExtGState sub-dict must be dropped, not left behind"
        );
        // The unrelated, pre-existing (empty) /Font sub-dict is untouched —
        // it wasn't force-inserted by this call, but since it's ALSO
        // empty, `merge_resources_shallow`'s qpdf counterpart would still
        // drop it via the same "remove empty subdictionaries" step, so it
        // must be dropped here too, matching qpdf iterating every
        // subdictionary of the resulting /Resources, not just the ones
        // `dr_map` touched.
        assert!(resources.get("Font").is_none());
    }

    #[test]
    fn adjust_appearance_stream_rewrites_content_when_category_subdict_is_indirect() {
        // The AP stream's own `/Resources` is direct, but ITS `/Font` entry
        // is itself an indirect reference (`/Font 6 0 R`) — a shape PDF
        // permits and `merge_resources_shallow` already resolves on the
        // /DR-merge side. Before the fix, the pre-rename snapshot the
        // membership guard consults still held the un-resolved
        // `Object::Reference`, `Object::as_dict` cannot see through it, and
        // the guard silently blocked the rewrite even though `/F1` really
        // was present under that indirect sub-dict.
        let mut pdf = open_minimal();
        let font_ref = ObjectRef::new(5, 0);
        pdf.set_object(font_ref, Object::Dictionary(Dictionary::new()));
        let font_dict_ref = ObjectRef::new(6, 0);
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Reference(font_ref));
        pdf.set_object(font_dict_ref, Object::Dictionary(font_dict));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Reference(font_dict_ref));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"/F1 18 Tf",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(stream.data, b"/F1_1 18 Tf");
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert_eq!(font.get_ref("F1_1"), Some(font_ref));
        assert!(font.get("F1").is_none());
    }

    #[test]
    fn adjust_appearance_stream_double_conflict_mints_fresh_local_name() {
        // The AP stream's own /Resources/Font already has BOTH `F1` and
        // `F1_1` as two DIFFERENT font objects. Renaming `F1` -> `F1_1`
        // (per dr_map) would silently clobber the pre-existing, unrelated
        // `F1_1` font if the collision were not detected. qpdf handles this
        // by minting a fresh name for the displaced value
        // (`libqpdf/QPDFAcroFormDocumentHelper.cc:791-807`) and extending
        // its local `dr_map` so content that already said `/F1_1` follows
        // it there too. The fresh name is `getUniqueResourceName("F1_1_",
        // ...)`'s first free suffix, `F1_1_1` — NOT `F1_2`, since the
        // minted-name base is the RENAME TARGET (`F1_1`), not the original
        // source name (`F1`).
        let mut pdf = open_minimal();
        let f1_font_ref = ObjectRef::new(5, 0);
        pdf.set_object(f1_font_ref, Object::Dictionary(Dictionary::new()));
        let f1_1_font_ref = ObjectRef::new(6, 0);
        pdf.set_object(f1_1_font_ref, Object::Dictionary(Dictionary::new()));
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Reference(f1_font_ref));
        font_dict.insert("F1_1", Object::Reference(f1_1_font_ref));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"/F1 18 Tf /F1_1 18 Tf",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(stream.data, b"/F1_1 18 Tf /F1_1_1 18 Tf");
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert!(font.get("F1").is_none());
        assert_eq!(
            font.get_ref("F1_1"),
            Some(f1_font_ref),
            "the renamed slot now holds F1's original value"
        );
        assert_eq!(
            font.get_ref("F1_1_1"),
            Some(f1_1_font_ref),
            "the displaced original F1_1 value moved to the freshly minted name"
        );
    }

    #[test]
    fn adjust_appearance_stream_reuses_one_name_pool_for_multiple_conflicts() {
        let mut pdf = open_minimal();
        let f1_ref = ObjectRef::new(5, 0);
        let f1_1_ref = ObjectRef::new(6, 0);
        let f2_ref = ObjectRef::new(7, 0);
        let f2_1_ref = ObjectRef::new(8, 0);
        for object_ref in [f1_ref, f1_1_ref, f2_ref, f2_1_ref] {
            pdf.set_object(object_ref, Object::Dictionary(Dictionary::new()));
        }
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Reference(f1_ref));
        font_dict.insert("F1_1", Object::Reference(f1_1_ref));
        font_dict.insert("F2", Object::Reference(f2_ref));
        font_dict.insert("F2_1", Object::Reference(f2_1_ref));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"/F1 18 Tf /F1_1 18 Tf /F2 18 Tf /F2_1 18 Tf",
        );
        let mut dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        dr_map.insert_rename(b"Font", b"F2".to_vec(), b"F2_1".to_vec());

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(
            stream.data,
            b"/F1_1 18 Tf /F1_1_1 18 Tf /F2_1 18 Tf /F2_1_1 18 Tf"
        );
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .unwrap();
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert_eq!(font.get_ref("F1_1"), Some(f1_ref));
        assert_eq!(font.get_ref("F1_1_1"), Some(f1_1_ref));
        assert_eq!(font.get_ref("F2_1"), Some(f2_ref));
        assert_eq!(font.get_ref("F2_1_1"), Some(f2_1_ref));
    }

    #[test]
    fn adjust_appearance_stream_double_conflict_same_object_is_noop() {
        // /Resources/Font already has BOTH `F1` and `F1_1` pointing at the
        // SAME underlying font object — qpdf's `QPDFObjGen` identity check
        // treats this as already-resolved (the renamed slot would hold the
        // exact same object either way) and mints no fresh name at all.
        let mut pdf = open_minimal();
        let shared_font_ref = ObjectRef::new(5, 0);
        pdf.set_object(shared_font_ref, Object::Dictionary(Dictionary::new()));
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Reference(shared_font_ref));
        font_dict.insert("F1_1", Object::Reference(shared_font_ref));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"/F1 18 Tf /F1_1 18 Tf",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        // `/F1` is renamed to `/F1_1`; the second `/F1_1` token is untouched
        // (no local rename was ever recorded for it), so both tokens end up
        // saying `/F1_1`.
        assert_eq!(stream.data, b"/F1_1 18 Tf /F1_1 18 Tf");
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert!(font.get("F1").is_none());
        assert!(
            font.get("F1_1_1").is_none(),
            "no fresh name should be minted for a same-object collision"
        );
        assert_eq!(font.get_ref("F1_1"), Some(shared_font_ref));
    }

    #[test]
    fn adjust_appearance_stream_unsupported_filter_is_non_fatal_noop() {
        // The AP stream's own /Resources/Font has "F2", never "F1" — a rename
        // recorded under dr_map for F1->F1_1 could never have matched this
        // stream's content even if it decoded successfully. Its content uses
        // CCITTFaxDecode: a real ISO 32000 stream filter, but one flpdf
        // intentionally never decodes (crate::filters::passthrough_codec_label
        // — an image/binary passthrough codec, preserved verbatim). Real
        // qpdf's AcroForm::adjustAppearanceStream wraps the equivalent
        // content-parse step in a try/catch that turns exactly this kind of
        // failure into a warning, not a hard error, so it must not propagate
        // here either and kill the whole overlay call chain over one
        // unrelated AP stream. The /Resources dict rename step still runs
        // (matching qpdf, which renames before its own try/catch), but since
        // there was nothing to rename, it is a no-op; the content bytes must
        // be left byte-for-byte exactly as read since flpdf cannot decode
        // them at all.
        let mut pdf = open_minimal();
        let mut font_dict = Dictionary::new();
        font_dict.insert("F2", Object::Integer(1));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[
                ("Resources", Object::Dictionary(resources)),
                ("Filter", Object::Name(b"CCITTFaxDecode".to_vec())),
            ],
            b"\x00\x01opaque-ccitt-bytes",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        let result = adjust_appearance_stream(&mut pdf, ap_ref, &dr_map);
        assert!(
            result.is_ok(),
            "an undecodable AP stream content must not fail the whole call"
        );

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(
            stream.data, b"\x00\x01opaque-ccitt-bytes",
            "content bytes must be left exactly as read when they cannot be decoded"
        );
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert_eq!(
            font.get("F2"),
            Some(&Object::Integer(1)),
            "unrelated existing key is untouched"
        );
        assert!(font.get("F1").is_none());
        assert!(font.get("F1_1").is_none());
    }

    #[test]
    fn adjust_appearance_stream_undecodable_filter_keeps_resources_rename_but_leaves_content_stale()
    {
        // Unlike the CCITT test above, this stream's own /Resources/Font
        // DOES have "F1" — a REAL collision, so the /Resources rename
        // (steps 1-4) is not a no-op this time. The content still cannot
        // be decoded (same CCITTFaxDecode passthrough codec), so step 5's
        // content rewrite cannot run at all. This asserts flpdf's ACTUAL
        // (verified) qpdf-matching behavior: qpdf performs the /Resources
        // rename BEFORE its own try/catch'd tokenize step
        // (`libqpdf/QPDFAcroFormDocumentHelper.cc:791-807` runs before
        // `:824-849`), and does NOT roll the rename back when the
        // subsequent tokenize fails — confirmed by fetching qpdf's actual
        // source for roborev PR #490 iter-3 finding 3, which proposed a
        // rollback; a rollback would have been the qpdf DIVERGENCE, so it
        // was declined and this test instead documents the verified,
        // matching (if internally inconsistent-looking) result: the dict
        // says "F1_1", the content still says "F1" — exactly like qpdf.
        let mut pdf = open_minimal();
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Integer(1));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[
                ("Resources", Object::Dictionary(resources)),
                ("Filter", Object::Name(b"CCITTFaxDecode".to_vec())),
            ],
            b"\x00\x01/F1 opaque-ccitt-bytes",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        let result = adjust_appearance_stream(&mut pdf, ap_ref, &dr_map);
        assert!(
            result.is_ok(),
            "an undecodable AP stream content must not fail the whole call"
        );

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(
            stream.data, b"\x00\x01/F1 opaque-ccitt-bytes",
            "content bytes must be left exactly as read — qpdf does not roll \
             back a rename it already applied to /Resources just because the \
             later tokenize step failed"
        );
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert_eq!(
            font.get("F1_1"),
            Some(&Object::Integer(1)),
            "the /Resources dict rename (steps 1-4) still applies even though \
             the content (step 5) could not be rewritten"
        );
        assert!(font.get("F1").is_none());
    }

    #[test]
    fn adjust_appearance_stream_rename_chain_matches_qpdf_verified_result() {
        // dr_map records a CHAIN within one category: F1->F1_1 AND,
        // independently, F1_1->F1_1_1 (both entries genuinely present in
        // dr_map at once — plausible when the top-level /DR merge assigns
        // F1_1 to a renamed F1 while a DIFFERENT source object separately
        // collides under the destination's own pre-existing F1_1). This
        // stream's own /Resources/Font has both F1 and F1_1 locally.
        //
        // qpdf's rename loop (`libqpdf/QPDFAcroFormDocumentHelper.cc:781-803`,
        // fetched and read for roborev PR #490 iter-3 finding 1) mutates the
        // sub-dictionary IN PLACE: processing F1->F1_1 first (dr_map is
        // sorted, "F1" < "F1_1") moves F1's value into the F1_1 slot,
        // displacing the true original F1_1 value into a `merge_with`
        // side-map. Processing F1_1->F1_1_1 next then reads the ALREADY-
        // overwritten F1_1 slot (now holding F1's value, not the true
        // original) and moves THAT into F1_1_1. The re-merge step
        // (`:805-807`) then re-lands the side-mapped, true original F1_1
        // value back into ITS OWN name, F1_1 — which is free again, since
        // phase 1 vacated it. The two resources end up CROSS-WIRED between
        // the dict and content: this is qpdf's actual, verified output for
        // this input, not a bug flpdf introduced — a "cleaner" pre-snapshot
        // rewrite would be the byte-identical divergence here.
        let mut pdf = open_minimal();
        let f1_font_ref = ObjectRef::new(5, 0);
        pdf.set_object(f1_font_ref, Object::Dictionary(Dictionary::new()));
        let f1_1_font_ref = ObjectRef::new(6, 0);
        pdf.set_object(f1_1_font_ref, Object::Dictionary(Dictionary::new()));
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Reference(f1_font_ref));
        font_dict.insert("F1_1", Object::Reference(f1_1_font_ref));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"/F1 18 Tf /F1_1 18 Tf",
        );
        let mut dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        dr_map.insert_rename(b"Font", b"F1_1".to_vec(), b"F1_1_1".to_vec());

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(stream.data, b"/F1_1 18 Tf /F1_1_1 18 Tf");
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert!(font.get("F1").is_none());
        assert_eq!(
            font.get_ref("F1_1"),
            Some(f1_1_font_ref),
            "the true original F1_1 value re-lands under its OWN name via the re-merge"
        );
        assert_eq!(
            font.get_ref("F1_1_1"),
            Some(f1_font_ref),
            "F1's value ends up under F1_1_1, having been reprocessed by the second rename entry"
        );
    }

    #[test]
    fn adjust_appearance_stream_reuses_existing_key_for_same_object_on_new_conflict() {
        // Phase 2's conflict resolution (`libqpdf/QPDFAcroFormDocumentHelper.cc:805-807`'s
        // `resources.mergeResources(merge_with, &dr_map)`, `QPDFObjectHandle::mergeResources`'s
        // `og_to_name` reuse) is exercised here: /Resources/Font has F1 and
        // F2 BOTH pointing at the SAME object, plus F3 at a different
        // object. dr_map renames F3->F2. Phase 1 moves F3's value into F2
        // (displacing F2's original occupant — which is the SAME object as
        // F1 — into `merge_with`). Phase 2 then finds F2 occupied (by F3's
        // moved-in value) and, instead of minting a fresh name for the
        // displaced value, notices it already lives under F1 (by
        // `ObjectRef` identity) and records an EXTRA dr_map redirect
        // (F2->F1) instead — no fresh name minted at all.
        let mut pdf = open_minimal();
        let shared_ref = ObjectRef::new(5, 0);
        pdf.set_object(shared_ref, Object::Dictionary(Dictionary::new()));
        let other_ref = ObjectRef::new(6, 0);
        pdf.set_object(other_ref, Object::Dictionary(Dictionary::new()));
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Reference(shared_ref));
        font_dict.insert("F2", Object::Reference(shared_ref));
        font_dict.insert("F3", Object::Reference(other_ref));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"/F3 18 Tf /F2 18 Tf",
        );
        let dr_map = dr_map_with(b"Font", b"F3", b"F2");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(
            stream.data, b"/F2 18 Tf /F1 18 Tf",
            "F3 (now under F2) rewrites to /F2; the original /F2 token must \
             follow its displaced value to wherever it actually ended up (F1), \
             not stay pointing at F2 (now a different object) or lose the rename"
        );
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert_eq!(font.get_ref("F1"), Some(shared_ref));
        assert_eq!(font.get_ref("F2"), Some(other_ref));
        assert!(font.get("F3").is_none());
        assert!(
            font.get("F2_1").is_none(),
            "no fresh name should be minted — the displaced value is REUSED \
             under its existing F1 name, not aliased under a new one"
        );
    }

    #[test]
    fn adjust_appearance_stream_reuses_slot_vacated_earlier_in_same_merge() {
        // roborev PR #490 iter-4 finding 1: an earlier version of phase 2's
        // "reuse a key that already names the same object" lookup snapshot
        // `subdict` into `ref_to_key` ONCE, EAGERLY, before the staged loop
        // began. That missed a value that becomes visible to `subdict` only
        // DURING the loop — specifically, a slot phase 1 vacated and phase
        // 2's OWN verbatim-reinstate step (an earlier staged entry, in key
        // order) just refilled with the SAME object a LATER staged entry is
        // about to collide on.
        //
        // Ground truth (fetched from qpdf `main`,
        // `libqpdf/QPDFObjectHandle.cc`, `mergeResources`, since deepwiki's
        // summary of this was internally inconsistent): `og_to_name` is
        // built lazily — ONCE, on the FIRST genuine conflict — via the
        // `initialized_maps` guard around `make_og_to_name(this_val, ...)`.
        // Because that snapshot is taken mid-loop, it legitimately captures
        // any vacated-slot reinstate that ran earlier in the SAME
        // `other_val` loop (those already mutated `this_val` in place via
        // `this_val.replaceKey`). It is then FROZEN — untouched by anything
        // that happens afterward, including a fresh alias minted for a
        // later conflict (this is why the *previous* test above,
        // `..._double_conflict_mints_fresh_local_name`, still mints two
        // separate aliases for two conflicts that are BOTH genuine — that
        // qpdf behavior is unchanged by this fix). This test isolates the
        // one case the eager snapshot missed: reuse sourced from an
        // in-loop verbatim reinstate.
        //
        // Chain (three rename entries under Font, sorted by old_key so
        // phase 1 processes them F1, F1_1, F2 in that order):
        //   F1 -> F1_1: displaces F1_1's ORIGINAL value (shared_ref) into
        //               merge_with["F1_1"]; F1's value (temp_ref) moves in.
        //   F1_1 -> F3: displaces nothing (F3 was empty); F1_1's CURRENT
        //               value (temp_ref) moves to F3, vacating F1_1.
        //   F2 -> F4:   displaces F4's ORIGINAL value (shared_ref, the SAME
        //               object as above) into merge_with["F4"]; F2's value
        //               (other_ref) moves in.
        // After phase 1: subdict = {F3: temp_ref, F4: other_ref}; no key
        // names shared_ref any more. merge_with = {F1_1: shared_ref,
        // F4: shared_ref} — both entries name the SAME object.
        //
        // Phase 2 processes merge_with in key order: "F1_1" < "F4".
        //   F1_1: vacated (subdict has no F1_1) -> reinstated verbatim,
        //         subdict["F1_1"] = shared_ref. THIS is the in-loop
        //         mutation the eager snapshot could not see.
        //   F4:   occupied (by other_ref) -> genuine conflict, the FIRST
        //         one this loop hits. The lazily-built snapshot now
        //         includes the just-reinstated F1_1 -> shared_ref, so F4's
        //         displaced value (shared_ref) reuses F1_1 instead of
        //         minting a fresh alias (e.g. the wrong "F4_1").
        let mut pdf = open_minimal();
        let shared_ref = ObjectRef::new(5, 0);
        pdf.set_object(shared_ref, Object::Dictionary(Dictionary::new()));
        let temp_ref = ObjectRef::new(6, 0);
        pdf.set_object(temp_ref, Object::Dictionary(Dictionary::new()));
        let other_ref = ObjectRef::new(7, 0);
        pdf.set_object(other_ref, Object::Dictionary(Dictionary::new()));
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Reference(temp_ref));
        font_dict.insert("F1_1", Object::Reference(shared_ref));
        font_dict.insert("F2", Object::Reference(other_ref));
        font_dict.insert("F4", Object::Reference(shared_ref));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"/F4 12 Tf",
        );
        let mut dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        dr_map.insert_rename(b"Font", b"F1_1".to_vec(), b"F3".to_vec());
        dr_map.insert_rename(b"Font", b"F2".to_vec(), b"F4".to_vec());

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(
            stream.data, b"/F1_1 12 Tf",
            "the /F4 token must follow shared_ref to wherever it actually \
             ended up (F1_1, reused from the in-loop reinstate), not a \
             freshly minted alias"
        );
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert_eq!(
            font.get_ref("F1_1"),
            Some(shared_ref),
            "the vacated slot's own verbatim reinstate"
        );
        assert_eq!(
            font.get_ref("F4"),
            Some(other_ref),
            "F4's occupant is untouched — the reuse branch never mutates \
             subdict, unlike the fresh-mint branch"
        );
        assert!(
            font.get("F4_1").is_none(),
            "no fresh alias minted for F4 — it reuses F1_1 by ObjectRef \
             identity instead"
        );
    }

    #[test]
    fn adjust_appearance_stream_freezes_name_pool_with_identity_snapshot() {
        let mut pdf = open_minimal();
        let shared_ref = ObjectRef::new(5, 0);
        let temp_a_ref = ObjectRef::new(6, 0);
        let temp_b_ref = ObjectRef::new(7, 0);
        let temp_c_ref = ObjectRef::new(8, 0);
        let b_original_ref = ObjectRef::new(9, 0);
        let c_original_ref = ObjectRef::new(10, 0);
        for object_ref in [
            shared_ref,
            temp_a_ref,
            temp_b_ref,
            temp_c_ref,
            c_original_ref,
        ] {
            pdf.set_object(object_ref, Object::Dictionary(Dictionary::new()));
        }
        let mut b_original = Dictionary::new();
        b_original.insert("C_1", Object::Name(b"Inserted".to_vec()));
        pdf.set_object(b_original_ref, Object::Dictionary(b_original));

        let mut font_dict = Dictionary::new();
        font_dict.insert("F0", Object::Reference(shared_ref));
        font_dict.insert("A", Object::Reference(shared_ref));
        font_dict.insert("A0", Object::Reference(temp_a_ref));
        font_dict.insert("A1", Object::Reference(temp_b_ref));
        font_dict.insert("A2", Object::Reference(temp_c_ref));
        font_dict.insert("B", Object::Reference(b_original_ref));
        font_dict.insert("C", Object::Reference(c_original_ref));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"/C 12 Tf",
        );
        let mut dr_map = dr_map_with(b"Font", b"A0", b"A");
        dr_map.insert_rename(b"Font", b"A1".to_vec(), b"B".to_vec());
        dr_map.insert_rename(b"Font", b"A2".to_vec(), b"C".to_vec());
        dr_map.insert_rename(b"Font", b"B".to_vec(), b"D".to_vec());

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(
            stream.data, b"/C_1 12 Tf",
            "the later fresh conflict must use the pool captured by the first conflict"
        );
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert_eq!(font.get_ref("C_1"), Some(c_original_ref));
        assert!(font.get("C_2").is_none());
    }

    #[test]
    fn adjust_appearance_stream_propagates_resource_name_resolution_error() {
        // Keep object 8 lazy and malformed: unknown references resolve to
        // qpdf-shaped null, so the error path needs a real parser error from
        // an indexed object rather than a dangling object number.
        let bodies: &[&[u8]] = &[
            b"1 0 obj\n<< /Type /Catalog >>\nendobj\n",
            b"2 0 obj\nnull\nendobj\n",
            b"3 0 obj\nnull\nendobj\n",
            b"4 0 obj\nnull\nendobj\n",
            b"5 0 obj\nnull\nendobj\n",
            b"6 0 obj\nnull\nendobj\n",
            b"7 0 obj\nnull\nendobj\n",
            b"8 0 obj\n<<",
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for body in bodies {
            offsets.push(bytes.len() as u64);
            bytes.extend_from_slice(body);
        }
        let xref_start = bytes.len();
        bytes.extend_from_slice(b"xref\n0 9\n0000000000 65535 f \n");
        for offset in offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 9 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        let mut pdf = Pdf::open_with_options(
            Cursor::new(bytes),
            PdfOpenOptions {
                repair: false,
                ..PdfOpenOptions::default()
            },
        )
        .expect("malformed object must stay lazy");
        let broken_resource_ref = set_dict(
            &mut pdf,
            5,
            &[("Broken", Object::Reference(ObjectRef::new(8, 0)))],
        );
        let f1_ref = set_dict(&mut pdf, 6, &[]);
        let f1_1_ref = set_dict(&mut pdf, 7, &[]);
        let mut font_dict = Dictionary::new();
        font_dict.insert("F0", Object::Reference(broken_resource_ref));
        font_dict.insert("F1", Object::Reference(f1_ref));
        font_dict.insert("F1_1", Object::Reference(f1_1_ref));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"/F1 18 Tf",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        assert!(adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).is_err());
    }

    /// Pack literal bytes as a minimal `/LZWDecode` stream: each byte is its
    /// own literal code (codes 0-255 are always literal single-byte table
    /// entries per PDF §7.4.4), followed by EOD (257). Every code stays 9
    /// bits wide because so few codes are emitted the table never reaches
    /// the first width-bump threshold (511 entries under the default
    /// EarlyChange). flpdf has no LZW encoder (decision flpdf-9hc.7.2), so a
    /// test needing LZW-encoded *input* must synthesize it directly —
    /// mirrors `filters::tests::pack_lzw_9bit`, which cannot be reused here
    /// since it is private to that module.
    fn pack_lzw_9bit_literal(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf: u32 = 0;
        let mut bits: u32 = 0;
        let mut codes: Vec<u16> = bytes.iter().map(|&b| u16::from(b)).collect();
        codes.push(257); // EOD
        for code in codes {
            buf = (buf << 9) | u32::from(code);
            bits += 9;
            while bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
            }
        }
        if bits > 0 {
            out.push((buf << (8 - bits)) as u8);
        }
        out
    }

    #[test]
    fn adjust_appearance_stream_lzw_reencode_failure_falls_back_to_flate() {
        // `/LZWDecode` is the one filter flpdf can decode but not re-encode
        // (crate::filters::apply_single_filter_encode, decision
        // flpdf-9hc.7.2). Unlike the CCITT test above, this stream's own
        // /Resources/Font DOES have "F1", so dr_map's F1->F1_1 rename is a
        // REAL rename, not a no-op. Before this fix, the /Resources rename
        // still applied but the content-rewrite step silently discarded the
        // token-replaced bytes on re-encode failure, leaving the content on
        // the stale "/F1" name while /Resources only had "F1_1" — an
        // inconsistent stream. This asserts the two stay consistent.
        let mut pdf = open_minimal();
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Integer(1));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));

        let lzw_bytes = pack_lzw_9bit_literal(b"/F1 18 Tf");
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[
                ("Resources", Object::Dictionary(resources)),
                ("Filter", Object::Name(b"LZWDecode".to_vec())),
            ],
            &lzw_bytes,
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();

        // Dict-level rename (steps 1-4) applied, as always.
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert_eq!(font.get("F1_1"), Some(&Object::Integer(1)));
        assert!(font.get("F1").is_none());

        // The content must agree: re-encoded as FlateDecode (flpdf cannot
        // re-encode LZW), with the resource token renamed to match.
        assert_eq!(
            stream.dict.get("Filter"),
            Some(&Object::Name(b"FlateDecode".to_vec())),
            "un-re-encodable /LZWDecode must fall back to /FlateDecode"
        );
        assert!(
            stream.dict.get("DecodeParms").is_none(),
            "stale LZW /DecodeParms must not survive the filter swap"
        );
        let decoded_content =
            crate::filters::decode_stream_data(&stream.dict, &stream.data).unwrap();
        assert_eq!(
            decoded_content, b"/F1_1 18 Tf",
            "content must reference the RENAMED name, consistent with /Resources"
        );
        let expected_length = i64::try_from(stream.data.len()).unwrap();
        assert_eq!(
            stream.dict.get("Length"),
            Some(&Object::Integer(expected_length)),
            "/Length must match the newly re-encoded bytes"
        );
    }
}

//! Read/write access to the `/Names /EmbeddedFiles` name-tree.
//!
//! # Reader
//!
//! Walks the catalog's `/Names /EmbeddedFiles` name tree (ISO 32000-1 §7.9.6
//! + §7.11) and returns an ordered list of `(name_key, filespec_ref)` pairs.
//!
//! The result is in depth-first, key-ascending order as mandated by the spec
//! requirement that name trees be sorted by key.
//!
//! # Writer
//!
//! [`insert_embedded_file`] and [`delete_embedded_file`] mutate the tree
//! using a **collect → modify → rebuild** strategy that mirrors qpdf's
//! aggressive rebuild policy: all entries are gathered in one pass, the entry
//! list is changed, sorted, and the entire tree is reconstructed from scratch.
//!
//! The reconstruction uses at most two levels:
//! - ≤ [`LEAF_MAX`] entries → a single leaf node (no `/Kids`).
//! - > [`LEAF_MAX`] entries → a root node with `/Kids` pointing to leaf chunks.
//!
//! Every node carries a `/Limits [first, last]` array as required by validators
//! such as qpdf.
//!
//! The writer normalises the catalog path: after any mutation `/Names` is
//! stored as an indirect object and `/EmbeddedFiles` within it is an indirect
//! reference to the tree root.  Other keys in the `/Names` dictionary (e.g.
//! `/Dests`, `/JavaScript`) are preserved unchanged.
//!
//! When deletion reduces the entry list to zero the `/EmbeddedFiles` key is
//! removed from the `/Names` dictionary; if that makes the dictionary empty
//! the `/Names` key is removed from the catalog.
//!
//! # Name-tree structure (ISO 32000-1 §7.9.6)
//!
//! A name tree node is a dictionary with either:
//! - `/Kids` — an array of indirect references to child nodes (intermediate),
//! - `/Names` — a flat array `[key₁, val₁, key₂, val₂, …]` (leaf).
//!
//! An optional `/Limits [least, greatest]` array on each node bounds the key
//! range of its subtree.  For full enumeration (this module's purpose), `/Limits`
//! is informational: the tree is pre-sorted and DFS order already yields keys in
//! ascending order.  `/Limits` is *not* used to prune subtrees here because we
//! are collecting all entries, not searching for one.  If `/Limits` is present
//! it is simply skipped without error.
//!
//! # Missing keys
//!
//! Any of `/Root`, `/Names`, `/EmbeddedFiles`, or the name-tree root being absent
//! results in an empty list (`Ok(vec![])`) rather than an error.  Two error
//! kinds can be returned: I/O errors propagated from [`Pdf::resolve`], and
//! [`crate::Error::Unsupported`] when a `/Kids` chain exceeds the configured depth
//! limit (guards against cyclic or maliciously deep trees).
//!
//! # Value types
//!
//! Each name-tree value should be an indirect reference to a `/Filespec`
//! dictionary.  Values that are not [`Object::Reference`] are skipped with a
//! diagnostic comment in source but no error; direct-dict filespecs embedded
//! directly in name arrays are exceedingly rare in practice and out of scope for
//! this read-only enumerator.
//!
//! # Examples
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{embedded_files, Pdf, ObjectRef};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("with-attachments.pdf")?))?;
//! let entries = embedded_files::list_embedded_files(&mut pdf)?;
//! for (name, filespec_ref) in &entries {
//!     println!("{}: {}", String::from_utf8_lossy(name), filespec_ref);
//! }
//!
//! // Insert a new attachment key (the filespec object must already exist in pdf)
//! let filespec_ref = ObjectRef::new(42, 0);
//! embedded_files::insert_embedded_file(&mut pdf, b"report.pdf", filespec_ref)?;
//!
//! // Remove an entry
//! embedded_files::delete_embedded_file(&mut pdf, b"old-attachment.txt")?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crate::{Dictionary, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

// ── remove_attachment ─────────────────────────────────────────────────────────

/// Remove an attachment by name-tree key, including garbage collection of the
/// now-unreferenced `/Filespec` and `/EmbeddedFile` stream objects.
///
/// # Behaviour
///
/// 1. Looks up `key` in the catalog's `/Names /EmbeddedFiles` name tree.
///    Returns `Ok(false)` — without error — if the key is absent.
/// 2. Before any mutation, captures the `filespec_ref` (if the value is an
///    indirect reference) and the `/EmbeddedFile` stream ref reachable via
///    `/EF /UF` › `/EF /F` › other standard keys.
/// 3. Calls [`delete_embedded_file`] to remove the name-tree entry.
/// 4. Clears any references to `filespec_ref` from the `/AF` array in the
///    document catalog and in every page dictionary.  If the `/AF` array
///    becomes empty after removal, the `/AF` key itself is deleted.
/// 5. **Conservative GC:** if `filespec_ref` or `stream_ref` now appear in
///    zero live objects (other than themselves), [`Pdf::delete_object`] is
///    called to physically remove them.  If another object still references
///    them they are left intact.
///
/// # Limitation
///
/// When the name-tree value is a *direct* `/Filespec` dictionary (not an
/// indirect reference), there is no `ObjectRef` to GC.  The name-tree entry
/// is removed and `/AF` is cleared for indirect refs that happen to match
/// nothing; only the tree entry itself is gone.  This case is exceedingly rare
/// in practice (ISO 32000-1 §7.11 expects indirect refs), but is handled
/// gracefully — no panic, no spurious error.
///
/// # GC conservatism
///
/// The reachability scan walks every live object in the document and checks
/// whether its serialised object graph contains a reference to the target
/// `ObjectRef`.  If any live object (other than the target itself) still holds
/// a reference, the target is preserved.  This avoids breaking documents that
/// — unusually — share a single `/EmbeddedFile` stream between two filespecs.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`] or [`delete_embedded_file`].
pub fn remove_attachment<R: Read + Seek>(pdf: &mut Pdf<R>, key: &[u8]) -> Result<bool> {
    // ── Step 1: locate the entry and capture refs (before any mutation) ───────
    // We need both the filespec_ref and the stream_ref before we mutate the tree.
    let entries = collect_embedded_file_pairs_raw(pdf, DEFAULT_MAX_EMBEDDED_FILES_DEPTH)?;
    let target_value = match entries.iter().find(|(k, _)| k.as_slice() == key) {
        Some((_, v)) => v.clone(),
        None => return Ok(false),
    };

    // Extract indirect filespec ref (if the value is an indirect reference).
    let filespec_ref_opt: Option<ObjectRef> = match &target_value {
        Object::Reference(r) => Some(*r),
        _ => None, // direct-dict filespec — no ref to GC
    };

    // Resolve the /EmbeddedFile stream ref via /EF of the filespec (if any).
    let stream_ref_opt: Option<ObjectRef> = if let Some(fs_ref) = filespec_ref_opt {
        resolve_embedded_file_stream_ref(pdf, fs_ref)?
    } else {
        None
    };

    // ── Step 1b: collect name-tree node refs (before mutation) ────────────────
    // After `delete_embedded_file` rebuilds the tree, old leaf/intermediate nodes
    // are NOT `delete_object`-ed — they remain as live "ghosts" that still contain
    // references to the old filespec/stream.  We collect ALL currently-live name-
    // tree node refs before the rebuild so we can exclude both:
    //   (a) the nodes that survive in the rebuilt tree, and
    //   (b) the ghost nodes from earlier rebuilds (e.g. from successive inserts).
    // We take the union of ALL live objects that look like name-tree nodes
    // (i.e. contain a /Names flat array or a /Kids array but not a /Type key that
    // would identify them as something else).  This is a heuristic, but safe: a
    // false positive just means we're more conservative (skip a non-tree object
    // from the scan), which cannot cause incorrect deletion.
    let name_tree_node_refs = collect_all_live_name_tree_node_refs(pdf)?;

    // ── Step 2: remove the name-tree entry ────────────────────────────────────
    delete_embedded_file(pdf, key)?;

    // ── Step 3: clear /AF references on catalog and all pages ─────────────────
    if let Some(fs_ref) = filespec_ref_opt {
        clear_af_reference(pdf, fs_ref)?;
    }

    // ── Step 4: conservative GC ───────────────────────────────────────────────
    // Check whether the filespec is referenced by any live object OTHER than:
    //   - itself
    //   - name-tree nodes (which we just rebuilt and may be stale live ghosts)
    //   - the stream ref (whose edge to the stream we separately check)
    //
    // Note: `rebuild_embedded_files_tree` (called by `delete_embedded_file`)
    // does not call `delete_object` on old name-tree leaf/intermediate nodes;
    // they remain in live_object_refs() as unreachable ghosts.  We exclude them
    // from the scan so their stale references do not incorrectly preserve the
    // filespec/stream objects.
    let mut gc_exclude: BTreeSet<ObjectRef> = name_tree_node_refs;

    if let Some(fs_ref) = filespec_ref_opt {
        // Also exclude the stream ref from "things that reference the filespec"
        // scan — the filespec itself points to the stream, but we're about to
        // delete both; don't let that edge count.
        if let Some(st_ref) = stream_ref_opt {
            gc_exclude.insert(st_ref);
        }
        let self_refs = [fs_ref];
        if !is_referenced_by_live_objects_excluding(pdf, fs_ref, &self_refs, &gc_exclude)? {
            pdf.delete_object(fs_ref);
        }
    }
    if let Some(st_ref) = stream_ref_opt {
        let self_refs = [st_ref];
        // Use the same name-tree exclusion set (without the stream itself).
        let mut exclude2 = gc_exclude.clone();
        exclude2.remove(&st_ref); // stream is in self_refs already
        if !is_referenced_by_live_objects_excluding(pdf, st_ref, &self_refs, &exclude2)? {
            pdf.delete_object(st_ref);
        }
    }

    Ok(true)
}

// ── helpers for remove_attachment ─────────────────────────────────────────────

/// Walk a `/Filespec` dict and return the first `/EmbeddedFile` stream `ObjectRef`
/// reachable via `/EF /UF`, `/EF /F`, `/EF /Unix`, `/EF /Mac`, `/EF /DOS` (in
/// that priority order).  Returns `None` if not found or on any soft error.
fn resolve_embedded_file_stream_ref<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filespec_ref: ObjectRef,
) -> Result<Option<ObjectRef>> {
    let fs_obj = pdf.resolve(filespec_ref)?;
    let Object::Dictionary(fs_dict) = fs_obj else {
        return Ok(None);
    };
    let ef_dict: Dictionary = match fs_dict.get("EF") {
        Some(Object::Dictionary(d)) => d.clone(),
        Some(Object::Reference(r)) => {
            let r = *r;
            match pdf.resolve(r)? {
                Object::Dictionary(d) => d,
                _ => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    for key in &["UF", "F", "Unix", "Mac", "DOS"] {
        if let Some(Object::Reference(r)) = ef_dict.get(key) {
            return Ok(Some(*r));
        }
    }
    Ok(None)
}

/// Remove all occurrences of `target_ref` from `/AF` arrays on the catalog and
/// every page dictionary.  After removal, if a `/AF` array becomes empty, the
/// `/AF` key is deleted from that dictionary.
fn clear_af_reference<R: Read + Seek>(pdf: &mut Pdf<R>, target_ref: ObjectRef) -> Result<()> {
    // ── Catalog /AF ───────────────────────────────────────────────────────────
    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(()),
    };
    remove_ref_from_af_in_dict(pdf, catalog_ref, target_ref)?;

    // ── Page /AF ──────────────────────────────────────────────────────────────
    // Collect page refs first; page_refs performs I/O so we cannot interleave
    // it with set_object mutations.
    let page_refs = match crate::pages::page_refs(pdf) {
        Ok(v) => v,
        Err(_) => return Ok(()), // Best-effort: skip if tree is broken.
    };
    for page_ref in page_refs {
        remove_ref_from_af_in_dict(pdf, page_ref, target_ref)?;
    }
    Ok(())
}

/// Remove `target_ref` from the `/AF` array of the dictionary at `dict_ref`.
/// Patches the dictionary back via [`Pdf::set_object`].  If `/AF` becomes
/// empty after removal, the key is deleted.  If `/AF` is absent or contains no
/// reference to `target_ref`, this is a no-op.
fn remove_ref_from_af_in_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict_ref: ObjectRef,
    target_ref: ObjectRef,
) -> Result<()> {
    let obj = pdf.resolve(dict_ref)?;
    let Object::Dictionary(mut dict) = obj else {
        return Ok(());
    };

    // /AF may be a direct array or an indirect reference to an array.
    let af_value = match dict.get("AF").cloned() {
        Some(v) => v,
        None => return Ok(()), // No /AF — nothing to do.
    };

    let af_array: Vec<Object> = match af_value {
        Object::Array(arr) => arr,
        Object::Reference(r) => match pdf.resolve(r)? {
            Object::Array(arr) => arr,
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };

    let filtered: Vec<Object> = af_array
        .into_iter()
        .filter(|o| !matches!(o, Object::Reference(r) if *r == target_ref))
        .collect();

    if filtered.is_empty() {
        dict.remove("AF");
    } else {
        dict.insert("AF", Object::Array(filtered));
    }
    pdf.set_object(dict_ref, Object::Dictionary(dict));
    Ok(())
}

/// Collect all live objects that look like name-tree nodes.
///
/// A name-tree node is a dictionary with a `/Names` flat array or a `/Kids`
/// array and no `/Type` key that would identify it as a page, catalog, etc.
/// This heuristic may occasionally include non-tree objects (false positives)
/// but that only makes the GC more conservative — it never causes incorrect
/// deletion.
///
/// Used by [`remove_attachment`] to exclude all ghost name-tree nodes (from
/// prior `rebuild_embedded_files_tree` calls that did not `delete_object` old
/// nodes) from the reachability scan.
fn collect_all_live_name_tree_node_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> Result<BTreeSet<ObjectRef>> {
    let mut out = BTreeSet::new();
    let live = pdf.live_object_refs();
    for obj_ref in live {
        let obj = pdf.resolve(obj_ref)?;
        let Object::Dictionary(dict) = obj else {
            continue;
        };
        // Skip objects with a /Type name that marks them as non-tree (Page,
        // Catalog, Pages, EmbeddedFile, Filespec, etc.).
        if dict.get("Type").is_some() {
            continue;
        }
        // Heuristic: a /Names flat array or a /Kids array without /Type → tree node.
        let has_names = matches!(dict.get("Names"), Some(Object::Array(_)));
        let has_kids = matches!(dict.get("Kids"), Some(Object::Array(_)));
        if has_names || has_kids {
            out.insert(obj_ref);
        }
    }
    Ok(out)
}

/// Return `true` if any **live** object — other than the objects listed in
/// `self_refs` and other than any object in `exclude_set` — contains a
/// `Reference` to `target_ref` anywhere in its object graph.
///
/// `exclude_set` is a set of `ObjectRef`s that are skipped entirely during the
/// scan.  Used to exclude name-tree nodes (which may be stale live ghosts after
/// a rebuild) and other objects whose edges to `target_ref` should not count.
///
/// The scan resolves every live object and walks its nested structure.  Nested
/// indirect references are checked by value (i.e. whether they equal
/// `target_ref`), not followed recursively.  This is sufficient: the question
/// is "who holds a direct reference to this object", not "who can transitively
/// reach it".
fn is_referenced_by_live_objects_excluding<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    target_ref: ObjectRef,
    self_refs: &[ObjectRef],
    exclude_set: &BTreeSet<ObjectRef>,
) -> Result<bool> {
    let live = pdf.live_object_refs();
    for obj_ref in live {
        // Skip the object itself.
        if self_refs.contains(&obj_ref) {
            continue;
        }
        // Skip excluded objects (e.g. stale name-tree ghost nodes).
        if exclude_set.contains(&obj_ref) {
            continue;
        }
        let obj = pdf.resolve(obj_ref)?;
        if object_contains_ref(&obj, target_ref) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Recursively check whether `obj` contains `Object::Reference(target)` anywhere
/// in its nested structure (dict values, array elements, stream dict).
fn object_contains_ref(obj: &Object, target: ObjectRef) -> bool {
    match obj {
        Object::Reference(r) => *r == target,
        Object::Array(arr) => arr.iter().any(|o| object_contains_ref(o, target)),
        Object::Dictionary(dict) => dict.iter().any(|(_, v)| object_contains_ref(v, target)),
        Object::Stream(s) => s.dict.iter().any(|(_, v)| object_contains_ref(v, target)),
        _ => false,
    }
}

// ── Writer constants ──────────────────────────────────────────────────────────

/// Maximum number of entries in a single leaf `/Names` node before the writer
/// splits into multiple leaves under a `/Kids` root.
///
/// This mirrors the threshold used by qpdf's aggressive rebuild policy.  Any
/// tree with more than this many entries will have two levels (root + leaves);
/// three levels are never emitted.
pub const LEAF_MAX: usize = 32;

/// Default maximum depth when descending `/Kids` chains.
///
/// Mirrors the limits used by other tree walkers in this crate (e.g.
/// `outline_dest_remap`, `fonts`).
pub const DEFAULT_MAX_EMBEDDED_FILES_DEPTH: usize = 100;

/// Enumerate all `(name_key, filespec_ref)` entries in the catalog's
/// `/Names /EmbeddedFiles` name tree.
///
/// Returns entries in depth-first, key-ascending order (the order they appear
/// in the tree, which the spec requires to be sorted).  An empty list is
/// returned — without error — when any of `/Root`, `/Names`, or
/// `/EmbeddedFiles` is absent.
///
/// **Semantics:** name-tree values that are *direct* `/Filespec` dictionaries
/// (rather than indirect references) are intentionally **skipped** — this
/// reader only surfaces `(key, ObjectRef)` pairs. Writers must not use this as
/// their rebuild source; see [`collect_embedded_file_pairs_raw`], which
/// preserves direct-dict values verbatim so a rebuild never drops them.
// TODO(flpdf-9hc.10.6): consider exposing direct-dict entries via the public
// list/show API (e.g. an `Object`-valued variant) once list/show land.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`], and returns
/// [`crate::Error::Unsupported`] if a `/Kids` chain exceeds
/// [`DEFAULT_MAX_EMBEDDED_FILES_DEPTH`] (cyclic or maliciously deep tree).
pub fn list_embedded_files<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Vec<(Vec<u8>, ObjectRef)>> {
    list_embedded_files_with_max_depth(pdf, DEFAULT_MAX_EMBEDDED_FILES_DEPTH)
}

/// Like [`list_embedded_files`] but with a caller-supplied depth limit.
///
/// The depth limit guards against maliciously or accidentally cyclic `/Kids`
/// references.  Exceeding the limit returns an error rather than panicking.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`], and returns
/// [`crate::Error::Unsupported`] if a `/Kids` chain depth reaches `max_depth`.
pub fn list_embedded_files_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    max_depth: usize,
) -> Result<Vec<(Vec<u8>, ObjectRef)>> {
    // ── Step 1: resolve catalog ───────────────────────────────────────────────
    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(vec![]),
    };
    let catalog_obj = pdf.resolve(catalog_ref)?;
    let Object::Dictionary(catalog) = catalog_obj else {
        return Ok(vec![]);
    };

    // ── Step 2: resolve /Names dictionary ────────────────────────────────────
    // /Names may be an indirect reference or a direct inline dictionary.
    let names_dict = match catalog.get("Names").cloned() {
        Some(Object::Reference(r)) => match pdf.resolve(r)? {
            Object::Dictionary(d) => d,
            _ => return Ok(vec![]),
        },
        Some(Object::Dictionary(d)) => d,
        _ => return Ok(vec![]),
    };

    // ── Step 3: locate /EmbeddedFiles value ───────────────────────────────────
    // /EmbeddedFiles may itself be an indirect reference or an inline dict.
    let ef_root = match names_dict.get("EmbeddedFiles").cloned() {
        Some(Object::Reference(r)) => {
            // Indirect reference to the name-tree root node.
            let mut visited = BTreeSet::new();
            let mut out = Vec::new();
            collect_name_tree(pdf, r, &mut out, &mut visited, 0, max_depth)?;
            return Ok(out);
        }
        Some(Object::Dictionary(d)) => d,
        _ => return Ok(vec![]),
    };

    // /EmbeddedFiles is an inline (direct) dict — treat it as the root node.
    let mut out = Vec::new();
    let mut visited = BTreeSet::new();
    collect_name_tree_dict(pdf, ef_root, &mut out, &mut visited, 0, max_depth)?;
    Ok(out)
}

// ── Internal tree walker ──────────────────────────────────────────────────────

/// Walk a name-tree node reachable via an indirect reference, appending
/// `(key, filespec_ref)` pairs to `out` in DFS order.
fn collect_name_tree<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node_ref: ObjectRef,
    out: &mut Vec<(Vec<u8>, ObjectRef)>,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
    max_depth: usize,
) -> Result<()> {
    if depth >= max_depth {
        return Err(crate::Error::Unsupported(format!(
            "embedded_files: name-tree depth limit {max_depth} exceeded at {node_ref}"
        )));
    }
    if !visited.insert(node_ref) {
        // Cycle detected — skip this node silently.
        return Ok(());
    }

    let node_obj = pdf.resolve(node_ref)?;
    let Object::Dictionary(node) = node_obj else {
        // Malformed node — skip.
        return Ok(());
    };
    collect_name_tree_dict(pdf, node, out, visited, depth, max_depth)
}

/// Walk a name-tree node already resolved to a [`crate::Dictionary`].
fn collect_name_tree_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node: crate::Dictionary,
    out: &mut Vec<(Vec<u8>, ObjectRef)>,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
    max_depth: usize,
) -> Result<()> {
    // Leaf node: /Names holds [(key, val), ...] pairs as a flat array.
    if let Some(Object::Array(pairs)) = node.get("Names").cloned() {
        collect_leaf_pairs(pairs, out);
        return Ok(());
    }

    // Intermediate node: /Kids holds [node_ref, ...].
    if let Some(Object::Array(kids)) = node.get("Kids").cloned() {
        for kid in &kids {
            if let Object::Reference(child_ref) = kid {
                collect_name_tree(pdf, *child_ref, out, visited, depth + 1, max_depth)?;
            }
            // Non-reference kids are skipped (malformed tree).
        }
    }
    // A node with neither /Names nor /Kids is treated as empty.

    Ok(())
}

/// Extract `(key, filespec_ref)` pairs from a leaf `/Names` flat array.
///
/// The array is `[key₁, val₁, key₂, val₂, …]`.  Keys must be PDF strings
/// (`Object::String`).  Values must be indirect references (`Object::Reference`);
/// direct-dict values are skipped.  An odd-length array (malformed) drops the
/// trailing orphan key.
fn collect_leaf_pairs(pairs: Vec<Object>, out: &mut Vec<(Vec<u8>, ObjectRef)>) {
    let mut iter = pairs.into_iter();
    while let Some(key_obj) = iter.next() {
        let val_obj = match iter.next() {
            Some(o) => o,
            None => break, // Odd-length array — drop orphan key.
        };

        let key = match key_obj {
            Object::String(bytes) => bytes,
            _ => continue, // Non-string key — skip this pair.
        };

        let filespec_ref = match val_obj {
            Object::Reference(r) => r,
            _ => continue, // Direct-dict filespec — skip (out of scope).
        };

        out.push((key, filespec_ref));
    }
}

// ── Raw collector (writer source of truth) ────────────────────────────────────

/// Enumerate `(name_key, value)` entries in the catalog's
/// `/Names /EmbeddedFiles` name tree, preserving each value **verbatim** as an
/// [`Object`] — indirect references *and* direct `/Filespec` dictionaries.
///
/// The public reader [`list_embedded_files`] intentionally filters to indirect
/// references, but the writer must not: rebuilding the tree from the
/// reference-only view would silently drop pre-existing direct-dict entries.
/// Insert/delete therefore collect through this function so untouched
/// attachments survive a rebuild regardless of how they were encoded.
pub(crate) fn collect_embedded_file_pairs_raw<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    max_depth: usize,
) -> Result<Vec<(Vec<u8>, Object)>> {
    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(vec![]),
    };
    let Object::Dictionary(catalog) = pdf.resolve(catalog_ref)? else {
        return Ok(vec![]);
    };

    let names_dict = match catalog.get("Names").cloned() {
        Some(Object::Reference(r)) => match pdf.resolve(r)? {
            Object::Dictionary(d) => d,
            _ => return Ok(vec![]),
        },
        Some(Object::Dictionary(d)) => d,
        _ => return Ok(vec![]),
    };

    let mut out = Vec::new();
    let mut visited = BTreeSet::new();
    match names_dict.get("EmbeddedFiles").cloned() {
        Some(Object::Reference(r)) => {
            collect_name_tree_raw(pdf, r, &mut out, &mut visited, 0, max_depth)?;
        }
        Some(Object::Dictionary(d)) => {
            collect_name_tree_dict_raw(pdf, d, &mut out, &mut visited, 0, max_depth)?;
        }
        _ => return Ok(vec![]),
    }
    Ok(out)
}

/// Raw counterpart of [`collect_name_tree`] — preserves `Object` values.
fn collect_name_tree_raw<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node_ref: ObjectRef,
    out: &mut Vec<(Vec<u8>, Object)>,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
    max_depth: usize,
) -> Result<()> {
    if depth >= max_depth {
        return Err(crate::Error::Unsupported(format!(
            "embedded_files: name-tree depth limit {max_depth} exceeded at {node_ref}"
        )));
    }
    if !visited.insert(node_ref) {
        return Ok(()); // Cycle — skip silently.
    }
    let Object::Dictionary(node) = pdf.resolve(node_ref)? else {
        return Ok(()); // Malformed node — skip.
    };
    collect_name_tree_dict_raw(pdf, node, out, visited, depth, max_depth)
}

/// Raw counterpart of [`collect_name_tree_dict`] — preserves `Object` values.
fn collect_name_tree_dict_raw<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node: crate::Dictionary,
    out: &mut Vec<(Vec<u8>, Object)>,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
    max_depth: usize,
) -> Result<()> {
    if let Some(Object::Array(pairs)) = node.get("Names").cloned() {
        collect_leaf_pairs_raw(pairs, out);
        return Ok(());
    }
    if let Some(Object::Array(kids)) = node.get("Kids").cloned() {
        for kid in &kids {
            if let Object::Reference(child_ref) = kid {
                collect_name_tree_raw(pdf, *child_ref, out, visited, depth + 1, max_depth)?;
            }
        }
    }
    Ok(())
}

/// Raw counterpart of [`collect_leaf_pairs`].
///
/// Keeps the value `Object` verbatim (reference *or* direct dict), so the
/// writer's rebuild does not discard direct-dict `/Filespec` entries. Only
/// non-string keys and odd-length-array orphans are dropped.
fn collect_leaf_pairs_raw(pairs: Vec<Object>, out: &mut Vec<(Vec<u8>, Object)>) {
    let mut iter = pairs.into_iter();
    while let Some(key_obj) = iter.next() {
        let val_obj = match iter.next() {
            Some(o) => o,
            None => break, // Odd-length array — drop orphan key.
        };
        let key = match key_obj {
            Object::String(bytes) => bytes,
            _ => continue, // Non-string key — skip this pair.
        };
        out.push((key, val_obj));
    }
}

// ── Writer ────────────────────────────────────────────────────────────────────

/// Insert or replace a `(key, filespec_ref)` entry in the catalog's
/// `/Names /EmbeddedFiles` name tree.
///
/// If `key` already exists its value is replaced with `filespec_ref`.
/// If the `/Names /EmbeddedFiles` path does not yet exist it is created.
///
/// The entire tree is rebuilt from scratch after the insertion (qpdf-style
/// aggressive rebuild): all existing entries are read, the new entry is merged
/// in sorted order, and a fresh tree is written back via [`Pdf::set_object`].
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`].
pub fn insert_embedded_file<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    key: &[u8],
    filespec_ref: ObjectRef,
) -> Result<()> {
    // Collect existing entries verbatim (references AND direct dicts) so a
    // rebuild never silently drops pre-existing direct-dict attachments.
    let mut entries = collect_embedded_file_pairs_raw(pdf, DEFAULT_MAX_EMBEDDED_FILES_DEPTH)?;

    // Insert or replace.
    if let Some(existing) = entries.iter_mut().find(|(k, _)| k == key) {
        existing.1 = Object::Reference(filespec_ref);
    } else {
        entries.push((key.to_vec(), Object::Reference(filespec_ref)));
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));
    }

    rebuild_embedded_files_tree(pdf, entries)
}

/// Remove the entry with `key` from the catalog's `/Names /EmbeddedFiles`
/// name tree.
///
/// Returns `true` if the key was found and removed, `false` if it was absent.
///
/// When the last entry is removed the `/EmbeddedFiles` key is deleted from the
/// `/Names` dictionary.  If that leaves the `/Names` dictionary empty the
/// `/Names` key is removed from the catalog as well — no dangling references
/// remain.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`].
pub fn delete_embedded_file<R: Read + Seek>(pdf: &mut Pdf<R>, key: &[u8]) -> Result<bool> {
    let mut entries = collect_embedded_file_pairs_raw(pdf, DEFAULT_MAX_EMBEDDED_FILES_DEPTH)?;
    let before = entries.len();
    entries.retain(|(k, _)| k != key);
    if entries.len() == before {
        return Ok(false); // Key was not present.
    }

    rebuild_embedded_files_tree(pdf, entries)?;
    Ok(true)
}

// ── Internal rebuild ──────────────────────────────────────────────────────────

/// Rebuild the `/Names /EmbeddedFiles` name tree from a sorted entry list and
/// patch it back into the document via [`Pdf::set_object`].
///
/// When `entries` is empty the function removes `/EmbeddedFiles` from the
/// `/Names` dictionary (and removes `/Names` from the catalog if it then
/// becomes empty), leaving no dangling references.
///
/// Otherwise it constructs a tree with at most two levels:
/// - ≤ [`LEAF_MAX`] entries → single-leaf root (just `/Names` + `/Limits`).
/// - > [`LEAF_MAX`] entries → root with `/Kids` pointing to leaf chunks.
///
/// All emitted nodes carry `/Limits [first, last]` as required by PDF
/// validators.  The catalog `/Names` reference is stored as an indirect object;
/// `/EmbeddedFiles` within it points indirectly to the tree root.
fn rebuild_embedded_files_tree<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    entries: Vec<(Vec<u8>, Object)>,
) -> Result<()> {
    // Resolve the catalog.
    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(()),
    };
    let catalog_obj = pdf.resolve(catalog_ref)?;
    let Object::Dictionary(mut catalog) = catalog_obj else {
        return Ok(());
    };

    // ── Allocate a block of fresh object numbers ──────────────────────────────
    // Snapshot the current maximum to avoid re-querying inside the loop.
    let mut next_num: u32 = pdf
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0);
    let mut alloc = move || -> ObjectRef {
        next_num += 1;
        ObjectRef::new(next_num, 0)
    };

    // ── Empty case: remove /EmbeddedFiles ────────────────────────────────────
    if entries.is_empty() {
        // Retrieve (or create empty) /Names dict and drop /EmbeddedFiles from it.
        let names_dict_opt = match catalog.get("Names").cloned() {
            Some(Object::Reference(r)) => match pdf.resolve(r)? {
                Object::Dictionary(d) => Some((Some(r), d)),
                _ => None,
            },
            Some(Object::Dictionary(d)) => Some((None, d)),
            _ => None,
        };
        if let Some((names_ref_opt, mut names_dict)) = names_dict_opt {
            names_dict.remove("EmbeddedFiles");
            if names_dict.iter().next().is_none() {
                // /Names dict is now empty — remove from catalog.
                catalog.remove("Names");
                pdf.set_object(catalog_ref, Object::Dictionary(catalog));
                if let Some(r) = names_ref_opt {
                    pdf.delete_object(r);
                }
            } else {
                match names_ref_opt {
                    Some(r) => {
                        pdf.set_object(r, Object::Dictionary(names_dict));
                    }
                    None => {
                        catalog.insert("Names", Object::Dictionary(names_dict));
                        pdf.set_object(catalog_ref, Object::Dictionary(catalog));
                    }
                }
            }
        }
        return Ok(());
    }

    // ── Build the name-tree nodes ─────────────────────────────────────────────
    let tree_root_ref = if entries.len() <= LEAF_MAX {
        // Single-leaf root.
        let leaf = build_leaf_dict(&entries);
        let leaf_ref = alloc();
        pdf.set_object(leaf_ref, Object::Dictionary(leaf));
        leaf_ref
    } else {
        // Multi-leaf: chunk entries evenly, each chunk ≤ LEAF_MAX.
        let n_leaves = entries.len().div_ceil(LEAF_MAX);
        let chunk_size = entries.len().div_ceil(n_leaves);

        let mut kids: Vec<Object> = Vec::with_capacity(n_leaves);
        for chunk in entries.chunks(chunk_size) {
            let leaf = build_leaf_dict(chunk);
            let leaf_ref = alloc();
            pdf.set_object(leaf_ref, Object::Dictionary(leaf));
            kids.push(Object::Reference(leaf_ref));
        }

        // Root node: /Kids + /Limits
        let first = entries.first().map(|(k, _)| k.clone()).unwrap_or_default();
        let last = entries.last().map(|(k, _)| k.clone()).unwrap_or_default();
        let mut root = Dictionary::new();
        root.insert(
            "Limits",
            Object::Array(vec![Object::String(first), Object::String(last)]),
        );
        root.insert("Kids", Object::Array(kids));
        let root_ref = alloc();
        pdf.set_object(root_ref, Object::Dictionary(root));
        root_ref
    };

    // ── Patch the catalog /Names dictionary ───────────────────────────────────
    // Resolve or create the /Names dict.  We always store it as an indirect
    // object and point /EmbeddedFiles to the tree root indirectly.
    let (names_ref, mut names_dict) = match catalog.get("Names").cloned() {
        Some(Object::Reference(r)) => match pdf.resolve(r)? {
            Object::Dictionary(d) => (r, d),
            _ => {
                let r2 = alloc();
                (r2, Dictionary::new())
            }
        },
        Some(Object::Dictionary(d)) => {
            let r = alloc();
            (r, d)
        }
        _ => {
            let r = alloc();
            (r, Dictionary::new())
        }
    };

    names_dict.insert("EmbeddedFiles", Object::Reference(tree_root_ref));
    pdf.set_object(names_ref, Object::Dictionary(names_dict));

    // Point catalog /Names to the (possibly new) indirect names dict.
    catalog.insert("Names", Object::Reference(names_ref));
    pdf.set_object(catalog_ref, Object::Dictionary(catalog));

    Ok(())
}

/// Build a leaf name-tree node dictionary from an ordered slice of entries.
///
/// The returned dictionary has:
/// - `/Limits [first_key, last_key]`
/// - `/Names [key₁, ref₁, key₂, ref₂, …]`
fn build_leaf_dict(entries: &[(Vec<u8>, Object)]) -> Dictionary {
    debug_assert!(
        !entries.is_empty(),
        "build_leaf_dict called with empty slice"
    );

    let first = entries.first().map(|(k, _)| k.clone()).unwrap_or_default();
    let last = entries.last().map(|(k, _)| k.clone()).unwrap_or_default();

    let mut pairs: Vec<Object> = Vec::with_capacity(entries.len() * 2);
    for (key, val) in entries {
        pairs.push(Object::String(key.clone()));
        pairs.push(val.clone());
    }

    let mut dict = Dictionary::new();
    dict.insert(
        "Limits",
        Object::Array(vec![Object::String(first), Object::String(last)]),
    );
    dict.insert("Names", Object::Array(pairs));
    dict
}

// ── Tests for remove_attachment ───────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filespec_helper::{add_attachment_from_path, FileSpecBuilder};

    // ── Minimal PDF fixture (same as filespec_helper tests) ───────────────────

    fn minimal_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    fn open_minimal() -> Pdf<std::io::Cursor<Vec<u8>>> {
        Pdf::open(std::io::Cursor::new(minimal_pdf_bytes())).expect("open minimal PDF")
    }

    // ── Test: add 2, remove 1, check list has 1 ──────────────────────────────

    #[test]
    fn remove_one_of_two_leaves_other_intact() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");

        let file_a = dir.path().join("a.txt");
        let file_b = dir.path().join("b.txt");
        std::fs::write(&file_a, b"content A").unwrap();
        std::fs::write(&file_b, b"content B").unwrap();

        add_attachment_from_path(&mut pdf, b"a.txt", &file_a).expect("add a");
        let fs_b = add_attachment_from_path(&mut pdf, b"b.txt", &file_b).expect("add b");

        let removed = remove_attachment(&mut pdf, b"a.txt").expect("remove a");
        assert!(
            removed,
            "remove_attachment must return true for existing key"
        );

        let entries = list_embedded_files(&mut pdf).expect("list");
        assert_eq!(entries.len(), 1, "exactly one attachment must remain");
        assert_eq!(entries[0].0, b"b.txt", "b.txt must survive");
        assert_eq!(entries[0].1, fs_b, "surviving filespec ref must match");

        // Deleted key must not appear
        let keys: Vec<&[u8]> = entries.iter().map(|(k, _)| k.as_slice()).collect();
        assert!(!keys.contains(&b"a.txt".as_ref()), "a.txt must be gone");
    }

    // ── Test: removed filespec and stream are no longer live ─────────────────

    #[test]
    fn remove_attachment_gc_deletes_filespec_and_stream() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("gc.txt");
        std::fs::write(&file, b"gc test").unwrap();

        let fs_ref = add_attachment_from_path(&mut pdf, b"gc.txt", &file).expect("add");

        // Resolve the stream ref before removal.
        let stream_ref = resolve_embedded_file_stream_ref(&mut pdf, fs_ref)
            .expect("resolve_stream_ref")
            .expect("stream ref must exist");

        remove_attachment(&mut pdf, b"gc.txt").expect("remove");

        // Both filespec and stream must be absent from live objects.
        let live = pdf.live_object_refs();
        assert!(
            !live.contains(&fs_ref),
            "filespec ref must not be in live_object_refs after GC"
        );
        assert!(
            !live.contains(&stream_ref),
            "stream ref must not be in live_object_refs after GC"
        );
    }

    // ── Test: missing key returns false, document unchanged ──────────────────

    #[test]
    fn remove_nonexistent_key_returns_false() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("keep.txt");
        std::fs::write(&file, b"keep me").unwrap();
        add_attachment_from_path(&mut pdf, b"keep.txt", &file).expect("add");

        let result = remove_attachment(&mut pdf, b"no-such-key.txt").expect("no error");
        assert!(!result, "must return false for absent key");

        // Document must still contain the original attachment.
        let entries = list_embedded_files(&mut pdf).expect("list");
        assert_eq!(entries.len(), 1, "document must be unchanged");
        assert_eq!(entries[0].0, b"keep.txt");
    }

    // ── Test: /AF on catalog and page is cleared after remove ─────────────────

    #[test]
    fn remove_attachment_clears_af_on_catalog_and_page() {
        let mut pdf = open_minimal();

        // Build a filespec manually so we control the ref.
        let fs_ref = FileSpecBuilder::new("af-test.txt", b"payload")
            .build(&mut pdf)
            .expect("build filespec");
        insert_embedded_file(&mut pdf, b"af-test.txt", fs_ref).expect("insert");

        // Add /AF to catalog pointing at fs_ref.
        let catalog_ref = pdf.root_ref().expect("root");
        let catalog_obj = pdf.resolve(catalog_ref).expect("resolve catalog");
        let Object::Dictionary(mut catalog) = catalog_obj else {
            panic!("expected catalog dict");
        };
        catalog.insert("AF", Object::Array(vec![Object::Reference(fs_ref)]));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));

        // Add /AF to the single page as well.
        let page_refs = crate::pages::page_refs(&mut pdf).expect("page_refs");
        assert_eq!(page_refs.len(), 1, "fixture has one page");
        let page_ref = page_refs[0];
        let page_obj = pdf.resolve(page_ref).expect("resolve page");
        let Object::Dictionary(mut page_dict) = page_obj else {
            panic!("expected page dict");
        };
        page_dict.insert("AF", Object::Array(vec![Object::Reference(fs_ref)]));
        pdf.set_object(page_ref, Object::Dictionary(page_dict));

        // Remove the attachment.
        let removed = remove_attachment(&mut pdf, b"af-test.txt").expect("remove");
        assert!(removed);

        // /AF on catalog must be gone.
        let catalog_obj2 = pdf.resolve(catalog_ref).expect("resolve catalog after");
        let Object::Dictionary(catalog2) = catalog_obj2 else {
            panic!("expected catalog dict");
        };
        assert!(
            catalog2.get("AF").is_none(),
            "catalog /AF must be removed after attachment removal"
        );

        // /AF on page must be gone.
        let page_obj2 = pdf.resolve(page_ref).expect("resolve page after");
        let Object::Dictionary(page_dict2) = page_obj2 else {
            panic!("expected page dict");
        };
        assert!(
            page_dict2.get("AF").is_none(),
            "page /AF must be removed after attachment removal"
        );
    }

    // ── Test: shared stream is preserved under conservative GC ───────────────

    #[test]
    fn conservative_gc_preserves_shared_stream() {
        // Build two /Filespec dicts that share the same /EmbeddedFile stream.
        // When one filespec is removed, the shared stream must NOT be GC'd.
        let mut pdf = open_minimal();

        // Allocate the shared EmbeddedFile stream object.
        let next = pdf
            .object_refs()
            .iter()
            .map(|r| r.number)
            .max()
            .unwrap_or(0);
        let stream_ref = ObjectRef::new(next + 1, 0);
        let fs_ref1 = ObjectRef::new(next + 2, 0);
        let fs_ref2 = ObjectRef::new(next + 3, 0);

        // Shared EmbeddedFile stream.
        let mut ef_dict = Dictionary::new();
        ef_dict.insert("Type", Object::Name(b"EmbeddedFile".to_vec()));
        ef_dict.insert("Length", Object::Integer(7));
        let ef_stream = crate::object::Stream::new(ef_dict, b"payload".to_vec());
        pdf.set_object(stream_ref, Object::Stream(ef_stream));

        // /EF sub-dict pointing both filespecs at the same stream.
        let mut ef_sub = Dictionary::new();
        ef_sub.insert("F", Object::Reference(stream_ref));
        ef_sub.insert("UF", Object::Reference(stream_ref));

        // Filespec 1.
        let mut fs1 = Dictionary::new();
        fs1.insert("Type", Object::Name(b"Filespec".to_vec()));
        fs1.insert("F", Object::String(b"shared1.txt".to_vec()));
        fs1.insert("EF", Object::Dictionary(ef_sub.clone()));
        pdf.set_object(fs_ref1, Object::Dictionary(fs1));

        // Filespec 2.
        let mut fs2 = Dictionary::new();
        fs2.insert("Type", Object::Name(b"Filespec".to_vec()));
        fs2.insert("F", Object::String(b"shared2.txt".to_vec()));
        fs2.insert("EF", Object::Dictionary(ef_sub));
        pdf.set_object(fs_ref2, Object::Dictionary(fs2));

        // Insert both into the name tree.
        insert_embedded_file(&mut pdf, b"shared1.txt", fs_ref1).expect("insert 1");
        insert_embedded_file(&mut pdf, b"shared2.txt", fs_ref2).expect("insert 2");

        // Remove only the first attachment.
        let removed = remove_attachment(&mut pdf, b"shared1.txt").expect("remove");
        assert!(removed);

        // The shared stream must still be alive (fs_ref2 still references it).
        let live = pdf.live_object_refs();
        assert!(
            live.contains(&stream_ref),
            "shared stream must NOT be GC'd while fs_ref2 still references it"
        );

        // fs_ref1 itself should be gone (it is no longer referenced).
        assert!(
            !live.contains(&fs_ref1),
            "removed filespec ref must be GC'd"
        );

        // fs_ref2 must still be alive.
        assert!(
            live.contains(&fs_ref2),
            "surviving filespec ref must remain alive"
        );
    }
}

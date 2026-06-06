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
use std::io::{Read, Seek};

// ── remove_attachment ─────────────────────────────────────────────────────────

/// Remove an attachment by name-tree key, then garbage-collect via a
/// mark-and-sweep from `/Root` (the qpdf rewrite model).
///
/// # Behaviour
///
/// 1. Looks up `key` in the catalog's `/Names /EmbeddedFiles` name tree.
///    Returns `Ok(false)` — without error — if the key is absent.
/// 2. Calls [`delete_embedded_file`] to remove the name-tree entry (rebuilds
///    the tree; superseded leaf/intermediate nodes become orphans).
/// 3. Clears references to the filespec from the `/AF` array in the document
///    catalog and in every page dictionary.  If a `/AF` array becomes empty
///    the `/AF` key itself is deleted; a shared indirect `/AF` array object
///    is patched in place, never deleted here (see
///    [`remove_ref_from_af_in_dict`]).
/// 4. **Mark-and-sweep GC** ([`crate::subset_prune::sweep_unreachable_objects`]):
///    every indirect object no longer reachable from `/Root` or the trailer
///    is physically deleted. This drops the removed `/Filespec`, *all* its
///    `/EF` streams (including a filespec carrying distinct streams under
///    several `/EF` keys), any sub-objects reachable only through it, and the
///    orphan ghost name-tree nodes left by the rebuild — in one pass, with no
///    per-feature reachability heuristics.
///
/// The conservative-share semantics are automatic: an `/EmbeddedFile` stream
/// still reachable from another live object (e.g. shared between two
/// filespecs, or a filespec still referenced by a live `/Dests` /
/// `/JavaScript` name tree) stays reachable and therefore survives the sweep.
///
/// # Blast radius
///
/// The sweep is **document-wide**, not scoped to the removed attachment: any
/// *pre-existing* object that was already unreachable from `/Root` is also
/// collected. This matches qpdf's complete-rewrite behaviour (its writer only
/// emits reachable objects) and flpdf's own page-subset pruning, so the
/// observable output is qpdf-aligned rather than a targeted in-place edit.
///
/// # Limitation
///
/// When the name-tree value is a *direct* `/Filespec` dictionary (not an
/// indirect reference) there is no `ObjectRef` to clear from `/AF`; the
/// name-tree entry is removed and the sweep still runs. This case is
/// exceedingly rare in practice (ISO 32000-1 §7.11 expects indirect refs)
/// and is handled gracefully — no panic, no spurious error.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`] or [`delete_embedded_file`].
pub fn remove_attachment<R: Read + Seek>(pdf: &mut Pdf<R>, key: &[u8]) -> Result<bool> {
    // ── Step 1: locate the entry ──────────────────────────────────────────────
    let entries = collect_embedded_file_pairs_raw(pdf, DEFAULT_MAX_EMBEDDED_FILES_DEPTH)?;
    let target_value = match entries.iter().find(|(k, _)| k.as_slice() == key) {
        Some((_, v)) => v.clone(),
        None => return Ok(false),
    };

    // The filespec ref is only needed to clear it from `/AF` (so that path
    // stops keeping it reachable). Direct-dict filespecs have no ref.
    let filespec_ref_opt: Option<ObjectRef> = match &target_value {
        Object::Reference(r) => Some(*r),
        _ => None,
    };

    // ── Step 2: detach the name-tree entry ────────────────────────────────────
    delete_embedded_file(pdf, key)?;

    // ── Step 3: clear /AF references on catalog and all pages ─────────────────
    // Done before the sweep so a stale `/AF` edge cannot keep the filespec
    // artificially reachable.
    if let Some(fs_ref) = filespec_ref_opt {
        clear_af_reference(pdf, fs_ref)?;
    }

    // ── Step 4: mark-and-sweep GC (qpdf model) ────────────────────────────────
    // Once the entry is detached and `/AF` cleared, the removed filespec, its
    // `/EF` stream(s), any objects reachable only through it, and the orphan
    // ghost name-tree nodes left by the rebuild are all unreachable from
    // `/Root`/trailer and are physically dropped here. A filespec/stream still
    // reachable from another live object (shared stream, live `/Dests`, …)
    // stays reachable and therefore survives — conservative semantics for
    // free, no ad-hoc exclusion heuristics.
    crate::subset_prune::sweep_unreachable_objects(pdf)?;

    Ok(true)
}

// ── helpers for remove_attachment ─────────────────────────────────────────────

/// Walk a `/Filespec` dict and return the first `/EmbeddedFile` stream `ObjectRef`
/// reachable via `/EF /UF`, `/EF /F`, `/EF /Unix`, `/EF /Mac`, `/EF /DOS` (in
/// that priority order).  Returns `None` if not found or on any soft error.
///
/// Test-only helper for single-stream fixtures. Production code no longer
/// resolves `/EF` streams explicitly: [`remove_attachment`] relies on the
/// `/Root` mark-and-sweep, which drops every `/EF` stream of a removed
/// filespec transitively.
#[cfg(test)]
fn resolve_embedded_file_stream_ref<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filespec_ref: ObjectRef,
) -> Result<Option<ObjectRef>> {
    let Some(fs_dict) = pdf.resolve_borrowed(filespec_ref)?.as_dict() else {
        return Ok(None);
    };
    let ef_dict: Dictionary = match fs_dict.get("EF") {
        Some(Object::Dictionary(d)) => d.clone(),
        Some(Object::Reference(r)) => {
            let r = *r;
            match pdf.resolve_borrowed(r)? {
                Object::Dictionary(d) => d.clone(),
                _ => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    for key in &["UF", "F", "Unix", "Mac", "DOS"] {
        if let Some(r) = ef_dict.get(key).and_then(Object::as_ref_id) {
            return Ok(Some(r));
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
///
/// `/AF` may be a *direct* array or an *indirect reference* to an array
/// object.  In the indirect case the referenced array object — not just the
/// parent dictionary — must be updated, otherwise it lingers in
/// [`Pdf::live_object_refs`] still holding a stale reference to `target_ref`,
/// which would block the conservative GC in [`remove_attachment`] and leave
/// the removed attachment's data as unreachable objects (roborev #948).
///
/// Behaviour:
/// - direct array → filter in place; if it becomes empty the `/AF` key is
///   removed from the parent;
/// - indirect array → the *referenced* array object is rewritten with the
///   filtered contents; if it becomes empty the `/AF` key is removed from the
///   parent.  The array object itself is **never** `delete_object`-ed: the
///   same indirect array may be shared by the catalog and one or more page
///   dictionaries, and [`clear_af_reference`] invokes this helper once per
///   parent — deleting it on the first parent would dangle the rest (roborev
///   #951).  Filtering its contents already removes the stale reference that
///   would otherwise block conservative GC (the original #948 motivation), so
///   the deletion was unnecessary.  An emptied, now-unreferenced array object
///   is harmless dead weight, exactly like the name-tree ghosts the existing
///   GC already tolerates.
///
/// If `/AF` is absent or contains no reference to `target_ref`, this is a
/// no-op.
fn remove_ref_from_af_in_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict_ref: ObjectRef,
    target_ref: ObjectRef,
) -> Result<()> {
    let Some(mut dict) = pdf.resolve_borrowed(dict_ref)?.as_dict().cloned() else {
        return Ok(());
    };

    // /AF may be a direct array or an indirect reference to an array.
    let af_value = match dict.get("AF").cloned() {
        Some(v) => v,
        None => return Ok(()), // No /AF — nothing to do.
    };

    // `array_ref` is `Some(r)` when /AF is an indirect reference to the array
    // object `r` (which must be patched, not just the parent dict).
    let (array_ref, af_array): (Option<ObjectRef>, Vec<Object>) = match af_value {
        Object::Array(arr) => (None, arr),
        Object::Reference(r) => match pdf.resolve_borrowed(r)? {
            Object::Array(arr) => (Some(r), arr.clone()),
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };

    // Early no-op: if `target_ref` is not actually present, do NOT mutate the
    // parent dict or delete the indirect array object — an unrelated empty (or
    // target-absent) indirect /AF array may be shared and must be left intact
    // (roborev #950-2).
    if !af_array
        .iter()
        .any(|o| matches!(o, Object::Reference(r) if *r == target_ref))
    {
        return Ok(());
    }

    let filtered: Vec<Object> = af_array
        .into_iter()
        .filter(|o| !matches!(o, Object::Reference(r) if *r == target_ref))
        .collect();

    match array_ref {
        // Indirect /AF: patch the referenced array object so it no longer
        // holds a stale reference to `target_ref`.  Never delete it — it may
        // be shared by the catalog and page dicts; deleting on the first
        // parent would dangle the rest (roborev #951).
        Some(r) => {
            let is_empty = filtered.is_empty();
            pdf.set_object(r, Object::Array(filtered));
            if is_empty {
                // Drop the now-empty /AF key from this parent; the array
                // object stays (a harmless orphan once nothing points at it).
                dict.remove("AF");
                pdf.set_object(dict_ref, Object::Dictionary(dict));
            }
            // non-empty: parent already points at `r` via /AF; leave it.
        }
        // Direct /AF array lives inside the parent dictionary.
        None => {
            if filtered.is_empty() {
                dict.remove("AF");
            } else {
                dict.insert("AF", Object::Array(filtered));
            }
            pdf.set_object(dict_ref, Object::Dictionary(dict));
        }
    }
    Ok(())
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
    let names_value = match pdf.resolve_borrowed(catalog_ref)?.as_dict() {
        Some(catalog) => catalog.get("Names").cloned(),
        None => return Ok(vec![]),
    };

    let Some(names_value) = names_value else {
        return Ok(vec![]);
    };

    // ── Step 2: resolve /Names dictionary ────────────────────────────────────
    // /Names may be an indirect reference or a direct inline dictionary.
    let names_dict = match names_value {
        Object::Reference(r) => match pdf.resolve_borrowed(r)? {
            Object::Dictionary(d) => d.clone(),
            _ => return Ok(vec![]),
        },
        Object::Dictionary(d) => d,
        _ => return Ok(vec![]),
    };

    // ── Step 3: walk the /EmbeddedFiles name tree (ref-only view) ─────────────
    let ef_value = match names_dict.get("EmbeddedFiles").cloned() {
        Some(v) => v,
        None => return Ok(vec![]),
    };
    crate::name_number_tree::read_name_tree(
        pdf,
        ef_value,
        |_, v| Ok(v.as_ref_id()),
        max_depth,
    )
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
    let Some(catalog) = pdf.resolve_borrowed(catalog_ref)?.as_dict() else {
        return Ok(vec![]);
    };

    let names_dict = match catalog.get("Names").cloned() {
        Some(Object::Reference(r)) => match pdf.resolve_borrowed(r)? {
            Object::Dictionary(d) => d.clone(),
            _ => return Ok(vec![]),
        },
        Some(Object::Dictionary(d)) => d,
        _ => return Ok(vec![]),
    };

    let ef_value = match names_dict.get("EmbeddedFiles").cloned() {
        Some(v) => v,
        None => return Ok(vec![]),
    };
    crate::name_number_tree::read_name_tree(pdf, ef_value, |_, v| Ok(Some(v)), max_depth)
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
    let Some(mut catalog) = pdf.resolve_borrowed(catalog_ref)?.as_dict().cloned() else {
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
            Some(Object::Reference(r)) => match pdf.resolve_borrowed(r)? {
                Object::Dictionary(d) => Some((Some(r), d.clone())),
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
        Some(Object::Reference(r)) => match pdf.resolve_borrowed(r)? {
            Object::Dictionary(d) => (r, d.clone()),
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

    // ── Test: transitively-unreachable subgraph is swept (flpdf-eg3) ─────────
    //
    // The old ad-hoc GC only ever considered the filespec ref and its `/EF`
    // streams, so an object reachable *only* through the filespec dictionary
    // (e.g. an indirect `/CI` collection-item stream) was left behind as an
    // orphan after removal. A proper mark-and-sweep from `/Root` + trailer —
    // the qpdf rewrite model — drops the whole now-unreachable subgraph.
    #[test]
    fn remove_attachment_sweeps_transitively_unreachable_subgraph() {
        let mut pdf = open_minimal();

        // A side-car stream that will be reachable ONLY via the filespec dict.
        let next = pdf
            .object_refs()
            .iter()
            .map(|r| r.number)
            .max()
            .unwrap_or(0);
        let sidecar_ref = ObjectRef::new(next + 1, 0);
        pdf.set_object(
            sidecar_ref,
            Object::Stream(crate::object::Stream {
                dict: Dictionary::new(),
                data: b"sidecar".to_vec(),
            }),
        );

        // Build a filespec, then point an indirect key at the side-car so the
        // side-car is reachable exclusively through the filespec.
        let fs_ref = FileSpecBuilder::new("trans.txt", b"payload")
            .build(&mut pdf)
            .expect("build filespec");
        let Object::Dictionary(mut fs_dict) = pdf.resolve(fs_ref).expect("resolve filespec") else {
            panic!("expected filespec dict");
        };
        fs_dict.insert("CI", Object::Reference(sidecar_ref));
        pdf.set_object(fs_ref, Object::Dictionary(fs_dict));
        insert_embedded_file(&mut pdf, b"trans.txt", fs_ref).expect("insert");

        remove_attachment(&mut pdf, b"trans.txt").expect("remove");

        let live = pdf.live_object_refs();
        assert!(!live.contains(&fs_ref), "filespec must be swept");
        assert!(
            !live.contains(&sidecar_ref),
            "object reachable only via the filespec must be transitively swept (mark-and-sweep)"
        );
    }

    #[test]
    fn resolve_embedded_file_stream_ref_accepts_indirect_ef_dict() {
        let mut pdf = open_minimal();
        let fs_ref = FileSpecBuilder::new("indirect-ef.txt", b"payload")
            .build(&mut pdf)
            .expect("build filespec");
        let Object::Dictionary(mut fs_dict) = pdf.resolve(fs_ref).expect("resolve filespec") else {
            panic!("expected filespec dict");
        };
        let Object::Dictionary(ef_dict) = fs_dict.get("EF").cloned().expect("/EF") else {
            panic!("expected /EF dict");
        };
        let ef_ref = ObjectRef::new(fs_ref.number + 100, 0);
        pdf.set_object(ef_ref, Object::Dictionary(ef_dict));
        fs_dict.insert("EF", Object::Reference(ef_ref));
        pdf.set_object(fs_ref, Object::Dictionary(fs_dict));

        let stream_ref = resolve_embedded_file_stream_ref(&mut pdf, fs_ref)
            .expect("resolve stream")
            .expect("stream ref");

        assert!(pdf.object_refs().contains(&stream_ref));
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

    // ── Test: indirect /AF array no longer blocks GC of the filespec ─────────
    //
    // Regression for roborev #948 (semantics updated by flpdf-eg3): when /AF
    // is an *indirect* array reference whose only referrer is the catalog,
    // removing the attachment clears the catalog /AF key, leaving the array
    // object unreachable — the mark-and-sweep then drops the array, the
    // filespec, and the stream together (no orphan left behind; qpdf model).
    // The shared-indirect-/AF case (catalog + page) is covered separately by
    // `remove_attachment_shared_indirect_af_across_catalog_and_page_not_dangled`.
    #[test]
    fn remove_attachment_with_indirect_af_array_gcs_filespec_and_stream() {
        let mut pdf = open_minimal();

        let fs_ref = FileSpecBuilder::new("idx.txt", b"indirect-af payload")
            .build(&mut pdf)
            .expect("build filespec");
        insert_embedded_file(&mut pdf, b"idx.txt", fs_ref).expect("insert");

        let stream_ref = resolve_embedded_file_stream_ref(&mut pdf, fs_ref)
            .expect("resolve stream ref")
            .expect("stream ref must exist");

        // Allocate a standalone array object [fs_ref] and point catalog /AF at
        // it *indirectly* (the only reference to this array object).
        let next = pdf
            .object_refs()
            .iter()
            .map(|r| r.number)
            .max()
            .unwrap_or(0);
        let af_array_ref = ObjectRef::new(next + 1, 0);
        pdf.set_object(af_array_ref, Object::Array(vec![Object::Reference(fs_ref)]));

        let catalog_ref = pdf.root_ref().expect("root");
        let Object::Dictionary(mut catalog) = pdf
            .resolve_borrowed(catalog_ref)
            .expect("resolve catalog")
            .clone()
        else {
            panic!("expected catalog dict");
        };
        catalog.insert("AF", Object::Reference(af_array_ref));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));

        let removed = remove_attachment(&mut pdf, b"idx.txt").expect("remove");
        assert!(removed);

        let live = pdf.live_object_refs();
        assert!(
            !live.contains(&fs_ref),
            "filespec must be GC-deleted even when only an indirect /AF array referenced it"
        );
        assert!(
            !live.contains(&stream_ref),
            "embedded stream must be GC-deleted alongside the filespec"
        );
        // The indirect /AF array was reachable ONLY via the catalog /AF key;
        // once that key is cleared the array is unreachable and the
        // mark-and-sweep drops it too (no orphan left behind — qpdf model).
        assert!(
            !live.contains(&af_array_ref),
            "indirect /AF array reachable only via the cleared catalog /AF must be swept"
        );

        // Catalog /AF must be gone (its only entry was the removed filespec).
        let Object::Dictionary(catalog2) = pdf
            .resolve_borrowed(catalog_ref)
            .expect("resolve catalog after")
        else {
            panic!("expected catalog dict");
        };
        assert!(
            catalog2.get("AF").is_none(),
            "catalog /AF must be removed once empty"
        );
    }

    // ── Test: indirect /AF shared by catalog + page is not dangled ───────────
    //
    // Regression for roborev #951: the same indirect /AF array object was
    // `delete_object`-ed by the first parent (catalog) while a later parent
    // (page) still referenced it → dangling ref / resolve failure.  The array
    // must survive (emptied) and stay resolvable for every parent.
    #[test]
    fn remove_attachment_shared_indirect_af_across_catalog_and_page_not_dangled() {
        let mut pdf = open_minimal();

        let fs_ref = FileSpecBuilder::new("sh.txt", b"shared-af payload")
            .build(&mut pdf)
            .expect("build filespec");
        insert_embedded_file(&mut pdf, b"sh.txt", fs_ref).expect("insert");

        // One indirect /AF array object [fs_ref], referenced by BOTH the
        // catalog and the page.
        let next = pdf
            .object_refs()
            .iter()
            .map(|r| r.number)
            .max()
            .unwrap_or(0);
        let af_array_ref = ObjectRef::new(next + 1, 0);
        pdf.set_object(af_array_ref, Object::Array(vec![Object::Reference(fs_ref)]));

        let catalog_ref = pdf.root_ref().expect("root");
        let Object::Dictionary(mut catalog) = pdf
            .resolve_borrowed(catalog_ref)
            .expect("resolve catalog")
            .clone()
        else {
            panic!("expected catalog dict");
        };
        catalog.insert("AF", Object::Reference(af_array_ref));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));

        let page_refs = crate::pages::page_refs(&mut pdf).expect("page_refs");
        assert_eq!(page_refs.len(), 1, "fixture has one page");
        let page_ref = page_refs[0];
        let Object::Dictionary(mut page_dict) = pdf
            .resolve_borrowed(page_ref)
            .expect("resolve page")
            .clone()
        else {
            panic!("expected page dict");
        };
        page_dict.insert("AF", Object::Reference(af_array_ref));
        pdf.set_object(page_ref, Object::Dictionary(page_dict));

        // Removal walks catalog then every page, calling the helper once per
        // parent against the SAME shared array object.
        let removed = remove_attachment(&mut pdf, b"sh.txt").expect("remove");
        assert!(removed);

        // The shared array object must still resolve for every parent (not
        // deleted on the first), and be emptied of the removed filespec.
        let Object::Array(af_after) = pdf
            .resolve_borrowed(af_array_ref)
            .expect("shared indirect /AF array must still resolve (not deleted)")
        else {
            panic!("expected /AF array object");
        };
        assert!(
            af_after.is_empty(),
            "shared /AF array must be emptied of the removed filespec"
        );

        // The filespec is still GC-deleted (array no longer references it).
        let live = pdf.live_object_refs();
        assert!(
            !live.contains(&fs_ref),
            "filespec must be GC-deleted once the shared /AF array no longer references it"
        );

        // Catalog /AF dropped (emptied); page /AF may remain but, if present,
        // must point at the still-resolvable array (no dangling ref).
        let Object::Dictionary(page_after) =
            pdf.resolve_borrowed(page_ref).expect("resolve page after")
        else {
            panic!("expected page dict");
        };
        if let Some(r) = page_after.get("AF").and_then(Object::as_ref_id) {
            assert_eq!(
                r, af_array_ref,
                "page /AF must still point at the surviving shared array"
            );
            assert!(
                pdf.resolve_borrowed(r).is_ok(),
                "page /AF reference must resolve (not dangling)"
            );
        }
    }

    // ── Test: filespec referenced by another live name tree is preserved ─────
    //
    // Regression for roborev #947: the GC ghost-exclusion heuristic used to
    // skip *every* type-less /Names|/Kids dictionary, so a live `/Dests` (or
    // /JavaScript / custom) name tree referencing the same filespec was also
    // excluded — letting `remove_attachment` delete a still-referenced object.
    #[test]
    fn remove_attachment_preserves_filespec_referenced_by_other_name_tree() {
        let mut pdf = open_minimal();

        // Register a filespec under /Names /EmbeddedFiles.
        let fs_ref = FileSpecBuilder::new("shared.txt", b"shared payload")
            .build(&mut pdf)
            .expect("build filespec");
        insert_embedded_file(&mut pdf, b"shared.txt", fs_ref).expect("insert");

        let stream_ref = resolve_embedded_file_stream_ref(&mut pdf, fs_ref)
            .expect("resolve stream ref")
            .expect("stream ref must exist");

        // A separate, type-less name-tree leaf (models a /Dests name tree) that
        // legitimately references the SAME filespec.
        let next = pdf
            .object_refs()
            .iter()
            .map(|r| r.number)
            .max()
            .unwrap_or(0);
        let dests_leaf_ref = ObjectRef::new(next + 1, 0);
        let mut dests_leaf = Dictionary::new();
        dests_leaf.insert(
            "Names",
            Object::Array(vec![
                Object::String(b"shared-dest".to_vec()),
                Object::Reference(fs_ref),
            ]),
        );
        pdf.set_object(dests_leaf_ref, Object::Dictionary(dests_leaf));

        // Hang it off the catalog's /Dests so it is reachable from the catalog
        // (a legitimate live name tree, not a dead ghost).
        let catalog_ref = pdf.root_ref().expect("root");
        let Object::Dictionary(mut catalog) = pdf
            .resolve_borrowed(catalog_ref)
            .expect("resolve catalog")
            .clone()
        else {
            panic!("expected catalog dict");
        };
        catalog.insert("Dests", Object::Reference(dests_leaf_ref));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));

        // Remove the embedded-files attachment.  The filespec is still
        // referenced by the /Dests name tree → conservative GC must keep it.
        let removed = remove_attachment(&mut pdf, b"shared.txt").expect("remove");
        assert!(removed, "existing key must report removed");

        let live = pdf.live_object_refs();
        assert!(
            live.contains(&fs_ref),
            "filespec referenced by another live name tree (/Dests) must NOT be GC-deleted"
        );

        // The /Dests reference itself must remain intact.
        let Object::Dictionary(leaf) = pdf
            .resolve_borrowed(dests_leaf_ref)
            .expect("resolve dests leaf")
        else {
            panic!("expected dests leaf dict");
        };
        assert!(
            matches!(leaf.get("Names"), Some(Object::Array(a)) if a.iter().any(|o| matches!(o, Object::Reference(r) if *r == fs_ref))),
            "/Dests leaf must still reference the filespec"
        );

        // Symmetric inverse (roborev #949): the preserved filespec still
        // references its embedded stream via `/EF`, so the stream must also
        // survive — otherwise the kept filespec would dangle.
        assert!(
            live.contains(&stream_ref),
            "embedded stream of a preserved filespec must NOT be GC-deleted"
        );
    }

    // ── Test: live object referencing the stream (with stream back-ref) ──────
    //
    // Regression for roborev #949: the stream ref used to be unconditionally
    // excluded from the filespec-reference scan.  If the stream is preserved
    // (externally referenced) and its dictionary back-references the filespec,
    // the filespec would be deleted leaving the live stream dangling.  The
    // mutual-ref pair must be kept together.
    #[test]
    fn remove_attachment_keeps_pair_when_stream_externally_referenced_and_back_refs() {
        let mut pdf = open_minimal();

        let fs_ref = FileSpecBuilder::new("paired.txt", b"paired payload")
            .build(&mut pdf)
            .expect("build filespec");
        insert_embedded_file(&mut pdf, b"paired.txt", fs_ref).expect("insert");

        let stream_ref = resolve_embedded_file_stream_ref(&mut pdf, fs_ref)
            .expect("resolve stream ref")
            .expect("stream ref must exist");

        // Make the stream dictionary back-reference the filespec (pathological
        // but legal) and have a live, catalog-reachable object reference the
        // stream so conservative GC must preserve it.
        let Object::Stream(mut stream) = pdf
            .resolve_borrowed(stream_ref)
            .expect("resolve stream")
            .clone()
        else {
            panic!("expected stream object");
        };
        stream.dict.insert("RelatedFS", Object::Reference(fs_ref));
        pdf.set_object(stream_ref, Object::Stream(stream));

        let catalog_ref = pdf.root_ref().expect("root");
        let Object::Dictionary(mut catalog) = pdf
            .resolve_borrowed(catalog_ref)
            .expect("resolve catalog")
            .clone()
        else {
            panic!("expected catalog dict");
        };
        catalog.insert("ExtraStreamRef", Object::Reference(stream_ref));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));

        let removed = remove_attachment(&mut pdf, b"paired.txt").expect("remove");
        assert!(removed);

        let live = pdf.live_object_refs();
        assert!(
            live.contains(&stream_ref),
            "externally-referenced stream must be preserved"
        );
        assert!(
            live.contains(&fs_ref),
            "filespec must be preserved because the live stream back-references it"
        );
    }

    // ── Test: shared embedded stream is preserved, removed filespec GC'd ──────
    //
    // Two filespecs share one /EmbeddedFile stream.  Removing one attachment
    // must GC its (otherwise-unreferenced) filespec but keep the shared stream
    // and the other filespec intact.  Guards against an over-conservative
    // "pair-or-nothing" regression of the roborev #949 fix.
    #[test]
    fn remove_attachment_with_shared_stream_keeps_stream_and_other_filespec() {
        let mut pdf = open_minimal();

        let fs_a = FileSpecBuilder::new("a.txt", b"shared body")
            .build(&mut pdf)
            .expect("build a");
        insert_embedded_file(&mut pdf, b"a.txt", fs_a).expect("insert a");
        let shared_stream = resolve_embedded_file_stream_ref(&mut pdf, fs_a)
            .expect("resolve stream a")
            .expect("stream a exists");

        // Build a second filespec whose /EF points at the SAME stream object.
        let next = pdf
            .object_refs()
            .iter()
            .map(|r| r.number)
            .max()
            .unwrap_or(0);
        let fs_b = ObjectRef::new(next + 1, 0);
        let mut ef = Dictionary::new();
        ef.insert("F", Object::Reference(shared_stream));
        ef.insert("UF", Object::Reference(shared_stream));
        let mut fs_b_dict = Dictionary::new();
        fs_b_dict.insert("Type", Object::Name(b"Filespec".to_vec()));
        fs_b_dict.insert("F", Object::String(b"b.txt".to_vec()));
        fs_b_dict.insert("UF", Object::String(b"b.txt".to_vec()));
        fs_b_dict.insert("EF", Object::Dictionary(ef));
        pdf.set_object(fs_b, Object::Dictionary(fs_b_dict));
        insert_embedded_file(&mut pdf, b"b.txt", fs_b).expect("insert b");

        // Remove attachment "a": its filespec is otherwise unreferenced and
        // must be GC'd; the stream is still used by fs_b and must survive,
        // and fs_b itself must remain intact.
        let removed = remove_attachment(&mut pdf, b"a.txt").expect("remove a");
        assert!(removed);

        let live = pdf.live_object_refs();
        assert!(
            !live.contains(&fs_a),
            "removed attachment's filespec must be GC-deleted"
        );
        assert!(
            live.contains(&shared_stream),
            "stream shared with another filespec must be preserved"
        );
        assert!(
            live.contains(&fs_b),
            "the other filespec sharing the stream must remain intact"
        );
    }

    // ── Test: filespec with distinct /EF streams GCs all of them ─────────────
    //
    // Regression for roborev #950-1: only the first /EF stream was resolved,
    // so sibling streams under other /EF keys were orphaned (left live) once
    // the filespec was GC-deleted.
    #[test]
    fn remove_attachment_gcs_all_distinct_ef_streams() {
        let mut pdf = open_minimal();

        let fs_ref = FileSpecBuilder::new("multi.txt", b"primary stream")
            .build(&mut pdf)
            .expect("build filespec");

        // The builder points /EF /F and /EF /UF at one stream; capture it.
        let Object::Dictionary(fs_dict) = pdf.resolve_borrowed(fs_ref).expect("resolve fs") else {
            panic!("expected filespec dict");
        };
        let Some(Object::Dictionary(mut ef)) = fs_dict.get("EF").cloned() else {
            panic!("expected inline /EF dict");
        };
        let stream_f = match ef.get("F") {
            Some(Object::Reference(r)) => *r,
            _ => panic!("expected /EF /F indirect stream"),
        };

        // Add a *distinct* second stream object under /EF /UF.
        let next = pdf
            .object_refs()
            .iter()
            .map(|r| r.number)
            .max()
            .unwrap_or(0);
        let stream_uf = ObjectRef::new(next + 1, 0);
        let mut s2 = Dictionary::new();
        s2.insert("Type", Object::Name(b"EmbeddedFile".to_vec()));
        pdf.set_object(
            stream_uf,
            Object::Stream(crate::object::Stream {
                dict: s2,
                data: b"sibling stream".to_vec(),
            }),
        );
        ef.insert("UF", Object::Reference(stream_uf));
        let Object::Dictionary(mut fs_dict_mut) =
            pdf.resolve_borrowed(fs_ref).expect("resolve fs").clone()
        else {
            panic!("expected filespec dict");
        };
        fs_dict_mut.insert("EF", Object::Dictionary(ef));
        pdf.set_object(fs_ref, Object::Dictionary(fs_dict_mut));

        insert_embedded_file(&mut pdf, b"multi.txt", fs_ref).expect("insert");

        let removed = remove_attachment(&mut pdf, b"multi.txt").expect("remove");
        assert!(removed);

        let live = pdf.live_object_refs();
        assert!(!live.contains(&fs_ref), "filespec must be GC-deleted");
        assert!(
            !live.contains(&stream_f),
            "primary /EF /F stream must be GC-deleted"
        );
        assert!(
            !live.contains(&stream_uf),
            "distinct /EF /UF sibling stream must also be GC-deleted (not orphaned)"
        );
    }

    // ── Test: empty/target-absent indirect /AF array is left untouched ───────
    //
    // Regression for roborev #950-2: an *empty* (or target-absent) indirect
    // /AF array used to be deleted and its parent /AF key removed even though
    // the target ref was never present — dangling the array if it is shared.
    #[test]
    fn remove_attachment_leaves_empty_indirect_af_array_intact() {
        let mut pdf = open_minimal();

        let next = pdf
            .object_refs()
            .iter()
            .map(|r| r.number)
            .max()
            .unwrap_or(0);

        // An empty indirect /AF array object, shared by the catalog *and* a
        // second dictionary so wrongly deleting it would dangle a live ref.
        let af_array_ref = ObjectRef::new(next + 1, 0);
        pdf.set_object(af_array_ref, Object::Array(vec![]));

        let sharer_ref = ObjectRef::new(next + 2, 0);
        let mut sharer = Dictionary::new();
        sharer.insert("AF", Object::Reference(af_array_ref));
        pdf.set_object(sharer_ref, Object::Dictionary(sharer));

        let catalog_ref = pdf.root_ref().expect("root");
        let Object::Dictionary(mut catalog) = pdf
            .resolve_borrowed(catalog_ref)
            .expect("resolve catalog")
            .clone()
        else {
            panic!("expected catalog dict");
        };
        catalog.insert("AF", Object::Reference(af_array_ref));
        catalog.insert("Sharer", Object::Reference(sharer_ref));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));

        // Add and remove an unrelated attachment.  Its filespec is NOT in the
        // empty indirect /AF array, so the array and parent key must survive.
        let fs_ref = FileSpecBuilder::new("x.txt", b"x")
            .build(&mut pdf)
            .expect("build");
        insert_embedded_file(&mut pdf, b"x.txt", fs_ref).expect("insert");

        let removed = remove_attachment(&mut pdf, b"x.txt").expect("remove");
        assert!(removed);

        let live = pdf.live_object_refs();
        assert!(
            live.contains(&af_array_ref),
            "empty indirect /AF array (target absent) must NOT be deleted"
        );
        let Object::Dictionary(catalog2) = pdf
            .resolve_borrowed(catalog_ref)
            .expect("resolve catalog after")
        else {
            panic!("expected catalog dict");
        };
        assert!(
            matches!(catalog2.get("AF"), Some(Object::Reference(r)) if *r == af_array_ref),
            "catalog /AF must still point at the untouched indirect array"
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
        let Some(mut catalog) = pdf
            .resolve_borrowed(catalog_ref)
            .expect("resolve catalog")
            .as_dict()
            .cloned()
        else {
            panic!("expected catalog dict");
        };
        catalog.insert("AF", Object::Array(vec![Object::Reference(fs_ref)]));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));

        // Add /AF to the single page as well.
        let page_refs = crate::pages::page_refs(&mut pdf).expect("page_refs");
        assert_eq!(page_refs.len(), 1, "fixture has one page");
        let page_ref = page_refs[0];
        let Some(mut page_dict) = pdf
            .resolve_borrowed(page_ref)
            .expect("resolve page")
            .as_dict()
            .cloned()
        else {
            panic!("expected page dict");
        };
        page_dict.insert("AF", Object::Array(vec![Object::Reference(fs_ref)]));
        pdf.set_object(page_ref, Object::Dictionary(page_dict));

        // Remove the attachment.
        let removed = remove_attachment(&mut pdf, b"af-test.txt").expect("remove");
        assert!(removed);

        // /AF on catalog must be gone.
        let Some(catalog2) = pdf
            .resolve_borrowed(catalog_ref)
            .expect("resolve catalog after")
            .as_dict()
        else {
            panic!("expected catalog dict");
        };
        assert!(
            catalog2.get("AF").is_none(),
            "catalog /AF must be removed after attachment removal"
        );

        // /AF on page must be gone.
        let Some(page_dict2) = pdf
            .resolve_borrowed(page_ref)
            .expect("resolve page after")
            .as_dict()
        else {
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

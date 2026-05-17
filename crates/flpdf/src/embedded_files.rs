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

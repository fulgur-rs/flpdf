//! Read-only enumeration of `/Names /EmbeddedFiles` name-tree entries.
//!
//! Walks the catalog's `/Names /EmbeddedFiles` name tree (ISO 32000-1 §7.9.6
//! + §7.11) and returns an ordered list of `(name_key, filespec_ref)` pairs.
//!
//! The result is in depth-first, key-ascending order as mandated by the spec
//! requirement that name trees be sorted by key.
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
//! results in an empty list (`Ok(vec![])`) rather than an error.  Only I/O
//! errors from [`Pdf::resolve`] are propagated.
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
//! use flpdf::{embedded_files, Pdf};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("with-attachments.pdf")?))?;
//! let entries = embedded_files::list_embedded_files(&mut pdf)?;
//! for (name, filespec_ref) in &entries {
//!     println!("{}: {}", String::from_utf8_lossy(name), filespec_ref);
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crate::{Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

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
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`].
pub fn list_embedded_files<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Vec<(Vec<u8>, ObjectRef)>> {
    list_embedded_files_with_max_depth(pdf, DEFAULT_MAX_EMBEDDED_FILES_DEPTH)
}

/// Like [`list_embedded_files`] but with a caller-supplied depth limit.
///
/// The depth limit guards against maliciously or accidentally cyclic `/Kids`
/// references.  Exceeding the limit returns an error rather than panicking.
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

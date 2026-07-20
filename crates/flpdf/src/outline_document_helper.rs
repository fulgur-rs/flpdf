//! High-level outline (`/Outlines`) document helper.
//!
//! [`OutlineDocumentHelper`] wraps a `&mut Pdf<R>` and exposes a cycle-safe,
//! iterable handle over the document outline (bookmark) tree, mirroring qpdf's
//! `QPDFOutlineDocumentHelper`. It materializes the tree into owned
//! [`OutlineNode`]s; navigation (`children`, `parent`, `count`, `dest`) lives on
//! each node, mirroring `QPDFOutlineObjectHelper`.
//!
//! # Example
//!
//! ```no_run
//! use flpdf::Pdf;
//! use std::io::Cursor;
//!
//! # fn f(bytes: Vec<u8>) -> flpdf::Result<()> {
//! let mut pdf = Pdf::open(Cursor::new(bytes))?;
//! if pdf.outline().has_outlines()? {
//!     pdf.outline().walk(|node, depth| {
//!         println!("{:indent$}{}", "", node.title, indent = depth * 2);
//!     })?;
//! }
//! # Ok(())
//! # }
//! ```

use crate::name_number_tree::read_name_tree;
use crate::{Diagnostic, Diagnostics, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

/// Default recursion limit for outline materialization. Matches
/// [`crate::outline::DEFAULT_MAX_OUTLINE_DEPTH`]. True unbounded/iterative deep
/// walking (1000+ levels, cycle diagnostics) is not currently supported.
pub const DEFAULT_MAX_OUTLINE_DEPTH: usize = crate::outline::DEFAULT_MAX_OUTLINE_DEPTH;

/// Indirection/`/D` nesting bound when resolving a destination. Mirrors the
/// constant in `outline_dest_remap`. Only exists to make malformed/cyclic
/// `/D` structures terminate instead of overflowing the stack.
const MAX_DEST_RESOLVE_DEPTH: usize = 64;

/// One materialized node of the outline tree (a bookmark).
///
/// Mirrors qpdf's `QPDFOutlineObjectHelper`. `children` are the resolved
/// `/First`->`/Next` chain; `parent` is the owning item's ref (`None` for
/// top-level items). `count` is the raw `/Count` value
/// (0 when absent), whose sign indicates open/closed per ISO 32000-1 section
/// 12.3.3.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineNode {
    /// Object ref of this outline item dictionary.
    pub object_ref: ObjectRef,
    /// Zero for top-level items, increasing per nesting level.
    pub depth: usize,
    /// `/Title` decoded with `from_utf8_lossy`; empty string when absent.
    /// Resolves one level of indirection (an indirect `/Title` ref).
    pub title: String,
    /// Raw `/Count` value; `0` when absent.
    pub count: i64,
    /// Parent item ref; `None` for top-level items.
    pub parent: Option<ObjectRef>,
    /// Resolved explicit destination (`/Dest`, or a `/A` GoTo action's `/D`),
    /// or `None` when absent or a still-unresolved named destination.
    pub dest: Option<Dest>,
    /// Child nodes in `/First`->`/Next` order.
    pub children: Vec<OutlineNode>,
}

/// A resolved explicit destination, e.g. `[pageRef /Fit ...]`. Mirrors the
/// array form qpdf `getDest` yields after resolving `/Dest`, `/A /GoTo /D`, and
/// named destinations.
#[derive(Debug, Clone, PartialEq)]
pub struct Dest {
    /// The explicit destination array. Element 0 is normally the page ref.
    pub array: Vec<Object>,
}

impl Dest {
    /// The destination page ref (array element 0), if it is an indirect ref.
    /// Mirrors qpdf `getDestPage`.
    pub fn page(&self) -> Option<ObjectRef> {
        self.array.first().and_then(Object::as_ref_id)
    }
}

/// High-level outline helper for a document. See module docs.
pub struct OutlineDocumentHelper<'a, R: Read + Seek> {
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> OutlineDocumentHelper<'a, R> {
    /// Wrap a document for outline access. Prefer [`Pdf::outline`].
    pub fn new(pdf: &'a mut Pdf<R>) -> Self {
        Self { pdf }
    }

    /// Return `true` if the catalog has an `/Outlines` dictionary with at least
    /// one top-level item (a resolvable `/First`). Mirrors qpdf `hasOutlines`.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the catalog and `/Outlines`
    /// objects (for example I/O or parse failures surfaced by [`Pdf::resolve_borrowed`]).
    pub fn has_outlines(&mut self) -> Result<bool> {
        Ok(self.outline_root_first()?.is_some())
    }

    /// Resolve the catalog `/Outlines` dict's `/First` child ref, if any.
    fn outline_root_first(&mut self) -> Result<Option<ObjectRef>> {
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let Object::Dictionary(catalog) = self.pdf.resolve_borrowed(catalog_ref)? else {
            return Ok(None);
        };
        let Some(outlines_ref) = catalog.get_ref("Outlines") else {
            return Ok(None);
        };
        let Object::Dictionary(root) = self.pdf.resolve_borrowed(outlines_ref)? else {
            return Ok(None);
        };
        Ok(root.get_ref("First"))
    }

    /// Materialize and return the top-level outline nodes (qpdf
    /// `getTopLevelOutlines`). "root" is this top-level vector; the `/Outlines`
    /// dict itself is not a navigable item and is not wrapped.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Unsupported`] if the outline nesting depth exceeds
    /// [`DEFAULT_MAX_OUTLINE_DEPTH`]. Propagates any error from resolving outline
    /// objects (for example I/O or parse failures surfaced by [`Pdf::resolve`]).
    pub fn get_root(&mut self) -> Result<Vec<OutlineNode>> {
        self.get_root_with_max_depth(DEFAULT_MAX_OUTLINE_DEPTH)
    }

    /// Like [`get_root`](Self::get_root) with a caller-supplied recursion limit.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Unsupported`] if the outline nesting depth exceeds
    /// `max_depth`. Propagates any error from resolving outline objects (for
    /// example I/O or parse failures surfaced by [`Pdf::resolve`]).
    pub fn get_root_with_max_depth(&mut self, max_depth: usize) -> Result<Vec<OutlineNode>> {
        let Some(first) = self.outline_root_first()? else {
            return Ok(Vec::new());
        };
        let mut visited = BTreeSet::new();
        self.build_siblings(first, 0, None, &mut visited, max_depth)
    }

    /// Build a `/First`->`/Next` sibling chain into owned nodes.
    fn build_siblings(
        &mut self,
        start: ObjectRef,
        depth: usize,
        parent: Option<ObjectRef>,
        visited: &mut BTreeSet<ObjectRef>,
        max_depth: usize,
    ) -> Result<Vec<OutlineNode>> {
        if depth >= max_depth {
            return Err(crate::Error::Unsupported(format!(
                "outline depth exceeds maximum of {max_depth} at {start}"
            )));
        }
        let mut nodes = Vec::new();
        let mut current = Some(start);
        while let Some(current_ref) = current {
            if !visited.insert(current_ref) {
                break; // cycle - stop this chain
            }
            let Object::Dictionary(dict) = self.pdf.resolve_borrowed(current_ref)? else {
                break;
            };
            // IMPORTANT (borrow order): `dict` borrows `self.pdf` (it is a
            // `resolve_borrowed` reference). Extract EVERY value we need into
            // owned locals here, ending the `dict` borrow, BEFORE any
            // `self.pdf.resolve(...)` call below - otherwise the borrow checker
            // rejects it. A later task adds `dest_src`/`action_src` here.
            let first = dict.get_ref("First");
            let next = dict.get_ref("Next");
            let title_src = dict.get("Title").cloned();
            let count_src = dict.get("Count").cloned();
            let dest_src = dict.get("Dest").cloned();
            let action_src = dict.get("A").cloned();
            // `dict` (and thus the &mut self.pdf borrow) is no longer used past
            // this point - owned values only from here on.
            let title = resolve_title(self.pdf, title_src)?;
            let count = resolve_int(self.pdf, count_src)?.unwrap_or(0);
            let dest = self.resolve_node_dest(dest_src, action_src)?;

            let children = match first {
                Some(first) => {
                    self.build_siblings(first, depth + 1, Some(current_ref), visited, max_depth)?
                }
                None => Vec::new(),
            };

            nodes.push(OutlineNode {
                object_ref: current_ref,
                depth,
                title,
                count,
                parent,
                dest,
                children,
            });
            current = next;
        }
        Ok(nodes)
    }

    /// Resolve a node's destination from `/Dest`, else a `/A` GoTo action's `/D`.
    /// Named/string destinations are resolved in a later task (return `None` here).
    fn resolve_node_dest(
        &mut self,
        dest: Option<Object>,
        action: Option<Object>,
    ) -> Result<Option<Dest>> {
        if let Some(d) = dest {
            if let Some(found) = self.dest_from_value(&d, MAX_DEST_RESOLVE_DEPTH)? {
                return Ok(Some(found));
            }
        }
        if let Some(a) = action {
            let action_obj = match a {
                Object::Reference(r) => self.pdf.resolve(r)?,
                other => other,
            };
            if let Some(adict) = action_obj.as_dict() {
                let is_goto = matches!(adict.get("S"), Some(Object::Name(n)) if n == b"GoTo");
                if is_goto {
                    if let Some(d) = adict.get("D").cloned() {
                        if let Some(found) = self.dest_from_value(&d, MAX_DEST_RESOLVE_DEPTH)? {
                            return Ok(Some(found));
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    /// Resolve a destination value (array / indirect / dict `/D`) to a [`Dest`].
    /// Named (`Name`/`String`) destinations are resolved against the catalog name
    /// tree / legacy `/Dests` dict via [`Self::resolve_named_dest`].
    fn dest_from_value(&mut self, value: &Object, depth: usize) -> Result<Option<Dest>> {
        if depth == 0 {
            return Ok(None);
        }
        match value {
            Object::Array(arr) => Ok(Some(Dest { array: arr.clone() })),
            Object::Reference(r) => {
                let concrete = self.pdf.resolve(*r)?;
                self.dest_from_value(&concrete, depth - 1)
            }
            Object::Dictionary(d) => match d.get("D").cloned() {
                Some(inner) => self.dest_from_value(&inner, depth - 1),
                None => Ok(None),
            },
            Object::Name(name) => self.resolve_named_dest(name, depth),
            Object::String(name) => self.resolve_named_dest(name, depth),
            _ => Ok(None),
        }
    }

    /// Resolve a named destination `name` to an explicit [`Dest`].
    ///
    /// Tries the modern catalog `/Names`->`/Dests` name tree first (PDF 1.2),
    /// then the legacy catalog `/Dests` dictionary (PDF 1.1). A name-tree or
    /// `/Dests` value may be the dest array directly or a `<< /D array >>` dict.
    ///
    /// `depth` is the remaining indirection/`/D` budget threaded from
    /// [`Self::dest_from_value`]; the post-lookup value is resolved with
    /// `depth - 1` so a cyclic named mapping (e.g. legacy `/Dests` `/a -> /b`,
    /// `/b -> /a`) strictly decreases the budget and terminates at the bound
    /// instead of overflowing the stack.
    fn resolve_named_dest(&mut self, name: &[u8], depth: usize) -> Result<Option<Dest>> {
        // 1. Modern: catalog /Names /Dests name tree (PDF 1.2+). /Names may be
        //    inline or an indirect ref; catalog_value handles both.
        if let Some(Object::Dictionary(mut names)) = self.catalog_value("Names")? {
            if let Some(dests_root) = names.remove("Dests") {
                let entries = read_name_tree(
                    self.pdf,
                    dests_root,
                    |_pdf, value| Ok(Some(value)),
                    DEFAULT_MAX_OUTLINE_DEPTH,
                )?;
                // Re-reads the whole name tree per named hop; acceptable because
                // each hop strictly decreases `depth` (no visited set needed).
                for (key, value) in entries {
                    if key.as_slice() == name {
                        return self.dest_from_value(&value, depth - 1);
                    }
                }
            }
        }
        // 2. Legacy: catalog /Dests dict (PDF 1.1).
        if let Some(Object::Dictionary(mut dests)) = self.catalog_value("Dests")? {
            if let Some(value) = dests.remove(name) {
                return self.dest_from_value(&value, depth - 1);
            }
        }
        Ok(None)
    }

    /// Resolve a catalog key's value to an owned object, following one level of
    /// indirection. Returns the value whether the catalog stores it as an
    /// indirect reference or as a direct (inline) object — so an inline
    /// `/Names`/`/Dests` dictionary is handled as well as the reference form.
    fn catalog_value(&mut self, key: &str) -> Result<Option<Object>> {
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let Object::Dictionary(catalog) = self.pdf.resolve_borrowed(catalog_ref)? else {
            return Ok(None);
        };
        let Some(value) = catalog.get(key).cloned() else {
            return Ok(None);
        };
        match value {
            Object::Reference(r) => Ok(Some(self.pdf.resolve(r)?)),
            other => Ok(Some(other)),
        }
    }

    /// Read every entry of the catalog's legacy `/Dests` dictionary (ISO
    /// 32000-1 §7.11.4; the PDF 1.1 named-destination dictionary, superseded
    /// — but not replaced — by the `/Names /Dests` name tree added in PDF
    /// 1.2). `/Dests` may be an indirect reference or a direct dictionary on
    /// the catalog; both forms are read via the same resolution [`Pdf::outline`]
    /// uses for named-destination lookup.
    ///
    /// Entries come back in the dictionary's lexicographic key order
    /// (matching [`crate::Dictionary::iter`], which is not necessarily the
    /// order the entries were declared in the source file). A value that
    /// cannot be resolved to an explicit destination array (for example a
    /// malformed non-array, non-reference value) yields `None` for that
    /// entry rather than dropping the name, so a caller can still see every
    /// declared name.
    ///
    /// Only the legacy dictionary is enumerated here; the `/Names /Dests`
    /// name tree is a separate structure with its own accessor.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the catalog or the `/Dests`
    /// dictionary/value objects (for example I/O or parse failures surfaced
    /// by [`Pdf::resolve`]).
    pub fn legacy_dests(&mut self) -> Result<Vec<(Vec<u8>, Option<Dest>)>> {
        // `catalog_value` resolves a single level of indirection but stops
        // short of a multi-hop holder chain (catalog /Dests -> r1 -> r2 ->
        // dict — a legal shape). Without this follow-through the reader
        // silently returns empty, and `check_legacy_dests` misses every
        // dangling target it is supposed to flag.
        let dests_obj = match self.catalog_value("Dests")? {
            Some(value @ Object::Reference(_)) => {
                crate::ref_chain::resolve_ref_chain(self.pdf, &value)?.0
            }
            Some(other) => other,
            None => return Ok(Vec::new()),
        };
        let Object::Dictionary(dests) = dests_obj else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for (name, value) in dests.iter() {
            let dest = self.dest_from_value(value, MAX_DEST_RESOLVE_DEPTH)?;
            out.push((name.to_vec(), dest));
        }
        Ok(out)
    }

    /// Pre-order iterator over every materialized node (owned). Each yielded
    /// node has its `children` cleared — the flattened view is linear and
    /// `depth` conveys structure; use [`get_root`](Self::get_root) or
    /// [`walk`](Self::walk) when you need populated `children`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Unsupported`] if the outline nesting depth exceeds
    /// [`DEFAULT_MAX_OUTLINE_DEPTH`]. Propagates any error from resolving outline
    /// objects (for example I/O or parse failures surfaced by [`Pdf::resolve`]).
    pub fn iter(&mut self) -> Result<impl Iterator<Item = OutlineNode>> {
        let roots = self.get_root()?;
        let mut flat = Vec::new();
        for node in roots {
            flatten_preorder(node, &mut flat);
        }
        Ok(flat.into_iter())
    }

    /// Visit every node pre-order, passing `(node, depth)` to `visitor`. The
    /// visited nodes have populated `children`. Mirrors a qpdf outline walk.
    ///
    /// For a runnable walkthrough see `examples/walk_outline.rs`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Unsupported`] if the outline nesting depth exceeds
    /// [`DEFAULT_MAX_OUTLINE_DEPTH`]. Propagates any error from resolving outline
    /// objects (for example I/O or parse failures surfaced by [`Pdf::resolve`]).
    pub fn walk<F: FnMut(&OutlineNode, usize)>(&mut self, mut visitor: F) -> Result<()> {
        let roots = self.get_root()?;
        for node in &roots {
            walk_node(node, &mut visitor);
        }
        Ok(())
    }
}

impl<R: Read + Seek> Pdf<R> {
    /// Return a high-level outline helper for this document.
    pub fn outline(&mut self) -> OutlineDocumentHelper<'_, R> {
        OutlineDocumentHelper::new(self)
    }
}

/// Validate the catalog's legacy `/Dests` dictionary
/// ([`OutlineDocumentHelper::legacy_dests`]): push a warning [`Diagnostic`]
/// for every entry whose destination's target page reference is not a page
/// currently reachable from the document's `/Pages` tree — a dangling
/// reference, a reference to a non-`/Page` object, or a page a prior edit
/// removed. A missing target is reported, not treated as document
/// corruption: it never turns into an `Err` on its own.
///
/// # Errors
///
/// Propagates any error from resolving the catalog or the `/Dests`
/// dictionary/value objects. A failure to enumerate the document's page tree
/// (for example a missing `/Pages` entry) is downgraded to a warning
/// [`Diagnostic`] instead, so the caller still receives a report.
///
/// # Examples
///
/// ```no_run
/// use flpdf::{check_legacy_dests, Pdf};
/// use std::fs::File;
/// use std::io::BufReader;
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
/// for diagnostic in check_legacy_dests(&mut pdf)?.entries() {
///     println!("{diagnostic:?}");
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn check_legacy_dests<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Diagnostics> {
    let entries = OutlineDocumentHelper::new(pdf).legacy_dests()?;
    let mut diagnostics = Diagnostics::default();
    if entries.is_empty() {
        return Ok(diagnostics);
    }

    // Every `/Dests` entry with a resolvable page ref is what the page-tree
    // walk below is validating; if not a single entry has one (all values
    // are named/string/unresolved), the walk cannot possibly flag anything
    // — skip the O(N) `page_refs(pdf)` traversal entirely.
    let has_resolvable_page_ref = entries
        .iter()
        .any(|(_, dest)| dest.as_ref().and_then(|d| d.page()).is_some());
    if !has_resolvable_page_ref {
        return Ok(diagnostics);
    }

    let live_pages: BTreeSet<ObjectRef> = match crate::pages::page_refs(pdf) {
        Ok(refs) => refs.into_iter().collect(),
        Err(error) => {
            diagnostics.push(Diagnostic::warning(
                format!("could not enumerate pages to validate /Dests targets: {error}"),
                None,
            ));
            return Ok(diagnostics);
        }
    };

    for (name, dest) in entries {
        let Some(dest) = dest else { continue };
        let Some(page_ref_raw) = dest.page() else {
            continue;
        };
        // Normalise through any holder chain: `/h [30 0 R /Fit]` with
        // `30 0 obj 3 0 R` should compare against the terminal page ref
        // `3 0 R`, not the intermediate `30 0 R`, otherwise a legitimately
        // live target is falsely flagged as dangling.
        let page_ref = resolve_page_ref_through_holders(pdf, page_ref_raw);
        if !live_pages.contains(&page_ref) {
            diagnostics.push(Diagnostic::warning(
                format!(
                    "named destination \"{}\" targets {page_ref}, which is not a page in the document",
                    String::from_utf8_lossy(&name)
                ),
                None,
            ));
        }
    }
    Ok(diagnostics)
}

/// Decode an outline `/Title`, resolving one level of indirection (review rule 2).
/// qpdf yields an empty string when absent or not a (resolved) string.
fn resolve_title<R: Read + Seek>(pdf: &mut Pdf<R>, value: Option<Object>) -> Result<String> {
    let resolved = match value {
        Some(Object::Reference(r)) => Some(pdf.resolve(r)?),
        other => other,
    };
    Ok(match resolved {
        Some(Object::String(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
        _ => String::new(),
    })
}

/// Push `node` (with `children` taken/emptied) then its descendants, pre-order.
fn flatten_preorder(mut node: OutlineNode, out: &mut Vec<OutlineNode>) {
    let children = std::mem::take(&mut node.children);
    out.push(node);
    for child in children {
        flatten_preorder(child, out);
    }
}

/// Invoke `visitor(node, node.depth)` then recurse into children, pre-order.
fn walk_node<F: FnMut(&OutlineNode, usize)>(node: &OutlineNode, visitor: &mut F) {
    visitor(node, node.depth);
    for child in &node.children {
        walk_node(child, visitor);
    }
}

/// Resolve one level of indirection and read an integer (review rule 2/3).
fn resolve_int<R: Read + Seek>(pdf: &mut Pdf<R>, value: Option<Object>) -> Result<Option<i64>> {
    match value {
        Some(Object::Reference(r)) => Ok(pdf.resolve(r)?.as_integer()),
        Some(other) => Ok(other.as_integer()),
        None => Ok(None),
    }
}

/// Walk an object reference through any indirect-holder chain (obj N is a
/// bare `Reference(M)`, obj M is a `Reference(K)`, …) and return the
/// terminal ObjectRef. Stops at the first non-`Reference` value or after
/// [`MAX_DEST_RESOLVE_DEPTH`] hops (cycle/deep-chain safety).
///
/// Used by `check_legacy_dests` and `check_name_tree_dests` to compare a
/// destination's page against the live page set: the destination's own
/// array element may be `ref → ref → page`, but `page_refs` returns only
/// terminals, so a naive `==` check would false-flag a live target as
/// dangling.
fn resolve_page_ref_through_holders<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    start: ObjectRef,
) -> ObjectRef {
    let mut current = start;
    for _ in 0..MAX_DEST_RESOLVE_DEPTH {
        match pdf.resolve(current) {
            Ok(Object::Reference(next)) if next != current => current = next,
            _ => return current,
        }
    }
    current
}

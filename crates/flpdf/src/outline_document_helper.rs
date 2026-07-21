//! High-level outline (`/Outlines`) document helper.
//!
//! [`OutlineDocumentHelper`] wraps a `&mut Pdf<R>` and exposes a cycle-safe,
//! iterable handle over the document outline (bookmark) tree, mirroring qpdf's
//! `QPDFOutlineDocumentHelper`. It materializes the tree into owned
//! [`OutlineNode`]s; navigation (`children`, `parent`, `count`, `dest`) lives on
//! each node, mirroring `QPDFOutlineObjectHelper`.
//!
//! [`OutlineDocumentHelper::get_root`] and the traversals built on it
//! ([`OutlineDocumentHelper::iter`], [`OutlineDocumentHelper::walk`]) walk the
//! `/First`/`/Next` chain iteratively rather than by native recursion, so a
//! document with tens of thousands of nested outline levels cannot overflow
//! the call stack; a shared visited set still cuts short any `/Next` or
//! `/First` cycle. [`check_outline_links`] separately validates that every
//! item's `/Parent` and `/Prev` actually match the tree that chain describes.
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

/// Depth bound shared by [`OutlineDocumentHelper::prune_se`] and the
/// `/Names`/`Dests` name-tree readers ([`OutlineDocumentHelper::name_tree_dests`]).
/// Matches [`crate::outline::DEFAULT_MAX_OUTLINE_DEPTH`]. Those traversals
/// still recurse natively, so this bound is kept modest to protect the call
/// stack. [`OutlineDocumentHelper::get_root`] and the traversals built on it
/// walk iteratively instead and use the much larger [`MAX_OUTLINE_WALK_DEPTH`].
pub const DEFAULT_MAX_OUTLINE_DEPTH: usize = crate::outline::DEFAULT_MAX_OUTLINE_DEPTH;

/// Depth bound for [`OutlineDocumentHelper::get_root`] (and the `iter`/`walk`
/// traversals built on it) and for [`check_outline_links`]. The walk itself
/// is iterative; the cap primarily bounds the returned [`OutlineNode`] tree
/// so that its automatically-derived `Clone`/`Debug`/`PartialEq` (which
/// recurse once per nesting level) and its default `Drop` cannot stack-
/// overflow. 5,000 levels comfortably covers any legitimate document while
/// leaving substantial headroom for the recursive derives.
pub const MAX_OUTLINE_WALK_DEPTH: usize = 5_000;

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
    /// qpdf `getDest()` result; `Object::Null` means no resolved destination.
    pub dest: Object,
    /// The `/SE` (structure-element) link: an indirect reference to a node in
    /// the document's structure tree (ISO 32000-2 section 12.3.3, table 151),
    /// or `None` when `/SE` is absent. Per spec `/SE` shall be an indirect
    /// reference; a direct (non-reference) `/SE` value is malformed and is
    /// also read as `None`.
    pub se: Option<ObjectRef>,
    /// Child nodes in `/First`->`/Next` order.
    pub children: Vec<OutlineNode>,
}

impl OutlineNode {
    /// Mirror qpdf `getDestPage()` without resolving the page operand.
    pub fn dest_page(&self) -> Object {
        match &self.dest {
            Object::Array(items) if !items.is_empty() => items[0].clone(),
            _ => Object::Null,
        }
    }
}

/// flpdf's normalized destination type returned by
/// [`OutlineDocumentHelper::legacy_dests`] and
/// [`OutlineDocumentHelper::name_tree_dests`].
///
/// This type is retained for those enumeration APIs; [`OutlineNode::dest`]
/// exposes the raw resolved object instead.
#[derive(Debug, Clone, PartialEq)]
pub struct Dest {
    /// The explicit destination array. Element 0 is normally the page ref.
    pub array: Vec<Object>,
}

impl Dest {
    /// The destination page ref (array element 0), if it is an indirect ref.
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

    /// Resolve the catalog `/Outlines` dict's own ref together with its
    /// `/First` child ref, if both are present. The outline dictionary's own
    /// ref is what a top-level item's `/Parent` should name (ISO 32000-1
    /// section 12.3.3: "The parent of a top-level item is the outline
    /// dictionary itself"), which [`Self::outline_root_first`] alone can't
    /// answer.
    fn outline_root(&mut self) -> Result<Option<(ObjectRef, ObjectRef)>> {
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
        let Some(first) = root.get_ref("First") else {
            return Ok(None);
        };
        Ok(Some((outlines_ref, first)))
    }

    /// Resolve the catalog `/Outlines` dict's `/First` child ref, if any.
    fn outline_root_first(&mut self) -> Result<Option<ObjectRef>> {
        Ok(self.outline_root()?.map(|(_outlines_ref, first)| first))
    }

    /// Materialize and return the top-level outline nodes (qpdf
    /// `getTopLevelOutlines`). "root" is this top-level vector; the `/Outlines`
    /// dict itself is not a navigable item and is not wrapped.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Unsupported`] if the outline nesting depth exceeds
    /// [`MAX_OUTLINE_WALK_DEPTH`]. Propagates any error from resolving outline
    /// objects (for example I/O or parse failures surfaced by [`Pdf::resolve`]).
    pub fn get_root(&mut self) -> Result<Vec<OutlineNode>> {
        self.get_root_with_max_depth(MAX_OUTLINE_WALK_DEPTH)
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

    /// Build a `/First`->`/Next` sibling chain into owned nodes, descending
    /// into each item's own `/First` children.
    ///
    /// Implemented as an explicit work stack rather than by native recursion:
    /// `current` is the sibling chain currently being extended and
    /// `ancestors` holds every enclosing chain waiting for it (or one of its
    /// descendants) to finish. This is what lets [`Self::get_root`] use a
    /// depth bound as large as [`MAX_OUTLINE_WALK_DEPTH`] without risking a
    /// stack overflow on a deeply (or maliciously) nested outline — a
    /// recursive walk would consume one native stack frame per nesting level.
    fn build_siblings(
        &mut self,
        start: ObjectRef,
        depth: usize,
        parent: Option<ObjectRef>,
        visited: &mut BTreeSet<ObjectRef>,
        max_depth: usize,
    ) -> Result<Vec<OutlineNode>> {
        if depth >= max_depth {
            return Err(too_deep(max_depth, start));
        }

        /// One `/First`->`/Next` sibling chain awaiting completion. `next` is
        /// the ref of the next sibling to resolve (`None` once the chain runs
        /// out); `nodes` accumulates that chain's completed siblings, each
        /// pushed with an empty `children` that is filled in once the frame
        /// for its own `/First` descendants — pushed onto `ancestors` right
        /// after — is folded back in.
        struct Frame {
            next: Option<ObjectRef>,
            parent: Option<ObjectRef>,
            depth: usize,
            nodes: Vec<OutlineNode>,
        }

        let mut current = Frame {
            next: Some(start),
            parent,
            depth,
            nodes: Vec::new(),
        };
        let mut ancestors: Vec<Frame> = Vec::new();

        loop {
            let Some(current_ref) = current.next else {
                // `current`'s chain is exhausted: fold its nodes into the
                // item that owns it, or return them if this was the top level.
                let Some(mut parent_frame) = ancestors.pop() else {
                    return Ok(current.nodes);
                };
                // Invariant: a frame is only pushed onto `ancestors` right
                // after its owning node is pushed onto that (about to become
                // `parent_frame`) frame's `nodes` below, so `nodes` is never
                // empty here.
                let owner = parent_frame
                    .nodes
                    .last_mut()
                    .expect("ancestors frame always has an owner node for its child frame");
                owner.children = std::mem::take(&mut current.nodes);
                current = parent_frame;
                continue;
            };

            if !visited.insert(current_ref) {
                current.next = None; // cycle - stop this chain
                continue;
            }
            let Object::Dictionary(dict) = self.pdf.resolve_borrowed(current_ref)? else {
                current.next = None; // non-dict item - stop this chain
                continue;
            };
            // IMPORTANT (borrow order): `dict` borrows `self.pdf` (it is a
            // `resolve_borrowed` reference). Extract EVERY value we need into
            // owned locals here, ending the `dict` borrow, BEFORE any
            // `self.pdf.resolve(...)` call below - otherwise the borrow checker
            // rejects it.
            let first = dict.get_ref("First");
            let next = dict.get_ref("Next");
            let se = dict.get_ref("SE");
            let title_src = dict.get("Title").cloned();
            let count_src = dict.get("Count").cloned();
            let dest_src = dict.get("Dest").cloned();
            let action_src = dict.get("A").cloned();
            // `dict` (and thus the &mut self.pdf borrow) is no longer used past
            // this point - owned values only from here on.
            let title = resolve_title(self.pdf, title_src)?;
            let count = resolve_int(self.pdf, count_src)?.unwrap_or(0);
            let dest = self.resolve_node_dest(dest_src.as_ref(), action_src.as_ref())?;

            current.next = next;
            current.nodes.push(OutlineNode {
                object_ref: current_ref,
                depth: current.depth,
                title,
                count,
                parent: current.parent,
                dest,
                se,
                children: Vec::new(),
            });

            if let Some(first) = first {
                let child_depth = current.depth + 1;
                if child_depth >= max_depth {
                    return Err(too_deep(max_depth, first));
                }
                ancestors.push(std::mem::replace(
                    &mut current,
                    Frame {
                        next: Some(first),
                        parent: Some(current_ref),
                        depth: child_depth,
                        nodes: Vec::new(),
                    },
                ));
            }
        }
    }

    /// Resolve a node's destination from `/Dest`, else a `/A` GoTo action's `/D`.
    fn resolve_node_dest(
        &mut self,
        dest: Option<&Object>,
        action: Option<&Object>,
    ) -> Result<Object> {
        let candidate = if let Some(dest) = dest {
            Some(dest.clone())
        } else {
            self.goto_action_dest(action)?
        };
        match candidate {
            Some(value) => self.resolve_node_dest_value(value),
            None => Ok(Object::Null),
        }
    }

    fn goto_action_dest(&mut self, action: Option<&Object>) -> Result<Option<Object>> {
        let Some(action) = action else {
            return Ok(None);
        };
        let Object::Dictionary(dict) = resolve_terminal_object(self.pdf, action.clone())? else {
            return Ok(None);
        };
        let Some(subtype) = dict.get("S").cloned() else {
            return Ok(None);
        };
        let subtype = resolve_terminal_object(self.pdf, subtype)?;
        if !matches!(subtype, Object::Name(ref name) if name == b"GoTo") {
            return Ok(None);
        }
        Ok(dict.get("D").cloned())
    }

    fn resolve_node_dest_value(&mut self, value: Object) -> Result<Object> {
        match resolve_terminal_object(self.pdf, value)? {
            Object::Name(name) => self.resolve_legacy_node_dest(&name),
            Object::String(bytes) => self.resolve_name_tree_node_dest(&bytes),
            other => Ok(other),
        }
    }

    fn resolve_legacy_node_dest(&mut self, name: &[u8]) -> Result<Object> {
        let Some(Object::Dictionary(dests)) = self.catalog_value_terminal("Dests")? else {
            return Ok(Object::Null);
        };
        match dests.get(name).cloned() {
            Some(value) => resolve_terminal_object(self.pdf, value),
            None => Ok(Object::Null),
        }
    }

    fn resolve_name_tree_node_dest(&mut self, bytes: &[u8]) -> Result<Object> {
        let lookup = qpdf_new_unicode_utf8_value(&crate::json_inspect::qpdf_utf8_value(bytes));
        let Some(Object::Dictionary(mut names)) = self.catalog_value_terminal("Names")? else {
            return Ok(Object::Null);
        };
        let Some(dests_root) = names.remove("Dests") else {
            return Ok(Object::Null);
        };
        let decode = |_pdf: &mut Pdf<R>, value| Ok(Some(value));
        let entries = read_name_tree(self.pdf, dests_root, decode, DEFAULT_MAX_OUTLINE_DEPTH)?;
        for (stored, value) in entries {
            if crate::json_inspect::qpdf_utf8_value(&stored) == lookup {
                return resolve_terminal_object(self.pdf, value);
            }
        }
        Ok(Object::Null)
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
        // 1. Modern: catalog /Names /Dests name tree (PDF 1.2+). /Names may
        //    be reached through a multi-hop holder chain (matches the sub-2
        //    fix to `name_tree_dests`).
        if let Some(Object::Dictionary(mut names)) = self.catalog_value_terminal("Names")? {
            if let Some(dests_root) = names.remove("Dests") {
                let entries = read_name_tree(
                    self.pdf,
                    dests_root,
                    |_pdf, value| Ok(Some(value)),
                    DEFAULT_MAX_OUTLINE_DEPTH,
                )?; // cov:ignore: /Names path requires sub-2's name-tree fixtures; tested there
                    // Re-reads the whole name tree per named hop; acceptable
                    // because each hop strictly decreases `depth` (no visited
                    // set needed).
                for (key, value) in entries {
                    if key.as_slice() == name {
                        return self.dest_from_value(&value, depth - 1);
                    } // cov:ignore: early return leaves this `}` unexecuted (LCOV quirk)
                }
            }
        }
        // 2. Legacy: catalog /Dests dict (PDF 1.1). Follow the same multi-hop
        //    holder chain `legacy_dests` uses so a name/string alias inside a
        //    chained-through /Dests dict still resolves.
        if let Some(Object::Dictionary(mut dests)) = self.catalog_value_terminal("Dests")? {
            if let Some(value) = dests.remove(name) {
                return self.dest_from_value(&value, depth - 1);
            } // cov:ignore: early return leaves this `}` unexecuted (LCOV quirk)
        }
        Ok(None)
    }

    /// Like [`Self::catalog_value`] but follows the full indirect reference
    /// chain to its terminal object, mirroring [`Self::legacy_dests`]. Used
    /// by [`Self::resolve_named_dest`] so a name/string alias inside a
    /// `/Dests` (or `/Names`) dict that is only reachable through more than
    /// one indirection still resolves.
    fn catalog_value_terminal(&mut self, key: &str) -> Result<Option<Object>> {
        Ok(match self.catalog_value(key)? {
            Some(value @ Object::Reference(_)) => {
                Some(crate::ref_chain::resolve_ref_chain(self.pdf, &value)?.0)
            }
            other => other,
        })
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

    /// Read every entry of the catalog's `/Names /Dests` name tree (ISO
    /// 32000-2 §7.9.6 + §12.3.2.3): the PDF 1.2+ modern named-destination
    /// structure, which supersedes — but does not replace, both may coexist
    /// — the legacy `/Catalog /Dests` dictionary read by
    /// [`Self::legacy_dests`]. The tree shape (`/Kids`/`/Names` nodes with
    /// `/Limits`) is identical to `/Names /EmbeddedFiles`; see
    /// [`crate::name_tree_dests`] for the writer.
    ///
    /// Entries come back in the tree's depth-first, key-ascending order (the
    /// order the spec requires the tree be sorted in — see
    /// [`crate::name_number_tree::read_name_tree`]). A value that cannot be
    /// resolved to an explicit destination array (for example a malformed
    /// non-array, non-reference, non-`/D`-bearing value) yields `None` for
    /// that entry rather than dropping the name, so a caller can still see
    /// every declared name.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the catalog, `/Names`, or the
    /// `/Dests` name-tree nodes. Returns [`crate::Error::Unsupported`] if a
    /// `/Kids` chain exceeds [`DEFAULT_MAX_OUTLINE_DEPTH`] (guards against
    /// cyclic or maliciously deep trees).
    pub fn name_tree_dests(&mut self) -> Result<Vec<(Vec<u8>, Option<Dest>)>> {
        // `catalog_value` resolves a single level of indirection but stops
        // short of a multi-hop holder chain (catalog /Names -> r1 -> r2 ->
        // dict). The writer (name_tree_dests module) uses `resolve_ref_chain`
        // for this; the reader must too or it silently returns empty when
        // a document keeps its /Names dictionary behind more than one ref.
        let names_obj = match self.catalog_value("Names")? {
            Some(value @ Object::Reference(_)) => {
                crate::ref_chain::resolve_ref_chain(self.pdf, &value)?.0
            }
            Some(other) => other,
            None => return Ok(Vec::new()),
        };
        let Object::Dictionary(mut names) = names_obj else {
            return Ok(Vec::new());
        };
        let Some(dests_root) = names.remove("Dests") else {
            return Ok(Vec::new());
        };
        let decode = |_pdf: &mut Pdf<R>, value: Object| Ok(Some(value));
        let raw = read_name_tree(self.pdf, dests_root, decode, DEFAULT_MAX_OUTLINE_DEPTH)?;
        let mut out = Vec::with_capacity(raw.len());
        for (name, value) in raw {
            let dest = self.dest_from_value(&value, MAX_DEST_RESOLVE_DEPTH)?;
            out.push((name, dest));
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
    /// [`MAX_OUTLINE_WALK_DEPTH`]. Propagates any error from resolving outline
    /// objects (for example I/O or parse failures surfaced by [`Pdf::resolve`]).
    pub fn iter(&mut self) -> Result<impl Iterator<Item = OutlineNode>> {
        let roots = self.get_root()?;
        Ok(flatten_preorder(roots).into_iter())
    }

    /// Visit every node pre-order, passing `(node, depth)` to `visitor`. The
    /// visited nodes have populated `children`. Mirrors a qpdf outline walk.
    ///
    /// For a runnable walkthrough see `examples/walk_outline.rs`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Unsupported`] if the outline nesting depth exceeds
    /// [`MAX_OUTLINE_WALK_DEPTH`]. Propagates any error from resolving outline
    /// objects (for example I/O or parse failures surfaced by [`Pdf::resolve`]).
    pub fn walk<F: FnMut(&OutlineNode, usize)>(&mut self, mut visitor: F) -> Result<()> {
        let roots = self.get_root()?;
        walk_nodes(&roots, &mut visitor);
        Ok(())
    }

    /// Drop dangling outline `/SE` links using [`DEFAULT_MAX_OUTLINE_DEPTH`].
    /// See [`prune_outline_se`] (the free-function form of this method, which
    /// most callers should prefer) for the full contract.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Unsupported`] if the outline nesting depth exceeds
    /// [`DEFAULT_MAX_OUTLINE_DEPTH`]. Propagates any error from resolving outline
    /// objects (for example I/O or parse failures surfaced by [`Pdf::resolve`]).
    pub fn prune_se(&mut self, live_struct_elem_refs: &BTreeSet<ObjectRef>) -> Result<usize> {
        self.prune_se_with_max_depth(live_struct_elem_refs, DEFAULT_MAX_OUTLINE_DEPTH)
    }

    /// Like [`Self::prune_se`] but with a caller-supplied recursion limit.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Unsupported`] if the outline nesting depth exceeds
    /// `max_depth`. Propagates any error from resolving outline objects.
    pub fn prune_se_with_max_depth(
        &mut self,
        live_struct_elem_refs: &BTreeSet<ObjectRef>,
        max_depth: usize,
    ) -> Result<usize> {
        let Some(first) = self.outline_root_first()? else {
            return Ok(0);
        };
        let mut visited = BTreeSet::new();
        walk_outline_se(
            self.pdf,
            first,
            0,
            &mut visited,
            live_struct_elem_refs,
            max_depth,
        )
    }

    /// Validate `/Parent`/`/Prev` linkage across the outline tree using
    /// [`MAX_OUTLINE_WALK_DEPTH`]. See
    /// [`Self::check_links_with_max_depth`] for the full contract.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Unsupported`] if the outline nesting depth
    /// exceeds [`MAX_OUTLINE_WALK_DEPTH`]. Propagates any error from
    /// resolving outline objects.
    pub fn check_links(&mut self) -> Result<Diagnostics> {
        self.check_links_with_max_depth(MAX_OUTLINE_WALK_DEPTH)
    }

    /// Like [`Self::check_links`] but with a caller-supplied recursion limit.
    ///
    /// Validates every outline item's `/Parent` and `/Prev` against the
    /// `/First`->`/Next` chain the tree actually follows (ISO 32000-1 section
    /// 12.3.3): a non-first sibling's `/Prev` should name the preceding
    /// sibling, the first sibling in a chain should have no `/Prev`, and
    /// every item's `/Parent` should name its containing item (or the
    /// `/Outlines` dictionary itself, for a top-level item). Each link that
    /// doesn't match pushes one warning [`Diagnostic`]. A `/Next` or `/First`
    /// cycle also pushes one warning and cuts that chain short right there —
    /// the same point [`Self::get_root`] bails out of a cyclic chain instead
    /// of looping forever — so a hostile or malformed outline can't cause an
    /// infinite loop or unbounded memory growth.
    ///
    /// Returns an empty [`Diagnostics`] when the document has no outline.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Unsupported`] if the outline nesting depth
    /// exceeds `max_depth`. Propagates any error from resolving outline
    /// objects (for example I/O or parse failures surfaced by [`Pdf::resolve_borrowed`]).
    pub fn check_links_with_max_depth(&mut self, max_depth: usize) -> Result<Diagnostics> {
        let Some((outlines_ref, first)) = self.outline_root()? else {
            return Ok(Diagnostics::default());
        };
        let mut visited = BTreeSet::new();
        let mut diagnostics = Diagnostics::default();
        walk_outline_links(
            self.pdf,
            first,
            outlines_ref,
            &mut visited,
            max_depth,
            &mut diagnostics,
        )?;
        Ok(diagnostics)
    }
}

/// Match `newUnicodeString(utf8).getUTF8Value()` in qpdf 11.9.0.
///
/// qpdf accepts up to six-byte UTF-8 forms while decoding, consumes malformed
/// sequences according to `QUtil::get_next_utf8_codepoint`, then writes U+FFFD
/// for every decode error, surrogate, or code point above U+10FFFF.
fn qpdf_new_unicode_utf8_value(utf8: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(utf8.len());
    let mut pos = 0;
    while pos < utf8.len() {
        let original_pos = pos;
        let mut byte = utf8[pos];
        pos += 1;

        if byte < 0x80 {
            result.push(byte);
            continue;
        }

        let mut bytes_needed = 0;
        let mut bit_check = 0x40;
        let mut to_clear = 0x80;
        while byte & bit_check != 0 {
            bytes_needed += 1;
            to_clear |= bit_check;
            bit_check >>= 1;
        }

        let mut error = !(1..=5).contains(&bytes_needed) || pos + bytes_needed > utf8.len();
        let mut codepoint = 0xfffd;
        if !error {
            codepoint = u32::from(byte & !to_clear);
            for _ in 0..bytes_needed {
                byte = utf8[pos];
                pos += 1;
                if byte & 0xc0 != 0x80 {
                    pos -= 1;
                    error = true;
                    break;
                }
                codepoint = (codepoint << 6) + u32::from(byte & 0x3f);
            }

            if !error {
                let lower_bounds = [0, 0, 1 << 7, 1 << 11, 1 << 16, 1 << 12, 1 << 26];
                let lower_bound = lower_bounds[pos - original_pos];
                if lower_bound > 0 && codepoint < lower_bound {
                    error = true;
                }
            }
        }

        let scalar = if error {
            '\u{fffd}'
        } else {
            char::from_u32(codepoint).unwrap_or('\u{fffd}')
        };
        let mut encoded = [0; 4];
        result.extend_from_slice(scalar.encode_utf8(&mut encoded).as_bytes());
    }
    result
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
/// # Known limitation
///
/// The live-page set comes from [`crate::pages::page_refs`], which walks
/// `/Kids` entries but does not currently follow a `/Kids` entry that is
/// itself a bare indirect reference chain (e.g. `/Kids [30 0 R]` where
/// `30 0 obj` is `3 0 R` and `3 0 obj` is the actual `/Page`). In that
/// corner case a legacy destination pointing at the terminal page is
/// falsely flagged as dangling. Documents produced by mainstream writers
/// (qpdf, Acrobat, common libraries) do not use bare-ref holders in
/// `/Kids`, so this is a hostile/hand-authored input concern rather than
/// a round-trip regression.
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

/// Validate the catalog's `/Names /Dests` name tree
/// ([`OutlineDocumentHelper::name_tree_dests`]): push a warning [`Diagnostic`]
/// for every entry whose destination's target page reference is not a page
/// currently reachable from the document's `/Pages` tree — a dangling
/// reference, a reference to a non-`/Page` object, or a page a prior edit
/// removed. A missing target is reported, not treated as document
/// corruption: it never turns into an `Err` on its own.
///
/// See [`check_legacy_dests`] for the equivalent check over the legacy
/// `/Catalog /Dests` dictionary; the two structures are validated
/// independently since a document may carry either or both.
///
/// # Errors
///
/// Propagates any error from resolving the catalog or the `/Names /Dests`
/// name-tree nodes. A failure to enumerate the document's page tree (for
/// example a missing `/Pages` entry) is downgraded to a warning
/// [`Diagnostic`] instead, so the caller still receives a report.
///
/// # Examples
///
/// ```no_run
/// use flpdf::{check_name_tree_dests, Pdf};
/// use std::fs::File;
/// use std::io::BufReader;
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
/// for diagnostic in check_name_tree_dests(&mut pdf)?.entries() {
///     println!("{diagnostic:?}");
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn check_name_tree_dests<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Diagnostics> {
    let entries = OutlineDocumentHelper::new(pdf).name_tree_dests()?;
    let mut diagnostics = Diagnostics::default();
    if entries.is_empty() {
        return Ok(diagnostics);
    }

    // Mirror `check_legacy_dests`: if no entry has a resolvable page ref, the
    // page-tree walk below can flag nothing. Skip it — otherwise a document
    // with only malformed/unresolvable name-tree destinations plus a broken
    // /Pages tree would emit a spurious "could not enumerate pages" warning.
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
                format!("could not enumerate pages to validate /Names /Dests targets: {error}"),
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
        // Normalise through any holder chain — mirrors the sub-1 fix on
        // `check_legacy_dests`: a destination whose page operand is stored
        // behind a holder (`/target [30 0 R /Fit]` with `30 0 obj 3 0 R`)
        // points at a live page, but a naïve `==` against page_refs
        // would false-flag it as dangling.
        let page_ref = resolve_page_ref_through_holders(pdf, page_ref_raw);
        if !live_pages.contains(&page_ref) {
            diagnostics.push(Diagnostic::warning(
                format!(
                    "named destination \"{}\" (in /Names /Dests) targets {page_ref}, which is not a page in the document",
                    String::from_utf8_lossy(&name)
                ),
                None,
            ));
        }
    }
    Ok(diagnostics)
}

/// Validate `/Parent`/`/Prev` linkage across the document's outline tree
/// ([`OutlineDocumentHelper::check_links`]): push a warning [`Diagnostic`]
/// for every outline item whose `/Parent` or `/Prev` doesn't match the
/// `/First`->`/Next` chain the tree actually follows, and for every `/Next`
/// or `/First` cycle encountered (the chain is then cut short there, exactly
/// as [`OutlineDocumentHelper::get_root`] does, rather than looping forever).
/// A missing or dangling link is reported, not treated as document
/// corruption: it never turns into an `Err` on its own.
///
/// # Errors
///
/// Returns [`crate::Error::Unsupported`] if the outline nesting depth exceeds
/// [`MAX_OUTLINE_WALK_DEPTH`]. Propagates any error from resolving outline
/// objects (for example I/O or parse failures surfaced by [`Pdf::resolve_borrowed`]).
///
/// # Examples
///
/// ```no_run
/// use flpdf::{check_outline_links, Pdf};
/// use std::fs::File;
/// use std::io::BufReader;
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
/// for diagnostic in check_outline_links(&mut pdf)?.entries() {
///     println!("{diagnostic:?}");
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn check_outline_links<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Diagnostics> {
    OutlineDocumentHelper::new(pdf).check_links()
}

/// Drop dangling outline `/SE` (structure-element) links.
///
/// Walks the document's `/Outlines` tree (`/First`/`/Next`/`/First` order,
/// same as [`OutlineDocumentHelper::get_root`]) and removes the `/SE` key
/// from every outline item dictionary whose `/SE` target is not a member of
/// `live_struct_elem_refs`. An outline item with no `/SE`, or whose `/SE` is
/// not a (spec-mandated) indirect reference, is left untouched — matching
/// how [`OutlineNode::se`] reads a non-conforming `/SE` value.
///
/// This function does not compute `live_struct_elem_refs` itself: pass in the
/// set of structure element refs that remain reachable from
/// `/StructTreeRoot` after the structure tree has been dropped or rebuilt.
/// When the structure tree is left intact, `/SE` links need no pruning —
/// leaving a document's `/Outlines` tree and `/StructTreeRoot` unmodified
/// (an ordinary read-then-[`crate::write_pdf`] round trip) already preserves
/// every `/SE` entry verbatim, the same as any other outline dictionary key.
///
/// Returns the number of `/SE` entries removed, for diagnostics.
///
/// # Errors
///
/// Returns [`crate::Error::Unsupported`] if the outline nesting depth exceeds
/// [`DEFAULT_MAX_OUTLINE_DEPTH`]. Propagates any error from resolving outline
/// objects (for example I/O or parse failures surfaced by [`Pdf::resolve`]).
///
/// # Examples
///
/// ```no_run
/// use flpdf::{prune_outline_se, ObjectRef, Pdf};
/// use std::collections::BTreeSet;
/// use std::fs::File;
/// use std::io::BufReader;
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
/// // Structure elements that remain reachable from /StructTreeRoot after a
/// // rebuild; an empty set prunes every outline /SE link.
/// let live: BTreeSet<ObjectRef> = BTreeSet::new();
/// let dropped = prune_outline_se(&mut pdf, &live)?;
/// println!("dropped {dropped} dangling /SE link(s)");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn prune_outline_se<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    live_struct_elem_refs: &BTreeSet<ObjectRef>,
) -> Result<usize> {
    OutlineDocumentHelper::new(pdf).prune_se(live_struct_elem_refs)
}

/// Like [`prune_outline_se`] but with a caller-supplied recursion limit.
///
/// # Errors
///
/// Returns [`crate::Error::Unsupported`] if the outline nesting depth exceeds
/// `max_depth`. Propagates any error from resolving outline objects.
pub fn prune_outline_se_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    live_struct_elem_refs: &BTreeSet<ObjectRef>,
    max_depth: usize,
) -> Result<usize> {
    OutlineDocumentHelper::new(pdf).prune_se_with_max_depth(live_struct_elem_refs, max_depth)
}

fn resolve_terminal_object<R: Read + Seek>(pdf: &mut Pdf<R>, value: Object) -> Result<Object> {
    match value {
        value @ Object::Reference(_) => Ok(crate::ref_chain::resolve_ref_chain(pdf, &value)?.0),
        other => Ok(other),
    }
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

/// Flatten `roots` and every descendant into a single `Vec`, pre-order, with
/// each node's `children` taken/emptied (see [`OutlineDocumentHelper::iter`]).
///
/// Iterative — an explicit stack of sibling iterators — rather than
/// recursive, so a tree materialized tens of thousands of levels deep (see
/// [`MAX_OUTLINE_WALK_DEPTH`]) can't overflow the call stack the way
/// recursing once per level would.
fn flatten_preorder(roots: Vec<OutlineNode>) -> Vec<OutlineNode> {
    let mut out = Vec::new();
    let mut stack: Vec<std::vec::IntoIter<OutlineNode>> = vec![roots.into_iter()];
    while let Some(level) = stack.last_mut() {
        match level.next() {
            None => {
                stack.pop();
            }
            Some(mut node) => {
                let children = std::mem::take(&mut node.children);
                out.push(node);
                if !children.is_empty() {
                    stack.push(children.into_iter());
                }
            }
        }
    }
    out
}

/// Invoke `visitor(node, node.depth)` for `roots` and every descendant,
/// pre-order.
///
/// Iterative for the same reason as [`flatten_preorder`]: an explicit stack
/// of sibling iterators in place of one native call frame per nesting level.
fn walk_nodes<F: FnMut(&OutlineNode, usize)>(roots: &[OutlineNode], visitor: &mut F) {
    let mut stack: Vec<std::slice::Iter<'_, OutlineNode>> = vec![roots.iter()];
    while let Some(level) = stack.last_mut() {
        match level.next() {
            None => {
                stack.pop();
            }
            Some(node) => {
                visitor(node, node.depth);
                if !node.children.is_empty() {
                    stack.push(node.children.iter());
                }
            }
        }
    }
}

/// Build the "outline depth exceeds maximum" [`crate::Error::Unsupported`]
/// used by [`OutlineDocumentHelper::build_siblings`] and
/// [`walk_outline_links`].
fn too_deep(max_depth: usize, at: ObjectRef) -> crate::Error {
    crate::Error::Unsupported(format!(
        "outline depth exceeds maximum of {max_depth} at {at}"
    ))
}

/// Iterative worklist backing [`OutlineDocumentHelper::check_links_with_max_depth`].
///
/// Unlike [`OutlineDocumentHelper::build_siblings`], no node tree is
/// materialized here, so frames don't need to be folded back into a parent
/// on completion — each frame stands alone and is simply discarded once its
/// `next` runs out. A frame is still pushed for a node's `/First` children
/// right after (so it pops, and is fully processed, before the node's own
/// `/Next` continuation) to match the depth-first order
/// [`OutlineDocumentHelper::get_root`]'s recursion would visit items in,
/// which keeps diagnostics in document order.
fn walk_outline_links<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    first: ObjectRef,
    outlines_ref: ObjectRef,
    visited: &mut BTreeSet<ObjectRef>,
    max_depth: usize,
    diagnostics: &mut Diagnostics,
) -> Result<()> {
    /// One pending sibling-chain continuation: `next` is the item to
    /// process (`None` once the chain this frame belongs to is exhausted),
    /// `expected_parent`/`expected_prev` are what that item's `/Parent` and
    /// `/Prev` should be per the `/First`->`/Next` chain.
    struct Frame {
        next: Option<ObjectRef>,
        expected_parent: ObjectRef,
        expected_prev: Option<ObjectRef>,
        depth: usize,
    }

    let mut stack = vec![Frame {
        next: Some(first),
        expected_parent: outlines_ref,
        expected_prev: None,
        depth: 0,
    }];

    while let Some(frame) = stack.pop() {
        let Some(current_ref) = frame.next else {
            continue; // this frame's chain already ran out
        };
        if frame.depth >= max_depth {
            return Err(too_deep(max_depth, current_ref));
        }
        if !visited.insert(current_ref) {
            diagnostics.push(Diagnostic::warning(
                format!(
                    "outline item {current_ref} was already visited via a /Next or /First cycle"
                ),
                None,
            ));
            continue; // cycle - bail this chain
        }
        let Object::Dictionary(dict) = pdf.resolve_borrowed(current_ref)? else {
            continue; // non-dict item - stop this chain
        };
        let actual_parent = dict.get_ref("Parent");
        let actual_prev = dict.get_ref("Prev");
        let first_child = dict.get_ref("First");
        let next_sibling = dict.get_ref("Next");

        check_ref_link(
            diagnostics,
            current_ref,
            "Parent",
            actual_parent,
            Some(frame.expected_parent),
        );
        check_ref_link(
            diagnostics,
            current_ref,
            "Prev",
            actual_prev,
            frame.expected_prev,
        );

        // Push the sibling continuation first so the `/First` descent
        // (pushed next) pops — and is fully processed — first, matching
        // `/First`-before-`/Next` recursion order.
        stack.push(Frame {
            next: next_sibling,
            expected_parent: frame.expected_parent,
            expected_prev: Some(current_ref),
            depth: frame.depth,
        });
        if let Some(first_child) = first_child {
            stack.push(Frame {
                next: Some(first_child),
                expected_parent: current_ref,
                expected_prev: None,
                depth: frame.depth + 1,
            });
        }
    }
    Ok(())
}

/// Compare an outline item's actual `/Parent` or `/Prev` ref (already read
/// via [`crate::Dictionary::get_ref`], so `None` also covers a value stored
/// as anything other than a direct reference) against what the
/// `/First`->`/Next` chain implies it should be, pushing one warning
/// [`Diagnostic`] when they differ.
fn check_ref_link(
    diagnostics: &mut Diagnostics,
    item_ref: ObjectRef,
    field: &str,
    actual: Option<ObjectRef>,
    expected: Option<ObjectRef>,
) {
    if actual == expected {
        return;
    }
    let describe = |r: Option<ObjectRef>| r.map_or_else(|| "none".to_string(), |r| r.to_string());
    diagnostics.push(Diagnostic::warning(
        format!(
            "outline item {item_ref}: /{field} is {}, expected {} to match the /First-/Next chain",
            describe(actual),
            describe(expected)
        ),
        None,
    ));
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

/// Walk one `/First`->`/Next` sibling chain (and recurse into `/First`
/// grandchildren), dropping any `/SE` not present in `live`. Returns the
/// number of `/SE` entries removed along this chain and its descendants.
///
/// Mirrors [`OutlineDocumentHelper::build_siblings`]'s traversal shape
/// (same cycle-safe `visited` set, same depth cap) but mutates in place via
/// [`Pdf::set_object`] instead of materializing [`OutlineNode`]s.
fn walk_outline_se<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    start: ObjectRef,
    depth: usize,
    visited: &mut BTreeSet<ObjectRef>,
    live: &BTreeSet<ObjectRef>,
    max_depth: usize,
) -> Result<usize> {
    if depth >= max_depth {
        return Err(crate::Error::Unsupported(format!(
            "outline depth exceeds maximum of {max_depth} at {start}"
        )));
    }

    let mut pruned = 0;
    let mut current = Some(start);
    while let Some(current_ref) = current {
        if !visited.insert(current_ref) {
            break; // cycle - stop this chain
        }

        // Peek via a borrow first (pruning is the rare case). We only
        // materialize an owned mutable dict + `set_object` when there is
        // actually a dangling `/SE` to drop; the common no-op path avoids
        // the deep dict clone entirely.
        let Object::Dictionary(dict) = pdf.resolve_borrowed(current_ref)? else {
            break;
        };
        let first = dict.get_ref("First");
        let next = dict.get_ref("Next");
        let se = dict.get_ref("SE");

        if let Some(se_ref) = se {
            if !live.contains(&se_ref) {
                // Now we need to mutate — take an owned dict via resolve().
                if let Object::Dictionary(mut dict) = pdf.resolve(current_ref)? {
                    dict.remove("SE");
                    pdf.set_object(current_ref, Object::Dictionary(dict));
                    pruned += 1;
                }
            }
        }

        if let Some(first) = first {
            pruned += walk_outline_se(pdf, first, depth + 1, visited, live, max_depth)?;
        }

        current = next;
    }

    Ok(pruned)
}

#[cfg(test)]
mod qpdf_utf8_tests {
    use super::qpdf_new_unicode_utf8_value;

    #[test]
    fn new_unicode_string_normalization_matches_qpdf_error_consumption() {
        let replacement = "\u{fffd}".as_bytes();
        let cases: &[(&[u8], &[u8])] = &[
            (b"ascii", b"ascii"),
            (
                &[0xc2, 0xa2, 0xf0, 0x9f, 0x92, 0xa9],
                &[0xc2, 0xa2, 0xf0, 0x9f, 0x92, 0xa9],
            ),
            (&[0x80], replacement),
            (&[0xff, b'a'], "\u{fffd}a".as_bytes()),
            (&[0xe2, 0x82], "\u{fffd}\u{fffd}".as_bytes()),
            (&[0xe2, b'(', 0xa1], "\u{fffd}(\u{fffd}".as_bytes()),
            (&[0xe2, 0x82, b'('], "\u{fffd}(".as_bytes()),
            (&[0xc0, 0xaf], replacement),
            (&[0xed, 0xa0, 0x80], replacement),
            (&[0xf4, 0x90, 0x80, 0x80], replacement),
            (&[0xf8, 0x80, 0x80, 0x80, 0x80], replacement),
            (&[0xf8, 0x80, 0x81, 0x80, 0x80], &[0xe1, 0x80, 0x80]),
            (&[0xfc, 0x84, 0x80, 0x80, 0x80, 0x80], replacement),
        ];
        for &(input, expected) in cases {
            assert_eq!(qpdf_new_unicode_utf8_value(input), expected, "{input:x?}");
        }
    }
}

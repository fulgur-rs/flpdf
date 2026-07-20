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

/// Default recursion limit for walking an outline action's `/Next` chain via
/// [`OutlineDocumentHelper::action_chain`]. ISO 32000-1 section 12.6.2:
/// `/Next` chains an action to further actions performed in order afterward
/// (a single action, or an array of actions). The bound exists to make a
/// hostile or malformed `/Next` chain (a self-reference, a mutual reference,
/// or an implausibly long chain) terminate instead of growing without bound
/// or looping forever.
pub const DEFAULT_MAX_ACTION_CHAIN_DEPTH: usize = 100;

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
    /// The `/SE` (structure-element) link: an indirect reference to a node in
    /// the document's structure tree (ISO 32000-2 section 12.3.3, table 151),
    /// or `None` when `/SE` is absent. Per spec `/SE` shall be an indirect
    /// reference; a direct (non-reference) `/SE` value is malformed and is
    /// also read as `None`, matching how [`Self::dest`] and the other fields
    /// on this type treat a non-conforming value as absent.
    pub se: Option<ObjectRef>,
    /// The item's own `/A` action (ISO 32000-1 section 12.6), typed by
    /// subtype, or `None` when `/A` is absent or does not resolve to a
    /// dictionary. A `/Next`-chained action is not included here — use
    /// [`OutlineDocumentHelper::action_chain`] to walk the full chain.
    pub action: Option<OutlineAction>,
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

/// An outline item's `/A` action, classified by its `/S` subtype (ISO
/// 32000-1 section 12.6.4). Every recognized variant resolves one level of
/// indirection on its subtype-specific fields, so a value stored as an
/// indirect reference reads the same as a direct one. A recognized subtype
/// that is missing its required field, or any other subtype (including one
/// with no `/S` at all), is preserved opaque as [`Self::Unknown`] — the
/// whole action dictionary, untouched.
#[derive(Debug, Clone, PartialEq)]
pub enum OutlineAction {
    /// `/S /GoTo`: a destination within this document. `d` is the action's
    /// `/D` value (an array, a name/string naming a destination, or a
    /// dictionary holding one of those under its own `/D`); see
    /// [`OutlineNode::dest`] for the same destination already resolved to an
    /// explicit page target.
    GoTo {
        /// The action's `/D` destination value.
        d: Object,
    },
    /// `/S /GoToR`: a destination in a remote (external) PDF document.
    GoToR {
        /// The remote file, as a file specification (ISO 32000-1 section 7.11):
        /// either a `Object::String`/`Object::HexString` file path or an
        /// `Object::Dictionary` with `/Type /Filespec`.
        f: Object,
        /// The destination within the remote file. Absent when the action
        /// simply opens the file at its default view (per ISO 32000-1 section
        /// 12.6.4.3, the destination is optional).
        d: Option<Object>,
    },
    /// `/S /URI`: resolve a uniform resource identifier.
    Uri {
        /// The URI, as the raw bytes of `/URI`. Preserved verbatim — this
        /// crate does not attempt URL parsing or normalization.
        uri: Vec<u8>,
    },
    /// `/S /Launch`: launch an application, typically to open a target file.
    Launch {
        /// The application/file to launch, as a file specification (same
        /// shape as [`Self::GoToR::f`]: string path or `/Type /Filespec`
        /// dictionary).
        f: Object,
    },
    /// `/S /Named`: execute a predefined, viewer-specific action such as
    /// `NextPage`, `PrevPage`, `FirstPage`, or `LastPage`.
    Named {
        /// The named action, as the raw bytes of `/N`.
        n: Vec<u8>,
    },
    /// Any other action subtype, including a missing `/S`: the full action
    /// dictionary, preserved exactly as read.
    Unknown(crate::Dictionary),
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
            // action_src is BORROWED here (as Option<&Object>) for the dest
            // fallback, then the owned value is MOVED into
            // parse_outline_action only in the Some branch — no double-use,
            // no clone. The None branch skips the call entirely.
            let dest = self.resolve_node_dest(dest_src.as_ref(), action_src.as_ref())?;
            let action = if let Some(a) = action_src {
                parse_outline_action(self.pdf, a)?
            } else {
                None
            };

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
                se,
                action,
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
        dest: Option<&Object>,
        action: Option<&Object>,
    ) -> Result<Option<Dest>> {
        if let Some(d) = dest {
            if let Some(found) = self.dest_from_value(d, MAX_DEST_RESOLVE_DEPTH)? {
                return Ok(Some(found));
            }
        }
        if let Some(a) = action {
            // Hold the resolved-owned Object on the stack so `adict` (a
            // borrow into it) satisfies the borrow checker for the rest
            // of this block.
            let resolved_owned;
            let adict = match a {
                Object::Reference(r) => {
                    resolved_owned = self.pdf.resolve(*r)?;
                    resolved_owned.as_dict()
                }
                other => other.as_dict(),
            };
            if let Some(adict) = adict {
                // Extract both /S and /D as owned values before ending the
                // adict borrow, so we can resolve /S via &mut self.pdf.
                let s_src = adict.get("S").cloned();
                let d_src = adict.get("D").cloned();
                // `/S` may be stored as an indirect reference; matching
                // Object::Name against a raw Object::Reference silently
                // misses the GoTo path. Resolve one level before matching.
                let s = resolve_one_level(self.pdf, s_src)?;
                let is_goto = matches!(s, Some(Object::Name(ref n)) if n == b"GoTo");
                if is_goto {
                    if let Some(d) = d_src {
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

    /// Walk an outline item's `/A` action and its `/Next` chain, using
    /// [`DEFAULT_MAX_ACTION_CHAIN_DEPTH`]. See
    /// [`Self::action_chain_with_max_depth`] for the full contract.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the item or its action objects
    /// (for example I/O or parse failures surfaced by [`Pdf::resolve`]).
    pub fn action_chain(&mut self, item_ref: ObjectRef) -> Result<Vec<OutlineAction>> {
        self.action_chain_with_max_depth(item_ref, DEFAULT_MAX_ACTION_CHAIN_DEPTH)
    }

    /// Like [`Self::action_chain`] but with a caller-supplied recursion limit.
    ///
    /// Returns every action reachable from `item_ref`'s `/A`, in visitation
    /// order: the item's own action first, then each action chained after it
    /// via `/Next` (a single action, or an array of actions performed in
    /// order after the action they extend — ISO 32000-1 section 12.6.2).
    /// Returns an empty `Vec` when `item_ref` has no `/A`, or when `/A` does
    /// not resolve to a dictionary.
    ///
    /// Bounded by `max_depth` and a cycle guard on indirect action objects,
    /// so a hostile or malformed `/Next` chain (a self-reference, a mutual
    /// reference, or an implausibly long chain) terminates instead of
    /// growing without bound or looping forever.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the item or its action objects
    /// (for example I/O or parse failures surfaced by [`Pdf::resolve`]).
    pub fn action_chain_with_max_depth(
        &mut self,
        item_ref: ObjectRef,
        max_depth: usize,
    ) -> Result<Vec<OutlineAction>> {
        let a_value = {
            let Object::Dictionary(dict) = self.pdf.resolve_borrowed(item_ref)? else {
                return Ok(Vec::new());
            };
            dict.get("A").cloned()
        };
        let Some(a_value) = a_value else {
            return Ok(Vec::new());
        };
        let mut visited = BTreeSet::new();
        let mut out = Vec::new();
        collect_action_chain(self.pdf, a_value, &mut visited, max_depth, &mut out)?;
        Ok(out)
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

/// Resolve one level of indirection on an optional value (review rule 2).
/// Returns `None` only when `value` itself is `None`.
fn resolve_one_level<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: Option<Object>,
) -> Result<Option<Object>> {
    Ok(match value {
        Some(Object::Reference(r)) => Some(pdf.resolve(r)?),
        other => other,
    })
}

/// Parse an outline item's raw, unresolved `/A` value into a typed
/// [`OutlineAction`] — this item's own action only; a chained `/Next` action
/// is not followed (see [`OutlineDocumentHelper::action_chain`] for that).
/// Resolves one level of indirection on the action value itself (review rule
/// 2 — `/A` may be stored as an indirect reference). Returns `None` when the
/// resolved value is not a dictionary.
fn parse_outline_action<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: Object,
) -> Result<Option<OutlineAction>> {
    let resolved = match value {
        Object::Reference(r) => pdf.resolve(r)?,
        other => other,
    };
    match resolved.into_dict() {
        Some(dict) => action_from_dict(pdf, dict).map(Some),
        None => Ok(None),
    }
}

/// Build an [`OutlineAction`] from an already-resolved action dictionary,
/// classifying it by its `/S` subtype (ISO 32000-1 section 12.6.4). Each
/// subtype-specific field is resolved one level of indirection (review rule
/// 2); a recognized subtype missing its required field, or any other
/// subtype (including an absent `/S`), is preserved opaque as
/// [`OutlineAction::Unknown`] — the whole dictionary, untouched (`dict` is
/// moved into that variant rather than cloned, since every branch above it
/// only clones the individual field it inspects).
fn action_from_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: crate::Dictionary,
) -> Result<OutlineAction> {
    let subtype = resolve_one_level(pdf, dict.get("S").cloned())?.and_then(Object::into_name);

    Ok(match subtype.as_deref() {
        Some(b"GoTo") => match resolve_one_level(pdf, dict.get("D").cloned())? {
            Some(d) => OutlineAction::GoTo { d },
            None => OutlineAction::Unknown(dict),
        },
        Some(b"GoToR") => match resolve_one_level(pdf, dict.get("F").cloned())? {
            Some(f) => {
                let d = resolve_one_level(pdf, dict.get("D").cloned())?;
                OutlineAction::GoToR { f, d }
            }
            None => OutlineAction::Unknown(dict),
        },
        Some(b"URI") => {
            match resolve_one_level(pdf, dict.get("URI").cloned())?.and_then(Object::into_string) {
                Some(uri) => OutlineAction::Uri { uri },
                None => OutlineAction::Unknown(dict),
            }
        }
        Some(b"Launch") => match resolve_one_level(pdf, dict.get("F").cloned())? {
            Some(f) => OutlineAction::Launch { f },
            None => OutlineAction::Unknown(dict),
        },
        Some(b"Named") => {
            match resolve_one_level(pdf, dict.get("N").cloned())?.and_then(Object::into_name) {
                Some(n) => OutlineAction::Named { n },
                None => OutlineAction::Unknown(dict),
            }
        }
        _ => OutlineAction::Unknown(dict),
    })
}

/// Recursive step of [`OutlineDocumentHelper::action_chain_with_max_depth`]:
/// classify `value` as an action (or an array of actions), append every
/// action found to `out` in visitation order, then repeat for each action's
/// own `/Next`.
///
/// `depth` bounds the recursion (both `/Next` hops and array descent);
/// `visited` records every indirect action object already processed so a
/// self- or mutually-referencing `/Next` (or the same action object reached
/// through more than one array element) is visited at most once — this is
/// what makes the traversal safe against a hostile PDF: a shared `visited`
/// set bounds total work by the document's distinct object count, not by
/// `depth` raised to the branching factor of nested `/Next` arrays.
fn collect_action_chain<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: Object,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
    out: &mut Vec<OutlineAction>,
) -> Result<()> {
    if depth == 0 {
        return Ok(()); // Chain depth exhausted (hostile/pathological /Next chain).
    }
    let resolved = match value {
        Object::Reference(r) => {
            if !visited.insert(r) {
                return Ok(()); // Cycle: this action object was already visited.
            }
            pdf.resolve(r)?
        }
        other => other,
    };
    match resolved {
        Object::Array(arr) => {
            for elem in arr {
                collect_action_chain(pdf, elem, visited, depth - 1, out)?;
            }
            Ok(())
        }
        Object::Dictionary(dict) => {
            // `/Next` is read raw and forwarded to the recursive call, which
            // resolves the `Object::Reference` case at its top (also feeding
            // `visited` for cycle detection). Resolving here would duplicate
            // that logic and split the cycle-check across two sites.
            let next = dict.get("Next").cloned();
            out.push(action_from_dict(pdf, dict)?);
            if let Some(next) = next {
                collect_action_chain(pdf, next, visited, depth - 1, out)?;
            }
            Ok(())
        }
        _ => Ok(()), // Not a conforming action dictionary or action array.
    }
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

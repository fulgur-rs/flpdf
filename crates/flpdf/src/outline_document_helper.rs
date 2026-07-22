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
//! document with deeply nested outline levels cannot overflow
//! the call stack; a shared visited set still cuts short any `/Next` or
//! `/First` cycle.
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
//!
//! qpdf-incompatible outline policy APIs were removed before flpdf 1.0.
//!
//! ```compile_fail
//! use flpdf::Dest;
//! ```
//!
//! ```compile_fail
//! use flpdf::{check_legacy_dests, check_name_tree_dests, check_outline_links};
//! ```
//!
//! ```compile_fail
//! use flpdf::{prune_outline_se, prune_outline_se_with_max_depth};
//! ```
//!
//! ```compile_fail
//! # use flpdf::Pdf;
//! # use std::io::Cursor;
//! # let mut pdf = Pdf::open(Cursor::new(Vec::<u8>::new())).unwrap();
//! let _ = pdf.outline().get_root_with_max_depth(10);
//! ```

use crate::name_number_tree::read_name_tree;
use crate::{Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

/// Depth bound used by the raw node destination resolver's name-tree lookup.
pub const DEFAULT_MAX_OUTLINE_DEPTH: usize = crate::outline::DEFAULT_MAX_OUTLINE_DEPTH;

/// Temporary construction cap for the current recursive `OutlineNode` value.
/// This remains private and is not a caller-configurable policy.
const TEMPORARY_OUTLINE_BUILD_DEPTH: usize = 5_000;

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
    /// the temporary internal construction cap. Propagates any error from resolving outline
    /// objects (for example I/O or parse failures surfaced by [`Pdf::resolve`]).
    pub fn get_root(&mut self) -> Result<Vec<OutlineNode>> {
        let Some(first) = self.outline_root_first()? else {
            return Ok(Vec::new());
        };
        let mut visited = BTreeSet::new();
        self.build_siblings(first, 0, None, &mut visited, TEMPORARY_OUTLINE_BUILD_DEPTH)
    }

    /// Build a `/First`->`/Next` sibling chain into owned nodes, descending
    /// into each item's own `/First` children.
    ///
    /// Implemented as an explicit work stack rather than by native recursion:
    /// `current` is the sibling chain currently being extended and
    /// `ancestors` holds every enclosing chain waiting for it (or one of its
    /// descendants) to finish. This is what lets [`Self::get_root`] use a
    /// large internal depth bound without risking a
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

    /// Like [`Self::catalog_value`] but follows the full indirect reference
    /// chain to its terminal object. Used by the raw named-destination lookup
    /// so a `/Dests` or `/Names` dictionary behind multiple holders resolves.
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

    /// Pre-order iterator over every materialized node (owned). Each yielded
    /// node has its `children` cleared — the flattened view is linear and
    /// `depth` conveys structure; use [`get_root`](Self::get_root) or
    /// [`walk`](Self::walk) when you need populated `children`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Unsupported`] if the outline nesting depth exceeds
    /// the temporary internal construction cap. Propagates any error from resolving outline
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
    /// the temporary internal construction cap. Propagates any error from resolving outline
    /// objects (for example I/O or parse failures surfaced by [`Pdf::resolve`]).
    pub fn walk<F: FnMut(&OutlineNode, usize)>(&mut self, mut visitor: F) -> Result<()> {
        let roots = self.get_root()?;
        walk_nodes(&roots, &mut visitor);
        Ok(())
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
/// recursive, so a deeply nested tree can't overflow the call stack the way
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
/// used by [`OutlineDocumentHelper::build_siblings`].
fn too_deep(max_depth: usize, at: ObjectRef) -> crate::Error {
    crate::Error::Unsupported(format!(
        "outline depth exceeds maximum of {max_depth} at {at}"
    ))
}

/// Resolve one level of indirection and read an integer (review rule 2/3).
fn resolve_int<R: Read + Seek>(pdf: &mut Pdf<R>, value: Option<Object>) -> Result<Option<i64>> {
    match value {
        Some(Object::Reference(r)) => Ok(pdf.resolve(r)?.as_integer()),
        Some(other) => Ok(other.as_integer()),
        None => Ok(None),
    }
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

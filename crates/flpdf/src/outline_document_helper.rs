//! High-level outline (`/Outlines`) document helper.
//!
//! [`OutlineDocumentHelper`] wraps a `&mut Pdf<R>` and materializes the document
//! outline (bookmarks) into an arena-backed [`crate::OutlineTree`], mirroring
//! qpdf's raw-object traversal for direct and indirect outline values.
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
//!     let tree = pdf.outline().get_tree()?;
//!     for (depth, _id, item) in tree.preorder() {
//!         println!("{:indent$}{}", "", item.title, indent = (depth - 1) * 2);
//!     }
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

use crate::outline::{OutlineId, OutlineItem, OutlineTree};
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

const QPDF_MAX_EXPANDED_OUTLINE_DEPTH: usize = 50;

#[derive(Clone)]
enum OutlineCursor {
    Direct(Object),
    Indirect(ObjectRef),
}

impl OutlineCursor {
    fn from_object(object: Object) -> Option<Self> {
        match object {
            Object::Null => None,
            Object::Reference(reference) => Some(Self::Indirect(reference)),
            direct => Some(Self::Direct(direct)),
        }
    }

    fn source_ref(&self) -> Option<ObjectRef> {
        match self {
            Self::Direct(_) => None,
            Self::Indirect(reference) => Some(*reference),
        }
    }
}

fn object_key(object: &Object, key: &str) -> Object {
    match object {
        Object::Dictionary(dict) => dict.get(key).cloned().unwrap_or(Object::Null),
        _ => Object::Null,
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

    /// Return `true` if the resolved catalog `/Outlines` dictionary has a
    /// non-null `/First` value. Mirrors qpdf `hasOutlines` construction.
    ///
    /// # Errors
    ///
    /// Propagates errors from resolving the catalog and `/Outlines` cursor.
    pub fn has_outlines(&mut self) -> Result<bool> {
        let Some(outlines) = self.catalog_outlines()? else {
            return Ok(false);
        };
        let Some(cursor) = OutlineCursor::from_object(outlines) else {
            return Ok(false);
        };
        let Object::Dictionary(dict) = self.resolve_cursor(&cursor)? else {
            return Ok(false);
        };
        let Some(first) = dict.get("First").cloned() else {
            return Ok(false);
        };
        let Some(first_cursor) = OutlineCursor::from_object(first) else {
            return Ok(false);
        };
        Ok(!matches!(self.resolve_cursor(&first_cursor)?, Object::Null))
    }

    fn resolve_cursor(&mut self, cursor: &OutlineCursor) -> Result<Object> {
        match cursor {
            OutlineCursor::Direct(object) => Ok(object.clone()),
            OutlineCursor::Indirect(reference) => self.pdf.resolve(*reference),
        }
    }

    fn catalog_outlines(&mut self) -> Result<Option<Object>> {
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let Object::Dictionary(catalog) = self.pdf.resolve(catalog_ref)? else {
            return Ok(None);
        };
        Ok(catalog.get("Outlines").cloned())
    }

    /// Materialize the qpdf-compatible outline arena.
    ///
    /// # Errors
    ///
    /// Propagates outline-resolution errors.
    pub fn get_tree(&mut self) -> Result<OutlineTree> {
        let mut tree = OutlineTree::new();
        let Some(outlines) = self.catalog_outlines()? else {
            return Ok(tree);
        };
        let Some(outlines_cursor) = OutlineCursor::from_object(outlines) else {
            return Ok(tree);
        };
        let Object::Dictionary(outlines) = self.resolve_cursor(&outlines_cursor)? else {
            return Ok(tree);
        };
        let Some(first) = outlines.get("First").cloned() else {
            return Ok(tree);
        };
        let Some(mut cursor) = OutlineCursor::from_object(first) else {
            return Ok(tree);
        };

        let mut top_level_seen = BTreeSet::new();
        let mut constructor_seen = BTreeSet::new();
        loop {
            if let Some(reference) = cursor.source_ref() {
                if !top_level_seen.insert(reference) {
                    break;
                }
            }

            let Some(id) = self.build_item(cursor, None, &mut tree, &mut constructor_seen)? else {
                break;
            };
            tree.roots.push(id);
            let Some(next) = OutlineCursor::from_object(object_key(&tree[id].object, "Next"))
            else {
                break;
            };
            cursor = next;
        }
        Ok(tree)
    }

    /// Materialize one item and all descendants using an explicit frame stack.
    /// The stack preserves qpdf's constructor seen-set placement without using
    /// one native call frame per outline level.
    fn build_item(
        &mut self,
        cursor: OutlineCursor,
        parent: Option<OutlineId>,
        tree: &mut OutlineTree,
        constructor_seen: &mut BTreeSet<ObjectRef>,
    ) -> Result<Option<OutlineId>> {
        let Some(root) = self.materialize_item(cursor, parent, tree)? else {
            return Ok(None);
        };
        if let Some(reference) = tree[root].source_ref {
            if !constructor_seen.insert(reference) {
                return Ok(Some(root));
            }
        }

        struct Frame {
            owner: OutlineId,
            next: Option<OutlineCursor>,
            depth: usize,
        }

        let mut frames = Vec::new();
        let first = OutlineCursor::from_object(object_key(&tree[root].object, "First"));
        if first.is_some() {
            frames.push(Frame {
                owner: root,
                next: first,
                depth: 2,
            });
        }

        while !frames.is_empty() {
            let next_cursor = frames.last_mut().and_then(|frame| frame.next.take());
            let Some(cursor) = next_cursor else {
                frames.pop();
                continue;
            };
            let (owner, child_depth) = {
                let frame = frames.last().expect("outline construction frame exists");
                (frame.owner, frame.depth)
            };
            let Some(child) = self.materialize_item(cursor, Some(owner), tree)? else {
                continue;
            };
            tree.items[owner.0].kids.push(child);

            let expand_child = if child_depth > QPDF_MAX_EXPANDED_OUTLINE_DEPTH {
                false
            } else if let Some(reference) = tree[child].source_ref {
                constructor_seen.insert(reference)
            } else {
                true
            };

            // qpdf advances the parent's raw child `/Next` chain even when the
            // child's constructor seen check prevented that child expanding.
            frames
                .last_mut()
                .expect("outline construction frame exists")
                .next = OutlineCursor::from_object(object_key(&tree[child].object, "Next"));

            if expand_child {
                let first = OutlineCursor::from_object(object_key(&tree[child].object, "First"));
                if first.is_some() {
                    frames.push(Frame {
                        owner: child,
                        next: first,
                        depth: child_depth + 1,
                    });
                }
            }
        }

        Ok(Some(root))
    }

    fn materialize_item(
        &mut self,
        cursor: OutlineCursor,
        parent: Option<OutlineId>,
        tree: &mut OutlineTree,
    ) -> Result<Option<OutlineId>> {
        let source_ref = cursor.source_ref();
        let object = self.resolve_cursor(&cursor)?;
        if matches!(object, Object::Null) {
            return Ok(None);
        }
        let (title_src, count_src, dest_src, action_src) = match &object {
            Object::Dictionary(dict) => (
                dict.get("Title").cloned(),
                dict.get("Count").cloned(),
                dict.get("Dest").cloned(),
                dict.get("A").cloned(),
            ),
            _ => (None, None, None, None),
        };
        let title = resolve_title(self.pdf, title_src)?;
        let count = resolve_count(self.pdf, count_src)?;
        let dest = self.resolve_node_dest(dest_src.as_ref(), action_src.as_ref())?;
        let id = OutlineId(tree.items.len());
        tree.items.push(OutlineItem {
            source_ref,
            parent,
            kids: Vec::new(),
            object,
            title,
            count,
            dest,
        });

        Ok(Some(id))
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
        match &dests_root {
            Object::Dictionary(_) => {}
            Object::Reference(_) => {
                if !matches!(
                    crate::ref_chain::resolve_ref_chain(self.pdf, &dests_root)?.0,
                    Object::Dictionary(_)
                ) {
                    return Ok(Object::Null);
                }
            }
            _ => return Ok(Object::Null),
        }
        match find_name_tree_value(self.pdf, dests_root.clone(), &lookup)? {
            NameTreeLookup::Found(value) => return resolve_terminal_object(self.pdf, value),
            NameTreeLookup::Missing => {}
            NameTreeLookup::Structural { error, root } => {
                self.pdf.push_warning(format!(
                    "attempting to repair after error: {}",
                    error.diagnostic()
                ));
                let entries = enumerate_name_tree_entries(self.pdf, root.clone())?;
                let repaired_root = repair_name_tree(self.pdf, root, entries)?;
                if let NameTreeLookup::Found(value) =
                    find_name_tree_value(self.pdf, repaired_root, &lookup)?
                {
                    return resolve_terminal_object(self.pdf, value);
                }
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

enum NameTreeLookup {
    Found(Object),
    Missing,
    Structural {
        error: NameTreeStructuralError,
        root: Object,
    },
}

struct NameTreeStructuralError {
    node_ref: Option<ObjectRef>,
    message: String,
}

impl NameTreeStructuralError {
    fn diagnostic(&self) -> String {
        match self.node_ref {
            Some(node_ref) => format!(
                "Name/Number tree node (object {}): {}",
                node_ref.number, self.message
            ),
            None => format!("Name/Number tree node: {}", self.message),
        }
    }
}

fn name_tree_iterator_warning(node_ref: Option<ObjectRef>, message: &str) -> String {
    match node_ref {
        Some(node_ref) => format!(
            "Name/Number tree node (object {}): {message}",
            node_ref.number
        ),
        None => message.to_string(),
    }
}

enum NameTreeKidSelection {
    Found(Object),
    Structural(NameTreeStructuralError),
}

enum NameTreeKidOrdering {
    Order(Ordering),
    Structural(NameTreeStructuralError),
}

enum NameTreeBinarySearch {
    Found(usize),
    Missing,
    Structural(NameTreeStructuralError),
}

enum NameTreeFirstBoundary {
    Empty,
    Invalid,
    Key(Vec<u8>),
    Structural(NameTreeStructuralError),
}

/// Find one key in a name tree using qpdf's `/Names`-or-`/Kids` descent.
///
/// Unlike the generic public tree walker, outline destination lookup has no
/// caller policy or fixed depth cap and never enumerates unrelated branches.
fn find_name_tree_value<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    mut cursor: Object,
    lookup: &[u8],
) -> Result<NameTreeLookup> {
    let (updated_root, first_boundary) = name_tree_begin_preflight(pdf, cursor)?;
    cursor = updated_root.clone();
    match first_boundary {
        NameTreeFirstBoundary::Empty => return Ok(NameTreeLookup::Missing),
        NameTreeFirstBoundary::Structural(error) => {
            return Ok(NameTreeLookup::Structural {
                error,
                root: updated_root,
            });
        }
        NameTreeFirstBoundary::Key(first) if lookup < first.as_slice() => {
            return Ok(NameTreeLookup::Missing);
        }
        NameTreeFirstBoundary::Invalid | NameTreeFirstBoundary::Key(_) => {}
    }

    let root_ref = cursor.as_ref_id();
    let mut seen = BTreeSet::new();
    loop {
        let (node, identity) = name_tree_node(pdf, cursor)?;
        let Some(mut node) = node else {
            return Ok(NameTreeLookup::Missing); // cov:ignore: begin preflight and kid comparison reject non-dictionary targeted cursors
        };
        if let Some(identity) = identity {
            if !seen.insert(identity) {
                return Ok(NameTreeLookup::Structural {
                    error: NameTreeStructuralError {
                        node_ref: Some(identity),
                        message: "loop detected in find".to_string(),
                    },
                    root: updated_root,
                });
            }
        }

        let names_value = node.remove("Names");
        if let Some(Object::Array(names)) = names_value {
            if !names.is_empty() {
                return find_name_tree_leaf_value(names, lookup, root_ref, updated_root);
            }
        }

        let Some(Object::Array(kids)) = node.remove("Kids") else {
            return Ok(NameTreeLookup::Structural {
                error: NameTreeStructuralError {
                    node_ref: identity,
                    message: "bad node during find".to_string(),
                },
                root: updated_root,
            });
        };
        if kids.is_empty() {
            return Ok(NameTreeLookup::Structural {
                error: NameTreeStructuralError {
                    node_ref: identity,
                    message: "bad node during find".to_string(),
                },
                root: updated_root,
            });
        }
        match select_name_tree_kid(pdf, &kids, lookup, root_ref, identity)? {
            NameTreeKidSelection::Found(next) => {
                cursor = next;
            }
            NameTreeKidSelection::Structural(error) => {
                return Ok(NameTreeLookup::Structural {
                    error,
                    root: updated_root,
                });
            }
        }
    }
}

/// Reproduce qpdf's `begin()` preflight in `NNTreeImpl::findInternal`.
///
/// qpdf 11.9.0 assigns `last_item = end()`, so only the first boundary is
/// effective. While descending to it, auto-repair converts every direct kid
/// to a fresh indirect object and replaces the actual parent array slot.
fn name_tree_begin_preflight<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    root: Object,
) -> Result<(Object, NameTreeFirstBoundary)> {
    let mut updated_root = root.clone();
    let mut cursor = root;
    let mut seen = BTreeSet::new();
    let mut last_object_number = pdf
        .object_refs()
        .iter()
        .map(|object_ref| object_ref.number)
        .max()
        .unwrap_or(0);

    loop {
        let (node, identity) = name_tree_node(pdf, cursor)?;
        let Some(mut node) = node else {
            pdf.push_warning(name_tree_iterator_warning(
                identity,
                "non-dictionary node while traversing name/number tree",
            ));
            return Ok((updated_root, NameTreeFirstBoundary::Empty));
        };
        if let Some(identity) = identity {
            if !seen.insert(identity) {
                pdf.push_warning(name_tree_iterator_warning(
                    Some(identity),
                    "loop detected while traversing name/number tree",
                ));
                return Ok((updated_root, NameTreeFirstBoundary::Empty));
            }
        }

        let mut has_empty_names = false;
        if let Some(Object::Array(names)) = node.get("Names") {
            if !names.is_empty() {
                if names.len() < 2 {
                    return Ok((
                        updated_root,
                        NameTreeFirstBoundary::Structural(NameTreeStructuralError {
                            node_ref: identity,
                            message: "update ivalue: items array is too short".to_string(),
                        }),
                    ));
                }
                let boundary = match names.first() {
                    Some(Object::String(key)) => {
                        NameTreeFirstBoundary::Key(crate::json_inspect::qpdf_utf8_value(key))
                    }
                    _ => NameTreeFirstBoundary::Invalid,
                };
                return Ok((updated_root, boundary));
            }
            has_empty_names = true;
        }

        let Some(Object::Array(mut kids)) = node.remove("Kids") else {
            if has_empty_names {
                return Ok((updated_root, NameTreeFirstBoundary::Empty));
            }
            pdf.push_warning(name_tree_iterator_warning(
                identity,
                "name/number tree node has neither non-empty /Names nor /Kids",
            ));
            return Ok((updated_root, NameTreeFirstBoundary::Empty));
        };
        if kids.is_empty() {
            if has_empty_names {
                return Ok((updated_root, NameTreeFirstBoundary::Empty));
            }
            pdf.push_warning(name_tree_iterator_warning(
                identity,
                "name/number tree node has neither non-empty /Names nor /Kids",
            ));
            return Ok((updated_root, NameTreeFirstBoundary::Empty));
        }

        let next = kids[0].clone();
        let next = if !matches!(next, Object::Reference(_)) {
            pdf.push_warning(name_tree_iterator_warning(
                identity,
                "converting kid number 0 to an indirect object",
            ));
            last_object_number = last_object_number
                .checked_add(1)
                .ok_or_else(|| Error::Unsupported("object-number space exhausted".to_string()))?;
            let next_ref = ObjectRef::new(last_object_number, 0);
            pdf.set_object(next_ref, next);
            let next = Object::Reference(next_ref);
            kids[0] = next.clone();
            node.insert("Kids", Object::Array(kids));

            if let Some(identity) = identity {
                pdf.set_object(identity, Object::Dictionary(node));
            } else {
                replace_direct_dests_root(pdf, node.clone())?;
                updated_root = Object::Dictionary(node);
            }
            next
        } else {
            next
        };
        cursor = next;
    }
}

fn name_tree_node<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    cursor: Object,
) -> Result<(Option<Dictionary>, Option<ObjectRef>)> {
    match cursor {
        Object::Dictionary(node) => Ok((Some(node), None)),
        Object::Reference(reference) => {
            let (terminal, terminal_ref) =
                crate::ref_chain::resolve_ref_chain(pdf, &Object::Reference(reference))?;
            Ok((terminal.into_dict(), terminal_ref.or(Some(reference))))
        }
        _ => Ok((None, None)),
    }
}

fn find_name_tree_leaf_value(
    names: Vec<Object>,
    lookup: &[u8],
    root_ref: Option<ObjectRef>,
    root: Object,
) -> Result<NameTreeLookup> {
    let pair_count = names.len() / 2;
    Ok(
        match qpdf_name_tree_binary_search(pair_count, false, |index| {
            let Object::String(stored) = &names[2 * index] else {
                return Ok(NameTreeKidOrdering::Structural(NameTreeStructuralError {
                    node_ref: root_ref,
                    message: format!("item at index {} is not the right type", 2 * index),
                }));
            };
            Ok(NameTreeKidOrdering::Order(lookup.cmp(
                crate::json_inspect::qpdf_utf8_value(stored).as_slice(),
            )))
        })? {
            NameTreeBinarySearch::Found(index) => {
                NameTreeLookup::Found(names[2 * index + 1].clone())
            }
            NameTreeBinarySearch::Missing => NameTreeLookup::Missing,
            NameTreeBinarySearch::Structural(error) => NameTreeLookup::Structural { error, root },
        },
    )
}

fn select_name_tree_kid<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    kids: &[Object],
    lookup: &[u8],
    root_ref: Option<ObjectRef>,
    node_ref: Option<ObjectRef>,
) -> Result<NameTreeKidSelection> {
    Ok(
        match qpdf_name_tree_binary_search(kids.len(), true, |index| {
            name_tree_kid_ordering(pdf, &kids[index], lookup, index, root_ref)
        })? {
            NameTreeBinarySearch::Found(index) => NameTreeKidSelection::Found(kids[index].clone()),
            NameTreeBinarySearch::Missing => {
                NameTreeKidSelection::Structural(NameTreeStructuralError {
                    node_ref,
                    message: "unexpected -1 from binary search of kids; limits may by wrong"
                        .to_string(),
                })
            }
            NameTreeBinarySearch::Structural(error) => NameTreeKidSelection::Structural(error),
        },
    )
}

fn qpdf_name_tree_binary_search<F>(
    num_items: usize,
    return_prev_if_not_found: bool,
    mut compare: F,
) -> Result<NameTreeBinarySearch>
where
    F: FnMut(usize) -> Result<NameTreeKidOrdering>,
{
    let mut max_idx = 1;
    while max_idx < num_items {
        max_idx <<= 1;
    }

    let mut step = max_idx / 2;
    let mut checks = max_idx;
    let mut index = step;
    let mut found_index = None;
    let mut found = false;
    let mut found_leq = false;

    while !found && checks > 0 {
        let status = if index < num_items {
            match compare(index)? {
                NameTreeKidOrdering::Order(ordering) => {
                    if ordering != Ordering::Less {
                        found_leq = true;
                        found_index = Some(index);
                    }
                    ordering
                }
                NameTreeKidOrdering::Structural(error) => {
                    return Ok(NameTreeBinarySearch::Structural(error));
                }
            }
        } else {
            Ordering::Less
        };

        if status == Ordering::Equal {
            found = true;
        } else {
            checks >>= 1;
            if checks > 0 {
                step >>= 1;
                if step == 0 {
                    step = 1;
                }
                if status == Ordering::Less {
                    index -= step;
                } else {
                    index += step;
                }
            }
        }
    }

    Ok(if found || (found_leq && return_prev_if_not_found) {
        NameTreeBinarySearch::Found(
            found_index.expect("qpdf binary search records every exact or prior match"),
        )
    } else {
        NameTreeBinarySearch::Missing
    })
}

/// Return the qpdf `withinLimits` comparison for `lookup` against one kid:
/// less when before the range, greater when after it, equal when within it.
fn name_tree_kid_ordering<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    kid: &Object,
    lookup: &[u8],
    kid_index: usize,
    root_ref: Option<ObjectRef>,
) -> Result<NameTreeKidOrdering> {
    let (node, _) = name_tree_node(pdf, kid.clone())?;
    let Some(node) = node else {
        return Ok(NameTreeKidOrdering::Structural(NameTreeStructuralError {
            node_ref: root_ref,
            message: format!("invalid kid at index {kid_index}"),
        }));
    };
    let Some(Object::Array(limits)) = node.get("Limits") else {
        return Ok(NameTreeKidOrdering::Structural(NameTreeStructuralError {
            node_ref: kid.as_ref_id(),
            message: "node is missing /Limits".to_string(),
        }));
    };
    let [Object::String(first), Object::String(last), ..] = limits.as_slice() else {
        return Ok(NameTreeKidOrdering::Structural(NameTreeStructuralError {
            node_ref: kid.as_ref_id(),
            message: "node is missing /Limits".to_string(),
        }));
    };
    let first = crate::json_inspect::qpdf_utf8_value(first);
    if lookup < first.as_slice() {
        return Ok(NameTreeKidOrdering::Order(Ordering::Less));
    }
    let last = crate::json_inspect::qpdf_utf8_value(last);
    if lookup > last.as_slice() {
        return Ok(NameTreeKidOrdering::Order(Ordering::Greater));
    }
    Ok(NameTreeKidOrdering::Order(Ordering::Equal))
}

/// Enumerate the reachable entries only when qpdf's targeted lookup reports a
/// structural error. This private repair walk is iterative, has no `/Kids`
/// depth cap, and keys cycle detection on the terminal indirect node identity.
fn enumerate_name_tree_entries<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    root: Object,
) -> Result<BTreeMap<Vec<u8>, Object>> {
    enum RepairWalkItem {
        Node { cursor: Object, at_root: bool },
        InvalidKid(usize),
    }

    let mut entries = BTreeMap::new();
    let mut stack = vec![RepairWalkItem::Node {
        cursor: root,
        at_root: true,
    }];
    let mut seen = BTreeSet::new();

    while let Some(item) = stack.pop() {
        let (cursor, at_root) = match item {
            RepairWalkItem::Node { cursor, at_root } => (cursor, at_root),
            RepairWalkItem::InvalidKid(kid_index) => {
                pdf.push_warning(format!("skipping over invalid kid at index {kid_index}"));
                continue;
            }
        };
        let (node, identity) = name_tree_node(pdf, cursor)?;
        let Some(mut node) = node else {
            continue; // cov:ignore: only validated dictionary kids are scheduled by this repair walk
        };
        if let Some(identity) = identity {
            if !seen.insert(identity) {
                pdf.push_warning("loop detected while traversing name/number tree");
                continue;
            }
        }

        let names_value = node.remove("Names");
        let mut allow_empty_root = false;
        if let Some(Object::Array(names)) = names_value {
            if !names.is_empty() {
                if names.len() < 2 {
                    return Err(Error::parse(
                        0,
                        NameTreeStructuralError {
                            node_ref: identity,
                            message: "update ivalue: items array is too short".to_string(),
                        }
                        .diagnostic(),
                    ));
                }
                let mut pairs = names.chunks_exact(2);
                for (pair_index, pair) in pairs.by_ref().enumerate() {
                    let Object::String(key) = &pair[0] else {
                        pdf.push_warning(format!("item {} has the wrong type", pair_index * 2));
                        continue;
                    };
                    let key =
                        qpdf_new_unicode_utf8_value(&crate::json_inspect::qpdf_utf8_value(key));
                    entries.insert(key, pair[1].clone());
                }
                if !pairs.remainder().is_empty() {
                    pdf.push_warning("items array doesn't have enough elements");
                }
                continue;
            }
            allow_empty_root = at_root;
        }

        if let Some(Object::Array(kids)) = node.remove("Kids") {
            if kids.is_empty() {
                if !allow_empty_root {
                    pdf.push_warning(name_tree_iterator_warning(
                        identity,
                        "name/number tree node has neither non-empty /Names nor /Kids",
                    ));
                }
                continue;
            }
            for (kid_index, kid) in kids.into_iter().enumerate().rev() {
                let (kid_node, _) = name_tree_node(pdf, kid.clone())?;
                let traversable = kid_node.as_ref().is_some_and(|kid_node| {
                    kid_node.get("Kids").is_some() || kid_node.get("Names").is_some()
                });
                if traversable {
                    stack.push(RepairWalkItem::Node {
                        cursor: kid,
                        at_root: false,
                    });
                } else {
                    stack.push(RepairWalkItem::InvalidKid(kid_index));
                }
            }
        } else if !allow_empty_root {
            pdf.push_warning(name_tree_iterator_warning(
                identity,
                "name/number tree node has neither non-empty /Names nor /Kids",
            ));
        }
    }

    Ok(entries)
}

fn repair_name_tree<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    original_root: Object,
    entries: BTreeMap<Vec<u8>, Object>,
) -> Result<Object> {
    let rebuilt_entries: Vec<(Vec<u8>, Object)> = entries
        .into_iter()
        .map(|(key, value)| (qpdf_unicode_string_bytes(&key), value))
        .collect();
    let rebuilt = build_repaired_name_tree_root(pdf, &rebuilt_entries)?;

    let (existing, terminal_ref) = name_tree_node(pdf, original_root.clone())?;
    let Some(mut existing) = existing else {
        return Ok(original_root); // cov:ignore: structural lookup only yields repair for a dictionary root
    };
    existing.remove("Kids");
    existing.remove("Names");
    if let Some(kids) = rebuilt.get("Kids") {
        existing.insert("Kids", kids.clone());
    }
    if let Some(names) = rebuilt.get("Names") {
        existing.insert("Names", names.clone());
    }

    match original_root {
        Object::Reference(_) => {
            let Some(terminal_ref) = terminal_ref else {
                return Ok(original_root); // cov:ignore: name_tree_node always returns identity for a reference root
            };
            pdf.set_object(terminal_ref, Object::Dictionary(existing));
            Ok(original_root)
        }
        Object::Dictionary(_) => {
            replace_direct_dests_root(pdf, existing.clone())?;
            Ok(Object::Dictionary(existing))
        }
        _ => Ok(original_root), // cov:ignore: structural lookup cannot originate from a scalar root
    }
}

fn build_repaired_name_tree_root<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    entries: &[(Vec<u8>, Object)],
) -> Result<Dictionary> {
    let max_object_number = pdf
        .object_refs()
        .iter()
        .map(|object_ref| object_ref.number)
        .max()
        .unwrap_or(0);
    let allocation_bound = u32::try_from(entries.len())
        .ok()
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| Error::Unsupported("object-number space exhausted".to_string()))?;
    max_object_number
        .checked_add(allocation_bound)
        .ok_or_else(|| Error::Unsupported("object-number space exhausted".to_string()))?;

    let mut next_object_number = max_object_number + 1;
    let mut nodes = vec![RepairedNameTreeNode {
        reference: None,
        kind: RepairedNameTreeNodeKind::Leaf(Vec::new()),
    }];

    for entry in entries.iter().cloned() {
        let mut path = vec![0];
        loop {
            let node_index = *path.last().expect("repair path contains the root");
            match &nodes[node_index].kind {
                RepairedNameTreeNodeKind::Leaf(_) => break,
                RepairedNameTreeNodeKind::Branch(kids) => {
                    path.push(*kids.last().expect("repair branch contains a child"));
                }
            }
        }
        let leaf_index = *path.last().expect("repair path contains a leaf");
        let RepairedNameTreeNodeKind::Leaf(leaf_entries) = &mut nodes[leaf_index].kind else {
            unreachable!("repair descent terminates at a leaf"); // cov:ignore: descent stops only on the Leaf match arm
        };
        leaf_entries.push(entry);

        while repaired_name_tree_node_overflows(&nodes[*path.last().unwrap()]) {
            let node_index = *path.last().unwrap();
            if path.len() == 1 {
                let old_kind = std::mem::replace(
                    &mut nodes[0].kind,
                    RepairedNameTreeNodeKind::Branch(Vec::new()),
                );
                let first_index = nodes.len();
                nodes.push(RepairedNameTreeNode {
                    reference: Some(ObjectRef::new(next_object_number, 0)),
                    kind: old_kind,
                });
                next_object_number += 1;
                let second_index =
                    split_repaired_name_tree_node(&mut nodes, first_index, &mut next_object_number);
                let RepairedNameTreeNodeKind::Branch(root_kids) = &mut nodes[0].kind else {
                    unreachable!("overflowing root becomes a branch"); // cov:ignore: root kind was replaced with Branch immediately above
                };
                root_kids.extend([first_index, second_index]);
                break;
            }

            let parent_index = path[path.len() - 2];
            let second_index =
                split_repaired_name_tree_node(&mut nodes, node_index, &mut next_object_number);
            let RepairedNameTreeNodeKind::Branch(parent_kids) = &mut nodes[parent_index].kind
            else {
                unreachable!("repair path parent is a branch"); // cov:ignore: path adds children only from Branch nodes
            };
            let child_position = parent_kids
                .iter()
                .position(|&child| child == node_index)
                .expect("repair path child belongs to parent");
            parent_kids.insert(child_position + 1, second_index);
            path.pop();
        }
    }

    let root = repaired_name_tree_dictionary(&nodes, 0, false);
    for node_index in 1..nodes.len() {
        let node_ref = nodes[node_index]
            .reference
            .expect("every repaired non-root node is indirect");
        pdf.set_object(
            node_ref,
            Object::Dictionary(repaired_name_tree_dictionary(&nodes, node_index, true)),
        );
    }
    Ok(root)
}

enum RepairedNameTreeNodeKind {
    Leaf(Vec<(Vec<u8>, Object)>),
    Branch(Vec<usize>),
}

struct RepairedNameTreeNode {
    reference: Option<ObjectRef>,
    kind: RepairedNameTreeNodeKind,
}

fn repaired_name_tree_node_overflows(node: &RepairedNameTreeNode) -> bool {
    match &node.kind {
        RepairedNameTreeNodeKind::Leaf(entries) => entries.len() > 32,
        RepairedNameTreeNodeKind::Branch(kids) => kids.len() > 32,
    }
}

fn split_repaired_name_tree_node(
    nodes: &mut Vec<RepairedNameTreeNode>,
    node_index: usize,
    next_object_number: &mut u32,
) -> usize {
    let second_kind = match &mut nodes[node_index].kind {
        RepairedNameTreeNodeKind::Leaf(entries) => {
            RepairedNameTreeNodeKind::Leaf(entries.split_off(16))
        }
        RepairedNameTreeNodeKind::Branch(kids) => {
            RepairedNameTreeNodeKind::Branch(kids.split_off(16))
        }
    };
    let second_index = nodes.len();
    nodes.push(RepairedNameTreeNode {
        reference: Some(ObjectRef::new(*next_object_number, 0)),
        kind: second_kind,
    });
    *next_object_number += 1;
    second_index
}

fn repaired_name_tree_dictionary(
    nodes: &[RepairedNameTreeNode],
    node_index: usize,
    include_limits: bool,
) -> Dictionary {
    let mut dictionary = Dictionary::new();
    match &nodes[node_index].kind {
        RepairedNameTreeNodeKind::Leaf(entries) => {
            let mut names = Vec::with_capacity(entries.len() * 2);
            for (key, value) in entries {
                names.push(Object::String(key.clone()));
                names.push(value.clone());
            }
            dictionary.insert("Names", Object::Array(names));
        }
        RepairedNameTreeNodeKind::Branch(kids) => {
            dictionary.insert(
                "Kids",
                Object::Array(
                    kids.iter()
                        .map(|&kid| {
                            Object::Reference(
                                nodes[kid]
                                    .reference
                                    .expect("every repaired child node is indirect"),
                            )
                        })
                        .collect(),
                ),
            );
        }
    }
    if include_limits {
        let first = repaired_name_tree_limit(nodes, node_index, true);
        let last = repaired_name_tree_limit(nodes, node_index, false);
        dictionary.insert(
            "Limits",
            Object::Array(vec![Object::String(first), Object::String(last)]),
        );
    }
    dictionary
}

fn repaired_name_tree_limit(
    nodes: &[RepairedNameTreeNode],
    mut node_index: usize,
    first: bool,
) -> Vec<u8> {
    loop {
        match &nodes[node_index].kind {
            RepairedNameTreeNodeKind::Leaf(entries) => {
                return if first {
                    entries.first()
                } else {
                    entries.last()
                }
                .expect("only non-empty repaired child nodes have limits")
                .0
                .clone();
            }
            RepairedNameTreeNodeKind::Branch(kids) => {
                node_index = if first {
                    *kids.first().expect("repair branch contains a child")
                } else {
                    *kids.last().expect("repair branch contains a child")
                };
            }
        }
    }
}

fn replace_direct_dests_root<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    repaired_root: Dictionary,
) -> Result<()> {
    let catalog_ref = pdf.root_ref().ok_or(Error::Missing("/Root"))?;
    let Object::Dictionary(mut catalog) = pdf.resolve(catalog_ref)? else {
        return Ok(()); // cov:ignore: this root was a dictionary when the direct /Dests root was read
    };
    let Some(names_value) = catalog.get("Names").cloned() else {
        return Ok(()); // cov:ignore: repair follows a /Dests root just read through catalog /Names
    };

    match names_value {
        Object::Dictionary(mut names) => {
            names.insert("Dests", Object::Dictionary(repaired_root));
            catalog.insert("Names", Object::Dictionary(names));
            pdf.set_object(catalog_ref, Object::Dictionary(catalog));
        }
        value @ Object::Reference(_) => {
            let (terminal, terminal_ref) = crate::ref_chain::resolve_ref_chain(pdf, &value)?;
            let Some(mut names) = terminal.into_dict() else {
                return Ok(()); // cov:ignore: the same terminal /Names value was a dictionary before lookup
            };
            let Some(terminal_ref) = terminal_ref else {
                return Ok(()); // cov:ignore: resolve_ref_chain always returns identity for a reference start
            };
            names.insert("Dests", Object::Dictionary(repaired_root));
            pdf.set_object(terminal_ref, Object::Dictionary(names));
        }
        _ => {} // cov:ignore: initial lookup accepts only direct or indirect /Names dictionaries
    }
    Ok(())
}

/// Encode the normalized UTF-8 key as qpdf `newUnicodeString`: PDFDocEncoding
/// when every scalar is representable, otherwise UTF-16BE with a BOM.
fn qpdf_unicode_string_bytes(utf8: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(utf8);
    let mut pdfdoc = Vec::with_capacity(text.len());
    for character in text.chars() {
        let mut encoded_character = [0; 4];
        let encoded_character = character.encode_utf8(&mut encoded_character).as_bytes();
        let encoded = (1_u16..=u16::from(u8::MAX))
            .map(|byte| byte as u8)
            .filter(|byte| !matches!(byte, 0x7f | 0x9f | 0xad))
            .find(|&byte| crate::json_inspect::qpdf_utf8_value(&[byte]) == encoded_character);
        let Some(encoded) = encoded else {
            return crate::filespec_helper::encode_utf16be(&text);
        };
        pdfdoc.push(encoded);
    }
    pdfdoc
}

/// Decode an outline `/Title`, resolving one level of indirection (review rule 2).
fn resolve_title<R: Read + Seek>(pdf: &mut Pdf<R>, value: Option<Object>) -> Result<String> {
    let Some(value) = value else {
        return Ok(String::new());
    };
    let resolved = resolve_scalar(pdf, value)?;
    Ok(qpdf_title(pdf, resolved))
}

fn qpdf_title<R: Read + Seek>(pdf: &mut Pdf<R>, value: Object) -> String {
    match value {
        Object::String(bytes) => {
            String::from_utf8_lossy(&crate::json_inspect::qpdf_utf8_value(&bytes)).into_owned()
        }
        other => {
            pdf.push_warning(format!(
                "operation for string attempted on object of type {}: returning empty string",
                qpdf_object_type_name(&other)
            ));
            String::new()
        }
    }
}

/// Read an outline `/Count`, resolving one level of indirection (review rule 2/3).
fn resolve_count<R: Read + Seek>(pdf: &mut Pdf<R>, value: Option<Object>) -> Result<i32> {
    let Some(value) = value else {
        return Ok(0);
    };
    let resolved = resolve_scalar(pdf, value)?;
    Ok(qpdf_count(pdf, resolved))
}

fn qpdf_count<R: Read + Seek>(pdf: &mut Pdf<R>, value: Object) -> i32 {
    let Object::Integer(value) = value else {
        pdf.push_warning(format!(
            "operation for integer attempted on object of type {}: returning 0",
            qpdf_object_type_name(&value)
        ));
        return 0;
    };
    if value < i64::from(i32::MIN) {
        pdf.push_warning("requested value of integer is too small; returning INT_MIN");
        i32::MIN
    } else if value > i64::from(i32::MAX) {
        pdf.push_warning("requested value of integer is too big; returning INT_MAX");
        i32::MAX
    } else {
        value as i32
    }
}

fn resolve_scalar<R: Read + Seek>(pdf: &mut Pdf<R>, value: Object) -> Result<Object> {
    match value {
        Object::Reference(r) => pdf.resolve(r),
        other => Ok(other),
    }
}

fn qpdf_object_type_name(value: &Object) -> &'static str {
    match value {
        Object::Null => "null",
        Object::Boolean(_) => "boolean",
        Object::Integer(_) => "integer",
        Object::Real(_) | Object::RealLiteral { .. } => "real",
        Object::Name(_) => "name",
        Object::String(_) => "string",
        Object::Array(_) => "array",
        Object::Dictionary(_) => "dictionary",
        Object::Stream(_) => "stream",
        Object::Reference(_) => "reference",
    }
}

#[cfg(test)]
mod qpdf_utf8_tests {
    use super::{
        qpdf_name_tree_binary_search, qpdf_new_unicode_utf8_value, qpdf_unicode_string_bytes,
        NameTreeBinarySearch, NameTreeKidOrdering, NameTreeStructuralError,
    };

    #[test]
    fn direct_name_tree_node_diagnostic_omits_an_object_number() {
        let error = NameTreeStructuralError {
            node_ref: None,
            message: "node is missing /Limits".to_string(),
        };
        assert_eq!(
            error.diagnostic(),
            "Name/Number tree node: node is missing /Limits"
        );
    }

    #[test]
    fn binary_search_uses_qpdf_power_of_two_visit_order() {
        let cases = [
            (3, 2, vec![2]),
            (4, 3, vec![2, 3]),
            (5, 0, vec![4, 2, 1, 0]),
        ];
        for (num_items, target, expected_visits) in cases {
            let mut visits = Vec::new();
            let result = qpdf_name_tree_binary_search(num_items, false, |index| {
                visits.push(index);
                Ok(NameTreeKidOrdering::Order(target.cmp(&index)))
            })
            .unwrap();
            assert!(matches!(result, NameTreeBinarySearch::Found(index) if index == target));
            assert_eq!(visits, expected_visits);
        }

        let mut visits = Vec::new();
        let result = qpdf_name_tree_binary_search(3, false, |index| {
            visits.push(index);
            Ok(NameTreeKidOrdering::Order(3_usize.cmp(&index)))
        })
        .unwrap();
        assert!(matches!(result, NameTreeBinarySearch::Missing));
        assert_eq!(visits, [2, 2]);
    }

    #[test]
    fn repaired_keys_use_qpdf_new_unicode_string_encoding() {
        assert_eq!(qpdf_unicode_string_bytes(b"shape"), b"shape");
        assert_eq!(qpdf_unicode_string_bytes("Ł".as_bytes()), vec![0x95]);
        assert_eq!(
            qpdf_unicode_string_bytes("😀".as_bytes()),
            vec![0xfe, 0xff, 0xd8, 0x3d, 0xde, 0x00]
        );
        assert_eq!(
            qpdf_unicode_string_bytes("�".as_bytes()),
            vec![0xfe, 0xff, 0xff, 0xfd]
        );
        assert_eq!(
            qpdf_unicode_string_bytes("\0".as_bytes()),
            vec![0xfe, 0xff, 0x00, 0x00]
        );
    }

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

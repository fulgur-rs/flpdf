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
        match find_name_tree_value(self.pdf, dests_root.clone(), &lookup)? {
            NameTreeLookup::Found(value) => return resolve_terminal_object(self.pdf, value),
            NameTreeLookup::Missing => {}
            NameTreeLookup::Structural(error) => {
                let entries = enumerate_name_tree_entries(self.pdf, dests_root.clone())?;
                if entries.is_empty() {
                    return Ok(Object::Null);
                }
                self.pdf.push_warning(format!(
                    "attempting to repair after error: {}",
                    error.diagnostic()
                ));
                let repaired_root = repair_name_tree(self.pdf, dests_root, entries)?;
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
    Structural(NameTreeStructuralError),
}

struct NameTreeStructuralError {
    node_ref: Option<ObjectRef>,
    message: &'static str,
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

enum NameTreeKidSelection {
    Found(Object),
    Missing,
    Structural(NameTreeStructuralError),
}

enum NameTreeKidOrdering {
    Order(Ordering),
    Missing,
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
    let mut seen = BTreeSet::new();
    loop {
        let (node, identity) = name_tree_node(pdf, cursor)?;
        let Some(mut node) = node else {
            return Ok(NameTreeLookup::Missing);
        };
        if let Some(identity) = identity {
            if !seen.insert(identity) {
                return Ok(NameTreeLookup::Missing);
            }
        }

        if let Some(Object::Array(names)) = node.remove("Names") {
            if !names.is_empty() {
                return Ok(match find_name_tree_leaf_value(names, lookup) {
                    Some(value) => NameTreeLookup::Found(value),
                    None => NameTreeLookup::Missing,
                });
            }
        }

        let Some(Object::Array(kids)) = node.remove("Kids") else {
            return Ok(NameTreeLookup::Missing);
        };
        match select_name_tree_kid(pdf, &kids, lookup)? {
            NameTreeKidSelection::Found(next) => cursor = next,
            NameTreeKidSelection::Missing => return Ok(NameTreeLookup::Missing),
            NameTreeKidSelection::Structural(error) => {
                return Ok(NameTreeLookup::Structural(error));
            }
        }
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

fn find_name_tree_leaf_value(names: Vec<Object>, lookup: &[u8]) -> Option<Object> {
    let pair_count = names.len() / 2;
    let mut low = 0;
    let mut high = pair_count;
    while low < high {
        let middle = low + (high - low) / 2;
        let Object::String(stored) = &names[2 * middle] else {
            return None;
        };
        match lookup.cmp(crate::json_inspect::qpdf_utf8_value(stored).as_slice()) {
            Ordering::Less => high = middle,
            Ordering::Greater => low = middle + 1,
            Ordering::Equal => return Some(names[2 * middle + 1].clone()),
        }
    }
    None
}

fn select_name_tree_kid<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    kids: &[Object],
    lookup: &[u8],
) -> Result<NameTreeKidSelection> {
    let mut low = 0;
    let mut high = kids.len();
    let mut previous = None;
    while low < high {
        let middle = low + (high - low) / 2;
        let ordering = match name_tree_kid_ordering(pdf, &kids[middle], lookup)? {
            NameTreeKidOrdering::Order(ordering) => ordering,
            NameTreeKidOrdering::Missing => return Ok(NameTreeKidSelection::Missing),
            NameTreeKidOrdering::Structural(error) => {
                return Ok(NameTreeKidSelection::Structural(error));
            }
        };
        match ordering {
            Ordering::Less => high = middle,
            Ordering::Equal => return Ok(NameTreeKidSelection::Found(kids[middle].clone())),
            Ordering::Greater => {
                previous = Some(middle);
                low = middle + 1;
            }
        }
    }
    Ok(match previous {
        Some(index) => NameTreeKidSelection::Found(kids[index].clone()),
        None => NameTreeKidSelection::Missing,
    })
}

/// Return the qpdf `withinLimits` comparison for `lookup` against one kid:
/// less when before the range, greater when after it, equal when within it.
fn name_tree_kid_ordering<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    kid: &Object,
    lookup: &[u8],
) -> Result<NameTreeKidOrdering> {
    let (node, _) = name_tree_node(pdf, kid.clone())?;
    let Some(node) = node else {
        return Ok(NameTreeKidOrdering::Missing);
    };
    let Some(Object::Array(limits)) = node.get("Limits") else {
        return Ok(NameTreeKidOrdering::Structural(NameTreeStructuralError {
            node_ref: kid.as_ref_id(),
            message: "node is missing /Limits",
        }));
    };
    let [Object::String(first), Object::String(last), ..] = limits.as_slice() else {
        return Ok(NameTreeKidOrdering::Structural(NameTreeStructuralError {
            node_ref: kid.as_ref_id(),
            message: "node is missing /Limits",
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
    let mut entries = BTreeMap::new();
    let mut stack = vec![root];
    let mut seen = BTreeSet::new();

    while let Some(cursor) = stack.pop() {
        let (node, identity) = name_tree_node(pdf, cursor)?;
        let Some(mut node) = node else {
            continue;
        };
        if let Some(identity) = identity {
            if !seen.insert(identity) {
                continue;
            }
        }

        if let Some(Object::Array(names)) = node.remove("Names") {
            if !names.is_empty() {
                for pair in names.chunks_exact(2) {
                    let Object::String(key) = &pair[0] else {
                        continue;
                    };
                    let key =
                        qpdf_new_unicode_utf8_value(&crate::json_inspect::qpdf_utf8_value(key));
                    entries.insert(key, pair[1].clone());
                }
                continue;
            }
        }

        if let Some(Object::Array(kids)) = node.remove("Kids") {
            stack.extend(kids.into_iter().rev());
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
    let (new_root_ref, nodes) = crate::name_number_tree::build_name_tree(entries, || {
        let object_ref = ObjectRef::new(next_object_number, 0);
        next_object_number += 1;
        object_ref
    });

    let mut root = None;
    for (node_ref, node) in nodes {
        if node_ref == new_root_ref {
            root = node.into_dict();
        } else {
            pdf.set_object(node_ref, node);
        }
    }
    root.ok_or_else(|| Error::Unsupported("name-tree rebuild produced no root".to_string()))
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
    use super::{qpdf_new_unicode_utf8_value, qpdf_unicode_string_bytes, NameTreeStructuralError};

    #[test]
    fn direct_name_tree_node_diagnostic_omits_an_object_number() {
        let error = NameTreeStructuralError {
            node_ref: None,
            message: "node is missing /Limits",
        };
        assert_eq!(
            error.diagnostic(),
            "Name/Number tree node: node is missing /Limits"
        );
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

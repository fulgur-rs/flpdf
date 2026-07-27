//! qpdf correspondence: QPDFOutlineDocumentHelper.cc and QPDFOutlineObjectHelper.cc responsibilities split with outline.rs.
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
use std::collections::BTreeSet;
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
            siblings_seen: BTreeSet<ObjectRef>,
        }

        let mut frames = Vec::new();
        let first = OutlineCursor::from_object(object_key(&tree[root].object, "First"));
        if first.is_some() {
            frames.push(Frame {
                owner: root,
                next: first,
                depth: 2,
                siblings_seen: BTreeSet::new(),
            });
        }

        while !frames.is_empty() {
            let next_cursor = frames.last_mut().and_then(|frame| frame.next.take());
            let Some(cursor) = next_cursor else {
                frames.pop();
                continue;
            };
            let (owner, child_depth) = {
                let frame = frames
                    .last_mut()
                    .expect("outline construction frame exists");
                if let Some(reference) = cursor.source_ref() {
                    if !frame.siblings_seen.insert(reference) {
                        frame.next = None;
                        continue;
                    }
                }
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
                        siblings_seen: BTreeSet::new(),
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
        let lookup = crate::json_inspect::qpdf_new_unicode_utf8_value(
            &crate::json_inspect::qpdf_utf8_value(bytes),
        );
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

        let original_root = dests_root.clone();
        let mut tree = crate::NameTree::new(dests_root, true);
        let found = tree.find_object(self.pdf, lookup.as_slice());
        if tree.root() != &original_root {
            if let Object::Dictionary(repaired_root) = tree.into_root() {
                write_back_direct_dests_root(self.pdf, repaired_root)?;
            }
        }

        match found? {
            Some(value) => resolve_terminal_object(self.pdf, value),
            None => Ok(Object::Null),
        }
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

fn write_back_direct_dests_root<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    repaired_root: Dictionary,
) -> Result<()> {
    let catalog_ref = pdf.root_ref().ok_or(Error::Missing("/Root"))?;
    let Object::Dictionary(mut catalog) = pdf.resolve(catalog_ref)? else {
        return Ok(()); // cov:ignore: resolve_name_tree_node_dest just resolved the catalog dictionary
    };
    let Some(names_value) = catalog.get("Names").cloned() else {
        return Ok(()); // cov:ignore: resolve_name_tree_node_dest just established catalog /Names
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
                return Ok(()); // cov:ignore: caller established /Names reference terminal as a dictionary
            };
            let Some(terminal_ref) = terminal_ref else {
                return Ok(()); // cov:ignore: caller established /Names reference has an indirect terminal
            };
            names.insert("Dests", Object::Dictionary(repaired_root));
            pdf.set_object(terminal_ref, Object::Dictionary(names));
        }
        _ => {} // cov:ignore: caller established catalog /Names as a dictionary or reference
    }
    Ok(())
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
        Object::Operator(_) => "operator",
        Object::InlineImage(_) => "inline-image",
        Object::Array(_) => "array",
        Object::Dictionary(_) => "dictionary",
        Object::Stream(_) => "stream",
        Object::Reference(_) => "reference",
    }
}

#[cfg(test)]
mod tests {
    use super::qpdf_object_type_name;
    use crate::Object;

    #[test]
    fn qpdf_object_type_name_labels_content_only_values() {
        assert_eq!(
            qpdf_object_type_name(&Object::Operator(b"q".to_vec())),
            "operator"
        );
        assert_eq!(
            qpdf_object_type_name(&Object::InlineImage(b"data".to_vec())),
            "inline-image"
        );
    }
}

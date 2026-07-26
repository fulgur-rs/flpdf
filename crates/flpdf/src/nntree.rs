//! Mirrors qpdf 11.9.0 `libqpdf/NNTree.cc`.
//!
//! Public wrappers corresponding to `QPDFNameTreeObjectHelper` and
//! `QPDFNumberTreeObjectHelper` are added in the next stacked layer.

use crate::json_inspect::{qpdf_new_unicode_utf8_value, qpdf_unicode_string_bytes};
use crate::ref_chain::resolve_ref_chain;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::fmt::Debug;
use std::io::{Read, Seek};
use std::marker::PhantomData;

pub(crate) const DEFAULT_SPLIT_THRESHOLD: usize = 32;

pub(crate) trait TreeKey {
    type Key: Clone + Debug + Eq + Ord;
    const ITEMS_KEY: &'static str;

    fn from_object(object: &Object) -> Option<Self::Key>;
    fn to_object(key: &Self::Key) -> Object;

    fn compare(left: &Self::Key, right: &Self::Key) -> Ordering {
        left.cmp(right)
    }
}

pub(crate) enum NameKey {}

impl TreeKey for NameKey {
    type Key = Vec<u8>;
    const ITEMS_KEY: &'static str = "Names";

    fn from_object(object: &Object) -> Option<Self::Key> {
        match object {
            Object::String(value) => Some(crate::json_inspect::qpdf_utf8_value(value)),
            _ => None,
        }
    }

    fn to_object(key: &Self::Key) -> Object {
        let normalized = qpdf_new_unicode_utf8_value(key);
        Object::String(qpdf_unicode_string_bytes(&normalized))
    }
}

pub(crate) enum NumberKey {}

impl TreeKey for NumberKey {
    type Key = i64;
    const ITEMS_KEY: &'static str = "Nums";

    fn from_object(object: &Object) -> Option<Self::Key> {
        match object {
            Object::Integer(value) => Some(*value),
            _ => None,
        }
    }

    fn to_object(key: &Self::Key) -> Object {
        Object::Integer(*key)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum NodeAnchor {
    Root,
    Indirect(ObjectRef),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct NodeHandle {
    anchor: NodeAnchor,
    direct_kids: Vec<usize>,
}

impl NodeHandle {
    fn root() -> Self {
        Self {
            anchor: NodeAnchor::Root,
            direct_kids: Vec::new(),
        }
    }

    fn indirect(object_ref: ObjectRef) -> Self {
        Self {
            anchor: NodeAnchor::Indirect(object_ref),
            direct_kids: Vec::new(),
        }
    }

    fn direct_kid(&self, kid_index: usize) -> Self {
        let mut direct_kids = self.direct_kids.clone();
        direct_kids.push(kid_index);
        Self {
            anchor: self.anchor.clone(),
            direct_kids,
        }
    }

    fn diagnostic_ref(&self) -> Option<ObjectRef> {
        if !self.direct_kids.is_empty() {
            return None;
        }
        match self.anchor {
            NodeAnchor::Indirect(object_ref) => Some(object_ref),
            NodeAnchor::Root => None,
        }
    }
}

struct ResolvedArray {
    values: Vec<Object>,
    source: Object,
    terminal_ref: Option<ObjectRef>,
}

impl ResolvedArray {
    fn into_object<R: Read + Seek>(self, pdf: &mut Pdf<R>) -> Object {
        if let Some(object_ref) = self.terminal_ref {
            pdf.set_object(object_ref, Object::Array(self.values));
            self.source
        } else {
            Object::Array(self.values)
        }
    }

    fn store<R: Read + Seek>(self, pdf: &mut Pdf<R>, dictionary: &mut Dictionary, key: &str) {
        let value = self.into_object(pdf);
        dictionary.insert(key, value);
    }
}

fn resolved_array<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: Option<&Object>,
) -> Result<Option<ResolvedArray>> {
    let Some(source) = value else {
        return Ok(None);
    };
    let (resolved, terminal_ref) = resolve_ref_chain(pdf, source)?;
    let Object::Array(values) = resolved else {
        return Ok(None);
    };
    Ok(Some(ResolvedArray {
        values,
        source: source.clone(),
        terminal_ref,
    }))
}

fn resolved_key<K: TreeKey, R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: &Object,
) -> Result<Option<K::Key>> {
    let (resolved, _) = resolve_ref_chain(pdf, value)?;
    Ok(K::from_object(&resolved))
}

#[derive(Clone, Debug)]
struct PathElement {
    node: NodeHandle,
    kid_number: usize,
}

pub(crate) struct NNTreeCursor<K: TreeKey> {
    path: Vec<PathElement>,
    leaf: Option<NodeHandle>,
    item_number: Option<usize>,
    current: Option<(K::Key, Object)>,
    marker: PhantomData<K>,
}

impl<K: TreeKey> NNTreeCursor<K> {
    fn empty() -> Self {
        Self {
            path: Vec::new(),
            leaf: None,
            item_number: None,
            current: None,
            marker: PhantomData,
        }
    }

    /// Whether traversal has selected an array slot.
    ///
    /// Malformed keys may leave a cursor positioned while [`Self::current`]
    /// is `None`; callers must not treat this as a usable key/value pair.
    pub(crate) fn positioned(&self) -> bool {
        self.item_number.is_some()
    }

    pub(crate) fn current(&self) -> Option<(&K::Key, &Object)> {
        self.current.as_ref().map(|(key, value)| (key, value))
    }

    fn cloned_current(&self) -> Option<(K::Key, Object)> {
        self.current.clone()
    }

    fn clear_position(&mut self) {
        self.leaf = None;
        self.item_number = None;
        self.current = None;
    }
}

pub(crate) struct NNTree<K: TreeKey> {
    root: Object,
    auto_repair: bool,
    split_threshold: usize,
    marker: PhantomData<K>,
}

impl<K: TreeKey> NNTree<K> {
    pub(crate) fn new(root: Object, auto_repair: bool) -> Self {
        Self {
            root,
            auto_repair,
            split_threshold: DEFAULT_SPLIT_THRESHOLD,
            marker: PhantomData,
        }
    }

    pub(crate) fn root(&self) -> &Object {
        &self.root
    }

    pub(crate) fn into_root(self) -> Object {
        self.root
    }

    pub(crate) fn begin<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<NNTreeCursor<K>> {
        let mut cursor = NNTreeCursor::empty();
        let root = self.root_handle(pdf)?;
        self.descend(pdf, &mut cursor, root, true, true)?;
        Ok(cursor)
    }

    pub(crate) fn end(&self) -> NNTreeCursor<K> {
        NNTreeCursor::empty()
    }

    pub(crate) fn last<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<NNTreeCursor<K>> {
        let mut cursor = NNTreeCursor::empty();
        let root = self.root_handle(pdf)?;
        self.descend(pdf, &mut cursor, root, false, true)?;
        Ok(cursor)
    }

    pub(crate) fn next<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
    ) -> Result<()> {
        self.increment(pdf, cursor, false)
    }

    pub(crate) fn previous<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
    ) -> Result<()> {
        self.increment(pdf, cursor, true)
    }

    pub(crate) fn find<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: &K::Key,
        return_previous_if_missing: bool,
    ) -> Result<NNTreeCursor<K>> {
        match self.find_internal(pdf, key, return_previous_if_missing) {
            Ok(cursor) => Ok(cursor),
            Err(error) if self.auto_repair => {
                let root = self.root_handle(pdf)?;
                self.warn(
                    pdf,
                    &root,
                    format!("attempting to repair after error: {error}"),
                );
                self.repair(pdf)?;
                self.find_internal(pdf, key, return_previous_if_missing)
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn set_split_threshold(&mut self, threshold: usize) {
        self.split_threshold = threshold;
    }

    pub(crate) fn insert<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: K::Key,
        value: Object,
    ) -> Result<NNTreeCursor<K>> {
        let mut allocator = ObjectAllocator::default();
        self.insert_with_allocator(pdf, &mut allocator, key, value)
    }

    fn insert_with_allocator<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        allocator: &mut ObjectAllocator,
        key: K::Key,
        value: Object,
    ) -> Result<NNTreeCursor<K>> {
        let mut cursor = self.find(pdf, &key, true)?;
        if !cursor.positioned() {
            return self.insert_first(pdf, allocator, key, value);
        }

        let is_exact = cursor
            .current()
            .is_some_and(|(current_key, _)| K::compare(&key, current_key) == Ordering::Equal);
        if is_exact {
            let leaf = cursor.leaf.clone().expect("valid cursor has a leaf");
            let item_number = cursor.item_number.expect("valid cursor has an item");
            let mut dictionary = self.load_node(pdf, &leaf)?;
            // cov:ignore-start: find just returned this leaf with an items array and no callback can mutate it here
            let Some(mut items) = resolved_array(pdf, dictionary.get(K::ITEMS_KEY))? else {
                return Err(structural_error(
                    leaf.diagnostic_ref(),
                    "node contains no items array",
                ));
            };
            // cov:ignore-end
            items.values[item_number + 1] = value;
            items.store(pdf, &mut dictionary, K::ITEMS_KEY);
            self.store_node(pdf, &leaf, dictionary)?;
            self.update_current(pdf, &mut cursor, false)?;
        } else {
            self.insert_after_with_allocator(pdf, allocator, &mut cursor, key, value)?;
        }
        Ok(cursor)
    }

    pub(crate) fn insert_after<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
        key: K::Key,
        value: Object,
    ) -> Result<()> {
        let mut allocator = ObjectAllocator::default();
        self.insert_after_with_allocator(pdf, &mut allocator, cursor, key, value)
    }

    fn insert_after_with_allocator<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        allocator: &mut ObjectAllocator,
        cursor: &mut NNTreeCursor<K>,
        key: K::Key,
        value: Object,
    ) -> Result<()> {
        if !cursor.positioned() {
            *cursor = self.insert_first(pdf, allocator, key, value)?;
            return Ok(());
        }

        let leaf = cursor.leaf.clone().expect("valid cursor has a leaf");
        let item_number = cursor.item_number.expect("valid cursor has an item");
        let mut dictionary = self.load_node(pdf, &leaf)?;
        let Some(mut items) = resolved_array(pdf, dictionary.get(K::ITEMS_KEY))? else {
            return Err(structural_error(
                leaf.diagnostic_ref(),
                "node contains no items array",
            ));
        };
        if items.values.len() < item_number + 2 {
            return Err(structural_error(
                leaf.diagnostic_ref(),
                "insert: items array is too short",
            ));
        }
        items.values.insert(item_number + 2, K::to_object(&key));
        items.values.insert(item_number + 3, value);
        items.store(pdf, &mut dictionary, K::ITEMS_KEY);
        self.store_node(pdf, &leaf, dictionary)?;
        self.reset_limits(pdf, cursor, leaf, cursor.path.len().checked_sub(1))?;
        cursor.item_number = Some(item_number + 2);
        self.update_current(pdf, cursor, false)?;
        let leaf = cursor.leaf.clone().expect("inserted item has a leaf");
        self.split_node_with_allocator(
            pdf,
            allocator,
            cursor,
            leaf,
            cursor.path.len().checked_sub(1),
        )
    }

    pub(crate) fn remove<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: &K::Key,
    ) -> Result<Option<Object>> {
        let mut cursor = self.find(pdf, key, false)?;
        let Some((_, value)) = cursor.cloned_current() else {
            return Ok(None);
        };
        self.remove_at(pdf, &mut cursor)?;
        Ok(Some(value))
    }

    pub(crate) fn remove_at<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
    ) -> Result<Option<Object>> {
        let Some((_, removed_value)) = cursor.cloned_current() else {
            return Ok(None);
        };
        let leaf = cursor.leaf.clone().expect("valid cursor has a leaf");
        let item_number = cursor.item_number.expect("valid cursor has an item");
        let mut dictionary = self.load_node(pdf, &leaf)?;
        let Some(mut items) = resolved_array(pdf, dictionary.get(K::ITEMS_KEY))? else {
            return Err(structural_error(
                leaf.diagnostic_ref(),
                "node contains no items array",
            ));
        };
        if item_number + 2 > items.values.len() {
            return Err(structural_error(
                leaf.diagnostic_ref(),
                "found short items array while removing an item",
            ));
        }
        items.values.drain(item_number..item_number + 2);
        let remaining = items.values.len();
        items.store(pdf, &mut dictionary, K::ITEMS_KEY);
        self.store_node(pdf, &leaf, dictionary)?;

        if remaining > 0 {
            if item_number == 0 || item_number == remaining {
                self.reset_limits(pdf, cursor, leaf.clone(), cursor.path.len().checked_sub(1))?;
            }
            if item_number == remaining {
                cursor.item_number = item_number.checked_sub(2);
                self.update_current(pdf, cursor, false)?;
                self.next(pdf, cursor)?;
            } else {
                self.update_current(pdf, cursor, false)?;
            }
            return Ok(Some(removed_value));
        }

        if cursor.path.is_empty() {
            cursor.item_number = None;
            cursor.current = None;
            self.reset_limits(pdf, cursor, leaf, None)?;
            return Ok(Some(removed_value));
        }

        self.remove_empty_leaf(pdf, cursor)?;
        Ok(Some(removed_value))
    }

    fn insert_first<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        allocator: &mut ObjectAllocator,
        key: K::Key,
        value: Object,
    ) -> Result<NNTreeCursor<K>> {
        let mut cursor = self.begin(pdf)?;
        let leaf = cursor
            .leaf
            .clone()
            .ok_or_else(|| structural_error(None, "unable to find a valid items node"))?;
        let mut dictionary = self.load_node(pdf, &leaf)?;
        // cov:ignore-start: begin returns an empty cursor leaf only after observing its items array
        let Some(mut items) = resolved_array(pdf, dictionary.get(K::ITEMS_KEY))? else {
            return Err(structural_error(
                self.root_handle(pdf)?.diagnostic_ref(),
                "unable to find a valid items node",
            ));
        };
        // cov:ignore-end
        items.values.insert(0, K::to_object(&key));
        items.values.insert(1, value);
        items.store(pdf, &mut dictionary, K::ITEMS_KEY);
        self.store_node(pdf, &leaf, dictionary)?;
        cursor.item_number = Some(0);
        self.update_current(pdf, &mut cursor, false)?;
        let parent_index = cursor.path.len().checked_sub(1);
        self.reset_limits(pdf, &cursor, leaf.clone(), parent_index)?;
        self.split_node_with_allocator(pdf, allocator, &mut cursor, leaf, parent_index)?;
        Ok(cursor)
    }

    fn repair<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<()> {
        let mut replacement_root = Dictionary::new();
        replacement_root.insert(K::ITEMS_KEY, Object::Array(Vec::new()));
        let mut replacement = NNTree::<K>::new(Object::Dictionary(replacement_root), false);
        replacement.set_split_threshold(self.split_threshold);

        let mut allocator = ObjectAllocator::default();
        let mut cursor = self.begin(pdf)?;
        while cursor.positioned() {
            if let Some((key, value)) = cursor.cloned_current() {
                replacement.insert_with_allocator(pdf, &mut allocator, key, value)?;
            }
            self.next(pdf, &mut cursor)?;
        }

        self.replace_root_contents(pdf, replacement.into_root())
    }

    fn replace_root_contents<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        replacement: Object,
    ) -> Result<()> {
        let (replacement, _) = resolve_ref_chain(pdf, &replacement)?;
        let Object::Dictionary(replacement) = replacement else {
            return Err(structural_error(
                None,
                "replacement root is not a dictionary",
            ));
        };
        let root = self.root_handle(pdf)?;
        let mut current = self.load_node(pdf, &root)?;
        current.remove("Kids");
        current.remove(K::ITEMS_KEY);
        if let Some(kids) = replacement.get("Kids") {
            current.insert("Kids", kids.clone());
        }
        if let Some(items) = replacement.get(K::ITEMS_KEY) {
            current.insert(K::ITEMS_KEY, items.clone());
        }
        self.store_node(pdf, &root, current)
    }

    fn split_node<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
        node: NodeHandle,
        parent_index: Option<usize>,
    ) -> Result<()> {
        let mut allocator = ObjectAllocator::default();
        self.split_node_with_allocator(pdf, &mut allocator, cursor, node, parent_index)
    }

    fn split_node_with_allocator<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        allocator: &mut ObjectAllocator,
        cursor: &mut NNTreeCursor<K>,
        mut node: NodeHandle,
        mut parent_index: Option<usize>,
    ) -> Result<()> {
        let dictionary = self.load_node(pdf, &node)?;
        let kids = resolved_array(pdf, dictionary.get("Kids"))?;
        let items = resolved_array(pdf, dictionary.get(K::ITEMS_KEY))?;
        let (array_key, array, threshold, is_leaf) = if let Some(kids) = kids {
            if kids.values.is_empty() {
                return Ok(());
            }
            ("Kids", kids, self.split_threshold, false)
        } else if let Some(items) = items {
            if items.values.is_empty() {
                return Ok(());
            }
            (K::ITEMS_KEY, items, 2 * self.split_threshold, true)
        } else {
            return Err(structural_error(
                node.diagnostic_ref(),
                "split called on invalid node",
            ));
        };
        if array.values.len() <= threshold {
            return Ok(());
        }

        let is_root = parent_index.is_none();
        if is_root {
            let mut first_dictionary = Dictionary::new();
            first_dictionary.insert(array_key, array.into_object(pdf));
            let first_ref = make_indirect(pdf, allocator, Object::Dictionary(first_dictionary))?;
            let first_handle = NodeHandle::indirect(first_ref);

            let mut root = self.load_node(pdf, &node)?;
            root.remove("Limits");
            root.remove(K::ITEMS_KEY);
            root.insert("Kids", Object::Array(vec![Object::Reference(first_ref)]));
            self.store_node(pdf, &node, root)?;

            if is_leaf {
                cursor.leaf = Some(first_handle.clone());
            } else if let Some(first_path) = cursor.path.first_mut() {
                first_path.node = first_handle.clone();
            }
            cursor.path.insert(
                0,
                PathElement {
                    node: node.clone(),
                    kid_number: 0,
                },
            );
            parent_index = Some(0);
            node = first_handle;
        }

        let parent_index = parent_index.expect("root was normalized above");
        let mut first_dictionary = self.load_node(pdf, &node)?;
        // cov:ignore-start: array_key was selected from this same node before root normalization
        let Some(mut first_half) = resolved_array(pdf, first_dictionary.get(array_key))? else {
            return Err(structural_error(
                node.diagnostic_ref(),
                format!("/{array_key} is not an array"),
            ));
        };
        // cov:ignore-end
        let start_index = (first_half.values.len() / 2) & !1;
        let second_half = first_half.values.split_off(start_index);
        first_half.store(pdf, &mut first_dictionary, array_key);
        self.store_node(pdf, &node, first_dictionary)?;
        self.reset_limits(pdf, cursor, node.clone(), Some(parent_index))?;

        let mut second_dictionary = Dictionary::new();
        second_dictionary.insert(array_key, Object::Array(second_half));
        let second_ref = make_indirect(pdf, allocator, Object::Dictionary(second_dictionary))?;
        let second_handle = NodeHandle::indirect(second_ref);
        self.reset_limits(pdf, cursor, second_handle.clone(), Some(parent_index))?;

        let parent_handle = cursor.path[parent_index].node.clone();
        let mut parent = self.load_node(pdf, &parent_handle)?;
        // cov:ignore-start: split cursor path was built from this parent Kids array
        let Some(mut parent_kids) = resolved_array(pdf, parent.get("Kids"))? else {
            return Err(structural_error(
                parent_handle.diagnostic_ref(),
                "node is missing /Kids",
            ));
        };
        // cov:ignore-end
        let first_kid_index = cursor.path[parent_index].kid_number;
        parent_kids
            .values
            .insert(first_kid_index + 1, Object::Reference(second_ref));
        parent_kids.store(pdf, &mut parent, "Kids");
        self.store_node(pdf, &parent_handle, parent)?;

        let old_index = if is_leaf {
            cursor.item_number.expect("split cursor points to an item")
        } else {
            cursor.path[parent_index + 1].kid_number
        };
        if old_index >= start_index {
            cursor.path[parent_index].kid_number += 1;
            if is_leaf {
                cursor.leaf = Some(second_handle);
                cursor.item_number = Some(old_index - start_index);
                self.update_current(pdf, cursor, false)?;
            } else {
                cursor.path[parent_index + 1].node = second_handle;
                cursor.path[parent_index + 1].kid_number -= start_index;
            }
        } // cov:ignore: LLVM assigns an uncovered region to this delimiter although both cursor-adjustment arms above execute

        if !is_root {
            let parent_handle = cursor.path[parent_index].node.clone();
            let grandparent_index = parent_index.checked_sub(1);
            self.reset_limits(pdf, cursor, parent_handle.clone(), grandparent_index)?;
            self.split_node_with_allocator(
                pdf,
                allocator,
                cursor,
                parent_handle,
                grandparent_index,
            )?; // cov:ignore: LLVM assigns the covered recursive call terminator a zero-count region
        }
        Ok(())
    }

    fn reset_limits<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &NNTreeCursor<K>,
        mut node: NodeHandle,
        mut parent_index: Option<usize>,
    ) -> Result<()> {
        loop {
            let mut dictionary = self.load_node(pdf, &node)?;
            let Some(index) = parent_index else {
                dictionary.remove("Limits");
                self.store_node(pdf, &node, dictionary)?;
                return Ok(());
            };

            let new_limits = self.edge_limits(pdf, &dictionary)?;
            let changed = match new_limits {
                Some((first, last)) => {
                    let old_limits = resolved_array(pdf, dictionary.get("Limits"))?;
                    let unchanged = if let Some(old_limits) = old_limits {
                        if old_limits.values.len() == 2 {
                            let old_first = resolved_key::<K, _>(pdf, &old_limits.values[0])?;
                            let old_last = resolved_key::<K, _>(pdf, &old_limits.values[1])?;
                            let new_first = resolved_key::<K, _>(pdf, &first)?;
                            let new_last = resolved_key::<K, _>(pdf, &last)?;
                            matches!(
                                (old_first, old_last, new_first, new_last),
                                (
                                    Some(old_first),
                                    Some(old_last),
                                    Some(new_first),
                                    Some(new_last)
                                ) if K::compare(&old_first, &new_first) == Ordering::Equal
                                    && K::compare(&old_last, &new_last) == Ordering::Equal
                            )
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if unchanged {
                        false
                    } else {
                        dictionary.insert("Limits", Object::Array(vec![first, last]));
                        self.store_node(pdf, &node, dictionary)?;
                        true
                    }
                }
                None => {
                    self.warn(pdf, &node, "unable to determine limits");
                    true
                }
            };

            if !changed || index == 0 {
                return Ok(());
            }
            node = cursor.path[index].node.clone();
            parent_index = index.checked_sub(1);
        }
    }

    fn edge_limits<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        dictionary: &Dictionary,
    ) -> Result<Option<(Object, Object)>> {
        if let Some(items) = resolved_array(pdf, dictionary.get(K::ITEMS_KEY))? {
            if items.values.len() >= 2 {
                return Ok(Some((
                    items.values[0].clone(),
                    items.values[(items.values.len() - 1) & !1].clone(),
                )));
            }
        }
        if let Some(kids) = resolved_array(pdf, dictionary.get("Kids"))? {
            if let (Some(first_kid), Some(last_kid)) = (kids.values.first(), kids.values.last()) {
                let (Object::Dictionary(first), _) = resolve_ref_chain(pdf, first_kid)? else {
                    return Ok(None);
                };
                let (Object::Dictionary(last), _) = resolve_ref_chain(pdf, last_kid)? else {
                    return Ok(None);
                };
                let (Some(first_limits), Some(last_limits)) = (
                    resolved_array(pdf, first.get("Limits"))?,
                    resolved_array(pdf, last.get("Limits"))?,
                ) else {
                    return Ok(None);
                };
                if first_limits.values.len() >= 2 && last_limits.values.len() >= 2 {
                    return Ok(Some((
                        first_limits.values[0].clone(),
                        last_limits.values[1].clone(),
                    )));
                } // cov:ignore: LLVM assigns the covered successful edge-limit return region to this delimiter
            } // cov:ignore: LLVM assigns the covered non-empty Kids branch region to this delimiter
        }
        Ok(None)
    }

    fn remove_empty_leaf<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
    ) -> Result<()> {
        loop {
            let path_index = cursor.path.len() - 1;
            let parent_handle = cursor.path[path_index].node.clone();
            let removed_kid = cursor.path[path_index].kid_number;
            let mut parent = self.load_node(pdf, &parent_handle)?;
            let Some(mut kids) = resolved_array(pdf, parent.get("Kids"))? else {
                return Err(structural_error(
                    parent_handle.diagnostic_ref(),
                    "node is missing /Kids",
                ));
            };
            kids.values.remove(removed_kid);
            let remaining_kids = kids.values.len();
            let remaining_kid_values = kids.values.clone();
            kids.store(pdf, &mut parent, "Kids");
            self.store_node(pdf, &parent_handle, parent)?;

            if remaining_kids > 0 {
                if removed_kid == 0 || removed_kid == remaining_kids {
                    self.reset_limits(
                        pdf,
                        cursor,
                        parent_handle.clone(),
                        path_index.checked_sub(1),
                    )?; // cov:ignore: LLVM maps this covered multi-line call terminator to a zero-count region
                } // cov:ignore: LLVM maps this covered limit-reset branch delimiter to a zero-count region
                cursor.clear_position();
                if removed_kid == remaining_kids {
                    cursor.path[path_index].kid_number -= 1;
                    let previous = remaining_kid_values.last().expect("non-empty").clone();
                    let child =
                        self.prepare_kid(pdf, &parent_handle, remaining_kids - 1, previous)?;
                    self.descend(pdf, cursor, child, false, true)?;
                    if cursor.positioned() {
                        self.next(pdf, cursor)?;
                    } // cov:ignore: LLVM maps this covered conditional delimiter to a zero-count region
                } else {
                    let next = remaining_kid_values[removed_kid].clone();
                    let child = self.prepare_kid(pdf, &parent_handle, removed_kid, next)?;
                    self.descend(pdf, cursor, child, true, true)?;
                }
                return Ok(());
            }

            if path_index == 0 {
                let mut root = self.load_node(pdf, &parent_handle)?;
                root.remove("Kids");
                root.insert(K::ITEMS_KEY, Object::Array(Vec::new()));
                self.store_node(pdf, &parent_handle, root)?;
                cursor.path.clear();
                cursor.clear_position();
                return Ok(());
            }
            cursor.path.pop();
        }
    }

    fn find_internal<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: &K::Key,
        return_previous_if_missing: bool,
    ) -> Result<NNTreeCursor<K>> {
        let first = self.begin(pdf)?;
        if !first.positioned() {
            return Ok(self.end());
        }
        if let Some((first_key, _)) = first.current() {
            if K::compare(key, first_key) == Ordering::Less {
                return Ok(self.end());
            }
        }
        // qpdf 11.9.0 initializes its `last_item` check with end(), not
        // last(), so after-maximum keys intentionally use the general search.

        let root = self.root_handle(pdf)?;
        let root_diagnostic_ref = root.diagnostic_ref();
        let mut node = root;
        let mut seen = HashSet::new();
        let mut cursor = NNTreeCursor::empty();

        loop {
            if !seen.insert(node.clone()) {
                return Err(structural_error(
                    node.diagnostic_ref(),
                    "loop detected in find",
                ));
            }

            let dictionary = self
                .load_node(pdf, &node)
                .map_err(|_| structural_error(node.diagnostic_ref(), "bad node during find"))?;
            let items = resolved_array(pdf, dictionary.get(K::ITEMS_KEY))?;
            let kids = resolved_array(pdf, dictionary.get("Kids"))?;

            if let Some(items) = items.as_ref().filter(|items| !items.values.is_empty()) {
                let index = binary_search(
                    items.values.len() / 2,
                    return_previous_if_missing,
                    |index| {
                        let item_number = 2 * index;
                        // cov:ignore-start: binary_search only supplies indices below items length divided by two
                        let Some(item) = items.values.get(item_number) else {
                            return Err(structural_error(
                                root_diagnostic_ref,
                                format!("item at index {item_number} is not the right type"),
                            ));
                        };
                        // cov:ignore-end
                        let Some(item_key) = resolved_key::<K, _>(pdf, item)? else {
                            return Err(structural_error(
                                root_diagnostic_ref,
                                format!("item at index {item_number} is not the right type"),
                            ));
                        };
                        Ok(K::compare(key, &item_key))
                    },
                )?;
                if let Some(index) = index {
                    cursor.leaf = Some(node);
                    cursor.item_number = Some(2 * index);
                    self.update_current(pdf, &mut cursor, false)?;
                }
                return Ok(cursor);
            }

            if let Some(kids) = kids.filter(|kids| !kids.values.is_empty()) {
                let index = binary_search(kids.values.len(), true, |index| {
                    let kid = kids
                        .values
                        .get(index)
                        .expect("binary-search index is in range");
                    let (resolved, terminal_ref) = resolve_ref_chain(pdf, kid)?;
                    let Object::Dictionary(kid_dictionary) = resolved else {
                        return Err(structural_error(
                            root_diagnostic_ref,
                            format!("invalid kid at index {index}"),
                        ));
                    };
                    self.within_limits(pdf, key, &kid_dictionary, terminal_ref)
                })?;
                let index = index.ok_or_else(|| {
                    structural_error(
                        node.diagnostic_ref(),
                        "unexpected -1 from binary search of kids; limits may by wrong",
                    )
                })?;
                let kid_object = kids.values[index].clone();
                cursor.path.push(PathElement {
                    node: node.clone(),
                    kid_number: index,
                });
                node = self.handle_for_kid(pdf, &node, index, &kid_object)?;
                continue;
            }

            return Err(structural_error(
                node.diagnostic_ref(),
                "bad node during find",
            ));
        }
    }

    fn within_limits<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        key: &K::Key,
        dictionary: &Dictionary,
        object_ref: Option<ObjectRef>,
    ) -> Result<Ordering> {
        let Some(limits) = resolved_array(pdf, dictionary.get("Limits"))? else {
            return Err(structural_error(object_ref, "node is missing /Limits"));
        };
        let (Some(first), Some(last)) = (
            limits
                .values
                .first()
                .map(|value| resolved_key::<K, _>(pdf, value))
                .transpose()?
                .flatten(),
            limits
                .values
                .get(1)
                .map(|value| resolved_key::<K, _>(pdf, value))
                .transpose()?
                .flatten(),
        ) else {
            return Err(structural_error(object_ref, "node is missing /Limits"));
        };
        if K::compare(key, &first) == Ordering::Less {
            Ok(Ordering::Less)
        } else if K::compare(key, &last) == Ordering::Greater {
            Ok(Ordering::Greater)
        } else {
            Ok(Ordering::Equal)
        }
    }

    fn handle_for_kid<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        parent: &NodeHandle,
        kid_number: usize,
        kid: &Object,
    ) -> Result<NodeHandle> {
        if matches!(kid, Object::Reference(_)) {
            let (_, terminal_ref) = resolve_ref_chain(pdf, kid)?;
            terminal_ref
                .map(NodeHandle::indirect)
                .ok_or_else(|| structural_error(parent.diagnostic_ref(), "invalid kid"))
        } else {
            Ok(parent.direct_kid(kid_number))
        }
    }

    fn root_handle<R: Read + Seek>(&self, pdf: &mut Pdf<R>) -> Result<NodeHandle> {
        let (_, terminal_ref) = resolve_ref_chain(pdf, &self.root)?;
        Ok(terminal_ref.map_or_else(NodeHandle::root, NodeHandle::indirect))
    }

    fn descend<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
        start: NodeHandle,
        first: bool,
        allow_empty: bool,
    ) -> Result<bool> {
        let original_path = cursor.path.clone();
        let original_leaf = cursor.leaf.clone();
        let original_item_number = cursor.item_number;
        let original_current = cursor.current.clone();
        let mut seen: HashSet<NodeHandle> = cursor
            .path
            .iter()
            .map(|element| element.node.clone())
            .collect();
        let mut node = start;

        loop {
            if !seen.insert(node.clone()) {
                self.warn(
                    pdf,
                    &node,
                    "loop detected while traversing name/number tree",
                );
                break;
            }

            let dictionary = match self.load_node(pdf, &node) {
                Ok(dictionary) => dictionary,
                Err(_) => {
                    self.warn(
                        pdf,
                        &node,
                        "non-dictionary node while traversing name/number tree",
                    );
                    break;
                }
            };
            let items = resolved_array(pdf, dictionary.get(K::ITEMS_KEY))?;
            let kids = resolved_array(pdf, dictionary.get("Kids"))?;

            if let Some(items) = items.as_ref().filter(|items| !items.values.is_empty()) {
                let item_number = if first {
                    0
                } else {
                    // qpdf 11.9.0 NNTreeIterator::deepen uses nitems - 2
                    // verbatim, including value-slot selection in odd arrays.
                    items.values.len().saturating_sub(2)
                };
                cursor.leaf = Some(node);
                cursor.item_number = Some(item_number);
                self.update_current(pdf, cursor, true)?;
                return Ok(true);
            }

            if let Some(kids) = kids.filter(|kids| !kids.values.is_empty()) {
                let kid_number = if first { 0 } else { kids.values.len() - 1 };
                let kid_object = kids.values[kid_number].clone();
                cursor.path.push(PathElement {
                    node: node.clone(),
                    kid_number,
                });
                node = self.prepare_kid(pdf, &node, kid_number, kid_object)?;
                continue;
            }

            if allow_empty && items.is_some() {
                cursor.leaf = Some(node);
                cursor.item_number = None;
                cursor.current = None;
                return Ok(true);
            }

            self.warn(
                pdf,
                &node,
                format!(
                    "name/number tree node has neither non-empty /{} nor /Kids",
                    K::ITEMS_KEY
                ),
            );
            break;
        }

        cursor.path = original_path;
        cursor.leaf = original_leaf;
        cursor.item_number = original_item_number;
        cursor.current = original_current;
        Ok(false)
    }

    fn increment<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
        backward: bool,
    ) -> Result<()> {
        if cursor.item_number.is_none() {
            cursor.path.clear();
            cursor.clear_position();
            let root = self.root_handle(pdf)?;
            self.descend(pdf, cursor, root, !backward, true)?;
            return Ok(());
        }

        loop {
            let leaf = cursor.leaf.clone().expect("valid cursor has a leaf");
            let dictionary = self.load_node(pdf, &leaf)?;
            let Some(items) = resolved_array(pdf, dictionary.get(K::ITEMS_KEY))? else {
                cursor.clear_position();
                return Ok(());
            };
            let item_number = cursor.item_number.expect("checked above");
            let candidate = if backward {
                item_number.checked_sub(2)
            } else {
                item_number
                    .checked_add(2)
                    .filter(|index| *index < items.values.len())
            };

            if let Some(candidate) = candidate {
                cursor.item_number = Some(candidate);
                self.update_current(pdf, cursor, true)?;
                if cursor.current.is_some() {
                    return Ok(());
                }
                continue;
            }

            cursor.clear_position();
            let mut descended = false;
            while let Some(last_index) = cursor.path.len().checked_sub(1) {
                let parent = cursor.path[last_index].node.clone();
                let dictionary = self.load_node(pdf, &parent)?;
                let Some(kids) = resolved_array(pdf, dictionary.get("Kids"))? else {
                    cursor.path.pop();
                    continue;
                };
                let mut kid_number = cursor.path[last_index].kid_number;
                loop {
                    let next_index = if backward {
                        kid_number.checked_sub(1)
                    } else {
                        kid_number
                            .checked_add(1)
                            .filter(|index| *index < kids.values.len())
                    };
                    let Some(next_index) = next_index else {
                        break;
                    };
                    kid_number = next_index;
                    let kid_object = kids.values[kid_number].clone();
                    if !self.kid_has_tree_shape(pdf, &kid_object)? {
                        self.warn(
                            pdf,
                            &parent,
                            format!("skipping over invalid kid at index {kid_number}"),
                        );
                        continue;
                    }
                    cursor.path[last_index].kid_number = kid_number;
                    let kid = self.prepare_kid(pdf, &parent, kid_number, kid_object)?;
                    if self.descend(pdf, cursor, kid, !backward, false)? {
                        descended = true;
                    }
                    break;
                }
                if descended {
                    break;
                }
                cursor.path.pop();
            }

            if !descended {
                return Ok(());
            }
            if cursor.current.is_some() {
                return Ok(());
            }
        }
    }

    fn update_current<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
        allow_invalid: bool,
    ) -> Result<()> {
        cursor.current = None;
        let (Some(leaf), Some(item_number)) = (&cursor.leaf, cursor.item_number) else {
            return Ok(());
        };
        let dictionary = self.load_node(pdf, leaf)?;
        let Some(items) = resolved_array(pdf, dictionary.get(K::ITEMS_KEY))? else {
            return Err(structural_error(
                leaf.diagnostic_ref(),
                format!("update ivalue: /{} is not an array", K::ITEMS_KEY),
            ));
        };
        if item_number + 1 >= items.values.len() {
            return Err(structural_error(
                leaf.diagnostic_ref(),
                "update ivalue: items array is too short",
            ));
        }
        let Some(key) = resolved_key::<K, _>(pdf, &items.values[item_number])? else {
            if allow_invalid {
                self.warn(pdf, leaf, format!("item {item_number} has the wrong type"));
                return Ok(());
            }
            return Err(structural_error(
                leaf.diagnostic_ref(),
                format!("item at index {item_number} is not the right type"),
            ));
        };
        cursor.current = Some((key, items.values[item_number + 1].clone()));
        Ok(())
    }

    fn prepare_kid<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        parent: &NodeHandle,
        kid_number: usize,
        kid_object: Object,
    ) -> Result<NodeHandle> {
        if matches!(kid_object, Object::Reference(_)) {
            let (_, terminal_ref) = resolve_ref_chain(pdf, &kid_object)?;
            return terminal_ref
                .map(NodeHandle::indirect)
                .ok_or_else(|| structural_error(parent.diagnostic_ref(), "invalid kid"));
        }

        if self.auto_repair {
            self.warn(
                pdf,
                parent,
                format!("converting kid number {kid_number} to an indirect object"),
            );
            let mut allocator = ObjectAllocator::default();
            let object_ref = make_indirect(pdf, &mut allocator, kid_object)?;
            let mut dictionary = self.load_node(pdf, parent)?;
            // cov:ignore-start: prepare_kid receives kid_object from this same parent Kids array
            let Some(mut kids) = resolved_array(pdf, dictionary.get("Kids"))? else {
                return Err(structural_error(
                    parent.diagnostic_ref(),
                    "node is missing /Kids",
                ));
            };
            // cov:ignore-end
            kids.values[kid_number] = Object::Reference(object_ref);
            kids.store(pdf, &mut dictionary, "Kids");
            self.store_node(pdf, parent, dictionary)?;
            Ok(NodeHandle::indirect(object_ref))
        } else {
            self.warn(
                pdf,
                parent,
                format!("kid number {kid_number} is not an indirect object"),
            );
            Ok(parent.direct_kid(kid_number))
        }
    }

    fn kid_has_tree_shape<R: Read + Seek>(&self, pdf: &mut Pdf<R>, kid: &Object) -> Result<bool> {
        let (resolved, _) = resolve_ref_chain(pdf, kid)?;
        let Object::Dictionary(dictionary) = resolved else {
            return Ok(false);
        };
        Ok(dictionary.get("Kids").is_some() || dictionary.get(K::ITEMS_KEY).is_some())
    }

    fn warn<R: Read + Seek>(&self, pdf: &mut Pdf<R>, node: &NodeHandle, message: impl AsRef<str>) {
        pdf.push_warning(structural_message(node.diagnostic_ref(), message));
    }

    fn load_node<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        handle: &NodeHandle,
    ) -> Result<Dictionary> {
        let mut dictionary = self.load_anchor(pdf, handle)?;
        for &kid_index in &handle.direct_kids {
            let kids = match resolved_array(pdf, dictionary.get("Kids"))? {
                Some(kids) => kids,
                None if dictionary.get("Kids").is_some() => {
                    return Err(structural_error(
                        handle.diagnostic_ref(),
                        "/Kids is not an array",
                    ));
                }
                None => {
                    return Err(structural_error(
                        handle.diagnostic_ref(),
                        "node is missing /Kids",
                    ));
                }
            };
            let kid = kids.values.get(kid_index).ok_or_else(|| {
                structural_error(
                    handle.diagnostic_ref(),
                    format!("invalid kid at index {kid_index}"),
                )
            })?;
            let Object::Dictionary(kid) = kid else {
                return Err(structural_error(
                    handle.diagnostic_ref(),
                    format!("invalid direct kid at index {kid_index}"),
                ));
            };
            dictionary = kid.clone();
        }
        Ok(dictionary)
    }

    fn store_node<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        handle: &NodeHandle,
        replacement: Dictionary,
    ) -> Result<()> {
        let mut dictionaries = vec![self.load_anchor(pdf, handle)?];
        for &kid_index in &handle.direct_kids {
            let parent = dictionaries.last().expect("anchor is present");
            let kids = match resolved_array(pdf, parent.get("Kids"))? {
                Some(kids) => kids,
                None if parent.get("Kids").is_some() => {
                    return Err(structural_error(
                        handle.diagnostic_ref(),
                        "/Kids is not an array",
                    ));
                }
                None => {
                    return Err(structural_error(
                        handle.diagnostic_ref(),
                        "node is missing /Kids",
                    ));
                }
            };
            let kid = kids.values.get(kid_index).ok_or_else(|| {
                structural_error(
                    handle.diagnostic_ref(),
                    format!("invalid kid at index {kid_index}"),
                )
            })?;
            let Object::Dictionary(kid) = kid else {
                return Err(structural_error(
                    handle.diagnostic_ref(),
                    format!("invalid direct kid at index {kid_index}"),
                ));
            };
            dictionaries.push(kid.clone());
        }

        let mut updated = replacement;
        for (&kid_index, mut parent) in handle
            .direct_kids
            .iter()
            .rev()
            .zip(dictionaries.into_iter().rev().skip(1))
        {
            // cov:ignore-start: dictionaries were cloned only after validating every parent Kids array and index above
            let Some(mut kids) = resolved_array(pdf, parent.get("Kids"))? else {
                return Err(structural_error(
                    handle.diagnostic_ref(),
                    "node is missing /Kids",
                ));
            };
            let kid = kids.values.get_mut(kid_index).ok_or_else(|| {
                structural_error(
                    handle.diagnostic_ref(),
                    format!("invalid kid at index {kid_index}"),
                )
            })?;
            // cov:ignore-end
            *kid = Object::Dictionary(updated);
            kids.store(pdf, &mut parent, "Kids");
            updated = parent;
        }

        match handle.anchor {
            NodeAnchor::Root => self.root = Object::Dictionary(updated),
            NodeAnchor::Indirect(object_ref) => {
                pdf.set_object(object_ref, Object::Dictionary(updated));
            }
        }
        Ok(())
    }

    fn load_anchor<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        handle: &NodeHandle,
    ) -> Result<Dictionary> {
        let object = match handle.anchor {
            NodeAnchor::Root => self.root.clone(),
            NodeAnchor::Indirect(object_ref) => pdf.resolve(object_ref)?,
        };
        let Object::Dictionary(dictionary) = object else {
            return Err(structural_error(handle.diagnostic_ref(), "bad node"));
        };
        Ok(dictionary)
    }
}

#[derive(Default)]
struct ObjectAllocator {
    next: Option<u64>,
}

fn make_indirect<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    allocator: &mut ObjectAllocator,
    value: Object,
) -> Result<ObjectRef> {
    // A fresh allocator is created for each tree update, so allocations made
    // through the same Pdf between updates are included in this initial scan.
    // Recursive splits and repair rebuilds then advance in O(1) per object.
    let next = *allocator.next.get_or_insert_with(|| {
        pdf.object_refs()
            .into_iter()
            .map(|object_ref| u64::from(object_ref.number))
            .max()
            .unwrap_or(0)
            + 1
    });
    let number = u32::try_from(next)
        .map_err(|_| Error::Unsupported("object-number space exhausted".to_string()))?;
    allocator.next = Some(next + 1);
    let object_ref = ObjectRef::new(number, 0);
    pdf.set_object(object_ref, value);
    Ok(object_ref)
}

fn binary_search<F>(
    count: usize,
    return_previous_if_missing: bool,
    mut compare: F,
) -> Result<Option<usize>>
where
    F: FnMut(usize) -> Result<Ordering>,
{
    let mut max_index = 1usize;
    while max_index < count {
        max_index <<= 1;
    }
    let mut step = max_index / 2;
    let mut checks = max_index;
    let mut index = step;
    let mut found_index = None;
    let mut found = false;
    let mut found_less_or_equal = false;

    while !found && checks > 0 {
        let status = if index < count {
            let status = compare(index)?;
            if status != Ordering::Less {
                found_less_or_equal = true;
                found_index = Some(index);
            }
            status
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
                    index = index.saturating_sub(step);
                } else {
                    index = index.saturating_add(step);
                }
            }
        }
    }

    Ok(
        if found || (found_less_or_equal && return_previous_if_missing) {
            found_index
        } else {
            None
        },
    )
}

fn structural_error(object_ref: Option<ObjectRef>, message: impl AsRef<str>) -> Error {
    Error::parse(0, structural_message(object_ref, message))
}

fn structural_message(object_ref: Option<ObjectRef>, message: impl AsRef<str>) -> String {
    let prefix = match object_ref {
        Some(object_ref) => format!("Name/Number tree node (object {}): ", object_ref.number),
        None => "Name/Number tree node: ".to_string(),
    };
    format!("{prefix}{}", message.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Dictionary, ObjectRef, Pdf};
    use std::io::Cursor;

    type TestPdf = Pdf<Cursor<Vec<u8>>>;

    fn empty_pdf() -> TestPdf {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.4\n");
        let off1 = bytes.len() as u64;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \n\
                 trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Pdf::open(Cursor::new(bytes)).expect("open")
    }

    fn name_leaf(entries: &[(&[u8], i64)], limits: Option<(&[u8], &[u8])>) -> Object {
        let mut dictionary = Dictionary::new();
        let items = entries
            .iter()
            .flat_map(|(key, value)| [Object::String(key.to_vec()), Object::Integer(*value)])
            .collect();
        dictionary.insert("Names", Object::Array(items));
        if let Some((first, last)) = limits {
            dictionary.insert(
                "Limits",
                Object::Array(vec![
                    Object::String(first.to_vec()),
                    Object::String(last.to_vec()),
                ]),
            );
        }
        Object::Dictionary(dictionary)
    }

    fn two_leaf_name_tree(pdf: &mut TestPdf) -> Object {
        let left_ref = ObjectRef::new(10, 0);
        let right_ref = ObjectRef::new(11, 0);
        pdf.set_object(
            left_ref,
            name_leaf(&[(b"a", 1), (b"b", 2)], Some((b"a", b"b"))),
        );
        pdf.set_object(
            right_ref,
            name_leaf(&[(b"c", 3), (b"d", 4)], Some((b"c", b"d"))),
        );
        let mut root = Dictionary::new();
        root.insert(
            "Kids",
            Object::Array(vec![
                Object::Reference(left_ref),
                Object::Reference(right_ref),
            ]),
        );
        Object::Dictionary(root)
    }

    fn root_with_one_direct_leaf() -> Object {
        let mut root = Dictionary::new();
        root.insert(
            "Kids",
            Object::Array(vec![name_leaf(&[(b"a", 1)], Some((b"a", b"a")))]),
        );
        Object::Dictionary(root)
    }

    fn number_leaf(entries: &[(i64, &[u8])], limits: Option<(i64, i64)>) -> Object {
        let mut dictionary = Dictionary::new();
        let items = entries
            .iter()
            .flat_map(|(key, value)| [Object::Integer(*key), Object::String(value.to_vec())])
            .collect();
        dictionary.insert("Nums", Object::Array(items));
        if let Some((first, last)) = limits {
            dictionary.insert(
                "Limits",
                Object::Array(vec![Object::Integer(first), Object::Integer(last)]),
            );
        }
        Object::Dictionary(dictionary)
    }

    fn two_leaf_number_tree(pdf: &mut TestPdf) -> Object {
        let left_ref = ObjectRef::new(10, 0);
        let right_ref = ObjectRef::new(11, 0);
        pdf.set_object(
            left_ref,
            number_leaf(&[(10, b"ten"), (20, b"twenty")], Some((10, 20))),
        );
        pdf.set_object(
            right_ref,
            number_leaf(&[(30, b"thirty"), (40, b"forty")], Some((30, 40))),
        );
        let mut root = Dictionary::new();
        root.insert(
            "Kids",
            Object::Array(vec![
                Object::Reference(left_ref),
                Object::Reference(right_ref),
            ]),
        );
        Object::Dictionary(root)
    }

    fn collect_number_entries(
        tree: &mut NNTree<NumberKey>,
        pdf: &mut TestPdf,
    ) -> Vec<(i64, Object)> {
        let mut entries = Vec::new();
        let mut cursor = tree.begin(pdf).expect("begin");
        while let Some((key, value)) = cursor.cloned_current() {
            entries.push((key, value));
            tree.next(pdf, &mut cursor).expect("next");
        }
        entries
    }

    fn collect_name_entries(
        tree: &mut NNTree<NameKey>,
        pdf: &mut TestPdf,
    ) -> Vec<(Vec<u8>, Object)> {
        let mut entries = Vec::new();
        let mut cursor = tree.begin(pdf).expect("begin");
        while let Some((key, value)) = cursor.cloned_current() {
            entries.push((key, value));
            tree.next(pdf, &mut cursor).expect("next");
        }
        entries
    }

    fn malformed_name_tree_with_missing_limits_and_valid_pairs(pdf: &mut TestPdf) -> Object {
        let leaf_ref = ObjectRef::new(10, 0);
        pdf.set_object(leaf_ref, name_leaf(&[(b"alpha", 1), (b"beta", 2)], None));
        let mut root = Dictionary::new();
        root.insert("Kids", Object::Array(vec![Object::Reference(leaf_ref)]));
        Object::Dictionary(root)
    }

    fn number_tree_shape(pdf: &mut TestPdf, object: &Object) -> String {
        let resolved = match object {
            Object::Reference(object_ref) => pdf.resolve(*object_ref).expect("resolve node"),
            object => object.clone(),
        };
        let Object::Dictionary(dictionary) = resolved else {
            panic!("tree node must be a dictionary"); // cov:ignore: test-shape guard
        };
        if let Some(Object::Array(items)) = dictionary.get("Nums") {
            let keys = items
                .chunks_exact(2)
                .map(|pair| match pair[0] {
                    Object::Integer(key) => key.to_string(),
                    ref other => panic!("unexpected number-tree key: {other:?}"), // cov:ignore: test-shape guard
                })
                .collect::<Vec<_>>()
                .join(",");
            return format!("L[{keys}]");
        }
        let Object::Array(kids) = dictionary.get("Kids").expect("node shape") else {
            panic!("/Kids must be an array"); // cov:ignore: test-shape guard
        };
        let children = kids
            .iter()
            .map(|kid| number_tree_shape(pdf, kid))
            .collect::<Vec<_>>()
            .join(",");
        format!("K({children})")
    }

    #[test]
    fn name_and_number_codecs_accept_only_their_qpdf_key_types() {
        assert_eq!(
            NameKey::from_object(&Object::String(b"alpha".to_vec())),
            Some(b"alpha".to_vec())
        );
        assert_eq!(NameKey::from_object(&Object::Integer(1)), None);
        assert_eq!(NumberKey::from_object(&Object::Integer(-7)), Some(-7));
        assert_eq!(NumberKey::from_object(&Object::String(b"7".to_vec())), None);
    }

    #[test]
    fn codec_round_trip_preserves_pdf_key_objects() {
        assert_eq!(
            NameKey::to_object(&b"alpha".to_vec()),
            Object::String(b"alpha".to_vec())
        );
        assert_eq!(NumberKey::to_object(&42), Object::Integer(42));
    }

    #[test]
    fn name_codec_matches_qpdf_utf8_value_and_new_unicode_string() {
        assert_eq!(
            NameKey::from_object(&Object::String(vec![0x95])),
            Some("Ł".as_bytes().to_vec())
        );
        assert_eq!(
            NameKey::to_object(&"Ł".as_bytes().to_vec()),
            Object::String(vec![0x95])
        );
        assert_eq!(
            NameKey::to_object(&"😀".as_bytes().to_vec()),
            Object::String(vec![0xfe, 0xff, 0xd8, 0x3d, 0xde, 0x00])
        );
        assert_eq!(
            NameKey::to_object(&b"a\0z".to_vec()),
            Object::String(b"a\0z".to_vec())
        );
        assert_eq!(qpdf_new_unicode_utf8_value(&[0xc2, b'A']), "�A".as_bytes());
        assert_eq!(qpdf_new_unicode_utf8_value(&[0xc0, 0x80]), "�".as_bytes());
        assert_eq!(qpdf_new_unicode_utf8_value(&[0xc2]), "�".as_bytes());
        assert_eq!(qpdf_new_unicode_utf8_value(&[0x80]), "�".as_bytes());
        assert_eq!(
            qpdf_new_unicode_utf8_value(&[0xf8, 0x88, 0x80, 0x80, 0x80]),
            "�".as_bytes()
        );
    }

    #[test]
    fn direct_kid_diagnostics_do_not_blame_the_indirect_anchor() {
        let anchor = ObjectRef::new(17, 0);
        assert_eq!(NodeHandle::indirect(anchor).diagnostic_ref(), Some(anchor));
        assert_eq!(
            NodeHandle::indirect(anchor).direct_kid(0).diagnostic_ref(),
            None
        );
    }

    #[test]
    fn direct_kid_store_writes_back_through_the_parent_array() {
        let mut pdf = empty_pdf();
        let mut leaf = Dictionary::new();
        leaf.insert("Names", Object::Array(Vec::new()));
        let mut root = Dictionary::new();
        root.insert("Kids", Object::Array(vec![Object::Dictionary(leaf)]));
        let mut tree = NNTree::<NameKey>::new(Object::Dictionary(root), false);
        let kid = NodeHandle::root().direct_kid(0);

        let mut changed = tree.load_node(&mut pdf, &kid).unwrap();
        changed.insert(
            "Names",
            Object::Array(vec![Object::String(b"a".to_vec()), Object::Integer(1)]),
        );
        tree.store_node(&mut pdf, &kid, changed).unwrap();

        let Object::Dictionary(root) = tree.root() else {
            panic!("root must remain direct"); // cov:ignore: test-shape guard
        };
        let Object::Array(kids) = root.get("Kids").unwrap() else {
            panic!("root /Kids must remain an array"); // cov:ignore: test-shape guard
        };
        let Object::Dictionary(leaf) = &kids[0] else {
            panic!("kid must remain direct"); // cov:ignore: test-shape guard
        };
        assert_eq!(
            leaf.get("Names"),
            Some(&Object::Array(vec![
                Object::String(b"a".to_vec()),
                Object::Integer(1),
            ]))
        );
    }

    #[test]
    fn indirect_node_store_updates_the_terminal_holder_target() {
        let mut pdf = empty_pdf();
        let holder = ObjectRef::new(20, 0);
        let terminal = ObjectRef::new(21, 0);
        pdf.set_object(holder, Object::Reference(terminal));
        pdf.set_object(terminal, Object::Dictionary(Dictionary::new()));
        let mut tree = NNTree::<NameKey>::new(Object::Reference(holder), true);

        let node = tree.root_handle(&mut pdf).unwrap();
        let mut changed = tree.load_node(&mut pdf, &node).unwrap();
        changed.insert("Names", Object::Array(Vec::new()));
        tree.store_node(&mut pdf, &node, changed.clone()).unwrap();

        assert_eq!(pdf.resolve(terminal).unwrap(), Object::Dictionary(changed));
        assert_eq!(tree.root(), &Object::Reference(holder));
    }

    #[test]
    fn indirect_structural_arrays_are_resolved_and_mutated_in_place() {
        let mut pdf = empty_pdf();
        let items_ref = ObjectRef::new(70, 0);
        let limits_ref = ObjectRef::new(71, 0);
        let leaf_ref = ObjectRef::new(72, 0);
        let kids_ref = ObjectRef::new(73, 0);
        pdf.set_object(
            items_ref,
            Object::Array(vec![
                Object::Integer(10),
                Object::String(b"ten".to_vec()),
                Object::Integer(20),
                Object::String(b"twenty".to_vec()),
            ]),
        );
        pdf.set_object(
            limits_ref,
            Object::Array(vec![
                Object::Integer(10),
                Object::Integer(20),
                Object::Integer(30),
            ]),
        );
        let mut leaf = Dictionary::new();
        leaf.insert("Nums", Object::Reference(items_ref));
        leaf.insert("Limits", Object::Reference(limits_ref));
        pdf.set_object(leaf_ref, Object::Dictionary(leaf));
        pdf.set_object(kids_ref, Object::Array(vec![Object::Reference(leaf_ref)]));
        let mut root = Dictionary::new();
        root.insert("Kids", Object::Reference(kids_ref));
        let mut tree = NNTree::<NumberKey>::new(Object::Dictionary(root), false);

        assert_eq!(
            tree.find(&mut pdf, &20, false).unwrap().current(),
            Some((&20, &Object::String(b"twenty".to_vec())))
        );
        tree.insert(&mut pdf, 15, Object::String(b"fifteen".to_vec()))
            .unwrap();
        assert_eq!(
            tree.remove(&mut pdf, &10).unwrap(),
            Some(Object::String(b"ten".to_vec()))
        );
        assert_eq!(
            collect_number_entries(&mut tree, &mut pdf),
            vec![
                (15, Object::String(b"fifteen".to_vec())),
                (20, Object::String(b"twenty".to_vec())),
            ]
        );
        assert!(matches!(
            pdf.resolve(leaf_ref).unwrap(),
            Object::Dictionary(leaf)
                if leaf.get("Nums") == Some(&Object::Reference(items_ref))
                    && leaf.get("Limits") == Some(&Object::Array(vec![
                        Object::Integer(15),
                        Object::Integer(20),
                    ]))
        ));
        assert!(matches!(
            tree.root(),
            Object::Dictionary(root)
                if root.get("Kids") == Some(&Object::Reference(kids_ref))
        ));
    }

    #[test]
    fn begin_next_previous_last_and_end_match_qpdf_cursor_rules() {
        let mut pdf = empty_pdf();
        let root = two_leaf_name_tree(&mut pdf);
        let mut tree = NNTree::<NameKey>::new(root, true);

        let mut cursor = tree.begin(&mut pdf).unwrap();
        assert_eq!(
            cursor.current().map(|(key, value)| (key.as_slice(), value)),
            Some((b"a".as_slice(), &Object::Integer(1)))
        );
        tree.next(&mut pdf, &mut cursor).unwrap();
        assert_eq!(
            cursor.current().map(|(key, value)| (key.as_slice(), value)),
            Some((b"b".as_slice(), &Object::Integer(2)))
        );
        tree.next(&mut pdf, &mut cursor).unwrap();
        assert_eq!(
            cursor.current().map(|(key, value)| (key.as_slice(), value)),
            Some((b"c".as_slice(), &Object::Integer(3)))
        );

        let mut end = tree.end();
        assert!(!end.positioned());
        tree.next(&mut pdf, &mut end).unwrap();
        assert_eq!(
            end.current().map(|(key, value)| (key.as_slice(), value)),
            Some((b"a".as_slice(), &Object::Integer(1)))
        );

        let mut end = tree.end();
        tree.previous(&mut pdf, &mut end).unwrap();
        assert_eq!(
            end.current().map(|(key, value)| (key.as_slice(), value)),
            Some((b"d".as_slice(), &Object::Integer(4)))
        );

        let mut last = tree.last(&mut pdf).unwrap();
        assert_eq!(
            last.current().map(|(key, value)| (key.as_slice(), value)),
            Some((b"d".as_slice(), &Object::Integer(4)))
        );
        tree.previous(&mut pdf, &mut last).unwrap();
        assert_eq!(
            last.current().map(|(key, value)| (key.as_slice(), value)),
            Some((b"c".as_slice(), &Object::Integer(3)))
        );
    }

    #[test]
    fn direct_kid_is_indirectized_only_when_auto_repair_is_enabled() {
        let mut repaired_pdf = empty_pdf();
        let root = root_with_one_direct_leaf();
        let mut repaired = NNTree::<NameKey>::new(root, true);
        let cursor = repaired.begin(&mut repaired_pdf).unwrap();
        assert!(cursor.positioned());
        assert!(matches!(
            repaired.root(),
            Object::Dictionary(root)
                if matches!(
                    root.get("Kids"),
                    Some(Object::Array(kids))
                        if matches!(kids.first(), Some(Object::Reference(_)))
                )
        ));
        assert!(repaired_pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|entry| entry
                .message
                .contains("converting kid number 0 to an indirect object")));

        let mut strict_pdf = empty_pdf();
        let root = root_with_one_direct_leaf();
        let mut strict = NNTree::<NameKey>::new(root, false);
        let cursor = strict.begin(&mut strict_pdf).unwrap();
        assert!(cursor.positioned());
        assert!(matches!(
            strict.root(),
            Object::Dictionary(root)
                if matches!(
                    root.get("Kids"),
                    Some(Object::Array(kids))
                        if matches!(kids.first(), Some(Object::Dictionary(_)))
                )
        ));
    }

    #[test]
    fn non_dictionary_root_warns_and_produces_end_cursor() {
        let mut pdf = empty_pdf();
        let mut tree = NNTree::<NameKey>::new(Object::Integer(42), true);

        let cursor = tree.begin(&mut pdf).unwrap();

        assert!(!cursor.positioned());
        assert_eq!(
            pdf.repair_diagnostics().entries()[0].message,
            "Name/Number tree node: non-dictionary node while traversing name/number tree"
        );
    }

    #[test]
    fn find_returns_exact_previous_or_end_like_qpdf() {
        let mut pdf = empty_pdf();
        let root = two_leaf_number_tree(&mut pdf);
        let mut tree = NNTree::<NumberKey>::new(root, false);

        assert_eq!(
            tree.find(&mut pdf, &20, false).unwrap().current(),
            Some((&20, &Object::String(b"twenty".to_vec())))
        );
        assert!(!tree.find(&mut pdf, &25, false).unwrap().positioned());
        assert_eq!(
            tree.find(&mut pdf, &25, true).unwrap().current(),
            Some((&20, &Object::String(b"twenty".to_vec())))
        );
        assert!(!tree.find(&mut pdf, &-1, true).unwrap().positioned());
        assert_eq!(
            tree.find(&mut pdf, &99, true).unwrap().current(),
            Some((&40, &Object::String(b"forty".to_vec())))
        );
    }

    #[test]
    fn find_crosses_name_tree_limits() {
        let mut pdf = empty_pdf();
        let root = two_leaf_name_tree(&mut pdf);
        let mut tree = NNTree::<NameKey>::new(root, false);

        let cursor = tree.find(&mut pdf, &b"c".to_vec(), false).unwrap();

        assert_eq!(
            cursor.current().map(|(key, value)| (key.as_slice(), value)),
            Some((b"c".as_slice(), &Object::Integer(3)))
        );
    }

    #[test]
    fn find_reports_the_indirect_node_missing_limits() {
        let mut pdf = empty_pdf();
        let leaf_ref = ObjectRef::new(10, 0);
        pdf.set_object(leaf_ref, number_leaf(&[(10, b"ten")], None));
        let mut root = Dictionary::new();
        root.insert("Kids", Object::Array(vec![Object::Reference(leaf_ref)]));
        let mut tree = NNTree::<NumberKey>::new(Object::Dictionary(root), false);

        let error = match tree.find(&mut pdf, &10, false) {
            Err(error) => error,
            Ok(_) => panic!("missing /Limits must fail targeted lookup"), // cov:ignore: negative-path assertion
        };

        assert_eq!(
            error.to_string(),
            "parse error at byte 0: Name/Number tree node (object 10): node is missing /Limits"
        );
    }

    #[test]
    fn find_reports_cycles_inconsistent_limits_and_empty_selected_kids() {
        let mut pdf = empty_pdf();
        let leaf_ref = ObjectRef::new(10, 0);
        let root_ref = ObjectRef::new(20, 0);
        pdf.set_object(leaf_ref, name_leaf(&[(b"a", 1)], Some((b"a", b"a"))));
        let mut cyclic_root = Dictionary::new();
        cyclic_root.insert(
            "Kids",
            Object::Array(vec![
                Object::Reference(leaf_ref),
                Object::Reference(root_ref),
            ]),
        );
        cyclic_root.insert(
            "Limits",
            Object::Array(vec![
                Object::String(b"b".to_vec()),
                Object::String(b"z".to_vec()),
            ]),
        );
        pdf.set_object(root_ref, Object::Dictionary(cyclic_root));
        let mut cyclic = NNTree::<NameKey>::new(Object::Reference(root_ref), false);
        assert!(cyclic.find(&mut pdf, &b"z".to_vec(), false).is_err());

        let mut inconsistent_root = Dictionary::new();
        inconsistent_root.insert(
            "Kids",
            Object::Array(vec![name_leaf(&[(b"a", 1)], Some((b"z", b"z")))]),
        );
        let mut inconsistent = NNTree::<NameKey>::new(Object::Dictionary(inconsistent_root), false);
        assert!(inconsistent.find(&mut pdf, &b"b".to_vec(), false).is_err());

        let valid_ref = ObjectRef::new(30, 0);
        let empty_ref = ObjectRef::new(31, 0);
        pdf.set_object(valid_ref, name_leaf(&[(b"a", 1)], Some((b"a", b"a"))));
        let mut empty = Dictionary::new();
        empty.insert(
            "Limits",
            Object::Array(vec![
                Object::String(b"b".to_vec()),
                Object::String(b"z".to_vec()),
            ]),
        );
        pdf.set_object(empty_ref, Object::Dictionary(empty));
        let mut root = Dictionary::new();
        root.insert(
            "Kids",
            Object::Array(vec![
                Object::Reference(valid_ref),
                Object::Reference(empty_ref),
            ]),
        );
        let mut malformed = NNTree::<NameKey>::new(Object::Dictionary(root), false);
        assert!(malformed.find(&mut pdf, &b"z".to_vec(), false).is_err());
    }

    #[test]
    fn direct_kid_find_and_limit_validation_cover_strict_paths() {
        let mut pdf = empty_pdf();
        let mut tree = NNTree::<NameKey>::new(root_with_one_direct_leaf(), false);
        assert_eq!(
            tree.find(&mut pdf, &b"a".to_vec(), false)
                .unwrap()
                .current()
                .map(|(key, value)| (key.as_slice(), value)),
            Some((b"a".as_slice(), &Object::Integer(1)))
        );

        let tree = NNTree::<NameKey>::new(Object::Dictionary(Dictionary::new()), false);
        for limits in [
            Object::Array(Vec::new()),
            Object::Array(vec![Object::Integer(1), Object::Integer(2)]),
        ] {
            let mut dictionary = Dictionary::new();
            dictionary.insert("Limits", limits);
            assert!(tree
                .within_limits(&mut pdf, &b"a".to_vec(), &dictionary, None)
                .is_err());
        }
    }

    #[test]
    fn traversal_warns_on_empty_nodes_and_cycles() {
        let mut pdf = empty_pdf();
        let mut empty = NNTree::<NameKey>::new(Object::Dictionary(Dictionary::new()), false);
        assert!(!empty.begin(&mut pdf).unwrap().positioned());

        let cycle_ref = ObjectRef::new(40, 0);
        let mut node = Dictionary::new();
        node.insert("Kids", Object::Array(vec![Object::Reference(cycle_ref)]));
        pdf.set_object(cycle_ref, Object::Dictionary(node));
        let mut cyclic = NNTree::<NameKey>::new(Object::Reference(cycle_ref), false);
        assert!(!cyclic.begin(&mut pdf).unwrap().positioned());

        let warnings = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message.as_str())
            .collect::<Vec<_>>();
        assert!(warnings
            .iter()
            .any(|message| message.contains("neither non-empty /Names nor /Kids")));
        assert!(warnings
            .iter()
            .any(|message| message.contains("loop detected while traversing")));
    }

    #[test]
    fn increment_recovers_from_changed_parent_and_invalid_middle_leaf() {
        let mut pdf = empty_pdf();
        let mut root = Dictionary::new();
        root.insert(
            "Names",
            Object::Array(vec![
                Object::String(b"a".to_vec()),
                Object::Integer(1),
                Object::String(b"b".to_vec()),
                Object::Integer(2),
            ]),
        );
        let mut leaf = NNTree::<NameKey>::new(Object::Dictionary(root), false);
        let mut cursor = leaf.begin(&mut pdf).unwrap();
        leaf.root
            .as_dict_mut()
            .unwrap()
            .insert("Names", Object::Integer(7));
        leaf.next(&mut pdf, &mut cursor).unwrap();
        assert!(!cursor.positioned());

        let mut parent = NNTree::<NameKey>::new(two_leaf_name_tree(&mut pdf), false);
        let mut cursor = parent.begin(&mut pdf).unwrap();
        parent.next(&mut pdf, &mut cursor).unwrap();
        parent.root.as_dict_mut().unwrap().remove("Kids");
        parent.next(&mut pdf, &mut cursor).unwrap();
        assert!(!cursor.positioned());

        let mut cross = NNTree::<NameKey>::new(two_leaf_name_tree(&mut pdf), false);
        let mut cursor = cross.find(&mut pdf, &b"c".to_vec(), false).unwrap();
        cross.previous(&mut pdf, &mut cursor).unwrap();
        assert_eq!(
            cursor.current().map(|(key, _)| key.as_slice()),
            Some(b"b".as_slice())
        );

        let left_ref = ObjectRef::new(50, 0);
        let middle_ref = ObjectRef::new(51, 0);
        let right_ref = ObjectRef::new(52, 0);
        pdf.set_object(left_ref, name_leaf(&[(b"a", 1)], Some((b"a", b"a"))));
        let mut middle = Dictionary::new();
        middle.insert(
            "Names",
            Object::Array(vec![Object::Integer(2), Object::Integer(2)]),
        );
        middle.insert(
            "Limits",
            Object::Array(vec![
                Object::String(b"b".to_vec()),
                Object::String(b"b".to_vec()),
            ]),
        );
        pdf.set_object(middle_ref, Object::Dictionary(middle));
        pdf.set_object(right_ref, name_leaf(&[(b"c", 3)], Some((b"c", b"c"))));
        let mut root = Dictionary::new();
        root.insert(
            "Kids",
            Object::Array(vec![
                Object::Reference(left_ref),
                Object::Reference(middle_ref),
                Object::Reference(right_ref),
            ]),
        );
        let mut tree = NNTree::<NameKey>::new(Object::Dictionary(root), false);
        let mut cursor = tree.begin(&mut pdf).unwrap();
        tree.next(&mut pdf, &mut cursor).unwrap();
        assert_eq!(
            cursor.current().map(|(key, _)| key.as_slice()),
            Some(b"c".as_slice())
        );
    }

    #[test]
    fn update_current_and_root_replacement_reject_malformed_state() {
        let mut pdf = empty_pdf();
        let mut tree = NNTree::<NameKey>::new(Object::Dictionary(Dictionary::new()), false);
        let mut empty = NNTreeCursor::<NameKey>::empty();
        tree.update_current(&mut pdf, &mut empty, false).unwrap();

        tree.root
            .as_dict_mut()
            .unwrap()
            .insert("Names", Object::Integer(1));
        let mut non_array = NNTreeCursor {
            path: Vec::new(),
            leaf: Some(NodeHandle::root()),
            item_number: Some(0),
            current: None,
            marker: PhantomData,
        };
        assert!(tree
            .update_current(&mut pdf, &mut non_array, false)
            .is_err());

        let mut wrong = Dictionary::new();
        wrong.insert(
            "Names",
            Object::Array(vec![Object::Integer(1), Object::Integer(2)]),
        );
        tree.root = Object::Dictionary(wrong);
        let mut cursor = NNTreeCursor {
            path: Vec::new(),
            leaf: Some(NodeHandle::root()),
            item_number: Some(0),
            current: None,
            marker: PhantomData,
        };
        assert!(tree.update_current(&mut pdf, &mut cursor, false).is_err());
        assert!(tree
            .replace_root_contents(&mut pdf, Object::Integer(1))
            .is_err());
    }

    #[test]
    fn direct_node_storage_rejects_each_malformed_path_shape() {
        let malformed_roots = [
            {
                let mut root = Dictionary::new();
                root.insert("Kids", Object::Integer(1));
                root
            },
            Dictionary::new(),
            {
                let mut root = Dictionary::new();
                root.insert("Kids", Object::Array(Vec::new()));
                root
            },
            {
                let mut root = Dictionary::new();
                root.insert("Kids", Object::Array(vec![Object::Integer(1)]));
                root
            },
        ];

        for root in malformed_roots {
            let mut pdf = empty_pdf();
            let handle = NodeHandle::root().direct_kid(0);
            let tree = NNTree::<NameKey>::new(Object::Dictionary(root.clone()), false);
            assert!(tree.load_node(&mut pdf, &handle).is_err());

            let mut tree = NNTree::<NameKey>::new(Object::Dictionary(root), false);
            assert!(tree
                .store_node(&mut pdf, &handle, Dictionary::new())
                .is_err());
        }
    }

    #[test]
    fn insert_orders_keys_replaces_duplicates_and_remove_returns_value() {
        let mut pdf = empty_pdf();
        let mut root = Dictionary::new();
        root.insert("Nums", Object::Array(Vec::new()));
        let mut tree = NNTree::<NumberKey>::new(Object::Dictionary(root), true);

        tree.insert(&mut pdf, 20, Object::String(b"old".to_vec()))
            .unwrap();
        tree.insert(&mut pdf, 10, Object::String(b"ten".to_vec()))
            .unwrap();
        tree.insert(&mut pdf, 30, Object::String(b"thirty".to_vec()))
            .unwrap();
        tree.insert(&mut pdf, 20, Object::String(b"new".to_vec()))
            .unwrap();

        assert_eq!(
            collect_number_entries(&mut tree, &mut pdf),
            vec![
                (10, Object::String(b"ten".to_vec())),
                (20, Object::String(b"new".to_vec())),
                (30, Object::String(b"thirty".to_vec())),
            ]
        );
        assert_eq!(
            tree.remove(&mut pdf, &20).unwrap(),
            Some(Object::String(b"new".to_vec()))
        );
        assert_eq!(tree.remove(&mut pdf, &20).unwrap(), None);
        assert_eq!(
            collect_number_entries(&mut tree, &mut pdf),
            vec![
                (10, Object::String(b"ten".to_vec())),
                (30, Object::String(b"thirty".to_vec())),
            ]
        );
    }

    #[test]
    fn cursor_mutation_handles_end_last_and_empty_root_boundaries() {
        let mut pdf = empty_pdf();
        let mut root = Dictionary::new();
        root.insert("Nums", Object::Array(Vec::new()));
        let mut tree = NNTree::<NumberKey>::new(Object::Dictionary(root), true);

        let mut end = tree.end();
        tree.insert_after(&mut pdf, &mut end, 10, Object::Integer(10))
            .unwrap();
        assert_eq!(end.current(), Some((&10, &Object::Integer(10))));

        let mut empty = tree.end();
        assert_eq!(tree.remove_at(&mut pdf, &mut empty).unwrap(), None);
        tree.insert(&mut pdf, 20, Object::Integer(20)).unwrap();
        let mut last = tree.find(&mut pdf, &20, false).unwrap();
        assert_eq!(
            tree.remove_at(&mut pdf, &mut last).unwrap(),
            Some(Object::Integer(20))
        );
        assert!(!last.positioned());

        let mut only = tree.find(&mut pdf, &10, false).unwrap();
        assert_eq!(
            tree.remove_at(&mut pdf, &mut only).unwrap(),
            Some(Object::Integer(10))
        );
        assert!(!only.positioned());
        assert!(!tree.begin(&mut pdf).unwrap().positioned());
    }

    #[test]
    fn malformed_cursors_report_mutation_errors() {
        fn cursor(item_number: usize) -> NNTreeCursor<NumberKey> {
            NNTreeCursor {
                path: Vec::new(),
                leaf: Some(NodeHandle::root()),
                item_number: Some(item_number),
                current: Some((1, Object::Integer(1))),
                marker: PhantomData,
            }
        }

        let mut pdf = empty_pdf();
        let mut tree = NNTree::<NumberKey>::new(Object::Dictionary(Dictionary::new()), false);
        assert!(tree
            .insert_after(&mut pdf, &mut cursor(0), 2, Object::Integer(2))
            .is_err());
        assert!(tree.remove_at(&mut pdf, &mut cursor(0)).is_err());

        let mut short = Dictionary::new();
        short.insert(
            "Nums",
            Object::Array(vec![Object::Integer(1), Object::Integer(1)]),
        );
        let mut tree = NNTree::<NumberKey>::new(Object::Dictionary(short), false);
        assert!(tree
            .insert_after(&mut pdf, &mut cursor(2), 2, Object::Integer(2))
            .is_err());
        assert!(tree.remove_at(&mut pdf, &mut cursor(2)).is_err());
    }

    #[test]
    fn removing_empty_leaf_handles_missing_first_and_last_parent_kids() {
        fn cursor(kid_number: usize, leaf: ObjectRef) -> NNTreeCursor<NameKey> {
            NNTreeCursor {
                path: vec![PathElement {
                    node: NodeHandle::root(),
                    kid_number,
                }],
                leaf: Some(NodeHandle::indirect(leaf)),
                item_number: None,
                current: None,
                marker: PhantomData,
            }
        }

        let mut pdf = empty_pdf();
        let mut missing = NNTree::<NameKey>::new(Object::Dictionary(Dictionary::new()), false);
        assert!(missing
            .remove_empty_leaf(&mut pdf, &mut cursor(0, ObjectRef::new(60, 0)))
            .is_err());

        let empty_ref = ObjectRef::new(60, 0);
        let valid_ref = ObjectRef::new(61, 0);
        pdf.set_object(empty_ref, name_leaf(&[], None));
        pdf.set_object(valid_ref, name_leaf(&[(b"a", 1)], Some((b"a", b"a"))));

        let mut first_root = Dictionary::new();
        first_root.insert(
            "Kids",
            Object::Array(vec![
                Object::Reference(empty_ref),
                Object::Reference(valid_ref),
            ]),
        );
        let mut first = NNTree::<NameKey>::new(Object::Dictionary(first_root), false);
        let mut first_cursor = cursor(0, empty_ref);
        first
            .remove_empty_leaf(&mut pdf, &mut first_cursor)
            .unwrap();
        assert_eq!(
            first_cursor.current().map(|(key, _)| key.as_slice()),
            Some(b"a".as_slice())
        );

        let mut last_root = Dictionary::new();
        last_root.insert(
            "Kids",
            Object::Array(vec![
                Object::Reference(valid_ref),
                Object::Reference(empty_ref),
            ]),
        );
        let mut last = NNTree::<NameKey>::new(Object::Dictionary(last_root), false);
        let mut last_cursor = cursor(1, empty_ref);
        last.remove_empty_leaf(&mut pdf, &mut last_cursor).unwrap();
        assert!(!last_cursor.positioned());
    }

    #[test]
    fn split_and_limit_helpers_handle_empty_and_malformed_nodes() {
        let mut pdf = empty_pdf();
        let mut cursor = NNTreeCursor::<NumberKey>::empty();

        let mut empty_kids = Dictionary::new();
        empty_kids.insert("Kids", Object::Array(Vec::new()));
        let mut tree = NNTree::<NumberKey>::new(Object::Dictionary(empty_kids), false);
        tree.split_node(&mut pdf, &mut cursor, NodeHandle::root(), None)
            .unwrap();

        let mut empty_items = Dictionary::new();
        empty_items.insert("Nums", Object::Array(Vec::new()));
        let mut tree = NNTree::<NumberKey>::new(Object::Dictionary(empty_items), false);
        tree.split_node(&mut pdf, &mut cursor, NodeHandle::root(), None)
            .unwrap();

        let mut tree = NNTree::<NumberKey>::new(Object::Dictionary(Dictionary::new()), false);
        assert!(tree
            .split_node(&mut pdf, &mut cursor, NodeHandle::root(), None)
            .is_err());
        tree.reset_limits(&mut pdf, &cursor, NodeHandle::root(), Some(0))
            .unwrap();
        assert!(pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|entry| entry.message.contains("unable to determine limits")));

        let mut one_item = Dictionary::new();
        one_item.insert("Nums", Object::Array(vec![Object::Integer(1)]));
        assert_eq!(tree.edge_limits(&mut pdf, &one_item).unwrap(), None);

        let mut bad_first = Dictionary::new();
        bad_first.insert("Kids", Object::Array(vec![Object::Integer(1)]));
        assert_eq!(tree.edge_limits(&mut pdf, &bad_first).unwrap(), None);

        let mut first = Dictionary::new();
        first.insert(
            "Limits",
            Object::Array(vec![Object::Integer(1), Object::Integer(1)]),
        );
        let mut bad_last = Dictionary::new();
        bad_last.insert(
            "Kids",
            Object::Array(vec![Object::Dictionary(first.clone()), Object::Integer(2)]),
        );
        assert_eq!(tree.edge_limits(&mut pdf, &bad_last).unwrap(), None);

        let mut missing_limits = Dictionary::new();
        missing_limits.insert(
            "Kids",
            Object::Array(vec![
                Object::Dictionary(Dictionary::new()),
                Object::Dictionary(first),
            ]),
        );
        assert_eq!(tree.edge_limits(&mut pdf, &missing_limits).unwrap(), None);
    }

    #[test]
    fn split_promotes_root_and_recursively_splits_parent_in_qpdf_order() {
        let mut pdf = empty_pdf();
        let mut root = Dictionary::new();
        root.insert("Nums", Object::Array(Vec::new()));
        let mut tree = NNTree::<NumberKey>::new(Object::Dictionary(root), true);
        tree.set_split_threshold(4);

        for key in 0..13 {
            tree.insert(&mut pdf, key, Object::Integer(key)).unwrap();
        }

        assert_eq!(
            number_tree_shape(&mut pdf, tree.root()),
            concat!(
                "K(",
                "K(L[0,1],L[2,3]),",
                "K(L[4,5],L[6,7],L[8,9],L[10,11,12])",
                ")"
            )
        );
        let Object::Dictionary(root) = tree.root() else {
            panic!("root must be a dictionary"); // cov:ignore: test-shape guard
        };
        assert_eq!(root.get("Limits"), None);
    }

    #[test]
    fn removing_every_split_entry_collapses_root_to_empty_items() {
        let mut pdf = empty_pdf();
        let mut root = Dictionary::new();
        root.insert("Nums", Object::Array(Vec::new()));
        let mut tree = NNTree::<NumberKey>::new(Object::Dictionary(root), true);
        tree.set_split_threshold(4);
        for key in 0..13 {
            tree.insert(&mut pdf, key, Object::Integer(key)).unwrap();
        }

        for key in 0..13 {
            assert_eq!(
                tree.remove(&mut pdf, &key).unwrap(),
                Some(Object::Integer(key))
            );
        }

        assert_eq!(number_tree_shape(&mut pdf, tree.root()), "L[]");
        assert!(!tree.begin(&mut pdf).unwrap().positioned());
    }

    #[test]
    fn removing_split_entries_from_the_end_moves_across_previous_leaves() {
        let mut pdf = empty_pdf();
        let mut root = Dictionary::new();
        root.insert("Nums", Object::Array(Vec::new()));
        let mut tree = NNTree::<NumberKey>::new(Object::Dictionary(root), true);
        tree.set_split_threshold(4);
        for key in 0..13 {
            tree.insert(&mut pdf, key, Object::Integer(key)).unwrap();
        }

        for key in (0..13).rev() {
            assert_eq!(
                tree.remove(&mut pdf, &key).unwrap(),
                Some(Object::Integer(key))
            );
        }

        assert_eq!(number_tree_shape(&mut pdf, tree.root()), "L[]");
    }

    #[test]
    fn default_threshold_splits_33_pairs_as_16_then_17() {
        let mut pdf = empty_pdf();
        let mut root = Dictionary::new();
        root.insert("Nums", Object::Array(Vec::new()));
        let mut tree = NNTree::<NumberKey>::new(Object::Dictionary(root), true);
        for key in 0..33 {
            tree.insert(&mut pdf, key, Object::Integer(key)).unwrap();
        }

        assert_eq!(
            number_tree_shape(&mut pdf, tree.root()),
            concat!(
                "K(",
                "L[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15],",
                "L[16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32]",
                ")"
            )
        );
        let Object::Dictionary(root) = tree.root() else {
            panic!("root must be a dictionary"); // cov:ignore: test-shape guard
        };
        assert_eq!(root.get("Limits"), None);
        let Some(Object::Array(kids)) = root.get("Kids") else {
            panic!("split root must contain /Kids"); // cov:ignore: test-shape guard
        };
        assert_eq!(
            kids,
            &vec![
                Object::Reference(ObjectRef::new(2, 0)),
                Object::Reference(ObjectRef::new(3, 0)),
            ]
        );
        let Object::Dictionary(first) = pdf.resolve(ObjectRef::new(2, 0)).unwrap() else {
            panic!("first leaf must be a dictionary"); // cov:ignore: test-shape guard
        };
        let Object::Dictionary(second) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
            panic!("second leaf must be a dictionary"); // cov:ignore: test-shape guard
        };
        assert_eq!(
            first.get("Limits"),
            Some(&Object::Array(vec![
                Object::Integer(0),
                Object::Integer(15)
            ]))
        );
        assert_eq!(
            second.get("Limits"),
            Some(&Object::Array(vec![
                Object::Integer(16),
                Object::Integer(32)
            ]))
        );
    }

    #[test]
    fn find_repairs_once_and_retries_when_auto_repair_is_enabled() {
        let mut pdf = empty_pdf();
        let root = malformed_name_tree_with_missing_limits_and_valid_pairs(&mut pdf);
        let mut tree = NNTree::<NameKey>::new(root, true);

        let found = tree.find(&mut pdf, &b"beta".to_vec(), false).unwrap();

        assert_eq!(
            found.current().map(|(key, value)| (key.as_slice(), value)),
            Some((b"beta".as_slice(), &Object::Integer(2)))
        );
        let warnings = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("attempting to repair after error:"));
        assert!(warnings[0].contains("node is missing /Limits"));
        assert_eq!(
            collect_name_entries(&mut tree, &mut pdf),
            vec![
                (b"alpha".to_vec(), Object::Integer(1)),
                (b"beta".to_vec(), Object::Integer(2)),
            ]
        );
    }

    #[test]
    fn find_does_not_mutate_when_auto_repair_is_disabled() {
        let mut pdf = empty_pdf();
        let root = malformed_name_tree_with_missing_limits_and_valid_pairs(&mut pdf);
        let original = root.clone();
        let mut tree = NNTree::<NameKey>::new(root, false);

        let error = match tree.find(&mut pdf, &b"beta".to_vec(), false) {
            Err(error) => error,
            Ok(_) => panic!("missing /Limits must fail without repair"), // cov:ignore: negative-path assertion
        };

        assert!(error.to_string().contains("node is missing /Limits"));
        assert_eq!(tree.root(), &original);
        assert!(pdf.repair_diagnostics().entries().is_empty());
    }

    #[test]
    fn repair_skips_invalid_kid_and_retains_later_entries_in_order() {
        let mut pdf = empty_pdf();
        let alpha = ObjectRef::new(10, 0);
        let middle = ObjectRef::new(11, 0);
        let target = ObjectRef::new(12, 0);
        pdf.set_object(
            alpha,
            name_leaf(&[(b"alpha", 1)], Some((b"alpha", b"alpha"))),
        );
        pdf.set_object(
            middle,
            name_leaf(&[(b"middle", 2)], Some((b"middle", b"middle"))),
        );
        pdf.set_object(target, name_leaf(&[(b"zulu", 3)], Some((b"zulu", b"zulu"))));
        let mut root = Dictionary::new();
        root.insert(
            "Kids",
            Object::Array(vec![
                Object::Reference(alpha),
                Object::Reference(middle),
                Object::Integer(42),
                Object::Reference(target),
            ]),
        );
        let mut tree = NNTree::<NameKey>::new(Object::Dictionary(root), true);

        let found = tree.find(&mut pdf, &b"zulu".to_vec(), false).unwrap();

        assert_eq!(
            found.current().map(|(key, _)| key.as_slice()),
            Some(b"zulu".as_slice())
        );
        let warnings = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("attempting to repair after error:"));
        assert!(warnings[0].contains("invalid kid at index 2"));
        assert!(warnings[1].contains("skipping over invalid kid at index 2"));
        assert_eq!(
            collect_name_entries(&mut tree, &mut pdf),
            vec![
                (b"alpha".to_vec(), Object::Integer(1)),
                (b"middle".to_vec(), Object::Integer(2)),
                (b"zulu".to_vec(), Object::Integer(3)),
            ]
        );
    }

    #[test]
    fn repair_updates_terminal_indirect_root_and_preserves_holder_and_other_keys() {
        let mut pdf = empty_pdf();
        let leaf = ObjectRef::new(10, 0);
        let holder = ObjectRef::new(20, 0);
        let terminal = ObjectRef::new(21, 0);
        pdf.set_object(leaf, name_leaf(&[(b"alpha", 1), (b"beta", 2)], None));
        let mut root = Dictionary::new();
        root.insert("Keep", Object::Integer(7));
        root.insert("Kids", Object::Array(vec![Object::Reference(leaf)]));
        pdf.set_object(holder, Object::Reference(terminal));
        pdf.set_object(terminal, Object::Dictionary(root));
        let mut tree = NNTree::<NameKey>::new(Object::Reference(holder), true);

        let found = tree.find(&mut pdf, &b"beta".to_vec(), false).unwrap();

        assert_eq!(
            found.current().map(|(key, _)| key.as_slice()),
            Some(b"beta".as_slice())
        );
        assert_eq!(pdf.resolve(holder).unwrap(), Object::Reference(terminal));
        let Object::Dictionary(repaired) = pdf.resolve(terminal).unwrap() else {
            panic!("terminal root must remain a dictionary"); // cov:ignore: test-shape guard
        };
        assert_eq!(repaired.get("Keep"), Some(&Object::Integer(7)));
        assert!(matches!(repaired.get("Names"), Some(Object::Array(_))));
        assert_eq!(repaired.get("Kids"), None);
        assert_eq!(tree.root(), &Object::Reference(holder));
    }

    #[test]
    fn repair_skips_wrong_key_type_and_keeps_later_valid_pair() {
        let mut pdf = empty_pdf();
        let mut names = Vec::new();
        for (key, value) in [
            (Object::String(b"alpha".to_vec()), 1),
            (Object::String(b"middle".to_vec()), 2),
            (Object::Integer(42), 3),
            (Object::String(b"zulu".to_vec()), 4),
        ] {
            names.push(key);
            names.push(Object::Integer(value));
        }
        let mut root = Dictionary::new();
        root.insert("Names", Object::Array(names));
        let mut tree = NNTree::<NameKey>::new(Object::Dictionary(root), true);

        let found = tree.find(&mut pdf, &b"zulu".to_vec(), false).unwrap();

        assert_eq!(
            found.current().map(|(key, value)| (key.as_slice(), value)),
            Some((b"zulu".as_slice(), &Object::Integer(4)))
        );
        assert_eq!(
            collect_name_entries(&mut tree, &mut pdf),
            vec![
                (b"alpha".to_vec(), Object::Integer(1)),
                (b"middle".to_vec(), Object::Integer(2)),
                (b"zulu".to_vec(), Object::Integer(4)),
            ]
        );
        let warnings = pdf.repair_diagnostics().entries();
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0]
            .message
            .contains("attempting to repair after error:"));
        assert!(warnings[0]
            .message
            .contains("item at index 4 is not the right type"));
        assert!(warnings[1].message.contains("item 4 has the wrong type"));
    }

    #[test]
    fn short_first_pair_remains_fatal_after_single_repair_warning() {
        let mut pdf = empty_pdf();
        let mut root = Dictionary::new();
        root.insert(
            "Names",
            Object::Array(vec![Object::String(b"alpha".to_vec())]),
        );
        let mut tree = NNTree::<NameKey>::new(Object::Dictionary(root), true);

        let error = match tree.find(&mut pdf, &b"alpha".to_vec(), false) {
            Err(error) => error,
            Ok(_) => panic!("short first pair must remain fatal"), // cov:ignore: negative-path assertion
        };

        assert!(error
            .to_string()
            .contains("update ivalue: items array is too short"));
        let warnings = pdf.repair_diagnostics().entries();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0]
            .message
            .contains("attempting to repair after error:"));
    }

    #[test]
    fn repair_splits_33_pairs_as_16_then_17_with_qpdf_allocation_order() {
        let mut pdf = empty_pdf();
        let leaf_ref = ObjectRef::new(10, 0);
        let mut names = Vec::new();
        for key in 0..33 {
            names.push(Object::String(format!("k{key:02}").into_bytes()));
            names.push(Object::Integer(key));
        }
        let mut leaf = Dictionary::new();
        leaf.insert("Names", Object::Array(names));
        pdf.set_object(leaf_ref, Object::Dictionary(leaf));
        let mut root = Dictionary::new();
        root.insert("Kids", Object::Array(vec![Object::Reference(leaf_ref)]));
        let mut tree = NNTree::<NameKey>::new(Object::Dictionary(root), true);

        let found = tree.find(&mut pdf, &b"k32".to_vec(), false).unwrap();

        assert_eq!(
            found.current().map(|(_, value)| value),
            Some(&Object::Integer(32))
        );
        let Object::Dictionary(root) = tree.root() else {
            panic!("repaired root must be a dictionary"); // cov:ignore: test-shape guard
        };
        let Some(Object::Array(kids)) = root.get("Kids") else {
            panic!("repaired root must contain /Kids"); // cov:ignore: test-shape guard
        };
        assert_eq!(
            kids,
            &vec![
                Object::Reference(ObjectRef::new(11, 0)),
                Object::Reference(ObjectRef::new(12, 0)),
            ]
        );
        let Object::Dictionary(first) = pdf.resolve(ObjectRef::new(11, 0)).unwrap() else {
            panic!("first repaired leaf must be a dictionary"); // cov:ignore: test-shape guard
        };
        let Object::Dictionary(second) = pdf.resolve(ObjectRef::new(12, 0)).unwrap() else {
            panic!("second repaired leaf must be a dictionary"); // cov:ignore: test-shape guard
        };
        assert_eq!(
            first.get("Limits"),
            Some(&Object::Array(vec![
                Object::String(b"k00".to_vec()),
                Object::String(b"k15".to_vec()),
            ]))
        );
        assert_eq!(
            second.get("Limits"),
            Some(&Object::Array(vec![
                Object::String(b"k16".to_vec()),
                Object::String(b"k32".to_vec()),
            ]))
        );
    }

    #[test]
    fn repair_with_zero_surviving_entries_installs_empty_items_array() {
        let mut pdf = empty_pdf();
        let mut root = Dictionary::new();
        root.insert(
            "Names",
            Object::Array(vec![Object::Integer(42), Object::Integer(1)]),
        );
        root.insert("Keep", Object::Integer(7));
        let mut tree = NNTree::<NameKey>::new(Object::Dictionary(root), true);

        let found = tree.find(&mut pdf, &b"target".to_vec(), false).unwrap();

        assert!(!found.positioned());
        let Object::Dictionary(root) = tree.root() else {
            panic!("root must remain direct"); // cov:ignore: test-shape guard
        };
        assert_eq!(root.get("Names"), Some(&Object::Array(Vec::new())));
        assert_eq!(root.get("Kids"), None);
        assert_eq!(root.get("Keep"), Some(&Object::Integer(7)));
    }

    #[test]
    fn object_allocator_scans_once_per_update_session() {
        let mut pdf = empty_pdf();
        pdf.set_object(ObjectRef::new(5, 0), Object::Null);
        let mut allocator = ObjectAllocator::default();

        assert_eq!(
            make_indirect(&mut pdf, &mut allocator, Object::Integer(6)).unwrap(),
            ObjectRef::new(6, 0)
        );
        assert_eq!(
            make_indirect(&mut pdf, &mut allocator, Object::Integer(7)).unwrap(),
            ObjectRef::new(7, 0)
        );

        pdf.set_object(ObjectRef::new(9, 0), Object::Null);
        let mut next_update = ObjectAllocator::default();
        assert_eq!(
            make_indirect(&mut pdf, &mut next_update, Object::Integer(10)).unwrap(),
            ObjectRef::new(10, 0)
        );
    }

    #[test]
    fn direct_kid_indirectization_propagates_object_number_exhaustion() {
        let mut pdf = empty_pdf();
        pdf.set_object(ObjectRef::new(u32::MAX, 0), Object::Null);
        let root = root_with_one_direct_leaf();
        let mut tree = NNTree::<NameKey>::new(root, true);

        let error = match tree.begin(&mut pdf) {
            Err(error) => error,
            Ok(_) => panic!("object-number exhaustion must be fatal"), // cov:ignore: negative-path assertion
        };

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: object-number space exhausted"
        );
    }
}

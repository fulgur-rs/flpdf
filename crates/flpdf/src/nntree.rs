//! Mirrors qpdf 11.9.0 `libqpdf/NNTree.cc`.
//!
//! Public wrappers corresponding to `QPDFNameTreeObjectHelper` and
//! `QPDFNumberTreeObjectHelper` are added in the next stacked layer.

use crate::ref_chain::resolve_ref_chain;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::cmp::Ordering;
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
            Object::String(value) => Some(value.clone()),
            _ => None,
        }
    }

    fn to_object(key: &Self::Key) -> Object {
        Object::String(key.clone())
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

#[derive(Clone, Debug, Eq, PartialEq)]
enum NodeAnchor {
    Root,
    Indirect(ObjectRef),
}

#[derive(Clone, Debug, Eq, PartialEq)]
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

    pub(crate) fn valid(&self) -> bool {
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
        self.find_internal(pdf, key, return_previous_if_missing)
    }

    fn find_internal<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: &K::Key,
        return_previous_if_missing: bool,
    ) -> Result<NNTreeCursor<K>> {
        let first = self.begin(pdf)?;
        let Some((first_key, _)) = first.current() else {
            return Ok(self.end());
        };
        if K::compare(key, first_key) == Ordering::Less {
            return Ok(self.end());
        }

        let root = self.root_handle(pdf)?;
        let root_diagnostic_ref = root.diagnostic_ref();
        let mut node = root;
        let mut seen = Vec::new();
        let mut cursor = NNTreeCursor::empty();

        loop {
            if seen.contains(&node) {
                return Err(structural_error(
                    node.diagnostic_ref(),
                    "loop detected in find",
                ));
            }
            seen.push(node.clone());

            let dictionary = self
                .load_node(pdf, &node)
                .map_err(|_| structural_error(node.diagnostic_ref(), "bad node during find"))?;
            let items = match dictionary.get(K::ITEMS_KEY) {
                Some(Object::Array(items)) => Some(items),
                _ => None,
            };
            let kids = match dictionary.get("Kids") {
                Some(Object::Array(kids)) => Some(kids),
                _ => None,
            };

            if let Some(items) = items.filter(|items| !items.is_empty()) {
                let index = binary_search(items.len() / 2, return_previous_if_missing, |index| {
                    let item_number = 2 * index;
                    let Some(item_key) = items.get(item_number).and_then(K::from_object) else {
                        return Err(structural_error(
                            root_diagnostic_ref,
                            format!("item at index {item_number} is not the right type"),
                        ));
                    };
                    Ok(K::compare(key, &item_key))
                })?;
                if let Some(index) = index {
                    cursor.leaf = Some(node);
                    cursor.item_number = Some(2 * index);
                    self.update_current(pdf, &mut cursor, false)?;
                }
                return Ok(cursor);
            }

            if let Some(kids) = kids.filter(|kids| !kids.is_empty()) {
                let index = binary_search(kids.len(), true, |index| {
                    let kid = kids.get(index).expect("binary-search index is in range");
                    let (resolved, terminal_ref) = resolve_ref_chain(pdf, kid)?;
                    let Object::Dictionary(kid_dictionary) = resolved else {
                        return Err(structural_error(
                            root_diagnostic_ref,
                            format!("invalid kid at index {index}"),
                        ));
                    };
                    self.within_limits(key, &kid_dictionary, terminal_ref)
                })?
                .ok_or_else(|| {
                    structural_error(
                        node.diagnostic_ref(),
                        "unexpected -1 from binary search of kids; limits may by wrong",
                    )
                })?;
                let kid_object = kids[index].clone();
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

    fn within_limits(
        &self,
        key: &K::Key,
        dictionary: &Dictionary,
        object_ref: Option<ObjectRef>,
    ) -> Result<Ordering> {
        let Some(Object::Array(limits)) = dictionary.get("Limits") else {
            return Err(structural_error(object_ref, "node is missing /Limits"));
        };
        let (Some(first), Some(last)) = (
            limits.first().and_then(K::from_object),
            limits.get(1).and_then(K::from_object),
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
        let mut seen: Vec<NodeHandle> = cursor
            .path
            .iter()
            .map(|element| element.node.clone())
            .collect();
        let mut node = start;

        loop {
            if seen.contains(&node) {
                self.warn(
                    pdf,
                    &node,
                    "loop detected while traversing name/number tree",
                );
                break;
            }
            seen.push(node.clone());

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
            let items = match dictionary.get(K::ITEMS_KEY) {
                Some(Object::Array(items)) => Some(items),
                _ => None,
            };
            let kids = match dictionary.get("Kids") {
                Some(Object::Array(kids)) => Some(kids),
                _ => None,
            };

            if let Some(items) = items.filter(|items| !items.is_empty()) {
                let item_number = if first {
                    0
                } else {
                    items.len().saturating_sub(2)
                };
                cursor.leaf = Some(node);
                cursor.item_number = Some(item_number);
                self.update_current(pdf, cursor, false)?;
                return Ok(true);
            }

            if let Some(kids) = kids.filter(|kids| !kids.is_empty()) {
                let kid_number = if first { 0 } else { kids.len() - 1 };
                let kid_object = kids[kid_number].clone();
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
            let items = match dictionary.get(K::ITEMS_KEY) {
                Some(Object::Array(items)) => items,
                _ => {
                    cursor.clear_position();
                    return Ok(());
                }
            };
            let item_number = cursor.item_number.expect("checked above");
            let candidate = if backward {
                item_number.checked_sub(2)
            } else {
                item_number
                    .checked_add(2)
                    .filter(|index| *index < items.len())
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
                let Some(Object::Array(kids)) = dictionary.get("Kids") else {
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
                            .filter(|index| *index < kids.len())
                    };
                    let Some(next_index) = next_index else {
                        break;
                    };
                    kid_number = next_index;
                    let kid_object = kids[kid_number].clone();
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
        let items = match dictionary.get(K::ITEMS_KEY) {
            Some(Object::Array(items)) => items,
            _ => {
                return Err(structural_error(
                    leaf.diagnostic_ref(),
                    format!("update ivalue: /{} is not an array", K::ITEMS_KEY),
                ));
            }
        };
        if item_number + 1 >= items.len() {
            if allow_invalid {
                self.warn(pdf, leaf, "items array doesn't have enough elements");
                return Ok(());
            }
            return Err(structural_error(
                leaf.diagnostic_ref(),
                "update ivalue: items array is too short",
            ));
        }
        let Some(key) = K::from_object(&items[item_number]) else {
            if allow_invalid {
                self.warn(pdf, leaf, format!("item {item_number} has the wrong type"));
                return Ok(());
            }
            return Err(structural_error(
                leaf.diagnostic_ref(),
                format!("item at index {item_number} is not the right type"),
            ));
        };
        cursor.current = Some((key, items[item_number + 1].clone()));
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
            let object_ref = make_indirect(pdf, kid_object)?;
            let mut dictionary = self.load_node(pdf, parent)?;
            let Some(Object::Array(mut kids)) = dictionary.remove("Kids") else {
                return Err(structural_error(
                    parent.diagnostic_ref(),
                    "node is missing /Kids",
                ));
            };
            kids[kid_number] = Object::Reference(object_ref);
            dictionary.insert("Kids", Object::Array(kids));
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
            let kids = match dictionary.get("Kids") {
                Some(Object::Array(kids)) => kids,
                Some(_) => {
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
            let kid = kids.get(kid_index).ok_or_else(|| {
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
            let kids = match parent.get("Kids") {
                Some(Object::Array(kids)) => kids,
                Some(_) => {
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
            let kid = kids.get(kid_index).ok_or_else(|| {
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
            let Some(Object::Array(mut kids)) = parent.remove("Kids") else {
                return Err(structural_error(
                    handle.diagnostic_ref(),
                    "node is missing /Kids",
                ));
            };
            let kid = kids.get_mut(kid_index).ok_or_else(|| {
                structural_error(
                    handle.diagnostic_ref(),
                    format!("invalid kid at index {kid_index}"),
                )
            })?;
            *kid = Object::Dictionary(updated);
            parent.insert("Kids", Object::Array(kids));
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

fn make_indirect<R: Read + Seek>(pdf: &mut Pdf<R>, value: Object) -> Result<ObjectRef> {
    let next = pdf
        .object_refs()
        .into_iter()
        .map(|object_ref| object_ref.number)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| Error::Unsupported("object-number space exhausted".to_string()))?;
    let object_ref = ObjectRef::new(next, 0);
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
            NameKey::to_object(&b"a\0z".to_vec()),
            Object::String(b"a\0z".to_vec())
        );
        assert_eq!(NumberKey::to_object(&42), Object::Integer(42));
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
            panic!("root must remain direct");
        };
        let Object::Array(kids) = root.get("Kids").unwrap() else {
            panic!("root /Kids must remain an array");
        };
        let Object::Dictionary(leaf) = &kids[0] else {
            panic!("kid must remain direct");
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
        assert!(!end.valid());
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
        assert!(cursor.valid());
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
        assert!(cursor.valid());
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

        assert!(!cursor.valid());
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
        assert!(!tree.find(&mut pdf, &25, false).unwrap().valid());
        assert_eq!(
            tree.find(&mut pdf, &25, true).unwrap().current(),
            Some((&20, &Object::String(b"twenty".to_vec())))
        );
        assert!(!tree.find(&mut pdf, &-1, true).unwrap().valid());
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
            Ok(_) => panic!("missing /Limits must fail targeted lookup"),
        };

        assert_eq!(
            error.to_string(),
            "parse error at byte 0: Name/Number tree node (object 10): node is missing /Limits"
        );
    }
}

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
        match self.anchor {
            NodeAnchor::Indirect(object_ref) => Some(object_ref),
            NodeAnchor::Root => None,
        }
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

    fn root_handle<R: Read + Seek>(&self, pdf: &mut Pdf<R>) -> Result<NodeHandle> {
        let (root, terminal_ref) = resolve_ref_chain(pdf, &self.root)?;
        if !matches!(root, Object::Dictionary(_)) {
            return Err(structural_error(terminal_ref, "bad node"));
        }
        Ok(terminal_ref.map_or_else(NodeHandle::root, NodeHandle::indirect))
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

fn structural_error(object_ref: Option<ObjectRef>, message: impl AsRef<str>) -> Error {
    let prefix = match object_ref {
        Some(object_ref) => format!("Name/Number tree node (object {}): ", object_ref.number),
        None => "Name/Number tree node: ".to_string(),
    };
    Error::parse(0, format!("{prefix}{}", message.as_ref()))
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
}

//! qpdf correspondence: `QPDFWriter::enqueueObjectsPCLm`.
//!
//! PCLm does not use the ordinary Catalog-first queue. qpdf reserves output
//! numbers as it enqueues page objects, their contents, image strips, and the
//! synthetic image-transform streams. The queue then continues with references
//! discovered while serializing those objects and ends with the Catalog.

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Seek};

use crate::{Object, ObjectRef, Pdf, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Item {
    Source {
        source: ObjectRef,
        output: ObjectRef,
    },
    Synthetic {
        output: ObjectRef,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct Plan {
    pub(crate) items: Vec<Item>,
    pub(crate) old_to_new: HashMap<ObjectRef, ObjectRef>,
    pub(crate) root: ObjectRef,
}

impl Plan {
    pub(crate) fn build<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Self> {
        let root = pdf.root_ref().ok_or(crate::Error::Missing("/Root"))?;
        let mut builder = Builder {
            pdf,
            items: Vec::new(),
            old_to_new: HashMap::new(),
            next_output: 1,
        };

        for page in crate::pages::page_refs(builder.pdf)? {
            builder.enqueue_reference(page);

            let page_object = builder.resolve_terminal(Object::Reference(page))?;
            let Some(page_dict) = page_object.as_dict() else {
                continue;
            };

            if let Some(contents) = page_dict.get("Contents") {
                builder.enqueue_value(contents)?;
            }

            if let Some(xobjects) = builder.page_xobjects(page_dict)? {
                for (_, image) in xobjects.iter() {
                    builder.enqueue_value(image)?;
                    builder.enqueue_synthetic();
                }
            }
        }

        builder.enqueue_reference(root);

        let mut cursor = 0;
        while cursor < builder.items.len() {
            let item = builder.items[cursor].clone();
            cursor += 1;
            let Item::Source { source, .. } = item else {
                continue;
            };
            let object = builder.pdf.resolve(source)?;
            builder.enqueue_value_with_stream_length_policy(&object)?;
        }

        let root = builder
            .old_to_new
            .get(&root)
            .copied()
            .ok_or(crate::Error::Missing("/Root"))?;
        Ok(Self {
            items: builder.items,
            old_to_new: builder.old_to_new,
            root,
        })
    }
}

struct Builder<'pdf, R: Read + Seek + 'static> {
    pdf: &'pdf mut Pdf<R>,
    items: Vec<Item>,
    old_to_new: HashMap<ObjectRef, ObjectRef>,
    next_output: u32,
}

impl<R: Read + Seek + 'static> Builder<'_, R> {
    fn enqueue_reference(&mut self, source: ObjectRef) {
        if source.number == 0 || self.old_to_new.contains_key(&source) {
            return;
        }
        let output = ObjectRef::new(self.next_output, 0);
        self.next_output = self.next_output.saturating_add(1);
        self.old_to_new.insert(source, output);
        self.items.push(Item::Source { source, output });
    }

    fn enqueue_synthetic(&mut self) {
        let output = ObjectRef::new(self.next_output, 0);
        self.next_output = self.next_output.saturating_add(1);
        self.items.push(Item::Synthetic { output });
    }

    fn enqueue_value(&mut self, value: &Object) -> Result<()> {
        self.enqueue_value_with_policy(value, false)
    }

    fn enqueue_value_with_stream_length_policy(&mut self, value: &Object) -> Result<()> {
        self.enqueue_value_with_policy(value, true)
    }

    fn enqueue_value_with_policy(&mut self, value: &Object, skip_length: bool) -> Result<()> {
        match value {
            Object::Reference(reference) => self.enqueue_reference(*reference),
            Object::Array(items) => {
                for item in items {
                    self.enqueue_value_with_policy(item, skip_length)?;
                }
            }
            Object::Dictionary(dict) => {
                let entries = crate::qpdf_null::snapshot_entries(dict, skip_length);
                for (_, value) in crate::qpdf_null::visible_entries(self.pdf, entries)? {
                    self.enqueue_value_with_policy(&value, skip_length)?;
                }
            }
            Object::Stream(stream) => {
                let entries = crate::qpdf_null::snapshot_entries(&stream.dict, true);
                for (_, value) in crate::qpdf_null::visible_entries(self.pdf, entries)? {
                    self.enqueue_value_with_policy(&value, true)?;
                }
            }
            Object::Null
            | Object::Boolean(_)
            | Object::Integer(_)
            | Object::Real(_)
            | Object::Name(_)
            | Object::String(_)
            | Object::RealLiteral { .. }
            | Object::Operator(_)
            | Object::InlineImage(_) => {}
        }
        Ok(())
    }

    fn resolve_terminal(&mut self, value: Object) -> Result<Object> {
        let mut current = value;
        let mut visited = BTreeSet::new();
        loop {
            let Object::Reference(reference) = current else {
                return Ok(current);
            };
            if !visited.insert(reference) {
                return Ok(Object::Null);
            }
            current = self.pdf.resolve(reference)?;
        }
    }

    fn page_xobjects(&mut self, page: &crate::Dictionary) -> Result<Option<crate::Dictionary>> {
        let Some(resources) = page.get("Resources").cloned() else {
            return Ok(None);
        };
        let resources = self.resolve_terminal(resources)?;
        let Some(resources) = resources.as_dict() else {
            return Ok(None);
        };
        let Some(xobjects) = resources.get("XObject").cloned() else {
            return Ok(None);
        };
        Ok(self.resolve_terminal(xobjects)?.into_dict())
    }
}

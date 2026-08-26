//! qpdf correspondence: `QPDFWriter::enqueueObjectsPCLm`.
//!
//! PCLm does not use the ordinary Catalog-first queue. qpdf reserves output
//! numbers as it enqueues page objects, their contents, image strips, and the
//! synthetic image-transform streams. The queue then continues with references
//! discovered while serializing those objects and ends with the Catalog.

use std::collections::HashMap;
use std::io::{Read, Seek};

use crate::{ObjectHandle, ObjectRef, Pdf, Result};

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
            let page_handle = builder.pdf.get_object_handle(page);
            let page_object = builder.resolve_terminal(page_handle)?;
            let Some(page_dict) = page_object.as_dictionary() else {
                continue; // cov:ignore: page_refs yields only dictionary /Type /Page leaves
            };

            builder.enqueue_reference(page);

            let contents = page_dict
                .get(b"/Contents".as_slice())
                .cloned()
                .unwrap_or_else(ObjectHandle::null);
            if !contents.is_null() {
                builder.enqueue_value(&contents)?;
            }

            if let Some(xobjects) = builder.page_xobjects(&page_object)? {
                for image in xobjects.values() {
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
            let object = builder.pdf.get_object_handle(source);
            builder.pdf.resolve(&object)?;
            builder.enqueue_object_children_with_stream_length_policy(&object)?;
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

    fn enqueue_value(&mut self, value: &ObjectHandle) -> Result<()> {
        self.enqueue_value_with_policy(value, false)
    }

    /// Enqueue the references owned by an already-queued source object.
    ///
    /// The queue item itself is indirect and must not be passed through
    /// `enqueue_value_with_policy`: that function correctly treats an
    /// indirect handle as one reference and returns, but doing so at this
    /// boundary would skip the object's dictionary children entirely. qpdf's
    /// `writeObject` discovers those children while emitting the queued item;
    /// PCLm reserves the complete queue before emission, so this explicit
    /// child walk preserves the same reservation order.
    fn enqueue_object_children_with_stream_length_policy(
        &mut self,
        value: &ObjectHandle,
    ) -> Result<()> {
        self.pdf.resolve(value)?;
        if let Some(entries) = value.as_dictionary() {
            for (key, child) in entries {
                if key.as_slice() == b"/Length" || child.try_is_null()? {
                    continue;
                }
                self.enqueue_value_with_policy(&child, true)?;
            }
        } else if let Some(stream_dict) = value.as_stream_dict() {
            self.pdf.resolve(&stream_dict)?;
            if let Some(entries) = stream_dict.as_dictionary() {
                for (key, child) in entries {
                    if key.as_slice() == b"/Length" || child.try_is_null()? {
                        continue;
                    }
                    self.enqueue_value_with_policy(&child, true)?;
                }
            }
        }
        Ok(())
    }

    fn enqueue_value_with_policy(&mut self, value: &ObjectHandle, skip_length: bool) -> Result<()> {
        if let Some(reference) = value.object_ref().or_else(|| value.as_reference()) {
            self.enqueue_reference(reference);
            return Ok(());
        }

        self.pdf.resolve(value)?;
        if let Some(items) = value.as_array() {
            for item in items {
                self.enqueue_value_with_policy(&item, skip_length)?;
            }
        } else if let Some(entries) = value.as_dictionary() {
            for (key, child) in entries {
                if skip_length && key.as_slice() == b"/Length" {
                    continue;
                }
                if child.try_is_null()? {
                    continue;
                }
                self.enqueue_value_with_policy(&child, skip_length)?;
            }
        } else if let Some(stream_dict) = value.as_stream_dict() {
            self.pdf.resolve(&stream_dict)?;
            if let Some(entries) = stream_dict.as_dictionary() {
                for (key, child) in entries {
                    if child.try_is_null()? || key.as_slice() == b"/Length" {
                        continue;
                    }
                    self.enqueue_value_with_policy(&child, true)?;
                }
            }
        }
        Ok(())
    }

    fn resolve_terminal(&mut self, value: ObjectHandle) -> Result<ObjectHandle> {
        self.pdf.resolve_to_terminal(&value)
    }

    fn page_xobjects(
        &mut self,
        page: &ObjectHandle,
    ) -> Result<Option<std::collections::BTreeMap<Vec<u8>, ObjectHandle>>> {
        let resources = page.get_key(b"/Resources");
        if resources.is_null() {
            return Ok(None);
        }
        let resources = self.resolve_terminal(resources)?;
        let Some(resources) = resources.as_dictionary() else {
            return Ok(None);
        };
        let Some(xobjects) = resources.get(b"/XObject".as_slice()).cloned() else {
            return Ok(None);
        };
        Ok(self.resolve_terminal(xobjects)?.as_dictionary())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read, Seek, SeekFrom};

    struct ReadFailingCursor {
        inner: Cursor<Vec<u8>>,
        fail_reads: bool,
    }

    impl Read for ReadFailingCursor {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.fail_reads {
                return Err(std::io::Error::other("injected PCLm planner read failure"));
            }
            self.inner.read(buffer)
        }
    }

    impl Seek for ReadFailingCursor {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    fn fixture_pdf() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(
            include_bytes!("../../../../tests/fixtures/compat/three-page.pdf").to_vec(),
        ))
        .expect("fixture must open")
    }

    fn builder<'pdf>(pdf: &'pdf mut Pdf<Cursor<Vec<u8>>>) -> Builder<'pdf, Cursor<Vec<u8>>> {
        Builder {
            pdf,
            items: Vec::new(),
            old_to_new: HashMap::new(),
            next_output: 1,
        }
    }

    #[test]
    fn plan_propagates_page_contents_resolution_errors() {
        let mut pdf = Pdf::open(ReadFailingCursor {
            inner: Cursor::new(
                include_bytes!("../../../../tests/fixtures/compat/three-page.pdf").to_vec(),
            ),
            fail_reads: false,
        })
        .expect("fixture must open");
        let page = crate::pages::page_refs(&mut pdf).unwrap()[0];
        let page_handle = pdf.get_object_handle(page);
        pdf.resolve(&page_handle).unwrap();
        page_handle
            .replace_key(
                b"/Contents",
                ObjectHandle::dictionary(vec![(
                    b"/Broken".to_vec(),
                    pdf.get_object_handle(ObjectRef::new(11, 0)),
                )]),
            )
            .unwrap();
        pdf.mark_object_handle_dirty(&page_handle).unwrap();
        pdf.resolver
            .with_reader_mut(|reader| reader.fail_reads = true);

        assert!(matches!(Plan::build(&mut pdf), Err(crate::Error::Io(_))));
    }

    #[test]
    fn plan_enqueues_xobject_and_its_synthetic_transform() {
        let mut pdf = fixture_pdf();
        let page = crate::pages::page_refs(&mut pdf).unwrap()[0];
        let page_handle = pdf.get_object_handle(page);
        pdf.resolve(&page_handle).unwrap();
        let xobjects = ObjectHandle::dictionary(vec![(
            b"/Im0".to_vec(),
            pdf.get_object_handle(ObjectRef::new(99, 0)),
        )]);
        let resources = ObjectHandle::dictionary(vec![(b"/XObject".to_vec(), xobjects)]);
        page_handle.replace_key(b"/Resources", resources).unwrap();
        page_handle
            .replace_key(
                b"/Contents",
                ObjectHandle::dictionary(vec![(b"/Marker".to_vec(), ObjectHandle::integer(1))]),
            )
            .unwrap();
        pdf.mark_object_handle_dirty(&page_handle).unwrap();
        pdf.replace_object_handle(
            ObjectRef::new(99, 0),
            ObjectHandle::stream(
                ObjectHandle::dictionary(Vec::new()),
                std::rc::Rc::new(b"image".to_vec()),
            ),
        )
        .unwrap();

        let plan = Plan::build(&mut pdf).expect("XObject plan");

        assert!(plan
            .items
            .iter()
            .any(|item| matches!(item, Item::Synthetic { .. })));
    }

    #[test]
    fn builder_page_xobjects_handles_missing_and_non_dictionary_resources() {
        let mut pdf = fixture_pdf();
        let page = ObjectHandle::dictionary(Vec::new());
        let mut current = builder(&mut pdf);
        assert!(current.page_xobjects(&page).unwrap().is_none());

        page.replace_key(b"/Resources", ObjectHandle::integer(1))
            .unwrap();
        assert!(current.page_xobjects(&page).unwrap().is_none());

        page.replace_key(b"/Resources", ObjectHandle::dictionary(Vec::new()))
            .unwrap();
        assert!(current.page_xobjects(&page).unwrap().is_none());
    }

    #[test]
    fn resolve_terminal_turns_a_reference_cycle_into_null() {
        let mut pdf = fixture_pdf();
        let cycle = ObjectRef::new(99, 0);
        pdf.replace_object_handle(
            cycle,
            ObjectHandle::from_value(crate::object_handle::ObjectValue::Reference(cycle)),
        )
        .unwrap();
        let cycle_handle = pdf.get_object_handle(cycle);
        let mut current = builder(&mut pdf);

        assert!(current.resolve_terminal(cycle_handle).unwrap().is_null());
    }
}

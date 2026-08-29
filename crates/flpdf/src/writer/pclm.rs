//! qpdf correspondence: `QPDFWriter::enqueueObjectsPCLm`.
//!
//! PCLm does not use the ordinary Catalog-first queue. qpdf reserves output
//! numbers as it enqueues page objects, their contents, image strips, and the
//! synthetic image-transform streams. The queue then continues with references
//! discovered while serializing those objects and ends with the Catalog. The
//! direct/indirect root split follows `QPDFWriter.cc:328-333,1160-1236,
//! 2068-2076,2928-2954` and the `qpdf/qtest/pclm.test` test-driver contract.

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Seek};

use crate::writer::rewrite_renumber::{collect_canonical_enqueue_refs, visible_raw_dict_entries};
use crate::{Object, ObjectHandle, ObjectRef, Pdf, Result};

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
    /// Remapped Catalog identity when the source `/Root` is indirect.
    pub(crate) root: Option<ObjectRef>,
    /// Canonical Catalog handle when the source `/Root` is direct.
    pub(crate) direct_root: Option<ObjectHandle>,
}

impl Plan {
    pub(crate) fn build<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Self> {
        let root_ref = pdf.root_ref();
        let direct_root = if root_ref.is_none() {
            let root_candidate = pdf.trailer_key_handle(b"Root");
            if root_candidate.is_null() {
                return Err(crate::Error::Missing("/Root"));
            }
            Some(pdf.root_handle()?)
        } else {
            None
        };
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
                continue; // cov:ignore: page_refs yields only dictionary /Type /Page leaves
            };

            if let Some(contents) = page_dict.get("Contents") {
                builder.enqueue_value(contents)?;
            } // cov:ignore: LLVM maps the executed page-contents continuation to the call line above

            if let Some(xobjects) = builder.page_xobjects(page_dict)? {
                for (_, image) in xobjects.iter() {
                    builder.enqueue_value(image)?;
                    builder.enqueue_synthetic();
                }
            }
        }

        if let Some(root) = root_ref {
            builder.enqueue_reference(root);
        } else if let Some(root) = direct_root.as_ref() {
            builder.enqueue_handle_with_stream_length_policy(root)?; // cov:ignore: direct-root enqueue is exercised by PCLm integration tests; LLVM maps this continuation to the call setup.
        } // cov:ignore: direct-root enqueue executes above; LLVM places this branch-exit counter on an uninstrumented continuation line.

        let mut cursor = 0;
        while cursor < builder.items.len() {
            let item = builder.items[cursor].clone();
            cursor += 1;
            let Item::Source { source, .. } = item else {
                continue;
            };
            let source_handle = builder.pdf.get_object_handle(source);
            builder.pdf.resolve(&source_handle)?;
            let object = source_handle.materialize()?;
            builder.enqueue_value_with_stream_length_policy(&object)?;
        }

        let root = root_ref.and_then(|root| builder.old_to_new.get(&root).copied());
        if root_ref.is_some() && root.is_none() {
            return Err(crate::Error::Missing("/Root")); // cov:ignore: enqueue_reference always inserts an indirect PCLm root before the plan map is read.
        }
        Ok(Self {
            items: builder.items,
            old_to_new: builder.old_to_new,
            root,
            direct_root,
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

    /// Enqueue the indirect descendants of a direct Catalog through the live
    /// handle graph. qpdf's `enqueueObject` recurses through a direct
    /// dictionary without assigning it an object number; the canonical
    /// collector supplies the same key/array order and skips a stream's
    /// output-owned `/Length` edge.
    fn enqueue_handle_with_stream_length_policy(&mut self, value: &ObjectHandle) -> Result<()> {
        let mut references = Vec::new();
        collect_canonical_enqueue_refs(self.pdf, value, 0, true, &mut references)?;
        for reference in references {
            self.enqueue_reference(reference);
        }
        Ok(())
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
                for (_, value) in visible_raw_dict_entries(self.pdf, dict, skip_length)? {
                    self.enqueue_value_with_policy(&value, skip_length)?;
                }
            }
            Object::Stream(stream) => {
                for (_, value) in visible_raw_dict_entries(self.pdf, &stream.dict, true)? {
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
            let handle = self.pdf.get_object_handle(reference);
            self.pdf.resolve(&handle)?;
            current = handle.materialize()?;
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

    #[test]
    fn plan_rejects_a_missing_root() {
        let mut pdf = Pdf::open(Cursor::new(
            b"%PDF-1.3\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 1 >>\n\
              startxref\n9\n%%EOF\n"
                .to_vec(),
        ))
        .expect("rootless fixture must open");

        let error = Plan::build(&mut pdf).expect_err("PCLm requires a trailer /Root");
        assert!(matches!(error, crate::Error::Missing("/Root")));
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
    fn plan_skips_a_page_that_resolves_to_a_non_dictionary() {
        let mut pdf = fixture_pdf();
        let page = crate::pages::page_refs(&mut pdf).unwrap()[0];
        pdf.set_object(page, Object::Integer(42));

        let plan = Plan::build(&mut pdf).expect("a scalar page is ignored by the PCLm planner");

        assert!(plan
            .items
            .iter()
            .any(|item| { matches!(item, Item::Source { source, .. } if *source == page) }));
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
        let mut page_dict = page_handle.materialize().unwrap().into_dict().unwrap();
        let mut contents = crate::Dictionary::new();
        contents.insert("Broken", Object::Reference(ObjectRef::new(11, 0)));
        page_dict.insert("Contents", Object::Dictionary(contents));
        pdf.set_object(page, Object::Dictionary(page_dict));
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
        let mut page_dict = page_handle.materialize().unwrap().into_dict().unwrap();
        let mut xobjects = crate::Dictionary::new();
        xobjects.insert("Im0", Object::Reference(ObjectRef::new(99, 0)));
        let mut resources = crate::Dictionary::new();
        resources.insert("XObject", Object::Dictionary(xobjects));
        page_dict.insert("Resources", Object::Dictionary(resources));
        let mut contents = crate::Dictionary::new();
        contents.insert("Marker", Object::Integer(1));
        page_dict.insert("Contents", Object::Dictionary(contents));
        pdf.set_object(page, Object::Dictionary(page_dict));
        pdf.set_object(
            ObjectRef::new(99, 0),
            Object::Stream(crate::Stream::new(
                crate::Dictionary::new(),
                b"image".to_vec(),
            )),
        );

        let plan = Plan::build(&mut pdf).expect("XObject plan");

        assert!(plan
            .items
            .iter()
            .any(|item| matches!(item, Item::Synthetic { .. })));
    }

    #[test]
    fn builder_page_xobjects_handles_missing_and_non_dictionary_resources() {
        let mut pdf = fixture_pdf();
        let mut page = crate::Dictionary::new();
        let mut current = builder(&mut pdf);
        assert!(current.page_xobjects(&page).unwrap().is_none());

        page.insert("Resources", Object::Integer(1));
        assert!(current.page_xobjects(&page).unwrap().is_none());

        page.insert("Resources", Object::Dictionary(crate::Dictionary::new()));
        assert!(current.page_xobjects(&page).unwrap().is_none());
    }

    #[test]
    fn resolve_terminal_turns_a_reference_cycle_into_null() {
        let mut pdf = fixture_pdf();
        let cycle = ObjectRef::new(99, 0);
        pdf.set_object(cycle, Object::Reference(cycle));
        let mut current = builder(&mut pdf);

        assert_eq!(
            current.resolve_terminal(Object::Reference(cycle)).unwrap(),
            Object::Null
        );
    }
}

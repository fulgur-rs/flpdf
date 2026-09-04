//! qpdf correspondence: `QPDFWriter::enqueueObjectsPCLm`.
//!
//! PCLm does not use the ordinary Catalog-first queue. qpdf reserves output
//! numbers as it enqueues page objects, their contents, image strips, and the
//! synthetic image-transform streams. The queue then continues with references
//! discovered while serializing those objects and ends with the Catalog. The
//! direct/indirect root split follows `QPDFWriter.cc:328-333,1160-1236,
//! 2068-2076,2928-2954` and the `qpdf/qtest/pclm.test` test-driver contract.

use std::collections::HashMap;
use std::io::{Read, Seek};

use crate::writer::rewrite_renumber::{collect_canonical_children, collect_canonical_enqueue_refs};
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
    /// Remapped Catalog identity when the source `/Root` is indirect.
    pub(crate) root: Option<ObjectRef>,
    /// Canonical Catalog handle when the source `/Root` is direct.
    pub(crate) direct_root: Option<ObjectHandle>,
}

impl Plan {
    pub(crate) fn build<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Self> {
        let root_candidate = pdf.trailer_key_handle(b"Root");
        if root_candidate.is_null() {
            return Err(crate::Error::Missing("/Root"));
        }
        let root_handle = pdf.root_handle()?;
        let root_ref = root_handle.object_ref();
        let direct_root = root_ref.is_none().then_some(root_handle);
        let mut builder = Builder {
            pdf,
            items: Vec::new(),
            old_to_new: HashMap::new(),
            next_output: 1,
        };

        for page in crate::pages::page_refs(builder.pdf)? {
            builder.enqueue_reference(page);

            let page_handle = builder.pdf.get_object_handle(page);
            builder.pdf.resolve(&page_handle)?;
            if page_handle.try_as_dictionary()?.is_none() {
                continue; // cov:ignore: page_refs yields only dictionary /Type /Page leaves
            }

            let contents = page_handle.try_get_key(b"/Contents")?;
            if !contents.is_null() {
                builder.enqueue_handle_with_stream_length_policy(&contents)?;
            }

            for image in builder.page_xobjects(&page_handle)? {
                builder.enqueue_handle_with_stream_length_policy(&image)?;
                builder.enqueue_synthetic();
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
            let mut references = Vec::new();
            collect_canonical_children(builder.pdf, &source_handle, 0, true, &mut references)?;
            for reference in references {
                builder.enqueue_reference(reference);
            }
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

    fn page_xobjects(&mut self, page: &ObjectHandle) -> Result<Vec<ObjectHandle>> {
        let resources = page.try_get_key(b"/Resources")?;
        let xobjects = resources.try_get_key(b"/XObject")?;
        xobjects
            .try_get_keys()?
            .into_iter()
            .map(|key| xobjects.try_get_key(&key))
            .collect()
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

    fn one_page_fixture_pdf() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(
            include_bytes!("../../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        ))
        .expect("one-page fixture must open")
    }

    fn one_page_fixture_with_unplanned_trailer_refs() -> Pdf<Cursor<Vec<u8>>> {
        let mut pdf = one_page_fixture_pdf();
        let probe = pdf
            .make_indirect_from_object_handle(ObjectHandle::string(b"unreachable".to_vec()))
            .expect("allocate the trailer-only object");
        assert_eq!(probe.object_ref(), Some(ObjectRef::new(8, 0)));
        pdf.trailer()
            .replace_key(b"/Probe", probe.clone())
            .expect("add a live trailer reference");
        pdf.trailer()
            .replace_key(b"/ProbeAgain", probe)
            .expect("add a repeated live trailer reference");
        pdf
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

    #[test]
    fn plan_skips_a_page_that_resolves_to_a_non_dictionary() {
        let mut pdf = fixture_pdf();
        let page = crate::pages::page_refs(&mut pdf).unwrap()[0];
        pdf.replace_object(page, ObjectHandle::integer(42))
            .expect("replace page with a scalar through the canonical route");

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
        let replacement = page_handle
            .shallow_copy()
            .expect("page dictionary must be shallow-copyable");
        let contents = ObjectHandle::dictionary(vec![(
            b"/Broken".to_vec(),
            pdf.get_object_handle(ObjectRef::new(11, 0)),
        )]);
        replacement
            .replace_key(b"/Contents", contents)
            .expect("replace page contents through the canonical route");
        pdf.replace_object(page, replacement)
            .expect("replace page through the canonical route");
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
        let replacement = page_handle
            .shallow_copy()
            .expect("page dictionary must be shallow-copyable");
        let image = pdf
            .new_stream_with_data(std::rc::Rc::new(b"image".to_vec()))
            .expect("create image stream through the canonical route");
        let xobjects = ObjectHandle::dictionary(vec![(b"/Im0".to_vec(), image)]);
        let resources = ObjectHandle::dictionary(vec![(b"/XObject".to_vec(), xobjects)]);
        let contents =
            ObjectHandle::dictionary(vec![(b"/Marker".to_vec(), ObjectHandle::integer(1))]);
        replacement
            .replace_key(b"/Resources", resources)
            .expect("replace page resources through the canonical route");
        replacement
            .replace_key(b"/Contents", contents)
            .expect("replace page contents through the canonical route");
        pdf.replace_object(page, replacement)
            .expect("replace page through the canonical route");

        let plan = Plan::build(&mut pdf).expect("XObject plan");

        assert!(plan
            .items
            .iter()
            .any(|item| matches!(item, Item::Synthetic { .. })));
    }

    #[test]
    fn writer_emits_unplanned_trailer_refs_with_qpdf_late_numbers() {
        let mut pdf = one_page_fixture_with_unplanned_trailer_refs();

        let options = crate::writer::WriterOptions {
            pclm: true,
            deterministic_id: true,
            ..crate::writer::WriterOptions::default()
        };
        let mut output = Vec::new();
        let result = crate::writer::write_pclm(&mut pdf, &mut output, &options);

        assert!(result.is_ok(), "qpdf-compatible PCLm output: {result:?}");
        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("xref\n0 7\n"));
        assert!(output.contains("/Info 7 0 R"));
        assert!(output.contains("/Probe 8 0 R"));
        assert!(output.contains("/ProbeAgain 8 0 R"));
        assert!(!output.contains("7 0 obj\n"));
        assert!(!output.contains("8 0 obj\n"));
    }

    #[test]
    fn writer_emits_unplanned_trailer_refs_with_generated_id() {
        let mut pdf = one_page_fixture_with_unplanned_trailer_refs();
        let options = crate::writer::WriterOptions {
            pclm: true,
            ..crate::writer::WriterOptions::default()
        };
        let mut output = Vec::new();
        let result = crate::writer::write_pclm(&mut pdf, &mut output, &options);

        assert!(result.is_ok(), "qpdf-compatible PCLm output: {result:?}");
        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("/Info 7 0 R"));
        assert!(output.contains("/Probe 8 0 R"));
        assert!(output.contains("/ProbeAgain 8 0 R"));
        assert!(!output.contains("7 0 obj\n"));
        assert!(!output.contains("8 0 obj\n"));
    }
}

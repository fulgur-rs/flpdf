//! qpdf correspondence: `QPDFWriter::enqueueObjectsPCLm`.
//!
//! PCLm does not use the ordinary Catalog-first queue. qpdf reserves output
//! numbers as it enqueues page objects, their contents, image strips, and the
//! synthetic image-transform streams, then the Catalog. References discovered
//! while serializing those initial objects are appended to the live queue. The
//! direct/indirect root split follows `QPDFWriter.cc:328-333,1160-1236,
//! 2068-2076,2928-2954` and the `qpdf/qtest/pclm.test` test-driver contract.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{Read, Seek};
use std::rc::Rc;

use crate::writer::rewrite_renumber::collect_canonical_enqueue_refs;
use crate::{ObjectHandle, ObjectRef, Pdf, Result};

/// Return qpdf's initial PCLm queue seeds in enqueue order. Only the
/// page/Contents/strip/synthetic/root seed differs from standard output; all
/// descendants are discovered by the shared live writer while those objects
/// are emitted (`QPDFWriter.cc:2928-2954`).
pub(crate) fn seed_handles<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Vec<ObjectHandle>> {
    let mut seeds = Vec::new();
    for page in crate::pages::page_refs(pdf)? {
        seeds.push(pdf.get_object_handle(page));

        let page_handle = pdf.get_object_handle(page);
        page_handle.try_dereference()?;
        let contents = page_handle.try_get_key(b"/Contents")?;
        if !contents.try_is_null()? {
            let mut references = Vec::new();
            collect_canonical_enqueue_refs(pdf, &contents, 0, true, &mut references)?;
            for reference in references {
                seeds.push(pdf.get_object_handle(reference));
            }
        }

        let resources = page_handle.try_get_key(b"/Resources")?;
        let xobjects = resources.try_get_key(b"/XObject")?;
        for key in xobjects.try_get_keys()? {
            let image = xobjects.try_get_key(&key)?;
            let mut references = Vec::new();
            collect_canonical_enqueue_refs(pdf, &image, 0, true, &mut references)?;
            for reference in references {
                seeds.push(pdf.get_object_handle(reference));
            }
            seeds.push(pdf.new_stream_with_data(Rc::new(b"q /image Do Q\n".to_vec()))?);
        }
    }

    let root = pdf.root_handle()?;
    let mut references = Vec::new();
    collect_canonical_enqueue_refs(pdf, &root, 0, true, &mut references)?;
    for reference in references {
        seeds.push(pdf.get_object_handle(reference));
    }
    Ok(seeds)
}

#[cfg(test)]
use crate::writer::rewrite_renumber::collect_canonical_children;

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
    /// Remapped Catalog identity when the source `/Root` is indirect.
    pub(crate) root: Option<ObjectRef>,
    /// Canonical Catalog handle when the source `/Root` is direct.
    pub(crate) direct_root: Option<ObjectHandle>,
}

impl Plan {
    pub(crate) fn build<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Self> {
        let root_candidate = pdf.trailer_key_handle(b"Root");
        // Only a direct null or an absent /Root short-circuits here. An
        // indirect value must reach `root_handle()`, whose dictionary gate is
        // qpdf's: `QPDF::getRoot` tests `root.isDictionary()`
        // (`libqpdf/QPDF.cc:2355-2360`), which dereferences first
        // (`QPDFObjectHandle.cc:432-435`), so an indirect reference resolving to
        // null reports "unable to find /Root dictionary" rather than a missing
        // key.
        if !root_candidate.is_indirect() && root_candidate.try_is_null()? {
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
            let _ = builder.enqueue_reference(page);

            let page_handle = builder.pdf.get_object_handle(page);
            page_handle.try_dereference()?;
            if !page_handle.try_is_dictionary()? {
                continue; // cov:ignore: page_refs yields only dictionary /Type /Page leaves
            }

            let contents = page_handle.try_get_key(b"/Contents")?;
            if !contents.try_is_null()? {
                builder.enqueue_handle_with_stream_length_policy(&contents)?;
            }

            for image in builder.page_xobjects(&page_handle)? {
                builder.enqueue_handle_with_stream_length_policy(&image)?;
                builder.enqueue_synthetic();
            }
        }

        let root = if let Some(root) = root_ref {
            builder.enqueue_reference(root)
        } else if let Some(root) = direct_root.as_ref() {
            builder.enqueue_handle_with_stream_length_policy(root)?; // cov:ignore: direct-root enqueue is exercised by PCLm integration tests; LLVM maps this continuation to the call setup.
            None
        } else {
            None
        }; // cov:ignore: direct-root enqueue executes above; LLVM places this branch-exit counter on an uninstrumented continuation line.
        if root_ref.is_some() && root.is_none() {
            return Err(crate::Error::Missing("/Root")); // cov:ignore: enqueue_reference always inserts an indirect PCLm root before the plan map is read.
        }
        Ok(Self {
            items: builder.items,
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
    fn enqueue_reference(&mut self, source: ObjectRef) -> Option<ObjectRef> {
        if source.number == 0 || self.old_to_new.contains_key(&source) {
            return self.old_to_new.get(&source).copied();
        }
        let output = ObjectRef::new(self.next_output, 0);
        self.next_output = self.next_output.saturating_add(1);
        self.old_to_new.insert(source, output);
        self.items.push(Item::Source { source, output });
        Some(output)
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
            let _ = self.enqueue_reference(reference);
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

/// qpdf's mutable `object_queue` after the PCLm-only initial enqueue pass.
///
/// The plan above reserves only page/content/image/synthetic/root numbers.
/// Indirect children encountered while serializing those values are appended
/// here, at the same `unparseChild` boundary that grows qpdf's queue.
pub(crate) struct EmissionQueue {
    pending: VecDeque<Item>,
    old_to_new: BTreeMap<ObjectRef, ObjectRef>,
    next_output: u32,
}

impl EmissionQueue {
    pub(crate) fn from_plan(plan: &Plan) -> Result<Self> {
        let mut old_to_new = BTreeMap::new();
        let mut next_output = 1_u32;
        for item in &plan.items {
            let output = match *item {
                Item::Source { source, output } => {
                    old_to_new.insert(source, output);
                    output
                }
                Item::Synthetic { output } => output,
            };
            next_output = output.number.checked_add(1).ok_or_else(|| {
                crate::Error::Unsupported("PCLm object number overflows u32".to_string())
            })?;
        }
        Ok(Self {
            pending: plan.items.iter().cloned().collect(),
            old_to_new,
            next_output,
        })
    }

    pub(crate) fn pop(&mut self) -> Option<Item> {
        self.pending.pop_front()
    }

    pub(crate) fn enqueue_handle<R: Read + Seek>(
        &mut self,
        pdf: &Pdf<R>,
        handle: ObjectHandle,
    ) -> Result<ObjectRef> {
        if handle.owning_pdf_unique_id() != Some(pdf.unique_id()) {
            return Err(crate::Error::Internal(
                "QPDFObjectHandle from different QPDF found while writing.  Use QPDF::copyForeignObject to add objects from another file."
                    .to_string(),
            ));
        }
        let source = handle.object_ref().ok_or_else(|| {
            crate::Error::Internal("PCLm dynamic child has no indirect identity".to_string())
        })?;
        if let Some(output) = self.old_to_new.get(&source).copied() {
            return Ok(output);
        }
        let output = ObjectRef::new(self.next_output, 0);
        self.next_output = self.next_output.checked_add(1).ok_or_else(|| {
            crate::Error::Unsupported("PCLm object number overflows u32".to_string())
        })?;
        self.old_to_new.insert(source, output);
        self.pending.push_back(Item::Source { source, output });
        Ok(output)
    }

    pub(crate) fn object_count(&self) -> Result<usize> {
        usize::try_from(self.next_output).map_err(|_| {
            crate::Error::Unsupported("PCLm object count does not fit in usize".to_string())
        })
    }

    pub(crate) fn into_old_to_new(self) -> BTreeMap<ObjectRef, ObjectRef> {
        self.old_to_new
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{Pipeline, PipelineError, PipelineResult};
    use crate::token_filter::{TokenFilter, TokenFilterOutput};
    use crate::tokenizer::Token;
    use crate::{Error, PdfWriter, StreamDataProvider};
    use std::cell::RefCell;
    use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
    use std::path::PathBuf;
    use std::process::Command;
    use std::rc::Rc;

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

    #[derive(Clone)]
    struct Events(Rc<RefCell<Vec<&'static str>>>);

    impl Events {
        fn new() -> Self {
            Self(Rc::new(RefCell::new(Vec::new())))
        }

        fn record(&self, event: &'static str) {
            self.0.borrow_mut().push(event);
        }

        fn snapshot(&self) -> Vec<&'static str> {
            self.0.borrow().clone()
        }
    }

    struct EventProvider {
        event: &'static str,
        payload: &'static [u8],
        events: Events,
    }

    struct SuccessfulUnfilteredRetryProvider {
        calls: Rc<RefCell<Vec<(bool, bool)>>>,
    }

    struct PassThroughFilter;

    impl TokenFilter for PassThroughFilter {
        fn handle_token(
            &mut self,
            token: &Token,
            output: &mut TokenFilterOutput<'_>,
        ) -> PipelineResult<()> {
            output.write_token(token)
        }
    }

    impl StreamDataProvider for SuccessfulUnfilteredRetryProvider {
        fn supports_retry(&self) -> bool {
            true
        }

        fn provide_stream_data_with_retry_by_id(
            &self,
            _object_number: u32,
            _generation: u16,
            pipeline: &mut dyn Pipeline,
            suppress_warnings: bool,
            will_retry: bool,
        ) -> crate::Result<bool> {
            self.calls
                .borrow_mut()
                .push((suppress_warnings, will_retry));
            pipeline
                .write(b"PCLm-unfiltered-success")
                .map_err(Error::from)?;
            pipeline.finish().map_err(Error::from)?;
            Ok(true)
        }
    }

    impl StreamDataProvider for EventProvider {
        fn provide_stream_data_by_id(
            &self,
            _object_number: u32,
            _generation: u16,
            pipeline: &mut dyn Pipeline,
        ) -> crate::Result<()> {
            self.events.record(self.event);
            pipeline.write(self.payload).map_err(Error::from)?;
            pipeline.finish().map_err(Error::from)
        }
    }

    struct SegmentFailureProvider {
        events: Events,
    }

    struct SegmentFailure<'a> {
        next: &'a mut dyn Pipeline,
    }

    impl Pipeline for SegmentFailure<'_> {
        fn identifier(&self) -> &str {
            "PCLm provider segment"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.next.write(data)
        }

        fn finish(&mut self) -> PipelineResult<()> {
            Err(PipelineError::runtime(
                "PCLm provider segment finish failure",
            ))
        }
    }

    impl StreamDataProvider for SegmentFailureProvider {
        fn provide_stream_data_by_id(
            &self,
            _object_number: u32,
            _generation: u16,
            pipeline: &mut dyn Pipeline,
        ) -> crate::Result<()> {
            self.events.record("provider:A");
            let mut segment = SegmentFailure { next: pipeline };
            segment.write(b"PCLm-provider-A").map_err(Error::from)?;
            segment.finish().map_err(Error::from)
        }
    }

    struct EventWriter {
        events: Events,
        bytes: Rc<RefCell<Vec<u8>>>,
    }

    impl Write for EventWriter {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.events.record("sink-write");
            self.bytes.borrow_mut().extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct PayloadRejectingWriter {
        events: Events,
    }

    impl Write for PayloadRejectingWriter {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            if data
                .windows(b"PCLm-provider-A".len())
                .any(|window| window == b"PCLm-provider-A")
            {
                self.events.record("sink-error:A");
                return Err(io::Error::other("PCLm sink rejected provider A"));
            }
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct FinishRejectingPipeline {
        events: Events,
    }

    impl Pipeline for FinishRejectingPipeline {
        fn identifier(&self) -> &str {
            "PCLm final sink"
        }

        fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.events.record("sink-finish-error");
            Err(PipelineError::runtime("PCLm final sink finish failure"))
        }
    }

    fn provider_stream(
        pdf: &Pdf<Cursor<Vec<u8>>>,
        provider: impl StreamDataProvider + 'static,
    ) -> ObjectHandle {
        let stream = pdf.new_stream().expect("create PCLm provider stream");
        stream
            .replace_stream_data_provider(Rc::new(provider), None, None)
            .expect("register PCLm provider");
        stream
    }

    fn attach_root_streams(
        pdf: &mut Pdf<Cursor<Vec<u8>>>,
        first: ObjectHandle,
        second: ObjectHandle,
    ) {
        let root = pdf.root_handle().expect("resolve PCLm Catalog");
        root.replace_key(b"/PclmProviderA", first)
            .expect("attach first PCLm provider");
        root.replace_key(b"/PclmProviderB", second)
            .expect("attach second PCLm provider");
    }

    fn configure_pclm_writer<'pdf>(
        pdf: &'pdf mut Pdf<Cursor<Vec<u8>>>,
    ) -> PdfWriter<'pdf, Cursor<Vec<u8>>> {
        let mut writer = PdfWriter::new(pdf);
        writer.set_pclm(true);
        writer.set_static_id(true);
        writer
    }

    fn exact_qpdf_11_9() -> bool {
        Command::new("qpdf")
            .arg("--version")
            .output()
            .ok()
            .and_then(|output| {
                String::from_utf8(output.stdout)
                    .ok()
                    .and_then(|stdout| stdout.lines().next().map(str::to_owned))
            })
            .is_some_and(|line| line == "qpdf version 11.9.0")
    }

    fn pinned_qpdf_source() -> Option<PathBuf> {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let output = Command::new(workspace.join("scripts/fetch-qpdf-source.sh"))
            .arg("--print-path")
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8(output.stdout).ok()?;
        Some(PathBuf::from(path.trim()))
    }

    fn first_page_content(path: &std::path::Path) -> Vec<u8> {
        let pages = Command::new("qpdf")
            .arg("--show-pages")
            .arg(path)
            .output()
            .expect("inspect page content references");
        assert!(
            pages.status.success(),
            "qpdf --show-pages failed: {}",
            String::from_utf8_lossy(&pages.stderr)
        );
        let pages = String::from_utf8(pages.stdout).expect("qpdf page listing is UTF-8");
        let content = pages
            .split_once("  content:\n")
            .and_then(|(_, rest)| rest.lines().next())
            .expect("qpdf page listing has a content stream");
        let object = content
            .split_whitespace()
            .next()
            .expect("content stream has an object number");
        let stream = Command::new("qpdf")
            .arg(format!("--show-object={object}"))
            .arg("--raw-stream-data")
            .arg(path)
            .output()
            .expect("read qpdf content stream");
        assert!(
            stream.status.success(),
            "qpdf --show-object failed: {}",
            String::from_utf8_lossy(&stream.stderr)
        );
        stream.stdout
    }

    #[test]
    fn plan_retains_only_qpdfs_initial_pclm_enqueue_order() {
        let mut pdf = one_page_fixture_pdf();
        let child = pdf
            .make_indirect_from_object_handle(ObjectHandle::string(b"late-child".to_vec()))
            .expect("allocate a Catalog child");
        let child_ref = child.object_ref().expect("indirect child identity");
        pdf.root_handle()
            .expect("resolve Catalog")
            .replace_key(b"/LateChild", child)
            .expect("attach Catalog child");

        let plan = Plan::build(&mut pdf).expect("build initial PCLm enqueue plan");

        assert!(
            !plan
                .items
                .iter()
                .any(|item| matches!(item, Item::Source { source, .. } if *source == child_ref)),
            "a Catalog child belongs to emission-time discovery, not the initial PCLm plan"
        );
    }

    #[test]
    fn pclm_reaches_the_final_sink_before_requesting_the_next_provider() {
        let events = Events::new();
        let bytes = Rc::new(RefCell::new(Vec::new()));
        let mut pdf = one_page_fixture_pdf();
        let first = provider_stream(
            &pdf,
            EventProvider {
                event: "provider:A",
                payload: b"PCLm-provider-A",
                events: events.clone(),
            },
        );
        let second = provider_stream(
            &pdf,
            EventProvider {
                event: "provider:B",
                payload: b"PCLm-provider-B",
                events: events.clone(),
            },
        );
        attach_root_streams(&mut pdf, first, second);

        let mut writer = configure_pclm_writer(&mut pdf);
        writer
            .set_output_writer(EventWriter {
                events: events.clone(),
                bytes: Rc::clone(&bytes),
            })
            .expect("install PCLm event sink");
        writer.write().expect("PCLm writer succeeds");

        assert!(!bytes.borrow().is_empty());
        let events = events.snapshot();
        let provider_a = events
            .iter()
            .position(|event| *event == "provider:A")
            .expect("provider A event");
        let provider_b = events
            .iter()
            .position(|event| *event == "provider:B")
            .expect("provider B event");
        assert!(
            events[provider_a + 1..provider_b].contains(&"sink-write"),
            "PCLm must emit A before requesting B; events: {events:?}"
        );
    }

    #[test]
    fn pclm_forced_uncompressed_successful_first_call_retries_on_filter_result() {
        let mut pdf = one_page_fixture_pdf();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let stream = provider_stream(
            &pdf,
            SuccessfulUnfilteredRetryProvider {
                calls: Rc::clone(&calls),
            },
        );
        stream
            .add_token_filter(Rc::new(RefCell::new(PassThroughFilter)))
            .expect("mark the provider stream as modified");
        pdf.root_handle()
            .expect("resolve Catalog")
            .replace_key(b"/PclmProvider", stream)
            .expect("attach PCLm provider");

        let mut writer = configure_pclm_writer(&mut pdf);
        writer.set_output_memory().expect("install memory output");
        writer
            .write()
            .expect("PCLm raw retry after an unfiltered success");

        assert_eq!(*calls.borrow(), vec![(false, true), (false, false)]);
    }

    #[test]
    fn pclm_progress_failure_happens_before_current_object_bytes_reach_sink() {
        let events = Events::new();
        let bytes = Rc::new(RefCell::new(Vec::new()));
        let mut pdf = one_page_fixture_pdf();
        let mut writer = configure_pclm_writer(&mut pdf);
        writer
            .set_output_writer(EventWriter {
                events,
                bytes: Rc::clone(&bytes),
            })
            .expect("install PCLm event sink");
        writer.register_progress_reporter(Box::new(|percent| {
            assert_eq!(percent, 0);
            Err(Error::System("PCLm progress failure".to_owned()))
        }));

        let error = writer
            .write()
            .expect_err("PCLm progress failure must escape before object output");

        assert!(error.to_string().contains("PCLm progress failure"));
        let bytes = bytes.borrow();
        assert!(bytes.starts_with(b"%PDF-"));
        assert!(
            !bytes
                .windows(b"1 0 obj\n".len())
                .any(|window| window == b"1 0 obj\n"),
            "progress failure must not leave the current PCLm object in the sink"
        );
    }

    #[test]
    fn pclm_content_normalization_matches_qpdf_11_9() {
        if !exact_qpdf_11_9() {
            eprintln!("qpdf 11.9.0 is unavailable; skipping PCLm normalization oracle");
            return;
        }
        let Some(source) = pinned_qpdf_source() else {
            eprintln!("pinned qpdf source is unavailable; skipping PCLm normalization oracle");
            return;
        };
        let input = source.join("qpdf/qtest/qpdf/good7.pdf");
        let temporary = tempfile::tempdir().expect("PCLm normalization tempdir");
        let normalized = temporary.path().join("qpdf-normalized.pdf");
        let qpdf = Command::new("qpdf")
            .args([
                "--normalize-content=y",
                "--stream-data=uncompress",
                "--object-streams=disable",
                "--static-id",
            ])
            .arg(&input)
            .arg(&normalized)
            .output()
            .expect("run qpdf content normalization");
        assert!(
            qpdf.status.success(),
            "qpdf normalization failed: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );

        let mut pdf = Pdf::open(Cursor::new(
            std::fs::read(&input).expect("read qpdf normalization input"),
        ))
        .expect("open qpdf normalization input");
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_pclm(true);
        writer.set_content_normalization(true);
        writer.set_static_id(true);
        writer
            .set_output_memory()
            .expect("install PCLm memory output");
        writer.write().expect("write normalized PCLm output");
        let actual = writer.get_buffer().expect("read normalized PCLm output");
        let actual_path = temporary.path().join("pclm-normalized.pdf");
        std::fs::write(&actual_path, actual).expect("write PCLm normalization output");

        assert_eq!(
            first_page_content(&actual_path),
            first_page_content(&normalized),
            "PCLm page content normalization must match qpdf 11.9.0"
        );
    }

    #[test]
    fn pclm_filtering_disabled_false_uses_the_first_retryable_attempt_once() {
        let mut pdf = one_page_fixture_pdf();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let calls_for_provider = Rc::clone(&calls);
        let stream = pdf.new_stream().expect("create retry-aware PCLm stream");
        stream
            .replace_stream_data_with_retry_callback(
                move |pipeline, suppress_warnings, will_retry| {
                    calls_for_provider
                        .borrow_mut()
                        .push((suppress_warnings, will_retry));
                    pipeline
                        .write(b"PCLm-disabled-filter")
                        .map_err(Error::from)?;
                    pipeline.finish().map_err(Error::from)?;
                    Ok(false)
                },
                None,
                None,
            )
            .expect("register retry-aware PCLm stream");
        stream
            .set_filter_on_write(false)
            .expect("disable filtering on PCLm stream");
        pdf.root_handle()
            .expect("resolve Catalog")
            .replace_key(b"/PclmProvider", stream)
            .expect("attach PCLm provider");

        let mut writer = configure_pclm_writer(&mut pdf);
        writer.set_output_memory().expect("install memory output");
        writer
            .write()
            .expect("filter-disabled false keeps the first source buffer");
        let output = writer.get_buffer().expect("PCLm memory output");

        assert_eq!(*calls.borrow(), vec![(false, true)]);
        assert!(output
            .windows(b"PCLm-disabled-filter".len())
            .any(|window| window == b"PCLm-disabled-filter"));
    }

    #[test]
    fn pclm_recoverable_filter_failure_retries_only_the_same_stream() {
        let mut pdf = one_page_fixture_pdf();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let calls_for_provider = Rc::clone(&calls);
        let stream = pdf.new_stream().expect("create retry-aware PCLm stream");
        stream
            .replace_stream_data_with_retry_callback(
                move |pipeline, suppress_warnings, will_retry| {
                    calls_for_provider
                        .borrow_mut()
                        .push((suppress_warnings, will_retry));
                    if will_retry {
                        return Ok(false);
                    }
                    pipeline.write(b"PCLm-retry-payload").map_err(Error::from)?;
                    pipeline.finish().map_err(Error::from)?;
                    Ok(true)
                },
                None,
                None,
            )
            .expect("register retry-aware PCLm stream");
        stream
            .add_token_filter(Rc::new(RefCell::new(PassThroughFilter)))
            .expect("mark the retrying stream as modified");
        pdf.root_handle()
            .expect("resolve Catalog")
            .replace_key(b"/PclmProvider", stream)
            .expect("attach PCLm provider");

        let mut writer = configure_pclm_writer(&mut pdf);
        writer.set_output_memory().expect("install memory output");
        writer.write().expect("PCLm raw retry succeeds");

        assert_eq!(*calls.borrow(), vec![(false, true), (false, false)]);
        assert!(writer
            .get_buffer()
            .expect("PCLm memory output")
            .windows(b"PCLm-retry-payload".len())
            .any(|window| window == b"PCLm-retry-payload"));
    }

    #[test]
    fn pclm_provider_segment_failure_never_requests_a_later_provider() {
        let events = Events::new();
        let mut pdf = one_page_fixture_pdf();
        let first = provider_stream(
            &pdf,
            SegmentFailureProvider {
                events: events.clone(),
            },
        );
        let second = provider_stream(
            &pdf,
            EventProvider {
                event: "provider:B",
                payload: b"PCLm-provider-B",
                events: events.clone(),
            },
        );
        attach_root_streams(&mut pdf, first, second);

        let mut writer = configure_pclm_writer(&mut pdf);
        writer.set_output_memory().expect("install memory output");
        let error = writer
            .write()
            .expect_err("PCLm provider segment failure must escape");

        assert!(error
            .to_string()
            .contains("PCLm provider segment finish failure"));
        assert_eq!(events.snapshot(), vec!["provider:A"]);
    }

    #[test]
    fn pclm_sink_write_failure_never_requests_a_later_provider() {
        let events = Events::new();
        let mut pdf = one_page_fixture_pdf();
        let first = provider_stream(
            &pdf,
            EventProvider {
                event: "provider:A",
                payload: b"PCLm-provider-A",
                events: events.clone(),
            },
        );
        let second = provider_stream(
            &pdf,
            EventProvider {
                event: "provider:B",
                payload: b"PCLm-provider-B",
                events: events.clone(),
            },
        );
        attach_root_streams(&mut pdf, first, second);

        let mut writer = configure_pclm_writer(&mut pdf);
        writer
            .set_output_writer(PayloadRejectingWriter {
                events: events.clone(),
            })
            .expect("install rejecting PCLm sink");
        let error = writer.write().expect_err("PCLm sink failure must escape");

        assert!(error.to_string().contains("PCLm sink rejected provider A"));
        let events = events.snapshot();
        assert!(events.contains(&"provider:A"));
        assert!(events.contains(&"sink-error:A"));
        assert!(
            !events.contains(&"provider:B"),
            "a terminal PCLm sink failure must stop the live queue: {events:?}"
        );
    }

    #[test]
    fn pclm_final_sink_finish_failure_never_requests_a_later_provider() {
        let events = Events::new();
        let mut pdf = one_page_fixture_pdf();
        let page = crate::pages::page_refs(&mut pdf).expect("PCLm page refs")[0];
        pdf.get_object_handle(page)
            .replace_key(b"/Contents", ObjectHandle::null())
            .expect("remove the fixture content stream");
        let first = provider_stream(
            &pdf,
            EventProvider {
                event: "provider:A",
                payload: b"PCLm-provider-A",
                events: events.clone(),
            },
        );
        let second = provider_stream(
            &pdf,
            EventProvider {
                event: "provider:B",
                payload: b"PCLm-provider-B",
                events: events.clone(),
            },
        );
        attach_root_streams(&mut pdf, first, second);

        let mut writer = configure_pclm_writer(&mut pdf);
        writer
            .set_output_pipeline(FinishRejectingPipeline {
                events: events.clone(),
            })
            .expect("install finish-rejecting PCLm pipeline");
        let error = writer
            .write()
            .expect_err("PCLm final sink finish failure must escape");

        assert!(error.to_string().contains("PCLm final sink finish failure"));
        let events = events.snapshot();
        assert!(events.contains(&"provider:A"));
        assert!(events.contains(&"sink-finish-error"));
        assert!(
            !events.contains(&"provider:B"),
            "a PCLm segment finish failure must stop the live queue: {events:?}"
        );
    }

    #[test]
    fn pclm_static_id_bytes_and_check_exit_match_qpdf_11_9() {
        if !exact_qpdf_11_9() {
            eprintln!("qpdf 11.9.0 is unavailable; skipping PCLm byte oracle");
            return;
        }
        let Some(source) = pinned_qpdf_source() else {
            eprintln!("pinned qpdf source is unavailable; skipping PCLm byte oracle");
            return;
        };
        let fixture = source.join("qpdf/qtest/qpdf/pclm-in.pdf");
        let oracle = source.join("qpdf/qtest/qpdf/pclm-out.pdf");
        let mut pdf = Pdf::open(Cursor::new(
            std::fs::read(&fixture).expect("read qpdf PCLm input"),
        ))
        .expect("open qpdf PCLm input");
        let mut writer = configure_pclm_writer(&mut pdf);
        writer.set_output_memory().expect("install memory output");
        writer.write().expect("write qpdf PCLm fixture");
        let actual = writer.get_buffer().expect("PCLm output bytes");
        let expected = std::fs::read(&oracle).expect("read qpdf 11.9.0 PCLm output");

        assert_eq!(actual, expected, "PCLm output must be byte-identical");

        let temporary = tempfile::tempdir().expect("PCLm oracle tempdir");
        let actual_path = temporary.path().join("actual.pdf");
        std::fs::write(&actual_path, &actual).expect("write PCLm output for qpdf check");
        let actual_check = Command::new("qpdf")
            .arg("--check")
            .arg(&actual_path)
            .output()
            .expect("check flpdf PCLm output");
        let oracle_check = Command::new("qpdf")
            .arg("--check")
            .arg(&oracle)
            .output()
            .expect("check qpdf PCLm output");
        assert_eq!(
            actual_check.status.code(),
            oracle_check.status.code(),
            "flpdf and qpdf PCLm outputs must have the same qpdf --check exit"
        );
        assert!(actual_check.status.success());
    }

    #[test]
    fn pclm_deterministic_id_output_is_repeatable_and_qpdf_readable() {
        fn write() -> Vec<u8> {
            let mut pdf = one_page_fixture_pdf();
            let mut writer = PdfWriter::new(&mut pdf);
            writer.set_pclm(true);
            writer.set_deterministic_id(true);
            writer.set_output_memory().expect("install memory output");
            writer.write().expect("write deterministic PCLm output");
            writer.get_buffer().expect("deterministic PCLm bytes")
        }

        let first = write();
        let second = write();
        assert_eq!(first, second, "PCLm deterministic IDs must be repeatable");

        if exact_qpdf_11_9() {
            let temporary = tempfile::tempdir().expect("deterministic PCLm tempdir");
            let output = temporary.path().join("deterministic-pclm.pdf");
            std::fs::write(&output, first).expect("write deterministic PCLm output");
            let check = Command::new("qpdf")
                .arg("--check")
                .arg(output)
                .output()
                .expect("check deterministic PCLm output");
            assert!(
                check.status.success(),
                "qpdf rejected deterministic PCLm output: {}",
                String::from_utf8_lossy(&check.stderr)
            );
        }
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
    fn plan_reports_qpdf_dictionary_error_for_an_indirect_null_root() {
        let mut bytes = b"%PDF-1.3\n".to_vec();
        let object_offset = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\nnull\nendobj\n");
        let xref_offset = bytes.len();
        bytes.extend_from_slice(
            format!("xref\n0 2\n0000000000 65535 f \n{object_offset:010} 00000 n \n").as_bytes(),
        );
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("indirect-null-root fixture must open");

        let error = Plan::build(&mut pdf).expect_err("an indirect null Root is not a dictionary");
        assert_eq!(error.to_string(), "unable to find /Root dictionary");
    }

    #[test]
    fn plan_does_not_resurrect_a_non_page_through_a_root_prewalk() {
        let mut pdf = fixture_pdf();
        let page = crate::pages::page_refs(&mut pdf).unwrap()[0];
        pdf.replace_object(page, ObjectHandle::integer(42))
            .expect("replace page with a scalar through the canonical route");

        let plan = Plan::build(&mut pdf).expect("a scalar page is ignored by the PCLm planner");

        assert!(!plan
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
        page_handle.try_is_scalar().unwrap();
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

        assert!(Plan::build(&mut pdf).is_err());
    }

    #[test]
    fn plan_enqueues_xobject_and_its_synthetic_transform() {
        let mut pdf = fixture_pdf();
        let page = crate::pages::page_refs(&mut pdf).unwrap()[0];
        let page_handle = pdf.get_object_handle(page);
        page_handle.try_is_scalar().unwrap();
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
        let result = crate::writer::output::with_buffer_sink(&mut output, |out| {
            crate::writer::write_pclm(&mut pdf, out, &options, None)
        });

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
        let result = crate::writer::output::with_buffer_sink(&mut output, |out| {
            crate::writer::write_pclm(&mut pdf, out, &options, None)
        });

        assert!(result.is_ok(), "qpdf-compatible PCLm output: {result:?}");
        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("/Info 7 0 R"));
        assert!(output.contains("/Probe 8 0 R"));
        assert!(output.contains("/ProbeAgain 8 0 R"));
        assert!(!output.contains("7 0 obj\n"));
        assert!(!output.contains("8 0 obj\n"));
    }
}

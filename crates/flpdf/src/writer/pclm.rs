//! PCLm writer behavior.
//!
//! The PCLm enqueue order itself lives with qpdf's one standard-writer queue
//! in [`crate::writer::plain::body`] (`QPDFWriter::enqueueObjectsPCLm`); this
//! module holds the PCLm-specific regression coverage for the route that
//! queue drives.
//!
//! qpdf correspondence: `QPDFWriter::enqueueObjectsPCLm`.

#[cfg(test)]
mod tests {
    use crate::pipeline::{Pipeline, PipelineError, PipelineResult};
    use crate::token_filter::{TokenFilter, TokenFilterOutput};
    use crate::tokenizer::{Token, TokenType};
    use crate::{Error, ObjectHandle, ObjectRef, Pdf, PdfWriter, Result, StreamDataProvider};
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
            assert_eq!(segment.identifier(), "PCLm provider segment");
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

    #[test]
    fn pass_through_filter_forwards_a_token_to_its_pipeline_output() {
        let mut buffer = crate::pipeline::buffer::Buffer::new("PCLm token buffer", None);
        let mut output = TokenFilterOutput::new(Some(&mut buffer));
        let mut filter = PassThroughFilter;
        let token = Token::new(TokenType::Word, b"Do".to_vec());

        {
            filter
                .handle_token(&token, &mut output)
                .expect("pass-through filter forwards the token");
        }
        buffer.finish().expect("token buffer finish");
        assert_eq!(buffer.take_buffer().expect("token bytes"), b"Do");
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

    /// Drive the PCLm route the way a caller does, returning the emitted
    /// bytes or the first writer error.
    fn write_pclm_bytes<R: std::io::Read + Seek + 'static>(pdf: &mut Pdf<R>) -> Result<Vec<u8>> {
        let mut writer = PdfWriter::new(pdf);
        writer.set_pclm(true);
        writer.set_static_id(true);
        writer.set_output_memory()?;
        writer.write()?;
        writer.get_buffer()
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

    fn pinned_qpdf_source_from(command: &std::path::Path) -> Option<PathBuf> {
        let output = Command::new(command).arg("--print-path").output().ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8(output.stdout).ok()?;
        Some(PathBuf::from(path.trim()))
    }

    fn pinned_qpdf_source() -> Option<PathBuf> {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        pinned_qpdf_source_from(&workspace.join("scripts/fetch-qpdf-source.sh"))
    }

    fn oracle_source_for(
        label: &str,
        qpdf_available: bool,
        source: Option<PathBuf>,
    ) -> Option<PathBuf> {
        if !qpdf_available {
            eprintln!("qpdf 11.9.0 is unavailable; skipping {label} oracle");
            return None;
        }
        let Some(source) = source else {
            eprintln!("pinned qpdf source is unavailable; skipping {label} oracle");
            return None;
        };
        Some(source)
    }

    fn pclm_oracle_source(label: &str) -> Option<PathBuf> {
        let qpdf_available = exact_qpdf_11_9();
        let source = qpdf_available.then(pinned_qpdf_source).flatten();
        oracle_source_for(label, qpdf_available, source)
    }

    fn first_page_content(path: &std::path::Path) -> Vec<u8> {
        let pages = Command::new("qpdf")
            .arg("--show-pages")
            .arg(path)
            .output()
            .expect("inspect page content references");
        let pages_error = format!(
            "qpdf --show-pages failed: {}",
            String::from_utf8_lossy(&pages.stderr)
        );
        assert!(pages.status.success(), "{pages_error}");
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
        let stream_error = format!(
            "qpdf --show-object failed: {}",
            String::from_utf8_lossy(&stream.stderr)
        );
        assert!(stream.status.success(), "{stream_error}");
        stream.stdout
    }

    #[test]
    fn pclm_oracle_helpers_cover_unavailable_and_failed_command_paths() {
        assert!(oracle_source_for("test", false, Some(PathBuf::from("/tmp/source"))).is_none());
        assert!(oracle_source_for("test", true, None).is_none());
        let source = PathBuf::from("/tmp/source");
        assert_eq!(
            oracle_source_for("test", true, Some(source.clone())),
            Some(source)
        );
        assert!(pinned_qpdf_source_from(std::path::Path::new("/bin/false")).is_none());
    }

    /// qpdf's PCLm seed enqueues pages, their contents, their strips, one
    /// image-transform stream per strip, and finally `/Root`
    /// (`QPDFWriter::enqueueObjectsPCLm`, `libqpdf/QPDFWriter.cc:2927-2955`).
    /// A Catalog child is not part of that seed: it is discovered when the
    /// Catalog itself is unparsed, so it is numbered after every seeded
    /// object.
    #[test]
    fn pclm_catalog_child_is_numbered_after_the_initial_enqueue() {
        let mut pdf = one_page_fixture_pdf();
        let child = pdf
            .make_indirect_from_object_handle(ObjectHandle::string(b"late-child".to_vec()))
            .expect("allocate a Catalog child");
        pdf.root_handle()
            .expect("resolve Catalog")
            .replace_key(b"/LateChild", child)
            .expect("attach Catalog child");

        let mut writer = configure_pclm_writer(&mut pdf);
        writer.set_output_memory().expect("install memory output");
        writer.write().expect("write PCLm output");
        let output = writer.get_buffer().expect("PCLm output bytes");
        let output = String::from_utf8_lossy(&output);

        // The one-page fixture seeds page, contents, and Catalog, so the
        // Catalog is object 3. Everything numbered above 3 was discovered
        // while a seeded object was unparsed: the page's own children first,
        // then the Catalog's.
        let catalog = output
            .split_once("\n3 0 obj\n")
            .expect("PCLm Catalog is the last seeded object")
            .1;
        assert!(
            catalog.starts_with("<< /LateChild 6 0 R"),
            "a Catalog child belongs to emission-time discovery: {catalog:.120}"
        );
        assert!(
            output.contains("\n6 0 obj\n(late-child)\nendobj\n"),
            "the discovered child is appended to the same queue: {output}"
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
        if let Some(source) = pclm_oracle_source("PCLm normalization") {
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
            let qpdf_error = format!(
                "qpdf normalization failed: {}",
                String::from_utf8_lossy(&qpdf.stderr)
            );
            assert!(qpdf.status.success(), "{qpdf_error}");

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
        } // cov:ignore: LLVM maps the covered PCLm normalization branch exit to this line
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
        let final_sink = FinishRejectingPipeline {
            events: events.clone(),
        };
        assert_eq!(final_sink.identifier(), "PCLm final sink");
        writer
            .set_output_pipeline(final_sink)
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
        if let Some(source) = pclm_oracle_source("PCLm byte") {
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
        } // cov:ignore: LLVM attributes the covered qpdf-check success branch to the nested assertion.
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
            let check_error = format!(
                "qpdf rejected deterministic PCLm output: {}",
                String::from_utf8_lossy(&check.stderr)
            );
            assert!(check.status.success(), "{check_error}");
        } // cov:ignore: LLVM attributes the covered exact-qpdf branch exit to the assertion above.
    }

    #[test]
    fn pclm_reports_qpdf_dictionary_error_for_a_missing_root() {
        let mut pdf = Pdf::open(Cursor::new(
            b"%PDF-1.3\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 1 >>\n\
              startxref\n9\n%%EOF\n"
                .to_vec(),
        ))
        .expect("rootless fixture must open");

        let error = write_pclm_bytes(&mut pdf).expect_err("PCLm requires a trailer /Root");
        assert_eq!(error.to_string(), "unable to find /Root dictionary");
    }

    #[test]
    fn pclm_reports_qpdf_dictionary_error_for_an_indirect_null_root() {
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

        let error =
            write_pclm_bytes(&mut pdf).expect_err("an indirect null Root is not a dictionary");
        assert_eq!(error.to_string(), "unable to find /Root dictionary");
    }

    #[test]
    fn pclm_seeds_a_non_page_kids_leaf_like_qpdf() {
        let mut pdf = fixture_pdf();
        let page = crate::pages::page_refs(&mut pdf).unwrap()[0];
        pdf.replace_object(page, ObjectHandle::integer(42))
            .expect("replace page with a scalar through the canonical route");

        let output = write_pclm_bytes(&mut pdf).expect("a scalar page leaf is seeded like qpdf");
        let output = String::from_utf8_lossy(&output);

        // qpdf's `getAllPagesInternal` treats any `/Kids` entry without
        // `/Kids` as a page leaf, scalar or not (`libqpdf/QPDF_pages.cc:91-131`),
        // and the PCLm seed numbers the first page object first.
        assert!(
            output.contains("\n1 0 obj\n42\nendobj"),
            "a scalar leaf is seeded as the first PCLm page: {output}"
        );
    }

    #[test]
    fn pclm_seeds_a_raw_generation_page_without_object_ref_projection() {
        if !exact_qpdf_11_9() {
            return;
        }
        let mut pdf = Pdf::empty().expect("empty PDF");
        let pages = pdf
            .root_handle()
            .expect("Catalog")
            .try_get_key(b"/Pages")
            .expect("root Pages");
        let page_ref = ObjectRef::new(17, 65_535);
        let page = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
            (b"/Parent".to_vec(), pages.clone()),
            (
                b"/MediaBox".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(612),
                    ObjectHandle::integer(792),
                ]),
            ),
            (
                b"/Resources".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/XObject".to_vec(),
                    ObjectHandle::dictionary(Vec::new()),
                )]),
            ),
        ]);
        pdf.replace_object(page_ref, page)
            .expect("install qpdf raw-generation page");
        let raw_page = pdf.get_object_handle_by_raw_identity(17, 65_535);
        pages
            .replace_key(b"/Kids", ObjectHandle::array(vec![raw_page]))
            .expect("attach raw-generation page");
        pages
            .replace_key(b"/Count", ObjectHandle::integer(1))
            .expect("set page count");

        let output = write_pclm_bytes(&mut pdf)
            .expect("PCLm queue must enqueue the raw page handle without ObjectRef projection");
        let directory = tempfile::tempdir().expect("PCLm output directory");
        let output_path = directory.path().join("raw-generation-pclm.pdf");
        std::fs::write(&output_path, output).expect("write PCLm output");
        let check = Command::new("qpdf")
            .arg("--check")
            .arg(&output_path)
            .output()
            .expect("qpdf checks PCLm output");
        assert_eq!(
            check.status.code(),
            Some(0),
            "qpdf accepts PCLm output: {}",
            String::from_utf8_lossy(&check.stderr)
        );
        let page_count = Command::new("qpdf")
            .arg("--show-npages")
            .arg(&output_path)
            .output()
            .expect("qpdf reads PCLm page count");
        assert!(page_count.status.success());
        assert_eq!(String::from_utf8_lossy(&page_count.stdout).trim(), "1");
    }

    #[test]
    fn pclm_propagates_page_contents_resolution_errors() {
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

        assert!(write_pclm_bytes(&mut pdf).is_err());
    }

    #[test]
    fn pclm_enqueues_xobject_and_its_synthetic_transform() {
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

        let output = write_pclm_bytes(&mut pdf).expect("XObject PCLm output");

        assert!(
            String::from_utf8_lossy(&output).contains("q /image Do Q\n"),
            "each strip is followed by its image-transform stream"
        );
    }

    #[test]
    fn writer_emits_unplanned_trailer_refs_with_qpdf_late_numbers() {
        let mut pdf = one_page_fixture_with_unplanned_trailer_refs();

        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_pclm(true);
        writer.set_deterministic_id(true);
        writer.set_output_memory().expect("install memory output");
        writer.write().expect("qpdf-compatible PCLm output");
        let output = writer.get_buffer().expect("PCLm output bytes");
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
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_pclm(true);
        writer.set_output_memory().expect("install memory output");
        writer.write().expect("qpdf-compatible PCLm output");
        let output = writer.get_buffer().expect("PCLm output bytes");
        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("/Info 7 0 R"));
        assert!(output.contains("/Probe 8 0 R"));
        assert!(output.contains("/ProbeAgain 8 0 R"));
        assert!(!output.contains("7 0 obj\n"));
        assert!(!output.contains("8 0 obj\n"));
    }
}

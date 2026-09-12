//! RED contracts for the non-linearized writer's final-output ownership.
//!
//! These tests intentionally keep the causal and allocation assertions red
//! until the writer emits each body object through its configured final sink.

use flpdf::{
    Error, ObjectHandle, ObjectStreamMode, Pdf, PdfWriter, Pipeline, PipelineError, PipelineResult,
    StreamDataProvider,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::io::{self, Cursor, ErrorKind, Write};
use std::rc::Rc;

struct LiveAllocationTracker;

const PROVIDER_A_PAYLOAD: &[u8] = b"provider-payload-A";

std::thread_local! {
    static ALLOCATION_TRACKING: Cell<bool> = const { Cell::new(false) };
    static LIVE_BYTES: Cell<usize> = const { Cell::new(0) };
    static PEAK_LIVE_BYTES: Cell<usize> = const { Cell::new(0) };
}

#[global_allocator]
static ALLOCATOR: LiveAllocationTracker = LiveAllocationTracker;

fn record_allocation(size: usize) {
    ALLOCATION_TRACKING.with(|tracking| {
        if tracking.get() {
            LIVE_BYTES.with(|live| {
                let live_bytes = live.get().saturating_add(size);
                live.set(live_bytes);
                PEAK_LIVE_BYTES.with(|peak| {
                    if live_bytes > peak.get() {
                        peak.set(live_bytes);
                    }
                });
            });
        }
    });
}

fn record_deallocation(size: usize) {
    ALLOCATION_TRACKING.with(|tracking| {
        if tracking.get() {
            LIVE_BYTES.with(|live| live.set(live.get().saturating_sub(size)));
        }
    });
}

fn start_allocation_measurement_after_fixture_baseline() {
    LIVE_BYTES.with(|live| live.set(0));
    PEAK_LIVE_BYTES.with(|peak| peak.set(0));
    ALLOCATION_TRACKING.with(|tracking| tracking.set(true));
}

fn finish_allocation_measurement() -> usize {
    ALLOCATION_TRACKING.with(|tracking| tracking.set(false));
    PEAK_LIVE_BYTES.with(Cell::get)
}

unsafe impl GlobalAlloc for LiveAllocationTracker {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record_allocation(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            record_allocation(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        record_deallocation(layout.size());
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let reallocated = unsafe { System.realloc(pointer, layout, new_size) };
        if !reallocated.is_null() {
            record_deallocation(layout.size());
            record_allocation(new_size);
        }
        reallocated
    }
}

fn minimal_pdf() -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/minimal.pdf").to_vec(),
    ))
    .expect("open minimal writer fixture")
}

#[derive(Clone)]
struct ProviderEvents(Rc<RefCell<Vec<String>>>);

impl ProviderEvents {
    fn record(&self, event: impl Into<String>) {
        self.0.borrow_mut().push(event.into());
    }

    fn snapshot(&self) -> Vec<String> {
        self.0.borrow().clone()
    }
}

struct PayloadProvider {
    name: &'static str,
    payload: Rc<Vec<u8>>,
    events: Option<ProviderEvents>,
}

impl StreamDataProvider for PayloadProvider {
    fn provide_stream_data_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
    ) -> flpdf::Result<()> {
        if let Some(events) = &self.events {
            events.record(format!("provider:{}", self.name));
        }
        pipeline.write(&self.payload).map_err(Error::from)?;
        pipeline.finish().map_err(Error::from)
    }
}

struct TerminalFailureProvider {
    events: ProviderEvents,
}

impl StreamDataProvider for TerminalFailureProvider {
    fn provide_stream_data_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        _pipeline: &mut dyn Pipeline,
    ) -> flpdf::Result<()> {
        self.events.record("provider:A");
        Err(Error::System("terminal provider A failure".to_owned()))
    }
}

struct SegmentFinishFailureProvider {
    events: ProviderEvents,
    payload: Rc<Vec<u8>>,
}

struct FailingSegmentFinish<'a> {
    next: &'a mut dyn Pipeline,
}

impl Pipeline for FailingSegmentFinish<'_> {
    fn identifier(&self) -> &str {
        "writer streaming test segment"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.next.write(data)
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Err(PipelineError::runtime("stream segment finish failure"))
    }
}

impl StreamDataProvider for SegmentFinishFailureProvider {
    fn provide_stream_data_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
    ) -> flpdf::Result<()> {
        self.events.record("provider:A");
        let mut segment = FailingSegmentFinish { next: pipeline };
        segment.write(&self.payload).map_err(Error::from)?;
        segment.finish().map_err(Error::from)
    }
}

struct RetryOnceProvider {
    calls: Rc<RefCell<Vec<(bool, bool)>>>,
    payload: Rc<Vec<u8>>,
}

impl StreamDataProvider for RetryOnceProvider {
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
    ) -> flpdf::Result<bool> {
        self.calls
            .borrow_mut()
            .push((suppress_warnings, will_retry));
        if will_retry {
            return Ok(false);
        }
        pipeline.write(&self.payload).map_err(Error::from)?;
        pipeline.finish().map_err(Error::from)?;
        Ok(true)
    }
}

fn provider_stream(
    pdf: &Pdf<Cursor<Vec<u8>>>,
    provider: impl StreamDataProvider + 'static,
) -> ObjectHandle {
    let stream = pdf.new_stream().expect("create indirect provider stream");
    stream
        .replace_stream_data_provider(Rc::new(provider), None, None)
        .expect("register provider on indirect stream");
    stream
}

fn attach_two_streams(pdf: &mut Pdf<Cursor<Vec<u8>>>, first: ObjectHandle, second: ObjectHandle) {
    let root = pdf.root_handle().expect("resolve live Catalog");
    root.replace_key(b"/WriterStreamingA", first)
        .expect("attach first provider stream");
    root.replace_key(b"/WriterStreamingB", second)
        .expect("attach second provider stream");
}

fn configure_disable_writer<'pdf>(
    pdf: &'pdf mut Pdf<Cursor<Vec<u8>>>,
) -> PdfWriter<'pdf, Cursor<Vec<u8>>> {
    let mut writer = PdfWriter::new(pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_compress_streams(false);
    writer.set_static_id(true);
    writer
}

struct EventWriter {
    events: ProviderEvents,
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

struct TerminalSinkWriter {
    events: ProviderEvents,
    accepted: Rc<RefCell<Vec<u8>>>,
    rejected: bool,
    matched_a_payload_prefix: usize,
}

impl Write for TerminalSinkWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.rejected {
            self.events.record("sink-error:A");
            return Err(io::Error::other(
                "terminal output sink rejected provider A payload",
            ));
        }

        for (index, &byte) in data.iter().enumerate() {
            if self.advance_a_payload_match(byte) {
                let payload_start = (index + 1).saturating_sub(PROVIDER_A_PAYLOAD.len());
                self.accepted
                    .borrow_mut()
                    .extend_from_slice(&data[..payload_start]);
                self.rejected = true;
                if payload_start > 0 {
                    self.events.record("sink-prefix");
                    return Ok(payload_start);
                }
                self.events.record("sink-error:A");
                return Err(io::Error::other(
                    "terminal output sink rejected provider A payload",
                ));
            }
        }

        self.accepted.borrow_mut().extend_from_slice(data);
        self.events.record("sink-prefix");
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl TerminalSinkWriter {
    fn advance_a_payload_match(&mut self, byte: u8) -> bool {
        let marker = PROVIDER_A_PAYLOAD;
        let mut matched = self.matched_a_payload_prefix;
        while matched > 0 && marker[matched] != byte {
            matched = Self::marker_fallback(matched);
        }
        if marker[matched] == byte {
            matched += 1;
        } else {
            matched = 0;
        }
        self.matched_a_payload_prefix = matched;
        matched == marker.len()
    }

    fn marker_fallback(prefix_len: usize) -> usize {
        let marker = PROVIDER_A_PAYLOAD;
        (0..prefix_len)
            .rev()
            .find(|&suffix_len| marker[..suffix_len] == marker[prefix_len - suffix_len..prefix_len])
            .unwrap_or(0)
    }

    fn new(events: ProviderEvents, accepted: Rc<RefCell<Vec<u8>>>) -> Self {
        Self {
            events,
            accepted,
            rejected: false,
            matched_a_payload_prefix: 0,
        }
    }
}

#[test]
fn terminal_sink_writer_rejects_a_payload_split_at_any_write_boundary() {
    for split in 1..PROVIDER_A_PAYLOAD.len() {
        let events = ProviderEvents(Rc::new(RefCell::new(Vec::new())));
        let accepted = Rc::new(RefCell::new(Vec::new()));
        let mut sink = TerminalSinkWriter::new(events, Rc::clone(&accepted));
        sink.write(b"%PDF-1.7\n")
            .expect("header bytes must be accepted");
        sink.write(&PROVIDER_A_PAYLOAD[..split])
            .expect("an incomplete A marker is not yet an error");
        let error = sink
            .write(&PROVIDER_A_PAYLOAD[split..])
            .expect_err("the rolling matcher must reject a split A marker");
        assert!(error
            .to_string()
            .contains("terminal output sink rejected provider A payload"));
        assert!(accepted.borrow().starts_with(b"%PDF-"));
    }
}

#[test]
fn non_linearized_disable_reaches_the_sink_before_the_next_stream_provider() {
    let events = ProviderEvents(Rc::new(RefCell::new(Vec::new())));
    let bytes = Rc::new(RefCell::new(Vec::new()));
    let mut pdf = minimal_pdf();
    let first = provider_stream(
        &pdf,
        PayloadProvider {
            name: "A",
            payload: Rc::new(PROVIDER_A_PAYLOAD.to_vec()),
            events: Some(events.clone()),
        },
    );
    let second = provider_stream(
        &pdf,
        PayloadProvider {
            name: "B",
            payload: Rc::new(b"provider-payload-B".to_vec()),
            events: Some(events.clone()),
        },
    );
    attach_two_streams(&mut pdf, first, second);

    let mut writer = configure_disable_writer(&mut pdf);
    writer
        .set_output_writer(EventWriter {
            events: events.clone(),
            bytes: Rc::clone(&bytes),
        })
        .expect("install event-recording Writer output");
    writer.write().expect("writer output succeeds");

    assert!(
        !bytes.borrow().is_empty(),
        "writer must produce final bytes"
    );
    let events = events.snapshot();
    let provider_a = events
        .iter()
        .position(|event| event == "provider:A")
        .expect("first provider event");
    let provider_b = events
        .iter()
        .position(|event| event == "provider:B")
        .expect("second provider event");
    assert!(
        events[provider_a + 1..provider_b]
            .iter()
            .any(|event| event == "sink-write"),
        "a final-sink write must occur after provider A and before provider B; events: {events:?}"
    );
}

#[test]
fn terminal_output_sink_failure_never_requests_the_second_provider() {
    let events = ProviderEvents(Rc::new(RefCell::new(Vec::new())));
    let accepted = Rc::new(RefCell::new(Vec::new()));
    let mut pdf = minimal_pdf();
    let first = provider_stream(
        &pdf,
        PayloadProvider {
            name: "A",
            payload: Rc::new(PROVIDER_A_PAYLOAD.to_vec()),
            events: Some(events.clone()),
        },
    );
    let second = provider_stream(
        &pdf,
        PayloadProvider {
            name: "B",
            payload: Rc::new(b"provider-payload-B".to_vec()),
            events: Some(events.clone()),
        },
    );
    attach_two_streams(&mut pdf, first, second);

    let mut writer = configure_disable_writer(&mut pdf);
    writer
        .set_output_writer(TerminalSinkWriter::new(
            events.clone(),
            Rc::clone(&accepted),
        ))
        .expect("install terminal output sink");
    let error = writer
        .write()
        .expect_err("terminal sink failure must escape the writer");

    assert!(error
        .to_string()
        .contains("terminal output sink rejected provider A payload"));
    assert!(
        accepted.borrow().starts_with(b"%PDF-"),
        "header and earlier document bytes must be accepted before A is rejected"
    );
    let events = events.snapshot();
    let provider_a_count = events.iter().filter(|event| *event == "provider:A").count();
    let sink_error = events
        .iter()
        .position(|event| event == "sink-error:A")
        .expect("the sink must identify A's rejected payload");
    assert_eq!(
        provider_a_count, 1,
        "A must not be retried after sink failure"
    );
    assert!(
        events[..sink_error]
            .iter()
            .any(|event| event == "provider:A"),
        "A must be requested before its payload reaches the rejecting sink"
    );
    assert!(
        !events.iter().any(|event| event == "provider:B"),
        "a terminal A sink failure must not request B or any later provider; events: {events:?}"
    );
}

#[test]
fn terminal_first_provider_failure_never_requests_the_second_provider() {
    let events = ProviderEvents(Rc::new(RefCell::new(Vec::new())));
    let mut pdf = minimal_pdf();
    let first = provider_stream(
        &pdf,
        TerminalFailureProvider {
            events: events.clone(),
        },
    );
    let second = provider_stream(
        &pdf,
        PayloadProvider {
            name: "B",
            payload: Rc::new(b"provider-payload-B".to_vec()),
            events: Some(events.clone()),
        },
    );
    attach_two_streams(&mut pdf, first, second);

    let mut writer = configure_disable_writer(&mut pdf);
    writer.set_output_memory().expect("install memory output");
    let error = writer
        .write()
        .expect_err("terminal first provider failure must escape");
    assert!(error.to_string().contains("terminal provider A failure"));
    assert_eq!(events.snapshot(), vec!["provider:A"]);
}

#[test]
fn segment_finish_failure_never_requests_the_second_provider() {
    let events = ProviderEvents(Rc::new(RefCell::new(Vec::new())));
    let mut pdf = minimal_pdf();
    let first = provider_stream(
        &pdf,
        SegmentFinishFailureProvider {
            events: events.clone(),
            payload: Rc::new(PROVIDER_A_PAYLOAD.to_vec()),
        },
    );
    let second = provider_stream(
        &pdf,
        PayloadProvider {
            name: "B",
            payload: Rc::new(b"provider-payload-B".to_vec()),
            events: Some(events.clone()),
        },
    );
    attach_two_streams(&mut pdf, first, second);

    let mut writer = configure_disable_writer(&mut pdf);
    writer.set_output_memory().expect("install memory output");
    let error = writer
        .write()
        .expect_err("stream segment finish failure must escape");
    assert!(error.to_string().contains("stream segment finish failure"));
    assert_eq!(events.snapshot(), vec!["provider:A"]);
}

#[test]
fn recoverable_filter_failure_retries_only_the_same_stream() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let events = ProviderEvents(Rc::new(RefCell::new(Vec::new())));
    let mut pdf = minimal_pdf();
    let first = provider_stream(
        &pdf,
        RetryOnceProvider {
            calls: Rc::clone(&calls),
            payload: Rc::new(b"retry-payload-A".to_vec()),
        },
    );
    let second = provider_stream(
        &pdf,
        PayloadProvider {
            name: "B",
            payload: Rc::new(b"provider-payload-B".to_vec()),
            events: Some(events.clone()),
        },
    );
    attach_two_streams(&mut pdf, first, second);

    let mut writer = configure_disable_writer(&mut pdf);
    writer.set_compress_streams(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("retry of first provider succeeds");

    assert_eq!(*calls.borrow(), vec![(false, true), (false, false)]);
    assert_eq!(events.snapshot(), vec!["provider:B"]);
}

struct ShortWriter {
    bytes: Rc<RefCell<Vec<u8>>>,
}

impl Write for ShortWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let accepted = data.len().min(7);
        self.bytes.borrow_mut().extend_from_slice(&data[..accepted]);
        Ok(accepted)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct InterruptedWriter {
    interrupted: Cell<bool>,
    bytes: Rc<RefCell<Vec<u8>>>,
}

impl Write for InterruptedWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if !self.interrupted.replace(true) {
            return Err(io::Error::from(ErrorKind::Interrupted));
        }
        self.bytes.borrow_mut().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct WriteZeroWriter;

impl Write for WriteZeroWriter {
    fn write(&mut self, _data: &[u8]) -> io::Result<usize> {
        Ok(0)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn arbitrary_writer_retries_short_positive_writes() {
    let bytes = Rc::new(RefCell::new(Vec::new()));
    let mut pdf = minimal_pdf();
    let mut writer = configure_disable_writer(&mut pdf);
    writer
        .set_output_writer(ShortWriter {
            bytes: Rc::clone(&bytes),
        })
        .expect("install short writer");
    writer
        .write()
        .expect("write_all must complete short writes");
    assert!(bytes.borrow().starts_with(b"%PDF-"));
}

#[test]
fn arbitrary_writer_retries_interrupted_writes() {
    let expected = write_memory_output();
    let bytes = Rc::new(RefCell::new(Vec::new()));
    let mut pdf = minimal_pdf();
    let mut writer = configure_disable_writer(&mut pdf);
    writer
        .set_output_writer(InterruptedWriter {
            interrupted: Cell::new(false),
            bytes: Rc::clone(&bytes),
        })
        .expect("install interrupted writer");
    writer.write().expect("write_all must retry Interrupted");
    assert_eq!(bytes.borrow().as_slice(), expected.as_slice());
}

#[test]
fn arbitrary_writer_surfaces_write_zero_as_an_io_failure() {
    let mut pdf = minimal_pdf();
    let mut writer = configure_disable_writer(&mut pdf);
    writer
        .set_output_writer(WriteZeroWriter)
        .expect("install zero-progress writer");
    let error = writer.write().expect_err("zero-progress write must fail");
    assert!(
        matches!(error, Error::Io(ref source) if source.kind() == ErrorKind::WriteZero),
        "expected WriteZero I/O error, got {error:?}"
    );
}

#[test]
fn memory_output_is_the_one_complete_output_owner() {
    let memory_bytes = write_memory_output();

    let writer_bytes = Rc::new(RefCell::new(Vec::new()));
    let mut writer_pdf = minimal_pdf();
    let mut writer = configure_disable_writer(&mut writer_pdf);
    writer
        .set_output_writer(ShortWriter {
            bytes: Rc::clone(&writer_bytes),
        })
        .expect("install arbitrary writer output");
    writer.write().expect("arbitrary writer succeeds");

    assert_eq!(memory_bytes, *writer_bytes.borrow());
}

fn write_memory_output() -> Vec<u8> {
    let mut memory_pdf = minimal_pdf();
    let mut memory_writer = configure_disable_writer(&mut memory_pdf);
    memory_writer
        .set_output_memory()
        .expect("install memory output");
    memory_writer.write().expect("memory writer succeeds");
    memory_writer
        .get_buffer()
        .expect("memory output can be taken exactly once")
}

#[test]
fn non_linearized_body_result_does_not_expose_a_complete_output_vec() {
    let source = include_str!("../src/writer/plain/body.rs");
    assert!(
        !source.contains("bytes: Vec<u8>"),
        "the non-linearized body result must return layout metadata, not a complete Vec"
    );
}

struct DiscardWriter;

impl Write for DiscardWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

const STREAM_PAYLOAD_BYTES: usize = 256 * 1024;
const ONE_STREAM_LOCAL_BUFFER_BOUND: usize = STREAM_PAYLOAD_BYTES;

fn measure_peak_live_delta(stream_count: usize) -> usize {
    let mut pdf = minimal_pdf();
    let root = pdf.root_handle().expect("resolve live Catalog");
    for index in 0..stream_count {
        let stream = provider_stream(
            &pdf,
            PayloadProvider {
                name: "allocation",
                payload: Rc::new(vec![b'x'; STREAM_PAYLOAD_BYTES]),
                events: None,
            },
        );
        root.replace_key(
            format!("/WriterStreamingAllocation{index:02}").as_bytes(),
            stream,
        )
        .expect("attach provider stream for allocation measurement");
    }

    start_allocation_measurement_after_fixture_baseline();
    let mut writer = configure_disable_writer(&mut pdf);
    writer
        .set_output_writer(DiscardWriter)
        .expect("install discard output");
    writer.write().expect("discard writer succeeds");
    finish_allocation_measurement()
}

#[test]
fn discard_output_peak_growth_is_bounded_by_one_stream_local_buffer() {
    let two_stream_peak = measure_peak_live_delta(2);
    let four_stream_peak = measure_peak_live_delta(4);

    assert!(
        four_stream_peak <= two_stream_peak + ONE_STREAM_LOCAL_BUFFER_BOUND,
        "four streams retained more than one permitted local stream buffer: two={two_stream_peak}, four={four_stream_peak}, bound={ONE_STREAM_LOCAL_BUFFER_BOUND}"
    );
}

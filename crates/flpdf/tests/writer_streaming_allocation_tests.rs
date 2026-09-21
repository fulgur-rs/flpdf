//! RED contracts for the non-linearized writer's final-output ownership.
//!
//! These tests intentionally keep the causal and allocation assertions red
//! until the writer emits each body object through its configured final sink.

use flpdf::{
    EncryptParams, Error, ObjectHandle, ObjectStreamMode, Pdf, PdfOpenOptions, PdfWriter, Pipeline,
    PipelineError, PipelineResult, StreamDataProvider,
};
use md5::Digest as _;
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
    static TOTAL_ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
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
            TOTAL_ALLOCATIONS.with(|count| count.set(count.get().saturating_add(1)));
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
    TOTAL_ALLOCATIONS.with(|count| count.set(0));
    ALLOCATION_TRACKING.with(|tracking| tracking.set(true));
}

fn finish_allocation_measurement() -> usize {
    ALLOCATION_TRACKING.with(|tracking| tracking.set(false));
    PEAK_LIVE_BYTES.with(Cell::get)
}

fn finish_allocation_count() -> usize {
    ALLOCATION_TRACKING.with(|tracking| tracking.set(false));
    TOTAL_ALLOCATIONS.with(Cell::get)
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

fn assert_fallback_reaches_sink_before_provider_b(
    configure: impl FnOnce(&mut PdfWriter<'_, Cursor<Vec<u8>>>),
) {
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
    configure(&mut writer);
    writer
        .set_output_writer(EventWriter {
            events: events.clone(),
            bytes: Rc::clone(&bytes),
        })
        .expect("install event-recording fallback Writer output");
    writer.write().expect("fallback writer output succeeds");

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
        "fallback must emit A to the final sink before requesting B: {events:?}"
    );
    assert!(bytes.borrow().starts_with(b"%PDF-"));
}

struct EventWriter {
    events: ProviderEvents,
    bytes: Rc<RefCell<Vec<u8>>>,
}

struct ArmOnProvider {
    name: &'static str,
    payload: Rc<Vec<u8>>,
    events: ProviderEvents,
    arm: Rc<Cell<bool>>,
}

impl StreamDataProvider for ArmOnProvider {
    fn provide_stream_data_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
    ) -> flpdf::Result<()> {
        self.events.record(format!("provider:{}", self.name));
        self.arm.set(true);
        pipeline.write(&self.payload).map_err(Error::from)?;
        pipeline.finish().map_err(Error::from)
    }
}

struct ArmableFailureWriter {
    events: ProviderEvents,
    bytes: Rc<RefCell<Vec<u8>>>,
    arm: Rc<Cell<bool>>,
    fail_after_stream_marker: bool,
    error_kind: ErrorKind,
    flushes: Rc<Cell<usize>>,
}

impl Write for ArmableFailureWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.fail_after_stream_marker {
            self.fail_after_stream_marker = false;
            self.events.record("sink-error:armed");
            return Err(io::Error::new(
                self.error_kind,
                "task-6 armed writer failure",
            ));
        }
        if self.arm.get()
            && data
                .windows(b"\nstream\n".len())
                .any(|part| part == b"\nstream\n")
        {
            self.arm.set(false);
            self.fail_after_stream_marker = true;
        }
        self.events.record("sink-write");
        self.bytes.borrow_mut().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes.set(self.flushes.get() + 1);
        self.events.record("sink-flush");
        Ok(())
    }
}

struct ArmOnObjStmWriter {
    bytes: Rc<RefCell<Vec<u8>>>,
    saw_objstm: bool,
    armed: bool,
    flushes: Rc<Cell<usize>>,
}

struct FirstThenTeardownErrorWriter {
    bytes: Rc<RefCell<Vec<u8>>>,
    arm: Rc<Cell<bool>>,
    stage: Rc<Cell<u8>>,
    flushes: Rc<Cell<usize>>,
}

impl Write for FirstThenTeardownErrorWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self.stage.get() {
            1 => {
                self.stage.set(2);
                self.bytes.borrow_mut().extend_from_slice(data);
                Ok(data.len())
            }
            2 => {
                self.stage.set(3);
                Err(io::Error::new(
                    ErrorKind::PermissionDenied,
                    "task-6 first encrypted payload failure",
                ))
            }
            3 => Ok(0),
            _ => {
                if self.arm.get()
                    && data
                        .windows(b"\nstream\n".len())
                        .any(|part| part == b"\nstream\n")
                {
                    self.arm.set(false);
                    self.stage.set(1);
                }
                self.bytes.borrow_mut().extend_from_slice(data);
                Ok(data.len())
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes.set(self.flushes.get() + 1);
        Ok(())
    }
}

impl Write for ArmOnObjStmWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.armed {
            return Ok(0);
        }
        if self.saw_objstm
            && data
                .windows(b"\nstream\n".len())
                .any(|part| part == b"\nstream\n")
        {
            self.armed = true;
        }
        if data
            .windows(b"/Type /ObjStm".len())
            .any(|part| part == b"/Type /ObjStm")
        {
            self.saw_objstm = true;
        }
        self.bytes.borrow_mut().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes.set(self.flushes.get() + 1);
        Ok(())
    }
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
        assert_eq!(
            sink.write(b"%PDF-1.7\n")
                .expect("header bytes must be accepted"),
            b"%PDF-1.7\n".len()
        );
        assert_eq!(
            sink.write(&PROVIDER_A_PAYLOAD[..split])
                .expect("an incomplete A marker is not yet an error"),
            split
        );
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
fn extra_header_fallback_reaches_the_sink_before_the_next_provider() {
    assert_fallback_reaches_sink_before_provider_b(|writer| {
        writer.set_extra_header_text("% task-6 extra header");
    });
}

#[test]
fn forced_version_fallback_reaches_the_sink_before_the_next_provider() {
    assert_fallback_reaches_sink_before_provider_b(|writer| {
        writer.set_object_stream_mode(ObjectStreamMode::Generate);
        writer.force_pdf_version("1.4", 0);
    });
}

#[test]
fn encrypted_fallback_reaches_the_sink_before_the_next_provider() {
    assert_fallback_reaches_sink_before_provider_b(|writer| {
        writer.set_encryption_parameters(EncryptParams::v4_aes128(b"user", b"owner"));
        writer.set_static_aes_iv(true);
    });
}

fn assert_armed_stream_writer_error_preserves_io_kind(encrypted: bool) {
    let events = ProviderEvents(Rc::new(RefCell::new(Vec::new())));
    let arm = Rc::new(Cell::new(false));
    let bytes = Rc::new(RefCell::new(Vec::new()));
    let flushes = Rc::new(Cell::new(0));
    let mut pdf = minimal_pdf();
    let first = provider_stream(
        &pdf,
        ArmOnProvider {
            name: "A",
            payload: Rc::new(vec![b'A'; 4096]),
            events: events.clone(),
            arm: Rc::clone(&arm),
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
    if encrypted {
        writer.set_encryption_parameters(EncryptParams::v4_aes128(b"user", b"owner"));
        writer.set_static_aes_iv(true);
    }
    writer
        .set_output_writer(ArmableFailureWriter {
            events: events.clone(),
            bytes,
            arm,
            fail_after_stream_marker: false,
            error_kind: ErrorKind::PermissionDenied,
            flushes: Rc::clone(&flushes),
        })
        .expect("install armed Writer sink");
    let error = writer
        .write()
        .expect_err("armed stream Writer failure must escape");

    assert!(
        matches!(error, Error::Io(ref source) if source.kind() == ErrorKind::PermissionDenied),
        "encrypted={encrypted} must preserve Error::Io kind, got {error:?}"
    );
    assert_eq!(flushes.get(), 1, "stream teardown must flush once");
    let events = events.snapshot();
    assert!(
        !events.iter().any(|event| event == "provider:B"),
        "events: {events:?}"
    );
}

#[test]
fn ordinary_stream_writer_error_preserves_io_kind_and_finishes_segment_once() {
    assert_armed_stream_writer_error_preserves_io_kind(false);
}

#[test]
fn encrypted_stream_writer_error_preserves_io_kind_and_finishes_segment_once() {
    assert_armed_stream_writer_error_preserves_io_kind(true);
}

#[test]
fn encrypted_stream_first_writer_error_wins_over_a_different_teardown_error() {
    let events = ProviderEvents(Rc::new(RefCell::new(Vec::new())));
    let arm = Rc::new(Cell::new(false));
    let stage = Rc::new(Cell::new(0));
    let flushes = Rc::new(Cell::new(0));
    let mut pdf = minimal_pdf();
    let first = provider_stream(
        &pdf,
        ArmOnProvider {
            name: "A",
            payload: Rc::new(PROVIDER_A_PAYLOAD.to_vec()),
            events: events.clone(),
            arm: Rc::clone(&arm),
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
    writer.set_encryption_parameters(EncryptParams::v4_aes128(b"user", b"owner"));
    writer.set_static_aes_iv(true);
    writer
        .set_output_writer(FirstThenTeardownErrorWriter {
            bytes: Rc::new(RefCell::new(Vec::new())),
            arm,
            stage,
            flushes: Rc::clone(&flushes),
        })
        .expect("install first-then-teardown error Writer");
    let error = writer
        .write()
        .expect_err("the first encrypted payload error must escape");

    assert!(
        matches!(error, Error::Io(ref source) if source.kind() == ErrorKind::PermissionDenied),
        "the first Writer error must win over teardown WriteZero, got {error:?}"
    );
    assert_eq!(flushes.get(), 1, "encrypted segment teardown must run once");
    assert!(!events.snapshot().iter().any(|event| event == "provider:B"));
}

#[test]
fn encrypted_objstm_write_zero_preserves_io_kind_and_finishes_segment_once() {
    let mut pdf = minimal_pdf();
    let root = pdf.root_handle().expect("resolve Catalog");
    for index in 0..4 {
        let member = pdf
            .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
                format!("/Member{index}").into_bytes(),
                ObjectHandle::integer(i64::from(index)),
            )]))
            .expect("create ObjStm member");
        root.replace_key(format!("/Generated{index}").as_bytes(), member)
            .expect("attach ObjStm member");
    }

    let flushes = Rc::new(Cell::new(0));
    let mut writer = configure_disable_writer(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_encryption_parameters(EncryptParams::v4_aes128(b"user", b"owner"));
    writer.set_static_aes_iv(true);
    writer
        .set_output_writer(ArmOnObjStmWriter {
            bytes: Rc::new(RefCell::new(Vec::new())),
            saw_objstm: false,
            armed: false,
            flushes: Rc::clone(&flushes),
        })
        .expect("install ObjStm WriteZero sink");
    let error = writer
        .write()
        .expect_err("encrypted ObjStm WriteZero must escape");

    assert!(
        matches!(error, Error::Io(ref source) if source.kind() == ErrorKind::WriteZero),
        "encrypted ObjStm must preserve WriteZero, got {error:?}"
    );
    assert_eq!(flushes.get(), 1, "ObjStm stream teardown must flush once");
}

#[test]
fn source_encrypted_fallback_reaches_the_sink_before_the_next_provider() {
    let mut source = minimal_pdf();
    let mut source_writer = configure_disable_writer(&mut source);
    source_writer.set_encryption_parameters(EncryptParams::v4_aes128(b"user", b"owner"));
    source_writer.set_static_aes_iv(true);
    source_writer
        .set_output_memory()
        .expect("install encrypted source output");
    source_writer.write().expect("write encrypted source");
    let encrypted = source_writer
        .get_buffer()
        .expect("take encrypted source output");
    let mut pdf = Pdf::open_with_options(
        Cursor::new(encrypted),
        PdfOpenOptions {
            password: b"user".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("open encrypted source");

    let events = ProviderEvents(Rc::new(RefCell::new(Vec::new())));
    let bytes = Rc::new(RefCell::new(Vec::new()));
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
    writer.set_static_aes_iv(true);
    writer
        .set_output_writer(EventWriter {
            events: events.clone(),
            bytes,
        })
        .expect("install source-encrypted output");
    writer.write().expect("rewrite source-encrypted PDF");

    let events = events.snapshot();
    let provider_a = events
        .iter()
        .position(|event| event == "provider:A")
        .unwrap();
    let provider_b = events
        .iter()
        .position(|event| event == "provider:B")
        .unwrap();
    assert!(
        events[provider_a + 1..provider_b]
            .iter()
            .any(|event| event == "sink-write"),
        "source-encrypted fallback must emit A before requesting B: {events:?}"
    );
}

#[test]
fn cleartext_metadata_stays_clear_while_other_encrypted_streams_do_not() {
    let mut pdf = minimal_pdf();
    let metadata_payload = b"task-6-cleartext-metadata";
    let secret_payload = b"task-6-encrypted-payload";
    let metadata = pdf
        .new_stream_with_data(Rc::new(metadata_payload.to_vec()))
        .expect("create metadata stream");
    metadata
        .as_stream_dict()
        .expect("metadata stream dictionary")
        .replace_key(b"/Type", ObjectHandle::name(b"Metadata".to_vec()))
        .expect("mark metadata stream");
    let secret = pdf
        .new_stream_with_data(Rc::new(secret_payload.to_vec()))
        .expect("create encrypted stream");
    let root = pdf.root_handle().expect("resolve Catalog");
    root.replace_key(b"/Metadata", metadata)
        .expect("attach metadata");
    root.replace_key(b"/Secret", secret)
        .expect("attach secret stream");

    let mut params = EncryptParams::v4_aes128(b"user", b"owner");
    params.encrypt_metadata = false;
    let mut writer = configure_disable_writer(&mut pdf);
    writer.set_encryption_parameters(params);
    writer.set_static_aes_iv(true);
    writer.set_output_memory().expect("install memory output");
    writer
        .write()
        .expect("write cleartext metadata encryption route");
    let bytes = writer.get_buffer().expect("take encrypted output");

    assert!(bytes
        .windows(metadata_payload.len())
        .any(|part| part == metadata_payload));
    assert!(!bytes
        .windows(secret_payload.len())
        .any(|part| part == secret_payload));
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

struct RecordingPipeline {
    events: ProviderEvents,
    bytes: Rc<RefCell<Vec<u8>>>,
    finishes: Rc<Cell<usize>>,
    fail_finish: Option<usize>,
}

impl Pipeline for RecordingPipeline {
    fn identifier(&self) -> &str {
        "task-6 recording output pipeline"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.events.record("sink-write");
        self.bytes.borrow_mut().extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        let count = self.finishes.get() + 1;
        self.finishes.set(count);
        self.events.record(format!("sink-finish:{count}"));
        if self.fail_finish == Some(count) {
            return Err(PipelineError::runtime(
                "task-6 output segment finish failure",
            ));
        }
        Ok(())
    }
}

#[test]
fn fallback_finishes_each_stream_segment_then_the_document_after_eof() {
    let events = ProviderEvents(Rc::new(RefCell::new(Vec::new())));
    let bytes = Rc::new(RefCell::new(Vec::new()));
    let finishes = Rc::new(Cell::new(0));
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
    writer.set_extra_header_text("% force fallback lifecycle");
    writer
        .set_output_pipeline(RecordingPipeline {
            events: events.clone(),
            bytes: Rc::clone(&bytes),
            finishes: Rc::clone(&finishes),
            fail_finish: None,
        })
        .expect("install recording pipeline");
    writer.write().expect("fallback pipeline write succeeds");

    assert_eq!(
        finishes.get(),
        3,
        "two stream segments plus document finish"
    );
    assert!(bytes.borrow().ends_with(b"%%EOF\n"));
    assert_eq!(
        events.snapshot().last().map(String::as_str),
        Some("sink-finish:3")
    );
}

#[test]
fn fallback_segment_finish_failure_stops_later_providers() {
    let events = ProviderEvents(Rc::new(RefCell::new(Vec::new())));
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
    writer.set_extra_header_text("% force fallback finish failure");
    writer
        .set_output_pipeline(RecordingPipeline {
            events: events.clone(),
            bytes: Rc::new(RefCell::new(Vec::new())),
            finishes: Rc::new(Cell::new(0)),
            fail_finish: Some(1),
        })
        .expect("install failing pipeline");
    let error = writer.write().expect_err("first segment finish must fail");

    assert!(error
        .to_string()
        .contains("task-6 output segment finish failure"));
    let events = events.snapshot();
    assert!(events.iter().any(|event| event == "provider:A"));
    assert!(
        !events.iter().any(|event| event == "provider:B"),
        "events: {events:?}"
    );
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
    let mut parsed = Pdf::open(Cursor::new(memory_bytes.clone()))
        .expect("streamed xref offsets must reopen the completed PDF");
    parsed
        .root_handle()
        .expect("streamed xref must resolve the written Catalog");

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

fn decode_hex_16(value: &[u8]) -> [u8; 16] {
    assert_eq!(value.len(), 32);
    let mut decoded = [0; 16];
    for (index, pair) in value.chunks_exact(2).enumerate() {
        let digit = |byte: u8| match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => panic!("non-hex deterministic ID byte: {byte}"),
        };
        decoded[index] = (digit(pair[0]) << 4) | digit(pair[1]);
    }
    decoded
}

fn deterministic_id1(bytes: &[u8]) -> ([u8; 16], usize) {
    let marker = b"/ID [";
    let marker_start = bytes
        .windows(marker.len())
        .position(|part| part == marker)
        .expect("deterministic ID marker");
    let cutoff = marker_start + marker.len() - 1;
    let first_end = bytes[cutoff + 1..]
        .iter()
        .position(|byte| *byte == b'>')
        .map(|offset| cutoff + 1 + offset)
        .expect("permanent ID terminator");
    assert_eq!(bytes[first_end + 1], b'<');
    let second_start = first_end + 2;
    let second_end = bytes[second_start..]
        .iter()
        .position(|byte| *byte == b'>')
        .map(|offset| second_start + offset)
        .expect("changing ID terminator");
    (decode_hex_16(&bytes[second_start..second_end]), cutoff)
}

fn expected_deterministic_id1(prefix: &[u8], info_suffix: &[u8]) -> [u8; 16] {
    let first = md5::Md5::digest(prefix);
    let mut seed = Vec::new();
    for byte in first {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        seed.push(HEX[(byte >> 4) as usize]);
        seed.push(HEX[(byte & 0x0f) as usize]);
    }
    seed.extend_from_slice(b" QPDF ");
    seed.extend_from_slice(info_suffix);
    let seed = &seed[..seed
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(seed.len())];
    md5::Md5::digest(seed).into()
}

#[test]
fn fallback_classic_deterministic_id_includes_xref_and_stops_after_open_bracket() {
    let mut pdf = minimal_pdf();
    pdf.trailer()
        .replace_key(
            b"/Info",
            ObjectHandle::dictionary(vec![
                (
                    b"/A".to_vec(),
                    ObjectHandle::string(b"before\0after".to_vec()),
                ),
                (b"/B".to_vec(), ObjectHandle::string(b"excluded".to_vec())),
            ]),
        )
        .expect("install deterministic Info seed");
    let mut writer = configure_disable_writer(&mut pdf);
    writer.set_static_id(false);
    writer.set_deterministic_id(true);
    writer.set_extra_header_text("% force classic deterministic fallback");
    writer.set_output_memory().expect("install memory output");
    writer
        .write()
        .expect("write classic deterministic fallback");
    let bytes = writer.get_buffer().expect("take deterministic output");

    let (actual, cutoff) = deterministic_id1(&bytes);
    let expected = expected_deterministic_id1(&bytes[..=cutoff], b" before\0after excluded");
    assert_eq!(actual, expected);
    let xref = bytes.windows(5).position(|part| part == b"xref\n").unwrap();
    assert!(xref < cutoff, "classic xref must precede the ID cutoff");
    assert_ne!(
        actual,
        expected_deterministic_id1(&bytes[..xref], b" before\0after excluded"),
        "classic xref bytes must contribute to the digest"
    );
}

#[test]
fn fallback_xref_stream_deterministic_id_excludes_the_stream_payload() {
    let mut pdf = minimal_pdf();
    let mut writer = configure_disable_writer(&mut pdf);
    writer.set_static_id(false);
    writer.set_deterministic_id(true);
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_extra_header_text("% force xref-stream deterministic fallback");
    writer.set_output_memory().expect("install memory output");
    writer
        .write()
        .expect("write xref-stream deterministic fallback");
    let bytes = writer.get_buffer().expect("take deterministic output");

    let (actual, cutoff) = deterministic_id1(&bytes);
    assert_eq!(actual, expected_deterministic_id1(&bytes[..=cutoff], b""));
    let payload_start = bytes[cutoff + 1..]
        .windows(b"stream\n".len())
        .position(|part| part == b"stream\n")
        .map(|offset| cutoff + 1 + offset + b"stream\n".len())
        .expect("xref stream payload follows ID dictionary entry");
    assert!(payload_start > cutoff);
    assert_ne!(
        actual,
        expected_deterministic_id1(&bytes, b""),
        "xref-stream payload and identifier bytes must remain after the digest cutoff"
    );
}

const LARGE_TRAILER_VALUE_BYTES: usize = 512 * 1024;
const DIRECT_METADATA_BUFFER_ALLOWANCE: usize = 128 * 1024;
const OBJSTM_MEMBER_COUNT: usize = 64;
const OBJSTM_MEMBER_PAYLOAD_BYTES: usize = 16 * 1024;
const OBJSTM_SINGLE_BUFFER_OVERHEAD: usize = 256 * 1024;

fn measure_direct_catalog_peak(padding: usize, mode: ObjectStreamMode, qdf: bool) -> usize {
    let mut pdf = minimal_pdf();
    let pages = pdf.get_object_handle(flpdf::ObjectRef::new(2, 0));
    pdf.trailer()
        .replace_key(
            b"/Root",
            ObjectHandle::dictionary(vec![
                (b"/Type".to_vec(), ObjectHandle::name(b"Catalog".to_vec())),
                (b"/Pages".to_vec(), pages),
                (
                    b"/Padding".to_vec(),
                    ObjectHandle::string(vec![b'c'; padding]),
                ),
            ]),
        )
        .expect("install direct Catalog");

    start_allocation_measurement_after_fixture_baseline();
    let mut writer = configure_disable_writer(&mut pdf);
    writer.set_object_stream_mode(mode);
    writer.set_qdf_mode(qdf);
    writer
        .set_output_writer(DiscardWriter)
        .expect("install discard output");
    writer.write().expect("direct Catalog write succeeds");
    finish_allocation_measurement()
}

fn measure_custom_trailer_peak(padding: usize, mode: ObjectStreamMode, qdf: bool) -> usize {
    let mut pdf = minimal_pdf();
    pdf.trailer()
        .replace_key(
            b"/Custom",
            ObjectHandle::dictionary(vec![(
                b"/Padding".to_vec(),
                ObjectHandle::string(vec![b't'; padding]),
            )]),
        )
        .expect("install custom trailer value");

    start_allocation_measurement_after_fixture_baseline();
    let mut writer = configure_disable_writer(&mut pdf);
    writer.set_object_stream_mode(mode);
    writer.set_qdf_mode(qdf);
    writer
        .set_output_writer(DiscardWriter)
        .expect("install discard output");
    writer.write().expect("custom trailer write succeeds");
    finish_allocation_measurement()
}

#[test]
fn direct_catalog_metadata_does_not_become_a_complete_local_buffer() {
    let baseline = measure_direct_catalog_peak(0, ObjectStreamMode::Disable, false);
    let large =
        measure_direct_catalog_peak(LARGE_TRAILER_VALUE_BYTES, ObjectStreamMode::Disable, false);

    assert!(
        large <= baseline + DIRECT_METADATA_BUFFER_ALLOWANCE,
        "direct Catalog serialization retained a body-sized buffer: baseline={baseline}, large={large}, allowance={DIRECT_METADATA_BUFFER_ALLOWANCE}"
    );
}

#[test]
fn direct_catalog_metadata_is_live_in_an_xref_stream() {
    let baseline = measure_direct_catalog_peak(0, ObjectStreamMode::Generate, false);
    let large =
        measure_direct_catalog_peak(LARGE_TRAILER_VALUE_BYTES, ObjectStreamMode::Generate, false);

    assert!(
        large <= baseline + DIRECT_METADATA_BUFFER_ALLOWANCE,
        "direct Catalog xref-stream serialization retained a body-sized buffer: baseline={baseline}, large={large}, allowance={DIRECT_METADATA_BUFFER_ALLOWANCE}"
    );
}

#[test]
fn custom_trailer_metadata_does_not_become_a_complete_local_buffer() {
    let baseline = measure_custom_trailer_peak(0, ObjectStreamMode::Generate, false);
    let large =
        measure_custom_trailer_peak(LARGE_TRAILER_VALUE_BYTES, ObjectStreamMode::Generate, false);

    assert!(
        large <= baseline + DIRECT_METADATA_BUFFER_ALLOWANCE,
        "custom trailer serialization retained a body-sized buffer: baseline={baseline}, large={large}, allowance={DIRECT_METADATA_BUFFER_ALLOWANCE}"
    );
}

#[test]
fn custom_trailer_metadata_is_live_in_a_classic_xref_table() {
    let baseline = measure_custom_trailer_peak(0, ObjectStreamMode::Disable, false);
    let large =
        measure_custom_trailer_peak(LARGE_TRAILER_VALUE_BYTES, ObjectStreamMode::Disable, false);

    assert!(
        large <= baseline + DIRECT_METADATA_BUFFER_ALLOWANCE,
        "custom trailer table serialization retained a body-sized buffer: baseline={baseline}, large={large}, allowance={DIRECT_METADATA_BUFFER_ALLOWANCE}"
    );
}

fn measure_generated_objstm_peak(qdf: bool) -> usize {
    let mut pdf = minimal_pdf();
    let root = pdf.root_handle().expect("resolve live Catalog");
    let mut members = Vec::with_capacity(OBJSTM_MEMBER_COUNT);
    for index in 0..OBJSTM_MEMBER_COUNT {
        let member = pdf
            .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
                b"/Payload".to_vec(),
                ObjectHandle::string(vec![b'o'; OBJSTM_MEMBER_PAYLOAD_BYTES]),
            )]))
            .expect("create ObjStm member");
        members.push(member);
        root.replace_key(
            format!("/ObjStmMember{index:02}").as_bytes(),
            members[index].clone(),
        )
        .expect("attach ObjStm member");
    }

    start_allocation_measurement_after_fixture_baseline();
    let mut writer = configure_disable_writer(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_qdf_mode(qdf);
    writer
        .set_output_writer(DiscardWriter)
        .expect("install discard output");
    writer.write().expect("generated ObjStm write succeeds");
    finish_allocation_measurement()
}

#[test]
fn generated_objstm_does_not_copy_the_complete_container_payload() {
    let peak = measure_generated_objstm_peak(false);
    let payload_bytes = OBJSTM_MEMBER_COUNT * OBJSTM_MEMBER_PAYLOAD_BYTES;

    assert!(
        peak <= payload_bytes.saturating_mul(2) + OBJSTM_SINGLE_BUFFER_OVERHEAD,
        "generated ObjStm retained a second full container payload: peak={peak}, payload={payload_bytes}, overhead={OBJSTM_SINGLE_BUFFER_OVERHEAD}"
    );
}

#[test]
fn generated_qdf_objstm_does_not_copy_the_complete_container_payload() {
    let peak = measure_generated_objstm_peak(true);
    let payload_bytes = OBJSTM_MEMBER_COUNT * OBJSTM_MEMBER_PAYLOAD_BYTES;

    assert!(
        peak <= payload_bytes.saturating_mul(2) + OBJSTM_SINGLE_BUFFER_OVERHEAD,
        "generated QDF ObjStm retained a second full container payload: peak={peak}, payload={payload_bytes}, overhead={OBJSTM_SINGLE_BUFFER_OVERHEAD}"
    );
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

const SMALL_OBJECT_QUEUE_SIZE: usize = 128;
const LARGE_OBJECT_QUEUE_SIZE: usize = 2048;
const OBJECT_QUEUE_MAP_GROWTH_BOUND: usize = 512 * 1024;

fn measure_object_queue_peak(object_count: usize) -> (usize, usize) {
    let mut pdf = minimal_pdf();
    let root = pdf.root_handle().expect("resolve live Catalog");
    for index in 0..object_count {
        let object = pdf
            .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
                b"/Value".to_vec(),
                ObjectHandle::integer(index as i64),
            )]))
            .expect("create queue object");
        root.replace_key(format!("/QueueObject{index:04}").as_bytes(), object)
            .expect("attach queue object");
    }

    start_allocation_measurement_after_fixture_baseline();
    let mut writer = configure_disable_writer(&mut pdf);
    writer.set_qdf_mode(true);
    writer
        .set_output_writer(DiscardWriter)
        .expect("install discard output");
    writer.write().expect("queue object rewrite succeeds");
    let peak = finish_allocation_measurement();
    let allocations = finish_allocation_count();
    (peak, allocations)
}

#[test]
fn plain_writer_map_lookup_does_not_retain_per_object_renumber_snapshots() {
    let (small_peak, small_allocations) = measure_object_queue_peak(SMALL_OBJECT_QUEUE_SIZE);
    let (large_peak, large_allocations) = measure_object_queue_peak(LARGE_OBJECT_QUEUE_SIZE);
    assert!(
        large_peak.saturating_sub(small_peak) <= OBJECT_QUEUE_MAP_GROWTH_BOUND,
        "per-object renumber snapshots grew with the queue map: small={small_peak}, large={large_peak}, bound={OBJECT_QUEUE_MAP_GROWTH_BOUND}"
    );
    assert!(
        large_allocations <= small_allocations.saturating_mul(32),
        "per-object renumber snapshots caused excessive allocation traffic: small={small_allocations}, large={large_allocations}"
    );
}

const SMALL_LINEARIZED_DICTIONARY_COUNT: usize = 64;
const LARGE_LINEARIZED_DICTIONARY_COUNT: usize = 512;
// The live qpdf-shaped dictionary walk should keep growth close to the
// per-entry child/key work rather than retaining a complete map clone for
// every emitted object. The pre-cutover complete-map walk measured about
// 45.5k additional allocations for this 64 -> 512 dictionary growth. The
// post-cutover measurement is about 22.6k on this workload; keep a
// margin for allocator noise while staying below the pre-cutover ~32.5k.
const LINEARIZED_DICTIONARY_ALLOCATION_GROWTH_BOUND: usize = 25_000;

fn measure_linearized_dictionary_allocations(dictionary_count: usize) -> usize {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
    ))
    .expect("open one-page linearization fixture");
    let root = pdf.root_handle().expect("resolve live Catalog");
    for index in 0..dictionary_count {
        let dictionary = ObjectHandle::dictionary(
            (0..8)
                .map(|key| {
                    (
                        format!("/SnapshotKey{key:02}").into_bytes(),
                        ObjectHandle::integer((index + key) as i64),
                    )
                })
                .collect(),
        );
        let object = pdf
            .make_indirect_from_object_handle(dictionary)
            .expect("create linearization dictionary");
        root.replace_key(format!("/LinearizedSnapshot{index:04}").as_bytes(), object)
            .expect("attach linearization dictionary");
    }

    start_allocation_measurement_after_fixture_baseline();
    let mut writer = configure_disable_writer(&mut pdf);
    writer.set_linearization(true);
    writer
        .set_output_writer(DiscardWriter)
        .expect("install discard output");
    writer.write().expect("linearized writer succeeds");
    finish_allocation_count()
}

#[test]
fn linearized_object_walk_does_not_clone_a_complete_dictionary_map_per_object() {
    let small = measure_linearized_dictionary_allocations(SMALL_LINEARIZED_DICTIONARY_COUNT);
    let large = measure_linearized_dictionary_allocations(LARGE_LINEARIZED_DICTIONARY_COUNT);
    let growth = large.saturating_sub(small);

    assert!(
        growth <= LINEARIZED_DICTIONARY_ALLOCATION_GROWTH_BOUND,
        "linearized object walk retained complete dictionary snapshots: small={small}, large={large}, growth={growth}, bound={LINEARIZED_DICTIONARY_ALLOCATION_GROWTH_BOUND}"
    );
}

#[test]
fn linearized_stream_emission_uses_the_length_override_owner() {
    let source = include_str!("../src/linearization/writer.rs");
    assert!(
        source.contains(
            "unparse_stream_body_with_qpdf_obj_gen_map_and_removed_with_options_and_length"
        ),
        "linearized stream emission must use the writer-owned length override primitive"
    );
    assert!(
        source.contains("let (stream_dict, data, dictionary_options)"),
        "linearized stream emission must retain the source dictionary handle instead of rebuilding it"
    );
}

//! What the canonical linearization route costs in allocated bytes, measured.
//!
//! The file installs a `#[global_allocator]` that records allocation sizes on
//! the measuring thread only, so libtest's concurrent tests cannot sample each
//! other's windows.

use flpdf::{ObjectHandle, ObjectStreamMode, Pdf, PdfWriter};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::{self, Cursor, Write};

std::thread_local! {
    static ALLOCATION_TRACKING: Cell<bool> = const { Cell::new(false) };
    static LIVE_BYTES: Cell<usize> = const { Cell::new(0) };
    static PEAK_LIVE_BYTES: Cell<usize> = const { Cell::new(0) };
}

struct LiveAllocationTracker;

fn record_allocation(size: usize) {
    let _ = ALLOCATION_TRACKING.try_with(|tracking| {
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
    let _ = ALLOCATION_TRACKING.try_with(|tracking| {
        if tracking.get() {
            LIVE_BYTES.with(|live| live.set(live.get().saturating_sub(size)));
        }
    });
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

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let reallocated = unsafe { System.realloc(pointer, layout, new_size) };
        if !reallocated.is_null() {
            record_deallocation(layout.size());
            record_allocation(new_size);
        }
        reallocated
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        record_deallocation(layout.size());
    }
}

#[global_allocator]
static ALLOCATOR: LiveAllocationTracker = LiveAllocationTracker;

fn start_allocation_measurement_after_fixture_baseline() {
    LIVE_BYTES.with(|live| live.set(0));
    PEAK_LIVE_BYTES.with(|peak| peak.set(0));
    ALLOCATION_TRACKING.with(|tracking| tracking.set(true));
}

fn finish_allocation_measurement() -> usize {
    ALLOCATION_TRACKING.with(|tracking| tracking.set(false));
    PEAK_LIVE_BYTES.with(Cell::get)
}

/// Swallows the linearized output so the measurement reads what the route
/// retains, not what an in-memory output buffer accumulates.
struct DiscardWriter;

impl Write for DiscardWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn one_page_fixture() -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
    ))
    .expect("open the one-page linearization fixture")
}

fn write_linearized(pdf: &mut Pdf<Cursor<Vec<u8>>>) {
    let mut writer = PdfWriter::new(pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_compress_streams(false);
    writer.set_static_id(true);
    writer.set_linearization(true);
    writer
        .set_output_writer(DiscardWriter)
        .expect("install the discard output");
    writer.write().expect("linearized write succeeds");
}

/// Peak live bytes during one linearized write of a document carrying
/// `object_count` extra indirect dictionaries.
///
/// The objects are deliberately tiny so the per-object bookkeeping the route
/// keeps — the renumber map, the cross-reference offsets, the object-user
/// sets — dominates the number instead of the payload they carry.
fn linearized_peak_for_object_count(object_count: usize) -> usize {
    let mut pdf = attach_small_objects(object_count);
    start_allocation_measurement_after_fixture_baseline();
    write_linearized(&mut pdf);
    finish_allocation_measurement()
}

fn attach_small_objects(object_count: usize) -> Pdf<Cursor<Vec<u8>>> {
    let mut pdf = one_page_fixture();
    let root = pdf.root_handle().expect("resolve the live Catalog");
    for index in 0..object_count {
        let object = pdf
            .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
                b"/V".to_vec(),
                ObjectHandle::integer(index as i64),
            )]))
            .expect("create a per-object bookkeeping entry");
        root.replace_key(format!("/Obj{index:05}").as_bytes(), object)
            .expect("attach the object to the Catalog");
    }
    pdf
}

fn linearized_peak_for_padding(padding: usize) -> usize {
    let mut pdf = one_page_fixture();
    let root = pdf.root_handle().expect("resolve the live Catalog");
    let payload = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Payload".to_vec(),
            ObjectHandle::string(vec![b'p'; padding]),
        )]))
        .expect("create the padded object");
    root.replace_key(b"/Padded", payload)
        .expect("attach the padded object");

    start_allocation_measurement_after_fixture_baseline();
    write_linearized(&mut pdf);
    finish_allocation_measurement()
}

const SMALL_OBJECT_COUNT: usize = 256;
const LARGE_OBJECT_COUNT: usize = 4096;

/// Peak live bytes one extra indirect object is allowed to add.
///
/// # Where the bound comes from
///
/// Each number below is the measured slope of peak live bytes between
/// [`SMALL_OBJECT_COUNT`] and [`LARGE_OBJECT_COUNT`], and each regression was
/// applied to the production route and measured, not estimated:
///
/// - canonical route: **666.6** bytes per object.
/// - the complete renumber map cloned once before pass 1 (`let mut
///   local_renumber = renumber.to_owned()` in place of the move):
///   **750.9** bytes per object.
/// - one tree-backed set allocated per object in the reverse object-user map
///   (the compact small-set representation disabled): **906.6** bytes per
///   object — one extra allocation per object, which this bound rejects with
///   28% to spare.
///
/// The bound sits between the canonical slope and the cheaper of the two
/// regressions — 6.5% of headroom above what the route costs today, 5.4%
/// below the first regression it has to reject. It discriminates rather than
/// accommodates: any second complete per-object map costs more than the 43
/// bytes per object of slack.
///
/// The counter sums `Layout` sizes, so the slope is independent of the system
/// allocator and of the optimization level; the debug and release figures were
/// byte-identical when this was measured.
const PEAK_BYTES_PER_OBJECT_BOUND: usize = 710;

/// The two-pass route keeps one copy of the per-object bookkeeping, not two.
///
/// qpdf's renumber table is a writer member assigned once while objects are
/// enqueued (`QPDFWriter.cc:1067,1107`); both linearization passes then read
/// it (`QPDFWriter.cc:2664`). Growing the object count here grows every
/// per-object structure the route owns at once, so a second complete copy of
/// any of them shows up as a steeper slope — which is what this measures,
/// instead of reading the production source for the spelling of a particular
/// clone.
#[test]
fn linearized_route_keeps_no_second_complete_per_object_map() {
    let small = linearized_peak_for_object_count(SMALL_OBJECT_COUNT);
    let large = linearized_peak_for_object_count(LARGE_OBJECT_COUNT);
    let extra_objects = LARGE_OBJECT_COUNT - SMALL_OBJECT_COUNT;
    let growth = large.saturating_sub(small);

    assert!(
        growth <= PEAK_BYTES_PER_OBJECT_BOUND * extra_objects,
        "linearization retained a second per-object map: small={small}, large={large}, \
         growth={growth} over {extra_objects} objects ({:.1} bytes per object), \
         bound={PEAK_BYTES_PER_OBJECT_BOUND}",
        growth as f64 / extra_objects as f64,
    );
}

const LINEARIZED_PADDING_BYTES: usize = 4 * 1024 * 1024;
/// Peak live bytes a four-megabyte payload is allowed to add.
///
/// Measured: the canonical route peaks at **10,181** bytes whether the payload
/// is 0, 1 MiB or 4 MiB — the body reaches the output sink without ever being
/// resident. Making the pass metadata own its body (a `Vec<u8>` of the written
/// length, the shape the pass output carried before it was streamed) peaks at
/// **8,401,584** bytes for the 4 MiB payload, because pass 1's body and the
/// final pass's body are alive at the same time.
///
/// A quarter of a megabyte of slack is far above the flat measurement and far
/// below one body, let alone two.
const PASS_BODY_RESIDENCY_ALLOWANCE: usize = 256 * 1024;

/// Neither layout pass keeps the document body it has written.
///
/// qpdf's first pass writes into a discard filter unless a pass-1 filename was
/// requested (`QPDFWriter.cc:2664-2672`), keeping only the measurements the
/// second pass needs. Peak residency is therefore a function of the object
/// count, not of how many bytes those objects carry. Scaling the payload
/// rather than the object count isolates that: the per-object bookkeeping is
/// identical in both measurements below.
#[test]
fn linearized_passes_retain_no_document_sized_body() {
    let unpadded = linearized_peak_for_padding(0);
    let padded = linearized_peak_for_padding(LINEARIZED_PADDING_BYTES);

    assert!(
        padded <= unpadded + PASS_BODY_RESIDENCY_ALLOWANCE,
        "a layout pass retained the document body: unpadded={unpadded}, \
         padded={padded} for a {LINEARIZED_PADDING_BYTES}-byte payload, \
         allowance={PASS_BODY_RESIDENCY_ALLOWANCE}"
    );
}

/// The measurements above are only meaningful if the payload is written.
///
/// A linearized write that silently dropped the padded object would report a
/// flat peak for the same reason a streaming one does.
///
/// This one deliberately does not arm the counter: it allocates the payload
/// and the output buffer outright, and arming it here would measure the
/// opposite of what the tests above establish.
#[test]
fn the_padded_payload_reaches_the_linearized_output() {
    let mut pdf = one_page_fixture();
    let root = pdf.root_handle().expect("resolve the live Catalog");
    let payload = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Payload".to_vec(),
            ObjectHandle::string(vec![b'p'; LINEARIZED_PADDING_BYTES]),
        )]))
        .expect("create the padded object");
    root.replace_key(b"/Padded", payload)
        .expect("attach the padded object");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_compress_streams(false);
    writer.set_static_id(true);
    writer.set_linearization(true);
    writer
        .set_output_memory()
        .expect("install the memory output");
    writer.write().expect("linearized write succeeds");
    let written = writer.get_buffer().expect("linearized output buffer");

    assert!(
        written.len() > LINEARIZED_PADDING_BYTES,
        "the padded payload never reached the output: {} bytes written for a \
         {LINEARIZED_PADDING_BYTES}-byte payload",
        written.len()
    );
}

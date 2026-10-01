//! What the canonical linearization route costs in allocated bytes, measured.
//!
//! The file installs a `#[global_allocator]` that records allocation sizes on
//! the measuring thread only, so libtest's concurrent tests cannot sample each
//! other's windows.

use flpdf::{
    EncryptParams, ObjectHandle, ObjectRef, ObjectStreamMode, PageDocumentHelper, PageInput, Pdf,
    PdfWriter,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeSet;
use std::io::{self, Cursor, Write};
use std::rc::Rc;

std::thread_local! {
    static ALLOCATION_TRACKING: Cell<bool> = const { Cell::new(false) };
    static LIVE_BYTES: Cell<usize> = const { Cell::new(0) };
    static PEAK_LIVE_BYTES: Cell<usize> = const { Cell::new(0) };
    static TOTAL_ALLOCATED_BYTES: Cell<usize> = const { Cell::new(0) };
    static TOTAL_ALLOCATION_COUNT: Cell<usize> = const { Cell::new(0) };
}

struct LiveAllocationTracker;

fn record_allocation(size: usize) {
    let _ = ALLOCATION_TRACKING.try_with(|tracking| {
        if tracking.get() {
            TOTAL_ALLOCATED_BYTES.with(|total| {
                total.set(total.get().saturating_add(size));
            });
            TOTAL_ALLOCATION_COUNT.with(|total| {
                total.set(total.get().saturating_add(1));
            });
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
    TOTAL_ALLOCATED_BYTES.with(|total| total.set(0));
    TOTAL_ALLOCATION_COUNT.with(|total| total.set(0));
    ALLOCATION_TRACKING.with(|tracking| tracking.set(true));
}

#[derive(Clone, Copy, Debug)]
struct AllocationStats {
    peak_live_bytes: usize,
    total_allocated_bytes: usize,
    total_allocation_count: usize,
}

fn finish_allocation_measurement() -> AllocationStats {
    ALLOCATION_TRACKING.with(|tracking| tracking.set(false));
    AllocationStats {
        peak_live_bytes: PEAK_LIVE_BYTES.with(Cell::get),
        total_allocated_bytes: TOTAL_ALLOCATED_BYTES.with(Cell::get),
        total_allocation_count: TOTAL_ALLOCATION_COUNT.with(Cell::get),
    }
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

fn write_linearized(pdf: &mut Pdf<Cursor<Vec<u8>>>, encrypted: bool) {
    let mut writer = PdfWriter::new(pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_compress_streams(false);
    writer.set_static_id(true);
    writer.set_linearization(true);
    if encrypted {
        writer.set_encryption_parameters(EncryptParams::v4_aes128(b"user", b"owner"));
    }
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
    write_linearized(&mut pdf, false);
    finish_allocation_measurement().peak_live_bytes
}

fn linearized_allocation_stats_for_object_count(object_count: usize) -> AllocationStats {
    let mut pdf = attach_small_objects(object_count);
    start_allocation_measurement_after_fixture_baseline();
    write_linearized(&mut pdf, false);
    finish_allocation_measurement()
}

fn encrypted_linearized_allocation_stats_for_object_count(object_count: usize) -> AllocationStats {
    let mut pdf = attach_small_objects(object_count);
    start_allocation_measurement_after_fixture_baseline();
    write_linearized(&mut pdf, true);
    finish_allocation_measurement()
}

fn linearized_allocation_stats_for_stream_count(stream_count: usize) -> AllocationStats {
    let mut pdf = one_page_fixture();
    let root = pdf.root_handle().expect("resolve the live Catalog");
    for index in 0..stream_count {
        let stream = pdf
            .new_stream_with_data(Rc::new(b"q\nQ\n".to_vec()))
            .expect("create a per-stream linearization input");
        root.replace_key(format!("/LinearizedStream{index:04}").as_bytes(), stream)
            .expect("attach the stream for linearization");
    }
    start_allocation_measurement_after_fixture_baseline();
    write_linearized(&mut pdf, false);
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

/// A page-scaled workload: every page owns a resource dictionary with
/// `objects_per_page` distinct indirect children. This makes page ownership
/// grow on both axes instead of attaching every synthetic object to the
/// Catalog, which exercises only object-count scaling.
fn page_private_object_fixture(page_count: usize, objects_per_page: usize) -> Pdf<Cursor<Vec<u8>>> {
    assert!(page_count > 0, "the fixture needs at least one page");
    let mut pdf = one_page_fixture();
    let template_page = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect("read the template page")
        .into_iter()
        .next()
        .expect("one-page fixture has a page");
    for _ in 1..page_count {
        PageDocumentHelper::new(&mut pdf)
            .add_page(PageInput::target(template_page.clone()), false)
            .expect("duplicate the template page");
    }

    let pages = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect("read duplicated pages");
    assert_eq!(pages.len(), page_count);
    for (page_index, page) in pages.into_iter().enumerate() {
        let mut xobjects = Vec::with_capacity(objects_per_page);
        for object_index in 0..objects_per_page {
            let value = page_index
                .saturating_mul(objects_per_page)
                .saturating_add(object_index);
            let object = pdf
                .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
                    b"/Value".to_vec(),
                    ObjectHandle::integer(value as i64),
                )]))
                .expect("create page-private resource object");
            xobjects.push((format!("/Private{object_index:04}").into_bytes(), object));
        }
        let resources = ObjectHandle::dictionary(vec![(
            b"/XObject".to_vec(),
            ObjectHandle::dictionary(xobjects),
        )]);
        page.replace_key(b"/Resources", resources)
            .expect("attach page-private resources");
    }
    pdf
}

fn linearized_allocated_bytes_for_page_private_objects(
    page_count: usize,
    objects_per_page: usize,
) -> usize {
    let mut pdf = page_private_object_fixture(page_count, objects_per_page);
    start_allocation_measurement_after_fixture_baseline();
    write_linearized(&mut pdf, false);
    finish_allocation_measurement().total_allocated_bytes
}

fn linearized_allocated_bytes_for_page_private_objects_with_duplicate_page_sets(
    page_count: usize,
    objects_per_page: usize,
) -> usize {
    let mut pdf = page_private_object_fixture(page_count, objects_per_page);
    start_allocation_measurement_after_fixture_baseline();
    // Calibration for the retired `Vec<BTreeSet<ObjectRef>>` route: all
    // non-first pages own one set containing their page-private resources.
    // Keep it live across the write so the measurement proves this workload
    // and bound expose the extra page-by-object container.
    let page_private_sets: Vec<BTreeSet<ObjectRef>> = (1..page_count)
        .map(|page| {
            // The old per-page list includes the page object itself as well
            // as its private resource children.
            (0..=objects_per_page)
                .map(|object| ObjectRef::new((page * objects_per_page + object) as u32 + 10_000, 0))
                .collect()
        })
        .collect();
    write_linearized(&mut pdf, false);
    std::hint::black_box(&page_private_sets);
    finish_allocation_measurement().total_allocated_bytes
}

const SMALL_PAGE_COUNT: usize = 2;
const LARGE_PAGE_COUNT: usize = 8;
const SMALL_PAGE_OBJECT_COUNT: usize = 2;
const LARGE_PAGE_OBJECT_COUNT: usize = 16;
// The clean route measured 2,174 allocated bytes per added page-private
// object and 2,176 bytes per page/object edge on the page-count axis. A
// synthetic duplicate `Vec<BTreeSet<ObjectRef>>` measured 2,200 and 2,211,
// respectively. Each bound sits between the current route and that retired
// ownership table, with the allocation sizes taken from `Layout`.
const PAGE_PRIVATE_OBJECT_ALLOCATED_BYTES_BOUND: usize = 2_187;
const PAGE_PRIVATE_PAGE_ALLOCATED_BYTES_BOUND: usize = 2_193;

#[test]
fn linearization_page_private_cost_scales_with_pages_and_objects() {
    let small_object_bytes = linearized_allocated_bytes_for_page_private_objects(
        LARGE_PAGE_COUNT,
        SMALL_PAGE_OBJECT_COUNT,
    );
    let large_object_bytes = linearized_allocated_bytes_for_page_private_objects(
        LARGE_PAGE_COUNT,
        LARGE_PAGE_OBJECT_COUNT,
    );
    let object_edges = LARGE_PAGE_COUNT * (LARGE_PAGE_OBJECT_COUNT - SMALL_PAGE_OBJECT_COUNT);
    let object_slope = large_object_bytes.saturating_sub(small_object_bytes) / object_edges;

    let small_page_bytes = linearized_allocated_bytes_for_page_private_objects(
        SMALL_PAGE_COUNT,
        LARGE_PAGE_OBJECT_COUNT,
    );
    let large_page_bytes = linearized_allocated_bytes_for_page_private_objects(
        LARGE_PAGE_COUNT,
        LARGE_PAGE_OBJECT_COUNT,
    );
    let page_edges = (LARGE_PAGE_COUNT - SMALL_PAGE_COUNT) * LARGE_PAGE_OBJECT_COUNT;
    let page_slope = large_page_bytes.saturating_sub(small_page_bytes) / page_edges;

    let duplicate_object_slope =
        (linearized_allocated_bytes_for_page_private_objects_with_duplicate_page_sets(
            LARGE_PAGE_COUNT,
            LARGE_PAGE_OBJECT_COUNT,
        ) - linearized_allocated_bytes_for_page_private_objects_with_duplicate_page_sets(
            LARGE_PAGE_COUNT,
            SMALL_PAGE_OBJECT_COUNT,
        )) / object_edges;
    let duplicate_page_slope =
        (linearized_allocated_bytes_for_page_private_objects_with_duplicate_page_sets(
            LARGE_PAGE_COUNT,
            LARGE_PAGE_OBJECT_COUNT,
        ) - linearized_allocated_bytes_for_page_private_objects_with_duplicate_page_sets(
            SMALL_PAGE_COUNT,
            LARGE_PAGE_OBJECT_COUNT,
        )) / page_edges;
    assert!(
        object_slope <= PAGE_PRIVATE_OBJECT_ALLOCATED_BYTES_BOUND,
        "linearized page-private object cost grew beyond the measured per-edge bound: \
         object_slope={object_slope}, bound={PAGE_PRIVATE_OBJECT_ALLOCATED_BYTES_BOUND}"
    );
    assert!(
        page_slope <= PAGE_PRIVATE_PAGE_ALLOCATED_BYTES_BOUND,
        "linearized page-count growth exceeded the measured per-edge bound: \
         page_slope={page_slope}, bound={PAGE_PRIVATE_PAGE_ALLOCATED_BYTES_BOUND}"
    );
    assert!(
        duplicate_object_slope > PAGE_PRIVATE_OBJECT_ALLOCATED_BYTES_BOUND,
        "the page/object measurement must reject a duplicate page-private set: \
         duplicate_object_slope={duplicate_object_slope}, \
         bound={PAGE_PRIVATE_OBJECT_ALLOCATED_BYTES_BOUND}"
    );
    assert!(
        duplicate_page_slope > PAGE_PRIVATE_PAGE_ALLOCATED_BYTES_BOUND,
        "the page-count measurement must reject a duplicate page-private set: \
         duplicate_page_slope={duplicate_page_slope}, \
         bound={PAGE_PRIVATE_PAGE_ALLOCATED_BYTES_BOUND}"
    );
}

const SMALL_LINEARIZED_STREAM_COUNT: usize = 2;
const LARGE_LINEARIZED_STREAM_COUNT: usize = 64;
const LINEARIZED_STREAM_EXTRA_BYTES_PER_STREAM_BOUND: usize = 4_450;
const LINEARIZED_STREAM_EXTRA_ALLOCS_PER_STREAM_BOUND: usize = 90;

/// Linearized stream emission scales with the number of streams, not with a
/// reconstructed copy of every source dictionary. The same-count ordinary
/// object workload is subtracted to remove the writer's per-object baseline.
#[test]
fn linearized_stream_dictionary_cost_scales_with_stream_count() {
    let stream_small = linearized_allocation_stats_for_stream_count(SMALL_LINEARIZED_STREAM_COUNT);
    let stream_large = linearized_allocation_stats_for_stream_count(LARGE_LINEARIZED_STREAM_COUNT);
    let object_small = linearized_allocation_stats_for_object_count(SMALL_LINEARIZED_STREAM_COUNT);
    let object_large = linearized_allocation_stats_for_object_count(LARGE_LINEARIZED_STREAM_COUNT);
    let additional_streams = LARGE_LINEARIZED_STREAM_COUNT - SMALL_LINEARIZED_STREAM_COUNT;
    let stream_only_bytes = stream_large
        .total_allocated_bytes
        .saturating_sub(stream_small.total_allocated_bytes)
        .saturating_sub(
            object_large
                .total_allocated_bytes
                .saturating_sub(object_small.total_allocated_bytes),
        );
    let stream_only_allocations = stream_large
        .total_allocation_count
        .saturating_sub(stream_small.total_allocation_count)
        .saturating_sub(
            object_large
                .total_allocation_count
                .saturating_sub(object_small.total_allocation_count),
        );
    let bytes_per_stream = stream_only_bytes / additional_streams;
    let allocations_per_stream = stream_only_allocations / additional_streams;

    assert!(
        bytes_per_stream <= LINEARIZED_STREAM_EXTRA_BYTES_PER_STREAM_BOUND,
        "linearized stream emission retained a per-stream dictionary copy: \
         bytes_per_stream={bytes_per_stream}, bound={LINEARIZED_STREAM_EXTRA_BYTES_PER_STREAM_BOUND}"
    );
    assert!(
        allocations_per_stream <= LINEARIZED_STREAM_EXTRA_ALLOCS_PER_STREAM_BOUND,
        "linearized stream emission allocated a per-stream dictionary copy: \
         allocations_per_stream={allocations_per_stream}, \
         bound={LINEARIZED_STREAM_EXTRA_ALLOCS_PER_STREAM_BOUND}"
    );
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
    write_linearized(&mut pdf, false);
    finish_allocation_measurement().peak_live_bytes
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
/// - qpdf-shaped writer-owned state: **697.3** bytes per object.
/// - the complete renumber map cloned once before pass 1 (`let mut
///   local_renumber = renumber.to_owned()` in place of the move):
///   **750.9** bytes per object.
/// - one tree-backed set allocated per object in the reverse object-user map
///   (the compact small-set representation disabled): **906.6** bytes per
///   object — one extra allocation per object, which this bound rejects with
///   28% to spare.
///
/// The bound sits between the current slope and the cheaper of the two
/// regressions — 1.8% of headroom above the shared-state route, 5.4% below
/// the first regression it has to reject. It discriminates rather than
/// accommodates: a second complete per-object map costs more than the
/// 12 bytes per object of slack.
///
/// The counter sums `Layout` sizes, so the slope is independent of the system
/// allocator and of the optimization level; the debug and release figures were
/// byte-identical when this was measured.
const PEAK_BYTES_PER_OBJECT_BOUND: usize = 710;

/// The two-pass route keeps one copy of the per-object bookkeeping, not two.
///
/// qpdf's renumber, xref, and length tables are writer members assigned while
/// objects are emitted (`QPDFWriter.cc:1041-1053,1067,1107`); both
/// linearization passes use that state (`QPDFWriter.cc:2664`). Growing the
/// object count here grows every per-object structure the route owns at once,
/// so a second complete copy of any of them shows up as a steeper slope —
/// which is what this measures, instead of reading production source for the
/// spelling of a particular clone.
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

const TOTAL_ALLOCATED_BYTES_PER_OBJECT_BOUND: usize = 3760;
const TOTAL_ALLOCATIONS_PER_OBJECT_BOUND: f64 = 36.0;
const ENCRYPTED_TOTAL_ALLOCATED_BYTES_PER_OBJECT_BOUND: usize = 3600;
const ENCRYPTED_TOTAL_ALLOCATIONS_PER_OBJECT_BOUND: f64 = 50.0;

/// Before writer-state reuse, the same 256-to-4096 object workload measured
/// 3,921.2 allocated bytes and 37.39 allocation calls per object. The gates
/// below require a material reduction after the per-object maps are removed.
#[test]
fn linearized_body_emission_reuses_writer_owned_allocation_state() {
    let small = linearized_allocation_stats_for_object_count(SMALL_OBJECT_COUNT);
    let large = linearized_allocation_stats_for_object_count(LARGE_OBJECT_COUNT);
    let extra_objects = LARGE_OBJECT_COUNT - SMALL_OBJECT_COUNT;
    let allocated_bytes_per_object = large
        .total_allocated_bytes
        .saturating_sub(small.total_allocated_bytes)
        / extra_objects;
    let allocations_per_object = large
        .total_allocation_count
        .saturating_sub(small.total_allocation_count) as f64
        / extra_objects as f64;
    assert!(
        allocated_bytes_per_object <= TOTAL_ALLOCATED_BYTES_PER_OBJECT_BOUND,
        "linearized body emission retained transient per-object writer state: \
         small={small:?}, large={large:?}, allocated_bytes_per_object={allocated_bytes_per_object}, \
         bound={TOTAL_ALLOCATED_BYTES_PER_OBJECT_BOUND}"
    );
    assert!(
        allocations_per_object <= TOTAL_ALLOCATIONS_PER_OBJECT_BOUND,
        "linearized body emission still allocates per-object adapter maps: \
         small={small:?}, large={large:?}, allocations_per_object={allocations_per_object:.2}, \
         bound={TOTAL_ALLOCATIONS_PER_OBJECT_BOUND:.2}"
    );
}

/// The shared-state encrypted route measures 3,581 bytes and 49.39 allocation
/// calls per object. A diagnostic mutation that cloned the file key in each
/// object emitter measured 3,613 bytes per object and fails this bound.
#[test]
fn encrypted_linearized_body_emission_reuses_writer_file_key_state() {
    let small = encrypted_linearized_allocation_stats_for_object_count(SMALL_OBJECT_COUNT);
    let large = encrypted_linearized_allocation_stats_for_object_count(LARGE_OBJECT_COUNT);
    let extra_objects = LARGE_OBJECT_COUNT - SMALL_OBJECT_COUNT;
    let allocated_bytes_per_object = large
        .total_allocated_bytes
        .saturating_sub(small.total_allocated_bytes)
        / extra_objects;
    let allocations_per_object = large
        .total_allocation_count
        .saturating_sub(small.total_allocation_count) as f64
        / extra_objects as f64;
    assert!(
        allocated_bytes_per_object <= ENCRYPTED_TOTAL_ALLOCATED_BYTES_PER_OBJECT_BOUND,
        "encrypted linearized emission allocated a per-object file-key clone: \
         small={small:?}, large={large:?}, allocated_bytes_per_object={allocated_bytes_per_object}, \
         bound={ENCRYPTED_TOTAL_ALLOCATED_BYTES_PER_OBJECT_BOUND}"
    );
    assert!(
        allocations_per_object <= ENCRYPTED_TOTAL_ALLOCATIONS_PER_OBJECT_BOUND,
        "encrypted linearized emission allocated per-object writer state: \
         small={small:?}, large={large:?}, allocations_per_object={allocations_per_object:.2}, \
         bound={ENCRYPTED_TOTAL_ALLOCATIONS_PER_OBJECT_BOUND:.2}"
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

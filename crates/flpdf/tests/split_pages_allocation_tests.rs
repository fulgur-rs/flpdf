//! What the canonical job split path costs in allocated bytes, measured rather
//! than argued.
//!
//! The file installs a `#[global_allocator]` that tracks live bytes and reads
//! the high-water mark across one `QPDFJob::split_pages` call, with the
//! caller's own buffer already allocated so that only what the call adds is
//! counted.
//!
//! **The file holds exactly one `#[test]`, deliberately.** libtest runs the
//! tests in a binary concurrently on separate threads, and the counters below
//! are process-global, so a second test would sample the first one's
//! allocations and vice versa.

use flpdf::job::{QPDFJob, SplitPageOptions};
use flpdf::Pdf;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Bytes currently allocated through this allocator, counted from process
/// start so a `dealloc` can never see a byte that was not counted in.
static LIVE: AtomicUsize = AtomicUsize::new(0);
/// High-water mark of [`LIVE`], reset at the start of each measurement.
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// Forwards everything to [`System`] and records the sizes on the way through.
///
/// Only `alloc` and `dealloc` are implemented: `realloc` and `alloc_zeroed`
/// have default `GlobalAlloc` implementations that go through this `alloc` and
/// this `dealloc`, so a growing `Vec` is accounted for without any code here to
/// account for it with.
struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(live, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Peak bytes live during `call`, minus what was live when it started.
///
/// The fixture is built before this is entered on purpose: assembling a
/// multi-megabyte document allocates one copy of it by itself, and counting
/// that would drown the number being read.
fn peak_growth_of(call: impl FnOnce()) -> usize {
    let baseline = LIVE.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    call();
    PEAK.load(Ordering::Relaxed).saturating_sub(baseline)
}

/// A two-page document padded to a few megabytes by one stream object that
/// nothing references.
///
/// The padding is deliberately unreachable from the page tree:
/// `QPDFJob::split_pages`
/// walks the pages, so a document whose bulk sat inside a *referenced* object
/// would allocate that bulk again as a resolved value, for reasons that have
/// nothing to do with what is being measured.
fn padded_two_page_pdf(padding: usize) -> Vec<u8> {
    let mut pdf: Vec<u8> = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");

    let mut offsets = Vec::new();
    let push = |pdf: &mut Vec<u8>, offsets: &mut Vec<u64>, body: &[u8]| {
        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(body);
    };

    push(
        &mut pdf,
        &mut offsets,
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
    );
    push(
        &mut pdf,
        &mut offsets,
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
    );
    push(
        &mut pdf,
        &mut offsets,
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );
    push(
        &mut pdf,
        &mut offsets,
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );

    let mut filler = format!("5 0 obj\n<< /Length {padding} >>\nstream\n").into_bytes();
    filler.resize(filler.len() + padding, b'x');
    filler.extend_from_slice(b"\nendstream\nendobj\n");
    push(&mut pdf, &mut offsets, &filler);

    let xref_start = pdf.len() as u64;
    let size = offsets.len() + 1;
    let mut xref = format!("xref\n0 {size}\n0000000000 65535 f \n");
    for offset in &offsets {
        xref.push_str(&format!("{offset:010} 00000 n \n"));
    }
    pdf.extend_from_slice(xref.as_bytes());
    pdf.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    // Not cosmetic: this `Vec` grew by doubling, so its allocation is close to
    // twice the document. Every number below is stated as a multiple of
    // `src.len()`, and leaving the spare capacity in makes the buffer that is
    // handed over cost almost twice what the same document costs anywhere else
    // in the measurement.
    pdf.shrink_to_fit();
    pdf
}

/// The source the caller hands over is never resident a second time.
///
/// `QPDFJob::split_pages` keeps its source document alive while each fresh
/// output is written. The owned `Vec` is moved into that document, so the
/// caller's buffer is not copied before the job starts.
///
/// # Where the bound comes from
///
/// The call is not free of document-sized buffers, and pretending otherwise
/// would make this a tuned number rather than a derived one. Two exist, both
/// found by capturing a backtrace at every allocation of at least `src.len()`
/// bytes rather than by reading the code and hoping:
///
/// - `Pdf::open_mem_owned` reads the input into its owned reader while loading
///   the xref (`xref::load_xref_state_with_repair` → `Read::read_to_end`).
/// - Each chunk is emitted through the canonical writer while the source and
///   output allocations coexist; the output buffer grows from empty and ends
///   at the next power of two — **two** documents' worth for a document that
///   is not one already.
///
/// Those two coexist, so three documents' worth of growth is expected and
/// measured (3.01× at the time of writing). A copy of the handed-over source
/// would be a fourth, so the bound is four — a threshold that discriminates
/// rather than accommodates.
///
/// **The caller's own buffer is deliberately outside the window.** It is
/// allocated before the probe is armed, so it sits in the baseline and does not
/// count — which is the point: after the handover it is not a second buffer at
/// all, it *is* the shared source.
///
/// # What this cannot see
///
/// A copy that frees the original immediately — `Arc::<[u8]>::from(vec)`, the
/// shape this one is easiest to be turned back into — costs a memcpy of the
/// whole document but no sustained residency, so it does not move this number.
/// The direct owned-source route is covered by this measurement's resident
/// allocation bound.
#[test]
fn split_pages_keeps_no_second_copy_of_the_source_it_is_handed() {
    let src = padded_two_page_pdf(4 * 1024 * 1024);
    let src_len = src.len();
    let tmpdir = tempfile::tempdir().expect("tmpdir");
    let template = tmpdir.path().join("out.pdf");

    let peak = peak_growth_of(|| {
        let mut pdf = Pdf::open_mem_owned(src).expect("source should parse");
        let mut job = QPDFJob::new();
        job.split_pages(&mut pdf, SplitPageOptions::new(1, &template))
            .expect("split should succeed");
    });

    assert!(
        tmpdir.path().join("out-1.pdf").exists() && tmpdir.path().join("out-2.pdf").exists(),
        "the measurement is only meaningful if the split actually ran"
    );
    assert!(
        peak < 4 * src_len,
        "splitting a {src_len}-byte document peaked {peak} bytes above the \
         caller's own buffer ({:.2}x the document); the reader's and writer's \
         own buffers account for three, and a copy of the handed-over source \
         would be the fourth",
        peak as f64 / src_len as f64,
    );
}

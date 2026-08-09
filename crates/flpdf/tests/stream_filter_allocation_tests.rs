//! Measure scalar `/DecodeParms` expansion without sharing a process-wide
//! allocator with unrelated integration tests.

use flpdf::{filters, Dictionary, Object};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

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

fn peak_growth_of(call: impl FnOnce()) -> usize {
    let baseline = LIVE.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    call();
    PEAK.load(Ordering::Relaxed).saturating_sub(baseline)
}

#[test]
fn scalar_decode_parms_do_not_expand_a_large_dictionary_per_filter() {
    let filter_count = 16;
    let large_value_size = 1 << 20;
    let filter = Object::Array(
        (0..filter_count)
            .map(|_| Object::Name(b"FlateDecode".to_vec()))
            .collect(),
    );
    let mut decode_parms = Dictionary::new();
    decode_parms.insert("Ignored", Object::String(vec![b'x'; large_value_size]));
    let mut dictionary = Dictionary::new();
    dictionary.insert("Filter", filter);
    dictionary.insert("DecodeParms", Object::Dictionary(decode_parms));

    let peak = peak_growth_of(|| {
        let _ = filters::decode_stream_data(&dictionary, b"not zlib");
    });

    assert!(
        peak < large_value_size * 4,
        "scalar DecodeParms peak allocation was {peak} bytes"
    );
}

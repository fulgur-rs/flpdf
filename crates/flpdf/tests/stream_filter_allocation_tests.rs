//! Measure `/DecodeParms` expansion without sharing a process-wide
//! allocator with unrelated integration tests.

#[path = "support/filter_handles.rs"]
mod filter_handles;

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

fn flate_filter_chain(count: usize) -> Object {
    Object::Array(
        (0..count)
            .map(|_| Object::Name(b"FlateDecode".to_vec()))
            .collect(),
    )
}

fn ignored_decode_parms(size: usize) -> Object {
    let mut decode_parms = Dictionary::new();
    decode_parms.insert("Ignored", Object::String(vec![b'x'; size]));
    Object::Dictionary(decode_parms)
}

#[test]
fn decode_parms_do_not_expand_large_values_per_filter() {
    let filter_count = 16;
    let large_value_size = 1 << 20;
    let mut dictionary = Dictionary::new();
    dictionary.insert("Filter", flate_filter_chain(filter_count));
    dictionary.insert("DecodeParms", ignored_decode_parms(large_value_size));
    let dictionary_handle = filter_handles::dictionary(&dictionary);

    let scalar_peak = peak_growth_of(|| {
        let _ = filters::decode_stream_data(&dictionary_handle, b"not zlib");
    });

    assert!(
        scalar_peak < large_value_size * 4,
        "scalar DecodeParms peak allocation was {scalar_peak} bytes"
    );

    let mut aligned_dictionary = Dictionary::new();
    aligned_dictionary.insert("Filter", flate_filter_chain(filter_count));
    aligned_dictionary.insert(
        "DecodeParms",
        Object::Array(
            (0..filter_count)
                .map(|_| ignored_decode_parms(large_value_size))
                .collect(),
        ),
    );
    let aligned_dictionary_handle = filter_handles::dictionary(&aligned_dictionary);

    let aligned_peak = peak_growth_of(|| {
        let _ = filters::decode_stream_data(&aligned_dictionary_handle, b"not zlib");
    });

    assert!(
        aligned_peak < large_value_size * 4,
        "aligned DecodeParms peak allocation was {aligned_peak} bytes"
    );
}

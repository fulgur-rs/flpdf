//! Allocation regressions for qpdf-shaped array/dictionary predicates.
//!
//! qpdf's `isArray` and `isDictionary` inspect only the resolved value type;
//! they do not copy the child collection. Keep this binary to one test so its
//! process-global allocator counter cannot observe another test concurrently.

use flpdf::ObjectHandle;
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn allocations_during(call: impl FnOnce()) -> usize {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    call();
    ALLOCATIONS.load(Ordering::Relaxed)
}

#[test]
fn wide_shape_predicates_do_not_allocate_child_snapshots() {
    let array = ObjectHandle::array((0..512).map(ObjectHandle::integer).collect());
    let dictionary = ObjectHandle::dictionary(
        (0..512)
            .map(|index| {
                (
                    format!("/K{index}").into_bytes(),
                    ObjectHandle::integer(index),
                )
            })
            .collect(),
    );

    let array_allocations = allocations_during(|| {
        for _ in 0..16 {
            assert!(black_box(&array).try_is_array().expect("array predicate"));
        }
    });
    let dictionary_allocations = allocations_during(|| {
        for _ in 0..16 {
            assert!(black_box(&dictionary)
                .try_is_dictionary()
                .expect("dictionary predicate"));
        }
    });

    assert_eq!(array_allocations, 0, "array predicate cloned its children");
    assert_eq!(
        dictionary_allocations, 0,
        "dictionary predicate cloned its entries"
    );
}

use flpdf::ObjectHandle;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let allocation = unsafe { System.alloc(layout) };
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        allocation
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn allocations_during(call: impl FnOnce()) -> usize {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    call();
    COUNTING.store(false, Ordering::Relaxed);
    ALLOCATIONS.load(Ordering::Relaxed)
}

#[test]
fn direct_scalar_uses_at_most_three_allocations() {
    let allocations = allocations_during(|| {
        let scalar = ObjectHandle::integer(7);
        std::hint::black_box(scalar);
    });

    assert!(
        allocations <= 3,
        "direct scalar used {allocations} allocations"
    );
}

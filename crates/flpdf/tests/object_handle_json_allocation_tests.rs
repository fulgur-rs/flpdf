//! Allocation regressions for qpdf-shaped JSON container traversal.
//!
//! qpdf writes arrays and dictionaries by walking their live containers.  A
//! JSON writer must not clone every container's child storage just to emit it.
//! Keep this binary to one test so its process-global allocator counter cannot
//! observe another test concurrently.

use flpdf::{json::Json, ObjectHandle, ObjectRef, Pdf, Pipeline, PipelineResult};
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

struct CountingSink {
    bytes: usize,
}

impl Pipeline for CountingSink {
    fn identifier(&self) -> &str {
        "json allocation sink"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.bytes += data.len();
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

fn allocations_for(handle: &ObjectHandle) -> usize {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    let mut sink = CountingSink { bytes: 0 };
    black_box(handle)
        .write_json(2, &mut sink, true, 0)
        .expect("JSON serialization");
    assert!(sink.bytes > 0);
    ALLOCATIONS.load(Ordering::Relaxed)
}

fn allocations_during(call: impl FnOnce()) -> usize {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    call();
    ALLOCATIONS.load(Ordering::Relaxed)
}

#[test]
fn json_writer_does_not_clone_container_storage() {
    const ENTRIES: usize = 256;
    let dictionary = ObjectHandle::dictionary(
        (0..ENTRIES)
            .map(|index| {
                (
                    format!("/K{index}").into_bytes(),
                    ObjectHandle::integer(index as i64),
                )
            })
            .collect(),
    );

    let allocations = allocations_for(&dictionary);
    assert!(
        allocations <= 4 * ENTRIES + 16,
        "JSON dictionary traversal cloned container storage: {allocations} allocations for {ENTRIES} entries"
    );

    let safe_key_allocations = allocations_for(&dictionary);
    assert!(
        safe_key_allocations <= ENTRIES + 64,
        "JSON dictionary traversal encoded safe keys through temporary Vecs: {safe_key_allocations} allocations for {ENTRIES} entries"
    );

    const ITEMS: usize = 256;
    let flat = ObjectHandle::array(
        (0..ITEMS)
            .map(|index| ObjectHandle::integer(index as i64))
            .collect(),
    );
    let nested = ObjectHandle::array(
        (0..ITEMS)
            .map(|index| ObjectHandle::array(vec![ObjectHandle::integer(index as i64)]))
            .collect(),
    );

    let flat_allocations = allocations_for(&flat);
    let nested_allocations = allocations_for(&nested);
    assert!(
        flat_allocations <= 64,
        "JSON integer serialization used temporary heap strings: {flat_allocations} allocations for {ITEMS} integers"
    );
    assert!(
        nested_allocations.saturating_sub(flat_allocations) <= ITEMS / 2,
        "nested array traversal cloned each child Vec: flat={flat_allocations}, nested={nested_allocations}"
    );

    let nested_dictionaries = ObjectHandle::array(
        (0..ITEMS)
            .map(|index| {
                ObjectHandle::dictionary(vec![(
                    b"/A".to_vec(),
                    ObjectHandle::integer(index as i64),
                )])
            })
            .collect(),
    );
    let nested_dictionary_allocations = allocations_for(&nested_dictionaries);
    assert!(
        nested_dictionary_allocations <= 64,
        "JSON dictionary cursor allocated key buffers per container: {nested_dictionary_allocations} allocations for {ITEMS} dictionaries"
    );

    let strings = ObjectHandle::array(
        (0..ITEMS)
            .map(|index| ObjectHandle::string(format!("text-{index}").into_bytes()))
            .collect(),
    );
    let string_allocations = allocations_for(&strings);
    assert!(
        string_allocations <= 64,
        "JSON string serialization copied printable payloads: {string_allocations} allocations for {ITEMS} strings"
    );

    let mut pdf = Pdf::empty().expect("empty PDF");
    let references = ObjectHandle::array(
        (1..=ITEMS)
            .map(|number| pdf.get_object_handle(ObjectRef::new(number as u32, 0)))
            .collect(),
    );
    let reference_allocations = allocations_for(&references);
    assert!(
        reference_allocations <= 64,
        "JSON indirect-reference serialization used temporary heap strings: {reference_allocations} allocations for {ITEMS} references"
    );
    drop(references);
    drop(pdf);

    const KEYS: usize = 256;
    let indentation_allocations = allocations_during(|| {
        let mut first = true;
        let mut sink = CountingSink { bytes: 0 };
        for _ in 0..KEYS {
            Json::write_next(&mut sink, &mut first, 2).expect("indentation write");
        }
    });
    assert!(
        indentation_allocations <= 64,
        "JSON indentation allocated a temporary Vec per entry: {indentation_allocations} allocations for {KEYS} entries"
    );
    let key_allocations = allocations_during(|| {
        let mut first = true;
        let mut sink = CountingSink { bytes: 0 };
        for _ in 0..KEYS {
            Json::write_dictionary_key(&mut sink, &mut first, b"obj:5000 0 R", 2)
                .expect("dictionary key write");
        }
    });
    assert!(
        key_allocations <= indentation_allocations + 64,
        "JSON dictionary-key framing allocated a temporary Vec per key: indentation={indentation_allocations}, keys={key_allocations}"
    );
}

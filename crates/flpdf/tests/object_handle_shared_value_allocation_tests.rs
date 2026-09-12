use flpdf::ObjectHandle;
use std::alloc::{GlobalAlloc, Layout, System};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);

const SIZE_CLASS_COUNT: usize = 6;
static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);
static LIVE_SIZE_CLASS_BYTES: [AtomicIsize; SIZE_CLASS_COUNT] = [
    AtomicIsize::new(0),
    AtomicIsize::new(0),
    AtomicIsize::new(0),
    AtomicIsize::new(0),
    AtomicIsize::new(0),
    AtomicIsize::new(0),
];
static SIZE_CLASS_ALLOCATIONS: [AtomicUsize; SIZE_CLASS_COUNT] = [
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
];
static SIZE_CLASS_BYTES: [AtomicUsize; SIZE_CLASS_COUNT] = [
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
];

#[derive(Clone, Copy, Debug)]
struct AllocationMeasurement {
    allocations: usize,
    allocated_bytes: usize,
    size_class_allocations: [usize; SIZE_CLASS_COUNT],
    size_class_bytes: [usize; SIZE_CLASS_COUNT],
    live_bytes: isize,
    live_size_class_bytes: [isize; SIZE_CLASS_COUNT],
}

#[derive(Clone, Copy, Debug)]
struct LiveAllocationSnapshot {
    bytes: isize,
    size_class_bytes: [isize; SIZE_CLASS_COUNT],
}

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let allocation = unsafe { System.alloc(layout) };
        if !allocation.is_null() {
            record_allocation(layout.size());
        }
        allocation
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let allocation = unsafe { System.alloc_zeroed(layout) };
        if !allocation.is_null() {
            record_allocation(layout.size());
        }
        allocation
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let allocation = unsafe { System.realloc(ptr, layout, new_size) };
        if !allocation.is_null() {
            record_reallocation(layout.size(), new_size);
        }
        allocation
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        record_deallocation(layout.size());
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn size_class(size: usize) -> usize {
    match size {
        0..=64 => 0,
        65..=128 => 1,
        129..=256 => 2,
        257..=512 => 3,
        513..=1024 => 4,
        _ => 5,
    }
}

fn adjust_live_bytes(size: usize, direction: isize) {
    let bytes = isize::try_from(size).expect("GlobalAlloc layout must fit in isize");
    LIVE_BYTES.fetch_add(bytes * direction, Ordering::Relaxed);
    LIVE_SIZE_CLASS_BYTES[size_class(size)].fetch_add(bytes * direction, Ordering::Relaxed);
}

fn record_allocation(size: usize) {
    adjust_live_bytes(size, 1);
    if !COUNTING.load(Ordering::Relaxed) {
        return;
    }

    record_allocation_event(size);
}

fn record_allocation_event(size: usize) {
    let class = size_class(size);
    ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    ALLOCATED_BYTES.fetch_add(size, Ordering::Relaxed);
    SIZE_CLASS_ALLOCATIONS[class].fetch_add(1, Ordering::Relaxed);
    SIZE_CLASS_BYTES[class].fetch_add(size, Ordering::Relaxed);
}

fn record_deallocation(size: usize) {
    adjust_live_bytes(size, -1);
}

fn record_reallocation(old_size: usize, new_size: usize) {
    adjust_live_bytes(old_size, -1);
    // GlobalAlloc::realloc requires a non-zero new size from its caller. Keep
    // the zero-size branch explicit so a successful zero-size extension does
    // not create a phantom live size class or event allocation.
    if new_size != 0 {
        adjust_live_bytes(new_size, 1);
        if COUNTING.load(Ordering::Relaxed) {
            record_allocation_event(new_size);
        }
    }
}

fn live_snapshot() -> LiveAllocationSnapshot {
    let mut size_class_bytes = [0; SIZE_CLASS_COUNT];
    for class in 0..SIZE_CLASS_COUNT {
        size_class_bytes[class] = LIVE_SIZE_CLASS_BYTES[class].load(Ordering::Relaxed);
    }
    LiveAllocationSnapshot {
        bytes: LIVE_BYTES.load(Ordering::Relaxed),
        size_class_bytes,
    }
}

fn reset_measurement() -> LiveAllocationSnapshot {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    for class in 0..SIZE_CLASS_COUNT {
        SIZE_CLASS_ALLOCATIONS[class].store(0, Ordering::Relaxed);
        SIZE_CLASS_BYTES[class].store(0, Ordering::Relaxed);
    }
    live_snapshot()
}

fn measure_construction<T>(call: impl FnOnce() -> T) -> (T, AllocationMeasurement) {
    let baseline = reset_measurement();
    COUNTING.store(true, Ordering::Relaxed);
    let value = call();
    COUNTING.store(false, Ordering::Relaxed);
    let after = live_snapshot();

    let mut size_class_allocations = [0; SIZE_CLASS_COUNT];
    let mut size_class_bytes = [0; SIZE_CLASS_COUNT];
    let mut live_size_class_bytes = [0; SIZE_CLASS_COUNT];
    for class in 0..SIZE_CLASS_COUNT {
        size_class_allocations[class] = SIZE_CLASS_ALLOCATIONS[class].load(Ordering::Relaxed);
        size_class_bytes[class] = SIZE_CLASS_BYTES[class].load(Ordering::Relaxed);
        live_size_class_bytes[class] =
            after.size_class_bytes[class] - baseline.size_class_bytes[class];
    }

    (
        value,
        AllocationMeasurement {
            allocations: ALLOCATIONS.load(Ordering::Relaxed),
            allocated_bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
            size_class_allocations,
            size_class_bytes,
            live_bytes: after.bytes - baseline.bytes,
            live_size_class_bytes,
        },
    )
}

fn report_measurement(label: &str, measurement: AllocationMeasurement) {
    assert_eq!(
        measurement.allocations,
        measurement.size_class_allocations.iter().sum::<usize>(),
        "{label}: size-class allocation totals must equal all allocations"
    );
    assert_eq!(
        measurement.allocated_bytes,
        measurement.size_class_bytes.iter().sum::<usize>(),
        "{label}: size-class byte totals must equal allocated bytes"
    );
    assert_eq!(
        measurement.live_bytes,
        measurement.live_size_class_bytes.iter().sum::<isize>(),
        "{label}: live size-class byte totals must equal live bytes"
    );
    eprintln!(
        "shared-value allocation {label}: allocations={} allocated_bytes={} live_bytes={} class_allocations=[<=64:{},65-128:{},129-256:{},257-512:{},513-1024:{},>1024:{}] class_bytes=[<=64:{},65-128:{},129-256:{},257-512:{},513-1024:{},>1024:{}] live_class_bytes=[<=64:{},65-128:{},129-256:{},257-512:{},513-1024:{},>1024:{}]",
        measurement.allocations,
        measurement.allocated_bytes,
        measurement.live_bytes,
        measurement.size_class_allocations[0],
        measurement.size_class_allocations[1],
        measurement.size_class_allocations[2],
        measurement.size_class_allocations[3],
        measurement.size_class_allocations[4],
        measurement.size_class_allocations[5],
        measurement.size_class_bytes[0],
        measurement.size_class_bytes[1],
        measurement.size_class_bytes[2],
        measurement.size_class_bytes[3],
        measurement.size_class_bytes[4],
        measurement.size_class_bytes[5],
        measurement.live_size_class_bytes[0],
        measurement.live_size_class_bytes[1],
        measurement.live_size_class_bytes[2],
        measurement.live_size_class_bytes[3],
        measurement.live_size_class_bytes[4],
        measurement.live_size_class_bytes[5],
    );
}

#[test]
fn direct_scalar_uses_at_most_three_allocations() {
    const WIDE_ITEMS: usize = 128;
    const STREAM_BYTES: usize = 64 * 1024;

    let (scalar, direct_scalar) = measure_construction(|| ObjectHandle::integer(7));
    std::hint::black_box(&scalar);
    report_measurement("direct-scalar", direct_scalar);
    assert_eq!(scalar.try_get_int_value().unwrap(), 7);

    assert!(
        direct_scalar.allocations <= 3,
        "direct scalar used {} allocations",
        direct_scalar.allocations
    );

    // Allocate fixtures before resetting the counters so every reported number
    // covers construction, not test-data preparation. This integration binary
    // deliberately has one test, keeping its process-global allocator isolated.
    let values = (0..WIDE_ITEMS)
        .map(|value| value as i64)
        .collect::<Vec<_>>();
    let dictionary_keys = (0..WIDE_ITEMS)
        .map(|value| format!("/MeasurementKey{value:03}").into_bytes())
        .collect::<Vec<_>>();
    let stream_data = Rc::new(vec![0x5a; STREAM_BYTES]);
    let alias_source = ObjectHandle::integer(7);

    let (wide_array, scalar_heavy) = measure_construction(|| {
        ObjectHandle::array(values.iter().copied().map(ObjectHandle::integer).collect())
    });
    std::hint::black_box(&wide_array);
    report_measurement("wide-array-scalar-heavy", scalar_heavy);
    assert!(wide_array.try_is_array().unwrap());
    assert_eq!(wide_array.try_get_array_n_items().unwrap(), WIDE_ITEMS);
    assert_eq!(
        wide_array
            .try_get_array_item(0)
            .unwrap()
            .try_get_int_value()
            .unwrap(),
        0
    );
    assert_eq!(
        wide_array
            .try_get_array_item((WIDE_ITEMS - 1) as i64)
            .unwrap()
            .try_get_int_value()
            .unwrap(),
        (WIDE_ITEMS - 1) as i64
    );

    let (wide_dictionary, dictionary_heavy) = measure_construction(|| {
        ObjectHandle::dictionary(
            dictionary_keys
                .iter()
                .zip(values.iter().copied())
                .map(|(key, value)| (key.clone(), ObjectHandle::integer(value)))
                .collect(),
        )
    });
    std::hint::black_box(&wide_dictionary);
    report_measurement("wide-dictionary", dictionary_heavy);
    assert!(wide_dictionary.try_is_dictionary().unwrap());
    assert_eq!(wide_dictionary.try_get_keys().unwrap().len(), WIDE_ITEMS);
    for (key, value) in dictionary_keys.iter().zip(values.iter().copied()) {
        assert_eq!(
            wide_dictionary
                .try_get_key(key)
                .unwrap()
                .try_get_int_value()
                .unwrap(),
            value
        );
    }

    let (wide_stream, stream_heavy) = measure_construction(|| {
        let dictionary = ObjectHandle::dictionary(
            dictionary_keys
                .iter()
                .zip(values.iter().copied())
                .map(|(key, value)| (key.clone(), ObjectHandle::integer(value)))
                .collect(),
        );
        ObjectHandle::stream(dictionary, Rc::clone(&stream_data))
    });
    std::hint::black_box(&wide_stream);
    report_measurement("wide-stream", stream_heavy);
    let stream_dictionary = wide_stream
        .as_stream_dict()
        .expect("wide value is a stream");
    assert!(stream_dictionary.try_is_dictionary().unwrap());
    assert_eq!(stream_dictionary.try_get_keys().unwrap().len(), WIDE_ITEMS);
    for (key, value) in dictionary_keys.iter().zip(values.iter().copied()) {
        assert_eq!(
            stream_dictionary
                .try_get_key(key)
                .unwrap()
                .try_get_int_value()
                .unwrap(),
            value
        );
    }
    let observed_stream_data = wide_stream
        .as_stream_data()
        .expect("wide stream has prepared data");
    assert!(Rc::ptr_eq(&observed_stream_data, &stream_data));

    let (aliases, alias_measurement) = measure_construction(|| {
        (0..WIDE_ITEMS)
            .map(|_| alias_source.clone())
            .collect::<Vec<_>>()
    });
    std::hint::black_box(&aliases);
    report_measurement("aliases", alias_measurement);
    assert_eq!(aliases.len(), WIDE_ITEMS);
    assert!(aliases
        .iter()
        .all(|alias| alias.is_same_object_as(&alias_source)));
}

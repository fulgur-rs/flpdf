use flpdf::ObjectHandle;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::rc::Rc;

const SIZE_CLASS_COUNT: usize = 6;

thread_local! {
    // libtest's coordinator can allocate even when this binary has one test.
    // Constant, drop-free TLS avoids allocating or registering a destructor
    // from inside GlobalAlloc. Only the measuring thread records events.
    static MEASUREMENT: Cell<Option<AllocationMeasurement>> = const { Cell::new(None) };
}

#[derive(Clone, Copy, Debug)]
struct AllocationMeasurement {
    allocations: usize,
    allocated_bytes: usize,
    size_class_allocations: [usize; SIZE_CLASS_COUNT],
    size_class_bytes: [usize; SIZE_CLASS_COUNT],
    live_bytes: isize,
    live_size_class_bytes: [isize; SIZE_CLASS_COUNT],
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

impl AllocationMeasurement {
    const ZERO: Self = Self {
        allocations: 0,
        allocated_bytes: 0,
        size_class_allocations: [0; SIZE_CLASS_COUNT],
        size_class_bytes: [0; SIZE_CLASS_COUNT],
        live_bytes: 0,
        live_size_class_bytes: [0; SIZE_CLASS_COUNT],
    };

    fn adjust_live_bytes(&mut self, size: usize, direction: isize) {
        // GlobalAlloc layouts fit in isize. These are net bytes allocated and
        // freed on this thread during the window, not process-wide live heap.
        let bytes = size as isize * direction;
        self.live_bytes += bytes;
        self.live_size_class_bytes[size_class(size)] += bytes;
    }

    fn record_allocation(&mut self, size: usize) {
        let class = size_class(size);
        self.allocations += 1;
        self.allocated_bytes += size;
        self.size_class_allocations[class] += 1;
        self.size_class_bytes[class] += size;
        self.adjust_live_bytes(size, 1);
    }
}

fn update_measurement(update: impl FnOnce(&mut AllocationMeasurement)) {
    // Allocator callbacks may also run during thread teardown. If TLS is no
    // longer accessible, leave System's allocation/deallocation unaffected.
    let _ = MEASUREMENT.try_with(|state| {
        if let Some(mut measurement) = state.get() {
            update(&mut measurement);
            state.set(Some(measurement));
        }
    });
}

fn record_allocation(size: usize) {
    update_measurement(|measurement| measurement.record_allocation(size));
}

fn record_deallocation(size: usize) {
    update_measurement(|measurement| measurement.adjust_live_bytes(size, -1));
}

fn record_reallocation(old_size: usize, new_size: usize) {
    update_measurement(|measurement| {
        measurement.adjust_live_bytes(old_size, -1);
        measurement.record_allocation(new_size);
    });
}

struct MeasurementGuard;

impl Drop for MeasurementGuard {
    fn drop(&mut self) {
        MEASUREMENT.with(|state| state.set(None));
    }
}

fn measure_construction<T>(call: impl FnOnce() -> T) -> (T, AllocationMeasurement) {
    MEASUREMENT.with(|state| {
        assert!(state.get().is_none(), "allocation measurements cannot nest");
        state.set(Some(AllocationMeasurement::ZERO));
    });
    // Stop recording on unwind as well as on success.
    let _guard = MeasurementGuard;
    let value = call();
    let measurement = MEASUREMENT.with(|state| state.take().unwrap());
    (value, measurement)
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
fn measurement_excludes_other_threads() {
    use std::sync::atomic::{AtomicBool, Ordering};

    // Barrier/Condvar can lazily allocate on platforms such as macOS. Keep
    // synchronization inside the measuring window allocation-free too.
    fn wait_for(signal: &AtomicBool) {
        while !signal.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
    }

    let start = AtomicBool::new(false);
    let allocated = AtomicBool::new(false);
    let finish = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            wait_for(&start);
            let temporary = std::hint::black_box(Box::new([0_u8; 96]));
            let mut noise = std::hint::black_box(vec![0_u8; 96]);
            noise.resize(2048, 0);
            std::hint::black_box(&noise);
            drop(temporary);
            allocated.store(true, Ordering::Release);
            wait_for(&finish);
            drop(noise);
        });
        let (value, measurement) = measure_construction(|| {
            start.store(true, Ordering::Release);
            wait_for(&allocated);
            std::hint::black_box(Box::new([0_u8; 32]))
        });
        // Release the worker before asserting, including on failure.
        finish.store(true, Ordering::Release);
        worker.join().unwrap();
        std::hint::black_box(&value);
        assert_eq!(measurement.allocations, 1, "{measurement:?}");
        assert_eq!(measurement.allocated_bytes, 32);
        assert_eq!(measurement.live_bytes, 32);
        assert_eq!(measurement.size_class_allocations, [1, 0, 0, 0, 0, 0]);
        assert_eq!(measurement.size_class_bytes, [32, 0, 0, 0, 0, 0]);
        assert_eq!(measurement.live_size_class_bytes, [32, 0, 0, 0, 0, 0]);
    });
}

#[test]
fn measurement_tracks_allocation_reallocation_and_frees() {
    use std::alloc::{alloc, alloc_zeroed, dealloc, handle_alloc_error, realloc};

    let small = Layout::from_size_align(32, 8).unwrap();
    let medium = Layout::from_size_align(96, 8).unwrap();
    let large = Layout::from_size_align(2048, 8).unwrap();
    let fixture_layout = Layout::from_size_align(513, 8).unwrap();
    // Every allocation below is checked and freed with its current layout;
    // the realloc result replaces the original pointer on success.
    unsafe {
        let fixture = alloc(fixture_layout);
        if fixture.is_null() {
            handle_alloc_error(fixture_layout);
        }
        let (value, measurement) = measure_construction(|| {
            let value = alloc(small);
            if value.is_null() {
                handle_alloc_error(small);
            }
            let zeroed = alloc_zeroed(medium);
            if zeroed.is_null() {
                handle_alloc_error(medium);
            }
            let value = realloc(value, small, large.size());
            if value.is_null() {
                handle_alloc_error(large);
            }
            dealloc(zeroed, medium);
            dealloc(fixture, fixture_layout);
            value
        });
        dealloc(value, large);

        assert_eq!(measurement.allocations, 3);
        assert_eq!(measurement.allocated_bytes, 2176);
        assert_eq!(measurement.live_bytes, 1535);
        assert_eq!(measurement.size_class_allocations, [1, 1, 0, 0, 0, 1]);
        assert_eq!(measurement.size_class_bytes, [32, 96, 0, 0, 0, 2048]);
        assert_eq!(measurement.live_size_class_bytes, [0, 0, 0, 0, -513, 2048]);
    }
    let (_, empty) = measure_construction(|| ());
    assert_eq!(empty.allocations, 0);
    assert_eq!(empty.allocated_bytes, 0);
    assert_eq!(empty.live_bytes, 0);
    assert_eq!(empty.size_class_allocations, [0; SIZE_CLASS_COUNT]);
    assert_eq!(empty.size_class_bytes, [0; SIZE_CLASS_COUNT]);
    assert_eq!(empty.live_size_class_bytes, [0; SIZE_CLASS_COUNT]);
}

#[test]
fn measurement_stops_on_unwind() {
    let result = std::panic::catch_unwind(|| measure_construction(|| panic!("measurement probe")));
    assert!(result.is_err());
    let (value, measurement) = measure_construction(|| std::hint::black_box(Box::new([0_u8; 32])));
    std::hint::black_box(&value);
    assert_eq!(measurement.allocations, 1);
    assert_eq!(measurement.allocated_bytes, 32);
    assert_eq!(measurement.live_bytes, 32);
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
    // covers construction, not test-data preparation. Thread-local measurement
    // also excludes allocations and frees from libtest and other tests.
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

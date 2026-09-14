# C11 runtime stream-filter registry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox ("- [ ]") syntax for tracking.

**Goal:** Add qpdf 11.9.0's public process-global runtime stream-filter registry and remove the fixed match as a second production registry.

**Architecture:** Keep ObjectHandle::prepare_stream_filter_plan as the sole ordinary filterability owner. Make the qpdf-shaped filter trait and Rust pipeline ownership handles public, store built-in and custom factories in one safe global registry, and wrap built-ins internally so Flate warning delivery remains private. Verify the observable registry contract against a pinned qpdf C++ probe.

**Tech Stack:** Rust workspace, OnceLock/Mutex/Arc, public Pipeline trait, qpdf 11.9.0 source, C++ API probe, Cargo tests, route/deviation/module-doc checkers.

---

### Task 1: Add the RED public-registry contract

**Files:**
- Create: crates/flpdf/tests/stream_filter_registry_tests.rs
- Modify: crates/flpdf/tests/filters_route_cutover_tests.rs

- [ ] **Step 1: Add a real custom filter integration test**

Create a test-only pipeline and filter that exercise the public API and canonical ObjectHandle decoder:

~~~rust
use flpdf::pipeline::{Pipeline, PipelineError, PipelineRef};
use flpdf::{
    register_stream_filter, DecodeLevel, ObjectHandle, OwnedDecodePipeline, Result, StreamFilter,
};

struct PrefixPipeline<'a> {
    next: PipelineRef<'a>,
}

impl Pipeline for PrefixPipeline<'_> {
    fn identifier(&self) -> &str {
        "registry prefix"
    }

    fn write(&mut self, data: &[u8]) -> std::result::Result<(), PipelineError> {
        self.next.write(b"custom:")?;
        self.next.write(data)
    }

    fn finish(&mut self) -> std::result::Result<(), PipelineError> {
        self.next.finish()
    }
}

struct PrefixFilter;

impl StreamFilter for PrefixFilter {
    fn get_decode_pipeline<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Ok(OwnedDecodePipeline::Stage(Box::new(PrefixPipeline { next })))
    }
}

#[test]
fn registered_filter_is_used_by_the_canonical_object_handle_decoder() {
    register_stream_filter(b"/FlpdfRegistryPrefix", || Ok(PrefixFilter));

    let stream = ObjectHandle::stream(
        ObjectHandle::dictionary(vec![(
            b"Filter".to_vec(),
            ObjectHandle::name(b"FlpdfRegistryPrefix".to_vec()),
        )]),
        std::rc::Rc::new(b"payload".to_vec()),
    );

    assert_eq!(
        stream
            .get_stream_data(DecodeLevel::Generalized)
            .unwrap()
            .as_slice(),
        b"custom:payload"
    );
}
~~~

- [ ] **Step 2: Run the RED test**

Run:

~~~text
cargo test -p flpdf --test stream_filter_registry_tests -- --nocapture
~~~

Expected: compilation fails because stream_filter is private and the public registration/ownership API is absent. Do not add production code before observing this failure.

- [ ] **Step 3: Add the route contract assertions**

Extend filters_route_cutover_tests.rs with source assertions that the public module exports register_stream_filter and StreamFilter, and that the public trait contains get_decode_pipeline rather than exposing the old internal name. Keep the C28/C43 absence assertions unchanged.

### Task 2: Expose the qpdf-shaped filter and ownership boundary

**Files:**
- Modify: crates/flpdf/src/pipeline.rs
- Modify: crates/flpdf/src/stream_filter.rs
- Modify: crates/flpdf/src/lib.rs

- [ ] **Step 1: Expose PipelineRef without changing behavior**

Change PipelineRef<'a> and its Borrowed/Owned variants from crate-private to public. Keep its From implementations and Pipeline delegation byte-for-byte unchanged. Update its documentation to describe only the qpdf non-owning Pipeline* to Rust ownership substitution.

- [ ] **Step 2: Rename the public trait method**

Change the trait method name from decode_pipeline_owned to get_decode_pipeline. Keep the existing default set_decode_params and specialized/lossy defaults. Remove set_warning_callback from the public trait; it is an internal built-in adapter hook and has no qpdf public counterpart.

- [ ] **Step 3: Expose OwnedDecodePipeline**

Make OwnedDecodePipeline and its Stage/NoStage variants public with documentation explaining that the caller owns the returned stage in Rust while qpdf's filter instance owns its shared pipeline. Do not add a partial-result or null sentinel variant.

- [ ] **Step 4: Re-export the public API**

Change lib.rs from crate-private stream_filter to public stream_filter and re-export register_stream_filter, OwnedDecodePipeline, and StreamFilter. Re-export PipelineRef from pipeline at the root only if needed by the public trait ergonomics; retain Pipeline as the existing public qpdf pipeline boundary. Do not expose registry storage, built-in filter structs, or warning adapters.

- [ ] **Step 5: Run the compile checkpoint**

Run:

~~~text
cargo test -p flpdf --test stream_filter_registry_tests --no-run
~~~

Expected: the test imports and trait implementation compile far enough that only register_stream_filter or registry lookup is still unavailable.

### Task 3: Build one built-in/custom registry

**Files:**
- Modify: crates/flpdf/src/stream_filter.rs

- [ ] **Step 1: Add the public factory registration function**

Implement this exact public shape:

~~~rust
pub fn register_stream_filter<F, S>(name: impl AsRef<[u8]>, factory: F)
where
    F: Fn() -> Result<S> + Send + Sync + 'static,
    S: StreamFilter + 'static,
{
    let factory = Arc::new(move || {
        factory().map(|filter| Box::new(filter) as Box<dyn StreamFilter>)
    });
    registry_guard().insert(name.as_ref().to_vec(), FilterFactory::Custom(factory));
}
~~~

The function must store the supplied name unchanged, including whether it has a leading slash, just as qpdf assigns the caller's string as a map key. It must not validate or normalize the name.

- [ ] **Step 2: Add registry storage and lookup**

Add a OnceLock<Mutex<BTreeMap<Vec<u8>, FilterFactory>>>, built-in entries under /Crypt, /FlateDecode, /LZWDecode, /ASCII85Decode, /ASCIIHexDecode, /RunLengthDecode, and /DCTDecode. Define FilterFactory with a built-in constructor function and a cloned custom factory. Recover a poisoned mutex with its contained map; do not invent a new error category.

Lookup must:
1. normalize the internal no-slash PDF name;
2. prepend one slash only for lookup;
3. clone the factory and release the mutex;
4. invoke the factory outside the mutex;
5. return Ok(None) for an unknown key and propagate factory Result errors.

- [ ] **Step 3: Preserve the internal warning hook**

Add a crate-private RegisteredStreamFilter trait mirroring the four qpdf methods plus set_warning_callback. Use a generic BuiltinStreamFilter<T> adapter for filters without a warning hook, a Flate-specific adapter that forwards set_warning_callback to FlateLzwStreamFilter, and a CustomStreamFilter adapter that delegates the public trait and ignores the private hook. Keep all three adapters private.

- [ ] **Step 4: Replace the fixed match**

Change stream_filter_for to return Result<Option<Box<dyn RegisteredStreamFilter>>> and make it read the one registry. Built-in constructors are registry entries, not a separate fallback match. Update every caller and rename built-in get_decode_pipeline implementations.

- [ ] **Step 5: Run the first GREEN test**

Run:

~~~text
cargo test -p flpdf --test stream_filter_registry_tests -- --nocapture
~~~

Expected: the PrefixFilter test passes through ObjectHandle::prepare_stream_filter_plan and get_stream_data.

### Task 4: Integrate planner behavior and prove ordering

**Files:**
- Modify: crates/flpdf/src/object_handle.rs
- Modify: crates/flpdf/tests/stream_filter_registry_tests.rs

- [ ] **Step 1: Use RegisteredStreamFilter in the planner**

Change StreamFilterPlan.filters to Box<dyn RegisteredStreamFilter>. In pipe_stream_data_inner, preserve the current reverse order: install the private warning callback, call get_decode_pipeline, wrap the returned stage, then pipe the source. Do not change decode-level gating, source retry, raw fallback, warning sink, or filterability return values.

- [ ] **Step 2: Add unique-name replacement coverage**

Add a second integration test that registers /FlpdfRegistryReplace twice with two filters producing different prefixes and asserts the second factory is used. Use a unique name so the test does not mutate a built-in used by another test process.

- [ ] **Step 3: Add full DecodeParms coverage**

Add a custom filter that stores whether set_decode_params received a dictionary containing /Marker 7. Register it under /FlpdfRegistryParams, decode a stream with that dictionary, and assert the flag before the test returns. This proves the live ObjectHandle, not a reduced snapshot, crosses the public boundary.

- [ ] **Step 4: Add factory order and failure coverage**

Use a shared test recorder and a filter array [known, unknown, known]. Assert both known factories were called and no DecodeParms value was resolved before the planner returned unfilterable. Add a factory returning Err(Error::Unsupported("factory failure".to_owned())) and assert that error is propagated.

- [ ] **Step 5: Add lifetime coverage**

Have the custom filter and its returned pipeline increment Drop counters. Decode through get_stream_data and assert the filter/pipeline drops occur only after the source has finished. Do not add an unregister API to make this test pass.

### Task 5: Add and run the pinned qpdf API probe

**Files:**
- Create: tests/oracle/qpdf_stream_filter_registry_probe.cc
- Create: scripts/qpdf-stream-filter-registry-probe.sh

- [ ] **Step 1: Implement the C++ probe**

The probe must:
- include qpdf/QPDF.hh, qpdf/QPDFObjectHandle.hh, qpdf/QPDFStreamFilter.hh, and qpdf/Pipeline.hh;
- implement a QPDFStreamFilter with a Pipeline stage that emits a prefix and forwards finish;
- register /FlpdfRegistryPrefix, create a QPDF with newStream("payload"), install /Filter /FlpdfRegistryPrefix, and call getStreamData;
- register the same name again and assert replacement output;
- record factory invocation count, filter destruction, pipeline destruction, and DecodeParms values;
- register /Fl and prove alias lookup still reaches /FlateDecode rather than a /Fl registration;
- use a known/unknown/known filter array and emit factory-construction order before the false filterable result;
- print stable key=value records and exit nonzero on any mismatch.

- [ ] **Step 2: Implement the trusted build wrapper**

Follow scripts/qpdf-objecthandle-uniform-identity-probe.sh: resolve the pinned source with fetch-qpdf-source.sh --print-path, verify commit 3b97c9bd266b7c32ea36d3536e22dab77412886d and clean tracked state, create a narrowly prefixed mktemp build directory, configure/build only pinned libqpdf out of tree, compile the probe against pinned include/libqpdf with an rpath to that build, verify ldd resolves the built libqpdf, run it, and clean only the validated temporary directory.

- [ ] **Step 3: Run the probe**

Run:

~~~text
scripts/qpdf-stream-filter-registry-probe.sh
~~~

Expected: the output records demonstrate qpdf's process-global replacement, alias order, factory order, full DecodeParms delivery, and lifetime. Compare those records to the Rust integration tests before changing implementation details.

### Task 6: Re-anchor route documentation

**Files:**
- Modify: docs/qpdf-correspondence.md
- Modify: docs/qpdf-route-matrix/c-stream-pipeline-encryption.md
- Modify: docs/qpdf-route-matrix/README.md
- Modify: docs/qpdf-route-matrix/tracked-symbols.txt
- Regenerate: docs/qpdf-module-doc-index.md

- [ ] **Step 1: Update C10/C11 rows**

Keep C10 as the built-in lookup owner, now backed by the shared registry. Mark C11 canonical only after public registration and all built-in/custom lookups use the same registry. Remove the stale “runtime registration remains unimplemented” note and recalculate aggregate counts with the documented parser.

- [ ] **Step 2: Update correspondence citations**

Record the public StreamFilter, PipelineRef, OwnedDecodePipeline, and register_stream_filter mappings using the verified qpdf header/source paths. Do not claim qpdf mutex or lock-poisoning behavior. Run:

~~~text
python3 scripts/qpdf-module-docs.py --write
python3 scripts/qpdf-module-docs.py --check
~~~

- [ ] **Step 3: Verify route and deviation accounting**

Run:

~~~text
python3 scripts/check-qpdf-route-matrix.py --check --qpdf-root /home/ubuntu/.cache/flpdf/qpdf-11.9.0
python3 scripts/check-qpdf-deviation-markers.py --check
python3 scripts/qpdf-route-callers.py --symbol crates/flpdf/src/stream_filter.rs::stream_filter_for
~~~

The expected census is the two canonical planner calls in `object_handle.rs`; no
independent fixed-match caller remains. `--expect-zero` is not applicable because
the canonical registry lookup itself remains a production caller.

### Task 7: Run all gates and prepare the bounded PR

- [ ] **Step 1: Run focused and full tests**

~~~text
cargo fmt --all -- --check
cargo test -p flpdf --test stream_filter_registry_tests
cargo test -p flpdf --test filters_route_cutover_tests
cargo test -p flpdf --test qpdf_route_hygiene_tests
cargo test -p flpdf
cargo test -p flpdf-qtest-tools
cargo test -p flpdf-cli --test cli_tests
cargo clippy --workspace --all-targets --all-features -- -D warnings
~~~

- [ ] **Step 2: Run strict docs and patch coverage**

~~~text
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/fix/flpdf-3yn9-48-96-c28 --lcov target/patch-cov.lcov
~~~

- [ ] **Step 3: Commit and close only C11**

~~~text
git diff --check
git status --short
git add crates/flpdf/src/pipeline.rs crates/flpdf/src/stream_filter.rs crates/flpdf/src/object_handle.rs crates/flpdf/src/lib.rs crates/flpdf/tests/stream_filter_registry_tests.rs tests/oracle/qpdf_stream_filter_registry_probe.cc scripts/qpdf-stream-filter-registry-probe.sh docs/qpdf-correspondence.md docs/qpdf-route-matrix docs/qpdf-module-doc-index.md docs/superpowers/specs/2026-09-14-c11-runtime-stream-filter-registry-design.md docs/superpowers/plans/2026-09-14-c11-runtime-stream-filter-registry.md
git commit -m "feat: add qpdf runtime stream-filter registry"
~~~

Close flpdf-3yn9.48.46 only after the local gates, pinned probe, caller census, documentation checks, and integrated PR readback are complete. Do not close the parent or merge; the integration session owns merge.

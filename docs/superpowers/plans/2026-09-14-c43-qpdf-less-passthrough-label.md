# C43 qpdf-less passthrough label removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the qpdf-less `passthrough_codec_label` helper and its dedicated diagnostic while preserving canonical stream-filter and writer behavior.

**Architecture:** The registered `stream_filter_for` map remains the decoder factory boundary. Unsupported names in the remaining recovery helper use the generic unsupported-filter error; no image-codec label classification survives. C28's public recovering API is outside this plan.

**Tech Stack:** Rust workspace, Cargo tests, Python route-caller/citation checks, and qpdf 11.9.0.

---

### Task 1: Add the C43 RED regression assertions

**Files:**
- Modify: `crates/flpdf/tests/filters_route_cutover_tests.rs:28-42`
- Modify: `crates/flpdf/src/stream_filter.rs` test module

- [ ] **Step 1: Require the obsolete helper and message to be absent**

Replace the old requirement that the helper remain with:

```rust
#[test]
fn passthrough_codec_label_bridge_is_removed() {
    let stream_filter =
        production_source(concat!(env!("CARGO_MANIFEST_DIR"), "/src/stream_filter.rs"));
    for forbidden in [
        "pub(crate) fn passthrough_codec_label(",
        "passthrough codec {label}: image/binary stream data is not decoded by flpdf",
    ] {
        assert!(
            !stream_filter.contains(forbidden),
            "stream_filter.rs still contains C43 bridge text: {forbidden}"
        );
    }
}
```

Keep the existing assertion that the old public `filters.rs` forwarding wrapper is absent.

- [ ] **Step 2: Add the generic unsupported-filter behavior test**

Inside the existing `#[cfg(test)] mod tests` in `stream_filter.rs`, add:

```rust
#[test]
fn undecodable_filters_use_the_generic_unsupported_boundary() {
    for name in [
        b"CCITTFaxDecode".as_slice(),
        b"JBIG2Decode".as_slice(),
        b"JPXDecode".as_slice(),
        b"BogusDecode".as_slice(),
    ] {
        let expected = format!(
            "unsupported stream filter: {}",
            std::str::from_utf8(name).expect("test filter name is UTF-8")
        );
        assert!(matches!(
            super::undecodable_filter_error(name),
            crate::Error::Unsupported(message) if message == expected
        ));
    }
}
```

- [ ] **Step 3: Run the new tests and verify RED**

Run:

```bash
cargo test -p flpdf --test filters_route_cutover_tests -- --nocapture
cargo test -p flpdf stream_filter::tests::undecodable_filters_use_the_generic_unsupported_boundary -- --nocapture
```

Expected: the route-contract test fails because the helper still exists, and the unit test fails because the current image-codec branch still emits the flpdf-only message.

### Task 2: Delete the qpdf-less production helper

**Files:**
- Modify: `crates/flpdf/src/stream_filter.rs:71-102`
- Modify: `crates/flpdf/src/overlay_appearance_stream.rs` comments only if they mention the deleted helper

- [ ] **Step 1: Remove the helper and special branch**

Delete `passthrough_codec_label` and reduce the function to:

```rust
pub(crate) fn undecodable_filter_error(filter_name: &[u8]) -> Error {
    Error::Unsupported(format!(
        "unsupported stream filter: {}",
        std::str::from_utf8(filter_name).unwrap_or("<binary>")
    ))
}
```

Do not alter `normalize_filter_name`, `validate_filter_factories`, `stream_filter_for`, DCT decoding, or writer passthrough.

- [ ] **Step 2: Run the focused tests GREEN**

Run the two commands from Task 1 Step 3. Expected: all focused tests pass.

- [ ] **Step 3: Prove C43 has no callers**

Run:

```bash
python3 scripts/qpdf-route-callers.py --symbol crates/flpdf/src/stream_filter.rs::passthrough_codec_label --expect-zero
```

Expected: the symbol is absent or has zero production and test callers. This proves only C43 cleanup, not C28 or route-wide parity.

### Task 3: Re-anchor route evidence and verify the slice

**Files:**
- Modify: `docs/qpdf-route-matrix/c-stream-pipeline-encryption.md:368`
- Modify: `docs/qpdf-route-matrix/README.md:44-46,99-105`

- [ ] **Step 1: Record C43 as removed**

Update the C43 row to show `canonical`, `absent`, and the fresh zero-caller result. Keep C28's residual bridge row and note separate. Recompute the same-run aggregates to C canonical 36 / bridge 1 / mixed 5, A-E canonical 101 / bridge 1 / mixed 58, and checker logical canonical 127 / bridge 7 / mixed 125; preserve all denominators and the no-route-wide-parity wording.

- [ ] **Step 2: Run format, Rust, qpdf, and route checks**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf --lib stream_filter
cargo test -p flpdf --test filters_route_cutover_tests
qpdf --show-object=6 --filtered-stream-data tests/fixtures/test_driver/stream_unfilterable.pdf
python3 scripts/check-qpdf-route-matrix.py --check
python3 scripts/check-qpdf-deviation-markers.py --check
```

Expected: Rust/check commands exit 0; qpdf 11.9.0 exits 2 with its generic `unable to filter stream data` diagnostic; route and deviation checks exit 0.

- [ ] **Step 3: Inspect and commit only the bounded slice**

Run:

```bash
git diff --check
git status --short
git diff --stat
git add crates/flpdf/src/stream_filter.rs crates/flpdf/tests/filters_route_cutover_tests.rs docs/qpdf-route-matrix/c-stream-pipeline-encryption.md
git commit -m "refactor: remove qpdf-less passthrough codec label bridge"
```

Expected: no generated fixtures or target files are committed. Beads closure and any PR lifecycle happen only after the verified commit and current-main readback.

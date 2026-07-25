# qpdf-shaped Plain Writer Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route plain full rewrites in Disable, Preserve, and Generate object-stream modes through one qpdf-shaped logical-plan, body-emission, and xref/trailer pipeline without regressing the current qpdf 11.9.0 byte-parity floor.

**Architecture:** Build writer-owned physical serializers first, then assemble xref/trailer output from a physical `BodyLayout`, then construct a qpdf-faithful `PlainWritePlan` from the existing traversal and renumbering algorithms. Keep legacy output live while those foundations are tested, switch Disable and Preserve separately, switch Generate last, and only then delete the superseded plain paths.

**Tech Stack:** Rust 2021 workspace; existing `Pdf`, `Object`, `Dictionary`, `CatalogFirstRenumber`, `GenerateRenumber`, and ObjStm planner; qpdf 11.9.0 as source and behavioral oracle; `qpdf-zlib-compat` for byte comparisons; Beads; dependent Git branches; Cargo tests, Clippy, strict rustdoc, and `scripts/patch-coverage.sh`.

## Global Constraints

- qpdf 11.9.0 (`v11.9.0`, commit `3b97c9bd266b7c32ea36d3536e22dab77412886d`) is the source and observed-behavior oracle.
- Keep `write_pdf`, `write_pdf_with_options`, `write_qdf`, `write_stream_to_buf`, `WriteOptions`, and `ObjectStreamMode` source-compatible.
- Keep QDF, output encryption, copy-encryption, source-encrypted rewrite, linearization, and incremental-update routing unchanged.
- Build the complete plain output in memory; preflight, planning, resolve, encode, and invariant errors must occur before writing to the caller's `W`.
- Restore output-only Catalog mutations and dirty state on every success and error path.
- Reuse `qpdf_null`, `CatalogFirstRenumber`, `GenerateRenumber`, and `writer::object_streams`; do not add another graph traversal or ObjStm classifier.
- Preserve qpdf's distinct standard-enqueue BFS, compressible-object DFS, and container-aware final numbering.
- Store source references and placement in `PlainWritePlan`; do not retain cloned source bodies or physical byte offsets.
- `BodyLayout` contains physical ordinary offsets and compressed container/index locations; it does not make reachability or numbering decisions.
- Byte identity is gated only with `qpdf-zlib-compat`; the default Pure Rust backend must retain semantic and round-trip behavior.
- Production routing remains unchanged in Layers 1 through 3.
- Each layer gets one Beads child, one dependent branch, focused verification, a committed head, a push, and `bd dolt push`.
- Routing layers measure changed-line coverage from the final committed `HEAD`; gated `crates/flpdf/src` patch coverage must be 100%.

---

## Current baseline and stack

The design branch is:

```text
refactor/flpdf-2tbp-plain-writer-pipeline
└── cabb30aa docs: design qpdf-shaped plain writer pipeline
```

The implementation stack is:

```text
stack/flpdf-2tbp-serialize   (flpdf-2tbp.1)
  └── stack/flpdf-2tbp-xref (flpdf-2tbp.2)
      └── stack/flpdf-2tbp-plan (flpdf-2tbp.3)
          └── stack/flpdf-2tbp-disable (flpdf-2tbp.4)
              └── stack/flpdf-2tbp-preserve (flpdf-2tbp.5)
                  └── stack/flpdf-2tbp-generate (flpdf-2tbp.6)
```

At `cabb30aa`, these parity tests are green:

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_generate_objstm_tests
# 7 passed

cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
# 11 passed
```

`flpdf-9hc.20.29` and `.30` contain stale statements that no non-linearized
Generate golden exists. Do not reimplement their completed portions. Keep them
open until Task 6 supplies the uniform one/two/three-page mode matrix and the
new pipeline is the only plain consumer.

## File structure

- Create `crates/flpdf/src/writer/serialize.rs`
  - Own ordinary stream framing, qpdf-ordered ordinary stream dictionaries,
    fixed-order ObjStm writing, and the existing qpdf-faithful xref-stream
    encoder.
- Replace `crates/flpdf/src/linearization/xref_stream.rs`
  - Thin compatibility re-export of `writer::serialize::xref_stream` while the
    implementation remains shared by linearization.
- Create `crates/flpdf/src/writer/plain/mod.rs`
  - Coordinate plan validation, body emission, and xref/trailer assembly.
- Create `crates/flpdf/src/writer/plain/plan.rs`
  - Build and validate `PlainWritePlan` using existing qpdf traversal and
    renumbering components.
- Create `crates/flpdf/src/writer/plain/body.rs`
  - Resolve, rewrite, re-encode, and serialize planned source objects and
    containers; return `BodyLayout`.
- Create `crates/flpdf/src/writer/plain/xref.rs`
  - Convert `BodyLayout` and `TrailerPlan` into a classic xref/trailer or a
    qpdf-shaped xref stream.
- Modify `crates/flpdf/src/writer.rs`
  - Keep the public facade and specialized modes; delegate eligible plain
    modes to `writer::plain`; delete superseded plain helpers only in Task 6.
- Modify `crates/flpdf/src/linearization/writer.rs`
  - Import the writer-owned shared xref-stream encoder.
- Modify `crates/flpdf/tests/cmp_diff_zero_tests.rs`
  - Extend the Disable/Preserve byte-parity matrix.
- Modify `crates/flpdf/tests/cmp_generate_objstm_tests.rs`
  - Extend the Generate byte-parity matrix.
- Modify `crates/flpdf/tests/object_streams_writer_tests.rs`
  - Pin mode routing, source-container preservation, downgrade, and structural
    round trips.
- Modify `tests/golden/regenerate.sh`
  - Generate any missing one/two/three-page mode goldens with pinned qpdf
    11.9.0 flags.

---

### Task 1: Layer 1 — writer-owned physical serialization primitives

**Beads:** `flpdf-2tbp.1`

**Branch:** `stack/flpdf-2tbp-serialize`, based on
`refactor/flpdf-2tbp-plain-writer-pipeline`

**Files:**
- Create: `crates/flpdf/src/writer/serialize.rs`
- Modify: `crates/flpdf/src/writer.rs:1-3, 2985-3002, 4534-4551, 4880-4982, 5040-5840`
- Replace: `crates/flpdf/src/linearization/xref_stream.rs`
- Modify: `crates/flpdf/src/linearization/writer.rs:65-80`
- Test: `crates/flpdf/src/writer/serialize.rs`
- Test: `crates/flpdf/tests/newline_before_endstream_tests.rs`
- Test: `crates/flpdf/tests/cmp_generate_objstm_tests.rs`
- Test: `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`

**Interfaces:**
- Consumes:
  - `Dictionary::write_pdf_stream(&mut Vec<u8>, bool)`
  - `Dictionary::write_pdf_with_id_writer`
  - `object_streams::ObjStmBody`
  - `object_streams::wrap_objstm_body`
  - `CompressStreams`
  - `NewlineBeforeEndstream`
- Produces:

```rust
pub fn write_stream_to_buf(
    out: &mut Vec<u8>,
    stream: &crate::Stream,
    policy: NewlineBeforeEndstream,
);

pub(crate) fn write_stream_with_id_writer(
    out: &mut Vec<u8>,
    stream: &crate::Stream,
    policy: NewlineBeforeEndstream,
    id_writer: Option<crate::object::TrailerIdWriter>,
);

pub(crate) fn write_qpdf_stream(
    out: &mut Vec<u8>,
    stream: &crate::Stream,
    policy: NewlineBeforeEndstream,
    refiltered: bool,
);

pub(crate) fn framing_adds_newline(
    data: &[u8],
    policy: NewlineBeforeEndstream,
) -> bool;

pub(crate) fn write_objstm_stream(
    out: &mut Vec<u8>,
    body: &object_streams::ObjStmBody,
    compress: CompressStreams,
    policy: NewlineBeforeEndstream,
) -> crate::Result<()>;

pub(crate) mod xref_stream {
    // Move the complete existing interface from
    // linearization/xref_stream.rs without semantic changes:
    // XrefStreamEntry, XrefWidths, XrefStreamDict, encode_payload,
    // encode_payload_raw, encode_payload_uncompressed, first_pass_widths,
    // second_pass_widths, build_entries, max_entry_offset, write_object,
    // write_object_with_id_writer, first_pass_region_len,
    // write_padded_region, and calculate_xref_stream_padding.
}
```

- `writer.rs` retains the public symbol with:

```rust
pub use serialize::write_stream_to_buf;
```

- `linearization/xref_stream.rs` becomes:

```rust
//! Compatibility namespace for the writer-owned qpdf xref-stream serializer.
pub(crate) use crate::writer::serialize::xref_stream::*;
```

- [ ] **Step 1: Claim the Bead, create the bottom branch, and record stale-roadmap evidence**

Run:

```bash
bd update flpdf-2tbp.1 --claim
git switch -c stack/flpdf-2tbp-serialize
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_generate_objstm_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
bd update flpdf-9hc.20.29 --append-notes "2026-07-25: current HEAD already passes cmp_generate_objstm_tests 7/7; remaining work is uniform shared-pipeline coverage, tracked by flpdf-2tbp."
bd update flpdf-9hc.20.30 --append-notes "2026-07-25: Generate goldens and cmp_generate_objstm_tests now exist; retain this issue for missing uniform one/two/three-page mode corpus until flpdf-2tbp.6."
```

Expected: `.1` is in progress; the branch points at the current plan-branch tip
(including both the approved design and this implementation plan); both
baseline commands pass; `.29/.30` remain open with current evidence.

- [ ] **Step 2: Add failing writer-owned serializer tests**

Declare the module at the top of `writer.rs`:

```rust
#[path = "writer/serialize.rs"]
pub(crate) mod serialize;
```

Create `writer/serialize.rs` with imports, the interface declarations above,
and these tests before adding the implementation:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::object_streams::ObjStmBody;

    #[test]
    fn raw_objstm_uses_qpdf_fixed_key_order() {
        let body = ObjStmBody {
            bytes: b"3 0\nnull\n".to_vec(),
            first_offset: 4,
            n_members: 1,
        };
        let mut out = Vec::new();
        write_objstm_stream(
            &mut out,
            &body,
            CompressStreams::No,
            NewlineBeforeEndstream::No,
        )
        .unwrap();
        assert_eq!(
            out,
            b"<< /Type /ObjStm /Length 9 /N 1 /First 4 >>\n\
              stream\n3 0\nnull\nendstream"
        );
    }

    #[test]
    fn framing_policy_matches_qpdf_last_lf_rule() {
        assert!(framing_adds_newline(
            b"payload",
            NewlineBeforeEndstream::No
        ));
        assert!(!framing_adds_newline(
            b"payload\n",
            NewlineBeforeEndstream::No
        ));
        assert!(framing_adds_newline(
            b"payload\n",
            NewlineBeforeEndstream::Yes
        ));
        assert!(!framing_adds_newline(
            b"payload",
            NewlineBeforeEndstream::Never
        ));
    }
}
```

- [ ] **Step 3: Run the new unit tests and verify the missing implementation**

Run:

```bash
cargo test -p flpdf writer::serialize::tests -- --nocapture
```

Expected: compilation fails because the declared serializer functions and
`xref_stream` implementation do not exist.

- [ ] **Step 4: Move stream framing and add the fixed-order ObjStm writer**

Move, without semantic changes:

- `write_stream_to_buf`;
- `write_stream_to_buf_with_id_writer`, renamed
  `write_stream_with_id_writer`;
- `write_stream_to_buf_qpdf_order`, renamed `write_qpdf_stream`;
- `write_stream_payload`;
- `stream_framing_adds_newline`, renamed `framing_adds_newline`.

Implement `write_objstm_stream` exactly as:

```rust
pub(crate) fn write_objstm_stream(
    out: &mut Vec<u8>,
    body: &object_streams::ObjStmBody,
    compress: CompressStreams,
    policy: NewlineBeforeEndstream,
) -> crate::Result<()> {
    let stream = object_streams::wrap_objstm_body(body, compress)?;
    out.extend_from_slice(b"<< /Type /ObjStm /Length ");
    out.extend_from_slice(stream.data.len().to_string().as_bytes());
    if stream.dict.get("Filter").is_some() {
        out.extend_from_slice(b" /Filter /FlateDecode");
    }
    out.extend_from_slice(
        format!(" /N {} /First {} >>", body.n_members, body.first_offset).as_bytes(),
    );
    write_stream_payload(out, &stream.data, policy);
    Ok(())
}
```

Keep `writer.rs` call sites behavior-identical by importing:

```rust
use serialize::{
    framing_adds_newline as stream_framing_adds_newline,
    write_qpdf_stream as write_stream_to_buf_qpdf_order,
    write_stream_with_id_writer as write_stream_to_buf_with_id_writer,
};
pub use serialize::write_stream_to_buf;
```

Replace the hand-written ObjStm dictionary block in
`write_pdf_containerized_qpdf` with:

```rust
serialize::write_objstm_stream(
    &mut bytes,
    &body,
    structural_compress,
    options.newline_before_endstream,
)?;
```

- [ ] **Step 5: Move the shared xref-stream encoder under the writer**

Move the implementation and its full test module from
`linearization/xref_stream.rs` into:

```rust
pub(crate) mod xref_stream {
    // existing implementation, imports, constants, helpers, and tests
}
```

inside `writer/serialize.rs`. Replace the old file with the compatibility
re-export shown in **Interfaces**. Change `linearization/writer.rs` to import:

```rust
use crate::writer::serialize::xref_stream;
```

Change `write_pdf_containerized_qpdf` to use the same writer-owned namespace.
Do not change any encoder signature, key order, width calculation, predictor,
padding, or golden constant.

- [ ] **Step 6: Run Layer 1 focused and regression gates**

Run:

```bash
cargo test -p flpdf writer::serialize::tests -- --nocapture
cargo test -p flpdf writer::serialize::xref_stream::tests -- --nocapture
cargo test -p flpdf --test newline_before_endstream_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_generate_objstm_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests
cargo test -p flpdf
cargo test -p flpdf-cli
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: all commands pass; Generate and linearized bytes remain unchanged.

- [ ] **Step 7: Commit, push, and persist Layer 1**

Run:

```bash
git add crates/flpdf/src/writer.rs crates/flpdf/src/writer/serialize.rs crates/flpdf/src/linearization/xref_stream.rs crates/flpdf/src/linearization/writer.rs
git commit -m "refactor(writer): extract physical serializers"
git push -u origin stack/flpdf-2tbp-serialize
bd close flpdf-2tbp.1 --reason="Writer-owned stream, ObjStm, and shared xref-stream serializers extracted with production routing unchanged."
bd dolt push
```

Expected: commit and both pushes succeed; `.2` becomes ready.

---

### Task 2: Layer 2 — BodyLayout-driven xref and trailer assembly

**Beads:** `flpdf-2tbp.2`

**Branch:** `stack/flpdf-2tbp-xref`, based on
`stack/flpdf-2tbp-serialize`

**Files:**
- Create: `crates/flpdf/src/writer/plain/mod.rs`
- Create: `crates/flpdf/src/writer/plain/xref.rs`
- Modify: `crates/flpdf/src/writer.rs:1-8`
- Test: `crates/flpdf/src/writer/plain/xref.rs`
- Test: `crates/flpdf/src/writer/serialize.rs`

**Interfaces:**
- Consumes:
  - `writer::serialize::xref_stream`
  - `Dictionary::write_pdf_trailer`
  - `write_deterministic_id_inline`
  - `ObjectRef`, `XrefForm`, and `NewlineBeforeEndstream`
- Produces:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CompressedLocation {
    pub(crate) container: u32,
    pub(crate) index: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BodyLayout {
    pub(crate) uncompressed: BTreeMap<u32, (u16, usize)>,
    pub(crate) compressed: BTreeMap<u32, CompressedLocation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum IdPlan {
    Materialized,
    Deterministic {
        source_id0: Option<Vec<u8>>,
        info_suffix: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct TrailerPlan {
    pub(crate) form: XrefForm,
    pub(crate) dictionary: Dictionary,
    pub(crate) root: ObjectRef,
    pub(crate) id: IdPlan,
    pub(crate) structural_filtered: bool,
}

pub(crate) fn append_xref_and_trailer(
    bytes: &mut Vec<u8>,
    layout: &BodyLayout,
    trailer: &TrailerPlan,
) -> crate::Result<()>;
```

`append_xref_and_trailer` computes `/Size` from `BodyLayout`. In stream form it
assigns the xref stream object to `max_layout_number + 1`, records its own
offset, and uses `serialize::xref_stream`. It never reads a `Pdf`.

- [ ] **Step 1: Claim Layer 2 and branch from the pushed Layer 1 head**

Run:

```bash
bd update flpdf-2tbp.2 --claim
git switch -c stack/flpdf-2tbp-xref
```

Expected: branch creation succeeds and `git merge-base --is-ancestor
stack/flpdf-2tbp-serialize HEAD` exits zero.

- [ ] **Step 2: Write failing synthetic classic-xref tests**

Declare the module in `writer.rs`:

```rust
#[path = "writer/plain/mod.rs"]
pub(crate) mod plain;
```

Create `plain/mod.rs`:

```rust
pub(crate) mod xref;
```

Add to `plain/xref.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn trailer(form: XrefForm) -> TrailerPlan {
        TrailerPlan {
            form,
            dictionary: Dictionary::new(),
            root: ObjectRef::new(1, 0),
            id: IdPlan::Materialized,
            structural_filtered: false,
        }
    }

    #[test]
    fn classic_xref_uses_layout_offsets_and_qpdf_trailer_shape() {
        let mut bytes = b"BODY".to_vec();
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (0, 0));
        append_xref_and_trailer(&mut bytes, &layout, &trailer(XrefForm::Table))
            .unwrap();
        assert_eq!(
            bytes,
            b"BODYxref\n0 2\n\
              0000000000 65535 f \n\
              0000000000 00000 n \n\
              trailer << /Root 1 0 R /Size 2 >>\n\
              startxref\n4\n%%EOF\n"
        );
    }

    #[test]
    fn layout_rejects_plain_and_compressed_collision() {
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(4, (0, 10));
        layout.compressed.insert(
            4,
            CompressedLocation {
                container: 3,
                index: 0,
            },
        );
        let err = layout.validate().unwrap_err();
        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("object 4")));
    }
}
```

- [ ] **Step 3: Run the classic tests and verify missing types/functions**

Run:

```bash
cargo test -p flpdf writer::plain::xref::tests::classic_xref -- --nocapture
cargo test -p flpdf writer::plain::xref::tests::layout_rejects -- --nocapture
```

Expected: compilation fails because `BodyLayout`, `TrailerPlan`,
`CompressedLocation`, `validate`, and `append_xref_and_trailer` do not exist.

- [ ] **Step 4: Implement `BodyLayout` validation and classic assembly**

Implement the types in **Interfaces** and:

```rust
impl BodyLayout {
    pub(crate) fn validate(&self) -> crate::Result<()> {
        for number in self.uncompressed.keys() {
            if self.compressed.contains_key(number) {
                return Err(crate::Error::Unsupported(format!(
                    "plain writer layout: object {number} is both uncompressed and compressed"
                )));
            }
        }
        Ok(())
    }

    fn max_number(&self) -> u32 {
        self.uncompressed
            .keys()
            .chain(self.compressed.keys())
            .copied()
            .max()
            .unwrap_or(0)
    }
}
```

The classic arm must:

1. validate the layout;
2. capture `xref_offset = bytes.len()`;
3. emit object zero as `0000000000 65535 f`;
4. emit every `1..size` slot from `uncompressed` or as free;
5. clone the supplied dictionary, set `/Root` and `/Size`;
6. write deterministic ID inline only for `IdPlan::Deterministic`;
7. append `startxref` and `%%EOF`.

- [ ] **Step 5: Write failing xref-stream layout tests**

Add:

```rust
#[test]
fn xref_stream_uses_minimal_widths_and_omits_full_range_index() {
    let mut bytes = b"BODY".to_vec();
    let mut layout = BodyLayout::default();
    layout.uncompressed.insert(1, (0, 0));
    layout.compressed.insert(
        2,
        CompressedLocation {
            container: 1,
            index: 0,
        },
    );
    append_xref_and_trailer(&mut bytes, &layout, &trailer(XrefForm::Stream))
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Type /XRef"));
    assert!(text.contains("/W [ 1 1 0 ]"));
    assert!(!text.contains("/Index"));
    assert!(text.contains("/Root 1 0 R /Size 4"));
    assert!(text.ends_with("startxref\n4\n%%EOF\n"));
}
```

- [ ] **Step 6: Run the xref-stream test and verify it fails**

Run:

```bash
cargo test -p flpdf writer::plain::xref::tests::xref_stream_uses -- --nocapture
```

Expected: FAIL because the stream arm is not implemented.

- [ ] **Step 7: Implement qpdf-shaped xref-stream assembly**

Use `serialize::xref_stream` as follows:

```rust
let xref_offset = bytes.len();
let max_number = layout.max_number();
let xref_number = max_number.checked_add(1).ok_or_else(|| {
    crate::Error::Unsupported("plain writer xref object number overflows u32".into())
})?;
let size = xref_number.checked_add(1).ok_or_else(|| {
    crate::Error::Unsupported("plain writer /Size overflows u32".into())
})?;

let mut offsets: BTreeMap<u32, usize> = layout
    .uncompressed
    .iter()
    .map(|(&number, &(_, offset))| (number, offset))
    .collect();
offsets.insert(xref_number, xref_offset);
let members: BTreeMap<u32, (u32, u32)> = layout
    .compressed
    .iter()
    .map(|(&number, location)| (number, (location.container, location.index)))
    .collect();
let entries = serialize::xref_stream::build_entries(&offsets, &members, 0, size);
let widths = serialize::xref_stream::second_pass_widths(
    serialize::xref_stream::max_entry_offset(&entries),
    0,
    max_number,
    members.values().map(|&(_, i)| u64::from(i)).max().unwrap_or(0),
);
```

Encode raw or predicted+Flate according to `structural_filtered`; remove
`Root`, `Size`, `ID`, `Encrypt`, and xref-only source keys from the trailer
clone before constructing `XrefStreamDict`; set `index: None`; write the ID
through either `write_object` or `write_object_with_id_writer`; append
`startxref` using the captured xref offset.

- [ ] **Step 8: Run Layer 2 verification**

Run:

```bash
cargo test -p flpdf writer::plain::xref::tests -- --nocapture
cargo test -p flpdf writer::serialize::xref_stream::tests -- --nocapture
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_generate_objstm_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests
cargo test -p flpdf
cargo test -p flpdf-cli
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: all pass; no production call site invokes `plain::xref` yet.

- [ ] **Step 9: Commit, push, and persist Layer 2**

Run:

```bash
git add crates/flpdf/src/writer.rs crates/flpdf/src/writer/plain/mod.rs crates/flpdf/src/writer/plain/xref.rs
git commit -m "refactor(writer): assemble xref from body layout"
git push -u origin stack/flpdf-2tbp-xref
bd close flpdf-2tbp.2 --reason="BodyLayout-driven classic and qpdf-shaped xref/trailer assembly added without production routing changes."
bd dolt push
```

---

### Task 3: Layer 3 — qpdf-faithful logical plain write plan

**Beads:** `flpdf-2tbp.3`

**Branch:** `stack/flpdf-2tbp-plan`, based on `stack/flpdf-2tbp-xref`

**Files:**
- Create: `crates/flpdf/src/writer/plain/plan.rs`
- Modify: `crates/flpdf/src/writer/plain/mod.rs`
- Modify: `crates/flpdf/src/rewrite_renumber.rs:20-110, 460-510`
- Test: `crates/flpdf/src/writer/plain/plan.rs`
- Test: `crates/flpdf/tests/object_streams_writer_tests.rs`

**Interfaces:**
- Consumes:
  - `CatalogFirstRenumber::build_qpdf`
  - `GenerateRenumber::build`
  - `object_streams::compressible_objgens_qpdf_plan`
  - `object_streams::even_split_into_streams`
  - `object_streams::plan_qpdf_preserve_object_streams`
  - `qpdf_null`
  - `TrailerPlan` and `CompressedLocation`
- Produces:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlannedMember {
    pub(crate) source: ObjectRef,
    pub(crate) output: ObjectRef,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlannedIndirectObject {
    Source {
        source: ObjectRef,
        output: ObjectRef,
    },
    ObjectStream {
        output: ObjectRef,
        members: Vec<PlannedMember>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct PlainWritePlan {
    pub(crate) version: String,
    pub(crate) objects: Vec<PlannedIndirectObject>,
    pub(crate) root: ObjectRef,
    pub(crate) old_to_new: HashMap<ObjectRef, ObjectRef>,
    pub(crate) removed_refs: BTreeSet<ObjectRef>,
    pub(crate) trailer: TrailerPlan,
}

impl PlainWritePlan {
    pub(crate) fn build<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        options: &WriteOptions,
    ) -> crate::Result<Self>;

    pub(crate) fn validate(&self) -> crate::Result<()>;

    pub(crate) fn new_for_original(
        &self,
        source: ObjectRef,
    ) -> Option<ObjectRef>;

    pub(crate) fn compressed_location(
        &self,
        output: ObjectRef,
    ) -> Option<CompressedLocation>;
}
```

Implement `NewNumberLookup for PlainWritePlan` so the existing qpdf-aware
reference rewriter consumes the plan directly.

- [ ] **Step 1: Claim Layer 3 and create its dependent branch**

Run:

```bash
bd update flpdf-2tbp.3 --claim
git switch -c stack/flpdf-2tbp-plan
```

- [ ] **Step 2: Write failing plan invariant tests**

Add `pub(crate) mod plan;` to `plain/mod.rs`, then add:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn source(source: u32, output: u32) -> PlannedIndirectObject {
        PlannedIndirectObject::Source {
            source: ObjectRef::new(source, 0),
            output: ObjectRef::new(output, 0),
        }
    }

    #[test]
    fn validation_rejects_duplicate_output_numbers() {
        let mut plan = plan_for_test(vec![source(1, 1), source(2, 1)]);
        plan.root = ObjectRef::new(1, 0);
        let err = plan.validate().unwrap_err();
        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("output object 1")));
    }

    #[test]
    fn validation_rejects_member_with_nonzero_generation() {
        let member = PlannedMember {
            source: ObjectRef::new(7, 1),
            output: ObjectRef::new(2, 1),
        };
        let plan = plan_for_test(vec![PlannedIndirectObject::ObjectStream {
            output: ObjectRef::new(1, 0),
            members: vec![member],
        }]);
        let err = plan.validate().unwrap_err();
        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("7 1 R")));
    }
}
```

Define the test constructor under `#[cfg(test)]`:

```rust
fn plan_for_test(objects: Vec<PlannedIndirectObject>) -> PlainWritePlan {
    let root_source = ObjectRef::new(1, 0);
    let root_output = ObjectRef::new(1, 0);
    PlainWritePlan {
        version: "1.5".to_string(),
        objects,
        root: root_output,
        old_to_new: HashMap::from([(root_source, root_output)]),
        removed_refs: BTreeSet::new(),
        trailer: TrailerPlan {
            form: XrefForm::Table,
            dictionary: Dictionary::new(),
            root: root_output,
            id: IdPlan::Materialized,
            structural_filtered: false,
        },
    }
}
```

- [ ] **Step 3: Run invariant tests and verify missing plan types**

Run:

```bash
cargo test -p flpdf writer::plain::plan::tests::validation -- --nocapture
```

Expected: compilation fails because the plan types and validation are absent.

- [ ] **Step 4: Implement plan types, lookup, and validation**

Validation must check:

```rust
let mut outputs = BTreeSet::new();
let mut sources = BTreeSet::new();
for object in &self.objects {
    match object {
        PlannedIndirectObject::Source { source, output } => {
            require_unique_output(&mut outputs, *output)?;
            require_unique_source(&mut sources, *source)?;
        }
        PlannedIndirectObject::ObjectStream { output, members } => {
            require_unique_output(&mut outputs, *output)?;
            for member in members {
                if member.source.generation != 0 || member.output.generation != 0 {
                    return Err(crate::Error::Unsupported(format!(
                        "plain writer plan: ObjStm member {} {} R must have generation 0",
                        member.source.number, member.source.generation
                    )));
                }
                require_unique_source(&mut sources, member.source)?;
                if !outputs.insert(member.output.number) {
                    return Err(crate::Error::Unsupported(format!(
                        "plain writer plan: output object {} has multiple placements",
                        member.output.number
                    )));
                }
            }
        }
    }
}
```

Also require the root in `old_to_new`, contiguous output coverage through the
maximum planned number, PDF >= 1.5 for any ObjectStream or stream-form trailer,
and no ObjectStream under a forced effective version below 1.5.

- [ ] **Step 5: Write failing mode-plan tests**

Use repository fixtures directly:

```rust
fn build(fixture: &str, mode: ObjectStreamMode) -> PlainWritePlan {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let mut pdf = Pdf::open(std::io::BufReader::new(
        std::fs::File::open(path).unwrap(),
    ))
    .unwrap();
    let options = WriteOptions {
        full_rewrite: true,
        object_streams: mode,
        static_id: true,
        newline_before_endstream: NewlineBeforeEndstream::Never,
        ..WriteOptions::default()
    };
    PlainWritePlan::build(&mut pdf, &options).unwrap()
}

#[test]
fn disable_plan_contains_only_source_objects() {
    let plan = build("three-page.pdf", ObjectStreamMode::Disable);
    assert!(plan.objects.iter().all(
        |object| matches!(object, PlannedIndirectObject::Source { .. })
    ));
    assert_eq!(plan.trailer.form, XrefForm::Table);
    plan.validate().unwrap();
}

#[test]
fn preserve_plan_keeps_source_objstm_members_together() {
    let plan = build("three-page-objstm.pdf", ObjectStreamMode::Preserve);
    assert!(plan.objects.iter().any(
        |object| matches!(object, PlannedIndirectObject::ObjectStream { members, .. }
            if !members.is_empty())
    ));
    assert_eq!(plan.trailer.form, XrefForm::Stream);
    plan.validate().unwrap();
}

#[test]
fn generate_plan_even_splits_132_eligible_objects() {
    let plan = build(
        "objstm-gen-nostream-130rev.pdf",
        ObjectStreamMode::Generate,
    );
    let sizes: Vec<usize> = plan
        .objects
        .iter()
        .filter_map(|object| match object {
            PlannedIndirectObject::ObjectStream { members, .. } => Some(members.len()),
            _ => None,
        })
        .collect();
    assert_eq!(sizes, vec![66, 66]);
    plan.validate().unwrap();
}
```

- [ ] **Step 6: Run the mode-plan tests and verify they fail**

Run:

```bash
cargo test -p flpdf writer::plain::plan::tests::disable_plan -- --nocapture
cargo test -p flpdf writer::plain::plan::tests::preserve_plan -- --nocapture
cargo test -p flpdf writer::plain::plan::tests::generate_plan -- --nocapture
```

Expected: tests fail because `PlainWritePlan::build` is absent.

- [ ] **Step 7: Implement mode-specific planning by composing existing algorithms**

Use exactly these strategies:

```rust
match options.object_streams {
    ObjectStreamMode::Disable => {
        let renumber = CatalogFirstRenumber::build_qpdf(pdf, true)?;
        build_sources_from_catalog_first(renumber)
    }
    ObjectStreamMode::Preserve => {
        let packing = object_streams::plan_qpdf_preserve_object_streams(pdf)?;
        if packing.batches.is_empty() && !source_has_compressed_entries(pdf) {
            let renumber = CatalogFirstRenumber::build_qpdf(pdf, true)?;
            build_sources_from_catalog_first(renumber)
        } else {
            let renumber =
                GenerateRenumber::build(pdf, &packing.batches, true, &packing.removed_refs)?;
            build_container_aware(renumber, packing.batches, packing.removed_refs)?
        }
    }
    ObjectStreamMode::Generate => {
        let compressible = object_streams::compressible_objgens_qpdf_plan(pdf)?;
        let groups = object_streams::even_split_into_streams(&compressible.eligible);
        let renumber =
            GenerateRenumber::build(pdf, &groups, true, &compressible.removed_refs)?;
        build_container_aware(renumber, groups, compressible.removed_refs)?
    }
}
```

Convert the chosen renumberer's pairs into `old_to_new`, construct source and
container placements in ascending output-number order, clone and trim the
source trailer, remap its visible references with `&old_to_new`, and only then
construct `PlainWritePlan`. Build the trailer with:

```rust
let mut dictionary = pdf.trailer().clone();
strip_incremental_trailer_keys(&mut dictionary);
remap_qpdf_trailer_refs_with_removed(
    pdf,
    &mut dictionary,
    &old_to_new,
    &removed_refs,
)?;
dictionary.insert("Root", Object::Reference(root));
apply_encrypt_trailer_entries(
    &mut dictionary,
    pdf,
    options,
    None,
    options.deterministic_id,
);
let id = if options.deterministic_id {
    IdPlan::Deterministic {
        source_id0: source_permanent_id(pdf.trailer()),
        info_suffix: deterministic_id_info_suffix(pdf),
    }
} else {
    IdPlan::Materialized
};
```

Use `effective_pdf_version` after placement is known so the ObjStm floor is
applied exactly when an ObjectStream is planned. Do not clone resolved source
bodies.

Choose the xref form with these exact rules:

```rust
let form = if force_version_below_1_5(options) {
    XrefForm::Table
} else if options.object_streams == ObjectStreamMode::Generate {
    XrefForm::Stream
} else if !groups.is_empty() {
    XrefForm::Stream
} else if options.object_streams == ObjectStreamMode::Preserve
    && source_had_compressed_objects
{
    // qpdf drops the now-empty source container set and emits a classic xref.
    XrefForm::Table
} else {
    pdf.last_xref_form()
};
```

`compressed_location` scans the planned ObjectStream whose member output
matches the argument and returns that container number and member index. It
does not inspect source xref state.

- [ ] **Step 8: Add shadow comparisons against legacy output**

For Disable (`three-page.pdf`), Preserve (`three-page-objstm.pdf`), and Generate
(`objstm-gen-nostream-130rev.pdf`):

1. build a plan from one fresh `Pdf`;
2. run the existing `write_pdf_with_options` on a second fresh `Pdf`;
3. reopen the legacy output;
4. assert the output `/Root` equals `plan.root`;
5. assert each planned source/container has an uncompressed xref entry;
6. assert each planned member has the plan's exact container/index compressed
   entry.

Add a helper with this signature:

```rust
fn assert_plan_matches_legacy_xref(
    fixture: &str,
    mode: ObjectStreamMode,
) {
    // exact five-step comparison above
}
```

The test must inspect `Pdf::source_xref_entries()` and compare
`XrefOffset::Offset` versus `XrefOffset::Compressed { stream, index }`.

- [ ] **Step 9: Run Layer 3 verification**

Run:

```bash
cargo test -p flpdf writer::plain::plan::tests -- --nocapture
cargo test -p flpdf --test object_streams_writer_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_generate_objstm_tests
cargo test -p flpdf
cargo test -p flpdf-cli
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: all pass; production output still comes from legacy paths.

- [ ] **Step 10: Commit, push, and persist Layer 3**

Run:

```bash
git add crates/flpdf/src/writer/plain/mod.rs crates/flpdf/src/writer/plain/plan.rs crates/flpdf/src/rewrite_renumber.rs crates/flpdf/tests/object_streams_writer_tests.rs
git commit -m "refactor(writer): add logical plain write plan"
git push -u origin stack/flpdf-2tbp-plan
bd close flpdf-2tbp.3 --reason="Validated qpdf-faithful logical plans added and shadow-compared against legacy output for all three modes."
bd dolt push
```

---

### Task 4: Layer 4 — body emitter and Disable production routing

**Beads:** `flpdf-2tbp.4`

**Branch:** `stack/flpdf-2tbp-disable`, based on `stack/flpdf-2tbp-plan`

**Files:**
- Create: `crates/flpdf/src/writer/plain/body.rs`
- Modify: `crates/flpdf/src/writer/plain/mod.rs`
- Modify: `crates/flpdf/src/writer.rs:3005-4320`
- Modify: `crates/flpdf/tests/cmp_diff_zero_tests.rs`
- Modify: `crates/flpdf/tests/object_streams_writer_tests.rs`
- Test: `crates/flpdf/src/writer/plain/body.rs`

**Interfaces:**
- Consumes:
  - `PlainWritePlan`
  - `renumber_qpdf_refs_in_place_with_removed`
  - `reencode_stream_for_compress`
  - `write_reencoded_object`
  - `serialize::write_objstm_stream`
  - `append_xref_and_trailer`
- Produces:

```rust
pub(crate) fn emit_bodies<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: &WriteOptions,
    plan: &PlainWritePlan,
) -> crate::Result<(Vec<u8>, BodyLayout)>;

pub(crate) fn write_plain<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    out: W,
    options: &WriteOptions,
) -> crate::Result<()>;

pub(crate) fn eligible(
    pdf_is_encrypted: bool,
    options: &WriteOptions,
    mode: ObjectStreamMode,
) -> bool;
```

For Task 4, `eligible` returns true only for Disable and only when QDF,
encryption, copy-encryption, and source encryption are absent.

- [ ] **Step 1: Claim and branch Layer 4**

Run:

```bash
bd update flpdf-2tbp.4 --claim
git switch -c stack/flpdf-2tbp-disable
```

- [ ] **Step 2: Write failing body-layout tests**

Add `pub(crate) mod body;` to `plain/mod.rs`, then add to `body.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disable_emission_records_every_planned_source_offset() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open_mem(fixture).unwrap();
        let options = WriteOptions {
            full_rewrite: true,
            object_streams: ObjectStreamMode::Disable,
            static_id: true,
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..WriteOptions::default()
        };
        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        let (_, layout) = emit_bodies(&mut pdf, &options, &plan).unwrap();
        assert!(layout.compressed.is_empty());
        assert_eq!(
            layout.uncompressed.len(),
            plan.objects.iter().filter(
                |object| matches!(object, PlannedIndirectObject::Source { .. })
            ).count()
        );
    }
}
```

- [ ] **Step 3: Run the body test and verify the missing emitter**

Run:

```bash
cargo test -p flpdf writer::plain::body::tests -- --nocapture
```

Expected: compilation fails because `emit_bodies` is absent.

- [ ] **Step 4: Implement body emission without xref output**

Begin bytes with:

```rust
let mut bytes = Vec::new();
bytes.extend_from_slice(format!("%PDF-{}\n", plan.version).as_bytes());
bytes.extend_from_slice(QPDF_BINARY_MARKER);
```

For each `plan.objects` item in ascending output number:

- `Source`: resolve by source ref; rewrite through `PlainWritePlan`; if stream,
  apply `reencode_stream_for_compress` and `write_reencoded_object`; otherwise
  call `Object::write_pdf`; wrap in `N 0 obj`/`endobj`; record the offset.
- `ObjectStream`: resolve every member, rewrite it, call
  `emit_objstm_body_from_resolved`, call `serialize::write_objstm_stream`, and
  record the container offset plus every member's `CompressedLocation`.

The emitter must not choose membership, assign numbers, remap the trailer, or
write an xref.

- [ ] **Step 5: Implement coordinator and Disable eligibility**

In `plain/mod.rs`:

```rust
pub(crate) fn write_plain<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriteOptions,
) -> crate::Result<()> {
    let plan = plan::PlainWritePlan::build(pdf, options)?;
    plan.validate()?;
    let (mut bytes, layout) = body::emit_bodies(pdf, options, &plan)?;
    xref::append_xref_and_trailer(&mut bytes, &layout, &plan.trailer)?;
    out.write_all(&bytes)?;
    Ok(())
}
```

Add a private predicate:

```rust
pub(crate) fn eligible(
    pdf_is_encrypted: bool,
    options: &WriteOptions,
    mode: ObjectStreamMode,
) -> bool {
    mode == ObjectStreamMode::Disable
        && !options.qdf
        && options.encrypt.is_none()
        && options.copy_encryption.is_none()
        && !pdf_is_encrypted
}
```

In `write_pdf_full_rewrite_inner`, after preflight and Catalog extension
mutation but before legacy planning:

```rust
if plain::eligible(
    pdf.encryption_ref().is_some(),
    options,
    options.object_streams,
) {
    return plain::write_plain(pdf, out, options);
}
```

The outer `write_pdf_full_rewrite` remains responsible for restoring the
Catalog and dirty flag after this return.

- [ ] **Step 6: Add explicit Disable routing and byte-parity tests**

Add a test-only thread-local counter in `plain/mod.rs`:

```rust
#[cfg(test)]
thread_local! {
    static PLAIN_PIPELINE_CALLS: Cell<usize> = const { Cell::new(0) };
}
```

Increment it at the start of `write_plain`; expose a test-only getter/resetter.
Keeping the observation thread-local prevents concurrent Rust tests from
resetting or incrementing one another's routing evidence.
Add unit tests proving:

- Disable increments the new counter;
- Preserve and Generate do not yet increment it;
- QDF and encrypt+Disable do not increment it.

In `cmp_diff_zero_tests.rs`, make the classic helper explicitly set:

```rust
opts.object_streams = ObjectStreamMode::Disable;
```

so one/two/three-page and stream fixtures prove the new route rather than the
default Preserve route.

- [ ] **Step 7: Run focused and byte-parity gates**

Run:

```bash
cargo test -p flpdf writer::plain -- --nocapture
cargo test -p flpdf --test object_streams_writer_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
cargo test -p flpdf --test deterministic_id_xref_stream_tests
cargo test -p flpdf
cargo test -p flpdf-cli
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: all pass; Disable uses the new route; all excluded modes remain
legacy.

- [ ] **Step 8: Commit Layer 4, then measure committed-HEAD patch coverage**

Run:

```bash
git add crates/flpdf/src/writer.rs crates/flpdf/src/writer/plain/mod.rs crates/flpdf/src/writer/plain/body.rs crates/flpdf/tests/cmp_diff_zero_tests.rs crates/flpdf/tests/object_streams_writer_tests.rs
git commit -m "refactor(writer): route disable through plain pipeline"
scripts/patch-coverage.sh --base stack/flpdf-2tbp-plan
```

Expected: patch coverage reports 100% for changed `crates/flpdf/src` lines. If
it reports an executable line with zero hits, add a test for that exact branch,
commit the test, and rerun the command from the new clean `HEAD`. Do not use a
reasonless coverage exclusion.

- [ ] **Step 9: Push and persist Layer 4**

Run:

```bash
git push -u origin stack/flpdf-2tbp-disable
bd close flpdf-2tbp.4 --reason="Disable now uses the shared plain pipeline with qpdf byte parity and 100% committed-HEAD patch coverage."
bd dolt push
```

---

### Task 5: Layer 5 — Preserve production routing

**Beads:** `flpdf-2tbp.5`

**Branch:** `stack/flpdf-2tbp-preserve`, based on
`stack/flpdf-2tbp-disable`

**Files:**
- Modify: `crates/flpdf/src/writer/plain/mod.rs`
- Modify: `crates/flpdf/src/writer/plain/plan.rs`
- Modify: `crates/flpdf/src/writer/plain/body.rs`
- Modify: `crates/flpdf/src/writer.rs:3188-3235, 4317-4740`
- Modify: `crates/flpdf/tests/cmp_diff_zero_tests.rs`
- Modify: `crates/flpdf/tests/object_streams_writer_tests.rs`
- Test: `crates/flpdf/src/writer/plain/plan.rs`

**Interfaces:**
- Extends `plain::eligible` to Disable and Preserve for eligible plain inputs.
- Keeps the Task 4 body/xref interfaces unchanged.
- Adds:

```rust
pub(crate) fn source_has_compressed_entries<R: Read + Seek>(
    pdf: &Pdf<R>,
) -> bool;
```

- [ ] **Step 1: Claim and branch Layer 5**

Run:

```bash
bd update flpdf-2tbp.5 --claim
git switch -c stack/flpdf-2tbp-preserve
```

- [ ] **Step 2: Write failing Preserve membership and routing tests**

Add tests that:

```rust
#[test]
fn preserve_source_objstm_members_keep_one_container_and_indices() {
    let plan = build("three-page-objstm.pdf", ObjectStreamMode::Preserve);
    let containers: Vec<_> = plan.objects.iter().filter_map(|object| match object {
        PlannedIndirectObject::ObjectStream { output, members } => {
            Some((*output, members.clone()))
        }
        _ => None,
    }).collect();
    assert_eq!(containers.len(), 1);
    assert!(!containers[0].1.is_empty());
    for (index, member) in containers[0].1.iter().enumerate() {
        assert_eq!(member.output.generation, 0);
        assert_eq!(index as u32, plan.compressed_location(member.output).unwrap().index);
    }
}

#[test]
fn preserve_without_source_objstm_uses_catalog_first_sources() {
    let plan = build("three-page.pdf", ObjectStreamMode::Preserve);
    assert!(plan.objects.iter().all(
        |object| matches!(object, PlannedIndirectObject::Source { .. })
    ));
}
```

Extend the routing counter test to expect Preserve to call `write_plain`.

- [ ] **Step 3: Run Preserve tests and verify routing expectation fails**

Run:

```bash
cargo test -p flpdf writer::plain::plan::tests::preserve -- --nocapture
cargo test -p flpdf writer::plain::tests::preserve -- --nocapture
```

Expected: plan membership tests pass from Task 3; the routing test fails because
Preserve still uses the specialized or legacy path.

- [ ] **Step 4: Extend eligibility and remove only the Preserve early return**

Change:

```rust
matches!(mode, ObjectStreamMode::Disable | ObjectStreamMode::Preserve)
```

in `plain::eligible`. Delete the plain unencrypted Preserve early-return block
that calls `write_pdf_containerized_qpdf`. Do not delete
`write_pdf_containerized_qpdf` yet because Generate still uses it.

Keep QDF, encryption, copy-encryption, and source-encrypted Preserve on legacy
routing.

- [ ] **Step 5: Cover empty-surviving-container and explicit deletion behavior**

Add tests using:

- `null-visible-stale-generation-objstm.pdf` for a Preserve plan whose
  qpdf compressible walk removes stale identities;
- `three-page-objstm.pdf` followed by `pdf.delete_object(...)` for an explicit
  deletion.

Assert:

- stale/removed members do not appear in any `PlannedMember`;
- a removed reference in a surviving array becomes null through
  `renumber_qpdf_refs_in_place_with_removed`;
- if no compressed member survives, `TrailerPlan.form == XrefForm::Table`;
- output reopens and has no dangling compressed xref entry.

Merge the operation's qpdf `removed_refs` with the exact explicit deletion set
only at plan construction; do not teach the serializer to infer removals.

- [ ] **Step 6: Add Preserve byte-parity matrix entries**

Refactor the test helper in `cmp_diff_zero_tests.rs` to accept an explicit
object-stream mode:

```rust
fn rewrite_qpdf_equivalent_mode(
    fixture: &str,
    mode: ObjectStreamMode,
) -> Vec<u8> {
    // existing helper body
    opts.object_streams = mode;
}
```

Add Preserve byte comparisons for:

- `one-page.pdf`;
- `two-page.pdf`;
- `three-page.pdf`;
- `three-page-objstm.pdf`;
- `objstm-lin-od-indirect-length.pdf`;
- `objstm-lin-od-indirect-length-flate.pdf`;
- `kept-indirect-length.pdf`.

Use committed qpdf `preserve.pdf` goldens. If one/two/three-page Preserve bytes
are identical to existing `static-id.pdf`, reference that same file rather than
duplicating it.

- [ ] **Step 7: Run Layer 5 gates**

Run:

```bash
cargo test -p flpdf writer::plain -- --nocapture
cargo test -p flpdf --test object_streams_writer_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_null_visibility_tests
cargo test -p flpdf
cargo test -p flpdf-cli
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: all pass; Disable and Preserve call the new pipeline; Generate and
all specialized modes remain unchanged.

- [ ] **Step 8: Commit and measure Layer 5 coverage**

Run:

```bash
git add crates/flpdf/src/writer.rs crates/flpdf/src/writer/plain/mod.rs crates/flpdf/src/writer/plain/plan.rs crates/flpdf/src/writer/plain/body.rs crates/flpdf/tests/cmp_diff_zero_tests.rs crates/flpdf/tests/object_streams_writer_tests.rs
git commit -m "refactor(writer): route preserve through plain pipeline"
scripts/patch-coverage.sh --base stack/flpdf-2tbp-disable
```

Expected: 100% patch coverage from the clean committed `HEAD`. Add tests and a
new commit for any uncovered executable line, then rerun.

- [ ] **Step 9: Push and persist Layer 5**

Run:

```bash
git push -u origin stack/flpdf-2tbp-preserve
bd close flpdf-2tbp.5 --reason="Preserve now uses the shared pipeline for classic and source-ObjStm inputs with qpdf byte parity and 100% patch coverage."
bd dolt push
```

---

### Task 6: Layer 6 — Generate routing, legacy cleanup, and final corpus

**Beads:** `flpdf-2tbp.6`

**Branch:** `stack/flpdf-2tbp-generate`, based on
`stack/flpdf-2tbp-preserve`

**Files:**
- Modify: `crates/flpdf/src/writer/plain/mod.rs`
- Modify: `crates/flpdf/src/writer/plain/plan.rs`
- Modify: `crates/flpdf/src/writer/plain/body.rs`
- Modify: `crates/flpdf/src/writer/plain/xref.rs`
- Modify: `crates/flpdf/src/writer.rs:3070-4740`
- Modify: `crates/flpdf/tests/cmp_generate_objstm_tests.rs`
- Modify: `crates/flpdf/tests/cmp_diff_zero_tests.rs`
- Modify: `crates/flpdf/tests/object_streams_writer_tests.rs`
- Modify: `tests/golden/regenerate.sh`
- Modify: `docs/superpowers/specs/2026-07-25-qpdf-shaped-plain-writer-pipeline-design.md`

**Interfaces:**
- `plain::eligible` accepts all three `ObjectStreamMode` variants for eligible
  plain inputs.
- `write_pdf_generate` and `write_pdf_containerized_qpdf` are removed.
- The public writer interfaces remain unchanged.

- [ ] **Step 1: Claim and branch Layer 6**

Run:

```bash
bd update flpdf-2tbp.6 --claim
git switch -c stack/flpdf-2tbp-generate
```

- [ ] **Step 2: Write failing Generate routing tests**

Extend the routing counter tests:

```rust
#[test]
fn generate_uses_shared_plain_pipeline() {
    reset_plain_pipeline_calls();
    write_fixture(ObjectStreamMode::Generate);
    assert_eq!(plain_pipeline_calls(), 1);
}
```

Add a test that writes `objstm-gen-nostream-130rev.pdf`, reopens the output,
and asserts exactly two `/Type /ObjStm` containers with 66 members each and
type-2 xref indices `0..65` in each container.

- [ ] **Step 3: Run Generate routing tests and verify the counter failure**

Run:

```bash
cargo test -p flpdf writer::plain::tests::generate_uses -- --nocapture
cargo test -p flpdf --test object_streams_writer_tests nostream_130 -- --nocapture
```

Expected: routing counter test fails because Generate still returns through
`write_pdf_generate`; the structural test remains green on the legacy route.

- [ ] **Step 4: Route Generate through the shared coordinator**

Change `plain::eligible` to:

```rust
matches!(
    mode,
    ObjectStreamMode::Disable
        | ObjectStreamMode::Preserve
        | ObjectStreamMode::Generate
)
```

for otherwise eligible plain inputs. Delete the Generate early-return block
from `write_pdf_full_rewrite_inner`.

Run the focused routing and existing Generate parity tests immediately:

```bash
cargo test -p flpdf writer::plain::tests::generate_uses -- --nocapture
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_generate_objstm_tests
```

Expected: both pass through the new pipeline.

- [ ] **Step 5: Remove superseded plain emitter code**

Delete:

- `generate_invariant`;
- `write_pdf_generate`;
- `write_pdf_containerized_qpdf`;
- the old plain-only container planning, member maps, container allocation, and
  the non-QDF unencrypted body/xref path now owned by `plain` from
  `write_pdf_full_rewrite_inner`;
- Task 3 shadow-comparison-only helpers;
- comments that describe the removed containers-above-max plain architecture.

Retain every helper still used by:

- incremental output;
- QDF;
- encrypt/copy-encrypt/source-encrypted full rewrite, including their generic
  non-QDF body and xref assembly until those routes move to `plain`;
- linearization;
- the public `write_stream_to_buf`.

Use:

```bash
rg -n 'write_pdf_generate|write_pdf_containerized_qpdf|generate_invariant|qpdf_null_visibility' crates/flpdf/src
```

Expected after cleanup: no reference to the three removed functions; any
remaining `qpdf_null_visibility` belongs only to an explicitly excluded legacy
mode and is documented as such.

- [ ] **Step 6: Build the uniform one/two/three-page mode matrix**

In `cmp_generate_objstm_tests.rs`, add Generate comparisons for:

- `one-page.pdf`;
- `two-page.pdf`;
- the existing `three-page.pdf`.

In `cmp_diff_zero_tests.rs`, ensure the same fixtures run under Disable and
Preserve. Use a table-driven test:

```rust
#[test]
fn one_two_three_page_mode_matrix_is_byte_identical_to_qpdf() {
    for fixture in ["one-page", "two-page", "three-page"] {
        for mode in [ObjectStreamMode::Disable, ObjectStreamMode::Preserve] {
            assert_mode_cmp_diff_zero(fixture, mode);
        }
    }
}
```

Generate remains in `cmp_generate_objstm_tests` because it requires xref-stream
goldens. Add missing qpdf references to `tests/golden/regenerate.sh` with:

```bash
qpdf --static-id --object-streams=generate \
  "$FIX/$fixture.pdf" \
  "$REF/$fixture/generate.pdf"
```

Do not regenerate or replace an existing golden unless qpdf 11.9.0 produces
different bytes and the difference is explained in the commit.

- [ ] **Step 7: Run the full curated parity corpus**

Run:

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_generate_objstm_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_null_visibility_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests
cargo test -p flpdf --test object_streams_writer_tests
cargo test -p flpdf --test deterministic_id_xref_stream_tests
cargo test -p flpdf --test newline_before_endstream_tests
```

Expected: every test passes; the first four commands prove qpdf byte parity
for the changed and adjacent paths.

- [ ] **Step 8: Run all workspace quality gates**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf
cargo test -p flpdf-cli
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
```

Expected: all commands exit zero. Existing explicitly ignored tests may remain
ignored; there must be no failure.

- [ ] **Step 9: Update the design status and commit Layer 6**

Append a delivery-status section to the design spec containing:

- Beads IDs `.1` through `.6`;
- branch names;
- the exact commit IDs already produced by Layers 1 through 5 and the Layer 6
  commit subject;
- exact parity test counts;
- confirmation that QDF/encryption/linearization/incremental routing stayed
  excluded.

Stage and commit:

```bash
git add crates/flpdf/src/writer.rs crates/flpdf/src/writer/serialize.rs crates/flpdf/src/writer/plain crates/flpdf/tests/cmp_generate_objstm_tests.rs crates/flpdf/tests/cmp_diff_zero_tests.rs crates/flpdf/tests/object_streams_writer_tests.rs tests/golden/regenerate.sh tests/golden/references docs/superpowers/specs/2026-07-25-qpdf-shaped-plain-writer-pipeline-design.md
git commit -m "refactor(writer): unify plain rewrite pipeline"
```

- [ ] **Step 10: Measure final committed-HEAD patch coverage**

Run:

```bash
scripts/patch-coverage.sh --base stack/flpdf-2tbp-preserve
```

Expected: the script exits zero and reports 100% for every changed executable
line under `crates/flpdf/src`.

If it reports uncovered lines:

1. add focused tests for the exact branches;
2. commit them as `test(writer): cover unified plain pipeline`;
3. rerun the full focused parity commands from Step 7;
4. rerun coverage from the new clean `HEAD`.

- [ ] **Step 11: Reconcile the old roadmap issues**

Verify their acceptance against committed tests:

```bash
bd show flpdf-9hc.20.29
bd show flpdf-9hc.20.30
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_generate_objstm_tests
```

If the fixed-order ObjStm/xref serialization, minimal `/W`, Predictor 12,
`/Index` omission, header formatting, and one/two/three-page goldens all pass,
close both:

```bash
bd close flpdf-9hc.20.30 --reason="Committed qpdf 11.9.0 Generate golden corpus and feature-gated cmp matrix cover one/two/three-page plus boundary fixtures."
bd close flpdf-9hc.20.29 --reason="Shared writer pipeline emits fixed-order ObjStm/xref streams with minimal W, Predictor 12, Index omission, and qpdf byte identity."
```

If any listed criterion is not met, leave the corresponding issue open and
replace its stale description/notes with the exact remaining fixture or byte
gap. Do not close it merely because `flpdf-2tbp.6` is complete.

- [ ] **Step 12: Close the refactor Beads and push all state**

Run:

```bash
bd close flpdf-2tbp.6 --reason="Generate routed through shared pipeline; legacy plain emitters removed; full parity, workspace, rustdoc, and 100% patch-coverage gates passed."
bd close flpdf-2tbp --reason="All six bottom-up writer layers completed and pushed."
bd dolt push
git push -u origin stack/flpdf-2tbp-generate
git status --short --branch
git rev-parse HEAD
git rev-parse '@{upstream}'
```

Expected:

- Beads push succeeds;
- Git push succeeds;
- working tree is clean;
- `HEAD` equals the upstream ref;
- every `.1` through `.6` child is closed;
- the parent is closed only after every acceptance criterion is evidenced.

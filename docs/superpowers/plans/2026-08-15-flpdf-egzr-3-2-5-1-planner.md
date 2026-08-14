# flpdf-egzr.3.2.5.1 — ObjectHandle writer planner cutover

## Scope

This layer moves qpdf 11.9.0 object-stream planning and membership traversal to
the live `ObjectHandle` graph. It covers:

- `writer/object_streams.rs`: eligibility, trailer-seeded DFS, stale-generation
  suppression, signature/encryption/stream/`/Length` exclusions, and
  preserve/generate membership;
- `rewrite_renumber.rs`: source-backed and synthetic ObjStm placement and
  Catalog-first numbering; and
- the shared planner call sites in `writer/plain/plan.rs` and the existing
  linearization/body validation interfaces that consume the planner predicate.

The body serializer, stream provider/filter pipeline, xref/trailer byte
emission, top-level writer cutover, and linearization ownership remain in the
dependent layers (`flpdf-egzr.3.2.5.2`, `.3.2.5.3`, `.3.2.5.4`, and
`flpdf-3yn9.5`). `qd46` remains downstream.

## qpdf oracle boundary

The pinned source at `/home/ubuntu/.cache/flpdf/qpdf-11.9.0` is authoritative:

| qpdf 11.9.0 source | ported responsibility |
| --- | --- |
| `QPDF.cc:2381-2474` | `getObjectStreamData` and `getCompressibleObjGens`: trailer-seeded LIFO DFS, visited-by-object-number, stale-generation `upper_bound` behavior, visible dictionary keys, array order, stream-dictionary traversal, `/Length` omission, signature fields, and `/Encrypt` exclusion |
| `QPDFWriter.cc:1938-2006` | preserve/generate source membership, `ceil(n/100)` stream count, and even member partitioning |
| `QPDFWriter.cc:2058-2184` | setup policy, reverse membership, page/root exclusions owned by later linearization/encryption slices |
| `QPDFWriter.cc:1072-1141,2907-3044` | canonical enqueue and standard writer placement order |
| `QPDF.cc:1980-2005` | `replaceObject`/`removeObject` mutate only the requested cache slot; already materialized ObjStm members remain live |

The planner uses `Pdf::trailer_handle`, `Pdf::get_object_handle`,
`Pdf::resolve_object_handle`, and fallible handle inspection. It does not add a
new `Object`/`resolve_borrowed` bridge to the moved planner responsibility.
The reader's small promotion helper exists only to preserve qpdf's already
materialized compressed-member cache entries across a legacy public
`set_object`/`delete_object` mutation while the remaining reader consumers are
still being cut over.

## RED → GREEN evidence

Focused tests added or exercised by this layer:

- live Catalog-handle mutation changes `compressible_objgens` order;
- indirect signature `/Type` resolution and encryption exclusion;
- source ObjStm member promotion across deletion and null replacement;
- source-backed renumbering follows live handle edges;
- preserve/generate object-stream writer fixtures and null/stale-generation
  behavior.

Exact verification commands:

```text
cargo fmt --all -- --check
cargo test -p flpdf --lib writer::object_streams::tests
cargo test -p flpdf --lib rewrite_renumber::tests
cargo test -p flpdf --lib writer::plain::plan::tests
cargo test -p flpdf --lib linearization::plan::tests
cargo test -p flpdf --test object_streams_writer_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_null_visibility_tests
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
cargo test
```

The final patch-coverage command is run from a clean committed worktree; its
result, PR CI, review readback, Beads dependency check, and `bd dolt push`
output are recorded in the Bead before closure.

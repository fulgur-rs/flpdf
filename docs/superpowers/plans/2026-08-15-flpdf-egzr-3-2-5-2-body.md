# flpdf-egzr.3.2.5.2 — ObjectHandle writer body cutover

## Scope

This layer moves the canonical plain-writer body path and its ObjStm body
emission from resolved `Object` snapshots to the live `ObjectHandle` graph.
It covers:

- `writer/plain/body.rs`: source-object dictionaries, arrays, signatures,
  reference remapping, stream dictionaries, provider-backed stream data, and
  the qpdf two-attempt stream pipe;
- `writer/object_streams.rs`: the two-pass ObjStm pair-table/body emitter with
  an ObjectHandle serializer callback; and
- the existing `writer/serialize.rs` stream-payload framing boundary, which
  receives the already-selected handle/pipeline bytes and keeps `/Length`
  independent from the optional framing LF.

The planner and membership policy are supplied by
`flpdf-egzr.3.2.5.1`. Xref/trailer serialization is the next layer
(`flpdf-egzr.3.2.5.3`), and the excluded-mode/top-level `writer.rs` consumer
cutover remains `flpdf-egzr.3.2.5.4`. Linearization (`flpdf-3yn9.5`) and stream
provider implementation semantics (`flpdf-3yn9.7`) remain separately owned.

## qpdf oracle boundary

The pinned source at `/home/ubuntu/.cache/flpdf/qpdf-11.9.0` is authoritative:

| qpdf 11.9.0 source | ported responsibility |
| --- | --- |
| `QPDFWriter.cc:1239-1314` | stream filtering decision, fresh-buffer retry, and `pipeStreamData` flags: `suppress_warnings=false`, `will_retry=(attempt==1)` |
| `QPDFWriter.cc:1318-1603` | live object unparse shape: array spacing, dictionary null suppression, signature `/Contents` hex spelling, stream-dictionary handling, and reference-vs-direct-child identity |
| `QPDFWriter.cc:1606-1623` | stream framing handoff after the selected payload and dictionary are serialized |
| `QPDFWriter.cc:1761-1796` | ObjStm member emission and the two-pass pair-table/body layout; the enclosing ObjStm stream is the encryption boundary |
| `QPDFWriter.cc:842-847, 1480-1510, 1528-1599, 2244-2256` | writer data-key and special-dictionary boundaries retained by the already-merged ObjectHandle/encryption primitives and the downstream top-level consumer cutover |

The canonical body path now resolves each source or ObjStm member as an
`ObjectHandle`, remaps child references through the plan, and serializes with
`unparse_object_with_ref_map_and_removed` / stream-dictionary counterparts.
The ObjStm pair table is still built in a first pass over the same member
sequence, while the body callback receives the live handle and member index.
No temporary `Object` tree is created for the canonical plain route.
For source object streams with `/Extends`, the body keeps qpdf's source
dictionary reference but does not synthesize inherited member values: qpdf
11.9.0's writer preserves `/Extends` while the extension stream's own member
cache remains authoritative (`QPDFWriter.cc:1731-1739`, `QPDF.cc:1700-1751`).

## RED → GREEN evidence

Focused tests cover:

- live-handle array/dictionary/signature unparse rules and null visibility in
  ObjStm bodies;
- source-body reference remapping and removed-reference nulling;
- source-backed ObjStm `/Extends` lookup through a live stream handle;
- provider-backed body emission with both qpdf retry attempts and exact flags;
- decoded, preserved, lone-Flate, recovered-EOL, and no-compression stream
  paths; and
- ObjStm two-pass offsets with both compressed and uncompressed containers.

Exact verification commands for the layer:

```text
cargo fmt --all -- --check
cargo test -p flpdf --lib writer::plain::body::tests
cargo test -p flpdf --lib writer::object_streams::tests::emit_objstm_body_from_handles_uses_live_qpdf_unparse_rules
cargo test -p flpdf --test object_streams_writer_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_null_visibility_tests
cargo test
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

The final patch-coverage result, full workspace tests, PR CI, review readback,
Beads dependency check, and `bd dolt push` output are recorded on
`flpdf-egzr.3.2.5.2` before the issue is closed after its dependent PR is
merged.

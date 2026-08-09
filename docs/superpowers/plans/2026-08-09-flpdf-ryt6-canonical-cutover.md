# Plan: cut `ObjectHandle` resolution over to the qpdf-shaped canonical resolver

**Goal:** Make `ObjectHandle` resolution follow qpdf 11.9.0 directly: one canonical object cache, one live parser, pipe-time stream decryption, and no legacy `Object` fallback or metadata-only reparse on that path.

**Scope:** This branch cuts over `Pdf::resolve_object_handle` and completes the canonical resolver's source classes needed by that API, including compressed object streams. The raw `Pdf::resolve`/`resolve_borrowed` API remains an explicitly legacy consumer boundary until `flpdf-egzr.3.2` migrates its in-tree consumers; it must not be called as a fallback from canonical resolution.

**Oracle:** Resolve the pinned qpdf 11.9.0 source with `scripts/fetch-qpdf-source.sh --print-path`, and use live `/usr/bin/qpdf` output. Relevant source seams are `QPDF::resolve`/`resolveObjectsInStream` (`libqpdf/QPDF.cc:1699-1788`), `QPDF::readObjectInStream` (`libqpdf/QPDF.cc:1450-1475`), `QPDFParser::withDescription`/`addScalar` (`libqpdf/QPDFParser.cc:413-443`), and `QPDFValue::getDescription` (`libqpdf/QPDFValue.cc:13-61`).

## 1. Pin the canonical contracts with RED tests

- Add object-description tests for qpdf's single `find`/`replace` behavior: no `$$` escape convention, one `$PO`/`$OG` replacement per template, and one `$VD` replacement in the already-rendered child string.
- Add a compressed-xref fixture whose ObjStm member is resolved through `ObjectHandle::try_dereference`; assert the canonical handle, member offset, nested indirect identity, and parser description.
- Add a resolution diagnostic regression that resolves a malformed uncompressed object through `resolve_object_handle` and asserts one warning, proving that the old parse-plus-reparse route is gone.
- Add a stream regression that observes the original `/Filter` and `/DecodeParms` handles after resolution; explicit `/Crypt` must not be removed or re-indexed during resolution.
- Run each new test individually and record the expected failure before implementation.

## 2. Make description expansion source-faithful

- Remove the input-dollar escaping adapter and implement qpdf's first-match replacement semantics in `ObjectSlot::get_description`.
- Keep parser-created `null` values without descriptions or parsed offsets.
- Update existing tests that encode the former compatibility escaping behavior to the qpdf source contract, with the pinned source citation beside the test.

## 3. Complete canonical source resolution

- Extend `ResolverHandle::resolve_indirect` with qpdf's type-2 `resolveObjectsInStream` behavior.
- Resolve the object-stream handle canonically, read raw bytes through `get_raw_stream_data`, decode the stream filters without mutating the source dictionary, parse every active member with the same qpdf-shaped direct-object parser, and install the matching xref members in the canonical cache.
- Preserve member-local parsed offsets and descriptions, resolve only members still owned by the selected xref entry, and guard repeated object-stream work with `resolved_object_streams`.
- Resolve free/absent entries to the canonical null/missing state as qpdf does; propagate actual I/O and parse errors instead of delegating to legacy resolution.
- Port `QPDF::readStream`'s repair path: when `repair` is enabled, catch an unusable `/Length` or missing `endstream`, scan for the first qpdf token boundary, retain the recovered source length lazily, and preserve the ordered diagnostics.

## 4. Delete the canonical legacy bridge

- Replace `Pdf::resolve_object_handle`'s `resolve_to_cache`-first logic with a direct `handle.try_dereference()` call and the canonical missing/null contract.
- Delete the native-parse/lift fallback, transformed-stream composition, and legacy diagnostic synchronization from this method.
- Ensure all ObjectHandle-native consumers and stream/filter accessors use the canonical handle graph. Keep raw `Pdf::resolve` as an explicitly legacy boundary with its own bounded recovery contract; the canonical path must never call back into raw resolution, and raw resolution must not bypass its own recovery bounds through canonical parsing.
- Mark the remaining raw API boundary for `flpdf-egzr.3.2`. A narrowly guarded initial-xref-reconstruction handoff may remain for existing raw callers, but it is not a general bridge: it is allowed only before `reconstructed_xref` becomes true and never routes canonical resolution back through raw `Object` parsing.

## 5. Verify and hand off

- Run `cargo fmt --all -- --check`.
- Run the focused resolver, parser, object-handle, stream-filter, and reader tests, then `cargo test -p flpdf` and the CLI compatibility tests.
- Run `git diff --check`, inspect the changed-line diff, push the branch, and persist Beads with `bd dolt push`.
- Do not resolve review threads in this implementation step; reply only after the verified code is pushed and the original inline evidence is read back.

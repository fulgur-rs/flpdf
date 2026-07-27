# JSON Stacked PR Review Remediation Design

## Goal

Address every actionable unresolved review finding on PRs #559 through #562
without changing qpdf 11.9.0 JSON bytes, partial-output boundaries, diagnostic
ordering, or the four-layer stacked-PR ownership model.

## Oracle boundaries

qpdf has two distinct dictionary-ordering contracts:

- Generic `JSON` dictionaries are maps keyed by the encoded JSON key
  (`libqpdf/JSON.cc`).
- `QPDF_Dictionary::writeJSON` iterates raw PDF-name order and escapes each key
  only at the sink (`libqpdf/QPDF_Dictionary.cc`).

flpdf must preserve both. `json::Json` therefore remains a map-sorted shared
value matching qpdf's generic JSON component. Exact PDF inspection output
continues through `OrderedPdfJson` and
`write_qpdf_json_v2_selected_objects_with_options`, which preserve raw PDF-name
order and fixed metadata order until emission.

The old materialized inspection builders cannot promise those exact output
bytes because their return type is generic `Json`. They will no longer be part
of the public byte-emission surface. The supported public replacement is the
incremental sink API. This is an intentional pre-1.0 breaking change and must
be marked as such for release-plz.

## Layered changes

### PR #559: JSON core

- Batch Base64 output into bounded chunks instead of issuing one downstream
  write for each three input bytes.
- Keep at most two unencoded bytes between blob callback writes.
- Preserve standard alphabet, padding, output bytes, and already-written
  prefix behavior on downstream failure.
- Refresh the standalone fuzz lockfile so `cargo check --locked` succeeds.

### PR #560: JSON parser

No new production change. The current parser already retries
`ErrorKind::Interrupted`; retain and rerun the focused regression tests.

### PR #561: JSON validation

- Snapshot dictionary members into keyed maps instead of vectors.
- Preserve qpdf's two passes: schema order first for missing/recursive errors,
  then value order for extra-key errors.
- Reduce lookup work from repeated linear scans to ordered-map lookups.
- Do not add a handler-reset API; the outdated handler-clear review scenario is
  not expressible by the current public shared-handler model.

### PR #562: JSON integration

- Keep the qpdf-absent test guard, raw-name ordered sink, and non-regular output
  handling already present.
- Restrict materialized inspection helpers that cannot preserve qpdf byte order
  to crate-internal use.
- Document the incremental exact-output replacement and mark the removal as a
  Conventional Commits breaking change so release-plz performs a pre-1.0 minor
  bump.

## Verification

Each modified layer is measured against its immediate parent and must report
100% patch coverage. The final stack also runs formatting, all-target/all-feature
clippy, workspace tests, strict rustdoc, the standalone fuzz locked build, and
live qpdf exact-output probes. GitHub replies and thread resolution remain
outside this change unless separately authorized.

# extensions-dictionary version controls design

**Target:** `flpdf-25kg.7.2.1` under `flpdf-25kg.7.2`

**Goal:** Make the qtest `extensions-dictionary.test` suite's top-level
`--min-version` and `--force-version` invocations produce the same version
header, Adobe extension dictionary, and qpdf `PDFVersion` observations as
qpdf 11.9.0.

## Scope and observable contract

The target suite has 156 rows: four source PDFs, nine version specifications,
both minimum and forced version operations, `test_driver 34` observations, and
non-QDF/QDF file comparisons for forced `1.8.5`. The qtest command shape is
`qpdf --static-id --min-version=M.m[.E] INPUT OUTPUT` (or
`--force-version=M.m[.E]`). The output header contains only `M.m`; the optional
third component is the Adobe extension level represented by Catalog
`/Extensions /ADBE /ExtensionLevel`.

The existing `rewrite --min-version` and `rewrite --force-version` behavior
remains supported. The top-level qpdf-shaped CLI, the rewrite subcommand, and
writer configuration must accept the same complete version syntax. Non-ADBE
developer prefixes under `/Extensions` must survive ADBE replacement or
removal.

The qtest parity manifest is owned by the separate `flpdf-qtest` repository.
This flpdf change verifies the paired run artifacts but does not vendor or
edit qtest files; manifest reclassification is a dependent follow-up.

## qpdf source model

The pinned qpdf 11.9.0 source is authoritative:

- `libqpdf/QPDFJob.cc:2833-2844` splits a full version string at the second
  dot into a base version and extension level.
- `libqpdf/QPDFJob.cc:2913-2924` applies the accumulated input version floor,
  then `--min-version`, then `--force-version` to each writer.
- `libqpdf/QPDFWriter.cc:217-265` compares minimum versions by major/minor
  and uses the larger extension level only when those versions tie.
- `libqpdf/QPDFWriter.cc:2176-2182` selects the minimum pair unless a forced
  pair is present, in which case the forced pair wins exactly.
- `libqpdf/QPDFWriter.cc:1356-1435` owns Catalog `/Extensions` handling:
  create ADBE when a positive extension level is required, replace stale ADBE,
  preserve other developer prefixes, and remove ADBE (and an empty container)
  when the final extension level is zero.
- `libqpdf/QPDF.cc:2323-2345` reads the header version and ADBE extension level
  independently for `PDFVersion`.
- qpdf `qtest/test_driver.cc:1252-1262` prints the exact version, extension
  level, Catalog `/Extensions`, and reconstructed `PDFVersion` values used by
  this suite.

## Architecture and data flow

Add one shared parser at the version/configuration boundary that converts
`M.m[.E]` into `(base_version: M.m, extension_level: E)`. Header/source
version parsing remains distinct from the optional CLI extension component, so
the emitter never receives an invalid `M.m.E` header string.

The CLI maps both top-level and rewrite-subcommand options through this one
conversion and supplies the pair to the existing canonical
`WriterConfiguration`/`PdfWriter` route. Top-level normal rewrite, linearize,
page-operation, and rewrite-subcommand construction must all carry the pair;
the attachment writer paths either carry it as well or reject it before
opening input rather than silently dropping a qpdf writer option.

The writer keeps ownership of final pair comparison and Catalog mutation. No
CLI-specific `/Extensions` formatter or second writer implementation is added.
The existing multi-input floor from `flpdf-jq0z` is reused; explicit minimum
version is combined with that floor before force-version is applied.

## Error and precedence behavior

For valid values, minimum-version comparison is pairwise: a higher M.m
replaces the prior pair and resets its extension level; equal M.m keeps the
larger extension level; a lower M.m is ignored. A valid force-version replaces
both version and extension level exactly, including extension level zero.
Invalid values are rejected at the CLI boundary with the existing
qpdf-shaped usage diagnostic, and library setters remain recoverable without
panic. The emitted header always uses the base M.m string.

## Verification

Add red tests before production changes for:

1. version-spec parsing of the nine qtest values, including `1.8.0` and
   extension-bearing `1.7.1`/`1.8.5`;
2. top-level and rewrite routing, minimum/force precedence, and invalid input;
3. Catalog ADBE creation/replacement/removal while preserving `/Potato`;
4. exact qpdf comparison for all four extension-dictionary inputs and the
   forced `1.8.5` QDF/non-QDF outputs.

Run the focused 156-row qtest suite from a disposable qtest datadir, retain
`harness.log` and `qtest-results.xml` from the same run, then run format,
focused tests, full flpdf/flpdf-cli tests, strict rustdoc, and all-features
clippy before claiming completion.

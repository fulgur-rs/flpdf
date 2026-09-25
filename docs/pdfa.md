# PDF/A preservation scope

This document describes what flpdf's qpdf-compatible writer preserves during
an ordinary full rewrite. It does not claim that flpdf creates, repairs, or
validates PDF/A files. This scope is tracked by `flpdf-9hc.21`.

## Reachable document data

An ordinary rewrite carries forward objects reachable from the document
catalog and trailer. The qpdf 11.9.0 writer starts from the trailer's `/Root`
and follows indirect references and direct arrays and dictionaries
(`QPDFWriter::enqueueObjectsStandard` and `QPDFWriter::enqueueObject` in
`libqpdf/QPDFWriter.cc:1072-1157,2907-2925`). Therefore reachable PDF/A-related
entries such as `/OutputIntents` (including `/DestOutputProfile`), `/MarkInfo`,
`/StructTreeRoot`, `/Lang`, and `/AF` remain part of the output object graph.
By default this does not preserve unreachable objects (`--preserve-unreferenced`
enqueues every cached object before `/Root` and does keep them), and it does not
guarantee that an input file was conforming or that a rewrite leaves it
conforming.

Preservation is about the reachable PDF values and stream payloads, not stable
object numbers or identical serialization. The writer may renumber objects and
apply its stream-filter, length, and framing policies.

## Metadata streams and stream framing

For cleartext `/Metadata` streams, qpdf 11.9.0 decodes the stream data and
writes it without a filter when the decode succeeds
(`QPDFWriter::willFilterStream` in `libqpdf/QPDFWriter.cc:1238-1299`). A filter
that cannot be decoded falls back to a raw second attempt that keeps the
original filter (`:1304-1313`), and disabling filter-on-write vetoes decoding
altogether. The decoded XMP packet is the content to preserve; the original
`/Filter`, `/Length`, and encoded bytes are not a byte-for-byte preservation
contract. The decoded payload of an ICC profile
referenced by `/DestOutputProfile` is likewise preserved when it can be
decoded; its encoded representation follows the selected writer options.

PDF/A requires a newline before `endstream`. qpdf's
`--newline-before-endstream` option inserts that newline and can help retain
compliance when rewriting an already compliant file. qpdf explicitly says
this does not teach it to generate PDF/A-compliant files
(`manual/cli.rst:1152-1165`). The option does not validate or repair a file.

## Page merge and split operations

The ordinary-rewrite preservation scope does not mean every page operation
carries every source document's catalog data:

- With `--pages`, qpdf keeps document-level information from the primary input
  PDF. Other inputs contribute selected pages; their document-level metadata
  is not merged into the primary catalog. `--empty` starts with an empty
  primary document (`manual/cli.rst:2517-2528`; `QPDFJob::handlePageSpecs` in
  `libqpdf/QPDFJob.cc:2360-2633`).
- With `--split-pages`, qpdf creates an empty PDF for each output and copies
  pages into it. Its manual says outlines, threads, and other document-level
  features are not preserved (`manual/cli.rst:1512-1519`; `QPDFJob::doSplitPages`
  in `libqpdf/QPDFJob.cc:2940-3027`).

Check the document-level information needed by your workflow after merging or
splitting, and validate PDF/A conformance separately.

## Conformance validation

flpdf does not provide PDF/A conformance validation, and neither flpdf nor
qpdf's `--check` establishes PDF/A conformance. qpdf says it does not generate
PDF/A-compliant files, and its `--check` command checks file structure,
encryption, linearization, and stream encoding rather than PDF/A-level
semantic conformance (`manual/cli.rst:3275-3305`). Use a dedicated validator,
such as [veraPDF](https://docs.verapdf.org/validation/), when conformance must
be established.

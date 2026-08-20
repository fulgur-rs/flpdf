# qpdf Repeated Attachment Segment Design

## Goal

Make repeated top-level `--add-attachment` segments reachable from the flpdf
CLI and preserve qpdf's one-job batch behavior.

## Oracle and responsibility

qpdf creates a fresh attachment configuration for every repeated top-level
flag (`libqpdf/QPDFJob_argv.cc:297-301`). Each terminated segment is finalized
by `QPDFJob::AttConfig::endAddAttachment`, which appends one record to
`attachments_to_add` (`libqpdf/QPDFJob_config.cc:911-936`). The job then loops
over that vector once and aggregates duplicate keys after processing all
records (`libqpdf/QPDFJob.cc:2046-2078`).

The current flpdf CLI stores repeated clap occurrences in a flat
`Vec<String>`, then parses one segment and invokes singular `add_attachment`.
That loses segment boundaries and prevents the existing batch API from being
used by the CLI.

## Architecture

Extract `--add-attachment` segments from the raw argv after the existing
overlay extraction, retaining the first segment in the residual argv so clap
continues to validate and dispatch the operation. Store every captured segment
as a `Vec<Vec<String>>`, parse each with the existing segment parser, and pass
the resulting `Vec<AttachmentAddOptions>` to one `QPDFJob::add_attachments`
call. This avoids enabling clap's unstable grouped-`Vec<Vec<T>>` feature and
keeps option parsing local to the existing qpdf-style raw-segment boundary.

## Scope and non-goals

In scope:

- repeated `--add-attachment` segment extraction and ordering;
- one batch job call for all parsed attachment options;
- CLI regression coverage for two distinct keys and the existing duplicate
  aggregation path.

Out of scope:

- changing the public `AttachmentAddOptions` or `QPDFJob::add_attachments` API;
- changing singular attachment syntax or other value-terminated segments;
- enabling global unstable clap features.

## Live evidence

With two terminated segments and explicit keys, qpdf exited successfully and
listed `one` and `two`. The current flpdf CLI exited 2 with an unknown-sub-flag
diagnostic and produced no output because clap flattened the two occurrences.

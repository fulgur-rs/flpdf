# qpdf Attachment Provider Error and FIFO Design

## Goal

Remove flpdf's extra attachment preflight open so provider-backed attachments
have the same open cadence as qpdf, while preserving qpdf's fatal
`open PATH: error` diagnostics.

## Oracle and responsibility

qpdf's path Filespec factory delegates directly to
`QPDFEFStreamObjectHelper::createEFStream(qpdf, QUtil::file_provider(path))`
(`libqpdf/QPDFFileSpecObjectHelper.cc:83-91`). The provider is invoked by
`newFromStream` to compute `/Params /Size` and `/CheckSum`, and qpdf's writer
invokes it again when emitting the stream (`libqpdf/QPDFEFStreamObjectHelper.cc:90-107,
131-148`). There is no separate preflight open in `QPDFJob::addAttachments`
(`libqpdf/QPDFJob.cc:2046-2078`).

The current flpdf callback provider already returns an `Error` from
`File::open`, but `job::attachments::add_attachments` opens the path once
before constructing the provider. That consumes an unexpected FIFO connection
and keeps qpdf-style error mapping in the wrong layer.

## Architecture

Move the existing qpdf-style open-error formatter into `filespec_helper.rs`
and apply it only to the path provider's `File::open` call. Remove the job
preflight. The existing provider `Result` channel then carries the mapped error
through `pipe_stream_data` and `new_from_stream`, while normal files retain the
same lazy size/checksum and writer-time reads.

## Scope and non-goals

In scope:

- two-open FIFO behavior matching qpdf;
- missing-file and permission-denied diagnostic preservation;
- provider-layer ownership of path-open mapping.

Out of scope:

- changing provider retry semantics or stream read-error policy;
- materializing attachment bytes eagerly;
- changing qpdf's warning for a provider that returns `false` without an
  error.

## Live evidence

With one-shot FIFO producers, qpdf succeeded with two producer connections;
current flpdf timed out with two and succeeded with three, proving the extra
preflight open. The post-fix probe must match qpdf's two-connection success.

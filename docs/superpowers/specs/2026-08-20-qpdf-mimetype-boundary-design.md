# qpdf Attachment Mimetype Boundary Design

## Goal

Match qpdf 11.9.0's responsibility boundary for attachment mimetype
validation: the command-line/configuration parser rejects values without a
slash, while the QPDFJob attachment component accepts and applies any value it
is given.

## Oracle and responsibility

- `QPDFJob::AttConfig::mimetype` checks `parameter.find('/')` and emits
  `mime type should be specified as type/subtype` when no slash is present
  (`libqpdf/QPDFJob_config.cc:888-895`).
- `QPDFJob::addAttachments` only checks whether the value is non-empty, then
  passes it to `QPDFEFStreamObjectHelper::setSubtype`
  (`libqpdf/QPDFJob.cc:2057-2065`).
- The current flpdf CLI parser stores `--mimetype` without validating it, and
  `job::attachments::add_attachments` performs the check inside the public
  library component. That produces the right common CLI error but gives the
  library route a responsibility qpdf does not assign to its component.

## Architecture

Keep mimetype as raw bytes in `AttachmentAddOptions`. Add the qpdf slash check
to `parse_add_attachment_segment`, remove the check from
`QPDFJob::add_attachments`, and preserve the existing stream subtype setter.
This keeps CLI behavior stable while allowing direct library callers to use the
same raw subtype values that qpdf's lower-level job consumer accepts.

## Scope and non-goals

In scope:

- validation of `--mimetype` at the CLI/configuration parsing boundary;
- direct library acceptance of a mimetype without `/`;
- regression coverage for both boundaries and the unchanged diagnostic.

Out of scope:

- MIME grammar normalization beyond qpdf's single slash-presence check;
- changing subtype serialization or the attachment stream object model;
- changing qpdf's CLI error footer or unrelated attachment validation.

## Live evidence

With `--mimetype=textplain`, qpdf and the current flpdf CLI both exit 2 and do
not create output. qpdf reports the same primary diagnostic plus its standard
usage footer; flpdf reports the same primary diagnostic. The pinned source
confirms that the shared error comes from the CLI/config layer, not
`addAttachments`.

# flpdf-25kg.3.38.3 direct primary AcroForm preservation

## Goal

Preserve the direct-versus-indirect representation of a primary `/AcroForm`
through a qpdf-compatible `--pages` merge. An inline primary dictionary must
remain inline when qpdf mutates its `/Fields`; no new indirect object should be
introduced solely by the page-merge consumer.

## Oracle boundary

Pinned qpdf 11.9.0 defines the responsibility in
`libqpdf/QPDFAcroFormDocumentHelper.cc`:

- `:38-46` (`getOrCreateAcroForm`) returns the existing `/AcroForm` dictionary
  unchanged, whether the catalog value is direct or an indirect dictionary;
  only a missing/non-dictionary value is replaced with a new indirect dict.
- `:49-59` (`addFormField`) mutates the returned AcroForm handle in place and
  appends `/Fields` without re-indirectizing an existing direct dictionary.
- `:62-110` (`addAndRenameFormFields`) uses the same AcroForm ownership path
  after its collision analysis.

The current flpdf page-job consumer at `job/page_merge.rs:629` used
`ensure_acroform_ref`, a legacy ref-valued helper that always replaced an
inline dictionary with a new indirect object. The canonical ObjectHandle
helper at `acroform_document_helper.rs:1245-1265` already preserves direct
handles; this slice exposes that route to the page-job consumer without
changing the old helper's semantics for its remaining callers.

## Design

- Use `canonical_get_or_create_acroform` from the canonical page-merge route.
- Keep existing indirect `/AcroForm` values indirect and create an indirect
  object only when qpdf would create one.
- Do not add a compatibility adapter, directness sentinel, or a second
  AcroForm implementation.
- Verify both representation and field contents against live qpdf 11.9.0.

## Acceptance criteria

- A primary with an inline `/AcroForm << /Fields [...] >>` remains direct after
  `--pages` merge in both qpdf and flpdf.
- The merged fields and page positions remain equal to qpdf.
- Existing indirect AcroForm, repeated-page, foreign-field, and fields-less
  AcroForm tests remain green.
- fmt, strict private-item rustdoc, all-features clippy, workspace tests,
  qpdf module/deviation checks, and fresh stacked-PR patch coverage pass.

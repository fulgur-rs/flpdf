# Form Resource-Pruning Parser Callback ObjectHandle Design

## Goal

Remove the production Form-XObject resource-pruning and ResourceReplacer
paths' legacy `Object`/`ParserCallbacks` boundary. Both paths will parse
decoded content through the existing `ObjectHandleParserCallbacks` route and
retain qpdf 11.9.0's resource-name, offset, inline-image, diagnostic, EOF, and
malformed-content behavior.

This is a bounded child of `flpdf-egzr.3.2.6`; it does not complete the page
group aggregate or the workspace-wide raw `Object` removal.

## qpdf oracle contract

The pinned qpdf source at `/home/ubuntu/.cache/flpdf/qpdf-11.9.0` establishes
the following one-to-one responsibilities:

- `QPDFObjectHandle::ParserCallbacks` receives `QPDFObjectHandle` values and
  content spans (`include/qpdf/QPDFObjectHandle.hh:202-227`).
- `parseContentStream_internal` parses ordinary objects and calls the handle
  callback; after `ID`, it emits an inline-image handle or a warning-only EOF
  diagnostic (`libqpdf/QPDFObjectHandle.cc:1776-1847`).
- `ResourceFinder` stores the last name and records it when a resource
  operator follows (`libqpdf/qpdf/ResourceFinder.hh:6-22`,
  `libqpdf/ResourceFinder.cc:3-56`). It has no raw `Object` callback surface.
- `QPDFPageObjectHelper::removeUnreferencedResourcesHelper` parses each page
  or Form with `ResourceFinder`, aborts pruning after parser warnings/errors,
  and shallow-copies only the categories it mutates
  (`libqpdf/QPDFPageObjectHelper.cc:539-649`).
- `QPDFAcroFormDocumentHelper` uses the same content-token resource-name
  machinery for its ResourceReplacer path; the qpdf ResourceFinder probe
  exercises that shared callback contract.

The live oracle is `/usr/bin/qpdf` 11.9.0. Existing ResourceFinder probe cases
cover the operator table, escaped names, malformed content, inline-image
events, and incomplete inline images; those cases remain the differential
acceptance authority. A real-PDF `--remove-unreferenced-resources=yes` probe
also showed that a malformed inline-image header does not veto pruning: qpdf
removed unused `/Font` entries and left the empty category dictionary.

## Current route inventory

The canonical route already exists:

- `content_stream.rs::ObjectHandleParserCallbacks` and
  `parse_content_stream_handles` emit live `ObjectHandle` values.
- `resource_finder.rs::handle_object_handle` and its
  `ObjectHandleParserCallbacks` implementation match qpdf's handle callback.
- The page pruning route in `resources.rs` already uses the canonical parser
  for the page and direct Form target.

The remaining mixed routes are the Form pre-pass and ResourceReplacer scan:

- `resources.rs::collect_used_names_for_form` calls the legacy
  `parse_content_stream_data`.
- `resources.rs::ResourceCallbacks` stores `Vec<Object>` and implements the
  legacy `ParserCallbacks` trait.
- `resource_replacer.rs::replace_resource_names` still feeds
  `ResourceFinder` through the raw recovering parser.
- `resource_finder.rs` retains `handle_object_borrowed` and the legacy trait
  implementation while the production ResourceReplacer caller still uses it.
- `content_stream.rs::parse_content_stream_data_recovering_inline_image_eof`
  exists only for that now-obsolete ResourceReplacer caller.

The existing `resources_form_pruning_production_uses_the_handle_route` guard
stops before `collect_used_names_for_form`, so it does not cover this gap.

## Design

1. Change `collect_used_names_for_form` to instantiate `ResourceFinder` and
   call `parse_content_stream_handles(bytes, None, &mut finder)`. The decoded
   Form bytes have no document-owned references, so the context remains
   `None`; parser offsets and callback order are preserved.
2. Delete `ResourceCallbacks`, its inline-image header validator, and the
   unused inline-image `/CS` helper. qpdf's parser emits an inline-image handle
   but `ResourceFinder` does not treat inline-image header names as resource
   operators; qpdf's pruning scope mutates only `/Font` and `/XObject`.
   Malformed inline headers therefore remain ordinary parser input and do not
   create a flpdf-only pruning veto.
3. Change `ResourceReplacer` to use `parse_content_stream_handles` and the
   canonical ResourceFinder callback. Then migrate its direct unit/probe
   helper from the raw parser and delete `handle_object_borrowed` plus the
   legacy `ParserCallbacks` implementation. Do not alter the shared raw parser
   or its unrelated consumers in this child.
4. Delete the now-unused raw recovering helper from `content_stream.rs`; the
   canonical handle parser already implements qpdf's warning-only inline-image
   EOF recovery.
5. Leave the already documented `resolve_to_terminal` bare-reference
   compensation in `resources.rs` untouched. It is a separate qpdf-unowned
   bridge and is not part of this parser callback cutover.

No new adapter, sentinel, duplicate parser, or public API is introduced.

## Test design

- Add a source route-contract regression that inspects the production
   sections of `resources.rs`, `resource_replacer.rs`, `resource_finder.rs`,
   and `content_stream.rs`, and asserts the Form callback uses
  `ResourceFinder` and `parse_content_stream_handles` without the deleted
  `ResourceCallbacks`, inline-header validator, or legacy parser markers.
  Run it before the production change and record the expected RED failure.
- Keep the existing ResourceFinder operator/offset tests and migrate their
  parser helper to the handle parser. Delete only raw-borrow ownership tests
  whose sole behavior is the removed qpdf-incompatible route.
- Keep ResourceReplacer replacement-length, malformed-content, inline-image,
  and failure-path tests on the same canonical finder route.
- Keep a regression proving malformed inline-image headers do not veto the
  Form resource scan, matching the real qpdf pruning probe.
- Preserve and run Form resource-pruning tests for direct/indirect resources,
  nested Forms, shared-resource veto, malformed ordinary content, and
  incomplete `BI`/`ID` sequences. Inline-image header `/CS` names are not
  ResourceFinder resource references in qpdf and therefore are not recorded by
  the Form pre-pass.
- Run the live qpdf 11.9.0 ResourceFinder differential probe when its probe
  binary is available, plus the focused page/resource suite.

## Non-goals

- No changes to qpdf's page/resource pruning algorithm or its warning timing.
- No removal of `content_stream::ParserCallbacks` for other consumers; only
  the now-unused recovering helper is removed.
- No changes to `resolve_to_terminal`, page/job Auto policy, resource-replacer
  token filtering, or the public `Object` API.
- No closure of `flpdf-egzr.3.2.6`, `flpdf-egzr.3.2`, or
  `flpdf-25kg.3.1` from this child alone.

## Verification gates

The child is complete only after fresh evidence for focused tests, fmt, strict
private rustdoc, all-features clippy, workspace tests, qpdf module/deviation
checks, and parent-relative 100% patch coverage. The implementation branch
must be rebased onto the latest `origin/main` before its Draft PR is created;
all CI checks, including `codecov/patch`, must pass before the PR is marked
ready.

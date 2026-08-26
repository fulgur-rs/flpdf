# Form Resource-Pruning Parser Callback ObjectHandle Design

## Goal

Remove the production Form-XObject resource-pruning path's legacy
`Object`/`ParserCallbacks` boundary. The Form pre-pass will parse decoded
content through the existing `ObjectHandleParserCallbacks` route and retain
qpdf 11.9.0's resource-name, offset, inline-image, diagnostic, EOF, and
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

The live oracle is `/usr/bin/qpdf` 11.9.0. Existing ResourceFinder probe cases
cover the operator table, escaped names, malformed content, inline-image
events, and incomplete inline images; those cases remain the differential
acceptance authority.

## Current route inventory

The canonical route already exists:

- `content_stream.rs::ObjectHandleParserCallbacks` and
  `parse_content_stream_handles` emit live `ObjectHandle` values.
- `resource_finder.rs::handle_object_handle` and its
  `ObjectHandleParserCallbacks` implementation match qpdf's handle callback.
- The page pruning route in `resources.rs` already uses the canonical parser
  for the page and direct Form target.

The remaining mixed route is the Form pre-pass:

- `resources.rs::collect_used_names_for_form` calls the legacy
  `parse_content_stream_data`.
- `resources.rs::ResourceCallbacks` stores `Vec<Object>` and implements the
  legacy `ParserCallbacks` trait.
- `resource_finder.rs` retains `handle_object_borrowed` and the legacy trait
  implementation even though no production caller remains.

The existing `resources_form_pruning_production_uses_the_handle_route` guard
stops before `collect_used_names_for_form`, so it does not cover this gap.

## Design

1. Change `ResourceCallbacks` to implement only
   `ObjectHandleParserCallbacks`. Its inline-image header buffer becomes
   `Vec<ObjectHandle>`, and header keys/values use `ObjectHandle` accessors.
2. Change `collect_used_names_for_form` to call
   `parse_content_stream_handles(bytes, None, &mut callbacks)`. The decoded
   Form bytes have no document-owned references, so the context remains
   `None`; parser offsets and callback order are preserved.
3. Keep the existing `BI`/`ID` state machine and custom non-built-in inline
   `/CS` recording exactly at the callback boundary. Inline-image payload
   handles remain ignored, while malformed/incomplete headers mark the Form
   incomplete and leave its resource dictionaries unchanged.
4. Migrate ResourceFinder's direct unit/probe helper from the raw parser to
   the handle parser, then delete `handle_object_borrowed` and the legacy
   `ParserCallbacks` implementation. Do not alter the shared legacy parser or
   its unrelated consumers in this child.
5. Leave the already documented `resolve_to_terminal` bare-reference
   compensation in `resources.rs` untouched. It is a separate qpdf-unowned
   bridge and is not part of this parser callback cutover.

No new adapter, sentinel, duplicate parser, or public API is introduced.

## Test design

- Add a source route-contract regression that inspects the production prefix
  of `resources.rs` and asserts the Form callback uses
  `ObjectHandleParserCallbacks`, `parse_content_stream_handles`, and
  `Vec<ObjectHandle>` without legacy `Object`/`ParserCallbacks` markers.
  Run it before the production change and record the expected RED failure.
- Keep the existing ResourceFinder operator/offset tests and migrate their
  parser helper to the handle parser. Delete only raw-borrow ownership tests
  whose sole behavior is the removed qpdf-incompatible route.
- Preserve and run Form resource-pruning tests for direct/indirect resources,
  nested Forms, shared-resource veto, malformed content, inline-image built-in
  and non-built-in `/CS`, and incomplete `BI`/`ID` sequences.
- Run the live qpdf 11.9.0 ResourceFinder differential probe when its probe
  binary is available, plus the focused page/resource suite.

## Non-goals

- No changes to qpdf's page/resource pruning algorithm or its warning timing.
- No removal of `content_stream::ParserCallbacks` for other consumers.
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

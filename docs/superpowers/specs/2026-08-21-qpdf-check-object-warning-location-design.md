# qpdf Check Object-Warning Location Design

## Goal

Match qpdf 11.9.0 when `--check` emits an object-context warning: a warning
whose QPDFExc has an empty filename and zero offset must not receive the input
filename from the job completion emitter.

## Oracle and current gap

qpdf `QPDFObjectHandle::objectWarning` constructs
`QPDFExc(qpdf_e_object, "", description, 0, warning)`
(`libqpdf/QPDFObjectHandle.cc:2203-2212`). `QPDFExc::createWhat` only prefixes
the object description when filename is empty (`libqpdf/QPDFExc.cc:19-49`).

flpdf's ObjectHandle warning path already stores the description in the
diagnostic message without filename or offset. `job/check.rs::emit_diagnostics`
currently recognizes parenthesized parser contexts but treats a message such as
`page object 3 0: ...` as a normal file warning and prepends the input path.

## Architecture

Add a small job-check classification for qpdf object-description prefixes and
use it in both location and separator selection. Contextless object warnings
will render as `WARNING: page object 3 0: ...`; parser diagnostics with an
explicit file/offset context retain their existing formatting.

## Scope and non-goals

In scope:

- object-warning emission from the QPDFJob check route;
- exact CLI stderr regression against qpdf;
- preserving existing filename/offset warning shapes.

Out of scope:

- changing ObjectHandle warning construction, which already matches qpdf;
- changing parser/resolver offsets or unrelated check diagnostics;
- reopening the legacy check consumer moved by PR #978.

## Live evidence

For `tests/fixtures/compat/chained-indirect-contents.pdf`, qpdf emits
`WARNING: page object 3 0: object is supposed to be a stream or an array of
streams but is neither`, while current flpdf prepends the input filename.

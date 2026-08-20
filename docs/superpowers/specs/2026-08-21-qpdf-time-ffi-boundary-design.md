# qpdf_time FFI Boundary Design

## Goal

Keep the crate-level `#![deny(unsafe_code)]` effective while allowing only the
platform acquisition code required to mirror qpdf 11.9.0's local-time route.

## Boundary

The safe qpdf-time value, formatter, cache, and fallback remain in the private
qpdf_time module. Unix libc calls and the `tzset` declaration are isolated in a
`unix_platform` module with a narrowly scoped lint allowance. Windows SDK calls
are isolated similarly in `windows_platform`. The outer module and all call
sites remain subject to the crate deny boundary.

No observable behavior changes: the parent implementation's qpdf source/live
evidence remains authoritative (`libqpdf/QUtil.cc:868-934`), and the Unix and
Windows APIs stay identical.

## Non-goals

- no new dependencies;
- no change to PDF date formatting, cache lifetime, or timezone behavior;
- no broad crate or workspace unsafe-code allowance.

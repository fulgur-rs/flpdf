# Shared qpdf Trailer Owner Design

Status: approved for implementation on 2026-09-07.

## Goal

Translate qpdf 11.9.0's single `QPDFWriter::getTrimmedTrailer` /
`QPDFWriter::writeTrailer` responsibility into the Rust writer boundary. The
first production cutover is the plain classic-xref route; the same owner must
also express the linearized first/second trailer forms and xref-stream
cleartext rules so later route cutovers do not create another trailer builder.

## Oracle contract

The pinned qpdf source is authoritative:

- `libqpdf/QPDFWriter.cc:2009-2031` shallow-copies the live trailer and
  removes `/ID`, `/Encrypt`, `/Prev`, `/Index`, `/W`, `/Length`, `/Filter`,
  `/DecodeParms`, `/Type`, and `/XRefStm`.
- `libqpdf/QPDFWriter.cc:1160-1236` emits the trimmed keys in sorted order,
  substitutes `/Size`, inserts fixed-width `/Prev` only for `t_lin_first`,
  writes pass-specific `/ID`, omits `/Encrypt` for `t_lin_second`, and keeps
  trailer bytes clear when writing an xref stream.
- `libqpdf/QPDFWriter.cc:2335-2495` calls the same trailer owner from both
  classic xref and xref-stream writers.
- `libqpdf/QPDFWriter.cc:2537-3044` calls the same owner from both
  linearization halves and the normal writer.

## Design

`writer/object.rs` remains the canonical live-`ObjectHandle` emission owner.
Its trailer API gains an explicit qpdf-shaped form value (`normal`,
`lin_first`, or `lin_second`) plus the output `/Size`, optional `/Prev`, QDF
format flag, xref-stream flag, reference map, removed-reference set, and ID
writer. The form value owns only the trailer contract; it does not own xref
row layout, `/W`, `/Index`, hint offsets, or linearization region padding.

`build_writer_trailer_handle` remains the one trimmed-trailer preparation
boundary. Generated writer-owned values (`/Size`, `/Root`, `/ID`, and
`/Encrypt`) are supplied by the caller according to the selected route, while
the shared emitter controls key visibility and final `/ID` then `/Encrypt`
ordering. The implementation must preserve direct child handles and live
reference remapping rather than rebuilding a raw `Dictionary` snapshot.

The plain classic route is migrated first. Its xref rows and `startxref`
framing stay in `writer/plain/xref.rs`, but trailer bytes are produced by the
shared owner. Linearized and xref-stream callers keep their specialized
physical layout until their follow-up route slices; they must be expressible
through the same owner without adding a second semantic trailer builder.

## Invariants and error boundaries

- `lin_first` emits `/Prev` immediately after `/Size` with qpdf's fixed
  21-byte right padding; `lin_second` emits only `/Size` from ordinary trailer
  keys and never emits `/Encrypt`.
- `/ID` is emitted after ordinary keys and before `/Encrypt`; a supplied ID
  writer replaces only the ID value bytes.
- Null visibility and removed-reference filtering are applied to ordinary
  trailer children exactly once. Writer-owned `/ID` and `/Encrypt` are not
  removed because of source-side reference filtering.
- Indirect children remain indirect references; the emitter does not
  dereference them to decide their serialized shape.
- No new legacy bridge, sentinel, or qpdf-incompatible fallback is added.
  Invalid map/reference state propagates the existing `Error` boundary.

## Verification

RED/GREEN coverage must pin the shared emitter's normal, both linearized
forms, QDF, xref-stream opener behavior, null-visible keys, `/Size`, `/ID`,
`/Encrypt`, and `/Prev` padding. Existing qpdf differential fixtures must
cover plain classic output first, then the relevant linearized and xref-stream
callers. Required gates are fmt, strict private rustdoc, all-features clippy,
workspace tests, qpdf module/doc/deviation checks, and fresh patch coverage.


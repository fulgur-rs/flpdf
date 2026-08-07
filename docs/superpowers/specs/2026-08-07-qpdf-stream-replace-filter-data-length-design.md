# QPDF Stream replaceFilterData Length Boundary Design

## Goal

Port the zero/nonzero length branch of qpdf 11.9.0
`QPDF_Stream::replaceFilterData` into the existing `ObjectHandle` stream
mutation boundary. Replacing a stream with an empty buffer removes `/Length`;
replacing it with a non-empty buffer writes the exact byte length.

## Oracle and responsibility

Pinned qpdf 11.9.0 is authoritative:

- `libqpdf/QPDF_Stream.cc:640-649` installs a replacement buffer, clears the
  provider source, and delegates filter and length mutation to
  `replaceFilterData`.
- `libqpdf/QPDF_Stream.cc:668-684` leaves uninitialized filter/decode-parms
  values untouched, removes `/Length` for length zero, and writes an integer
  `/Length` otherwise.
- `libqpdf/QPDFObjectHandle.cc:1344-1362` delegates both buffer entry points to
  the stream object.
- `libqpdf/QPDF_Dictionary.cc:135-146` supplies the direct-null/removal
  dictionary mutation semantics already ported by `flpdf-25kg.3.20`.

The length decision belongs to the stream replacement primitive. Consumers
must not add empty-buffer exceptions, and the future provider implementation
must be able to call the same boundary with length zero.

## Chosen design

Add a private `ObjectHandle::replace_filter_data` helper that mirrors qpdf's
private `QPDF_Stream::replaceFilterData`. It obtains the live stream dictionary,
applies optional `/Filter` and `/DecodeParms` replacements, then removes or
sets `/Length` according to the supplied `usize` length.

`ObjectHandle::replace_stream_data` keeps its public signature and buffer
sharing contract. It installs the caller's `Rc<Vec<u8>>` without copying and
delegates all dictionary mutation to `replace_filter_data`. Non-stream handles
remain no-ops; changing that error surface belongs to `flpdf-3yn9.7`.

This keeps one qpdf-shaped boundary for both current buffer replacement and
future provider registration without implementing a provider slot now.

## Alternatives rejected

1. Branch only inside `replace_stream_data` and leave filter mutation inline.
   This fixes today's buffer case but leaves no reusable qpdf responsibility
   boundary for the provider follow-up.
2. Add empty-buffer branches to callers. This duplicates stream semantics in
   consumers and lets different routes disagree about `/Length`.
3. Represent unknown length as `/Length 0`. qpdf explicitly removes the key;
   retaining zero changes the observable stream dictionary.

## Testing

Follow RED to GREEN TDD in `object_handle.rs`:

- change the existing shared-empty-buffer test to require an absent `/Length`,
  proving payload identity remains shared while the dictionary changes;
- cover existing and missing `/Length` with empty buffers;
- cover exact non-empty length and repeated empty/non-empty replacement;
- cover a document-owned indirect stream dictionary mutation;
- retain existing optional filter/decode-parms and non-stream behavior tests.

Verification includes focused unit tests, formatting, workspace all-feature
clippy, the full workspace test suite, qpdf-compatible byte gates, and fresh
changed-line coverage against `main`.

## Scope boundaries

This change does not implement `StreamDataProvider`, provider storage or
execution, `QPDF::newStream`, a new non-stream error, writer migration,
Filespec migration, or a direct-stream API expansion.

# Linearized Hint Single-Shot Splice Design

## Status

Approved in-session on 2026-08-09.

Bead: `flpdf-26l3`.

## Goal

Replace flpdf's hint-stream length convergence loop with the qpdf 11.9.0
linearization writer's two-pass orchestration: omit the hint object during pass
1, derive all hint tables from pass-1 offsets and lengths, build the complete
hint-stream object once, and splice those exact bytes during pass 2.

## qpdf responsibility boundary

The pinned qpdf 11.9.0 source is authoritative:

- `QPDFWriter::writeLinearized` (`QPDFWriter.cc:2537-2904`) writes the file
  twice. At the reserved hint slot pass 1 records only the xref offset and
  writes no bytes (`:2793-2803`). After pass 1 it calls `writeHintStream`
  exactly once, then pass 2 writes the saved buffer unchanged (`:2860-2884`).
- `QPDFWriter::writeHintStream` (`QPDFWriter.cc:2287-2332`) generates the
  hint payload, applies the hint object's data key, computes the encrypted
  `/Length`, and makes the newline-before-`endstream` decision from the emitted
  bytes. The resulting buffer includes the complete indirect object framing.
- `QPDF::calculateHPageOffset`, `calculateHSharedObject`, and
  `calculateHOutline` (`QPDF_linearization.cc:1482-1645`) consume pass-1 xref
  and object-length state once. Stored offsets use qpdf's virtual coordinate
  convention; `adjusted_offset` (`:876-884`) restores the hint object's bytes
  when checking the final file.
- Xref offsets are adjusted by the hint length and xref-stream regions are
  fixed-width (`QPDFWriter.cc:2344-2505`, `:2730-2848`), so pass 2 is pass 1
  plus one deterministic hint-object splice.

## Implementation shape

`crates/flpdf/src/linearization/writer.rs` will own one
`LinearizedPassOutput` result containing pass bytes, xref offsets, section
offsets, and xref metadata. `do_write_pass` will accept an optional complete
hint-object byte buffer. `None` means qpdf pass 1 and leaves the reserved slot
empty; `Some` appends the already-encrypted/framed object without re-encoding
it.

`write_linearized_impl` will always execute exactly one pass-1 write. It will
compute page/shared/outline hint values directly from that pass's virtual xref
offsets and byte lengths, encode the hint payload once, and call the existing
qpdf-shaped hint emitter once to produce the complete buffer. It will then
execute exactly one final pass with that buffer. Deterministic-ID hashing and
the optional pass-1 debug file reuse the same pass-1 output. Classic xref and
non-encrypted ObjStm/xref-stream layouts retain their existing fixed padding;
encrypted classic output encrypts the saved hint object once. Encrypted plus
ObjStm remains rejected under `flpdf-j4ph`'s separate responsibility.

The convergence cap and its `did not converge` error path are removed. Missing
offsets, lengths, overflow, malformed plans, and fixed-region overflow remain
ordinary existing writer errors.

## Verification contract

- A RED unit regression fixes the qpdf pass-1 virtual-offset contract for the
  outline hint builder: pass-1 offsets are already adjusted coordinates and
  must not be subtracted by a guessed hint-object length.
- Existing encrypted random-IV hint-table consistency coverage remains and no
  longer exercises a convergence loop.
- Existing qpdf-zlib-compatible classic and ObjStm byte-parity suites remain
  the output oracle; `qpdf --check-linearization` and pass-1-file tests cover
  the two-pass layout.
- `cargo fmt --all -- --check`, all-feature clippy, workspace tests, qpdf
  module-doc checks, and changed-line coverage are required before handoff.

## Non-goals

- Do not implement encrypted + ObjStm linearization; that is owned by
  `flpdf-j4ph`.
- Do not add a compatibility bridge, increase an iteration cap, or preserve a
  local convergence mechanism that qpdf does not have.
- Do not change Beads status to closed until implementation and verification
  are complete.

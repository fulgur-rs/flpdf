# qpdf Live Parser Frame Stack Design

## Status

Approved in design review on 2026-08-06.

Bead: `flpdf-25kg.3.18`.

## Goal

Replace the recursive live file-object parser with qpdf 11.9.0's explicit
parser-frame stack so that qpdf's 500-container limit is independent of the
caller's native thread-stack size.

## Fixed qpdf 11.9.0 Facts

The pinned source installed by `scripts/fetch-qpdf-source.sh` is
authoritative.

- `QPDFParser` represents an open array or dictionary with a `StackFrame` in
  `std::vector<StackFrame> stack`; the frame holds the partially built array
  or dictionary, dictionary state, key, offset, and recovery state
  (`libqpdf/qpdf/QPDFParser.hh:33-48,75`).
- `parse` creates the first frame for `[` or `<<` and enters the iterative
  `parseRemainder` loop (`libqpdf/QPDFParser.cc:72-79`).
- Nested open tokens append a frame to that vector. Before doing so, qpdf
  rejects the 501st container when the existing stack has 500 frames,
  warning `ignoring excessively deeply nested data structure` and returning
  null (`libqpdf/QPDFParser.cc:288-301`).
- A matching close finalizes the child object, pops its frame, and adds the
  child to the parent frame (`libqpdf/QPDFParser.cc:220-232,243-277`).

qpdf therefore does not need recursive C++ calls to parse nested containers.

## Architecture

`LiveFileParser` retains its live tokenizer, handle resolver, diagnostics,
integer/reference lookahead, warning behavior, parsed-offset rules, and
no-context behavior. It replaces recursive `array`, `dictionary`, and
`parse_from_token` container calls with one loop and a `Vec<LiveFrame>`.

Each `LiveFrame` stores exactly the state that must survive nested parsing:

- array values, or dictionary values plus orphan values;
- the dictionary's pending key and opening-token offset; and
- the opening token offset used as the completed container's parsed offset.

When a scalar, null, or indirect-reference handle is complete, the parser
adds it to the top frame. A close token finalizes the top frame and feeds its
result into its parent; if it closes the last frame, that result is returned.
Malformed token recovery, duplicate-key diagnostics, synthetic dictionary
keys, `endobj` treatment, and all qpdf diagnostic offsets retain their current
observable contracts.

`MAX_PARSE_DEPTH` remains 500. It limits `frames.len()` before a new frame is
pushed, exactly as qpdf limits `stack.size()` before `emplace_back`.

## Testing

The existing 500-level test remains a small-stack test. It must pass in a
256 KiB unnamed thread on Linux, macOS, Windows, and ARM because parsing no
longer grows the native call stack with nesting. The 501-level recovery test
continues to assert qpdf's null result and exact diagnostic.

Focused tests also preserve ordinary file-object recovery, dictionary
recovery, nested indirect references, parsed offsets, and ObjectHandle's
no-context behavior. After the focused suite passes, run the flpdf library
suite and the relevant workspace/docs gates before publishing the fix.

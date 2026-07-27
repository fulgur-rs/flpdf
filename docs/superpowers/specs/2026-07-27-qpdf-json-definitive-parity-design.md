# qpdf JSON definitive parity design

**Issue:** `flpdf-qxba.6`
**Stack:** `flpdf-qxba.6.1`–`flpdf-qxba.6.4` / PRs #559–#562
**Date:** 2026-07-27
**Oracle:** qpdf 11.9.0 (`v11.9.0`) — `libqpdf/JSON.cc`,
`libqpdf/JSONHandler.cc`, and `libqpdf/Pl_StdioFile.cc`
**Oracle path:** Resolve with `scripts/fetch-qpdf-source.sh --print-path`.

## Status and relationship to the component design

The four JSON component layers are implemented and published as a stacked PR,
but definitive differential probes found behavior that the initial port's
Rust-specific borrowing strategy does not preserve. This document specifies
the follow-up required before the stack can claim full qpdf 11.9.0 parity.

This design supersedes the initial component design in these areas:

- `JsonHandler` callbacks use shared `Fn`, not `FnMut`;
- handler dispatch reads live configuration at qpdf's dispatch boundaries,
  rather than retaining a whole-handler snapshot;
- JSON container writers do not snapshot container contents before opening
  delimiters;
- blob Base64 finalization happens only after a successful callback;
- diagnostic messages preserve arbitrary bytes; and
- side-file output has qpdf stdio-compatible buffering and finish semantics.

The public callback API may break. flpdf is pre-v1.0, and preserving qpdf's
component responsibilities and observable behavior takes priority over
compatibility with the stack's unpublished callback API.

## Observed parity gaps

The design addresses these differential results:

1. If a sink replaces dictionary member `a` after `"a": ` is emitted, qpdf
   writes the replacement while flpdf writes the old value.
2. If a sink appends to an array after `[` is emitted, qpdf visits the appended
   element while flpdf writes its pre-open snapshot.
3. Re-entering the same blob callback or active `JsonHandler` callback succeeds
   in qpdf but triggers a `RefCell` mutable-borrow panic in flpdf.
4. If a blob callback writes one raw byte and then fails, qpdf leaves only the
   opening quote. flpdf's encoder Drop currently appends the Base64 tail.
5. A whole-handler snapshot strongly retains replaced child targets until
   dispatch ends. qpdf releases a replaced, not-currently-selected target
   immediately.
6. qpdf diagnostics preserve malformed high-bit input bytes. flpdf currently
   UTF-8-encodes or replaces some bytes while constructing parser, schema, and
   handler errors.
7. For a small file-mode stream written to `/dev/full`, qpdf completes the main
   JSON and exits successfully, while flpdf treats the final side-file flush as
   fatal.

The separately observed missing-`/Pages` behavior for
`--json-key=pages` predates this stack and remains a separate Bead. It is not
mixed into these four already-reviewed component layers.

## Oracle ownership

The behavior above follows directly from qpdf's component boundaries:

- `JSON.cc:100-118` opens a container before beginning its map/vector range
  iteration.
- `JSON.cc:85-90` writes a dictionary key before calling `write` through the
  referenced mapped value.
- `JSON.cc:183-190` opens a blob quote, invokes the producer, and calls
  `base64.finish()` only after the producer returns successfully.
- `JSONHandler.cc:7-38` stores callbacks and child handlers in shared members.
- `JSONHandler.cc:120-180` reads those members at each dispatch step rather than
  copying the complete handler graph.
- `JSON.cc:426-446` traverses the live dictionary and array containers.
- `Pl_StdioFile.cc:25-37` reports errors from writes that reach `fwrite`.
- `Pl_StdioFile.cc:41-45` ignores `fflush` failure unless `errno == EBADF`.

The Rust implementation must reproduce these public boundaries without using
unsafe code or holding a `RefCell` borrow across user-controlled code.

## `JsonHandler` shared-handle architecture

`JsonHandler` itself becomes a cloneable handle backed by
`Rc<RefCell<Handlers>>`. Callers register callbacks and child handlers through
`&self`; `handle` also takes `&self`.

Callbacks are stored as `Rc<dyn Fn(...)>` values. Registration methods may
accept generic `Fn + 'static` closures for ergonomics, but dispatch always
clones the exact callback it is about to invoke, drops the `Handlers` borrow,
and only then invokes the callback. Callers that need mutable captured state
use `Cell`, `RefCell`, or another explicit interior-mutability type.

Child dictionary, array, and fallback handlers are stored as cloned
`JsonHandler` handles. `SharedJsonHandler` is removed rather than retained as a
second public abstraction. All stack consumers migrate to `JsonHandler`.

The public handle supplies `downgrade`, and `WeakJsonHandler` supplies
`upgrade`, so callbacks can refer back to a handler without being forced to
create a strong ownership cycle. Capturing a strong clone remains allowed and
has the same deliberate cycle risk as a C++ `shared_ptr` graph.

`HandlerSnapshot`, `ActiveHandler`, `DispatchContext`, and the special
`handle_shared` entry point are removed.

### Live dispatch boundaries

`handle` performs short, independent reads at the same boundaries visible in
`JSONHandler.cc`:

1. clone and invoke the current any-value callback, if present;
2. clone and invoke the matching current scalar callback;
3. clone and invoke the current dictionary-start callback;
4. for every member, look up and clone only its current exact or fallback child
   handler immediately before recursive dispatch;
5. re-read and invoke the current dictionary-end callback;
6. clone and invoke the current array-start callback;
7. re-read and clone the current array-item handler for every element;
8. re-read and invoke the current array-end callback; and
9. finally, clone and dispatch through the current general fallback.

No callback or recursive `handle` call executes while `Handlers` is borrowed.
The same handler can therefore be synchronously re-entered. Configuration
changes made by earlier callbacks are visible to later dispatch steps, and a
replaced target that has not been selected is not retained by an unrelated
snapshot.

This remains a single-threaded `Rc` API. The change does not add `Send`,
`Sync`, threads, or unsafe code.

## Live JSON writer

No `Json` value remains borrowed while bytes are passed to a user-controlled
sink. Scalars clone only the bytes or primitive needed for their next output
operation. Containers retain their shared handle but obtain traversal state
after their opening delimiter has been written.

### Dictionaries

After writing `{`, the writer repeatedly selects the next live encoded key
strictly greater than the last emitted key. This is the safe Rust counterpart
of advancing through qpdf's ordered `std::map`:

- an insertion after the current cursor can be visited;
- an insertion before the cursor is not revisited;
- deletion of a future member prevents it from being visited; and
- lexical encoded-key order remains unchanged.

For each selected member, the writer keeps the selected value handle only as a
safe fallback, writes the key and `: `, and then looks up the key in the live
map again. If the mapped value was replaced during key output, the replacement
is written. Deleting the current map entry during key output has no defined
qpdf contract; flpdf safely writes the previously selected handle rather than
panicking.

### Arrays

The writer emits `[` before obtaining its element handles. It then clones the
element handles once and iterates that list. An append triggered by the opening
delimiter is therefore visible, matching qpdf's iterator-creation timing.

qpdf's only public array mutation is append. Appending after vector iteration
has begun can invalidate qpdf's iterator and has no supported oracle contract,
so flpdf does not attempt to emulate that undefined region.

### Blob Base64

The blob producer is stored as `Rc<dyn Fn(&mut dyn Write) ->
io::Result<()>>`. It is cloned and invoked with no `Json` borrow held.

A dedicated streaming Base64 adapter buffers at most two raw bytes:

- complete three-byte groups are encoded immediately;
- the final one- or two-byte tail and padding are emitted only by an explicit
  successful `finish`;
- `Drop` performs no output; and
- the closing quote is emitted only after `finish` succeeds.

Thus a producer that writes raw `x` and then returns an error leaves only the
opening quote. A producer that fails after complete three-byte groups retains
the already-emitted groups, matching qpdf's pipeline. Sink write errors
continue to propagate immediately.

## Byte-native diagnostics

qpdf uses byte-oriented `std::string` messages. Rust `String` cannot preserve
that contract for malformed parser input or non-UTF-8 keys and paths.

The component introduces `JsonMessage(Vec<u8>)` with:

- `as_bytes` and `into_bytes` for exact diagnostics;
- byte-preserving builders for ASCII prefixes, decimal offsets, paths, keys,
  and offending input;
- `Debug`; and
- a documented lossy `Display` implementation for human-facing fallback only.

`JsonError`, `JsonHandlerError`, and schema validation errors store
`JsonMessage` rather than forcing message content through `String`. The CLI
writes exact message bytes to stderr with `Write::write_all`; it does not pass
these messages through `eprintln!` or `String::from_utf8_lossy`.

The error types continue to implement `std::error::Error`. Exact parity tests
use the byte API, never `Display`.

## qpdf-compatible side-file sink

Rust `File` is unbuffered, whereas qpdf writes side files through a buffered C
`FILE*`. The side-file boundary gets a dedicated buffered writer; general Rust
writers retain normal `Write` semantics.

The compatibility writer uses the pinned Linux oracle's 4 KiB effective stdio
buffer contract. This is not inferred from the `BUFSIZ` macro: a direct glibc
2.39 probe against `/dev/full` shows that 4,095 bytes remain buffered until
`fflush`, while a 4,096-byte `fwrite` reaches the device and fails immediately.
The `/dev/full` file reports a 4,096-byte preferred block size, which is the
effective allocation selected by glibc for this stream.

Within that measured contract:

- ordinary writes accumulate until the buffer must drain;
- an error while draining during `write` is propagated as a side-file write
  failure;
- successful completion attempts one final drain/flush;
- a final `EBADF` remains fatal, as in `Pl_StdioFile::finish`;
- other final flush errors, including `ENOSPC`, are ignored; and
- Drop performs no second flush.

This makes a small `/dev/full` payload appear successfully written to qpdf's
pipeline and preserves completion of the main JSON. A payload that forces a
buffer drain during ordinary writing still fails. File creation errors and
ordinary write errors are never hidden.

The buffer size is an oracle property, not a generic public default. Tests lock
behavior below, at, and above the boundary so an environment change cannot
silently alter parity.

## Stacked delivery and tests

Every PR must independently retain 100% patch coverage against its direct
parent.

### PR #559 — core value and writer

- dictionary replacement after key output;
- live dictionary insertion after `{`;
- array append after `[`;
- same blob callback re-entry;
- partial blob output when the producer fails; and
- exact output bytes against qpdf 11.9.0 probes.

### PR #560 — parser

- malformed `0x80` and `0xff` input;
- exact message text, offset, and original bytes; and
- absence of UTF-8 expansion and replacement characters.

### PR #561 — schema and handler

- synchronous re-entry into the same active handler;
- live handler reconfiguration during dispatch;
- immediate Drop of a replaced, not-selected child target;
- non-UTF-8 schema key and handler path diagnostics; and
- migration of all stack consumers from `FnMut`/`SharedJsonHandler`.

### PR #562 — CLI integration and side files

- deterministic fake-sink cases below, at, and above 4 KiB;
- ordinary write failure, final `ENOSPC`, and final `EBADF`;
- Linux `/dev/full` comparison with the qpdf 11.9.0 executable;
- complete main JSON, exit status, stdout, and stderr exact bytes; and
- preservation of normal-file side-file data.

Each layer starts with a failing regression derived from the qpdf probe, then
implements the smallest responsibility-preserving change. Before submission,
each branch runs formatting, its focused JSON suites, crate tests, workspace
tests, workspace clippy with warnings denied, strict private-item rustdoc, the
qpdf module-doc check, and changed-line coverage against its own parent.

## Rejected alternatives

### Patch the existing snapshots

Adding exceptions to `HandlerSnapshot` and pre-open container snapshots would
retain the ownership model that caused the gaps. Each new callback timing case
would require another exception and would continue retaining unrelated
targets.

### Queue `FnMut` re-entry

Deferring a re-entrant call until the outer callback returns changes event
order and which live handler configuration is observed. qpdf re-entry is
synchronous.

### Keep both handler APIs

Retaining `SharedJsonHandler` beside a cloneable `JsonHandler` creates two
ownership models for one qpdf component and makes tests pass through different
dispatch paths. The stack is pre-v1.0 and should cut over once.

### Finalize Base64 from Drop

Drop-based finalization cannot distinguish normal completion from unwinding or
an explicit producer error. qpdf calls `finish` only on the successful path.

### Treat every side-file flush error as fatal

That is conventional Rust I/O behavior but contradicts
`Pl_StdioFile::finish` and the observed `/dev/full` result. The relaxed finish
rule remains isolated to the qpdf side-file adapter.

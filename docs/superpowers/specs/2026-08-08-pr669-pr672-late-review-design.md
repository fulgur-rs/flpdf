# PR #669 / #672 late review design

## Scope

This review pass handles two active inline comments without broadening the
QPDFLogger contract beyond qpdf 11.9.0.

- PR #669 `discussion_r3739060513`: classify and reply only. Do not add a
  transactional concurrency guarantee around stdout reservation.
- PR #672 `discussion_r3739068261`: remove the extra colon between a non-empty
  input description and an object-scoped warning message.
- Do not resolve either thread, merge either PR, or modify unrelated warning,
  logger, cache, or CLI behavior.

## Oracle decisions

### PR #669: oracle mismatch

`QPDFLogger.hh:45-83` describes stdout-save collision avoidance as a
best-effort safeguard and explicitly says it is not a guarantee.
`QPDFLogger.cc:80-165` returns shared pipeline handles and does not invalidate
handles obtained before `saveToStandardOutput`. Holding flpdf's logger-state
mutex across `info()` would therefore add a stronger contract while leaving
the public `get_info()` stale-handle path unchanged. The Rust `Send` and `Sync`
auto traits provide memory safety, not atomicity across separate logger calls.

Action: no semantic change. Reply with the qpdf responsibility boundary and
the unchanged verification result.

### PR #672: oracle match

`QPDFExc.cc:25-49` formats a non-empty filename followed by object/offset
context as `filename (object ..., offset ...): message`, with a space before
the opening parenthesis. `QPDF.cc:488-493` prepends only `WARNING: ` before
writing the formatted exception.

The live qpdf 11.9.0 probe

```text
qpdf --show-object=5 tests/fixtures/compat/chained-indirect-contents.pdf
```

exits 3 and writes:

```text
WARNING: tests/fixtures/compat/chained-indirect-contents.pdf (object 5 0, offset 232): expected endobj
```

flpdf currently inserts `: ` after the description unconditionally, producing
`...pdf: (object ...)`.

Action: when the already-formatted diagnostic message starts with `(`, join a
non-empty description and the message with one space. Preserve the existing
empty-description and explicit-offset cases.

## Implementation and tests

Follow RED to GREEN:

1. Change the existing resolver formatter unit expectation and the lazy
   warning integration expectations to the qpdf byte form; run them and record
   the expected RED caused by the extra colon.
2. Make the smallest change in `route_warning` to select `" "` only for an
   object-prefixed message with a non-empty description. Keep all other
   separators unchanged.
3. Re-run the focused formatter and `pdf_logger_tests`, then workspace fmt,
   denied-warning clippy, workspace tests, module-doc checks, and fresh
   changed-line coverage against PR #672's base branch.
4. Push only to PR #672 after all local gates pass and wait for its CI. Reply
   once in each original thread with classification and evidence. Read both
   threads back and leave them unresolved.

## Tracking and completion

Bead `flpdf-25kg.5.6` owns this review pass and blocks the P4 completion gate
while open. Close and push it only after the PR head, CI, inline replies, and
thread states have been read back successfully.

# flpdf threat model

- Status: living document
- Last reviewed: 2026-06-11 (initial version, audited against the source at
  that date)

flpdf is a pure-Rust PDF read/write library (`crates/flpdf`) with a
qpdf-compatible CLI (`crates/flpdf-cli`). This document defines what flpdf
promises when handed hostile input, what it explicitly does **not** promise,
and how those promises are verified.

The structure follows the de-facto threat model of
[qpdf](https://github.com/qpdf/qpdf), which is not collected in a single
document but is spelled out in its fuzzer harness ("you should be able to
throw anything at libqpdf and it will respond without any memory errors and
never do anything worse than throwing a QPDFExc"), its `fuzz_mode`
documentation (memory/time limits are deliberately *not* imposed in normal
operation), and its CVE history (memory errors, infinite loops, and
stack overflow from unbounded recursion were all treated as security bugs).
flpdf adopts the same posture, translated to Rust.

See [SECURITY.md](../SECURITY.md) for how to report a vulnerability.

## 1. Scope and trust boundary

**Input PDFs are untrusted.** Every byte of an input document is treated as
attacker-controlled: the header, xref tables and streams, object syntax,
stream data, filter chains, encryption dictionaries, signature dictionaries,
and everything reached through the repair paths (`Pdf::open_with_repair`,
`Pdf::open_best_effort`). The repair paths exist precisely to accept damaged
input, so they widen — not narrow — the attack surface and are held to the
same guarantees.

**The caller is trusted.** How the API is used (which operations are invoked,
where output is written), the passwords supplied for decryption or
encryption, and build configuration are the caller's responsibility. For the
CLI, command-line arguments are trusted; the files those arguments point to
are not.

Threats that involve tricking a *human* (e.g. a PDF that renders misleading
content) are out of scope: flpdf transforms and inspects documents, it does
not render them.

## 2. Core guarantees

For **any** input byte sequence, flpdf aims to guarantee:

- **(a) No undefined behavior.** The `flpdf` crate contains no `unsafe` code;
  memory safety follows from Rust plus vetted dependencies. (Mechanical
  enforcement via `#![forbid(unsafe_code)]` is planned — see §8.)
- **(b) No panic, no abort.** Malformed input is reported through the typed
  [`Error`](../crates/flpdf/src/error.rs) enum, never via `panic!`,
  `unwrap()` on attacker-reachable values, out-of-bounds indexing, or
  arithmetic overflow. This is the Rust translation of qpdf's "never anything
  worse than a QPDFExc" rule. Stack exhaustion from unbounded recursion
  counts as an abort and therefore violates this guarantee.
- **(c) Bounded traversal.** Every walk over the object graph terminates,
  including on documents with reference cycles, self-referential streams, or
  pathologically deep trees. Termination is enforced by visited sets and/or
  explicit depth limits (§5).
- **(d) Honest diagnostics.** `check`/repair report what was wrong with the
  document; recovery never silently fabricates success on input it could not
  actually interpret.

These guarantees describe the contract we hold ourselves to. Known
deviations currently exist; they are tracked in §8 rather than silently
excluded from the contract.

## 3. What we consider a vulnerability

A report is treated as a security bug if untrusted input bytes can cause any
of the following:

- **Undefined behavior or memory unsafety** (only possible via dependencies
  or future `unsafe` code, but in scope regardless).
- **A panic or abort** from a *logic* fault — `panic!`, `unwrap()` on
  attacker-reachable values, out-of-bounds indexing, arithmetic overflow, or
  stack overflow from unbounded recursion. qpdf precedent: CVE-2018-9918
  (stack overflow on deeply nested direct objects, fixed with a nesting depth
  limit). Resource-exhaustion aborts (e.g. an allocation failure on a
  compression bomb) are out of scope per §4, not security bugs.
- **Non-termination** — an infinite loop while parsing or traversing.
  qpdf precedent: CVE-2017-9209 / CVE-2017-9210.
- **Silent removal of signature evidence *beyond what qpdf does***
  ([signed-pdf.md](signed-pdf.md)): flpdf-specific logic that strips or nulls a
  signature object qpdf would keep (e.g. the destination null-out that nulled an
  arbitrary referenced object, `flpdf-hn1g.11`), without the documented
  `--remove-restrictions` opt-in. Scoped against qpdf: behaviours qpdf also
  performs are **not** violations pre-v1.0 — *invalidating* a signature by a
  full rewrite (qpdf proceeds, leaving it present-but-invalid), and
  *garbage-collecting* a signature field that a `--pages` selection orphaned by
  dropping its page (qpdf drops it too, with no warning), both match qpdf. A
  preserve-by-default refusal/warning is a deferred post-v1.0 improvement,
  `flpdf-hn1g.14`.
- **Accepting a wrong password as valid** (authentication bypass in the
  standard security handler).

## 4. What we do not guarantee

Matching qpdf's posture, the following are explicitly **not** promised and
are not treated as vulnerabilities on their own:

- **Bounded memory or processing time.** Legitimate PDFs can be very large
  and complex, so flpdf imposes no global memory or time limits in normal
  operation. In particular, today:
  - stream decoding (`FlateDecode`, `LZWDecode`, …) places no cap on output
    size **by default**, so a compression bomb can exhaust memory;
  - some operations read the whole file or whole streams into memory.
  Callers that process untrusted input should run flpdf under external
  resource limits (container memory limits, `ulimit`/rlimits, timeouts).
  The ordinary decode APIs cap `/Filter` chains at 16 stages by default through
  `DecodeLimits::default()`. Callers must explicitly set
  `DecodeLimits::max_filter_chain` to `None` to opt out; the qpdf compatibility
  test driver does so to reproduce qpdf's uncapped `qpdf_dl_all` behavior.
  An opt-in decode-output limit comparable to qpdf's
  `Pl_Flate::setMemoryLimit` is available via `filters::DecodeLimits` /
  `filters::decode_stream_data_with_limits` (default unbounded; embedders set
  `max_output` to bound each `FlateDecode` / `LZWDecode` stage). qpdf's CLI has
  no corresponding flag, so `flpdf-cli` does not expose this cap either; a
  content stream that exceeds a caller-supplied limit is reported as a
  warning, not as corruption. The cap is only reachable by calling
  `filters::decode_stream_data_with_limits` (or its `_recovering` variant)
  directly with a caller-supplied `DecodeLimits`; the ordinary document
  paths (`Pdf::resolve`, the page/resource/attachment helpers, `PdfWriter`,
  and `job::QPDFJob::check` / the `--check` route) all hard-code
  `DecodeLimits::default()` (`max_output: None`) internally and expose no
  way to set the cap, so they decode unbounded regardless of this opt-in —
  matching qpdf's own uncapped `qpdf_dl_all` traversal for the check pass.
  flpdf's other document paths still place no output cap by default.
- **PDF permission enforcement.** Owner-password usage restrictions
  (printing, copying, …) are advisory metadata under the PDF specification.
  flpdf, like qpdf, can remove them (`--remove-restrictions`); this is a
  feature, not a bypass.
- **Strength of legacy PDF cryptography.** Reading RC4- and MD5-based
  encryption (V=1/2/4, R=2/3/4) is required for compatibility and is not a
  vulnerability. *Selecting* RC4-weak encryption parameters via `--encrypt`,
  and selecting deprecated R=5 (AES-256) parameters, is gated behind an
  explicit `--allow-weak-crypto` opt-in; a rewrite that preserves an input's
  existing weak encryption is not a new selection and is not gated, matching
  qpdf's write-only weak-crypto policy. Existing weakly encrypted inputs
  remain readable on every open path. AES-CBC without integrity protection,
  MD5 in key derivation, etc. are properties of the PDF standard security
  handler, not of flpdf.
- **Bugs inside dependencies** (`flate2`, RustCrypto crates, …) that flpdf
  does not reach or amplify with attacker-controlled input. These should be
  reported upstream; flpdf's responsibility is to update promptly. A
  dependency bug that a malformed PDF *can* drive through flpdf — e.g. memory
  unsafety in a decoder — stays in scope per §3, not excluded here.
- **Side channels.** Timing or memory-access side channels in password
  checking and decryption are out of scope.

## 5. Built-in defenses

Inventory of the mechanisms that uphold §2, as of the last review:

| Mechanism | Where |
| --- | --- |
| Depth limits (= 100) on recursive tree walks: page tree, outlines, name/number trees, embedded files, AcroForm fields, structure tree, signature fields | `DEFAULT_MAX_*_DEPTH` constants in `pages.rs`, `outline_document_helper.rs`, `nntree.rs`, `embedded_files.rs`, `acroform_field_prune.rs`, `struct_tree_pg.rs`, `signatures.rs` |
| Depth limits (= 64) on destination-reference and action chains | `MAX_DEST_RESOLVE_DEPTH` (`outline_dest_remap.rs`), `MAX_ACTION_CHAIN_DEPTH` (`page_extract.rs`) |
| Cycle detection (visited sets) on iterative chain following: xref `/Prev` chains, outline `/Next` chains, field `/Parent` chains (an iterative `while`-loop with a visited set — terminating; the missing depth cap is hardening only, `flpdf-hn1g.3`) | `xref.rs` (`merge_previous_xref_sections`), `outline_document_helper.rs` (`build_item`), `annotation_helper.rs`, `signatures.rs`, `json_inspect.rs` |
| Checked arithmetic and non-negative validation on parser-derived sizes (`/Length` bounds, PNG-predictor row math, LZW table size cap of 4096 entries) | `parser.rs`, `filters.rs` |
| Reference resolution that cannot loop (cache-based; unresolvable references resolve to null) | `reader.rs` (`resolve`, `resolve_borrowed`) |
| Weak-crypto write gate: selecting new RC4 or deprecated R=5 (AES-256) encryption parameters via `--encrypt` requires the explicit `--allow-weak-crypto` opt-in (a preserve-only rewrite of an already weakly encrypted input is not gated) | `parse_encrypt_segment`'s `guard_weak` (`main.rs`) refuses the new selection; encrypted inputs remain readable, matching qpdf's write-only weak-crypto policy |
| OS CSPRNG for AES IVs and key material | `getrandom` in `security/` |
| Signed-PDF qpdf-compatible handling (full rewrite proceeds, leaving signatures present-but-invalid like qpdf; signatures are stripped only via the explicit `--remove-restrictions` opt-in, never silently). A preserve-by-default *refusal* is a deferred post-v1.0 improvement (`flpdf-hn1g.14`). | [signed-pdf.md](signed-pdf.md), `signatures.rs` |
| Traversal boundaries on page closures: stop at other `Page`/`Catalog` dicts and skip `/Kids` on `/Pages` nodes; `/Parent` is intentionally followed upward for inherited resources, bounded by the `Page`/`Catalog` stop; no brute-force scans of all live objects | `page_closure.rs`, [.claude/rules/pdf-rust-review-patterns.md](../.claude/rules/pdf-rust-review-patterns.md) |

## 6. Verification

Current:

- Unit and integration tests across both crates. Contributors are required
  to run a 100% changed-line coverage gate on `crates/flpdf` before opening a
  PR (`scripts/patch-coverage.sh`); this is a local contribution-process
  gate, not yet CI-enforced. CI separately runs whole-workspace
  `cargo llvm-cov` and uploads the report to Codecov.
- Code review against the recurring-pitfall rules (unresolved indirect
  references, unsigned casts, unbounded graph traversal) in
  [.claude/rules/pdf-rust-review-patterns.md](../.claude/rules/pdf-rust-review-patterns.md).

Planned (§8): a `cargo-fuzz` harness covering the full
open → check → write pipeline, mirroring qpdf's `qpdf_fuzzer`, with fuzz
findings fixed and pinned as regression tests. qpdf runs 15 fuzz targets
(whole-pipeline, per-feature, per-codec) continuously on OSS-Fuzz; that is
the long-term bar.

## 7. Reporting a vulnerability

See [SECURITY.md](../SECURITY.md). Issues that fall under §3 are treated as
security bugs and prioritized accordingly; issues that fall under §4 are
ordinary bugs or feature requests.

## 8. Known gaps

Honest list of places where the implementation does not yet meet §2, found
by the 2026-06-11 audit. IDs refer to the in-repo beads tracker
(`bd show <id>`).

| Gap | Guarantee affected | Tracking |
| --- | --- | --- |
| Object parser recursion (`Parser::object` → `dictionary`/`array`) has no depth limit; deeply nested input (`<</A <</B …>>>>`, `[[[…]]]`) can overflow the stack and abort. Same shape as qpdf CVE-2018-9918. | (b) no panic/abort | `flpdf-hn1g.1` |
| Structural ref-walkers recurse over direct array/dictionary/stream-dictionary structure with no depth cap (e.g. `page_closure::collect_refs_in_object`, `subset_prune::walk_refs`), unlike `rewrite_renumber`'s `MAX_INLINE_DEPTH`-bounded `collect_refs`; a resolved object with deeply nested direct structure can overflow the stack during page-closure copy or `--pages`/attachment GC. Currently shadowed by the parser gap (`flpdf-hn1g.1`) but independent uncapped paths; the fix is a single shared bounded walk. | (b) no panic/abort | `flpdf-hn1g.9` |
| No fuzz harness exists; guarantees (b)/(c) are asserted but not continuously exercised. | verification | `flpdf-hn1g.2` |
| `inherited_field_value` `/Parent` walks in `signatures.rs` and `json_inspect.rs` rely on visited sets only (terminating, but no depth cap unlike their `annotation_helper.rs` counterpart). | (c) bounded traversal | `flpdf-hn1g.3` |
| Decode-side resource-exhaustion mitigations are now in place (was: no opt-in decode-output limit and no `/Filter` chain length cap). The ordinary decode APIs cap `/Filter` chains at 16 stages by default through `DecodeLimits::default()`. Callers must explicitly set `DecodeLimits::max_filter_chain` to `None` to opt out; the qpdf compatibility test driver does so to reproduce qpdf's uncapped `qpdf_dl_all` behavior. An opt-in output limit is provided via `filters::DecodeLimits` / `decode_stream_data_with_limits` (default unbounded). Compression bombs remain out of scope by default per §4. | §4 mitigation (delivered) | `flpdf-hn1g.4` |
| `#![forbid(unsafe_code)]` not yet declared (no `unsafe` exists in `crates/flpdf/src/`; the attribute would make that mechanical). | (a) enforcement | `flpdf-hn1g.6` |

## Appendix A: attack surface inventory

Entry points through which untrusted bytes reach flpdf:

| Surface | Entry points |
| --- | --- |
| Document opening (strict) | `Pdf::open`, `Pdf::open_mem` |
| Document opening (repair — widest surface) | `Pdf::open_with_repair`, `Pdf::open_best_effort`, `Pdf::open_with_options` and `open_mem*` variants |
| Lazy object loading | `Pdf::resolve` / `resolve_borrowed` (xref offsets, object syntax, object streams) |
| Stream decoding | filter pipeline in `filters.rs`: Flate, LZW, ASCII85, ASCIIHex, RunLength (+ pass-through DCT/JBIG2/JPX/CCITT) |
| Decryption | standard security handler (`security/`): RC4-40/128, AES-128 (V4/R4), AES-256 (V5/R5 deprecated, V5/R6); qpdf-compatible UTF-8 validation/raw password bytes and the reader-side V5 prefix boundary (qpdf does not apply SASLprep) |
| Validation | `job::QPDFJob::check` and the CLI `--check` route |
| Writing (reads everything it writes) | `PdfWriter`, QDF repair, linearization |
| Signature inspection | `signatures.rs` (`/ByteRange`, signature dictionaries, certificates) |
| CLI (drives all of the above on argv-named files) | `flpdf-cli`: `check`, `rewrite`, `qdf`, `qdf-fix`, `linearize`, `dump-object`, `show-stream`, `pages`/`--pages`, `--split-pages`, attachment options, encryption options, JSON output |
| Cross-document operations (two untrusted documents interacting) | `--pages` merging, `--copy-attachments-from`, `--copy-encryption` |

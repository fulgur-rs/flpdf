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
- **A panic or abort**, including stack overflow from unbounded recursion.
  qpdf precedent: CVE-2018-9918 (stack overflow on deeply nested direct
  objects, fixed with a nesting depth limit).
- **Non-termination** — an infinite loop while parsing or traversing.
  qpdf precedent: CVE-2017-9209 / CVE-2017-9210.
- **A violation of the signed-PDF integrity policy**
  ([signed-pdf.md](signed-pdf.md)): an edit that silently invalidates or
  strips signature protection without the documented opt-in.
- **Accepting a wrong password as valid** (authentication bypass in the
  standard security handler).

## 4. What we do not guarantee

Matching qpdf's posture, the following are explicitly **not** promised and
are not treated as vulnerabilities on their own:

- **Bounded memory or processing time.** Legitimate PDFs can be very large
  and complex, so flpdf imposes no global memory or time limits in normal
  operation. In particular, today:
  - stream decoding (`FlateDecode`, `LZWDecode`, …) places no cap on output
    size, so a compression bomb can exhaust memory;
  - `/Filter` chains have no length limit, allowing multiplicative
    expansion across stages;
  - some operations read the whole file or whole streams into memory.
  Callers that process untrusted input should run flpdf under external
  resource limits (container memory limits, `ulimit`/rlimits, timeouts).
  Opt-in decode limits comparable to qpdf's `Pl_Flate::memory_limit` are
  planned (§8).
- **PDF permission enforcement.** Owner-password usage restrictions
  (printing, copying, …) are advisory metadata under the PDF specification.
  flpdf, like qpdf, can remove them (`--remove-restrictions`); this is a
  feature, not a bypass.
- **Strength of legacy PDF cryptography.** Reading RC4- and MD5-based
  encryption (V=1/2/4, R=2/3/4) is required for compatibility and is not a
  vulnerability. *Creating* RC4-weak output is gated behind an explicit
  `--allow-weak-crypto` opt-in; that gate is not yet enforced on the CLI
  R=5 (AES-256) *write* path, although the read path already requires the
  opt-in for R=5 (tracked as `flpdf-hn1g.8`). AES-CBC without integrity
  protection, MD5 in key derivation, etc. are properties of the PDF standard
  security handler, not of flpdf.
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
| Depth limits (= 100) on recursive tree walks: page tree, outlines, name/number trees, fonts, embedded files, AcroForm fields, structure tree, signature fields | `DEFAULT_MAX_*_DEPTH` constants in `pages.rs`, `outline.rs`, `name_number_tree.rs`, `fonts.rs`, `embedded_files.rs`, `acroform_field_prune.rs`, `struct_tree_pg.rs`, `signatures.rs` |
| Depth limits (= 64) on destination-reference and action chains | `MAX_DEST_RESOLVE_DEPTH` (`outline_dest_remap.rs`), `MAX_ACTION_CHAIN_DEPTH` (`page_extract.rs`) |
| Cycle detection (visited sets) on iterative chain following: xref `/Prev` chains, object-stream `/Extends` chains, outline `/Next` chains, field `/Parent` chains | `xref.rs` (`merge_previous_xref_sections`), `reader.rs` (`collect_object_stream_chain`), `outline.rs` (`walk_outline`), `annotation_helper.rs`, `signatures.rs`, `json_inspect.rs` |
| Checked arithmetic and non-negative validation on parser-derived sizes (`/Length` bounds, PNG-predictor row math, LZW table size cap of 4096 entries) | `parser.rs`, `filters.rs` |
| Reference resolution that cannot loop (cache-based; unresolvable references resolve to null) | `reader.rs` (`resolve`, `resolve_borrowed`) |
| Weak-crypto write gate (weakly encrypted output requires explicit opt-in) | `Error::Encrypted(WeakCryptoNotAllowed)`, CLI `--allow-weak-crypto` |
| OS CSPRNG for AES IVs and key material | `getrandom` in `security/` |
| Signed-PDF preserve-by-default policy (edits that would invalidate signatures are rejected unless explicitly opted in) | [signed-pdf.md](signed-pdf.md), `signatures.rs` |
| Traversal boundaries per review rules (stop at other `Page`/`Catalog` dicts, skip `/Parent` during closure walks, no brute-force scans of all live objects) | [.claude/rules/pdf-rust-review-patterns.md](../.claude/rules/pdf-rust-review-patterns.md) |

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
| Object-stream `/Extends` chains are followed by recursive `collect_object_stream_chain` (`reader.rs`) guarded by a visited set (cycle detection) but no depth cap; a deep *acyclic* `/Extends` chain can overflow the stack and abort before a cycle is ever detected. Same class as the parser-recursion gap above. | (b) no panic/abort | `flpdf-hn1g.7` |
| No fuzz harness exists; guarantees (b)/(c) are asserted but not continuously exercised. | verification | `flpdf-hn1g.2` |
| `inherited_field_value` `/Parent` walks in `signatures.rs` and `json_inspect.rs` rely on visited sets only (terminating, but no depth cap unlike their `annotation_helper.rs` counterpart). | (c) bounded traversal | `flpdf-hn1g.3` |
| No opt-in decode-output limits and no `/Filter` chain length cap (compression bombs covered by §4, but mitigations are worth offering). | §4 mitigation | `flpdf-hn1g.4` |
| `#![forbid(unsafe_code)]` not yet declared (no `unsafe` exists in `crates/flpdf/src/`; the attribute would make that mechanical). | (a) enforcement | `flpdf-hn1g.6` |

## Appendix A: attack surface inventory

Entry points through which untrusted bytes reach flpdf:

| Surface | Entry points |
| --- | --- |
| Document opening (strict) | `Pdf::open`, `Pdf::open_mem` |
| Document opening (repair — widest surface) | `Pdf::open_with_repair`, `Pdf::open_best_effort`, `Pdf::open_with_options` and `open_mem*` variants |
| Lazy object loading | `Pdf::resolve` / `resolve_borrowed` (xref offsets, object syntax, object streams) |
| Stream decoding | filter pipeline in `filters.rs`: Flate, LZW, ASCII85, ASCIIHex, RunLength (+ pass-through DCT/JBIG2/JPX/CCITT) |
| Decryption | standard security handler (`security/`): RC4-40/128, AES-128 (V4/R4), AES-256 (V5/R5 deprecated, V5/R6); password normalization incl. SASLprep |
| Validation | `check_reader`, `check_reader_strict`, `check_reader_with_options` |
| Writing (reads everything it writes) | `write_pdf`, `write_qdf`, linearization |
| Signature inspection | `signatures.rs` (`/ByteRange`, signature dictionaries, certificates) |
| CLI (drives all of the above on argv-named files) | `flpdf-cli`: `check`, `rewrite`, `qdf`, `qdf-fix`, `linearize`, `dump-object`, `show-stream`, `pages`/`--pages`, `--split-pages`, attachment options, encryption options, JSON output |
| Cross-document operations (two untrusted documents interacting) | `--pages` merging, `--copy-attachments-from`, `--copy-encryption-from` |

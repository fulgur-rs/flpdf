# flpdf Threat Model — Design

- Date: 2026-06-11
- Issue: flpdf-pcor
- Status: accepted

## Problem

flpdf parses attacker-controlled PDFs (including via repair paths) but had no
documented security posture: no SECURITY.md, no threat model, no fuzzing, and
no stated line between "vulnerability" and "expected behavior on hostile
input". Contributors and users had no contract to hold the code to.

## Reference: qpdf's de-facto threat model

qpdf publishes no SECURITY.md or threat-model document. Its posture is
distributed across:

- `fuzz/qpdf_fuzzer.cc`: "you should be able to throw anything at libqpdf and
  it will respond without any memory errors and never do anything worse than
  throwing a QPDFExc or std::runtime_error."
- `include/qpdf/global.hh` (`fuzz_mode`): memory/time limits are deliberately
  **not** imposed in normal operation because legitimate PDFs can be huge;
  limits exist only while fuzzing.
- CVE history: memory errors (CVE-2017-11624..27), infinite loops
  (CVE-2017-9209/9210), and stack overflow from unbounded nesting
  (CVE-2018-9918, fixed with a ~500 depth limit) were all treated as
  security bugs.
- OSS-Fuzz: 15 targets (whole-pipeline, per-feature, per-codec), ASAN/UBSAN.

## Decisions

1. **Scope**: threat-model document + gap analysis, with gaps filed as beads
   issues (epic `flpdf-hn1g`). No code fixes in this change.
2. **Methodology**: guarantee-based (qpdf style), not STRIDE. Sections:
   trust boundary → core guarantees → vulnerability criteria → explicit
   non-guarantees → built-in defenses → verification → reporting → known
   gaps → attack-surface appendix.
3. **Placement**: `docs/threat-model.md` (body) + thin root `SECURITY.md`
   (reporting procedure, GitHub private vulnerability reporting). Both in
   English, matching existing `docs/` convention.
4. **Honesty rule**: core guarantees state the contract; current deviations
   are listed in a "Known gaps" section with issue IDs instead of being
   silently carved out of the contract.

## Audit findings (2026-06-11, read-only)

Three audits of `crates/flpdf/src/` were run to ground the document:

- **Panic paths**: the panic-free guarantee currently fails in exactly one
  place — `Parser::object()`/`dictionary()`/`array()` recursion has no depth
  limit, so deeply nested dicts/arrays overflow the stack (abort). Same shape
  as qpdf CVE-2018-9918. All `unwrap`/`expect`/`unreachable!`/indexing sites
  in production code were found to be guarded by invariants. →
  `flpdf-hn1g.1`.
- **Termination**: all graph walks terminate. Recursive tree walks carry
  `DEFAULT_MAX_*_DEPTH = 100` limits (7 constants) plus visited sets;
  iterative chains (xref `/Prev`, ObjStm `/Extends`, outline `/Next`,
  dest/action chains) carry visited sets and/or 64-step caps. Two
  `inherited_field_value()` walks (signatures.rs, json_inspect.rs) have
  visited sets but no depth cap — terminating, hardening only. →
  `flpdf-hn1g.3`.
- **Resource limits**: none, matching qpdf's normal-operation posture.
  FlateDecode/LZW output is unbounded, `/Filter` chains have no length cap,
  some paths read whole files into memory. Documented as out of scope in §4
  of the threat model; opt-in limits filed as `flpdf-hn1g.4`.
- Additionally: no `unsafe` exists in `crates/flpdf/src/`, but
  `#![forbid(unsafe_code)]` is not declared → `flpdf-hn1g.6`.

## Resulting issues (epic flpdf-hn1g)

| ID | P | Summary |
| --- | --- | --- |
| flpdf-hn1g.1 | 1 | parser recursion depth limit (stack-overflow fix) |
| flpdf-hn1g.2 | 1 | cargo-fuzz harness (open → check → write) |
| flpdf-hn1g.3 | 2 | depth cap for `inherited_field_value` walks |
| flpdf-hn1g.4 | 2 | opt-in decode limits + filter-chain length cap |
| flpdf-hn1g.5 | 2 | enable GitHub private vulnerability reporting (manual) |
| flpdf-hn1g.6 | 2 | add `#![forbid(unsafe_code)]` |

## Alternatives considered

- **STRIDE / formal DFD modeling**: rejected — designed for multi-actor
  systems; a single-process parser library reduces to "hostile bytes in,
  guarantees out", which the qpdf-style structure captures directly.
- **Everything in SECURITY.md**: rejected — the threat model is a living
  engineering document; SECURITY.md stays a thin, stable reporting page.

# qpdf 11.9.0 Observable Parity Roadmap Design

## Goal

Reach observable parity with pinned qpdf 11.9.0 before flpdf 1.0, using
consumer-driven vertical slices rather than pass-count-driven patches or a
blanket bottom-up rewrite.

The roadmap covers:

- CLI output, diagnostics, exit status, file side effects, and generated PDF
  bytes;
- Rust API behavior corresponding to libqpdf concepts, including lifecycle and
  error timing;
- all upstream qtest behavior applicable to the supported Linux target; and
- internal qpdf components only where required to reproduce consumer-visible
  behavior.

The roadmap is a convergence layer over existing Beads epics. It does not
replace completed component work or duplicate open issues.

## Fixed Scope

### Oracle

- qpdf 11.9.0 at the commit enforced by `scripts/fetch-qpdf-source.sh` is the
  source and behavioral oracle.
- Later qpdf versions may be inspected to understand history, but do not change
  the pre-1.0 acceptance target.
- When local intuition, a design sketch, and qpdf disagree, measured qpdf
  behavior wins.

### Supported platform

- The official pre-1.0 parity gate runs on Linux x86_64.
- Windows-only behavior, such as Windows shell glob expansion, is excluded from
  the qtest denominator.
- Portable semantics exercised by a platform-specific upstream helper are
  retained as ordinary Rust oracle tests where relevant.

### API boundary

- C and C++ ABI or symbol compatibility is out of scope.
- `qpdf-ctest` itself is not ported and its 53 C-API invocations do not count in
  the applicable qtest denominator.
- The underlying PDF behaviors exercised only through `qpdf-ctest` must be
  mapped to equivalent Rust API oracle tests or explicitly shown to be covered
  elsewhere.

### Byte identity

- With `qpdf-zlib-compat`, every supported writer route must be byte-identical
  to qpdf under the same deterministic options.
- The default Pure Rust build retains `miniz_oxide`; it must be structurally and
  semantically identical but is not required to emit identical deflate bytes.
- No other output-byte-changing deviation is admitted.

## Current Gap Snapshot

This section is a dated input to the roadmap, not the permanent completion
ledger. Phase 0 replaces it with a machine-readable current baseline.

### qtest observability

The 2026-07-29 full survey using the `flpdf-n9t0.2` test-driver branch plus a
prototype qtest shim reported:

- 2,762 qtest-reported subtests;
- 191 passes, up from 169 on the compared main snapshot;
- 39/39 existing allowlist entries passing;
- zero allowlist regressions; and
- 2,599 informational failures before applicable/excluded classification.

The raw pass fraction is not the parity fraction. It includes unavailable
helpers, C-API-only tests, platform-specific tests, and failures that cannot yet
reach flpdf behavior. The survey parser also reports 2,790 results for qtest's
2,762 total, so its denominator must not be used as a closure gate until that
drift is fixed.

Current observability gaps include:

- `test_driver` implements only test 1; upstream uses additional IDs including
  0, 3, 28, 33, 34, 39, 52–71, 76–77, 80, 81, and 85;
- `flpdf-egzr` inventories 13 missing helpers and 88 invocations, of which
  `qpdf-ctest` contributes 53 C-API invocations that are now excluded from
  direct porting; and
- helper absence, CLI-surface absence, and actual PDF behavior differences are
  not yet represented as separate machine-readable states.

### Component and consumer gaps

`docs/qpdf-correspondence.md` shows that most qpdf responsibilities exist in
some form, but many remain distributed across route-specific implementations.
The observable missing or incomplete areas include:

- TIFF Predictor 2;
- DCT decoding and `--decode-level` consumers;
- remaining Pipeline production cutovers for AES, MD5/SHA, logging,
  page-content concatenation, and final Flate producers;
- repair behavior, diagnostic wording, offsets, and default recovery policy;
- CLI options and warning lifecycle;
- QDF/encryption null visibility and other route-specific writer behavior; and
- byte gates for encrypted and incremental writer routes.

The fact that a responsibility is `smeared` is not by itself a parity bug. It
becomes roadmap work only when the distribution causes duplicated behavior,
prevents a faithful consumer cutover, or leaves an observable route ungated.

## Delivery Method

Requirements are derived top-down and implementations are built bottom-up
within one vertical slice:

1. reproduce one qpdf consumer contract with an upstream qtest, a live oracle
   probe, or a byte differential;
2. add the failing Rust regression;
3. introduce the smallest lower-level primitive required by that contract;
4. wire the real production consumer;
5. delete the replaced eager, whole-buffer, or flpdf-only route;
6. run focused differential tests and the invariant full survey; and
7. record the measured pass-set change without using it as the design goal.

Examples of consumer contracts include:

- `isIndirect()` inspects identity without resolving the target;
- a stream decoder resolves only parameters consumed by its filter;
- a warning uses the source location owned by the object that triggered it; and
- warning completion may still produce an output file and return exit status 3.

A task is not complete when it merely adds a qpdf-shaped type or unused
abstraction.

## Roadmap

### Phase 0 — Authoritative measurement

Create the permanent parity ledger and stable survey contract.

Deliverables:

- a machine-readable classification for every qtest subtest:
  `applicable`, `excluded`, `represented`, `blocked`, `passing`, or `failing`;
- an explicit rationale and replacement-test reference for every excluded or
  represented test;
- a fix for the `2,790 parsed / 2,762 reported` survey drift;
- one reproducible command that records qpdf pin, flpdf commit, qtest commit,
  applicable denominator, passes, failure clusters, and allowlist regressions;
  and
- a root-cause-to-Bead mapping that prevents duplicate issues.

Phase 0 blocks newly created parity implementation slices. It does not block
already active, independently specified work.

### Phase 1 — Unlock upstream observation

Make applicable qtest behavior reach Rust code.

Deliverables:

- merge and wire the existing test-driver test 1 implementation;
- port test-driver IDs in responsibility-based slices rather than one monolith;
- implement the 12 non-C helpers remaining after removing `qpdf-ctest` from
  `flpdf-egzr`;
- map `qpdf-ctest` underlying behavior to Rust oracle tests; and
- distinguish shim/infrastructure failures from behavior failures in survey
  output.

Helper ports are not accepted solely because a binary exists on `PATH`; their
merged output and exit status must match the pinned helper.

### Phase 2 — Object model, parser, and repair

Converge lazy/eager boundaries and recovery semantics.

Deliverables:

- inventory `QPDFObjectHandle` consumers and state whether each operation
  inspects raw identity, resolves one value, follows a chain, or traverses a
  graph;
- replace broad eager resolution with consumer-specific access;
- converge file-object parsing, xref/trailer recovery, page-tree recovery, and
  dangling-reference behavior;
- match repair timing, warning text, object/offset attribution, ordering, and
  fatal-versus-warning decisions; and
- use test-driver 0/1 and exception paths as focused gates.

### Phase 3 — Streams, filters, and crypto

Close the remaining decode and Pipeline gaps.

Deliverables:

- TIFF Predictor 2 production support;
- DCT decoding and decode-level behavior;
- remaining QPDFStreamFilter/Flate producer cutovers;
- AES stream/string Pipeline cutover;
- MD5/SHA/count/discard production cutovers; and
- specialized-filter, decode-levels, and encryption closure matrices.

Codec parity includes chunk boundaries, finish behavior, error timing, and
diagnostics, not only whole-buffer output.

### Phase 4 — QPDFJob, CLI, and transforms

Converge user-facing orchestration.

Deliverables:

- remaining legacy and modern argument forms;
- missing qpdf options required by applicable tests;
- QPDFLogger-compatible output routing;
- warning exit and trailing-summary behavior;
- page-content concatenation cutover;
- copy-foreign, Form XObject, attachments, annotations, and related
  test-driver/helper slices; and
- deletion of replaced route-specific orchestration.

### Phase 5 — Writer convergence

Extend the existing byte-identical foundation across every supported route.

The matrix covers:

- plain full rewrite;
- QDF;
- object-stream/xref-stream output;
- linearized output;
- encrypted and copy-encryption output; and
- incremental output.

Each tuple records semantic comparison for the Pure Rust build and byte
comparison for `qpdf-zlib-compat`. Encrypted and incremental byte gates are new
required work. Existing open route-specific issues remain authoritative and
are dependencies rather than duplicated children.

### Phase 6 — Closure

The root epic closes only when:

- every applicable qtest passes on Linux x86_64;
- every represented test points to a passing Rust oracle test;
- excluded tests have an approved scope rationale;
- the result is stable across three independent builds;
- applicable divergence allowlists are empty;
- every observable writer tuple satisfies its configured semantic or byte
  gate;
- `docs/qpdf-correspondence.md` contains no unexplained missing consumer
  behavior; and
- no parity-scoped Bead remains open.

## Beads Structure

Create one new P1 root epic:

- `pre-v1.0 qpdf 11.9.0 observable parity`

Create seven child epics:

1. authoritative measurement;
2. upstream observation unlock;
3. object/parser/repair convergence;
4. stream/filter/crypto convergence;
5. QPDFJob/CLI/transform convergence;
6. writer convergence; and
7. parity closure.

Existing epics and issues are dependencies of these phase epics. They are not
reparented or recreated.

### Existing dependency groups

- observation: `flpdf-n9t0`, `flpdf-egzr`;
- object/parser/repair: `flpdf-9hc.17`, `flpdf-mfir`, `flpdf-ud7r`,
  `flpdf-xm72`, and existing recovery issues;
- stream/filter/crypto: `flpdf-qynx.5`, `flpdf-qynx.8`, `flpdf-qynx.9`,
  `flpdf-qynx.10`;
- QPDFJob/CLI/transforms: `flpdf-qynx.4`, `flpdf-qynx.7`,
  `flpdf-9hc.23`, `flpdf-w5ny`, `flpdf-w1cs`;
- writer: the completed children of `flpdf-9hc.20` as the existing floor,
  plus `flpdf-9hc.42`, `flpdf-9hc.29`, `flpdf-cecz`, and `flpdf-j4ph`.

### New gap issues

Create issues for:

- applicable qtest manifest and exclusion registry;
- full-survey result-count drift;
- `qpdf-ctest` underlying-behavior mapping;
- remaining test-driver ID inventory;
- eager/lazy `QPDFObjectHandle` consumer audit;
- TIFF Predictor 2 cutover;
- DCT/decode-level cutover;
- encrypted writer byte gate;
- incremental writer byte gate; and
- final applicable-qtest and three-build stability gate.

Update `flpdf-egzr` to exclude direct `qpdf-ctest` porting and describe the
remaining 12 helpers and 35 invocations.

### Dependency policy

- Phase 0 blocks new parity implementation slices.
- Phases 2, 3, and 4 may proceed independently after their measurement or
  helper prerequisites exist.
- Phase 5 proceeds tuple-by-tuple once the relevant behavior is available; it
  does not wait for every unrelated CLI slice.
- Phase 6 depends on all preceding phase epics.
- The root epic depends on Phase 6.

Large implementation areas are split into stacked PRs. Each stack layer must
have its own RED/GREEN evidence and 100% changed executable-line coverage
against its actual parent branch.

## Issue Acceptance Template

Every implementation issue records:

1. the qpdf 11.9.0 source or live-oracle contract;
2. the consumer and production route being changed;
3. the old route that will be removed;
4. focused observable fixtures, including failure paths;
5. differential commands and expected output/exit behavior;
6. fresh 100% changed executable-line coverage; and
7. the full-survey snapshot before and after the slice.

Inventory issues instead require a complete bounded list, ownership,
dependency ordering, and explicit follow-up Bead IDs.

## Non-goals

- C or C++ ABI compatibility;
- Windows-specific parity in the pre-1.0 Linux gate;
- qpdf versions newer than 11.9.0;
- one-to-one Rust type layout with qpdf classes;
- component ports with no consumer or oracle evidence;
- maximizing raw qtest passes by weakening comparisons or expanding an
  allowlist; and
- making default `miniz_oxide` emit zlib-identical compressed bytes.

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
- `qpdf-ctest` itself is not ported and its C-API invocations do not count in
  the applicable qtest denominator, except for the bounded
  `deterministic-id.test` `qpdf-ctest 19` adapter. That case asserts portable
  writer behavior rather than C ABI compatibility and is implemented through
  the Rust-native `flpdf-qtest-tools` boundary.
- The underlying PDF behaviors exercised only through `qpdf-ctest` must be
  mapped to equivalent Rust API oracle tests or explicitly shown to be covered
  elsewhere.

### Byte identity

- With `qpdf-zlib-compat`, every supported writer route for which qpdf produces
  the corresponding output route must be byte-identical to qpdf under the same
  deterministic options.
- The default Pure Rust build retains `miniz_oxide`; it must be structurally and
  semantically identical but is not required to emit identical deflate bytes.
- Incremental output is an flpdf-specific route: qpdf 11.9.0 opens file output
  with `wb+` and `writeStandard()` starts by writing a new header
  (`libqpdf/QPDFWriter.cc:83-98,2991-3001`), so there is no qpdf-produced
  incremental byte stream to compare. Its gate compares final-document
  semantics and structure and validates appended-revision invariants including
  `/Prev`, xref form, generations, trailer/ID handling, and warning status.
- No other output-byte-changing deviation is admitted for comparable
  qpdf-produced routes.

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
  0, 2, 3, 28, 33–36, 39, 52–71, 76–77, 80, 81, and 85;
- `flpdf-egzr` inventories 13 missing helpers and 88 invocations, of which
  `qpdf-ctest` contributes 53 C-API invocations that are excluded from direct
  porting, with the portable deterministic-ID test19 adapter as the explicit
  exception; and
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
- an encrypted-writer byte gate and an incremental-writer semantic,
  structural, and append-invariant gate.

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

- one non-overlapping composite `state` for every qtest subtest:
  `applicable`, `excluded`, `represented`, `blocked`, `passing`, or `failing`.
  The applicable denominator is the sum of `applicable`, `blocked`, `passing`,
  and `failing`; `passing` is the current direct-pass count, while `blocked`
  and `failing` distinguish observation infrastructure from reached behavior;
- explicit composite-state transitions: an unmeasured runnable entry starts as
  `applicable` and moves to `blocked`, `passing`, or `failing` after survey;
  `blocked` moves to `passing` or `failing` when observation is unlocked;
  `failing` and `passing` may transition between each other as behavior changes;
  scope evidence may reclassify an entry as `excluded` or `represented`, with
  the required rationale or replacement reference;
- an explicit rationale for every excluded test, a concrete Rust-test reference
  for every represented test, and—by Phase 1 closure—a concrete Rust test or
  narrower follow-up Bead for every excluded entry that exercises portable PDF
  behavior; true ABI-only exclusions require rationale only. During Phase 0,
  direct `qpdf-ctest` entries may provisionally reference the actual
  created-or-reused Phase 1 mapping Bead ID; Phase 1 replaces every
  provisional reference with a Rust test, a narrower follow-up Bead, or an
  ABI-only scope rationale;
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
- port test-driver IDs in responsibility-based slices rather than one
  monolith. Every implementation follow-up created by the inventory becomes a
  Phase 1 child with the roadmap labels, this spec ID, and the full
  implementation acceptance template; it blocks the inventory task and
  therefore Phase 1 closure;
- implement the 11 Linux-applicable non-C helpers remaining after removing
  `qpdf-ctest` and Windows-only `test_shell_glob` from `flpdf-egzr`; the
  deterministic-ID test19 adapter remains a bounded Rust-native qtest helper
  rather than a C ABI port;
- map `qpdf-ctest` underlying behavior to Rust oracle tests and eliminate all
  provisional mapping-Bead references from the manifest. Every uncovered
  portable behavior becomes a Phase 1 child implementation issue with the
  roadmap labels, this spec ID, and the full implementation acceptance
  template; it blocks the mapping task and therefore Phase 1 closure; and
- distinguish shim/infrastructure failures from behavior failures in survey
  output.

The Phase 0 manifest task owns the initial machine-readable
infrastructure-versus-behavior split. Phase 1 consumes that ledger, replaces
the provisional C-API mapping references, and closes only after no provisional
reference remains.

Helper ports are not accepted solely because a binary exists on `PATH`; their
merged output and exit status must match the pinned helper.

### Phase 2 — Object model, parser, and repair

Converge lazy/eager boundaries and recovery semantics.

Deliverables:

- inventory `QPDFObjectHandle` consumers and state whether each operation
  inspects raw identity, resolves one value, follows a chain, or traverses a
  graph. Every implementation follow-up created for a discovered mismatch
  becomes a Phase 2 child with the roadmap labels, this spec ID, and the full
  implementation acceptance template; it blocks the audit task and therefore
  Phase 2 closure;
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
- encrypted output plus a deterministic `--copy-encryption-from` tuple whose
  donor PDF, passwords, permissions, IDs, and randomness inputs are fixed; and
- incremental output.

Each qpdf-produced tuple records semantic and structural comparison for the
Pure Rust build and byte comparison for `qpdf-zlib-compat`. This applies
independently to direct encryption and the donor-based copy-encryption tuple.
The encrypted byte gate is new required work. The flpdf-specific incremental
tuple records final-document
semantic and structural comparison plus appended-revision invariants; it is
explicitly excluded from qpdf byte identity because qpdf produces only a full
rewrite. Existing open route-specific issues remain authoritative and are
dependencies rather than duplicated children.

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
- no parity-scoped implementation Bead outside the ordered closure chain
  remains open. The final closure task closes first, then the Phase 6 epic,
  then the root epic; each closure item exempts itself and its still-open
  closure ancestors when evaluating this condition.

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

Existing required epics block their phase epics directly. Because Beads permits
an epic to be blocked only by another epic, each phase with required non-epic
work has a child completion-gate task; required tasks and features block that
gate, and parent-child progress prevents the phase epic from closing early.
Non-observable refactors may remain non-blocking roadmap associations.
Existing work is not reparented or recreated.

### Existing dependency groups

- observation: `flpdf-n9t0`, `flpdf-egzr`;
- object/parser/repair: **no phase blocker** (see the note below); completion-gate
  blockers `flpdf-ud7r`, `flpdf-xm72`, `flpdf-fmb9`, and `flpdf-4zt3`;
  `flpdf-9hc.17` and `flpdf-mfir` remain non-blocking related associations —
  accessor deduplication has no observable parity contract;
- stream/filter/crypto: phase blocker `flpdf-qynx.5`; completion-gate blockers
  `flpdf-qynx.8`, `flpdf-qynx.9`, and `flpdf-qynx.10`;
- QPDFJob/CLI/transforms: phase blocker `flpdf-9hc.23`; completion-gate blockers
  `flpdf-qynx.4`, `flpdf-qynx.7`, `flpdf-w5ny`, and `flpdf-w1cs`;
- writer: the completed children of `flpdf-9hc.20` as the existing floor,
  with `flpdf-9hc.20` as the phase blocker; completion-gate blockers
  `flpdf-9hc.42`, `flpdf-9hc.29`, `flpdf-cecz`, and `flpdf-j4ph`.

#### Correction (2026-08-03): object/parser/repair has no phase blocker

`flpdf-9hc.17` was a phase blocker on `flpdf-25kg.3` and has been demoted to
`relates-to`. Beads propagates a blocked parent's state to its children — the
policy above already says so — so that one edge held all twelve children of a
P1 phase epic out of `bd ready`, including `flpdf-25kg.3.5`, on behalf of a P3
epic. That contradicts "Phases 2, 3, and 4 may proceed independently after
their measurement or helper prerequisites exist".

It is `relates-to` rather than a completion-gate blocker because Beads rejects
epic-to-task blocking (`tasks can only block other tasks, not epics`), and
`flpdf-9hc.17` is an epic with eight children. The completion gate
`flpdf-25kg.3.2` can only be blocked by non-epic work, which is why its four
existing blockers are all tasks.

Reviewing its children confirms it was never an implementation prerequisite:
`--ignore-xref-streams`, `--suppress-recovery`, and the CLI slice are flags;
trailer and page-tree rebuild are recovery routines; the diagnostic slice is
wording. Only `flpdf-9hc.17.5` (object-stream resilience) touches the same
territory as `flpdf-25kg.3.5`, and the natural order there is the reverse —
canonical `resolveObjectsInStream` first, resilience on top of it.

**`flpdf-25kg.5` and `flpdf-25kg.6` still carry the same shape**: P3 epics
`flpdf-9hc.23` and `flpdf-9hc.20` gate a P2 and a P1 phase respectively. They
were left alone here; decide them on their own evidence rather than by
analogy.

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
- incremental writer semantic, structural, and append-invariant gate; and
- final applicable-qtest and three-build stability gate.

Create four coordination-only completion-gate tasks under Phases 2–5. These
tasks add no implementation scope; they mechanically prevent a phase epic from
closing while one of its required independently owned non-epic issues is open.

Update `flpdf-egzr` to exclude direct `qpdf-ctest` porting and Windows-only
`test_shell_glob`, leaving 11 Linux-applicable helpers and 34 invocations.

### Dependency policy

- Blocking edges express actual implementation readiness. Beads permits
  epic-to-epic blocking but rejects task/feature-to-epic blocking; non-epic
  work items may use mixed blocking types. Required non-epic work therefore
  blocks a child completion-gate task, while `relates-to` is reserved for
  non-blocking roadmap associations.
- Phase 0 blocks new parity implementation slices.
- Phases 2, 3, and 4 may proceed independently after their measurement or
  helper prerequisites exist.
- Phase 5 proceeds tuple-by-tuple once the relevant behavior is available; it
  does not wait for every unrelated CLI slice.
- Phase 6 depends on all preceding phase epics.
- The root epic owns all seven phases through parent-child progress; do not add
  an explicit root-to-Phase-6 blocking edge because Beads propagates a blocked
  parent's state to its children and would prevent Phase 0 from becoming ready.

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

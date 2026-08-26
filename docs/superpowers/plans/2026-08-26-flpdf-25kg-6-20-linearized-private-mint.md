# Implementation plan — flpdf-25kg.6.20

## 1. Add the qpdf oracle fixture

- Add `tests/fixtures/compat/objstm-lin-firstpage-private-mint.pdf` with a
  direct inherited `/Pages /MediaBox` array, one page, two page-private
  annotation dictionaries, a font, and a content stream.
- Add the matching qpdf 11.9.0 generation command to
  `tests/golden/regenerate.sh` and generate the `--static-id` golden at
  `tests/golden/references/objstm-lin-firstpage-private-mint/linearize-objstm-static.pdf`.
- Run `qpdf --check` on the source and `qpdf --check-linearization` on the
  golden before adding the regression assertion.

## 2. RED regression test

- Add structural and strict static-ID Generate tests to
  `crates/flpdf/tests/cmp_linearize_objstm_tests.rs` using the existing golden
  comparison helper. The source fixture has no `/ID`, so deterministic-ID seed
  differences remain outside this layout regression.
- Run the focused test against the current implementation and record the
  expected first diff: flpdf places the optimization-minted first-page-private
  plain object after the first-half ObjStm container, while qpdf places it
  before that container.

## 3. Minimal implementation

- In `linearization/writer.rs`, keep the initial `part6_outline_objects`
  post-container set and the existing post-optimization handling for
  `part3_objects`, open-document plain objects, and outlines.
- Remove only `part2_objects` from the post-optimization extension. This lets
  `RenumberMap::place_objstm_members_per_half` retain a newly minted
  first-page-private plain object in the pre-container first-half sequence.
- Update the nearby comment to state the qpdf-backed private/shared distinction
  and avoid implying that every post-optimization first-half object follows a
  container.

## 4. Verification

- Run the focused new tests and the complete linearization unit/integration
  suites.
- Run `cargo fmt -- --check`, strict rustdoc, all-features clippy, workspace
  tests, qpdf module correspondence tests, deviation tests, and patch
  coverage.
- Re-run the qpdf live comparison and `qpdf --check-linearization` on the new
  output; verify existing first-page private/shared and shared-mint controls
  are unchanged.

## 5. Delivery

- Commit the fixture/test first (RED evidence), then the implementation and
  documentation updates.
- Fetch and rebase onto the latest `origin/main`, rerun the relevant gates,
  push the branch, and create a Draft PR.
- Wait for all CI and review checks, then mark the PR ready only when every
  required check is green. Do not merge.
- Record the PR and verification state in Beads while leaving the issue open
  for integration closeout; run `bd dep cycles` and `bd dolt push`.

# Borrowed ResourceFinder Operands Review Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the unnecessary deep clone of ordinary content-stream operands and reply to both outstanding PR #578 review threads with verified implementation or qpdf 11.9.0 evidence.

**Architecture:** Add one crate-private borrowed classification entry point to `ResourceFinder`; keep the owned `ParserCallbacks` entry point as a delegating adapter. `ResourceCallbacks` will classify the borrowed object before consuming that same object for inline-image and XObject logic. Preserve qpdf's buffered `Pl_QPDFTokenizer` lifecycle and explain that decision in-thread instead of changing it.

**Tech Stack:** Rust 2021, flpdf parser callbacks, qpdf 11.9.0 pinned source and differential probe, Cargo, llvm-cov, GitHub GraphQL.

## Global Constraints

- qpdf 11.9.0 behavior and lifecycle are the oracle.
- Do not change `QpdfTokenizer` buffering or callback timing.
- Clone only resource-name bytes that `ResourceFinder` must retain.
- Keep inline-image opacity, raw offsets, diagnostic handling, XObject encounter order, and structural-error propagation unchanged.
- Add no dependency and no public API.
- CI patch coverage must remain 100% of changed executable lines.
- Reply in the two original review threads; do not resolve them unless the user separately requests resolution.

---

### Task 1: Classify ResourceFinder operands by borrow

**Files:**
- Modify: `crates/flpdf/src/resource_finder.rs:92-126`
- Modify: `crates/flpdf/src/resources.rs:718-729`
- Test: `crates/flpdf/src/resource_finder.rs` unit-test module

**Interfaces:**
- Consumes: `Object`, `ParseControl`, `ParserCallbacks`, and the existing `operator_resource_type` state machine.
- Produces: `ResourceFinder::handle_object_borrowed(&mut self, object: &Object, offset: usize, length: usize) -> Result<ParseControl>`.

- [ ] **Step 1: Add focused tests for the borrowed classification contract**

Add these tests to the `resource_finder.rs` test module. The first production
mutation they catch is deleting the borrowed entry point or consuming the
caller's object; the second catches failure to retain owned name bytes or
failure to classify the following operator.

```rust
#[test]
fn borrowed_large_operand_remains_owned_by_the_caller() {
    let operand = Object::String(vec![b'x'; 1024 * 1024]);
    let original_ptr = operand.as_string().unwrap().as_ptr();
    let mut finder = ResourceFinder::default();

    finder
        .handle_object_borrowed(&operand, 17, 1024 * 1024)
        .unwrap();

    assert!(finder.has_pending_operands());
    assert_eq!(operand.as_string().unwrap().as_ptr(), original_ptr);
    assert_eq!(operand.as_string().unwrap().len(), 1024 * 1024);
}

#[test]
fn borrowed_name_is_retained_for_the_following_operator() {
    let name = Object::Name(b"F1".to_vec());
    let operator = Object::Operator(b"Tf".to_vec());
    let mut finder = ResourceFinder::default();

    finder.handle_object_borrowed(&name, 23, 3).unwrap();
    finder.handle_object_borrowed(&operator, 27, 2).unwrap();

    assert_eq!(name.as_name(), Some(b"F1".as_slice()));
    assert!(
        finder
            .names_by_resource_type()
            .get(b"Font".as_slice())
            .unwrap()
            .get(b"F1".as_slice())
            .unwrap()
            .contains(&23)
    );
}
```

- [ ] **Step 2: Run the focused tests and record strict RED**

Run:

```bash
cargo test -p flpdf --lib resource_finder::tests::borrowed_large_operand_remains_owned_by_the_caller -- --exact
cargo test -p flpdf --lib resource_finder::tests::borrowed_name_is_retained_for_the_following_operator -- --exact
```

Expected: compilation fails because `ResourceFinder::handle_object_borrowed`
does not exist. This is the intended RED; a syntax or fixture failure is not.

- [ ] **Step 3: Add the minimal borrowed classifier**

Move the existing match body into the crate-private borrowed method. Clone only
the name bytes that must outlive the callback:

```rust
impl ResourceFinder {
    pub(crate) fn handle_object_borrowed(
        &mut self,
        object: &Object,
        offset: usize,
        _length: usize,
    ) -> Result<ParseControl> {
        match object {
            Object::Name(name) => {
                self.pending_operands = true;
                self.last_name = Some((name.clone(), offset));
            }
            Object::Operator(operator) => {
                self.last_operator_started_at_boundary = !self.pending_operands;
                self.pending_operands = false;
                if let Some(resource_type) = operator_resource_type(operator) {
                    self.record_last_name(resource_type);
                }
            }
            Object::InlineImage(_) => {}
            _ => self.pending_operands = true,
        }
        Ok(ParseControl::Continue)
    }
}
```

Keep the parser-facing owned callback as a thin adapter:

```rust
fn handle_object(
    &mut self,
    object: Object,
    offset: usize,
    length: usize,
) -> Result<ParseControl> {
    self.handle_object_borrowed(&object, offset, length)
}
```

- [ ] **Step 4: Route ResourceCallbacks through the borrowed classifier**

Replace the clone dispatch with a borrow before the existing consuming match:

```rust
self.finder
    .handle_object_borrowed(&object, offset, length)?;
match object {
    // keep every existing arm unchanged
}
```

Do not recreate resource classification in `ResourceCallbacks`. The borrowed
finder method already ignores `Object::InlineImage`, so the payload remains
opaque without any payload-sized clone.

- [ ] **Step 5: Run focused GREEN and boundary regressions**

Run:

```bash
cargo test -p flpdf --lib borrowed_ -- --nocapture
cargo test -p flpdf --lib resource_finder::tests -- --nocapture
cargo test -p flpdf --test resource_pruning_tests
```

Expected: all tests pass. Mentally mutate the borrowed name clone away and the
name/operator test must fail; remove the borrowed finder call from
`ResourceCallbacks` and the pruning tests must fail.

- [ ] **Step 6: Run code-quality and parity gates**

Run:

```bash
cargo fmt -- --check
cargo clippy -p flpdf --lib --tests -- -D warnings
cargo test -p flpdf
bash scripts/qpdf-tokenizer-diff.sh
git diff --check
```

Expected: all commands pass. The differential output must report all five
qpdf tokenizer/resource cases as passing.

- [ ] **Step 7: Commit the tested implementation**

```bash
git add crates/flpdf/src/resource_finder.rs crates/flpdf/src/resources.rs
git commit -m "fix(resources): avoid cloning borrowed operands"
```

- [ ] **Step 8: Run fresh changed-line coverage on the committed tree**

Run:

```bash
scripts/patch-coverage.sh --base origin/main
```

Expected: `uncovered 0` and `PASS (100%)`. If a real new branch is uncovered,
add a behavior test that fails when that branch is removed, amend the commit,
and rerun Step 5, Step 6, and this fresh coverage command on the amended HEAD.

### Task 2: Publish and reply to both review threads

**Files:**
- Read: `/home/ubuntu/.cache/flpdf/qpdf-11.9.0/libqpdf/Pl_QPDFTokenizer.cc:30-64`
- Read: `crates/flpdf/src/pipeline/qpdf_tokenizer.rs:60-101`
- Read: `crates/flpdf/src/resource_finder.rs`
- Read: `crates/flpdf/src/resources.rs`

**Interfaces:**
- Consumes: verified Task 1 commit and PR #578 thread IDs
  `PRRT_kwDOSYPosM6USPdj` and `PRRT_kwDOSYPosM6USeTG`.
- Produces: pushed PR HEAD and one factual reply in each original review thread.

- [ ] **Step 1: Verify publication scope**

Run:

```bash
git status --short --branch
git log --oneline origin/feature/flpdf-qynx-3-resource-cutover..HEAD
git diff --stat origin/feature/flpdf-qynx-3-resource-cutover...HEAD
```

Expected: a clean worktree and only the approved design plus borrowed-operand
implementation commits ahead of the remote branch.

- [ ] **Step 2: Push Git and Beads**

Run:

```bash
git push origin feature/flpdf-qynx-3-resource-cutover
bd dolt push
```

Expected: both pushes succeed and the remote branch points at local `HEAD`.

- [ ] **Step 3: Reply to the buffered-tokenizer thread**

Use `addPullRequestReviewThreadReply` for
`PRRT_kwDOSYPosM6USPdj`:

```bash
current_head="$(git rev-parse --short HEAD)"
body="I rechecked this against the pinned qpdf 11.9.0 implementation. \`Pl_QPDFTokenizer::write\` appends each chunk to \`Pl_Buffer\`, and tokenization starts only in \`finish\` after constructing a \`BufferInputSource\` (\`libqpdf/Pl_QPDFTokenizer.cc:30-44\`). The current buffered input and delayed callback/error timing intentionally mirror that lifecycle; processing tokens during \`write\` would diverge from the oracle. A separate one-shot allocation API could be considered independently, but it should not replace this pipeline contract, so no tokenizer change is included. The full qpdf tokenizer differential remains green at \`${current_head}\`."
gh api graphql \
  -f query='mutation($thread: ID!, $body: String!) { addPullRequestReviewThreadReply(input: {pullRequestReviewThreadId: $thread, body: $body}) { comment { url } } }' \
  -F thread='PRRT_kwDOSYPosM6USPdj' \
  -f body="$body"
```

- [ ] **Step 4: Reply to the borrowed-operand thread**

Use `addPullRequestReviewThreadReply` for
`PRRT_kwDOSYPosM6USeTG`:

```bash
implementation_commit="$(git log -1 --format=%h -- crates/flpdf/src/resource_finder.rs)"
body="Fixed in \`${implementation_commit}\`. \`ResourceFinder\` now has a borrowed object-classification path, and \`ResourceCallbacks\` uses it before consuming the same object. Only resource-name bytes that must be retained are cloned; large strings, arrays, dictionaries, streams, and inline-image payloads are no longer duplicated. The parser callback contract, inline-image opacity, XObject encounter order, and error propagation are unchanged. Verified with the full \`flpdf\` suite, Clippy, the qpdf 11.9.0 differential, and fresh changed-line coverage at 100% with 0 uncovered executable lines."
gh api graphql \
  -f query='mutation($thread: ID!, $body: String!) { addPullRequestReviewThreadReply(input: {pullRequestReviewThreadId: $thread, body: $body}) { comment { url } } }' \
  -F thread='PRRT_kwDOSYPosM6USeTG' \
  -f body="$body"
```

- [ ] **Step 5: Verify replies without resolving threads**

Re-fetch PR #578 with the bundled `fetch_comments.py` workflow and GraphQL.
Expected:

- each target thread contains the new reply;
- both threads remain `isResolved: false`;
- PR `headRefOid` equals local `HEAD`.

Do not call `resolveReviewThread`; the user requested replies only.

- [ ] **Step 6: Wait for GitHub checks and perform final readback**

Run:

```bash
gh pr checks 578 --watch --interval 10
git status --short --branch
git rev-parse HEAD
git rev-parse origin/feature/flpdf-qynx-3-resource-cutover
```

Expected: all required checks pass, the worktree is clean, and both OIDs are
identical. Report that the PR remains blocked while the replied threads are
unresolved.

# Indirect-Length ObjStm Repair Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore the pre-refactor public and strict parser contract for streams without a directly usable `/Length`, so best-effort xref reconstruction can recover ObjStm members whose container uses an indirect length.

**Architecture:** Extend the Layer 2 completion policy with a mode that performs token-bounded recovery but requires an actual `endstream` or `endobj` terminator. Layer 4 compatibility entry points use that policy and consume the completion engine's recovery-EOL metadata, preserving both legacy logical payload bytes and strict rejection of truncated streams.

**Tech Stack:** Rust workspace, `cargo test`, qpdf 11.9.0 compatibility oracle, GitHub stacked PRs, Beads.

## Global Constraints

- qpdf 11.9.0 source and observed behavior are the parity oracle.
- Do not add an xref-only stream scanner or restore removed parser-owned stream machinery.
- Keep strict empty-body, top-level-reference, trailing-byte, and missing-terminator behavior unchanged.
- Keep recovery token-bounded and within the supplied object slice.
- Preserve raw recovery EOL ownership for production reader decryption; compatibility parser entry points return the legacy logical payload without the framing EOL.
- Put the completion policy on `stack/flpdf-15jp-stream-completion` and the parser/xref integration on `stack/flpdf-15jp-container-xref-routing`.
- Do not resolve the review thread until committed-HEAD verification and push succeed.
- Keep `flpdf-15jp` and its children open because the other whole-stack review blockers remain.

## File Map

- `crates/flpdf/src/reader/file_object.rs`: add the terminator-required bounded policy and share recovery-EOL removal.
- `crates/flpdf/src/parser.rs`: compose strict indirect parsing with the new policy.
- `crates/flpdf/tests/parser_tests.rs`: cover public `parse_object` compatibility.
- `crates/flpdf/tests/xref_tests.rs`: cover best-effort reconstruction of an ObjStm with indirect `/Length`.

---

### Task 1: Add terminator-required bounded completion in Layer 2

**Files:**
- Modify: `crates/flpdf/src/reader/file_object.rs:15-19`
- Modify: `crates/flpdf/src/reader/file_object.rs:270-330`
- Modify: `crates/flpdf/src/reader/file_object.rs:430-470`
- Test: `crates/flpdf/src/reader/file_object.rs`

**Interfaces:**
- Consumes: `finish_file_object(input, pending, resolved_indirect_length, RecoveryPolicy)`.
- Produces: `RecoveryPolicy::RequireTerminator`, which recovers an unusable length when a token-bounded terminator exists and returns `Error::Parse` when none exists.

- [ ] **Step 1: Add failing policy tests**

Add these tests to `reader/file_object.rs`:

```rust
#[test]
fn require_terminator_policy_recovers_unresolved_length() {
    let input =
        b"1 0 obj\n<< /Length 9 0 R >>\nstream\nabc\nendstream\nendobj\n";
    let pending = parse_file_object_syntax(input).unwrap();
    let completed =
        finish_file_object(input, pending, None, RecoveryPolicy::RequireTerminator).unwrap();
    assert_eq!(completed.object.as_stream().unwrap().data, b"abc\n");
    assert_eq!(
        completed.included_recovery_eol,
        Some(IncludedStreamDataEol::Lf)
    );
}

#[test]
fn require_terminator_policy_rejects_truncated_stream() {
    let input = b"1 0 obj\n<< /Length 9 0 R >>\nstream\nabc";
    let pending = parse_file_object_syntax(input).unwrap();
    assert!(
        finish_file_object(input, pending, None, RecoveryPolicy::RequireTerminator).is_err()
    );
}
```

- [ ] **Step 2: Run the tests to verify RED**

Run:

```bash
cargo test -p flpdf require_terminator_policy -- --nocapture
```

Expected: compile failure because `RecoveryPolicy::RequireTerminator` does not exist.

- [ ] **Step 3: Add the policy variant**

Change the enum to:

```rust
pub(crate) enum RecoveryPolicy {
    Strict,
    Bounded,
    RequireTerminator,
}
```

- [ ] **Step 4: Make recovery absence explicit**

Change `recover_stream_boundary` to return `Option<(usize, usize)>`:

```rust
fn recover_stream_boundary(
    input: &[u8],
    data_start: usize,
    diagnostics: &mut Vec<FileObjectDiagnostic>,
) -> Option<(usize, usize)> {
    diagnostics.push(FileObjectDiagnostic {
        kind: FileObjectDiagnosticKind::AttemptingStreamLengthRecovery,
        relative_offset: data_start,
    });

    if let Some(terminator) = find_recovery_terminator(input, data_start) {
        let data_end = terminator.position();
        let length = data_end - data_start;
        diagnostics.push(FileObjectDiagnostic {
            kind: if length == 0 {
                FileObjectDiagnosticKind::EmptyRecoveredStream
            } else {
                FileObjectDiagnosticKind::RecoveredStreamLength { length }
            },
            relative_offset: data_start,
        });
        return Some((data_end, terminator.after_body()));
    }

    diagnostics.push(FileObjectDiagnostic {
        kind: FileObjectDiagnosticKind::EmptyRecoveredStream,
        relative_offset: data_start,
    });
    None
}
```

- [ ] **Step 5: Preserve Bounded behavior and enforce RequireTerminator**

In `complete_stream`, replace the recovery match arm with:

```rust
None if policy != RecoveryPolicy::Strict => {
    if let Some(kind) = invalid_length.as_ref() {
        diagnostics.push(FileObjectDiagnostic {
            kind: kind.clone(),
            relative_offset: 0,
        });
    } else {
        diagnostics.push(FileObjectDiagnostic {
            kind: FileObjectDiagnosticKind::ExpectedEndstream,
            relative_offset: exact_end.unwrap_or(data_start),
        });
    }

    match recover_stream_boundary(input, data_start, &mut diagnostics) {
        Some((end, after)) => (
            end,
            after,
            included_stream_data_eol(input, data_start, end),
        ),
        None if policy == RecoveryPolicy::RequireTerminator => {
            return Err(Error::parse(data_start, "stream data exceeds input"));
        }
        None => (data_start, input.len(), None),
    }
}
```

Leave the `Strict` error arm unchanged. This preserves the existing qpdf-style empty-stream result for `Bounded` callers while giving compatibility entry points the legacy truncated-stream error.

- [ ] **Step 6: Run Layer 2 tests**

Run:

```bash
cargo test -p flpdf require_terminator_policy
cargo test -p flpdf reader::file_object::tests
cargo fmt --all -- --check
git diff --check
```

Expected: PASS.

- [ ] **Step 7: Commit and push Layer 2**

```bash
git add crates/flpdf/src/reader/file_object.rs
git commit -m "refactor: require a stream recovery terminator"
git push origin stack/flpdf-15jp-stream-completion
```

- [ ] **Step 8: Propagate Layer 2 through the stack**

Merge Layer 2 into Layer 3, push Layer 3, then merge Layer 3 into Layer 4 and push Layer 4:

```bash
git -C /tmp/flpdf-15jp-layer3-ci merge --no-edit stack/flpdf-15jp-stream-completion
git -C /tmp/flpdf-15jp-layer3-ci push origin stack/flpdf-15jp-normal-object-routing
git -C /home/ubuntu/flpdf/.worktrees/flpdf-15jp-qpdf-file-reader merge --no-edit stack/flpdf-15jp-normal-object-routing
git -C /home/ubuntu/flpdf/.worktrees/flpdf-15jp-qpdf-file-reader push origin stack/flpdf-15jp-container-xref-routing
```

Expected: both merges are clean and every remote head matches its local branch.

### Task 2: Restore compatibility entry points and ObjStm repair in Layer 4

**Files:**
- Modify: `crates/flpdf/src/reader/file_object.rs:104-125`
- Modify: `crates/flpdf/src/reader/file_object.rs:205-235`
- Modify: `crates/flpdf/src/parser.rs:36-46`
- Test: `crates/flpdf/src/parser.rs`
- Test: `crates/flpdf/tests/parser_tests.rs`
- Test: `crates/flpdf/tests/xref_tests.rs`

**Interfaces:**
- Consumes: `RecoveryPolicy::RequireTerminator` and existing `IncludedStreamDataEol`.
- Produces: unchanged `parse_object` and `parse_indirect_object` signatures with legacy stream recovery; best-effort xref repair reconstructs `XrefOffset::Compressed { stream: 5, index: 0 }` for an indirect-length ObjStm.

- [ ] **Step 1: Add failing public parser tests**

Add this helper and three tests to `crates/flpdf/tests/parser_tests.rs`:

```rust
fn parsed_stream_data(input: &[u8]) -> Vec<u8> {
    parse_object(input)
        .expect("stream must parse")
        .into_stream()
        .expect("expected stream")
        .data
}

#[test]
fn public_parser_recovers_indirect_stream_length() {
    assert_eq!(
        parsed_stream_data(b"<< /Length 9 0 R >>\nstream\nabc\nendstream"),
        b"abc"
    );
}

#[test]
fn public_parser_recovers_missing_stream_length() {
    assert_eq!(
        parsed_stream_data(b"<< >>\nstream\nabc\nendstream"),
        b"abc"
    );
}

#[test]
fn public_parser_recovers_non_integer_stream_length() {
    assert_eq!(
        parsed_stream_data(b"<< /Length /Bad >>\nstream\nabc\nendstream"),
        b"abc"
    );
}
```

- [ ] **Step 2: Add the failing strict parser test**

Add this test to `parser.rs`'s `stream_length_tests` module:

```rust
#[test]
fn strict_indirect_parser_recovers_unresolved_indirect_length() {
    let input =
        b"3 0 obj\n<< /Length 9 0 R >>\nstream\nstrict payload\nendstream\nendobj\n";
    let (_, object) = parse_indirect_object(input).expect("strict indirect stream must parse");
    assert_eq!(
        object.as_stream().expect("expected stream").data,
        b"strict payload"
    );
}
```

- [ ] **Step 3: Add the failing xref-repair test**

Add this test beside the existing best-effort ObjStm tests:

```rust
#[test]
fn best_effort_recovers_objstm_with_indirect_length() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let objstm_data = b"7 0 <</Foo 1>>";
    let objstm_offset = bytes.len() as u64;
    bytes.extend_from_slice(
        b"5 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length 6 0 R >>\nstream\n",
    );
    bytes.extend_from_slice(objstm_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("6 0 obj\n{}\nendobj\n", objstm_data.len()).as_bytes());

    let start_xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n")
            .as_bytes(),
    );
    bytes[start_xref + 2] = b'z';

    load_xref_and_trailer(&mut Cursor::new(bytes.clone()))
        .expect_err("corrupt xref must fail strict loading");
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();

    assert_eq!(
        loaded.entries.get(&ObjectRef::new(5, 0)),
        Some(&XrefOffset::Offset(objstm_offset))
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(7, 0)),
        Some(&XrefOffset::Compressed {
            stream: 5,
            index: 0,
        })
    );
}
```

- [ ] **Step 4: Run tests to verify RED**

Run:

```bash
cargo test -p flpdf --test parser_tests public_parser_recovers -- --nocapture
cargo test -p flpdf strict_indirect_parser_recovers_unresolved_indirect_length -- --nocapture
cargo test -p flpdf --test xref_tests best_effort_recovers_objstm_with_indirect_length -- --nocapture
```

Expected: public and strict parser tests fail with unusable `/Length`; xref repair fails because object 7 has no compressed entry.

- [ ] **Step 5: Share recovery-EOL removal**

Extract the body of `FileObjectRead::remove_included_recovery_eol_for_decryption` into:

```rust
fn remove_included_recovery_eol(
    object: &mut Object,
    included_recovery_eol: &mut Option<IncludedStreamDataEol>,
) -> Option<RecoveredStreamEol> {
    let included = (*included_recovery_eol)?;
    let stream = object
        .as_stream_mut()
        .expect("included recovery EOL belongs to a stream");
    let eol = included.as_bytes();
    assert!(
        stream.data.ends_with(eol),
        "included recovery EOL must remain in raw stream data"
    );
    stream.data.truncate(stream.data.len() - eol.len());
    *included_recovery_eol = None;
    Some(included.as_removed())
}
```

Delegate the existing method:

```rust
pub(crate) fn remove_included_recovery_eol_for_decryption(
    &mut self,
) -> Option<RecoveredStreamEol> {
    remove_included_recovery_eol(&mut self.object, &mut self.included_recovery_eol)
}
```

- [ ] **Step 6: Restore public direct-stream compatibility**

In `finish_strict_direct_object`, call `complete_stream` with
`RecoveryPolicy::RequireTerminator`, preserve trailing-byte validation, then
call:

```rust
let _ = remove_included_recovery_eol(
    &mut completed.object,
    &mut completed.included_recovery_eol,
);
```

Return `completed.object`. Do not change grammar or exact integer-length behavior.

- [ ] **Step 7: Restore strict indirect-stream compatibility**

Change `parse_indirect_object` to:

```rust
pub(crate) fn parse_indirect_object(input: &[u8]) -> Result<(ObjectRef, Object)> {
    let pending = crate::reader::file_object::parse_strict_file_object_syntax(input)?;
    let mut completed = crate::reader::file_object::finish_file_object(
        input,
        pending,
        None,
        crate::reader::file_object::RecoveryPolicy::RequireTerminator,
    )?;
    let _ = completed.remove_included_recovery_eol_for_decryption();
    Ok((completed.object_ref, completed.object))
}
```

- [ ] **Step 8: Run focused tests to verify GREEN**

Run:

```bash
cargo test -p flpdf --test parser_tests
cargo test -p flpdf strict_indirect_parser_recovers_unresolved_indirect_length
cargo test -p flpdf reader::file_object::tests
cargo test -p flpdf --test xref_tests
```

Expected: PASS, including existing missing-`endstream`, trailing-byte, exact-length, and raw-EOL tests.

- [ ] **Step 9: Commit Layer 4**

```bash
git add crates/flpdf/src/parser.rs crates/flpdf/src/reader/file_object.rs crates/flpdf/tests/parser_tests.rs crates/flpdf/tests/xref_tests.rs
git commit -m "fix: recover indirect-length ObjStm entries"
```

### Task 3: Verify, publish, and resolve the review thread

**Files:**
- No production file changes expected.
- Review thread: `https://github.com/fulgur-rs/flpdf/pull/537#discussion_r3647353999`

**Interfaces:**
- Consumes: committed Layer 2 and Layer 4 changes.
- Produces: pushed stack heads, green local gates, an in-thread reply, and only the requested thread resolved.

- [ ] **Step 1: Run quality gates**

Run:

```bash
cargo fmt --all -- --check
git diff --check
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo build --release
```

Expected: all commands exit 0.

- [ ] **Step 2: Gate changed-line coverage from committed HEAD**

Run on Layer 2:

```bash
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --all-features --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/stack/flpdf-15jp-file-object-syntax --lcov target/patch-cov.lcov
```

Run again on Layer 4:

```bash
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --all-features --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/stack/flpdf-15jp-normal-object-routing --lcov target/patch-cov.lcov
```

Expected: both changed-line coverage reports are exactly 100%.

- [ ] **Step 3: Run qpdf compatibility smoke**

Run:

```bash
qpdf --version
cd /home/ubuntu/flpdf-qtest
scripts/run.sh
```

Expected: qpdf reports 11.9.0 and the harness log contains `basic-parsing 41 (create qdf) ... PASSED`. Other known aggregate qtest failures remain informational.

- [ ] **Step 4: Review and push final heads**

Run:

```bash
git status --short --branch
git diff --stat origin/stack/flpdf-15jp-normal-object-routing..HEAD
git log --oneline origin/stack/flpdf-15jp-normal-object-routing..HEAD
git push origin stack/flpdf-15jp-container-xref-routing
```

Expected: the Layer 4 worktree is clean and the remote head equals local `HEAD`.

- [ ] **Step 5: Reply in the exact review thread**

Use `addPullRequestReviewThreadReply` for `PRRT_kwDOSYPosM6Tojct`:

```text
Fixed in `<commit>` by composing compatibility parser entry points with terminator-required bounded completion. Strict grammar and truncated-stream rejection remain unchanged, while best-effort xref repair now recovers an ObjStm whose `/Length` is indirect. Added public-parser, strict-parser, and end-to-end xref regression coverage.

Verified with:
- `cargo test -p flpdf --test parser_tests`
- `cargo test -p flpdf --test xref_tests`
- `cargo test`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- committed-HEAD patch coverage: 100%
```

- [ ] **Step 6: Resolve only this thread**

Use `resolveReviewThread` for `PRRT_kwDOSYPosM6Tojct`.

Expected: `isResolved` is `true`. Do not resolve unrelated findings.

- [ ] **Step 7: Persist tracker state**

Add a Beads comment to `flpdf-15jp.4` with the pushed commit, verification, and thread URL, then run:

```bash
bd dolt push
```

Keep `flpdf-15jp` and all four children open because the remaining whole-stack
blockers are not addressed here.

# ObjectHandle-native crypt-filter selector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port qpdf 11.9.0's `QPDF::interpretCF` boundary as an ObjectHandle-native, error-propagating crypt-filter selector without migrating a production consumer.

**Architecture:** Keep one private value-selection core over `Option<&[u8]>`. The existing materialized `Object` adapter and a new `ObjectHandle` adapter both call that core; only the handle adapter owns lazy resolution and its errors.

**Tech Stack:** Rust 2021, `ObjectHandle`, `EncryptionState`, `BTreeMap`, pinned qpdf 11.9.0 source, Cargo unit/workspace tests, Clippy, `cargo llvm-cov`, and Beads.

## Global Constraints

- Read `.claude/rules/qpdf-port-design-patterns.md` completely before changing code.
- Pinned qpdf 11.9.0 source is the semantic oracle; resolve it with `scripts/fetch-qpdf-source.sh --print-path` and keep it read-only.
- Preserve qpdf's exact branch order: `crypt_filters` lookup, built-in `Identity`, then `Unknown`; every non-name is `Identity`.
- Lazy resolution and resolver errors belong only to the ObjectHandle adapter and must never be converted to a selection sentinel.
- Do not modify `decryptStream`, warnings, key derivation, AES/RC4 stages, legacy resolve-time decryption, or any production consumer.
- Do not commit or push Git changes. The design, plan, source, tests, and docs remain as worktree changes per the user's instruction.
- Every production change follows RED -> GREEN and every changed executable line must have fresh 100% coverage.

## File map

- Modify `crates/flpdf/src/reader.rs`: add selector tests, the shared name-selection core, and the ObjectHandle-native entry point. Keep all crypt-filter selection responsibility in this existing module.
- Modify `docs/qpdf-correspondence.md`: record the declaration/implementation line mapping and ObjectHandle-native contract.
- No other production or test file changes are allowed. The already-approved design and this plan live under `docs/superpowers/` and are documentation-only.

---

### Task 1: Add and verify the ObjectHandle-native selector

**Files:**
- Modify: `crates/flpdf/src/reader.rs:4121-4144`
- Test: `crates/flpdf/src/reader.rs:4588-4642`

**Interfaces:**
- Consumes: `ObjectHandle::try_as_name(&self) -> Result<Option<Vec<u8>>>`, `EncryptionState::crypt_filters`, and the existing `interpret_cf(&BTreeMap<Vec<u8>, EncryptionMode>, Option<&Object>) -> EncryptionMode` caller boundary.
- Produces: `pub(in crate::reader) fn interpret_cf_from_handle(encryption: &EncryptionState, cf: &ObjectHandle) -> Result<EncryptionMode>` and one private `interpret_cf_name` value core.

- [x] **Step 1: Reconfirm the pinned oracle immediately before editing**

Run:

```bash
qpdf_source="$(scripts/fetch-qpdf-source.sh --print-path)"
sed -n '1122,1127p' "$qpdf_source/include/qpdf/QPDF.hh"
sed -n '700,716p' "$qpdf_source/libqpdf/QPDF_encryption.cc"
git -C "$qpdf_source" status --short
```

Expected: the declaration takes `QPDFObjectHandle`; the implementation checks the map before `/Identity`, returns `e_unknown` for unmatched names and `e_none` for non-names; source status is empty.

- [x] **Step 2: Add direct, indirect, equivalence, and error-path tests before production code**

Add these tests beside the existing `interpret_cf_*` tests. They reuse the sealed resolver harness already exported by `object_handle::identity_tests`:

```rust
#[test]
fn interpret_cf_from_handle_matches_the_materialized_selector_and_qpdf_order() {
    let mut encryption = explicit_rc4_encryption_state();
    encryption
        .crypt_filters
        .insert(b"Identity".to_vec(), EncryptionMode::Aes128);

    let cases = [
        (
            Object::Name(b"StdCF".to_vec()),
            ObjectHandle::name(b"StdCF".to_vec()),
            EncryptionMode::Rc4,
        ),
        (
            Object::Name(b"Identity".to_vec()),
            ObjectHandle::name(b"Identity".to_vec()),
            EncryptionMode::Aes128,
        ),
        (
            Object::Name(b"NoSuchCF".to_vec()),
            ObjectHandle::name(b"NoSuchCF".to_vec()),
            EncryptionMode::Unknown,
        ),
        (
            Object::Integer(7),
            ObjectHandle::integer(7),
            EncryptionMode::Identity,
        ),
        (
            Object::Null,
            ObjectHandle::null(),
            EncryptionMode::Identity,
        ),
    ];

    for (object, handle, expected) in cases {
        assert_eq!(
            interpret_cf(&encryption.crypt_filters, Some(&object)),
            expected
        );
        assert_eq!(
            interpret_cf_from_handle(&encryption, &handle).unwrap(),
            expected
        );
    }

    let builtin_identity = explicit_rc4_encryption_state();
    assert_eq!(
        interpret_cf_from_handle(
            &builtin_identity,
            &ObjectHandle::name(b"Identity".to_vec()),
        )
        .unwrap(),
        EncryptionMode::Identity
    );
}

#[test]
fn interpret_cf_from_handle_lazily_resolves_an_indirect_name() {
    let encryption = explicit_rc4_encryption_state();
    let (handle, _resolver) =
        crate::object_handle::identity_tests::resolver_bearing_handle(ObjectValue::Name(
            b"StdCF".to_vec(),
        ));

    assert!(!handle.is_resolved());
    assert_eq!(
        interpret_cf_from_handle(&encryption, &handle).unwrap(),
        EncryptionMode::Rc4
    );
    assert!(handle.is_resolved());
}

#[test]
fn interpret_cf_from_handle_propagates_resolution_failures() {
    let encryption = explicit_rc4_encryption_state();

    let (dropped, resolver) =
        crate::object_handle::identity_tests::resolver_bearing_handle(ObjectValue::Name(
            b"StdCF".to_vec(),
        ));
    drop(resolver);
    assert_eq!(
        interpret_cf_from_handle(&encryption, &dropped)
            .unwrap_err()
            .to_string(),
        "object 20 0 belongs to a dropped PDF"
    );

    let (failing, _resolver) =
        crate::object_handle::identity_tests::error_resolving_handle(ObjectRef::new(21, 0));
    assert_eq!(
        interpret_cf_from_handle(&encryption, &failing)
            .unwrap_err()
            .to_string(),
        "resolver failed"
    );
}
```

- [x] **Step 3: Run the focused tests and capture the RED state**

Run:

```bash
cargo test -p flpdf --lib interpret_cf_from_handle -- --nocapture
```

Expected: compilation fails with `cannot find function interpret_cf_from_handle in this scope`. A failure caused by an existing API assumption instead must be corrected in the tests before production code is added.

- [x] **Step 4: Add the minimal shared core and handle adapter**

Replace the body-only responsibility currently embedded in `interpret_cf` with this shared core and adapter:

```rust
fn interpret_cf_name(
    crypt_filters: &BTreeMap<Vec<u8>, EncryptionMode>,
    filter: Option<&[u8]>,
) -> EncryptionMode {
    let Some(filter) = filter else {
        return EncryptionMode::Identity;
    };
    if let Some(mode) = crypt_filters.get(filter) {
        return *mode;
    }
    if filter == b"Identity" {
        return EncryptionMode::Identity;
    }
    EncryptionMode::Unknown
}

fn interpret_cf(
    crypt_filters: &BTreeMap<Vec<u8>, EncryptionMode>,
    cf: Option<&Object>,
) -> EncryptionMode {
    interpret_cf_name(crypt_filters, cf.and_then(Object::as_name))
}

/// qpdf `QPDF::interpretCF`'s ObjectHandle boundary
/// (`include/qpdf/QPDF.hh:1122-1127`,
/// `libqpdf/QPDF_encryption.cc:700-716`).
#[allow(dead_code)] // production consumer cutover belongs to flpdf-25kg.3.12
pub(in crate::reader) fn interpret_cf_from_handle(
    encryption: &EncryptionState,
    cf: &ObjectHandle,
) -> Result<EncryptionMode> {
    let filter = cf.try_as_name()?;
    Ok(interpret_cf_name(
        &encryption.crypt_filters,
        filter.as_deref(),
    ))
}
```

Keep the existing `interpret_cf` qpdf documentation, augmenting it only where necessary to state that it is the materialized adapter. Do not change its callers.

- [x] **Step 5: Run the focused tests and existing selector tests in GREEN**

Run:

```bash
cargo test -p flpdf --lib interpret_cf_from_handle -- --nocapture
cargo test -p flpdf --lib interpret_cf_ -- --nocapture
```

Expected: all new handle tests pass, and the pre-existing materialized selector tests remain green.

- [x] **Step 6: Inspect the production diff for scope and branch order**

Run:

```bash
git diff -- crates/flpdf/src/reader.rs
rg -n "interpret_cf_name|interpret_cf_from_handle|decrypt_stream|warning" crates/flpdf/src/reader.rs
```

Expected: the production diff contains only the shared value core and the new adapter; no stream consumer, warning, key derivation, or cipher-stage code changes.

---

### Task 2: Record correspondence and run all quality gates

**Files:**
- Modify: `docs/qpdf-correspondence.md:134-177`
- Verify: `crates/flpdf/src/reader.rs`

**Interfaces:**
- Consumes: the verified `interpret_cf_from_handle` contract from Task 1 and pinned qpdf line evidence.
- Produces: durable correspondence documentation, full test/coverage evidence, and an append-only Beads implementation note while leaving the issue and Git branch uncommitted.

- [x] **Step 1: Add the qpdf correspondence row**

Add a row adjacent to the existing `EncryptionParameters` entry:

```markdown
| `QPDF::interpretCF` (`QPDF.hh`; `QPDF_encryption.cc`) | `1122-1127`; `700-716` | `reader.rs` の `interpret_cf_name` / `interpret_cf` / `interpret_cf_from_handle` | ✅ 値選択を共有し、ObjectHandle 版は `try_as_name` で lazy resolve。`crypt_filters` → built-in `/Identity` → `e_unknown`、non-name → `e_none` の順と resolver error 伝播を維持。production consumer cutover は `flpdf-25kg.3.12` |
```

Do not change the status of the broad `QPDF_encryption.cc` row; this issue completes only `interpretCF`, not the whole file.

- [x] **Step 2: Format and run focused/static checks**

Run:

```bash
cargo fmt --all
cargo fmt --all -- --check
python3 scripts/qpdf-module-docs.py --check
cargo test -p flpdf --lib interpret_cf_ -- --nocapture
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: all commands exit 0 with no formatting, correspondence-metadata, test, or Clippy error.

- [x] **Step 3: Run crate and workspace regression suites**

Run:

```bash
cargo test -p flpdf
cargo test --workspace
```

Expected: both commands exit 0. Any pre-existing or environment-dependent failure must be recorded exactly and not described as a pass.

- [x] **Step 4: Generate fresh coverage for the dirty, intentionally uncommitted tree**

Run:

```bash
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/flpdf-25kg-3-15.lcov
git diff --unified=0 --no-color HEAD -- crates/flpdf/src/reader.rs |
  perl -ne 'if (/^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/) {$line=$1; next} next if /^\+\+\+/; if (/^\+/) {print "$line\n"; ++$line; next} next if /^-/; ++$line if defined $line && /^ /' \
  > /tmp/flpdf-25kg-3-15-added-lines.txt
awk -F'[:,]' '
  NR == FNR { added[$1] = 1; next }
  /^SF:.*\/crates\/flpdf\/src\/reader.rs$/ { inside = 1; next }
  /^end_of_record$/ { inside = 0 }
  inside && /^DA:/ && ($2 in added) {
    executable++
    if ($3 == 0) { uncovered++; lines = lines " " $2 }
  }
  END {
    printf "reader.rs changed executable lines: %d; uncovered: %d%s\n", executable, uncovered, lines
    exit(uncovered != 0)
  }
' /tmp/flpdf-25kg-3-15-added-lines.txt target/flpdf-25kg-3-15.lcov
```

Expected: fresh instrumented tests complete and the final line reports at least one changed executable line with `uncovered: 0`. The repository's normal `scripts/patch-coverage.sh` is not used here because it deliberately diffs committed `HEAD`; the user explicitly prohibited committing, so it would falsely report no changed lines.

- [x] **Step 5: Perform the final scope and worktree checks**

Run:

```bash
git diff --check
git status --short
git diff --name-only HEAD
git diff -- crates/flpdf/src/reader.rs docs/qpdf-correspondence.md
```

Expected: no whitespace errors; changed implementation files are only `crates/flpdf/src/reader.rs` and `docs/qpdf-correspondence.md`, plus the untracked approved design and plan documents. There is no commit and no production consumer cutover.

- [x] **Step 6: Append verification evidence to Beads without closing the issue**

Run after every listed check succeeds:

```bash
bd note flpdf-25kg.3.15 "Implementation verified in isolated worktree: ObjectHandle-native interpretCF selector added with lazy name resolution and error propagation; focused, flpdf, workspace, Clippy, module-doc, and fresh changed-line coverage checks passed; Git changes intentionally remain uncommitted at user request."
bd show flpdf-25kg.3.15
bd dolt push
```

Expected: the new note is present, the issue remains `IN_PROGRESS` because its implementation is not committed, and Beads persistence succeeds. Do not run `git commit`, `git push`, or `bd close`.

# ResolverCore encryption-parameters (m->encp) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Give `ResolverCore` a shared, mutable-in-place handle to the
document's encryption parameters (qpdf `m->encp`), reachable from
`ResolverHandle` for a future pipe-time decryption primitive
(flpdf-25kg.3.10), with `Pdf::encryption` migrated to the same cell so there
is exactly one storage location — matching qpdf's single `shared_ptr<EncryptionParameters>`.

**Architecture:** `ResolverCore<R>` gains
`encryption_parameters: Rc<RefCell<Option<EncryptionState>>>`, constructed
empty in `ResolverHandle::new_shared` and exposed via a new
`ResolverHandle::encryption_parameters(&self) -> Rc<RefCell<Option<EncryptionState>>>`
accessor (clone-the-Rc-out, short borrow). `Pdf::encryption`'s field type
changes from `Option<EncryptionState>` to the same `Rc<RefCell<..>>`, cloned
from the resolver at construction time, so `Pdf` and `ResolverCore` hold two
handles to one allocation — no mirrored writes. Every existing
`self.encryption` read site in `reader.rs` gets a `.borrow()` inserted;
`encryption_file_key`'s return type changes from `Option<&[u8]>` to
`Option<Vec<u8>>` since a borrow can no longer outlive the accessor call.
Nothing consumes the parameters yet (no output-byte change) — this is
scaffolding for flpdf-25kg.3.10.

**Tech Stack:** Rust, `std::rc::Rc` / `std::cell::RefCell` (already used
throughout `reader/resolver.rs` for the same borrow-discipline reasons).

**Design reference:** the full qpdf citation set and field-mapping table are
saved in the beads issue's design field — `bd show flpdf-25kg.3.11`. Read it
before starting; this plan assumes it.

---

## Before you start

Read `crates/flpdf/src/reader/resolver.rs:95-206` (the `ResolverCore` struct
doc and field list) and `crates/flpdf/src/reader.rs:77-206` (the `Pdf` struct
and `EncryptionState`) in full. The borrow-discipline convention this module
already follows — take a `RefCell` borrow, extract what's needed, drop it
within one expression, never hold it across a call that could re-enter — is
not optional style here; it is what keeps resolution's re-entrancy safe (see
`resolver.rs`'s module doc, lines 21-85). Follow it for every new accessor.

All file:line references below are against the worktree HEAD
(`c61592ee`, `feature/flpdf-25kg.3.11-resolver-encp`). If a prior task shifted
lines, re-`grep` rather than trusting the numbers blindly.

---

### Task 1: `ResolverCore` carries the encryption parameters; `ResolverHandle` exposes them

**Files:**
- Modify: `crates/flpdf/src/reader/resolver.rs:104-115` (doc rewrite),
  `:116-200` (struct field), `:412-434` (`new_shared`), add a new accessor
  near the other `ResolverHandle` accessors (e.g. after `header_offset`,
  around line 576-578).
- Test: `crates/flpdf/src/reader/resolver.rs` (`#[cfg(test)] mod tests`,
  starting line 1561).

**Step 1: Write the failing test — AC6 case 4 ("parameters absent")**

Add near the top of `mod tests` (after `minimal_pdf_bytes`, before the first
`#[test]`), a small helper that builds a bare resolver directly — the same
constructor `Pdf::open_with_repair_mode` uses, called with no `Pdf` involved
at all, so the test observes the state *before* any authentication step:

```rust
    use super::ResolverHandle;
    use crate::{Diagnostics, XrefEntry};
    use std::collections::BTreeMap;
    use std::io::Cursor;

    /// A resolver built directly, bypassing `Pdf::open` entirely — the state
    /// qpdf's `Members::encp(new EncryptionParameters)` is in immediately
    /// after `QPDF` construction, before `initializeEncryption()` has run.
    fn bare_resolver() -> std::rc::Rc<ResolverHandle<Cursor<Vec<u8>>>> {
        ResolverHandle::new_shared(
            Cursor::new(minimal_pdf_bytes()),
            0,
            BTreeMap::<ObjectRef, XrefEntry>::new(),
            false,
            Diagnostics::default(),
            0,
        )
    }

    /// AC6 case 4: a resolver on which no authentication step has run at all
    /// reports no encryption parameters. This is qpdf's
    /// `encryption_initialized == false` state — pinned separately from
    /// `an_unencrypted_document_reports_no_encryption_parameters` (which runs
    /// full `Pdf::open` and reaches `encryption_initialized == true,
    /// encrypted == false`), because both collapse to the same `None` through
    /// flpdf's `Option<EncryptionState>` and this asserts that collapse holds
    /// for the pipe-side accessor too.
    #[test]
    fn a_resolver_with_no_authentication_attempted_reports_no_encryption_parameters() {
        let resolver = bare_resolver();
        assert!(resolver.encryption_parameters().borrow().is_none());
    }
```

Check `Diagnostics` implements `Default` (used elsewhere in this file's
construction path — `loaded.repair_diagnostics` is a `Diagnostics`; confirm
with `grep -n "struct Diagnostics" -A5 crates/flpdf/src/lib.rs` or wherever
it's defined, and adjust to whatever the existing zero-value constructor is
if it's not literally `Default`).

**Step 2: Run test to verify it fails**

Run: `cd .worktrees/flpdf-25kg-3-11-resolver-encp && cargo test -p flpdf --lib reader::resolver::tests::a_resolver_with_no_authentication_attempted_reports_no_encryption_parameters`
Expected: compile error — `encryption_parameters` is not a method on `ResolverHandle`.

**Step 3: Implement the field and accessor**

In `resolver.rs`, add the field to `ResolverCore` (after `repair_diagnostics`,
before the closing `}` at line 200):

```rust
    /// qpdf `m->encp` (`include/qpdf/QPDF.hh:1463`), the encryption
    /// parameters `QPDF::pipeStreamData`'s static overload takes as its first
    /// argument and consults before piping a stream
    /// (`libqpdf/QPDF.cc:2477-2492`) — the primitive flpdf-25kg.3.10 adds.
    ///
    /// `Rc<RefCell<..>>` rather than a bare `Option<EncryptionState>`,
    /// because qpdf's `m->encp` is a `std::shared_ptr<EncryptionParameters>`:
    /// a second owner (`ForeignStreamData::encp`, `QPDF.hh:939`) holds the
    /// *same* allocation, constructed from `QPDF::Members::encp` by copying
    /// the shared_ptr (`QPDF.cc:2266`), not by copying the data. `Pdf`
    /// mirrors that shape: it holds a clone of this same `Rc`, so its one
    /// write (`Pdf::authenticate_if_encrypted`) is visible here without a
    /// second write site.
    ///
    /// Constructed empty (`None`) in [`ResolverHandle::new_shared`] and
    /// populated later, matching `Members::encp(new EncryptionParameters)`
    /// (`QPDF.cc:201`): default-constructed at document-construction time,
    /// `encrypted = false` until `initializeEncryption()` runs. flpdf has no
    /// separate `encrypted`/`encryption_initialized` pair — the outer
    /// `Option` serves both; see the field-mapping table in this issue's
    /// design (`bd show flpdf-25kg.3.11`) for the disclosed collapse.
    encryption_parameters: Rc<RefCell<Option<crate::reader::EncryptionState>>>,
```

Rewrite the "one member deliberately missing" doc block at lines 104-115 —
delete the paragraph explaining `encp`'s absence and its stated reason
(wrong twice over: it said the field would arrive with *string* decryption;
it is arriving now for pipe-time *stream* decryption, and resolve-time
decryption gains no new consumer here). Replace with a short note that the
field is present and names its consumer:

```rust
/// One member is present for a consumer this slice does not yet have: the
/// encryption parameters (qpdf `m->encp`), added in flpdf-25kg.3.11 so
/// flpdf-25kg.3.10's pipe-time stream decryption has something to read.
/// Resolve-time *string* decryption is unrelated and unchanged — qpdf
/// decrypts strings during `readObjectAtOffset`'s parse
/// (`StringDecrypter`, `libqpdf/QPDF.cc:1337-1339`) but streams only at pipe
/// time (`decryptStream`, `QPDF.cc:2491`); wiring the string decrypter in is
/// still flpdf-25kg.3.5 AC2. See [`ResolverCore::encryption_parameters`].
```

In `new_shared` (line ~412-434), initialize the field inside the
`ResolverCore { .. }` literal:

```rust
                encryption_parameters: Rc::new(RefCell::new(None)),
```

Add the accessor to `impl<R: Read + Seek> ResolverHandle<R>` (near
`header_offset`, since it has the same "one short borrow, clone out" shape):

```rust
    /// This document's encryption parameters, in their shared, mutable-in-
    /// place form — the pipe-side door onto [`ResolverCore::encryption_parameters`].
    ///
    /// Clones the `Rc` under a single short borrow, matching every other
    /// accessor here: nothing is held once this returns, so a caller that
    /// then does I/O through `self` cannot double-borrow.
    pub(crate) fn encryption_parameters(
        &self,
    ) -> Rc<RefCell<Option<crate::reader::EncryptionState>>> {
        self.core.borrow().encryption_parameters.clone()
    }
```

`EncryptionState` is currently private to `reader.rs`; it does not need a
visibility bump — `reader::resolver` is a descendant module of `reader` and
can already see `reader`'s private items. Confirm this compiles as-is before
assuming otherwise.

**Step 4: Run test to verify it passes**

Run: `cargo test -p flpdf --lib reader::resolver::tests::a_resolver_with_no_authentication_attempted_reports_no_encryption_parameters`
Expected: PASS.

**Step 5: Full-module regression check**

Run: `cargo test -p flpdf --lib reader::` — expect the same 184 tests as the
baseline, now 185 (the new one), all passing. `cargo build -p flpdf` and
`cargo build -p flpdf-cli` must still succeed (nothing outside `resolver.rs`
references the new field yet, so this should be a no-op check).

**Step 6: Commit**

```bash
git add crates/flpdf/src/reader/resolver.rs
git commit -m "feat(resolver): carry qpdf's m->encp as a shared cell on ResolverCore"
```

---

### Task 2: Migrate `Pdf::encryption` onto the same shared cell

**Files:**
- Modify: `crates/flpdf/src/reader.rs:182` (field decl), `:690-722`
  (`open_with_repair_mode`), `:921` (the one write site), `:7763` (the one
  test-injection site), plus every read site listed below.

**Step 1: Change the field type and construction**

`reader.rs:182`:
```rust
    encryption: Rc<RefCell<Option<EncryptionState>>>,
```

Add `use std::cell::RefCell;` and `use std::rc::Rc;` to `reader.rs`'s imports
if not already present (`grep -n "^use std::" crates/flpdf/src/reader.rs`
first — `resolver.rs` imports these but `reader.rs` may not).

`reader.rs:690-722` (`open_with_repair_mode`): hoist the resolver's
encryption cell out before the struct literal, same pattern already used for
`unique_id`:

```rust
        let resolver = ResolverHandle::new_shared(
            reader,
            loaded_state.header_offset,
            source_xref_entries,
            options.repair,
            loaded.repair_diagnostics,
            unique_id,
        );
        let encryption = resolver.encryption_parameters();
        let mut pdf = Self {
            unique_id,
            resolver,
            version: loaded.version,
            ...
            encryption,
        };
```

(Keep every other field in the literal as-is; only `resolver:` becomes a
bare `resolver` shorthand referencing the hoisted local, and the trailing
`encryption: None,` becomes `encryption,`.)

**Step 2: Fix the one write site**

`reader.rs:921`:
```rust
        *self.encryption.borrow_mut() = Some(EncryptionState {
            file_key,
            stream_mode,
            string_mode,
            crypt_filters,
            encrypt_metadata,
            encrypt_ref,
            weak_crypto,
            permissions,
            user_password_matched,
            owner_password_matched,
        });
```

**Step 3: Fix every read site**

Single-expression sites — insert `.borrow()` before `.as_ref()` /
`.is_some()` / `.is_none()` (works because `Ref<Option<T>>` derefs to
`&Option<T>`, so `.as_ref()`/`.is_some()`/`.is_none()` resolve through the
guard via auto-deref inside one expression — no lifetime issue since the
temporary `Ref` lives for the whole expression):

| Line | Before | After |
| --- | --- | --- |
| `515` | `self.encryption.is_some()` | `self.encryption.borrow().is_some()` |
| `519-521` | `self.encryption.as_ref().and_then(...)` | `self.encryption.borrow().as_ref().and_then(...)` |
| `526-528` | `self.encryption.as_ref().is_some_and(...)` | `self.encryption.borrow().as_ref().is_some_and(...)` |
| `533-535` | `self.encryption.as_ref().map(\|e\| e.permissions)` | `self.encryption.borrow().as_ref().map(\|e\| e.permissions)` |
| `541-543` | same shape (`user_password_matched`) | add `.borrow()` |
| `551-553` | same shape (`owner_password_matched`) | add `.borrow()` |
| `586` | `self.encryption.is_none()` | `self.encryption.borrow().is_none()` |

`encryption_file_key` (`561-565`) cannot keep returning `Option<&[u8]>` — the
borrow cannot outlive the `Ref` guard once the store is behind a `RefCell`,
and this crate forbids `unsafe`. Change the signature and body:

```rust
    /// The derived file encryption key, if the document was opened as an
    /// encrypted file. `None` for plaintext PDFs.
    ///
    /// Read-only accessor for the `show-encryption-key` inspection
    /// subcommand; does not run or alter authentication. Returns an owned
    /// copy — this is an inspection accessor, not a hot path, and the
    /// underlying storage is now shared (see [`ResolverCore::encryption_parameters`]).
    pub fn encryption_file_key(&self) -> Option<Vec<u8>> {
        self.encryption
            .borrow()
            .as_ref()
            .map(|encryption| encryption.file_key.clone())
    }
```

Multi-statement sites — bind the `Ref` to a named local so it survives the
whole block, rather than relying on `if let`/`let-else` temporary extension
through a method-call chain:

`reader.rs:607-610` (inside `encryption_info`, used through line 629):
```rust
        let encryption_guard = self.encryption.borrow();
        let encryption = encryption_guard
            .as_ref()
            .expect("checked is_some above; authenticate_if_encrypted set it");
```
(`self.encryption.is_none()` at line 586 also needs its `.borrow()`, done
above — confirm that check and this guard don't conflict: both are
immutable borrows, which `RefCell` allows concurrently.)

`reader.rs:2006-2008`:
```rust
        if native_parsed {
            let encryption_guard = self.encryption.borrow();
            if let Some(encryption) = encryption_guard.as_ref() {
                decrypt_object_value_strings(object_ref, &mut value, encryption)?;
            }
        }
```

`reader.rs:2888-2890` (`decrypt_resolved_object`, used through line 2916):
```rust
        let encryption_guard = self.encryption.borrow();
        let Some(encryption) = encryption_guard.as_ref() else {
            return Ok((object, false));
        };
```

**Step 4: Fix the test-injection site**

`reader.rs:7763`:
```rust
        *pdf.encryption.borrow_mut() = Some(EncryptionState {
            stream_mode: EncryptionMode::Rc4,
            ..explicit_rc4_encryption_state()
        });
```

**Step 5: Fix `flpdf-cli`'s two callers**

`crates/flpdf-cli/src/main.rs:2556-2562` — `donor.encryption_file_key()` now
returns `Option<Vec<u8>>`; drop the trailing `.to_vec()`:
```rust
    let file_key: Vec<u8> = donor
        .encryption_file_key()
        .ok_or_else(|| { .. })?;
```

`crates/flpdf-cli/src/main.rs:5102-5107` — `match pdf.encryption_file_key()`
binds `key: Vec<u8>` now instead of `&[u8]`; check what `hex_lower` expects
(`grep -n "fn hex_lower" crates/flpdf-cli/src/main.rs`) and pass `&key` if it
takes a slice, or `key` unchanged if it already takes `impl AsRef<[u8]>` /
`Vec<u8>`. Do not change `hex_lower` itself.

**Step 6: Build and run the full existing suite**

Run: `cargo build -p flpdf -p flpdf-cli 2>&1 | tail -50` — fix any remaining
call sites the compiler surfaces (this table is not guaranteed exhaustive;
trust `cargo build`'s errors over the table if they disagree).

Run: `cargo test -p flpdf --lib reader:: && cargo test -p flpdf --lib`
Expected: same pass count as Task 1's baseline (no regressions — this task
changes storage mechanics only, not behavior).

**Step 7: Commit**

```bash
git add crates/flpdf/src/reader.rs crates/flpdf-cli/src/main.rs
git commit -m "refactor(reader): back Pdf::encryption with ResolverCore's shared cell"
```

---

### Task 3: AC6 cases 1-3 — unencrypted / RC4 / AES through the pipe-side accessor

**Files:**
- Test: `crates/flpdf/src/reader/resolver.rs` (`mod tests`).

**Step 1: Write the three failing tests**

Reach the resolver through `pdf.resolver` — `resolver` is private to
`reader`, but `reader::resolver::tests` is a descendant of `reader` and can
see it directly (verify: if this doesn't compile, the fallback is a
`#[cfg(test)] pub(crate) fn resolver(&self) -> &Rc<ResolverHandle<R>>` on
`Pdf` in `reader.rs`, but try the direct-field-access route first — it needs
no new production code).

```rust
    /// AC6 case 1: a document with no `/Encrypt` entry authenticates
    /// (`Pdf::authenticate_if_encrypted` runs and returns early) and reports
    /// no encryption parameters — the `encryption_initialized == true,
    /// encrypted == false` qpdf state, pinned separately from
    /// `a_resolver_with_no_authentication_attempted_reports_no_encryption_parameters`'s
    /// `encryption_initialized == false`, even though both observe `None`.
    #[test]
    fn an_unencrypted_document_reports_no_encryption_parameters() {
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        assert!(pdf.resolver.encryption_parameters().borrow().is_none());
    }

    /// AC6 case 2: an RC4-encrypted document reports its parameters through
    /// the same accessor a pipe-side caller (flpdf-25kg.3.10) will use.
    #[test]
    fn an_rc4_document_reports_its_encryption_parameters() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/tests/fixtures/encrypted/v2-rc4-128-r3.pdf"
        );
        let bytes = std::fs::read(path)
            .expect("encrypted fixture missing: tests/fixtures/encrypted/v2-rc4-128-r3.pdf");
        let options = crate::PdfOpenOptions {
            password: b"user-v2".to_vec(),
            allow_weak_crypto: true,
            ..crate::PdfOpenOptions::default()
        };
        let pdf = Pdf::open_mem_owned_with_options(bytes, options).expect("open RC4 fixture");
        let cell = pdf.resolver.encryption_parameters();
        let guard = cell.borrow();
        let encryption = guard.as_ref().expect("RC4 fixture must authenticate");
        assert_eq!(encryption.stream_mode, crate::reader::EncryptionMode::Rc4);
    }

    /// AC6 case 3: an AES-encrypted document reports its parameters the same
    /// way.
    #[test]
    fn an_aes_document_reports_its_encryption_parameters() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/tests/fixtures/encrypted/v4-aes-128-r4.pdf"
        );
        let bytes = std::fs::read(path)
            .expect("encrypted fixture missing: tests/fixtures/encrypted/v4-aes-128-r4.pdf");
        let options = crate::PdfOpenOptions {
            password: b"user-v4-aes".to_vec(),
            ..crate::PdfOpenOptions::default()
        };
        let pdf = Pdf::open_mem_owned_with_options(bytes, options).expect("open AES fixture");
        let cell = pdf.resolver.encryption_parameters();
        let guard = cell.borrow();
        let encryption = guard.as_ref().expect("AES fixture must authenticate");
        assert_eq!(encryption.stream_mode, crate::reader::EncryptionMode::Aes128);
    }
```

Check `EncryptionMode`'s exact variant names and visibility
(`grep -n "enum EncryptionMode" -A 15 crates/flpdf/src/reader.rs`) before
trusting `Rc4`/`Aes128` verbatim — adjust to whatever the real variants are
named, and confirm the enum derives `PartialEq`/`Debug` for `assert_eq!` (add
if missing — check for existing derives first; this enum likely already
needs them since `reader.rs`'s own tests compare modes).

`Pdf::open_mem_owned_with_options` takes ownership of `bytes: Vec<u8>` per
its signature at `reader.rs:3210` — confirm the exact parameter order and
name before using it.

**Step 2: Run tests to verify they fail (or don't compile) first**

`cargo test` accepts only a single positional `[TESTNAME]` filter
(`cargo test --help`: `Usage: cargo test [OPTIONS] [TESTNAME] [-- [ARGS]...]`)
— passing three names in one invocation makes the second one an unexpected
argument and the command exits before running anything. Run each
individually:

```bash
cargo test -p flpdf --lib reader::resolver::tests::an_unencrypted_document_reports_no_encryption_parameters
cargo test -p flpdf --lib reader::resolver::tests::an_rc4_document_reports_its_encryption_parameters_through_the_shared_cell
cargo test -p flpdf --lib reader::resolver::tests::an_aes_document_reports_its_encryption_parameters_through_the_shared_cell
```

Before Task 1/2 land, `encryption_parameters` doesn't exist — but by this
point in the plan it does, so these should compile immediately. If they pass
immediately, that's expected (Tasks 1-2 already implemented the mechanism);
this task's value is coverage of the three real-fixture paths, not driving
new production code.

**Step 3: Run and confirm pass**

Expected: PASS, all three.

**Step 4: Commit**

```bash
git add crates/flpdf/src/reader/resolver.rs
git commit -m "test(resolver): cover unencrypted/RC4/AES through the encryption-parameters accessor"
```

---

### Task 4: `docs/qpdf-correspondence.md`

**Files:**
- Modify: `docs/qpdf-correspondence.md:133`.

**Step 1: Update the ResolverCore row**

The row's field list currently reads `` `m->file` / `m->xref_table` /
`m->obj_cache` / `m->resolving` / `m->resolved_object_streams` /
`m->attempt_recovery` `` — add `` / `m->encp` `` to that list, and a short
clause noting it now includes the encryption parameters for
flpdf-25kg.3.10's pipe primitive. Keep the row's 🔀 classification (this is
qpdf's own field, ported 1:1 — not a container substitution needing a ⚪
row).

**Step 2: Record the `encrypted`/`encryption_initialized` collapse separately**

The field-list update above covers the `m->encp` *field addition* (a
straight 1:1 port, no ⚪ needed). It does not cover a second, narrower thing:
`ResolverCore::encryption_parameters`'s doc discloses that flpdf's single
`Option<EncryptionState>` stands in for *two* independent qpdf booleans,
`encrypted` and `encryption_initialized` (`EncryptionParameters`,
`QPDF.hh:899-921`) — collapsing "not yet initialized" and "initialized, not
encrypted" into one `None`. That is a structural data-model simplification,
not a plain field port, so per CLAUDE.md's deviation classification (B)
condition 3 and `.claude/rules/qpdf-port-design-patterns.md` rule 5, it needs
its own two-location record: the module doc (already done, Task 1) and a
line in `docs/qpdf-correspondence.md` — right now its only record is the
beads issue's design field, not the repo.

Add a ⚪ row (or an inline note on the existing row 133, whichever this
file's conventions prefer for a sub-field-level disclosure — check a few
neighboring ⚪ rows for the pattern) stating: flpdf's `Option<EncryptionState>`
(`reader.rs:191`) collapses qpdf's `encrypted`/`encryption_initialized` pair;
safe because `encryption_initialized` is only a re-entry guard inside
`initializeEncryption()` (`QPDF_encryption.cc:721`, `:724`), which itself has
exactly one call site per document (`QPDF.cc:471`) — flpdf cannot structurally
hit the re-entry case qpdf guards against, so the collapse changes no
observable behavior. Cite `include/qpdf/QPDF.hh:899-921` for the two-boolean
shape being collapsed.

**Step 3: Commit**

```bash
git add docs/qpdf-correspondence.md
git commit -m "docs: record m->encp in the ResolverCore correspondence row"
```

---

### Task 5: Quality gates

**Files:** none (verification only).

**Step 1: Format and lint**

```bash
cargo fmt --check
cargo clippy -p flpdf -p flpdf-cli --all-targets -- -D warnings
```
Fix anything flagged (expect none if the above tasks followed existing
style).

**Step 2: Full test suite**

```bash
cargo test -p flpdf
cargo test -p flpdf-cli
```
Expected: everything green, no new failures.

**Step 3: Byte-identical corpus**

Per CLAUDE.md, run the qpdf-zlib-compat gated byte-identical tests — this
issue must not change a single output byte (nothing consumes the new
parameters yet). A loose substring filter (`compat_baseline`, `byte_identical`)
is not sufficient here: it can match zero tests and report success without
running anything, silently skipping the real gate. Use the explicit target
list `.github/workflows/ci.yml` actually runs (currently lines 147-197 —
re-`grep -n "qpdf-zlib-compat" .github/workflows/ci.yml` rather than trusting
a cached line range, since CI's target list can grow):

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test zlib_compat_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_null_visibility_tests
cargo test -p flpdf --features qpdf-zlib-compat --test deterministic_id_qpdf_parity_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_generate_objstm_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests
cargo test -p flpdf --features qpdf-zlib-compat --lib overlay::byte_gate
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical_overlay
cargo test -p flpdf-cli --features qpdf-zlib-compat --test compat_baseline_static_id -- --nocapture
cargo test -p flpdf-cli --features qpdf-zlib-compat --test compat_matrix_baseline -- --nocapture
cargo test -p flpdf-qtest-tools --features qpdf-zlib-compat --test e2e
```
Every one of these must report `0 failed` with a nonzero pass count (a
`0 passed` result for any of them means the target name is stale, not that
the gate is satisfied). If nothing changed at the byte level, as expected,
all of these should be unaffected — but confirm by reading the pass counts
rather than assuming from a clean exit code.

**Step 4: Patch coverage**

Commit all work first (coverage measures the working tree; the gate diffs
against HEAD, so an uncommitted tree produces a false read). Then:

```bash
scripts/patch-coverage.sh --base main
```

`flpdf` changed lines must hit 100%. Every new site this plan touches is
exercised by either an existing test (the mechanical `.borrow()` sites, via
Task 2 Step 6's regression run) or a new one (Task 1/3's four AC6 cases).
If anything is genuinely uncovered, add a test — do not `cov:ignore` without
a documented reason in the PR description per CLAUDE.md.

**Step 5: Qualitative check**

Before opening a PR, re-read CLAUDE.md's "質的チェック" note: confirm the
new tests' assertions are substantive (they compare specific `stream_mode`
values and `is_none()`, not just "did not panic").

# DecodeParms Null-Key Consumer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make both flpdf `/DecodeParms` shape readers omit dictionary keys whose values resolve to null, matching qpdf 11.9.0 `QPDF_Dictionary::getKeys`, while preserving filter-specific resolution boundaries and all non-null validation.

**Architecture:** Keep null-aware enumeration at the object-model boundary. The `ObjectHandle` reader delegates consuming filters to `ObjectHandle::try_get_keys` and then fetches retained values through `try_get_key`; the legacy `Object` reader omits direct `Object::Null` entries generically before retained-key reduction. Non-consuming filters keep their present-versus-absent behavior and do not resolve dictionary children. The existing `FilterSpec`, `DecodeParams`, codec, predictor, warning, and pipeline layers remain unchanged.

**Tech Stack:** Rust workspace, qpdf 11.9.0 source and executable oracle, existing qpdf test-driver fixtures/goldens, Cargo tests, cargo-llvm-cov, Beads.

## Global Constraints

- Pinned qpdf 11.9.0 `QPDF_Dictionary::getKeys` (`libqpdf/QPDF_Dictionary.cc:117-127`), `QPDFObjectHandle::getKeys` (`libqpdf/QPDFObjectHandle.cc:997-1009`), `SF_FlateLzwDecode::setDecodeParms` (`libqpdf/SF_FlateLzwDecode.cc:21-72`), and `QPDF_Stream::filterable` (`libqpdf/QPDF_Stream.cc:378-485`) are authoritative.
- Use the prerequisite `ObjectHandle::try_get_keys`; do not duplicate null resolution with a filter-local `try_is_null` loop or key-name special cases.
- A consuming stage must enumerate through `try_get_keys` once for that stage. Apply retained-key reduction only after enumeration has resolved every child and removed nullish keys.
- For a retained handle key, fetch the value through `params.try_get_key(&key)` and classify it with the existing `param_value_from_handle` path.
- Direct null, indirect-to-null, missing/dangling-to-null, and resolver loop-to-null values are omitted. Resolver errors propagate unchanged and are not treated as null.
- The legacy `Object` reader can omit only direct `Object::Null`; it must not follow `Object::Reference`, invent a resolver, or broaden its responsibility.
- Null omission is independent of key name. Unknown null-valued keys are omitted by enumeration just like recognized ones; unknown non-null keys are still touched by a consuming handle reader before retention drops them.
- A present dictionary that becomes empty remains `DecodeParams::Present(Vec::new())`, not `Absent`. This preserves rejection by filters inheriting base `QPDFStreamFilter::setDecodeParms`.
- Non-consuming filters must not resolve `/DecodeParms` dictionary children. Preserve the shared scalar snapshot used by those stages and the live-handle behavior across mixed filter chains.
- Keep non-null invalid parameters unchanged: a name-valued `/Predictor`, out-of-range predictor, invalid geometry, and invalid `/EarlyChange` remain unfilterable.
- Do not change `ObjectHandle::try_get_keys`, `try_is_null`, `try_get_key`, dictionary storage, retained-key policy, `ParamValue`, `FlateLzwStreamFilter::set_decode_params`, codec/predictor pipelines, warning semantics, or writer dictionary preservation.
- Existing oracle inputs and goldens at `tests/fixtures/test_driver/stream_decode_parms_{direct,indirect}_null.{pdf,out}` already satisfy the issue's probe/golden requirement. Verify them; do not re-bless or regenerate `.out` files unless a live pinned-oracle mismatch is first demonstrated and separately reviewed.
- Preserve unrelated worktrees and the main checkout's untracked files. All implementation work stays in `/home/ubuntu/flpdf/.worktrees/flpdf-h8mv-decodeparms-null-keys` on `feature/flpdf-h8mv-decodeparms-null-keys`.
- Follow RED→GREEN: commit tests only after seeing the expected failure, implement the smallest consumer change, and run focused tests before wider gates.
- Require formatting, clippy, full workspace tests, the pinned qpdf differential, and fresh 100% changed executable-line coverage before closing the Bead.

---

### Task 1: Pin the oracle contract and add failing behavior tests

**Files:**
- Modify: `crates/flpdf/src/stream_filter.rs:1708-1758, 2051-2200, 2635-3065` (unit tests and stale divergence comments)
- Modify: `crates/flpdf/src/filters.rs:2635-2676, 4054-4070` (public API tests and corpus contract comments)
- Verify unchanged: `tests/fixtures/test_driver/stream_decode_parms_direct_null.pdf`
- Verify unchanged: `tests/fixtures/test_driver/stream_decode_parms_direct_null.out`
- Verify unchanged: `tests/fixtures/test_driver/stream_decode_parms_indirect_null.pdf`
- Verify unchanged: `tests/fixtures/test_driver/stream_decode_parms_indirect_null.out`

**Interfaces:**
- Exercises: `decode_filter_specs_from_object`, `decode_filter_specs_from_handle`, `decode_stream_data`, `encode_stream_data`.
- Reuses: `params`, `ObjectHandle::new_indirect_unresolved`, `ObjectHandle::set_missing`, `resolver_bearing_handle`, `logged_resolver_bearing_handle`, and existing dropped-resolver helpers.
- Produces: absolute RED tests for legacy direct-null behavior, handle direct/indirect/missing-null behavior, resolver-error propagation, and public decode/encode behavior.

- [ ] **Step 1: Reconfirm source responsibility and the committed live-oracle contract**

  Run:

  ```bash
  qpdf_source=$(scripts/fetch-qpdf-source.sh --print-path)
  sed -n '117,127p' "$qpdf_source/libqpdf/QPDF_Dictionary.cc"
  sed -n '352,356p;997,1009p;2375,2382p' "$qpdf_source/libqpdf/QPDFObjectHandle.cc"
  sed -n '21,72p' "$qpdf_source/libqpdf/SF_FlateLzwDecode.cc"
  sed -n '378,485p' "$qpdf_source/libqpdf/QPDF_Stream.cc"
  qpdf --version
  test "$(qpdf --show-object=6 --filtered-stream-data tests/fixtures/test_driver/stream_decode_parms_direct_null.pdf)" = abc
  test "$(qpdf --show-object=6 --filtered-stream-data tests/fixtures/test_driver/stream_decode_parms_indirect_null.pdf)" = abc
  bash tests/fixtures/test_driver/generate.sh --check
  git diff --exit-code -- tests/fixtures/test_driver
  ```

  Expected: source shows `getKeys()` testing every value with `isNull()`, qpdf reports 11.9.0, both fixture probes produce exactly `abc` with exit 0, fixture generation is byte-stable, and no oracle file changes.

- [ ] **Step 2: Split the public non-integer control from the new null success case**

  In `crates/flpdf/src/filters.rs`, replace the current loop in `non_integer_decode_params_values_remain_unfilterable` with two absolute tests. Keep the name-valued control rejected:

  ```rust
  #[test]
  fn non_null_non_integer_decode_params_values_remain_unfilterable() {
      let mut parms = Dictionary::new();
      parms.insert("Predictor", Object::Name(b"12".to_vec()));
      let mut dict = Dictionary::new();
      dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
      dict.insert("DecodeParms", Object::Dictionary(parms));

      let expected =
          "unsupported PDF feature: stream filter FlateDecode does not support supplied /DecodeParms";
      assert_eq!(
          decode_stream_data(&dict, b"not deflate data")
              .unwrap_err()
              .to_string(),
          expected
      );
      assert_eq!(
          encode_stream_data(&dict, b"data").unwrap_err().to_string(),
          expected
      );
  }
  ```

  Add the new public contract:

  ```rust
  #[test]
  fn null_decode_params_values_are_omitted_before_decode_and_encode() {
      let mut parms = Dictionary::new();
      parms.insert("Predictor", Object::Null);
      let mut dict = Dictionary::new();
      dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
      dict.insert("DecodeParms", Object::Dictionary(parms));

      let encoded = flate_only(b"abc");
      assert_eq!(decode_stream_data(&dict, &encoded).unwrap(), b"abc");

      let reencoded = encode_stream_data(&dict, b"abc").unwrap();
      assert_eq!(decode_stream_data(&dict, &reencoded).unwrap(), b"abc");
  }
  ```

  Do not weaken the out-of-range predictor and invalid-geometry tests beside these cases.

- [ ] **Step 3: Add absolute reader tests for generic null omission**

  In `crates/flpdf/src/stream_filter.rs`, update `object_shape_reader_reduces_each_parameter_value_to_its_bounded_shape` so `/Predictor null` is absent from the expected entries. Add a focused legacy test with one retained non-null control:

  ```rust
  #[test]
  fn object_shape_reader_omits_null_valued_keys_before_retention() {
      let specs = decode_filter_specs_from_object(
          Some(&Object::Name(b"FlateDecode".to_vec())),
          Some(&params(&[
              ("Columns", Object::Integer(4)),
              ("Predictor", Object::Null),
              ("Unused", Object::Null),
          ])),
          None,
      )
      .unwrap();

      assert_eq!(
          specs[0].decode_params,
          DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Int(4))])
      );
  }
  ```

  Add a handle test that distinguishes all three nullish child routes while retaining one live integer:

  ```rust
  #[test]
  fn handle_reader_omits_direct_indirect_and_missing_null_valued_keys() {
      let (indirect_null, _resolver) = resolver_bearing_handle(ObjectValue::Null);
      let missing = ObjectHandle::new_indirect_unresolved(ObjectRef::new(21, 0), -1);
      missing.set_missing();
      let parms = ObjectHandle::dictionary(vec![
          (b"Columns".to_vec(), ObjectHandle::integer(4)),
          (b"Predictor".to_vec(), ObjectHandle::null()),
          (b"Colors".to_vec(), indirect_null.clone()),
          (b"BitsPerComponent".to_vec(), missing.clone()),
      ]);

      let specs = decode_filter_specs_from_handle(
          &ObjectHandle::name(b"FlateDecode".to_vec()),
          &parms,
          None,
      )
      .unwrap();

      assert!(indirect_null.is_resolved());
      assert!(missing.is_resolved());
      assert_eq!(
          specs[0].decode_params,
          DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Int(4))])
      );
  }
  ```

  Keep the corpus row `"null-valued /DecodeParms key (flpdf-h8mv)"` for both-reader coverage, but rewrite its comment to state that the row now pins the qpdf-compatible success path rather than a known divergence.

- [ ] **Step 4: Add the getKeys-before-retention error control**

  Add a consuming-handle test with a dropped resolver under an unretained key:

  ```rust
  #[test]
  fn handle_reader_propagates_get_keys_errors_before_retention() {
      let dropped = {
          let (handle, resolver) = resolver_bearing_handle(ObjectValue::Integer(4));
          drop(resolver);
          handle
      };
      let parms = ObjectHandle::dictionary(vec![(b"Unused".to_vec(), dropped)]);

      let error = decode_filter_specs_from_handle(
          &ObjectHandle::name(b"FlateDecode".to_vec()),
          &parms,
          None,
      )
      .unwrap_err();

      assert_eq!(error.to_string(), "object 20 0 belongs to a dropped PDF");
  }
  ```

  This is distinct from the existing retained `/Columns` dropped-document matrix: it fails if retention is moved ahead of null-aware enumeration.

- [ ] **Step 5: Run the tests and record the intended RED**

  Run:

  ```bash
  cargo test -p flpdf --lib stream_filter::tests::object_shape_reader_omits_null_valued_keys_before_retention -- --exact
  cargo test -p flpdf --lib stream_filter::tests::handle_reader_omits_direct_indirect_and_missing_null_valued_keys -- --exact
  cargo test -p flpdf --lib stream_filter::tests::handle_reader_propagates_get_keys_errors_before_retention -- --exact
  cargo test -p flpdf --lib filters::tests::null_decode_params_values_are_omitted_before_decode_and_encode -- --exact
  ```

  Expected before implementation: the legacy reader test still includes `/Predictor` as `ParamValue::Other`; the handle test still includes nullish retained keys as `Other`; the public test fails because `encode_stream_data`/`decode_stream_data` return the current unsupported error. The resolver-error control may already pass because the old loop touches unretained values; that is acceptable as a non-regression control, while the other three tests must be RED.

- [ ] **Step 6: Commit the RED tests and updated test comments**

  Run:

  ```bash
  cargo fmt --all
  git diff --check
  git diff -- crates/flpdf/src/stream_filter.rs crates/flpdf/src/filters.rs
  git add crates/flpdf/src/stream_filter.rs crates/flpdf/src/filters.rs
  git commit -m "test: pin qpdf DecodeParms null-key behavior"
  ```

  Expected: the commit contains tests/comments only; no production reader function has changed yet.

---

### Task 2: Route consuming handle reads through try_get_keys and omit legacy direct nulls

**Files:**
- Modify: `crates/flpdf/src/stream_filter.rs:378-690, 977-1040, 2474-2535`
- Test: `crates/flpdf/src/stream_filter.rs`
- Test: `crates/flpdf/src/filters.rs`

**Interfaces:**
- Consumes: `ObjectHandle::try_get_keys(&self) -> Result<BTreeSet<Vec<u8>>>`, `ObjectHandle::try_get_key(&self, key: &[u8]) -> Result<ObjectHandle>`, `filter_reads_decode_params`, `retains_decode_param_key`, `param_value_from_handle`, `param_value_without_resolving`, and `param_value_from_object`.
- Produces: a consuming-handle reduction path backed by the qpdf-shaped primitive and a legacy direct-null omission rule.
- Preserves: one shared dictionary snapshot for replicated non-consuming stages, `DecodeParams::Present` for present empty dictionaries, and all downstream filter behavior.

- [ ] **Step 1: Add a consuming-handle helper that follows getKeys/getKey order**

  Replace the resolving branch currently embedded in `decode_params_from_entries` with a dedicated helper of this shape:

  ```rust
  fn decode_params_from_consuming_handle(
      params: &ObjectHandle,
      filter_name: &[u8],
  ) -> Result<DecodeParams> {
      let retains_crypt_name = is_crypt_filter(filter_name);
      let mut retained = Vec::new();
      for key in params.try_get_keys()? {
          if !retains_decode_param_key(&key, retains_crypt_name) {
              continue;
          }
          let value = params.try_get_key(&key)?;
          let keeps_name = is_crypt_name_key(&key, retains_crypt_name);
          retained.push((key, param_value_from_handle(&value, keeps_name)?));
      }
      Ok(DecodeParams::Present(retained))
  }
  ```

  The `try_get_keys` call must occur before retained-key filtering. Do not replace it with raw-map iteration plus `try_is_null`, and do not special-case the five geometry names.

- [ ] **Step 2: Restrict the snapshot-based helper to non-consuming stages**

  Keep `decode_params_from_entries` (or rename it to make its role explicit), but remove its `resolve_values` branch. Its loop must only retain keys and classify direct/already-resolved values through `param_value_without_resolving`:

  ```rust
  fn decode_params_from_entries(
      entries: Option<&BTreeMap<Vec<u8>, ObjectHandle>>,
      filter_name: &[u8],
  ) -> Result<DecodeParams> {
      let Some(entries) = entries else {
          return Ok(DecodeParams::Present(Vec::new()));
      };
      let retains_crypt_name = is_crypt_filter(filter_name);
      let retained = entries
          .iter()
          .filter(|(key, _)| retains_decode_param_key(key, retains_crypt_name))
          .map(|(key, value)| (key.clone(), param_value_without_resolving(value)))
          .collect();
      Ok(DecodeParams::Present(retained))
  }
  ```

  This helper must only be called when `filter_reads_decode_params(filter_name)` is false.

- [ ] **Step 3: Dispatch array items and replicated scalars without regressing snapshot bounds**

  In `decode_params_from_handle`, preserve the whole-object `try_is_null` check, then dispatch consuming stages before taking a raw dictionary snapshot:

  ```rust
  fn decode_params_from_handle(params: &ObjectHandle, filter_name: &[u8]) -> Result<DecodeParams> {
      if params.try_is_null()? {
          return Ok(DecodeParams::Absent);
      }
      if filter_reads_decode_params(filter_name) {
          return decode_params_from_consuming_handle(params, filter_name);
      }
      decode_params_from_entries(params.try_as_dictionary()?.as_ref(), filter_name)
  }
  ```

  In `replicated_decode_params`, enumerate the same scalar handle once per consuming stage while retaining at most one shared raw-map snapshot for all non-consuming stages. Use a nested option so “snapshot not needed” is distinct from “present non-dictionary”:

  ```rust
  fn replicated_decode_params(
      params: &ObjectHandle,
      names: &[Vec<u8>],
  ) -> Result<Vec<DecodeParams>> {
      let entries = names
          .iter()
          .any(|name| !filter_reads_decode_params(name))
          .then(|| params.try_as_dictionary())
          .transpose()?;

      names
          .iter()
          .map(|name| {
              if filter_reads_decode_params(name) {
                  decode_params_from_consuming_handle(params, name)
              } else {
                  decode_params_from_entries(
                      entries.as_ref().and_then(|entries| entries.as_ref()),
                      name,
                  )
              }
          })
          .collect()
  }
  ```

  Confirm this compiles without cloning the whole dictionary once per non-consuming stage. Preserve `handle_reader_lets_a_later_stage_see_a_value_an_earlier_stage_resolved`: snapshot children are shared handles, so a consuming stage's resolution remains visible to a later non-consuming stage.

- [ ] **Step 4: Omit direct null generically in the legacy Object reader**

  In `decode_params_from_object`, filter null values before retained-key reduction:

  ```rust
  Some(dict) => dict
      .iter()
      .filter(|(_, value)| !matches!(value, Object::Null))
      .filter(|(key, _)| retains_decode_param_key(key, retains_crypt_name))
      .map(|(key, value)| {
          let keeps_name = is_crypt_name_key(key, retains_crypt_name);
          (key.to_vec(), param_value_from_object(value, keeps_name))
      })
      .collect(),
  ```

  Do not omit `Object::Reference`, `Object::Name`, or other non-null shapes. The null filter must not be nested inside a `/Predictor` match.

- [ ] **Step 5: Rewrite responsibility comments to match the new boundary**

  Update the documentation on `replicated_decode_params`, `decode_params_from_handle`, `decode_params_from_entries`, `param_value_without_resolving`, and `decode_params_from_object` so it states:

  - consuming stages use `try_get_keys` per qpdf `setDecodeParms` call;
  - `try_get_keys` resolves all children and omits nullish keys before retention;
  - non-consuming stages use the shared snapshot without resolving children;
  - legacy `Object` can omit only direct null;
  - the previous `flpdf-h8mv` divergence is closed.

  Remove stale prose that says a null-valued `/Predictor` survives as `Other`, that `try_is_null`'s answer is discarded, or that the corpus intentionally preserves flpdf's rejection.

  Also update the retained-key discussion near `RETAINED_DECODE_PARAM_KEYS` and the handle-reader mutation matrix in the test module: the consuming path is now `try_get_keys` followed by retained `try_get_key` calls, while the non-consuming path remains snapshot-based. Use this scan to find every stale issue-era statement:

  ```bash
  rg -n 'flpdf-h8mv|null-valued|try_is_null|decode_params_from_entries' crates/flpdf/src/stream_filter.rs crates/flpdf/src/filters.rs
  ```

  Expected: the issue ID may remain in the corpus row label for traceability, but no comment says flpdf still rejects the row or discards a null-test result.

- [ ] **Step 6: Run focused GREEN tests and neighboring non-regression tests**

  Run:

  ```bash
  cargo test -p flpdf --lib stream_filter::tests::object_shape_reader_omits_null_valued_keys_before_retention -- --exact
  cargo test -p flpdf --lib stream_filter::tests::handle_reader_omits_direct_indirect_and_missing_null_valued_keys -- --exact
  cargo test -p flpdf --lib stream_filter::tests::handle_reader_propagates_get_keys_errors_before_retention -- --exact
  cargo test -p flpdf --lib stream_filter::tests::handle_reader_resolves_an_unretained_decode_parms_value_for_a_filter_that_reads_them -- --exact
  cargo test -p flpdf --lib stream_filter::tests::handle_reader_never_resolves_a_decode_parms_value_for_a_filter_that_ignores_them -- --exact
  cargo test -p flpdf --lib stream_filter::tests::handle_reader_lets_a_later_stage_see_a_value_an_earlier_stage_resolved -- --exact
  cargo test -p flpdf --lib stream_filter::tests::handle_reader_matches_object_reader_for_every_filter_shape -- --exact
  cargo test -p flpdf --lib filters::tests::null_decode_params_values_are_omitted_before_decode_and_encode -- --exact
  cargo test -p flpdf --lib filters::tests::non_null_non_integer_decode_params_values_remain_unfilterable -- --exact
  cargo test -p flpdf --lib filters::tests::equivalence::legacy_and_native_entry_points_agree_on_every_corpus_row -- --exact
  ```

  Expected: all commands pass. The null cases reduce to `Present` with null keys absent, public decode/encode succeeds with default predictor behavior, unknown non-null values still resolve for consuming filters, ignored-filter values remain unresolved, mixed-chain liveness remains intact, and both readers/entry points agree.

- [ ] **Step 7: Mutation-check both null-omission routes**

  Use `apply_patch` for each temporary mutation and its exact inverse; do not use `git restore` or overwrite the file. First replace the legacy filter

  ```rust
  .filter(|(_, value)| !matches!(value, Object::Null))
  ```

  with

  ```rust
  .filter(|(_, _)| true)
  ```

  then run:

  ```bash
  cargo test -p flpdf --lib stream_filter::tests::object_shape_reader_omits_null_valued_keys_before_retention -- --exact
  ```

  Expected: FAIL because `/Predictor` returns as `ParamValue::Other`. Apply the inverse patch immediately and rerun the test; expected PASS.

  Next replace the consuming helper's

  ```rust
  for key in params.try_get_keys()? {
  ```

  with this raw, non-null-aware enumeration:

  ```rust
  for key in params
      .try_as_dictionary()?
      .unwrap_or_default()
      .into_keys()
  {
  ```

  then run:

  ```bash
  cargo test -p flpdf --lib stream_filter::tests::handle_reader_omits_direct_indirect_and_missing_null_valued_keys -- --exact
  ```

  Expected: FAIL because the three retained nullish keys survive as `ParamValue::Other`. Apply the inverse patch immediately, rerun the test, and require PASS. Finally run `git diff --check` and inspect `git diff` to confirm neither mutation remains.

- [ ] **Step 8: Inspect and commit the minimal production change**

  Run:

  ```bash
  cargo fmt --all
  git diff --check
  git diff -- crates/flpdf/src/stream_filter.rs crates/flpdf/src/filters.rs
  git status --short
  git add crates/flpdf/src/stream_filter.rs crates/flpdf/src/filters.rs
  git commit -m "fix: skip null-valued DecodeParms keys"
  ```

  Expected: production changes are confined to the reader/reduction boundary in `stream_filter.rs`; `filters.rs` contains test/comment changes only. There are no changes to `object_handle.rs`, filter implementations, or pipelines.

---

### Task 3: Verify oracle parity, update correspondence, and close the issue

**Files:**
- Modify: `docs/qpdf-correspondence.md:121,202`
- Verify: `crates/flpdf/src/stream_filter.rs`
- Verify: `crates/flpdf/src/filters.rs`
- Verify unchanged: `tests/fixtures/test_driver/stream_decode_parms_{direct,indirect}_null.{pdf,out}`

**Interfaces:**
- Consumes: the committed reader change and existing qpdf oracle/golden corpus.
- Produces: durable correspondence documentation, release-quality verification, a closed/persisted Bead, and a pushed feature branch.

- [ ] **Step 1: Update correspondence without overstating the larger port**

  In the ObjectHandle row at `docs/qpdf-correspondence.md:121`, replace the trailing statement that consumer migration belongs to `flpdf-h8mv` with a statement that `stream_filter.rs` consuming stages now use `try_get_keys` before retained-key reduction.

  In the QPDFStreamFilter row at `docs/qpdf-correspondence.md:202`:

  - remove “qpdf permits null-valued `/DecodeParms` keys but flpdf rejects them” from known deviations;
  - record that the handle reader uses `try_get_keys` to omit direct, indirect, and dangling nullish entries;
  - record that the legacy reader performs the equivalent direct-null omission within its non-resolving responsibility;
  - preserve the existing warning-channel deviation and the overall row status.

  Do not change unrelated responsibility rows or mark the whole ObjectHandle migration complete.

- [ ] **Step 2: Run formatting, lint, focused suites, and the full workspace suite**

  Run:

  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test -p flpdf --lib stream_filter::tests
  cargo test -p flpdf --lib filters::tests
  cargo test -p flpdf-qtest-tools --test driver_goldens test_0_1_fixtures_match_committed_qpdf_merged_output -- --exact
  cargo test
  git diff --check
  ```

  Expected: every command exits 0; the committed direct/indirect null goldens match flpdf-test-driver; ignored tests may remain ignored, but there are no failures or clippy warnings.

- [ ] **Step 3: Run the pinned qpdf differential and protect oracle files from drift**

  Run:

  ```bash
  bash scripts/qpdf-test-driver-diff.sh
  bash tests/fixtures/test_driver/generate.sh --check
  git diff --exit-code -- tests/fixtures/test_driver
  ```

  Expected: pinned qpdf and flpdf test-driver outputs match all manifest fixtures and CLI probes; the generator reports stable authored PDFs; no `.pdf` or `.out` changes exist.

- [ ] **Step 4: Commit correspondence documentation**

  Run:

  ```bash
  git diff -- docs/qpdf-correspondence.md
  git add docs/qpdf-correspondence.md
  git commit -m "docs: record DecodeParms null-key parity"
  git status --short
  ```

  Expected: the worktree is clean. A committed clean tree is required before the patch-coverage gate because it compares `HEAD` while instrumenting the current tree.

- [ ] **Step 5: Generate fresh coverage and require 100% changed-line coverage**

  Run:

  ```bash
  cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
  scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
  ```

  Expected: coverage generation completes and `patch-coverage.sh` reports 100% coverage for every changed executable line under `crates/flpdf/src`. If a reachable branch is uncovered, add a behavior-focused test, rerun focused/full gates, commit it, regenerate LCOV, and rerun the patch gate; do not use a coverage-ignore marker for reachable behavior.

- [ ] **Step 6: Review final scope and history**

  Run:

  ```bash
  git diff --stat origin/main...HEAD
  git diff origin/main...HEAD -- crates/flpdf/src/stream_filter.rs crates/flpdf/src/filters.rs docs/qpdf-correspondence.md docs/superpowers/specs/2026-08-07-decodeparms-null-key-consumer-design.md docs/superpowers/plans/2026-08-07-decodeparms-null-key-consumer.md
  git log --oneline origin/main..HEAD
  git status --short
  ```

  Expected: only the approved spec, this plan, two reader/test files, and correspondence documentation differ from `origin/main`; oracle fixtures/goldens, `object_handle.rs`, filter implementations, pipelines, and unrelated files are unchanged; the tree is clean.

- [ ] **Step 7: Close and persist the Bead, then publish the feature branch**

  Run only after every preceding gate passes:

  ```bash
  bd close flpdf-h8mv --reason="Matched qpdf 11.9.0 null-aware DecodeParms key enumeration in both readers with public decode/encode tests, direct/indirect/missing handle coverage, existing oracle goldens, pinned differential, workspace gates, and 100% changed-line coverage"
  bd dolt push
  git push -u origin feature/flpdf-h8mv-decodeparms-null-keys
  bd show flpdf-h8mv --json | jq '.[0] | {id, status, assignee}'
  git status --short
  ```

  Expected: Beads and git pushes succeed, the issue reads `closed`, and the worktree remains clean. Do not open or merge a pull request unless separately requested.

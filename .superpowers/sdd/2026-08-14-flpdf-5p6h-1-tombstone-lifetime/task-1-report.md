# Task 1 report: qpdf tombstone lifetime oracle and RED regressions

## Scope

Task 1 only. No Task 2 production refactoring was made.

## qpdf 11.9.0 oracle contract

The probe builds against the pinned qpdf commit `3b97c9bd266b7c32ea36d3536e22dab77412886d`, checks that its tracked source tree is clean, verifies `/usr/bin/qpdf` reports 11.9.0, and confirms `ldd` binds the probe to the freshly built pinned `libqpdf.so`.

The removal observation uses only public qpdf API: `replaceObject(3, 0, QPDFObjectHandle::newNull())`. `QPDF.hh:374-386` documents that null replacement effectively removes an object from the written file. It is not a direct `QPDF::removeObject` call: `removeObject` appears in the class's private section (`QPDF.hh:858-1041`) and is therefore non-exported/non-callable by this C++ probe. The source-level audit at `QPDF.cc:1996-2006` establishes its distinct internal behavior: it erases the exact xref key, converts an outstanding cached object to null, clears its object/generation identity, and removes the cache entry. The Rust direct-removal regression explicitly names this private-method source contract; it does not claim that it was observed through the public probe.

The public probe's asserted machine-readable observations are:

```
registration.xref=2.0
registration.all=2.0,3.1
baseline.recovery.xref=1.0,2.0
baseline.recovery.all=1.0,2.0,3.0
removal_proxy.recovery.xref=1.0,2.0
removal_proxy.recovery.all=1.0,2.0,3.0
removal_proxy.recovery.get_3_0_initialized=true
removal_proxy.recovery.get_3_0_null=true
replacement.recovery.xref=1.0,2.0
replacement.recovery.all=1.0,2.0,3.0,3.1
replacement.recovery.get_3_0_initialized=true
replacement.recovery.get_3_0_value=70
replacement.recovery.get_3_1_initialized=true
replacement.recovery.get_3_1_value=71
runtime.check.status=3
```

`runtime.check.status=3` is qpdf's expected damaged-file/reconstruction result for the same fixture emitted by the probe.

## Focused validation

The brief's literal Cargo commands include bare test names plus `--exact`. Rust's libtest exposes these tests with their module-qualified names, so each literal command exited 0 after running `0 tests`; this is recorded as a command-invocation limitation, not validation evidence. The equivalent fully-qualified focused commands were then run.

| Command | Result |
| --- | --- |
| `cargo test -p flpdf --lib reader::resolver::tests::reconstruction_does_not_reregister_privately_removed_unindexed_object -- --exact` | PASS (1 test) |
| `cargo test -p flpdf --lib reader::resolver::tests::set_object_generation_replacement_matches_qpdf_tombstone_lifetime -- --exact` | RED, expected Task 2 gap: replacement-only keys remain in `get_xref_table` after recovery |
| `cargo test -p flpdf --lib reader::resolver::tests::replace_object_handle_generation_replacement_matches_qpdf_tombstone_lifetime -- --exact` | RED, same expected Task 2 gap |
| `cargo test -p flpdf --lib xref::tests::xref_registration_free_object_suppression_is_local_to_registration -- --exact` | PASS (1 test) |
| `scripts/qpdf-tombstone-lifetime-probe.sh` | PASS |

The failing generation-replacement regressions are intentionally retained as RED evidence for later production tasks. No production code was changed to make them pass.

## Fix Round 1

### Corrected private-removal contract

The earlier direct-removal regression was incorrect and has been replaced with `reconstruction_reregisters_privately_removed_unindexed_object_like_qpdf`. Pinned qpdf source establishes the contract without making a private call: `QPDF::removeObject` erases only the exact xref/cache state (`QPDF.cc:1996-2006`), while `reconstruct_xref` scans every recoverable object header (`QPDF.cc:516-575`) and `insertReconstructedXrefEntry` suppresses an object only when the recovery-start `deleted_objects` set contains its number (`QPDF.cc:1194-1210`). `removeObject` does not add to that set. Therefore recovery re-registers the fixture's stale `3 0` body, mints its canonical handle, includes it in `get_xref_table` and `get_all_objects`, and resolves it to integer `99`.

The executable probe remains explicitly a `removal_proxy`: it uses only documented public `replaceObject(..., QPDFObjectHandle::newNull())`, never claims to call private `removeObject`, and reports its public null-replacement results under `removal_proxy.*`. No private/public macro hack is used. The brief's `removeObject` executable-probe wording is impossible against qpdf 11.9.0's public API; private behavior is instead validated from the pinned source locations above.

### Captured fully-qualified focused validation

The commands below were run after the fix. This is the captured stdout/stderr and exit status; bare-name `--exact` commands remain documented above only as 0-test invocation limitations.

#### `cargo fmt --all -- --check`

```text
exit=0
```

#### `cargo test -p flpdf --lib reader::resolver::tests::reconstruction_reregisters_privately_removed_unindexed_object_like_qpdf -- --exact`

```text
exit=101
Finished `test` profile [unoptimized + debuginfo] target(s) in 0.02s
Running unittests src/lib.rs (target/debug/deps/flpdf-14847bfdb19da50f)

running 1 test
WARNING: reported number of objects (4) is not one plus the highest object number (2)
WARNING: file is damaged
WARNING: offset 9: expected 1 0 obj
WARNING: Attempting to reconstruct cross-reference table
test reader::resolver::tests::reconstruction_reregisters_privately_removed_unindexed_object_like_qpdf ... FAILED

failures:

---- reader::resolver::tests::reconstruction_reregisters_privately_removed_unindexed_object_like_qpdf stdout ----

thread 'reader::resolver::tests::reconstruction_reregisters_privately_removed_unindexed_object_like_qpdf' (63) panicked at crates/flpdf/src/reader/resolver.rs:11352:9:
assertion failed: pdf.get_xref_table().contains_key(&removed_ref)
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 4331 filtered out; finished in 0.00s
error: test failed, to rerun pass `-p flpdf --lib`
```

#### `cargo test -p flpdf --lib reader::resolver::tests::set_object_generation_replacement_matches_qpdf_tombstone_lifetime -- --exact`

```text
exit=101
Finished `test` profile [unoptimized + debuginfo] target(s) in 0.02s
Running unittests src/lib.rs (target/debug/deps/flpdf-14847bfdb19da50f)

running 1 test
WARNING: reported number of objects (4) is not one plus the highest object number (2)
WARNING: file is damaged
WARNING: offset 9: expected 1 0 obj
WARNING: Attempting to reconstruct cross-reference table
test reader::resolver::tests::set_object_generation_replacement_matches_qpdf_tombstone_lifetime ... FAILED

failures:

---- reader::resolver::tests::set_object_generation_replacement_matches_qpdf_tombstone_lifetime stdout ----

thread 'reader::resolver::tests::set_object_generation_replacement_matches_qpdf_tombstone_lifetime' (63) panicked at crates/flpdf/src/reader/resolver.rs:11396:9:
qpdf keeps replacement-only entries out of getXRefTable after recovery
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:

    reader::resolver::tests::set_object_generation_replacement_matches_qpdf_tombstone_lifetime

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 4331 filtered out; finished in 0.00s
error: test failed, to rerun pass `-p flpdf --lib`
```

#### `cargo test -p flpdf --lib reader::resolver::tests::replace_object_handle_generation_replacement_matches_qpdf_tombstone_lifetime -- --exact`

```text
exit=101
Finished `test` profile [unoptimized + debuginfo] target(s) in 0.02s
Running unittests src/lib.rs (target/debug/deps/flpdf-14847bfdb19da50f)

running 1 test
WARNING: reported number of objects (4) is not one plus the highest object number (2)
WARNING: file is damaged
WARNING: offset 9: expected 1 0 obj
WARNING: Attempting to reconstruct cross-reference table
test reader::resolver::tests::replace_object_handle_generation_replacement_matches_qpdf_tombstone_lifetime ... FAILED

failures:

---- reader::resolver::tests::replace_object_handle_generation_replacement_matches_qpdf_tombstone_lifetime stdout ----

thread 'reader::resolver::tests::replace_object_handle_generation_replacement_matches_qpdf_tombstone_lifetime' (63) panicked at crates/flpdf/src/reader/resolver.rs:11396:9:
qpdf keeps replacement-only entries out of getXRefTable after recovery
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:

    reader::resolver::tests::replace_object_handle_generation_replacement_matches_qpdf_tombstone_lifetime

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 4331 filtered out; finished in 0.00s
error: test failed, to rerun pass `-p flpdf --lib`
```

#### `cargo test -p flpdf --lib xref::tests::xref_registration_free_object_suppression_is_local_to_registration -- --exact`

```text
exit=0
Finished `test` profile [unoptimized + debuginfo] target(s) in 0.02s
Running unittests src/lib.rs (target/debug/deps/flpdf-14847bfdb19da50f)

running 1 test
test xref::tests::xref_registration_free_object_suppression_is_local_to_registration ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 4331 filtered out; finished in 0.00s
```

#### `scripts/qpdf-tombstone-lifetime-probe.sh`

```text
exit=0
registration.xref=2.0
registration.all=2.0,3.1
baseline.recovery.xref=1.0,2.0
baseline.recovery.all=1.0,2.0,3.0
removal_proxy.recovery.xref=1.0,2.0
removal_proxy.recovery.all=1.0,2.0,3.0
removal_proxy.recovery.get_3_0_initialized=true
removal_proxy.recovery.get_3_0_null=true
replacement.recovery.xref=1.0,2.0
replacement.recovery.all=1.0,2.0,3.0,3.1
replacement.recovery.get_3_0_initialized=true
replacement.recovery.get_3_0_value=70
replacement.recovery.get_3_1_initialized=true
replacement.recovery.get_3_1_value=71
runtime.check.status=3
```

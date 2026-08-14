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

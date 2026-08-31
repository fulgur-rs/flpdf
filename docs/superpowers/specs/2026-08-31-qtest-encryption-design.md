# qtest `encryption.test` parity design

## Goal

Make the vendored qpdf 11.9.0 `encryption.test` suite pass all 331 reported
testcases through flpdf's canonical Rust reader, writer, Job/CLI, and portable
qpdf-ctest process-adapter paths.

The ten qpdf-ctest rows are included in the suite goal. They do not require a
C or C++ ABI: the existing Rust `qpdf-ctest` binary is the process boundary and
must reproduce the portable observations and lifecycle output of the selected
qpdf C helper cases.

## Oracle evidence

The pinned qpdf source is `/home/ubuntu/.cache/flpdf/qpdf-11.9.0` and the
behavioral executable is qpdf 11.9.0. The relevant ownership is:

| qpdf responsibility | Source | Required behavior |
|---|---|---|
| Encryption argv state machine | `libqpdf/qpdf/auto_job_init.hh:133-166`, `libqpdf/QPDFJob_argv.cc:164-230` | Accept positional and dashed password/bit forms, reject mixed forms, select the key-length option table, and apply subflags in order. |
| Job write policy | `libqpdf/QPDFJob.cc:2725-2828` | Select R=2/R=3/R=4/R=5/R=6, emit the modern accessibility warning, gate RC4, and dispatch R=2 to its separate permission setter. |
| Parsed encryption defaults/authentication | `libqpdf/QPDF_encryption.cc:719-950` | Retain parsed state before authentication, use 128 bits when V=2 `/Length` is absent/invalid, retain V<5 recovered user passwords, and report `invalid password`. |
| Inspection and JSON | `libqpdf/QPDFJob.cc:700-765,1200-1270` | Share the exact encryption report between `--check` and `--show-encryption`; expose recovered password and optional key with qpdf's match semantics. |
| Portable qpdf-ctest cases | `qpdf/qpdf-ctest.c:135-169,268-348,300-310` | Reproduce test 2, 11, 12, 13, 15, 17, and 18 using canonical PDF APIs and exact process output. |

A same-run baseline on flpdf main `73c6d6d7` used the vendored qtest corpus,
the repository shims, and release helper binaries. It reported 331 total,
241 passes, and 90 failures. The failures clustered into named encryption
arguments, R=2 permissions, V=2 default length, V<5 JSON recovery, check/key
inspection, R=5 policy, wrong-password wording, and unsupported qpdf-ctest
numbers. The paired baseline artifacts are retained at
`/tmp/flpdf-encryption-baseline.odrQM9/harness.log` and
`/tmp/flpdf-encryption-baseline.odrQM9/qtest-results.xml`.

## Route inventory and ownership

The current `parse_encrypt_segment` in `crates/flpdf-cli/src/main.rs` is a
mixed route: it parses the qpdf Job grammar, applies Job policy, and constructs
writer parameters in one CLI helper. The current `PermissionsConfig` also
represents only the R>=3 permission layout, while the writer uses it for R=2.
The canonical target is a qpdf-shaped Job encryption configuration that emits
typed writer parameters and configuration warnings; the Standard handler
continues to own cryptographic dictionary construction and byte emission.

The existing parsed `EncryptionInspectionState` and authenticated
`EncryptionState` are the canonical reader sources. JSON and check consumers
must read their projections rather than re-reading `/Encrypt` independently.
The existing qpdf-ctest Rust binary is a thin process adapter; it may project
portable qpdf C-helper observations but may not contain a second encryption
algorithm implementation.

## Implementation slices

The work is split into dependency-ordered slices so each change has one qpdf
responsibility and can be tested independently.

1. **R=2 permission primitive** (`flpdf-61e`): introduce the separate R=2
   four-permission representation and `/P` encoding, then connect V=1/R=2
   writer construction without reinterpreting R>=3 fields.
2. **Job encryption argv/policy** (`flpdf-25kg.5.10`): capture the raw
   `--encrypt` segment at the CLI edge, parse the qpdf positional/dashed forms
   in the Job-owned configuration route, support the complete qpdf encryption
   option tables, preserve left-to-right mutation, emit the R>3 accessibility
   warning, allow qpdf's R=5 path, and pass typed settings to the canonical
   writer.
3. **Reader/inspection/JSON** (`flpdf-25kg.4.12` and the existing
   `flpdf-oox1` report slice): correct V=2 fallback length, map CLI
   BadPassword rendering to qpdf's `invalid password`, emit recovered V<5
   user passwords in JSON/report output, and propagate `--show-encryption-key`
   through the check report without authorizing unauthenticated state.
4. **Portable qpdf-ctest encryption cases** (`flpdf-25kg.2.8`): add test
   numbers 2, 11, 12, 13, 15, 17, and 18 using `Pdf`/`PdfWriter`, preserving
   existing test 1, 19, 20, version, and unsupported-number contracts.
5. **Suite ledger verification**: run the isolated full `encryption` suite,
   validate the same-run `harness.log`/`qtest-results.xml` pair, and update
   only exact evidence-backed manifest rows. No expected qpdf output is
   changed.

## Data flow

The CLI captures one terminated encryption segment. The Job configuration
parser resolves either positional `(user, owner, bits)` or dashed values,
selects the corresponding qpdf option table, and retains typed R=2 and R>=3
permission state separately. The writer receives `EncryptParams`, builds the
appropriate Standard handler dictionary, derives keys, and emits the output.

For read-only operations, the reader parses encryption fields before trying
the supplied password. A successful authentication installs
`EncryptionState`; a failed authentication may retain only the inspection
snapshot. `QPDFJob::check`, `show-encryption`, and JSON use that snapshot and
never use its fields as a decryption key. The raw key is printed only when
authenticated or explicitly opened through the raw-key path.

The qpdf-ctest adapter calls the same reader/writer APIs. Test 2 converts the
typed bad-password result into qpdf's C-helper error projection. Tests 11,
12, 15, 17, and 18 construct typed writer parameters; test 13 reads the
retained user-password projection and writes with encryption preservation
disabled.

## Error and warning rules

- Keep `Error::Encrypted(EncryptedError::BadPassword)` as the typed Rust
  classification.
- At the qpdf-compatible CLI boundary, render that classification as
  `invalid password` with the input filename, matching qpdf 11.9.0.
- A missing or invalid V=2 `/Length` is a qpdf fallback to 128 bits, not an
  authentication failure.
- `--accessibility=n` is accepted for modern encryption and emits qpdf's
  warning; it does not clear the modern accessibility capability.
- R=5 is accepted by qpdf's `--force-R5` writer path. RC4 write refusal stays
  governed by qpdf's RC4 policy.
- Partial inspection state can report encryption metadata but can never be
  passed to writer or stream/string decryption consumers.

## Acceptance

Completion requires all 331 `encryption.test` testcase elements to be PASS in
one fresh run, with no unexpected passes/failures, and the qtest XML and
human-readable harness log from that same run. The Rust side must pass
formatting, focused tests, all-features clippy, workspace tests, and the
repository's qpdf module/deviation checks. Beads must be acyclic, read back
after every mutation, and be persisted with `bd dolt push`.

The qtest repository remains separate from flpdf. Its vendored qpdf fixtures
are not copied into flpdf; only its manifest/state rows are changed after the
corresponding behavior is proven.

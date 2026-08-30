# qtest encryption.test Implementation Plan

> For agentic workers: REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Make all 331 qpdf 11.9.0 encryption.test testcases pass through the canonical flpdf Rust reader, writer, Job/CLI, and portable qpdf-ctest adapter.

**Architecture:** Keep qpdf encryption configuration, Standard-handler cryptography, inspection state, and process-adapter lifecycles as separate responsibility units. The CLI captures a terminated raw encryption segment, the Job-owned configuration parser resolves qpdf positional/dashed grammar, the writer receives typed R=2 or R>=3 permission state, and read-only consumers use canonical encryption projections. qpdf-ctest remains a thin Rust process adapter and never reimplements cryptography.

**Tech Stack:** Rust workspace (flpdf, flpdf-cli, flpdf-qtest-tools), clap, qpdf 11.9.0 pinned source and /usr/bin/qpdf, vendored qtest 1.9, cargo test, cargo clippy, qtest XML/log verification, Beads.

---

### Task 1: Port the R=2 permission primitive

**Beads:** flpdf-61e

**Files:**
- Modify: crates/flpdf/src/encryption/permissions.rs
- Modify: crates/flpdf/src/encryption.rs
- Modify: crates/flpdf/src/encryption/standard.rs
- Modify: crates/flpdf/src/writer.rs
- Test: crates/flpdf/src/encryption/permissions.rs
- Test: crates/flpdf-cli/tests/encrypt_cli_tests.rs

- [ ] Step 1: Write RED tests for qpdf's separate R=2 encoding.

Add a dedicated four-field R2PermissionsConfig test beside the existing R>=3 permission tests. The desired test is:

~~~rust
#[test]
fn r2_permissions_encode_the_qpdf_four_bit_layout() {
    let permissions = R2PermissionsConfig {
        print: false,
        modify: true,
        extract: true,
        annotate: true,
    };
    assert_eq!(permissions.to_p_bits(), -8);
}
~~~

Add a CLI regression test that writes a 40-bit PDF with --print=n --modify=y --extract=n --annotate=n and reads /P through the Rust reader. The qpdf oracle command is:

~~~bash
qpdf --encrypt --allow-weak-crypto '' master 40 --print=n --modify=y --extract=n --annotate=n -- input.pdf output.pdf
qpdf --show-encryption --password=master output.pdf
~~~

- [ ] Step 2: Run the RED tests.

~~~bash
cargo test -p flpdf --lib r2_permissions_encode_the_qpdf_four_bit_layout
cargo test -p flpdf-cli --test encrypt_cli_tests r2
~~~

Expected: compilation or assertion failure caused by the missing R=2 representation, not by a fixture or test typo.

- [ ] Step 3: Implement the qpdf-shaped R=2 type and writer path.

Add an explicit R2PermissionsConfig and carry it through EncryptParams. Encode only qpdf's R=2 print bit 3, modify bit 4, extract bit 5, and annotate bit 6, with the reserved bits set according to qpdf's R=2 writer output. Make the V=1/R=2 writer branch pass the R=2 value to build_v1_v2_encrypt_dict. Keep PermissionsConfig for the R>=3 family; do not make it branch on a revision.

The core encoder has this shape, with the constants confirmed against qpdf before finalizing:

~~~rust
pub(crate) fn to_p_bits(&self) -> i32 {
    let mut bits = 0xFFFF_FFC0u32;
    if self.print { bits |= 0x004; }
    if self.modify { bits |= 0x008; }
    if self.extract { bits |= 0x010; }
    if self.annotate { bits |= 0x020; }
    bits as i32
}
~~~

- [ ] Step 4: Run the focused R=2 and existing R>=3 tests.

~~~bash
cargo test -p flpdf --lib encryption::permissions
cargo test -p flpdf --lib encryption::standard
cargo test -p flpdf-cli --test encrypt_cli_tests r2
~~~

Expected: all focused tests pass and existing R>=3 permission expectations remain unchanged.

- [ ] Step 5: Commit.

~~~bash
git add crates/flpdf/src/encryption/permissions.rs crates/flpdf/src/encryption.rs crates/flpdf/src/encryption/standard.rs crates/flpdf/src/writer.rs crates/flpdf-cli/tests/encrypt_cli_tests.rs
git commit -m "feat: port qpdf R2 encryption permissions"
~~~

### Task 2: Port QPDFJob encryption argv grammar and write policy

**Beads:** flpdf-25kg.5.10

**Files:**
- Modify: crates/flpdf-cli/src/main.rs
- Modify: crates/flpdf/src/job/lifecycle.rs or the selected Job encryption module
- Modify: crates/flpdf/src/encryption.rs
- Test: crates/flpdf-cli/src/main.rs
- Test: crates/flpdf-cli/tests/encrypt_cli_tests.rs

- [ ] Step 1: Write RED tests for positional and dashed qpdf forms.

Add real parser tests:

~~~rust
#[test]
fn encrypt_parser_accepts_dashed_passwords_and_bits_before_the_terminator() {
    let tokens = vec![
        "--user-password=u".to_owned(),
        "--bits=256".to_owned(),
        "--allow-insecure".to_owned(),
    ];
    let parsed = parse_encrypt_segment(&tokens, false).expect("dashed qpdf form");
    assert_eq!(parsed.method, EncryptMethod::V5R6Aes256);
    assert_eq!(parsed.user_password, b"u");
    assert!(parsed.owner_password.is_empty());
}

#[test]
fn encrypt_parser_rejects_mixed_positional_and_dashed_passwords() {
    let tokens = vec![
        "user".to_owned(),
        "--owner-password=owner".to_owned(),
        "128".to_owned(),
    ];
    let error = parse_encrypt_segment(&tokens, true).expect_err("mixed form");
    assert!(error.to_string().contains(
        "positional and dashed encryption arguments may not be mixed"
    ));
}
~~~

Add CLI tests for --encrypt --bits=128 --force-V4 --, --encrypt --bits=128 --cleartext-metadata --, --encrypt --bits=128 --use-aes=y --, and --encrypt user owner 256 --force-R5 --. Compare /V, /R, /CFM, /EncryptMetadata, and qpdf --check output.

- [ ] Step 2: Run the RED tests.

~~~bash
cargo test -p flpdf-cli --bin flpdf encrypt_parser_accepts_dashed_passwords_and_bits_before_the_terminator
cargo test -p flpdf-cli --bin flpdf encrypt_parser_rejects_mixed_positional_and_dashed_passwords
~~~

Expected: the current Clap capture rejects the named form or passes named tokens to a parser that cannot interpret them.

- [ ] Step 3: Capture the raw terminated segment and implement the qpdf state machine.

Change the encrypt argument capture to accept zero or more values through the -- terminator. Implement separate positional and dashed modes, an accumulator for the three positional values, named user/owner/bits values, a selected key-length table, and the termination boundary. Reject mixing before opening input or output. Apply later options left-to-right after bits selects the qpdf table.

Map the selected key length and flags to typed methods:

~~~rust
match key_len {
    40 => EncryptMethod::V1Rc440,
    128 if force_v4 && use_aes => EncryptMethod::V4Aes128,
    128 if force_v4 => EncryptMethod::V4Rc4128,
    128 => EncryptMethod::V2Rc4128,
    256 if force_r5 => EncryptMethod::V5R5Aes256,
    256 => EncryptMethod::V5R6Aes256,
    _ => return Err(invalid_key_length(key_len)),
}
~~~

Do not reject R=5 merely because it is deprecated. QPDFJob.cc:2752-2763 gates RC4 paths but accepts the force-R5 path. Retain RC4 refusal. Retain an explicit accessibility-was-set-to-n bit so R>3 can emit qpdf's warning while the resulting capability remains allowed.

- [ ] Step 4: Run parser tests and direct qpdf probes.

~~~bash
cargo test -p flpdf-cli --bin flpdf encrypt_parser
cargo test -p flpdf-cli --test encrypt_cli_tests encrypt
/usr/bin/qpdf --encrypt --user-password=u --bits=256 --allow-insecure -- tests/fixtures/minimal.pdf /tmp/qpdf-named-encrypt.pdf
/usr/bin/qpdf --encrypt user owner 256 --force-R5 -- tests/fixtures/minimal.pdf /tmp/qpdf-force-r5.pdf
~~~

Expected: equivalent qpdf and flpdf commands agree on status, warnings, and encryption dictionary projections.

- [ ] Step 5: Commit.

~~~bash
git add crates/flpdf-cli/src/main.rs crates/flpdf/src/job crates/flpdf/src/encryption.rs crates/flpdf-cli/tests/encrypt_cli_tests.rs
git commit -m "feat: align qpdf encryption argument handling"
~~~

### Task 3: Complete reader, inspection, check, and JSON parity

**Beads:** flpdf-25kg.4.12; coordinate the full report-owned portion with existing flpdf-oox1.

**Files:**
- Modify: crates/flpdf/src/encryption/state.rs
- Modify: crates/flpdf/src/reader.rs
- Modify: crates/flpdf/src/job/json_sections.rs
- Modify: crates/flpdf/src/job/check.rs
- Modify: crates/flpdf-cli/src/main.rs
- Test: crates/flpdf/src/encryption/state.rs
- Test: crates/flpdf/src/job/check.rs
- Test: crates/flpdf-cli/tests/encrypt_cli_tests.rs
- Test: crates/flpdf-cli/tests/cli_password_hex_key_tests.rs

- [ ] Step 1: Write RED tests for V=2 fallback, JSON recovery, and CLI wording.

Add a real fixture test:

~~~rust
#[test]
fn v2_missing_length_uses_qpdf_128_bit_fallback() {
    let mut pdf = open_fixture("bad-encryption-length.pdf");
    assert!(pdf.is_encrypted());
    let info = pdf.encryption_info().expect("encryption info").expect("encrypted");
    assert_eq!(info.length_bits, 128);
}
~~~

Add a JSON test for an owner-password-opened R=3 file that expects a string-valued recovereduserpassword. Add a CLI test that runs qpdf -qdf --password=quack on the wrong-password fixture and expects invalid password, while the API test still matches Error::Encrypted(EncryptedError::BadPassword).

- [ ] Step 2: Run the RED tests.

~~~bash
cargo test -p flpdf --lib v2_missing_length_uses_qpdf_128_bit_fallback
cargo test -p flpdf-cli --test encrypt_cli_tests recovereduserpassword
cargo test -p flpdf-cli --test encrypt_cli_tests invalid_password
~~~

Expected: current V=2 authentication fails due to the 40-bit fallback, JSON emits null, and CLI output says encrypted PDF: incorrect password.

- [ ] Step 3: Implement qpdf's V=2 fallback and canonical projections.

Use qpdf's default selection: V<=1 -> 40, V=4 -> 128, V=5 -> 256, and V=2 with absent or invalid /Length -> 128. Keep malformed type handling distinct from the numeric fallback. Have json_sections::build_encrypt_section_with_options use Pdf::encryption_info for match flags and recovered user password; emit the file key only for show_encryption_key.

Keep actionable_password_error as presentation conversion only:

~~~rust
fn actionable_password_error(error: flpdf::Error) -> Box<dyn std::error::Error> {
    if is_bad_password_error(&error) {
        return "invalid password".into();
    }
    error.into()
}
~~~

Preserve filename decoration at the outer error_with_file boundary and preserve the typed BadPassword variant.

- [ ] Step 4: Thread show-encryption-key through the canonical check renderer.

Make the qpdf Job check configuration carry the flag into the existing emit_encryption_report path. Do not add a second report formatter in main.rs. Verify the key is after User password = and before match lines, matching QPDFJob::showEncryption.

- [ ] Step 5: Run focused reader, JSON, check, and CLI tests.

~~~bash
cargo test -p flpdf --lib encryption::state
cargo test -p flpdf --lib job::check
cargo test -p flpdf --test reader_tests
cargo test -p flpdf-cli --test encrypt_cli_tests
cargo test -p flpdf-cli --test cli_password_hex_key_tests
~~~

Expected: all focused tests pass, including wrong-password, V=2 malformed-length, recovered-password, and key-output cases.

- [ ] Step 6: Commit.

~~~bash
git add crates/flpdf/src/encryption/state.rs crates/flpdf/src/reader.rs crates/flpdf/src/job/json_sections.rs crates/flpdf/src/job/check.rs crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/encrypt_cli_tests.rs crates/flpdf-cli/tests/cli_password_hex_key_tests.rs
git commit -m "fix: align qpdf encryption inspection output"
~~~

### Task 4: Extend the portable qpdf-ctest adapter

**Beads:** flpdf-25kg.2.8

**Files:**
- Modify: crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs
- Modify: crates/flpdf-qtest-tools/tests/qpdf_ctest_cli.rs

- [ ] Step 1: Write RED adapter tests for test numbers 2, 11, 12, 13, 15, 17, and 18.

Use real qtest fixtures through temporary paths. Assert process output and output existence. Test 2 must expect the qpdf C error projection; test 13 must print user password before C test 13 done.

~~~rust
for test_number in [11, 12, 15, 17, 18] {
    Command::cargo_bin("qpdf-ctest")
        .unwrap()
        .args([
            test_number.to_string(),
            input.to_string_lossy().into_owned(),
            String::new(),
            output.to_string_lossy().into_owned(),
        ])
        .assert()
        .success()
        .stdout(format!("C test {test_number} done\n"));
}
~~~

- [ ] Step 2: Run the RED adapter tests.

~~~bash
cargo test -p flpdf-qtest-tools --test qpdf_ctest_cli
~~~

Expected: test numbers 2, 11, 12, 13, 15, 17, and 18 fail with the current unsupported-number error.

- [ ] Step 3: Implement selected qpdf-ctest lifecycles.

Extend numeric dispatch without changing test 1, 19, 20, --version, or unsupported-number behavior. Use shared adapter helpers for one-attempt authentication, static ID/static AES IV writer settings, and qpdf-shaped error reporting.

Construct typed parameters matching qpdf:
- test 11: R=2, user1/owner1, print false, modify/extract/annotate true.
- test 12: R=3, user2/owner2, all R>=3 capabilities true except print low.
- test 15: R=4 AES, user2/owner2, all R>=3 capabilities true except print low.
- test 17: R=5 AES, user3/owner3, all R>=3 capabilities true except print low.
- test 18: R=6 AES, user4/owner4, all R>=3 capabilities true except print low.

Use existing public constructors or add only qpdf-shaped constructors required by the writer. Do not copy Algorithm 3/4/5/6/7/8/9 code into this binary. Test 2 must map BadPassword to code 4, the input filename, position 0, and detail invalid password, then finish with exit 0 as qpdf-ctest does. Test 13 must use the retained user-password projection and disable preserve_encryption.

- [ ] Step 4: Run adapter tests and inspect qtest C-helper rows.

~~~bash
cargo test -p flpdf-qtest-tools --test qpdf_ctest_cli
~~~

Run the targeted qtest suite with TESTS=encryption and inspect rows 239-258. Expected: all qpdf-ctest invocation rows and dependent checks pass.

- [ ] Step 5: Commit.

~~~bash
git add crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs crates/flpdf-qtest-tools/tests/qpdf_ctest_cli.rs
git commit -m "feat: cover qpdf encryption ctest lifecycles"
~~~

### Task 5: Run the full encryption suite and reconcile the qtest ledger

**Files:**
- Modify in qtest worktree only: parity/qtest-11.9.0.jsonl
- Read-only artifacts: survey/latest/harness.log, qtest-results.xml, qtest.log

- [ ] Step 1: Create an isolated qtest worktree.

~~~bash
git -C /home/ubuntu/flpdf-qtest worktree add /tmp/flpdf-qtest-encryption -b feature/flpdf-qtest-encryption
~~~

Use the flpdf branch release binaries through FLPDF_DIR. Do not edit or delete vendored qpdf fixtures. Keep qtest's own qtest.log separate from harness.log.

- [ ] Step 2: Run a fresh targeted encryption suite.

Copy vendor/qpdf-qtest to a disposable datadir, copy every executable shim to a disposable bindir, and run qtest-driver with TESTS=encryption. Capture combined output as harness.log in a separate artifact directory.

Expected: Total tests 331, Passes 331, Failures 0, Unexpected Passes 0, Expected Failures 0, Missing Tests 0, Extra Tests 0.

- [ ] Step 3: Update only exact encryption manifest rows.

Parse the same-run XML by testid, confirm every encryption N testcase has outcome pass, and update only corresponding encryption rows from failing/blocked/excluded to passing. Remove stale Bead/rationale fields only as required by the manifest schema. Leave unrelated suite rows unchanged.

- [ ] Step 4: Validate the paired artifacts and ledger.

~~~bash
python3 scripts/verify-parity-manifest.py survey/latest/harness.log survey/latest/qtest-results.xml parity/qtest-11.9.0.jsonl
~~~

Expected: exit 0, zero encryption failures, zero blocked encryption rows, and zero excluded encryption rows.

- [ ] Step 5: Commit the qtest ledger.

~~~bash
git -C /tmp/flpdf-qtest add parity/qtest-11.9.0.jsonl
git -C /tmp/flpdf-qtest commit -m "test: promote encryption qtest parity"
~~~

### Task 6: Run quality gates and close the tracked work

- [ ] Step 1: Run formatting and strict static checks.

~~~bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
git diff --check
~~~

Expected: every command exits 0.

- [ ] Step 2: Run all workspace tests.

~~~bash
cargo test --workspace --all-features
~~~

Expected: zero failures in flpdf, flpdf-cli, and flpdf-qtest-tools.

- [ ] Step 3: Run a full qtest survey for regression evidence.

~~~bash
FLPDF_DIR=/home/ubuntu/flpdf/.worktrees/flpdf-61e-r2-encryption QTEST_FULL=1 /tmp/flpdf-qtest-encryption/scripts/run.sh
~~~

Inspect the same-run artifacts and confirm every encryption N row is PASS even if unrelated suites retain pre-existing classified gaps.

- [ ] Step 4: Verify worktrees, Beads, and commits.

~~~bash
git -C /home/ubuntu/flpdf/.worktrees/flpdf-61e-r2-encryption status --short --branch
git -C /tmp/flpdf-qtest-encryption status --short --branch
bd show flpdf-61e flpdf-25kg.5.10 flpdf-25kg.4.12 flpdf-25kg.2.8 --json
bd dep cycles
~~~

Expected: implementation worktrees are clean and bd dep cycles prints No dependency cycles detected.

- [ ] Step 5: Close completed issues and persist Beads.

Close only issues whose acceptance criteria have fresh evidence, then read them back and run:

~~~bash
bd close flpdf-61e flpdf-25kg.5.10 flpdf-25kg.4.12 flpdf-25kg.2.8
bd show flpdf-61e flpdf-25kg.5.10 flpdf-25kg.4.12 flpdf-25kg.2.8 --json
bd dep cycles
bd dolt push
~~~

Expected: close readbacks contain evidence, cycles are absent, and Beads push prints Push complete.

- [ ] Step 6: Push feature branches only after all evidence is green.

~~~bash
git push --set-upstream origin feature/flpdf-61e-r2-permissions
git -C /tmp/flpdf-qtest-encryption push --set-upstream origin feature/flpdf-qtest-encryption
~~~

Do not push main. Retain dirty, detached, or unmerged worktrees until their state is independently verified.

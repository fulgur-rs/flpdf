# CI Workflow YAML Parser Replacement Design

**Issue:** flpdf-qxba.3
**Date:** 2026-07-27

## Goal

Replace the hand-written YAML interpretation in
`crates/flpdf-cli/tests/ci_workflow_contract.rs` with `yaml-rust2`, while
preserving the repository-specific CI contract rules.

The contract must continue to prove that:

- every whole-file `qpdf-zlib-compat` integration test is invoked by CI;
- the `quality` job runs both qpdf module-correspondence checks;
- each module-correspondence command is gating rather than conditional, allowed
  to fail, or run through a custom shell.

YAML syntax is the parser crate's responsibility. This test remains
responsible only for selecting jobs and steps and applying flpdf's policy.

## Dependency Choice

Use `yaml-rust2` 0.11 with default features disabled.

`yaml-rust2` provides a YAML 1.2 tree parser and handles the syntax currently
reimplemented in the test, including:

- quoted mapping keys;
- flow-style mappings and sequences;
- anchors and aliases;
- literal and folded block scalars;
- comments, whitespace, and CRLF input.

The CI workflow is trusted UTF-8 source embedded with `include_str!`, so the
optional non-UTF-8 decoding feature is unnecessary.

Declare the version in `[workspace.dependencies]` and consume it from
`flpdf-cli` under `[dev-dependencies]`. The workflow-contract test is excluded
from the published crate archive, and the dependency is not linked into the
`flpdf` binary or library.

## Parsed Model

Parse each input as exactly one YAML document. The root must be a mapping.
Malformed YAML, multiple documents, or an unexpected root shape is a contract
test error rather than a non-match.

Small tree-access helpers provide:

- string-key lookup in a YAML mapping;
- required mapping or sequence conversion with contextual errors;
- mapping-key presence checks for policy fields;
- scalar extraction for `run`.

The helpers operate on `yaml_rust2::Yaml`; they do not recreate YAML lexical
rules or indentation state.

Two input shapes are supported because the unit tests exercise both:

1. a complete GitHub Actions workflow with `jobs`;
2. a synthetic job body containing `defaults`, `steps`, and job policy fields
   at the document root.

The complete-workflow path selects the `quality` mapping directly through
`jobs["quality"]`. Raw-text job slicing (`workflow_job_key` and
`quality_job_body`) is removed.

## Command Matching and Gating

Command matching only examines parsed `run` scalar values.

For the qpdf module-correspondence checks, a run scalar matches only when its
decoded scalar value, after surrounding whitespace is trimmed, equals the
required command. This preserves the current rejection of shell suffixes,
inactive branches, heredoc bodies, and unrelated commands while naturally
handling YAML literal and folded scalars.

For discovered whole-file `qpdf-zlib-compat` tests, preserve the existing
command-boundary rule: inspect lines inside parsed `run` scalars and accept the
required command only at the beginning of a trimmed line, followed by either
end-of-line or whitespace. These platform-specific tests intentionally live in
a conditional Linux-amd64 step, so this presence contract does not apply the
module-correspondence gating rules. The raw workflow text is never scanned.

A job is gating only when:

- it has no `if` key;
- `continue-on-error` is absent or the YAML boolean `false`;
- neither workflow-level nor job-level `defaults.run.shell` is present.

A step is gating only when:

- it has no `if` key;
- it has no `shell` key;
- `continue-on-error` is absent or the YAML boolean `false`.

The module-correspondence contract succeeds when at least one matching parsed
step is gating and its containing job is gating. Its search is restricted to
the parsed `quality` job.

The compatibility-test presence contract searches parsed `run` scalars in all
jobs. Job and step conditions are allowed because those commands are expected
to be platform-specific; parsing only prevents comments, metadata, and other
YAML fields from satisfying the contract.

The presence of a key is policy-significant even if its value is `null` or an
expression. This is intentionally conservative: an unfamiliar conditional,
failure mode, or shell must not accidentally satisfy a required gate.

## Error Handling

Parsing and structural access return contextual `Result` values. Top-level
contract tests unwrap them with messages that identify the malformed workflow
shape. A valid workflow with no qualifying command remains an ordinary `false`
match so the existing missing-command assertions stay useful.

Individual malformed steps that are not mappings are ignored because GitHub
Actions will reject them independently; mappings with non-string `run` values
do not match.

## Test Strategy

Keep the current behavioral regression tests and adapt them to the parsed
helpers. Add focused cases that prove the crate, rather than local lexical
logic, handles:

- quoted job, `steps`, and `run` keys;
- flow-style job/default/step mappings;
- anchors and aliases;
- literal and folded run scalars;
- CRLF workflows.

Replace the old fail-closed rejection of flow-style
`defaults.run.working-directory` with a positive case. Once the mapping is
actually parsed, a working-directory default is not a custom shell and should
not disable the module-correspondence gate.

Add negative coverage for:

- commands outside `run`;
- commands in another job;
- workflow-level and job-level default shells;
- job- and step-level `if`, `shell`, and `continue-on-error`;
- malformed or multi-document YAML.

Verification runs in increasing scope:

1. `cargo fmt -- --check`;
2. `cargo test -p flpdf-cli --test ci_workflow_contract`;
3. `cargo test -p flpdf-cli`;
4. `cargo test`.

Because dependency metadata changes, also run `cargo package -p flpdf-cli
--allow-dirty` to preserve the workspace-only test exclusion contract.

## Non-Goals

- Deserializing the complete GitHub Actions schema.
- Interpreting shell syntax beyond the existing command-boundary policy.
- Validating arbitrary workflow semantics already checked by GitHub Actions.
- Moving this workspace-only contract test into production code.

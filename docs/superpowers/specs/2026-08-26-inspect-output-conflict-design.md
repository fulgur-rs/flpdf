# Inspect-Only Output Conflict Design

## Goal

Make every top-level flpdf inspection flag that has no output-file
semantics reject a second positional output path, matching qpdf 11.9.0's
`QPDFJob::checkConfiguration` contract.

## Oracle and responsibility

qpdf's `QPDFJob::Config` setters set `require_outfile=false` for
`check`, `checkLinearization`, `listAttachments`, `showAttachment`,
`showEncryption`, `showLinearization`, `showNpages`, `showPages`, `showXref`,
and `showObject` (`libqpdf/QPDFJob_config.cc:72-83,378-382,543-593,766-770`).
The shared `QPDFJob::checkConfiguration` rejects any configured output path
when that flag is false with `no output file may be given for this option`
(`libqpdf/QPDFJob.cc:567-595`). Live qpdf 11.9.0 probes confirmed exit 2 for
each scoped flag with a valid input and a second positional path.

flpdf's top-level `Cli` has one shared `output: Option<PathBuf>` positional.
The parser is the existing clap replacement for qpdf's argument/configuration
boundary. The qpdf-shaped correction is therefore to declare the same
incompatibility in clap, before input dispatch or file mutation.

## Chosen design

Add `conflicts_with = "output"` directly to these eight `Cli` fields:

- `check`
- `show_object`
- `show_npages`
- `show_pages`
- `show_xref`
- `show_linearization`
- `list_attachments`
- `show_attachment`

`check_linearization` and `show_encryption` already have this protection and
remain unchanged. `--is-encrypted` and `--requires-password` are not
top-level `Cli` flags with the shared output positional, so they are outside
this change. No lifecycle, resolver, writer, or UsageError path changes.

The known clap-vs-qpdf usage wording difference remains within the documented
`QPDFArgParser`/`QPDFJob_config` replacement boundary; this slice preserves
the existing parser convention used by `show_encryption` and tests exit 2,
pre-dispatch rejection, and no output creation.

## Tests

Add one integration test per scoped flag in
`crates/flpdf-cli/tests/cli_inspect_output_conflicts.rs`. Each test invokes
the real `flpdf` binary with `tests/fixtures/minimal.pdf` and a unique
temporary output path, then asserts:

- exit code 2;
- clap's conflict diagnostic contains `cannot be used with`;
- stdout is empty; and
- the output path was not created.

The existing successful inspection tests and the existing
`top_level_show_encryption_rejects_output_file` test remain unchanged.

## Non-goals

- Do not rewrite qpdf's diagnostic text through a second CLI validation path.
- Do not add output conflicts to output-producing flags or subcommands.
- Do not change inspection result formatting, warning completion, or file
  opening behavior for valid invocations.
- Do not add a compatibility alias or a new dependency.

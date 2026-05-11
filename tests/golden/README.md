# tests/golden — qpdf Reference Outputs

This directory contains reference PDF outputs produced by **qpdf** and used
as ground-truth fixtures for flpdf compatibility tests.

## qpdf Version

References were generated with:

```text
qpdf version 11.9.0
```

## Regenerating References

Run the script from the repository root:

```bash
bash tests/golden/regenerate.sh
```

The script is idempotent — running it multiple times produces byte-identical
output.  It also regenerates the derived fixtures under
`tests/fixtures/compat/` if they are missing.

## Directory Layout

```text
tests/golden/
  references/
    <fixture-stem>/          # One directory per input fixture
      plain.pdf              # qpdf --deterministic-id <fixture> <out>
      static-id.pdf          # qpdf --static-id <fixture> <out>
      linearize.pdf          # qpdf --linearize --deterministic-id <fixture> <out>
```

> **Note on `plain.pdf`:** The spec defines `plain` as `qpdf <fixture> <out>`,
> but bare qpdf injects a random `/ID` on every run.  All `plain.pdf` references
> therefore use `--deterministic-id` (content-hash-based ID) to make regeneration
> byte-stable.  The `encrypted-r4-three-page/plain.pdf` reference uses
> `--decrypt --static-id` instead, because `--deterministic-id` is incompatible
> with encrypted input.

### Fixture × Flag Matrix

| Fixture                      | plain | static-id | linearize |
|------------------------------|-------|-----------|-----------|
| one-page.pdf                 | ✓     | ✓         | ✓         |
| two-page.pdf                 | ✓     | ✓         | ✓         |
| three-page.pdf               | ✓     | ✓         | ✓         |
| linearized-one-page.pdf      | ✓     |           |           |
| encrypted-r4-three-page.pdf  | ✓     |           |           |
| attachment-two-page.pdf      | ✓     | ✓         |           |

Total: **13 reference files**.

### Notes on Specific References

- **`encrypted-r4-three-page/plain.pdf`**: qpdf decrypts the AES-128 R=4
  encrypted fixture using the empty user password (the default) and writes an
  unencrypted copy.  `--decrypt --static-id` is used instead of
  `--deterministic-id` because `--deterministic-id` is incompatible with
  encrypted input files.
- **`linearized-one-page/plain.pdf`**: qpdf strips the linearisation hints
  when rewriting; the output is a valid but non-linearised PDF.  This is
  expected behaviour.

## Size Policy

**Each reference file must be smaller than 100 KB.**  The `regenerate.sh`
script enforces this limit and exits with an error if any file exceeds it.
Current sizes are all well below 3 KB.

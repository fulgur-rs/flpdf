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

## Overlay / underlay QDF variants (flpdf-9hc.16.13)

The following 3 goldens exercise the `--qdf --no-original-object-ids` writer
path in combination with `--overlay` / `--underlay`. They act as regression
catchers for the library-layer QDF+NoOID+overlay parity. Exact byte parity
against qpdf's `uo-1..uo-8` runtest suite is *not* covered here; that is
validated by `flpdf-qtest`'s `compare-files` runtest steps, which live in a
separate repository to isolate the qtest framework's Artistic 2.0 license
from this tree.

| Golden                                            | Scenario                                                |
|---------------------------------------------------|---------------------------------------------------------|
| `overlay/three-page-overlay-one-page-qdf.pdf`     | single overlay onto page 1 (smallest QDF+overlay)       |
| `overlay/three-page-overlay-and-underlay-qdf.pdf` | overlay + underlay on the same dest pages               |
| `overlay/three-page-two-overlays-qdf.pdf`         | two `--overlay` flags composed left-to-right            |

qpdf command (all three): `qpdf --static-id --qdf --no-original-object-ids ...`.

## Indirect `/Contents` Array QDF marker (flpdf-10de)

`qdf-contents-ref-array/qdf-static-id.pdf` pins a page whose `/Contents`
is an indirect reference to an Array containing two content-stream references.
qpdf emits `%% Contents for page 1` before each stream, but not before the
intermediate Array object. The source streams contain `A\n` and `B\n` as their
actual payloads so this gate remains independent of the non-EOL
`%QDF: ignore_newline` gap tracked by `flpdf-tzgk`.

qpdf command:
`qpdf --static-id --qdf --warning-exit-0 tests/fixtures/compat/qdf-contents-ref-array.pdf tests/golden/references/qdf-contents-ref-array/qdf-static-id.pdf`.

## Non-EOL QDF stream framing (flpdf-tzgk)

`qdf-ignore-newline/qdf-static-id.pdf` pins a metadata stream whose logical
payload is the single byte `A`, with no trailing LF. qpdf adds an LF for stream
framing, writes `%QDF: ignore_newline` immediately before the indirect length
holder, and stores the raw payload length `1` in that holder.

qpdf command:
`qpdf --static-id --qdf --warning-exit-0 tests/fixtures/compat/qdf-ignore-newline.pdf tests/golden/references/qdf-ignore-newline/qdf-static-id.pdf`.

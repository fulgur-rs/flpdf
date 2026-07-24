# qpdf-test-compare fixtures

Small, deterministic PDFs used by the `qpdf-test-compare` crate's end-to-end
test suite (`crates/qpdf-test-compare/tests/e2e.rs`). Each pair exercises a
specific branch of the compare tool's semantics that the unit tests can't
reach at the CLI level (stdout is byte-verbatim, exit codes, argv wiring).

The pairs deliberately target scenarios that the compare tool must
**tolerate** (match paths) or must **surface** (differ paths), so a single
byte-verbatim `assert_eq!` at the CLI boundary is enough to prove each
branch of `main.rs` is wired correctly.

## Regeneration

```sh
bash tests/fixtures/compare_for_test/generate.sh
```

`generate.sh` uses `python3` (stdlib `zlib`) to write every fixture in one
pass, and `bash generate.sh --check` runs `qpdf --check` on all six files as
a sanity gate.

## Licensing

All PDFs in this directory are flpdf-authored derivative works and inherit
the flpdf repository's Apache-2.0/MIT license (via the qpdf-as-tool /
Python-as-tool pattern: the outputs are our own bytes, produced by our own
script). **No file in this directory is copied from qpdf-qtest's `qtest/`
corpus.** That corpus is Artistic 2.0 and lives in a separate repository on
purpose; see `bd recall flpdf-qtest-is-a-separate-repo-specifically-to` for
the rationale.

## Fixture inventory

| Pair | Files | What it exercises |
| --- | --- | --- |
| /ID diff | `id_differs_a.pdf`, `id_differs_b.pdf` | Trailer `/ID[1]` differs (same `/ID[0]`); after `clean_trailer` blanks the second half, both trailers collapse to the same bytes → match, exit 0. |
| FlateDecode compression variance | `flate_miniz.pdf`, `flate_zlib.pdf` | Object 3 is a `/Filter /FlateDecode` stream whose compressed bytes and `/Length` differ across the pair but whose decoded content is identical → match, exit 0. Also proves `/Length` stripping in `compare_streams`. |
| Object-body diff | `differ_body_a.pdf`, `differ_body_b.pdf` | Object 2's `/Count` differs (`0` vs `1`); same shape everywhere else so the trailer compare passes and the per-object walk surfaces `"2 0: object contents differ"` → diff, exit 2. |

The password-plumbing e2e tests reuse `../encrypted/v5-aes-256-r6.pdf`
(password `user-v5-r6`) from the encrypted-fixture directory rather than
duplicating an encrypted PDF here.

# Design: qtest Unicode filenames helper (`flpdf-test-unicode-filenames`)

**Issue**: `flpdf-egzr.1` — Port qpdf 11.9.0 `qpdf/test_unicode_filenames.cc`
as a Rust helper binary consumed by `flpdf-qtest`.

**Roadmap**: `docs/superpowers/specs/2026-07-29-qpdf-observable-parity-roadmap-design.md`,
Phase 1.

---

## 1. Binary

### Name and location

- Cargo binary: `flpdf-test-unicode-filenames`
- Source: `crates/flpdf-qtest-tools/src/bin/unicode_filenames.rs`
- Shim calls this via the name `test_unicode_filenames` (matching qpdf's historical
  test-harness name). The shim itself lives in `flpdf-qtest` and is implemented
  separately.

### Cargo wiring

Add to `crates/flpdf-qtest-tools/Cargo.toml`:

```toml
[[bin]]
name = "flpdf-test-unicode-filenames"
path = "src/bin/unicode_filenames.rs"
```

No library dependency on `flpdf` — the helper is pure file I/O.

### Behaviour (qpdf 11.9.0, Linux path)

Reference: `qpdf/test_unicode_filenames.cc:61–82` (non-Windows `main` + `copy`).

1. Opens `minimal.pdf` in the current directory for reading (`rb`).
2. Creates two output files in the current directory using UTF-8 encoded names:
   - `auto-ü.pdf` → byte sequence `auto-\xc3\xbc.pdf`
   - `auto-öπ.pdf` → byte sequence `auto-\xc3\xb6\xcf\x80.pdf`
3. Copies the input file into both outputs using `do_copy()` — see §2.
4. On success: prints `created Unicode filenames\n` to stdout, exits 0.
5. On any error: prints a message to stderr, exits 2.

The binary takes no arguments and ignores `argv`.

### Error messages (exact qpdf match)

| Condition | stderr | Exit |
|---|---|---|
| Input file cannot be opened, or *either* output file cannot be opened | `errors opening files\n` | 2 |
| Write failure mid-loop (`fwrite` writes fewer bytes than requested, leaving `len > 0` when the loop breaks) | `errors reading or writing\n` | 2 |

---

## 2. Copy algorithm — exact reproduction

Reference: `qpdf/test_unicode_filenames.cc:12–30` (`do_copy`).

```c
char buf[10240];
size_t len = 0;
while ((len = fread(buf, 1, sizeof(buf), in)) > 0) {
    fwrite(buf, 1, len, out);
}
if (len != 0) {
    // error
}
```

The Rust implementation replicates this exactly:

1. Allocate a fixed `[u8; 10240]` buffer.
2. Define a `do_copy(in: &Path, out: &Path)` helper that opens both files, loops
   `in.read(&mut buf)?` → `out.write_all(&buf[..n])?`, and closes both.
3. From `main`, call `do_copy` **twice** — once for `auto-ü.pdf`, once for
   `auto-öπ.pdf` — re-opening `minimal.pdf` each time. This matches qpdf's
   `copy(f1)` → `copy(f2)` sequence (`qpdf/test_unicode_filenames.cc:77–78`)
   where `copy()` calls `fopen("minimal.pdf", "rb")` internally.
4. Read errors are treated like EOF (qpdf does not check `ferror` after `fread` returns 0,
   so a read error silently exits the loop the same way EOF does). The write error
   branch (`fwrite` partial write → `len != 0`) mirrors qpdf's dead code path.

> The buffer size **must** be 10240 — that is part of the observable behaviour
> (it controls the chunk count that could affect error detection points in a
> partial-failure scenario).

---

## 3. Tests

**File**: `crates/flpdf-qtest-tools/tests/unicode_filenames.rs`

**Framework**: `assert_cmd::Command::cargo_bin("flpdf-test-unicode-filenames")`
with `tempfile::TempDir` for per-test working directories.

### Test cases

| # | Name | Setup | Assertions |
|---|---|---|---|
| 1 | Happy path — successful copy | Copy `tests/fixtures/minimal.pdf` into tempdir as `minimal.pdf` | `exit == 0`, `stdout == "created Unicode filenames\n"`, `stderr == ""`, `auto-ü.pdf` and `auto-öπ.pdf` exist and are byte-identical to `minimal.pdf` |
| 2 | Input file missing | Empty tempdir (no `minimal.pdf`) | `exit == 2`, `stderr == "errors opening files\n"`, `auto-ü.pdf` created as empty file (qpdf calls `fopen("wb")` before checking the input handle), `auto-öπ.pdf` not created |
| 3 | Output path is a directory | `minimal.pdf` present, `auto-ü.pdf` exists as a directory | `exit == 2`, `stderr == "errors opening files\n"`, `auto-öπ.pdf` not created |
| 4 | Read/write error mid-copy | *Tested via code path but untestable with regular filesystem — covered by comment only or tested via artificial means (e.g. partial write on a special device).* | `exit == 2`, `stderr == "errors reading or writing\n"` |

### Fixture notes

- Case 3 confirms that attempting to open an existing directory as `"wb"` yields
  `EISDIR`, which Rust surfaces as an I/O error — equivalent to qpdf's `fopen`
  returning `nullptr` with `errno == EISDIR`, caught by the `do_copy`
  `nullptr` check → `"errors opening files"`.
- Case 4 covers the read/write error branch (`len != 0` after loop). In practice
  this cannot be triggered with a regular filesystem; qpdf itself has no
  corresponding harness test for this path. We keep the code branch but accept
  that it may not be covered by filesystem-level integration tests.

### Differential recording

Each test includes a comment citing:
- The qpdf source line range (e.g. `qpdf/test_unicode_filenames.cc:12–82`)
- The expected stdout/stderr/exit as observed from running the actual qpdf
  `test_unicode_filenames` binary in the same conditions

---

## 4. Ownership: flpdf-qtest invocations

The helper is consumed by two `.test` files in `flpdf-qtest`:

| .test file | Subtest | Type |
|---|---|---|
| `unicode-filenames.test` | `create unicode filenames` | `$td->COMMAND => "test_unicode_filenames"` |
| `replace-input.test` | `create unicode filenames` | Same invocation pattern |

Once the binary and shim are in place, both invocations move from
"helper-absent → unable-to-run-command" to executing the Rust port.

---

## 5. Acceptance criteria mapping

| AC | Coverage |
|---|---|
| Cite and reproduce pinned qpdf 11.9.0 behaviour | Design §1–2 + test comments |
| Add Rust helper binary, release-build wiring, PATH shim; remove helper-absent route | Binary + Cargo.toml (§1); shim in flpdf-qtest (separate step) |
| Match stdout, stderr, exit, UTF-8 filenames, copied bytes, file side effects | Test cases 1–4 (§3) |
| Cover successful copies + input-open, output-create/write, usage-independent failure paths | Test cases 1–4 (§3) |
| Record differential commands and expected merged output/exit against pinned qpdf | Test comments (§3) |
| Reach fresh 100% changed executable-line coverage | `cargo llvm-cov` verification post-implementation |
| Record full-survey snapshots before and after with zero allowlist regression | Survey workflow (done after binary + shim are both ready) |

---

## 6. Dependencies and constraints

- **No `flpdf` library dependency**: Pure `std::fs` / `std::io`.
- **No dev-dependency additions needed**: `assert_cmd`, `tempfile`, `predicates`
  are already in `Cargo.toml`.
- **Platform**: Linux x86_64 only (matching the roadmap scope).
- **qpdf-oracle scope**: The Windows `WINDOWS_WMAIN` path in the original source
  is out of scope.

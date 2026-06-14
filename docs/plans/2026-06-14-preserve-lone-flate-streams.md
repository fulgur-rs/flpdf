# Preserve already-lone-/FlateDecode streams (qpdf parity) — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make flpdf's full-rewrite and linearized write paths emit an already
*lone* `/FlateDecode` stream **verbatim** (no decode + re-encode) by default —
matching qpdf — unless an explicit `recompress_flate` opt-in is set. Tracked as
beads `flpdf-9slx`.

**Architecture:** Both write paths already compute
`source_filter_is_lone_flate = is_lone_flate(stream.dict.get("Filter"))` and use
it only for dict key-ordering. We add a `WriteOptions::recompress_flate` field
(default `false`) and, at both stream-emission sites, gate: when the *effective*
policy is `CompressStreams::Yes` **specifically**, the source is a lone
`/FlateDecode`, and `!recompress_flate`, emit the stream verbatim with `/Length`
normalized to `data.len()` instead of calling `apply_stream_compress_policy`.
Plus a CLI `rewrite --recompress-flate` flag.

**Tech Stack:** Rust (edition per workspace), `flate2` (feature
`qpdf-zlib-compat` pins libz level 6 for qpdf byte-parity), `clap` (CLI), qpdf
11.9.0 (golden generation), `cargo llvm-cov` / `scripts/patch-coverage.sh`.

---

## Key facts established during design (do not re-derive)

- **Two call sites share the policy** (both call `apply_stream_compress_policy`):
  - Plain: `crates/flpdf/src/writer.rs` ~2946–2954 (inside the big object loop).
  - Linearized: `crates/flpdf/src/linearization/writer.rs::append_body_object`
    ~459–514 (call at ~481).
- **Gate on `CompressStreams::Yes` specifically**, NOT any `Some(_)`. QDF and
  `--stream-data=uncompress` / `--compress-streams=n` map to
  `CompressStreams::No` and MUST still decode lone-Flate. Preserve-mode is the
  existing `None` arm (already verbatim) — leave it alone.
- **No isDataModified tracker needed.** flpdf's mutation paths
  (coalesce/rotate/appearance/decode) decode → strip `/Filter` → produce
  unfiltered streams, so a stream that is lone-Flate at write time is an
  unmodified original. The only non-writer body-stream path that creates a
  lone-Flate stream is `filespec_helper.rs` (attachments), which uses the same
  `filters::encode_stream_data` backend as `apply_stream_compress_policy`, so
  preserve and re-encode produce identical bytes there (no regression).
- **`/Length` must be normalized** to `data.len()` as a direct integer on the
  preserve path (qpdf writes a direct `/Length`; a source may carry an indirect
  `/Length M 0 R`).
- **dict key order:** for a lone-Flate source, `refiltered` already computes to
  `false` (since `!source_filter_is_lone_flate` is false), so the existing
  serializer writes `/Filter` … `/Length` last (lexicographic) — matching qpdf's
  preserved-stream order. Verified: on HEAD the library full_rewrite output is
  byte-identical to `qpdf --static-id` EXCEPT the stream payload bytes.
- **Avoid cloning stream data** (review rule #1): the linearized site has
  `stream: &Stream`; do not `stream.clone()`. Clone only the (small) dict.

## Fixture facts (verified)

- A large, deterministic content stream is REQUIRED: an 82-byte stream does not
  distinguish zlib level 6 from level 9. The chosen content (600 rectangle-fill
  operators, ~10.9 KB raw) compresses to 1974 bytes at BOTH level 6 and level 9
  but with **different bytes** (verified). Same length is fortuitous: linearized
  offsets/`/L` are identical between re-encode and preserve, so the only diff is
  the stream payload.
- Generation (verified end-to-end):
  `qpdf --recompress-flate --compression-level=9 --object-streams=disable
  --stream-data=compress --static-id` → lone `/FlateDecode` at level 9.
- On HEAD, the library `full_rewrite` path re-encodes (level 6) → payload differs
  from source (RED). Linearized likewise. After preserve → byte-identical.
- Linearized `/ID[1]` does NOT match qpdf for a new fixture (separate
  flpdf-wb76-class concern); the linearized parity test therefore uses the
  existing `mask_id1` structural comparison (which still includes the stream
  bytes, so it is RED on HEAD and GREEN after the fix).

---

## Task 1: Create the lone-Flate-L9 fixture + goldens

**Files:**
- Modify: `tests/golden/regenerate.sh` (add Phase-1 fixture generation + Phase-2
  goldens)
- Create (generated, committed): `tests/fixtures/compat/lone-flate-l9.pdf`
- Create (generated, committed):
  `tests/golden/references/lone-flate-l9/static-id.pdf`,
  `tests/golden/references/lone-flate-l9/linearize.pdf`

**Step 1: Add fixture generation to `regenerate.sh` Phase 1.**

Insert after the `attachment-two-page.pdf` block (before `echo ""` ending
Phase 1):

```bash
if [[ ! -f "$FIX/lone-flate-l9.pdf" ]]; then
    echo "Generating lone-flate-l9.pdf ..."
    # A single-page PDF whose content stream is large enough that zlib level 9
    # and level 6 produce DIFFERENT compressed bytes (a tiny stream would not,
    # giving a false-green parity test). The content is fully deterministic
    # (no RNG). qpdf re-encodes it to a lone /FlateDecode at level 9; flpdf must
    # PRESERVE those bytes verbatim (qpdf's default), so re-encoding at level 6
    # would diverge. --object-streams=disable keeps the content stream as an
    # individual body object (classic xref, no ObjStm) so the parity is direct.
    TMPDIR_L9="$(mktemp -d)"
    trap 'rm -rf "$TMPDIR_L9"' EXIT
    python3 - "$TMPDIR_L9/raw.pdf" <<'PY'
import sys
ops = b""
for i in range(600):
    ops += b"%d %d %d %d re f\n" % (i % 500, (i * 7) % 700, (i * 3) % 40 + 5, (i * 5) % 40 + 5)
def obj(n, body): return b"%d 0 obj\n" % n + body + b"\nendobj\n"
o1 = obj(1, b"<< /Type /Catalog /Pages 2 0 R >>")
o2 = obj(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
o3 = obj(3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R >>")
o4 = obj(4, b"<< /Length %d >>\nstream\n" % len(ops) + ops + b"\nendstream")
body = b"%PDF-1.4\n"
offsets = []
for o in (o1, o2, o3, o4):
    offsets.append(len(body)); body += o
xref_pos = len(body)
body += b"xref\n0 5\n0000000000 65535 f \n"
for off in offsets:
    body += b"%010d 00000 n \n" % off
body += b"trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % xref_pos
open(sys.argv[1], "wb").write(body)
PY
    qpdf --recompress-flate --compression-level=9 --object-streams=disable \
        --stream-data=compress --static-id --warning-exit-0 \
        "$TMPDIR_L9/raw.pdf" "$FIX/lone-flate-l9.pdf"
    trap - EXIT
    rm -rf "$TMPDIR_L9"
else
    echo "Skipping lone-flate-l9.pdf (already exists)"
fi
```

**Step 2: Add goldens to `regenerate.sh` Phase 2.**

Add `"$REF/lone-flate-l9"` to the `mkdir -p` list, then after the
`attachment-two-page` golden block add:

```bash
# --- lone-flate-l9: static-id (plain) + linearize. Both PRESERVE the level-9
#     lone /FlateDecode streams verbatim (qpdf default; no --recompress-flate). ---
qpdf --static-id --warning-exit-0 \
    "$FIX/lone-flate-l9.pdf" "$REF/lone-flate-l9/static-id.pdf"
echo "lone-flate-l9/static-id.pdf"

qpdf --linearize --deterministic-id --warning-exit-0 \
    "$FIX/lone-flate-l9.pdf" "$REF/lone-flate-l9/linearize.pdf"
echo "lone-flate-l9/linearize.pdf"
```

Update the `=== All N references generated ===` count (13 → 15).

**Step 3: Run the script to generate the committed artifacts.**

Run: `bash tests/golden/regenerate.sh`
Expected: prints "Generating lone-flate-l9.pdf", both goldens, all size-check
lines "OK" (the three files are ~1–3 KB, well under 100 KB).

**Step 4: Sanity-check the fixture is a lone /FlateDecode at non-default level.**

Run:
```bash
qpdf --json --json-key=objects tests/fixtures/compat/lone-flate-l9.pdf \
  | grep -i flate
strings tests/fixtures/compat/lone-flate-l9.pdf | grep -E '/Length [0-9]+ /Filter /FlateDecode'
```
Expected: a single `/FlateDecode` content stream (no ASCII85, no DecodeParms).

**Step 5: Commit.**

```bash
git add tests/golden/regenerate.sh tests/fixtures/compat/lone-flate-l9.pdf \
    tests/golden/references/lone-flate-l9/
git commit -m "test(flpdf): add lone-/FlateDecode (level 9) fixture + qpdf goldens (flpdf-9slx)"
```

---

## Task 2: Behavioral preservation tests (RED) — feature-independent

These prove the core behavior without the `qpdf-zlib-compat` feature (after the
fix there is no deflate at all — the single stream is preserved verbatim). They
are the primary, robust proof and run in default CI.

**Files:**
- Create: `crates/flpdf/tests/lone_flate_preserve_tests.rs`
- (No `[[test]]` entry needed — `crates/flpdf/tests/*.rs` are auto-discovered
  unless an explicit `[[test]]` exists; confirm by running the test by name.)

**Step 1: Write the failing tests.**

```rust
//! flpdf preserves an already-lone-`/FlateDecode` stream verbatim under the
//! default compress policy, matching qpdf (which does not recompress a lone
//! Flate stream unless `--recompress-flate` is given). These assert behavior
//! against the SOURCE bytes, so they need no deflate-backend feature: a preserve
//! is a verbatim copy, and a re-encode (the pre-fix behavior) produces different
//! bytes at flpdf's compression level than the level-9 source.

use flpdf::linearization::{write_linearized, LinearizationPlan, RenumberMap};
use flpdf::{
    write_pdf_with_options, CompressStreams, NewlineBeforeEndstream, Pdf, WriteOptions,
};
use std::path::Path;

const FIXTURE: &str = "lone-flate-l9.pdf";

fn fixture_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(FIXTURE)
}

/// Return the bytes of the largest `stream ... endstream` payload — the page
/// content stream in this single-page fixture.
fn largest_stream_payload(data: &[u8]) -> Vec<u8> {
    let needle = b"stream\n";
    let mut best: Vec<u8> = Vec::new();
    let mut i = 0usize;
    while let Some(rel) = data[i..].windows(needle.len()).position(|w| w == needle) {
        let s = i + rel + needle.len();
        let e = s + data[s..]
            .windows(b"endstream".len())
            .position(|w| w == b"endstream")
            .expect("endstream must follow stream");
        if e - s > best.len() {
            best = data[s..e].to_vec();
        }
        i = e + b"endstream".len();
    }
    best
}

fn source_payload() -> Vec<u8> {
    largest_stream_payload(&std::fs::read(fixture_path()).unwrap())
}

fn plain_rewrite(opts: WriteOptions) -> Vec<u8> {
    let mut pdf =
        Pdf::open(std::io::BufReader::new(std::fs::File::open(fixture_path()).unwrap())).unwrap();
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &opts).unwrap();
    out
}

fn base_opts() -> WriteOptions {
    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.static_id = true;
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;
    opts
}

#[test]
fn plain_full_rewrite_preserves_lone_flate_verbatim() {
    let out = plain_rewrite(base_opts());
    assert_eq!(
        largest_stream_payload(&out),
        source_payload(),
        "default compress policy must preserve a lone /FlateDecode stream verbatim"
    );
}

#[test]
fn linearized_preserves_lone_flate_verbatim() {
    let mut pdf =
        Pdf::open(std::io::BufReader::new(std::fs::File::open(fixture_path()).unwrap())).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();
    let renumber = RenumberMap::from_plan(&plan);
    let mut pdf2 =
        Pdf::open(std::io::BufReader::new(std::fs::File::open(fixture_path()).unwrap())).unwrap();
    let mut opts = WriteOptions::default();
    opts.deterministic_id = true;
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;
    let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap();
    doc.back_patch().unwrap();
    assert_eq!(
        largest_stream_payload(&doc.bytes),
        source_payload(),
        "linearized output must preserve a lone /FlateDecode stream verbatim"
    );
}

#[test]
fn recompress_flate_reencodes_lone_flate() {
    let mut opts = base_opts();
    opts.recompress_flate = true;
    let out = plain_rewrite(opts);
    let payload = largest_stream_payload(&out);
    assert_ne!(
        payload,
        source_payload(),
        "recompress_flate=true must re-encode the lone /FlateDecode stream"
    );
    // It must still be a single /FlateDecode (re-encoded), not raw bytes.
    assert!(
        out.windows(b"/Filter /FlateDecode".len())
            .any(|w| w == b"/Filter /FlateDecode"),
        "re-encoded stream must still declare a single /FlateDecode filter"
    );
}

#[test]
fn uncompress_policy_decodes_lone_flate() {
    // The preserve gate is CompressStreams::Yes-specific: under Uncompress the
    // lone /FlateDecode must be decoded (no /Filter), not preserved.
    let mut opts = base_opts();
    opts.compress_streams = CompressStreams::No;
    let out = plain_rewrite(opts);
    let payload = largest_stream_payload(&out);
    // Decoded content is the raw rectangle operators ("re f" ops), much larger
    // than the ~1974-byte compressed source and not equal to it.
    assert_ne!(payload, source_payload());
    assert!(
        payload.windows(4).any(|w| w == b"re f"),
        "Uncompress must emit decoded raw content, not preserved compressed bytes"
    );
}
```

**Step 2: Run to verify they FAIL for the right reason.**

Run:
`cargo test -p flpdf --test lone_flate_preserve_tests`
Expected (on the pre-fix tree, but `recompress_flate` field does not exist yet →
COMPILE error). That is acceptable as the first RED, but to see a *behavioral*
RED first, temporarily comment out the two tests that reference
`recompress_flate` and run the other two:
- `plain_full_rewrite_preserves_lone_flate_verbatim` → FAIL (payload re-encoded).
- `uncompress_policy_decodes_lone_flate` → PASS (already decodes).
Then uncomment. The compile error is resolved in Task 3.

**Step 3: Commit the tests (still red).**

```bash
git add crates/flpdf/tests/lone_flate_preserve_tests.rs
git commit -m "test(flpdf): RED — lone-/FlateDecode preservation behavior (flpdf-9slx)"
```

---

## Task 3: Add `recompress_flate` + gate the plain path

**Files:**
- Modify: `crates/flpdf/src/writer.rs` (WriteOptions field; gate at ~2947)

**Step 1: Add the `recompress_flate` field to `WriteOptions`.**

Place it directly after the `stream_data` field (~line 362), matching the
doc-comment style of neighbours. English only (public doc), no issue IDs.

```rust
    /// Re-encode streams that are already a lone `/FlateDecode`.
    ///
    /// By default (`false`) a stream whose source filter is a single
    /// `/FlateDecode` is emitted **verbatim** under [`CompressStreams::Yes`] —
    /// its already-compressed bytes are preserved rather than decoded and
    /// re-encoded. This mirrors qpdf, which does not recompress a lone-Flate
    /// stream unless `--recompress-flate` is given.
    ///
    /// Set to `true` to force such streams through a decode + re-encode pass
    /// (equivalent to `qpdf --recompress-flate`). Has no effect under
    /// [`CompressStreams::No`] / [`StreamDataMode::Uncompress`] (which always
    /// decode) or [`StreamDataMode::Preserve`] (which never decodes).
    pub recompress_flate: bool,
```

**Step 2: Gate the plain emission (writer.rs ~2946–2954).**

Replace the `match effective_stream_policy(options)` that builds `reencoded`:

```rust
            let source_filter_is_lone_flate = is_lone_flate(stream.dict.get("Filter"));
            let mut reencoded = match effective_stream_policy(options) {
                // qpdf preserves an already-lone-/FlateDecode stream verbatim
                // under the compress policy (no decode + re-encode) unless
                // recompression is explicitly requested. Normalize /Length to
                // the raw data length (a source may carry an indirect /Length).
                Some(CompressStreams::Yes)
                    if source_filter_is_lone_flate && !options.recompress_flate =>
                {
                    let mut stream = stream;
                    let len = i64::try_from(stream.data.len()).unwrap_or(i64::MAX);
                    stream.dict.insert("Length", Object::Integer(len));
                    Object::Stream(stream)
                }
                Some(compress_policy) => apply_stream_compress_policy(&stream, compress_policy),
                // Preserve mode: pass dict + raw bytes verbatim, no decode/re-encode.
                None => Object::Stream(stream),
            };
```

(The existing `refiltered` computation at ~3053 already yields `false` for a
lone-Flate source, so the preserved stream serializes with `/Length` last in
lexicographic order — matching qpdf. No change needed there.)

**Step 3: Build + run the plain behavioral tests.**

Run:
`cargo test -p flpdf --test lone_flate_preserve_tests \
  plain_full_rewrite_preserves_lone_flate_verbatim recompress_flate_reencodes_lone_flate uncompress_policy_decodes_lone_flate`
Expected: all PASS. (`linearized_*` still FAILS — Task 4.)

**Step 4: Commit.**

```bash
git add crates/flpdf/src/writer.rs
git commit -m "feat(flpdf): preserve lone-/FlateDecode verbatim on plain rewrite; add recompress_flate (flpdf-9slx)"
```

---

## Task 4: Gate the linearized path

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs::append_body_object` (~471+)

**Step 1: Add the preserve branch in `append_body_object`.**

After computing `source_filter_is_lone_flate` (~480) and BEFORE
`let reencoded = apply_stream_compress_policy(stream, policy);`, insert:

```rust
    // qpdf preserves an already-lone-/FlateDecode stream verbatim under the
    // compress policy (no decode + re-encode) unless recompression is requested.
    // Emit the dict (lexicographic, /Length last — `refiltered = false`) with
    // /Length normalized to the raw data length, then the data verbatim. Clone
    // only the (small) dict, never the stream data.
    if matches!(policy, CompressStreams::Yes)
        && source_filter_is_lone_flate
        && !options.recompress_flate
    {
        let offset = bytes.len();
        bytes.extend_from_slice(
            format!("{} {} obj\n", new_ref.number, new_ref.generation).as_bytes(),
        );
        let mut dict = stream.dict.clone();
        let len = i64::try_from(stream.data.len()).unwrap_or(i64::MAX);
        dict.insert("Length", Object::Integer(len));
        crate::writer::write_preserved_stream(bytes, &dict, &stream.data);
        bytes.extend_from_slice(b"\nendobj\n");
        return offset;
    }
```

**Step 2: Add the `write_preserved_stream` helper in `writer.rs`.**

A small `pub(crate)` choke-point so the linearized path can emit a preserved
stream without cloning the data buffer. Place it near
`write_stream_to_buf_qpdf_order` (~3576). It serializes the dict in qpdf's
stream-dict key order (`refiltered = false`: sorted keys, `/Length` last) and
frames the payload with `NewlineBeforeEndstream::Never` (qpdf's linearized body
framing). The helper lives in `writer.rs`, so it calls the (private)
`write_stream_payload` directly — no visibility change needed; `write_pdf_stream`
is already `pub(crate)`.

```rust
/// Emit a preserved (verbatim) stream body: the `dict` in qpdf's stream-dict key
/// order (`/Length` pulled out and written last; no re-filtering) followed by the
/// raw `data` framed with no newline before `endstream`. Used to emit an
/// already-lone-/FlateDecode stream without decode + re-encode. The caller is
/// responsible for setting `dict`'s `/Length` to `data.len()`.
pub(crate) fn write_preserved_stream(buf: &mut Vec<u8>, dict: &Dictionary, data: &[u8]) {
    dict.write_pdf_stream(buf, false);
    write_stream_payload(buf, data, NewlineBeforeEndstream::Never);
}
```

Confirm imports: `CompressStreams` is already imported in
`linearization/writer.rs:75`; `Object` is in scope. Add
`write_preserved_stream` only if you choose the helper (the
`crate::writer::write_preserved_stream` path avoids a new import).

**Step 3: Build + run all behavioral tests.**

Run: `cargo test -p flpdf --test lone_flate_preserve_tests`
Expected: all 4 PASS (including `linearized_preserves_lone_flate_verbatim`).

**Step 4: Commit.**

```bash
git add crates/flpdf/src/linearization/writer.rs crates/flpdf/src/writer.rs
git commit -m "feat(flpdf): preserve lone-/FlateDecode verbatim on linearized path (flpdf-9slx)"
```

---

## Task 5: qpdf byte-parity tests (RED→GREEN) under qpdf-zlib-compat

**Files:**
- Modify: `crates/flpdf/tests/cmp_diff_zero_tests.rs` (plain strict)
- Modify: `crates/flpdf/tests/cmp_linearize_tests.rs` (linearized structural)

**Step 1: Plain strict byte-identity.**

Append to `cmp_diff_zero_tests.rs`:

```rust
#[test]
fn lone_flate_l9_plain_rewrite_is_byte_identical_to_qpdf_static_id() {
    // A lone /FlateDecode source compressed at level 9: flpdf must preserve the
    // bytes verbatim (qpdf default), so re-encoding at level 6 would diverge.
    assert_cmp_diff_zero("lone-flate-l9.pdf", "lone-flate-l9");
}
```

**Step 2: Linearized structural byte-identity (masks /ID[1] only).**

Append to `cmp_linearize_tests.rs` (uses the existing
`assert_linearize_structurally_byte_identical`; `/ID[1]` is a separate
flpdf-wb76-class divergence for new fixtures, so mask it — the comparison still
includes the preserved stream bytes):

```rust
#[test]
fn lone_flate_l9_linearized_structurally_byte_identical_to_qpdf() {
    assert_linearize_structurally_byte_identical("lone-flate-l9.pdf", "lone-flate-l9");
}
```

**Step 3: Verify RED on the pre-fix tree, GREEN now.** Since the fix is already
in (Tasks 3–4), confirm these PASS:

Run:
`cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests --test cmp_linearize_tests`
Expected: all PASS, including the two new tests.

To *demonstrate* they would have caught the bug (optional, recommended): `git
stash` the writer changes is not possible (committed) — instead temporarily set
`recompress_flate`-equivalent by reverting the gate locally, confirm RED, then
restore. If skipping, note in the PR that RED-on-HEAD was verified during design
via probe (payload differed: level 6 vs level 9).

**Step 4: Commit.**

```bash
git add crates/flpdf/tests/cmp_diff_zero_tests.rs crates/flpdf/tests/cmp_linearize_tests.rs
git commit -m "test(flpdf): qpdf byte-parity for preserved lone-/FlateDecode (plain+linearized) (flpdf-9slx)"
```

---

## Task 6: CLI `rewrite --recompress-flate` flag

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs` (RewriteCmd arg + wiring)
- Modify/Create: a CLI test (best-effort; flpdf-cli coverage is report-only)

**Step 1: Add the clap arg to `RewriteCmd`** (after `stream_data`, ~line 1053):

```rust
    /// Re-encode streams that are already a lone /FlateDecode (default: preserve
    /// them verbatim, matching qpdf). Mirrors `qpdf --recompress-flate`.
    #[arg(long = "recompress-flate")]
    recompress_flate: bool,
```

**Step 2: Wire it into `WriteOptions`** (in the RewriteCmd → options section,
after the `stream_data` mapping ~line 1934):

```rust
    options.recompress_flate = cmd.recompress_flate;
```

**Step 3: Add a CLI test** (best-effort) asserting `rewrite --full-rewrite
--static-id` on `lone-flate-l9.pdf` preserves the content stream, while adding
`--recompress-flate` changes it. Follow the existing CLI test harness in
`crates/flpdf-cli/tests/` (locate a representative `rewrite` test and mirror it).
If the harness invokes the built binary, compare the largest stream payload to
the fixture's.

**Step 4: Build + run.**

Run: `cargo test -p flpdf-cli`
Expected: PASS (and `cargo build -p flpdf-cli` clean).

**Step 5: Commit.**

```bash
git add crates/flpdf-cli/
git commit -m "feat(flpdf-cli): add rewrite --recompress-flate (opt into recompressing lone /FlateDecode) (flpdf-9slx)"
```

---

## Task 7: Quality gates

**Step 1: Audit existing lone-Flate-sensitive tests for shifts.**

Run the full suite both ways:
```bash
cargo test -p flpdf
cargo test -p flpdf --features qpdf-zlib-compat
cargo test -p flpdf-cli
```
Expected: all green. Pay attention to `stream_dict_length_first_tests`
(`already_flate_source_keeps_length_last` — asserts ordering, should still hold)
and any attachment / embedded-file tests (attachments now preserved, but
identical bytes — see design note). If any test asserted on *re-encoded*
lone-Flate `/Length` or bytes, update it to reflect preservation and note why.

**Step 2: fmt + clippy.**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --features qpdf-zlib-compat -- -D warnings
```
(See memory: CI Quality gate runs `cargo fmt --check`; the `repeat_n` MSRV rule.)

**Step 3: doc check** (public doc rules): no beads IDs / internal jargon /
Japanese in `///`/`//!`; the new `recompress_flate` doc uses spec/behavior
language and intra-doc links (`[`CompressStreams`]` etc.).
```bash
cargo doc -p flpdf --no-deps
```

**Step 4: Patch coverage (PR gate).** Commit everything first, then:
```bash
cargo llvm-cov --workspace --features qpdf-zlib-compat --lcov --output-path /tmp/9slx.lcov
scripts/patch-coverage.sh --base main --lcov /tmp/9slx.lcov
```
Expected: flpdf changed lines 100% covered. The gate condition's branches are
covered by Task 2 tests (preserve, recompress=true, Uncompress-decodes, and the
existing ASCII85 one-page tests exercise the not-lone-Flate fall-through). If any
new line is uncovered, add a test (do NOT use `cov:ignore` unless truly
untestable, with a reason in the PR).

**Step 5: qualitative check.** Confirm error-arm / boundary / extreme-input
coverage is meaningful (not just line-executed): preserve vs re-encode vs decode
are all asserted on real bytes; the `recompress_flate` opt-in is exercised both
directions.

---

## Acceptance checklist (from beads flpdf-9slx)

- [ ] `flpdf rewrite` (plain) of a lone-/FlateDecode (non-default level) input is
  byte-identical to `qpdf --static-id` (Task 5 plain test).
- [ ] `flpdf rewrite --linearize` is byte-identical (structurally, `/ID[1]`
  masked) to `qpdf --linearize --deterministic-id` (Task 5 linearized test).
- [ ] Preservation holds verbatim, feature-independent (Task 2).
- [ ] `--recompress-flate` opt-in re-enables recompression (Tasks 2, 6).
- [ ] No isDataModified tracker introduced; no stream-data clone added.
- [ ] flpdf changed-line coverage 100%; fmt/clippy/doc clean.

## Out of scope (file follow-ups if surfaced)

- Linearized `/ID[1]` byte-parity for arbitrary fixtures (flpdf-wb76 class).
- Document-wide opt-in decode limits (flpdf-roq0) — unrelated.

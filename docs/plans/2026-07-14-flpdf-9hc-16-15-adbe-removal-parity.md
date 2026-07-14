# ADBE Extension Removal Trigger (qpdf L1406-1436 parity) — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Broaden the `/Extensions /ADBE` strip trigger in `write_pdf_full_rewrite` to match qpdf's `have_extensions_adbe` semantics (any `/ADBE` key, regardless of `/ExtensionLevel` shape), and add byte-gate tests proving parity with `qpdf` 11.9.0 for both the "whole `/Extensions` removed" and "`/ADBE` removed, non-ADBE prefix preserved" branches.

**Architecture:** Add a `pub(crate)` helper `catalog_has_extensions_adbe(pdf)` next to `strip_adbe_extension` in `crates/flpdf/src/writer.rs`. Swap the trigger at `writer.rs:3018` from `source_ext > 0` to `catalog_has_extensions_adbe(pdf)?`. Existing `strip_adbe_extension` code is already correct (handles both whole-removal and partial-removal via the empty-check). Add 2 hand-crafted fixture PDFs + 2 qpdf-generated goldens + 1 new byte-gate test file. Preserve existing regression tests unchanged.

**Tech Stack:** Rust, `flate2` (miniz_oxide by default), `qpdf` 11.9.0 (fixture/golden generation), Python 3 (fixture hand-crafting in `regenerate.sh`).

**Beads issue:** flpdf-9hc.16.15 (parent epic: flpdf-9hc.16 Overlay/underlay, root epic: flpdf-9hc qpdf-equivalent FLPDF).

---

## Task 1: Add unit test for whole-`/Extensions`-removal (source /ADBE without /ExtensionLevel)

**Files:**
- Modify: `crates/flpdf/src/writer.rs` (add test in the existing `#[cfg(test)] mod tests` block)

**Step 1: Write the failing test**

Locate the existing `write_pdf_full_rewrite_generate_mode_strips_stale_adbe_on_floor_bump` test in `crates/flpdf/src/writer.rs` (around line 7461). Add these two new tests immediately after `write_pdf_full_rewrite_v4_aes128_source_1_3_with_adbe_strips_stale_ext` (around line 7780, before the closing `}` of `mod tests`):

```rust
#[test]
fn write_pdf_full_rewrite_strips_stale_adbe_when_source_has_no_extension_level() {
    // qpdf QPDFWriter.cc L1387/L1408 (whole /Extensions removed) parity:
    // source /Extensions /ADBE dict has no /ExtensionLevel (or non-integer).
    // adobe_extension_level() returns None → source_ext = 0. The pre-broadening
    // trigger (`source_ext > 0`) would skip strip and let /ADBE pass through;
    // the broadened trigger (`catalog_has_extensions_adbe`) fires and drops
    // /Extensions entirely because /ADBE is the only key.
    let mut src = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    offsets.push(src.len());
    src.extend_from_slice(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
          /Extensions << /ADBE << /BaseVersion /1.4 >> >> >>\nendobj\n",
    );
    offsets.push(src.len());
    src.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
    let startxref = src.len();
    let count = offsets.len() + 1;
    src.extend_from_slice(format!("xref\n0 {count}\n0000000000 65535 f \n").as_bytes());
    for off in &offsets {
        src.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    src.extend_from_slice(
        format!(
            "trailer\n<< /Size {count} /Root 1 0 R >>\n\
             startxref\n{startxref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    // Sanity: adobe_extension_level() returns None because /ExtensionLevel is absent.
    {
        let mut src_pdf = crate::Pdf::open_mem(&src).expect("source must open");
        assert_eq!(src_pdf.adobe_extension_level(), None);
    }
    let options = WriteOptions {
        full_rewrite: true,
        static_id: true,
        ..WriteOptions::default()
    };
    let out = write_full_rewrite_with(&src, &options);
    let mut reopened = crate::Pdf::open_mem_owned(out).expect("output must open");
    // The whole /Extensions dict must be gone from the output Catalog.
    let root_ref = reopened.trailer().get_ref("Root").expect("Root ref");
    let catalog = reopened
        .resolve(root_ref)
        .expect("resolve root")
        .into_dict()
        .expect("root is dict");
    assert!(
        catalog.get("Extensions").is_none(),
        "stale /ADBE without /ExtensionLevel must trigger whole-/Extensions removal: {catalog:?}"
    );
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf --lib write_pdf_full_rewrite_strips_stale_adbe_when_source_has_no_extension_level 2>&1 | tail -20`

Expected: FAIL with an assertion failure showing `/Extensions` still present in Catalog.

**Step 3: Commit the failing test**

```bash
git add crates/flpdf/src/writer.rs
git commit -m "test(flpdf): failing test for /ADBE strip when source lacks /ExtensionLevel"
```

---

## Task 2: Add unit test for partial /ADBE removal (non-ADBE prefix preserved)

**Files:**
- Modify: `crates/flpdf/src/writer.rs` (add second test in the same test module)

**Step 1: Write the failing test**

Append immediately after the test from Task 1:

```rust
#[test]
fn write_pdf_full_rewrite_strips_stale_adbe_no_ext_level_preserving_vendor_prefix() {
    // qpdf QPDFWriter.cc L1432 (removeKey /ADBE, keep other extensions) parity:
    // source /Extensions has /ADBE without /ExtensionLevel AND a non-ADBE
    // developer prefix (/XYZW). Broadened trigger must fire and strip /ADBE
    // only, leaving /XYZW intact.
    let mut src = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    offsets.push(src.len());
    src.extend_from_slice(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
          /Extensions << /ADBE << /BaseVersion /1.4 >> \
          /XYZW << /BaseVersion /1.4 /ExtensionLevel 1 >> >> >>\nendobj\n",
    );
    offsets.push(src.len());
    src.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
    let startxref = src.len();
    let count = offsets.len() + 1;
    src.extend_from_slice(format!("xref\n0 {count}\n0000000000 65535 f \n").as_bytes());
    for off in &offsets {
        src.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    src.extend_from_slice(
        format!(
            "trailer\n<< /Size {count} /Root 1 0 R >>\n\
             startxref\n{startxref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    // Sanity: adobe_extension_level() returns None (source /ExtensionLevel absent).
    {
        let mut src_pdf = crate::Pdf::open_mem(&src).expect("source must open");
        assert_eq!(src_pdf.adobe_extension_level(), None);
    }
    let options = WriteOptions {
        full_rewrite: true,
        static_id: true,
        ..WriteOptions::default()
    };
    let out = write_full_rewrite_with(&src, &options);
    let mut reopened = crate::Pdf::open_mem_owned(out).expect("output must open");
    let root_ref = reopened.trailer().get_ref("Root").expect("Root ref");
    let catalog = reopened
        .resolve(root_ref)
        .expect("resolve root")
        .into_dict()
        .expect("root is dict");
    // /Extensions must still be present, containing only /XYZW.
    let ext_dict = catalog
        .get("Extensions")
        .expect("/Extensions must survive because /XYZW is present")
        .as_dict()
        .expect("/Extensions must be a direct dict after strip");
    assert!(
        ext_dict.get("ADBE").is_none(),
        "stale /ADBE without /ExtensionLevel must be stripped: {ext_dict:?}"
    );
    assert!(
        ext_dict.get("XYZW").is_some(),
        "non-ADBE developer prefix must survive: {ext_dict:?}"
    );
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf --lib write_pdf_full_rewrite_strips_stale_adbe_no_ext_level_preserving_vendor_prefix 2>&1 | tail -20`

Expected: FAIL with assertion `stale /ADBE without /ExtensionLevel must be stripped: {...}` (dict shows /ADBE still present).

**Step 3: Commit the failing test**

```bash
git add crates/flpdf/src/writer.rs
git commit -m "test(flpdf): failing test for /ADBE strip preserving non-ADBE prefix"
```

---

## Task 3: Implement `catalog_has_extensions_adbe` helper + swap trigger

**Files:**
- Modify: `crates/flpdf/src/writer.rs` — add helper next to `strip_adbe_extension` (around line 836, after the closing `}` of `strip_adbe_extension`); swap trigger at line 3018.

**Step 1: Add the new helper**

Insert directly after `strip_adbe_extension`'s closing `}` (currently line 836 — verify by grepping `fn strip_adbe_extension` and finding its end):

```rust
/// Detect whether the destination Catalog carries `/Extensions /ADBE` in any
/// form (dict-valued or via indirect reference; regardless of `/ExtensionLevel`
/// presence or value).
///
/// Mirrors qpdf's `have_extensions_adbe = keys.count("/ADBE") > 0` check
/// (QPDFWriter.cc L1387). Used as the strip trigger for `eff_ext == 0`: when
/// the effective extension level is zero, qpdf removes stale `/ADBE` whether
/// or not the source dict carried a valid `/ExtensionLevel`; the previous
/// `adobe_extension_level() > 0` gate only fired for positive integer
/// `/ExtensionLevel` and silently passed through malformed / partial /ADBE
/// entries.
///
/// # Errors
///
/// - Propagates [`Pdf::resolve`] errors when materialising the Catalog or an
///   indirect `/Extensions` value.
fn catalog_has_extensions_adbe<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<bool> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(false);
    };
    let catalog = pdf.resolve(root_ref)?;
    let Some(catalog_dict) = catalog.as_dict() else {
        return Ok(false);
    };
    let Some(extensions_val) = catalog_dict.get("Extensions").cloned() else {
        return Ok(false);
    };
    let extensions = match extensions_val {
        Object::Dictionary(d) => d,
        Object::Reference(r) => match pdf.resolve(r)?.into_dict() {
            Some(d) => d,
            None => return Ok(false),
        },
        _ => return Ok(false),
    };
    Ok(extensions.get("ADBE").is_some())
}
```

**Step 2: Swap the trigger**

Find the block at `crates/flpdf/src/writer.rs:3016-3025`:

```rust
if eff_ext > 0 {
    inject_adbe_extension(pdf, eff_ver, eff_ext)?;
} else if source_ext > 0 {
    // Source carried an ADBE extension level that did not survive the
    // pairwise version race (min_version bump or ObjStm floor drops
    // it to 0). Removing the stale /ADBE entry from the destination
    // Catalog keeps `/BaseVersion` from contradicting the emitted
    // header version.
    strip_adbe_extension(pdf)?;
}
```

Replace with:

```rust
if eff_ext > 0 {
    inject_adbe_extension(pdf, eff_ver, eff_ext)?;
} else if catalog_has_extensions_adbe(pdf)? {
    // qpdf QPDFWriter.cc L1387/L1408/L1432: when the effective extension
    // level is 0, any `/Extensions /ADBE` key must be removed —
    // whether from a prior injection that lost the pairwise version race
    // (min_version bump / ObjStm floor drops the ext to 0) or from a
    // stale/malformed source /ADBE without a valid /ExtensionLevel.
    // strip_adbe_extension handles both branches: it drops /Extensions
    // when nothing else remains, otherwise keeps it with the non-ADBE
    // developer prefixes intact.
    strip_adbe_extension(pdf)?;
}
```

Note: `source_ext` is still computed above (used only for the strip gate that we just replaced) but is no longer read after this change. Verify by grepping `source_ext` in the surrounding block. If unused, remove the local binding entirely; but if `source_ext` is used elsewhere in the block, keep it. Check with:

```bash
grep -n "source_ext" crates/flpdf/src/writer.rs | head -10
```

If `source_ext` is only referenced in the injection block: delete the `let source_ext = ...` line inside the anonymous scope (around line 2997).

**Step 3: Also update `strip_adbe_extension` doc**

Locate `strip_adbe_extension`'s doc comment (currently around line 782):

Replace the first paragraph:

```rust
/// Strip `/Extensions /ADBE` from the destination Catalog when the effective
/// extension level is 0. This complements [`inject_adbe_extension`]: when a
/// version race (min_version bump or ObjStm floor) drops the pairwise ext to
/// 0 but the source Catalog carries a stale `/ADBE` entry, that stale entry
/// would otherwise survive the renumber walk and produce an output whose
/// Catalog `/BaseVersion` contradicts the emitted header version.
```

with:

```rust
/// Strip `/Extensions /ADBE` from the destination Catalog when the effective
/// extension level is 0. This complements [`inject_adbe_extension`] and
/// mirrors qpdf's removal branches (QPDFWriter.cc L1408 whole-`/Extensions`
/// removal and L1432 `/ADBE`-only removal). Fires for two related cases:
/// (1) a version race (min_version bump or ObjStm floor) drops the pairwise
/// ext to 0 but the source Catalog carries an `/ADBE` entry that would
/// otherwise survive; (2) the source Catalog carries a stale / malformed
/// `/ADBE` (no `/ExtensionLevel` or non-integer) even without a race — qpdf
/// removes it based on key existence, not `/ExtensionLevel` validity, so
/// flpdf must match to preserve byte parity.
```

**Step 4: Build and run the two new unit tests**

Run:

```bash
cargo build -p flpdf --lib 2>&1 | tail -5
cargo test -p flpdf --lib write_pdf_full_rewrite_strips_stale_adbe 2>&1 | tail -20
```

Expected: 2 new tests pass + existing `strips_stale_adbe_*` tests still pass. Also verify the existing tests that guard the trigger-side semantics still pass:

```bash
cargo test -p flpdf --lib write_pdf_full_rewrite_does_not_leave_root_dirty_flag_set write_pdf_full_rewrite_preserves_pre_existing_root_dirty_flag write_pdf_full_rewrite_does_not_dirty_caller_pdf_across_writes 2>&1 | tail -20
```

Expected: all pass.

**Step 5: Run the full lib test suite as regression check**

```bash
cargo test -p flpdf --lib --quiet 2>&1 | tail -5
```

Expected: all tests pass (baseline was 1958 passing; expect 1960 with the two new tests).

**Step 6: Commit**

```bash
git add crates/flpdf/src/writer.rs
git commit -m "feat(flpdf): broaden /ADBE strip trigger to any /ADBE key (qpdf L1387 parity)"
```

---

## Task 4: Add fixtures to `tests/golden/regenerate.sh`

**Files:**
- Modify: `tests/golden/regenerate.sh`

**Step 1: Locate insertion point**

Find the section for hn1g.15 (remove-restrictions fixtures) at the tail of the file — the new blocks should go before the golden-generation Phase 2 (or wherever new fixtures are typically inserted; check with `grep -n "one-page-adbe\|adbe-strip\|remove-restrictions" tests/golden/regenerate.sh`). Add the two `if [[ ! -f ... ]]; then ... fi` blocks in Phase 1 (fixture generation phase, before goldens are produced).

**Step 2: Insert Fixture 1 generation**

Add to `tests/golden/regenerate.sh`:

```bash
# --- flpdf-9hc.16.15: /Extensions /ADBE removal parity fixtures ---
# Hand-crafted source PDFs with malformed /ADBE (no /ExtensionLevel) so the
# strip trigger fires on plain rewrite. qpdf's CLI cannot inject an ADBE dict
# without a valid /ExtensionLevel, so we build the raw bytes directly.
# Content-stream-free → deflate-independent (no --features qpdf-zlib-compat
# needed to verify byte parity).

if [[ ! -f "$FIX/one-page-stale-adbe-no-ext.pdf" ]]; then
    echo "Generating one-page-stale-adbe-no-ext.pdf ..."
    python3 - "$FIX/one-page-stale-adbe-no-ext.pdf" <<'PY'
import sys
out_path = sys.argv[1]
body = bytearray(b"%PDF-1.4\n%\xbf\xf7\xa2\xfe\n")
objs = [
    b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R "
    b"/Extensions << /ADBE << /BaseVersion /1.4 >> >> >>\nendobj\n",
    b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n",
    b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> >>\nendobj\n",
]
offsets = []
for o in objs:
    offsets.append(len(body))
    body.extend(o)
startxref = len(body)
size = len(objs) + 1
body.extend(f"xref\n0 {size}\n0000000000 65535 f \n".encode())
for off in offsets:
    body.extend(f"{off:010} 00000 n \n".encode())
body.extend(
    f"trailer\n<< /Size {size} /Root 1 0 R >>\n"
    f"startxref\n{startxref}\n%%EOF\n".encode()
)
open(out_path, "wb").write(bytes(body))
PY
else
    echo "Skipping one-page-stale-adbe-no-ext.pdf (already exists)"
fi

if [[ ! -f "$FIX/one-page-stale-adbe-no-ext-vendor.pdf" ]]; then
    echo "Generating one-page-stale-adbe-no-ext-vendor.pdf ..."
    python3 - "$FIX/one-page-stale-adbe-no-ext-vendor.pdf" <<'PY'
import sys
out_path = sys.argv[1]
body = bytearray(b"%PDF-1.4\n%\xbf\xf7\xa2\xfe\n")
objs = [
    b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R "
    b"/Extensions << /ADBE << /BaseVersion /1.4 >> "
    b"/XYZW << /BaseVersion /1.4 /ExtensionLevel 1 >> >> >>\nendobj\n",
    b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n",
    b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> >>\nendobj\n",
]
offsets = []
for o in objs:
    offsets.append(len(body))
    body.extend(o)
startxref = len(body)
size = len(objs) + 1
body.extend(f"xref\n0 {size}\n0000000000 65535 f \n".encode())
for off in offsets:
    body.extend(f"{off:010} 00000 n \n".encode())
body.extend(
    f"trailer\n<< /Size {size} /Root 1 0 R >>\n"
    f"startxref\n{startxref}\n%%EOF\n".encode()
)
open(out_path, "wb").write(bytes(body))
PY
else
    echo "Skipping one-page-stale-adbe-no-ext-vendor.pdf (already exists)"
fi
```

**Step 3: Insert golden generation blocks**

Find Phase 2 (golden generation) or an equivalent section that iterates fixture stems and generates goldens. Add:

```bash
# --- flpdf-9hc.16.15: qpdf oracle for /Extensions /ADBE removal branches ---
for stem in one-page-stale-adbe-no-ext one-page-stale-adbe-no-ext-vendor; do
    mkdir -p "$REF/$stem"
    qpdf --static-id --newline-before-endstream=no --warning-exit-0 \
        "$FIX/$stem.pdf" "$REF/$stem/adbe-strip.pdf"
    echo "$stem/adbe-strip.pdf"
done
```

Locate the exact placement by grepping for a similar existing `for stem in ...` block that generates goldens.

**Step 4: Sanity check with shellcheck (if available)**

```bash
shellcheck tests/golden/regenerate.sh 2>&1 | grep -v "^$" | head -20 || echo "shellcheck not available"
```

Expected: no new warnings introduced by the added blocks.

**Step 5: Commit**

```bash
git add tests/golden/regenerate.sh
git commit -m "test(flpdf): regenerate.sh — hand-craft /ADBE-strip fixtures + goldens"
```

---

## Task 5: Run `regenerate.sh` to produce fixtures + goldens

**Files:**
- Create: `tests/fixtures/compat/one-page-stale-adbe-no-ext.pdf`
- Create: `tests/fixtures/compat/one-page-stale-adbe-no-ext-vendor.pdf`
- Create: `tests/golden/references/one-page-stale-adbe-no-ext/adbe-strip.pdf`
- Create: `tests/golden/references/one-page-stale-adbe-no-ext-vendor/adbe-strip.pdf`

**Step 1: Verify qpdf version**

```bash
qpdf --version | head -1
```

Expected: `qpdf version 11.9.0`. If not this exact version, stop and resolve before running `regenerate.sh` (per `tests/golden/README.md`).

**Step 2: Run the script**

```bash
bash tests/golden/regenerate.sh 2>&1 | grep -E "Generating|Skipping|adbe-strip" | head -20
```

Expected output includes:
```
Generating one-page-stale-adbe-no-ext.pdf ...
Generating one-page-stale-adbe-no-ext-vendor.pdf ...
one-page-stale-adbe-no-ext/adbe-strip.pdf
one-page-stale-adbe-no-ext-vendor/adbe-strip.pdf
```

**Step 3: Spot-check golden bytes**

```bash
# Fixture 1: whole /Extensions removed → Catalog has no /Extensions.
grep -a "Extensions\|Type /Catalog" tests/golden/references/one-page-stale-adbe-no-ext/adbe-strip.pdf | head -3

# Fixture 2: /ADBE removed, /XYZW preserved.
grep -a "Extensions\|ADBE\|XYZW\|Type /Catalog" tests/golden/references/one-page-stale-adbe-no-ext-vendor/adbe-strip.pdf | head -3
```

Expected:
- Fixture 1 golden Catalog: `<< /Pages 2 0 R /Type /Catalog >>` (no /Extensions).
- Fixture 2 golden Catalog: `<< /Extensions << /XYZW << /BaseVersion /1.4 /ExtensionLevel 1 >> >> /Pages 2 0 R /Type /Catalog >>` (only /XYZW).

**Step 4: Commit**

```bash
git add tests/fixtures/compat/one-page-stale-adbe-no-ext.pdf \
        tests/fixtures/compat/one-page-stale-adbe-no-ext-vendor.pdf \
        tests/golden/references/one-page-stale-adbe-no-ext \
        tests/golden/references/one-page-stale-adbe-no-ext-vendor
git commit -m "test(flpdf): fixtures + goldens for /ADBE strip parity"
```

---

## Task 6: Add byte-gate test file

**Files:**
- Create: `crates/flpdf/tests/adbe_removal_qpdf_parity.rs`

**Step 1: Write the test file**

Create `crates/flpdf/tests/adbe_removal_qpdf_parity.rs` with:

```rust
//! Byte-identity: flpdf plain full-rewrite emits qpdf's Catalog /Extensions
//! /ADBE removal (QPDFWriter.cc L1408 whole /Extensions removal and L1432
//! /ADBE-only removal) byte-for-byte.
//!
//! Proves flpdf's broadened strip trigger (`catalog_has_extensions_adbe`)
//! matches qpdf's `have_extensions_adbe = keys.count("/ADBE") > 0`
//! (QPDFWriter.cc L1387) on inputs whose source /ADBE dict lacks a valid
//! `/ExtensionLevel` — the case the previous `adobe_extension_level() > 0`
//! gate silently passed through.
//!
//! Fixtures are content-stream-free, so byte-identity is independent of the
//! deflate backend — this file is NOT gated on `qpdf-zlib-compat`.

use flpdf::{write_pdf_with_options, NewlineBeforeEndstream, Pdf, WriteOptions};
use std::path::Path;

/// Plain full-rewrite of `fixture` with qpdf-matching option set; return bytes.
fn adbe_removal_qpdf_equivalent(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let opts = WriteOptions {
        full_rewrite: true,
        static_id: true,
        newline_before_endstream: NewlineBeforeEndstream::Never,
        ..WriteOptions::default()
    };

    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &opts).unwrap();
    out
}

fn golden(stem: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(stem)
        .join("adbe-strip.pdf");
    std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
}

fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    if a == b {
        return None;
    }
    let common = a.len().min(b.len());
    for i in 0..common {
        if a[i] != b[i] {
            return Some(i);
        }
    }
    Some(common)
}

fn assert_parity(fixture: &str, stem: &str) {
    let actual = adbe_removal_qpdf_equivalent(fixture);
    let expected = golden(stem);
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{fixture}: not byte-identical to qpdf adbe-strip golden \
             (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            &actual[lo..(off + 16).min(actual.len())],
            &expected[lo..(off + 16).min(expected.len())],
        );
    }
}

#[test]
fn whole_extensions_removed_when_source_adbe_lacks_extension_level_byte_identical_to_qpdf() {
    // qpdf L1408: /Extensions has only /ADBE and we don't want /ADBE → drop
    // whole /Extensions from Catalog.
    assert_parity(
        "one-page-stale-adbe-no-ext.pdf",
        "one-page-stale-adbe-no-ext",
    );
}

#[test]
fn non_adbe_prefix_preserved_when_source_adbe_lacks_extension_level_byte_identical_to_qpdf() {
    // qpdf L1432: /Extensions has /ADBE + non-ADBE prefix and we don't want
    // /ADBE → remove /ADBE key only, keep /Extensions with other keys.
    assert_parity(
        "one-page-stale-adbe-no-ext-vendor.pdf",
        "one-page-stale-adbe-no-ext-vendor",
    );
}
```

**Step 2: Run the byte gate**

```bash
cargo test -p flpdf --test adbe_removal_qpdf_parity 2>&1 | tail -20
```

Expected: both tests pass.

If parity fails: the panic will print the first diff offset with 16 bytes of context around it. Common causes:
- `newline_before_endstream` mismatch → we use `Never` and qpdf uses `--newline-before-endstream=no`.
- `/ID` mismatch → both must use the qpdf static-id constant (`static_id: true` + `--static-id`).
- Xref form mismatch — flpdf defaults to classic xref (`table`), which matches qpdf's default on our fixtures.
- Deflate mismatch — fixtures are content-stream-free, so no deflate involved.

If any diff persists, run `xxd tests/golden/references/one-page-stale-adbe-no-ext/adbe-strip.pdf | head -30` and the corresponding flpdf output to isolate.

**Step 3: Commit**

```bash
git add crates/flpdf/tests/adbe_removal_qpdf_parity.rs
git commit -m "test(flpdf): byte-gate for /ADBE removal parity vs qpdf 11.9.0"
```

---

## Task 7: Regression, coverage, lint, doc gates

**Files:** (verification only, no new edits unless a gate flags something)

**Step 1: Full test suite (default backend, miniz_oxide)**

```bash
cargo test --workspace 2>&1 | tail -10
```

Expected: all tests pass. Baseline: 1958 lib tests + prior integration tests + 2 new lib tests + 2 new integration tests → net +4.

**Step 2: Full test suite (qpdf-zlib-compat backend)**

```bash
cargo test --workspace --features qpdf-zlib-compat 2>&1 | tail -10
```

Expected: all tests pass. Our new fixtures are content-stream-free, so the deflate backend has no effect on the byte gate, but the wider regression matters.

**Step 3: Patch coverage gate**

```bash
bash scripts/patch-coverage.sh --base main 2>&1 | tail -20
```

Expected: script exits 0. Any uncovered lines on the `flpdf` diff must either be tested or explicitly `// cov:ignore: <reason>` (annotated in the PR description). The new helper `catalog_has_extensions_adbe` has three fall-through branches (`root_ref None`, non-dict Catalog, non-dict/-ref extensions value) that are defensive; the two unit tests exercise the primary path (return `true`) and the return-`false` no-Extensions path is exercised any time an existing test writes a Catalog without `/Extensions`. If the gate flags a defensive branch, add `// cov:ignore-start` / `// cov:ignore-end` with a one-line reason (see `strip_adbe_extension`'s existing pattern).

**Step 4: fmt / clippy / doc**

```bash
cargo fmt --check 2>&1 | tail -5
cargo clippy --workspace -- -D warnings 2>&1 | tail -10
cargo doc --workspace --no-deps 2>&1 | grep -iE "warning|error" | tail -10
```

Expected: no fmt drift, no clippy warnings, no doc warnings.

**Step 5: Verify the diff is small and self-contained**

```bash
git log --oneline main..HEAD
git diff --stat main..HEAD
```

Expected: 6-7 small commits touching only `crates/flpdf/src/writer.rs`, `crates/flpdf/tests/adbe_removal_qpdf_parity.rs`, `tests/golden/regenerate.sh`, and the 4 new fixture/golden `.pdf` files.

**Step 6: No commit needed** — this task is verification. If any gate fails, fix and add a new commit.

---

## Task 8: Push branch and create PR

**Files:** none — GitHub-only operations.

**Step 1: Rebase to latest main (if needed)**

```bash
git fetch origin main
git rebase origin/main
```

If conflicts arise (unlikely — writer.rs L3016-3025 and adjacent code haven't changed recently, per `git log main -20 -- crates/flpdf/src/writer.rs`), resolve preserving both semantics.

**Step 2: Push branch**

```bash
git push -u origin feat/flpdf-9hc-16-15-adbe-removal-parity
```

**Step 3: Create PR via `gh pr create`**

Follow the standard PR message format. Reference the beads issue and the qpdf source lines.

```bash
gh pr create --title "feat(flpdf): broaden /ADBE strip trigger to any /ADBE key (qpdf L1387/L1408/L1432 parity)" --body "$(cat <<'EOF'
## Summary

Closes flpdf-9hc.16.15 (parent: flpdf-9hc.16 Overlay/underlay epic).

Broaden the `/Extensions /ADBE` strip trigger in `write_pdf_full_rewrite` from
`source_ext > 0` (positive integer `/ExtensionLevel` required) to
`catalog_has_extensions_adbe(pdf)` (any `/ADBE` key existence, matching qpdf's
`have_extensions_adbe = keys.count("/ADBE") > 0` at `QPDFWriter.cc` L1387).

The previous gate silently passed through stale/malformed `/ADBE` entries
(no `/ExtensionLevel`, or `/ExtensionLevel` not an integer), producing
Catalogs that qpdf strips but flpdf preserved. The strip helper itself
(`strip_adbe_extension`) already handles both qpdf branches correctly:
- L1408 whole `/Extensions` removal when the dict only carried `/ADBE`;
- L1432 partial removal (drop `/ADBE`, keep non-ADBE developer prefixes).

## Test plan

- [x] 2 new unit tests in `writer.rs`:
  - `write_pdf_full_rewrite_strips_stale_adbe_when_source_has_no_extension_level`
  - `write_pdf_full_rewrite_strips_stale_adbe_no_ext_level_preserving_vendor_prefix`
- [x] 2 new byte-gate tests in `crates/flpdf/tests/adbe_removal_qpdf_parity.rs`
  vs `qpdf` 11.9.0 goldens (content-stream-free fixtures, deflate-independent).
- [x] All 4 existing `strips_stale_adbe_*` tests remain green (no regression
  on the floor-bump strip path).
- [x] `does_not_leave_root_dirty_flag_set` / `preserves_pre_existing_root_dirty_flag`
  remain green (broadened trigger is precise: no-op strip is not triggered
  when `/ADBE` is absent).
- [x] `cargo test --workspace` (default miniz_oxide backend) — all pass.
- [x] `cargo test --workspace --features qpdf-zlib-compat` — all pass.
- [x] `scripts/patch-coverage.sh` — 100% patch coverage on `flpdf` changes.
- [x] `cargo fmt --check`, `cargo clippy -D warnings`, `cargo doc` clean.

## References

- qpdf `libqpdf/QPDFWriter.cc` L1355-1436 (writeStandard / Extensions handling).
- PR #469 (original `inject_adbe_extension` / `strip_adbe_extension`; deferred
  this case with "no test-visible case in the current fixture set").
- beads issue flpdf-9hc.16.15.
EOF
)"
```

**Step 4: Verify PR opens cleanly**

```bash
gh pr view --web
```

Or check CI status:

```bash
gh pr checks
```

Wait for CI. Address any failures (rebase if `main` moved, re-run `regenerate.sh` if a golden drift occurred).

**Step 5: Close the beads issue**

After the PR merges (not before), from the main worktree:

```bash
cd /home/ubuntu/flpdf
bd close flpdf-9hc.16.15
```

---

## Notes for the executing engineer

- **Byte parity is fragile**: any change to `newline_before_endstream`, `/ID` generation, or dict key order in the writer will surface as a byte-gate failure. The two new fixtures are content-stream-free so deflate is not involved.
- **qpdf version pin**: `regenerate.sh` gates on qpdf 11.9.0 exactly. Do not upgrade unless the entire golden matrix is re-blessed.
- **Coverage gate ignore rules**: if the coverage gate flags defensive branches in `catalog_has_extensions_adbe` (the three `return Ok(false)` fall-throughs), use `// cov:ignore-start` / `// cov:ignore-end` with a one-line reason. See how `strip_adbe_extension` handles this at `writer.rs:798-804`.
- **Do not introduce**: public API changes (helper is `fn`-scope), new WriteOptions fields, or changes to the linearize / QDF paths (all out of scope).
- **CI file list**: memory note `flpdf-ci-bytes-identical-explicit-test-list` says byte-identical tests must be explicitly listed in `.github/workflows/ci.yml`. The new file is content-stream-free (not zlib-gated) so it should already run under the default `cargo test --workspace` invocation; verify with `grep -n "byte_gate\|adbe_removal" .github/workflows/ci.yml` — if the workflow uses per-test invocation, add our file. If it uses `cargo test --workspace`, no CI edit needed.

# Overlay/Underlay Source Version-Floor Propagation Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Propagate source PDF header version AND Adobe extension_level from
overlay/underlay sources to the destination output, so `flpdf` matches qpdf
byte-identically when a source's version > dest version (in particular the
AES-256 encrypted-source case with ext_level=8).

**Architecture:** Mirror qpdf's layer split. `QPDFJob → flpdf-cli main.rs` performs
per-input accumulation (`max_input_version`); `QPDFWriter → flpdf writer.rs`
carries `min_version` + new `min_extension_level` state and injects
`/Extensions /ADBE /BaseVersion /ExtensionLevel` into the Catalog when
`effective_extension_level > 0`. Overlay library code (`apply_overlay_specs`) is
version-agnostic (mirrors qpdf's overlay code).

**Tech Stack:** Rust workspace (`crates/flpdf` + `crates/flpdf-cli`). Byte-parity
tests are `cfg(all(test, feature = "qpdf-zlib-compat"))`; structural tests run on
default features. Fixtures live under `tests/fixtures/compat/` and
`tests/golden/references/overlay/`.

**Design (source of truth):** `bd show flpdf-9hc.16.8` — DESIGN section.

---

## Fixture Setup

### Task 0: Fixtures & goldens

Generate the three files needed by later byte gates. This is setup, not TDD —
run once, verify the qpdf oracle bytes, commit.

**Files:**
- Create: `tests/fixtures/compat/one-page-v17.pdf`
- Create: `tests/golden/references/overlay/three-page-overlay-v17-source.pdf`
- Create: `tests/golden/references/overlay/three-page-overlay-encrypted-source.pdf`

**Step 1: Generate `one-page-v17.pdf`**

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-9hc-16-8
# Bump minimal.pdf (1.3) to 1.7 header. If a suitable minimal 1.3 fixture is
# not already present, use tests/fixtures/compat/one-page.pdf.
qpdf --min-version=1.7 --static-id \
  tests/fixtures/compat/one-page.pdf \
  tests/fixtures/compat/one-page-v17.pdf
head -c 12 tests/fixtures/compat/one-page-v17.pdf | tr -d '\0'; echo
# Expected: %PDF-1.7
```

**Step 2: Generate `three-page-overlay-v17-source.pdf` golden**

```bash
qpdf --static-id \
  tests/fixtures/compat/three-page.pdf \
  --overlay tests/fixtures/compat/one-page-v17.pdf -- \
  tests/golden/references/overlay/three-page-overlay-v17-source.pdf
head -c 12 tests/golden/references/overlay/three-page-overlay-v17-source.pdf | tr -d '\0'; echo
# Expected: %PDF-1.7
# Expect NO /Extensions/ADBE in Catalog (source ext_level == 0)
grep -aE 'Extensions|ADBE' tests/golden/references/overlay/three-page-overlay-v17-source.pdf || echo "no ADBE (expected)"
```

**Step 3: Generate `three-page-overlay-encrypted-source.pdf` golden**

```bash
qpdf --static-id \
  tests/fixtures/compat/three-page.pdf \
  --overlay tests/fixtures/compat/one-page-enc-u.pdf --password=u -- \
  tests/golden/references/overlay/three-page-overlay-encrypted-source.pdf
head -c 12 tests/golden/references/overlay/three-page-overlay-encrypted-source.pdf | tr -d '\0'; echo
# Expected: %PDF-1.7
ls -la tests/golden/references/overlay/three-page-overlay-encrypted-source.pdf
# Expected size ~2804 bytes
grep -aE 'Extensions|ADBE|ExtensionLevel' tests/golden/references/overlay/three-page-overlay-encrypted-source.pdf
# Expected: /Extensions << /ADBE << /BaseVersion /1.7 /ExtensionLevel 8 >> >>
```

**Step 4: Commit**

```bash
git add tests/fixtures/compat/one-page-v17.pdf \
        tests/golden/references/overlay/three-page-overlay-v17-source.pdf \
        tests/golden/references/overlay/three-page-overlay-encrypted-source.pdf
git commit -m "test(flpdf-9hc.16.8): add overlay version-floor fixtures & goldens

- one-page-v17.pdf: non-encrypted %PDF-1.7 single-page source for
  pure version-floor gate.
- three-page-overlay-v17-source.pdf: qpdf golden (header bumped 1.3→1.7,
  no /Extensions injection since source ext_level==0).
- three-page-overlay-encrypted-source.pdf: re-added qpdf golden (2804
  bytes; header 1.7, Catalog carries /Extensions /ADBE /BaseVersion /1.7
  /ExtensionLevel 8) for the AES-256 encrypted-source case.

flpdf-9hc.16.8"
```

---

## Library layer (crates/flpdf)

### Task 1: `Pdf::adobe_extension_level` public API

**Files:**
- Modify: `crates/flpdf/src/reader.rs` (add pub method, ~line 776 area)
- Modify: `crates/flpdf/src/check.rs` (replace private helper with the new
  method's implementation OR keep helper and delegate)
- Modify: `crates/flpdf/src/lib.rs` (no change if `reader::Pdf` is already
  re-exported; check)

**Step 1: Write the failing test**

Append to `crates/flpdf/src/reader.rs` test module (search
`#[cfg(test)]` at file bottom):

```rust
#[test]
fn adobe_extension_level_reads_catalog_extensions_adbe() {
    // Fixture: Catalog with /Extensions /ADBE /ExtensionLevel 8, generated
    // inline as ASCII bytes so the test has no fixture dependency.
    let mut src = Vec::new();
    src.extend_from_slice(b"%PDF-1.7\n");
    src.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions 4 0 R >>\nendobj\n");
    src.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    src.extend_from_slice(b"4 0 obj\n<< /ADBE << /BaseVersion /1.7 /ExtensionLevel 8 >> >>\nendobj\n");
    // ... minimal xref + trailer as in existing check.rs `extension_level_pdf_bytes`
    // (copy that helper's body verbatim for the assembly).
    let mut pdf = Pdf::open(Cursor::new(src)).unwrap();
    assert_eq!(pdf.adobe_extension_level(), Some(8));
}

#[test]
fn adobe_extension_level_absent_returns_none() {
    let bytes = b"%PDF-1.3\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
                  2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
                  xref\n0 3\n0000000000 65535 f\n0000000009 00000 n\n0000000067 00000 n\n\
                  trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n130\n%%EOF\n";
    let mut pdf = Pdf::open(Cursor::new(&bytes[..])).unwrap();
    assert_eq!(pdf.adobe_extension_level(), None);
}
```

Run: `cargo test -p flpdf reader::adobe_extension_level -- --exact`
Expected: FAIL — method not defined.

**Step 2: Move `adobe_extension_level` from check.rs to reader.rs as `pub fn` on
`Pdf`**

In `crates/flpdf/src/reader.rs`, near `pub fn version()` (line 776):

```rust
/// Adobe extension level from the catalog's `/Extensions /ADBE /ExtensionLevel`,
/// resolving indirect references at each step. `None` when the catalog has no
/// Extensions or the ADBE prefix is absent. Only the `/ADBE` developer prefix
/// is honoured, matching qpdf's `--check` version banner and the extension
/// level qpdf accumulates into `max_input_version`.
pub fn adobe_extension_level(&mut self) -> Option<i64> {
    use crate::resolve::resolve_value;
    let root_ref = self.root_ref()?;
    let catalog = resolve_value(self, Object::Reference(root_ref))?;
    let extensions = resolve_value(self, catalog.as_dict()?.get("Extensions")?.clone())?;
    let adbe = resolve_value(self, extensions.as_dict()?.get("ADBE")?.clone())?;
    let level = resolve_value(self, adbe.as_dict()?.get("ExtensionLevel")?.clone())?;
    level.as_integer()
}
```

In `crates/flpdf/src/check.rs`:
- Delete `fn adobe_extension_level` (private).
- At line 223 replace `extension_level: adobe_extension_level(&mut pdf)` with
  `extension_level: pdf.adobe_extension_level()`.

**Step 3: Run tests**

```bash
cargo test -p flpdf reader::adobe_extension_level -- --exact
cargo test -p flpdf check::  # ensure existing check tests still pass
```
Expected: PASS.

**Step 4: Commit**

```bash
git add crates/flpdf/src/reader.rs crates/flpdf/src/check.rs
git commit -m "feat(flpdf): promote adobe_extension_level to pub Pdf method

Needed by flpdf-cli's overlay accumulator (flpdf-9hc.16.8) to gather
extension_level from opened source Pdfs and pass it to WriteOptions.

flpdf-9hc.16.8"
```

---

### Task 2: `WriteOptions::min_extension_level` field

**Files:**
- Modify: `crates/flpdf/src/writer.rs` (WriteOptions struct near line 229)

**Step 1: Write the failing test**

In `writer.rs` tests (search for existing WriteOptions default tests):

```rust
#[test]
fn write_options_default_min_extension_level_is_none() {
    let options = WriteOptions::default();
    assert!(options.min_extension_level.is_none());
}

#[test]
fn write_options_min_extension_level_stores_and_returns_value() {
    let options = WriteOptions {
        min_extension_level: Some(8),
        ..Default::default()
    };
    assert_eq!(options.min_extension_level, Some(8));
}
```

Run: `cargo test -p flpdf write_options_default_min_extension_level -- --exact`
Expected: FAIL — no such field.

**Step 2: Add the field**

In `crates/flpdf/src/writer.rs`, immediately after `pub min_version:
Option<String>` (line 229):

```rust
/// Enforce a minimum Adobe extension level in the output catalog's
/// `/Extensions /ADBE /ExtensionLevel`.
///
/// The effective extension level is computed pairwise with `min_version`
/// per qpdf semantics: a higher `min_version` RESETS the extension level
/// (does not carry it across a version bump). When the resulting effective
/// level is > 0, the writer injects
/// `/Extensions << /ADBE << /BaseVersion /<ver> /ExtensionLevel <lvl> >> >>`
/// into the Catalog. When 0, no injection (existing Catalog untouched).
///
/// Mirrors qpdf `--min-version <version>-<level>` (the level portion) and the
/// extension_level qpdf's `QPDFJob` accumulates into `max_input_version` from
/// every opened input's catalog.
pub min_extension_level: Option<i64>,
```

**Step 3: Run tests**

```bash
cargo test -p flpdf write_options -- --exact
```
Expected: PASS.

**Step 4: Commit**

```bash
git add crates/flpdf/src/writer.rs
git commit -m "feat(flpdf): add WriteOptions::min_extension_level

Field carrying the Adobe extension-level floor. When set > 0, the writer
will inject Catalog /Extensions /ADBE /BaseVersion /ExtensionLevel on
emit (implemented in a later task).

Mirrors qpdf QPDFWriter's min_extension_level.

flpdf-9hc.16.8"
```

---

### Task 3: Effective (version, ext_level) pairwise helper

**Files:**
- Modify: `crates/flpdf/src/writer.rs` (near `effective_pdf_version` at line 478)

**Step 1: Write the failing tests**

In `writer.rs` tests:

```rust
#[test]
fn effective_pdf_version_and_ext_no_bump_when_source_geq() {
    // Source 1.7, options empty → source wins, ext 0 stays 0.
    let options = WriteOptions::default();
    let (v, e) = effective_pdf_version_and_ext("1.7", 0, &options, false, false);
    assert_eq!(v, "1.7");
    assert_eq!(e, 0);
}

#[test]
fn effective_pdf_version_and_ext_pairwise_min_version_bump_resets_ext() {
    // Source (1.3, 0), min_version=1.7, min_ext=0 → (1.7, 0).
    let options = WriteOptions {
        min_version: Some("1.7".into()),
        min_extension_level: None,
        ..Default::default()
    };
    let (v, e) = effective_pdf_version_and_ext("1.3", 0, &options, false, false);
    assert_eq!(v, "1.7");
    assert_eq!(e, 0);
}

#[test]
fn effective_pdf_version_and_ext_pairwise_min_carries_ext_when_ver_matches() {
    // Source 1.7 ext 0, min 1.7 ext 8 → (1.7, 8).
    let options = WriteOptions {
        min_version: Some("1.7".into()),
        min_extension_level: Some(8),
        ..Default::default()
    };
    let (v, e) = effective_pdf_version_and_ext("1.7", 0, &options, false, false);
    assert_eq!(v, "1.7");
    assert_eq!(e, 8);
}

#[test]
fn effective_pdf_version_and_ext_higher_source_wins_and_resets_min_ext() {
    // Source (2.0, 0), min (1.7, 8) → source ver wins → (2.0, 0). ext RESETS.
    let options = WriteOptions {
        min_version: Some("1.7".into()),
        min_extension_level: Some(8),
        ..Default::default()
    };
    let (v, e) = effective_pdf_version_and_ext("2.0", 0, &options, false, false);
    assert_eq!(v, "2.0");
    assert_eq!(e, 0);
}

#[test]
fn effective_pdf_version_and_ext_source_ext_wins_when_ver_matches() {
    // Source (1.7, 8), min (1.7, 0) → (1.7, 8).
    let options = WriteOptions {
        min_version: Some("1.7".into()),
        min_extension_level: Some(0),
        ..Default::default()
    };
    let (v, e) = effective_pdf_version_and_ext("1.7", 8, &options, false, false);
    assert_eq!(v, "1.7");
    assert_eq!(e, 8);
}
```

Run: `cargo test -p flpdf effective_pdf_version_and_ext -- --exact`
Expected: FAIL — helper not defined.

**Step 2: Add the helper**

In `crates/flpdf/src/writer.rs` next to `effective_pdf_version`:

```rust
/// Compute the effective (PDF version, Adobe extension level) pair to write,
/// applying qpdf's pairwise combined rule (QPDFWriter.cc L217-250):
///
/// - `min_version` unset → take (source_ver, source_ext).
/// - new_ver > current → take (new_ver, new_ext).  **Extension level RESETS
///   across a version bump; it does not carry.**
/// - new_ver == current AND new_ext > current_ext → take ext only.
/// - new_ver < current → ignore.
///
/// This helper is the pair-aware sibling of [`effective_pdf_version`]; it must
/// stay in lockstep with that function's version arithmetic. The extension_level
/// is only meaningful when > 0; callers should injection-gate on that.
pub fn effective_pdf_version_and_ext<'a>(
    source: &'a str,
    source_ext: i64,
    options: &'a WriteOptions,
    linearize: bool,
    object_streams: bool,
) -> (&'a str, i64) {
    // Reuse effective_pdf_version for the version half.
    let ver = effective_pdf_version(source, options, linearize, object_streams);
    // Ext-level pairwise: only carry min_extension_level when the effective
    // version equals options.min_version (i.e. min_version won or matched).
    let ext = match options.min_version.as_deref() {
        Some(min_v) if min_v == ver => {
            // Version tied with min_version → take max(source_ext, min_ext).
            let cand = options.min_extension_level.unwrap_or(0);
            source_ext.max(cand)
        }
        _ => {
            // Version came from source (or source_or_object_streams_floor) → keep source_ext.
            source_ext
        }
    };
    (ver, ext)
}
```

**Step 3: Run tests**

```bash
cargo test -p flpdf effective_pdf_version_and_ext -- --exact
```
Expected: PASS all 5.

**Step 4: Commit**

```bash
git add crates/flpdf/src/writer.rs
git commit -m "feat(flpdf): add effective_pdf_version_and_ext pairwise helper

Extends effective_pdf_version with the extension_level dimension using
qpdf's pairwise rule (a higher min_version RESETS ext_level; ties allow
the higher ext_level to win). Used by the Catalog /Extensions/ADBE
injection in a later task.

Ref: qpdf QPDFWriter.cc L217-250 setMinimumPDFVersion.

flpdf-9hc.16.8"
```

---

### Task 4: Catalog `/Extensions /ADBE` injection in writer

**Files:**
- Modify: `crates/flpdf/src/writer.rs`
  (`write_pdf_full_rewrite` near line 2579 — pre-emission mutation of Catalog)

**Step 1: Write the failing test**

Add to `writer.rs` tests. This is a **library byte-level assertion** — no
qpdf-zlib-compat gate needed because we only assert on the header + Catalog
substring, not the whole output:

```rust
#[test]
fn write_pdf_full_rewrite_injects_extensions_adbe_when_min_ext_gt_zero() {
    // Minimal 1.3 document.
    let mut src = Vec::new();
    src.extend_from_slice(b"%PDF-1.3\n%\xa0\xa1\xa2\xa3\n");
    src.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    src.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    // ... complete xref + trailer inline (copy pattern from existing minimal
    // fixture helpers in writer.rs tests, ~line 4640 or similar).

    let mut pdf = Pdf::open(Cursor::new(src)).unwrap();
    let mut out = Vec::new();
    let options = WriteOptions {
        full_rewrite: true,
        min_version: Some("1.7".into()),
        min_extension_level: Some(8),
        static_id: true,
        ..Default::default()
    };
    write_pdf_with_options(&mut pdf, &mut out, &options).unwrap();

    // Header raised.
    assert!(out.starts_with(b"%PDF-1.7\n"), "header not 1.7: {:?}", &out[..12]);
    // Catalog carries /Extensions/ADBE with the injected values.
    let s = String::from_utf8_lossy(&out);
    assert!(
        s.contains("/Extensions"),
        "no /Extensions in output"
    );
    assert!(
        s.contains("/ADBE"),
        "no /ADBE in output"
    );
    assert!(
        s.contains("/BaseVersion /1.7"),
        "no /BaseVersion /1.7 in output"
    );
    assert!(
        s.contains("/ExtensionLevel 8"),
        "no /ExtensionLevel 8 in output"
    );
}

#[test]
fn write_pdf_full_rewrite_no_injection_when_min_ext_is_none_or_zero() {
    let mut src = Vec::new();
    src.extend_from_slice(b"%PDF-1.3\n%\xa0\xa1\xa2\xa3\n");
    // ... same minimal doc as above.

    let mut pdf = Pdf::open(Cursor::new(src)).unwrap();
    let mut out = Vec::new();
    let options = WriteOptions {
        full_rewrite: true,
        min_version: Some("1.7".into()),
        min_extension_level: None, // no ext -> no injection
        static_id: true,
        ..Default::default()
    };
    write_pdf_with_options(&mut pdf, &mut out, &options).unwrap();
    assert!(out.starts_with(b"%PDF-1.7\n"));
    let s = String::from_utf8_lossy(&out);
    assert!(!s.contains("/Extensions"), "unexpected /Extensions injected");
}
```

Run: `cargo test -p flpdf write_pdf_full_rewrite_injects_extensions -- --exact`
Expected: FAIL — injection not implemented.

**Step 2: Implement injection**

In `crates/flpdf/src/writer.rs`, inside `write_pdf_full_rewrite` (near the top,
after the options-adjustment block that starts around line 2601), add a
pre-emission mutation step. Use `effective_pdf_version_and_ext` to decide.

Concrete site: immediately BEFORE `let renumber = CatalogFirstRenumber::build(pdf, true)?;`
(around line 2657):

```rust
// flpdf-9hc.16.8: Adobe extension level propagation via Catalog
// /Extensions /ADBE injection (mirrors qpdf QPDFWriter.cc L1355-1450).
// Fires only when the caller set min_extension_level > 0 (usually via
// flpdf-cli overlay/underlay accumulation). No-op otherwise.
{
    let source_ver = pdf.version().to_string();
    let source_ext = pdf.adobe_extension_level().unwrap_or(0);
    let (eff_ver, eff_ext) = effective_pdf_version_and_ext(
        &source_ver,
        source_ext,
        options,
        false, // linearize is decided later; ext logic is independent
        false, // object_streams likewise
    );
    if eff_ext > 0 {
        inject_adbe_extension(pdf, eff_ver, eff_ext)?;
    }
    // Bind to _ so the (String, i64) tuple isn't unused when eff_ext == 0.
    let _ = (eff_ver, eff_ext);
}
```

Then add the `inject_adbe_extension` helper below the pair helper:

```rust
/// Ensure the destination Catalog carries
/// `/Extensions << /ADBE << /BaseVersion /<ver> /ExtensionLevel <lvl> >> >>`.
///
/// Mirrors qpdf QPDFWriter.cc L1355-1450:
/// - If Catalog has no /Extensions: create a direct dict with /ADBE.
/// - If Catalog has an /Extensions dict: overwrite the /ADBE entry only,
///   leaving non-ADBE developer prefixes intact.
/// - If /Extensions is an indirect reference: resolve, mutate the referred
///   dict, and re-store the whole mutated Extensions inline on the Catalog
///   (qpdf writes it inline; do the same for byte parity).
///
/// Called only when the effective extension level is > 0.
fn inject_adbe_extension<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    version: &str,
    extension_level: i64,
) -> Result<()> {
    use crate::object::{Dictionary, Object};
    let root_ref = pdf.root_ref().ok_or(crate::Error::Missing("/Root"))?;
    let mut catalog = pdf
        .resolve(Object::Reference(root_ref))?
        .into_dictionary()
        .ok_or_else(|| crate::Error::Unsupported("/Root is not a dictionary".into()))?;

    let mut extensions = match catalog.remove("Extensions") {
        Some(obj) => pdf.resolve(obj)?.into_dictionary().unwrap_or_default(),
        None => Dictionary::new(),
    };

    let mut adbe = Dictionary::new();
    adbe.insert("BaseVersion", Object::Name(format!("/{version}").into_bytes()));
    adbe.insert("ExtensionLevel", Object::Integer(extension_level));
    extensions.insert("ADBE", Object::Dictionary(adbe));
    catalog.insert("Extensions", Object::Dictionary(extensions));

    pdf.set_object(root_ref, Object::Dictionary(catalog))?;
    Ok(())
}
```

Refer to existing writer.rs helpers for exact `Object::Name` construction
and `pdf.set_object` signature (adjust as needed; the shape above is
illustrative).

**Step 3: Run tests**

```bash
cargo test -p flpdf write_pdf_full_rewrite_injects_extensions_adbe -- --exact
cargo test -p flpdf write_pdf_full_rewrite_no_injection -- --exact
```
Expected: BOTH PASS.

**Step 4: Verify no regression on existing writer tests**

```bash
cargo test -p flpdf writer:: 2>&1 | tail -20
```
Expected: no test count regression.

**Step 5: Commit**

```bash
git add crates/flpdf/src/writer.rs
git commit -m "feat(flpdf): inject Catalog /Extensions /ADBE when effective ext > 0

Mirror qpdf QPDFWriter.cc L1355-1450: when write_pdf_full_rewrite runs
with WriteOptions.min_extension_level > 0, mutate the destination Catalog
to carry /Extensions /ADBE /BaseVersion /<ver> /ExtensionLevel <lvl>
before the emission loop. Preserves any non-ADBE developer prefixes.

No-op when min_extension_level is None or 0 — existing outputs unchanged.

flpdf-9hc.16.8"
```

---

### Task 5: Library byte gate — pure version-floor

**Files:**
- Modify: `crates/flpdf/src/overlay.rs` (byte_gate module ~line 872)

**Step 1: Write the failing test**

Append to `mod byte_gate` in `overlay.rs`:

```rust
#[test]
fn overlay_pure_source_version_floor_bytes() {
    use std::fs;

    let dest_bytes = fs::read("tests/fixtures/compat/three-page.pdf").unwrap();
    let source_bytes = fs::read("tests/fixtures/compat/one-page-v17.pdf").unwrap();
    let golden = fs::read(
        "tests/golden/references/overlay/three-page-overlay-v17-source.pdf",
    ).unwrap();

    let mut dest = Pdf::open(Cursor::new(dest_bytes.clone())).unwrap();
    let mut src = Pdf::open(Cursor::new(source_bytes)).unwrap();

    // Mirror flpdf-cli accumulation: max(dest.version, src.version) with
    // pairwise ext.
    let max_ver = max_version_str(dest.version(), src.version());
    let dest_ext = dest.adobe_extension_level().unwrap_or(0);
    let src_ext = src.adobe_extension_level().unwrap_or(0);
    let max_ext = combined_ext(dest.version(), dest_ext, src.version(), src_ext);

    let mut specs = vec![OverlaySpec {
        source: src,
        kind: OverlayKind::Overlay,
        from: PageRange::parse("").unwrap(),
        to: PageRange::parse("").unwrap(),
        repeat: None,
    }];
    apply_overlay_specs(&mut dest, &mut specs).unwrap();

    let mut out = Vec::new();
    let opts = WriteOptions {
        full_rewrite: true,
        static_id: true,
        min_version: Some(max_ver),
        min_extension_level: (max_ext > 0).then_some(max_ext),
        newline_before_endstream: NewlineBeforeEndstream::Never,
        ..Default::default()
    };
    write_pdf_with_options(&mut dest, &mut out, &opts).unwrap();

    if out != golden {
        // Compact diff-style diagnostic — first diff offset + surrounding bytes.
        let n = out.iter().zip(&golden).take_while(|(a, b)| a == b).count();
        panic!(
            "byte mismatch at offset {n}: got {:?} want {:?} (out={} golden={})",
            &out.get(n..n.saturating_add(16)),
            &golden.get(n..n.saturating_add(16)),
            out.len(), golden.len(),
        );
    }
}
```

Also add the two helper functions in the byte_gate module (or import if they
already exist in a common test util):

```rust
fn max_version_str(a: &str, b: &str) -> String {
    let pa = super::parse_pdf_version(a).unwrap_or((1, 0));
    let pb = super::parse_pdf_version(b).unwrap_or((1, 0));
    let m = pa.max(pb);
    format!("{}.{}", m.0, m.1)
}

/// Pairwise (ver, ext) combiner for two inputs (both are opened Pdfs).
fn combined_ext(av: &str, ae: i64, bv: &str, be: i64) -> i64 {
    let pa = super::parse_pdf_version(av).unwrap_or((1, 0));
    let pb = super::parse_pdf_version(bv).unwrap_or((1, 0));
    match pa.cmp(&pb) {
        std::cmp::Ordering::Greater => ae,
        std::cmp::Ordering::Less => be,
        std::cmp::Ordering::Equal => ae.max(be),
    }
}
```

Run:
```bash
cargo test -p flpdf --features qpdf-zlib-compat \
  overlay::byte_gate::overlay_pure_source_version_floor_bytes -- --exact
```
Expected: FAIL (whether from writer not injecting or from bytes not matching
depends on order-of-implementation; if Tasks 1-4 landed first, this should
now PASS on the header assertion but may fail on the full byte compare due
to a missed detail — iterate until PASS).

**Step 2: Iterate until PASS**

- Inspect `first_diff` output; compare qpdf golden vs flpdf output near
  that offset.
- Most likely first diff is at the Catalog dict — verify /Extensions/ADBE
  is NOT present (source ext_level should be 0). If it is, fix injection
  gate.
- Header must be `%PDF-1.7` — if `%PDF-1.3`, the min_version plumbing is
  wrong.

**Step 3: Commit when green**

```bash
git add crates/flpdf/src/overlay.rs
git commit -m "test(flpdf): library byte gate for pure version-floor overlay

three-page.pdf (1.3) + non-encrypted one-page-v17.pdf → header %PDF-1.7,
NO /Extensions/ADBE injection (source ext_level == 0). Validates the
version-floor half of flpdf-9hc.16.8 in isolation from the AES-256
decrypt-import case.

flpdf-9hc.16.8"
```

---

### Task 6: Library byte gate — encrypted-source ext_level

**Files:**
- Modify: `crates/flpdf/src/overlay.rs` (byte_gate module, next to Task 5)

**Step 1: Write the failing test**

```rust
#[test]
fn overlay_encrypted_source_extension_level_bytes() {
    use std::fs;

    let dest_bytes = fs::read("tests/fixtures/compat/three-page.pdf").unwrap();
    let source_bytes = fs::read("tests/fixtures/compat/one-page-enc-u.pdf").unwrap();
    let golden = fs::read(
        "tests/golden/references/overlay/three-page-overlay-encrypted-source.pdf",
    ).unwrap();

    let mut dest = Pdf::open(Cursor::new(dest_bytes)).unwrap();
    let src_opts = PdfOpenOptions {
        password: b"u".to_vec(),
        ..Default::default()
    };
    let mut src = Pdf::open_with_options(Cursor::new(source_bytes), src_opts).unwrap();

    let max_ver = max_version_str(dest.version(), src.version());
    let dest_ext = dest.adobe_extension_level().unwrap_or(0);
    let src_ext = src.adobe_extension_level().unwrap_or(0);
    let max_ext = combined_ext(dest.version(), dest_ext, src.version(), src_ext);

    let mut specs = vec![OverlaySpec {
        source: src,
        kind: OverlayKind::Overlay,
        from: PageRange::parse("").unwrap(),
        to: PageRange::parse("").unwrap(),
        repeat: None,
    }];
    apply_overlay_specs(&mut dest, &mut specs).unwrap();

    let mut out = Vec::new();
    let opts = WriteOptions {
        full_rewrite: true,
        static_id: true,
        min_version: Some(max_ver),
        min_extension_level: (max_ext > 0).then_some(max_ext),
        newline_before_endstream: NewlineBeforeEndstream::Never,
        ..Default::default()
    };
    write_pdf_with_options(&mut dest, &mut out, &opts).unwrap();

    if out != golden {
        let n = out.iter().zip(&golden).take_while(|(a, b)| a == b).count();
        panic!(
            "byte mismatch at offset {n}: got {:?} want {:?} (out={} golden={})",
            &out.get(n..n.saturating_add(16)),
            &golden.get(n..n.saturating_add(16)),
            out.len(), golden.len(),
        );
    }
}
```

Run:
```bash
cargo test -p flpdf --features qpdf-zlib-compat \
  overlay::byte_gate::overlay_encrypted_source_extension_level -- --exact
```
Expected: FAIL until injection lands.

**Step 2: Iterate until PASS**

- Verify header is 1.7 and Catalog now contains `/Extensions /ADBE
  /BaseVersion /1.7 /ExtensionLevel 8`.
- If injection is present but bytes still differ, log the first-diff offset
  and inspect the surrounding structure. Likely candidates:
  - `/Extensions` key placement (should sort before `/PageMode`
    alphabetically — matches qpdf).
  - `/ADBE` inner-dict key order (qpdf emits `/BaseVersion` then
    `/ExtensionLevel` alphabetically).
  - Whitespace / spacing in the injected dict matches qpdf's canonical
    `<< /K V >>` form.

**Step 3: Commit when green**

```bash
git add crates/flpdf/src/overlay.rs
git commit -m "test(flpdf): library byte gate for encrypted-source overlay ext_level

three-page.pdf + AES-256 one-page-enc-u.pdf (password=u) → header 1.7 +
/Extensions /ADBE /BaseVersion /1.7 /ExtensionLevel 8 in Catalog.
Closes the ~61-byte residual documented in flpdf-9hc.16.8.

flpdf-9hc.16.8"
```

---

## CLI layer (crates/flpdf-cli)

### Task 7: CLI accumulator in `main.rs`

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs` (rewrite path around line 3099)

**Step 1: Locate the rewrite site**

Around line 3099 in `crates/flpdf-cli/src/main.rs`:

```rust
if !overlay_specs.is_empty() {
    let mut built = build_overlay_specs(overlay_specs, repair, password.allow_weak_crypto)?;
    flpdf::apply_overlay_specs(&mut pdf, &mut built)?;
}

let mut out = File::create(output)?;
write_pdf_with_options(&mut pdf, &mut out, &options)?;
```

**Step 2: Insert the accumulator**

Between the `apply_overlay_specs` call and `write_pdf_with_options`, add:

```rust
// flpdf-9hc.16.8: propagate max(dest, source) header version and Adobe
// extension_level to the writer (mirrors qpdf QPDFJob.cc L1714 + L2913).
if !overlay_specs.is_empty() {
    let mut built = build_overlay_specs(overlay_specs, repair, password.allow_weak_crypto)?;

    let mut max_ver: (u8, u8) = flpdf::parse_pdf_version(pdf.version()).unwrap_or((1, 0));
    let mut max_ext: i64 = pdf.adobe_extension_level().unwrap_or(0);
    for spec in built.iter_mut() {
        let sv = flpdf::parse_pdf_version(spec.source.version()).unwrap_or((1, 0));
        let se = spec.source.adobe_extension_level().unwrap_or(0);
        match sv.cmp(&max_ver) {
            std::cmp::Ordering::Greater => { max_ver = sv; max_ext = se; }
            std::cmp::Ordering::Equal   => { max_ext = max_ext.max(se); }
            std::cmp::Ordering::Less    => {}
        }
    }
    // Preserve any pre-existing --min-version CLI arg with pairwise max.
    if let Some(ref current) = options.min_version {
        let cur = flpdf::parse_pdf_version(current).unwrap_or((1, 0));
        let cur_ext = options.min_extension_level.unwrap_or(0);
        match cur.cmp(&max_ver) {
            std::cmp::Ordering::Greater => { max_ver = cur; max_ext = cur_ext; }
            std::cmp::Ordering::Equal   => { max_ext = max_ext.max(cur_ext); }
            std::cmp::Ordering::Less    => {}
        }
    }
    options.min_version = Some(format!("{}.{}", max_ver.0, max_ver.1));
    options.min_extension_level = (max_ext > 0).then_some(max_ext);

    flpdf::apply_overlay_specs(&mut pdf, &mut built)?;
}
```

Note: the caller must have `options` bound mutably. If it currently isn't,
promote its binding (`let mut options = ...`).

Also re-export `parse_pdf_version` if it isn't already public from `flpdf::`:

```rust
// crates/flpdf/src/lib.rs
pub use writer::{
    apply_stream_compress_policy, effective_pdf_version, parse_pdf_version, write_pdf,
    // ...
};
```

Verify with `grep -n parse_pdf_version crates/flpdf/src/lib.rs`.

**Step 3: Build + existing tests**

```bash
cargo build -p flpdf-cli
cargo test -p flpdf-cli 2>&1 | tail -20
```
Expected: no regressions.

**Step 4: Commit**

```bash
git add crates/flpdf-cli/src/main.rs
git commit -m "feat(flpdf-cli): accumulate max input version/ext_level for overlay/underlay

Mirrors qpdf QPDFJob.cc L1714 (max_input_version.updateIfGreater across
every opened input) + L2913 (w.setMinimumPDFVersion). For each opened
overlay/underlay source, pairwise-update (max_ver, max_ext) against the
dest and any pre-existing --min-version, then pass through to the
WriteOptions passed to write_pdf_with_options.

flpdf-9hc.16.8"
```

---

### Task 8: CLI structural test — pure version-floor

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_overlay.rs`

**Step 1: Write the failing test**

Append (at file bottom, before the closing `}` if any):

```rust
#[test]
fn overlay_bumps_header_from_source_pure_version_floor() {
    use std::fs;
    use std::process::Command;

    let dest = "tests/fixtures/compat/three-page.pdf";
    let src = "tests/fixtures/compat/one-page-v17.pdf";
    let out_path = tempfile::NamedTempFile::new().unwrap().into_temp_path();

    let status = Command::new(env!("CARGO_BIN_EXE_flpdf"))
        .arg("rewrite")
        .arg("--static-id")
        .arg("--overlay").arg(src).arg("--")
        .arg(dest).arg(&out_path)
        .status().unwrap();
    assert!(status.success(), "flpdf rewrite failed");

    let out = fs::read(&out_path).unwrap();
    assert!(out.starts_with(b"%PDF-1.7\n"),
        "header not raised to 1.7: {:?}", &out[..12]);

    // Parse output via library reader and assert no /Extensions/ADBE
    // (source ext_level == 0).
    let mut pdf = flpdf::Pdf::open(std::io::Cursor::new(out)).unwrap();
    assert_eq!(pdf.adobe_extension_level(), None,
        "unexpected /Extensions/ADBE injection when source ext == 0");
}
```

Path handling: reference `tests/fixtures/compat/*` via `env!("CARGO_MANIFEST_DIR")`
or the workspace-root-relative pattern already used by other cli_overlay tests.
Grep the file for the existing pattern.

Run: `cargo test -p flpdf-cli overlay_bumps_header_from_source_pure_version_floor -- --exact`

Expected: PASS immediately (Task 7 already landed).

**Step 2: Commit**

```bash
git add crates/flpdf-cli/tests/cli_overlay.rs
git commit -m "test(flpdf-cli): structural test for pure version-floor overlay

Runs \`flpdf rewrite --overlay one-page-v17.pdf ...\`, asserts the output
header is %PDF-1.7 and Catalog has NO /Extensions/ADBE (source
ext_level == 0). Validates CLI accumulator plumbing without depending on
byte-identity (CLI cannot emit NewlineBeforeEndstream::Never — see bd
memory flpdf-cli-cannot-emit-newline-never-verify-byte-parity-at-library).

flpdf-9hc.16.8"
```

---

### Task 9: CLI structural test — encrypted source ext_level

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_overlay.rs`

**Step 1: Write the failing test**

Append:

```rust
#[test]
fn overlay_bumps_header_and_injects_adbe_from_encrypted_source() {
    use std::fs;
    use std::process::Command;

    let dest = "tests/fixtures/compat/three-page.pdf";
    let src  = "tests/fixtures/compat/one-page-enc-u.pdf";
    let out_path = tempfile::NamedTempFile::new().unwrap().into_temp_path();

    let status = Command::new(env!("CARGO_BIN_EXE_flpdf"))
        .arg("rewrite")
        .arg("--static-id")
        .arg("--overlay").arg(src).arg("--password=u").arg("--")
        .arg(dest).arg(&out_path)
        .status().unwrap();
    assert!(status.success(), "flpdf rewrite failed");

    let out = fs::read(&out_path).unwrap();
    assert!(out.starts_with(b"%PDF-1.7\n"),
        "header not raised to 1.7: {:?}", &out[..12]);

    let mut pdf = flpdf::Pdf::open(std::io::Cursor::new(out)).unwrap();
    assert_eq!(pdf.adobe_extension_level(), Some(8),
        "expected /Extensions/ADBE/ExtensionLevel 8");
}
```

Run: `cargo test -p flpdf-cli overlay_bumps_header_and_injects_adbe_from_encrypted_source -- --exact`

Expected: PASS.

**Step 2: Commit**

```bash
git add crates/flpdf-cli/tests/cli_overlay.rs
git commit -m "test(flpdf-cli): structural test for encrypted-source overlay ext_level

Runs \`flpdf rewrite --overlay one-page-enc-u.pdf --password=u ...\`,
asserts header %PDF-1.7 and Catalog carries /Extensions /ADBE
/ExtensionLevel 8. Validates the CLI accumulator + writer injection
together on the AES-256 source path (closes the ~61-byte residual
documented in flpdf-9hc.16.8, at the structural level).

flpdf-9hc.16.8"
```

---

## Integration

### Task 10: CI enumeration

**Files:**
- Modify: `.github/workflows/ci.yml`

**Step 1: Locate the qpdf-zlib-compat byte-test list**

```bash
grep -n "qpdf-zlib-compat\|byte_gate\|byte-identical" .github/workflows/ci.yml
```

**Step 2: Append the two new library gates**

Add the following two entries to the enumerated test list (exact YAML shape
depends on the existing structure; align with sibling entries):

```
- overlay::byte_gate::overlay_pure_source_version_floor_bytes
- overlay::byte_gate::overlay_encrypted_source_extension_level_bytes
```

**Step 3: Sanity local verify**

Locally run the qpdf-zlib-compat gate suite for the overlay module:

```bash
cargo test -p flpdf --features qpdf-zlib-compat overlay::byte_gate -- --nocapture 2>&1 | tail -30
```
Expected: all overlay byte gates PASS (existing 6 + 2 new = 8).

**Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci(flpdf-9hc.16.8): enumerate new overlay version-floor byte gates

Add overlay_pure_source_version_floor_bytes and
overlay_encrypted_source_extension_level_bytes to the qpdf-zlib-compat
byte-test enumeration. Without explicit listing, feature-gated tests
are silently skipped in CI (bd memory flpdf-ci-bytes-identical-explicit-test-list).

flpdf-9hc.16.8"
```

---

## Cleanup

### Task 11: Remove stale defer comments

**Files:**
- Modify: `crates/flpdf/src/overlay.rs` (comments around line 858-867)

**Step 1: Locate**

The block currently reads (paraphrased):

```rust
// Explicit deferrals (NOT covered here, by design):
//   - Encrypted-source --password byte-identity: deferred to flpdf-9hc.16.8
//     (source version-floor propagation). qpdf raises the output version to
//     max(dest, sources) for AES-256 sources; flpdf keeps the dest version, so
//     those bytes diverge. The behavioral --password path is covered in
//     crates/flpdf-cli/tests/cli_overlay.rs.
```

Replace with a single line noting the coverage now exists:

```rust
//   - Encrypted-source --password byte-identity: covered by
//     overlay_encrypted_source_extension_level_bytes (below).
```

Or delete the entire deferral note if the rest of the comment block still
makes sense — inspect adjacent lines.

**Step 2: Verify build**

```bash
cargo build --workspace
```

**Step 3: Commit**

```bash
git add crates/flpdf/src/overlay.rs
git commit -m "chore(flpdf): drop stale flpdf-9hc.16.8 defer note

overlay_encrypted_source_extension_level_bytes now covers what was
deferred. Replaced the defer comment with a pointer to the gate.

flpdf-9hc.16.8"
```

---

### Task 12: Verification & final sweep

**Step 1: Run the full test suite**

```bash
cargo test --workspace 2>&1 | tail -20
```
Expected: green.

**Step 2: Run the qpdf-zlib-compat gate**

```bash
cargo test -p flpdf --features qpdf-zlib-compat 2>&1 | tail -20
```
Expected: all 6+2 overlay byte gates PASS.

**Step 3: Patch coverage check (per CLAUDE.md gate)**

```bash
# Must be run WITHOUT qpdf-zlib-compat (bd memory llvm-cov-no-qpdf-zlib-compat).
git status  # ensure clean (no uncommitted changes)
scripts/patch-coverage.sh --base main 2>&1 | tail -30
```
Expected:
- `flpdf` crate: 100% patch coverage on changed lines.
- `flpdf-cli` crate: report-only.

If uncovered lines exist in flpdf-touched files, either add tests or annotate
`// cov:ignore: <reason>` with the reason in the PR description.

**Step 4: fmt + clippy**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --features qpdf-zlib-compat -- -D warnings
```
Expected: both clean.

**Step 5: Verify final bd design/description alignment**

Skim `bd show flpdf-9hc.16.8` and cross-check that ACCEPTANCE items in the
design are satisfied. Note any deferrals in the PR body.

---

## Post-Task PR / Session-Close (per CLAUDE.md)

- `bd close flpdf-9hc.16.8`
- `git push origin feat/flpdf-9hc-16-8-overlay-version-floor`
- `gh pr create` with title `feat: propagate overlay source version-floor
  and ADBE extension_level (flpdf-9hc.16.8)` and a body summarising:
  - What qpdf behavior is now mirrored (Job accumulation + Writer injection).
  - The two new library byte gates (E.1/E.3 pure floor; E.2 encrypted).
  - The two new CLI structural tests.
  - Deferrals (non-overlay CLI paths, ADBE removal path, non-ADBE developer
    prefixes).

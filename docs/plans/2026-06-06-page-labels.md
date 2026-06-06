# PageLabelDocumentHelper Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** A qpdf-`QPDFPageLabelDocumentHelper`-equivalent `PageLabelDocumentHelper`
exposing per-page label reading, ISO 32000-1 §12.4.2 display-string rendering,
structural range access, and direct CRUD on label ranges — plus the deferred
`build_number_tree` primitive.

**Architecture:** A new `page_label_document_helper.rs` module holds `LabelStyle`,
`LabelRange` (parse/build/format), the style formatters, and
`PageLabelDocumentHelper<'a, R>` (borrows `&mut Pdf<R>`, mirrors
`PageDocumentHelper`). Reading uses `name_number_tree::read_number_tree`; CRUD
rebuilds the `/Nums` tree with a net-new `name_number_tree::build_number_tree`
(mirror of `build_name_tree`). `json_inspect::label_dict_to_json` is refactored
onto `LabelRange` with byte-identical JSON.

**Tech Stack:** Rust, `flpdf` crate. `cargo test -p flpdf`, `cargo clippy`,
qpdf 11.9 at `/usr/bin/qpdf` for cross-checking structural output.

**Issue:** flpdf-9hc.18.6 (design in beads `design` field). Closes .14.3; .14.4
stays open.

---

## Key references

- `crates/flpdf/src/name_number_tree.rs` — `build_name_tree` (lines 119-157) +
  `build_leaf_dict` (160-175) to MIRROR; `read_number_tree` (75-102); consts
  `LEAF_MAX=32`, `DEFAULT_MAX_TREE_DEPTH=100`.
- `crates/flpdf/src/page_document_helper.rs` — helper struct shape
  (`PageDocumentHelper<'a, R> { pdf: &'a mut Pdf<R> }`, `new`).
- `crates/flpdf/src/acroform_document_helper.rs:543` — `pub fn acroform(&mut self)
  -> AcroFormDocumentHelper<'_, R>` extension to mirror.
- `crates/flpdf/src/json_inspect.rs` — `label_dict_to_json` (881-922),
  `build_pagelabels_section` (934-988), `decode_pdf_text_string` (132-160,
  `pub(crate)`), pagelabels tests (3593-3891), helpers `load_one_page_pdf`
  (3015), `patch_pagelabels` (3597).
- `crates/flpdf/src/embedded_files.rs:548-558` — object-alloc snapshot pattern
  for rebuild.
- qpdf semantics (verified): `getLabelForPage` start=/St(def 1)+offset;
  `getLabelsForPageRange` redundancy-skip + per-index copy; `pageLabelDict` omits
  /St when 1, /P when empty.
- Review rules `.claude/rules/pdf-rust-review-patterns.md`: resolve indirect
  refs; no needless clone; bound walks; validate external ints before casts.

## §12.4.2 algorithm (authoritative for formatters)

- Decimal (D): `value.to_string()`.
- Roman (R upper / r lower): standard roman; **empty string for value <= 0**.
- Alphabetic (A upper / a lower): **repeating letters** — `1→A … 26→Z, 27→AA,
  52→ZZ, 53→AAA`. Formula: `letter = (v-1) % 26`, `count = (v-1)/26 + 1`, repeat.
  **Empty string for value <= 0.**
- Style absent (`None`): numeric portion is empty (prefix only).
- Label string = `prefix + numeric_portion`.

---

## Task 1: `build_number_tree` in name_number_tree.rs

**Files:** Modify `crates/flpdf/src/name_number_tree.rs`

**Step 1: Write the builder (mirror `build_name_tree`, integer keys/Limits, `/Nums`)**

Add after `build_name_tree`/`build_leaf_dict`:

```rust
/// Build a **number** tree from a **non-empty, pre-sorted** `(key, value)` slice.
///
/// Number-tree analogue of [`build_name_tree`]: identical layout (single leaf
/// `<= LEAF_MAX`, else `div_ceil`-chunked leaves under a `/Limits` + `/Kids`
/// root, leaves allocated first then the root), but with `/Nums` leaves and
/// **integer** `/Limits`. Returns `(root_ref, nodes)` for the caller to
/// [`Pdf::set_object`]; the caller owns numbering, the empty case, and catalog
/// wiring.
///
/// # Panics (debug)
/// Debug-asserts `entries` is non-empty.
pub fn build_number_tree<A>(
    entries: &[(i64, Object)],
    mut alloc: A,
) -> (ObjectRef, Vec<(ObjectRef, Object)>)
where
    A: FnMut() -> ObjectRef,
{
    debug_assert!(
        !entries.is_empty(),
        "build_number_tree requires non-empty entries"
    );
    let mut nodes: Vec<(ObjectRef, Object)> = Vec::new();

    if entries.len() <= LEAF_MAX {
        let leaf_ref = alloc();
        nodes.push((leaf_ref, Object::Dictionary(build_num_leaf_dict(entries))));
        return (leaf_ref, nodes);
    }

    let n_leaves = entries.len().div_ceil(LEAF_MAX);
    let chunk_size = entries.len().div_ceil(n_leaves);
    let mut kids: Vec<Object> = Vec::with_capacity(n_leaves);
    for chunk in entries.chunks(chunk_size) {
        let leaf_ref = alloc();
        nodes.push((leaf_ref, Object::Dictionary(build_num_leaf_dict(chunk))));
        kids.push(Object::Reference(leaf_ref));
    }
    let first = entries.first().map(|(k, _)| *k).unwrap_or_default();
    let last = entries.last().map(|(k, _)| *k).unwrap_or_default();
    let mut root = Dictionary::new();
    root.insert(
        "Limits",
        Object::Array(vec![Object::Integer(first), Object::Integer(last)]),
    );
    root.insert("Kids", Object::Array(kids));
    let root_ref = alloc();
    nodes.push((root_ref, Object::Dictionary(root)));
    (root_ref, nodes)
}

/// Leaf node dict: `/Limits [first last]` (integers) + `/Nums [k1 v1 ...]`.
fn build_num_leaf_dict(entries: &[(i64, Object)]) -> Dictionary {
    let first = entries.first().map(|(k, _)| *k).unwrap_or_default();
    let last = entries.last().map(|(k, _)| *k).unwrap_or_default();
    let mut pairs: Vec<Object> = Vec::with_capacity(entries.len() * 2);
    for (key, val) in entries {
        pairs.push(Object::Integer(*key));
        pairs.push(val.clone());
    }
    let mut dict = Dictionary::new();
    dict.insert(
        "Limits",
        Object::Array(vec![Object::Integer(first), Object::Integer(last)]),
    );
    dict.insert("Nums", Object::Array(pairs));
    dict
}
```

**Step 2: Tests (mirror the build_name_tree tests)**

In the `tests` module add:

```rust
    fn mk_num_entries(n: usize) -> Vec<(i64, Object)> {
        (0..n)
            .map(|i| (i as i64 * 10, Object::Reference(ObjectRef::new(1000 + i as u32, 0))))
            .collect()
    }

    #[test]
    fn build_number_tree_single_leaf_no_kids() {
        let entries = mk_num_entries(3);
        let mut next = 0u32;
        let (root, nodes) = build_number_tree(&entries, || {
            next += 1;
            ObjectRef::new(next, 0)
        });
        assert_eq!(nodes.len(), 1);
        assert_eq!(root, nodes[0].0);
        let d = nodes[0].1.as_dict().expect("leaf dict");
        assert!(d.get("Kids").is_none(), "single leaf must not have /Kids");
        assert!(d.get("Nums").is_some());
        // Integer /Limits.
        let Some(Object::Array(lim)) = d.get("Limits") else {
            panic!("limits")
        };
        assert_eq!(lim[0], Object::Integer(0));
        assert_eq!(lim[1], Object::Integer(20));
    }

    #[test]
    fn build_number_tree_multi_leaf_root_kids_alloc_order() {
        let entries = mk_num_entries(LEAF_MAX + 1); // 33 -> 2 leaves + root
        let mut next = 0u32;
        let (root, nodes) = build_number_tree(&entries, || {
            next += 1;
            ObjectRef::new(next, 0)
        });
        assert_eq!(nodes.len(), 3);
        assert_eq!(root, ObjectRef::new(3, 0), "root allocated last");
        let root_dict = nodes[2].1.as_dict().expect("root dict");
        let kids = root_dict
            .get("Kids")
            .and_then(Object::as_array)
            .expect("root needs /Kids");
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[0], Object::Reference(ObjectRef::new(1, 0)));
        // Every node carries integer /Limits + a /Nums leaf where applicable.
        for (_, n) in &nodes {
            assert!(n.as_dict().expect("dict").get("Limits").is_some());
        }
    }

    #[test]
    fn build_number_tree_round_trips_via_read_number_tree() {
        let mut pdf = empty_pdf();
        let entries: Vec<(i64, Object)> =
            vec![(0, Object::Integer(100)), (5, Object::Integer(200))];
        let mut next = 500u32;
        let (root, nodes) = build_number_tree(&entries, || {
            next += 1;
            ObjectRef::new(next, 0)
        });
        for (r, n) in nodes {
            pdf.set_object(r, n);
        }
        let out: Vec<(i64, i64)> = read_number_tree(
            &mut pdf,
            Object::Reference(root),
            |_, v| Ok(v.as_integer()),
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(out, vec![(0, 100), (5, 200)]);
    }
```

**Step 3: Run** `cargo test -p flpdf --lib name_number_tree` — expect PASS.

**Step 4: Clippy** `cargo clippy -p flpdf --all-targets -- -D warnings` — clean.

**Step 5: Commit**

```bash
git add crates/flpdf/src/name_number_tree.rs
git commit -m "feat(name_number_tree): build_number_tree mirroring build_name_tree (flpdf-9hc.18.6)"
```

---

## Task 2: module skeleton + `LabelStyle` + `LabelRange` + formatters

**Files:** Create `crates/flpdf/src/page_label_document_helper.rs`; modify
`crates/flpdf/src/lib.rs` (add `pub mod page_label_document_helper;` — fmt will
alphabetize it among the `page_*` modules).

**Step 1: Module + types + formatters (TDD: tests written alongside)**

Create `crates/flpdf/src/page_label_document_helper.rs`:

```rust
//! qpdf `QPDFPageLabelDocumentHelper`-equivalent page-label access.
//!
//! [`PageLabelDocumentHelper`] reads, renders (ISO 32000-1 §12.4.2), and edits
//! the catalog `/PageLabels` number tree. [`LabelRange`] models one label range
//! (`/S` style, `/P` prefix, `/St` start). The number-tree walking/building is
//! delegated to [`crate::name_number_tree`].

use crate::{Dictionary, Object, ObjectRef, Pdf, Result};
use std::io::{Read, Seek};

/// Page-label numbering style (ISO 32000-1 §12.4.2 `/S`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelStyle {
    /// `/D` — decimal arabic.
    Decimal,
    /// `/R` — uppercase roman.
    RomanUpper,
    /// `/r` — lowercase roman.
    RomanLower,
    /// `/A` — uppercase letters (A, B, …, Z, AA, …).
    AlphaUpper,
    /// `/a` — lowercase letters.
    AlphaLower,
    /// No `/S` — labels have no numeric portion (prefix only).
    None,
}

impl LabelStyle {
    /// Map a `/S` name's bytes to a style; unrecognised/absent → [`LabelStyle::None`].
    pub fn from_name(name: &[u8]) -> Self {
        match name {
            b"D" => LabelStyle::Decimal,
            b"R" => LabelStyle::RomanUpper,
            b"r" => LabelStyle::RomanLower,
            b"A" => LabelStyle::AlphaUpper,
            b"a" => LabelStyle::AlphaLower,
            _ => LabelStyle::None,
        }
    }

    /// The `/S` name string, or `None` for [`LabelStyle::None`].
    pub fn to_name(self) -> Option<&'static str> {
        match self {
            LabelStyle::Decimal => Some("D"),
            LabelStyle::RomanUpper => Some("R"),
            LabelStyle::RomanLower => Some("r"),
            LabelStyle::AlphaUpper => Some("A"),
            LabelStyle::AlphaLower => Some("a"),
            LabelStyle::None => None,
        }
    }
}

/// One `/PageLabels` range: numbering style, prefix, and starting value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelRange {
    /// Numbering style (`/S`).
    pub style: LabelStyle,
    /// Label prefix (`/P`), decoded; empty when absent.
    pub prefix: String,
    /// First value in the range (`/St`); defaults to 1.
    pub start: i64,
}

impl LabelRange {
    /// Parse a label dictionary (`/S`, `/P`, `/St`). Unrecognised/absent `/S`
    /// → [`LabelStyle::None`]; absent `/St` → 1; `/P` decoded via
    /// [`crate::json_inspect::decode_pdf_text_string`] with lossy fallback.
    pub fn from_dict(dict: &Dictionary) -> Self {
        let style = match dict.get("S") {
            Some(Object::Name(bytes)) => LabelStyle::from_name(bytes),
            _ => LabelStyle::None,
        };
        let prefix = match dict.get("P") {
            Some(Object::String(bytes)) => crate::json_inspect::decode_pdf_text_string(bytes)
                .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned()),
            _ => String::new(),
        };
        let start = match dict.get("St") {
            Some(Object::Integer(n)) => *n,
            _ => 1,
        };
        LabelRange { style, prefix, start }
    }

    /// Build a label dictionary mirroring qpdf `pageLabelDict`: `/S` name when
    /// the style is not [`LabelStyle::None`]; `/P` only when non-empty; `/St`
    /// only when `!= 1`.
    pub fn to_dict(&self) -> Dictionary {
        let mut d = Dictionary::new();
        if let Some(name) = self.style.to_name() {
            d.insert("S", Object::Name(name.into()));
        }
        if !self.prefix.is_empty() {
            d.insert("P", Object::String(self.prefix.clone().into_bytes()));
        }
        if self.start != 1 {
            d.insert("St", Object::Integer(self.start));
        }
        d
    }

    /// Render the display label for `value` (§12.4.2): `prefix` followed by the
    /// style-formatted number. [`LabelStyle::None`] and non-positive numeric
    /// values contribute no numeric portion.
    pub fn format(&self, value: i64) -> String {
        let mut s = self.prefix.clone();
        match self.style {
            LabelStyle::Decimal => s.push_str(&value.to_string()),
            LabelStyle::RomanUpper => s.push_str(&to_roman(value, true)),
            LabelStyle::RomanLower => s.push_str(&to_roman(value, false)),
            LabelStyle::AlphaUpper => s.push_str(&to_alpha(value, true)),
            LabelStyle::AlphaLower => s.push_str(&to_alpha(value, false)),
            LabelStyle::None => {}
        }
        s
    }
}

/// Format `value` as a roman numeral (`upper` → uppercase). Empty for `value <= 0`.
fn to_roman(value: i64, upper: bool) -> String {
    if value <= 0 {
        return String::new();
    }
    const TABLE: &[(i64, &str, &str)] = &[
        (1000, "M", "m"),
        (900, "CM", "cm"),
        (500, "D", "d"),
        (400, "CD", "cd"),
        (100, "C", "c"),
        (90, "XC", "xc"),
        (50, "L", "l"),
        (40, "XL", "xl"),
        (10, "X", "x"),
        (9, "IX", "ix"),
        (5, "V", "v"),
        (4, "IV", "iv"),
        (1, "I", "i"),
    ];
    let mut v = value;
    let mut out = String::new();
    for &(n, up, lo) in TABLE {
        while v >= n {
            out.push_str(if upper { up } else { lo });
            v -= n;
        }
    }
    out
}

/// Format `value` as repeating letters (§12.4.2): 1→A … 26→Z, 27→AA, 53→AAA.
/// Empty for `value <= 0`.
fn to_alpha(value: i64, upper: bool) -> String {
    if value <= 0 {
        return String::new();
    }
    let v = value - 1;
    let letter = (v % 26) as u8;
    let count = (v / 26) + 1;
    let ch = if upper { b'A' + letter } else { b'a' + letter } as char;
    std::iter::repeat(ch).take(count as usize).collect()
}
```

Add `pub mod page_label_document_helper;` to `lib.rs`. NOTE:
`decode_pdf_text_string` is `pub(crate)` in `json_inspect` — it is reachable as
`crate::json_inspect::decode_pdf_text_string`. If the path does not resolve
(visibility), widen it minimally or re-call the same UTF-16/PDFDoc logic; prefer
reusing it.

**Step 2: Formatter + type tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roman_matches_spec() {
        assert_eq!(to_roman(1, true), "I");
        assert_eq!(to_roman(4, true), "IV");
        assert_eq!(to_roman(9, false), "ix");
        assert_eq!(to_roman(40, true), "XL");
        assert_eq!(to_roman(90, false), "xc");
        assert_eq!(to_roman(400, true), "CD");
        assert_eq!(to_roman(900, true), "CM");
        assert_eq!(to_roman(3888, true), "MMMDCCCLXXXVIII");
        assert_eq!(to_roman(0, true), "");
        assert_eq!(to_roman(-3, false), "");
    }

    #[test]
    fn alpha_repeating_letters() {
        assert_eq!(to_alpha(1, true), "A");
        assert_eq!(to_alpha(26, true), "Z");
        assert_eq!(to_alpha(27, true), "AA");
        assert_eq!(to_alpha(52, false), "zz");
        assert_eq!(to_alpha(53, true), "AAA");
        assert_eq!(to_alpha(0, true), "");
    }

    #[test]
    fn label_range_format_prefix_and_styles() {
        let d = LabelRange { style: LabelStyle::Decimal, prefix: "A-".into(), start: 1 };
        assert_eq!(d.format(5), "A-5");
        let r = LabelRange { style: LabelStyle::RomanLower, prefix: String::new(), start: 1 };
        assert_eq!(r.format(3), "iii");
        let none = LabelRange { style: LabelStyle::None, prefix: "Cover".into(), start: 1 };
        assert_eq!(none.format(9), "Cover", "None style => prefix only, no number");
    }

    #[test]
    fn label_range_dict_round_trip() {
        let r = LabelRange { style: LabelStyle::RomanUpper, prefix: "App-".into(), start: 5 };
        let dict = r.to_dict();
        assert_eq!(dict.get("S"), Some(&Object::Name("R".into())));
        assert_eq!(dict.get("St"), Some(&Object::Integer(5)));
        assert_eq!(LabelRange::from_dict(&dict), r);
        // Defaults omitted: St=1 and empty prefix and None style produce empty dict.
        let bare = LabelRange { style: LabelStyle::None, prefix: String::new(), start: 1 };
        assert!(bare.to_dict().iter().next().is_none(), "all-default range => empty dict");
    }
}
```

**Step 3: Run** `cargo test -p flpdf --lib page_label_document_helper` — PASS.
**Step 4: Clippy** clean (e.g. `std::iter::repeat(ch).take(n)` may trip a lint —
if so use `(0..count).map(|_| ch).collect()`).

**Step 5: Commit**

```bash
git add crates/flpdf/src/page_label_document_helper.rs crates/flpdf/src/lib.rs
git commit -m "feat(page_labels): LabelStyle/LabelRange + §12.4.2 formatters (flpdf-9hc.18.6)"
```

---

## Task 3: `PageLabelDocumentHelper` read methods + `Pdf::page_labels()`

**Files:** Modify `crates/flpdf/src/page_label_document_helper.rs`

**Step 1: Helper struct, catalog access, reading methods**

Add above the `tests` module:

```rust
/// Default `/Kids` depth limit for `/PageLabels` walks.
use crate::name_number_tree::DEFAULT_MAX_TREE_DEPTH;

/// High-level helper for a document's `/PageLabels` number tree.
///
/// Construct with [`PageLabelDocumentHelper::new`] or [`Pdf::page_labels`]. The
/// helper caches nothing; methods re-read the live document.
pub struct PageLabelDocumentHelper<'a, R: Read + Seek> {
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> PageLabelDocumentHelper<'a, R> {
    /// Create a new helper borrowing `pdf` mutably.
    pub fn new(pdf: &'a mut Pdf<R>) -> Self {
        Self { pdf }
    }

    /// Resolve the catalog's `/PageLabels` value (Reference or inline dict), or
    /// `None` when absent.
    fn pagelabels_root(&mut self) -> Result<Option<Object>> {
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let Some(catalog) = self.pdf.resolve_borrowed(catalog_ref)?.as_dict() else {
            return Ok(None);
        };
        Ok(catalog.get("PageLabels").cloned())
    }

    /// Whether the document carries a `/PageLabels` tree with at least the root.
    pub fn has_page_labels(&mut self) -> Result<bool> {
        Ok(self.pagelabels_root()?.is_some())
    }

    /// All label ranges as `(first_page_index, LabelRange)`, ascending by index.
    /// Empty when `/PageLabels` is absent.
    pub fn ranges(&mut self) -> Result<Vec<(i64, LabelRange)>> {
        let Some(root) = self.pagelabels_root()? else {
            return Ok(vec![]);
        };
        crate::name_number_tree::read_number_tree(
            self.pdf,
            root,
            |pdf, v| {
                let dict = match v {
                    Object::Dictionary(d) => Some(d),
                    Object::Reference(r) => pdf.resolve_borrowed(r)?.as_dict().cloned(),
                    _ => None,
                };
                Ok(dict.map(|d| LabelRange::from_dict(&d)))
            },
            DEFAULT_MAX_TREE_DEPTH,
        )
    }

    /// The effective label for a 0-based page index (qpdf `getLabelForPage`):
    /// the range whose first index is the largest `<= page_idx`, with `start`
    /// offset to that page. `None` when no range applies (no `/PageLabels`, or
    /// the page precedes the first range).
    pub fn label_for_page(&mut self, page_idx: i64) -> Result<Option<LabelRange>> {
        let ranges = self.ranges()?;
        // ranges is ascending; take the last with first_index <= page_idx.
        let mut chosen: Option<&(i64, LabelRange)> = None;
        for entry in &ranges {
            if entry.0 <= page_idx {
                chosen = Some(entry);
            } else {
                break;
            }
        }
        Ok(chosen.map(|(first, r)| {
            let offset = page_idx - first;
            LabelRange {
                style: r.style,
                prefix: r.prefix.clone(),
                start: r.start + offset,
            }
        }))
    }

    /// The rendered display string for a 0-based page index. Falls back to
    /// 1-based decimal (`(page_idx + 1)`) when no range applies — matching the
    /// "default 1-based numeric labels" requirement.
    pub fn label_string_for_page(&mut self, page_idx: i64) -> Result<String> {
        match self.label_for_page(page_idx)? {
            Some(effective) => Ok(effective.format(effective.start)),
            None => Ok((page_idx + 1).to_string()),
        }
    }
}

/// Extension constructor mirroring [`Pdf::acroform`].
impl<R: Read + Seek> Pdf<R> {
    /// Return a high-level page-label helper for this document.
    pub fn page_labels(&mut self) -> PageLabelDocumentHelper<'_, R> {
        PageLabelDocumentHelper::new(self)
    }
}
```

(Note: `label_for_page` with the offset added to `start` yields a range whose
`.format(start)` gives the page's label; `label_string_for_page` calls
`format(effective.start)`.)

**Step 2: Tests — build a fixture with a multi-range /PageLabels and assert**

Reuse the json_inspect test approach but here use a minimal in-memory PDF. Add a
local fixture helper + tests:

```rust
    use std::io::Cursor;

    fn pdf_with_pagelabels(nums: Vec<Object>) -> Pdf<Cursor<Vec<u8>>> {
        // Minimal one-page PDF; then attach an inline /PageLabels leaf via set_object.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.7\n");
        let off1 = bytes.len() as u64;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = bytes.len() as u64;
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = bytes.len() as u64;
        bytes.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("open");
        // /PageLabels root leaf at obj 10, catalog points to it.
        let pl_ref = ObjectRef::new(10, 0);
        let mut leaf = Dictionary::new();
        leaf.insert("Nums", Object::Array(nums));
        pdf.set_object(pl_ref, Object::Dictionary(leaf));
        let catalog_ref = pdf.root_ref().unwrap();
        let mut catalog = pdf.resolve_borrowed(catalog_ref).unwrap().as_dict().unwrap().clone();
        catalog.insert("PageLabels", Object::Reference(pl_ref));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));
        pdf
    }

    fn label_dict(style: &str, st: Option<i64>, prefix: Option<&str>) -> Object {
        let mut d = Dictionary::new();
        d.insert("S", Object::Name(style.into()));
        if let Some(s) = st {
            d.insert("St", Object::Integer(s));
        }
        if let Some(p) = prefix {
            d.insert("P", Object::String(p.as_bytes().to_vec()));
        }
        Object::Dictionary(d)
    }

    #[test]
    fn label_string_multi_range_matches_spec() {
        // /Nums [0 <</S /r>> 3 <</S /D /St 1>> 6 <</S /D /P "A-" /St 1>>]
        let mut pdf = pdf_with_pagelabels(vec![
            Object::Integer(0),
            label_dict("r", None, None),
            Object::Integer(3),
            label_dict("D", Some(1), None),
            Object::Integer(6),
            label_dict("D", Some(1), Some("A-")),
        ]);
        let mut h = pdf.page_labels();
        assert!(h.has_page_labels().unwrap());
        assert_eq!(h.label_string_for_page(0).unwrap(), "i");
        assert_eq!(h.label_string_for_page(1).unwrap(), "ii");
        assert_eq!(h.label_string_for_page(2).unwrap(), "iii");
        assert_eq!(h.label_string_for_page(3).unwrap(), "1");
        assert_eq!(h.label_string_for_page(5).unwrap(), "3");
        assert_eq!(h.label_string_for_page(6).unwrap(), "A-1");
        assert_eq!(h.label_string_for_page(8).unwrap(), "A-3");
    }

    #[test]
    fn label_for_page_offsets_start() {
        let mut pdf = pdf_with_pagelabels(vec![
            Object::Integer(0),
            label_dict("D", Some(10), None),
        ]);
        let mut h = pdf.page_labels();
        let lab = h.label_for_page(2).unwrap().expect("range applies");
        assert_eq!(lab.start, 12, "/St 10 + offset 2");
        assert_eq!(lab.style, LabelStyle::Decimal);
    }

    #[test]
    fn no_pagelabels_defaults_to_decimal() {
        let mut pdf = pdf_with_pagelabels(vec![]); // empty /Nums -> ranges empty
        // Remove the (empty) /PageLabels to exercise the absent path too:
        let mut h = pdf.page_labels();
        assert_eq!(h.label_string_for_page(0).unwrap(), "1");
        assert_eq!(h.label_string_for_page(4).unwrap(), "5");
        assert!(h.label_for_page(0).unwrap().is_none());
    }

    #[test]
    fn page_before_first_range_defaults_to_decimal() {
        let mut pdf = pdf_with_pagelabels(vec![
            Object::Integer(3),
            label_dict("R", Some(1), None),
        ]);
        let mut h = pdf.page_labels();
        assert_eq!(h.label_string_for_page(0).unwrap(), "1", "page before first range");
        assert_eq!(h.label_string_for_page(3).unwrap(), "I");
    }
```

**Step 3: Run** `cargo test -p flpdf --lib page_label_document_helper` — PASS.
**Step 4: Cross-check (manual, document in commit, not a test):** generate a
fixture and compare structural ranges to `qpdf --json=2 <fixture> | jq .pagelabels`
if convenient; note in the commit that per-page strings are §12.4.2-derived.
**Step 5: Clippy clean. Commit**

```bash
git add crates/flpdf/src/page_label_document_helper.rs
git commit -m "feat(page_labels): PageLabelDocumentHelper read + Pdf::page_labels (flpdf-9hc.18.6)"
```

---

## Task 4: `labels_for_page_range` (qpdf `getLabelsForPageRange` port)

**Files:** Modify `crates/flpdf/src/page_label_document_helper.rs`

**Step 1: Implement on the helper (port qpdf logic verbatim in behavior)**

```rust
    /// qpdf `getLabelsForPageRange` port: collect the label entries needed to
    /// reproduce the labels of pages `start_idx..=end_idx` if they were
    /// renumbered to begin at `new_start_idx`. Returns `(new_index, LabelRange)`
    /// pairs (the first entry plus every explicit entry in the source range),
    /// with the leading entry skipped when it is redundant against an implied
    /// continuation. Read-only; intended for page-extraction wiring (.14.4).
    pub fn labels_for_page_range(
        &mut self,
        start_idx: i64,
        end_idx: i64,
        new_start_idx: i64,
    ) -> Result<Vec<(i64, LabelRange)>> {
        let ranges = self.ranges()?;
        // Set of explicit source indices, for hasIndex().
        let explicit: std::collections::BTreeSet<i64> = ranges.iter().map(|(i, _)| *i).collect();

        let mut out: Vec<(i64, LabelRange)> = Vec::new();

        // First page label (or fabricated default decimal start = 1 + new_start).
        let first_label = match self.label_for_page(start_idx)? {
            Some(r) => r,
            None => LabelRange {
                style: LabelStyle::Decimal,
                prefix: String::new(),
                start: 1 + new_start_idx,
            },
        };

        // Redundancy skip vs the previous pushed entry (same style+prefix and
        // st_delta == idx_delta).
        let mut skip_first = false;
        if out.len() >= 1 {
            // (out is empty at this point; kept for parity/clarity — never true here)
        }
        let _ = &mut skip_first;

        if !skip_first {
            out.push((new_start_idx, first_label.clone()));
        }

        let idx_offset = new_start_idx - start_idx;
        let mut i = start_idx + 1;
        while i <= end_idx {
            if explicit.contains(&i) {
                if let Some(lab) = self.label_for_page(i)? {
                    out.push((i + idx_offset, lab));
                }
            }
            i += 1;
        }
        Ok(out)
    }
```

NOTE on the redundancy skip: qpdf's skip applies when this helper is called
repeatedly appending into one shared vector. As a single self-contained call the
leading entry is always emitted (the vector starts empty). Keep the method
self-contained (always emit the first entry); document this difference from
qpdf's accumulating signature. If a later .14.4 needs the accumulating form, it
can dedupe across calls. Drop the dead `skip_first` scaffold — implement simply:

```rust
        let mut out = vec![(new_start_idx, first_label)];
        let idx_offset = new_start_idx - start_idx;
        for i in (start_idx + 1)..=end_idx {
            if explicit.contains(&i) {
                if let Some(lab) = self.label_for_page(i)? {
                    out.push((i + idx_offset, lab));
                }
            }
        }
        Ok(out)
```

**Step 2: Tests**

```rust
    #[test]
    fn labels_for_page_range_renumbers_and_copies_explicit() {
        // ranges at 0 (roman) and 5 (decimal). Extract pages 3..=6 to new_start 0.
        let mut pdf = pdf_with_pagelabels(vec![
            Object::Integer(0),
            label_dict("r", Some(1), None),
            Object::Integer(5),
            label_dict("D", Some(1), None),
        ]);
        let mut h = pdf.page_labels();
        let out = h.labels_for_page_range(3, 6, 0).unwrap();
        // First page (idx 3) is in the roman range with offset 3 -> start 4.
        assert_eq!(out[0].0, 0);
        assert_eq!(out[0].1.style, LabelStyle::RomanLower);
        assert_eq!(out[0].1.start, 4);
        // Page 5 has an explicit entry -> copied, renumbered to new index 2.
        assert!(out.iter().any(|(idx, r)| *idx == 2 && r.style == LabelStyle::Decimal));
    }
```

**Step 3: Run / clippy / commit**

```bash
git add crates/flpdf/src/page_label_document_helper.rs
git commit -m "feat(page_labels): labels_for_page_range qpdf parity (flpdf-9hc.18.6)"
```

---

## Task 5: `set_range` / `remove_range` (CRUD via build_number_tree)

**Files:** Modify `crates/flpdf/src/page_label_document_helper.rs`

**Step 1: Implement raw collection + rebuild + catalog wiring**

```rust
    /// Collect the raw `(index, value Object)` entries of the `/PageLabels` tree
    /// verbatim (values un-decoded), for rebuild.
    fn raw_entries(&mut self) -> Result<Vec<(i64, Object)>> {
        let Some(root) = self.pagelabels_root()? else {
            return Ok(vec![]);
        };
        crate::name_number_tree::read_number_tree(
            self.pdf,
            root,
            |_, v| Ok(Some(v)),
            DEFAULT_MAX_TREE_DEPTH,
        )
    }

    /// Insert or replace the label range whose first page index is
    /// `first_page_idx`. Rebuilds the `/Nums` tree and points the catalog
    /// `/PageLabels` at the new (indirect) root.
    pub fn set_range(&mut self, first_page_idx: i64, range: LabelRange) -> Result<()> {
        let mut entries = self.raw_entries()?;
        let value = Object::Dictionary(range.to_dict());
        match entries.iter_mut().find(|(k, _)| *k == first_page_idx) {
            Some(e) => e.1 = value,
            None => {
                entries.push((first_page_idx, value));
                entries.sort_by_key(|(k, _)| *k);
            }
        }
        self.rebuild(entries)
    }

    /// Remove the label range whose first page index is `first_page_idx`.
    /// Returns `false` if no such range exists. When the last range is removed,
    /// `/PageLabels` is dropped from the catalog.
    pub fn remove_range(&mut self, first_page_idx: i64) -> Result<bool> {
        let mut entries = self.raw_entries()?;
        let before = entries.len();
        entries.retain(|(k, _)| *k != first_page_idx);
        if entries.len() == before {
            return Ok(false);
        }
        self.rebuild(entries)?;
        Ok(true)
    }

    /// Rebuild `/PageLabels` from sorted entries and patch the catalog. Empty
    /// entries → remove `/PageLabels`.
    fn rebuild(&mut self, entries: Vec<(i64, Object)>) -> Result<()> {
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(());
        };
        let Some(mut catalog) = self.pdf.resolve_borrowed(catalog_ref)?.as_dict().cloned() else {
            return Ok(());
        };

        if entries.is_empty() {
            catalog.remove("PageLabels");
            self.pdf.set_object(catalog_ref, Object::Dictionary(catalog));
            return Ok(());
        }

        let mut next_num: u32 = self
            .pdf
            .object_refs()
            .iter()
            .map(|r| r.number)
            .max()
            .unwrap_or(0);
        let mut alloc = move || -> ObjectRef {
            next_num += 1;
            ObjectRef::new(next_num, 0)
        };

        let (root_ref, nodes) =
            crate::name_number_tree::build_number_tree(&entries, &mut alloc);
        for (r, node) in nodes {
            self.pdf.set_object(r, node);
        }
        catalog.insert("PageLabels", Object::Reference(root_ref));
        self.pdf.set_object(catalog_ref, Object::Dictionary(catalog));
        Ok(())
    }
```

**Step 2: Tests (round-trip)**

```rust
    #[test]
    fn set_range_inserts_and_round_trips() {
        let mut pdf = pdf_with_pagelabels(vec![]); // start with empty /PageLabels root
        {
            let mut h = pdf.page_labels();
            h.set_range(0, LabelRange { style: LabelStyle::RomanLower, prefix: String::new(), start: 1 }).unwrap();
            h.set_range(3, LabelRange { style: LabelStyle::Decimal, prefix: "A-".into(), start: 1 }).unwrap();
        }
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges[0].1.style, LabelStyle::RomanLower);
        assert_eq!(ranges[1].0, 3);
        assert_eq!(ranges[1].1.prefix, "A-");
        assert_eq!(h.label_string_for_page(4).unwrap(), "A-2");
    }

    #[test]
    fn set_range_replaces_existing_index() {
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0), label_dict("D", Some(1), None)]);
        {
            let mut h = pdf.page_labels();
            h.set_range(0, LabelRange { style: LabelStyle::RomanUpper, prefix: String::new(), start: 1 }).unwrap();
        }
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].1.style, LabelStyle::RomanUpper);
    }

    #[test]
    fn remove_range_drops_entry_and_pagelabels_when_empty() {
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0), label_dict("D", Some(1), None)]);
        {
            let mut h = pdf.page_labels();
            assert!(h.remove_range(0).unwrap());
            assert!(!h.remove_range(99).unwrap(), "absent index => false");
        }
        let mut h = pdf.page_labels();
        assert!(!h.has_page_labels().unwrap(), "/PageLabels dropped when empty");
        assert_eq!(h.label_string_for_page(0).unwrap(), "1", "defaults after removal");
    }
```

**Step 3: Run / clippy / commit**

```bash
git add crates/flpdf/src/page_label_document_helper.rs
git commit -m "feat(page_labels): set_range/remove_range CRUD via build_number_tree (flpdf-9hc.18.6)"
```

---

## Task 6: refactor `json_inspect::label_dict_to_json` onto `LabelRange`

**Files:** Modify `crates/flpdf/src/json_inspect.rs`

**Step 1: Reuse `LabelRange::from_dict`, keep JSON byte-identical**

Replace the body of `label_dict_to_json` (lines 881-922) so it derives from
`LabelRange` but emits the same JSON keys (`first`, `prefix`, `style`) with the
same values (style = the `/S` letter string or `JsonValue::Null`):

```rust
fn label_dict_to_json(dict: &Dictionary) -> JsonValue {
    let range = crate::page_label_document_helper::LabelRange::from_dict(dict);
    let style = match range.style.to_name() {
        Some(s) => JsonValue::String(s.to_string()),
        None => JsonValue::Null,
    };
    JsonValue::Object(vec![
        ("first".to_string(), JsonValue::Integer(range.start)),
        ("prefix".to_string(), JsonValue::String(range.prefix)),
        ("style".to_string(), style),
    ])
}
```

This preserves: `first` = `/St` default 1; `prefix` = decoded `/P` (lossy
fallback) or ""; `style` = "D"/"R"/"r"/"A"/"a" or null for absent/unrecognised.

**Step 2: Run the pagelabels JSON tests (must stay green — byte-identical)**

Run: `cargo test -p flpdf --lib json_inspect`
Expected: all pagelabels tests pass unchanged (`pagelabels_single_range_decimal`,
`pagelabels_multiple_ranges`, `pagelabels_no_style_gives_null`, etc.).

**Step 3: Commit**

```bash
git add crates/flpdf/src/json_inspect.rs
git commit -m "refactor(json_inspect): label JSON derives from LabelRange (flpdf-9hc.18.6)"
```

---

## Task 7: exports, rustdoc, final verification

**Files:** Modify `crates/flpdf/src/lib.rs`

**Step 1: Re-export the public API** (alphabetical, near `page_document_helper`):

```rust
pub use page_label_document_helper::{LabelRange, LabelStyle, PageLabelDocumentHelper};
```

And add to the `name_number_tree` re-export line:

```rust
pub use name_number_tree::{
    build_name_tree, build_number_tree, read_name_tree, read_number_tree,
    DEFAULT_MAX_TREE_DEPTH, LEAF_MAX,
};
```

**Step 2: rustdoc** — every `pub` item documented. Run
`cargo doc -p flpdf --no-deps 2>&1 | grep -i warning` — no new warnings for the
new module.

**Step 3: Full suite** `cargo test -p flpdf` — all pass (≥ 1050 lib + integration).

**Step 4: Clippy + fmt**
`cargo clippy -p flpdf --all-targets -- -D warnings` clean;
`cargo fmt -p flpdf -- --check` clean.

**Step 5: qpdf structural cross-check (optional, document result)** — build a
small fixture PDF in-tree with a known /PageLabels and confirm
`qpdf --json=2 fixture.pdf | jq .pagelabels` matches `ranges()` structurally.

**Step 6: Commit**

```bash
git add crates/flpdf/src/lib.rs
git commit -m "feat(page_labels): export public API + build_number_tree (flpdf-9hc.18.6)"
```

---

## Done criteria

- `PageLabelDocumentHelper` (has_page_labels, ranges, label_for_page,
  label_string_for_page, labels_for_page_range, set_range, remove_range) +
  `Pdf::page_labels()`, plus `LabelStyle`/`LabelRange`, all documented & exported.
- `name_number_tree::build_number_tree` added + exported, mirroring
  `build_name_tree`, round-trip-tested.
- `label_dict_to_json` derives from `LabelRange`; pagelabels JSON unchanged.
- Formatters match §12.4.2 (roman edge values, alpha Z→AA, non-positive → "").
- `cargo test -p flpdf` green; clippy `-D warnings` clean; fmt clean.
- Closes .14.3 on merge; .14.4 (page-op rebalance) stays open with
  build_number_tree + labels_for_page_range as its infra.

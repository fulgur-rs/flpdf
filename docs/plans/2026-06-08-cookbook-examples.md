# Cookbook Examples (flpdf-9hc.18.9) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add five idiomatic, self-verifying cookbook examples (extract first 5 pages, list
form fields, walk outline, pull attachments, reorder pages) mirroring qpdf's example programs,
cross-reference them from the helper API docs, and add a README pointer to the examples dir.

**Architecture:** Each example is a standalone `crates/flpdf/examples/*.rs` `main()` that builds
a hermetic synthetic fixture (in-memory bytes or via the library's own write API), runs the
helper API it demonstrates, and `assert!`s the result so `cargo run --example` self-verifies.
Shared fixture builders live in `examples/common/mod.rs` (already included via
`#[path = "common/mod.rs"] mod common;`). Doc cross-references use the existing plain-backtick
`` `examples/foo.rs` `` convention (examples are not intra-doc linkable).

**Tech Stack:** Rust, flpdf crate public API (`Pdf::open`, `PagePlan`, `rebuild_page_tree`,
`Pdf::acroform`, `Pdf::outline`, `list_attachment_info` / `extract_attachment`,
`PageDocumentHelper`, `FileSpecBuilder` / `insert_embedded_file`).

**Working dir:** `/home/ubuntu/flpdf/.worktrees/flpdf-9hc.18.9-examples`

**Key conventions to mirror (from existing `examples/extract_pages.rs`):**
- `WriteOptions` is `#[non_exhaustive]`: build from `Default` + mutate, with
  `#[allow(clippy::field_reassign_with_default)]`.
- Use temp files via `common::temp_path` / `common::write_temp`; `drop()` open handles before
  `remove_file` (Windows note in a comment).
- End with a `println!` summary line.
- Module doc `//!` at top with a `Run with: cargo run --example <name> -p flpdf` line.

---

## Task 1: Fixture builders — AcroForm + Outline PDFs

**Files:**
- Modify: `crates/flpdf/examples/common/mod.rs`

Add two synthetic byte builders next to `build_shared_font_pdf`. Hand-build minimal valid
PDFs with a correct `xref` table. Pattern-match the offset/xref bookkeeping already used in
`build_shared_font_pdf`.

**Step 1: Add `build_acroform_pdf`**

```rust
/// Build a minimal single-page PDF carrying an interactive form (`/AcroForm`)
/// with two top-level fields: a text field `FirstName` (value `Alice`) and a
/// checkbox `Agree` (value `/Off`). Each field dictionary is also its widget
/// annotation (the merged field/widget form), referenced from the page `/Annots`.
///
/// Object layout:
///   1: Catalog (`/Pages`, `/AcroForm`)
///   2: Pages root
///   3: AcroForm (`/Fields`, `/DA`)
///   4: Page (`/Annots` -> 5, 6)
///   5: text field/widget `FirstName`
///   6: checkbox field/widget `Agree`
pub fn build_acroform_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    let objs: [(u32, &str); 6] = [
        (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 3 0 R >>"),
        (2, "<< /Type /Pages /Kids [4 0 R] /Count 1 >>"),
        (3, "<< /Fields [5 0 R 6 0 R] /DA (/Helv 0 Tf 0 g) /NeedAppearances true >>"),
        (
            4,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R 6 0 R] >>",
        ),
        (
            5,
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (FirstName) /V (Alice) \
             /Rect [100 700 300 720] >>",
        ),
        (
            6,
            "<< /Type /Annot /Subtype /Widget /FT /Btn /T (Agree) /V /Off \
             /Rect [100 660 120 680] >>",
        ),
    ];
    for (n, body) in objs {
        offsets.insert(n, out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    append_xref_trailer(&mut out, &offsets, 6);
    out
}
```

**Step 2: Add `build_outline_pdf`**

```rust
/// Build a minimal 2-page PDF with a document outline (`/Outlines`) two levels
/// deep: `Chapter 1` (with child `Section 1.1`) and `Chapter 2`. Each item has
/// an explicit `/Dest [page /Fit]`.
///
/// Object layout:
///   1: Catalog (`/Pages`, `/Outlines`)
///   2: Pages root (Kids 4, 5)
///   3: Outlines root (First 6, Last 7, Count 3)
///   4,5: Page objects (share font 8)
///   6: item `Chapter 1`   (Parent 3, First/Last 9, Next 7, Dest -> 4)
///   7: item `Chapter 2`   (Parent 3, Prev 6,        Dest -> 5)
///   8: shared Font
///   9: item `Section 1.1` (Parent 6,                Dest -> 4)
pub fn build_outline_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    let page = "/Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                /Resources << /Font << /F1 8 0 R >> >>";
    let objs: [(u32, String); 9] = [
        (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 3 0 R >>".into()),
        (2, "<< /Type /Pages /Kids [4 0 R 5 0 R] /Count 2 >>".into()),
        (3, "<< /Type /Outlines /First 6 0 R /Last 7 0 R /Count 3 >>".into()),
        (4, format!("<< {page} >>")),
        (5, format!("<< {page} >>")),
        (
            6,
            "<< /Title (Chapter 1) /Parent 3 0 R /First 9 0 R /Last 9 0 R \
             /Count 1 /Next 7 0 R /Dest [4 0 R /Fit] >>"
                .into(),
        ),
        (
            7,
            "<< /Title (Chapter 2) /Parent 3 0 R /Prev 6 0 R /Dest [5 0 R /Fit] >>".into(),
        ),
        (8, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into()),
        (
            9,
            "<< /Title (Section 1.1) /Parent 6 0 R /Dest [4 0 R /Fit] >>".into(),
        ),
    ];
    for (n, body) in objs {
        offsets.insert(n, out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    append_xref_trailer(&mut out, &offsets, 9);
    out
}
```

**Step 3: Extract a shared `append_xref_trailer` helper** (DRY — `build_shared_font_pdf`'s
xref tail can also call it; refactor it to do so).

```rust
/// Append a classic `xref` table + `trailer` + `startxref`/`%%EOF` for objects
/// `1..=last`. `offsets` must contain a byte offset for every object `1..=last`.
fn append_xref_trailer(out: &mut Vec<u8>, offsets: &BTreeMap<u32, u64>, last: u32) {
    let xref_start = out.len() as u64;
    let size = last + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for n in 1..=last {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&n]).as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
}
```

Refactor `build_shared_font_pdf` to use `append_xref_trailer` (replace its inline xref tail).

**Step 4: Verify builders compile**

Run: `cargo build --examples -p flpdf`
Expected: PASS (builders are `dead_code`-allowed; nothing uses them yet, that's fine).

**Step 5: Commit**

```bash
git add crates/flpdf/examples/common/mod.rs
git commit -m "test(examples): add AcroForm + outline fixture builders [flpdf-9hc.18.9]"
```

---

## Task 2: Example — extract first 5 pages

**Files:**
- Create: `crates/flpdf/examples/extract_first_5_pages.rs`

**Step 1: Write the example** (mirrors `extract_pages.rs`, but contiguous 1..=5 on an 8-page src)

```rust
//! Extract the first 5 pages of a document into a new file.
//!
//! Run with: `cargo run --example extract_first_5_pages -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::fs::File;
use std::io::{BufReader, BufWriter};

use flpdf::{pages::page_refs, rebuild_page_tree, ObjectRef, PagePlan, Pdf, WriteOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // An 8-page source document (all pages share one font object).
    let src_path = common::write_temp("first5-src", &common::build_shared_font_pdf(8))?;
    let out_path = common::temp_path("first5-out");

    let mut pdf = Pdf::open(BufReader::new(File::open(&src_path)?))?;

    // Select the first five pages (1-based 1..=5).
    let plan = PagePlan::from_1based_indices(&mut pdf, &[1, 2, 3, 4, 5])?;
    let selected: Vec<ObjectRef> = plan.pages().iter().map(|p| p.page_ref).collect();

    rebuild_page_tree(&mut pdf, &selected)?;

    #[allow(clippy::field_reassign_with_default)]
    let opts = {
        let mut opts = WriteOptions::default();
        opts.full_rewrite = true;
        opts
    };
    let out = BufWriter::new(File::create(&out_path)?);
    flpdf::write_pdf_with_options(&mut pdf, out, &opts)?;

    let mut out_pdf = Pdf::open(BufReader::new(File::open(&out_path)?))?;
    let count = page_refs(&mut out_pdf)?.len();
    assert_eq!(count, 5, "expected 5 pages, got {count}");
    println!("extract_first_5_pages: output has {count} pages");

    drop(pdf);
    drop(out_pdf);
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&out_path);
    Ok(())
}
```

**Step 2: Run it**

Run: `cargo run --example extract_first_5_pages -p flpdf`
Expected: prints `extract_first_5_pages: output has 5 pages`, exit 0.

**Step 3: Commit**

```bash
git add crates/flpdf/examples/extract_first_5_pages.rs
git commit -m "docs(examples): add extract_first_5_pages cookbook example [flpdf-9hc.18.9]"
```

---

## Task 3: Example — list all form fields

**Files:**
- Create: `crates/flpdf/examples/list_form_fields.rs`

**Step 1: Write the example**

```rust
//! List every interactive form field with its type and value.
//!
//! Run with: `cargo run --example list_form_fields -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::fs::File;
use std::io::BufReader;

use flpdf::Pdf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let src_path = common::write_temp("forms-src", &common::build_acroform_pdf())?;
    let mut pdf = Pdf::open(BufReader::new(File::open(&src_path)?))?;

    // `field_infos` reconstructs the dotted full name, resolves inherited
    // `/FT` / `/V`, and follows indirect references for us.
    let infos = pdf.acroform().field_infos()?;
    for info in &infos {
        let ft = info
            .field_type
            .as_deref()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_else(|| "?".into());
        let value = match &info.value {
            Some(v) => format!("{v:?}"),
            None => "<none>".into(),
        };
        println!("  {} : /{} = {}", info.full_name, ft, value);
    }
    assert_eq!(infos.len(), 2, "expected 2 form fields, got {}", infos.len());
    println!("list_form_fields: {} field(s)", infos.len());

    drop(pdf);
    let _ = std::fs::remove_file(&src_path);
    Ok(())
}
```

> Executor note: confirm the printed value form for a PDF string. If `info.value` is a
> `Object::String`, `{v:?}` is acceptable for a demo; keep it simple.

**Step 2: Run it**

Run: `cargo run --example list_form_fields -p flpdf`
Expected: prints two field lines (`FirstName` /Tx, `Agree` /Btn) and a summary; exit 0.

**Step 3: Commit**

```bash
git add crates/flpdf/examples/list_form_fields.rs
git commit -m "docs(examples): add list_form_fields cookbook example [flpdf-9hc.18.9]"
```

---

## Task 4: Example — walk the outline

**Files:**
- Create: `crates/flpdf/examples/walk_outline.rs`

**Step 1: Write the example**

```rust
//! Walk the document outline (bookmarks), printing an indented tree.
//!
//! Run with: `cargo run --example walk_outline -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::cell::Cell;
use std::fs::File;
use std::io::BufReader;

use flpdf::Pdf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let src_path = common::write_temp("outline-src", &common::build_outline_pdf())?;
    let mut pdf = Pdf::open(BufReader::new(File::open(&src_path)?))?;

    let visited = Cell::new(0usize);
    // `walk` performs a depth-first traversal, handing each node its depth.
    pdf.outline().walk(|node, depth| {
        println!("{}{}", "  ".repeat(depth), node.title);
        visited.set(visited.get() + 1);
    })?;

    assert_eq!(visited.get(), 3, "expected 3 outline items, got {}", visited.get());
    println!("walk_outline: visited {} outline item(s)", visited.get());

    drop(pdf);
    let _ = std::fs::remove_file(&src_path);
    Ok(())
}
```

> Executor note: verify `walk`'s closure signature is `FnMut(&OutlineNode, usize)` (it is, per
> `outline_document_helper.rs:354`). A `Cell` avoids borrow conflicts; or use a plain `&mut`
> counter if the closure allows it. Adjust to whatever compiles cleanly.

**Step 2: Run it**

Run: `cargo run --example walk_outline -p flpdf`
Expected:
```
Chapter 1
  Section 1.1
Chapter 2
walk_outline: visited 3 outline item(s)
```

**Step 3: Commit**

```bash
git add crates/flpdf/examples/walk_outline.rs
git commit -m "docs(examples): add walk_outline cookbook example [flpdf-9hc.18.9]"
```

---

## Task 5: Example — pull all attachments

**Files:**
- Create: `crates/flpdf/examples/pull_attachments.rs`

**Step 1: Write the example** — build the fixture with the library's own write API, then list
+ extract.

```rust
//! Pull every embedded attachment out of a document to disk.
//!
//! Run with: `cargo run --example pull_attachments -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::fs::File;
use std::io::{BufReader, BufWriter};

use flpdf::{
    extract_attachment, list_attachment_info, FileSpecBuilder, Pdf, WriteOptions,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Build a base PDF, then attach two files using the library's own API.
    let base_path = common::write_temp("attach-base", &common::build_shared_font_pdf(1))?;
    let with_files = common::temp_path("attach-src");
    {
        let mut pdf = Pdf::open(BufReader::new(File::open(&base_path)?))?;
        attach(&mut pdf, "notes.txt", b"hello from flpdf")?;
        attach(&mut pdf, "data.csv", b"a,b,c\n1,2,3\n")?;
        #[allow(clippy::field_reassign_with_default)]
        let opts = {
            let mut opts = WriteOptions::default();
            opts.full_rewrite = true;
            opts
        };
        let out = BufWriter::new(File::create(&with_files)?);
        flpdf::write_pdf_with_options(&mut pdf, out, &opts)?;
    }

    // Re-open and pull each attachment back out.
    let mut pdf = Pdf::open(BufReader::new(File::open(&with_files)?))?;
    let infos = list_attachment_info(&mut pdf)?;
    let mut pulled = 0usize;
    for info in &infos {
        let bytes = extract_attachment(&mut pdf, &info.key)?;
        let name = info.display_name.clone().unwrap_or_else(|| "<unnamed>".into());
        println!("  pulled {} ({} bytes)", name, bytes.len());
        pulled += 1;
    }
    assert_eq!(pulled, 2, "expected 2 attachments, got {pulled}");
    println!("pull_attachments: pulled {pulled} attachment(s)");

    drop(pdf);
    let _ = std::fs::remove_file(&base_path);
    let _ = std::fs::remove_file(&with_files);
    Ok(())
}

fn attach<R: std::io::Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
    name: &str,
    payload: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    // FileSpecBuilder creates the /Filespec + /EmbeddedFile and registers it
    // in the document's /Names /EmbeddedFiles name tree.
    FileSpecBuilder::new(name.as_bytes(), payload.to_vec()).build(pdf)?;
    Ok(())
}
```

> Executor note (IMPORTANT — verify against source before trusting this code):
> - Confirm `FileSpecBuilder::new(...).build(&mut pdf)` actually registers the filespec in the
>   `/Names /EmbeddedFiles` name tree so `list_attachment_info` finds it. Read
>   `filespec_helper.rs:625` (`build`). If `build` only creates the object WITHOUT name-tree
>   registration, use `insert_embedded_file(&mut pdf, key, ...)` (`embedded_files.rs:478`)
>   instead — check its signature and use it. The attachment MUST be discoverable by
>   `list_attachment_info` (`attachment_list.rs:120`), which walks the name tree.
> - `extract_attachment(&mut pdf, key)` keys off the name-tree key (`info.key`), per
>   `filespec_helper.rs:878`.
> Pick whichever API combination round-trips; the asserts will tell you.

**Step 2: Run it**

Run: `cargo run --example pull_attachments -p flpdf`
Expected: prints two `pulled ...` lines + summary; exit 0.

**Step 3: Commit**

```bash
git add crates/flpdf/examples/pull_attachments.rs
git commit -m "docs(examples): add pull_attachments cookbook example [flpdf-9hc.18.9]"
```

---

## Task 6: Example — reorder pages

**Files:**
- Create: `crates/flpdf/examples/reorder_pages.rs`

**Step 1: Write the example** — reverse a 3-page document via `rebuild_page_tree`.

```rust
//! Reorder a document's pages (here: reverse them) and write the result.
//!
//! Run with: `cargo run --example reorder_pages -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::fs::File;
use std::io::{BufReader, BufWriter};

use flpdf::{pages::page_refs, rebuild_page_tree, Pdf, WriteOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let src_path = common::write_temp("reorder-src", &common::build_shared_font_pdf(3))?;
    let out_path = common::temp_path("reorder-out");

    let mut pdf = Pdf::open(BufReader::new(File::open(&src_path)?))?;

    // Current page order, then reversed.
    let original = page_refs(&mut pdf)?;
    let mut reversed = original.clone();
    reversed.reverse();

    rebuild_page_tree(&mut pdf, &reversed)?;

    #[allow(clippy::field_reassign_with_default)]
    let opts = {
        let mut opts = WriteOptions::default();
        opts.full_rewrite = true;
        opts
    };
    let out = BufWriter::new(File::create(&out_path)?);
    flpdf::write_pdf_with_options(&mut pdf, out, &opts)?;

    // Re-open: the new first page is the old last page.
    let mut out_pdf = Pdf::open(BufReader::new(File::open(&out_path)?))?;
    let new_order = page_refs(&mut out_pdf)?;
    assert_eq!(new_order.len(), 3, "expected 3 pages");
    println!(
        "reorder_pages: {} pages, reversed order applied",
        new_order.len()
    );

    drop(pdf);
    drop(out_pdf);
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&out_path);
    Ok(())
}
```

> Executor note: `page_refs` returns refs in page order. After `rebuild_page_tree(&reversed)`,
> the rewritten doc's page objects may be renumbered, so don't assert ref equality across the
> rewrite — assert on *count* and (optionally) that order changed by comparing page content if
> cheap. Keep the demo assert simple (count == 3). Consider using `PageDocumentHelper` if you
> prefer; either path satisfies "reorder pages". Pick the one that reads cleanest.

**Step 2: Run it**

Run: `cargo run --example reorder_pages -p flpdf`
Expected: prints summary; exit 0.

**Step 3: Commit**

```bash
git add crates/flpdf/examples/reorder_pages.rs
git commit -m "docs(examples): add reorder_pages cookbook example [flpdf-9hc.18.9]"
```

---

## Task 7: Cross-reference examples from helper API docs

**Files:**
- Modify: `crates/flpdf/src/acroform_document_helper.rs` (helper struct doc or `field_infos` doc)
- Modify: `crates/flpdf/src/outline_document_helper.rs` (helper struct doc or `walk` doc)
- Modify: `crates/flpdf/src/attachment_list.rs` (`list_attachment_info` doc)
- Modify: `crates/flpdf/src/page_document_helper.rs` (helper struct doc)
- Optional: `crates/flpdf/src/page_plan.rs` / `page_tree_rebuild.rs` (note extract_first_5_pages)

**Rules (from `.claude/rules/pdf-rust-doc-review-patterns.md`):** published `///`, English only,
NO beads issue IDs, plain backtick path (examples are not intra-doc linkable), no broken
intra-doc links, no internal jargon.

**Step 1: Add one sentence each**, matching the existing convention, e.g. in
`page_plan.rs:63` (`` see the runnable `examples/extract_pages.rs` ``). Examples:
- acroform helper: `` /// For a runnable walkthrough see `examples/list_form_fields.rs`. ``
- outline helper / `walk`: `` /// For a runnable walkthrough see `examples/walk_outline.rs`. ``
- `list_attachment_info`: `` /// For a runnable walkthrough see `examples/pull_attachments.rs`. ``
- page document helper: `` /// For a runnable walkthrough see `examples/reorder_pages.rs`. ``

Place each on the relevant doc comment (struct-level or method-level), not as a trailing `//`.

**Step 2: Verify docs build with no broken links**

Run: `cargo doc -p flpdf --no-deps 2>&1 | grep -i warning` → expect no `broken_intra_doc_links`.
Run: `cargo test --doc -p flpdf` → expect PASS.

**Step 3: Commit**

```bash
git add crates/flpdf/src/*.rs
git commit -m "docs: cross-reference cookbook examples from helper API [flpdf-9hc.18.9]"
```

---

## Task 8: README pointer to examples directory

**Files:**
- Modify: `README.md` (repo root)

**Step 1: Add an "Examples" section** listing the runnable examples and the
`cargo run --example <name> -p flpdf` invocation, pointing at `crates/flpdf/examples/`.
Cover all runnable examples (existing + new): `inspect`, `extract_page`, `extract_pages`,
`extract_first_5_pages`, `list_form_fields`, `walk_outline`, `pull_attachments`,
`reorder_pages`, `merge_pdfs`, `splice_pages`.

**Step 2: Verify** the section renders (no broken markdown) and paths are correct.

**Step 3: Commit**

```bash
git add README.md
git commit -m "docs: add examples directory pointer to README [flpdf-9hc.18.9]"
```

---

## Task 9: Final verification (acceptance gate)

**Step 1: Build all examples**

Run: `cargo build --examples -p flpdf` → PASS.

**Step 2: RUN every example** (acceptance: "compile + run on fixtures") — existing + new:

```bash
for ex in inspect extract_page extract_pages extract_first_5_pages \
          list_form_fields walk_outline pull_attachments reorder_pages \
          merge_pdfs splice_pages; do
  echo "== $ex =="; cargo run --quiet --example "$ex" -p flpdf || exit 1
done
```
Expected: every example exits 0 (asserts pass).

**Step 3: Lint + doctest**

Run: `cargo clippy --examples -p flpdf -- -D warnings` → PASS.
Run: `cargo test --doc -p flpdf` → PASS.

**Step 4:** Confirm the four deliverables are all met:
- [ ] 5 new examples present and runnable
- [ ] helper API docs cross-reference them (English, no issue IDs)
- [ ] README points at examples dir
- [ ] all examples compile AND run on fixtures

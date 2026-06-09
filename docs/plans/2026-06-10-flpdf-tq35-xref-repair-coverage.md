# xref.rs Repair-Path Coverage Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (or subagent-driven-development) to implement this plan task-by-task.

**Goal:** Raise `crates/flpdf/src/xref.rs` line coverage above its 74.60% baseline by adding hand-built corrupt-PDF fixtures that drive the previously-uncovered repair and error arms through the public API.

**Architecture:** Pure test-only work. No production code changes. All `recover_*` / `format_repair_diagnostic` / `merge_previous_xref_sections` / `parse_xref_*` / `ByteCursor::*` helpers are private, so every branch is reached through the public entry points `load_xref_and_trailer` (strict), `load_xref_and_trailer_with_repair(_, true)`, and `load_xref_and_trailer_best_effort`. Tests are added to the existing `crates/flpdf/tests/xref_tests.rs`, following its existing fixture-builder idiom (`corrupt_xref_pdf()`).

**Tech Stack:** Rust, `cargo test -p flpdf --test xref_tests`, `cargo llvm-cov --workspace` for the coverage delta.

**Measurement (acceptance evidence):** Baseline reproduced with `cargo llvm-cov --workspace --summary-only` = `flpdf/src/xref.rs` **74.60% line / 144 uncovered / 66.15% fn**. Final task re-measures and records before→after.

**Out of scope / documented-unreachable on 64-bit:** Lines 88-90, 92 (`usize::try_from(startxref: u64)` overflow) and 164-165 (`/Prev` u64→usize overflow) cannot fire where `usize == u64`. Do NOT chase them; note them in a code comment as 32-bit-only defensive arms.

**Reference for uncovered lines (from `cargo llvm-cov report --show-missing-lines`):**
`80-82, 88-90, 92, 114-115, 119-122, 168-169, 171, 252-258, 269-272, 278-323, 325-330, 334, 340-348, 359, 367-370, 376, 425-426, 436, 446-449, 470-472, 482, 506, 510, 525-528, 542, 550, 566, 581, 585, 588-590, 616-618, 627-629, 640, 643, 714, 737, 756-759, 774`

**Conventions to follow (read first):**
- Existing fixture builder `corrupt_xref_pdf()` at `crates/flpdf/tests/xref_tests.rs:279`.
- Existing imports at top of `xref_tests.rs` (`load_xref_and_trailer`, `load_xref_and_trailer_best_effort`, `Error`, `ObjectRef`, `XrefForm`, plus `XrefOffset` used in tests). Add `load_xref_and_trailer_with_repair` to the import list when needed; confirm it is re-exported from the crate root (`crates/flpdf/src/lib.rs`) before importing — if it is not public, drive the same branches via `load_xref_and_trailer_best_effort`.
- Helper to build encoded xref streams already exists: `build_encoded_xref_stream_entries` / `make_xref_stream_object` at `xref_tests.rs:220`/`:230`. Reuse these for malformed-stream fixtures (mutate one field) rather than hand-encoding from scratch.
- Tests assert on the public surface only: returned `LoadedXref` fields (`entries`, `trailer`, `repair_diagnostics`, `version`, `last_xref_form`) for success, and `Error` variant + message substring for failures.

---

### Task 1: Repair-path recovery arms (best-effort / with-repair)

**Files:**
- Modify: `crates/flpdf/tests/xref_tests.rs` (append tests + fixture helpers near the existing `corrupt_xref_pdf` block)

Covers lines: `80-82, 114-115, 119-122, 168-169, 171, 252-258, 269-272, 334, 340-348, 359, 367-370` and `recover_compressed_offsets_from_objstm` success path `278-323, 325-330`.

**Step 1: Write failing tests** — add these tests. Build on `corrupt_xref_pdf()` and its valid sibling (a well-formed PDF where you then corrupt one thing). Keep each fixture minimal.

1. `best_effort_recovers_objstm_compressed_entries`
   - Build a PDF whose objects include a valid `/Type /ObjStm` stream (N objects, decodable — reuse the writer or hand-encode a tiny uncompressed ObjStm with `/Filter` absent so `decode_stream_data` is a no-op). Corrupt the `xref` token so strict parse fails and `load_xref_and_trailer_best_effort` falls into `recover_xref_entries`, which must detect the ObjStm (lines 252-258) and populate `XrefOffset::Compressed` entries (drives `recover_compressed_offsets_from_objstm` 278-323 and `parse_non_negative_i64` 325-330 on the success path).
   - Assert at least one recovered entry is `XrefOffset::Compressed { stream, .. }` with the ObjStm's object number.

2. `best_effort_errors_when_no_objects_to_recover`
   - Header + `startxref`/trailer but NO indirect objects (e.g. only `%PDF-1.7\n` and a corrupt xref). `load_xref_and_trailer_best_effort` must `Err` with message containing `"unable to recover xref entries"` (lines 269-272).

3. `best_effort_errors_when_trailer_missing`
   - A PDF with a recoverable indirect object but the `trailer` keyword removed/corrupted, so `recover_trailer` returns `"trailer dictionary not found"` (line 359). (Needs `recover_xref_entries` to succeed first, then `recover_trailer` to fail.)

4. `best_effort_errors_when_trailer_not_dictionary`
   - `trailer` keyword present but followed by a non-dict token (e.g. `trailer\n42\n`). Drives `recover_trailer` 367-370 (`"trailer dictionary is not a dictionary"`). Ensure a recoverable object exists so the scan reaches `recover_trailer`.

5. `repair_diagnostic_aggregates_multiple_errors`
   - Construct a PDF that makes BOTH `parse_startxref` fail (no `startxref`) AND the linear scan still succeed, accumulating ≥2 entries in `parse_errors` so `format_repair_diagnostic` hits the multi-error arm (340-348). Assert the diagnostic message contains `"; "` joining clauses, or simply that two distinct error fragments appear. (If two errors cannot both be accumulated through the public path, fall back to asserting the single-error path stays covered and document the limitation in a comment — do not fabricate.)

6. `with_repair_diagnostic_on_otherwise_valid_parse` (lines 119-122)
   - A PDF where `parse_startxref` fails (so `parse_errors` is non-empty, startxref=0) BUT `parse_xref_from_start` at offset 0 still… most likely this path is only reachable when startxref recovery still yields a parseable xref. If unreachable through the public API, cover the equivalent warning-append behavior via the best-effort linear-scan diagnostic instead and note line 119-122 may be unreachable. Investigate, don't force.

7. `circular_prev_recovers_with_repair` + `circular_prev_rejected_strict` (lines 168-169, 171)
   - Build a valid initial xref whose trailer's `/Prev` points back to itself (or to an offset whose xref also has `/Prev` pointing to the first), forming a cycle. Strict `load_xref_and_trailer` must `Err` with `"xref /Prev is circular"` (171). `load_xref_and_trailer_best_effort` (or `_with_repair(_, true)`) must return `Ok` (168-169) — the cycle is tolerated.

   For lines 114-115 (`merge_previous_xref_sections` error under repair falls into linear scan): make a `/Prev` that points to a malformed xref location (not circular, just garbage) and confirm best-effort returns `Ok` via linear scan with a repair diagnostic.

**Step 2: Run to verify** — `cargo test -p flpdf --test xref_tests 2>&1 | tail -20`. Each new test must PASS (the production code already handles these branches; a failure means a real bug — stop and report).

**Step 3: Commit**
```bash
git add crates/flpdf/tests/xref_tests.rs
git commit -m "test(flpdf): cover xref repair/recovery arms (flpdf-tq35)"
```

---

### Task 2: Strict xref-table error arms

**Files:**
- Modify: `crates/flpdf/tests/xref_tests.rs`

Covers lines: `425-426` (free-entry next does not fit u32), `436` (entry status not `f`/`n`), `446-449` (table trailer not a dictionary).

**Step 1: Write failing tests** (strict mode, `load_xref_and_trailer` expecting `Err`):

1. `rejects_xref_table_free_next_overflow` — an `f` entry whose 10-digit offset field exceeds `u32::MAX` (e.g. `9999999999`). Expect `Error::Parse` containing `"free xref next object does not fit u32"`.
2. `rejects_xref_table_bad_entry_status` — an entry whose status byte is neither `f` nor `n` (e.g. `x`). Expect message `"xref table entry status is not f or n"`.
3. `rejects_xref_table_trailer_not_dictionary` — well-formed entries but `trailer` followed by a non-dict (e.g. an integer). Expect `"trailer is not a dictionary"`.

Reuse `corrupt_xref_pdf`'s table-building style but keep these as small standalone fixtures.

**Step 2: Run** — `cargo test -p flpdf --test xref_tests 2>&1 | tail -20`, all PASS.

**Step 3: Commit**
```bash
git add crates/flpdf/tests/xref_tests.rs
git commit -m "test(flpdf): cover strict xref-table error arms (flpdf-tq35)"
```

---

### Task 3: Strict xref-stream error arms + ByteCursor edges

**Files:**
- Modify: `crates/flpdf/tests/xref_tests.rs`

Covers lines: `470-472, 506, 510, 525-528, 542, 550, 566, 581, 585, 588-590, 616-618, 627-629, 640, 643, 714, 737, 756-759, 774` (skip the 64-bit-unreachable `usize` overflow arms).

**Step 1: Write failing tests.** Use `make_xref_stream_object` / `build_encoded_xref_stream_entries` (xref_tests.rs:220-255) and mutate one field per fixture. Strict mode, expecting `Err` unless noted:

1. `rejects_xref_stream_non_stream_object` (470-472) — `startxref` points at an indirect object that is a dictionary, not a stream. Expect `Error::Unsupported("xref stream expected an indirect object stream")`.
2. `rejects_xref_stream_w_not_array` (506) — `/W` is an integer. `"/W must be array"`.
3. `rejects_xref_stream_w_wrong_length` (510) — `/W [1 1]`. `"/W must contain three integers"`.
4. `rejects_xref_stream_index_odd_length` (525-528) — `/Index [0]`. `"/Index must contain an even number of integers"`.
5. `rejects_xref_stream_index_not_array` (542) — `/Index 5`. `"/Index must be array"`.
6. `xref_stream_index_zero_count_range_skipped` (550) — `/Index [0 0 1 1]` so `build_xref_ranges` skips the zero-count chunk; assert it loads OK and only object 1 is present. (Success case.)
7. `rejects_xref_stream_zero_widths` (566) — `/W [0 0 0]`. `"invalid cross-reference stream widths"`.
8. `rejects_xref_stream_truncated_data` (581) — `/W` implies an entry width larger than the decoded stream data. `"xref stream data truncated"`.
9. `loads_xref_stream_with_w0_zero_defaults_type_one` (585) — `/W [0 4 2]` (object type defaults to 1). Assert it loads and produces `XrefOffset::Offset` entries. (Success case.)
10. `rejects_xref_stream_object_type_overflow` (588-590) — `/W [2 ..]` with a type field value > 255. `"xref stream object type does not fit u8"`.
11. `rejects_xref_stream_type2_stream_number_overflow` (616-618) — a type-2 entry whose field1 exceeds `u32::MAX` (needs `w1 >= 5`). `"xref stream object number does not fit u32"`.
12. `rejects_xref_stream_unsupported_entry_type` (627-629) — an entry with object_type 3 (`/W [1 ..]` value 3). `Error::Unsupported` containing `"unsupported xref entry type 3"`.
13. `rejects_xref_stream_size_not_integer` (640) — `/Size` is a name, exercising `parse_non_negative_u64`'s non-integer arm. `"/Size is not integer"`. (Choose whichever `parse_non_negative_u64` caller is easiest; `/Size` is read at xref_tests-visible point.)
14. `rejects_xref_stream_negative_size` (643) — `/Size -1`. `"/Size is negative"`.

ByteCursor edge arms (reachable via malformed xref *table* widths/short input):
15. `rejects_xref_table_truncated_entry` — an xref table truncated mid-entry so `ByteCursor::read_fixed`/`read_byte` hit end-of-input (covers 714, 756-759). Expect a parse error (`"unexpected end of"` …).
16. `rejects_xref_stream_field_truncated` (737) — a stream whose declared width runs past the decoded data inside `read_be_u64`. May overlap with #8; if #8 already covers 737, drop this and note it.
17. `rejects_xref_table_missing_object_count` (774) — a table header line missing the count integer, so `read_unsigned` finds no digits. `"expected unsigned integer"`.

Note: some target lines (e.g. 482, 376) are sub-expressions on otherwise-covered lines and may flip green incidentally. Don't write a dedicated test for a line that's already green after the above — re-check coverage in Task 4 and only backfill genuine gaps.

**Step 2: Run** — `cargo test -p flpdf --test xref_tests 2>&1 | tail -25`, all PASS.

**Step 3: Commit**
```bash
git add crates/flpdf/tests/xref_tests.rs
git commit -m "test(flpdf): cover strict xref-stream error arms (flpdf-tq35)"
```

---

### Task 4: Measure coverage delta, backfill, document unreachable arms

**Files:**
- Modify: `crates/flpdf/tests/xref_tests.rs` (only if backfill needed)

**Step 1: Re-measure**
```bash
cargo llvm-cov --workspace --summary-only 2>/dev/null | grep "flpdf/src/xref.rs"
cargo llvm-cov report --show-missing-lines --color never 2>/dev/null | grep -A1 "src/xref.rs"
```
Record before (74.60%) → after. Expect a substantial rise (target: meaningfully above 74.60%; the remaining gap should be only the documented-unreachable 64-bit arms plus any genuinely-unreachable defensive arms).

**Step 2: Triage remaining uncovered lines.** For each still-red line decide: (a) genuine gap I can reach → add a fixture and re-run; (b) unreachable through public API on 64-bit → leave it, ensure there is a one-line comment in `xref.rs` OR in the test module documenting why (e.g. `// Lines 88-92 / 164-165: usize::try_from(u64) overflow — only reachable on 32-bit targets.`). Do NOT add `#[cfg]` hacks or alter production logic to chase lines.

**Step 3: Full crate test + fmt**
```bash
cargo test -p flpdf 2>&1 | tail -15
cargo fmt --all
cargo fmt --all --check
```
(`cargo fmt --check` is a CI quality gate — must be clean before push.)

**Step 4: Commit**
```bash
git add -A
git commit -m "test(flpdf): backfill + document unreachable xref arms; record coverage delta (flpdf-tq35)"
```

---

## Done criteria
- `cargo test -p flpdf` green.
- `cargo fmt --all --check` clean.
- `cargo llvm-cov --workspace` shows `xref.rs` line% meaningfully above 74.60%, with the residual gap explained (documented-unreachable arms).
- No production code behavior changed (test-only diff, aside from at most a clarifying comment).

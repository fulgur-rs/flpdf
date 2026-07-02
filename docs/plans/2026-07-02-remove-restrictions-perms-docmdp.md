# --remove-restrictions を qpdf disableDigitalSignatures と byte-identical にする Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `flpdf rewrite --remove-restrictions` の署名処理を qpdf 11.9.0 の `QPDFAcroFormDocumentHelper::disableDigitalSignatures` と byte-identical に一致させ、(1) catalog `/Perms /DocMDP` 認証署名の検出漏れ (hn1g.15 本体) と (2) AcroForm Sig フィールドの `/FT` 保持・`/Fields` 残留の乖離を同時に解消する。

**Architecture:** ライブラリ `crates/flpdf/src/signatures.rs` に qpdf 2 関数を忠実移植する — `remove_security_restrictions`(catalog `/Perms` 除去 + `/AcroForm /SigFlags` を 0) と、それを呼ぶ `disable_digital_signatures`(Sig フィールドから `/FT /V /SV /Lock` 除去 + orphan Sig dict 削除 + top-level `/Fields` から erase)。CLI `main.rs` の `--remove-restrictions` 2経路を新関数 1 呼び出しに置換し、`pdf_has_signature_evidence` を catalog `/Perms` 検出込みに拡張する。検証は content-stream の無い 3 fixture の qpdf `--remove-restrictions --static-id` golden との byte 比較 (deflate 非依存なので `qpdf-zlib-compat` 非 gate)。

**Tech Stack:** Rust (workspace crates `flpdf` / `flpdf-cli`), qpdf 11.9.0 oracle, `tests/golden/regenerate.sh`, patch-coverage gate。

**qpdf oracle 参照 (検証根拠, 外部利用者が確認可能な事実):**
- `libqpdf/QPDFAcroFormDocumentHelper.cc:419` `disableDigitalSignatures`
- `libqpdf/QPDF.cc:2659` `removeSecurityRestrictions` (`/Perms` 除去 + SigFlags=0)
- `libqpdf/QPDFAcroFormDocumentHelper.cc:113` `removeFormFields` (top-level `/Fields` から erase)
- `libqpdf/QPDFJob.cc:2147` `--remove-restrictions` → `disableDigitalSignatures`

**方針の逸脱メモ (PR/コミットに残す):** issue fix direction の「`/DSS` も strip」は誤り。qpdf は `/DSS` を触らない (`removeSecurityRestrictions` は `/Perms` のみ)。よって `/DSS` は保持する。

---

### Task 1: 3 fixture + qpdf golden を追加 (検証インフラの前提)

**Files:**
- Modify: `tests/golden/regenerate.sh` (Phase 1 に fixture 生成, Phase 2 に golden 生成)
- Create (生成物・commit): `tests/fixtures/compat/perms-docmdp-one-page.pdf`, `acroform-sig-field-only.pdf`, `acroform-sig-widget.pdf`
- Create (生成物・commit): `tests/golden/references/{perms-docmdp-one-page,acroform-sig-field-only,acroform-sig-widget}/remove-restrictions.pdf`

**Step 1:** `regenerate.sh` Phase 1 (fixture 生成ブロック群の末尾, `objstm-lin-cap-boundary-199-bearing` の後) に 3 fixture 生成を追加。各 fixture は content stream を持たない (deflate 非依存)。python ヒアドキュメントで正しい xref offset を計算 (既存ブロックの作法に一致)。

```bash
# --- hn1g.15: --remove-restrictions == qpdf disableDigitalSignatures fixtures ---
# 全て content stream 無し(deflate非依存)。qpdf --remove-restrictions の3ケースを固定:
#  perms-docmdp : catalog /Perms /DocMDP のみ(AcroForm無し) -> /Perms除去, Sig GC
#  field-only   : AcroForm /Fields[sig], Sig は /Annots 未参照 -> /Fields[], field GC, SigFlags 0
#  widget       : Sig が page /Annots からも参照(merged) -> /Fields[], field は annot 生存, Sig GC
if [[ ! -f "$FIX/perms-docmdp-one-page.pdf" ]]; then
    echo "Generating perms-docmdp-one-page.pdf ..."
    python3 - "$FIX/perms-docmdp-one-page.pdf" <<'PY'
import sys
objs = {
  1: b"<< /Type /Catalog /Pages 2 0 R /Perms << /DocMDP 4 0 R >> >>",
  2: b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>",
  3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
  4: b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 10 20 30] /Contents <00> >>",
}
order=[1,2,3,4]
out=bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n"); offs={}
for n in order: offs[n]=len(out); out+=b"%d 0 obj\n"%n+objs[n]+b"\nendobj\n"
xo=len(out); size=len(order)+1
out+=b"xref\n0 %d\n0000000000 65535 f \n"%size
for n in range(1,size): out+=b"%010d 00000 n \n"%offs[n]
out+=b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n"%(size,xo)
open(sys.argv[1],"wb").write(out)
PY
else echo "Skipping perms-docmdp-one-page.pdf (already exists)"; fi

if [[ ! -f "$FIX/acroform-sig-field-only.pdf" ]]; then
    echo "Generating acroform-sig-field-only.pdf ..."
    python3 - "$FIX/acroform-sig-field-only.pdf" <<'PY'
import sys
objs = {
  1: b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>",
  2: b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>",
  3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
  4: b"<< /Fields [5 0 R] /SigFlags 3 >>",
  5: b"<< /FT /Sig /T (Approval) /V 6 0 R /Rect [0 0 0 0] >>",
  6: b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 10 20 30] /Contents <00> >>",
}
order=[1,2,3,4,5,6]
out=bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n"); offs={}
for n in order: offs[n]=len(out); out+=b"%d 0 obj\n"%n+objs[n]+b"\nendobj\n"
xo=len(out); size=len(order)+1
out+=b"xref\n0 %d\n0000000000 65535 f \n"%size
for n in range(1,size): out+=b"%010d 00000 n \n"%offs[n]
out+=b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n"%(size,xo)
open(sys.argv[1],"wb").write(out)
PY
else echo "Skipping acroform-sig-field-only.pdf (already exists)"; fi

if [[ ! -f "$FIX/acroform-sig-widget.pdf" ]]; then
    echo "Generating acroform-sig-widget.pdf ..."
    python3 - "$FIX/acroform-sig-widget.pdf" <<'PY'
import sys
objs = {
  1: b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>",
  2: b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>",
  3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
  4: b"<< /Fields [5 0 R] /SigFlags 3 >>",
  5: b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (Approval) /V 6 0 R /Rect [10 20 30 40] /P 3 0 R >>",
  6: b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 10 20 30] /Contents <00> >>",
}
order=[1,2,3,4,5,6]
out=bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n"); offs={}
for n in order: offs[n]=len(out); out+=b"%d 0 obj\n"%n+objs[n]+b"\nendobj\n"
xo=len(out); size=len(order)+1
out+=b"xref\n0 %d\n0000000000 65535 f \n"%size
for n in range(1,size): out+=b"%010d 00000 n \n"%offs[n]
out+=b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n"%(size,xo)
open(sys.argv[1],"wb").write(out)
PY
else echo "Skipping acroform-sig-widget.pdf (already exists)"; fi
```

**Step 2:** `regenerate.sh` Phase 2 (reference outputs) に golden 生成を追加:

```bash
# --- hn1g.15: qpdf --remove-restrictions == disableDigitalSignatures oracle ---
for stem in perms-docmdp-one-page acroform-sig-field-only acroform-sig-widget; do
    mkdir -p "$REF/$stem"
    qpdf --remove-restrictions --static-id --warning-exit-0 \
        "$FIX/$stem.pdf" "$REF/$stem/remove-restrictions.pdf"
    echo "$stem/remove-restrictions.pdf"
done
```

**Step 3:** 生成を実行:
Run: `bash tests/golden/regenerate.sh 2>&1 | grep -Ei "perms-docmdp|acroform-sig|remove-restrictions"`
Expected: 3 fixture "Generating ..." + 3 golden 行が出力。

**Step 4:** golden の中身を目視確認 (qpdf の期待挙動と一致するか):
Run: `for s in perms-docmdp-one-page acroform-sig-field-only acroform-sig-widget; do echo "== $s =="; qpdf --qdf tests/golden/references/$s/remove-restrictions.pdf - 2>/dev/null | grep -E "obj|/Perms|/Fields|/SigFlags|/FT|/ByteRange"; done`
Expected:
- perms-docmdp: `/Perms` 無し, `/ByteRange` 無し, catalog/pages/page の 3 obj。
- field-only: `/Fields [ ]` 空, `/SigFlags 0`, `/FT` 無し, obj5/6 無し。
- widget: `/Fields [ ]` 空, `/SigFlags 0`, obj5 は `/Subtype /Widget /T (Approval)` 生存で `/FT`/`/V` 無し, `/ByteRange` 無し。

**Step 5:** Commit:
```bash
git add tests/golden/regenerate.sh tests/fixtures/compat/perms-docmdp-one-page.pdf \
  tests/fixtures/compat/acroform-sig-field-only.pdf tests/fixtures/compat/acroform-sig-widget.pdf \
  tests/golden/references/perms-docmdp-one-page tests/golden/references/acroform-sig-field-only \
  tests/golden/references/acroform-sig-widget
git commit -m "test(hn1g.15): add /Perms-DocMDP + AcroForm-sig fixtures and qpdf --remove-restrictions goldens"
```

---

### Task 2: ライブラリ `remove_security_restrictions` (qpdf QPDF::removeSecurityRestrictions)

**Files:**
- Modify: `crates/flpdf/src/signatures.rs` (新 pub fn 追加; `resolve_catalog_acroform` / `AcroformHome` 再利用)
- Modify: `crates/flpdf/src/lib.rs` (re-export)
- Test: `crates/flpdf/tests/sig_flags_tests.rs`

**Step 1: Write failing tests**
`sig_flags_tests.rs` に追加。ヘルパ (in-memory PDF builder) は既存テストの流儀に合わせる。
```rust
#[test]
fn remove_security_restrictions_drops_perms_and_zeros_sigflags() {
    // catalog に /Perms、AcroForm に /SigFlags 3 を持つ最小 PDF
    let mut pdf = open_fixture(perms_and_acroform_pdf());
    assert!(remove_security_restrictions(&mut pdf).unwrap());
    let root = pdf.resolve(pdf.root_ref().unwrap()).unwrap();
    let Object::Dictionary(cat) = root else { panic!() };
    assert!(cat.get("Perms").is_none(), "/Perms must be removed");
    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), Some(0));
}

#[test]
fn remove_security_restrictions_is_noop_without_perms_or_sigflags() {
    let mut pdf = open_fixture(plain_one_page_pdf());
    assert!(!remove_security_restrictions(&mut pdf).unwrap());
}

#[test]
fn remove_security_restrictions_removes_perms_when_acroform_absent() {
    // DocMDP-only (AcroForm 無し) -> /Perms のみ除去、changed=true
    let mut pdf = open_fixture(perms_docmdp_only_pdf());
    assert!(remove_security_restrictions(&mut pdf).unwrap());
    let Object::Dictionary(cat) = pdf.resolve(pdf.root_ref().unwrap()).unwrap() else { panic!() };
    assert!(cat.get("Perms").is_none());
}
```

**Step 2:** Run: `cargo test -p flpdf --test sig_flags_tests remove_security_restrictions`
Expected: FAIL — `remove_security_restrictions` 未定義 (compile error)。

**Step 3: Implement** `signatures.rs`:
```rust
/// Remove qpdf-supported security restrictions, mirroring
/// `QPDF::removeSecurityRestrictions` (qpdf 11.9.0, QPDF.cc:2659).
///
/// Drops the catalog `/Perms` entry unconditionally and, when `/AcroForm` is a
/// dictionary that carries `/SigFlags`, replaces `/SigFlags` with `0`. Returns
/// `true` when either change was applied.
///
/// # Errors
///
/// Propagates any error from resolving the catalog and `/AcroForm` objects
/// (for example I/O or parse failures surfaced by [`Pdf::resolve`]).
pub fn remove_security_restrictions<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<bool> {
    let Some(root_ref) = pdf.root_ref() else { return Ok(false); };
    let Object::Dictionary(mut catalog) = pdf.resolve(root_ref)? else { return Ok(false); };
    let mut changed = catalog.remove("Perms").is_some();
    if changed {
        pdf.set_object(root_ref, Object::Dictionary(catalog));
    }
    // /AcroForm /SigFlags -> 0 (qpdf replaces unconditionally when the key exists).
    if let Some((home, mut acroform)) = resolve_catalog_acroform(pdf)? {
        if acroform.get("SigFlags").is_some()
            && sig_flags_from_acroform_dict(&acroform) != Some(0)
        {
            acroform.insert("SigFlags", Object::Integer(0));
            write_back_acroform(pdf, home, acroform);
            changed = true;
        }
    }
    Ok(changed)
}
```
`clear_sig_flags` の `AcroformHome` 書き戻し 6 行を `write_back_acroform(pdf, home, acroform)` ヘルパに抽出し、`clear_sig_flags` と `remove_security_restrictions` で共有 (DRY)。

注記: qpdf は `hasKey("/SigFlags")` なら値に関わらず 0 をセットするが、既に 0 の場合 flpdf 側で dirty マークを増やさないよう `!= Some(0)` を条件に加える (出力 byte は同一。dirty 最小化)。

**Step 4:** Run: `cargo test -p flpdf --test sig_flags_tests remove_security_restrictions`
Expected: PASS (3 tests)。

**Step 5: Commit**
```bash
git add crates/flpdf/src/signatures.rs crates/flpdf/src/lib.rs crates/flpdf/tests/sig_flags_tests.rs
git commit -m "feat(signatures): add remove_security_restrictions (qpdf QPDF::removeSecurityRestrictions parity)"
```

---

### Task 3: ライブラリ `disable_digital_signatures` (qpdf disableDigitalSignatures)

**Files:**
- Modify: `crates/flpdf/src/signatures.rs` (新 pub fn + 内部 walker `disable_sig_field` + `erase_fields_from_acroform`)
- Modify: `crates/flpdf/src/lib.rs` (re-export)
- Test: `crates/flpdf/tests/sig_flags_tests.rs`

**Step 1: Write failing tests**
```rust
#[test]
fn disable_digital_signatures_strips_sig_field_keys_and_erases_from_fields() {
    // AcroForm /Fields[5], field5 = /FT /Sig /T /V 6 /Rect, Sig dict 6
    let mut pdf = open_fixture(acroform_sig_field_only_pdf());
    assert!(disable_digital_signatures(&mut pdf).unwrap());
    // /Fields 空
    let (_, acro) = resolve_catalog_acroform_pub(&mut pdf); // via acroform_sig_flags + fields probe
    // field5 から /FT /V /SV /Lock 除去 (但し /T 保持), Sig dict 6 削除
    let Object::Dictionary(f5) = pdf.resolve(objref(5)).unwrap() else { panic!() };
    assert!(f5.get("FT").is_none() && f5.get("V").is_none());
    assert_eq!(f5.get("T").map(|_| true), Some(true), "/T must be preserved");
    assert!(pdf.signatures().unwrap().is_empty());
    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), Some(0));
    // top-level /Fields から 5 0 R が消えている
    assert!(fields_array_is_empty(&mut pdf));
}

#[test]
fn disable_digital_signatures_docmdp_only_removes_perms() {
    let mut pdf = open_fixture(perms_docmdp_only_pdf());
    assert!(disable_digital_signatures(&mut pdf).unwrap());
    let Object::Dictionary(cat) = pdf.resolve(pdf.root_ref().unwrap()).unwrap() else { panic!() };
    assert!(cat.get("Perms").is_none());
}

#[test]
fn disable_digital_signatures_is_noop_on_unsigned() {
    let mut pdf = open_fixture(plain_one_page_pdf());
    assert!(!disable_digital_signatures(&mut pdf).unwrap());
}

#[test]
fn disable_digital_signatures_respects_field_depth_limit() {
    // /Kids ネストを DEFAULT_MAX_SIGNATURE_FIELD_DEPTH+1 段 -> Err(Unsupported) にはせず
    // 既存 strip 系と同様に安全に打ち切り(過走査しない)。visited で循環も停止。
    // (具体的アサートは既存 strip_signature_values の深さテストに合わせる)
}
```
テストヘルパ (`open_fixture`, `objref`, fields 探査) は既存 `sig_flags_tests.rs` のヘルパを流用/追加。`resolve_catalog_acroform` は非 pub なので、`/Fields` 空確認は `acroform_sig_flags` 経由や公開 API で行う (テスト内 helper で catalog→AcroForm→Fields を resolve)。

**Step 2:** Run: `cargo test -p flpdf --test sig_flags_tests disable_digital_signatures`
Expected: FAIL — 未定義。

**Step 3: Implement** `signatures.rs`:
```rust
/// Disable digital signatures for `--remove-restrictions`, mirroring
/// `QPDFAcroFormDocumentHelper::disableDigitalSignatures` (qpdf 11.9.0,
/// QPDFAcroFormDocumentHelper.cc:419).
///
/// 1. [`remove_security_restrictions`]: drop catalog `/Perms`, zero
///    `/AcroForm /SigFlags`.
/// 2. For every terminal AcroForm field whose (inherited) `/FT` is `/Sig`,
///    remove `/FT`, `/V`, `/SV`, `/Lock` (the field name `/T` is preserved) and
///    delete the now-orphaned signature dictionary that `/V` referenced.
/// 3. Erase those fields' references from the top-level `/AcroForm /Fields`
///    array. On a full rewrite a field still reachable from a page `/Annots`
///    survives as a plain annotation; a field-only entry becomes unreferenced
///    and is dropped by the writer's garbage collection.
///
/// Returns `true` when anything changed. `/DSS` is intentionally left untouched,
/// matching qpdf (`removeSecurityRestrictions` only removes `/Perms`).
///
/// # Errors
///
/// Propagates errors from resolving the catalog, `/AcroForm`, `/Fields`, and
/// field-tree objects (surfaced by [`Pdf::resolve`]).
pub fn disable_digital_signatures<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<bool> {
    let mut changed = remove_security_restrictions(pdf)?;

    let Some((_, mut acroform)) = resolve_catalog_acroform(pdf)? else { return Ok(changed); };
    let Some(fields_obj) = acroform.remove("Fields") else { return Ok(changed); };

    let mut to_remove: Vec<ObjectRef> = Vec::new();
    let mut seen = BTreeSet::new();
    for field in resolve_array(pdf, fields_obj)? {
        if let Object::Reference(field_ref) = field {
            disable_sig_field(pdf, field_ref, None, 0, &mut seen, &mut to_remove, &mut changed)?;
        }
    }
    if !to_remove.is_empty() {
        changed |= erase_fields_from_acroform(pdf, &to_remove)?;
    }
    Ok(changed)
}
```
内部 walker `disable_sig_field` は `strip_signature_values_from_field` を土台に、Sig field で
`/FT`/`/V`/`/SV`/`/Lock` を remove + `/V` 宛先 Sig dict を `delete_object` + `to_remove.push(field_ref)`。
深さ上限・visited・`is_pure_widget` skip・`/Kids` 降下は既存 walker と同一の作法 (review-pattern #4)。
`erase_fields_from_acroform` は catalog→AcroForm を resolve し、top-level `/Fields` 配列から
`to_remove` に含まれる `Object::Reference` 要素を除去して書き戻す (`AcroformHome` 経由)。

**Step 4:** Run: `cargo test -p flpdf --test sig_flags_tests disable_digital_signatures`
Expected: PASS。

**Step 5: Commit**
```bash
git add crates/flpdf/src/signatures.rs crates/flpdf/src/lib.rs crates/flpdf/tests/sig_flags_tests.rs
git commit -m "feat(signatures): add disable_digital_signatures (qpdf disableDigitalSignatures parity)"
```

---

### Task 4: CLI 配線 — 検出拡張 + 新関数への置換

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs` (`pdf_has_signature_evidence` ~3177, 2 経路 ~2925-2935 / ~2954-2970)

**Step 1: Write failing test** — Task 6 の CLI behavioral テストで表出させる方が自然。ここでは既存
`remove_restrictions` CLI テストが引き続き緑であること + Task 6 の新 DocMDP テストで検証する。
まず `pdf_has_signature_evidence` の拡張から:

**Step 2:** `pdf_has_signature_evidence` を拡張 — catalog に `/Perms` があれば true:
```rust
fn pdf_has_signature_evidence<R: Read + Seek>(pdf: &mut Pdf<R>) -> CliResult<bool> {
    let has_sig_flags = acroform_sig_flags(pdf)?
        .is_some_and(|flags| flags & (SIG_FLAGS_SIGNATURES_EXIST | SIG_FLAGS_APPEND_ONLY) != 0);
    if has_sig_flags || !pdf.signatures()?.is_empty() {
        return Ok(true);
    }
    // catalog /Perms (DocMDP/UR3 certification) only — qpdf --remove-restrictions
    // removes /Perms even without /AcroForm evidence (QPDF::removeSecurityRestrictions).
    // /DSS is intentionally NOT considered: qpdf does not strip it, so detecting a
    // /DSS-only doc would emit a spurious "signatures invalidated" warning.
    if let Some(root_ref) = pdf.root_ref() {
        if let Object::Dictionary(catalog) = pdf.resolve(root_ref)? {
            return Ok(catalog.get("Perms").is_some());
        }
    }
    Ok(false)
}
```

**Step 3:** 2 経路の `clear_sig_flags(&mut pdf)?; strip_signature_values(&mut pdf)?;` を
`disable_digital_signatures(&mut pdf)?;` 1 呼び出しに置換 (linearize path の `pdf2`、通常 path の `pdf`)。
`use flpdf::{... disable_digital_signatures ...}` を import に追加、不要になった
`clear_sig_flags`/`strip_signature_values` import はこの経路では外す (他所で使っていなければ)。

**Step 4:** Run: `cargo test -p flpdf-cli --test cli_tests remove_restrictions`
Expected: PASS — 既存 2 テストは assertion (signatures empty, no `/V `, no `/ByteRange`, SigFlags 0) が
新挙動でも成立するため緑のまま。

**Step 5: Commit**
```bash
git add crates/flpdf-cli/src/main.rs
git commit -m "fix(cli): --remove-restrictions detects catalog /Perms + uses disable_digital_signatures (flpdf-hn1g.15)"
```

---

### Task 5: byte-identity oracle テスト (capstone, 3 ケース)

**Files:**
- Create: `crates/flpdf/tests/remove_restrictions_qpdf_parity.rs`

**Step 1: Write failing test** — content stream 無し = deflate 非依存なので **feature-gate 無し**。
`cmp_diff_zero_tests.rs` の `first_diff` パターンを流用。
```rust
use flpdf::{disable_digital_signatures, write_pdf_with_options, NewlineBeforeEndstream, Pdf, WriteOptions};
use std::path::Path;

fn remove_restrictions_qpdf_equivalent(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat").join(fixture);
    let mut pdf = Pdf::open(std::io::BufReader::new(std::fs::File::open(&path).unwrap())).unwrap();
    disable_digital_signatures(&mut pdf).unwrap();
    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.static_id = true;
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &opts).unwrap();
    out
}
fn golden(stem: &str) -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/references").join(stem).join("remove-restrictions.pdf");
    std::fs::read(&p).unwrap_or_else(|e| panic!("read golden {p:?}: {e}"))
}
fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> { /* cmp_diff_zero_tests.rs と同一 */ }
fn assert_parity(fixture: &str, stem: &str) {
    let actual = remove_restrictions_qpdf_equivalent(fixture);
    let expected = golden(stem);
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!("{fixture}: not byte-identical to qpdf --remove-restrictions golden (flpdf={} golden={} first diff {off})\nflpdf : {:?}\ngolden: {:?}",
            actual.len(), expected.len(), &actual[lo..(off+16).min(actual.len())], &expected[lo..(off+16).min(expected.len())]);
    }
}

#[test] fn perms_docmdp_only_is_byte_identical_to_qpdf() { assert_parity("perms-docmdp-one-page.pdf", "perms-docmdp-one-page"); }
#[test] fn acroform_sig_field_only_is_byte_identical_to_qpdf() { assert_parity("acroform-sig-field-only.pdf", "acroform-sig-field-only"); }
#[test] fn acroform_sig_widget_survives_as_annotation_byte_identical_to_qpdf() { assert_parity("acroform-sig-widget.pdf", "acroform-sig-widget"); }
```

**Step 2:** Run: `cargo test -p flpdf --test remove_restrictions_qpdf_parity`
Expected: 実装が正しければ 3 PASS。差分が出たら first_diff の offset/バイト列で原因特定
(key 順序・/ID・GC 漏れ等)。**差分が出たら Task 2/3 の実装を修正** — golden は正 (oracle)。

**Step 3:** (差分修正があれば) 修正 → 再実行して 3 PASS。

**Step 4:** patch-coverage の前段として fmt/clippy:
Run: `cargo fmt && cargo clippy -p flpdf -p flpdf-cli --all-targets -- -D warnings 2>&1 | tail`
Expected: 変更なし / warning ゼロ。

**Step 5: Commit**
```bash
git add crates/flpdf/tests/remove_restrictions_qpdf_parity.rs
git commit -m "test(hn1g.15): byte-identity vs qpdf --remove-restrictions for /Perms + AcroForm sig cases"
```

---

### Task 6: CLI behavioral テスト (DocMDP + merged widget) + 既存テスト補強

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_tests.rs` (fixture builder + 新テスト)

**Step 1: Write tests**
- `signed_perms_docmdp_pdf()` builder (catalog /Perms /DocMDP + Sig, AcroForm 無し)。
- `rewrite_remove_restrictions_strips_docmdp_perms_and_warns`: `--remove-restrictions` で
  exit 0 + warning、出力に `/Perms`/`/ByteRange`/`/DocMDP` が無い、`pdf.signatures()` 空。
- 既存 `rewrite_remove_restrictions_strips_signatures_and_warns` に「/Fields が空」「field の
  `/FT` が無い」アサートを追加 (新挙動の behavioral 固定)。merged widget builder で
  「annotation が生存 (/Subtype /Widget 残る) が /FT/V 無し」も検証。

**Step 2:** Run: `cargo test -p flpdf-cli --test cli_tests remove_restrictions`
Expected: 全 PASS (既存 2 + 新規)。

**Step 3: Commit**
```bash
git add crates/flpdf-cli/tests/cli_tests.rs
git commit -m "test(cli): --remove-restrictions DocMDP /Perms + merged-widget survival (flpdf-hn1g.15)"
```

---

### Task 7: 品質ゲート (PR 作成前)

**Step 1:** worktree を commit 済みにする (patch-coverage は HEAD を diff)。
**Step 2:** patch-coverage:
Run: `scripts/patch-coverage.sh --base main`
Expected: `flpdf` 変更行 100%。未カバー行があればテスト追加 or `// cov:ignore: <理由>`。
Run 前に llvm-cov を回すなら `qpdf-zlib-compat` **無し** (メモリ: compat baseline は miniz 固定)。
**Step 3:** 全体テスト (関連クレート):
Run: `cargo test -p flpdf -p flpdf-cli 2>&1 | tail -20`
Expected: 全 PASS。
**Step 4:** fmt --check:
Run: `cargo fmt --check` (メモリ: CI Quality = fmt --check)。
**Step 5:** 質的チェック: 新公開挙動のエラーアーム/境界 (深さ上限・空 /Fields・/Perms inline vs indirect・
merged widget 生存) のテストが実在するか確認。
**Step 6:** doc レビュー: 新 pub fn の `///` は英語・`# Errors` あり・intra-doc リンク・issue ID 混入無し
(`.claude/rules/pdf-rust-doc-review-patterns.md`)。

---

## リスク / 注意
- **catalog inline vs indirect**: `/Perms` 除去と SigFlags 書き戻しは `AcroformHome` 相当で正しい
  object に書き戻す。inline AcroForm (catalog 内直書き) の分岐をテストで踏む。
- **byte-identity の /ID**: `static_id=true` と `qpdf --static-id` の /ID 一致は既存 gated テストで
  実証済み。stream 無し fixture でも同様のはず。差が出たら first_diff で /ID 位置を確認。
- **既存 public API**: `clear_sig_flags`/`strip_signature_values` は残す (public + テスト有り)。
  disable パスから呼ばなくなるだけ。pre-1.0 で互換考慮不要だが削除は別 issue 扱い。
- **nested Sig field**: top-level `/Fields` のみ erase (qpdf removeFormFields と一致)。専用 fixture は任意。

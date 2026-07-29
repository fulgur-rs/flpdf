# qpdf `test_driver` 相当バイナリの設計

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:writing-plans to turn this design into a task-by-task plan before implementing.

**Goal:** qtest `basic-parsing.test` の 21 件の `test_driver N goodM.pdf` subtest を救済する。
現状はいずれも `TestDriver->runtest: unable to run command` / exit 2 で FAIL している。

**Beads:** flpdf-n9t0.2（実装、`test_0_1` に限定）/ flpdf-n9t0.3（shim）/
flpdf-n9t0.4（前提バグ、PR #584 で完了）/ flpdf-n9t0.5（前提 chore、PR #22 + #589）/
flpdf-n9t0.6（`test_3` 実装、n9t0.2 から分離）/ flpdf-n9t0.7（follow-up バグ、§4 参照）。
親 epic は flpdf-n9t0。

**Oracle:** qpdf v11.9.0 `qpdf/test_driver.cc`（`test_0_1` = 20 件、`test_3` = good14 の 1 件）。
本物の `test_driver` バイナリで `good1.out` / `good21.out` の再現を実測確認済み。

---

## 0. 責務の所在

`test_driver` は flpdf-cli（qpdf CLI 互換）の話ではない。qpdf の **ライブラリ API**
（`QPDF` / `QPDFObjectHandle`）を直接叩く C++ テストツールであり、必要なのは

- (a) flpdf-qtest 側の PATH shim（flpdf-n9t0.3）
- (b) qpdf のオブジェクトハンドル意味論を露出するテスト用バイナリ（flpdf-n9t0.2）

の 2 つ。`flpdf-qtest/shim/` には現在 `qpdf` / `qpdf-test-compare` / `fix-qdf` /
`zlib-flate` の 4 つしか無い。

## 1. 依存順序

```
flpdf-n9t0.4 (P1 bug)          flpdf-n9t0.5 (chore)
  文字列 unparse の qpdf 一致      crate リネーム・ヘルパー集約
        └──────────┬──────────────────┘
                   ▼
            flpdf-n9t0.2  test_driver 実装（test_0_1）
                   ▼
            flpdf-n9t0.3  flpdf-qtest に shim 設置
```

n9t0.4 と n9t0.5 は互いに独立で並行可。bd 側に `blocked-by` を張ってあるので
`bd ready` には 4 と 5 だけが出る。**n9t0.4 は PR #584（`b7bfbad9`）でマージ済み**
なので、残る前提は n9t0.5 だけ。

### 1.1 なぜ n9t0.4 が前提だったのか

`Object::write_pdf` は qpdf の `QPDFObjectHandle::unparse` と一致していなかった。
`crates/flpdf/src/object.rs` の `is_printable_string` は「全バイトが `0x20..=0x7e`」で
hex/literal を選んでいたが、qpdf の `QPDF_String::useHexString`（`libqpdf/QPDF_String.cc:72-90`）は

- `\b \t \n \f \r` を hex 強制の対象にしない（リテラル内でエスケープする）
- 非 ASCII は `5 * non_ascii > len` の閾値を超えたときだけ hex にする
- 閾値を生き延びたが ISO-Latin-1 印字不可なバイトは 3 桁 8 進エスケープにする

実測（qpdf 11.9.0 vs flpdf @ eef2bbc4、`flpdf rewrite --static-id` と `qpdf --static-id`）:

| 入力の文字列 | qpdf | flpdf（修正前） |
|---|---|---|
| `(a\nb)` | `(a\nb)` | `<610a62>` |
| 24 バイト中 2 バイトが非 ASCII | `(caf<C3><A9> latte and teas xyz)` | `<636166c3a9…>` |

`good13` の QDF 差分はこの 2 箇所と、その帰結である xref オフセットの一律 +4 ずれだけだった。

**修正後の実測**（`b7bfbad9`）: `good1..good21` を `qpdf --static-id -qdf` と
`flpdf rewrite --qdf --static-id` で比較したとき byte-identical な数が
good{1,5,6,8,12,20,21} の **7 件**から good{1,5,6,8,**9**,12,**13**,20,21} の
**9 件**になった。good9（string）と good13（nesting, strings, names）が反転し、
既に一致していたものの回帰はゼロ。実装は `use_hex_string` として
`QPDF_String::useHexString` を移植し、リテラル側に 5 文字のエスケープと 8 進
フォールバックを追加したもの。

## 2. crate レイアウト

n9t0.5 で `crates/flpdf-test-compare` を `crates/flpdf-qtest-tools` にリネームし、
qtest ハーネス用のヘルパーバイナリを 1 crate に集約する。**binary 名は据え置く**
（flpdf-qtest の shim は `FLPDF_TEST_COMPARE_BIN` 経由で `target/release/flpdf-test-compare`
という binary 名しか見ていない）。

```
crates/flpdf-qtest-tools/
  Cargo.toml
    [[bin]] name = "flpdf-test-compare"   path = "src/main.rs"       # 既存
    [[bin]] name = "flpdf-test-driver"    path = "src/bin/driver.rs" # 新規
  src/
    lib.rs           既存 + driver モジュールの re-export
    common.rs        新規 — program_name(argv0) を main.rs から移動
    clean.rs         既存（compare 専用）
    compare.rs       既存
    orchestrator.rs  既存
    output.rs        既存 + バイナリ書き出しヘルパー
    main.rs          既存 compare — program_name を common に委譲
    driver/
      mod.rs         runtest ディスパッチ / "test N done" / invalid test
      handle.rs      Handle 型と QPDFObjectHandle 意味論
      test_0_1.rs
```

既存ファイルは移動しない。n9t0.2 が `main.rs` に触るのは `program_name` の移設 1 点。

`program_name` を共有するのは整形上の都合ではなく仕様。qpdf の `main` は compare 側も
test_driver 側も `strrchr(argv[0], '/') + 1` で `whoami` を作り、それが usage と
エラー行の接頭辞になる。複製すると片方だけ直る事故が起きる。

qpdf 自身は `compare-for-test/qpdf-test-compare.cc` と `qpdf/test_driver.cc` を
別ディレクトリ・別ターゲットに置いている。1 crate への集約は出力バイトに影響しない
ビルド構成の差であり、CLAUDE.md の逸脱分類 (B) に該当する。

**`docs/qpdf-correspondence.md` への追記は行わない**（PR #589 で判断）。同表は
`libqpdf/*.cc` を flpdf モジュールへ対応づけるもので、列は `qpdf` / `行` / `flpdf` /
`状態`（逸脱候補表は `逸脱候補` / `qpdf 行数` / `byte 影響`）。`compare-for-test/`
配下のテストヘルパーはそもそも 1 行も載っていない。加えて qpdf 自身がこの 2 つを
1 つの CMake プロジェクトから作っているため、1 cargo package への集約は「qpdf の
ソースを Rust の機構で置き換えた逸脱」ではなく、むしろ qpdf の構造をなぞったもの。
CLAUDE.md (B) 条件 3 の「モジュール doc に 1 行」は `crates/flpdf-qtest-tools/src/lib.rs`
の crate doc で満たしている。

## 3. CLI 契約とディスパッチ順序

```
test_driver <n> <filename1> [arg2]
```

`test_driver.cc:3571-3593` より:

- `argc < 3 || argc > 4` → stderr に `Usage: <whoami> n filename1 [arg2]`、exit 2
- 例外は `e.what()` を stderr に 1 行、exit 2
- 正常終了は exit 0

`runtest`（`test_driver.cc:3457-3569`）の順序:

```
1. QPDF pdf;
2. n による読込分岐            ← 先
3. test_functions.find(n)      ← 後
     見つからなければ throw std::runtime_error("invalid test " + n)
4. 関数呼び出し
5. stdout に "test <n> done"
```

**読込がルックアップより先**である点が重要。壊れた PDF を未実装番号に食わせると
`invalid test 50` ではなく parse エラーが出る。「番号を先に検証して早期リターン」は
qpdf と挙動が変わるので避けること。

読込分岐は `n % 2 == 1` → メモリ、`n % 4 == 0` → パス、それ以外の偶数 → `FILE*`。
実装対象の 1 と 3 はどちらも奇数なので実際に通るのはメモリ経路（`Pdf::open_mem`）だけだが、
分岐そのものは qpdf の形で書く。

### 3.1 fail-loud

実装していない番号は `invalid test <n>` を stderr に出して exit 2。
n9t0.3 で shim を PATH に置いた瞬間、basic-parsing 以外の `.test` が呼ぶ ~97 個の
test 番号すべてにこれが効く。黙って成功させると記録済みベースライン
（Passes 140 / allowlist 39/39 PASS / 回帰 0）が静かに動く。

### 3.2 バッファリング

qpdf は `QUtil::setLineBuf(stdout)` = `setvbuf(_IOLBF)`。Rust の `io::Stdout` は
`LineWriter` なので既定で一致する。ただし **stderr に書く前に必ず stdout を flush** する。
`good14.out` が `<610062> (MOO)WARNING: good14.pdf (offset 628): …` と改行なしで繋がっており、
qtest は両ストリームを 1 本に束ねて捕捉するため、flush 規律を外すと順序が入れ替わる。

## 4. `Handle` — QPDFObjectHandle 意味論

```rust
struct Handle {
    resolved: Object,            // 解決済みの値
    indirect: Option<ObjectRef>, // 間接参照だったならその参照
}
```

qpdf の `QPDFObjectHandle` は未解決の参照を保持したまま型を聞かれると解決するハンドル。
`get_key` の時点で解決し参照を `indirect` に残せば、`isIndirect()` / `unparse()` /
`unparseResolved()` の 3 つを賄える。

意味論は flpdf 本体ではなくこの crate 側に置く。flpdf の `Option` ベース API と
二重化させないため。

**`get_key`** — 欠落キーは `Handle { resolved: Object::Null, indirect: None }`。
根拠は `QPDF_Dictionary::getKey` のコメント（"PDF spec says fetching a non-existent key
from a dictionary returns the null object"）。これが subtest 1 "implicit null" の中身。

**`has_key`** — `QPDF_Dictionary::hasKey`（`QPDF_Dictionary.cc:98-101`）は
`items.count(key) > 0 && !items[key].isNull()`。`isNull()` が間接参照を解決するため、
`good2`（直接 null）・`good3`（dangling ref）・`good4`（実在する null）が揃って `false` になり、
3 つとも `/QTest is implicit` を出す。flpdf 側は `Pdf::resolve_borrowed` の既存挙動
（「unknown / freed / broken な参照は Null に解決」、`reader.rs:1131`）がそのまま `good3` に対応する。

**`type_code` / `type_name`** — `Constants.h:108-128` の `qpdf_object_type_e` の並びが
そのまま数値。名前は各 `QPDF_*.cc` の `QPDFValue(::ot_X, "…")` が真。

| code | name | 実測元 |
|---|---|---|
| 0 / 1 | `uninitialized` / `reserved` | flpdf に対応物なし |
| 2 | `null` | good1/2/3/4 |
| 3 | `boolean` | good5 |
| 4 / 5 | `integer` / `real` | good7 / good8 |
| 6 / 7 | `string` / `name` | good9 / good15 |
| 8 / 9 / 10 | `array` / `dictionary` / `stream` | good21 / good11 / good12 |
| 11 / 12 | `operator` / `inline-image` | — |
| 13 / 14 | `unresolved` / `destroyed` | flpdf に対応物なし |

`ot_inlineimage` の名前は `"inlineimage"` ではなく **`"inline-image"`**（ハイフン入り）。
flpdf の `Object` には 0/1/13/14 に相当する variant が無いので、enum の判別子ではなく
明示テーブルで書く。

**`unparse` / `unparse_resolved`** — `Object::write_pdf` を再利用し（n9t0.4 で
qpdf の `unparse` と一致済み）、**stream だけ特例**（`QPDF_Stream::unparse` は
間接参照を返す。`good12` の
`unparseResolved: 7 0 R` が根拠）。入れ子は解決しない — `good21` の
`[ /literal null /indirect 8 0 R /undefined 10 0 R ]` の通り配列要素の参照は `N G R` のまま残り、
これは `write_pdf` の `Object::Reference` 分岐と一致する。

**既知の欠落（flpdf-n9t0.7、follow-up）**: `Dictionary::write_pdf` は辞書キーを
一切エスケープしない（`object.rs:834-843`。QDF 経路の `write_pdf_qdf` は
`write_name_escaped` を正しく呼ぶが、コンパクト経路には無い）。実測
（`qpdf --static-id` vs `flpdf rewrite --static-id`、`/a#20b (x)` を持つ辞書）:

```
qpdf:  << /a#20b (x) >>
flpdf: << /a b (x) >>   ← トークン境界が壊れる
```

`test_0_1` の dictionary 分岐（`good11`）は幸い `/a` のようなエスケープ不要な
キーのみなので n9t0.2 の実装自体はブロックされないが、§7 の `dict_keys` fixture に
エスケープが要るキーを含めることはできない（n9t0.7 が着地するまで）。

## 5. `test_0_1`（20 subtest）

出力は 5 パート固定:

```
[hasKey が false なら] /QTest is implicit
/QTest is {in,}direct and has type <name> (<code>)
<型別の 1〜N 行>
unparse: <…>
unparseResolved: <…>
```

型別行で注意する点:

- `real` はリテラルをそのまま保持する（`good10`: `unparse: [ 1 (2) 8 0 R 0.0 -0.0 0. -0. ]`）。
  flpdf の `Object::RealLiteral { value, literal }` が既に対応済み
- `stream` は `"/QTest is a stream.  Dictionary: "` — **ピリオドの後がスペース 2 つ**
- stream は raw（`Stream.data` をそのまま）と decoded（`decode_stream_data`）を両方 stdout へ。
  decode 失敗時は `Stream data is not filterable.`
- `array` / `dictionary` は要素ごとに `  item N is {in,}direct` / `  /key is {in,}direct`
- **decode の前に `/Filter` と `/DecodeParms` を解決する。** `decode_stream_data` は
  `dict.get("Filter")` / `dict.get("DecodeParms")` を直接 `decode_stream_data_with_filters`
  へ渡すだけで間接参照を解決しない（`filters.rs`）。`Object::Reference` が来ると
  `decode_filter_specs`（`stream_filter.rs:55-59`）が `Unsupported` を返し、qpdf が
  正しくフィルタする場面でも `Stream data is not filterable.` になってしまう。既存の
  `flpdf-qtest-tools`（旧 `flpdf-test-compare`）の `compare.rs` が全く同じ理由で
  `resolve_stream_keys` を持っているので、同じ解決ステップをここでも呼ぶ

この 20 subtest は既存の flpdf 公開 API だけで完結する — `Pdf::open_mem` / `trailer()` /
`resolve_borrowed()` / `Stream { pub dict, pub data }` / `decode_stream_data` /
`write_pdf`。

## 6. `test_3` は n9t0.2 から切り出す

`good14` だけが使う経路（subtest 40）。`/QStreams` 配列を回して各 stream を正規化しながら
stdout に流し、期待出力には qpdf の警告が本文中に割り込む:

```
WARNING: good14.pdf (offset 628): content normalization encountered bad tokens
```

出所は `QPDF_Stream.cc:624-634` の 3 連 `warn(...)`。`QPDF_Stream::warn` は
`qpdf->warn(qpdf_e_damaged_pdf, "", this->parsed_offset, message)` で、書式は
`QPDFExc::createWhat`（`QPDFExc.cc:18-41`）の `<filename> (offset <N>): <message>`。

必要になるのは 3 つで、flpdf に無いのは 1 番目:

1. **stream オブジェクトの `parsed_offset`** — 警告に載るファイル内オフセット
2. 警告文の逐語再現 — qpdf 側のタイポ `"may be corrupted but is may still useful"` も含めて
3. stdout flush 規律（3.2）

判定フラグ自体は flpdf の `ContentNormalization::any_bad_tokens()` /
`last_token_was_bad()` が既に持っている。

**21 件中 1 件のために別種の作業（オフセット追跡 + 警告文言の移植）を要求するため、
n9t0.2 は `test_0_1` に限定し `test_3` は flpdf-n9t0.6 に切り出した。** `invalid test 3`
で fail-loud するのでベースラインは静かに動かない。

## 7. fixture とテスト

```
tests/fixtures/test_driver/
  README.md        flpdf-authored の license 注記（compare_for_test/README.md に倣う）
  generate.sh      PDF は python3 で生成、期待出力は本物の test_driver で生成
  implicit_null.{pdf,out}        欠落キー
  direct_null.{pdf,out}          /QTest null
  dangling_ref.{pdf,out}         存在しないオブジェクトへの参照
  indirect_null.{pdf,out}        実在する null オブジェクト
  indirect_bool.{pdf,out}        hasKey が true になる対照
  integer.{pdf,out}              top-level integer（good7 相当）
  real.{pdf,out}                 top-level indirect real（good8 相当）
  string_hex_literal.{pdf,out}   n9t0.4 の境界（\n 入り・閾値ちょうど・8 進フォールバック）
  name_escape.{pdf,out}          /hex#20strings 相当
  array_indirect.{pdf,out}       要素ごとの direct/indirect（real literal の
                                  verbatim 保持 0.0 -0.0 0. -0. も good10 に倣いここで確認）
  dict_keys.{pdf,out}            キーの lexicographic 順（ASCII-safe キーのみ。
                                  エスケープが要るキーは flpdf-n9t0.7 が着地するまで
                                  追加できない — §4 参照）
  stream_flate.{pdf,out}         raw / uncompressed / dict unparse
  stream_indirect_filter.{pdf,out}  /Filter が間接参照のストリーム（§5 参照）
  stream_unfilterable.{pdf,out}  Stream data is not filterable.
```

`test_0_1` は `/QTest` 1 個の型でしか分岐できないため、`integer.pdf` と `real.pdf`
は分けた別ファイルにする（`good7`/`good8` も qpdf 側で別ファイル）。1 ファイルに
両方を詰めると array 分岐だけを通り、integer/real 分岐は未検証のまま残る。

`flpdf-qtest/vendor/qpdf-qtest/` からのコピーは一切しない
（`tests/fixtures/compare_for_test/README.md` の方針）。

**通常の `cargo test` が実際にこの比較を行う経路**: `crates/flpdf-qtest-tools/tests/driver_goldens.rs`
がコミット済みの `tests/fixtures/test_driver/*.{pdf,out}` を読み、`flpdf-test-driver` を
`assert_cmd` 経由で起動して stdout を `.out` と突き合わせる（qpdf ビルド不要）。
`scripts/qpdf-test-driver-diff.sh`（既存の `qpdf-{tokenizer,rc4,lzw-png,stream-codecs}-diff.sh`
と同じ形）は別役割で、`scripts/fetch-qpdf-source.sh` で pinned source を取り本物の
`test_driver` をビルドして fixture を再生成・オラクル照合するための開発者ツール。
CI/`cargo test` はコミット済み `.out` に対する `driver_goldens.rs` だけを回す。

**flpdf を出力生成に使う場面では必ず `flpdf rewrite` を使う。** トップレベルの
`flpdf in out` は完全な書き直しをせず入力にバイトを追記する別経路で、qpdf の
`qpdf in out` とは挙動が違う（epic の good17 非 QDF 失敗 67/68/69 と符合する）。

### 7.1 カバレッジ

`n9t0.5` の一部として `scripts/patch-coverage.sh` の `REPORT_PREFIXES` を
`crates/flpdf-test-compare/src/` から `crates/flpdf-qtest-tools/src/` へ更新する
（PR #589 で実施済み、§8.1 参照）。一方 **n9t0.4 は `crates/flpdf/src/` を触ったので
変更行 100% ゲートの対象**だった（変更 82 行 / 未カバー 0 で PASS）。

## 8. 残るリスク

1. **`write_pdf` と `QPDFObjectHandle::unparse` の一致は全網羅では未検証。**
   real / name / dict / array は good7/8/10/11/13/15/21 で実測一致を確認したが、
   本当の担保は probe で全 fixture を突き合わせること。
2. **辞書キーのエスケープ欠落（flpdf-n9t0.7、follow-up）。** §4 参照。
   n9t0.2 本体はブロックしないが、`dict_keys` fixture の対象範囲を制限する。

### 8.1 解消済み

- ~~**n9t0.4 に既存のセーフティネットが無い。**~~ 着手前の実測では
  `cargo test --workspace --features qpdf-zlib-compat --no-fail-fast` が 132
  テストバイナリすべて緑で、既存 golden はどれも当該経路を通っていなかった。
  n9t0.4 で qpdf オラクル由来の 9 テスト（境界値・hex 強制・8 進フォールバックを含む）
  を新設して解消。
- ~~**good13 の QDF が緑になるのは見込み。**~~ `b7bfbad9` で byte-identical を確認済み。
- ~~**basic-parsing の subtest 38 / 39 が緑になるかは未確認。**~~ flpdf-qtest#22 の
  検証実走（`FLPDF_DIR` 経由ビルド）で確認済み: `basic-parsing 38 (create qdf) ... PASSED`
  `basic-parsing 39 (check output) ... PASSED`。
- ~~**n9t0.5 のクロスリポジトリ順序。**~~ flpdf-qtest の `scripts/run.sh` と
  `.github/workflows/ci.yml` が package 名（`-p flpdf-test-compare`）を直に
  書いていた点は事実だが、実際に採った対処は「新旧 package 名の入れ替え」ではなく
  **package 名依存そのものを外す**こと（`-p` → `--bin`。flpdf-qtest#22、マージ済み）。
  binary 名は不変なので dual-name 互換は不要。この後 flpdf 側で実際にリネームし
  （PR #589）、バイナリを削除した状態から `FLPDF_DIR` ビルド分岐を実走させて
  `Passes: 34/69`（リネーム前と完全に同一）を確認済み。
- ~~**カバレッジ prefix の更新漏れ。**~~ PR #589 で `patch-coverage.sh` の
  `REPORT_PREFIXES` を更新済み、`--allow-dirty` 付きで PASS を確認済み。

## 9. スコープ外

- `test_driver` の残り ~97 個の test 関数（fail-loud で `invalid test N`）
- flpdf 本体への `ObjectHandle` 公開 API の追加
- binary 名の変更、compare アルゴリズムの変更

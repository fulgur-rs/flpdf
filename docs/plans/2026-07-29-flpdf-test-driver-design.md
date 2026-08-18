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

**n9t0.3 は shim 設置だけでなく、flpdf-qtest 側のビルドコマンドの更新も含む。**
flpdf-qtest#22（マージ済み）で `scripts/run.sh` と `.github/workflows/ci.yml` は
`cargo build --release --bin flpdf --bin flpdf-test-compare` に固定した
（`--bin` は「指定した 1 バイナリだけをビルドする」— `cargo build --help` の定義どおり）。
`flpdf-test-driver` を同じ crate に `[[bin]]` として追加しても、この 2 箇所の
コマンドに `--bin flpdf-test-driver` を足さない限り、クリーンな checkout では
生成されない。shim（n9t0.3）をいくら正しく配線しても、実行対象のバイナリが
存在しなければ 21 件は依然として失敗する。n9t0.3 の作業内容にこの更新を含める。

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

**`test_functions` は `test_0_1` を id 0 と id 1 の両方に登録している**
（`test_driver.cc` の `{0, test_0_1}, {1, test_0_1}, {2, test_2}, …`）。
id 0 は偶数なので `n % 4 == 0` → パス経由の `processFile(filename1)`（メモリではない）
を通り、さらに `runtest` 冒頭の `if (n == 0) { pdf.setAttemptRecovery(false); }` で
recovery を無効化する — id 1（recovery 既定値のままメモリ読込）とは読込経路も
recovery 設定も異なる。id 3（`test_3`）は奇数なのでメモリ経路。

**basic-parsing.test は id 0 を一度も呼ばない**（`%goodtest_overrides` は good14 の
`3` だけで、他はすべて既定の `1`）ので、n9t0.2 の 20 subtest 自体は id 1 の
メモリ経路だけで完結する。ただし id 0 は「未実装の別関数」ではなく test_0_1 と
**同一の関数**なので、`invalid test 0` を fail-loud で返すと qpdf の実際の
サポート範囲より狭くなる（§3.1 の ~97 個の他 `.test` 呼び出しに影響しうる）。
id 0 のパス経由・no-recovery 読込は本 issue の検証範囲外（flpdf 側のどの
`Pdf::open*` が qpdf の `setAttemptRecovery(false)` と一致するか未確認）のため、
**n9t0.2 では明示的にスコープ外とする**（§9 参照）。分岐そのものは qpdf の形で書く。

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

> **[provisional — settled by TDD, not by this document]**

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

**参照チェーンを終端まで辿る。** `Pdf::resolve_borrowed` は 1 回の解決で止まる —
対象オブジェクトの本体自体が別の参照（`/QTest 6 0 R` で obj 6 の本体が `7 0 R`、
obj 7 の本体が `true`）だと `Object::Reference` のまま返る。qpdf の
`QPDFObjectHandle::dereference()` は終端まで辿るので、1 回だけの解決では
`qtest.getTypeCode()` が中間の参照を分類してしまい qpdf と食い違う。flpdf 本体には
`ref_chain.rs` にまさにこの用途の共有プリミティブ（`resolve_ref_chain` /
`terminal_ref_of_chain`、深さ上限 `MAX_REF_CHAIN_DEPTH = 64`、20 モジュールが使用）が
あるが **`pub(crate)`** — 別 crate の `Handle` からは呼べない。同じ形（`Object::Reference`
である限り解決を繰り返す、64 hop で打ち切り）のループを `handle.rs` にローカルに実装する。
**`indirect` に残すのは最初の 1 hop の `ObjectRef` だけ**（qpdf の `unparse()` はハンドル
自身の objgen を出す。`good3`/`good4` の `unparse: 7 0 R` は 1 hop の例なので多段でも
振る舞いは変わらない）。`resolved` にはチェーンを辿り切った終端値を格納する。

> **[/provisional]**

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

> **[provisional — settled by TDD, not by this document]**

- **decode の前に `/Filter` と `/DecodeParms` を § 4 の参照チェーン解決で解決する
  ——top-level・配列要素のどちらも、両方とも多段チェーンでありうる。**
  `decode_stream_data` は `dict.get("Filter")` / `dict.get("DecodeParms")` を直接
  `decode_stream_data_with_filters` へ渡すだけで間接参照を解決しない（`filters.rs`）。
  `Object::Reference` が来ると `decode_filter_specs`（`stream_filter.rs:55-59`）が
  `Unsupported` を返す。
  - **top-level が多段チェーンでも 1 hop では足りない。** `/Filter 8 0 R` で obj 8 の
    中身が `9 0 R`、obj 9 が `/FlateDecode` という 2 hop 以上の形は、1 回だけの
    `resolve` では `Object::Reference` のまま残り `decode_filter_specs` の
    `Some(_) => Err` に落ちる。§4 のチェーン解決子をここにも適用する。
  - **配列要素も同様。** `/Filter [8 0 R]`（配列要素が間接参照）は
    `decode_filter_specs`（`stream_filter.rs:47-53`）が各要素へ直接 `as_name()` を
    呼ぶだけなので、要素ごとに §4 のチェーン解決を適用してから渡す必要がある。
  - **`/DecodeParms` 辞書の値も同様。** `/DecodeParms << /Predictor 8 0 R
    /Columns 9 0 R >>` のように **辞書の値**が間接参照だと、
    `FlateLzwStreamFilter::set_decode_params`（`stream_filter.rs:238-`）が
    `params.iter()` で得た値をそのまま `clamped_int_param`（`value.as_integer()`
    を呼ぶだけで解決しない）に渡すため `None` になり、`filterable = false`。
    `/Filter` / `/DecodeParms` の解決は「値そのもの」だけでなく、値が
    dictionary なら**そのエントリ値も**チェーン解決してから渡す必要がある。
  - **`/DecodeParms` の「コンテナ自体」も間接参照でありうる —
    しかもこちらは unfilterable にすらならない、より悪い失敗モード。**
    `/DecodeParms 8 0 R`（コンテナ全体が間接）や `/DecodeParms [9 0 R]`
    （配列要素が間接な辞書）は、`decode_filter_specs` の decode_params
    array 分岐（`stream_filter.rs:66-80`、各要素を Null チェックのみで
    そのまま通す）を経て `set_decode_params` に渡ると、
    `params.as_dict()` が `Object::Reference` に対して `None` を返すため
    `let Some(params) = params.as_dict() else { return true; }` の
    早期リターンに入る。これは「フィルタ不可」ではなく**「パラメータ無し
    （デフォルト値）として黙って通す」**分岐 — qpdf が実際の Predictor/Columns
    で decode する場面で、flpdf は既定値で decode してしまい**中身の異なる
    バイト列を出力する**。単なる unfilterable 誤判定より悪い。
    `/DecodeParms` は「値そのもの（top-level 間接）」「配列要素」「配列要素が
    指す辞書のエントリ値」の 3 層すべてを §4 のチェーン解決に通してから
    `set_decode_params` に渡す。
  
  qpdf は `QPDFObjectHandle::isName()` / `getIntValueAsInt()` の自動 dereference で
  top-level・配列要素・辞書エントリのいずれもチェーンの終端まで解決するので、
  flpdf 側も**値の形（直接参照か配列か辞書か）で分岐せず、resolve 対象を毎回
  §4 の同じチェーン解決子に通す**のが正しい設計。
  既存の `flpdf-qtest-tools`（旧 `flpdf-test-compare`）の `compare.rs` の
  `resolve_stream_keys` は全く同じ理由で存在するが 1 hop しか見ておらず
  （top-level・配列要素どちらも未対応）、この限界は `flpdf-qtest-tools` 側の
  compare 精度（zlib 差異の許容漏れ、false-negative）にも影響する
  （flpdf-n9t0.8、follow-up）。test_driver の設計としては新規実装側だけを
  正しくすれば足りる

> **[/provisional]**

この 20 subtest は既存の flpdf 公開 API だけで完結する — `Pdf::open_mem_with_options`
（**`Pdf::open_mem` ではない**。後者は `Pdf::open(Cursor::new(bytes))` に委譲し
`PdfOpenOptions::default()`（`repair: true`、qpdf既定recovery）を使う。strict経路が必要なら
`repair: false` を明示する。qpdf の `QPDF` は
`attempt_recovery{true}` がデフォルトメンバ初期化子（`QPDF.hh:1461`）— `runtest` が
`setAttemptRecovery(false)` を呼ぶのは `n == 0` のときだけなので、id 1（と id 3）は
recovery 有効のまま読み込む。`repair: true` を明示した `PdfOpenOptions` で
`open_mem_with_options` を呼ぶ） / `trailer()` /
`resolve_borrowed()` / `Stream { pub dict, pub data }` / `decode_stream_data` /
`write_pdf`。strict な `repair: false` オプションのまま実装すると、qpdf が repair で読める
壊れた xref/trailer を持つ入力に対して test_0_1 を呼ぶ前の読込段階でエラーになり、
`invalid test N` にも正常な test_0_1 出力にもならない — repairable な壊れ方をする
fixture を 1 本 fixture 一覧（§7）に加えて検証する。

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

**good14 の golden テストは merged-stream capture が要る。** `assert_cmd::Command::output()`
は stdout/stderr を別バッファで捕捉する。`good14.out` は
`<610062> (MOO)WARNING: good14.pdf (offset 628): …` のように **stdout の途中に
stderr の警告が改行なしで割り込む**形を要求するため、別バッファ比較では
この interleaving を検証できない（stdout 側と stderr 側を別々に正しく出しても
テストが偽陽性で通る）。n9t0.6 の golden テストは、子プロセスの stdout と stderr を
同じファイル記述子へ向けて起動し（Unix なら stderr を stdout の fd に `dup2`
する、または `std::process::Command` に生の `Stdio` をハンドオフする）、
端末が見るのと同じバイト列で比較する。**merged バイト列の比較だけでなく exit code 0
もアサートする** — §7 の `driver_goldens.rs` に追加した `.assert().success()`
要件（round 3）は test_0_1 側にしか書いておらず、merged capture は生の
`Stdio` を使うため `assert_cmd` の `.assert()` 経由にならない。merged 出力が
正しいまま exit 2 する回帰（qtest は exit code も見るので FAIL になる）を
このテストが見逃さないよう、`Command::status()` / `wait()` の終了コードを
明示的に確認するコードをテストに含める。

**警告に載るファイル名は argv[2] の生文字列そのもの。** `test_driver.cc` の `runtest`
は `n % 2 == 1`（test_3 が該当）で `processMemoryFile(filename1, …)` を呼び、
`filename1 = argv[2]`。この文字列は qpdf 内部で「description」として保持され、
警告の書式（`QPDFExc::createWhat`、`<filename> (offset N): <message>`）にそのまま
載る。`good14.out` の `WARNING: good14.pdf (offset 628): …` が `good14.pdf` という
**相対パスの basename** なのはこのため。golden テストが
`CARGO_MANIFEST_DIR` 由来の絶対パスを argv に渡すと、警告の埋め込み文字列が
コミット済み `.out` と一致しなくなりチェックアウト場所依存になる。
n9t0.6 の golden テストは、子プロセスの working directory を fixture ディレクトリに
設定し、argv には basename（`good14.pdf`）だけを渡す（または、他に安定した
一意の相対文字列を argv[2] として固定する）。

§7 の `driver_goldens.rs`（test_0_1 用）は
この問題を持たない — **`repairable_input` fixture を除き**、test_0_1 のフィクスチャは
全て整形式で `decode_stream_data` のデフォルト経路（`reject_decode_warning`、警告を
stderr ではなく `Err` にする）を通るため、stderr 出力自体が発生しない。

**`repairable_input` fixture は stderr が空でない側の例外。** `QPDF::warn()`
（`QPDF.cc:487-493`）は `suppress_warnings` でない限り `"WARNING: " + e.what() + "\n"`
を stderr（既定 logger の `getWarn()`）に書く。id 1 は `setAttemptRecovery(false)` を
呼ばない（§3）ので repair が発火すれば警告が出る。`reconstruct_xref`
（`QPDF.cc:516-`）は最低 3 行の warn を出す:

```
WARNING: <filename>: file is damaged
WARNING: <filename>: <実際に repair の引き金になった例外のメッセージ>
WARNING: <filename>: Attempting to reconstruct cross-reference table
```

（`damagedPDF("", 0, …)` は object/offset が両方空なので `QPDFExc::createWhat` の
括弧書き `(object, offset)` 部分は出ない — `<filename>: <message>` の形。）
この fixture の golden は上記 3 行 + 正常な test_0_1 出力（exit 0）を要求する。

**`repairable_input` も good14 と同じ理由でファイル名固定と merged capture が要る。**
警告文中の `<filename>` は argv[2] の生文字列なので（§6 と同じ argv[2]→description の
経路。ここは `n % 2 == 1` の `processMemoryFile` 経由で、good14 と同じ「description
に argv がそのまま載る」形）、working directory を fixture ディレクトリに設定し
argv には basename だけを渡す。**さらに、stdout/stderr を別バッファでアサートする
だけでは不十分** — repair の 3 行 warn は test_0_1 が動く**前**（pre-dispatch の
open 中）に出るべきものなので、正しい実装は「stderr 3 行 → stdout 5 パート」の順で
書き込む。診断を溜めて test_0_1 の後にまとめて出すような実装は、stdout と stderr を
別々に比較する Cargo test では両方とも内容が一致するため検出できないが、qtest の
merged capture（両ストリームをインターリーブしたまま比較）では順序が食い違って
FAIL する。§6 の merged capture の仕組みをこの fixture にも適用する。

## 7. fixture とテスト

```
tests/fixtures/test_driver/
  README.md        flpdf-authored の license 注記（compare_for_test/README.md に倣う）
  generate.sh      PDF は python3 で生成、期待出力は本物の test_driver で生成
  repairable_input.{pdf,out}     壊れた xref/trailer だが qpdf が repair で読める
                                  入力（§5 参照。strict な Pdf::open_mem のままだと
                                  この fixture だけ pre-dispatch で読込エラーになり
                                  test_0_1 に到達しない。stderr が空でない唯一の
                                  test_0_1 fixture — 3 行の WARNING を要求する。
                                  working directory を fixture ディレクトリに設定し
                                  argv には basename だけを渡す点、merged capture で
                                  順序まで検証する点は good14 と同じ扱い）
  implicit_null.{pdf,out}        欠落キー
  direct_null.{pdf,out}          /QTest null
  dangling_ref.{pdf,out}         存在しないオブジェクトへの参照
  indirect_null.{pdf,out}        実在する null オブジェクト
  indirect_bool_true.{pdf,out}   hasKey が true になる対照。/QTest = true
  indirect_bool_false.{pdf,out}  同上、/QTest = false（bool 値そのものを
                                  出力に含む分岐なので、true 固定で出す
                                  実装を通してしまわないよう両方必要）
  chained_reference.{pdf,out}    多段間接参照（§4 参照。/QTest 6 0 R -> 7 0 R -> true
                                  のような 2 hop 以上のチェーン）
  integer.{pdf,out}              top-level integer（good7 相当）
  real.{pdf,out}                 top-level indirect real（good8 相当）
  string_hex_literal.{pdf,out}   n9t0.4 の境界（\n 入り・閾値ちょうど・8 進フォールバック）
  name_escape.{pdf,out}          /hex#20strings 相当
  array_indirect.{pdf,out}       要素ごとの direct/indirect（real literal の
                                  verbatim 保持 0.0 -0.0 0. -0. も good10 に倣いここで確認）
  dict_keys.{pdf,out}            キーの lexicographic 順 **かつ少なくとも 1 つは
                                  間接値**（`/a 8 0 R` のように）を含む。§5 の
                                  「  /key is {in,}direct」出力は direct/indirect
                                  両方を実測しないと、常に direct を返す実装が
                                  全 golden を通ってしまう（ASCII-safe キーのみ。
                                  エスケープが要るキーは flpdf-n9t0.7 が着地するまで
                                  追加できない — §4 参照）
  stream_flate.{pdf,out}         raw / uncompressed / dict unparse
  stream_indirect_filter.{pdf,out}       /Filter 自体が間接参照のストリーム（§5 参照）
  stream_chained_filter.{pdf,out}        /Filter 8 0 R で obj 8 の中身が 9 0 R、
                                          obj 9 が /FlateDecode という 2 hop 以上の
                                          top-level チェーン（§5 参照。1 回だけの
                                          resolve では Object::Reference が残り失敗する）
  stream_indirect_filter_array.{pdf,out} /Filter が [8 0 R] のように配列要素が
                                          間接参照のストリーム（§5 参照。
                                          top-level 1 hop の resolve では救えない）
  stream_indirect_decode_parms.{pdf,out}  /DecodeParms << /Predictor 8 0 R
                                          /Columns 9 0 R >> のように辞書の
                                          エントリ値が間接参照のストリーム（§5 参照）
  stream_indirect_decode_parms_container.{pdf,out}  /DecodeParms [9 0 R]
                                          のようにコンテナ自体・配列要素が
                                          間接参照で、かつ Predictor が
                                          効いている（デフォルト値と異なる）
                                          ストリーム（§5 参照。unfilterable
                                          にはならず、既定値で decode した
                                          誤ったバイト列を返す方の失敗モード
                                          — decoded 出力の byte 比較でしか
                                          検出できない）
  stream_unfilterable.{pdf,out}  Stream data is not filterable.

tests/fixtures/test_driver/test_3/
  tokenizing_pipeline.{pdf,out}  flpdf-authored な test_3 (n9t0.6) 用 fixture。
                                  good14.pdf 自体は vendor からコピー禁止（下記）
                                  なので、bad token・コメント・CR/CRLF混在・
                                  未終端 inline image marker など good14.out が
                                  例示する normalization の性質を再現する別内容の
                                  PDF を新規に作る。n9t0.6 の merged capture
                                  golden（§6）が「qpdf ビルド不要で ordinary
                                  cargo test が回る」ためにはこの fixture が
                                  必須 — §6 に merged capture の仕組みだけ書いて
                                  対象となる入力そのものを inventory に
                                  加えていなかった
```

`test_0_1` は `/QTest` 1 個の型でしか分岐できないため、`integer.pdf` と `real.pdf`
は分けた別ファイルにする（`good7`/`good8` も qpdf 側で別ファイル）。1 ファイルに
両方を詰めると array 分岐だけを通り、integer/real 分岐は未検証のまま残る。

`flpdf-qtest/vendor/qpdf-qtest/` からのコピーは一切しない
（`tests/fixtures/compare_for_test/README.md` の方針）。

**通常の `cargo test` が実際にこの比較を行う経路**: `crates/flpdf-qtest-tools/tests/driver_goldens.rs`
がコミット済みの `tests/fixtures/test_driver/*.{pdf,out}` を読み、`flpdf-test-driver` を
`assert_cmd` 経由で起動して stdout を `.out` と突き合わせ、**かつ `.assert().success()`
で exit code 0 も、stderr が空であること（`repairable_input` を除く。上記の
3 行の WARNING が期待値）も**アサートする（qpdf ビルド不要）。
`assert_cmd::Command::output()` は非 0 終了でも stdout を返すため、stdout 比較だけでは
「期待どおりのバイトを出力してから exit 2 する」回帰を見逃す — qtest 自身は
`basic-parsing.test` の `EXIT_STATUS => 0` で終了コードを見ているので、golden テスト側
もこれを見ないと qtest が FAIL とみなすケースを golden が PASS させてしまう。stderr も
同様: qtest は stdout/stderr を両方まとめて期待出力と突き合わせるので、
stdout・exit code が正しいまま stderr にだけ余計な warning/diagnostic が出る回帰は、
stderr を見ない golden では検出できない。

**fail-loud dispatch（§3.1）の契約もこの golden スイートで検証する。** 成功ケース
（`test_0_1` fixture）だけでなく、未実装番号を渡したときに `invalid test <n>` +
exit 2 になることを確認する negative test と、§3 の「読込がルックアップより先」
（壊れた PDF を未実装番号に食わせると parse エラーが優先される）を確認する test を
`driver_goldens.rs` に加える。n9t0.3 で shim を PATH に置いた瞬間、basic-parsing 以外の
`.test` が呼ぶ ~97 個の test 番号すべてに fail-loud の契約が効くため、ここが黙って
壊れると golden 全緑のまま互換性ベースラインだけが動く。

**negative test は部分一致ではなく完全一致を要求する。** `.stderr(predicate::contains(...))`
のような部分一致は、`invalid test <n>` を出しつつ `test <n> done` も stdout に
漏らすバグや、malformed PDF ケースで parse エラーと `invalid test <n>` の**両方**が
出るバグを見逃す（どちらも期待文字列を「含む」ので通ってしまう）。qpdf は最初の
例外で止まり、qtest は捕捉したバイト列をそのまま突き合わせるので、余計な出力は
観測可能な差分になる。negative test は stdout が空であること、stderr が
期待する診断 1 行ちょうどであることを要求する（競合する診断が無いことも
明示的にアサートする）。

**§3 の argc 境界の両側を golden でカバーする。** `argc < 3 || argc > 4` の境界は
「受理される 4 引数形（`arg2` あり）が誤って reject されない」ことと
「2 引数・5 引数が Usage + exit 2 になる」ことの両方を確認しないと片手落ち。
4 引数を誤って reject する実装は `test_0_1` の成功ケース（3 引数のみ使う。§3 参照）
では検出できないが、qtest が `test_driver <n> <file> <password>` の形で未実装番号を
呼ぶ場面（`test_2`, `test_35`, `test_36` 相当）では Usage を返すべきでないところで
返してしまい、fail-loud の互換性ベースラインを変える。**ただし arg2 を id 1 / id 3
の読込に転用してはならない。** `runtest`（`test_driver.cc:3492-3494`）で `arg2` を
password として使うのは `n == 35 || n == 36` の分岐だけ（コメント
`// arg2 is password` 参照）で、id 1・id 3 が通る `else` 分岐
（`processMemoryFile(filename1, file_buf.get(), size)`）は `arg2` を一切参照しない。
4 引数形は**構文として受理するだけ**でよく、`PdfOpenOptions::password` へは繋がない
（暗号化 fixture でのパスワード転送テストは id 35/36 相当を実装する段になってから）。
`scripts/qpdf-test-driver-diff.sh`（既存の `qpdf-{tokenizer,rc4,lzw-png,stream-codecs}-diff.sh`
と同じ形）は別役割で、`scripts/fetch-qpdf-source.sh` で pinned source を取り本物の
`test_driver` をビルドして fixture を再生成・オラクル照合するための開発者ツール。
CI/`cargo test` はコミット済み `.out` に対する `driver_goldens.rs` だけを回す。

**この設計の受け入れ条件は、自作 fixture の golden が全緑になることではなく、
本物の basic-parsing.test の 21 subtest が実際に PASS することである。**
`driver_goldens.rs` と `qpdf-test-driver-diff.sh` はどちらも自分で用意した
synthetic fixture しか検証しない — shim の配線ミス（n9t0.3）や、good1〜good21
固有の何か（例えば実ファイルのオフセット・エンコーディングの組み合わせ）に
起因する乖離は、これらのテストが全緑でも見逃しうる。n9t0.3 と n9t0.6 が
揃った時点で、flpdf-qtest の `FLPDF_DIR` 経由ビルド（本文書で既に basic-parsing
の subtest 38/39 の PASS を確認したのと同じ手順）で `basic-parsing.test` を
実走し、`test_driver N goodM.pdf` 形の 21 subtest（`implicit null` 〜
`array with indirect nulls`、good14 の `tokenizing pipeline` を含む。
「create qdf」「check output」は qpdf-cli 側で既に PASS 済みの別 subtest なので
ここには含まない）が **qtest の PASS/FAIL 判定として**すべて PASS することを
確認する。これが本 epic（flpdf-n9t0）のゴール達成の一次証拠であり、
`driver_goldens.rs` 全緑はその代理指標に過ぎない。

**flpdf を出力生成に使う場面では必ず `flpdf rewrite` を使う。** トップレベルの
`flpdf in out` は完全な書き直しをせず入力にバイトを追記する別経路で、qpdf の
`qpdf in out` とは挙動が違う（epic の good17 非 QDF 失敗 67/68/69 と符合する）。

### 7.1 カバレッジ

`n9t0.5` の一部として `scripts/patch-coverage.sh` の `REPORT_PREFIXES` を
`crates/flpdf-test-compare/src/` から `crates/flpdf-qtest-tools/src/` へ更新する
（PR #589 で実施・検証済みだが、**PR #589 自体は本稿時点で未マージ**（`gh pr view 589`
→ `OPEN`）。§2 のレイアウト（`crates/flpdf-qtest-tools/`）は #589 マージ後に main へ
着地する — n9t0.2 の実装を始める前に #589 のマージ状態を確認すること。§8 参照）。
一方 **n9t0.4 は `crates/flpdf/src/` を触ったので変更行 100% ゲートの対象**だった
（変更 82 行 / 未カバー 0 で PASS）。

## 8. 残るリスク

1. **`write_pdf` と `QPDFObjectHandle::unparse` の一致は全網羅では未検証。**
   real / name / dict / array は good7/8/10/11/13/15/21 で実測一致を確認したが、
   本当の担保は probe で全 fixture を突き合わせること。
2. **辞書キーのエスケープ欠落（flpdf-n9t0.7、follow-up）。** §4 参照。
   n9t0.2 本体はブロックしないが、`dict_keys` fixture の対象範囲を制限する。
3. **PR #589（crate リネーム）が本稿時点で未マージ。** `gh pr view 589` → `OPEN`。
   §2 のレイアウト（`crates/flpdf-qtest-tools/`）と §7.1 のカバレッジ prefix
   更新は #589 のブランチ上では実施・検証済みだが、`origin/main` にはまだ
   `crates/flpdf-test-compare/` のまま存在する。n9t0.2 の着手前に #589 のマージ
   状態を確認すること — 未マージのまま §2 のパスを前提に実装を始めると、
   存在しないディレクトリを探すことになる。

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

## 9. スコープ外

- `test_driver` の残り ~97 個の test 関数（fail-loud で `invalid test N`）
- **id 0（`test_0_1` のもう一方の登録先）** — basic-parsing.test は呼ばないため
  n9t0.2 の 20 subtest には無関係。パス経由・no-recovery の読込に対応する
  flpdf 側 API が未検証（§3 参照）。実装するまでの間、id 0 は
  `invalid test 0` を返す — qpdf の実際のサポート範囲より狭いことは
  承知の上でのスコープ判断（follow-up 候補）
- flpdf 本体への `ObjectHandle` 公開 API の追加
- binary 名の変更、compare アルゴリズムの変更

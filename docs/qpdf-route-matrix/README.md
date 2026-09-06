# flpdf ↔ qpdf route matrix（canonical / bridge / consumer 棚卸し）

**Oracle:** qpdf 11.9.0（`scripts/fetch-qpdf-source.sh --print-path` で解決される pinned source と
`/usr/bin/qpdf` 11.9.0 の実機挙動）。本表の引用（`libqpdf/X.cc:N-M`、`crates/<crate>/src/<file>.rs::<Symbol>`）は
`scripts/check-qpdf-route-matrix.py --check` でファイル・行範囲・識別子の実在を検証する。
**関連:** [`docs/qpdf-correspondence.md`](../qpdf-correspondence.md)（責務対応表。本表はその上に
「経路（route）」軸を足したもので、対応表の行を置き換えない）/ Beads `flpdf-3yn9.41`（親 epic `flpdf-3yn9`）
**調査日:** 2026-09-06（再監査 `flpdf-3yn9.48`、文書更新 `flpdf-3yn9.48.1`）。
監査開始時の main `4a2faf5c` と pinned qpdf source に基づき、領域別表の分類・責務境界・既存issueとの対応を更新した。
160行は履歴上の行集合を維持している。今回の分類更新は全行の parity テスト合格を意味しない。
未更新の行番号は過去snapshotを含む。

`scripts/check-qpdf-route-matrix.py` は
ファイルと行範囲の実在・識別子の宣言のみを検証し、行の中身の一致は検証しないので、行番号は
対応表と同じくスナップショットとして読み、cutover slice が各行に触るときに再アンカーする。

## 1. 目的

同じ qpdf 責務が flpdf の複数経路に分散していると、個別テストが通っても全 consumer で同じ挙動になる
保証がなく、route を一括切替したときに差分の責任箇所も追えない（直近の例: encrypted non-linearized
Preserve ObjStm の採番が、qpdf の enqueue 時 container-first に対して一部 route だけ Catalog-first +
container-above-max だった — `flpdf-hi08` / PR #1486）。本表は残る mixed route を横断して

1. 各 qpdf 責務の **canonical owner を 1 つだけ** 定め、
2. legacy bridge を「旧表現を翻訳する層」としてのみ残し、その **残 caller を機械的に追跡** し、
3. consumer を 1 つずつ cutover するための **順序・前提・RED test・完了判定** を定義する

ための preflight である。production semantics・public API はこの文書では変えない。

### 全体集計（160 行）

領域 A〜E の 5 ファイルを合わせた分類の内訳は次の 1 組だけである。以降の節はこの数を再掲しない。

| canonical | bridge | mixed | unknown | 合計 |
|---|---|---|---|---|
| 62 | 11 | 86 | 1 | 160 |

再現コマンド（`\|` でエスケープされたセル内パイプを先に潰してから7列目を読む）:

```sh
for f in docs/qpdf-route-matrix/[a-e]-*.md; do
  sed 's/\\|/§/g' "$f" | awk -F'|' '$2 ~ /^ *(A|B|C|D|E-)[0-9]+ *$/ {gsub(/ /,"",$7); print $7}'
done | sort | uniq -c
```

## 2. 方法

- **qpdf から出発する。** 各領域はまず qpdf 側の state / call order / error・warning boundary を
  書き出し、その後で flpdf の entrypoint を対応付ける（`.claude/rules/qpdf-port-design-patterns.md` 1）。
- **識別子・行範囲は書く前に実在確認する**（同 7）。qpdf 側は `rg -n '<symbol>' $Q/libqpdf/<file>` と
  `sed -n 'N,Mp'` で読んだ範囲だけを引用する。flpdf 側は `rg -n` の出力を根拠に caller を数える。
- **caller の数え方**: `rg -n '\b<symbol>\b' crates --glob '*.rs'` を production（`src/` の非 `#[cfg(test)]`
  部分）と test（`tests/` および `mod tests` 以降）に分けて `prod: N (files) / test: M` と書く。
  領域ファイルはこの規約を各自の冒頭で細則化しており、**5 ファイルの細則は完全には一致していない**
  （§8 の X-6）。以降の再測定は `scripts/qpdf-route-callers.py` の実装を唯一の規約とする（§6）。
- **probe の書式**: source だけで観測挙動が決まらない行は `probe: <コマンド> → <観測>` を evidence 列に書く。
  probe を実行していない推測は書かない（unknown にする）。
- **履歴差分**: PR #1486 由来の記録には `(#1486)` が残るが、現在の再監査は merge 後の tree を対象とする。過去の完了範囲と現在残る consumer 移行を区別する。

### 行フォーマット（各領域ファイルの表は 8 列固定）

| # | qpdf responsibility owner | qpdf evidence | flpdf current entrypoint | callers (prod / test) | classification | canonical owner | remaining bridge callers / notes |
|---|---|---|---|---|---|---|---|

- **qpdf responsibility owner**: `QPDF::resolve` のような実在識別子（複数可）。
- **qpdf evidence**: `libqpdf/QPDF.cc:1700-1753` / `include/qpdf/QPDF.hh:724-996` / `probe: ...`。
- **flpdf current entrypoint**: `crates/flpdf/src/reader.rs::Pdf::resolve`（visibility を併記: `pub` / `pub(crate)` / private）。
- **callers (prod / test)**: `prod: 3 (writer.rs, job/lifecycle.rs) / test: 12`。
- **classification**: §3 の 4 値のみ。
- **canonical owner**: その責務の flpdf 側正本（1 つだけ）。無ければ `absent`。
- **remaining bridge callers / notes**: bridge / mixed は残 caller を全列挙。unknown は必要 probe。

## 3. 分類定義

| 分類 | 定義 |
|---|---|
| **canonical** | flpdf の当該 entrypoint がその qpdf 責務の唯一の正本で、アルゴリズム・呼び出し順序が cite した qpdf code と 1:1 に対応し、production caller が全てここを通る。 |
| **bridge** | それ自体に qpdf 対応物が無く、canonical route が全 consumer に行き渡れば不要になる経路。2 形がある: (i) 旧表現と canonical 表現を翻訳するためだけに存在する層、(ii) qpdf に対応する処理が無い flpdf 固有の補助経路（CLAUDE.md 逸脱分類 (C) — 例: 明示的 `Pdf::resolve` による解決タイミング補正、dirty 追跡、retry 予算）。どちらも削除対象であり、残 caller を列挙する（ゼロなら削除候補）。caller が 20 を超える場合は、再現可能な `rg` コマンドとファイル別件数で列挙に代える。責務レベルでは qpdf 対応物があっても、経路（入口）としての対応物が無ければ bridge になりうる（例: qpdf が公開しない private 処理を `pub` で包んだ死んだ wrapper）。bridge に qpdf semantics を足さない。 |
| **mixed** | 1 つの qpdf 責務が flpdf 側で 2 つ以上の経路に分かれ、順序・採番・診断のいずれかが経路間で異なりうる状態。または 1 つの flpdf 経路が 2 つ以上の qpdf 責務を畳んでいる状態。 |
| **unknown** | qpdf source / 既存 probe では責務境界を決められない。必要な追加 source 箇所か probe コマンドを書き、推測で分類しない。 |

`docs/qpdf-correspondence.md` の ✅ / 🔀 / ⚪ とは別の述語である: 対応表は「責務の対応と境界一致」、
本表は「その責務に至る **経路が 1 本か**」を問う。✅ の行でも consumer 側に bridge が残っていれば
本表では mixed / bridge になりうる。

履歴行の例外: C44は未実装public責務を既知の未移植としてmixedに保持する。
D19/D30はcanonical ownerへ委譲するbyte-neutral test scaffolding、D27は当該pre-write sweep撤去完了としてcanonicalに分類する。
この例外をproduction routeの重複や全体parityの証拠に広げない。

## 4. 領域別 matrix

| ファイル | 行数 | canonical | bridge | mixed | unknown |
|---|---|---|---|---|---|
| [A. ObjectHandle / Resolver — object identity, lazy resolve, ownership, teardown](a-objecthandle-resolver.md) | 24 | 6 | 5 | 13 | 0 |
| [B. parser / xref recovery / warning・error・diagnostics](b-parser-recovery-diagnostics.md) | 34 | 13 | 1 | 19 | 1 |
| [C. stream data provider / decode / retry / filter / encryption / `/Length`](c-stream-pipeline-encryption.md) | 42 | 26 | 2 | 14 | 0 |
| [D. writer — reachability, ObjStm planning / renumber / emission, xref / trailer, encryption, linearize](d-writer.md) | 31 | 12 | 0 | 19 | 0 |
| [E. QPDFJob / CLI / C API 相当の consumer・adaptor](e-job-cli-capi.md) | 29 | 5 | 3 | 21 | 0 |

## 5. 責任境界と不変条件

本節は **記述的** である（cutover の設計は §7）。各表は「破ると出力バイトまたは観測可能な診断が
どう変わるか」を優先して並べた不変条件で、`flpdf の現状` 列は領域ファイルの行 ID（A7 / D12 …）を指す。
本節に残る個別caller数は初回監査の履歴値で、現在の機械計測は§6.2、owner別判定は領域表の再監査行を参照する。

### 5.A ObjectHandle / Resolver

| 不変条件 / 境界 | qpdf の根拠 | flpdf の現状（該当行） | 壊すと何が変わるか |
|---|---|---|---|
| **`getObject` は resolve しない。resolve が起きるのはアクセサの `dereference()` からだけ**（`QPDF::resolve` を呼べるのは `QPDFObject` のみで、public な明示 resolve 入口は存在しない） | `libqpdf/QPDF.cc:1951-1959`（「This method is called by the parser and therefore must not resolve any objects.」）/ `include/qpdf/QPDF.hh:770-781`（`Resolver` の friend は `QPDFObject` 1 つ）/ `include/qpdf/QPDF.hh:1031` | A3 canonical（`crates/flpdf/src/reader.rs::Pdf::get_object_handle`）、A4 / A5 canonical。ただし A7 が `Pdf::resolve` という qpdf に無い public 入口を開けており prod 256 箇所が使う | parse 中に resolve が誘発されると qpdf が `std::logic_error` にする再入状態（B5）が flpdf では観測できないまま通る。逆に A7 を残したまま A6 を統合すると、`pdf.resolve(&h)?;` → `h.as_dictionary()` の 2 段イディオムが恒久化する |
| **型アクセサは必ず dereference する** — 未解決の間接 handle でも `asInteger` / `isNull` は正しい型と値を返す | `libqpdf/QPDFObjectHandle.cc:240-446` / `libqpdf/QPDFObjectHandle.cc:2375-2383` | A6 mixed。解決しない `as_*` / `is_null` 族が prod 合計 686、解決する `try_as_integer` が prod 28 | 未解決の間接 handle に `as_dictionary()` が `None`、`is_null()` が `false` を返す。`/Filter` や `/Type` の判定でこれが起きると分岐が落ち、書き出しバイトが変わる |
| **object cache に「削除済み」の永続 tombstone は存在しない** — `removeObject` は cache cell ごと erase し、`deleted_objects` は xref 構築が終われば clear される | `libqpdf/QPDF.cc:1995-2005` / `libqpdf/QPDF.cc:706-708` / `libqpdf/QPDF.cc:575` | A2 mixed（`CacheEntry` の `Missing` / `Deleted` に qpdf 対応物なし）。A17 は tombstone を手で消す分岐を持ち、`crates/flpdf/src/reader.rs:1569-1575` のコメント自身が逸脱を明記 | `get_all_objects`（A9）と `live_object_refs`（A10）の列挙が食い違い、writer の到達性集合が経路ごとに変わる。同じ入力で出力 object 数が route 依存になる |
| **通常の document-owned 型不一致は warning + null/false。dereference や contextless warning は throw しうる** | `libqpdf/QPDFObjectHandle.cc:2168-2189,965-989` | A8 mixed。両 accessor 族とも resolve し、convenience `get_key` / `has_key` が `try_*` の `Err` を panic に変換する | 通常の型不一致自体は panic の証拠ではない。lazy resolution・warning 配送・contextless warning の例外経路を fallible accessor へ移し、warning と例外伝播の境界を保つ |
| **採番は `getObjectCount()+1` の 1 本**（`obj_cache` の最大 key に基づく） | `libqpdf/QPDF.cc:1872-1880` / `libqpdf/QPDF.cc:1271-1283` | A11 mixed。facade 側 `Pdf::next_available_object_ref` は `Pdf::object_refs()`（A10、legacy cache 混じり）と canonical の max を取る | legacy 側にしか無い ref が採番を押し上げ、新規 object の番号が qpdf と 1 つ以上ずれる。以降の全 xref offset が変わる |
| **`makeIndirectObject` は同じ `shared_ptr` を cache に登録する（alias が保たれる）** | `libqpdf/QPDF.cc:1882-1888` / `libqpdf/QPDF.cc:1890-1897` | A12 mixed。`Pdf::make_indirect_object_handle` は `direct_value_clone()` で shallow copy するのでコンテナが分離する | promote 後に元 handle を `appendItem` / `replaceKey` しても新 object 側に反映されない。probe A-2 |
| **teardown は `xref_table.clear()` → `obj_cache` 全件 disconnect の 1 本** | `libqpdf/QPDF.cc:215-236` / `libqpdf/QPDFObject.cc:13-17` | A20 mixed。walk が 2 本（`crates/flpdf/src/reader/resolver.rs::disconnect_all` と `crates/flpdf/src/xref.rs:85-126` の `BootstrapCache` の `Drop`）で、qpdf が先に行う `xref_table.clear()` に対応する行が無い | bootstrap 側だけが持つ handle が canonical の teardown walk から漏れる。`xref_table` を先に潰さないため「teardown 後に resolve が成功する」窓が理論上残る。probe A-4 |

### 5.B parser / xref recovery / warning・error・diagnostics

| 不変条件 / 境界 | qpdf の根拠 | flpdf の現状（該当行） | 壊すと何が変わるか |
|---|---|---|---|
| **warning sink は `m->warnings` 1 本で、順序は `warn` 呼び出し順そのもの**（`push_back` 以外に並べ替え・重複除去・分類は無い） | `libqpdf/QPDF.cc:487-494` / `include/qpdf/QPDF.hh:1475` | B28 canonical（`push_warning_with_offset` に全 push が合流）。B27 mixed — `Pdf` 構築後は 1 本だが、構築前は `BootstrapHandleState` → `XrefReadContext` → `LoadedXref` → `ResolverCore` の 3 段 staging で、連結順は `append_diagnostics_to` を呼んだ時点で決まる | `qpdf --check` の warning 行の並びが flpdf と変わる。bootstrap handle 由来の warning と xref 自身の warning を両方出す入力で顕在化する。probe B-P4 |
| **`suppress_warnings` は表示だけを止め、sink への push は常に起きる** | `libqpdf/QPDF.cc:487-494` | B28 canonical。`route_warning` が logger 行を組むのは push の後（`crates/flpdf/src/reader/resolver.rs:1911-1928`） | `--no-warn` 指定時に `hasWarnings()` が false になり exit code が 0 と 3 の間で変わる |
| **resolve 境界で例外は必ず warning に降格し、未解決なら null になる**（`QPDF::resolve` から例外は出ない） | `libqpdf/QPDF.cc:1737-1742` / `libqpdf/QPDF.cc:1745-1749` | A4 canonical。B22 mixed — resolve 側の再構築経路は entry が compressed だと `Error::Unsupported` を返す（`crates/flpdf/src/reader/resolver.rs:1674-1677`）が、qpdf は type 1 以外を一律 warn + null にする（`libqpdf/QPDF.cc:1618-1633`） | qpdf が warn + null で続行する入力で flpdf が `Err` を返し、その object 以降の処理が止まる。probe B-P3 |
| **回復予算は `bool` 1 個**（2 回目の `reconstruct_xref` は引数の例外をそのまま re-throw する。残り回数カウンタは存在しない） | `libqpdf/QPDF.cc:518-522` / `include/qpdf/QPDF.hh:1480` | B25 mixed（open 時は `already_reconstructed` を経由して `ResolverCore` に転記）。B34 bridge — qpdf に対応物のない 64 回の read-to-end fallback 予算（`crates/flpdf/src/pdf.rs:148-154`） | 破損 PDF で回復の起きる回数が変わり、reconstruct の 3 連 warn（B33）が余分に出る／出ない |
| **reconstruct が xref から消すのは type 1 entry のみ。ObjStm の内部は意図的に走査しない** | `libqpdf/QPDF.cc:532-541` / `libqpdf/QPDF.cc:618-622` | B24 canonical（**両側 absent が 1:1 対応**）。B23 canonical で scan 本体は 2 経路が共有 | 回復後に compressed entry を復元すると、qpdf が到達しない object を出力に含める。コミット `6ddb9661` で 1 度是正済みの退行そのもの |
| **xref entry の上書き規則は 3 primitive で違う**（`insertXrefEntry` = first-seen wins、`insertFreeXrefEntry` = 未登録時のみ、`insertReconstructedXrefEntry` = 後勝ち + `deleted_objects` 抑止） | `libqpdf/QPDF.cc:1149-1184` / `libqpdf/QPDF.cc:1187-1192` / `libqpdf/QPDF.cc:1197-1210` | B19 canonical。B20 mixed — `deleted_objects` 抑止が `merge_recovered_qpdf_state` の事後 `retain` で適用され、bootstrap の 4 つの handoff のうち初段 parse 失敗（`crates/flpdf/src/xref.rs:1368-1383`）だけがこれを通らない | 増分更新 PDF でどの世代の object が読まれるかが変わる。free entry を含む classic xref の直後が壊れた入力で `qpdf --show-xref` と食い違う。probe B-P2 |
| **`QPDF::readToken` は `allow_bad = true` を保証するが、全token consumerがこの責務ではない** | `libqpdf/QPDF.cc:1535-1539,1801-1814,846-946` | B7 mixed、集約entrypointはabsent。canonical/bootstrap ObjStm headerは `next_integer`、classic xrefはByteCursorを使う | ObjStm headerはqpdfの2 token読取→integer検査順へ移行する。classic xrefはreadLine/parse_xrefEntryに対応するため、4箇所のfalseを一律trueにする修正は誤り（B-P7） |
| **`QPDFParser` は context があれば warn、無ければ同じ診断を例外に昇格する** | `libqpdf/QPDFParser.cc:487-498`（`libqpdf/QPDFParser.cc:496` が throw）/ `libqpdf/QPDFParser.cc:161-165` | B3 canonical（`has_context` 分岐 1 本）。B32 mixed — qpdf の 2 軸（例外クラス × `qpdf_error_code_e`）を `crates/flpdf/src/error.rs::Error` の 1 軸に畳んでいる | document なしの parse で構文エラーが黙って null になる。`Error::Parse` だけが reconstruct の trigger（`crates/flpdf/src/reader/resolver.rs:1615`）なので、振り分けを誤ると回復分岐自体が起きなくなる |

### 5.C stream data provider / decode / retry / filter / encryption / `/Length`

| 不変条件 / 境界 | qpdf の根拠 | flpdf の現状（該当行） | 壊すと何が変わるか |
|---|---|---|---|
| **stream の復号は pipe 時、文字列の復号は parse 時**（`decryptStream` の呼び出し元は static `pipeStreamData` の 1 箇所だけで、`resolve` / `readStream` からは呼ばれない） | `libqpdf/QPDF.cc:2489-2492` / `libqpdf/QPDF_encryption.cc:1044-1154` / `libqpdf/QPDF_encryption.cc:976-1039` | C14 / C15 / C12 canonical。`crates/flpdf/src/reader/resolver.rs:125-129` が境界を明記 | qpdf の `getRawStreamData` 自体が復号済み・filter未decodeのbytesを返す（`libqpdf/QPDF_Stream.cc:363-375`）。復号の前倒しはoriginal sourceとpipeの責務をずらし、復号済み入力を既存decrypt段へ再投入する等の不整合を招く。rawを暗号文の意味で扱わない |
| **byte source は 3 つで優先順位が固定**（`stream_data` buffer > `stream_provider` > original の `parsed_offset`+`length`）。`parsed_offset == 0` は「data 無し」で `std::logic_error` | `libqpdf/QPDF_Stream.cc:571-622` / `libqpdf/QPDF_Stream.cc:605-607` | C2 canonical（`crates/flpdf/src/object_handle.rs::pipe_stream_source`） | `replaceStreamData` 後も元ファイルを読み続ける、あるいは置換前の stream で「data 無し」の内部エラーが出る |
| **provider stream の `/Length` は `Pl_Count` の実測で検証する。不一致は programmer error（`std::runtime_error`）で、`/Length` が無ければ実測値を書き戻す** | `libqpdf/QPDF_Stream.cc:594-600` / `libqpdf/QPDF_Stream.cc:601-604` / `libqpdf/QPDF_Stream.cc:678-680` | C2 canonical、C38 canonical（`crates/flpdf/src/object_handle.rs::replace_filter_data` に `/Length` 契約を集約） | provider が宣言と違うバイト数を出しても黙って通り、`/Length` と実データがずれた PDF を出力する |
| **`willFilterStream` の判定順序**（filter-on-write veto、metadata / normalize / compress の排他 chain） | `libqpdf/QPDFWriter.cc:1254-1285,2543-2551` | C20 canonical。C22 mixed — plain の `is_data_modified()` 早期 return と linearized の事前 probe は非対称だが、plain/QDF の出力cacheと qpdf の linearize 専用 optimizer probe を含めて照合する | 非対称だけで出力不一致や早期 return 撤去を決めない。C-U3 の library harness で provider 呼出回数、filter、payload を確認する |
| **書き出し時に stream dict から削除するのは `/Filter` と `/DecodeParms` の 2 キーだけ** | `libqpdf/QPDFWriter.cc:1440-1486` / `libqpdf/QPDFWriter.cc:1451-1455` | C21 mixed。`crates/flpdf/src/writer/plain/body.rs:860-865` が `/F` `/FFilter` `/FDecodeParms` も削除する。`// qpdf-deviation:` マーカー無し | 外部ファイル参照 stream の `/F` が出力から消え、qpdf 出力と byte 単位で違う |
| **`compute_data_key` は読み側と書き側で同じ 1 実装を共有する** | `libqpdf/QPDF_encryption.cc:324-357`（呼び出し元は `libqpdf/QPDF_encryption.cc:963` と `libqpdf/QPDFWriter.cc:845` の 2 箇所のみ） | C17 / C18 canonical — `crates/flpdf/src/encryption/primitives.rs::compute_data_key` に統合し、reader `key_for_object` と writer `set_data_key` が共有。旧 writer 複製を削除した | V/R・key長5/16/24/32・AES/RC4・非zero generation を qpdf 11.9.0 C++ oracle vectors で固定。reader cache と writer generation 0 の call contract は各 consumer に保持 |
| **pipe されるバイト数は常に `length`**（`recoverStreamLength` が復元した length は `endstream` 直前の改行を含み、qpdf はそれをそのまま pipe する） | `libqpdf/QPDF.cc:2496-2500` / `libqpdf/QPDF.cc:1488-1492` | C42 mixed — `crates/flpdf/src/reader/resolver.rs::pipe_stream_data_from_input` は通常・暗号化・foreign の全 route で caller の `length` を変更しない。`RecoveredStreamEol` は `job/inspection.rs::unparse_object_with_stream_data` の `dump-object` 再シリアライズframingだけに使い、show-streamのraw/decoded payloadはrecovered spanをtrimせず出す（`flpdf-zvjf`, `flpdf-hj7v`） | `encrypted-recovered-eol.pdf` と unencrypted recovered-length fixture で qpdf と flpdf の raw bytes、recovered length、content warning、exit 3 を一致させる |
| **ObjStm / xref stream / hint stream は `willFilterStream` を通らない**（deflate を直付けする） | `libqpdf/QPDFWriter.cc:1659-1665` / `libqpdf/QPDFWriter.cc:2422-2432` / `libqpdf/QPDFWriter.cc:2286-2330` | C31 / C32 / C33 canonical。plain / linearized の両 route が同じ primitive を共有 | ObjStm 本体に `--decode-level` や `--recompress-flate` が効いてしまい、container の payload が qpdf と変わる |

### 5.D writer

| 不変条件 / 境界 | qpdf の根拠 | flpdf の現状（該当行） | 壊すと何が変わるか |
|---|---|---|---|
| **採番は enqueue 時に enqueue 順で行い、container-first**（ObjStm メンバーに出会ったら container を先に enqueue し、container 採番時に member 範囲を即時予約する） | `libqpdf/QPDFWriter.cc:1072-1141` / `libqpdf/QPDFWriter.cc:1057-1069` | D2 mixed（canonical owner = `crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber`）。D3 mixed — `crates/flpdf/src/writer/rewrite_renumber.rs::CanonicalCatalogFirstRenumber` が prod 5 箇所に残り、container のある経路では container-above-max になる | container が member より後ろの番号になり、xref と ObjStm の中身が qpdf と全く別の番号体系になる。`flpdf-hi08` / PR #1486 が捕らえた乖離そのもの |
| **standard の書き込み順は enqueue 順、linearized は専用の2 pass。両者は object emission primitive を共有する** | `libqpdf/QPDFWriter.cc:1761-1809,2537-2904,2991-3044` | D11 mixed — plain、specialized、PCLm、linearized に emission が分散する | qpdf も standard と linearized の制御ループは別であり、4ループを1本にすること自体は完了条件ではない。`writeObject` / `unparseObject` owner と standard/PCLm の enqueue 中 body loop を復元し、consumer ごとに出力順を照合する |
| **classic xref の欠番/type≠1 は通常出力で `std::logic_error`。object 0 と pass-1 `suppress_offsets` は別分岐** | `libqpdf/QPDFWriter.cc:2343-2379` / `libqpdf/QPDFXRefEntry.cc:27-32` | D12 mixed。plain の欠番 `00000 f` と他経路の `65535 f` はどちらも oracle 契約に対応しない | 共有 `writeXRefTable` 相当 primitive では欠番/type≠1を `Error::Internal` に対応付ける。producer に欠番が生じる条件の調査は別であり、primitive 移植を止める前提ではない（D-U3） |
| **encryption dictionary は body 全 object の後・xref の直前に置く**（standard 経路。番号はその時点の `next_objid++`） | `libqpdf/QPDFWriter.cc:3017-3019` / `libqpdf/QPDFWriter.cc:2244-2256` | D15 canonical（`crates/flpdf/src/writer/encrypted_strings.rs::write_encryption_dictionary_handle`）。plain pipeline は暗号化経路を持たない（`crates/flpdf/src/writer/plain/mod.rs:50-62`）ので、暗号化された非 linearized 出力は必ず legacy coordinator を通る | `/Encrypt` の object 番号が body 中に割り込み、以降の全 object 番号と xref offset がずれる |
| **xref / trailer を書く実装は qpdf 全体で 1 組**（standard / pclm / linearized が同じ `writeXRefTable` / `writeXRefStream` / `writeTrailer` を共有する） | `libqpdf/QPDFWriter.cc:2343-2379` / `libqpdf/QPDFWriter.cc:2392-2495` / `libqpdf/QPDFWriter.cc:1160-1236` | D12 / D13 / D14 mixed。table 側は 4 実装、stream 側は 2 実装（legacy coordinator は plain へ委譲済み）。`getTrimmedTrailer` 相当（`crates/flpdf/src/writer.rs::build_writer_trailer_handle`）は既に 1 本 | trailer key の順序・`/Size` の差し替え・`/ID` の扱いが経路ごとに変わる。table と stream で非対称なので、`--object-streams` の指定だけで trailer の書式が変わりうる |
| **`prepareFileForWrite` は `write()` が linearized / standard に分岐する前に 1 度だけ走る** | `libqpdf/QPDFWriter.cc:2036-2056` / `libqpdf/QPDFWriter.cc:2187-2213` | D25 mixed。`emit_canonical_pdf_inner` の先頭と `write_linearized_for_pdf_writer` の中で別々に行い、さらに qpdf に対応物のない 2 重 snapshot / restore（`crates/flpdf/src/writer.rs:2109`、`crates/flpdf/src/writer.rs:2132`）を持つ | `/Root /Extensions /ADBE` の direct 化が linearized と非 linearized で違う結果になり、catalog の内容が route 依存になる |
| **linearize は pass1 → hint 1 回計算 → pass2 で、収束ループは無い**（pass 2 が pass 1 の padding に収まらなければ `std::logic_error` で失敗する設計） | `libqpdf/QPDFWriter.cc:2656-2904` / `libqpdf/QPDFWriter.cc:2864-2884` / `libqpdf/QPDFWriter.cc:2498-2507` | D20 canonical — `write_linearized_impl` に layout pass をまたぐループは無い。ただし収束ループ前提の stale コメントが `crates/flpdf/src/linearization/hint_shared.rs:1081` ほか 4 箇所に残る | D-U6はsourceで確認済み。`SharedObjectHintTable::from_plan` がmember/container mapから最終番号を算出し、writerはlocationを更新する。残るconvergenceコメントはstale docで、番号producer不在のbugではない |
| **ObjStm 候補集合は trailer 起点の LIFO DFS の訪問順で決まる**（dict key は `rbegin()` の逆順 push、array は末尾から push。stream 自身 / `/Sig` / encryption dict は除外、`/Length` edge は辿らない） | `libqpdf/QPDF.cc:2393-2474` | D8 mixed — 入口が 2 つあり、`get_compressible_objgens`（薄い側）を linearized 経路だけが使う。D6 mixed — Preserve batch の導出が 3 実装（plain / legacy coordinator / linearized） | どの object が ObjStm に入るか、container 内の member 順、stale generation の除去が経路ごとに変わる。linearized Preserve は source-index 順を保持する独自導出、plain は compressible 集合との intersection と objgen sort。D31 は mixed と確定した（D-U1） |

### 5.E QPDFJob / CLI / C API

| 不変条件 / 境界 | qpdf の根拠 | flpdf の現状（該当行） | 壊すと何が変わるか |
|---|---|---|---|
| **`run()` は `createQPDF()` → `writeQPDF()` の 2 呼び出しだけ**（この 2 段構成は「QPDF を作ってから書き出す前に改変できるようにするため」に意図的に公開されている） | `libqpdf/QPDFJob.cc:513-520` / `include/qpdf/QPDFJob.hh:371-373` | E-1 / E-2 mixed。flpdf の `run` は `create_qpdf` → `run_document_erased` で `write_qpdf` を呼ばず、qpdf が `createQPDF` の内側に持つ変換 5 段は private の `crates/flpdf/src/job/lifecycle.rs::run_document_stages` に移っている | public 2 段だけを使う consumer が `--pages` / `--rotate` / overlay を指定しても変換が一切走らない。現在の唯一の外部 consumer が変換を要求しない argv しか使わないため未観測。probe E-P1 |
| **「検査するか / 分割するか / 書くか」の判断は `writeQPDF` の内側にあり、判定は `createsOutput()` 1 個** | `libqpdf/QPDFJob.cc:483-511` / `libqpdf/QPDFJob.cc:528-532` | E-3 mixed。3 分岐は `write_qpdf` の中ではなく `run_document_stages` の末尾にあり、しかも「inspection フラグ 10 種の OR → `json_version` → `check` または出力先なし → `write_qpdf`」の 4 段 | 出力指定と inspection フラグを同時に与えたときの優先順位が qpdf と変わり、出力ファイルが作られる／作られないが逆になる |
| **`createQPDF` の変換は固定順序**（`updateFromJSON` → `handlePageSpecs` → `handleRotations` → `handleUnderOverlay` → `handleTransformations`。`addAttachments` / `copyAttachments` は `handleTransformations` の内側） | `libqpdf/QPDFJob.cc:428-481` / `libqpdf/QPDFJob.cc:2242-2247` | E-12 mixed。`run_document_stages` は順序を保つが、CLI はこの経路を通らず `flpdf::optimize_images` / `flpdf::flatten_rotation_on_pages` などを個別に呼ぶ（E-9 / E-11 / E-13 も同型） | `--pages` と `--rotate` と `--overlay` を同時指定したときの適用順が CLI 側では保証されない。ページの回転・overlay の重なりが qpdf と変わる |
| **入力は必ず `doProcessOnce` 経由で開き、`QPDF` 構築直後に `setQPDFOptions`（`noWarn` → `setSuppressWarnings`）を適用してから読む** | `libqpdf/QPDFJob.cc:1695-1716` / `libqpdf/QPDFJob.cc:650-666` / `libqpdf/QPDFJob.cc:663-665` | E-29 mixed。`crates/flpdf/src/job/lifecycle.rs::QPDFJob::open_with_description`、`open_document_with_description`、`open_for_encryption_inspection_with_description`、`open_job_source` は job suppression を open 前に適用済み。CLI の通常入力・overlay/underlay・copy-encryption・encryption probe・attachment copy・page source・JSON input も同じ policy を使用する（reopenable page source は `crates/flpdf-cli/src/main.rs::open_page_source`）。 | `--no-warn` で open-time warning の stderr delivery を抑止し、warning collection と qpdf の exit status は保持する。reopenable source の separate implementation は構造上残るが suppression policy は共通 |
| **CLI 実行ファイルは `QPDFJob` の public surface しか触らない**（`initializeFromArgv` → `run` → `getExitCode` の 3 呼び出し、62 行） | `qpdf/qpdf.cc:26-44` / `libqpdf/qpdfjob-c.cc:19-161` | E-21 mixed（`crates/flpdf-cli/src/main.rs::main` は 9313 行で `run()` は `--job-json-file` の 1 箇所のみ）。E-22 / E-23 canonical — C API 相当の 2 consumer だけが qpdf の構造を正しく踏襲している | `QPDFJob` の private orchestration を直しても CLI の挙動が追随しない（逆も同じ）。argv 解釈の正本が CLI 側と library 側の 2 本になる（E-17） |
| **exit code は状態を溜めて `getExitCode()` で 1 回だけ判定する** | `libqpdf/QPDFJob.cc:522-564` / `libqpdf/QPDFJob.cc:534-564` | E-19 mixed。`complete(creates_output)` を各ステージが個別に呼び、CLI からも 6 箇所呼ぶ。E-7 — inspection の個別 public メソッドはその場で `complete` するが `doInspection` 相当の経路は `*_report`（完了しない）を使う | 複数の inspection フラグを同時指定したときの warning 集計と exit code が qpdf と食い違う。probe E-P3 |
| **`qpdf_check_pdf`（C API）は `doCheck` を呼ばない** — `QPDFWriter` に `Pl_Discard` + `setDecodeLevel(qpdf_dl_all)` を設定して `write()` するだけ | `libqpdf/qpdf-c.cc:224-231` / `libqpdf/qpdf-c.cc:58-66` | E-23 canonical（`crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs` がこの構造を保持）。E-8 の `QPDFJob::check` は別責務 | C API 相当の check が `--check` と同じ診断を出すようになり、qtest の期待出力が変わる |
| **`QPDFJob` の C API は pure pass-through で、例外は `wrap_qpdfjob` 1 箇所で `getMessagePrefix() + ": " + what()` に整形される** | `libqpdf/qpdfjob-c.cc:32-41` / `libqpdf/qpdfjob-c.cc:88-96` | E-22 canonical。ただし flpdf の対応物 `crates/flpdf/src/job/lifecycle.rs::report_job_error` は `QPDFJob` の public メソッドとして置かれており、qpdf では C wrapper 側にある | エラー文言の prefix が C API 経由と library 直呼びで変わる（qpdf は C wrapper 経由のときだけ prefix が付く） |

## 6. 二重正本トラッカー

追跡対象の symbol manifest は [tracked-symbols.txt](tracked-symbols.txt)。現行の bridge / mixed は97行だが、
manifest は行集合の完全な機械変換ではなく、削除済みsymbolの0確認とcanonical側の分母も保持する。
B7 / C44 の未実装ownerには追跡すべきRust symbolがなく、B29は既存 `num_warnings` と未実装drainを区別する。
symbol数はmanifestの非comment・非空行から数え、行数と同一視しない。
数え方の正本は `scripts/qpdf-route-callers.py` の module docstring と実装である。

### 6.1 manifest は 2 群に分かれている（混ぜて読まないこと）

| 群 | 中身 | `--expect-zero` |
|---|---|---|
| **(a) deletable route** | bridge 行の entrypoint と、mixed 行のうち canonical owner に吸収されるべき側の経路。canonical owner が全 consumer に行き渡れば production caller が 0 になる | **意味を持つ。完了判定に使う** |
| **(b) baseline denominator** | mixed 行の canonical 側 entrypoint（A1 の `ResolverCore`、D2 の `ObjectStreamRenumber` のように、entrypoint と canonical owner が同じ行）、複数行が共有する primitive、および leaf が総称的で他 symbol と衝突するもの | **当ててはいけない。** 0 になることは想定されていない。cutover 前後で数が減ったか変わらなかったかを読むための分母 |

(b) に落ちる代表例が B32 の `Error`（同名参照が広い）と D1 の `write`で、この 2 行の
mixed は「1 つの flpdf 経路が 2 つ以上の qpdf 責務を畳んでいる」側の mixed（§3）であり、
削除できる bridge ではない。0 を期待する対象ではない。

### 6.2 計測（2026-09-06、`flpdf-3yn9.48` 再監査tree）

`python3 scripts/qpdf-route-callers.py --root /home/ubuntu/flpdf/.worktrees/flpdf-3yn9-48-route-audit`
を再実行した出力。leafの同名衝突や型・束縛参照を含むため、production callerを特定するときはreceiverも確認する。
canonical test scaffoldingのD19/D30や削除済みsymbolも履歴追跡のためmanifestに残る。

```
crates/flpdf/src/cache.rs::ObjectCache: prod 3 (2 files) / test 4
    crates/flpdf/src/engine.rs 2, crates/flpdf/src/pdf.rs 1
crates/flpdf/src/cache.rs::CacheEntry: prod 43 (2 files) / test 13
    crates/flpdf/src/cache.rs 31, crates/flpdf/src/reader.rs 12
crates/flpdf/src/object_handle.rs::ObjectHandle::as_dictionary: prod 179 (43 files) / test 207
    crates/flpdf/src/acroform_document_helper.rs 14, crates/flpdf/src/object_handle.rs 13, crates/flpdf/src/page_annotation_flatten.rs 13, crates/flpdf/src/form_field_object_helper.rs 12, crates/flpdf/src/page_object_helper.rs 10, crates/flpdf/src/page_splice.rs 9, crates/flpdf/src/signatures.rs 9, crates/flpdf/src/optimization/inherited_attrs.rs 7, crates/flpdf/src/xref.rs 7, crates/flpdf-qtest-tools/src/driver/test_34_41.rs 6, crates/flpdf-qtest-tools/src/compare.rs 5, crates/flpdf-qtest-tools/src/driver/handle.rs 5, crates/flpdf-qtest-tools/src/driver/test_88_98.rs 5, crates/flpdf/src/job/json_sections.rs 5, crates/flpdf/src/resources.rs 5, crates/flpdf/src/form_field_object_helper/rendering.rs 4, crates/flpdf/src/pages.rs 4, crates/flpdf/src/writer/plain/body.rs 4, crates/flpdf/src/job/acroform_field_prune.rs 3, crates/flpdf/src/object_copy.rs 3, crates/flpdf/src/overlay_appearance_stream.rs 3, crates/flpdf/src/reader.rs 3, crates/flpdf/src/reader/file_object.rs 3, crates/flpdf/src/thread_bead_p.rs 3, crates/flpdf-qtest-tools/src/renumber.rs 2, crates/flpdf/src/annotation_object_helper.rs 2, crates/flpdf/src/filespec_helper/embedded_file_stream.rs 2, crates/flpdf/src/job/resource_pruning.rs 2, crates/flpdf/src/page_document_helper.rs 2, crates/flpdf-qtest-tools/src/clean.rs 1, crates/flpdf-qtest-tools/src/driver/test_0_1.rs 1, crates/flpdf-qtest-tools/src/driver/test_18_25.rs 1, crates/flpdf-qtest-tools/src/driver/test_42_49.rs 1, crates/flpdf-qtest-tools/src/driver/test_80_87.rs 1, crates/flpdf-qtest-tools/src/metadata.rs 1, crates/flpdf/src/document_json.rs 1, crates/flpdf/src/filespec_helper/filespec.rs 1, crates/flpdf/src/job/page_specs.rs 1, crates/flpdf/src/job/rotate.rs 1, crates/flpdf/src/json/input.rs 1, crates/flpdf/src/pages/tree_rebuild.rs 1, crates/flpdf/src/pdf.rs 1, crates/flpdf/src/writer/encrypted_strings.rs 1
crates/flpdf/src/object_handle.rs::ObjectHandle::as_array: prod 127 (41 files) / test 201
    crates/flpdf/src/object_handle.rs 8, crates/flpdf/src/page_object_helper.rs 8, crates/flpdf-qtest-tools/src/compare.rs 7, crates/flpdf-qtest-tools/src/driver/handle.rs 7, crates/flpdf-qtest-tools/src/driver/test_18_25.rs 7, crates/flpdf/src/acroform_document_helper.rs 7, crates/flpdf/src/page_annotation_flatten.rs 6, crates/flpdf/src/writer/plain/body.rs 6, crates/flpdf-qtest-tools/src/driver/test_02_09.rs 5, crates/flpdf/src/job/json_sections.rs 5, crates/flpdf-qtest-tools/src/driver/test_72_79.rs 4, crates/flpdf/src/form_field_object_helper.rs 4, crates/flpdf/src/page_splice.rs 4, crates/flpdf/src/signatures.rs 4, crates/flpdf-qtest-tools/src/driver/test_34_41.rs 3, crates/flpdf-qtest-tools/src/driver/test_50_55.rs 3, crates/flpdf-qtest-tools/src/driver/test_88_98.rs 3, crates/flpdf/src/annotation_object_helper.rs 3, crates/flpdf/src/job/acroform_field_prune.rs 3, crates/flpdf-qtest-tools/src/renumber.rs 2, crates/flpdf-qtest-tools/src/tokenizer_runner.rs 2, crates/flpdf/src/linearization/check.rs 2, crates/flpdf/src/linearization/writer.rs 2, crates/flpdf/src/object_copy.rs 2, crates/flpdf/src/optimization/inherited_attrs.rs 2, crates/flpdf/src/reader.rs 2, crates/flpdf/src/thread_bead_p.rs 2, crates/flpdf-cli/src/main.rs 1, crates/flpdf-qtest-tools/src/clean.rs 1, crates/flpdf-qtest-tools/src/driver/test_0_1.rs 1, crates/flpdf-qtest-tools/src/driver/test_10_17.rs 1, crates/flpdf-qtest-tools/src/driver/test_42_49.rs 1, crates/flpdf-qtest-tools/src/metadata.rs 1, crates/flpdf/src/form_field_object_helper/rendering.rs 1, crates/flpdf/src/job/inspection.rs 1, crates/flpdf/src/linearization/show.rs 1, crates/flpdf/src/outline_object_helper.rs 1, crates/flpdf/src/pages.rs 1, crates/flpdf/src/pages/tree_rebuild.rs 1, crates/flpdf/src/writer.rs 1, crates/flpdf/src/xref.rs 1
crates/flpdf/src/object_handle.rs::ObjectHandle::as_integer: prod 73 (28 files) / test 267
    crates/flpdf/src/linearization/check.rs 8, crates/flpdf/src/page_object_helper.rs 6, crates/flpdf-qtest-tools/src/driver/test_10_17.rs 5, crates/flpdf-qtest-tools/src/driver/test_80_87.rs 5, crates/flpdf/src/acroform_document_helper.rs 5, crates/flpdf-qtest-tools/src/driver/test_72_79.rs 4, crates/flpdf/src/job/json_sections.rs 4, crates/flpdf/src/object_handle.rs 4, crates/flpdf/src/form_field_object_helper.rs 3, crates/flpdf/src/linearization/show.rs 3, crates/flpdf/src/page_splice.rs 3, crates/flpdf-qtest-tools/src/driver/test_02_09.rs 2, crates/flpdf-qtest-tools/src/driver/test_18_25.rs 2, crates/flpdf-qtest-tools/src/renumber.rs 2, crates/flpdf/src/annotation_object_helper.rs 2, crates/flpdf/src/form_field_object_helper/rendering.rs 2, crates/flpdf/src/signatures.rs 2, crates/flpdf-qtest-tools/src/driver/test_0_1.rs 1, crates/flpdf-qtest-tools/src/driver/test_26_33.rs 1, crates/flpdf-qtest-tools/src/driver/test_34_41.rs 1, crates/flpdf-qtest-tools/src/driver/test_42_49.rs 1, crates/flpdf/src/default_appearance.rs 1, crates/flpdf/src/filespec_helper/embedded_file_stream.rs 1, crates/flpdf/src/nntree.rs 1, crates/flpdf/src/page_annotation_flatten.rs 1, crates/flpdf/src/reader.rs 1, crates/flpdf/src/reader/resolver.rs 1, crates/flpdf/src/stream_filter.rs 1
crates/flpdf/src/object_handle.rs::ObjectHandle::as_name: prod 58 (29 files) / test 125
    crates/flpdf-qtest-tools/src/compare.rs 5, crates/flpdf/src/page_object_helper.rs 5, crates/flpdf-qtest-tools/src/driver/handle.rs 4, crates/flpdf/src/form_field_object_helper.rs 4, crates/flpdf/src/object_handle.rs 4, crates/flpdf/src/linearization/check.rs 3, crates/flpdf-qtest-tools/src/driver/test_34_41.rs 2, crates/flpdf-qtest-tools/src/renumber.rs 2, crates/flpdf/src/annotation_object_helper.rs 2, crates/flpdf/src/job/inspection.rs 2, crates/flpdf/src/job/json_sections.rs 2, crates/flpdf/src/json/input.rs 2, crates/flpdf/src/page_splice.rs 2, crates/flpdf/src/pages.rs 2, crates/flpdf/src/parser.rs 2, crates/flpdf/src/signatures.rs 2, crates/flpdf-qtest-tools/src/driver/test_0_1.rs 1, crates/flpdf-qtest-tools/src/driver/test_72_79.rs 1, crates/flpdf-qtest-tools/src/driver/test_80_87.rs 1, crates/flpdf-qtest-tools/src/tokenizer_runner.rs 1, crates/flpdf/src/acroform_document_helper.rs 1, crates/flpdf/src/default_appearance.rs 1, crates/flpdf/src/filespec_helper/embedded_file_stream.rs 1, crates/flpdf/src/filespec_helper/filespec.rs 1, crates/flpdf/src/form_field_object_helper/rendering.rs 1, crates/flpdf/src/job/acroform_field_prune.rs 1, crates/flpdf/src/job/image_optimization.rs 1, crates/flpdf/src/optimization/inherited_attrs.rs 1, crates/flpdf/src/resource_finder.rs 1
crates/flpdf/src/object_handle.rs::ObjectHandle::as_string: prod 62 (27 files) / test 95
    crates/flpdf/src/acroform_document_helper.rs 7, crates/flpdf-qtest-tools/src/driver/test_88_98.rs 4, crates/flpdf/src/encryption/state.rs 4, crates/flpdf/src/linearization/writer.rs 4, crates/flpdf/src/signatures.rs 4, crates/flpdf/src/writer.rs 4, crates/flpdf-qtest-tools/src/driver/test_02_09.rs 3, crates/flpdf-qtest-tools/src/driver/test_34_41.rs 3, crates/flpdf/src/filespec_helper/embedded_file_stream.rs 3, crates/flpdf/src/object_handle.rs 3, crates/flpdf-qtest-tools/src/driver/test_42_49.rs 2, crates/flpdf-qtest-tools/src/driver/test_50_55.rs 2, crates/flpdf-qtest-tools/src/renumber.rs 2, crates/flpdf/src/form_field_object_helper.rs 2, crates/flpdf/src/outline_object_helper.rs 2, crates/flpdf/src/writer/plain/xref.rs 2, crates/flpdf-qtest-tools/src/driver/test_0_1.rs 1, crates/flpdf-qtest-tools/src/driver/test_72_79.rs 1, crates/flpdf-qtest-tools/src/driver/test_80_87.rs 1, crates/flpdf/src/filespec_helper/filespec.rs 1, crates/flpdf/src/job/page_merge.rs 1, crates/flpdf/src/nntree.rs 1, crates/flpdf/src/outline_document_helper.rs 1, crates/flpdf/src/page_label_document_helper.rs 1, crates/flpdf/src/parser.rs 1, crates/flpdf/src/writer/encrypted_strings.rs 1, crates/flpdf/src/writer/plain/plan.rs 1
crates/flpdf/src/object_handle.rs::ObjectHandle::as_real: prod 19 (11 files) / test 14
    crates/flpdf/src/page_object_helper.rs 5, crates/flpdf-qtest-tools/src/renumber.rs 2, crates/flpdf/src/acroform_document_helper.rs 2, crates/flpdf/src/form_field_object_helper/rendering.rs 2, crates/flpdf/src/object_handle.rs 2, crates/flpdf-qtest-tools/src/driver/test_02_09.rs 1, crates/flpdf-qtest-tools/src/driver/test_80_87.rs 1, crates/flpdf/src/annotation_object_helper.rs 1, crates/flpdf/src/default_appearance.rs 1, crates/flpdf/src/linearization/check.rs 1, crates/flpdf/src/pages/repair.rs 1
crates/flpdf/src/object_handle.rs::ObjectHandle::is_null: prod 167 (53 files) / test 218
    crates/flpdf/src/page_object_helper.rs 16, crates/flpdf/src/form_field_object_helper.rs 13, crates/flpdf/src/acroform_document_helper.rs 12, crates/flpdf-qtest-tools/src/driver/handle.rs 11, crates/flpdf/src/object_handle.rs 9, crates/flpdf-qtest-tools/src/driver/test_42_49.rs 6, crates/flpdf/src/job/page_merge.rs 6, crates/flpdf/src/resources.rs 6, crates/flpdf-qtest-tools/src/driver/test_88_98.rs 5, crates/flpdf/src/encryption/state.rs 5, crates/flpdf/src/page_annotation_flatten.rs 5, crates/flpdf/src/signatures.rs 5, crates/flpdf/src/writer/rewrite_renumber.rs 5, crates/flpdf/src/reader.rs 4, crates/flpdf-qtest-tools/src/driver/mod.rs 3, crates/flpdf-qtest-tools/src/driver/test_26_33.rs 3, crates/flpdf/src/linearization/check.rs 3, crates/flpdf/src/memory_usage.rs 3, crates/flpdf/src/pages.rs 3, crates/flpdf-libjpeg-compat/src/ffi.rs 2, crates/flpdf-qtest-tools/src/driver/test_0_1.rs 2, crates/flpdf/src/filespec_helper/filespec.rs 2, crates/flpdf/src/form_field_object_helper/rendering.rs 2, crates/flpdf/src/optimization/inherited_attrs.rs 2, crates/flpdf/src/pages/repair.rs 2, crates/flpdf/src/pdf.rs 2, crates/flpdf/src/reader/resolver.rs 2, crates/flpdf/src/writer/pclm.rs 2, crates/flpdf/src/xref.rs 2, crates/flpdf-qtest-tools/src/character_encoding.rs 1, crates/flpdf-qtest-tools/src/driver/test_02_09.rs 1, crates/flpdf-qtest-tools/src/driver/test_18_25.rs 1, crates/flpdf-qtest-tools/src/driver/test_34_41.rs 1, crates/flpdf-qtest-tools/src/driver/test_80_87.rs 1, crates/flpdf-qtest-tools/src/metadata.rs 1, crates/flpdf-qtest-tools/src/tokenizer_runner.rs 1, crates/flpdf/src/filespec_helper/embedded_file_stream.rs 1, crates/flpdf/src/job/acroform_field_prune.rs 1, crates/flpdf/src/job/attachment_list.rs 1, crates/flpdf/src/job/inspection.rs 1, crates/flpdf/src/job/overlay.rs 1, crates/flpdf/src/job/resource_pruning.rs 1, crates/flpdf/src/json/handler.rs 1, crates/flpdf/src/json/input.rs 1, crates/flpdf/src/linearization/show.rs 1, crates/flpdf/src/nntree.rs 1, crates/flpdf/src/object_copy.rs 1, crates/flpdf/src/page_document_helper.rs 1, crates/flpdf/src/pages/tree_rebuild.rs 1, crates/flpdf/src/parser.rs 1, crates/flpdf/src/qpdf_time.rs 1, crates/flpdf/src/writer.rs 1, crates/flpdf/src/writer/plain/plan.rs 1
crates/flpdf/src/reader.rs::Pdf::resolve: prod 254 (57 files) / test 469
    crates/flpdf/src/page_object_helper.rs 21, crates/flpdf/src/page_splice.rs 16, crates/flpdf/src/job/json_sections.rs 15, crates/flpdf-qtest-tools/src/driver/test_42_49.rs 13, crates/flpdf-qtest-tools/src/driver/handle.rs 12, crates/flpdf/src/job/acroform_field_prune.rs 12, crates/flpdf-qtest-tools/src/compare.rs 11, crates/flpdf/src/page_annotation_flatten.rs 10, crates/flpdf/src/annotation_object_helper.rs 9, crates/flpdf-qtest-tools/src/driver/test_88_98.rs 7, crates/flpdf/src/linearization/plan.rs 7, crates/flpdf/src/writer.rs 7, crates/flpdf/src/writer/rewrite_renumber.rs 7, crates/flpdf/src/reader.rs 6, crates/flpdf-qtest-tools/src/driver/test_64_71.rs 5, crates/flpdf-qtest-tools/src/tokenizer_runner.rs 5, crates/flpdf/src/linearization/writer.rs 5, crates/flpdf/src/pages.rs 5, crates/flpdf-cli/src/main.rs 4, crates/flpdf-qtest-tools/src/driver/test_18_25.rs 4, crates/flpdf-qtest-tools/src/driver/test_50_55.rs 4, crates/flpdf/src/form_field_object_helper.rs 4, crates/flpdf/src/form_field_object_helper/rendering.rs 4, crates/flpdf/src/job/overlay.rs 4, crates/flpdf/src/json/handler.rs 4, crates/flpdf/src/pdf.rs 4, crates/flpdf/src/writer/plain/body.rs 4, crates/flpdf/src/optimization/inherited_attrs.rs 3, crates/flpdf-qtest-tools/src/clean.rs 2, crates/flpdf-qtest-tools/src/driver/test_0_1.rs 2, crates/flpdf-qtest-tools/src/driver/test_80_87.rs 2, crates/flpdf/src/embedded_files.rs 2, crates/flpdf/src/filespec_helper/embedded_file_stream.rs 2, crates/flpdf/src/job/outline_dest_remap.rs 2, crates/flpdf/src/job/page_merge.rs 2, crates/flpdf/src/optimization.rs 2, crates/flpdf/src/outline_document_helper.rs 2, crates/flpdf/src/page_extract.rs 2, crates/flpdf/src/resources.rs 2, crates/flpdf/src/thread_bead_p.rs 2, crates/flpdf/src/writer/pclm.rs 2, crates/flpdf-qtest-tools/src/driver/test_02_09.rs 1, crates/flpdf-qtest-tools/src/driver/test_26_33.rs 1, crates/flpdf-qtest-tools/src/driver/test_34_41.rs 1, crates/flpdf-qtest-tools/src/driver/test_72_79.rs 1, crates/flpdf/src/acroform_document_helper.rs 1, crates/flpdf/src/document_json.rs 1, crates/flpdf/src/filespec_helper/filespec.rs 1, crates/flpdf/src/job/attachment_list.rs 1, crates/flpdf/src/job/attachments.rs 1, crates/flpdf/src/job/lifecycle.rs 1, crates/flpdf/src/job/page_plan.rs 1, crates/flpdf/src/job/rotate.rs 1, crates/flpdf/src/objr_obj_annot_p.rs 1, crates/flpdf/src/page_document_helper.rs 1, crates/flpdf/src/signatures.rs 1, crates/flpdf/src/struct_tree_pg.rs 1
crates/flpdf/src/reader.rs::Pdf::resolve_handle: prod 160 (24 files) / test 7
    crates/flpdf/src/acroform_document_helper.rs 47, crates/flpdf-qtest-tools/src/driver/test_02_09.rs 16, crates/flpdf/src/job/page_merge.rs 15, crates/flpdf/src/signatures.rs 14, crates/flpdf/src/page_annotation_flatten.rs 12, crates/flpdf/src/page_object_helper.rs 10, crates/flpdf/src/job/resource_pruning.rs 6, crates/flpdf/src/page_label_document_helper.rs 6, crates/flpdf/src/filespec_helper/filespec.rs 5, crates/flpdf/src/embedded_files.rs 3, crates/flpdf/src/job/page_specs.rs 3, crates/flpdf/src/outline_document_helper.rs 3, crates/flpdf/src/outline_object_helper.rs 3, crates/flpdf/src/resources.rs 3, crates/flpdf-qtest-tools/src/driver/handle.rs 2, crates/flpdf-qtest-tools/src/driver/test_64_71.rs 2, crates/flpdf/src/document_json.rs 2, crates/flpdf/src/optimization/inherited_attrs.rs 2, crates/flpdf-qtest-tools/src/driver/test_0_1.rs 1, crates/flpdf/src/filespec_helper/embedded_file_stream.rs 1, crates/flpdf/src/overlay_appearance_stream.rs 1, crates/flpdf/src/pages.rs 1, crates/flpdf/src/pages/repair.rs 1, crates/flpdf/src/writer/object_streams/eligibility.rs 1
crates/flpdf/src/reader.rs::Pdf::resolve_handle_ref: prod 14 (4 files) / test 0
    crates/flpdf/src/thread_bead_p.rs 6, crates/flpdf/src/job/page_merge.rs 4, crates/flpdf/src/filespec_helper/embedded_file_stream.rs 2, crates/flpdf/src/filespec_helper/filespec.rs 2
crates/flpdf/src/reader.rs::Pdf::resolve_qpdf_json_handle: prod 1 (1 files) / test 0
    crates/flpdf/src/json_inspect.rs 1
crates/flpdf/src/object_handle.rs::ObjectHandle::get_key: prod 142 (28 files) / test 460
    crates/flpdf/src/form_field_object_helper.rs 19, crates/flpdf-qtest-tools/src/driver/test_10_17.rs 17, crates/flpdf-qtest-tools/src/driver/test_56_63.rs 16, crates/flpdf-qtest-tools/src/driver/test_50_55.rs 8, crates/flpdf/src/form_field_object_helper/rendering.rs 8, crates/flpdf-qtest-tools/src/driver/test_18_25.rs 7, crates/flpdf-qtest-tools/src/driver/handle.rs 5, crates/flpdf-qtest-tools/src/driver/test_34_41.rs 5, crates/flpdf/src/annotation_object_helper.rs 5, crates/flpdf/src/linearization/writer.rs 5, crates/flpdf-qtest-tools/src/driver/test_02_09.rs 4, crates/flpdf-qtest-tools/src/driver/test_26_33.rs 4, crates/flpdf-qtest-tools/src/driver/test_42_49.rs 4, crates/flpdf-qtest-tools/src/driver/test_64_71.rs 4, crates/flpdf-qtest-tools/src/driver/test_88_98.rs 4, crates/flpdf-qtest-tools/src/large_file.rs 4, crates/flpdf/src/page_object_helper.rs 4, crates/flpdf-qtest-tools/src/compare.rs 3, crates/flpdf/src/optimization/inherited_attrs.rs 3, crates/flpdf-qtest-tools/src/clean.rs 2, crates/flpdf-qtest-tools/src/tokenizer_runner.rs 2, crates/flpdf/src/filespec_helper/embedded_file_stream.rs 2, crates/flpdf/src/resources.rs 2, crates/flpdf-cli/src/main.rs 1, crates/flpdf-qtest-tools/src/driver/test_72_79.rs 1, crates/flpdf/src/embedded_files.rs 1, crates/flpdf/src/job/inspection.rs 1, crates/flpdf/src/object_handle.rs 1
crates/flpdf/src/object_handle.rs::ObjectHandle::has_key: prod 8 (6 files) / test 100
    crates/flpdf-qtest-tools/src/clean.rs 2, crates/flpdf/src/page_object_helper.rs 2, crates/flpdf-qtest-tools/src/tokenizer_runner.rs 1, crates/flpdf/src/embedded_files.rs 1, crates/flpdf/src/optimization/inherited_attrs.rs 1, crates/flpdf/src/pages.rs 1
crates/flpdf/src/reader.rs::Pdf::get_all_objects: prod 8 (6 files) / test 9
    crates/flpdf/src/document_json.rs 3, crates/flpdf-qtest-tools/src/driver/test_50_55.rs 1, crates/flpdf-qtest-tools/src/metadata.rs 1, crates/flpdf-qtest-tools/src/renumber.rs 1, crates/flpdf/src/reader.rs 1, crates/flpdf/src/writer/rewrite_renumber.rs 1
crates/flpdf/src/reader.rs::Pdf::object_refs: prod 12 (4 files) / test 44
    crates/flpdf/src/linearization/plan.rs 4, crates/flpdf/src/reader.rs 4, crates/flpdf-qtest-tools/src/tokenizer_runner.rs 3, crates/flpdf/src/nntree.rs 1
crates/flpdf/src/reader.rs::Pdf::live_object_refs: prod 7 (5 files) / test 24
    crates/flpdf-qtest-tools/src/orchestrator.rs 2, crates/flpdf/src/linearization/plan.rs 2, crates/flpdf/src/job/page_merge.rs 1, crates/flpdf/src/writer/object_streams/eligibility.rs 1, crates/flpdf/src/writer/rewrite_renumber.rs 1
crates/flpdf/src/reader.rs::Pdf::resolved_count: prod 1 (1 files) / test 1
    crates/flpdf/src/reader.rs 1
crates/flpdf/src/reader.rs::Pdf::next_available_object_ref: prod 2 (2 files) / test 2
    crates/flpdf/src/page_annotation_flatten.rs 1, crates/flpdf/src/reader.rs 1
crates/flpdf/src/reader.rs::Pdf::make_indirect_object_handle: prod 31 (14 files) / test 56
    crates/flpdf/src/acroform_document_helper.rs 6, crates/flpdf-qtest-tools/src/driver/test_10_17.rs 5, crates/flpdf/src/page_splice.rs 4, crates/flpdf-qtest-tools/src/large_file.rs 3, crates/flpdf/src/filespec_helper/filespec.rs 2, crates/flpdf/src/page_document_helper.rs 2, crates/flpdf/src/page_extract.rs 2, crates/flpdf-qtest-tools/src/driver/test_50_55.rs 1, crates/flpdf-qtest-tools/src/driver/test_56_63.rs 1, crates/flpdf-qtest-tools/src/driver/test_72_79.rs 1, crates/flpdf/src/form_field_object_helper/rendering.rs 1, crates/flpdf/src/job/acroform_field_prune.rs 1, crates/flpdf/src/page_annotation_flatten.rs 1, crates/flpdf/src/page_object_helper.rs 1
crates/flpdf/src/reader.rs::Pdf::synchronize_cache_with_resolver_xref: prod 6 (1 files) / test 2
    crates/flpdf/src/reader.rs 6
crates/flpdf/src/pdf.rs::legacy_resolution_state_synced: prod 4 (2 files) / test 0
    crates/flpdf/src/engine.rs 2, crates/flpdf/src/reader.rs 2
crates/flpdf/src/reader.rs::Pdf::mark_object_handle_dirty: prod 179 (49 files) / test 69
    crates/flpdf/src/acroform_document_helper.rs 29, crates/flpdf/src/page_object_helper.rs 16, crates/flpdf/src/page_annotation_flatten.rs 14, crates/flpdf/src/job/page_merge.rs 7, crates/flpdf/src/page_splice.rs 7, crates/flpdf/src/pages/tree_rebuild.rs 7, crates/flpdf/src/resources.rs 7, crates/flpdf/src/form_field_object_helper/rendering.rs 6, crates/flpdf/src/job/page_specs.rs 6, crates/flpdf-qtest-tools/src/driver/test_02_09.rs 5, crates/flpdf/src/filespec_helper/embedded_file_stream.rs 5, crates/flpdf/src/job/acroform_field_prune.rs 5, crates/flpdf/src/pages/repair.rs 5, crates/flpdf-qtest-tools/src/driver/test_50_55.rs 4, crates/flpdf/src/overlay_appearance_stream.rs 4, crates/flpdf/src/embedded_files.rs 3, crates/flpdf/src/filespec_helper/filespec.rs 3, crates/flpdf-qtest-tools/src/clean.rs 2, crates/flpdf-qtest-tools/src/document_construction.rs 2, crates/flpdf-qtest-tools/src/driver/test_26_33.rs 2, crates/flpdf/src/form_field_object_helper.rs 2, crates/flpdf/src/job/image_optimization.rs 2, crates/flpdf/src/job/outline_dest_remap.rs 2, crates/flpdf/src/nntree.rs 2, crates/flpdf/src/objr_obj_annot_p.rs 2, crates/flpdf/src/optimization/inherited_attrs.rs 2, crates/flpdf/src/page_extract.rs 2, crates/flpdf/src/page_label_document_helper.rs 2, crates/flpdf/src/reader.rs 2, crates/flpdf/src/signatures.rs 2, crates/flpdf/src/thread_bead_p.rs 2, crates/flpdf-cli/src/main.rs 1, crates/flpdf-qtest-tools/src/driver/test_56_63.rs 1, crates/flpdf-qtest-tools/src/driver/test_72_79.rs 1, crates/flpdf-qtest-tools/src/driver/test_80_87.rs 1, crates/flpdf-qtest-tools/src/large_file.rs 1, crates/flpdf/src/annotation_object_helper.rs 1, crates/flpdf/src/job/attachments.rs 1, crates/flpdf/src/job/lifecycle.rs 1, crates/flpdf/src/job/overlay.rs 1, crates/flpdf/src/job/rotate.rs 1, crates/flpdf/src/json/input.rs 1, crates/flpdf/src/linearization/writer.rs 1, crates/flpdf/src/object_handle.rs 1, crates/flpdf/src/optimization.rs 1, crates/flpdf/src/page_document_helper.rs 1, crates/flpdf/src/pdf.rs 1, crates/flpdf/src/struct_tree_pg.rs 1, crates/flpdf/src/writer.rs 1
crates/flpdf/src/reader.rs::Pdf::mark_object_dirty: prod 6 (3 files) / test 2
    crates/flpdf/src/object_copy.rs 3, crates/flpdf/src/page_splice.rs 2, crates/flpdf/src/reader.rs 1
crates/flpdf/src/reader.rs::Pdf::mark_object_handle_mutated: prod 8 (2 files) / test 1
    crates/flpdf/src/reader.rs 5, crates/flpdf/src/filespec_helper/embedded_file_stream.rs 3
crates/flpdf/src/pdf.rs::dirty_object_refs: prod 5 (2 files) / test 8
    crates/flpdf/src/reader.rs 3, crates/flpdf/src/engine.rs 2
crates/flpdf/src/pdf.rs::handle_mutated_object_refs: prod 4 (2 files) / test 0
    crates/flpdf/src/engine.rs 2, crates/flpdf/src/reader.rs 2
crates/flpdf/src/xref.rs::BootstrapCache: prod 2 (1 files) / test 2
    crates/flpdf/src/xref.rs 2
crates/flpdf/src/reader/resolver.rs::read_window: prod 5 (1 files) / test 0
    crates/flpdf/src/reader.rs 5
crates/flpdf/src/object_handle.rs::legacy_dictionary_key: prod 6 (3 files) / test 0
    crates/flpdf/src/stream_filter.rs 3, crates/flpdf/src/parser.rs 2, crates/flpdf/src/writer/object.rs 1
crates/flpdf/src/pdf.rs::compressed_member_parents: prod 3 (2 files) / test 7
    crates/flpdf/src/engine.rs 2, crates/flpdf/src/reader.rs 1
crates/flpdf/src/reader/resolver.rs::ResolverCore: prod 5 (1 files) / test 0
    crates/flpdf/src/reader/resolver.rs 5
crates/flpdf/src/reader/resolver.rs::object_cache: prod 10 (1 files) / test 1
    crates/flpdf/src/reader/resolver.rs 10
crates/flpdf/src/object_handle.rs::ObjectHandle::try_as_integer: prod 28 (11 files) / test 6
    crates/flpdf/src/writer.rs 7, crates/flpdf/src/xref.rs 5, crates/flpdf/src/page_label_document_helper.rs 4, crates/flpdf/src/encryption/state.rs 2, crates/flpdf/src/object_handle.rs 2, crates/flpdf/src/page_object_helper.rs 2, crates/flpdf/src/reader/file_object.rs 2, crates/flpdf/src/pages/repair.rs 1, crates/flpdf/src/pdf.rs 1, crates/flpdf/src/reader/resolver.rs 1, crates/flpdf/src/stream_filter.rs 1
crates/flpdf/src/object_handle.rs::ObjectHandle::try_get_key: prod 351 (59 files) / test 245
    crates/flpdf/src/acroform_document_helper.rs 47, crates/flpdf/src/writer.rs 24, crates/flpdf/src/page_object_helper.rs 20, crates/flpdf/src/linearization/check.rs 19, crates/flpdf/src/object_handle.rs 18, crates/flpdf/src/page_label_document_helper.rs 13, crates/flpdf/src/page_annotation_flatten.rs 11, crates/flpdf/src/encryption/state.rs 10, crates/flpdf/src/job/page_merge.rs 10, crates/flpdf/src/linearization/writer.rs 9, crates/flpdf-qtest-tools/src/driver/test_42_49.rs 7, crates/flpdf/src/job/acroform_field_prune.rs 7, crates/flpdf/src/outline_document_helper.rs 7, crates/flpdf/src/page_splice.rs 7, crates/flpdf/src/pdf.rs 7, crates/flpdf/src/reader/resolver.rs 7, crates/flpdf/src/filters.rs 6, crates/flpdf/src/linearization/plan.rs 6, crates/flpdf/src/overlay_appearance_stream.rs 6, crates/flpdf/src/pages.rs 6, crates/flpdf/src/job/image_optimization.rs 5, crates/flpdf/src/linearization/show.rs 5, crates/flpdf/src/optimization.rs 5, crates/flpdf/src/pages/repair.rs 5, crates/flpdf/src/signatures.rs 5, crates/flpdf/src/writer/object_streams/eligibility.rs 5, crates/flpdf/src/writer/plain/plan.rs 5, crates/flpdf/src/xref.rs 5, crates/flpdf/src/job/inspection.rs 4, crates/flpdf/src/outline_object_helper.rs 4, crates/flpdf/src/pages/tree_rebuild.rs 4, crates/flpdf/src/resources.rs 4, crates/flpdf/src/writer/pclm.rs 4, crates/flpdf-qtest-tools/src/driver/test_64_71.rs 3, crates/flpdf/src/filespec_helper/filespec.rs 3, crates/flpdf/src/job/page_specs.rs 3, crates/flpdf/src/job/resource_pruning.rs 3, crates/flpdf-qtest-tools/src/document_construction.rs 2, crates/flpdf-qtest-tools/src/driver/test_02_09.rs 2, crates/flpdf-qtest-tools/src/driver/test_88_98.rs 2, crates/flpdf-qtest-tools/src/renumber.rs 2, crates/flpdf/src/embedded_files.rs 2, crates/flpdf/src/encryption/crypt_filters.rs 2, crates/flpdf/src/job/outline_dest_remap.rs 2, crates/flpdf/src/reader.rs 2, crates/flpdf/src/thread_bead_p.rs 2, crates/flpdf/src/writer/plain/body.rs 2, crates/flpdf-qtest-tools/src/driver/test_72_79.rs 1, crates/flpdf-qtest-tools/src/driver/test_80_87.rs 1, crates/flpdf/src/form_field_object_helper.rs 1, crates/flpdf/src/job/attachments.rs 1, crates/flpdf/src/job/json_sections.rs 1, crates/flpdf/src/job/rotate.rs 1, crates/flpdf/src/nntree.rs 1, crates/flpdf/src/page_document_helper.rs 1, crates/flpdf/src/page_extract.rs 1, crates/flpdf/src/stream_filter.rs 1, crates/flpdf/src/struct_tree_pg.rs 1, crates/flpdf/src/writer/rewrite_renumber.rs 1
crates/flpdf/src/reader/resolver.rs::get_object_count: prod 4 (4 files) / test 9
    crates/flpdf/src/document_json.rs 1, crates/flpdf/src/reader.rs 1, crates/flpdf/src/reader/resolver.rs 1, crates/flpdf/src/writer.rs 1
crates/flpdf/src/reader/resolver.rs::next_obj_gen: prod 4 (3 files) / test 2
    crates/flpdf/src/reader/resolver.rs 2, crates/flpdf/src/nntree.rs 1, crates/flpdf/src/reader.rs 1
crates/flpdf/src/reader/resolver.rs::make_indirect_from_object_handle: prod 23 (14 files) / test 20
    crates/flpdf-qtest-tools/src/document_construction.rs 5, crates/flpdf/src/nntree.rs 5, crates/flpdf/src/pages/tree_rebuild.rs 2, crates/flpdf-qtest-tools/src/driver/test_02_09.rs 1, crates/flpdf-qtest-tools/src/driver/test_88_98.rs 1, crates/flpdf/src/embedded_files.rs 1, crates/flpdf/src/object_copy.rs 1, crates/flpdf/src/object_handle.rs 1, crates/flpdf/src/optimization.rs 1, crates/flpdf/src/optimization/inherited_attrs.rs 1, crates/flpdf/src/overlay_appearance_stream.rs 1, crates/flpdf/src/pages/repair.rs 1, crates/flpdf/src/reader.rs 1, crates/flpdf/src/reader/resolver.rs 1
crates/flpdf/src/reader/resolver.rs::remove_object: prod 0 (0 files) / test 1
crates/flpdf/src/reader.rs::Pdf::replace_object: prod 17 (10 files) / test 129
    crates/flpdf/src/json/input.rs 4, crates/flpdf-qtest-tools/src/driver/test_10_17.rs 2, crates/flpdf/src/page_annotation_flatten.rs 2, crates/flpdf/src/reader.rs 2, crates/flpdf/src/writer.rs 2, crates/flpdf/src/embedded_files.rs 1, crates/flpdf/src/job/outline_dest_remap.rs 1, crates/flpdf/src/job/page_merge.rs 1, crates/flpdf/src/object_copy.rs 1, crates/flpdf/src/page_extract.rs 1
crates/flpdf/src/reader.rs::Pdf::swap_objects: prod 3 (2 files) / test 7
    crates/flpdf-qtest-tools/src/driver/test_10_17.rs 2, crates/flpdf/src/reader.rs 1
crates/flpdf/src/reader/resolver.rs::disconnect_all: prod 1 (1 files) / test 0
    crates/flpdf/src/pdf.rs 1
crates/flpdf/src/object_handle.rs::ObjectHandle::disconnect: prod 2 (2 files) / test 26
    crates/flpdf/src/reader/resolver.rs 1, crates/flpdf/src/xref.rs 1
crates/flpdf/src/object_handle.rs::canonical_dictionary_key: prod 24 (8 files) / test 2
    crates/flpdf/src/nntree.rs 6, crates/flpdf/src/filespec_helper/filespec.rs 4, crates/flpdf/src/form_field_object_helper.rs 4, crates/flpdf/src/filespec_helper/embedded_file_stream.rs 3, crates/flpdf/src/job/json_sections.rs 2, crates/flpdf/src/object_handle.rs 2, crates/flpdf/src/parser.rs 2, crates/flpdf/src/json/input.rs 1
crates/flpdf/src/parser.rs::ContentHandleParser: prod 1 (1 files) / test 0
    crates/flpdf/src/content_stream.rs 1
crates/flpdf/src/parser.rs::content_good_count: prod 9 (1 files) / test 0
    crates/flpdf/src/parser.rs 9
crates/flpdf/src/parser.rs::content_give_up: prod 6 (1 files) / test 0
    crates/flpdf/src/parser.rs 6
crates/flpdf/src/xref.rs::read_uncompressed_object: prod 1 (1 files) / test 4
    crates/flpdf/src/xref.rs 1
crates/flpdf/src/reader/file_object.rs::finish_file_object_handle: prod 1 (1 files) / test 2
    crates/flpdf/src/xref.rs 1
crates/flpdf/src/reader/file_object.rs::recover_stream_boundary: prod 1 (1 files) / test 0
    crates/flpdf/src/reader/file_object.rs 1
crates/flpdf/src/reader/file_object.rs::RecoveryPolicy: prod 19 (2 files) / test 4
    crates/flpdf/src/reader/file_object.rs 10, crates/flpdf/src/xref.rs 9
crates/flpdf/src/xref.rs::resolve_objects_in_stream: prod 1 (1 files) / test 0
    crates/flpdf/src/xref.rs 1
crates/flpdf/src/xref.rs::parse_xref_from_start: prod 3 (1 files) / test 6
    crates/flpdf/src/xref.rs 3
crates/flpdf/src/reader/resolver.rs::reconstruct_xref_and_retry: prod 1 (1 files) / test 2
    crates/flpdf/src/reader/resolver.rs 1
crates/flpdf/src/xref.rs::XrefLoadOptions: prod 16 (2 files) / test 52
    crates/flpdf/src/xref.rs 15, crates/flpdf/src/engine.rs 1
crates/flpdf/src/xref.rs::LoadedXref: prod 7 (1 files) / test 2
    crates/flpdf/src/xref.rs 7
crates/flpdf/src/xref.rs::BootstrapHandleState: prod 7 (1 files) / test 10
    crates/flpdf/src/xref.rs 7
crates/flpdf/src/reader/resolver.rs::qpdf_exception_what: prod 1 (1 files) / test 0
    crates/flpdf/src/reader/resolver.rs 1
crates/flpdf/src/reader/resolver.rs::route_warning: prod 5 (1 files) / test 3
    crates/flpdf/src/reader/resolver.rs 5
crates/flpdf/src/pdf.rs::resolution_fallbacks_remaining: prod 8 (2 files) / test 0
    crates/flpdf/src/reader.rs 6, crates/flpdf/src/engine.rs 2
crates/flpdf/src/engine.rs::MAX_RESOLUTION_FALLBACKS: prod 2 (1 files) / test 0
    crates/flpdf/src/engine.rs 2
crates/flpdf/src/reader.rs::parse_source_file_object_at: prod 1 (1 files) / test 0
    crates/flpdf/src/reader.rs 1
crates/flpdf/src/parser.rs::too_many_bad_tokens: prod 6 (1 files) / test 0
    crates/flpdf/src/parser.rs 6
crates/flpdf/src/reader/resolver.rs::read_object_at_offset_with_description: prod 3 (1 files) / test 1
    crates/flpdf/src/reader/resolver.rs 3
crates/flpdf/src/reader/file_object.rs::parse_file_object_handle_syntax: prod 2 (2 files) / test 2
    crates/flpdf/src/reader.rs 1, crates/flpdf/src/xref.rs 1
crates/flpdf/src/reader/resolver.rs::validate_stream_line_end: prod 1 (1 files) / test 0
    crates/flpdf/src/reader/resolver.rs 1
crates/flpdf/src/reader/resolver.rs::recover_stream_length: prod 2 (1 files) / test 2
    crates/flpdf/src/reader/resolver.rs 2
crates/flpdf/src/reader/resolver.rs::resolve_object_stream_with_failure_kind: prod 1 (1 files) / test 1
    crates/flpdf/src/reader/resolver.rs 1
crates/flpdf/src/xref.rs::parse_trailer_candidate: prod 1 (1 files) / test 1
    crates/flpdf/src/xref.rs 1
crates/flpdf/src/xref.rs::merge_previous_xref_sections_with_observer: prod 2 (1 files) / test 0
    crates/flpdf/src/xref.rs 2
crates/flpdf/src/xref.rs::scan_object_header_after_first_token: prod 1 (1 files) / test 0
    crates/flpdf/src/xref.rs 1
crates/flpdf/src/xref.rs::merge_recovered_qpdf_state: prod 3 (1 files) / test 0
    crates/flpdf/src/xref.rs 3
crates/flpdf/src/xref.rs::recover_xref_from_linear_scan: prod 4 (1 files) / test 0
    crates/flpdf/src/xref.rs 4
crates/flpdf/src/reader/resolver.rs::reconstructed_xref: prod 11 (2 files) / test 23
    crates/flpdf/src/reader/resolver.rs 7, crates/flpdf/src/reader.rs 4
crates/flpdf/src/reader/resolver.rs::attempt_recovery: prod 13 (3 files) / test 1
    crates/flpdf/src/reader/resolver.rs 10, crates/flpdf/src/reader.rs 2, crates/flpdf/src/engine.rs 1
crates/flpdf/src/reader/resolver.rs::repair_diagnostics: prod 96 (15 files) / test 183
    crates/flpdf/src/xref.rs 60, crates/flpdf/src/reader/resolver.rs 12, crates/flpdf-qtest-tools/src/metadata.rs 4, crates/flpdf/src/engine.rs 4, crates/flpdf/src/job/check.rs 4, crates/flpdf-cli/src/main.rs 2, crates/flpdf/src/job/inspection.rs 2, crates/flpdf-qtest-tools/src/driver/mod.rs 1, crates/flpdf-qtest-tools/src/driver/test_10_17.rs 1, crates/flpdf-qtest-tools/src/driver/test_56_63.rs 1, crates/flpdf-qtest-tools/src/driver/test_64_71.rs 1, crates/flpdf-qtest-tools/src/driver/test_80_87.rs 1, crates/flpdf/src/job/lifecycle.rs 1, crates/flpdf/src/json/document.rs 1, crates/flpdf/src/reader.rs 1
crates/flpdf/src/object_handle.rs::format_qpdf_exception_what: prod 3 (3 files) / test 4
    crates/flpdf/src/content_stream.rs 1, crates/flpdf/src/object_handle.rs 1, crates/flpdf/src/page_document_helper.rs 1
crates/flpdf/src/error.rs::Error: prod 1273 (114 files) / test 592
    crates/flpdf/src/job/lifecycle.rs 110, crates/flpdf/src/xref.rs 103, crates/flpdf/src/writer.rs 80, crates/flpdf/src/reader/resolver.rs 71, crates/flpdf/src/object_handle.rs 68, crates/flpdf-cli/src/main.rs 47, crates/flpdf/src/linearization/writer.rs 46, crates/flpdf/src/page_splice.rs 30, crates/flpdf-qtest-tools/src/metadata.rs 27, crates/flpdf/src/page_object_helper.rs 26, crates/flpdf/src/writer/plain/plan.rs 24, crates/flpdf/src/object_copy.rs 22, crates/flpdf/src/nntree.rs 21, crates/flpdf/src/json/input.rs 20, crates/flpdf-qtest-tools/src/driver/test_10_17.rs 18, crates/flpdf/src/qdf_fix.rs 18, crates/flpdf/src/job/check.rs 16, crates/flpdf-qtest-tools/src/driver/test_80_87.rs 15, crates/flpdf/src/parser.rs 15, crates/flpdf/src/job/page_specs.rs 14, crates/flpdf/src/linearization/check.rs 14, crates/flpdf/src/stream_filter.rs 14, crates/flpdf/src/writer/plain/xref.rs 13, crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs 12, crates/flpdf-qtest-tools/src/driver/test_26_33.rs 12, crates/flpdf/src/error.rs 12, crates/flpdf/src/job/page_split.rs 12, crates/flpdf/src/writer/rewrite_renumber.rs 12, crates/flpdf-qtest-tools/src/driver/test_56_63.rs 11, crates/flpdf/src/job/page_combine.rs 11, crates/flpdf/src/writer/plain/body.rs 11, crates/flpdf/src/page_label_document_helper.rs 10, crates/flpdf/src/reader.rs 10, crates/flpdf-qtest-tools/src/driver/mod.rs 9, crates/flpdf-qtest-tools/src/driver/test_0_1.rs 9, crates/flpdf-qtest-tools/src/renumber.rs 9, crates/flpdf/src/engine.rs 9, crates/flpdf/src/filters.rs 9, crates/flpdf/src/job/attachments.rs 9, crates/flpdf/src/page_document_helper.rs 9, crates/flpdf/src/qutil.rs 9, crates/flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs 8, crates/flpdf-qtest-tools/src/driver/test_72_79.rs 8, crates/flpdf/src/job/page_range.rs 8, crates/flpdf/src/job/rotate_spec.rs 8, crates/flpdf/src/json_inspect.rs 8, crates/flpdf/src/page_extract.rs 8, crates/flpdf/src/pages.rs 8, crates/flpdf-qtest-tools/src/driver/test_02_09.rs 7, crates/flpdf-qtest-tools/src/large_file.rs 7, crates/flpdf/src/filespec_helper/embedded_file_stream.rs 7, crates/flpdf/src/job/overlay.rs 7, crates/flpdf/src/job/page_merge.rs 7, crates/flpdf/src/tokenizer.rs 7, crates/flpdf-qtest-tools/src/driver/test_50_55.rs 6, crates/flpdf/src/acroform_document_helper.rs 6, crates/flpdf/src/writer/object.rs 6, crates/flpdf/src/job/inspection.rs 5, crates/flpdf/src/job/page_plan.rs 5, crates/flpdf/src/linearization/back_patch.rs 5, crates/flpdf/src/linearization/plan.rs 5, crates/flpdf/src/linearization/show.rs 5, crates/flpdf/src/logger.rs 5, crates/flpdf/src/page_annotation_flatten.rs 5, crates/flpdf/src/pages/tree_rebuild.rs 5, crates/flpdf/src/reader/file_object.rs 5, crates/flpdf/src/writer/encrypted_strings.rs 5, crates/flpdf/src/content_stream.rs 4, crates/flpdf/src/filespec_helper/shared.rs 4, crates/flpdf/src/json/document.rs 4, crates/flpdf/src/pipeline/stdio_file.rs 4, crates/flpdf-libjpeg-compat/src/ffi.rs 3, crates/flpdf/src/diagnostics.rs 3, crates/flpdf/src/encryption/password.rs 3, crates/flpdf/src/encryption/state.rs 3, crates/flpdf/src/form_field_object_helper.rs 3, crates/flpdf/src/form_field_object_helper/rendering.rs 3, crates/flpdf/src/optimization/inherited_attrs.rs 3, crates/flpdf/src/pages/repair.rs 3, crates/flpdf-qtest-tools/src/character_encoding.rs 2, crates/flpdf-qtest-tools/src/compare.rs 2, crates/flpdf-qtest-tools/src/driver/test_42_49.rs 2, crates/flpdf-qtest-tools/src/driver/test_88_98.rs 2, crates/flpdf/src/embedded_files.rs 2, crates/flpdf/src/filespec_helper/filespec.rs 2, crates/flpdf/src/job/acroform_field_prune.rs 2, crates/flpdf/src/job/json.rs 2, crates/flpdf/src/job/outline_dest_remap.rs 2, crates/flpdf/src/job/rotate.rs 2, crates/flpdf/src/linearization/hint_stream.rs 2, crates/flpdf/src/pdf.rs 2, crates/flpdf/src/signatures.rs 2, crates/flpdf/src/writer/object_streams/emission.rs 2, crates/flpdf/src/writer/pclm.rs 2, crates/flpdf-qtest-tools/src/bin/test_parsedoffset.rs 1, crates/flpdf-qtest-tools/src/bin/test_xref.rs 1, crates/flpdf-qtest-tools/src/document_construction.rs 1, crates/flpdf-qtest-tools/src/driver/handle.rs 1, crates/flpdf-qtest-tools/src/driver/test_34_41.rs 1, crates/flpdf-qtest-tools/src/driver/test_64_71.rs 1, crates/flpdf-qtest-tools/src/tokenizer_runner.rs 1, crates/flpdf/src/bit_stream.rs 1, crates/flpdf/src/encryption/primitives.rs 1, crates/flpdf/src/encryption/standard.rs 1, crates/flpdf/src/job/image_optimization.rs 1, crates/flpdf/src/json/handler.rs 1, crates/flpdf/src/json/value.rs 1, crates/flpdf/src/optimization.rs 1, crates/flpdf/src/outline_document_helper.rs 1, crates/flpdf/src/page_form_xobject.rs 1, crates/flpdf/src/pipeline.rs 1, crates/flpdf/src/resources.rs 1, crates/flpdf/src/struct_tree_pg.rs 1, crates/flpdf/src/writer/serialize.rs 1
crates/flpdf/src/xref.rs::push_repair_diagnostics: prod 2 (1 files) / test 0
    crates/flpdf/src/xref.rs 2
crates/flpdf/src/object_handle.rs::pipe_stream_data_for_object_stream: prod 1 (1 files) / test 0
    crates/flpdf/src/reader/resolver.rs 1
crates/flpdf/src/filters.rs::stream_filter_capabilities: prod 1 (1 files) / test 0
    crates/flpdf/src/json_inspect.rs 1
crates/flpdf/src/stream_filter.rs::decode_filter_specs_from_handle: prod 3 (1 files) / test 0
    crates/flpdf/src/filters.rs 3
crates/flpdf/src/json_inspect.rs::stream_payload_with_decode_status: prod 2 (1 files) / test 0
    crates/flpdf/src/json_inspect.rs 2
crates/flpdf/src/filters.rs::decode_stream_data: prod 4 (3 files) / test 1
    crates/flpdf-qtest-tools/src/compare.rs 2, crates/flpdf-qtest-tools/src/driver/test_34_41.rs 1, crates/flpdf/src/json_inspect.rs 1
crates/flpdf/src/filters.rs::decode_stream_data_from_handle: prod 4 (3 files) / test 0
    crates/flpdf/src/xref.rs 2, crates/flpdf/src/filespec_helper/embedded_file_stream.rs 1, crates/flpdf/src/resources.rs 1
crates/flpdf/src/filters.rs::decode_stream_data_recovering: prod 0 (0 files) / test 1
crates/flpdf/src/filters.rs::decode_stream_data_recovering_with_limits: prod 2 (2 files) / test 2
    crates/flpdf-qtest-tools/src/driver/test_0_1.rs 1, crates/flpdf/src/filters.rs 1
crates/flpdf/src/filters.rs::encode_stream_data: prod 1 (1 files) / test 3
    crates/flpdf-qtest-tools/src/driver/test_02_09.rs 1
crates/flpdf/src/filters.rs::encode_stream_data_from_handle: prod 4 (3 files) / test 0
    crates/flpdf/src/overlay_appearance_stream.rs 2, crates/flpdf/src/filters.rs 1, crates/flpdf/src/writer/object_streams/emission.rs 1
crates/flpdf/src/filters.rs::is_decoded_filter: prod 1 (1 files) / test 0
    crates/flpdf/src/job/inspection.rs 1
crates/flpdf/src/filters.rs::passthrough_codec_label: prod 3 (3 files) / test 0
    crates/flpdf/src/filters.rs 1, crates/flpdf/src/job/inspection.rs 1, crates/flpdf/src/stream_filter.rs 1
crates/flpdf/src/encryption/primitives.rs::compute_data_key: prod 2 (2 files) / test 1
    crates/flpdf/src/encryption/state.rs 1, crates/flpdf/src/writer/encryption_state.rs 1
crates/flpdf/src/writer/plain/body.rs::canonical_stream_filter_probe: prod 2 (1 files) / test 0
    crates/flpdf/src/writer/plain/body.rs 2
crates/flpdf/src/writer/plain/body.rs::canonical_stream_filter_probe_for_linearization: prod 2 (1 files) / test 0
    crates/flpdf/src/linearization/plan.rs 2
crates/flpdf/src/writer/plain/body.rs::canonical_stream_will_be_refiltered_with_policy: prod 2 (2 files) / test 0
    crates/flpdf/src/linearization/plan.rs 1, crates/flpdf/src/writer/plain/body.rs 1
crates/flpdf/src/object_handle.rs::write_stream_json: prod 2 (1 files) / test 0
    crates/flpdf/src/document_json.rs 2
crates/flpdf/src/object_handle.rs::prepare_stream_filter_plan: prod 1 (1 files) / test 0
    crates/flpdf/src/object_handle.rs 1
crates/flpdf/src/writer/plain/body.rs::canonical_stream_output_with_rewrite_policy: prod 1 (1 files) / test 0
    crates/flpdf/src/writer/plain/body.rs 1
crates/flpdf/src/writer/rewrite_renumber.rs::CanonicalCatalogFirstRenumber: prod 5 (3 files) / test 0
    crates/flpdf/src/writer/plain/plan.rs 3, crates/flpdf/src/linearization/plan.rs 1, crates/flpdf/src/writer.rs 1
crates/flpdf/src/writer/object_streams/planning.rs::plan_object_streams_with_reachability: prod 1 (1 files) / test 0
    crates/flpdf/src/writer.rs 1
crates/flpdf/src/linearization/plan.rs::objstm_membership_linearized_with_eligibility: prod 2 (2 files) / test 0
    crates/flpdf/src/linearization/plan.rs 1, crates/flpdf/src/linearization/writer.rs 1
crates/flpdf/src/writer/object_streams/eligibility.rs::get_compressible_objgens: prod 2 (1 files) / test 0
    crates/flpdf/src/linearization/plan.rs 2
crates/flpdf/src/writer.rs::source_objstm_container_for_batch: prod 1 (1 files) / test 0
    crates/flpdf/src/writer.rs 1
crates/flpdf/src/writer/object.rs::write_trailer_with_ref_map: prod 8 (1 files) / test 0
    crates/flpdf/src/writer.rs 8
crates/flpdf/src/linearization/writer.rs::write_linearized: prod 0 (0 files) / test 3
crates/flpdf/src/writer.rs::snapshot_catalog_extensions: prod 2 (2 files) / test 1
    crates/flpdf/src/linearization/writer.rs 1, crates/flpdf/src/writer.rs 1
crates/flpdf/src/writer.rs::restore_catalog_extensions: prod 2 (2 files) / test 1
    crates/flpdf/src/linearization/writer.rs 1, crates/flpdf/src/writer.rs 1
crates/flpdf/src/writer.rs::qpdf_preserve_source_objstm: prod 7 (1 files) / test 0
    crates/flpdf/src/writer.rs 7
crates/flpdf/src/writer.rs::write_qpdf_to_memory: prod 2 (1 files) / test 16
    crates/flpdf-cli/src/main.rs 2
crates/flpdf/src/writer.rs::PdfWriter::write: prod 328 (74 files) / test 1003
    crates/flpdf/src/object_handle.rs 39, crates/flpdf-qtest-tools/src/driver/test_42_49.rs 29, crates/flpdf/src/json/writer.rs 16, crates/flpdf-qtest-tools/src/driver/test_72_79.rs 14, crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs 10, crates/flpdf-qtest-tools/src/driver/test_26_33.rs 10, crates/flpdf-cli/src/main.rs 9, crates/flpdf-qtest-tools/src/driver/test_02_09.rs 8, crates/flpdf-qtest-tools/src/driver/test_0_1.rs 8, crates/flpdf-qtest-tools/src/driver/test_34_41.rs 8, crates/flpdf/src/linearization/show.rs 8, crates/flpdf/src/writer.rs 8, crates/flpdf-qtest-tools/src/tokenizer_runner.rs 7, crates/flpdf/src/job/lifecycle.rs 7, crates/flpdf/src/pipeline/run_length.rs 7, crates/flpdf-qtest-tools/src/driver/test_56_63.rs 6, crates/flpdf-qtest-tools/src/driver/test_64_71.rs 6, crates/flpdf/src/content_normalizer.rs 5, crates/flpdf/src/form_field_object_helper/rendering.rs 5, crates/flpdf/src/logger.rs 5, crates/flpdf/src/pipeline/aes.rs 5, crates/flpdf-qtest-tools/src/driver/test_10_17.rs 4, crates/flpdf-qtest-tools/src/driver/test_18_25.rs 4, crates/flpdf-qtest-tools/src/driver/test_50_55.rs 4, crates/flpdf/src/encryption/standard.rs 4, crates/flpdf/src/job/json.rs 4, crates/flpdf/src/pipeline/dct.rs 4, crates/flpdf/src/pipeline/png_filter.rs 4, crates/flpdf/src/pipeline/stream_codecs_oracle.rs 4, crates/flpdf/src/stream_filter.rs 4, crates/flpdf/src/document_json.rs 3, crates/flpdf/src/json_inspect.rs 3, crates/flpdf/src/linearization/check.rs 3, crates/flpdf/src/page_object_helper.rs 3, crates/flpdf/src/pipeline.rs 3, crates/flpdf/src/pipeline/flate.rs 3, crates/flpdf/src/pipeline/stdio_file.rs 3, crates/flpdf-qtest-tools/src/document_construction.rs 2, crates/flpdf-qtest-tools/src/driver/test_80_87.rs 2, crates/flpdf-qtest-tools/src/large_file.rs 2, crates/flpdf/src/job/attachments.rs 2, crates/flpdf/src/json/input.rs 2, crates/flpdf/src/linearization/writer.rs 2, crates/flpdf/src/pipeline/ascii85_decoder.rs 2, crates/flpdf/src/pipeline/base64.rs 2, crates/flpdf/src/pipeline/lzw.rs 2, crates/flpdf/src/pipeline/lzw_png_oracle.rs 2, crates/flpdf/src/pipeline/test_support.rs 2, crates/flpdf/src/qpdf_time.rs 2, crates/flpdf/src/qutil.rs 2, crates/flpdf/src/token_filter.rs 2, crates/flpdf-qtest-tools/src/metadata.rs 1, crates/flpdf-qtest-tools/src/renumber.rs 1, crates/flpdf/src/bit_writer.rs 1, crates/flpdf/src/filespec_helper/embedded_file_stream.rs 1, crates/flpdf/src/filespec_helper/shared.rs 1, crates/flpdf/src/job/check.rs 1, crates/flpdf/src/job/inspection.rs 1, crates/flpdf/src/job/page_split.rs 1, crates/flpdf/src/object_ref.rs 1, crates/flpdf/src/pages.rs 1, crates/flpdf/src/pipeline/ascii_hex.rs 1, crates/flpdf/src/pipeline/buffer.rs 1, crates/flpdf/src/pipeline/concatenate.rs 1, crates/flpdf/src/pipeline/count.rs 1, crates/flpdf/src/pipeline/md5.rs 1, crates/flpdf/src/pipeline/rc4.rs 1, crates/flpdf/src/pipeline/sha2.rs 1, crates/flpdf/src/pipeline/string.rs 1, crates/flpdf/src/pipeline/tiff_predictor.rs 1, crates/flpdf/src/reader/resolver.rs 1, crates/flpdf/src/resource_replacer.rs 1, crates/flpdf/src/writer/object_streams/emission.rs 1, crates/flpdf/src/writer/serialize.rs 1
crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber: prod 4 (2 files) / test 0
    crates/flpdf/src/writer/plain/plan.rs 3, crates/flpdf/src/writer.rs 1
crates/flpdf/src/writer/object_streams/planning.rs::ObjectStreamGroup: prod 20 (4 files) / test 0
    crates/flpdf/src/writer/plain/plan.rs 10, crates/flpdf/src/writer/rewrite_renumber.rs 6, crates/flpdf/src/writer.rs 2, crates/flpdf/src/writer/object_streams/planning.rs 2
crates/flpdf/src/writer/object_streams/planning.rs::plan_qpdf_preserve_object_streams_with_unreferenced: prod 1 (1 files) / test 0
    crates/flpdf/src/writer/plain/plan.rs 1
crates/flpdf/src/writer/object_streams/eligibility.rs::compressible_objgens_qpdf_plan: prod 9 (6 files) / test 1
    crates/flpdf/src/writer/object_streams/planning.rs 3, crates/flpdf/src/linearization/plan.rs 2, crates/flpdf/src/linearization/writer.rs 1, crates/flpdf/src/writer.rs 1, crates/flpdf/src/writer/object_streams/eligibility.rs 1, crates/flpdf/src/writer/plain/plan.rs 1
crates/flpdf/src/reader/resolver.rs::source_xref_entries: prod 29 (9 files) / test 3
    crates/flpdf/src/reader/resolver.rs 9, crates/flpdf/src/writer.rs 5, crates/flpdf/src/reader.rs 4, crates/flpdf/src/linearization/writer.rs 3, crates/flpdf/src/engine.rs 2, crates/flpdf/src/linearization/plan.rs 2, crates/flpdf/src/writer/object_streams/planning.rs 2, crates/flpdf/src/writer/plain/plan.rs 1, crates/flpdf/src/writer/rewrite_renumber.rs 1
crates/flpdf/src/writer/plain/body.rs::emit_bodies: prod 1 (1 files) / test 0
    crates/flpdf/src/writer/plain/mod.rs 1
crates/flpdf/src/writer/plain/xref.rs::append_xref_and_trailer: prod 2 (2 files) / test 1
    crates/flpdf/src/writer.rs 1, crates/flpdf/src/writer/plain/mod.rs 1
crates/flpdf/src/writer/plain/plan.rs::canonical_trailer_entries_with_visibility: prod 2 (2 files) / test 2
    crates/flpdf/src/writer.rs 1, crates/flpdf/src/writer/plain/plan.rs 1
crates/flpdf/src/writer.rs::build_writer_trailer_handle: prod 5 (2 files) / test 0
    crates/flpdf/src/writer.rs 4, crates/flpdf/src/writer/plain/plan.rs 1
crates/flpdf/src/writer.rs::EncryptionContext: prod 21 (3 files) / test 0
    crates/flpdf/src/writer.rs 12, crates/flpdf/src/linearization/writer.rs 7, crates/flpdf/src/writer/encrypted_strings.rs 2
crates/flpdf/src/writer/object_streams/planning.rs::filter_objstm_batches_for_output: prod 1 (1 files) / test 0
    crates/flpdf/src/writer.rs 1
crates/flpdf/src/writer/pclm.rs::Plan: prod 1 (1 files) / test 4
    crates/flpdf/src/writer.rs 1
crates/flpdf/src/writer.rs::write_pclm: prod 1 (1 files) / test 3
    crates/flpdf/src/writer.rs 1
crates/flpdf/src/writer.rs::inject_adbe_extension: prod 2 (2 files) / test 0
    crates/flpdf/src/linearization/writer.rs 1, crates/flpdf/src/writer.rs 1
crates/flpdf/src/writer.rs::strip_adbe_extension: prod 2 (2 files) / test 0
    crates/flpdf/src/linearization/writer.rs 1, crates/flpdf/src/writer.rs 1
crates/flpdf/src/writer/plain/plan.rs::retain_reachable_object_stream_members: prod 2 (1 files) / test 0
    crates/flpdf/src/writer/plain/plan.rs 2
crates/flpdf-cli/src/main.rs::write_with_pdf_writer: prod 8 (1 files) / test 0
    crates/flpdf-cli/src/main.rs 8
crates/flpdf/src/job/attachment_list.rs::format_attachment_list_with_sink: prod 1 (1 files) / test 0
    crates/flpdf/src/job/attachments.rs 1
crates/flpdf/src/job/attachment_list.rs::AttachmentInfo: prod 0 (0 files) / test 0
crates/flpdf/src/job/json.rs::write_json: prod 10 (3 files) / test 19
    crates/flpdf/src/document_json.rs 6, crates/flpdf/src/object_handle.rs 3, crates/flpdf-qtest-tools/src/driver/test_88_98.rs 1
crates/flpdf/src/job/acroform_field_prune.rs::prune_acroform_after_subset: prod 2 (1 files) / test 17
    crates/flpdf/src/job/page_specs.rs 2
crates/flpdf/src/reader.rs::Pdf::qtest_object_value_source_offsets: prod 1 (1 files) / test 0
    crates/flpdf-qtest-tools/src/driver/test_0_1.rs 1
crates/flpdf/src/reader.rs::Pdf::qtest_array_item_source_offsets: prod 1 (1 files) / test 0
    crates/flpdf-qtest-tools/src/driver/test_0_1.rs 1
crates/flpdf/src/reader.rs::Pdf::qtest_decode_parms_source_offset: prod 1 (1 files) / test 0
    crates/flpdf-qtest-tools/src/driver/test_0_1.rs 1
crates/flpdf/src/job/lifecycle.rs::QPDFJob::run: prod 17 (11 files) / test 224
    crates/flpdf-qtest-tools/src/driver/test_80_87.rs 4, crates/flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs 3, crates/flpdf-qtest-tools/src/character_encoding.rs 2, crates/flpdf-cli/src/main.rs 1, crates/flpdf-qtest-tools/src/bin/driver.rs 1, crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs 1, crates/flpdf-qtest-tools/src/bin/test_large_file.rs 1, crates/flpdf-qtest-tools/src/bin/test_renumber.rs 1, crates/flpdf-qtest-tools/src/bin/tokenizer.rs 1, crates/flpdf-qtest-tools/src/main.rs 1, crates/flpdf/src/object_copy.rs 1
crates/flpdf/src/job/lifecycle.rs::QPDFJob::create_qpdf: prod 3 (2 files) / test 7
    crates/flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs 2, crates/flpdf/src/job/lifecycle.rs 1
crates/flpdf/src/job/lifecycle.rs::QPDFJob::write_qpdf: prod 2 (2 files) / test 4
    crates/flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs 1, crates/flpdf/src/job/lifecycle.rs 1
crates/flpdf/src/job/page_split.rs::QPDFJob::split_pages: prod 22 (2 files) / test 16
    crates/flpdf-cli/src/main.rs 13, crates/flpdf/src/job/lifecycle.rs 9
crates/flpdf/src/job/lifecycle.rs::QPDFJob::write_json_with_version: prod 7 (4 files) / test 0
    crates/flpdf/src/job/lifecycle.rs 3, crates/flpdf-cli/src/main.rs 2, crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs 1, crates/flpdf/src/job/json.rs 1
crates/flpdf/src/job/lifecycle.rs::write_configured_json: prod 1 (1 files) / test 0
    crates/flpdf/src/job/lifecycle.rs 1
crates/flpdf/src/job/lifecycle.rs::run_configured_inspection: prod 1 (1 files) / test 0
    crates/flpdf/src/job/lifecycle.rs 1
crates/flpdf/src/job/check.rs::QPDFJob::check: prod 20 (2 files) / test 146
    crates/flpdf/src/job/lifecycle.rs 12, crates/flpdf-cli/src/main.rs 8
crates/flpdf/src/job/attachments.rs::QPDFJob::list_attachments: prod 9 (2 files) / test 7
    crates/flpdf/src/job/lifecycle.rs 5, crates/flpdf-cli/src/main.rs 4
crates/flpdf/src/job/page_specs.rs::QPDFJob::handle_page_specs: prod 6 (3 files) / test 18
    crates/flpdf-cli/src/main.rs 4, crates/flpdf/src/job/lifecycle.rs 1, crates/flpdf/src/job/page_specs.rs 1
crates/flpdf/src/job/overlay.rs::apply_overlay_specs: prod 3 (2 files) / test 0
    crates/flpdf-cli/src/main.rs 2, crates/flpdf/src/job/lifecycle.rs 1
crates/flpdf/src/job/overlay.rs::overlay_verbose_report: prod 2 (1 files) / test 0
    crates/flpdf-cli/src/main.rs 2
crates/flpdf/src/job/lifecycle.rs::run_document_stages: prod 3 (1 files) / test 0
    crates/flpdf/src/job/lifecycle.rs 3
crates/flpdf/src/job/image_optimization.rs::optimize_images: prod 23 (2 files) / test 2
    crates/flpdf-cli/src/main.rs 20, crates/flpdf/src/job/lifecycle.rs 3
crates/flpdf/src/job/rotate.rs::flatten_rotation_on_pages: prod 3 (2 files) / test 10
    crates/flpdf-cli/src/main.rs 2, crates/flpdf/src/job/lifecycle.rs 1
crates/flpdf/src/job/lifecycle.rs::apply_configured_rotations: prod 3 (1 files) / test 0
    crates/flpdf/src/job/lifecycle.rs 3
crates/flpdf/src/job/rotate.rs::apply_rotate_to_pages: prod 2 (2 files) / test 16
    crates/flpdf-cli/src/main.rs 1, crates/flpdf/src/job/lifecycle.rs 1
crates/flpdf/src/job/rotate_spec.rs::RotateSpec: prod 5 (3 files) / test 5
    crates/flpdf-cli/src/main.rs 2, crates/flpdf/src/job/lifecycle.rs 2, crates/flpdf/src/job/rotate_spec.rs 1
crates/flpdf/src/job/page_range.rs::PageRange: prod 39 (7 files) / test 74
    crates/flpdf-cli/src/main.rs 13, crates/flpdf/src/job/overlay.rs 9, crates/flpdf/src/job/lifecycle.rs 6, crates/flpdf/src/job/page_combine.rs 4, crates/flpdf/src/job/rotate_spec.rs 4, crates/flpdf/src/job/page_specs.rs 2, crates/flpdf/src/job/page_plan.rs 1
crates/flpdf/src/job/resource_pruning.rs::should_remove_unreferenced_resources: prod 2 (2 files) / test 10
    crates/flpdf-cli/src/main.rs 1, crates/flpdf/src/job/page_merge.rs 1
crates/flpdf/src/job/lifecycle.rs::QPDFJob::initialize_from_argv: prod 3 (1 files) / test 16
    crates/flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs 3
crates/flpdf/src/job/lifecycle.rs::QPDFJob::complete: prod 22 (7 files) / test 15
    crates/flpdf-cli/src/main.rs 6, crates/flpdf/src/job/lifecycle.rs 5, crates/flpdf/src/document_json.rs 3, crates/flpdf/src/job/check.rs 2, crates/flpdf/src/pipeline/stream_codecs_oracle.rs 2, crates/flpdf/src/reader/resolver.rs 2, crates/flpdf/src/resources.rs 2
crates/flpdf/src/job/lifecycle.rs::QPDFJob::has_warnings: prod 12 (3 files) / test 7
    crates/flpdf-cli/src/main.rs 8, crates/flpdf-qtest-tools/src/driver/test_80_87.rs 2, crates/flpdf/src/job/check.rs 2
crates/flpdf-cli/src/main.rs::main: prod 0 (0 files) / test 0
crates/flpdf/src/job/lifecycle.rs::QPDFJob::open_document: prod 10 (1 files) / test 1
    crates/flpdf/src/linearization/writer.rs 10
crates/flpdf/src/job/lifecycle.rs::QPDFJob::open: prod 63 (28 files) / test 1283
    crates/flpdf-cli/src/main.rs 19, crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs 5, crates/flpdf-qtest-tools/src/driver/test_88_98.rs 3, crates/flpdf-qtest-tools/src/metadata.rs 3, crates/flpdf/src/job/lifecycle.rs 3, crates/flpdf/src/reader.rs 3, crates/flpdf-qtest-tools/src/renumber.rs 2, crates/flpdf/src/engine.rs 2, crates/flpdf/src/linearization/check.rs 2, crates/flpdf/src/linearization/show.rs 2, crates/flpdf/src/qdf_fix.rs 2, crates/flpdf-cli/src/arg_parser.rs 1, crates/flpdf-qtest-tools/src/bin/unicode_filenames.rs 1, crates/flpdf-qtest-tools/src/character_encoding.rs 1, crates/flpdf-qtest-tools/src/driver/mod.rs 1, crates/flpdf-qtest-tools/src/driver/test_26_33.rs 1, crates/flpdf-qtest-tools/src/driver/test_56_63.rs 1, crates/flpdf-qtest-tools/src/driver/test_64_71.rs 1, crates/flpdf-qtest-tools/src/driver/test_80_87.rs 1, crates/flpdf-qtest-tools/src/large_file.rs 1, crates/flpdf-qtest-tools/src/output.rs 1, crates/flpdf/src/filespec_helper/embedded_file_stream.rs 1, crates/flpdf/src/job/page_combine.rs 1, crates/flpdf/src/json/document.rs 1, crates/flpdf/src/json/input.rs 1, crates/flpdf/src/pipeline/stream_codecs_oracle.rs 1, crates/flpdf/src/qutil.rs 1, crates/flpdf/src/writer.rs 1
```

### 6.3 tracker の数字と行セルの乖離

以下の数値は **2026-09-05 の比較履歴**であり、§6.2の再計測値ではない。
数え方の違いを説明する目的で保持する。現在の総数は§6.2、特定ownerの残callerは領域別表の
再監査行とreceiver確認を優先する。trackerは同名leaf・束縛・型参照も含む。
旧D19の複数行文字列誤検出は現在の `mask_rust_source` によって解消している。

| 行 | symbol（leaf） | 行セル prod | tracker prod | 乖離の理由 | 今後どちらを正とするか |
|---|---|---|---|---|---|
| A7 | `resolve` | 256 | 257 | 行は「`\.resolve\(` 全件 − 非 `Pdf` receiver 23 件」の差集合。tracker の素の leaf 一致は `PageRange::resolve` 等も拾う | **行の方法**（leaf が曖昧）。数え直すときは行に書かれた差集合手順を再実行する |
| A10 | `object_refs` | 6 | 14 | tracker は `let object_refs = pdf.object_refs();` のような **ローカル束縛と for パターン**（`crates/flpdf/src/linearization/plan.rs:1071` 付近に 3 件）も数える。行は `Pdf::object_refs()` の呼び出しだけを数えた | tracker（型位置・束縛も「移行が要る実参照」なので規約どおり） |
| A15 | `synchronize_cache_with_resolver_xref` | 6 / test 2 | 6 / test 2 | `.46` で A14 の production deletion facade とその ObjStm 昇格 helper を撤去し、現在は reader.rs の remaining legacy-cache synchronization callers と test-only remove routeだけが残る | tracker |
| A23 | `legacy_dictionary_key` | 8 | 6 | 行が挙げた 8 箇所のうち `crates/flpdf/src/stream_filter.rs:48` と `crates/flpdf/src/writer/object.rs:10` は `use` 行。tracker は `use` 行を除外する | tracker |
| A24 | `compressed_member_parents` | 6 | 6 | A14 専用の ObjStm 昇格 helper は `.46` で撤去した。残る provenance state は A2/A15 の legacy cache 列を畳むまで保持する | tracker |
| B25 | `reconstructed_xref` | 6 | 12 | 行は `reconstructed_xref(`（開き括弧つき）で数えたのでフィールド読み書きが落ちる。tracker は leaf 一致なので両方数える | tracker（行側の注記「`\breconstructed_xref\b` だと 48 hit」は除外規則を適用する前の生 `rg` 件数） |
| B26 | `attempt_recovery` | 1 | 10 | 同上（`attempt_recovery(` で数えた行 vs フィールド参照も含む tracker） | tracker |
| B27 | `repair_diagnostics` | 20 | 97 | 行は公開ドア `.repair_diagnostics()` の呼び出しだけ。tracker は同名フィールドへのアクセスも数える | tracker（ただし B27 の主張「構築後の sink は 1 本」は呼び出しドアの数に依存しないので影響しない） |
| C17 + C18 | `compute_data_key` | 共有1 | 2（prod、2 consumer）+ 1 test | `crates/flpdf/src/encryption/primitives.rs::compute_data_key` が qpdf static primitive の単一正本。reader `key_for_object` と writer `set_data_key` が利用し、旧 state-local duplicate は削除 | tracker（共有 primitive の caller と oracle vectors） |
| C28 | `decode_stream_data_recovering` | 1 | 0 | 行は 2 symbol をまとめて「prod 1（`test_0_1.rs:332`）」と書いたが、その呼び出しは実際には `decode_stream_data_recovering_with_limits`（tracker prod 2） | tracker（行の粒度が粗かった） |
| C29 | `encode_stream_data_from_handle` | 3 | 4 | 4 件目は `crates/flpdf/src/filters.rs:350` — 同じ C29 の対である `encode_stream_data` からの内部委譲で、行は外部 caller だけを挙げていた | tracker |
| D6 / D8 | `plan_qpdf_preserve_object_streams_with_unreferenced` / `get_compressible_objgens` / `compressible_objgens_qpdf_plan` | 2 / 3 / 9 | 1 / 2 / 7 | 行は `writer/object_streams/mod.rs` の `pub use` 再輸出行を prod に数えている。tracker は `use` 行（複数行の継続を含む）を除外する | tracker |
| D9 | `source_xref_entries` | 10 | 32 | 行は「writer / linearization 系のみ。reader 内部の 5 箇所は除く」と **手で範囲を絞った**。tracker は全 crate を数える（reader 側 16、engine 2 を含む） | tracker（行の 10 は「writer 側の再実装が何箇所あるか」を示す別の数で、cutover の分母ではない） |
| D16 | `EncryptionContext` | 22 | 21 | 合計だけでなくファイル別内訳も一致しない（`writer.rs` 13→12、`linearization/writer.rs` 6→7、`writer/encrypted_strings.rs` 3→2）。除外規則（宣言行 / `use` 行 / `impl` ヘッダ）の適用差 | tracker |
| D19 | `write_linearized` | 0 | 旧1 → 現0 | 複数行文字列の旧誤検出はsource全体のmaskで解消済み | canonical test scaffoldingであり削除ゲート対象外 |
| D30 | `write_qpdf_to_memory` | 0 | 旧2 | CLIローカルの同名関数に衝突する。現値は§6.2 | test-only helperはcanonical scaffolding。production callerはreceiver/pathで確認する |
| D29 | `qpdf_preserve_source_objstm` | 1 | 1 | reachable な §6.2 snapshot では PR #1486 merge 後の `crates/flpdf/src/writer.rs:3919,3997` を含む | tracker（行の caller 数と tracker の leaf 数が一致） |
| E-5 | `split_pages` | 2 | 23 | leaf が `QPDFJob::split_pages` メソッドと `configuration.split_pages` フィールド（`crates/flpdf/src/job/lifecycle.rs:191`）に衝突 | 行（leaf が曖昧）。tracker の 23 は分母としてのみ読む |
| E-9 | `format_attachment_list_with_sink` | 0 | 1 | `.43` で buffer-returning wrapperを削除した後も、`QPDFJob::list_attachments` から logger sink として 1 箇所だけ呼ばれる | tracker |
| E-9 | `list_attachments` | 1 | 11 | leaf が `QPDFJob::list_attachments` の直接呼び出しに加えて、同名の configuration field なども拾う。`.43` の test/example 移行後は直接呼び出しが `attachments.rs`、`attachment_list.rs`、`pull_attachments.rs` に増えた | tracker（leaf が曖昧） |
| E-12 | `optimize_images` | 6 | 23 | 同上（`crates/flpdf-cli/src/main.rs` 19 件 = `flpdf::optimize_images` の 6 呼び出し + `--optimize-images` の引数処理、`crates/flpdf/src/job/lifecycle.rs` 4 件 = configuration フィールド） | 行（leaf が曖昧） |
| E-19 | `complete` / `has_warnings` | 13 / 8 | 22 / 12 | 行は `QPDFJob::complete()` / `QPDFJob::has_warnings()` の **呼び出しだけ**を数え、型位置・フィールド参照・同名の別項目を含めていない | tracker（分母として。`QPDFJob` メソッドの呼び出し数だけが要るときは行の数を使う） |
| E-24 | `write_json` | 0（free 関数） | 10 | leaf が `crates/flpdf/src/document_json.rs` の同名関数（6 件、加えて `crates/flpdf-qtest-tools/src/driver/test_88_98.rs:342` からの同関数呼び出し 1 件）と `crates/flpdf/src/object_handle.rs` の 3 件にも衝突（計 10） | 行（leaf が曖昧）。free `write_json` 自身の prod caller は 0 のまま |
| E-14 / E-15 | `RotateSpec::parse` / `PageRange::parse` | 2 / 14 | — | leaf `parse` は使用不能（workspace 全体で prod 292）。manifest には **型名** `RotateSpec`（prod 5）/ `PageRange`（prod 39）を登録して代用している | 行（型名の数字は別の量なので、method の caller 数は行の `rg` 手順で数え直す） |

領域 D は他の 4 領域より tracker との乖離が多い。原因は §8 X-6 に書いたとおり、D ファイルが
「モジュール直下の最初の `#[cfg(test)] mod` より前＝prod」という単純化を採ったのに対し、
A ファイルがその単純化は `object_handle.rs`（桁 0 の `#[cfg(test)] mod` が 21 個）で成立しないと
指摘し、tracker は A 側の brace 追跡規約を実装しているためである。

### 6.4 完了判定

**(a) deletable route の bridge が削除可能になる条件は次の 2 つを両方満たすこと:**

1. `python3 scripts/qpdf-route-callers.py --symbol <leaf> --expect-zero` が exit 0 を返す
   （production caller ゼロ）。
2. その symbol を使っていた test が canonical owner 経由へ移行済みか削除済みであること
   （tracker の `test` 列が 0 になるか、残った test が canonical route を検証するものに
   書き換わっていること）。

1 だけでは足りない。`crates/flpdf/tests/*.rs` は別コンパイル単位なので、production caller が
ゼロでも統合テストが `pub` を要求している限り可視性を狭められない（E-24 / E-26 がこの形）。

**2026-09-06 の再監査:** D27 の single-source / multi-source sweep はともに撤去済み。
旧C19/C28/E-27のdead wrapperは `.42`、C23/E-9の選定済みtest-only routeは `.43` で削除済み。
残る C28 `decode_stream_data_recovering_with_limits` は driver test 0/1 の移行が必要であり、
C28 行全体を削除済みとは扱わない。E-24 / E-26 は別途 public consumer と test を移行する。

D19 / D30 は canonical implementation に委譲するbyte-neutral test scaffoldingで、削除対象ではない。
特に D19 は back-patch 前の観測点を保持する。CLI の同名 `write_qpdf_to_memory` は別関数なので
leaf の総数からこのhelperのproduction callerを推測しない。

**D27 follow-up（`flpdf-3yn9.44.1`）:** linearization の stream-parameter probe は
deletable route の独立 symbol にはせず、D21 の canonical owner
`crates/flpdf/src/optimization.rs::Optimization` の callback に吸収した。したがって
`tracked-symbols.txt` に全 object prepass の symbol を追加せず、D27 の完了状態と
「linearized planning も page/trailer/root 起点の到達範囲だけを解決する」境界を
`d-writer.md` と §7.3 の follow-up 記録で追跡する。

**P1 follow-up（`flpdf-3yn9.44.1.1`）:** ObjStm planning の `/Length` 除外集合も
`QPDF::getCompressibleObjGens` 相当の到達可能 walk から供給するよう統一した。
`pdf.object_refs()` による全 xref の事前解決は削除し、非 linearized の
Preserve/Generate と linearized Preserve の各回帰テストで orphan の reader-level
failure を越えないことを確認する。qpdf の writer setup が `getObjectCount` で全 xref
を解決して warning/null 回復する責務とは別なので、qpdf が orphan を常に無視すると
いう意味ではない。malformed orphan の live qpdf 比較は `--warning-exit-0` と warning
出力を検証する。

**(b) baseline denominator には `--expect-zero` を当てない。** その行の完了判定は
「canonical owner に収束したか」であって「caller が 0 になったか」ではない。

## 7. cutover 計画と最初の bounded cutover

§7.1 / §7.3は最初のD27 cutoverの選定・実施履歴である。現在の後続順序は§7.2、issue対応は§7.4を参照する。
primitive新設には既存consumerのRED、dead route削除には0 callerと移行済みtestを使い、全sliceに同じzero-caller条件を課さない。

### 7.1 初回の選定基準と候補比較（履歴）

選定基準は **依存の少なさ × 完成可能性** の 2 つだけ。qtest の pass 数は使わない
（qtest manifest は別リポジトリで管理されており、本表の route とは対応しない）。
これに加えて、最初の cutover には次の 3 条件を課す。

1. **今日 qpdf と観測差がある**こと。dead route の削除（prod 0）は何も変わらないので
   RED test を持てず、最初の cutover にはならない（§7.4 の hygiene スライスへ回す）。
2. **特殊ケースを要求しない**こと（`.claude/rules/qpdf-port-design-patterns.md` 2）。
   sentinel・新規 panic 分岐・qpdf に対応物のない中間表現を要するなら、それは前提の
   逸脱が未修正というシグナルなので別スライスに切り出す。
3. 完了判定が `scripts/qpdf-route-callers.py --symbol <leaf> --expect-zero` で
   機械的に取れること（§6.4 の (a) 群であること）。

| 候補（行） | tracker prod / test | 今日観測できる乖離（probe 証拠） | 絡み（同時に触ってはいけない行） | 判定 |
|---|---|---|---|---|
| **D27** pre-write reachability routes (完了) | 0 / 0 | **完了。** qpdf の `QPDFWriter::enqueueObject` / `enqueueObjectsStandard` を writer の emission boundary として採用し、single-source の旧 `sweep_unreachable_objects` と multi-source `--pages` の `_except` sweep をともに撤去した。single-source の qpdf-zlib byte gate と、multi-source の preserve control を維持する | A14（`.46` で完了）、D2/D3/D11（採番・body loop）は非対象 | **完了**（§7.3） |
| A14 `Pdf::replace_object` → `ResolverHandle::replace_object` | 0 / 0 | **完了。** qpdf の public deletion 相当 `replaceObject(og, newNull())` に route を統合し、signature value stripping は eager deletion を行わず writer の到達性に委ねる。`--remove-restrictions --preserve-unreferenced` の qpdf byte compare も GREEN | A2 / A15 / A24 / A13（legacy cache/tombstone は別 slice） | `.46` で `Pdf::delete_object` と全 production/test caller を撤去した |
| B14 `crates/flpdf/src/xref.rs::parse_xref_from_start` | 3 / 6 | **観測できず。** `probe:` 自分の startxref を指す `/Prev` を持つ 1 section PDF で `qpdf --check` / `flpdf --check` → 双方とも `file is damaged` / `loop detected following xref tables` / `Attempting to reconstruct cross-reference table` の 3 行・同順・exit 3。B14 が予測する診断の二重 push は現れない | なし（単一ファイル `crates/flpdf/src/xref.rs`） | 保留。RED が立たない。B-P1 は「二重 push は起きない」で決着させ、行の主張を弱める（§7.4 の後続 issue） |
| C22 `crates/flpdf/src/writer/plain/body.rs::canonical_stream_filter_probe` | 初回2 / 0 | **観測できず（CLI 経路では）。** `probe: qpdf --static-id --normalize-content=y [--linearize] two.pdf` と flpdf 同等 → plain / `--linearize` の双方で byte 一致。早期 return を踏むには token filter 登録が要り、それは CLI から到達しない（C-U3 は library harness を要求する） | C20 canonical / C21 mixed（判定順序と責務境界を保つ） | 保留。harness を先に作る（§7.2 stream family の3） |
| D3 `crates/flpdf/src/writer/rewrite_renumber.rs::CanonicalCatalogFirstRenumber` | 初回5 / 0 | `flpdf-hi08` / PR #1486のencrypted Preserve修正はmerge済み。残る先行walkとemission統合は別責務 | D2 / D5 / D6 / D11 / D12 | 初回は保留。現在はD12共有primitive等を独立sliceにできる（§7.2.5） |

E-29 は `.47` で完了したため候補表から除外した。`--no-warn` の open-time delivery は
ordinary input だけでなく、overlay/underlay、copy-encryption、encryption probe、attachment
copy、page source、JSON input の各 route で qpdf と同じ抑制 policy を受ける。残る
`open_page_source` の direct `open_file_with_options` は、source を後で close/reopen する
必要があるための構造上の例外であり、open 前に同じ `PdfOpenOptions::suppress_warnings` を設定する。

**probe 実行時の注記**: 出力に `/FlateDecode` が含まれる（`strings q.pdf \| grep -c Flate` → 2）ため、
上表の byte 比較はすべて `qpdf-zlib-compat` feature でビルドした flpdf で行った。
CLAUDE.md 逸脱分類 (A) のとおり、既定の miniz_oxide ビルドではこの比較は成立しない。

**probe 中に見つかった、本表に無い乖離**（本節では扱わないが記録する）:
B14 の probe で、qpdf は open 時の 3 つの warning を `checking <file>` の **前** に出すのに対し
flpdf は **後** に出す。診断の本文・件数・exit code は一致するので B14 の主張とは別件で、
warning の flush 位置（B27 / E-7）の問題である。

### 7.2 route family ごとの cutover 順序

各 family の順序は「その family 内で先に畳まないと後段が特殊ケースを要求するもの」から並べる。
「なぜこの順序が qpdf の呼び出し順を壊さないか」は §5 の該当不変条件で説明する。

#### 7.2.1 resolver（領域 A）

1. **A2**（`crates/flpdf/src/cache.rs::ObjectCache` / `crates/flpdf/src/cache.rs::CacheEntry` の legacy 二重帳簿）を
   `crates/flpdf/src/reader/resolver.rs::ResolverCore`（A1）へ畳む。前提: probe A-1（差分列挙）。
2. **A15 / A24** — A2 の同期層（`crates/flpdf/src/reader.rs::Pdf::synchronize_cache_with_resolver_xref`）と
   ObjStm メンバー昇格経路。1 が終われば消える。単独では畳めない。
3. **A9 / A10 / A11** — 列挙 3 本と採番。
4. **A13 / A14 / A17** — tombstone に触る変異 API。**A14 は `.46` で完了し、
   `replaceObject(og, newNull())` に相当する A16 の canonical routeへ統合した。**
5. **A6 / A8 → A7** — 非解決アクセサ族を `try_*` 族へ寄せてから `crates/flpdf/src/reader.rs::Pdf::resolve` を落とす。
   逆順にはできない。
6. **A20** — teardown walk 2 本の統合。bootstrap 構造（A1）が 1 に依存する。

qpdf 呼び出し順を壊さない理由: §5.A 第 3 行（「object cache に永続 tombstone は存在しない」）が
1〜4 の責務を分ける。`qpdf_removed_refs` は insert 0件の空集合だったため、`flpdf-3yn9.48.21` でfield・初期化・全filter・無効なremoveを撤去した。
qpdfのcanonical removeObject、xref/cacheの列挙と変異は既存ownerを使用する。A9/A10/A13/A16/A17に残るfacade責務は別sliceであり、分類集計はこの削除だけでは変わらない。
このdead filter削除は、bootstrap cache / trailer child登録のowner統合から独立に進められる。§5.A 第 5 行（採番は `getObjectCount()+1` の 1 本）が
A11 を A10 の後に置く理由で、facade の `next_available_object_ref` は A10 の結果と canonical の max を
取るため A10 が 1 本にならないと採番が確定しない。§5.A 第 1 行・第 2 行（`getObject` は resolve しない /
型アクセサは必ず dereference する）が 5 の向きを決める — A6 が解決するようになって初めて
`pdf.resolve(&h)?;` → `h.as_dictionary()` の 2 段イディオムが冗長になる。

#### 7.2.2 parser・diagnostics（領域 B）

1. **B5 / B7 の欠落primitive** — document-owned ParseGuardとreadToken責務を先に移植し、
   再入拒否・guard復元・ObjStm headerの2 token読取順を固定する。xref ByteCursorのfalseを一律trueにしない。
2. **B29** — 既存warning collectionへgetWarningsのdrain / anyWarningsを移植し、Job完了consumerから移す。
   `num_warnings` は既存。loggerへ表示済みのwarningをdrain時に再出力しない。
3. **B8〜B13 / B34** — canonical file-object/header/stream/trailer責務へbootstrap consumerを移す。
   B10のEOL warning、B11のrecovery、B13のreadTrailer、B34のfallback撤去をbounded sliceに分ける。
4. **B22 / B25 / B20** — canonical reconstructと3種のxref登録primitiveへconsumerを順次移行する。
   B20のdeleted_objects抑止primitiveは全reconstruct統合を待たずに移植できる。
5. **B27 / B28 / B30 / B32 / B33** — bootstrap handoff・warning配送・例外分類を同じownerへ寄せる。
   warning順序とresolve境界のwarn/null降格を各sliceで検証する。
6. **B14** — 自己参照Prevの既存probeでは二重warningは再現しない。visited初期化の変更を先に決めず、
   他のchain条件と読取順を確認する。

warning collectionやtoken primitiveの移植を、領域A全体の統合完了に依存させない。
必要なhandle identity / parser context前提をconsumer単位で切り出す。

#### 7.2.3 stream（領域 C）

1. **hygiene**（§7.4）— C19 / C28 の dead route 削除は `.42` で完了。C23 の `pub` 撤去は `.43` で完了。
2. **C21 の辞書差** — `/F` `/FFilter` `/FDecodeParms` 削除の到達条件をoracle fixtureで固定し、qpdfの責務に沿って修正する。逸脱マーカーは修正完了の代替にしない。
3. **C22** — C-U3 library harnessでplain/QDF cacheとlinearized probeの挙動を照合する。非対称だけで早期return撤去を決めない。
4. **C42 / B11** — EOL 差し引きの決着は `flpdf-zvjf`（pipe）と `flpdf-hj7v`（show-stream）で完了。`RecoveredStreamEol` は `dump-object` 再シリアライズ専用として残る。前提: probe C-U1。
5. **C27** — bootstrapのdocument/source ownerを整えてからxref stream payloadのdecodeをcanonical pipeへ移す（`flpdf-3yn9.48.43`）。B17のxref entry構文処理の正本とは別責務である。
6. **C44** — 未実装public `getStreamJSON` / deferred `StreamBlobProvider`を別primitiveとして移植する。C-U2は直接APIをprobeする。canonical C24 `write_stream_json` を二重pipeへ変更しない。
7. **C4 / C8 / C9 / C25〜C29 / C43、E-27 / E-28** — provider/copy/decodeの不足primitiveを明示し、xrefとdriver test 0/1など既知consumerからbounded cutoverする。

qpdf 呼び出し順を壊さない理由: §5.C 第 4 行（`willFilterStream` の判定順序）が 2 と 3 を
C20 / C21 の**後ろ**に置く理由 — 判定順序の canonical owner が確定していない状態で早期 return を
外すと、veto → metadata / normalize / compress の排他 chain が経路ごとに別の結果になる。
§5.C 第 1 行（stream の復号は pipe 時、文字列の復号は parse 時）が 4 と 5 の境界で、
decode 経路を canonical `pipe_stream_data` へ寄せても復号のタイミングは動かないことを保証する。

#### 7.2.4 encryption（領域 C の暗号化行 + D15 / D16 / D17）

1. **probe C-U4 完了** — `crates/flpdf/src/encryption/primitives.rs::compute_data_key` の
   qpdf oracle vectors が、V={1,2,4,5}・R=6固定、key長5/16/24/32、AES/RC4、非zero generationを固定した。
   `encryption_R` は qpdf原典でも未使用であり、reader/writerの別実装比較は不要（`libqpdf/QPDF_encryption.cc:324-357`）。
2. **C19 削除**は`.42`で完了。
3. **C18 → C17 完了** — 2 実装を `encryption/primitives.rs` の 1 本へ統合。reader cache と writer generation 0 consumer は維持。
4. **D15 / D16** — D15の暗号辞書emissionは維持し、D16の暗号setup/stateの分裂を
   qpdfのdoWriteSetup責務へ統合する（`flpdf-3yn9.48.62`）。共有鍵と分岐前setupを前提にconsumerごとに移す。

qpdf 呼び出し順を壊さない理由: §5.C 第 6 行（`compute_data_key` は読み書きで同じ 1 実装を共有する）が
1〜3 の順序そのもので、等価テストを先に置かないと統合の正しさを事後に確かめる手段が無い。
X-5 のとおり D17（set / unparse / clear の順序）は既に canonical なので、鍵計算を 1 本にしても
呼び出し順は動かない。§5.D 第 4 行（encryption dictionary は body 全 object の後・xref の直前）が
4 を最後に置く理由。

#### 7.2.5 writer（領域 D）

writer全体を最初のsliceから除外する根拠はない。D12のoracle契約は明確で、共有primitiveを
consumer単位に移植できる。採番・body loopの大規模統合を全作業の前提にはしない。

1. **D12 / D13 / D14** — `writeXRefTable` / stream / trailerの共有primitiveを移植し、
   plain等の1 consumerから接続する。D12の欠番/type≠1は `Error::Internal`、
   object 0とpass-1 `suppress_offsets`はqpdf通り別分岐にする。producer調査D-U3は並行可能。
2. **D2 / D3 / D11** — enqueue時のcontainer-first採番と共有object emissionを復元する。
   standard/PCLmのbody loopとlinearizedの2 passはqpdfでも別であり、その境界を保つ。
3. **D5 / D6 / D8 / D31** — 共有Preserve membershipからlinearized consumerを導く。
   source-index順を保つ現経路とqpdfのobjgen順の差をD-U1で固定する。
4. **D25 / D26** — 分岐前prepare/setupとlinearization固有optimizeの責務を復元する。
   subset/snapshotの意味をconsumerごとに照合する。
5. **D20** — hint最終値のproducerは確認済み。staleコメントを訂正し、既存 `flpdf-o99` で現行writerの統合試験を確認する（D-U6）。

D27の全pre-write sweepとfollow-upは完了済み。D19 / D30はbyte-neutral test scaffoldingで、
削除専用sliceを作らない。既存strict Preserve byte testsの存在は確認済みだが、今回の文書更新では
合否を再測定していない。

#### 7.2.6 job（領域 E）

1. **E-29（完了）** — `open_with_description`、`open_document_with_description`、
   `open_for_encryption_inspection_with_description`、`open_job_source`、JSON seed の各入力境界で
   `suppress_warnings` を open 前に OR / 適用した（`libqpdf/QPDFJob.cc:663-665`）。CLI 直書きの
   `Pdf::open_with_options` / `Pdf::create_from_json` は通常 route から消えたが、direct
   `Pdf::open_with_options` は2つの exception route に残る: reopenable source を必要とする
   `open_page_source`（`open_file_with_options`）、および qpdf の `copyAttachments`
   （`libqpdf/QPDFJob.cc:2100`、donor を `processFile(other, ...)` で job 本体の main input
   slot と独立に開く）に対応する `run_copy_attachments_from` の attachment donor open
   （donor を job 経由で開くと `job.input_name()` が donor のパスで上書きされ、後続の
   duplicate-key エラーが target ではなく donor を誤って名指すため、意図的に job 非経由）。
   どちらも同じ `suppress_warnings` option を open 前に渡す。
   `crates/flpdf-cli/tests/cli_no_warn.rs` が ordinary・secondary・JSON・split-pages route と
   qpdf の warning delivery / exit status を比較する。
2. **E-19 / E-7** — `complete` / exit code の 1 回判定化。前提: probe E-P3。
3. **E-2 / E-3** — `create_qpdf` の内側へ変換 5 段を戻し、3 分岐を `write_qpdf` の内側へ移す。
   前提: probe E-P1。
4. **E-4 / E-10 / E-21** — CLIの出力・page/source orchestrationを `QPDFJob` へ寄せる。`flpdf-hxmj` の限定sliceはclosedで、残consumerの完了を意味しない。前提: probe E-P4。
5. **E-15 / E-17** — argv/Configのrange syntax検証を共有 `parse_numrange(max=0)` へ寄せ、page count判明後に実値を展開する。qpdfにもsyntax-only modeがある（`libqpdf/QPDFJob_argv.cc:240-272`）。
6. **E-9 / E-24 / E-26 / E-14** — 可視性と命名をconsumer移行とともに整理する。closedの既存sliceを未完了前提に戻さず、残るsurfaceを区別する。
7. **E-27 / E-28** — test 0/1の既知stream warning bridgeから移行する。未照合case/APIだけを追加調査し、A〜D全行の確定を待たない。

qpdf 呼び出し順を壊さない理由: §5.E 第 4 行（入力は必ず `doProcessOnce` 経由で開き、
`QPDF` 構築直後に `setQPDFOptions` を適用してから読む）が 1 を最初に置く理由で、
読み込み側の option 適用が 3 経路で違ったままだと 3 / 4 の統合後にどの経路の挙動が正だったか
判別できなくなる。§5.E 第 1 行（`run()` は `createQPDF` → `writeQPDF` の 2 呼び出しだけ）と
第 2 行（分岐は `writeQPDF` の内側・判定は `createsOutput()` 1 個）が 3 を 4 の前に置く理由 —
public 2 段契約を先に正さないと、CLI を寄せる先の意味が qpdf と違う。
§5.E 第 6 行（exit code は溜めて 1 回だけ判定）が 2 を 3 の前に置く理由。

### 7.3 最初の bounded cutover: D27 の pre-write sweep 撤去（完了）

**対象だったもの**: 旧 `sweep_unreachable_objects` route の
production consumer 3 つすべて。3 consumer と旧 wrapper の撤去、writer 出力の byte gate、
依存テストの移行まで完了した。着手順は oracle 付きの 2 つを先に落とす順序だった。

| # | target consumer | 経路 | qpdf oracle |
|---|---|---|---|
| 1 | `crates/flpdf/src/job/page_subset.rs`（旧 consumer） | 単一 source `--pages`。ページ選択後の resource pruning は残し、文書全体の到達性は writer に委譲した | あり（下の byte gate + control 2 本） |
| 2 | `crates/flpdf/src/embedded_files.rs`（旧 consumer） | library 専用。name-tree entry の除去と Filespec の null 化だけを行い、後段 writer の preserve policy に委譲した | あり。qpdf の `removeEmbeddedFile` は name tree から外して `replaceObject(og, newNull())` を呼ぶだけ（`libqpdf/QPDFEmbeddedFileDocumentHelper.cc:105-121`） |
| 3 | `crates/flpdf/src/page_extract.rs`（旧 consumer） | library 専用。新しい document の構築だけを行い、construction-only object の出力判定は writer に委譲した | **なし。** `extract_pages` 自体は qpdf に対応物のない flpdf 固有のライブラリ機能だが、qpdf 非対応の pre-write pass を追加しないという D27 の責務境界で整理した |

`.45` の multi-source follow-up も同じ責務境界で完了した。`QPDFJob::handlePageSpecs` は
一次文書を保持したまま選択ページを foreign-copy し、非選択ページを null に置くだけなので、
flpdf の `--preserve-unreferenced` primary-object copy 後に独立した in-memory sweep は不要である。
`sweep_unreachable_objects_except` の module と caller は撤去し、writer に到達性判断を委譲した。
D3/D11 の採番差は別責務として残るため、multi-source の検証は object 数・内容 control とする。
`crates/flpdf-cli/tests/cli_preserve_unreferenced_pages.rs` の
`multi_source_pages_preserve_orphan_reference_to_primary_catalog_resolves_to_target_catalog` は、
qpdf 11.9.0 の QDF と object 数・参照番号を正規化した object 内容を比較する。

**明示的な非対象**:

- A14 は `.46` で完了。public deletion facade `Pdf::delete_object` は撤去し、qpdf の
  `replaceObject(og, newNull())` に対応する `Pdf::replace_object` を正本にした。
- D2 / D3 の採番実装。本 cutover は「到達性判定を writer の enqueue walk に任せる」だけで、
  enqueue walk 自体には手を入れない。

**qpdf 側の根拠**: qpdf の writer には独立した削除パスが無い。到達性は
`QPDFWriter::enqueueObject`（`libqpdf/QPDFWriter.cc:1072-1141`）が書き込み時に決め、
`--preserve-unreferenced` はその判定を `enqueueObjectsStandard` が
`getAllObjects()` 全件を先に enqueue することで上書きする（`libqpdf/QPDFWriter.cc:2909-2914`）。
`handlePageSpecs` が非選択ページに対して行うのは
`pdf.replaceObject(page.getObjectHandle().getObjGen(), QPDFObjectHandle::newNull())` だけで
（`libqpdf/QPDFJob.cc:2596-2608`）、object を cache から消しはしない。
`--remove-unreferenced-resources` が動かすのはページごとの `/Resources` 剪定だけである
（`libqpdf/QPDFJob.cc:2443-2445`、`libqpdf/QPDFJob.cc:2540-2550`）。
flpdf の pre-write sweep はこのどれにも対応しない追加処理だった。single-source と
multi-source の両方で保存すべき object を writer に渡る前に消してしまわないよう、
対象 route を撤去した。

#### RED differential test（cutover 前の記録、現在は GREEN）

- **fixture**: `tests/fixtures/compat/d27-two-page-distinct-resources.pdf` の 2 ページ
  classic xref PDF。両ページが**共有しない** `/Contents` stream と `/Font` を 1 つずつ持つ
  （object 1 = Catalog, 2 = Pages, 3 = Page1, 4 = Page1 contents, 5 = Page1 resource
  dictionary, 6 = Page1 font, 7 = Page2, 8 = Page2 contents, 9 = Page2 resource dictionary,
  10 = Page2 font）。ページ 1 だけを選ぶことで object 7 / 8 / 9 / 10 が非参照になる。
  共有しない構成が必要なのは、`--remove-unreferenced-resources` の `auto` 判定が
  「共有 resource があるときだけ剪定する」ためで、`=yes` を明示しないと sweep 自体が走らない。
- **qpdf command**:
  `qpdf --static-id --preserve-unreferenced --remove-unreferenced-resources=yes --pages . 1 -- two.pdf q.pdf`
- **flpdf command**:
  `flpdf rewrite --static-id --preserve-unreferenced --remove-unreferenced-resources=yes --pages . 1 -- two.pdf f.pdf`
- **比較方法**: `qpdf-zlib-compat` feature でビルドした flpdf による出力ファイルの byte compare
  （出力に `/FlateDecode` が含まれるため、既定の miniz_oxide ビルドではこの比較は成立しない
  — CLAUDE.md 逸脱分類 (A)）。
- **cutover 前に FAIL したことの証拠**:
  `cmp` は qpdf と flpdf で差分を報告した。同じ 2 コマンドを `--qdf` 付きで実行すると、
  qpdf 側は非選択ページの slot を `null` として残し、page 2 の contents / resource dictionary /
  font を中身ごと保存するのに対し、旧 flpdf 側の pre-write sweep はその非参照 subgraph を
  writer に渡る前に削除した。
- **cutover 後の期待値がこの qpdf 出力であることの裏取り（control 2 本）**:
  1. `probe: qpdf --static-id --preserve-unreferenced --pages . 1 -- two.pdf` と flpdf 同等
     （`--remove-unreferenced-resources=no` を明示する。既定 `auto` もこの fixture では `No` に落ちる —
     `crates/flpdf/src/job/page_specs.rs:191-197`。`no`/`auto` では `prune_after_subset` が早期 return し、
     `/Resources` の刈り込みと sweep が同時に止まる）→ **双方 1045 バイトで byte 一致**。
     つまり sweep を走らせない経路では、preserved orphan の採番・出力が既に qpdf と一致している
     （control 1 と 2 を合わせて、撤去後の期待値が qpdf 出力であることを裏取りする）。
  2. `probe: qpdf --static-id --remove-unreferenced-resources=yes --pages . 1 -- two.pdf` と flpdf 同等
     （`--preserve-unreferenced` なし）→ **双方 684 バイトで byte 一致**。
     sweep は `--preserve-unreferenced` を付けたときにしか観測されない = 撤去の影響範囲が
     この 1 フラグに限られる。
- fixture は `tests/fixtures/compat/d27-two-page-distinct-resources.pdf` に追加済み、byte test は
  `crates/flpdf/tests/cmp_preserve_unreferenced_sweep_tests.rs`（`qpdf-zlib-compat` gated）に置き、
  `.github/workflows/ci.yml` の bytes-identical テスト列挙にも追加済み。

#### 完了判定

§6.4 の (a) の 2 条件をそのまま使う。

1. `python3 scripts/qpdf-route-callers.py --root . --symbol sweep_unreachable_objects --expect-zero`
   が exit 0 を返す（single-source の旧 symbol 自体を削除済み）。
2. single-source の
   `crates/flpdf/tests/page_extract_outline_nullout_tests.rs` /
   `crates/flpdf/tests/page_extract_structtree_pg_tests.rs` /
   `crates/flpdf/tests/page_subset_job_route_tests.rs` /
   `crates/flpdf/tests/page_extract_thread_bead_p_tests.rs` のうち
   sweep 後の `live_object_refs()` に依存しているものが、writer の出力バイトを見る形へ
   書き換わっているか削除されていること。
3. 上の RED byte test が GREEN になること。
4. `python3 scripts/qpdf-route-callers.py --root . --symbol sweep_unreachable_objects_except --expect-zero`
   が exit 0（prod 0 / test 0）を返し、`cargo test -p flpdf-cli --test
   cli_preserve_unreferenced_pages` の multi-source preserve control 5 件が GREEN になること。

#### D27 follow-up: linearization prepass（`flpdf-3yn9.44.1`）

親 cutover で pre-write sweep を撤去した結果、linearized planning に残っていた
`stream_refs_to_skip_parameter_edges` の全 object 走査が、到達不能 stream の解決という
別の qpdf mismatch を露呈した。この follow-up では、qpdf 11.9.0 の
`QPDF::optimize`（`libqpdf/QPDF_optimization.cc:57-118,261-338`）と
`QPDFWriter::writeLinearized`（`libqpdf/QPDFWriter.cc:2537-2561`）に合わせ、
stream-parameter の probe と skip 記録を既存の `Optimization::optimize` callback 内へ
移した。callback は page、trailer key、Catalog root-key の mark walk から呼ばれるため、
`pdf.object_refs()` を全件 resolve する standalone prepass は存在しない。linearization
object universe のフィルタも到達性判定を先に行い、discarded object の型確認のために
resolve しない。

回帰 fixture は `crates/flpdf/tests/linearization_unreachable_stream_tests.rs`。
qpdf 11.9.0 の `--linearize` が成功する一方、unreachable stream の間接 `/Length`
holder を読むとだけ失敗する reader を使い、flpdf の linearized write が成功し、生成物を
qpdf `--check` が受理することを確認する。qpdf source からの到達境界、実機 qpdf の成功、
flpdf の RED→GREEN test はすべて同一 fixture で再実行可能である。

#### 次の cutover へ進む条件

1〜3 がすべて満たされ、かつ
`python3 scripts/qpdf-route-callers.py --root . --symbol sweep_unreachable_objects_except --expect-zero` が
prod 0 / test 0 を返すこと。`.46` では `delete_object` も prod 0 / test 0 を確認し、
A14 を完了した。D27 の後続 cutover も完了し、D27 と独立に進めていた hygiene 2 slice を
継続する。E-29 は `.47`（§7.2.6 の 1）で完了済みである
（qpdf の `QPDFAcroFormDocumentHelper::disableDigitalSignatures` は `/FT` `/V` `/SV` `/Lock` の
キーを消すだけで signature dictionary を削除しない、`libqpdf/QPDFAcroFormDocumentHelper.cc:418-439`）。
逆に 1 が満たせない（consumer 2 / 3 の test 移行が想定より重い）と分かった時点では、
以降の候補は§7.2の再監査順序で判断する。D27の完了済み依存を再開しない。

### 7.4 前提 / 後続 issue

いずれも親 epic は `flpdf-3yn9`。既存 issue は重複させず参照する
（`flpdf-xsq1` 可視性 debt / `flpdf-7bkv` json_inspect の staged migration /
`flpdf-ei0h` 命名 / `flpdf-hxmj` CLI `--pages` 経路統合）。

| 位置づけ | 内容 | issue ID |
|---|---|---|
| 完了（hygiene、ゼロリスク） | §6.4 (a-i) の prod 0 かつ test 0 だったC19/C28/E-27のdead route 4 symbolを削除し、`crates/flpdf/src/encryption/keys.rs` の `#![allow(dead_code)]` を外した。ゲートは各 leaf の `--expect-zero` | `flpdf-3yn9.42` |
| 完了（hygiene、test 移行あり） | §6.4 (a-ii) の C23/E-9 test-only route を canonical `PdfWriter`/`QPDFJob::list_attachments` 経由へ移して削除。`AttachmentInfo` と `format_attachment_list_with_sink` の可視性判断は `flpdf-xsq1` に残す | `flpdf-3yn9.43` |
| 本体 | 最初の bounded cutover（§7.3）。D27 の pre-write sweep 撤去 | `flpdf-3yn9.44` |
| 完了（D27 の multi-source follow-up） | `sweep_unreachable_objects_except` とその module を撤去。D3/D11 の採番差が残るため、`--preserve-unreferenced` multi-source `--pages` は object 数・内容 control で検証し、byte gate は採番統合後に行う。A14 の着手条件を満たす | `flpdf-3yn9.45`（`flpdf-3yn9.44` に依存） |
| 完了 | A14 `Pdf::delete_object` の撤去と `replaceObject(og, newNull())` への cutover。§7.2.1 の 4 | `flpdf-3yn9.46`（`flpdf-3yn9.45` に依存） |
| 完了 | E-29 の `suppress_warnings` を全入力 route の open 前に適用（§7.2.6 の 1）。RED/GREEN と qpdf route 比較は `cli_no_warn.rs` | `flpdf-3yn9.47` |
| 記録の訂正 | D27 行の旧 `sweep_unreachable_objects` を `_except` と writer owner に張り替える件。B14 の「診断を二重に push する」という主張が probe で再現しない件（§7.1）。B27 / E-7 の warning flush 位置（qpdf は `checking <file>` の前、flpdf は後）が本表のどの行にも記録されていない件 | issue 化せず本 PR 内で反映済み — d-writer.md D27 行、`tracked-symbols.txt` の `_except` 行、D27 byte gate、B14 行と P1 の probe 結果、README §7.1のwarning flush記録 |

### 7.5 全残経路の closure backlog（2026-09-06）

親 epic **`flpdf-3yn9.48`** に、文書更新1件（`flpdf-3yn9.48.1`）と
実装・調査64件（`flpdf-3yn9.48.2`–`flpdf-3yn9.48.65`）を登録した。
各領域末尾の「再監査の issue 対応」から行IDごとの担当issueを引ける。
既存open issueは再利用し、状態をここに複製せず `bd show <id>` で確認する。

| 領域 | 新規issue範囲 | 分割した責務 |
|---|---|---|
| Job / CLI | `flpdf-3yn9.48.2`–`flpdf-3yn9.48.11` | range正本、create/write契約、argv/Config、CLI consumer別切替、driver case監査 |
| ObjectHandle / parser / diagnostics | `flpdf-3yn9.48.12`–`flpdf-3yn9.48.36` | parse前document state、bootstrap、parser mode/guard、xref回復、typed例外とdrain、identity/accessor、最後の旧cache/dirty/API削除 |
| Stream / filter / encryption | `flpdf-3yn9.48.37`–`flpdf-3yn9.48.49` | stream warning、ObjStm例外、共通鍵、Form/EF/bootstrap/qtest移行、registry/getStreamJSON、最後のdecoder/encoder削除 |
| Writer | `flpdf-3yn9.48.50`–`flpdf-3yn9.48.65` | source map、container identity、候補walk、live queue、共有emission/xref/trailer、setup/ADBE、残consumer移行 |

依存は qpdf の責務で設定した。例:
document state → bootstrap reader/ObjStm → rebind/replay撤去、
stream warning → test 0/1 pipe/logger → source再parse/64回budget撤去、
共有writeObject → plain Disableのlive queue → 残writer cohort移行 → facade cache/dirty撤去。
各issueは最初のconsumerを限定し、残consumerを別の依存PRで進める。
型名が同じだけの一括置換、旧bridgeへの意味論追加、marker追加だけの完了は認めない。

既存 `flpdf-1far` はappearance token-filter移行、`flpdf-44hb` はdonor処理順、
`flpdf-xsq1` / `flpdf-ei0h` は公開surface/命名、`flpdf-oq7g` / `flpdf-o99` は
Preserve/hintの受入確認として接続する。closed `flpdf-hxmj` / `flpdf-7bkv` や
D27関連issueを未完了前提として再登録しない。

この更新はsource/caller監査とbacklog整理であり、全経路parityの達成ではない。
残るB5の観測は `flpdf-3yn9.48.17`、未照合driver caseは `flpdf-3yn9.48.11` が所有する。
監査中に `flpdf-h6fe` がPR #1569（`0f208a59`）でmergeされたことも確認した。
canonical readStreamのwarning context修正は済作業として扱い、bootstrap経路統合の
`flpdf-3yn9.48.13` ではその回帰を引き継ぐ。行番号とcaller snapshotの基準は冒頭の監査commitである。


## 8. unknown と必要 probe 一覧

### 8.1 ID 表記について

領域ファイルの probe ID は **そのままでは衝突する**: C は `U1`–`U4`、D は `U1`–`U6` を使い
`U1` が 2 つの別物を指す。B は `P1`–`P8`、E は `P-1`–`P-4` でハイフン 1 つしか違わない。
A は接頭辞なしの `1`–`4`。本節では領域接頭辞を付けて `A-1` … `E-P4` と表記する（全 26 件）。

### 8.2 unknown 行（1件）と必要probe

**B5 / B-P5**だけが行単位でunknown。qpdfのdocument-owned `ParseGuard` は
`libqpdf/QPDF.cc:476-485` / `libqpdf/QPDFParser.cc:29-34` にある。
通常parseがresolveしないことは省略理由にならない。再入triggerと正常/異常return時のguard復元を
oracleで固定し、未移植primitiveをfile/content parserの前提にする。

B29 / D31 / E-28はsourceでmixedと判定した。E-28の未照合case/APIは引き続きunknownであり、
行分類の確定を全caseの照合完了とは扱わない。C42は以前のpipe/show-stream修正完了を反映した。

### 8.3 分類済みのprobeと既存結果（25件）

手順の詳細とsource引用は各領域表の同じprobe IDを参照する。「確認済み」「完了」は
記録済みsliceの範囲を指し、この文書更新で全parity suiteを再実行したという意味ではない。

| ID | 状態 | 影響する行 | 次の確認 / 保持する契約 |
|---|---|---|---|
| A-1 | 未観測 | A2/A10 | legacy/cache列挙差をfixture集合で比較し、bootstrap identityの前提を切り出す。常に空だったremoved-ref集合は `flpdf-3yn9.48.21` で撤去済みで、観測差の根拠ではない。 |
| A-2 | 未観測 | A12 | qpdf/RustでmakeIndirectObject後に元handleのarray/dictを変更し、aliasが保たれるか比較する。 |
| A-3 | 内部契約確認 | A13 | removeObjectのcache eraseとowner切断をinternal witnessで確認する。削除済みpublic wrapperをprobe前提に戻さない。 |
| A-4 | 未観測 | A1/A20 | bootstrap handleの持越しidentityとdisconnect順序を確認する。 |
| B-P1 | 既存probeで二重warningなし | B14 | 自己参照Prevでは双方同じ3 warning・exit 3。追加chainを調べ、二重pushを既知事実として扱わない。 |
| B-P2 | consumer調査 | B20 | 初段parse失敗handoffのdeleted_objects状態と登録primitiveの抑止をoracle fixtureで照合する。 |
| B-P3 | 未観測 | B22 | 再構築後compressed entryのresolveを比較し、qpdfのwarn/nullとRustの例外境界を固定する。 |
| B-P4 | 未観測 | B27 | bootstrap handleとxref双方のwarningを出すfixtureでcollection/delivery順を比較する。 |
| B-P6 | owner確認済み | B29 | QPDF.cc:345-363のdrain/anyWarningsを同じdocument collectionへ移植し、Job完了から移す。num_warningsは既存。 |
| B-P7 | owner対応確認済み | B7 | ObjStm headerの2 token読取をQPDF::readTokenへ対応付ける。classic xrefのByteCursorはreadLine/parse_xrefEntry責務であり一律trueへ変えない。 |
| B-P8 | 一部旧記述訂正 | B13/B17 | unknown xref stream entry typeは現src/testに存在する。stream keyword found in trailer等の残条件を個別fixtureで照合する。 |
| C-U1 | 既存sliceで解決 | C42 | flpdf-zvjf/flpdf-hj7vのrecovered full-length pipe/show-streamを維持する。dump-objectのframing metadataは別責務。 |
| C-U2 | API欠落確定 | C44 | 直接getStreamJSONとdeferred blobのC++ harnessでprovider回数とlifetimeを固定する。CLIのwriteStreamJSON/C24を二重pipeへ変更しない。 |
| C-U3 | 未観測 | C22/C39 | token filter/providerを登録したlibrary harnessでplain/QDF cacheとlinearized optimizer probeを比較する。非対称だけでbugとはしない。 |
| C-U4 | 完了 | C17/C18 | pinned qpdf headerをincludeしたC++ oracle probeと32固定vectorで、V/R・key長5/16/24/32・AES/RC4・非zero generationを確認し、単一primitiveへ統合した。 |
| D-U1 | mixed確定・出力差を追加確認 | D6/D31 | source-index順のlinearized Preserveと共有membershipのobjgen順を比較する。既存strict Preserve byte testsを利用する。 |
| D-U2 | sourceで解決 | D26 | normalized_streamsの参照はqpdfのnormalize_content条件内に限られる（`libqpdf/QPDFWriter.cc:1279`）。decode-onlyでそのsetが無いこと自体は差にならないが、page修復triggerとsetupの3map共有は移植対象。 |
| D-U3 | oracle契約確定 | D12 | 欠番/type≠1をError::Internalにするprimitive testとproducerの欠番到達調査を分ける。fake free rowの選択問題ではない。 |
| D-U4 | scaffoldingとして判定済み | D19/D30 | canonical writerに委譲するbyte-neutral test helper。D19のback-patch前観測点を保持し、削除専用issueは不要。 |
| D-U5 | 完了 | D27 | single/multi-source双方のsweepがRust全域0 hit。新たなsweep cleanupは不要。 |
| D-U6 | sourceで解決 | D20 | `crates/flpdf/src/linearization/writer.rs:4152` がmember/container mapを渡し、`crates/flpdf/src/linearization/hint_shared.rs:311` が最終番号を算出、writerはlocationのみ更新する。最終値のproducerは存在し、convergence-loopコメントはstale。 |
| E-P1 | public契約の追加観測 | E-1/E-2 | 対応済みConfigまたはinitialize_from_jsonで変換設定を作り、create後の状態→追加変更→write出力を比較する。未対応argv rotateでprobeを止めない。 |
| E-P2 | mixed確定・case別調査継続 | E-28 | 既知test 0/1 bridgeから移行し、imports/型経由method/呼出順も含め未照合caseをownerへ対応付ける。 |
| E-P3 | 未観測 | E-7/E-19 | 複数inspection指定でwarning collection・stderr・exit codeをqpdfと比較する。 |
| E-P4 | source/consumer調査 | E-4/E-21 | writeOutfile内のreplace-input rename/backupとcloseInputSource順を現finish_replace_inputに対応付ける。 |

### 8.4 領域間の矛盾・境界（合成時に判明したもの）

領域ファイルは 5 人が独立に書いたため、同じ symbol / 同じ qpdf 関数が領域をまたいで
別々に分類されている箇所がある。**どちらかを選ばずに、両方を記録する**。

| ID | 何が食い違うか | 両側の主張 | 解決に要ること |
|---|---|---|---|
| **X-1** | `crates/flpdf/src/reader/resolver.rs::recover_stream_length` の実装本数 | **B11 は `mixed`** — 「2 実装」。もう 1 本は `crates/flpdf/src/reader/file_object.rs::recover_stream_boundary` で、qpdf の `attempt_recovery` 1 bit（`libqpdf/QPDF.cc:1391`）を `RecoveryPolicy`（`RequireEndstream` / `Bounded`）という 2 値の別概念に置き換えている。C42 の pipe-side EOL subtraction は `flpdf-zvjf` で削除済み | B11 は recovery boundary の実装数、C42 は recovered length を pipe する責務として読む。残る `RecoveredStreamEol` は `dump-object` 再シリアライズの framing metadata であり、pipe length や show-stream payload の補正ではない |
| **X-2** | C42 が要求した領域跨ぎの確認が B 側で行われていない | C-U1 は `flpdf-zvjf` と `flpdf-hj7v` で解決済み。B11 の `recover_stream_boundary` は xref bootstrap の raw stream framing、C42 の `recover_stream_length` は canonical resolver の source length を担い、show-stream はその payload を無加工で出す。暗号化 canonical pipe はどちらも caller の length を変更しない | C-U1 の qpdf AESv2 probe・unencrypted show-stream probe・canonical/foreign/writer tests で、recovery metadata が pipe-side subtraction や show-stream trim に戻らないことを確認する |
| **X-3** | `QPDF::readStream` の分類が領域で逆 | **C41 は `canonical`**（`/Length` 検証 + `endstream` 確認を `crates/flpdf/src/reader/resolver.rs::read_stream` 1 本が持つ）。**B10 は `mixed`**（`validateStreamLineEnd` の 3 warning が `crates/flpdf/src/reader/resolver.rs::validate_stream_line_end` と `crates/flpdf/src/reader/file_object.rs::finish_file_object_handle` の 2 実装にある） | 粒度違いで両立する（同じ qpdf 関数の別部分を見ている）。ただし **C41 だけを読むと `readStream` が完全に片付いて見える**。cutover 時は B10 の 2 実装を先に畳む |
| **X-4** | xref stream の読み出しが 2 領域で別分類 | **B17 は `canonical`**（`crates/flpdf/src/xref.rs::parse_xref_stream` 1 本）。**C27 は `mixed`** で、その production caller として `crates/flpdf/src/xref.rs:752` と `crates/flpdf/src/xref.rs:3266` を挙げる（whole-buffer の `decode_stream_data_from_handle` 経由）。両者とも qpdf 側は `libqpdf/QPDF.cc:1051` の `getStreamData(qpdf_dl_specialized)` に対応する | 「xref stream を parse する経路」は 1 本でも、「その payload を decode する経路」は canonical な `pipe_stream_data` を通っていない。C27 の 2 caller を canonical へ寄せても B17 の判定は変わらないので、**独立に進められる** |
| **X-5** | writer 側 data key が D では canonical、C では mixed | **D17 は `canonical`**（`crates/flpdf/src/writer/encryption_state.rs::WriterEncryptionState` が set / unparse / clear の順序を写す）。**C18 も `canonical`**（`encryption/primitives.rs::compute_data_key` の共有 primitive を呼ぶ） | 順序と鍵計算をそれぞれ qpdf 責務どおり保持し、C-U4 oracle vectors の後に duplicate を削除した |
| **X-6** | 5 ファイルの caller 数え方の細則が一致していない | **A ファイル**は「D の『モジュール直下の最初の `#[cfg(test)] mod` より前＝prod』という単純化は本領域では使えない」と明記する（`object_handle.rs` は桁 0 の `#[cfg(test)] mod` を 21 個持ち間に production コードが挟まる）。**D ファイル**はその単純化を採用している。B / C / E はさらに別の細則を書いている | `scripts/qpdf-route-callers.py` は A 側の brace 追跡規約を実装している。**以降の再測定は tracker を唯一の規約とする**（§6）。D の行セルが tracker と最も乖離するのはこの差が原因（§6.3） |
| **X-7** | 行の完了とwriter全体の完了の区別 | A14はreplaceObjectのcanonical routeへ移行済み。D27もsingle/multi-source sweepを撤去済みでcanonical | D27の到達性削除pass撤去は完了だが、D2/D3/D11の採番・emission統合は別責務として追跡する |

# D. writer — reachability, ObjStm planning / renumber / emission, xref / trailer, encryption, linearize

対象: `QPDFWriter` の `enqueueObject` / `enqueueObjectsStandard`（採番の正本、container-first）、
`preserveObjectStreams` / `generateObjectStreams`、`writeObject` / `writeObjectStream`、
`writeXRefTable` / `writeXRefStream` / `writeTrailer`、`writeEncryptionDictionary` と
`setEncryptionParameters*`、`writeLinearized`（pass1 → hint → pass2）、`QPDF_optimization` /
`QPDF_linearization` の object universe。flpdf 側は `writer.rs`（`emit_canonical_pdf_inner` の
shared plain pipeline と legacy coordinator の分岐）、`writer/rewrite_renumber.rs`、
`writer/object_streams/*`、`writer/plain/*`、`writer/encryption_state.rs`、
`writer/encrypted_strings.rs`、`writer/pclm.rs`、`linearization/{writer,renumber,plan}.rs`、
`optimization.rs`。

**PR #1486（`flpdf-hi08`、`feature/flpdf-hi08-encrypted-preserve-objstm`）は 2026-09-04 に
merge 済み（`35233ba3`）。** 本表の作成時は in-flight だったため main（`8fd1a2bf`）+ #1486 として
記述し、#1486 由来の行・注記に `(#1486)` を残している。`crates/flpdf/src/writer.rs` の行番号引用は
作成時の main（`8fd1a2bf`、#1486 未適用）を基準にしており、#1486 の merge で `writer.rs:3888` 以降は
最大で数十行ずれる。引用の再アンカーは本表の保守項目（README §8 参照）。

2026-09-06 の再監査では `4a2faf5c` の Rust と pinned qpdf 11.9.0 を照合し、
D2/D5/D7/D11/D12/D19/D20/D25/D27/D29/D30/D31 と U1–U6 の責務・分類を更新した。
未更新行の caller 数・行番号は上記の旧 snapshot のままで、全行の再測定とは主張しない。
末尾の specialized 採番条件も同じ HEAD の実装に更新した。

**caller の数え方（本ファイル共通）:** `rg -n '\b<leaf>\b' crates --glob '*.rs'` の出力から
次の 5 種を除いた残りを、`src/` のモジュール直下 `#[cfg(test)] mod …` より前＝`prod`、
それ以降と `crates/*/tests/` ＝`test` として数える。除外するのは (a) コメント専用行、
(b) 宣言行（`fn`/`struct`/`enum` …）、(c) `use` 行 — **複数行 `use { … }` の継続行を含む**、
(d) `impl <Type>` のヘッダ行、(e) 文字列リテラル内の言及。
型位置での参照（引数型・戻り値型・フィールド型・パターン）は「呼び出し」ではないが実参照なので数に含める。
各行の実体は `sed -n` で確認済み。**2 つの罠を明示する**: (1) 項目単位の `#[cfg(test)]`（`writer.rs:31` の
`write_qpdf_to_memory`、`linearization/writer.rs:3093` の `write_linearized`）は
モジュール test 区画の外にあるので行位置では test 判定されない — これらは宣言自体が
test-only であり `prod: 0` とする。(2) `flpdf-cli/src/main.rs:334` は **同名で別シグネチャの
CLI ローカル `write_qpdf_to_memory`** を宣言しており、`main.rs:5806,6021` はそちらの呼び出しで
flpdf 側とは無関係。

## qpdf 責務モデル

pinned qpdf 11.9.0 の `libqpdf/QPDFWriter.cc`（3044 行）/ `include/qpdf/QPDFWriter.hh`（705 行）/
`libqpdf/QPDF_optimization.cc`（381 行）/ `libqpdf/QPDF_linearization.cc` / `libqpdf/QPDF.cc` を
`rg -n` と `sed -n` で読んだ範囲だけを引用する。引用した全関数の終端行は
「その行が単独の `}` であること」を機械的に確認した。flpdf のファイルは本節を書き終えるまで
開いていない。

### D-0. top-level 呼び出し順序（`write()`）

`QPDFWriter::write()`（`libqpdf/QPDFWriter.cc:2187-2213`）は次の順で固定:

1. `doWriteSetup()`（`libqpdf/QPDFWriter.cc:2059-2184`）— `did_write_setup` で 1 度だけ。
   - linearized なら `qdf_mode=false`。pclm なら decode none / compress off / `encrypted=false`。
   - qdf なら normalize / compress off / decode generalized の既定。
   - **encryption の優先順位**: 明示的に `encrypted` なら `preserve_encryption=false`。そうでなく
     normalize / decode / pclm / qdf のいずれかでも `preserve_encryption=false`。残った
     `preserve_encryption` の場合のみ `copyEncryptionParameters(m->pdf)`
     （`libqpdf/QPDFWriter.cc:651-702`）。
   - `forced_pdf_version` があれば `disableIncompatibleEncryption`、1.5 未満なら
     `object_stream_mode = qpdf_o_disable`。
   - qdf / normalize / decode のいずれかなら `initializeSpecialStreams()`
     （`libqpdf/QPDFWriter.cc:1912-1936`; page → `page_object_to_seq`、`/Contents` →
     `contents_to_page_seq` / `normalized_streams`）。
   - qdf なら `direct_stream_lengths=false`。
   - **Preserve / Generate / Disable の分岐位置はここ 1 箇所**: `qpdf_o_disable` は no-op、
     `qpdf_o_preserve` → `preserveObjectStreams()`、`qpdf_o_generate` → `generateObjectStreams()`。
   - 後処理: linearized なら全 page を `object_to_object_stream` から erase。linearized
     **または encrypted** なら root を erase（Adobe Reader 8.0.0 回避）。
   - 逆写像 `object_stream_to_objects`（`std::map<int, std::set<QPDFObjGen>>`）と
     `max_ostream_index` を構築。ObjStm が 1 つでもあれば最低版 1.5。`final_pdf_version` 決定。
2. `events_expected` の設定（progress 用）。
3. `prepareFileForWrite()`（`libqpdf/QPDFWriter.cc:2036-2056`）: `fixDanglingReferences()`
   （`libqpdf/QPDF.cc:1259-1269`）、`/Root /Extensions` と `/ADBE` を direct 化。
4. `linearized` なら `writeLinearized()`（`libqpdf/QPDFWriter.cc:2537-2904`）、そうでなければ
   `writeStandard()`（`libqpdf/QPDFWriter.cc:2991-3044`）。**pclm は独立した top-level 経路では
   なく、`writeStandard` の中で `enqueueObjectsPCLm` を選ぶだけ**。
5. `pipeline->finish()`、file close、buffer 回収、progress 完了。

### D-1. 採番の正本は単一の `enqueueObject`（container-first、member 範囲は即時予約）

`QPDFWriter::enqueueObject`（`libqpdf/QPDFWriter.cc:1072-1141`）が **非 linearized 経路の唯一の
採番点**。状態は `m->obj_renumber`（`std::map<QPDFObjGen,int>`）、`m->object_queue`、`m->next_objid`。

- indirect の場合:
  - 他 QPDF 所有なら `std::logic_error`。qdf で `/Type /XRef` stream なら無視。
  - 未採番（`obj_renumber.count(og)==0`）かつ **ObjStm メンバー**（`object_to_object_stream.count(og)`）なら
    `obj_renumber[og]=0`（ループ検出用 sentinel）を置き、**container を
    `enqueueObject(m->pdf.getObjectByID(stream_id, 0))` で先に enqueue する**。メンバー自身は
    queue に入らない。
  - 未採番かつメンバーでないなら `object_queue.push_back(object)`、`obj_renumber[og] = next_objid++`。
    **その object 自身が container**（gen 0 かつ `object_stream_to_objects.count(obj)`）なら、
    **非 linearized に限り** `assignCompressedObjectNumbers(og)`
    （`libqpdf/QPDFWriter.cc:1057-1069`）で
    **`object_stream_to_objects[objid]`（`std::set<QPDFObjGen>` = objgen 昇順）の全メンバーに
    連番を即時予約**する。container でなく stream かつ `!direct_stream_lengths` なら `/Length` 用に
    1 番だけ予約。
  - 既採番で `obj_renumber[og]==0` は自己参照 ObjStm として無視。
- direct の場合: 非 linearized なら array / dict の子を再帰 enqueue（dict は null 値を skip）。
  linearized では **何もしない**。

`unparseChild`（`libqpdf/QPDFWriter.cc:1144-1157`）は非 linearized で書き込み中に子を
`enqueueObject` するため、採番は「queue の先頭から `writeObject` → `unparseObject` → 子を
発見した瞬間に採番」という **書き込み順の遅延採番**。書き込み順序 = `object_queue` 順 = 採番順。

`enqueueObjectsStandard`（`libqpdf/QPDFWriter.cc:2907-2925`）: `preserve_unreferenced_objects` なら
先に `getAllObjects()`（`libqpdf/QPDF.cc:1286-1295`、`obj_cache` 昇順）を全 enqueue → 次に
`getTrimmedTrailer().getKey("/Root")` → 残る trailer key を `getKeys()`（sorted）順に enqueue。
`enqueueObjectsPCLm`（`libqpdf/QPDFWriter.cc:2928-2954`）は page → `/Contents` →
`/Resources /XObject` の各 strip とその直後に新規 `q /image Do Q\n` stream → 最後に `/Root`。

### D-2. object universe（reachable set vs `getAllObjects`）

- 非 linearized・`preserve_unreferenced=false`: universe は **trailer から `enqueueObject` で到達した
  集合**（Root → 他 trailer key → 書き込み時の子）。到達しない object は書かれない。
  **qpdf に「削除パス」は存在しない** — 到達しないものは単に enqueue されない。
- 非 linearized・`preserve_unreferenced=true`: `getAllObjects()` を先に全 enqueue。
- ObjStm 候補（Preserve / Generate 共通）: `QPDF::getCompressibleObjGens()`
  （`libqpdf/QPDF.cc:2393-2474`）— trailer を起点に **stack（LIFO）で、dict key は `rbegin()` の
  逆順 push、array は `getArrayItem(n-i)` で末尾から push** する DFS。stream 自身と
  `/Sig`(`/ByteRange`+`/Contents`) と encryption dict は結果から除外、stream の `/Length` edge は
  辿らない、stale generation の object は `removeObject` して skip。訪問順が結果 vector の順。
- Preserve の入力: `QPDF::getObjectStreamData`（`libqpdf/QPDF.cc:2381-2390`）= 入力 xref の
  type 2 entry（obj → 元 container 番号）。`preserveObjectStreams`
  （`libqpdf/QPDFWriter.cc:1939-1967`）は `preserve_unreferenced` でなければ
  `getCompressibleObjGens()` の集合で filter し、gen 0 の (obj, stream) だけを
  `object_to_object_stream` へ入れる。**出力 container の番号は元の番号ではなく D-1 の enqueue で
  決まる**（`object_to_object_stream` の値は `enqueueObject(getObjectByID(stream_id, 0))` の
  対象を指すだけ）。
- Generate: `generateObjectStreams`（`libqpdf/QPDFWriter.cc:1970-2006`）は
  `n_object_streams = (eligible+99)/100`、`n_per = eligible/n_object_streams`（割り切れなければ +1）、
  `n_per` 件ごとに `makeIndirectObject(newNull())` で **source QPDF 側に新規 null object を作り、
  その objid を container id とする**。`/Extends` は扱わない。出力番号はやはり D-1 の enqueue 時。
- 2026-09-13（`flpdf-r96x`）: source-backed Preserve の ObjStm member は、qpdf の
  `object_stream_to_objects` 内 `std::set<QPDFObjGen>` を `writeObjectStream` が歩く順に合わせる。
  multi-source の fresh target では local target number が source ObjGen と逆転し得るため、
  `writer_object_order` に記録した original-object provenance を使って member を並べる。
  Generate/Synthetic の新規 group はこの規則を使わず、既存の discovery/order policy を維持する。
- Preserve の source membership emission: `preserve_unreferenced_objects=true` の場合、
  `QPDFWriter::preserveObjectStreams` は `getCompressibleObjGens()` の eligibility intersection を
  適用せず、source ObjStm の `/Type /Sig` + `/ByteRange` + `/Contents` dictionary も
  `writeObjectStream` でそのまま member として出力する（`libqpdf/QPDFWriter.cc:1939-1967,1621-1758`）。
  signature の除外は通常の eligibility walk に限定する。`flpdf-lhzo` の
  `cmp_diff_zero_tests::qdf_preserve_unreferenced_signature_objstm_matches_qpdf_11_9` が、
  `--static-id --qdf --preserve-unreferenced` の status と bytes、および既解消済み2 fixtureを
  qpdf 11.9.0 と比較する。
- 2026-09-13（`flpdf-idd3`）: legacy planned Preserve の object-stream planner と
  linearized Preserve の source-container mapping は、setup 時に取得した同じ
  `source_object_stream_data` snapshot を受け取る。writer 内の source-container exclusion
  helper (`rewrite_renumber`) は別の seed-membership consumerであり、今回の snapshot配管の
  対象外として残る。
- linearized: `QPDF::optimize`（`libqpdf/QPDF_optimization.cc:57-118`）が `/Outlines` の indirect 化、
  `pushInheritedAttributesToPage`、各 page / trailer key（`/Root` 以外）/ root key ごとに
  `updateObjectMaps` で `obj_user_to_objects` / `object_to_obj_users` を作り、
  `filterCompressedObjects`（`libqpdf/QPDF_optimization.cc:340-381`）で compressed member の user を
  container に付け替える。universe = `object_to_obj_users` のキー集合で、
  `calculateLinearizationData`（`libqpdf/QPDF_linearization.cc:963-1403`）末尾で
  `num_placed == num_wanted` を検査する。

2026-09-13（`flpdf-zv0i`）: plain planned Preserve の source ObjStm 有無も、
production setup が取得した `get_object_stream_data` の D9 snapshot を consumer に渡して判定する。
test-only の `PlainWritePlan::build` wrapper も同じ canonical ownerから snapshotを作るため、
`source_xref_entries()` を再走査する source-presence predicate は残さない。linearized の source
container lookup と `rewrite_renumber` の source-container helper は、presence 判定ではなく
別の source membership consumer として後続スコープに残る。

### D-3. `writeObject` / `writeObjectStream`

- `writeObject(object, object_stream_index = -1)`（`libqpdf/QPDFWriter.cc:1761-1809`）:
  `object_stream_index==-1` かつ gen 0 かつ container なら `writeObjectStream(object)` へ委譲。
  それ以外は progress → qdf コメント（`%% Page N` / `%% Contents for page N`）→ top-level なら
  `%% Original object ID`（qdf）、`openObject(new_id)`（`libqpdf/QPDFWriter.cc:1036-1045`:
  `xref[objid]` にオフセット記録、`"N 0 obj\n"`）、`setDataKey(new_id)`
  （`libqpdf/QPDFWriter.cc:843-847`）、`unparseObject(object,0,0)`、`cur_data_key.clear()`、
  `closeObject(new_id)`（`libqpdf/QPDFWriter.cc:1048-1054`: `"\nendobj\n"`、qdf は追加 `"\n"`、
  `lengths[objid]` 記録）。ObjStm 内なら `unparseObject(..., f_in_ostream)` + `"\n"`。
  `!direct_stream_lengths` かつ stream なら `new_id+1` で `/Length` object。
- `writeObjectStream(object)`（`libqpdf/QPDFWriter.cc:1621-1758`）: `object` は Generate では
  null placeholder。2 パス（pass 1 は `pushDiscardFilter` でオフセット計測、pass 2 は
  `writeObjectStreamOffsets`（`libqpdf/QPDFWriter.cc:1606-1618`）を discard で 1 度書いて `first` を
  確定 → `Pl_Buffer`（+`Pl_Flate` if `compress_streams && !qdf_mode`）に本体）。メンバーは
  **`object_stream_to_objects[old_id]`（objgen 昇順）** を順に、qdf なら
  `%% Object stream: object N, index I[; original object ID: …]`、stream メンバーは
  `"stream found inside object stream; treating as null"` と warn して null 化、`writeObject(obj, count)`、
  `xref[new_obj] = QPDFXRefEntry(new_id, count)`。dict は `/Type /ObjStm` `/Length`
  （`adjustAESStreamLength` 後）`/Filter /FlateDecode`（compressed 時）`/N` `/First`、元 object が
  非 null で `/Extends` が indirect なら `unparseChild(extends, 1, f_in_ostream)` で複写。本体は
  `pushEncryptionFilter` 経由、`newline_before_endstream` で `"\n"`、`endstream`。

### D-4. xref / trailer

- `writeStandard`（`libqpdf/QPDFWriter.cc:2991-3044`）: deterministic なら MD5 pipeline →
  `writeHeader()`（`libqpdf/QPDFWriter.cc:2266-2284`; pclm は `%PCLm 1.0`、qdf は `%QDF-1.0`）→
  `extra_header_text` → `enqueueObjectsPCLm` or `enqueueObjectsStandard` → **queue を先頭から
  `writeObject`**（queue は書き込み中に伸びる）→ **`encrypted` なら `writeEncryptionDictionary()`** →
  `xref_offset` 記録 → **`object_stream_to_objects` が空なら
  `writeXRefTable(t_normal, 0, next_objid-1, next_objid)`、空でなければ `xref_id = next_objid++` して
  `writeXRefStream(xref_id, xref_id, xref_offset, t_normal, 0, next_objid-1, next_objid)`** →
  `startxref\n<xref_offset>\n%%EOF\n`。
- `writeXRefTable`（`libqpdf/QPDFWriter.cc:2343-2379`、4 引数の委譲オーバーロードは
  `libqpdf/QPDFWriter.cc:2335-2340`）: `xref\n<first> <count>\n`、entry 0 は
  `0000000000 65535 f \n`、他は `%010d 00000 n \n`（hint 補正あり）→
  `writeTrailer(which, size, false, prev, pass)` → `"\n"`。
- `writeXRefStream`（`libqpdf/QPDFWriter.cc:2392-2495`、7 引数の委譲オーバーロードは
  `libqpdf/QPDFWriter.cc:2382-2389`）: `f1_size = max(bytesNeeded(max_offset+hint_length),
  bytesNeeded(max_id))`、`f2_size = bytesNeeded(max_ostream_index)`、`esize = 1+f1+f2`；
  `xref[xref_id]` を先に記録；`compress_streams && !qdf_mode` なら `Pl_Flate`
  （linearize pass 1 は `skip_compression`）+ `Pl_PNGFilter(esize)`；entry type 0/1/2 をバイナリ
  書き出し；dict は `/Type /XRef /Length [/Filter /FlateDecode /DecodeParms << /Columns esize
  /Predictor 12 >>] /W [ 1 f1 f2 ]`、`first==0 && last==size-1` でない時だけ `/Index`；
  `writeTrailer(..., true, ...)`；`\nstream\n` 本体 `\nendstream`。
- `writeTrailer`（`libqpdf/QPDFWriter.cc:1160-1236`）: `getTrimmedTrailer()`
  （`libqpdf/QPDFWriter.cc:2009-2032`: `/ID /Encrypt /Prev /Index /W /Length /Filter /DecodeParms
  /Type /XRefStm` を除去）; xref stream なら `cur_data_key.clear()`（trailer 文字列は暗号化しない）、
  table なら `trailer <<`；`t_lin_second` は `/Size` のみ、それ以外は `getKeys()`（sorted）順で
  `/Size` を差し替え（`t_lin_first` は続けて `/Prev` を 21 桁分パディング）、他は `unparseChild`；
  `/ID [` は pass 1 でゼロ列、pass 0 で `deterministic_id` なら `computeDeterministicIDData()` の後
  `generateID()`；`t_lin_second` 以外で `encrypted` なら ` /Encrypt <encryption_dict_objid> 0 R`；`>>`。

**qpdf は非 linearized の全経路（standard / pclm）で同一の `writeXRefTable` / `writeXRefStream` /
`writeTrailer` を共有する。** linearized も同じ 3 関数を追加引数付きで呼ぶ。xref/trailer を書く
実装は qpdf 全体で 1 組しかない。

### D-5. encryption

- `setEncryptionParametersInternal`（`libqpdf/QPDFWriter.cc:777-840`）: `encryption_dictionary`
  （`std::map<std::string,std::string>` = key 昇順）に `/Filter /V /Length /R /P /O /U`、V≥5 なら
  `/OE /UE /Perms`、R≥4 かつ `!encrypt_metadata` なら `/EncryptMetadata false`、V が 4 か 5 なら
  `/StmF /StrF /CF`；min version（R≥6→1.7 ext8、R5→1.7 ext3、R4→1.6/1.5、R3→1.4、他→1.3）；
  `encrypted=true`；V<5 なら `compute_encryption_key`。
- `setEncryptionParameters`（`libqpdf/QPDFWriter.cc:591-648`）: bits 1,2 を常に clear、R>3 なら
  bit 10 を clear 対象から除外（＝常に set）、`P` を組み、**`generateID()` を先に呼んで `m->id1` を
  O/U 計算に使う**、V<5 は `compute_encryption_O_U`、V≥5 は `compute_encryption_parameters_V5`
  → internal。
- `copyEncryptionParameters`（`libqpdf/QPDFWriter.cc:651-702`）: trailer `/Encrypt` から
  V / Length / EncryptMetadata / R / P / O / U（V≥5 は OE / UE / Perms と `getEncryptionKey()`）を
  取り、`id1` は **元ファイルの `/ID[0]`**、V≥4 なら AES を強制 → internal。
- `writeEncryptionDictionary`（`libqpdf/QPDFWriter.cc:2244-2256`）:
  `openObject(m->encryption_dict_objid)`（standard では 0 なので **その時点の `next_objid++`**
  = body 全 object の後の番号）、`<<` + `" " key " " value` を map 順 + `" >>"`。
  **standard では body 書き出し完了後・xref の直前**。linearized では番号は事前予約（part4 の直後）で、
  書き出しは `part4_end_marker` の object の直後。
- `setDataKey(objid)`（`libqpdf/QPDFWriter.cc:843-847`）: object ごとに `compute_data_key`。
  `pushEncryptionFilter`（`libqpdf/QPDFWriter.cc:976-1000`）が stream 本体を、`unparseObject` が
  string を暗号化する。

**D16 current slice (2026-09-07):** `writer.rs::build_writer_setup` now builds
the shared `EncryptionParameters` once after qpdf-shaped option normalization.
Standard and linearized routes consume that state and assign their own
qpdf-specific `/Encrypt` slot through `EncryptionParameters::into_context`;
password/file-key/dictionary construction and ID0 are not rebuilt per route.
The common `PdfWriter::write` lifecycle still owns setup, special-stream
initialization, graph preparation, and dispatch ordering. qtest exceptions
`.48.45` are outside this route slice.

### D-6. linearize は pass1 → hint 1 回計算 → pass2 で、反復しない

`writeLinearized`（`libqpdf/QPDFWriter.cc:2537-2904`）:

1. `discardGeneration(object_to_object_stream → object_to_object_stream_no_gen)`
   （`libqpdf/QPDFWriter.cc:2510-2534`; 同 objid で gen 違いがあれば `std::runtime_error`）。
2. `m->pdf.optimize(object_to_object_stream_no_gen, true, skip_stream_parameters)`。
3. `QPDF::Writer::getLinearizedParts`（`include/qpdf/QPDF.hh:729-740` →
   `libqpdf/QPDF_linearization.cc:1435-1449` → `calculateLinearizationData`
   `libqpdf/QPDF_linearization.cc:963-1403`）で part4/6/7/8/9。part の中身
   （`libqpdf/QPDF_linearization.cc:1174-1336`）: part4 = root + open-document keys、
   part6 = 先頭 page + first-page private + first-page shared（+ outlines が first page 側なら outlines）、
   part7 = 2 ページ目以降の page とその private、part8 = other-page shared、part9 = 残り
   （thumb / outlines / lc_other）。
4. **採番は enqueue ではなく事前計算**: second half（part7+8+9 の uncompressed 数）を 1 から、
   `need_xref_stream = !object_to_object_stream.empty()` なら second_half_xref、part7/8/9 の
   container に `assignCompressedObjectNumbers`；first half は lindict → first_half_xref →
   part4 範囲 → encryption dict（encrypted 時）→ hint → part6 範囲 → part4/6 の container メンバー。
   ObjStm を生成しない classic 経路の second-half `RenumberMap` も、part7 → part8 →
   Pages tree → private/shared thumbnails → outlines → `lc_other` の順で予約する
   （`QPDF_linearization.cc:1280-1338`）。
5. `enqueuePart(part4)` / `(part6)` / `(part7,8,9)` を `next_objid` を各 part 先頭にリセットして実行し、
   各 part 後に `next_objid` が期待値でなければ `std::runtime_error`。
6. **2 パス**: pass 1 は `pushDiscardFilter`（または `lin_pass1_filename`）+ deterministic なら MD5；
   各 pass で `writeHeader` → lindict（pass 2 のみ実値、`writePad` で 200 byte 枠）→
   `extra_header_text` → first xref（stream なら pass 1 で `first_half_max_obj_offset = 1<<25`、
   `skip_compression=(pass==1)`、`calculateXrefStreamPadding`
   （`libqpdf/QPDFWriter.cc:2498-2507`）で pad、pass 2 で同位置まで pad しズレれば
   `std::logic_error`；table なら `startxref\n0\n%%EOF\n`）→ `object_queue` を順に `writeObject`、
   `part4_end_marker` の直後に `writeEncryptionDictionary` と hint（pass 1 は `xref[hint_id]` の
   オフセット記録のみ、pass 2 は `writeBuffer(hint_buffer)`）、`part6_end_marker` で
   `part6_end_offset` → second xref → `startxref` → `discardGeneration(obj_renumber → …_no_gen)`。
7. **pass 1 終了時に 1 度だけ** `computeDeterministicIDData`（deterministic 時）、`file_size` 確定、
   `writeHintStream(hint_id)`（`libqpdf/QPDFWriter.cc:2287-2332` → `QPDF::Writer::generateHintStream`
   `libqpdf/QPDF_linearization.cc:1758-1796`）を `Pl_Buffer` に書いて `hint_length` を得る。
   **収束ループは無い**。pass 2 は pass 1 の padding に収まらなければ `std::logic_error` で失敗する設計。
   `newline_before_endstream` は linearization で clear されず、`writeObject` の通常stream
   （`libqpdf/QPDFWriter.cc:1551-1566`）と `writeObjectStream` の ObjStm container
   （同 `:1752-1755`）へそのまま適用される。一方 `writeHintStream` はこの設定を参照せず、
   暗号化後の末尾が LF でない場合だけ LF を追加する（同 `:2319-2329`）。
8. linearized では `enqueueObject` の direct 子再帰も、`enqueueObject` からの
   `assignCompressedObjectNumbers` 呼び出しも無効（D-1）。

### D-7. public / private 境界（`include/qpdf/QPDFWriter.hh`）

- public（`include/qpdf/QPDFWriter.hh:55-439`）: constructor 3 種、出力設定
  （`setOutputFilename` / `setOutputFile` / `setOutputMemory` / `getBuffer` /
  `getBufferSharedPointer` / `setOutputPipeline`）、モード設定（`setObjectStreamMode` /
  `setStreamDataMode` / `setCompressStreams` / `setDecodeLevel` / `setRecompressFlate` /
  `setContentNormalization` / `setQDFMode` / `setPreserveUnreferencedObjects` /
  `setNewlineBeforeEndstream` / `setMinimumPDFVersion` ×2 / `forcePDFVersion` /
  `setExtraHeaderText` / `setDeterministicID` / `setStaticID` / `setStaticAesIV` /
  `setSuppressOriginalObjectIDs` / `setLinearization` / `setLinearizationPass1Filename` /
  `setPCLm`）、暗号設定（`setPreserveEncryption` / `copyEncryptionParameters` /
  `setR{2,3,4}EncryptionParametersInsecure` / `setR{5,6}EncryptionParameters`）、
  `registerProgressReporter`、`getFinalVersion`、`write`、`getRenumberedObjGen`、
  `getWrittenXRefTable`。
- private（`include/qpdf/QPDFWriter.hh:440-608`）: `enqueueObject` /
  `assignCompressedObjectNumbers` / `writeObjectStream` / `writeObject` / `writeTrailer` /
  `willFilterStream` / `unparseObject` / `unparseChild` / `initializeSpecialStreams` /
  `preserveObjectStreams` / `generateObjectStreams` / `generateID` /
  `setEncryptionParameters` / `setEncryptionParametersInternal` / `setDataKey` / `openObject` /
  `closeObject` / `getTrimmedTrailer` / `prepareFileForWrite` / `enqueueObjectsStandard` /
  `enqueueObjectsPCLm` / `writeStandard` / `writeLinearized` / `enqueuePart` /
  `writeEncryptionDictionary` / `doWriteSetup` / `writeHeader` / `writeHintStream` /
  `writeXRefTable` ×2 / `writeXRefStream` ×2 / `calculateXrefStreamPadding` / pipeline stack /
  `discardGeneration`。**書き込み手順を外から起動する public API は `write()` 1 つだけ**。
- `QPDF::Writer`（`include/qpdf/QPDF.hh:724-768`、`friend class QPDFWriter`）: `getLinearizedParts` /
  `generateHintStream` / `getObjectStreamData` / `getCompressibleObjGens` の 4 つだけを開く。

### D-8. 経路の要約（flpdf 対応付けの基準）

| qpdf 経路 | 条件 | 採番 | ObjStm | xref |
|---|---|---|---|---|
| `writeStandard` + `enqueueObjectsStandard` | `!linearized && !pclm` | `enqueueObject`（遅延、container-first） | `object_stream_mode` 通り（linearized/encrypted なら root、linearized なら page も除外） | `object_stream_to_objects.empty()` で table / stream を選択 |
| `writeStandard` + `enqueueObjectsPCLm` | `pclm` | 同上（page → contents → strips → root） | pclm は decode none / 非圧縮 / 非暗号だが ObjStm 分岐自体は共通 | **standard と同一の xref/trailer** |
| `writeLinearized` | `linearized` | `writeLinearized` 事前計算 + `enqueuePart` | page / root を除外 | `need_xref_stream` で 2 組の table / stream |

encryption は上記 3 経路すべてで `doWriteSetup` の同一分岐（D-0）を通り、
`writeEncryptionDictionary` の位置だけが standard（body 後）/ linearized（part4 後）で異なる。

## route matrix

| # | qpdf responsibility owner | qpdf evidence | flpdf current entrypoint | callers (prod / test) | classification | canonical owner | remaining bridge callers / notes |
|---|---|---|---|---|---|---|---|
| D1 | `QPDFWriter::write` / `QPDFWriter::doWriteSetup`（mode 正規化 → 単一 dispatch） | `libqpdf/QPDFWriter.cc:2187-2213,2059-2184` | `crates/flpdf/src/writer.rs::PdfWriter::write`（`pub`、`crates/flpdf/src/writer.rs:719`） | prod: 5 (`crates/flpdf/src/job/check.rs:350`, `crates/flpdf/src/job/lifecycle.rs:2506`, `crates/flpdf/src/job/page_split.rs:282`, `crates/flpdf-cli/src/main.rs:330,343`) / test: 25 files（`crates/flpdf-qtest-tools/` の driver 43 箇所と `crates/flpdf/examples/` 7 箇所を除く） | mixed | `crates/flpdf/src/writer.rs::PdfWriter::write` | qpdf は `write()` 内で 2 分岐（`writeLinearized` / `writeStandard`）、pclm は `writeStandard` 内の 1 段深い分岐。flpdf は `settings.linearization` で `write_linearized_for_pdf_writer`（`crates/flpdf/src/writer.rs:773`）、非 linearized は `emit_canonical_pdf` 内で PCLm の shared live route / plain route / legacy coordinator へ分岐する。dispatch 条件表は「WriterOptions と route の対応」節 |
| D2 | `QPDFWriter::enqueueObject`（非 linearized の唯一の採番点、container-first・member 範囲即時予約） | `libqpdf/QPDFWriter.cc:1072-1141,1057-1069` | plain の live `crates/flpdf/src/writer/plain/body.rs::LiveQueue`（emission-time queue、Disable と Preserve 全体）と specialized standard live coordinator | plain live queue prod: 1（Disable と Preserve、unnormalized 全体） / specialized standard Generate が追加。QDF/normalize Generate と source ObjStm-bearing Preserve は planned consumerの残スコープ | mixed | `LiveQueue`（plain の unnormalized bounded consumer） | plain Disable は root/trailer seed → first-seen numbering → `WriteObject::write_object` 中の child discovery を qpdf 順で実行する。PCLm と specialized standard は `.48.65`/`.48.s07c` の live consumer 移行でこの queue owner に接続済み。2026-09-13（`flpdf-3yn9.48.87`）: 非暗号・非QDF・非normalize の Generate も、setupで `generateObjectStreams` 相当の候補と fresh null container を snapshotした後、specialized standard live coordinatorの同じqueueへ接続した。Generate の container-first予約とcallback child discoveryを共有する。QDF/normalize Generate、source ObjStm-bearing Preserveの planned consumer、およびlinearizedの専用採番は後続。2026-09-08（`flpdf-3yn9.48.65` 第1 cohort）: `writer/plain/mod.rs::write_plain` の live queue dispatch を unnormalized Preserve 全体（source に既存 ObjStm がある場合を含む）へ拡張した。source に ObjStm が無い側はqpdf の `preserveObjectStreams` は source に ObjStm が無ければ mapping を作る前に return し（`QPDFWriter.cc:1941-1945`）、`object_stream_to_objects` が空のままになるため `enqueueObject` の container 分岐（`:1097-1106`）にも入らず、1.5 floor（`:2172-2173`）も付かず、classic cross-reference table（`:3023-3025`）になる——つまり Disable と同一形になる。`PlainWritePlan::build` 側も同じ条件で既に `CanonicalCatalogFirstRenumber` フォールバックへ分岐していた（真の ObjStm packing は未使用）ため、新規の採番ロジックは不要だった。source に ObjStm がある側は `:1955-1966` が同 map を埋め、`enqueueObject` が member の discovery を container へ redirect し、`assignCompressedObjectNumbers`（`:1057-1069`）が container の初回 enqueue 時に全 member を採番する。`LiveQueue::register_object_streams` / `member_to_container` / `container_to_members` がその container-first 採番を写し、`has_object_stream_hint` が 1.5 floor と cross-reference stream 選択を同じ setup-time membership から導く。QDF/normalize の Disable と source ObjStm なし Preserve は `flpdf-ay5b` で QDF length-holder / page-context を含む同じ live queueへ接続した。残るのは QDF/normalize Generate と source-backed Preserveの planned consumerである。 |
| D3 | 同上（`enqueueObject` の container なし側 = 純粋な到達順採番） | `libqpdf/QPDFWriter.cc:1072-1141` | `crates/flpdf/src/writer/plain/body.rs::LiveQueue` → `LiveObjectEmitter` の `ObjectWriterEmission` map callback、および specialized standard live coordinator | prod: plain live 1 + specialized standard Generate / test: live mutation + qpdf differential | mixed | plain の live `LiveQueue`（Disable/Preserve と plain Generate bounded cohort） | `CanonicalCatalogFirstRenumber` は QDF/normalize Generate と source ObjStm-bearing Preserve の planned consumerに残る。plain の live production write path（Disable と unnormalized Preserve、QDF/normalize の Disable・source ObjStm なし Preserve）は pre-write mapを使わず、`unparseChild` 相当の map callback が新しい indirect child を queue に追加する。2026-09-13（`flpdf-3yn9.48.87`）: plain Generateも setup時の候補・container snapshotを使って specialized standard live bodyへ入り、progress callbackが後からroot graphへ追加した childをqueueが発見して採番・出力する回帰を追加した。QDF/normalize Generateとsource ObjStm-bearing Preserveは引き続きplanned。QDF では static QDF serializer のため、同一の visible child 順をemission-time に走査してから mapを固定する。 2026-09-08（`flpdf-3yn9.48.63`）: `linearization/plan.rs::from_pdf_with_writer_options` の pre-optimize `CanonicalCatalogFirstRenumber` warmup 呼び出し（戻り値を捨てるだけの副作用専用呼び出し）を撤去した。`Optimization::optimize`（`crates/flpdf/src/optimization.rs::build_maps`）自身が既に canonical `ObjectHandle` route で page/trailer/root を1パス走査し、malformed stream の framing/length recovery も同じ経路で行うため、この warmup は現行コード形状に対して恒常的に冗長だった（qpdf の `writeLinearized` も setup 直後に `optimize()` を呼ぶだけで別 warmup パスを持たない、`QPDFWriter.cc:2536-2554`）。damaged stream を `/Root` 側・trailer 側それぞれに単独配置した2fixtureで実 qpdf 11.9.0 と stderr（順序・件数・文言）・出力バイトとも一致することを確認し、workspace 全2728 lib test + linearization integration testも warmup 除去前後で無変化と確認済み。 |
| D4 | `QPDFWriter::writeLinearized` の事前採番（second half → first half、part 単位） | `libqpdf/QPDFWriter.cc:2575-2646` | `crates/flpdf/src/linearization/renumber.rs::RenumberMap`（`pub`、`crates/flpdf/src/linearization/renumber.rs:84`） | prod: 19 (`crates/flpdf/src/linearization/writer.rs`=12, `crates/flpdf/src/linearization/renumber.rs`=2, `crates/flpdf/src/linearization/part1.rs`=2, `crates/flpdf/src/linearization/{hint_page,hint_shared,plan}.rs`=各1) / test: 107 (8 files) | canonical | `crates/flpdf/src/linearization/renumber.rs::RenumberMap` | qpdf も linearized では `enqueueObject` の採番を使わないので、専用機構が 1 本あること自体は逸脱でない。`RenumberMap::from_plan`（`crates/flpdf/src/linearization/renumber.rs:191`）が part 順の slot 割当を持つ |
| D5 | `QPDFWriter::assignCompressedObjectNumbers`（container enqueue 時に member 範囲を即時予約） | `libqpdf/QPDFWriter.cc:1057-1069` | `crates/flpdf/src/writer/object_streams/planning.rs::ObjectStreamGroup`（`pub(crate)`、`crates/flpdf/src/writer/object_streams/planning.rs:73`）を `ObjectStreamRenumber::build_with_stream_policy` に渡す | prod: 19 (`crates/flpdf/src/writer/plain/plan.rs`=10, `crates/flpdf/src/writer/rewrite_renumber.rs`=6, `crates/flpdf/src/writer/object_streams/planning.rs:100,244`, `crates/flpdf/src/writer.rs:4005`) / test: 0 | mixed | `crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber` | Preserve は `SourceBacked { source, members }`、plain Generate は qpdf の `makeIndirectObject(newNull())` に対応する `Generated { source, members }` を作り、両方の source identity を採番へ渡す。2026-09-13（`flpdf-3yn9.48.87`）: 非暗号・非QDF・非normalize の Generate は、この `Generated` groupを specialized standard live queueへ登録し、container first と member range reservationをemission-time ownerに接続した。生成した null placeholder は出力後に writer-owned allocation として除去し、再書き込みの preserve-unreferenced seed に混入させない。QDF/normalize Generate と linearized/残る legacy consumerのSynthetic groupは後続移行対象。 |
| D6 | `QPDFWriter::preserveObjectStreams`（source container map ∩ compressible set） | `libqpdf/QPDFWriter.cc:1939-1967` | `crates/flpdf/src/writer/object_streams/planning.rs::plan_qpdf_preserve_object_streams_with_unreferenced` | prod: plain Preserve 全体（live route も `writer/plain/mod.rs` でこの planner を呼び、返った group を live queue へ登録する。QDF/normalize は source ObjStm がある場合だけ planned consumer を使う） / test: empty-map ordering | mixed | plain Preserve planner | document `get_object_stream_data` を compressible walk より先に呼び、空なら解決せず戻る。specialized coordinator と linearized Preserve は `.48.54` で同じ source-backed membership owner に移行した。live queue emission への接続は `.48.65` 第1 cohort で完了しており、`write_plain_live_disable` がこの planner を直接呼び、`LiveQueue::register_object_streams` が返された source-backed group を container-first 採番の membership として受け取る。QDF/normalize の Disable と source ObjStm なし Preserve は `flpdf-ay5b` で同じ live bodyへ接続した。残るのは Generate と source-backed Preserve の計画系 consumerである。 |
| D7 | `QPDFWriter::generateObjectStreams` の even split 部分（null container allocation は D5） | `libqpdf/QPDFWriter.cc:1970-2006` | `crates/flpdf/src/writer/object_streams/eligibility.rs::even_split_into_streams`（`pub(crate)`、`crates/flpdf/src/writer/object_streams/eligibility.rs:167`） | prod: 4 (`crates/flpdf/src/writer/object_streams/mod.rs:9` re-export, `crates/flpdf/src/writer/plain/plan.rs:237`, `crates/flpdf/src/linearization/writer.rs:3149`, `crates/flpdf/src/linearization/plan.rs:2724`) / test: 0 | canonical | `crates/flpdf/src/writer/object_streams/eligibility.rs::even_split_into_streams` | even split は既存の1本を使用し、plain Generate と linearized Generate の D5 allocation / Generated identityへ接続した。`.48.87` / `.48.88` ではこの snapshotを specialized standard live coordinatorが消費し、`.9zbro` では linearized plannerも同じ setup-time候補から group/slot数を決める。linearizedの専用 two-pass emission自体と残る legacy coordinatorの Synthetic consumerは別責務として残る |
| D8 | `QPDF::getCompressibleObjGens`（trailer 起点の LIFO DFS、stream/Sig/Encrypt 除外） | `libqpdf/QPDF.cc:2393-2474,1996-2005` | production `crates/flpdf/src/writer/object_streams/eligibility.rs::compressible_objgens_qpdf_plan`（test wrapper: `crates/flpdf/src/reader.rs::get_compressible_objgens`）→ setup `crates/flpdf/src/writer.rs::WriterSetupState` → plain/specialized/linearized consumer | plain/linearized Generate（setup snapshot）/ tests: generation aliases, visited order, bounds, exclusions, provider lifetime | mixed | `crates/flpdf/src/writer/object_streams/eligibility.rs::compressible_objgens_qpdf_plan` | Encrypt取得→getObjectCount→object number bitmap→live cache upper_boundの順。stale generationはqpdf同様に`Pdf::remove_object_handle`→resolver cache eraseを実行し、retained aliasをdirect null化する。stream Length edgeだけを省き、他edgeから到達するLengthは候補に残す。plain Generateのsetup-time snapshotは `.48.87` / `.48.88` で specialized standard live consumerへ、linearized Generateは `.9zbro` で専用plannerへ接続済み。Preserveのintersection、linearizedの専用two-pass emission、他の残consumerは別責務として残る。旧reader wrapperはtest-onlyである。 |
| D9 | `QPDF::getObjectStreamData`（入力 xref の type 2 entry → source container） | `libqpdf/QPDF.cc:2381-2390`、`include/qpdf/QPDF.hh:757-761` | `crates/flpdf/src/reader.rs::get_object_stream_data` → `crates/flpdf/src/reader/resolver.rs::get_object_stream_data` | prod: document entry → plain Preserve / tests: mixed rows, prefilled map, live rows, lazy IO | mixed | document-owned type-2 mapping | source xref 正本から objnumber → container number を caller map へ追記・上書きし、clear や object 解決をしない。plain Preserve の再filterを撤去。`source_xref_entries` は reader 自身の責務と残 consumer 用に維持し、legacy `plan_preserve`、writer の source-container lookup、linearized の membership/lookup は後続で移行する。 |
| D10 | `QPDFWriter::writeObjectStream`（2 パス・objgen 昇順 member・`/Extends` 複写） | `libqpdf/QPDFWriter.cc:1621-1758,1606-1618` | `crates/flpdf/src/writer/object_streams/emission.rs::emit_objstm_body_from_handles_with_writer`（`pub(crate)`、`crates/flpdf/src/writer/object_streams/emission.rs:45`） + linearized container wrapper `wrap_objstm_body_as_handle`（`:183`） | prod: 3 (`crates/flpdf/src/writer/plain/body.rs:63`, `crates/flpdf/src/writer.rs:4860`, `crates/flpdf/src/linearization/writer.rs:329`) + re-export 1 (`crates/flpdf/src/writer/object_streams/mod.rs:13`) / test: wrapper ownership 1 | canonical | `crates/flpdf/src/writer/object_streams/emission.rs::emit_objstm_body_from_handles_with_writer` + `wrap_objstm_body_as_handle` | ObjStm body の 2 パス生成は 3 経路すべてがこの 1 関数を通る。linearized wrapper は `ObjStmBody` を消費して `Rc<Vec<u8>>` を handle と sink で共有し、qpdf の `shared_ptr<Buffer>` と同じ単一 payload ownership を保つ（`QPDFWriter.cc:1636-1750,881-884,925-965`）。QDF 変種は `…_qdf`（`crates/flpdf/src/writer/object_streams/emission.rs:58`）。ただし container dict の組み立て（`/Type /ObjStm /Length /Filter /N /First`）と `/Extends` の付与は呼び出し側に残っており、そこは D11 の smear に含まれる |
| D11 | `QPDFWriter::writeObject` / `unparseObject`（body 1 オブジェクトの emission） | `libqpdf/QPDFWriter.cc:1761-1809,1318-1603` | `crates/flpdf/src/writer/write_object.rs::WriteObject` → plain Disable / specialized standard / PCLm / QDF-normalize bounded cohort `LiveObjectEmitter`、remaining planned `PlainObjectEmitter` | shared prod: live consumers + planned consumers / tests: writer emission + live queue | mixed | `WriteObject::write_object` + plain live `LiveObjectEmitter` | 共通 owner が progress、QDF framing、set/clear data key、open/unparse/close を所有し、plain Disable・specialized standard・PCLm・QDF/normalize の bounded cohort は live queue で child を発見する。`.48.87` で非暗号・非QDF・非normalize Generateの fresh ObjStmも setup snapshotから同じ specialized live queueへ接続し、progress callback childの発見を固定した。残るのはQDF/normalize Generate、source ObjStm-bearing Preserveの planned consumer、暗号化系の未移行 packing/child discovery、linearizedのplanned consumersである。 |
| D12 | `QPDFWriter::writeXRefTable`（classic xref table） | `libqpdf/QPDFWriter.cc:2343-2379,2335-2340` | 共有 primitive `crates/flpdf/src/writer/plain/xref.rs::write_xref_table`（`pub(crate)`、`:287`）を plain / specialized standard consumer `append_xref_and_trailer` から接続。PCLm は `writer.rs::write_pclm` が独自の classic-xref ループを持ち、この append owner を通らない | shared prod: live consumers + legacy/linearized consumers / test: 7 | mixed | `crates/flpdf/src/writer/plain/xref.rs::write_xref_table` | qpdf の entry 0、type-1 offset、generation 0、range、`suppress_offsets`、hint補正を共有primitiveへ移植した。missing/free/type2を `00000 f`/`65535 f` として捏造せず、`QPDFXRefEntry::getOffset`（`QPDFXRefEntry.cc:27-32`）と同じ `Error::Internal("getOffset called for xref entry of type != 1")` を返す。plain classic・specialized standard は shared append owner を使う。PCLm は `write_pclm` 内の route-local な classic-xref ループで、`append_xref_and_trailer` の caller ではない。legacy/linearizedの残るtable loopは後続sliceへ委譲する。正常writerのgap producer調査（U3）は本sliceと分離する |
| D13 | `QPDFWriter::writeXRefStream`（type 0/1/2 バイナリ + PNG predictor） | `libqpdf/QPDFWriter.cc:2392-2495,2382-2389` | 共有 owner `crates/flpdf/src/writer/serialize.rs::xref_stream`（`build_entries_with_self` / `encode_payload_for_policy` / widths / dictionary）を plain と linearized の全 stream consumer から利用 | shared prod: 3（plain / linearized first-half / linearized second-half） / test: 3 | canonical | `crates/flpdf/src/writer/serialize.rs::xref_stream` | `build_entries_with_self` が qpdf の self-xref 事前登録、type-1 field 3 の常時 zero、type-2 member index を一度に確定する。`encode_payload_for_policy` が plain と linearized pass-1/final の raw・PNG predictor・Flate policy を共有する。legacy coordinator は plain owner へ委譲し、linearized の first-half/second-half は consumer 固有の範囲・padding・trailer framing だけを保持する |
| D14 | `QPDFWriter::writeTrailer` / `getTrimmedTrailer` | `libqpdf/QPDFWriter.cc:1160-1236,2009-2032` | `crates/flpdf/src/writer.rs::build_writer_trailer_handle`（private）と `crates/flpdf/src/writer/object.rs::write_trailer_with_ref_map`（`crates/flpdf/src/writer/object.rs:952`） | `build_writer_trailer_handle` centralizes trailer trimming; route-specific live-handle writers and xref framing consume it through OutputSink | mixed | `crates/flpdf/src/writer.rs::build_writer_trailer_handle` plus route-specific sink writers | qpdf trimming/key exclusion is centralized; null visibility and xref-form framing remain at the canonical live serializer/xref owners. |
| D15 | `QPDFWriter::writeEncryptionDictionary`（body 後・xref 前、`std::map` の key 昇順） | `libqpdf/QPDFWriter.cc:2244-2256`、`writeStandard` からの呼び出し位置は `libqpdf/QPDFWriter.cc:3017-3019`（他の呼び出し元は `writeLinearized` の `libqpdf/QPDFWriter.cc:2795`） | `crates/flpdf/src/writer/encrypted_strings.rs::write_encryption_dictionary_handle`（`pub(crate)`、`crates/flpdf/src/writer/encrypted_strings.rs:313`） | prod: 2 (`crates/flpdf/src/writer.rs:5163` legacy coordinator, `crates/flpdf/src/linearization/writer.rs:2360`) / test: 0 | canonical | `crates/flpdf/src/writer/encrypted_strings.rs::write_encryption_dictionary_handle` | 出力位置も qpdf と一致: legacy coordinator は body 全 object の後・`let xref_offset = bytes.len();` の直前（`crates/flpdf/src/writer.rs:5155-5170`）、linearized は part4 の直後。**plain pipeline は暗号化経路を一切持たない**（`plain::eligible` が `encrypt.is_none() && copy_encryption.is_none() && !pdf_is_encrypted` を要求する、`crates/flpdf/src/writer/plain/mod.rs:50-62`）ので、暗号化された非 linearized 出力は必ず legacy coordinator を通る |
| D16 | `QPDFWriter::setEncryptionParametersInternal` / `setEncryptionParameters` / `copyEncryptionParameters` | `libqpdf/QPDFWriter.cc:777-840,591-648,651-702` | `crates/flpdf/src/writer.rs::WriterSetupState`（`pub(crate)`、`crates/flpdf/src/writer.rs:2735`）→ `EncryptionParameters::into_context` | prod: shared setup 1 (`crates/flpdf/src/writer.rs`), route consumers 2 (`crates/flpdf/src/writer.rs`, `crates/flpdf/src/linearization/writer.rs`) / test: 1 | mixed | `crates/flpdf/src/writer.rs::build_writer_setup` | qpdf は 3 つの setter が `setEncryptionParametersInternal` 1 本へ収束し、以後は `m->encryption_dictionary` という単一 state。flpdf も qpdf 形状の option 正規化後に `WriterSetupState` が `EncryptionParameters` を一度だけ構築し、standard / linearized は各 route の `/Encrypt` slot だけを割り当てて `EncryptionParameters::into_context` から同じ辞書・file key・ID0・metadata state を使う。`EncryptionContext` は route-specific slot を含む出力 consumer state であり、setter の再構築 owner ではない。`/Encrypt` の最小版数決定（R6→1.7 ext8 等）は `crates/flpdf/src/writer.rs::encryption_version_floor` に分離されている |
| D17 | `QPDFWriter::setDataKey` / `pushEncryptionFilter`（object ごとの data key） | `libqpdf/QPDFWriter.cc:843-847,976-1000` | `crates/flpdf/src/writer/encryption_state.rs::WriterEncryptionState`（`pub(crate)`、`crates/flpdf/src/writer/encryption_state.rs:40`） | prod: 5 (`crates/flpdf/src/writer/encrypted_strings.rs:32,49,239`, `crates/flpdf/src/writer/encryption_state.rs:48`, `crates/flpdf/src/writer.rs:3191`) / test: 9 (`crates/flpdf/src/writer/encryption_state.rs`) | canonical | `crates/flpdf/src/writer/encryption_state.rs::WriterEncryptionState` | `set_data_key` / `with_object_data_key` が qpdf の set/unparse/clear 順序を写す。`docs/qpdf-correspondence.md:389` §3 に既存記載あり |
| D18 | `QPDFWriter::writeLinearized`（production 経路） | `libqpdf/QPDFWriter.cc:2537-2904` | `crates/flpdf/src/linearization/writer.rs::write_linearized_for_pdf_writer`（`pub(crate)`、`crates/flpdf/src/linearization/writer.rs:3179`） | prod: 1 (`crates/flpdf/src/writer.rs:924`) / test: 0 | canonical | `crates/flpdf/src/linearization/writer.rs::write_linearized_for_pdf_writer` | production の linearized 出力はこの 1 本のみ。`PdfWriter::write` は `emit_canonical_pdf` より前に分岐するので、linearized は plain / legacy / pclm のどれとも合流しない |
| D19 | 同上（plan/renumber を外から与える test 用入口） | `libqpdf/QPDFWriter.cc:2537-2904` | `crates/flpdf/src/linearization/writer.rs::write_linearized`（`pub(crate)`、項目単位 `#[cfg(test)]`） | prod: 0 / test: 3 (`linearization/show.rs:1999`, `linearization/back_patch.rs:382,754`) | canonical | `crates/flpdf/src/linearization/writer.rs::write_linearized_for_pdf_writer` | canonical implementation に直接委譲する test scaffolding として分類。旧表の production bridge/deletion 候補という扱いを訂正する。`back_patch.rs:382` は back-patch 前の `LinearizedDocument` を検証するため、この観測点を持つ。`PdfWriter::write` は内部で back-patch まで行うので、単純な入口置換はテストの責務を失う。`:754` は error、`show.rs:1999` は出力 fixture を観測する。別の PDF 表現や legacy semantics を維持している経路ではない |
| D20 | `writeLinearized` の 2 パス構造（pass1 → hint 1 回 → pass2、反復なし） | `libqpdf/QPDFWriter.cc:2656-2904`、特に `libqpdf/QPDFWriter.cc:2864-2884` | `crates/flpdf/src/linearization/writer.rs::write_linearized_for_pdf_writer` 内の `do_write_pass` 2 回（`crates/flpdf/src/linearization/writer.rs:3889` pass1 / `crates/flpdf/src/linearization/writer.rs:4406` 付近 final） | prod: 1 / test: 0（D18 と同一関数） | canonical | `crates/flpdf/src/linearization/writer.rs::write_linearized_for_pdf_writer` | `flpdf-26l3`（収束ループ廃止）は解消済み。pass 1 → hint 構築 → pass 2 の 2 パス構造である。`hint_shared.rs:1081,1089,1103` と `part1.rs:25` に残る convergence-loop コメントは stale と確認した。最終 `first_object_number` は `SharedObjectHintTable::from_plan`（`hint_shared.rs:311`）が member/container map から算出し、`linearization/writer.rs:4152` がその map を渡す。`:4298` は番号を読み location を更新するだけで、後段の番号 patch は不要。U6 と既存 `flpdf-o99` の古い前提を参照 |
| D21 | `QPDF::optimize` / `filterCompressedObjects`（linearized の object-user map） | `libqpdf/QPDF_optimization.cc:57-118,340-381` | `crates/flpdf/src/optimization.rs::Optimization`（`pub(crate)`、`crates/flpdf/src/optimization.rs:21`） | prod: 10 (`crates/flpdf/src/linearization/plan.rs:879,1001,2395,2480,2836`, `crates/flpdf/src/linearization/check.rs:335,410,465`, `crates/flpdf/src/linearization/writer.rs:3370`, `crates/flpdf/src/optimization.rs:32`) / test: 1 | canonical | `crates/flpdf/src/optimization.rs::Optimization` | `docs/qpdf-correspondence.md` §4 が ✅ 済みと記載し、実装も 1 モジュールに集約されている。`prepare_for_linearized_write`（`crates/flpdf/src/optimization.rs:152`）は `optimize` と `prepare_pdf` を共有する部分適用で、prod caller は `crates/flpdf/src/linearization/writer.rs:3370` の 1 箇所。D27 follow-up の stream-parameter probe も `Optimization::update_object_maps` の callback 内でだけ実行し、qpdf の page/trailer/root 起点の到達範囲を越えて `pdf.object_refs()` を解決しない |
| D22 | `QPDF::getLinearizedParts` / `calculateLinearizationData`（part4/6/7/8/9） | `libqpdf/QPDF_linearization.cc:1435-1449,963-1403,1174-1336` | `crates/flpdf/src/linearization/plan.rs::LinearizationPlan`（`pub`、`crates/flpdf/src/linearization/plan.rs:744`） | prod: 21 (`crates/flpdf/src/linearization/writer.rs`=9, `crates/flpdf/src/linearization/hint_page.rs`=4, `crates/flpdf/src/linearization/{plan,renumber}.rs`=各3, `crates/flpdf/src/linearization/{hint_shared,part1}.rs`=各1) / test: 57 (10 files) | canonical | `crates/flpdf/src/linearization/plan.rs::LinearizationPlan` | `from_pdf_with_writer_options`（`crates/flpdf/src/linearization/plan.rs:959`）が production 入口。part 分類は qpdf の `lc_*` 集合を写している |
| D23 | `doWriteSetup` の ObjStm 除外（linearized なら page + root、encrypted なら root） | `libqpdf/QPDFWriter.cc:2140-2158` | `crates/flpdf/src/writer/object_streams/planning.rs::filter_objstm_batches_for_output`（`pub(crate)`、`crates/flpdf/src/writer/object_streams/planning.rs:165`） | prod: 1 (`crates/flpdf/src/writer.rs:3889` legacy coordinator) + re-export 1 / test: 0 | mixed | `crates/flpdf/src/writer/object_streams/planning.rs::filter_objstm_batches_for_output` | `.48.54` で specialized coordinator、linearized Preserve、linearized Generate の全てが `filter_objstm_batches_for_output` を共有し、page dictionaries/Catalog の除外を一正本から適用する。plain pipeline は呼ばないが、`plain::eligible` が encrypted を除外し linearized は別経路なので `output_linearized \|\| output_encrypted` の組み合わせは各 route の呼び出し側で固定する。 |
| D24 | `QPDFWriter::enqueueObjectsPCLm` + `writeStandard`（PCLmだけが初期seedとphysical emissionを専用化） | `libqpdf/QPDFWriter.cc:2928-2954,2991-3044` | `crates/flpdf/src/writer/pclm.rs::Plan`（`Plan::build`）→ `crates/flpdf/src/writer.rs::write_pclm`（`:3544`） | prod: PCLm route 1 / test: PCLm live callback + route contracts | canonical | `crates/flpdf/src/writer.rs::write_pclm` | qpdfのPCLmは page → contents → strip/synthetic → root を専用の初期seedでqueueへ入れた後、共通`writeObject`/xref/trailerへ戻る。flpdfは`pclm::Plan`/`EmissionQueue`が同じseedとemission-time child discoveryを持つ一方、`write_pclm`自身がPCLm body loop・classic xref・trailerを`OutputSink`へ書く。plain::write_plain/emit_liveとは共有しない。 |
| D25 | `QPDFWriter::prepareFileForWrite` と root `unparseObject` の ADBE reconciliation | `libqpdf/QPDFWriter.cc:1347-1435,1773-1794,2036-2056` | `crates/flpdf/src/writer.rs::prepare_file_for_write` と全 writer root consumer の `ObjectWriterEmission::output_root_copy_with_adbe` | prod: common preparation 1 + non-linearized root consumers（plain / specialized / PCLm / remaining planned coordinator）+ linearized pass1/pass2。legacy ADBE helper caller 0 | canonical | `crates/flpdf/src/writer/object.rs::root_output_copy_with_adbe` | `.48.60` の linearized、`.48.s07c` の specialized standard、`.48.86` の PCLm、ay5b bounded QDF/normalize は output-time root copy を使用する。`.60` ではさらに encrypted QDF/normalize の source-ObjStm-free bounded cohortをlive bodyへ接続し、残るlegacy coordinatorも indirect Root・ObjStm member・direct trailerの3境界を同じcopyへ切替え、inject/strip/snapshot/restoreの定義・production callerを撤去した。Generate/source-ObjStm-bearing Preserve、direct Rootのplanned queue、その他暗号化系のchild discovery／packingはD2/D3/D11のmixed残スコープであり、D25のroot ownership移行を全writer parityとは扱わない。`prepare_file_for_write` の恒久的directizationとfixDanglingReferencesは維持する。 |
| D26 | `QPDFWriter::initializeSpecialStreams`（page seq / contents seq / normalized streams） | `libqpdf/QPDFWriter.cc:1912-1936`、トリガは `libqpdf/QPDFWriter.cc:2113-2115` | `crates/flpdf/src/writer.rs::initialize_special_streams`（`PdfWriter::write` が setup で呼び、`emit_canonical_pdf_with_special_streams` が specialized consumer として受け取る） | prod: 1 (`crates/flpdf/src/writer.rs` の `PdfWriter::write`) / test: 3（direct wrapper と setup snapshot tests） | mixed | `crates/flpdf/src/writer.rs::initialize_special_streams` | qpdf と同じ `qdf \|\| content_normalization \|\| decode_level != None` trigger で、修復済み page snapshot から 3 map と direct-content container set を一度だけ生成する。QDF の `page_seq` / `contents_seq` と stream policy の normalized set は同じ state を参照し、normalized set の適用自体は `content_normalization` gate に限定する。`flpdf-0s1ey` で linearized consumer もこの setup state を writer-side normalization enabled のまま消費し、事前正規化済み handle markerで二重tokenizeを避ける。 |
| D27 | `enqueueObject` による到達性（qpdf に独立した削除パスは無い） | `libqpdf/QPDFWriter.cc:1072-1141,2907-2925` | `sweep_unreachable_objects` と multi-source merge 専用の `sweep_unreachable_objects_except` はともに撤去済み。書き込み時の canonical owner は `crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber` | pre-write route: prod 0 / test 0 | canonical | `crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber`（書き込み経路の到達性） | pre-write sweep の撤去という本行の責務は完了。`sweep_unreachable_objects` と `_except` は Rust 全域 0 hit（2026-09-06）。closed `flpdf-3yn9.44` / `.45` が single/multi-source consumer の撤去、`.44.1` / `.44.1.1` が reachable stream probe の移行を所有する。新たな sweep cleanup は不要。書き込み前採番 walk と実際の emission の統合は D2/D3/D11 の別責務なので、本行の完了を writer 全体の単一 owner 完了とは扱わない |
| D28 | `getCompressibleObjGens` と writer到達性・stream parameter処理の分離 | `libqpdf/QPDF.cc:2393-2474`、`libqpdf/QPDFWriter.cc:1953-1958` | `crates/flpdf/src/writer/object_streams/eligibility.rs::compressible_objgens_qpdf_plan` → setup `crates/flpdf/src/writer.rs::WriterSetupState` → `crates/flpdf/src/writer/plain/plan.rs::PlainWritePlan` / `crates/flpdf/src/linearization/plan.rs::LinearizationPlan` | Generate candidate: setup-time document owner → plain/linearized planned consumer / remaining writer rewalks | mixed | `crates/flpdf/src/writer/object_streams/eligibility.rs::compressible_objgens_qpdf_plan` owns candidate selection | qpdfのEncrypt取得→getObjectCount→LIFO walkを1本で行い、plain Generateとlinearized Generateは`prepareFileForWrite`前の同じsnapshotを消費する。linearized planはdirectization後もsetup-time eligible refsをobject universeへ保持する。snapshot無しのtest-only plan fallbackだけがpost-prepare rewalkを使う。writerのstream-parameter処理、採番reachability再walk、Preserve intersectionは別責務であり、候補選択へ混合しない。 |
| D29 | `enqueueObject` の container-first を Preserve に適用（#1486 のスコープ） | `libqpdf/QPDFWriter.cc:1072-1118,1057-1069` | plain Disable/unnormalized Preserve/非暗号・非QDF・非normalize Generate は `writer/plain/body.rs::LiveQueue`、planned ObjStm consumerは `writer/rewrite_renumber.rs::ObjectStreamRenumber` | mixed | mixed | plain live `LiveQueue`（bounded standard consumer） | plain Disableのcontainerなし到達順とchild発見は移行済み。`.48.65` で unnormalized Preserveをlive queueへ接続し、`.48.87` で対象Generateの Generated groupも同じcontainer-first reservationへ接続した。QDF/normalize Generate、source ObjStm-bearing PreserveのQDF/normalize、linearized、その他のlegacy coordinatorは後続sliceに残る。 |
| D30 | `write()` から出力バイトへの単一 pipeline（`QPDFWriter::write` の後段） | `libqpdf/QPDFWriter.cc:2196-2213` | `crates/flpdf/src/writer.rs::write_qpdf_to_memory`（`pub(crate)` + 項目単位 `#[cfg(test)]`、`:29`） | prod: 0 / test: 15 (`job/{page_subset,rotate,acroform_field_prune}.rs`, `pages/tree_rebuild.rs`, `page_annotation_flatten.rs`, `page_splice.rs`, `embedded_files.rs`) | canonical | `crates/flpdf/src/writer.rs::PdfWriter::write` | canonical writer lifecycle を使う byte-neutral test scaffolding。`:29-41` は `PdfWriter::new` → configure → memory sink → `write` → `get_buffer` だけで、表現変換や旧 semantics の保存をしない。旧 bridge 分類を訂正し、削除専用 issue は不要とする。CLI にある同名の別関数はこの test-only helper の production caller ではない |
| D31 | `QPDFWriter::preserveObjectStreams` の linearized 側適用（`doWriteSetup` で Preserve を決めた後、`writeLinearized` が `object_to_object_stream_no_gen` として使う） | `libqpdf/QPDFWriter.cc:1939-1967,2541,2575-2617` | `linearization/plan.rs::objstm_batches_preserve`（`:2519`）→ `route_objstm_containers` | `objstm_batches` の Preserve arm が呼び、`linearization/writer.rs::ObjStmLayout::resolve_batches`（`:151`）が結果を使用 | mixed | `QPDFWriter::preserveObjectStreams` 相当の共有 membership owner が必要（D6） | unknown は source 調査で解消。`.48.54` で `:2529-2578` の raw-xref 再構築を document-owned `plan_qpdf_preserve_object_streams_with_unreferenced` へ置換し、assigned/signature filtering と part routing だけを linearization consumer に残した。linearized Generate の page/Catalog erase も同じ `filter_objstm_batches_for_output` に移行した。qpdf は setup 時の source map と ObjGen 昇順逆 map を linearization に渡す（`QPDFWriter.cc:1939-1967,2159-2170,2541`）。**2026-09-10（`flpdf-oq7g`）**: pre-/O の source-container / plain open-document emission を assigned object number 順へ統合し、source ObjStm 内の `/OpenAction` action dict と plain JS stream の順序を qpdf と一致させた。`objstm-lin-openaction-preserve-bearing.pdf` の full-byte RED/GREEN を追加。|

## 2026-09-13 current-main reconciliation after #1857/#1858/#1860/#1861/#1862 (`flpdf-3yn9.48.89`, `flpdf-2zgsh`)

上の D2/D3/D5/D7/D8/D11 には、各 bounded slice 前の履歴 snapshot が一行に
残っている。current `origin/main=f08ecba2e` では、次の記述を正本とする。

* PCLmを除く非linearized standardは `crates/flpdf/src/writer/plain/mod.rs::write_plain` から
  `crates/flpdf/src/writer/plain/body.rs::emit_live` へ一回だけ入り、Disable、Preserve、
  Generate、QDF、normalize、明示／入力暗号化を同じ live queue／body／xref-trailer ownerで
  処理する。`options.pclm` は `emit_canonical_pdf_inner` から
  `crates/flpdf/src/writer.rs::write_pclm`へ分岐し、`pclm::Plan`/`EmissionQueue`、PCLm body、
  classic xref/trailerを専用に持つ。共有するのはOutputSinkとObjectHandle serializerの
  一部であり、plain::write_plain/emit_liveのownerではない。
* qpdfのPCLmも `writeStandard` 内で `enqueueObjectsPCLm` の専用seedを選ぶ
  （`libqpdf/QPDFWriter.cc:2928-2954,2991-3044`）。したがってこの専用routeは未実装の
  legacy fallbackではない。PCLmのlate trailer referenceは `writer.rs:3682-3711` の
  route-local mapで、qpdf setupがPCLmで`qdf=false`/decode none/compress off/encryption off
  にする（`QPDFWriter.cc:2072-2076`）ため、plain helperのQDF専用のstream length-holder
  `+2`、XRef stream除外は適用しない。この境界をh5yreの文書受入として記録する。
* `QPDFWriter::generateObjectStreams` の責務は候補走査、even split、fresh container allocation
  に分かれる。flpdfでは候補を
  `writer/object_streams/eligibility.rs::compressible_objgens_qpdf_plan`、分割を
  `even_split_into_streams`、nonlinearized Generateのfresh sourceを
  `PdfWriter::write`のsetup、linearized Generateのgroup/slotを
  `linearization/{plan,writer,renumber}.rs`がそれぞれ所有する。linearizedのpart7/8/9 slot、
  two-pass body、hint/xrefはdedicated ownerであり、`.48.87`、`.48.88`、lz4a/#1858でplannedの
  ままではない。
* #1857 の streaming refactor 後も、上記の route 分離自体は変わらない。残る
  `mixed` は「未実装」の意味ではなく、qpdf の standard／PCLm／linearized が異なる
  physical layout と setup責務を持つこと、ならびに未着手の別 follow-up を表す。
  `D1/D2/D3/D5/D8/D11/D12/D14/D26/D28/D29/D31` の classification をこの
  bounded reconciliationだけで canonicalへ一括変更しない。

この section は上記履歴行の caller／planned wording を current-main の実装へ
再アンカーするものであり、全 route matrix の mixed/bridge 解消や full-writer
parity を主張しない。checker の current logical aggregate は README §1 の
259 rows（canonical 119 / mixed 128 / bridge 12 / unknown 0）である。

**PCLm root/late-trailer boundary (current main):** PR #1861 (`flpdf-ccij8`) now sends
only an indirect source `/Root` through `output_root_copy_with_adbe`; the direct-root
non-reconciliation contract remains. This fixes the PCLm root regression without making
PCLm share the plain live body owner. PCLm's local late-reference map is intentionally
non-QDF (`qdf=false`) and is separately covered by the PCLm late-number tests; it must not
be described as the QDF `extend_late_trailer_map` behavior.

**D14 current slice (2026-09-07):** `writer/object.rs::TrailerKind` and
`ObjectWriterEmission::write_trailer_with_ref_map_and_kind` now own the qpdf
normal/QDF/linearized-form contract. `writer/plain/plan.rs::PlainWritePlan`
retains the live trimmed trailer handle and source-to-output map, and
`writer/plain/xref.rs::append_xref_and_trailer` is the first
production consumer for classic xref. Xref rows and `startxref` remain local
to the plain physical writer. Specialized/legacy xref-stream and
linearized callers remain explicit follow-up consumers; they must not recreate
the semantic trailer loop.

**A6/A7/A8 first writer cohort (2026-09-08, `.48.32`):** production
`writer/plain/{body,plan}.rs` no longer uses `Pdf::resolve` as a separate
prelude. Direct-seed traversal uses the resolving `try_*` accessors, and
emission-only handles use the canonical handle resolver directly before the
existing writer serializer. `python3 scripts/qpdf-route-callers.py` reports
no production `Pdf::resolve` caller in `writer/plain`; linearization and the
other writer cohorts remain explicit follow-up scope. This preserves qpdf's
accessor ordering (`libqpdf/QPDFObjectHandle.cc:240-446,965-989`) without a
workspace-wide mechanical conversion.

## WriterOptions と route の対応

dispatch は 2 段。まず `PdfWriter::write`（`crates/flpdf/src/writer.rs:719-798`）が
`settings.linearization`（= `WriterOptions` ではなく `WriterSettings`、
`crates/flpdf/src/writer/settings.rs:38`）を見て linearized を切り離し、非 linearized だけが
`emit_canonical_pdf` → `emit_canonical_pdf_inner`（`crates/flpdf/src/writer.rs:3494-3804`）へ入る。
`emit_canonical_pdf_inner` は、まず `options.pclm` なら
`crates/flpdf/src/writer.rs::write_pclm`、それ以外の全ての非linearized standard modeなら
`crates/flpdf/src/writer/plain/mod.rs::write_plain`へ1回だけ委譲する。後者は
`crates/flpdf/src/writer/plain/body.rs::emit_live`のlive queue、direct stream policy、
xref/trailer sinkを共有する。

`plain::eligible`（`crates/flpdf/src/writer/plain/mod.rs:405-421`）はsetup時のGenerate
membership snapshot最適化に使う補助条件であり、非linearized routeをlegacyへ振り分ける
dispatch条件ではない。force<1.5によるObjStm抑制、deterministic/static ID、
preserve-unreferenced、decode/compress/newline policy、暗号化状態は、同じplain live
consumerへ渡すeffective writer optionsとして処理される。

| 条件（上から順に最初に一致したもの） | route | 実体 |
|---|---|---|
| `WriterSettings::linearization == true` | `write_linearized` | `crates/flpdf/src/linearization/writer.rs::write_linearized_for_pdf_writer`（`crates/flpdf/src/writer.rs:773`。`options.qdf` はここで強制的に false にされる） |
| `options.pclm == true` | `pclm` | `crates/flpdf/src/writer.rs::write_pclm`（dispatch は `emit_canonical_pdf_inner`） |
| `options.qdf == true` | shared plain live QDF | `crates/flpdf/src/writer/plain/mod.rs::write_plain` → `crates/flpdf/src/writer/plain/body.rs::emit_live`（QDF length holder/page context） |
| `options.content_normalization == true` | shared plain live normalize | `crates/flpdf/src/writer/plain/mod.rs::write_plain` → `crates/flpdf/src/writer/plain/body.rs::emit_live` |
| `options.encrypt.is_some()` / `options.copy_encryption.is_some()` | shared plain live encrypted | `crates/flpdf/src/writer/plain/mod.rs::write_plain`（bodyのencryption contextとlate trailer map） |
| 入力が暗号化されている（`pdf.is_encrypted()`） | shared plain live | `crates/flpdf/src/writer/plain/mod.rs::write_plain` |
| `options.object_streams` が Disable / Preserve / Generate | shared plain live | `crates/flpdf/src/writer/plain/body.rs::LiveQueue`（setup membershipがあればcontainer-first） |
| extra header、deterministic/static ID、decode/compress/newline policy | shared plain live | `crates/flpdf/src/writer/plain/mod.rs::write_plain`（各policyを同じOutputSinkへ渡す） |

## Current owner table after the integrated writer slices

The old `PlainWritePlan`/specialized-coordinator table above the `origin/main=4a2faf5c`
snapshot is historical. The current production boundaries are:

| current route | setup / membership owner | body / emission owner | late trailer numbering |
|---|---|---|---|
| non-linearized standard except PCLm | `writer.rs` の `build_writer_setup` が Preserve の `source_object_stream_data` snapshot と Generate setup を持ち、`plain::build_live_object_stream_plan` は渡された snapshot を消費する | `plain::write_plain` → `plain::emit_live` → `LiveQueue`/`WriteObject` | shared `plain::extend_late_trailer_map` and direct-root assignment |
| PCLm | `pclm::Plan::build` → `pclm::EmissionQueue` | `writer.rs::write_pclm` own page/content/strip/synthetic/root loop, classic xref and trailer; shared OutputSink/object serializer only | route-local map in `writer.rs:3682-3711`, intentionally `qdf=false`; no QDF length-holder `+2` or XRef exclusion |
| linearized | `linearization::plan`/writer setup | dedicated two-pass `linearization` writer | linearization-specific trailer/hint owner |

The table separates owner boundaries; it does not change the row classifications or claim
that all writer routes are byte-parity complete.

## unknown / probe

| 項目 | 決められない理由 | 必要な source / probe |
|---|---|---|
| U1（D31、source 調査済み）: linearized Preserve の共有 owner | `linearization/plan.rs:2519-2611` は source-index 順で独立に membership を構築し、plain の共有 Preserve と同じ owner ではない。D31 を unknown から mixed に変更 | source-index と objgen の順序が異なる入力、stale generation、preserve-unreferenced を実 qpdf と比較する canonical RED を用意し、共有 membership → linearized routing の順で移行する。既存 `flpdf-oq7g` にも既存 strict byte tests の存在を反映する |
| U2（source で解決）: decode-only と normalized set | `normalized_streams` を読むのは `QPDFWriter.cc:1279` の `normalize_content && normalized_streams.count(old_og)`。decode-only ではこの集合を参照しない。page/content seq map は QDF コメントで使う（`:1774-1781`） | D26 で setup snapshot の owner を `initialize_special_streams` に統合済み。decode-only でも page repair と map generation の trigger は維持し、normalized set の適用だけ `content_normalization` gate に残す。`flpdf-0s1ey` で linearized consumer の state wiringも修正済みで、事前正規化済みhandle markerを同じ per-stream policyへ渡す |
| U3（producer 側は未確定）: classic xref の欠番 | 正常な writer 呼び出しから非zero欠番を作れるかは未検証。ただし qpdf の error 契約は確定: `QPDFWriter.cc:2368` → `QPDFXRefEntry.cc:27-32` は type≠1 を `std::logic_error` とする。fake free row の符号化を選ぶメンテナ判断は不要 | canonical `writeXRefTable` の範囲・type契約を RED/GREEN で移植し、通常の欠番は `Error::Internal` にする。欠番を生む producer が見つかった場合はその採番/emission不変条件を別 slice で修復する。xref-stream の type0 および linearization pass1 の suppress_offsets と混同しない |
| U4（caller 調査済み）: test-only 入口 | D30 は `PdfWriter` に直接委譲する15 test callerの補助。D19 は3 test callerで、`back_patch.rs:382` が back-patch 前の文書を必要とする | 両者を production bridge の撤去対象から外す。byte-neutral test scaffolding として canonical 経路を使う分類に訂正し、単なる zero-caller 化を目的に受入れテストの観測点を失わない |
| U5（解消済み）: D27 の削除 sweep | `sweep_unreachable_objects` と `sweep_unreachable_objects_except` は両方とも Rust 全域で0 hit。closed `flpdf-3yn9.44/.45/.44.1/.44.1.1` が撤去と後続到達性修正を所有 | 削除済み実装への marker追加・新規cleanupは不要。D2/D3/D11 の残る先行採番/emission責務を別に追跡する |
| U6（source で解決）: hint の最終番号の owner | `linearization/writer.rs:4152` が `SharedObjectHintTable::from_plan` に member/container map を渡す。`hint_shared.rs:311` が最終番号を算出し、writer `:4298` はその番号を読んで location だけ更新する | 4つの convergence-loop コメントは stale doc。最終値の書き込み元不在という live bug はこの箇所にはない。`flpdf-o99` の「packing未実装」「writerが番号を後patch」という古い前提も見直す。既存テストの現時点の合否は今回未測定 |


## 2026-09-06 再監査の issue 対応

親 epic は `flpdf-3yn9.48`。下表は責務と実装 issue の対応であり、完了状態は `bd show <id>` で確認する。
各 issue の受入条件に qpdf 根拠、最初の consumer、残 caller と削除条件を記録した。

| 対象行 | Beads issue | 責務 / 移行 slice |
|---|---|---|
| `D17` | `flpdf-3yn9.48.39` | reader/writer のcompute_data_keyをqpdf共通primitiveへ統合する |
| `D6` / `D9` | `flpdf-3yn9.48.50` | QPDF::getObjectStreamData のsource mappingを移植しplain Preserveを接続する |
| `D5` / `D7` | `flpdf-3yn9.48.51` | generateObjectStreams のnull-container identityを移植しplain Generateを接続する |
| `D8` / `D28` | `flpdf-3yn9.48.52` | getCompressibleObjGensのwalk・stale object除去を一正本へ移植する |
| `D1` / `D2` / `D3` / `D11` / `D24` / `D29` | `flpdf-3yn9.48.53` | enqueueObject/unparseChild/writeStandardのlive queueを移植しplain Disableを接続する |
| `D6` / `D23` / `D29` / `D31` | `flpdf-3yn9.48.54` | preserveObjectStreams/setup membershipを正本化しspecialized・linearizedへ移行する |
| `D11` | `flpdf-3yn9.48.55` | QPDFWriter::writeObjectの共通emissionを移植しplain bodyから接続する |
| `D14` | `flpdf-3yn9.48.56` | QPDFWriter::writeTrailerのnormal/linearized契約を共有ownerへ移植する |
| `D12` | `flpdf-3yn9.48.57` | writeXRefTableを忠実移植し独自missing free-row出力を撤去する |
| `D13` | `flpdf-3yn9.48.58` | writeXRefStreamのlayout契約を共有ownerへ統合する |
| `D1` / `D25` | `flpdf-3yn9.48.59` | prepareFileForWriteのgraph準備を分岐前に一度だけ実行する |
| `D25` | `flpdf-3yn9.48.60` | root unparseObjectへADBE出力処理を移しsnapshot/restoreを撤去する |
| `D14` | `flpdf-nhet` | `getTrimmedTrailer` の xref-structural key と source trailer key の境界を揃え、`/F`・`/FFilter`・`/FDecodeParms` の到達性・late numbering を保持する |
| `D14` | `flpdf-53t8k` | specialized standard live の body 後 trailer walk に `/Root` 前後の late indirect-reference numbering を接続する |
| `D1` / `D2` / `D3` | `flpdf-pphb7` | plain writer の outer dispatch と内部 consumer選択を `PlainRoute` の挙動分類へ集約し、text-slice route contractを撤去する |
| `D2` / `D3` / `D5` / `D7` / `D8` / `D11` | `flpdf-3yn9.48.87` | 非暗号・非QDF・非normalize の plain Generate を setup snapshot付き specialized standard live queueへ接続する |
| `D25` | `flpdf-mcjj` | PCLm の direct `/Root` では qpdf の `is_root` 判定を満たさないため ADBE reconciliation を適用しない |
| `D26` | `flpdf-3yn9.48.61` | initializeSpecialStreamsのpage/content/normalized mapをsetupで一度生成する |
| `D16` / `D1` | `flpdf-3yn9.48.62` | encryption設定・doWriteSetupを単一writer stateに揃える |
| `D3` | `flpdf-3yn9.48.63` | linearizationの採番engineを使ったcache warmup迂回を撤去する |
| `D11` | `flpdf-3yn9.48.64` | unparseObjectのrefiltered stream辞書処理をqpdfの責務に揃える |
| `D1` / `D2` / `D3` / `D11` / `D24` / `D29` | `flpdf-3yn9.48.65` | live writer queueへ残Preserve/Generate・specialized・QDF・PCLm consumerを段階移行する |
| `D31` | `flpdf-oq7g` | linearized Preserve のbyte-parity受入拡張 |
| `D20` | `flpdf-o99` | shared hint container番号のwriter統合試験 |
| `D11` | `flpdf-vo76` | 外部stream Lengthの既存受入 |

## 2026-09-12: specialized standard live-queue slice (`flpdf-s07c`)

`flpdf-s07c` は、`qdf=false`・`content_normalization=false`・`pclm=false` の
non-linearized specialized standard cohortを、qpdfの増加する object queueへ接続した。
`crates/flpdf/src/writer.rs::emit_canonical_pdf_inner` が
`ObjectStreamMode::Disable/Preserve/Generate`、明示暗号化、source encryptionの
preserve/decrypt、extra headerをこのcohortの入口として持ち、bodyは
`crates/flpdf/src/writer/plain/body.rs::emit_live` の
`LiveQueue` → `WriteObject` → dynamic child mapを通る。

qpdfの `enqueueObject` / `unparseChild` / `writeStandard`
（`libqpdf/QPDFWriter.cc:1072-1157,1761-1809,2907-3044`）に合わせ、採番は
child tokenを実際にunparseする時点で行う。Generateの候補membershipは
`prepareFileForWrite` より前のsetup snapshotを使用し、rootのADBE output
shallow-copyが置換した旧childを、queueが見ていない限り発見・保持しない。
ObjStmはsource-backed Preserveまたはqpdfのfresh null placeholderを
`Generated` groupとして同じqueueに登録し、container first / member range
reservation / encrypted container framingを共有する。

今回のREDは、specializedのprogress callbackがまだqueueにない `/Pages` childを
追加すると固定renumber mapが `absent from renumber map` で失敗すること。GREEN後は
そのcallback回帰、encrypted全3 ObjStm mode、`adbe-orphan-url.pdf` の
qpdf 11.9.0 byte parity（Disable/Preserve/Generate）、encrypted sourceの
decrypt/preserve matrixを確認した。

このsliceでD2/D3/D11の **specialized standard** はliveになったが、QDF/normalize
（`flpdf-ay5b`）と linearized の別consumerは残る。PCLm は別の shared-live
consumer slice (`flpdf-3yn9.48.86`) に切り出した。RootのADBE ownership自体は
各consumerの output-time shallow copyへ集約し、旧inject・strip・snapshot・restore
bridgeは `.60` で撤去する。Generate/source-ObjStm-bearing Preserve、暗号化系の
child discoveryとObjStm packingはD2/D3/D11のmixed残スコープであり、Root ownerの
caller-zeroだけからroute matrix全体のbridge/mixed解消や全writer parityを主張しない。

後続のoracle再確認で、specialized consumerの境界もqpdfに合わせて補正した。
GenerateのObjStm placeholderは`getObjectCount`より前にsetupで確保し、progressの
first/final passは`writeObject`と同じ`indicateProgress(false, false)`位置で動かす。
`EncryptMetadata=false`は`/Type /Metadata`のstreamだけをcleartextにし、通常の
direct Streamは暗号化する。Preserveのstale generation removalはGenerateと同じく
emissionまで渡し、classic xref trailerのdirect Rootもdynamic serializerを通して
payload/framingを保持する。これらはQPDFWriter.cc:1251-1278,1639-1707,
1953-1966,1998-2004,2189-2195,3023-3031に対応する。

## 2026-09-13: PCLm shared live consumer (`flpdf-3yn9.48.86`)

PCLm の production route は `writer/pclm.rs::Plan` が qpdf の
`enqueueObjectsPCLm` と同じ page → `/Contents` → strip/synthetic → `/Root`
の初期seedを保持し、`writer.rs::write_pclm` が専用queueと
`writer/plain/body.rs::emit_live`相当のObjectHandle serializerをOutputSinkへ接続する。
PCLmの差分は初期seed順とPCLm固有のxref/trailer framingで、emission中のchild discovery、
direct/indirect Root、stream payloadはqpdfのPCLm writeStandard順に処理される。

RED→GREEN は progress callback で追加した indirect child の出力、間接 null
trailer value の late-number 可視性、body 後に変更された `/ID` の遅延生成を
固定した。pinned qpdf 11.9.0 の `qpdf/qtest/pclm-in.pdf` → `pclm-out.pdf`
（860952 bytes）は byte-identical。旧 `pclm::Plan` は unit-test planner のみで、
production caller ではない。D24 はこの sliceで `canonical` へ更新したが、
QDF/normalize と linearized の別 consumer、および D25 の残 helper caller は
引き続き未完了である。

## 2026-09-13: QDF/normalize live consumer (`flpdf-ay5b`)

QDF と page-content normalization のうち、`Disable` および source に ObjStm
がない `Preserve` は、先行する `PlainWritePlan` の採番を使わず、
`writer/plain/body.rs::LiveQueue` → `WriteObject::write_object` の live consumer
へ接続した。QDF は qpdf の `direct_stream_lengths=false` に対応する output-only
length holder を stream の直後に予約し、setup で生成済みの page/content map と
同じ body loop が `%% Page` / `%% Contents` と normalization policy を選択する。
QDF serializer が静的 mapを要求するため、各 `writeObject` の progress callback
後に visible child 順を走査して queue へ登録し、その後の serializer map は確定済み
queue を読む。この境界は old `/ADBE` descendants を pre-plan で保持しないため、
`adbe-orphan-url.pdf` の qpdf 11.9.0 parityを回復する。

`qdf_special_streams_tests.rs` は QDF と normalize-content の page/content fixture
群を qpdf と byte 比較し、`writer_object_emission_tests.rs` は両 mode の callback
による root child追加と stream replacement を固定する。`adbe_ext_qpdf_parity.rs`
では QDF/normalize の ADBE orphan が RED→GREEN になった。Generate と source
ObjStm-bearing Preserve は ObjStm packing/order を含む planned consumerとして
この sliceの対象外であり、暗号化入力・出力暗号化も別 consumerとして残る。

この slice により D2/D3/D11 の QDF/normalize bounded cohort は live になった。
Generate/source-ObjStm-bearing Preserve は planned consumerとして残り、暗号化の
source-ObjStm-free QDF/normalize cohortは `.60` で同じlive bodyへ接続する。

## 2026-09-13: Root ADBE helper caller-zero (`flpdf-3yn9.48.60`)

`.60` は、既に移行済みだった specialized standard・linearized・PCLm・QDF/normalize
bounded cohortに加え、encrypted QDF/normalize の source-ObjStm-free cohortをlive
bodyへ接続し、legacy coordinatorに残るRootの3つのemission境界も
`ObjectWriterEmission::output_root_copy_with_adbe`へ接続した。

* 通常bodyのindirect Catalogはbody emission時にshallow copyを作る。
* Generate/PreserveのObjStm member Catalogもmember serializerの直前に同じcopyを作る。
* direct Catalogはtrailer handleへoutput copyを渡し、classic/xref-streamのdirect-root
  spliceが同じ値を出す。

これにより `inject_adbe_extension`、`strip_adbe_extension`、
`snapshot_catalog_extensions`、`restore_catalog_extensions` と、それ専用だった
raw restoration primitiveは定義・production callerとも0になった。成功時とsink
failure時のshared direct `/Extensions` aliasは `adbe_shared_state_tests.rs` のlegacy
QDFケースで確認した。`prepare_file_for_write` のfixDanglingReferencesと恒久的な
`/Extensions`・`/ADBE` directizationは保持する。

Root ownershipの切替はD25をcanonicalへ更新するが、Generate/source-ObjStm-bearing
Preserve、direct Rootのplanned queue、暗号化系の残るchild discovery／ObjStm packingは
D2/D3/D11のmixed残差である。したがって、このsliceの完了はroute matrix全体の
bridge/mixed解消や全writer parityを意味しない。

## 2026-09-13: trailer external-file keys (`flpdf-nhet`)

qpdf 11.9.0 の `QPDFWriter::getTrimmedTrailer` は `/ID`・`/Encrypt`・`/Prev` と
xref stream の10 keyだけを除去し、`/F`・`/FFilter`・`/FDecodeParms` は通常の
trailer keyとして `writeTrailer` の sorted walk に残す
（`libqpdf/QPDFWriter.cc:1160-1236,2009-2031`）。従来の flpdf はこの3 keyまで
writer-owned structural keyとして除去していたため、trailer-only `/F` の indirect
objectを live queue／planned reachability から落とし、`/Info`・後続 trailer参照の
番号と `/Size` を qpdf とずらしていた。

`flpdf-nhet` は同じ10 key集合を normal live/planned、QDF/normalize の shared
late-trailer map、PCLm、linearized trailer-entry serializerへ適用した。repository
fixture `trailer-external-file-keys.pdf` で、qpdf 11.9.0 との normal Disable、planned
Generate、QDF Preserve、linearized の byte/status を比較し、PCLm の page/root-only
seed後に late-number される trailer reference も専用テストで固定した。これは D14
の source-key boundary 修正であり、writeTrailer の複数 consumer が残る D14 mixed
分類や route matrix 全体の parity 完了を意味しない。

`flpdf-53t8k` は nhet 後に判明した specialized standard live の late-reference
欠落を補う。qpdf は body queue と `/Encrypt` dictionary の後、`writeTrailer` の
sorted key walk で `unparseChild` → `enqueueObject` を実行する
（`libqpdf/QPDFWriter.cc:1072-1157,1160-1236,2991-3031`）。specialized の旧実装は
body mapだけを xref serializerへ渡していたため、progress callback が trailer の
`/F` に追加した indirect child が map から欠落した。現在の
`crates/flpdf/src/writer/plain/mod.rs::write_plain` と
`crates/flpdf/src/writer/plain/xref.rs::append_xref_and_trailer` は plain/PCLm の
canonical sink境界で `/Root` 前後の late map、direct `/Root` の動的 child mapを使う
ようにし、body object や xref rowを後付けせず qpdf の trailer-time numberだけを
割り当てる。xref-stream routeでは qpdf が `writeTrailer` 前に xref-stream object
自身の番号を予約するため、late mapの開始点もその予約の後へ進める。AES-128・
Disable/Generate・0% callback の RED→GREEN 回帰を `writer_object_emission_tests.rs`
に追加した。これは specialized の bounded D14
修正であり、legacy/planned/linearized の writeTrailer ownerや route matrix 全体の
mixed/bridge 解消を意味しない。

`flpdf-mcjj` は PCLm の direct-root semantics を補正する。qpdf の
`QPDFWriter::Members::root_og` は direct `/Root` では `(-1, 0)` となり、
`unparseObject` の `old_og == root_og` guard が false のため、source Catalog の
`/Extensions /ADBE` は final PDF version に調停されない
（`libqpdf/QPDFWriter.cc:53,1374-1436`）。PCLm の direct-root copy は
`output_root_copy_with_adbe(..., false)` を使い、indirect Catalog の true 経路は
変更しない。direct `/Root` に不一致の `/ADBE` を持つ fixture-built document の
PCLm regression test で、source `/1.4`・level 5 が保持されることを固定する。
これは direct-root semantics の bounded fixであり、PCLm と writer 全体の parity
完了を意味しない。

## 2026-09-13: plain route behavior classification (`flpdf-pphb7`)

この履歴上のroute classificationは、qpdfのmode判定を外側とplain consumerに
二重保持しないための中間形だった。現在は非linearized standardの全modeを
`crates/flpdf/src/writer/plain/mod.rs::write_plain`からlive consumerへ直接接続し、
PCLmだけを`crates/flpdf/src/writer.rs::write_pclm`へ分岐する。qpdfは
QDF/normalizationのmode dispatchとobject-stream setupを独立に扱う
（`libqpdf/QPDFWriter.cc:2038-2140`）ため、setup snapshotは同じwriter stateから
plain live bodyへ渡される。

入力組み合わせの unit behavior tests は Disable／Preserve（source membershipの
有無を含む）、QDF、Generate、extra header、encrypted input、requested/effective
mode不一致を直接判定する。これにより Preserveを外す、source membership条件を
後置する、QDF条件を落とす、outer dispatchだけを変える mutationを、production
source textの切り出し範囲に依存せず検出する。`plain_live_queue_route_tests.rs`
からは当該 route-selection text checksを削除し、D9 source-membership ownerの
qpdf correspondence checkと実出力のADBE regressionだけを残した。

これは route classification／testability の bounded sliceであり、各 consumerの
qpdf parityや route matrix 全体の mixed/bridge 解消を意味しない。

## 2026-09-13: plain Generate の specialized live cutover (`flpdf-3yn9.48.87`)

`.48.87` は、非linearized・非PCLm・非暗号・`qdf=false`・
`content_normalization=false` の `ObjectStreamMode::Generate`について、setupで取得した
membershipを`crates/flpdf/src/writer/plain/mod.rs::write_plain`から
`crates/flpdf/src/writer/plain/body.rs::emit_live`へ渡す境界を完成させた。
現在のplain writerはGenerateを含む全standard modeを同じlive queueで処理し、QDF/normalize、
暗号化、source ObjStm-bearing Preserveも別のlegacy coordinatorへ戻さない。

qpdf 11.9.0 は `generateObjectStreams` で compressible membershipを決め、
`makeIndirectObject(newNull())` の fresh containerを `getObjectCount` 前に source
documentへ追加する（`libqpdf/QPDFWriter.cc:1970-2006`）。flpdfも setup の同じ境界で
`compressible_objgens_qpdf_plan` → `even_split_into_streams` → fresh placeholderを
snapshotする。その後 qpdf の `enqueueObjectsStandard` / `writeStandard` は
`enqueueObject`・`unparseChild` を通る増加する queueを body emissionまで歩く
（`libqpdf/QPDFWriter.cc:1072-1157,2907-3031`）。flpdfは snapshotした
`ObjectStreamGroup::Generated`を specialized queueへ登録し、container-firstの
reservation、ObjStm emission、xref/trailerを既存 live ownerで処理する。multi-source
mergeで local object numberとqpdfのsource/occurrence orderが異なる場合も、Generated
memberは `writer_object_order_key` を通して qpdf順に並べる（通常の単一文書では
local ObjGen順）。multi-source の provenance sort は even split の後、各 group内に
だけ適用する。全候補を先にsortすると qpdfのDFS group境界を移動させるためである。

ObjStmの `/Length` は候補から「参照先object」ごとに除外するのではなく、qpdfの
traversalで stream辞書の `/Length` edgeだけをpushしない。したがって同じ holderが
別の通常辞書edgeから到達すれば `eligible` に残り、Generate ObjStmへ入る。`.48.87`
ではその alias fixtureのqpdf byte parityも固定し、`indirect_objstm_length_refs` を
後段の一律除外に使わない。

受入れは qpdf 11.9.0 の Generate 9ケース（1/2/3 page、no-stream、130 reverse、
force 1.4、missing reference、indirect length variants）の status/bytes比較と、
progress callbackが setup後に rootへ追加した `/PlainGenerateProgressChild` を live
queueが発見して出力する regressionで固定する。この時点では QDF Generate と normalize
Generateを次 sliceへ残していたが、`.48.88` で specialized liveへ cutoverした。
source ObjStm-bearing Preserve、linearized、その他の暗号化・planned consumerについては
このsliceから parity完了を主張しない。

## 2026-09-13: QDF/normalize Generate の specialized live cutover (`flpdf-3yn9.48.88`)

`.48.88` は、非 linearized・非PCLm・非暗号・effective/requested mode一致の
`ObjectStreamMode::Generate` について、QDF または content normalization が有効でも
`PlainWritePlan` の planned consumerを経由せず、`.48.87` と同じ
`emit_specialized_standard_live_with_page_context` → `LiveQueue` へ接続する。
`PlainRoute::OutsidePlain` はこの outer-dispatch境界を表し、Disable/Preserveの
QDF/normalize live cohortや、force-version suppression後の別 consumerとは混同しない。

Generate membershipは qpdf と同じ setup snapshot（`QPDFWriter.cc:1970-2006`）で
候補・even split・fresh null containerを確定し、その `ObjectStreamGroup::Generated`
を live queueへ登録する。container初回発見時の全 member予約、memberの source ObjGen順、
group境界、progress callback後の child discoveryは通常 Generateと共通である
（`QPDFWriter.cc:1057-1157,2907-3031`）。DFS候補順を split前に保持し、provenance/order
調整は各 group内に限定する。

QDFでは `writeObjectStream` の二つの passを live member serializerで実行する。
各 memberの `%% Object stream: object N, index I` と optional original-object-ID marker、
page/content commentを marker後に出力し、pair-table offsetは最初の marker直後を基準に
再構成する。containerは直接 `/Length`、`/N`、`/First` を持ち、QDFでは圧縮しない。
containerの `endobj` 後の空行も含め、`QPDFWriter.cc:1606-1809` の framingに合わせる。
member serializerは visible childを queueへ発見・採番してから QDF mapを固定し、rootなら
ADBE output copyを使う。通常 streamの output-only length holderは従来どおり保持し、
ObjStm containerには追加しない。

content normalizationでは QDF markerを追加せず、同じ live queueと generated groupを使い、
`initializeSpecialStreams` の page/content mapだけを normalization policyに適用する。
qpdf-zlib parityは `cli_pages_objstm_order_qpdf.rs` の multi-source QDF Generate と
normalize Generate、`cli_qdf.rs` の QDF Generate/batch-cap、`writer_object_emission_tests.rs`
の callback-child regressionで確認する。source ObjStm-bearing Preserveの QDF/normalize、
explicit/copy encryption、encrypted input、linearized、PCLm、force-version-suppressed
Generateはこの cutoverの対象外であり、残る mixed/bridge scopeとして扱う。

## 2026-09-13: second-half ObjStm の within-part ordering (`flpdf-lz4a`)

qpdf 11.9.0 の linearized Generate は、global even-split で作った ObjStm
containerを `filterCompressedObjects` 後の `std::set<QPDFObjGen>` と各 part の
走査規則で配置する。second half の大枠は `part7 → part8 → part9` で、part7 は
page-by-page、part9 は Pages tree → private/shared thumbnails → outlines →
残りの順である（`QPDF_optimization.cc:340-380`、
`QPDF_linearization.cc:1223-1337`）。Generate container の ObjGen は
`generateObjectStreams` の split順に新規発行される（`QPDFWriter.cc:1970-2006`）が、
part7/part9 の category 間ではその順をそのまま使えない。

そのため flpdf は Generate の part4 rest batches を qpdf の category rank
（Pages、thumbnail private/shared、outlines、remaining `lc_other`）で stable sort し、
同一 category 内だけ split順を保持する。private thumbnail の page番号も rank に含め、
shared thumbnail は `others > 0` でも qpdf の `thumbs > 1` 分類を優先する。
plain thumbnail streamも同じ分類で pre/post-container を決める。
`second_half_container_anchors` と `RenumberMap::place_objstm_members_per_half` は
この batch 順を plain object の配置へ反映する。`objstm-lin-otherpage-pages-200-100` は
異なる非先頭 page の part7 containerを
固定し、`objstm-lin-otherpage-private-250-0` は同一 page の複数 part7、
`objstm-lin-outlines-multi-1-250` は同一 part9 category内の複数 containerを固定する。
さらに `objstm-lin-part9-categories-74-225` は DFS では先に来る outlines containerと
後続の Pages/rest containerを共存させ、qpdf が Pages/rest → outlines → remaining rest
と出力する category 間順序を固定する。全 fixture の qpdf 11.9.0 goldenとの strict
byte parity、container数、round-trip、`qpdf --check-linearization`、`qpdf --check` が
成功した。shared thumbnail + document-other、Pages + shared thumbnail、single
thumbnail + Catalog document-other の境界も strict parity で固定した。.48.87 の
非 linearized pre-split/group境界検証と合わせ、h07n の
linearized within-part ordering scopeは実装・実測済みである。

## 2026-09-14: linearized raw QpdfObjGen identity (`flpdf-474u8`)

qpdf 11.9.0 の `QPDFWriter::Members::obj_renumber` は linearization pass でも
`std::map<QPDFObjGen, int>` を正本にし、`enqueueObject`、`unparseChild`、
`assignCompressedObjectNumbers`、`calculateLinearizationData` の各境界で
generation を含む identity を保持する（`include/qpdf/QPDFWriter.hh:668-670`;
`libqpdf/QPDFWriter.cc:1057-1157,2510-2654,2858`）。`discardGeneration` は hint の
object-number-only view を作る箇所に限られ、writer の canonical key ではない。

flpdf の linearized plan/renumber/writer は、`Optimization` の raw object-user map と
private raw part vectorsを `QpdfObjGen` で保持し、既存 `ObjectRef` fields は checked
projection として残す。`QpdfObjGen::to_object_ref()` が拒否する `5 65536` も raw slotへ
generation-zero output referenceを持ち、Catalog/page/trailer の child serializer は
同じ raw lookupを使う。ObjStm の eligibility は qpdf と同じく valid gen-0 memberの境界に
限定し、raw object を synthetic `ObjectRef` にして ObjStmへ押し込まない。

linearized compact/QDF、stream dictionary、encrypted string、trailer `/ID`、ObjStm
memberの両 passには `Fn(QpdfObjGen) -> ObjectRef` と raw removed setを渡す。stale
generationの removed setは一度だけ writer boundaryで構築して借用し、per-objectの
`ObjectRef` set再生成を行わない。hintの page/shared inputsもraw identityを使って
object count、shared index、pass-1 byte lengthを計算するため、raw childの first-page、
Part-8、ObjStm併用ケースで `qpdf --check-linearization` の警告を出さない。

対象は `crates/flpdf/src/optimization.rs`、`linearization/{plan,renumber,hint_page,hint_shared,writer}.rs`、
`writer/{object,encrypted_strings}.rs` と raw header regressionである。これは既存の
non-linearized raw writer routeを変更せず、linearizationだけに残っていた
`ObjectRef` narrowing gapを埋める bounded sliceである。

## 2026-09-15: linearized raw identity follow-ups (`flpdf-pwyo2`)

`flpdf-474u8` 後に残っていた linearization の raw-only consumerを、qpdf 11.9.0
の同一責務へ接続した。

- D16/D17 の stream encryptionは、`QPDFWriter::willFilterStream` の
  `/Type /Metadata` 判定（`QPDFWriter.cc:1234-1314,1537-1556`）を
  `linearization/writer.rs::append_body_object_with_raw_identity` で使う。
  `metadata_ref` の checked projection比較はraw linearizationでは使わず、raw non-metadata
  streamを誤ってcleartextにしない。
- D26 の `initializeSpecialStreams` 相当は `/Contents` の直接stream memberを
  `QpdfObjGen` で記録し、D11/D26の `skip_stream_parameters` は同じraw identityで
  `/Filter`/`/DecodeParms`をclosureから除外する。対応するqpdf箇所は
  `QPDFWriter.cc:1912-1936` と `QPDF_optimization.cc:261-333`。
- D31 のlinearization part orderは、raw Part 7をpageごとの`QPDFObjGen`集合へ
  mergeし、Part 8 shared hint entryをphysical output unit順に統合する。generated
  second-half ObjStm anchorはraw plain peerを含めた最初のcompressed member位置を使い、
  page shared identifiersはoutput番号ではなく raw `obj_user_to_objects` 順にする。
  `QPDF_linearization.cc:1228-1270,1351-1402`、`QPDFWriter.cc:1057-1118,2579-2654`。
- D20/D31 のoutline hintはraw `/Outlines` rootを `RenumberMap::new_for_raw` で解決し、
  root-firstのoutline partと連続unit countを保持する。`QPDF_linearization.cc:1406-1432,1614-1631`。

`crates/flpdf/tests/qpdf_obj_gen_header_tests.rs` と
`linearization/hint_page.rs` のraw regressionは、raw metadata、content normalizationと
parameter omission、Part 7/8 order、outline `/O`、ObjStm anchor、跨ぎpart shared-IDを
固定する。Part 8/ObjStmの生成物は pinned qpdf 11.9.0 の
`--check-linearization` を警告なしで通過する。今回の行は既存 qpdf semantics の不足を
埋めるものであり、qpdf-deviation markerを追加しない。

## 2026-09-15: linearized Generate setup-time ObjStm membership (`flpdf-9zbro`)

qpdf 11.9.0 は `generateObjectStreams` で候補と fresh null container を
`prepareFileForWrite` より前に確定する。その後の `prepareFileForWrite` は Catalog の
indirect `/Extensions` を directize するため、linearized planner が準備後の到達性だけを
再計算すると、既に ObjStm への所属を決めた旧 indirect dictionaryを落としてしまう
（`QPDFWriter.cc:1970-2006,2034-2055,2537-2645`）。

`.9zbro` は `WriterSetupState` の Generate snapshotを linearized planへ渡し、候補の
stale-generation setと even-split数を同じ setup stateから使うようにした。さらに
setup-time eligible refsを線形化 object universeへ戻すことで、directization後にも qpdfの
ObjStm memberを保持する。専用の linearization two-pass layout、hint、ObjStm emissionの
責務は従来どおり linearization ownerに残し、別の route bridgeは追加していない。

`linearize-indirect-extensions.pdf` の `--linearize --object-streams=generate`
について、実 qpdf 11.9.0 と flpdf の全出力 bytes、`/O`、ObjStm member数が一致する
回帰を `cmp_linearize_objstm_tests.rs` に追加した。対象を含む linearized ObjStm 152 tests、
linearization 63 tests、Generate 34 testsは qpdf-zlib-compat で成功し、qpdf-deviation
markerは追加していない。

## 2026-09-15: direct `/Outlines` root-first ordering (`flpdf-oqz1e`)

qpdf の `QPDF::optimize` は direct な `/Outlines` dictionary を
`makeIndirectObject` で indirect 化する。その後の `pushOutlinesToPart` は、part6 の
first-page private/shared objects の後に plain outline root を置き、続けて
`lc_outlines` に含まれる ObjStm container と outline objects を置く
（`QPDF_optimization.cc:57-82`、`QPDF_linearization.cc:1188-1216,1406-1432`）。

flpdf は `RenumberMap::place_objstm_members_per_half` に first-half の outline batch 境界と
ObjStm member ではない outline root を渡し、通常 first-page containers → outline root →
outline containers → ineligible outline streams の順を保持する。既存の q9o3 境界である
ineligible outline stream の container 後置は変更しない。

`tests/fixtures/json-diff/direct-outlines.pdf` の Generate linearizationについて、
qpdf 11.9.0 との strict byte parity と default-feature の root/container order guardを
追加した。qpdf-zlib-compat の linearized ObjStm 153 tests、linearization 63 tests、
Generate structural 35 testsが成功し、qpdf-deviation markerは追加していない。

## 2026-09-15: linearized explicit content normalization state (`flpdf-0s1ey`)

qpdf 11.9.0 は `--linearize --normalize-content=y` でも、
`initializeSpecialStreams` が記録した page content stream に対して
`willFilterStream` の normalize 分岐を選び、通常の `compress_streams` 分岐へ進まない
（`QPDFWriter.cc:1279-1284,1912-1936`）。linearized writer はこの state を保持したまま
`QPDF::optimize` の per-stream callback と emissionへ渡す
（`QPDFWriter.cc:2537-2553`; `QPDF_optimization.cc:261-333`）。

flpdf のCLIは警告順序と正規化済みbytesを安定させるためcreate stageでpage contentを
正規化してからwrite stageへ渡す。従来はlinearize時に
`WriterOptions::content_normalization`をfalseへ戻していたため、
`linearization_content_normalize_refs`とlinearized emissionが
`content_normalization_applied` markerを参照できず、正規化済みcontentをFlate再圧縮していた。
`.0s1ey` はwriter-side optionを有効なまま保持し、既に正規化済みのhandleではmarkerが
二重tokenizeを防ぎつつqpdfの非圧縮policyを選ぶようにした。direct page leafを含む
canonical page repairは既存の`PageDocumentHelper` setup snapshotを使う。

`crates/flpdf-cli/tests/cli_byte_identical.rs::cli_linearize_normalize_content_is_byte_identical_to_qpdf`
で、`one-page`、`multi-contents-one-page`、`shared-stream-objstm`、`direct-leaf-kid`を
qpdf 11.9.0とqpdf-zlib-compatでfull-byte比較する。既存のCR改行正規化、warning、
`qpdf --check-linearization`回帰も維持し、qpdf-deviation markerやlegacy bridgeは追加しない。

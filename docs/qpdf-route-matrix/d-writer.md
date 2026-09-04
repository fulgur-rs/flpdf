# D. writer — reachability, ObjStm planning / renumber / emission, xref / trailer, encryption, linearize

対象: `QPDFWriter` の `enqueueObject` / `enqueueObjectsStandard`（採番の正本、container-first）、
`preserveObjectStreams` / `generateObjectStreams`、`writeObject` / `writeObjectStream`、
`writeXRefTable` / `writeXRefStream` / `writeTrailer`、`writeEncryptionDictionary` と
`setEncryptionParameters*`、`writeLinearized`（pass1 → hint → pass2）、`QPDF_optimization` /
`QPDF_linearization` の object universe。flpdf 側は `writer.rs`（`emit_canonical_pdf_inner` の
shared plain pipeline と legacy coordinator の分岐）、`writer/rewrite_renumber.rs`、
`writer/object_streams/*`、`writer/plain/*`、`writer/reachability.rs`、`writer/encryption_state.rs`、
`writer/encrypted_strings.rs`、`writer/pclm.rs`、`linearization/{writer,renumber,plan}.rs`、
`optimization.rs`。

**PR #1486（`flpdf-hi08`、`feature/flpdf-hi08-encrypted-preserve-objstm`）は 2026-09-04 に
merge 済み（`35233ba3`）。** 本表の作成時は in-flight だったため main（`8fd1a2bf`）+ #1486 として
記述し、#1486 由来の行・注記に `(#1486)` を残している。`crates/flpdf/src/writer.rs` の行番号引用は
作成時の main（`8fd1a2bf`、#1486 未適用）を基準にしており、#1486 の merge で `writer.rs:3888` 以降は
最大で数十行ずれる。引用の再アンカーは本表の保守項目（README §8 参照）。

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
- linearized: `QPDF::optimize`（`libqpdf/QPDF_optimization.cc:57-118`）が `/Outlines` の indirect 化、
  `pushInheritedAttributesToPage`、各 page / trailer key（`/Root` 以外）/ root key ごとに
  `updateObjectMaps` で `obj_user_to_objects` / `object_to_obj_users` を作り、
  `filterCompressedObjects`（`libqpdf/QPDF_optimization.cc:340-381`）で compressed member の user を
  container に付け替える。universe = `object_to_obj_users` のキー集合で、
  `calculateLinearizationData`（`libqpdf/QPDF_linearization.cc:963-1403`）末尾で
  `num_placed == num_wanted` を検査する。

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
| D1 | `QPDFWriter::write` / `QPDFWriter::doWriteSetup`（mode 正規化 → 単一 dispatch） | `libqpdf/QPDFWriter.cc:2187-2213,2059-2184` | `crates/flpdf/src/writer.rs::PdfWriter::write`（`pub`、`crates/flpdf/src/writer.rs:719`） | prod: 5 (`crates/flpdf/src/job/check.rs:350`, `crates/flpdf/src/job/lifecycle.rs:2506`, `crates/flpdf/src/job/page_split.rs:282`, `crates/flpdf-cli/src/main.rs:330,343`) / test: 25 files（`crates/flpdf-qtest-tools/` の driver 43 箇所と `crates/flpdf/examples/` 7 箇所を除く） | mixed | `crates/flpdf/src/writer.rs::PdfWriter::write` | qpdf は `write()` 内で 2 分岐（`writeLinearized` / `writeStandard`）、pclm は `writeStandard` 内の 1 段深い分岐。flpdf は **4 つの独立実装**へ分岐する: `settings.linearization` で `write_linearized_for_pdf_writer`（`crates/flpdf/src/writer.rs:773`）、以降 `emit_canonical_pdf` 内で `write_pclm` / `plain::write_plain` / legacy coordinator。dispatch 条件表は「WriterOptions と route の対応」節 |
| D2 | `QPDFWriter::enqueueObject`（非 linearized の唯一の採番点、container-first・member 範囲即時予約） | `libqpdf/QPDFWriter.cc:1072-1141,1057-1069` | `crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber`（`pub(crate)`、`crates/flpdf/src/writer/rewrite_renumber.rs:626`） | prod: 4 (`crates/flpdf/src/writer/plain/plan.rs:687,688,727`, `crates/flpdf/src/writer.rs:4014` legacy coordinator) / test: 0 | mixed | `crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber` | qpdf の 1 採番機構に対し flpdf は 3 種: `ObjectStreamRenumber`（container-first を再現）、`CanonicalCatalogFirstRenumber`（D3）、`linearization/renumber.rs::RenumberMap`（D4）。しかも分裂は plain pipeline の**内側**にもある（D3 参照）。`ObjectStreamRenumber` が canonical なのは container-first を再現する唯一の実装だから |
| D3 | 同上（`enqueueObject` の container なし側 = 純粋な到達順採番） | `libqpdf/QPDFWriter.cc:1072-1141` | `crates/flpdf/src/writer/rewrite_renumber.rs::CanonicalCatalogFirstRenumber`（`pub(crate)`、`crates/flpdf/src/writer/rewrite_renumber.rs:89`） | prod: 5 (`crates/flpdf/src/writer/plain/plan.rs:172,185,709` [`:172` Disable / `:185` Preserve-no-source-ObjStm / `:709` reachability oracle]、`crates/flpdf/src/writer.rs:3788` legacy coordinator、`crates/flpdf/src/linearization/plan.rs:991`) / test: 0（`crates/flpdf/tests/rewrite_renumber_module_route_tests.rs:107,130` はコメントと文字列リテラル内の言及で、規則 (a)(e) により caller ではない） | mixed | `crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber` | container が 1 つも無い場合に限り D2 と一致するため、Disable / Preserve-without-source-ObjStm では byte 差が出ていない。container がある経路（Preserve-with-source / Generate / legacy coordinator の非 #1486 スライス）で container-above-max になるのが `flpdf-hi08` が捕らえた乖離。残 caller は上記 5 箇所すべて |
| D4 | `QPDFWriter::writeLinearized` の事前採番（second half → first half、part 単位） | `libqpdf/QPDFWriter.cc:2575-2646` | `crates/flpdf/src/linearization/renumber.rs::RenumberMap`（`pub`、`crates/flpdf/src/linearization/renumber.rs:84`） | prod: 19 (`crates/flpdf/src/linearization/writer.rs`=12, `crates/flpdf/src/linearization/renumber.rs`=2, `crates/flpdf/src/linearization/part1.rs`=2, `crates/flpdf/src/linearization/{hint_page,hint_shared,plan}.rs`=各1) / test: 107 (8 files) | canonical | `crates/flpdf/src/linearization/renumber.rs::RenumberMap` | qpdf も linearized では `enqueueObject` の採番を使わないので、専用機構が 1 本あること自体は逸脱でない。`RenumberMap::from_plan`（`crates/flpdf/src/linearization/renumber.rs:191`）が part 順の slot 割当を持つ |
| D5 | `QPDFWriter::assignCompressedObjectNumbers`（container enqueue 時に member 範囲を即時予約） | `libqpdf/QPDFWriter.cc:1057-1069` | `crates/flpdf/src/writer/object_streams/planning.rs::ObjectStreamGroup`（`pub(crate)`、`crates/flpdf/src/writer/object_streams/planning.rs:73`）を `ObjectStreamRenumber::build_with_stream_policy` に渡す | prod: 19 (`crates/flpdf/src/writer/plain/plan.rs`=10, `crates/flpdf/src/writer/rewrite_renumber.rs`=6, `crates/flpdf/src/writer/object_streams/planning.rs:100,244`, `crates/flpdf/src/writer.rs:4005`) / test: 0 | mixed | `crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber` | `ObjectStreamGroup` は `Synthetic { members }`（Generate）と `SourceBacked { source, members }`（Preserve）の 2 変種。legacy coordinator は main では常に `Synthetic` を作り Preserve の source 同一性を採番へ渡していなかった。**(#1486)** が `qpdf_preserve_source_objstm` 成立時に `SourceBacked` を構築して `ObjectStreamRenumber` へ渡すよう変更（main では `crates/flpdf/src/writer.rs:3999-4023` が常に `Synthetic` を作る） |
| D6 | `QPDFWriter::preserveObjectStreams`（source container map ∩ compressible set） | `libqpdf/QPDFWriter.cc:1939-1967` | `crates/flpdf/src/writer/object_streams/planning.rs::plan_qpdf_preserve_object_streams_with_unreferenced`（`pub(crate)`、`crates/flpdf/src/writer/object_streams/planning.rs:199`） | prod: 2 (`crates/flpdf/src/writer/object_streams/mod.rs:19` re-export, `crates/flpdf/src/writer/plain/plan.rs:197`) / test: 0 | mixed | `crates/flpdf/src/writer/object_streams/planning.rs::plan_qpdf_preserve_object_streams_with_unreferenced` | plain pipeline は必ずここを通るが、legacy coordinator は別経路（`crates/flpdf/src/writer.rs:3863` の `plan_object_streams_with_reachability`）、linearized は `crates/flpdf/src/linearization/plan.rs::objstm_membership_linearized_with_eligibility` を使う。Preserve の batch 導出が 3 実装に分かれている |
| D7 | `QPDFWriter::generateObjectStreams`（`ceil(n/100)` 分割、null container 生成） | `libqpdf/QPDFWriter.cc:1970-2006` | `crates/flpdf/src/writer/object_streams/eligibility.rs::even_split_into_streams`（`pub(crate)`、`crates/flpdf/src/writer/object_streams/eligibility.rs:167`） | prod: 4 (`crates/flpdf/src/writer/object_streams/mod.rs:9` re-export, `crates/flpdf/src/writer/plain/plan.rs:237`, `crates/flpdf/src/linearization/writer.rs:3149`, `crates/flpdf/src/linearization/plan.rs:2724`) / test: 0 | canonical | `crates/flpdf/src/writer/object_streams/eligibility.rs::even_split_into_streams` | 分割アルゴリズム自体は 1 本に集約済み。legacy coordinator は `plan_object_streams_with_reachability`（`crates/flpdf/src/writer/object_streams/planning.rs:138`）経由で同じ分割へ到達する |
| D8 | `QPDF::getCompressibleObjGens`（trailer 起点の LIFO DFS、stream/Sig/Encrypt 除外） | `libqpdf/QPDF.cc:2393-2474` | `crates/flpdf/src/writer/object_streams/eligibility.rs::get_compressible_objgens`（`pub(crate)`、`crates/flpdf/src/writer/object_streams/eligibility.rs:77`） | prod: 3 (`crates/flpdf/src/writer/object_streams/mod.rs:9` re-export, `crates/flpdf/src/linearization/plan.rs:976,2720`) / test: 0 | mixed | `crates/flpdf/src/writer/object_streams/eligibility.rs::compressible_objgens_qpdf_plan` | 同じ qpdf 関数に対し flpdf 側の入口が 2 つある: `get_compressible_objgens`（`Vec<ObjectRef>` だけ返す薄い層、`crates/flpdf/src/writer/object_streams/eligibility.rs:80` で後者へ委譲）と `compressible_objgens_qpdf_plan`（`crates/flpdf/src/writer/object_streams/eligibility.rs:93`、`CompressiblePlan` = eligible + `removed_refs`）。後者は qpdf の stale-generation `removeObject` 副作用まで返すので責務が広く、linearized 経路だけが薄い側を使う。`compressible_objgens_qpdf_plan` prod: 9 (`crates/flpdf/src/writer/object_streams/mod.rs:8` re-export, `crates/flpdf/src/writer/object_streams/planning.rs:9,208,334`, `crates/flpdf/src/writer/object_streams/eligibility.rs:80`, `crates/flpdf/src/writer/plain/plan.rs:230`, `crates/flpdf/src/writer.rs:3855`, `crates/flpdf/src/linearization/writer.rs:3148`, `crates/flpdf/src/linearization/plan.rs:1129`) / test: 0 |
| D9 | `QPDF::getObjectStreamData`（入力 xref の type 2 entry → source container） | `libqpdf/QPDF.cc:2381-2390` | `crates/flpdf/src/reader/resolver.rs::source_xref_entries`（`pub(crate)`、`crates/flpdf/src/reader/resolver.rs:2117`） | prod: 10（writer / linearization 系のみ。reader 内部の 5 箇所は除く）(`crates/flpdf/src/writer.rs:4123`, `crates/flpdf/src/writer/object_streams/planning.rs:216,276`, `crates/flpdf/src/writer/plain/plan.rs:808`, `crates/flpdf/src/writer/rewrite_renumber.rs:73`, `crates/flpdf/src/linearization/plan.rs:1120,2484`, `crates/flpdf/src/linearization/writer.rs:3573,3850,4515`) / test: 1 (`crates/flpdf/src/reader/resolver.rs:9360`) | mixed | `crates/flpdf/src/reader/resolver.rs::source_xref_entries` | qpdf は「type 2 → (obj, container)」の map を `getObjectStreamData` が 1 度作り、`preserveObjectStreams` だけが読む。flpdf は生の `BTreeMap<ObjectRef, XrefEntry>` を返す reader API になっており、`XrefEntry::Compressed` の filter を **呼び出し側ごとに 10 回** 書き直している（legacy coordinator は `crates/flpdf/src/writer.rs::source_objstm_container_for_batch`（`crates/flpdf/src/writer.rs:3080`）、linearization writer は `source_container_by_member` を `crates/flpdf/src/linearization/writer.rs:3572,3849,4514` で 3 度独立に構築）。同名の宣言が `crates/flpdf/src/reader.rs:761` にもある（`resolver` へ委譲する薄いラッパー）ので `rg` 時は注意 |
| D10 | `QPDFWriter::writeObjectStream`（2 パス・objgen 昇順 member・`/Extends` 複写） | `libqpdf/QPDFWriter.cc:1621-1758,1606-1618` | `crates/flpdf/src/writer/object_streams/emission.rs::emit_objstm_body_from_handles_with_writer`（`pub(crate)`、`crates/flpdf/src/writer/object_streams/emission.rs:45`） | prod: 3 (`crates/flpdf/src/writer/plain/body.rs:63`, `crates/flpdf/src/writer.rs:4860`, `crates/flpdf/src/linearization/writer.rs:329`) + re-export 1 (`crates/flpdf/src/writer/object_streams/mod.rs:13`) / test: 0 | canonical | `crates/flpdf/src/writer/object_streams/emission.rs::emit_objstm_body_from_handles_with_writer` | ObjStm body の 2 パス生成は 3 経路すべてがこの 1 関数を通る。QDF 変種は `…_qdf`（`crates/flpdf/src/writer/object_streams/emission.rs:58`）。ただし container dict の組み立て（`/Type /ObjStm /Length /Filter /N /First`）と `/Extends` の付与は呼び出し側に残っており、そこは D11 の smear に含まれる |
| D11 | `QPDFWriter::writeObject` / `unparseObject`（body 1 オブジェクトの emission） | `libqpdf/QPDFWriter.cc:1761-1809,1318-1603` | `crates/flpdf/src/writer/plain/body.rs::emit_bodies`（`pub(crate)`、`crates/flpdf/src/writer/plain/body.rs:18`） | prod: 1 (`crates/flpdf/src/writer/plain/mod.rs:35`) / test: 0 | mixed | `crates/flpdf/src/writer/plain/body.rs::emit_bodies` | qpdf の「queue を回して `writeObject`」1 ループに対し flpdf は 4 実装: `emit_bodies`（plain）、legacy coordinator の inline 出力ループ（`crates/flpdf/src/writer.rs:4469` の `for (new_ref, old_ref) in &renumbered`）、`write_pclm` の inline ループ（`crates/flpdf/src/writer.rs:3381` の `for item in &plan.items`）、`crates/flpdf/src/linearization/writer.rs` の `do_write_pass`（`crates/flpdf/src/linearization/writer.rs:2065`）。共通 primitive は `crates/flpdf/src/writer/object.rs` / `crates/flpdf/src/writer/serialize.rs` 側にあるがループ自体は共有していない |
| D12 | `QPDFWriter::writeXRefTable`（classic xref table） | `libqpdf/QPDFWriter.cc:2343-2379,2335-2340` | `crates/flpdf/src/writer/plain/xref.rs::append_xref_and_trailer`（`pub(crate)`、`crates/flpdf/src/writer/plain/xref.rs:75`）の `XrefForm::Table` アーム | prod: 2 (`crates/flpdf/src/writer/plain/mod.rs:36`, `crates/flpdf/src/writer.rs:5433`) / test: 2 (writer/plain/plan.rs 内 test) | mixed | `crates/flpdf/src/writer/plain/xref.rs::append_xref_and_trailer` | `xref\n0 N\n` を書く実装が **4 箇所**: `crates/flpdf/src/writer/plain/xref.rs:254`（canonical）、`crates/flpdf/src/writer.rs:5191`（legacy coordinator の `XrefForm::Table` アーム — plain 版へは委譲していない）、`crates/flpdf/src/writer.rs:3451`（`write_pclm`）、`crates/flpdf/src/linearization/writer.rs:808,1008`（first-page / main）。qpdf は 1 実装。`docs/qpdf-correspondence.md:389` の §3 は「xref 出力が 3 箇所」と記載しているが、`write_pclm` を数えると 4。**欠番 entry の扱いは qpdf に対応物が無い**: qpdf の classic table は object 0 を `0000000000 65535 f ` にし（`libqpdf/QPDFWriter.cc:2363`）、他の i は必ず `m->xref[i].getOffset()` を通る（`libqpdf/QPDFWriter.cc:2364-2373`）。`m->xref` は `std::map<int, QPDFXRefEntry>`（`include/qpdf/QPDFWriter.hh:669`）なので欠番 i は type 0 の既定エントリを default-insert し（`include/qpdf/QPDFXRefEntry.hh:66-68` の `type{0}`）、`getOffset()` は type≠1 で `std::logic_error` を投げる（`libqpdf/QPDFXRefEntry.cc:27-32`）。つまり **qpdf は欠番行を書かず、書こうとすれば throw する** — `openObject` が `m->xref[objid]` を埋め、`writeObjectStream` が member に type 2 を入れるという「欠番なし不変条件」に依存している。`0000000000 00000 n ` が出るのは `suppress_offsets == true` のときだけで（宣言 `libqpdf/QPDFWriter.cc:2349` / 使用 `libqpdf/QPDFWriter.cc:2366`）、これを渡すのは `writeLinearized` の pass 1 のみ（first-half table の padding 用であって欠番符号化ではない）。type 0 を明示的に書くのは `writeXRefStream` の `case 0:`（`libqpdf/QPDFWriter.cc:2437-2444`、binary zeros）だけ。flpdf は 4 実装すべてが欠番行を明示出力し、しかも符号化が 2 通りに割れている: `crates/flpdf/src/writer/plain/xref.rs:260` が `0000000000 00000 f `、`crates/flpdf/src/writer.rs:5197`・`crates/flpdf/src/writer.rs:3469`・`crates/flpdf/src/linearization/writer.rs:1018` が `0000000000 65535 f `。**これは「qpdf と一致していない」ではなく「揃える先の qpdf 挙動が存在しない」flpdf 固有挙動**であり、欠番が実際に生じうるかは U3 |
| D13 | `QPDFWriter::writeXRefStream`（type 0/1/2 バイナリ + PNG predictor） | `libqpdf/QPDFWriter.cc:2392-2495,2382-2389` | `crates/flpdf/src/writer/plain/xref.rs::append_xref_and_trailer` の `XrefForm::Stream` アーム | prod: 2 / test: 2（D12 と同一関数） | mixed | `crates/flpdf/src/writer/plain/xref.rs::append_xref_and_trailer` | xref **stream** については legacy coordinator が `crates/flpdf/src/writer.rs:5433` で plain 版へ委譲しており（`crates/flpdf/src/writer.rs:5336-5338` の「delegated to the canonical plain-writer xref layer」）、実装は 2 本（plain 共有 + linearization の `write_main_xref_stream_and_trailer` / `patch_first_page_xref`）。table 側（D12）だけが 4 本に割れている非対称が smear の核心 |
| D14 | `QPDFWriter::writeTrailer` / `getTrimmedTrailer` | `libqpdf/QPDFWriter.cc:1160-1236,2009-2032` | `crates/flpdf/src/writer/plain/plan.rs::canonical_trailer_entries_with_visibility`（`pub(crate)`、`crates/flpdf/src/writer/plain/plan.rs:588`）と `crates/flpdf/src/writer.rs::build_writer_trailer_handle`（private、`crates/flpdf/src/writer.rs:3015`） | `canonical_trailer_entries_with_visibility` prod: 2 (`crates/flpdf/src/writer/plain/plan.rs:581`, `crates/flpdf/src/writer.rs:5419`) / test: 2。`build_writer_trailer_handle` prod: 5 (`crates/flpdf/src/writer/plain/plan.rs:334`, `crates/flpdf/src/writer.rs:3485,3531,5220,5363`) / test: 0 | mixed | `crates/flpdf/src/writer.rs::build_writer_trailer_handle`（`getTrimmedTrailer` 相当）| **`getTrimmedTrailer` 側は既に 1 本に集約されている**: `build_writer_trailer_handle` を plain（`crates/flpdf/src/writer/plain/plan.rs:334`）・pclm（`crates/flpdf/src/writer.rs:3485,3531`）・legacy classic（`crates/flpdf/src/writer.rs:5220`）・legacy xref stream（`crates/flpdf/src/writer.rs:5363`）の 4 経路すべてが通る。割れているのは **`writeTrailer` の書き出し側** で、`crates/flpdf/src/writer/object.rs::write_trailer_with_ref_map`（`crates/flpdf/src/writer/object.rs:151` trait / `:952` impl。pclm `crates/flpdf/src/writer.rs:3511,3521,3555,3565` と legacy classic `:5272,5283,5309,5321` が使う）、`crates/flpdf/src/writer/plain/xref.rs` の `write_canonical_classic_trailer`（`crates/flpdf/src/writer/plain/xref.rs:270`）+ xref-stream 用 trailer、`crates/flpdf/src/linearization/writer.rs:1023` の raw byte 組み立て（key 順を `/Size /ID` に固定するため `write_pdf` を使わないと明記）の 3 系統。`canonical_trailer_entries_with_visibility` は「どの entry が null 可視性を通るか」を決める補助で、xref-stream trailer だけが使う。null 値 dict キー抑制（`libqpdf/QPDFWriter.cc:1490-1491`）は `suppress_null_values` で共通化済み |
| D15 | `QPDFWriter::writeEncryptionDictionary`（body 後・xref 前、`std::map` の key 昇順） | `libqpdf/QPDFWriter.cc:2244-2256`、`writeStandard` からの呼び出し位置は `libqpdf/QPDFWriter.cc:3017-3019`（他の呼び出し元は `writeLinearized` の `libqpdf/QPDFWriter.cc:2795`） | `crates/flpdf/src/writer/encrypted_strings.rs::write_encryption_dictionary_handle`（`pub(crate)`、`crates/flpdf/src/writer/encrypted_strings.rs:313`） | prod: 2 (`crates/flpdf/src/writer.rs:5163` legacy coordinator, `crates/flpdf/src/linearization/writer.rs:2360`) / test: 0 | canonical | `crates/flpdf/src/writer/encrypted_strings.rs::write_encryption_dictionary_handle` | 出力位置も qpdf と一致: legacy coordinator は body 全 object の後・`let xref_offset = bytes.len();` の直前（`crates/flpdf/src/writer.rs:5155-5170`）、linearized は part4 の直後。**plain pipeline は暗号化経路を一切持たない**（`plain::eligible` が `encrypt.is_none() && copy_encryption.is_none() && !pdf_is_encrypted` を要求する、`crates/flpdf/src/writer/plain/mod.rs:50-62`）ので、暗号化された非 linearized 出力は必ず legacy coordinator を通る |
| D16 | `QPDFWriter::setEncryptionParametersInternal` / `setEncryptionParameters` / `copyEncryptionParameters` | `libqpdf/QPDFWriter.cc:777-840,591-648,651-702` | `crates/flpdf/src/writer.rs::EncryptionContext`（`pub(crate)`、`crates/flpdf/src/writer.rs:2311`） | prod: 22 (`crates/flpdf/src/writer.rs`=13, `crates/flpdf/src/linearization/writer.rs`=6, `crates/flpdf/src/writer/encrypted_strings.rs`=3) / test: 0 | mixed | `crates/flpdf/src/writer.rs::EncryptionContext` | qpdf は 3 つの setter が `setEncryptionParametersInternal` 1 本へ収束し、以後は `m->encryption_dictionary` という単一 state。flpdf は `EncryptionContext` を legacy coordinator と linearization writer がそれぞれ組み立てる。`/Encrypt` の最小版数決定（R6→1.7 ext8 等）は `crates/flpdf/src/writer.rs::encryption_version_floor`（`crates/flpdf/src/writer.rs:1766`）に分離されている |
| D17 | `QPDFWriter::setDataKey` / `pushEncryptionFilter`（object ごとの data key） | `libqpdf/QPDFWriter.cc:843-847,976-1000` | `crates/flpdf/src/writer/encryption_state.rs::WriterEncryptionState`（`pub(crate)`、`crates/flpdf/src/writer/encryption_state.rs:40`） | prod: 5 (`crates/flpdf/src/writer/encrypted_strings.rs:32,49,239`, `crates/flpdf/src/writer/encryption_state.rs:48`, `crates/flpdf/src/writer.rs:3191`) / test: 9 (`crates/flpdf/src/writer/encryption_state.rs`) | canonical | `crates/flpdf/src/writer/encryption_state.rs::WriterEncryptionState` | `set_data_key` / `with_object_data_key` が qpdf の set/unparse/clear 順序を写す。`docs/qpdf-correspondence.md:389` §3 に既存記載あり |
| D18 | `QPDFWriter::writeLinearized`（production 経路） | `libqpdf/QPDFWriter.cc:2537-2904` | `crates/flpdf/src/linearization/writer.rs::write_linearized_for_pdf_writer`（`pub(crate)`、`crates/flpdf/src/linearization/writer.rs:3109`） | prod: 1 (`crates/flpdf/src/writer.rs:773`) / test: 0 | canonical | `crates/flpdf/src/linearization/writer.rs::write_linearized_for_pdf_writer` | production の linearized 出力はこの 1 本のみ。`PdfWriter::write` は `emit_canonical_pdf` より前に分岐するので、linearized は plain / legacy / pclm のどれとも合流しない |
| D19 | 同上（plan/renumber を外から与える test 用入口） | `libqpdf/QPDFWriter.cc:2537-2904` | `crates/flpdf/src/linearization/writer.rs::write_linearized`（`pub(crate)` + 項目単位 `#[cfg(test)]`、`crates/flpdf/src/linearization/writer.rs:3093`） | prod: 0 / test: 2 (`crates/flpdf/src/linearization/show.rs:1901`, `crates/flpdf/src/linearization/back_patch.rs:382`) | bridge | `crates/flpdf/src/linearization/writer.rs::write_linearized_for_pdf_writer` | qpdf に「plan を外部から渡す」対応物は無い。`#[cfg(test)]` により production からは到達不能で、`write_linearized_impl` を共有するだけの薄い入口。残る `rg` hit は `use` 行 2 件（`crates/flpdf/src/linearization/show.rs:1863`, `crates/flpdf/src/linearization/back_patch.rs:332`）、`.expect("write_linearized")` の文字列リテラル 1 件（`crates/flpdf/src/linearization/back_patch.rs:383`）、コメント 16 件で、いずれも caller ではない。prod caller ゼロなので削除候補（fixture 側を `write_linearized_for_pdf_writer` へ寄せられるかは別途判断） |
| D20 | `writeLinearized` の 2 パス構造（pass1 → hint 1 回 → pass2、反復なし） | `libqpdf/QPDFWriter.cc:2656-2904`、特に `libqpdf/QPDFWriter.cc:2864-2884` | `crates/flpdf/src/linearization/writer.rs::write_linearized_for_pdf_writer` 内の `do_write_pass` 2 回（`crates/flpdf/src/linearization/writer.rs:3986` pass1 / `crates/flpdf/src/linearization/writer.rs:4380` 付近 final） | prod: 1 / test: 0（D18 と同一関数） | canonical | `crates/flpdf/src/linearization/writer.rs::write_linearized_for_pdf_writer` | **`flpdf-26l3`（収束ループ廃止）は実装上も解消済み**を確認: `write_linearized_impl`（`crates/flpdf/src/linearization/writer.rs:3254`）内に layout pass をまたぐ `for` / `while` / `loop` は無く、`do_write_pass` の逐次 2 回呼び出しと、その間の 1 回きりの hint table 構築（`crates/flpdf/src/linearization/writer.rs:4333` の `encode_hint_stream`）だけ。ただし `crates/flpdf/src/linearization/hint_shared.rs:1081,1089,1103` と `crates/flpdf/src/linearization/part1.rs:25` に「convergence loop」を前提とした **stale なコメントが 4 箇所残っている**（挙動ではなく記述の負債） |
| D21 | `QPDF::optimize` / `filterCompressedObjects`（linearized の object-user map） | `libqpdf/QPDF_optimization.cc:57-118,340-381` | `crates/flpdf/src/optimization.rs::Optimization`（`pub(crate)`、`crates/flpdf/src/optimization.rs:21`） | prod: 10 (`crates/flpdf/src/linearization/plan.rs:879,1001,2395,2480,2836`, `crates/flpdf/src/linearization/check.rs:335,410,465`, `crates/flpdf/src/linearization/writer.rs:3370`, `crates/flpdf/src/optimization.rs:32`) / test: 1 | canonical | `crates/flpdf/src/optimization.rs::Optimization` | `docs/qpdf-correspondence.md` §4 が ✅ 済みと記載し、実装も 1 モジュールに集約されている。`prepare_for_linearized_write`（`crates/flpdf/src/optimization.rs:152`）は `optimize` と `prepare_pdf` を共有する部分適用で、prod caller は `crates/flpdf/src/linearization/writer.rs:3370` の 1 箇所。D27 follow-up の stream-parameter probe も `Optimization::update_object_maps` の callback 内でだけ実行し、qpdf の page/trailer/root 起点の到達範囲を越えて `pdf.object_refs()` を解決しない |
| D22 | `QPDF::getLinearizedParts` / `calculateLinearizationData`（part4/6/7/8/9） | `libqpdf/QPDF_linearization.cc:1435-1449,963-1403,1174-1336` | `crates/flpdf/src/linearization/plan.rs::LinearizationPlan`（`pub`、`crates/flpdf/src/linearization/plan.rs:744`） | prod: 21 (`crates/flpdf/src/linearization/writer.rs`=9, `crates/flpdf/src/linearization/hint_page.rs`=4, `crates/flpdf/src/linearization/{plan,renumber}.rs`=各3, `crates/flpdf/src/linearization/{hint_shared,part1}.rs`=各1) / test: 57 (10 files) | canonical | `crates/flpdf/src/linearization/plan.rs::LinearizationPlan` | `from_pdf_with_writer_options`（`crates/flpdf/src/linearization/plan.rs:959`）が production 入口。part 分類は qpdf の `lc_*` 集合を写している |
| D23 | `doWriteSetup` の ObjStm 除外（linearized なら page + root、encrypted なら root） | `libqpdf/QPDFWriter.cc:2140-2158` | `crates/flpdf/src/writer/object_streams/planning.rs::filter_objstm_batches_for_output`（`pub(crate)`、`crates/flpdf/src/writer/object_streams/planning.rs:165`） | prod: 1 (`crates/flpdf/src/writer.rs:3889` legacy coordinator) + re-export 1 / test: 0 | mixed | `crates/flpdf/src/writer/object_streams/planning.rs::filter_objstm_batches_for_output` | 同じ除外規則が `crates/flpdf/src/linearization/plan.rs::objstm_membership_linearized_with_eligibility`（`crates/flpdf/src/linearization/plan.rs:2709`）の `:2727-2732` に **独立実装** されている（page refs + root の erase セット）。plain pipeline は呼ばないが、`plain::eligible` が encrypted を除外し linearized は別経路なので `output_linearized \|\| output_encrypted` が常に false になり、現状は差が出ない。除外規則を変えるときは 2 箇所を同時に直す必要がある |
| D24 | `QPDFWriter::enqueueObjectsPCLm` + `writeStandard`（pclm は xref/trailer を standard と共有） | `libqpdf/QPDFWriter.cc:2928-2954,2991-3044` | `crates/flpdf/src/writer/pclm.rs::Plan`（`pub(crate)`、`crates/flpdf/src/writer/pclm.rs:28`）と `crates/flpdf/src/writer.rs::write_pclm`（private、`crates/flpdf/src/writer.rs:3354`） | `write_pclm` prod: 1 (`crates/flpdf/src/writer.rs:3672`) / test: 0 | mixed | `crates/flpdf/src/writer/plain/xref.rs::append_xref_and_trailer`（xref/trailer 部分） | qpdf の pclm は `enqueueObjectsPCLm` だけが差分で、body ループ・xref・trailer は `writeStandard` と完全共有。flpdf の `write_pclm` は body ループ・classic xref（`crates/flpdf/src/writer.rs:3451`）・trailer 書き出しをすべて自前で持つ独立実装で、qpdf の共有構造を再現していない |
| D25 | `QPDFWriter::prepareFileForWrite`（`/Extensions` / `/ADBE` の direct 化 + `fixDanglingReferences`） | `libqpdf/QPDFWriter.cc:2036-2056`、`libqpdf/QPDF.cc:1259-1269` | `crates/flpdf/src/writer.rs::inject_adbe_extension`（`pub(crate)`、`crates/flpdf/src/writer.rs:1905`）/ `crates/flpdf/src/writer.rs::strip_adbe_extension`（`crates/flpdf/src/writer.rs:1987`） | prod: 2 (`crates/flpdf/src/writer.rs:3657` `emit_canonical_pdf_inner`, `crates/flpdf/src/linearization/writer.rs:3800`) / test: 0 | mixed | `crates/flpdf/src/writer.rs::inject_adbe_extension` | qpdf は `write()` が `writeLinearized` / `writeStandard` の **前に 1 度だけ** `prepareFileForWrite` を呼ぶ。flpdf は同等処理を `emit_canonical_pdf_inner` の先頭と `write_linearized_for_pdf_writer` の中で **別々に** 行い、さらに output-only の復元を `snapshot_catalog_extensions` / `restore_catalog_extensions`（`crates/flpdf/src/writer.rs:2109,2132`）と `emit_canonical_pdf`（`crates/flpdf/src/writer.rs:3312`）の 2 重 snapshot で扱う。qpdf に対応物のない復元機構 |
| D26 | `QPDFWriter::initializeSpecialStreams`（page seq / contents seq / normalized streams） | `libqpdf/QPDFWriter.cc:1912-1936`、トリガは `libqpdf/QPDFWriter.cc:2113-2115` | `crates/flpdf/src/writer.rs::PdfWriter::write` 内の `PageDocumentHelper::get_all_pages()` 呼び出し（`crates/flpdf/src/writer.rs:759-762`） | prod: 2 (`crates/flpdf/src/writer.rs:761` `PdfWriter::write`, `crates/flpdf/src/writer.rs:3727` legacy coordinator の `qdf_page_refs`) / test: 0 | mixed | `crates/flpdf/src/writer.rs::PdfWriter::write` | トリガ条件（`qdf \|\| content_normalization \|\| decode_level != None`）は qpdf と一致。ただし qpdf の 1 関数が作る 3 つの map のうち flpdf が持つのは page-tree 修復と `normalized_stream_refs`（`crates/flpdf/src/writer.rs:3731-3742`、legacy coordinator のみ）だけで、`page_object_to_seq` / `contents_to_page_seq`（QDF コメント用）は emission 側に散っている。同じ page 修復が `PdfWriter::write` と legacy coordinator で二重に走る |
| D27 | `enqueueObject` による到達性（qpdf に独立した削除パスは無い） | `libqpdf/QPDFWriter.cc:1072-1141,2907-2925` | 旧 `sweep_unreachable_objects` route は削除済み。残る `crates/flpdf/src/writer/reachability.rs::sweep_unreachable_objects_except` は multi-source merge 専用で、書き込み時の canonical owner は `crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber` | 旧 pre-write route: prod 0 / test 0。merge-only `_except`: prod 1 (`crates/flpdf/src/job/page_merge.rs:1224`) / test 3 (`writer/reachability.rs` 内) | mixed | `crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber`（書き込み経路の到達性） | qpdf writer の到達性判定は書き込み時に行われるため、page selection / attachment removal / extraction の pre-write sweep は撤去した。`sweep_unreachable_objects_except` は複数 source の保護参照を扱う別 leaf として残り、D3 / D11 の cutover 対象である。D27 follow-up（`flpdf-3yn9.44.1`）では linearized の stream-parameter probe を `Optimization` の page/trailer/root 起点 callback へ移し、unreachable stream の indirect `/Length` holder を解決しない回帰を追加した |
| D28 | `getCompressibleObjGens` の到達性 filter（`preserveObjectStreams` の第 2 引数相当） | `libqpdf/QPDFWriter.cc:1953-1958` | `crates/flpdf/src/writer/plain/plan.rs::retain_reachable_object_stream_members`（private、`crates/flpdf/src/writer/plain/plan.rs:698`） | prod: 2 (`crates/flpdf/src/writer/plain/plan.rs:210` Preserve, `crates/flpdf/src/writer/plain/plan.rs:251` Generate) / test: 0 | mixed | `crates/flpdf/src/writer/object_streams/eligibility.rs::compressible_objgens_qpdf_plan` | 1 つの flpdf 経路が 2 つの qpdf 責務を畳んでいる例（§3 の mixed 第 2 節）: 到達性判定のために `CanonicalCatalogFirstRenumber::build_qpdf_with_stream_policy` を **採番目的ではなく reachability oracle として** 呼び、その結果でグループ member を retain する。qpdf 側は `getCompressibleObjGens` の返り値集合で filter するだけで、採番機構は関与しない |
| D29 | `enqueueObject` の container-first を Preserve に適用（#1486 のスコープ） | `libqpdf/QPDFWriter.cc:1072-1118,1057-1069` | `crates/flpdf/src/writer.rs::emit_canonical_pdf_inner` の `qpdf_preserve_source_objstm` 分岐 **(#1486)**。**識別子 `qpdf_preserve_source_objstm` は #1486 diff にのみ存在し、main の worktree では `rg` が 0 hit になる** | prod: 1（`emit_canonical_pdf_inner` 内の分岐 1 箇所のみ）/ test: 1 (`crates/flpdf-qtest-tools/tests/qpdf_ctest_cli.rs` の `qpdf_ctest_preserves_qpdf_objstm_enqueue_order_for_encryption_and_decryption`) **(#1486)** | mixed | `crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber` | **(#1486)** ゲート条件は diff 上 `!options.qdf && options.object_streams == ObjectStreamMode::Preserve && !plan.batches.is_empty()` で、**`encrypting` の項は含まれない**。encrypted 限定になるのは条件ではなく到達性の帰結（`plain::eligible` が非 qdf Preserve を plain へ流すので、legacy coordinator へ落ちるのは encryption / copy-encryption / source-encrypted / `extra_header_text` / content-normalization のいずれかがある場合だけ）。#1486 後も legacy coordinator には Catalog-first 経路（D3）が Disable / 抑制された Preserve 用に残る |
| D30 | `write()` から出力バイトへの単一 pipeline（`QPDFWriter::write` の後段） | `libqpdf/QPDFWriter.cc:2196-2213` | `crates/flpdf/src/writer.rs::write_qpdf_to_memory`（`pub(crate)` + 項目単位 `#[cfg(test)]`、`crates/flpdf/src/writer.rs:31`） | prod: 0 / test: 14 (`crates/flpdf/src/job/{page_subset,rotate,acroform_field_prune}.rs`, `crates/flpdf/src/pages/tree_rebuild.rs`, `crates/flpdf/src/page_annotation_flatten.rs`, `crates/flpdf/src/page_splice.rs`) | bridge | `crates/flpdf/src/writer.rs::PdfWriter::write` | 宣言自身の doc が「Test-only convenience」と明記。**同名の別関数** `write_qpdf_to_memory` が `crates/flpdf-cli/src/main.rs:334` にあり（`(pdf, output, &options)` の異なるシグネチャ）、`main.rs:5806,6021` の 2 prod 呼び出しはそちらを指すので、この行の bridge とは無関係 |
| D31 | `QPDFWriter::preserveObjectStreams` の linearized 側適用（`doWriteSetup` で Preserve を決めた後、`writeLinearized` が `object_to_object_stream_no_gen` として使う） | `libqpdf/QPDFWriter.cc:1939-1967,2541,2575-2617` | `crates/flpdf/src/linearization/plan.rs::objstm_batches`（`pub(crate)`、`crates/flpdf/src/linearization/plan.rs:2332`）と `crates/flpdf/src/linearization/plan.rs::route_objstm_containers`（`crates/flpdf/src/linearization/plan.rs:2835`） | `objstm_batches` prod: 1 (`crates/flpdf/src/linearization/writer.rs:146`) / test: 0。`route_objstm_containers` prod: 2 (`crates/flpdf/src/linearization/plan.rs:2411,2529`) / test: 0（統合テスト側の hit はすべてコメント） | unknown | unknown | `objstm_membership_linearized_with_eligibility`（D23）は Generate の even-split しか扱わないため、linearized Preserve の container 由来・member 順序・stale-generation 除去がどこで決まるかを読み切っていない。D6 の canonical owner （`plan_qpdf_preserve_object_streams_with_unreferenced`）と同一結果になるかも未確認。必要な作業は U1 |

## WriterOptions と route の対応

dispatch は 2 段。まず `PdfWriter::write`（`crates/flpdf/src/writer.rs:719-798`）が
`settings.linearization`（= `WriterOptions` ではなく `WriterSettings`、
`crates/flpdf/src/writer/settings.rs:38`）を見て linearized を切り離し、非 linearized だけが
`emit_canonical_pdf` → `emit_canonical_pdf_inner`（`crates/flpdf/src/writer.rs:3576-5439`）へ入る。
`emit_canonical_pdf_inner` の順序は
(1) `deterministic_id && static_id` の排他チェック →
(2) **force<1.5 による ObjStm 抑制** →
(3) `encrypt && copy_encryption` の排他チェック →
(4) `/Extensions /ADBE` 注入・除去 →
(5) `if options.pclm → write_pclm` →
(6) `if plain::eligible(...) → plain::write_plain` →
(7) それ以外は legacy coordinator。

`plain::eligible`（`crates/flpdf/src/writer/plain/mod.rs:50-62`）は次を **すべて** 満たすことを要求する:

```
mode == options.object_streams      (mode = 抑制前の requested_object_streams)
&& !options.qdf
&& !options.pclm
&& options.extra_header_text.is_empty()
&& options.encrypt.is_none()
&& options.copy_encryption.is_none()
&& !options.content_normalization
&& !pdf_is_encrypted
```

第 1 項が非自明である。`mode` は **force<1.5 抑制の前** に取った `requested_object_streams` で、
`options.object_streams` は抑制後の値。したがって
**`force_pdf_version < 1.5` かつ Generate（または非暗号の Preserve）は、抑制で両者が食い違うため
常に legacy coordinator へ落ちる** — 他のオプションが何も「特殊」でなくても、である。

また `eligible` に **含まれない** ものも重要: `decode_level`、`deterministic_id` / `static_id`、
`preserve_unreferenced_objects`、`newline_before_endstream`、`compress_streams` は
plain pipeline へ入ることを妨げない。

| 条件（上から順に最初に一致したもの） | route | 実体 |
|---|---|---|
| `WriterSettings::linearization == true` | `write_linearized` | `crates/flpdf/src/linearization/writer.rs::write_linearized_for_pdf_writer`（`crates/flpdf/src/writer.rs:773`。`options.qdf` はここで強制的に false にされる） |
| `options.pclm == true` | `pclm` | `crates/flpdf/src/writer.rs::write_pclm`（`crates/flpdf/src/writer.rs:3672`） |
| `options.qdf == true` | legacy coordinator | `plain::eligible` の `!qdf` に落ちる |
| `options.encrypt.is_some()` | legacy coordinator | 出力暗号化。plain は暗号化経路を持たない |
| `options.copy_encryption.is_some()` | legacy coordinator | copy-encryption |
| 入力が暗号化されている（`pdf.is_encrypted()`） | legacy coordinator | 復号して平文出力する場合も含む |
| `options.content_normalization == true` | legacy coordinator | `normalized_stream_refs` を使う経路 |
| `!options.extra_header_text.is_empty()` | legacy coordinator | plain は extra header を出力しない |
| `force_pdf_version < 1.5` かつ requested が Generate | legacy coordinator | 抑制で `Disable` になり `mode != options.object_streams` |
| `force_pdf_version < 1.5` かつ requested が Preserve かつ **非** encrypting | legacy coordinator | 同上（encrypting の Preserve は抑制されないので、この行では落ちない） |
| 上記いずれにも当たらない（Disable / Preserve / Generate × 任意の `decode_level` / `deterministic_id` / `static_id` / `preserve_unreferenced_objects` / `compress_streams`） | shared plain pipeline | `crates/flpdf/src/writer/plain/mod.rs::write_plain`（`crates/flpdf/src/writer.rs:3676`） |

plain pipeline 内部の採番も 1 本ではない（`PlainWritePlan::build`、
`crates/flpdf/src/writer/plain/plan.rs:117-266`）:

| plain 内の分岐 | 採番 |
|---|---|
| `ObjectStreamMode::Disable` | `CanonicalCatalogFirstRenumber`（`crates/flpdf/src/writer/plain/plan.rs:172`） |
| `Preserve` かつ source に compressed entry **なし** | `CanonicalCatalogFirstRenumber`（`crates/flpdf/src/writer/plain/plan.rs:185`） |
| `Preserve` かつ source に compressed entry **あり** | `ObjectStreamRenumber`（`renumber_plain`、`crates/flpdf/src/writer/plain/plan.rs:219`） |
| `Generate` | `ObjectStreamRenumber`（`renumber_plain`、`crates/flpdf/src/writer/plain/plan.rs:259`） |

legacy coordinator 内部（main の状態）:

| legacy 内の分岐 | 採番 |
|---|---|
| `options.qdf && !plan.batches.is_empty()` | `ObjectStreamRenumber` |
| `qpdf_generate_encrypted`（`encrypting && !qdf && Generate && !batches.is_empty()`） | `ObjectStreamRenumber` |
| `qpdf_preserve_source_objstm`（`!qdf && Preserve && !batches.is_empty()`）**(#1486)** | `ObjectStreamRenumber` + `ObjectStreamGroup::SourceBacked` |
| それ以外 | `CanonicalCatalogFirstRenumber` + container-above-max |

## unknown / probe

| 項目 | 決められない理由 | 必要な source / probe |
|---|---|---|
| U1（= 行 D31）: linearized 経路の Preserve batch 導出が D6 の canonical owner と同じ member 順序・同じ stale-generation 除去を行うか | `crates/flpdf/src/linearization/plan.rs::objstm_batches`（`crates/flpdf/src/linearization/plan.rs:2332`）と `route_objstm_containers`（`crates/flpdf/src/linearization/plan.rs:2835`）を読み切っていない。`objstm_membership_linearized_with_eligibility` は Generate の even-split しか扱わないので、linearized Preserve の経路が別にある | `rg -n 'ObjStmBatchPlan\|route_objstm_containers\|ContainerPart' crates/flpdf/src/linearization/plan.rs` を読み、`preserveObjectStreams`（`libqpdf/QPDFWriter.cc:1939-1967`）との member 順序・filter 条件を 1:1 で突き合わせる |
| U2: plain pipeline で `decode_level != None` のとき `initializeSpecialStreams` 相当（`normalized_streams`）が不要と言い切れるか | qpdf は `qdf_mode \|\| normalize_content \|\| stream_decode_level` の 3 条件で `initializeSpecialStreams` を呼ぶ（`libqpdf/QPDFWriter.cc:2113-2115`）が、flpdf の `normalized_stream_refs` は `content_normalization` のみで作られ（`crates/flpdf/src/writer.rs:3731-3742`）、plain pipeline には相当物が無い。`normalized_streams` が decode-only 経路で出力に影響するかを qpdf source から確定していない | `rg -n 'normalized_streams' $Q/libqpdf/QPDFWriter.cc` で全参照箇所を洗い、`willFilterStream`（`libqpdf/QPDFWriter.cc:1239-1315`）内での使われ方が `normalize_content` 依存かを確認。影響するなら `qpdf --decode-level=generalized --static-id` と flpdf の byte 比較 probe を追加 |
| U3: D12 の欠番 entry 2 通り（plain `00000 f` / 他 3 実装 `65535 f`）が実出力に現れうるか | 符号化の差と「揃える先の qpdf 挙動が存在しない」ことは確認済み（D12 参照）。未確定なのは **非 linearized の flpdf 採番で 1..max に欠番が生じる入力があるか**。生じるなら flpdf は qpdf が `std::logic_error` で落ちる状態を黙って出力していることになり、2 通りの符号化のどちらを正とするかは oracle 照合ではなくメンテナ判断になる。`filter_objstm_batches_for_output` / `retain_reachable_object_stream_members` の事後 filter が採番後に member を落とす経路が候補 | `plan.batches` の filter 前後で `old_to_new` の値域が 1..max で連続かを assert する unit probe を足す。**qpdf 側の probe は成立しない** — qpdf は欠番行を書く前に `std::logic_error` を投げるので、`qpdf --show-xref` に欠番行は原理的に現れない（D12 参照） |
| U4: D30（`write_qpdf_to_memory`）と D19（`write_linearized`）の test-only 入口を削除できるか | test caller が実際に `PdfWriter` 経由へ書き換え可能かは、各 test の前提（plan を外から差し替えているか等）を読まないと決まらない | `rg -n 'write_linearized\(' crates/flpdf/src/linearization/{back_patch,show}.rs` の各呼び出しが独自 `LinearizationPlan` を組み立てているかを確認する |
| U5: D27 の旧 `sweep_unreachable_objects` を `qpdf-deviation` マーカー対象とすべきか | 旧 pre-write route と 3 production consumer は D27 cutover で削除済み。残る `_except` は multi-source merge の別 leaf で、qpdf の single-source writer reachability との差を表す marker ではない | `python3 scripts/check-qpdf-deviation-markers.py --check` と `python3 scripts/qpdf-route-callers.py --root . --symbol sweep_unreachable_objects --expect-zero`。残る `_except` は prod 1 のまま次の cutoverで扱う |
| U6: D20 の 4 コメントが指す「後で patch される」hint フィールドを、ループ廃止後は **誰が最終値へ書き込んでいるか** | `crates/flpdf/src/linearization/hint_shared.rs:1081,1089,1103` は「convergence loop が責任を持って patch する」と明記している。収束ループが無いことは確認済み（D20）なので、残る問いは「コメントが陳腐化しただけか」ではなく **その最終値の書き込み元が実在するか**。実在しなければ hint table に placeholder が残る live bug、実在すれば純粋な doc 負債 | `sed -n '1070,1110p' crates/flpdf/src/linearization/hint_shared.rs` で当該フィールド名を特定し、そのフィールドへの書き込み元を `rg -n '<field>' crates/flpdf/src/linearization/` で全列挙する。`crates/flpdf/src/linearization/writer.rs:4333` の `encode_hint_stream` が pass-1 offset から再導出しているなら doc 負債で確定。linearized 出力の byte gate が通っている以上 live bug の可能性は低いが、**確認はしていない** |

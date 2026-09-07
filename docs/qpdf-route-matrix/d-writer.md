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
| D1 | `QPDFWriter::write` / `QPDFWriter::doWriteSetup`（mode 正規化 → 単一 dispatch） | `libqpdf/QPDFWriter.cc:2187-2213,2059-2184` | `crates/flpdf/src/writer.rs::PdfWriter::write`（`pub`、`crates/flpdf/src/writer.rs:719`） | prod: 5 (`crates/flpdf/src/job/check.rs:350`, `crates/flpdf/src/job/lifecycle.rs:2506`, `crates/flpdf/src/job/page_split.rs:282`, `crates/flpdf-cli/src/main.rs:330,343`) / test: 25 files（`crates/flpdf-qtest-tools/` の driver 43 箇所と `crates/flpdf/examples/` 7 箇所を除く） | mixed | `crates/flpdf/src/writer.rs::PdfWriter::write` | qpdf は `write()` 内で 2 分岐（`writeLinearized` / `writeStandard`）、pclm は `writeStandard` 内の 1 段深い分岐。flpdf は **4 つの独立実装**へ分岐する: `settings.linearization` で `write_linearized_for_pdf_writer`（`crates/flpdf/src/writer.rs:773`）、以降 `emit_canonical_pdf` 内で `write_pclm` / `plain::write_plain` / legacy coordinator。dispatch 条件表は「WriterOptions と route の対応」節 |
| D2 | `QPDFWriter::enqueueObject`（非 linearized の唯一の採番点、container-first・member 範囲即時予約） | `libqpdf/QPDFWriter.cc:1072-1141,1057-1069` | `crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber`（`pub(crate)`、`crates/flpdf/src/writer/rewrite_renumber.rs:626`） | prod: 4 (`crates/flpdf/src/writer/plain/plan.rs:687,688,727`, `crates/flpdf/src/writer.rs:4014` legacy coordinator) / test: 0 | mixed | 未完成（既存の container-first 部分は `ObjectStreamRenumber`） | 非 linearized の採番が `ObjectStreamRenumber` と `CanonicalCatalogFirstRenumber`（D3）に分かれる。両者とも書き込み前に graph を walk して map を完成させる（`rewrite_renumber.rs:207,799`）。qpdf は `unparseChild`（`QPDFWriter.cc:1144-1157`）が書き込み中に子を enqueue するため、container-first があるだけでは責務全体の canonical 完了とは言えない。`RenumberMap`（D4）は qpdf 自身にもある linearized 専用採番なので、この重複の削除対象ではない |
| D3 | 同上（`enqueueObject` の container なし側 = 純粋な到達順採番） | `libqpdf/QPDFWriter.cc:1072-1141` | `crates/flpdf/src/writer/rewrite_renumber.rs::CanonicalCatalogFirstRenumber`（`pub(crate)`、`crates/flpdf/src/writer/rewrite_renumber.rs:89`） | prod: 5 (`crates/flpdf/src/writer/plain/plan.rs:172,185,709` [`:172` Disable / `:185` Preserve-no-source-ObjStm / `:709` reachability oracle]、`crates/flpdf/src/writer.rs:3788` legacy coordinator、`crates/flpdf/src/linearization/plan.rs:991`) / test: 0（`crates/flpdf/tests/rewrite_renumber_module_route_tests.rs:107,130` はコメントと文字列リテラル内の言及で、規則 (a)(e) により caller ではない） | mixed | `crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber` | container が 1 つも無い場合に限り D2 と一致するため、Disable / Preserve-without-source-ObjStm では byte 差が出ていない。container がある経路（Preserve-with-source / Generate / legacy coordinator の非 #1486 スライス）で container-above-max になるのが `flpdf-hi08` が捕らえた乖離。残 caller は上記 5 箇所すべて |
| D4 | `QPDFWriter::writeLinearized` の事前採番（second half → first half、part 単位） | `libqpdf/QPDFWriter.cc:2575-2646` | `crates/flpdf/src/linearization/renumber.rs::RenumberMap`（`pub`、`crates/flpdf/src/linearization/renumber.rs:84`） | prod: 19 (`crates/flpdf/src/linearization/writer.rs`=12, `crates/flpdf/src/linearization/renumber.rs`=2, `crates/flpdf/src/linearization/part1.rs`=2, `crates/flpdf/src/linearization/{hint_page,hint_shared,plan}.rs`=各1) / test: 107 (8 files) | canonical | `crates/flpdf/src/linearization/renumber.rs::RenumberMap` | qpdf も linearized では `enqueueObject` の採番を使わないので、専用機構が 1 本あること自体は逸脱でない。`RenumberMap::from_plan`（`crates/flpdf/src/linearization/renumber.rs:191`）が part 順の slot 割当を持つ |
| D5 | `QPDFWriter::assignCompressedObjectNumbers`（container enqueue 時に member 範囲を即時予約） | `libqpdf/QPDFWriter.cc:1057-1069` | `crates/flpdf/src/writer/object_streams/planning.rs::ObjectStreamGroup`（`pub(crate)`、`crates/flpdf/src/writer/object_streams/planning.rs:73`）を `ObjectStreamRenumber::build_with_stream_policy` に渡す | prod: 19 (`crates/flpdf/src/writer/plain/plan.rs`=10, `crates/flpdf/src/writer/rewrite_renumber.rs`=6, `crates/flpdf/src/writer/object_streams/planning.rs:100,244`, `crates/flpdf/src/writer.rs:4005`) / test: 0 | mixed | `crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber` | 現行の Preserve は `writer.rs:4108` で `SourceBacked { source, members }` を作り、source identity を採番へ渡す。Generate の `Synthetic { members }` は source identity を持たず（`rewrite_renumber.rs:642`）、plain は `plain/plan.rs:237-242` で構築する。qpdf は `generateObjectStreams`（`QPDFWriter.cc:1999`）で source QPDF に indirect null container を生成してから採番するため、この allocation 責務は D7 の分割アルゴリズムとは別の未移植範囲。Preserve の旧「常に Synthetic」という記述は #1486 merge 後には成立しない |
| D6 | `QPDFWriter::preserveObjectStreams`（source container map ∩ compressible set） | `libqpdf/QPDFWriter.cc:1939-1967` | `crates/flpdf/src/writer/object_streams/planning.rs::plan_qpdf_preserve_object_streams_with_unreferenced` | prod: plain Preserve / test: empty-map ordering | mixed | plain Preserve planner | document `get_object_stream_data` を compressible walk より先に呼び、空なら解決せず戻る。legacy coordinator と linearized は別の Preserve batch 導出を保持しており、残 consumer は `.48.54`/`.48.65` で移行する。 |
| D7 | `QPDFWriter::generateObjectStreams` の even split 部分（null container allocation は D5） | `libqpdf/QPDFWriter.cc:1970-2006` | `crates/flpdf/src/writer/object_streams/eligibility.rs::even_split_into_streams`（`pub(crate)`、`crates/flpdf/src/writer/object_streams/eligibility.rs:167`） | prod: 4 (`crates/flpdf/src/writer/object_streams/mod.rs:9` re-export, `crates/flpdf/src/writer/plain/plan.rs:237`, `crates/flpdf/src/linearization/writer.rs:3149`, `crates/flpdf/src/linearization/plan.rs:2724`) / test: 0 | canonical | `crates/flpdf/src/writer/object_streams/eligibility.rs::even_split_into_streams` | 分割アルゴリズム自体は 1 本に集約済み。legacy coordinator は `plan_object_streams_with_reachability` 経由で同じ分割へ到達する。canonical 判定は even split の責務に限定する。qpdf が同じ関数内で行う source indirect null container の生成は D5 の未移植範囲であり、この行は `generateObjectStreams` 全体の完了を意味しない |
| D8 | `QPDF::getCompressibleObjGens`（trailer 起点の LIFO DFS、stream/Sig/Encrypt 除外） | `libqpdf/QPDF.cc:2393-2474,1996-2005` | `crates/flpdf/src/reader.rs::get_compressible_objgens` → live resolver cache/removal | plain Generate / tests: generation aliases, visited order, bounds, exclusions, provider lifetime | mixed | document-owned `Pdf::get_compressible_objgens` | Encrypt取得→getObjectCount→object number bitmap→live cache upper_boundの順。stale generationは正本からremoveしretained aliasをdirect null化する。stream Length edgeだけを省き、他edgeから到達するLengthは候補に残す。plain Generateを接続。旧eligibilityのgeneration snapshot/removed_refsとplain Preserve・linearized consumerは残移行対象としてマーク済み。 |
| D9 | `QPDF::getObjectStreamData`（入力 xref の type 2 entry → source container） | `libqpdf/QPDF.cc:2381-2390`、`include/qpdf/QPDF.hh:757-761` | `crates/flpdf/src/reader.rs::get_object_stream_data` → `crates/flpdf/src/reader/resolver.rs::get_object_stream_data` | prod: document entry → plain Preserve / tests: mixed rows, prefilled map, live rows, lazy IO | mixed | document-owned type-2 mapping | source xref 正本から objnumber → container number を caller map へ追記・上書きし、clear や object 解決をしない。plain Preserve の再filterを撤去。`source_xref_entries` は reader 自身の責務と残 consumer 用に維持し、legacy `plan_preserve`、writer の source-container lookup、linearized の membership/lookup は後続で移行する。 |
| D10 | `QPDFWriter::writeObjectStream`（2 パス・objgen 昇順 member・`/Extends` 複写） | `libqpdf/QPDFWriter.cc:1621-1758,1606-1618` | `crates/flpdf/src/writer/object_streams/emission.rs::emit_objstm_body_from_handles_with_writer`（`pub(crate)`、`crates/flpdf/src/writer/object_streams/emission.rs:45`） | prod: 3 (`crates/flpdf/src/writer/plain/body.rs:63`, `crates/flpdf/src/writer.rs:4860`, `crates/flpdf/src/linearization/writer.rs:329`) + re-export 1 (`crates/flpdf/src/writer/object_streams/mod.rs:13`) / test: 0 | canonical | `crates/flpdf/src/writer/object_streams/emission.rs::emit_objstm_body_from_handles_with_writer` | ObjStm body の 2 パス生成は 3 経路すべてがこの 1 関数を通る。QDF 変種は `…_qdf`（`crates/flpdf/src/writer/object_streams/emission.rs:58`）。ただし container dict の組み立て（`/Type /ObjStm /Length /Filter /N /First`）と `/Extends` の付与は呼び出し側に残っており、そこは D11 の smear に含まれる |
| D11 | `QPDFWriter::writeObject` / `unparseObject`（body 1 オブジェクトの emission） | `libqpdf/QPDFWriter.cc:1036-1054,1761-1809,1318-1603` | `crates/flpdf/src/writer/write_object.rs::WriteObject` → `crates/flpdf/src/writer/plain/body.rs::PlainObjectEmitter` | plain top-level / source-backed container dispatch | mixed | `WriteObject::write_object` | 共通 primitive が container 判定、progress、QDF コメント、採番済み ID の open/setDataKey/unparse/clear/close、member newline、indirect Length holder の順序を所有する。plain の通常 object は live handle を progress 後に unparse する。source-backed container は共通入口から既存 container owner へ委譲。生成 container の identity、ObjStm member の旧 newline helper、specialized/PCLm/linearized consumer は後続の `.48.51`/`.48.65` で移行する。先行 placement の子発見は `.48.53` に残る。 |
| D12 | `QPDFWriter::writeXRefTable`（classic xref table） | `libqpdf/QPDFWriter.cc:2343-2379,2335-2340` | 共有 primitive `crates/flpdf/src/writer/plain/xref.rs::write_xref_table`（`pub(crate)`、`:287`）を plain first consumer `append_xref_and_trailer`（`:75`）から接続 | shared prod: 1 / first consumer prod: 2 / test: 7 | mixed | `crates/flpdf/src/writer/plain/xref.rs::write_xref_table` | qpdf の entry 0、type-1 offset、generation 0、range、`suppress_offsets`、hint補正を共有primitiveへ移植した。missing/free/type2を `00000 f`/`65535 f` として捏造せず、`QPDFXRefEntry::getOffset`（`QPDFXRefEntry.cc:27-32`）と同じ `Error::Internal("getOffset called for xref entry of type != 1")` を返す。plain classicが最初のconsumerで、specialized/PCLm/linearizedの残るtable loopは後続sliceへ委譲する。正常writerのgap producer調査（U3）は本sliceと分離する |
| D13 | `QPDFWriter::writeXRefStream`（type 0/1/2 バイナリ + PNG predictor） | `libqpdf/QPDFWriter.cc:2392-2495,2382-2389` | 共有 owner `crates/flpdf/src/writer/serialize.rs::xref_stream`（`build_entries_with_self` / `encode_payload_for_policy` / widths / dictionary）を plain と linearized の全 stream consumer から利用 | shared prod: 3（plain / linearized first-half / linearized second-half） / test: 3 | canonical | `crates/flpdf/src/writer/serialize.rs::xref_stream` | `build_entries_with_self` が qpdf の self-xref 事前登録、type-1 field 3 の常時 zero、type-2 member index を一度に確定する。`encode_payload_for_policy` が plain と linearized pass-1/final の raw・PNG predictor・Flate policy を共有する。legacy coordinator は plain owner へ委譲し、linearized の first-half/second-half は consumer 固有の範囲・padding・trailer framing だけを保持する |
| D14 | `QPDFWriter::writeTrailer` / `getTrimmedTrailer` | `libqpdf/QPDFWriter.cc:1160-1236,2009-2032` | `crates/flpdf/src/writer/plain/plan.rs::canonical_trailer_entries_with_visibility`（`pub(crate)`、`crates/flpdf/src/writer/plain/plan.rs:588`）と `crates/flpdf/src/writer.rs::build_writer_trailer_handle`（private、`crates/flpdf/src/writer.rs:3015`） | `canonical_trailer_entries_with_visibility` prod: 2 (`crates/flpdf/src/writer/plain/plan.rs:581`, `crates/flpdf/src/writer.rs:5419`) / test: 2。`build_writer_trailer_handle` prod: 5 (`crates/flpdf/src/writer/plain/plan.rs:334`, `crates/flpdf/src/writer.rs:3485,3531,5220,5363`) / test: 0 | mixed | `crates/flpdf/src/writer.rs::build_writer_trailer_handle`（`getTrimmedTrailer` 相当）| **`getTrimmedTrailer` 側は既に 1 本に集約されている**: `build_writer_trailer_handle` を plain（`crates/flpdf/src/writer/plain/plan.rs:334`）・pclm（`crates/flpdf/src/writer.rs:3485,3531`）・legacy classic（`crates/flpdf/src/writer.rs:5220`）・legacy xref stream（`crates/flpdf/src/writer.rs:5363`）の 4 経路すべてが通る。割れているのは **`writeTrailer` の書き出し側** で、`crates/flpdf/src/writer/object.rs::write_trailer_with_ref_map`（`crates/flpdf/src/writer/object.rs:151` trait / `:952` impl。pclm `crates/flpdf/src/writer.rs:3511,3521,3555,3565` と legacy classic `:5272,5283,5309,5321` が使う）、`crates/flpdf/src/writer/plain/xref.rs` の `write_canonical_classic_trailer`（`crates/flpdf/src/writer/plain/xref.rs:270`）+ xref-stream 用 trailer、`crates/flpdf/src/linearization/writer.rs:1023` の raw byte 組み立て（key 順を `/Size /ID` に固定するため `write_pdf` を使わないと明記）の 3 系統。`canonical_trailer_entries_with_visibility` は「どの entry が null 可視性を通るか」を決める補助で、xref-stream trailer だけが使う。null 値 dict キー抑制（`libqpdf/QPDFWriter.cc:1490-1491`）は `suppress_null_values` で共通化済み |
| D15 | `QPDFWriter::writeEncryptionDictionary`（body 後・xref 前、`std::map` の key 昇順） | `libqpdf/QPDFWriter.cc:2244-2256`、`writeStandard` からの呼び出し位置は `libqpdf/QPDFWriter.cc:3017-3019`（他の呼び出し元は `writeLinearized` の `libqpdf/QPDFWriter.cc:2795`） | `crates/flpdf/src/writer/encrypted_strings.rs::write_encryption_dictionary_handle`（`pub(crate)`、`crates/flpdf/src/writer/encrypted_strings.rs:313`） | prod: 2 (`crates/flpdf/src/writer.rs:5163` legacy coordinator, `crates/flpdf/src/linearization/writer.rs:2360`) / test: 0 | canonical | `crates/flpdf/src/writer/encrypted_strings.rs::write_encryption_dictionary_handle` | 出力位置も qpdf と一致: legacy coordinator は body 全 object の後・`let xref_offset = bytes.len();` の直前（`crates/flpdf/src/writer.rs:5155-5170`）、linearized は part4 の直後。**plain pipeline は暗号化経路を一切持たない**（`plain::eligible` が `encrypt.is_none() && copy_encryption.is_none() && !pdf_is_encrypted` を要求する、`crates/flpdf/src/writer/plain/mod.rs:50-62`）ので、暗号化された非 linearized 出力は必ず legacy coordinator を通る |
| D16 | `QPDFWriter::setEncryptionParametersInternal` / `setEncryptionParameters` / `copyEncryptionParameters` | `libqpdf/QPDFWriter.cc:777-840,591-648,651-702` | `crates/flpdf/src/writer.rs::WriterSetupState`（`pub(crate)`、`crates/flpdf/src/writer.rs:2735`）→ `EncryptionParameters::into_context` | prod: shared setup 1 (`crates/flpdf/src/writer.rs`), route consumers 2 (`crates/flpdf/src/writer.rs`, `crates/flpdf/src/linearization/writer.rs`) / test: 1 | mixed | `crates/flpdf/src/writer.rs::build_writer_setup` | qpdf は 3 つの setter が `setEncryptionParametersInternal` 1 本へ収束し、以後は `m->encryption_dictionary` という単一 state。flpdf も qpdf 形状の option 正規化後に `WriterSetupState` が `EncryptionParameters` を一度だけ構築し、standard / linearized は各 route の `/Encrypt` slot だけを割り当てて `EncryptionParameters::into_context` から同じ辞書・file key・ID0・metadata state を使う。`EncryptionContext` は route-specific slot を含む出力 consumer state であり、setter の再構築 owner ではない。`/Encrypt` の最小版数決定（R6→1.7 ext8 等）は `crates/flpdf/src/writer.rs::encryption_version_floor` に分離されている |
| D17 | `QPDFWriter::setDataKey` / `pushEncryptionFilter`（object ごとの data key） | `libqpdf/QPDFWriter.cc:843-847,976-1000` | `crates/flpdf/src/writer/encryption_state.rs::WriterEncryptionState`（`pub(crate)`、`crates/flpdf/src/writer/encryption_state.rs:40`） | prod: 5 (`crates/flpdf/src/writer/encrypted_strings.rs:32,49,239`, `crates/flpdf/src/writer/encryption_state.rs:48`, `crates/flpdf/src/writer.rs:3191`) / test: 9 (`crates/flpdf/src/writer/encryption_state.rs`) | canonical | `crates/flpdf/src/writer/encryption_state.rs::WriterEncryptionState` | `set_data_key` / `with_object_data_key` が qpdf の set/unparse/clear 順序を写す。`docs/qpdf-correspondence.md:389` §3 に既存記載あり |
| D18 | `QPDFWriter::writeLinearized`（production 経路） | `libqpdf/QPDFWriter.cc:2537-2904` | `crates/flpdf/src/linearization/writer.rs::write_linearized_for_pdf_writer`（`pub(crate)`、`crates/flpdf/src/linearization/writer.rs:3109`） | prod: 1 (`crates/flpdf/src/writer.rs:773`) / test: 0 | canonical | `crates/flpdf/src/linearization/writer.rs::write_linearized_for_pdf_writer` | production の linearized 出力はこの 1 本のみ。`PdfWriter::write` は `emit_canonical_pdf` より前に分岐するので、linearized は plain / legacy / pclm のどれとも合流しない |
| D19 | 同上（plan/renumber を外から与える test 用入口） | `libqpdf/QPDFWriter.cc:2537-2904` | `crates/flpdf/src/linearization/writer.rs::write_linearized`（`pub(crate)`、項目単位 `#[cfg(test)]`） | prod: 0 / test: 3 (`linearization/show.rs:1999`, `linearization/back_patch.rs:382,754`) | canonical | `crates/flpdf/src/linearization/writer.rs::write_linearized_for_pdf_writer` | canonical implementation に直接委譲する test scaffolding として分類。旧表の production bridge/deletion 候補という扱いを訂正する。`back_patch.rs:382` は back-patch 前の `LinearizedDocument` を検証するため、この観測点を持つ。`PdfWriter::write` は内部で back-patch まで行うので、単純な入口置換はテストの責務を失う。`:754` は error、`show.rs:1999` は出力 fixture を観測する。別の PDF 表現や legacy semantics を維持している経路ではない |
| D20 | `writeLinearized` の 2 パス構造（pass1 → hint 1 回 → pass2、反復なし） | `libqpdf/QPDFWriter.cc:2656-2904`、特に `libqpdf/QPDFWriter.cc:2864-2884` | `crates/flpdf/src/linearization/writer.rs::write_linearized_for_pdf_writer` 内の `do_write_pass` 2 回（`crates/flpdf/src/linearization/writer.rs:3986` pass1 / `crates/flpdf/src/linearization/writer.rs:4380` 付近 final） | prod: 1 / test: 0（D18 と同一関数） | canonical | `crates/flpdf/src/linearization/writer.rs::write_linearized_for_pdf_writer` | `flpdf-26l3`（収束ループ廃止）は解消済み。pass 1 → hint 構築 → pass 2 の 2 パス構造である。`hint_shared.rs:1081,1089,1103` と `part1.rs:25` に残る convergence-loop コメントは stale と確認した。最終 `first_object_number` は `SharedObjectHintTable::from_plan`（`hint_shared.rs:311`）が member/container map から算出し、`linearization/writer.rs:4152` がその map を渡す。`:4298` は番号を読み location を更新するだけで、後段の番号 patch は不要。U6 と既存 `flpdf-o99` の古い前提を参照 |
| D21 | `QPDF::optimize` / `filterCompressedObjects`（linearized の object-user map） | `libqpdf/QPDF_optimization.cc:57-118,340-381` | `crates/flpdf/src/optimization.rs::Optimization`（`pub(crate)`、`crates/flpdf/src/optimization.rs:21`） | prod: 10 (`crates/flpdf/src/linearization/plan.rs:879,1001,2395,2480,2836`, `crates/flpdf/src/linearization/check.rs:335,410,465`, `crates/flpdf/src/linearization/writer.rs:3370`, `crates/flpdf/src/optimization.rs:32`) / test: 1 | canonical | `crates/flpdf/src/optimization.rs::Optimization` | `docs/qpdf-correspondence.md` §4 が ✅ 済みと記載し、実装も 1 モジュールに集約されている。`prepare_for_linearized_write`（`crates/flpdf/src/optimization.rs:152`）は `optimize` と `prepare_pdf` を共有する部分適用で、prod caller は `crates/flpdf/src/linearization/writer.rs:3370` の 1 箇所。D27 follow-up の stream-parameter probe も `Optimization::update_object_maps` の callback 内でだけ実行し、qpdf の page/trailer/root 起点の到達範囲を越えて `pdf.object_refs()` を解決しない |
| D22 | `QPDF::getLinearizedParts` / `calculateLinearizationData`（part4/6/7/8/9） | `libqpdf/QPDF_linearization.cc:1435-1449,963-1403,1174-1336` | `crates/flpdf/src/linearization/plan.rs::LinearizationPlan`（`pub`、`crates/flpdf/src/linearization/plan.rs:744`） | prod: 21 (`crates/flpdf/src/linearization/writer.rs`=9, `crates/flpdf/src/linearization/hint_page.rs`=4, `crates/flpdf/src/linearization/{plan,renumber}.rs`=各3, `crates/flpdf/src/linearization/{hint_shared,part1}.rs`=各1) / test: 57 (10 files) | canonical | `crates/flpdf/src/linearization/plan.rs::LinearizationPlan` | `from_pdf_with_writer_options`（`crates/flpdf/src/linearization/plan.rs:959`）が production 入口。part 分類は qpdf の `lc_*` 集合を写している |
| D23 | `doWriteSetup` の ObjStm 除外（linearized なら page + root、encrypted なら root） | `libqpdf/QPDFWriter.cc:2140-2158` | `crates/flpdf/src/writer/object_streams/planning.rs::filter_objstm_batches_for_output`（`pub(crate)`、`crates/flpdf/src/writer/object_streams/planning.rs:165`） | prod: 1 (`crates/flpdf/src/writer.rs:3889` legacy coordinator) + re-export 1 / test: 0 | mixed | `crates/flpdf/src/writer/object_streams/planning.rs::filter_objstm_batches_for_output` | 同じ除外規則が `crates/flpdf/src/linearization/plan.rs::objstm_membership_linearized_with_eligibility`（`crates/flpdf/src/linearization/plan.rs:2709`）の `:2727-2732` に **独立実装** されている（page refs + root の erase セット）。plain pipeline は呼ばないが、`plain::eligible` が encrypted を除外し linearized は別経路なので `output_linearized \|\| output_encrypted` が常に false になり、現状は差が出ない。除外規則を変えるときは 2 箇所を同時に直す必要がある |
| D24 | `QPDFWriter::enqueueObjectsPCLm` + `writeStandard`（pclm は xref/trailer を standard と共有） | `libqpdf/QPDFWriter.cc:2928-2954,2991-3044` | `crates/flpdf/src/writer/pclm.rs::Plan`（`pub(crate)`、`crates/flpdf/src/writer/pclm.rs:28`）と `crates/flpdf/src/writer.rs::write_pclm`（private、`crates/flpdf/src/writer.rs:3354`） | `write_pclm` prod: 1 (`crates/flpdf/src/writer.rs:3672`) / test: 0 | mixed | `crates/flpdf/src/writer/plain/xref.rs::append_xref_and_trailer`（xref/trailer 部分） | qpdf の pclm は `enqueueObjectsPCLm` だけが差分で、body ループ・xref・trailer は `writeStandard` と完全共有。flpdf の `write_pclm` は body ループ・classic xref（`crates/flpdf/src/writer.rs:3451`）・trailer 書き出しをすべて自前で持つ独立実装で、qpdf の共有構造を再現していない |
| D25 | `QPDFWriter::prepareFileForWrite`（`/Extensions` / `/ADBE` の direct 化 + `fixDanglingReferences`）と root `unparseObject` の出力専用 ADBE reconciliation | `libqpdf/QPDFWriter.cc:1347-1435,2036-2056`、`libqpdf/QPDF.cc:1259-1269` | permanent graph preparation: `crates/flpdf/src/writer.rs::prepare_file_for_write`（`:1629`）、plain root output owner: `crates/flpdf/src/writer/object.rs::root_output_copy_with_adbe` → `writer/plain/{plan,body}.rs` | prepare prod: 1 / plain output prod: 3 / test: 3 | mixed | plain `ObjectWriterEmission::write_root_object_with_ref_map_and_removed` | qpdf の unsafe shallow copy 境界を plain root に移し、`/ADBE` の作成・置換・除去を live Catalog へ事前適用しない。plain route は snapshot/restore を使わず、specialized/QDF/PCLm/linearized の legacy consumer と snapshot cleanup は後続。root container のみ unsafe shallow copy とし、既存 direct Extensions 内の ADBE 置換/削除は live alias にも反映する。先行 placement が捨てる ADBE の参照先を出力する差は未解消であり、`.48.53`/`.48.65` の live queue 移行が `.48.60` 完了の前提。 |
| D26 | `QPDFWriter::initializeSpecialStreams`（page seq / contents seq / normalized streams） | `libqpdf/QPDFWriter.cc:1912-1936`、トリガは `libqpdf/QPDFWriter.cc:2113-2115` | `crates/flpdf/src/writer.rs::initialize_special_streams`（`PdfWriter::write` が setup で呼び、`emit_canonical_pdf_with_special_streams` が specialized consumer として受け取る） | prod: 1 (`crates/flpdf/src/writer.rs` の `PdfWriter::write`) / test: 3（direct wrapper と setup snapshot tests） | mixed | `crates/flpdf/src/writer.rs::initialize_special_streams` | qpdf と同じ `qdf \|\| content_normalization \|\| decode_level != None` trigger で、修復済み page snapshot から 3 map と direct-content container set を一度だけ生成する。QDF の `page_seq` / `contents_seq` と stream policy の normalized set は同じ state を参照し、normalized set の適用自体は `content_normalization` gate に限定する。linearized/他 route の consumer 移行は後続 |
| D27 | `enqueueObject` による到達性（qpdf に独立した削除パスは無い） | `libqpdf/QPDFWriter.cc:1072-1141,2907-2925` | `sweep_unreachable_objects` と multi-source merge 専用の `sweep_unreachable_objects_except` はともに撤去済み。書き込み時の canonical owner は `crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber` | pre-write route: prod 0 / test 0 | canonical | `crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber`（書き込み経路の到達性） | pre-write sweep の撤去という本行の責務は完了。`sweep_unreachable_objects` と `_except` は Rust 全域 0 hit（2026-09-06）。closed `flpdf-3yn9.44` / `.45` が single/multi-source consumer の撤去、`.44.1` / `.44.1.1` が reachable stream probe の移行を所有する。新たな sweep cleanup は不要。書き込み前採番 walk と実際の emission の統合は D2/D3/D11 の別責務なので、本行の完了を writer 全体の単一 owner 完了とは扱わない |
| D28 | `getCompressibleObjGens` と writer到達性・stream parameter処理の分離 | `libqpdf/QPDF.cc:2393-2474`、`libqpdf/QPDFWriter.cc:1953-1958` | document candidate walk / `crates/flpdf/src/writer/plain/plan.rs::retain_reachable_object_stream_members` | Generate candidate: document owner / remaining writer rewalks | mixed | `Pdf::get_compressible_objgens` owns candidate selection | plain Generateの候補選択から旧removed_refsとindirect ObjStm Length集合を切り離した。writerのstream-parameter処理、採番reachability再walk、Preserve intersection、linearizedは別の残責務であり、候補選択へ混合しない。 |
| D29 | `enqueueObject` の container-first を Preserve に適用（#1486 のスコープ） | `libqpdf/QPDFWriter.cc:1072-1118,1057-1069` | `writer.rs::emit_canonical_pdf_inner` の `qpdf_preserve_source_objstm`（`:4008`、現行 main に実在） | declaration 1; production 用の batch sort/group build/renumber/container/body routing が同じ関数内で参照 | mixed | `crates/flpdf/src/writer/rewrite_renumber.rs::ObjectStreamRenumber` | 現行条件は `options.object_streams == ObjectStreamMode::Preserve && !plan.batches.is_empty()`。旧 #1486 diff の `!options.qdf` 条件も現在は無い。`:4108` が `SourceBacked` を作り、`:4150` 付近で `ObjectStreamRenumber` を呼ぶ。closed `flpdf-hi08` は encrypted Preserve の numbering 修正を所有する。残る事前 Catalog-first walk、複数 mode branch、出力後 chunk merge は D2/D3/D11 の migration 範囲であり、旧 #1486 不在を理由に再実装しない |
| D30 | `write()` から出力バイトへの単一 pipeline（`QPDFWriter::write` の後段） | `libqpdf/QPDFWriter.cc:2196-2213` | `crates/flpdf/src/writer.rs::write_qpdf_to_memory`（`pub(crate)` + 項目単位 `#[cfg(test)]`、`:29`） | prod: 0 / test: 15 (`job/{page_subset,rotate,acroform_field_prune}.rs`, `pages/tree_rebuild.rs`, `page_annotation_flatten.rs`, `page_splice.rs`, `embedded_files.rs`) | canonical | `crates/flpdf/src/writer.rs::PdfWriter::write` | canonical writer lifecycle を使う byte-neutral test scaffolding。`:29-41` は `PdfWriter::new` → configure → memory sink → `write` → `get_buffer` だけで、表現変換や旧 semantics の保存をしない。旧 bridge 分類を訂正し、削除専用 issue は不要とする。CLI にある同名の別関数はこの test-only helper の production caller ではない |
| D31 | `QPDFWriter::preserveObjectStreams` の linearized 側適用（`doWriteSetup` で Preserve を決めた後、`writeLinearized` が `object_to_object_stream_no_gen` として使う） | `libqpdf/QPDFWriter.cc:1939-1967,2541,2575-2617` | `linearization/plan.rs::objstm_batches_preserve`（`:2519`）→ `route_objstm_containers` | `objstm_batches` の Preserve arm が呼び、`linearization/writer.rs::ObjStmLayout::resolve_batches`（`:151`）が結果を使用 | mixed | `QPDFWriter::preserveObjectStreams` 相当の共有 membership owner が必要（D6） | unknown は source 調査で解消。`:2529-2578` が raw xref から source container 別に再構築し、source-index 順を保持、assigned 集合/page/root/type/Sig で filter する。plain の共有 Preserve（`writer/object_streams/planning.rs:214` 以降）は compressible 集合との intersection と objgen sort を使うので、同一 owner ではない。qpdf は setup 時の source map と objgen 昇順逆 map を linearization に渡す（`QPDFWriter.cc:1939-1967,2159-2170,2541`）。既存 `flpdf-oq7g` の byte-parity 課題と共有 membership への consumer 移行を関連付ける。Preserve strict byte tests 自体は `cmp_linearize_objstm_tests.rs:587,1307` 等に既に存在するため、「Generate のみ検証」という旧記録は stale。今回そのテストの合否は再測定していない |

**D14 current slice (2026-09-07):** `writer/object.rs::TrailerKind` and
`ObjectWriterEmission::write_trailer_with_ref_map_and_kind` now own the qpdf
normal/QDF/linearized-form contract. `writer/plain/plan.rs::PlainWritePlan`
retains the live trimmed trailer handle and source-to-output map, and
`writer/plain/xref.rs::append_xref_and_trailer_with_handle` is the first
production consumer for classic xref. Xref rows and `startxref` remain local
to the plain physical writer. Specialized/PCLm/legacy xref-stream and
linearized callers remain explicit follow-up consumers; they must not recreate
the semantic trailer loop.

## WriterOptions と route の対応

dispatch は 2 段。まず `PdfWriter::write`（`crates/flpdf/src/writer.rs:719-798`）が
`settings.linearization`（= `WriterOptions` ではなく `WriterSettings`、
`crates/flpdf/src/writer/settings.rs:38`）を見て linearized を切り離し、非 linearized だけが
`emit_canonical_pdf` → `emit_canonical_pdf_inner`（`crates/flpdf/src/writer.rs:3576-5439`）へ入る。
`emit_canonical_pdf_inner` の順序は
(1) `deterministic_id && !static_id` の effective-ID 判定（両方指定時は
static ID を出力しつつ、deterministic の暗号化禁止は保持）→
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

specialized coordinator 内部（`4a2faf5c` の状態）:

| legacy 内の分岐 | 採番 |
|---|---|
| `options.qdf && !plan.batches.is_empty()` | `ObjectStreamRenumber`（Preserve の場合は下記 source-backed group を使用） |
| `qpdf_generate_standard`（`!qdf && Generate && !batches.is_empty()`、`writer.rs:3986`） | `ObjectStreamRenumber`。encrypting 条件は無く、decrypted input 等の specialized Generate も含む |
| `qpdf_preserve_source_objstm`（`Preserve && !batches.is_empty()`、`writer.rs:4008`） | `ObjectStreamRenumber` + `ObjectStreamGroup::SourceBacked`。QDF でも成立 |
| それ以外 | `CanonicalCatalogFirstRenumber` + container-above-max |

## unknown / probe

| 項目 | 決められない理由 | 必要な source / probe |
|---|---|---|
| U1（D31、source 調査済み）: linearized Preserve の共有 owner | `linearization/plan.rs:2519-2611` は source-index 順で独立に membership を構築し、plain の共有 Preserve と同じ owner ではない。D31 を unknown から mixed に変更 | source-index と objgen の順序が異なる入力、stale generation、preserve-unreferenced を実 qpdf と比較する canonical RED を用意し、共有 membership → linearized routing の順で移行する。既存 `flpdf-oq7g` にも既存 strict byte tests の存在を反映する |
| U2（source で解決）: decode-only と normalized set | `normalized_streams` を読むのは `QPDFWriter.cc:1279` の `normalize_content && normalized_streams.count(old_og)`。decode-only ではこの集合を参照しない。page/content seq map は QDF コメントで使う（`:1774-1781`） | D26 で setup snapshot の owner を `initialize_special_streams` に統合済み。decode-only でも page repair と map generation の trigger は維持し、normalized set の適用だけ `content_normalization` gate に残す。linearized/他 route の state consumer は後続 |
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
| `D26` | `flpdf-3yn9.48.61` | initializeSpecialStreamsのpage/content/normalized mapをsetupで一度生成する |
| `D16` / `D1` | `flpdf-3yn9.48.62` | encryption設定・doWriteSetupを単一writer stateに揃える |
| `D3` | `flpdf-3yn9.48.63` | linearizationの採番engineを使ったcache warmup迂回を撤去する |
| `D11` | `flpdf-3yn9.48.64` | unparseObjectのrefiltered stream辞書処理をqpdfの責務に揃える |
| `D1` / `D2` / `D3` / `D11` / `D24` / `D29` | `flpdf-3yn9.48.65` | live writer queueへ残Preserve/Generate・specialized・QDF・PCLm consumerを段階移行する |
| `D31` | `flpdf-oq7g` | linearized Preserve のbyte-parity受入拡張 |
| `D20` | `flpdf-o99` | shared hint container番号のwriter統合試験 |
| `D11` | `flpdf-vo76` | 外部stream Lengthの既存受入 |

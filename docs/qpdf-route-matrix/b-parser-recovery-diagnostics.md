# B. parser / xref recovery / warning・error・diagnostics

対象: `QPDFParser` の object parse、`QPDF` の xref 読み取り（classic / stream / hybrid）と
`reconstruct_xref` 回復、`readObjectAtOffset` / `readStream` の damaged-object 境界、
`QPDF::warn` → `m->warnings` の単一 sink と `QPDFExc` / `QPDFLogger` の文言・順序契約。
flpdf 側は `parser.rs`、`xref.rs`、`reader/file_object.rs`、`reader/resolver.rs` の recovery 部、
`diagnostics.rs`、`logger.rs`、`error.rs`、`tokenizer.rs`。

## qpdf 責務モデル

qpdf 11.9.0 pinned source。以下の行範囲はすべて `rg -n` / `sed -n` / `awk` で実際に読んだもの。

### state（`include/qpdf/QPDF.hh:1440-1485`）

本領域が触る `m->` フィールドは次の 1 組で、すべて **1 つの `QPDF` インスタンスに 1 つ**:

| フィールド | 宣言 | 役割 |
|---|---|---|
| `log` | `include/qpdf/QPDF.hh:1453` | warning 文字列の**表示先**。sink 本体ではない |
| `tokenizer` | `include/qpdf/QPDF.hh:1455` | file / ObjStm / trailer で共有される唯一の tokenizer |
| `file` | `include/qpdf/QPDF.hh:1456` | `getName()` / `getLastOffset()` が `QPDFExc` の filename / offset の既定値源 |
| `last_object_description` | `include/qpdf/QPDF.hh:1457` | `damagedPDF` の `object` 既定値（`setLastObjectDescription`, `libqpdf/QPDF.cc:1298-1310`） |
| `ignore_xref_streams` / `suppress_warnings` / `attempt_recovery` | `include/qpdf/QPDF.hh:1459-1461` | `/XRefStm` 無視 / warn の**表示**抑止（sink への push は止めない）/ 回復分岐の有効化 |
| `xref_table` | `include/qpdf/QPDF.hh:1465` | classic / stream / reconstructed のいずれも **同じ 1 つの表** に入る |
| `deleted_objects` | `include/qpdf/QPDF.hh:1466` | 通常読みでは free entry の集合。reconstruct 中は「回復開始時に xref から消した uncompressed object の集合」に意味が変わる（`libqpdf/QPDF.cc:1204-1206` のコメント） |
| `obj_cache` / `resolving` / `resolved_object_streams` | `include/qpdf/QPDF.hh:1467-1468,1485` | 領域 A と共有。本領域では `resolve` の loop guard と ObjStm 一括 cache に使う |
| `trailer` | `include/qpdf/QPDF.hh:1469` | `setTrailer` は **最初の 1 回だけ** 採用（`libqpdf/QPDF.cc:507-513`） |
| `warnings` | `include/qpdf/QPDF.hh:1475` | **唯一の warning sink**。順序 = `warn` 呼び出し順 |
| `reconstructed_xref` / `fixed_dangling_refs` / `in_parse` / `parsed` | `include/qpdf/QPDF.hh:1480-1484` | 回復済み（再入禁止フラグ） / dangling 再修正フラグ / 再入 parse 検出 / parse 完了 |

**回復予算は `bool` 1 つで表現される**: `reconstructed_xref` が true なら `reconstruct_xref` は
渡された例外をそのまま re-throw する（`libqpdf/QPDF.cc:518-522`）。qpdf に「残り回復回数」の
カウンタは存在しない。

### call order

1. **`QPDF::parse(password)`**（`libqpdf/QPDF.cc:424-473`）
   - `%PDF-` を先頭 1024 byte から探し、無ければ `warn(damagedPDF("", 0, "can't find PDF header"))` して
     `pdf_version = "1.2"`（`libqpdf/QPDF.cc:430-437`）。**throw しない**。
   - 末尾 1054 byte から `startxref` を `findLast` し、`readToken` で offset を読む（`libqpdf/QPDF.cc:439-448`）。
   - `xref_offset == 0` なら `throw damagedPDF("", 0, "can't find startxref")`。内側 try が
     `read_xref` を包み、`QPDFExc` は素通し、その他 `std::exception` は
     `damagedPDF("", 0, "error reading xref: " + e.what())` に**包み直して** throw。外側 catch が
     `attempt_recovery` なら `reconstruct_xref(e)`、でなければ re-throw（`libqpdf/QPDF.cc:450-469`）。
   - `initializeEncryption(); m->parsed = true;`（`libqpdf/QPDF.cc:471-472`）。
   - **`attempt_recovery` の分岐は全ソースで 3 箇所だけ**: `libqpdf/QPDF.cc:463`（parse 直下）、
     `:1391`（`readStream`）、`:1563`（`readObjectAtOffset`）。setter は `:336`。
2. **`QPDF::read_xref(xref_offset)`**（`libqpdf/QPDF.cc:626-719`）— `while (xref_offset)` で
   `visited` に積みながら:
   - offset 直後の空白を読み飛ばし（`skipped_space`）、7 byte 読んで `"xref"` + 空白なら
     `read_xrefTable(xref_offset + skip)`、そうでなければ `read_xrefStream(xref_offset)`
     （`libqpdf/QPDF.cc:638-677`）。`skipped_space` なら
     `warn("extraneous whitespace seen before xref")`（`libqpdf/QPDF.cc:662`）。
   - 戻り値（`/Prev`）が `visited` にあれば
     `throw damagedPDF("", 0, "loop detected following xref tables")`（`libqpdf/QPDF.cc:679-683`）。
   - ループ後: `trailer` 未初期化なら
     `throw "unable to find trailer while reading xref"`（`libqpdf/QPDF.cc:686-688`）。
     `/Size` と `max(xref_table.rbegin, deleted_objects.rbegin)+1` の不一致は **warn**
     （`libqpdf/QPDF.cc:689-704`）。`deleted_objects.clear()`（`libqpdf/QPDF.cc:708`）。
     同 obj 番号の複数 generation は最高 gen のみ残し `removeObject`（`libqpdf/QPDF.cc:710-718`）。
3. **`QPDF::read_xrefTable(xref_offset)`**（`libqpdf/QPDF.cc:846-946`, classic）
   - 50 byte の `linebuf` を `parse_xrefFirst`（`libqpdf/QPDF.cc:722`）で解析、失敗は
     `throw damagedPDF("xref table", "xref syntax invalid")`（`libqpdf/QPDF.cc:862`）。
   - 各 entry は `readLine(30)` → `parse_xrefEntry`（`libqpdf/QPDF.cc:770`）、失敗は
     `throw "invalid xref entry (obj=N)"`（`libqpdf/QPDF.cc:876-879`）。`'f'` は `deleted_items` に
     **保留**、`'n'` は `insertXrefEntry(i, 1, f1, f2)`（`libqpdf/QPDF.cc:880-885`）。
   - `trailer` キーワードまで subsection を繰り返し、`readTrailer()` が dictionary でなければ
     `throw "expected trailer dictionary"`（`libqpdf/QPDF.cc:894-900`）。
   - 最初の trailer のみ `setTrailer`; `/Size` 欠落 / 非整数は **throw**（`libqpdf/QPDF.cc:902-913`）。
   - **hybrid**: `cur_trailer` に `/XRefStm` があり `ignore_xref_streams` でなければ
     `(void)read_xrefStream(/XRefStm)` を呼び、**戻り値は捨てて** trailer の `/Prev` を使う。
     非整数なら `throw "invalid /XRefStm"`（`libqpdf/QPDF.cc:915-927`）。
   - **その後で** 保留していた `deleted_items` を `insertFreeXrefEntry`（`libqpdf/QPDF.cc:929-932`）—
     XRefStm 側の entry が free 化に勝つための順序。
   - `/Prev` 非整数は throw、無ければ 0 を返す（`libqpdf/QPDF.cc:934-945`）。
4. **`QPDF::read_xrefStream(xref_offset)`**（`libqpdf/QPDF.cc:949-969`, stream）
   - `readObjectAtOffset(false, xref_offset, "xref stream", QPDFObjGen(0, 0), x_og, true)` を
     `catch (QPDFExc&)` で**握りつぶし**（`libqpdf/QPDF.cc:954-959`）、`isStreamOfType("/XRef")` なら
     `processXRefStream`、そうでなければ `throw damagedPDF("", xref_offset, "xref not found")`
     （`libqpdf/QPDF.cc:960-967`）。
5. **`QPDF::processXRefStream(xref_offset, xref_obj)`**（`libqpdf/QPDF.cc:972-1146`）
   - `/W`（3 要素以上の整数配列）/ `/Size` 整数 / `/Index` 配列か null の検証、`W[i] > sizeof(qpdf_offset_t)`、
     `entry_size == 0`、`/Index` 要素数（偶数・2 以上）、非整数、entry 数 overflow は
     いずれも **throw damagedPDF("xref stream", xref_offset, ...)**（`libqpdf/QPDF.cc:977-1045`）。
   - `getStreamData(qpdf_dl_specialized)` の実サイズが期待より **小さければ throw、大きければ warn**
     （`libqpdf/QPDF.cc:1051-1065`）。
   - entry ループ: `W[0] == 0` は type 1 既定（`libqpdf/QPDF.cc:1082-1085`）、`/Index` chunk 越境の
     整数 overflow は `throw std::range_error`（`libqpdf/QPDF.cc:1096-1102`。`parse` の
     `catch (std::exception&)` で `"error reading xref: "` に包まれる）、type 0 は
     `insertFreeXrefEntry(QPDFObjGen(obj, 0))`（f2 は無視、`libqpdf/QPDF.cc:1121-1124`）、
     それ以外は `insertXrefEntry`（`libqpdf/QPDF.cc:1126`）。
   - trailer 未初期化なら `setTrailer(dict)`、`/Prev` 非整数は throw（`libqpdf/QPDF.cc:1130-1143`）。
6. **xref entry 挿入の 3 primitive**（3 つとも同じ `m->xref_table` に書くが、上書き規則が違う）
   - `insertXrefEntry(obj, f0, f1, f2)`（`libqpdf/QPDF.cc:1149-1184`）: `deleted_objects` にあれば無視、
     `try_emplace` で **first-seen wins**（新しい update を先に読む前提）、type 1/2 以外は
     `throw "unknown xref stream entry type"`。
   - `insertFreeXrefEntry(og)`（`libqpdf/QPDF.cc:1187-1192`）: `xref_table` に無いときだけ
     `deleted_objects.insert`。
   - `insertReconstructedXrefEntry(obj, f1, f2)`（`libqpdf/QPDF.cc:1197-1210`）:
     `obj > 0 && 0 <= f2 < 65535` を満たさなければ無視、`deleted_objects` に無ければ **上書き**
     （`xref_table[og] = QPDFXRefEntry(f1)`）。reconstruct 専用の「後勝ち」primitive。
7. **`QPDF::reconstruct_xref(QPDFExc& e)`**（`libqpdf/QPDF.cc:516-623`）
   - `m->reconstructed_xref` が既に true なら **`throw e`**（再帰回復の禁止、`libqpdf/QPDF.cc:518-522`）。
     `reconstructed_xref = true; fixed_dangling_refs = false;`（`libqpdf/QPDF.cc:524-526`）。
   - warn を **この順で 3 回**: `"file is damaged"` → 引数 `e` → `"Attempting to reconstruct
     cross-reference table"`（`libqpdf/QPDF.cc:528-530`）。
   - `xref_table` から **type 1 のみ** を全削除（`libqpdf/QPDF.cc:532-541`）。type 2 entry は残る。
   - 先頭から EOF まで `findAndSkipNextEOL` で行を刻み、行頭で `readToken(m->file, MAX_LEN=100)`。
     `token_start >= next_line_start` なら次行へ持ち越し、`int int obj` なら
     `insertReconstructedXrefEntry(obj, token_start, gen)`、trailer 未初期化かつ `trailer` word なら
     `readTrailer()` → dictionary なら `setTrailer`（`libqpdf/QPDF.cc:543-574`）。
     `deleted_objects.clear()`（`libqpdf/QPDF.cc:575`）。
   - trailer が無ければ、`xref_table` の type 1 のうち `isStreamOfType("/XRef")`（例外は `continue`）で
     **最大 offset** の stream の dict を `setTrailer` し、`read_xref(max_offset)` を呼ぶ。失敗は
     `throw "error decoding candidate xref stream while recovering damaged file"`
     （`libqpdf/QPDF.cc:577-608`）。
   - それでも無ければ
     `throw "unable to find trailer dictionary while recovering damaged file"`（`libqpdf/QPDF.cc:610-616`）。
   - **ObjStm 内部は意図的に走査しない**: `libqpdf/QPDF.cc:618-622` のコメント「We could iterate through
     the objects looking for streams and try to find objects inside of them, but it's probably not worth
     the trouble. ... If we wanted to do anything that involved looking at stream contents, we'd also
     have to call initializeEncryption() here.」。**回復後の `xref_table` に type 2 entry が残るのは
     削除対象が type 1 だけ（`libqpdf/QPDF.cc:532-541`）だからであって、ObjStm を decode して
     compressed entry を復元する処理は qpdf に存在しない。**（`libqpdf/QPDF.cc:611-613` は別件で、
     「最後に読んだ object が xref stream なら trailer を取れるかも」という未実装メモ。）
8. **object 読み取り**
   - `QPDF::resolve(og)`（`libqpdf/QPDF.cc:1700-1753`）: `m->resolving` に居れば
     `warn("loop detected resolving object N G")` + null cache（`libqpdf/QPDF.cc:1706-1713`）。
     type 1 → `readObjectAtOffset(true, offset, "", og, a_og, false)`、type 2 →
     `resolveObjectsInStream`、他 → `throw "has unexpected xref entry type"`
     （`libqpdf/QPDF.cc:1716-1736`）。**`catch (QPDFExc& e) { warn(e); }` /
     `catch (std::exception& e) { warn(damagedPDF("", 0, "object N/G: error reading object: " +
     e.what())); }`**（`libqpdf/QPDF.cc:1737-1742`）— **resolve 境界で例外は必ず warning に降格**し、
     未解決なら null（`libqpdf/QPDF.cc:1745-1749`）。
   - `QPDF::readObjectAtOffset(try_recovery, offset, description, exp_og, og, skip_cache_if_in_xref)`
     （`libqpdf/QPDF.cc:1541-1697`）: `exp_og.getObj() == 0` なら `check_og = try_recovery = false`
     （`libqpdf/QPDF.cc:1550-1560`）; `!attempt_recovery` なら `try_recovery = false`
     （`libqpdf/QPDF.cc:1563-1565`）; `offset == 0` は `warn(damagedPDF(0, "object has offset 0"))` +
     null（`libqpdf/QPDF.cc:1571-1575`）; `N G obj` 3 token 検査、失敗は `throw "expected n n obj"`、
     `objid == 0` は `throw "object with ID 0"`、`exp_og != og` は `try_recovery` なら throw、
     でなければ warn して続行（`libqpdf/QPDF.cc:1591-1613`）;
     `catch (QPDFExc& e)` で `try_recovery` なら `reconstruct_xref(e)` → `xref_table[exp_og]` が type 1 なら
     **`readObjectAtOffset(false, new_offset, ...)` で 1 回だけ再試行**、無ければ
     `warn("object N G not found in file after regenerating cross reference table")` + null
     （`libqpdf/QPDF.cc:1614-1637`）; `readObject` 後、endobj 後の空白を読み飛ばし EOF なら
     `throw "EOF after endobj"`（`libqpdf/QPDF.cc:1649-1662`）; `skip_cache_if_in_xref &&
     xref_table.count(og)` なら cache しない（`libqpdf/QPDF.cc:1664-1693`）。
   - `QPDF::readObject(description, og)`（`libqpdf/QPDF.cc:1330-1357`）: `setLastObjectDescription` →
     `QPDFParser(...).parse(empty, false)` → `empty` なら `warn("empty object treated as null")` して
     **即 return**（`libqpdf/QPDF.cc:1341-1346`）→ dictionary + `stream` token なら `readStream` →
     `endobj` でなければ **warn** `"expected endobj"`（`libqpdf/QPDF.cc:1347-1355`）。
   - `QPDF::readStream(object, og, offset)`（`libqpdf/QPDF.cc:1360-1399`）: `validateStreamLineEnd`
     （`libqpdf/QPDF.cc:1401-1449`。CR のみ / 不正終端 / 余分空白の 3 種を **warn**）→ `/Length` 欠落は
     `throw "stream dictionary lacks /Length key"`、非整数は
     `throw "/Length key in stream dictionary is not an integer"`、`endstream` 不在は
     `throw "expected endstream"`（`libqpdf/QPDF.cc:1370-1389`）→
     `catch (QPDFExc& e) { if (m->attempt_recovery) { warn(e); length = recoverStreamLength(...); }
     else throw; }`（`libqpdf/QPDF.cc:1390-1397`）。
   - `QPDF::recoverStreamLength(input, og, stream_offset)`（`libqpdf/QPDF.cc:1481-1533`）:
     `warn("attempting to recover stream length")` → `findFirst("end", ...)` +
     `findEndstream`（`libqpdf/QPDF.cc:1469-1479`）で `endstream` / `endobj` を探し、`xref_table` の
     次 type-1 offset で「この object 内か」を検査（結果は QTC のみで挙動を変えない、
     `libqpdf/QPDF.cc:1499-1521`）→ `length == 0` なら
     `warn("unable to recover stream data; treating stream as empty")`、それ以外は
     `warn("recovered stream length: N")`（`libqpdf/QPDF.cc:1523-1529`）。
   - `QPDF::resolveObjectsInStream(n)`（`libqpdf/QPDF.cc:1755-1833`）: 既に展開済みなら即 return
     （`libqpdf/QPDF.cc:1758-1761`）; 非 stream は `throw "supposed object stream N is not a stream"`、
     `/Type` 不一致は **warn** `"has wrong type"`、`/N` `/First` 非整数は `throw "has incorrect keys"`、
     header の非整数は `throw "expected integer in object stream header"`
     （`libqpdf/QPDF.cc:1764-1808`）; `last_object_description = "object "` に固定して
     `readObjectInStream`（`libqpdf/QPDF.cc:1451-1467`。`empty` は warn）し、
     **`xref_table[og]` が type 2 かつ同 stream 番号のものだけ cache**（`libqpdf/QPDF.cc:1815-1832`）。
   - `QPDF::readTrailer()`（`libqpdf/QPDF.cc:1312-1328`）: `QPDFParser(m->file, "trailer", ...)`;
     `empty` は `warn("trailer", "empty object treated as null")`、dictionary の後に `stream` があれば
     `warn("stream keyword found in trailer")`; `setLastOffset(offset)` で object 先頭に戻す。
9. **`QPDFParser::parse(empty, content_stream)`**（`libqpdf/QPDFParser.cc:26-127`）/
   **`parseRemainder`**（`libqpdf/QPDFParser.cc:129-377`）
   - `QPDF::ParseGuard pg(context)`（`libqpdf/QPDFParser.cc:34`; `include/qpdf/QPDF.hh:798-817` →
     `QPDF::inParse`, `libqpdf/QPDF.cc:476-485`）で **再入 parse は `std::logic_error`**。
   - `tokenizer.nextToken` が false（`error_message` 非空）なら `warn(tokenizer.getErrorMessage())`
     （`libqpdf/QPDFParser.cc:38-40,141-143`）。
   - top-level: `tt_eof` → `warn("unexpected EOF")` + null、`tt_bad` → null（warn は nextToken 側で済み）、
     brace / `]` / `>>` → warn + null、`tt_word` は `endobj` なら `empty = true` + seek back、
     他は `warn("unknown token while reading object; treating as string")` + String
     （`libqpdf/QPDFParser.cc:42-126`）。
   - remainder: 整数 2 個 + `R` は `context->getObject(id, gen)`（**`context == nullptr` なら
     `throw std::logic_error`**, `libqpdf/QPDFParser.cc:161-165`）; `id < 1 || gen < 0 || gen >= 65535`
     は null（`libqpdf/QPDFParser.cc:166-176`）; `stack.size() > 499` は
     `warn("ignoring excessively deeply nested data structure")` + null
     （`libqpdf/QPDFParser.cc:290-293`）; 不正 token は `tooManyBadTokens`
     （`libqpdf/QPDFParser.cc:472-485`。`good_count <= 4` の間に `bad_count > 5` で
     `warn("too many errors; giving up on reading object")` + null）を経て `addNull`;
     dict 終端で key 未充足は
     `warn("dictionary ended prematurely; using null as value for last key")`
     （`libqpdf/QPDFParser.cc:248-254`）、非 name key は `fixMissingKeys`
     （`libqpdf/QPDFParser.cc:446-470`。`/QPDFFakeN` 挿入 + warn）、重複 key は `warnDuplicateKey`
     （`libqpdf/QPDFParser.cc:500-507`）; `/Type /Sig` + `/ByteRange` + `/Contents` 文字列は
     復号前の生 `contents_string` で差し替え（`libqpdf/QPDFParser.cc:260-265`）。
   - **`QPDFParser` から出る throw は 2 経路**: `libqpdf/QPDFParser.cc:163` の `std::logic_error`
     （context 無しで `R` に遭遇）と、`QPDFParser::warn(QPDFExc const&)`
     （`libqpdf/QPDFParser.cc:487-498`）が `context == nullptr` のとき **warning を throw に昇格**する
     経路。つまり「document なしで parser を使うと、document ありなら warning になる全ての診断が
     例外として出る」のが qpdf の契約であり、構文不正で throw しないのは context がある場合に限る。
10. **warning sink と例外境界**
    - **`QPDF::warn(QPDFExc const& e)`**（`libqpdf/QPDF.cc:487-494`）:
      `m->warnings.push_back(e); if (!m->suppress_warnings) { *m->log->getWarn() << "WARNING: " <<
      m->warnings.back().what() << "\n"; }` — **sink は `m->warnings` の 1 本**で、push は常に起きる。
      `suppress_warnings` は表示だけを止める。`warn(error_code, object, offset, message)`
      （`libqpdf/QPDF.cc:496-504`）は `QPDFExc(error_code, getFilename(), object, offset, message)` を
      作って同じ sink へ。**順序の契約は `warn` の呼び出し順そのもの**（`push_back` 以外に
      並べ替え・重複除去・分類は無い）。
    - **`QPDFParser::warn`**（`libqpdf/QPDFParser.cc:487-498`）: `context` があれば `context->warn(e)`
      で同じ 1 本の sink へ、無ければ throw。`warn(offset, msg)`（`libqpdf/QPDFParser.cc:509-513`）は
      `QPDFExc(qpdf_e_damaged_pdf, input->getName(), object_description, offset, msg)`、
      `warn(msg)`（`libqpdf/QPDFParser.cc:515-519`）は `input->getLastOffset()` を使う。
    - `getWarnings()`（`libqpdf/QPDF.cc:345-352`）は **返して clear**、`anyWarnings` / `numWarnings`
      （`libqpdf/QPDF.cc:353-363`）は clear しない。
    - **`QPDFExc`**（`include/qpdf/QPDFExc.hh:29-77`, `libqpdf/QPDFExc.cc:3-51`）は
      `std::runtime_error` の派生。`createWhat(filename, object, offset, message)`
      （`libqpdf/QPDFExc.cc:18-51`）の文言規則: `filename` があれば先頭;
      `!(object.empty() && offset == 0)` のとき（filename があれば）`" ("` + `object` +
      （`offset > 0` なら `", "`）+ `"offset N"` + `")"`; ここまでが非空なら `": "`; 最後に `message`。
      **`offset == 0` は「offset 無し」として扱われる**（負値も同様に出力されない）。
    - **`QPDF::damagedPDF` 6 overload**（`libqpdf/QPDF.cc:2596-2644`）: error code は常に
      `qpdf_e_damaged_pdf`; filename は `input->getName()` か `m->file->getName()`; object 省略時は
      `m->last_object_description`; offset 省略時は `m->file->getLastOffset()`。
      `stopOnError(message)`（`libqpdf/QPDF.cc:2589-2593`）は `damagedPDF("", message)` を throw。
    - **`std::logic_error` を投げる箇所**（qpdf 側のバグ / API 誤用 = flpdf `Error::Internal`）:
      `QPDF::inParse` 再入（`libqpdf/QPDF.cc:481`）、`showXRefTable` の未知 type
      （`libqpdf/QPDF.cc:1231`）、`QPDFParser` の context 無し `R`（`libqpdf/QPDFParser.cc:163`）、
      `QPDFTokenizer` の内部状態異常（`libqpdf/QPDFTokenizer.cc:241,248`）と `expectInlineImage`
      （`libqpdf/QPDFTokenizer.cc:770`）、`QPDFLogger::setSave` / `throwIfNull`
      （`libqpdf/QPDFLogger.cc:200,252`）。
    - **`std::runtime_error` 系**（= damaged PDF、flpdf `Error::System`）: すべての `damagedPDF` と、
      `processXRefStream` の `std::range_error`（`libqpdf/QPDF.cc:1101`）。
11. **tokenizer の読み取り境界**
    - `QPDF::readToken(input, max_len)`（`libqpdf/QPDF.cc:1535-1539`）は **常に `allow_bad = true`**、
      context = `m->last_object_description`。つまり **`QPDF` 経由の token 読みは bad token で
      throw しない**。
    - `QPDFTokenizer::readToken(input, context, allow_bad, max_len)`
      （`libqpdf/QPDFTokenizer.cc:887-911`）: `nextToken` → `getToken`; `tt_bad` かつ `!allow_bad` なら
      `throw QPDFExc(qpdf_e_damaged_pdf, input.getName(), context, input.getLastOffset(),
      token.getErrorMessage())`。
    - `QPDFTokenizer::nextToken(input, context, max_len)`（`libqpdf/QPDFTokenizer.cc:920-965`）:
      `st_token_ready` まで 1 byte ずつ `handleCharacter`; `max_len` 超過は `tt_bad` +
      `"exceeded allowable length while reading token"`（`libqpdf/QPDFTokenizer.cc:948-954`）;
      戻り値は `error_message.empty()`。**tokenizer 自身は warn しない** — 文言を持つだけで、
      sink へ送るのは `QPDFParser::warn(tokenizer.getErrorMessage())` 側。
12. **`QPDFLogger`**（`libqpdf/QPDFLogger.cc:109-116,218-246,248-255`）: `p_warn` は既定 `nullptr` で
    `getWarn` は `getError`（既定 stderr）へフォールバック; `setOutputStreams` は
    `p_warn = nullptr` に戻す（`libqpdf/QPDFLogger.cc:244`）。**logger は sink ではない** —
    `m->warnings` に積まれるかどうかに logger は一切関与しない。

### error / warning boundary（要約）

| 層 | throw | warn（`m->warnings` へ push） |
|---|---|---|
| `parse` | startxref 無し / `read_xref` の全 throw を `attempt_recovery` で `reconstruct_xref` に渡す。回復不能なら呼び出し元へ | header 無し |
| `read_xref*` / `processXRefStream` | 構文・`/Size`・`/Prev`・`/XRefStm`・`/W`・`/Index`・loop・trailer 不在 | 余分空白、`/Size` 不一致、stream data 過大 |
| `reconstruct_xref` | 再入（引数 `e` を re-throw）、候補 xref stream decode 失敗、trailer 不在 | 3 連 warn（順序固定） |
| `resolve` | **無し**（すべて warn に降格） | loop、下位の全 `QPDFExc`、`std::exception` |
| `readObjectAtOffset` | `try_recovery == false` のときの `n n obj` 不一致 / ID 0 / EOF after endobj | offset 0、回復後の再試行失敗、`check_og` 不一致（非回復時） |
| `readObject` / `readStream` | `/Length` 不正・`endstream` 不在（`attempt_recovery == false` 時のみ外へ） | empty object、`expected endobj`、line-end 3 種、回復時は `readStream` の throw も warn 化、`recoverStreamLength` 2 種 |
| `resolveObjectsInStream` | 非 stream、keys 不正、header 不正 | `/Type` 不一致、empty object |
| `QPDFParser` | context 無し `R` の `std::logic_error`、および context 無しのとき全 warning を `QPDFExc` として昇格 | context 有りのときは上記 9 のすべて |
| `QPDFTokenizer` | `allow_bad == false` の `readToken` のみ（`QPDF` 経由は常に `allow_bad = true`） | 自身は warn しない（文言のみ提供） |

## route matrix

2026-09-06再監査では日付を明記した行とprobeの現状を更新した。
全行のcaller数を再集計したものではなく、未更新行の数値は元調査時点の値である。

caller の数え方（領域 D と同じ規約。全行がこれに従う）: `rg -n '<pattern>' crates --glob '*.rs'`
の出力から、まず **test** を分ける — `tests/` / `benches/` / `fuzz/` 配下、`#[cfg(test)]` が付いた
モジュールまたは個別項目の中身。残りのうち、次は **数えない**: コメントのみの行、宣言行
（`fn` / `struct` / `enum` / `trait` / `type` / `const` / `static` / `mod` / `union` と `impl <Type>` ヘッダ）、
`use` 行（複数行の継続を含む）、文字列リテラル内だけの出現。数えるのは **呼び出し箇所と
型位置の参照** のみ。`prod` の括弧内はファイル名。flpdf の宣言可視性は entrypoint 列に併記する。

| # | qpdf responsibility owner | qpdf evidence | flpdf current entrypoint | callers (prod / test) | classification | canonical owner | remaining bridge callers / notes |
|---|---|---|---|---|---|---|---|
| B1 | `QPDFParser::parse` / `QPDFParser::parseRemainder`（file object 本体の逐次 parse） | `libqpdf/QPDFParser.cc:26-127,129-377` | `crates/flpdf/src/parser.rs::parse_live_file_object_with_context`（private。実体は `crates/flpdf/src/parser.rs::LiveFileParser`） | prod: 4 (parser.rs) / test: 0 | canonical | `crates/flpdf/src/parser.rs::parse_live_file_object_with_context` | 公開ドア 4 本（`parse_live_file_object` / `parse_live_file_object_with_decrypter` / `parse_explicit_object_handle_with_description` / `parse_object_handle_with_context`, `crates/flpdf/src/parser.rs:259-322`）はすべて薄い委譲で、file object の parse ロジックは 1 本。`has_context: bool` が qpdf の `context == nullptr` に対応 |
| B2 | 同上を `content_stream = true` で呼ぶ経路（content stream の parse） | `libqpdf/QPDFParser.cc:27,44-47,99-100,193-196,313-320,338-348` — qpdf は **同じ `QPDFParser`** を bool 1 個で切り替える | `crates/flpdf/src/parser.rs::parse_live_content_stream_object`（`pub(crate)`。`crates/flpdf/src/content_stream.rs::parse_content_stream_handles_internal` から使う） | prod: 1 (content_stream.rs) / test: 0 | canonical | `crates/flpdf/src/parser.rs::parse_live_file_object_with_context` | **2026-09-08（`flpdf-3yn9.48.16` で解消）**: 構造的に別だった再帰下降 parser（`ContentHandleParser`、旧 `parser.rs:1809-2131`）を撤去し、`LiveFileParser` 自身に `content_stream: bool` フィールドを追加して qpdf の `content_stream` 引数を1:1で再現した（EOF ×2箇所・bare word ×1箇所・整数の ref-lookahead 抑止 ×1箇所の4分岐のみが content_stream で分岐し、それ以外は完全共有）。`parse_live_content_stream_object`（新規 thin wrapper）が唯一のconsumer。`ContentHandleResolver`（旧・ad hoc な `direct`/`direct_at` メソッド）は `HandleResolver` trait の正規実装に変換し、`indirect_handle` は content stream が参照を構築しないため `unreachable!()`。統合時に qpdf の `fixMissingKeys`（`QPDFParser.cc:447-469`）の `names` 衝突回避チェックが `ContentHandleParser::finish_content_dictionary` に欠落していた真の regression を発見し修正（`LiveFileParser::finish_dictionary` の既存 `orphan_names` チェックを流用）。同様に 500-container 制限超過が旧実装ではハード `Error`（"object nesting too deep"）だったが、qpdf の実際の契約（`QPDFParser.cc:311-318`、warning + null recovery、非エラー）に合わせて修正（`crates/flpdf/tests/object_handle_content_parser_tests.rs::parse_page_contents_reports_non_bad_token_diagnostics_and_nesting_limit` が RED→GREEN で固定）。contextless route の `Error::System` 昇格 / document-owned route の `DocumentResolver` sink 送達（`content_stream.rs::deliver_diagnostic`）は変更なしで維持。 |
| B3 | `QPDFParser::warn`（`context == nullptr` のとき warning を `QPDFExc` として throw に昇格） | `libqpdf/QPDFParser.cc:487-498`（`:496` が throw）。`libqpdf/QPDFObjectHandle.cc:1672-1698` から context 無しで呼ばれる | `crates/flpdf/src/parser.rs::parse_explicit_object_handle`（`pub(crate)`。`has_context = false` で `parse_live_file_object_with_context` を呼ぶ、`crates/flpdf/src/parser.rs:287-311`） | prod: 1 (object_handle.rs) / test: 0 | canonical | `crates/flpdf/src/parser.rs::parse_live_file_object_with_context` | B1 と同じ 1 実装の分岐なので経路は 1 本。qpdf の `R` に対する `std::logic_error`（`libqpdf/QPDFParser.cc:161-165`）と、warning 昇格（`:496`）の 2 種の throw を両方この分岐で扱えているかは B32 の error 分類と合わせて確認が要る |
| B4 | `QPDFParser::tooManyBadTokens`（`good_count <= 4` かつ `bad_count > 5` で打ち切り） | `libqpdf/QPDFParser.cc:472-485` | `crates/flpdf/src/parser.rs::too_many_bad_tokens`（private、`LiveFileParser` の `good_count`/`bad_count` フィールド） | prod: 6 (parser.rs) / test: 0 | canonical | `crates/flpdf/src/parser.rs::too_many_bad_tokens` | **2026-09-08（`flpdf-3yn9.48.16` で解消）**: B2 の帰結。content stream 用の独立カウンタ（`content_good_count`/`content_bad_count`/`content_give_up`）を撤去し、`LiveFileParser` 1 個の `good_count`/`bad_count` を file/content stream 両方の parse で共有する qpdf のメンバー構造（`libqpdf/QPDFParser.cc:137,144`）に一致させた。同じ入力なら打ち切り位置が経路間で必ず一致する |
| B5 | `QPDF::inParse` / `QPDF::ParseGuard`（parse再入をlogic_errorで拒否） | `libqpdf/QPDF.cc:476-485`, `include/qpdf/QPDF.hh:798-817`, `libqpdf/QPDFParser.cc:29-34` | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::in_parse`（`pub(crate)`。`crate::parser::HandleResolver::begin_parse`/`end_parse` の default no-op が contextless 側、document-owned 側は `ChildHandles`（`reader/resolver.rs:4535`）と `ResolverHandle as CanonicalTrailerOwner`（`xref.rs:89-97`）が override し、`OffsetHandleResolver`（`parser.rs:1934-1945`）と `CanonicalTrailerParser`（`xref.rs:953-963`）が forward する） | `in_parse(` prod: 4 — `reader/resolver.rs:4536,4546`（`ChildHandles`）と `xref.rs:90,96`（`CanonicalTrailerOwner`）/ test: 4。`begin_parse(`/`end_parse(` の prod 呼出しは各 3 — `parser.rs:424,426`（`LiveFileParser::parse` 本体の bracket）、`parser.rs:1940,1944`（ObjStm member を通す rebasing adapter）、`xref.rs:958,962`（classic trailer） / test: 3, 2 | canonical | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::in_parse` | qpdf が context を渡す 3 経路 — `readObject`（`QPDF.cc:1339`）、`readObjectInStream`（`:1459`）、`readTrailer`（`:1317`）— のすべてで guard が有効。bootstrap 経路は `BootstrapHandleParser` が `LiveFileParser::parse` を通らないため未到達で、段階移行として残る。content stream（`QPDFObjectHandle.cc:1812`）は `flpdf-3yn9.48.16`（2026-09-08）で `parse_live_content_stream_object` 経由の `LiveFileParser::parse` を通るようになり、`begin_parse`/`end_parse` 自体は呼ばれる（`parser.rs::LiveFileParser::parse`）。ただし content stream 用の `HandleResolver`（`ContentHandleResolver`）は default no-op 実装のままで、`DocumentResolver` trait に `in_parse` 相当のフックが無いため、実際の document の再入フラグへは forward しない——「未到達」ではなく「到達するが no-op」という、より狭い残課題に変わった。 |
| B6 | `QPDFTokenizer::readToken(input, context, allow_bad, max_len)` / `nextToken`（bad token の文言決定と `max_len` 打ち切り） | `libqpdf/QPDFTokenizer.cc:887-911,920-965` | `crates/flpdf/src/tokenizer.rs::read_token`（`pub`） | prod: 16 (tokenizer.rs, parser.rs, xref.rs, content_stream.rs, resolver.rs, pipeline/qpdf_tokenizer.rs, flpdf-qtest-tools/tokenizer_runner.rs) / test: 0 | canonical | `crates/flpdf/src/tokenizer.rs::read_token` | 実装は 1 本。`allow_bad` / `max_len` の 2 引数も qpdf と 1:1 |
| B7 | `QPDF::readToken`（`allow_bad = true` 固定） | `libqpdf/QPDF.cc:1535-1539,1801-1814,846-946` | `crates/flpdf/src/xref.rs::read_trailer`（trailer dictionary後のlookahead） / `Tokenizer::next_object_stream_integer`（ObjStm） / `ByteCursor::read_token`（classic xref） | `next_object_stream_integer` prod: 4（canonical resolver / bootstrap xref）・test: 1。`read_trailer` と ObjStm は qpdf の `readToken` 契約を局所的に使い、classic xref は別責務として残す | mixed | `crates/flpdf/src/xref.rs::read_trailer`（trailer lookahead） | **2026-09-08（`flpdf-3yn9.48.14`）**: ObjStm headerの両consumerを専用 `next_object_stream_integer`（内部 `read_token(true, 0)` → integer検査）へ移行した。汎用 `next_integer` と classic xref の `ByteCursor::read_token(false)` は変更していない。 |
| B8 | `QPDF::readObjectAtOffset`（`N G obj` 検査・offset 0・回復 1 回・cache 登録） | `libqpdf/QPDF.cc:1541-1697` | `crates/flpdf/src/reader/resolver.rs::read_object_at_offset_with_description`（private） / bootstrap 側は `crates/flpdf/src/xref.rs::read_uncompressed_object`（private） | prod: 3 (resolver.rs) + 1 (xref.rs) / test: 1 + 4 | mixed | `crates/flpdf/src/reader/resolver.rs::read_object_at_offset_with_description` | xref bootstrap（`Pdf` 構築前）と canonical resolver（構築後）で別実装。qpdf は `read_xrefStream` からも `resolve` からも同じ `readObjectAtOffset` を呼ぶ（`libqpdf/QPDF.cc:956`, `:1725`）。残 caller（bridge 側）: `crates/flpdf/src/xref.rs:915`（`read_uncompressed_object` 呼び出し）。`crates/flpdf/src/xref.rs:576-721` は `qpdf-deviation-start/end` でマーク済み（reconstruction-only の read window bound） |
| B9 | `QPDF::readObject`（`endobj` framing・`empty object treated as null`・`expected endobj` warn） | `libqpdf/QPDF.cc:1330-1357` | `crates/flpdf/src/reader/file_object.rs::parse_file_object_handle_syntax` + `crates/flpdf/src/reader/file_object.rs::finish_file_object_handle`（どちらも `pub(crate)`） | `parse_file_object_handle_syntax(` prod: 2 (reader.rs, xref.rs) / test: 2。`finish_file_object_handle(` prod: 1 (xref.rs) / test: 2 | mixed | `crates/flpdf/src/reader/file_object.rs::parse_file_object_handle_syntax` | 2 段（syntax → finish）に割った framing 自体は 1 実装だが、`crates/flpdf/src/reader.rs:2168` が legacy 経路から直接呼び、`crates/flpdf/src/xref.rs:537-545` が bootstrap から呼ぶ。canonical resolver は `read_object_at_offset_with_description`（B8）内で別に framing する。残 bridge caller: `crates/flpdf/src/reader.rs:2168`, `crates/flpdf/src/xref.rs:537`, `crates/flpdf/src/xref.rs:545` |
| B10 | `QPDF::readStream` + `QPDF::validateStreamLineEnd`（`/Length` 検査・`endstream` 検査・行末 3 種の warn） | `libqpdf/QPDF.cc:1360-1399,1401-1449` | `crates/flpdf/src/reader/resolver.rs::validate_stream_line_end`（private） / bootstrap・legacy 側は `crates/flpdf/src/reader/file_object.rs::finish_file_object_handle` | `validate_stream_line_end(` prod: 1 (resolver.rs) / test: 0。`finish_file_object_handle(` prod: 1 (xref.rs) / test: 2 | mixed | `crates/flpdf/src/reader/resolver.rs::validate_stream_line_end` | qpdf の 3 warning 文言（`stream keyword followed by carriage return only` / `not followed by proper line terminator` / `followed by extraneous whitespace`）は `rg -l` で 2 / 2 / 1 ファイルにまたがって現れ、resolver 側と file_object 側の両方が持つ。qpdf は 1 関数 |
| B11 | `QPDF::recoverStreamLength`（`attempting to recover stream length` → `endstream`/`endobj` 探索 → 2 種の結果 warn） | `libqpdf/QPDF.cc:1469-1479,1481-1533` | `crates/flpdf/src/reader/resolver.rs::recover_stream_length`（private） / `crates/flpdf/src/reader/file_object.rs::recover_stream_boundary`（private） | `recover_stream_length(` prod: 2 (resolver.rs) / test: 2。`recover_stream_boundary(` prod: 1 (file_object.rs) / test: 0 | mixed | `crates/flpdf/src/reader/resolver.rs::recover_stream_length` | 2 実装。`file_object.rs` 側は `RecoveryPolicy`（`crates/flpdf/src/reader/file_object.rs:11-16`）という qpdf に対応物のない enum で `RequireEndstream` / `Bounded` を切り替え、qpdf の「`attempt_recovery` の有無」1 bit（`libqpdf/QPDF.cc:1391`）を 2 値の別概念に置き換えている。残 bridge caller: `crates/flpdf/src/reader/file_object.rs:377` |
| B12 | `QPDF::resolveObjectsInStream`（ObjStm 展開・`/N` `/First` 検査・`has wrong type` warn・xref 一致メンバーのみ cache） | `libqpdf/QPDF.cc:1755-1833`, `libqpdf/QPDF.cc:1451-1467` | `crates/flpdf/src/reader/resolver.rs::resolve_object_stream_with_failure_kind`（private） / `crates/flpdf/src/xref.rs::resolve_objects_in_stream`（`BootstrapHandleDocument` の private メソッド） | `resolve_object_stream_with_failure_kind(` prod: 1 (resolver.rs) / test: 1。`resolve_objects_in_stream(` prod: 1 (xref.rs) / test: 2 | mixed | `crates/flpdf/src/reader/resolver.rs::resolve_object_stream_with_failure_kind` | qpdf は `m->resolved_object_streams` を 1 つ持つ（`include/qpdf/QPDF.hh:1485`）。flpdf は bootstrap 用に `crates/flpdf/src/xref.rs::BootstrapHandleState`（`resolving` / `resolved_object_streams` / `diagnostics` / `handles` を持つ、`crates/flpdf/src/xref.rs:67-73`）という **並行する第 2 の document 状態** を持つ。**2026-09-08（`.48.14`）**: bootstrap側も specialized `get_stream_data`、direct member parser、headerの最後勝ち、effective xref/default-free、親streamのend offsets、ObjStm descriptionをcanonical/qpdf順へ揃えた。owner-less reconstructionのbounded read windowは保持し、全loaderをlive canonical ownerへ差し替える変更は回帰（bounded-read性能・warning/state）になるため採用していない。残るbridgeはBootstrapHandleStateの統合・rebind/replayで、`.48.15` の後続範囲。 |
| B13 | `QPDF::readTrailer`（trailer 位置の復元・`empty object` warn・`stream keyword found in trailer` warn） | `libqpdf/QPDF.cc:1312-1328` | `crates/flpdf/src/xref.rs::read_trailer`（classic と reconstruct candidate の共通 private helper） | prod: 3 (xref.rs) / test: 0 | canonical | `crates/flpdf/src/xref.rs::read_trailer` | 2026-09-08: classic owner有り／無しの両経路と `parse_trailer_candidate` が同じ handle-producing parser、parser diagnostics、empty warning、dictionary後の `read_token(true, 0)` lookaheadを通る。`trailer_warning` が qpdf と同じ object attributionを付け、`trailer_start` が復元された logical last-offsetとして `/Size`・`/Prev` validationへ渡る。live probeで `stream keyword found in trailer` と empty candidateの warning detail/offsetをqpdf 11.9.0と照合した。 |
| B14 | `QPDF::read_xref` の `while (xref_offset)` ループ（`/Prev` 追跡と `visited` による loop 検出） | `libqpdf/QPDF.cc:626-719`（`visited.insert(xref_offset)` が `:631`、検出が `:679-683`） | `crates/flpdf/src/xref.rs::parse_xref_from_start_with_owner` と `merge_previous_xref_sections_with_observer`（private。後者の visitor が初段 offset を seed） | initial/previous/candidate re-entry は `parse_xref_from_start_with_owner` と同じ seeded visitorを使用 | canonical | `crates/flpdf/src/xref.rs::merge_previous_xref_sections_with_observer` | 2026-09-08: 各 `read_xref` 相当の section parse後に `loaded.loaded.startxref`（非zero）を `visited` へ初期登録し、`/Prev` を読む前に初段自己参照を検出する。既存P1の三連 recovery warningはqpdf/flpdfとも一度だけで、二重warningをbugとして扱わない。classicの固定幅entry parseはB15の責務として変更していない。 |
| B15 | `QPDF::read_xrefTable` の subsection / entry 解析（`parse_xrefFirst` / `parse_xrefEntry`、`'f'` の後回し） | `libqpdf/QPDF.cc:722,770,846-893,929-932` | `crates/flpdf/src/xref.rs::parse_xref_table`（private） | prod: 1 (xref.rs) / test: 0 | canonical | `crates/flpdf/src/xref.rs::parse_xref_table` | 呼び出し元は `crates/flpdf/src/xref.rs:1574` の 1 箇所のみ。`'f'` の後回し（`deferred_free`, `crates/flpdf/src/xref.rs:1580-1588,1692-1694`）も qpdf の `deleted_items` 順序（`libqpdf/QPDF.cc:849,880-885,929-932`）と一致 |
| B16 | classic trailer の `/Size` 検証（最初の trailer のみ、`hasKey` → `isInteger` の順） | `libqpdf/QPDF.cc:902-913` | `crates/flpdf/src/xref.rs::validate_classic_trailer`（private） | prod: 1 (xref.rs) / test: 0 | canonical | `crates/flpdf/src/xref.rs::validate_classic_trailer` | 呼び出し元は `crates/flpdf/src/xref.rs:1658` の 1 箇所。`docs/qpdf-correspondence.md:187-197`（Classic xref trailer validation, 2026-09-02）が qpdf の `readTrailer` 位置復元（`libqpdf/QPDF.cc:1312-1328`）まで含めて対応を記録済み。`trailer dictionary lacks /Size key` の文言も flpdf src に実在 |
| B17 | `QPDF::read_xrefStream` + `QPDF::processXRefStream`（`/W` `/Index` `/Size`検証とentry decode） | `libqpdf/QPDF.cc:949-969,972-1146` | `crates/flpdf/src/xref.rs::parse_xref_stream`（private） | 主経路とhybrid `/XRefStm` が同じ関数を呼ぶ。全件数は今回未再集計 | canonical | `crates/flpdf/src/xref.rs::parse_xref_stream` | 経路は1本。旧注記の **`unknown xref stream entry type` がsrcに無いという主張は古い**: 2026-09-06には `crates/flpdf/src/xref.rs:3744` の実装と `:4870` のtest期待が存在し、`flpdf-4sgf` はclosed。`Cross-reference stream does not have proper /W and /Index keys` の文言対応は別途確認する。 |
| B18 | hybrid: classic trailer の `/XRefStm` を読み、**戻り値を捨てて** trailer の `/Prev` を使う | `libqpdf/QPDF.cc:915-927` | `crates/flpdf/src/xref.rs::merge_xref_stream_from_classic_trailer`（private） | prod: 1 (xref.rs) / test: 2 | canonical | `crates/flpdf/src/xref.rs::merge_xref_stream_from_classic_trailer` | 呼び出し元は `crates/flpdf/src/xref.rs:1683` の 1 箇所。`invalid /XRefStm` の文言も実在。`ignore_xref_streams` 相当は `crates/flpdf/src/xref.rs::XrefLoadOptions` が持つ |
| B19 | `QPDF::insertXrefEntry` / `QPDF::insertFreeXrefEntry`（first-seen wins と object-number 単位の tombstone） | `libqpdf/QPDF.cc:1149-1184,1187-1192` | `crates/flpdf/src/xref.rs::XrefRegistration`（private struct。`insert_xref_entry` / `insert_free_xref_entry`） | `insert_xref_entry(` prod: 2 (xref.rs) / test: 19。`insert_free_xref_entry(` prod: 2 (xref.rs) / test: 0 | canonical | `crates/flpdf/src/xref.rs::XrefRegistration` | `crates/flpdf/src/reader/resolver.rs:2596` にも同名 `insert_xref_entry` があるが、`:2595` の `#[cfg(test)]` 配下の fixture 専用で production 経路ではない — 上の規約どおり test 側に数えている（test 19 のうち 1 件）。`crates/flpdf/src/xref.rs:188-196` の doc が `deleted_objects` の lifetime（`libqpdf/QPDF.cc:686-708` vs `:575`）まで書き分けている |
| B20 | `QPDF::insertReconstructedXrefEntry`（reconstruct 専用の後勝ち上書き + `obj > 0 && 0 <= gen < 65535` guard + `deleted_objects` 抑止） | `libqpdf/QPDF.cc:1197-1210` | `crates/flpdf/src/xref.rs::scan_object_header_after_first_token`（private。guard を実装） + `crates/flpdf/src/xref.rs:2428` の `entries.insert`（後勝ち） | prod: 1 (xref.rs) / test: 0 | mixed | `crates/flpdf/src/xref.rs::recover_xref_entries` | guard（`crates/flpdf/src/xref.rs:3056-3059`）と後勝ち（`:2428`）は qpdf と一致。3 つ目の条件 `m->deleted_objects.count(obj)` による抑止（`libqpdf/QPDF.cc:1204-1209`）は `recover_xref_entries` 自身ではなく **呼び出し後の `crates/flpdf/src/xref.rs::merge_recovered_qpdf_state` が `retain` で事後適用** する（`crates/flpdf/src/xref.rs:2344-2354`。コメントが `libqpdf/QPDF.cc:516-575,1194-1210` と scan filter の `:575` clear まで対応づけている）。mixed の理由は **この事後適用が経路ごとに有無が違う** こと: bootstrap の 4 つの handoff のうち `crates/flpdf/src/xref.rs:1403`（pending trigger）/ `:1432`（`/Prev` chain 失敗）/ `:1486`（`/Size` 由来）は merge を通すが、初段 parse 失敗の handoff（`crates/flpdf/src/xref.rs:1368-1383`）は merge を経ず `discard_lower_generations` だけで return する。resolver 経路は `crates/flpdf/src/reader/resolver.rs:1648-1654` のコメントで「qpdf の filter は適用できない」と別途判断済み。残る未決は P2 |
| B21 | `read_xref` 後段: `/Size` 不一致 warn と「同一 object 番号は最高 generation のみ残す」 | `libqpdf/QPDF.cc:689-704,708,710-718` | `crates/flpdf/src/xref.rs::append_xref_size_warning_for`（private） + `crates/flpdf/src/xref.rs::discard_lower_generations`（private） | `append_xref_size_warning_for(` prod: 3 (xref.rs) / test: 0。`discard_lower_generations(` prod: 4 (xref.rs) / test: 0 | canonical | `crates/flpdf/src/xref.rs::append_xref_size_warning_for` | どちらも実装は 1 本で、複数の完了地点（通常 / reconstruct / 候補 xref stream 再入）から同じ関数を呼んでいる。`is not one plus the highest object number` の文言も一致。`crates/flpdf/src/xref.rs:223-233` の doc が `QPDF::removeObject`（`libqpdf/QPDF.cc:710-718`）との対応を記録 |
| B22 | `QPDF::reconstruct_xref`（再入禁止 → 3 連 warn → type 1 削除 → 行スキャン → 候補 xref stream からの trailer 復元 → 失敗時 throw） | `libqpdf/QPDF.cc:516-623` | `crates/flpdf/src/xref.rs::recover_xref_from_linear_scan`（private、open 時） / `crates/flpdf/src/reader/resolver.rs::reconstruct_xref_and_retry`（private、resolve 時） | `recover_xref_from_linear_scan(` prod: 4 (xref.rs) / test: 0。`reconstruct_xref_and_retry(` prod: 1 (resolver.rs) / test: 2 | mixed | `crates/flpdf/src/xref.rs::recover_xref_from_linear_scan` | qpdf は 1 実装を 2 箇所（`libqpdf/QPDF.cc:463` parse 直下、`:1617` `readObjectAtOffset` の再試行）から呼ぶ。flpdf は呼び出し箇所ごとに別実装で、resolve 側は qpdf の後半（候補 `/XRef` stream からの trailer 復元 `libqpdf/QPDF.cc:577-608` と `unable to find trailer dictionary while recovering damaged file` の throw `:610-616`）を実装しない。**2026-09-08 部分解消・probe は継続（P3、`flpdf-3yn9.48.19`）**: 再構築後の entry が compressed だった場合、`reconstruct_xref_and_retry` を `Error::Unsupported` ではなく `Free`/`None` と同じ `Ok(None)`（呼び出し元が `not found in file after regenerating cross reference table` を warn して null 解決）へ統一した。qpdf の retry 条件は `getType() == 1` 限定（`QPDF.cc:1618`）で、compressed を含むそれ以外は全て同じ「見つからない」分岐（`:1628-1633`）に落ち、`resolveObjectsInStream` は試みない — 呼び出し元にあった `Error::Unsupported` 捕捉からの `resolve_object_stream_or_null` への再ルーティング（削除済み、旧 `crates/flpdf/src/reader/resolver.rs:4938-4957`、`cov:ignore`扱いで未検証のまま残っていた）が実際の逸脱だった。**2026-09-08（`flpdf-3yn9.48.69`）**: open 時、canonical owner が候補 discovery/re-entry で実オブジェクトを解決すると `push_qpdf_warning` が即座に live 配送する一方、trio・行スキャンの trailer diagnostics は `LoadedXref.repair_diagnostics` に溜めて `Pdf::open` の最後で配送するため、qpdf の 1 本の `m->warnings` 呼び出し順が live/buffered の 2 チャンネルに分かれ、live 側が先に印字される逆転バグがあった（`crates/flpdf-qtest-tools` 相当の `qpdf --check qpdf/qtest/qpdf/issue-146.pdf` で再現、実 qpdf 11.9.0 の `issue-146.out` と比較し確認）。`ResolverCore::defer_live_repair_diagnostics` と `CanonicalTrailerOwner::begin_deferred_diagnostics`/`end_deferred_diagnostics`（`crates/flpdf/src/reader/resolver.rs`）で、候補 discovery（`find_xref_stream_trailer_candidate_canonical`）と候補 re-entry（`recover_trailer_from_xref_stream_candidate` 内の `parse_xref_from_start_with_owner` 呼び出し）の live 配送を窓の間だけ止め、その間に記録された分を呼び出し順で `repair_diagnostics` へ差し戻す形で解消した（`crates/flpdf/src/xref.rs::DeferredDiagnosticsGuard`）。issue-146.pdf で実 qpdf 11.9.0 の stderr と byte-identical、二重印字なしを確認済み レビュー指摘2件を追って修正: (1) 候補再入で読み取り自体の warning を窓で回収した後、`build_xref_stream` 自身が push する診断（wrong-size 等）を **別 sink** に分離し、読み取り側を先に・build 診断を後に追記する順序へ修正（元は同じ sink を共有し逆順になっていた）。(2) `merge_previous_xref_sections_with_observer` の `/Prev` 走査（reconstruction context のみ）にも同じ `DeferredDiagnosticsGuard` を掛け、候補の `/Prev` 先が live warning を出すケースも回収するよう拡張。(1)(2) は合成 fixture で pre-fix の逆転順を再現してから修正を確認した（3）さらにレビュー指摘を受け、`/Prev` の **offset 解決**（`merge_previous_xref_sections_with_observer` 冒頭と各ホップの `resolve_previous_xref_offset`）も同じ `DeferredDiagnosticsGuard` で包んだ。**ただしこの 3 件目は逆転を再現できていない**: 指摘の合成ケース（間接 `/Prev` の対象が `expected endobj` を出す）では、line scan が対象を先に見つけるため discovery 側で解決・警告が起き、この変更の有無にかかわらず順序は正しい。呼び出しが窓の外にある構造自体は事実なので既存パターンとの対称性を保つ防御的な整合として入れ、追加したテストは修正の証明ではなく不変条件の固定として書いた。 |
| B23 | `reconstruct_xref` の行スキャン本体（行頭から `MAX_LEN = 100` で token を読み `int int obj` を拾う） | `libqpdf/QPDF.cc:543-575` | `crates/flpdf/src/xref.rs::recover_xref_entries`（`pub(crate)`） | prod: 2 (xref.rs, resolver.rs) / test: 1 | canonical | `crates/flpdf/src/xref.rs::recover_xref_entries` | B22 の 2 経路が **スキャン本体だけは共有** している（`crates/flpdf/src/reader/resolver.rs:1655` と `crates/flpdf/src/xref.rs:2228`）。B22 が mixed なのはスキャンの前後（warn 順序・trailer 復元・retry）であって、この primitive ではない |
| B24 | reconstruct 後の ObjStm 内部走査（qpdf は **意図的に行わない**） | `libqpdf/QPDF.cc:618-622`（コメント）+ `:532-541`（削除対象は type 1 のみ） | absent | prod: 0 / test: 0 | canonical | absent | **両側 absent が 1:1 対応そのもの**（`6ddb9661` / `flpdf-hyy2` で削除済み）。bridge にはしない — §3 の bridge は「旧表現と canonical 表現を翻訳する経路」の定義で、ここには翻訳すべき経路が最初から無い。かつて存在した `recover_objstm_compressed_entries` はコミット `6ddb9661`（"fix: align damaged xref recovery with qpdf ObjStm behavior", 2026-08-09）で削除済み。現在 `rg -n 'recover_objstm_compressed_entries' crates` は 0 件で、`crates/flpdf/src/reader/resolver.rs:1648-1654` のコメントが「type 1 行のみ削除」の qpdf 挙動を明示している。回復後に type 2 entry が残るのは削除対象が type 1 だけだから、という理解が両経路のコードに反映されている |
| B25 | `m->reconstructed_xref`（回復予算 = `bool` 1 個。2 回目は例外を re-throw） | `include/qpdf/QPDF.hh:1480`, `libqpdf/QPDF.cc:518-522,524-526` | `crates/flpdf/src/reader/resolver.rs::reconstructed_xref`（`pub(crate)` getter。実体は `crates/flpdf/src/reader/resolver.rs:350-356` の core フィールド） / open 時は `crates/flpdf/src/xref.rs:1646` の `already_reconstructed` | `reconstructed_xref(` prod: 6 (reader.rs 4, resolver.rs 2) / test: 22（`crates/flpdf/src/reader.rs:1337-1338` は `:1336` の `#[cfg(test)]` 配下なので test に数える。`\breconstructed_xref\b` だと 48 hit になり再現しない） | mixed | `crates/flpdf/src/reader/resolver.rs::reconstructed_xref` | 状態そのものは 1 つに集約されているが、open 時の回復は `Pdf` 構築前に起きるため `LoadedXrefState::already_reconstructed` を経由して `ResolverCore` に転記される（`crates/flpdf/src/engine.rs:215-224`）。qpdf は `QPDF` が最初から存在するので転記が無い。B22 と同根 |
| B26 | `m->attempt_recovery`（回復分岐の on/off。分岐点は全ソースで 3 箇所） | `include/qpdf/QPDF.hh:1461`, `libqpdf/QPDF.cc:336,463,1391,1563` | `crates/flpdf/src/reader/resolver.rs::attempt_recovery`（`pub(crate)`） / open 時は `crates/flpdf/src/xref.rs::XrefLoadOptions` の `allow_repair` | `attempt_recovery(` prod: 1 (resolver.rs) / test: 0。`XrefLoadOptions` prod: 16 (xref.rs, engine.rs) / test: 42。`RecoveryPolicy` prod: 17 (file_object.rs, xref.rs) / test: 6 | mixed | `crates/flpdf/src/reader/resolver.rs::attempt_recovery` | 同じ 1 bit が 2 つの型に分かれて存在する。さらに `crates/flpdf/src/reader/file_object.rs::RecoveryPolicy`（B11）が同じ設定を 3 つ目の表現（`RequireEndstream` / `Bounded`）に翻訳しており、`crates/flpdf/src/xref.rs:517-523` が `allow_repair` からそれを導出する。qpdf の分岐点 3 箇所に対して flpdf の分岐点は数え直しが要る |
| B27 | `m->warnings`（**唯一の warning sink**。順序 = `warn` 呼び出し順、`push_back` のみ） | `include/qpdf/QPDF.hh:1475`, `libqpdf/QPDF.cc:487-494` | `crates/flpdf/src/reader/resolver.rs::ResolverCore` の `repair_diagnostics` フィールド（`crates/flpdf/src/reader/resolver.rs:357-366`）。初段classic trailerは `.48.12` でparse前に構築したcanonical resolverへ接続したが、owner-less xref stream/ObjStm bootstrapのdiagnosticsは `LoadedXref` / `BootstrapHandleState` stagingに残る。canonical openも `LoadedXref::repair_diagnostics` を engine の `replay_warnings`（`crates/flpdf/src/engine.rs:307`）へ渡すため、warning replay/live delivery cutoverは未完了 | 公開ドア `.repair_diagnostics()` — prod: 20 (flpdf-cli/main.rs, flpdf-qtest-tools 5 ファイル, engine.rs, job/check.rs, job/inspection.rs, job/lifecycle.rs, json/document.rs, reader.rs) / test: 162 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverCore` | `.48.12` の最初のconsumerはclassic trailerのdocument/cache identity。`.48.15.1` で canonical `Pdf::open` の `LoadedXrefState::bootstrap_cache`保持、canonical trailer/xref-stream rebind、第二 cache teardownを撤去した（`crates/flpdf/src/xref.rs:232-254`, `crates/flpdf/src/engine.rs:277-307`, `crates/flpdf/src/reader.rs:1582-1608`）。残る `BootstrapHandleState::diagnostics` → `XrefReadContext::diagnostics` → `LoadedXref::repair_diagnostics` の owner-less handoff と canonical warning replay/live delivery は parent `.48.15` の後続範囲。probeはP4を継続 |
| B28 | `QPDF::warn` (warning push and conditional logger delivery) | `libqpdf/QPDF.cc:487-494,496-504` | `crates/flpdf/src/reader/resolver.rs::push_qpdf_warning` | typed `QpdfExc` producers route through one collection and logger path | canonical | `crates/flpdf/src/reader/resolver.rs::push_qpdf_warning` | Independent exception fields are collected before `what_bytes` logger delivery; suppression affects logger only, matching qpdf.
| B29 | `QPDF::getWarnings`（返してclear） / `anyWarnings` / `numWarnings` | `libqpdf/QPDF.cc:345-363`, `libqpdf/QPDFJob.cc:476,493,796,1690,2116,3071` | `Pdf::get_warnings` / `any_warnings` / `num_warnings`（`crates/flpdf/src/reader.rs:335-362`）と、互換観測用に残した `repair_diagnostics` snapshot | `ResolverHandle::get_warnings` / `any_warnings`（`crates/flpdf/src/reader/resolver.rs:2050-2069`）を `ResolverCore::repair_diagnostics` に接続。Jobの完了境界は `QPDFJob::drain_document_warnings`（`crates/flpdf/src/job/lifecycle.rs:3891-3905`）へ移行し、foreign/open-time の中間判定は `any_warnings` を使う | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverCore`（B27のwarning collection） | **2026-09-08 bounded cutover**: `get_warnings` は logger 再配送なしで順序付き collection を返して clear、`any_warnings` は非破壊、`num_warnings` は直近 drain 後の件数を返す。抑制中も収集は継続する。残る snapshot/bookmark consumer は CLI の lazy warning emission、OpenFailure、xref/bootstrap handoff、check の段階的な新規 warning 検出、qtest/test assertionsであり、caller-zero による `repair_diagnostics` 撤去は後続 slice。 |
| B30 | `QPDFExc::createWhat` (raw filename/object/signed offset/detail composition) | `libqpdf/QPDFExc.cc:18-51`, `include/qpdf/QPDFExc.hh:29-77` | `crates/flpdf/src/error.rs::QpdfExc` | `QpdfExc::new` stores all fields and `what_bytes` exposes the first-NUL C-string result | canonical | `crates/flpdf/src/error.rs::QpdfExc` | All warning producers construct the typed primitive directly; no prefix sniffing or duplicate formatter remains.
| B31 | `QPDFLogger`（`p_warn` 既定 null → `getError` へフォールバック、`setOutputStreams` で `p_warn` を null に戻す） | `libqpdf/QPDFLogger.cc:109-116,218-246,248-255` | `crates/flpdf/src/logger.rs::QPDFLogger`（`pub`。`get_warn` フォールバックは `crates/flpdf/src/logger.rs:181-184`） | `get_warn(` prod: 1 (logger.rs) / test: 3 (`crates/flpdf/tests/qpdf_logger_tests.rs:50,163,186`) | canonical | `crates/flpdf/src/logger.rs::QPDFLogger` | 逐語移植で、経路も 1 本。`get_warn` の唯一の production 呼び出し元は `crates/flpdf/src/logger.rs:170`（`QPDFLogger::warn` の中）で、warning の実配送はそこを通る — `route_warning`（B30）は `logger.warn(line)` を呼ぶ（`crates/flpdf/src/reader/resolver.rs:493,516`）ので、`get_warn` へは `QPDFLogger::warn` 経由で 1 本に合流する。`get_warn` の `p_warn` → `get_error` フォールバックと `set_output_streams` の `warn = None`（`crates/flpdf/src/logger.rs:282`）が qpdf（`libqpdf/QPDFLogger.cc:110-116,244`）と 1:1。qpdf 同様 **sink ではない**（`m->warnings` への push には関与しない）ことは B27/B28 の構造で保たれている |
| B32 | 例外分類: `QPDFExc`（`std::runtime_error` 派生 + `qpdf_error_code_e`）と `std::logic_error` / `std::runtime_error` | `include/qpdf/QPDFExc.hh:29-77`, `libqpdf/QPDF.cc:481,1231`, `libqpdf/QPDFParser.cc:163`, `libqpdf/QPDFTokenizer.cc:241,248,770`, `libqpdf/QPDFLogger.cc:200,252`, `libqpdf/QPDF.cc:1101`（`std::range_error`） | `crates/flpdf/src/error.rs::Error`（`pub` enum。`Internal` / `System` / `Parse` / `Pages` / `Encrypted` / `Missing` / `Unsupported` / `Usage` / `Io` / `FileIo` / `OpenFailure`） | `Error::Internal(` prod: 166 / test: 91（計 257 = 全出現）、`Error::System(` prod: 103 / test: 138（計 241。全出現は 242 で、差の 1 件は `crates/flpdf-qtest-tools/src/driver/test_18_25.rs:171` のコメント行のため上の規約で除外）、`Error::parse(` prod: 161 / test: 10（計 171 = 全出現） | mixed | `crates/flpdf/src/error.rs::Error` | 型は 1 つに集約されているが、**qpdf の 2 軸（例外クラス × `qpdf_error_code_e`）を Rust の 1 軸に畳んでいる**。`Internal` ↔ `std::logic_error` / `System` ↔ `std::runtime_error` は `crates/flpdf/src/error.rs:42-43` の doc が明記する 1:1 対応。一方 `Parse` は qpdf の `QPDFExc(qpdf_e_damaged_pdf, ...)` に相当するが `QPDFExc` の他の error code（`qpdf_e_object` / `qpdf_e_pages` 等）は `Pages` など別 variant に分かれ、`QPDFExc` という 1 つのクラスが flpdf では複数 variant に散る。`Missing` / `Unsupported` / `OpenFailure` は qpdf に対応する例外クラス・error code が無い。`crates/flpdf/src/reader/resolver.rs:1615` は「`Error::Parse` だけが reconstruct の trigger になる」ことを qpdf の `catch (QPDFExc&)`（`libqpdf/QPDF.cc:1614`）と対応づけているので、この振り分けは回復分岐の正しさに直接効く |
| B33 | `reconstruct_xref` の 3 連 warn（`file is damaged` → 引数 `e` → `Attempting to reconstruct cross-reference table`、順序固定） | `libqpdf/QPDF.cc:528-530` | `crates/flpdf/src/xref.rs::push_repair_diagnostics`（private、open 時） / `crates/flpdf/src/reader/resolver.rs:1626-1636`（resolve 時のインライン 3 連 `push_warning`） | `push_repair_diagnostics(` prod: 2 (xref.rs) / test: 0。resolver 側インラインは 1 箇所 (resolver.rs:1626-1636) | mixed | `crates/flpdf/src/xref.rs::push_repair_diagnostics` | B22 の帰結。2 実装があり、中央の「引数 `e` をそのまま warn する」部分の文言生成が別。`push_repair_diagnostics`（`crates/flpdf/src/xref.rs:2892-2921`）は trigger error のメッセージごとに 5 通りの分岐で `QPDFExc` 相当の文言を再構成するのに対し、resolver 側（`crates/flpdf/src/reader/resolver.rs:1631-1635`）は `"(object N G, offset X): message"` を固定書式で組む。qpdf はどちらも `warn(e)` の 1 行で、文言は `e` が作られた時点の `createWhat` が決めている（B30） |
| B34 | （qpdf に対応物なし）bounded windowのread-to-end fallback予算 | absent — `libqpdf/QPDF.cc:1541-1697` はlive sourceをseek/parseする | 削除済み（旧 `pdf.rs` の `resolution_fallbacks_remaining`、`engine.rs` の `MAX_RESOLUTION_FALLBACKS`） | prod: 0 / test: 0 | canonical | absent | A22のqtest metadata/source-stream offset再parseだけが使っていた予算。2026-09-08（`.44`）でE27/A22の外部callerが0になった後、2026-09-08（`.25`）で `resolution_fallbacks_remaining` フィールド・`MAX_RESOLUTION_FALLBACKS` 定数・両初期化箇所・唯一の消費者だった `qtest_read_source_object_with_retry`/`parse_source_file_object_at` ごと撤去した。qpdf に対応物のない独自予算が丸ごと消えたため canonical（absent）に区分し直す。 |

`.27.1` で `error.rs::QpdfExc` / `QpdfErrorCode` を追加した。これはB30/B32の
structured primitiveだけであり、既存resolverの3 formatter、prefix sniffing、
`Error`/`Diagnostic` consumer移行は後続stack層で行う。特にB30の負offset・embedded
NUL・non-UTF8挙動はqpdf C++ probe（`/tmp/qpdfexc_probe.out`）でgetter raw bytesと
`std::string(e.what())` observable bytesを分離して確認済みである。

B30の既存行にある「負値もoffset無し」という要約は不正確である。qpdfは
`object.empty() && offset == 0` のときだけ位置部分を省略し、filenameが非空・
objectが空・offsetが負値なら `filename (): message` を生成する。`.27.1` の
`QpdfExc::create_what` とfocused testはこの分岐を正本に合わせている。
このfilename-only/negative-offset形はpinned C++ probeの`case=filename-only`
出力（`what_cstr=662028293a206d`）でも確認済みである。

2026-09-08に `.48.68` の回帰を qpdf 11.9.0 と照合した。`startxref` が欠落・不正、または値 `0` の場合、qpdf は `QPDF.cc:450-452` で `read_xref` を呼ばず、`QPDF.cc:516-575` の `reconstruct_xref` へ直接進む。flpdf も `load_xref_state_from_bytes` で同じ分岐にし、logical offset 0 の speculative read が先頭の旧 object を canonical cache に登録することを止めた。これにより同一 generation を再利用する Catalog の後発 `/PageLabels` number tree が writer traversal に残り、qpdf の `append-page-content-damaged.pdf` に対する `--static-id -qdf --no-original-object-ids` 出力と 16484 bytes で byte-identical になった。flpdf-authored の `tests/fixtures/compat/recovered-catalog-pagelabels.pdf` と `reader_tests.rs` の `/PageLabels`/`/Nums` 回帰も追加した。

2026-09-08（`flpdf-buy0`）: /Prev hop内のwarning順序をqpdfへ合わせた。
qpdfは `m->warnings` 単一sinkへ呼出時点でpushする
（`libqpdf/QPDF.cc:487-494`）ため、classic trailerのparser warningが
hybrid `/XRefStm` read warningより先になる。flpdfではcanonical ownerの
deferred diagnosticsをclassic sectionのbuffered diagnosticsより前に一括spliceしていたため、
hop内に分割した順序調整と合成RED/GREEN fixtureを追加し、
`stream keyword found in trailer` → `stream filter type is not name or array`
をqpdf CLIと照合した。`flpdf-5tt9` の外側deferral window、root route、
qtest exceptionsは変更しない。

### 分類集計

| 分類 | 件数 | 行 |
|---|---|---|
| canonical | 15 | B1, B3, B5, B6, B15, B16, B17, B18, B19, B21, B23, B24, B28, B31, B34 |
| mixed | 19 | B2, B4, B7, B8, B9, B10, B11, B12, B13, B14, B20, B22, B25, B26, B27, B29, B30, B32, B33 |
| bridge | 0 | — |
| unknown | 0 | — |

合計 34 行。2026-09-06の再確認でB29はdocumentのwarning collectionをownerと確定し、
未移植drainと既存snapshotが併存するmixedへ更新した。2026-09-07にB5の
`ResolverHandle::in_parse`primitiveを移植しunknownからcanonicalへ更新した
（`flpdf-3yn9.48.17`）。`.48.14` でB7のObjStm consumerに
`next_object_stream_integer`を追加したため、canonical ownerが`absent`なのはB24の1行。
2026-09-08（`.25`）: B34 が持っていた「qpdfに無い状態をflpdfが持っている」
fallback-budget（`resolution_fallbacks_remaining`）を削除したため、B34 を bridge から
canonical へ移し bridge を 0 にした（残る参照は `tests/qpdf_route_hygiene_tests.rs:82` の
存在しないことを検査する hygiene テストのみ）。B24は
**両者とも持たないのが正しい**（qpdf 自身が `libqpdf/QPDF.cc:618-622` でやらないと
明言している処理）ため、absent 同士の一致として canonical に数える。

## unknown / probe

| probe | 対象行 | 必要な確認 |
|---|---|---|
| P1 | B14 | **解消（2026-09-08、`flpdf-3yn9.48.18`）**: `/Prev` が初段 startxref を指す classic PDFを qpdf 11.9.0 と flpdfで比較した。両方とも `file is damaged` / `loop detected following xref tables` / `Attempting to reconstruct cross-reference table` の3行・同順・exit 3で、既存の三連warningは一度だけ。追加した stream-token fixtureでは qpdf/flpdfとも `stream keyword found in trailer` が一度だけで、`merge_previous_xref_sections_with_observer` の seeded visitorが初段を再parseしないことを回帰テストで固定した。二重warningをbugとして扱わない。 |
| P2 | B20 | **2026-09-08 probe 継続（`flpdf-3yn9.48.19`）**: `load_xref_state_from_bytes` の初段 parse 失敗 handoff がこの filter を飛ばす経路自体は現存する（未変更）が、`registration.deleted_objects` へ free entry が実際にコミットされる箇所（`parse_xref_from_start_with_owner` のクラシック分岐は `deferred_free` を `merge_xref_stream_from_classic_trailer` 成功後にのみ適用、`parse_xref_stream` は自身の `build_result` が `Ok` の場合のみ free entry ループへ到達）と、`reconstruction_trigger` を立てる唯一の生成箇所（`read_uncompressed_object_with_end`、`crates/flpdf/src/xref.rs:794-799`）が同じ呼び出しで必ず `Err` を即時返す（`?` で伝播）ことを突き合わせると、「free entry が committed 済みかつ trigger も立っている」状態を作る経路が見当たらなかった——trigger が立つ呼び出しは常にその場で失敗し、free entry ループより先に return する。この分析はソースの読み合わせのみで、実際に壊れた fixture を作って `qpdf --show-xref` / flpdf `get_xref_table` を突き合わせる実証はまだ行っていない。次に着手する際は実証を先にすること（分析だけで確定と扱わない）  **到達可能な経路が判明したため「非issue」の暫定判断は撤回する**: owner-less の hybrid xref 経路では trigger と非空 deleted set が共存しうる。xref stream の間接 `/Length` が食い違う classic entry を介して解決されると `resolve_length` はそのエラーを `Missing` へ落としつつ `reconstruction_trigger` を保持したままにするため、stream recovery と `build_result` が成功しうる。その場合 `parse_xref_stream` はデコード済みの free 行を `registration.insert_free_xref_entry`（`crates/flpdf/src/xref.rs:4302-4310`）でコミットしてから、保持していた trigger を `Err` として返す（`:4336-4342`）。初段 parse 失敗の handoff は`registration.entries` しか渡さない（`:1915-1949`）ので、この filter は依然として落ちる。**hybrid fixture での qpdf 比較を実施するまで、この probe は開いたままにする。** |
| P3 | B22 | **2026-09-08 部分解消・probe は継続（`flpdf-3yn9.48.19`）**: `reconstruct_xref_and_retry` の compressed 分岐を `Ok(None)`（`Free`/`None` と同じ warn+null）へ統一し、`qpdf --check` 相当の `object N G not found in file after regenerating cross reference table` 警告と null 解決を再構築後の compressed entry でも再現するようにした。新規ユニットテスト `reconstruct_xref_and_retry_treats_a_post_reconstruction_compressed_entry_as_not_found`（`crates/flpdf/src/reader/resolver.rs`）で `Ok(None)` を直接検証。呼び出し元にあった `Error::Unsupported` 捕捉からの `resolve_object_stream_or_null` 再ルーティング（未カバーで `cov:ignore` 済みだった箇所）は削除した  **未確認として残す部分**: 本番経路ではこの分岐に到達できない。`resolve_indirect` が `reconstruct_xref_and_retry` を呼ぶのは `Uncompressed` アームだけで、再構築は `install_source_xref_entries`（`crates/flpdf/src/reader/resolver.rs:2890`）がテーブルを丸ごと置換する形で行われ、`recover_xref_entries`（`crates/flpdf/src/xref.rs:3111`）は`XrefEntry::Uncompressed` しか挿入しない。qpdf 側も同じで、`reconstruct_xref` が削除するのはtype 1 のみ（`libqpdf/QPDF.cc:531-540`）だが、要求 objgen は 1 エントリしか持てないためtype 1 を消した後にその objgen が type 2 として残ることはなく、`getType() == 1`（`:1618`）の compressed 側は qpdf でも防御的な分岐である。したがって新規ユニットテストは`source_xref_entries` に `Compressed` 行を直接注入して分岐だけを固定しており、警告文言と null 解決を end-to-end で通したわけではない。**この probe は「再構築後に compressed entry が残る実 fixture を構成できるか」の確認として開いたままにする**。 |
| P4 | B27 | bootstrap handle の warning（`BootstrapHandleState::diagnostics` に入るもの、例: 参照された `/Length` object の読みが壊れている）と、xref table 自身の warning（`/Size` 不一致など）を **両方** 発生させる fixture を作り、`qpdf --check` の warning 順序と flpdf の `repair_diagnostics().entries()` の順序を比較する。flpdf は `append_diagnostics_to`（`crates/flpdf/src/xref.rs:1156-1161`）を呼んだ時点で連結順が決まるため、warn 発生順と一致しない可能性がある。qpdf は常に `warn` 呼び出し順（`libqpdf/QPDF.cc:487-494`） |
| P5 | B5 | **解消済み（2026-09-07、`flpdf-3yn9.48.17`）**: 2026-09-06時点ではparserのresolver呼出はhandle生成/description取得のみで通常のparseが解決しないことを確認していたが、qpdf自身もparse中にresolveしない前提でParseGuardを持つ（`libqpdf/QPDFParser.cc:29-34`）ため「不要guard」とは結論しなかった。実際の再入triggerは無い（`context->getObject`相当は解決しない）ため、`ResolverHandle::in_parse`の対称チェック両方向を直接probeで固定し、`LiveFileParser::parse`が失敗return時もguardを復元することをmockリゾルバで検証した。B5行を参照。 |
| P6 | B29 | **owner確認済み（2026-09-06）**: qpdf `libqpdf/QPDFJob.cc:493,796,1690` はJob処理完了でgetWarningsをdrainし、`:476,2116,3071` はanyWarningsを使う。`libqpdf/QPDF.cc:345-363` のdrain/non-drain契約を同じdocument collectionへ移植し、Job完了consumerから移す。現Rustのnum_warningsは既存であり再実装対象ではない。 |
| P7 | B7 | **解消（2026-09-08、`flpdf-3yn9.48.14`）**: canonical/bootstrapのObjStm headerを専用 `Tokenizer::next_object_stream_integer`（`read_token(true, 0)` を2回呼ぶconsumer）へ移行し、qpdf `libqpdf/QPDF.cc:1801-1814` の token読取→integer検査順を固定した。汎用 `next_integer` と xref ByteCursorの `read_token(false)` はclassic readLine/parse_xrefEntry責務として変更していない。 |
| P8 | B13, B17 | **B13のstream/empty条件を解消（2026-09-08、`flpdf-3yn9.48.18`）**: 共通 `read_trailer` の qpdf `readToken` lookahead と trailer attributionを追加し、`stream keyword found in trailer`（offset 134）と empty candidate（offset 53）をqpdf 11.9.0と照合した。`unknown xref stream entry type` は `crates/flpdf/src/xref.rs` に存在し、旧「文言不在」列挙から除外する（closed `flpdf-4sgf`）。その他のxref文言は条件ごとにoracle fixtureと現診断を比較し、不在・別文言・未到達を区別する。一括文字列検索だけで全条件未実装とは結論しない。 |


## 2026-09-06 再監査の issue 対応

親 epic は `flpdf-3yn9.48`。下表は責務と実装 issue の対応であり、完了状態は `bd show <id>` で確認する。
各 issue の受入条件に qpdf 根拠、最初の consumer、残 caller と削除条件を記録した。

| 対象行 | Beads issue | 責務 / 移行 slice |
|---|---|---|
| `B27` | `flpdf-3yn9.48.12` | QPDF document stateをparse前から所有しclassic trailerを同じcacheへ生成する |
| `B7` / `B8` / `B9` / `B10` / `B11` | `flpdf-3yn9.48.13` | bootstrap object readerをQPDF::readObjectAtOffset/readObject/readStreamへ移行する |
| `B7` / `B12` | `flpdf-3yn9.48.14` | bootstrap ObjStm展開をcanonical resolveObjectsInStreamへ移行する |
| `B27` | `flpdf-3yn9.48.15` | bootstrap rebind・warning replay・第二teardownを撤去する |
| `B2` / `B4` | `flpdf-3yn9.48.16` | content parserをQPDFParserのcontent_stream modeへ統合する |
| `B5` | `flpdf-3yn9.48.17` | QPDF::inParse/ParseGuardのdocument再入契約を移植する |
| `B7` / `B13` / `B14` | `flpdf-3yn9.48.18` | QPDF::read_xref/readTrailerの初段・Prev共通経路を移植する |
| `B20` / `B22` / `B25` / `B26` / `B33` | `flpdf-3yn9.48.19` | reconstruct_xrefのopen時・resolve時を同一document責務へ統合する |
| `B34` | `flpdf-3yn9.48.25` | test0 cutover後にsource metadata再parse・window・64回retry budgetを撤去する |
| `B29` | `flpdf-3yn9.48.26` | QPDF::getWarnings drain・anyWarningsを移植しJob完了consumerを移行する |
| `B30` / `B32` | `flpdf-3yn9.48.27` | QPDFExcの構造化context/createWhatを移植しresolver warningに統合する |
| `B12` | `flpdf-3yn9.48.38` | ObjStm codec例外境界をQPDF::resolveへ集約する |
| `B17` | `flpdf-3yn9.48.43` | bootstrap xref stream payload consumerをcanonical specialized pipeへ移行する |
| `B8` / `B9` / `B10` / `B11` | `flpdf-scj3` | candidate読取warningの欠落（canonical読取移行へ） |
| `B8` / `B9` / `B10` / `B11` | `flpdf-q759` | candidate EOF-after-endobj契約（canonical読取移行へ） |
| `B8` / `B9` / `B10` / `B11` | `flpdf-8q38` | 既存reader診断の回帰対象 |

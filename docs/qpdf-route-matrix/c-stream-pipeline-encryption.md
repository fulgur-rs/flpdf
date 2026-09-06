# C. stream data provider / decode / retry / filter / encryption / `/Length`

対象: `QPDF_Stream` の 3 つの data source（`stream_data` / `stream_provider` / original）、
`pipeStreamData` の decode level・`filterable` 判定・retry family、`replaceStreamData` /
`replaceFilterData` の `/Length` 契約、`QPDF::Pipe::pipeStreamData` と pipe 時 `decryptStream`、
`QPDFWriter::willFilterStream` の compress / decode 分岐、`copyStreamData`。
flpdf 側は `object_handle.rs` の stream 面、`filters.rs`、`stream_filter.rs`、`pipeline/*`、
`writer/plain/body.rs` と `writer.rs` の書き出し policy、`reader/resolver.rs` の pipe 時復号、
`encryption/*`。

## qpdf 責務モデル

すべて pinned qpdf 11.9.0 source（`scripts/fetch-qpdf-source.sh --print-path`）を `rg -n` / `sed -n` で
読んだ範囲のみ引用する。**ファイル名のない `:N-M` は checker が検証しないため、本節では
すべてファイル名付きで引用する。**

### C-1. `QPDF_Stream` の state と 3 つの data source

`libqpdf/qpdf/QPDF_Stream.hh:101-106` のフィールドは `filter_on_write` / `stream_dict` / `length` /
`stream_data` / `stream_provider` / `token_filters` の 6 つ。これに基底 `QPDFValue` の `parsed_offset`
（`QPDF_Stream::setDescription` `libqpdf/QPDF_Stream.cc:298-304` 経由で `QPDF::readStream` から渡る
`stream_offset`）が加わる。stream のバイト列は次の **3 source のどれか 1 つ** から供給され、
`pipeStreamData` 内の分岐順序（`libqpdf/QPDF_Stream.cc:571-622`）がそのまま優先順位である:

| 優先 | source | 設定経路 | 排他 |
|---|---|---|---|
| 1 | `stream_data`（`std::shared_ptr<Buffer>`） | `QPDF_Stream::replaceStreamData(Buffer)` のみ（`libqpdf/QPDF_Stream.cc:640-649`） | 同時に `stream_provider = nullptr`（`libqpdf/QPDF_Stream.cc:647`） |
| 2 | `stream_provider`（`StreamDataProvider`） | `QPDF_Stream::replaceStreamData(provider)`（`libqpdf/QPDF_Stream.cc:651-660`） | 同時に `stream_data = nullptr`（`libqpdf/QPDF_Stream.cc:658`）、`/Length` は **削除**（`replaceFilterData(..., 0)` → `libqpdf/QPDF_Stream.cc:678-680`） |
| 3 | original（`parsed_offset` + `length` でファイルから読む） | `QPDF::readStream`（`libqpdf/QPDF.cc:1361-1399`）が `QPDF_Stream::create(this, og, object, stream_offset, length)`（`libqpdf/QPDF.cc:1398`）で生成 | `parsed_offset == 0` なら「data 無し」として `std::logic_error("pipeStreamData called for stream with no data")`（`libqpdf/QPDF_Stream.cc:605-607`） |

`QPDF::newStream()` / `reserveStream` は offset=0, length=0 で生成する（`libqpdf/QPDF.cc:1911-1916`,
`libqpdf/QPDF.cc:1945-1949`）ので、`replaceStreamData` されるまで source 3 の「data 無し」状態。
`newStream(Buffer)` / `newStream(string)` は直後に `replaceStreamData(data, newNull(), newNull())` を
呼ぶ（`libqpdf/QPDF.cc:1918-1932`）。

parse 時の `/Length` 契約（`libqpdf/QPDF.cc:1370-1397`）: `/Length` が Integer でなければ
`damagedPDF`（null → "stream dictionary lacks /Length key"、非整数 → "/Length key in stream dictionary
is not an integer"）、整数なら `stream_offset + length` へ seek して次 token が `endstream` でなければ
"expected endstream"。`attempt_recovery` なら warn して `recoverStreamLength`
（`libqpdf/QPDF.cc:1482-1530`）へ、さもなくば throw。
**parse 時には復号もフィルタ解決も行わない**（`readStream` は offset/length を記録するだけ）。

`recoverStreamLength` は `findFirst("end", stream_offset, 0, ef)` で `endstream`/`endobj` を探し、
`QPDF::findEndstream`（`libqpdf/QPDF.cc:1469-1479`）が token 先頭へ seek し戻すため、
**復元 length は `endstream` 直前の改行を含む**（`libqpdf/QPDF.cc:1488-1492`）。qpdf は
この length をそのまま pipe する（改行を落とす処理は無い）。

### C-2. `QPDF_Stream::pipeStreamData` の call order（`libqpdf/QPDF_Stream.cc:487-638`）

1. `filter = !(encode_flags == 0 && decode_level == qpdf_dl_none)`（`libqpdf/QPDF_Stream.cc:504`）。
2. `filter` なら `filterable(filters, specialized, lossy)`（`libqpdf/QPDF_Stream.cc:507`、C-3）で判定し、
   さらに decode level で gate: `decode_level < qpdf_dl_all && lossy` → `filter = false`、
   `decode_level < qpdf_dl_specialized && specialized` → `filter = false`
   （`libqpdf/QPDF_Stream.cc:508-513`）。
3. `pipeline == nullptr` なら **probe mode**: 戻り値は `filter`（「フィルタ可能か」）
   （`libqpdf/QPDF_Stream.cc:523-527`）。
4. pipeline を **逆順** に組む（`libqpdf/QPDF_Stream.cc:529-569`）: `qpdf_ef_compress` →
   `Pl_Flate(a_deflate)`、`qpdf_ef_normalize` → `Pl_QPDFTokenizer(ContentNormalizer)`、
   `token_filters` を rbegin から `Pl_QPDFTokenizer`、最後に decode filters を rbegin から
   `getDecodePipeline`（nullptr を返す filter は段を追加しない — `SF_Crypt`
   `libqpdf/QPDF_Stream.cc:52-57`）。`Pl_Flate` には `warn` callback を付ける
   （`libqpdf/QPDF_Stream.cc:564-567`）。
5. source dispatch（`libqpdf/QPDF_Stream.cc:571-622`、C-1 の優先順位）:
   - `stream_data`: `write` + `finish`（`libqpdf/QPDF_Stream.cc:571-574`）。
   - `stream_provider`: `Pl_Count` で包み（`libqpdf/QPDF_Stream.cc:576`）、**呼び出し側が
     `supportsRetry()` を見て** 4 引数版 `provideStreamData(og, &count, suppress_warnings, will_retry)`
     （bool 戻り）か 2 引数版（void）かを選ぶ（`libqpdf/QPDF_Stream.cc:577-585`）。false 戻りなら
     `filter = false; success = false`（`libqpdf/QPDF_Stream.cc:580-581`）。その後 `/Length` 検証:
     `success && stream_dict.hasKey("/Length")` なら `count.getCount()` と `/Length` を比較し、
     不一致は `std::runtime_error("stream data provider for N G provided X bytes instead of expected
     Y bytes")`（**programmer error**、入力破損ではない。`libqpdf/QPDF_Stream.cc:594-600`）。`/Length` が
     無ければ実測値で `replaceKey("/Length", newInteger(actual_length))`
     （`libqpdf/QPDF_Stream.cc:601-604`）。
   - `parsed_offset == 0`: `std::logic_error`（`libqpdf/QPDF_Stream.cc:605-607`）。
   - original: `QPDF::Pipe::pipeStreamData(qpdf, og, parsed_offset, length, stream_dict, pipeline,
     suppress_warnings, will_retry)`（`libqpdf/QPDF_Stream.cc:608-621`、C-4）。false 戻りなら
     `filter = false; success = false`。
6. `filter && !suppress_warnings && normalizer && anyBadTokens()` で normalize 警告 3 種
   （`libqpdf/QPDF_Stream.cc:624-635`）。
7. 戻り値は `success`（overall）、`*filterp` は「フィルタを **実際に** 適用したか」（失敗時 false）。

public 面（`libqpdf/QPDFObjectHandle.cc:1300-1342`, `include/qpdf/QPDFObjectHandle.hh:1041-1068`）は
3 overload で、**戻り値の意味が違う**:

- 6 引数版（`libqpdf/QPDFObjectHandle.cc:1300-1311`）= `success` をそのまま返し、
  `filtering_attempted` は out-param。
- 5 引数版（`libqpdf/QPDFObjectHandle.cc:1313-1325`）= `success` を **捨てて**
  `filtering_attempted` を返す。`QPDFWriter::willFilterStream`（`libqpdf/QPDFWriter.cc:1293`）が
  呼ぶのはこちらなので、qpdf の writer は source success を直接は見ない —
  source 失敗時に `pipeStreamData` 自身が `filter` を落とす（`libqpdf/QPDF_Stream.cc:580-581`,
  `libqpdf/QPDF_Stream.cc:619-620`）ことに依存している。
- legacy 4 引数版（`libqpdf/QPDFObjectHandle.cc:1327-1342`）は
  `filter → qpdf_dl_generalized` へ写像する。

`getStreamData(level)`（`libqpdf/QPDF_Stream.cc:344-360`）= `Pl_Buffer` へ pipe、`!filtered` なら
`QPDFExc(qpdf_e_unsupported, ..., "getStreamData called on unfilterable stream")`。
`getRawStreamData()`（`libqpdf/QPDF_Stream.cc:362-376`）= `qpdf_dl_none` で pipe、`success == false` なら
`QPDFExc(qpdf_e_unsupported, ..., "error getting raw stream data")`。どちらも whole-buffer だが
内部は同じ `pipeStreamData` を通る（= qpdf に「whole-buffer 経路」という別 route は無く、
`Pl_Buffer` を末端に置いた pipe の別名にすぎない）。

### C-3. `filterable` と filter factory（`libqpdf/QPDF_Stream.cc:378-485`）

- `/Filter`: null / Name / Array-of-Name 以外は `warn("stream filter type is not name or array")` → false
  （`libqpdf/QPDF_Stream.cc:386-415`）。`filter_abbreviations`（`libqpdf/QPDF_Stream.cc:72-83`、
  `/AHx` `/A85` `/LZW` `/Fl` `/RL` `/CCF` `/DCT`）を展開。
- `filter_factories`（`libqpdf/QPDF_Stream.cc:85-94`）は **static map**: `/Crypt`（`SF_Crypt`
  `libqpdf/QPDF_Stream.cc:27-58`）/ `/FlateDecode` / `/LZWDecode` / `/RunLengthDecode` / `/DCTDecode` /
  `/ASCII85Decode` / `/ASCIIHexDecode`。未登録名が 1 つでもあれば false
  （`libqpdf/QPDF_Stream.cc:425-435`、warning 無し）。`QPDF_Stream::registerStreamFilter`
  （`libqpdf/QPDF_Stream.cc:147-152`、`QPDF::registerStreamFilter` `include/qpdf/QPDF.hh:193` から公開）で
  **実行時追加** できる。
- `/DecodeParms`: 空配列は null 扱い（`libqpdf/QPDF_Stream.cc:443-445`）、配列なら要素ごと、
  それ以外は filter 数だけ複製（`libqpdf/QPDF_Stream.cc:446-454`）。
  `filters.size() != 0 && decode_parms.size() != filters.size()` は
  `warn("stream /DecodeParms length is inconsistent with filters")` → false
  （`libqpdf/QPDF_Stream.cc:458-461`）。
- 各 filter の `setDecodeParms`（`include/qpdf/QPDFStreamFilter.hh:43-44`）→ false なら
  filterable=false。`isSpecializedCompression` / `isLossyCompression`
  （`include/qpdf/QPDFStreamFilter.hh:58-61`）を集計（`libqpdf/QPDF_Stream.cc:467-482`）。
  `SF_FlateLzwDecode::setDecodeParms`（`libqpdf/SF_FlateLzwDecode.cc:22-73`）: `/Predictor` は
  1, 2, 10-15 のみ、`/Columns` `/Colors` `/BitsPerComponent` は Integer 必須、LZW の `/EarlyChange` は
  0/1、`predictor > 1 && columns == 0` は false。
- `SF_Crypt::setDecodeParms`（`libqpdf/QPDF_Stream.cc:33-50`）は `/Type` `/Name` 以外のキーがあれば
  false、decode pipeline は作らない（復号は C-5 の `decryptStream` が担う）。
- **警告経路は 1 本**: `filterable` の 2 つの warning も `Pl_Flate` の warn callback も
  `QPDF_Stream::warn`（`libqpdf/QPDF_Stream.cc:694-698`）を通り、`parsed_offset` 付きで
  `QPDF::warn(qpdf_e_damaged_pdf, ...)` に入る。

### C-4. `QPDF::Pipe::pipeStreamData` と retry family

- `QPDF::Pipe`（`include/qpdf/QPDF.hh:819-839`）は `friend class QPDF_Stream` の private 入口で、
  `QPDF::pipeStreamData(og, offset, length, dict, pipeline, suppress_warnings, will_retry)`
  （`libqpdf/QPDF.cc:2541-2562`）→ static `QPDF::pipeStreamData(encp, file, qpdf_for_warning, og, offset,
  length, stream_dict, pipeline, suppress_warnings, will_retry)`（`libqpdf/QPDF.cc:2476-2539`）へ委譲。
- static 版の順序: `encp->encrypted` なら **ここで** `decryptStream` を pipeline の前段に挿す
  （`libqpdf/QPDF.cc:2489-2492`、C-5）→ `seek(offset)` → `length` バイト読み（short read は
  `damagedPDF("unexpected EOF reading stream data")` `libqpdf/QPDF.cc:2496-2500`）→ `write` →
  `finish` → true。
- error boundary（`libqpdf/QPDF.cc:2505-2538`）: `QPDFExc` は `suppress_warnings` でなければ `warn(e)`；
  その他 `std::exception` は `"error decoding stream data for object N G: <what>"` を warn し、
  `will_retry` なら追加で `"stream will be re-processed without filtering to avoid data loss"` を warn。
  `finish` 未到達なら例外を握りつぶして `finish` を試みる。戻り値 false。
- **retry を決めるのは呼び出し側**: `QPDFWriter::willFilterStream`（`libqpdf/QPDFWriter.cc:1288-1310`）と
  `QPDF_Stream::writeStreamJSON`（`libqpdf/QPDF_Stream.cc:256-268`）が `attempt 1..2` ループで
  `will_retry = (attempt == 1)` を渡し、失敗なら decode level を落として再試行する。
  `pipeStreamData` 自身は retry しない。
- `StreamDataProvider`（`include/qpdf/QPDFObjectHandle.hh:72-127`,
  `libqpdf/QPDFObjectHandle.cc:48-90`）: `supports_retry` は ctor 引数。`QPDFObjGen` 版 2 つは
  `(int, int)` 版へ委譲し、`(int, int)` 版の既定実装は
  `std::logic_error("you must override provideStreamData -- see QPDFObjectHandle.hh")`。
  `FunctionProvider`（`libqpdf/QPDFObjectHandle.cc:1374-1429`）は `std::function<void(Pipeline*)>` →
  `supports_retry=false`、`std::function<bool(Pipeline*, bool, bool)>` → `supports_retry=true`。
- `pipeForeignStreamData`（`libqpdf/QPDF.cc:2564-2585`）は foreign 側の `encp` / `file` で static 版を
  呼ぶ（= 復号は **元ファイルの** encryption parameters で行う）。

### C-5. 暗号化: 復号は pipe 時、文字列は parse 時（`libqpdf/QPDF_encryption.cc`）

- `QPDF::decryptStream`（`libqpdf/QPDF_encryption.cc:1044-1154`）を呼ぶのは static
  `QPDF::pipeStreamData` の `libqpdf/QPDF.cc:2491` の **1 箇所のみ**
  （`rg -n 'decryptStream' $Q/libqpdf/*.cc` で確認）。`resolve` / `readStream` では呼ばれない。
  順序: `/Type /XRef` は復号しない（`libqpdf/QPDF_encryption.cc:1058-1061`）→ V≥4 なら `/Filter` に
  `/Crypt` があれば `/DecodeParms`（dict の `/CryptFilterDecodeParms` `/Name`、または filter/decode_parms が
  同長配列の場合の該当要素）を `interpretCF`（`libqpdf/QPDF_encryption.cc:700-716`）で解釈
  （`libqpdf/QPDF_encryption.cc:1063-1094`）→ 未決なら `!encrypt_metadata && /Metadata` は `e_none`、
  さもなくば `cf_stream`（`libqpdf/QPDF_encryption.cc:1096-1103`）→ `e_none` は復号なし、
  `e_aes`/`e_aesv3` は AES、`e_rc4` は RC4、未知は warn `"unknown encryption filter for streams
  (check <source>); streams may be decrypted improperly"` して `cf_stream = e_aes` にリセット
  （`libqpdf/QPDF_encryption.cc:1105-1134`）→ `getKeyForObject`
  （`libqpdf/QPDF_encryption.cc:954-974`、`cached_key_og` で 1 object 分キャッシュ）→
  `Pl_AES_PDF` / `Pl_RC4` を pipeline の前段に置き `pipeline` を差し替える
  （`libqpdf/QPDF_encryption.cc:1136-1153`）。
- `QPDF::compute_data_key`（`libqpdf/QPDF_encryption.cc:324-357`、`include/qpdf/QPDF.hh:551` で
  public static）: V≥5 は encryption_key そのまま、それ以外は key + objid 下位 3 byte + gen 下位 2 byte
  （+ AES なら `"sAlT"`）の MD5 を `min(連結後の入力長, 16)` に切る。**qpdf 内でこの関数は 1 つだけ**で、
  読み側 `getKeyForObject`（`libqpdf/QPDF_encryption.cc:963`）と書き側 `QPDFWriter::setDataKey`
  （`libqpdf/QPDFWriter.cc:845`）が同じ実装を共有する。
- `QPDF::decryptString`（`libqpdf/QPDF_encryption.cc:976-1039`）は **parse 時** に `QPDF::readObject`
  （`libqpdf/QPDF.cc:1330-1340`）が `StringDecrypter` を `QPDFParser` に渡して呼ばせる
  （`libqpdf/QPDFParser.cc:114-118`, `libqpdf/QPDFParser.cc:358-361`）。直接 object
  （`!og.isIndirect()`）は復号しない。stream と string で復号のタイミングが違う。
- 書き出し側: `QPDFWriter::pushEncryptionFilter`（`libqpdf/QPDFWriter.cc:975-999`）が
  `encrypted && !cur_data_key.empty()` のとき `Pl_AES_PDF(encrypt=true)` / `Pl_RC4` を push。
  `adjustAESStreamLength`（`libqpdf/QPDFWriter.cc:965-973`）で AES 時 `/Length` を
  `+ 32 - (len & 0xf)`。metadata で `!encrypt_metadata` なら `cur_data_key.clear()`
  （`libqpdf/QPDFWriter.cc:1545-1548`）で暗号化を外す。

### C-6. `QPDFWriter::willFilterStream` と stream 書き出し

`willFilterStream`（`libqpdf/QPDFWriter.cc:1238-1315`、宣言は `include/qpdf/QPDFWriter.hh:488-492`。
`include/qpdf/QPDFWriter.hh:440` の `private:` 配下で、間の `PipelinePopper` は
`include/qpdf/QPDFWriter.hh:456-472` に閉じたネストクラス）の判定順序:

1. `is_metadata = stream_dict.isDictionaryOfType("/Metadata")`（`libqpdf/QPDFWriter.cc:1251-1253`）。
2. `filter = isDataModified() || compress_streams || stream_decode_level`（`libqpdf/QPDFWriter.cc:1254`）。
3. `!getFilterOnWrite()` → `filter = false`（`libqpdf/QPDFWriter.cc:1255-1259`）。
4. `filter_on_write && compress_streams` かつ `!recompress_flate && !isDataModified() && /Filter が Name で
   /FlateDecode or /Fl` → `filter = false`（"not recompressing /FlateDecode"
   `libqpdf/QPDFWriter.cc:1260-1271`）。
5. `filter_on_write && is_metadata && (!encrypted || !encrypt_metadata)` → `filter = true;
   compress_stream = false; uncompress = true`（`libqpdf/QPDFWriter.cc:1274-1278`）；else
   `filter_on_write && normalize_content && normalized_streams.count(og)` → `normalize = true;
   filter = true`（`libqpdf/QPDFWriter.cc:1279-1281`）；else `filter_on_write && filter &&
   compress_streams` → `compress_stream = true`（`libqpdf/QPDFWriter.cc:1282-1285`）。
6. attempt 1..2: `pushPipeline(new Pl_Buffer("stream data"))` → 5 引数
   `stream.pipeStreamData(m->pipeline, encode_flags, decode_level, false, attempt == 1)`
   （`libqpdf/QPDFWriter.cc:1288-1299`。`encode_flags` は `filter&&normalize ? ef_normalize` |
   `filter&&compress_stream ? ef_compress`、`decode_level` は `filter ? (uncompress ? qpdf_dl_all :
   stream_decode_level) : qpdf_dl_none`）。`std::runtime_error` は `"error while getting stream data for
   <unparse>: "` を前置して再 throw（`libqpdf/QPDFWriter.cc:1300-1303`）。`filter && !filtered` →
   `filter = false` で再試行（`libqpdf/QPDFWriter.cc:1304-1309`）。
7. `!filtered` → `compress_stream = false`（`libqpdf/QPDFWriter.cc:1311-1313`）。戻り値 = `filtered`
   （→ `f_filtered` flag）。

呼び出し元は 2 箇所: `unparseObject` の `ot_stream` 分岐（`libqpdf/QPDFWriter.cc:1539`）と
`writeLinearized` の `skip_stream_parameters` lambda（`libqpdf/QPDFWriter.cc:2543-2551`、
`stream_data = nullptr`）。

`unparseObject` の stream 分岐（`libqpdf/QPDFWriter.cc:1528-1566`）:
`cur_stream_length = stream_data->getSize()`（= **書き出すバイト列を先に確定してから** dict を書く）→
`adjustAESStreamLength` → dict を `f_stream | f_filtered` で `unparseObject`（dict 側
`libqpdf/QPDFWriter.cc:1440-1486`: `/Length` 削除、空 `/DecodeParms` 配列削除、`f_filtered` なら
`/Filter` `/DecodeParms` 削除、さもなくば `/Crypt` だけ除去。**削除するのはこの 2 キーだけ**）→
`/Length` は `direct_stream_lengths` なら直値、さもなくば `cur_stream_length_id 0 R`
（`libqpdf/QPDFWriter.cc:1508-1518`）、`compress && f_filtered` なら `/Filter /FlateDecode`
（`libqpdf/QPDFWriter.cc:1519-1523`）→ `"\nstream\n"` → `pushEncryptionFilter` → `writeBuffer` →
`newline_before_endstream || (qdf_mode && last_char != '\n')` で `"\n"` → `"endstream"`。

decode level / compress の設定源: `setStreamDataMode`（`libqpdf/QPDFWriter.cc:148-169`）/
`setCompressStreams`（`libqpdf/QPDFWriter.cc:171-176`）/ `setDecodeLevel`
（`libqpdf/QPDFWriter.cc:178-183`）/ `setRecompressFlate`（`libqpdf/QPDFWriter.cc:185-189`）。
`doWriteSetup`（`libqpdf/QPDFWriter.cc:2072-2088`）で pclm → `dl_none` + 非圧縮、qdf →
`normalize_content` 既定 true / `compress_streams` 既定 false / `stream_decode_level` 既定
`qpdf_dl_generalized`。`normalize_content || stream_decode_level || pclm || qdf_mode` なら
`preserve_encryption = false`（`libqpdf/QPDFWriter.cc:2090-2097`）。

`willFilterStream` を **通らない** stream: object stream（`libqpdf/QPDFWriter.cc:1659-1665`、
`compress_streams && !qdf_mode` で `Pl_Flate` 直付け。`/Length` は
`adjustAESStreamLength(length)` 後に直値 `libqpdf/QPDFWriter.cc:1719-1721`）、xref stream
（`libqpdf/QPDFWriter.cc:2422-2432`、`Pl_Flate` + `Pl_PNGFilter`）、hint stream
（`libqpdf/QPDFWriter.cc:2292`, `libqpdf/QPDFWriter.cc:2312`）。

### C-7. `copyStreamData` と `CopiedStreamDataProvider`

- `QPDF::copyStreamData(result, foreign)`（`libqpdf/QPDF.cc:2215-2276`）は foreign copy
  （`replaceForeignIndirectObjects` `libqpdf/QPDF.cc:2158-2201`）と同一 QPDF 内 copy
  （`QPDF::StreamCopier` `include/qpdf/QPDF.hh:784-795` 経由で
  `QPDFObjectHandle::copyStream` `libqpdf/QPDFObjectHandle.cc:2136-2151` から）の両方に使われる。順序:
  1. `copied_stream_data_provider` を遅延生成（`libqpdf/QPDF.cc:2223-2227`、`supports_retry = true`
     `libqpdf/QPDF.cc:126-130`）。
  2. `immediate_copy_from && stream_buffer == nullptr` なら **foreign 側で**
     `replaceStreamData(getRawStreamData(), /Filter, /DecodeParms)` して buffer 化
     （`libqpdf/QPDF.cc:2241-2251`）。
  3. foreign の source に応じて 3 分岐（`libqpdf/QPDF.cc:2254-2275`）: buffer →
     `result.replaceStreamData(buffer, dict/Filter, dict/DecodeParms)`；provider →
     `registerForeignStream(local_og, foreign handle)` + `replaceStreamData(m->copied_streams, ...)`
     （foreign QPDF は生存必須）；original →
     `ForeignStreamData(encp, file, foreign_og, parsed_offset, length, dict)` を登録 +
     `replaceStreamData(m->copied_streams, ...)`。
- `CopiedStreamDataProvider::provideStreamData(og, pipeline, suppress_warnings, will_retry)`
  （`libqpdf/QPDF.cc:132-149`）: `foreign_stream_data[og]` があれば `pipeForeignStreamData`（C-4）、
  無ければ `foreign_streams[og].pipeStreamData(pipeline, nullptr, 0, qpdf_dl_none, suppress_warnings,
  will_retry)`（raw で転送。decode/encode は copy 先の pipe 時に行う）。

### C-8. `replaceStreamData` / `replaceFilterData` の `/Length` 契約と public 面

- `replaceFilterData(filter, decode_parms, length)`（`libqpdf/QPDF_Stream.cc:668-685`）: `filter` /
  `decode_parms` は `isInitialized()` のときだけ `replaceKey`（**未初期化 handle = 現状維持**、null =
  削除。`include/qpdf/QPDFObjectHandle.hh:1080-1084`）。`length == 0` → `/Length` 削除、それ以外 →
  `/Length` を設定。Buffer 版は `data->getSize()`、provider 版は常に 0（→ 削除し、初回 pipe 時に
  `Pl_Count` の実測で補う、C-2 手順 5）。
- `QPDFObjectHandle::replaceStreamData` 5 overload（`libqpdf/QPDFObjectHandle.cc:1344-1429`）: Buffer /
  `std::string`（コピー） / `StreamDataProvider` / `std::function<void(Pipeline*)>` /
  `std::function<bool(Pipeline*, bool, bool)>`。`replaceDict`（`libqpdf/QPDFObjectHandle.cc:1283-1286`
  → `libqpdf/QPDF_Stream.cc:687-692`）は dict を丸ごと差し替え、`setDictDescription` を再設定。
- `setFilterOnWrite` / `getFilterOnWrite`（`libqpdf/QPDF_Stream.cc:154-164`、public
  `libqpdf/QPDFObjectHandle.cc:1264-1274`）、`isDataModified() == !token_filters.empty()`
  （`libqpdf/QPDF_Stream.cc:320-324`）、`addTokenFilter`（`libqpdf/QPDF_Stream.cc:662-666`）。
  `QPDFObjectHandle::filterAsContents`（`libqpdf/QPDFObjectHandle.cc:1761-1767`）は
  `Pl_QPDFTokenizer` を外側に置いて `pipeStreamData(&token_pipeline, 0, qpdf_dl_specialized)`。
  `coalesceContentStreams`（`libqpdf/QPDFObjectHandle.cc:1549-1572`）は `CoalesceProvider`
  （`libqpdf/QPDFObjectHandle.cc:92-118`）を `replaceStreamData(provider, newNull(), newNull())` で
  登録する（provider 経路の代表的な内部利用）。

### C-9. libqpdf 内で `pipeStreamData` / `getStreamData` / `getRawStreamData` を呼ぶ側

`rg -n '\.pipeStreamData\(|\.getStreamData\(|\.getRawStreamData\(|->pipeStreamData\(|->getStreamData\(|->getRawStreamData\('
$Q/libqpdf --glob '*.cc'` の全 24 件のうち、`QPDFObjectHandle.cc` の wrapper 自身
（`libqpdf/QPDFObjectHandle.cc:1291`, `libqpdf/QPDFObjectHandle.cc:1297`,
`libqpdf/QPDFObjectHandle.cc:1309`, `libqpdf/QPDFObjectHandle.cc:1322`）を除いた 20 件:

| 呼び出し元 | decode level / flags | 用途 |
|---|---|---|
| `libqpdf/QPDFWriter.cc:1293` | `willFilterStream` が決める（C-6） | 書き出し |
| `libqpdf/QPDF.cc:144` | `qpdf_dl_none`, retry 引数透過 | `CopiedStreamDataProvider`（C-7） |
| `libqpdf/QPDF.cc:1051`（`QPDF::processXRefStream`） | `getStreamData(qpdf_dl_specialized)` | xref stream 読み出し |
| `libqpdf/QPDF.cc:1792`（`QPDF::resolveObjectsInStream` `libqpdf/QPDF.cc:1756-1800`） | `getStreamData(qpdf_dl_specialized)` | ObjStm 展開（例外は catch しない） |
| `libqpdf/QPDF.cc:2247`（`copyStreamData` の immediate 分岐） | `getRawStreamData()` | C-7 手順 2 |
| `libqpdf/QPDF_Stream.cc:106`（`StreamBlobProvider::operator()` `libqpdf/QPDF_Stream.cc:103-107`） | 呼び出し時の `decode_level` 透過 | JSON inline blob |
| `libqpdf/QPDF_linearization.cc:320`（`QPDF::readHintStream`） | `qpdf_dl_specialized` | hint stream |
| `libqpdf/QPDFObjectHandle.cc:1722`（`QPDFObjectHandle::pipeContentStreams` `libqpdf/QPDFObjectHandle.cc:1708-1730`） | `qpdf_dl_specialized` | page content 連結 |
| `libqpdf/QPDFObjectHandle.cc:1766`（`filterAsContents`） | `qpdf_dl_specialized` | token filter |
| `libqpdf/QPDFPageObjectHelper.cc:521`（`pipeContents`） | `qpdf_dl_specialized` | page contents pipe |
| `libqpdf/QPDFEFStreamObjectHelper.cc:141`（`newFromStream`） | `qpdf_dl_all`, `Pl_Count` | 添付サイズ |
| `libqpdf/QPDFJob.cc:201,216,246`（`ImageOptimizer`） | `qpdf_dl_specialized`（`:201` は probe） | image optimize |
| `libqpdf/QPDFJob.cc:818,825`（`doShowObj`） | `:818` probe `qpdf_dl_all`、`:825` `qpdf_dl_all` + `ef_normalize` | `--show-object` |
| `libqpdf/QPDFJob.cc:925`（`doShowAttachment`） | `qpdf_dl_all` | `--show-attachment` |
| `libqpdf/QPDFJob.cc:1071`（`doJSONPages`） | probe `m->decode_level`, suppress | JSON `filterable` |
| `libqpdf/QPDFJob.cc:1993`（`handleUnderOverlay`） | `getRawStreamData()` → `replaceStreamData(..., uninitialized, uninitialized)` | overlay/underlay の form XObject |
| `libqpdf/qpdf-c.cc:1747`（`qpdf_oh_get_stream_data`） | 引数透過 | C API |

これらはすべて **同じ** `QPDF_Stream::pipeStreamData` を通る。

## route matrix

caller 数は `rg` 出力から数え、宣言行・`use` 行・doc 行を除いた **呼び出し箇所** を数える。
production は `crates/*/src/` の非 `#[cfg(test)]` 部分、test は `tests/` と `#[cfg(test)] mod`。
`crates/flpdf-qtest-tools/src/` は別 crate の production コードなので production に数えるが、
qtest parity harness 専用であることを注記する。

| # | qpdf responsibility owner | qpdf evidence | flpdf current entrypoint | callers (prod / test) | classification | canonical owner | remaining bridge callers / notes |
|---|---|---|---|---|---|---|---|
| C1 | `QPDF_Stream::pipeStreamData` の filter 段構築（手順 1-4, 6） | `libqpdf/QPDF_Stream.cc:487-569,624-637` | `crates/flpdf/src/object_handle.rs::pipe_stream_data_inner`（private, `crates/flpdf/src/object_handle.rs:6410-6588`） | prod: 2 (`crates/flpdf/src/object_handle.rs:6177,6202` = `pipe_stream_data` と `pipe_stream_data_for_object_stream`) / test: 0 | canonical | `crates/flpdf/src/object_handle.rs::pipe_stream_data_inner` | 逆順構築・decode level gate・normalizer 警告の遅延発行まで qpdf 通り。qpdf に無い第 7 引数 `recover_codec_errors` を持つ（C4 行） |
| C2 | `QPDF_Stream::pipeStreamData` の source dispatch（手順 5）と provider `/Length` 検証 | `libqpdf/QPDF_Stream.cc:571-622` | `crates/flpdf/src/object_handle.rs::pipe_stream_source`（private, `crates/flpdf/src/object_handle.rs:6767-6865`） | prod: 5 (`crates/flpdf/src/object_handle.rs:6457,6470,6484,6559,6754`、すべて同ファイル内) / test: 0 | canonical | `crates/flpdf/src/object_handle.rs::pipe_stream_source` | 3 source の優先順位・`Pl_Count`・`supports_retry` 分岐・`/Length` 不一致の `Error::System`・`parsed_offset == 0` の `Error::Internal` が 1:1 |
| C3 | `QPDFObjectHandle::pipeStreamData`（6 引数 / 5 引数 overload） | `libqpdf/QPDFObjectHandle.cc:1300-1325`, `include/qpdf/QPDFObjectHandle.hh:1041-1059` | `crates/flpdf/src/object_handle.rs::pipe_stream_data`（`pub`, `crates/flpdf/src/object_handle.rs:6168-6186`） | prod: 24 (13 files; うち 5 は flpdf-qtest-tools = parity harness) / test: 67 (3 files) | canonical | `crates/flpdf/src/object_handle.rs::pipe_stream_data` | flpdf は 6 引数版だけを持ち、5 引数版（success を捨てて `filtering_attempted` を返す）に対応する overload が無い。呼び出し側 `writer/plain/body.rs:838-841` が `filtering_attempted && success` を計算して同じ値に到達する（`pipe_stream_data_inner` も失敗時に `filtering_attempted` を落とす: `crates/flpdf/src/object_handle.rs:6577-6579`）ので観測値は同じ。legacy 4 引数版（`libqpdf/QPDFObjectHandle.cc:1327-1342`）にも対応物なし |
| C4 | 同上（ObjStm resolve 経路） | `libqpdf/QPDF.cc:1792`（`getStreamData(qpdf_dl_specialized)` を catch せず呼ぶ） | `crates/flpdf/src/object_handle.rs::pipe_stream_data_for_object_stream`（`pub(crate)`, `crates/flpdf/src/object_handle.rs:6193-6211`） | prod: 1 (`crates/flpdf/src/reader/resolver.rs:2188`) / test: 0 | mixed | `crates/flpdf/src/object_handle.rs::pipe_stream_data` | qpdf は ObjStm でも通常の `getStreamData` を呼ぶだけで、codec error 回復の分岐を持たない。flpdf は `recover_codec_errors = true` で `PipelineError::Runtime` を `Error::Unsupported("error decoding stream data: …")` へ写す（`crates/flpdf/src/object_handle.rs:6867-6877`）。resolver 側は `getStreamData` の unfilterable 判定もインライン展開している（`crates/flpdf/src/reader/resolver.rs:2186-2201`） |
| C5 | `QPDF_Stream::getStreamData` | `libqpdf/QPDF_Stream.cc:344-360` | `crates/flpdf/src/object_handle.rs::get_stream_data`（`pub`, `crates/flpdf/src/object_handle.rs:6223-6252`） | prod: 19 (11 files; うち 15 hits は flpdf-qtest-tools = parity harness、残り 4 は flpdf-cli `main.rs:6153`、`crates/flpdf/src/job/inspection.rs:102,210`、`crates/flpdf/src/linearization/check.rs:1612`) / test: 29 (12 files) | canonical | `crates/flpdf/src/object_handle.rs::get_stream_data` | `!filtering_attempted` の `QPDFExc` 相当（filename + parsed offset 付き）まで写している。qpdf に無い `stream_data_succeeded` 判定を追加している（`crates/flpdf/src/object_handle.rs:6246-6250`）が、qpdf の 6 引数 `pipeStreamData` は同条件で `filtered=false` も返すため観測差は無い |
| C6 | `QPDF_Stream::getRawStreamData` | `libqpdf/QPDF_Stream.cc:362-376` | `crates/flpdf/src/object_handle.rs::get_raw_stream_data`（`pub`, `crates/flpdf/src/object_handle.rs:6720-6728`） | prod: 19 (14 files; うち 5 は flpdf-qtest-tools) / test: 71 (15 files) | canonical | `crates/flpdf/src/object_handle.rs::get_raw_stream_data` | 内部は `pipe_raw_stream_data`（`crates/flpdf/src/object_handle.rs:6730-6764`）→ `pipe_stream_source` で、filter 段を作らない点まで qpdf 通り |
| C7 | `QPDF_Stream::filterable`（factory lookup + `/DecodeParms` 整合 + `setDecodeParms`） | `libqpdf/QPDF_Stream.cc:378-485` | `crates/flpdf/src/object_handle.rs::prepare_stream_filter_plan`（private, `crates/flpdf/src/object_handle.rs:6609-6713`） | prod: 1 (`crates/flpdf/src/object_handle.rs:6469`) / test: 0 | mixed | `crates/flpdf/src/object_handle.rs::prepare_stream_filter_plan` | filter判定の対応範囲: `/Filter` shape → `FILTER_TYPE_ERROR` warning + false（`libqpdf/QPDF_Stream.cc:411-415`）、factory lookup を `/DecodeParms` 読み取りより先（`libqpdf/QPDF_Stream.cc:419-435`）、空 `/DecodeParms` 配列 → null 複製（`libqpdf/QPDF_Stream.cc:443-454`）、長さ不一致 warning（`libqpdf/QPDF_Stream.cc:458-461`）、`setDecodeParms` false → 全体 false（`libqpdf/QPDF_Stream.cc:467-482`）。**sourceで確認した責務差**: qpdf は `filterable` の warning も `Pl_Flate` の warning も `QPDF_Stream::warn`（`libqpdf/QPDF_Stream.cc:694-698`、`parsed_offset` 付き）1 本に流すが、flpdf は前者を `object_warning`、後者を `stream_data_warning`（`crates/flpdf/src/object_handle.rs:6599-6607`）と別経路にしている |
| C8 | 同上（decode level gate 用の capability 判定だけを切り出したもの） | `libqpdf/QPDF_Stream.cc:467-482,508-513` | `crates/flpdf/src/object_handle.rs::prepare_stream_filter_plan`（private, `crates/flpdf/src/object_handle.rs:6609-6719`） | prod: 1 (`crates/flpdf/src/object_handle.rs:6469`) / test: 0 | canonical | `crates/flpdf/src/object_handle.rs::prepare_stream_filter_plan` | 旧 `filters::stream_filter_capabilities` は `.40` で撤去。これは旧routeの完了記録であり、qpdf `filterable` 全責務の完了を意味しない。`parsed_offset` warning経路の分裂は `.48.37` で追跡する。 |
| C9 | 同上（whole-buffer decode 用の spec 読み取り） | `libqpdf/QPDF_Stream.cc:386-461` | `crates/flpdf/src/stream_filter.rs::decode_filter_specs_from_handle`（`pub(crate)`, `crates/flpdf/src/stream_filter.rs:320`） | prod: 3 (`crates/flpdf/src/filters.rs:46,357,456`) / test: 0 | mixed | `crates/flpdf/src/object_handle.rs::prepare_stream_filter_plan` | `filterable` の 3 つ目の実装。qpdf に無い chain 長上限 `MAX_FILTER_CHAIN_LEN = 16`（`crates/flpdf/src/filters.rs:14-20` が意図的逸脱と明記）を持ち、shape 違反を warning ではなく `Error::Unsupported` にする |
| C10 | `QPDF_Stream::filter_factories` の built-in factory lookup | `libqpdf/QPDF_Stream.cc:85-94,419-435` | `crates/flpdf/src/stream_filter.rs::stream_filter_for`（`pub(crate)`, `crates/flpdf/src/stream_filter.rs:1064-1076`） | prod: 6 (`crates/flpdf/src/stream_filter.rs:98,122,1156`, `crates/flpdf/src/filters.rs:635,775`, `crates/flpdf/src/object_handle.rs:6667`) / test: 0 | canonical | `crates/flpdf/src/stream_filter.rs::stream_filter_for` | built-in lookup は qpdf と同じ7名称（`Crypt`/`FlateDecode`/`LZWDecode`/`ASCII85Decode`/`ASCIIHexDecode`/`RunLengthDecode`/`DCTDecode`）。runtime登録APIは親issue `.48.46` の後続scopeであり、このsliceでは既存matchを維持する。`FilterSpec`はnative `ObjectHandle`を保持し、縮約DecodeParams/snapshot層は削除済み。`registerStreamFilter`との差は 🔀 として残す。 |
| C11 | `QPDFStreamFilter` 抽象（`setDecodeParms` / `getDecodePipeline` / `isSpecializedCompression` / `isLossyCompression`） | `include/qpdf/QPDFStreamFilter.hh:26-66`, `libqpdf/QPDFStreamFilter.cc:1-19` | `crates/flpdf/src/stream_filter.rs::StreamFilter`（trait, `pub(crate)`, `crates/flpdf/src/stream_filter.rs:410-467`） | prod: 実装7種（`stream_filter_for`の返り値経由） / test: 同ファイル内unit tests | mixed | `crates/flpdf/src/stream_filter.rs::StreamFilter` | 既定`set_decode_params`はqpdfの`decode_parms.isNull()`をnative `ObjectHandle`で写し、Flate/LZWとCryptはfull handleをqpdf順に検査する。built-in consumerは移行済みだが、qpdfのpublic runtime registration contractは親issue `.48.46` の後続scopeで未完のためmixedを維持する。旧 `DecodeParams` owned snapshot、`ParamValue`、retention helperは削除済み。whole-bufferの`pipe_decode_recovering`はqpdfにないflpdf内部実行面で、既存のqpdf-deviation記録を維持する。 |
| C12 | `QPDF::Pipe::pipeStreamData` / static `QPDF::pipeStreamData`（original source 読み出し + error boundary） | `include/qpdf/QPDF.hh:819-839`, `libqpdf/QPDF.cc:2476-2562` | `crates/flpdf/src/reader/resolver.rs::pipe_stream_data`（`pub(crate)`）→ `crates/flpdf/src/reader/resolver.rs::pipe_stream_data_from_input`（private） | `pipe_stream_data_from_input` は通常の source と C13 の foreign provider から呼ばれる。`ResolverHandle::pipe_stream_data` は `DocumentResolver` trait impl 越しに `ObjectHandle` から呼ばれる | canonical | `crates/flpdf/src/reader/resolver.rs::pipe_stream_data_from_input` | 復号前置 → seek → 一括 read → write/finish → 失敗時 warning の順序と 2 種の catch arm が 1:1。qpdf と同じく recovered stream framing を含む caller の `length` を変更せず、C42 の EOL subtraction は `flpdf-zvjf` で除去した |
| C13 | `QPDF::pipeForeignStreamData` / `ForeignStreamData` | `libqpdf/QPDF.cc:110-124,2564-2585` | `crates/flpdf/src/reader/resolver.rs::original_stream_data_provider_for_destination`（`pub(crate)`, `crates/flpdf/src/reader/resolver.rs:1096`） | prod: 2 (`crates/flpdf/src/reader/resolver.rs:1073,1089`。`crates/flpdf/src/object_handle.rs:423` は `DocumentResolver` trait の既定メソッド宣言で caller ではない) / test: 3 (`crates/flpdf/src/reader/resolver.rs:4828,4902`, `crates/flpdf/src/object_handle.rs:8726`) | canonical | `crates/flpdf/src/reader/resolver.rs::original_stream_data_provider_for_destination` | source 側の `StreamInput`/encryption state/objgen/offset/length と destination dictionary を凍結し、destination resolver を warning sink にする点まで qpdf 通り（`description_override` が qpdf の `file` 引数、`crates/flpdf/src/reader/resolver.rs:3655-3662`） |
| C14 | `QPDF::decryptStream`（method 決定 + AES/RC4 前置） | `libqpdf/QPDF_encryption.cc:1044-1154`（唯一の呼び出し元 `libqpdf/QPDF.cc:2491`） | `crates/flpdf/src/reader/resolver.rs::inspect_stream_encryption`（private, `crates/flpdf/src/reader/resolver.rs:3959-4034`）+ `pipe_stream_data_from_input` の復号分岐（`crates/flpdf/src/reader/resolver.rs:3693-3807`） | `inspect_stream_encryption` prod: 2 (`crates/flpdf/src/reader/resolver.rs:3693` = pipe 経路、`crates/flpdf/src/reader/resolver.rs:3479` = `recovered_stream_eol_is_transformed`) / test: 0 | canonical | `crates/flpdf/src/reader/resolver.rs::pipe_stream_data_from_input` | **復号は pipe 時のみ**（resolve 時ではない）という qpdf の境界を保持。`/XRef` early return、V≥4 gate、`/Crypt` の dict/array 両形、Metadata より Crypt 優先、unknown warning + `cf_stream` 書き換えまで 1:1（`docs/qpdf-correspondence.md:317` の ✅ と一致） |
| C15 | `QPDF::decryptString`（parse 時の文字列復号） | `libqpdf/QPDF_encryption.cc:976-1039`, `libqpdf/QPDF.cc:1330-1340` | `crates/flpdf/src/reader/resolver.rs::decrypt_string`（`StringDecrypter` impl, `crates/flpdf/src/reader/resolver.rs:788`） | prod: parse 経路 (`crates/flpdf/src/reader/resolver.rs:3038-3043`) / test: 同ファイル | canonical | `crates/flpdf/src/reader/resolver.rs::decrypt_string` | stream と string で復号タイミングが違うという qpdf の構造を保持（`crates/flpdf/src/reader/resolver.rs:125-129` が明記） |
| C16 | `QPDF::getKeyForObject`（1 object 分の key cache） | `libqpdf/QPDF_encryption.cc:954-974` | `crates/flpdf/src/encryption/state.rs::key_for_object`（`pub(crate)`, `crates/flpdf/src/encryption/state.rs:185-191`） | prod: 3 (`crates/flpdf/src/reader/resolver.rs:3741,3744`, `crates/flpdf/src/encryption/state.rs:172`) / test: 7 (`crates/flpdf/src/reader/resolver.rs` の unit tests) | canonical | `crates/flpdf/src/encryption/state.rs::key_for_object` | cache key が `og` のみで `use_aes` を含まない点まで qpdf 通り（`crates/flpdf/src/encryption/state.rs:183-184` に明記） |
| C17 | `QPDF::compute_data_key`（読み書き共有の 1 実装） | `libqpdf/QPDF_encryption.cc:324-357`, `include/qpdf/QPDF.hh:551`（呼び出し元は `libqpdf/QPDF_encryption.cc:963` と `libqpdf/QPDFWriter.cc:845` の 2 箇所） | 共有 primitive `crates/flpdf/src/encryption/primitives.rs::compute_data_key`（`pub(crate)`）→ reader `EncryptionState::key_for_object` が `(obj,gen)` cache miss 時に呼ぶ | prod: reader 1（`crates/flpdf/src/encryption/state.rs::key_for_object`）+ writer 1（`crates/flpdf/src/writer/encryption_state.rs::set_data_key`） / test: 1（qpdf oracle vectors） | canonical | `crates/flpdf/src/encryption/primitives.rs::compute_data_key` | qpdf と同じ 1 実装を読み書きで共有。V≥5 は file key 直返し、V<5 は key + objid 下位 3 byte + gen 下位 2 byte（+ AES `sAlT`）を MD5 し `min(連結後入力長, 16)` に切る。V/R・key長5/16/24/32・AES/RC4・非zero generation の C++ qpdf oracle vectors を固定 |
| C18 | `QPDFWriter::setDataKey`（共有 `QPDF::compute_data_key` の writer consumer） | `libqpdf/QPDFWriter.cc:842-847` | `crates/flpdf/src/writer/encryption_state.rs::WriterEncryptionState::set_data_key`（private、generation 0 を渡して共有 primitive を呼ぶ） | prod: 1 (`set_data_key`) / test: lifecycle tests + C17 oracle vectors | canonical | `crates/flpdf/src/writer/encryption_state.rs::WriterEncryptionState` | writer は qpdf の emitted object ID と generation 0 の call contract を保持し、ObjStm member を個別計算しない。reader cache 境界も `EncryptionState::key_for_object` に保持し、旧 writer duplicate primitive は削除 |
| C20 | `QPDFWriter::willFilterStream` の policy 判定（手順 1-5） | `libqpdf/QPDFWriter.cc:1238-1285`, `include/qpdf/QPDFWriter.hh:488-492` | `crates/flpdf/src/writer/plain/body.rs::canonical_stream_filter_plan`（private, `crates/flpdf/src/writer/plain/body.rs:921-989`） | prod: 2 (`crates/flpdf/src/writer/plain/body.rs:720,804`) / test: 0 | canonical | `crates/flpdf/src/writer/plain/body.rs::canonical_stream_filter_plan` | `getFilterOnWrite()` veto を metadata/normalize/compress より先に置く順序（`libqpdf/QPDFWriter.cc:1255-1259`）、metadata/normalize/compress の排他 if-else chain（`libqpdf/QPDFWriter.cc:1274-1285`）、lone-Flate 非再圧縮（`libqpdf/QPDFWriter.cc:1260-1271`）を保持。qpdf に対応物の無い `content_normalization_applied` marker を追加で参照する（`crates/flpdf/src/writer/plain/body.rs:942-943`） |
| C21 | `QPDFWriter::willFilterStream` の 2 回試行・buffer 保持と `unparseObject` の出力辞書加工 | `libqpdf/QPDFWriter.cc:1287-1315,1440-1486` | pipe/cache owner: `crates/flpdf/src/writer/plain/body.rs::canonical_stream_output_with_rewrite_policy`（private, `crates/flpdf/src/writer/plain/body.rs:914`）; dictionary owner: `crates/flpdf/src/writer/object.rs::prepare_stream_dict_entries`（private, `crates/flpdf/src/writer/object.rs:1183`） | plain / specialized / linearized consumers carry `StreamDictionaryOptions`; `plain::plan` cache reuses the same dictionary policy and provider bytes | canonical | `crates/flpdf/src/writer/object.rs::prepare_stream_dict_entries` | `filtering_attempted` と `add_flate_filter` を分離し、成功したfilter時は`/Filter`/`/DecodeParms`だけを削除、compression成功時だけFlateを追加する。unfiltered branchは空`/DecodeParms`と`/Crypt`だけをqpdf順で処理し、`/F` `/FFilter` `/FDecodeParms`を保持する。library RED/GREEN + C++ oracle `qpdf_refiltered_stream_dictionary_probe.cc` はrefilter/decode/filter-on-write veto/metadata/token-filter/provider retryの辞書と呼出順を比較する。 |
| C22 | `QPDFWriter::willFilterStream` の probe 呼び（`skip_stream_parameters`） | `libqpdf/QPDFWriter.cc:2543-2551` | `crates/flpdf/src/writer/plain/body.rs::canonical_stream_filter_probe`（private, `crates/flpdf/src/writer/plain/body.rs:714-754`） | prod: 2 (`crates/flpdf/src/writer/plain/body.rs:688,711`) / test: 0 | mixed | `crates/flpdf/src/writer/plain/body.rs::canonical_stream_filter_plan` | plain 側の入口 `canonical_stream_will_be_refiltered_with_policy`（`crates/flpdf/src/writer/plain/body.rs:675-694`）は `is_data_modified()` なら probe 前に `false` を返す。qpdf の `willFilterStream` にこの早期 return は無く、`isDataModified()` は逆に `filter` を **立てる** 入力である（`libqpdf/QPDFWriter.cc:1254`）。linearized 側 `canonical_stream_filter_probe_for_linearization`（`crates/flpdf/src/writer/plain/body.rs:706-712`）はこの早期 return を通らない 。plain / QDF は現在全 indirect stream の出力を cache し、qpdf の linearize にだけ optimizer 事前 probe があるため、この非対称だけで不一致とは判定しない（U3） |
| C24 | `QPDF_Stream::writeStreamJSON` の 2 回試行 + dict 正規化 | `libqpdf/QPDF_Stream.cc:206-296` | `crates/flpdf/src/object_handle.rs::write_stream_json`（`pub(crate)`, `crates/flpdf/src/object_handle.rs:6268-6409`） | prod: 2 (`crates/flpdf/src/document_json.rs:363,451`) / test: 8 (`crates/flpdf/src/object_handle.rs` の unit tests) | canonical | `crates/flpdf/src/object_handle.rs::write_stream_json` | mode 検証の 3 つの `std::logic_error` 文言、`no_data_key`、`attempt 1..2` で `decode_level` を落とす retry、`/Length` 削除と `filter && filtered` での `/Filter` `/DecodeParms` 削除が 1:1（`docs/qpdf-correspondence.md:715` の ✅ と一致） |
| C25 | `QPDF_Stream::pipeStreamData` の decode-level 判断 + decode（JSON payload 用） | `libqpdf/QPDF_Stream.cc:504-513,571-622` | `crates/flpdf/src/object_handle.rs::pipe_stream_data` / `::get_stream_data`（`crates/flpdf/src/object_handle.rs:6173-6181,6228-6252`） | prod: canonical callers（writer/inspection等） / test: canonical stream unit tests。旧 `json_inspect` payload wrappersは `.40` で撤去 | canonical | `crates/flpdf/src/object_handle.rs::pipe_stream_data` | 旧 `stream_payload_with_decode_status` は、`get_raw_stream_data` + capability判定 + whole-buffer decoder の重複経路であり削除。raw取得は `get_raw_stream_data`、decode-level gateとdecodeは `get_stream_data`/`pipe_stream_data`、filtered→raw retryはC24 `write_stream_json`が所有する。 |
| C26 | （C25 の下請け）filter chain の whole-buffer decode | `libqpdf/QPDF_Stream.cc:487-638`（qpdf に whole-buffer 専用 route は無い） | `crates/flpdf/src/filters.rs::decode_stream_data`（`pub`, `crates/flpdf/src/filters.rs:75-82`） | prod: 3 (`crates/flpdf-qtest-tools/src/compare.rs:85,89`, `crates/flpdf-qtest-tools/src/driver/test_34_41.rs:519`。parity harnessだが、`#[cfg(test)]`（`compare.rs:299` / `test_34_41.rs:814`）より前にあるので production に数える) / test: 1 | mixed | `crates/flpdf/src/object_handle.rs::pipe_stream_data` | flpdf 本体の残 production caller は parity harness の 3 件。C25の旧json_inspect consumerは`.40`でcanonical stream APIへ移行・撤去した。source dispatchも復号もtoken filterも持たず、呼び出し側が別途取得した `&[u8]` を受け取る |
| C27 | 同上（`ObjectHandle`-native 版） | `libqpdf/QPDF_Stream.cc:487-638` | `crates/flpdf/src/filters.rs::decode_stream_data_from_handle`（`pub(crate)`, `crates/flpdf/src/filters.rs:405-409`） | prod: 4 (`crates/flpdf/src/filespec_helper/embedded_file_stream.rs:273`, `crates/flpdf/src/resources.rs:269`, `crates/flpdf/src/xref.rs:752,3266`) / test: 0 | mixed | `crates/flpdf/src/object_handle.rs::pipe_stream_data` | `docs/qpdf-correspondence.md:312` の「production 経路の `decode_stream_data`/`encode_stream_data` 呼び出しはテストコードのみに残る」に反する現存 production caller。現在の `crates/flpdf/src/xref.rs:751` は bootstrap ObjStm（qpdf `libqpdf/QPDF.cc:1792`）、`crates/flpdf/src/xref.rs:3381` は xref stream（qpdf `libqpdf/QPDF.cc:1051`）。ともに qpdf は `getStreamData(qpdf_dl_specialized)` を使う |
| C28 | 同上（recovering 版・explicit limits 版の公開 API） | 対応物なし（qpdf は部分デコード結果を返す API を持たない） | `crates/flpdf/src/filters.rs::decode_stream_data_recovering`（`pub`, `crates/flpdf/src/filters.rs:187-192`）/ `::decode_stream_data_recovering_with_limits`（`pub`, `crates/flpdf/src/filters.rs:202-208`） | `decode_stream_data_recovering` prod: 0 / test: 1；`decode_stream_data_recovering_with_limits` prod: 2 (2 files) / test: 2 | bridge | `crates/flpdf/src/object_handle.rs::pipe_stream_data` | **route と責務を分けて読む**: この経路は責務のレベルでも qpdf に対応物が無い（qpdf は部分デコード結果を返す API を持たない）ので、単純な `bridge`。公開 wrapper 内部の委譲を除く実 production consumer は flpdf-qtest-tools の test 0 のみで、canonical pipe/logger への移行対象。`DecodeLimits`（`crates/flpdf/src/filters.rs:222-235`）は qpdf に対応物のない hardening budget。旧whole-buffer limits wrapperは`.42`で削除 |
| C29 | `pipeStreamData` の `qpdf_ef_compress`（deflate 段） | `libqpdf/QPDF_Stream.cc:536-542` | `crates/flpdf/src/filters.rs::encode_stream_data`（`pub`, `crates/flpdf/src/filters.rs:327-329`）/ `::encode_stream_data_from_handle`（`pub(crate)`, `crates/flpdf/src/filters.rs:351-359`） | `encode_stream_data` prod: 1 (`crates/flpdf-qtest-tools/src/driver/test_02_09.rs:516`) / test: 3。`encode_stream_data_from_handle` prod: 4 (`crates/flpdf/src/overlay_appearance_stream.rs:172,183`, `crates/flpdf/src/filters.rs:328`, `crates/flpdf/src/writer/object_streams/emission.rs:171`) / test: 0 | mixed | `crates/flpdf/src/object_handle.rs::pipe_stream_data`（ordinary stream）/ `crates/flpdf/src/writer/object_streams/emission.rs::wrap_objstm_body_as_handle`（ObjStm、C31） | `emission.rs:171` の利用は qpdf 側でも `pipeStreamData` を通らない ObjStm 経路（`libqpdf/QPDFWriter.cc:1659-1665`）なので責務としては正しい。現在の `overlay_appearance_stream.rs:210-235` の owner は `QPDFAcroFormDocumentHelper::adjustAppearanceStream`。qpdf は `parseAsContents` 後に `ResourceReplacer` を `addTokenFilter` する（`libqpdf/QPDFAcroFormDocumentHelper.cc:680-690`）ので、eager 再エンコードに対応物は無い |
| C30 | `QPDFWriter::unparseObject` の stream 出力（`pushEncryptionFilter` + `adjustAESStreamLength` + endstream 改行） | `libqpdf/QPDFWriter.cc:965-999,1528-1566` | `crates/flpdf/src/writer.rs::write_stream_payload_with_pipeline`（`pub(crate)`, `crates/flpdf/src/writer.rs:3245-3264`）/ `::write_stream_payload_with_pipeline_qdf`（`crates/flpdf/src/writer.rs:3268-3290`）/ `::adjust_aes_stream_length`（`crates/flpdf/src/writer.rs:3140`） | `write_stream_payload_with_pipeline` prod: 3 (`crates/flpdf/src/writer.rs:5042`, `crates/flpdf/src/linearization/writer.rs:379,678`) / test: 0。`adjust_aes_stream_length` prod: 4 (`crates/flpdf/src/writer.rs:4646,4992`, `crates/flpdf/src/linearization/writer.rs:353,652`) / test: 1 | canonical | `crates/flpdf/src/writer.rs::write_stream_payload_with_pipeline_qdf` | plain / linearized の両 route が同じ primitive を共有する。`newline_before_endstream` または `qdf_mode && last_char != '\n'` で改行を足す規則（`libqpdf/QPDFWriter.cc:1560-1565`）と `+ 32 - (len & 0xf)`（`libqpdf/QPDFWriter.cc:965-973`）が 1:1 |
| C31 | `QPDFWriter::writeObjectStream` の ObjStm 圧縮（`willFilterStream` を通らない） | `libqpdf/QPDFWriter.cc:1659-1665,1719-1721` | `crates/flpdf/src/writer/object_streams/emission.rs::wrap_objstm_body_as_handle`（`pub(crate)`, `crates/flpdf/src/writer/object_streams/emission.rs:159-210`） | prod: 3 (`crates/flpdf/src/writer/serialize.rs:77`, `crates/flpdf/src/writer.rs:4973`, `crates/flpdf/src/linearization/writer.rs:340`) / test: 1 (`crates/flpdf/src/writer/object_streams/emission.rs:226`) | canonical | `crates/flpdf/src/writer/object_streams/emission.rs::wrap_objstm_body_as_handle` | plain / linearized の両 route が同じ関数を使う。qpdf と同じく `pipeStreamData` を通さず deflate を直付けする |
| C32 | xref stream の payload 生成（`Pl_PNGFilter` + `Pl_Flate`、`willFilterStream` を通らない） | `libqpdf/QPDFWriter.cc:2422-2432` | `crates/flpdf/src/writer/serialize.rs::encode_payload`（`pub(crate)`, `crates/flpdf/src/writer/serialize.rs:203-227`）と `::encode_payload_raw` / `::encode_payload_uncompressed` | `encode_payload` prod: 3 (`crates/flpdf/src/writer/plain/xref.rs:140`, `crates/flpdf/src/linearization/writer.rs:1557,1661`) / test: 0 | canonical | `crates/flpdf/src/writer/serialize.rs::encode_payload` | 3 変種は qpdf の `compress_streams`/`skip_compression` の 3 状態に対応（pass-1 の predictor-only は `libqpdf/QPDFWriter.cc:2426-2430` の `skip_compression` 分岐）。plain / linearized が同じ 3 関数を共有 |
| C33 | hint stream の生成（`willFilterStream` を通らない） | `libqpdf/QPDFWriter.cc:2286-2330` | `crates/flpdf/src/linearization/hint_stream.rs::encode_hint_stream`（`pub`, `crates/flpdf/src/linearization/hint_stream.rs:520-531`） | prod: 1 (`crates/flpdf/src/linearization/writer.rs:4333`) / test: 17 (`crates/flpdf/src/linearization/hint_stream.rs` と `crates/flpdf/src/linearization/show.rs`) | canonical | `crates/flpdf/src/linearization/hint_stream.rs::encode_hint_stream` | `compressed = compress_streams && !qdf_mode` と `adjustAESStreamLength(hlen)` の順序は `crates/flpdf/src/linearization/writer.rs` 側が保持 |
| C34 | `QPDF::copyStreamData`（3 分岐 + `immediate_copy_from`） | `libqpdf/QPDF.cc:2215-2276` | `crates/flpdf/src/reader/resolver.rs::copy_stream_data`（`pub(crate)`, `crates/flpdf/src/reader/resolver.rs:1016-1081`） | prod: 3 (`crates/flpdf/src/object_copy.rs:559`, `crates/flpdf/src/object_handle.rs:5099`, `crates/flpdf/src/reader/resolver.rs:4241` = trait impl の委譲。`crates/flpdf/src/object_handle.rs:397` は trait 既定メソッド宣言) / test: 2 | canonical | `crates/flpdf/src/reader/resolver.rs::copy_stream_data` | buffer / provider / original の 3 分岐と `immediate_copy_from` を source 側 resolver から見る点まで 1:1（`docs/qpdf-correspondence.md:260` の記載と一致） |
| C35 | `QPDF::CopiedStreamDataProvider` | `libqpdf/QPDF.cc:126-163` | `crates/flpdf/src/object_handle.rs::copied_stream_data_provider`（`pub(crate)`, `crates/flpdf/src/object_handle.rs:318-320`） | prod: 1 (`crates/flpdf/src/reader/resolver.rs:1063`) / test: 0 | canonical | `crates/flpdf/src/object_handle.rs::copied_stream_data_provider` | `supports_retry = true`、`qpdf_dl_none` で raw 転送、retry 引数透過が 1:1 |
| C36 | `QPDFObjectHandle::copyStream` | `libqpdf/QPDFObjectHandle.cc:2136-2151`, `include/qpdf/QPDF.hh:784-795` | `crates/flpdf/src/object_handle.rs::copy_stream`（`pub`, `crates/flpdf/src/object_handle.rs:5074`） | prod: 4 (`crates/flpdf/src/acroform_document_helper.rs:2111,2126`, `crates/flpdf/src/job/image_optimization.rs:131`, `crates/flpdf-qtest-tools/src/driver/test_72_79.rs:815` = parity harness) / test: 1 | canonical | `crates/flpdf/src/object_handle.rs::copy_stream` | `StreamCopier` 経由で `copy_stream_data`（C34）に委譲する構造まで一致 |
| C37 | `QPDFObjectHandle::StreamDataProvider` 基底（既定実装 + `supportsRetry`） | `include/qpdf/QPDFObjectHandle.hh:72-127`, `libqpdf/QPDFObjectHandle.cc:48-90` | `crates/flpdf/src/object_handle.rs::StreamDataProvider`（trait, `pub`） | prod: 実装 4 種（`CallbackProvider`/`RetryCallbackProvider`/`CopiedStreamDataProvider`/`CoalesceContentProvider`、`crates/flpdf/src/object_handle.rs:230-360`）+ 外部実装 / test: 20 | canonical | `crates/flpdf/src/object_handle.rs::StreamDataProvider` | `QPDFObjGen` 版 → `(int,int)` 版の委譲と既定 `logic_error` を trait 既定メソッドへ写している（`docs/qpdf-correspondence.md:261` の ✅） |
| C38 | `QPDFObjectHandle::replaceStreamData` 5 overload + `QPDF_Stream::replaceFilterData` の `/Length` 契約 | `libqpdf/QPDFObjectHandle.cc:1344-1429`, `libqpdf/QPDF_Stream.cc:640-660,668-685` | `crates/flpdf/src/object_handle.rs::replace_stream_data`（`pub`, `crates/flpdf/src/object_handle.rs:5789-5809`）/ `::replace_stream_data_provider`（`crates/flpdf/src/object_handle.rs:5831-5864`）/ `::replace_stream_data_with_callback` / `::replace_stream_data_with_retry_callback`、共有 `::replace_filter_data`（private, `crates/flpdf/src/object_handle.rs:5941-5964`） | `replace_stream_data` prod: 16 (10 files) / test: 15。`replace_stream_data_provider` prod: 10 (7 files) / test: 20。`replace_stream_data_with_retry_callback` prod: 1 (`crates/flpdf-qtest-tools/src/driver/test_72_79.rs:685` = parity harness のみ) / test: 7 | canonical | `crates/flpdf/src/object_handle.rs::replace_filter_data` | `length == 0` → `/Length` 削除、`filter`/`decode_parms` が未初期化なら現状維持という 2 つの契約を 1 箇所に集約している。qpdf の `std::string` overload に対応する専用 API は無い（`Rc<Vec<u8>>` 版に集約） |
| C39 | `QPDF_Stream::setFilterOnWrite` / `getFilterOnWrite` / `isDataModified` / `addTokenFilter` | `libqpdf/QPDF_Stream.cc:154-164,320-324,662-666`, `libqpdf/QPDFObjectHandle.cc:1264-1274` | `crates/flpdf/src/object_handle.rs::set_filter_on_write` / `::get_filter_on_write` / `::is_data_modified`（`pub(crate)`）/ `::add_token_filter`（`pub`） | `get_filter_on_write` prod: 1 (`crates/flpdf/src/writer/plain/body.rs:935`) / test: 5。`is_data_modified` prod: 5 (`crates/flpdf/src/writer/plain/body.rs:685,969,973`, `crates/flpdf/src/writer/plain/plan.rs:138`, `crates/flpdf/src/linearization/plan.rs:158`) / test: 2。`add_token_filter` prod: 4 (`crates/flpdf/src/form_field_object_helper/rendering.rs:388`, `crates/flpdf/src/object_handle.rs:6147`, `crates/flpdf/src/page_object_helper.rs:1288`, `crates/flpdf-qtest-tools/src/driver/test_72_79.rs:241`) / test: 4 | canonical | `crates/flpdf/src/object_handle.rs::set_filter_on_write` 他（qpdf と同じく `QPDF_Stream` state の直接アクセサ） | `docs/qpdf-correspondence.md:262`（✅）と一致。`is_data_modified` の consumer が writer 2 route + plan 2 route に分かれるのは C22 の早期 return と同じ差の現れ |
| C40 | `QPDFObjectHandle::coalesceContentStreams` / `CoalesceProvider` / `pipeContentStreams` / `filterAsContents` | `libqpdf/QPDFObjectHandle.cc:92-118,1549-1572,1708-1730,1761-1767` | `crates/flpdf/src/object_handle.rs::coalesce_content_streams` / `::pipe_content_streams` / `::filter_as_contents`（すべて `pub`） | `coalesce_content_streams` prod: 5 (4 files, flpdf-cli `main.rs:4405` 含む) / test: 7。`pipe_content_streams` prod: 4 (3 files) / test: 1。`filter_as_contents` prod: 1 (`crates/flpdf/src/page_object_helper.rs:1247`) / test: 2 | canonical | `crates/flpdf/src/object_handle.rs::coalesce_content_streams` | provider 経路の内部利用（`replaceStreamData(provider, newNull(), newNull())`）まで写している |
| C41 | `QPDF::readStream` の `/Length` 検証 + `endstream` 確認 | `libqpdf/QPDF.cc:1361-1399` | `crates/flpdf/src/reader/resolver.rs::read_stream`（private, `crates/flpdf/src/reader/resolver.rs:3246-3332`） | prod: 1 (`crates/flpdf/src/reader/resolver.rs:3156`) / test: 0 | canonical | `crates/flpdf/src/reader/resolver.rs::read_stream` | 3 つのメッセージ（"stream dictionary lacks /Length key" / "/Length key in stream dictionary is not an integer" / "expected endstream"）と `attempt_recovery` 分岐が 1:1（`crates/flpdf/src/reader/resolver.rs:3335-3348`） |
| C42 | `QPDF::recoverStreamLength` | `libqpdf/QPDF.cc:1482-1530` | `crates/flpdf/src/reader/resolver.rs::recover_stream_length`（private） | prod: canonical resolver の stream recovery 呼び出し / test: recovery fixture と EOL metadata tests | mixed | `crates/flpdf/src/reader/resolver.rs::recover_stream_length` | warning 3 種、`endobj` 巻き戻し、全 recovered length の計算は 1:1。`RecoveredStreamEol` は `dump-object` 再シリアライズのframing専用metadataとして残り、C12のAES/RC4 pipeとshow-streamのraw/decoded payloadはqpdfと同じく `length` の全spanをそのまま渡す（`flpdf-zvjf`, `flpdf-hj7v`）。 |
| C43 | （なし — flpdf 固有の分類 helper） | 対応物なし | `crates/flpdf/src/filters.rs::is_decoded_filter`（`pub`, `crates/flpdf/src/filters.rs:87-89`）/ `::passthrough_codec_label`（`pub`, `crates/flpdf/src/filters.rs:74-76`） | `is_decoded_filter` prod: 1 (`crates/flpdf/src/job/inspection.rs:91`) / test: 0 | bridge | `crates/flpdf/src/stream_filter.rs::stream_filter_for` | **route と責務を分けて読む**: この経路は責務のレベルでも qpdf に対応物が無い（C28 と同型の単純な `bridge`）。`stream_filter.rs::is_decoded_filter`（`crates/flpdf/src/stream_filter.rs:229`）への `pub` 再公開で、qpdf は「デコードできるか」を `filterable` 越しにしか答えない（probe mode、`libqpdf/QPDF_Stream.cc:523-527`）ので、名前だけで答えるこの API に qpdf 対応物は無い |

| # | qpdf responsibility owner | qpdf evidence | flpdf current entrypoint | callers (prod / test) | classification | canonical owner | remaining bridge callers / notes |
|---|---|---|---|---|---|---|---|
| C44 | `QPDFObjectHandle::getStreamJSON` / `QPDF_Stream::getStreamJSON` の inline blob 供給（`StreamBlobProvider`） | `include/qpdf/QPDFObjectHandle.hh:1235-1240`, `libqpdf/QPDFObjectHandle.cc:1649-1657`, `libqpdf/QPDF_Stream.cc:96-107,186-204` | 対応 entrypoint 無し（public `getStreamJSON` 未実装） | prod: 0 / test: 0（未実装 API のため。C24 の caller は含めない） | mixed | Rust 側 canonical owner 未実装 | API 欠落を追跡する行。従来は別責務の `write_stream_json` をこの API の代替として混同していた。C24 の `write_stream_json` は canonical であり、二重 pipe へ修正する対象ではない。欠落 `getStreamJSON` / deferred `StreamBlobProvider` は別 primitive として移植する |

C44 は未実装の public 責務を追跡する行として保持する（2026-09-06 再監査）。
`write_stream_json` の責務は C24 の `QPDF_Stream::writeStreamJSON` であり、qpdf 自身も
`libqpdf/QPDF_Stream.cc:243-295` で payload を buffer 化して inline base64 を書く。
これは `getStreamJSON` の不完全な実装ではない。独立した public API
`QPDFObjectHandle::getStreamJSON`（`include/qpdf/QPDFObjectHandle.hh:1235-1240`,
`libqpdf/QPDFObjectHandle.cc:1649-1657`）と `StreamBlobProvider`
（`libqpdf/QPDF_Stream.cc:96-107,186-204`）には現在 Rust 側の対応 entrypoint が無い。
この欠落は別の primitive 移植対象であり、既存 `write_stream_json` を二重 pipe 化してはならない。

### 2026-09-06 consumer 再監査

次の記録は既存の green test を完全 parity の根拠にせず、現存 caller と責務を再確認したもの。
旧集計の直接 caller 数だけでは、public wrapper 内部の呼び出しを実際の production consumer と
取り違えるため、root caller も併記する。

- **C4**: `reader/resolver.rs:2384` の ObjStm consumer と
  `object_handle.rs:6198,6874` の専用 pipe / error mapping は現存する。qpdf は
  `QPDF.cc:1792` で通常の `getStreamData(qpdf_dl_specialized)` を呼び、例外を
  `QPDF::resolve`（`QPDF.cc:1737-1744`）で扱う。専用 source error flag の除去は、
  original / replaced buffer / provider ごとの例外と warning 境界を確認してから行う。
- **C8 / C25**: `qpdf_raw_stream_payload`、`stream_payload_for_decode_level`、
  `stream_payload_with_decode_status`、`filters::stream_filter_capabilities` は
  `.40` で撤去。`document_json` は既にC24正本を呼ぶ。raw取得は
  `ObjectHandle::get_raw_stream_data`、decodeは `get_stream_data`/`pipe_stream_data`、
  JSONのfiltered→raw retryは `ObjectHandle::write_stream_json` が所有する。
- **C27**: `xref.rs:751` は **bootstrap ObjStm** の
  `BootstrapHandleDocument::resolve_objects_in_stream`（qpdf `QPDF.cc:1792`）、
  `xref.rs:3381` が **xref stream**（qpdf `QPDF.cc:1051`）である。
  bootstrap owner の `DocumentResolver` 実装は `xref.rs:885` にあり、通常 resolver の
  original pipe を備えていない。bootstrap owner の責務統合を先に確認する。
  残る `resources.rs:277` の Form pre-pass と
  `filespec_helper/embedded_file_stream.rs:273` の `payload` は別 consumer slice。
  Form は qpdf `QPDFPageObjectHelper.cc:539-649` の `parseContents` / `ResourceFinder`
  経路へ対応させる。closed `flpdf-egzr.3.2.8.4` は handle 移行で、whole-buffer route の
  撤去ではない。
- **C26 bridge note**: `filters.rs::decode_prepared_specs` は legacy whole-buffer route の
  setter拒否を保持し、canonical `ObjectHandle::prepare_stream_filter_plan` の全setter走査とは
  意図的に同一化していない。この残差は `.49` のbridge移行で扱い、ここでbridgeへqpdf責務を
  追加しない。
- **C28**: `decode_stream_data_recovering_with_limits` の実 production consumer は
  `flpdf-qtest-tools/src/driver/test_0_1.rs:332` のみ（公開 wrapper の内部委譲を除く）。
  test 0 の手製 `/DecodeParms` 診断と recovering event 列を canonical pipe / logger に
  移す slice は E-27 の `qtest_*` 診断 metadata consumer と同一。primitive warning 境界を
  先に揃え、caller が無くなった API / `DecodeLimits` は後続 cleanup で扱う。
- **C29**: appearance consumer の owner は `QPDFJob::handleUnderOverlay` ではなく
  **`QPDFAcroFormDocumentHelper::adjustAppearanceStream`**
  （`QPDFAcroFormDocumentHelper.cc:615-696`）。qpdf は `parseAsContents` の後に
  `ResourceReplacer` を `addTokenFilter` する（同ファイル `:680-690`）。現存 Rust の
  `overlay_appearance_stream.rs:210-235` は eager decode / re-encode を行うため、
  この遅延 token-filter consumer を切り出す。既存 open `flpdf-1far` の oracle 不足も同経路。
  ObjStm emission の `writer/object_streams/emission.rs:171` は qpdf も直接 Flate を
  接続する別責務（C31）なので、ordinary stream の pipe へ一括移行しない。
- **C26 の harness consumer**: `driver/test_34_41.rs:519` の synthetic filter dictionary は、
  qpdf `test_driver.cc:1329-1331` の raw stream pipe → bare `Pl_Flate` に置き換える対象。
  `compare.rs:85,89` の decoded comparison は別 slice。`decode_stream_data` の削除は
  JSON wrapper とこれらの consumer が無くなった後。
- **C42 / C43**: EOL metadata は `job/inspection.rs:273` の独自 `dump-object` framing、
  filter 名だけの分類は同ファイル `:90-97` の `show_stream` binary label に残る。
  qpdf `QPDFJob.cc:806-832` は stream dictionary 表示と pipe を使い、どちらの shortcut も
  持たない。CLI consumer の移行後に metadata / classifier を削除する。

C7 の `prepare_stream_filter_plan` は現在も `object_warning` を使う
（`object_handle.rs:6643,6648,6691`）。qpdf の `QPDF_Stream::warn`
（`QPDF_Stream.cc:694-698`）は parsed offset 経路なので、test 0 の手製診断を
撤去する前提として malformed `/Filter` / `/DecodeParms` の warning を独立に検証する。
C10 の runtime `registerStreamFilter` 欠落は、`match` という入れ物だけでは説明できない
public 契約の欠落。C21 の `/F` / `/FFilter` / `/FDecodeParms` 削除も現在
`writer/plain/body.rs:861-865` に残る。C7/C21はこの診断分裂・責務混在によりmixedへ訂正した。
C10のcanonicalはbuilt-in lookupに限定し、runtime登録の公開契約は別issueで移植する。

## unknown / probe

| ID | 問い | 決めるのに要る source / probe |
|---|---|---|
| U3 | C22 の caller ごとの callback timing と出力保持が qpdf の writer 責務に一致するか | plain / QDF は現在全 indirect stream の完成出力を cache する（`writer/plain/plan.rs:135-158`, `writer.rs:3847-3875`、closed `flpdf-25kg.2.2.15`）。qpdf の linearized optimizer は `QPDFWriter.cc:2543-2553` で明示的に事前 probe する。したがって early return の有無だけでは不一致とは言えない。stateful token filter / retry-aware provider の call order・warning・bytes を plain / QDF / linearize それぞれの qpdf owner と比較し、残る planner 分岐を確認する |
| U4 | C17-C18 の reader/writer consumer が共有 primitive と同じ key を返すか | 完了。pinned qpdf headerをincludeしたC++ probeで、`objid=0x010203`、`generation=0x0405`、V={1,2,4,5}、R=6固定、key長={5,16,24,32}、AES/RC4を全組合せ確認し、`encryption_R` は qpdf原典でも未使用であることを確認した。結果を `crates/flpdf/src/encryption/primitives.rs` の32固定vectorで検証。qpdf側は1実装なので、reader/writerの旧2実装差分テストは不要になった |

## 分類集計（2026-09-06 再監査時点）

| 分類 | 件数 | 行 |
|---|---|---|
| canonical | 29 | C1, C2, C3, C5, C6, C8, C10, C12, C13, C14, C15, C16, C17, C18, C20, C24, C25, C30, C31, C32, C33, C34, C35, C36, C37, C38, C39, C40, C41 |
| mixed | 11 | C4, C7, C9, C11, C21, C22, C26, C27, C29, C42, C44 |
| bridge | 2 | C28, C43 |
| unknown | 0 | なし（C42 の pipe/show-stream EOL subtraction は `flpdf-zvjf` と `flpdf-hj7v` で qpdf parity として解決） |

U3 は「分類は決まっているが、残る実装差と出力への影響を検証する」項目なので
`unknown` 行にはせず、対応する mixed 行（C22）から参照している。U4 は C17/C18 の
shared primitive 統合と oracle vector 検証が完了しているため、未完了 probe としては扱わない。
旧 U2 は `writeStreamJSON` と未実装 `getStreamJSON` の責務を取り違えていたため除外した。
C44 はその API 欠落を追跡し、既存 C24 の不一致とは扱わない。

`bridge` の判定基準は README §3 の通り **経路（route）に qpdf 対応物が無いこと** で、
責務（responsibility）に qpdf 対応物があるかどうかとは別に問う。本領域の 2 行はこの区別で読む:
C28 / C43 は責務のレベルでも qpdf に対応物が無い。
qpdf 側にも実装にも対応物がある複数実装は `bridge` ではなく `mixed` に置く。旧C19は`.42`で削除した。


## 2026-09-06 再監査の issue 対応

親 epic は `flpdf-3yn9.48`。下表は責務と実装 issue の対応であり、完了状態は `bd show <id>` で確認する。
各 issue の受入条件に qpdf 根拠、最初の consumer、残 caller と削除条件を記録した。

| 対象行 | Beads issue | 責務 / 移行 slice |
|---|---|---|
| `C27` | `flpdf-3yn9.48.14` | bootstrap ObjStm展開をcanonical resolveObjectsInStreamへ移行する |
| `C7` | `flpdf-3yn9.48.37` | QPDF_Stream::filterable のwarningをparsed-offset付き正本経路に統一する |
| `C4` | `flpdf-3yn9.48.38` | ObjStm codec例外境界をQPDF::resolveへ集約する |
| `C17` / `C18` | `flpdf-3yn9.48.39` | reader/writer のcompute_data_keyをqpdf共通primitiveへ統合する |
| `C8` / `C25` | `flpdf-3yn9.48.40` | test-only JSON payload compatibility APIをcanonical stream APIへ移して撤去する |
| `C27` | `flpdf-3yn9.48.41` | Form resource pre-passをparseContents/ResourceFinderの共有経路へ移行する |
| `C27` | `flpdf-3yn9.48.42` | EF payload consumerをcanonical stream buffer sinkへ移行する |
| `C27` | `flpdf-3yn9.48.43` | bootstrap xref stream payload consumerをcanonical specialized pipeへ移行する |
| `C28` | `flpdf-3yn9.48.44` | qtest test0/1をcanonical pipe/loggerへ移し手製stream診断を撤去する |
| `C26` / `C29` | `flpdf-3yn9.48.45` | qtest direct codec consumerをqpdfと同じPipeline段へ移行する |
| `C10` | `flpdf-3yn9.48.46` | runtime registerStreamFilter の公開registry契約を移植する |
| `C44` | `flpdf-3yn9.48.47` | public getStreamJSONとdeferred StreamBlobProviderを忠実移植する |
| `C42` / `C43` | `flpdf-3yn9.48.48` | dump-object framingとshow_stream binary-label迂回をcanonical CLI cutover後に撤去する |
| `C9` / `C26` / `C28` / `C29` | `flpdf-3yn9.48.49` | whole-buffer decoder/encoderとrecovering compatibility経路を最後のcaller移行後に撤去する |
| `C21` / `C22` | `flpdf-3yn9.48.64` | unparseObjectのrefiltered stream辞書処理をqpdfの責務に揃える |
| `C29` | `flpdf-1far` | adjustAppearanceStream のtoken-filter正本とoracle試験 |
| `C21` | `flpdf-vo76` | 外部stream Lengthの既存受入 |

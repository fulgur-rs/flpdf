# qpdf StreamDataProvider 設計

## 目的

qpdf 11.9.0 の `QPDFObjectHandle::StreamDataProvider` と、provider-backed
stream source を `ObjectHandle` に忠実に移植する。provider は登録時に実行せず、
既存の replaced buffer / parsed original source と同じ stream の source model に
第三の source として入る。空 `Vec` や direct-stream 用の adapter で provider を
表現しない。

この設計の対象は provider の契約、所有、置換、source dispatch への接続である。
filterable/reverse decoder pipeline は `flpdf-3yn9.6` が所有する既存の qpdf-shaped
pipeline を再利用し、EmbeddedFile の consumer migration は `flpdf-25kg.4.4` に
残す。

## qpdf 11.9.0 の確定事項

参照する pinned source は、開発者固有の cache path を文書へ埋め込まず、
`scripts/fetch-qpdf-source.sh --print-path` の解決結果を使う。シェル上では
次のように取得する。

```bash
qpdf_source="$(scripts/fetch-qpdf-source.sh --print-path)"
```

1. `QPDFObjectHandle::StreamDataProvider` は `supports_retry` を保持し、通常形と
   retry-aware 形の `provideStreamData` を持つ。`QPDFObjGen` 形は `(objid,
   generation)` 形へ委譲し、基底の未実装形は
   `you must override provideStreamData -- see QPDFObjectHandle.hh` の
   `std::logic_error` を投げる。`supportsRetry()` は保存値を返す。
   (`include/qpdf/QPDFObjectHandle.hh:68-127`,
   `libqpdf/QPDFObjectHandle.cc:48-90`)
2. qpdf に provider 専用の `newStream` overload はない。provider-backed stream は
   `QPDF::newStream()` の後に
   `replaceStreamData(provider, newNull(), newNull())` を呼んで作る。
   (`libqpdf/QPDFEFStreamObjectHelper.cc:102-107`)
3. `replaceStreamData(provider, filter, decode_parms)` は provider を保持し、
   replaced buffer を clear し、共通の `replaceFilterData` を length `0` で呼ぶ。
   buffer overload は逆に provider を clear する。
   (`libqpdf/QPDF_Stream.cc:640-660`)
4. source dispatch の順序は replaced buffer、provider、`parsed_offset == 0` の
   no-data error、original parsed source である。provider branch は
   `Pl_Count` を downstream に挿入し、retry-aware provider の bool を伝播し、
   実測 byte 数と既存 `/Length` を検証するか、未設定なら `/Length` を設定する。
   (`libqpdf/QPDF_Stream.cc:571-620`)
5. filter/decode-parameters の uninitialized は既存値を保持し、direct null は
   key を削除する。provider/buffer の length `0` は `/Length` を削除し、非 zero は
   実値を設定する。(`include/qpdf/QPDFObjectHandle.hh:1080-1084`,
   `libqpdf/QPDF_Stream.cc:669-685`)
6. provider の各 invocation は同一 byte 列を出力し、linearized write では複数回
   呼ばれ得る。provider は PDF object を変更してはならず、`QPDFObjGen` は複数
   stream を識別するために利用できる。
   (`include/qpdf/QPDFObjectHandle.hh:80-105`)

## Rust 側の責務境界

### Provider 契約

`object_handle.rs` に qpdf の nested class に対応する公開
`StreamDataProvider` trait を置く。Rust では overload を分けた名前にするが、次の
4 つの責務と委譲関係を保持する。

trait は `Rc<dyn StreamDataProvider>` として stream value に保持できる object-safe
な形にし、provider の所有権は登録した stream が持つ。

- `provide_stream_data(ObjectRef, &mut dyn Pipeline) -> Result<()>`
- `(u32, u16, &mut dyn Pipeline) -> Result<()>` の legacy identity form
- retry-aware の `ObjectRef` form
- retry-aware の `(u32, u16, &mut dyn Pipeline, bool, bool) -> Result<bool>`

`ObjectRef` form は番号・generation formへ委譲する。少なくとも一方の form を
overrideしなければ qpdf と同じ object-layer `Error::Internal` の契約エラーに
する。provider から downstream `PipelineError` を返す場合は既存の
`Error::Internal` / `Error::System` 変換を使い、provider API 自体を
`PipelineResult` にしない。

### Stream source の表現

`ObjectValue::Stream` に `stream_provider: Option<Rc<dyn StreamDataProvider>>` を
追加し、qpdf の `stream_data` / `stream_provider` の exclusive ownership を
表現する。

| source | `stream_data` | `stream_provider` | parsed offset | 意味 |
|---|---:|---:|---:|---|
| replaced buffer | `Some` | `None` | 任意 | 共有 `Rc<Vec<u8>>` を直接出力 |
| provider | `None` | `Some` | 任意 | pipe 時にだけ provider を呼ぶ |
| original | `None` | `None` | `> 0` | resolver の parsed source を読む |
| qpdf new empty stream | `None` | `None` | `0` | no-data error |

buffer replacement は provider を clear し、provider replacement は buffer を
clearする。登録・置換時には providerを呼ばない。providerの `Rc` は stream value の
clone と owner drop 後の surviving handle で保持される。

### 公開置換入口

`ObjectHandle` に provider-backed `replace_stream_data` 相当を追加する。filter と
decode parameters は既存 buffer overload と同じ境界を使う。

- `None` は qpdf の uninitialized handle として既存 key を保持する。
- `Some(ObjectHandle::null())` は canonical `replace_key` の direct-null removal
  を通る。
- その他の handle は key を置換する。
- provider 登録時は共通 length boundary に `0` を渡し、既存 `/Length` を消す。

qpdf の `std::function<void(Pipeline*)>` と retry-aware function overload も、
provider trait を実装する qpdf-shaped adapter として同じ入口へ委譲する。adapter は
callback を登録時に実行せず、provider lifetime を `Rc` で保持する。

### Pipe の source dispatch

既存の `ObjectHandle::pipe_stream_data` / `pipe_stream_source` の filter pipeline
を変更せず、source branch だけを qpdf 順序に拡張する。provider branch は
`flpdf-3yn9.6` が既に持つ `Pipeline` chaining を downstream に置き、providerの
通常/retry formを `supports_retry()` に従って呼ぶ。`Pl_Count` 相当の count、
retry/success の扱い、既存 `/Length` との比較、成功時の `/Length` 更新は
`flpdf-3yn9.6` の pipe 実行責務として一箇所に保つ。`.3yn9.7` はこの branch が
利用できる source と contract を提供し、filter構築を複製しない。

provider branch が false を返した場合の raw retry と warning の境界は existing
resolver / writer contract を維持する。provider failure は no-data や original
source failure に置き換えず、qpdf の provider branch のエラー分類を保つ。

## 失敗原子性と不変条件

- provider 登録の途中で callbackを呼ばない。
- providerを設定したstreamの `as_stream_data()` は `None` を返し、buffer replacement
  後は provider が見えない。
- provider invocation は同じ `ObjectRef` を受け、同じ stream に対して再度呼ばれても
  同じ bytes を出す。
- provider は `stream_dict` や他の PDF object を mutation しない。
- provider の unknown length は `/Length` の sentinel にしない。count が実測値を
  設定するまで zero length と混同しない。
- 非 stream handle の provider replacement は qpdf-shaped `Error::System` の object
  assertion boundary として扱い、既存の silently-no-op を残さない。

## 検証方針

設計後の stacked layers は次の順序にする。

1. qpdf oracle probe と RED tests: providerの遅延実行、object identity、source
   replacement、null/unknown `/Length`、通常/retry delegation、default error、
   repeated call を qpdf 11.9.0 で固定する。
2. provider contract/storage: trait、function adapter、stream value の第三 source、
   replacement ownership、dictionary boundary を実装する。
3. pipe integration: completed `.3yn9.6` pipeline に provider branch を接続し、
   `Pl_Count`、retry、length mismatch、warning/error propagation を実装する。
4. consumer migration: providerを必要とする EmbeddedFile は `.25kg.4.4` で移行する。

各 layer は親 branch との差分だけで RED→GREEN、focused tests、fmt、workspace
quality gates、changed-line coverage 100% を確認する。`ObjectHandle::stream` の
empty buffer sentinel、Filespec-local facade、provider-to-Vec collector、legacy
raw `Object` bridge は追加しない。

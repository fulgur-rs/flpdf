# A. ObjectHandle / Resolver — object identity, lazy resolve, ownership, teardown

対象: `QPDF` が持つ object identity（`QPDFObjGen` → `ObjCache`）、lazy resolve の唯一経路
（`QPDFObjectHandle::dereference` → `QPDFObject::resolve` → `QPDF::Resolver::resolve` →
`QPDF::resolve`）、object の生成・置換・交換・削除（`makeIndirectObject` / `replaceObject` /
`swapObjects` / `removeObject`）、および input source の切り離しと `~QPDF` の teardown。
flpdf 側は `crates/flpdf/src/reader.rs`（`Pdf`）/ `crates/flpdf/src/reader/resolver.rs`
（`ResolverCore` / `ResolverHandle`）/
`crates/flpdf/src/object_handle.rs`（`ObjectHandle` と `ObjectValue` は同一ファイル）/
`crates/flpdf/src/pdf.rs`（`Pdf` の struct 定義と `Drop`）が対応面。

**読み方の注意**: 本表の `classification` は「qpdf 責務に至る flpdf の経路が 1 本か」を問う
（README §3）。`docs/qpdf-correspondence.md` の ✅ 行でも、consumer 側に二重経路が残っていれば
ここでは `mixed` / `bridge` になる。

2026-09-06 再監査では、下記で日付を明記した行の実装・残caller・例外境界を更新した。
全行のcaller件数を再集計したものではない。未更新行の数値は元の調査時点の値であり、
着手前にreceiverとtest区画を識別して再計測する。

## qpdf 責務モデル

### state（`QPDF::Members`、`include/qpdf/QPDF.hh:1456-1485`）

resolve に関わる state は 5 つだけで、すべて `Members` の private フィールド。
`Members` は `QPDF` と `ResolveRecorder` のみを friend にする（`include/qpdf/QPDF.hh:1440-1443`）。

| qpdf state | 型 | 役割 |
|---|---|---|
| `m->file` | `std::shared_ptr<InputSource>` | 入力ソース。既定は `InvalidInputSource`（`libqpdf/QPDF.cc:198-203`）で、触ると `std::logic_error`（`libqpdf/QPDF.cc:99-106`）。 |
| `m->xref_table` | `std::map<QPDFObjGen, QPDFXRefEntry>` | og → xref entry。resolve の dispatch 元。 |
| `m->obj_cache` | `std::map<QPDFObjGen, ObjCache>` | **object identity の唯一の正本**。`ObjCache` は `{object, end_before_space, end_after_space}` の 3 フィールドのみ（`include/qpdf/QPDF.hh:868-889`）。 |
| `m->resolving` | `std::set<QPDFObjGen>` | resolve 中の og。`ResolveRecorder` の ctor/dtor が insert/erase する（`include/qpdf/QPDF.hh:980-996`）。 |
| `m->resolved_object_streams` | `std::set<int>` | 展開済み ObjStm 番号。二重展開を防ぐ（`libqpdf/QPDF.cc:1756-1761`）。 |

**この 5 つに `m->deleted_objects`（`include/qpdf/QPDF.hh:1466`）が入らないのは意図的**: これは
`insertFreeXrefEntry`（`libqpdf/QPDF.cc:1186-1192`）が xref 構築中に free 行を記録し、
`insertReconstructedXrefEntry`（`libqpdf/QPDF.cc:1204-1209`）の上書き抑止と `/Size` 整合
warning（`libqpdf/QPDF.cc:694-696`）にだけ使う **xref 構築専用のフィルタ**で、用が済むと
`libqpdf/QPDF.cc:706-708` で「We no longer need the deleted_objects table … to make sure we
never depend on its being set」というコメントとともに clear される（reconstruct 開始時にも
`libqpdf/QPDF.cc:575` で clear）。したがって resolve/identity の state ではなく、**qpdf の
object cache には「削除済み」という永続 tombstone が一切存在しない** — 後述 A2 / A17 の
flpdf側にも永続の削除tombstoneは残さず、canonical resolverのcache cell消去を正本にする。

`ObjCache` に入るのは `std::shared_ptr<QPDFObject>` で、`QPDFObject` は
`std::shared_ptr<QPDFValue> value` 1 本しか持たない薄い indirection
（`libqpdf/qpdf/QPDFObject_private.hh:19-180`）。`QPDFValue` 側に `qpdf` ポインタと `og` が載る
（`libqpdf/qpdf/QPDFValue.hh:149-152`）。**`QPDFValue` の派生に `Reference` は無い** —
`QPDF_Array` / `Bool` / `Destroyed` / `Dictionary` / `InlineImage` / `Integer` / `Name` / `Null` /
`Operator` / `Real` / `Reserved` / `Stream` / `String` / `Unresolved` の 14 種のみ
（`probe: ls $Q/libqpdf/qpdf/QPDF_*.hh → 14 ファイル、QPDF_Reference.hh は無い`）。
「間接参照である」ことは値の種類ではなく **handle の og が非 0 か**
（`QPDFObjectHandle::isIndirect` = `obj != nullptr && getObjectID() != 0`、
`include/qpdf/QPDFObjectHandle.hh:1629-1639`）で表される。未解決状態も専用の値
`QPDF_Unresolved`（`libqpdf/qpdf/QPDF_Unresolved.hh:6-17`）で表され、参照値ではない。

### call order — resolve への経路は 1 本しかない

1. `QPDFObjectHandle` の **すべての型アクセサ**（`getTypeCode` / `asArray` / … / `isNull` /
   `isStream` …）が `dereference()` を通る（`libqpdf/QPDFObjectHandle.cc:240-446`）。
2. `dereference()` は `isInitialized()` を見てから `obj->resolve()` を呼ぶだけ
   （`libqpdf/QPDFObjectHandle.cc:2375-2383`）。
3. `QPDFObject::resolve()` は `isUnresolved()` なら `doResolve()`
   （`libqpdf/qpdf/QPDFObject_private.hh:155-167`）。
4. `doResolve()` は `QPDF::Resolver::resolve(value->qpdf, og)`（`libqpdf/QPDFObject.cc:6-11`）。
5. `QPDF::Resolver` は `friend class QPDFObject` **のみ**を許す nested class
   （`include/qpdf/QPDF.hh:770-781`）。`QPDF::resolve` 自身は private
   （`include/qpdf/QPDF.hh:1031`）。

つまり **`QPDF::resolve` を呼べるのは `QPDFObject` だけ**で、アクセサ経由以外に resolve は起きない。
この非対称の相方が `QPDF::getObject`（`libqpdf/QPDF.cc:1951-1959`）で、コメントが
「This method is called by the parser and therefore must not resolve any objects.」と明記し、
cache に無ければ `QPDF_Unresolved` を **入れるだけ**で handle を返す。
**取得（getObject）は resolve しない / 解決（resolve）はアクセサからしか起きない** が本領域の背骨。

`QPDF::resolve` 本体（`libqpdf/QPDF.cc:1699-1753`）の順序:

1. `isUnresolved(og)` でなければ即 return（`isUnresolved` = 未 cache または cache 値が
   `ot_unresolved`、`libqpdf/QPDF.cc:1860-1870`）。
2. `m->resolving` に og があれば **loop warning** を出し、cache を `QPDF_Null` にして return。
3. `ResolveRecorder rr(this, og)` で `m->resolving` に登録（スコープ離脱で自動 erase）。
4. `m->xref_table` に og があれば entry type で dispatch:
   type 1 → `readObjectAtOffset(true, offset, "", og, a_og, false)`、
   type 2 → `resolveObjectsInStream(entry.getObjStreamNumber())`、
   それ以外 → `damagedPDF(... "has unexpected xref entry type")` を throw。
5. 4 の `QPDFExc` / `std::exception` は **catch して `warn` に落とす**（例外を外に出さない）。
6. なお未解決なら `QPDF_Null` を cache（"PDF spec says unknown objects resolve to the null object"）。
7. 最後に `result->setDefaultDescription(this, og)`。

cache 更新は必ず `updateCache`（`libqpdf/QPDF.cc:1842-1858`）を通る。既存 entry があれば
`cache.object->assign(object)` で **同一 `QPDFObject` の中身を差し替える**（既存 handle が
新しい値を見る）。無ければ新規 `ObjCache` を入れる。

ObjStm 展開（`libqpdf/QPDF.cc:1756-1833`）は、`resolved_object_streams` で二重展開を防ぎ、
stream の `end_before_space` / `end_after_space` を **メンバー全員に配る**。さらに
「xref を再チェックし、実際にここで解決されるものだけを cache する」— append で上書きされた
メンバーは cache しない。

### object 生成・置換・交換・削除

| qpdf | 可視性 | 挙動 |
|---|---|---|
| `makeIndirectObject(oh)` | public（`include/qpdf/QPDF.hh:359`） | 未初期化なら `std::logic_error`。`makeIndirectFromQPDFObject` へ委譲（`libqpdf/QPDF.cc:1890-1897`）。 |
| `makeIndirectFromQPDFObject` | private（`include/qpdf/QPDF.hh:1038`） | `nextObjGen()`（= `getObjectCount()+1`、`libqpdf/QPDF.cc:1872-1880`）で採番し `obj_cache` に直接入れる（`libqpdf/QPDF.cc:1882-1888`）。 |
| `newIndirectNull()` | public（`include/qpdf/QPDF.hh:355`） | `makeIndirectFromQPDFObject(QPDF_Null::create())`（`libqpdf/QPDF.cc:1905-1909`）。 |
| `replaceObject(og, oh)` | public（`include/qpdf/QPDF.hh:384-386`） | indirect / 未初期化なら `std::logic_error`。`updateCache(og, oh.getObj(), -1, -1)`（`libqpdf/QPDF.cc:1985-1993`）。 |
| `swapObjects(og1, og2)` | public（`include/qpdf/QPDF.hh:391-393`） | **先に両方を `resolve` してから** `swapWith`（`libqpdf/QPDF.cc:2284-2291`）。`swapWith` は value と og を交換（`libqpdf/qpdf/QPDFObject_private.hh:121-130`）。 |
| `removeObject(og)` | **private**（`include/qpdf/QPDF.hh:1041`） | xref から erase し、cache 済みなら値を `QPDF_Null` に assign して og を切り、cache から erase（`libqpdf/QPDF.cc:1995-2005`）。 |

**public API としての「削除」は `removeObject` ではない**: `include/qpdf/QPDF.hh:374-382` が
「replacing an object with `QPDFObjectHandle::newNull()` effectively removes the object from the
file」と明記する。`removeObject` は内部専用。

`getAllObjects`（`libqpdf/QPDF.cc:1285-1295`）は `fixDanglingReferences()` →
`obj_cache` 全走査。`fixDanglingReferences`（`libqpdf/QPDF.cc:1256-1269`）は
`m->fixed_dangling_refs` で 1 度きりにし、`resolveXRefTable()`（`libqpdf/QPDF.cc:1239-1254`）が
xref 全 og を resolve する。**`resolveXRefTable` が xref reconstruction を誘発したら false を返し、
`fixDanglingReferences` はもう 1 度だけ回す**（reconstruct 後の xref で再走）。

### error / warning boundary

- **warning に落ちるもの**: resolve 中の loop 検出、resolve 中に投げられた `QPDFExc` /
  `std::exception`（`libqpdf/QPDF.cc:1706-1745`）、ObjStm の `/Type` が `/ObjStm` でない
  （`libqpdf/QPDF.cc:1776-1780`）、型不一致アクセサの `typeWarning`
  （`libqpdf/QPDFObjectHandle.cc:2168-2188`、warn したうえで null / 空を返す）。
- **例外を投げるもの**（`std::logic_error` 系 = 呼び出し側の契約違反）:
  未初期化 handle の indirect 化、`replaceObject` に indirect handle、未初期化 handle の
  dereference（`libqpdf/QPDFObjectHandle.cc:1586-1593`）、`InvalidInputSource` への操作
  （`libqpdf/QPDF.cc:99-106`）、`nextObjGen` の `std::range_error`。
- **`damagedPDF` の throw** は ObjStm 展開など resolve の内側で起き、`QPDF::resolve` の
  catch が warning へ変換する。resolve の外へは出ない。

### teardown

- `closeInputSource()`（public、`include/qpdf/QPDF.hh:166`、実装 `libqpdf/QPDF.cc:277-281`）は
  `m->file` を `InvalidInputSource` に差し替えるだけ。cache は触らない。以後の I/O は
  `std::logic_error`。
- `~QPDF`（`libqpdf/QPDF.cc:215-236`）は **`m->xref_table.clear()` を先に**行い
  （resolve が成功しうる可能性を潰す）、`obj_cache` 全件に `disconnect()` を呼び、
  `ot_null` 以外は `destroy()`（値を `QPDF_Destroyed` の共有インスタンスに差し替え、
  `libqpdf/QPDFObject.cc:13-17`）。これは相互参照する `shared_ptr` の循環を切るための処理で、
  「QPDF が生きている間は絶対にやってはいけない」とコメントが明記する。

2026-09-08（`flpdf-lomd`）: qpdfの `read_xrefStream` は各xref stream objectを
`readObjectAtOffset` でobj_cacheへ読み込んだ後に `processXRefStream` を実行する
（`libqpdf/QPDF.cc:951-962,1640-1686`）。後続revisionがそのObjectRefをfreeまたは
supersedeしても、`getAllObjects` はcache上の履歴objectを保持し、effective xrefの
live viewとは分離される（`QPDF.cc:1239-1295`）。flpdfのcanonical xref parserも
`LoadedXrefState::parsed_xref_streams`へstream handleを渡し、`Pdf`構築後に
`qpdf_parsed_xref_stream_refs`で`live_object_refs`から除外する経路を固定した。
incremental fixtureでは、object 5の履歴xref streamがcanonical cache viewには残り、最新revision
でfreeになった後のcanonical live viewからは消えることをRED/GREENで確認する。

2026-09-09（`flpdf-3yn9.48.22`）: A1/A2/A9/A10/A11/A13/A15/A16/A17/A24 の
facade cache cutoverを完了した。`crates/flpdf/src/cache.rs`、`CacheEntry`、
`Pdf::object_refs`、`Pdf::live_object_refs`、`Pdf::resolved_count`、
`synchronize_cache_with_resolver_xref`、`legacy_resolution_state_synced`、
`qpdf_parsed_xref_stream_refs`、`qpdf_dangling_refs`、`compressed_member_parents`
を削除し、writer/linearization/page-mergeの残consumerは
`ResolverCore::object_cache`をsource-xref keyとunionした private
`Pdf::canonical_object_refs` / `canonical_live_object_refs`へ移行した。
qpdfの完全列挙APIは引き続き`Pdf::get_all_objects`で公開し、歴史的xref streamは
complete cache viewには残し、effective xrefまたはdocument-owned allocationだけを
live viewに含める。`removeObject`後のwriter-local removed setはtombstoneではなく、
compressible walkが返すoperation-local setだけを使用する。

## route matrix

**caller の数え方（本ファイル共通、領域 B/D と同じ規約）**: `rg -n --glob '*.rs' '<pattern>' crates` の
出力から次の 5 種を除いた残りを数える — (a) コメント専用行、(b) 宣言行（`fn`/`struct`/`enum` …）、
(c) `use` 行（**複数行 `use { … }` の継続行を含む**）、(d) `impl <Type>` のヘッダ行、
(e) 文字列リテラル内の言及。型位置での参照（引数型・戻り値型・フィールド型・パターン）は
「呼び出し」ではないが実参照なので数に含める。**prod** = `src/` の非 test 部分、
**test** = `crates/*/tests/` と `mod tests`（＝`#[cfg(test)]` ブロック内）。
ファイル数は **basename ではなく crate 相対パスの一意数**で数える（`plan.rs` のように
同名ファイルが複数ディレクトリにあるため）。

**本領域固有の 2 つの罠**: (1) 領域 D の「モジュール直下の最初の `#[cfg(test)] mod …` より前＝prod」
という単純化は本領域では使えない — `crates/flpdf/src/object_handle.rs` は桁 0 の
`#[cfg(test)] mod X { … }` を **21 個持ち、その間に production コードが挟まる**。ここでは
桁 0 の `#[cfg(test)]` + `mod X {` から桁 0 の `}` までを **各ブロック個別に** test 区画として扱う
（この違いを無視すると `object_handle.rs` の prod が半分近く test に誤計上される）。
`crates/flpdf/src/json/input_tests.rs` は `crates/flpdf/src/json/mod.rs:18-19` で
`#[cfg(test)] mod input_tests;` と gate されているためファイル全体が test。
(2) impl 内の項目単位 `#[cfg(test)]`（`crates/flpdf/src/reader.rs:1328,1336,1587`）はモジュール
test 区画の外にあるので行位置では test 判定されない — 宣言自体が test-only なので該当行は
notes で個別に `prod: 0` と断る。`fuzz/` は別枠で数える（本領域は全行 0 件）。
`crates/flpdf-cli` と `crates/flpdf-qtest-tools`（qpdf の `qpdf/test_driver.cc` 等に対応する
実バイナリ）は prod に数え、後者由来は notes に明記する。caller が 20 を超える行は README §3 に従い、
**再現可能な `rg` コマンドとファイル別件数**で全列挙に代える。

| # | qpdf responsibility owner | qpdf evidence | flpdf current entrypoint | callers (prod / test) | classification | canonical owner | remaining bridge callers / notes |
|---|---|---|---|---|---|---|---|
| A1 | `QPDF::Members::obj_cache`（object identity の唯一の正本） | `include/qpdf/QPDF.hh:1467`、`include/qpdf/QPDF.hh:868-889` | `crates/flpdf/src/reader/resolver.rs::ResolverCore::object_cache`（private） | prod: canonical resolver only / test: 0 | canonical | `crates/flpdf/src/reader/resolver.rs::ResolverCore::object_cache` | `flpdf-3yn9.48.22` で facade cache と bootstrap/provenanceの二重帳簿を撤去。source-xref keys と canonical handles は同じ qpdf-shaped document state の view として扱う。 |
| A2 | `QPDF::ObjCache`（`{object, end_before_space, end_after_space}` の 3 フィールド、private nested class） | `include/qpdf/QPDF.hh:868-889` | `crates/flpdf/src/reader/resolver.rs::ResolverCore::object_cache` | prod: canonical resolver only / test: 0 | canonical | `crates/flpdf/src/reader/resolver.rs::ResolverCore::object_cache` | `flpdf-3yn9.48.22` で qpdfにない6状態の `CacheEntry` facade、公開 `cache` module/export、compatibility cacheを削除。offset metadataはcanonical handle slot側に残る。 |
| A3 | `QPDF::getObject(QPDFObjGen)`（**resolve しない** handle 取得） | `libqpdf/QPDF.cc:1951-1959`、`include/qpdf/QPDF.hh:362-372` | `crates/flpdf/src/reader.rs::Pdf::get_object_handle`（`pub`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::get_object_handle`（`pub(crate)`） | `rg -n '\.get_object_handle\(' crates`（facade 経由と resolver 直呼びの合算）prod: 257 (66 files; うち `crates/flpdf-qtest-tools` の driver 群 `test_10_17.rs` 他) / test: 533 | canonical | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::get_object_handle` | `Pdf::get_object_handle` は 1 行の委譲（`crates/flpdf/src/reader.rs:1313-1321`）で、`or_insert_with` が qpdf の `if (!isCached(og)) { obj_cache[og] = ObjCache(QPDF_Unresolved::create(...)) }` に 1:1 対応。resolve を起こさないという qpdf の契約も守られている。 |
| A4 | `QPDF::resolve(og)`（loop warning → xref type dispatch → catch して warn → null fallback） | `libqpdf/QPDF.cc:1699-1753` | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::resolve_indirect`（`DocumentResolver` の impl、`crates/flpdf/src/reader/resolver.rs:4437`） | `rg -n --glob '*.rs' '[.:]resolve_indirect\(' crates` → prod: **1**、`crates/flpdf/src/object_handle.rs:2610`（`try_dereference` 内、trait 越しの唯一の呼び出し）/ test: 4 (parser.rs `:1101`, reader/resolver.rs `:6316`, object_handle.rs `:8642`, xref.rs `:4916`) | canonical | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::resolve_indirect` | `ResolveMark`（`crates/flpdf/src/reader/resolver.rs:622`）が qpdf の `ResolveRecorder`（`include/qpdf/QPDF.hh:980-996`）、`finish_indirect_resolution` が qpdf の catch → warn → null cache（`libqpdf/QPDF.cc:1737-1749`）に対応。呼び出し元が少ないのは qpdf と同じ構造（`QPDF::Resolver` friend が `QPDFObject` 1 つだけ、`include/qpdf/QPDF.hh:770-781`）で、通常は A5 経由でしか到達しない。 |
| A5 | `QPDFObjectHandle::dereference()` → `QPDFObject::resolve()` → `QPDF::Resolver::resolve` | `libqpdf/QPDFObjectHandle.cc:2375-2383`、`libqpdf/qpdf/QPDFObject_private.hh:155-167`、`libqpdf/QPDFObject.cc:6-11` | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_dereference`（`pub(crate)`、`crates/flpdf/src/object_handle.rs:2586`） | prod: 189 (27 files) / test: 55 | canonical | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_dereference` | 未初期化 handle に対する `Error::Internal("attempted to dereference an uninitialized QPDFObjectHandle")` まで qpdf の `std::logic_error`（`libqpdf/QPDFObjectHandle.cc:1586-1593`）と同文。ここが唯一の resolve 入口であること自体は守られている（A6 も A7 もここへ落ちる）。 |
| A6 | 型アクセサが暗黙に dereference する（`asInteger`/`asDictionary`/`isNull` 等が全て `dereference()` を通る） | `libqpdf/QPDFObjectHandle.cc:240-446` | 2 族が併存: **解決する** `try_*` 族（`crates/flpdf/src/object_handle.rs::ObjectHandle::try_as_integer` 等、`pub(crate)`/`pub`）と、**解決しない** `as_*`/`is_null` 族（`crates/flpdf/src/object_handle.rs::ObjectHandle::as_dictionary` 等、`pub`、`crates/flpdf/src/object_handle.rs:3821-3906`） | 非解決族 prod 合計 679: `as_dictionary` 189 / `is_null` 163 / `as_array` 126 / `as_integer` 74 / `as_name` 53 / `as_string` 53 / `as_real` 21。test 合計 1053。解決族 `try_as_integer` prod: 28 / test: 6 | mixed | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_as_integer`（`try_*` 族） | **本領域最大の二重経路**。qpdf は `asInteger()` も `isNull()` も必ず `dereference()` を通すので、未解決の間接 handle でも正しい型/値を返す。flpdf の `as_*`/`is_null` は doc 自身が "never performs resolution itself" と明記し、未解決の間接 handle に `None`/`false` を返す（`crates/flpdf/src/object_handle.rs:3860-3878`）。同じ handle に対して 2 族が **異なる答え**を返しうるのが mixed の実体。この差を埋めるために呼び出し側が A7 を前置している。2026-09-08（`flpdf-3yn9.48.29`、reader/resolver cohort）: 本領域自身のファイル（`pdf.rs`/`reader.rs`/`reader/resolver.rs`/`reader/file_object.rs`/`cache.rs`）内の A7 前置 caller を全数点検し、`pdf.rs::extension_level_handle`（3 箇所）・`pdf.rs::root_handle`・`reader.rs::remove_security_restrictions`（2 箇所）を `try_as_dictionary`/`try_is_dictionary`/`try_as_integer` へ寄せ、前置していた `self.resolve(...)` を全廃した。あわせて `reader.rs::encrypt_dictionary_handle` の `encrypt.is_null()`（非解決族）を `try_is_null()` に修正 -- qpdf は `m->trailer.hasKey("/Encrypt")`（`libqpdf/QPDF_encryption.cc:729`、resolved-null を absent 扱いする `QPDF_Dictionary::hasKey`)で判定するため、間接参照が null に解決する `/Encrypt` では非解決 `is_null()` が誤答しうる。ただし `engine.rs`の`open_with_repair_mode_as`が`initialize_encryption_inspection`（既に`try_is_null()`を使用）を`authenticate_if_encrypted`より先に必ず呼ぶため、共有 object cache 経由でこの経路は現行の呼び出し順序では到達不能（`/tmp/qpdf-probes/encrypt-resolves-null-clean.pdf` で qpdf 実行環境・flpdf 双方とも修正前後で "File is not encrypted" のまま変化なしと確認済み）。`reader/resolver.rs`/`reader/file_object.rs`/`cache.rs` 自身の prod code には A7 前置 caller が無く（前者の非解決 `is_null()` 3 箇所は resolver 内部の再入回避チェックで、後者の `as_dictionary()` 2 箇所は direct-parse 結果への型観察でどちらも AC の「parserのdirect-only型観察は再入回避契約として保持」に該当）、本 cohort の bridge caller は 0 件になった。次 cohort（`.48.30`〜`.48.33`）の残 caller数は未再計測。2026-09-08（`flpdf-3yn9.48.30`、page/object helper cohort 第1スライス）: `crates/flpdf/src/page_object_helper.rs` の共有継承属性 walk 3 関数（`resolve_attribute_target`/`get_attribute_for_target`/`resolve_inherited_rotate_with_max_depth`、qpdf `QPDFPageObjectHelper::getAttribute` 相当、`libqpdf/QPDFPageObjectHelper.cc:236-247`）の A6/A7 bridge を `try_as_dictionary`/`try_as_name`/`try_is_null` へ寄せた。同ファイルの残りは前スライス後の再計測で `.resolve(` 19 / `resolve_handle`系 9 / `as_x`系 45 / `get_key`系 6（prod のみ、`#[cfg(test)] mod tests` 開始行より前）で、他の page/object helper ファイル（`annotation_object_helper.rs`/`page_splice.rs`/`page_annotation_flatten.rs`/`page_form_xobject.rs` 等）は未着手のまま `flpdf-3yn9.48.70` に切り出した。 2026-09-08（`flpdf-3yn9.48.70`、page/object helper cohort の `annotation_object_helper.rs` 全体）: 同ファイル全9箇所の A6/A7 bridge（`resolved_key`/`get_appearance_stream`/`array_as_rectangle`/`rectangle_from_handle`/`matrix_from_handle`）を閉じ、`rectangle_from_handle`/`matrix_from_handle`/`as_number` は resolve 不要になったため `&mut self` を落として自由関数化した。実装中に「`try_get_key` は受信側 (self) だけを resolve し、返す子 handle 自体は resolve しない」という誤解に基づく regression（`get_appearance_stream` で `ap_sub`/`ap_sub_val` の明示 resolve を誤って削除）を作り込み、`page_annotation_flatten` の既存テスト23件の失敗で検出・訂正した——`try_get_key`/`try_get_keys` 等「self を resolve してから self の値を見る」アクセサと、`try_get_key` のように「self を resolve するが返す子は resolve しない」アクセサを混同しないこと。 |
| A7 | `QPDF::resolve` を呼べるのは `QPDFObject` だけ（public な明示 resolve API は存在しない） | `include/qpdf/QPDF.hh:770-781`（`Resolver` の friend は `QPDFObject` のみ）、`include/qpdf/QPDF.hh:1031`（`resolve` は private） | `crates/flpdf/src/reader.rs::Pdf::resolve`（`pub`、実体は `handle.try_dereference()` 1 行、`crates/flpdf/src/reader.rs:1931-1937`） | `rg -n --glob '*.rs' '\.resolve\(' crates` → prod 255 / test 426。うち `Pdf::resolve` でないものは 23 件（prod 7: `PageRange::resolve` の `job/lifecycle.rs:2734`・`job/overlay.rs:421,422,424`・`job/page_plan.rs:103`・`flpdf-cli/src/main.rs:5865`、および `json/handler.rs:191` の `handler.resolve()`。test 16: `PageRange::resolve` の `job/rotate_spec.rs:223,233,255,264,393`・`job/page_range.rs:557,565,582,686`・`flpdf-cli/src/main.rs:9149,9152,9156,9177,9180,9184,9216`）→ **prod: 248 (56 files) / test: 410**。ファイル別 prod 上位: page_object_helper.rs 21 / page_splice.rs 16 / job/json_sections.rs 15 / flpdf-qtest-tools driver/test_42_49.rs 13 / job/acroform_field_prune.rs 12 / flpdf-qtest-tools driver/handle.rs 12 / flpdf-qtest-tools compare.rs 11 / page_annotation_flatten.rs 10 / page_form_xobject.rs 9 / annotation_object_helper.rs 9（残り 46 ファイル） | bridge | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_dereference`（A5） | **README §3 bridge の形 (ii)**（qpdf に対応する処理が無い flpdf 固有の補助経路 = CLAUDE.md 逸脱分類 (C) の「明示的 `Pdf::resolve` による解決タイミング補正」そのもの）。qpdf には public な明示 resolve 入口が存在せず、アクセサ自身が使う直前に解決する。flpdf では A6 の非解決アクセサ族があるため、呼び出し側が `pdf.resolve(&h)?;` → `h.as_dictionary()` という qpdf に無い 2 段イディオムを踏む。**削除対象**であり、A6 を `try_*` 族へ寄せれば 256 箇所とも不要になる。`Pdf::resolve_handle`（`crates/flpdf/src/reader.rs:1943`）・`Pdf::resolve_handle_ref`（`:1949`）も同じ bridge の薄いラッパー。**数え方の注記**: `\.resolve\(&` 単独は prod 208 / test 404 で、`target.resolve(&page)` は拾うが `pdf.resolve(handle)`（`&` なしの既参照変数）を落とす。`\bpdf\.resolve\(` 単独は prod 235 / test 376 で、逆に `&` なし形は拾うが `target`/`source`/`oldpdf`/`qpdf`/`actual_pdf` のような `pdf` で始まらない receiver を落とす。両者の和でもまだ `flpdf-qtest-tools/src/compare.rs:26,28` と `crates/flpdf/src/reader.rs:1944,1954` を取りこぼすため、行の数字は上記の「`\.resolve\(` 全件から非 `Pdf` receiver 23 件を引く」方式を採る。2026-09-08（`flpdf-3yn9.48.29`）: reader/resolver cohort（本領域自身の `pdf.rs`/`reader.rs`/`reader/resolver.rs`/`reader/file_object.rs`/`cache.rs`）内の前置 `self.resolve(...)?; ...as_*()/is_null()` 呼び出しを 6 箇所（`pdf.rs::extension_level_handle` 3・`pdf.rs::root_handle` 1・`reader.rs::remove_security_restrictions` 2）閉じ、`Pdf::resolve`/`resolve_handle`/`resolve_handle_ref` 自体は残る ~249 件の他 cohort（`.48.30`〜`.48.33` の page/object helper・Job/CLI・writer/linearization・qtest tools consumer）向けに未削除のまま維持した（撤去自体は `.48.23`）。本 cohort の bridge caller は 0 件。2026-09-08（`flpdf-3yn9.48.30`）: page/object helper cohort の第1スライスとして `page_object_helper.rs::resolve_attribute_target`/`get_attribute_for_target`/`resolve_inherited_rotate_with_max_depth` の前置 `pdf.resolve(...)?`/`pdf.resolve_handle(...)?`/直接の `handle.try_dereference()?`（いずれも直後の非解決アクセサとの2段イディオム）を計4箇所閉じた（同ファイル残存 19 件・他cohortファイル未着手分は `flpdf-3yn9.48.70` へ）。 2026-09-08（`flpdf-3yn9.48.70`）: page/object helper cohort の `annotation_object_helper.rs` 全体（9 件の `.resolve(`）を閉じた。同ファイル残り3件（`:106` の `resolved_key` は解決済みハンドルを返すこと自体が契約、`:284`/`:302` は直後の `as_stream_dict` に `try_*` 版が無いため。いずれも A6 側の accessor が揃えば閉じられる）、他 page/object helper ファイル（`page_splice.rs`/`page_annotation_flatten.rs`/`page_form_xobject.rs`/`page_object_helper.rs` 残存分/その他未計測ファイル）は未着手のまま。 2026-09-08（`flpdf-3yn9.48.33`）: `flpdf-qtest-tools/src/driver/handle.rs`（上表の prod 12 件）は本スライスでは A8（`get_key`/`has_key`）のみ移行し、A6/A7 の `resolve` + `as_x` 統合には着手していない（12 件は未消化のまま残る）。見送り理由は**スコープのみ**である。当初この行には「同ファイルの `resolve_handle`（`:52-59`）が `value.object_ref()` を `pdf.resolve(value)` の**前**に捕捉しており、`DecodeParmsWarningSource::ObjectBody`/`ArrayItem` の帰属がその捕捉順序に依存するため畳み込めない」と書いていたが、これは誤りなので撤回する: `ObjectHandle::try_dereference`（`crates/flpdf/src/object_handle.rs:2561-2585`）は同一 canonical slot を in-place で解決するだけで `ValueIdentity` を書き換えず、`set_resolved`（`:2305-2318`）→ `replace_shared_state`（`:1916`）も値と children のみを差し替える。`identity.object_ref` へ代入するのは `remove_from_document`（`:2152`）とswap（`:2003-2004`）の 2 箇所だけで、いずれも解決経路には現れない。よって `object_ref()` は resolve の前後で同値であり、捕捉位置は帰属先を変えない。次段で本ファイルの A6/A7 に着手してよいが、無条件の clearance ではない: 畳み込み自体の安全性も accessor の戻り値で裏取りした: `try_is_null` は `Result<bool>`（`crates/flpdf/src/object_handle.rs:2856-2858`）、`try_as_dictionary`/`try_as_name`/`try_as_array` は `Result<Option<_>>`（`:2867-2872`/`:2906`/`:3005`）で、いずれも `try_dereference()?` の直後に非解決版をそのまま呼ぶだけなので、型不一致は従来どおり `None`/`false` を返し `Err` にはならない。したがって**前置 `pdf.resolve(&h)?` を持つサイト**では畳み込み後の `Err` 集合が畳み込み前と同一で、型不一致時の fallback 分岐も維持される（A8 の `get_key` → `try_get_key` が panic を `Err` へ移すのとは性質が異なる）。逆に**前置 resolve を持たない純粋 A6 サイト**は解決自体が追加されるため `Err` が新たに生じうる。ただし `try_*` 族には **`is_initialized()` の短絡を持つものと持たないものがある**（持つ: `try_as_array`（`:3005-3010`）・`try_is_array`・`try_is_dictionary`・`try_is_integer`・`try_is_name`・`try_is_number`・`try_is_scalar`。持たない: `try_as_dictionary`・`try_as_name`・`try_is_null`）。短絡を持つ族は未初期化 handle に対して `try_dereference` を呼ばず `Ok(None)`/`Ok(false)` を返すため、未初期化 handle が来うるサイトを畳み込むと、前置 `pdf.resolve(&h)?` が返していた `Error::Internal("attempted to dereference an uninitialized QPDFObjectHandle")`（`crates/flpdf/src/object_handle.rs:2561-2567`）が消える。この方向は qpdf 準拠側への移動である: qpdf の `dereference()` は未初期化で **false を返すだけで throw せず**（`libqpdf/QPDFObjectHandle.cc:2376-2383`）、`asArray`/`asDictionary`/`asName` はいずれも `dereference() ? obj->as<...>() : nullptr`（`:253-256`/`:265-268`/`:283-286`）、`isArray`/`isDictionary` も `dereference() && ...`（`:426-429`）で、未初期化 handle に対して例外を投げるのは `unparseResolved`/`getJSON` 系だけ（`:1587-1593`/`:1616-1624`）。したがって畳み込み時は「`Err` が消えること自体は regression ではない」と扱ってよいが、その `Err` に依存したテスト assertion があれば更新が要る。短絡の有無が族内で揃っていない点（`try_as_dictionary`/`try_as_name`/`try_is_null` は未初期化で `Err` を返し qpdf と食い違う）は別途 follow-up。本ファイルの 12 件は A7（前置 resolve あり）として計上しているが、着手時には各サイトが実際に前置 resolve を持つことを個別に確認し、`resolved_decode_params_handle` 系の帰属テスト（`non_dictionary_decode_parameter_warnings_keep_indices_and_types` 等）が同じ帰属を返すことを probe してから進めること — qtest の parity 台帳は別リポジトリ `fulgur-rs/flpdf-qtest` が保持するため、`cargo test --workspace` の緑だけでは帰属 regression を検出できない。 2026-09-08（`flpdf-hkty`）: 本ファイルの12箇所を実地監査した結果、変換対象は0件と判明——qpdf 側の対応は `test_driver.cc` の直接呼び出しではなく `QPDF_Stream::filterable`（`libqpdf/QPDF_Stream.cc:379-484`）で、こちらは値 getter を必ず非throwing な型 gate の内側でしか呼ばない —— `getName()` は `isName()` の真分岐、`getArrayItem(i)`/`getName()` は `isArray()` の真分岐の中だけで、型が合わない配列要素は getter を呼ばず `filters_okay = false` に落とす（`libqpdf/QPDF_Stream.cc:391-409`）。そのため type warning は出得ず、`filterable` 自身が出す warning は `filters_okay` が false のときの `stream filter type is not name or array` （`:411-415`）だけである。よって flpdf 側の非解決 `as_x`/`is_null` は `try_get_*_value` へ寄せる対象（flpdf-5ed3 の test_02_09.rs と同型）ではなく、resolve 後の型分岐としてそのまま正しい。`resolve_handle` の生成物を `resolve_filter_structure_handle`（filter が直接値なら no-op）へ渡す設計により、`is_null()` の false 判定が常に resolve 側へフォールスルーすることを 既存テスト `an_indirect_filter_resolving_to_null_is_an_empty_filter_chain` で確認済み。唯一の未検証残件（`resolve_stream_dictionary_handle` の `decode_params_value.is_null()`、filterable=false または `/Filter` が直接 null の分岐でのみ resolve が skip されうる）は fixture 不足のため `flpdf-fj9t` へ切り出した。 |
| A8 | `QPDFObjectHandle::getKey` / `hasKey`（typeWarning と null/false fallback、例外伝播） | `libqpdf/QPDFObjectHandle.cc:965-989,2168-2189` | `crates/flpdf/src/object_handle.rs::ObjectHandle::get_key` / `has_key` と `try_get_key` / `try_has_key` | 残 caller は `rg -n '\.(get_key\|has_key)\(' crates --glob '*.rs'` で receiver を確認する。2026-09-06 は実装・例外境界を再確認し、全件数は再集計していない | mixed | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_get_key` | 両族とも resolve する。`crates/flpdf/src/object_handle.rs:3921-3938` の convenience 版は `try_*` の `Err` を panic にする。**通常の document-owned 型不一致は warning + null/false で成功し、それ自体が panic になるわけではない**。問題は lazy resolution / warning 配送失敗や contextless warning の例外を panic に変換すること。qpdf の `typeWarning` も dereference や contextless `warn` が throw しうるので「例外を投げない」という旧記述は訂正する。consumer ごとに fallible accessor へ移行する。2026-09-08（`flpdf-3yn9.48.29`）: reader/resolver cohort 自身のファイルを点検し、非 `try_` 版 `get_key`/`has_key`（panic convenience）呼び出しは 0 件だった。次 cohort の残 caller数は未再計測。2026-09-08（`flpdf-3yn9.48.70`）: `annotation_object_helper.rs` の非 `try_` 版 `get_key` 5 箇所（`resolved_key`/`get_appearance_stream` ×2/`build_page_content_for_appearance` ×2）を `try_get_key` へ寄せ、panic bridge を閉じた。同ファイル残り0件。 2026-09-08（`flpdf-3yn9.48.33`、qtest tools cohort 第1スライス）: `crates/flpdf-qtest-tools/src/driver/handle.rs` の prod 側 `get_key` 5 箇所（`resolve_stream_dictionary_handle` 内 3 箇所、`remove_identity_crypt_stages_handle` 内 2 箇所、いずれも `flpdf::Result` を返す関数内）を `try_get_key` + `?` へ移行した。`get_key`/`has_key` は `try_get_key(key).unwrap_or_else(panic)` の薄いラッパーで、受信側のみを resolve し返す子 handle 自体は resolve しないため、この変換は失敗経路（panic → `Result` 伝播）のみを変え、返り値の解決状態・タイミングは変えない（同ファイルのテスト 15 件、`resolve_stream_dictionary_handle`/`remove_identity_crypt_stages_handle` 経由の既存 assertion で確認済み）。`has_key` の prod caller は同ファイルに無し。同ファイルの A6/A7 は本スライスのスコープ外（A7 の注記参照）。 2026-09-08（`flpdf-3yn9.48.71`、qtest tools cohort 第2スライス）: `crates/flpdf-qtest-tools/src/driver/test_56_63.rs` の prod 側 `get_key` 16箇所全てを `try_get_key` + `?` へ移行した（`test_56_59_body` 1箇所、`run_test_60` 5箇所、`run_test_62` の `/Q1`/`/Q2`/`/Q3` 整数アクセサ検証ブロック 10箇所）。同ファイルは `resolve`/`resolve_handle`/非解決 `as_x` 系が元々ゼロで、A6/A7 のタイミング判断が 一切不要な純粋 A8 変換だった。全16箇所とも `flpdf::Result` を返す関数（または 同シグネチャのクロージャ）内にあり、シグネチャ変更は不要。`driver/mod.rs` の `test_62_integer_accessors_match_qpdf_output`（`/Q1`/`/Q2`/`/Q3` ブロック）、`tests/driver_cli.rs` の `test_60_completes_all_resource_merges_and_writes_output`（`run_test_60`）、`test_56_63::tests::test_56_59_body_runs_the_canonical_overlay_route`（`test_56_59_body`）の3テストが変更なしで green のまま、全16箇所の変換を実地に確認した。残り `flpdf-qtest-tools` cohort の実測値は `flpdf-3yn9.48.71` issue 本文参照。 2026-09-08（`flpdf-3yn9.48.71`、qtest tools cohort 追加スライス）: 続けて `driver/test_10_17.rs`（`get_key` 17箇所、`run_test_11`/`run_test_14`/`check_page_contents`/`run_test_16`/`run_test_17`）の prod 側 `get_key` を全て `try_get_key` + `?` へ移行した。`run_test_14` には「Force qdict but not qarray to resolve, matching qpdf's source order」という resolve タイミングが qpdf のソース順序に 厳密対応するコメント付きの箇所があるが、`get_key`/`try_get_key` はどちらも受信側のみを resolve し返す子 handle は resolve しないため、この変換は resolve の発生有無・タイミングを 一切変えない（`test_14_matches_qpdf_swap_and_replace_sequence`/`test_14_drains_the_repair_warning_from_resolving_qdict` が変更なしで green のままこの順序保存を確認）。同ファイルの A6 非解決アクセサ（`as_integer`/`as_array`）6箇所は 本スライスのスコープ外。 2026-09-08（`flpdf-3yn9.48.71`、qtest tools cohort 追加スライス）: `crates/flpdf-qtest-tools/src/compare.rs`（qpdf の `compare-for-test/qpdf-test-compare.cc` の `compareObjects` 移植、qtest ハーネス自身の比較オラクル）の prod 側 `get_key` 3箇所（`stream_is_xref`/`resolved_filter_names_exact`/`remove_consumed_crypt_stages`）を `try_get_key` + `?` へ移行した。3箇所とも直後に返された子 handle 自体への明示`pdf.resolve(&filter)?`/`pdf.resolve(&type_handle)?` を伴っており、この明示 resolve 行は 今回一切変更していない——`get_key`/`try_get_key` はどちらも受信側のみ resolve し子 handle は resolve しないため、直後の明示 resolve は変換後も引き続き必要かつ有効。同ファイルの 他の resolve+`as_x` 系（`compare_objects`/`compare_streams`/`resolve_compare_children` 等）は 本スライスのスコープ外。当初この理由を「`resolve_compare_children` の `seen` 循環検出が resolve 順序に依存するため」としていたが、これは撤回する: `seen` の一致判定は `ObjectHandle::is_same_object_as`（`Rc::ptr_eq`、`crates/flpdf/src/object_handle.rs:1601-1603`）で、resolve は同一 `Rc<RefCell<ObjectSlot>>` を in-place で書き換えるだけなので、resolve の前後で ポインタ一致は変わらない。実際の理由は別にある: `flpdf-qtest-tools` は `flpdf` クレートの外側で、`try_as_dictionary`/`try_as_array`/`try_as_name`/`try_as_integer` は `pub(crate)` のみ、`try_as_string`/`try_as_real` に至っては存在しない（`crates/flpdf/src/object_handle.rs` 全体を grep して確認）。この crate から `resolve + as_x` を `try_as_x` へ畳む手段自体が無く、`pdf.resolve(&h)?; h.as_x()` は現状 flpdf の pub API 境界内で書ける唯一の形——`driver/test_02_09.rs` 冒頭のモジュール doc も同じ理由を明記している（`.claude/rules/qpdf-port-design-patterns.md` 8 の pub 境界の話と同型）。解消するには `try_as_*` 族を `pub` にする、または `try_as_string`/`try_as_real` を新設するという flpdf 側の API 拡張判断が要り、qtest-tools/flpdf-cli 側の 個別ファイル修正では閉じられない。 2026-09-08（`flpdf-3yn9.48.71`、qtest tools cohort 追加スライス）: `crates/flpdf-qtest-tools/src/driver/test_02_09.rs` の局所ヘルパー `dict_key(pdf, handle, key)`（`pdf.resolve(handle)?; Ok(handle.get_key(key))`——`try_get_key` と全く同じ動作を手書きしていたもの）の呼び出し 21箇所全てを `handle.try_get_key(key)?` へ置き換え、`dict_key` 自体を削除した（未使用関数として clippy の `-D warnings` に掛かるため）。あわせて `dict_key` を経由していなかった直接呼び出し 3 箇所（`run_test_4` 内の `trailer.get_key(b"/QTest")`・`qtest.get_key(b"/A")`・`trailer.get_key(b"/QTest2")`、いずれも `flpdf::Result<()>` を返す関数内）も `try_get_key` + `?` へ移行し、同ファイルの prod 側 `get_key`/`has_key` は残り 0 になった。同ファイルの `resolve_handle`（`pdf.resolve` の 薄いラッパー）は残存箇所（15箇所）で引き続き使う——`as_string`/`as_real`/`as_array` 等 自身の resolve+as_x 折り畳みは、`try_as_dictionary`/`try_as_array`/`try_as_name`/`try_as_integer` が `pub(crate)` のみで `try_as_string`/`try_as_real` は未実装のため、本 crate（`flpdf-qtest-tools`）からは実行不能で対象外（`flpdf` 側の pub 可視性判断が 別途必要、同ファイル冒頭のモジュール doc に同じ理由の記載あり）。 2026-09-08（flpdf-5ed3）: 上記の「pub(crate)/try_get_string_value 未実装」という診断を訂正する——try_get_string_value/try_get_utf8_value/try_get_numeric_value/try_get_array_item/try_get_array_as_vector は 2026-08-30 の 03b0b0c1 で既に pub 実装済みで、単に本 issue が把握していなかっただけだった（try_as_* の pub(crate) 化自体は無関係——qpdf の asX 系は include/qpdf/QPDFObjectHandle.hh:1365 の private: 直後にあり、そもそも public ではない）。test_02_09.rs の resolve_handle+as_x 系（run_test_2/3/4/5）を test_driver.cc の対応行と突き合わせ、値取得を伴う 7 箇所を既存の try_get_*_value/try_get_array_item へ移行した。**run_test_5 の /QStrings・/QNumbers の 2 箇所は移行対象外として残す**——ここは値ではなく容器が配列かどうかの型分岐で、qpdf 側も `test_driver.cc` が `isArray()` で分けたうえで要素ごとに値 getter を呼ぶ形（`qpdf/test_driver.cc` の test_5）。`resolve_handle` で容器を解決してから非解決 `as_array()` で分岐する現在の形が対応しており、`try_get_array_as_vector` に寄せると配列でない容器で型 warning を出す挙動が変わる（flpdf-3yn9.48.44 が確立した「driver/handle.rs の DecodeParms 帰属再構築を offset 直採取へ寄せる」パターンと同型）。driver/handle.rs 側の残り12箇所は本 issue のスコープ外として flpdf-hkty へ切り出す。 |
| A9 | `QPDF::getAllObjects`（`fixDanglingReferences` → `obj_cache` 全走査） | `libqpdf/QPDF.cc:1239-1269,1285-1295` | `crates/flpdf/src/reader.rs::Pdf::get_all_objects`（`pub`） | prod callers は writer/rewrite_renumber.rs、document_json.rs、reader.rs、qtest-tools の driver / renumber / metadata。全件数は今回未再集計 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::get_all_objects` | `.48.20`で履歴trailer参照の未解決登録をopen境界へ移し、Pdfに保持していた参照集合とgetAllObjectsのlate mint/eager resolveを撤去した。canonical列挙は各cache keyへnewIndirectを適用する。facadeのnumber 0 / generation 65535 filterとbootstrap cache自体の責務移行は後続のためmixedを継続する。 |
| A10 | 同じ `obj_cache` の列挙（qpdf は `getAllObjects` 1 本のみ） | `libqpdf/QPDF.cc:1285-1295` | private `Pdf::canonical_object_refs` / `Pdf::canonical_live_object_refs`; public complete boundary `Pdf::get_all_objects` | prod: canonical views only / test: integration helpers use `get_all_objects` | canonical | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::all_object_handles`（A9） | `.48.22` で qpdfにない `object_refs` / `live_object_refs` / `resolved_count` のfacade APIとcache mergeを削除。complete viewはcanonical cache keys、live viewはeffective xrefまたはdocument-owned allocationを使う。 |
| A11 | `QPDF::getObjectCount` / `QPDF::nextObjGen` | `libqpdf/QPDF.cc:1271-1283,1872-1880` | `ResolverHandle::get_object_count` / `next_obj_gen` と `Pdf::get_object_count` / `next_available_object_ref` | `rg -n -e 'get_object_count\(' -e 'next_obj_gen\(' -e 'next_available_object_ref\(' crates --glob '*.rs'` で facade と resolver を識別する。全件数は今回未再集計 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::next_obj_gen` | `.48.20`で`Pdf::get_object_count`はcanonical cache-key最大値に委譲し、列挙によるidentity更新を除いた。public makeIndirect factoryも`next_obj_gen`へ統一済み。`next_available_object_ref`は残るfacade consumer用で、legacy混じり`object_refs()`を使うためA11全体はmixedを継続する。 |
| A12 | `QPDF::makeIndirectObject` → `makeIndirectFromQPDFObject`（`nextObjGen` で採番し `obj_cache[next]` に **同じ `shared_ptr` を** 入れる） | `libqpdf/QPDF.cc:1890-1897`、`libqpdf/QPDF.cc:1882-1888` | `crates/flpdf/src/reader.rs::Pdf::make_indirect_object_handle`（`pub`）と `crates/flpdf/src/reader.rs::Pdf::make_indirect_from_object_handle`（`pub`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::make_indirect_from_object_handle`（`pub(crate)`） | public make_indirect_object_handle: prod 31 (14 files) / test 89。両public factoryは同じresolver ownerへ委譲 | canonical | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::make_indirect_from_object_handle` | `.48.20`でpublic clone allocatorを撤去。initialized guard → canonical nextObjGen → 同じQObjectをcache登録 → shared ValueIdentityを更新する。indirect/reserved/destroyedと別document入力も受け付け、解決・tree claim・子再帰を行わない。cache lookup/列挙は要求keyにidentityを戻す。`make_indirect_object_owner_tests.rs`とC++ oracle probesでalias、共有replacement、採番上限、履歴trailer参照の採番、writer出力のgolden byte一致を検証。 |
| A13 | `QPDF::removeObject`（private。xref erase → cache値をnullにassign → og解除 → cache erase） | `libqpdf/QPDF.cc:1995-2005`, `include/qpdf/QPDF.hh:1041` | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::remove_object` | 唯一の facade caller `crates/flpdf/src/reader.rs::Pdf::remove_object_handle` は `#[cfg(test)]` → prod: 0 / test: 1 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::remove_object` | canonical primitive自体はqpdfのprivate責務を持ち、test-onlyであることは削除理由にならない。`.48.21` ではこのprimitiveとtestを保持した。残るmixed部分はtest facadeが前後に加えるcache同期とmutation bookkeeping。`.46` でhandle-retaining variantは撤去済みで、**現在のcanonical removalがog/identityを保持するという旧probe前提は誤り**。A2/A15の後処理とともにfacadeの余分な同期を畳み、primitiveとその意味を検証するtestは保持する。 |
| A14 | public な「オブジェクト削除」は `replaceObject(og, newNull())`（`removeObject` は内部専用） | `include/qpdf/QPDF.hh:374-382`、`include/qpdf/QPDF.hh:384-386` | A16 の `crates/flpdf/src/reader.rs::Pdf::replace_object`（`pub`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::replace_object` | `delete_object` の production/test caller は 0（`scripts/qpdf-route-callers.py --symbol delete_object --expect-zero`） | canonical | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::replace_object` | `.46` で flpdf 固有の `Pdf::delete_object` とその consumer/test callers を撤去した。signature value stripping は qpdf と同じく `/V` を外すだけで signature dictionary を eager delete せず、明示的な null replacement が必要な test は public `replace_object(og, ObjectHandle::null())` を使う。 |
| A15 | （qpdf に対応物なし） | qpdf source/include に object-cache synchronization は存在しない | 削除済み（`synchronize_cache_with_resolver_xref` / `legacy_resolution_state_synced`） | prod: 0 / test: 0 | canonical | none | `.48.22` で二重cacheのrecovery synchronizationと `refs_after_xref_recovery` を撤去。recovery後のxrefはResolverCoreの同じsource table/cacheが直接正本になる。 |
| A16 | `QPDF::replaceObject(og, oh)`（indirect/未初期化を `std::logic_error` で拒否 → `updateCache(og, obj, -1, -1)`） | `libqpdf/QPDF.cc:1985-1993`、`libqpdf/QPDF.cc:1842-1858` | `crates/flpdf/src/reader.rs::Pdf::replace_object`（`pub`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::replace_object`（`pub(crate)`） | prod: 17 (json/input.rs ×4, reader.rs ×2, writer.rs ×2, page_annotation_flatten.rs ×2, `crates/flpdf-qtest-tools` driver/test_10_17.rs ×2, embedded_files.rs, page_extract.rs, object_copy.rs, job/outline_dest_remap.rs, job/page_merge.rs) / test: 113 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::replace_object` | 中核の `updateCache` 相当は canonical へ委譲されているが、facade が前後に qpdf 非対応の 3 操作を挟む（`synchronize_cache_with_resolver_xref`、`qpdf_parsed_xref_stream_refs`/`qpdf_dangling_refs` からの除去、`crates/flpdf/src/reader.rs::Pdf::replace_object`）。`mark_object_handle_mutated` は `flpdf-3yn9.48.24` で撤去済み。空集合からの無効なremoveは `.48.21` で撤去した。同じ「値の差し替え」が canonical cache と legacy 2 集合の両方に記録されるため、片方だけを見る consumer（A10）と結果が食い違いうる。 |
| A17 | `QPDF::swapObjects(og1, og2)`（**先に両方 resolve** → `swapWith` で value と og を交換） | `libqpdf/QPDF.cc:2284-2291`、`libqpdf/qpdf/QPDFObject_private.hh:121-130` | `crates/flpdf/src/reader.rs::Pdf::swap_objects`（`pub`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::swap_objects`（`pub(crate)`） | prod: 3 (reader.rs `:1555`、`crates/flpdf-qtest-tools` driver/test_10_17.rs `:295`,`:323`) / test: 7 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::swap_objects` | A16 と同型。空のremoved-ref集合へのremoveは `.48.21` で撤去した。加えて `CacheEntry::Deleted` / `CacheEntry::Missing` / `CacheEntry::Reserved` の tombstone を手で消す分岐（`crates/flpdf/src/reader.rs:1432-1450`）を持ち、コード内コメント自身が「qpdf has no persistent "deleted" or "missing" tombstone」と A2 の逸脱を明記している。 |
| A18 | （qpdf に対応物なし。qpdf の writer は `obj_cache` を走査するだけで dirty bit を持たない） | `probe: rg -ni 'dirty' /home/ubuntu/.cache/flpdf/qpdf-11.9.0/libqpdf /home/ubuntu/.cache/flpdf/qpdf-11.9.0/include → 0 hits`、`include/qpdf/QPDF.hh:1467`（obj_cache のみ） | 削除済み。`crates/flpdf/src/reader.rs` の dirty API と `crates/flpdf/src/pdf.rs` の dirty set は撤去され、mutation は共有 canonical handle graph を直接更新する | dirty tracking symbol scan: prod 0 / test 0（`crates/flpdf/src`、`crates/flpdf-cli/src`、`crates/flpdf-qtest-tools/src`）。writer は `ResolverCore::object_cache` の live handles を直接走査する | canonical | absent | `flpdf-3yn9.48.24` で qpdf に存在しない dirty bookkeeping と全 caller を撤去。`QPDF::replaceObject` は共有 cache value を更新し（`libqpdf/QPDF.cc:1986-1993`）、`QPDFWriter` は cache を直接準備・列挙する（`libqpdf/QPDFWriter.cc:2036-2195,2909-2915`）。既存の mutation/output 回帰テストと route contract で、明示通知なしの live graph 観測を固定した。 |
| A19 | `QPDF::closeInputSource`（`m->file` を `InvalidInputSource` に差し替えるだけ。cache は触らない） | `libqpdf/QPDF.cc:277-281`、`include/qpdf/QPDF.hh:162-166`、`libqpdf/QPDF.cc:99-106` | `crates/flpdf/src/pdf.rs::Pdf::close_input_source`（`pub`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::close_input_source`（`pub(crate)`、`crates/flpdf/src/reader/resolver.rs:2062`） | `rg -n 'close_input_source\(' crates` の宣言 2 行を除いた prod 2: `Pdf::close_input_source` の外部呼び出しは `crates/flpdf-qtest-tools/src/driver/test_72_79.rs:276` の 1 件のみ、もう 1 件は `crates/flpdf/src/pdf.rs:227` の canonical への委譲 / test: 2 (reader.rs `:2280`, resolver.rs `:5641`) | canonical | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::close_input_source` | `Pdf::close_input_source` は resolver の差し替えに加えて `set_input_source_stay_open(false)` を呼ぶ（`crates/flpdf/src/pdf.rs:226-229`）。これは qpdf の `ClosedFileInputSource::stayOpen` を持つ file source が `m->file` 置換で最後の owner を失う挙動の再現で、doc に理由が明記されている（同 `:218-225`）。逸脱ではなく `shared_ptr` reset 相当の補完。 |
| A20 | `QPDF::~QPDF`（`xref_table.clear()` → `obj_cache` 全件 `disconnect()` → `ot_null` 以外は `destroy()`） | `libqpdf/QPDF.cc:215-236`、`libqpdf/QPDFObject.cc:13-17`、`libqpdf/qpdf/QPDFObject_private.hh:19-180` | `crates/flpdf/src/pdf.rs::Pdf::drop`（`impl Drop for Pdf`、`crates/flpdf/src/pdf.rs:236-253`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::disconnect_all`（`:1862-1871`）。**owner-less bootstrap 期には第 2 の walk** `crates/flpdf/src/xref.rs::BootstrapCache` の `Drop`（`:183-223`） | `.disconnect()` prod: 2、`crates/flpdf/src/reader/resolver.rs:1869`（`disconnect_all` 内）と `crates/flpdf/src/xref.rs:220`（`BootstrapCache::drop` 内）/ test: 26（全て object_handle.rs） | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::disconnect_all` | `ObjectHandle::disconnect`（`crates/flpdf/src/object_handle.rs:2537-2560`）自体は qpdf の `disconnect()`+`destroy()` の組を `ot_null` を `destroy` しない条件込みで再現しており正確。`.48.15.1` で canonical `disconnect_all` は `source_xref_entries.clear()` を先に実行してから canonical object cache の handle を disconnect/destroyし、qpdfの clear-before-disconnect 順序（`QPDF.cc:215-236`）を揃えた。mixed の理由は **owner-less residual の walk が 1 本残る**こと: canonical cache を歩く `disconnect_all` と、owner-less `BootstrapCache` 自身の別 handle map（`BootstrapHandleState.handles`）を歩く `Drop`。qpdf の parse は最初から `m->obj_cache` に積むので teardown は 1 本しかない。2026-09-08（`.48.72`）: その owner-less public loader を撤去し、二重 document state と第 2 の teardown walk も production から消えた。`disconnect` は `slot.object_ref.is_none()` で **早期 return する**（`crates/flpdf/src/object_handle.rs:2540-2542`）ので `slot.resolver` を残す経路が形式上あるが、それが起きるのは (a) 直接 handle — `try_dereference` は `object_ref` が無い時点で `Ok(())` を返すので resolve 経路自体が無い、(b) 既に 1 回 disconnect 済みの handle — その 1 回目で `resolver` は `None` になっている、の 2 つだけで、いずれも「teardown 後に resolve が成功する」窓にはならない。 |
| A21 | 「間接参照である」ことは値の種類ではなく handle の og が担う（`QPDFValue` 派生に `Reference` は無い） | `include/qpdf/QPDFObjectHandle.hh:1629-1639`、`probe: ls $Q/libqpdf/qpdf/QPDF_*.hh → Array/Bool/Destroyed/Dictionary/InlineImage/Integer/Name/Null/Operator/Real/Reserved/Stream/String/Unresolved の 14 ファイルのみ、QPDF_Reference.hh は無い` | `crates/flpdf/src/object_handle.rs::ObjectValue`（`pub(crate)`、宣言は `crates/flpdf/src/object_handle.rs:1105`） | `ObjectValue::Reference` 参照は 3 件のみで、いずれも **不在を保証する guard か説明コメント**（`crates/flpdf/src/json_inspect.rs:19` のコメント、`crates/flpdf/tests/final_object_model_route_tests.rs:47,97`）→ prod: 0 / test: 2 | canonical | `crates/flpdf/src/object_handle.rs::ObjectValue` | **背景情報の訂正**: 依頼文と `docs/qpdf-correspondence.md` §1 は `ObjectValue::Reference` を「削除予定」として扱うが、**すでに削除済み**で `crates/flpdf/tests/final_object_model_route_tests.rs` が再導入を禁じている。同様に `resolve_to_terminal*` は `probe: rg -n 'resolve_to_terminal' crates --glob '*.rs' → 0 hits` で存在せず、`Pdf::resolve_handle` の doc も「the canonical value model has no reference-as-value variant」と明記する。`Unresolved`/`Reserved`/`Destroyed` の 3 sentinel も qpdf の同名 value 型に 1:1 対応。**この行は経路ではなく値型の行**である — entrypoint 欄が指すのは呼び出し口ではなく `ObjectValue` という型そのもので、`ObjectValue::Reference` と `resolve_to_terminal*` が実際に消えていることを確認するために記録している。したがって `canonical` の定義（唯一の正本であり cite した qpdf code と 1:1）は **型レベルで**適用される: flpdf の値ファミリが qpdf の `QPDFValue` 派生 14 種と 1:1 で、参照を表す第 15 の variant を持たない、という意味。 |
| A22 | （qpdf に対応物なし。qpdf は live input source を読む） | `libqpdf/QPDF.cc:1360-1398,1541-1697` | 削除済み（旧 `resolver.rs` の `read_window`/`read_to_owned`） | prod: 0 / test: 0 | canonical | absent | 2026-09-08（`.44`）: `read_window`/`read_to_owned` の外部 caller（`qtest_read_source_object_with_retry`/`parse_source_file_object_at` 経由で `driver/test_0_1.rs` へ辿り着いていた分）が `try_get_parsed_offset()` の生成時キャプチャへの置換（E27 参照）で 0 になった。2026-09-08（`.25`）: 残っていた内部 caller（`qtest_read_source_object_with_retry`/`object_body_start_within`/`parse_source_file_object_at`/`parse_source_file_object_handles`/`SourceFramingHandles`）ごと `read_window`/`read_to_owned`/`BULK_READ_CHUNK` を撤去した。qpdf に対応物のない補助経路が丸ごと消えたため canonical（absent）に区分し直す。 |
| A23 | （qpdf に対応物なし。dictionary key は `/` 付き decoded name） | `libqpdf/QPDFParser.cc:464`, `libqpdf/QPDF_Name.cc:27-49`、stream-filter ownerは`libqpdf/SF_FlateLzwDecode.cc:22-73` / `libqpdf/QPDF_Stream.cc:33-50` | `crates/flpdf/src/object_handle.rs::legacy_dictionary_key` と `canonical_dictionary_key` | 実呼出 prod: 2 — parser.rs:631,738（いずれも完全修飾呼び出しで import 行は残らない）。writer/object.rsのname emissionは`.48.35`でcanonical keyを直接扱う経路へ移行し、stream_filter.rsのkey判定3 callerは`.48.36`で移行済み / test: 0 | bridge | `crates/flpdf/src/object_handle.rs::canonical_dictionary_key` | 旧 `Object` / `Dictionary` は削除済み。残るhelper責務はparser warningの1群のみ（writer name emissionは`.48.35`で外れた）。stream-filterはqpdf同様slash付きkey比較へcutover済みで、raw slashless keyは正規化せずunknownとして扱う。parser/writerの残callerが0になるまでhelper定義は残す。 |
| A24 | `QPDF::resolveObjectsInStream`（`resolved_object_streams` で二重展開防止、xref 再チェックで上書き済みメンバーを cache しない） | `libqpdf/QPDF.cc:1756-1833` | canonical 側は `crates/flpdf/src/reader/resolver.rs::ResolverCore` の `resolved_object_streams`（`crates/flpdf/src/reader/resolver.rs:324-327`）。facade 側には `crates/flpdf/src/pdf.rs:136` の `compressed_member_parents` provenance map が残る | `compressed_member_parents` prod: 6 (3 files) / test: 4。A14 専用だった ObjStm 昇格 helper は `.46` で撤去 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverCore` | canonical 側の ObjStm 展開は qpdf に対応する一方、`compressed_member_parents` は legacy cache synchronization の移行状態を記録する flpdf 側 provenance で、qpdf の `ObjCache` には対応物がない。A2/A15 と同じ legacy cache 列を畳む段階まで保持する。 |

2026-09-08（`flpdf-1f9f`）: owner-less bootstrap の ObjStm member parserも
member本体と辞書・配列内の nested direct valueへ同じ description contextを渡すようにした。
qpdf は member の警告を decoded InputSource 名（`<file> object stream N`、
`libqpdf/QPDF.cc:1793-1805`）・parser へ渡す `object M 0`（`:1451-1459`）・parsed offset の
3 つから `QPDFParser::warn` で組み立てる（`libqpdf/QPDFParser.cc:509-513`）。flpdf の
description template はそのレンダリング済み prefix 全体を保持する（`$PO` が offset の
プレースホルダ、`crates/flpdf/src/object_handle.rs:940`）ので、入力 description と
`object stream N` を template に含める — canonical reader の
`object_stream_description_template` と同形。これは
A24の cache/recheck 責務とは独立した description propagation の補正であり、canonical
ResolverHandle側の object-stream routeや reconstruction-only bounded windowは変更しない。
2026-09-08（`flpdf-5snx`）: qpdf の未初期化 handle 契約を
`try_as_dictionary` / `try_as_name` / `try_is_null`へ反映した。
`dereference()` は falseを返し、前二者は `Ok(None)`、後者は `Ok(false)`
となる（`libqpdf/QPDFObjectHandle.cc:2376-2383,265-268,283-286,353-356`）。
値要求経路の `try_dereference` Internal errorと、initialized handleの
resolver error伝播は保持する。A6/A7のconsumer移行とqtest exceptionsは対象外。

2026-09-08（`flpdf-92r5`）: owner-less `BootstrapHandleDocument` に
`DocumentResolver::warn` と `warn_stream_data` を実装し、qpdfの
`QPDF_Stream::warn` → `QPDF::warn`（`libqpdf/QPDF_Stream.cc:695-698`,
`libqpdf/QPDF.cc:487-494`）と同じくwarningを収集してdecodeを継続するようにした。
`ObjectHandle::stream_data_warning`（`crates/flpdf/src/object_handle.rs:6650-6685`）の
parsed offsetあり／なし両方のboundaryを対象にし、ObjStmのrecoverable codec warningで
memberを失わないことをRED/GREENテストで確認した。A24のcanonical cache/recheck
責務、reconstruction-only bounded window、qtest exceptionsは対象外。

### A6の追加確認: getParsedOffsetもlazy accessor

`.40` の C8/C25 payload-helper cascade cleanupで、A7の旧
`Pdf::resolve_qpdf_json_handle`（JSON payload helper専用、他callerなし）も撤去した。
一般の `Pdf::resolve` / `resolve_handle` bridge は本記録の対象外であり、別issueの範囲を維持する。

2026-09-06: `QPDFObjectHandle::getParsedOffset` は `dereference()` 後にoffsetを返す
（`libqpdf/QPDFObjectHandle.cc:1875-1882`）。現Rustの
`crates/flpdf/src/object_handle.rs:3671-3673` はslotの素読みであり、
`crates/flpdf/src/job/check.rs:652` は未解決handle取得直後にこれを呼ぶ。
canonical getter契約とcheck consumerを最初のsliceとし、残consumerを順に移行する。
parser内部の呼出（`crates/flpdf/src/parser.rs:428,1758`）はparse中に間接解決を
起こさないqpdf契約も確認する必要があるため、全呼出の機械置換にはしない。
E27の手製source metadata再parseと関連するが、stream data開始offsetとは別の値である。

2026-09-08（`flpdf-3yn9.48.28` 実装時点での訂正）: 上記の `check.rs:652` 呼出は
`e91e9913`（`flpdf-3yn9.48.27.2`、typed qpdf warnings/catches移行）で既に撤去
済みであることを確認した — `linearization_parameter_offset` が
`pdf.get_object_handle(candidate).get_parsed_offset()` を呼ぶ経路ごと
`pdf.source_last_offset()` 直接呼出に置き換わっており、`job/check.rs` は
もはや `get_parsed_offset` を一切呼ばない。記録された「check consumerを
最初のslice」という前提は着手時点で無効だった（設計パターン4「記録された
依存順序を疑う」の該当例）。canonical getter契約自体は
`ObjectHandle::try_get_parsed_offset`（`crates/flpdf/src/object_handle.rs`、
`get_parsed_offset` の直後）として実装し、代わりに
`crates/flpdf/src/json/input.rs::JsonReactor::replace_object`
（`QPDF_json.cc:441-445` の `replacement.getParsedOffset()` 呼出に対応、唯一の
非parser内部・非E27対象の生きた consumer）を最初のsliceとして移行した。
qpdf 11.9.0 オラクル確認: 一度も定義されないobject番号を不正値として使うと
offsetなし（`-1`）で一致するが、既出objectを使うケースはqpdf実機で offset
110・flpdf側で offset 126 と乖離することを発見した（`flpdf-0tsv` で追跡、
`set_object_description`/`Json::start()` のcontainer位置トラッキングの
真因調査が必要、本issueのlazy-dereference契約とは無関係な別種の逸脱）。
CLI（`flpdf-cli/src/main.rs:7146`）・qtest metadata
（`flpdf-qtest-tools/src/metadata.rs:261,286`）はE27の対象consumerのため
引き続き未移行（`.25`/`.37`/`.44` が追跡）。

2026-09-08（`flpdf-3yn9.48.31`、Job/CLI JSON section cohort）: qpdfの
`QPDFJob::doJSONPages`/`doJSONEncrypt`（`QPDFJob.cc:1030-1093,1206-1279`）に
対応する `crates/flpdf/src/job/json_sections.rs` の production 区画を
`ObjectHandle` resolving accessorへ切り替えた。`collect_content_refs` と
`image_to_json` は `try_dereference`/`try_as_*` を直接使い、encrypt dictionary
projection は `try_as_dictionary`/`try_as_integer`/`try_as_name` と
`effective_length_bits` の canonical resolverへ委譲する。旧 production 区画を
`rg -n '\.resolve(?:_handle|_handle_ref)?\s*\('` と
`rg -n '\.(as_dictionary|as_array|as_integer|as_name|get_key|has_key|is_null)\s*\('`
（最初の `#[cfg(test)]` 行 1226 より前）で再計測した結果、bridge/accessor
caller はそれぞれ **0 / 0**（変更前は 15 / 16）。streamの辞書viewだけは
qpdfの `getDict`（`QPDFObjectHandle.cc:1257-1262`）に相当する
`try_dereference` 後の `as_stream_dict` として残る。失敗は `Result` のまま
伝播し、qpdfのsection order・helper ownership・JSON出力は変更しない。
残る Job/CLI の他consumer（`job/acroform_field_prune.rs`、`attachments.rs`、
`page_merge.rs`、`rotate.rs`、`page_split.rs`、`json_sections.rs`外のJSON/CLI
caller等）は次の限定sliceで再計測する。

### A6/A7/A8 page-object-helper residual cohort `flpdf-3yn9.48.23.1` (2026-09-08)

The bounded non-qtest `crates/flpdf/src/page_object_helper.rs` cutover now has
zero production occurrences of `.resolve(`, `resolve_handle`,
`resolve_handle_ref`, `get_key`, or `has_key` (measured by a production-only
source contract). Page/Form resource, annotation, rectangle, matrix, and
inherited-attribute reads use the resolving `try_*` accessors; the resolver
error path is covered by `page_helper_propagates_unowned_resolution_errors`.
The remaining direct `as_*` observations in this file are limited to
programmatically parsed inline-image values, newly constructed writer values,
or numeric fallback after the handle has already been resolved; they are not
caller-side resolution bridges. The qpdf order and error boundary remain
anchored to `libqpdf/QPDFObjectHandle.cc:240-446,965-989,2168-2189`.

Before this cohort, the route-caller audit measured global production residuals
of `Pdf::resolve` 219, `Pdf::resolve_handle` 154, and
`Pdf::resolve_handle_ref` 14; after it, 200, 145, and 14 respectively. Panic
`get_key`/`has_key` residuals changed from 92/8 to 88/6. These are global
residuals in later cohorts, qtest-tools, and other page/object files, not a
claim that the parent `.48.23` route is complete. qtest exceptions remain
outside this cohort.

### A6/A7 page-annotation-flatten slice `flpdf-3yn9.48.23.3` (2026-09-09)

The production path in
`crates/flpdf/src/page_annotation_flatten.rs` now has zero explicit
`Pdf::resolve`, `resolve_handle`, or `resolve_handle_ref` bridge callers.
Dictionary, null, integer, and array observations use the resolving
`try_*` accessors. Stream-dictionary inspection retains an explicit
`ObjectHandle::try_dereference` immediately before the non-resolving
`as_stream_dict`, because that is the qpdf `getDict`/stream accessor
boundary and no resolving Rust counterpart exists.

The slice preserves qpdf's
`QPDFPageDocumentHelper.cc:56-138` order: appearance and flag gates precede
resource/XObject mutation, page-content wrappers, and annotation removal.
The production source contract fixes the caller-zero boundary, while
`resolve_array_item_handles_propagates_an_unresolved_child_error` verifies
that a resolver failure remains a `Result`. Test-only legacy flatten modes and
the separate `AnnotationObjectHelper` residual helper boundary remain
outside this slice.

### A6/A7 page-label helper slice `flpdf-3yn9.48.23.4` (2026-09-09)

The production path in
`crates/flpdf/src/page_label_document_helper.rs` now has zero explicit
`Pdf::resolve`, `resolve_handle`, or `resolve_handle_ref` bridge callers.
The live label dictionary, catalog root, effective label, and prefix paths use
`try_dereference`, `try_as_dictionary`, `try_as_name`, `try_as_integer`, and
`try_is_null` at the qpdf accessor boundary. `/P` is the exception: it is
dereferenced through the canonical handle and then read with the silent
`as_string`, falling back to an empty prefix for any other type.
`getLabelForPage` copies `/P` verbatim without inspecting its type
(`QPDFPageLabelDocumentHelper.cc:38,48` — only `/St` gets an `isInteger()`
check at `:41`), so the warning-emitting `try_get_string_value` port must not
read it; doing so raised qpdf's string typeWarning and turned a
non-string-prefix run into exit 3.

The number-tree depth/error policy, raw `/S`/`/P`/`/St` presence, and
`/St` offset reconstruction remain unchanged and are anchored to
`QPDFPageLabelDocumentHelper.cc:1-96,104-133`. The route contract and
`label_range_propagates_an_unresolved_handle_error` cover the caller-zero and
Result-propagation boundaries. The rendering-only qpdf deviation and test-only
fixtures remain outside this slice.

### A6/A7 page-label provenance and foreign-owner follow-up `flpdf-hrgj` (2026-09-10)

The raw label consumer boundary now distinguishes qpdf's primary-owner path
from foreign/destroyed-source paths. Primary raw label copies register every
entry in the persistent foreign-object map as
`WriterObjectOrderKey::primary`, so QDF `%% Original object ID` comments use
the source ObjGen for indirect `/S` and `/P` descendants. Foreign label
dictionaries use `shallow_copy`: direct scalar values become destination-safe
copies while indirect descendants remain foreign and reach the canonical
writer ownership check. This matches qpdf's `handlePageSpecs` primary handle
handoff (`libqpdf/QPDFJob.cc:2511-2593`), split-pages handoff
(`libqpdf/QPDFJob.cc:2960-3010`), `QPDFWriter::enqueueObject`
(`libqpdf/QPDFWriter.cc:1072-1082`), and QDF provenance emission
(`libqpdf/QPDFWriter.cc:1774-1787`).

Pinned qpdf 11.9.0 probes cover valid indirect `/S` and `/P` labels: primary+
secondary `--pages` preserves the qpdf original-object comments; split-pages
and `--empty --pages` return qpdf's foreign/destroyed-QPDF failures instead of
silently copying the handles. The existing raw label dictionary semantics and
the rendering-only deviation remain unchanged.

### A6/A7 page-document final-tree slice `flpdf-3yn9.48.23.5` (2026-09-09)

The final-page clear path in
`crates/flpdf/src/page_document_helper.rs` no longer uses the explicit
`Pdf::resolve` followed by non-resolving `as_dictionary` inspection. The
existing canonical page-tree/root gates remain unchanged; only the
`/Pages` root validation at the final empty-tree mutation now uses
`try_as_dictionary`, preserving lazy resolver errors and the live root
identity.

The qpdf mutation order remains anchored to
`QPDFPageDocumentHelper.cc:37-52` and
`QPDF_pages.cc:253-266,304-316`. The production route contract and
`page_document_helper_qpdf_tests` cover caller-zero, cycle/error context,
and final-page empty-tree behavior. No qtest or qtest-exceptions route is
included.

### A6/A7/A8 outline-helper slice `flpdf-3yn9.48.23.6` (2026-09-09)

The production outline document/object helper cohort now has zero explicit
`Pdf::resolve`, `resolve_handle`, or `resolve_handle_ref` bridge callers
in the migrated accessors. Outline item title/count/destination reads use the
live handle resolver at the qpdf accessor boundary, while raw destination
array page operands remain un-resolved as required by qpdf's
`getDestPage` contract. Catalog/outlines and named-destination traversal
retain the existing live cache and sibling seen-set order.

The source and error boundaries are anchored to
`QPDFOutlineDocumentHelper.cc:16-21,47-90` and
`QPDFOutlineObjectHelper.cc:47-98`. The production route contract and
`resolve_value_handle_propagates_an_unresolved_handle_error` cover
caller-zero and Result propagation. Rendering-only deviations and qtest
exceptions remain outside this slice.

### A6/A7/A8 Job/page/resource/JSON consumer slice `flpdf-3yn9.48.23.8` (2026-09-10)

The remaining non-qtest production consumers in
`job/page_merge.rs`, `job/page_specs.rs`, `job/resource_pruning.rs`, `pages.rs`,
`pages/repair.rs`, `resources.rs`, `overlay_appearance_stream.rs`, and
`document_json.rs` now have zero explicit `Pdf::resolve`, `resolve_handle`, or
`resolve_handle_ref` calls and zero non-resolving `get_key`, `has_key`,
`as_dictionary`, `as_array`, `as_integer`, `as_name`, or `is_null` bridge
callers. The cutover uses the existing `ObjectHandle::try_dereference`,
`try_get_*`, `try_as_*`, and `try_is_*` boundary, preserving `Result` error
propagation and qpdf's accessor order.

The qpdf owner boundaries are `QPDFObjectHandle.cc:240-446,759-785,965-989`
for typed/key accessors and `:2168-2189` for warning/error delivery;
`QPDFPageObjectHelper.cc:224-263,318-399,486-649` for inherited attributes,
XObject traversal, parsing, and resource pruning; `QPDF_pages.cc:39-150` for
page-tree repair/enumeration; `QPDFJob.cc:2251-2632` for shared-resource policy
and page selection; and `QPDFJob.cc:958-1620,3094-3116` plus
`QPDF_json.cc:852-905` for JSON section order and document serialization.

Two silent post-resolution inspections remain intentionally explicit because
there is no resolving `try_as_string`/`try_as_real` counterpart: AcroForm `/T`
name decoding in `job/page_merge.rs` and rectangle real-number fallback in
`pages/repair.rs`. Stream-dictionary views likewise remain after the canonical
resolution step. These are qpdf-shaped direct type observations, not caller-side
resolution bridges. The new `job_page_resource_json_route_contract_tests` fixes
the production caller-zero boundary, and
`inherited_attribute_walk_propagates_an_unresolved_parent_child_error` verifies that a
resolver failure remains a `Result`. qtest and qtest-exceptions consumers remain
outside this slice.

### A6/A7/A8 linearization check/show accessor slice `flpdf-3yn9.48.23.9` (2026-09-10)

The bounded linearization consumer slice in
`crates/flpdf/src/linearization/check.rs` and `show.rs` now uses the resolving
`try_as_integer`, `try_as_array`, `try_as_name`, and `try_is_null` accessors for
qpdf's `/Linearized`, `/L`, `/N`, `/O`, `/P`, `/H`, `/S`, `/T`, and `/PageMode`
observations. The production route contract reports zero non-resolving
dictionary, array, integer, name, null, key, or `Pdf::resolve*` bridge calls in
these two modules. Silent `as_real`/`as_string` observations remain only where
there is no resolving Rust counterpart; qtest exception attribution and writer
emission routes are outside this slice.

The qpdf source boundary is `QPDFObjectHandle.cc:240-446,759-785,965-989,
2168-2189`, with the linearization owner at `QPDF_linearization.cc:84-230,
419-470`. Existing qpdf differential tests for `check-linearization`,
`show-linearization`, page-operation linearization, and deep linearization
remain the behavioral RED/GREEN evidence. The CRLF-normalized
`linearization_accessor_route_contract_tests` guards the production caller-zero
boundary on Windows as well as Unix.

### 2026-09-09 canonical cache cutover supersession

The A1/A2/A9/A10/A11/A13/A15/A16/A17/A24 rows above were authored before
`flpdf-3yn9.48.22` removed the facade cache. Their historical caller counts and
`mixed`/`bridge` descriptions of `ObjectCache`, `CacheEntry`,
`synchronize_cache_with_resolver_xref`, and `compressed_member_parents` are
superseded by the cutover note at lines 144-160. The current production symbols
are the private `Pdf::canonical_object_refs` and
`Pdf::canonical_live_object_refs`; the public qpdf enumeration boundary is
`Pdf::get_all_objects`. The route checker remains structural; this note is the
semantic owner record for the cutover until the next full matrix recount.

### A6/A7 page-splice accessor slice `flpdf-3yn9.48.23.2` (2026-09-09)

The production path in `crates/flpdf/src/page_splice.rs` had 16 explicit
`Pdf::resolve` calls paired with non-resolving dictionary, name, array, or
integer inspection. The bounded slice replaces those pairs with
`try_as_dictionary`, `try_as_name`, `try_as_array`, and
`try_as_integer`; the production source contract now measures zero
`.resolve(`, `resolve_handle`, or `resolve_handle_ref` callers in this
module. Page ordering, direct-leaf promotion, duplicate-page copying,
cycle/count validation, and mutation order are unchanged.

qpdf's `shallowCopy` dereferences before copying
(`QPDFObjectHandle.cc:2073-2079`). flpdf's `shallow_copy` is intentionally
non-resolving, so the duplicate-page copy site in `normalize_insert_pages`
retains the canonical `ObjectHandle::try_dereference` prerequisite rather than
reintroducing `Pdf::resolve`; the copy inside `collect_page_refs` is already
covered by the `try_as_dictionary` that classified the kid. The unresolved-child test
`leaf_count_of_propagates_an_unresolved_child_resolution_error` verifies
fallible resolver propagation, and the route-contract integration test fixes
the caller-zero boundary. The separate default page-tree depth policy remains
outside this accessor slice.

## unknown / probe

本領域は 24 行すべてを source と実行済み probe で分類できたため、`unknown` に落ちた行は無い。
以下は「分類は決まったが、cutover の設計にはさらに観測が要る」項目と、その probe。

1. **A2 / A10 — canonical source-xref key と object cache の実際の乖離量**。両者が食い違う入力が
   どれだけあるかは source からは決まらない。必要 probe:
   `cargo test -p flpdf --features qpdf-zlib-compat` を通した状態で、
   `Pdf::get_all_objects()` と canonical live view の結果差分を fixture 全体
   （`crates/flpdf/tests/fixtures/`）で取る一時ハーネスを書き、差分が出る fixture を列挙する。
   差分があるfixtureはcanonical cutoverのRED候補になる。差分ゼロでも責務同一性や
   全caller移行の証明にはならず、一括削除の根拠にはしない。writer等のconsumerを
   責務ごとに移行し、最後にcaller-zeroを確認してcacheを削除する。
2. **A12 — `.48.20`で観測差を解消**。public factoryのコンテナcloneで
   retained aliasのmutationが出力に届かないREDを確認し、canonical promotionへ移した。
   `tests/oracle/qpdf_make_indirect_object_probe.cc`と
   `qpdf_make_indirect_object_states_probe.cc`はqpdf 11.9.0で再昇格、共有値のobjgen、
   reserved/destroyed、未解決入力、最大IDの契約を確認する。
3. **A13 — canonical removalとtest facadeを区別する**。qpdfのcache erase / og解除
   （`libqpdf/QPDF.cc:1995-2005`）を担うprimitiveは現Rustにもある。旧記述の
   「flpdfはhandle identityを残す」は撤去済みvariantと混同していた。
   今後の検証対象はtest facadeのcache同期・mutation bookkeepingを外した後も、
   floating nullとcache/xref removalのcanonical契約が保たれること。privateで正当な
   qpdf primitiveとそのtestを、production callerが0であることだけで削除しない。
4. **A20 / A1(3) — bootstrap 期の handle が canonical cache に持ち越されるか**。
   `BootstrapCache::drop` は自分の `handles` を disconnect するが、bootstrap で作られた
   `ObjectHandle` が後で `ResolverCore::object_cache` にも入るなら、同じ handle が 2 回
   disconnect され（2 回目は `crates/flpdf/src/object_handle.rs:2540-2542` の早期 return）、
   逆に bootstrap 側だけが持つ handle は canonical の teardown walk から漏れる。
   必要 probe: `BootstrapCache::drop` の直前・直後で
   `ResolverHandle::registered_handle(ref).is_same_object_as(bootstrap_handle)` を
   全 fixture について評価する一時テストを書き、重なりの有無を確定する。
   併せて `crates/flpdf/src/reader.rs:1329` の `Pdf::resolver_is_uniquely_owned`
   （現状 `#[cfg(test)]`）が drop 直前に全 fixture で `true` を返すことも確認する
   （`ResolverHandle` を外部で `Rc` 保持する経路が生じると A20 の「窓は生じない」論拠が崩れるため）。


### 2026-09-10 current caller audit: outline/destination remap bounded cutover

`flpdf-3yn9.48.23.13` migrated the non-qtest production callers in
`crates/flpdf/src/job/outline_dest_remap.rs`. Before the cutover the fresh
tracker measured 2 `Pdf::resolve` callers in that file; after the cutover the
scoped production counts are zero for `resolve`, `resolve_handle`, and
`resolve_handle_ref`, while the existing `try_*` destination/accessor routes
remain in place. The route contract is
`crates/flpdf/tests/outline_dest_remap_route_contract_tests.rs`.

The qpdf source boundary is `libqpdf/QPDFJob.cc:2469-2470,2585-2608`:
original page-tree membership drives null-out, and surviving destination
references are remapped without dropping navigation entries. The handle-level
authority is `libqpdf/QPDFObjectHandle.cc:240-446,965-989,2168-2189`. The
remaining qtest exception and unrelated struct-tree/thread-bead, writer/CLI,
and stream caller counts are intentionally not included in this bounded slice.
### 2026-09-10 current caller audit: thread-bead bounded cutover

`flpdf-3yn9.48.23.14` migrated the non-qtest production callers in
`crates/flpdf/src/thread_bead_p.rs`: the fresh pre-cutover census was
`resolve_handle_ref: 6` and `Pdf::resolve: 2`. The post-cutover scoped counts
are zero, and the now-unused `Pdf::resolve_handle_ref` facade was removed from
`crates/flpdf/src/reader.rs`. The route contract is
`crates/flpdf/tests/thread_bead_route_contract_tests.rs`.

The qpdf boundary is `libqpdf/QPDFJob.cc:2469-2470,2599-2608` for page-driven
null-out and `libqpdf/QPDFWriter.cc:1110-1160` for dictionary null visibility;
the handle-level resolution/error behavior is
`libqpdf/QPDFObjectHandle.cc:2375-2383,240-446,965-989,2168-2189`. The
remaining qtest exception and unrelated struct-tree, writer/CLI, and stream
caller counts are intentionally not included in this bounded slice.

### 2026-09-10 current caller audit: page-extract bounded cutover

`flpdf-3yn9.48.23.15` migrated the two non-qtest production callers in
`crates/flpdf/src/page_extract.rs`: the fresh pre-cutover census was
`Pdf::resolve: 2`, at the copied-page `/Parent` mutation and duplicate-page
shallow-clone sites. The post-cutover scoped count is zero. The route contract
is `crates/flpdf/tests/page_extract_route_contract_tests.rs`.

The qpdf boundary is `libqpdf/QPDF_pages.cc:205-250`, where duplicate pages
are shallow-copied and `/Parent` is replaced, with receiver-resolution
contracts in `libqpdf/QPDFObjectHandle.cc:1200-1208,2073-2079` and the
`QPDFPageDocumentHelper::addPage` delegation in
`libqpdf/QPDFPageDocumentHelper.cc:36-53`. flpdf's `replace_key` already
resolves its receiver; `shallow_copy` intentionally does not, so the latter
retains an explicit canonical `try_dereference` at qpdf's boundary. Page
selection, duplicate identity, shared children, PageLabels, and writer
provenance remain outside the accessor-only change. qtest exceptions, the
thread-bead/struct-tree drop-family, and the separate merge/drop semantic
issue remain out of scope.

### 2026-09-10 current caller audit: AcroForm appearance renderer bounded cutover

`flpdf-3yn9.48.23.16` migrated the four non-qtest production
`Pdf::resolve` callers in
`crates/flpdf/src/form_field_object_helper/rendering.rs`: the
`resolve_canonical` helper and the widget boundaries in appearance
installation, Tx generation, and Ch generation. The post-cutover scoped
production count is zero. The route contract is
`crates/flpdf/tests/form_field_rendering_route_contract_tests.rs`.

The qpdf boundary is `libqpdf/QPDFFormFieldObjectHelper.cc:766-860` for
appearance selection, rectangle/font lookup, and `ValueSetter` installation;
`libqpdf/QPDFAcroFormDocumentHelper.cc:393-415` owns Tx/Ch dispatch; and
`libqpdf/QPDFObjectHandle.cc:240-446,789-824,965-989` owns resolving typed/key
access. flpdf now uses canonical `try_dereference`/`try_*` accessors at each
equivalent graph boundary. Silent stream-dictionary and parser-token/string-
real observations remain only after dereference where no equivalent silent
resolving accessor exists. Existing rendering layout and malformed-input
warning behavior are covered by the focused FormField and CLI qpdf tests;
qtest exceptions and unrelated AcroForm/FileSpec/signature routes remain out
of scope.

### 2026-09-10 current caller audit: optimization bounded cutover

`flpdf-3yn9.48.23.11` migrated the non-qtest production callers in
`crates/flpdf/src/optimization.rs` and
`crates/flpdf/src/optimization/inherited_attrs.rs`. Before the cutover the
fresh tracker measured `optimization.rs` `Pdf::resolve: 2` and
`optimization/inherited_attrs.rs` `Pdf::resolve: 3`, `resolve_handle: 2`,
`get_key: 3`, and `has_key: 1`; after the cutover each of those scoped
production counts is zero. The route contract is
`crates/flpdf/tests/optimization_accessor_route_contract_tests.rs`.

The qpdf source boundary is `libqpdf/QPDF_optimization.cc:70-78,117-187,190-245`:
qpdf resolves the live root before its `/Outlines` test, enumerates the four
inheritable keys in the page walk, resolves each value for the null-as-absent
decision, and uses the same ordered walk for direct and indirect descendants.
The handle-level authority is
`libqpdf/QPDFObjectHandle.cc:240-446,965-989,2168-2189`; flpdf now reaches it
through `try_dereference`, `try_get_key`, `try_has_key`,
`try_as_dictionary`, `try_as_array`, and `try_is_dictionary_of_type` rather
than through a caller-side `Pdf::resolve` or panic accessor. The remaining
qtest exception and unrelated writer/CLI/stream caller counts are intentionally
not included in this bounded slice.
### 2026-09-10 current caller audit: AcroForm field-prune bounded cutover

`flpdf-3yn9.48.23.12` migrated the non-qtest production callers in
`crates/flpdf/src/job/acroform_field_prune.rs`. Before the cutover the fresh
tracker measured 12 `Pdf::resolve` callers in that file; after the cutover the
scoped production counts are zero for `resolve`, `resolve_handle`,
`resolve_handle_ref`, non-resolving `as_dictionary`/`as_array`/`as_name`/
`is_null`, and panic `get_key`/`has_key`. The route contract is
`crates/flpdf/tests/acroform_field_prune_route_contract_tests.rs`.

The qpdf source boundary is `libqpdf/QPDFJob.cc:2585-2645` and
`libqpdf/QPDFAcroFormDocumentHelper.cc:235-365`: page selection snapshots
original page membership, retains fields associated with selected widgets, and
resolves field-tree children through qpdf's handle accessors. The common lazy
and warning/error boundary is
`libqpdf/QPDFObjectHandle.cc:240-446,965-989,2168-2189`. The remaining qtest
exception and unrelated writer/CLI/stream caller counts are intentionally not
included in this bounded slice.

## 2026-09-06 再監査の issue 対応

親 epic は `flpdf-3yn9.48`。下表は責務と実装 issue の対応であり、完了状態は `bd show <id>` で確認する。
各 issue の受入条件に qpdf 根拠、最初の consumer、残 caller と削除条件を記録した。

| 対象行 | Beads issue | 責務 / 移行 slice |
|---|---|---|
| `A1` / `A9` / `A20` | `flpdf-3yn9.48.12` | QPDF document stateをparse前から所有しclassic trailerを同じcacheへ生成する |
| `A1` / `A20` | `flpdf-3yn9.48.15`（child `.48.72` / `.48.73`） | owner-less public xref API/production bootstrap routeを`.48.72`、canonical warning live sinkとreplay/deferral撤去を`.48.73`で実施する |
| `A11` / `A12` | `flpdf-3yn9.48.20` | makeIndirectObjectのclone allocatorをcanonical identity promotionへ移行する |
| `A9` / `A13` / `A16` / `A17` | `flpdf-3yn9.48.21` | 書込み元が消えたqpdf_removed_refs空集合フィルタを撤去する |
| `A1` / `A2` / `A9` / `A10` / `A11` / `A13` / `A15` / `A16` / `A17` / `A24` | `flpdf-3yn9.48.22` | 最後のconsumer移行後にfacade ObjectCache・同期・provenanceを撤去する |
| `A6` / `A7` / `A8` | `flpdf-3yn9.48.23` | accessor移行後にpublic resolve・panic convenience bridgeを撤去する |
| `A18` | `flpdf-3yn9.48.24` | writer共有cache観測への移行後にdirty trackingを撤去する |
| `A22` | `flpdf-3yn9.48.25` | test0 cutover後にsource metadata再parse・window・64回retry budgetを撤去する |
| `A6` / `A22` | `flpdf-3yn9.48.28` | getParsedOffsetをlazy dereference契約へ揃えcheck consumerを移行する |
| `A6` / `A7` / `A8` | `flpdf-3yn9.48.29` | ObjectHandle resolving accessorへreader/resolver consumerを移行する |
| `A6` / `A7` / `A8` | `flpdf-3yn9.48.30` | ObjectHandle resolving accessorへpage/object helper consumerを移行する |
| `A6` / `A7` / `A8` | `flpdf-3yn9.48.31` | ObjectHandle resolving accessorへJob/CLI consumerを移行する |
| `A6` / `A7` / `A8` | `flpdf-3yn9.48.32` | ObjectHandle resolving accessorへwriter/linearization consumerを移行する |
| `A6` / `A7` / `A8` | `flpdf-3yn9.48.33` | ObjectHandle resolving accessorへqtest tools consumerを移行する |
| `A23` | `flpdf-3yn9.48.34` | legacy_dictionary_keyのparser warning consumerをcanonical nameへ移行する |
| `A23` | `flpdf-3yn9.48.35` | legacy_dictionary_keyのwriter name emission consumerをcanonical nameへ移行する |
| `A23` | `flpdf-3yn9.48.36` | legacy_dictionary_keyのstream-filter key判定 consumerをcanonical nameへ移行する |
| `A22` | `flpdf-3yn9.48.44` | qtest test0/1をcanonical pipe/loggerへ移し手製stream診断を撤去する |
| `A9` / `A11` | `flpdf-3sbf` | ObjGen object 0 の不変条件 |

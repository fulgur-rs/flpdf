# A. ObjectHandle / Resolver — object identity, lazy resolve, ownership, teardown

対象: `QPDF` が持つ object identity（`QPDFObjGen` → `ObjCache`）、lazy resolve の唯一経路
（`QPDFObjectHandle::dereference` → `QPDFObject::resolve` → `QPDF::Resolver::resolve` →
`QPDF::resolve`）、object の生成・置換・交換・削除（`makeIndirectObject` / `replaceObject` /
`swapObjects` / `removeObject`）、および input source の切り離しと `~QPDF` の teardown。
flpdf 側は `crates/flpdf/src/reader.rs`（`Pdf`）/ `crates/flpdf/src/reader/resolver.rs`
（`ResolverCore` / `ResolverHandle`）/ `crates/flpdf/src/cache.rs` /
`crates/flpdf/src/object_handle.rs`（`ObjectHandle` と `ObjectValue` は同一ファイル）/
`crates/flpdf/src/pdf.rs`（`Pdf` の struct 定義と `Drop`）が対応面。

**読み方の注意**: 本表の `classification` は「qpdf 責務に至る flpdf の経路が 1 本か」を問う
（README §3）。`docs/qpdf-correspondence.md` の ✅ 行でも、consumer 側に二重経路が残っていれば
ここでは `mixed` / `bridge` になる。

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

## route matrix

**caller の数え方（本ファイルの規約）**: `rg -n --glob '*.rs' '<call-shaped pattern>' crates` の出力から、
宣言行と `//` コメント行を除いて数える。**test** = `crates/*/tests/` 以下・`benches/`・`examples/`、
`*_tests.rs`（`crates/flpdf/src/json/input_tests.rs` は `crates/flpdf/src/json/mod.rs:18-19` で
`#[cfg(test)] mod input_tests;` と gate されている）、および `src/` ファイル内の
**桁 0 の `#[cfg(test)]` + `mod X { … }` ブロックの内側**（`crates/flpdf/src/object_handle.rs` は
この形の test mod を 21 個持ち、その **間に production コードが挟まる**ので、
「最初の `#[cfg(test)]` 以降は全部 test」という数え方は使えない。閉じ括弧は rustfmt により
桁 0 の `}`）。**prod** = それ以外。`fuzz/` は別枠で数える（本領域は全行 0 件）。
`crates/flpdf-cli` と `crates/flpdf-qtest-tools`（qpdf の `qpdf/test_driver.cc` 等に対応する
実バイナリ）は prod に数え、後者由来は notes に明記する。impl 内の `#[cfg(test)]` 付き単発関数
（例 `crates/flpdf/src/reader.rs:1328,1336,1587`）はこの規約では prod 側に落ちるため、
該当する行は notes で個別に断る。

| # | qpdf responsibility owner | qpdf evidence | flpdf current entrypoint | callers (prod / test) | classification | canonical owner | remaining bridge callers / notes |
|---|---|---|---|---|---|---|---|
| A1 | `QPDF::Members::obj_cache`（object identity の唯一の正本） | `include/qpdf/QPDF.hh:1467`、`include/qpdf/QPDF.hh:868-889` | `crates/flpdf/src/reader/resolver.rs::ResolverCore` の `object_cache` フィールド（`crates/flpdf/src/reader/resolver.rs:287-306`、private） | prod: 該当フィールドは `ResolverHandle` 経由でのみ触られる（`crates/flpdf/src/reader/resolver.rs` 内、`self.core.borrow*().object_cache`）/ test: 同上 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverCore` | qpdf の 1 つの `m->obj_cache` に対し flpdf は **3 つの map** を持つ: (1) canonical 側の `object_cache: BTreeMap<ObjectRef, ObjectHandle>`、(2) facade 側の `crates/flpdf/src/cache.rs::ObjectCache`（A2）、(3) xref bootstrap 期だけ生きる `crates/flpdf/src/xref.rs::BootstrapHandleState` の `handles`（`crates/flpdf/src/xref.rs:66-73`）。(3) は `resolving` / `resolved_object_streams` まで自前で持つ resolve state の完全な複製で、`Drop` に独自の disconnect walk がある（A20）。(1) は resolve/allocate の全経路が通るが、(2) は open 時の xref から作られ、以後 `crates/flpdf/src/reader.rs` の 12 箇所（`:1048,1066,1097,1157,1162,1169,1173,1265,1570,1574,1843,2027`）でしか更新されないため、通常の resolve では **更新されない**。(1) と (2) の乖離を後追いで埋めるのが A15。 |
| A2 | `QPDF::ObjCache`（`{object, end_before_space, end_after_space}` の 3 フィールド、private nested class） | `include/qpdf/QPDF.hh:868-889` | `crates/flpdf/src/cache.rs::ObjectCache` + `crates/flpdf/src/cache.rs::CacheEntry`（どちらも `pub`、`crates/flpdf/src/lib.rs:92` の `pub mod cache` と `:157` の `pub use` で crate 外へ公開） | `CacheEntry` prod: 47 (reader.rs, cache.rs, lib.rs) / test: 11。`ObjectCache` 型名 prod: 8 (pdf.rs, cache.rs, lib.rs, engine.rs) / test: 6 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverCore`（A1） | qpdf の `ObjCache` は「値 + 2 つの offset」だけで、**未解決/予約/削除は値の型**（`QPDF_Unresolved` / `QPDF_Reserved` / `QPDF_Null`）が表す。flpdf の `CacheEntry` は `Unresolved{offset}` / `Compressed{stream,index}` / `Resolved(handle)` / `Missing` / `Reserved` / `Deleted` の 6 状態を **cache 側**に持つ二重表現で、`Missing` と `Deleted` に至っては qpdf に対応する状態が無い（`removeObject` は cache cell ごと erase する、`libqpdf/QPDF.cc:1995-2005`）。qpdf の `end_before_space`/`end_after_space` に対応するのは cache ではなく handle 側の `ObjectSlot` フィールド（`crates/flpdf/src/object_handle.rs:1019-1020`）。可視性も qpdf の private nested class に対し `pub`（`.claude/rules/qpdf-port-design-patterns.md` 8 の根拠 1〜3 いずれにも該当しない）。 |
| A3 | `QPDF::getObject(QPDFObjGen)`（**resolve しない** handle 取得） | `libqpdf/QPDF.cc:1951-1959`、`include/qpdf/QPDF.hh:362-372` | `crates/flpdf/src/reader.rs::Pdf::get_object_handle`（`pub`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::get_object_handle`（`pub(crate)`） | `rg -n '\.get_object_handle\(' crates`（facade 経由と resolver 直呼びの合算）prod: 257 (64 files; うち `crates/flpdf-qtest-tools` の driver 群 `test_10_17.rs` 他) / test: 533 | canonical | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::get_object_handle` | `Pdf::get_object_handle` は 1 行の委譲（`crates/flpdf/src/reader.rs:1313-1321`）で、`or_insert_with` が qpdf の `if (!isCached(og)) { obj_cache[og] = ObjCache(QPDF_Unresolved::create(...)) }` に 1:1 対応。resolve を起こさないという qpdf の契約も守られている。 |
| A4 | `QPDF::resolve(og)`（loop warning → xref type dispatch → catch して warn → null fallback） | `libqpdf/QPDF.cc:1699-1753` | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::resolve_indirect`（`DocumentResolver` の impl、`crates/flpdf/src/reader/resolver.rs:4437`） | prod: 5 (resolver.rs, xref.rs) / test: 5 | canonical | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::resolve_indirect` | `ResolveMark`（`crates/flpdf/src/reader/resolver.rs:622`）が qpdf の `ResolveRecorder`（`include/qpdf/QPDF.hh:980-996`）、`finish_indirect_resolution` が qpdf の catch → warn → null cache（`libqpdf/QPDF.cc:1737-1749`）に対応。呼び出し元が少ないのは qpdf と同じ構造（`QPDF::Resolver` friend が `QPDFObject` 1 つだけ、`include/qpdf/QPDF.hh:770-781`）で、通常は A5 経由でしか到達しない。 |
| A5 | `QPDFObjectHandle::dereference()` → `QPDFObject::resolve()` → `QPDF::Resolver::resolve` | `libqpdf/QPDFObjectHandle.cc:2375-2383`、`libqpdf/qpdf/QPDFObject_private.hh:155-167`、`libqpdf/QPDFObject.cc:6-11` | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_dereference`（`pub(crate)`、`crates/flpdf/src/object_handle.rs:2586`） | prod: 189 (25 files) / test: 55 | canonical | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_dereference` | 未初期化 handle に対する `Error::Internal("attempted to dereference an uninitialized QPDFObjectHandle")` まで qpdf の `std::logic_error`（`libqpdf/QPDFObjectHandle.cc:1586-1593`）と同文。ここが唯一の resolve 入口であること自体は守られている（A6 も A7 もここへ落ちる）。 |
| A6 | 型アクセサが暗黙に dereference する（`asInteger`/`asDictionary`/`isNull` 等が全て `dereference()` を通る） | `libqpdf/QPDFObjectHandle.cc:240-446` | 2 族が併存: **解決する** `try_*` 族（`crates/flpdf/src/object_handle.rs::ObjectHandle::try_as_integer` 等、`pub(crate)`/`pub`）と、**解決しない** `as_*`/`is_null` 族（`crates/flpdf/src/object_handle.rs::ObjectHandle::as_dictionary` 等、`pub`、`crates/flpdf/src/object_handle.rs:3821-3906`） | 非解決族 prod 合計 686: `as_dictionary` 190 / `is_null` 164 / `as_array` 129 / `as_integer` 74 / `as_name` 54 / `as_string` 53 / `as_real` 22。test 合計 1053。解決族 `try_as_integer` prod: 28 / test: 6 | mixed | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_as_integer`（`try_*` 族） | **本領域最大の二重経路**。qpdf は `asInteger()` も `isNull()` も必ず `dereference()` を通すので、未解決の間接 handle でも正しい型/値を返す。flpdf の `as_*`/`is_null` は doc 自身が "never performs resolution itself" と明記し、未解決の間接 handle に `None`/`false` を返す（`crates/flpdf/src/object_handle.rs:3860-3878`）。同じ handle に対して 2 族が **異なる答え**を返しうるのが mixed の実体。この差を埋めるために呼び出し側が A7 を前置している。 |
| A7 | `QPDF::resolve` を呼べるのは `QPDFObject` だけ（public な明示 resolve API は存在しない） | `include/qpdf/QPDF.hh:770-781`（`Resolver` の friend は `QPDFObject` のみ）、`include/qpdf/QPDF.hh:1031`（`resolve` は private） | `crates/flpdf/src/reader.rs::Pdf::resolve`（`pub`、実体は `handle.try_dereference()` 1 行、`crates/flpdf/src/reader.rs:1931-1937`） | prod: 211 (51 files) / test: 404 | bridge | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_dereference`（A5） | qpdf に **public 対応物が無い**明示 resolve 入口。存在理由は A6 の非解決アクセサ族で、呼び出し側は `pdf.resolve(&h)?;` → `h.as_dictionary()` という qpdf に無い 2 段イディオムを踏む。A6 を `try_*` 族へ寄せれば 211 箇所とも不要になる。**これは表現の翻訳ではなくタイミングの補正**で、qpdf ではアクセサ自身が担う「使う直前に解決する」を呼び出し側へ外出ししたものにすぎない。旧表現を翻訳する層ではないので、cutover でこの bridge に翻訳先を作ってはならない — 消す方向にしか畳めない。`Pdf::resolve_handle`（`crates/flpdf/src/reader.rs:1943`、prod 経路）・`Pdf::resolve_handle_ref`（`:1949`）・`Pdf::resolve_qpdf_json_handle`（`:1997`）も同じ bridge の薄いラッパー。 |
| A8 | `QPDFObjectHandle::getKey` / `hasKey`（型不一致は `typeWarning` + null/false、例外は投げない） | `libqpdf/QPDFObjectHandle.cc:979-1009`、`libqpdf/QPDFObjectHandle.cc:2168-2189` | `crates/flpdf/src/object_handle.rs::ObjectHandle::get_key`（`pub`、失敗時 **panic**）と `crates/flpdf/src/object_handle.rs::ObjectHandle::try_get_key`（`pub`、`Result`） | `get_key` prod: 143 / test: 435。`has_key` prod: 9 / test: 99。`try_get_key` prod: 351 / test: 229 | mixed | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_get_key` | A6 と違い両族とも resolve はする（`get_key` は `try_get_key` の `unwrap_or_else(panic!)`、`crates/flpdf/src/object_handle.rs:3922-3925`）。差は **失敗時の挙動**で、qpdf は warning に落として null を返すのに対し `get_key` は panic する（`.claude/rules/qpdf-port-design-patterns.md` 5「`throw` は panic ではない」に照らして panic 側が逸脱）。cutover は 143 箇所の `get_key`/9 箇所の `has_key` を `try_*` へ寄せる形になる。 |
| A9 | `QPDF::getAllObjects`（`fixDanglingReferences` → `obj_cache` 全走査、filter 無し） | `libqpdf/QPDF.cc:1285-1295`、`libqpdf/QPDF.cc:1256-1269`、`libqpdf/QPDF.cc:1239-1254` | `crates/flpdf/src/reader.rs::Pdf::get_all_objects`（`pub`） | prod: 8 (writer/rewrite_renumber.rs, document_json.rs ×3, reader.rs, および `crates/flpdf-qtest-tools` の driver/test_50_55.rs, renumber.rs, metadata.rs) / test: 7 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::all_object_handles` | 中核（`fix_dangling_references` + `all_object_handles`、`crates/flpdf/src/reader/resolver.rs:1332-1381`）は qpdf と 1:1 だが、facade 側が (a) `register_trailer_references()` による事前 seed（`crates/flpdf/src/reader.rs:1853-1868`）、(b) `qpdf_removed_refs` による除外（`crates/flpdf/src/pdf.rs:172-176`）、(c) `object_ref.number != 0 && generation != u16::MAX` の除外を足している。(b) は A13 の bridge 由来で qpdf に対応物が無い。 |
| A10 | 同じ `obj_cache` の列挙（qpdf は `getAllObjects` 1 本のみ） | `libqpdf/QPDF.cc:1285-1295` | `crates/flpdf/src/reader.rs::Pdf::object_refs`（`pub`）/ `crates/flpdf/src/reader.rs::Pdf::live_object_refs`（`pub`）/ `crates/flpdf/src/reader.rs::Pdf::resolved_count`（`pub`） | `live_object_refs` prod: 8 (writer/reachability.rs, writer/eligibility.rs, writer/rewrite_renumber.rs, writer/orchestrator.rs, job/page_merge.rs, linearization/plan.rs) / test: 32。`object_refs` prod: 6 / test: 41。`resolved_count` prod: 1 / test: 1 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::all_object_handles`（A9） | qpdf に対応物の無い 3 本目・4 本目の列挙経路。どちらも facade の `ObjectCache`（A2）を主軸に `canonical_object_refs`（`crates/flpdf/src/reader.rs:1259-1299`）で canonical 側とマージし、`qpdf_removed_refs` / `qpdf_parsed_xref_stream_refs` / `handle_mutated_object_refs`（いずれも qpdf 非対応、`crates/flpdf/src/pdf.rs:160`,`:171`,`:176`）で絞る。`Pdf::get_all_objects` と結果が一致する保証は無い。writer の到達性計算がここに依存しているため、cutover 順序上は A2 より後。 |
| A11 | `QPDF::getObjectCount` / `QPDF::nextObjGen` | `libqpdf/QPDF.cc:1271-1283`、`libqpdf/QPDF.cc:1872-1880` | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::get_object_count`（`pub(crate)`）/ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::next_obj_gen`（`pub(crate)`）と、facade 側の `crates/flpdf/src/reader.rs::Pdf::get_object_count`（`pub(crate)`）/ `crates/flpdf/src/reader.rs::Pdf::next_available_object_ref`（`pub(crate)`） | `rg -n 'get_object_count\(' crates` の宣言 2 行を除いた prod 4: facade 側 `Pdf::get_object_count` 2 (document_json.rs `:247`, writer.rs `:1607`)、canonical 側 `ResolverHandle::get_object_count` 2 (reader.rs `:1665`, resolver.rs `:1399` の `next_obj_gen` 内) / test: 9（全て resolver.rs）。`Pdf::next_available_object_ref` は宣言 (`crates/flpdf/src/reader.rs:1689`) を除く prod: 2 (reader.rs `:1666`, page_annotation_flatten.rs `:470`) / test: 2 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::next_obj_gen` | 採番の上限計算が 2 本ある。canonical 側は `max_object_number()`（canonical cache の最大 key、`crates/flpdf/src/reader/resolver.rs:1337-1344`）で qpdf の `obj_cache.rbegin()` に対応。facade 側の `next_available_object_ref` は `Pdf::object_refs()`（A10、legacy cache 混じり）と `resolver.max_object_number()` の **max** を取るため、legacy 側にしか無い ref が採番を押し上げうる。qpdf は `getObjectCount` 1 本で、`i32::MAX` 境界の `std::range_error` に対応する分岐は両方が持つ。 |
| A12 | `QPDF::makeIndirectObject` → `makeIndirectFromQPDFObject`（`nextObjGen` で採番し `obj_cache[next]` に **同じ `shared_ptr` を** 入れる） | `libqpdf/QPDF.cc:1890-1897`、`libqpdf/QPDF.cc:1882-1888` | `crates/flpdf/src/reader.rs::Pdf::make_indirect_object_handle`（`pub`）と `crates/flpdf/src/reader.rs::Pdf::make_indirect_from_object_handle`（`pub`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::make_indirect_from_object_handle`（`pub(crate)`） | `make_indirect_object_handle` prod: 33 (15 files、うち `crates/flpdf-qtest-tools` の driver 群 12) / test: 53 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::make_indirect_from_object_handle` | 2 本の採番経路。`Pdf::make_indirect_object_handle`（`crates/flpdf/src/reader.rs:1654-1681`）は resolver の primitive を **使わず**、`resolver.get_object_count()` → `next_available_object_ref()`（A11 の facade 側）→ `get_object_handle` → `set_resolved(direct_value_clone)` → `mark_object_dirty` と独自に組み立て、qpdf と違って **値を shallow copy** する（qpdf は同じ `shared_ptr` を登録するので alias が保たれる）。`Pdf::make_indirect_from_object_handle` の方は canonical へ委譲し alias を保つ。qpdf の `makeIndirectObject` に対応するのは前者の名前だが、挙動は後者の側が近い。 |
| A13 | `QPDF::removeObject`（**private**。xref から erase → cache 値を `QPDF_Null` に assign → og を切る → cache から erase） | `libqpdf/QPDF.cc:1995-2005`、`include/qpdf/QPDF.hh:1041` | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::remove_object`（`pub(crate)`）と `crates/flpdf/src/reader/resolver.rs::ResolverHandle::remove_object_preserving_handle`（`pub(crate)`） | `ResolverHandle::remove_object` の唯一の呼び出し元 `crates/flpdf/src/reader.rs::Pdf::remove_object_handle` は **`#[cfg(test)]`**（`crates/flpdf/src/reader.rs:1587-1588`）→ prod: 0 / test: 2。`remove_object_preserving_handle` prod: 1 (reader.rs `:1148`) / test: 0 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::remove_object` | qpdf の 1 メソッドが flpdf では 2 つに割れている。`remove_object` は qpdf の逐語訳だが production からは到達不能（テスト専用）で、production が通るのは `remove_object_preserving_handle`（handle identity を残す flpdf 独自の変種）。qpdf に対応物の無い後者が唯一の実経路であるという逆転が mixed の実体。 |
| A14 | public な「オブジェクト削除」は `replaceObject(og, newNull())`（`removeObject` は内部専用） | `include/qpdf/QPDF.hh:374-382`、`include/qpdf/QPDF.hh:384-386` | `crates/flpdf/src/reader.rs::Pdf::delete_object`（`pub`、戻り値なし） | prod: 2 (writer/reachability.rs `:63`, signatures.rs `:260`) / test: 8 | bridge | `crates/flpdf/src/reader.rs::Pdf::replace_object`（A16） | qpdf に対応する public API が無い削除入口。実体は `synchronize_cache_with_resolver_xref()`（A15）+ `qpdf_removed_refs` 追加 + `promote_resolved_object_stream_members` + `remove_object_preserving_handle`（A13）+ `set_resolved(Null)` + `cache.set_deleted` + `dirty_object_refs` 追加（`crates/flpdf/src/reader.rs:1125-1164`）で、**qpdf 非対応の副作用を 4 つ**足している。doc も「Keep the indirect identity for this legacy public API」と legacy であることを明記。残 prod caller は 2 箇所のみで、最も安く畳める行。 |
| A15 | （qpdf に対応物なし） | `probe: rg -ni 'synchroniz' $Q/libqpdf $Q/include → 1 hit（include/qpdf/QPDF.hh:695、pages tree の updateAllPagesCache の説明であり object cache とは無関係）` | `crates/flpdf/src/reader.rs::Pdf::synchronize_cache_with_resolver_xref`（`pub(crate)`）+ 一度きりフラグ `crates/flpdf/src/pdf.rs:147`（`legacy_resolution_state_synced`） | `synchronize_cache_with_resolver_xref` prod: 8 (reader.rs のみ。`:1130` delete_object, `:1530` replace_object, `:1554` swap_objects, `:1592` remove_object_handle ほか) / test: 1。`legacy_resolution_state_synced` prod: 5 (pdf.rs, reader.rs ×2, engine.rs ×2) / test: 0 | bridge | absent | A1/A2 の二重 cache が resolve 時 xref 再構築でずれるのを、次の legacy 読み出し直前に片方向で埋め戻すだけの層。qpdf は cache が 1 つなので同期処理自体が存在しない。`cache.rs::ObjectCache::synchronize_with_xref`（`crates/flpdf/src/cache.rs:56-87`）と `refs_after_xref_recovery`（`:94-135`）も同じ bridge の一部。A2 を畳めば全体が消える。 |
| A16 | `QPDF::replaceObject(og, oh)`（indirect/未初期化を `std::logic_error` で拒否 → `updateCache(og, obj, -1, -1)`） | `libqpdf/QPDF.cc:1985-1993`、`libqpdf/QPDF.cc:1842-1858` | `crates/flpdf/src/reader.rs::Pdf::replace_object`（`pub`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::replace_object`（`pub(crate)`） | prod: 17 (json/input.rs ×4, reader.rs ×2, writer.rs ×2, page_annotation_flatten.rs ×2, `crates/flpdf-qtest-tools` driver/test_10_17.rs ×2, embedded_files.rs, page_extract.rs, object_copy.rs, job/outline_dest_remap.rs, job/page_merge.rs) / test: 113 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::replace_object` | 中核の `updateCache` 相当は canonical へ委譲されているが、facade が前後に qpdf 非対応の 5 操作を挟む（`synchronize_cache_with_resolver_xref`、`qpdf_removed_refs`/`qpdf_parsed_xref_stream_refs`/`qpdf_dangling_refs` からの除去、`mark_object_handle_mutated`、`crates/flpdf/src/reader.rs:1515-1537`）。同じ「値の差し替え」が canonical cache と legacy 3 集合の両方に記録されるため、片方だけを見る consumer（A10）と結果が食い違いうる。 |
| A17 | `QPDF::swapObjects(og1, og2)`（**先に両方 resolve** → `swapWith` で value と og を交換） | `libqpdf/QPDF.cc:2284-2291`、`libqpdf/qpdf/QPDFObject_private.hh:121-130` | `crates/flpdf/src/reader.rs::Pdf::swap_objects`（`pub`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::swap_objects`（`pub(crate)`） | prod: 3 (reader.rs `:1555`、`crates/flpdf-qtest-tools` driver/test_10_17.rs `:295`,`:323`) / test: 7 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::swap_objects` | A16 と同型。加えて `CacheEntry::Deleted` / `CacheEntry::Missing` / `CacheEntry::Reserved` の tombstone を手で消す分岐（`crates/flpdf/src/reader.rs:1569-1575`）を持ち、コード内コメント自身が「qpdf has no persistent "deleted" or "missing" tombstone」と A2 の逸脱を明記している。 |
| A18 | （qpdf に対応物なし。qpdf の writer は `obj_cache` を走査するだけで dirty bit を持たない） | `probe: rg -ni 'dirty' $Q/libqpdf $Q/include → 0 hits`、`include/qpdf/QPDF.hh:1467`（obj_cache のみ） | `crates/flpdf/src/reader.rs::Pdf::mark_object_handle_dirty`（`pub`）/ `crates/flpdf/src/reader.rs::Pdf::mark_object_dirty`（`pub`）/ `crates/flpdf/src/reader.rs::Pdf::mark_object_handle_mutated`（`pub(crate)`）と、`crates/flpdf/src/pdf.rs:155` の `dirty_object_refs` と `crates/flpdf/src/pdf.rs:160` の `handle_mutated_object_refs` | `mark_object_handle_dirty` prod: 172 (47 files) / test: 63。`mark_object_dirty` prod: 6 (page_splice.rs, object_copy.rs, reader.rs) / test: 2。`mark_object_handle_mutated` prod: 9 (reader.rs, filespec_helper/embedded_file_stream.rs) / test: 0 | bridge | absent | qpdf は `QPDFObjectHandle` を変更すれば `obj_cache` の共有 `QPDFValue` がそのまま writer に見えるので、変更の記録という概念自体が無い。flpdf は `ObjectHandle` の live 変更を facade へ伝える手段が無いため 172 箇所で手動マークしている（`crates/flpdf/src/object_handle.rs:3975-3987` の `replace_key` doc が「This also has no path to inform the owning `Pdf`」と明記）。**本領域で最大の bridge caller 数**で、writer が canonical cache を直接走査する形（領域 D）にならないと畳めない。 |
| A19 | `QPDF::closeInputSource`（`m->file` を `InvalidInputSource` に差し替えるだけ。cache は触らない） | `libqpdf/QPDF.cc:277-281`、`include/qpdf/QPDF.hh:162-166`、`libqpdf/QPDF.cc:99-106` | `crates/flpdf/src/pdf.rs::Pdf::close_input_source`（`pub`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::close_input_source`（`pub(crate)`、`crates/flpdf/src/reader/resolver.rs:2062`） | `rg -n 'close_input_source\(' crates` の宣言 2 行を除いた prod 2: `Pdf::close_input_source` の外部呼び出しは `crates/flpdf-qtest-tools/src/driver/test_72_79.rs:276` の 1 件のみ、もう 1 件は `crates/flpdf/src/pdf.rs:227` の canonical への委譲 / test: 2 (reader.rs `:2280`, resolver.rs `:5641`) | canonical | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::close_input_source` | `Pdf::close_input_source` は resolver の差し替えに加えて `set_input_source_stay_open(false)` を呼ぶ（`crates/flpdf/src/pdf.rs:226-229`）。これは qpdf の `ClosedFileInputSource::stayOpen` を持つ file source が `m->file` 置換で最後の owner を失う挙動の再現で、doc に理由が明記されている（同 `:218-225`）。逸脱ではなく `shared_ptr` reset 相当の補完。 |
| A20 | `QPDF::~QPDF`（`xref_table.clear()` → `obj_cache` 全件 `disconnect()` → `ot_null` 以外は `destroy()`） | `libqpdf/QPDF.cc:215-236`、`libqpdf/QPDFObject.cc:13-17`、`libqpdf/qpdf/QPDFObject_private.hh:19-180` | `crates/flpdf/src/pdf.rs::Pdf::drop`（`impl Drop for Pdf`、`crates/flpdf/src/pdf.rs:191-209`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::disconnect_all`。**bootstrap 期には第 2 の walk** `crates/flpdf/src/xref.rs::BootstrapCache` の `Drop`（`crates/flpdf/src/xref.rs:85-126`） | `.disconnect()` prod: 2、`crates/flpdf/src/reader/resolver.rs:1690`（`disconnect_all` 内）と `crates/flpdf/src/xref.rs:122`（`BootstrapCache::drop` 内）/ test: 26（全て object_handle.rs） | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::disconnect_all` | `ObjectHandle::disconnect`（`crates/flpdf/src/object_handle.rs:2537-2560`）自体は qpdf の `disconnect()`+`destroy()` の組を `ot_null` を `destroy` しない条件込みで再現しており正確。mixed の理由は **walk が 2 本**あること: canonical cache を歩く `disconnect_all` と、`BootstrapCache` 自身の別 handle map (`crates/flpdf/src/xref.rs:66-73` の `BootstrapHandleState.handles`) を歩く `Drop`。後者のコメント自身が「runs before `Pdf` owns a `ResolverHandle` that could perform the normal qpdf-style disconnect walk」と、qpdf に無い bootstrap 段階のための複製であることを明記する。qpdf の parse は最初から `m->obj_cache` に積むので teardown も 1 本しかない。qpdf が先に行う `m->xref_table.clear()` に対応する行は flpdf に無い。`disconnect` は `slot.object_ref.is_none()` で **早期 return する**（`crates/flpdf/src/object_handle.rs:2540-2542`）ので `slot.resolver` を残す経路が形式上あるが、それが起きるのは (a) 直接 handle — `try_dereference` は `object_ref` が無い時点で `Ok(())` を返すので resolve 経路自体が無い、(b) 既に 1 回 disconnect 済みの handle — その 1 回目で `resolver` は `None` になっている、の 2 つだけで、いずれも「teardown 後に resolve が成功する」窓にはならない。 |
| A21 | 「間接参照である」ことは値の種類ではなく handle の og が担う（`QPDFValue` 派生に `Reference` は無い） | `include/qpdf/QPDFObjectHandle.hh:1629-1639`、`probe: ls $Q/libqpdf/qpdf/QPDF_*.hh → Array/Bool/Destroyed/Dictionary/InlineImage/Integer/Name/Null/Operator/Real/Reserved/Stream/String/Unresolved の 14 ファイルのみ、QPDF_Reference.hh は無い` | `crates/flpdf/src/object_handle.rs::ObjectValue`（`pub(crate)`、宣言は `crates/flpdf/src/object_handle.rs:1105`） | `ObjectValue::Reference` 参照は 3 件のみで、いずれも **不在を保証する guard か説明コメント**（`crates/flpdf/src/json_inspect.rs:19` のコメント、`crates/flpdf/tests/final_object_model_route_tests.rs:47,97`）→ prod: 0 / test: 2 | canonical | `crates/flpdf/src/object_handle.rs::ObjectValue` | **背景情報の訂正**: 依頼文と `docs/qpdf-correspondence.md` §1 は `ObjectValue::Reference` を「削除予定」として扱うが、**すでに削除済み**で `crates/flpdf/tests/final_object_model_route_tests.rs` が再導入を禁じている。同様に `resolve_to_terminal*` は `probe: rg -n 'resolve_to_terminal' crates --glob '*.rs' → 0 hits` で存在せず、`Pdf::resolve_handle` の doc も「the canonical value model has no reference-as-value variant」と明記する。`Unresolved`/`Reserved`/`Destroyed` の 3 sentinel も qpdf の同名 value 型に 1:1 対応。 |
| A22 | （qpdf に対応物なし。qpdf は `m->file` を live に読み、bounded owned-window helper を持たない） | `libqpdf/QPDF.cc:1360-1398`（`readStream` の save/restore seam）、`probe: rg -ni 'window' $Q/libqpdf $Q/include → 31 hits、全て "Windows"（OS 名）で input-source の読み窓は 0 件` | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::read_window`（`pub(crate)`、`#[deprecated(note = "no qpdf counterpart; …")]`、`crates/flpdf/src/reader/resolver.rs:2476-2483`） | prod: 5、すべて `crates/flpdf/src/reader.rs:960,966,989,1005,1011` / test: 0 | bridge | absent | 既に機械可読にマーク済み（CLAUDE.md 逸脱分類 (C)）。private helper `read_to_owned`（`crates/flpdf/src/reader/resolver.rs:2493-2497`）も同じ `#[deprecated]`。残 caller は `Pdf::source_stream_data_offset` 経由の `parse_source_file_object_at` と `qtest_read_source_object_with_retry` の 2 関数に閉じており、後者の consumer は `crates/flpdf-qtest-tools/src/driver/test_0_1.rs:260,267,282` のみ。doc 自身が「Do not build on its shape」と後継（`readObjectAtOffset`/`readStream` の save/restore 移植）を指定している。 |
| A23 | （qpdf に対応物なし。qpdf の dictionary key は常に `/` 付きの decoded name 1 表現） | `probe: rg -ni 'legacy' $Q/libqpdf $Q/include → 15 hits、いずれも page/content-stream helper と openssl provider で dictionary key 表現の話は 0 件` | `crates/flpdf/src/object_handle.rs::legacy_dictionary_key`（`pub(crate)`、`crates/flpdf/src/object_handle.rs:1280-1282`）と対の `crates/flpdf/src/object_handle.rs::canonical_dictionary_key` | `legacy_dictionary_key` prod: 8 (writer/object.rs `:10`,`:196`、parser.rs `:627`,`:734`、stream_filter.rs `:48`,`:586`,`:621`,`:623`) / test: 0 | bridge | `crates/flpdf/src/object_handle.rs::canonical_dictionary_key` | 「先頭 `/` を落とす」だけの表現変換で、doc 自身が「the legacy `Object`/`Dictionary` bridge deliberately omits that slash」と旧表現向けであることを明記。旧 `Object`/`Dictionary` 自体は PR #1360 で削除済みなので、残る 8 caller は writer の name escape・parser の warning 文字列・stream filter の key 判定で、いずれも `/` 付きのまま扱えるようにすれば消える。 |
| A24 | `QPDF::resolveObjectsInStream`（`resolved_object_streams` で二重展開防止、xref 再チェックで上書き済みメンバーを cache しない） | `libqpdf/QPDF.cc:1756-1833` | canonical 側は `crates/flpdf/src/reader/resolver.rs::ResolverCore` の `resolved_object_streams`（`crates/flpdf/src/reader/resolver.rs:324-327`）。facade 側に `crates/flpdf/src/reader.rs::Pdf::promote_resolved_object_stream_members`（private）と `crates/flpdf/src/pdf.rs:136` の `compressed_member_parents` | `promote_resolved_object_stream_members` prod: 2 (reader.rs `:1146`, `:1597`) / test: 1 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverCore` | canonical 側の ObjStm 展開は qpdf に対応するが、facade は展開済みメンバーを legacy `ObjectCache` へ「昇格」させる別経路（`crates/flpdf/src/reader.rs:1040-1099`）と、`compressed_member_parents` という provenance map を別に持つ。後者は A15 の同期処理で xref 再構築後に選別される（`crates/flpdf/src/reader.rs:2035-2043`）。qpdf の対応物は `ObjCache` の `end_before_space`/`end_after_space` の配布だけで、provenance map は無い。A2 と一緒にしか畳めない。 |

## unknown / probe

本領域は 24 行すべてを source と実行済み probe で分類できたため、`unknown` に落ちた行は無い。
以下は「分類は決まったが、cutover の設計にはさらに観測が要る」項目と、その probe。

1. **A2 / A10 — legacy `ObjectCache` と canonical cache の実際の乖離量**。両者が食い違う入力が
   どれだけあるかは source からは決まらない。必要 probe:
   `cargo test -p flpdf --features qpdf-zlib-compat` を通した状態で、
   `Pdf::get_all_objects()` と `Pdf::live_object_refs()` の結果差分を fixture 全体
   （`crates/flpdf/tests/fixtures/`）で取る一時ハーネスを書き、差分が出る fixture を列挙する。
   差分ゼロなら A2 は「無害な二重帳簿」として一括削除でき、差分があるならその fixture が
   cutover の RED test になる。
2. **A12 — `Pdf::make_indirect_object_handle` の shallow copy が qpdf と観測差を生むか**。
   qpdf は `makeIndirectFromQPDFObject` で **同じ `shared_ptr`** を登録するので、
   promote 後に元 handle の直接値を書き換えると新オブジェクト側にも見える
   （`libqpdf/QPDF.cc:1882-1888`）。flpdf の `direct_value_clone()`
   （`crates/flpdf/src/reader.rs:1655`）は shallow copy なので、配列/辞書の **コンテナ自体**は
   分離する。必要 probe: `qpdf` の C++ 側で `makeIndirectObject` 後に元 handle の
   `appendItem`/`replaceKey` を行い、新オブジェクトの出力に反映されるかを
   `/usr/bin/qpdf` 相当のテストドライバ（`qpdf/test_driver.cc`）で確認し、同じ手順を
   `crates/flpdf-qtest-tools` 側で走らせて出力バイトを比較する。
3. **A13 — `remove_object_preserving_handle` が qpdf 出力に与える差**。qpdf の `removeObject` は
   cache cell ごと erase して og も切る（`libqpdf/QPDF.cc:1995-2005`）が、flpdf は handle identity を
   残す。qpdf 側の `removeObject` は private でどの public 経路からも直接は呼ばれないため、
   観測可能な差が出るのは JSON 出力と writer の到達性計算だけのはず。必要 probe:
   `qpdf --json=2` と flpdf の同等出力を、`Pdf::delete_object` 相当の操作
   （qpdf 側は `replaceObject(og, newNull())`）を挟んだ前後で比較する。
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

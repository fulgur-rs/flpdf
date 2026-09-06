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
`CacheEntry::Deleted` / `Missing` に qpdf 対応物が無いという判定の根拠がこれ。

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
| A1 | `QPDF::Members::obj_cache`（object identity の唯一の正本） | `include/qpdf/QPDF.hh:1467`、`include/qpdf/QPDF.hh:868-889` | `crates/flpdf/src/reader/resolver.rs::ResolverCore` の `object_cache` フィールド（`crates/flpdf/src/reader/resolver.rs:287-306`、private） | `rg -n --glob '*.rs' 'object_cache' crates` → 17 行（コメント 4 行を規約 (a) で除いて 13 件）、うち宣言 `crates/flpdf/src/reader/resolver.rs:306` を除いて **prod: 12 (reader/resolver.rs のみ) / test: 0**（書き込みは `:887,932` の初期化・`:983,1181,1441` の `insert`・`:1533` の `remove`、読み出しは `:1148,1314,1333,1340,1556,1689`。全件が `ResolverHandle` のメソッド内で、このフィールドに外から触れる経路は存在しない） | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverCore` | qpdf の 1 つの `m->obj_cache` に対し flpdf は **3 つの map** を持つ: (1) canonical 側の `object_cache: BTreeMap<ObjectRef, ObjectHandle>`、(2) facade 側の `crates/flpdf/src/cache.rs::ObjectCache`（A2）、(3) xref bootstrap 期だけ生きる `crates/flpdf/src/xref.rs::BootstrapHandleState` の `handles`（`crates/flpdf/src/xref.rs:66-73`）。(3) は `resolving` / `resolved_object_streams` まで自前で持つ resolve state の完全な複製で、`Drop` に独自の disconnect walk がある（A20）。(1) は resolve/allocate の全経路が通るが、(2) は open 時の xref (`crates/flpdf/src/engine.rs:209` の `ObjectCache::from_offsets`) から作られ、以後 `crates/flpdf/src/reader.rs` の `self.cache.` 12 箇所のうち **書き込みは 4 箇所だけ**（`:1097` `set_resolved`、`:1162` `set_deleted`、`:1574` `set_resolved`、`:2027` `synchronize_with_xref`）で、残る 8 箇所（`:1048,1066,1157,1169,1173,1265,1570,1843`）は `entry`/`resolved_count`/`deleted_refs` の **読み出し**。通常の resolve 経路はこの 4 つのどれも通らないので (2) は **更新されない**。(1) と (2) の乖離を後追いで埋めるのが A15。 |
| A2 | `QPDF::ObjCache`（`{object, end_before_space, end_after_space}` の 3 フィールド、private nested class） | `include/qpdf/QPDF.hh:868-889` | `crates/flpdf/src/cache.rs::ObjectCache` + `crates/flpdf/src/cache.rs::CacheEntry`（どちらも `pub`、`crates/flpdf/src/lib.rs:92` の `pub mod cache` と `:157` の `pub use` で crate 外へ公開） | `CacheEntry` prod: 47 (reader.rs, cache.rs, lib.rs) / test: 11。`ObjectCache` 型名 prod: 8 (pdf.rs, cache.rs, lib.rs, engine.rs) / test: 6 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverCore`（A1） | qpdf の `ObjCache` は「値 + 2 つの offset」だけで、**未解決/予約/削除は値の型**（`QPDF_Unresolved` / `QPDF_Reserved` / `QPDF_Null`）が表す。flpdf の `CacheEntry` は `Unresolved{offset}` / `Compressed{stream,index}` / `Resolved(handle)` / `Missing` / `Reserved` / `Deleted` の 6 状態を **cache 側**に持つ二重表現で、`Missing` と `Deleted` に至っては qpdf に対応する状態が無い（`removeObject` は cache cell ごと erase する、`libqpdf/QPDF.cc:1995-2005`）。qpdf の `end_before_space`/`end_after_space` に対応するのは cache ではなく handle 側の `ObjectSlot` フィールド（`crates/flpdf/src/object_handle.rs:1019-1020`）。可視性も qpdf の private nested class に対し `pub`（`.claude/rules/qpdf-port-design-patterns.md` 8 の根拠 1〜3 いずれにも該当しない）。 |
| A3 | `QPDF::getObject(QPDFObjGen)`（**resolve しない** handle 取得） | `libqpdf/QPDF.cc:1951-1959`、`include/qpdf/QPDF.hh:362-372` | `crates/flpdf/src/reader.rs::Pdf::get_object_handle`（`pub`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::get_object_handle`（`pub(crate)`） | `rg -n '\.get_object_handle\(' crates`（facade 経由と resolver 直呼びの合算）prod: 257 (66 files; うち `crates/flpdf-qtest-tools` の driver 群 `test_10_17.rs` 他) / test: 533 | canonical | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::get_object_handle` | `Pdf::get_object_handle` は 1 行の委譲（`crates/flpdf/src/reader.rs:1313-1321`）で、`or_insert_with` が qpdf の `if (!isCached(og)) { obj_cache[og] = ObjCache(QPDF_Unresolved::create(...)) }` に 1:1 対応。resolve を起こさないという qpdf の契約も守られている。 |
| A4 | `QPDF::resolve(og)`（loop warning → xref type dispatch → catch して warn → null fallback） | `libqpdf/QPDF.cc:1699-1753` | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::resolve_indirect`（`DocumentResolver` の impl、`crates/flpdf/src/reader/resolver.rs:4437`） | `rg -n --glob '*.rs' '[.:]resolve_indirect\(' crates` → prod: **1**、`crates/flpdf/src/object_handle.rs:2610`（`try_dereference` 内、trait 越しの唯一の呼び出し）/ test: 4 (parser.rs `:1101`, reader/resolver.rs `:6316`, object_handle.rs `:8642`, xref.rs `:4916`) | canonical | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::resolve_indirect` | `ResolveMark`（`crates/flpdf/src/reader/resolver.rs:622`）が qpdf の `ResolveRecorder`（`include/qpdf/QPDF.hh:980-996`）、`finish_indirect_resolution` が qpdf の catch → warn → null cache（`libqpdf/QPDF.cc:1737-1749`）に対応。呼び出し元が少ないのは qpdf と同じ構造（`QPDF::Resolver` friend が `QPDFObject` 1 つだけ、`include/qpdf/QPDF.hh:770-781`）で、通常は A5 経由でしか到達しない。 |
| A5 | `QPDFObjectHandle::dereference()` → `QPDFObject::resolve()` → `QPDF::Resolver::resolve` | `libqpdf/QPDFObjectHandle.cc:2375-2383`、`libqpdf/qpdf/QPDFObject_private.hh:155-167`、`libqpdf/QPDFObject.cc:6-11` | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_dereference`（`pub(crate)`、`crates/flpdf/src/object_handle.rs:2586`） | prod: 189 (27 files) / test: 55 | canonical | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_dereference` | 未初期化 handle に対する `Error::Internal("attempted to dereference an uninitialized QPDFObjectHandle")` まで qpdf の `std::logic_error`（`libqpdf/QPDFObjectHandle.cc:1586-1593`）と同文。ここが唯一の resolve 入口であること自体は守られている（A6 も A7 もここへ落ちる）。 |
| A6 | 型アクセサが暗黙に dereference する（`asInteger`/`asDictionary`/`isNull` 等が全て `dereference()` を通る） | `libqpdf/QPDFObjectHandle.cc:240-446` | 2 族が併存: **解決する** `try_*` 族（`crates/flpdf/src/object_handle.rs::ObjectHandle::try_as_integer` 等、`pub(crate)`/`pub`）と、**解決しない** `as_*`/`is_null` 族（`crates/flpdf/src/object_handle.rs::ObjectHandle::as_dictionary` 等、`pub`、`crates/flpdf/src/object_handle.rs:3821-3906`） | 非解決族 prod 合計 686: `as_dictionary` 190 / `is_null` 164 / `as_array` 129 / `as_integer` 74 / `as_name` 54 / `as_string` 53 / `as_real` 22。test 合計 1053。解決族 `try_as_integer` prod: 28 / test: 6 | mixed | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_as_integer`（`try_*` 族） | **本領域最大の二重経路**。qpdf は `asInteger()` も `isNull()` も必ず `dereference()` を通すので、未解決の間接 handle でも正しい型/値を返す。flpdf の `as_*`/`is_null` は doc 自身が "never performs resolution itself" と明記し、未解決の間接 handle に `None`/`false` を返す（`crates/flpdf/src/object_handle.rs:3860-3878`）。同じ handle に対して 2 族が **異なる答え**を返しうるのが mixed の実体。この差を埋めるために呼び出し側が A7 を前置している。 |
| A7 | `QPDF::resolve` を呼べるのは `QPDFObject` だけ（public な明示 resolve API は存在しない） | `include/qpdf/QPDF.hh:770-781`（`Resolver` の friend は `QPDFObject` のみ）、`include/qpdf/QPDF.hh:1031`（`resolve` は private） | `crates/flpdf/src/reader.rs::Pdf::resolve`（`pub`、実体は `handle.try_dereference()` 1 行、`crates/flpdf/src/reader.rs:1931-1937`） | `rg -n --glob '*.rs' '\.resolve\(' crates` → prod 263 / test 426。うち `Pdf::resolve` でないものは 23 件（prod 7: `PageRange::resolve` の `job/lifecycle.rs:2734`・`job/overlay.rs:421,422,424`・`job/page_plan.rs:103`・`flpdf-cli/src/main.rs:5865`、および `json/handler.rs:191` の `handler.resolve()`。test 16: `PageRange::resolve` の `job/rotate_spec.rs:223,233,255,264,393`・`job/page_range.rs:557,565,582,686`・`flpdf-cli/src/main.rs:9149,9152,9156,9177,9180,9184,9216`）→ **prod: 256 (56 files) / test: 410**。ファイル別 prod 上位: page_object_helper.rs 21 / page_splice.rs 16 / job/json_sections.rs 15 / flpdf-qtest-tools driver/test_42_49.rs 13 / job/acroform_field_prune.rs 12 / flpdf-qtest-tools driver/handle.rs 12 / flpdf-qtest-tools compare.rs 11 / page_annotation_flatten.rs 10 / page_form_xobject.rs 9 / annotation_object_helper.rs 9（残り 46 ファイル） | bridge | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_dereference`（A5） | **README §3 bridge の形 (ii)**（qpdf に対応する処理が無い flpdf 固有の補助経路 = CLAUDE.md 逸脱分類 (C) の「明示的 `Pdf::resolve` による解決タイミング補正」そのもの）。qpdf には public な明示 resolve 入口が存在せず、アクセサ自身が使う直前に解決する。flpdf では A6 の非解決アクセサ族があるため、呼び出し側が `pdf.resolve(&h)?;` → `h.as_dictionary()` という qpdf に無い 2 段イディオムを踏む。**削除対象**であり、A6 を `try_*` 族へ寄せれば 256 箇所とも不要になる。`Pdf::resolve_handle`（`crates/flpdf/src/reader.rs:1943`）・`Pdf::resolve_handle_ref`（`:1949`）も同じ bridge の薄いラッパー。**数え方の注記**: `\.resolve\(&` 単独は prod 211 / test 404 で、`target.resolve(&page)` は拾うが `pdf.resolve(handle)`（`&` なしの既参照変数）を落とす。`\bpdf\.resolve\(` 単独は prod 235 / test 376 で、逆に `&` なし形は拾うが `target`/`source`/`oldpdf`/`qpdf`/`actual_pdf` のような `pdf` で始まらない receiver を落とす。両者の和でもまだ `flpdf-qtest-tools/src/compare.rs:26,28` と `crates/flpdf/src/reader.rs:1944,1954` を取りこぼすため、行の数字は上記の「`\.resolve\(` 全件から非 `Pdf` receiver 23 件を引く」方式を採る。 |
| A8 | `QPDFObjectHandle::getKey` / `hasKey`（typeWarning と null/false fallback、例外伝播） | `libqpdf/QPDFObjectHandle.cc:965-989,2168-2189` | `crates/flpdf/src/object_handle.rs::ObjectHandle::get_key` / `has_key` と `try_get_key` / `try_has_key` | 残 caller は `rg -n '\.(get_key\|has_key)\(' crates --glob '*.rs'` で receiver を確認する。2026-09-06 は実装・例外境界を再確認し、全件数は再集計していない | mixed | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_get_key` | 両族とも resolve する。`crates/flpdf/src/object_handle.rs:3921-3938` の convenience 版は `try_*` の `Err` を panic にする。**通常の document-owned 型不一致は warning + null/false で成功し、それ自体が panic になるわけではない**。問題は lazy resolution / warning 配送失敗や contextless warning の例外を panic に変換すること。qpdf の `typeWarning` も dereference や contextless `warn` が throw しうるので「例外を投げない」という旧記述は訂正する。consumer ごとに fallible accessor へ移行する。 |
| A9 | `QPDF::getAllObjects`（`fixDanglingReferences` → `obj_cache` 全走査） | `libqpdf/QPDF.cc:1239-1269,1285-1295` | `crates/flpdf/src/reader.rs::Pdf::get_all_objects`（`pub`） | prod callers は writer/rewrite_renumber.rs、document_json.rs、reader.rs、qtest-tools の driver / renumber / metadata。全件数は今回未再集計 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::get_all_objects` | `.48.20`で履歴trailer参照の未解決登録をopen境界へ移し、Pdfに保持していた参照集合とgetAllObjectsのlate mint/eager resolveを撤去した。canonical列挙は各cache keyへnewIndirectを適用する。facadeのnumber 0 / generation 65535 filterとbootstrap cache自体の責務移行は後続のためmixedを継続する。 |
| A10 | 同じ `obj_cache` の列挙（qpdf は `getAllObjects` 1 本のみ） | `libqpdf/QPDF.cc:1285-1295` | `crates/flpdf/src/reader.rs::Pdf::object_refs`（`pub`）/ `crates/flpdf/src/reader.rs::Pdf::live_object_refs`（`pub`）/ `crates/flpdf/src/reader.rs::Pdf::resolved_count`（`pub`） | `live_object_refs` prod: 7 (writer/eligibility.rs, writer/rewrite_renumber.rs, writer/orchestrator.rs, job/page_merge.rs, linearization/plan.rs) / test: 24。`object_refs` prod: 6 / test: 41。`resolved_count` prod: 1 / test: 1 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::all_object_handles`（A9） | qpdf に対応物の無い 3 本目・4 本目の列挙経路。どちらも facade の `ObjectCache`（A2）を主軸に `crates/flpdf/src/reader.rs::canonical_object_refs`で canonical 側とマージし、`qpdf_parsed_xref_stream_refs` / `handle_mutated_object_refs`（`crates/flpdf/src/pdf.rs:206,217`）で絞る。空だった removed-ref filterは `.48.21` で撤去した。`Pdf::get_all_objects` と結果が一致する保証は無い。writer の到達性計算がここに依存しているため、cutover 順序上は A2 より後。 |
| A11 | `QPDF::getObjectCount` / `QPDF::nextObjGen` | `libqpdf/QPDF.cc:1271-1283,1872-1880` | `ResolverHandle::get_object_count` / `next_obj_gen` と `Pdf::get_object_count` / `next_available_object_ref` | `rg -n -e 'get_object_count\(' -e 'next_obj_gen\(' -e 'next_available_object_ref\(' crates --glob '*.rs'` で facade と resolver を識別する。全件数は今回未再集計 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::next_obj_gen` | `.48.20`で`Pdf::get_object_count`はcanonical cache-key最大値に委譲し、列挙によるidentity更新を除いた。public makeIndirect factoryも`next_obj_gen`へ統一済み。`next_available_object_ref`は残るfacade consumer用で、legacy混じり`object_refs()`を使うためA11全体はmixedを継続する。 |
| A12 | `QPDF::makeIndirectObject` → `makeIndirectFromQPDFObject`（`nextObjGen` で採番し `obj_cache[next]` に **同じ `shared_ptr` を** 入れる） | `libqpdf/QPDF.cc:1890-1897`、`libqpdf/QPDF.cc:1882-1888` | `crates/flpdf/src/reader.rs::Pdf::make_indirect_object_handle`（`pub`）と `crates/flpdf/src/reader.rs::Pdf::make_indirect_from_object_handle`（`pub`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::make_indirect_from_object_handle`（`pub(crate)`） | public make_indirect_object_handle: prod 31 (14 files) / test 89。両public factoryは同じresolver ownerへ委譲 | canonical | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::make_indirect_from_object_handle` | `.48.20`でpublic clone allocatorを撤去。initialized guard → canonical nextObjGen → 同じQObjectをcache登録 → shared ValueIdentityを更新する。indirect/reserved/destroyedと別document入力も受け付け、解決・tree claim・子再帰を行わない。cache lookup/列挙は要求keyにidentityを戻す。`make_indirect_object_owner_tests.rs`とC++ oracle probesでalias、共有replacement、採番上限、履歴trailer参照の採番、writer出力のgolden byte一致を検証。 |
| A13 | `QPDF::removeObject`（private。xref erase → cache値をnullにassign → og解除 → cache erase） | `libqpdf/QPDF.cc:1995-2005`, `include/qpdf/QPDF.hh:1041` | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::remove_object` | 唯一の facade caller `crates/flpdf/src/reader.rs::Pdf::remove_object_handle` は `#[cfg(test)]` → prod: 0 / test: 1 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::remove_object` | canonical primitive自体はqpdfのprivate責務を持ち、test-onlyであることは削除理由にならない。`.48.21` ではこのprimitiveとtestを保持した。残るmixed部分はtest facadeが前後に加えるcache同期とmutation bookkeeping。`.46` でhandle-retaining variantは撤去済みで、**現在のcanonical removalがog/identityを保持するという旧probe前提は誤り**。A2/A15の後処理とともにfacadeの余分な同期を畳み、primitiveとその意味を検証するtestは保持する。 |
| A14 | public な「オブジェクト削除」は `replaceObject(og, newNull())`（`removeObject` は内部専用） | `include/qpdf/QPDF.hh:374-382`、`include/qpdf/QPDF.hh:384-386` | A16 の `crates/flpdf/src/reader.rs::Pdf::replace_object`（`pub`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::replace_object` | `delete_object` の production/test caller は 0（`scripts/qpdf-route-callers.py --symbol delete_object --expect-zero`） | canonical | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::replace_object` | `.46` で flpdf 固有の `Pdf::delete_object` とその consumer/test callers を撤去した。signature value stripping は qpdf と同じく `/V` を外すだけで signature dictionary を eager delete せず、明示的な null replacement が必要な test は public `replace_object(og, ObjectHandle::null())` を使う。 |
| A15 | （qpdf に対応物なし） | `probe: rg -ni 'synchroniz' $Q/libqpdf $Q/include → 1 hit（include/qpdf/QPDF.hh:695、pages tree の updateAllPagesCache の説明であり object cache とは無関係）` | `crates/flpdf/src/reader.rs::Pdf::synchronize_cache_with_resolver_xref`（`pub(crate)`）+ 一度きりフラグ `crates/flpdf/src/pdf.rs:147`（`legacy_resolution_state_synced`） | `synchronize_cache_with_resolver_xref` prod: 6 (reader.rs のみ) / test: 2。`legacy_resolution_state_synced` prod: 5 (pdf.rs, reader.rs ×2, engine.rs ×2) / test: 0 | bridge | absent | A1/A2 の二重 cache が resolve 時 xref 再構築でずれるのを、次の legacy 読み出し直前に片方向で埋め戻すだけの層。qpdf は cache が 1 つなので同期処理自体が存在しない。`cache.rs::ObjectCache::synchronize_with_xref`（`crates/flpdf/src/cache.rs:56-87`）と `refs_after_xref_recovery`（`:94-135`）も同じ bridge の一部。A2 を畳めば全体が消える。 |
| A16 | `QPDF::replaceObject(og, oh)`（indirect/未初期化を `std::logic_error` で拒否 → `updateCache(og, obj, -1, -1)`） | `libqpdf/QPDF.cc:1985-1993`、`libqpdf/QPDF.cc:1842-1858` | `crates/flpdf/src/reader.rs::Pdf::replace_object`（`pub`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::replace_object`（`pub(crate)`） | prod: 17 (json/input.rs ×4, reader.rs ×2, writer.rs ×2, page_annotation_flatten.rs ×2, `crates/flpdf-qtest-tools` driver/test_10_17.rs ×2, embedded_files.rs, page_extract.rs, object_copy.rs, job/outline_dest_remap.rs, job/page_merge.rs) / test: 113 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::replace_object` | 中核の `updateCache` 相当は canonical へ委譲されているが、facade が前後に qpdf 非対応の 4 操作を挟む（`synchronize_cache_with_resolver_xref`、`qpdf_parsed_xref_stream_refs`/`qpdf_dangling_refs` からの除去、`mark_object_handle_mutated`、`crates/flpdf/src/reader.rs::Pdf::replace_object`）。空集合からの無効なremoveは `.48.21` で撤去した。同じ「値の差し替え」が canonical cache と legacy 2 集合の両方に記録されるため、片方だけを見る consumer（A10）と結果が食い違いうる。 |
| A17 | `QPDF::swapObjects(og1, og2)`（**先に両方 resolve** → `swapWith` で value と og を交換） | `libqpdf/QPDF.cc:2284-2291`、`libqpdf/qpdf/QPDFObject_private.hh:121-130` | `crates/flpdf/src/reader.rs::Pdf::swap_objects`（`pub`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::swap_objects`（`pub(crate)`） | prod: 3 (reader.rs `:1555`、`crates/flpdf-qtest-tools` driver/test_10_17.rs `:295`,`:323`) / test: 7 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::swap_objects` | A16 と同型。空のremoved-ref集合へのremoveは `.48.21` で撤去した。加えて `CacheEntry::Deleted` / `CacheEntry::Missing` / `CacheEntry::Reserved` の tombstone を手で消す分岐（`crates/flpdf/src/reader.rs:1432-1450`）を持ち、コード内コメント自身が「qpdf has no persistent "deleted" or "missing" tombstone」と A2 の逸脱を明記している。 |
| A18 | （qpdf に対応物なし。qpdf の writer は `obj_cache` を走査するだけで dirty bit を持たない） | `probe: rg -ni 'dirty' $Q/libqpdf $Q/include → 0 hits`、`include/qpdf/QPDF.hh:1467`（obj_cache のみ） | `crates/flpdf/src/reader.rs::Pdf::mark_object_handle_dirty`（`pub`）/ `crates/flpdf/src/reader.rs::Pdf::mark_object_dirty`（`pub`）/ `crates/flpdf/src/reader.rs::Pdf::mark_object_handle_mutated`（`pub(crate)`）と、`crates/flpdf/src/pdf.rs:155` の `dirty_object_refs` と `crates/flpdf/src/pdf.rs:160` の `handle_mutated_object_refs` | `rg -n --glob '*.rs' '\.mark_object_handle_dirty\(' crates` → prod: 172 (48 files) / test: 63。ファイル別 prod 上位: acroform_document_helper.rs 29 / page_object_helper.rs 16 / page_annotation_flatten.rs 14 / page_splice.rs 7 / resources.rs 7 / job/page_merge.rs 7 / form_field_object_helper/rendering.rs 6 / pages/repair.rs 5 / pages/tree_rebuild.rs 5 / filespec_helper/embedded_file_stream.rs 5（残り 38 ファイル）。`mark_object_dirty` prod: 6 (page_splice.rs, object_copy.rs, reader.rs) / test: 2。`mark_object_handle_mutated` prod: 9 (reader.rs, filespec_helper/embedded_file_stream.rs) / test: 0 | bridge | absent | **README §3 bridge の形 (ii)**（qpdf に対応する処理が無い flpdf 固有の補助経路 = CLAUDE.md 逸脱分類 (C) の「dirty 追跡」そのもの）。qpdf は `QPDFObjectHandle` を変更すれば `obj_cache` の共有 `QPDFValue` がそのまま writer に見えるので、変更の記録という概念自体が無い。flpdf は `ObjectHandle` の live 変更を facade へ伝える手段が無いため 172 箇所で手動マークしている（`crates/flpdf/src/object_handle.rs:3975-3987` の `replace_key` doc が「This also has no path to inform the owning `Pdf`」と明記）。**本領域で最大の bridge caller 数**。**削除対象**だが、writer が canonical cache を直接走査する形（領域 D）になるまでは畳めない。 |
| A19 | `QPDF::closeInputSource`（`m->file` を `InvalidInputSource` に差し替えるだけ。cache は触らない） | `libqpdf/QPDF.cc:277-281`、`include/qpdf/QPDF.hh:162-166`、`libqpdf/QPDF.cc:99-106` | `crates/flpdf/src/pdf.rs::Pdf::close_input_source`（`pub`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::close_input_source`（`pub(crate)`、`crates/flpdf/src/reader/resolver.rs:2062`） | `rg -n 'close_input_source\(' crates` の宣言 2 行を除いた prod 2: `Pdf::close_input_source` の外部呼び出しは `crates/flpdf-qtest-tools/src/driver/test_72_79.rs:276` の 1 件のみ、もう 1 件は `crates/flpdf/src/pdf.rs:227` の canonical への委譲 / test: 2 (reader.rs `:2280`, resolver.rs `:5641`) | canonical | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::close_input_source` | `Pdf::close_input_source` は resolver の差し替えに加えて `set_input_source_stay_open(false)` を呼ぶ（`crates/flpdf/src/pdf.rs:226-229`）。これは qpdf の `ClosedFileInputSource::stayOpen` を持つ file source が `m->file` 置換で最後の owner を失う挙動の再現で、doc に理由が明記されている（同 `:218-225`）。逸脱ではなく `shared_ptr` reset 相当の補完。 |
| A20 | `QPDF::~QPDF`（`xref_table.clear()` → `obj_cache` 全件 `disconnect()` → `ot_null` 以外は `destroy()`） | `libqpdf/QPDF.cc:215-236`、`libqpdf/QPDFObject.cc:13-17`、`libqpdf/qpdf/QPDFObject_private.hh:19-180` | `crates/flpdf/src/pdf.rs::Pdf::drop`（`impl Drop for Pdf`、`crates/flpdf/src/pdf.rs:191-209`）→ `crates/flpdf/src/reader/resolver.rs::ResolverHandle::disconnect_all`。**bootstrap 期には第 2 の walk** `crates/flpdf/src/xref.rs::BootstrapCache` の `Drop`（`crates/flpdf/src/xref.rs:85-126`） | `.disconnect()` prod: 2、`crates/flpdf/src/reader/resolver.rs:1690`（`disconnect_all` 内）と `crates/flpdf/src/xref.rs:122`（`BootstrapCache::drop` 内）/ test: 26（全て object_handle.rs） | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::disconnect_all` | `ObjectHandle::disconnect`（`crates/flpdf/src/object_handle.rs:2537-2560`）自体は qpdf の `disconnect()`+`destroy()` の組を `ot_null` を `destroy` しない条件込みで再現しており正確。mixed の理由は **walk が 2 本**あること: canonical cache を歩く `disconnect_all` と、`BootstrapCache` 自身の別 handle map (`crates/flpdf/src/xref.rs:66-73` の `BootstrapHandleState.handles`) を歩く `Drop`。後者のコメント自身が「runs before `Pdf` owns a `ResolverHandle` that could perform the normal qpdf-style disconnect walk」と、qpdf に無い bootstrap 段階のための複製であることを明記する。qpdf の parse は最初から `m->obj_cache` に積むので teardown も 1 本しかない。qpdf が先に行う `m->xref_table.clear()` に対応する行は flpdf に無い。`disconnect` は `slot.object_ref.is_none()` で **早期 return する**（`crates/flpdf/src/object_handle.rs:2540-2542`）ので `slot.resolver` を残す経路が形式上あるが、それが起きるのは (a) 直接 handle — `try_dereference` は `object_ref` が無い時点で `Ok(())` を返すので resolve 経路自体が無い、(b) 既に 1 回 disconnect 済みの handle — その 1 回目で `resolver` は `None` になっている、の 2 つだけで、いずれも「teardown 後に resolve が成功する」窓にはならない。 |
| A21 | 「間接参照である」ことは値の種類ではなく handle の og が担う（`QPDFValue` 派生に `Reference` は無い） | `include/qpdf/QPDFObjectHandle.hh:1629-1639`、`probe: ls $Q/libqpdf/qpdf/QPDF_*.hh → Array/Bool/Destroyed/Dictionary/InlineImage/Integer/Name/Null/Operator/Real/Reserved/Stream/String/Unresolved の 14 ファイルのみ、QPDF_Reference.hh は無い` | `crates/flpdf/src/object_handle.rs::ObjectValue`（`pub(crate)`、宣言は `crates/flpdf/src/object_handle.rs:1105`） | `ObjectValue::Reference` 参照は 3 件のみで、いずれも **不在を保証する guard か説明コメント**（`crates/flpdf/src/json_inspect.rs:19` のコメント、`crates/flpdf/tests/final_object_model_route_tests.rs:47,97`）→ prod: 0 / test: 2 | canonical | `crates/flpdf/src/object_handle.rs::ObjectValue` | **背景情報の訂正**: 依頼文と `docs/qpdf-correspondence.md` §1 は `ObjectValue::Reference` を「削除予定」として扱うが、**すでに削除済み**で `crates/flpdf/tests/final_object_model_route_tests.rs` が再導入を禁じている。同様に `resolve_to_terminal*` は `probe: rg -n 'resolve_to_terminal' crates --glob '*.rs' → 0 hits` で存在せず、`Pdf::resolve_handle` の doc も「the canonical value model has no reference-as-value variant」と明記する。`Unresolved`/`Reserved`/`Destroyed` の 3 sentinel も qpdf の同名 value 型に 1:1 対応。**この行は経路ではなく値型の行**である — entrypoint 欄が指すのは呼び出し口ではなく `ObjectValue` という型そのもので、`ObjectValue::Reference` と `resolve_to_terminal*` が実際に消えていることを確認するために記録している。したがって `canonical` の定義（唯一の正本であり cite した qpdf code と 1:1）は **型レベルで**適用される: flpdf の値ファミリが qpdf の `QPDFValue` 派生 14 種と 1:1 で、参照を表す第 15 の variant を持たない、という意味。 |
| A22 | （qpdf に対応物なし。qpdf は live input source を読む） | `libqpdf/QPDF.cc:1360-1398,1541-1697` | `crates/flpdf/src/reader/resolver.rs::ResolverHandle::read_window`（`pub(crate)`、deprecated） | prod: 5、全て `crates/flpdf/src/reader.rs:988,994,1017,1033,1039` / test: 0 | bridge | absent | `read_to_owned` も同じdeprecated bridge。残callerは `qtest_read_source_object_with_retry` と `parse_source_file_object_at`。前者はplural qtest metadata APIs経由で `crates/flpdf-qtest-tools/src/driver/test_0_1.rs:260,267,282`、後者は `source_stream_data_offset` 経由で同 `:343,389` に達する。E27/C28のtest_0 canonical pipe/logger consumer移行で両群を扱い、その後B34のfallback予算とともにcaller-zero削除する。削除済み単数metadata wrapperとは別の残経路。 |
| A23 | （qpdf に対応物なし。dictionary key は `/` 付き decoded name） | `libqpdf/QPDFParser.cc:464`, `libqpdf/QPDF_Name.cc:27-49`、stream-filter ownerは`libqpdf/SF_FlateLzwDecode.cc:22-73` / `libqpdf/QPDF_Stream.cc:33-50` | `crates/flpdf/src/object_handle.rs::legacy_dictionary_key` と `canonical_dictionary_key` | 実呼出 prod: 3 — parser.rs:629,736、writer/object.rs:196。stream_filter.rsのkey判定3 callerは`.48.36`でcanonical keyを直接保持・比較へ移行。別にimport 2行 / test: 0 | bridge | `crates/flpdf/src/object_handle.rs::canonical_dictionary_key` | 旧 `Object` / `Dictionary` は削除済み。残るhelper責務はparser warningとwriter name emissionの2群。stream-filterはqpdf同様slash付きkey比較へcutover済みで、raw slashless keyは正規化せずunknownとして扱う。parser/writerの残callerが0になるまでhelper定義は残す。 |
| A24 | `QPDF::resolveObjectsInStream`（`resolved_object_streams` で二重展開防止、xref 再チェックで上書き済みメンバーを cache しない） | `libqpdf/QPDF.cc:1756-1833` | canonical 側は `crates/flpdf/src/reader/resolver.rs::ResolverCore` の `resolved_object_streams`（`crates/flpdf/src/reader/resolver.rs:324-327`）。facade 側には `crates/flpdf/src/pdf.rs:136` の `compressed_member_parents` provenance map が残る | `compressed_member_parents` prod: 6 (3 files) / test: 4。A14 専用だった ObjStm 昇格 helper は `.46` で撤去 | mixed | `crates/flpdf/src/reader/resolver.rs::ResolverCore` | canonical 側の ObjStm 展開は qpdf に対応する一方、`compressed_member_parents` は legacy cache synchronization の移行状態を記録する flpdf 側 provenance で、qpdf の `ObjCache` には対応物がない。A2/A15 と同じ legacy cache 列を畳む段階まで保持する。 |

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

## unknown / probe

本領域は 24 行すべてを source と実行済み probe で分類できたため、`unknown` に落ちた行は無い。
以下は「分類は決まったが、cutover の設計にはさらに観測が要る」項目と、その probe。

1. **A2 / A10 — legacy `ObjectCache` と canonical cache の実際の乖離量**。両者が食い違う入力が
   どれだけあるかは source からは決まらない。必要 probe:
   `cargo test -p flpdf --features qpdf-zlib-compat` を通した状態で、
   `Pdf::get_all_objects()` と `Pdf::live_object_refs()` の結果差分を fixture 全体
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


## 2026-09-06 再監査の issue 対応

親 epic は `flpdf-3yn9.48`。下表は責務と実装 issue の対応であり、完了状態は `bd show <id>` で確認する。
各 issue の受入条件に qpdf 根拠、最初の consumer、残 caller と削除条件を記録した。

| 対象行 | Beads issue | 責務 / 移行 slice |
|---|---|---|
| `A1` / `A9` / `A20` | `flpdf-3yn9.48.12` | QPDF document stateをparse前から所有しclassic trailerを同じcacheへ生成する |
| `A1` / `A20` | `flpdf-3yn9.48.15` | bootstrap rebind・warning replay・第二teardownを撤去する |
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

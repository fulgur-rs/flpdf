# D. writer — reachability, ObjStm planning / renumber / emission, xref / trailer, encryption, linearize

対象: `QPDFWriter` の `enqueueObject` / `enqueueObjectsStandard`（採番の正本、container-first）、
`preserveObjectStreams` / `generateObjectStreams`、`writeObject` / `writeObjectStream`、
`writeXRefTable` / `writeXRefStream` / `writeTrailer`、`writeEncryptionDictionary` と
`setEncryptionParameters*`、`writeLinearized`（pass1 → hint → pass2）、`QPDF_optimization` /
`QPDF_linearization` の object universe。flpdf 側は `writer.rs`（`emit_canonical_pdf_inner` の
shared plain pipeline と legacy coordinator の分岐）、`writer/rewrite_renumber.rs`
（`CanonicalCatalogFirstRenumber` / `ObjectStreamRenumber`）、`writer/object_streams/*`、
`writer/plain/*`、`writer/reachability.rs`、`writer/encryption_state.rs`、`writer/encrypted_strings.rs`、
`writer/serialize.rs`、`writer/object.rs`、`writer/pclm.rs`、`linearization/{writer,renumber,plan}.rs`、
`optimization.rs`。

**in-flight:** PR #1486（`flpdf-hi08`）が encrypted non-linearized Preserve route を変更中。
本表は main + #1486 を記述し、#1486 由来の行・注記に `(#1486)` を付ける。

## qpdf 責務モデル

（qpdf 側を先に書く: state / call order / error・warning boundary。特に「単一の `enqueueObject` が
採番の正本」「Preserve / Generate / Disable の分岐位置」「encryption dictionary は body の後」
「linearize は反復しない」を確定する）

## route matrix

| # | qpdf responsibility owner | qpdf evidence | flpdf current entrypoint | callers (prod / test) | classification | canonical owner | remaining bridge callers / notes |
|---|---|---|---|---|---|---|---|

## WriterOptions と route の対応

（どの `WriterOptions` / `WriterConfiguration` の組み合わせが shared plain pipeline / legacy coordinator /
`write_linearized` / `pclm` のどれを辿るかの表）

## unknown / probe

（決められない行と、決めるのに必要な source 箇所または probe コマンド）

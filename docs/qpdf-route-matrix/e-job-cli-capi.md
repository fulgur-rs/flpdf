# E. QPDFJob / CLI / C API 相当の consumer・adaptor

対象: `QPDFJob` の public surface（`run` / `createQPDF` / `writeQPDF` / `getExitCode` 等）と
private orchestration（`doInspection` / `handlePageSpecs` / `handleUnderOverlay` 等）の境界、
`qpdf/qpdf.cc`（CLI は `QPDFJob` public しか触らない）、`libqpdf/qpdf-c.cc`（C API が `QPDF` /
`QPDFWriter` / `QPDFJob` のどの public を叩くか）。flpdf 側は `job/lifecycle.rs`（`QPDFJob`）、
`job/mod.rs` / `json_inspect.rs` / `lib.rs` の re-export、`crates/flpdf-cli/src/main.rs` が
`QPDFJob` public を経由せず直接触る crate 項目、`crates/flpdf-qtest-tools`（`qpdf-c` /
`test_driver` の consumer 相当）。

既知の debt（重複 issue を作らず本表から引用する）: `flpdf-xsq1`（pub 可視性）、`flpdf-7bkv`
（`json_inspect.rs` compatibility re-export の撤去）、`flpdf-ei0h`（命名）、`flpdf-hxmj`
（single-/multi-source `--pages` パイプライン統合）。

## qpdf 責務モデル

（qpdf 側を先に書く: `QPDFJob::run` の call order、public / private 境界。`QPDFJob.hh` の
ネストクラス内 `private:` を外側に漏らして数えない）

## route matrix

| # | qpdf responsibility owner | qpdf evidence | flpdf current entrypoint | callers (prod / test) | classification | canonical owner | remaining bridge callers / notes |
|---|---|---|---|---|---|---|---|

## unknown / probe

（決められない行と、決めるのに必要な source 箇所または probe コマンド）

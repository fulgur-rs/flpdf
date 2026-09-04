# B. parser / xref recovery / warning・error・diagnostics

対象: `QPDFParser` の object parse、`QPDF` の xref 読み取り（classic / stream / hybrid）と
`reconstruct_xref` 回復、`readObjectAtOffset` / `readStream` の damaged-object 境界、
`QPDF::warn` → `m->warnings` の単一 sink と `QPDFExc` / `QPDFLogger` の文言・順序契約。
flpdf 側は `parser.rs`、`xref.rs`、`reader/file_object.rs`、`reader/resolver.rs` の recovery 部、
`diagnostics.rs`、`logger.rs`、`error.rs`、`tokenizer.rs`、`content_stream.rs`。

## qpdf 責務モデル

（qpdf 側を先に書く: state / call order / error・warning boundary）

## route matrix

| # | qpdf responsibility owner | qpdf evidence | flpdf current entrypoint | callers (prod / test) | classification | canonical owner | remaining bridge callers / notes |
|---|---|---|---|---|---|---|---|

## unknown / probe

（決められない行と、決めるのに必要な source 箇所または probe コマンド）

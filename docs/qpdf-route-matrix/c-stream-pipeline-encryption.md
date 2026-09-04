# C. stream data provider / decode / retry / filter / encryption / `/Length`

対象: `QPDF_Stream` の 3 つの data source（`stream_data` / `stream_provider` / original）、
`pipeStreamData` の decode level・filterable 判定・retry family、`replaceStreamData` /
`replaceFilterData` の `/Length` 契約、`QPDF::Pipe::pipeStreamData` と pipe 時 `decryptStream`、
`QPDFWriter::willFilterStream` の compress / decode 分岐、`copyStreamData`。
flpdf 側は `object_handle.rs` の stream 面、`filters.rs`、`stream_filter.rs`、`pipeline/*`、
`writer.rs::apply_stream_compress_policy` / `write_stream_payload_with_pipeline*`、
`reader/resolver.rs` の復号位置、`encryption/*`。

## qpdf 責務モデル

（qpdf 側を先に書く: state / call order / error・warning boundary）

## route matrix

| # | qpdf responsibility owner | qpdf evidence | flpdf current entrypoint | callers (prod / test) | classification | canonical owner | remaining bridge callers / notes |
|---|---|---|---|---|---|---|---|

## unknown / probe

（決められない行と、決めるのに必要な source 箇所または probe コマンド）

# A. ObjectHandle / Resolver — object identity, lazy resolve, ownership, teardown

対象: qpdf の `QPDF` が所有する object cache・遅延解決・indirect object の割当/置換/交換・
入力ソースの lifecycle/teardown と、それに対応する `QPDFObjectHandle` / `QPDFObject` / `QPDFValue` の
共有 identity。flpdf 側は `reader/resolver.rs`（`ResolverCore` / `ResolverHandle` / `DocumentResolver`）、
`reader.rs` / `pdf.rs` / `engine.rs` の `Pdf` 面、`cache.rs`、`object_handle.rs`。

## qpdf 責務モデル

（qpdf 側を先に書く: state / call order / error・warning boundary / teardown）

## route matrix

| # | qpdf responsibility owner | qpdf evidence | flpdf current entrypoint | callers (prod / test) | classification | canonical owner | remaining bridge callers / notes |
|---|---|---|---|---|---|---|---|

## unknown / probe

（決められない行と、決めるのに必要な source 箇所または probe コマンド）

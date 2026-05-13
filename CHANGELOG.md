# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] - 2026-05-13

* ci: add GitHub Actions quality and test workflow by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/4
* Add qpdf compatibility fixture set and golden baselines by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/5
* feat: page tree introspection and linearized rewrite preservation by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/2
* feat: add page tree introspection and linearized rewrite preservation by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/6
* feat: add best-effort xref repair recovery by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/3
* fix: restore compat page-tree CLI behavior by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/7
* fix: make qdf dump all objects and preserve generations by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/8
* Add incremental append-only writer by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/9
* test: verify multi-generation /Prev chain after incremental writes by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/10
* Fix object stream rewrite metadata and stream decoding by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/11
* Fix chained /Prev xref loading and regressions by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/12
* Sanitize incremental trailer when source trailer is xref stream by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/14
* Preserve xref stream form and /Prev chain in incremental writes by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/15
* Support ObjStm Extends chains by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/16
* Support incremental object deletion by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/17
* feat(api): promote CLI helpers to public lib and document surface by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/18
* linearization: define LinearizationPlan struct by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/20
* linearization: compute first-page object closure for Part 2 by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/21
* linearization: add RenumberMap (Annex F object renumbering) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/22
* linearization: emit Part 1 header + param dict with placeholders by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/23
* linearization: PageOffsetHintTable data structure (Annex F.3.1) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/24
* linearization: SharedObjectHintTable data structure (Annex F.3.2) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/25
* linearization: encode hint stream with FlateDecode (Annex F.4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/26
* linearization: layout writer orchestrating Parts 1-6 + offsets by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/27
* linearization: back-patch /L /H /O /E /T /N in param dict by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/28
* cli: add --linearize flag and check-linearization subcommand by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/29
* test: round-trip qpdf oracle tests (linearization) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/30
* test(qpdf-oracle): tighten Warn classification (flpdf-23w) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/32
* fix(linearization): batch of Critical+Major review fixes from PR #31 by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/33
* fix(linearization): qpdf-grade writer (flpdf-b82) — single-page clean by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/34
* fix(linearization): align SharedObjectHintTable with qpdf checkHSharedObject (closes flpdf-b82) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/35
* chore(linearization): P2 hardening (closes flpdf-qsx and flpdf-0ku) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/36
* fix(linearization): address PR #31 review (qpdf doc + 1-object-per-group model) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/37
* epic: linearized output (acceptance gate for flpdf-9hc.2) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/31
* test(writer): cover multi-member ObjStm rewrites by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/19
* feat(flpdf): add qpdf-zlib-compat feature for bytes-identical deflate by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/38
* feat(flpdf): add --static-id flag and WriteOptions for deterministic /ID by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/39
* feat(linearization): promote [Pages, Info, Catalog] to the Part 4 head (renumber 1/2) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/40
* feat(linearization): relocate ParamDict / HintStream slots to qpdf positions (renumber 2/2) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/41
* feat(linearization): partition Part 4 into qpdf part7/8/9 (stack 1/4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/42
* feat(object): qpdf token-boundary whitespace rules for array writer (stack 2/4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/43
* feat(linearization): emit /N before /T in param dict to match qpdf (stack 3/4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/44
* feat(object): emit printable strings as literal escapes instead of hex (stack 4/4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/45
* feat(writer): emit qpdf binary marker and preserve source version in write_qdf by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/46
* feat(version): PDF header version selection (input inherit + --min-version/--force-version) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/47
* feat(linearization): populate trailer /Info /Prev /ID for linearized output by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/48
* fix(linearization): emit startxref 0 in Part 1 trailer to match qpdf by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/49
* test(compat): add QpdfJsonComparator and StructuralComparator by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/51
* test(compat): scaffold qpdf/flpdf comparison matrix harness by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/50
* fix(linearization): include non-first-page shared objects in Shared Object Hint Table by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/52
* test(compat): curated corpus + qpdf reference outputs by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/53
* test(compat): static-id baseline byte comparison by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/54
* test(compat): per-flag golden matrix baseline by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/55
* docs(compat): decisions registry for needs-review divergences by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/56
* fix(test): tolerate CRLF on Windows in compat baseline snapshots by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/59
* ci(compat): golden matrix step + PR template checkbox by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/57
* docs(compat): golden matrix workflow + divergence categories by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/58
* feat(filters): ASCII85Decode encoder/decoder in pipeline (flpdf-9hc.20.26) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/60
* feat(writer): full-rewrite path (decode+re-encode every stream) (flpdf-9hc.20.27) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/61
* test: verify Info dict byte-equality and document preservation policy (flpdf-9hc.20.10) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/62
* feat(filter): FlateDecode-only policy under full_rewrite (flpdf-9hc.20.17) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/63
* feat(cli): top-level qpdf-style flat flags for linearize by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/69
* feat(security): wire RustCrypto primitives [epic/flpdf-9hc-3 #1/5] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/64
* feat(error): add Error::Encrypted with subkinds and Diagnostics integration [epic/flpdf-9hc-3 #2/5] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/65
* feat(security): implement V=1/V=2 key derivation (Algorithms 2, 6, 7) [epic/flpdf-9hc-3 #3/5] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/66
* feat(security): implement per-object key derivation (Algorithm 1) [epic/flpdf-9hc-3 #4/5] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/67
* feat(security): implement V=4 key derivation and /CF dispatch [epic/flpdf-9hc-3 #5/5] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/68
* Add legacy R5 AES-256 key derivation by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/70
* Add R6 AES-256 key derivation by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/71
* Add encrypted string decryption pass by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/72
* Add encrypted stream decode helper by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/73
* Wire password-based encrypted PDF opens by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/74
* Gate weak crypto PDF opens by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/75
* Support explicit Crypt stream filters by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/76
* Expose encryption permissions by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/77
* Add encrypted fixture corpus by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/78
* Support plaintext rewrite for encrypted PDFs by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/79
* Add --password-mode flag with SASLprep for V=5 by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/80
* Lock in encrypted-input rejection for rewrite --linearize by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/81
* Add ASCIIHexDecode filter codec by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/82
* Add ASCII85Decode filter-dispatch integration tests by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/83
* Add RunLengthDecode filter codec by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/84
* Add show-stream CLI subcommand for decoded stream output by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/85
* Add README.md by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/86
* Prepare flpdf for crates.io publish (epic flpdf-cag) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/87
* Release automation: release-prepare.yml + release.yml + CHANGELOG seed by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/88


**Full Changelog**: https://github.com/fulgur-rs/flpdf/commits/v0.1.1

## [0.1.0] - 2026-05-13

### Added

- Initial release: pure-Rust PDF toolkit modeled on qpdf, providing a reader
  (`Pdf::open`, `pages::page_refs`, `fonts::font_entries`,
  `filters::decode_stream_data`), an incremental writer (`write_pdf`,
  `write_qdf`), and a diagnostics pass (`check_reader`).
- `flpdf-cli` binary with `pages`, `dump-object`, `qdf`, `rewrite`,
  `show-info`, `show-catalog`, `show-metadata`, `show-stream` subcommands,
  mirroring the qpdf-equivalent inspection and rewrite surface.
- Encrypted-PDF support via Standard handler V1/V2/V4/V5 (RC4 / AES) behind
  the `--password` family of CLI flags.
- Linearization writer with hint stream generation.
- Optional `qpdf-zlib-compat` feature for byte-identical FlateDecode output
  against qpdf's `compress2()`.

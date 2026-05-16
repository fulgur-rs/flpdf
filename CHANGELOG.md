# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.3] - 2026-05-16

* Move flpdf publish dry-run from CI to release-prepare by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/92
* flpdf-9hc.5.1: ObjStm eligibility predicate (per-object) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/93
* flpdf-9hc.5.2: ObjStm packing planner: group eligible objects into batches by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/94
* flpdf-9hc.5.3: ObjStm body emitter (header pairs + object bodies) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/95
* flpdf-9hc.5.4: ObjStm stream wrapping with /FlateDecode by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/96
* flpdf-9hc.5.5: Mode dispatch: WriteOptions.object_streams field by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/97
* flpdf-9hc.5.6: Writer integration: route eligible objects through ObjStm packer by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/98
* flpdf-9hc.5.7: Force-upgrade xref form to Stream when ObjStm batches are present by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/99
* flpdf-9hc.5.10: CLI: --object-streams=preserve|disable|generate flag by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/100
* flpdf-9hc.5.11: Tests: 3 modes vs multi-ObjStm fixtures + qpdf cross-check by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/101
* Add pages::page_content_bytes helper (flpdf-avm.1) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/102
* Add pages::resolve_inherited_resources helper (flpdf-avm.2) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/105
* Add Pdf::open_mem / open_mem_owned in-memory openers (flpdf-avm.3) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/104
* Fix encode_stream_data Array filter order (flpdf-fh8) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/106
* Add flpdf::json emitter with order-preserving objects (flpdf-9hc.11.1) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/107
* Add qpdf JSON v2 envelope builder (flpdf-9hc.11.2) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/108
* Add pdf_object_to_json + build_qpdf_key (flpdf-9hc.11.3) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/109
* Add /pages serializer (flpdf-9hc.11.4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/110
* Add /pagelabels serializer + composite build_qpdf_json_v2 (flpdf-9hc.11.5) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/111
* Add /outlines serializer (flpdf-9hc.11.6) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/112
* Add /acroform serializer (flpdf-9hc.11.7) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/116
* Add /attachments serializer (flpdf-9hc.11.8) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/114
* Add /encrypt serializer + owner/user password tracking (flpdf-9hc.11.9) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/115
* Add StreamDataMode for qpdf JSON v2 stream payloads (flpdf-9hc.11.10) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/117
* Add JsonKey + filter_json_keys for --json-key (flpdf-9hc.11.11) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/121
* Add JsonObjectSelector + filter_json_objects (flpdf-9hc.11.12) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/119
* Wire --json and friends into flpdf-cli (flpdf-9hc.11.13) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/120
* flpdf-9hc.5.8.1: LinearizationPlan Part3/Part4 ObjStm batch planner by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/122
* Resilient stacked-merge workflow: design + stacked-merge skill (flpdf-418, flpdf-1oe) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/128
* flpdf-9hc.5.8.2: Thread ObjStm batch plan into linearized Part3/Part4 emission by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/129
* stacked-merge: standardize on --rebase merge + plain rebase (method B) (flpdf-b0o) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/130
* flpdf-9hc.5.8.3: Shared Object Hint Table ObjStm-awareness; defer Part-3 packing by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/124
* flpdf-56u: Split first-page/main xref streams + RenumberMap ObjStm slots by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/125
* flpdf-9hc.5.8.4: ObjStm-aware linearization check; keep Part-3 plain (qpdf-clean) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/126
* flpdf-9hc.5.8.5: Epic acceptance-gate integration tests + factual corrections by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/127
* roborev-fix: apply_static_id values[1] guard + real /E placement assert (504/777 + stale triage) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/131
* flpdf-9hc.23.2: qpdf-compatible --check exit codes (0/2/3) [stack 1/6] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/132
* flpdf-9hc.3.21: V=5 auth error parity (BadPassword before weak-crypto) [stack 2/6] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/133
* flpdf-9hc.3.20: owner/user password-match test matrix [stack 3/6] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/134
* flpdf-9hc.3.17: encryption inspection CLI subcommands [stack 4/6] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/135
* flpdf-9hc.3.18: rewrite --remove-restrictions [stack 5/6] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/136
* flpdf-9hc.3.19: --password-is-hex-key / --suppress-password-recovery [stack 6/6] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/137
* Fix release-prepare dry-run failing on uncommitted version bump by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/138


**Full Changelog**: https://github.com/fulgur-rs/flpdf/compare/v0.1.2...v0.1.3

## [0.1.2] - 2026-05-14

* Release automation: release-prepare.yml + release.yml + CHANGELOG seed by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/88
* release: v0.1.1 by @github-actions[bot] in https://github.com/fulgur-rs/flpdf/pull/90

## New Contributors
* @github-actions[bot] made their first contribution in https://github.com/fulgur-rs/flpdf/pull/90

**Full Changelog**: https://github.com/fulgur-rs/flpdf/compare/v0.1.0...v0.1.2

## [0.1.1] - 2026-05-13

* Release automation: release-prepare.yml + release.yml + CHANGELOG seed by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/88


**Full Changelog**: https://github.com/fulgur-rs/flpdf/compare/v0.1.0...v0.1.1

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

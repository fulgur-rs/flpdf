# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.4] - 2026-05-30

* flpdf-9hc.12.1: Content stream tokenizer (operators + operands) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/140
* flpdf-9hc.12.2: --normalize-content writer by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/141
* flpdf-9hc.12.3: --coalesce-contents (combine /Contents array) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/142
* flpdf-9hc.12.4: --remove-unreferenced-resources=auto/yes/no by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/143
* flpdf-9hc.12.5: --compress-streams=y/n by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/144
* flpdf-9hc.12.6: --newline-before-endstream emitter by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/145
* flpdf-9hc.12.7: CLI wiring for 5 optimization flags by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/146
* flpdf-9hc.12.8: E2E optimization flag matrix tests by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/147
* [8.1] Page range syntax parser (:odd/:even position-based, qpdf-verified) (flpdf-9hc.8.1) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/148
* [8.4] /Rotate manipulation (set/add, i64-normalized) (flpdf-9hc.8.4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/161
* [8.2] Page selection plan (single document) (flpdf-9hc.8.2) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/150
* [8.5] --rotate flag parser (flpdf-9hc.8.5) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/151
* [8.3] Multi-input page list combiner (flpdf-9hc.8.3) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/152
* [8.7] --split-pages chunked output (flpdf-9hc.8.7) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/153
* [8.8] Page tree rebuild after extraction/merge/rotate (flpdf-9hc.8.8) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/154
* [8.6] --collate combinator (round-robin) (flpdf-9hc.8.6) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/155
* [8.9] Resource pruning on extracted subsets (flpdf-9hc.8.9) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/156
* [8.10] Outline / named-destination remap (indirect/dict/direct forms) (flpdf-9hc.8.10) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/157
* [8.11] AcroForm field preservation across extract (flpdf-9hc.8.11) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/158
* [8.12] CLI: --pages/--rotate/--split-pages/--collate plumbing (flpdf-9hc.8.12) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/159
* [8.13] Tests: page-op matrix vs qpdf 11.9.0 (flpdf-9hc.8.13) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/160
* flpdf-9hc.18.1: PageDocumentHelper (pages traversal/mutation API) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/162
* flpdf-9hc.18.3: PageObjectHelper (per-page typed accessors) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/163
* flpdf-9hc.18.7: FileSpec + EmbeddedFileStream typed helpers by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/164
* flpdf-9hc.18.8: Annotation + FormField typed object helpers by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/165
* [flpdf-9hc.10.1] /Names /EmbeddedFiles name tree reader by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/166
* [flpdf-9hc.10.2] /Names /EmbeddedFiles name tree writer (insert/delete with rebalance) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/167
* [flpdf-9hc.10.3] /Filespec dict construction (/F /UF /Type /EF /Params /Desc /AFRelationship) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/168
* [flpdf-9hc.10.4] Add attachment from disk (FlateDecode, observable-equivalent) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/169
* [flpdf-9hc.10.5] Remove attachment by key (reachability-based GC, /AF cleanup) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/170
* [flpdf-9hc.10.6] List attachments (with --verbose detail) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/176
* [flpdf-9hc.10.7] Show / extract attachment to stdout or file by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/172
* [flpdf-9hc.10.8] Copy attachments from another document (with --prefix) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/173
* [flpdf-9hc.10.9] CLI: --add/-remove/-list/-show/-copy-attachments flags by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/174
* [flpdf-9hc.10.10] Tests: attachment lifecycle round-trip + qpdf cross-check by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/175
* remove_attachment GC: replace ad-hoc exclude-set logic with /Root mark-and-sweep (flpdf-eg3) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/177
* page_split: --split-pages=1 emits single-number -N.pdf (qpdf parity, flpdf-s5e) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/178
* Support Unicode attachment filenames by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/179
* ci: mirror fulgur test platform matrix (arm Linux, macOS, coverage) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/180
* flpdf-9hc.13.1: --min-version / --force-version honored on incremental path by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/181
* flpdf-9hc.13.2: default /ID is fresh random per save (ISO 32000-1 §14.4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/182
* flpdf-9hc.13.4: --static-id warns it is test-only; pin qpdf byte parity by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/183
* flpdf-9hc.13.5: accept --no-original-object-ids (top-level + rewrite) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/184
* [sub-1] flpdf-9hc.6.1 — Stream decompression in QDF mode (safe filters) + LZWDecode + WriteOptions::qdf by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/196
* [sub-2] flpdf-9hc.6.2 —  Force ObjectStreamMode::Disable in QDF by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/185
* [sub-4] flpdf-9hc.6.4 —  Emit %QDF-1.0 header marker by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/186
* [sub-5] flpdf-9hc.6.5 —  Emit %% Original object ID comments (qpdf wording) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/187
* [sub-6] flpdf-9hc.6.6 —  Force classic xref table in QDF by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/188
* [sub-3] flpdf-9hc.6.3 —  QDF body+trailer formatting (sorted keys, multiline) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/189
* [sub-7] flpdf-9hc.6.7 —  fix_qdf library (Length/xref/Size/startxref repair) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/190
* [sub-12] flpdf-9hc.6.12 —  QDF writer indirect /Length H 0 R + holder by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/191
* [sub-m41] flpdf-m41 —  parser recovers indirect /Length via endstream scan by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/192
* [sub-13] flpdf-9hc.6.13 —  qdf_fix.rs token-aware hardening by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/193
* [sub-8] flpdf-9hc.6.8 —  CLI --qdf flag + qdf-fix subcommand by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/194
* [sub-9] flpdf-9hc.6.9 — QDF round-trip + qdf-fix end-to-end matrix by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/198
* [sub-10] flpdf-9hc.6.10 — QDF framing parity (object-0 suppression + inter-object blank line) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/197
* QDF followups: write_qdf canonical, fix_qdf holder validation, xref-authoritative indirect /Length (flpdf-9hc.24/.25/.27) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/199
* QDF followups 2: per-invocation temp dirs + whole-file QDF detection (flpdf-9hc.26/.28) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/200
* fix(cli): silence --static-id warning on top-level qpdf-shaped alias (flpdf-4x6) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/201
* feat(writer): incremental generate-mode ObjStm packing (flpdf-9hc.5.9) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/202
* [flpdf-jcd.4] feat(filters): add PNG predictor encode path by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/203
* [flpdf-jcd.6] feat(writer,cli): add --stream-data flag by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/204
* fix(linearization/plan): propagate /Parent-walk resolve errors (flpdf-ws2) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/206
* [flpdf-jcd.7] test: multi-filter chain coverage by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/205
* fix(cli,json_inspect): side-file naming uses bare object number (flpdf-rq1) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/208
* fix(test,ci): tolerate qpdf 12 zero-page --check crash, re-enable qpdf on Windows/macOS CI (flpdf-d4k) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/209
* test(compat_matrix_baseline): scope /ID elision to trailer/xref-stream dicts (flpdf-d6j) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/210
* chore(license): align LICENSE-APACHE with canonical Apache 2.0 text by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/212
* feat(linearization): variable-width param dict integers (flpdf-9hc.20.25) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/211
* test(writer): combined-paths regression for incremental Generate ObjStm (flpdf-9hc.5.12) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/213
* test(json-diff): qpdf JSON v2 schema-diff corpus runner (flpdf-9hc.11.14) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/215
* docs(linearization): correct shared-hint table to "1-object-per-group" (M=N) (flpdf-9hc.20.21) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/214
* fix(json-inspect): emit b:<hex> for non-text PDF strings per qpdf JSON v2 by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/216
* feat(linearization): populate per-page content_length hint fields (flpdf-602) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/217
* docs(spec): align core design with shipped decrypt + deferred re-encrypt (flpdf-p64.8) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/218
* feat(security): /Encrypt dictionary builder for V=1/V=2 (flpdf-9hc.4.1) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/219
* feat(security): /Encrypt dictionary builder for V=4 CF (flpdf-9hc.4.2) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/220
* feat(security): /Encrypt dictionary builder for V=5 R=6 + /Perms blob (flpdf-9hc.4.3 + 4.8 partial) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/221
* Add Object accessor helpers by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/223
* feat(security): writer-side string + stream encryption passes (flpdf-9hc.4.5 + 4.6) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/222
* Refactor Object accessor callsites by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/225
* Refactor outline_dest_remap callsites to Object accessors by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/226
* Refactor remaining callsites to Object accessors by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/227
* feat(security): writer-side explicit /Crypt filter chain entry (flpdf-9hc.4.7) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/228
* feat(cli): --decrypt flag (qpdf-compatible silent /Encrypt strip) (flpdf-9hc.4.10) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/229
* feat(permissions): typed PermissionsConfig with /P bitfield encode/decode (flpdf-9hc.4.8) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/230
* feat(writer): library-side encrypt-on-write for V=4 AES-128 (flpdf-9hc.4.9 walking skeleton) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/231
* feat(cli): --encrypt for V=4 AES-128 (flpdf-9hc.4.9 CLI surface) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/232
* feat(writer,cli): --static-aes-iv test-only deterministic AES IV (flpdf-9hc.4.13) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/233
* feat(cli,writer): --copy-encryption-from for V=4 AES-128 donors (flpdf-9hc.4.11) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/234
* feat(writer,cli): V=5 R=6 AES-256 encrypt-on-write dispatch (flpdf-9hc.4.9.4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/235
* feat(cli): --allow-insecure for V=5 R=6 empty-owner encryption (flpdf-9hc.4.14) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/236
* feat(writer,cli): RC4 writer dispatch — V=1/V=2/V=4 RC4 (flpdf-9hc.4.9.1/.2/.3) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/237
* test(cli): flpdf-encrypt → qpdf-decrypt matrix + empty-user edge (flpdf-9hc.4.12, partial) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/238
* feat(cli): --encrypt permission sub-flags for 128/256-bit (flpdf-9hc.4.9.5) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/239
* feat(cli,writer): --encrypt --cleartext-metadata for V=4/V=5 (flpdf-9hc.4.9.6) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/240
* feat(writer): ObjStm + encryption — encrypt container as single blob (flpdf-9hc.4.16) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/241
* test(writer): xref-stream preserved under --encrypt with --object-streams=disable (flpdf-9hc.4.17) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/242
* feat(pages): PageWalk iterator — consolidate /Pages tree traversal by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/243
* feat(writer,cli): --force-R5 — V=5 R=5 AES-256 writer (flpdf-9hc.4.15) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/244


**Full Changelog**: https://github.com/fulgur-rs/flpdf/compare/v0.1.3...v0.1.4

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

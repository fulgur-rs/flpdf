# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.7] - 2026-07-04

<!-- Release notes generated using configuration in .github/release.yml at main -->

### Features
* feat(writer): NewlineBeforeEndstream::Never + cmp-diff-0 vs qpdf [flpdf-onao] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/303
* feat(flpdf): qpdf null-out parity for --pages outline/named-dest remap (flpdf-9hc.20.32) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/313
* feat(flpdf): qpdf null-out parity for --pages link-annot & /OpenAction dests (flpdf-9hc.20.33) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/315
* feat(flpdf): qpdf drop parity for --pages struct-tree StructElem /Pg (flpdf-9hc.20.35) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/316
* feat(workflow): pre-PR patch-coverage gate (flpdf 100%, cli best-effort) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/319
* feat(flpdf): qpdf --pages MCR/OBJR /Pg drop parity (flpdf-h2sm) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/318
* feat(flpdf): qpdf --pages thread-bead /P drop parity (flpdf-9hc.20.34) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/320
* feat(cli): qpdf-format stderr diagnostics (WARNING: <file>: <msg>) (flpdf-tc3e) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/321
* feat(cli): qpdf --check stdout checking block (flpdf-l3jx) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/324
* feat(flpdf): extract_pages multi-page extract with shared-resource dedup (flpdf-5h5.4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/325
* feat(fuzz): cargo-fuzz whole-document harness open→check→write (flpdf-hn1g.2) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/331
* feat(flpdf): multi-document merge primitive (merge_documents) — qpdf --pages parity (flpdf-5h5.6) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/329
* feat(flpdf): #![forbid(unsafe_code)] on flpdf + flpdf-cli (flpdf-hn1g.6) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/335
* feat(flpdf): opt-in decode-output limits + /Filter chain length cap (flpdf-hn1g.4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/338
* feat(flpdf): qpdf-equivalent --deterministic-id (flpdf-9hc.13.3/.13.6/.13.7) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/337
* feat(flpdf): byte-level qpdf /ID parity for --deterministic-id (flpdf-9hc.13.9) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/339
* feat(flpdf): deterministic /ID for linearized output (flpdf-9hc.13.8) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/340
* feat(flpdf): --check decodes page content streams, errors on decode failure (flpdf-gvyz) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/345
* feat(flpdf): --check opt-in decode-memory-limit (zip-bomb guard) (flpdf-svbm) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/348
* feat(flpdf): linearized output byte-identical to qpdf --linearize --deterministic-id (flpdf-9hc.13.10) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/350
* feat(flpdf): preserve already-lone-/FlateDecode streams verbatim (qpdf parity) (flpdf-9slx) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/354
* feat(flpdf): deterministic /ID direct-write — flat paths, qpdf mechanism (L1, flpdf-9hc.13.12) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/360
* feat(flpdf): deterministic /ID direct-write — classic linearized (qpdf 2-pass) (L2, flpdf-u5m8) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/361
* feat(flpdf-cli): overlay/underlay segment parser (flpdf-9hc.16.1) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/362
* feat(flpdf): overlay/underlay page content patching + byte gate (flpdf-9hc.16.3) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/364
* feat(flpdf): overlay/underlay page-range mapping (flpdf-9hc.16.4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/365
* feat(flpdf): compose multiple overlay/underlay specs (flpdf-9hc.16.5) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/366
* feat(flpdf-cli): wire --overlay/--underlay into rewrite (flpdf-9hc.16.6) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/367
* feat(flpdf): floor linearized ObjStm header to 1.5 on real emission (flpdf-6pcx · stack 2/3) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/372
* feat(flpdf): qpdf-faithful xref-stream encoder — predictor 12, /W [1 2 1] (flpdf-6pcx · stack 3/3) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/373
* feat(flpdf): wire qpdf xref-stream encoder into linearized writer — two-pass writePad (flpdf-4z56 · stack 4/5) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/374
* feat(flpdf): ObjStm container byte-parity — qpdf offset table + dict key order (flpdf-0i0s · stack 5/5) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/375
* feat(flpdf): deterministic /ID[1] byte-parity via qpdf pass-1 digest (flpdf-9ntt · stack 6/6) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/376
* feat(flpdf): ObjStm linearized qpdf structural parity (numbering/member-set/check-clean) — epic flpdf-ihb by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/371
* feat(flpdf): qpdf generate-mode ObjStm port — DFS order, even split, container-first renumber (flpdf-g6hb.1, WIP) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/381
* feat(flpdf): show-linearization (qpdf --show-linearization compat) + hint-stream decoder by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/383
* feat(flpdf-cli): qpdf-zlib-compat feature + E2E byte-identical CLI verification by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/385
* feat(flpdf): linearized generate ObjStm byte-identical at >cap (Phase 2, flpdf-g6hb.2) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/382
* feat(flpdf): in_open_document linearization category (objstm-generate) (flpdf-1dmy) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/386
* feat(flpdf): in_outlines linearization category — Outlines hint table (objstm-generate part9) (flpdf-rm09) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/387
* feat(linearization): thumbnail lc categories route to part9 (flpdf-b2lp) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/395
* feat(flpdf-9hc.13.11): preserve non-16-byte /ID[0] under --deterministic-id (qpdf getOriginalID1 parity) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/417
### Bug Fixes
* fix(writer): never emit object 0 as a body object in plain rewrite [flpdf-9hc.31] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/296
* fix(extract): neutralize cross-page annotation destinations [flpdf-4924] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/299
* fix(writer): Catalog-first object renumbering for plain rewrite [flpdf-9hc.32] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/300
* fix(writer): emit stream dicts in qpdf key order (/Length pulled out) [flpdf-tqu1] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/301
* fix(writer): classic trailer on the 'trailer' line in qpdf key order [flpdf-9hc.20.28] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/302
* fix(extract): neutralize /SD and cross-page /P vectors targeting absent pages (flpdf-2tmg) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/307
* fix: resolve indirect /V and /DV in FormFieldObjectHelper::field_value by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/309
* fix: re-open qdf+Never output with indirect /Length holder (flpdf-9hc.20.31) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/310
* fix: normalize indirect stream-valued fonts to their dictionary (flpdf-k8ms) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/312
* fix(flpdf): emit qpdf-compatible warning sequence for xref repair (flpdf-ny1f) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/317
* fix(flpdf): null-out guards surviving remapped refs (flpdf-9hc.20.36) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/323
* fix(flpdf): bound parser recursion depth to prevent stack overflow (flpdf-hn1g.1) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/327
* fix(flpdf): bound object-stream /Extends chain depth to prevent stack overflow (flpdf-hn1g.7) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/328
* fix(cli): gate deprecated R=5 (AES-256) write behind --allow-weak-crypto by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/330
* fix(flpdf): bound ref-walker inline structural depth across 8 walkers (flpdf-hn1g.9) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/332
* fix(flpdf): bound inherited_field_value /Parent chain depth (flpdf-hn1g.3) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/334
* fix(flpdf): preserve /DR resource named /P on standalone field-copy path (flpdf-4ue7) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/333
* fix(flpdf): eradicate remaining holder-chain matching gaps via shared resolve_ref_chain (flpdf-k7xx) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/336
* fix(flpdf): drop /GoTo /SD in primary inline /OpenAction, fall back to /D (flpdf-ahkf) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/342
* fix(flpdf): follow holder chains across structural one-hop resolve sites (flpdf-3x23) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/341
* fix(flpdf): collapse ResourcesLoc::Indirect holder chain to terminal (flpdf-12jh) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/343
* fix(flpdf): name/number tree root omits /Limits (ISO 32000-2 7.9.6/7.9.7, qpdf parity) (flpdf-k42w) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/347
* fix(flpdf): repair private-item rustdoc intra-doc links (flpdf-2mn) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/351
* fix(flpdf): show-encryption[-key] weak-crypto correct-password parity with qpdf (flpdf-ysb5) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/353
* fix(flpdf): --check opens weak-crypto files as read-only inspection (flpdf-mc7f) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/355
* fix(flpdf): drop OBJR /Obj-survived annotation /P, GC orphan page (qpdf --pages parity) (flpdf-u2kh) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/356
* fix(flpdf): drop thread-bead /P to a removed page nulled by a surviving dest (flpdf-eyey) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/358
* fix(flpdf): linearized ObjStm byte-parity for ineligible first-page shared stream (flpdf-ihb.2) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/380
* fix(flpdf): drop source ObjStm/XRef structural containers from linearized body (flpdf-zbf9) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/388
* fix(flpdf): route UseOutlines ObjStm outline containers to first-page section (flpdf-vvjr.1) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/389
* fix(flpdf): route classic linearize outline objects to correct half (flpdf-vvjr.2) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/390
* fix(in_outlines): exclude second-half ObjStm containers from Shared Object Hint Table by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/392
* fix(hint): in_open_document precedence + skip OD ObjStm containers from first-page SOHT by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/393
* fix(plan): verify multi-container OD ordering (flpdf-699x) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/394
* fix(linearization): preserve keeps source ObjStm grouping at >cap (flpdf-ihb.4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/397
* fix(overlay): close 4 Form XObject byte-parity gaps vs qpdf (flpdf-9hc.16.10) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/399
* fix(linearize): drop orphaned indirect /Length holders to match qpdf GC (flpdf-2vfg) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/400
* fix(rewrite): drop orphaned indirect /Length holders on full-rewrite paths (flpdf-sqkq) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/401
* fix(cli): close overlay/underlay behavior & qpdf-parity gaps (flpdf-9hc.16.9) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/402
* fix(linearize): order in_outlines above in_open_document for shared streams (flpdf-ci0r) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/403
* fix(cli): honor explicit empty --to= / --repeat= in overlay (flpdf-9hc.16.11) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/404
* fix(writer): suppress generated object/xref streams under forced sub-1.5 header (flpdf-ipc6) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/406
* fix(writer): downgrade inherited xref-stream form to classic table when force<1.5 (flpdf-w35w) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/407
* fix(linearize): exclude part9 outline-routed containers from part8 SOHT (flpdf-7aek) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/405
* fix(linearize): emit second-half ObjStm containers in part rank order (flpdf-g1eu) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/409
* fix(linearize): emit ineligible part6 outline stream after its container (flpdf-q9o3) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/408
* fix(writer): directize /Length for kept-holder passthrough/non-decodable streams (flpdf-q1j2) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/410
* fix(linearize): first-page closure ignores /Length holders + part6 source-number order (flpdf-hwx0) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/411
* fix(linearize): route open-document closure to part4 in preserve/disable mode (flpdf-lubb) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/412
* fix(writer): direct-ize /Length under --stream-data=preserve (flpdf-3g8o) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/414
* fix(linearize): GC unreachable source lin-artifacts when re-linearizing (flpdf-phfu) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/413
* fix(cli): plain rewrite must not prune /Resources entries (flpdf-79ef) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/415
* fix(writer): skip /Length edges in renumber walk, drop pre-GC orphan scan (flpdf-orv9) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/416
* fix(linearize): apply in_outlines>first-page precedence on classic path (flpdf-q2zw) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/418
* fix(overlay): normalize box geometry like qpdf getArrayAsRectangle (flpdf-lkk7) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/419
* fix(golden): tolerate placeholder-JPEG warning in kept-indirect-length --check (flpdf-rnai) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/420
* fix(linearize): drop unplanned refs from generate-mode ObjStm batches (flpdf-4vpi) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/421
* fix(linearize): harden canonical_shared_hints sort against missing renumber entry (flpdf-hn1g.10) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/422
* fix(pages): null-out only removed original page leaves, not arbitrary dest targets (flpdf-hn1g.11) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/423
* fix(signatures): proceed on signed full-rewrite like qpdf, drop the refusal (flpdf-hn1g.13) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/424
* fix(signatures): add seen-set to walk_signature_rewrite_field to prevent AcroForm DoS (flpdf-4ydy) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/425
* fix(filters): guard PNG predictor empty-input allocation against DoS (flpdf-te5g) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/426
* fix(objstm): drop dangling trailer refs in non-linearized generate (flpdf-ndjy) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/429
* fix(qdf): bound regenerated xref by object-count completeness, fixing dense-xref DoS + max_num overflow (flpdf-rnnr) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/428
* fix(qdf): tighten fix_qdf to strict 1..N file order now writer is canonical (flpdf-o10m) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/431
* fix(linearize): drop/null-ize dangling & object-0 body refs (flpdf-5apf) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/432
* fix(linearize): resurrect missing-xref array refs as null objects (flpdf-0gyq) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/433
* fix(linearize): classify first-page objects shared via document-level refs (flpdf-8891) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/435
* fix(linearize): classify first-page-direct missing array refs into Part 2 (flpdf-o9im) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/434
* fix(xref): rewrite repair scan to qpdf line-by-line reconstruct — O(n²) DoS (flpdf-m3oe) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/436
* fix(linearize): push inherited page attributes before linearization (flpdf-8wo1) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/438
* fix(coverage): scope patch-coverage.sh missing_cov exemption to declaration-only files by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/439
* fix(hn1g.15): --remove-restrictions strips /Perms /DocMDP + AcroForm sig fields (qpdf disableDigitalSignatures parity) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/440
* fix(resources): ever-seen Form XObject traversal fixes exponential-recursion DoS (flpdf-u79t) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/442
* fix(linearize): clone /Page leaves shared across /Pages parents (qpdf cache() parity, flpdf-52md) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/449
* fix(flpdf-zda0): other-page object with others>0 is lc_other (part9), not part7 by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/451
* fix(linearize): override page-tree /Type keys (qpdf 11.9.0 getAllPagesInternal parity, flpdf-nd38 PR1/4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/452
* fix(linearize): default missing leaf /MediaBox to letter/ANSI A (qpdf 11.9.0 parity, flpdf-nd38 PR2/4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/453
* fix(linearize): convert direct /Kids leaf to indirect (qpdf 11.9.0 parity, flpdf-nd38 PR3/4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/454
### Documentation
* docs: cookbook examples + API cross-references [flpdf-9hc.18.9] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/306
* docs(flpdf): verify thread-bead /P remap vs qpdf 11.9.0 duplicate-page (flpdf-77ra) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/322
* docs(flpdf): add threat model and security policy (flpdf-pcor) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/326
* docs(flpdf): mandate qpdf byte-identical mimicry as top-priority pre-v1.0 policy (flpdf-jiw6) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/359
### Internal
* fix: bounds-check xref stream offset before slicing (DoS, #304) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/305
* test(flpdf): helper API smoke + round-trip capstone [flpdf-9hc.18.10] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/308
* test(flpdf): cover public helper error paths (fonts, page/annotation helpers) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/311
* test(flpdf): cover xref.rs repair/recovery + strict error arms (flpdf-tq35) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/314
* perf(flpdf): share visited set across extract_pages closure union (flpdf-11lj) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/344
* perf(flpdf): share visited set across merge_documents closure unions (flpdf-kaej) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/346
* test(flpdf): merge trim×+N-rename composition on a secondary non-terminal field (flpdf-2c7k) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/349
* ci(flpdf): gate broken intra-doc links in the quality job (flpdf-80xq) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/352
* test(flpdf): cover linearized /F external-file lone-Flate exclusion outcome (flpdf-2tdg) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/357
* test(flpdf-vvjr.3): verify multi-container outline group_length with K=200 fixture by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/391
* test(linearization): verify-and-close ihb.3 cap-boundary stranded container by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/396
* test(linearization): correct stale SOHT comment now that fmlf is fixed by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/398
* ci(flpdf-3nrm): add PR labeler for release-notes:* labels (phase 1) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/427
* ci: add .github/release.yml for release-notes categorization (flpdf-q04y) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/437
* ci(flpdf-0i8y): pin release.yml credential/publish actions to commit SHA by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/441
* test(linearize): pin no-stream page hint-stream byte-parity; +6 gap is DEFLATE-backend, not encoder (flpdf-05jt) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/443
* test(linearize): add write_linearized depth-overflow error-arm test (flpdf-60gv) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/444
* ci(flpdf-6ri8): add dependabot.yml (github-actions + cargo) + harden pr-labeler by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/445
* ci(flpdf-zgvb): dependabot self-labels release-notes:internal + pr-labeler skips dependabot by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/448
* build(deps): bump the github-actions group across 1 directory with 3 updates by @dependabot[bot] in https://github.com/fulgur-rs/flpdf/pull/446
* ci(flpdf-r9ff): restore dependabot "dependencies" label alongside release-notes:internal by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/450
### Other Changes
* QDF: emit length-holders in sequential emission order for idempotence by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/430

## New Contributors
* @dependabot[bot] made their first contribution in https://github.com/fulgur-rs/flpdf/pull/446

**Full Changelog**: https://github.com/fulgur-rs/flpdf/compare/v0.1.6...v0.1.7

## [0.1.6] - 2026-06-07

* fix(outline_dest_remap): saturating /Count accumulation (flpdf-35z) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/273
* feat(default_appearance): /DA parser (font/size/color) [flpdf-9hc.9.3] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/274
* feat(standard_font_metrics): Adobe Core14 glyph width tables [flpdf-9hc.9.4] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/275
* feat(appearance): Tx text-field appearance stream renderer [flpdf-9hc.9.5] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/276
* feat(outline): OutlineDocumentHelper — Pdf::outline() iterable outline tree handle (flpdf-9hc.18.5) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/287
* feat(appearance): Btn checkbox/radio/pushbutton appearance renderer [flpdf-9hc.9.6] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/277
* feat(appearance): Ch combo/list appearance renderer [flpdf-9hc.9.7] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/278
* feat(page_annotation_enum): per-page annotation enumeration + widget→field linkage [flpdf-9hc.9.2] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/279
* feat(page_annotation_flatten): flatten annotations into page content [flpdf-9hc.9.8] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/280
* feat(cli): --flatten-annotations / --generate-appearances / --flatten-rotation [flpdf-9hc.9.10] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/281
* test(cli): observable-equivalence suite for AcroForm/annotation transforms [flpdf-9hc.9.11] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/282
* feat(filters): passthrough codecs + show-stream binary marker (flpdf-9hc.7.4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/283
* fix(outline): resolve indirect /Title + decode UTF-16BE titles [flpdf-289y] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/288
* feat(filters): explicit passthrough + LZWEncode-unsupported in dispatch (flpdf-9hc.7.5) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/284
* test(filters): multi-filter chains for LZW/passthrough codecs (flpdf-9hc.7.6) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/285
* test(cli): --stream-data x {LZW,DCT,JBIG2,JPX,CCITT} coverage (flpdf-9hc.7.7) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/286
* fix(resources): degrade gracefully on undecodable page /Contents [flpdf-s9s] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/289
* fix(cli): requires-password returns 3 for weak-crypto file with correct password (flpdf-63g) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/290
* docs(signed-pdf): add signed-PDF policy & scope doc [flpdf-9hc.22.9] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/291
* docs(rules): add public-API documentation review patterns [flpdf-l90q] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/292
* docs: sweep internal tracker noise from public-API doc comments [flpdf-cmlw] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/294
* fix(linearization): share QPDF_BINARY_MARKER so --linearize emits qpdf marker [flpdf-9hc.30] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/295
* feat(extract): single-page extract primitive [flpdf-5h5.3] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/293
* docs: complete public-API doc — # Errors / # Examples / intra-doc links [flpdf-xvv5] by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/297


**Full Changelog**: https://github.com/fulgur-rs/flpdf/compare/v0.1.5...v0.1.6

## [0.1.5] - 2026-06-06

* feat(reader): add borrowed object resolution by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/246
* Refactor internal resolve call sites to borrow objects by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/247
* fix(linearization): match qpdf nbits_shared_identifier formula (flpdf-9hc.20.22) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/248
* fix(linearization): always populate shared_hints to match qpdf (flpdf-vvl) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/249
* fix(json,cli): apply DecodeLevel to inline/file stream payloads (flpdf-5st) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/207
* feat(page_closure): per-page transitive object closure walker by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/250
* feat(page_splice): surgical /Pages /Kids splice with /Count maintenance by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/251
* Add signature field inspection API by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/252
* feat(object_copy): cross-document object copier (renumber + cycle handling) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/253
* [codex] Add signature rewrite impact checks by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/254
* Add AcroForm document helper by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/255
* feat(signatures): /AcroForm /SigFlags read, preserve, surface, clear (flpdf-9hc.22.6) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/256
* feat(signatures): assert incremental write preserves signed /ByteRange (flpdf-9hc.22.4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/259
* Strip signatures with remove-restrictions by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/258
* Refuse full rewrites of signed PDFs by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/257
* CLI: signed-PDF flag plumbing — Error::Signed mapping + AC matrix (flpdf-9hc.22.7) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/260
* docs: add Gemini review pattern rules (.claude/rules) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/262
* fix(signatures): resolve indirect /FT in walk_signature_rewrite_field (flpdf-967) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/264
* Add AcroForm field metadata traversal by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/261
* /Rotate flattening (CTM + box transform) (flpdf-9hc.9.9) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/263
* fix(acroform): bound reference-chain depth in collect_reachable_refs (flpdf-qjx) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/265
* perf(page_closure): use resolve_borrowed in BFS to avoid full clone (flpdf-do3) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/266
* feat(name_number_tree): generic name/number tree iteration (flpdf-9hc.18.4) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/267
* feat(page_labels): PageLabelDocumentHelper + build_number_tree (flpdf-9hc.18.6) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/268
* docs(flpdf-5h5.8): page-op API rustdoc + runnable examples by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/269
* test(outline_dest_remap): recursion-guard regression tests (flpdf-ypq) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/270
* docs(flpdf): fix broken rustdoc intra-doc links under -D warnings (flpdf-q8w) by @mitsuru in https://github.com/fulgur-rs/flpdf/pull/271


**Full Changelog**: https://github.com/fulgur-rs/flpdf/compare/v0.1.4...v0.1.5

## [Unreleased]

## [0.5.0](https://github.com/fulgur-rs/flpdf/compare/v0.4.0...v0.5.0) - 2026-08-08

### Added

- *(reader)* implement resolution xref reconstruction retry and fallback
- *(xref)* read hybrid xref streams
- *(object)* port getIntValue and getIntValueAsInt
- *(object)* warn from getKey and getKeys like qpdf
- *(object)* port warnIfPossible and objectWarning
- *(object)* port QPDFObjectHandle::typeWarning through a document warn receiver
- *(linearization)* expose writer-owned pass1 output
- route document warnings through qpdf logger
- add qpdf-style shared logger
- *(stream-filter)* register SF_Crypt through the filter factory
- *(stream-filter)* add getDecodePipeline-equivalent stage factory
- *(pipeline)* add PipelineRef for borrowed-or-owned next slots
- *(pipeline)* add public Discard terminal
- *(object_handle)* unify direct and indirect object identity
- *(reader)* add Pdf::empty() canonical minimal-document factory
- *(reader)* port qpdf stream decryption pipeline
- *(reader)* decrypt strings during object parsing
- *(parser)* add qpdf string decrypter hook
- *(parser)* port live qpdf file-object parsing
- *(writer)* add emitted-object data key state
- *(linearization)* encrypt body-object strings/streams and the hint stream
- *(linearization)* write /Encrypt in the first-half trailer only
- *(linearization)* emit the /Encrypt dictionary object
- *(linearization)* build EncryptionContext and reserve its object slot
- *(linearization)* reject object-streams + encrypt + linearize
- *(linearization)* add RenumberMap::reserve_encrypt_dict_slot
- *(object_handle)* add ObjectHandle::unparse_trailer
- *(object_handle)* add ObjectHandle::unparse_stream_body
- *(object_handle)* add ObjectHandle::unparse_object_qdf
- *(object_handle)* add unparse_object with null-suppression
- *(reader)* port qpdf's EncryptionParameters and interpretCF verbatim
- *(resolver)* report a failing input source instead of swallowing it
- *(resolver)* warn on a negative stream offset instead of failing silently
- *(resolver)* port pipeStreamData's failure arm and finish-once rule
- *(resolver)* warn on a truncated stream read as qpdf does
- *(resolver)* port QPDF::pipeStreamData's read path
- *(pipeline)* port Pl_AES_PDF's remaining vector and padding controls
- *(pipeline)* port Pl_AES_PDF's encrypting CBC path and setIV
- *(pipeline)* port Pl_AES_PDF's decrypting CBC path
- *(object_handle)* share stream payloads like qpdf's shared_ptr<Buffer>
- *(resolver)* resolve uncompressed objects by streaming the input source
- *(resolver)* move qpdf's `m->warnings` onto the resolver and emit the loop warning
- *(resolver)* port qpdf's `ResolveRecorder` as a `Drop` guard
- *(reader)* own the canonical resolver and attach it to every vended handle
- *(object_handle)* carry both PDF identity and resolver on one slot
- decode stream data from an ObjectHandle dictionary
- read filter specs from an ObjectHandle stream dictionary
- add lazily-dereferencing ObjectHandle value accessors
- align annotation flatten with qpdf helper ([#619](https://github.com/fulgur-rs/flpdf/pull/619))
- *(flpdf)* complete qpdf Filespec helper D1 ([#617](https://github.com/fulgur-rs/flpdf/pull/617))
- *(flpdf)* align qpdf page document helper boundary ([#616](https://github.com/fulgur-rs/flpdf/pull/616))
- add qpdf-native ObjectHandle dereference primitive ([#620](https://github.com/fulgur-rs/flpdf/pull/620))
- *(flpdf)* migrate json_inspect.rs section builders to ObjectHandle (flpdf-9ctq) ([#613](https://github.com/fulgur-rs/flpdf/pull/613))
- *(flpdf)* migrate pdf_object_to_json to ObjectHandle (flpdf-egzr.3.2.4) ([#612](https://github.com/fulgur-rs/flpdf/pull/612))
- *(flpdf)* chase ObjectValue::Reference redirects to their terminal value ([#607](https://github.com/fulgur-rs/flpdf/pull/607))
- *(flpdf)* ObjectHandle in-place mutation API (flpdf-egzr.3.2.12) ([#606](https://github.com/fulgur-rs/flpdf/pull/606))
- *(object_handle)* add qpdf-compatible unparse/unparse_resolved
- *(object_handle)* add qpdf-compatible type_code/type_name
- *(object_handle)* promote is_resolved to public API
- *(object_handle)* round out boolean/real/name/string/reference accessors
- *(object_handle)* add Operator/InlineImage value representation
- *(reader)* get_all_object_handles and trailer_handle
- *(reader)* materialization bridge - resolve/resolve_borrowed cut onto ObjectHandle graph
- *(parser)* build the ObjectHandle graph with parsed offsets for file objects
- *(reader)* dual-write ObjectHandle resolution alongside the legacy engine
- *(reader)* canonical indirect ObjectHandle registry on Pdf
- *(object_handle)* real ObjectValue payload for direct handles
- *(object_handle)* parsed-offset sentinel and set-once contract
- *(object_handle)* scaffold ObjectHandle identity
- *(qtest)* expose canonical qpdf string operations
- *(filters)* configure filter chain limit
- *(pipeline)* add qpdf PlStdioFile terminal
- *(pipeline)* add sticky PlOStream terminal
- *(pipeline)* add qpdf-compatible PlBase64
- *(pipeline)* add PlConcatenate finish suppression
- *(pipeline)* publish contract and add PlString
- *(pipeline)* add qpdf Pl_LZWDecoder and Pl_PNGFilter components
- route ASCII and RunLength decode through pipelines
- add qpdf RunLength pipeline
- add qpdf ASCIIHex pipeline
- add qpdf ASCII85 pipeline
- *(content)* add qpdf resource replacer
- *(content)* add qpdf resource finder
- add qpdf token filter pipeline
- expose stream filter driver contract
- run stream flate through pipeline
- add qpdf stream filter driver
- *(rc4)* add qpdf PlRc4 pipeline stage
- build qpdf object-user maps
- add optimization object-user maps
- add xref entry value component
- add pipeline-backed bit writer
- add qpdf bit stream reader
- add qpdf-compatible flate pipeline
- add buffer and count pipeline stages
- add pipeline lifecycle contract
- *(content)* add qpdf token normalizer
- *(json)* add qpdf stdio side-file adapter
- *(json)* port qpdf JSON handler
- *(json)* port qpdf schema checking
- *(json)* add qpdf parser Reactor
- *(json)* parse qpdf JSON containers
- *(json)* parse qpdf scalar JSON
- *(json)* add qpdf incremental writer
- *(json)* match qpdf tree and blob writing
- *(json)* add qpdf shared containers
- *(json)* add qpdf shared scalar model
- *(content)* add qpdf parser callbacks
- *(object)* add qpdf content object values
- *(tokenizer)* match qpdf inline image scanning
- *(tokenizer)* port qpdf push state machine
- *(nntree)* add typed helpers and migrate consumers
- *(nntree)* port structural auto-repair
- *(nntree)* port mutation and recursive splitting
- *(nntree)* port targeted lookup
- *(nntree)* port bidirectional cursor traversal
- *(pdf-version)* add qpdf-compatible value type

### Fixed

- remove obsolete xref recovery state
- preserve candidate xref diagnostic order
- keep parser docs valid outside tests
- align damaged xref recovery with qpdf ObjStm behavior
- *(linearization)* satisfy review quality gates
- *(linearization)* splice hint stream from single pass
- *(docs)* qualify recovered object handle link
- *(reader)* enumerate recovered xref objects
- *(reader)* route recovered state through public paths
- *(reader)* sync recovered xref before mutations
- *(reader)* preserve qpdf recovery fallbacks
- synchronize legacy state after xref recovery
- oracle-match corrections from Codex review (C1/C2/C3/C4/C6)
- *(xref)* require type on hybrid xref streams
- *(object)* propagate the clamp-warning sink failure try_get_int_value_as_int's doc claimed it couldn't
- *(object)* propagate live resolution failures from warnIfPossible
- *(linearization)* match qpdf pass1 stdio lifecycle
- *(linearization)* match qpdf pass1 boundaries
- *(linearization)* satisfy pass1 clippy gate
- *(reader)* avoid duplicate object warning offsets
- *(reader)* preserve object warning prefix syntax
- *(check)* preserve terminal repair warnings
- *(check)* propagate warning logger failures
- *(reader)* route warnings before terminal open errors
- *(reader)* format warning locations like qpdf
- *(logger)* normalize standard output streams
- *(stream-filter)* correct the SF_Crypt citation and re-measure the mutation map
- *(stream-filter)* keep every /DecodeParms key on a Crypt stage
- *(object)* keep a promoted stream's dictionary private again
- *(object)* refuse to shallow-copy a stream, matching QPDF_Stream::copy
- *(object)* share a promoted stream's dictionary instead of privatizing it
- *(object)* return the canonical terminal handle instead of a copy
- *(writer)* rebuild null-replaced source ObjStm
- *(writer)* rebuild deleted source ObjStm placeholders
- *(engine)* retain factory rustdoc links
- *(object_handle)* attach children after terminal replacement
- *(object_handle)* disconnect shared values like qpdf
- *(page_extract,page_merge)* delegate target construction to Pdf::empty()
- fix filespec payload from lazy original source
- distinguish destroyed handles from null
- align qpdf header validation
- *(parser)* match qpdf reference diagnostics
- *(parser)* use qpdf-style live frame stack
- *(parser)* scope live input conversion by target width
- *(writer)* match qpdf AES data key length
- *(linearization)* pin the hint stream's AES IV across the convergence loop
- *(linearization)* replace shape-enumeration with a generic /Extensions indirect-ref walk
- *(linearization)* also reject a nested indirect /ADBE, not just top-level
- *(linearization)* cov:ignore catalog_extensions_is_indirect's defensive guards
- *(linearization)* inject /Extensions /ADBE /ExtensionLevel for V5 encryption
- *(linearization)* replace vacuous hint-stream ciphertext check with a falsifiable one
- *(linearization)* cov:ignore the V5 reopen failure-only panic branches
- *(linearization)* port --cleartext-metadata exemption to the linearized writer
- *(linearization)* collapse encrypt-skip nested ifs, cov:ignore phantom-uncovered lines
- *(linearization)* pin exact byte spacing of /Encrypt in trailer test
- *(linearization)* move cov:ignore markers inline (patch-coverage keys off the code line, not a preceding comment line)
- *(linearization)* avoid the literal cov:ignore token in a doc comment
- *(object_handle)* defer Sig+ByteRange detection to the loop's own key
- *(object_handle)* apply Sig+ByteRange hex-string case and fix refiltered ordering
- *(object_handle)* correct doc nits in unparse_stream_body_qdf
- *(object_handle)* add ObjectHandle::unparse_stream_body_qdf
- *(object_handle)* address review findings on 26ebf6af
- *(object_handle)* handle Stream self in unparse_stream_body
- *(object_handle)* address review findings on 275dad04
- *(object_handle)* correct unparse_object doc claims, pin boundary cases
- *(resolver)* report getLastOffset and make the stream allocation fallible
- *(resolver)* finish the pipeline after every failure, and seek before allocating
- *(writer)* use qpdf's static AES initialization vector
- *(pipeline)* put the AES module's qpdf classification on one line
- *(resolver)* report a malformed object's position in the file
- *(resolver)* let a direct value end the input, as the stream path does
- *(resolver)* grow the stack on a chained `/Length` instead of aborting
- *(resolver)* raise the body parse's own diagnostics as warnings
- *(resolver)* validate a declared /Length before allocating it
- *(resolver)* keep the D4 classification on one line
- *(resolver)* use is_multiple_of in the reluctant-reader fixture
- *(resolver)* repair the CI doc gate, prune 20 redundant bounds, and correct four claims
- *(fuzz)* repair the fuzz target the `Arc<[u8]>` change broke, and correct four ledger claims
- drop beads issue IDs from user-facing help and error text
- match qpdf doListAttachments in --list-attachments
- resolve /DecodeParms values only for filters that read them
- apply codec prefix decode params outside debug_assert (flpdf-4rfl)
- track direct handle containment owners ([#623](https://github.com/fulgur-rs/flpdf/pull/623))
- *(filespec)* align factory ownership with qpdf ([#622](https://github.com/fulgur-rs/flpdf/pull/622))
- *(flpdf)* address Codex post-merge findings on PR #613 (flpdf-9ctq) ([#614](https://github.com/fulgur-rs/flpdf/pull/614))
- *(reader)* don't charge a stream dictionary its own inline-nesting level
- *(reader)* decrypt native-parsed strings at handle population, not just on the legacy resolve() path
- make the ar_archive_writer MSRV pin an active resolver constraint
- *(flpdf)* preserve content-token objects passed to set_object
- *(flpdf)* reset parsed offset when disconnecting a handle
- *(flpdf)* reset parsed offset when a handle is marked missing
- *(flpdf)* stop ObjectHandle Debug from recursing through indirect cycles
- *(flpdf)* break Rc reference cycles between resolved indirect handles
- *(reader)* memoize trailer_handle for canonical identity across calls
- *(reader)* address review findings in the materialization bridge
- *(parser,reader)* resolve two code-quality Minor findings
- *(reader)* fall back to lift when the native bounded window is insufficient
- *(reader)* bound lift/lift_to_handle recursion depth, fix test docstring overclaim
- *(reader)* correct qpdf citation path, record Send/Sync auto-trait deviation
- *(object_handle)* is_null must not assume null for unresolved indirect handles
- *(object_handle)* cover the zero-offset boundary, drop stale dead_code on Repr
- *(object_handle)* correct QPDFValue.hh citation for set-once offset guard
- *(object_handle)* satisfy qpdf-module-docs check, derive Debug on handle types
- *(object_handle)* move Rc<RefCell> deviation note to a plain comment
- *(writer)* escape non-QDF dictionary keys
- *(xref)* honor leading PDF header origin
- *(xref)* scope qpdf header validation to repair
- *(qtest)* close final driver parity gaps
- *(xref)* allow empty reconstructed xref
- *(filters)* avoid strict decode data cloning
- *(qtest)* preserve failed repair diagnostics
- *(reader)* retry bounded stream offset parsing
- *(qtest)* preserve bounded recovery event ordering
- *(qtest)* preserve downstream cleanup boundary
- *(qtest)* preserve multistage recovery order
- *(qtest)* preserve recovery output event order
- *(filters)* replay warnings after decode errors
- *(qtest)* preserve stream diagnostic order
- *(qtest)* recover partial stream decode output
- *(qtest)* derive stream warning offsets from parser
- *(json)* retain oversized stdio tails
- *(json)* match stdio buffered write boundaries
- *(json)* preserve stdio interrupted writes
- *(pipeline)* match qpdf interrupted writes
- *(json)* buffer file output and refresh fuzz lock
- *(json)* preserve raw PDF write units
- *(json)* match qpdf pipeline write boundaries
- *(pipeline)* integrate byte-exact errors after rebase
- *(json)* match qpdf dictionary key writes
- *(filters)* construct every decode pipeline before decoding any stage
- satisfy stable Rust clippy
- preflight stream codec pipelines
- *(resources)* avoid cloning borrowed operands
- *(resources)* preserve replacements before inline image EOF
- *(resources)* preserve XObject encounter order
- *(resources)* avoid redundant callback clones
- *(resources)* address review findings
- *(resources)* align qpdf edge contracts
- address final stream filter review
- surface flate warnings during checks
- reject unreported flate warnings
- preserve RC4 stream allocations
- address PlRc4 review findings
- mirror qpdf pipeline error propagation
- align flate dictionary timing with qpdf
- complete flate streaming state semantics
- *(cli)* tolerate non-stream page contents
- *(cli)* normalize indirect page content streams
- *(tokenizer)* mirror qpdf constructed tokens
- *(json)* match qpdf lazy object failure prefix
- *(json)* match qpdf side-file finish semantics
- *(json)* make stdio adapter writes retry-safe
- *(json)* preserve raw dictionary key order
- *(json)* preserve side-file error context
- *(json)* match qpdf file-mode failure boundaries
- *(json)* write file payloads incrementally
- *(json)* preserve decoded payloads and verify output handles
- *(json)* normalize emitted stream dictionaries
- *(json)* reject output aliases of input
- *(json)* preserve empty qpdf object map shape
- *(json)* preserve PDF real number tokens
- *(json)* break recursive handler ownership cycles
- *(json)* preserve validation diagnostic bytes
- *(json)* refresh active recursive snapshots
- *(json)* re-read live container callbacks
- *(json)* match qpdf handler ownership and live lookup
- *(json)* make handler dispatch live and cycle-safe
- *(json)* allow shared callback reentry
- *(json)* support recursive handler graphs
- *(json)* preserve parser diagnostic bytes
- *(json)* retry interrupted parser reads
- *(json)* parse readers incrementally
- *(json)* match qpdf low surrogate sentinel
- *(json)* batch blob base64 writes
- *(json)* preserve Base64 tails across writes
- *(json)* match qpdf blob callback boundaries
- *(json)* observe live writer mutations
- *(json)* write dictionaries from live entries
- *(json)* release array borrow before callbacks
- *(json)* stream blob base64 output
- *(json)* allow dictionary mutation during iteration
- *(json)* preserve NaN sign
- *(json)* match qpdf special real values
- *(parser)* count dictionary close in recovery streak
- *(parser)* recover malformed content like qpdf
- *(parser)* remove remaining pull scanners
- *(nntree)* attribute search errors to root
- *(nntree)* preserve split invariants
- *(json)* preserve indirect names holder chains
- *(nntree)* enforce helper boundaries
- *(nntree)* attribute deep lookup errors
- *(nntree)* harden indirect tree updates
- *(nntree)* preserve NUL in PDFDocEncoding keys
- preserve compressed object warnings
- preserve tokenizer review parity
- address tokenizer review feedback
- match qpdf JSON name projection
- preserve integer tokenizer diagnostics
- parse objects through qpdf-shaped tokens
- *(writer)* derive xref form from final object placement
- *(writer)* enforce plain pipeline invariants
- *(writer)* exclude deleted preserve fallback refs
- *(writer)* exclude removed and structural sources
- *(writer)* preserve legacy routing intent
- *(writer)* repair unparseable headers for plain xref streams
- *(writer)* enforce plain stream plan floors
- *(writer)* validate plain write plan consistency
- *(writer)* preserve plain xref entry bounds
- *(writer)* validate classic xref layouts
- preserve previous xref diagnostics
- preserve lazy stream recovery diagnostics
- keep strict recovery terminators line-anchored

### Other

- cover xref diagnostic propagation paths
- Merge pull request #693 from fulgur-rs/fix/flpdf-4zt3-drop-objstm-recovery
- Merge pull request #691 from fulgur-rs/feature/flpdf-26l3-linearized-hint-splice
- *(linearization)* close patch coverage gaps
- *(linearization)* cover malformed page plan
- Merge pull request #690 from fulgur-rs/fix/flpdf-2unc-hint-stream-convergence-predicate
- Merge pull request #689 from fulgur-rs/feature/flpdf-25kg.3.30-effective-xref
- Merge pull request #688 from fulgur-rs/feature/flpdf-25kg.3.33
- Merge pull request #687 from fulgur-rs/feat/flpdf-25kg.3.32
- *(cache)* cover recovered enumeration states
- *(reader)* cover public recovery bridge branches
- add coverage tests and cov:ignore annotations for xref reconstruction
- initialize PdfOpenOptions using struct literal in tests
- format resolver tests with cargo fmt
- Merge pull request #683 from fulgur-rs/feature/flpdf-9hc.17.1-ignore-xref-streams
- *(plans)* fix five more plan/comment inaccuracies caught by review
- *(object)* serialize the default-logger captures and pin the silent getKey
- *(object)* defer the getKey and getKeys warning arms
- *(object)* cover the sinkless-resolver, dropped-document, and live-resolver routes
- *(linearization)* classify pass1 coverage exclusions
- *(linearization)* document pass1 marker invariant
- *(reader)* format warning regression
- *(reader)* update warning formatter test
- cover logger reset and identity contracts
- *(stream-filter)* cover the test doubles' decode_pipeline bodies
- state observability and finish labelling the deviation records
- name the byte gate and correct the decode-pipeline deviation records
- record the decode-pipeline ownership and factory substitutions
- *(stream-filter)* cover decode_pipeline construction, streaming, and faults
- *(pipeline)* let Flate and LZW take a borrowed or owned next
- *(reader)* record why the chased terminal is never NotYetResolved
- Merge pull request #676 from fulgur-rs/fix/flpdf-um4z-source-objstm
- *(writer)* cover ObjStm planning error paths
- *(writer)* place Preserve ObjStm by source identity
- *(writer)* renumber source-backed ObjStm groups
- *(writer)* retain Preserve ObjStm group identity
- Merge pull request #668 from fulgur-rs/feature/flpdf-qynx.8-pl-discard
- *(filespec)* use canonical Discard terminal
- restore factory correspondence accuracy
- record Pdf engine factory ownership
- *(engine)* extract Pdf factory orchestration
- Merge pull request #664 from fulgur-rs/feature/flpdf-25kg.3.26-uniform-object
- *(object_handle)* cover disconnect terminal states
- *(object_handle)* close uniform identity branch coverage
- *(object_handle)* satisfy clippy for stream identity assertion
- *(object_handle)* correct qpdf teardown scope
- *(object_handle)* map uniform slots to qpdf 11.9.0
- *(object_handle)* derive containment roots from shared slots
- Merge pull request #657 from fulgur-rs/feature/flpdf-25kg.3.19-empty-pdf-factory
- *(engine)* move Pdf::empty() out of reader.rs into new engine.rs
- port lazy original stream source
- complete string decryption coverage
- cover parser string decryption parity
- Merge pull request #651 from fulgur-rs/feature/flpdf-25kg.3.18-qpdf-parser-live
- *(parser)* scope qpdf small-stack coverage
- *(parser)* cover explicit parse recovery errors
- *(parser)* cover live recovery boundaries
- Merge pull request #649 from fulgur-rs/feature/flpdf-25kg.3.15-interpret-cf
- Merge pull request #646 from fulgur-rs/feature/flpdf-txag-linearize-encrypt
- *(linearization)* stop overclaiming the hint-stream IV-pinning fix
- *(linearization)* mark remaining defensive/coverage-artifact lines
- *(writer)* extract cipher_needs_aes_iv to fix a coverage-region split
- *(linearization)* fix stale test comment claiming linearize+encrypt is unsupported
- *(linearization)* cover resolve_catalog_adbe_status's non-Dict/Ref arm
- *(writer)* update stale single-caller comments on the ADBE helpers
- *(linearization)* note the V5 negative plaintext-leak checks' premise
- *(linearization)* cover V5R6Aes256/V5R5Aes256 and hint/xref encryption qualitative checks
- *(linearization)* guard cleartext-metadata against the default re-filter policy
- *(linearization)* fix stale qpdf citation, warn future implementers about shared xref-stream fns
- *(linearization)* cov:ignore the encrypt-emission test's failure-only lines
- *(linearization)* document write_linearized's new copy-encryption error arm
- *(linearization)* harden the id0-placeholder invariant with a debug_assert
- *(linearization)* rename cleartext-metadata test to match what it proves
- *(linearization)* cover the new encrypt-context block, mark ignored test's body cov:ignore
- *(linearization)* make the guard-message assertion branch-independent
- *(linearization)* compute /ID before renumbering
- *(linearization)* clarify ObjStm+encrypt guard is not a qpdf constraint
- *(writer)* widen encryption context visibility to pub(crate)
- *(json)* name the QPDF_json.cc module document_json (flpdf-ridh)
- *(json)* retire the materialized qpdf-key twin (flpdf-ridh)
- *(json)* split QPDF::writeJSON out of json_inspect (flpdf-ridh)
- *(qpdf-correspondence)* record ObjectHandle writer-emission primitives
- *(object_handle)* close DecodeParms non-refiltered coverage gap on 878c878c
- *(object_handle)* address review finding on faa40220
- *(object_handle)* promote try_dereference/try_is_null out of dead-code
- *(object)* promote real_literal_is_safe to pub(crate)
- Merge pull request #643 from fulgur-rs/feature/flpdf-25kg.3.13-encryption-parameters-parity
- *(reader)* point EncryptionMode at the methods that resolve a cipher
- *(reader)* pin the missing-/V rejection on both authentication paths
- *(reader)* cover the unknown crypt-filter report and the /EFF selector
- Merge pull request #640 from fulgur-rs/feature/flpdf-25kg.3.10-pipe-stream-data
- *(resolver)* share one finish counter across the pipe tests
- *(resolver)* escape the placeholder in the decoding-failure message
- *(resolver)* drop the fault reader's unreachable healthy read path
- Merge pull request #639 from fulgur-rs/feature/flpdf-bv2r-u-padding
- Merge pull request #637 from fulgur-rs/feature/flpdf-25kg.3.11-resolver-encp
- *(pipeline)* close the last two uncovered AES lines
- *(pipeline)* cover the AES stage's non-PKCS#7 strip and short-block recovery
- *(pipeline)* mark the AES stage not-yet-wired, matching the other Pl_* stages
- sweep comments that still name the old stream fields
- *(object_handle)* name the stream fields after qpdf's members
- *(object_handle)* state why sharing the payload is sound
- *(object_handle)* drop the shape-correspondence claim for Rc<Vec<u8>>
- *(object_handle)* assert buffer identity against the test's own Rc
- Merge pull request #632 from fulgur-rs/feature/flpdf-1c7z-sha2-pipeline
- Merge pull request #630 from fulgur-rs/feature/flpdf-25kg.3.5-canonical-resolver
- *(resolver)* pin that a buffer-boundary EOF still refills
- *(resolver)* cite the throw that raises `expected n n obj`
- *(resolver)* pull the input in geometric steps, not fixed ones
- *(resolver)* tighten the QPDFParser citations
- *(page_split)* take the source by value instead of copying it
- *(resolver)* pin the `endobj` half of the EOF token, and the two seeks
- *(resolver)* assert the overflow refusal without an uncovered arm
- *(resolver)* pin /AP /N nested dereference through the owning document
- *(resolver)* replace two reasoned claims in the new fixture with measured ones
- *(resolver)* pin the /Length seam with a self-referential fixture
- *(object-handle)* assert the null variants with matches!, not a mapping
- *(resolver)* evict the legacy read helpers from ResolverCore
- *(resolver)* pin that two nested read_stream frames keep separate offsets
- *(resolver)* cover the streaming read's error and EOF branches
- *(resolver)* move the canonical handle registry onto the resolver
- *(check)* drop a clone the owned snapshot made redundant, and pin why snapshots are safe
- *(resolver)* record that the loop null's two routes differ but are unobservable
- *(reader)* pin the `Arc`-not-`Rc` rationale with doctests instead of asserting it
- *(reader)* require `R: 'static`, and take `Arc<[u8]>` in `open_mem`
- *(object_handle)* correct three parity-ledger claims on the new constructor
- *(object_handle)* make the dead_code note true in both builds
- bound a /DecodeParms name to the Crypt stage that reads it
- cover inline and non-dictionary filespec name-tree values
- Merge pull request #626 from fulgur-rs/feature/flpdf-25kg.3.4-objecthandle-stream-decode
- say max_filter_chain's None is a caller's choice, not the default
- correct three claims in the replicated-snapshot ledger
- snapshot a replicated /DecodeParms once, not once per filter
- qualify the qpdf and flpdf citations added for the length accessor
- correct what a dropped document does to a /Filter handle (D1)
- size both filter arrays before snapshotting them
- state the retention-insensitivity claim for both readers
- retain only the /DecodeParms keys a filter reads
- pin the per-spec threading; narrow the non-resolving classification claim
- keep the new assertions' failure paths off the coverage gate
- pin the corpus helper's direct-only guard arm
- scope the shared-engine claim to max_output
- record the ObjectHandle-native decode boundary
- pin the warning text and zlib code the corpus leans on
- scope the D4 corpus row to reader agreement
- pin legacy and ObjectHandle decode equivalence
- pin the native decode path's Crypt provider and live indirect dictionary
- pin that the native entry point dereferences its stream dictionary
- scope the getStreamData and recovering-form parity claims
- scope the chain-count and /DecodeParms parity claims to what holds
- scope the key-order note and record the shared-fixture contract
- cite the qpdf base-class default behind set_decode_params
- pin that a non-dictionary /DecodeParms reduces to Present, not Absent
- scope the Crypt arm pinning claim to the mutations proved
- state the D2 test's pinning claim as the mutation actually proved it
- bound the ParamValue integer invariant to what each reader can honor
- split the getIntValueAsInt clamp from its Object reader
- pass DecodeParams to the Crypt stage provider
- pin what a non-dictionary /DecodeParms actually leaves in place
- take DecodeParams in StreamFilter::set_decode_params
- correct DecodeParams qpdf attributions and tighten the bridge
- give FilterSpec a shape-neutral DecodeParams
- Port qpdf page insertion ownership ([#621](https://github.com/fulgur-rs/flpdf/pull/621))
- 対応表を Phase 2 着手時に再測し、責務の帰属誤りを訂正 (flpdf-1e5g) ([#615](https://github.com/fulgur-rs/flpdf/pull/615))
- pin the exact MAX_PARSE_DEPTH boundary for trailer_key_handle ([#611](https://github.com/fulgur-rs/flpdf/pull/611))
- remove driver::Handle, consume ObjectHandle directly ([#610](https://github.com/fulgur-rs/flpdf/pull/610))
- Merge pull request #603 from fulgur-rs/feat/flpdf-egzr-3-2-1-objecthandle-api
- Fix stack-overflow abort in native ObjectHandle parser recursion
- *(flpdf)* fix accessor docs to say only unresolved indirect handles miss
- Fix resolve()/resolve_borrowed() regression on deeply nested compressed objects
- *(flpdf)* cover the remaining ObjectHandle Debug resolution states
- *(flpdf)* cover the direct-handle arm of ObjectHandle::strong_count
- *(reader)* cover delete_object's split-out object-0 early return
- *(object_handle)* close patch-coverage gaps in the materialization bridge
- *(reader)* record the narrow bounded-window-only native reparse gap
- *(reader)* mark lift's now-unreachable Stream/Operator/InlineImage/Reference arm
- *(parser)* close patch-coverage gaps in the new ObjectHandle native path
- *(reader)* cite qpdf's per-document object cache for get_object_handle identity
- *(object_handle)* fix is_null/dictionary/real_literal doc accuracy
- Merge pull request #597 from fulgur-rs/flpdf-egzr.4
- Fix rustdoc private-intra-doc-link on prepare_for_optimization
- Address remaining flpdf-test-tokenizer review findings
- Move token_type_name back out of flpdf, matching qpdf's own layout
- Simplify tokenizer_runner.rs and dedup token_type_name
- Fix rustdoc private_intra_doc_links on resolve_ref_chain
- Follow the full /Type reference chain when classifying object streams
- Fix endstream search to match qpdf's Finder algorithm, narrow qtest-driver exposure
- Remove qtest_tokenizer.rs; make tokenizer module directly pub under feature gate
- Move token_type_name to tokenizer_runner, matching qpdf test_tokenizer.cc placement
- Add flpdf-test-tokenizer binary: qtest helper for tokenizer observable parity
- classify qtest string correspondence
- *(filters)* cover leading identity Crypt stage
- *(qtest)* eliminate final patch coverage gaps
- *(qtest)* cover final parity boundaries
- *(xref)* simplify strict header parse
- *(coverage)* exercise adapter and writer boundaries
- *(qtest)* cover strict replay boundaries
- *(qtest)* cover predictor warning replay
- *(qtest)* simplify bounded fixture assertion
- *(qtest)* cover bounded recovery cleanup
- *(qtest)* cover recovery boundary mapping
- *(qtest)* cover chained recovery cleanup
- *(qtest)* cover prior recovery error data
- *(qtest)* cover final recovery warning order
- *(json)* cover portable side-file error assertion
- *(json)* make side-file open error portable
- *(pipeline)* record JSON stdio correspondence
- *(pipeline)* assert stdio callback lifecycle
- *(pipeline)* cover qpdf stdio lifecycle oracle
- *(json)* use qpdf top-level file lifecycle
- *(json)* expose side-file lifecycle for tests
- *(json)* cut side files over to PlStdioFile
- *(json)* keep batching assertion covered
- *(json)* cover raw non-finite scalar rejection
- *(json)* close chunk trace coverage gaps
- *(pipeline)* restore coverage assertions
- *(pipeline)* annotate unreachable coverage regions
- *(pipeline)* close JSON stage coverage gaps
- *(pipeline)* record JSON stage correspondence
- *(cli)* delegate JSON terminals to library
- *(json)* pin reader errors to parse category
- *(json)* remove obsolete Write surfaces
- *(json)* cut serialization over to Pipeline
- *(pipeline)* add qpdf JSON stage oracle
- *(pipeline)* verify empty concatenate writes
- Merge pull request #586 from fulgur-rs/fix/flpdf-eata-probe-etxtbsy
- *(probe)* run stand-in probe scripts through /bin/sh
- *(pipeline)* drive the LZW/PNG fake probe through a shell script
- *(pipeline)* replay every LZW/PNG oracle case against flpdf itself
- *(filters)* drop the unreachable legacy decode route
- *(filters)* correct the decode and encode contracts after the cutover
- *(filters)* route LZW and PNG predictor through the qpdf stream filter
- clarify check codec limits
- use portable true probe path
- keep codec parity assertions covered
- avoid direct exec for fake codec probes
- execute RunLength boundary fallbacks
- verify stream codecs against qpdf
- remove whole-buffer stream codecs
- cover ASCII85 pipeline edge paths
- Merge pull request #578 from fulgur-rs/feature/flpdf-qynx-3-resource-cutover
- *(resources)* exclude sink boilerplate from coverage
- clarify resource pipeline ownership
- *(content)* make resource finder failure probe portable
- *(content)* cover resource finder oracle boundary
- *(content)* remove obsolete scanner helpers
- *(resources)* use shared resource finder
- *(overlay)* cut appearance streams over to resource replacer
- *(overlay)* cut default appearance over to resource replacer
- *(content)* cut normalizer over to tokenizer pipeline
- cover qpdf token filter pipeline
- scope stream warning helper to tests
- borrow first stream filter input
- cover inline content warning location
- retain flate warnings before later decode errors
- cover stream filter driver branches
- align empty object streams with PlFlate
- cut filters over to PlFlate
- cover tokenizer probe wrapper
- *(rc4)* cover pipeline contract boundaries
- *(rc4)* stabilize probe process boundary
- *(rc4)* cut stream consumers over to PlRc4
- *(rc4)* add qpdf PlRc4 differential
- cover optimization error paths
- index optimization correspondence
- cut linearization over to optimization maps
- align xref coverage exclusions
- finish xref entry cutover
- cut over all xref entry consumers
- Merge pull request #572 from fulgur-rs/feature/flpdf-qynx-2-1-rc4-core
- pin qpdf flate finish error category
- scope stack budget regression to linux x86_64
- close pipeline review gaps
- cover pipeline boundary contracts
- record pipeline component correspondence
- satisfy pipeline lint gates
- route hint decoding through bit stream
- route hint encoding through pipelines
- share flate buffer status handling
- remove unreachable flate test states
- align flate framing cases with qpdf oracle
- exercise flate buffer exhaustion paths
- cover flate defensive state paths
- cover flate streaming edge states
- *(content)* address review feedback
- *(pages)* avoid private intra-doc link
- *(content)* cover oracle harness and string forms
- *(docs)* audit content normalizer mirror
- *(content)* gate qpdf normalizer parity
- *(content)* retain inline image callback coverage
- *(content)* replace object normalizer
- *(json)* [**breaking**] finalize exact output API
- *(json)* cover final stdio retry boundaries
- *(json)* align integration tests with live API
- *(json)* update side-file path ownership
- *(json)* update stream payload helper purpose
- *(json)* cover integration projection branches
- *(json)* cut CLI over to qpdf streaming
- *(json)* stream qpdf JSON document output
- *(json)* migrate inspection values
- *(json)* use keyed schema lookups
- *(json)* assert handler errors as bytes
- *(json)* cover unconfigured boolean handler
- *(json)* make handlers live shared handles
- *(json)* classify validation qpdf correspondence
- *(json)* cover live fallback replacement
- *(json)* remove unreachable schema branch
- *(json)* cover validation fallthroughs
- *(json)* cover every pattern schema member
- *(json)* cover schema recursive validation
- *(json)* correct parser streaming contract
- *(json)* cover diagnostic message debug output
- *(json)* classify parser qpdf correspondence
- *(json)* complete live writer branch coverage
- *(json)* document unreachable writer tag guards
- *(json)* classify core qpdf correspondence
- *(json)* cover qpdf real rounding
- *(json)* cover snapshot routing
- *(json)* cover scalar and container edges
- *(json)* cover shared value behavior
- *(tokenizer)* lock qpdf all-mode parity
- *(content)* remove duplicate content lexer
- *(appearance)* consume qpdf content events
- *(content)* migrate core callback consumers
- *(tokenizer)* mark inline layer as qpdf mirror
- *(tokenizer)* keep core layer correspondence partial
- *(tokenizer)* mark qpdf component mirror
- *(parser)* route pulls through qpdf tokenizer
- *(tokenizer)* mirror qpdf token values
- classify flpdf modules by qpdf correspondence
- *(nntree)* cover repair boundary branches
- *(nntree)* consolidate outline destination lookup
- *(nntree)* cover repaired holder writeback
- *(page-labels)* cover malformed insert propagation
- *(nntree)* cover helper boundary paths
- *(nntree)* avoid duplicate direct-array clone
- *(nntree)* pin NUL repair to qpdf oracle
- *(nntree)* share qpdf string encoding
- *(nntree)* cover malformed indirect limits
- *(nntree)* clarify qpdf lookup and allocation parity
- *(nntree)* record malformed-array parity
- *(nntree)* annotate coverage mapping artifacts
- *(nntree)* close patch coverage gaps
- *(nntree)* cover defensive engine paths
- *(nntree)* complete engine parity gates
- *(nntree)* add shared key and node storage
- *(matrix)* centralize qpdf affine transforms
- *(pdf-version)* centralize version constants
- *(writer)* cover malformed version encryption floor
- route PDF versions through PdfVersion
- cover qpdf name escape recovery
- cover qpdf tokenizer edge states
- route lexical readers through tokenizer
- align real fixture with qpdf number tokens
- add qpdf-shaped object tokenizer
- *(writer)* preserve xref stream path coverage
- *(writer)* drop duplicated plain xref cases
- *(writer)* route generate through plain pipeline
- *(writer)* make preserve fallback assertion exhaustive
- *(writer)* align preserve assertions with plain plan
- *(writer)* pin preserve ObjStm source-number order
- *(writer)* route preserve through plain pipeline
- *(writer)* make disable placement assertion exhaustive
- *(writer)* cover plain body error propagation
- *(writer)* route disable through plain pipeline
- *(writer)* document planner coverage invariants
- *(writer)* cover plain plan invariants
- *(writer)* add logical plain write plan
- *(writer)* cover plain xref edge cases
- *(writer)* assemble xref from body layout
- *(writer)* exclude unreachable ObjStm wrap error arm
- *(writer)* extract physical serializers
- cover xref diagnostics through fallback
- Merge branch 'stack/flpdf-15jp-normal-object-routing' into stack/flpdf-15jp-container-xref-routing
- Merge branch 'stack/flpdf-15jp-stream-completion' into stack/flpdf-15jp-normal-object-routing
- require real endstream when requested
- require a stream recovery terminator
- Merge branch 'stack/flpdf-15jp-file-object-syntax' into stack/flpdf-15jp-stream-completion
- gate stacked layers with complete coverage
- add qpdf file-object syntax model

## [0.4.0](https://github.com/fulgur-rs/flpdf/compare/v0.3.0...v0.4.0) - 2026-07-24

### Fixed

- *(linearize)* drop stale live generations
- *(writer)* remap removed refs in generate trailer
- *(writer)* avoid cloning stream payloads in ObjStm walk
- *(writer)* address preserve review findings
- *(writer)* resolve qpdf signature key visibility
- *(writer)* isolate qpdf preserve eligibility
- *(writer)* constrain qpdf preserve object streams
- *(writer)* port qpdf null-aware standard enqueue
- *(linearize)* retain exact page users for objstm routing
- *(linearize)* address final object user review
- *(linearize)* preserve qpdf thumbnail user order
- preserve first-page ObjStm container order
- *(linearize)* match qpdf preserved ObjStm union routing
- match qpdf file object parsing

### Other

- Merge commit '95f37c93' into refactor/flpdf-9hc-41-linearize-null-walk
- Merge commit '9d1e73f9' into refactor/flpdf-9hc-41-linearize-null-walk
- *(linearize)* complete merged xref fixture
- Merge updated null traversal base into generate layer
- *(writer)* cover empty preserve ID path
- *(qpdf-null)* cover dictionary visibility helpers
- *(writer)* share qpdf null resolution
- *(linearize)* reuse object-user routing snapshot
- *(linearize)* fix routing snapshot documentation
- *(linearize)* retain object-user routing snapshot
- Merge pull request #521 from fulgur-rs/fix/flpdf-19ac-firstpage-objstm-order
- *(linearize)* cover thumbnail user routing
- *(linearize)* avoid private rustdoc link
- *(linearize)* annotate unreachable route invariant
- *(linearize)* cover qpdf container placement invariants
- *(linearize)* retain qpdf ObjStm container routes
- Match qpdf QDF ignore_newline semantics
- *(flpdf-10de)* pin indirect QDF contents array parity

## [0.3.0](https://github.com/fulgur-rs/flpdf/compare/v0.2.1...v0.3.0) - 2026-07-22

### Added

- *(flpdf-7nu4)* index outlines by destination page
- *(flpdf-x5yi)* support direct outline values
- *(flpdf-nm2o)* match qpdf outline destinations
- *(flpdf-nm2o)* add qpdf PDF string decoder
- *(flpdf-9hc.14.7)* deep outline walker with cycle detection and /Prev/Next diagnostic
- *(flpdf-9hc.14.8)* merge /PageLabels across all merge_documents inputs
- *(flpdf-9hc.14.8)* reconstruct /PageLabels in extract_pages
- *(flpdf-9hc.14.8)* reconstruct /PageLabels per --split-pages chunk
- *(flpdf-9hc.14.5)* outline /A action coverage (GoTo/GoToR/URI/Launch/Named)
- *(flpdf-9hc.14.6)* outline /SE structure-element link preservation and pruning
- *(flpdf-9hc.14.4)* /PageLabels number tree writer with rebalance
- *(flpdf-9hc.14.2)* read/write /Names /Dests name tree
- *(flpdf-9hc.14.1)* read/write catalog /Dests with page remap

### Fixed

- *(flpdf-9hc.38.2.1)* preserve qpdf repair state
- *(flpdf-9hc.38)* preserve qpdf repair state
- *(flpdf-9hc.38)* preserve incremental trailer refs
- *(flpdf-9hc.38)* discover trailer dangling refs
- *(flpdf-9hc.38)* include dangling refs in json metadata
- *(flpdf-9hc.38)* match dynamic qpdf json metadata
- *(flpdf-9hc.38)* build selected json sections lazily
- *(flpdf-9hc.38)* preserve json repair warnings
- *(flpdf-9hc.38)* match qpdf short name tree handling
- *(flpdf-9hc.38)* match qpdf name tree begin preflight
- *(flpdf-9hc.38)* match qpdf name tree search order
- *(flpdf-9hc.38)* complete malformed name tree repair
- *(flpdf-9hc.38)* repair malformed outline name trees
- *(flpdf-9hc.38)* close final outline parity gaps
- *(flpdf-9hc.38.2)* match qpdf outline JSON v2
- *(flpdf-7nu4.1)* normalize zero outline page bucket
- *(flpdf-guru)* match qpdf outline scalar accessors
- *(flpdf-3g9k)* remove unreachable depth guard
- *(flpdf-3g9k)* match qpdf outline depth boundary
- *(flpdf-x5yi)* stop outlines at resolved null
- *(flpdf-0hrl)* preserve document carrier boundaries
- *(flpdf-0hrl)* mirror qpdf page-boundary null-out in merges
- *(flpdf-0hrl)* null copied removed pages during extraction
- *(flpdf-nm2o)* preserve malformed qpdf destination keys
- *(flpdf-9hc.14.8)* qualify Self::labels_for_page_range in LabelRange doc
- *(flpdf-9hc.14.8)* page-label reconstruction bugs + qpdf-shaped write API
- *(flpdf-9hc.14.5)* resolve indirect /S in resolve_node_dest fallback
- *(flpdf-9hc.14.5)* borrow action in resolve_node_dest; non-destructive test helper
- *(flpdf-9hc.14.4)* use checked arithmetic + i64::try_from at usize boundaries
- *(flpdf-9hc.14.4)* merge redundant neighbor after insert_pages shift too
- *(flpdf-9hc.14.2)* qualify [ObjectRef] intra-doc link with crate::
- *(flpdf-9hc.14.2)* put cov:ignore markers on the flagged lines themselves

### Other

- *(flpdf-9hc.38)* cover name tree lookup shapes
- *(flpdf-9hc.38.1)* remove outline-specific policy
- *(flpdf-0hrl)* reuse selected page sets
- *(flpdf-0hrl)* clarify inline destination remap order
- *(flpdf-0hrl)* address merge null-out review
- *(flpdf-0hrl)* add generic page-boundary closure root
- *(flpdf-nm2o)* pin qpdf outline destination oracle
- *(flpdf-nm2o)* [**breaking**] remove typed outline actions
- *(flpdf-nm2o)* cover missing destination stores
- *(flpdf-nm2o)* cover qpdf UTF-16LE decoder
- *(flpdf-9hc.14.9)* correct rebuild_page_tree preservation scope
- *(flpdf-9hc.14.9)* scope outline/dest preservation claim per operation
- *(flpdf-9hc.14.9)* tighten fixture edge cases per iter-5 findings
- *(flpdf-9hc.14.9)* tighten test-helper doc comments per iter-4 findings
- *(flpdf-9hc.14.9)* drop unnecessary clones per iter-2 findings
- *(flpdf-9hc.14.9)* partially DRY test helpers per iter-1 findings
- *(flpdf-9hc.14.9)* outline + page-label round-trip and page-op e2e suite
- *(flpdf-9hc.14.7)* remove Drop on OutlineNode; cap walk depth at 5K
- *(flpdf-9hc.14.7)* explain why n.title/n.action need clone in tests
- *(flpdf-9hc.14.7)* cover non-dict-item branches in the new iterative walkers
- *(flpdf-9hc.14.8)* cov:ignore panic guards in to_dict prefix tests
- *(flpdf-9hc.14.8)* fix empty-primary label pollution + UTF-16BE prefix
- *(flpdf-9hc.14.8)* eliminate redundant /PageLabels tree parses (4 HIGH)
- *(flpdf-9hc.14.8)* apply iteration-4 defensive-check findings
- *(flpdf-9hc.14.8)* apply iteration-3 comment findings
- *(flpdf-9hc.14.8)* apply iteration-2 comment findings
- *(flpdf-9hc.14.8)* refine iteration-1 findings
- *(flpdf-9hc.14.8)* fix private-intra-doc-link warning
- *(flpdf-9hc.14.8)* mark defensive test-shape-guard panics cov:ignore
- *(flpdf-9hc.14.5)* 7 codex round-2 fixes on /A action typing
- *(flpdf-9hc.14.5)* apply iteration-5 comment refinements
- *(flpdf-9hc.14.5)* comment intent for borrow-then-move and stack-owned resolve
- *(flpdf-9hc.14.5)* apply roborev iteration-2 LOW findings
- *(flpdf-9hc.14.6)* read-then-write in walk_outline_se (medium)
- *(flpdf-9hc.14.6)* assert /SE round-trip target still resolves
- *(flpdf-9hc.14.6)* cover non-dictionary outline item in /SE prune walk
- *(flpdf-9hc.14.4)* insert_pages preserves surviving labels; remove_pages doc
- *(flpdf-9hc.14.4)* preserve default decimal labels in remove_pages fallback
- *(flpdf-9hc.14.4)* cover remove_pages fabricated-None and trailing-shift paths
- *(flpdf-9hc.14.4)* O(N) remove_pages + write_labels key dedup
- *(flpdf-9hc.14.4)* cover overflow branches from checked-arithmetic fix
- *(flpdf-9hc.14.2)* normalise name-tree dest page refs through holder
- *(flpdf-9hc.14.2)* cover check_name_tree_dests in-loop continue
- *(flpdf-9hc.14.2)* mirror check_legacy_dests short-circuit in check_name_tree_dests
- *(flpdf-9hc.14.2)* cover non-dict /Names fallback in name_tree_dests
- *(flpdf-9hc.14.2)* reader follows multi-hop /Names + unstable sort
- *(flpdf-9hc.14.2)* close patch-coverage gaps in the /Names /Dests writer
- *(flpdf-9hc.14.1)* resolve alias through chained /Dests dict
- *(flpdf-9hc.14.1)* document /Kids bare-ref holder chain limitation
- *(flpdf-9hc.14.1)* cover non-dict /Dests + holder-chain depth cap
- *(flpdf-9hc.14.1)* follow /Dests + page-ref holder chains
- *(flpdf-9hc.14.1)* cover in-loop continue when only some /Dests lack a page ref
- *(flpdf-9hc.14.1)* short-circuit check_legacy_dests when nothing to validate
- *(flpdf-9hc.14.1)* cover check_legacy_dests early-return branches

## [0.2.1](https://github.com/fulgur-rs/flpdf/compare/v0.2.0...v0.2.1) - 2026-07-19

### Added

- *(flpdf-4r6l.4)* port qpdf ResourceReplacer / adjustAppearanceStream
- *(flpdf-4r6l.3)* adjustDefaultAppearances (/DA Font-name rewrite)
- *(flpdf-4r6l.2)* mergeResources conflict rename + dr_map threading

### Fixed

- *(flpdf-4r6l.4)* lazily trigger the source /DR merge on the first field-bearing annot
- *(flpdf-4r6l.4)* freeze og_to_name snapshot lazily, not eagerly, in AP-stream re-merge
- *(flpdf-4r6l.4)* fix broken intra-doc link to a private sibling function
- *(flpdf-4r6l.4)* port qpdf's two-phase AP-stream rename + drop presence guard
- *(flpdf-4r6l.4)* update stream /Length on successful re-encode
- *(flpdf-4r6l.4)* fall back to FlateDecode when AP-stream re-encode fails
- *(flpdf-4r6l.4)* make AP-stream content decode/re-encode non-fatal
- *(flpdf-4r6l.4)* port qpdf's findEI 10-token lookahead for inline images
- *(flpdf-4r6l.4)* resolve indirect category sub-dicts + implement AP double-conflict rename
- *(flpdf-4r6l.4)* treat BI/ID/EI inline image data as opaque in resource_replacer
- *(flpdf-4r6l.4)* cov:ignore the last if-let closing brace
- *(flpdf-4r6l.4)* close patch-coverage gaps in the new AP-stream port
- *(flpdf-4r6l.3)* mark closing-brace instrumentation artifact as cov:ignore
- *(flpdf-4r6l.3)* clear stale DR renames before annotation-only placements
- *(flpdf-4r6l.3)* rewrite /Properties in /DA + reuse verbatim inserts
- *(flpdf-4r6l.3)* handle indirect /DA and rewrite non-Font operators
- *(flpdf-4r6l.3)* per-placement merge + escape /DA rename bytes
- *(flpdf-4r6l.3)* clear by_name at each merge start (stale-map leak)
- *(flpdf-4r6l.3)* source-ref-keyed dr_map reuse + resolve indirect /DR/Font
- *(flpdf-4r6l.3)* dest-scoped dr_map + reuse prior rename across placements
- *(flpdf-4r6l.3)* silence coverage on malformed-/DA match arm
- *(flpdf-4r6l.2)* shallow-copy indirect dest sub-dict into fresh ref
- *(flpdf-4r6l.2)* resolve indirect /DR resource-type refs in merge_resources_shallow

### Other

- *(flpdf-4r6l.4)* cover the qpdf-verified undecodable-filter+collision case
- *(flpdf-4r6l.4)* cover EI-followed-by-delimiter branch in inline-image scan
- *(flpdf-4r6l.4)* unignore existing-/DR overlay byte gate
- *(flpdf-4r6l.3)* assert on category() presence, not is_empty()
- *(flpdf-4r6l.3)* cover /DA operand-parse-error branch, tidy fixture loop
- *(flpdf-4r6l.3)* unit tests for adjust_default_appearance
- *(flpdf-4r6l.2)* drop intermediate src_types Vec in merge_resources_shallow
- *(flpdf-4r6l.2)* unit tests for DR merge conflict rename
- *(flpdf-4r6l.1)* cov:ignore the #[ignore]d Layer 4 byte gate body
- *(flpdf-4r6l.1)* ignored byte gate for AcroForm /DR merge collision

## [0.2.0](https://github.com/fulgur-rs/flpdf/compare/v0.1.10...v0.2.0) - 2026-07-18

### Added

- *(flpdf-9hc.34)* overlay copy-annotations byte-identical parity
- *(flpdf-9hc.34)* wire annotation copy through overlay pipeline
- *(flpdf-9hc.34)* survey_source_annotations + template_from_survey
- *(flpdf)* broaden /ADBE strip trigger to any /ADBE key (qpdf L1387 parity)

### Fixed

- *(flpdf-hdsz)* route incremental xref-stream trailer /ID through compact writer
- *(flpdf-hdsz)* route xref-stream trailer /ID through compact writer + review touch-ups
- *(flpdf-hdsz)* route incremental trailer /ID through qpdf-compact writer
- *(flpdf-hdsz)* drop array token-boundary rule; hand-roll trailer /ID like qpdf
- *(flpdf)* repair broken intra-doc links in spec_page_sources
- *(flpdf)* clippy fixes for CI Quality gate
- *(flpdf-9hc.34)* roborev review 2133 findings
- *(flpdf-9hc.34)* roborev review 2132 findings
- *(flpdf-9hc.34)* roborev review 2131 findings
- *(flpdf-9hc.34)* roborev review 2130 findings
- *(flpdf-9hc.34)* roborev review 2129 findings
- *(flpdf-9hc.34)* roborev review 2128 findings
- *(flpdf)* PageRange::resolve preserves duplicates (qpdf-parity)
- *(flpdf-cli)* emit `wrote file` per split chunk, not the never-written template
- *(flpdf)* preserve explicit newline_before_endstream=Yes under QDF; narrow No's EOL check to '\n' (PR #473 review)
- *(flpdf)* reduce clones in QDF /Contents pre-scan (PR #473 review)
- *(flpdf)* emit QDF %% Page N and %% Contents for page N markers (flpdf-9hc.16.13)

### Other

- *(flpdf)* tighten CLI byte-gate forward-pointer scope
- *(flpdf)* update overlay.rs deferral comment to point at CLI byte gate
- *(flpdf-hdsz)* materialize xref-stream regression assert message for patch-coverage
- *(flpdf-hdsz)* materialize assert-message renderings for patch-coverage
- *(flpdf-9hc.34)* cov:ignore-block for fully_qualified_name_of /Parent walk
- *(flpdf-9hc.34)* cov:ignore for defensive / malformed-input / llvm-cov artifact branches
- *(flpdf-9hc.34)* fixture for source with annots but no /AcroForm + simplify merge_resources_shallow
- *(flpdf-9hc.34)* copy-annotations byte gate for dest with indirect /Fields
- *(flpdf-9hc.34)* copy-annotations byte gate for source /P + inline annot
- *(flpdf-9hc.34)* copy-annotations byte gate for source /AcroForm with direct /DR
- *(flpdf-9hc.34)* copy-annotations byte gate for dest with existing /AcroForm
- *(flpdf-9hc.34)* copy-annotations byte gate for source /AcroForm /DA + /Q defaults
- *(flpdf-9hc.34)* underlay copy-annotations byte gate
- *(flpdf-9hc.34)* cov:ignore-block reader.rs linearized_hint_ref match guard
- *(flpdf-9hc.34)* cover remaining RealLiteral gaps and cov:ignore llvm-cov artifacts
- *(flpdf-9hc.34)* unit-test Object::RealLiteral match arms
- *(flpdf-9hc.34)* overlay_annotations module skeleton
- *(flpdf-9hc.34)* fixtures/goldens + place_form_xobject returns cm
- *(flpdf)* update adbe_ext_qpdf_parity module doc — INJECTION cases are now present
- *(flpdf)* byte-gate for /ADBE inject parity vs qpdf 11.9.0 (3 shapes)
- *(flpdf)* scope module doc to REMOVAL and drop redundant stem arg in assert_parity
- *(flpdf)* rename adbe_removal_qpdf_parity → adbe_ext_qpdf_parity + parametrise helpers
- *(flpdf)* byte-gate for /ADBE removal parity vs qpdf 11.9.0
- *(flpdf)* catalog_has_extensions_adbe uses resolve_borrowed + indirect-ref test
- *(flpdf)* failing test for /ADBE strip preserving non-ADBE prefix
- *(flpdf)* failing test for /ADBE strip when source lacks /ExtensionLevel
- warn about cargo-machete / cargo-udeps false positives
- document DEFLATE backend consumer-choice policy in README
- *(flpdf)* update stale QDF newline-promotion references (flpdf-9hc.16.13)
- *(flpdf)* overlay+QDF byte-gate for two-overlay declaration order (flpdf-9hc.16.13)
- *(flpdf)* overlay+underlay+QDF byte-gate same-page composition (flpdf-9hc.16.13)
- *(flpdf)* overlay+QDF byte-gate for single-page overlay (flpdf-9hc.16.13)

## [0.1.10](https://github.com/fulgur-rs/flpdf/compare/v0.1.9...v0.1.10) - 2026-07-10

### Added

- *(flpdf)* add overlay_verbose_report inspection API
- *(flpdf-cli)* default to newline-before-endstream=never (qpdf parity)
- *(flpdf)* inject Catalog /Extensions /ADBE when effective ext > 0
- *(flpdf)* add effective_pdf_version_and_ext pairwise helper
- *(flpdf)* add WriteOptions::min_extension_level
- *(flpdf)* promote adobe_extension_level to pub Pdf method

### Fixed

- *(flpdf)* restore Catalog dirty flag + resolve preexisting clippy
- *(flpdf)* fold encryption floor into version race + snapshot Catalog
- *(flpdf)* honor ObjStm floor + force_version in pairwise ext rule
- *(flpdf)* honor pairwise ext rule + inject ADBE before generate dispatch

### Other

- *(flpdf)* pass n_source into resolve_spec_pairs to avoid double page_refs walk
- *(flpdf)* cover overlay_verbose_report error propagation
- *(flpdf)* extract kind_stable_partition helper
- *(flpdf)* split resolve_spec_pairs out of spec_page_sources
- *(flpdf)* guard write_qdf public wrapper against Never-framing regression
- *(flpdf)* cover strip_adbe_extension edge cases + mark defensive branches
- *(flpdf)* drop redundant to_owned() on eff_ver in inject call site
- *(flpdf)* mark defensive branches in inject_adbe_extension cov:ignore
- *(flpdf)* drop stale flpdf-9hc.16.8 defer note in overlay byte_gate
- *(flpdf)* library byte gate for encrypted-source overlay ext_level
- *(flpdf)* library byte gate for pure version-floor overlay

## [0.1.9](https://github.com/fulgur-rs/flpdf/compare/v0.1.8...v0.1.9) - 2026-07-07

### Fixed

- *(linearize)* classify first-page /Thumb target as lc_first_page_shared (flpdf-hn1g.16)
- *(linearize)* gate generate-mode part7 container routing on others (flpdf-pn7h)

### Other

- *(linearize)* reuse live set + drop per-thumb own_set alloc (flpdf-hn1g.16)
- *(linearize)* thumb detection via accessor chain for patch-coverage (flpdf-hn1g.16)
- *(linearize)* byte-identical generate-mode thumb-firstpage-shared vs qpdf (flpdf-hn1g.16)
- *(linearize)* thumb-target-is-first-page-object classification tests (flpdf-hn1g.16)
- *(linearize)* pin generate-mode byte golden for otherpage-shared docother drift (flpdf-w0vu)
- Merge pull request #464 from fulgur-rs/fix/pn7h-generate-others-gate

## [0.1.8](https://github.com/fulgur-rs/flpdf/compare/v0.1.7...v0.1.8) - 2026-07-04

### Fixed

- *(linearize)* repair page tree unconditionally, matching qpdf 11.9.0 getAllPagesInternal (no reconstruction gate) (flpdf-s5i2)
- *(flpdf-jggp)* /Info is number-sorted lc_other, not a fixed part9-head slot

### Other

- Merge pull request #458 from fulgur-rs/test/flpdf-d8pc-rotate-inheritance-byte
- *(linearize)* add flpdf-s5i2 implementation plan; rustfmt test reflow
- *(linearize)* qpdf-oracle byte-identical golden for reconstructed shared-page input (flpdf-s5i2)
- *(linearize)* reconstructed interior /Type override + clean-input no-op regression (flpdf-s5i2)

### Added

* flpdf-9hc.9.9: `/Rotate` flattening — `flatten_rotation_on_pages` bakes a page's
  effective `/Rotate` into its content via a prepended `cm` matrix, transforms the
  page boxes (`/MediaBox`, `/CropBox`, `/BleedBox`, `/TrimBox`, `/ArtBox`) and
  annotation `/Rect` with the same matrix, and clears `/Rotate` to `0`. Visual
  rendering is unchanged. Caveat (held for review): annotation `/QuadPoints` and
  `/AP` `/Matrix` are not rotated, and output is not byte-identical to the source.

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

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

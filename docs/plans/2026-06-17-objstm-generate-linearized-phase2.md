<!-- For Claude: design/plan doc for flpdf-g6hb.2 (Phase 2). Keep this header. -->

# ObjStm `generate` — linearized (flpdf-g6hb.2, Phase 2)

Status: ground truth measured; implementation in progress.
Date: 2026-06-17. qpdf oracle: 11.9.0.

Phase 1 (non-linearized) is merged (PR #381). This doc covers the **linearized**
`--object-streams=generate` path and unblocks **flpdf-ihb.3**.

## Problem (one line)

`--linearize --object-streams=generate` diverges from qpdf at `>cap`: flpdf packs
per-part greedily (`canonicalise_first_half_batch` + `objstm_batches_generate`),
qpdf even-splits the GLOBAL eligible set then routes each container wholesale into
a linearization part by the UNION of its members' users.

## qpdf algorithm (verified against 11.9.0 source + measured output)

1. **Membership — `generateObjectStreams` (QPDFWriter.cc:1970-2006)** — same as
   Phase 1: `getCompressibleObjGens` DFS over the WHOLE doc; `n_streams =
   ceil(n/100)`; `n_per = ceil(n/n_streams)`; container ObjGen =
   `makeIndirectObject(null)` (allocated after all source objects, in even-split
   order → container0 ObjGen < container1 < …). Members of a container are a
   contiguous DFS slice. **DFS uses lexical key order** (`std::set<std::string>`),
   confirmed: `gen_mixed_shared 60 70` puts page1-only `/P1,/P10,/P11`
   (= source 66,75,76) — the first 3 in lexical order — into stream 0.

2. **Erase (linearized) — QPDFWriter.cc:2141-2161** — after the split, erase every
   page DICTIONARY from `object_to_object_stream`; for linearized||encrypted also
   erase the root Catalog. Those become uncompressed. (`/Pages` tree node and
   `/Info` are NOT erased — they stay ObjStm members.)

3. **Container → part = UNION of member users — `filterCompressedObjects`
   (QPDF_optimization.cc:340-380)** — compressed members are *replaced by their
   container ObjGen* in the obj_user maps, so the container inherits the union of
   all members' users. Then `calculateLinearizationData`
   (QPDF_linearization.cc:963-1200) categorizes the container by the standard
   `lc_*` rules (checked in this order):
   `is_root` → part4; `in_outlines` → part6/9; `in_open_document`
   (/Encrypt,/ViewerPreferences,/PageMode,/Threads,/OpenAction,/AcroForm) → part4;
   `in_first_page && others==0 && other_pages==0 && thumbs==0` → first_page_private
   (part6); `in_first_page` → first_page_shared (part6);
   `other_pages==1 && others==0 && thumbs==0` → other_page_private (part7);
   `other_pages>1` → other_page_shared (part8); thumbs → part9; else → part9.
   `others` is incremented by `ou_trailer_key` (non-/Encrypt, e.g. /Info) and
   `ou_root_key` (non-open-doc, non-/Outlines, e.g. /Pages) — NOT by page refs.

4. **Numbering — QPDFWriter.cc:2563-2655** —
   ```
   second half: [part7 ∪ part8 ∪ part9 UNCOMPRESSED, incl. containers, in part order]
                → second-half xref → [all those parts' MEMBERS]
   first half:  lindict → first-page xref → [part4 uncompressed] → [encrypt] → hint
                → [part6 uncompressed, incl. containers] → [part4+part6 MEMBERS]
   ```
   The container object IS one of the "uncompressed" objects (it has a real
   offset); members get the highest numbers of their half.

## Measured ground truth (fixtures in docs/plans/tools/)

* `gen_mixed_shared.py 60 70` (135 eligible, 2×68): containerA(stream0, part6) =
  {Pages,Info,shared 6-65,p1only 66,75,76} 65 members; containerB(stream1, part7) =
  {p1only rest} 67 members. Numbering: part7 page1/c1/containerB = new1/2/3,
  second xref new4, containerB members new5-71; part6 page0/c0/containerA =
  new76/77/78, members new79-143.
* `gen_three_page_shared.py 2 120` (128 eligible, 2×64): containerA(part6) =
  {Pages,Info,A1,A2(page0-private),G-fonts}; containerB(**part8**) = {G-fonts only,
  reach {1,2}} → `other_pages=2>1`. Confirms part8 routing and that the SAME
  G-fonts split across two containers land in different parts (6 vs 8) purely by
  their container's union category.

## Implementation approach (DECISION: faithful restructure)

Mirror qpdf: make ObjStm containers **first-class objects** with synthetic
ObjGens (= `max_source_objid + 1 + even_split_index`), inject them into the part
categorization via a member→container remap (the `filterCompressedObjects`
analogue), and let the existing renumber walk number them among the uncompressed
objects in part order. This makes the Finding-4 divergence disappear instead of
being patched.

* **Reuse flpdf's existing signals** (`first_page_set`, `all_referenced_pages`)
  for the union categorization — proven to reproduce all measured routings
  (containerA→part6, containerB→part7/part8). `/Pages` tree node and `/Info` are
  reach-0 (compute_closure does not add ancestor /Pages nor follow trailer), so a
  container is first-page iff any member ∈ first_page_set.
* **Finding 4 (renumber):** flpdf's current `place_objstm_members_per_half`
  numbers second-half containers AFTER the main xref; qpdf numbers them AMONG the
  uncompressed (part order) BEFORE the xref. The 3-page fixture does not
  discriminate (its part8 container is the last uncompressed object). A
  discriminating golden is required (see below).

## Advisor de-risk order: 2 → 1 → 4

* **(2) FIRST — validate the remap** into `from_pdf` categorization on
  `gen_mixed_shared` (reproduce part6/part7) before touching writer.rs / hints.
  Load-bearing; if awkward, the approach shifts.
* **(1) renumber discriminator — CONFIRMED** via
  `gen_part7_part8_discriminator.py 2 250 2` (260 eligible → 3 containers; a pure
  part7 container coexists with the shared Form XObject X, a part8 *uncompressed*
  stream). qpdf 11.9.0 second-half numbering: `Page1=1, c1=2, container3[part7]=3,
  Page2=4, c2=5, X[part8]=6, container7[part8]=7, xref=8`, members 9+ — the part7
  container is numbered BEFORE the part8 plain X and sits in page-1's private group
  (after c1, before Page2) by ObjGen. flpdf-current's
  `place_objstm_members_per_half` does `[all plain in part order][all containers]
  [xref][members]` → `…X=5, xref=6, container3=7, container7=8`, swapping them.
  The earlier 3-page fixture did not discriminate (its lone part8 container was the
  last uncompressed object). **Fix:** make containers first-class part-vector
  objects (synthetic ObjGen = `max_source_objid + 1 + even_split_index`) injected
  into `from_pdf` so each lands in its routed part at its ObjGen position (a part7
  container into its owning page's private group); `place_objstm_members_per_half`
  then only relocates the *members* (type-2) to after each half's xref. Net
  second-half = `[part7 incl. containers][part8 incl. containers + plain X][part9]
  → xref → members`, matching qpdf.
* **(4) hint tables in scope** — `hint_page.rs` page-0 nobjects now counts the
  container as one object; `hint_shared.rs` nshared shifts. ihb.3 IS this
  consistency. Not a follow-up.

## Progress (2026-06-17)

* **Stage A — global even-split membership** (commit 78bf8c1): `objstm_batches_generate`
  now = `objstm_membership_linearized` + `route_objstm_containers`. `objstm-lin-sharedfonts-100`
  (2 part6 containers, 50+51) is **strictly** byte-identical to qpdf (incl. deterministic
  `/ID[1]`) — end-to-end proof the membership+routing pipeline is sound. 6 existing
  linearize-objstm goldens unchanged.
* **Stage B.1 — page-private compression** (e1036b8): `resolve_batches` now drops only page
  DICTIONARIES, not all `part4_other_pages_private` (qpdf compresses page-private fonts).
* **Stage B.2 — second-half container numbering** (d0b505f): `place_objstm_members_per_half`
  numbers second-half containers before the main xref. `mixed`'s object numbering now matches
  qpdf exactly; residual is 2 bytes of layout.

### Next-session order (advisor-revised — supersedes the 2→1→4 ordering above)

1. **ihb.3 hint tables FIRST, then re-measure.** `mixed`/`threepage` residual = hint stream
   1 byte over-long (`/H [.. 133]` vs `132`) + 1 padding byte before the first-page xref
   (obj 73 @ 217 vs 216). Do NOT assume these are independent: qpdf's pass-1 lindict
   reserved-space estimate can include the hint-stream size, so fixing the hint-table count
   (page0 nobjects must count the part6 container; nshared shifts — `hint_page.rs`/`hint_shared.rs`)
   may also remove the padding byte. Fix hint table → regenerate `mixed` → only then chase any
   surviving padding byte.
2. **`part4_batches` must be PART-ordered, not even-split-ordered.** `objstm_batches_generate`
   currently pushes non-FirstPage containers in even-split (ObjGen) order; B.2 emits them in
   that order. qpdf places part7 → part8 → part9, each at its position among **all** second-half
   uncompressed objects (part rank, then page-group for part7, then ObjGen). `disc` passes the
   part-order check only coincidentally (stream1→part7, stream2→part8). The general fix —
   position each second-half container at its qpdf part-position — **subsumes both B.2's swap and
   `disc`'s interleaving**. Do not un-ignore `disc` until this lands.
3. **Regression-lock B.2:** B.2 has no green test (only `mixed`, ignored). Getting `mixed` green
   (step 1) locks the swap against silent CI-green refactors — higher priority than `disc`.

### Status update (2026-06-17): sf100 + mixed + threepage byte-identical; only `disc` left

Stages B.1–B.6 landed (`resolve_batches` page-dict-only filter; finding-4 swap; per-page
object-count + byte-length container folds; lindict obj-number-digit pad; Part-8 shared-hint split).
`disc` remains: flpdf emits second-half containers contiguously before the xref in batch order;
qpdf interleaves them by PART. **Critical:** a container goes at the END of its part-group
(qpdf's part vectors are ObjGen-sorted and the synthetic container ObjGen is the group's highest),
NOT at its first member's position. Verified: golden `disc` page-1 group is `[Page1, c1,
part7-container]` — `c1` precedes the container even though the B-font *members* precede `c1` in
closure order. Build the second-half order as `for page i: [plain privates of i] + [page-i part7
container]; then [part8 plain] + [part8 containers]; then [part9 plain] + [part9 containers]` — via
either synthetic-ref injection at the group ends (from_plan numbers them in place; members relocate
after the xref) or an explicit order computed in the writer. `mixed`/`threepage` are unaffected (a
lone second-half container at the group end equals today's append). `part4_batches` must also be
part-ordered (part7→part8→part9), not even-split order.
4. When un-ignoring, add the **strict** variants too (sf100 has both); the part7/8 ignored set
   must be surfaced in the PR body (ignored tests are invisible in green CI).
5. B.1 touched `resolve_batches` (shared with Preserve): a page-private font in a *source* ObjStm
   now stays compressed under Preserve (was dropped) — more correct, but add/verify a Preserve
   byte-parity check. (The existing Preserve suite stayed green, but has no fixture with that shape.)

## Explicit deviation (逸脱明示)

Container routing for `open_document` (/OpenAction,/AcroForm,/ViewerPreferences,
/PageMode,/Threads), /Outlines, and thumbnail members is NOT modeled: qpdf checks
`in_open_document`/`in_outlines`/thumbnail BEFORE `in_first_page`, so such a member
in stream 0 would route the whole container to part4/part9 overriding first-page.
flpdf's page-reach categorization (pre-existing limitation) does not model the
`others`/open-document/outline counters. These do not occur for the supported
corpus (fonts, no outlines/acroform/thumbnails); /Info,/Catalog,/Pages are
DFS-early and always land in stream 0's first-page container so they never
mis-route. Tracked for a faithful ObjUser port if the corpus expands.

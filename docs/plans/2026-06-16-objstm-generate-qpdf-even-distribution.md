<!-- For Claude: design/plan doc for flpdf-g6hb. Keep this header. -->

# ObjStm `generate`: mimic qpdf `generateObjectStreams` (flpdf-g6hb)

Status: investigation complete; implementation not started.
Date: 2026-06-16. qpdf oracle: 11.9.0.

## Problem (one line)

flpdf's `--object-streams=generate` does not reproduce qpdf's object-stream
layout once more than one container is needed; the divergence is structural
(distribution + renumbering), not just the cap.

## qpdf ground truth (verified against 11.9.0 source + observed output)

### Algorithm — `QPDFWriter::generateObjectStreams` (libqpdf/QPDFWriter.cc:1969-2006)

```
eligible = QPDF::getCompressibleObjGens(pdf)            // ORDERED, see below
n_object_streams = (eligible.size() + 99) / 100         // ceil(n/100)   :1981
if n_object_streams == 0 { return }
n_per = eligible.size() / n_object_streams
if n_per * n_object_streams < eligible.size() { n_per += 1 }   // ceil(n/n_streams)
n = 0
for og in eligible {                                    // :1991-2005
    if n % n_per == 0 { n = 0; cur = makeIndirectObject(null) }  // new container
    object_to_object_stream[og] = cur
    n += 1
}
```

Comment :1974 — "distribute objects approximately evenly without having any
object stream exceed 100 members." So objects are split **evenly**, never
greedy-100-then-spill.

### Co-location order — `QPDF::getCompressibleObjGens` (libqpdf/QPDF.cc:2392-2474)

A **DFS from the trailer/root**, traversing the pages tree. Children are pushed
in reverse-sorted-key order (so they pop ascending); array items pushed reversed
(pop in array order); indirect `/Length` omitted; streams, `/Sig` dicts and the
encryption dict are excluded from the result but streams are still traversed for
children. Object **numbers do not drive the order** — structure does.

### Within-stream order

`object_stream_to_objects` is `std::map<int, std::set<QPDFObjGen>>`
(QPDFWriter.hh:680); the write site iterates that set (QPDFWriter.cc:1066). A
`std::set<QPDFObjGen>` is ascending obj/gen, so **members serialize in ascending
object number** within a container.

### Renumbering (generate-mode only)

Plain `qpdf in out` (no ObjStm) does **not** renumber (measured: Root stays
`1 0 R`). Generating ObjStms **does** renumber: each container object is numbered
**immediately before its members**, members follow in stream order, the xref
stream is last. Measured on the 120-page fixture below:

```
obj 1   = ObjStm container #1   (holds 61 members -> objs 2..62; Catalog=2, Pages=3, ...)
obj 63  = ObjStm container #2   (holds 61 members -> objs 64..124)
obj 125 = XRef stream
Root: 1 0 R (source)  ->  2 0 R (output)
```

## Net model (load-bearing)

1. DFS traversal (getCompressibleObjGens) yields the eligible list in order.
2. Even split: `ceil(n/100)` streams, `n_per = ceil(n/streams)` consecutive each.
3. Renumber: per stream, container number then its members (in traversal order);
   non-eligible objects keep their place in the enqueue walk; xref stream last.
4. Within a container, serialize ascending by (new) object number.

`n <= 100` => one stream; that one-container shape is what every existing
(linearized) golden exercises (all <= 17 eligible).

## Empirical reproduction (the bug, and the oracle)

Fixture generator (commit alongside this doc):
`docs/plans/tools/gen_multipage.py` — emits an N-page classic-xref PDF; `reverse`
mode lists `/Kids` in descending object number so DFS order != numeric order
(needed to discriminate DFS grouping from number grouping in tests).

120-page fixture (122 eligible: Catalog + Pages + 120 page dicts), non-linearized
`--object-streams=generate --static-id`:

| | distribution | container objs | member numbering |
|---|---|---|---|
| qpdf 11.9.0 | **61 + 61** (even) | 1, 63 (before members) | renumbered, container-first |
| flpdf (HEAD) | **100 + 22** (greedy) | 123, 124 (after members) | source numbers preserved |

So flpdf's non-linearized generate diverges on all three axes, at this scale.

## Scope / phasing

This is larger than "swap `chunks(cap)` for even split". Recommended split:

- **Phase 1 — non-linearized generate** (`writer/object_streams.rs::plan_generate`
  + the generate renumber/writer path). Requires: getCompressibleObjGens DFS
  order, even split, container-interspersed renumbering, within-stream number
  sort. Fixture is easy (page dicts are eligible when not linearized; only erased
  at QPDFWriter.cc:2143-2148). **No existing non-linearized-generate golden — this
  establishes first parity coverage.**
- **Phase 2 — linearized generate** (`linearization/plan.rs::canonicalise_first_half_batch`
  + hint counters). qpdf assigns streams globally, erases page dicts/Catalog
  (QPDFWriter.cc:2141-2161), then partitions part6/part8. Unblocks flpdf-ihb.3
  (the stranded /Info+/Pages container is an artifact of flpdf's greedy first-half
  chunk; even split dissolves it). Needs a `>cap` first-page-shared fixture +
  qpdf ground truth (overlaps flpdf-6pcx).

Regression guard for both: the change must be a byte no-op on all 9 existing
`linearize-objstm` goldens (<= 17 eligible => single container).

## Model corrections (verified 2026-06-16 — these overrule looser statements above)

Two measurements refined the model; both matter for not writing wrong code:

### A. Within-stream numbering is by SOURCE object number, not traversal order

`object_stream_to_objects[stream]` is `std::set<QPDFObjGen>` keyed on the
**source** objgen (assignment runs before renumber). New numbers are assigned by
iterating that set, i.e. ascending source number. The getCompressibleObjGens DFS
order therefore only decides **which stream** an object lands in (the `n_per`
grouping when n > 100); it does **not** order members within a stream.

Consequence: for `n <= 100` (one stream) the DFS order is irrelevant — output is
fully determined by ascending source number. A discriminating fixture for the DFS
grouping **must be > 100 eligible**. (A reverse-/Kids fixture with n<=100 would
pass under pure source-number sorting and give false confidence.)

Verified: reverse-/Kids 5-page fixture (7 eligible, 1 stream) — qpdf numbers
members in ascending source order (src3->4 … src7->8), regardless of /Kids order.

### B. flpdf diverges from qpdf at EVERY scale, not just > cap

Even a 7-eligible single-stream file diverges:

| | container | members | Root |
|---|---|---|---|
| qpdf | obj 1 (FIRST) | renumbered 2-8, ascending source | 2 0 R |
| flpdf (HEAD) | obj 8 (LAST) | source numbers preserved 1-7 | 1 0 R |

So the core gap is qpdf's **generate-mode renumbering scheme** (container numbered
*before* its members; members renumbered consecutively in ascending source order;
xref stream last), which flpdf does not do at all. Even distribution (> 100) is a
refinement on top, not the main divergence. flpdf non-linearized generate has
never been byte-identical to qpdf.

## Phase 1 decomposition (revised)

- **Step A — generate-mode renumbering** (validate on a SMALL n<=100 fixture).
  Container-first numbering, members renumbered ascending-source after it, xref
  last. One container, so NO even-split / NO DFS needed yet. First RED:
  byte-parity (qpdf-zlib-compat) of `--object-streams=generate` on a ~7-object
  fixture == qpdf. This is the smallest byte-verifiable increment.
- **Step B — even distribution + DFS grouping for n > 100** (validate on a > 100
  discriminating fixture where DFS order != numeric order). Adds ceil/ceil n_per
  split and getCompressibleObjGens DFS to decide membership; within-stream stays
  ascending source number.

## Open question (Step A)

Exact interleaving of *non-eligible* (uncompressed) reachable objects in the
renumber sequence must be measured (the fixtures so far have only container +
members + xref). Add a fixture with an uncompressed-but-reachable stream and pin
where its new number lands relative to the container.

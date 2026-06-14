# qpdf compat decisions registry

This document tracks where flpdf deliberately deviates (or might deviate)
from qpdf's byte-for-byte output, and records the current policy decision
for each known deviation point.

Each entry corresponds to a `needs-review` subtask in the beads tracker
(epic `flpdf-9hc`); the entry ID matches the bd issue ID.

## Conventions

Decision states:
- **byte-identical**: flpdf must match qpdf bytes (no flexibility)
- **observable**: flpdf must match qpdf at a structural / semantic level
  (e.g. qpdf --json equivalence), bytes may differ
- **deferred**: decision pending; current behaviour is whatever flpdf
  emits today, recorded for future investigation
- **divergent**: flpdf deliberately differs from qpdf (rationale captured
  below)

When a subtask's decision is settled, update the entry from `deferred` to
the final state and add a short rationale.

## Entries

Grouped by area for navigation.

### Stream encoding

#### `flpdf-9hc.5.4` — ObjStm stream wrapping with FlateDecode

**Decision:** deferred  
**Owner:** unassigned  
**Rationale:** Wrapping ObjStm bodies with FlateDecode requires byte-identical zlib output to match qpdf; this is unlikely without vendoring qpdf's exact deflate variant. Decision between accepting observable equivalence (structurally valid ObjStm that qpdf can decode) vs. byte-identical compression is pending measurements.

#### `flpdf-9hc.7.2` — LZWEncode encoder

**Decision:** deferred  
**Owner:** unassigned  
**Rationale:** LZW encoding strategy (table reset cadence, code emission boundaries) is implementation-defined within the PDF spec; byte-identical output to qpdf is unlikely. Decision pending on whether to accept observable equivalence, mirror qpdf via a vendored encoder, or skip LZWEncode entirely and rely only on FlateDecode for new compression.

#### `flpdf-9hc.10.4` — Add attachment from disk (compressed stream bytes-compat)

**Decision:** deferred  
**Owner:** unassigned  
**Rationale:** Embedded file streams use FlateDecode compression; /CheckSum (MD5 of raw bytes) is deterministic but FlateDecode bytes are not. This is the same zlib parity question as `.5.4`; the decision for both should be made together. Observable equivalence (qpdf can list and extract the attachment) may be sufficient.

### CLI flag behaviour

#### `flpdf-9hc.12.2` — --normalize-content writer (whitespace/layout)

**Decision:** deferred  
**Owner:** unassigned  
**Rationale:** Re-emitting content streams in normalized form (one operator per line, consistent token formatting) must match qpdf's exact whitespace and token-emission policy for byte-identical output. Decision pending on whether observable equivalence (same operator sequence when parsed) is acceptable.

#### `flpdf-9hc.12.5` — --compress-streams=y/n (FlateDecode on/off)

**Decision:** deferred  
**Owner:** unassigned  
**Rationale:** The global FlateDecode compression toggle inherits the same zlib-parity problem as `.5.4`, `.7.2`, and `.10.4`. The policy decision applies project-wide; this entry cross-links to those issues and defers until the project-wide zlib parity policy is settled.

#### `flpdf-9hc.13.3` — --deterministic-id (MD5 over output bytes)

**Decision:** deferred  
**Owner:** unassigned  
**Rationale:** qpdf derives /ID from MD5 over output bytes; reproducing qpdf's exact deterministic /ID requires byte-identical output up to the trailer. flpdf's deterministic-id will be stable across runs but NOT equal to qpdf's value on the same input. Decision between shipping a self-stable deterministic /ID, attempting full byte parity, or explicitly documenting the divergence.

#### `flpdf-9hc.13.10` — linearized --deterministic-id /ID[1] (reproduce qpdf's first-pass digest)

**Decision:** byte parity achieved (reconstruct qpdf's first-pass digest buffer)
**Owner:** unassigned

**Context:** flpdf's linearized `rewrite --linearize --deterministic-id` output is **byte-identical to `qpdf 11.9.0 --linearize --deterministic-id`** on the stream-free fixture corpus (one/two/three-page), the changing identifier `/ID[1]` included. Reaching this required two things: aligning the classic linearized layout with qpdf (first-page object numbering — a page's `/Resources` subtree numbered ahead of its `/Contents`; physical placement — catalog emitted at the start of the first-page section, remaining body in ascending object number after `/E`; the Part-1 trailer `/ID` spacing; and recompressed-body `endstream` framing), **and** reproducing qpdf's deterministic-`/ID` seed exactly.

**How `/ID[1]` is reproduced:** qpdf computes the linearized deterministic ID during its **first write pass** and MD5-hashes that entire first-pass buffer through `%%EOF`, *before* the hint stream is written (`QPDFWriter::writeLinearized` → `computeDeterministicIDData`, qpdf 11.9.0). That pass-1 buffer differs from the final (pass-2) buffer only in length-preserving, reconstructible ways: the linearization parameter dict is emitted empty (`<< >>`, padded to the same region size so the first-page `xref` keyword still lands at its fixed offset), the primary hint stream object is **absent** (every object physically after it shifts down by the hint length), the first-page xref subsection carries formatted zero-offset entries (qpdf never back-patches it in pass 1), the first-page trailer `/Prev` is `0`, and every `/ID` array is all-zero. flpdf rebuilds that exact pass-1 buffer with one extra emission pass (`do_write_pass(.., pass1_digest = true)`; no convergence loop — pass 1 has no hint to converge), digests it (`md5` → `det_id_data`), forms the seed `det_id_data + " QPDF " + Σ(" " + Info_string_value)`, and `md5`s the seed to get `/ID[1]` (`/ID[0]` = source `/ID[0]` when present, else `/ID[1]`). The reconstructed pass-1 buffer is verified byte-for-byte against `qpdf --linearize-pass1` (md5 one-page `cdb6b8f1…`, two-page `15d4c05c…`, three-page `52c00da9…`).

**Scope:** the strict `/ID` byte parity applies to the **classic stream-free path** (`objstm_layout.is_empty()`), which is the corpus. When ObjStm / xref-stream output is requested, qpdf's first pass uses xref *streams* (a different pass-1 layout), so flpdf keeps the prior behaviour there: a self-stable deterministic `/ID` digested from its own final buffer. Byte parity for the ObjStm pass-1 layout is out of scope.

**Consequence for tests:** the strict byte-identity tests (`{one,two,three}_page_linearized_is_byte_identical_to_qpdf` in `crates/flpdf/tests/cmp_linearize_tests.rs`, gated on `qpdf-zlib-compat`) now PASS (un-`#[ignore]`d). The `*_structurally_byte_identical_to_qpdf` tests are retained as a narrower diagnostic (they mask the `/ID[1]` hex run before comparing, isolating a layout regression from an `/ID[1]`-digest regression). `tests/golden/compat-matrix.md` is unaffected: its `byte-equal` column reflects the default Pure-Rust build (miniz_oxide deflate, `/ID` elided from the fingerprint) and intentionally stays `diverge`; the `/ID` byte parity here is the feature-gated `qpdf-zlib-compat` + `--deterministic-id` combination, verified by `cmp_linearize_tests` rather than the matrix.

### Appearance generation

#### `flpdf-9hc.9.5` — Tx (text field) appearance stream renderer

**Decision:** deferred  
**Owner:** unassigned  
**Rationale:** Generating /AP/N streams for text fields involves token ordering, whitespace, and optional padding that differ from qpdf's exact instruction sequence. Byte-identical match is unlikely; decision pending on whether observable equivalence (field value renders correctly) is sufficient.

#### `flpdf-9hc.9.6` — Btn (checkbox/radio/pushbutton) appearance renderer

**Decision:** deferred  
**Owner:** unassigned  
**Rationale:** Button widget appearances (/AP/N + /AP/D for checked/unchecked states and captions) share the same byte-parity caveat as `.9.5`. Decision on byte-level vs. observable parity applies uniformly across all appearance renderers.

#### `flpdf-9hc.9.7` — Ch (combo/list) appearance renderer

**Decision:** deferred  
**Owner:** unassigned  
**Rationale:** Choice field appearances (selected value in combo, highlighted entries in list) share the same byte-parity caveat as `.9.5` and depend on that issue's decision. Observable equivalence (correct rendering of selection) is the working assumption.

### Annotation / form rendering

#### `flpdf-9hc.9.8` — Annotation flattening (Form XObject + Do op into page content)

**Decision:** deferred  
**Owner:** unassigned  
**Rationale:** Flattening annotations by inserting `q cm Do Q` into the page content stream introduces CTM precision and layout differences versus qpdf. Byte-identical output is unlikely; visual rendering equivalence is the practical target pending a formal decision.

#### `flpdf-9hc.9.9` — /Rotate flattening (CTM + box transform)

**Decision:** deferred  
**Owner:** unassigned  
**Rationale:** Clearing /Rotate by prepending a `cm` transform and rotating all page boxes involves floating-point values and content stream whitespace that will not match qpdf byte-for-byte. Decision consistent with `.9.8` and `.12.2` normalize policy.

#### `flpdf-9hc.16.3` — Destination content stream patching (q cm Do Q)

**Decision:** deferred  
**Owner:** unassigned  
**Rationale:** Overlay/underlay compositing appends or prepends `q cm Do Q` to destination page content; float formatting in the `cm` matrix and content stream whitespace will not match qpdf. Decision should be consistent with the `.12.2` normalize-content policy.

### JSON output

#### `flpdf-9hc.11.1` — JSON formatting policy (key order, indentation)

**Decision:** observable (structural equivalence)  
**Owner:** flpdf-9hc.11.1  
**Rationale:** Byte-identical JSON output requires reproducing qpdf's exact key-ordering policy and serializer quirks (2-space indent, fixed internal key order, trailing newlines). These are fragile to qpdf upstream changes and add no information value for consumers, since all real consumers route bytes through a JSON parser. Decision: pursue structural equivalence (same parsed JSON value) rather than byte-identical output. Concessions kept for maximum compatibility: keys emitted in qpdf's fixed v2 internal order where defined by downstream subtasks (the emitter preserves insertion order via `Vec<(String, JsonValue)>`); 2-space indent; LF line endings; UTF-8; trailing LF at end of file. Byte-identical output can be revisited as a separate issue if a downstream measurement shows it is required.

### Identifier generation

#### `flpdf-9hc.4.4` — File key generation (random vs deterministic)

**Decision:** deferred  
**Owner:** unassigned  
**Rationale:** For V=5 R=6 the file key is 32 independent random bytes; byte-identical output with qpdf is fundamentally not achievable except via fixed seeds. Decision pending on whether to use random keys + provide a `--deterministic-id` mode that matches qpdf's deterministic seed derivation, or always use random keys and forgo byte equality. Linked to `.13.3`.

#### `flpdf-9hc.13.2` — default /ID generation (random per save)

**Decision:** observable  
**Owner:** Mitsuru Hayasaka  
**Rationale:** The default trailer `/ID` is freshly randomized per save (ISO 32000-1 §14.4): element 1 preserved from a well-formed source `/ID` on re-save, element 2 always fresh. This matches qpdf's *observable* default behaviour (random `/ID`s differ between runs); exact byte parity with qpdf's `/ID` is inherently impossible because both tools randomize independently. Consequence for the byte-identical safety net: `tests/golden/compat-matrix.md`'s `flpdf-sha` fingerprint and `byte-equal` column are now computed with the trailer `/ID` array **elided** — fingerprinting it verbatim would be non-deterministic. This was an intentional golden re-bless; comparator verdicts are unchanged (still `32 diverge + 4 match`), and `plain` vs `static-id` fixtures now share a fingerprint, proving the only inter-mode difference is `/ID`. Deterministic / static `/ID` modes are tracked separately under `.13.3` (deferred) and `.13.4` (--static-id, byte-parity with qpdf's pi-digit constant, accepted). `tests/golden/baseline-static-id.md` is unaffected (no drift).

## AcroForm & annotation transforms (flpdf-9hc.9)

This section documents the deliberate divergences between flpdf and qpdf for
the appearance-generation and annotation-flattening transforms introduced in
subepic `flpdf-9hc.9` (subtasks .9.5 through .9.9).  The integration test
suite (`crates/flpdf-cli/tests/cli_acroform_transforms.rs`) verifies observable
equivalence for each of the categories below.

### Appearance streams (general) — `.9.5` / `.9.6` / `.9.7`

**Decision:** observable  
**Applies to:** `flpdf-9hc.9.5` (Tx renderer), `flpdf-9hc.9.6` (Btn renderer),
`flpdf-9hc.9.7` (Ch renderer)  
**Rationale:** Appearance-stream generation targets **observable equivalence**:
the generated `/AP/N` XObject must render the correct value/state in a standard
viewer, but byte-level or instruction-level identity with qpdf output is not a
goal.  Specific known divergences:

- **Token order and whitespace**: flpdf emits its own operator sequence
  (e.g. `BT Tf Td Tj ET`) with LF-separated lines; qpdf may produce a different
  ordering or spacing.
- **Auto-size heuristic**: font size is chosen by `(bbox_h − 2.0).clamp(4.0, 12.0)`
  for single-line fields — an approximation that matches common viewers but not
  qpdf's exact computation.
- **Vertical centering and quadding**: centering approximations may differ from
  qpdf's pixel-level layout.
- **ZapfDingbats glyph positioning** (Btn checkbox/radio): glyph advance-width
  approximation (`0.7 × em`) is used for centering; qpdf may use embedded metrics.

**needs-review caveat:** `.9.5`, `.9.6`, `.9.7` are tagged `needs-review`; the
observable-equivalence policy adopted here supersedes the prior `deferred` state
pending a formal sign-off.

### Btn widget limitations — `.9.6`

**Decision:** divergent (known limitation)  
**Applies to:** `flpdf-9hc.9.6`  
**Rationale:** The following qpdf features are **not implemented** in the Btn
appearance renderer:

- `/MK/BG` (background fill colour): not rendered.  The appearance background
  is transparent.
- `/MK/BC` (border colour): not rendered.  No border is drawn around the widget.
- ZapfDingbats defaults: checkbox on-state uses glyph `4` (✔, U+0034 in
  ZapfDingbats); radio on-state uses glyph `l` (●, U+006C).  These match the
  most common viewer defaults but may differ from qpdf if a document uses
  non-standard `/MK/CA` overrides that qpdf respects differently.

### Annotation flattening (content-stream layout) — `.9.8`

**Decision:** observable  
**Applies to:** `flpdf-9hc.9.8`  
**Rationale:** `--flatten-annotations` burns each annotation's `/AP/N` Form
XObject into the page content stream via `q {cm} Do Q`.  The resulting content
stream differs from qpdf in:

- **Matrix precision**: CTM values are formatted with up to 6 significant
  figures; qpdf may use fewer or more.
- **Content stream layout**: flpdf appends the `q cm Do Q` block with LF
  separators; qpdf's exact whitespace and line structure may differ.
- **XObject registration**: flpdf assigns a fresh name in `/Resources/XObject`;
  qpdf may reuse existing names or emit them in a different order.

The test suite verifies that the `Do` operator is present in the decoded page
content and that the annotation is absent from `/Annots` — visual equivalence.

**needs-review caveat:** `.9.8` is tagged `needs-review`; the observable policy
adopted here supersedes the prior `deferred` state.

### /Rotate flattening — `.9.9`

**Decision:** observable  
**Applies to:** `flpdf-9hc.9.9`  
**Rationale:** `--flatten-rotation` removes `/Rotate` by prepending a `cm`
rotation matrix to the page content and adjusting `/MediaBox` and other page
boxes.  Float formatting in the `cm` matrix and the rewritten box values will
differ from qpdf byte-for-byte.  See also existing entry `flpdf-9hc.9.9` above
(this entry cross-links for completeness).

### Plain-rewrite object renumbering (Catalog-first) — `.32`

**Decision:** byte-identical (intent) — **renumber order achieved; full byte
parity pending two follow-ups**  
**Applies to:** `flpdf-9hc.32`  
**Rationale:** The plain (non-linearized) full-rewrite path now renumbers output
objects in qpdf's Catalog-first breadth-first order (seed the trailer `/Root`
then remaining trailer indirect refs in sorted-key order; BFS descending into
dictionary keys in lexicographic order and arrays in element order;
first-encounter-wins; new numbers `1..=N`). This matches `qpdf --static-id`
byte-for-byte for **all non-stream objects** (Catalog, Info, Pages, Page
objects) and reproduces qpdf's exact object order and trailer
(`/Info 2 0 R /Root 1 0 R`).

**Behavior changes (qpdf-consistent):**
- **Unreferenced objects are dropped.** The renumber numbers only objects
  reachable from the trailer seed; unreachable ("orphan") objects are not
  emitted, matching qpdf's default (no `--preserve-unreferenced`) and flpdf's
  qdf/disable paths. ObjStm batches are filtered to reachable members so
  generate/preserve modes drop orphans rather than failing.
- **qdf `%% Original object ID: A`** now records the *input* number `A` while the
  object header carries the renumbered *output* number `N` (`A != N` in general),
  matching `qpdf --qdf`.

**Full byte-identity is NOT yet reached.** Two further divergences remain,
tracked as siblings under `flpdf-9hc.20`:
- `flpdf-tqu1` — stream dictionaries must serialize with `/Length` first (qpdf
  order); flpdf currently emits lexicographic key order.
- `flpdf-onao` — deflate output parity. Standard zlib level 6 (the default)
  reproduces qpdf's bytes exactly; available via the existing `qpdf-zlib-compat`
  feature (classic libz), pinned to the linked libz version. The default
  `miniz_oxide` backend diverges.

Because `byte-equal` is all-or-nothing, the `tests/golden/compat-matrix.md`
`byte-equal`/`qpdf-json` verdicts stay `diverge` until both land; only the
`flpdf-sha` fingerprints were re-blessed for the renumber, and
`structural=match` is preserved for all static-id rows.

## Cross-references

- Subepic `flpdf-9hc.20`: bytes-identical roadmap (this registry is the
  consolidation point for that epic's writer + harness work).
- The compat matrix at `tests/golden/compat-matrix.md` is the empirical
  baseline for current divergences.

## Stale-entry check (manual)

To verify the registry covers every current `needs-review` subtask:

```shell
bd list --label=needs-review
```

Compare the IDs with this document's `#### \`<id>\`` headers. New
`needs-review` subtasks must be added here within the PR that introduces
them. Full CI automation of this check is tracked separately.

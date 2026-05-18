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

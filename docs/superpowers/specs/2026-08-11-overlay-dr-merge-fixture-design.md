# Overlay `/DR` Merge Differential Fixture Design

## Goal

Add a small, committed destination fixture that reaches qpdf 11.9.0's
`QPDFObjectHandle::mergeResources` collision-name scope which is not covered
by the existing overlay fixtures. Record the observed qpdf/flpdf result in the
existing `qpdf-zlib-compat` overlay byte-gate module.

## Discriminating shape

The destination `/AcroForm/DR/Font` remains a direct dictionary with:

- `/F0` pointing to an indirect dictionary whose keys do not include
  `/F1_1`;
- `/F1` pointing to the destination Helvetica object;
- an existing `/F1_1` pointing to the same destination object.

The overlay source is the existing
`form-fields-and-annotations.pdf`, whose `/DR/Font/F1` points to a distinct
Courier object. qpdf's `getResourceNames()` sees only the nested dictionary's
keys, so it chooses `/F1_1` and overwrites the pre-existing hidden key.
flpdf's `unique_dr_name()` scans the direct category keys, so it chooses
`/F1_2` and preserves `/F1_1`.

## Test flow

The library byte-gate test will use the existing overlay helpers and writer
recipe, compare against a qpdf 11.9.0 QDF golden, and assert the known
divergence explicitly. The golden will be generated with:

```text
qpdf --qdf --static-id --no-original-object-ids --min-version=1.6 \
  tests/fixtures/compat/overlay-dr-merge-hidden-collision.pdf \
  --overlay tests/fixtures/compat/form-fields-and-annotations.pdf \
  --repeat=1 -- tests/golden/references/overlay/overlay-dr-merge-hidden-collision.pdf
```

The test will additionally assert that qpdf's copied field `/DA` uses
`/F1_1`, while current flpdf output uses `/F1_2`. This makes the expected
divergence reviewable and prevents the fixture from silently becoming
irrelevant.

Existing gates continue to cover the neighboring paths: source without
`/AcroForm`, direct source `/DR`, ordinary direct-key collision, and indirect
resource sub-dictionaries. No production algorithm or documentation comment
is changed in this issue; reconciling the new divergence belongs to a
follow-up.

## Acceptance mapping

- The committed fixture contains the indirect nested dictionary and hidden
  collision key.
- The qpdf golden is generated from pinned qpdf 11.9.0 and checked by the
  feature-gated overlay test.
- The test records a confirmed byte divergence and its resource-name cause.
- The existing neighboring gates remain unchanged and must stay green.

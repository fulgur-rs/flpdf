# Design — flpdf-25kg.6.20: keep page-1-private optimization mints before ObjStm

## Goal

Make `--linearize --object-streams=generate` match qpdf 11.9.0 when optimization
promotes a non-scalar inherited page attribute (for example a direct `/MediaBox`
array) to a fresh indirect object that is private to the first page.

## Oracle result

The pinned qpdf 11.9.0 source establishes this order:

1. `QPDFWriter::doWriteSetup` calls `generateObjectStreams` before
   `writeLinearized`, and `writeLinearized` calls `QPDF::optimize` afterward
   (`QPDFWriter.cc:2059-2160, 2537-2557`).
2. `QPDF::getCompressibleObjGens` records only indirect candidates
   (`QPDF.cc:2393-2445`). A direct non-scalar inherited value is not in that
   set.
3. `pushInheritedAttributesToPage` promotes that value with
   `makeIndirectObject` (`QPDF_optimization.cc:159-202`).
4. `calculateLinearizationData` classifies the minted object as
   `lc_first_page_private` when its only object user is page 0, and emits
   first-page private objects before first-page shared objects
   (`QPDF_linearization.cc:1031-1149, 1150-1222`).

The generated ObjStm placeholder is allocated before optimization, but qpdf's
linearization renumbering places the newly minted first-page-private plain
object in the first-page plain sequence before the corresponding ObjStm
container. A live one-page fixture with an inherited direct `/MediaBox`, two
page-private annotation dictionaries, and a font confirms qpdf emits the plain
minted array before the container. Current flpdf emits the same array after the
container because `first_half_post_plain` includes all post-optimization
`part2_objects`.

## Scope and boundary

- Keep the pre-optimization Generate eligibility snapshot unchanged; it already
  correctly excludes the newly minted array from ObjStm membership.
- Keep `objstm_membership_linearized_with_eligibility` unchanged; the live
  output confirms the array is plain in current flpdf and only its placement is
  wrong.
- Change only the first-half post-container classification so post-optimization
  `part2_objects` remain in the ordinary first-half sequence. Retain the
  existing post-container handling for `part3_objects`, open-document plain
  objects, and outline objects until each has its own qpdf oracle.
- Do not change the deferred canonical-prefix naming cleanup.

## Test oracle

Add a small valid classic-xref fixture and a qpdf 11.9.0
`linearize-objstm.pdf` golden. The qpdf-zlib compatibility test must assert
byte identity. The fixture isolates the discriminator: the inherited array is
first-page-private, while the annotation dictionaries and font supply the
first-half ObjStm container. Existing first-page shared/minted tests remain
regression controls for the retained post-container path.

## Acceptance criteria

1. The new Generate linearization test is RED before the production change and
   GREEN afterward.
2. The new fixture output is byte-identical to the committed qpdf 11.9.0
   golden under `qpdf-zlib-compat`, including object numbering, ObjStm
   membership, xref stream, hint stream, and `/ID[1]`.
3. `qpdf --check-linearization` accepts the qpdf golden and the flpdf output.
4. Existing linearization ObjStm tests, especially first-page private/shared
   ordering and post-optimization shared-object hint ordering, remain green.
5. No Generate eligibility, object-user classification, or second-half routing
   is broadened as part of this fix.

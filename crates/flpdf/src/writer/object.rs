//! qpdf correspondence: `QPDFWriter::unparseObject`, `unparseChild`, `writeTrailer`, and the writer-owned live-handle emission boundary.
//!
//! qpdf sources: `libqpdf/QPDFWriter.cc:1072-1810,2236-2376,2907-3035`.
//!
//! The object graph remains owned by [`crate::ObjectHandle`].  This module
//! owns traversal, output-reference remapping, null visibility, QDF framing,
//! and emission-time string policy.  Keeping this boundary in `writer/`
//! prevents the object model from growing a second writer responsibility.

use crate::object_handle::{legacy_dictionary_key, ObjectHandle, ObjectValue};
use crate::{Error, ObjectRef, Result};
use std::collections::BTreeSet;

/// The single writer-owned emission surface for live `ObjectHandle` values.
///
/// This is deliberately crate-private: it is the writer-owned replacement for
/// the removed handle-owned emission route. All implementations and callers
/// live at the writer boundary, while the handle itself retains only graph
/// identity, payload, and mutation responsibilities.
pub(crate) trait ObjectWriterEmission {
    fn write_object(&self, out: &mut Vec<u8>) -> Result<()>;
    #[cfg(test)]
    fn write_object_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>;
    #[cfg(test)]
    fn write_object_qdf(&self, out: &mut Vec<u8>, indent: usize) -> Result<()>;
    fn write_object_qdf_with_ref_map_and_removed(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()>;
    #[cfg(test)]
    fn write_object_qdf_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>;
    fn write_object_with_ref_map_and_removed(
        &self,
        out: &mut Vec<u8>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()>;
    fn write_root_object_with_ref_map_and_removed(
        &self,
        out: &mut Vec<u8>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        final_pdf_version: &str,
        final_extension_level: i64,
    ) -> Result<()>;
    fn write_object_with_ref_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>;
    fn write_object_qdf_with_ref_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>;

    fn write_stream_body(&self, out: &mut Vec<u8>, refiltered: bool) -> Result<()>;
    #[cfg(test)]
    fn write_stream_body_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>;
    #[cfg(test)]
    fn write_stream_body_qdf(&self, out: &mut Vec<u8>, indent: usize) -> Result<()>;
    fn write_stream_body_qdf_with_ref_map_and_removed_and_length(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length_ref: Option<ObjectRef>,
    ) -> Result<()>;
    fn write_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length_ref: Option<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>;
    #[cfg(test)]
    fn write_stream_body_qdf_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>;
    fn write_stream_body_with_ref_map_and_removed(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()>;
    /// Stream-dictionary emission with a direct output `/Length` override.
    /// The source handle remains unchanged; this mirrors qpdf's stream writer,
    /// which computes the emitted length from the bytes supplied to its pipe.
    fn write_stream_body_with_ref_map_and_removed_and_length(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length: usize,
    ) -> Result<()>;
    fn write_stream_body_with_ref_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>;

    #[cfg(test)]
    fn write_trailer(
        &self,
        out: &mut Vec<u8>,
        xref_stream: bool,
        id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
    ) -> Result<()>;
    #[allow(clippy::too_many_arguments)]
    fn write_trailer_with_ref_map(
        &self,
        out: &mut Vec<u8>,
        xref_stream: bool,
        qdf: bool,
        id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        suppress_null_values: bool,
    ) -> Result<()>;
    #[cfg(test)]
    fn write_dictionary_with_ref_map_and_id_writer(
        &self,
        out: &mut Vec<u8>,
        id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        suppress_null_values: bool,
    ) -> Result<()>;
    fn write_id_value_with_ref_map(
        &self,
        out: &mut Vec<u8>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()>;
}

const UNPARSE_STACK_RED_ZONE: usize = 32 * 1024;
const UNPARSE_STACK_GROWTH_SIZE: usize = 1024 * 1024;

fn reserved_unparse_error() -> Error {
    Error::System("QPDFObjectHandle: attempting to unparse a reserved object".to_owned())
}

fn unresolved_unparse_error() -> Error {
    Error::Internal("attempted to unparse an unresolved QPDFObjectHandle".to_owned())
}

fn destroyed_unparse_error() -> Error {
    Error::Internal("attempted to unparse a QPDFObjectHandle from a destroyed QPDF".to_owned())
}

fn write_dictionary_key(out: &mut Vec<u8>, key: &[u8]) {
    if key.starts_with(b"/") {
        out.push(b'/');
        crate::pdf_syntax::write_name_escaped(out, legacy_dictionary_key(key));
    } else {
        // QPDF_Name::normalizeName preserves the first byte of a raw qpdf
        // dictionary key (`libqpdf/QPDF_Name.cc:27-50`). In particular,
        // `replaceKey("Array1", ...)` is intentionally emitted as the
        // slashless token `Array1`; do not silently canonicalize it here.
        crate::pdf_syntax::write_name_escaped(out, key);
    }
}

impl ObjectWriterEmission for ObjectHandle {
    /// This handle's plain (non-QDF) writer-emission form
    /// (`QPDFWriter::unparseObject`, `QPDFWriter.cc:1318-1527`, called with
    /// `level=0, flags=0`). Distinct from [`Self::unparse`]/
    /// [`Self::unparse_resolved`], which port a different qpdf function
    /// (`QPDFObjectHandle::unparse`) with a different contract — do not
    /// conflate the two. Forces resolution of `self` (mirroring qpdf's own
    /// implicit `dereference()` on `object`'s first `isXxx()` type check
    /// inside `unparseObject` itself) and of every indirect dictionary
    /// entry reached along the way, to apply qpdf's null-valued-key
    /// suppression rule (`:1490-1491`); an indirect entry that survives
    /// suppression writes as its own `"N G R"` reference form, never
    /// inlined.
    ///
    /// If `self` is an *indirect* handle whose resolved value is a `Stream`,
    /// this call reaches `unparse_object_value`'s `Stream` arm directly (it
    /// does not go through [`write_child`]'s indirect-reference check the
    /// way a *child* position would) and inlines just the stream's
    /// dictionary — `<< ... >>` with no `stream`/`endstream` framing and no
    /// `/Length`-last repositioning. That is not what qpdf's real
    /// stream-writing call produces at this position; this primitive simply
    /// does not implement qpdf's stream-writing path
    /// (`QPDFWriter::unparseObject` entered with `f_stream` flags). The
    /// dedicated primitive for that is `write_stream_body`, which current
    /// writer routes call when stream framing is required; calling
    /// `write_object` directly on a stream-resolving handle
    /// is an underspecified, undocumented-by-qpdf shape whose current output
    /// is pinned, in `unparse_object_tests`, by
    /// `unparse_object_on_an_indirect_handle_resolving_to_a_stream_inlines_the_dictionary`
    /// rather than derived from any qpdf oracle.
    fn write_object(&self, out: &mut Vec<u8>) -> Result<()> {
        unparse_object_walk(self, out)
    }

    /// Writer-emission counterpart of [`Self::write_object`] that routes
    /// every ordinary direct PDF string through `write_string`. The qpdf
    /// signature `/Contents` exception remains cleartext hexadecimal because
    /// `QPDFWriter.cc:1501` supplies `f_hex_string | f_no_encryption`; it is
    /// therefore intentionally not sent to the callback. This is the
    /// emission-time hook used by qpdf's encrypted `unparseObject` branch
    /// (`QPDFWriter.cc:1567-1599`): containers and indirect child identity
    /// remain owned by `ObjectHandle`, while the caller supplies only the
    /// string representation policy.
    #[cfg(test)]
    fn write_object_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
    {
        unparse_object_walk_with_string_writer(self, out, write_string)
    }

    /// QDF-mode counterpart of [`Self::write_object`] — same qpdf function
    /// and the same call shape (`QPDFWriter::unparseObject`,
    /// `QPDFWriter.cc:1318-1527`, `level=0, flags=0`), but with the writer's
    /// own `m->qdf_mode` member set to `true` rather than `false` — a mode
    /// flag `unparseObject` checks internally, not an alternate set of call
    /// arguments. Carries forward this port's existing split between compact
    /// and QDF container framing rather than re-deriving the indent
    /// arithmetic from scratch: `indent` is the column (number of leading
    /// spaces) at which *this* value's own opening delimiter sits, an array
    /// or dictionary's children are written at `indent + 2`, and its closing
    /// delimiter (`]` / `>>`) returns to column `indent` on its own line —
    /// exactly the established qpdf-shaped contract. Every scalar (including
    /// a resolved indirect handle) writes byte-identically to the non-QDF form; only array,
    /// dictionary, and stream-dictionary-inlining framing differ.
    ///
    /// Applies the exact same null-suppression rule as [`Self::write_object`]
    /// (dictionary entries only — `QPDFWriter.cc:1490-1491`; an array keeps
    /// null elements verbatim, `QPDF_Array::unparse` has no such rule) via
    /// the same [`visible_dict_entries`] helper, and the same forced
    /// top-level resolution of `self` before dispatch. See
    /// [`Self::write_object`]'s own doc for the identical
    /// indirect-handle-resolving-to-a-`Stream` caveat: this call dispatches
    /// on `self` directly, bypassing the child-position reference check, so
    /// it inlines just the dictionary rather than implementing qpdf's real
    /// stream-writing framing. The dedicated primitive for *this* (QDF-mode)
    /// shape is [`Self::write_stream_body_qdf`] -- not
    /// [`Self::write_stream_body`], which has no `indent` parameter and
    /// only ever produces the compact single-line form; that one is the
    /// dedicated primitive for [`Self::write_object`]'s own (non-QDF)
    /// identical caveat instead. Do not conflate the two when fixing this
    /// shape at a real call site.
    #[cfg(test)]
    fn write_object_qdf(&self, out: &mut Vec<u8>, indent: usize) -> Result<()> {
        unparse_object_walk_qdf(self, indent, out)
    }

    /// QDF writer emission with output-reference remapping and qpdf null
    /// visibility for references removed during this write.
    fn write_object_qdf_with_ref_map_and_removed(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()> {
        unparse_object_walk_qdf_with_ref_map(self, indent, out, map, removed_refs)
    }

    /// QDF-mode counterpart of [`Self::write_object_with_string_writer`],
    /// including its cleartext hexadecimal signature `/Contents` exception.
    #[cfg(test)]
    fn write_object_qdf_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
    {
        unparse_object_walk_qdf_with_string_writer(self, indent, out, write_string)
    }

    /// Writer-emission counterpart that additionally treats references in
    /// `removed_refs` as qpdf nulls. This is the canonical equivalent of
    /// `renumber_qpdf_refs_in_place_with_removed` for live handle graphs: an
    /// array keeps the position as `null`, while dictionary visibility drops
    /// the null-valued key.
    fn write_object_with_ref_map_and_removed(
        &self,
        out: &mut Vec<u8>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()> {
        unparse_object_walk_with_ref_map(self, out, map, removed_refs)
    }

    /// Emit the root through qpdf's output-only `unparseObject` mutation.
    ///
    /// qpdf makes an unsafe shallow copy of the root dictionary before
    /// reconciling `/Extensions /ADBE` (`QPDFWriter.cc:1347-1435`). Keep that
    /// copy local to serialization. Existing direct Extensions remain shared:
    /// replacing or removing ADBE there also changes the live graph. Creating
    /// or removing the root's Extensions key changes only the output copy.
    fn write_root_object_with_ref_map_and_removed(
        &self,
        out: &mut Vec<u8>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        final_pdf_version: &str,
        final_extension_level: i64,
    ) -> Result<()> {
        let root = root_output_copy_with_adbe(self, final_pdf_version, final_extension_level)?;
        unparse_object_walk_with_ref_map(&root, out, map, removed_refs)
    }

    /// Encrypted writer counterpart of
    /// [`Self::write_object_with_ref_map_and_removed`]. Reference identity,
    /// qpdf null visibility, and string encryption are all applied while the
    /// live handle graph is walked.
    fn write_object_with_ref_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
    {
        unparse_object_walk_with_ref_map_and_string_writer(
            self,
            out,
            map,
            removed_refs,
            write_string,
        )
    }

    /// QDF/encrypted counterpart of
    /// [`Self::write_object_with_ref_map_and_removed_with_string_writer`].
    fn write_object_qdf_with_ref_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
    {
        unparse_object_walk_qdf_with_ref_map_and_string_writer(
            self,
            indent,
            out,
            map,
            removed_refs,
            write_string,
        )
    }
    /// This stream-dictionary handle's writer-emission form, matching
    /// `Dictionary::write_pdf_stream`'s established layout (`object.rs`)
    /// -- the `/Length`-last, optionally re-filtered
    /// stream-dictionary shape `QPDFWriter::unparseObject`'s stream branch
    /// produces when it delegates to its own dictionary branch
    /// (`QPDFWriter.cc:1440-1442` enters with `flags |= f_stream`;
    /// `1451-1455`, only when `refiltered`, drops `/Filter`/`/DecodeParms`;
    /// `1488-1527` is the dictionary-branch loop that writes the surviving
    /// keys, `/Length`, and, when `refiltered`, a fresh `/Filter
    /// /FlateDecode`) -- plus the same null-suppression rule as
    /// [`Self::write_object`], since this delegation target is the
    /// identical dictionary branch.
    ///
    /// Like `write_pdf_stream` itself, this primitive does not replicate
    /// every qpdf step in that line range: the unconditional
    /// empty-`/DecodeParms`-array removal (`1444-1449`), the
    /// `/Crypt`-filter stripping in the non-refiltered branch
    /// (`1456-1485`), qpdf's `compress && (flags & f_filtered)` gate on the
    /// trailing `/Filter /FlateDecode` append (`1519`, driven by
    /// `refiltered` alone here), and qpdf's own computed `/Length` *value*
    /// (`1508-1518`: `stream_length`/`cur_stream_length_id`, not the
    /// dictionary's own stored value) are all out of scope -- inherited
    /// unchanged from `write_pdf_stream`'s own established simplifications
    /// (see that function's doc for the full qpdf-correspondence caveat).
    ///
    /// `self` normally resolves to a `Dictionary` directly -- this
    /// primitive's usual caller already holds an already-resolved stream's
    /// dictionary handle (see below). It also accepts `self` resolving to a
    /// `Stream { stream_dict, .. }`, the same shape [`Self::write_object`]'s
    /// own `Stream` arm accepts when an indirect handle resolves to a stream
    /// (see that primitive's own doc for why this shape is reachable): in
    /// that case `stream_dict` -- itself an [`ObjectHandle`], not
    /// necessarily already resolved -- is forced to resolve (propagating any
    /// error, e.g. a dropped document, the same way the top-level `self`
    /// resolution below does; see `unparse_stream_body_resolves_an_unresolved_indirect_stream_dict`
    /// and `unparse_stream_body_propagates_a_dropped_document_error_from_stream_dict`,
    /// which fail without this call) and its entries are used exactly as if
    /// `self` had been that dictionary handle to begin with. Any other
    /// resolved shape for `self`, or a `stream_dict` that itself resolves to
    /// something other than a `Dictionary`, degrades to an empty `<< >>`,
    /// mirroring `write_pdf_stream`'s own typed-input assumption (this
    /// crate's writer never calls it on anything else).
    ///
    /// Forces resolution of `self` before dispatch, the same as
    /// [`Self::write_object`]'s own top-level entry point -- this primitive's
    /// usual caller already
    /// holds an already-resolved stream's dictionary handle, but nothing
    /// enforces that at the type level, and an as-yet-unresolved indirect
    /// handle whose document has been dropped must surface as an error
    /// here too, not silently degrade to an empty `<< >>` the way an
    /// unresolved [`Self::with_value`] read alone would (see
    /// `unparse_stream_body_propagates_a_dropped_document_error`, which
    /// fails without this call).
    fn write_stream_body(&self, out: &mut Vec<u8>, refiltered: bool) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        self.try_dereference()?;
        self.with_value(|value| {
            let entries = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                Some(ObjectValue::Stream { stream_dict, .. }) => {
                    // `stream_dict` is itself an `ObjectHandle` that may not
                    // yet be resolved (e.g. a mock-resolver-bearing indirect
                    // handle whose value is a `Stream` wrapping another
                    // indirect dictionary handle) -- force its own
                    // resolution, mirroring the `self.try_dereference()?`
                    // above, before reading its value.
                    stream_dict.try_dereference()?;
                    stream_dict.with_value(|dict_value| match dict_value {
                        Some(ObjectValue::Dictionary(entries)) => entries
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                        _ => Vec::new(),
                    })
                }
                _ => Vec::new(),
            };
            unparse_stream_dict_entries(&entries, refiltered, out)
        })
    }

    /// Stream-dictionary counterpart of
    /// [`Self::write_stream_body`] that routes ordinary direct PDF strings
    /// through `write_string` while retaining qpdf's `/Length` and refilter
    /// ordering. A signature `/Contents` value remains cleartext hexadecimal,
    /// matching qpdf's `f_hex_string | f_no_encryption` flags.
    #[cfg(test)]
    fn write_stream_body_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
    {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        self.try_dereference()?;
        self.with_value(|value| {
            let entries = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                Some(ObjectValue::Stream { stream_dict, .. }) => {
                    stream_dict.try_dereference()?;
                    stream_dict.with_value(|dict_value| match dict_value {
                        Some(ObjectValue::Dictionary(entries)) => entries
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                        _ => Vec::new(),
                    })
                }
                _ => Vec::new(),
            };
            unparse_stream_dict_entries_with_string_writer(&entries, refiltered, out, write_string)
        })
    }

    /// QDF-mode counterpart of [`Self::write_stream_body`] -- same
    /// delegation-target dimension as the QDF object writer is to
    /// [`Self::write_object`] (`m->qdf_mode` set to `true` inside the
    /// same `QPDFWriter::unparseObject` dictionary branch,
    /// `QPDFWriter.cc:1346-1527`; the `f_stream`/`f_filtered` handling at
    /// `:1440-1455` and the `/Length`-then-`/Filter` tail at `:1508-1524`
    /// run unconditionally there, regardless of `m->qdf_mode` -- only
    /// `indent`/`writeStringQDF` differ between the two modes), matching
    /// `Dictionary::write_pdf_stream_qdf`'s established layout
    /// (`object.rs:1036`) -- multi-line QDF framing (`<<\n`, each
    /// surviving key at `indent + 2` with a trailing `\n`, closing `>>` at
    /// `indent`), with `/Length` pulled out of the iteration and written
    /// last, immediately before `>>` -- plus the same null-suppression
    /// rule as the QDF object writer and [`Self::write_stream_body`],
    /// via the same [`visible_dict_entries`] helper.
    ///
    /// Unlike [`Self::write_stream_body`], this primitive has **no
    /// `refiltered` parameter** -- matching `Dictionary::write_pdf_stream_qdf`'s
    /// own signature exactly, which has none either. This is not fixed by
    /// the caller already holding a settled `/Filter`/`/Length`: unlike a
    /// stored *value*, `refiltered` in the compact path controls emitted
    /// *key order* (`/Filter` pulled after `/Length` vs. left at its plain
    /// alphabetical position) regardless of what `/Filter` already
    /// contains, so a settled dict does not make the dimension moot on its
    /// own. Real qpdf's `unparseObject` *does* apply the identical
    /// `f_filtered` key-pull-and-reappend logic inside `m->qdf_mode` too
    /// (`QPDFWriter.cc:1451-1455`/`:1519-1522`, the same `if` guards,
    /// unguarded by `qdf_mode`) -- so a genuinely re-filtered stream on the
    /// QDF full-rewrite path is, like `write_pdf_stream_qdf` itself, an
    /// existing, out-of-scope simplification this primitive matches rather
    /// than one this task introduces or is asked to fix: this primitive's
    /// signature simply mirrors its delegation target's real (already
    /// simplified) shape, the same convention every other primitive in
    /// this family follows for the legacy function it ports.
    ///
    /// `self` accepts the same two shapes [`Self::write_stream_body`]
    /// does -- a `Dictionary` directly, or a `Stream { stream_dict, .. }`
    /// whose (possibly still-unresolved) `stream_dict` is forced to
    /// resolve -- with the identical error-propagation behavior for every
    /// other shape (degrading to an empty dictionary in this layout's own
    /// `<<\n>>` shape, not the compact sibling's `<< >>`); see that
    /// primitive's own doc for the full contract, which this one mirrors
    /// exactly except for the QDF layout and the missing `refiltered`
    /// parameter.
    #[cfg(test)]
    fn write_stream_body_qdf(&self, out: &mut Vec<u8>, indent: usize) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        self.try_dereference()?;
        self.with_value(|value| {
            let entries = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                Some(ObjectValue::Stream { stream_dict, .. }) => {
                    // Mirrors `write_stream_body`'s identical
                    // `stream_dict.try_dereference()?` -- see that
                    // primitive's own doc for why this is needed rather
                    // than a plain `with_value` read.
                    stream_dict.try_dereference()?;
                    stream_dict.with_value(|dict_value| match dict_value {
                        Some(ObjectValue::Dictionary(entries)) => entries
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                        _ => Vec::new(),
                    })
                }
                _ => Vec::new(),
            };
            unparse_stream_dict_entries_qdf(&entries, indent, out)
        })
    }

    /// QDF stream-dictionary emission with the same reference remapping and
    /// null visibility rules, but with an optional synthetic `/Length`
    /// reference. QDF full-rewrite streams do not retain the source length;
    /// qpdf writes a fresh holder immediately after the stream body. Keeping
    /// that override at the serializer boundary avoids manufacturing a fake
    /// source handle for an output-only object number.
    fn write_stream_body_qdf_with_ref_map_and_removed_and_length(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length_ref: Option<ObjectRef>,
    ) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        self.try_dereference()?;
        self.with_value(|value| {
            let entries = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                Some(ObjectValue::Stream { stream_dict, .. }) => {
                    stream_dict.try_dereference()?;
                    stream_dict.with_value(|dict_value| match dict_value {
                        Some(ObjectValue::Dictionary(entries)) => entries
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                        _ => Vec::new(),
                    })
                }
                _ => Vec::new(),
            };
            unparse_stream_dict_entries_qdf_with_ref_map(
                &entries,
                indent,
                out,
                map,
                removed_refs,
                length_ref,
            )
        })
    }

    /// QDF stream-dictionary emission that combines output-reference
    /// remapping, removed-reference null visibility, and encrypted string
    /// serialization.
    fn write_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length_ref: Option<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
    {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        self.try_dereference()?;
        self.with_value(|value| {
            let entries = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                Some(ObjectValue::Stream { stream_dict, .. }) => {
                    stream_dict.try_dereference()?;
                    stream_dict.with_value(|dict_value| match dict_value {
                        Some(ObjectValue::Dictionary(entries)) => entries
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                        _ => Vec::new(),
                    })
                }
                _ => Vec::new(),
            };
            unparse_stream_dict_entries_qdf_with_ref_map_and_string_writer(
                &entries,
                indent,
                out,
                map,
                removed_refs,
                length_ref,
                write_string,
            )
        })
    }

    /// QDF-mode stream-dictionary counterpart of
    /// [`Self::write_stream_body_with_string_writer`], including its
    /// cleartext hexadecimal signature `/Contents` exception.
    #[cfg(test)]
    fn write_stream_body_qdf_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
    {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        self.try_dereference()?;
        self.with_value(|value| {
            let entries = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                Some(ObjectValue::Stream { stream_dict, .. }) => {
                    stream_dict.try_dereference()?;
                    stream_dict.with_value(|dict_value| match dict_value {
                        Some(ObjectValue::Dictionary(entries)) => entries
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                        _ => Vec::new(),
                    })
                }
                _ => Vec::new(),
            };
            unparse_stream_dict_entries_qdf_with_string_writer(&entries, indent, out, write_string)
        })
    }

    /// Stream-dictionary writer emission with output reference remapping and
    /// qpdf null visibility for references removed during this write.
    fn write_stream_body_with_ref_map_and_removed(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        self.try_dereference()?;
        self.with_value(|value| {
            let entries = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                Some(ObjectValue::Stream { stream_dict, .. }) => {
                    stream_dict.try_dereference()?;
                    stream_dict.with_value(|dict_value| match dict_value {
                        Some(ObjectValue::Dictionary(entries)) => entries
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                        _ => Vec::new(),
                    })
                }
                _ => Vec::new(),
            };
            unparse_stream_dict_entries_with_ref_map(&entries, refiltered, out, map, removed_refs)
        })
    }

    fn write_stream_body_with_ref_map_and_removed_and_length(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length: usize,
    ) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        self.try_dereference()?;
        self.with_value(|value| {
            let entries = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                Some(ObjectValue::Stream { stream_dict, .. }) => {
                    stream_dict.try_dereference()?;
                    stream_dict.with_value(|dict_value| match dict_value {
                        Some(ObjectValue::Dictionary(entries)) => entries
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                        _ => Vec::new(),
                    })
                }
                _ => Vec::new(),
            };
            unparse_stream_dict_entries_with_ref_map_and_length(
                &entries,
                refiltered,
                out,
                map,
                removed_refs,
                Some(length),
            )
        })
    }

    /// Compact stream-dictionary emission with output-reference remapping,
    /// removed-reference null visibility, and encrypted string serialization.
    fn write_stream_body_with_ref_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
    {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        self.try_dereference()?;
        self.with_value(|value| {
            let entries = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                Some(ObjectValue::Stream { stream_dict, .. }) => {
                    stream_dict.try_dereference()?;
                    stream_dict.with_value(|dict_value| match dict_value {
                        Some(ObjectValue::Dictionary(entries)) => entries
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                        _ => Vec::new(),
                    })
                }
                _ => Vec::new(),
            };
            unparse_stream_dict_entries_with_ref_map_and_string_writer(
                &entries,
                refiltered,
                out,
                map,
                removed_refs,
                write_string,
            )
        })
    }

    /// This trailer-shaped dictionary handle's writer-emission form,
    /// porting the caller-visible shape of `QPDFWriter::writeTrailer`
    /// (`QPDFWriter.cc:1160-1236`): the `"trailer <<"` opener (only when
    /// `xref_stream` is `false` -- the xref-stream dictionary's own `<<`
    /// and xref-specific keys, e.g. `/Type`/`/W`/`/Index`, are the
    /// caller's responsibility, matching `writeXRefStream`'s hand-emitted
    /// literals, `QPDFWriter.cc:2391-2495`, which never route through
    /// `unparseObject` or this primitive at all), an unconditional
    /// per-key loop with no `isNull` suppression (`:1174-1192` has no
    /// such check, unlike `unparseObject`'s dictionary branch that
    /// [`Self::write_object`]/[`Self::write_stream_body`] all apply through
    /// `visible_dict_entries`), `/ID` and `/Encrypt` excluded from that
    /// loop and forced last in that order when present, and the closing
    /// `>>` (`:1235`, written unconditionally in both `xref_stream`
    /// cases -- this is why `xref_stream = true` still needs a call into
    /// this function at all, despite skipping the opener). Always
    /// produces the compact (non-QDF) one-line form -- `writeTrailer`'s
    /// own `writeStringQDF` calls (`:1169,1175,1190,1195,1233`) are
    /// QDF-only formatting this primitive does not replicate, matching
    /// handle-native trailer serializer's identical compact-only scope; the
    /// QDF classic trailer is emitted separately by
    /// the canonical writer (`write_qdf_trailer`, `writer.rs`).
    ///
    /// **Narrower than the full C++ function -- read before reusing for a
    /// new caller.** Real `writeTrailer` first calls `getTrimmedTrailer()`
    /// (`:1163`, `:2009-2029`) to remove `/ID`, `/Encrypt`, `/Prev`,
    /// `/Index`, `/W`, `/Length`, `/Filter`, `/DecodeParms`, `/Type`, and
    /// `/XRefStm` from a *copy* of the live document trailer before this
    /// shape ever runs; special-cases `/Size`'s *value* from a
    /// `size: int` parameter, with an additional inline `/Prev <offset>`
    /// append when `which == t_lin_first` (`:1179-1186`); and derives
    /// `/ID`'s value from writer state (`generateID()`/`m->id1`/`m->id2`)
    /// and `/Encrypt`'s from `m->encryption_dict_objid` rather than from
    /// the (already-stripped) dict at all. None of that lives here.
    /// Trimming, the `/Size` value substitution, and the `t_lin_first`
    /// inline `/Prev` are the caller's responsibility -- matching this
    /// crate's own already-established split, where
    /// `strip_writer_trailer_history_keys`/`strip_xref_stream_trailer_keys`
    /// (`writer.rs`) do the trimming and `writer.rs:4012`'s
    /// `trailer.insert("Size", ...)` supplies the correct value before
    /// either the legacy raw serializer or this primitive ever runs. This
    /// primitive has no `which`/`size`/`prev`
    /// parameters at all, so `t_lin_first` is out of scope for the same
    /// reason `t_lin_second` is (see below). `/ID` and `/Encrypt` are
    /// read from `self`'s own stored values instead of from writer state
    /// -- the caller is expected to have already placed the correct
    /// values there (`apply_encrypt_trailer_handle_entries` and the canonical
    /// ID helpers in `writer.rs`), the same contract already established by
    /// qpdf's `writeTrailer` is preserved for that dimension.
    ///
    /// `id_writer`, when `Some`, substitutes for the stored `/ID` value
    /// (used by the deterministic-`/ID` writer to emit a content-derived
    /// identifier inline). When `None`, the stored `/ID` value is written
    /// in qpdf's compact `[<hex1><hex2>]` shape with no spaces
    /// (mirroring qpdf's established compact byte shape, implemented here
    /// directly on `ObjectHandle` rather
    /// than bridged through `Object` -- see `write_id_style_value_handle`
    /// below); an indirect `/ID` value writes as its own `"N G R"`
    /// reference form instead, matching `write_child`'s reference-vs-recurse
    /// split rather than being inlined.
    ///
    /// `self` must resolve to a `Dictionary`; a non-dictionary value
    /// (including `self` itself, forced via `try_dereference`, the same
    /// top-level-entry-point pattern [`Self::write_object`]/
    /// [`Self::write_stream_body`] already use) degrades to an empty
    /// trailer shell, mirroring `write_pdf_stream`/`write_pdf_trailer`'s
    /// own typed-input assumption.
    ///
    /// Out of scope, deliberately: `which == t_lin_second`
    /// (`QPDFWriter.cc:1170-1172`, linearization second pass, `/Size`-only)
    /// and `which == t_lin_first`'s inline `/Prev` (above) have no
    /// equivalent here. A linearization-writer consumer needing either
    /// form is a different primitive.
    #[cfg(test)]
    fn write_trailer(
        &self,
        out: &mut Vec<u8>,
        xref_stream: bool,
        id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
    ) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        self.try_dereference()?;
        self.with_value(|value| {
            let entries: Vec<(Vec<u8>, ObjectHandle)> = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                _ => Vec::new(),
            };
            unparse_trailer_entries(&entries, xref_stream, id_writer, out)
        })
    }

    /// Trailer writer with the same live-handle reference remapping used by
    /// the canonical full-rewrite body route.
    ///
    /// `writeTrailer` is a separate qpdf responsibility from
    /// `unparseObject`: it does not apply ordinary dictionary null
    /// suppression, but its child values still have to be emitted from the
    /// live handle graph and rewritten into the output-number space.  The
    /// `qdf` flag selects qpdf's line-oriented classic-trailer spelling and
    /// passes that mode through to direct child containers, as
    /// `writeTrailer`'s `unparseChild(..., 1, 0)` does
    /// (`QPDFWriter.cc:1160-1236`). Indirect child handles remain references in
    /// either mode.
    #[allow(clippy::too_many_arguments)] // qpdf keeps trailer layout, ID, mapping, and visibility controls orthogonal
    fn write_trailer_with_ref_map(
        &self,
        out: &mut Vec<u8>,
        xref_stream: bool,
        qdf: bool,
        id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        suppress_null_values: bool,
    ) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        self.try_dereference()?;
        self.with_value(|value| {
            let entries: Vec<(Vec<u8>, ObjectHandle)> = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                _ => Vec::new(),
            };
            unparse_trailer_entries_with_ref_map(
                &entries,
                xref_stream,
                qdf,
                id_writer,
                map,
                removed_refs,
                suppress_null_values,
                out,
            )
        })
    }

    /// Serialize a synthetic xref-stream dictionary from live handles in
    /// lexicographic key order. Unlike `writeTrailer`, an xref stream owns its
    /// surrounding `<< >>` and therefore does not use the trailer prefix or
    /// `/ID`-last ordering. This is the handle-native counterpart of the
    /// structural dictionary writer in `QPDFWriter.cc:2391-2495`.
    #[cfg(test)]
    fn write_dictionary_with_ref_map_and_id_writer(
        &self,
        out: &mut Vec<u8>,
        id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        suppress_null_values: bool,
    ) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        self.try_dereference()?;
        self.with_value(|value| {
            let entries: Vec<(Vec<u8>, ObjectHandle)> = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                _ => Vec::new(),
            };
            unparse_dictionary_entries_with_ref_map_and_id_writer(
                &entries,
                id_writer,
                map,
                removed_refs,
                suppress_null_values,
                out,
            )
        })
    }

    /// Serialize a trailer `/ID` value in qpdf's compact
    /// `[<hex0><hex1>]` form while retaining the live handle and reference-map
    /// boundary. Linearized classic trailers keep their fixed-width `/Prev`
    /// field outside the generic trailer primitive, but still use this helper
    /// for the identifier itself (`QPDFWriter.cc:1194-1222`).
    fn write_id_value_with_ref_map(
        &self,
        out: &mut Vec<u8>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()> {
        write_id_style_value_handle_with_ref_map(self, out, map, removed_refs)
    }
}
// Writes a stream dictionary's own body -- `write_stream_body`'s sole
// callee -- matching `Dictionary::write_pdf_stream`'s established shape
// (`object.rs`) with `visible_dict_entries`'s null-suppression layered on
// top, the same delegation `unparse_dict_entries` above makes to that
// helper for the plain (non-stream) dictionary case. `/Length` is captured
// during the single suppressed-entries pass and written last rather than in
// its natural (sorted) position; when `refiltered`, `/Filter` and
// `/DecodeParms` are dropped from that pass and a fresh `/Filter
// /FlateDecode` is appended after `/Length` instead -- both spellings
// verified byte-for-byte against `write_pdf_stream` (`object.rs`) before
// this primitive's tests were written. Also applies the same
// `/Contents`-in-a-`/Sig`-dictionary hex-string special case
// `unparse_dict_entries` applies -- real qpdf's own guard
// (`QPDFWriter.cc:1497-1503`) has no `f_stream` gate, so in principle it
// covers a stream object whose dict happens to be `/Type /Sig` with
// `/ByteRange` too (unusual -- signature dictionaries aren't normally
// streams -- but not structurally ruled out by qpdf's own code).
//
// When `refiltered`, `/Filter` and `/DecodeParms` are excluded from the
// entries `visible_dict_entries` ever sees, rather than left in and skipped
// later during the write loop: real qpdf removes those two keys from a
// shallow copy of the dict entirely BEFORE its null-suppression loop even
// starts (`object.removeKey("/Filter")`/`object.removeKey("/DecodeParms")`
// at `QPDFWriter.cc:1454-1455`, both ahead of the shared loop at
// `:1488-1491`) -- it never calls `isNull()` on a key it is about to
// discard anyway. This primitive previously did the opposite order (compute
// suppression over every entry, including `/Filter`/`/DecodeParms`, and
// only skip those two keys afterward inside the write loop), which could
// force-resolve -- and needlessly fail on -- a stale or unsupported
// indirect `/Filter`/`/DecodeParms` reference that is guaranteed to be
// irrelevant to the refiltered output. The corresponding qpdf writer path
// removes these entries before checking which dictionary values are visible.
fn unparse_stream_dict_entries(
    entries: &[(Vec<u8>, ObjectHandle)],
    refiltered: bool,
    out: &mut Vec<u8>,
) -> Result<()> {
    let excluded_entries;
    let entries: &[(Vec<u8>, ObjectHandle)] = if refiltered {
        excluded_entries = entries
            .iter()
            .filter(|entry| {
                entry.0.as_slice() != b"/Filter" && entry.0.as_slice() != b"/DecodeParms"
            })
            .cloned()
            .collect::<Vec<_>>();
        &excluded_entries
    } else {
        entries
    };
    out.extend_from_slice(b"<<");
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(entries)? {
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        out.push(b' ');
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            write_child(value, out)?;
        }
    }
    if let Some(length) = length_value {
        out.extend_from_slice(b" /Length ");
        write_child(length, out)?;
    }
    if refiltered {
        out.extend_from_slice(b" /Filter /FlateDecode");
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

// QDF-mode sibling of `unparse_stream_dict_entries` above --
// `write_stream_body_qdf`'s sole callee -- matching
// `Dictionary::write_pdf_stream_qdf`'s established shape (`object.rs`)
// with `visible_dict_entries`'s null-suppression layered on top, the same
// delegation `unparse_dict_entries_qdf` makes to that helper for the
// plain (non-stream) QDF dictionary case. `/Length` is captured during
// the single suppressed-entries pass and written last, at `indent + 2`,
// immediately before the closing `>>` at `indent` -- no `refiltered`
// dimension exists here, matching `write_pdf_stream_qdf`'s own signature
// (see `write_stream_body_qdf`'s own doc for why). Verified byte-for-byte
// against `write_pdf_stream_qdf` (`object.rs`) before this primitive's
// tests were written. Applies the same `/Contents`-in-a-`/Sig`-dictionary
// hex-string special case `unparse_stream_dict_entries` applies, for the
// same reason (see that function's own doc); this function has no
// `refiltered` parameter to begin with, so the Finding-2
// remove-then-suppress reordering that primitive also needed does not apply
// here -- there is no `/Filter`/`/DecodeParms` drop in this function for
// that reordering to fix.
#[cfg(test)]
fn unparse_stream_dict_entries_qdf(
    entries: &[(Vec<u8>, ObjectHandle)],
    indent: usize,
    out: &mut Vec<u8>,
) -> Result<()> {
    out.extend_from_slice(b"<<\n");
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(entries)? {
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        push_spaces(out, indent + 2);
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            write_child_qdf(value, indent + 2, out)?;
        }
        out.push(b'\n');
    }
    if let Some(length) = length_value {
        push_spaces(out, indent + 2);
        out.extend_from_slice(b"/Length ");
        write_child_qdf(length, indent + 2, out)?;
        out.push(b'\n');
    }
    push_spaces(out, indent);
    out.extend_from_slice(b">>");
    Ok(())
}

fn unparse_stream_dict_entries_qdf_with_ref_map(
    entries: &[(Vec<u8>, ObjectHandle)],
    indent: usize,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    length_ref: Option<ObjectRef>,
) -> Result<()> {
    out.extend_from_slice(b"<<\n");
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(entries)? {
        if is_removed_reference(value, removed_refs) {
            continue;
        }
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        push_spaces(out, indent + 2);
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            write_child_qdf_with_ref_map(value, indent + 2, out, map, removed_refs)?;
        }
        out.push(b'\n');
    }
    if let Some(length_ref) = length_ref {
        push_spaces(out, indent + 2);
        out.extend_from_slice(b"/Length ");
        out.extend_from_slice(length_ref.to_string().as_bytes());
        out.push(b'\n');
    } else if let Some(length) = length_value {
        push_spaces(out, indent + 2);
        out.extend_from_slice(b"/Length ");
        write_child_qdf_with_ref_map(length, indent + 2, out, map, removed_refs)?;
        out.push(b'\n');
    }
    push_spaces(out, indent);
    out.extend_from_slice(b">>");
    Ok(())
}

fn unparse_stream_dict_entries_qdf_with_ref_map_and_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    indent: usize,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    length_ref: Option<ObjectRef>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    out.extend_from_slice(b"<<\n");
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(entries)? {
        if is_removed_reference(value, removed_refs) {
            continue;
        }
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        push_spaces(out, indent + 2);
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            out.push(b'\n');
            continue;
        }
        write_child_qdf_with_ref_map_and_string_writer(
            value,
            indent + 2,
            out,
            map,
            removed_refs,
            write_string,
        )?; // cov:ignore: LLVM maps the covered mapped stream child call continuation to this line
        out.push(b'\n');
    }
    push_spaces(out, indent + 2);
    out.extend_from_slice(b"/Length ");
    if let Some(length_ref) = length_ref {
        out.extend_from_slice(length_ref.to_string().as_bytes());
    } else if let Some(length) = length_value {
        write_child_qdf_with_ref_map_and_string_writer(
            length,
            indent + 2,
            out,
            map,
            removed_refs,
            write_string,
        )?; // cov:ignore: LLVM maps the covered mapped length child call continuation to this line
    } else {
        out.extend_from_slice(b"null");
    }
    out.push(b'\n');
    push_spaces(out, indent);
    out.extend_from_slice(b">>");
    Ok(())
}

#[cfg(test)]
fn unparse_stream_dict_entries_with_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    refiltered: bool,
    out: &mut Vec<u8>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    let excluded_entries;
    let entries: &[(Vec<u8>, ObjectHandle)] = if refiltered {
        excluded_entries = entries
            .iter()
            .filter(|entry| {
                entry.0.as_slice() != b"/Filter" && entry.0.as_slice() != b"/DecodeParms"
            })
            .cloned()
            .collect::<Vec<_>>();
        &excluded_entries
    } else {
        entries
    };
    out.extend_from_slice(b"<<");
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(entries)? {
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        out.push(b' ');
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if try_write_sig_contents_with_string_writer(value, force_hex_string, out)? {
            continue;
        }
        write_child_with_string_writer(value, out, write_string)?;
    }
    if let Some(length) = length_value {
        out.extend_from_slice(b" /Length ");
        write_child_with_string_writer(length, out, write_string)?;
    }
    if refiltered {
        out.extend_from_slice(b" /Filter /FlateDecode");
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

#[cfg(test)]
fn unparse_stream_dict_entries_qdf_with_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    indent: usize,
    out: &mut Vec<u8>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    out.extend_from_slice(b"<<\n");
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(entries)? {
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        push_spaces(out, indent + 2);
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if try_write_sig_contents_with_string_writer(value, force_hex_string, out)? {
            out.push(b'\n');
            continue;
        }
        write_child_qdf_with_string_writer(value, indent + 2, out, write_string)?;
        out.push(b'\n');
    }
    if let Some(length) = length_value {
        push_spaces(out, indent + 2);
        out.extend_from_slice(b"/Length ");
        write_child_qdf_with_string_writer(length, indent + 2, out, write_string)?;
        out.push(b'\n');
    }
    push_spaces(out, indent);
    out.extend_from_slice(b">>");
    Ok(())
}

fn unparse_stream_dict_entries_with_ref_map(
    entries: &[(Vec<u8>, ObjectHandle)],
    refiltered: bool,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    unparse_stream_dict_entries_with_ref_map_and_length(
        entries,
        refiltered,
        out,
        map,
        removed_refs,
        None,
    )
}

fn unparse_stream_dict_entries_with_ref_map_and_length(
    entries: &[(Vec<u8>, ObjectHandle)],
    refiltered: bool,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    length_override: Option<usize>,
) -> Result<()> {
    let excluded_entries;
    let entries: &[(Vec<u8>, ObjectHandle)] = if refiltered {
        excluded_entries = entries
            .iter()
            .filter(|entry| {
                entry.0.as_slice() != b"/Filter" && entry.0.as_slice() != b"/DecodeParms"
            })
            .cloned()
            .collect::<Vec<_>>();
        &excluded_entries
    } else {
        entries
    };
    out.extend_from_slice(b"<<");
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(entries)? {
        if is_removed_reference(value, removed_refs) {
            continue;
        }
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        out.push(b' ');
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            write_child_with_ref_map(value, out, map, removed_refs)?;
        }
    }
    if let Some(length) = length_override {
        out.extend_from_slice(b" /Length ");
        out.extend_from_slice(length.to_string().as_bytes());
    } else if let Some(length) = length_value {
        out.extend_from_slice(b" /Length ");
        write_child_with_ref_map(length, out, map, removed_refs)?;
    }
    if refiltered {
        out.extend_from_slice(b" /Filter /FlateDecode");
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

fn unparse_stream_dict_entries_with_ref_map_and_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    refiltered: bool,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    let excluded_entries;
    let entries: &[(Vec<u8>, ObjectHandle)] = if refiltered {
        excluded_entries = entries
            .iter()
            .filter(|entry| {
                entry.0.as_slice() != b"/Filter" && entry.0.as_slice() != b"/DecodeParms"
            })
            .cloned()
            .collect::<Vec<_>>();
        &excluded_entries
    } else {
        entries
    };
    out.extend_from_slice(b"<<");
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(entries)? {
        if is_removed_reference(value, removed_refs) {
            continue;
        }
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        out.push(b' ');
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            continue;
        }
        write_child_with_ref_map_and_string_writer(value, out, map, removed_refs, write_string)?;
    }
    if let Some(length) = length_value {
        out.extend_from_slice(b" /Length ");
        write_child_with_ref_map_and_string_writer(length, out, map, removed_refs, write_string)?;
    }
    if refiltered {
        out.extend_from_slice(b" /Filter /FlateDecode");
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

// Writes one child handle's bytes for the plain-unparse family serviced by
// `unparse_object_walk` below: an indirect child always writes as its own
// `"N G R"` reference form, never recursed into — the same reference-vs-
// recurse split used by the child-unparse helpers above already
// apply, mirroring `QPDFWriter::unparseChild`'s own `child.isIndirect()`
// check (`libqpdf/QPDFWriter.cc:1144-1156`, the check itself at `:1149`),
// which `unparseObject`'s array-element and dictionary-value loops call into
// for exactly this decision (`:1342`, `:1503`) instead of inlining it. A
// direct child recurses through `unparse_object_walk`.
//
// No separate reserved check here, for the same reason the child-unparse helper
// has none in its own reference-vs-recurse decision (see its own doc): the
// decision below is `isIndirect()`-only, matching `unparseChild` exactly,
// and never inspects the referenced object's resolved type. An *indirect*
// reserved child always takes the reference-token branch below without ever
// being dereferenced here. A *direct* reserved child does still get
// rejected, but one level down: the `None` branch recurses into
// `unparse_object_walk`, whose own `is_reserved` check
// (`QPDF_Reserved::unparse()`, `libqpdf/QPDF_Reserved.cc:22-26`'s throw)
// runs on whatever handle it is entered with, top-level `self` or a
// recursed-into direct child alike -- this function does not need its own
// copy of that check to get the same result.
pub(crate) fn write_child(handle: &ObjectHandle, out: &mut Vec<u8>) -> Result<()> {
    if let Some(object_ref) = handle.object_ref() {
        out.extend_from_slice(object_ref.to_string().as_bytes());
        return Ok(());
    }
    unparse_object_walk(handle, out)
}

// Filters `entries` down to the ones `unparseObject`'s dictionary branch
// would actually write (`QPDFWriter.cc:1490-1491`). Forces resolution of
// every indirect *value* via `try_is_null` to decide suppression -- this is
// the one place in this primitive family that performs that particular
// hidden I/O qpdf's own `isNull()` performs and every other *value*
// accessor in this file deliberately avoids (see `unparse_resolved`'s own
// doc on why *it* does not resolve on the caller's behalf).
// `unparse_object_walk` separately forces resolution of `self` -- a
// different target, for a different reason: dispatching on `self`'s own
// resolved type, not deciding whether to suppress it. Neither forced
// resolution is a contract violation here: `QPDFWriter::unparseObject` is a
// writer-internal path with no no-hidden-I/O constraint to begin with.
pub(crate) fn visible_dict_entries(
    entries: &[(Vec<u8>, ObjectHandle)],
) -> Result<Vec<(&Vec<u8>, &ObjectHandle)>> {
    let mut visible = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        if !value.try_is_null()? {
            visible.push((key, value));
        }
    }
    Ok(visible)
}

fn is_removed_reference(handle: &ObjectHandle, removed_refs: &BTreeSet<ObjectRef>) -> bool {
    handle
        .object_ref()
        .is_some_and(|object_ref| removed_refs.contains(&object_ref))
}

/// Copy the root container and reconcile `/ADBE` with qpdf's child aliasing.
fn root_output_copy_with_adbe(
    source: &ObjectHandle,
    final_pdf_version: &str,
    final_extension_level: i64,
) -> Result<ObjectHandle> {
    let root = source.unsafe_shallow_copy()?;
    let mut extensions = if root.try_has_key(b"/Extensions")?
        && root.try_get_key(b"/Extensions")?.try_is_dictionary()?
    {
        Some(root.try_get_key(b"/Extensions")?)
    } else {
        None
    };
    let (have_adbe, have_other) = if let Some(extensions) = &extensions {
        let mut keys = extensions.try_get_keys()?;
        let have_adbe = keys.remove(b"/ADBE".as_slice());
        (have_adbe, !keys.is_empty())
    } else {
        (false, false)
    };
    let need_adbe = final_extension_level > 0;
    if need_adbe {
        if !(have_other || have_adbe) {
            let created = ObjectHandle::dictionary(Vec::new());
            root.replace_key(b"/Extensions", created.clone())?;
            extensions = Some(created);
        }
    } else if !have_other && have_adbe {
        root.remove_key(b"/Extensions");
        extensions = None;
    }

    if let Some(extensions) = extensions {
        let adbe = extensions.try_get_key(b"/ADBE")?;
        let preserves_existing = adbe.try_is_dictionary()?
            && adbe
                .try_get_key(b"/BaseVersion")?
                .try_is_name_and_equals(final_pdf_version.as_bytes())?
            && adbe.try_get_key(b"/ExtensionLevel")?.try_as_integer()?
                == Some(final_extension_level);
        if !preserves_existing {
            if need_adbe {
                extensions.replace_key(
                    b"/ADBE",
                    ObjectHandle::dictionary(vec![
                        (
                            b"/BaseVersion".to_vec(),
                            ObjectHandle::name(final_pdf_version.as_bytes().to_vec()),
                        ),
                        (
                            b"/ExtensionLevel".to_vec(),
                            ObjectHandle::integer(final_extension_level),
                        ),
                    ]),
                )?;
            } else {
                extensions.remove_key(b"/ADBE");
            }
        }
    }

    Ok(root)
}

// The sole recursion hub for the plain unparse family (`ObjectHandle::
// write_object` and its callees below), mirroring the unparse walk's
// own single-hub pattern above for the same stack-growth reason: an
// `ObjectHandle` tree built through public factories carries no depth bound
// the parser enforces on parsed input. Also forces resolution of `handle`
// itself before inspecting its value: every call into this hub either comes
// from `write_object`'s top-level entry point (whose argument may still be
// an unresolved indirect handle) or from a direct child that `write_child`
// has already filtered past its own indirect check (so `handle` here is
// always already direct in that case, making the call a no-op) — mirroring
// qpdf's own implicit `dereference()` on `object`'s first `isXxx()` type
// check inside `unparseObject` itself, rather than the no-hidden-I/O
// contract [`ObjectHandle::with_value`]'s other callers rely on.
enum UnparseContainer {
    Array(Vec<ObjectHandle>),
    Dictionary(Vec<(Vec<u8>, ObjectHandle)>),
    Stream(ObjectHandle),
}

// qpdf's writer walks a live container and does not clone scalar payloads.
// The RefCell borrow must nevertheless be released before a child is resolved,
// since resolution can mutate the same shared state. Snapshot only the edges
// needed for a later recursive descent; scalar/name/string bytes are emitted
// while their borrow is still active.
fn snapshot_unparse_container(value: &ObjectValue) -> Option<UnparseContainer> {
    match value {
        ObjectValue::Array(children) => Some(UnparseContainer::Array(children.clone())),
        ObjectValue::Dictionary(entries) => Some(UnparseContainer::Dictionary(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        )),
        ObjectValue::Stream { stream_dict, .. } => {
            Some(UnparseContainer::Stream(stream_dict.clone()))
        }
        _ => None,
    }
}

fn unparse_container(container: UnparseContainer, out: &mut Vec<u8>) -> Result<()> {
    match container {
        UnparseContainer::Array(children) => {
            // QPDFWriter.cc:1334-1345: no token-boundary rule, a space is
            // written before every element regardless of adjacency.
            out.push(b'[');
            for child in children {
                out.push(b' ');
                write_child(&child, out)?;
            }
            out.extend_from_slice(b" ]");
        }
        UnparseContainer::Dictionary(entries) => unparse_dict_entries(&entries, out)?,
        UnparseContainer::Stream(stream_dict) => {
            // This primitive inlines only a stream's dictionary; stream
            // framing remains `write_stream_body`'s responsibility.
            unparse_object_walk(&stream_dict, out)?;
        }
    }
    Ok(())
}

fn unparse_object_walk(handle: &ObjectHandle, out: &mut Vec<u8>) -> Result<()> {
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        let container = handle.with_value(|value| match value {
            Some(value) => {
                if let Some(container) = snapshot_unparse_container(value) {
                    Ok(Some(container))
                } else {
                    // Scalars have no child to resolve, so serialize them
                    // while the borrow is active instead of cloning payloads.
                    unparse_object_value(value, out).map(|()| None)
                }
            }
            None => {
                // cov:ignore-start: unreachable once `try_dereference()`
                // above has returned `Ok`; retain the conservative null
                // fallback for a resolver that violates that invariant.
                out.extend_from_slice(b"null");
                Ok(None)
                // cov:ignore-end
            }
        })?;
        match container {
            Some(container) => unparse_container(container, out),
            None => Ok(()),
        }
    })
}

pub(crate) fn unparse_object_value(value: &ObjectValue, out: &mut Vec<u8>) -> Result<()> {
    match value {
        ObjectValue::Null => out.extend_from_slice(b"null"),
        ObjectValue::Unresolved => return Err(unresolved_unparse_error()),
        ObjectValue::Reserved => return Err(reserved_unparse_error()),
        ObjectValue::Destroyed => return Err(destroyed_unparse_error()),
        ObjectValue::Boolean(v) => out.extend_from_slice(if *v { b"true" } else { b"false" }),
        ObjectValue::Integer(v) => out.extend_from_slice(v.to_string().as_bytes()),
        ObjectValue::Real(v) => out.extend_from_slice(v.to_string().as_bytes()),
        ObjectValue::RealLiteral { value, literal } => {
            if crate::pdf_syntax::real_literal_is_safe(literal, *value) {
                out.extend_from_slice(literal);
            } else {
                out.extend_from_slice(value.to_string().as_bytes());
            }
        }
        ObjectValue::Name(name) => {
            out.push(b'/');
            crate::pdf_syntax::write_name_escaped(out, name);
        }
        ObjectValue::String(value) => crate::pdf_syntax::write_string_value(out, value),
        ObjectValue::Operator(value) | ObjectValue::InlineImage(value) => {
            out.extend_from_slice(value);
        }
        ObjectValue::Array(children) => {
            // QPDFWriter.cc:1334-1345: no token-boundary rule, a space is
            // written before every element regardless of adjacency.
            out.push(b'[');
            for child in children {
                out.push(b' ');
                write_child(child, out)?;
            }
            out.extend_from_slice(b" ]");
        }
        ObjectValue::Dictionary(entries) => {
            let entries: Vec<(Vec<u8>, ObjectHandle)> = entries
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            unparse_dict_entries(&entries, out)?;
        }
        ObjectValue::Stream { stream_dict, .. } => {
            // Reachable two ways, not just one: a *direct* Stream value (no
            // qpdf counterpart -- a real QPDFObjectHandle's resolved value
            // is never itself a stream outside an indirect object), and an
            // *indirect* `self` at the top level of `write_object` that
            // resolves to a stream (a real, reachable qpdf shape -- see
            // `ObjectHandle::write_object`'s own doc). The latter is
            // reachable here because `write_object`/`unparse_object_walk`
            // call this dispatch directly on `self`, bypassing `write_child`
            // entirely; `write_child` only gates *child* positions (array
            // elements, dictionary values) during recursion, where it never
            // recurses into an indirect handle -- so an *indirect* child
            // resolving to a stream short-circuits to its own `"N G R"`
            // form and never reaches this arm. A *direct* child whose value
            // is a Stream does reach it, by the first case above.
            //
            // Either way, this arm inlines only the dictionary, deliberately
            // not the `stream`/`endstream` framing: that framing (and the
            // `/Length`-last, optionally re-filtered stream-dictionary
            // layout it wraps) is `write_stream_body`'s own, separately
            // scoped responsibility -- this generic dispatch does not
            // implement qpdf's real
            // stream-writing path for the indirect case either.
            unparse_object_walk(stream_dict, out)?;
        }
    }
    Ok(())
}

type ObjectRefMap<'a> = dyn Fn(ObjectRef) -> Result<ObjectRef> + 'a;

// Ref-map sibling of `write_child` above -- same reference-vs-recurse split
// on `handle.object_ref()` alone, so the same reasoning applies: an
// *indirect* reserved child takes this `Some` branch (writing its mapped
// reference token, or `null` if renumbering removed it, per the
// qpdf-rewrite null-handling below) without ever being dereferenced here.
// See `write_child`'s own doc for why no separate reserved check belongs in
// a child-position function at all: the `None` branch below recurses into
// `unparse_object_walk_with_ref_map`, whose own `is_reserved` check already
// rejects a *direct* reserved child the same way it rejects a reserved
// top-level `self`. This is the primitive `writer/plain/body.rs`/
// `writer/plain/plan.rs` actually call in production, so a direct reserved child
// reaching a live document write is already covered by this path.
fn write_child_with_ref_map(
    handle: &ObjectHandle,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    if let Some(object_ref) = handle.object_ref() {
        if object_ref.number == 0 || removed_refs.contains(&object_ref) {
            // qpdf's direct-null identity is object number zero, not an
            // output reference (QPDFObjectHandle.cc:344-350). A removed
            // identity follows the same null path in the qpdf rewrite.
            out.extend_from_slice(b"null");
            return Ok(());
        }
        let mapped = map(object_ref)?;
        out.extend_from_slice(mapped.to_string().as_bytes());
        return Ok(());
    }
    unparse_object_walk_with_ref_map(handle, out, map, removed_refs)
}

fn unparse_object_walk_with_ref_map(
    handle: &ObjectHandle,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        let container = handle.with_value(|value| match value {
            Some(value) => {
                if let Some(container) = snapshot_unparse_container(value) {
                    Ok(Some(container))
                } else {
                    // Scalar payloads are written under the borrow; only
                    // container edges need an owned snapshot before descent.
                    unparse_object_value_with_ref_map(value, out, map, removed_refs).map(|()| None)
                }
            }
            None => {
                // cov:ignore-start: successful dereference exposes Null for
                // the null fallback or errors while unresolved.
                out.extend_from_slice(b"null");
                Ok(None)
                // cov:ignore-end
            }
        })?;
        match container {
            Some(container) => unparse_container_with_ref_map(container, out, map, removed_refs),
            None => Ok(()),
        }
    })
}

fn unparse_container_with_ref_map(
    container: UnparseContainer,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    match container {
        UnparseContainer::Array(children) => {
            out.push(b'[');
            for child in children {
                out.push(b' ');
                write_child_with_ref_map(&child, out, map, removed_refs)?;
            }
            out.extend_from_slice(b" ]");
        }
        UnparseContainer::Dictionary(entries) => {
            unparse_dict_entries_with_ref_map(&entries, out, map, removed_refs)?;
        }
        UnparseContainer::Stream(stream_dict) => {
            unparse_object_walk_with_ref_map(&stream_dict, out, map, removed_refs)?;
        }
    }
    Ok(())
}

fn unparse_object_value_with_ref_map(
    value: &ObjectValue,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    match value {
        ObjectValue::Array(children) => {
            out.push(b'[');
            for child in children {
                out.push(b' ');
                write_child_with_ref_map(child, out, map, removed_refs)?;
            }
            out.extend_from_slice(b" ]");
        }
        ObjectValue::Dictionary(entries) => {
            let entries: Vec<(Vec<u8>, ObjectHandle)> = entries
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            unparse_dict_entries_with_ref_map(&entries, out, map, removed_refs)?;
        }
        ObjectValue::Stream { stream_dict, .. } => {
            unparse_object_walk_with_ref_map(stream_dict, out, map, removed_refs)?;
        }
        _ => unparse_object_value(value, out)?,
    }
    Ok(())
}

fn unparse_dict_entries_with_ref_map(
    entries: &[(Vec<u8>, ObjectHandle)],
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    out.extend_from_slice(b"<<");
    for (key, value) in visible_dict_entries(entries)? {
        if is_removed_reference(value, removed_refs) {
            continue;
        }
        out.push(b' ');
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            write_child_with_ref_map(value, out, map, removed_refs)?;
        }
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

// Detects the sibling condition `QPDFWriter::unparseObject`'s dictionary
// branch checks per key before special-casing `/Contents`
// (`QPDFWriter.cc:1497-1498`: `object.isDictionaryOfType("/Sig") &&
// object.hasKey("/ByteRange")`) -- `object` there is the dict *being
// written* (this function's own `entries`), not the `/Contents` value
// itself. Checked in qpdf's own short-circuit order: `/Type` first, then
// `/ByteRange` only if `/Type` was `/Sig` -- `isDictionaryOfType`
// (`QPDFObjectHandle.cc:461-466`) resolves `/Type`'s own value through
// `getKey("/Type").isNameAndEquals("/Sig")` (`isNameAndEquals` calls
// `isName()`, which dereferences), so an indirect `/Type` value is
// force-resolved here too, matching that (the suppression predicate above,
// `visible_dict_entries`, already accepts this same "no-hidden-I/O
// constraint" tradeoff for the identical writer-internal reason -- see its
// own doc).
//
// `hasKey` is **not** pure map-containment despite its name:
// `QPDFObjectHandle::hasKey` (`QPDFObjectHandle.cc:965-976`) delegates to
// `QPDF_Dictionary::hasKey` (`QPDF_Dictionary.cc:98-101`), which is
// `items.count(key) > 0 && !items[key].isNull()` -- `isNull()`
// (`QPDFObjectHandle.cc:353-356`) dereferences too, so a `/ByteRange` key
// whose value resolves to null (directly or indirectly) counts as *absent*,
// the same null-suppression rule `visible_dict_entries` already applies to
// dict entries generally. `/ByteRange`'s own value is therefore
// force-resolved here as well -- but only after `/Type` was already
// confirmed `/Sig`, matching qpdf's `&&` short-circuit: a dict whose
// `/Type` is not `/Sig` never touches `/ByteRange`'s resolver at all.
pub(crate) fn dict_is_sig_with_byte_range(entries: &[(Vec<u8>, ObjectHandle)]) -> Result<bool> {
    let Some((_, type_value)) = entries.iter().find(|entry| entry.0.as_slice() == b"/Type") else {
        return Ok(false);
    };
    type_value.try_dereference()?;
    let is_sig = type_value.with_value(
        |value| matches!(value, Some(ObjectValue::Name(name)) if name.as_slice() == b"Sig"),
    );
    if !is_sig {
        return Ok(false);
    }
    let Some((_, byte_range_value)) = entries
        .iter()
        .find(|entry| entry.0.as_slice() == b"/ByteRange")
    else {
        return Ok(false);
    };
    Ok(!byte_range_value.try_is_null()?)
}

// Applies qpdf's `/Contents`-in-a-signature-dictionary hex-string special
// case (`QPDFWriter.cc:1490-1504`) to a single dict-value child in place of
// the ordinary `write_child`/`write_child_qdf` call, when `force_hex_string`
// is set (every call site below passes `key.as_slice() == b"Contents" &&
// dict_is_sig_with_byte_range(entries)?`, matching the `key == "/Contents" &&
// object.isDictionaryOfType(...) && object.hasKey(...)` guard at the same
// source lines). Returns `Ok(true)` when it wrote the value itself -- the
// caller must not also call the ordinary child-writer in that case -- or
// `Ok(false)` when the ordinary path should run instead.
//
// The key check must come *first* in that `&&`, not merely for a byte-for-byte
// mirror of qpdf's own operand order: qpdf's guard sits inside the *same*
// per-item loop that already visits every key for null-suppression
// (`:1488-1491`), so `isDictionaryOfType`/`hasKey`'s own resolution of
// `/Type`/`/ByteRange` only ever runs when that loop's *current* item is
// literally `/Contents` -- a dict with no `/Contents` key never reaches it at
// all. Every call site below evaluates `dict_is_sig_with_byte_range(entries)?`
// on that same short-circuited, per-key-gated basis -- once, lazily, only if
// and when the loop below actually reaches a surviving `/Contents` key --
// rather than once, unconditionally, before the loop starts.
//
// Note what this ordering fix does *not* claim: unlike the
// `refiltered`-key exclusion (see `unparse_stream_dict_entries`'s own doc),
// this is not a "never touched at all" guarantee for `/Type`/`/ByteRange`.
// Both remain ordinary surviving dict keys -- unlike `/Filter`/`/DecodeParms`
// under `refiltered`, they are never removed from `entries` -- so
// `visible_dict_entries`'s own generic per-item null check
// (`:1488`/`isNull()`, mirrored here) force-resolves them anyway whenever
// they are present, independent of whether `/Contents` exists at all. What
// hoisting this call above the loop (as this function previously did)
// actually changes is *ordering*: it force-resolves `/Type` (and,
// conditionally, `/ByteRange`) *before* the null-suppression pass runs at
// all, so a dict whose surviving keys straddle `/Type` in the dict's own
// (`BTreeMap`) alphabetical order surfaces `/Type`'s own resolution error
// ahead of an earlier-sorting key's error that qpdf's single-pass loop would
// have reached first. Gating the call on the loop's *current* key, as fixed
// here, keeps that resolution order aligned with qpdf's own single pass.
// The test
// `unparse_object_defers_type_and_byte_range_resolution_until_a_contents_key_is_reached`
// below, which pins this ordering against a dict with no `/Contents` key at
// all -- and documents, in its own comment, why a plain success-vs-error
// assertion cannot observe this fix at all).
//
// Matches `unparseChild`'s own indirect-first short-circuit
// (`QPDFWriter.cc:1149-1156`): an indirect child still writes as its own
// `"N G R"` reference form regardless of the flag -- real qpdf's flags are
// consulted only inside `unparseObject`, which `unparseChild` never reaches
// for an indirect child at all -- so this only ever has an effect on a
// *direct* child. Even then, it only affects a child whose resolved value is
// itself a String: qpdf's own `f_hex_string` handling lives inside
// `unparseObject`'s `ot_string` arm alone (`QPDFWriter.cc:1567,1594-1595`);
// every other resolved type's arm never inspects the flag, so a non-String
// direct child (unusual for `/Contents` in practice, but not structurally
// ruled out) falls through to the ordinary child-writer unaffected, matching
// that.
//
// Deliberately does not implement `f_no_encryption` (`QPDFWriter.cc:1501`)
// -- qpdf's `ot_string` arm consults it only inside its own `m->encrypted`
// branch (`:1569-1593`), routing this one child's bytes through a
// non-encrypting sub-pipeline while the rest of the document is encrypted.
// This crate's `ObjectHandle` writer-emission primitives carry no
// pipeline/encryption context at all -- every one of them is a plain
// `(&self, out: &mut Vec<u8>, ...) -> Result<()>` -- so there is no
// encryption state to route around in the first place here; wiring an
// actual encryption pipeline around these bytes is a future
// consumer-migration/encryption-integration concern this primitive does not
// implement, matching the scope limits `write_stream_body`/
// `write_trailer` already document for their own out-of-scope qpdf steps
// (e.g. the `t_lin_second` branch, the `/Crypt`-filter stripping logic).
fn try_write_sig_contents_hex_string(
    handle: &ObjectHandle,
    force_hex_string: bool,
    out: &mut Vec<u8>,
) -> Result<bool> {
    if !force_hex_string || handle.object_ref().is_some() {
        return Ok(false);
    }
    handle.try_dereference()?;
    Ok(handle.with_value(|value| {
        if let Some(ObjectValue::String(bytes)) = value {
            crate::pdf_syntax::write_hex_string(out, bytes);
            true
        } else {
            false
        }
    }))
}

// Writes `<< /K1 v1 /K2 v2 >>` with qpdf's suppression rule applied
// (`QPDFWriter.cc:1488-1527`, non-stream case: no `/Length` tail). Matches
// `Dictionary::write_pdf`'s own key-writing shape (`object.rs:839-848`): a
// leading space, then `/` + the escaped key, pushed separately since
// `write_name_escaped` does not write the leading slash itself. Also applies
// the `/Contents`-in-a-`/Sig`-dictionary hex-string special case that same
// qpdf loop applies unconditionally (`QPDFWriter.cc:1490-1504`) -- see
// `dict_is_sig_with_byte_range`/`try_write_sig_contents_hex_string`'s own
// docs for the detection/writing split.
fn unparse_dict_entries(entries: &[(Vec<u8>, ObjectHandle)], out: &mut Vec<u8>) -> Result<()> {
    out.extend_from_slice(b"<<");
    for (key, value) in visible_dict_entries(entries)? {
        out.push(b' ');
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            write_child(value, out)?;
        }
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

// Append `n` ASCII space bytes to `out` — the QDF family's own copy of
// `object.rs`'s private `push_spaces` helper. Not reusable across the module
// boundary (that one is not `pub(crate)`, and this task's scope is
// `object_handle.rs` only), but the two are one-line bodies, not logic worth
// sharing at the cost of widening `object.rs`'s API for a single call site.
fn push_spaces(out: &mut Vec<u8>, n: usize) {
    out.resize(out.len() + n, b' ');
}

// QDF-mode sibling of `write_child` above: an indirect child always writes
// as its own `"N G R"` reference form regardless of QDF mode — qpdf never
// inlines an indirect object at a child position in either mode, the same
// unconditional child/reference split already applies. A direct
// child recurses through `unparse_object_walk_qdf` at `indent`, the same
// column its own container already committed to for this child (an array
// element or dict value sits at its container's `indent + 2`; see
// `unparse_object_value_qdf`'s own Array/Dictionary arms for where that
// `+ 2` is actually applied before calling this).
//
// No separate reserved check either, for the identical reason `write_child`
// has none (see its own doc for the full trace): an *indirect* reserved
// child always takes the reference-token branch below without ever being
// dereferenced here, and a *direct* one is still rejected one level down,
// by `unparse_object_walk_qdf`'s own `is_reserved` check on whatever
// handle the `None` branch below recurses into.
fn write_child_qdf(handle: &ObjectHandle, indent: usize, out: &mut Vec<u8>) -> Result<()> {
    if let Some(object_ref) = handle.object_ref() {
        out.extend_from_slice(object_ref.to_string().as_bytes());
        return Ok(());
    }
    unparse_object_walk_qdf(handle, indent, out)
}

// QDF-mode sibling of `unparse_object_walk` above, threading an `indent`
// column through the same forced-top-level-resolution / stack-growth-wrapped
// recursion hub shape. See that function's own doc for why `try_dereference`
// is forced here rather than left to `with_value`'s ordinary no-hidden-I/O
// contract, and for the same conservative-null fallback rationale on the
// `None` arm below.
fn unparse_object_walk_qdf(handle: &ObjectHandle, indent: usize, out: &mut Vec<u8>) -> Result<()> {
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        let container = handle.with_value(|value| match value {
            Some(value) => {
                if let Some(container) = snapshot_unparse_container(value) {
                    Ok(Some(container))
                } else {
                    // QDF changes container framing only; scalar bytes take
                    // the same no-copy path as compact unparse.
                    unparse_object_value_qdf(value, indent, out).map(|()| None)
                }
            }
            None => {
                // cov:ignore-start: unreachable once `try_dereference()`
                // above has returned `Ok` -- see `unparse_object_walk`'s own
                // identical arm for why.
                out.extend_from_slice(b"null");
                Ok(None)
                // cov:ignore-end
            }
        })?;
        match container {
            Some(container) => unparse_container_qdf(container, indent, out),
            None => Ok(()),
        }
    })
}

fn unparse_container_qdf(
    container: UnparseContainer,
    indent: usize,
    out: &mut Vec<u8>,
) -> Result<()> {
    match container {
        UnparseContainer::Array(children) => {
            // qpdf's QDF array arm: `[`, a newline, then each
            // child at `indent + 2`, followed by the closing bracket at
            // `indent`.
            out.push(b'[');
            out.push(b'\n');
            for child in children {
                push_spaces(out, indent + 2);
                write_child_qdf(&child, indent + 2, out)?;
                out.push(b'\n');
            }
            push_spaces(out, indent);
            out.push(b']');
        }
        UnparseContainer::Dictionary(entries) => {
            unparse_dict_entries_qdf(&entries, indent, out)?;
        }
        UnparseContainer::Stream(stream_dict) => {
            unparse_object_walk_qdf(&stream_dict, indent, out)?;
        }
    }
    Ok(())
}

// QDF-mode sibling of `unparse_object_value` above. Only the container arms
// (`Array`, `Dictionary`, the `Stream` dictionary-inlining arm) differ from
// the plain form -- every scalar/name/string/reference arm is byte-identical
// between the two modes (the qpdf QDF writer's own fallthrough to
// `self.write_pdf(out)` for everything but its three container arms is the
// same split), so this delegates that whole fallthrough set to
// `unparse_object_value` itself rather than duplicating its match arms.
fn unparse_object_value_qdf(value: &ObjectValue, indent: usize, out: &mut Vec<u8>) -> Result<()> {
    match value {
        ObjectValue::Array(children) => {
            // qpdf's QDF array arm: `[`, a newline,
            // then per element `indent + 2` leading spaces + the child's own
            // QDF form + a trailing newline, then `indent` leading spaces and
            // `]`.
            out.push(b'[');
            out.push(b'\n');
            for child in children {
                push_spaces(out, indent + 2);
                write_child_qdf(child, indent + 2, out)?;
                out.push(b'\n');
            }
            push_spaces(out, indent);
            out.push(b']');
        }
        ObjectValue::Dictionary(entries) => {
            let entries: Vec<(Vec<u8>, ObjectHandle)> = entries
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            unparse_dict_entries_qdf(&entries, indent, out)?;
        }
        ObjectValue::Stream { stream_dict, .. } => {
            // Same reachability and "inlines only the dictionary" caveat as
            // `unparse_object_value`'s own `Stream` arm (see its doc) --
            // but note that arm's doc names `write_stream_body` (the
            // *compact* primitive) as the dedicated responsible primitive
            // for its own caveat; for *this* QDF arm the dedicated
            // primitive is `write_stream_body_qdf` instead, not that one
            // (it has no `indent` parameter and only ever produces the
            // compact single-line form). This recurses into the stream's
            // dictionary handle at the *same* `indent`, not `indent + 2` --
            // a stream dictionary is not a child sitting inside a container
            // the way an array element or dict value is; it occupies this
            // same value's own position, exactly as qpdf's QDF writer's
            // `Stream` arm calls `stream.dict.write_pdf_qdf(out, indent)` at
            // the unincremented indent before appending its
            // `stream`/`endstream` framing.
            unparse_object_walk_qdf(stream_dict, indent, out)?;
        }
        // Every remaining scalar variant has no QDF-specific
        // framing -- reuse `unparse_object_value`'s own arms for them
        // verbatim rather than duplicating scalar-formatting logic. Spelled
        // out explicitly (rather than an `other =>` catch-all) so this match
        // stays exhaustive: adding a new `ObjectValue` variant, or removing
        // one of the three container arms above, is a compile error here
        // instead of a silent fallthrough -- the same enforcement
        // `unparse_object_value` itself already gets from having no
        // catch-all arm at all.
        ObjectValue::Null
        | ObjectValue::Unresolved
        | ObjectValue::Reserved
        | ObjectValue::Destroyed
        | ObjectValue::Boolean(_)
        | ObjectValue::Integer(_)
        | ObjectValue::Real(_)
        | ObjectValue::RealLiteral { .. }
        | ObjectValue::Name(_)
        | ObjectValue::String(_)
        | ObjectValue::Operator(_)
        | ObjectValue::InlineImage(_) => unparse_object_value(value, out)?,
    }
    Ok(())
}

// QDF-mode sibling of `unparse_dict_entries` above: `<<\n`, then one
// `  /Key value\n` line per surviving entry (indented `indent + 2`, keys in
// the same lexicographic order `visible_dict_entries` preserves), then `>>`
// at column `indent` on its own line -- matches `Dictionary::write_pdf_qdf`'s
// own layout (`object.rs`) exactly, including its documented empty-dictionary
// shape (`<<\n<indent spaces>>>`) when every entry is absent or suppressed.
// Suppression itself is `visible_dict_entries`, unchanged from the plain
// path -- QDF mode does not alter *which* entries survive, only how the
// survivors are laid out. Applies the same `/Contents`-in-a-`/Sig`-dictionary
// hex-string special case `unparse_dict_entries` applies -- real qpdf's own
// guard (`QPDFWriter.cc:1497-1503`) is unconditional across `m->qdf_mode`.
fn unparse_dict_entries_qdf(
    entries: &[(Vec<u8>, ObjectHandle)],
    indent: usize,
    out: &mut Vec<u8>,
) -> Result<()> {
    out.extend_from_slice(b"<<\n");
    for (key, value) in visible_dict_entries(entries)? {
        push_spaces(out, indent + 2);
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            write_child_qdf(value, indent + 2, out)?;
        }
        out.push(b'\n');
    }
    push_spaces(out, indent);
    out.extend_from_slice(b">>");
    Ok(())
}

fn write_child_qdf_with_ref_map(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    if let Some(object_ref) = handle.object_ref() {
        if object_ref.number == 0 || removed_refs.contains(&object_ref) {
            out.extend_from_slice(b"null");
        } else {
            out.extend_from_slice(map(object_ref)?.to_string().as_bytes());
        }
        return Ok(());
    }
    unparse_object_walk_qdf_with_ref_map(handle, indent, out, map, removed_refs)
}

fn unparse_object_walk_qdf_with_ref_map(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        let container = handle.with_value(|value| match value {
            Some(value) => {
                if let Some(container) = snapshot_unparse_container(value) {
                    Ok(Some(container))
                } else {
                    unparse_object_value_qdf_with_ref_map(value, indent, out, map, removed_refs)
                        .map(|()| None)
                }
            }
            None => {
                // cov:ignore-start: after try_dereference, a live non-reserved handle cannot expose None
                out.extend_from_slice(b"null");
                Ok(None)
                // cov:ignore-end
            }
        })?;
        match container {
            Some(container) => {
                unparse_container_qdf_with_ref_map(container, indent, out, map, removed_refs)
            }
            None => Ok(()),
        }
    })
}

fn unparse_container_qdf_with_ref_map(
    container: UnparseContainer,
    indent: usize,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    match container {
        UnparseContainer::Array(children) => {
            out.push(b'[');
            out.push(b'\n');
            for child in children {
                push_spaces(out, indent + 2);
                write_child_qdf_with_ref_map(&child, indent + 2, out, map, removed_refs)?;
                out.push(b'\n');
            }
            push_spaces(out, indent);
            out.push(b']');
        }
        UnparseContainer::Dictionary(entries) => {
            unparse_dict_entries_qdf_with_ref_map(&entries, indent, out, map, removed_refs)?;
        }
        UnparseContainer::Stream(stream_dict) => {
            unparse_object_walk_qdf_with_ref_map(&stream_dict, indent, out, map, removed_refs)?;
        }
    }
    Ok(())
}

fn unparse_object_value_qdf_with_ref_map(
    value: &ObjectValue,
    _indent: usize,
    out: &mut Vec<u8>,
    _map: &ObjectRefMap<'_>,
    _removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    // `unparse_object_walk_qdf_with_ref_map` snapshots every array,
    // dictionary, and stream before entering this borrow-scoped fallback.
    // A bare reference is handled there as well, before this function is
    // called. The only values that can reach this helper are therefore
    // scalar payloads, whose QDF spelling is identical to the compact form.
    unparse_object_value(value, out)
}

fn unparse_dict_entries_qdf_with_ref_map(
    entries: &[(Vec<u8>, ObjectHandle)],
    indent: usize,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    out.extend_from_slice(b"<<\n");
    for (key, value) in visible_dict_entries(entries)? {
        if is_removed_reference(value, removed_refs) {
            continue;
        }
        push_spaces(out, indent + 2);
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            write_child_qdf_with_ref_map(value, indent + 2, out, map, removed_refs)?;
        }
        out.push(b'\n');
    }
    push_spaces(out, indent);
    out.extend_from_slice(b">>");
    Ok(())
}

fn write_child_with_ref_map_and_string_writer<F>(
    handle: &ObjectHandle,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    if let Some(object_ref) = handle.object_ref() {
        if object_ref.number == 0 || removed_refs.contains(&object_ref) {
            out.extend_from_slice(b"null");
        } else {
            out.extend_from_slice(map(object_ref)?.to_string().as_bytes());
        }
        return Ok(());
    }
    unparse_object_walk_with_ref_map_and_string_writer(handle, out, map, removed_refs, write_string)
}

fn unparse_object_walk_with_ref_map_and_string_writer<F>(
    handle: &ObjectHandle,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        let container = handle.with_value(|value| match value {
            Some(value) => {
                if let Some(container) = snapshot_unparse_container(value) {
                    Ok(Some(container))
                } else {
                    unparse_object_value_with_ref_map_and_string_writer(
                        value,
                        out,
                        map,
                        removed_refs,
                        write_string,
                    )
                    .map(|()| None)
                }
            }
            None => {
                // cov:ignore-start: after try_dereference, a live non-reserved handle cannot expose None
                out.extend_from_slice(b"null");
                Ok(None)
                // cov:ignore-end
            }
        })?;
        match container {
            Some(container) => unparse_container_with_ref_map_and_string_writer(
                container,
                out,
                map,
                removed_refs,
                write_string,
            ),
            None => Ok(()),
        }
    })
}

fn unparse_container_with_ref_map_and_string_writer<F>(
    container: UnparseContainer,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    match container {
        UnparseContainer::Array(children) => {
            out.push(b'[');
            for child in children {
                out.push(b' ');
                write_child_with_ref_map_and_string_writer(
                    &child,
                    out,
                    map,
                    removed_refs,
                    write_string,
                )?; // cov:ignore: LLVM maps the covered child call continuation to this line
            }
            out.extend_from_slice(b" ]");
        }
        UnparseContainer::Dictionary(entries) => {
            unparse_dict_entries_with_ref_map_and_string_writer(
                &entries,
                out,
                map,
                removed_refs,
                write_string,
            )?; // cov:ignore: LLVM maps the covered dictionary call continuation to this line
        }
        UnparseContainer::Stream(stream_dict) => {
            unparse_object_walk_with_ref_map_and_string_writer(
                &stream_dict,
                out,
                map,
                removed_refs,
                write_string,
            )?; // cov:ignore: LLVM maps the covered stream-dictionary call continuation to this line
        }
    }
    Ok(())
}

fn unparse_object_value_with_ref_map_and_string_writer<F>(
    value: &ObjectValue,
    out: &mut Vec<u8>,
    _map: &ObjectRefMap<'_>,
    _removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    match value {
        ObjectValue::String(bytes) => write_string(out, bytes),
        _ => unparse_object_value(value, out),
    }
}

fn unparse_dict_entries_with_ref_map_and_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    out.extend_from_slice(b"<<");
    for (key, value) in visible_dict_entries(entries)? {
        if is_removed_reference(value, removed_refs) {
            continue;
        }
        out.push(b' ');
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            continue;
        }
        write_child_with_ref_map_and_string_writer(value, out, map, removed_refs, write_string)?;
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

fn write_child_qdf_with_ref_map_and_string_writer<F>(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    if let Some(object_ref) = handle.object_ref() {
        if object_ref.number == 0 || removed_refs.contains(&object_ref) {
            out.extend_from_slice(b"null");
        } else {
            out.extend_from_slice(map(object_ref)?.to_string().as_bytes());
        }
        return Ok(());
    }
    unparse_object_walk_qdf_with_ref_map_and_string_writer(
        handle,
        indent,
        out,
        map,
        removed_refs,
        write_string,
    )
}

fn unparse_object_walk_qdf_with_ref_map_and_string_writer<F>(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
        // cov:ignore: reserved precondition closure has no independent LLVM counter
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        let container = handle.with_value(|value| match value {
            Some(value) => {
                if let Some(container) = snapshot_unparse_container(value) {
                    Ok(Some(container))
                } else {
                    unparse_object_value_qdf_with_ref_map_and_string_writer(
                        value,
                        indent,
                        out,
                        map,
                        removed_refs,
                        write_string,
                    )
                    .map(|()| None)
                }
            }
            None => {
                // cov:ignore-start: after try_dereference, a live non-reserved handle cannot expose None
                out.extend_from_slice(b"null");
                Ok(None)
                // cov:ignore-end
            }
        })?;
        match container {
            Some(container) => unparse_container_qdf_with_ref_map_and_string_writer(
                container,
                indent,
                out,
                map,
                removed_refs,
                write_string,
            ),
            None => Ok(()),
        }
    })
}

fn unparse_container_qdf_with_ref_map_and_string_writer<F>(
    container: UnparseContainer,
    indent: usize,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    match container {
        UnparseContainer::Array(children) => {
            out.push(b'[');
            out.push(b'\n');
            for child in children {
                push_spaces(out, indent + 2);
                write_child_qdf_with_ref_map_and_string_writer(
                    &child,
                    indent + 2,
                    out,
                    map,
                    removed_refs,
                    write_string,
                )?; // cov:ignore: LLVM maps the covered child call continuation to this line
                out.push(b'\n');
            }
            push_spaces(out, indent);
            out.push(b']');
        }
        UnparseContainer::Dictionary(entries) => {
            unparse_dict_entries_qdf_with_ref_map_and_string_writer(
                &entries,
                indent,
                out,
                map,
                removed_refs,
                write_string,
            )?; // cov:ignore: LLVM maps the covered dictionary call continuation to this line
        }
        UnparseContainer::Stream(stream_dict) => {
            unparse_object_walk_qdf_with_ref_map_and_string_writer(
                &stream_dict,
                indent,
                out,
                map,
                removed_refs,
                write_string,
            )?; // cov:ignore: LLVM maps the covered stream-dictionary call continuation to this line
        }
    }
    Ok(())
}

fn unparse_object_value_qdf_with_ref_map_and_string_writer<F>(
    value: &ObjectValue,
    _indent: usize,
    out: &mut Vec<u8>,
    _map: &ObjectRefMap<'_>,
    _removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    match value {
        ObjectValue::String(bytes) => write_string(out, bytes),
        _ => unparse_object_value(value, out),
    }
}

fn unparse_dict_entries_qdf_with_ref_map_and_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    indent: usize,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    out.extend_from_slice(b"<<\n");
    for (key, value) in visible_dict_entries(entries)? {
        if is_removed_reference(value, removed_refs) {
            continue;
        }
        push_spaces(out, indent + 2);
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            out.push(b'\n');
            continue;
        }
        write_child_qdf_with_ref_map_and_string_writer(
            value,
            indent + 2,
            out,
            map,
            removed_refs,
            write_string,
        )?; // cov:ignore: LLVM maps the covered mapped dictionary child call continuation to this line
        out.push(b'\n');
    }
    push_spaces(out, indent);
    out.extend_from_slice(b">>");
    Ok(())
}

#[cfg(test)]
fn write_child_with_string_writer<F>(
    handle: &ObjectHandle,
    out: &mut Vec<u8>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    if let Some(object_ref) = handle.object_ref() {
        out.extend_from_slice(object_ref.to_string().as_bytes());
        return Ok(());
    }
    unparse_object_walk_with_string_writer(handle, out, write_string)
}

#[cfg(test)]
fn unparse_container_with_string_writer<F>(
    container: UnparseContainer,
    out: &mut Vec<u8>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    match container {
        UnparseContainer::Array(children) => {
            out.push(b'[');
            for child in children {
                out.push(b' ');
                write_child_with_string_writer(&child, out, write_string)?;
            }
            out.extend_from_slice(b" ]");
        }
        UnparseContainer::Dictionary(entries) => {
            unparse_dict_entries_with_string_writer(&entries, out, write_string)?;
        }
        UnparseContainer::Stream(stream_dict) => {
            unparse_object_walk_with_string_writer(&stream_dict, out, write_string)?;
        }
    }
    Ok(())
}

#[cfg(test)]
fn unparse_object_walk_with_string_writer<F>(
    handle: &ObjectHandle,
    out: &mut Vec<u8>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        let container = handle.with_value(|value| match value {
            Some(value) => {
                if let Some(container) = snapshot_unparse_container(value) {
                    Ok(Some(container))
                } else {
                    unparse_object_value_with_string_writer(value, out, write_string).map(|()| None)
                }
            }
            None => {
                // cov:ignore-start: successful dereference exposes Null for
                // the null fallback or errors while unresolved.
                out.extend_from_slice(b"null");
                Ok(None)
                // cov:ignore-end
            }
        })?;
        match container {
            Some(container) => unparse_container_with_string_writer(container, out, write_string),
            None => Ok(()),
        }
    })
}

#[cfg(test)]
fn unparse_object_value_with_string_writer<F>(
    value: &ObjectValue,
    out: &mut Vec<u8>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    match value {
        ObjectValue::String(bytes) => write_string(out, bytes),
        _ => unparse_object_value(value, out),
    }
}

#[cfg(test)]
fn try_write_sig_contents_with_string_writer(
    handle: &ObjectHandle,
    force_hex_string: bool,
    out: &mut Vec<u8>,
) -> Result<bool> {
    if !force_hex_string || handle.object_ref().is_some() {
        return Ok(false);
    }
    handle.try_dereference()?;
    let bytes = handle.with_value(|value| match value {
        Some(ObjectValue::String(bytes)) => Some(bytes.clone()),
        _ => None,
    });
    let Some(bytes) = bytes else {
        return Ok(false);
    };
    // QPDFWriter.cc:1501 adds f_no_encryption together with f_hex_string for
    // signature contents. The ordinary string callback is therefore bypassed
    // here: qpdf keeps this value cleartext and only changes its spelling to
    // hexadecimal, even while the surrounding object is encrypted.
    crate::pdf_syntax::write_hex_string(out, &bytes);
    Ok(true)
}

#[cfg(test)]
fn unparse_dict_entries_with_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    out: &mut Vec<u8>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    out.extend_from_slice(b"<<");
    for (key, value) in visible_dict_entries(entries)? {
        out.push(b' ');
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if try_write_sig_contents_with_string_writer(value, force_hex_string, out)? {
            continue;
        }
        write_child_with_string_writer(value, out, write_string)?;
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

#[cfg(test)]
fn write_child_qdf_with_string_writer<F>(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut Vec<u8>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    if let Some(object_ref) = handle.object_ref() {
        out.extend_from_slice(object_ref.to_string().as_bytes());
        return Ok(());
    }
    unparse_object_walk_qdf_with_string_writer(handle, indent, out, write_string)
}

#[cfg(test)]
fn unparse_container_qdf_with_string_writer<F>(
    container: UnparseContainer,
    indent: usize,
    out: &mut Vec<u8>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    match container {
        UnparseContainer::Array(children) => {
            out.push(b'[');
            out.push(b'\n');
            for child in children {
                push_spaces(out, indent + 2);
                write_child_qdf_with_string_writer(&child, indent + 2, out, write_string)?;
                out.push(b'\n');
            }
            push_spaces(out, indent);
            out.push(b']');
        }
        UnparseContainer::Dictionary(entries) => {
            unparse_dict_entries_qdf_with_string_writer(&entries, indent, out, write_string)?;
        }
        UnparseContainer::Stream(stream_dict) => {
            unparse_object_walk_qdf_with_string_writer(&stream_dict, indent, out, write_string)?;
        }
    }
    Ok(())
}

#[cfg(test)]
fn unparse_object_walk_qdf_with_string_writer<F>(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut Vec<u8>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        let container = handle.with_value(|value| match value {
            Some(value) => {
                if let Some(container) = snapshot_unparse_container(value) {
                    Ok(Some(container))
                } else {
                    unparse_object_value_qdf_with_string_writer(value, indent, out, write_string)
                        .map(|()| None)
                }
            }
            None => {
                // cov:ignore-start: successful dereference exposes Null for
                // the null fallback or errors while unresolved.
                out.extend_from_slice(b"null");
                Ok(None)
                // cov:ignore-end
            }
        })?;
        match container {
            Some(container) => {
                unparse_container_qdf_with_string_writer(container, indent, out, write_string)
            }
            None => Ok(()),
        }
    })
}

#[cfg(test)]
fn unparse_object_value_qdf_with_string_writer<F>(
    value: &ObjectValue,
    _indent: usize,
    out: &mut Vec<u8>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    match value {
        ObjectValue::String(bytes) => write_string(out, bytes),
        _ => unparse_object_value(value, out),
    }
}

#[cfg(test)]
fn unparse_dict_entries_qdf_with_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    indent: usize,
    out: &mut Vec<u8>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
{
    out.extend_from_slice(b"<<\n");
    for (key, value) in visible_dict_entries(entries)? {
        push_spaces(out, indent + 2);
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if try_write_sig_contents_with_string_writer(value, force_hex_string, out)? {
            out.push(b'\n');
            continue;
        }
        write_child_qdf_with_string_writer(value, indent + 2, out, write_string)?;
        out.push(b'\n');
    }
    push_spaces(out, indent);
    out.extend_from_slice(b">>");
    Ok(())
}

// `write_trailer`'s sole callee. Writes the (already-trimmed,
// already-/Size-correct -- see that method's doc) entries in an
// unconditional loop -- no `visible_dict_entries` call, deliberately: this
// is the one dictionary-shaped writer-emission primitive in this family
// that does not suppress null-valued keys, matching `writeTrailer`'s own
// key loop (`QPDFWriter.cc:1174-1192`), which has no `isNull` check
// anywhere in it. Also has no `/Contents`-in-a-`/Sig`-dictionary hex-string
// special case, deliberately: that guard lives in `unparseObject`'s
// dictionary branch alone (`QPDFWriter.cc:1490-1504`), a different loop
// `writeTrailer`'s own key loop never calls into -- `writeTrailer` calls
// `unparseChild(trailer.getKey(key), 1, 0)` directly for every non-`/Size`
// key (`:1188`), and a trailer is never itself a signature dictionary in
// any case.
#[cfg(test)]
fn unparse_trailer_entries(
    entries: &[(Vec<u8>, ObjectHandle)],
    xref_stream: bool,
    mut id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
    out: &mut Vec<u8>,
) -> Result<()> {
    if !xref_stream {
        out.extend_from_slice(b"trailer <<");
    }
    let mut id_value: Option<&ObjectHandle> = None;
    let mut encrypt_value: Option<&ObjectHandle> = None;
    for (key, value) in entries {
        match key.as_slice() {
            b"/ID" => {
                id_value = Some(value);
                continue;
            }
            b"/Encrypt" => {
                encrypt_value = Some(value);
                continue;
            }
            _ => {}
        }
        out.push(b' ');
        write_dictionary_key(out, key);
        out.push(b' ');
        write_child(value, out)?;
    }
    if let Some(value) = id_value {
        out.extend_from_slice(b" /ID ");
        match id_writer.as_mut() {
            Some(write_id) => write_id(out),
            None => write_id_style_value_handle(value, out)?,
        }
    }
    if let Some(value) = encrypt_value {
        out.extend_from_slice(b" /Encrypt ");
        write_child(value, out)?;
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

#[allow(clippy::too_many_arguments)] // mirrors writeTrailer's independent layout, ID, mapping, and visibility controls
fn unparse_trailer_entries_with_ref_map(
    entries: &[(Vec<u8>, ObjectHandle)],
    xref_stream: bool,
    qdf: bool,
    mut id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
    map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
    suppress_null_values: bool,
    out: &mut Vec<u8>,
) -> Result<()> {
    if qdf {
        out.extend_from_slice(b"trailer <<\n");
    } else if !xref_stream {
        out.extend_from_slice(b"trailer <<");
    }

    let mut id_value: Option<&ObjectHandle> = None;
    let mut encrypt_value: Option<&ObjectHandle> = None;
    for (key, value) in entries {
        // `/ID` and `/Encrypt` are installed by the writer in output space.
        // They must not be discarded because their output reference happens
        // to reuse an object number recorded in the source-side removal set.
        // qpdf's writeTrailer handles these writer-owned keys separately from
        // the ordinary trailer-child filtering (`QPDFWriter.cc:1174-1192`).
        match key.as_slice() {
            b"/ID" => {
                id_value = Some(value);
                continue;
            }
            b"/Encrypt" => {
                encrypt_value = Some(value);
                continue;
            }
            _ => {}
        }
        if suppress_null_values && value.try_is_null()? {
            continue;
        }
        if is_removed_reference(value, removed_refs) {
            continue;
        }

        if qdf {
            out.extend_from_slice(b"  ");
        } else {
            out.push(b' ');
        }
        write_dictionary_key(out, key);
        out.push(b' ');
        if key.as_slice() == b"/Root" && value.object_ref().is_none() {
            // An inline Catalog is writer-owned, but its indirect descendants
            // remain in source space until this final child walk. qpdf's
            // `unparseChild` recurses into that direct dictionary, so preserve
            // the direct `/Root` shape while applying the caller's map below.
            if qdf {
                write_child_qdf_with_ref_map(value, 2, out, map, removed_refs)?;
            } else {
                write_child_with_ref_map(value, out, map, removed_refs)?;
            }
        } else if matches!(key.as_slice(), b"/Root" | b"/Encrypt") {
            // An indirect `/Root` or `/Encrypt` installed by the writer already
            // carries an output-space reference and must not be remapped again.
            if qdf {
                write_child_qdf(value, 2, out)?;
            } else {
                write_child(value, out)?;
            }
        } else if qdf {
            write_child_qdf_with_ref_map(value, 2, out, map, removed_refs)?;
        } else {
            write_child_with_ref_map(value, out, map, removed_refs)?;
        }
        if qdf {
            out.push(b'\n');
        }
    }

    if let Some(value) = id_value {
        if qdf {
            out.extend_from_slice(b"  /ID ");
        } else {
            out.extend_from_slice(b" /ID ");
        }
        match id_writer.as_mut() {
            Some(write_id) => write_id(out),
            None => write_id_style_value_handle_with_ref_map(value, out, map, removed_refs)?,
        }
    }
    if let Some(value) = encrypt_value {
        out.extend_from_slice(b" /Encrypt ");
        write_child(value, out)?;
    }

    if qdf {
        if id_value.is_some() || encrypt_value.is_some() {
            out.push(b'\n');
        }
        out.extend_from_slice(b">>\n");
    } else {
        out.extend_from_slice(b" >>");
    }
    Ok(())
}

#[cfg(test)]
fn unparse_dictionary_entries_with_ref_map_and_id_writer(
    entries: &[(Vec<u8>, ObjectHandle)],
    mut id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
    map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
    suppress_null_values: bool,
    out: &mut Vec<u8>,
) -> Result<()> {
    out.extend_from_slice(b"<<");
    for (key, value) in entries {
        if suppress_null_values && value.try_is_null()? {
            continue;
        }
        if is_removed_reference(value, removed_refs) {
            continue;
        }
        out.push(b' ');
        write_dictionary_key(out, key);
        out.push(b' ');
        if key.as_slice() == b"/ID" {
            match id_writer.as_mut() {
                Some(write_id) => write_id(out),
                None => write_id_style_value_handle_with_ref_map(value, out, map, removed_refs)?,
            }
        } else if matches!(key.as_slice(), b"/Root" | b"/Encrypt") {
            write_child(value, out)?;
        } else {
            write_child_with_ref_map(value, out, map, removed_refs)?;
        }
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

// Writes a trailer's `/ID` value in qpdf's `writeTrailer` compact shape:
// `[<hex1><hex2>]`, no spaces (`QPDFWriter.cc:1194-1222`, `/ID [` then the
// two identifier strings via `QPDF_String::unparse(true)`, then `]`).
// Mirrors qpdf's identifier writer byte-for-byte, but walks `value`'s own
// `ObjectHandle` shape directly: an indirect `value` (an `/ID` array stored as
// a reference -- not a shape real qpdf itself ever produces, but nothing
// at the type level rules it out) writes as its own `"N G R"` form via
// `write_child`, checked before any shape inspection, matching
// `write_child`'s own reference-vs-recurse split and never inlining an
// indirect value regardless of what it resolves to. A direct
// `Array([String, String])` gets the compact hex-pair form; any other
// direct shape (wrong arity, non-string elements) falls back to
// `write_child`'s generic form rather than silently truncating -- the
// same "fall back, don't truncate" choice `write_id_style_value` makes.
#[cfg(test)]
fn write_id_style_value_handle(value: &ObjectHandle, out: &mut Vec<u8>) -> Result<()> {
    if value.object_ref().is_some() {
        return write_child(value, out);
    }
    let compact: Option<(Vec<u8>, Vec<u8>)> = value.with_value(|v| match v {
        Some(ObjectValue::Array(items)) if items.len() == 2 => {
            let string_bytes = |item: &ObjectHandle| {
                item.with_value(|iv| match iv {
                    Some(ObjectValue::String(s)) => Some(s.clone()),
                    _ => None,
                })
            };
            match (string_bytes(&items[0]), string_bytes(&items[1])) {
                (Some(b0), Some(b1)) => Some((b0, b1)),
                _ => None,
            }
        }
        _ => None,
    });
    match compact {
        Some((b0, b1)) => {
            out.push(b'[');
            crate::pdf_syntax::write_hex_string(out, &b0);
            crate::pdf_syntax::write_hex_string(out, &b1);
            out.push(b']');
            Ok(())
        }
        None => write_child(value, out),
    }
}

fn write_id_style_value_handle_with_ref_map(
    value: &ObjectHandle,
    out: &mut Vec<u8>,
    map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    if value.object_ref().is_some() {
        return write_child_with_ref_map(value, out, map, removed_refs);
    }
    let compact: Option<(Vec<u8>, Vec<u8>)> = value.with_value(|v| match v {
        Some(ObjectValue::Array(items)) if items.len() == 2 => {
            let string_bytes = |item: &ObjectHandle| {
                item.with_value(|iv| match iv {
                    Some(ObjectValue::String(s)) => Some(s.clone()),
                    _ => None,
                })
            };
            match (string_bytes(&items[0]), string_bytes(&items[1])) {
                (Some(b0), Some(b1)) => Some((b0, b1)),
                _ => None,
            }
        }
        _ => None,
    });
    match compact {
        Some((b0, b1)) => {
            out.push(b'[');
            crate::pdf_syntax::write_hex_string(out, &b0);
            crate::pdf_syntax::write_hex_string(out, &b1);
            out.push(b']');
            Ok(())
        }
        None => unparse_object_walk_with_ref_map(value, out, map, removed_refs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn dictionary_key_writer_preserves_qpdfs_first_byte() {
        let mut out = Vec::new();
        write_dictionary_key(&mut out, b"/Canonical");
        out.push(b' ');
        write_dictionary_key(&mut out, b"Raw");
        assert_eq!(out, b"/Canonical Raw");
    }

    #[test]
    fn dictionary_ref_map_writer_accepts_a_trailer_id_callback() -> Result<()> {
        let dictionary = ObjectHandle::dictionary(vec![
            (
                b"/ID".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::string(b"old-a".to_vec()),
                    ObjectHandle::string(b"old-b".to_vec()),
                ]),
            ),
            (b"/Keep".to_vec(), ObjectHandle::integer(1)),
        ]);
        let mut output = Vec::new();
        let mut write_id = |out: &mut Vec<u8>| out.extend_from_slice(b"[<01><02>]");
        let map = |object_ref: ObjectRef| Ok(object_ref);
        dictionary.write_dictionary_with_ref_map_and_id_writer(
            &mut output,
            Some(&mut write_id),
            &map,
            &BTreeSet::new(),
            false,
        )?; // cov:ignore: LLVM maps this successful generic dictionary call continuation to test cleanup
        assert_eq!(output, b"<< /ID [<01><02>] /Keep 1 >>");
        Ok(())
    }

    #[test]
    fn mapped_stream_writer_overrides_length_and_suppresses_null_children() -> Result<()> {
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"/Length".to_vec(), ObjectHandle::integer(99)),
                (b"/Null".to_vec(), ObjectHandle::null()),
                (b"/Keep".to_vec(), ObjectHandle::integer(1)),
            ]),
            Rc::new(b"abc".to_vec()),
        );
        let mut output = Vec::new();
        let removed = BTreeSet::new();
        let map = |object_ref: ObjectRef| Ok(object_ref);

        stream.write_stream_body_with_ref_map_and_removed_and_length(
            &mut output,
            false,
            &map,
            &removed,
            3,
        )?; // cov:ignore: LLVM attributes this successful stream-emission continuation to the test call cleanup.

        assert_eq!(output, b"<< /Keep 1 /Length 3 >>");

        let dictionary =
            ObjectHandle::dictionary(vec![(b"/Keep".to_vec(), ObjectHandle::integer(1))]);
        let mut dictionary_output = Vec::new();
        dictionary.write_stream_body_with_ref_map_and_removed_and_length(
            &mut dictionary_output,
            false,
            &map,
            &removed,
            2,
        )?; // cov:ignore: LLVM attributes this successful dictionary-emission continuation to the test call cleanup.
        assert_eq!(dictionary_output, b"<< /Keep 1 /Length 2 >>");

        let scalar = ObjectHandle::integer(1);
        let mut scalar_output = Vec::new();
        scalar.write_stream_body_with_ref_map_and_removed_and_length(
            &mut scalar_output,
            false,
            &map,
            &removed,
            1,
        )?; // cov:ignore: LLVM attributes this successful scalar-emission continuation to the test call cleanup.
        assert_eq!(scalar_output, b"<< /Length 1 >>");

        let malformed_stream =
            ObjectHandle::stream(ObjectHandle::integer(1), Rc::new(b"x".to_vec()));
        let mut malformed_output = Vec::new();
        malformed_stream.write_stream_body_with_ref_map_and_removed_and_length(
            &mut malformed_output,
            false,
            &map,
            &removed,
            1,
        )?; // cov:ignore: LLVM attributes this successful malformed-stream fallback continuation to test cleanup.
        assert_eq!(malformed_output, b"<< /Length 1 >>");

        let reserved = ObjectHandle::new_reserved_direct();
        let error = reserved
            .write_stream_body_with_ref_map_and_removed_and_length(
                &mut Vec::new(),
                false,
                &map,
                &removed,
                0,
            )
            .expect_err("reserved stream emission must be rejected");
        assert!(error.to_string().contains("reserved object"));
        Ok(())
    }
}

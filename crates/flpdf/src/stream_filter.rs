//! qpdf correspondence: QPDFStreamFilter.cc and QPDF_Stream.cc filter-name, DecodeParms-alignment, and decode-pipeline construction responsibilities, read from either Object-shaped or ObjectHandle-shaped /Filter and /DecodeParms values.
//!
//! # Known Crypt-parity gap: no `SF_Crypt::setDecodeParms` validation
//!
//! qpdf refuses a `Crypt` stage whose `/DecodeParms` carry any key other than
//! `/Type` or `/Name`, and validates a present `/Type` through
//! `isDictionaryOfType("/CryptFilterDecodeParms")` — the whole body of
//! `SF_Crypt::setDecodeParms` (`libqpdf/QPDF_Stream.cc:33-50`). It returns
//! `false` on anything else, which `QPDF_Stream::filterable` turns into its
//! own `filterable = false` at `:471`/`:479-481`.
//!
//! flpdf reproduces none of that. `filters::prepare_decode_filters` peels a
//! `Crypt` spec off into `PreparedStage::Crypt` without inspecting its
//! parameters at all, and the installed crypt provider then decides the
//! outcome — for every non-decrypting entry point that provider is
//! `filters::reject_crypt_stage`, which errors unconditionally, so no shape of
//! `/DecodeParms` is currently distinguishable there. This belongs to the
//! Phase 3 AES/Crypt cutover, where a provider that can succeed makes the
//! difference observable; it is recorded here rather than fixed, and nothing
//! in `flpdf-25kg.3.4` changes Crypt semantics.
//!
//! # Recorded deviation: `DecodeParams` is an owned, reduced snapshot
//!
//! qpdf replicates one `QPDFObjectHandle` — a `shared_ptr` — across the filter
//! chain and copies no dictionary. [`DecodeParams`] owns its entries instead,
//! so it retains only what some consumer reads:
//! [`RETAINED_DECODE_PARAM_KEYS`] under every filter,
//! [`CRYPT_RETAINED_DECODE_PARAM_KEY`] under `Crypt` alone, and a name's bytes
//! only under that one key ([`is_crypt_name_key`]) — elsewhere a name reduces
//! to [`ParamValue::Other`], which no consumer can tell apart. Output bytes,
//! filterability and error timing are unaffected — `SF_FlateLzwDecode`'s key
//! walk (`libqpdf/SF_FlateLzwDecode.cc:32-66`) has no `else` arm, so a key it
//! does not name never reaches its `filterable`, and nothing reconstructs an
//! emitted `/DecodeParms` from this type; the writer copies the source
//! dictionary.

use crate::object_handle::ObjectHandle;
use crate::pipeline::ascii85::Ascii85Decoder;
use crate::pipeline::ascii_hex::AsciiHexDecoder;
use crate::pipeline::buffer::Buffer;
use crate::pipeline::flate::{Flate, FlateAction, DEFAULT_OUT_BUFFER_SIZE};
use crate::pipeline::lzw::LzwDecoder;
use crate::pipeline::png_filter::{PngFilter, PngFilterAction};
use crate::pipeline::run_length::{RunLength, RunLengthAction};
use crate::pipeline::{Pipeline, PipelineError, PipelineRef, PipelineResult};
use crate::{Error, Object, Result};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

pub(crate) const DECODE_OUTPUT_LIMIT_PREFIX: &str = "decoded output exceeds configured limit of";

/// Bounded `/DecodeParms` view: everything `StreamFilter::set_decode_params`
/// needs, with no `Object` or `ObjectHandle` left in it.
///
/// `Absent` covers both a missing key and an explicit null, matching
/// `QPDF_Stream::filterable`'s treatment of a null `/DecodeParms` and
/// `SF_FlateLzwDecode::setDecodeParms`'s sole early return
/// (`SF_FlateLzwDecode.cc:24-26`).
/// `Present` carries the dictionary's entries in iteration order; a present
/// non-dictionary yields `Present` with no entries, which is what qpdf sees:
/// `setDecodeParms` asks `QPDFObjectHandle::getKeys`
/// (`QPDFObjectHandle.cc:997-1009`) for every non-null object, and it is
/// *`getKeys`* — not `setDecodeParms` — that warns
/// `typeWarning("dictionary", "treating as empty")` (`:1005`) and hands back
/// an empty key set.
///
/// **Entry order is not part of this type's contract.** qpdf iterates
/// `getKeys()`'s `std::set<std::string>`, so it sees keys sorted. The `Object`
/// shape reader agrees today by construction rather than by intent:
/// `Dictionary` is a `BTreeMap` keyed by the raw name bytes, and because
/// qpdf's keys differ only by a uniform leading `/`, the two orderings
/// coincide. A handle-shaped reader entering through
/// `ObjectHandle::as_dictionary` — a `BTreeMap` over the same keys — would
/// coincide for the same reason. A `Vec` cannot state any of that, and nothing
/// needs it to: every filter assigns each key independently and runs its only
/// cross-key check after the loop (`SF_FlateLzwDecode.cc:68-70`). A future
/// Crypt provider that reads [`Self::entries`] order-dependently would be the
/// first code to care.
#[derive(Debug, PartialEq)]
pub(crate) enum DecodeParams {
    Absent,
    Present(Vec<(Vec<u8>, ParamValue)>),
}

/// A `/DecodeParms` value reduced to the bounded scalars any filter reads.
///
/// **Invariant:** `Int` appears exactly where `QPDFObjectHandle::isInteger`
/// admits a value — `obj->getTypeCode() == ::ot_integer`
/// (`QPDFObjectHandle.cc:358-362`) — carrying it already put through
/// `getIntValueAsInt`'s both-ends saturation (`:526-543`). `Name` and `Other`
/// are qpdf's `else` branch: `SF_FlateLzwDecode::setDecodeParms` reaches
/// `getIntValueAsInt` only behind an `isInteger()` guard
/// (`SF_FlateLzwDecode.cc:33`, `:43`, `:56`) and sets `filterable = false`
/// otherwise, so a filter matching on those two variants reproduces that
/// branch without re-inspecting the value.
///
/// `isInteger` also `dereference()`s first, and honoring that is each shape
/// reader's job rather than this type's: `param_value_from_object` classifies
/// whatever `Object` it is handed, so an indirect integer inside a
/// `/DecodeParms` dictionary reduces to `Other` there. The handle-shaped
/// reader closes that gap (plan decision D1 of `flpdf-25kg.3.4`) **for the
/// filters that read parameter entries at all** — qpdf dereferences a value
/// only through `SF_FlateLzwDecode`'s `getKeys()`/`getKey()` walk, so for
/// every other filter `decode_params_from_entries` reads the value without
/// resolving. See [`filter_reads_decode_params`], and
/// [`param_value_without_resolving`] for what such a value classifies as and
/// why that classification is unobservable. This enum is the same either way.
///
/// The `Name`/`Other` split is flpdf's, not qpdf's: `Name` exists for `Crypt`'s
/// `/Name`, which selects the crypt filter, so carrying it now keeps Phase 3's
/// AES/Crypt cutover from having to widen this shared type. Every
/// `StreamFilter` still treats the two identically; only the Crypt stage's
/// provider closure is positioned to tell them apart.
///
/// **`Name` therefore carries a payload only where that provider reads one —
/// the `/Name` key of a `Crypt` stage** ([`is_crypt_name_key`]). A name under
/// any other retained key reduces to `Other`: `/Columns /Identity` is `Other`,
/// not `Name(b"Identity")`. Nothing can tell the difference, because the only
/// production match on either variant is `set_decode_params`' shared
/// `ParamValue::Name(_) | ParamValue::Other => filterable = false` arm, and
/// qpdf's counterpart asks `isInteger()` and never inspects a non-integer's
/// kind. Keeping the payload anyway is what made a `DecodeParams` grow with
/// its input — see [`RETAINED_DECODE_PARAM_KEYS`]' bound.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ParamValue {
    Int(i32),
    Name(Vec<u8>),
    Other,
}

impl DecodeParams {
    pub(crate) fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    pub(crate) fn entries(&self) -> &[(Vec<u8>, ParamValue)] {
        match self {
            Self::Absent => &[],
            Self::Present(entries) => entries,
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct FilterSpec {
    pub(crate) name: Vec<u8>,
    pub(crate) decode_params: DecodeParams,
}

impl FilterSpec {
    pub(crate) fn normalized_name(&self) -> &[u8] {
        normalize_filter_name(&self.name)
    }
}

pub(crate) fn normalize_filter_name(name: &[u8]) -> &[u8] {
    match name {
        b"Fl" => b"FlateDecode",
        b"LZW" => b"LZWDecode",
        b"A85" => b"ASCII85Decode",
        b"AHx" => b"ASCIIHexDecode",
        b"RL" => b"RunLengthDecode",
        b"CCF" => b"CCITTFaxDecode",
        b"DCT" => b"DCTDecode",
        name => name,
    }
}

/// `QPDF_Stream::filterable`'s `warn("stream filter type is not name or
/// array")` (`libqpdf/QPDF_Stream.cc:413`). flpdf raises the same text as an
/// error instead of a warning; see plan decision D3 of `flpdf-25kg.3.4`.
const FILTER_TYPE_ERROR: &str = "stream filter type is not name or array";

/// `QPDF_Stream::filterable`'s `warn("stream /DecodeParms length is
/// inconsistent with filters")` (`libqpdf/QPDF_Stream.cc:459`), raised as an
/// error rather than a warning just as [`FILTER_TYPE_ERROR`] is.
///
/// **flpdf reaches this check on inputs qpdf does not.** qpdf validates every
/// filter name against `filter_factories` first and returns on an unknown one
/// (`QPDF_Stream.cc:433-435`), so `:459`'s condition is never evaluated for a
/// stream whose `/Filter` names an unimplemented codec. Neither flpdf shape
/// reader consults filter-name validity at all — that is
/// `filters::prepare_decode_filters`' job, downstream of [`FilterSpec`] — so
/// an unknown filter combined with a misaligned `/DecodeParms` reports the
/// length error here where qpdf would report the unknown filter. Tracked as
/// beads `flpdf-vatj` (P2); not fixed here, because Task 5's contract is that
/// the two readers keep the same branch order. Both readers diverge from qpdf
/// *identically*, so the legacy-vs-native equivalence gate stays valid.
const DECODE_PARMS_LENGTH_ERROR: &str = "stream /DecodeParms length is inconsistent with filters";

/// Reject a `/Filter` chain longer than `maximum`.
///
/// Unlike qpdf, which caps nothing here, flpdf refuses pathological chains on
/// the decode path; `filters::MAX_FILTER_CHAIN_LEN` documents that divergence.
///
/// Both shape readers call this, so the cap's *body* — the comparison and the
/// message — has exactly one definition. Its *position* does not: each reader
/// hand-places two calls (one inside the `/Filter` array arm, ahead of
/// per-item validation; one after the `/DecodeParms` branch), and
/// `filters::validate_filter_chain_len` places a fifth. Nothing structural
/// keeps those placements aligned — only
/// `handle_reader_matches_object_reader_for_every_filter_shape`, which sweeps
/// the corpus at `None`, `Some(16)`, and `Some(0)` and fails if either
/// reader's placement drifts.
pub(crate) fn validate_filter_chain_count(count: usize, maximum: Option<usize>) -> Result<()> {
    if let Some(maximum) = maximum.filter(|maximum| count > *maximum) {
        return Err(Error::Unsupported(format!(
            "filter chain length {count} exceeds maximum of {maximum}"
        )));
    }
    Ok(())
}

pub(crate) fn decode_filter_specs_from_object(
    filter: Option<&Object>,
    decode_params: Option<&Object>,
    max_filter_chain: Option<usize>,
) -> Result<Vec<FilterSpec>> {
    let names: Vec<&[u8]> = match filter {
        None | Some(Object::Null) => return Ok(Vec::new()),
        Some(Object::Name(name)) => vec![name],
        Some(Object::Array(items)) => {
            // Counted on the array as parsed, so an over-long chain is
            // reported ahead of a malformed item or a `/DecodeParms` length
            // mismatch.
            validate_filter_chain_count(items.len(), max_filter_chain)?;
            items
                .iter()
                .map(|item| {
                    item.as_name()
                        .ok_or_else(|| Error::Unsupported(FILTER_TYPE_ERROR.to_string()))
                })
                .collect::<Result<_>>()?
        }
        Some(_) => return Err(Error::Unsupported(FILTER_TYPE_ERROR.to_string())),
    };

    if names.is_empty() {
        return Ok(Vec::new());
    }

    let params = match decode_params {
        None | Some(Object::Null) => vec![None; names.len()],
        Some(Object::Array(items)) if items.is_empty() => vec![None; names.len()],
        Some(Object::Array(items)) => {
            if items.len() != names.len() {
                return Err(Error::Unsupported(DECODE_PARMS_LENGTH_ERROR.to_string()));
            }
            items
                .iter()
                .map(|item| (!matches!(item, Object::Null)).then_some(item))
                .collect()
        }
        Some(item) => vec![Some(item); names.len()],
    };

    validate_filter_chain_count(names.len(), max_filter_chain)?;

    Ok(names
        .into_iter()
        .zip(params)
        .map(|(name, decode_params)| FilterSpec {
            name: name.to_vec(),
            decode_params: decode_params_from_object(decode_params, name),
        })
        .collect())
}

/// The same read as [`decode_filter_specs_from_object`], entered through the
/// resolving `try_*` accessors.
///
/// `QPDF_Stream::filterable` reaches `/Filter` and `/DecodeParms` through
/// `stream_dict.getKey` (`libqpdf/QPDF_Stream.cc:386`, `:441`) and their
/// members through `getArrayItem` (`:400`, `:448`), then inspects each with
/// `isNull`/`isName`/`isArray`/`isInteger` — every one of which dereferences
/// through the owning `QPDF` first. So an indirect child is read as the object
/// it points at, which a `&Object` walk cannot do and which the 2026-08-03
/// live-qpdf probe recorded in plan decision D1 of `flpdf-25kg.3.4`.
///
/// That is unconditional for `/Filter`, each `/Filter` array item, the
/// `/DecodeParms` handle, and each `/DecodeParms` array item — every position
/// `QPDF_Stream::filterable` itself inspects. It is *conditional* one level
/// deeper: a `/DecodeParms` dictionary **value** is reached only by
/// `SF_FlateLzwDecode::setDecodeParms`, so this reader resolves one only when
/// [`filter_reads_decode_params`] holds for that spec's filter.
///
/// A missing key arrives here as a null handle, exactly as `getKey` hands one
/// back (`libqpdf/QPDFObjectHandle.cc:979-988`), so absent and null share the
/// `isNull` branch just as they do in qpdf.
///
/// This deliberately does not share a body with the `Object` reader: the two
/// differ only in *how* a value is inspected, and hiding that behind a trait
/// would reintroduce the shape wrapper this seam exists to avoid. Everything
/// downstream of [`FilterSpec`] — the codec stack, predictor geometry, limits,
/// and warning ordering — stays a single copy.
#[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
pub(crate) fn decode_filter_specs_from_handle(
    filter: &ObjectHandle,
    decode_params: &ObjectHandle,
    max_filter_chain: Option<usize>,
) -> Result<Vec<FilterSpec>> {
    let names: Vec<Vec<u8>> = if filter.try_is_null()? {
        return Ok(Vec::new());
    } else if let Some(name) = filter.try_as_name()? {
        vec![name]
    } else if let Some(count) = filter.try_array_len()? {
        // Counted through `try_array_len`, not `try_as_array`, so a chain the
        // cap is about to reject is never snapshotted — qpdf sizes this loop
        // with `getArrayNItems` (`libqpdf/QPDF_Stream.cc:398`), which reads
        // the length off the borrowed array in place. The snapshot below
        // therefore only happens once the count is known to be acceptable.
        validate_filter_chain_count(count, max_filter_chain)?;
        // `try_array_len` already answered `Some`, so this cannot be `None`;
        // `flatten` states that without a panicking `expect`.
        filter
            .try_as_array()?
            .into_iter()
            .flatten()
            .map(|item| {
                item.try_as_name()?
                    .ok_or_else(|| Error::Unsupported(FILTER_TYPE_ERROR.to_string()))
            })
            .collect::<Result<_>>()?
    } else {
        return Err(Error::Unsupported(FILTER_TYPE_ERROR.to_string()));
    };

    if names.is_empty() {
        return Ok(Vec::new());
    }

    let params: Vec<DecodeParams> = if decode_params.try_is_null()? {
        absent_params(names.len())
    } else if let Some(count) = decode_params.try_array_len()? {
        // Same length-before-snapshot shape as the `/Filter` arm: qpdf sizes
        // this loop with `getArrayNItems` as well (`libqpdf/QPDF_Stream.cc:443`
        // for the empty-array reduction, `:447` for the per-index walk), and
        // both the empty reduction and the length mismatch are decided from
        // the count alone — so a mismatched array is rejected without being
        // snapshotted.
        if count == 0 {
            absent_params(names.len())
        } else {
            if count != names.len() {
                return Err(Error::Unsupported(DECODE_PARMS_LENGTH_ERROR.to_string()));
            }
            decode_params
                .try_as_array()?
                .into_iter()
                .flatten()
                .zip(&names)
                .map(|(item, name)| decode_params_from_handle(&item, name))
                .collect::<Result<_>>()?
        }
    } else {
        // One handle replicated across the chain, exactly as qpdf pushes the
        // same `QPDFObjectHandle` per filter (`QPDF_Stream.cc:450-454`).
        replicated_decode_params(decode_params, &names)?
    };

    validate_filter_chain_count(names.len(), max_filter_chain)?;

    Ok(names
        .into_iter()
        .zip(params)
        .map(|(name, decode_params)| FilterSpec {
            name,
            decode_params,
        })
        .collect())
}

fn absent_params(count: usize) -> Vec<DecodeParams> {
    (0..count).map(|_| DecodeParams::Absent).collect()
}

/// [`decode_params_from_handle`] for the one `/DecodeParms` handle a non-null,
/// non-array value replicates across the whole chain.
///
/// **Why not [`decode_params_from_handle`] in a loop.** Consuming stages use
/// [`decode_params_from_consuming_handle`]'s `try_get_keys` path per stage,
/// without taking a raw-map snapshot. Non-consuming stages use
/// [`decode_params_from_entries`]'s [`ObjectHandle::try_as_dictionary`] path,
/// which hands back a *clone* of the whole `BTreeMap` — every key copied, plus
/// one `Rc` bump per value, `ObjectHandle` being a newtype over one. Calling
/// that route once per filter turned an oversized `/DecodeParms` into one full
/// snapshot per stage — measured, sixteen for a sixteen-filter chain — however
/// few keys survive [`RETAINED_DECODE_PARAM_KEYS`], because the retention test
/// runs inside the walk, after the snapshot. Nor was the stage count capped in
/// general:
/// `max_filter_chain` bounds it only where [`crate::filters::DecodeLimits`]
/// carries one. `DecodeLimits::default()` does, but the field is a `pub
/// Option` a caller may set to `None`, which the entry-point corpus sweeps.
/// Taking the non-consuming snapshot here instead costs at most one whatever
/// the chain length is, and those per-filter walks then borrow it.
///
/// **The per-filter walk itself is deliberately kept.** qpdf calls
/// `filter->setDecodeParms(decode_item)` once per filter
/// (`libqpdf/QPDF_Stream.cc:467-482`). Each consuming call uses
/// `decode_parms.getKeys()` followed by retained `getKey(key)` calls
/// (`libqpdf/SF_FlateLzwDecode.cc:29-31`): `getKeys` resolves every child and
/// omits nullish keys before retention, including unrecognized keys. flpdf
/// follows that order through [`decode_params_from_consuming_handle`].
///
/// Non-consuming stages do not inspect children in qpdf. They instead borrow
/// one shared raw-map snapshot, so a child resolved by an earlier consuming
/// stage remains live for a later non-consuming stage; see
/// [`param_value_without_resolving`] and
/// `handle_reader_lets_a_later_stage_see_a_value_an_earlier_stage_resolved`.
///
/// The null test [`decode_params_from_handle`] opens with is deliberately not
/// repeated here: this is reached only from
/// [`decode_filter_specs_from_handle`]'s final arm, past
/// `decode_params.try_is_null()?` having answered `false`. That makes *that*
/// call load-bearing rather than convergent — an indirect `/DecodeParms`
/// resolving to null reads as `Absent` only because of it, which
/// `handle_reader_reads_an_indirect_scalar_decode_parms_resolving_to_null_as_absent`
/// pins.
fn replicated_decode_params(params: &ObjectHandle, names: &[Vec<u8>]) -> Result<Vec<DecodeParams>> {
    let entries = names
        .iter()
        .any(|name| !filter_reads_decode_params(name))
        .then(|| params.try_as_dictionary())
        .transpose()?;
    names
        .iter()
        .map(|name| {
            if filter_reads_decode_params(name) {
                decode_params_from_consuming_handle(params, name)
            } else {
                decode_params_from_entries(
                    entries.as_ref().and_then(|entries| entries.as_ref()),
                    name,
                )
            }
        })
        .collect()
}

/// Does the filter named `filter_name` read `/DecodeParms` *entries*?
///
/// qpdf draws this line per filter, not per key. `QPDFStreamFilter`'s base
/// `setDecodeParms` is `return decode_parms.isNull();`
/// (`libqpdf/QPDFStreamFilter.cc:3-7`) — it inspects the parameter handle
/// itself and never touches an entry — and ASCII85, ASCIIHex, and RunLength
/// all inherit it. `SF_FlateLzwDecode::setDecodeParms` is the one that walks
/// `decode_parms.getKeys()` and `getKey(key)`
/// (`libqpdf/SF_FlateLzwDecode.cc:29-31`). Since every `QPDFObjectHandle`
/// inspector dereferences first, a `/DecodeParms` value behind an indirect
/// reference is resolved by qpdf iff its filter is in the second group.
///
/// **The source is the evidence.** A live probe cannot settle this by itself:
/// qpdf resolves a dangling reference to null silently, so "qpdf printed
/// nothing about object 99" is equally consistent with looking and with not
/// looking. The 2026-08-03 qpdf 11.9.0 probe pins only the observable half —
/// `/ASCIIHexDecode` with a present `/DecodeParms` fails at
/// `setDecodeParms`, exiting 2 with "unable to filter stream data".
///
/// The predicate lives on [`StreamFilter`] and is reached through
/// [`stream_filter_for`] rather than being a name list here, so it cannot
/// drift from that registry the way a second list would. In qpdf the same
/// distinction is implicit — it is simply whether a filter's `setDecodeParms`
/// override calls `getKeys()`.
///
/// That covers the `StreamFilter`-backed half only. The `Crypt` arm below is
/// [`is_crypt_filter`], which carries the note about shadowing
/// `filters::prepare_decode_filters`' identical test.
///
/// Abbreviations are expanded first because qpdf expands them at
/// `QPDF_Stream.cc:419-423`, ahead of the `filter_factories` lookup at `:425`,
/// so `/Fl` reaches `SF_FlateLzwDecode` and consumes.
///
/// An unknown name has no registered filter, so it lands on `false`. That is
/// *closer* to qpdf, which fails the factory lookup and returns at
/// `QPDF_Stream.cc:433-435` without ever reading `/DecodeParms` at `:441` —
/// but not identical, because flpdf still resolves the `/DecodeParms` handle
/// itself. That residue is beads `flpdf-vatj`, not this function's business.
fn filter_reads_decode_params(filter_name: &[u8]) -> bool {
    // `Crypt` is deliberately here rather than falling through to `false`: it
    // is not a `StreamFilter` at all. `filters::prepare_decode_filters` peels
    // it off into `PreparedStage::Crypt` before consulting `stream_filter_for`,
    // and the crypt provider is then handed `&stage.spec.decode_params` — plan
    // decision D2 of `flpdf-25kg.3.4` has that provider selecting its crypt
    // filter from `/Name`. Leaving it out would silently starve that reader.
    if is_crypt_filter(filter_name) {
        return true;
    }
    stream_filter_for(normalize_filter_name(filter_name))
        .is_some_and(|filter| filter.reads_decode_params())
}

/// Is this the one filter name that is not a [`StreamFilter`]?
///
/// The single spelling of the `b"Crypt"` literal
/// [`filter_reads_decode_params`] and [`CRYPT_RETAINED_DECODE_PARAM_KEY`] both
/// turn on. It still shadows the identical test in
/// `filters::prepare_decode_filters`, which is where a `Crypt` spec is routed
/// away from the registry in the first place; the two must move together,
/// because there is no registry entry to keep them in step.
///
/// Abbreviations are expanded first because qpdf expands them at
/// `QPDF_Stream.cc:419-423`, ahead of the `filter_factories` lookup at `:425`.
/// qpdf's `filter_abbreviations` table (`QPDF_Stream.cc:72-83`) holds `/AHx`,
/// `/A85`, `/LZW`, `/Fl`, `/RL`, `/CCF`, `/DCT` and nothing else, and
/// [`normalize_filter_name`] mirrors exactly those seven — so no abbreviation
/// expands to `/Crypt` today and this call changes no answer. It is here so
/// that adding one to either table cannot make this predicate disagree with
/// [`filter_reads_decode_params`]' registry lookup, which normalizes too.
fn is_crypt_filter(filter_name: &[u8]) -> bool {
    normalize_filter_name(filter_name) == b"Crypt"
}

/// Read a `/DecodeParms` value into [`DecodeParams`].
///
/// `filter_name` determines whether this call follows qpdf's consuming
/// `getKeys`/`getKey` reduction or a non-consuming stage borrows the shared
/// raw dictionary snapshot.
///
/// `try_is_null` stays unconditional: qpdf's base `setDecodeParms` asks
/// `decode_parms.isNull()` (`QPDFStreamFilter.cc:6`) for this array item.
/// The consuming branch then lets `try_get_keys` resolve all children; only a
/// non-consuming branch takes `try_as_dictionary`'s shared snapshot.
///
/// A replicated scalar goes through [`replicated_decode_params`], which calls
/// [`decode_params_from_consuming_handle`] once per consuming stage and takes
/// at most one shared snapshot for all non-consuming stages.
///
fn decode_params_from_handle(params: &ObjectHandle, filter_name: &[u8]) -> Result<DecodeParams> {
    if params.try_is_null()? {
        return Ok(DecodeParams::Absent);
    }
    if filter_reads_decode_params(filter_name) {
        return decode_params_from_consuming_handle(params, filter_name);
    }
    decode_params_from_entries(params.try_as_dictionary()?.as_ref(), filter_name)
}

/// One consuming filter's qpdf-shaped `/DecodeParms` read.
///
/// `try_get_keys` resolves every child and omits nullish keys before this
/// reader retains the bounded set. Retained values are then fetched with
/// `try_get_key`, matching qpdf's `getKeys` then `getKey` order.
fn decode_params_from_consuming_handle(
    params: &ObjectHandle,
    filter_name: &[u8],
) -> Result<DecodeParams> {
    let retains_crypt_name = is_crypt_filter(filter_name);
    let mut retained = Vec::new();
    for key in params.try_get_keys()? {
        if !retains_decode_param_key(&key, retains_crypt_name) {
            continue;
        }
        let value = params.try_get_key(&key)?;
        let keeps_name = is_crypt_name_key(&key, retains_crypt_name);
        retained.push((key, param_value_from_handle(&value, keeps_name)?));
    }
    Ok(DecodeParams::Present(retained))
}

/// One non-consuming filter's read of an already-snapshotted `/DecodeParms`
/// dictionary. `None` is a present non-dictionary and yields `Present` with no
/// entries. Non-consuming stages never resolve children.
///
/// Borrowing rather than owning is what lets [`replicated_decode_params`] run
/// this once per filter off a single snapshot. The children are the live
/// handles, not copies of their values, so a value an earlier stage resolved
/// is resolved for a later one too — the same thing that held when every stage
/// re-snapshotted the dictionary, since `ObjectHandle::clone` shares the
/// indirect slot.
fn decode_params_from_entries(
    entries: Option<&BTreeMap<Vec<u8>, ObjectHandle>>,
    filter_name: &[u8],
) -> Result<DecodeParams> {
    let Some(entries) = entries else {
        return Ok(DecodeParams::Present(Vec::new()));
    };
    let retains_crypt_name = is_crypt_filter(filter_name);
    let retained = entries
        .iter()
        .filter(|(key, _)| retains_decode_param_key(key, retains_crypt_name))
        .map(|(key, value)| (key.clone(), param_value_without_resolving(value)))
        .collect();
    Ok(DecodeParams::Present(retained))
}

/// Classify one `/DecodeParms` value, dereferencing it as qpdf's `isInteger`
/// does.
///
/// `keeps_name` is [`is_crypt_name_key`] for the key this value sits under: the
/// name payload is owned only where the crypt provider reads one. Everywhere
/// else a name reduces to [`ParamValue::Other`], which no consumer can
/// distinguish from `Name` — see [`ParamValue`]. `try_as_name` still runs, so
/// the dereference qpdf performs is unchanged; only the payload is dropped.
fn param_value_from_handle(value: &ObjectHandle, keeps_name: bool) -> Result<ParamValue> {
    if let Some(int) = value.try_as_integer()? {
        return Ok(ParamValue::Int(clamp_to_i32(int)));
    }
    Ok(match value.try_as_name()? {
        Some(name) if keeps_name => ParamValue::Name(name),
        _ => ParamValue::Other,
    })
}

/// [`param_value_from_handle`] for a non-consuming filter that never reads an
/// entry. It only classifies shared-snapshot children and never resolves them.
///
/// Deliberately the shape of [`param_value_from_object`]: the same
/// classification off the same non-resolving accessor, so a *direct* value
/// reduces to the identical [`ParamValue`] all three readers agree on.
///
/// **No name test at all**, where the other two readers have one. This is
/// reached only when [`filter_reads_decode_params`] is false, and
/// [`is_crypt_filter`] implies that predicate, so [`is_crypt_name_key`] could
/// never be true here: a payload kept at this position would be one nothing
/// reads. A name reduces to `Other` exactly as it does under every other
/// non-`Crypt` filter — see [`ParamValue`].
///
/// An indirect value that is *still unresolved* reduces to `Other` — the same
/// thing the `Object` reader yields for `Object::Reference`. An indirect value
/// that some earlier read already resolved does not: `ObjectHandle::as_integer`
/// reports an indirect handle's already-resolved value, so a
/// `/DecodeParms` value sharing an object with an earlier-visited position
/// would classify as `Int` here. That is a real difference once
/// `flpdf-25kg.3.5` wires the live resolver, and it is deliberately not
/// defended against, because it is unobservable: the only filters routed here
/// are the ones whose `set_decode_params` reads nothing but `is_absent()`, and
/// `is_absent()` distinguishes `Absent` from `Present` without looking at a
/// single [`ParamValue`]. What must not vary is `Absent` vs `Present`, and
/// that is decided upstream by the two unconditional calls on the parameter
/// handle itself.
fn param_value_without_resolving(value: &ObjectHandle) -> ParamValue {
    match value.as_integer() {
        Some(int) => ParamValue::Int(clamp_to_i32(int)),
        None => ParamValue::Other,
    }
}

/// The legacy `Object` shape reader's counterpart of the handle reduction.
///
/// This reader resolves nothing, because an indirect value is an
/// `Object::Reference` it classifies as [`ParamValue::Other`] without looking
/// through (plan decision D1 of `flpdf-25kg.3.4`). `filter_name` decides both
/// whether qpdf's filter enumerates entries — and therefore whether direct
/// `Object::Null` values are omitted — and whether `/Name` is retained for
/// `Crypt`. References and every other non-null shape remain
/// `ParamValue::Other` when retained.
fn decode_params_from_object(params: Option<&Object>, filter_name: &[u8]) -> DecodeParams {
    let omits_null_values = filter_reads_decode_params(filter_name);
    let retains_crypt_name = is_crypt_filter(filter_name);
    match params {
        None | Some(Object::Null) => DecodeParams::Absent,
        Some(object) => DecodeParams::Present(match object.as_dict() {
            Some(dict) => dict
                .iter()
                .filter(|(_, value)| !omits_null_values || !matches!(value, Object::Null))
                .filter(|(key, _)| retains_decode_param_key(key, retains_crypt_name))
                .map(|(key, value)| {
                    let keeps_name = is_crypt_name_key(key, retains_crypt_name);
                    (key.to_vec(), param_value_from_object(value, keeps_name))
                })
                .collect(),
            None => Vec::new(),
        }),
    }
}

fn param_value_from_object(value: &Object, keeps_name: bool) -> ParamValue {
    match clamped_int_param(value) {
        Some(int) => ParamValue::Int(int),
        None => match value.as_name() {
            Some(name) if keeps_name => ParamValue::Name(name.to_vec()),
            _ => ParamValue::Other,
        },
    }
}

struct OutputBuffer {
    data: Vec<u8>,
    max_output: Option<usize>,
    cleanup_data_start: Option<usize>,
    finish_phase: Rc<Cell<bool>>,
    output_position: Rc<Cell<usize>>,
}

impl OutputBuffer {
    fn new(max_output: Option<usize>) -> Self {
        Self {
            data: Vec::new(),
            max_output,
            cleanup_data_start: None,
            finish_phase: Rc::new(Cell::new(false)),
            output_position: Rc::new(Cell::new(0)),
        }
    }

    fn finish_phase(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.finish_phase)
    }

    fn output_position(&self) -> Rc<Cell<usize>> {
        Rc::clone(&self.output_position)
    }

    fn cleanup_data_start(&self) -> usize {
        self.cleanup_data_start.unwrap_or(self.data.len())
    }
}

impl Pipeline for OutputBuffer {
    fn identifier(&self) -> &str {
        "stream data buffer"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        if self.finish_phase.get() && self.cleanup_data_start.is_none() {
            self.cleanup_data_start = Some(self.data.len());
        }
        if let Some(limit) = self.max_output {
            let remaining = limit.saturating_sub(self.data.len());
            if data.len() > remaining {
                self.data.extend_from_slice(&data[..remaining]);
                self.output_position.set(self.data.len());
                return Err(PipelineError::runtime(format!(
                    "{DECODE_OUTPUT_LIMIT_PREFIX} {limit} bytes"
                )));
            }
        }
        self.data.extend_from_slice(data);
        self.output_position.set(self.data.len());
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

fn map_pipeline_error(error: PipelineError) -> Error {
    Error::Unsupported(error.into_string_lossy())
}

pub(crate) struct FilterDecodeOutcome {
    pub(crate) data: Vec<u8>,
    pub(crate) cleanup_data_start: usize,
    pub(crate) error: Option<FilterDecodeError>,
}

pub(crate) struct FilterDecodeError {
    pub(crate) error: Error,
    pub(crate) during_write: bool,
    pub(crate) output_offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FilterDecodePhase {
    Write,
    Finish,
}

impl FilterDecodeOutcome {
    #[cfg(test)]
    fn complete(data: Vec<u8>) -> Self {
        let cleanup_data_start = data.len();
        Self {
            data,
            cleanup_data_start,
            error: None,
        }
    }

    #[cfg(test)]
    fn into_strict_result(self) -> Result<Vec<u8>> {
        match self.error {
            Some(error) => Err(error.error),
            None => Ok(self.data),
        }
    }
}

/// Pipe one complete encoded buffer through a stage with qpdf's error cleanup.
///
/// `QPDF::pipeStreamData` calls `finish` after a failed `write`, ignores that
/// secondary result, and keeps the original exception. The stage's sink remains
/// owned by its caller, so bytes forwarded before the failure stay accessible.
struct StagePipelineError {
    error: PipelineError,
    during_write: bool,
    output_offset: usize,
}

fn write_and_finish(
    stage: &mut dyn Pipeline,
    data: &[u8],
    finish_phase: Option<&Cell<bool>>,
    output_position: &Cell<usize>,
) -> Option<StagePipelineError> {
    if let Some(finish_phase) = finish_phase {
        finish_phase.set(false);
    }
    match stage.write(data) {
        Ok(()) => {
            if let Some(finish_phase) = finish_phase {
                finish_phase.set(true);
            }
            stage.finish().err().map(|error| StagePipelineError {
                error,
                during_write: false,
                output_offset: output_position.get(),
            })
        }
        Err(error) => {
            let output_offset = output_position.get();
            if let Some(finish_phase) = finish_phase {
                finish_phase.set(true);
            }
            let _ = stage.finish();
            Some(StagePipelineError {
                error,
                during_write: true,
                output_offset,
            })
        }
    }
}

fn map_stage_error(error: StagePipelineError) -> FilterDecodeError {
    FilterDecodeError {
        error: map_pipeline_error(error.error),
        during_write: error.during_write,
        output_offset: error.output_offset,
    }
}

#[cfg(test)]
pub(crate) fn ignore_warning(
    _: &str,
    _: i32,
    _: usize,
    _: FilterDecodePhase,
) -> PipelineResult<()> {
    Ok(())
}

#[cfg(test)]
fn ignore_codec_warning(_: &str, _: i32) -> PipelineResult<()> {
    Ok(())
}

#[cfg(test)]
thread_local! {
    static EXPECTED_FIRST_INPUT: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn expect_first_filter_input(data: &[u8]) {
    EXPECTED_FIRST_INPUT.set(data.as_ptr() as usize);
}

/// Rust equivalent of qpdf's `QPDFStreamFilter` extension boundary.
///
/// `pipe_decode` owns construction and completion of the filter's decode
/// pipeline. A whole-buffer result keeps flpdf's public API stable while the
/// individual codecs are migrated to incremental `Pipeline` stages.
pub(crate) trait StreamFilter {
    /// Port of `QPDFStreamFilter::setDecodeParms`
    /// (`libqpdf/QPDFStreamFilter.cc:3-7`), whose whole body is
    /// `return decode_parms.isNull();` — documented at
    /// `include/qpdf/QPDFStreamFilter.hh:41-42` as "The default implementation
    /// accepts a null object and rejects everything else". `DecodeParams` has
    /// already folded a missing key into the null case, so `is_absent()` is
    /// that `isNull()`.
    fn set_decode_params(&mut self, decode_params: &DecodeParams) -> bool {
        decode_params.is_absent()
    }

    /// Does [`Self::set_decode_params`] look at the parameter *entries*?
    ///
    /// `false` alongside the default `set_decode_params` above, which reads
    /// nothing but `is_absent()` — qpdf's `decode_parms.isNull()`. A filter
    /// overriding one should consider the other: this is the flpdf-side
    /// statement of whether qpdf's counterpart calls `getKeys()`.
    ///
    /// It is not a decode decision — it decides whether the `ObjectHandle`
    /// shape reader *dereferences* each value, so that flpdf touches exactly
    /// the objects qpdf touches. See [`filter_reads_decode_params`], which is
    /// the only caller.
    fn reads_decode_params(&self) -> bool {
        false
    }

    /// Build the filter's decode pipeline without decoding anything.
    ///
    /// `QPDF_Stream::pipeStreamData` constructs every filter's decode pipeline
    /// before it writes the first byte, so a stage whose parameters cannot form
    /// a pipeline is rejected even when an earlier stage would have failed on
    /// the data itself.
    fn preflight_decode_pipeline(&self) -> Result<()> {
        Ok(())
    }

    /// Port of `QPDFStreamFilter::getDecodePipeline`
    /// (`include/qpdf/QPDFStreamFilter.hh:46-49`): build this filter's decode
    /// stage around `next` and return it without decoding anything. qpdf
    /// declares it pure virtual, so there is no default here either.
    ///
    /// `Result` carries the construction failures qpdf raises from the stage
    /// constructors themselves; `None` is qpdf's `nullptr`, which the caller
    /// reads as "this filter contributes no stage" and leaves its own `next`
    /// in place (`QPDF_Stream.cc:561-563`). In qpdf 11.9.0 the only filter
    /// that returns it is `SF_Crypt` (`QPDF_Stream.cc:52-56`).
    ///
    /// qpdf keeps each constructed stage in the filter instance and hands the
    /// caller a non-owning pointer. The stage is returned by value here
    /// instead — see [`crate::pipeline::PipelineRef`] for why, and
    /// `QPDF_Stream.cc:559-568` for the caller-side loop this feeds.
    ///
    /// The Flate warn callback is deliberately absent: qpdf installs it at the
    /// `pipeStreamData` caller (`QPDF_Stream.cc:564-567`), not here.
    #[allow(dead_code)]
    fn decode_pipeline<'a>(
        &mut self,
        next: &'a mut dyn Pipeline,
    ) -> Result<Option<Box<dyn Pipeline + 'a>>>;

    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        max_output: Option<usize>,
        warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome>;

    #[cfg(test)]
    fn pipe_decode(
        &mut self,
        data: &[u8],
        max_output: Option<usize>,
        warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<Vec<u8>> {
        self.pipe_decode_recovering(data, max_output, warn)?
            .into_strict_result()
    }

    // flpdf's current public decode API always requests full decoding, so
    // classification becomes a production decision only when decode levels
    // are introduced. Keep the qpdf extension contract available to later
    // registered filters.
    #[allow(dead_code)]
    fn is_specialized_compression(&self) -> bool {
        false
    }

    #[allow(dead_code)]
    fn is_lossy_compression(&self) -> bool {
        false
    }
}

/// Rust equivalent of qpdf's `SF_FlateLzwDecode`.
///
/// One filter serves `FlateDecode` and `LZWDecode`, owns the shared predictor
/// parameters, and builds the decode chain codec-then-predictor.
struct FlateLzwStreamFilter {
    lzw: bool,
    predictor: i32,
    columns: i32,
    colors: i32,
    bits_per_component: i32,
    early_code_change: bool,
}

impl FlateLzwStreamFilter {
    /// Construct with the PDF specification defaults qpdf uses.
    fn new(lzw: bool) -> Self {
        Self {
            lzw,
            predictor: 1,
            columns: 1,
            colors: 1,
            bits_per_component: 8,
            early_code_change: true,
        }
    }
}

/// The `/DecodeParms` keys some flpdf consumer reads under *every* filter.
///
/// [`DecodeParams`] keeps only these, plus
/// [`CRYPT_RETAINED_DECODE_PARAM_KEY`] under a `Crypt` stage. qpdf has no
/// counterpart, and needs none: it replicates a `QPDFObjectHandle` — a
/// `shared_ptr` — across the filter chain and never copies the dictionary.
/// `DecodeParams` owns its entries, so a scalar `/DecodeParms` replicated
/// across an *n*-filter chain would otherwise be converted and stored *n*
/// times.
///
/// These five are exactly the ones
/// `<FlateLzwStreamFilter as StreamFilter>::set_decode_params` matches below —
/// the one `setDecodeParms` override in qpdf 11.9.0 that reads entries at all
/// (`libqpdf/SF_FlateLzwDecode.cc:32-66`). qpdf spells its keys with the
/// leading `/`; flpdf's `Dictionary` keys never carry one, so these do not
/// either — matching `set_decode_params`' own `b"Predictor"` arms rather than
/// qpdf's literals.
///
/// **These five are retained whatever the filter is named, and that is not
/// laziness.** [`png_encode_geometry`] builds a `FlateLzwStreamFilter` for
/// whichever name `filters::encode_stream_data` hands it and feeds that filter
/// this same [`DecodeParams`], so dropping the geometry under a non-Flate name
/// would change what the public encode path does with, say, `/Filter
/// [/ASCII85Decode]` carrying `/Predictor 12`. `/Name` has no such second
/// consumer, which is why it alone is per-filter.
///
/// This is *per key*, not per filter, within that set: `EarlyChange` survives
/// under `/FlateDecode` even though `set_decode_params` consults it only for
/// LZW. Bounding the set is the point; making it minimal per filter is not.
///
/// # The bound this states
///
/// For every filter but `Crypt`, a [`FilterSpec`]'s `DecodeParams` is a
/// **constant** whatever the source dictionary holds: at most these five keys,
/// 49 bytes of key text in total, each carrying a `ParamValue` that owns
/// nothing on the heap.
/// `retained_decode_parameter_bytes_do_not_grow_with_a_name_valued_parameter`
/// pins that as an exact figure through both shape readers — 63 bytes per
/// `ASCIIHexDecode` stage (49 of keys plus the 14-byte filter name), 1008 for
/// a 16-stage chain — with a name of 16 bytes and of one mebibyte alike, in
/// *every* retained slot at once rather than only under `/Name`.
///
/// **The retained keys were never the unbounded part.** They are drawn from
/// this fixed array, so their total is fixed too, and shrinking the array was
/// never what this bound needed. `ParamValue::Name(Vec<u8>)` was the unbounded
/// one: qpdf's object parser tokenizes with `QPDFTokenizer::nextToken`'s
/// default `max_len` of 0 (`QPDFParser.cc:38`, `:141`;
/// `include/qpdf/QPDFTokenizer.hh:205`), which
/// `QPDFTokenizer.cc:948`'s `if (max_len && ...)` treats as no cap at all, and
/// flpdf's `tokenizer::Tokenizer::read_token` mirrors that `max_len != 0`
/// test. So a name is as long as the file makes it.
///
/// **Restricting the key `/Name` is not on its own enough**, because a name
/// value fits any slot: `/Predictor /<one-mebibyte-name>` passes this key test
/// and used to be copied per stage — 16,777,584 bytes retained across a
/// 16-stage `ASCIIHexDecode` chain, measured. The payload rule in
/// [`ParamValue`] is the other half, and it is what makes this a bound rather
/// than a bound on one key.
///
/// **Retention is not resolution.** A consuming stage first calls
/// [`decode_params_from_consuming_handle`], whose `try_get_keys` resolves every
/// child and omits nullish keys before this retained-key bound applies. A
/// non-consuming stage instead uses [`decode_params_from_entries`] over the
/// shared snapshot and resolves no children. The former closes the
/// `flpdf-h8mv` null-key divergence without widening either retained set.
const RETAINED_DECODE_PARAM_KEYS: [&[u8]; 5] = [
    b"BitsPerComponent",
    b"Colors",
    b"Columns",
    b"EarlyChange",
    b"Predictor",
];

/// The one `/DecodeParms` key retained for `Crypt` stages and nowhere else.
///
/// Plan decision D2 of `flpdf-25kg.3.4` has the crypt provider selecting its
/// crypt filter from `/Name`, and `filters::CryptProvider` is
/// `FnMut(&DecodeParams, &[u8])` — no handle in it — so [`DecodeParams`] is
/// that provider's only route to the name. Nothing else reads it: the base
/// `StreamFilter::set_decode_params` reads only `is_absent()`,
/// `FlateLzwStreamFilter::set_decode_params` matches only
/// [`RETAINED_DECODE_PARAM_KEYS`] and lets every other key fall through its
/// `_ => {}` arm, and [`png_encode_geometry`] reaches the parameters through
/// that same filter.
///
/// qpdf needs no such route. `SF_Crypt::setDecodeParms`
/// (`libqpdf/QPDF_Stream.cc:33-50`) stores nothing at all — it only decides
/// `filterable` and leaves the value to `decryptStream`, which re-reads it off
/// the live object graph (`decode_parms.getKey("/Name")`,
/// `libqpdf/QPDF_encryption.cc:1072`, and the `/CF` filter's own `/Name` at
/// `:1085-1087`).
///
/// **This is a per-stage residual, not parity, and it is not a bound on the
/// chain.** Each `Crypt` stage keeps its own copy, so `/Filter [/Crypt /Crypt
/// …]` sharing one scalar `/DecodeParms` still holds one copy of the name per
/// stage — capped by `filters::DecodeLimits::max_filter_chain` where a caller
/// sets one (16 under `DecodeLimits::default()`) and uncapped where a caller
/// passes `None`, which `filters::encode_stream_data` does unconditionally.
/// qpdf holds zero copies at any chain length. Restricting retention to
/// `Crypt` narrows the exposure to chains that name `Crypt`; it does not
/// remove it.
const CRYPT_RETAINED_DECODE_PARAM_KEY: &[u8] = b"Name";

fn retains_decode_param_key(key: &[u8], retains_crypt_name: bool) -> bool {
    RETAINED_DECODE_PARAM_KEYS.contains(&key) || is_crypt_name_key(key, retains_crypt_name)
}

/// Is this the entry whose *name payload* some consumer reads?
///
/// The one place a `ParamValue::Name` keeps its bytes, and the same test
/// [`retains_decode_param_key`] admits [`CRYPT_RETAINED_DECODE_PARAM_KEY`] by —
/// spelled once so the two cannot drift into retaining a key whose value is
/// then dropped, or owning a payload under a key that is not kept.
///
/// `retains_crypt_name` is redundant with the key test at every present call
/// site, since `/Name` is retained only under `Crypt`. It is passed anyway:
/// consuming classification runs after `try_get_keys` filters nullish keys and
/// after this retention test; a future reordering must not start owning
/// payloads under every filter.
fn is_crypt_name_key(key: &[u8], retains_crypt_name: bool) -> bool {
    retains_crypt_name && key == CRYPT_RETAINED_DECODE_PARAM_KEY
}

/// Apply `getIntValueAsInt`'s saturation (`QPDFObjectHandle.cc:531-538`), which
/// pins a value below `INT_MIN` to `INT_MIN` and one above `INT_MAX` to
/// `INT_MAX` rather than failing — so a `/Columns` far beyond `INT_MAX` behaves
/// as `INT_MAX` does.
///
/// qpdf also emits `warnIfPossible("requested value of integer is too small;
/// returning INT_MIN")` (and the `INT_MAX` counterpart) on those two branches;
/// flpdf does not reproduce those diagnostics, only the value.
///
/// This is the shape-independent half of the parity, kept separate from
/// `clamped_int_param` so a second `/DecodeParms` shape reader clamps through
/// this one copy instead of restating the bounds.
fn clamp_to_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Read an `Object` the way qpdf reads a `/DecodeParms` value: `None` for every
/// shape `QPDFObjectHandle::isInteger` (`QPDFObjectHandle.cc:358-362`) rejects,
/// and otherwise the clamped integer.
///
/// The filters no longer call this. It runs once per value in
/// `param_value_from_object`, so the clamp is applied while the `Object` shape
/// is read and every filter sees only the already-clamped `ParamValue::Int`.
fn clamped_int_param(value: &Object) -> Option<i32> {
    value.as_integer().map(clamp_to_i32)
}

/// Mirror `QIntC::to_uint`, whose range failure is a `std::runtime_error`.
fn to_uint(value: i32) -> Result<u32> {
    u32::try_from(value).map_err(|_| {
        Error::Unsupported(format!(
            "integer out of range converting {value} from a 4-byte signed type to a 4-byte unsigned type"
        ))
    })
}

impl StreamFilter for FlateLzwStreamFilter {
    /// `SF_FlateLzwDecode::setDecodeParms` is the one override in qpdf 11.9.0
    /// that walks `decode_parms.getKeys()` (`SF_FlateLzwDecode.cc:29-31`), so
    /// this is the one filter whose parameter values qpdf dereferences.
    fn reads_decode_params(&self) -> bool {
        true
    }

    fn set_decode_params(&mut self, decode_params: &DecodeParams) -> bool {
        // The one early return SF_FlateLzwDecode::setDecodeParms has
        // (SF_FlateLzwDecode.cc:24-26), for a null /DecodeParms. Every other
        // shape walks the keys and then falls through to the trailing check at
        // :68-70, which a present-but-empty parameter set reaches.
        if decode_params.is_absent() {
            return true;
        }

        let mut filterable = true;
        for (key, value) in decode_params.entries() {
            let key = key.as_slice();
            match key {
                b"Predictor" => match *value {
                    ParamValue::Int(predictor) => {
                        self.predictor = predictor;
                        if !((predictor == 1) || (predictor == 2) || (10..=15).contains(&predictor))
                        {
                            filterable = false;
                        }
                    }
                    ParamValue::Name(_) | ParamValue::Other => filterable = false,
                },
                b"Columns" | b"Colors" | b"BitsPerComponent" => match *value {
                    // qpdf stores these without range validation and defers
                    // rejection to pipeline construction.
                    ParamValue::Int(parameter) => match key {
                        b"Columns" => self.columns = parameter,
                        b"Colors" => self.colors = parameter,
                        _ => self.bits_per_component = parameter,
                    },
                    ParamValue::Name(_) | ParamValue::Other => filterable = false,
                },
                // qpdf consults /EarlyChange only for LZW streams.
                b"EarlyChange" if self.lzw => match *value {
                    ParamValue::Int(early_change) => {
                        self.early_code_change = early_change == 1;
                        if !((early_change == 0) || (early_change == 1)) {
                            filterable = false;
                        }
                    }
                    ParamValue::Name(_) | ParamValue::Other => filterable = false,
                },
                _ => {}
            }
        }

        if (self.predictor > 1) && (self.columns == 0) {
            filterable = false;
        }

        filterable
    }

    fn preflight_decode_pipeline(&self) -> Result<()> {
        if let Some((columns, colors, bits_per_component)) = self.decode_predictor_geometry()? {
            let mut sink = OutputBuffer::new(None);
            PngFilter::new(
                "png decode",
                &mut sink,
                PngFilterAction::Decode,
                columns,
                colors,
                bits_per_component,
            )
            .map_err(map_pipeline_error)?;
        }
        Ok(())
    }

    /// Mirrors `SF_FlateLzwDecode::getDecodePipeline`
    /// (`libqpdf/SF_FlateLzwDecode.cc:75-110`): a predictor stage first when
    /// the parameters call for one, with `next` reassigned to it, then the
    /// codec wrapping whichever `next` resulted. The codec is what the caller
    /// receives.
    fn decode_pipeline<'a>(
        &mut self,
        next: &'a mut dyn Pipeline,
    ) -> Result<Option<Box<dyn Pipeline + 'a>>> {
        let next: PipelineRef<'a> = match self.decode_predictor_geometry()? {
            Some((columns, colors, bits_per_component)) => {
                let predictor = PngFilter::new(
                    "png decode",
                    next,
                    PngFilterAction::Decode,
                    columns,
                    colors,
                    bits_per_component,
                )
                .map_err(map_pipeline_error)?;
                PipelineRef::Owned(Box::new(predictor))
            }
            None => PipelineRef::Borrowed(next),
        };
        let stage: Box<dyn Pipeline + 'a> = if self.lzw {
            Box::new(LzwDecoder::new("lzw decode", next, self.early_code_change))
        } else {
            Box::new(
                Flate::new(
                    "stream inflate",
                    next,
                    FlateAction::Inflate,
                    DEFAULT_OUT_BUFFER_SIZE,
                )
                .map_err(map_pipeline_error)?,
            )
        };
        Ok(Some(stage))
    }

    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        max_output: Option<usize>,
        warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        let geometry = self.decode_predictor_geometry()?;
        let mut sink = OutputBuffer::new(max_output);
        let finish_phase = sink.finish_phase();
        let output_position = sink.output_position();
        // SF_FlateLzwDecode::getDecodePipeline builds the chain from the sink
        // outward, so the predictor stage is constructed before the codec and
        // any construction failure precedes every codec write.
        let error = match geometry {
            Some((columns, colors, bits_per_component)) => {
                let mut predictor = PngFilter::new(
                    "png decode",
                    &mut sink,
                    PngFilterAction::Decode,
                    columns,
                    colors,
                    bits_per_component,
                )
                .map_err(map_pipeline_error)?;
                let phase = Some(finish_phase.as_ref());
                self.pipe_codec(&mut predictor, data, warn, phase, &output_position)?
            }
            None => {
                self.pipe_codec(&mut sink, data, warn, Some(&finish_phase), &output_position)?
            }
        };
        Ok(FilterDecodeOutcome {
            cleanup_data_start: sink.cleanup_data_start(),
            data: sink.data,
            error,
        })
    }
}

impl FlateLzwStreamFilter {
    /// Resolve the predictor geometry the decode chain needs, if any.
    ///
    /// This reproduces the failures `SF_FlateLzwDecode::getDecodePipeline`
    /// raises while constructing the chain, so both the preflight and the
    /// decode itself reject exactly the same parameters.
    fn decode_predictor_geometry(&self) -> Result<Option<(u32, u32, u32)>> {
        if (10..=15).contains(&self.predictor) {
            return Ok(Some((
                to_uint(self.columns)?,
                to_uint(self.colors)?,
                to_uint(self.bits_per_component)?,
            )));
        }
        if self.predictor == 2 {
            // Declared deviation: qpdf builds Pl_TIFFPredictor here. flpdf has
            // no TIFF predictor component yet and reports the restriction at
            // qpdf's construction point.
            return Err(Error::Unsupported(
                "/DecodeParms /Predictor 2 is not supported for this stream type".to_string(),
            ));
        }
        Ok(None)
    }

    fn pipe_codec(
        &self,
        next: &mut dyn Pipeline,
        data: &[u8],
        warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
        finish_phase: Option<&Cell<bool>>,
        output_position: &Cell<usize>,
    ) -> Result<Option<FilterDecodeError>> {
        let error = if self.lzw {
            let mut stage = LzwDecoder::new("lzw decode", next, self.early_code_change);
            write_and_finish(&mut stage, data, finish_phase, output_position)
        } else {
            let mut stage = Flate::new(
                "stream inflate",
                next,
                FlateAction::Inflate,
                DEFAULT_OUT_BUFFER_SIZE,
            )
            .map_err(map_pipeline_error)?;
            stage.set_warn_callback(|message, code| {
                let phase = filter_decode_phase(finish_phase);
                warn(message, code, output_position.get(), phase)
            });
            write_and_finish(&mut stage, data, finish_phase, output_position)
        };
        Ok(error.map(map_stage_error))
    }
}

fn filter_decode_phase(finish_phase: Option<&Cell<bool>>) -> FilterDecodePhase {
    if finish_phase.is_some_and(Cell::get) {
        FilterDecodePhase::Finish
    } else {
        FilterDecodePhase::Write
    }
}

struct Ascii85StreamFilter;

impl StreamFilter for Ascii85StreamFilter {
    /// Mirrors `SF_ASCII85Decode::getDecodePipeline`
    /// (`libqpdf/qpdf/SF_ASCII85Decode.hh:14-19`), a single `Pl_ASCII85Decoder`.
    fn decode_pipeline<'a>(
        &mut self,
        next: &'a mut dyn Pipeline,
    ) -> Result<Option<Box<dyn Pipeline + 'a>>> {
        Ok(Some(Box::new(Ascii85Decoder::new("ascii85 decode", next))))
    }

    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        max_output: Option<usize>,
        _warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        decode_ascii85(data, max_output)
    }
}

struct AsciiHexStreamFilter;

impl StreamFilter for AsciiHexStreamFilter {
    /// Mirrors `SF_ASCIIHexDecode::getDecodePipeline`
    /// (`libqpdf/qpdf/SF_ASCIIHexDecode.hh:14-19`), a single
    /// `Pl_ASCIIHexDecoder`.
    fn decode_pipeline<'a>(
        &mut self,
        next: &'a mut dyn Pipeline,
    ) -> Result<Option<Box<dyn Pipeline + 'a>>> {
        Ok(Some(Box::new(AsciiHexDecoder::new(
            "asciiHex decode",
            next,
        ))))
    }

    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        max_output: Option<usize>,
        _warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        decode_ascii_hex(data, max_output)
    }
}

struct RunLengthStreamFilter;

impl StreamFilter for RunLengthStreamFilter {
    /// Mirrors `SF_RunLengthDecode::getDecodePipeline`
    /// (`libqpdf/qpdf/SF_RunLengthDecode.hh:14-20`), a single `Pl_RunLength`
    /// in its decode action.
    fn decode_pipeline<'a>(
        &mut self,
        next: &'a mut dyn Pipeline,
    ) -> Result<Option<Box<dyn Pipeline + 'a>>> {
        Ok(Some(Box::new(RunLength::new(
            "runlength decode",
            next,
            RunLengthAction::Decode,
        ))))
    }

    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        max_output: Option<usize>,
        _warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        decode_run_length(data, max_output)
    }

    fn is_specialized_compression(&self) -> bool {
        true
    }
}

#[cfg(test)]
struct TestStreamFilter;

#[cfg(test)]
impl StreamFilter for TestStreamFilter {
    // Passes data through untouched, so it builds no stage of its own —
    // qpdf's nullptr, which leaves the caller writing straight to `next`.
    fn decode_pipeline<'a>(
        &mut self,
        _: &'a mut dyn Pipeline,
    ) -> Result<Option<Box<dyn Pipeline + 'a>>> {
        Ok(None)
    }

    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        _: Option<usize>,
        _: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        Ok(FilterDecodeOutcome::complete(data.to_vec()))
    }
}

#[cfg(test)]
struct BorrowedInputProbe;

#[cfg(test)]
impl StreamFilter for BorrowedInputProbe {
    // The probe only inspects the input buffer it is handed; it transforms
    // nothing, so it contributes no stage.
    fn decode_pipeline<'a>(
        &mut self,
        _: &'a mut dyn Pipeline,
    ) -> Result<Option<Box<dyn Pipeline + 'a>>> {
        Ok(None)
    }

    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        _: Option<usize>,
        _: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        EXPECTED_FIRST_INPUT.with(|expected| {
            assert_eq!(data.as_ptr() as usize, expected.get());
        });
        Ok(FilterDecodeOutcome::complete(data.to_vec()))
    }
}

#[cfg(test)]
struct PostPreflightFailure;

#[cfg(test)]
impl StreamFilter for PostPreflightFailure {
    // Fails on every route past the preflight, the decode route included.
    fn decode_pipeline<'a>(
        &mut self,
        _: &'a mut dyn Pipeline,
    ) -> Result<Option<Box<dyn Pipeline + 'a>>> {
        Err(Error::Internal(
            "test post-preflight decode failure".to_string(),
        ))
    }

    fn pipe_decode_recovering(
        &mut self,
        _: &[u8],
        _: Option<usize>,
        _: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        Err(Error::Internal(
            "test post-preflight decode failure".to_string(),
        ))
    }
}

pub(crate) fn stream_filter_for(filter_name: &[u8]) -> Option<Box<dyn StreamFilter>> {
    match filter_name {
        b"FlateDecode" => Some(Box::new(FlateLzwStreamFilter::new(false))),
        b"LZWDecode" => Some(Box::new(FlateLzwStreamFilter::new(true))),
        b"ASCII85Decode" => Some(Box::new(Ascii85StreamFilter)),
        b"ASCIIHexDecode" => Some(Box::new(AsciiHexStreamFilter)),
        b"RunLengthDecode" => Some(Box::new(RunLengthStreamFilter)),
        #[cfg(test)]
        b"TestRejectDecode" => Some(Box::new(TestStreamFilter)),
        #[cfg(test)]
        b"TestBorrowedInput" => Some(Box::new(BorrowedInputProbe)),
        #[cfg(test)]
        b"TestPostPreflightFailure" => Some(Box::new(PostPreflightFailure)),
        _ => None,
    }
}

fn decode_ascii85(data: &[u8], max_output: Option<usize>) -> Result<FilterDecodeOutcome> {
    let mut sink = OutputBuffer::new(max_output);
    let finish_phase = sink.finish_phase();
    let output_position = sink.output_position();
    let error = {
        let mut stage = Ascii85Decoder::new("ascii85 decode", &mut sink);
        write_and_finish(&mut stage, data, Some(&finish_phase), &output_position)
            .map(map_stage_error)
    };
    Ok(FilterDecodeOutcome {
        cleanup_data_start: sink.cleanup_data_start(),
        data: sink.data,
        error,
    })
}

fn decode_ascii_hex(data: &[u8], max_output: Option<usize>) -> Result<FilterDecodeOutcome> {
    let mut sink = OutputBuffer::new(max_output);
    let finish_phase = sink.finish_phase();
    let output_position = sink.output_position();
    let error = {
        let mut stage = AsciiHexDecoder::new("asciiHex decode", &mut sink);
        write_and_finish(&mut stage, data, Some(&finish_phase), &output_position)
            .map(map_stage_error)
    };
    Ok(FilterDecodeOutcome {
        cleanup_data_start: sink.cleanup_data_start(),
        data: sink.data,
        error,
    })
}

fn decode_run_length(data: &[u8], max_output: Option<usize>) -> Result<FilterDecodeOutcome> {
    let mut sink = OutputBuffer::new(max_output);
    let finish_phase = sink.finish_phase();
    let output_position = sink.output_position();
    let error = {
        let mut stage = RunLength::new("runlength decode", &mut sink, RunLengthAction::Decode);
        write_and_finish(&mut stage, data, Some(&finish_phase), &output_position)
            .map(map_stage_error)
    };
    Ok(FilterDecodeOutcome {
        cleanup_data_start: sink.cleanup_data_start(),
        data: sink.data,
        error,
    })
}

#[cfg(test)]
fn decode_flate_chunks<'a>(
    chunks: impl IntoIterator<Item = &'a [u8]>,
    max_output: Option<usize>,
    warn: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
) -> Result<Vec<u8>> {
    let mut sink = OutputBuffer::new(max_output);
    {
        let mut flate = Flate::new(
            "stream inflate",
            &mut sink,
            FlateAction::Inflate,
            DEFAULT_OUT_BUFFER_SIZE,
        )
        .map_err(map_pipeline_error)?;
        flate.set_warn_callback(|message, code| warn(message, code));
        for chunk in chunks {
            flate.write(chunk).map_err(map_pipeline_error)?;
        }
        flate.finish().map_err(map_pipeline_error)?;
    }
    Ok(sink.data)
}

#[cfg(test)]
fn decode_flate(data: &[u8], max_output: Option<usize>) -> Result<Vec<u8>> {
    decode_flate_chunks([data], max_output, &mut ignore_codec_warning)
}

pub(crate) fn encode_flate(data: &[u8]) -> Result<Vec<u8>> {
    let mut sink = Buffer::new("stream data buffer", None);
    {
        let mut flate = Flate::new(
            "compress stream",
            &mut sink,
            FlateAction::Deflate,
            DEFAULT_OUT_BUFFER_SIZE,
        )
        .map_err(map_pipeline_error)?;
        flate.write(data).map_err(map_pipeline_error)?;
        flate.finish().map_err(map_pipeline_error)?;
    }
    sink.take_buffer().map_err(map_pipeline_error)
}

/// Resolve the PNG predictor geometry a writer must apply for `/DecodeParms`.
///
/// Returns `Ok(None)` when the parameters select no PNG predictor. The
/// parameters are validated through the same `SF_FlateLzwDecode` state the
/// decode path uses, so both directions accept exactly the same dictionaries.
pub(crate) fn png_encode_geometry(
    filter_name: &[u8],
    decode_params: &DecodeParams,
) -> Result<Option<(u32, u32, u32)>> {
    let mut filter = FlateLzwStreamFilter::new(filter_name == b"LZWDecode");
    if !filter.set_decode_params(decode_params) {
        return Err(Error::Unsupported(format!(
            "stream filter {} does not support supplied /DecodeParms",
            String::from_utf8_lossy(filter_name)
        )));
    }
    filter.decode_predictor_geometry()
}

/// Apply the PNG predictor to unencoded stream data.
///
/// qpdf's `Pl_PNGFilter` encoder always emits the Up filter, so the predictor
/// number selects only whether the predictor runs, never which row filter the
/// output uses.
pub(crate) fn encode_png_predictor(
    data: &[u8],
    columns: u32,
    colors: u32,
    bits_per_component: u32,
) -> Result<Vec<u8>> {
    let mut sink = Buffer::new("stream data buffer", None);
    {
        let mut stage = PngFilter::new(
            "png encode",
            &mut sink,
            PngFilterAction::Encode,
            columns,
            colors,
            bits_per_component,
        )
        .map_err(map_pipeline_error)?;
        stage.write(data).map_err(map_pipeline_error)?;
        stage.finish().map_err(map_pipeline_error)?;
    }
    sink.take_buffer().map_err(map_pipeline_error)
}

pub(crate) fn encode_run_length(data: &[u8]) -> Result<Vec<u8>> {
    let mut sink = Buffer::new("stream data buffer", None);
    {
        let mut stage = RunLength::new("compress stream", &mut sink, RunLengthAction::Encode);
        stage.write(data).map_err(map_pipeline_error)?;
        stage.finish().map_err(map_pipeline_error)?;
    }
    sink.take_buffer().map_err(map_pipeline_error)
}

/// `pub(crate)` so `filters.rs`'s entry-point equivalence corpus can reuse
/// [`tests::shape_corpus`] and [`tests::handle_from_object`] instead of keeping
/// a second copy that could grow a row this module's corpus never sees. Only
/// those two helpers are crate-visible; the tests themselves stay private.
/// `object_handle::identity_tests` is the same arrangement.
#[cfg(test)]
pub(crate) mod tests {
    use super::{
        decode_filter_specs_from_handle, decode_filter_specs_from_object, decode_flate,
        decode_flate_chunks, decode_params_from_object, encode_flate, encode_run_length,
        ignore_codec_warning, ignore_warning, normalize_filter_name, stream_filter_for,
        AsciiHexStreamFilter, DecodeParams, FilterSpec, FlateLzwStreamFilter, ObjectHandle,
        OutputBuffer, ParamValue, Pipeline, StreamFilter, DECODE_OUTPUT_LIMIT_PREFIX,
        RETAINED_DECODE_PARAM_KEYS,
    };
    use crate::object_handle::identity_tests::{
        logged_resolver_bearing_handle, resolver_bearing_handle,
    };
    use crate::object_handle::ObjectValue;
    use crate::pipeline::lzw::pack_codes;
    use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
    use std::cell::{Cell, RefCell};
    use std::io::Cursor;

    #[test]
    fn run_length_encoder_uses_qpdf_two_byte_run() {
        assert_eq!(encode_run_length(b"AA").unwrap(), [0xff, b'A', 0x80]);
    }

    #[test]
    fn scalar_decode_parms_are_reused_for_each_filter() {
        let filter = Object::Array(vec![
            Object::Name(b"FlateDecode".to_vec()),
            Object::Name(b"ASCII85Decode".to_vec()),
        ]);
        let decode_parms = params(&[("Columns", Object::Integer(7))]);

        let specs =
            decode_filter_specs_from_object(Some(&filter), Some(&decode_parms), None).unwrap();

        let replicated = DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Int(7))]);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].decode_params, replicated);
        assert_eq!(specs[1].decode_params, replicated);
    }

    #[test]
    fn decode_parms_array_must_align_with_filter_array() {
        let filter = Object::Array(vec![
            Object::Name(b"FlateDecode".to_vec()),
            Object::Name(b"ASCII85Decode".to_vec()),
        ]);
        let params = Object::Array(vec![Object::Null]);

        let error =
            decode_filter_specs_from_object(Some(&filter), Some(&params), None).unwrap_err();

        assert!(matches!(error, Error::Unsupported(_)));
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream /DecodeParms length is inconsistent with filters"
        );
    }

    #[test]
    fn empty_decode_parms_array_is_null_and_filter_abbreviation_expands() {
        let filter = Object::Name(b"Fl".to_vec());
        let params = Object::Array(Vec::new());

        let specs = decode_filter_specs_from_object(Some(&filter), Some(&params), None).unwrap();

        assert_eq!(specs[0].normalized_name(), b"FlateDecode");
        assert!(specs[0].decode_params.is_absent());
    }

    #[test]
    fn no_filter_ignores_decode_parms() {
        let params = Object::Array(vec![Object::Integer(1)]);

        let specs = decode_filter_specs_from_object(None, Some(&params), None).unwrap();

        assert!(specs.is_empty());
    }

    #[test]
    fn non_name_filter_item_is_rejected_before_decode() {
        let filter = Object::Array(vec![Object::Integer(1)]);

        let error = decode_filter_specs_from_object(Some(&filter), None, None).unwrap_err();

        assert!(matches!(error, Error::Unsupported(_)));
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream filter type is not name or array"
        );
    }

    #[test]
    fn scalar_non_name_filter_is_rejected_before_decode() {
        let error =
            decode_filter_specs_from_object(Some(&Object::Integer(1)), None, None).unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream filter type is not name or array"
        );
    }

    #[test]
    fn empty_filter_array_ignores_decode_parms() {
        let filter = Object::Array(Vec::new());
        let params = Object::Array(vec![Object::Integer(1)]);

        assert!(
            decode_filter_specs_from_object(Some(&filter), Some(&params), None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn object_shape_reader_distinguishes_absent_null_and_present_non_dictionary() {
        let name = Object::Name(b"FlateDecode".to_vec());

        let absent = decode_filter_specs_from_object(Some(&name), None, None).unwrap();
        assert!(matches!(absent[0].decode_params, DecodeParams::Absent));
        assert!(absent[0].decode_params.entries().is_empty());

        let null = decode_filter_specs_from_object(Some(&name), Some(&Object::Null), None).unwrap();
        assert!(matches!(null[0].decode_params, DecodeParams::Absent));

        let scalar = Object::Integer(1);
        let present = decode_filter_specs_from_object(Some(&name), Some(&scalar), None).unwrap();
        // Both halves of this contrast are qpdf's.
        // `SF_FlateLzwDecode::setDecodeParms` reads a non-dictionary through
        // `getKeys()`, which treats it as empty and leaves the filter
        // filterable (`QPDFObjectHandle.cc:997-1009`), but qpdf's base-class
        // default `QPDFStreamFilter::setDecodeParms`
        // (`libqpdf/QPDFStreamFilter.cc:3-7`) is `return decode_parms.isNull()`
        // and rejects that very same object. `Present(vec![])` has to reach
        // both answers, which is why it stays distinct from `Absent`.
        assert!(matches!(present[0].decode_params, DecodeParams::Present(_)));
        assert!(present[0].decode_params.entries().is_empty());
    }

    /// Both halves of the reduction in one read: each value drops to its
    /// bounded [`ParamValue`], and each key outside
    /// [`RETAINED_DECODE_PARAM_KEYS`] drops entirely.
    ///
    /// `/Whatever` carries the same `Object::Null` as the `/Predictor` above
    /// it, which is omitted before bounded reduction. Only the key differs,
    /// so the assertion separates "dropped because unread" from "dropped
    /// because null". The public decode/encode behavior for this null-valued
    /// row is asserted by `filters::tests::null_decode_params_values_are_omitted_before_decode_and_encode`
    /// and by the corpus's "null-valued /DecodeParms key" row.
    ///
    /// **The filter is `/Crypt` so that `/Name` is in the retained set at all**
    /// ([`CRYPT_RETAINED_DECODE_PARAM_KEY`]); under `/FlateDecode` it would
    /// drop as `/Whatever` does and this read would witness no
    /// `ParamValue::Name` at all. Which filter it is changes nothing else
    /// here: the `Object` reader never resolves, so
    /// [`filter_reads_decode_params`] — the other decision a name settles —
    /// cannot reach this path.
    #[test]
    fn object_shape_reader_reduces_each_parameter_value_to_its_bounded_shape() {
        let name = Object::Name(b"Crypt".to_vec());
        let dictionary = params(&[
            // getIntValueAsInt saturates at both ends, so pin both.
            ("Colors", Object::Integer(i64::from(i32::MIN) - 10)),
            ("Columns", Object::Integer(i64::from(i32::MAX) + 10)),
            ("Name", Object::Name(b"Identity".to_vec())),
            ("Predictor", Object::Null),
            ("Whatever", Object::Null),
        ]);

        let specs = decode_filter_specs_from_object(Some(&name), Some(&dictionary), None).unwrap();

        assert_eq!(
            specs[0].decode_params.entries().to_vec(),
            vec![
                (b"Colors".to_vec(), ParamValue::Int(i32::MIN)),
                (b"Columns".to_vec(), ParamValue::Int(i32::MAX)),
                (b"Name".to_vec(), ParamValue::Name(b"Identity".to_vec())),
            ]
        );
    }

    #[test]
    fn object_shape_reader_omits_null_valued_keys_before_retention() {
        let specs = decode_filter_specs_from_object(
            Some(&Object::Name(b"FlateDecode".to_vec())),
            Some(&params(&[
                ("Columns", Object::Integer(4)),
                ("Predictor", Object::Null),
                ("Unused", Object::Null),
            ])),
            None,
        )
        .unwrap();

        assert_eq!(
            specs[0].decode_params,
            DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Int(4))])
        );
    }

    /// Exactly the keys a read keeps, spelled out — and that the set differs
    /// by filter in exactly one key.
    ///
    /// **The expectation is a literal, not [`RETAINED_DECODE_PARAM_KEYS`]
    /// itself.** Reading the constant on both sides would be tautological in
    /// the one direction that matters — deleting a key would shrink
    /// expectation and result together and stay green.
    ///
    /// Shrinking the set is exactly what a "tidy the unused keys" change would
    /// do, so each key needs somewhere to fail. Measured on
    /// `cargo test -p flpdf --lib` **before `/Name` became `Crypt`-only**,
    /// deleting each key in turn: `Predictor` 35 failures, `Columns` 34,
    /// `Name` 5, `BitsPerComponent` and `Colors` 4 each — and `EarlyChange`
    /// just 2, this test and
    /// `filters::tests::lzw_early_change_reaches_the_codec_from_decode_parms`,
    /// which was written for it. Before those two existed, deleting
    /// `EarlyChange` reddened nothing at all: the legacy-vs-native corpus gate
    /// is structurally blind to such a loss, being relative, and dropping a
    /// key moves both readers together.
    ///
    /// The two halves fail in opposite directions, which is what makes the
    /// `Crypt`-only rule falsifiable from here. Measured on
    /// `cargo test -p flpdf --lib`, in each direction:
    ///
    /// - Dropping `/Name` from `retains_decode_param_key` outright reddens the
    ///   `/Crypt` half — five tests, this one plus
    ///   `filters::tests::crypt_stage_receives_the_name_parameter_a_provider_selects_on`,
    ///   [`handle_reader_resolves_a_crypt_decode_parms_value`],
    ///   [`object_shape_reader_reduces_each_parameter_value_to_its_bounded_shape`]
    ///   and
    ///   [`retained_decode_parameter_bytes_do_not_grow_with_a_name_valued_parameter`].
    /// - Dropping the `Crypt` gate from [`is_crypt_name_key`], so `/Name` is
    ///   kept whatever the filter, reddens the `/FlateDecode` half — two, this
    ///   one and that byte test.
    /// - Owning a name payload under *every* retained key — the half-fix that
    ///   leaves `/Predictor /<long name>` amplifying — reddens two others:
    ///   that byte test and
    ///   [`a_non_resolving_read_classifies_direct_values_exactly_as_the_object_reader_does`].
    ///
    /// No codec or encode-path test moved in any of the three, which is the
    /// same enumeration [`CRYPT_RETAINED_DECODE_PARAM_KEY`] states from the
    /// reading side.
    #[test]
    fn the_object_reader_keeps_exactly_the_read_decode_parameter_keys() {
        let dictionary = params(&[
            ("BitsPerComponent", Object::Integer(1)),
            ("Colors", Object::Integer(1)),
            ("Columns", Object::Integer(1)),
            ("EarlyChange", Object::Integer(1)),
            ("Name", Object::Integer(1)),
            ("Predictor", Object::Integer(1)),
            ("Unread", Object::Integer(1)),
        ]);
        let keys = |filter: &[u8]| {
            decode_filter_specs_from_object(
                Some(&Object::Name(filter.to_vec())),
                Some(&dictionary),
                None,
            )
            .unwrap()
            .swap_remove(0)
            .decode_params
            .entries()
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>()
        };

        assert_eq!(
            keys(b"FlateDecode"),
            // `Dictionary` is a `BTreeMap`, so this is the source order too.
            vec![
                b"BitsPerComponent".to_vec(),
                b"Colors".to_vec(),
                b"Columns".to_vec(),
                b"EarlyChange".to_vec(),
                b"Predictor".to_vec(),
            ]
        );
        assert_eq!(
            keys(b"Crypt"),
            vec![
                b"BitsPerComponent".to_vec(),
                b"Colors".to_vec(),
                b"Columns".to_vec(),
                b"EarlyChange".to_vec(),
                b"Name".to_vec(),
                b"Predictor".to_vec(),
            ]
        );
    }

    /// The bytes a read holds onto: each filter name, plus each retained key
    /// and the payload of each retained `Name`. `Int` and `Other` carry
    /// nothing beyond the enum itself.
    ///
    /// Deliberately the quantity the 2026-08-03 measurement reported, so its
    /// figures and this test's are the same number.
    fn retained_bytes(specs: &[FilterSpec]) -> usize {
        specs
            .iter()
            .map(|spec| {
                spec.name.len()
                    + spec
                        .decode_params
                        .entries()
                        .iter()
                        .map(|(key, value)| {
                            key.len()
                                + match value {
                                    ParamValue::Name(name) => name.len(),
                                    ParamValue::Int(_) | ParamValue::Other => 0,
                                }
                        })
                        .sum::<usize>()
            })
            .sum()
    }

    /// The cost [`RETAINED_DECODE_PARAM_KEYS`] exists to bound.
    ///
    /// **Not a qpdf-parity test.** qpdf replicates a `QPDFObjectHandle` — a
    /// `shared_ptr` — across the chain and copies no dictionary, so there is no
    /// parity claim here in either direction. This is flpdf's own cost,
    /// introduced when `FilterSpec` stopped borrowing `&'a Object` and started
    /// owning a neutral [`DecodeParams`]: a scalar `/DecodeParms` under an
    /// *n*-entry `/Filter` array was then converted and stored *n* times, on
    /// `decode_filter_specs_from_object` — the legacy `decode_stream_data`
    /// path, which allocated nothing at all before.
    ///
    /// Asserted two ways, both absolute: the identical figure at two source
    /// sizes, and a ceiling. A ratio would pass while both sides scaled.
    ///
    /// This grows the *number* of unread keys.
    /// [`retained_decode_parameter_bytes_do_not_grow_with_a_name_valued_parameter`]
    /// grows the *length of the retained values* instead, which is the
    /// dimension `RETAINED_DECODE_PARAM_KEYS` alone never bounded.
    #[test]
    fn retained_decode_parameter_bytes_do_not_grow_with_the_source_dictionary() {
        const CHAIN: usize = 16;
        // 16 stages, each holding at most the five retained keys (16 bytes at
        // the longest) and a 14-byte filter name: 16 * (5 * 16 + 14) = 1504.
        const CEILING: usize = 4096;

        let specs_for = |filler: usize| {
            let mut dictionary = Dictionary::new();
            // Every retained key, so this bounds a real retention rather than
            // the empty set an all-unread dictionary would satisfy trivially.
            for key in RETAINED_DECODE_PARAM_KEYS {
                dictionary.insert(key, Object::Integer(1));
            }
            // `/Name` is retained only under `/Crypt`, so beneath the
            // `ASCIIHexDecode` chain below it is simply one more unread key.
            dictionary.insert("Name", Object::Name(vec![b'v'; 64]));
            // 64-byte keys with 64-byte `/Name` values, as measured.
            for index in 0..filler {
                dictionary.insert(format!("Unread{index:058}"), Object::Name(vec![b'v'; 64]));
            }
            // `ASCIIHexDecode` reads no `/DecodeParms` entry whatsoever, so
            // not one of these bytes can reach a decode decision.
            let filter = Object::Array(vec![Object::Name(b"ASCIIHexDecode".to_vec()); CHAIN]);

            decode_filter_specs_from_object(
                Some(&filter),
                Some(&Object::Dictionary(dictionary)),
                None,
            )
            .unwrap()
        };

        let small = retained_bytes(&specs_for(1));
        let large = retained_bytes(&specs_for(1024));

        assert_eq!(
            small, large,
            "retention grew with the source dictionary: {small} bytes at 1 unread key, \
             {large} at 1024"
        );
        assert!(
            large <= CEILING,
            "retained {large} bytes, ceiling {CEILING}"
        );
    }

    /// The bound the `Crypt` payload rule exists to state: under a filter that
    /// is not `Crypt`, a chain holds the *same* number of bytes however long
    /// the source's names are — in **every** retained slot at once, not only
    /// under `/Name`.
    ///
    /// **This is the dimension the retained key set never bounded.** The keys
    /// come from [`RETAINED_DECODE_PARAM_KEYS`], a fixed array, so their total
    /// was constant from the start;
    /// [`retained_decode_parameter_bytes_do_not_grow_with_the_source_dictionary`]
    /// already pinned that. `ParamValue::Name(Vec<u8>)` is the one field that
    /// owns a heap buffer of the input's own size, and a scalar `/DecodeParms`
    /// replicated across the chain cloned it once per stage.
    ///
    /// **Every retained key carries a name, not just `/Name`.** Restricting the
    /// *key* `/Name` to `Crypt` does not bound this on its own, because a name
    /// value fits any slot: `/Predictor /<one-mebibyte-name>` passes the key
    /// test too. Measured against that half-fix — `/Name` `Crypt`-only but a
    /// name payload owned wherever it appears — this chain retained 16,777,584
    /// bytes. Filling only one slot would let the next forgotten slot pass.
    ///
    /// **All three shape readers are measured**, because the legacy `Object`
    /// route [`decode_params_from_object`], consuming handle route
    /// [`decode_params_from_consuming_handle`], and non-consuming handle route
    /// [`decode_params_from_entries`] apply retention and classification
    /// separately. The corpus's "/Name under a filter that is not Crypt" row
    /// catches a rule applied to only one of these routes — measured, forcing
    /// the non-consuming handle route's retention gate to `true` reddens
    /// [`handle_reader_matches_object_reader_for_every_filter_shape`] naming
    /// exactly that row — but it is a *relative* gate and would stay green if
    /// the rule were dropped from all three routes together. This test is the
    /// absolute one.
    ///
    /// Both a non-consuming and a consuming filter are measured, because they
    /// classify through different functions
    /// ([`param_value_without_resolving`] and [`param_value_from_handle`]).
    ///
    /// The figures are exact constants, not ratios — a ratio would pass while
    /// both sides scaled.
    #[test]
    fn retained_decode_parameter_bytes_do_not_grow_with_a_name_valued_parameter() {
        const CHAIN: usize = 16;
        // The five retained keys are `BitsPerComponent` 16 + `Colors` 6 +
        // `Columns` 7 + `EarlyChange` 11 + `Predictor` 9 = 49 bytes of key
        // text, each carrying a `ParamValue` that owns nothing on the heap.
        const KEYS: usize = 49;

        let all_slots_named = |name_len: usize| {
            let mut dictionary = Dictionary::new();
            for key in RETAINED_DECODE_PARAM_KEYS {
                dictionary.insert(key, Object::Name(vec![b'v'; name_len]));
            }
            dictionary.insert("Name", Object::Name(vec![b'v'; name_len]));
            Object::Dictionary(dictionary)
        };
        let both_readers = |filter: &Object, parms: &Object| {
            let from_object =
                decode_filter_specs_from_object(Some(filter), Some(parms), None).unwrap();
            let from_handle = decode_filter_specs_from_handle(
                &handle_from_object(Some(filter)),
                &handle_from_object(Some(parms)),
                None,
            )
            .unwrap();
            assert_eq!(from_object, from_handle, "the two readers disagreed");
            from_object
        };

        // One scalar `/DecodeParms` replicated across the chain: the shape the
        // review comments named, where a retained name is cloned per stage.
        // `ASCIIHexDecode` reads no entry, `FlateDecode` reads five.
        for filter in [b"ASCIIHexDecode".as_slice(), b"FlateDecode".as_slice()] {
            let expected = CHAIN * (filter.len() + KEYS);
            let chain = Object::Array(vec![Object::Name(filter.to_vec()); CHAIN]);
            // Built eagerly rather than as an `assert_eq!` message argument,
            // which would only ever run on failure.
            let named = String::from_utf8_lossy(filter).into_owned();
            for name_len in [16, 1 << 20] {
                let retained = retained_bytes(&both_readers(&chain, &all_slots_named(name_len)));
                assert_eq!(
                    retained, expected,
                    "retained {retained} bytes under {named} at name length {name_len}"
                );
            }
        }

        // The other half of the rule, so this cannot be satisfied by dropping
        // every payload: a `Crypt` stage still receives the name its provider
        // selects on, and there retention does track its length. That is the
        // authorized per-stage residual, so the figure grows with the name
        // where the others do not — and only for `/Name`, so the four other
        // slots' mebibyte names are still absent from it.
        for name_len in [16, 1 << 20] {
            let crypt = both_readers(&Object::Name(b"Crypt".to_vec()), &all_slots_named(name_len));
            assert!(crypt[0]
                .decode_params
                .entries()
                .contains(&(b"Name".to_vec(), ParamValue::Name(vec![b'v'; name_len]))));
            // 5 bytes of filter name, 49 of geometry keys, `Name` itself, and
            // its payload — once.
            assert_eq!(retained_bytes(&crypt), 5 + KEYS + 4 + name_len);
        }
    }

    // ----- flpdf-25kg.3.4 Task 5: the ObjectHandle-native shape reader -----

    /// Every `/Filter` + `/DecodeParms` shape `QPDF_Stream::filterable`
    /// (`libqpdf/QPDF_Stream.cc:379-484`) distinguishes, written once and fed
    /// to both shape readers.
    ///
    /// **This corpus is DIRECT-ONLY, and that limit is load-bearing.** Plan
    /// decision D1 of `flpdf-25kg.3.4` makes the two readers *deliberately*
    /// disagree on an indirect child: the `Object` reader classifies
    /// `Object::Reference` as `ParamValue::Other` (or as a non-name filter
    /// item), while the handle reader dereferences it, which the 2026-08-03
    /// live-qpdf probe confirmed is what qpdf does. (One position is narrower:
    /// a `/DecodeParms` dictionary value under a filter that does not read
    /// entries is *not* dereferenced, so there the two readers agree — see
    /// [`filter_reads_decode_params`]. The disagreement still holds for every
    /// other position, which is why an indirect row is barred outright rather
    /// than case by case.) Adding an indirect row here would therefore assert
    /// the wrong thing; indirect coverage lives in the
    /// `handle_reader_dereferences_*` /
    /// `handle_reader_surfaces_a_dropped_document_*` tests below.
    pub(crate) fn shape_corpus() -> Vec<(&'static str, Option<Object>, Option<Object>)> {
        let flate = || Object::Name(b"FlateDecode".to_vec());
        let ascii85 = || Object::Name(b"ASCII85Decode".to_vec());
        let two_filters = || Object::Array(vec![flate(), ascii85()]);
        let geometry = || {
            params(&[
                ("BitsPerComponent", Object::Integer(8)),
                ("Colors", Object::Integer(3)),
                ("Columns", Object::Integer(4)),
                ("EarlyChange", Object::Integer(0)),
                ("Predictor", Object::Integer(12)),
            ])
        };
        let mut overlong = vec![flate(); 16];
        overlong.push(Object::Integer(1));

        vec![
            ("absent /Filter", None, None),
            ("null /Filter", Some(Object::Null), None),
            ("name /Filter", Some(flate()), None),
            ("abbreviation Fl", Some(Object::Name(b"Fl".to_vec())), None),
            (
                "abbreviation AHx",
                Some(Object::Name(b"AHx".to_vec())),
                None,
            ),
            ("array of names", Some(two_filters()), None),
            (
                "array with a non-name item",
                Some(Object::Array(vec![flate(), Object::Integer(1)])),
                None,
            ),
            (
                "non-name non-array scalar /Filter",
                Some(Object::Integer(1)),
                None,
            ),
            // A dictionary `/Filter` is the one non-name non-array shape the
            // handle reader could tell apart from the object reader, since it
            // is the only shape reachable by an accessor the handle reader has
            // and the branch does not use (`try_as_dictionary`). Both must
            // still land on `FILTER_TYPE_ERROR`.
            (
                "dictionary /Filter",
                Some(params(&[("Predictor", Object::Integer(12))])),
                None,
            ),
            (
                "empty /Filter array ignores /DecodeParms",
                Some(Object::Array(Vec::new())),
                Some(Object::Array(vec![Object::Integer(1)])),
            ),
            (
                "empty /DecodeParms array",
                Some(flate()),
                Some(Object::Array(Vec::new())),
            ),
            ("null /DecodeParms", Some(flate()), Some(Object::Null)),
            (
                "aligned /DecodeParms array",
                Some(two_filters()),
                Some(Object::Array(vec![geometry(), geometry()])),
            ),
            (
                "aligned /DecodeParms array with a null item",
                Some(two_filters()),
                Some(Object::Array(vec![geometry(), Object::Null])),
            ),
            (
                "misaligned /DecodeParms array",
                Some(two_filters()),
                Some(Object::Array(vec![Object::Null])),
            ),
            // The one row where the *trailing* chain count competes with the
            // `/DecodeParms` mismatch: a scalar `/Filter` never reaches the
            // array arm's count, so under `Some(0)` only a reader that still
            // counts last reports the mismatch.
            (
                "scalar /Filter with a misaligned /DecodeParms array",
                Some(flate()),
                Some(Object::Array(vec![geometry(), geometry()])),
            ),
            (
                "scalar /DecodeParms replicated across two filters",
                Some(two_filters()),
                Some(geometry()),
            ),
            (
                "present non-dictionary /DecodeParms",
                Some(flate()),
                Some(Object::Integer(1)),
            ),
            ("full parameter dictionary", Some(flate()), Some(geometry())),
            (
                "a non-integer value for each parameter",
                Some(flate()),
                Some(params(&[
                    ("BitsPerComponent", Object::Boolean(true)),
                    ("Colors", Object::Real(3.5)),
                    ("Columns", Object::String(b"4".to_vec())),
                    ("EarlyChange", Object::Array(Vec::new())),
                    ("Predictor", Object::Name(b"Up".to_vec())),
                ])),
            ),
            // Decision D4: qpdf's `QPDF_Dictionary::getKeys`
            // (`libqpdf/QPDF_Dictionary.cc:118-127`) skips every null-valued
            // entry, so qpdf tolerates this. Both readers must preserve the
            // qpdf-compatible success path; this row is tracked as beads
            // `flpdf-h8mv`.
            (
                "null-valued /DecodeParms key (flpdf-h8mv)",
                Some(flate()),
                Some(params(&[("Predictor", Object::Null)])),
            ),
            (
                "Crypt filter",
                Some(Object::Name(b"Crypt".to_vec())),
                Some(params(&[("Name", Object::Name(b"Identity".to_vec()))])),
            ),
            // The same `/DecodeParms` under a filter that is not `Crypt`.
            // `CRYPT_RETAINED_DECODE_PARAM_KEY` makes the two rows differ —
            // this one retains nothing — and nothing else in this corpus pairs
            // a `/Name` with a non-`Crypt` filter, so without it a retention
            // rule applied to only one reader would go unnoticed here.
            (
                "/Name under a filter that is not Crypt",
                Some(flate()),
                Some(params(&[("Name", Object::Name(b"Identity".to_vec()))])),
            ),
            (
                "unknown filter name",
                Some(Object::Name(b"NoSuchDecode".to_vec())),
                None,
            ),
            (
                "over-long chain whose last item is also a non-name",
                Some(Object::Array(overlong)),
                None,
            ),
        ]
    }

    /// The direct handle form of a corpus `Object`. It covers exactly the
    /// shapes [`shape_corpus`] uses and panics on the rest, so `Reference` —
    /// the one shape the direct-only rule genuinely forbids — cannot slip into
    /// a row by accident. The other rejected shapes (`Stream`, `RealLiteral`,
    /// `Operator`, `InlineImage`) are direct and merely unused; widening this
    /// to admit one is fine, adding `Reference` is not.
    pub(crate) fn handle_from_object(object: Option<&Object>) -> ObjectHandle {
        match object {
            None | Some(Object::Null) => ObjectHandle::null(),
            Some(Object::Boolean(value)) => ObjectHandle::boolean(*value),
            Some(Object::Integer(value)) => ObjectHandle::integer(*value),
            Some(Object::Real(value)) => ObjectHandle::real(*value),
            Some(Object::Name(name)) => ObjectHandle::name(name.clone()),
            Some(Object::String(bytes)) => ObjectHandle::string(bytes.clone()),
            Some(Object::Array(items)) => ObjectHandle::array(
                items
                    .iter()
                    .map(|item| handle_from_object(Some(item)))
                    .collect(),
            ),
            Some(Object::Dictionary(dictionary)) => ObjectHandle::dictionary(
                dictionary
                    .iter()
                    .map(|(key, value)| (key.to_vec(), handle_from_object(Some(value))))
                    .collect(),
            ),
            Some(other) => panic!(
                "{other:?} is outside the shape corpus: `Reference` is barred \
                 by the direct-only rule, and the rest are direct but have no \
                 row here"
            ),
        }
    }

    /// Nothing in [`shape_corpus`] reaches the guard arm above, so without this
    /// test it is unexecuted code. Deleting the arm is not the risk — the match
    /// would stop being exhaustive and fail to compile. Weakening it is:
    /// swapping the `panic!` for a permissive fallback such as
    /// `ObjectHandle::null()` compiles and keeps every other test green while
    /// silently admitting an `Object::Reference` row. That would matter,
    /// because plan decision D1 of `flpdf-25kg.3.4` makes the two readers
    /// diverge on an indirect child *by design* — the `Object` reader sees
    /// `ParamValue::Other`, the handle reader dereferences — so such a row
    /// would make
    /// [`handle_reader_matches_object_reader_for_every_filter_shape`] assert
    /// agreement that qpdf itself does not have.
    #[test]
    #[should_panic(expected = "is outside the shape corpus")]
    fn handle_from_object_refuses_an_indirect_reference() {
        handle_from_object(Some(&Object::Reference(crate::ObjectRef::new(4, 0))));
    }

    /// `Error` is not `PartialEq`, so compare the message instead — the two
    /// readers must agree on `Ok`/`Err` *and* on the exact text.
    fn comparable(result: Result<Vec<FilterSpec>>) -> std::result::Result<Vec<FilterSpec>, String> {
        result.map_err(|error| error.to_string())
    }

    #[test]
    fn handle_reader_matches_object_reader_for_every_filter_shape() {
        for (label, filter, parms) in shape_corpus() {
            let filter_handle = handle_from_object(filter.as_ref());
            let parms_handle = handle_from_object(parms.as_ref());
            // Every limit, because the two chain counts only show up under a
            // cap — `Some(16)` reaches the array arm's count, `Some(0)` also
            // reaches the trailing one — while every other row must be
            // unaffected by a cap at all.
            for max_filter_chain in [None, Some(16), Some(0)] {
                let from_object = comparable(decode_filter_specs_from_object(
                    filter.as_ref(),
                    parms.as_ref(),
                    max_filter_chain,
                ));
                let from_handle = comparable(decode_filter_specs_from_handle(
                    &filter_handle,
                    &parms_handle,
                    max_filter_chain,
                ));

                assert_eq!(
                    from_object, from_handle,
                    "shape {label:?} diverged at max_filter_chain {max_filter_chain:?}"
                );
            }
        }
    }

    #[test]
    fn handle_reader_counts_the_raw_filter_array_before_inspecting_its_items() {
        // The cap is checked on the array as parsed, so an over-long chain is
        // reported ahead of the trailing non-name item — the same precedence
        // `decode_rejects_overlong_filter_chain_before_malformed_item`
        // (`filters.rs`) pins for the `Object` reader. Asserting the message
        // (not just that the two readers agree) is what makes this absolute.
        // NOTE for anyone copying this into a bigger corpus: `vec![handle; n]`
        // clones one `Rc`, so all 16 entries are the *same slot*, not 16
        // distinct children. Harmless here — the cap fires before any item is
        // inspected — but with indirect children it would share resolution
        // state between supposedly independent items and pass for the wrong
        // reason. Build such a vector with `(0..n).map(|_| ...)` instead.
        let mut items = vec![ObjectHandle::name(b"FlateDecode".to_vec()); 16];
        items.push(ObjectHandle::integer(1));

        let error = decode_filter_specs_from_handle(
            &ObjectHandle::array(items),
            &ObjectHandle::null(),
            Some(16),
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: filter chain length 17 exceeds maximum of 16"
        );
    }

    #[test]
    fn handle_reader_decides_both_array_rejections_from_the_length_alone() {
        // Both array positions are settled by `try_array_len` — qpdf's
        // `getArrayNItems`, read in place off the borrowed array
        // (`libqpdf/QPDF_Stream.cc:398` for `/Filter`, `:443`/`:447` for
        // `/DecodeParms`) — so a rejected array is never snapshotted.
        //
        // The *absence* of that snapshot is deliberately not asserted, because
        // it is not observable from a test: cloning a `Vec<ObjectHandle>` runs
        // no user code (it is `Rc` pointer clones only) and the clone is
        // dropped before the error returns, so neither a resolver call count
        // nor an `ObjectHandle::strong_count` sample can separate the two
        // shapes. Only a `#[global_allocator]` probe could, and installing one
        // across this crate's whole test binary is out of proportion to a
        // resource fix. What this pins instead is the decision that makes
        // dropping the snapshot legal — each rejection is reached from the
        // count and from nothing else — at a length where the snapshot would
        // actually have cost something.
        let long_chain: Vec<ObjectHandle> = (0..4096)
            .map(|_| ObjectHandle::name(b"FlateDecode".to_vec()))
            .collect();

        // The trailing non-name keeps this on the *array arm's* count: without
        // it the count that runs after the `/DecodeParms` branch would produce
        // the identical message, and the assertion would not pin which count
        // ran — only that some count did.
        let mut with_a_non_name = long_chain.clone();
        with_a_non_name.push(ObjectHandle::integer(1));
        assert_eq!(
            decode_filter_specs_from_handle(
                &ObjectHandle::array(with_a_non_name),
                &ObjectHandle::null(),
                Some(16),
            )
            .unwrap_err()
            .to_string(),
            "unsupported PDF feature: filter chain length 4097 exceeds maximum of 16"
        );

        assert_eq!(
            decode_filter_specs_from_handle(
                &ObjectHandle::name(b"FlateDecode".to_vec()),
                &ObjectHandle::array(long_chain),
                None,
            )
            .unwrap_err()
            .to_string(),
            "unsupported PDF feature: stream /DecodeParms length is inconsistent with filters"
        );
    }

    #[test]
    fn handle_reader_keeps_the_non_array_answer_its_length_accessor_gives_it() {
        // `try_array_len` answers `None` for a non-array where qpdf's
        // `getArrayNItems` warns and returns 0
        // (`libqpdf/QPDFObjectHandle.cc:763-766`). Both call sites lean on
        // that: `Some(0)` would turn a scalar `/Filter` into an empty chain —
        // an accepted unfiltered stream in place of a rejected document — and
        // would collapse a scalar `/DecodeParms` from `Present` to `Absent`,
        // erasing the distinction `QPDFStreamFilter::setDecodeParms`
        // (`libqpdf/QPDFStreamFilter.cc:3-7`) rejects on. The divergence is
        // pre-existing — `try_as_array` already answered `None` — and is
        // pinned here so the length accessor cannot quietly adopt qpdf's
        // treat-as-empty while looking like a pure resource change.
        assert_eq!(
            decode_filter_specs_from_handle(&ObjectHandle::integer(1), &ObjectHandle::null(), None)
                .unwrap_err()
                .to_string(),
            "unsupported PDF feature: stream filter type is not name or array"
        );

        let specs = decode_filter_specs_from_handle(
            &ObjectHandle::name(b"FlateDecode".to_vec()),
            &ObjectHandle::integer(1),
            None,
        )
        .expect("a non-array /DecodeParms is replicated per filter, not rejected");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].decode_params, DecodeParams::Present(Vec::new()));
    }

    #[test]
    fn both_readers_count_a_scalar_filter_against_the_chain_cap_too() {
        // The array arm's count never sees a scalar `/Filter`, so the count
        // that runs after the `/DecodeParms` branch is the only one left to
        // catch it. This is a live public-API path, not a vestigial one:
        // `DecodeLimits` is `pub` with a `pub max_filter_chain`, so
        // `decode_stream_data_with_limits(&dict, data, DecodeLimits {
        // max_filter_chain: Some(0), .. })` reaches it from outside the crate.
        // It carried over from `filters.rs`'s
        // `validate_filter_chain_count(specs.len(), ...)`; pin it here so
        // moving it into the readers did not silently drop it.
        let name = Object::Name(b"FlateDecode".to_vec());
        let expected = "unsupported PDF feature: filter chain length 1 exceeds maximum of 0";

        assert_eq!(
            decode_filter_specs_from_object(Some(&name), None, Some(0))
                .unwrap_err()
                .to_string(),
            expected
        );
        assert_eq!(
            decode_filter_specs_from_handle(
                &ObjectHandle::name(b"FlateDecode".to_vec()),
                &ObjectHandle::null(),
                Some(0),
            )
            .unwrap_err()
            .to_string(),
            expected
        );
    }

    #[test]
    fn both_readers_report_a_decode_parms_mismatch_ahead_of_the_trailing_chain_count() {
        // The absolute half of the corpus's "scalar /Filter with a misaligned
        // /DecodeParms array" row. That row only pins that the two readers
        // *agree*, so hoisting the trailing count above the /DecodeParms
        // branch in both readers would leave it green. This pins which error
        // wins, which is pre-Task-5 `filters.rs` behavior worth preserving.
        let name = Object::Name(b"FlateDecode".to_vec());
        let misaligned = Object::Array(vec![Object::Null, Object::Null]);
        let expected =
            "unsupported PDF feature: stream /DecodeParms length is inconsistent with filters";

        assert_eq!(
            decode_filter_specs_from_object(Some(&name), Some(&misaligned), Some(0))
                .unwrap_err()
                .to_string(),
            expected
        );
        assert_eq!(
            decode_filter_specs_from_handle(
                &ObjectHandle::name(b"FlateDecode".to_vec()),
                &ObjectHandle::array(vec![ObjectHandle::null(), ObjectHandle::null()]),
                Some(0),
            )
            .unwrap_err()
            .to_string(),
            expected
        );
    }

    // ----- Decision D1: the handle reader dereferences its children -----
    //
    // `QPDF_Stream::filterable` reaches every child through a
    // `QPDFObjectHandle` accessor — `getKey("/Filter")` (`QPDF_Stream.cc:386`),
    // `filter_obj.getArrayItem(i)` (`:400`), `getKey("/DecodeParms")` (`:441`),
    // `decode_obj.getArrayItem(i)` (`:448`) — and each of `isNull`, `isName`,
    // `isArray`, `isInteger` dereferences before it inspects. That is the whole
    // reason this reader exists, and it is the one property Task 7's
    // direct-only equivalence gate structurally cannot test.
    //
    // Each case below starts from an *unresolved* indirect handle in one child
    // position, so the value that comes back proves that position resolved.
    //
    // The `/DecodeParms` dictionary-value position is the one exception, and
    // it is qpdf's, not flpdf's: only `SF_FlateLzwDecode::setDecodeParms`
    // reaches a value (`SF_FlateLzwDecode.cc:29-31`), so that position is
    // resolved iff `filter_reads_decode_params` holds. Its tests come in
    // pairs — a Flate case proving resolution, an ASCIIHex case proving its
    // absence — and the negative half asserts a *call count*, because "never
    // looked" is not observable in the returned value alone.
    //
    // **What that does and does not pin.** A per-site mutation matrix,
    // replacing each resolving access in `decode_filter_specs_from_handle` and
    // its helpers with its non-resolving counterpart, one at a time, kills the
    // following five positions:
    //
    // - `filter.try_is_null` —
    //   `handle_reader_reads_an_indirect_filter_resolving_to_null_as_no_filters`
    // - the `/Filter` item's `try_as_name` —
    //   `handle_reader_dereferences_an_indirect_filter_array_and_its_items`
    // - `decode_params.try_is_null` —
    //   `handle_reader_reads_an_indirect_scalar_decode_parms_resolving_to_null_as_absent`
    // - `decode_params_from_handle`'s `try_is_null` —
    //   `handle_reader_dereferences_a_decode_parms_array_item_that_resolves_to_null`
    // - `decode_params_from_consuming_handle`'s `try_get_keys` —
    //   `handle_reader_resolves_an_unretained_decode_parms_value_for_a_filter_that_reads_them`
    //
    // The accessor calls that remain survive as *already-resolved* sites, and
    // that is not a bug:
    // `try_dereference` is idempotent, so `filter.try_as_name`/`try_as_array`,
    // `decode_params.try_as_array`, the non-consuming `try_as_dictionary`
    // snapshot sites, and `param_value_from_handle`'s
    // `try_as_integer`/`try_as_name` each inspect a slot an earlier `try_*` at
    // the same position already resolved, and behave identically to their
    // non-resolving twins. `param_value_from_handle` is reached only after
    // `try_get_keys` has resolved and filtered children.
    //
    // `filter.try_array_len` and `decode_params.try_array_len` have no
    // non-resolving twin to swap in. The
    // equivalent mutation is deleting `try_dereference` from the accessor
    // itself, which likewise survives here and is killed instead by
    // `object_handle`'s own
    // `try_array_len_resolves_an_indirect_array_through_its_document`.
    //
    // The matrix covers only those resolving positions. It says nothing
    // about `param_value_without_resolving`, which has no `try_*` to mutate:
    // the mutation that discriminates *it* is routing a non-consuming stage
    // through the consuming helper, which the paired tests above cover in both
    // directions. Nor does it cover
    // *where* `replicated_decode_params` takes its one dictionary snapshot —
    // moving that call back inside the per-filter loop leaves the whole suite
    // green, because a snapshot count is not observable through any harness
    // here. What is observable is the snapshot staying live, which
    // `handle_reader_lets_a_later_stage_see_a_value_an_earlier_stage_resolved`
    // pins.
    //
    // Every site stays `try_*` for uniformity and for robustness against a
    // future reordering — not because a test can tell each one apart today.
    // Read the tests below as guarding every child *position*, not every call
    // site.

    #[test]
    fn handle_reader_dereferences_an_indirect_filter_name() {
        let (filter, _resolver) = resolver_bearing_handle(ObjectValue::Name(b"Fl".to_vec()));
        assert!(!filter.is_resolved());

        let specs = decode_filter_specs_from_handle(&filter, &ObjectHandle::null(), None).unwrap();

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].normalized_name(), b"FlateDecode");
        assert!(filter.is_resolved());
    }

    #[test]
    fn handle_reader_reads_an_indirect_filter_resolving_to_null_as_no_filters() {
        // `filter_obj.isNull()` (`libqpdf/QPDF_Stream.cc:391`) dereferences, so
        // a dangling `/Filter` reference reads as "no filters" rather than as
        // a bad filter type. That is the shape a broken reference takes once
        // `flpdf-25kg.3.5` wires the resolver: `set_missing` resolves the slot
        // to null (pinned by `set_missing_marks_the_handle_resolved_to_null`,
        // `object_handle.rs`).
        let (filter, _resolver) = resolver_bearing_handle(ObjectValue::Null);

        let specs = decode_filter_specs_from_handle(&filter, &ObjectHandle::null(), None).unwrap();

        assert!(specs.is_empty());
    }

    #[test]
    fn handle_reader_dereferences_an_indirect_filter_array_and_its_items() {
        let (item, _item_resolver) =
            resolver_bearing_handle(ObjectValue::Name(b"ASCII85Decode".to_vec()));
        let (filter, _filter_resolver) = resolver_bearing_handle(ObjectValue::Array(vec![
            ObjectHandle::name(b"FlateDecode".to_vec()),
            item,
        ]));

        let specs = decode_filter_specs_from_handle(&filter, &ObjectHandle::null(), None).unwrap();

        assert_eq!(
            specs
                .iter()
                .map(|spec| spec.name.clone())
                .collect::<Vec<_>>(),
            vec![b"FlateDecode".to_vec(), b"ASCII85Decode".to_vec()]
        );
    }

    #[test]
    fn handle_reader_dereferences_an_indirect_decode_parms_dictionary_and_its_values() {
        let (columns, _columns_resolver) = resolver_bearing_handle(ObjectValue::Integer(4));
        let (parms, _parms_resolver) = resolver_bearing_handle(ObjectValue::Dictionary(
            [
                (b"Columns".to_vec(), columns),
                (b"Predictor".to_vec(), ObjectHandle::integer(12)),
            ]
            .into_iter()
            .collect(),
        ));

        let specs = decode_filter_specs_from_handle(
            &ObjectHandle::name(b"FlateDecode".to_vec()),
            &parms,
            None,
        )
        .unwrap();

        assert_eq!(
            specs[0].decode_params,
            DecodeParams::Present(vec![
                (b"Columns".to_vec(), ParamValue::Int(4)),
                (b"Predictor".to_vec(), ParamValue::Int(12)),
            ])
        );
    }

    #[test]
    fn handle_reader_omits_direct_indirect_and_missing_null_valued_keys() {
        let (indirect_null, _resolver) = resolver_bearing_handle(ObjectValue::Null);
        let missing = ObjectHandle::new_indirect_unresolved(ObjectRef::new(21, 0), -1);
        missing.set_missing();
        let parms = ObjectHandle::dictionary(vec![
            (b"Columns".to_vec(), ObjectHandle::integer(4)),
            (b"Predictor".to_vec(), ObjectHandle::null()),
            (b"Colors".to_vec(), indirect_null.clone()),
            (b"BitsPerComponent".to_vec(), missing.clone()),
        ]);

        let specs = decode_filter_specs_from_handle(
            &ObjectHandle::name(b"FlateDecode".to_vec()),
            &parms,
            None,
        )
        .unwrap();

        assert!(indirect_null.is_resolved());
        assert!(missing.is_resolved());
        assert_eq!(
            specs[0].decode_params,
            DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Int(4))])
        );
    }

    #[test]
    fn handle_reader_dereferences_an_indirect_decode_parms_array_and_its_items() {
        let (item, _item_resolver) = resolver_bearing_handle(ObjectValue::Dictionary(
            [(b"Columns".to_vec(), ObjectHandle::integer(7))]
                .into_iter()
                .collect(),
        ));
        let (parms, _parms_resolver) = resolver_bearing_handle(ObjectValue::Array(vec![item]));

        let specs = decode_filter_specs_from_handle(
            &ObjectHandle::name(b"FlateDecode".to_vec()),
            &parms,
            None,
        )
        .unwrap();

        assert_eq!(
            specs[0].decode_params,
            DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Int(7))])
        );
    }

    #[test]
    fn handle_reader_dereferences_a_decode_parms_array_item_that_resolves_to_null() {
        // `SF_FlateLzwDecode::setDecodeParms` early-returns on `isNull()`
        // (`libqpdf/SF_FlateLzwDecode.cc:24-26`), and `isNull` dereferences —
        // so an indirect item pointing at a null object is `Absent`, not a
        // present non-dictionary.
        let (item, _item_resolver) = resolver_bearing_handle(ObjectValue::Null);

        let specs = decode_filter_specs_from_handle(
            &ObjectHandle::name(b"FlateDecode".to_vec()),
            &ObjectHandle::array(vec![item]),
            None,
        )
        .unwrap();

        assert_eq!(specs[0].decode_params, DecodeParams::Absent);
    }

    // ----- The per-filter boundary on /DecodeParms *values* -----
    //
    // The decisive pair. Both build the identical shape — a direct
    // `/DecodeParms` dictionary holding one indirect value — and differ only
    // in `/Filter`, so the only thing the assertion can be reading is the
    // per-filter decision. Keeping `/Filter` and the dictionary direct means
    // the value is the only handle that *could* resolve, which is what makes
    // the log length unambiguous.

    #[test]
    fn handle_reader_never_resolves_a_decode_parms_value_for_a_filter_that_ignores_them() {
        // `ASCIIHexDecode` inherits `QPDFStreamFilter::setDecodeParms`
        // (`libqpdf/QPDFStreamFilter.cc:3-7`), whose whole body is
        // `return decode_parms.isNull();` — it never reaches an entry. The
        // 2026-08-03 qpdf 11.9.0 probe (`/Filter /ASCIIHexDecode /DecodeParms
        // << /Unused 99 0 R >>`, object 99 dangling) exits 2 with "unable to
        // filter stream data" and reports nothing about object 99 — the
        // observable half only; see `filter_reads_decode_params` for why a
        // probe cannot decide the resolution question and the source can.
        //
        // A returned value cannot express "never looked" — an unresolved
        // handle and a resolved non-integer both read as `Other`. The call log
        // can, which is why this test asserts on it.
        let (value, _resolver, calls) = logged_resolver_bearing_handle(ObjectValue::Integer(4));
        let parms = ObjectHandle::dictionary(vec![(b"Unused".to_vec(), value.clone())]);

        let specs = decode_filter_specs_from_handle(
            &ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
            &parms,
            None,
        )
        .unwrap();

        // Compared against an empty vector rather than asserted `is_empty()`,
        // so a regression prints which object flpdf fetched. qpdf fetches
        // none.
        assert_eq!(*calls.borrow(), Vec::new());
        assert!(!value.is_resolved());
        // `/Unused` is outside `RETAINED_DECODE_PARAM_KEYS`, so no entry
        // survives — but the parameter set is still `Present`, and
        // `set_decode_params`'s `is_absent()` reads exactly that, so the
        // stream is refused just as qpdf's base implementation refuses it.
        assert_eq!(specs[0].decode_params, DecodeParams::Present(Vec::new()));
        assert!(!specs[0].decode_params.is_absent());
    }

    #[test]
    fn handle_reader_resolves_an_unretained_decode_parms_value_for_a_filter_that_reads_them() {
        // Retention is not resolution, and this is the test that keeps the two
        // apart. It is the sibling above with one byte changed — the filter —
        // so the returned `DecodeParams` is identical (`/Unused` survives
        // neither read) and the *only* thing that differs is the call log.
        //
        // qpdf resolves this object. `SF_FlateLzwDecode::setDecodeParms` walks
        // `decode_parms.getKeys()` (`SF_FlateLzwDecode.cc:29-31`), and
        // `QPDF_Dictionary::getKeys` tests `!isNull()` on every value in the
        // dictionary, unrecognized keys included
        // (`libqpdf/QPDF_Dictionary.cc:118-127`); `isNull` dereferences. The
        // 2026-08-03 qpdf 11.9.0 probe recorded the observable half: with
        // `/DecodeParms << /Unused 99 0 R >>` and object 99 damaged,
        // `/FlateDecode` reports `(object 99 0, offset 342): expected n n obj`
        // while `/ASCIIHexDecode` says nothing about 99.
        //
        // Replacing `try_get_keys` with raw dictionary enumeration leaves this
        // unretained value unresolved and reddens this test.
        let (value, _resolver, calls) = logged_resolver_bearing_handle(ObjectValue::Integer(4));
        let parms = ObjectHandle::dictionary(vec![(b"Unused".to_vec(), value.clone())]);

        let specs = decode_filter_specs_from_handle(
            &ObjectHandle::name(b"FlateDecode".to_vec()),
            &parms,
            None,
        )
        .unwrap();

        assert_eq!(*calls.borrow(), vec![crate::ObjectRef::new(20, 0)]);
        assert!(value.is_resolved());
        assert_eq!(specs[0].decode_params, DecodeParams::Present(Vec::new()));
    }

    #[test]
    fn handle_reader_resolves_a_decode_parms_value_for_a_filter_that_reads_them() {
        // The positive half: `SF_FlateLzwDecode::setDecodeParms` walks
        // `getKeys()`/`getKey()` (`libqpdf/SF_FlateLzwDecode.cc:29-31`) and
        // `isInteger()` dereferences, so this value *is* fetched.
        let (value, _resolver, calls) = logged_resolver_bearing_handle(ObjectValue::Integer(4));
        let parms = ObjectHandle::dictionary(vec![(b"Columns".to_vec(), value)]);

        let specs = decode_filter_specs_from_handle(
            &ObjectHandle::name(b"FlateDecode".to_vec()),
            &parms,
            None,
        )
        .unwrap();

        assert_eq!(*calls.borrow(), vec![crate::ObjectRef::new(20, 0)]);
        assert_eq!(
            specs[0].decode_params,
            DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Int(4))])
        );
    }

    #[test]
    fn each_decode_parms_array_item_follows_its_own_filter() {
        // Every other test here uses a scalar `/Filter`, where one name makes
        // `filter_reads_decode_params(name)` indistinguishable from indexing
        // `names[0]`, from a reversed zip, or from hoisting the predicate out
        // of the loop. This is the only test that pins the *per-spec*
        // threading `decode_filter_specs_from_handle`'s doc claims — and the
        // corpus cannot help, because its two-filter rows are all direct.
        //
        // Non-consuming first is deliberate: a reversed zip flips both
        // assertions rather than leaving one of them accidentally true. The
        // two values get separate resolvers so neither log can pick up the
        // other's call.
        let (ahx_value, _ahx_resolver, ahx_calls) =
            logged_resolver_bearing_handle(ObjectValue::Integer(4));
        let (flate_value, _flate_resolver, flate_calls) =
            logged_resolver_bearing_handle(ObjectValue::Integer(4));

        let specs = decode_filter_specs_from_handle(
            &ObjectHandle::array(vec![
                ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ]),
            &ObjectHandle::array(vec![
                ObjectHandle::dictionary(vec![(b"Columns".to_vec(), ahx_value)]),
                ObjectHandle::dictionary(vec![(b"Columns".to_vec(), flate_value)]),
            ]),
            None,
        )
        .unwrap();

        assert_eq!(*ahx_calls.borrow(), Vec::new());
        assert_eq!(*flate_calls.borrow(), vec![crate::ObjectRef::new(20, 0)]);
        assert_eq!(
            specs[0].decode_params,
            DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Other)])
        );
        assert_eq!(
            specs[1].decode_params,
            DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Int(4))])
        );
    }

    // ----- What the shared scalar snapshot must keep -----

    #[test]
    fn handle_reader_lets_a_later_stage_see_a_value_an_earlier_stage_resolved() {
        // `replicated_decode_params` takes *one* dictionary snapshot for the
        // whole chain. This pins that the snapshot's children are the live
        // handles rather than frozen values: the `/FlateDecode` stage resolves
        // the shared `/Columns` object, and the `/ASCIIHexDecode` stage *after*
        // it sees `Int(4)` off `param_value_without_resolving`'s non-resolving
        // accessors while the one *before* it saw `Other`. That is exactly what
        // held when every stage re-snapshotted the dictionary, because
        // `ObjectHandle::clone` shares the indirect slot
        // (`try_get_key_resolves_the_same_indirect_slot_once`, `object_handle`).
        //
        // It is also what rules out the other shape this could have taken —
        // converting once per distinct consuming/non-consuming group and
        // cloning the result
        // between stages. That would hand `specs[2]` the `Other` computed for
        // `specs[0]`, changing the output for this input. Which of `Other` and
        // `Int(4)` a non-consuming stage gets is inert downstream: a filter
        // reaches the non-resolving branch exactly when its `set_decode_params`
        // is `StreamFilter`'s default body, which reads nothing but
        // `DecodeParams::is_absent()`, and an unregistered name has no filter
        // to read the set at all; qpdf's base `setDecodeParms` likewise never
        // looks at a value. So this asserts the values to pin *snapshot
        // liveness*, not to make either classification a contract — see
        // `param_value_without_resolving`.
        let (value, _resolver, calls) = logged_resolver_bearing_handle(ObjectValue::Integer(4));
        let parms = ObjectHandle::dictionary(vec![(b"Columns".to_vec(), value)]);

        let specs = decode_filter_specs_from_handle(
            &ObjectHandle::array(vec![
                ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
                ObjectHandle::name(b"FlateDecode".to_vec()),
                ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
            ]),
            &parms,
            None,
        )
        .unwrap();

        // One object, fetched once — `try_dereference` short-circuits on an
        // already-resolved slot, so the log length is a property of the
        // resolver, not of how many times the chain walked the dictionary.
        assert_eq!(*calls.borrow(), vec![crate::ObjectRef::new(20, 0)]);
        assert_eq!(
            specs[0].decode_params,
            DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Other)])
        );
        assert_eq!(
            specs[1].decode_params,
            DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Int(4))])
        );
        assert_eq!(
            specs[2].decode_params,
            DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Int(4))])
        );
    }

    #[test]
    fn handle_reader_reads_an_indirect_scalar_decode_parms_resolving_to_null_as_absent() {
        // `decode_filter_specs_from_handle`'s `decode_params.try_is_null()?` is
        // the *only* null test a replicated scalar now gets:
        // `replicated_decode_params` does not repeat it, because it is reached
        // only past that call answering `false`. Swapping that one call for the
        // non-resolving `is_null()` sends an unresolved indirect handle down
        // the replicated arm, where `try_as_dictionary` reports a resolved null
        // as "not a dictionary" and every stage comes back
        // `Present(Vec::new())` instead of `Absent` — a difference
        // `set_decode_params` reads directly.
        //
        // Two filters, so the assertion covers the replication rather than a
        // single spec.
        let (parms, _resolver) = resolver_bearing_handle(ObjectValue::Null);

        let specs = decode_filter_specs_from_handle(
            &ObjectHandle::array(vec![
                ObjectHandle::name(b"FlateDecode".to_vec()),
                ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
            ]),
            &parms,
            None,
        )
        .unwrap();

        assert_eq!(specs[0].decode_params, DecodeParams::Absent);
        assert_eq!(specs[1].decode_params, DecodeParams::Absent);
    }

    #[test]
    fn handle_reader_resolves_a_crypt_decode_parms_value() {
        // `Crypt` is the one consuming name that is not a `StreamFilter`:
        // `filters::prepare_decode_filters` routes it to
        // `PreparedStage::Crypt`, whose provider is handed
        // `&stage.spec.decode_params` and, under plan decision D2 of
        // `flpdf-25kg.3.4`, reads `/Name` to select its crypt filter. Without
        // its own arm in `filter_reads_decode_params` it would fall through to
        // the unregistered-name `false` and starve that provider.
        //
        // This test is what makes that arm load-bearing: the corpus's "Crypt
        // filter" row is direct, so it stays green either way.
        let (value, _resolver, calls) =
            logged_resolver_bearing_handle(ObjectValue::Name(b"Identity".to_vec()));
        let parms = ObjectHandle::dictionary(vec![(b"Name".to_vec(), value)]);

        let specs =
            decode_filter_specs_from_handle(&ObjectHandle::name(b"Crypt".to_vec()), &parms, None)
                .unwrap();

        assert_eq!(*calls.borrow(), vec![crate::ObjectRef::new(20, 0)]);
        assert_eq!(
            specs[0].decode_params,
            DecodeParams::Present(vec![(
                b"Name".to_vec(),
                ParamValue::Name(b"Identity".to_vec())
            )])
        );
    }

    #[test]
    fn handle_reader_expands_an_abbreviation_before_deciding_whether_to_resolve() {
        // qpdf rewrites `/Fl` to `/FlateDecode` at `QPDF_Stream.cc:419-423`,
        // *ahead* of the `filter_factories` lookup at `:425`, so an
        // abbreviated Flate filter reads its parameters like the spelled-out
        // one. The corpus cannot catch a missing `normalize_filter_name` here:
        // its "abbreviation Fl" row carries no `/DecodeParms` at all.
        let (value, _resolver, calls) = logged_resolver_bearing_handle(ObjectValue::Integer(4));
        let parms = ObjectHandle::dictionary(vec![(b"Columns".to_vec(), value)]);

        let specs =
            decode_filter_specs_from_handle(&ObjectHandle::name(b"Fl".to_vec()), &parms, None)
                .unwrap();

        assert_eq!(*calls.borrow(), vec![crate::ObjectRef::new(20, 0)]);
        assert_eq!(
            specs[0].decode_params,
            DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Int(4))])
        );
    }

    #[test]
    fn a_non_resolving_read_classifies_direct_values_exactly_as_the_object_reader_does() {
        // The half of the change that must *not* be observable: skipping
        // resolution may not change what a direct value becomes.
        // `try_dereference` short-circuits on a direct handle, so the two
        // reads are the same walk — this pins that they stay the same walk,
        // across all three `ParamValue` variants, for a non-consuming filter.
        //
        // Every key is one `RETAINED_DECODE_PARAM_KEYS` keeps, so retention
        // cannot hide a classification difference by dropping the entry that
        // would have shown it. `ParamValue::Name` is not among the variants
        // here and cannot be: it carries a payload only under `/Crypt`'s
        // `/Name` (`is_crypt_name_key`), and neither filter below is `/Crypt`.
        // `handle_reader_resolves_a_crypt_decode_parms_value` and the corpus's
        // "Crypt filter" row are where the two readers are held to the same
        // answer for that variant.
        //
        // `/Colors` therefore carries a *name* and `/Predictor` a string, both
        // expected to be `Other`: that pins the payload rule as the two
        // readers' shared behavior rather than one reader's, which a single
        // non-name `Other` would not.
        let entries = || {
            vec![
                (b"Colors".to_vec(), ObjectHandle::name(b"Identity".to_vec())),
                (b"Columns".to_vec(), ObjectHandle::integer(7)),
                (b"Predictor".to_vec(), ObjectHandle::string(b"x".to_vec())),
            ]
        };
        let expected = DecodeParams::Present(vec![
            (b"Colors".to_vec(), ParamValue::Other),
            (b"Columns".to_vec(), ParamValue::Int(7)),
            (b"Predictor".to_vec(), ParamValue::Other),
        ]);

        // Spelled out rather than looped so a failure names the filter by the
        // line it is on, without a message argument that only runs on failure.
        let read = |filter: &[u8]| {
            decode_filter_specs_from_handle(
                &ObjectHandle::name(filter.to_vec()),
                &ObjectHandle::dictionary(entries()),
                None,
            )
            .unwrap()
            .swap_remove(0)
            .decode_params
        };

        assert_eq!(read(b"ASCIIHexDecode"), expected);
        assert_eq!(read(b"FlateDecode"), expected);
    }

    #[test]
    fn handle_reader_leaves_a_dropped_document_unread_for_a_filter_that_ignores_decode_parms() {
        // The counterpart to the "/DecodeParms dictionary value" row of
        // `handle_reader_surfaces_a_dropped_document_from_every_child_position`.
        // A severed handle at that position is *not* an error under a
        // non-consuming filter, and that is not flpdf being lax: qpdf's
        // ASCIIHex filter never fetches the object, so a broken reference
        // there cannot be diagnosed by qpdf either. The stream is still
        // refused downstream — `set_decode_params` sees a `Present` parameter
        // set — just not for the reference's sake.
        let dropped = {
            let (handle, resolver) = resolver_bearing_handle(ObjectValue::Integer(4));
            drop(resolver);
            handle
        };

        let specs = decode_filter_specs_from_handle(
            &ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
            &ObjectHandle::dictionary(vec![(b"Columns".to_vec(), dropped)]),
            None,
        )
        .expect("qpdf never dereferences this value, so neither does flpdf");

        assert_eq!(
            specs[0].decode_params,
            DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Other)])
        );
    }

    #[test]
    fn handle_reader_propagates_get_keys_errors_before_retention() {
        let dropped = {
            let (handle, resolver) = resolver_bearing_handle(ObjectValue::Integer(4));
            drop(resolver);
            handle
        };
        let parms = ObjectHandle::dictionary(vec![(b"Unused".to_vec(), dropped)]);

        let error = decode_filter_specs_from_handle(
            &ObjectHandle::name(b"FlateDecode".to_vec()),
            &parms,
            None,
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "object 20 0 belongs to a dropped PDF");
    }

    #[test]
    fn handle_reader_surfaces_a_dropped_document_from_every_child_position() {
        // `try_dereference`'s `Error::Internal` has to reach the caller from
        // each position this reader dereferences; silently reading the severed
        // handle as "absent" or "not a name" would hide a broken document
        // behind a plausible answer.
        //
        // Four of the five positions are unconditional. The fifth — a
        // `/DecodeParms` dictionary value — is conditional on the filter, so
        // it is listed with `flate()` here and its non-consuming counterpart
        // is `handle_reader_leaves_a_dropped_document_unread_for_a_filter_\
        // that_ignores_decode_parms`, which asserts `Ok` precisely because
        // qpdf never fetches that object either.
        let dropped = || {
            let (handle, resolver) =
                resolver_bearing_handle(ObjectValue::Name(b"FlateDecode".to_vec()));
            drop(resolver);
            handle
        };
        let flate = || ObjectHandle::name(b"FlateDecode".to_vec());
        let cases: Vec<(&str, ObjectHandle, ObjectHandle)> = vec![
            ("/Filter itself", dropped(), ObjectHandle::null()),
            (
                "a /Filter array item",
                ObjectHandle::array(vec![dropped()]),
                ObjectHandle::null(),
            ),
            ("/DecodeParms itself", flate(), dropped()),
            (
                "a /DecodeParms array item",
                flate(),
                ObjectHandle::array(vec![dropped()]),
            ),
            (
                "a /DecodeParms dictionary value",
                flate(),
                ObjectHandle::dictionary(vec![(b"Columns".to_vec(), dropped())]),
            ),
        ];

        for (label, filter, parms) in cases {
            let error = decode_filter_specs_from_handle(&filter, &parms, None)
                .expect_err(&format!("{label} must not read as absent"));

            assert_eq!(
                error.to_string(),
                "object 20 0 belongs to a dropped PDF",
                "{label}"
            );
        }
    }

    /// A document whose object 2 is the bare name `/FlateDecode`, so a
    /// registry handle for `2 0 R` can stand in for an indirect `/Filter`.
    fn pdf_with_a_filter_name_object() -> Vec<u8> {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let catalog = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let filter = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n/FlateDecode\nendobj\n");
        let xref_start = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n0 3\n0000000000 65535 f \n{catalog:010} 00000 n \n{filter:010} 00000 n \n\
                 trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn handle_reader_rejects_a_filter_disconnected_by_pdf_teardown() {
        // Production teardown, not a synthetic resolver: `Pdf::drop`
        // (`reader.rs`, mirroring `QPDF::~QPDF`, `libqpdf/QPDF.cc:215-236`)
        // calls `ObjectHandle::disconnect` on every registry entry, leaving
        // the slot `Destroyed`. `try_dereference` treats every state other
        // than `NotYetResolved` as terminal, so a surviving handle does *not*
        // raise `Error::Internal("... belongs to a dropped PDF")` — that error
        // belongs to the other path, a still-`NotYetResolved` handle whose
        // resolver `Weak` has expired.
        //
        // qpdf's `isNull()` accepts only `ot_null`
        // (`libqpdf/QPDFObjectHandle.cc:352-356`), so an `ot_destroyed`
        // `/Filter` falls through `isName()`/`isArray()` and is rejected by
        // `QPDF_Stream::filterable` (`libqpdf/QPDF_Stream.cc:391-413`).
        let mut pdf = Pdf::open(Cursor::new(pdf_with_a_filter_name_object())).expect("open");
        let filter = pdf.get_object_handle(ObjectRef::new(2, 0));
        pdf.resolve_object_handle(&filter).expect("resolve /Filter");

        // Control, so the assertion after the drop cannot pass vacuously for a
        // handle that never carried a value in the first place.
        let live = decode_filter_specs_from_handle(&filter, &ObjectHandle::null(), None)
            .expect("a resolved /Filter name reads normally");
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].name, b"FlateDecode".to_vec());

        drop(pdf);

        assert!(filter.is_resolved(), "disconnect leaves a terminal state");
        assert_eq!(filter.type_code(), 14, "qpdf ot_destroyed");
        assert!(!filter.is_null());
        let error = decode_filter_specs_from_handle(&filter, &ObjectHandle::null(), None)
            .expect_err("a destroyed /Filter is not absent");
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream filter type is not name or array"
        );
    }

    #[test]
    fn output_buffer_has_qpdf_pipeline_identifier() {
        assert_eq!(OutputBuffer::new(None).identifier(), "stream data buffer");
    }

    #[test]
    fn one_element_decode_parms_array_aligns_with_name_filter() {
        let filter = Object::Name(b"FlateDecode".to_vec());
        let decode_parms = Object::Array(vec![params(&[("Columns", Object::Integer(7))])]);

        let specs =
            decode_filter_specs_from_object(Some(&filter), Some(&decode_parms), None).unwrap();

        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs[0].decode_params,
            DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Int(7))])
        );
    }

    #[test]
    fn qpdf_filter_abbreviations_expand_without_changing_full_names() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"Fl", b"FlateDecode"),
            (b"LZW", b"LZWDecode"),
            (b"A85", b"ASCII85Decode"),
            (b"AHx", b"ASCIIHexDecode"),
            (b"RL", b"RunLengthDecode"),
            (b"CCF", b"CCITTFaxDecode"),
            (b"DCT", b"DCTDecode"),
            (b"FlateDecode", b"FlateDecode"),
        ];

        for &(abbreviation, expected) in cases {
            let filter = Object::Name(abbreviation.to_vec());
            let specs = decode_filter_specs_from_object(Some(&filter), None, None).unwrap();
            assert_eq!(specs[0].normalized_name(), expected);
        }
    }

    #[test]
    fn flate_decode_is_invariant_across_input_chunks() {
        let encoded = encode_flate(b"chunk boundaries must not matter").unwrap();
        let whole = decode_flate_chunks([encoded.as_slice()], None, &mut |_, _| Ok(())).unwrap();
        let split = decode_flate_chunks(encoded.chunks(1), None, &mut |_, _| Ok(())).unwrap();

        assert_eq!(whole, b"chunk boundaries must not matter");
        assert_eq!(split, whole);
    }

    #[test]
    fn flate_limit_rejects_one_byte_over_but_accepts_exact_boundary() {
        let encoded = encode_flate(&vec![b'A'; 2_000]).unwrap();

        let error = decode_flate(&encoded, Some(1_999)).unwrap_err();

        assert!(matches!(error, Error::Unsupported(_)));
        assert_eq!(
            error.to_string(),
            format!(
                "unsupported PDF feature: {DECODE_OUTPUT_LIMIT_PREFIX} {} bytes",
                1_999
            )
        );
        assert_eq!(decode_flate(&encoded, Some(2_000)).unwrap().len(), 2_000);
    }

    #[test]
    fn incomplete_input_reports_qpdf_warning_before_downstream_finish() {
        let warnings = RefCell::new(Vec::new());

        let decoded = decode_flate_chunks([b"\x78".as_slice()], None, &mut |message, code| {
            warnings.borrow_mut().push((message.to_string(), code));
            Ok(())
        })
        .unwrap();

        assert!(decoded.is_empty());
        assert_eq!(
            warnings.into_inner(),
            vec![(
                "input stream is complete but output may still be valid".to_string(),
                -5,
            )]
        );
        assert!(
            decode_flate_chunks([b"\x78".as_slice()], None, &mut ignore_codec_warning)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn empty_inflate_skips_codec_and_warning_like_qpdf() {
        let decoded =
            decode_flate_chunks(std::iter::empty(), None, &mut ignore_codec_warning).unwrap();

        assert!(decoded.is_empty());
    }

    #[test]
    fn malformed_flate_header_retains_qpdf_pipeline_identifier_and_timing() {
        let error = decode_flate_chunks(
            [b"\x78\x00".as_slice(), b"not reached".as_slice()],
            None,
            &mut ignore_codec_warning,
        )
        .unwrap_err();

        assert!(matches!(error, Error::Unsupported(_)));
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream inflate: inflate: data: incorrect header check"
        );
    }

    #[test]
    fn empty_encode_skips_codec_and_emits_no_wrapper_like_qpdf() {
        assert!(encode_flate(b"").unwrap().is_empty());
    }

    #[test]
    fn flate_factory_exposes_qpdf_stream_filter_contract() {
        let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");

        assert!(filter.set_decode_params(&DecodeParams::Absent));
        assert!(!filter.is_specialized_compression());
        assert!(!filter.is_lossy_compression());

        let encoded = encode_flate(b"factory pipeline").unwrap();
        let decoded = filter
            .pipe_decode(&encoded, None, &mut |_, _, _, _| Ok(()))
            .unwrap();
        assert_eq!(decoded, b"factory pipeline");
    }

    /// Crosses the shape seam deliberately: a present non-dictionary is only
    /// visible as an `Object`, and the property under test is that reading it
    /// leaves the filter filterable the way `getKeys`' empty key set does
    /// (`QPDFObjectHandle.cc:997-1009`), touching none of the five scalar
    /// parameters `SF_FlateLzwDecode` keeps.
    #[test]
    fn flate_factory_treats_non_dictionary_decode_params_as_empty_like_qpdf() {
        let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");
        assert!(filter.set_decode_params(&flate_decode_params(Some(&Object::Integer(1)))));

        // Assert on the concrete type as well, because the boxed
        // `dyn StreamFilter` above can report only the `bool`, not which
        // parameters survived.
        let params = Object::String(b"not a dictionary".to_vec());
        let neutral = flate_decode_params(Some(&params));
        // Pin the reduction itself, not only its effect: "empty like qpdf"
        // means `Present` with no entries. Reducing a non-dictionary to
        // `Absent` instead would take `SF_FlateLzwDecode`'s `isNull()` early
        // return (`SF_FlateLzwDecode.cc:24-26`), which qpdf reaches only for a
        // real null — yet a freshly constructed adapter answers `true` and
        // keeps its defaults either way, so the assertions below cannot tell
        // the two apart on their own.
        assert!(matches!(neutral, DecodeParams::Present(_)));

        let mut flate = FlateLzwStreamFilter::new(false);
        assert!(flate.set_decode_params(&neutral));
        assert_eq!(
            (
                flate.predictor,
                flate.columns,
                flate.colors,
                flate.bits_per_component,
                flate.early_code_change,
            ),
            (1, 1, 1, 8, true),
            "a non-dictionary parameter object leaves every default in place"
        );
    }

    #[test]
    fn factory_returns_all_production_stream_filters() {
        for name in [
            b"FlateDecode".as_slice(),
            b"LZWDecode",
            b"ASCII85Decode",
            b"ASCIIHexDecode",
            b"RunLengthDecode",
        ] {
            assert!(stream_filter_for(name).is_some(), "{name:?}");
        }
    }

    fn params(entries: &[(&str, Object)]) -> Object {
        let mut dictionary = Dictionary::new();
        for (key, value) in entries {
            dictionary.insert(*key, value.clone());
        }
        Object::Dictionary(dictionary)
    }

    /// [`decode_params_from_object`] under `/FlateDecode`, for the filter tests
    /// below that feed a real dictionary to a `FlateLzwStreamFilter`.
    ///
    /// The filter name reaching that reader decides one thing only —
    /// whether `/Name` is retained ([`CRYPT_RETAINED_DECODE_PARAM_KEY`]) — and
    /// no dictionary passed through here carries a `/Name`, so every call is
    /// unaffected by which non-`Crypt` name is used. Tests that are *about*
    /// that decision call `decode_filter_specs_from_object` and name their
    /// filter explicitly.
    fn flate_decode_params(params: Option<&Object>) -> DecodeParams {
        decode_params_from_object(params, b"FlateDecode")
    }

    /// Build the `/DecodeParms` shape `StreamFilter::set_decode_params` reads.
    ///
    /// The filters no longer see an `Object`, so a filter test states the
    /// bounded values directly; the `Object` -> `ParamValue` reduction is
    /// `param_value_from_object`'s contract and is pinned by
    /// `object_shape_reader_reduces_each_parameter_value_to_its_bounded_shape`.
    fn neutral_params(entries: &[(&str, ParamValue)]) -> DecodeParams {
        DecodeParams::Present(
            entries
                .iter()
                .map(|(key, value)| (key.as_bytes().to_vec(), value.clone()))
                .collect(),
        )
    }

    fn accepts(lzw: bool, entries: &[(&str, ParamValue)]) -> bool {
        FlateLzwStreamFilter::new(lzw).set_decode_params(&neutral_params(entries))
    }

    #[test]
    fn flate_lzw_filter_accepts_absent_and_null_decode_params() {
        let mut filter = FlateLzwStreamFilter::new(true);
        // A missing key and an explicit null both reduce to `Absent`.
        assert!(filter.set_decode_params(&flate_decode_params(None)));
        assert!(filter.set_decode_params(&flate_decode_params(Some(&Object::Null))));
        assert!(!filter.is_specialized_compression());
        assert!(!filter.is_lossy_compression());
    }

    #[test]
    fn flate_filter_reads_predictor_geometry_from_neutral_params() {
        let mut filter = FlateLzwStreamFilter::new(false);
        let params = DecodeParams::Present(vec![
            (b"Predictor".to_vec(), ParamValue::Int(12)),
            (b"Columns".to_vec(), ParamValue::Int(4)),
        ]);
        assert!(filter.set_decode_params(&params));
        assert_eq!(filter.predictor, 12);
        assert_eq!(filter.columns, 4);
    }

    #[test]
    fn a_fresh_flate_filter_accepts_both_present_shapes_the_neutral_form_merges() {
        // The neutral form collapses "present non-dictionary" and "present empty
        // dictionary" into `Present(vec![])`, removing flpdf's own early
        // `return true` for a non-dictionary. That shortcut was never qpdf's:
        // `SF_FlateLzwDecode::setDecodeParms` (`libqpdf/SF_FlateLzwDecode.cc:21-73`)
        // early-returns only for `isNull()` (`:24-26`); a present non-dictionary
        // reaches `getKeys()`, which warns `typeWarning("dictionary", "treating as
        // empty")` (`libqpdf/QPDFObjectHandle.cc:997-1009`, warning at `:1005`),
        // yields an empty set, and falls through to the trailing
        // `(predictor > 1) && (columns == 0)` check at `:68-70`. So this merge is a
        // CONVERGENCE toward qpdf, not a tolerated loss.
        //
        // Both shapes still answer `true` because this test — like every
        // production caller — applies params to a freshly constructed adapter
        // (defaults `predictor = 1, columns = 1`), making that trailing check
        // false either way. That freshness is an assumption of *this* test, not
        // something it can detect the loss of;
        // `a_dirtied_adapter_shows_why_absent_short_circuits_and_present_does_not`
        // is what pins the behavior once an adapter carries prior geometry.
        assert!(
            FlateLzwStreamFilter::new(false).set_decode_params(&DecodeParams::Present(Vec::new()))
        );
        assert!(
            FlateLzwStreamFilter::new(true).set_decode_params(&DecodeParams::Present(Vec::new()))
        );
    }

    #[test]
    fn a_dirtied_adapter_shows_why_absent_short_circuits_and_present_does_not() {
        let mut filter = FlateLzwStreamFilter::new(false);
        // Setup, not the property under test: this first call exists only to
        // leave predictor = 12 / columns = 0 behind for the two calls below.
        // The predictor/columns rule it returns `false` on is pinned on its own
        // by `a_predictor_above_one_requires_a_nonzero_columns_value`, so this
        // is not a duplicate of that test.
        //
        // `/Predictor` is stored before its own range check
        // (`SF_FlateLzwDecode.cc:34` then `:35-38`) and `/Columns` is stored with
        // no range validation at all (`:46`), so predictor = 12 and columns = 0
        // both stick even though the trailing `(predictor > 1) && (columns == 0)`
        // check at `:68-70` answers `filterable = false`.
        assert!(!filter.set_decode_params(&neutral_params(&[
            ("Predictor", ParamValue::Int(12)),
            ("Columns", ParamValue::Int(0)),
        ])));
        assert_eq!((filter.predictor, filter.columns), (12, 0));
        // `SF_FlateLzwDecode.cc:24-26` returns true for `isNull()` before reading
        // any key, regardless of prior state. Removing flpdf's matching
        // `is_absent()` early return fails only this line.
        assert!(filter.set_decode_params(&DecodeParams::Absent));
        // Present-but-empty does not short-circuit: it reaches `:68-70` still
        // carrying the geometry the first call left behind.
        assert!(!filter.set_decode_params(&DecodeParams::Present(Vec::new())));
    }

    #[test]
    fn predictor_values_outside_the_supported_set_are_not_filterable() {
        for predictor in [1, 2, 10, 11, 12, 13, 14, 15] {
            assert!(
                accepts(false, &[("Predictor", ParamValue::Int(predictor))]),
                "predictor {predictor}"
            );
        }
        for predictor in [-1, 0, 3, 9, 16, 100] {
            assert!(
                !accepts(false, &[("Predictor", ParamValue::Int(predictor))]),
                "predictor {predictor}"
            );
        }
        assert!(!accepts(
            false,
            &[("Predictor", ParamValue::Name(b"12".to_vec()))]
        ));
    }

    #[test]
    fn a_predictor_above_one_requires_a_nonzero_columns_value() {
        assert!(!accepts(
            false,
            &[
                ("Predictor", ParamValue::Int(12)),
                ("Columns", ParamValue::Int(0)),
            ]
        ));
        assert!(accepts(
            false,
            &[
                ("Predictor", ParamValue::Int(1)),
                ("Columns", ParamValue::Int(0)),
            ]
        ));
        assert!(accepts(
            false,
            &[
                ("Predictor", ParamValue::Int(12)),
                ("Columns", ParamValue::Int(4)),
            ]
        ));
    }

    #[test]
    fn geometry_parameters_are_retained_without_range_validation() {
        let mut filter = FlateLzwStreamFilter::new(false);
        assert!(filter.set_decode_params(&neutral_params(&[
            ("Predictor", ParamValue::Int(12)),
            ("Columns", ParamValue::Int(-4)),
            ("Colors", ParamValue::Int(-1)),
            ("BitsPerComponent", ParamValue::Int(99)),
        ])));
        assert_eq!(
            (filter.columns, filter.colors, filter.bits_per_component),
            (-4, -1, 99)
        );
        assert!(!accepts(false, &[("Columns", ParamValue::Other)]));
        assert!(!accepts(false, &[("Colors", ParamValue::Other)]));
        assert!(!accepts(false, &[("BitsPerComponent", ParamValue::Other)]));
    }

    /// Crosses the shape seam deliberately: the clamp now happens while the
    /// `Object` is read, so only an out-of-range `Object::Integer` can show
    /// that a `/Columns` beyond `INT_MAX` still reaches the filter as
    /// `INT_MAX`.
    #[test]
    fn integer_parameters_saturate_at_the_32_bit_boundary() {
        let mut filter = FlateLzwStreamFilter::new(false);
        assert!(
            filter.set_decode_params(&flate_decode_params(Some(&params(&[
                ("Predictor", Object::Integer(12)),
                ("Columns", Object::Integer(i64::from(i32::MAX) + 10)),
                ("Colors", Object::Integer(i64::from(i32::MIN) - 10)),
            ]))))
        );
        assert_eq!((filter.columns, filter.colors), (i32::MAX, i32::MIN));
    }

    #[test]
    fn early_change_is_read_only_for_lzw_streams() {
        let mut lzw = FlateLzwStreamFilter::new(true);
        assert!(lzw.set_decode_params(&neutral_params(&[("EarlyChange", ParamValue::Int(0))])));
        assert!(!lzw.early_code_change);

        let mut lzw = FlateLzwStreamFilter::new(true);
        assert!(lzw.set_decode_params(&neutral_params(&[("EarlyChange", ParamValue::Int(1))])));
        assert!(lzw.early_code_change);

        // A value outside {0, 1} makes an LZW stream unfilterable.
        assert!(!accepts(true, &[("EarlyChange", ParamValue::Int(7))]));
        assert!(!accepts(
            true,
            &[("EarlyChange", ParamValue::Name(b"1".to_vec()))]
        ));

        // The same parameters are ignored entirely on a Flate stream.
        let mut flate = FlateLzwStreamFilter::new(false);
        assert!(flate.set_decode_params(&neutral_params(&[("EarlyChange", ParamValue::Int(7))])));
        assert!(flate.early_code_change);
    }

    /// `set_decode_params`' `_ => {}` arm, which no reader can now reach:
    /// [`RETAINED_DECODE_PARAM_KEYS`] drops such a key before a `FilterSpec`
    /// is built, and this test hands the filter a `DecodeParams` directly. It
    /// stays because the arm is what makes retention *safe* to add — if an
    /// unrecognized key mattered to a filter, dropping it would not be.
    #[test]
    fn unrecognized_decode_params_keys_are_ignored() {
        assert!(accepts(
            true,
            &[("Whatever", ParamValue::Name(b"x".to_vec()))]
        ));
    }

    #[test]
    fn lzw_streams_decode_through_the_registered_filter() {
        let mut dictionary = Dictionary::new();
        dictionary.insert("Filter", Object::Name(b"LZWDecode".to_vec()));
        let mut filter = stream_filter_for(b"LZWDecode").expect("registered LZW filter");

        let decoded = filter
            .pipe_decode(&[0x80, 0x10, 0x60, 0x20], None, &mut ignore_warning)
            .expect("LZW decode");

        assert_eq!(decoded, b"A");
    }

    #[test]
    fn lzw_early_change_zero_changes_the_decoded_bytes() {
        let stream: &[u8] = &[0x80, 0x10, 0x48, 0x50, 0x28, 0x24, 0x0e, 0x0d, 0x01];
        let mut filter = stream_filter_for(b"LZWDecode").expect("registered LZW filter");
        assert!(filter.set_decode_params(&neutral_params(&[("EarlyChange", ParamValue::Int(0))])));

        let decoded = filter
            .pipe_decode(stream, None, &mut ignore_warning)
            .expect("LZW decode");

        assert_eq!(decoded, b"ABABABABABAB");
    }

    #[test]
    fn abbreviated_filter_names_reach_the_flate_and_lzw_filters() {
        for name in [b"Fl".as_slice(), b"LZW"] {
            assert!(
                stream_filter_for(normalize_filter_name(name)).is_some(),
                "{name:?}"
            );
        }
    }

    #[test]
    fn tiff_predictor_is_reported_at_pipeline_construction() {
        let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");
        assert!(filter.set_decode_params(&neutral_params(&[
            ("Predictor", ParamValue::Int(2)),
            ("Columns", ParamValue::Int(4)),
        ])));

        let error = filter
            .pipe_decode(b"", None, &mut ignore_warning)
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: /DecodeParms /Predictor 2 is not supported for this stream type"
        );
    }

    #[test]
    fn negative_geometry_is_rejected_when_the_predictor_pipeline_is_built() {
        for (key, value) in [("Columns", -4), ("Colors", -1), ("BitsPerComponent", -8)] {
            let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");
            assert!(filter.set_decode_params(&neutral_params(&[
                ("Predictor", ParamValue::Int(12)),
                ("Columns", ParamValue::Int(4)),
                (key, ParamValue::Int(value)),
            ])));

            let error = filter
                .pipe_decode(b"", None, &mut ignore_warning)
                .unwrap_err();

            assert_eq!(
                error.to_string(),
                format!(
                    "unsupported PDF feature: integer out of range converting {value} \
                     from a 4-byte signed type to a 4-byte unsigned type"
                ),
                "{key}"
            );
        }
    }

    #[test]
    fn invalid_predictor_geometry_is_reported_before_any_codec_write() {
        let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");
        assert!(filter.set_decode_params(&neutral_params(&[
            ("Predictor", ParamValue::Int(12)),
            ("Columns", ParamValue::Int(4)),
            ("BitsPerComponent", ParamValue::Int(3)),
        ])));

        // The input is not valid deflate data, so reaching the codec at all
        // would produce a different error.
        let error = filter
            .pipe_decode(b"not deflate", None, &mut ignore_warning)
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: PNGFilter created with invalid bits_per_sample \
             not 1, 2, 4, 8, or 16"
        );
    }

    #[test]
    fn predictor_decoding_runs_after_the_codec_in_one_chain() {
        let rows: &[u8] = &[2, 0x01, 0x02, 0x03, 0x04, 2, 0x01, 0x01, 0x01, 0x01];
        let encoded = encode_flate(rows).expect("flate encode");
        let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");
        assert!(filter.set_decode_params(&neutral_params(&[
            ("Predictor", ParamValue::Int(12)),
            ("Columns", ParamValue::Int(4)),
        ])));

        let decoded = filter
            .pipe_decode(&encoded, None, &mut ignore_warning)
            .expect("predicted flate decode");

        assert_eq!(
            decoded,
            vec![0x01, 0x02, 0x03, 0x04, 0x02, 0x03, 0x04, 0x05]
        );
    }

    #[test]
    fn the_output_limit_applies_to_post_predictor_bytes() {
        let rows: &[u8] = &[2, 0x01, 0x02, 0x03, 0x04, 2, 0x01, 0x01, 0x01, 0x01];
        let encoded = encode_flate(rows).expect("flate encode");
        let predicted = neutral_params(&[
            ("Predictor", ParamValue::Int(12)),
            ("Columns", ParamValue::Int(4)),
        ]);

        let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");
        assert!(filter.set_decode_params(&predicted));
        assert_eq!(
            filter
                .pipe_decode(&encoded, Some(8), &mut ignore_warning)
                .expect("eight decoded bytes fit the cap")
                .len(),
            8
        );

        let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");
        assert!(filter.set_decode_params(&predicted));
        let error = filter
            .pipe_decode(&encoded, Some(7), &mut ignore_warning)
            .unwrap_err();
        assert!(
            error.to_string().contains(DECODE_OUTPUT_LIMIT_PREFIX),
            "{error}"
        );
    }

    #[test]
    fn lzw_decoding_honors_the_output_limit() {
        let mut filter = stream_filter_for(b"LZWDecode").expect("registered LZW filter");
        let error = filter
            .pipe_decode(&[0x80, 0x10, 0x60, 0x20], Some(0), &mut ignore_warning)
            .unwrap_err();

        assert!(
            error.to_string().contains(DECODE_OUTPUT_LIMIT_PREFIX),
            "{error}"
        );
    }

    #[test]
    fn flate_and_lzw_predictor_finish_output_keeps_its_cleanup_boundary() {
        let predictor = neutral_params(&[
            ("Predictor", ParamValue::Int(12)),
            ("Columns", ParamValue::Int(2)),
        ]);

        let mut flate = stream_filter_for(b"FlateDecode").expect("registered Flate filter");
        assert!(flate.set_decode_params(&predictor));
        let mut flate_payload = encode_flate(&[0, b'A']).expect("encode predicted bytes");
        flate_payload.truncate(flate_payload.len() - 4);
        let mut warnings = Vec::new();
        let flate_outcome = flate
            .pipe_decode_recovering(&flate_payload, Some(1), &mut |message, code, _, _| {
                warnings.push((message.to_string(), code));
                Ok(())
            })
            .expect("constructed Flate pipeline");
        assert_eq!(flate_outcome.data, b"A");
        assert_eq!(flate_outcome.cleanup_data_start, 0);
        assert!(
            !flate_outcome
                .error
                .expect("finish output hits limit")
                .during_write
        );
        assert_eq!(
            warnings,
            [(
                "input stream is complete but output may still be valid".to_string(),
                -5
            )]
        );

        let mut lzw = stream_filter_for(b"LZWDecode").expect("registered LZW filter");
        assert!(lzw.set_decode_params(&predictor));
        let lzw_outcome = lzw
            .pipe_decode_recovering(
                &pack_codes(&[256, 0, u32::from(b'A'), 257], true),
                Some(1),
                &mut ignore_warning,
            )
            .expect("constructed LZW pipeline");
        assert_eq!(lzw_outcome.data, b"A");
        assert_eq!(lzw_outcome.cleanup_data_start, 0);
        assert!(
            !lzw_outcome
                .error
                .expect("finish output hits limit")
                .during_write
        );
    }

    #[test]
    fn codec_warning_phase_distinguishes_write_from_finish() {
        let phase = Cell::new(false);
        assert_eq!(
            super::filter_decode_phase(None),
            super::FilterDecodePhase::Write
        );
        assert_eq!(
            super::filter_decode_phase(Some(&phase)),
            super::FilterDecodePhase::Write
        );
        phase.set(true);
        assert_eq!(
            super::filter_decode_phase(Some(&phase)),
            super::FilterDecodePhase::Finish
        );
    }

    #[test]
    fn ascii_and_run_length_factories_expose_qpdf_stream_filter_contract() {
        for (name, specialized) in [
            (b"ASCII85Decode".as_slice(), false),
            (b"ASCIIHexDecode".as_slice(), false),
            (b"RunLengthDecode".as_slice(), true),
        ] {
            let mut filter = stream_filter_for(name).expect("registered stream filter");

            assert!(filter.set_decode_params(&DecodeParams::Absent), "{name:?}");
            assert!(
                !filter.set_decode_params(&DecodeParams::Present(Vec::new())),
                "{name:?}"
            );
            assert_eq!(filter.is_specialized_compression(), specialized, "{name:?}");
            assert!(!filter.is_lossy_compression(), "{name:?}");
        }
    }

    #[test]
    fn ascii_and_run_length_factories_decode_through_pipelines() {
        let cases: &[(&[u8], &[u8], &[u8])] = &[
            (b"ASCII85Decode", b"z~>", &[0, 0, 0, 0]),
            (b"ASCIIHexDecode", b"4142>", b"AB"),
            (b"RunLengthDecode", &[0xff, b'A', 0x80], b"AA"),
        ];

        for &(name, encoded, expected) in cases {
            let decoded = stream_filter_for(name)
                .expect("registered stream filter")
                .pipe_decode(encoded, None, &mut ignore_warning)
                .unwrap();

            assert_eq!(decoded, expected, "{name:?}");
        }
    }

    #[test]
    fn ascii_and_run_length_factories_enforce_output_limit_boundaries() {
        let cases: &[(&[u8], &[u8], &[u8])] = &[
            (b"ASCII85Decode", b"z~>", &[0, 0, 0, 0]),
            (b"ASCIIHexDecode", b"4142>", b"AB"),
            (b"RunLengthDecode", &[0xff, b'A', 0x80], b"AA"),
        ];

        for &(name, encoded, expected) in cases {
            let below = expected.len() - 1;
            let error = stream_filter_for(name)
                .expect("registered stream filter")
                .pipe_decode(encoded, Some(below), &mut ignore_warning)
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("unsupported PDF feature: {DECODE_OUTPUT_LIMIT_PREFIX} {below} bytes"),
                "{name:?}"
            );

            let decoded = stream_filter_for(name)
                .expect("registered stream filter")
                .pipe_decode(encoded, Some(expected.len()), &mut ignore_warning)
                .unwrap();
            assert_eq!(decoded, expected, "{name:?}");
        }
    }

    #[test]
    fn ascii_hex_decode_pipeline_decodes_through_the_caller_sink() {
        let mut sink = OutputBuffer::new(None);
        {
            let mut filter = AsciiHexStreamFilter;
            let mut stage = filter.decode_pipeline(&mut sink).unwrap().unwrap();
            stage.write(b"616263>").unwrap();
            stage.finish().unwrap();
        }
        assert_eq!(sink.data, b"abc");
    }

    #[test]
    fn default_stream_filter_contract_accepts_only_null_params() {
        let mut filter = stream_filter_for(b"TestRejectDecode").expect("test filter");

        // `Absent` is how a missing key and an explicit null both arrive.
        assert!(filter.set_decode_params(&DecodeParams::Absent));
        assert!(!filter.set_decode_params(&DecodeParams::Present(Vec::new())));
        assert_eq!(
            filter
                .pipe_decode(b"test filter", None, &mut ignore_warning)
                .unwrap(),
            b"test filter"
        );
    }
}

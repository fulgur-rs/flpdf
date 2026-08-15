//! qpdf correspondence: QPDFStreamFilter.cc and QPDF_Stream.cc filter-name, DecodeParms-alignment, and decode-pipeline construction responsibilities, read from either Object-shaped or ObjectHandle-shaped /Filter and /DecodeParms values.
//!
//! # `SF_Crypt::setDecodeParms` validation is reproduced but unreached
//!
//! qpdf refuses a `Crypt` stage whose `/DecodeParms` carry any key other than
//! `/Type` or `/Name`, and validates a present `/Type` through
//! `isDictionaryOfType("/CryptFilterDecodeParms")` — the whole body of
//! `SF_Crypt::setDecodeParms` (`libqpdf/QPDF_Stream.cc:33-50`). It returns
//! `false` on anything else, which `QPDF_Stream::filterable` turns into its
//! own `filterable = false` at `:471`/`:479-481`.
//!
//! [`CryptStreamFilter`] reproduces that walk key for key, and
//! [`stream_filter_for`] registers it under `Crypt` the way qpdf's
//! `filter_factories` does (`QPDF_Stream.cc:85-94`). **No production decode
//! reaches it.** `filters::prepare_decode_filters` routes a `Crypt` spec to
//! `PreparedStage::Crypt` before it consults [`stream_filter_for`], so the
//! installed crypt provider decides the outcome instead; for every
//! non-decrypting entry point that provider is `filters::reject_crypt_stage`,
//! which errors unconditionally, so no shape of `/DecodeParms` is
//! distinguishable there. The filter's own answers are therefore pinned by
//! unit tests rather than by an end-to-end decode.
//!
//! The check's input is intact: a `Crypt` stage's [`DecodeParams`] carries the
//! whole key set it reads, with `/Type`'s name bytes preserved — see
//! [`retains_decode_param_key`].
//!
//! # Recorded deviation: `DecodeParams` is an owned, reduced snapshot
//!
//! qpdf replicates one `QPDFObjectHandle` — a `shared_ptr` — across the filter
//! chain and copies no dictionary. [`DecodeParams`] owns its entries instead,
//! so it retains only what some consumer reads
//! ([`retains_decode_param_key`]): [`RETAINED_DECODE_PARAM_KEYS`] under every
//! filter, and **every key under `Crypt`**, whose `setDecodeParms` reads the
//! whole key set. A name's bytes survive only under the two keys some consumer
//! compares them against ([`CRYPT_NAME_PAYLOAD_DECODE_PARAM_KEYS`]); elsewhere
//! a name reduces to [`ParamValue::Other`], which no consumer can tell apart.
//!
//! Output bytes and error timing are unaffected: nothing reconstructs an
//! emitted `/DecodeParms` from this type — the writer copies the source
//! dictionary. **Filterability is decided from this snapshot**, so the
//! retained set has to be exactly what each `setDecodeParms` reads.
//! `SF_FlateLzwDecode`'s key walk (`libqpdf/SF_FlateLzwDecode.cc:32-66`) has
//! no `else` arm, so a key it does not name never reaches its `filterable` and
//! the five geometry keys suffice there. `SF_Crypt::setDecodeParms`
//! (`libqpdf/QPDF_Stream.cc:33-50`) has an `else` arm that refuses on any
//! other key, which is why a `Crypt` stage keeps all of them.

use crate::object_handle::{legacy_dictionary_key, ObjectHandle};
use crate::pipeline::ascii85::Ascii85Decoder;
use crate::pipeline::ascii_hex::AsciiHexDecoder;
use crate::pipeline::buffer::Buffer;
use crate::pipeline::dct::PlDct;
use crate::pipeline::flate::{Flate, FlateAction, DEFAULT_OUT_BUFFER_SIZE};
use crate::pipeline::lzw::LzwDecoder;
use crate::pipeline::png_filter::{PngFilter, PngFilterAction};
use crate::pipeline::run_length::{RunLength, RunLengthAction};
use crate::pipeline::tiff_predictor::{TiffPredictor, TiffPredictorAction};
use crate::pipeline::{Pipeline, PipelineError, PipelineRef, PipelineResult};
use crate::{Error, Object, Result};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

pub(crate) const DECODE_OUTPUT_LIMIT_PREFIX: &str = "decoded output exceeds configured limit of";

/// The message a refused `Crypt` stage reports, wherever the refusal happens.
///
/// Two routes can produce it: `filters::reject_crypt_stage`, the crypt provider
/// every non-decrypting decode entry point installs, and
/// [`CryptStreamFilter`]'s `pipe_decode_recovering`, the registry-side route
/// nothing reaches today. One definition rather than one literal per route is
/// what makes the public error genuinely fixed if a `Crypt` stage is ever
/// routed through the registry — two literals could only ever happen to agree.
pub(crate) const CRYPT_STAGE_UNSUPPORTED: &str = "unsupported stream filter: Crypt";

type FilterWarningCallback = Box<dyn FnMut(&str, i32) -> PipelineResult<()> + 'static>;

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
/// an empty key set. Live parser handles forward that warning through their
/// document resolver; a contextless programmatic handle retains qpdf's
/// throwing `typeWarning` branch.
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
/// The `Name`/`Other` split is flpdf's, not qpdf's: `Name` exists for the two
/// name comparisons a `Crypt` stage's consumers make — `/Name` selects the
/// crypt filter and `/Type` is matched against `/CryptFilterDecodeParms` — so
/// carrying it now keeps Phase 3's AES/Crypt cutover from having to widen this
/// shared type. Every other `StreamFilter` still treats the two variants
/// identically.
///
/// **`Name` therefore carries a payload only where one of those comparisons
/// reads it — the `/Name` and `/Type` keys of a `Crypt` stage**
/// ([`CRYPT_NAME_PAYLOAD_DECODE_PARAM_KEYS`]). A name under any other key
/// reduces to `Other`: `/Columns /Identity` is `Other`, not
/// `Name(b"Identity")`, and so is a `Crypt` stage's own `/Foo /Identity`.
/// Nothing can tell the difference, because the only production match on
/// either variant is `set_decode_params`' shared
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

/// Return a human-readable codec label if `filter_name` is one of the four
/// image/binary codecs (`DCTDecode`, `JBIG2Decode`, `JPXDecode`,
/// `CCITTFaxDecode`) that the writer always emits verbatim rather than
/// re-encoding.
///
/// This is an **encode-side** classification, independent of whether flpdf's
/// decode path can currently decode the codec — see [`is_decoded_filter`] for
/// that question. Keeping this classification beside the filter registry lets
/// the qpdf-shaped factory check use the same diagnostic that the later
/// decode stage would have produced for a codec with no decode factory at
/// all.
pub(crate) fn passthrough_codec_label(filter_name: &[u8]) -> Option<&'static str> {
    match filter_name {
        b"DCTDecode" => Some("DCTDecode"),
        b"JBIG2Decode" => Some("JBIG2Decode"),
        b"JPXDecode" => Some("JPXDecode"),
        b"CCITTFaxDecode" => Some("CCITTFaxDecode"),
        _ => None,
    }
}

/// Return whether flpdf's decode path can actually decode `filter_name`.
///
/// [`stream_filter_for`] registers a factory for `Crypt` too, but
/// `filters::prepare_decode_filters` always routes a `Crypt` spec to the
/// installed crypt provider before consulting the registry (see the module
/// doc's "`SF_Crypt::setDecodeParms`... unreached" section), so `Crypt` is
/// excluded here to keep this predicate honest about what a caller like
/// `show-stream` will actually observe.
pub(crate) fn is_decoded_filter(filter_name: &[u8]) -> bool {
    filter_name != b"Crypt" && stream_filter_for(filter_name).is_some()
}

/// Report why a filter name has no decode factory.
pub(crate) fn undecodable_filter_error(filter_name: &[u8]) -> Error {
    if let Some(label) = passthrough_codec_label(filter_name) {
        return Error::Unsupported(format!(
            "passthrough codec {label}: image/binary stream data is not decoded by flpdf (preserved verbatim)"
        ));
    }
    Error::Unsupported(format!(
        "unsupported stream filter: {}",
        std::str::from_utf8(filter_name).unwrap_or("<binary>")
    ))
}

/// Validate `/Filter` names at the same stage as qpdf's `filter_factories`
/// lookup (`QPDF_Stream.cc:419-435`), before `/DecodeParms` is inspected.
pub(crate) fn validate_filter_factories<'a, I>(names: I) -> Result<()>
where
    I: IntoIterator<Item = &'a [u8]>,
{
    for name in names {
        let normalized = normalize_filter_name(name);
        if stream_filter_for(normalized).is_none() {
            return Err(undecodable_filter_error(normalized));
        }
    }
    Ok(())
}

/// `QPDF_Stream::filterable`'s `warn("stream filter type is not name or
/// array")` (`libqpdf/QPDF_Stream.cc:413`). flpdf raises the same text as an
/// error instead of a warning; see plan decision D3 of `flpdf-25kg.3.4`.
pub(crate) const FILTER_TYPE_ERROR: &str = "stream filter type is not name or array";

/// `QPDF_Stream::filterable`'s `warn("stream /DecodeParms length is
/// inconsistent with filters")` (`libqpdf/QPDF_Stream.cc:459`), raised as an
/// error rather than a warning just as [`FILTER_TYPE_ERROR`] is.
///
/// qpdf validates every filter name against `filter_factories` first and
/// returns on an unknown one (`QPDF_Stream.cc:433-435`), so `:459`'s condition
/// is never evaluated for a stream whose `/Filter` names an unimplemented
/// codec. Both flpdf shape readers make the same factory decision before
/// reading `/DecodeParms`, through [`validate_filter_factories`].
pub(crate) const DECODE_PARMS_LENGTH_ERROR: &str =
    "stream /DecodeParms length is inconsistent with filters";

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

    validate_filter_factories(names.iter().copied())?;

    let params: Vec<Option<&Object>> = match decode_params {
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
        Some(item) => {
            // Keep the direct reader borrowed. The public path already owns
            // the parsed object graph, so cloning the whole DecodeParms array
            // before cloning each item would briefly retain two copies of
            // attacker-controlled dictionaries or strings.
            validate_filter_chain_count(names.len(), max_filter_chain)?;
            return Ok(names
                .into_iter()
                .map(|name| FilterSpec {
                    name: name.to_vec(),
                    decode_params: decode_params_from_object(Some(item), name),
                })
                .collect());
        }
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

    validate_filter_factories(names.iter().map(Vec::as_slice))?;

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
/// An unknown name has no registered filter, so it lands on `false`. The shape
/// readers perform that factory lookup before `/DecodeParms` is read, matching
/// qpdf's early return at `QPDF_Stream.cc:433-435` and leaving the later
/// parameter-reduction code concerned only with registered filters.
fn filter_reads_decode_params(filter_name: &[u8]) -> bool {
    // No caller's answer depends on this arm today, and no assertion can
    // witness it: `CryptStreamFilter` is registered and its
    // `reads_decode_params` answers `true` as well, so the lookup below would
    // return the same for every input and deleting this arm would fail no
    // test. It is kept because the registry is not what a `Crypt` stage's
    // parameters are read for. `filters::prepare_decode_filters` peels the
    // spec off into `PreparedStage::Crypt` before consulting
    // `stream_filter_for`, and the crypt provider is then handed
    // `&stage.spec.decode_params` — plan decision D2 of `flpdf-25kg.3.4` has
    // that provider selecting its crypt filter from `/Name`. Leaving it out
    // would make that reader's supply depend on a registry entry it never
    // queries.
    if is_crypt_filter(filter_name) {
        return true;
    }
    stream_filter_for(normalize_filter_name(filter_name))
        .is_some_and(|filter| filter.reads_decode_params())
}

/// Is this the one filter name whose stage flpdf decrypts rather than decodes?
///
/// [`CryptStreamFilter`] is a [`StreamFilter`] and is registered like any
/// other, but `filters::prepare_decode_filters` routes a `Crypt` spec to
/// `PreparedStage::Crypt` before it reaches the registry, so this predicate —
/// not the registry — is what the parameter-retention rules turn on.
///
/// The single spelling of the `b"Crypt"` literal
/// [`filter_reads_decode_params`], [`retains_decode_param_key`] and
/// [`keeps_crypt_name_payload`] all turn on. It still shadows the identical
/// test in `filters::prepare_decode_filters`; the two must move together,
/// because the registry entry is not what keeps them in step.
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
pub(crate) fn decode_params_from_handle(
    params: &ObjectHandle,
    filter_name: &[u8],
) -> Result<DecodeParams> {
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
    let crypt_stage = is_crypt_filter(filter_name);
    let mut retained = Vec::new();
    for key in params.try_get_keys()? {
        let logical_key = legacy_dictionary_key(&key);
        if !retains_decode_param_key(logical_key, crypt_stage) {
            continue;
        }
        let value = params.try_get_key(&key)?;
        let keeps_name = keeps_crypt_name_payload(logical_key, crypt_stage);
        let consumes_integer = consumes_integer_decode_param_key(logical_key, filter_name);
        retained.push((
            logical_key.to_vec(),
            param_value_from_handle(&value, keeps_name, consumes_integer)?,
        ));
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
    let crypt_stage = is_crypt_filter(filter_name);
    let retained = entries
        .iter()
        .filter(|(key, _)| retains_decode_param_key(legacy_dictionary_key(key), crypt_stage))
        .map(|(key, value)| {
            let logical_key = legacy_dictionary_key(key);
            (logical_key.to_vec(), param_value_without_resolving(value))
        })
        .collect();
    Ok(DecodeParams::Present(retained))
}

/// Does qpdf convert this retained key through `getIntValueAsInt`?
///
/// `SF_FlateLzwDecode::setDecodeParms` does so for the four geometry keys and
/// for `/EarlyChange` only in its LZW instance
/// (`libqpdf/SF_FlateLzwDecode.cc:34-57`). `SF_Crypt` walks every key but
/// performs only its `/Type` and `/Name` checks
/// (`libqpdf/QPDF_Stream.cc:33-50`), so an integer under an unknown key must
/// not produce a saturation warning even though the key is retained.
fn consumes_integer_decode_param_key(key: &[u8], filter_name: &[u8]) -> bool {
    if is_crypt_filter(filter_name) {
        return false;
    }
    match key {
        b"Predictor" | b"Columns" | b"Colors" | b"BitsPerComponent" => true,
        b"EarlyChange" => normalize_filter_name(filter_name) == b"LZWDecode",
        _ => false,
    }
}

/// Classify one `/DecodeParms` value, dereferencing it as qpdf's `isInteger`
/// does.
///
/// `keeps_name` is [`keeps_crypt_name_payload`] for the key this value sits
/// under: the name payload is owned only where a `Crypt` stage's consumers
/// compare it. Everywhere else — including a `Crypt` stage's own unknown keys,
/// which are retained for their names alone — a name reduces to
/// [`ParamValue::Other`], which no consumer can distinguish from `Name` — see
/// [`ParamValue`]. `try_as_name` still runs, so the dereference qpdf performs
/// is unchanged; only the payload is dropped. `consumes_integer` is the
/// key-level qpdf boundary for `getIntValueAsInt` and its saturation warning.
fn param_value_from_handle(
    value: &ObjectHandle,
    keeps_name: bool,
    consumes_integer: bool,
) -> Result<ParamValue> {
    if let Some(int) = value.try_as_integer()? {
        let int = if consumes_integer {
            clamp_handle_value_to_i32(int, value)?
        } else {
            clamp_to_i32(int)
        };
        return Ok(ParamValue::Int(int));
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
/// [`is_crypt_filter`] implies that predicate, so
/// [`keeps_crypt_name_payload`] could never be true here: a payload kept at
/// this position would be one nothing reads. A name reduces to `Other` exactly
/// as it does under every other non-`Crypt` filter — see [`ParamValue`].
///
/// That same implication is what confines [`retains_decode_param_key`]'s
/// keep-every-key arm to `Crypt`: [`decode_params_from_entries`] passes the
/// flag on, but the flag is unreachably `true` here, so a non-consuming stage
/// keeps [`RETAINED_DECODE_PARAM_KEYS`] and no more.
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

/// Reduce a `/DecodeParms` dictionary for the `Object` shape reader.
///
/// The direct reader borrows dictionary values and reduces only the bounded
/// parameter view. The xref bootstrap reader uses the resolver-aware sibling
/// below because it must dereference the values it inspects. `filter_name`
/// decides both whether qpdf's filter enumerates entries — and therefore
/// whether null values are omitted — and whether the stage is `Crypt`, which
/// keeps every key ([`retains_decode_param_key`]) with name payloads under
/// [`CRYPT_NAME_PAYLOAD_DECODE_PARAM_KEYS`].
fn decode_params_from_object(params: Option<&Object>, filter_name: &[u8]) -> DecodeParams {
    let omits_null_values = filter_reads_decode_params(filter_name);
    let crypt_stage = is_crypt_filter(filter_name);
    match params {
        None | Some(Object::Null) => DecodeParams::Absent,
        Some(object) => DecodeParams::Present(match object.as_dict() {
            Some(dict) => dict
                .iter()
                .filter(|(_, value)| !omits_null_values || !matches!(value, Object::Null))
                .filter(|(key, _)| retains_decode_param_key(key, crypt_stage))
                .map(|(key, value)| {
                    let keeps_name = keeps_crypt_name_payload(key, crypt_stage);
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
#[allow(dead_code)]
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
    /// `pipeStreamData` caller (`QPDF_Stream.cc:564-567`), not here. That
    /// installation sits *outside* the `if (decode_pipeline)` guard, so a
    /// filter contributing no stage leaves the stage the preceding iteration
    /// installed as the chain head and lets it take the callback again.
    // The qpdf-shaped ObjectHandle consumer owns the production-style caller;
    // the public legacy decode helpers still use pipe_decode_recovering.
    fn decode_pipeline<'a>(
        &mut self,
        next: &'a mut dyn Pipeline,
    ) -> Result<Option<Box<dyn Pipeline + 'a>>> {
        match self.decode_pipeline_owned(PipelineRef::Borrowed(next))? {
            OwnedDecodePipeline::Stage(stage) => Ok(Some(stage)),
            OwnedDecodePipeline::NoStage(_) => Ok(None),
        }
    }

    /// Construct the same stage with a downstream pipeline that may already
    /// own inner stages. The borrowed [`Self::decode_pipeline`] surface keeps
    /// qpdf's `Pipeline*` shape for primitive callers; this companion is the
    /// Rust ownership seam used by `QPDF_Stream::pipeStreamData`'s reverse
    /// chain construction.
    fn decode_pipeline_owned<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>>;

    /// Install the qpdf `QPDF_Stream::pipeStreamData` warning callback on a
    /// filter that constructs a Flate stage. Other filters ignore it.
    fn set_warning_callback(&mut self, _callback: FilterWarningCallback) {}

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

/// Result of constructing a stage around a downstream pipeline that may
/// already own inner stages. `NoStage` returns the downstream slot so the
/// caller can keep threading it through a filter such as qpdf's `Crypt`.
#[allow(dead_code)]
pub(crate) enum OwnedDecodePipeline<'a> {
    Stage(Box<dyn Pipeline + 'a>),
    NoStage(PipelineRef<'a>),
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
    warning_callback: Option<FilterWarningCallback>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PredictorKind {
    Png,
    Tiff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PredictorGeometry {
    kind: PredictorKind,
    columns: u32,
    colors: u32,
    bits_per_component: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PredictorAction {
    Encode,
    Decode,
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
            warning_callback: None,
        }
    }
}

/// The `/DecodeParms` keys some flpdf consumer reads under *every* filter.
///
/// [`DecodeParams`] keeps only these, except under a `Crypt` stage, which
/// keeps every key because its `setDecodeParms` reads every key — see
/// [`retains_decode_param_key`]. qpdf has no
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
/// laziness.** The encode path validates each registered filter's
/// `set_decode_params` contract before applying a codec: Flate/LZW consume the
/// predictor geometry, while ASCII85/ASCIIHex/RunLength reject a present
/// `/DecodeParms` just as qpdf's default filter does. Dropping the geometry
/// before that validation would turn a non-null parameter dictionary into an
/// absent one. The keys beyond these five have no such second consumer, which
/// is why they are kept per-filter — under `Crypt` and nowhere else.
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
/// **A `Crypt` stage is the exception. Per stage, it grows in key text.** It
/// keeps every key, so its snapshot tracks the *number and length of the
/// source's key names* — that is what makes filterability reproducible at all
/// ([`retains_decode_param_key`]). It does not track the source's *values*:
/// only [`CRYPT_NAME_PAYLOAD_DECODE_PARAM_KEYS`] carry a name payload, so an
/// unknown key contributes its key bytes and nothing else.
/// `a_crypt_stage_grows_only_by_the_key_bytes_of_an_unknown_entry` pins that
/// split as an exact figure, with an unknown key's value of one mebibyte.
///
/// **That is per stage, and the chain multiplies it.** An *n*-stage `/Crypt`
/// chain sharing one scalar `/DecodeParms` holds *n* copies of that key text
/// while the source grew by only *n* filter names, so the retained total is
/// not bounded by the source's size — see
/// [`CRYPT_NAME_PAYLOAD_DECODE_PARAM_KEYS`]' per-stage residual, which this
/// widening enlarges from the `/Name` payload to the whole key set.
/// `a_crypt_chain_holds_the_whole_key_set_once_per_stage` pins the product.
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

/// The two `/DecodeParms` keys whose *name payload* a `Crypt` stage's
/// consumers read, and which are kept nowhere else.
///
/// `/Name` selects the crypt filter. Plan decision D2 of `flpdf-25kg.3.4` has
/// the crypt provider doing that selection, and `filters::CryptProvider` is
/// `FnMut(&DecodeParams, &[u8])` — no handle in it — so [`DecodeParams`] is
/// that provider's only route to the name. qpdf needs no such route:
/// `decryptStream` re-reads it off the live object graph
/// (`decode_parms.getKey("/Name")`, `libqpdf/QPDF_encryption.cc:1072`, and the
/// `/CF` filter's own `/Name` at `:1085-1087`).
///
/// `/Type` decides whether the stage is filterable at all.
/// `SF_Crypt::setDecodeParms` (`libqpdf/QPDF_Stream.cc:41-43`) admits a
/// `/Type`-bearing dictionary only when
/// `isDictionaryOfType("/CryptFilterDecodeParms")` holds, and that is
/// `getKey("/Type").isNameAndEquals(...)` (`QPDFObjectHandle.cc:462-466`) — a
/// comparison against the name's bytes. Reduced to [`ParamValue::Other`] the
/// value would be indistinguishable from `/Type /Foo`, which qpdf refuses.
///
/// No other key's payload is read anywhere: the base
/// `StreamFilter::set_decode_params` reads only `is_absent()`,
/// `FlateLzwStreamFilter::set_decode_params` asks `isInteger`-shaped questions
/// of [`RETAINED_DECODE_PARAM_KEYS`] and lets every other key fall through its
/// `_ => {}` arm, and [`predictor_encode_geometry`] reaches the parameters through
/// that same filter.
///
/// **This is a per-stage residual, not parity, and it is not a bound on the
/// chain.** Each `Crypt` stage converts and keeps its own copy, so `/Filter
/// [/Crypt /Crypt …]` sharing one scalar `/DecodeParms` holds, per stage, that
/// dictionary's **whole key text** plus a payload for each of these two keys —
/// [`retains_decode_param_key`] keeps every key under `Crypt`, so the residual
/// is the key set and not just these two names. An *n*-stage chain therefore
/// retains *n* × that quantity while the source grew by only the *n* filter
/// names, which
/// `a_crypt_chain_holds_the_whole_key_set_once_per_stage` pins as an exact
/// figure at two chain lengths. It is capped by
/// `filters::DecodeLimits::max_filter_chain` where a caller sets one (16 under
/// `DecodeLimits::default()`) and uncapped where a caller passes `None`, which
/// `filters::encode_stream_data` does unconditionally. qpdf holds one
/// `shared_ptr` at any chain length.
///
/// Keeping every key is required — dropping one would accept a stream
/// `SF_Crypt::setDecodeParms` rejects — so this residual is the cost of
/// reproducing filterability, not an oversight to bound away. Restricting it
/// to `Crypt` narrows the exposure to chains that name `Crypt`; it does not
/// remove it.
const CRYPT_NAME_PAYLOAD_DECODE_PARAM_KEYS: [&[u8]; 2] = [b"Name", b"Type"];

/// Does a stage's [`DecodeParams`] keep this key at all?
///
/// **A `Crypt` stage keeps every key, and that is this same rule rather than an
/// exception to it.** The rule is "retain what the consumer reads", and
/// `SF_Crypt::setDecodeParms` (`libqpdf/QPDF_Stream.cc:33-50`) reads the whole
/// key set: it walks `decode_parms.getKeys()` and sets `filterable = false` on
/// the first key that is neither `/Type` nor `/Name`. A key dropped here is a
/// key that stage could no longer refuse, so `/DecodeParms << /Foo 1 >>` would
/// reach it as an empty entry set and be accepted where qpdf rejects.
///
/// Every other filter keeps [`RETAINED_DECODE_PARAM_KEYS`] only, because
/// `SF_FlateLzwDecode::setDecodeParms` is the only other `setDecodeParms` that
/// looks at an entry and it names exactly those five
/// (`libqpdf/SF_FlateLzwDecode.cc:32-66`).
///
/// The cost of the `Crypt` arm is key bytes and nothing else:
/// [`keeps_crypt_name_payload`] admits two keys, so an unknown key's value
/// still reduces to [`ParamValue::Other`] — see [`RETAINED_DECODE_PARAM_KEYS`]'
/// bound, which this widening therefore leaves standing for every filter but
/// `Crypt`.
fn retains_decode_param_key(key: &[u8], crypt_stage: bool) -> bool {
    crypt_stage || RETAINED_DECODE_PARAM_KEYS.contains(&key)
}

/// Is this the entry whose *name payload* some consumer reads?
///
/// The only place a `ParamValue::Name` keeps its bytes.
///
/// **A payload is never owned under a key that is not kept**, and that holds
/// structurally rather than by keeping two lists aligned: this answers `true`
/// only when `crypt_stage` does, and [`retains_decode_param_key`] answers
/// `true` for *every* key when `crypt_stage` does. The shared flag is what the
/// two cannot drift apart on, in place of the single key literal they used to
/// share.
///
/// `crypt_stage` changes no *read's* answer as the two arrays stand today.
/// This runs only for keys [`retains_decode_param_key`] already kept, and
/// under a non-`Crypt` filter those are [`RETAINED_DECODE_PARAM_KEYS`], whose
/// five keys happen to be disjoint from the array above. That disjointness is
/// a property of the two current arrays, not a standing invariant: adding
/// `/Name` to the geometry array would make them overlap and, without this
/// gate, hand the payload back to every filter — the amplification
/// [`RETAINED_DECODE_PARAM_KEYS`]' bound exists to prevent.
///
/// No read's answer depends on this gate today, so it is asserted directly by
/// `a_name_payload_is_kept_under_a_crypt_stage_and_only_there`.
fn keeps_crypt_name_payload(key: &[u8], crypt_stage: bool) -> bool {
    crypt_stage && CRYPT_NAME_PAYLOAD_DECODE_PARAM_KEYS.contains(&key)
}

/// Apply `getIntValueAsInt`'s value saturation
/// (`QPDFObjectHandle.cc:525-543`), which pins a value below `INT_MIN` to
/// `INT_MIN` and one above `INT_MAX` to `INT_MAX` rather than failing.
///
/// This is the shape-independent half of the parity, kept separate from
/// `clamped_int_param` so a second `/DecodeParms` shape reader clamps through
/// this one copy instead of restating the bounds.
fn clamp_to_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Apply qpdf's saturation and its `warnIfPossible` diagnostics to a live
/// handle. `QPDFObjectHandle::getIntValueAsInt` calls `warnIfPossible` only
/// after the caller has established that the value is an integer
/// (`SF_FlateLzwDecode.cc:34-57`), so this helper deliberately does not emit a
/// type warning for names or other non-integer values.
fn clamp_handle_value_to_i32(value: i64, handle: &ObjectHandle) -> Result<i32> {
    if value < i64::from(i32::MIN) {
        handle.warn_if_possible("requested value of integer is too small; returning INT_MIN")?;
    } else if value > i64::from(i32::MAX) {
        handle.warn_if_possible("requested value of integer is too big; returning INT_MAX")?;
    }
    Ok(clamp_to_i32(value))
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

    fn set_warning_callback(&mut self, callback: FilterWarningCallback) {
        self.warning_callback = Some(callback);
    }

    fn preflight_decode_pipeline(&self) -> Result<()> {
        if let Some(geometry) = self.decode_predictor_geometry()? {
            let mut sink = OutputBuffer::new(None);
            let _predictor = make_predictor_pipeline(geometry, &mut sink, PredictorAction::Decode)?;
        }
        Ok(())
    }

    /// Mirrors `SF_FlateLzwDecode::getDecodePipeline`
    /// (`libqpdf/SF_FlateLzwDecode.cc:75-110`): a predictor stage first when
    /// the parameters call for one, with `next` reassigned to it, then the
    /// codec wrapping whichever `next` resulted. The codec is what the caller
    /// receives.
    fn decode_pipeline_owned<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        let next: PipelineRef<'a> = match self.decode_predictor_geometry()? {
            Some(geometry) => make_predictor_pipeline(geometry, next, PredictorAction::Decode)?,
            None => next,
        };
        let stage: Box<dyn Pipeline + 'a> = if self.lzw {
            Box::new(LzwDecoder::new("lzw decode", next, self.early_code_change))
        } else {
            let mut flate = Flate::new(
                "stream inflate",
                next,
                FlateAction::Inflate,
                DEFAULT_OUT_BUFFER_SIZE,
            )
            .map_err(map_pipeline_error)?;
            if let Some(callback) = self.warning_callback.take() {
                flate.set_warn_callback(callback);
            }
            Box::new(flate)
        };
        Ok(OwnedDecodePipeline::Stage(stage))
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
            Some(geometry) => {
                let mut predictor =
                    make_predictor_pipeline(geometry, &mut sink, PredictorAction::Decode)?;
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
    fn decode_predictor_geometry(&self) -> Result<Option<PredictorGeometry>> {
        let kind = if (10..=15).contains(&self.predictor) {
            Some(PredictorKind::Png)
        } else if self.predictor == 2 {
            Some(PredictorKind::Tiff)
        } else {
            None
        };
        kind.map(|kind| {
            Ok(PredictorGeometry {
                kind,
                columns: to_uint(self.columns)?,
                colors: to_uint(self.colors)?,
                bits_per_component: to_uint(self.bits_per_component)?,
            })
        })
        .transpose()
    }

    /// Run the codec stage of the whole-buffer route over `data`.
    ///
    /// **Recorded deviation:** the `Pl_Flate` warn callback is installed here,
    /// on the stage this function constructs, where qpdf installs it at the
    /// `getDecodePipeline` caller (`QPDF_Stream.cc:564-567`). Every `Pl_Flate`
    /// this route builds still receives the callback qpdf would install at
    /// that filter's own iteration, so warning text and order are unchanged.
    /// What installing it here cannot reproduce is qpdf's other case: the cast
    /// runs once per filter rather than once per constructed stage, so an
    /// iteration whose filter builds nothing lands it on a stage constructed
    /// elsewhere — see [`StreamFilter::decode_pipeline`]. Both that case and
    /// the placement belong with the port of `QPDF_Stream::pipeStreamData`.
    ///
    /// Nothing today can observe the difference: this route decodes each
    /// filter's whole buffer in one call, constructing and finishing that
    /// filter's stages within it, so no chain head survives into the next
    /// filter — and `Crypt`, the only filter whose `decode_pipeline` builds
    /// nothing, never reaches this function at all, because
    /// `filters::prepare_decode_filters` routes its spec to
    /// `PreparedStage::Crypt` instead of to a codec adapter.
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

fn make_predictor_pipeline<'a>(
    geometry: PredictorGeometry,
    next: impl Into<PipelineRef<'a>>,
    action: PredictorAction,
) -> Result<PipelineRef<'a>> {
    let next = next.into();
    let pipeline = match (geometry.kind, action) {
        (PredictorKind::Png, PredictorAction::Encode) => Box::new(
            PngFilter::new(
                "png encode",
                next,
                PngFilterAction::Encode,
                geometry.columns,
                geometry.colors,
                geometry.bits_per_component,
            )
            .map_err(map_pipeline_error)?,
        ) as Box<dyn Pipeline + 'a>,
        (PredictorKind::Png, PredictorAction::Decode) => Box::new(
            PngFilter::new(
                "png decode",
                next,
                PngFilterAction::Decode,
                geometry.columns,
                geometry.colors,
                geometry.bits_per_component,
            )
            .map_err(map_pipeline_error)?,
        ) as Box<dyn Pipeline + 'a>,
        (PredictorKind::Tiff, PredictorAction::Encode) => Box::new(
            TiffPredictor::new(
                "tiff encode",
                next,
                TiffPredictorAction::Encode,
                geometry.columns,
                geometry.colors,
                geometry.bits_per_component,
            )
            .map_err(map_pipeline_error)?,
        ) as Box<dyn Pipeline + 'a>,
        (PredictorKind::Tiff, PredictorAction::Decode) => Box::new(
            TiffPredictor::new(
                "tiff decode",
                next,
                TiffPredictorAction::Decode,
                geometry.columns,
                geometry.colors,
                geometry.bits_per_component,
            )
            .map_err(map_pipeline_error)?,
        ) as Box<dyn Pipeline + 'a>,
    };
    Ok(PipelineRef::Owned(pipeline))
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
    fn decode_pipeline_owned<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Ok(OwnedDecodePipeline::Stage(Box::new(Ascii85Decoder::new(
            "ascii85 decode",
            next,
        ))))
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
    fn decode_pipeline_owned<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Ok(OwnedDecodePipeline::Stage(Box::new(AsciiHexDecoder::new(
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
    fn decode_pipeline_owned<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Ok(OwnedDecodePipeline::Stage(Box::new(RunLength::new(
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

struct DctStreamFilter;

impl StreamFilter for DctStreamFilter {
    /// Mirrors `SF_DCTDecode::getDecodePipeline`
    /// (`libqpdf/qpdf/SF_DCTDecode.hh:14-19`), a single `Pl_DCT` decode stage.
    fn decode_pipeline_owned<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Ok(OwnedDecodePipeline::Stage(Box::new(PlDct::new(
            "DCT decode",
            next,
        ))))
    }

    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        max_output: Option<usize>,
        _warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        let mut sink = OutputBuffer::new(max_output);
        let finish_phase = sink.finish_phase();
        let output_position = sink.output_position();
        let error = {
            let mut stage = PlDct::new("DCT decode", &mut sink).with_max_output(max_output);
            write_and_finish(
                &mut stage,
                data,
                Some(finish_phase.as_ref()),
                &output_position,
            )
            .map(map_stage_error)
        };
        let cleanup_data_start = sink.cleanup_data_start();
        Ok(FilterDecodeOutcome {
            data: sink.data,
            cleanup_data_start,
            error,
        })
    }

    fn is_specialized_compression(&self) -> bool {
        true
    }

    fn is_lossy_compression(&self) -> bool {
        true
    }
}

/// Port of the anonymous-namespace `SF_Crypt` in `libqpdf/QPDF_Stream.cc:27-58`.
///
/// It decodes nothing. Its whole contribution is deciding filterability from
/// `/DecodeParms`, which is why qpdf lists it in `filter_factories`
/// (`QPDF_Stream.cc:85-94`) beside that table's six codec filters even though
/// its `getDecodePipeline` returns `nullptr`.
struct CryptStreamFilter;

/// Locate a `Crypt` stage's `Type` entry, or `None` when it has none.
///
/// **A plain entry-set lookup answers qpdf's `hasKey` only because a `Crypt`
/// stage's parameters arrive with null values already dropped**, matching
/// `QPDF_Dictionary::getKeys` (`QPDF_Dictionary.cc:118-127`) and
/// `QPDF_Dictionary::hasKey` (`:98-101`), which both skip a null value. Both
/// readers that can feed a `Crypt` stage do drop them —
/// `decode_params_from_object` gates on `filter_reads_decode_params`, and
/// `decode_params_from_consuming_handle` takes its keys from `try_get_keys` —
/// and the same [`filter_reads_decode_params`] arm selects them for `Crypt`.
/// `decode_params_from_entries` keeps null values and is the reader a `Crypt`
/// stage never reaches. These helpers are therefore private to this filter
/// rather than methods on [`DecodeParams`]: under a non-consuming filter the
/// same lookup would not mean qpdf's `hasKey`.
fn crypt_params_type_entry(decode_params: &DecodeParams) -> Option<&ParamValue> {
    decode_params
        .entries()
        .iter()
        .find(|(key, _)| key.as_slice() == b"Type")
        .map(|(_, value)| value)
}

/// `decode_parms.hasKey("/Type")` (`QPDFObjectHandle.cc:965-976`), read off the
/// located entry.
fn crypt_params_have_type(type_entry: Option<&ParamValue>) -> bool {
    type_entry.is_some()
}

/// `decode_parms.isDictionaryOfType("/CryptFilterDecodeParms")`
/// (`QPDFObjectHandle.cc:462-466`), which with an empty `subtype` reduces to
/// `isDictionary() && getKey("/Type").isNameAndEquals("/CryptFilterDecodeParms")`.
///
/// The `isDictionary()` conjunct needs no counterpart: a present
/// non-dictionary `/DecodeParms` reduces to `Present` with no entries, so the
/// key walk that reaches this predicate never runs. `getKey` returns null for
/// an absent key and `isNameAndEquals` is then `false`, which is what `None`
/// yields here.
fn crypt_params_are_dictionary_of_type(type_entry: Option<&ParamValue>) -> bool {
    matches!(type_entry, Some(ParamValue::Name(name)) if name.as_slice() == b"CryptFilterDecodeParms")
}

impl StreamFilter for CryptStreamFilter {
    /// `SF_Crypt::setDecodeParms` walks `decode_parms.getKeys()`
    /// (`QPDF_Stream.cc:40`), so it reads the parameter entries.
    fn reads_decode_params(&self) -> bool {
        true
    }

    /// Port of `SF_Crypt::setDecodeParms` (`QPDF_Stream.cc:33-50`).
    ///
    /// Every key must be `Type` or `Name`, and a present `Type` must name
    /// `/CryptFilterDecodeParms`. Observed against qpdf 11.9.0 on 2026-08-08
    /// through `qpdf --show-object=4 --filtered-stream-data` over a stream
    /// whose `/Filter` is `/Crypt`: `<< /Name /Identity >>`,
    /// `<< /Type /CryptFilterDecodeParms >>` and `<< >>` exit 0, while
    /// `<< /Type /Foo >>`, `<< /Foo 1 >>` and
    /// `<< /Type /CryptFilterDecodeParms /Foo 1 >>` exit 2 with "unable to
    /// filter stream data".
    ///
    /// The `Type` validity test is evaluated *inside* the loop, as qpdf
    /// evaluates it, even though hoisting the predicate would answer
    /// identically on every shape — qpdf's loop shape is kept deliberately,
    /// not because behaviour depends on it.
    fn set_decode_params(&mut self, decode_params: &DecodeParams) -> bool {
        // QPDF_Stream.cc:36-38. `DecodeParams` has already folded a missing
        // key into the null case.
        if decode_params.is_absent() {
            return true;
        }
        // qpdf looks the `/Type` entry up on the dictionary once per iteration.
        // The answer cannot differ between iterations — `decode_params` is
        // borrowed immutably for the whole loop — and locating the entry is not
        // an observable operation: it reads an already-materialized `Vec`,
        // resolves no indirect object, emits no warning, and cannot fail. It is
        // therefore done once here and read per iteration. Both predicates stay
        // where qpdf evaluates them, so every per-key decision and its position
        // in the loop are unchanged. The `Vec`'s lookup cost against qpdf's
        // `std::map` follows from the snapshot shape recorded at the top of
        // this module; it is not what this is for.
        let type_entry = crypt_params_type_entry(decode_params);
        let mut filterable = true;
        for (key, _) in decode_params.entries() {
            if ((key.as_slice() == b"Type") || (key.as_slice() == b"Name"))
                && ((!crypt_params_have_type(type_entry))
                    || crypt_params_are_dictionary_of_type(type_entry))
            {
                // qpdf handles these two in decryptStream.
            } else {
                filterable = false;
            }
        }
        filterable
    }

    /// Port of `SF_Crypt::getDecodePipeline` (`QPDF_Stream.cc:52-56`), whose
    /// whole body returns `nullptr`: a `Crypt` stage contributes no decode
    /// stage, because decryption happens in `decryptStream` instead.
    ///
    /// A caller that installs this `None` must therefore already be reading
    /// through a decrypting source; qpdf's filter loop
    /// (`QPDF_Stream.cc:559-568`) runs after `decryptStream` has been applied
    /// to the source bytes. Without such a source the stage is not merely
    /// absent but wrong, and silently so — ciphertext would pass through as
    /// plaintext with neither an error nor a warning, which is why the
    /// decode route below refuses instead of returning the bytes.
    fn decode_pipeline_owned<'a>(
        &mut self,
        _next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Ok(OwnedDecodePipeline::NoStage(_next))
    }

    /// Refuse to decode, reporting [`CRYPT_STAGE_UNSUPPORTED`].
    ///
    /// qpdf has no counterpart to mirror: `SF_Crypt` contributes no pipeline
    /// and decryption is `decryptStream`'s job, so this route is flpdf's
    /// alone. Nothing reaches it today —
    /// `filters::prepare_decode_filters` routes a `Crypt` spec to
    /// `PreparedStage::Crypt` before the registry is consulted, and the crypt
    /// provider every non-decrypting entry point installs is
    /// `filters::reject_crypt_stage`. Sharing that provider's message — the
    /// same constant, not a second copy of it — is what keeps the public error
    /// unchanged if decoding is ever routed here instead.
    fn pipe_decode_recovering(
        &mut self,
        _data: &[u8],
        _max_output: Option<usize>,
        _warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        Err(Error::Unsupported(CRYPT_STAGE_UNSUPPORTED.to_string()))
    }
}

#[cfg(test)]
struct TestStreamFilter;

#[cfg(test)]
impl StreamFilter for TestStreamFilter {
    // Passes data through untouched, so it builds no stage of its own —
    // qpdf's nullptr, which leaves the caller writing straight to `next`.
    fn decode_pipeline_owned<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Ok(OwnedDecodePipeline::NoStage(next))
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
    fn decode_pipeline_owned<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Ok(OwnedDecodePipeline::NoStage(next))
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
    fn decode_pipeline_owned<'a>(&mut self, _: PipelineRef<'a>) -> Result<OwnedDecodePipeline<'a>> {
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

/// Construct the filter registered under `filter_name`, if any.
///
/// **Recorded deviation (CLAUDE.md class (B)):** qpdf holds the same registry
/// in a `std::map`, `QPDF_Stream::filter_factories` (`QPDF_Stream.cc:85-94`).
/// Nothing iterates that map — the only read is a lookup by name
/// (`QPDF_Stream.cc:425-426`) — so a `match` carries a name-to-factory
/// mapping just as faithfully. What a `match` cannot carry is
/// `QPDF_Stream::registerStreamFilter` (`QPDF_Stream.cc:148-151`), which lets
/// a library user add a factory at run time; flpdf exposes no counterpart, and
/// adding one would mean replacing this `match`.
///
/// The container and qpdf's registered production codecs are represented here;
/// the DCT stage itself is the qpdf-shaped streaming primitive, and the
/// whole-buffer adapter below drives that same stage for legacy callers.
pub(crate) fn stream_filter_for(filter_name: &[u8]) -> Option<Box<dyn StreamFilter>> {
    match filter_name {
        b"Crypt" => Some(Box::new(CryptStreamFilter)),
        b"FlateDecode" => Some(Box::new(FlateLzwStreamFilter::new(false))),
        b"LZWDecode" => Some(Box::new(FlateLzwStreamFilter::new(true))),
        b"ASCII85Decode" => Some(Box::new(Ascii85StreamFilter)),
        b"ASCIIHexDecode" => Some(Box::new(AsciiHexStreamFilter)),
        b"RunLengthDecode" => Some(Box::new(RunLengthStreamFilter)),
        b"DCTDecode" => Some(Box::new(DctStreamFilter)),
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

/// Resolve the predictor geometry a writer must apply for `/DecodeParms`.
///
/// Returns `Ok(None)` when the parameters select no predictor. The
/// parameters are validated through the same `SF_FlateLzwDecode` state the
/// decode path uses, so both directions accept exactly the same dictionaries.
fn predictor_encode_geometry(
    filter_name: &[u8],
    decode_params: &DecodeParams,
) -> Result<Option<PredictorGeometry>> {
    // qpdf's default QPDFStreamFilter::setDecodeParms accepts only a null
    // object. ASCII85, ASCIIHex, and RunLength inherit that contract; only
    // SF_FlateLzwDecode consumes predictor parameters. Validate the registered
    // non-Flate filter here so encoding cannot produce bytes for a stream that
    // the inverse decode path rejects.
    if !matches!(filter_name, b"FlateDecode" | b"LZWDecode") {
        let Some(mut filter) = stream_filter_for(filter_name) else {
            // Let the codec encoder report an unknown or passthrough filter.
            return Ok(None);
        };
        if !filter.set_decode_params(decode_params) {
            return Err(Error::Unsupported(format!(
                "stream filter {} does not support supplied /DecodeParms",
                String::from_utf8_lossy(filter_name)
            )));
        }
        return Ok(None);
    }

    let mut filter = FlateLzwStreamFilter::new(filter_name == b"LZWDecode");
    if !filter.set_decode_params(decode_params) {
        return Err(Error::Unsupported(format!(
            "stream filter {} does not support supplied /DecodeParms",
            String::from_utf8_lossy(filter_name)
        )));
    }
    filter.decode_predictor_geometry()
}

/// Apply the predictor selected by `/DecodeParms` before a codec's encode step.
pub(crate) fn encode_predictor(
    data: &[u8],
    filter_name: &[u8],
    decode_params: &DecodeParams,
) -> Result<Vec<u8>> {
    let Some(geometry) = predictor_encode_geometry(filter_name, decode_params)? else {
        return Ok(data.to_vec());
    };
    encode_predictor_stage(data, geometry)
}

fn encode_predictor_stage(data: &[u8], geometry: PredictorGeometry) -> Result<Vec<u8>> {
    let mut sink = Buffer::new("stream data buffer", None);
    {
        let mut predictor = make_predictor_pipeline(geometry, &mut sink, PredictorAction::Encode)?;
        predictor.write(data).map_err(map_pipeline_error)?;
        predictor.finish().map_err(map_pipeline_error)?;
    }
    sink.take_buffer().map_err(map_pipeline_error)
}

/// Apply the PNG predictor to unencoded stream data.
///
/// qpdf's `Pl_PNGFilter` encoder always emits the Up filter, so the predictor
/// number selects only whether the predictor runs, never which row filter the
/// output uses.
#[cfg(test)]
pub(crate) fn encode_png_predictor(
    data: &[u8],
    columns: u32,
    colors: u32,
    bits_per_component: u32,
) -> Result<Vec<u8>> {
    encode_predictor_stage(
        data,
        PredictorGeometry {
            kind: PredictorKind::Png,
            columns,
            colors,
            bits_per_component,
        },
    )
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
        consumes_integer_decode_param_key, decode_filter_specs_from_handle,
        decode_filter_specs_from_object, decode_flate, decode_flate_chunks,
        decode_params_from_handle, decode_params_from_object, encode_flate, encode_run_length,
        ignore_codec_warning, ignore_warning, keeps_crypt_name_payload, normalize_filter_name,
        param_value_from_handle, stream_filter_for, Ascii85StreamFilter, AsciiHexStreamFilter,
        CryptStreamFilter, DecodeParams, FilterSpec, FlateLzwStreamFilter, ObjectHandle,
        OutputBuffer, ParamValue, Pipeline, PipelineError, PipelineResult, RunLengthStreamFilter,
        StreamFilter, DECODE_OUTPUT_LIMIT_PREFIX, RETAINED_DECODE_PARAM_KEYS,
    };
    use crate::object_handle::identity_tests::{
        logged_resolver_bearing_handle, resolver_bearing_handle,
    };
    use crate::object_handle::warning_emission_tests::{handle_resolving, warnings};
    use crate::object_handle::ObjectValue;
    use crate::pipeline::lzw::pack_codes;
    use crate::pipeline::test_support::{RecordingSink, Trace, TraceCall};
    use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
    use std::cell::{Cell, RefCell};
    use std::env;
    use std::fs;
    use std::io::{Cursor, ErrorKind};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::rc::Rc;

    fn test_jpeg() -> Vec<u8> {
        let pixels = [0u8, 32, 64, 96, 128, 160, 192, 224, 255, 240, 120, 8];
        libjpeg_turbo_rs::compress(
            &pixels,
            2,
            2,
            libjpeg_turbo_rs::PixelFormat::Rgb,
            75,
            libjpeg_turbo_rs::Subsampling::S444,
        )
        .expect("test JPEG must encode")
    }

    fn dct_qpdf_fixture(jpeg: &[u8]) -> tempfile::TempDir {
        let mut pdf = b"%PDF-1.3\n%\xff\xff\xff\xff\n".to_vec();
        let mut object_offsets = Vec::new();
        let mut append_object = |object_number: u32, body: &[u8]| {
            object_offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{object_number} 0 obj\n").as_bytes());
            pdf.extend_from_slice(body);
            if !body.ends_with(b"\n") {
                pdf.push(b'\n');
            }
            pdf.extend_from_slice(b"endobj\n");
        };

        append_object(1, b"<< /Type /Catalog /Pages 2 0 R >>\n");
        append_object(2, b"<< /Type /Pages /Kids [] /Count 0 >>");
        let mut image = format!(
            "<< /Type /XObject /Subtype /Image /Filter /DCTDecode /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length {} >>\nstream\n",
            jpeg.len()
        )
        .into_bytes();
        image.extend_from_slice(jpeg);
        image.extend_from_slice(b"\nendstream\n");
        append_object(3, &image);

        assert_eq!(object_offsets.len(), 3);
        let xref_offset = pdf.len();
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        for offset in &object_offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n");
        let startxref_value_start = pdf.len();
        pdf.extend_from_slice(xref_offset.to_string().as_bytes());
        let startxref_value_end = pdf.len();
        pdf.extend_from_slice(b"\n%%EOF\n");

        assert!(pdf
            .windows(b"/Root 1 0 R".len())
            .any(|window| window == b"/Root 1 0 R"));
        assert!(pdf[xref_offset..].starts_with(b"xref\n0 4\n"));
        for (index, offset) in object_offsets.iter().enumerate() {
            let header = format!("{} 0 obj\n", index + 1);
            assert!(pdf[*offset..].starts_with(header.as_bytes()));
        }
        let recorded_startxref =
            std::str::from_utf8(&pdf[startxref_value_start..startxref_value_end])
                .expect("startxref must be ASCII")
                .parse::<usize>()
                .expect("startxref must be a decimal offset");
        assert_eq!(recorded_startxref, xref_offset);

        let directory = tempfile::tempdir().expect("temporary qpdf fixture directory");
        fs::write(directory.path().join("dct-image.pdf"), pdf)
            .expect("write deterministic DCT qpdf fixture");
        directory
    }

    fn canonical_dct_bytes(jpeg: &[u8]) -> Vec<u8> {
        let mut filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(filter.set_decode_params(&DecodeParams::Absent));
        let mut sink = DctSink::default();
        {
            let mut stage = filter
                .decode_pipeline(&mut sink)
                .expect("DCT stage construction must succeed")
                .expect("DCT filter must contribute a decode stage");
            stage.write(jpeg).expect("canonical DCT write must succeed");
            stage.finish().expect("canonical DCT finish must succeed");
        }

        sink.writes.into_iter().flatten().collect()
    }

    fn hex_bytes(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn qpdf_candidates_select_explicit_override_without_environment() {
        let override_path = Path::new("/tmp/qpdf-override");
        assert_eq!(
            qpdf_candidates_for(Some(override_path)),
            vec![override_path.to_path_buf()]
        );
    }

    #[test]
    fn qpdf_candidates_ignore_empty_override_and_use_default_order() {
        let candidates = qpdf_candidates_for(Some(Path::new("")));
        #[cfg(target_os = "linux")]
        assert_eq!(
            candidates,
            vec![PathBuf::from(PINNED_QPDF_BINARY), PathBuf::from("qpdf")]
        );
        #[cfg(not(target_os = "linux"))]
        assert_eq!(candidates, vec![PathBuf::from("qpdf")]);
    }

    #[test]
    fn qpdf_candidates_use_pinned_linux_then_path_without_override() {
        let candidates = qpdf_candidates_for(None);
        #[cfg(target_os = "linux")]
        assert_eq!(
            candidates,
            vec![PathBuf::from(PINNED_QPDF_BINARY), PathBuf::from("qpdf")]
        );
        #[cfg(not(target_os = "linux"))]
        assert_eq!(candidates, vec![PathBuf::from("qpdf")]);
    }

    #[test]
    fn qpdf_version_not_found_tries_the_next_candidate() {
        assert_eq!(
            classify_qpdf_version(Path::new("/usr/bin/qpdf"), true, QpdfVersionProbe::NotFound,),
            QpdfVersionDecision::TryNext
        );
    }

    #[test]
    fn qpdf_version_not_found_reports_skip_for_the_last_candidate() {
        assert_eq!(
            classify_qpdf_version(Path::new("qpdf"), false, QpdfVersionProbe::NotFound,),
            QpdfVersionDecision::Skip(
                "qpdf unavailable; skipping DCT differential for qpdf version 11.9.0".to_owned()
            )
        );
    }

    #[test]
    fn qpdf_version_launch_failure_is_not_a_skip() {
        assert_eq!(
            classify_qpdf_version(
                Path::new("/bad/qpdf"),
                false,
                QpdfVersionProbe::LaunchError("permission denied".to_owned()),
            ),
            QpdfVersionDecision::Fail(
                "failed to invoke /bad/qpdf --version for qpdf version 11.9.0: permission denied"
                    .to_owned()
            )
        );
    }

    #[test]
    fn qpdf_version_status_failure_preserves_diagnostic_fields() {
        assert_eq!(
            classify_qpdf_version(
                Path::new("/bad/qpdf"),
                false,
                QpdfVersionProbe::Output {
                    status_success: false,
                    status: "exit status: 1".to_owned(),
                    stdout: b"bad".to_vec(),
                    stderr: b"diagnostic\n".to_vec(),
                },
            ),
            QpdfVersionDecision::Fail(
                "/bad/qpdf --version failed while checking qpdf version 11.9.0: status=exit status: 1\nstdout length=3 hex=62 61 64\nstderr length=11 hex=64 69 61 67 6e 6f 73 74 69 63 0a text=\"diagnostic\\n\""
                    .to_owned()
            )
        );
    }

    #[test]
    fn qpdf_version_stderr_failure_preserves_diagnostic_fields() {
        assert_eq!(
            classify_qpdf_version(
                Path::new("/bad/qpdf"),
                false,
                QpdfVersionProbe::Output {
                    status_success: true,
                    status: "exit status: 0".to_owned(),
                    stdout: b"qpdf version 11.9.0\n".to_vec(),
                    stderr: b"noise\n".to_vec(),
                },
            ),
            QpdfVersionDecision::Fail(
                "/bad/qpdf --version wrote stderr while checking qpdf version 11.9.0: length=6 hex=6e 6f 69 73 65 0a text=\"noise\\n\""
                    .to_owned()
            )
        );
    }

    #[test]
    fn qpdf_version_mismatch_preserves_stdout_diagnostic() {
        assert_eq!(
            classify_qpdf_version(
                Path::new("/bad/qpdf"),
                false,
                QpdfVersionProbe::Output {
                    status_success: true,
                    status: "exit status: 0".to_owned(),
                    stdout: b"qpdf version 12.0.0\n".to_vec(),
                    stderr: Vec::new(),
                },
            ),
            QpdfVersionDecision::Fail(
                "/bad/qpdf reported an unexpected qpdf version; expected qpdf version 11.9.0, stdout length=20 hex=71 70 64 66 20 76 65 72 73 69 6f 6e 20 31 32 2e 30 2e 30 0a text=\"qpdf version 12.0.0\\n\""
                    .to_owned()
            )
        );
    }

    #[test]
    fn qpdf_version_accepts_pinned_first_line_and_ignores_qpdf_footer() {
        assert_eq!(
            classify_qpdf_version(
                Path::new("/usr/bin/qpdf"),
                false,
                QpdfVersionProbe::Output {
                    status_success: true,
                    status: "exit status: 0".to_owned(),
                    stdout: b"qpdf version 11.9.0\nRun qpdf --copyright for details.\n".to_vec(),
                    stderr: Vec::new(),
                },
            ),
            QpdfVersionDecision::Select
        );
    }

    #[test]
    fn qpdf_resolution_falls_back_after_not_found_and_selects_next_candidate() {
        let candidates = vec![PathBuf::from("/usr/bin/qpdf"), PathBuf::from("qpdf")];
        let mut probes = vec![
            QpdfVersionProbe::NotFound,
            QpdfVersionProbe::Output {
                status_success: true,
                status: "exit status: 0".to_owned(),
                stdout: b"qpdf version 11.9.0\n".to_vec(),
                stderr: Vec::new(),
            },
        ]
        .into_iter();
        assert_eq!(
            resolve_qpdf_candidates(&candidates, |_| probes.next().expect("probe fixture")),
            QpdfResolution::Selected(PathBuf::from("qpdf"))
        );
    }

    #[test]
    fn qpdf_resolution_returns_skip_for_final_not_found() {
        let candidates = vec![PathBuf::from("qpdf")];
        assert_eq!(
            resolve_qpdf_candidates(&candidates, |_| QpdfVersionProbe::NotFound),
            QpdfResolution::Skip(
                "qpdf unavailable; skipping DCT differential for qpdf version 11.9.0".to_owned()
            )
        );
    }

    #[test]
    fn qpdf_resolution_returns_failure_without_fallback() {
        let candidates = vec![PathBuf::from("qpdf"), PathBuf::from("other-qpdf")];
        assert_eq!(
            resolve_qpdf_candidates(&candidates, |_| {
                QpdfVersionProbe::LaunchError("permission denied".to_owned())
            }),
            QpdfResolution::Fail(
                "failed to invoke qpdf --version for qpdf version 11.9.0: permission denied"
                    .to_owned()
            )
        );
    }

    #[test]
    fn qpdf_resolution_reports_empty_candidate_list() {
        assert_eq!(
            resolve_qpdf_candidates(&[], |_| QpdfVersionProbe::NotFound),
            QpdfResolution::NoCandidate
        );
    }

    #[test]
    fn qpdf_version_probe_reports_missing_binary_without_fixture_side_effects() {
        assert_eq!(
            qpdf_version_probe(Path::new(
                "/this/path/does/not/exist/flpdf-qpdf-missing-binary",
            )),
            QpdfVersionProbe::NotFound
        );
    }

    #[test]
    fn qpdf_version_probe_reports_command_input_failure() {
        let probe = qpdf_version_probe(Path::new("qpdf\0"));
        assert!(matches!(probe, QpdfVersionProbe::LaunchError(message) if !message.is_empty()));
    }

    #[test]
    fn qpdf_resolution_selected_returns_the_binary_path() {
        assert_eq!(
            qpdf_resolution_to_option(QpdfResolution::Selected(PathBuf::from("qpdf"))),
            Some(PathBuf::from("qpdf"))
        );
    }

    #[test]
    fn qpdf_resolution_skip_returns_none_after_diagnostic() {
        assert_eq!(
            qpdf_resolution_to_option(QpdfResolution::Skip("missing qpdf".to_owned())),
            None
        );
    }

    #[test]
    #[should_panic(expected = "version mismatch")]
    fn qpdf_resolution_failure_panics_after_diagnostic() {
        qpdf_resolution_to_option(QpdfResolution::Fail("version mismatch".to_owned()));
    }

    #[test]
    #[should_panic(expected = "candidate list must not be empty")]
    fn qpdf_resolution_empty_candidate_result_panics() {
        qpdf_resolution_to_option(QpdfResolution::NoCandidate);
    }

    const FLPDF_QPDF_BIN: &str = "FLPDF_QPDF_BIN";
    const PINNED_QPDF_BINARY: &str = "/usr/bin/qpdf";
    const PINNED_QPDF_VERSION: &str = "qpdf version 11.9.0";

    #[derive(Debug, PartialEq, Eq)]
    enum QpdfVersionProbe {
        NotFound,
        LaunchError(String),
        Output {
            status_success: bool,
            status: String,
            stdout: Vec<u8>,
            stderr: Vec<u8>,
        },
    }

    #[derive(Debug, PartialEq, Eq)]
    enum QpdfVersionDecision {
        TryNext,
        Skip(String),
        Fail(String),
        Select,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum QpdfResolution {
        Selected(PathBuf),
        Skip(String),
        Fail(String),
        NoCandidate,
    }

    fn qpdf_candidates_for(override_path: Option<&Path>) -> Vec<PathBuf> {
        if let Some(path) = override_path {
            if !path.as_os_str().is_empty() {
                return vec![path.to_path_buf()];
            }
        }

        #[cfg(target_os = "linux")]
        let candidates = vec![PathBuf::from(PINNED_QPDF_BINARY), PathBuf::from("qpdf")];
        #[cfg(not(target_os = "linux"))]
        let candidates = vec![PathBuf::from("qpdf")];
        candidates
    }

    fn qpdf_candidates() -> Vec<PathBuf> {
        let override_path = env::var_os(FLPDF_QPDF_BIN);
        qpdf_candidates_for(override_path.as_deref().map(Path::new))
    }

    fn classify_qpdf_version(
        candidate: &Path,
        has_next_candidate: bool,
        probe: QpdfVersionProbe,
    ) -> QpdfVersionDecision {
        let candidate_display = candidate.display().to_string();
        match probe {
            QpdfVersionProbe::NotFound => {
                if has_next_candidate {
                    QpdfVersionDecision::TryNext
                } else {
                    QpdfVersionDecision::Skip(format!(
                        "{candidate_display} unavailable; skipping DCT differential for {PINNED_QPDF_VERSION}"
                    ))
                }
            }
            QpdfVersionProbe::LaunchError(error) => QpdfVersionDecision::Fail(format!(
                "failed to invoke {candidate_display} --version for {PINNED_QPDF_VERSION}: {error}"
            )),
            QpdfVersionProbe::Output {
                status_success,
                status,
                stdout,
                stderr,
            } => {
                if !status_success {
                    return QpdfVersionDecision::Fail(format!(
                        "{candidate_display} --version failed while checking {PINNED_QPDF_VERSION}: status={status}\nstdout length={} hex={}\nstderr length={} hex={} text={:?}",
                        stdout.len(),
                        hex_bytes(&stdout),
                        stderr.len(),
                        hex_bytes(&stderr),
                        String::from_utf8_lossy(&stderr),
                    ));
                }
                if !stderr.is_empty() {
                    return QpdfVersionDecision::Fail(format!(
                        "{candidate_display} --version wrote stderr while checking {PINNED_QPDF_VERSION}: length={} hex={} text={:?}",
                        stderr.len(),
                        hex_bytes(&stderr),
                        String::from_utf8_lossy(&stderr),
                    ));
                }
                let version_stdout = String::from_utf8_lossy(&stdout);
                let first_line = version_stdout.lines().next().unwrap_or_default();
                if first_line != PINNED_QPDF_VERSION {
                    return QpdfVersionDecision::Fail(format!(
                        "{candidate_display} reported an unexpected qpdf version; expected {PINNED_QPDF_VERSION}, stdout length={} hex={} text={version_stdout:?}",
                        stdout.len(),
                        hex_bytes(&stdout),
                    ));
                }
                QpdfVersionDecision::Select
            }
        }
    }

    fn resolve_qpdf_candidates<Probe>(candidates: &[PathBuf], mut probe: Probe) -> QpdfResolution
    where
        Probe: FnMut(&Path) -> QpdfVersionProbe,
    {
        for (index, candidate) in candidates.iter().enumerate() {
            match classify_qpdf_version(candidate, index + 1 < candidates.len(), probe(candidate)) {
                QpdfVersionDecision::TryNext => continue,
                QpdfVersionDecision::Skip(message) => return QpdfResolution::Skip(message),
                QpdfVersionDecision::Fail(message) => return QpdfResolution::Fail(message),
                QpdfVersionDecision::Select => {
                    return QpdfResolution::Selected(candidate.clone());
                }
            }
        }
        QpdfResolution::NoCandidate
    }

    fn qpdf_version_probe(candidate: &Path) -> QpdfVersionProbe {
        match Command::new(candidate).arg("--version").output() {
            Ok(version) => QpdfVersionProbe::Output {
                status_success: version.status.success(),
                status: format!("{:?}", version.status),
                stdout: version.stdout,
                stderr: version.stderr,
            },
            Err(error) if error.kind() == ErrorKind::NotFound => QpdfVersionProbe::NotFound,
            Err(error) => QpdfVersionProbe::LaunchError(error.to_string()),
        }
    }

    fn qpdf_resolution_to_option(resolution: QpdfResolution) -> Option<PathBuf> {
        match resolution {
            QpdfResolution::Selected(candidate) => Some(candidate),
            QpdfResolution::Skip(message) => {
                eprintln!("{message}");
                None
            }
            QpdfResolution::Fail(message) => panic!("{message}"),
            QpdfResolution::NoCandidate => {
                unreachable!("candidate list must not be empty")
            }
        }
    }

    fn pinned_qpdf_11_9_0() -> Option<PathBuf> {
        let candidates = qpdf_candidates();
        qpdf_resolution_to_option(resolve_qpdf_candidates(&candidates, qpdf_version_probe))
    }

    #[cfg(feature = "qpdf-libjpeg-compat")]
    fn test_late_truncated_jpeg() -> Vec<u8> {
        let pixels: Vec<u8> = (0..(16 * 16 * 3))
            .map(|value| (value % 256) as u8)
            .collect();
        libjpeg_turbo_rs::compress(
            &pixels,
            16,
            16,
            libjpeg_turbo_rs::PixelFormat::Rgb,
            75,
            libjpeg_turbo_rs::Subsampling::S444,
        )
        .expect("late-truncation test JPEG must encode")
    }

    fn test_grayscale_jpeg() -> Vec<u8> {
        libjpeg_turbo_rs::compress(
            &[64u8, 192],
            2,
            1,
            libjpeg_turbo_rs::PixelFormat::Grayscale,
            75,
            libjpeg_turbo_rs::Subsampling::S444,
        )
        .expect("grayscale test JPEG must encode")
    }

    fn test_cmyk_jpeg() -> Vec<u8> {
        libjpeg_turbo_rs::compress(
            &[0u8, 64, 128, 255],
            1,
            1,
            libjpeg_turbo_rs::PixelFormat::Cmyk,
            75,
            libjpeg_turbo_rs::Subsampling::S444,
        )
        .expect("CMYK test JPEG must encode")
    }

    fn test_12_bit_jpeg() -> Vec<u8> {
        libjpeg_turbo_rs::compress_12bit(
            &[2048i16],
            1,
            1,
            1,
            75,
            libjpeg_turbo_rs::Subsampling::S444,
        )
        .expect("12-bit test JPEG must encode")
    }

    fn test_unknown_component_jpeg() -> Vec<u8> {
        let mut jpeg = libjpeg_turbo_rs::compress(
            &[128u8],
            1,
            1,
            libjpeg_turbo_rs::PixelFormat::Grayscale,
            75,
            libjpeg_turbo_rs::Subsampling::S444,
        )
        .expect("unknown-component test JPEG must encode");
        let sof = jpeg
            .windows(2)
            .position(|marker| marker == [0xff, 0xc0])
            .expect("baseline JPEG must contain SOF0");
        let segment_length = u16::from_be_bytes([jpeg[sof + 2], jpeg[sof + 3]]);
        assert_eq!(segment_length, 11);
        jpeg[sof + 9] = 2;
        jpeg[sof + 2..sof + 4].copy_from_slice(&(segment_length + 3).to_be_bytes());
        let second_component = sof + 2 + usize::from(segment_length);
        jpeg.splice(second_component..second_component, [2, 0x11, 0]);
        jpeg
    }

    #[derive(Default)]
    struct DctSink {
        writes: Vec<Vec<u8>>,
        write_attempts: usize,
        finishes: usize,
        finish_attempts: usize,
        fail_write: bool,
        fail_finish: bool,
        #[cfg(feature = "qpdf-libjpeg-compat")]
        panic_write: bool,
    }

    impl Pipeline for DctSink {
        fn identifier(&self) -> &str {
            "dct test sink"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.write_attempts += 1;
            #[cfg(feature = "qpdf-libjpeg-compat")]
            if self.panic_write {
                panic!("dct test downstream panic");
            }
            if self.fail_write {
                Err(PipelineError::runtime("dct test write failure"))
            } else {
                self.writes.push(data.to_vec());
                Ok(())
            }
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.finish_attempts += 1;
            if self.fail_finish {
                Err(PipelineError::runtime("dct test finish failure"))
            } else {
                self.finishes += 1;
                Ok(())
            }
        }
    }

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
    fn unknown_filter_is_rejected_before_decode_parms_mismatch_by_each_shape_reader() {
        let filter = Object::Array(vec![
            Object::Name(b"BogusDecode".to_vec()),
            Object::Name(b"FlateDecode".to_vec()),
        ]);
        let params = Object::Array(vec![Object::Null]);
        let expected = "unsupported PDF feature: unsupported stream filter: BogusDecode";

        assert_eq!(
            decode_filter_specs_from_object(Some(&filter), Some(&params), None)
                .unwrap_err()
                .to_string(),
            expected
        );

        assert_eq!(
            decode_filter_specs_from_handle(
                &handle_from_object(Some(&filter)),
                &handle_from_object(Some(&params)),
                None,
            )
            .unwrap_err()
            .to_string(),
            expected
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

    /// Each value drops to its bounded [`ParamValue`], and a null-valued key
    /// drops entirely whatever its name.
    ///
    /// `/Whatever` carries the same `Object::Null` as the `/Predictor` above
    /// it. Under this `/Crypt` filter both are dropped for the same reason —
    /// qpdf's `getKeys` (`libqpdf/QPDF_Dictionary.cc:118-127`) never reports a
    /// null-valued entry, so neither reaches retention — which is what makes
    /// the pair worth asserting here: a `Crypt` stage keeps every *reported*
    /// key ([`retains_decode_param_key`]), so `/Whatever`'s absence can only be
    /// the null omission. The public decode/encode behavior for this
    /// null-valued row is asserted by
    /// `filters::tests::null_decode_params_values_are_omitted_before_decode_and_encode`
    /// and by the corpus's "null-valued /DecodeParms key" row.
    ///
    /// **The filter is `/Crypt` so that `/Name` is kept, and kept with its
    /// bytes** ([`CRYPT_NAME_PAYLOAD_DECODE_PARAM_KEYS`]); under
    /// `/FlateDecode` the key is outside [`RETAINED_DECODE_PARAM_KEYS`] and
    /// drops, so this read would witness no `ParamValue::Name` at all. Which
    /// filter it is changes nothing else
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

    /// Exactly the keys a read keeps, spelled out — and that a `Crypt` stage's
    /// set is the whole dictionary where every other filter's is five keys.
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
    /// `Crypt` rule falsifiable from here. Measured on
    /// `cargo test -p flpdf --lib`, in each direction:
    ///
    /// - Narrowing [`retains_decode_param_key`] back to
    ///   `RETAINED_DECODE_PARAM_KEYS ∪ CRYPT_NAME_PAYLOAD_DECODE_PARAM_KEYS`,
    ///   so a `Crypt` stage no longer keeps its unknown keys, reddens the
    ///   `/Crypt` half — four tests, this one plus
    ///   [`a_crypt_stage_retains_every_key_so_unknown_ones_stay_visible`],
    ///   [`a_crypt_stage_grows_only_by_the_key_bytes_of_an_unknown_entry`] and
    ///   [`a_crypt_chain_holds_the_whole_key_set_once_per_stage`].
    /// - Widening it the other way — every stage keeping every key, so the
    ///   rule stops being `Crypt`-only — reddens the `/FlateDecode` half
    ///   instead, and only that half: five tests, this one plus
    ///   [`retained_decode_parameter_bytes_do_not_grow_with_the_source_dictionary`],
    ///   [`retained_decode_parameter_bytes_do_not_grow_with_a_name_valued_parameter`],
    ///   [`handle_reader_never_resolves_a_decode_parms_value_for_a_filter_that_ignores_them`]
    ///   and
    ///   [`handle_reader_resolves_an_unretained_decode_parms_value_for_a_filter_that_reads_them`].
    /// - Owning a name payload under *every* retained key — the half-fix that
    ///   leaves `/Predictor /<long name>` amplifying — reddens five:
    ///   [`a_crypt_stage_retains_every_key_so_unknown_ones_stay_visible`] and
    ///   [`a_crypt_stage_grows_only_by_the_key_bytes_of_an_unknown_entry`]
    ///   again, plus
    ///   [`retained_decode_parameter_bytes_do_not_grow_with_a_name_valued_parameter`],
    ///   [`a_non_resolving_read_classifies_direct_values_exactly_as_the_object_reader_does`]
    ///   and [`a_name_payload_is_kept_under_a_crypt_stage_and_only_there`].
    /// - Dropping `/Type` from [`CRYPT_NAME_PAYLOAD_DECODE_PARAM_KEYS`] reddens
    ///   two, [`a_crypt_stage_keeps_the_type_name_bytes`] and
    ///   [`a_name_payload_is_kept_under_a_crypt_stage_and_only_there`] — so the
    ///   two payload keys are independently droppable and each has its own
    ///   witness.
    ///
    /// No codec or encode-path test moved in any of the four.
    ///
    /// A fifth mutation belongs on this map but not in this test: dropping the
    /// `crypt_stage` gate from [`keeps_crypt_name_payload`] reddens no read at
    /// all, and is witnessed by
    /// [`a_name_payload_is_kept_under_a_crypt_stage_and_only_there`] instead.
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
                b"Unread".to_vec(),
            ]
        );
    }

    /// One direct `/Filter` + `/DecodeParms` pair as *both* shape readers
    /// produce it, which must be the same specs.
    ///
    /// Retention and classification are applied separately in
    /// [`decode_params_from_object`] and [`decode_params_from_consuming_handle`],
    /// so a rule stated for one reader alone would go unnoticed by an
    /// object-reader-only assertion. Both arguments are direct, which is what
    /// makes agreement the right expectation at all — the two readers diverge
    /// on an indirect child by design, as [`shape_corpus`] explains.
    fn specs_from_both_readers(filter: &Object, parms: &Object) -> Vec<FilterSpec> {
        let from_object = decode_filter_specs_from_object(Some(filter), Some(parms), None).unwrap();
        let from_handle = decode_filter_specs_from_handle(
            &handle_from_object(Some(filter)),
            &handle_from_object(Some(parms)),
            None,
        )
        .unwrap();
        assert_eq!(from_object, from_handle, "the two readers disagreed");
        from_object
    }

    /// [`specs_from_both_readers`] for a single-stage `/Filter`.
    fn decode_params_from_both_readers(filter: &[u8], parms: &Object) -> DecodeParams {
        specs_from_both_readers(&Object::Name(filter.to_vec()), parms)
            .swap_remove(0)
            .decode_params
    }

    /// A `Crypt` stage keeps every key, because its qpdf counterpart reads
    /// every key.
    ///
    /// `SF_Crypt::setDecodeParms` (`libqpdf/QPDF_Stream.cc:33-50`) walks
    /// `decode_parms.getKeys()` and sets `filterable = false` on the first key
    /// that is neither `/Type` nor `/Name`. The key *set* is what decides
    /// filterability there, so a key dropped during retention is a key the
    /// stage can no longer refuse: `/DecodeParms << /Foo … >>` would arrive as
    /// an empty entry set and be accepted where qpdf rejects.
    ///
    /// This is [`RETAINED_DECODE_PARAM_KEYS`]' own rule — keep what the
    /// consumer reads — applied to a consumer that reads all of them. It is
    /// not an exception carved out for `Crypt`.
    ///
    /// **The unknown key arrives without a value payload.** `/Foo` carries a
    /// name and still reduces to [`ParamValue::Other`], exactly as a name
    /// under a retained-but-unread key does everywhere else, so widening the
    /// key set costs key bytes and nothing more;
    /// [`a_crypt_stage_grows_only_by_the_key_bytes_of_an_unknown_entry`] is
    /// the byte-level statement of that.
    #[test]
    fn a_crypt_stage_retains_every_key_so_unknown_ones_stay_visible() {
        let params = decode_params_from_both_readers(
            b"Crypt",
            &params(&[
                ("Foo", Object::Name(b"Identity".to_vec())),
                ("Name", Object::Name(b"Identity".to_vec())),
            ]),
        );

        assert_eq!(
            params.entries(),
            [
                (b"Foo".to_vec(), ParamValue::Other),
                (b"Name".to_vec(), ParamValue::Name(b"Identity".to_vec())),
            ]
        );
    }

    /// `/Type` reaches a `Crypt` stage with its name bytes, because
    /// `isDictionaryOfType` compares them.
    ///
    /// `SF_Crypt::setDecodeParms` accepts a `/Type`-bearing dictionary only
    /// when `decode_parms.isDictionaryOfType("/CryptFilterDecodeParms")`
    /// (`libqpdf/QPDF_Stream.cc:41-43`), which reads the `/Type` value's name.
    /// [`ParamValue::Other`] would make that test irreproducible: it cannot be
    /// told apart from `/Type /Foo`, which qpdf refuses.
    ///
    /// `/Type` is therefore the second key whose name payload some consumer
    /// reads, alongside `/Name` — and, like `/Name`, only under `Crypt`.
    #[test]
    fn a_crypt_stage_keeps_the_type_name_bytes() {
        let params = decode_params_from_both_readers(
            b"Crypt",
            &params(&[("Type", Object::Name(b"CryptFilterDecodeParms".to_vec()))]),
        );

        assert_eq!(
            params.entries(),
            [(
                b"Type".to_vec(),
                ParamValue::Name(b"CryptFilterDecodeParms".to_vec())
            )]
        );
    }

    /// The `crypt_stage` gate on [`keeps_crypt_name_payload`], asserted
    /// directly because no read can reach it.
    ///
    /// **Predicate-level rather than end-to-end, because no read can reach the
    /// gate.** It is consulted only for keys [`retains_decode_param_key`]
    /// already kept, and under a non-`Crypt` filter those are
    /// [`RETAINED_DECODE_PARAM_KEYS`], disjoint from the payload array — so
    /// removing the gate changes no read's answer, measured. These assertions
    /// are the only thing that turns red.
    ///
    /// The keys are spelled as literals rather than read from
    /// [`CRYPT_NAME_PAYLOAD_DECODE_PARAM_KEYS`], for the reason
    /// [`the_object_reader_keeps_exactly_the_read_decode_parameter_keys`]
    /// gives: reading the constant on both sides would shrink expectation and
    /// result together.
    #[test]
    fn a_name_payload_is_kept_under_a_crypt_stage_and_only_there() {
        assert!(keeps_crypt_name_payload(b"Name", true));
        assert!(keeps_crypt_name_payload(b"Type", true));

        assert!(!keeps_crypt_name_payload(b"Name", false));
        assert!(!keeps_crypt_name_payload(b"Type", false));

        // Not every key, even under `Crypt`: retention keeps `/Foo`, the
        // payload rule does not.
        assert!(!keeps_crypt_name_payload(b"Foo", true));
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

    /// The cost [`RETAINED_DECODE_PARAM_KEYS`] exists to bound, **under every
    /// filter but `Crypt`**.
    ///
    /// The scope is not an escape hatch. A `Crypt` stage keeps every key
    /// ([`retains_decode_param_key`]) because `SF_Crypt::setDecodeParms`
    /// (`libqpdf/QPDF_Stream.cc:33-50`) refuses on any key outside `/Type` and
    /// `/Name`, so there the count of unread keys is precisely what decides
    /// filterability and cannot be dropped. Bounding it away would make flpdf
    /// accept streams qpdf rejects. What is still bounded there is the *value*
    /// side, which is
    /// [`a_crypt_stage_grows_only_by_the_key_bytes_of_an_unknown_entry`]. The
    /// chain below is `ASCIIHexDecode` for that reason, not incidentally.
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
            // `/Name` is kept only under `/Crypt`, so beneath the
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
    /// **Every retained key carries a name, not just `/Name`.** No rule on
    /// *which keys are kept* bounds this, because a name value fits any slot:
    /// `/Predictor /<one-mebibyte-name>` passes the geometry key test too.
    /// Measured against that half-fix — `/Name` restricted to `Crypt` but a
    /// name payload owned wherever it appears — this chain retained 16,777,584
    /// bytes. Filling only one slot would let the next forgotten slot pass.
    /// [`keeps_crypt_name_payload`] is what bounds it, which is why widening
    /// the *key* set for `Crypt` did not move any figure here.
    ///
    /// **All three shape readers are measured**, because the legacy `Object`
    /// route [`decode_params_from_object`], consuming handle route
    /// [`decode_params_from_consuming_handle`], and non-consuming handle route
    /// [`decode_params_from_entries`] apply retention and classification
    /// separately. The corpus's "/Name and /Type under a filter that reads no
    /// entry" row catches a rule applied to only one of these routes —
    /// measured, forcing the non-consuming handle route's retention gate to
    /// `true` reddens [`handle_reader_matches_object_reader_for_every_filter_shape`]
    /// naming exactly that row — but it is a *relative* gate and would stay
    /// green if the rule were dropped from all three routes together. This
    /// test is the absolute one.
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
                let retained =
                    retained_bytes(&specs_from_both_readers(&chain, &all_slots_named(name_len)));
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
        //
        // Every key here is one the geometry set already kept, so the *key*
        // widening a `Crypt` stage brings does not move this figure; what it
        // costs is measured by
        // `a_crypt_stage_grows_only_by_the_key_bytes_of_an_unknown_entry`.
        for name_len in [16, 1 << 20] {
            let crypt = specs_from_both_readers(
                &Object::Name(b"Crypt".to_vec()),
                &all_slots_named(name_len),
            );
            assert!(crypt[0]
                .decode_params
                .entries()
                .contains(&(b"Name".to_vec(), ParamValue::Name(vec![b'v'; name_len]))));
            // 5 bytes of filter name, 49 of geometry keys, `Name` itself, and
            // its payload — once.
            assert_eq!(retained_bytes(&crypt), 5 + KEYS + 4 + name_len);
        }
    }

    /// What a `Crypt` stage's widened key set actually costs: the *key* bytes
    /// of each unknown entry, and not one byte of its value.
    ///
    /// [`a_crypt_stage_retains_every_key_so_unknown_ones_stay_visible`] states
    /// the rule; this puts a number on it, because "no payload" is the half
    /// that a bound depends on and that a `ParamValue` assertion at one small
    /// size would not catch. The unknown key's value is a name of one mebibyte
    /// in the second round, so a reader that owned payloads under every key
    /// would miss the expected figure by a factor of 16,000.
    ///
    /// The unknown key is itself long (64 bytes) so that the key half is not
    /// swamped by the constants around it: dropping unknown keys entirely —
    /// the pre-widening behavior — misses the figure too.
    ///
    /// **`Crypt` is where the growth is authorized**, so this is the
    /// counterpart of
    /// [`retained_decode_parameter_bytes_do_not_grow_with_the_source_dictionary`],
    /// which pins the *absence* of any such growth under every other filter.
    #[test]
    fn a_crypt_stage_grows_only_by_the_key_bytes_of_an_unknown_entry() {
        const UNKNOWN_KEY_LEN: usize = 64;

        for name_len in [16, 1 << 20] {
            let mut dictionary = Dictionary::new();
            dictionary.insert("Name", Object::Name(vec![b'v'; name_len]));
            dictionary.insert(
                format!("Unread{:058}", 0),
                Object::Name(vec![b'v'; name_len]),
            );
            let specs = specs_from_both_readers(
                &Object::Name(b"Crypt".to_vec()),
                &Object::Dictionary(dictionary),
            );

            // 5 bytes of filter name, `Name` plus its payload, and the unknown
            // key's 64 bytes of key text — its mebibyte value is not there.
            assert_eq!(
                retained_bytes(&specs),
                5 + 4 + name_len + UNKNOWN_KEY_LEN,
                "at name length {name_len}"
            );
        }
    }

    /// The cell the two byte tests above leave uncrossed: a `Crypt` stage's
    /// widened key set, replicated across a chain.
    ///
    /// [`retained_decode_parameter_bytes_do_not_grow_with_the_source_dictionary`]
    /// runs a 16-stage chain but deliberately under `ASCIIHexDecode`, and
    /// [`a_crypt_stage_grows_only_by_the_key_bytes_of_an_unknown_entry`] is a
    /// single `Crypt` stage. Neither measures the product, which is where the
    /// cost lives: one scalar `/DecodeParms` shared by an *n*-stage `/Crypt`
    /// chain is converted and stored once per stage, so the retained total is
    /// *n* × the source's whole key text while the source itself grew by only
    /// the *n* filter names.
    ///
    /// **This pins a cost, not a bound**, which is why it asserts an equality
    /// at two chain lengths rather than a ceiling. The growth is required:
    /// `SF_Crypt::setDecodeParms` (`libqpdf/QPDF_Stream.cc:33-50`) decides
    /// filterability from the key set, so a stage that dropped keys to stay
    /// small would accept streams qpdf rejects. The assertion therefore states
    /// what the figure *is*, and fails just as loudly if a later change makes
    /// it smaller. qpdf holds one `shared_ptr` at any chain length — see
    /// [`CRYPT_NAME_PAYLOAD_DECODE_PARAM_KEYS`]' per-stage residual note, which
    /// is the deviation this measures rather than a defect it guards.
    ///
    /// The unknown keys carry integers, so nothing here is payload: the figure
    /// is key text alone, multiplied by the chain.
    #[test]
    fn a_crypt_chain_holds_the_whole_key_set_once_per_stage() {
        const UNKNOWN_KEYS: usize = 8;
        const UNKNOWN_KEY_LEN: usize = 64;
        // Per stage: 5 bytes of filter name, `Name` (4) with its 8-byte
        // payload, and eight 64-byte unknown keys carrying nothing — 529.
        const PER_STAGE: usize = 5 + 4 + 8 + UNKNOWN_KEYS * UNKNOWN_KEY_LEN;

        let mut dictionary = Dictionary::new();
        dictionary.insert("Name", Object::Name(b"Identity".to_vec()));
        for index in 0..UNKNOWN_KEYS {
            dictionary.insert(format!("Unread{index:058}"), Object::Integer(1));
        }
        let parms = Object::Dictionary(dictionary);

        for chain in [1, 16] {
            let filter = Object::Array(vec![Object::Name(b"Crypt".to_vec()); chain]);
            let retained = retained_bytes(&specs_from_both_readers(&filter, &parms));

            assert_eq!(
                retained,
                chain * PER_STAGE,
                "retained {retained} bytes across a {chain}-stage /Crypt chain"
            );
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
            // The two shapes `SF_Crypt::setDecodeParms` decides on that no
            // other filter's `setDecodeParms` looks at: a key outside
            // `/Type`/`/Name`, which it refuses, and a `/Type` whose name it
            // compares. Both are kept by `retains_decode_param_key`'s `Crypt`
            // arm, and only `/Type` carries its name bytes out of the second.
            (
                "Crypt filter with a key it must refuse",
                Some(Object::Name(b"Crypt".to_vec())),
                Some(params(&[("Foo", Object::Name(b"Identity".to_vec()))])),
            ),
            (
                "Crypt filter with /Type and /Name",
                Some(Object::Name(b"Crypt".to_vec())),
                Some(params(&[
                    ("Name", Object::Name(b"Identity".to_vec())),
                    ("Type", Object::Name(b"CryptFilterDecodeParms".to_vec())),
                ])),
            ),
            // The same `/DecodeParms` under a filter that is not `Crypt`.
            // The `Crypt` arm of `retains_decode_param_key` makes the two rows
            // differ — this one retains nothing — and nothing else in this
            // corpus pairs a `/Name` with a non-`Crypt` filter, so without it a
            // retention rule applied to only one reader would go unnoticed
            // here. Like every row, it is a *relative* gate: it reddens when
            // one reader is changed and not the other, and stays green when
            // neither is. The absolute statements live in
            // `a_crypt_stage_retains_every_key_so_unknown_ones_stay_visible`
            // and `a_crypt_stage_keeps_the_type_name_bytes`, which assert
            // through both readers.
            (
                "/Name under a filter that is not Crypt",
                Some(flate()),
                Some(params(&[("Name", Object::Name(b"Identity".to_vec()))])),
            ),
            // The counterpart for `/Type`, which joined `/Name` as a
            // name-payload key: under a non-`Crypt` filter it is dropped.
            (
                "/Type under a filter that is not Crypt",
                Some(flate()),
                Some(params(&[(
                    "Type",
                    Object::Name(b"CryptFilterDecodeParms".to_vec()),
                )])),
            ),
            // A shape `filterable` genuinely distinguishes: `SF_ASCII85Decode`
            // inherits the base `setDecodeParms`, `return decode_parms.isNull()`
            // (`libqpdf/QPDFStreamFilter.cc:3-7`), so a present non-null
            // `/DecodeParms` makes the stage unfilterable whatever its keys
            // are — the opposite of the `/Crypt` rows above, where the keys
            // decide. It is also the only way into `decode_params_from_entries`,
            // the third retention site, which every other row here reaches
            // past. Measured: forcing that route's `crypt_stage` to `true`
            // reddens `handle_reader_matches_object_reader_for_every_filter_shape`
            // naming this row.
            (
                "/Name and /Type under a filter that reads no entry",
                Some(ascii85()),
                Some(params(&[
                    ("Name", Object::Name(b"Identity".to_vec())),
                    ("Type", Object::Name(b"CryptFilterDecodeParms".to_vec())),
                ])),
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
            // qpdf's parser stamps the document context on a direct scalar
            // child. Keep that context for the one row whose consuming
            // `getKeys()` call emits a recoverable type warning; all other
            // direct rows are unchanged shape comparisons.
            let (parms_handle, _parms_resolver) = if label == "present non-dictionary /DecodeParms"
            {
                let (handle, resolver) = handle_resolving(ObjectValue::Integer(1));
                (handle, Some(resolver))
            } else {
                (handle_from_object(parms.as_ref()), None)
            };
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
        // (`libqpdf/QPDFStreamFilter.cc:3-7`) rejects on. The shape reader
        // keeps the present scalar, and its consuming `getKeys()` call emits
        // qpdf's recoverable type warning before returning an empty key set.
        assert_eq!(
            decode_filter_specs_from_handle(&ObjectHandle::integer(1), &ObjectHandle::null(), None)
                .unwrap_err()
                .to_string(),
            "unsupported PDF feature: stream filter type is not name or array"
        );

        let (decode_params, _resolver) = handle_resolving(ObjectValue::Integer(1));
        let specs = decode_filter_specs_from_handle(
            &ObjectHandle::name(b"FlateDecode".to_vec()),
            &decode_params,
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
        // here and cannot be: it carries a payload only under a `/Crypt`
        // stage's `/Name` or `/Type` (`keeps_crypt_name_payload`), and neither
        // filter below is `/Crypt`.
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
            assert_eq!(normalize_filter_name(abbreviation), expected);
            if stream_filter_for(expected).is_some() {
                let filter = Object::Name(abbreviation.to_vec());
                let specs = decode_filter_specs_from_object(Some(&filter), None, None).unwrap();
                assert_eq!(specs[0].normalized_name(), expected);
            }
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

    #[test]
    fn dct_factory_is_registered_and_classified() {
        let mut filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");

        assert!(filter.set_decode_params(&DecodeParams::Absent));
        assert!(filter.is_specialized_compression());
        assert!(filter.is_lossy_compression());

        let mut sink = DctSink::default();
        let stage = filter
            .decode_pipeline(&mut sink)
            .expect("DCT stage construction must succeed")
            .expect("DCT filter must contribute a decode stage");
        assert_eq!(stage.identifier(), "DCT decode");
    }

    #[test]
    fn dct_factory_accepts_only_absent_decode_params() {
        let mut absent = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(absent.set_decode_params(&DecodeParams::Absent));
        let mut sink = DctSink::default();
        assert!(absent
            .decode_pipeline(&mut sink)
            .expect("DCT stage construction must succeed")
            .is_some());

        let mut present = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(!present.set_decode_params(&DecodeParams::Present(Vec::new())));
    }

    #[test]
    fn dct_stage_decodes_chunked_input_one_scanline_per_write() {
        let jpeg = test_jpeg();
        let mut filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(filter.set_decode_params(&DecodeParams::Absent));
        let mut sink = DctSink::default();

        {
            let mut stage = filter
                .decode_pipeline(&mut sink)
                .expect("DCT stage construction must succeed")
                .expect("DCT filter must contribute a decode stage");
            for chunk in jpeg.chunks(5) {
                stage.write(chunk).expect("chunked JPEG write must succeed");
            }
            stage.finish().expect("JPEG finish must succeed");
        }

        assert_eq!(sink.writes.len(), 2, "one downstream write per scanline");
        assert!(sink.writes.iter().all(|write| write.len() == 6));
        assert_eq!(
            sink.writes,
            vec![
                vec![0, 52, 132, 113, 99, 90],
                vec![210, 196, 187, 255, 139, 34],
            ]
        );
        assert_eq!(sink.finishes, 1);
    }

    #[test]
    fn dct_whole_buffer_route_decodes_and_honors_output_limit() {
        let jpeg = test_jpeg();
        let mut filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(filter.set_decode_params(&DecodeParams::Absent));

        let decoded = filter
            .pipe_decode_recovering(&jpeg, None, &mut ignore_warning)
            .expect("whole-buffer DCT route must construct");
        assert!(decoded.error.is_none());
        let canonical = canonical_dct_bytes(&jpeg);
        assert_eq!(decoded.data, canonical);
        assert_eq!(decoded.cleanup_data_start, 0);

        // Exactly the full decoded size must still succeed: the guard below
        // rejects only output that would *exceed* the cap, matching every
        // other filter's `OutputBuffer` enforcement (`data.len() > remaining`).
        let mut exact = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(exact.set_decode_params(&DecodeParams::Absent));
        let exact = exact
            .pipe_decode_recovering(&jpeg, Some(canonical.len()), &mut ignore_warning)
            .expect("DCT route at the exact output size must construct");
        assert!(exact.error.is_none());
        assert_eq!(exact.data, canonical);

        // One byte under the full decoded size must trip flpdf's decode-bomb
        // guard.
        let mut limited = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(limited.set_decode_params(&DecodeParams::Absent));
        let limited = limited
            .pipe_decode_recovering(&jpeg, Some(canonical.len() - 1), &mut ignore_warning)
            .expect("limited whole-buffer DCT route must construct");
        let error = limited.error.expect("DCT output limit must be reported");
        assert!(!error.during_write);
        assert!(error.error.to_string().contains(DECODE_OUTPUT_LIMIT_PREFIX));

        // The two backends enforce the cap at different points, and this is
        // the deliberate divergence Finding 1 introduces, not an oversight:
        #[cfg(not(feature = "qpdf-libjpeg-compat"))]
        {
            // The default backend rejects in `PlDct::finish`
            // (`crates/flpdf/src/pipeline/dct.rs`) *before* decoding any
            // pixels: the declared header dimensions already exceed the
            // cap, so `ScanlineDecoder::read_scanline` (whose first call
            // eagerly decodes the whole image on this backend) is never
            // reached and no bytes reach the downstream sink.
            assert_eq!(error.output_offset, 0);
            assert_eq!(limited.data.len(), 0);
            assert_eq!(limited.cleanup_data_start, 0);
        }
        #[cfg(feature = "qpdf-libjpeg-compat")]
        {
            // Real libjpeg decodes one scanline at a time
            // (`flpdf-libjpeg-compat/csrc/jpeg_compat.c`'s
            // `jpeg_read_scanlines` loop), so the sink's ordinary
            // per-write cap enforcement already sees each row as it's
            // produced and fills up to (not past) the limit before
            // erroring, like every other filter's `OutputBuffer`.
            assert_eq!(error.output_offset, canonical.len() - 1);
            assert_eq!(limited.data.len(), canonical.len() - 1);
            assert_eq!(limited.cleanup_data_start, 0);
        }
    }

    /// The oracle is qpdf 11.9.0. On Linux the resolver prefers the pinned
    /// `/usr/bin/qpdf`, otherwise it uses PATH `qpdf`; `FLPDF_QPDF_BIN` is an
    /// explicit override. Every selected executable is checked with
    /// `--version` before the real-PDF probe, and only an absent candidate skips
    /// this test. qpdf 11.9.0 consumes `/DCTDecode` at `decode-level=all`:
    /// `SF_DCTDecode` (`SF_DCTDecode.hh:8-40`) constructs `Pl_DCT`, whose
    /// `decompress` path (`Pl_DCT.cc:298-326`) writes libjpeg scanline bytes to
    /// the next pipeline. The real-PDF probe below pins that source/behavior
    /// boundary against the canonical Rust stage.
    #[test]
    fn dct_qpdf_filtered_stream_data_matches_decode_pipeline_exactly() {
        let qpdf = match pinned_qpdf_11_9_0() {
            Some(qpdf) => qpdf,
            None => return, // cov:ignore: supported oracle tests require the pinned qpdf 11.9.0 executable; absence only skips the external differential check
        };
        let qpdf_display = qpdf.display().to_string();
        eprintln!("DCT qpdf differential using {qpdf_display} ({PINNED_QPDF_VERSION})");
        let jpeg = test_jpeg();
        let directory = dct_qpdf_fixture(&jpeg);
        let fixture = directory.path().join("dct-image.pdf");
        let check = Command::new(&qpdf)
            .arg("--check")
            .arg(&fixture)
            .output()
            // cov:ignore-start: external pinned qpdf launch failure is unobservable in the successful oracle test; the diagnostic is retained.
            .unwrap_or_else(|error| {
                panic!("failed to invoke {qpdf_display} --check for {PINNED_QPDF_VERSION}: {error}")
            })
            // cov:ignore-end
            ;
        let check_stdout_hex = hex_bytes(&check.stdout);
        let check_stderr_hex = hex_bytes(&check.stderr);
        // cov:ignore-start: external pinned qpdf failure diagnostics are unobservable in the successful oracle test; the exact status assertion is retained.
        assert!(
            check.status.success(),
            "qpdf --check failed for {qpdf_display} {PINNED_QPDF_VERSION}: status={:?}\nstdout length={} hex={}\nstderr length={} hex={} text={:?}",
            check.status,
            check.stdout.len(),
            check_stdout_hex,
            check.stderr.len(),
            check_stderr_hex,
            String::from_utf8_lossy(&check.stderr),
        );
        // cov:ignore-end
        // cov:ignore-start: external pinned qpdf failure diagnostics are unobservable in the successful oracle test; the exact stderr assertion is retained.
        assert!(
            check.stderr.is_empty(),
            "qpdf --check wrote stderr for {qpdf_display} {PINNED_QPDF_VERSION}: length={} hex={} text={:?}",
            check.stderr.len(),
            check_stderr_hex,
            String::from_utf8_lossy(&check.stderr),
        );
        // cov:ignore-end
        let output = Command::new(&qpdf)
            .arg("--show-object=3")
            .arg("--filtered-stream-data")
            .arg(&fixture)
            .output()
            // cov:ignore-start: external pinned qpdf launch failure is unobservable in the successful oracle test; the diagnostic is retained.
            .unwrap_or_else(|error| {
                panic!(
                    "failed to invoke {qpdf_display} DCT differential for {PINNED_QPDF_VERSION}: {error}"
                )
            })
            // cov:ignore-end
            ;
        let canonical = canonical_dct_bytes(&jpeg);
        let qpdf_stdout_hex = hex_bytes(&output.stdout);
        let qpdf_stderr_hex = hex_bytes(&output.stderr);
        let canonical_hex = hex_bytes(&canonical);
        eprintln!(
            "DCT qpdf probe {qpdf_display} ({PINNED_QPDF_VERSION}): --check status={:?} stdout={} stderr={}; filtered status={:?} stdout={} stderr={}; canonical={}",
            check.status,
            check.stdout.len(),
            check.stderr.len(),
            output.status,
            output.stdout.len(),
            output.stderr.len(),
            canonical.len(),
        );

        // cov:ignore-start: external pinned qpdf failure formatting is unobservable when the supported oracle succeeds; retain the assertion and diagnostic.
        assert!(
            output.status.success(),
            "qpdf DCT differential failed for {qpdf_display} {PINNED_QPDF_VERSION}: status={:?}\nstdout length={} hex={}\nstderr length={} hex={} text={:?}",
            output.status,
            output.stdout.len(),
            qpdf_stdout_hex,
            output.stderr.len(),
            qpdf_stderr_hex,
            String::from_utf8_lossy(&output.stderr),
        );
        // cov:ignore-end
        // cov:ignore-start: external pinned qpdf stderr failure formatting is unobservable when the supported oracle is silent; retain the assertion and diagnostic.
        assert!(
            output.stderr.is_empty(),
            "qpdf DCT differential wrote stderr for {qpdf_display} {PINNED_QPDF_VERSION}: length={} hex={} text={:?}",
            output.stderr.len(),
            qpdf_stderr_hex,
            String::from_utf8_lossy(&output.stderr),
        );
        // cov:ignore-end
        // cov:ignore-start: an external qpdf output mismatch is the failure assertion itself; the supported qpdf 11.9.0 oracle exercises only the success path.
        assert_eq!(
            output.stdout,
            canonical,
            "qpdf DCT differential mismatch for {qpdf_display} {PINNED_QPDF_VERSION}\nqpdf stdout length={} hex={}\ncanonical DctSink length={} hex={}",
            output.stdout.len(),
            qpdf_stdout_hex,
            canonical.len(),
            canonical_hex,
        );
        // cov:ignore-end
    }

    #[test]
    fn dct_stage_empty_and_repeated_finish_forward_finish() {
        let mut empty_filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(empty_filter.set_decode_params(&DecodeParams::Absent));
        let mut empty_sink = DctSink::default();
        assert_eq!(empty_sink.identifier(), "dct test sink");
        {
            let mut stage = empty_filter
                .decode_pipeline(&mut empty_sink)
                .expect("DCT stage construction must succeed")
                .expect("DCT filter must contribute a decode stage");
            stage.finish().expect("empty JPEG finish must succeed");
        }
        assert!(empty_sink.writes.is_empty());
        assert_eq!(empty_sink.finishes, 1);
        assert_eq!(empty_sink.finish_attempts, 1);

        let mut empty_error_filter =
            stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(empty_error_filter.set_decode_params(&DecodeParams::Absent));
        let mut empty_error_sink = DctSink {
            fail_finish: true,
            ..DctSink::default()
        };
        let error = {
            let mut stage = empty_error_filter
                .decode_pipeline(&mut empty_error_sink)
                .expect("DCT stage construction must succeed")
                .expect("DCT filter must contribute a decode stage");
            stage
                .finish()
                .expect_err("empty finish failure must be returned")
        };
        assert_eq!(error.to_string(), "dct test finish failure");
        assert!(empty_error_sink.writes.is_empty());
        assert_eq!(empty_error_sink.finishes, 0);
        assert_eq!(empty_error_sink.finish_attempts, 1);

        let mut repeated_filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(repeated_filter.set_decode_params(&DecodeParams::Absent));
        let mut repeated_sink = DctSink::default();
        {
            let mut stage = repeated_filter
                .decode_pipeline(&mut repeated_sink)
                .expect("DCT stage construction must succeed")
                .expect("DCT filter must contribute a decode stage");
            stage.finish().expect("first finish must succeed");
            stage.finish().expect("repeated finish must succeed");
        }
        assert!(repeated_sink.writes.is_empty());
        assert_eq!(repeated_sink.finishes, 2);
        assert_eq!(repeated_sink.finish_attempts, 2);
    }

    #[test]
    fn dct_stage_preserves_codec_error_and_does_not_finish_downstream() {
        {
            let mut filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
            assert!(filter.set_decode_params(&DecodeParams::Absent));
            let mut sink = DctSink::default();
            let error = {
                let mut stage = filter
                    .decode_pipeline(&mut sink)
                    .expect("DCT stage construction must succeed")
                    .expect("DCT filter must contribute a decode stage");
                stage
                    .write(b"not a jpeg")
                    .expect("DCT stage buffers malformed input");
                stage
                    .finish()
                    .expect_err("malformed JPEG must fail at finish")
            };

            assert!(matches!(error, PipelineError::Runtime(_)));
            #[cfg(not(feature = "qpdf-libjpeg-compat"))]
            assert_eq!(
                error.to_string(),
                "DCT decode: Not a JPEG file: starts with 0x6e 0x6f"
            );
            #[cfg(feature = "qpdf-libjpeg-compat")]
            assert_eq!(
                error.to_string(),
                "DCT decode: Not a JPEG file: starts with 0x6e 0x6f"
            );
            assert!(sink.writes.is_empty());
            assert_eq!(sink.write_attempts, 0);
            assert_eq!(sink.finishes, 0);
            assert_eq!(sink.finish_attempts, 0);
        }

        {
            let jpeg = test_jpeg();
            let truncated = &jpeg[..jpeg.len() / 2];
            let mut filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
            assert!(filter.set_decode_params(&DecodeParams::Absent));
            let mut sink = DctSink::default();
            let error = {
                let mut stage = filter
                    .decode_pipeline(&mut sink)
                    .expect("DCT stage construction must succeed")
                    .expect("DCT filter must contribute a decode stage");
                stage
                    .write(truncated)
                    .expect("DCT stage buffers truncated input");
                stage
                    .finish()
                    .expect_err("truncated JPEG must fail at finish")
            };

            assert!(matches!(error, PipelineError::Runtime(_)));
            assert_eq!(
                error.to_string(),
                "DCT decode: invalid jpeg data reading from buffer"
            );
            assert!(sink.writes.is_empty());
            assert_eq!(sink.write_attempts, 0);
            assert_eq!(sink.finishes, 0);
            assert_eq!(sink.finish_attempts, 0);
        }

        {
            let mut filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
            assert!(filter.set_decode_params(&DecodeParams::Absent));
            let mut sink = DctSink::default();
            let error = {
                let mut stage = filter
                    .decode_pipeline(&mut sink)
                    .expect("DCT stage construction must succeed")
                    .expect("DCT filter must contribute a decode stage");
                stage
                    .write(b"x")
                    .expect("short malformed input must buffer");
                stage
                    .finish()
                    .expect_err("short malformed JPEG must fail at finish")
            };

            assert!(matches!(error, PipelineError::Runtime(_)));
            assert_eq!(
                error.to_string(),
                "DCT decode: invalid jpeg data reading from buffer"
            );
            assert!(sink.writes.is_empty());
            assert_eq!(sink.write_attempts, 0);
            assert_eq!(sink.finishes, 0);
            assert_eq!(sink.finish_attempts, 0);
        }
    }

    /// A corrupt JPEG that starts with a valid SOI marker (so the "Not a
    /// JPEG file" check does not fire) but fails for a reason other than
    /// running out of buffered bytes must fall through to the generic
    /// `{identifier}: {error}` diagnostic, not the `UnexpectedEof`-specific
    /// "invalid jpeg data reading from buffer" wording reserved for
    /// qpdf's whole-buffer `fill_buffer_input_buffer` over-read case.
    ///
    /// This still diverges from qpdf's own wording (`Unsupported marker
    /// type 0x02`, `/usr/include/jerror.h:132`): `libjpeg-turbo-rs` 0.8.0's
    /// `InvalidMarker` payload is structurally always `0`, not the reserved
    /// marker byte, and the crate accepts (skips) reserved markers that real
    /// libjpeg rejects immediately, so the byte the crate reports isn't the
    /// byte real libjpeg would report — a text remap here would render the
    /// wrong hex value. Confirmed genuine `libjpeg-turbo-rs` capability gap
    /// and deferred, not fixed, tracked as flpdf-69n1.
    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn dct_stage_preserves_non_eof_libjpeg_diagnostic() {
        // SOI, then a reserved/invalid marker (0xFF 0x02) with a well-formed
        // 4-byte segment length — enough bytes present that this is not an
        // end-of-data condition.
        let malformed: &[u8] = &[
            0xFF, 0xD8, 0xFF, 0x02, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let mut filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(filter.set_decode_params(&DecodeParams::Absent));
        let mut sink = DctSink::default();
        let error = {
            let mut stage = filter
                .decode_pipeline(&mut sink)
                .expect("DCT stage construction must succeed")
                .expect("DCT filter must contribute a decode stage");
            stage
                .write(malformed)
                .expect("malformed JPEG with valid SOI must buffer");
            stage.finish().expect_err("invalid marker must be rejected")
        };
        assert!(matches!(error, PipelineError::Runtime(_)));
        assert_eq!(error.to_string(), "DCT decode: invalid marker: 0xFF00");
        assert!(sink.writes.is_empty());
        assert_eq!(sink.finish_attempts, 0);
    }

    #[cfg(feature = "qpdf-libjpeg-compat")]
    #[test]
    fn dct_compat_rejects_late_truncation_after_scanline_output() {
        let jpeg = test_late_truncated_jpeg();
        assert_eq!(&jpeg[jpeg.len() - 2..], [0xff, 0xd9]);

        let mut filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(filter.set_decode_params(&DecodeParams::Absent));
        let mut sink = DctSink::default();
        let error = {
            let mut stage = filter
                .decode_pipeline(&mut sink)
                .expect("DCT stage construction must succeed")
                .expect("DCT filter must contribute a decode stage");
            stage
                .write(&jpeg[..jpeg.len() - 2])
                .expect("late-truncated JPEG must buffer");
            stage
                .finish()
                .expect_err("missing EOI must be a codec error")
        };

        // The C source must report EOF only after libjpeg has emitted its
        // already-decoded scanlines.
        assert!(!sink.writes.is_empty());
        assert!(matches!(error, PipelineError::Runtime(_)));
        assert_eq!(
            error.to_string(),
            "DCT decode: invalid jpeg data reading from buffer"
        );
        assert_eq!(sink.finishes, 0);
    }

    #[test]
    fn dct_stage_preserves_downstream_write_error() {
        let jpeg = test_jpeg();
        let mut filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(filter.set_decode_params(&DecodeParams::Absent));
        let mut sink = DctSink {
            fail_write: true,
            ..DctSink::default()
        };
        let error = {
            let mut stage = filter
                .decode_pipeline(&mut sink)
                .expect("DCT stage construction must succeed")
                .expect("DCT filter must contribute a decode stage");
            stage.write(&jpeg).expect("DCT stage buffers input");
            stage
                .finish()
                .expect_err("downstream write failure must be returned")
        };

        assert_eq!(error.to_string(), "dct test write failure");
        assert!(sink.writes.is_empty());
        assert_eq!(sink.write_attempts, 1);
        assert_eq!(sink.finishes, 0);
        assert_eq!(sink.finish_attempts, 0);
    }

    #[cfg(feature = "qpdf-libjpeg-compat")]
    #[test]
    fn dct_compat_contains_downstream_panic_at_callback_boundary() {
        let jpeg = test_jpeg();
        let mut filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(filter.set_decode_params(&DecodeParams::Absent));
        let mut sink = DctSink {
            panic_write: true,
            ..DctSink::default()
        };
        let error = {
            let mut stage = filter
                .decode_pipeline(&mut sink)
                .expect("DCT stage construction must succeed")
                .expect("DCT filter must contribute a decode stage");
            stage.write(&jpeg).expect("DCT stage buffers input");
            stage
                .finish()
                .expect_err("downstream panic must become a pipeline error")
        };

        assert_eq!(
            error.to_string(),
            "DCT decode: downstream pipeline panicked"
        );
        assert_eq!(sink.write_attempts, 1);
        assert_eq!(sink.finishes, 0);
        assert_eq!(sink.finish_attempts, 0);
    }

    #[test]
    fn dct_stage_preserves_downstream_finish_error() {
        let jpeg = test_jpeg();
        let mut filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(filter.set_decode_params(&DecodeParams::Absent));
        let mut sink = DctSink {
            fail_finish: true,
            ..DctSink::default()
        };
        let error = {
            let mut stage = filter
                .decode_pipeline(&mut sink)
                .expect("DCT stage construction must succeed")
                .expect("DCT filter must contribute a decode stage");
            stage.write(&jpeg).expect("JPEG write must succeed");
            stage
                .finish()
                .expect_err("downstream finish failure must be returned")
        };

        assert_eq!(error.to_string(), "dct test finish failure");
        assert_eq!(sink.writes.len(), 2);
        assert!(sink.writes.iter().all(|write| write.len() == 6));
        assert_eq!(sink.write_attempts, 2);
        assert_eq!(sink.finishes, 0);
        assert_eq!(sink.finish_attempts, 1);
    }

    #[test]
    fn dct_stage_uses_default_component_widths() {
        for (jpeg, expected_row_length) in [(test_grayscale_jpeg(), 2), (test_cmyk_jpeg(), 4)] {
            let mut filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
            assert!(filter.set_decode_params(&DecodeParams::Absent));
            let mut sink = DctSink::default();
            {
                let mut stage = filter
                    .decode_pipeline(&mut sink)
                    .expect("DCT stage construction must succeed")
                    .expect("DCT filter must contribute a decode stage");
                stage.write(&jpeg).expect("component JPEG must buffer");
                stage.finish().expect("component JPEG must decode");
            }
            assert_eq!(sink.writes.len(), 1);
            assert_eq!(sink.writes[0].len(), expected_row_length);
            assert_eq!(sink.finishes, 1);
        }
    }

    #[test]
    fn dct_stage_rejects_non_eight_bit_precision() {
        let mut filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(filter.set_decode_params(&DecodeParams::Absent));
        let mut sink = DctSink::default();
        let error = {
            let mut stage = filter
                .decode_pipeline(&mut sink)
                .expect("DCT stage construction must succeed")
                .expect("DCT filter must contribute a decode stage");
            stage
                .write(&test_12_bit_jpeg())
                .expect("12-bit JPEG must buffer");
            stage.finish().expect_err("12-bit JPEG must be rejected")
        };
        assert!(matches!(error, PipelineError::Runtime(_)));
        // Both backends now report libjpeg's own JERR_BAD_PRECISION wording
        // ("Unsupported JPEG data precision %d", `/usr/include/jerror.h:70`):
        // the default backend's own precision gate is worded to match it,
        // and the compatibility backend gets it for free from real libjpeg.
        assert_eq!(
            error.to_string(),
            "DCT decode: Unsupported JPEG data precision 12"
        );
        assert!(sink.writes.is_empty());
        assert_eq!(sink.finish_attempts, 0);
    }

    #[cfg(not(feature = "qpdf-libjpeg-compat"))]
    #[test]
    fn dct_stage_rejects_unknown_component_count() {
        let mut filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(filter.set_decode_params(&DecodeParams::Absent));
        let mut sink = DctSink::default();
        let error = {
            let mut stage = filter
                .decode_pipeline(&mut sink)
                .expect("DCT stage construction must succeed")
                .expect("DCT filter must contribute a decode stage");
            stage
                .write(&test_unknown_component_jpeg())
                .expect("unknown-component JPEG must buffer");
            stage
                .finish()
                .expect_err("unknown component count must be rejected")
        };
        assert!(matches!(error, PipelineError::Runtime(_)));
        assert_eq!(
            error.to_string(),
            "DCT decode: unsupported JPEG component count 2"
        );
        assert!(sink.writes.is_empty());
        assert_eq!(sink.finish_attempts, 0);
    }

    #[cfg(feature = "qpdf-libjpeg-compat")]
    #[test]
    fn dct_compat_accepts_unknown_component_count_like_qpdf() {
        let mut filter = stream_filter_for(b"DCTDecode").expect("registered DCT filter");
        assert!(filter.set_decode_params(&DecodeParams::Absent));
        let mut sink = DctSink::default();
        {
            let mut stage = filter
                .decode_pipeline(&mut sink)
                .expect("DCT stage construction must succeed")
                .expect("DCT filter must contribute a decode stage");
            stage
                .write(&test_unknown_component_jpeg())
                .expect("unknown-component JPEG must buffer");
            stage
                .finish()
                .expect("qpdf-compatible backend must preserve output components");
        }
        assert_eq!(sink.writes.len(), 1);
        assert_eq!(sink.writes[0].len(), 2);
        assert_eq!(sink.finishes, 1);
    }

    /// qpdf's `filter_factories` (`QPDF_Stream.cc:85-94`) holds `/Crypt`
    /// alongside six codecs, so [`stream_filter_for`] holds it too. DCTDecode
    /// contributes the streaming stage; its whole-buffer adapter drives the
    /// same stage so both callers exercise the qpdf DCT decode primitive.
    ///
    /// The legacy `filters::prepare_decode_filters` route still routes a
    /// `Crypt` spec to `PreparedStage::Crypt` before this lookup. The
    /// qpdf-shaped `ObjectHandle::pipe_stream_data` route reaches this factory
    /// directly, and this asserts the registration so it cannot silently
    /// disappear from that caller.
    #[test]
    fn factory_returns_the_crypt_filter() {
        assert!(stream_filter_for(b"Crypt").is_some());
    }

    /// `SF_Crypt::setDecodeParms` walks `getKeys()` (`QPDF_Stream.cc:40`).
    ///
    /// Nothing in production asks: [`filter_reads_decode_params`] short-circuits
    /// on [`is_crypt_filter`] before it consults the registry, so the two agree
    /// rather than one deciding.
    #[test]
    fn crypt_filter_reads_its_decode_params() {
        assert!(CryptStreamFilter.reads_decode_params());
    }

    /// `SF_Crypt` (`QPDF_Stream.cc:27-58`) declares only `setDecodeParms` and
    /// `getDecodePipeline`, overriding neither `isSpecializedCompression` nor
    /// `isLossyCompression`, so both fall through to the base class's `false`
    /// (`QPDFStreamFilter.cc:9-19`).
    ///
    /// Inheriting is the whole assertion: every other registered filter states
    /// this classification somewhere, and `Crypt` is registered like any of
    /// them, so an override added here would otherwise go unwitnessed.
    #[test]
    fn crypt_inherits_the_default_compression_classification() {
        assert!(!CryptStreamFilter.is_specialized_compression());
        assert!(!CryptStreamFilter.is_lossy_compression());
    }

    /// Build the entry set a `Crypt` stage's `/DecodeParms` reduce to.
    ///
    /// Entries go in sorted key order, which is what both readers produce and
    /// what qpdf's `getKeys()` `std::set` yields. Null-valued keys are absent
    /// because both readers that can feed a `Crypt` stage drop them, matching
    /// `QPDF_Dictionary::getKeys` (`QPDF_Dictionary.cc:118-127`).
    fn crypt_accepts(entries: &[(&str, ParamValue)]) -> bool {
        CryptStreamFilter.set_decode_params(&neutral_params(entries))
    }

    fn name_value(name: &str) -> ParamValue {
        ParamValue::Name(name.as_bytes().to_vec())
    }

    /// Every `/DecodeParms` shape below was observed against qpdf 11.9.0 on
    /// 2026-08-08, through `qpdf --show-object=4 --filtered-stream-data` over
    /// a PDF whose object 4 is a stream with `/Filter /Crypt` and the shape
    /// under test. Exit 0 (or 3, "succeeded with warnings", with the data
    /// emitted) means qpdf filtered the stream, so `setDecodeParms` returned
    /// `true`; exit 2 came with "unable to filter stream data".
    ///
    /// | `/DecodeParms`                                  | exit | accepts |
    /// |-------------------------------------------------|------|---------|
    /// | absent                                          | 0    | yes     |
    /// | `null`                                          | 0    | yes     |
    /// | `<< >>`                                         | 0    | yes     |
    /// | `42`                                            | 3    | yes     |
    /// | `<< /Name /Identity >>`                         | 0    | yes     |
    /// | `<< /Name /StdCF >>`                            | 0    | yes     |
    /// | `<< /Type /CryptFilterDecodeParms >>`           | 0    | yes     |
    /// | `<< /Type /CryptFilterDecodeParms /Name /Identity >>` | 0 | yes  |
    /// | `<< /Type /Foo >>`                              | 2    | no      |
    /// | `<< /Type /Foo /Name /Identity >>`              | 2    | no      |
    /// | `<< /Type 1 /Name /Identity >>`                 | 2    | no      |
    /// | `<< /Foo 1 >>`                                  | 2    | no      |
    /// | `<< /Name /Identity /Foo 1 >>`                  | 2    | no      |
    /// | `<< /Type /CryptFilterDecodeParms /Foo 1 >>`    | 2    | no      |
    ///
    /// The `42` row exits 3 because `getKeys` warns `typeWarning("dictionary",
    /// "treating as empty")` (`QPDFObjectHandle.cc:1005`) and hands back an
    /// empty key set; the data is still filtered, so it is an acceptance.
    /// flpdf's live parser handles now emit the same warning through their
    /// document resolver before reducing the value to `Present` with no
    /// entries. A contextless programmatic handle retains qpdf's throwing
    /// `typeWarning` branch.
    ///
    /// The probes used direct values throughout. A dangling indirect reference
    /// resolves to null silently and `getKeys` then drops the key, so an
    /// indirect probe could not show whether qpdf inspected the value.
    #[test]
    fn crypt_accepts_only_type_and_name_keys() {
        // A null or missing /DecodeParms is SF_Crypt's early return
        // (QPDF_Stream.cc:36-38); both reduce to `Absent`.
        assert!(CryptStreamFilter.set_decode_params(&DecodeParams::Absent));
        // An empty dictionary and a present non-dictionary both reduce to an
        // empty entry set, which never enters the key loop.
        assert!(crypt_accepts(&[]));

        assert!(crypt_accepts(&[("Name", name_value("Identity"))]));
        // SF_Crypt never reads /Name's value; decryptStream does.
        assert!(crypt_accepts(&[("Name", name_value("StdCF"))]));
        assert!(crypt_accepts(&[(
            "Type",
            name_value("CryptFilterDecodeParms")
        )]));
        assert!(crypt_accepts(&[
            ("Name", name_value("Identity")),
            ("Type", name_value("CryptFilterDecodeParms")),
        ]));

        // A present /Type that is not /CryptFilterDecodeParms fails
        // isDictionaryOfType, so even the /Type and /Name keys are refused.
        assert!(!crypt_accepts(&[("Type", name_value("Foo"))]));
        assert!(!crypt_accepts(&[
            ("Name", name_value("Identity")),
            ("Type", name_value("Foo")),
        ]));
        // /Type present but not a name: isNameAndEquals is false.
        assert!(!crypt_accepts(&[
            ("Name", name_value("Identity")),
            ("Type", ParamValue::Int(1)),
        ]));

        // Any key outside /Type and /Name lands on the else arm, whatever
        // /Type says — the shapes only expressible since a Crypt stage began
        // retaining its whole key set.
        assert!(!crypt_accepts(&[("Foo", ParamValue::Int(1))]));
        assert!(!crypt_accepts(&[
            ("Foo", ParamValue::Int(1)),
            ("Name", name_value("Identity")),
        ]));
        assert!(!crypt_accepts(&[
            ("Foo", ParamValue::Int(1)),
            ("Type", name_value("CryptFilterDecodeParms")),
        ]));
    }

    /// `SF_Crypt::getDecodePipeline` returns `nullptr` (`QPDF_Stream.cc:52-56`).
    #[test]
    fn crypt_builds_no_decode_stage() {
        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        assert!(CryptStreamFilter
            .decode_pipeline(&mut sink)
            .expect("stage construction must succeed")
            .is_none());
        assert_eq!(*trace.borrow(), Trace::default());
    }

    /// The decode route is flpdf's own, and repeats the message
    /// `filters::reject_crypt_stage` produces so that routing a `Crypt` stage
    /// here would leave the public error unchanged.
    #[test]
    fn crypt_refuses_to_decode() {
        // `.err().unwrap()` rather than `.unwrap_err()`: the success type is
        // `FilterDecodeOutcome`, which has no `Debug`.
        let error = CryptStreamFilter
            .pipe_decode_recovering(b"anything", None, &mut |_, _, _, _| Ok(()))
            .err()
            .unwrap();
        assert!(matches!(error, Error::Unsupported(_)), "{error:?}");
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: unsupported stream filter: Crypt"
        );
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
    /// The filter name reaching that reader decides one thing only — whether
    /// this is the `Crypt` stage that keeps every key
    /// ([`retains_decode_param_key`]) with name payloads under
    /// [`CRYPT_NAME_PAYLOAD_DECODE_PARAM_KEYS`]. Every non-`Crypt` name
    /// therefore produces the identical reduction whatever the dictionary
    /// holds, so no call here is sensitive to the name chosen. Tests that are
    /// *about* that decision call `decode_filter_specs_from_object` and name
    /// their filter explicitly.
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
    fn handle_decode_params_warn_when_integer_values_are_saturated() {
        let cases = [
            (
                i64::from(i32::MIN) - 1,
                ParamValue::Int(i32::MIN),
                "object 3 0: requested value of integer is too small; returning INT_MIN",
            ),
            (
                i64::from(i32::MAX) + 1,
                ParamValue::Int(i32::MAX),
                "object 3 0: requested value of integer is too big; returning INT_MAX",
            ),
        ];

        for (value, expected, warning) in cases {
            let (handle, recorder) = handle_resolving(ObjectValue::Integer(value));

            assert_eq!(
                param_value_from_handle(&handle, false, true).unwrap(),
                expected
            );
            assert_eq!(warnings(&recorder), [warning]);
        }
    }

    #[test]
    fn snapshot_decode_params_saturates_without_warning_for_non_consuming_filters() {
        for filter_name in [
            b"ASCIIHexDecode".as_slice(),
            b"ASCII85Decode".as_slice(),
            b"RunLengthDecode".as_slice(),
        ] {
            let value = i64::from(i32::MAX) + 1;
            let (child, recorder) = handle_resolving(ObjectValue::Integer(value));
            assert_eq!(child.try_as_integer().unwrap(), Some(value));

            let params = ObjectHandle::dictionary(vec![(b"/Columns".to_vec(), child)]);
            assert_eq!(
                decode_params_from_handle(&params, filter_name).unwrap(),
                DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Int(i32::MAX)),])
            );
            assert!(
                warnings(&recorder).is_empty(),
                "{filter_name:?} emitted a saturation warning"
            );
        }
    }

    #[test]
    fn direct_decode_params_saturate_integer_values() {
        let params = ObjectHandle::dictionary(vec![(
            b"/Columns".to_vec(),
            ObjectHandle::integer(i64::from(i32::MIN) - 1),
        )]);

        assert_eq!(
            decode_params_from_handle(&params, b"ASCIIHexDecode").unwrap(),
            DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Int(i32::MIN)),])
        );
    }

    #[test]
    fn consuming_decode_params_warn_only_for_integer_consuming_keys() {
        let cases = [
            (b"FlateDecode".as_slice(), b"Columns".as_slice(), true),
            (b"LZWDecode".as_slice(), b"EarlyChange".as_slice(), true),
            (b"FlateDecode".as_slice(), b"EarlyChange".as_slice(), false),
        ];

        for (filter_name, key, warns) in cases {
            let value = i64::from(i32::MAX) + 1;
            let (child, recorder) = handle_resolving(ObjectValue::Integer(value));
            let params = ObjectHandle::dictionary(vec![(key.to_vec(), child)]);

            assert_eq!(
                decode_params_from_handle(&params, filter_name).unwrap(),
                DecodeParams::Present(vec![(key.to_vec(), ParamValue::Int(i32::MAX)),])
            );
            if warns {
                assert_eq!(
                    warnings(&recorder),
                    ["object 3 0: requested value of integer is too big; returning INT_MAX"]
                );
            } else {
                assert!(warnings(&recorder).is_empty());
            }
        }
    }

    #[test]
    fn crypt_decode_params_do_not_warn_for_unknown_integer_keys() {
        let value = i64::from(i32::MAX) + 1;
        let (child, recorder) = handle_resolving(ObjectValue::Integer(value));
        let params = ObjectHandle::dictionary(vec![(b"/Unknown".to_vec(), child)]);

        assert_eq!(
            decode_params_from_handle(&params, b"Crypt").unwrap(),
            DecodeParams::Present(vec![(b"Unknown".to_vec(), ParamValue::Int(i32::MAX))])
        );
        assert!(warnings(&recorder).is_empty());
    }

    #[test]
    fn unrecognized_decode_param_keys_do_not_consume_integers() {
        assert!(!consumes_integer_decode_param_key(
            b"Unknown",
            b"FlateDecode"
        ));
    }

    #[test]
    fn handle_decode_param_names_do_not_emit_integer_warnings() {
        let (handle, recorder) = handle_resolving(ObjectValue::Name(b"Identity".to_vec()));

        assert_eq!(
            param_value_from_handle(&handle, true, false).unwrap(),
            ParamValue::Name(b"Identity".to_vec())
        );
        assert!(warnings(&recorder).is_empty());
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
    fn tiff_predictor_is_constructed_at_pipeline_construction() {
        let cases = [
            (b"FlateDecode".as_slice(), encode_flate(b"A").unwrap()),
            (b"LZWDecode".as_slice(), vec![0x80, 0x10, 0x60, 0x20]),
        ];

        for (name, encoded) in cases {
            let mut filter = stream_filter_for(name).expect("registered filter");
            assert!(filter.set_decode_params(&neutral_params(&[
                ("Predictor", ParamValue::Int(2)),
                ("Columns", ParamValue::Int(1)),
            ])));

            let decoded = filter
                .pipe_decode(&encoded, None, &mut ignore_warning)
                .expect("TIFF predictor decode");
            assert_eq!(decoded, b"A", "{name:?}");
        }
    }

    #[test]
    fn negative_geometry_is_rejected_when_the_predictor_pipeline_is_built() {
        for predictor in [2, 12] {
            for (key, value) in [("Columns", -4), ("Colors", -1), ("BitsPerComponent", -8)] {
                let mut filter =
                    stream_filter_for(b"FlateDecode").expect("registered Flate filter");
                assert!(filter.set_decode_params(&neutral_params(&[
                    ("Predictor", ParamValue::Int(predictor)),
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
                    "predictor {predictor}, {key}"
                );
            }
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

    /// Two complete PNG rows for a `/Columns 4 /Colors 1 /BitsPerComponent 8`
    /// geometry: filter byte 0 (None) passes `abcd` through, filter byte 2 (Up)
    /// adds the decoded row above, so `1 1 1 1` becomes `bcde`.
    const PNG_PREDICTED_ROWS: &[u8] = &[0, b'a', b'b', b'c', b'd', 2, 1, 1, 1, 1];
    const PNG_PREDICTOR_DECODED: &[u8] = b"abcdbcde";

    /// Pack `data` as one literal LZW code per byte between a clear and an EOD
    /// code, the shape
    /// `flate_and_lzw_predictor_finish_output_keeps_its_cleanup_boundary`
    /// already uses.
    ///
    /// Every code stays 9 bits wide because the table grows by one entry per
    /// code and the width change is 252 codes away.
    fn lzw_encoded(data: &[u8]) -> Vec<u8> {
        let mut codes = vec![256];
        codes.extend(data.iter().map(|&byte| u32::from(byte)));
        codes.push(257);
        pack_codes(&codes, true)
    }

    fn png_predictor_params() -> DecodeParams {
        neutral_params(&[
            ("Predictor", ParamValue::Int(12)),
            ("Columns", ParamValue::Int(4)),
        ])
    }

    /// Drive `filter`'s decode stage over `encoded` in `chunk`-sized writes
    /// followed by a single `finish`, and report what the sink saw.
    ///
    /// The payload must span at least three writes so the stage is exercised
    /// across write boundaries rather than handed a whole buffer.
    fn stream_decode_pipeline(
        filter: &mut dyn StreamFilter,
        encoded: &[u8],
        chunk: usize,
    ) -> Rc<RefCell<Trace>> {
        assert!(
            encoded.chunks(chunk).count() >= 3,
            "the payload must span at least three writes"
        );
        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        {
            let mut stage = filter
                .decode_pipeline(&mut sink)
                .expect("stage construction must succeed")
                .expect("this filter contributes a stage");
            for part in encoded.chunks(chunk) {
                stage.write(part).expect("write must succeed");
            }
            stage.finish().expect("finish must succeed");
        }
        trace
    }

    fn finish_count(trace: &Trace) -> usize {
        trace
            .calls
            .iter()
            .filter(|call| matches!(call, TraceCall::Finish { .. }))
            .count()
    }

    #[test]
    fn predictor_construction_failure_precedes_every_write() {
        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        let mut filter = FlateLzwStreamFilter::new(false);
        // /Colors 0 is what `PngFilter::new` rejects as an invalid
        // samples_per_pixel; /Columns keeps its default 1, so the filter still
        // accepts the parameters and the rejection lands in the stage factory.
        assert!(filter.set_decode_params(&neutral_params(&[
            ("Predictor", ParamValue::Int(12)),
            ("Colors", ParamValue::Int(0)),
        ])));

        // `.err().unwrap()` rather than `.unwrap_err()`: the success type is
        // `Option<Box<dyn Pipeline>>`, which has no `Debug`.
        let error = filter.decode_pipeline(&mut sink).err().unwrap();

        assert!(error.to_string().contains("samples_per_pixel"), "{error}");
        assert!(trace.borrow().calls.is_empty());
    }

    #[test]
    fn flate_decode_pipeline_streams_chunked_writes_to_the_sink() {
        let encoded = encode_flate(b"hello flate world").unwrap();
        let mut filter = FlateLzwStreamFilter::new(false);
        let trace = stream_decode_pipeline(&mut filter, &encoded, 3);
        let trace = trace.borrow();
        assert_eq!(trace.output, b"hello flate world");
        assert_eq!(finish_count(&trace), 1);
    }

    #[test]
    fn lzw_decode_pipeline_streams_chunked_writes_to_the_sink() {
        let encoded = lzw_encoded(b"hello lzw world");
        let mut filter = FlateLzwStreamFilter::new(true);
        let trace = stream_decode_pipeline(&mut filter, &encoded, 3);
        let trace = trace.borrow();
        assert_eq!(trace.output, b"hello lzw world");
        assert_eq!(finish_count(&trace), 1);
    }

    #[test]
    fn ascii85_decode_pipeline_streams_chunked_writes_to_the_sink() {
        let mut filter = Ascii85StreamFilter;
        let trace = stream_decode_pipeline(&mut filter, b"9jqo^BlbD-~>", 3);
        let trace = trace.borrow();
        assert_eq!(trace.output, b"Man is d");
        assert_eq!(finish_count(&trace), 1);
    }

    #[test]
    fn ascii_hex_decode_pipeline_streams_chunked_writes_to_the_sink() {
        let mut filter = AsciiHexStreamFilter;
        let trace = stream_decode_pipeline(&mut filter, b"616263>", 3);
        let trace = trace.borrow();
        assert_eq!(trace.output, b"abc");
        assert_eq!(finish_count(&trace), 1);
    }

    #[test]
    fn run_length_decode_pipeline_streams_chunked_writes_to_the_sink() {
        // A three-byte literal run, then 'z' repeated 257 - 0xfe = 3 times.
        let encoded: &[u8] = &[0x02, b'a', b'b', b'c', 0xfe, b'z', 0x80];
        let mut filter = RunLengthStreamFilter;
        let trace = stream_decode_pipeline(&mut filter, encoded, 3);
        let trace = trace.borrow();
        assert_eq!(trace.output, b"abczzz");
        assert_eq!(finish_count(&trace), 1);
    }

    #[test]
    fn predictor_flate_decode_pipeline_streams_through_both_stages() {
        let encoded = encode_flate(PNG_PREDICTED_ROWS).unwrap();
        let mut filter = FlateLzwStreamFilter::new(false);
        assert!(filter.set_decode_params(&png_predictor_params()));

        let trace = stream_decode_pipeline(&mut filter, &encoded, 3);

        let trace = trace.borrow();
        assert_eq!(trace.output, PNG_PREDICTOR_DECODED);
        assert_eq!(finish_count(&trace), 1);
    }

    #[test]
    fn predictor_lzw_decode_pipeline_streams_through_both_stages() {
        let encoded = lzw_encoded(PNG_PREDICTED_ROWS);
        let mut filter = FlateLzwStreamFilter::new(true);
        assert!(filter.set_decode_params(&png_predictor_params()));

        let trace = stream_decode_pipeline(&mut filter, &encoded, 3);

        let trace = trace.borrow();
        assert_eq!(trace.output, PNG_PREDICTOR_DECODED);
        assert_eq!(finish_count(&trace), 1);
    }

    #[test]
    fn downstream_write_failure_propagates_out_of_the_stage() {
        let mut sink = RecordingSink::new(&[1], &[]);
        let trace = sink.trace();
        let error = {
            let mut filter = AsciiHexStreamFilter;
            let mut stage = filter.decode_pipeline(&mut sink).unwrap().unwrap();
            stage.write(b"616263>").unwrap_err()
        };

        assert_eq!(error.to_string(), "sink write failure 1");
        assert_eq!(
            trace.borrow().calls,
            [TraceCall::Write {
                data: b"a".to_vec(),
                failed: true,
            }]
        );
    }

    #[test]
    fn downstream_finish_failure_propagates_out_of_the_stage() {
        let mut sink = RecordingSink::new(&[], &[1]);
        let trace = sink.trace();
        let error = {
            let mut filter = AsciiHexStreamFilter;
            let mut stage = filter.decode_pipeline(&mut sink).unwrap().unwrap();
            stage.write(b"616263>").unwrap();
            stage.finish().unwrap_err()
        };

        assert_eq!(error.to_string(), "sink finish failure 1");
        let trace = trace.borrow();
        assert_eq!(trace.output, b"abc");
        assert_eq!(
            trace.calls.last(),
            Some(&TraceCall::Finish { failed: true }),
            "the failure must follow every write"
        );
    }

    /// The failure has to cross the owned predictor stage the two-stage chain
    /// holds, which the single-stage cases above never construct.
    ///
    /// `finish` is the crossing chosen here; a write failure would work too,
    /// because the predictor re-chunks by row and the sink's first write is
    /// always the first decoded row regardless of how the codec buffered.
    #[test]
    fn a_two_stage_chain_forwards_a_finish_failure_through_the_owned_predictor() {
        let encoded = encode_flate(PNG_PREDICTED_ROWS).unwrap();
        let mut sink = RecordingSink::new(&[], &[1]);
        let trace = sink.trace();
        let error = {
            let mut filter = FlateLzwStreamFilter::new(false);
            assert!(filter.set_decode_params(&png_predictor_params()));
            let mut stage = filter.decode_pipeline(&mut sink).unwrap().unwrap();
            for chunk in encoded.chunks(3) {
                stage.write(chunk).unwrap();
            }
            stage.finish().unwrap_err()
        };

        assert_eq!(error.to_string(), "sink finish failure 1");
        let trace = trace.borrow();
        assert_eq!(trace.output, PNG_PREDICTOR_DECODED);
        assert_eq!(finish_count(&trace), 1);
    }

    /// The stage outlives its constructor, not just its construction.
    ///
    /// Dropping the filter before the stage is driven is what distinguishes
    /// returning the chain by value from qpdf's arrangement, where the filter
    /// instance owns every stage it builds and the caller holds a non-owning
    /// pointer (`QPDF_Stream.cc:559-568`). Tie the stage's lifetime back to
    /// `&mut self` and this scope stops compiling.
    #[test]
    fn the_stage_may_outlive_construction_and_be_dropped_before_the_sink_is_read() {
        let encoded = encode_flate(PNG_PREDICTED_ROWS).unwrap();
        let mut sink = OutputBuffer::new(None);
        {
            let mut stage = {
                let mut filter = FlateLzwStreamFilter::new(false);
                assert!(filter.set_decode_params(&png_predictor_params()));
                filter.decode_pipeline(&mut sink).unwrap().unwrap()
            };
            for chunk in encoded.chunks(3) {
                stage.write(chunk).unwrap();
            }
            stage.finish().unwrap();
        }
        assert_eq!(sink.data, PNG_PREDICTOR_DECODED);
    }

    /// The two routes coexist, so they must agree on the bytes they produce.
    ///
    /// Agreement is only expected where the payload emits no codec warning:
    /// `pipe_decode_recovering` installs the Flate warn callback qpdf installs
    /// at its `pipeStreamData` caller (`QPDF_Stream.cc:564-567`), and
    /// `decode_pipeline` deliberately does not.
    #[test]
    fn decode_pipeline_and_whole_buffer_route_agree() {
        let flate = encode_flate(b"agreement across routes").unwrap();
        let lzw = lzw_encoded(b"agreement across routes");
        let cases: &[(&[u8], &[u8], &[u8])] = &[
            (b"FlateDecode", &flate, b"agreement across routes"),
            (b"LZWDecode", &lzw, b"agreement across routes"),
            (b"ASCII85Decode", b"9jqo^BlbD-~>", b"Man is d"),
            (b"ASCIIHexDecode", b"616263>", b"abc"),
            (
                b"RunLengthDecode",
                &[0x02, b'a', b'b', b'c', 0xfe, b'z', 0x80],
                b"abczzz",
            ),
        ];

        for &(name, encoded, expected) in cases {
            let mut streaming = stream_filter_for(name).expect("registered stream filter");
            let trace = stream_decode_pipeline(&mut *streaming, encoded, 3);
            let whole_buffer = stream_filter_for(name)
                .expect("registered stream filter")
                .pipe_decode(encoded, None, &mut ignore_warning)
                .unwrap();

            assert_eq!(whole_buffer, expected, "{name:?}");
            assert_eq!(trace.borrow().output, whole_buffer, "{name:?}");
        }
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

    /// Every registered test double answers `decode_pipeline` the way its own
    /// `pipe_decode_recovering` behaves.
    ///
    /// The owned stage factory is required by the trait, while the borrowed
    /// `decode_pipeline` adapter is shared. `PostPreflightFailure` is the one
    /// whose role reaches past "contributes no stage": it exists to fail on
    /// *every* route past the preflight, and only the whole-buffer half of that
    /// claim is checked elsewhere
    /// (`filters::tests::recovering_decode_propagates_a_post_preflight_adapter_error`).
    #[test]
    fn the_registered_test_doubles_agree_across_both_decode_routes() {
        let mut sink = OutputBuffer::new(None);

        // Both transform nothing, so each is qpdf's nullptr: no stage, and the
        // caller keeps writing straight to its own `next`.
        for name in [
            b"TestRejectDecode".as_slice(),
            b"TestBorrowedInput".as_slice(),
        ] {
            let mut filter = stream_filter_for(name).expect("registered test filter");
            assert!(
                filter
                    .decode_pipeline(&mut sink)
                    .expect("neither double fails construction")
                    .is_none(),
                "{name:?} transforms nothing, so it contributes no stage"
            );
        }
        assert!(
            sink.data.is_empty(),
            "a filter contributing no stage writes nothing of its own"
        );

        let mut filter =
            stream_filter_for(b"TestPostPreflightFailure").expect("registered test filter");
        // `.err().unwrap()` rather than `.unwrap_err()`: the success type is
        // `Option<Box<dyn Pipeline>>`, which has no `Debug`.
        let staged = filter.decode_pipeline(&mut sink).err().unwrap();
        let buffered = filter
            .pipe_decode(b"encoded input", None, &mut ignore_warning)
            .unwrap_err();

        assert_eq!(staged.to_string(), "test post-preflight decode failure");
        assert_eq!(buffered.to_string(), staged.to_string());
    }
}

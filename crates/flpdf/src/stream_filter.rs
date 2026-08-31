//! qpdf correspondence: QPDFStreamFilter.cc and QPDF_Stream.cc filter-name, DecodeParms-alignment, and decode-pipeline construction responsibilities, read from ObjectHandle-shaped /Filter and /DecodeParms values.
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
use crate::pipeline::ascii85_decoder::Ascii85Decoder;
use crate::pipeline::ascii_hex::AsciiHexDecoder;
use crate::pipeline::buffer::Buffer;
use crate::pipeline::dct::PlDct;
use crate::pipeline::flate::{Flate, FlateAction, DEFAULT_OUT_BUFFER_SIZE};
use crate::pipeline::lzw::LzwDecoder;
use crate::pipeline::png_filter::{PngFilter, PngFilterAction};
use crate::pipeline::run_length::{RunLength, RunLengthAction};
use crate::pipeline::tiff_predictor::{TiffPredictor, TiffPredictorAction};
use crate::pipeline::{Pipeline, PipelineError, PipelineRef, PipelineResult};
use crate::{Error, Result};
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
/// codec. The handle reader makes the same factory decision before reading
/// `/DecodeParms`, through [`validate_filter_factories`].
pub(crate) const DECODE_PARMS_LENGTH_ERROR: &str =
    "stream /DecodeParms length is inconsistent with filters";

/// Reject a `/Filter` chain longer than `maximum`.
///
/// Unlike qpdf, which caps nothing here, flpdf refuses pathological chains on
/// the decode path; `filters::MAX_FILTER_CHAIN_LEN` documents that divergence.
///
/// The handle reader calls this before it snapshots an array, so the cap's
/// *body* — the comparison and the message — has exactly one definition.
pub(crate) fn validate_filter_chain_count(count: usize, maximum: Option<usize>) -> Result<()> {
    if let Some(maximum) = maximum.filter(|maximum| count > *maximum) {
        return Err(Error::Unsupported(format!(
            "filter chain length {count} exceeds maximum of {maximum}"
        )));
    }
    Ok(())
}

/// The same read as `decode_filter_specs_from_object`, entered through the
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
/// Deliberately the shape of `param_value_from_object`: the same
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
/// thing a non-consuming stage observes for an unresolved indirect value. An indirect value
/// that some earlier read already resolved does not: `ObjectHandle::as_integer`
/// reports an indirect handle's already-resolved value, so a
/// `/DecodeParms` value sharing an object with an earlier-visited position
/// would classify as `Int` here. That distinction is intentionally not exposed
/// by the non-consuming filter path: the filters routed here read nothing but
/// `is_absent()`, and `is_absent()` distinguishes `Absent` from `Present`
/// without looking at a single [`ParamValue`]. What must not vary is `Absent`
/// versus `Present`, and that is decided upstream by the two unconditional
/// calls on the parameter handle itself.
fn param_value_without_resolving(value: &ObjectHandle) -> ParamValue {
    match value.as_integer() {
        Some(int) => ParamValue::Int(clamp_to_i32(int)),
        None => ParamValue::Other,
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

/// Rust equivalent of qpdf's `QPDFStreamFilter` extension boundary.
///
/// `pipe_decode_recovering` owns construction and completion of the filter's
/// decode pipeline. A whole-buffer result keeps the legacy decode helpers
/// stable while the individual codecs use incremental `Pipeline` stages.
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

    /// Install the optional qpdf-head TIFF row-memory budget before preflight
    /// and execution. Other filters ignore this setting.
    fn set_tiff_memory_limit(&mut self, _limit: Option<usize>) {}

    /// Construct the same stage with a downstream pipeline that may already
    /// own inner stages. This is the Rust ownership seam used by
    /// `QPDF_Stream::pipeStreamData`'s reverse chain construction.
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

    /// Whether this filter is a specialized compression codec for qpdf's
    /// stream capability classification.
    fn is_specialized_compression(&self) -> bool {
        false
    }

    /// Whether this filter is a lossy compression codec for qpdf's stream
    /// capability classification.
    fn is_lossy_compression(&self) -> bool {
        false
    }
}

/// Result of constructing a stage around a downstream pipeline that may
/// already own inner stages. `NoStage` returns the downstream slot so the
/// caller can keep threading it through a filter such as qpdf's `Crypt`.
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
    tiff_max_memory: Option<usize>,
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
            tiff_max_memory: None,
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
            let _predictor = make_predictor_pipeline(
                geometry,
                &mut sink,
                PredictorAction::Decode,
                self.tiff_max_memory,
            )?;
        }
        Ok(())
    }

    fn set_tiff_memory_limit(&mut self, limit: Option<usize>) {
        self.tiff_max_memory = limit;
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
            Some(geometry) => make_predictor_pipeline(
                geometry,
                next,
                PredictorAction::Decode,
                self.tiff_max_memory,
            )?,
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
                let mut predictor = make_predictor_pipeline(
                    geometry,
                    &mut sink,
                    PredictorAction::Decode,
                    self.tiff_max_memory,
                )?;
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
    /// elsewhere — see qpdf's `getDecodePipeline` boundary. Both that case and
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
    tiff_max_memory: Option<usize>,
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
            TiffPredictor::new_with_memory_limit(
                "tiff encode",
                next,
                TiffPredictorAction::Encode,
                geometry.columns,
                geometry.colors,
                geometry.bits_per_component,
                tiff_max_memory,
            )
            .map_err(map_pipeline_error)?,
        ) as Box<dyn Pipeline + 'a>,
        (PredictorKind::Tiff, PredictorAction::Decode) => Box::new(
            TiffPredictor::new_with_memory_limit(
                "tiff decode",
                next,
                TiffPredictorAction::Decode,
                geometry.columns,
                geometry.colors,
                geometry.bits_per_component,
                tiff_max_memory,
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
    // qpdf-deviation: no qpdf counterpart -- QPDFStreamFilter has no
    // execute-time decode call (only setDecodeParms/getDecodePipeline), so
    // this call shape (invoking a Crypt stage's decode step directly) is one
    // qpdf can never produce; this refusal guards flpdf's own registry route
    // rather than reproducing any qpdf behavior.
    fn pipe_decode_recovering(
        &mut self,
        _data: &[u8],
        _max_output: Option<usize>,
        _warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        Err(Error::Unsupported(CRYPT_STAGE_UNSUPPORTED.to_string()))
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
        let mut predictor =
            make_predictor_pipeline(geometry, &mut sink, PredictorAction::Encode, None)?;
        predictor.write(data).map_err(map_pipeline_error)?;
        predictor.finish().map_err(map_pipeline_error)?;
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

#[cfg(test)]
mod tests {
    use super::{DecodeParams, FlateLzwStreamFilter, ParamValue, StreamFilter};
    use crate::pipeline::test_support::RecordingSink;
    use crate::pipeline::PipelineRef;

    fn wide_tiff_decode_params() -> DecodeParams {
        DecodeParams::Present(vec![
            (b"Predictor".to_vec(), ParamValue::Int(2)),
            (b"Columns".to_vec(), ParamValue::Int(536_870_911)),
            (b"Colors".to_vec(), ParamValue::Int(1)),
            (b"BitsPerComponent".to_vec(), ParamValue::Int(8)),
        ])
    }

    fn wide_tiff_filter() -> FlateLzwStreamFilter {
        let mut filter = FlateLzwStreamFilter::new(false);
        assert!(filter.set_decode_params(&wide_tiff_decode_params()));
        filter.set_tiff_memory_limit(Some(1 << 20));
        filter
    }

    #[test]
    fn owned_decode_pipeline_applies_tiff_memory_limit_before_codec_construction() {
        let mut filter = wide_tiff_filter();
        let mut sink = RecordingSink::new(&[], &[]);
        let error = match filter.decode_pipeline_owned(PipelineRef::from(&mut sink)) {
            Err(error) => error,
            Ok(_) => panic!("the wide TIFF row must exceed the configured budget"),
        };

        assert!(error
            .to_string()
            .contains("TIFFPredictor memory limit exceeded"));
    }

    #[test]
    fn recovering_decode_pipeline_applies_tiff_memory_limit_before_codec_writes() {
        let mut filter = wide_tiff_filter();
        let error = match filter.pipe_decode_recovering(&[], None, &mut |_, _, _, _| Ok(())) {
            Err(error) => error,
            Ok(_) => panic!("the wide TIFF row must exceed the configured budget"),
        };

        assert!(error
            .to_string()
            .contains("TIFFPredictor memory limit exceeded"));
    }

    #[test]
    fn encode_predictor_uses_the_tiff_stream_filter_pipeline() {
        let params = DecodeParams::Present(vec![
            (b"Predictor".to_vec(), ParamValue::Int(2)),
            (b"Columns".to_vec(), ParamValue::Int(2)),
            (b"Colors".to_vec(), ParamValue::Int(1)),
            (b"BitsPerComponent".to_vec(), ParamValue::Int(8)),
        ]);

        assert_eq!(
            super::encode_predictor(&[10, 20], b"FlateDecode", &params).unwrap(),
            [10, 10]
        );
    }
}

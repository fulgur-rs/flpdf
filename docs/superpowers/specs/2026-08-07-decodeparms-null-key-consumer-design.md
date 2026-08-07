# DecodeParms Null-Key Consumer Design

## Goal

Make flpdf consume `/DecodeParms` dictionaries the way qpdf 11.9.0 does:
filters that call `QPDFObjectHandle::getKeys()` must omit every key whose value
resolves to null before interpreting the remaining values. This closes
`flpdf-h8mv` for both the legacy `Object` reader and the `ObjectHandle` reader
without moving the behavior into a filter-specific workaround.

## qpdf responsibility model

The behavior is split across two qpdf responsibilities:

1. `QPDF_Dictionary::getKeys` walks every dictionary entry, calls
   `QPDFObjectHandle::isNull()` on its value, and returns only the non-null keys
   in sorted order (`libqpdf/QPDF_Dictionary.cc:117-127`). `isNull()` performs
   lazy dereference (`libqpdf/QPDFObjectHandle.cc:352-356,2375-2382`), so direct
   null, indirect-to-null, and dangling/cyclic references that resolve to null
   are omitted by the same primitive.
2. `SF_FlateLzwDecode::setDecodeParms` returns early only when the whole
   `/DecodeParms` object is null. Otherwise it consumes `getKeys()` and calls
   `getKey()` for each returned key (`libqpdf/SF_FlateLzwDecode.cc:21-72`).
   `SF_Crypt::setDecodeParms` uses the same key-enumeration primitive
   (`libqpdf/QPDF_Stream.cc:33-50`). Filters inheriting the base
   `QPDFStreamFilter::setDecodeParms` inspect only whether the whole parameter
   object is null and do not enumerate entries (`libqpdf/QPDFStreamFilter.cc:3-7`).

`QPDF_Stream::filterable` creates one filter instance per declared stage and
calls `setDecodeParms` once per stage in order (`libqpdf/QPDF_Stream.cc:378-485`).
The consumer must therefore preserve per-stage enumeration and error order.

## Existing flpdf boundary

`ObjectHandle::try_get_keys(&self) -> Result<BTreeSet<Vec<u8>>>` already owns
the qpdf dictionary responsibility. It resolves the holder and every child,
omits null-resolving values, preserves sorted key order, releases container
borrows before resolver calls, and propagates resolver errors unchanged.
`flpdf-25kg.3.23` implemented and independently tested this prerequisite.

The remaining divergence is in `stream_filter.rs`:

- `decode_params_from_object` iterates the raw `Dictionary`, so a direct
  `Object::Null` under `/Predictor` becomes `ParamValue::Other`.
- `decode_params_from_entries` calls `try_is_null()` for consuming filters but
  discards the boolean, so direct and resolved null handles remain in the
  retained parameter set as `ParamValue::Other`.
- `FlateLzwStreamFilter::set_decode_params` correctly rejects
  `ParamValue::Other`; the error is therefore upstream entry enumeration, not
  filter validation.

Both shape readers produce the same `FilterSpec` and share all codec,
predictor, limit, and warning-ordering code below that boundary. The fix stays
entirely in parameter reading and leaves the shared decoder unchanged.

## Chosen design

### ObjectHandle reader

For a filter whose qpdf counterpart enumerates `/DecodeParms` entries, read
the keys through `params.try_get_keys()`. Iterate that returned set, apply the
existing bounded retention policy only after enumeration, and retrieve each
retained value through `params.try_get_key(&key)` before reducing it to
`ParamValue`.

This order is load-bearing:

1. `try_get_keys` touches every value, including unknown keys, before the
   retained-key filter.
2. Null-resolving keys never reach value classification.
3. Resolver errors preserve dictionary-key order and stop at the same first
   failing child as the primitive.
4. A retained non-null value is classified through the existing resolving
   integer/name accessors.

A scalar `/DecodeParms` replicated across multiple filters is enumerated once
per consuming stage, matching qpdf's one `setDecodeParms` call per filter.
Non-consuming base filters continue to inspect only whether the parameter
object is absent/null; they must not resolve dictionary children merely
because flpdf stores a bounded snapshot.

### Legacy Object reader

The legacy reader has no resolver and therefore cannot reproduce indirect
resolution. For the direct values it can observe, it applies the same
responsibility boundary: when the selected filter enumerates entries, omit
every `Object::Null` value before the existing retained-key and
`ParamValue` conversion. The rule is independent of key name; it is not a
special case for `/Predictor`.

For filters inheriting the base `setDecodeParms`, a present dictionary remains
`DecodeParams::Present` even if all entries are direct null. Those filters
still reject the non-null parameter object, as qpdf does.

### Filter behavior

`FlateLzwStreamFilter::set_decode_params`, parameter defaults, predictor
validation, and pipeline construction remain unchanged. A dictionary whose
only recognized entries resolve to null becomes a present-but-empty parameter
set, so Flate/LZW retain constructor defaults and stay filterable. A non-null
invalid value such as `/Predictor 5` or `/Predictor /Up` remains rejected.

## Rejected approaches

1. **Discard the result of `try_is_null` but add `continue` locally.** This can
   reproduce the immediate output, but bypasses the completed
   `QPDF_Dictionary::getKeys` primitive and leaves holder/non-dictionary,
   ordering, and future consumers split across two implementations.
2. **Treat `ParamValue::Other` as absent inside Flate/LZW.** `Other` also
   represents non-null names, arrays, strings, and references on the legacy
   path. Accepting it would weaken qpdf's integer validation and put dictionary
   visibility in the filter layer.
3. **Filter only the five recognized parameter names.** qpdf resolves every
   dictionary value while constructing `getKeys`, including unknown keys.
   Key-name-specific skipping would change resolver side effects and error
   precedence.

## Error and ordering behavior

- Direct, indirect-to-null, dangling-to-null, and cycle-to-null values are
  omitted only through the dictionary-key enumeration responsibility.
- A genuine resolver error propagates unchanged; it is never converted to an
  absent key.
- Keys remain bytewise sorted through `BTreeSet`/`BTreeMap`, matching qpdf's
  `std::set<std::string>` for decoded names.
- Non-null invalid recognized values remain unfilterable before codec work.
- Unknown non-null keys remain ignored by Flate/LZW after `getKeys` has touched
  their values.
- Existing warning-channel divergence tracked outside `flpdf-h8mv` is not
  changed.

## Test design

Development follows RED -> GREEN:

1. Change the existing direct-null test so `/Predictor null` succeeds while a
   non-null non-integer still fails. Run it before production changes and
   observe the expected failure.
2. Add handle-reader tests for direct null, indirect-to-null, and
   dangling-to-null keys. Assert the resulting `FilterSpec` is present with an
   empty parameter list, not absent, and that a non-null control is retained.
3. Add a resolver-error test proving `try_get_keys` failure propagates before
   parameter classification.
4. Update the legacy-vs-native shape and entry-point corpus row so both paths
   succeed for direct null and decode real Flate payload bytes.
5. Add or update a live qpdf 11.9.0 oracle fixture/golden covering direct null
   and indirect-to-null, with an invalid predictor control that remains
   rejected.
6. Mutation-check the boundary by temporarily retaining a null key; the new
   focused tests must fail for the expected filterability mismatch.

After GREEN, run focused tests, `cargo fmt --all -- --check`, workspace clippy,
workspace tests, the qpdf compatibility gates relevant to stream decoding, and
fresh changed-line coverage at 100%.

## Documentation

Update the `QPDFStreamFilter.cc` row in `docs/qpdf-correspondence.md` to remove
the `flpdf-h8mv` known divergence and record that both readers now omit null
keys before parameter reduction. Keep the existing `ObjectHandle::try_get_keys`
mapping unchanged except for removing stale "consumed later" wording if the
implementation makes it inaccurate.

## Non-goals

- Implementing `QPDFStreamFilter::getDecodePipeline` or `QPDF_Stream::pipeStreamData`.
- Changing unknown-filter versus `/DecodeParms` length-error precedence
  (`flpdf-vatj`).
- Implementing the non-dictionary qpdf warning channel or changing public error
  text beyond the now-filterable null-key case.
- Changing TIFF predictor support, Crypt decryption, codec algorithms, output
  limits, writer behavior, or CLI behavior unrelated to the public decode API.
- Removing the legacy `Object` route; this issue keeps both existing readers in
  behavioral agreement for direct shapes until their planned cutover.

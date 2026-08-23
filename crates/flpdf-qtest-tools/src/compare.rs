//! Per-object semantic comparison matching qpdf v11.9.0's
//! `compareObjects(label, act, exp)` from `compare-for-test/qpdf-test-compare.cc`.
//!
//! Returns `Ok("")` when the two objects match, `Ok("<label>: <reason>")`
//! when they differ, or `Err(flpdf::Error)` when a stream decode fails —
//! matching qpdf's throw-on-decode-failure semantics, which the CLI turns
//! into a stderr message + exit 2 with NO stdout dump.
//!
//! Oracle: qpdf 11.9.0 `compare-for-test/qpdf-test-compare.cc:46-105`.
//! `unparseResolved` and null-entry suppression are defined by
//! `libqpdf/QPDFObjectHandle.cc:1575-1593` and
//! `libqpdf/QPDF_Dictionary.cc:59-69`.

use std::collections::BTreeSet;
use std::io::{Read, Seek};
use std::rc::Rc;

use flpdf::{Dictionary, Object, ObjectHandle, Pdf};

/// Compare two canonical [`ObjectHandle`]s the way qpdf's
/// `qpdf-test-compare` does.
///
/// Returns `Ok("")` when they match, `Ok("<label>: <reason>")` when they
/// differ. `reason` is one of qpdf's fixed set: `different types`, `object
/// contents differ`, `stream dictionaries differ`, `stream data size
/// differs`, `stream data differs`.
///
/// Returns `Err` when the stream branch fails to decode a `/FlateDecode`
/// payload on either side — mirroring qpdf's oracle, which throws from
/// `getStreamData()` and is caught by `main()` as an exit-2 error printed
/// to stderr (with no stdout output).
///
/// `actual_pdf` / `expected_pdf` are the source documents each handle came
/// from; the stream branch needs them to resolve indirect `/Filter`,
/// `/DecodeParms`, and `/Type` values. Non-stream objects are compared by
/// `ObjectHandle::unparse_resolved`, which is qpdf's `unparseResolved()`
/// shape: nested indirect handles render as `N G R` and are not walked.
///
/// Stream objects are compared by (a) their dictionaries with `/Length`
/// stripped, then (b) their data — skipped when `/Type == /XRef`,
/// decompressed via [`flpdf::filters::decode_stream_data`] when
/// `/FlateDecode` appears in `/Filter`, otherwise raw.
pub fn compare_objects<A, E>(
    label: &str,
    act: &ObjectHandle,
    exp: &ObjectHandle,
    actual_pdf: &mut Pdf<A>,
    expected_pdf: &mut Pdf<E>,
) -> flpdf::Result<String>
where
    A: Read + Seek,
    E: Read + Seek,
{
    let act = actual_pdf.resolve_object_handle_to_terminal(act)?;
    let exp = expected_pdf.resolve_object_handle_to_terminal(exp)?;
    if act.type_code()? != exp.type_code()? {
        return Ok(format!("{label}: different types"));
    }
    if act.as_stream_dict().is_some() {
        return compare_streams(label, &act, &exp, actual_pdf, expected_pdf);
    }
    let mut actual_seen = Vec::new();
    resolve_compare_children(&act, actual_pdf, &mut actual_seen, 0)?;
    let mut expected_seen = Vec::new();
    resolve_compare_children(&exp, expected_pdf, &mut expected_seen, 0)?;
    if act.unparse_resolved() != exp.unparse_resolved() {
        return Ok(format!("{label}: object contents differ"));
    }
    Ok(String::new())
}

fn compare_streams<A, E>(
    label: &str,
    act: &ObjectHandle,
    exp: &ObjectHandle,
    actual_pdf: &mut Pdf<A>,
    expected_pdf: &mut Pdf<E>,
) -> flpdf::Result<String>
where
    A: Read + Seek,
    E: Read + Seek,
{
    let act_dict = act
        .as_stream_dict()
        .ok_or_else(|| flpdf::Error::Internal("actual object lost its stream dictionary".into()))?
        .shallow_copy()?;
    let exp_dict = exp
        .as_stream_dict()
        .ok_or_else(|| flpdf::Error::Internal("expected object lost its stream dictionary".into()))?
        .shallow_copy()?;
    act_dict.remove_key(b"/Length");
    exp_dict.remove_key(b"/Length");

    // qpdf's dictionary unparse calls `isNull()` for each entry before it
    // emits the entry (`QPDF_Dictionary.cc:59-69`). Resolve dictionary
    // children on both sides for that null-suppression decision. Direct
    // containers inside arrays are walked recursively, while indirect array
    // elements remain opaque and retain their `N G R` spelling.
    let mut actual_seen = Vec::new();
    resolve_compare_children(&act_dict, actual_pdf, &mut actual_seen, 0)?;
    let mut expected_seen = Vec::new();
    resolve_compare_children(&exp_dict, expected_pdf, &mut expected_seen, 0)?;

    // qpdf compares the unparsed dictionaries before it asks the stream
    // handle to inspect /Type or /Filter. Keep indirect child handles in
    // these copies so their `N G R` spelling remains visible.
    if act_dict.unparse_resolved() != exp_dict.unparse_resolved() {
        return Ok(format!("{label}: stream dictionaries differ"));
    }

    if stream_is_xref(&act_dict, actual_pdf)? {
        return Ok(String::new());
    }
    let uncompress = stream_uses_flatedecode(&act_dict, actual_pdf)?;
    if uncompress {
        let act_data = raw_stream_data(act)?;
        let mut act_decode_dict = materialize_decode_dictionary(&act_dict, actual_pdf)?;
        // The canonical stream route has already consumed decryption while
        // producing `raw_stream_data`. qpdf's `getStreamData` likewise does
        // not send that already-consumed Crypt stage through the codec chain
        // (`QPDF_Stream.cc:345-374`, `QPDF_encryption.cc:1041-1153`).
        remove_consumed_crypt_stages(&mut act_decode_dict);
        let act_filter_handle = filter_dictionary_handle(&act_decode_dict);
        let decoded_act =
            flpdf::filters::decode_stream_data(&act_filter_handle, act_data.as_ref())?;

        let exp_data = raw_stream_data(exp)?;
        let mut exp_decode_dict = materialize_decode_dictionary(&exp_dict, expected_pdf)?;
        remove_consumed_crypt_stages(&mut exp_decode_dict);
        let exp_filter_handle = filter_dictionary_handle(&exp_decode_dict);
        let decoded_exp =
            flpdf::filters::decode_stream_data(&exp_filter_handle, exp_data.as_ref())?;
        return Ok(compare_stream_bytes(label, &decoded_act, &decoded_exp));
    }

    let act_data = raw_stream_data(act)?;
    let exp_data = raw_stream_data(exp)?;
    Ok(compare_stream_bytes(
        label,
        act_data.as_ref(),
        exp_data.as_ref(),
    ))
}

fn compare_stream_bytes(label: &str, act_bytes: &[u8], exp_bytes: &[u8]) -> String {
    if act_bytes.len() != exp_bytes.len() {
        return format!("{label}: stream data size differs");
    }
    if act_bytes != exp_bytes {
        return format!("{label}: stream data differs");
    }
    String::new()
}

fn raw_stream_data(handle: &ObjectHandle) -> flpdf::Result<Rc<Vec<u8>>> {
    handle
        .as_stream_data()
        .map_or_else(|| handle.get_raw_stream_data(), Ok)
}

fn stream_is_xref<R: Read + Seek>(
    stream_dict: &ObjectHandle,
    pdf: &mut Pdf<R>,
) -> flpdf::Result<bool> {
    let type_handle = stream_dict.get_key(b"/Type");
    let type_handle = pdf.resolve_object_handle_to_terminal(&type_handle)?;
    Ok(type_handle.as_name().is_some_and(|name| name == b"XRef"))
}

fn stream_uses_flatedecode<R: Read + Seek>(
    stream_dict: &ObjectHandle,
    pdf: &mut Pdf<R>,
) -> flpdf::Result<bool> {
    // qpdf's compare-for-test path uses `isNameAndEquals("/FlateDecode")`
    // here, so abbreviated names such as `/Fl` must stay on raw-data compare.
    Ok(resolved_filter_names_exact(stream_dict, pdf)?
        .names
        .iter()
        .any(|name| name == b"FlateDecode"))
}

struct ResolvedFilterNames {
    names: Vec<Vec<u8>>,
    valid: bool,
}

fn resolved_filter_names<R: Read + Seek>(
    stream_dict: &ObjectHandle,
    pdf: &mut Pdf<R>,
) -> flpdf::Result<ResolvedFilterNames> {
    resolved_filter_names_with_normalization(stream_dict, pdf, true)
}

fn resolved_filter_names_exact<R: Read + Seek>(
    stream_dict: &ObjectHandle,
    pdf: &mut Pdf<R>,
) -> flpdf::Result<ResolvedFilterNames> {
    resolved_filter_names_with_normalization(stream_dict, pdf, false)
}

fn resolved_filter_names_with_normalization<R: Read + Seek>(
    stream_dict: &ObjectHandle,
    pdf: &mut Pdf<R>,
    normalize: bool,
) -> flpdf::Result<ResolvedFilterNames> {
    let filter = pdf.resolve_object_handle_to_terminal(&stream_dict.get_key(b"/Filter"))?;
    if let Some(name) = filter.as_name() {
        return Ok(ResolvedFilterNames {
            names: vec![if normalize {
                normalize_filter_name(&name).to_vec()
            } else {
                name.to_vec()
            }],
            valid: is_known_filter_name(&name),
        });
    }
    let Some(items) = filter.as_array() else {
        return Ok(ResolvedFilterNames {
            names: Vec::new(),
            valid: filter.is_null(),
        });
    };
    let mut names = Vec::with_capacity(items.len());
    let mut valid = true;
    for item in items {
        let item = pdf.resolve_object_handle_to_terminal(&item)?;
        if let Some(name) = item.as_name() {
            names.push(if normalize {
                normalize_filter_name(&name).to_vec()
            } else {
                name.to_vec()
            });
            valid &= is_known_filter_name(&name);
        } else {
            // qpdf validates every Filter array item in its original
            // position (QPDF_Stream.cc:396-415). Keep an empty slot for an
            // invalid item so the legacy DecodeParms bridge cannot assign a
            // later filter's parameter dictionary to the wrong index.
            names.push(Vec::new());
            valid = false;
        }
    }
    Ok(ResolvedFilterNames { names, valid })
}

fn normalize_filter_name(name: &[u8]) -> &[u8] {
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

fn is_known_filter_name(name: &[u8]) -> bool {
    matches!(
        normalize_filter_name(name),
        b"Crypt"
            | b"FlateDecode"
            | b"LZWDecode"
            | b"RunLengthDecode"
            | b"DCTDecode"
            | b"ASCII85Decode"
            | b"ASCIIHexDecode"
    )
}

// Materialize exactly at the still-legacy filters::decode_stream_data
// boundary. Resolve the stream-level key and each array item first so the
// legacy dictionary sees the same direct names that qpdf's accessors expose.
fn materialize_decode_dictionary<R: Read + Seek>(
    stream_dict: &ObjectHandle,
    pdf: &mut Pdf<R>,
) -> flpdf::Result<Dictionary> {
    let entries = stream_dict
        .as_dictionary()
        .ok_or_else(|| flpdf::Error::Internal("stream dictionary is not a dictionary".into()))?;
    let filter_names = resolved_filter_names(stream_dict, pdf)?;
    let mut legacy = Dictionary::new();
    for (key, value) in entries {
        let value = match key.as_slice() {
            b"/Filter" => materialize_resolved_for_legacy(&value, pdf, 0)?,
            b"/DecodeParms" if filter_names.valid => {
                materialize_decode_params_for_legacy(&value, &filter_names.names, pdf)?
            }
            b"/DecodeParms" => materialize_legacy_without_resolution(&value)?,
            // `/Type` is inspected by qpdf separately for the xref-stream
            // fast path; it is not a filter decode parameter. Preserve its
            // indirect spelling here (`QPDF_Stream.cc:379-484`).
            _ => materialize_legacy_without_resolution(&value)?,
        };
        let key = key.strip_prefix(b"/").unwrap_or(key.as_slice());
        legacy.insert(key, value);
    }
    Ok(legacy)
}

/// Pass only the filter-owned keys across the canonical ObjectHandle boundary.
/// The qtest comparator keeps its materialized dictionary for warning/source
/// attribution, but codec dispatch must not call a legacy `&Dictionary` API.
fn filter_dictionary_handle(dict: &Dictionary) -> ObjectHandle {
    let mut entries = Vec::new();
    for (key, handle_key) in [
        ("Filter", b"/Filter".as_slice()),
        ("DecodeParms", b"/DecodeParms".as_slice()),
    ] {
        if let Some(value) = dict.get(key) {
            entries.push((handle_key.to_vec(), filter_value_handle(value)));
        }
    }
    ObjectHandle::dictionary(entries)
}

fn filter_value_handle(value: &Object) -> ObjectHandle {
    match value {
        Object::Null => ObjectHandle::null(),
        Object::Boolean(value) => ObjectHandle::boolean(*value),
        Object::Integer(value) => ObjectHandle::integer(*value),
        Object::Real(value) => ObjectHandle::real(*value),
        Object::RealLiteral { value, .. } => ObjectHandle::real(*value),
        Object::Name(value) => ObjectHandle::name(value.clone()),
        Object::String(value) => ObjectHandle::string(value.clone()),
        Object::Array(values) => {
            ObjectHandle::array(values.iter().map(filter_value_handle).collect())
        }
        Object::Dictionary(dictionary) => ObjectHandle::dictionary(
            dictionary
                .iter()
                .map(|(key, value)| {
                    let mut canonical = Vec::with_capacity(key.len() + 1);
                    canonical.push(b'/');
                    canonical.extend_from_slice(key);
                    (canonical, filter_value_handle(value))
                })
                .collect(),
        ),
        Object::Reference(_) | Object::Stream(_) | Object::Operator(_) | Object::InlineImage(_) => {
            ObjectHandle::boolean(false)
        }
    }
}

fn materialize_decode_params_for_legacy<R: Read + Seek>(
    handle: &ObjectHandle,
    filter_names: &[Vec<u8>],
    pdf: &mut Pdf<R>,
) -> flpdf::Result<Object> {
    let params = pdf.resolve_object_handle_to_terminal(handle)?;
    if let Some(items) = params.as_array() {
        return Ok(Object::Array(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let filters = filter_names
                        .get(index)
                        .map_or_else(Vec::new, |filter| vec![filter.as_slice()]);
                    materialize_decode_params_item(item, &filters, pdf)
                })
                .collect::<flpdf::Result<Vec<_>>>()?,
        ));
    }

    // A scalar /DecodeParms object is replicated to every filter stage by
    // qpdf (`QPDF_Stream.cc:425-438`). If several stages consume dictionary
    // entries, retain the union of the keys read by all of them.
    let filters: Vec<&[u8]> = filter_names.iter().map(Vec::as_slice).collect();
    materialize_decode_params_item(&params, &filters, pdf)
}

fn materialize_decode_params_item<R: Read + Seek>(
    handle: &ObjectHandle,
    filters: &[&[u8]],
    pdf: &mut Pdf<R>,
) -> flpdf::Result<Object> {
    let handle = pdf.resolve_object_handle_to_terminal(handle)?;
    if !filters.iter().copied().any(is_decode_parameter_consumer) {
        // Non-consuming filters only inspect whether the parameter object is
        // null. Resolve that root object, but leave all child references
        // untouched (`SF_FlateLzwDecode.cc:29-66` versus the base filter's
        // `setDecodeParms`).
        return materialize_legacy_without_resolution(&handle);
    }

    let Some(entries) = handle.as_dictionary() else {
        return materialize_legacy_without_resolution(&handle);
    };
    let allowed: BTreeSet<&'static [u8]> = filters
        .iter()
        .copied()
        .flat_map(|filter| decode_parameter_keys(filter).iter().copied())
        .collect();
    let mut selected = Dictionary::new();
    for key in allowed {
        let canonical = [b"/".as_slice(), key].concat();
        let Some(value) = entries.get(&canonical) else {
            continue;
        };
        let value = pdf.resolve_object_handle_to_terminal(value)?;
        selected.insert(key, materialize_legacy_without_resolution(&value)?);
    }
    Ok(Object::Dictionary(selected))
}

fn materialize_legacy_without_resolution(handle: &ObjectHandle) -> flpdf::Result<Object> {
    if let Some(object_ref) = handle.object_ref() {
        if !handle.is_resolved() {
            return Ok(Object::Reference(object_ref));
        }
    }
    Ok(legacyize_object(handle.materialize()?))
}

fn is_decode_parameter_consumer(name: &[u8]) -> bool {
    matches!(name, b"FlateDecode" | b"LZWDecode")
}

fn decode_parameter_keys(name: &[u8]) -> &'static [&'static [u8]] {
    if name == b"LZWDecode" {
        &[
            b"Predictor",
            b"Columns",
            b"Colors",
            b"BitsPerComponent",
            b"EarlyChange",
        ]
    } else {
        &[b"Predictor", b"Columns", b"Colors", b"BitsPerComponent"]
    }
}

fn remove_consumed_crypt_stages(dict: &mut Dictionary) {
    let Some(filter) = dict.get(b"Filter").cloned() else {
        return;
    };
    let Some(filters) = filter_names_from_legacy_object(&filter) else {
        return;
    };
    let crypt_indices: BTreeSet<usize> = filters
        .iter()
        .enumerate()
        .filter_map(|(index, name)| (name == b"Crypt").then_some(index))
        .collect();
    if crypt_indices.is_empty() {
        return;
    }

    match filter {
        Object::Name(_) => {
            dict.remove("Filter");
            dict.remove("DecodeParms");
        }
        Object::Array(values) => {
            if let Some(Object::Array(params)) = dict.get("DecodeParms") {
                // qpdf validates the original filter/parameter positions
                // before any consumed Crypt stage is removed
                // (`QPDF_Stream.cc:452-464`). Preserve the mismatch so the
                // decoder rejects the stream instead of accepting a
                // shortened, apparently aligned chain.
                // qpdf treats an empty DecodeParms array as a null scalar
                // before expanding it across the original filter list
                // (`QPDF_Stream.cc:443-445`), so only non-empty arrays can
                // represent a positional mismatch here.
                if !params.is_empty() && params.len() != filters.len() {
                    return;
                }
            }
            let remaining: Vec<Object> = values
                .into_iter()
                .enumerate()
                .filter_map(|(index, value)| (!crypt_indices.contains(&index)).then_some(value))
                .collect();
            if remaining.is_empty() {
                dict.remove("Filter");
                dict.remove("DecodeParms");
            } else {
                dict.insert("Filter", Object::Array(remaining));
                if let Some(Object::Array(params)) = dict.get("DecodeParms").cloned() {
                    if params.len() == filters.len() {
                        let params = params
                            .into_iter()
                            .enumerate()
                            .filter_map(|(index, value)| {
                                (!crypt_indices.contains(&index)).then_some(value)
                            })
                            .collect();
                        dict.insert("DecodeParms", Object::Array(params));
                    }
                }
            }
        }
        _ => {}
    }
}

fn filter_names_from_legacy_object(filter: &Object) -> Option<Vec<Vec<u8>>> {
    match filter {
        Object::Name(name) => Some(vec![normalize_filter_name(name).to_vec()]),
        Object::Array(items) => Some(
            items
                .iter()
                .map(|item| match item {
                    Object::Name(name) => Some(normalize_filter_name(name).to_vec()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?,
        ),
        _ => None,
    }
}

fn resolve_compare_children<R: Read + Seek>(
    handle: &ObjectHandle,
    pdf: &mut Pdf<R>,
    seen: &mut Vec<ObjectHandle>,
    depth: usize,
) -> flpdf::Result<()> {
    if depth > 500
        || seen
            .iter()
            .any(|ancestor| ancestor.is_same_object_as(handle))
    {
        return Ok(());
    }
    if let Some(items) = handle.as_array() {
        seen.push(handle.clone());
        for child in items {
            // qpdf's array unparse leaves indirect elements as `N G R`; only
            // direct containers can contain descendants that need a
            // dictionary null-suppression walk.
            if !child.is_direct() {
                continue;
            }
            let terminal = pdf.resolve_object_handle_to_terminal(&child)?;
            if terminal.as_dictionary().is_some() || terminal.as_array().is_some() {
                resolve_compare_children(&terminal, pdf, seen, depth + 1)?;
            }
        }
        seen.pop();
        return Ok(());
    }
    let Some(entries) = handle.as_dictionary() else {
        return Ok(());
    };
    seen.push(handle.clone());
    for child in entries.into_values() {
        let child_is_direct = child.is_direct();
        let terminal = pdf.resolve_object_handle_to_terminal(&child)?;
        if child_is_direct && (terminal.as_dictionary().is_some() || terminal.as_array().is_some())
        {
            // qpdf's dictionary unparse resolves an immediate child for its
            // null-suppression decision, but an indirect dictionary child is
            // serialized as `N G R` and its descendants remain opaque to the
            // compare-for-test walk. Recurse only through direct containers.
            resolve_compare_children(&terminal, pdf, seen, depth + 1)?;
        }
    }
    seen.pop();
    Ok(())
}

// ObjectHandle::materialize deliberately preserves an indirect child as an
// Object::Reference. That is correct for unparse and for ordinary consumers,
// but qpdf's filter accessors dereference each /Filter and /DecodeParms child
// before decoding. Perform that one semantic conversion here, with a bound
// so a malformed direct graph cannot turn this legacy bridge into unbounded
// recursion.
fn materialize_resolved_for_legacy<R: Read + Seek>(
    handle: &ObjectHandle,
    pdf: &mut Pdf<R>,
    depth: usize,
) -> flpdf::Result<Object> {
    if depth > 500 {
        return Ok(Object::Null);
    }
    let handle = pdf.resolve_object_handle_to_terminal(handle)?;
    if let Some(items) = handle.as_array() {
        return Ok(Object::Array(
            items
                .iter()
                .map(|item| materialize_resolved_for_legacy(item, pdf, depth + 1))
                .collect::<flpdf::Result<Vec<_>>>()?,
        ));
    }
    if let Some(entries) = handle.as_dictionary() {
        let mut dict = Dictionary::new();
        for (key, value) in entries {
            let key = key.strip_prefix(b"/").unwrap_or(key.as_slice());
            dict.insert(
                key,
                materialize_resolved_for_legacy(&value, pdf, depth + 1)?,
            );
        }
        return Ok(Object::Dictionary(dict));
    }
    Ok(legacyize_object(handle.materialize()?))
}

// The canonical graph stores qpdf dictionary keys with their leading slash;
// the still-legacy filters::decode_stream_data API predates that cutover and
// looks up bare names. Keep this conversion at that one boundary, including
// nested DecodeParms dictionaries, rather than teaching the canonical model a
// second slashless key representation.
fn legacyize_object(object: Object) -> Object {
    match object {
        Object::Array(values) => Object::Array(values.into_iter().map(legacyize_object).collect()),
        Object::Dictionary(dict) => Object::Dictionary(legacyize_dictionary(dict)),
        Object::Stream(mut stream) => {
            stream.dict = legacyize_dictionary(stream.dict);
            Object::Stream(stream)
        }
        scalar => scalar,
    }
}

fn legacyize_dictionary(dict: Dictionary) -> Dictionary {
    let mut legacy = Dictionary::new();
    for (key, value) in dict.iter() {
        let key = key.strip_prefix(b"/").unwrap_or(key);
        legacy.insert(key, legacyize_object(value.clone()));
    }
    legacy
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use flpdf::{Dictionary, Object, ObjectHandle, ObjectRef, Stream};
    use std::io::{Cursor, Write};
    use std::rc::Rc;

    // Reuse the flpdf-authored minimal fixture used elsewhere in the tree —
    // hand-computed xref offsets in a `const &[u8]` are too error-prone.
    // `include_bytes!` bakes the file into the test binary so the test does
    // not depend on runtime working directory. Tests with *direct* filter
    // names never exercise `resolve_stream_keys` so the Pdf's contents are
    // immaterial for those — they just need a well-formed Pdf handle.
    const MINIMAL_PDF: &[u8] = include_bytes!("../../../tests/fixtures/minimal.pdf");

    fn dummy_pdf() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open_mem_owned(MINIMAL_PDF.to_vec()).expect("open dummy PDF")
    }

    fn handle_from_object(pdf: &mut Pdf<Cursor<Vec<u8>>>, object: &Object) -> ObjectHandle {
        match object {
            Object::Null => ObjectHandle::null(),
            Object::Boolean(value) => ObjectHandle::boolean(*value),
            Object::Integer(value) => ObjectHandle::integer(*value),
            Object::Real(value) => ObjectHandle::real(*value),
            Object::RealLiteral { value, literal } => {
                ObjectHandle::real_literal(*value, literal.clone())
            }
            Object::Name(value) => ObjectHandle::name(value.clone()),
            Object::String(value) => ObjectHandle::string(value.clone()),
            Object::Operator(value) => ObjectHandle::operator(value.clone()),
            Object::InlineImage(value) => ObjectHandle::inline_image(value.clone()),
            Object::Reference(object_ref) => pdf.get_object_handle(*object_ref),
            Object::Array(values) => ObjectHandle::array(
                values
                    .iter()
                    .map(|value| handle_from_object(pdf, value))
                    .collect(),
            ),
            Object::Dictionary(dict) => ObjectHandle::dictionary(
                dict.iter()
                    .map(|(key, value)| {
                        let mut canonical_key = Vec::with_capacity(key.len() + 1);
                        canonical_key.push(b'/');
                        canonical_key.extend_from_slice(key);
                        (canonical_key, handle_from_object(pdf, value))
                    })
                    .collect(),
            ),
            Object::Stream(stream) => ObjectHandle::stream(
                handle_from_object(pdf, &Object::Dictionary(stream.dict.clone())),
                Rc::new(stream.data.clone()),
            ),
        }
    }

    /// Test helper: unwrap the `flpdf::Result<String>` from `compare_objects`.
    /// Decode failures are exercised through a dedicated Err-path test rather
    /// than every match/diff assertion, so unwrapping here is safe and keeps
    /// the assertions readable.
    fn cmp(label: &str, a: &Object, e: &Object) -> String {
        let mut a_pdf = dummy_pdf();
        let mut e_pdf = dummy_pdf();
        let a = handle_from_object(&mut a_pdf, a);
        let e = handle_from_object(&mut e_pdf, e);
        compare_objects(label, &a, &e, &mut a_pdf, &mut e_pdf)
            .expect("no decode failure in this scenario")
    }

    #[test]
    fn identical_integers_match() {
        assert_eq!(cmp("obj", &Object::Integer(42), &Object::Integer(42)), "");
    }

    #[test]
    fn different_integers_report_object_contents_differ() {
        assert_eq!(
            cmp("obj", &Object::Integer(1), &Object::Integer(2)),
            "obj: object contents differ"
        );
    }

    #[test]
    fn different_type_codes_report_different_types() {
        assert_eq!(
            cmp("obj", &Object::Integer(1), &Object::Name(b"n".to_vec())),
            "obj: different types"
        );
    }

    #[test]
    fn handle_from_object_covers_scalar_and_container_variants() {
        assert_eq!(cmp("null", &Object::Null, &Object::Null), "");
        assert_eq!(
            cmp("bool", &Object::Boolean(true), &Object::Boolean(true)),
            ""
        );
        assert_eq!(cmp("real", &Object::Real(1.25), &Object::Real(1.25)), "");
        assert_eq!(
            cmp(
                "name",
                &Object::Name(b"Name".to_vec()),
                &Object::Name(b"Name".to_vec()),
            ),
            ""
        );
        assert_eq!(
            cmp(
                "operator",
                &Object::Operator(b"q".to_vec()),
                &Object::Operator(b"q".to_vec()),
            ),
            ""
        );
        assert_eq!(
            cmp(
                "inline-image",
                &Object::InlineImage(b"BI ID EI".to_vec()),
                &Object::InlineImage(b"BI ID EI".to_vec()),
            ),
            ""
        );
        assert_eq!(
            cmp(
                "array",
                &Object::Array(vec![Object::Integer(1)]),
                &Object::Array(vec![Object::Integer(1)]),
            ),
            ""
        );
    }

    #[test]
    fn canonical_handle_compare_omits_null_dictionary_entries_like_qpdf() {
        let with_null = ObjectHandle::dictionary(vec![
            (b"/Null".to_vec(), ObjectHandle::null()),
            (b"/Value".to_vec(), ObjectHandle::integer(1)),
        ]);
        let without_null =
            ObjectHandle::dictionary(vec![(b"/Value".to_vec(), ObjectHandle::integer(1))]);
        let mut a_pdf = dummy_pdf();
        let mut e_pdf = dummy_pdf();

        assert_eq!(
            compare_objects("handle", &with_null, &without_null, &mut a_pdf, &mut e_pdf,)
                .expect("direct handle comparison must succeed"),
            "",
            "qpdf's QPDF_Dictionary::unparse omits direct null entries"
        );
    }

    #[test]
    fn canonical_handle_compare_resolves_null_children_on_both_sides() {
        let mut a_pdf = dummy_pdf();
        let mut e_pdf = dummy_pdf();
        let a_missing = a_pdf.get_object_handle(ObjectRef::new(99, 0));
        let e_missing = e_pdf.get_object_handle(ObjectRef::new(99, 0));
        let actual = ObjectHandle::dictionary(vec![(b"/Null".to_vec(), a_missing)]);
        let expected = ObjectHandle::dictionary(vec![(b"/Null".to_vec(), e_missing)]);

        // Simulate an earlier stream inspection that resolved only the
        // actual-side child. qpdf's unparseResolved resolves the same child
        // on both dictionaries before deciding whether to omit it.
        let actual_null = actual.get_key(b"/Null");
        assert!(a_pdf
            .resolve_object_handle_to_terminal(&actual_null)
            .expect("resolve actual missing child")
            .is_null());

        assert_eq!(
            compare_objects("handle", &actual, &expected, &mut a_pdf, &mut e_pdf,)
                .expect("null-child comparison must succeed"),
            "",
            "the same unresolved null child must be omitted on both sides"
        );
    }

    #[test]
    fn canonical_handle_compare_keeps_indirect_array_elements_unresolved() {
        let mut a_pdf = dummy_pdf();
        let mut e_pdf = dummy_pdf();
        let a_missing = a_pdf.get_object_handle(ObjectRef::new(99, 0));
        let e_missing = e_pdf.get_object_handle(ObjectRef::new(100, 0));
        let actual = ObjectHandle::array(vec![a_missing.clone(), ObjectHandle::integer(1)]);
        let expected = ObjectHandle::array(vec![e_missing.clone(), ObjectHandle::integer(2)]);

        assert_eq!(
            compare_objects("array", &actual, &expected, &mut a_pdf, &mut e_pdf,)
                .expect("array comparison must not visit an indirect element"),
            "array: object contents differ"
        );
        assert!(
            !a_missing.is_resolved() && !e_missing.is_resolved(),
            "qpdf array unparse keeps indirect children as references"
        );
    }

    #[test]
    fn canonical_handle_compare_resolves_nulls_in_a_direct_dictionary_inside_an_array() {
        let mut a_pdf = dummy_pdf();
        let mut e_pdf = dummy_pdf();
        let a_missing = a_pdf.get_object_handle(ObjectRef::new(99, 0));
        let e_missing = e_pdf.get_object_handle(ObjectRef::new(100, 0));
        let actual = ObjectHandle::array(vec![ObjectHandle::dictionary(vec![(
            b"/Null".to_vec(),
            a_missing,
        )])]);
        let expected = ObjectHandle::array(vec![ObjectHandle::dictionary(vec![(
            b"/Null".to_vec(),
            e_missing,
        )])]);

        assert_eq!(
            compare_objects("array-dict", &actual, &expected, &mut a_pdf, &mut e_pdf,)
                .expect("direct dictionary in an array must be comparable"),
            "",
            "qpdf recursively unparses direct dictionaries inside arrays"
        );
    }

    #[test]
    fn canonical_handle_compare_resolves_nulls_in_a_dictionary_array_value() {
        let mut a_pdf = dummy_pdf();
        let mut e_pdf = dummy_pdf();
        let a_missing = a_pdf.get_object_handle(ObjectRef::new(99, 0));
        let e_missing = e_pdf.get_object_handle(ObjectRef::new(100, 0));
        let actual = ObjectHandle::dictionary(vec![(
            b"/Nested".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::dictionary(vec![(
                b"/Null".to_vec(),
                a_missing,
            )])]),
        )]);
        let expected = ObjectHandle::dictionary(vec![(
            b"/Nested".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::dictionary(vec![(
                b"/Null".to_vec(),
                e_missing,
            )])]),
        )]);

        assert_eq!(
            compare_objects("dict-array", &actual, &expected, &mut a_pdf, &mut e_pdf,)
                .expect("array-valued dictionary must be comparable"),
            "",
            "qpdf recursively unparses direct array-valued dictionary contents"
        );
    }

    #[test]
    fn canonical_stream_compare_suppresses_null_dictionary_entries_on_both_sides() {
        let mut a_pdf = dummy_pdf();
        let mut e_pdf = dummy_pdf();
        let a_missing = a_pdf.get_object_handle(ObjectRef::new(99, 0));
        let e_missing = e_pdf.get_object_handle(ObjectRef::new(100, 0));
        let actual = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"/Unused".to_vec(), a_missing),
                (b"/Length".to_vec(), ObjectHandle::integer(3)),
            ]),
            Rc::new(b"abc".to_vec()),
        );
        let expected = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"/Unused".to_vec(), e_missing),
                (b"/Length".to_vec(), ObjectHandle::integer(3)),
            ]),
            Rc::new(b"abc".to_vec()),
        );

        assert_eq!(
            compare_objects("stream", &actual, &expected, &mut a_pdf, &mut e_pdf,)
                .expect("stream dictionary null suppression must succeed"),
            ""
        );
    }

    #[test]
    fn compare_does_not_descend_into_an_indirect_dictionary_child() {
        fn build_pdf_and_nested_handle(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> ObjectHandle {
            let nested_ref = ObjectRef::new(10, 0);
            let chain_start = ObjectRef::new(11, 0);
            let mut nested = Dictionary::new();
            nested.insert(b"Bad", Object::Reference(chain_start));
            pdf.set_object(nested_ref, Object::Dictionary(nested));

            let mut current = chain_start;
            for number in 0..70 {
                let next = ObjectRef::new(100 + number, 0);
                pdf.set_object(current, Object::Reference(next));
                current = next;
            }
            pdf.set_object(current, Object::Integer(1));

            let nested_handle = pdf.get_object_handle(nested_ref);
            ObjectHandle::dictionary(vec![(b"/Nested".to_vec(), nested_handle)])
        }

        let mut actual_pdf = dummy_pdf();
        let mut expected_pdf = dummy_pdf();
        let actual = build_pdf_and_nested_handle(&mut actual_pdf);
        let expected = build_pdf_and_nested_handle(&mut expected_pdf);
        let actual_before = actual_pdf.repair_diagnostics().entries().len();
        let expected_before = expected_pdf.repair_diagnostics().entries().len();

        assert_eq!(
            compare_objects(
                "nested",
                &actual,
                &expected,
                &mut actual_pdf,
                &mut expected_pdf,
            )
            .expect("an indirect child dictionary is still comparable"),
            ""
        );
        assert_eq!(
            actual_pdf.repair_diagnostics().entries().len(),
            actual_before,
            "qpdf compares the indirect child as N G R without resolving its descendants"
        );
        assert_eq!(
            expected_pdf.repair_diagnostics().entries().len(),
            expected_before,
            "qpdf compares the indirect child as N G R without resolving its descendants"
        );
    }

    #[test]
    fn canonical_handle_compare_resolves_indirect_filter_array_items() {
        let source = b"handle filter array";
        let compressed_a = zlib(source, Compression::none());
        let compressed_e = zlib(source, Compression::best());
        let mut a_pdf = dummy_pdf();
        let mut e_pdf = dummy_pdf();
        a_pdf.set_object(ObjectRef::new(1, 0), Object::Name(b"FlateDecode".to_vec()));
        e_pdf.set_object(ObjectRef::new(1, 0), Object::Name(b"FlateDecode".to_vec()));
        let a_filter = a_pdf.get_object_handle(ObjectRef::new(1, 0));
        let e_filter = e_pdf.get_object_handle(ObjectRef::new(1, 0));
        let a_dict = ObjectHandle::dictionary(vec![
            (b"/Filter".to_vec(), ObjectHandle::array(vec![a_filter])),
            (
                b"/Length".to_vec(),
                ObjectHandle::integer(compressed_a.len() as i64),
            ),
        ]);
        let e_dict = ObjectHandle::dictionary(vec![
            (b"/Filter".to_vec(), ObjectHandle::array(vec![e_filter])),
            (
                b"/Length".to_vec(),
                ObjectHandle::integer(compressed_e.len() as i64),
            ),
        ]);
        let actual = ObjectHandle::stream(a_dict, Rc::new(compressed_a));
        let expected = ObjectHandle::stream(e_dict, Rc::new(compressed_e));

        assert_eq!(
            compare_objects("handle stream", &actual, &expected, &mut a_pdf, &mut e_pdf,)
                .expect("indirect filter array items must decode"),
            "",
            "qpdf's filter inspection dereferences array items before isName"
        );
    }

    #[test]
    fn legacy_decode_boundary_normalizes_nested_keys_and_container_values() {
        let mut pdf = dummy_pdf();
        let stream_dict = ObjectHandle::dictionary(vec![
            (
                b"/Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
            (
                b"/DecodeParms".to_vec(),
                ObjectHandle::dictionary(vec![(b"/Predictor".to_vec(), ObjectHandle::integer(1))]),
            ),
            (
                b"/Other".to_vec(),
                ObjectHandle::array(vec![ObjectHandle::integer(7)]),
            ),
        ]);

        let legacy = materialize_decode_dictionary(&stream_dict, &mut pdf)
            .expect("legacy decode boundary materializes nested values");
        let parms = legacy
            .get(b"DecodeParms")
            .and_then(Object::as_dict)
            .expect("DecodeParms uses legacy key spelling");
        assert_eq!(
            parms.get(b"Predictor").and_then(Object::as_integer),
            Some(1)
        );
        let other = legacy
            .get(b"Other")
            .and_then(Object::as_array)
            .expect("ordinary container values are legacyized");
        assert_eq!(other[0].as_integer(), Some(7));
    }

    #[test]
    fn legacy_decode_boundary_resolves_only_flate_parameters() {
        let mut pdf = dummy_pdf();
        let missing = pdf.get_object_handle(ObjectRef::new(99, 0));
        let stream_dict = ObjectHandle::dictionary(vec![
            (
                b"/Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
            (
                b"/DecodeParms".to_vec(),
                ObjectHandle::dictionary(vec![
                    (b"/Predictor".to_vec(), ObjectHandle::integer(1)),
                    (b"/Unused".to_vec(), missing),
                ]),
            ),
        ]);

        let legacy = materialize_decode_dictionary(&stream_dict, &mut pdf)
            .expect("only consumed Flate parameters need resolution");
        let parms = legacy
            .get(b"DecodeParms")
            .and_then(Object::as_dict)
            .expect("DecodeParms dictionary remains present");
        assert_eq!(
            parms.get(b"Predictor").and_then(Object::as_integer),
            Some(1)
        );
        assert!(
            parms.get(b"Unused").is_none(),
            "an unknown Flate parameter must not resolve its indirect value"
        );
    }

    #[test]
    fn legacy_decode_boundary_unions_scalar_parameters_across_filters() {
        let mut pdf = dummy_pdf();
        let stream_dict = ObjectHandle::dictionary(vec![
            (
                b"/Filter".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                    ObjectHandle::name(b"LZWDecode".to_vec()),
                ]),
            ),
            (
                b"/DecodeParms".to_vec(),
                ObjectHandle::dictionary(vec![
                    (b"/Predictor".to_vec(), ObjectHandle::integer(1)),
                    (b"/EarlyChange".to_vec(), ObjectHandle::integer(0)),
                ]),
            ),
        ]);

        let legacy = materialize_decode_dictionary(&stream_dict, &mut pdf)
            .expect("scalar DecodeParms must use every consuming filter's keys");
        let parms = legacy
            .get(b"DecodeParms")
            .and_then(Object::as_dict)
            .expect("DecodeParms dictionary remains present");
        assert_eq!(
            parms.get(b"Predictor").and_then(Object::as_integer),
            Some(1)
        );
        assert_eq!(
            parms.get(b"EarlyChange").and_then(Object::as_integer),
            Some(0)
        );
    }

    #[test]
    fn legacy_decode_boundary_keeps_decode_params_shallow_for_invalid_filters() {
        let mut pdf = dummy_pdf();
        let second_missing = pdf.get_object_handle(ObjectRef::new(99, 0));
        let third_missing = pdf.get_object_handle(ObjectRef::new(100, 0));
        let stream_dict = ObjectHandle::dictionary(vec![
            (
                b"/Filter".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                    ObjectHandle::integer(7),
                    ObjectHandle::name(b"LZWDecode".to_vec()),
                ]),
            ),
            (
                b"/DecodeParms".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::dictionary(vec![(
                        b"/Predictor".to_vec(),
                        ObjectHandle::integer(1),
                    )]),
                    ObjectHandle::dictionary(vec![(b"/EarlyChange".to_vec(), second_missing)]),
                    ObjectHandle::dictionary(vec![(
                        b"/EarlyChange".to_vec(),
                        third_missing.clone(),
                    )]),
                ]),
            ),
        ]);

        let legacy = materialize_decode_dictionary(&stream_dict, &mut pdf)
            .expect("invalid intermediate Filter item still reaches the legacy boundary");
        let params = legacy
            .get(b"DecodeParms")
            .and_then(Object::as_array)
            .expect("DecodeParms array remains positional");
        assert_eq!(params.len(), 3);
        assert!(matches!(
            params[1]
                .as_dict()
                .and_then(|dict| dict.get(b"EarlyChange")),
            Some(Object::Reference(ObjectRef {
                number: 99,
                generation: 0
            }))
        ));
        assert_eq!(
            params[2]
                .as_dict()
                .and_then(|dict| dict.get(b"EarlyChange")),
            Some(&Object::Reference(ObjectRef {
                number: 100,
                generation: 0
            }))
        );
        assert!(
            !third_missing.is_resolved(),
            "an invalid Filter chain must not resolve later DecodeParms children"
        );
    }

    #[test]
    fn legacy_decode_boundary_does_not_resolve_stream_type() {
        let mut pdf = dummy_pdf();
        let missing = pdf.get_object_handle(ObjectRef::new(99, 0));
        let stream_dict = ObjectHandle::dictionary(vec![
            (
                b"/Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
            (b"/Type".to_vec(), missing),
        ]);

        let legacy = materialize_decode_dictionary(&stream_dict, &mut pdf)
            .expect("stream type is not consumed by the decoder");
        assert!(matches!(
            legacy.get(b"Type"),
            Some(Object::Reference(ObjectRef {
                number: 99,
                generation: 0
            }))
        ));
    }

    #[test]
    fn consumed_crypt_stages_are_removed_with_aligned_decode_params() {
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"Crypt".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"Crypt".to_vec()),
            ]),
        );
        dict.insert(
            "DecodeParms",
            Object::Array(vec![
                Object::Dictionary(Dictionary::new()),
                Object::Dictionary(Dictionary::new()),
                Object::Dictionary(Dictionary::new()),
            ]),
        );

        remove_consumed_crypt_stages(&mut dict);

        assert_eq!(
            dict.get("Filter"),
            Some(&Object::Array(vec![Object::Name(b"FlateDecode".to_vec())]))
        );
        assert_eq!(
            dict.get("DecodeParms"),
            Some(&Object::Array(vec![Object::Dictionary(Dictionary::new())]))
        );
    }

    #[test]
    fn consumed_crypt_stage_keeps_misaligned_decode_params() {
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"Crypt".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ]),
        );
        dict.insert(
            "DecodeParms",
            Object::Array(vec![Object::Dictionary(Dictionary::new())]),
        );

        remove_consumed_crypt_stages(&mut dict);

        assert_eq!(
            dict.get("Filter"),
            Some(&Object::Array(vec![
                Object::Name(b"Crypt".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ]))
        );
        assert_eq!(
            dict.get("DecodeParms"),
            Some(&Object::Array(vec![Object::Dictionary(Dictionary::new())]))
        );
    }

    #[test]
    fn legacy_bridge_handles_stream_values_and_depth_guard() {
        let mut pdf = dummy_pdf();
        let nested = ObjectHandle::dictionary(vec![(
            b"/Array".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::integer(3)]),
        )]);
        let resolved = materialize_resolved_for_legacy(&nested, &mut pdf, 0)
            .expect("nested dictionary resolves at the legacy boundary");
        let resolved = resolved.as_dict().expect("nested dictionary materializes");
        assert!(resolved.get(b"Array").and_then(Object::as_array).is_some());

        let mut stream_dict = Dictionary::new();
        stream_dict.insert(b"/Length", Object::Integer(0));
        let legacy_stream = legacyize_object(Object::Stream(Stream::new(stream_dict, Vec::new())));
        let legacy_stream = legacy_stream.as_stream().expect("stream remains a stream");
        assert!(legacy_stream.dict.get(b"Length").is_some());

        let mut dictionary = Dictionary::new();
        dictionary.insert(b"/Nested", Object::Integer(4));
        let legacy_dictionary = legacyize_object(Object::Dictionary(dictionary));
        let legacy_dictionary = legacy_dictionary
            .as_dict()
            .expect("dictionary remains a dictionary");
        assert_eq!(
            legacy_dictionary
                .get(b"Nested")
                .and_then(Object::as_integer),
            Some(4)
        );

        let bounded = materialize_resolved_for_legacy(&ObjectHandle::integer(1), &mut pdf, 501)
            .expect("depth guard returns a null value");
        assert!(matches!(bounded, Object::Null));
    }

    #[test]
    fn equal_dictionaries_with_nested_references_match() {
        // Nested `Object::Reference` renders as "N G R" via `write_pdf` and
        // is NOT further dereferenced. Two dicts with the same shape and the
        // same references therefore serialize identically.
        let build = || {
            let mut d = Dictionary::new();
            d.insert(b"Type", Object::Name(b"Catalog".to_vec()));
            d.insert(b"Pages", Object::Reference(ObjectRef::new(3, 0)));
            d.insert(b"Version", Object::Name(b"1.7".to_vec()));
            Object::Dictionary(d)
        };
        assert_eq!(cmp("obj", &build(), &build()), "");
    }

    #[test]
    fn dictionary_insert_order_does_not_matter() {
        // `Dictionary` is BTreeMap-backed so `write_pdf` output is sorted by
        // key. Two dicts populated in different orders still compare equal.
        let mut a = Dictionary::new();
        a.insert(b"A", Object::Integer(1));
        a.insert(b"B", Object::Integer(2));
        a.insert(b"C", Object::Integer(3));
        let mut b = Dictionary::new();
        b.insert(b"C", Object::Integer(3));
        b.insert(b"A", Object::Integer(1));
        b.insert(b"B", Object::Integer(2));
        assert_eq!(
            cmp("obj", &Object::Dictionary(a), &Object::Dictionary(b)),
            ""
        );
    }

    #[test]
    fn reference_vs_integer_report_different_types() {
        assert_eq!(
            cmp(
                "obj",
                &Object::Reference(ObjectRef::new(3, 0)),
                &Object::Integer(3),
            ),
            "obj: different types"
        );
    }

    #[test]
    fn equal_strings_match() {
        assert_eq!(
            cmp(
                "obj",
                &Object::String(b"hello".to_vec()),
                &Object::String(b"hello".to_vec()),
            ),
            ""
        );
    }

    #[test]
    fn real_and_real_literal_with_equal_write_bytes_match() {
        // `Object::Real(1.5)` and `Object::RealLiteral { value: 1.5, literal:
        // b"1.5" }` are NOT equal under `PartialEq` (different enum variants)
        // but both `write_pdf` to `b"1.5"`. A qpdf-parity implementation must
        // treat them as equal (matches qpdf's `unparse`-based comparison).
        let real = Object::Real(1.5);
        let real_literal = Object::RealLiteral {
            value: 1.5,
            literal: b"1.5".to_vec(),
        };
        // Sanity: PartialEq disagrees but write_pdf agrees.
        assert_ne!(real, real_literal);
        let (mut a, mut b) = (Vec::new(), Vec::new());
        real.write_pdf(&mut a);
        real_literal.write_pdf(&mut b);
        assert_eq!(a, b);

        // Both are PDF reals → same type_code → falls into the write-bytes
        // compare path, which reports no diff.
        assert_eq!(cmp("obj", &real, &real_literal), "");
    }

    // ---------- stream branch ----------

    fn zlib(bytes: &[u8], level: Compression) -> Vec<u8> {
        let mut e = ZlibEncoder::new(Vec::new(), level);
        e.write_all(bytes).unwrap();
        e.finish().unwrap()
    }

    fn raw_stream(len: i64, data: Vec<u8>) -> Stream {
        let mut d = Dictionary::new();
        d.insert(b"Length", Object::Integer(len));
        Stream::new(d, data)
    }

    #[test]
    fn identical_streams_match() {
        let a = Object::Stream(raw_stream(10, b"0123456789".to_vec()));
        let e = Object::Stream(raw_stream(10, b"0123456789".to_vec()));
        assert_eq!(cmp("1 0", &a, &e), "");
    }

    #[test]
    fn stream_length_only_diff_matches_when_data_equal() {
        // Same raw data but different /Length values → Length is stripped
        // before dict compare, so the dicts compare equal and the raw data
        // compare succeeds. (Yes, /Length disagreeing with data length is
        // "invalid" PDF, but the compare tool must not care — that's the
        // whole point of stripping it.)
        let a = Object::Stream(raw_stream(1, b"same".to_vec()));
        let e = Object::Stream(raw_stream(999, b"same".to_vec()));
        assert_eq!(cmp("2 0", &a, &e), "");
    }

    #[test]
    fn stream_dict_type_diff_reports_stream_dictionaries_differ() {
        let mut ad = Dictionary::new();
        ad.insert(b"Length", Object::Integer(3));
        ad.insert(b"Type", Object::Name(b"Foo".to_vec()));
        let mut ed = Dictionary::new();
        ed.insert(b"Length", Object::Integer(3));
        ed.insert(b"Type", Object::Name(b"Bar".to_vec()));
        let a = Object::Stream(Stream::new(ad, b"abc".to_vec()));
        let e = Object::Stream(Stream::new(ed, b"abc".to_vec()));
        assert_eq!(cmp("3 0", &a, &e), "3 0: stream dictionaries differ");
    }

    #[test]
    fn xref_stream_skips_data_compare() {
        // /Type /XRef with the same dict but wildly differing data should
        // still match — qpdf skips xref-stream data validation entirely.
        // The two sides only differ in .data (raw payload); /Length is set
        // to the same placeholder on both sides so the pre-strip dict-bytes
        // compare doesn't reveal the payload difference through /Length.
        let make = |data: Vec<u8>| {
            let mut d = Dictionary::new();
            d.insert(b"Type", Object::Name(b"XRef".to_vec()));
            d.insert(b"Length", Object::Integer(0));
            Object::Stream(Stream::new(d, data))
        };
        let a = make(b"totally different bytes".to_vec());
        let e = make(b"and yet still matches".to_vec());
        assert_eq!(cmp("4 0", &a, &e), "");
    }

    #[test]
    fn flate_same_decoded_different_compressed_matches() {
        // Same source payload, encoded at different compression levels →
        // compressed bytes differ, decoded bytes match, `/FlateDecode` in
        // /Filter routes through decompress path.
        let source = b"the quick brown fox jumps over the lazy dog. \
                       the quick brown fox jumps over the lazy dog. \
                       the quick brown fox jumps over the lazy dog.";
        let compressed_a = zlib(source, Compression::none());
        let compressed_e = zlib(source, Compression::best());
        assert_ne!(
            compressed_a, compressed_e,
            "test premise: compressed bytes differ"
        );

        let make = |data: Vec<u8>| {
            let mut d = Dictionary::new();
            d.insert(b"Filter", Object::Name(b"FlateDecode".to_vec()));
            d.insert(b"Length", Object::Integer(data.len() as i64));
            Object::Stream(Stream::new(d, data))
        };
        let a = make(compressed_a);
        let e = make(compressed_e);
        assert_eq!(cmp("5 0", &a, &e), "");
    }

    #[test]
    fn abbreviated_flate_filter_stays_on_raw_compare_path() {
        let source = b"the abbreviated Flate filter must not select qpdf's decoded comparison path";
        let compressed_a = zlib(source, Compression::fast());
        let compressed_e = zlib(source, Compression::best());
        assert_ne!(
            compressed_a, compressed_e,
            "test premise: compressed bytes differ"
        );

        let make = |data: Vec<u8>| {
            let mut d = Dictionary::new();
            d.insert(b"Filter", Object::Name(b"Fl".to_vec()));
            d.insert(b"Length", Object::Integer(data.len() as i64));
            Object::Stream(Stream::new(d, data))
        };
        assert_eq!(
            cmp("abbreviated", &make(compressed_a), &make(compressed_e)),
            "abbreviated: stream data differs"
        );
    }

    #[test]
    fn filter_array_containing_flatedecode_triggers_decompress() {
        // Direct unit test of the detector (rather than crafting a genuine
        // multi-filter round-trip in an e2e test). An Array /Filter whose
        // first element is /FlateDecode must route through the decompress
        // path.
        let mut pdf = dummy_pdf();
        let d = ObjectHandle::dictionary(vec![(
            b"/Filter".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::name(b"FlateDecode".to_vec()),
                ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
            ]),
        )]);
        assert!(
            stream_uses_flatedecode(&d, &mut pdf).expect("direct filter array is readable"),
            "FlateDecode-first Array must trigger decompress"
        );
        // And a positional variant: FlateDecode not first.
        let d2 = ObjectHandle::dictionary(vec![(
            b"/Filter".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ]),
        )]);
        assert!(
            stream_uses_flatedecode(&d2, &mut pdf).expect("direct filter array is readable"),
            "FlateDecode anywhere in Array must trigger decompress"
        );
        // Negative: no FlateDecode.
        let d3 = ObjectHandle::dictionary(vec![(
            b"/Filter".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::name(b"ASCIIHexDecode".to_vec())]),
        )]);
        assert!(!stream_uses_flatedecode(&d3, &mut pdf).unwrap());
    }

    #[test]
    fn non_flate_filter_with_diff_raw_reports_stream_data_differs() {
        // /Filter /ASCIIHexDecode is not FlateDecode → raw compare, same
        // size but different bytes → "stream data differs".
        let make = |data: Vec<u8>| {
            let mut d = Dictionary::new();
            d.insert(b"Filter", Object::Name(b"ASCIIHexDecode".to_vec()));
            d.insert(b"Length", Object::Integer(data.len() as i64));
            Object::Stream(Stream::new(d, data))
        };
        // Same length, different content (hex-alphabet bytes so nothing
        // downstream is tempted to decode them).
        let a = make(b"41 42 43>".to_vec());
        let e = make(b"44 45 46>".to_vec());
        assert_eq!(cmp("6 0", &a, &e), "6 0: stream data differs");
    }

    #[test]
    fn indirect_flate_filter_reference_routes_through_decompress() {
        // Regression: qpdf's `QPDFObjectHandle::isName()` auto-dereferences,
        // so `/Filter 1 0 R` (where object 1 0 is `/FlateDecode`) still
        // routes through `getStreamData()`. Our detector must do the same,
        // otherwise two streams whose /Filter is the SAME reference but
        // whose zlib bytes differ would be reported as "stream data size
        // differs" instead of matching.
        let source = b"hello indirect filter world";
        let make = |data: Vec<u8>| {
            let mut d = Dictionary::new();
            d.insert(b"Filter", Object::Reference(ObjectRef::new(1, 0)));
            d.insert(b"Length", Object::Integer(0));
            Object::Stream(Stream::new(d, data))
        };
        let a = make(zlib(source, Compression::none()));
        let e = make(zlib(source, Compression::best()));

        // Both dummy pdfs have object 1 0 (the Catalog in MINIMAL_PDF).
        // Overwrite the cached value so `resolve` returns Name("FlateDecode")
        // — this is fine here because we never write the pdf out.
        let mut a_pdf = dummy_pdf();
        a_pdf.set_object(ObjectRef::new(1, 0), Object::Name(b"FlateDecode".to_vec()));
        let mut e_pdf = dummy_pdf();
        e_pdf.set_object(ObjectRef::new(1, 0), Object::Name(b"FlateDecode".to_vec()));

        let a = handle_from_object(&mut a_pdf, &a);
        let e = handle_from_object(&mut e_pdf, &e);
        assert_eq!(
            compare_objects("indirect", &a, &e, &mut a_pdf, &mut e_pdf)
                .expect("resolution + decode must succeed"),
            "",
            "indirect /Filter reference must resolve to Name and decode"
        );
    }

    #[test]
    fn flate_decode_failure_propagates_as_err() {
        // Both sides claim /FlateDecode but the payload is not a valid zlib
        // stream, so `decode_stream_data` returns Err. Our `compare_objects`
        // must propagate the Err (matching qpdf's throw-and-catch → stderr +
        // exit 2 with NO stdout dump). If we swallowed it into a diff string,
        // main would cat the actual file to stdout, which could be mistaken
        // for a genuine mismatch of the actual file.
        let make = |data: Vec<u8>| {
            let mut d = Dictionary::new();
            d.insert(b"Filter", Object::Name(b"FlateDecode".to_vec()));
            d.insert(b"Length", Object::Integer(data.len() as i64));
            Object::Stream(Stream::new(d, data))
        };
        // Not a zlib stream — the decoder must reject.
        let bogus = b"\x00\x01\x02not zlib\xff\xff\xff".to_vec();
        let a = make(bogus.clone());
        let e = make(bogus);
        let mut a_pdf = dummy_pdf();
        let mut e_pdf = dummy_pdf();
        let a = handle_from_object(&mut a_pdf, &a);
        let e = handle_from_object(&mut e_pdf, &e);
        assert!(compare_objects("8 0", &a, &e, &mut a_pdf, &mut e_pdf).is_err());
    }

    #[test]
    fn actual_decode_failure_precedes_expected_filter_resolution() {
        let mut actual_pdf = dummy_pdf();
        let mut actual_params = Dictionary::new();
        actual_params.insert(b"Predictor", Object::Integer(1));
        actual_pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(actual_params));
        let actual = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (
                    b"/Filter".to_vec(),
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                ),
                (
                    b"/DecodeParms".to_vec(),
                    actual_pdf.get_object_handle(ObjectRef::new(1, 0)),
                ),
            ]),
            Rc::new(b"\x00\x01\x02not zlib\xff\xff\xff".to_vec()),
        );

        let mut expected_pdf = dummy_pdf();
        let chain_start = ObjectRef::new(11, 0);
        let mut current = chain_start;
        for number in 0..70 {
            let next = ObjectRef::new(100 + number, 0);
            expected_pdf.set_object(current, Object::Reference(next));
            current = next;
        }
        expected_pdf.set_object(current, Object::Integer(1));
        let mut expected_params = Dictionary::new();
        expected_params.insert(b"Predictor", Object::Reference(chain_start));
        expected_pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(expected_params));
        let expected = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (
                    b"/Filter".to_vec(),
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                ),
                (
                    b"/DecodeParms".to_vec(),
                    expected_pdf.get_object_handle(ObjectRef::new(1, 0)),
                ),
            ]),
            Rc::new(zlib(b"expected payload", Compression::default())),
        );
        let expected_before = expected_pdf.repair_diagnostics().entries().len();

        assert!(
            compare_objects(
                "actual-first",
                &actual,
                &expected,
                &mut actual_pdf,
                &mut expected_pdf,
            )
            .is_err(),
            "the actual corrupt payload must fail the comparison"
        );
        assert_eq!(
            expected_pdf.repair_diagnostics().entries().len(),
            expected_before,
            "qpdf does not prepare the expected stream after an actual decode failure"
        );
    }

    #[test]
    fn uncompressed_size_diff_reports_stream_data_size_differs() {
        // No /Filter, different payload lengths, /Length stripped → matching
        // dicts, then the size-differs branch fires before the byte compare.
        let mut ad = Dictionary::new();
        ad.insert(b"Length", Object::Integer(3));
        let mut ed = Dictionary::new();
        ed.insert(b"Length", Object::Integer(4));
        let a = Object::Stream(Stream::new(ad, b"abc".to_vec()));
        let e = Object::Stream(Stream::new(ed, b"abcd".to_vec()));
        assert_eq!(cmp("7 0", &a, &e), "7 0: stream data size differs");
    }
}

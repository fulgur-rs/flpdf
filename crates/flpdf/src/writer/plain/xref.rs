//! qpdf correspondence: QPDFWriter.cc classic and stream xref emission for the plain writer.
use std::collections::BTreeMap;

use crate::writer::{serialize::xref_stream, write_deterministic_id_inline};
#[cfg(test)]
use crate::{Dictionary, Object};
use crate::{ObjectHandle, ObjectRef, XrefEntry, XrefForm};

/// Location of an object encoded inside an object-stream container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CompressedLocation {
    pub(crate) container: u32,
    pub(crate) index: u32,
}

/// Physical locations of the objects already written into a plain PDF body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BodyLayout {
    pub(crate) uncompressed: BTreeMap<u32, (u16, usize)>,
    pub(crate) compressed: BTreeMap<u32, CompressedLocation>,
}

impl BodyLayout {
    pub(crate) fn validate(&self) -> crate::Result<()> {
        for number in self.uncompressed.keys() {
            if self.compressed.contains_key(number) {
                return Err(crate::Error::Unsupported(format!(
                    "plain writer layout: object {number} is both uncompressed and compressed"
                )));
            }
        }
        Ok(())
    }

    fn max_number(&self) -> u32 {
        self.uncompressed
            .keys()
            .chain(self.compressed.keys())
            .copied()
            .max()
            .unwrap_or(0)
    }
}

/// How the trailer `/ID` is provided while its bytes are assembled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum IdPlan {
    Materialized {
        value: Option<(Vec<u8>, Vec<u8>)>,
    },
    Deterministic {
        source_id0: Option<Vec<u8>>,
        info_suffix: Vec<u8>,
    },
}

/// Inputs needed to assemble an xref section after a plain body has been emitted.
#[derive(Clone, Debug)]
pub(crate) struct TrailerPlan {
    pub(crate) form: XrefForm,
    /// Canonical trailer entries from the live ObjectHandle graph. Keys remain
    /// decoded until emission so qpdf's raw-name sort is preserved.
    pub(crate) canonical_entries: Vec<(Vec<u8>, Vec<u8>)>,
    pub(crate) root: ObjectRef,
    pub(crate) id: IdPlan,
    pub(crate) encrypt: Option<ObjectRef>,
    pub(crate) structural_filtered: bool,
}

/// Append a classic xref table or xref stream for an already-written body.
pub(crate) fn append_xref_and_trailer(
    bytes: &mut Vec<u8>,
    layout: &BodyLayout,
    trailer: &TrailerPlan,
) -> crate::Result<BTreeMap<ObjectRef, XrefEntry>> {
    layout.validate()?;

    match trailer.form {
        XrefForm::Table if layout.compressed.is_empty() => {
            append_classic_xref_and_trailer(bytes, layout, trailer)
        }
        XrefForm::Table => Err(crate::Error::Unsupported(
            "plain writer classic xref cannot represent compressed objects".into(),
        )),
        XrefForm::Stream => append_xref_stream_and_trailer(bytes, layout, trailer),
    }
}

fn append_xref_stream_and_trailer(
    bytes: &mut Vec<u8>,
    layout: &BodyLayout,
    trailer: &TrailerPlan,
) -> crate::Result<BTreeMap<ObjectRef, XrefEntry>> {
    let xref_offset = bytes.len();
    let max_number = layout.max_number();
    let xref_number = max_number.checked_add(1).ok_or_else(|| {
        crate::Error::Unsupported("plain writer xref object number overflows u32".into())
    })?;
    let size = xref_number
        .checked_add(1)
        .ok_or_else(|| crate::Error::Unsupported("plain writer /Size overflows u32".into()))?;

    let mut offsets: BTreeMap<u32, usize> = layout
        .uncompressed
        .iter()
        .map(|(&number, &(_, offset))| (number, offset))
        .collect();
    offsets.insert(xref_number, xref_offset);
    let members: BTreeMap<u32, (u32, u32)> = layout
        .compressed
        .iter()
        .map(|(&number, location)| (number, (location.container, location.index)))
        .collect();
    let mut entries = xref_stream::build_entries(&offsets, &members, 0, size);
    for (&number, &(generation, _)) in &layout.uncompressed {
        entries[number as usize].field3 = u64::from(generation);
    }
    let max_generation = layout
        .uncompressed
        .values()
        .map(|&(generation, _)| u64::from(generation))
        .max()
        .unwrap_or(0);
    let max_member_index = members
        .values()
        .map(|&(_, index)| u64::from(index))
        .max()
        .unwrap_or(0);
    let widths = xref_stream::second_pass_widths(
        xref_stream::max_entry_offset(&entries),
        0,
        max_number,
        max_generation.max(max_member_index),
    );
    let payload = if trailer.structural_filtered {
        xref_stream::encode_payload(&entries, widths)?
    } else {
        xref_stream::encode_payload_raw(&entries, widths)?
    };

    let dictionary = xref_stream::XrefStreamDict {
        filtered: trailer.structural_filtered,
        widths,
        index: None,
        info: None,
        root: Some(trailer.root),
        size,
        prev: None,
        trailer: None,
        canonical_entries: Some(&trailer.canonical_entries),
        id: None,
        encrypt: trailer.encrypt,
    };
    let xref_ref = ObjectRef::new(xref_number, 0);
    match &trailer.id {
        IdPlan::Materialized { value } => {
            let id = value
                .as_ref()
                .map(|(id0, id1)| (id0.as_slice(), id1.as_slice()));
            let dictionary = xref_stream::XrefStreamDict { id, ..dictionary };
            xref_stream::write_object(bytes, xref_ref, &dictionary, &payload);
        }
        IdPlan::Deterministic {
            source_id0,
            info_suffix,
        } => {
            let mut id_writer = |out: &mut Vec<u8>| {
                write_deterministic_id_inline(out, info_suffix, source_id0.as_deref())
            };
            xref_stream::write_object_with_id_writer(
                bytes,
                xref_ref,
                &dictionary,
                &payload,
                &mut id_writer,
            );
        }
    }
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    written_xref_stream(layout, xref_ref, xref_offset)
}

#[cfg(test)]
pub(crate) fn materialized_id(
    dictionary: &Dictionary,
) -> crate::Result<Option<(Vec<u8>, Vec<u8>)>> {
    let Some(id) = dictionary.get("ID") else {
        return Ok(None);
    };
    match id {
        Object::Array(values) => match values.as_slice() {
            [Object::String(id0), Object::String(id1)] => Ok(Some((id0.clone(), id1.clone()))),
            _ => Err(crate::Error::Unsupported(
                "plain writer materialized /ID must contain two strings".into(),
            )),
        },
        _ => Err(crate::Error::Unsupported(
            "plain writer materialized /ID must be an array".into(),
        )),
    }
}

/// Read the writer-owned `/ID` value from the canonical handle graph.
///
/// qpdf's trailer writer accepts an absent `/ID`, but when an identifier is
/// present it must be exactly an array of two string values before the xref
/// layer can emit it (`QPDFWriter.cc:1160-1236`). The handle is resolved
/// lazily so indirect `/ID` values follow the same route as every other
/// writer trailer entry.
pub(crate) fn materialized_id_handle(
    id: &ObjectHandle,
) -> crate::Result<Option<(Vec<u8>, Vec<u8>)>> {
    if id.try_is_null()? {
        return Ok(None);
    }
    let Some(values) = id.try_as_array()? else {
        return Err(crate::Error::Unsupported(
            "plain writer materialized /ID must be an array".into(),
        ));
    };
    let [id0, id1] = values.as_slice() else {
        return Err(crate::Error::Unsupported(
            "plain writer materialized /ID must contain two strings".into(),
        ));
    };
    id0.try_dereference()?;
    id1.try_dereference()?;
    match (id0.as_string(), id1.as_string()) {
        (Some(id0), Some(id1)) => Ok(Some((id0, id1))),
        _ => Err(crate::Error::Unsupported(
            "plain writer materialized /ID must contain two strings".into(),
        )),
    }
}

fn append_classic_xref_and_trailer(
    bytes: &mut Vec<u8>,
    layout: &BodyLayout,
    trailer: &TrailerPlan,
) -> crate::Result<BTreeMap<ObjectRef, XrefEntry>> {
    let xref_offset = bytes.len();
    let size = layout
        .max_number()
        .checked_add(1)
        .ok_or_else(|| crate::Error::Unsupported("plain writer /Size overflows u32".into()))?;
    if layout
        .uncompressed
        .values()
        .any(|&(_, offset)| offset as u64 >= 10_000_000_000)
    {
        return Err(crate::Error::Unsupported(
            "plain writer classic xref offset exceeds ten digits".into(),
        ));
    }

    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for number in 1..size {
        if let Some(&(generation, offset)) = layout.uncompressed.get(&number) {
            bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
        } else {
            bytes.extend_from_slice(b"0000000000 00000 f \n");
        }
    }

    bytes.extend_from_slice(b"trailer ");
    write_canonical_classic_trailer(bytes, trailer, size, &trailer.canonical_entries);
    bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
    written_xref_table(layout, size)
}

fn write_canonical_classic_trailer(
    bytes: &mut Vec<u8>,
    trailer: &TrailerPlan,
    size: u32,
    canonical: &[(Vec<u8>, Vec<u8>)],
) {
    let mut entries = canonical.to_vec();
    entries.push((
        b"/Root".to_vec(),
        format!("{} {} R", trailer.root.number, trailer.root.generation).into_bytes(),
    ));
    entries.push((b"/Size".to_vec(), size.to_string().into_bytes()));
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    bytes.extend_from_slice(b"<<");
    for (key, value) in entries {
        bytes.push(b' ');
        bytes.push(b'/');
        crate::object::write_name_escaped(bytes, key.strip_prefix(b"/").unwrap_or(&key));
        bytes.push(b' ');
        bytes.extend_from_slice(&value);
    }

    if let IdPlan::Materialized {
        value: Some((id0, id1)),
    } = &trailer.id
    {
        bytes.extend_from_slice(b" /ID [<");
        write_hex(bytes, id0);
        bytes.extend_from_slice(b"><");
        write_hex(bytes, id1);
        bytes.extend_from_slice(b">]");
    } else if let IdPlan::Deterministic {
        source_id0,
        info_suffix,
    } = &trailer.id
    {
        bytes.extend_from_slice(b" /ID ");
        write_deterministic_id_inline(bytes, info_suffix, source_id0.as_deref());
    }

    if let Some(encrypt) = trailer.encrypt {
        bytes.extend_from_slice(b" /Encrypt ");
        bytes.extend_from_slice(format!("{} {} R", encrypt.number, encrypt.generation).as_bytes());
    }
    bytes.extend_from_slice(b" >>");
}

fn write_hex(out: &mut Vec<u8>, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize]);
        out.push(HEX[(byte & 0x0f) as usize]);
    }
}

fn written_xref_table(
    layout: &BodyLayout,
    size: u32,
) -> crate::Result<BTreeMap<ObjectRef, XrefEntry>> {
    let mut result = BTreeMap::new();
    for number in 1..size {
        if let Some(&(_generation, offset)) = layout.uncompressed.get(&number) {
            result.insert(
                ObjectRef::new(number, 0),
                XrefEntry::Uncompressed {
                    // cov:ignore-start: offsets originate in Vec::len and usize fits u64
                    // on every supported target.
                    offset: u64::try_from(offset).map_err(|_| {
                        crate::Error::Unsupported("xref offset does not fit u64".to_string())
                    })?,
                    // cov:ignore-end
                },
            );
        }
    }
    Ok(result)
}

fn written_xref_stream(
    layout: &BodyLayout,
    xref_ref: ObjectRef,
    xref_offset: usize,
) -> crate::Result<BTreeMap<ObjectRef, XrefEntry>> {
    let size = xref_ref
        .number
        .checked_add(1)
        .ok_or_else(|| crate::Error::Unsupported("plain writer /Size overflows u32".into()))?;
    let mut result = BTreeMap::new();
    for number in 1..size {
        if number == xref_ref.number {
            result.insert(
                ObjectRef::new(xref_ref.number, 0),
                XrefEntry::Uncompressed {
                    // cov:ignore-start: offsets originate in Vec::len and usize fits u64
                    // on every supported target.
                    offset: u64::try_from(xref_offset).map_err(|_| {
                        crate::Error::Unsupported("xref offset does not fit u64".to_string())
                    })?,
                    // cov:ignore-end
                },
            );
        } else if let Some(&(_generation, offset)) = layout.uncompressed.get(&number) {
            result.insert(
                ObjectRef::new(number, 0),
                XrefEntry::Uncompressed {
                    // cov:ignore-start: offsets originate in Vec::len and usize fits u64
                    // on every supported target.
                    offset: u64::try_from(offset).map_err(|_| {
                        crate::Error::Unsupported("xref offset does not fit u64".to_string())
                    })?,
                    // cov:ignore-end
                },
            );
        } else if let Some(location) = layout.compressed.get(&number) {
            result.insert(
                ObjectRef::new(number, 0),
                XrefEntry::Compressed {
                    stream: location.container,
                    index: location.index,
                },
            );
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ObjectHandle;

    fn trailer(form: XrefForm) -> TrailerPlan {
        TrailerPlan {
            form,
            canonical_entries: Vec::new(),
            root: ObjectRef::new(1, 0),
            id: IdPlan::Materialized { value: None },
            encrypt: None,
            structural_filtered: false,
        }
    }

    #[test]
    fn classic_xref_uses_layout_offsets_and_qpdf_trailer_shape() {
        let mut bytes = b"BODY".to_vec();
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (0, 0));
        append_xref_and_trailer(&mut bytes, &layout, &trailer(XrefForm::Table)).unwrap();
        assert_eq!(
            bytes,
            b"BODYxref\n0 2\n\
              0000000000 65535 f \n\
              0000000000 00000 n \n\
              trailer << /Root 1 0 R /Size 2 >>\n\
              startxref\n4\n%%EOF\n"
        );
    }

    #[test]
    fn canonical_classic_trailer_writes_deterministic_id_and_encrypt() {
        let mut bytes = b"BODY".to_vec();
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (0, 0));
        let trailer = TrailerPlan {
            form: XrefForm::Table,
            canonical_entries: vec![(b"/Added".to_vec(), b"true".to_vec())],
            root: ObjectRef::new(1, 0),
            id: IdPlan::Deterministic {
                source_id0: Some(vec![0x01, 0x02]),
                info_suffix: vec![0x03, 0x04],
            },
            encrypt: Some(ObjectRef::new(8, 0)),
            structural_filtered: false,
        };

        append_xref_and_trailer(&mut bytes, &layout, &trailer).unwrap();
        let text = String::from_utf8(bytes).unwrap();

        assert!(text.contains("/Added true"));
        assert!(text.contains("/ID [<"));
        assert!(text.contains("] /Encrypt 8 0 R >>"));
    }

    #[test]
    fn classic_xref_free_gap_uses_generation_zero_in_bytes_and_result() {
        let mut bytes = b"BODY".to_vec();
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (0, 0));
        layout.uncompressed.insert(3, (0, 2));

        let result = append_xref_and_trailer(&mut bytes, &layout, &trailer(XrefForm::Table))
            .expect("classic xref emission succeeds");

        assert_eq!(
            bytes
                .windows(b"0000000000 65535 f \n".len())
                .filter(|window| *window == b"0000000000 65535 f \n")
                .count(),
            1,
            "only the object-0 row carries generation 65535"
        );
        assert!(!result.contains_key(&ObjectRef::new(2, 0)));
        assert!(!result.contains_key(&ObjectRef::new(2, 65535)));
        assert!(result.keys().all(|object_ref| object_ref.generation == 0));
    }

    #[test]
    fn layout_rejects_plain_and_compressed_collision() {
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(4, (0, 10));
        layout.compressed.insert(
            4,
            CompressedLocation {
                container: 3,
                index: 0,
            },
        );
        let err = layout.validate().unwrap_err();
        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("object 4")));
    }

    #[test]
    fn xref_stream_preserves_uncompressed_generation() {
        let mut bytes = b"BODY".to_vec();
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (2, 0));

        append_xref_and_trailer(&mut bytes, &layout, &trailer(XrefForm::Stream)).unwrap();

        let stream_start = bytes
            .windows(b"stream\n".len())
            .position(|window| window == b"stream\n")
            .unwrap()
            + b"stream\n".len();
        let stream_end = bytes
            .windows(b"\nendstream".len())
            .position(|window| window == b"\nendstream")
            .unwrap();
        assert_eq!(
            &bytes[stream_start..stream_end],
            &[0, 0, 0, 1, 0, 2, 1, 4, 0]
        );
    }

    #[test]
    fn xref_stream_rejects_object_number_overflow_before_mutating_bytes() {
        let mut bytes = b"BODY".to_vec();
        let original = bytes.clone();
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(u32::MAX, (0, 0));

        let err =
            append_xref_and_trailer(&mut bytes, &layout, &trailer(XrefForm::Stream)).unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("object number")));
        assert_eq!(bytes, original);
    }

    #[test]
    fn structurally_filtered_xref_stream_encodes_its_payload() {
        let mut bytes = b"BODY".to_vec();
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (0, 0));
        let mut trailer = trailer(XrefForm::Stream);
        trailer.structural_filtered = true;

        append_xref_and_trailer(&mut bytes, &layout, &trailer).unwrap();

        assert!(String::from_utf8_lossy(&bytes).contains("/Filter /FlateDecode"));
    }

    #[test]
    fn materialized_id_accepts_exactly_two_strings() {
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            "ID",
            Object::Array(vec![
                Object::String(b"first".to_vec()),
                Object::String(b"second".to_vec()),
            ]),
        );

        assert_eq!(
            materialized_id(&dictionary).unwrap(),
            Some((b"first".to_vec(), b"second".to_vec()))
        );
    }

    #[test]
    fn materialized_id_handle_accepts_exactly_two_strings() {
        let handle = ObjectHandle::array(vec![
            ObjectHandle::string(b"first".to_vec()),
            ObjectHandle::string(b"second".to_vec()),
        ]);

        assert_eq!(
            materialized_id_handle(&handle).unwrap(),
            Some((b"first".to_vec(), b"second".to_vec()))
        );
    }

    #[test]
    fn materialized_id_handle_accepts_a_missing_id() {
        assert_eq!(materialized_id_handle(&ObjectHandle::null()).unwrap(), None);
    }

    #[test]
    fn materialized_id_handle_rejects_a_non_array() {
        let error = materialized_id_handle(&ObjectHandle::integer(1)).unwrap_err();

        assert!(matches!(error, crate::Error::Unsupported(message)
            if message.contains("must be an array")));
    }

    #[test]
    fn materialized_id_handle_rejects_an_array_with_the_wrong_shape() {
        let handle = ObjectHandle::array(vec![ObjectHandle::string(b"only".to_vec())]);
        let error = materialized_id_handle(&handle).unwrap_err();

        assert!(matches!(error, crate::Error::Unsupported(message)
            if message.contains("two strings")));
    }

    #[test]
    fn materialized_id_handle_rejects_non_string_elements() {
        let handle = ObjectHandle::array(vec![
            ObjectHandle::integer(1),
            ObjectHandle::string(b"second".to_vec()),
        ]);
        let error = materialized_id_handle(&handle).unwrap_err();

        assert!(matches!(error, crate::Error::Unsupported(message)
            if message.contains("two strings")));
    }

    #[test]
    fn materialized_id_rejects_wrong_array_shape() {
        let mut dictionary = Dictionary::new();
        dictionary.insert("ID", Object::Array(vec![Object::String(b"only".to_vec())]));

        let err = materialized_id(&dictionary).unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("two strings")));
    }

    #[test]
    fn materialized_id_rejects_non_array() {
        let mut dictionary = Dictionary::new();
        dictionary.insert("ID", Object::Integer(1));

        let err = materialized_id(&dictionary).unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("must be an array")));
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn classic_xref_rejects_offsets_that_exceed_ten_digits_before_mutating_bytes() {
        let mut bytes = b"BODY".to_vec();
        let original = bytes.clone();
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (0, 10_000_000_000));

        let err =
            append_xref_and_trailer(&mut bytes, &layout, &trailer(XrefForm::Table)).unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("offset")));
        assert_eq!(bytes, original);
    }

    #[test]
    fn xref_stream_rejects_size_overflow_before_mutating_bytes() {
        let mut bytes = b"BODY".to_vec();
        let original = bytes.clone();
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(u32::MAX - 1, (0, 0));

        let err =
            append_xref_and_trailer(&mut bytes, &layout, &trailer(XrefForm::Stream)).unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("/Size overflows")));
        assert_eq!(bytes, original);
    }

    #[test]
    fn classic_xref_rejects_size_overflow_before_mutating_bytes() {
        let mut bytes = b"BODY".to_vec();
        let original = bytes.clone();
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(u32::MAX, (0, 0));

        let err =
            append_xref_and_trailer(&mut bytes, &layout, &trailer(XrefForm::Table)).unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("/Size overflows")));
        assert_eq!(bytes, original);
    }

    #[test]
    fn xref_stream_uses_minimal_widths_and_omits_full_range_index() {
        let mut bytes = b"BODY".to_vec();
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (0, 0));
        layout.compressed.insert(
            2,
            CompressedLocation {
                container: 1,
                index: 0,
            },
        );
        append_xref_and_trailer(&mut bytes, &layout, &trailer(XrefForm::Stream)).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/Type /XRef"));
        assert!(text.contains("/W [ 1 1 0 ]"));
        assert!(!text.contains("/Index"));
        assert!(text.contains("/Root 1 0 R /Size 4"));
        assert!(text.ends_with("startxref\n4\n%%EOF\n"));
    }

    #[test]
    fn xref_stream_emits_encrypt_after_materialized_id() {
        let mut bytes = b"BODY".to_vec();
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (0, 0));
        let mut trailer = trailer(XrefForm::Stream);
        trailer.id = IdPlan::Materialized {
            value: Some((b"first".to_vec(), b"second".to_vec())),
        };
        trailer.encrypt = Some(ObjectRef::new(8, 0));

        append_xref_and_trailer(&mut bytes, &layout, &trailer).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let id = text.find("/ID [<").expect("xref stream ID");
        let encrypt = text.find("/Encrypt 8 0 R").expect("xref stream Encrypt");
        assert!(
            encrypt > id,
            "xref stream must write /Encrypt after /ID: {text}"
        );
    }

    #[test]
    fn classic_xref_emits_free_entries_for_layout_holes() {
        let mut bytes = b"BODY".to_vec();
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(2, (0, 0));

        append_xref_and_trailer(&mut bytes, &layout, &trailer(XrefForm::Table)).unwrap();

        assert!(String::from_utf8_lossy(&bytes).contains(
            "0000000000 65535 f \n\
             0000000000 00000 f \n\
             0000000000 00000 n \n"
        ));
    }

    #[test]
    fn deterministic_xref_stream_uses_id_writer_independent_of_canonical_entries() {
        let mut bytes = b"BODY".to_vec();
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (0, 0));
        let mut trailer = trailer(XrefForm::Stream);
        trailer
            .canonical_entries
            .push((b"/Added".to_vec(), b"1".to_vec()));
        trailer.id = IdPlan::Deterministic {
            source_id0: None,
            info_suffix: Vec::new(),
        };

        append_xref_and_trailer(&mut bytes, &layout, &trailer).unwrap();

        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/Added 1"));
        assert!(text.contains("/ID [<"));
    }

    #[test]
    fn deterministic_classic_xref_writes_id_without_materialized_source_id() {
        let mut bytes = b"BODY".to_vec();
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (0, 0));
        let mut trailer = trailer(XrefForm::Table);
        trailer.id = IdPlan::Deterministic {
            source_id0: None,
            info_suffix: Vec::new(),
        };

        append_xref_and_trailer(&mut bytes, &layout, &trailer).unwrap();

        assert!(String::from_utf8_lossy(&bytes).contains("/ID [<"));
    }

    #[test]
    fn classic_xref_rejects_compressed_layout_before_mutating_bytes() {
        let mut bytes = b"BODY".to_vec();
        let original = bytes.clone();
        let mut layout = BodyLayout::default();
        layout.compressed.insert(
            2,
            CompressedLocation {
                container: 1,
                index: 0,
            },
        );

        let err =
            append_xref_and_trailer(&mut bytes, &layout, &trailer(XrefForm::Table)).unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("classic xref")));
        assert_eq!(bytes, original);
    }
}

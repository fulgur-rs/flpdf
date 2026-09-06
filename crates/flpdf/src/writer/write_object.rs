//! qpdf correspondence: `QPDFWriter::writeObject`, `openObject`, and `closeObject`.
//!
//! qpdf sources: `libqpdf/QPDFWriter.cc:1036-1054,1761-1809`.
//!
//! The internal Rust trait supplies the writer's state and output operations;
//! the shared method retains qpdf's control flow. This replaces C++ member
//! access with borrowed Rust state without changing the emission sequence.

use std::collections::BTreeMap;

use super::encryption_state::WriterEncryptionState;
use crate::{ObjectHandle, ObjectRef, Result};

/// QDF fields read for the object currently being written.
#[derive(Clone, Copy)]
pub(crate) struct QdfObjectInfo {
    pub(crate) page_sequence: Option<usize>,
    pub(crate) contents_sequence: Option<usize>,
    pub(crate) suppress_original_object_ids: bool,
}

/// Current stream fields used when `direct_stream_lengths` is false.
#[derive(Clone, Copy)]
pub(crate) struct IndirectStreamLength {
    pub(crate) cur_stream_length: usize,
    pub(crate) added_newline: bool,
}

/// Writer operations called by the single `writeObject` implementation.
///
/// Implementations supply live object serialization, never an already
/// serialized body: the progress callback precedes that serialization and
/// can mutate the object about to be written.
pub(crate) trait WriteObject {
    type ObjectStreamContainer;

    fn object_stream_container(&self, object: ObjectRef) -> Option<Self::ObjectStreamContainer>;
    fn write_object_stream(
        &mut self,
        object: &ObjectHandle,
        container: Self::ObjectStreamContainer,
    ) -> Result<()>;
    fn indicate_progress(&mut self) -> Result<()>;
    fn output_number(&self, object: ObjectRef) -> Result<u32>;
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<()>;
    fn output_count(&self) -> usize;
    fn xref(&mut self) -> &mut BTreeMap<u32, (u16, usize)>;
    fn lengths(&mut self) -> &mut BTreeMap<u32, usize>;
    fn encryption_state(&mut self) -> &mut WriterEncryptionState;
    fn unparse_object(&mut self, object: &ObjectHandle, in_object_stream: bool) -> Result<()>;

    /// `None` represents qpdf's default `qdf_mode == false`.
    fn qdf_object_info(&self, _object: ObjectRef) -> Option<QdfObjectInfo> {
        None
    }

    /// `None` represents qpdf's default `direct_stream_lengths == true`.
    fn indirect_stream_length(&self) -> Option<IndirectStreamLength> {
        None
    }

    /// The already-allocated-id case of qpdf's `openObject`.
    fn open_object(&mut self, object: u32) -> Result<()> {
        let offset = self.output_count();
        self.xref().insert(object, (0, offset));
        self.write_bytes(object.to_string().as_bytes())?;
        self.write_bytes(b" 0 obj\n")
    }

    fn close_object(&mut self, object: u32, qdf: bool) -> Result<()> {
        self.write_bytes(b"\nendobj\n")?;
        if qdf {
            self.write_bytes(b"\n")?;
        }
        let length = self.output_count() - self.xref()[&object].1;
        self.lengths().insert(object, length);
        Ok(())
    }

    /// Translate `QPDFWriter::writeObject` in its original operation order.
    fn write_object(
        &mut self,
        object: &ObjectHandle,
        object_stream_index: Option<u32>,
    ) -> Result<()> {
        // qpdf getObjGen returns (0, 0) for a direct object. Normal queue and
        // ObjStm callers supply indirect objects; preserve the accessor's
        // actual value rather than inventing an identity for a direct value.
        let old_og = object.object_ref().unwrap_or(ObjectRef::new(0, 0));
        if object_stream_index.is_none() && old_og.generation == 0 {
            if let Some(container) = self.object_stream_container(old_og) {
                return self.write_object_stream(object, container);
            }
        }

        self.indicate_progress()?;
        let new_id = self.output_number(old_og)?;
        let qdf = self.qdf_object_info(old_og);
        if let Some(info) = qdf {
            if let Some(sequence) = info.page_sequence {
                self.write_bytes(b"%% Page ")?;
                self.write_bytes(sequence.to_string().as_bytes())?;
                self.write_bytes(b"\n")?;
            }
            if let Some(sequence) = info.contents_sequence {
                self.write_bytes(b"%% Contents for page ")?;
                self.write_bytes(sequence.to_string().as_bytes())?;
                self.write_bytes(b"\n")?;
            }
        }

        if object_stream_index.is_none() {
            if qdf.is_some_and(|info| !info.suppress_original_object_ids) {
                let comment = format!(
                    "%% Original object ID: {} {}\n",
                    old_og.number, old_og.generation
                );
                self.write_bytes(comment.as_bytes())?;
            }
            self.open_object(new_id)?;
            self.encryption_state().set_data_key(new_id);
            self.unparse_object(object, false)?;
            self.encryption_state().clear_data_key();
            self.close_object(new_id, qdf.is_some())?;
        } else {
            self.unparse_object(object, true)?;
            self.write_bytes(b"\n")?;
        }

        if let Some(length) = self.indirect_stream_length() {
            object.try_dereference()?;
            if object.as_stream_dict().is_some() {
                if qdf.is_some() && length.added_newline {
                    self.write_bytes(b"%QDF: ignore_newline\n")?;
                }
                self.open_object(new_id + 1)?;
                self.write_bytes(length.cur_stream_length.to_string().as_bytes())?;
                self.close_object(new_id + 1, qdf.is_some())?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::{serialize, ObjectWriterEmission};
    use crate::{NewlineBeforeEndstream, Pdf};
    use std::collections::BTreeSet;
    use std::io::Cursor;
    use std::rc::Rc;

    /// Memory-output writer state using the real handle unparser and stream sink.
    struct MemoryWriter {
        bytes: Vec<u8>,
        xref: BTreeMap<u32, (u16, usize)>,
        lengths: BTreeMap<u32, usize>,
        encryption: WriterEncryptionState,
        qdf: Option<QdfObjectInfo>,
        direct_stream_lengths: bool,
        cur_stream_length: usize,
        added_newline: bool,
        container: bool,
        fail_progress: bool,
        fail_after: Option<usize>,
        observed_key: Option<Vec<u8>>,
    }

    impl MemoryWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                xref: BTreeMap::new(),
                lengths: BTreeMap::new(),
                encryption: WriterEncryptionState::new(true, vec![1; 5], false, 2, 3),
                qdf: None,
                direct_stream_lengths: true,
                cur_stream_length: 0,
                added_newline: false,
                container: false,
                fail_progress: false,
                fail_after: None,
                observed_key: None,
            }
        }
    }

    impl WriteObject for MemoryWriter {
        type ObjectStreamContainer = ();

        fn object_stream_container(&self, _object: ObjectRef) -> Option<()> {
            self.container.then_some(())
        }

        fn write_object_stream(&mut self, object: &ObjectHandle, (): ()) -> Result<()> {
            self.open_object(1)?;
            serialize::write_stream_to_buf(&mut self.bytes, object, NewlineBeforeEndstream::Never)?;
            self.close_object(1, false)
        }

        fn indicate_progress(&mut self) -> Result<()> {
            if self.fail_progress {
                return Err(crate::Error::System("progress callback failure".into()));
            }
            Ok(())
        }

        fn output_number(&self, _object: ObjectRef) -> Result<u32> {
            Ok(1)
        }

        fn write_bytes(&mut self, bytes: &[u8]) -> Result<()> {
            if self
                .fail_after
                .is_some_and(|limit| self.bytes.len() >= limit)
            {
                return Err(std::io::Error::other("output sink failure").into());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(())
        }

        fn output_count(&self) -> usize {
            self.bytes.len()
        }
        fn xref(&mut self) -> &mut BTreeMap<u32, (u16, usize)> {
            &mut self.xref
        }
        fn lengths(&mut self) -> &mut BTreeMap<u32, usize> {
            &mut self.lengths
        }
        fn encryption_state(&mut self) -> &mut WriterEncryptionState {
            &mut self.encryption
        }
        fn qdf_object_info(&self, _object: ObjectRef) -> Option<QdfObjectInfo> {
            self.qdf
        }
        fn indirect_stream_length(&self) -> Option<IndirectStreamLength> {
            (!self.direct_stream_lengths).then_some(IndirectStreamLength {
                cur_stream_length: self.cur_stream_length,
                added_newline: self.added_newline,
            })
        }

        fn unparse_object(&mut self, object: &ObjectHandle, _in_object_stream: bool) -> Result<()> {
            self.observed_key = self.encryption.current_data_key().map(<[u8]>::to_vec);
            object.try_dereference()?;
            if object.as_stream_dict().is_some() {
                let data = object.get_raw_stream_data()?;
                self.cur_stream_length = data.len();
                if self.qdf.is_some() {
                    let length_ref = (!self.direct_stream_lengths).then_some(ObjectRef::new(2, 0));
                    object.write_stream_body_qdf_with_ref_map_and_removed_and_length_with_options(
                        &mut self.bytes,
                        0,
                        &|object| Ok(object),
                        &BTreeSet::new(),
                        length_ref,
                        crate::writer::StreamDictionaryOptions::preserve(),
                    )?;
                } else {
                    object.write_stream_body(&mut self.bytes, false)?;
                }
                self.added_newline = serialize::framing_adds_newline_with_qdf(
                    &data,
                    NewlineBeforeEndstream::Never,
                    self.qdf.is_some(),
                );
                serialize::write_stream_payload_with_qdf(
                    &mut self.bytes,
                    &data,
                    NewlineBeforeEndstream::Never,
                    self.qdf.is_some(),
                );
                Ok(())
            } else if self.qdf.is_some() {
                object.write_object_qdf(&mut self.bytes, 0)
            } else {
                ObjectWriterEmission::write_object(object, &mut self.bytes)
            }
        }
    }

    fn indirect(value: ObjectHandle) -> (Pdf<Cursor<Vec<u8>>>, ObjectHandle) {
        let pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
        ))
        .unwrap();
        let handle = pdf.make_indirect_from_object_handle(value).unwrap();
        (pdf, handle)
    }

    fn stream(data: &[u8]) -> ObjectHandle {
        ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"/Length".to_vec(),
                ObjectHandle::integer(data.len() as i64),
            )]),
            Rc::new(data.to_vec()),
        )
    }

    #[test]
    fn top_level_framing_records_offsets_lengths_and_uses_the_emitted_data_key() {
        let (_pdf, object) = indirect(ObjectHandle::integer(42));
        let mut writer = MemoryWriter::new();
        writer.write_object(&object, None).unwrap();
        assert_eq!(writer.bytes, b"1 0 obj\n42\nendobj\n");
        assert_eq!(writer.xref.get(&1), Some(&(0, 0)));
        assert_eq!(writer.lengths.get(&1), Some(&writer.bytes.len()));
        assert_eq!(
            writer.observed_key,
            Some(vec![
                0x95, 0x80, 0x33, 0x93, 0xa0, 0x9e, 0x46, 0xba, 0xb0, 0x04
            ])
        );
        assert!(writer.encryption.current_data_key().is_none());
    }

    #[test]
    fn member_adds_one_newline_without_object_framing_or_key_changes() {
        let (_pdf, object) = indirect(ObjectHandle::integer(42));
        let mut writer = MemoryWriter::new();
        writer.encryption.set_data_key(8);
        let original_key = writer.encryption.current_data_key().unwrap().to_vec();
        writer.write_object(&object, Some(0)).unwrap();
        assert_eq!(writer.bytes, b"42\n");
        assert!(writer.xref.is_empty());
        assert!(writer.lengths.is_empty());
        assert_eq!(
            writer.encryption.current_data_key(),
            Some(original_key.as_slice())
        );
        assert_eq!(writer.observed_key, Some(original_key));
    }

    #[test]
    fn qdf_comments_and_indirect_length_follow_the_stream_and_ignore_added_newline() {
        let (_pdf, object) = indirect(stream(b"abc"));
        let old = object.object_ref().unwrap();
        let mut writer = MemoryWriter::new();
        writer.qdf = Some(QdfObjectInfo {
            page_sequence: Some(1),
            contents_sequence: Some(2),
            suppress_original_object_ids: false,
        });
        writer.direct_stream_lengths = false;
        writer.write_object(&object, None).unwrap();
        let expected = format!("%% Page 1\n%% Contents for page 2\n%% Original object ID: {} {}\n1 0 obj\n<<\n  /Length 2 0 R\n>>\nstream\nabc\nendstream\nendobj\n\n%QDF: ignore_newline\n2 0 obj\n3\nendobj\n\n", old.number, old.generation);
        assert_eq!(writer.bytes, expected.as_bytes());
        for number in [1, 2] {
            let offset = writer.xref[&number].1;
            let length = writer.lengths[&number];
            assert!(writer.bytes[offset..offset + length].ends_with(b"\nendobj\n\n"));
        }
    }

    #[test]
    fn qdf_suppression_and_existing_newline_do_not_add_extra_comments() {
        let (_pdf, object) = indirect(stream(b"abc\n"));
        let mut writer = MemoryWriter::new();
        writer.qdf = Some(QdfObjectInfo {
            page_sequence: None,
            contents_sequence: None,
            suppress_original_object_ids: true,
        });
        writer.direct_stream_lengths = false;
        writer.write_object(&object, None).unwrap();
        assert_eq!(writer.bytes, b"1 0 obj\n<<\n  /Length 2 0 R\n>>\nstream\nabc\nendstream\nendobj\n\n2 0 obj\n4\nendobj\n\n");
    }

    #[test]
    fn non_stream_gets_no_length_holder_when_direct_lengths_are_disabled() {
        let (_pdf, object) = indirect(ObjectHandle::integer(42));
        let mut writer = MemoryWriter::new();
        writer.direct_stream_lengths = false;
        writer.qdf = Some(QdfObjectInfo {
            page_sequence: None,
            contents_sequence: None,
            suppress_original_object_ids: true,
        });
        writer.write_object(&object, None).unwrap();
        assert_eq!(writer.bytes, b"1 0 obj\n42\nendobj\n\n");
        assert_eq!(writer.xref.len(), 1);
    }

    #[test]
    fn unparse_failure_keeps_the_key_and_open_offset_without_closing_the_object() {
        let (_pdf, object) = indirect(ObjectHandle::dictionary(vec![(
            b"/Broken".to_vec(),
            ObjectHandle::uninitialized(),
        )]));
        let mut writer = MemoryWriter::new();
        assert!(writer.write_object(&object, None).is_err());
        assert!(writer.bytes.starts_with(b"1 0 obj\n"));
        assert!(writer.encryption.current_data_key().is_some());
        assert_eq!(writer.xref.get(&1), Some(&(0, 0)));
        assert!(writer.lengths.is_empty());
    }

    #[test]
    fn output_failure_before_unparse_keeps_the_recorded_offset_without_setting_a_key() {
        let (_pdf, object) = indirect(ObjectHandle::integer(42));
        let mut writer = MemoryWriter::new();
        writer.fail_after = Some(0);
        assert!(writer.write_object(&object, None).is_err());
        assert!(writer.bytes.is_empty());
        assert_eq!(writer.xref.get(&1), Some(&(0, 0)));
        assert!(writer.encryption.current_data_key().is_none());
    }

    #[test]
    fn output_failure_after_unparse_observes_the_cleared_key() {
        let (_pdf, object) = indirect(ObjectHandle::integer(42));
        let mut writer = MemoryWriter::new();
        writer.fail_after = Some(b"1 0 obj\n42".len());
        assert!(writer.write_object(&object, None).is_err());
        assert_eq!(writer.bytes, b"1 0 obj\n42");
        assert!(writer.encryption.current_data_key().is_none());
        assert!(writer.lengths.is_empty());
    }

    #[test]
    fn container_dispatch_precedes_the_normal_object_progress_callback() {
        let value = stream(b"2 0\n42\n");
        value
            .as_stream_dict()
            .unwrap()
            .replace_key(b"/Type", ObjectHandle::name(b"ObjStm".to_vec()))
            .unwrap();
        let (_pdf, object) = indirect(value);
        let mut writer = MemoryWriter::new();
        writer.container = true;
        writer.fail_progress = true;
        writer.write_object(&object, None).unwrap();
        assert_eq!(
            writer.bytes,
            b"1 0 obj\n<< /Type /ObjStm /Length 7 >>\nstream\n2 0\n42\nendstream\nendobj\n"
        );
    }

    #[test]
    fn progress_failure_happens_before_object_output_or_data_key_setup() {
        let (_pdf, object) = indirect(ObjectHandle::integer(42));
        let mut writer = MemoryWriter::new();
        writer.fail_progress = true;
        assert!(writer.write_object(&object, None).is_err());
        assert!(writer.bytes.is_empty());
        assert!(writer.xref.is_empty());
        assert!(writer.encryption.current_data_key().is_none());
    }
    #[test]
    fn compact_stream_uses_its_direct_length_without_a_qdf_newline() {
        let (_pdf, object) = indirect(stream(b"abc"));
        let mut writer = MemoryWriter::new();
        writer.write_object(&object, None).unwrap();
        assert_eq!(
            writer.bytes,
            b"1 0 obj\n<< /Length 3 >>\nstream\nabcendstream\nendobj\n"
        );
    }

    #[test]
    fn qdf_stream_preserves_indirect_dictionary_entries() {
        let (pdf, object) = indirect(stream(b"abc"));
        let peer = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(7))
            .unwrap();
        let peer_id = peer.object_ref().unwrap();
        object
            .as_stream_dict()
            .unwrap()
            .replace_key(b"/Peer", peer)
            .unwrap();
        let mut writer = MemoryWriter::new();
        writer.qdf = Some(QdfObjectInfo {
            page_sequence: None,
            contents_sequence: None,
            suppress_original_object_ids: true,
        });
        writer.write_object(&object, None).unwrap();
        let reference = format!("/Peer {} {} R", peer_id.number, peer_id.generation);
        assert!(writer
            .bytes
            .windows(reference.len())
            .any(|window| window == reference.as_bytes()));
    }

    #[test]
    fn qdf_stream_dictionary_failure_does_not_close_the_object_or_clear_the_key() {
        let (_pdf, object) = indirect(stream(b"abc"));
        object
            .as_stream_dict()
            .unwrap()
            .replace_key(b"/Broken", ObjectHandle::uninitialized())
            .unwrap();
        let mut writer = MemoryWriter::new();
        writer.qdf = Some(QdfObjectInfo {
            page_sequence: None,
            contents_sequence: None,
            suppress_original_object_ids: true,
        });
        assert!(writer.write_object(&object, None).is_err());
        assert!(writer.encryption.current_data_key().is_some());
        assert!(writer.lengths.is_empty());
    }
}

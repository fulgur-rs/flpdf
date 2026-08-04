//! qpdf correspondence: QPDFWriter.cc standard write pipeline split across plain writer modules.
use std::io::{Read, Seek, Write};

#[cfg(test)]
use std::cell::Cell;

use crate::{ObjectStreamMode, Pdf, WriteOptions};

pub(crate) mod body;
pub(crate) mod plan;
pub(crate) mod xref;

#[cfg(test)]
thread_local! {
    static PLAIN_PIPELINE_CALLS: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn write_plain<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    out: W,
    options: &WriteOptions,
) -> crate::Result<()> {
    #[cfg(test)]
    PLAIN_PIPELINE_CALLS.with(|calls| calls.set(calls.get() + 1));

    let plan = plan::PlainWritePlan::build(pdf, options)?;
    write_planned(pdf, out, options, &plan)
}

fn write_planned<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriteOptions,
    plan: &plan::PlainWritePlan,
) -> crate::Result<()> {
    plan.validate()?;
    let (mut bytes, layout) = body::emit_bodies(pdf, options, plan)?;
    xref::append_xref_and_trailer(&mut bytes, &layout, &plan.trailer)?;
    out.write_all(&bytes)?;
    Ok(())
}

pub(crate) fn eligible(
    pdf_is_encrypted: bool,
    options: &WriteOptions,
    mode: ObjectStreamMode,
) -> bool {
    mode == options.object_streams
        && !options.qdf
        && options.encrypt.is_none()
        && options.copy_encryption.is_none()
        && !pdf_is_encrypted
}

#[cfg(test)]
pub(crate) fn pipeline_calls() -> usize {
    PLAIN_PIPELINE_CALLS.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_pipeline_calls() {
    PLAIN_PIPELINE_CALLS.with(|calls| calls.set(0));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        write_pdf_with_options, Dictionary, Object, ObjectStreamMode, Pdf, Stream, WriteOptions,
    };
    use std::io::Cursor;

    fn write_with(options: &WriteOptions) {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        write_pdf_with_options(&mut pdf, Vec::new(), options).unwrap();
    }

    #[test]
    fn all_plain_modes_route_through_the_new_pipeline() {
        for object_streams in [
            ObjectStreamMode::Disable,
            ObjectStreamMode::Preserve,
            ObjectStreamMode::Generate,
        ] {
            reset_pipeline_calls();
            write_with(&WriteOptions {
                full_rewrite: true,
                object_streams,
                static_id: true,
                ..WriteOptions::default()
            });
            assert_eq!(pipeline_calls(), 1);
        }

        for object_streams in [
            ObjectStreamMode::Disable,
            ObjectStreamMode::Preserve,
            ObjectStreamMode::Generate,
        ] {
            reset_pipeline_calls();
            write_with(&WriteOptions {
                full_rewrite: true,
                object_streams,
                qdf: true,
                static_id: true,
                ..WriteOptions::default()
            });
            assert_eq!(pipeline_calls(), 0);

            reset_pipeline_calls();
            write_with(&WriteOptions {
                full_rewrite: true,
                object_streams,
                encrypt: Some(crate::encrypt_setup::EncryptParams::v4_aes128(
                    b"user".to_vec(),
                    b"owner".to_vec(),
                )),
                static_id: true,
                static_aes_iv: true,
                ..WriteOptions::default()
            });
            assert_eq!(pipeline_calls(), 0);
        }
    }

    #[test]
    fn generate_uses_shared_plain_pipeline() {
        reset_pipeline_calls();
        write_with(&WriteOptions {
            full_rewrite: true,
            object_streams: ObjectStreamMode::Generate,
            static_id: true,
            ..WriteOptions::default()
        });
        assert_eq!(pipeline_calls(), 1);
    }

    #[test]
    fn requested_preserve_and_generate_forced_below_1_5_stay_legacy() {
        for object_streams in [ObjectStreamMode::Preserve, ObjectStreamMode::Generate] {
            reset_pipeline_calls();
            write_with(&WriteOptions {
                full_rewrite: true,
                object_streams,
                force_version: Some("1.4".into()),
                static_id: true,
                ..WriteOptions::default()
            });
            assert_eq!(pipeline_calls(), 0);
        }
    }

    #[test]
    fn pipeline_call_observation_is_thread_local() {
        reset_pipeline_calls();

        std::thread::spawn(|| {
            reset_pipeline_calls();
            write_with(&WriteOptions {
                full_rewrite: true,
                object_streams: ObjectStreamMode::Disable,
                static_id: true,
                ..WriteOptions::default()
            });
            assert_eq!(pipeline_calls(), 1);
        })
        .join()
        .unwrap();

        assert_eq!(pipeline_calls(), 0);
    }

    #[test]
    fn eligibility_excludes_copy_and_source_encryption() {
        for mode in [
            ObjectStreamMode::Disable,
            ObjectStreamMode::Preserve,
            ObjectStreamMode::Generate,
        ] {
            let options = WriteOptions {
                object_streams: mode,
                ..WriteOptions::default()
            };
            assert!(eligible(false, &options, mode));
            assert!(!eligible(true, &options, mode));

            let copy_options = WriteOptions {
                object_streams: mode,
                copy_encryption: Some(crate::encrypt_setup::CopyEncryptionSource {
                    encrypt_dict: crate::Dictionary::new(),
                    file_key: Vec::new(),
                    id0: Vec::new(),
                    object_key_alg: crate::ObjectKeyAlg::Aes,
                }),
                ..WriteOptions::default()
            };
            assert!(!eligible(false, &copy_options, mode));
        }
    }

    #[test]
    fn invalid_prebuilt_member_leaves_caller_writer_unchanged() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let options = WriteOptions {
            object_streams: ObjectStreamMode::Generate,
            ..WriteOptions::default()
        };
        let plan = plan::PlainWritePlan::build(&mut pdf, &options).unwrap();
        let members: Vec<_> = plan
            .objects
            .iter()
            .filter_map(|object| match object {
                plan::PlannedIndirectObject::ObjectStream { members, .. } => Some(members),
                plan::PlannedIndirectObject::Source { .. } => None,
            })
            .flatten()
            .cloned()
            .collect();
        let member = members.into_iter().next().unwrap();
        let mut dict = Dictionary::new();
        dict.insert("Length", Object::Integer(0));
        pdf.set_object(
            member.source,
            Object::Stream(Stream {
                dict,
                data: Vec::new(),
            }),
        );
        let original = b"caller sentinel".to_vec();
        let mut caller = original.clone();

        let error = write_planned(&mut pdf, &mut caller, &options, &plan).unwrap_err();

        assert!(matches!(error, crate::Error::Unsupported(ref message)
            if message.contains("plain writer body invariant")));
        assert_eq!(caller, original);
    }
}

use std::io::{Read, Seek, Write};

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{ObjectStreamMode, Pdf, WriteOptions};

pub(crate) mod body;
pub(crate) mod plan;
pub(crate) mod xref;

#[cfg(test)]
static PLAIN_PIPELINE_CALLS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn write_plain<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriteOptions,
) -> crate::Result<()> {
    #[cfg(test)]
    PLAIN_PIPELINE_CALLS.fetch_add(1, Ordering::Relaxed);

    let plan = plan::PlainWritePlan::build(pdf, options)?;
    plan.validate()?;
    let (mut bytes, layout) = body::emit_bodies(pdf, options, &plan)?;
    xref::append_xref_and_trailer(&mut bytes, &layout, &plan.trailer)?;
    out.write_all(&bytes)?;
    Ok(())
}

pub(crate) fn eligible(
    pdf_is_encrypted: bool,
    options: &WriteOptions,
    mode: ObjectStreamMode,
) -> bool {
    mode == ObjectStreamMode::Disable
        && !options.qdf
        && options.encrypt.is_none()
        && options.copy_encryption.is_none()
        && !pdf_is_encrypted
}

#[cfg(test)]
pub(crate) fn pipeline_calls() -> usize {
    PLAIN_PIPELINE_CALLS.load(Ordering::Relaxed)
}

#[cfg(test)]
pub(crate) fn reset_pipeline_calls() {
    PLAIN_PIPELINE_CALLS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{write_pdf_with_options, ObjectStreamMode, Pdf, WriteOptions};

    fn write_with(options: &WriteOptions) {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open_mem(fixture).unwrap();
        write_pdf_with_options(&mut pdf, Vec::new(), options).unwrap();
    }

    #[test]
    fn only_plain_disable_routes_through_the_new_pipeline() {
        reset_pipeline_calls();
        write_with(&WriteOptions {
            full_rewrite: true,
            object_streams: ObjectStreamMode::Disable,
            static_id: true,
            ..WriteOptions::default()
        });
        assert_eq!(pipeline_calls(), 1);

        for object_streams in [ObjectStreamMode::Preserve, ObjectStreamMode::Generate] {
            reset_pipeline_calls();
            write_with(&WriteOptions {
                full_rewrite: true,
                object_streams,
                static_id: true,
                ..WriteOptions::default()
            });
            assert_eq!(pipeline_calls(), 0);
        }

        reset_pipeline_calls();
        write_with(&WriteOptions {
            full_rewrite: true,
            object_streams: ObjectStreamMode::Disable,
            qdf: true,
            static_id: true,
            ..WriteOptions::default()
        });
        assert_eq!(pipeline_calls(), 0);

        reset_pipeline_calls();
        write_with(&WriteOptions {
            full_rewrite: true,
            object_streams: ObjectStreamMode::Disable,
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

    #[test]
    fn eligibility_excludes_copy_and_source_encryption() {
        let options = WriteOptions {
            object_streams: ObjectStreamMode::Disable,
            ..WriteOptions::default()
        };
        assert!(eligible(false, &options, ObjectStreamMode::Disable));
        assert!(!eligible(true, &options, ObjectStreamMode::Disable));

        let copy_options = WriteOptions {
            object_streams: ObjectStreamMode::Disable,
            copy_encryption: Some(crate::encrypt_setup::CopyEncryptionSource {
                encrypt_dict: crate::Dictionary::new(),
                file_key: Vec::new(),
                id0: Vec::new(),
                object_key_alg: crate::ObjectKeyAlg::Aes,
            }),
            ..WriteOptions::default()
        };
        assert!(!eligible(false, &copy_options, ObjectStreamMode::Disable));
    }
}

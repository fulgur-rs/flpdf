//! Private qpdf-shaped settings used by [`super::QPDFWriter`].

use std::path::PathBuf;

use crate::encrypt_setup::{CopyEncryptionSource, EncryptParams};

use super::{
    CompressStreams, NewlineBeforeEndstream, ObjectStreamMode, StreamDataMode, WriteOptions,
};

/// Controls how much stream decoding a writer setting requests.
///
/// This is the writer setting counterpart to qpdf's stream decode level. It
/// is intentionally distinct from the JSON inspection enum with the same
/// qpdf spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum DecodeLevel {
    /// Keep stream data encoded.
    None,
    /// Decode filters handled by qpdf's generalized stream decoder.
    #[default]
    Generalized,
    /// Decode specialized filters as well.
    Specialized,
    /// Decode every supported filter.
    All,
}

/// The private settings state owned by [`super::QPDFWriter`].
///
/// The current emitter still consumes [`WriteOptions`]. `to_write_options`
/// is a temporary, private Task 2A bridge; it always enables the emitter's
/// full-rewrite branch and does not expose that selector on this object.
#[allow(dead_code)]
pub(crate) struct WriterSettings {
    pub(crate) object_stream_mode: ObjectStreamMode,
    pub(crate) stream_data_mode: Option<StreamDataMode>,
    pub(crate) compress_streams: bool,
    pub(crate) decode_level: DecodeLevel,
    pub(crate) recompress_flate: bool,
    pub(crate) content_normalization: bool,
    pub(crate) qdf_mode: bool,
    pub(crate) preserve_unreferenced_objects: bool,
    pub(crate) newline_before_endstream: bool,
    pub(crate) minimum_pdf_version: Option<(String, i64)>,
    pub(crate) forced_pdf_version: Option<(String, i64)>,
    pub(crate) extra_header_text: String,
    pub(crate) deterministic_id: bool,
    pub(crate) static_id: bool,
    pub(crate) static_aes_iv: bool,
    pub(crate) suppress_original_object_ids: bool,
    pub(crate) preserve_encryption: bool,
    pub(crate) encryption_parameters: Option<EncryptParams>,
    pub(crate) copy_encryption: Option<CopyEncryptionSource>,
    pub(crate) linearization: bool,
    pub(crate) linearization_pass1_filename: Option<PathBuf>,
    pub(crate) pclm: bool,
    pub(crate) progress_reporter: Option<Box<dyn FnMut(u8) + 'static>>,
}

impl Default for WriterSettings {
    fn default() -> Self {
        Self {
            object_stream_mode: ObjectStreamMode::Preserve,
            stream_data_mode: None,
            compress_streams: true,
            decode_level: DecodeLevel::None,
            recompress_flate: false,
            content_normalization: false,
            qdf_mode: false,
            preserve_unreferenced_objects: false,
            newline_before_endstream: false,
            minimum_pdf_version: None,
            forced_pdf_version: None,
            extra_header_text: String::new(),
            deterministic_id: false,
            static_id: false,
            static_aes_iv: false,
            suppress_original_object_ids: false,
            preserve_encryption: true,
            encryption_parameters: None,
            copy_encryption: None,
            linearization: false,
            linearization_pass1_filename: None,
            pclm: false,
            progress_reporter: None,
        }
    }
}

impl WriterSettings {
    /// Convert the qpdf-shaped settings into the legacy emitter's private
    /// option representation for this temporary full-rewrite slice.
    pub(crate) fn to_write_options(&self) -> WriteOptions {
        let mut options = WriteOptions {
            // The compatibility bridge must never select the incremental
            // route, even though WriteOptions still contains that old field.
            full_rewrite: true,
            object_streams: self.object_stream_mode,
            preserve_unreferenced_objects: self.preserve_unreferenced_objects,
            compress_streams: if self.compress_streams {
                CompressStreams::Yes
            } else {
                CompressStreams::No
            },
            decode_level: self.decode_level,
            // QPDFWriter translates set_stream_data_mode into compress/decode
            // state. Keep this bridge field clear so the legacy effective
            // policy cannot override the qpdf setter ordering.
            stream_data: self.stream_data_mode,
            recompress_flate: self.recompress_flate,
            qdf: self.qdf_mode,
            newline_before_endstream: if self.newline_before_endstream {
                NewlineBeforeEndstream::Yes
            } else {
                // qpdf's false setting is the existing Never default.
                NewlineBeforeEndstream::Never
            },
            static_id: self.static_id,
            deterministic_id: self.deterministic_id,
            static_aes_iv: self.static_aes_iv,
            no_original_object_ids: self.suppress_original_object_ids,
            encrypt: self.encryption_parameters.clone(),
            copy_encryption: self.copy_encryption.clone(),
            ..WriteOptions::default()
        };

        options.min_version = self
            .minimum_pdf_version
            .as_ref()
            .map(|(version, _)| version.clone());
        options.min_extension_level = self
            .minimum_pdf_version
            .as_ref()
            .map(|(_, extension_level)| *extension_level);
        options.force_version = self
            .forced_pdf_version
            .as_ref()
            .map(|(version, _)| version.clone());

        options
    }
}

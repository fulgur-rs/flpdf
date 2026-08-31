//! qpdf correspondence: QPDFWriter.cc writer-setting state and conversion to emission options.
//!
//! Private qpdf-shaped settings used by [`super::PdfWriter`].

use std::path::PathBuf;

use crate::encryption::{CopyEncryptionSource, EncryptParams};

use super::{
    CompressStreams, NewlineBeforeEndstream, ObjectStreamMode, ProgressReporter, StreamDataMode,
    WriterOptions,
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

/// The private settings state owned by [`super::PdfWriter`].
///
/// The current emitter consumes [`WriterOptions`]. `to_write_options` keeps
/// the qpdf-shaped public setter state separate from the emitter's internal
/// option representation.
#[derive(Debug, Clone)]
pub(crate) struct WriterSettings {
    pub(crate) object_stream_mode: ObjectStreamMode,
    pub(crate) stream_data_mode: Option<StreamDataMode>,
    pub(crate) compress_streams: bool,
    pub(crate) compress_streams_set: bool,
    pub(crate) decode_level: DecodeLevel,
    pub(crate) decode_level_set: bool,
    pub(crate) recompress_flate: bool,
    pub(crate) compression_level: Option<i32>,
    pub(crate) content_normalization: bool,
    pub(crate) content_normalization_set: bool,
    pub(crate) qdf_mode: bool,
    pub(crate) preserve_unreferenced_objects: bool,
    pub(crate) newline_before_endstream: NewlineBeforeEndstream,
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
    pub(crate) progress_reporter: Option<ProgressReporter>,
}

impl Default for WriterSettings {
    fn default() -> Self {
        Self {
            object_stream_mode: ObjectStreamMode::Preserve,
            stream_data_mode: None,
            compress_streams: true,
            compress_streams_set: false,
            decode_level: DecodeLevel::None,
            decode_level_set: false,
            recompress_flate: false,
            compression_level: None,
            content_normalization: false,
            content_normalization_set: false,
            qdf_mode: false,
            preserve_unreferenced_objects: false,
            newline_before_endstream: NewlineBeforeEndstream::Never,
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
    /// Convert the qpdf-shaped settings into the canonical emitter's private
    /// option representation.
    pub(crate) fn to_write_options(&self) -> WriterOptions {
        // PdfWriter applies qpdf's QDF defaults during write setup, after all public
        // setters have run. Only values that were never explicitly set are
        // replaced: QDF enables content normalization, disables compression,
        // and raises the decode level to generalized
        // (QPDFWriter.cc:2078-2087).
        let content_normalization =
            self.content_normalization || (self.qdf_mode && !self.content_normalization_set);
        // qpdf's QDF serializer never emits compressed stream bodies:
        // QPDFWriter.cc uses `compress_streams && !qdf_mode` at the emission
        // boundary, so even an explicit setCompressStreams(true) is ignored
        // while QDF mode is active. Keep the effective emitter option aligned
        // with that observable rule rather than preserving the setter bit.
        let compress_streams = if self.qdf_mode {
            false
        } else {
            self.compress_streams
        };
        let decode_level = if self.qdf_mode && !self.decode_level_set {
            DecodeLevel::Generalized
        } else {
            self.decode_level
        };

        let mut options = WriterOptions {
            object_streams: self.object_stream_mode,
            preserve_unreferenced_objects: self.preserve_unreferenced_objects,
            compress_streams: if compress_streams {
                CompressStreams::Yes
            } else {
                CompressStreams::No
            },
            decode_level,
            // PdfWriter translates set_stream_data_mode into compress/decode
            // state. Keep this field aligned with that setter ordering.
            stream_data: self.stream_data_mode,
            recompress_flate: self.recompress_flate,
            compression_level: self.compression_level,
            content_normalization,
            qdf: self.qdf_mode,
            qdf_stream_policy_precomputed: true,
            newline_before_endstream: self.newline_before_endstream,
            static_id: self.static_id,
            deterministic_id: self.deterministic_id,
            static_aes_iv: self.static_aes_iv,
            no_original_object_ids: self.suppress_original_object_ids,
            encrypt: self.encryption_parameters.clone(),
            copy_encryption: self.copy_encryption.clone(),
            pclm: self.pclm,
            progress_reporter: self.progress_reporter.clone(),
            ..WriterOptions::default()
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
        options.force_extension_level = self
            .forced_pdf_version
            .as_ref()
            .map(|(_, extension_level)| *extension_level);
        options.extra_header_text = self.extra_header_text.clone();

        options
    }
}

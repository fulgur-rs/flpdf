//! Shared test helpers for the helper-API integration tests.
//!
//! Lives in a `common/` subdirectory so Cargo treats it as a module included
//! by each test binary rather than as its own test target.

#![allow(dead_code)]

use flpdf::job::{CheckError, QPDFJob};
use flpdf::ObjectRef;
use flpdf::{
    CompressStreams, CopyEncryptionSource, DecodeLevel, EncryptParams, NewlineBeforeEndstream,
    ObjectHandle, ObjectStreamMode, Pdf, PdfWriter, Result, StreamDataMode,
};
use std::collections::BTreeMap;
use std::io::{Read, Seek, Write};

/// Resolve one canonical object for integration assertions without creating
/// an owned second object model.
pub trait PdfCanonicalTestExt {
    fn resolve_canonical_object(&mut self, object_ref: ObjectRef) -> Result<ObjectHandle>;
}

impl<R: Read + Seek + 'static> PdfCanonicalTestExt for Pdf<R> {
    fn resolve_canonical_object(&mut self, object_ref: ObjectRef) -> Result<ObjectHandle> {
        let handle = self.get_object_handle(object_ref);
        self.resolve(&handle)?;
        Ok(handle)
    }
}

/// Result shape used by integration tests that only need to assert that the
/// canonical qpdf job check accepted an emitted PDF.
#[derive(Debug)]
pub struct CheckTestResult {
    pub valid: bool,
    pub diagnostics: flpdf::Diagnostics,
}

/// Run the canonical qpdf-shaped document check for integration-test output.
pub fn check_output<R: Read + Seek + 'static>(
    source: R,
) -> std::result::Result<CheckTestResult, CheckError> {
    let mut job = QPDFJob::new();
    let mut pdf = job
        .open(
            source,
            "test.pdf",
            flpdf::PdfOpenOptions {
                repair: true,
                ..flpdf::PdfOpenOptions::default()
            },
        )
        .map_err(CheckError::Operation)?;
    let result = job.check(&mut pdf);
    let diagnostics = pdf.repair_diagnostics();
    match result {
        Ok(_) => Ok(CheckTestResult {
            valid: true,
            diagnostics,
        }),
        Err(CheckError::ErrorsDetected) => Ok(CheckTestResult {
            valid: false,
            diagnostics,
        }),
        Err(error) => Err(error),
    }
}

/// Test-only settings used while the integration corpus migrates to the
/// public qpdf-shaped writer. This deliberately has no PDF route selector:
/// [`write_with_settings`] always emits a fresh PdfWriter output.
#[derive(Debug, Clone)]
pub struct WriterTestSettings {
    pub decode_level: DecodeLevel,
    pub content_normalization: bool,
    pub static_id: bool,
    pub deterministic_id: bool,
    pub static_aes_iv: bool,
    pub min_version: Option<String>,
    pub min_extension_level: Option<i64>,
    pub force_version: Option<String>,
    pub force_extension_level: Option<i64>,
    pub extra_header_text: String,
    pub no_original_object_ids: bool,
    pub object_streams: ObjectStreamMode,
    pub preserve_unreferenced_objects: bool,
    pub compress_streams: CompressStreams,
    pub newline_before_endstream: NewlineBeforeEndstream,
    pub qdf: bool,
    pub stream_data: Option<StreamDataMode>,
    pub recompress_flate: bool,
    pub encrypt: Option<EncryptParams>,
    pub copy_encryption: Option<CopyEncryptionSource>,
    pub preserve_encryption: bool,
    pub pclm: bool,
}

impl Default for WriterTestSettings {
    fn default() -> Self {
        Self {
            decode_level: DecodeLevel::Generalized,
            content_normalization: false,
            static_id: false,
            deterministic_id: false,
            static_aes_iv: false,
            min_version: None,
            min_extension_level: None,
            force_version: None,
            force_extension_level: None,
            extra_header_text: String::new(),
            no_original_object_ids: false,
            object_streams: ObjectStreamMode::Preserve,
            preserve_unreferenced_objects: false,
            compress_streams: CompressStreams::Yes,
            newline_before_endstream: NewlineBeforeEndstream::Never,
            qdf: false,
            stream_data: None,
            recompress_flate: false,
            encrypt: None,
            copy_encryption: None,
            preserve_encryption: true,
            pclm: false,
        }
    }
}

impl WriterTestSettings {
    fn apply<R: Read + Seek + 'static>(&self, writer: &mut PdfWriter<'_, R>) -> Result<()> {
        writer.set_object_stream_mode(self.object_streams);
        // PdfWriter applies these defaults during qdf write setup only when
        // the caller did not set the corresponding knobs. Keep the migration
        // adapter's qdf defaults unset so QDF remains editable even for source
        // streams that are already lone Flate streams.
        if !self.qdf || !matches!(self.compress_streams, CompressStreams::Yes) {
            writer.set_compress_streams(matches!(self.compress_streams, CompressStreams::Yes));
        }
        if !self.qdf || !matches!(self.decode_level, DecodeLevel::Generalized) {
            writer.set_decode_level(self.decode_level);
        }
        writer.set_recompress_flate(self.recompress_flate);
        if !self.qdf || self.content_normalization {
            writer.set_content_normalization(self.content_normalization);
        }
        writer.set_qdf_mode(self.qdf);
        writer.set_preserve_unreferenced_objects(self.preserve_unreferenced_objects);
        writer.set_newline_before_endstream(matches!(
            self.newline_before_endstream,
            NewlineBeforeEndstream::Yes
        ));
        if let Some(version) = self.min_version.as_ref() {
            writer.set_minimum_pdf_version(version.clone(), self.min_extension_level.unwrap_or(0));
        }
        if let Some(version) = self.force_version.as_ref() {
            writer.force_pdf_version(version.clone(), self.force_extension_level.unwrap_or(0));
        }
        writer.set_extra_header_text(self.extra_header_text.clone());
        writer.set_deterministic_id(self.deterministic_id);
        writer.set_static_id(self.static_id);
        writer.set_static_aes_iv(self.static_aes_iv);
        writer.set_suppress_original_object_ids(self.no_original_object_ids);
        if self.pclm {
            writer.set_pclm(true);
        }
        if let Some(params) = self.encrypt.clone() {
            writer.set_encryption_parameters(params);
        } else if let Some(source) = self.copy_encryption.clone() {
            writer.copy_encryption_parameters(source);
        }
        writer.set_preserve_encryption(self.preserve_encryption);
        if let Some(mode) = self.stream_data {
            writer.set_stream_data_mode(mode);
        }
        Ok(())
    }
}

/// Emit one fresh PDF through the canonical qpdf writer for integration tests.
pub fn write_with_settings<R: Read + Seek + 'static, W: Write>(
    pdf: &mut Pdf<R>,
    out: W,
    settings: &WriterTestSettings,
) -> Result<()> {
    let mut writer = PdfWriter::new(pdf);
    settings.apply(&mut writer)?;
    writer.set_output_memory()?;
    writer.write()?;
    let bytes = writer.get_buffer()?;
    let mut out = out;
    out.write_all(&bytes)?;
    Ok(())
}

/// Emit through the canonical writer and return the final identities for the
/// supplied source objects. Full rewrite intentionally renumbers objects, so
/// tests that inspect ObjStm members must use this mapping rather than assume
/// source object numbers survive.
pub fn write_with_settings_and_mapping<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    settings: &WriterTestSettings,
    source_refs: &[ObjectRef],
) -> Result<(Vec<u8>, BTreeMap<ObjectRef, ObjectRef>)> {
    let mut writer = PdfWriter::new(pdf);
    settings.apply(&mut writer)?;
    writer.set_output_memory()?;
    writer.write()?;
    let mapping = source_refs
        .iter()
        .filter_map(|source| {
            writer
                .get_renumbered_obj_gen(*source)
                .ok()
                .flatten()
                .map(|output| (*source, output))
        })
        .collect();
    let bytes = writer.get_buffer()?;
    Ok((bytes, mapping))
}

/// Emit one linearized PDF through the canonical qpdf writer for integration
/// tests.
pub fn write_linearized_with_settings<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    settings: &WriterTestSettings,
) -> Result<Vec<u8>> {
    let mut writer = PdfWriter::new(pdf);
    settings.apply(&mut writer)?;
    writer.set_linearization(true);
    writer.set_output_memory()?;
    writer.write()?;
    writer.get_buffer()
}

/// Emit one fresh PDF with qpdf writer defaults.
pub fn write_default<R: Read + Seek + 'static, W: Write>(pdf: &mut Pdf<R>, out: W) -> Result<()> {
    write_with_settings(pdf, out, &WriterTestSettings::default())
}

/// Emit QDF output through the canonical qpdf writer for integration tests.
pub fn write_qdf_output<R: Read + Seek + 'static, W: Write>(
    pdf: &mut Pdf<R>,
    out: W,
) -> Result<()> {
    let settings = WriterTestSettings {
        qdf: true,
        object_streams: ObjectStreamMode::Disable,
        ..WriterTestSettings::default()
    };
    write_with_settings(pdf, out, &settings)
}

/// Build a PDF from a set of already-serialised indirect objects.
///
/// `objects` is a slice of `(object_number, "<<...>>" body)` where the body is
/// everything between `N 0 obj\n` and `\nendobj\n`. The cross-reference table
/// and trailer are generated automatically; `root` names the `/Root` object.
///
/// Object offsets are kept in a `BTreeMap` so xref generation is O(N log N).
pub fn build_pdf(objects: &[(u32, String)], root: u32) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.5\n".to_vec();
    let max_num = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
    let mut offsets: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();
    for (num, body) in objects {
        offsets.insert(*num, out.len() as u64);
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(body.as_bytes());
        out.extend_from_slice(b"\nendobj\n");
    }
    let total = max_num as usize + 1;
    let xref_start = out.len() as u64;
    let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
    for i in 1..=max_num {
        if let Some(off) = offsets.get(&i) {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        } else {
            xref.push_str("0000000000 65535 f \n");
        }
    }
    out.extend_from_slice(xref.as_bytes());
    let trailer =
        format!("trailer\n<< /Size {total} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

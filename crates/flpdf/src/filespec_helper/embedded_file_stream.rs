//! qpdf correspondence: QPDFEFStreamObjectHelper.cc.

use super::shared::{ensure_indirect_handle_belongs_to_pdf, qpdf_style_open_error};
use crate::filters::{decode_stream_data_from_handle, DecodeLimits};
use crate::object_handle::{canonical_dictionary_key, StreamDataProvider};
use crate::pdf_string::utf8_value;
use crate::pipeline::count::Count;
use crate::pipeline::md5::PlMd5;
use crate::pipeline::{Discard, Pipeline};
use crate::writer::DecodeLevel;
use crate::{Error, ObjectHandle, ObjectRef, Pdf, Result};
use std::cell::RefCell;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;
use std::rc::Rc;

// ── EmbeddedFileStream ────────────────────────────────────────────────────────

/// Wrapper for a `/EmbeddedFile` stream (ISO 32000-1 §7.11.4).
///
/// Construct via [`crate::FileSpec::embedded_file`] rather than directly.
///
/// All accessors are cheap: only [`payload`](EmbeddedFileStream::payload)
/// performs I/O (decoding the filter chain).
pub struct EmbeddedFileStream<'a, R: Read + Seek + 'static> {
    /// qpdf's shared `/EmbeddedFile` object handle. Unlike a copied stream
    /// value, this preserves identity and lets metadata setters
    /// update its dictionary without cloning its payload.
    stream: ObjectHandle,
    // The wrapper still owns the document's exclusive borrow. RefCell only
    // permits qpdf-shaped read accessors to perform explicit resolution.
    pdf: RefCell<&'a mut Pdf<R>>,
}

impl<'a, R: Read + Seek> EmbeddedFileStream<'a, R> {
    /// Create an indirect `/EmbeddedFile` stream from decoded data, including
    /// qpdf's computed `/Params /Size` and binary MD5 `/CheckSum` values.
    ///
    /// This Rust form of qpdf's `createEFStream` returns the created
    /// `ObjectHandle`; use [`Self::new`] to obtain the borrowing helper.
    pub fn create_ef_stream(pdf: &mut Pdf<R>, data: impl AsRef<[u8]>) -> Result<ObjectHandle> {
        let stream = pdf.new_stream()?;
        stream.replace_stream_data(
            Rc::new(data.as_ref().to_vec()),
            Some(ObjectHandle::null()),
            Some(ObjectHandle::null()),
        );
        Self::new_from_stream(pdf, stream)
    }

    /// Create an indirect `/EmbeddedFile` stream from a deferred qpdf-style
    /// provider.  The provider is retained by the stream and is not invoked
    /// until the common finalization path pipes the stream data.
    ///
    /// This is qpdf's provider overload from
    /// `QPDFEFStreamObjectHelper.cc:102-107`: qpdf creates an empty stream,
    /// installs the provider with `replaceStreamData`, and then delegates to
    /// one `newFromStream` implementation.
    pub fn create_ef_stream_from_provider(
        pdf: &mut Pdf<R>,
        provider: Rc<dyn StreamDataProvider>,
    ) -> Result<ObjectHandle> {
        let stream = pdf.new_stream()?;
        stream.replace_stream_data_provider(
            provider,
            Some(ObjectHandle::null()),
            Some(ObjectHandle::null()),
        )?; // cov:ignore: Pdf::new_stream guarantees a stream handle here
        Self::new_from_stream(pdf, stream)
    }

    /// Apply qpdf's shared `newFromStream` EmbeddedFile finalization.
    ///
    /// `QPDFEFStreamObjectHelper.cc:131-148` sets `/Type` before piping the
    /// decoded stream through `Pl_Count -> Pl_MD5 -> Pl_Discard`.  `/Params`
    /// is populated only after a successful pipe; a failed provider/filter
    /// path gets qpdf's warning and never falls back to a materialized length
    /// or a second digest computation.
    pub(super) fn new_from_stream(pdf: &mut Pdf<R>, stream: ObjectHandle) -> Result<ObjectHandle> {
        let stream_dict = stream.as_stream_dict().ok_or_else(|| {
            Error::System("EmbeddedFile factory received a non-stream object".to_string())
        })?;
        stream_dict.replace_key(b"/Type", ObjectHandle::name(b"EmbeddedFile".to_vec()))?;
        pdf.mark_object_handle_dirty(&stream)?;

        let mut discard = Discard;
        let mut md5 = PlMd5::new("EF md5", &mut discard);
        let (success, size, checksum) = {
            let mut count = Count::new("EF size", &mut md5);
            let mut filtering_attempted = false;
            let success = stream.pipe_stream_data(
                &mut count,
                &mut filtering_attempted,
                0,
                DecodeLevel::All,
                false,
                false,
            )?;
            let size = count.count();
            drop(count);
            let checksum = if success {
                Some(
                    hex::decode(md5.get_hex_digest()?)
                        .expect("PlMd5 always returns a hexadecimal digest"),
                )
            } else {
                None
            };
            (success, size, checksum)
        };

        if success {
            let checksum = checksum.expect("successful EmbeddedFile pipe has a checksum");
            stream_dict.replace_key(
                b"/Params",
                ObjectHandle::dictionary(vec![
                    (
                        b"/Size".to_vec(),
                        ObjectHandle::integer(
                            i64::try_from(size).map_err(|_| {
                                // cov:ignore-start: an in-memory stream cannot exceed PDF's signed integer range in tests
                                Error::System(
                                    "EmbeddedFile size exceeds PDF integer range".to_string(),
                                )
                                // cov:ignore-end
                            })?, // cov:ignore: closing line of the unreachable signed-PDF-range guard
                        ),
                    ),
                    (b"/CheckSum".to_vec(), ObjectHandle::string(checksum)),
                ]),
            )?; // cov:ignore: factory-created stream and parameter values share this PDF
            pdf.mark_object_handle_dirty(&stream)?;
        } else {
            stream.warn_if_possible("unable to get stream data for new embedded file stream")?;
        }

        Ok(stream)
    }

    /// Create an embedded-file stream from a filesystem path.
    ///
    /// The path is retained by qpdf's provider-shaped callback and the file is
    /// opened afresh for each stream pipe. This is the Rust equivalent of
    /// qpdf's `QUtil::file_provider` overload: it does not materialize the
    /// complete file into an intermediate `Vec<u8>`, and repeated reads use
    /// the same provider source. This follows the path overload in
    /// `QPDFFileSpecObjectHelper.cc:85-105` and the provider construction in
    /// `QPDFEFStreamObjectHelper.cc:90-107`.
    pub fn create_ef_stream_from_path<P: AsRef<Path>>(
        pdf: &mut Pdf<R>,
        path: P,
    ) -> Result<ObjectHandle> {
        let path = path.as_ref().to_path_buf();
        let stream = pdf.new_stream()?;
        stream.replace_stream_data_with_callback(
            move |pipeline| {
                let mut file =
                    File::open(&path).map_err(|error| qpdf_style_open_error(&path, error))?;
                let mut buffer = [0_u8; 8192];
                loop {
                    let read = file.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    pipeline.write(&buffer[..read]).map_err(Error::from)?;
                }
                pipeline.finish().map_err(Error::from)?;
                Ok(())
            },
            Some(ObjectHandle::null()),
            Some(ObjectHandle::null()),
        )?; // cov:ignore: Pdf::new_stream guarantees a stream handle here
        Self::new_from_stream(pdf, stream)
    }

    /// Construct a wrapper for a direct or indirect `/EmbeddedFile` stream
    /// handle, matching qpdf's `QPDFEFStreamObjectHelper(QPDFObjectHandle)`
    /// constructor. An indirect handle must be the canonical handle of `pdf`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] for a handle owned by another [`Pdf`].
    /// Rust makes qpdf's document ownership explicit here rather than silently
    /// resolving the same object number in the supplied document.
    pub fn new(stream: ObjectHandle, pdf: &'a mut Pdf<R>) -> Result<Self> {
        ensure_indirect_handle_belongs_to_pdf(&stream, pdf, "embedded-file")?;
        Ok(Self {
            stream,
            pdf: RefCell::new(pdf),
        })
    }

    fn resolved_stream(&self) -> Result<Option<(ObjectHandle, ObjectHandle, Option<ObjectRef>)>> {
        // QPDFEFStreamObjectHelper::getParam calls QPDFObjectHandle::getDict
        // (libqpdf/QPDFEFStreamObjectHelper.cc:20-28), whose
        // asStreamWithAssert/assertType path raises this runtime error for a
        // non-stream object (libqpdf/QPDFObjectHandle.cc:319-324, 2215-2223).
        let (stream, terminal_ref) = self.pdf.borrow_mut().resolve_handle_ref(&self.stream)?;
        let stream = match terminal_ref {
            Some(object_ref) => {
                let mut pdf = self.pdf.borrow_mut();
                let stream = pdf.get_object_handle(object_ref);
                pdf.resolve(&stream)?;
                stream
            }
            None => stream,
        };
        if let Some(dictionary) = stream.as_stream_dict() {
            return Ok(Some((stream, dictionary, terminal_ref)));
        }
        if stream.is_null() {
            return Ok(None);
        }
        let type_name = stream.type_name()?;
        Err(Error::System(format!(
            "operation for stream attempted on object of type {}",
            type_name
        )))
    }

    fn resolved_key(&self, dictionary: &ObjectHandle, key: &[u8]) -> Result<ObjectHandle> {
        let key = canonical_dictionary_key(key);
        let value = dictionary.get_key(&key);
        self.pdf.borrow_mut().resolve_handle(&value)
    }

    fn param_value(&self, key: &[u8]) -> Result<ObjectHandle> {
        let Some((_, stream_dict, _)) = self.resolved_stream()? else {
            return Ok(ObjectHandle::null());
        };
        let params = self.resolved_key(&stream_dict, b"Params")?;
        if params.as_dictionary().is_none() {
            return Ok(ObjectHandle::null());
        }
        self.resolved_key(&params, key)
    }

    /// Decode and return the payload bytes.
    ///
    /// Applies the stream's full filter chain (e.g. `/FlateDecode`) via
    /// [`crate::filters::decode_stream_data`].
    ///
    /// # Errors
    ///
    /// Propagates any error from the filter decoder (unsupported filter,
    /// corrupt data, etc.).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use flpdf::{FileSpec, ObjectRef, Pdf};
    /// # use std::fs::File;
    /// # use std::io::BufReader;
    /// # let mut pdf = Pdf::open(BufReader::new(File::open("a.pdf")?))?;
    /// # let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    /// if let Some(mut ef) = fs.embedded_file()? {
    ///     let data: Vec<u8> = ef.payload()?;
    ///     assert!(!data.is_empty());
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn payload(&self) -> Result<Vec<u8>> {
        let Some((stream, stream_dict, _)) = self.resolved_stream()? else {
            return Err(Error::Unsupported(
                "expected an /EmbeddedFile stream object".to_string(),
            ));
        };
        // The dictionary describes the encoded stream, while raw data goes
        // through the stream primitive so it works for both parsed original
        // source bytes and replacement buffers.
        let data = stream.get_raw_stream_data()?;
        decode_stream_data_from_handle(&stream_dict, data.as_ref(), DecodeLimits::default())
    }

    /// Pipe the decoded payload through the canonical qpdf stream pipeline.
    ///
    /// This combines `QPDFFileSpecObjectHelper::getEmbeddedFileStream`
    /// (`libqpdf/QPDFFileSpecObjectHelper.cc:61-77`) with
    /// `QPDFObjectHandle::pipeStreamData` (`libqpdf/QPDFObjectHandle.cc:1301-1324`).
    /// A filter failure is returned as `Ok(false)` after the owning document
    /// has received its warning, matching qpdf's `doShowAttachment` caller,
    /// which intentionally ignores that boolean.
    pub(crate) fn pipe_stream_data(&self, pipeline: &mut dyn Pipeline) -> Result<bool> {
        let Some((stream, _, _)) = self.resolved_stream()? else {
            return Err(Error::Unsupported(
                "expected an /EmbeddedFile stream object".to_string(),
            ));
        };
        let mut filtering_attempted = false;
        stream.pipe_stream_data(
            pipeline,
            &mut filtering_attempted,
            0,
            DecodeLevel::All,
            false,
            false,
        )
    }

    /// Return the MIME type from `/Subtype`, as raw bytes.
    ///
    /// `/Subtype` is a PDF name, e.g. `b"application/pdf"`.  Returns `None`
    /// when the key is absent.
    ///
    /// # Errors
    ///
    /// Returns `Ok(None)` for all missing/wrong-type cases; never errors.
    pub fn mimetype(&self) -> Result<Option<Vec<u8>>> {
        let Some((_, stream_dict, _)) = self.resolved_stream()? else {
            return Ok(None);
        };
        Ok(self.resolved_key(&stream_dict, b"Subtype")?.as_name())
    }

    /// Return `/Params /CreationDate` as a raw PDF date byte sequence.
    ///
    /// PDF date format: `D:YYYYMMDDHHmmSSOHH'mm'` (ISO 32000-1 §7.9.4).
    /// No date parsing is performed — the bytes are returned as-is.
    ///
    /// # Errors
    ///
    /// Returns `Ok(None)` for all missing/wrong-type cases.
    pub fn creation_date(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.param_value(b"CreationDate")?.as_string())
    }

    /// Return `/Params /ModDate` as a raw PDF date byte sequence.
    ///
    /// # Errors
    ///
    /// Returns `Ok(None)` for all missing/wrong-type cases.
    pub fn modification_date(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.param_value(b"ModDate")?.as_string())
    }

    /// Return `/Params /CheckSum` as raw bytes (typically a 16-byte MD5 hash).
    ///
    /// # Errors
    ///
    /// Returns `Ok(None)` for all missing/wrong-type cases.
    pub fn checksum(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.param_value(b"CheckSum")?.as_string())
    }

    /// Return `/Params /Size` — the uncompressed file size in bytes.
    ///
    /// # Errors
    ///
    /// Returns `Ok(None)` for all missing/wrong-type cases.
    pub fn size(&self) -> Result<Option<i64>> {
        Ok(self.param_value(b"Size")?.as_integer())
    }

    /// Return `/Params /CreationDate` through qpdf's UTF-8 string view.
    ///
    /// The byte vector mirrors qpdf's `std::string`: an explicit UTF-8 BOM
    /// may be followed by invalid UTF-8, which Rust's [`String`] cannot hold.
    pub fn get_creation_date(&self) -> Result<Vec<u8>> {
        Ok(self
            .creation_date()?
            .map(|value| utf8_value(&value))
            .unwrap_or_default())
    }

    /// Return `/Params /ModDate` through qpdf's UTF-8 string view.
    pub fn get_mod_date(&self) -> Result<Vec<u8>> {
        Ok(self
            .modification_date()?
            .map(|value| utf8_value(&value))
            .unwrap_or_default())
    }

    /// Return `/Params /Size`, or qpdf's `0` default when it is absent,
    /// negative, or not representable as `usize`.
    pub fn get_size(&self) -> Result<usize> {
        let Some(value) = self.size()? else {
            return Ok(0);
        };
        if value < 0 {
            return Ok(0);
        }
        Ok(u32::try_from(value).unwrap_or(u32::MAX) as usize)
    }

    /// Return `/Subtype` as a MIME-type string, without a leading slash.
    pub fn get_subtype(&self) -> Result<Vec<u8>> {
        Ok(self
            .mimetype()?
            .filter(|value| !value.is_empty())
            .unwrap_or_default())
    }

    /// Return `/Params /CheckSum` as qpdf's binary string value.
    pub fn get_checksum(&self) -> Result<Vec<u8>> {
        Ok(self.checksum()?.unwrap_or_default())
    }

    fn set_param(&mut self, key: &str, value: Vec<u8>) -> Result<()> {
        let Some((_, stream_dict, stream_ref)) = self.resolved_stream()? else {
            return Ok(());
        };
        let params = stream_dict.get_key(b"/Params");
        let (resolved, terminal_ref) = self.pdf.borrow_mut().resolve_handle_ref(&params)?;
        if resolved.as_dictionary().is_some() {
            let target = match terminal_ref {
                Some(object_ref) => {
                    let mut pdf = self.pdf.borrow_mut();
                    let target = pdf.get_object_handle(object_ref);
                    pdf.resolve(&target)?;
                    target
                }
                None => resolved,
            };
            {
                let mut pdf = self.pdf.borrow_mut();
                if let Some(object_ref) = terminal_ref.or(stream_ref) {
                    pdf.mark_object_handle_mutated(object_ref);
                } else {
                    pdf.mark_object_handle_dirty(&target)?;
                }
            }
            let key = canonical_dictionary_key(key.as_bytes());
            target.replace_key(&key, ObjectHandle::string(value))?;
            return Ok(());
        }

        {
            let mut pdf = self.pdf.borrow_mut();
            if let Some(object_ref) = stream_ref {
                pdf.mark_object_handle_mutated(object_ref);
            } else {
                pdf.mark_object_handle_dirty(&stream_dict)?;
            }
        }
        stream_dict.replace_key(
            b"/Params",
            ObjectHandle::dictionary(vec![(
                canonical_dictionary_key(key.as_bytes()),
                ObjectHandle::string(value),
            )]),
        )?; // cov:ignore: the factory-created parameter value is always unowned
        Ok(())
    }

    /// Set `/Params /CreationDate` to a raw PDF date string.
    pub fn set_creation_date(&mut self, value: impl AsRef<[u8]>) -> Result<&mut Self> {
        self.set_param("CreationDate", value.as_ref().to_vec())?;
        Ok(self)
    }

    /// Set `/Params /ModDate` to a raw PDF date string.
    pub fn set_mod_date(&mut self, value: impl AsRef<[u8]>) -> Result<&mut Self> {
        self.set_param("ModDate", value.as_ref().to_vec())?;
        Ok(self)
    }

    /// Set `/Subtype` to a MIME type represented as logical PDF Name bytes.
    pub fn set_subtype(&mut self, value: impl AsRef<[u8]>) -> Result<&mut Self> {
        let Some((_, stream_dict, stream_ref)) = self.resolved_stream()? else {
            return Ok(self);
        };
        {
            let mut pdf = self.pdf.borrow_mut();
            if let Some(object_ref) = stream_ref {
                pdf.mark_object_handle_mutated(object_ref);
            } else {
                pdf.mark_object_handle_dirty(&stream_dict)?;
            }
        }
        stream_dict.replace_key(b"/Subtype", ObjectHandle::name(value.as_ref().to_vec()))?;
        Ok(self)
    }
}
